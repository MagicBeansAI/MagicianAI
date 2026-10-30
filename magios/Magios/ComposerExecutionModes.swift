import Foundation
import SwiftUI
import QuickLook

struct ChatGodModeState: Decodable {
    let enabled: Bool
    let overrideChoice: Bool?
    let defaultEnabled: Bool
    /// God mode's asks approved without asking, except deletes, root and unseen code.
    let autoApprove: Bool?
    enum CodingKeys: String, CodingKey { case enabled; case overrideChoice = "override", defaultEnabled = "default", autoApprove = "auto_approve" }
}

struct SuperGodModeState: Decodable {
    let active: Bool
    let enabled: Bool
    let untilMs: Int64?
    let remainingSecs: Int?
    enum CodingKeys: String, CodingKey { case active, enabled; case untilMs = "until_ms", remainingSecs = "remaining_secs" }
    func activeAt(_ now: Date = Date()) -> Bool {
        active && (untilMs.map { Double($0) > now.timeIntervalSince1970 * 1000 } ?? true)
    }
    func endLabel(_ now: Date = Date()) -> String {
        guard let untilMs else { return "until turned off" }
        return "\(max(1, Int(ceil((Double(untilMs) / 1000 - now.timeIntervalSince1970) / 60)))) min left"
    }
}

enum SuperGodModeDuration: String { case hour = "1h", untilOff = "forever" }

enum ExecutionModeError: LocalizedError {
    case unavailable(String)
    var errorDescription: String? { if case .unavailable(let message) = self { return message }; return nil }
}

/// Native controls use the same owner boundary as the web, with one credential
/// snapshot for each request. No local preference grants execution authority.
struct ExecutionModeAPI {
    var session: URLSession = .shared
    var connection: () -> MobileConnectionProfile? = { MagicianAccess.connectionProfile }
    /// Capture before scheduling work so a pending save cannot follow a host switch.
    func frozen() -> Self {
        let profile = connection()
        var copy = self
        copy.connection = { profile }
        return copy
    }
    func chat(_ id: String) async throws -> ChatGodModeState { try await request(path: chatPath(id)) }
    func saveChat(_ id: String, enabled: Bool?) async throws -> ChatGodModeState {
        try await request(path: chatPath(id), body: ["enabled": enabled.map { $0 as Any } ?? NSNull()])
    }
    /// `nil` follows the Settings default.
    func saveChatAutoApprove(_ id: String, _ autoApprove: Bool?) async throws -> ChatGodModeState {
        try await request(path: chatPath(id), body: ["auto_approve": autoApprove.map { $0 as Any } ?? NSNull()])
    }
    func superMode() async throws -> SuperGodModeState { try await request(path: "/settings/super-god-mode") }
    func saveSuper(_ duration: SuperGodModeDuration?) async throws -> SuperGodModeState {
        var body: [String: Any] = ["enabled": duration != nil]
        if let duration { body["duration"] = duration.rawValue }
        return try await request(path: "/settings/super-god-mode", body: body)
    }
    private func chatPath(_ id: String) -> String {
        let segment = id.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-_.~"))) ?? ""
        return "/chat/sessions/\(segment)/god-mode"
    }
    private func request<T: Decodable>(path: String, body: [String: Any]? = nil) async throws -> T {
        guard let profile = connection(),
              var components = URLComponents(url: profile.publicOrigin, resolvingAgainstBaseURL: false) else {
            throw ExecutionModeError.unavailable("Connect to your Magician in Settings.")
        }
        components.percentEncodedPath = components.percentEncodedPath.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            .split(separator: "/").map(String.init).reduce("") { $0 + "/" + $1 } + "/api/magician/v2" + path
        guard let url = components.url else { throw ExecutionModeError.unavailable("Invalid Magician URL.") }
        var request = URLRequest(url: url)
        request.httpMethod = body == nil ? "GET" : "PUT"
        request.timeoutInterval = 30
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.headers(profile: profile).forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw ExecutionModeError.unavailable("Invalid execution mode response.") }
        guard (200..<300).contains(http.statusCode) else {
            let error = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            throw ExecutionModeError.unavailable(error?["message"] as? String ?? "Execution mode request failed (\(http.statusCode)).")
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
}

/// The execution-mode menu's in-flight save. A finished save always releases
/// the menu, even when the chat changed while it ran; after a reset (chat
/// switch, reconnect) an older save finishing cannot release a newer one.
struct ExecutionModeSaveGate: Equatable {
    private(set) var busy = false
    private var generation = 0
    /// Starts a save and returns its token; nil while one is in flight.
    mutating func begin() -> Int? {
        guard !busy else { return nil }
        busy = true; generation += 1
        return generation
    }
    mutating func finish(_ save: Int) { if save == generation { busy = false } }
    mutating func reset() { busy = false; generation += 1 }
}

struct ComposerPrivilegePreference: PreferenceKey {
    static var defaultValue: String? { nil }
    static func reduce(value: inout String?, nextValue: () -> String?) { value = nextValue() ?? value }
}

@MainActor
struct ComposerExecutionModes: View {
    let sessionID: String?
    var showChatMode = true
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.scenePhase) private var scenePhase
    @State private var chat: ChatGodModeState?
    @State private var superMode: SuperGodModeState?
    @State private var saveGate = ExecutionModeSaveGate()
    @State private var loadError: String?
    @State private var actionError: String?
    @State private var confirmation: Choice?
    @State private var connectionRevision = 0
    @State private var requestRevision = 0
    @State private var now = Date()
    @State private var loadedContext: String?
    @State private var loadedProfile: MobileConnectionProfile?
    private let api = ExecutionModeAPI()
    private enum Choice: Equatable { case chatOn, chatOff, chatDefault, autoOn, autoOff, superHour, superForever, superOff }
    private var busy: Bool { saveGate.busy }
    private var context: String { "\(MagicianAccess.activeOrigin ?? "")|\(connectionRevision)|\(sessionID ?? "")" }
    private var superActive: Bool { superMode?.activeAt(now) == true }
    private var label: String {
        if superActive { return "Super God · all chats · \(superMode!.endLabel(now))" }
        if chat?.enabled == true && chat?.autoApprove == true { return "God mode · auto-approve · this chat" }
        if chat?.enabled == true { return "God mode · this chat" }
        return showChatMode ? "God modes" : "Super God"
    }
    private var chatReady: Bool { chat != nil && !(sessionID ?? "").isEmpty && !busy }

