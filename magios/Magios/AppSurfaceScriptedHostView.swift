import SwiftUI
import WebKit

// Scripted custom-surface host for iOS (plan 1.6, `custom_surfaces_v1`).
//
// One WKWebView per surface, on the shipped launcher proxy pattern: a
// `.nonPersistent()` website data store, a per-installation synthetic
// scheme origin (`magapp-surface://<installation_id>/...`) so
// installations are mutually cross-origin, the minted session reference
// riding the address path (the backend asset route requires it, and a
// relative subresource retains path segments while dropping a base query), a
// navigation delegate that denies everything except the initial
// entry-document load (plus exactly one crash-recovery reload per
// renderer crash, each recorded against the kernel's reload/crash
// budget through the reload-note route), all redirects rejected (a
// valid surface asset is canonical and never redirects), a 32 MiB
// resource cap, and authentication applied only inside the proxied
// request. The bridge rides a single WKScriptMessageHandler channel
// with the same closed eight-operation method set as the web host, and
// exhausting its message budget closes the session rather than
// silently dropping. The frame holds no credentials.

enum AppSurfaceScriptedPolicy {
    static let scheme = "magapp-surface"
    static let bridgeChannel = "magicianSurfaceBridge"
    static let unsupportedNotice = "Custom surfaces are not supported on this client."
    static let failedNotice = "The custom surface failed safely and was closed."
    /// The per-session bridge message budget, mirroring the kernel
    /// watchdog (32 messages). The message that would exceed the budget
    /// is answered with an error reply and the session CLOSES with the
    /// failed notice — a silent local drop would run the frame's
    /// sequence counter ahead and wedge every later message with no
    /// closed-state UX at all.
    static let maximumBridgeMessages = 32

    static let bridgeMethods: Set<String> = [
        "query_data",
        "mutate_data",
        "launch_action",
        "get_action_run",
        "compose_action_run",
        "cancel_action_run",
        "read_entity_changes",
        "contract_capabilities"
    ]

    /// The per-installation synthetic origin for one surface frame. The
    /// address carries the minted session reference as a path segment: the
    /// backend asset route requires it (the frame's origin can carry no
    /// headers), and a relative subresource retains it.
    static func schemeURL(installationID: String, digest: String, path: String, session: String) -> URL? {
        guard isCanonicalAssetTail(digest: digest, path: path),
              isPathSafeSessionReference(session)
        else { return nil }
        var components = URLComponents()
        components.scheme = scheme
        components.host = installationID
        components.path = "/\(session)/\(digest)/\(path)"
        return components.url
    }

    /// Canonical asset tails only: a `blake3:<64 lowercase hex>` digest and
    /// a `surfaces/`-relative path with no escapes.
    static func isCanonicalAssetTail(digest: String, path: String) -> Bool {
        let digestParts = digest.split(
            separator: ":",
            maxSplits: 1,
            omittingEmptySubsequences: false
        )
        guard digestParts.count == 2,
              digestParts[0] == "blake3",
              let hex = digestParts.last,
              hex.count == 64,
              hex.allSatisfy({ ("0"..."9").contains($0) || ("a"..."f").contains($0) })
        else { return false }
        guard path.hasPrefix("surfaces/"),
              !path.contains(".."),
              !path.contains("\\"),
              !path.contains("%")
        else { return false }
        return true
    }

    /// Session references the kernel mints (`bridge-scripted:<opaque>:<uuid hex>`)
    /// never need percent-encoding in a path segment; anything that would is refused
    /// rather than encoded, so the credential cannot be silently rewritten.
    static func isPathSafeSessionReference(_ session: String) -> Bool {
        !session.isEmpty
            && session.range(of: "^[A-Za-z0-9_.:-]+$", options: .regularExpression) != nil
    }

    /// Map one scheme URL onto the backend digest-keyed asset route for
    /// this installation. The proxy attaches authentication here, never
    /// in the frame; the frame's own credential is the leading session path
    /// segment, which must be present and canonical, and is forwarded verbatim.
    /// `assetRoute` selects the backend asset family. The full-page scripted
    /// host serves `custom-surface-v1`; a widget-sized mini frame serves
    /// `mini-frame-v1`. Everything else about the mapping — the canonical-tail
    /// validation, the session credential, the refusal to carry a query — is
    /// identical, so the two families share one proxy rather than one of them
    /// growing a looser copy.
    static func destinationURL(
        for schemeURL: URL,
        origin: URL,
        installationID: String,
        assetRoute: String = "custom-surface-v1"
    ) -> URL? {
        guard assetRoute == "custom-surface-v1" || assetRoute == "mini-frame-v1",
              schemeURL.scheme == scheme,
              schemeURL.host?.lowercased() == installationID.lowercased(),
              let source = URLComponents(url: schemeURL, resolvingAgainstBaseURL: false)
        else { return nil }
        let percentEncodedPath = source.percentEncodedPath
        let trimmed = percentEncodedPath.hasPrefix("/") ? String(percentEncodedPath.dropFirst()) : percentEncodedPath
        // The proxied destination gets the same canonical-tail validation
        // the initial entry URL gets (`isCanonicalAssetTail`): a digest
        // and a `surfaces/`-relative member with no escapes. Scheme and
        // host alone are not sufficient.
        let parts = trimmed.split(separator: "/", maxSplits: 2, omittingEmptySubsequences: false)
        guard parts.count == 3,
              isPathSafeSessionReference(String(parts[0])),
              isCanonicalAssetTail(digest: String(parts[1]), path: String(parts[2])),
              source.percentEncodedQuery == nil
        else { return nil }
        var components = URLComponents(url: origin, resolvingAgainstBaseURL: false)
        components?.path =
            "/api/magician/v2/apps/installations/\(installationID)/\(assetRoute)/assets/\(trimmed)"
        components?.percentEncodedQuery = nil
        components?.fragment = nil
        return components?.url
    }

    /// Navigation locks: the initial entry-document load is the only
    /// permitted top-level navigation. Every subsequent navigation,
    /// redirect, popup and `window.open` is denied.
    static func permits(navigationURL: URL, initialURL: URL, isInitialLoad: Bool) -> Bool {
        isInitialLoad && navigationURL == initialURL
    }

    /// All redirects are rejected: a valid surface asset is canonical and
    /// never redirects, and rejecting prevents credentials crossing
    /// origins (the launcher proxy's existing rule).
    static func admitsRedirect() -> Bool { false }

    /// The closed bridge method set; anything else is dropped.
    static func bridgeMethodIsAdmitted(_ method: String) -> Bool {
        bridgeMethods.contains(method)
    }

    /// JSONSerialization may legally emit raw U+2028/U+2029 inside JSON
    /// string literals; both are JavaScript line terminators, so an
    /// unescaped payload would terminate the `evaluateJavaScript`
    /// program early and the reply would be lost. Escaping them to
    /// `\u2028`/`\u2029` inside the string literals is semantically
    /// identical JSON and inert to the JS parser; the delivery channel
    /// (evaluateJavaScript) is unchanged.
    static func javaScriptSafeJSON(_ payload: String) -> String {
        payload
            .replacingOccurrences(of: "\u{2028}", with: "\\u2028")
            .replacingOccurrences(of: "\u{2029}", with: "\\u2029")
    }

