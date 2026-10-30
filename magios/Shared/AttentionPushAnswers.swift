import CryptoKit
import Foundation
import os
import UserNotifications

// Answer an Attention ask straight from its push notification.
//
// The push itself never carries the question: it says *that* an ask waits
// (`kind: attention_requested`) and, when the backend judged it answerable
// from a notification, *how* (`answer_mode`: confirm | choice | text). The
// Notification Service Extension (`MagiosNotificationService`) then reads the
// ask with this iPhone's own credential, re-checks that it really is that
// simple (defense in depth), and adds a per-ask category of answer buttons.
// The app delegate posts the chosen answer to `/hitl/{id}/respond` with
// channel `ios_push`. Everything here is pure or Foundation-only so the
// extension stays small and the rules are unit-testable from the app.

enum AttentionPushAnswer {
    static let categoryPrefix = "MAGICAN_ASK_"
    static let confirmAction = "MAGICAN_ASK_CONFIRM"
    static let denyAction = "MAGICAN_ASK_DENY"
    static let textAction = "MAGICAN_ASK_TEXT"
    static let optionActionPrefix = "opt:"
    /// userInfo key under which the extension leaves the answer context.
    static let contextKey = "magican_answer"
    static let channel = "ios_push"
    /// App Group (`MagicianAccess.store`) flag; off by default.
    static let showQuestionsOnLockScreenKey = "attentionPushShowQuestionsOnLockScreenV1"
    static let genericTitle = "Magican needs your input"
    static let genericBody = "Open Attention to keep your work moving."
    /// What iOS shows instead of the question while previews are hidden.
    static let hiddenPreviewsPlaceholder = "Unlock to see the question"
    static let fetchTimeout: TimeInterval = 8
    static let maxChoiceOptions = 4

    static func showQuestionsOnLockScreen(_ store: UserDefaults = MagicianAccess.store) -> Bool {
        store.bool(forKey: showQuestionsOnLockScreenKey)
    }
}

enum AttentionPushAnswerMode: String, Equatable {
    case confirm, choice, text
}

/// The push's root keys this feature reads.
struct AttentionPushEnvelope: Equatable {
    let correlationID: String
    let source: String?
    let mode: AttentionPushAnswerMode
    let optionCount: Int?