    var body: some View {
        HStack(spacing: 4) {
            modeMenu
            if showChatMode && chat?.enabled == true {
                Button {
                    if chat?.autoApprove == true { perform(.autoOff) } else { confirmation = .autoOn }
                } label: {
                    Label("Auto-approve", systemImage: chat?.autoApprove == true ? "checkmark.circle.fill" : "circle")
                        .font(.themed(10, weight: .semibold))
                        .foregroundStyle(chat?.autoApprove == true ? theme.onAccentColor : theme.textColor)
                        .padding(.horizontal, 7).padding(.vertical, 5)
                        .background(chat?.autoApprove == true ? theme.accentFillColor : theme.surfaceColor,
                                    in: RoundedRectangle(cornerRadius: 6))
                        .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.cardBorderColor, lineWidth: 1))
                        .frame(minHeight: 44)
                }.buttonStyle(.plain).disabled(!chatReady)
                    .accessibilityValue(chat?.autoApprove == true ? "On" : "Off")
            }
        }
        .preference(key: ComposerPrivilegePreference.self, value: loadError != nil ? nil : superActive ? "Super God mode" : chat?.enabled == true ? (chat?.autoApprove == true ? "God mode · Auto-approve" : "God mode") : nil)
    }

    private var modeMenu: some View {
        Menu {
            if showChatMode {
                Button(chat?.enabled == true ? "Turn off God mode for this chat" : "Turn on God mode for this chat") {
                    if chat?.enabled == true { perform(.chatOff) } else { confirmation = .chatOn }
                }.disabled(!chatReady)
                Button(chat?.autoApprove == true ? "Turn off auto-approve for this chat" : "Auto-approve this chat") {
                    if chat?.autoApprove == true { perform(.autoOff) } else { confirmation = .autoOn }
                }.disabled(!chatReady || chat?.enabled != true)
                Button("Use Settings default for this chat") {
                    if chat?.enabled == false && chat?.defaultEnabled == true { confirmation = .chatDefault }
                    else { perform(.chatDefault) }
                }.disabled(!chatReady)
                Divider()
            }
            if superActive {
                Button("Turn off Super God mode", role: .destructive) { perform(.superOff) }
            } else {
                Button("Super God mode for 1 hour") { confirmation = .superHour }.disabled(superMode == nil)
                Button("Super God mode until turned off") { confirmation = .superForever }.disabled(superMode == nil)
            }
            Text("Super God mode applies to every chat and run on this backend.")
            if let loadError { Text(loadError) }
            Button("Refresh modes") { Task { await refresh() } }
        } label: {
            Label(label, systemImage: "shield.lefthalf.filled")
                .labelStyle(.iconOnly)
                .font(.system(size: 16))
                .foregroundStyle(superActive || chat?.enabled == true ? theme.dangerColor : theme.secondaryTextColor)
                .frame(minWidth: 44, minHeight: 44)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(busy)
        .accessibilityLabel(label)
        .accessibilityIdentifier("composer-execution-modes")
        .confirmationDialog(confirmation == .autoOn ? "Auto-approve God mode in this chat?" : "Enable \(confirmation == .chatOn || confirmation == .chatDefault ? "God mode for this chat" : "Super God mode")?",
            isPresented: Binding(get: { confirmation != nil }, set: { if !$0 { confirmation = nil } }), titleVisibility: .visible) {
            if let choice = confirmation {
                Button("Enable", role: .destructive) { confirmation = nil; perform(choice) }
            }
            Button("Cancel", role: .cancel) { confirmation = nil }
        } message: {
            if confirmation == .autoOn {
                Text("What God mode would ask you in this chat and the runs it starts is approved without asking. It still asks before anything that deletes (rm -r or -f, find -delete, git reset --hard, a forced push), anything run as root (sudo, doas), code a command does not show, and the blocked list.")
            } else if confirmation == .chatOn || confirmation == .chatDefault {
                Text("This chat and runs it starts may request shell, file and network access beyond the normal sandbox. You still approve operations once, for this session or always.")
            } else {
                Text("Applies to all chats and runs on this backend. After launch approval, harnesses run without sandbox restrictions. \(confirmation == .superHour ? "Turns off after one hour." : "Stays on until you turn it off.")")
            }
        }
        .alert("Execution mode could not be saved", isPresented: Binding(get: { actionError != nil }, set: { if !$0 { actionError = nil } })) {
            Button("OK", role: .cancel) { actionError = nil }
        } message: { Text(actionError ?? "") }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            connectionRevision += 1; requestRevision += 1
            chat = nil; superMode = nil; confirmation = nil; saveGate.reset(); actionError = nil; loadedContext = nil; loadedProfile = nil
        }
        .task(id: "\(context)|\(scenePhase == .active)") {
            // A save still running for the previous chat must not keep this
            // chat's menu disabled.
            chat = nil; superMode = nil; confirmation = nil; loadedContext = nil; loadedProfile = nil; saveGate.reset()
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                if !busy && confirmation == nil { await refresh() }
                do { try await Task.sleep(nanoseconds: 15_000_000_000) } catch { return }
            }
        }
    }
    private func refresh() async {
        guard !busy else { return }
        requestRevision += 1
        let revision = requestRevision, target = context
        let requestAPI = api.frozen(), profile = requestAPI.connection()
        now = Date()
        do {
            let newChat = showChatMode && sessionID != nil ? try await requestAPI.chat(sessionID!) : nil
            let newSuper = try await requestAPI.superMode()
            guard !Task.isCancelled, target == context, revision == requestRevision, profile == api.connection() else { return }
            chat = newChat; superMode = newSuper; loadError = nil; now = Date(); loadedContext = target; loadedProfile = profile
        } catch {
            guard !Task.isCancelled, target == context, revision == requestRevision else { return }
            loadError = error.localizedDescription
        }
    }
    private func perform(_ choice: Choice) {
        let requestAPI = api.frozen()
        guard loadedContext == context, let profile = loadedProfile,
              profile == requestAPI.connection(), let save = saveGate.begin() else { return }
        requestRevision += 1
        let target = context, revision = requestRevision, id = sessionID
        Task {
            defer { saveGate.finish(save) }
            guard !Task.isCancelled, target == context, revision == requestRevision, profile == api.connection() else { return }
            do {
                switch choice {
                case .chatOn, .chatOff, .chatDefault:
                    if let id {
                        let result = try await requestAPI.saveChat(id, enabled: choice == .chatDefault ? nil : choice == .chatOn)
                        if target == context && revision == requestRevision && profile == api.connection() { chat = result }
                    }
                case .autoOn, .autoOff:
                    if let id {
                        let result = try await requestAPI.saveChatAutoApprove(id, choice == .autoOn)
                        if target == context && revision == requestRevision && profile == api.connection() { chat = result }
                    }
                case .superHour, .superForever, .superOff:
                    let duration: SuperGodModeDuration? = choice == .superOff ? nil : choice == .superHour ? .hour : .untilOff
                    let result = try await requestAPI.saveSuper(duration)
                    if target == context && revision == requestRevision && profile == api.connection() { superMode = result; now = Date() }
                }
            } catch { if target == context { actionError = error.localizedDescription } }
        }
    }
}

