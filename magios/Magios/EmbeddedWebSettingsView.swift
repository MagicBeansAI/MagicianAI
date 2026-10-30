import SwiftUI
import UIKit
import WebKit

/// Settings → "<Product> settings": the paired machine's shared web Settings
/// (`/settings?embed=1&host=ios`), shown full screen inside the app.
///
/// The page signs itself in with the ordinary web sign-in. The paired-device
/// token is NOT injected: the server refuses device bearers for web sessions
/// (`/auth/session`), so the page redirects itself to `/login?redirectTo=…`
/// and back. The web view uses the persistent default data store so that
/// one-time sign-in (and any Cloudflare Access login in front of the machine)
/// survives relaunches.
///
/// Only Settings, the login page and the sign-in flow stay in the web view;
/// any other app page or external link opens in Safari.
struct EmbeddedWebSettingsView: View {
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.openURL) private var openURL
    @State private var target: URL?
    @State private var resolving = true
    @State private var progress: Double = 0
    @State private var loading = true
    @State private var failure: String?
    @State private var reloadToken = 0

    var body: some View {
        ZStack(alignment: .top) {
            theme.backgroundColor.ignoresSafeArea()

            if let target {
                EmbeddedSettingsWebView(
                    url: target,
                    reloadToken: reloadToken,
                    backgroundColor: UIColor(theme.backgroundColor),
                    onProgress: { progress = $0 },
                    onLoadingChange: { isLoading in
                        loading = isLoading
                        if isLoading { failure = nil }
                    },
                    onFailure: { failure = $0 },
                    openExternally: { openURL($0) }
                )
                .opacity(failure == nil ? 1 : 0)
                .accessibilityIdentifier("settings-web-view")
            }

            if let failure {
                failureView(failure)
            } else if resolving || (loading && progress < 0.15) {
                ProgressView()
                    .tint(theme.accentColor)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityLabel("Loading \(ProductIdentity.productName) settings")
            }

            if failure == nil, !resolving, loading {
                ProgressView(value: min(max(progress, 0.05), 1))
                    .progressViewStyle(.linear)
                    .tint(theme.accentColor)
            }
        }
        .navigationTitle("\(ProductIdentity.productName) settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbarColorScheme(theme.colorScheme, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(action: reload) {
                    Image(systemName: "arrow.clockwise")
                        .foregroundColor(theme.accentColor)
                }
                .disabled(target == nil)
                .accessibilityLabel("Refresh \(ProductIdentity.productName) settings")
                .accessibilityIdentifier("settings-web-refresh")
            }
        }
        .task {
            guard target == nil else { return }
            let resolved = await EmbeddedWebSettings.resolveSettingsURL()
            resolving = false
            if let resolved {
                target = resolved
            } else {
                failure = "Pair this iPhone with a machine first, then open \(ProductIdentity.productName) settings again."
            }
        }
    }

    /// The top bar's Refresh and the failure view's Retry: load the page again.
    private func reload() {
        guard target != nil else { return }
        failure = nil
        progress = 0
        loading = true
        reloadToken += 1
    }

    private func failureView(_ message: String) -> some View {
        VStack(spacing: 14) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 28, weight: .semibold))
                .foregroundColor(theme.warningColor)
            Text("\(ProductIdentity.productName) settings didn't load")
                .font(.themed(16, weight: .semibold))
                .foregroundColor(theme.textColor)
            Text(message)
                .font(.themed(13))
                .foregroundColor(theme.secondaryTextColor)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            if target != nil {
                Button(action: reload) {
                    Label("Retry", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.borderedProminent)
                .tint(theme.accentColor)
                .accessibilityIdentifier("settings-web-retry")
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Where the embedded web Settings lives and what may load inside it.
enum EmbeddedWebSettings {
    /// UI origins resolved this launch, keyed by the paired API origin, so a
    /// second open does not wait on the machine registry again.
    @MainActor private static var resolvedOrigins: [String: URL] = [:]

    /// `<web UI origin>/settings?embed=1&host=ios` for the active machine, or
    /// nil when this iPhone is not paired.
    @MainActor
    static func resolveSettingsURL(
        profile: MobileConnectionProfile? = MagicianAccess.connectionProfile
    ) async -> URL? {
        guard let profile else { return nil }
        let key = MobileConnectionProfiles.originKey(profile.publicOrigin) ?? profile.publicOrigin.absoluteString
        if let cached = resolvedOrigins[key] {
            return settingsURL(origin: cached)
        }
        let registry = (try? await RemoteMachinesClient.fetch(profile: profile).machines) ?? []
        let origin = webOrigin(profile: profile, registry: registry)
        // An unreachable registry falls back to the API origin without
        // caching it, so the next open asks again.
        if !registry.isEmpty { resolvedOrigins[key] = origin }
        return settingsURL(origin: origin)
    }

    /// The active machine's web UI origin: its registry `ui_origin` when that
    /// is usable from this iPhone, else the paired API origin.
    ///
    /// A loopback `ui_origin` would mean this iPhone, and a private-network
    /// one is unreachable while the phone is paired over the remote route.
    static func webOrigin(profile: MobileConnectionProfile, registry: [RemoteMachine]) -> URL {
        let key = MobileConnectionProfiles.originKey(profile.publicOrigin)
        let machine = registry.first { MobileConnectionProfiles.originKey($0.apiOrigin) == key }
            ?? registry.first(where: \.isSelf)
        guard let raw = machine?.uiOrigin,
              let ui = MobileEnrollmentLink.normalizedOrigin(raw),
              !isLoopback(ui.host ?? ""),
              MobileConnectionMode.forOrigin(ui) == .remote
                || MobileConnectionMode.forOrigin(profile.publicOrigin) == .sameWifi else {
            return profile.publicOrigin
        }
        return ui
    }

    static func settingsURL(origin: URL) -> URL? {
        guard var components = URLComponents(url: origin, resolvingAgainstBaseURL: false) else { return nil }
        components.path = "/settings"
        components.queryItems = [
            URLQueryItem(name: "embed", value: "1"),
            URLQueryItem(name: "host", value: "ios"),
        ]
        components.fragment = nil
        return components.url
    }

    static func isLoopback(_ rawHost: String) -> Bool {
        let host = rawHost.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        return host == "localhost" || host == "::1" || host.hasPrefix("127.")
    }

    // MARK: Navigation policy

    enum Decision: Equatable {
        case allow
        case openExternally
    }

    /// Same scheme, host and port.
    static func sameOrigin(_ lhs: URL, _ rhs: URL) -> Bool {
        lhs.scheme?.lowercased() == rhs.scheme?.lowercased()
            && lhs.host?.lowercased() == rhs.host?.lowercased()
            && effectivePort(lhs) == effectivePort(rhs)
    }

    private static func effectivePort(_ url: URL) -> Int? {
        if let port = url.port { return port }
        switch url.scheme?.lowercased() {
        case "https": return 443
        case "http": return 80
        default: return nil
        }
    }

    /// App paths that stay in the embedded view: Settings itself, the login
    /// page it redirects to, and Cloudflare Access's own callback paths.
    static func isAllowedAppPath(_ path: String) -> Bool {
        path == "/settings" || path == "/settings/"
            || path.hasPrefix("/login")
            || path.hasPrefix("/cdn-cgi/access/")
    }

    static func isCloudflareAccessHost(_ host: String?) -> Bool {
        let host = host?.lowercased() ?? ""
        return host == "cloudflareaccess.com" || host.hasSuffix(".cloudflareaccess.com")
    }

    /// Whether a navigation may load in the embedded view or goes to Safari.
    ///
    /// - Subframes and `about:` pages always load.
    /// - On the machine's origin only Settings, `/login…` and Cloudflare
    ///   Access callbacks load; other app pages open in Safari.
    /// - Off the origin, a Cloudflare Access sign-in host loads, and so does
    ///   anything reached from a page that is already off the origin (the
    ///   identity provider pages of that sign-in) or by a redirect rather
    ///   than a tapped link. A tapped external link opens in Safari.
    static func decide(
        url: URL?,
        appOrigin: URL,
        isMainFrame: Bool,
        isLinkActivated: Bool,
        currentURL: URL?
    ) -> Decision {
        guard let url else { return .allow }
        let scheme = url.scheme?.lowercased() ?? ""
        if scheme == "about" { return .allow }
        guard isMainFrame else { return .allow }
        guard scheme == "http" || scheme == "https" else { return .openExternally }
        if sameOrigin(url, appOrigin) {
            return isAllowedAppPath(url.path) ? .allow : .openExternally
        }
        if isCloudflareAccessHost(url.host) { return .allow }
        if !isLinkActivated { return .allow }
        if let currentURL,
           let currentScheme = currentURL.scheme?.lowercased(),
           currentScheme == "http" || currentScheme == "https",
           !sameOrigin(currentURL, appOrigin) {
            return .allow
        }
        return .openExternally
    }

    /// Cancelled navigations and policy interruptions are not load failures.
    static func isBenignLoadError(_ error: Error) -> Bool {
        let nsError = error as NSError
        if nsError.domain == NSURLErrorDomain && nsError.code == NSURLErrorCancelled { return true }
        // WebKitErrorFrameLoadInterruptedByPolicyChange
        if nsError.domain == "WebKitErrorDomain" && nsError.code == 102 { return true }
        return false
    }
}

/// A plain persistent-store `WKWebView` for the embedded Settings page.
private struct EmbeddedSettingsWebView: UIViewRepresentable {
    let url: URL
    let reloadToken: Int
    let backgroundColor: UIColor
    let onProgress: (Double) -> Void
    let onLoadingChange: (Bool) -> Void
    let onFailure: (String) -> Void
    let openExternally: (URL) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(parent: self) }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .default()
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = context.coordinator
        webView.uiDelegate = context.coordinator
        webView.allowsBackForwardNavigationGestures = false
        webView.isOpaque = false
        webView.backgroundColor = backgroundColor
        webView.scrollView.backgroundColor = backgroundColor
        context.coordinator.attach(to: webView)
        context.coordinator.loadedReloadToken = reloadToken
        webView.load(URLRequest(url: url))
        return webView
    }

    func updateUIView(_ webView: WKWebView, context: Context) {
        context.coordinator.parent = self
        webView.backgroundColor = backgroundColor
        webView.scrollView.backgroundColor = backgroundColor
        if context.coordinator.loadedReloadToken != reloadToken {
            context.coordinator.loadedReloadToken = reloadToken
            webView.load(URLRequest(url: url))
        }
    }

    static func dismantleUIView(_ webView: WKWebView, coordinator: Coordinator) {
        webView.stopLoading()
        coordinator.detach()
    }

    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate {
        var parent: EmbeddedSettingsWebView
        var loadedReloadToken = 0
        private var observations: [NSKeyValueObservation] = []
        private var lastExternal: (url: URL, at: Date)?
        private var lastMainResponse: Int?

        init(parent: EmbeddedSettingsWebView) { self.parent = parent }

        private var appOrigin: URL { parent.url }

        func attach(to webView: WKWebView) {
            observations = [
                webView.observe(\.estimatedProgress, options: [.new]) { [weak self] view, _ in
                    let value = view.estimatedProgress
                    DispatchQueue.main.async { self?.parent.onProgress(value) }
                },
                webView.observe(\.isLoading, options: [.new]) { [weak self] view, _ in
                    let value = view.isLoading
                    DispatchQueue.main.async { self?.parent.onLoadingChange(value) }
                },
                // SvelteKit routes in-page with pushState, which never asks
                // the navigation delegate. Watch the URL so a client-side jump
                // to another app page still leaves for Safari.
                webView.observe(\.url, options: [.new]) { [weak self] view, _ in
                    DispatchQueue.main.async { self?.checkClientSideRoute(view) }
                },
            ]
        }

        func detach() {
            observations.forEach { $0.invalidate() }
            observations = []
        }

        private func checkClientSideRoute(_ webView: WKWebView) {
            guard let current = webView.url,
                  EmbeddedWebSettings.sameOrigin(current, appOrigin),
                  !EmbeddedWebSettings.isAllowedAppPath(current.path) else { return }
            openExternally(current)
            if webView.canGoBack {
                webView.goBack()
            } else {
                webView.load(URLRequest(url: parent.url))
            }
        }

        private func openExternally(_ url: URL) {
            // A target=_blank link can reach both the policy check and
            // createWebViewWith; open it once.
            if let last = lastExternal, last.url == url, Date().timeIntervalSince(last.at) < 1 { return }
            lastExternal = (url, Date())
            parent.openExternally(url)
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            let url = navigationAction.request.url
            // No target frame: a new-window request. It never loads here.
            guard let targetFrame = navigationAction.targetFrame else {
                if let url, url.scheme?.lowercased() != "about" { openExternally(url) }
                decisionHandler(.cancel)
                return
            }
            switch EmbeddedWebSettings.decide(
                url: url,
                appOrigin: appOrigin,
                isMainFrame: targetFrame.isMainFrame,
                isLinkActivated: navigationAction.navigationType == .linkActivated,
                currentURL: webView.url
            ) {
            case .allow:
                decisionHandler(.allow)
            case .openExternally:
                if let url { openExternally(url) }
                decisionHandler(.cancel)
            }
        }

        func webView(_ webView: WKWebView, decidePolicyFor navigationResponse: WKNavigationResponse,
                     decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void) {
            if navigationResponse.isForMainFrame,
               let response = navigationResponse.response as? HTTPURLResponse {
                lastMainResponse = response.statusCode
                if response.statusCode >= 400 {
                    parent.onFailure("The settings website returned HTTP \(response.statusCode). Tap Refresh to try again.")
                }
            }
            decisionHandler(.allow)
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            let loadedURL = webView.url
            // Give a client-rendered page time to populate its document.
            DispatchQueue.main.asyncAfter(deadline: .now() + 10) { [weak self, weak webView] in
                guard let self, let webView, !webView.isLoading, webView.url == loadedURL else { return }
                webView.evaluateJavaScript("Boolean(document.body && (document.body.innerText.trim().length || document.querySelector('form,input,button,img,svg,canvas,iframe')))") { [weak self, weak webView] result, error in
                    guard let self, let webView, error == nil, webView.url == loadedURL else { return }
                    if result as? Bool == false {
                        let page = webView.url?.host ?? "this machine"
                        let status = self.lastMainResponse.map { "HTTP \($0), " } ?? ""
                        self.parent.onFailure("The settings website returned an empty page (\(status)\(page)). Tap Refresh to try again.")
                    }
                }
            }
        }

        func webView(
            _ webView: WKWebView,
            createWebViewWith configuration: WKWebViewConfiguration,
            for navigationAction: WKNavigationAction,
            windowFeatures: WKWindowFeatures
        ) -> WKWebView? {
            if let url = navigationAction.request.url, url.scheme?.lowercased() != "about" {
                openExternally(url)
            }
            return nil
        }

        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            report(error)
        }

        func webView(
            _ webView: WKWebView,
            didFailProvisionalNavigation navigation: WKNavigation!,
            withError error: Error
        ) {
            report(error)
        }

        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            parent.onFailure("The page stopped unexpectedly.")
        }

        private func report(_ error: Error) {
            guard !EmbeddedWebSettings.isBenignLoadError(error) else { return }
            parent.onFailure(error.localizedDescription)
        }
    }
}
