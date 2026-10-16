import os
import UIKit
import UserNotifications

/// Posts an answer chosen on a notification the extension made answerable
/// (`MAGICAN_ASK_*` categories), then replaces that notification with a short,
/// question-free result. Runs in the background: every answer action requires
/// an unlocked device but never opens the app.
enum AttentionPushAnswerHandler {
    /// `false` when `response` is not a push-answer action (the caller then
    /// handles it as before — legacy Approve/Reject, or open the ask).
    static func handle(_ response: UNNotificationResponse, session: URLSession = .shared) async -> Bool {
        let action = response.actionIdentifier
        guard AttentionPushAnswerAction.isAnswerAction(action) else { return false }
        let request = response.notification.request
        let backgroundTask = await MainActor.run {
            UIApplication.shared.beginBackgroundTask(withName: "magican-push-answer")
        }
        defer {
            Task { @MainActor in UIApplication.shared.endBackgroundTask(backgroundTask) }
        }

        let result: AttentionPushAnswerResult
        if let context = AttentionPushAnswerContext(userInfo: request.content.userInfo),
           let value = AttentionPushAnswerAction.value(
               actionIdentifier: action,
               userText: (response as? UNTextInputNotificationResponse)?.userText,
               context: context
           ) {
            result = await post(context: context, value: value, session: session)
            if result != .failed {
                await MainActor.run {
                    PendingHitlTracker.shared.drop(context.correlationID)
                    AppActions.shared.markAttentionDirty()
                }
            }
        } else {
            AttentionPushDiagnostics.logger.error(
                "app answer refused locally action=\(actionKind(action), privacy: .public) reason=no_context_or_value"
            )
            result = .failed
        }
        await replace(request, with: result)
        return true
    }

    static func post(
        context: AttentionPushAnswerContext,
        value: [String: Any],
        session: URLSession = .shared
    ) async -> AttentionPushAnswerResult {
        let corr = AttentionPushDiagnostics.correlationHash(context.correlationID)
        guard MagicianAccess.isConfigured,
              let url = respondURL(correlationID: context.correlationID) else {
            AttentionPushDiagnostics.logger.error(
                "app answer corr=\(corr, privacy: .public) mode=\(context.mode.rawValue, privacy: .public) not_connected"
            )
            return .failed
        }
        var request = URLRequest(url: url, timeoutInterval: 20)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try? JSONSerialization.data(
            withJSONObject: AttentionPushAnswerAction.requestBody(context: context, value: value)
        )
        let started = Date()
        let reply = try? await session.data(for: request)
        let latency = Int(Date().timeIntervalSince(started) * 1000)
        let status = (reply?.1 as? HTTPURLResponse)?.statusCode
        let result = AttentionPushAnswerResult.from(statusCode: status, body: reply?.0)
        // Never the answer value, the question, or credentials.
        AttentionPushDiagnostics.logger.info(
            "app answer corr=\(corr, privacy: .public) mode=\(context.mode.rawValue, privacy: .public) respond_status=\(status.map(String.init) ?? "network_error", privacy: .public) latency_ms=\(latency, privacy: .public) result=\(String(describing: result), privacy: .public)"
        )
        return result
    }

    /// The action's kind, never an option id (ids can carry content).
    static func actionKind(_ identifier: String) -> String {
        identifier.hasPrefix(AttentionPushAnswer.optionActionPrefix) ? "option" : identifier
    }

    static func respondURL(correlationID: String, base: URL = MagicianAccess.baseURL) -> URL? {
        guard let hitl = URL(string: "\(base.absoluteString)/api/magician/v2/hitl") else { return nil }
        return hitl.appendingPathComponent(correlationID).appendingPathComponent("respond")
    }

    /// The result notification: same identifier (so it replaces the ask) and
    /// thread, no buttons, and only the routing keys a tap needs to open the
    /// ask — never the question.
    static func resultContent(
        for result: AttentionPushAnswerResult,
        replacing original: UNNotificationContent
    ) -> UNMutableNotificationContent {
        let content = UNMutableNotificationContent()
        content.title = result.title
        content.body = result.body
        content.threadIdentifier = original.threadIdentifier
        content.interruptionLevel = result == .failed ? .active : .passive
        var userInfo: [AnyHashable: Any] = ["kind": "attention_answer_result"]
        for key in ["correlation_id", "source", "deep_link"] {
            if let value = original.userInfo[key] { userInfo[key] = value }
        }
        content.userInfo = userInfo
        return content
    }

    private static func replace(_ request: UNNotificationRequest, with result: AttentionPushAnswerResult) async {
        let content = resultContent(for: result, replacing: request.content)
        try? await UNUserNotificationCenter.current().add(
            UNNotificationRequest(identifier: request.identifier, content: content, trigger: nil)
        )
    }
}