/// Local display preferences and unsent text use the same server/account/chat boundary.
struct ChatDraftStore {
    let defaults: UserDefaults
    func read(_ key: String) -> String { defaults.string(forKey: key + "|draft") ?? "" }
    func save(_ text: String, key: String) {
        if text.isEmpty { defaults.removeObject(forKey: key + "|draft") }
        else { defaults.set(text, forKey: key + "|draft") }
    }
}

enum ChatSessionAccounting {
    static func fileSize(_ file: WorkJSON) -> String {
        guard let size = Int64(file["size"].display), size >= 0 else { return "Size unavailable" }
        return ByteCountFormatter.string(fromByteCount: size, countStyle: .binary)
    }
    static func fileSizeSummary(_ inventory: WorkJSON) -> String {
        let files = inventory["files"].array ?? []
        let sizes = files.compactMap { Int64($0["size"].display).flatMap { $0 >= 0 ? $0 : nil } }
        let total = sizes.reduce(Int64(0)) { sum, size in sum > Int64.max - size ? Int64.max : sum + size }
        let unknown = files.count - sizes.count
        let partial = (Int(inventory["unavailable_files"].display) ?? 0) > 0 || !(inventory["unavailable_tasks"].array ?? []).isEmpty
        return "\(files.count) \(files.count == 1 ? "file" : "files") · \(unknown > 0 || partial ? "Known size" : "Total"): \(ByteCountFormatter.string(fromByteCount: total, countStyle: .binary))"
            + (unknown > 0 ? " · \(unknown) \(unknown == 1 ? "size" : "sizes") unavailable" : "")
    }
    static func recordedDate(_ raw: String) -> Date? {
        if let milliseconds = Double(raw) { return Date(timeIntervalSince1970: milliseconds / 1000) }
        let precise = ISO8601DateFormatter()
        precise.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return precise.date(from: raw) ?? ISO8601DateFormatter().date(from: raw)
    }
    static func sql(_ session: String, byModel: Bool = false) -> String {
        let id = "'" + session.replacingOccurrences(of: "'", with: "''") + "'"
        return "SELECT \(byModel ? "provider, model, " : "")SUM(measured_calls) AS calls, " +
            "CASE WHEN SUM(token_calls) > 0 THEN SUM(input_tokens) END AS input_tokens, " +
            "CASE WHEN SUM(token_calls) > 0 THEN SUM(output_tokens) END AS output_tokens, " +
            "CASE WHEN SUM(cache_read_calls) > 0 THEN SUM(cache_read_tokens) END AS cache_read_tokens, " +
            "CASE WHEN SUM(cache_write_calls) > 0 THEN SUM(cache_creation_tokens) END AS cache_creation_tokens, " +
            "CASE WHEN SUM(priced_calls) > 0 THEN SUM(cost_usd) END AS cost_usd, " +
            "SUM(measured_calls) - SUM(priced_calls) AS unpriced_calls FROM llm_cost_daily WHERE chat_session_id = \(id)" +
            (byModel ? " GROUP BY provider, model ORDER BY cost_usd DESC NULLS LAST, calls DESC" : "")
    }
    static func outputPath(_ raw: String) -> String? {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URLComponents(string: text), url.scheme == nil, url.host == nil,
              url.query == nil, url.fragment == nil, !text.hasPrefix("//") else { return nil }
        let path = url.path
        guard !["/api/", "/work", "/notes", "/tasks", "/chat", "/t/", "/settings", "/attention", "/vibedev"].contains(where: { path.hasPrefix($0) }),
              !path.split(separator: "/").contains(".."),
              path.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }),
              path.contains("/") || !(path as NSString).pathExtension.isEmpty else { return nil }
        return path
    }
    @MainActor
    static func openOnHost(_ path: String, session: String, client: WorkAPIClient) async throws {
        guard client.isCurrentConnection else { throw URLError(.cancelled) }
        _ = try await client.request("/api/magician/v2/chat/sessions/\(WorkAPIClient.segment(session))/outputs/open-file",
                                     method: "POST", body: .object(["absolute_path": .string(path)]))
        guard client.isCurrentConnection else { throw URLError(.cancelled) }
    }
}

