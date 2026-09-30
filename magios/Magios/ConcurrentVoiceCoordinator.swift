import Foundation
import Combine
import UIKit

struct VoiceRequestRecord: Decodable, Identifiable, Equatable {
    let id: String
    let parentSessionId: String
    let branchSessionId: String
    let title: String
    let workStatus: String
    let deliveryStatus: String
    var speechText: String?
    var resultMessageId: String?
    var pendingTasks: [String]?
    var taskNotification: Bool?
    let createdAt: Int64
    let updatedAt: Int64
    var error: String?
    var readAt: Int64?
    var uiThreadId: String?
    var contextSessionId: String?
    func visible(selectedId: String?) -> Bool {
        running || ["claimed", "playing"].contains(deliveryStatus) ||
            (deliveryStatus != "dismissed" && (id == selectedId || (readAt == nil && deliveryStatus != "played")))
    }
    var running: Bool {
        ["accepted", "running"].contains(workStatus) || (!(pendingTasks ?? []).isEmpty && workStatus != "cancelled")
    }
    var label: String {
        if !(pendingTasks ?? []).isEmpty && workStatus != "cancelled" { return "Task running" }
        switch workStatus {
        case "accepted": return "Queued"
        case "running": return "Working"
        case "completed": return deliveryStatus == "played" ? "Answered" : readAt != nil ? "Read" : "Ready"
        default: return workStatus.capitalized
        }
    }
}

struct VoiceDeliveryOutput: Decodable {
    let deviceId: String
    let interactionId: String
    let epoch: Int64
    var expiresAt: Int64?
}

struct VoiceRequestSnapshot: Decodable {
    let revision: Int64
    let requests: [VoiceRequestRecord]
    var output: VoiceDeliveryOutput?
}

