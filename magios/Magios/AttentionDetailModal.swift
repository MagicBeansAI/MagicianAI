import SwiftUI
import Combine
import UIKit

enum BrowserSignInError: LocalizedError {
    case unavailable, changed, pageChanged, insecure
    /// A short, secret-free message from the server (sign-in or managed-bot connect).
    case server(String)
    var errorDescription: String? {
        switch self {
        case .server(let message): return message
        case .unavailable: return "Sign-in could not continue. Reopen the request, or finish on the computer."
        case .changed: return "The Magican connection changed. Reopen sign-in on the intended computer."
        case .pageChanged: return "The sign-in page changed. Review the picture before trying again."
        case .insecure: return "Use HTTPS to your Magican computer for private sign-in, or choose Open on computer."
        }
    }
}

private final class BrowserSignInRedirectPolicy: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// Narrow viewer credentials stay in memory, separate from the owner's bearer.
@MainActor struct BrowserSignInLaunch: Identifiable {
    private static let transport: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.urlCache = nil
        config.httpCookieStorage = nil
        config.urlCredentialStorage = nil
        return URLSession(configuration: config, delegate: BrowserSignInRedirectPolicy(), delegateQueue: nil)
    }()
    let id = UUID()
    let baseURL: URL
    let headers: [String: String]
    let ticket: String
    let authorizationURL: URL?
    let current: @MainActor () -> Bool
    var connected = false

    static func start(baseURL: URL, headers: [String: String], correlationId: String? = nil,
                      skill: String? = nil, endpointAlias: String? = nil, onComputer: Bool = false,
                      current: @escaping @MainActor () -> Bool) async throws -> Self {
        guard current() else { throw BrowserSignInError.changed }
        var request = URLRequest(url: baseURL.appendingPathComponent("api/magician/v2/auth/browser-signin/launch"))
        request.httpMethod = "POST"
        request.timeoutInterval = 45
        headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        var payload: [String: Any] = skill.map { ["skill": $0] } ?? ["correlation_id": correlationId ?? ""]
        if skill != nil, let endpointAlias { payload["endpoint_alias"] = endpointAlias }
        if onComputer { payload["open_on_computer"] = true }
        request.httpBody = try JSONSerialization.data(withJSONObject: payload)
        let (data, response) = try await transport.data(for: request)
        guard current() else { throw BrowserSignInError.changed }
        guard data.count <= 4 * 1024 * 1024,
              let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw BrowserSignInError.unavailable }
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            if let message = json["message"] as? String, !message.isEmpty {
                throw BrowserSignInError.server(String(message.prefix(1024)))
            }
            throw BrowserSignInError.unavailable
        }
        if ["connected", "computer_opened"].contains(json["mode"] as? String ?? "") {
            var result = Self(baseURL: baseURL, headers: headers, ticket: "", authorizationURL: nil, current: current)
            result.connected = true
            return result
        }
        if json["mode"] as? String == "direct", let raw = json["authorization_url"] as? String,
           let url = URL(string: raw), url.scheme == "https", url.user == nil, url.password == nil {
            return Self(baseURL: baseURL, headers: headers, ticket: "", authorizationURL: url, current: current)
        }
        guard let ticket = json["ticket"] as? String,
              ticket.range(of: "^bsi_[a-f0-9]{64}$", options: .regularExpression) != nil else { throw BrowserSignInError.unavailable }
        let launch = Self(baseURL: baseURL, headers: headers, ticket: ticket, authorizationURL: nil, current: current)
        guard allowsPrivateViewer(baseURL) else {
            _ = try? await launch.request("close", token: ticket)
            throw BrowserSignInError.insecure
        }
        return launch
    }

    private static func allowsPrivateViewer(_ baseURL: URL) -> Bool {
        baseURL.scheme == "https" || (baseURL.scheme == "http" && ["localhost", "127.0.0.1", "[::1]", "::1"].contains(baseURL.host ?? ""))
    }

    /// The computer-mode viewer for a ticket handed out elsewhere (a managed
    /// bot's Google `login_url` step), on this connection.
    func viewerLaunch(ticket: String) async throws -> Self {
        guard current() else { throw BrowserSignInError.changed }
        guard ticket.range(of: "^bsi_[a-f0-9]{64}$", options: .regularExpression) != nil else { throw BrowserSignInError.unavailable }
        let launch = Self(baseURL: baseURL, headers: headers, ticket: ticket, authorizationURL: nil, current: current)
        guard Self.allowsPrivateViewer(baseURL) else {
            _ = try? await launch.request("close", token: ticket)
            throw BrowserSignInError.insecure
        }
        return launch
    }

    /// `POST bot/{start|input|state|cancel}` with the owner's headers.
    func botRequest(_ action: String, body: [String: Any]) async throws -> [String: Any] {
        guard current() else { throw BrowserSignInError.changed }
        var request = URLRequest(url: baseURL.appendingPathComponent("api/magician/v2/auth/browser-signin/bot/\(action)"))
        request.httpMethod = "POST"
        request.timeoutInterval = 45
        request.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response) = try await Self.transport.data(for: request)
        guard current() else { throw BrowserSignInError.changed }
        guard data.count <= 4 * 1024 * 1024 else { throw BrowserSignInError.unavailable }
        let result = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            if let message = result["message"] as? String, !message.isEmpty { throw BrowserSignInError.server(message) }
            throw BrowserSignInError.unavailable
        }
        return result
    }

    static func currentConnection() -> Self {
        let profile = MagicianAccess.connectionProfile
        let base = profile?.publicOrigin ?? MagicianAccess.baseURL
        let headers = MagicianAccess.headers(profile: profile)
        return Self(baseURL: base, headers: headers, ticket: "", authorizationURL: nil,
                    current: { MagicianAccess.connectionProfile == profile
                        && (MagicianAccess.connectionProfile?.publicOrigin ?? MagicianAccess.baseURL) == base
                        && MagicianAccess.headers(profile: profile) == headers })
    }

    func request(_ action: String, token: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        guard action == "close" || current() else { throw BrowserSignInError.changed }
        var request = URLRequest(url: baseURL.appendingPathComponent("api/magician/v2/auth/browser-signin/\(action)"))
        request.httpMethod = "POST"
        request.timeoutInterval = action == "close" ? 5 : 45
        request.cachePolicy = .reloadIgnoringLocalAndRemoteCacheData
        headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue(token, forHTTPHeaderField: "X-Magician-Signin")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response) = try await Self.transport.data(for: request)
        guard action == "close" || current() else { throw BrowserSignInError.changed }
        guard data.count <= 4 * 1024 * 1024,
              let result = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw BrowserSignInError.unavailable }
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            if result["error"] as? String == "signin_page_changed" { throw BrowserSignInError.pageChanged }
            throw BrowserSignInError.unavailable
        }
        return result
    }
}

private struct BrowserSignInActionStyle: ButtonStyle {
    @ObservedObject private var themeManager = ThemeManager.shared
    @Environment(\.isEnabled) private var isEnabled
    var prominent = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.semibold))
            .multilineTextAlignment(.center)
            .padding(.horizontal, 12).padding(.vertical, 10)
            .frame(maxWidth: .infinity, minHeight: 44)
            .foregroundStyle(prominent ? themeManager.onAccentColor : themeManager.textColor)
            .background(RoundedRectangle(cornerRadius: 10)
                .fill(prominent ? themeManager.accentFillColor : themeManager.surfaceColor))
            .overlay(RoundedRectangle(cornerRadius: 10)
                .stroke(prominent ? Color.clear : themeManager.controlBorderColor, lineWidth: 1))
            .contentShape(RoundedRectangle(cornerRadius: 10))
            .opacity(isEnabled ? (configuration.isPressed ? 0.75 : 1) : 0.45)
    }
}

/// How a managed bot (Google Workspace, Telegram, WhatsApp) can be connected.
enum ManagedBotMethod: String, Equatable {
    case loginURL = "login_url"
    case code
    case pairingCode = "pairing_code"
    case qr

    /// Code and pairing-code logins start from the account's phone number.
    var needsPhone: Bool { self == .code || self == .pairingCode }

    func label(bot: String) -> String {
        switch self {
        case .loginURL: return "Sign in with Google"
        case .code: return "Code"
        case .pairingCode: return "Link with phone number"
        case .qr: return bot.hasPrefix("telegram") ? "QR" : "Scan QR"
        }
    }

    /// Known methods in the row's order; rows without them fall back by bot name.
    static func parse(_ raw: Any?, bot: String?) -> [ManagedBotMethod] {
        var listed: [ManagedBotMethod] = []
        for method in ((raw as? [Any]) ?? []).compactMap({ ($0 as? String).flatMap(ManagedBotMethod.init(rawValue:)) })
        where !listed.contains(method) { listed.append(method) }
        if !listed.isEmpty { return listed }
        let bot = bot ?? ""
        if bot.hasPrefix("gmail-") { return [.loginURL] }
        if bot.hasPrefix("telegram") { return [.code, .qr] }
        if bot.hasPrefix("whatsapp") { return [.pairingCode, .qr] }
        return []
    }
}

/// One Service-accounts row. Engines before the status field omit `status`,
/// `detail` and `endpoint_alias`; those rows read as unknown. Engines before
/// managed bots omit `kind`, `bot`, `profile` and `methods`.
struct BrowserServiceAccount: Identifiable, Equatable {
    enum Status: String {
        case connected
        case needsSignIn = "needs_sign_in"
        case authenticating
        case unknown
        case unavailable