    /// `nil` for anything that is not an answerable attention push — those are
    /// delivered unchanged (open-only).
    init?(userInfo: [AnyHashable: Any]) {
        guard userInfo["kind"] as? String == "attention_requested",
              let correlation = (userInfo["correlation_id"] as? String)?
                .trimmingCharacters(in: .whitespacesAndNewlines), !correlation.isEmpty,
              let rawMode = userInfo["answer_mode"] as? String,
              let mode = AttentionPushAnswerMode(rawValue: rawMode) else { return nil }
        correlationID = correlation
        let source = (userInfo["source"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
        self.source = (source?.isEmpty ?? true) ? nil : source
        self.mode = mode
        if let count = userInfo["option_count"] as? Int {
            optionCount = count
        } else if let count = userInfo["option_count"] as? NSNumber {
            optionCount = count.intValue
        } else if let raw = userInfo["option_count"] as? String, let count = Int(raw) {
            optionCount = count
        } else {
            optionCount = nil
        }
    }
}

/// The parts of one fetched Attention feed item (`GET /feed/attention/{id}`)
/// that decide whether, and how, it can be answered from a notification.
/// Derivations mirror `AttentionItem` (source, input type, schema, options,
/// execution/task ids) so a push answer routes exactly like an in-app one.
struct AttentionPushAsk: Equatable {
    struct Option: Equatable {
        let id: String
        let label: String
        let requiresInput: Bool
        let actionType: String?
    }

    var itemType: String = ""
    var status: String = ""
    var source: String = "agentic"
    var inputType: String = "text"
    var question: String = ""
    var hint: String?
    var options: [Option] = []
    var confirmLabel: String?
    var denyLabel: String?
    var placeholder: String?
    var destructive = false
    var multiple = false
    var sensitive = false
    var hasQuestions = false
    var stalledWait = false
    var executionID: String?
    var taskID: String?

    init() {}

    init?(json data: Data) {
        guard let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return nil }
        // The single-ask read answers with the feed item bare or as `{item: …}`.
        self.init(item: root["item"] as? [String: Any] ?? root)
    }

    init(item: [String: Any]) {
        func string(_ value: Any?) -> String? {
            guard let text = value as? String else { return nil }
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        }
        let metadata = item["metadata"] as? [String: Any] ?? [:]
        let hitl = metadata["hitl_request"] as? [String: Any] ?? [:]
        let ownSchema = metadata["input_schema"] as? [String: Any]
        let hitlSchema = hitl["input_schema"] as? [String: Any]
        let schema = ownSchema ?? hitlSchema ?? [:]
        let schemas = [ownSchema, hitlSchema].compactMap { $0 }

        itemType = string(item["item_type"]) ?? ""
        status = string(item["status"]) ?? ""

        if itemType == "approval" {
            source = "approval"
        } else if let own = string(metadata["source"]) {
            source = own
        } else if let nested = string(hitl["source"]) {
            source = nested
        } else {
            switch string(metadata["attention_kind"]) {
            case "user_request.pending": source = "user_request"
            case "max_iterations_reached": source = "escalation"
            case "diff_approval": source = "diff_approval"
            case "clarification": source = "clarification"
            case "plan_approval": source = "plan_approval"
            default: source = "agentic"
            }
        }

        if itemType == "approval" {
            inputType = "confirmation"
        } else if source == "mcp_oauth" {
            inputType = "external_action"
        } else {
            inputType = string(ownSchema?["type"]) ?? string(metadata["input_type"])
                ?? string(hitlSchema?["type"]) ?? string(hitl["input_type"]) ?? "text"
        }

        let title = string(item["title"]) ?? ""
        question = string(metadata["question"]) ?? string(hitl["prompt"]) ?? title
        hint = string(metadata["hint"]) ?? string(item["summary"])

        let rawOptions = (schema["options"] as? [[String: Any]]) ?? (metadata["options"] as? [[String: Any]]) ?? []
        options = rawOptions.compactMap { raw in
            guard let id = string(raw["id"]) else { return nil }
            let action = raw["action"] as? [String: Any]
            return Option(
                id: id,
                label: string(raw["label"]) ?? id,
                requiresInput: raw["requires_input"] as? Bool ?? false,
                actionType: string(action?["type"])
            )
        }
        confirmLabel = string(schema["confirm_label"])
        denyLabel = string(schema["deny_label"])
        placeholder = string(schema["placeholder"])
        destructive = schemas.contains { $0["destructive"] as? Bool == true }
        multiple = schemas.contains { $0["multiple"] as? Bool == true }
        // The backend's value-free spec marks a secret; the two secure-browser
        // request types predate it and are secret by name (`AttentionItem.isSensitive`).
        sensitive = schemas.contains { schema in
            if let spec = schema["sensitive"], !(spec is NSNull) { return true }
            let requestType = string(schema["request_type"]) ?? ""
            return requestType == "secure_browser_input" || requestType == "secure_browser_confirm"
        }
        hasQuestions = schemas.contains { !(($0["questions"] as? [Any]) ?? []).isEmpty }
        stalledWait = string(metadata["attention_reason"]) == "waiting_without_question"

        let scope = hitl["scope"] as? [String: Any]
        executionID = [string(scope?["execution_id"]), string(hitl["execution_id"]),
                       string(metadata["execution_id"]), string(item["execution_id"])]
            .compactMap { $0 }.first
        taskID = [string(item["task_id"]), string(metadata["task_id"])].compactMap { $0 }.first

        // The server's respond handle (`metadata.respond`, Rust
        // `HitlRespondHandle`) names the source, input type and run to post;
        // the derivations above are the fallback for older servers.
        if let respond = (metadata["respond"] as? [String: Any]) ?? (hitl["respond"] as? [String: Any]),
           let handleSource = string(respond["source"]),
           let handleInputType = string(respond["input_type"]) {
            source = handleSource
            inputType = handleInputType
            if let handleExecution = string(respond["execution_id"]) { executionID = handleExecution }
            if let handleTask = string(respond["task_id"]) { taskID = handleTask }
        }
    }
}

/// What the notification offers for one ask — produced only when the fetched
/// content agrees with the push's `answer_mode`.
struct AttentionPushAnswerPlan: Equatable {
    struct Choice: Equatable {
        let id: String
        let label: String
    }

