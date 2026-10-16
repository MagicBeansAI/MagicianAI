import UserNotifications

/// Adds answer buttons to an Attention push the backend marked answerable
/// (`answer_mode`). The ask is read with this iPhone's own credential, checked
/// again against `AttentionPushEligibility`, and given a per-ask category.
/// Every failure — no connection, a slow or refused read, an ask that does not
/// fit, previews that would leak it, or the system deadline — delivers the
/// push unchanged, so the tap still opens the ask exactly as before.
final class NotificationService: UNNotificationServiceExtension {
    private let lock = NSLock()
    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var fallback: UNNotificationContent?
    private var work: Task<Void, Never>?
    /// The decision so far — what `serviceExtensionTimeWillExpire` records.
    private var progress: AttentionPushDecisionRecord?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        lock.lock()
        self.contentHandler = contentHandler
        fallback = request.content
        lock.unlock()

        let userInfo = request.content.userInfo
        guard let envelope = AttentionPushEnvelope(userInfo: userInfo) else {
            // An attention push without an answer mode is open-only by design;
            // record it so "no buttons" is explainable. Others are not ours.
            if userInfo["kind"] as? String == "attention_requested" {
                let correlation = userInfo["correlation_id"] as? String ?? ""
                AttentionPushDiagnostics.record(.init(
                    at: Date(),
                    correlationHash: AttentionPushDiagnostics.correlationHash(correlation),
                    answerMode: (userInfo["answer_mode"] as? String).map { "unsupported:\($0)" } ?? "none",
                    outcome: "fallback",
                    fallbackReason: "no_answer_mode"
                ))
            }
            deliver(request.content, record: nil)
            return
        }
        update(.init(
            at: Date(),
            correlationHash: AttentionPushDiagnostics.correlationHash(envelope.correlationID),
            answerMode: envelope.mode.rawValue,
            outcome: "fallback"
        ))
        // Holds the extension until it has answered iOS, one way or the other.
        work = Task {
            let content = await self.enrich(request.content, envelope: envelope)
            self.deliver(content, record: self.currentProgress())
        }
    }

    override func serviceExtensionTimeWillExpire() {
        work?.cancel()
        lock.lock()
        let original = fallback
        var record = progress
        lock.unlock()
        record?.outcome = "fallback"
        record?.fallbackReason = "deadline"
        if let original { deliver(original, record: record) }
    }

    /// Hands iOS exactly one content, whichever path finishes first, and
    /// records that path's decision.
    private func deliver(_ content: UNNotificationContent, record: AttentionPushDecisionRecord?) {
        lock.lock()
        let handler = contentHandler
        contentHandler = nil
        lock.unlock()
        guard let handler else { return }
        if let record { AttentionPushDiagnostics.record(record) }
        handler(content)
    }

    private func update(_ record: AttentionPushDecisionRecord) {
        lock.lock()
        progress = record
        lock.unlock()
    }

    private func currentProgress() -> AttentionPushDecisionRecord? {
        lock.lock()
        defer { lock.unlock() }
        return progress
    }

    private func mutate(_ change: (inout AttentionPushDecisionRecord) -> Void) {
        lock.lock()
        if var record = progress {
            change(&record)
            progress = record
        }
        lock.unlock()
    }

    private func keepOriginal(_ original: UNNotificationContent, reason: String) -> UNNotificationContent {
        mutate {
            $0.outcome = "fallback"
            $0.fallbackReason = reason
        }
        return original
    }

    private func enrich(
        _ original: UNNotificationContent,
        envelope: AttentionPushEnvelope
    ) async -> UNNotificationContent {
        let center = UNUserNotificationCenter.current()
        let settings = await center.notificationSettings()
        let showOnLockScreen = AttentionPushAnswer.showQuestionsOnLockScreen()
        let reveal = AttentionPushPrivacy.revealsAsk(
            showOnLockScreen: showOnLockScreen,
            previews: settings.showPreviewsSetting
        )
        mutate {
            $0.privacy = AttentionPushPrivacy.decisionLabel(
                showOnLockScreen: showOnLockScreen,
                previews: settings.showPreviewsSetting
            )
        }
        // Open-only by privacy: nothing to fetch, nothing to register.
        guard reveal else { return keepOriginal(original, reason: "privacy") }

        let report = await AttentionPushAskClient.fetch(correlationID: envelope.correlationID)
        mutate {
            $0.fetchStatus = report.httpStatus
            $0.fetchLatencyMs = report.latencyMs
        }
        if Task.isCancelled { return keepOriginal(original, reason: "cancelled") }
        let ask: AttentionPushAsk
        switch report.outcome {
        case .ask(let fetched): ask = fetched
        case .unavailable(let reason): return keepOriginal(original, reason: "fetch_\(reason)")
        }

        let decision = AttentionPushEligibility.evaluate(ask, envelope: envelope)
        mutate { $0.eligibility = decision.reason }
        guard case .eligible(let plan) = decision else { return keepOriginal(original, reason: "ineligible") }

        let category = AttentionPushCategories.category(for: plan)
        let inUse = Set(await center.deliveredNotifications().map(\.request.content.categoryIdentifier))
        let existing = await center.notificationCategories()
        center.setNotificationCategories(
            AttentionPushCategories.merged(existing: existing, adding: category, inUse: inUse)
        )
        // Registration is asynchronous; a notification delivered before its
        // category lands shows no buttons. Wait briefly for it to appear.
        var registered = false
        for _ in 0..<10 where !Task.isCancelled {
            if await center.notificationCategories().contains(where: { $0.identifier == category.identifier }) {
                registered = true
                break
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        mutate { $0.categoryRegistered = registered }
        guard registered else { return keepOriginal(original, reason: "category_not_registered") }
        if Task.isCancelled { return keepOriginal(original, reason: "cancelled") }
        mutate {
            $0.outcome = "enriched"
            $0.fallbackReason = nil
        }
        return AttentionPushContent.enriched(original, plan: plan, reveal: true)
    }
}