        var title: String {
            switch self {
            case .connected: return "Connected"
            case .needsSignIn: return "Needs sign-in"
            case .authenticating: return "Signing in…"
            case .unknown: return "Unknown"
            case .unavailable: return "Unavailable"
            }
        }
    }

    enum Kind: String {
        case governedOAuth = "governed_oauth"
        case browserCLI = "browser_cli"
        case managedBot = "managed_bot"
    }

    let name: String
    let label: String
    let status: Status
    let detail: String?
    let endpointAlias: String?
    var kind: Kind? = nil
    var bot: String? = nil
    var profile: String? = nil
    var methods: [ManagedBotMethod] = []
    var id: String { (bot ?? endpointAlias).map { "\(name):\($0)" } ?? name }
    /// The bot to connect through `bot/start`, for a managed-bot row.
    var managedBot: String? { kind == .managedBot ? bot : nil }

    static func parse(_ rows: [[String: Any]]) -> [BrowserServiceAccount] {
        rows.compactMap { row in
            guard let name = row["name"] as? String else { return nil }
            func text(_ key: String) -> String? { (row[key] as? String).flatMap { $0.isEmpty ? nil : $0 } }
            let kind = text("kind").flatMap(Kind.init(rawValue:))
            let bot = text("bot")
            return BrowserServiceAccount(name: name, label: row["label"] as? String ?? name,
                                         status: Status(rawValue: row["status"] as? String ?? "") ?? .unknown,
                                         detail: text("detail"), endpointAlias: text("endpoint_alias"),
                                         kind: kind, bot: bot, profile: text("profile"),
                                         methods: kind == .managedBot ? ManagedBotMethod.parse(row["methods"], bot: bot) : [])
        }
    }
}

/// `BotConnectState` from `bot/start|input|state|cancel`.
struct ManagedBotConnectState: Equatable {
    enum PendingKind: String {
        case phone, code, password
        case pairingCode = "pairing_code"
        case loginURL = "login_url"
        case qr
    }

    struct Pending: Equatable {
        let kind: PendingKind
        let label: String?
        let pairingCode: String?
        /// Decoded `data:image/png;base64,…` bytes (the engine sends PNG QR codes only).
        let qrPNG: Data?
        let signInMode: String?
        let signInTicket: String?
    }

    let bot: String
    let status: BrowserServiceAccount.Status
    let detail: String?
    let pending: Pending?

    static func parse(bot: String, _ json: [String: Any]) -> ManagedBotConnectState {
        func text(_ object: [String: Any], _ key: String) -> String? { (object[key] as? String).flatMap { $0.isEmpty ? nil : $0 } }
        var pending: Pending?
        if let raw = json["pending"] as? [String: Any], let kind = (raw["kind"] as? String).flatMap(PendingKind.init(rawValue:)) {
            let signIn = raw["sign_in"] as? [String: Any] ?? [:]
            let prefix = "data:image/png;base64,"
            let qr = text(raw, "qr_data_url").flatMap { $0.hasPrefix(prefix) ? Data(base64Encoded: String($0.dropFirst(prefix.count))) : nil }
            pending = Pending(kind: kind, label: text(raw, "label"), pairingCode: text(raw, "pairing_code"), qrPNG: qr,
                              signInMode: text(signIn, "mode"), signInTicket: text(signIn, "ticket"))
        }
        return ManagedBotConnectState(bot: text(json, "bot") ?? bot,
                                      status: BrowserServiceAccount.Status(rawValue: json["status"] as? String ?? "") ?? .unknown,
                                      detail: text(json, "detail"), pending: pending)
    }
}

private struct ManagedBotConnectTarget: Identifiable {
    let account: BrowserServiceAccount
    let connection: BrowserSignInLaunch
    var id: String { account.id }
}

/// Connect sheet for a managed bot: method choice, phone, code, two-step
/// password, WhatsApp pairing code, QR, or Google's computer-mode viewer.
@MainActor struct ManagedBotConnectSheet: View {
    let account: BrowserServiceAccount
    let connection: BrowserSignInLaunch
    @Environment(\.dismiss) private var dismiss
    @State private var method: ManagedBotMethod
    @State private var started = false
    @State private var phone = ""
    @State private var field = ""
    @State private var fieldKind: ManagedBotConnectState.PendingKind?
    @State private var state: ManagedBotConnectState?
    @State private var busy = false
    @State private var message = ""
    @State private var copied = false
    @State private var openedTicket = ""
    @State private var viewer: BrowserSignInLaunch?
    @State private var latest = 0
    @State private var pollTask: Task<Void, Never>?
    @State private var closed = false

    init(account: BrowserServiceAccount, connection: BrowserSignInLaunch) {
        self.account = account
        self.connection = connection
        _method = State(initialValue: account.methods.first ?? .qr)
    }

    private var bot: String { account.managedBot ?? account.name }
    private var pending: ManagedBotConnectState.Pending? { state?.pending }
    private var authenticating: Bool { state?.status == .authenticating }

    var body: some View {
        NavigationStack {
            Form {
                if !started { chooseSection } else { flowSection }
                if !message.isEmpty {
                    Section { Text(message).font(.callout).foregroundStyle(.red) }
                }
            }
            .navigationTitle(account.label).navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(authenticating ? "Cancel" : "Close") { cancel() }.disabled(busy && authenticating)
                }
            }
        }
        .sheet(item: $viewer) { launch in BrowserSignInPanel(launch: launch) }
        .task { if !started && account.methods.count <= 1 && !method.needsPhone { await start() } }
        .onDisappear { closed = true; pollTask?.cancel(); field = "" }
    }

    @ViewBuilder private var chooseSection: some View {
        if account.methods.count > 1 {
            Section("How to connect") {
                Picker("Method", selection: $method) {
                    ForEach(account.methods, id: \.rawValue) { Text($0.label(bot: bot)).tag($0) }
                }.pickerStyle(.inline).labelsHidden()
            }
        }
        if method.needsPhone {
            Section {
                TextField("+91 98765 43210", text: $phone)
                    .keyboardType(.phonePad).textContentType(.telephoneNumber)
            } header: { Text("Phone number") } footer: { Text("Include the country code.") }
        }
        Section {
            Button(busy ? "Starting…" : "Continue") { Task { await start() } }
                .disabled(busy || (method.needsPhone && phone.trimmingCharacters(in: .whitespaces).isEmpty))
        }
    }

    @ViewBuilder private var flowSection: some View {
        if let state {
            if state.status == .connected {
                Section { Label("Connected.", systemImage: "checkmark.circle.fill").foregroundStyle(.green) }
            } else {
                if let detail = state.detail { Section { Text(detail).font(.callout) } }
                if authenticating, let pending { pendingSection(pending) }
                else if authenticating { Section { HStack { ProgressView(); Text("Waiting…").foregroundStyle(.secondary) } } }
                if !authenticating {
                    Section { Button("Try again") { retry() } }
                }
            }
        } else {
            Section { ProgressView() }
        }
    }

    @ViewBuilder private func pendingSection(_ pending: ManagedBotConnectState.Pending) -> some View {
        switch pending.kind {
        case .phone:
            Section {
                TextField("+91 98765 43210", text: $field).keyboardType(.phonePad).textContentType(.telephoneNumber)
                Button("Send") { submit("phone") }.disabled(busy || trimmed.isEmpty)
            } header: { Text(pending.label ?? "Phone number") } footer: { Text("Include the country code.") }
        case .code:
            Section(pending.label ?? "Code") {
                TextField("Code", text: $field).keyboardType(.numberPad).textContentType(.oneTimeCode)
                Button("Send code") { submit("code") }.disabled(busy || trimmed.isEmpty)
            }
        case .password:
            Section {
                SecureField("Two-step password", text: $field).textContentType(.password)
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                Button("Send") { submit("password") }.disabled(busy || trimmed.isEmpty)
            } header: { Text(pending.label ?? "Two-step password") } footer: { Text("Sent once to the sign-in. Never saved.") }
        case .pairingCode:
            if let code = pending.pairingCode {
                Section {
                    Text(code).font(.system(size: 34, weight: .bold, design: .monospaced)).kerning(4)
                        .textSelection(.enabled).frame(maxWidth: .infinity).accessibilityLabel("Pairing code \(code)")
                    Button(copied ? "Copied" : "Copy") {
                        UIPasteboard.general.setItems([["public.utf8-plain-text": code]],
                                                      options: [.localOnly: true, .expirationDate: Date().addingTimeInterval(300)])
                        copied = true
                    }
                } footer: {
                    Text("In WhatsApp: Settings → Linked devices → Link a device → Link with phone number instead, then enter this code.")
                }
            }
        case .qr:
            Section {
                if let data = pending.qrPNG, let image = UIImage(data: data) {
                    Image(uiImage: image).interpolation(.none).resizable().scaledToFit()
                        .frame(maxWidth: 260).frame(maxWidth: .infinity)
                        .accessibilityLabel("QR code to scan")
                } else {
                    Text("The QR code could not be shown. Try again, or use the code option.")
                }
            } footer: {
                if account.methods.count > 1 { Text("Scan from another device, or use the code option.") }
            }
        case .loginURL:
            Section {
                if pending.signInMode == "computer_opened" {
                    Text("Finish the sign-in on your computer — this updates automatically.")
                } else {
                    if let ticket = pending.signInTicket, ticket != openedTicket {
                        Button("Open sign-in") { openViewer(ticket) }.disabled(busy)
                    } else {
                        Text("Finish signing in in the sign-in window.").foregroundStyle(.secondary)
                    }
                    Button("Open on computer") { Task { await start(onComputer: true) } }.disabled(busy)
                }
            }
        }
    }

    private var trimmed: String { field.trimmingCharacters(in: .whitespacesAndNewlines) }

    private func call(_ action: String, _ extra: [String: Any] = [:]) async throws -> ManagedBotConnectState? {
        latest += 1
        let mine = latest
        var body: [String: Any] = ["bot": bot]
        extra.forEach { body[$0.key] = $0.value }
        let json = try await connection.botRequest(action, body: body)
        // A poll that started before a newer request must not overwrite its answer.
        guard !closed, mine == latest else { return nil }
        return ManagedBotConnectState.parse(bot: bot, json)
    }

    private func apply(_ next: ManagedBotConnectState?) {
        guard let next, !closed else { return }
        state = next
        if next.pending?.kind != fieldKind { fieldKind = next.pending?.kind; field = ""; copied = false }
        pollTask?.cancel()
        if next.status == .authenticating { schedulePoll() }
        else if next.status == .connected {
            pollTask = Task { @MainActor in
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                if !Task.isCancelled { dismiss() }
            }
        }
    }

    private func schedulePoll() {
        pollTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            guard !Task.isCancelled, !closed else { return }
            if busy { schedulePoll(); return }
            do { let next = try await call("state"); apply(next) }
            catch {
                guard !closed else { return }
                message = error.localizedDescription
                if case .changed? = error as? BrowserSignInError { return }
                schedulePoll()
            }
        }
    }

    private func run(_ action: String, _ extra: [String: Any] = [:]) async -> Bool {
        busy = true; message = ""
        defer { busy = false }
        do { let next = try await call(action, extra); apply(next); return true }
        catch { if !closed { message = error.localizedDescription }; return false }
    }

    /// `onComputer` opens Google's sign-in in the computer's own browser instead of the viewer.
    private func start(onComputer: Bool = false) async {
        guard !busy else { return }
        var extra: [String: Any] = ["method": method.rawValue]
        if onComputer { extra["open_on_computer"] = true }
        if method.needsPhone {
            let number = phone.trimmingCharacters(in: .whitespaces)
            guard !number.isEmpty else { return }
            extra["phone"] = number
        }
        openedTicket = ""
        if await run("start", extra) { started = true }
    }

    private func submit(_ step: String) {
        let value = trimmed
        guard !busy, !value.isEmpty else { return }
        // Never keep a two-step password (or a code) in the form once it is sent.
        field = ""
        Task { _ = await run("input", ["step": step, "value": value]) }
    }

    private func cancel() {
        pollTask?.cancel()
        guard started, authenticating else { dismiss(); return }
        Task {
            _ = await run("cancel")
            dismiss()
        }
    }

    private func retry() {
        pollTask?.cancel()
        state = nil; message = ""; started = false
    }

    private func openViewer(_ ticket: String) {
        busy = true; message = ""
        Task { @MainActor in
            defer { busy = false }
            do { viewer = try await connection.viewerLaunch(ticket: ticket); openedTicket = ticket }
            catch { message = error.localizedDescription }
        }
    }
}