    let mode: AttentionPushAnswerMode
    let correlationID: String
    let source: String
    let inputType: String
    let question: String
    let choices: [Choice]
    let confirmLabel: String
    let denyLabel: String
    let placeholder: String?
    let executionID: String?
    let taskID: String?

    var context: AttentionPushAnswerContext {
        AttentionPushAnswerContext(
            mode: mode,
            correlationID: correlationID,
            source: source,
            inputType: inputType,
            executionID: executionID,
            taskID: taskID,
            optionIDs: choices.map(\.id)
        )
    }
}

enum AttentionPushEligibility {
    /// The answer plan when `ask` can be answered from a notification in
    /// `envelope.mode`; `nil` means open-only (tap opens the ask).
    ///
    /// Never for a sensitive, destructive or multi-question ask; then
    /// - confirm: a confirmation.
    /// - choice: one pick from 1–4 options, none asking for typed input
    ///   (`allow_other` stays answerable; "Other" is simply not offered).
    /// - text: plain text.
    static func plan(for ask: AttentionPushAsk, envelope: AttentionPushEnvelope) -> AttentionPushAnswerPlan? {
        if case .eligible(let plan) = evaluate(ask, envelope: envelope) { return plan }
        return nil
    }

    enum Decision: Equatable {
        case eligible(AttentionPushAnswerPlan)
        /// A short, content-free reason code for diagnostics.
        case openOnly(String)

        var reason: String {
            switch self {
            case .eligible: "eligible"
            case .openOnly(let reason): reason
            }
        }
    }

    /// `plan(for:envelope:)` with the reason an ask stays open-only.
    static func evaluate(_ ask: AttentionPushAsk, envelope: AttentionPushEnvelope) -> Decision {
        if ask.stalledWait { return .openOnly("stalled_wait") }
        if !(ask.status.isEmpty || ask.status == "needs_action") { return .openOnly("status_not_pending") }
        if ["failed", "running"].contains(ask.itemType) { return .openOnly("item_not_actionable") }
        if ask.sensitive { return .openOnly("sensitive") }
        // A destructive ask is app-only in every mode, not just confirm.
        if ask.destructive { return .openOnly("destructive") }
        if ask.hasQuestions { return .openOnly("multi_question_form") }
        if ask.question.isEmpty { return .openOnly("no_question") }

        var choices: [AttentionPushAnswerPlan.Choice] = []
        switch envelope.mode {
        case .confirm:
            guard ask.inputType == "confirmation" else { return .openOnly("mode_mismatch") }
        case .choice:
            guard ask.inputType == "choice" else { return .openOnly("mode_mismatch") }
            if ask.multiple { return .openOnly("multiple_select") }
            if !(1...AttentionPushAnswer.maxChoiceOptions).contains(ask.options.count) {
                return .openOnly("option_count_out_of_range")
            }
            if let announced = envelope.optionCount, announced != ask.options.count {
                return .openOnly("option_count_mismatch")
            }
            if Set(ask.options.map(\.id)).count != ask.options.count { return .openOnly("duplicate_option_ids") }
            if ask.options.contains(where: \.requiresInput) { return .openOnly("option_requires_input") }
            if ask.options.contains(where: { ($0.actionType ?? "respond_choice") != "respond_choice" }) {
                return .openOnly("option_action_not_choice")
            }
            choices = ask.options.map { .init(id: $0.id, label: $0.label) }
        case .text:
            guard ask.inputType == "text" else { return .openOnly("mode_mismatch") }
        }
        return .eligible(AttentionPushAnswerPlan(
            mode: envelope.mode,
            correlationID: envelope.correlationID,
            // The push names the canonical `HitlRequested` source the legacy
            // Approve/Reject path already posts; the feed derivation is the
            // fallback when an older backend leaves it out.
            source: envelope.source ?? ask.source,
            inputType: ask.inputType,
            question: ask.question,
            choices: choices,
            confirmLabel: ask.confirmLabel ?? "Approve",
            denyLabel: ask.denyLabel ?? "Reject",
            placeholder: ask.placeholder,
            executionID: ask.executionID,
            taskID: ask.taskID
        ))
    }
}

/// Lock-screen privacy (owner decision). By default the question and its
/// answer buttons are only shown where iOS itself hides them until the phone is
/// unlocked; "Show questions on lock screen" opts into showing them anyway.
enum AttentionPushPrivacy {
    static func revealsAsk(showOnLockScreen: Bool, previews: UNShowPreviewsSetting) -> Bool {
        if showOnLockScreen { return true }
        switch previews {
        // iOS reveals the body (and the actions) only after Face ID / passcode.
        case .whenAuthenticated: return true
        // `.always` would put the question on the lock screen; `.never` shows
        // only the placeholder, so the buttons would answer an unseen question.
        case .always, .never: return false
        @unknown default: return false
        }
    }