    /// The reload-note route for one minted session: the kernel counts
    /// every frame reload and renderer crash against its reload/crash
    /// budget. The HOST calls it (authentication rides the proxied
    /// request, never the frame); the session reference must be one the
    /// kernel could have minted.
    static func reloadNoteURL(origin: URL, installationID: String, session: String) -> URL? {
        guard isPathSafeSessionReference(session) else { return nil }
        return URL(
            string: "/api/magician/v2/apps/installations/\(installationID)/custom-surface-v1/sessions/\(session)/reload-note",
            relativeTo: origin
        )?.absoluteURL
    }

    /// Wiring (1.6 completion): the launcher probes the scripted host only
    /// at an installation root — the one launcher-reachable address where
    /// the web page falls through to the scripted host (a declared MUIJ
    /// view route hydrates successfully and never falls through). The probe
    /// therefore omits `?route=`
    ///
    /// (On web the scripted host is also the fallback at declared view
    /// routes when declarative hydration fails; iOS never probes there —
    /// a transient probe failure at the installation root falls to the
    /// WebView, whose in-page scripted fallback is blocked by
    /// `permitsNetworkPath`.), and the kernel answers with the
    /// package's first declared entry point — exactly the web host's root
    /// flow. `/apps/<installation>` is a root; any deeper canonical route
    /// is not.
    static func probesScriptedHostAtRouteRoot(_ route: String) -> Bool {
        guard AppRoutePolicy.isCanonicalRoute(route) else { return false }
        let segments = route.split(separator: "/", omittingEmptySubsequences: true)
        return segments.count == 2
    }
}

/// Context for one scripted surface: the connection profile used only
/// inside the proxied requests, the installation identity, and the
/// initial entry-document URL.
struct AppSurfaceHostContext: Equatable {
    let profile: MobileConnectionProfile
    let installationID: String
    let initialURL: URL
    /// Which backend asset family this frame's proxy serves. Defaults to the
    /// full-page scripted surface; a widget-sized mini frame names its own.
    var assetRoute: String = "custom-surface-v1"

    var origin: URL { profile.publicOrigin }

    func authorize(_ request: inout URLRequest) {
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
    }
}

/// The closed notice a client renders when it cannot instantiate the
/// host contract — never a degraded fallback that widens anything.
struct AppSurfaceUnsupportedNotice: View {
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "lock.shield")
                .font(.title)
                .foregroundStyle(.secondary)
            Text(AppSurfaceScriptedPolicy.unsupportedNotice)
                .font(.callout)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(maxWidth: .infinity, minHeight: 160)
        .accessibilityIdentifier("app-surface-unsupported-notice")
    }
}

/// The closed failure notice rendered in place of a torn-down surface —
/// parity with the web host's closed-state handling (a refused session
/// replaces the frame with the kernel's failed-safely copy).
struct AppSurfaceFailedNotice: View {
    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "xmark.shield")
                .font(.title)
                .foregroundStyle(.secondary)
            Text(AppSurfaceScriptedPolicy.failedNotice)
                .font(.callout)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(maxWidth: .infinity, minHeight: 160)
        .accessibilityIdentifier("app-surface-failed-notice")
    }
}

/// The script-message bridge relay. The frame's only channel is the
/// single `magicianSurfaceBridge` handler; every method outside the
/// closed set and every sequence gap is dropped, and the message that
/// would exceed the message budget is answered with an error reply
/// before the session closes — the frame's sequence counter must never
/// silently run ahead of a dropped message.
///
/// The queue is generic so the ordering invariant can be pinned without a
/// network fixture. It is main-thread confined: WebKit admission order enters
/// here unchanged, and completion of one authenticated HTTP submission starts
/// exactly one successor. The completion callback has no success/failure bit on
/// purpose — either outcome advances the FIFO, so one bounded operation error
/// cannot poison later sequence numbers.
final class AppSurfaceScriptedBridgeSubmissionFIFO<Element> {
    typealias Submit = (Element, @escaping () -> Void) -> Void

    private let submit: Submit
    private var pending: [Element] = []
    private var inFlight = false

    init(submit: @escaping Submit) {
        self.submit = submit
    }

    func enqueue(_ element: Element) {
        dispatchPrecondition(condition: .onQueue(.main))
        pending.append(element)
        startNextIfIdle()
    }

    func cancelPending() {
        dispatchPrecondition(condition: .onQueue(.main))
        pending.removeAll(keepingCapacity: false)
    }

    private func startNextIfIdle() {
        dispatchPrecondition(condition: .onQueue(.main))
        guard !inFlight, !pending.isEmpty else { return }
        inFlight = true
        let next = pending.removeFirst()
        submit(next) { [weak self] in
            let advance = {
                guard let self else { return }
                self.inFlight = false
                self.startNextIfIdle()
            }
            if Thread.isMainThread {
                advance()
            } else {
                DispatchQueue.main.async(execute: advance)
            }
        }
    }
}

final class AppSurfaceScriptedBridge: NSObject, WKScriptMessageHandler {
    private struct PendingRelay {
        let requestID: String
        let method: String
        let sequence: Int
        let payload: Any?
        let viewOrAction: String?
    }

    private let context: AppSurfaceHostContext
    private let plan: AppSurfaceScriptedPlan
    private let session: URLSession
    private var lastSequence = 0
    private var messages = 0
    private var closedNotice: String?
    private var isShutdown = false
    private lazy var submissions = AppSurfaceScriptedBridgeSubmissionFIFO<PendingRelay> {
        [weak self] relay, completion in
        guard let self else {
            completion()
            return
        }
        self.submit(relay, completion: completion)
    }

    /// Parity with the web host's closed-state handling: a 409 watchdog
    /// refusal (the kernel answers 409 `app_custom_surface_denied`/
    /// `app_custom_surface_unavailable`; the 410 branch below is
    /// defensive client parity) closes the session — the bridge stops
    /// relaying, no reply reaches the frame, and the host swaps the
    /// surface for the closed failure notice.
    var onSessionClosed: ((String) -> Void)?

    init(context: AppSurfaceHostContext, plan: AppSurfaceScriptedPlan) {
        self.context = context
        self.plan = plan
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        configuration.httpCookieAcceptPolicy = .never
        self.session = URLSession(configuration: configuration)
        super.init()
    }

    func shutdown() {
        guard Thread.isMainThread else {
            DispatchQueue.main.async { [weak self] in self?.shutdown() }
            return
        }
        isShutdown = true
        submissions.cancelPending()
        session.invalidateAndCancel()
        replyWebView = nil
    }