private struct BrowserServiceAccountStatusChip: View {
    let status: BrowserServiceAccount.Status
    private var tint: Color {
        switch status {
        case .connected: return .green
        case .needsSignIn: return .orange
        case .authenticating: return .blue
        case .unknown, .unavailable: return .secondary
        }
    }
    var body: some View {
        Text(status.title)
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 8).padding(.vertical, 2)
            .foregroundStyle(tint)
            .background(Capsule().fill(tint.opacity(0.15)))
            .accessibilityLabel("Status: \(status.title)")
    }
}

@MainActor struct BrowserServiceAccountsView: View {
    @Environment(\.openURL) private var openURL
    @State private var accounts: [BrowserServiceAccount] = []
    @State private var connection: BrowserSignInLaunch?
    @State private var browserSignIn: BrowserSignInLaunch?
    @State private var managedBot: ManagedBotConnectTarget?
    @State private var loading = true
    @State private var busy = false
    @State private var message = ""
    var body: some View {
        List {
            Text("Connect an installed service on this Magican computer. Credentials stay on that computer.")
                .font(.caption).foregroundStyle(.secondary)
            if loading { ProgressView() }
            if !message.isEmpty { Text(message).font(.callout) }
            if !loading && accounts.isEmpty { Text("No supported service accounts found. Install a service skill from Skills first.") }
            ForEach(accounts) { account in
                Section {
                    VStack(alignment: .leading, spacing: 12) {
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 8) {
                                Text(account.name).font(.headline)
                                BrowserServiceAccountStatusChip(status: account.status)
                            }
                            Text(account.label).font(.caption).foregroundStyle(.secondary)
                            if let detail = account.detail {
                                Text(detail).font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        if account.status == .unavailable {
                            // Missing binary or environment: nothing to connect until it is installed.
                            EmptyView()
                        } else if account.managedBot != nil {
                            Button {
                                guard let connection else { return }
                                message = ""
                                managedBot = ManagedBotConnectTarget(account: account, connection: connection)
                            } label: { Text(account.status == .connected ? "Reconnect" : "Connect") }
                            .buttonStyle(BrowserSignInActionStyle(prominent: true))
                            .disabled(busy || connection == nil || account.methods.isEmpty)
                        } else {
                            Button {
                                guard let connection else { return }
                                busy = true; message = ""
                                Task { @MainActor in
                                    defer { busy = false }
                                    do {
                                        let launch = try await BrowserSignInLaunch.start(baseURL: connection.baseURL, headers: connection.headers, skill: account.name, endpointAlias: account.endpointAlias, current: connection.current)
                                        if launch.connected { message = "Connected."; await reload() }
                                        else if let url = launch.authorizationURL { openURL(url) }
                                        else { browserSignIn = launch }
                                    } catch { message = error.localizedDescription }
                                }
                            } label: { Text(account.status == .connected ? "Reconnect" : "Connect") }
                            .buttonStyle(BrowserSignInActionStyle(prominent: true))
                            .disabled(busy || connection == nil)
                            Button("Open on computer") {
                                guard let connection else { return }
                                busy = true; message = ""
                                Task { @MainActor in
                                    defer { busy = false }
                                    do {
                                        _ = try await BrowserSignInLaunch.start(baseURL: connection.baseURL, headers: connection.headers, skill: account.name, endpointAlias: account.endpointAlias, onComputer: true, current: connection.current)
                                        message = "Opened on the computer. Waiting for the service to confirm sign-in."
                                    } catch { message = error.localizedDescription }
                                }
                            }
                            .buttonStyle(BrowserSignInActionStyle())
                            .disabled(busy || connection == nil)
                        }
                    }.padding(.vertical, 8)
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Service accounts")
        .sheet(item: $browserSignIn, onDismiss: { Task { await reload() } }) { launch in BrowserSignInPanel(launch: launch) }
        .sheet(item: $managedBot, onDismiss: { Task { await reload() } }) { target in
            ManagedBotConnectSheet(account: target.account, connection: target.connection)
        }
        .refreshable { await reload() }
        // Sign-in often finishes in Safari or on the computer: re-check on return.
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.willEnterForegroundNotification)) { _ in
            Task { await reload() }
        }
        .task { await reload() }
    }

    private func reload() async {
        let selected = BrowserSignInLaunch.currentConnection()
        connection = selected
        do {
            let result = try await selected.request("accounts", token: "")
            accounts = BrowserServiceAccount.parse(result["accounts"] as? [[String: Any]] ?? [])
        } catch { if accounts.isEmpty { message = error.localizedDescription } }
        loading = false
    }
}

@MainActor struct BrowserSignInPanel: View {
    let launch: BrowserSignInLaunch
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var credential = ""
    @State private var image: UIImage?
    @State private var origin = "Starting private sign-in…"
    @State private var message = ""
    @State private var revision = ""
    @State private var sequence: UInt64 = 0
    @State private var polling = false
    @State private var sending = false
    @State private var closed = false
    @State private var typed = ""

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 12) {
                    Text("This private browser runs on your Magican computer. Input is kept out of agent history.")
                        .font(.caption).foregroundStyle(.secondary)
                    Text(origin).font(.caption.monospaced()).textSelection(.enabled)
                    if !message.isEmpty { Text(message).font(.callout).accessibilityAddTraits(.updatesFrequently) }
                    if let image {
                        GeometryReader { geometry in
                            Image(uiImage: image).resizable().scaledToFit()
                                .onTapGesture { point in
                                    send(["kind":"tap", "x":point.x / geometry.size.width, "y":point.y / geometry.size.height])
                                }
                        }.aspectRatio(390.0 / 720.0, contentMode: .fit)
                    } else if !closed { ProgressView() }
                    HStack(spacing: 8) {
                        Button("Scroll up") { send(["kind":"scroll", "delta":-400]) }
                        Button("Scroll down") { send(["kind":"scroll", "delta":400]) }
                    }.buttonStyle(BrowserSignInActionStyle())
                        .disabled(closed || sending || revision.isEmpty)
                    HStack(spacing: 8) {
                        Button("Next field") { send(["kind":"key", "key":"Tab"]) }
                        Button("Delete") { send(["kind":"key", "key":"Backspace"]) }
                        Button("Enter") { send(["kind":"key", "key":"Enter"]) }
                    }.buttonStyle(BrowserSignInActionStyle())
                        .disabled(closed || sending || revision.isEmpty)
                    SecureField("Text for the selected field", text: $typed)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .textFieldStyle(.roundedBorder).disabled(closed || sending || revision.isEmpty)
                    Button("Enter text") {
                        let value = typed; typed = ""
                        send(["kind":"text", "value":value])
                    }.buttonStyle(BrowserSignInActionStyle(prominent: true))
                        .disabled(typed.isEmpty || sending || closed || revision.isEmpty)
                    Text("Tap a field in the picture before entering text. Some providers require finishing on the computer.")
                        .font(.caption).foregroundStyle(.secondary)
                }.padding()
            }
            .navigationTitle("Finish sign-in").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { close(); dismiss() } } }
        }
        .task {
            do {
                guard !closed else { return }
                credential = launch.ticket
                let result = try await launch.request("claim", token: launch.ticket)
                if closed {
                    if let token = result["session_token"] as? String { _ = try? await launch.request("close", token: token) }
                    return
                }
                if result["status"] as? String == "connected" { message = "Connected. You can return to Magican."; close(); return }
                guard let token = result["session_token"] as? String else { throw BrowserSignInError.unavailable }
                credential = token
                while !Task.isCancelled && !closed {
                    if scenePhase == .active && !sending { await refresh() }
                    try await Task.sleep(nanoseconds: 700_000_000)
                }
            } catch is CancellationError { }
            catch { message = error.localizedDescription; close() }
        }
        .onChange(of: scenePhase) { _, phase in if phase != .active { typed = ""; image = nil } }
        .onDisappear { close() }
    }

    private func refresh() async {
        guard !closed && !polling else { return }
        polling = true
        defer { polling = false }
        do {
            let frame = try await launch.request("frame", token: credential)
            guard !closed && scenePhase == .active else { return }
            if frame["status"] as? String != "waiting" {
                message = frame["status"] as? String == "connected" ? "Connected. You can return to Magican." : "This sign-in has ended. Reopen the request to retry."
                close(); return
            }
            if frame["retry_after_ms"] != nil { return }
            if let encoded = frame["image"] as? String, let bytes = Data(base64Encoded: encoded) { image = UIImage(data: bytes) }
            else { image = nil }
            origin = frame["origin"] as? String ?? ""
            revision = frame["frame_revision"] as? String ?? ""
        } catch { message = error.localizedDescription; close() }
    }

    private func send(_ input: [String: Any]) {
        guard !closed && !sending && !revision.isEmpty else { return }
        sending = true
        let expected = revision
        Task { @MainActor in
            defer { sending = false }
            // Wait for a frame already in flight. Never replay a text/key
            // operation after an uncertain response.
            while polling && !closed { try? await Task.sleep(nanoseconds: 30_000_000) }
            guard !closed else { return }
            sequence += 1
            do { _ = try await launch.request("input", token: credential, body: ["sequence":sequence, "frame_revision":expected, "input":input]) }
            catch BrowserSignInError.pageChanged { revision = ""; image = nil; message = "The page changed. Review the picture before trying again." }
            catch { message = "\(error.localizedDescription) Input was not retried. Reopen sign-in to continue."; close(); return }
            await refresh()
        }
    }

    private func close() {
        guard !closed else { return }
        closed = true; typed = ""; image = nil
        let token = credential
        credential = ""
        if !token.isEmpty { Task { _ = try? await launch.request("close", token: token) } }
    }
}

