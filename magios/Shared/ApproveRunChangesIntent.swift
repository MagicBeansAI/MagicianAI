import AppIntents
import Foundation

/// Backs the **Approve** button on the "Magician at work" and followed-item
/// Live Activities when the featured item is a run waiting on its diff
/// approval. Runs WITHOUT opening the app, like `StopObservationIntent`: it
/// answers the approval by id with the same call the run screen's Approve
/// makes (`VibeDevAPI.answerDiffApproval`), so every other surface clears.
///
/// It changes nothing on screen itself. The server's next push (or the app's
/// next poll) moves the activity once the run goes on; showing "approved"
/// before the server took the answer would be a guess.
public struct ApproveRunChangesIntent: AppIntent {
    public static var title: LocalizedStringResource = "Approve changes"
    public static var description = IntentDescription("Approves the changes a Magician coding run is waiting on.")
    public static var openAppWhenRun = false
    public static var isDiscoverable = false

    @Parameter(title: "Approval")
    public var approvalID: String

    public init() {}

    public init(approvalID: String) {
        self.approvalID = approvalID
    }

    public func perform() async throws -> some IntentResult {
        guard MagicianAccess.isConfigured, !approvalID.isEmpty else { return .result() }
        var request = URLRequest(
            url: MagicianAccess.baseURL
                .appendingPathComponent("api/magician/v2/hitl")
                .appendingPathComponent(approvalID)
                .appendingPathComponent("respond"),
            timeoutInterval: 15
        )
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        let body: [String: Any] = [
            "source": "diff_approval",
            "channel": "ios",
            "value": ["type": "choice", "selected_id": "apply"],
            "input_type": "diff_approval",
            "correlation_id": approvalID,
            "pause_state_id": approvalID,
        ]
        request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        if let (_, response) = try? await URLSession.shared.data(for: request),
           let http = response as? HTTPURLResponse,
           !(200..<300).contains(http.statusCode), http.statusCode != 409 {
            // 409 is an approval answered elsewhere already: nothing to do.
            debugLog("Live Activity approval was refused: HTTP \(http.statusCode)")
        }
        return .result()
    }
}
