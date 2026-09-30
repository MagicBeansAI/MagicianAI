import SwiftUI
import WebKit
import PDFKit
import AVKit
import UIKit
import MarkdownUI

// MARK: - Artifact model

/// A result artifact + the type routing that decides how to render it (mirrors the
/// web ChatContentBlocks dispatch). Built from a task output (deep panel) or a
/// direct URL (chat content blocks).
struct ArtifactRef: Identifiable, Equatable {
    let id = UUID()
    let resolvedURL: String
    let mime: String?
    let displayName: String

    var filename: String { displayName }
    var url: URL? { URL(string: resolvedURL) }
    var kind: ArtifactKind { ArtifactKind.from(mime: mime, filename: displayName) }
    var isMagicianOwned: Bool {
        MagicianAccess.isMagicianRuntimeURL(url, profile: MagicianAccess.connectionProfile)
    }

    /// Access-gated task output on the tunnel.
    static func taskOutput(taskId: String, relativePath: String, mime: String?) -> ArtifactRef {
        let normalized = relativePath.replacingOccurrences(
            of: "^/?outputs/",
            with: "",
            options: .regularExpression
        )
        let encoded = normalized
            .split(separator: "/")
            .filter { $0 != "." && $0 != ".." }
            .map { $0.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? String($0) }
            .joined(separator: "/")
        let url = "\(MagicianAccess.baseURL.absoluteString)/api/magician/v3/tasks/\(taskId)/outputs/\(encoded)"
        return ArtifactRef(resolvedURL: url, mime: mime, displayName: (normalized as NSString).lastPathComponent)
    }

    /// Access-gated output owned by a chat session rather than a task.
    static func sessionOutput(sessionId: String, relativePath: String, mime: String?) -> ArtifactRef {
        let normalized = relativePath.replacingOccurrences(
            of: "^/?outputs/",
            with: "",
            options: .regularExpression
        )
        let encoded = normalized
            .split(separator: "/")
            .filter { $0 != "." && $0 != ".." }
            .map { $0.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? String($0) }
            .joined(separator: "/")
        let url = "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/outputs/\(encoded)"
        return ArtifactRef(resolvedURL: url, mime: mime, displayName: (normalized as NSString).lastPathComponent)
    }

    /// A content-block URL (absolute, or tunnel-relative like `/api/…`).
    static func direct(
        url: String,
        mime: String?,
        name: String?,
        baseURL: URL = MagicianAccess.baseURL
    ) -> ArtifactRef {
        let full = url.hasPrefix("http")
            ? url
            : "\(baseURL.absoluteString)\(url.hasPrefix("/") ? "" : "/")\(url)"
        let derived = name ?? (URL(string: full)?.lastPathComponent ?? "file")
        return ArtifactRef(resolvedURL: full, mime: mime, displayName: derived)
    }
}

enum ArtifactKind: Equatable {
    case image, pdf, html, video, audio
    case text(TextKind)
    case other
    enum TextKind { case markdown, json, plain }

    static func from(mime: String?, filename: String) -> ArtifactKind {
        let m = (mime ?? "").lowercased()
        let ext = (filename as NSString).pathExtension.lowercased()
        if m.hasPrefix("image/") || ["png", "jpg", "jpeg", "gif", "webp", "heic"].contains(ext) { return .image }
        if m == "application/pdf" || ext == "pdf" { return .pdf }
        if m == "text/html" || m == "application/xhtml+xml" || ["html", "htm", "xhtml"].contains(ext) { return .html }
        if m.hasPrefix("video/") || ["mp4", "mov", "m4v", "webm"].contains(ext) { return .video }
        if m.hasPrefix("audio/") || ["mp3", "wav", "m4a", "aac"].contains(ext) { return .audio }
        if m == "text/markdown" || ["md", "markdown"].contains(ext) { return .text(.markdown) }
        if m == "application/json" || m == "text/json" || ext == "json" { return .text(.json) }
        if m.hasPrefix("text/") || ["txt", "log", "csv", "tsv"].contains(ext) { return .text(.plain) }
        return .other
    }

    var label: String {
        switch self {
        case .image: return "Image"
        case .pdf: return "PDF document"
        case .html: return "HTML page"
        case .video: return "Video"
        case .audio: return "Audio"
        case .text(.markdown): return "Markdown"
        case .text(.json): return "JSON"
        case .text: return "Text"
        case .other: return "File"
        }
    }
    var systemIcon: String {
        switch self {
        case .image: return "photo"
        case .pdf: return "doc.richtext"
        case .html: return "chevron.left.forwardslash.chevron.right"
        case .video: return "play.rectangle"
        case .audio: return "waveform"
        case .text: return "doc.plaintext"
        case .other: return "doc"
        }
    }
}