enum ChatContextPage: String, CaseIterable {
    case tasks, settings, files, models
    var title: String {
        switch self {
        case .tasks: return "Tasks"
        case .settings: return "Thread settings"
        case .files: return "Files"
        case .models: return "Models"
        }
    }
}

struct ChatContextSheet: Identifiable {
    let sessionID: String
    let threadID: String?
    let page: ChatContextPage
    let client: WorkAPIClient?
    var id: String { "\(sessionID)|\(threadID ?? "")|\(page.rawValue)" }
}

struct ChatSessionDetails: View {
    let sessionID: String
    let page: ChatContextPage
    let client: WorkAPIClient?
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var files: WorkJSON?
    @State private var usage: WorkJSON?
    @State private var models: WorkJSON?
    @State private var error: String?
    @State private var loading = true
    @State private var revision = 0
    @State private var asset: ChatAssetSelection?
    @State private var loadedClient: WorkAPIClient?
    @State private var pendingDelete: WorkJSON?
    @State private var deleting = false
    private var totals: [String: WorkJSON] {
        let columns = usage?["columns"].array ?? [], row = usage?["rows"].array?.first?.array ?? []
        return Dictionary(columns.enumerated().compactMap { index, value in
            guard let key = value.text, index < row.count else { return nil }
            return (key, row[index])
        }, uniquingKeysWith: { _, last in last })
    }
    var body: some View {
        NavigationStack {
            List {
                if loading { ProgressView() }
                if let error { Text(error).foregroundStyle(.red) }
                if page == .models {
                    Section("Whole chat") {
                        usageRow("Calls", "calls")
                        usageRow("Input tokens", "input_tokens")
                        usageRow("Output tokens", "output_tokens")
                        usageRow("Cache read tokens", "cache_read_tokens")
                        usageRow("Cache write tokens", "cache_creation_tokens")
                        usageRow("Cost (USD)", "cost_usd")
                        usageRow("Calls without a price", "unpriced_calls")
                    }
                    Section("Models") {
                        ForEach(models?["rows"].array ?? [], id: \.formatted) { row in
                            let columns = models?["columns"].array ?? []
                            let values = row.array ?? []
                            let fields = Dictionary(columns.enumerated().compactMap { index, column -> (String, WorkJSON)? in
                                guard let key = column.text, index < values.count else { return nil }
                                return (key, values[index])
                            }, uniquingKeysWith: { _, last in last })
                            let value: (String) -> String = { key in fields[key].flatMap { $0 == .null ? nil : $0.display } ?? "Not reported" }
                            VStack(alignment: .leading, spacing: 4) {
                                Text("\(value("model")) · \(value("provider"))").font(.headline)
                                Text("\(value("calls")) calls · Input: \(value("input_tokens")) · Output: \(value("output_tokens"))")
                                Text("Cache read: \(value("cache_read_tokens")) · Cache write: \(value("cache_creation_tokens"))")
                                Text("Cost (USD): \(value("cost_usd")) · Unpriced calls: \(value("unpriced_calls"))")
                            }.font(.caption)
                        }
                        if models?["rows"].array?.isEmpty == true { Text("No model usage recorded yet.") }
                    }
                }
                if page == .files {
                    Section("Files") {
                        if let files { Text(ChatSessionAccounting.fileSizeSummary(files)).font(.caption).foregroundStyle(.secondary) }
                        ForEach(files?["files"].array ?? [], id: \.formatted) { file in
                            Button {
                                guard let path = file["path"].text, let client = loadedClient, client.isCurrentConnection else { return }
                                let base: String
                                if file["source"].text == "task", let task = file["task_id"].text {
                                    base = "/api/magician/v3/tasks/\(WorkAPIClient.segment(task))/outputs/"
                                } else { base = "/api/magician/v2/chat/sessions/\(WorkAPIClient.segment(sessionID))/outputs/" }
                                asset = ChatAssetSelection(path: base + path.split(separator: "/").map { WorkAPIClient.segment(String($0)) }.joined(separator: "/"),
                                    name: file["name"].text ?? "File", client: client)
                            } label: {
                                VStack(alignment: .leading) {
                                    Text(file["name"].text ?? "File")
                                    Text(file["source"].text == "upload" ? "Uploaded" : "Artifact").font(.caption).foregroundStyle(.secondary)
                                    if let task = file["task_id"].text { Text("Task: \(task)").font(.caption).foregroundStyle(.secondary) }
                                    if let execution = file["execution_id"].text { Text("Execution: \(execution)").font(.caption).foregroundStyle(.secondary) }
                                    Text("Chat: \(file["session_id"].text ?? sessionID)").font(.caption).foregroundStyle(.secondary)
                                    Text(fileDate(file)).font(.caption).foregroundStyle(.secondary)
                                    Text(ChatSessionAccounting.fileSize(file)).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            Button("Delete \(file["name"].text ?? "file")", role: .destructive) { pendingDelete = file }
                                .disabled(deleting || loading || loadedClient?.isCurrentConnection != true)
                        }
                        if files?["files"].array?.isEmpty == true { Text("No files in this chat yet.") }
                        if let files, files["unavailable_files"].display != "0" || !(files["unavailable_tasks"].array ?? []).isEmpty {
                            Text("Some files are unavailable. Refresh to retry.").font(.caption)
                        }
                    }
                }
            }
            .navigationTitle(page.title).navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) { Button("Refresh") { revision += 1 }.disabled(deleting) }
            }
            .sheet(item: $asset) { ChatAssetPreview(asset: $0) }
            .confirmationDialog("Delete file from this chat?", isPresented: Binding(get: { pendingDelete != nil }, set: { if !$0 { pendingDelete = nil } }), titleVisibility: .visible) {
                if let file = pendingDelete {
                    Button("Delete \(file["name"].text ?? "file")", role: .destructive) {
                        pendingDelete = nil
                        Task { await delete(file) }
                    }
                }
                Button("Cancel", role: .cancel) { pendingDelete = nil }
            } message: {
                Text("Any chat copy will be permanently removed and existing chat links may stop working. Any task original stays available under Work.")
            }
            .task(id: "\(sessionID)|\(page.rawValue)|\(revision)|\(scenePhase == .active)") {
                if scenePhase == .active { await refresh() }
            }
            .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
                files = nil; usage = nil; models = nil; asset = nil; pendingDelete = nil; loadedClient = nil; dismiss()
            }
        }
    }
    private func delete(_ file: WorkJSON) async {
        guard !deleting, let id = file["id"].text, let client = loadedClient, client.isCurrentConnection else { return }
        deleting = true
        defer { deleting = false }
        do {
            _ = try await client.request("/api/magician/v2/chat/sessions/\(WorkAPIClient.segment(sessionID))/assets/\(WorkAPIClient.segment(id))", method: "DELETE")
            guard client.isCurrentConnection, !Task.isCancelled else { return }
            revision += 1
        } catch { if client.isCurrentConnection { self.error = error.localizedDescription } }
    }
    private func fileDate(_ file: WorkJSON) -> String {
        let raw = file["created_at"].text ?? file["created_at"].display
        let date = ChatSessionAccounting.recordedDate(raw)
        return date?.formatted(date: .abbreviated, time: .shortened) ?? "Date unavailable"
    }
    private func usageRow(_ title: String, _ key: String) -> some View {
        LabeledContent(title, value: totals[key].flatMap { $0 == .null ? nil : $0.display } ?? "Unavailable")
    }
    private func refresh() async {
        files = nil; usage = nil; models = nil; error = nil; loading = true
        do {
            guard let client else { throw ExecutionModeError.unavailable("Connect to your Magician in Settings.") }
            guard client.isCurrentConnection else { dismiss(); return }
            if page == .files {
                do {
                    let (value, _) = try await client.request("/api/magician/v2/chat/sessions/\(WorkAPIClient.segment(sessionID))/assets")
                    guard client.isCurrentConnection, !Task.isCancelled else { return }
                    loadedClient = client; files = value
                } catch { if !Task.isCancelled { self.error = error.localizedDescription } }
                loading = false
                return
            }
            while !Task.isCancelled {
                guard client.isCurrentConnection else { dismiss(); return }
                do {
                    let (value, _) = try await client.request("/api/magician/v2/analytics/llm_calls/query", method: "POST",
                        body: .object(["sql": .string(ChatSessionAccounting.sql(sessionID))]))
                    guard client.isCurrentConnection, !Task.isCancelled else { return }
                    let (modelUsage, _) = try await client.request("/api/magician/v2/analytics/llm_calls/query", method: "POST",
                        body: .object(["sql": .string(ChatSessionAccounting.sql(sessionID, byModel: true))]))
                    guard client.isCurrentConnection, !Task.isCancelled else { return }
                    usage = value; models = modelUsage; error = nil
                } catch { if !Task.isCancelled { self.error = error.localizedDescription } }
                loading = false
                try await Task.sleep(for: .seconds(15))
            }
        } catch { if !Task.isCancelled { self.error = error.localizedDescription; loading = false } }
    }
}