    private func closeSession(_ notice: String) {
        guard Thread.isMainThread else {
            DispatchQueue.main.async { [weak self] in self?.closeSession(notice) }
            return
        }
        guard closedNotice == nil else { return }
        closedNotice = notice
        submissions.cancelPending()
        replyWebView?.stopLoading()
        let handler = onSessionClosed
        DispatchQueue.main.async { handler?(notice) }
    }

    func userContentController(
        _ userContentController: WKUserContentController,
        didReceive message: WKScriptMessage
    ) {
        guard message.name == AppSurfaceScriptedPolicy.bridgeChannel,
              !isShutdown,
              closedNotice == nil,
              let body = message.body as? [String: Any],
              let method = body["method"] as? String,
              AppSurfaceScriptedPolicy.bridgeMethodIsAdmitted(method),
              let sequence = body["sequence"] as? Int,
              sequence == lastSequence + 1,
              let requestID = body["request_id"] as? String,
              !requestID.isEmpty
        else { return }
        guard messages < AppSurfaceScriptedPolicy.maximumBridgeMessages else {
            // Budget exhausted: a silent local drop would run the frame's
            // sequence counter ahead and wedge every later message with no
            // closed-state UX. Answer THIS message with an error reply,
            // then close the session exactly like the server's 409
            // watchdog refusal does.
            deliver(reply: ["request_id": requestID, "error": "bridge_message_budget_exceeded"])
            closeSession(AppSurfaceScriptedPolicy.failedNotice)
            return
        }
        lastSequence = sequence
        messages += 1
        let viewOrAction = body["view_or_action"] as? String
        relay(
            requestID: requestID,
            method: method,
            sequence: sequence,
            payload: body["payload"],
            viewOrAction: (viewOrAction?.isEmpty == false) ? viewOrAction : nil
        )
    }

    private func relay(
        requestID: String,
        method: String,
        sequence: Int,
        payload: Any?,
        viewOrAction: String?
    ) {
        let relay = PendingRelay(
            requestID: requestID,
            method: method,
            sequence: sequence,
            payload: payload,
            viewOrAction: viewOrAction
        )
        let enqueue = { [weak self] in
            guard let self, !self.isShutdown, self.closedNotice == nil else { return }
            self.submissions.enqueue(relay)
        }
        if Thread.isMainThread {
            enqueue()
        } else {
            DispatchQueue.main.async(execute: enqueue)
        }
    }

    /// Submit exactly one FIFO head. Even an ordinary HTTP/decoding error calls
    /// `completion`, so the next admitted sequence may proceed. A session-level
    /// 409/410 closes the bridge and clears pending heads before completion;
    /// replies continue to use this head's captured request id.
    private func submit(_ relay: PendingRelay, completion: @escaping () -> Void) {
        dispatchPrecondition(condition: .onQueue(.main))
        guard !isShutdown, closedNotice == nil else {
            completion()
            return
        }
        guard let bridgeURL = URL(
            string: "/api/magician/v2/apps/installations/\(context.installationID)/custom-surface-v1/bridge",
            relativeTo: context.origin
        )?.absoluteURL else {
            deliver(reply: ["request_id": relay.requestID, "error": "bridge_bad_url"])
            completion()
            return
        }
        var request = URLRequest(url: bridgeURL)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        context.authorize(&request)
        var envelope: [String: Any] = [
            "schema_version": 1,
            "request_id": relay.requestID,
            "sequence": relay.sequence,
            "method": relay.method,
            "origin": "null",
            "session_ref": plan.sessionRef,
            "nonce": plan.nonce,
            "installation_id": context.installationID,
            "package_revision_ref": plan.packageRevisionRef,
            "surface_revision": plan.surfaceRevision,
            "grant_revision": plan.grantRevision,
            "payload": relay.payload ?? [:]
        ]
        // The kernel uses `view_or_action` as the InvokeAction cross-check;
        // it is forwarded exactly like the web host forwards it.
        if let viewOrAction = relay.viewOrAction {
            envelope["view_or_action"] = viewOrAction
        }
        request.httpBody = try? JSONSerialization.data(withJSONObject: envelope)
        let task = session.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self else {
                    completion()
                    return
                }
                defer { completion() }
                guard !self.isShutdown, self.closedNotice == nil else { return }
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                // Failure parity with the web host: a 409 watchdog refusal
                // (410 kept as defensive client parity — the kernel answers
                // 409) tears the session down (no reply, closed notice); any
                // other non-2xx is an error reply; only 2xx JSON is a result.
                if status == 409 || status == 410 {
                    self.closeSession(AppSurfaceScriptedPolicy.failedNotice)
                    return
                }
                let reply: [String: Any]
                if let error {
                    reply = ["request_id": relay.requestID, "error": error.localizedDescription]
                } else if !(200...299).contains(status) {
                    reply = ["request_id": relay.requestID, "error": "bridge_http_\(status)"]
                } else if let data, let json = try? JSONSerialization.jsonObject(with: data) {
                    reply = ["request_id": relay.requestID, "result": json]
                } else {
                    reply = ["request_id": relay.requestID, "error": "bridge_failed"]
                }
                self.deliver(reply: reply)
            }
        }
        task.resume()
    }

    /// Deliver one reply to the frame over the single JS channel. The
    /// payload is escaped against raw U+2028/U+2029 — JavaScript line
    /// terminators JSONSerialization may legally emit inside string
    /// literals — so the evaluation never throws (and the reply never
    /// gets lost) on a legal payload.
    private func deliver(reply: [String: Any]) {
        guard closedNotice == nil,
              let payload = (try? JSONSerialization.data(withJSONObject: reply))
                  .flatMap({ String(data: $0, encoding: .utf8) })
        else { return }
        let script =
            "window.__magicianSurfaceBridgeReply(\(AppSurfaceScriptedPolicy.javaScriptSafeJSON(payload)));"
        DispatchQueue.main.async {
            self.replyWebView?.evaluateJavaScript(script, completionHandler: nil)
        }
    }

    /// Record one renderer crash or reload against the kernel's
    /// reload/crash budget (the same reload-note route the web host
    /// calls). Authentication rides the proxied request — the workspace-bound
    /// device bearer is attached here, never by the frame. A
    /// 409 refusal (budget quarantined, session gone — the kernel only
    /// ever answers 409 on this route; the 410 branch is defensive
    /// client parity) runs the same closed path a refused bridge
    /// message runs.
    func noteReload() {
        guard closedNotice == nil,
              let reloadNoteURL = AppSurfaceScriptedPolicy.reloadNoteURL(
                origin: context.origin,
                installationID: context.installationID,
                session: plan.sessionRef
              )
        else { return }
        var request = URLRequest(url: reloadNoteURL)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        context.authorize(&request)
        let task = session.dataTask(with: request) { [weak self] _, response, _ in
            guard let self else { return }
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if status == 409 || status == 410 {
                self.closeSession(AppSurfaceScriptedPolicy.failedNotice)
            }
        }
        task.resume()
    }

    weak var replyWebView: WKWebView?
}