// MARK: - Router view

/// Full-screen viewer that renders an artifact by type — the iOS equivalent of the
/// web "Open ↗": HTML in a WKWebView (auth headers injected on every request so
/// server-relative subresources resolve), PDF via PDFKit, video/audio via AVPlayer,
/// text/markdown/json inline, images full-screen, everything else → share sheet.
struct ArtifactViewer: View {
    let artifact: ArtifactRef
    @Environment(\.dismiss) private var dismiss
    @StateObject private var theme = ThemeManager.shared
    @State private var sharePayload: SharePayload?

    var body: some View {
        NavigationView {
            content
                .background(theme.backgroundColor.ignoresSafeArea())
                .navigationTitle(artifact.filename)
                .navigationBarTitleDisplayMode(.inline)
                .navigationBarItems(
                    leading: Button("Done") { dismiss() },
                    trailing: HStack(spacing: 18) {
                        if !artifact.isMagicianOwned {
                            Button(action: openInBrowser) { Image(systemName: "safari") }
                        }
                        Menu {
                            if !artifact.isMagicianOwned, let u = artifact.url {
                                // Sharing the URL surfaces Copy, Mail, Messages, other apps…
                                Button { sharePayload = SharePayload(items: [u]) } label: {
                                    Label("Share link", systemImage: "link")
                                }
                            }
                            Button(action: shareOriginal) {
                                Label("Share file", systemImage: "doc")
                            }
                        } label: { Image(systemName: "square.and.arrow.up") }
                    }
                )
                .sheet(item: $sharePayload) { ShareSheet(items: $0.items) }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch artifact.kind {
        case .image:
            ScrollView([.horizontal, .vertical]) {
                if let u = artifact.url { AuthAsyncImage(urlString: u.absoluteString, maxHeight: 4000) }
            }
        case .html:
            if let u = artifact.url { AuthWebView(url: u) } else { unavailable }
        case .pdf:
            if let u = artifact.url { PDFKitView(url: u) } else { unavailable }
        case .video, .audio:
            if let u = artifact.url { MediaPlayerView(url: u) } else { unavailable }
        case .text(let kind):
            if let u = artifact.url { InlineTextView(url: u, kind: kind, theme: theme) } else { unavailable }
        case .other:
            otherFallback
        }
    }

    private var unavailable: some View {
        Text("Couldn't build the artifact URL.").foregroundColor(theme.secondaryTextColor).padding()
    }

    private var otherFallback: some View {
        VStack(spacing: 16) {
            Image(systemName: "doc").font(.system(size: 44)).foregroundColor(theme.accentColor)
            Text(artifact.filename).font(.themed(17, weight: .semibold)).foregroundColor(theme.textColor)
            Button(action: shareOriginal) {
                Label("Open / Share", systemImage: "square.and.arrow.up")
                    .padding(.horizontal, 16).padding(.vertical, 10)
                    .background(theme.accentColor).foregroundColor(theme.onAccentColor).cornerRadius(12)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// External artifacts are ordinary links. Magician-owned files stay in the
    /// authenticated viewer because Safari cannot receive the device bearer.
    private func openInBrowser() {
        guard !artifact.isMagicianOwned, let u = artifact.url else { return }
        UIApplication.shared.open(u)
    }

    /// Download the raw bytes (authed) → temp file → system share sheet.
    private func shareOriginal() {
        guard let url = artifact.url else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        URLSession.shared.downloadTask(with: request) { tempURL, _, _ in
            guard let tempURL = tempURL else { return }
            let dest = FileManager.default.temporaryDirectory.appendingPathComponent(artifact.filename)
            try? FileManager.default.removeItem(at: dest)
            try? FileManager.default.moveItem(at: tempURL, to: dest)
            DispatchQueue.main.async { self.sharePayload = SharePayload(items: [dest]) }
        }.resume()
    }
}

/// Wrapper so a share sheet can be presented via `.sheet(item:)` from anywhere.
struct SharePayload: Identifiable { let id = UUID(); let items: [Any] }

// MARK: - HTML (WKWebView with per-request auth header injection)

/// Loads Access-gated HTML by serving every request (main doc + server-relative
/// subresources) through a custom-scheme handler that rewrites to the tunnel URL
/// and adds the CF-Access headers — so subresources like `<img src="/api/…">`
/// resolve exactly as they do in the web's "Open ↗".
struct AuthWebView: UIViewRepresentable {
    let url: URL
    static let scheme = "magartifact"

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.setURLSchemeHandler(ArtifactSchemeHandler(), forURLScheme: Self.scheme)
        let webView = WKWebView(frame: .zero, configuration: config)
        // Rewrite the enrolled HTTPS origin to magartifact://<host>/<path> so
        // root-relative subresources stay on the intercepted scheme.
        if let schemed = Self.toSchemeURL(url) {
            webView.load(URLRequest(url: schemed))
        }
        return webView
    }
    func updateUIView(_ uiView: WKWebView, context: Context) {}

    static func toSchemeURL(_ url: URL) -> URL? {
        guard var comps = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return nil }
        comps.scheme = scheme
        return comps.url
    }
    static func toHttpsURL(_ url: URL) -> URL? {
        guard var comps = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return nil }
        comps.scheme = "https"
        return comps.url
    }
}

private final class ArtifactSchemeHandler: NSObject, WKURLSchemeHandler {
    private var tasks: [ObjectIdentifier: URLSessionDataTask] = [:]