    /// Content-free label of the decision, for diagnostics.
    static func decisionLabel(showOnLockScreen: Bool, previews: UNShowPreviewsSetting) -> String {
        if showOnLockScreen { return "reveal:setting_on" }
        switch previews {
        case .whenAuthenticated: return "reveal:previews_when_unlocked"
        case .always: return "hidden:previews_always"
        case .never: return "hidden:previews_never"
        @unknown default: return "hidden:previews_unknown"
        }
    }
}

/// The Settings hint under "Show questions on lock screen": with the toggle
/// off, iOS previews `Always` or `Never` keep every question hidden (the
/// privacy rule above), which otherwise looks like a broken feature.
enum AttentionPushPreviewHint: Equatable {
    case previewsAlways
    case previewsNever

    static func hint(showOnLockScreen: Bool, previews: UNShowPreviewsSetting) -> AttentionPushPreviewHint? {
        guard !showOnLockScreen else { return nil }
        switch previews {
        case .always: return .previewsAlways
        case .never: return .previewsNever
        default: return nil
        }
    }

    var message: String {
        switch self {
        case .previewsAlways:
            "iPhone shows Magican previews Always, so questions stay hidden. Set Settings › Notifications › Magican › Show Previews to When Unlocked to see them after Face ID — or turn this on."
        case .previewsNever:
            "iPhone never shows Magican previews, so questions stay hidden. Set Show Previews to When Unlocked to see them after Face ID — or turn this on."
        }
    }
}

/// One Notification Service Extension decision — never the question, option
/// labels or credentials; the correlation id only as a short hash.
struct AttentionPushDecisionRecord: Codable, Equatable {
    var at: Date
    var correlationHash: String
    var answerMode: String
    var privacy: String?
    var fetchStatus: Int?
    var fetchLatencyMs: Int?
    var eligibility: String?
    var categoryRegistered: Bool?
    /// `enriched` or `fallback`.
    var outcome: String
    var fallbackReason: String?

    enum CodingKeys: String, CodingKey {
        case at, privacy, eligibility, outcome
        case correlationHash = "correlation_hash"
        case answerMode = "answer_mode"
        case fetchStatus = "fetch_status"
        case fetchLatencyMs = "fetch_latency_ms"
        case categoryRegistered = "category_registered"
        case fallbackReason = "fallback_reason"
    }
}

enum AttentionPushDiagnostics {
    static let logger = Logger(subsystem: "com.magicbeans100x.magican", category: "push-answer")
    /// App Group (`MagicianAccess.store`) ring of the latest decisions.
    static let recordsKey = "attentionPushDecisionsV1"
    static let maxRecords = 20

    static func correlationHash(_ correlationID: String) -> String {
        String(AttentionPushCategories.identifier(correlationID: correlationID)
            .dropFirst(AttentionPushAnswer.categoryPrefix.count))
    }

    static func appending(
        _ record: AttentionPushDecisionRecord,
        to records: [AttentionPushDecisionRecord]
    ) -> [AttentionPushDecisionRecord] {
        Array((records + [record]).suffix(maxRecords))
    }