/// The minted host plan as fetched from the host endpoint; every binding
/// field here is authority the frame itself never supplies. The
/// installation identity is decoded too (not just implied by the request)
/// so the mounting flow can refuse a plan minted for another installation,
/// mirroring the web host's identity check.
struct AppSurfaceScriptedPlan: Equatable, Codable {
    let installationID: String
    let sessionRef: String
    let nonce: String
    let packageRevisionRef: String
    let surfaceRevision: Int
    let grantRevision: Int
    let entryDocumentDigest: String
    let entryDocument: String

    enum CodingKeys: String, CodingKey {
        case installationID = "installation_id"
        case sessionRef = "session_ref"
        case nonce
        case packageRevisionRef = "package_revision_ref"
        case surfaceRevision = "surface_revision"
        case grantRevision = "grant_revision"
        case entryDocumentDigest = "entry_document_digest"
        case entryDocument = "entry_document"
    }
}

/// Wiring-level observation (1.6 completion): publishes the host's closed
/// session notice so the mounting surface can present
/// `AppSurfaceFailedNotice` in place of the torn-down frame. The notice
/// arrives on the main queue (the bridge's `onSessionClosed` dispatches
/// there), matching the SwiftUI publishing discipline this file already
/// uses for reply evaluation.
final class AppSurfaceScriptedSessionObserver: ObservableObject {
    @Published private(set) var closedNotice: String?

    func sessionDidClose(_ notice: String) {
        guard closedNotice == nil else { return }
        closedNotice = notice
    }
}

/// One scripted surface frame: nonpersistent storage, per-installation
/// scheme origin, deny-all-but-initial navigation, redirects rejected,
/// single bridge channel, 32 MiB resource cap.
struct AppSurfaceScriptedWebView: UIViewRepresentable {
    let context: AppSurfaceHostContext
    let plan: AppSurfaceScriptedPlan
    /// Wiring hook (1.6 completion): receives the closed-session notice
    /// when the bridge session is refused, so the mounting view swaps the
    /// torn-down frame for `AppSurfaceFailedNotice`. Optional so the host
    /// itself stays self-contained.
    var sessionObserver: AppSurfaceScriptedSessionObserver? = nil

    func makeCoordinator() -> Coordinator {
        Coordinator(context: context, plan: plan, sessionObserver: sessionObserver)
    }

    func makeUIView(context uiContext: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.setURLSchemeHandler(
            uiContext.coordinator.schemeHandler,
            forURLScheme: AppSurfaceScriptedPolicy.scheme
        )
        configuration.userContentController.add(
            uiContext.coordinator.bridge,
            name: AppSurfaceScriptedPolicy.bridgeChannel
        )
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = uiContext.coordinator
        uiContext.coordinator.bridge.replyWebView = webView
        webView.isInspectable = false
        webView.load(URLRequest(url: context.initialURL))
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context _: Context) {}

    static func dismantleUIView(_ uiView: WKWebView, coordinator: Coordinator) {
        uiView.stopLoading()
        coordinator.shutdown()
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        private let context: AppSurfaceHostContext
        fileprivate let schemeHandler: AppSurfaceSchemeHandler
        fileprivate let bridge: AppSurfaceScriptedBridge
        private let sessionObserver: AppSurfaceScriptedSessionObserver?
        private var initialLoadSeen = false
        /// Set while one crash-recovery reload is in flight: the reload
        /// re-loads the same canonical entry document, so it is admitted
        /// through the same `permits` rule the initial load uses, exactly
        /// once per renderer crash. The allowance is consumed by the one
        /// navigation decision that consults it (read-and-clear in
        /// `decidePolicyFor`), so a cancelled crash-recovery reload can
        /// never leave it parked for an unrelated later navigation to
        /// inherit.
        private var crashRecoveryReloadPending = false

        /// The closed notice recorded when the bridge session is refused
        /// (409 watchdog parity with the web host; the 410 branch is
        /// defensive client parity). The host presents
        /// `AppSurfaceFailedNotice` in place of the torn-down frame.
        private(set) var closedNotice: String?

        init(
            context: AppSurfaceHostContext,
            plan: AppSurfaceScriptedPlan,
            sessionObserver: AppSurfaceScriptedSessionObserver? = nil
        ) {
            self.context = context
            self.schemeHandler = AppSurfaceSchemeHandler(context: context)
            self.bridge = AppSurfaceScriptedBridge(context: context, plan: plan)
            self.sessionObserver = sessionObserver
            super.init()
            bridge.onSessionClosed = { [weak self] notice in
                self?.close(notice)
                // The bridge dispatches this handler on the main queue, so
                // the published notice satisfies SwiftUI's main-thread
                // publishing discipline.
                self?.sessionObserver?.sessionDidClose(notice)
            }
        }

        func close(_ notice: String) {
            guard closedNotice == nil else { return }
            closedNotice = notice
            bridge.replyWebView?.stopLoading()
        }

        func shutdown() {
            schemeHandler.shutdown()
            bridge.shutdown()
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            // Consume-once: the crash-recovery allowance applies to
            // exactly THIS navigation decision — read-and-clear before the
            // `permits` consultation, so a cancelled decision cannot leave
            // the flag set and make the NEXT navigation
            // initial-load-eligible. (WKWebView exposes no testable seam
            // for this delegate path; the independent safety gate is the
            // URL equality inside `permits` below, which pins the
            // destination to the canonical entry document either way.)
            let crashRecoveryReload = crashRecoveryReloadPending
            crashRecoveryReloadPending = false
            guard let url = navigationAction.request.url,
                  AppSurfaceScriptedPolicy.permits(
                    navigationURL: url,
                    initialURL: context.initialURL,
                    isInitialLoad: !initialLoadSeen || crashRecoveryReload
                  )
            else {
                decisionHandler(.cancel)
                return
            }
            initialLoadSeen = true
            decisionHandler(.allow)
        }

        /// Post-failure parity with the web host's frame-error hook: the
        /// failed provisional load is observed here so the host keeps an
        /// exact record of the initial entry load's outcome; the surface's
        /// own budgets govern any further loading.
        func webView(
            _ webView: WKWebView,
            didFailProvisionalNavigation navigation: WKNavigation!,
            withError error: Error
        ) {
            _ = webView
            _ = navigation
            _ = error
        }

        /// Renderer crash recovery: the WebView reloads its current — and
        /// only ever loaded — canonical entry document, and the crash is
        /// recorded against the kernel's reload/crash budget through the
        /// reload-note route. A 409 there (budget quarantined, session
        /// gone — the kernel only ever answers 409 on this route; the
        /// client's 410 branch is defensive parity) runs the same
        /// `onSessionClosed` flow a watchdog refusal
        /// on the bridge takes, so the failed notice replaces the frame
        /// and a surface can never crash-loop past its budget unnoticed.
        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            crashRecoveryReloadPending = true
            webView.reload()
            bridge.noteReload()
        }
    }
}

