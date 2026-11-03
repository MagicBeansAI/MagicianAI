import SwiftUI
import UIKit

/// A link to one document in Magician's own Notes app.
///
/// The canonical form is relative — `/notes?path=<note path>&workspace=<id>` —
/// as the server writes it into chat replies and note records and the web
/// `/notes` page reads it. The same route on a paired machine's absolute
/// origin, and `magican://notes?path=…&workspace=…` (OS links, notifications),
/// mean the same thing. `workspace` is absent on older links; those belong to
/// the workspace the app is signed in to.
struct NoteLink: Equatable {
    /// The note's path inside the notes library (`Tasks/report.md`).
    let path: String
    /// The workspace the note lives in, when the link names one.
    let workspace: String?
    /// Normalized origin key of an absolute `https://…/notes` link. Nil for a
    /// relative or `magican://` link, which always means the current machine.
    let origin: String?

    static func parse(_ url: URL) -> NoteLink? { parse(url.absoluteString) }

    static func parse(_ raw: String) -> NoteLink? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let components = URLComponents(string: trimmed) else { return nil }
        var origin: String?
        switch components.scheme?.lowercased() {
        case nil:
            guard components.host == nil, normalizedRoute(components.path) == "/notes" else { return nil }
        case let scheme? where MagicanAppURL.isScheme(scheme):
            guard components.host?.lowercased() == "notes",
                  components.path.isEmpty || components.path == "/" else { return nil }
        case let scheme? where scheme == "http" || scheme == "https":
            guard components.user == nil, components.password == nil,
                  let host = components.host, !host.isEmpty,
                  normalizedRoute(components.path) == "/notes" else { return nil }
            let port = components.port.map { ":\($0)" } ?? ""
            let renderedHost = host.contains(":") && !host.hasPrefix("[") ? "[\(host)]" : host
            guard let key = MobileConnectionProfiles.originKey("\(scheme)://\(renderedHost)\(port)") else {
                return nil
            }
            origin = key
        default:
            return nil
        }
        let items = components.queryItems ?? []
        func value(_ name: String) -> String? {
            let found = items.first(where: { $0.name == name })?.value?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return found?.isEmpty == false ? found : nil
        }
        guard var path = value("path") else { return nil }
        while path.hasPrefix("/") { path.removeFirst() }
        guard !path.isEmpty, !path.contains("\0") else { return nil }
        return NoteLink(path: path, workspace: value("workspace"), origin: origin)
    }

    private static func normalizedRoute(_ path: String) -> String {
        path.count > 1 && path.hasSuffix("/") ? String(path.dropLast()) : path
    }
}

/// What opening a [NoteLink] should do on this device.
enum NoteLinkResolution: Equatable {
    /// The note is in the workspace the app is signed in to.
    case open(path: String)
    /// Another paired machine holds that workspace: switch to it, then open.
    case switchMachine(origin: String, path: String)
    /// No paired machine on this device is signed in to that workspace.
    case otherWorkspace(String)
    /// An absolute link on a machine this device is not paired with; leave it
    /// to the browser.
    case notOurs

    static func resolve(
        _ link: NoteLink,
        active: MobileConnectionProfile?,
        profiles: [MobileConnectionProfile]
    ) -> NoteLinkResolution {
        func key(_ profile: MobileConnectionProfile) -> String? {
            MobileConnectionProfiles.originKey(profile.publicOrigin)
        }
        let activeKey = active.flatMap(key)
        if let origin = link.origin {
            guard let target = profiles.first(where: { key($0) == origin })
                    ?? (activeKey == origin ? active : nil) else { return .notOurs }
            if let workspace = link.workspace, workspace != target.workspace {
                return .otherWorkspace(workspace)
            }
            return origin == activeKey ? .open(path: link.path) : .switchMachine(origin: origin, path: link.path)
        }
        guard let workspace = link.workspace, let active, workspace != active.workspace else {
            return .open(path: link.path)
        }
        // A relative link names no machine. Switch only when exactly one other
        // paired machine is signed in to that workspace; two would be a guess.
        let candidates = profiles.filter { $0.workspace == workspace && key($0) != activeKey }
        if candidates.count == 1, let origin = key(candidates[0]) {
            return .switchMachine(origin: origin, path: link.path)
        }
        return .otherWorkspace(workspace)
    }
}