    static func records(_ store: UserDefaults = MagicianAccess.store) -> [AttentionPushDecisionRecord] {
        guard let data = store.data(forKey: recordsKey) else { return [] }
        return (try? JSONDecoder().decode([AttentionPushDecisionRecord].self, from: data)) ?? []
    }

    static func record(_ record: AttentionPushDecisionRecord, store: UserDefaults = MagicianAccess.store) {
        let updated = appending(record, to: records(store))
        if let data = try? JSONEncoder().encode(updated) { store.set(data, forKey: recordsKey) }
        let fetchStatus = record.fetchStatus.map(String.init) ?? "-"
        let latency = record.fetchLatencyMs.map(String.init) ?? "-"
        let category = record.categoryRegistered.map { $0 ? "registered" : "missing" } ?? "-"
        logger.info("nse decision corr=\(record.correlationHash, privacy: .public) mode=\(record.answerMode, privacy: .public) privacy=\(record.privacy ?? "-", privacy: .public) fetch_status=\(fetchStatus, privacy: .public) latency_ms=\(latency, privacy: .public) eligibility=\(record.eligibility ?? "-", privacy: .public) category=\(category, privacy: .public) outcome=\(record.outcome, privacy: .public) reason=\(record.fallbackReason ?? "-", privacy: .public)")
    }
}

enum AttentionPushCategories {
    static func identifier(correlationID: String) -> String {
        let digest = SHA256.hash(data: Data(correlationID.utf8))
        let hex = digest.prefix(6).map { String(format: "%02x", $0) }.joined()
        return AttentionPushAnswer.categoryPrefix + hex
    }

    /// Every answer action requires an unlocked device; none opens the app.
    static func category(for plan: AttentionPushAnswerPlan) -> UNNotificationCategory {
        let actions: [UNNotificationAction]
        switch plan.mode {
        case .confirm:
            actions = [
                UNNotificationAction(identifier: AttentionPushAnswer.confirmAction, title: plan.confirmLabel,
                                     options: [.authenticationRequired]),
                UNNotificationAction(identifier: AttentionPushAnswer.denyAction, title: plan.denyLabel,
                                     options: [.authenticationRequired, .destructive]),
            ]
        case .choice:
            actions = plan.choices.map {
                UNNotificationAction(identifier: AttentionPushAnswer.optionActionPrefix + $0.id, title: $0.label,
                                     options: [.authenticationRequired])
            }
        case .text:
            actions = [
                UNTextInputNotificationAction(
                    identifier: AttentionPushAnswer.textAction,
                    title: "Reply",
                    options: [.authenticationRequired],
                    textInputButtonTitle: "Send",
                    textInputPlaceholder: plan.placeholder ?? "Type your answer"
                ),
            ]
        }
        return UNNotificationCategory(
            identifier: identifier(correlationID: plan.correlationID),
            actions: actions,
            intentIdentifiers: [],
            hiddenPreviewsBodyPlaceholder: AttentionPushAnswer.hiddenPreviewsPlaceholder,
            options: [.hiddenPreviewsShowTitle]
        )
    }

    /// `existing` plus `adding`, without per-ask categories no delivered
    /// notification still uses (`inUse`). Other categories are kept as is.
    static func merged(
        existing: Set<UNNotificationCategory>,
        adding: UNNotificationCategory,
        inUse: Set<String>
    ) -> Set<UNNotificationCategory> {
        var result = existing.filter { category in
            category.identifier != adding.identifier
                && (!category.identifier.hasPrefix(AttentionPushAnswer.categoryPrefix)
                    || inUse.contains(category.identifier))
        }
        result.insert(adding)
        return result
    }
}

/// What the app needs to post an answer, left in the notification's userInfo
/// under `AttentionPushAnswer.contextKey` by the extension.
struct AttentionPushAnswerContext: Equatable {
    let mode: AttentionPushAnswerMode
    let correlationID: String
    let source: String
    let inputType: String
    let executionID: String?
    let taskID: String?
    let optionIDs: [String]