/// The scheme proxy: serves digest-keyed surface bytes from the backend
/// with authentication attached only here, rejects every redirect, and
/// enforces the 32 MiB resource cap on delivery.
final class AppSurfaceSchemeHandler: NSObject, WKURLSchemeHandler, URLSessionDataDelegate, URLSessionTaskDelegate {
    private struct Pending {
        let schemeTask: WKURLSchemeTask
        let networkTask: URLSessionDataTask
        var receivedBytes = 0
    }

    private static let maximumResourceBytes = Int(AppRouteResourceLimits.maximumResponseBytes)
    private let context: AppSurfaceHostContext
    private let deliveryQueue = DispatchQueue(label: "ai.magicbeans.magican.app-surface-delivery")
    private var pendingBySessionTask: [Int: Pending] = [:]
    private lazy var session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 60
        configuration.httpCookieAcceptPolicy = .never
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        return URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
    }()

    init(context: AppSurfaceHostContext) {
        self.context = context
        super.init()
    }

    func shutdown() {
        let networkTasks: [URLSessionDataTask] = deliveryQueue.sync {
            let tasks = pendingBySessionTask.values.map(\.networkTask)
            pendingBySessionTask.removeAll(keepingCapacity: false)
            return tasks
        }
        networkTasks.forEach { $0.cancel() }
        session.invalidateAndCancel()
    }

    func webView(_ webView: WKWebView, start urlSchemeTask: WKURLSchemeTask) {
        guard let sourceURL = urlSchemeTask.request.url,
              let destination = AppSurfaceScriptedPolicy.destinationURL(
                for: sourceURL,
                origin: context.origin,
                installationID: context.installationID,
                assetRoute: context.assetRoute
              )
        else {
            urlSchemeTask.didFailWithError(URLError(.badURL))
            return
        }
        var request = URLRequest(url: destination, timeoutInterval: 30)
        request.httpMethod = "GET"
        context.authorize(&request)
        let task = session.dataTask(with: request)
        deliveryQueue.sync {
            pendingBySessionTask[task.taskIdentifier] = Pending(
                schemeTask: urlSchemeTask,
                networkTask: task
            )
        }
        task.resume()
    }

    func webView(_ webView: WKWebView, stop urlSchemeTask: WKURLSchemeTask) {
        let identity = ObjectIdentifier(urlSchemeTask)
        let networkTask: URLSessionDataTask? = deliveryQueue.sync {
            guard let identifier = pendingBySessionTask.first(where: {
                ObjectIdentifier($0.value.schemeTask) == identity
            })?.key else { return nil }
            return pendingBySessionTask.removeValue(forKey: identifier)?.networkTask
        }
        networkTask?.cancel()
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        // A valid surface asset is canonical and never redirects; rejecting
        // all redirects prevents credentials crossing origins.
        completionHandler(nil)
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        deliveryQueue.async { [weak self] in
            guard let self,
                  let pending = self.pendingBySessionTask[dataTask.taskIdentifier],
                  AppRouteResourceLimits.admitsExpectedContentLength(
                    response.expectedContentLength
                  ),
                  let http = response as? HTTPURLResponse,
                  let rewritten = HTTPURLResponse(
                    url: pending.schemeTask.request.url ?? response.url!,
                    statusCode: http.statusCode,
                    httpVersion: "HTTP/1.1",
                    headerFields: http.allHeaderFields.reduce(into: [String: String]()) {
                        result, pair in
                        if let key = pair.key as? String, let value = pair.value as? String,
                           ["cache-control", "content-security-policy", "content-type",
                            "etag", "x-content-type-options"].contains(key.lowercased()) {
                            result[key] = value
                        }
                    }
                  )
        else {
                completionHandler(.cancel)
                self?.failOnDeliveryQueue(dataTask.taskIdentifier, error: URLError(.badServerResponse))
                return
            }
            pending.schemeTask.didReceive(rewritten)
            completionHandler(.allow)
        }
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        deliveryQueue.async { [weak self] in
            guard let self, var pending = self.pendingBySessionTask[dataTask.taskIdentifier]
            else { return }
            let (newSize, overflow) = pending.receivedBytes.addingReportingOverflow(data.count)
            guard !overflow, newSize <= Self.maximumResourceBytes else {
                self.pendingBySessionTask.removeValue(forKey: dataTask.taskIdentifier)
                pending.networkTask.cancel()
                pending.schemeTask.didFailWithError(URLError(.dataLengthExceedsMaximum))
                return
            }
            pending.receivedBytes = newSize
            self.pendingBySessionTask[dataTask.taskIdentifier] = pending
            pending.schemeTask.didReceive(data)
        }
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        deliveryQueue.async { [weak self] in
            guard let pending = self?.pendingBySessionTask.removeValue(forKey: task.taskIdentifier)
            else { return }
            if let error {
                pending.schemeTask.didFailWithError(error)
            } else {
                pending.schemeTask.didFinish()
            }
        }
    }

    private func failOnDeliveryQueue(_ taskIdentifier: Int, error: Error) {
        let pending = pendingBySessionTask.removeValue(forKey: taskIdentifier)
        pending?.networkTask.cancel()
        pending?.schemeTask.didFailWithError(error)
    }
}

// MARK: - Page-bounded mini-frame host (gate S4, iOS half)

/// Widget-sized mini frames, mirroring the web host's contract
/// (`appMiniFrame.ts`) rather than forking an iOS dialect. The render model is
/// platform-neutral by design, so every budget, every plan field and every
/// refusal reason here is the web one.
///
/// A mini frame is the reviewed ESCALATION for bespoke rendering, never a
/// default. Three independent host acts must all succeed before one line of
/// app HTML runs inside a page:
///
///  1. the widget render batch carries a `mini_frame` declaration BESIDE a
///     complete native model — the exact same-view fallback the runtime
///     already compiles — so a refused frame degrades to real content and
///     never to a hole;
///  2. the host mints a plan for that exact installation/widget/generation;
///  3. this client's page, session and visible budgets still have room.
///
/// Any one of them failing leaves the native model rendered.
enum AppMiniFramePolicy {
    /// The synthetic per-installation origin. Mini-frame assets ride the same
    /// scheme proxy the full-page scripted host uses, pointed at the
    /// mini-frame asset route; each frame gets its own nonpersistent store, so
    /// two frames for one installation still share nothing.
    static let assetRoute = "mini-frame-v1"
    /// The kernel sandbox constant, identical to the full-page scripted host.
    /// `allow-same-origin` would hand the frame the host's own origin, so it
    /// is not a tunable and a plan naming anything else is refused.
    static let sandbox = "allow-scripts"
    /// At most two mini frames may be admitted for one page's widget regions.
    static let maximumFramesPerPage = 2
    /// …and at most twelve across the whole app session.
    static let maximumFramesPerSession = 12
    /// …of which at most two may be mounted at the same time, anywhere.
    static let maximumVisibleFrames = 2
    /// A frame continuously out of view for this long is unmounted. Leaving a
    /// widget behind the fold must not leave a renderer running for the life
    /// of the page.
    static let unmountTTLSeconds: TimeInterval = 30
    /// Widget-sized, not page-sized: `APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX`.
    static let maximumFrameHeightPx = 480
    /// `MAX_MINI_FRAME_ENTRY_POINT_SEGMENTS`.
    static let maximumEntryPointSegments = 16
    static let maximumEntryPointBytes = 512
    static let maximumEntryPointSegmentBytes = 128