/// The single-item HITL popup. Renders the input control for the item's
/// input_type and submits the matching AgenticResumeValue, or cancels (aborted).
///
/// **All eleven input types have a shape of their own here.** Five used to
/// borrow another's and lose the schema that said what was being asked, exactly
/// as they did on the web (see `docs/components/unified-ui/unified-task-panel.md`,
/// *Answering an ask*): `tool_authorization` and `sandbox_override` rendered as
/// an ordinary option list, `external_action` dropped `instructions`,
/// `file_path` dropped `multiple` and `filter`, and `confirmation` dropped
/// `destructive`. Every one of those fields was already on the wire.
struct AttentionDetailModal: View {
    let item: AttentionItem
    @ObservedObject var viewModel: AttentionViewModel
    @Environment(\.dismiss) private var dismiss
    @StateObject private var themeManager = ThemeManager.shared

    @State private var textInput = ""
    @State private var otherInput = ""
    @State private var selectedIds: Set<String> = []
    /// A choice option that requires an accompanying note (web `requires_input`) —
    /// selecting it reveals a field instead of submitting immediately.
    @State private var selectedChoiceId: String?
    /// Per-file selection for a multi-file diff_approval (defaults to all).
    @State private var selectedFilePaths: Set<String> = []
    @State private var formValues: [String: String] = [:]
    @State private var formSkipped: Set<String> = []
    /// Choice / multi-choice form questions: the option ids picked per question.
    @State private var formSelections: [String: Set<String>] = [:]
    @Environment(\.openURL) private var openURL
    /// Whether an answer (or an explicit cancel) has been posted. A secret ask
    /// dismissed any other way is cancelled on the way out — the pending
    /// operation must retire rather than wait out its window — and the typed
    /// value is cleared with the view.
    @State private var answered = false
    @State private var browserSignIn: BrowserSignInLaunch?
    @State private var browserSignInBusy = false
    @State private var browserSignInError: String?
    @State private var browserSignInNotice: String?
    /// The task whose plan Review plan opened (the Plan Inspector, pushed).
    @State private var reviewPlanRoute: PlanInspectorRoute?
    /// Ticks once a second while a secret ask with a deadline is shown, so the
    /// window counts down and the field closes when it ends.
    @State private var now = Date()
    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let from = item.delegatedFromLabel {
                        // C2: asked by a delegated child; the answer resumes the child.
                        Label(from, systemImage: "arrow.turn.down.right")
                            .font(.themed(12, weight: .medium))
                            .foregroundColor(themeManager.secondaryTextColor)
                    }
                    if let chain = item.chainLabel {
                        Text(chain.uppercased())
                            .font(.themed(11, weight: .bold))
                            .foregroundColor(themeManager.accentColor)
                    }
                    if item.isStalledWait {
                        stalledWait
                    } else {
                    if let preview = item.approvalPreview {
                        // The gated tool's own card stands in for the prompt
                        // (the server's readable summary of the same card);
                        // the raw call stays one tap away inside it.
                        ApprovalPreviewCard(preview: preview, rawCall: item.approvalRawCall)
                    } else {
                    Text(item.prompt)
                        .font(.themed(17, weight: .semibold))
                        .foregroundColor(themeManager.textColor)
                    if let hint = item.hint, !hint.isEmpty, hint != item.prompt {
                        Text(hint)
                            .font(.themed(15))
                            .foregroundColor(themeManager.secondaryTextColor)
                    }
                    }

                    input
                    }

                    // "Review draft" (a Relentless accepted draft → Work
                    // Quality) or the source the row names: a Work
                    // destination opens in-app, a web page in the browser.
                    if let review = item.reviewLink {
                        Button {
                            if review.workTarget != nil { dismiss() }
                            review.open()
                        } label: {
                            HStack(spacing: 6) {
                                Image(systemName: "arrow.up.right.square")
                                Text(review.label)
                            }
                            .font(.themed(14, weight: .medium))
                            .foregroundColor(themeManager.accentColor)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("attention-review-link")
                    }

                    // A stalled wait has no ask to abort; its own block cancels the run.
                    if !item.isStalledWait {
                        Button(action: { answered = true; viewModel.cancel(item); dismiss() }) {
                            Text("Cancel")
                                .frame(maxWidth: .infinity)
                                .padding()
                                .foregroundColor(themeManager.dangerColor)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding()
            }
            .safeAreaInset(edge: .bottom) {
                if item.isGodMode, !GodGrantScope.grouped(item.grantOptions).isEmpty {
                    GodModeGrantControls(options: item.grantOptions) { id in
                        answered = true
                        viewModel.submitChoice(item, optionId: id)
                        dismiss()
                    }
                    .padding().background(themeManager.backgroundColor)
                }
            }
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationTitle(item.isStalledWait ? "Stalled run"
                             : item.isMcpOAuth ? "Sign-in needed"
                             : item.itemType.isEmpty ? "Attention" : item.itemType.capitalized)
            .navigationBarTitleDisplayMode(.inline)
            .navigationBarItems(trailing: Button("Close") { dismiss() })
            .planInspectorDestination($reviewPlanRoute)
        }
        .onReceive(clock) { tick in
            if item.sensitiveDeadline != nil { now = tick }
        }

        .onDisappear {
            // Closing a secret ask without answering is an answer: cancel it so
            // the run can raise a fresh challenge instead of waiting out the
            // window. Nothing typed survives the view.
            if item.isSensitive && !answered {
                answered = true
                viewModel.cancel(item, reason: "dismissed")
            }
            textInput = ""
            formValues = [:]
        }
    }