    var userInfoValue: [String: Any] {
        var value: [String: Any] = [
            "mode": mode.rawValue,
            "correlation_id": correlationID,
            "source": source,
            "input_type": inputType,
            "option_ids": optionIDs,
        ]
        if let executionID { value["execution_id"] = executionID }
        if let taskID { value["task_id"] = taskID }
        return value
    }

    init(mode: AttentionPushAnswerMode, correlationID: String, source: String, inputType: String,
         executionID: String?, taskID: String?, optionIDs: [String]) {
        self.mode = mode
        self.correlationID = correlationID
        self.source = source
        self.inputType = inputType
        self.executionID = executionID
        self.taskID = taskID
        self.optionIDs = optionIDs
    }

    init?(userInfo: [AnyHashable: Any]) {
        guard let raw = userInfo[AttentionPushAnswer.contextKey] as? [String: Any],
              let mode = (raw["mode"] as? String).flatMap(AttentionPushAnswerMode.init(rawValue:)),
              let correlationID = raw["correlation_id"] as? String, !correlationID.isEmpty,
              let source = raw["source"] as? String, !source.isEmpty,
              let inputType = raw["input_type"] as? String, !inputType.isEmpty else { return nil }
        self.init(
            mode: mode,
            correlationID: correlationID,
            source: source,
            inputType: inputType,
            executionID: raw["execution_id"] as? String,
            taskID: raw["task_id"] as? String,
            optionIDs: raw["option_ids"] as? [String] ?? []
        )
    }
}

enum AttentionPushAnswerAction {
    static func isAnswerAction(_ identifier: String) -> Bool {
        identifier == AttentionPushAnswer.confirmAction
            || identifier == AttentionPushAnswer.denyAction
            || identifier == AttentionPushAnswer.textAction
            || identifier.hasPrefix(AttentionPushAnswer.optionActionPrefix)
    }

    /// The `AgenticResumeValue` for one action, or `nil` when the action does
    /// not fit the ask (wrong mode, unknown option, empty reply).
    static func value(
        actionIdentifier: String,
        userText: String?,
        context: AttentionPushAnswerContext
    ) -> [String: Any]? {
        switch context.mode {
        case .confirm:
            switch actionIdentifier {
            case AttentionPushAnswer.confirmAction: return ["type": "confirmation", "confirmed": true]
            case AttentionPushAnswer.denyAction: return ["type": "confirmation", "confirmed": false]
            default: return nil
            }
        case .choice:
            guard actionIdentifier.hasPrefix(AttentionPushAnswer.optionActionPrefix) else { return nil }
            let id = String(actionIdentifier.dropFirst(AttentionPushAnswer.optionActionPrefix.count))
            guard context.optionIDs.contains(id) else { return nil }
            return ["type": "choice", "selected_id": id]
        case .text:
            guard actionIdentifier == AttentionPushAnswer.textAction else { return nil }
            let text = (userText ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            return text.isEmpty ? nil : ["type": "text", "value": text]
        }
    }

    /// The `/hitl/{correlation_id}/respond` body — the in-app answer's shape
    /// (`AttentionViewModel.postResponse`) with channel `ios_push`.
    static func requestBody(context: AttentionPushAnswerContext, value: [String: Any]) -> [String: Any] {
        var body: [String: Any] = [
            "source": context.source,
            "input_type": context.inputType,
            "value": value,
            "channel": AttentionPushAnswer.channel,
            "correlation_id": context.correlationID,
        ]
        if let executionID = context.executionID, !executionID.isEmpty { body["execution_id"] = executionID }
        if let taskID = context.taskID, !taskID.isEmpty,
           ["clarification", "plan_approval"].contains(context.source) {
            body["task_id"] = taskID
        }
        return body
    }
}

enum AttentionPushAnswerResult: Equatable {
    case sent, alreadyAnswered, failed

    static func from(statusCode: Int?, body: Data?) -> AttentionPushAnswerResult {
        guard let status = statusCode else { return .failed }
        let payload = body.flatMap { (try? JSONSerialization.jsonObject(with: $0)) as? [String: Any] }
        let reason = payload?["reason"] as? String
        switch status {
        case 200..<300:
            if payload?["status"] as? String == "reask_required" { return .failed }
            if payload?["accepted"] as? Bool == false || payload?["resumed"] as? Bool == false {
                return reason == "already_resolved" ? .alreadyAnswered : .failed
            }
            return .sent
        case 404, 409, 410:
            return .alreadyAnswered
        default:
            return .failed
        }
    }