    /// The declared entry route, validated exactly as the web client validates
    /// it. `:` is excluded, so a parameterised route is refused: V1 has no
    /// input mapping for a widget, and a frame at a route nobody declared is
    /// the one thing this gate exists to prevent.
    static func isDeclaredEntryPoint(_ entryPoint: String) -> Bool {
        guard entryPoint.utf8.count <= maximumEntryPointBytes,
              entryPoint.hasPrefix("/"),
              !entryPoint.unicodeScalars.contains(where: {
                  CharacterSet.controlCharacters.contains($0)
              }),
              !entryPoint.contains("\\"), !entryPoint.contains("?"),
              !entryPoint.contains("#"), !entryPoint.contains("%"),
              !entryPoint.contains(":")
        else { return false }
        let segments = entryPoint.dropFirst().split(separator: "/", omittingEmptySubsequences: false)
        guard !segments.isEmpty, segments.count <= maximumEntryPointSegments else { return false }
        return segments.allSatisfy { segment in
            !segment.isEmpty
                && segment.utf8.count <= maximumEntryPointSegmentBytes
                && segment != "." && segment != ".."
                && segment.utf8.allSatisfy {
                    ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 90)
                        || ($0 >= 97 && $0 <= 122) || $0 == 95 || $0 == 45 || $0 == 46
                }
        }
    }
}

/// A widget's declared escalation to a sandboxed frame. It rides beside a
/// complete native model, never instead of one.
struct AppMiniFrameDeclaration: Decodable, Equatable {
    let entryPoint: String
    let maxHeightPx: Int

    enum CodingKeys: String, CodingKey, CaseIterable {
        case entryPoint = "entry_point"
        case maxHeightPx = "max_height_px"
    }

    init?(entryPoint: String, maxHeightPx: Int) {
        guard AppMiniFramePolicy.isDeclaredEntryPoint(entryPoint),
              maxHeightPx > 0,
              maxHeightPx <= AppMiniFramePolicy.maximumFrameHeightPx else { return nil }
        self.entryPoint = entryPoint
        self.maxHeightPx = maxHeightPx
    }

    init(from decoder: Decoder) throws {
        let present = try decoder.container(keyedBy: AnyMiniFrameCodingKey.self)
            .allKeys
            .map(\.stringValue)
        let values = try decoder.container(keyedBy: CodingKeys.self)
        let entryPoint = try values.decode(String.self, forKey: .entryPoint)
        let maxHeightPx = try values.decode(Int.self, forKey: .maxHeightPx)
        // The member set is exact in BOTH directions: an unrecognised key means
        // the declaration was written against a contract this client does not
        // implement, and guessing at it is how a frame ends up somewhere nobody
        // reviewed.
        guard Set(present) == Set(CodingKeys.allCases.map(\.stringValue)),
              let declaration = AppMiniFrameDeclaration(
                entryPoint: entryPoint,
                maxHeightPx: maxHeightPx
              )
        else { throw AppMiniFrameError.invalidDeclaration }
        self = declaration
    }
}

private struct AnyMiniFrameCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { return nil }
}

enum AppMiniFrameError: LocalizedError, Equatable {
    case invalidDeclaration
    case invalidPlan

    var errorDescription: String? { "The app widget mini frame was refused." }
}

/// The exact target a plan may be minted for, taken from the rendered widget.
///
/// Only a `ready` item may name one: `ready` is the render contract saying the
/// closed read compiled and produced content, and the escalation may ride only
/// on top of that. An `unsupported` or `unavailable` widget claiming a frame
/// would be asking this client to run app code IN PLACE OF content the host
/// could not produce — the exact inversion this gate refuses.
struct AppMiniFrameTarget: Equatable {
    let installationID: String
    let installationGeneration: UInt64
    let widgetID: String
    let declaration: AppMiniFrameDeclaration

    /// Stable ledger key for one rendered widget's frame.
    var ledgerKey: String {
        "\(installationID)\u{0}\(widgetID)\u{0}\(installationGeneration)"
    }

    init?(item: AppNativeWidgetItem) {
        guard case .ready = item.state,
              let declaration = item.miniFrame,
              let generation = item.installationGeneration, generation > 0,
              AppsDirectoryContract.isOpaqueID(item.installationID),
              AppsDirectoryContract.isName(item.widgetID) else { return nil }
        installationID = item.installationID
        installationGeneration = generation
        widgetID = item.widgetID
        self.declaration = declaration
    }
}

/// A host-minted mini-frame plan. Every field is checked against the target
/// that was asked for, not merely against a shape: a plan for another
/// installation, widget or generation is a stale or substituted binding, and
/// mounting it would give one app's declaration another app's frame.
struct AppMiniFrameHostPlan: Decodable, Equatable {
    let schemaVersion: Int
    let sandbox: String
    let csp: String
    let sessionRef: String
    let nonce: String
    let installationID: String
    let installationGeneration: UInt64
    let packageRevisionRef: String
    let widgetID: String
    let entryPoint: String
    let entryDocument: String
    let entryDocumentDigest: String
    let entryURL: String
    let maxHeightPx: Int

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case sandbox, csp, nonce
        case sessionRef = "session_ref"
        case installationID = "installation_id"
        case installationGeneration = "installation_generation"
        case packageRevisionRef = "package_revision_ref"
        case widgetID = "widget_id"
        case entryPoint = "entry_point"
        case entryDocument = "entry_document"
        case entryDocumentDigest = "entry_document_digest"
        case entryURL = "entry_url"
        case maxHeightPx = "max_height_px"
    }

    func isAdmitted(for target: AppMiniFrameTarget) -> Bool {
        schemaVersion == 1
            && sandbox == AppMiniFramePolicy.sandbox
            // Named explicitly so a future widening of the constant cannot let
            // the frame onto the host's own origin by accident.
            && !sandbox.contains("allow-same-origin")
            && csp.contains("default-src 'none'")
            && csp.contains("connect-src 'none'")
            && csp.contains("script-src 'self'")
            && csp.contains("frame-ancestors")
            && AppsDirectoryContract.isReference(sessionRef)
            // The frame's credential rides the asset path, so a session
            // reference that would need percent-encoding is refused rather
            // than rewritten.
            && AppSurfaceScriptedPolicy.isPathSafeSessionReference(sessionRef)
            && !nonce.isEmpty
            && installationID == target.installationID
            && installationGeneration == target.installationGeneration
            && widgetID == target.widgetID
            && AppsDirectoryContract.isReference(packageRevisionRef)
            && entryPoint == target.declaration.entryPoint
            && entryDocument.hasSuffix(".html")
            && AppSurfaceScriptedPolicy.isCanonicalAssetTail(
                digest: entryDocumentDigest,
                path: entryDocument
            )
            && entryURL == Self.expectedEntryURL(
                installationID: installationID,
                sessionRef: sessionRef,
                digest: entryDocumentDigest,
                document: entryDocument
            )
            && maxHeightPx == target.declaration.maxHeightPx
            && maxHeightPx > 0
            && maxHeightPx <= AppMiniFramePolicy.maximumFrameHeightPx
    }

    static func expectedEntryURL(
        installationID: String,
        sessionRef: String,
        digest: String,
        document: String
    ) -> String {
        "/api/magician/v2/apps/installations/\(installationID)/"
            + "\(AppMiniFramePolicy.assetRoute)/assets/\(sessionRef)/\(digest)/\(document)"
    }
}