enum ChatThreadSettingsPatch {
    static func make(threadID: String, originalName: String, originalMemory: String, name: String, memory: String) -> WorkJSON {
        let title = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let effectiveName = title.isEmpty ? threadID : title
        var patch: [String: WorkJSON] = [:]
        if effectiveName != originalName { patch["name"] = .string(effectiveName) }
        if memory != originalMemory { patch["memory_text"] = .string(memory) }
        return .object(patch)
    }
}

/// Durable thread context, separate from device/application Settings.
struct ChatThreadSettings: View {
    let threadID: String
    let client: WorkAPIClient?
    var onSaved: (UiThreadRecord) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var memory = ""
    @State private var originalName = ""
    @State private var originalMemory = ""
    @State private var loaded = false
    @State private var saving = false
    @State private var saved = false
    @State private var error: String?
    @State private var revision = 0
    private var path: String { "/api/magician/v2/ui-threads/\(WorkAPIClient.segment(threadID))" }
    private var patch: WorkJSON {
        ChatThreadSettingsPatch.make(threadID: threadID, originalName: originalName, originalMemory: originalMemory, name: name, memory: memory)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("Saved name and context for #\(threadID).")
                        .font(.caption).foregroundStyle(.secondary)
                    if !loaded && error == nil { ProgressView() }
                    if let error {
                        Text(error).foregroundStyle(.red)
                        if !loaded { Button("Retry") { revision += 1 } }
                    }
                }
                Section("Name") { TextField("Thread name", text: $name).disabled(!loaded || saving) }
                Section("Thread memory") {
                    TextEditor(text: $memory).frame(minHeight: 160).disabled(!loaded || saving)
                        .accessibilityLabel("Thread memory")
                }
                if saved { Text("Saved").foregroundStyle(.secondary) }
            }
            .navigationTitle("Thread settings").navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    Button(saving ? "Saving…" : "Save") { Task { await save() } }
                        .disabled(!loaded || saving || client?.isCurrentConnection != true || patch.object?.isEmpty != false)
                }
            }
            .onChange(of: name) { _, _ in saved = false }
            .onChange(of: memory) { _, _ in saved = false }
            .task(id: revision) { await load() }
            .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
                dismiss()
            }
        }
    }

    private func load() async {
        loaded = false; error = nil
        do {
            guard let client else { throw ExecutionModeError.unavailable("Connect to your Magician in Settings.") }
            guard client.isCurrentConnection else { dismiss(); return }
            let (value, _) = try await client.request(path)
            guard client.isCurrentConnection, !Task.isCancelled else { return }
            guard value["thread"].object != nil else { throw URLError(.badServerResponse) }
            name = value["thread"]["name"].text ?? threadID
            memory = value["thread"]["memory_text"].text ?? ""
            originalName = name; originalMemory = memory
            loaded = true
        } catch { if !Task.isCancelled { self.error = error.localizedDescription } }
    }

    private func save() async {
        guard !saving, let client, client.isCurrentConnection else { return }
        saving = true; error = nil
        defer { saving = false }
        let title = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let submittedName = title.isEmpty ? threadID : title
        let submittedMemory = memory
        let submittedPatch = patch
        guard submittedPatch.object?.isEmpty == false else { return }
        do {
            let (value, _) = try await client.request(path, method: "PUT", body: submittedPatch)
            guard client.isCurrentConnection, !Task.isCancelled else { return }
            if let data = try? JSONEncoder().encode(value["thread"]),
               let thread = try? JSONDecoder().decode(UiThreadRecord.self, from: data) {
                onSaved(thread)
            }
            originalName = submittedName; originalMemory = submittedMemory
            saved = true
        } catch { if client.isCurrentConnection { self.error = error.localizedDescription } }
    }
}