/// Opens note links natively from anywhere in the app: Markdown in chat
/// replies, task summaries, Attention details, notes previews, and
/// `magican://notes` deep links. The Notes screen is presented over whatever
/// is showing (a sheet included), so the caller needs no presentation state.
enum NoteLinkRouter {
    /// Handles `raw` when it is a note link this device can resolve; false
    /// leaves it to the caller (browser, Work routing, …).
    @discardableResult
    static func open(_ raw: String?) -> Bool {
        guard let raw, let link = NoteLink.parse(raw) else { return false }
        return open(link)
    }

    @discardableResult
    static func open(_ url: URL) -> Bool { open(url.absoluteString) }

    @discardableResult
    static func open(_ link: NoteLink) -> Bool {
        let resolution = NoteLinkResolution.resolve(
            link,
            active: MagicianAccess.connectionProfile,
            profiles: MagicianAccess.allProfiles
        )
        switch resolution {
        case .notOurs:
            return false
        case .open(let path):
            Task { @MainActor in presentNotes(path: path) }
        case .switchMachine(let origin, let path):
            Task { @MainActor in
                guard MagicianAccess.activate(origin: origin) else {
                    presentOtherWorkspace(link.workspace ?? "")
                    return
                }
                // Switching rebuilds the app's view tree (and closes its
                // sheets); present once that has settled.
                try? await Task.sleep(nanoseconds: 600_000_000)
                presentNotes(path: path)
            }
        case .otherWorkspace(let workspace):
            Task { @MainActor in presentOtherWorkspace(workspace) }
        }
        return true
    }

    /// `.environment(\.openURL, NoteLinkRouter.openURLAction)` on a subtree:
    /// note links open natively, everything else keeps the system behavior.
    static var openURLAction: OpenURLAction {
        OpenURLAction { url in open(url) ? .handled : .systemAction }
    }

    /// The Notes library at its root (a `/notes` link naming no note).
    static func openLibrary() {
        Task { @MainActor in presentNotes(path: nil) }
    }

    @MainActor
    private static func presentNotes(path: String?) {
        guard let presenter = topViewController() else { return }
        let theme = ThemeManager.shared
        weak var host: UIViewController?
        let root = NotesBrowserView(initialPath: path, onClose: { host?.dismiss(animated: true) })
            .tint(theme.accentColor)
        let controller = UIHostingController(rootView: root)
        controller.modalPresentationStyle = .fullScreen
        if let forced = theme.forcedColorScheme {
            controller.overrideUserInterfaceStyle = forced == .dark ? .dark : .light
        }
        host = controller
        presenter.present(controller, animated: true)
    }

    @MainActor
    private static func presentOtherWorkspace(_ workspace: String) {
        guard let presenter = topViewController() else { return }
        let named = workspace.isEmpty ? "another workspace" : "the “\(workspace)” workspace"
        let alert = UIAlertController(
            title: "This note is in another workspace",
            message: "It belongs to \(named), which this device is not signed in to. Pair this device with that workspace to open it.",
            preferredStyle: .alert
        )
        alert.addAction(UIAlertAction(title: "OK", style: .cancel))
        presenter.present(alert, animated: true)
    }

    @MainActor
    private static func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let window = scenes
            .sorted { ($0.activationState == .foregroundActive ? 0 : 1) < ($1.activationState == .foregroundActive ? 0 : 1) }
            .flatMap(\.windows)
            .first(where: \.isKeyWindow) ?? scenes.flatMap(\.windows).first
        var top = window?.rootViewController
        while let next = top?.presentedViewController, !next.isBeingDismissed { top = next }
        return top
    }
}