/// Shares the backend ledger with web and Android. Capture owns a frozen context;
/// only the device holding the output lease may speak, and completion means audio ended.
@MainActor
final class ConcurrentVoiceCoordinator: ObservableObject {
    static let shared = ConcurrentVoiceCoordinator()
    typealias Request = (String, [String: Any]?) async throws -> Data
    typealias Playback = (VoiceRequestRecord, @escaping () -> Void, @escaping (SpeechPlaybackResult) -> Void) -> Void
    @Published private(set) var requests: [VoiceRequestRecord] = []
    @Published private(set) var focus: VoiceRequestRecord?
    @Published private(set) var speaking: String?
    @Published var error: String?
    var available: [VoiceRequestRecord] { requests.filter { $0.visible(selectedId: focus?.id) }.reversed() }
    var screenActive = false
    var voiceInteracted = false
    var liveActive = false
    var foregroundBusy: () -> Bool = { false }
    var liveOutputBusy: () -> Bool = { false }
    var focusChanged: (VoiceRequestRecord?) -> Void = { _ in }
    private var snapshot = VoiceRequestSnapshot(revision: -1, requests: [])
    private let request: Request
    private let play: Playback
    private let stopPlayback: () -> Void
    private let now: () -> TimeInterval
    private let deviceId = UUID().uuidString
    private var interactionId = UUID().uuidString
    private var focusEpoch: Int64 = 0
    private var captureFocus: VoiceRequestRecord?
    private var capturing = false
    private var inputPending = false
    private var active = false
    private var tickBusy = false
    private var quietAfter: TimeInterval = 0
    private var lastLease: TimeInterval = 0
    private var generation = 0
    private var replays: [String] = []
    private var monitor: Task<Void, Never>?
    private var scopeKey = ""
    private final class Playing {
        let row: VoiceRequestRecord
        let attempt = UUID().uuidString
        let output: Int64
        let focusEpoch: Int64
        var started = false
        init(_ row: VoiceRequestRecord, output: Int64, focusEpoch: Int64) {
            self.row = row; self.output = output; self.focusEpoch = focusEpoch
        }
    }
    private var playing: Playing?
    init(request: Request? = nil, play: Playback? = nil, stop: (() -> Void)? = nil, now: @escaping () -> TimeInterval = { Date().timeIntervalSince1970 }) {
        self.request = request ?? Self.httpRequest
        self.play = play ?? { row, started, finished in
            SpeechSynthesizer.shared.speak(row.speechText ?? "", messageId: "voice-delivery-\(row.id)", focusPolicy: .concurrentVoiceOwner, onStart: started, completion: finished)
        }
        self.stopPlayback = stop ?? { SpeechSynthesizer.shared.stop() }
        self.now = now
    }
    static func httpRequest(_ path: String, _ body: [String: Any]?) async throws -> Data {
        var components = URLComponents(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2\(path)")!
        components.queryItems = [URLQueryItem(name: "workspace", value: MagicianAccess.workspace)]
        var request = URLRequest(url: components.url!)
        request.timeoutInterval = 30
        MagicianAccess.authorize(&request)
        if let body {
            request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let response = response as? HTTPURLResponse, (200...299).contains(response.statusCode) else {
            let problem = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            throw NSError(domain: "VoiceRequests", code: (response as? HTTPURLResponse)?.statusCode ?? 0,
                          userInfo: [NSLocalizedDescriptionKey: problem?["error"] as? String ?? "Voice requests are unavailable."])
        }
        return data
    }
    static func decode<T: Decodable>(_ type: T.Type, _ data: Data) throws -> T {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }
    func start() {
        guard monitor == nil, !isRunningUnderTests else { return }
        monitor = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                let key = "\(MagicianAccess.baseURL)|\(MagicianAccess.principal)|\(MagicianAccess.workspace)"
                if self.scopeKey.isEmpty { self.scopeKey = key }
                else if self.scopeKey != key { self.reset(); self.scopeKey = key }
                await self.tick()
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }
    func activate() { active = true; start() }
    func select(_ row: VoiceRequestRecord?) {
        focusEpoch += 1; focus = row
        if !capturing && !inputPending { captureFocus = row }
        focusChanged(row)
    }
    func captureStarted() {
        activate(); voiceInteracted = true; captureFocus = focus; focusEpoch += 1
        capturing = true; inputPending = true
        if playing != nil { stopPlayback() }
    }
    func captureStopped() { capturing = false }
    func inputSettled() { inputPending = false; captureFocus = focus; quietAfter = now() + 0.5 }
    func foregroundStarted() { focusEpoch += 1; if playing != nil { stopPlayback() } }
    func foregroundStopped() { quietAfter = now() + 0.5 }
    func target(_ parent: String) -> (parent: String, context: String?) {
        captureFocus.map { ($0.parentSessionId, $0.branchSessionId) } ?? (parent, nil)
    }
    func replay(_ id: String) { activate(); if !replays.contains(id) && playing?.row.id != id { replays.append(id) } }
    func reset() {
        generation += 1; active = false; focusEpoch += 1
        if playing != nil { stopPlayback() }; playing = nil; speaking = nil
        snapshot = VoiceRequestSnapshot(revision: -1, requests: []); requests = []; focus = nil; captureFocus = nil
        capturing = false; inputPending = false; replays = []; lastLease = 0
        interactionId = UUID().uuidString; voiceInteracted = false; error = nil
        focusChanged(nil)
    }
    func deactivate() {
        active = false; focusEpoch += 1; capturing = false; inputPending = false
        if playing != nil { stopPlayback() }
        if let output = snapshot.output, output.deviceId == deviceId, output.interactionId == interactionId {
            let body = identity(output.epoch).merging(["action": "release"]) { _, value in value }
            Task { try? await command(body) }
        }
    }
    func cancel(_ id: String) async throws {
        update(try Self.decode(VoiceRequestSnapshot.self, await request("/media/voice/requests/\(id)/cancel", [:])))
    }
    func dismiss(_ id: String) async throws { try await command(["action": "dismiss", "request_id": id]) }
    func markRead(_ id: String) async throws { try await command(["action": "read", "request_id": id]) }
    func result(_ id: String) async throws -> String {
        let data = try await request("/media/voice/requests/\(id)/result", nil)
        let object = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        let content = object?["content"] as? [String: Any]
        return content?["text"] as? String ?? content?["summary"] as? String ?? "Open Review work for the complete result."
    }
    func action(_ work: @escaping () async throws -> Void) { Task { do { try await work() } catch { self.error = error.localizedDescription } } }
    func submit(_ parent: String, text: String, options: [String: Any], voiceInput: Bool = false) async throws {
        let target = voiceInput ? target(parent) : (parent: parent, context: nil as String?)
        activate(); defer { if voiceInput { inputSettled() } }
        var cancellation = text.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: ".!?")).lowercased()
        if cancellation.hasPrefix("please ") { cancellation = String(cancellation.dropFirst(7)) }
        let current = ["cancel that request", "cancel this request", "cancel the current request"].contains(cancellation)
        if current || ["cancel the previous request", "cancel all background requests"].contains(cancellation) {
            update(try Self.decode(VoiceRequestSnapshot.self, await request("/media/voice/requests", nil)))
            let all = cancellation == "cancel all background requests"
            var rows = requests.filter { $0.taskNotification != true && $0.running &&
                (all || (current && target.context != nil ? $0.branchSessionId == target.context : $0.parentSessionId == target.parent)) }.sorted { $0.createdAt > $1.createdAt }
            if !all { rows = Array(rows.prefix(1)) }
            guard !rows.isEmpty else { throw NSError(domain: "VoiceRequests", code: 404, userInfo: [NSLocalizedDescriptionKey: "No matching background request is running."]) }
            for row in rows { try await cancel(row.id) }; return
        }
        var body = options; body["text"] = text; body["submission_id"] = UUID().uuidString
        body["context_session_id"] = target.context
        let path = "/chat/sessions/\(target.parent)/voice/requests"
        do { _ = try await request(path, body) }
        catch is URLError { _ = try await request(path, body) } // exact same idempotency key
    }
    private func safe() -> Bool {
        active && screenActive && !capturing && !inputPending && now() >= quietAfter &&
        (voiceInteracted || liveActive || !replays.isEmpty || AudioSettings.shared.speakReplies) &&
        !TutorAudioFocus.shared.isActive && !foregroundBusy() && !liveOutputBusy() && SpeechSynthesizer.shared.activeMessageId == nil
    }
    private func identity(_ epoch: Int64) -> [String: Any] { ["device_id": deviceId, "interaction_id": interactionId, "epoch": epoch] }
    private func update(_ value: VoiceRequestSnapshot) {
        guard value.revision >= snapshot.revision else { return }
        snapshot = value; requests = value.requests; error = nil
        if let prior = focus {
            if let current = requests.first(where: { $0.id == prior.id && $0.deliveryStatus != "dismissed" }) { focus = current }
            else { select(nil) }
        }
    }
    private func command(_ body: [String: Any]) async throws {
        let gen = generation
        let value = try Self.decode(VoiceRequestSnapshot.self, await request("/media/voice/delivery", body))
        if gen == generation { update(value) }
    }
    private func receipt(_ p: Playing, _ event: String) async throws {
        try await command(identity(p.output).merging(["action": "playback", "request_id": p.row.id, "attempt_id": p.attempt, "event": event]) { _, value in value })
    }
    func tick() async {
        guard !tickBusy else { return }; tickBusy = true; defer { tickBusy = false }
        let gen = generation
        do {
            let value = try Self.decode(VoiceRequestSnapshot.self, await request("/media/voice/requests", nil))
            guard gen == generation else { return }; update(value)
            guard active else { return }
            if playing == nil, let owner = snapshot.output, Double(owner.expiresAt ?? 0) > now() * 1000,
               owner.deviceId != deviceId || owner.interactionId != interactionId { return }
            if (playing != nil || safe()) && (now() - lastLease > 8 || snapshot.output == nil) {
                try await command(["action": "acquire", "device_id": deviceId, "interaction_id": interactionId]); lastLease = now()
            }
            guard gen == generation else { return }
            if let playing { try await receipt(playing, "progress"); return }
            guard safe() else { return }
            let replay = replays.first
            let row = replay.flatMap { id in requests.first { $0.id == id && $0.speechText != nil } } ??
                (replay == nil ? requests.filter { $0.deliveryStatus == "pending" && $0.readAt == nil && $0.speechText != nil }.min { $0.updatedAt < $1.updatedAt } : nil)
            guard let row, let output = snapshot.output else { if replay != nil { replays.removeFirst() }; return }
            let p = Playing(row, output: output.epoch, focusEpoch: focusEpoch)
            try await command(identity(output.epoch).merging(["action": "claim", "request_id": row.id, "attempt_id": p.attempt, "focus_epoch": focusEpoch, "replay": replay == row.id]) { _, value in value })
            guard gen == generation else { return }
            guard safe(), p.focusEpoch == focusEpoch else { try await receipt(p, "rejected"); return }
            replays.removeAll { $0 == row.id }
            playing = p
            var startedReceipt: Task<Void, Never>?
            play(row, { [weak self] in
                guard let self else { return }
                guard active, !capturing, !inputPending, gen == generation, p.focusEpoch == focusEpoch else { stopPlayback(); return }
                guard !p.started else { return }; p.started = true; focus = row; captureFocus = row; speaking = row.id; focusChanged(row)
                startedReceipt = Task { do { try await self.receipt(p, "started") } catch { self.stopPlayback(); self.error = error.localizedDescription } }
            }, { [weak self] outcome in
                Task { @MainActor in
                    guard let self else { return }
                    await startedReceipt?.value
                    do { if gen == self.generation { try await self.receipt(p, outcome == .completed && p.started ? "completed" : "interrupted") } }
                    catch { self.error = error.localizedDescription }
                    if self.playing === p { self.playing = nil; self.speaking = nil; self.quietAfter = self.now() + 0.5 }
                }
            })
        } catch { if playing != nil { stopPlayback() }; if gen == generation { self.error = error.localizedDescription } }
    }
}
