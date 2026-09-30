import Foundation

/// Starts a task-backed execution for the keyboard **Act** lane. Mirrors the
/// proven Webpage-Assist pattern: `POST /executions` with `skip_planning:true`
/// + `internal:true` (a direct run, not a plan the user must manage). Returns the
/// task id; the caller stashes it via `SharedActions` so the app can open it.
///
/// Act is ALWAYS verification-card-gated in the UI — this client just runs the
/// already-confirmed goal.
public struct KeyboardActClient {
    public var session: URLSession = .shared
    public var baseURL: URL = MagicianAccess.baseURL
    public var timeout: TimeInterval = 30

    public init() {}

    public func run(title: String, goal: String) async throws -> String {
        let body: [String: Any] = [
            "title": title,
            "initial_message": goal,
            "ui_thread_id": "keyboard",
            "skip_planning": true,
            "internal": true,
        ]
        var req = URLRequest(url: baseURL.appendingPathComponent("api/magician/v2/executions"), timeoutInterval: timeout)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&req)
        req.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse, (200...299).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
        guard
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let execution = obj["execution"] as? [String: Any],
            let taskId = execution["task_id"] as? String,
            !taskId.isEmpty
        else { throw URLError(.cannotParseResponse) }
        return taskId
    }
}