/// The one opener for every link tapped in chat — Markdown replies, structured
/// blocks and actions, and activity details. `classifyActivityLink` (the Swift
/// twin of web `classifyRef`) decides what a link is; this decides what the app
/// does with it:
///
/// - note links → the native Notes screen (`NoteLinkRouter`);
/// - task outputs and `/api/…` resources → the authenticated in-app viewer
///   (`ArtifactViewer`), since Safari cannot carry the device bearer;
/// - other host files (absolute paths, `file://`, workspace-relative) →
///   open-file on the paired machine through the chat session;
/// - chat links (`/chat?session=…`, `/t/<thread>[/chat][?session=…]`) → that
///   conversation, else the thread's latest; tasks, Work, Attention, Today,
///   Settings, Observe, Notes and VibeDev routes → in-app navigation; any
///   other app route → the web page in the authenticated viewer;
/// - http(s), mailto, tel, `magican://` → the system.
///
/// A tap it cannot open is reported through `fail`, never dropped.
struct ChatLinkRouter {
    static let failureMessage = "Can't open link"

    let sessionId: String?
    /// Presents the authenticated in-app viewer.
    let present: (ArtifactRef) -> Void
    /// Shows a short failure message.
    let fail: (String) -> Void
    /// Opens a task in place (a pushed detail); nil uses the app-wide route.
    var openTask: ((String) -> Void)? = nil
    /// Opens a run in place; false falls back to the run's debug page.
    var openExecution: ((String) -> Bool)? = nil

    func open(_ url: URL) { open(url.absoluteString) }

    func open(_ raw: String) {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return fail(Self.failureMessage) }
        if NoteLinkRouter.open(trimmed) { return }
        switch classifyActivityLink(trimmed, origin: MagicianAccess.baseURL) {
        case .external(let value):
            guard let url = URL(string: value) else { return fail(Self.failureMessage) }
            let fail = self.fail
            Task { @MainActor in
                UIApplication.shared.open(url, options: [:]) { opened in
                    if !opened { fail(Self.failureMessage) }
                }
            }
        case .task(let id):
            guard let id, !id.isEmpty else {
                AppActions.shared.requestWork(WorkNavigationTarget(kind: .all))
                return
            }
            if let openTask { openTask(id) } else { AppActions.shared.requestTask(id) }
        case .execution(let executionId):
            if openExecution?(executionId) == true { return }
            let encoded = executionId.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? executionId
            present(.direct(url: "/debug?execution_id=\(encoded)", mime: "text/html", name: "Run"))
        case .thread(let id):
            AppActions.shared.requestThread(id)
        case .attention(let id):
            AppActions.shared.requestAttention(itemID: id)
        case .today(let rawSection):
            AppActions.shared.requestToday(section: rawSection.flatMap(TodaySection.init(rawValue:)))
        case .settings:
            AppActions.shared.requestSettings()
        case .observe:
            AppActions.shared.requestObserve()
        case .taskOutput(let taskId, let relativePath):
            present(.taskOutput(taskId: taskId, relativePath: relativePath, mime: nil))
        case .artifact(let path):
            present(.direct(url: path, mime: nil, name: nil))
        case .file(let path):
            openOnHost(path)
        case .chat(let sessionId, let threadId):
            AppActions.shared.requestThread(threadId, session: sessionId)
        case .vibeDev:
            AppActions.shared.requestVibeDev()
        case .notes:
            NoteLinkRouter.openLibrary()
        case .webRoute(let route):
            let present = self.present
            let openRoute: (String) -> Void = { target in
                if AppActions.shared.requestWorkRoute(target) { return }
                present(.direct(url: target, mime: "text/html", name: MagicianAccess.productName))
            }
            // `/t/<key>/tasks…`: the key may be the thread's slug or an old
            // one; Work filters by the canonical id.
            if let key = chatThreadRouteKey(route) {
                ThreadDirectory.shared.resolve(key) { id in openRoute(chatRoute(route, withThreadId: id)) }
            } else {
                openRoute(route)
            }
        case .unknown:
            fail(Self.failureMessage)
        }
    }

    /// Opens a file on the paired machine; the server checks it against the
    /// chat session's scope.
    private func openOnHost(_ path: String) {
        guard let sessionId, !sessionId.isEmpty else {
            return fail("\(Self.failureMessage): this file is not attached to a chat.")
        }
        let fail = self.fail
        Task { @MainActor in
            do {
                try await ActivityPathClient.perform(sessionId: sessionId, absolutePath: path, action: .file)
            } catch {
                fail(Self.failureMessage)
            }
        }
    }
}