    /// Never names the question: the result may show on a locked screen.
    var title: String {
        switch self {
        case .sent: "Answer sent"
        case .alreadyAnswered: "Already answered"
        case .failed: "Couldn't send your answer — tap to open"
        }
    }

    var body: String {
        switch self {
        case .sent: "Magican is on it."
        case .alreadyAnswered: "This question was settled elsewhere."
        case .failed: "Open Magican to answer it there."
        }
    }
}

enum AttentionPushAskFetch: Equatable {
    case ask(AttentionPushAsk)
    /// Any failure, a 404 included (an older server has no single-ask read,
    /// so 404 does not prove the ask was answered): keep the generic push.
    /// The reason is a content-free code for diagnostics.
    case unavailable(String)
}

struct AttentionPushAskFetchReport: Equatable {
    let outcome: AttentionPushAskFetch
    let httpStatus: Int?
    let latencyMs: Int?
}

enum AttentionPushAskClient {
    /// One path segment, every reserved character escaped (ids may hold `/`).
    static func url(correlationID: String, base: URL = MagicianAccess.baseURL) -> URL? {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        guard let segment = correlationID.addingPercentEncoding(withAllowedCharacters: allowed) else { return nil }
        let root = base.absoluteString.hasSuffix("/") ? String(base.absoluteString.dropLast()) : base.absoluteString
        return URL(string: "\(root)/api/magician/v2/feed/attention/\(segment)")
    }

    static func fetch(correlationID: String, session: URLSession? = nil) async -> AttentionPushAskFetchReport {
        guard MagicianAccess.isConfigured else {
            return .init(outcome: .unavailable("not_connected"), httpStatus: nil, latencyMs: nil)
        }
        guard let url = url(correlationID: correlationID) else {
            return .init(outcome: .unavailable("bad_url"), httpStatus: nil, latencyMs: nil)
        }
        var request = URLRequest(url: url, timeoutInterval: AttentionPushAnswer.fetchTimeout)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&request)
        let session = session ?? {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.timeoutIntervalForRequest = AttentionPushAnswer.fetchTimeout
            configuration.timeoutIntervalForResource = AttentionPushAnswer.fetchTimeout
            return URLSession(configuration: configuration)
        }()
        let started = Date()
        let reply = try? await session.data(for: request)
        let latency = Int(Date().timeIntervalSince(started) * 1000)
        guard let (data, response) = reply, let http = response as? HTTPURLResponse else {
            return .init(outcome: .unavailable("network_error"), httpStatus: nil, latencyMs: latency)
        }
        guard (200..<300).contains(http.statusCode) else {
            return .init(outcome: .unavailable("http_\(http.statusCode)"), httpStatus: http.statusCode, latencyMs: latency)
        }
        guard let ask = AttentionPushAsk(json: data) else {
            return .init(outcome: .unavailable("undecodable"), httpStatus: http.statusCode, latencyMs: latency)
        }
        return .init(outcome: .ask(ask), httpStatus: http.statusCode, latencyMs: latency)
    }
}

enum AttentionPushContent {
    /// The delivered content for an answerable ask. `reveal == false` keeps
    /// the generic text and adds no buttons (open-only).
    static func enriched(
        _ original: UNNotificationContent,
        plan: AttentionPushAnswerPlan,
        reveal: Bool
    ) -> UNNotificationContent {
        guard reveal, let content = original.mutableCopy() as? UNMutableNotificationContent else {
            return original
        }
        content.title = AttentionPushAnswer.genericTitle
        content.body = plan.question
        content.categoryIdentifier = AttentionPushCategories.identifier(correlationID: plan.correlationID)
        var userInfo = content.userInfo
        userInfo[AttentionPushAnswer.contextKey] = plan.context.userInfoValue
        content.userInfo = userInfo
        return content
    }
}
