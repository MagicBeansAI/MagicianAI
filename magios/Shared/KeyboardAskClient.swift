import Foundation

/// Streams answers for the keyboard **Ask** lane over the chat SSE. Reuses a
/// single `keyboard-ask` chat session (cached in the App Group) so follow-ups
/// keep context; recreates it if the server drops it. Messages are tagged
/// `source_surface: keyboard` (honest provenance).
public struct KeyboardAskClient {
    public var session: URLSession = .shared
    public var baseURL: URL = MagicianAccess.baseURL
    public var timeout: TimeInterval = 60

    public init() {}

    private static let sessionKey = "keyboard.askSessionId"
    private static var store: UserDefaults { UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard }

    /// Cached ask session id, creating one on first use.
    public func resolveSession() async throws -> String {
        if let cached = Self.store.string(forKey: Self.sessionKey), !cached.isEmpty {
            return cached
        }
        let id = try await createSession()
        Self.store.set(id, forKey: Self.sessionKey)
        return id
    }

    private func createSession() async throws -> String {
        let url = baseURL.appendingPathComponent("api/magician/v2/chat/new")
        var req = URLRequest(url: url, timeoutInterval: timeout)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        authorize(&req)
        req.httpBody = "{}".data(using: .utf8)
        let (data, _) = try await session.data(for: req)
        guard
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let s = obj["session"] as? [String: Any],
            let id = s["id"] as? String
        else { throw URLError(.cannotParseResponse) }
        return id
    }

    /// Stream the answer to `question`; `onToken` receives the growing text.
    public func ask(_ question: String, sessionId: String, onToken: @escaping (String) -> Void) async throws {
        let url = baseURL.appendingPathComponent(
            "api/magician/v2/chat/sessions/\(sessionId)/messages/stream"
        )
        var req = URLRequest(url: url, timeoutInterval: timeout)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        authorize(&req)
        req.httpBody = try JSONSerialization.data(withJSONObject: [
            "text": question,
            "source_surface": "keyboard",
        ])

        let (bytes, response) = try await session.bytes(for: req)
        if let http = response as? HTTPURLResponse, !(200...299).contains(http.statusCode) {
            if http.statusCode == 404 || http.statusCode == 410 {
                Self.store.removeObject(forKey: Self.sessionKey)   // session gone — recreate next time
            }
            throw URLError(.badServerResponse)
        }

        var accumulated = ""
        for try await line in bytes.lines {
            guard line.hasPrefix("data:") else { continue }
            let json = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
            if let d = json.data(using: .utf8),
               let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
               let token = obj["text"] as? String, !token.isEmpty {
                accumulated += token
                onToken(accumulated)
            }
        }
    }

    // MARK: internals

    private func authorize(_ req: inout URLRequest) {
        MagicianAccess.authorize(&req)
    }
}