/// Ask the host to mint a plan for one admitted target. It is only ever called
/// after a declaration was found, so a deployment whose widgets declare no
/// frames issues no requests at all. Every refusal — including the 404 of a
/// host that mints no mini frames — throws, and the caller keeps the native
/// model.
enum AppMiniFrameHostClient {
    static let maximumResponseBytes = 64 * 1_024

    static func hostRequest(
        profile: MobileConnectionProfile,
        target: AppMiniFrameTarget
    ) throws -> URLRequest {
        guard var components = URLComponents(
            url: profile.publicOrigin.appendingPathComponent(
                "api/magician/v2/apps/installations/\(target.installationID)/"
                    + "\(AppMiniFramePolicy.assetRoute)/host"
            ),
            resolvingAgainstBaseURL: false
        ) else { throw AppMiniFrameError.invalidPlan }
        components.queryItems = [URLQueryItem(name: "widget", value: target.widgetID)]
        guard let url = components.url else { throw AppMiniFrameError.invalidPlan }
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
        return request
    }

    static func decodePlan(_ data: Data, target: AppMiniFrameTarget) throws -> AppMiniFrameHostPlan {
        guard !data.isEmpty, data.count <= maximumResponseBytes else {
            throw AppMiniFrameError.invalidPlan
        }
        let plan = try JSONDecoder().decode(AppMiniFrameHostPlan.self, from: data)
        guard plan.isAdmitted(for: target) else { throw AppMiniFrameError.invalidPlan }
        return plan
    }

    static func fetchPlan(
        profile: MobileConnectionProfile,
        target: AppMiniFrameTarget
    ) async throws -> AppMiniFrameHostPlan {
        let (data, response) = try await BoundedAppsDirectoryDataLoader(
            maximumBytes: maximumResponseBytes
        ).load(hostRequest(profile: profile, target: target))
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else {
            throw AppMiniFrameError.invalidPlan
        }
        return try decodePlan(data, target: target)
    }
}

enum AppMiniFrameRefusal: String, Equatable {
    case pageBudget
    case sessionBudget
    case visibleBudget
}

enum AppMiniFrameVerdict: Equatable {
    case admitted
    case refused(AppMiniFrameRefusal)
}

/// App-session ledger for mini frames: the session budget, the app-wide
/// visible limit, and the out-of-view TTL.
///
/// Time is passed in rather than read, so the TTL is testable and so a view
/// cannot sweep against a different clock than it measured with.
@MainActor
final class AppMiniFrameSessionLedger {
    /// The one ledger for this app session, mirroring the web module's single
    /// document-scoped ledger.
    static let shared = AppMiniFrameSessionLedger()

    /// One mounted frame's TTL state. A struct rather than a `Date?` value in
    /// the dictionary: assigning `nil` into a dictionary of optionals REMOVES
    /// the entry, which would silently turn "this frame became visible" into
    /// "this frame was released".
    private struct MountedFrame {
        var hiddenSince: Date?
    }

    private var sessionSpent = 0
    private var mounted: [String: MountedFrame] = [:]

    func openPage() -> AppMiniFramePageLease { AppMiniFramePageLease(ledger: self) }

    /// Page leases are the only supported entry point.
    fileprivate func admit(key: String, now: Date) -> AppMiniFrameVerdict {
        if mounted[key] != nil { return .admitted }
        guard sessionSpent < AppMiniFramePolicy.maximumFramesPerSession else {
            return .refused(.sessionBudget)
        }
        guard mounted.count < AppMiniFramePolicy.maximumVisibleFrames else {
            return .refused(.visibleBudget)
        }
        sessionSpent += 1
        // A frame starts hidden and its TTL clock starts now: one admitted
        // below the fold must expire on its own rather than linger unseen
        // until the page is left.
        mounted[key] = MountedFrame(hiddenSince: now)
        return .admitted
    }

    fileprivate func release(key: String) { mounted.removeValue(forKey: key) }

    func isMounted(key: String) -> Bool { mounted[key] != nil }

    fileprivate func noteVisibility(key: String, visible: Bool, now: Date) {
        guard let state = mounted[key] else { return }
        if visible {
            mounted[key] = MountedFrame(hiddenSince: nil)
            return
        }
        // Only the FIRST hidden report starts the clock; a repeated report must
        // not keep pushing the deadline forward.
        if state.hiddenSince == nil { mounted[key] = MountedFrame(hiddenSince: now) }
    }

    /// Unmount every frame out of view past the TTL, returning the released
    /// keys so their hosts can tear the renderer down. A frame never reported
    /// visible has been hidden since admission.
    fileprivate func sweepExpired(now: Date) -> [String] {
        let expired = mounted
            .compactMap { key, state -> String? in
                guard let hiddenSince = state.hiddenSince,
                      now.timeIntervalSince(hiddenSince) >= AppMiniFramePolicy.unmountTTLSeconds
                else { return nil }
                return key
            }
            .sorted()
        for key in expired { mounted.removeValue(forKey: key) }
        return expired
    }

    /// Test-only reset; a real app session has exactly one ledger.
    func resetForTest() {
        sessionSpent = 0
        mounted = [:]
    }

    var mountedCountForTest: Int { mounted.count }
    var sessionSpentForTest: Int { sessionSpent }
}

/// One page's claim on the session ledger. The page slot region opens a lease
/// when it mounts and closes it when it unmounts, so leaving a page returns its
/// visible slots even if a frame's own teardown was skipped.
///
/// A page budget is spent on admission and is NOT refunded by a TTL unmount:
/// the escalation was reviewed and taken. Only closing the page returns it.
@MainActor
final class AppMiniFramePageLease {
    private unowned let ledger: AppMiniFrameSessionLedger
    private var held: Set<String> = []
    private var spent = 0
    private var closed = false

    fileprivate init(ledger: AppMiniFrameSessionLedger) { self.ledger = ledger }

    func admit(key: String, now: Date) -> AppMiniFrameVerdict {
        if closed { return .refused(.pageBudget) }
        if held.contains(key) { return .admitted }
        guard spent < AppMiniFramePolicy.maximumFramesPerPage else {
            return .refused(.pageBudget)
        }
        let verdict = ledger.admit(key: key, now: now)
        guard verdict == .admitted else { return verdict }
        spent += 1
        held.insert(key)
        return verdict
    }

