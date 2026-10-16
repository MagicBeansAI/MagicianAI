import Combine
import Foundation

/// A large program roster must not launch one overview request per member at once.
private actor WorkProgramReadGate {
    static let shared = WorkProgramReadGate()
    private var active = 0
    private var waiting: [CheckedContinuation<Void, Never>] = []
    func request(_ client: WorkAPIClient, path: String) async throws -> (WorkJSON, HTTPURLResponse) {
        if active < 4 { active += 1 }
        else { await withCheckedContinuation { waiting.append($0) } }
        defer {
            if waiting.isEmpty { active -= 1 }
            else { waiting.removeFirst().resume() }
        }
        try Task.checkCancellation()
        guard client.isCurrentConnection else { throw URLError(.cancelled) }
        return try await client.request(path)
    }
}

extension WorkJSON {
    subscript(_ key: String) -> WorkJSON { object?[key] ?? .null }
    var display: String {
        switch self {
        case .string(let value): return value
        case .integer(let value): return String(value)
        case .unsigned(let value): return String(value)
        case .number(let value): return String(value)
        case .bool(let value): return value ? "Yes" : "No"
        case .null: return "—"
        default: return formatted
        }
    }
    static func parseObject(_ text: String) throws -> WorkJSON {
        let value = try JSONDecoder().decode(WorkJSON.self, from: Data(text.utf8))
        guard value.object != nil else { throw WorkAPIError.invalidResponse }
        return value
    }
}

/// Every mounted resource has its own generation, including writes awaited
/// after a confirmation. Switching connection immediately clears old content.
@MainActor
final class WorkResource: ObservableObject {
    @Published private(set) var scopeID = UUID()
    @Published private(set) var value: WorkJSON?
    @Published private(set) var loading = false
    @Published private(set) var busy = false
    @Published var error: String?
    @Published private(set) var notice: String?
    private var generation = UUID()
    private var client: WorkAPIClient?
    private var loadTask: Task<Void, Never>?
    private var connectionObserver: AnyCancellable?
    private(set) var path = ""
    private var pendingPath: String?
    private var invalidated = false
    private let clientFactory: () throws -> WorkAPIClient

    init(clientFactory: @escaping () throws -> WorkAPIClient = { try .connected() }) {
        self.clientFactory = clientFactory
        connectionObserver = NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)
            .receive(on: RunLoop.main).sink { [weak self] _ in self?.connectionChanged() }
    }
    deinit { loadTask?.cancel() }
    func load(_ path: String) {
        guard !busy else { pendingPath = path; return }
        pendingPath = nil
        guard !invalidated, client?.isCurrentConnection != false else {
            clear(); invalidated = true
            error = "Connection changed. Reopen this work view."; return
        }
        loadTask?.cancel()
        if self.path != path { value = nil; notice = nil; scopeID = UUID() }
        self.path = path
        let ticket = UUID(); generation = ticket; loading = true; error = nil
        do { client = try clientFactory() }
        catch { self.error = error.localizedDescription; loading = false; return }
        guard let client else { return }
        loadTask = Task { [weak self] in
            do {
                let (payload, response) = try await (path.hasSuffix("/harness-overview") ? WorkProgramReadGate.shared.request(client, path: path) : client.request(path))
                var next = payload
                if path.range(of: "^/api/magician/v2/agents/[^/?]+$", options: .regularExpression) != nil, next.object != nil, next["etag"].text == nil, let etag = response.value(forHTTPHeaderField: "ETag"), !etag.isEmpty { next = next.setting(["etag"], to: .string(etag)) }
                guard let self, !Task.isCancelled, generation == ticket, client.isCurrentConnection else { return }
                value = next; loading = false
            } catch {
                guard let self, !Task.isCancelled, generation == ticket, client.isCurrentConnection else { return }
                self.error = error.localizedDescription; loading = false
            }
        }
    }
    func file(_ path: String) async -> Data? {
        guard !busy, let client, client.isCurrentConnection else { error = "Connection changed. Reopen this view."; return nil }
        let ticket = generation; busy = true; error = nil; notice = nil
        defer { finishRequest(ticket) }
        do {
            let data = try await client.file(path)
            guard generation == ticket, client.isCurrentConnection else { return nil }
            return data
        } catch {
            guard generation == ticket, client.isCurrentConnection else { return nil }
            self.error = error.localizedDescription; return nil
        }
    }
    func refresh() { if let next = pendingPath ?? (path.isEmpty ? nil : path) { load(next) } }
    func clear() {
        scopeID = UUID()
        loadTask?.cancel(); generation = UUID(); client = nil
        pendingPath = nil
        value = nil; error = nil; notice = nil; loading = false; busy = false
    }
    private func connectionChanged() {
        invalidated = true; clear()
        error = "Connection changed. Reopen this work view."
    }
    private func finishRequest(_ ticket: UUID) {
        guard generation == ticket else { return }
        busy = false
        if let next = pendingPath {
            let requestError = error, requestNotice = notice
            load(next)
            if !invalidated {
                error = requestError ?? error
                notice = requestNotice ?? notice
            }
        }
    }

    /// Return the exact reviewed response to the caller; never publish a late
    /// result or allow a stale screen to write to a previous connection.
    func perform(_ path: String, method: String = "POST", body: WorkJSON? = .object([:]), etag: String? = nil, successNotice: String? = "Saved") async -> WorkJSON? {
        guard !busy, let client, client.isCurrentConnection else {
            error = "Connection changed. Refresh before continuing."; return nil
        }
        let ticket = generation; busy = true; error = nil; notice = nil
        defer { finishRequest(ticket) }
        do {
            let result = try await client.request(path, method: method, body: body, etag: etag).0
            guard generation == ticket, client.isCurrentConnection else { return nil }
            notice = method == "GET" ? nil : successNotice; return result
        } catch {
            guard generation == ticket, client.isCurrentConnection else { return nil }
            self.error = error.localizedDescription; return nil
        }
    }
}