private struct ChatAssetSelection: Identifiable {
    let id = UUID()
    let path: String
    let name: String
    let client: WorkAPIClient
}

private struct ChatAssetPreview: View {
    let asset: ChatAssetSelection
    @Environment(\.dismiss) private var dismiss
    @State private var file: URL?
    @State private var preview: URL?
    @State private var error: String?
    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                if let error { Text(error) }
                else if let file {
                    Button("Preview") { preview = file }
                    ShareLink("Save or share", item: file)
                } else { ProgressView("Loading file…") }
            }.padding().navigationTitle(asset.name).navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
            .quickLookPreview($preview)
            .task {
                do {
                    guard asset.client.isCurrentConnection else { throw URLError(.cancelled) }
                    let data = try await asset.client.file(asset.path)
                    guard asset.client.isCurrentConnection, !Task.isCancelled else { return }
                    let folder = FileManager.default.temporaryDirectory.appendingPathComponent("chat-artifacts/" + UUID().uuidString, isDirectory: true)
                    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
                    let name = (asset.name as NSString).lastPathComponent
                    let target = folder.appendingPathComponent(name.isEmpty ? "artifact" : name)
                    try data.write(to: target, options: .atomic)
                    file = target; preview = target
                } catch { if !Task.isCancelled { self.error = error.localizedDescription } }
            }
            .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in file = nil; preview = nil; dismiss() }
        }
    }
}