    func release(key: String) {
        guard held.remove(key) != nil else { return }
        ledger.release(key: key)
    }

    /// Report one held frame's visibility into the TTL clock.
    func noteVisibility(key: String, visible: Bool, now: Date) {
        guard held.contains(key) else { return }
        ledger.noteVisibility(key: key, visible: visible, now: now)
    }

    /// Sweep the session's out-of-view frames. The TTL is a property of the
    /// app session, so any lease's sweep clears every expired frame; keys this
    /// page held are forgotten here so closing the page cannot release a slot
    /// the ledger has already reclaimed for someone else.
    func sweepExpired(now: Date) -> [String] {
        let expired = ledger.sweepExpired(now: now)
        for key in expired { held.remove(key) }
        return expired
    }

    func close() {
        guard !closed else { return }
        closed = true
        for key in held { ledger.release(key: key) }
        held = []
    }
}

/// One widget-sized sandboxed frame.
///
/// It renders the widget's native model until all three of the page lease, the
/// minted plan, and the fail-closed parse succeed — and returns to the model
/// after any refusal, so a missing frame is never a missing widget.
///
/// Visibility drives the unmount TTL. iOS has no intersection observer, so the
/// honest signals are the ones the platform actually gives: the scene leaving
/// the foreground marks the frame hidden, and a view removed from the hierarchy
/// tears down at once. A frame swept by the TTL stays retired for the life of
/// this authority — the page budget was already spent, and remounting on every
/// scroll reversal would turn one reviewed escalation into an unbounded series.
struct AppMiniFrameHostView<Fallback: View>: View {
    let target: AppMiniFrameTarget
    let lease: AppMiniFramePageLease
    /// Changes whenever the rendered authority changes; forces a fresh mint.
    let authorityKey: String
    @ViewBuilder let fallback: () -> Fallback

    @Environment(\.scenePhase) private var scenePhase
    @State private var plan: AppMiniFrameHostPlan?
    @State private var context: AppSurfaceHostContext?
    @State private var admittedKey = ""
    @State private var retired = false

    var body: some View {
        Group {
            if let plan, let context, !retired {
                AppMiniFrameWebView(context: context)
                    .frame(height: CGFloat(plan.maxHeightPx))
                    .clipShape(RoundedRectangle(cornerRadius: 13, style: .continuous))
                    .accessibilityIdentifier("app-mini-frame-\(target.installationID)-\(target.widgetID)")
            } else {
                fallback()
            }
        }
        .task(id: authorityKey) { await mount() }
        .task(id: authorityKey) { await sweepLoop() }
        .onChange(of: scenePhase) { _, phase in
            guard !admittedKey.isEmpty else { return }
            lease.noteVisibility(key: admittedKey, visible: phase == .active, now: Date())
        }
        .onDisappear { teardown() }
    }

    @MainActor
    private func mount() async {
        teardown()
        retired = false
        guard let profile = MagicianAccess.connectionProfile,
              case .admitted = lease.admit(key: target.ledgerKey, now: Date()) else { return }
        admittedKey = target.ledgerKey
        lease.noteVisibility(key: admittedKey, visible: scenePhase == .active, now: Date())
        do {
            let minted = try await AppMiniFrameHostClient.fetchPlan(profile: profile, target: target)
            guard MagicianAccess.connectionProfile == profile,
                  admittedKey == target.ledgerKey,
                  let initialURL = AppSurfaceScriptedPolicy.schemeURL(
                    installationID: minted.installationID,
                    digest: minted.entryDocumentDigest,
                    path: minted.entryDocument,
                    session: minted.sessionRef
                  ) else {
                releaseAdmission()
                return
            }
            plan = minted
            context = AppSurfaceHostContext(
                profile: profile,
                installationID: minted.installationID,
                initialURL: initialURL,
                assetRoute: AppMiniFramePolicy.assetRoute
            )
        } catch {
            // Every refusal — no such host, a stale binding, a malformed plan —
            // lands here, and the native model stands.
            releaseAdmission()
        }
    }

    /// A quarter of the TTL bounds the worst-case overshoot without waking the
    /// app on a fast timer.
    @MainActor
    private func sweepLoop() async {
        let interval = max(1, AppMiniFramePolicy.unmountTTLSeconds / 4)
        while !Task.isCancelled {
            do {
                try await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            } catch {
                return
            }
            guard !Task.isCancelled, !admittedKey.isEmpty else { continue }
            if lease.sweepExpired(now: Date()).contains(admittedKey) {
                // The ledger already released the slot; drop the renderer and
                // stay retired for the life of this authority.
                admittedKey = ""
                plan = nil
                context = nil
                retired = true
            }
        }
    }

    @MainActor
    private func releaseAdmission() {
        plan = nil
        context = nil
        guard !admittedKey.isEmpty else { return }
        lease.release(key: admittedKey)
        admittedKey = ""
    }

    @MainActor
    private func teardown() {
        releaseAdmission()
    }
}

/// The frame itself: nonpersistent storage, the per-installation synthetic
/// scheme origin proxied onto the mini-frame asset route, deny-all-but-initial
/// navigation, redirects rejected, and NO bridge — the plan's CSP says
/// `connect-src 'none'`, so a mini frame is a render surface with no channel of
/// its own. The frame holds no credentials; authentication is attached only
/// inside the proxied request.
struct AppMiniFrameWebView: UIViewRepresentable {
    let context: AppSurfaceHostContext

    func makeCoordinator() -> Coordinator { Coordinator(context: context) }

    func makeUIView(context uiContext: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.setURLSchemeHandler(
            uiContext.coordinator.schemeHandler,
            forURLScheme: AppSurfaceScriptedPolicy.scheme
        )
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = uiContext.coordinator
        webView.isInspectable = false
        webView.isOpaque = false
        webView.scrollView.isScrollEnabled = false
        webView.load(URLRequest(url: context.initialURL))
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context _: Context) {}

    static func dismantleUIView(_ uiView: WKWebView, coordinator: Coordinator) {
        uiView.stopLoading()
        coordinator.shutdown()
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        private let context: AppSurfaceHostContext
        fileprivate let schemeHandler: AppSurfaceSchemeHandler
        private var initialLoadSeen = false

        init(context: AppSurfaceHostContext) {
            self.context = context
            schemeHandler = AppSurfaceSchemeHandler(context: context)
        }

        func shutdown() { schemeHandler.shutdown() }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            // The entry document is the only permitted top-level navigation.
            // A mini frame has no crash-recovery allowance: it costs a session
            // against the same budgets, so a crashed frame falls back to the
            // widget's native model instead of reloading itself.
            guard let url = navigationAction.request.url,
                  AppSurfaceScriptedPolicy.permits(
                    navigationURL: url,
                    initialURL: context.initialURL,
                    isInitialLoad: !initialLoadSeen
                  )
            else {
                decisionHandler(.cancel)
                return
            }
            initialLoadSeen = true
            decisionHandler(.allow)
        }
    }
}