    /// Review plan · Reject · Approve · Approve & Run (design §2). Approving
    /// does not start a run, so the two affirmatives are spelled out: Approve
    /// keeps it for later, Approve & Run starts it now.
    private var planApprovalButtons: some View {
        let taskId = item.owningTaskId
        let choices = PlanApprovalActions.choices(hasTask: taskId != nil)
        return VStack(spacing: 8) {
            if choices.contains(.review), let taskId {
                Button { reviewPlanRoute = PlanInspectorRoute(taskId: taskId, title: item.title) } label: {
                    Label(PlanApprovalChoice.review.label(), systemImage: "list.bullet.clipboard")
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .foregroundColor(themeManager.accentColor)
                        .overlay(RoundedRectangle(cornerRadius: 10)
                            .stroke(themeManager.accentColor.opacity(0.5), lineWidth: 1))
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("plan-approval-review")
            }
            HStack(spacing: 8) {
                planChoiceButton(.reject, fill: themeManager.dangerColor.opacity(0.12),
                                 text: themeManager.dangerColor)
                planChoiceButton(.approve, fill: themeManager.controlColor, text: themeManager.textColor)
            }
            planChoiceButton(.approveAndRun, fill: themeManager.accentFillColor, text: themeManager.onAccentColor)
        }
    }

    private func planChoiceButton(_ choice: PlanApprovalChoice, fill: Color, text: Color) -> some View {
        Button {
            answered = true
            viewModel.submitPlanApproval(item, choice: choice)
            dismiss()
        } label: {
            Text(choice.label(confirmLabel: item.schema?.confirmLabel, denyLabel: item.schema?.denyLabel))
                .font(.themed(15, weight: .semibold))
                .frame(maxWidth: .infinity, minHeight: 44)
                .background(fill)
                .foregroundColor(text)
                .cornerRadius(10)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("plan-approval-\(choice.rawValue)")
    }

    /// Discard · Send, then Always allow <agent> to send to <recipient> (design
    /// `2026-10-25-outward-send-review-design.md`, Part 2). Send is this exact
    /// message to this exact recipient, once; the remembered grant also sends,
    /// so it is outlined rather than filled. The labels are the server's.
    @ViewBuilder
    private var outwardReviewButtons: some View {
        if let choices = item.outwardReviewOptions {
            VStack(alignment: .leading, spacing: 8) {
                if let line = item.outwardReview?.recipientLine {
                    Text(line)
                        .font(.themed(13))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .accessibilityIdentifier("outward-review-recipients")
                }
                HStack(spacing: 8) {
                    outwardChoiceButton(choices.discard, fill: themeManager.dangerColor.opacity(0.12),
                                        text: themeManager.dangerColor)
                    outwardChoiceButton(choices.send, fill: themeManager.accentFillColor,
                                        text: themeManager.onAccentColor, bold: true)
                }
                if let always = choices.alwaysAllow {
                    Button {
                        answered = true
                        viewModel.submitOutwardReview(item, optionId: always.id)
                        dismiss()
                    } label: {
                        Text(always.label)
                            .font(.themed(15, weight: .medium))
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity, minHeight: 44)
                            .padding(.horizontal, 8)
                            .foregroundColor(themeManager.accentColor)
                            .overlay(RoundedRectangle(cornerRadius: 10)
                                .stroke(themeManager.accentColor.opacity(0.5), lineWidth: 1))
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("outward-review-\(always.id)")
                }
                if let expiry = item.outwardReview?.expiryLine(now: now) {
                    Text(expiry)
                        .font(.themed(13))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .accessibilityIdentifier("outward-review-expiry")
                }
            }
        }
    }

    private func outwardChoiceButton(_ option: AttentionGrantOption, fill: Color, text: Color,
                                     bold: Bool = false) -> some View {
        Button {
            answered = true
            viewModel.submitOutwardReview(item, optionId: option.id)
            dismiss()
        } label: {
            Text(option.label)
                .font(.themed(15, weight: bold ? .bold : .semibold))
                .frame(maxWidth: .infinity, minHeight: 44)
                .background(fill)
                .foregroundColor(text)
                .cornerRadius(10)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("outward-review-\(option.id)")
    }

    private var secretExpired: Bool {
        guard let deadline = item.sensitiveDeadline else { return false }
        return now >= deadline
    }

    private var secretSecondsLeft: Int? {
        guard let deadline = item.sensitiveDeadline else { return nil }
        return max(0, Int(deadline.timeIntervalSince(now).rounded(.up)))
    }

    /// What will happen to the value, the window when there is one, and past
    /// the window a fresh ask instead of a field the destination would refuse.
    @ViewBuilder
    private var sensitiveBanner: some View {
        if item.sensitiveSpec != nil {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(secretExpired
                        ? "This code's window has closed — ask for a fresh one."
                        : item.isOneTime
                            ? "Used once, then discarded. Never shown to the assistant."
                            : "Held privately for this run. Never shown to the assistant.")
                        .font(.themed(13))
                        .foregroundColor(themeManager.secondaryTextColor)
                    Spacer()
                    if let seconds = secretSecondsLeft, !secretExpired {
                        Text(seconds >= 120 ? "\(seconds / 60) min left" : "\(seconds)s left")
                            .font(.themedMono(.caption))
                            .foregroundColor(themeManager.secondaryTextColor)
                            .accessibilityLabel("Time left")
                    }
                }
                if secretExpired {
                    Button("Request a fresh code") {
                        answered = true
                        textInput = ""
                        viewModel.cancel(item, reason: "fresh_code_requested")
                        dismiss()
                    }
                    .font(.themed(14, weight: .medium))
                    .buttonStyle(.plain)
                    .foregroundColor(themeManager.accentColor)
                }
            }
            .padding(10)
            .background(themeManager.secondaryTextColor.opacity(0.08))
            .cornerRadius(10)
        }
    }

    @ViewBuilder
    private var input: some View {
        sensitiveBanner
        switch item.renderKind {
        // **A grant, not a question**, and the reason it left the `choice` arm.
        // Both of these used to render as an ordinary option list: `Deny` looked
        // identical to `Allow for This Run`, the tool or command being authorized
        // appeared only inside the sentence the backend composed around it, and a
        // sandbox escape read exactly like "which quarter?".
        case "tool_authorization", "sandbox_override":
            VStack(alignment: .leading, spacing: 12) {
                ForEach(item.isGodMode ? item.diffFiles : []) { file in
                    DisclosureGroup(file.path) {
                        ScrollView([.horizontal, .vertical]) {
                            Text(file.unifiedDiff ?? "").font(.system(size: 12, design: .monospaced)).textSelection(.enabled)
                        }.frame(maxHeight: 240)
                    }
                }
                grantBlock
                if !item.isGodMode || GodGrantScope.grouped(item.grantOptions).isEmpty {
                // The refusal first and in the filled style, then every grant the
                // ask actually offers. A tool authorization carries three ids —
                // `allow_once`, `allow_always`, `deny` — and the middle one writes
                // the tool into the session allowlist, so collapsing them into one
                // Allow button would answer a broader question than was asked.
                Button(action: {
                    answered = true
                    viewModel.submitChoice(item, optionId: item.grantDenyOption?.id ?? "deny")
                    dismiss()
                }) {
                    Text(item.grantDenyOption?.label ?? "Deny")
                        .font(.themed(15, weight: .bold))
                        .frame(maxWidth: .infinity).padding()
                        .background(themeManager.accentFillColor)
                        .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
                }
                .buttonStyle(.plain)
                ForEach(item.grantAllowOptions) { opt in
                    Button(action: { answered = true; viewModel.submitChoice(item, optionId: opt.id); dismiss() }) {
                        Text(opt.label)
                            .font(.themed(15, weight: .medium))
                            .frame(maxWidth: .infinity).padding()
                            .foregroundColor(themeManager.dangerColor)
                            .overlay(RoundedRectangle(cornerRadius: 10)
                                .stroke(themeManager.dangerColor.opacity(0.5), lineWidth: 1))
                    }
                    .buttonStyle(.plain)
                }
                }
            }

        case "choice":
            VStack(spacing: 8) {
                ForEach(item.options, id: \.id) { opt in
                    Button(action: {
                        if opt.requiresInput == true {
                            selectedChoiceId = opt.id       // reveal the note field; submit below
                        } else {
                            answered = true
                            viewModel.submitChoice(item, optionId: opt.id, otherValue: otherInput)
                            dismiss()
                        }
                    }) { optionLabel(opt.label, selected: selectedChoiceId == opt.id) }
                    .buttonStyle(.plain)
                }
                if item.schema?.allowOther == true {
                    TextField("Other…", text: $otherInput).textFieldStyle(.roundedBorder)
                }
                // A selected option that requires input: show the field + submit.
                if let sel = selectedChoiceId,
                   item.options.first(where: { $0.id == sel })?.requiresInput == true {
                    TextField("Add details…", text: $otherInput, axis: .vertical)
                        .lineLimit(1...4).textFieldStyle(.roundedBorder)
                    submitButton("Submit") {
                        answered = true
                        viewModel.submitChoice(item, optionId: sel, otherValue: otherInput); dismiss()
                    }
                    .disabled(otherInput.isEmpty)
                }
            }

        case "diff_approval":
            if item.diffFiles.count > 1 {
                // Per-file selection (web parity): choose which files to apply.
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(item.diffFiles) { file in
                        Button(action: {
                            if selectedFilePaths.contains(file.path) { selectedFilePaths.remove(file.path) }
                            else { selectedFilePaths.insert(file.path) }
                        }) {
                            HStack(spacing: 8) {
                                Image(systemName: selectedFilePaths.contains(file.path) ? "checkmark.square.fill" : "square")
                                    .foregroundColor(themeManager.accentColor)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(file.path).font(.themed(13, weight: .medium))
                                        .foregroundColor(themeManager.textColor).lineLimit(1).truncationMode(.middle)
                                    HStack(spacing: 8) {
                                        if let a = file.additions, a > 0 { Text("+\(a)").foregroundColor(themeManager.successColor) }
                                        if let d = file.deletions, d > 0 { Text("−\(d)").foregroundColor(themeManager.dangerColor) }
                                        Text(fileStatusLabel(file.status)).foregroundColor(themeManager.secondaryTextColor)
                                    }.font(.themed(11))
                                }
                                Spacer()
                            }
                            .padding(10).background(themeManager.surfaceColor).cornerRadius(8)
                        }
                        .buttonStyle(.plain)
                    }
                    HStack(spacing: 8) {
                        Button(action: { answered = true; viewModel.submitDiffApproval(item, apply: false); dismiss() }) {
                            Text("Reject").frame(maxWidth: .infinity).padding()
                                .background(themeManager.dangerColor.opacity(0.12))
                                .foregroundColor(themeManager.dangerColor).cornerRadius(10)
                        }
                        .buttonStyle(.plain)
                        Button(action: {
                            let all = selectedFilePaths.count == item.diffFiles.count
                            answered = true
                            viewModel.submitDiffApproval(item, apply: true, paths: all ? [] : Array(selectedFilePaths))
                            dismiss()
                        }) {
                            Text(selectedFilePaths.count == item.diffFiles.count ? "Apply all" : "Apply \(selectedFilePaths.count)")
                                .frame(maxWidth: .infinity).padding()
                                .background(themeManager.accentFillColor)
                                .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
                        }
                        .buttonStyle(.plain)
                        .disabled(selectedFilePaths.isEmpty)
                    }
                }
                .onAppear { if selectedFilePaths.isEmpty { selectedFilePaths = Set(item.diffFiles.map(\.path)) } }
            } else {
                // Whole-proposal apply/reject.
                HStack(spacing: 8) {
                    Button(action: { answered = true; viewModel.submitDiffApproval(item, apply: false); dismiss() }) {
                        Text("Reject").frame(maxWidth: .infinity).padding()
                            .background(themeManager.dangerColor.opacity(0.12))
                            .foregroundColor(themeManager.dangerColor).cornerRadius(10)
                    }
                    .buttonStyle(.plain)
                    Button(action: { answered = true; viewModel.submitDiffApproval(item, apply: true); dismiss() }) {
                        Text("Apply").frame(maxWidth: .infinity).padding()
                            .background(themeManager.accentFillColor)
                            .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
                    }
                    .buttonStyle(.plain)
                }
            }

        case "file_path":
            VStack(alignment: .leading, spacing: 8) {
                // `multiple` and `filter` were both on the wire and neither reached
                // the reader, so a prompt wanting three CSVs looked exactly like one
                // wanting a config file. The separator is stated because the answer
                // is split on it.
                Text(item.pathFieldLabel)
                    .font(.themed(12, weight: .medium))
                    .foregroundColor(themeManager.secondaryTextColor)
                TextField(item.placeholder.isEmpty ? "/path/to/file" : item.placeholder, text: $textInput)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled(true)
                    .textInputAutocapitalization(.never)
                submitButton("Send") { answered = true; viewModel.submitFilePaths(item, paths: pathsFromInput); dismiss() }
                    .disabled(pathsFromInput.isEmpty)
            }

        case "external_action":
            VStack(alignment: .leading, spacing: 8) {
                // `instructions` is the whole content of this input type and was
                // read by no surface: the reader got a bare note field under a
                // prompt that assumed they already knew what to go and do.
                if let instructions = item.externalInstructions {
                    Text(instructions)
                        .font(.themed(15))
                        .foregroundColor(themeManager.textColor)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(10)
                        .background(themeManager.surfaceColor)
                        .cornerRadius(8)
                }
                // C4: the page to go to (an `mcp_oauth` consent screen). Opens in
                // the browser; the answer is the acknowledgement below.
                if item.isMcpOAuth {
                    Button(browserSignInBusy ? "Opening sign-in…" : "Finish sign-in here") {
                        browserSignInBusy = true
                        browserSignInError = nil
                        Task { @MainActor in
                            defer { browserSignInBusy = false }
                            do {
                                let launch = try await viewModel.launchBrowserSignIn(for: item)
                                if launch.connected { browserSignInNotice = "Connected." }
                                else if let url = launch.authorizationURL { openURL(url) }
                                else { browserSignIn = launch }
                            } catch { browserSignInError = error.localizedDescription }
                        }
                    }.buttonStyle(BrowserSignInActionStyle(prominent: true))
                        .disabled(browserSignInBusy)
                    Button("Open on computer") {
                        browserSignInBusy = true
                        Task { @MainActor in
                            defer { browserSignInBusy = false }
                            do { try await viewModel.openBrowserSignInOnComputer(for: item); browserSignInNotice = "Opened on the computer. Waiting for the service to confirm sign-in." }
                            catch { browserSignInError = error.localizedDescription }
                        }
                    }.buttonStyle(BrowserSignInActionStyle())
                        .disabled(browserSignInBusy)
                    // Declining is an answer, not a close: it posts an abort, the
                    // computer cancels the sign-in, and the ask leaves every surface.
                    if let dismissLabel = item.externalDismissLabel {
                        Button(dismissLabel) {
                            answered = true
                            viewModel.cancel(item, reason: "owner_declined")
                            dismiss()
                        }
                        .font(.themed(15))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 6)
                        .disabled(browserSignInBusy)
                        .accessibilityIdentifier("attention-signin-not-now")
                    }
                    if let error = browserSignInError { Text(error).foregroundStyle(.red).font(.caption) }
                    if let notice = browserSignInNotice { Text(notice).foregroundStyle(.secondary).font(.caption) }
                    Color.clear.frame(height: 0).sheet(item: $browserSignIn) { launch in
                        BrowserSignInPanel(launch: launch)
                    }
                } else if let url = item.externalActionURL {
                    Button(action: { openURL(url) }) {
                        HStack(spacing: 6) {
                            Image(systemName: "arrow.up.right.square")
                            Text(item.isMcpOAuth ? "Open sign-in page" : "Open page")
                        }
                        .font(.themed(15, weight: .semibold))
                        .frame(maxWidth: .infinity).padding()
                        .foregroundColor(themeManager.accentColor)
                        .overlay(RoundedRectangle(cornerRadius: 10)
                            .stroke(themeManager.accentColor.opacity(0.5), lineWidth: 1))
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("attention-external-open")
                    Text(url.host ?? url.absoluteString)
                        .font(.themedMono(.caption2))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .lineLimit(1).truncationMode(.middle)
                }
                if !item.isMcpOAuth {
                TextField("Optional note…", text: $textInput, axis: .vertical)
                    .lineLimit(1...4).textFieldStyle(.roundedBorder)
                // The acknowledgement is the answer and the note is optional, so
                // this is never disabled.
                submitButton(item.externalDoneLabel) {
                    answered = true
                    viewModel.submitExternalDone(item, guidance: textInput.isEmpty ? nil : textInput); dismiss()
                }
                }
            }

        case "multi_choice":
            VStack(spacing: 8) {
                ForEach(item.options, id: \.id) { opt in
                    Button(action: {
                        if selectedIds.contains(opt.id) { selectedIds.remove(opt.id) } else { selectedIds.insert(opt.id) }
                    }) {
                        HStack {
                            Image(systemName: selectedIds.contains(opt.id) ? "checkmark.square.fill" : "square")
                                .foregroundColor(themeManager.accentColor)
                            Text(opt.label).foregroundColor(themeManager.textColor)
                            Spacer()
                        }
                        .padding(10)
                        .background(themeManager.surfaceColor)
                        .cornerRadius(8)
                    }
                    .buttonStyle(.plain)
                }
                submitButton("Submit") {
                    answered = true
                    viewModel.submitMultiChoice(item, ids: Array(selectedIds)); dismiss()
                }
                .disabled(selectedIds.isEmpty)
            }

        case "confirmation" where PlanApprovalActions.isPlanApproval(source: item.source, inputType: item.inputType):
            planApprovalButtons

        // A preference alignment hold: its typed choices, the private reason,
        // a separate lasting-change section and a reply (P6.2).
        case "confirmation" where item.alignment != nil:
            if let projection = item.alignment {
                AlignmentAskView(item: item, projection: projection, viewModel: viewModel) {
                    answered = true
                    dismiss()
                }
            }

        // A captured outward send held for review: Send, Discard, and the
        // remembered grant when offered — a choice id, never a bare confirm.
        case "confirmation" where item.isOutwardReview:
            outwardReviewButtons

        case "confirmation":
            HStack(spacing: 8) {
                Button(action: { answered = true; viewModel.submitConfirmation(item, confirmed: false); dismiss() }) {
                    Text(item.denyLabel).frame(maxWidth: .infinity).padding()
                        .background(themeManager.dangerColor.opacity(0.12))
                        .foregroundColor(themeManager.dangerColor).cornerRadius(10)
                }
                .buttonStyle(.plain)
                // The backend's `destructive` flag bands the affirmative and
                // changes nothing about what is posted — a confirmed destructive
                // action and a confirmed benign one are the same response value.
                // Filled accent invites the tap; outlined does not.
                Button(action: { answered = true; viewModel.submitConfirmation(item, confirmed: true); dismiss() }) {
                    Text(item.confirmLabel).frame(maxWidth: .infinity).padding()
                        .background(item.isDestructive ? Color.clear : themeManager.accentFillColor)
                        .foregroundColor(item.isDestructive ? themeManager.dangerColor : themeManager.onAccentColor)
                        .cornerRadius(10)
                        .overlay(RoundedRectangle(cornerRadius: 10)
                            .stroke(item.isDestructive ? themeManager.dangerColor.opacity(0.6) : Color.clear, lineWidth: 1))
                }
                .buttonStyle(.plain)
            }

        case "password":
            // A masked field. When the widget type is `text`/`guidance` (the
            // backend classified the wording), the value posted keeps the
            // widget's type — the pause expects it — and is never trimmed.
            VStack(spacing: 8) {
                SecureField(item.placeholder, text: $textInput)
                    .textFieldStyle(.roundedBorder)
                    // No `.password` content type: it would invite the save-password prompt (a Keychain copy).
                    .disabled(secretExpired)
                submitButton("Send") {
                    answered = true
                    switch item.inputType {
                    case "text": viewModel.submitText(item, textInput)
                    case "guidance": viewModel.submitGuidance(item, textInput)
                    default: viewModel.submitPassword(item, textInput)
                    }
                    dismiss()
                }
                .disabled(textInput.isEmpty || secretExpired)
            }

        case "otp":
            // A one-time code: masked, numeric-friendly, offered by the
            // keyboard when the phone just received it, posted as the exact
            // string. The wire value rides the password shape; the type is
            // what says "code".
            VStack(spacing: 8) {
                SecureField(item.placeholder == "Type your response…" ? "Enter the code…" : item.placeholder, text: $textInput)
                    .textFieldStyle(.roundedBorder)
                    .textContentType(.oneTimeCode)
                    .disabled(secretExpired)
                submitButton("Send") {
                    answered = true
                    switch item.inputType {
                    case "text": viewModel.submitText(item, textInput)
                    case "guidance": viewModel.submitGuidance(item, textInput)
                    default: viewModel.submitPassword(item, textInput)
                    }
                    dismiss()
                }
                .disabled(textInput.isEmpty || secretExpired)
            }

        case "guidance":
            VStack(spacing: 8) {
                TextField(item.placeholder, text: $textInput, axis: .vertical)
                    .lineLimit(2...6).textFieldStyle(.roundedBorder)
                submitButton("Send") { answered = true; viewModel.submitGuidance(item, textInput); dismiss() }
                    .disabled(textInput.isEmpty)
            }

        case "form":
            VStack(alignment: .leading, spacing: 12) {
                ForEach(item.formQuestions) { question in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text(question.prompt.isEmpty ? "Question" : question.prompt)
                                .font(.themed(15, weight: .medium))
                                .foregroundColor(themeManager.textColor)
                            Spacer()
                            Button(formSkipped.contains(question.id) ? "Unskip" : "Skip") {
                                if formSkipped.contains(question.id) { formSkipped.remove(question.id) }
                                else { formSkipped.insert(question.id) }
                            }
                            .font(.themed(12, weight: .medium))
                            .buttonStyle(.plain)
                            .foregroundColor(themeManager.secondaryTextColor)
                        }
                        if !formSkipped.contains(question.id), isChoiceQuestion(question) {
                            formChoiceField(question)
                        } else if !formSkipped.contains(question.id) {
                            let fieldKind = item.sensitiveFieldKind(question.id)
                            let binding = Binding(
                                get: { formValues[question.id] ?? "" },
                                set: { formValues[question.id] = $0 }
                            )
                            if hitlFieldIsMasked(sensitiveKind: fieldKind) {
                                SecureField(fieldKind == "otp" ? "Enter the code…" : "Kept private", text: binding)
                                    .textFieldStyle(.roundedBorder)
                                    .textContentType(fieldKind == "otp" ? .oneTimeCode : nil)
                                    .disabled(secretExpired)
                            } else {
                                TextField("Your answer", text: binding)
                                    .textFieldStyle(.roundedBorder)
                                    .textContentType(fieldKind == "login_identifier" ? .username : nil)
                                    .autocorrectionDisabled(fieldKind == "login_identifier")
                                    .textInputAutocapitalization(fieldKind == "login_identifier" ? .never : nil)
                                if fieldKind == "login_identifier" {
                                    Text("Kept private: used only to sign in, never shown to the assistant.")
                                        .font(.themed(12))
                                        .foregroundColor(themeManager.secondaryTextColor)
                                }
                            }
                        }
                    }
                }
                HStack {
                    Button("Skip all") {
                        formSkipped = Set(item.formQuestions.map(\.id))
                        submitFormAnswers()
                    }
                    .buttonStyle(.plain)
                    .foregroundColor(themeManager.secondaryTextColor)
                    .disabled(item.formQuestions.isEmpty)
                    Spacer()
                    submitButton("Submit") { submitFormAnswers() }
                        .disabled(!formCanSubmit || secretExpired)
                }
            }

        default: // "text" and any unknown type fall back to free text
            VStack(spacing: 8) {
                TextField(item.placeholder, text: $textInput, axis: .vertical)
                    .lineLimit(1...6).textFieldStyle(.roundedBorder)
                submitButton("Send") { answered = true; viewModel.submitText(item, textInput); dismiss() }
                    .disabled(textInput.isEmpty)
            }
        }
    }

    /// C3: the run waits on the user but no question reached any surface.
    /// Nothing to answer — the reader can cancel the run or open its task.
    @ViewBuilder
    private var stalledWait: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Waiting on you, but no question was delivered", systemImage: "exclamationmark.bubble")
                .font(.themed(17, weight: .semibold))
                .foregroundColor(themeManager.textColor)
            if let summary = item.summary, !summary.isEmpty,
               summary != "Waiting on you, but no question was delivered" {
                Text(summary).font(.themed(15)).foregroundColor(themeManager.secondaryTextColor)
            }
            if item.title != "Waiting on you, but no question was delivered" && !item.title.isEmpty {
                Text(item.title).font(.themed(13)).foregroundColor(themeManager.secondaryTextColor)
            }
            if item.stalledWaitActions.contains("cancel") {
                Button(action: { answered = true; viewModel.cancelStalledExecution(item); dismiss() }) {
                    Text("Cancel run")
                        .font(.themed(15, weight: .bold))
                        .frame(maxWidth: .infinity).padding()
                        .background(themeManager.dangerColor.opacity(0.12))
                        .foregroundColor(themeManager.dangerColor).cornerRadius(10)
                }
                .buttonStyle(.plain)
                .disabled(item.responseExecutionId == nil)
                .accessibilityIdentifier("attention-stalled-cancel")
            }
            if item.stalledWaitActions.contains("open_task"), let taskId = item.owningTaskId {
                Button(action: { dismiss(); AppActions.shared.requestTask(taskId) }) {
                    Text("Open task")
                        .font(.themed(15, weight: .semibold))
                        .frame(maxWidth: .infinity).padding()
                        .background(themeManager.accentFillColor)
                        .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("attention-stalled-open-task")
            }
        }
    }

    /// What is being authorized, shown verbatim.
    ///
    /// Banded and outlined, which no other field here is: a reader working
    /// through a queue must not approve a sandbox escape the way they answer
    /// "which quarter?", and the only thing stopping them is this block not
    /// looking like a question. The subject is monospaced and wraps rather than
    /// truncating — a command you can only see half of is a command you cannot
    /// judge.
    @ViewBuilder
    private var grantBlock: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(item.isOriginalFolderEdit ? "ORIGINAL FOLDER EDIT" : item.grantKind == "sandbox" ? "SANDBOX OVERRIDE REQUESTED" : "TOOL AUTHORIZATION REQUESTED")
                .font(.themed(11, weight: .bold))
                .foregroundColor(themeManager.dangerColor)
            if !item.grantSubject.isEmpty {
                Text(item.grantSubject)
                    .font(.themedMono(.footnote))
                    .foregroundColor(themeManager.textColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let detail = item.grantDetail {
                Text(detail)
                    .font(.themed(13))
                    .foregroundColor(themeManager.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            // Empty for shell overrides, which renders nothing rather than "no roots".
            ForEach(item.grantRoots, id: \.self) { root in
                Text("• \(root)")
                    .font(.themedMono(.caption2))
                    .foregroundColor(themeManager.secondaryTextColor)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(themeManager.dangerColor.opacity(0.08))
        .cornerRadius(10)
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(themeManager.dangerColor.opacity(0.45), lineWidth: 1))
    }

    /// The paths typed into the field.
    ///
    /// Split on commas **only when the ask allows several**, so a single path
    /// containing a comma is still one path. Blank entries are dropped, and an
    /// empty result disables the button — a path is the answer, so an empty one
    /// is not an answer, unlike a free-text response where the empty string is a
    /// thing a reader may mean.
    private var pathsFromInput: [String] {
        let trimmed = textInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard item.wantsMultiplePaths else { return trimmed.isEmpty ? [] : [trimmed] }
        return trimmed
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    private func optionLabel(_ label: String, selected: Bool = false) -> some View {
        Text(label)
            .font(.themed(15, weight: .bold))
            .frame(maxWidth: .infinity)
            .padding(12)
            .background(selected ? themeManager.accentColor.opacity(0.15) : themeManager.surfaceColor)
            .foregroundColor(themeManager.accentColor)
            .cornerRadius(10)
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(themeManager.accentColor.opacity(selected ? 0.8 : 0.3), lineWidth: selected ? 2 : 1))
    }

    private func fileStatusLabel(_ status: String?) -> String {
        switch status?.uppercased() {
        case "A": return "Added"
        case "D": return "Deleted"
        case "R": return "Renamed"
        default: return "Modified"
        }
    }

    private var formCanSubmit: Bool {
        let questions = item.formQuestions
        guard !questions.isEmpty else { return false }
        return questions.allSatisfy { question in
            if formSkipped.contains(question.id) { return true }
            if isChoiceQuestion(question) { return !(formSelections[question.id] ?? []).isEmpty }
            return !(formValues[question.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    private func isChoiceQuestion(_ question: AttentionFormQuestion) -> Bool {
        question.answersByOptions
    }

    @ViewBuilder
    private func formChoiceField(_ question: AttentionFormQuestion) -> some View {
        let multiple = question.inputType == "multi_choice"
        VStack(spacing: 6) {
            ForEach(question.options ?? [], id: \.id) { opt in
                let picked = formSelections[question.id]?.contains(opt.id) == true
                Button(action: {
                    var current = formSelections[question.id] ?? []
                    if multiple {
                        if picked { current.remove(opt.id) } else { current.insert(opt.id) }
                    } else {
                        current = picked ? [] : [opt.id]
                    }
                    formSelections[question.id] = current
                }) {
                    HStack {
                        Image(systemName: multiple
                              ? (picked ? "checkmark.square.fill" : "square")
                              : (picked ? "largecircle.fill.circle" : "circle"))
                            .foregroundColor(themeManager.accentColor)
                        Text(opt.label).foregroundColor(themeManager.textColor)
                        Spacer()
                    }
                    .padding(10)
                    .background(themeManager.surfaceColor)
                    .cornerRadius(8)
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func submitFormAnswers() {
        let answers = item.formAnswers(values: formValues, selections: formSelections, skipped: formSkipped)
        answered = true
        viewModel.submitForm(item, answers: answers)
        dismiss()
    }

    private func submitButton(_ title: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).bold()
                .frame(maxWidth: .infinity).padding()
                .background(themeManager.accentFillColor)
                .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
        }
        .buttonStyle(.plain)
    }
}

/// A confirmation's structured approval card (`input_schema.approval_preview`):
/// who wants to change what, warnings to read first, the agent's stated
/// reason, the change as a line diff, the evidence offered and neutral notes,
/// with the raw call collapsed underneath for auditing.
///
/// Everything is plain text: `why`, `evidence` and the diff lines are
/// agent-authored and go through `Text(verbatim:)`, never Markdown.
struct ApprovalPreviewCard: View {
    let preview: ApprovalPreview
    let rawCall: String
    @ObservedObject private var theme = ThemeManager.shared
    @State private var showRaw = false
    /// The diff and a long raw call scroll inside these heights instead of
    /// stretching the sheet; both grow with Dynamic Type.
    @ScaledMetric(relativeTo: .body) private var diffViewport: CGFloat = 340
    @ScaledMetric(relativeTo: .body) private var rawViewport: CGFloat = 180

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(verbatim: preview.title)
                .font(.themed(17, weight: .semibold))
                .foregroundColor(theme.textColor)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityAddTraits(.isHeader)
            if !preview.warnings.isEmpty { warnings }
            if let why = preview.why {
                VStack(alignment: .leading, spacing: 6) {
                    sectionLabel(preview.whyLabel ?? "Why (agent's words)")
                    Text(verbatim: why)
                        .font(.themed(15))
                        .foregroundColor(theme.textColor)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                        .padding(.leading, 10)
                        .overlay(alignment: .leading) {
                            Rectangle().fill(theme.secondaryTextColor.opacity(0.35)).frame(width: 3)
                        }
                }
                .accessibilityElement(children: .combine)
            }
            if let diff = preview.diff { changes(diff) }
            if !preview.evidence.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    sectionLabel("Evidence")
                    ForEach(Array(preview.evidence.enumerated()), id: \.offset) { _, entry in
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(verbatim: "•").foregroundColor(theme.secondaryTextColor)
                            Text(verbatim: entry)
                                .foregroundColor(theme.textColor)
                                .fixedSize(horizontal: false, vertical: true)
                                .textSelection(.enabled)
                        }
                        .font(.themed(14))
                    }
                }
            }
            ForEach(Array(preview.notes.enumerated()), id: \.offset) { _, note in
                Text(verbatim: note)
                    .font(.themed(13))
                    .foregroundColor(theme.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if !rawCall.isEmpty {
                DisclosureGroup("Raw call", isExpanded: $showRaw) { raw }
                    .font(.themed(13, weight: .medium))
                    .foregroundColor(theme.secondaryTextColor)
                    .tint(theme.secondaryTextColor)
                    .accessibilityIdentifier("approval-preview-raw")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(theme.cardColor, in: RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous).stroke(theme.cardBorderColor, lineWidth: 1))
        .accessibilityIdentifier("approval-preview")
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(verbatim: text.uppercased())
            .font(.themed(11, weight: .bold))
            .foregroundColor(theme.secondaryTextColor)
    }

    /// Read before anything else, so a stale base or missing target cannot
    /// be approved past.
    private var warnings: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(preview.warnings.enumerated()), id: \.offset) { _, warning in
                Label {
                    Text(verbatim: warning.message)
                        .font(.themed(14, weight: .semibold))
                        .foregroundColor(theme.textColor)
                        .fixedSize(horizontal: false, vertical: true)
                } icon: {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundColor(theme.warningColor)
                }
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Warning: \(warning.message)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(theme.warningColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.warningColor.opacity(0.5), lineWidth: 1))
        .accessibilityIdentifier("approval-preview-warnings")
    }

    @ViewBuilder
    private func changes(_ diff: ApprovalPreview.Diff) -> some View {
        let rendered = Self.unifiedDiff(diff)
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                sectionLabel("Changes")
                Text(verbatim: "+\(diff.added)").foregroundColor(theme.successColor)
                Text(verbatim: "−\(diff.removed)").foregroundColor(theme.dangerColor)
            }
            .font(.themedMono(.footnote, weight: .semibold))
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Changes: \(diff.added) lines added, \(diff.removed) lines removed")
            Text(verbatim: diff.path)
                .font(.themedMono(.caption))
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(1).truncationMode(.middle)
            // Long lines scroll sideways rather than wrapping, so indentation
            // and line structure read as they will be written.
            if rendered.lineCount + rendered.hunks.count * 2 > 18 {
                ScrollView(.vertical) { UnifiedDiffView(diff: rendered, wrap: false) }
                    .frame(height: diffViewport)
                    .accessibilityIdentifier("approval-preview-diff")
            } else {
                UnifiedDiffView(diff: rendered, wrap: false)
                    .accessibilityIdentifier("approval-preview-diff")
            }
            if diff.truncated && diff.omittedChangedLines > 0 {
                Text(verbatim: "\(diff.omittedChangedLines) more changed lines not shown")
                    .font(.themed(13))
                    .foregroundColor(theme.secondaryTextColor)
            }
        }
    }

    @ViewBuilder
    private var raw: some View {
        let text = Text(verbatim: rawCall)
            .font(.themedMono(.caption))
            .foregroundColor(theme.textColor)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
        if rawCall.count > 600 {
            ScrollView(.vertical) { text.padding(.vertical, 4) }.frame(height: rawViewport)
        } else {
            text.padding(.top, 4)
        }
    }

    /// The preview's structured hunks in the shared diff renderer's model,
    /// numbered from each hunk's own start.
    static func unifiedDiff(_ diff: ApprovalPreview.Diff) -> UnifiedDiff {
        var rendered = UnifiedDiff(oldPath: diff.path, newPath: diff.path)
        var lineID = 0
        var previousOldEnd = 1
        for (index, hunk) in diff.hunks.enumerated() {
            var old = hunk.oldStart, new = hunk.newStart
            var lines: [UnifiedDiffLine] = []
            for line in hunk.lines {
                switch line.kind {
                case .context:
                    lines.append(UnifiedDiffLine(id: lineID, kind: .context, oldNumber: old, newNumber: new, text: line.text)); old += 1; new += 1
                case .add:
                    lines.append(UnifiedDiffLine(id: lineID, kind: .added, oldNumber: nil, newNumber: new, text: line.text)); new += 1
                case .remove:
                    lines.append(UnifiedDiffLine(id: lineID, kind: .removed, oldNumber: old, newNumber: nil, text: line.text)); old += 1
                }
                lineID += 1
            }
            rendered.hunks.append(UnifiedDiffHunk(
                id: index, header: hunk.header, oldStart: hunk.oldStart, oldCount: hunk.oldLines,
                newStart: hunk.newStart, newCount: hunk.newLines, section: "", lines: lines,
                unchangedBefore: max(0, hunk.oldStart - previousOldEnd)))
            previousOldEnd = hunk.oldStart + hunk.oldLines
        }
        return rendered
    }
}

/// The task whose plan a Review plan button opens: the Plan Inspector's
/// Graph (a draft shows the approval bar); the task detail with the Plan act
/// open where no inspector host is available.
struct PlanReviewTarget: Identifiable, Equatable {
    let taskId: String
    var id: String { taskId }

    /// The detail screen route for this review: the Plan act open.
    var route: TaskDetailRoute { TaskDetailRoute(taskId: taskId, focus: .plan) }

    /// The Plan Inspector route for this review.
    var inspectorRoute: PlanInspectorRoute { PlanInspectorRoute(taskId: taskId, tab: .graph) }
}