    func webView(_ webView: WKWebView, start urlSchemeTask: WKURLSchemeTask) {
        guard let schemedURL = urlSchemeTask.request.url,
              let realURL = AuthWebView.toHttpsURL(schemedURL) else {
            urlSchemeTask.didFailWithError(URLError(.badURL)); return
        }
        var request = URLRequest(url: realURL)
        request.httpMethod = urlSchemeTask.request.httpMethod ?? "GET"
        MagicianAccess.authorize(&request)
        let task = URLSession.shared.dataTask(with: request) { data, response, error in
            if let error = error {
                urlSchemeTask.didFailWithError(error); return
            }
            if let response = response { urlSchemeTask.didReceive(response) }
            if let data = data { urlSchemeTask.didReceive(data) }
            urlSchemeTask.didFinish()
        }
        tasks[ObjectIdentifier(urlSchemeTask)] = task
        task.resume()
    }

    func webView(_ webView: WKWebView, stop urlSchemeTask: WKURLSchemeTask) {
        let key = ObjectIdentifier(urlSchemeTask)
        tasks[key]?.cancel()
        tasks[key] = nil
    }
}

// MARK: - PDF

struct PDFKitView: UIViewRepresentable {
    let url: URL
    func makeUIView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        URLSession.shared.dataTask(with: request) { data, _, _ in
            guard let data = data, let doc = PDFDocument(data: data) else { return }
            DispatchQueue.main.async { view.document = doc }
        }.resume()
        return view
    }
    func updateUIView(_ uiView: PDFView, context: Context) {}
}

// MARK: - Video / Audio (AVPlayer with auth headers)

struct MediaPlayerView: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context: Context) -> AVPlayerViewController {
        let asset = AVURLAsset(
            url: url,
            options: ["AVURLAssetHTTPHeaderFieldsKey": MagicianAccess.authorizedHeaders(for: url)]
        )
        let item = AVPlayerItem(asset: asset)
        let controller = AVPlayerViewController()
        controller.player = AVPlayer(playerItem: item)
        return controller
    }
    func updateUIViewController(_ uiViewController: AVPlayerViewController, context: Context) {}
}

// MARK: - Inline text / markdown / json

struct InlineTextView: View {
    let url: URL
    let kind: ArtifactKind.TextKind
    @ObservedObject var theme: ThemeManager
    @State private var content: String?
    @State private var failed = false
    private let cap = 256 * 1024

    var body: some View {
        ScrollView {
            if let content = content {
                if kind == .markdown {
                    Markdown(content)
                        .markdownTextStyle { ForegroundColor(theme.textColor) }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding()
                } else {
                    Text(content)
                        // Raw text / JSON / code preview — keep monospaced so
                        // columns and indentation stay aligned in the theme's mono role.
                        .font(.themedMono(.footnote))
                        .foregroundColor(theme.textColor)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                        .padding()
                }
            } else if failed {
                Text("Preview failed.").foregroundColor(theme.secondaryTextColor).padding()
            } else {
                ProgressView().padding(40)
            }
        }
        .onAppear(perform: load)
    }

    private func load() {
        guard content == nil, !failed else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        URLSession.shared.dataTask(with: request) { data, _, _ in
            guard let data = data, var text = String(data: data.prefix(cap), encoding: .utf8) else {
                DispatchQueue.main.async { failed = true }; return
            }
            if kind == .json, let obj = try? JSONSerialization.jsonObject(with: data),
               let pretty = try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys]),
               let prettyStr = String(data: pretty, encoding: .utf8) {
                text = prettyStr
            }
            let final = text
            DispatchQueue.main.async { content = final }
        }.resume()
    }
}
