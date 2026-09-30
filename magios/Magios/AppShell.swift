import SwiftUI
import AVFoundation

/// Global left slide-out drawer state. The hamburger (in each tab's top bar)
/// opens it; the drawer overlay is rendered once at the `AppTabView` root so it
/// slides over the whole app (content + tab bar).
final class DrawerController: ObservableObject {
    static let shared = DrawerController()
    @Published var isOpen = false

    func open() { withAnimation(.easeOut(duration: 0.25)) { isOpen = true } }
    func close() { withAnimation(.easeOut(duration: 0.25)) { isOpen = false } }
    func toggle() { withAnimation(.easeOut(duration: 0.25)) { isOpen.toggle() } }
}

/// The hamburger button — a leading top-bar item on every tab. Kept tiny so it
/// drops into each tab's existing `.toolbar` without disturbing its other items.
struct HamburgerButton: View {
    @ObservedObject private var drawer = DrawerController.shared
    var body: some View {
        Button { drawer.toggle() } label: {
            Image(systemName: "line.3.horizontal")
                .font(.system(size: 17, weight: .semibold))
        }
        .accessibilityLabel("Menu")
    }
}

extension View {
    /// Add the hamburger as a leading top-bar item. Applied by tabs that already
    /// have a navigation bar (Tasks, Attention, Observe). Tabs with a custom
    /// header (Today, Chat) place `HamburgerButton()` in that header directly.
    func hamburgerToolbar() -> some View {
        toolbar { ToolbarItem(placement: .topBarLeading) { HamburgerButton() } }
    }
}

/// A persistent "now observing" pill docked above the tab bar on every tab while
/// an in-app capture is live — ambient safety so a running observation is never
/// forgotten. Tap to jump to Observe; Stop ends it in place.
struct ObservationMiniBar: View {
    @ObservedObject private var listen = ListenController.shared
    @ObservedObject private var theme = ThemeManager.shared
    var onTap: () -> Void

    private var isVisible: Bool {
        switch listen.state {
        case .starting, .listening, .paused: return true
        default: return false
        }
    }
    private var paused: Bool {
        if case .paused = listen.state { return true }
        return false
    }

    var body: some View {
        if isVisible {
            HStack(spacing: 10) {
                Circle()
                    .fill(paused ? theme.warningColor : theme.dangerColor)
                    .frame(width: 9, height: 9)
                Text(paused ? "Paused" : "Listening")
                    .font(.subheadline.weight(.semibold))
                    .foregroundColor(theme.textColor)
                if let started = listen.startedAt {
                    Text(started, style: .timer)
                        .font(.caption.monospacedDigit())
                        .foregroundColor(theme.secondaryTextColor)
                        .frame(maxWidth: 52, alignment: .leading)
                }
                Spacer()
                Text("Observe")
                    .font(.caption).foregroundColor(theme.secondaryTextColor)
                Button {
                    Task { await ListenController.shared.stop() }
                } label: {
                    Image(systemName: "stop.fill")
                        .font(.caption2)
                        .foregroundColor(theme.dangerColor)
                        .padding(7)
                        .background(theme.dangerColor.opacity(0.12))
                        .clipShape(Circle())
                }
                .buttonStyle(.plain)
            }
            .padding(.leading, 14)
            .padding(.trailing, 6)
            .padding(.vertical, 8)
            .background(
                Capsule()
                    .fill(theme.cardColor)
                    .overlay(Capsule().stroke((paused ? theme.warningColor : theme.dangerColor).opacity(0.55), lineWidth: 1))
            )
            .contentShape(Capsule())
            .onTapGesture { onTap() }
            .padding(.horizontal, 12)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }
}

/// The in-app half of the ambient orb: a pill docked above the tab bar for as
/// long as a listening window is open.
///
/// **It exists because the orb is not reachable while the user is inside Magican.**
/// The Dynamic Island does not present the owning app's own Live Activity while
/// that app is in the foreground, and the Lock Screen presentation is by
/// definition not on screen either — so for the whole time the user is in the
/// app, the only control that can stop an armed microphone is somewhere they
/// cannot get to. That is this feature's signature failure reached without any
/// failure at all, just by opening the app. `ObservationMiniBar` above is the
/// same answer to the same hazard for in-app capture; this is its twin.
///
/// It says that the window is available and what phrase wakes it, rather than
/// presenting quiet availability as a second "Listen" product concept.
struct AmbientMiniBar: View {
    @ObservedObject private var ambient = AmbientController.shared
    @ObservedObject private var theme = ThemeManager.shared

    /// True only for the states in which a listening window is actually open.
    ///
    /// `AmbientState.windowIsOpen` rather than a `switch` here, and rather than
    /// `state.orbPhase != nil`: that reduction maps `.recoverableError` onto
    /// `.armed`, which is right for the orb (a window that is still listening) and
    /// wrong here (a refused arm lands there with no window at all). A bar offering
    /// to stop a window that never opened is the inverse lie. The predicate moved
    /// onto the state so the ambient control in Settings answers it identically —
    /// two copies is how that bug comes back.
    private var isVisible: Bool { ambient.state.windowIsOpen }

    private var status: String {
        switch ambient.state {
        case .heard, .connecting: return "Starting conversation…"
        case .conversing(.thinking): return "Thinking"
        case .conversing(.speaking): return "Speaking"
        default:
            guard let phrase = ambient.listeningFor?.phrases.first else { return "Available" }
            return "Available — say “\(phrase)”"
        }
    }

    var body: some View {
        if isVisible {
            HStack(spacing: 10) {
                Circle()
                    .fill(theme.accentColor)
                    .frame(width: 9, height: 9)
                // The power warning the window is running in spite of — as TEXT,
                // and it REPLACES the phrase line for as long as it applies. One
                // line is the chip's shape (owner decision, 2026-07-30), and the
                // warning is the entire justification for having armed in Low
                // Power Mode at all, so it cannot be dropped and must not stack:
                // a Low Power window's most important sentence wins the only
                // line there is, and the phrase returns when the warning clears.
                Text(ambient.powerWarning?.warning ?? status)
                    .font(.subheadline.weight(.semibold))
                    .foregroundColor(ambient.powerWarning != nil ? theme.warningColor : theme.textColor)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 6)
                if let expiresAt = ambient.expiresAt {
                    Text(expiresAt, style: .timer)
                        .font(.caption.monospacedDigit())
                        .foregroundColor(theme.secondaryTextColor)
                        // Wider than `ObservationMiniBar`'s 52: that one counts
                        // elapsed time up from zero, this one counts a leash of
                        // up to eight hours down, so it has to fit `7:59:59`.
                        .frame(maxWidth: 64, alignment: .trailing)
                }
                Button {
                    // No reason: this is the user's own doing, and a reasonless
                    // end dismisses the orb immediately rather than lingering
                    // with an explanation nobody needs.
                    Task { await AmbientController.shared.disarm(reason: nil) }
                } label: {
                    Image(systemName: "stop.fill")
                        .font(.caption2)
                        .foregroundColor(theme.dangerColor)
                        .padding(7)
                        .background(theme.dangerColor.opacity(0.12))
                        // A rounded square where the twin bar keeps its circle —
                        // owner decision (2026-07-30), not drift.
                        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Stop ambient listening")
            }
            .padding(.leading, 14)
            .padding(.trailing, 6)
            .padding(.vertical, 8)
            .background(
                Capsule()
                    .fill(theme.cardColor)
                    .overlay(Capsule().stroke(theme.accentColor.opacity(0.55), lineWidth: 1))
            )
            .padding(.horizontal, 12)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }
}

/// The slide-out drawer overlay — dimmed backdrop + a left panel. Rendered once
/// at the app root. Contents are a slim utility menu: identity/scope header,
/// Settings, and About.
struct SideMenuOverlay: View {
    @ObservedObject private var drawer = DrawerController.shared
    @ObservedObject private var actions = AppActions.shared
    @ObservedObject private var theme = ThemeManager.shared
    @State private var showSettings = false
    @State private var showAudioNotes = false
    @State private var showNotes = false
    @State private var showApps = false
    @State private var showThinkingMap: Bool = {
        let arguments = ProcessInfo.processInfo.arguments
        return arguments.contains("--thinking-map")
            || arguments.contains("--thinking-map-demo")
            || arguments.contains("--thinking-map-empty")
            || arguments.contains("--thinking-map-prototype")
            || arguments.contains("--thinking-map-prototype-autoplay")
            || arguments.contains { $0.hasPrefix("--thinking-map-prototype-step=") }
    }()

    var body: some View {
        ZStack(alignment: .leading) {
            if drawer.isOpen {
                Color.black.opacity(0.35)
                    .ignoresSafeArea()
                    .onTapGesture { drawer.close() }
                    .transition(.opacity)

                SideMenuContent(
                    showSettings: $showSettings,
                    showAudioNotes: $showAudioNotes,
                    showNotes: $showNotes,
                    showApps: $showApps,
                    showThinkingMap: $showThinkingMap
                )
                    .frame(maxWidth: 300, maxHeight: .infinity, alignment: .leading)
                    .background(theme.backgroundColor.ignoresSafeArea(edges: .vertical))
                    .transition(.move(edge: .leading))
            }
        }
        .sheet(isPresented: $showSettings) {
            // SettingsView owns its own NavigationView; presented as a sheet
            // (swipe-to-dismiss) now that it's no longer a bottom tab.
            SettingsView()
        }
        .onReceive(actions.$mobileConnectionRequestID.dropFirst()) { _ in
            drawer.close()
            showSettings = true
        }
        .onAppear {
            // Cold-launch deep links can be routed before Combine installs the
            // subscriber above; the pending capability is the durable-in-memory
            // latch that closes that race.
            if actions.pendingMobileConnection != nil {
                drawer.close()
                showSettings = true
            }
        }
        .sheet(isPresented: $showAudioNotes) {
            AudioNotesView()
        }
        .fullScreenCover(isPresented: $showNotes) {
            NotesBrowserView()
        }
        .fullScreenCover(isPresented: $showApps) {
            AppsLauncherView()
        }
        .fullScreenCover(isPresented: $showThinkingMap) {
            ThinkingMapView()
        }
    }
}

private struct SideMenuContent: View {
    @ObservedObject private var drawer = DrawerController.shared
    @ObservedObject private var theme = ThemeManager.shared
    @Binding var showSettings: Bool
    @Binding var showAudioNotes: Bool
    @Binding var showNotes: Bool
    @Binding var showApps: Bool
    @Binding var showThinkingMap: Bool

    private var appVersion: String {
        let v = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
        let b = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "?"
        return "v\(v) (\(b))"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Identity / scope header.
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 10) {
                    Image(systemName: "sparkles")
                        .font(.title2)
                        .foregroundColor(theme.accentColor)
                    Text("Magican")
                        .font(.themedBrand(22, weight: .bold))
                }
                Text("\(MagicianAccess.principal) / \(MagicianAccess.workspace)")
                    .font(.footnote)
                    .foregroundColor(theme.secondaryTextColor)
            }
            .padding(.horizontal, 20)
            .padding(.top, 24)
            .padding(.bottom, 20)

            Divider().padding(.horizontal, 16)

            menuRow(icon: "gearshape.fill", title: "Settings") {
                drawer.close()
                // Defer so the drawer close animation finishes before the sheet.
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { showSettings = true }
            }

            menuRow(icon: "waveform.badge.plus", title: "Audio Notes") {
                drawer.close()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                    showAudioNotes = true
                }
            }

            menuRow(icon: "book.pages.fill", title: "Notes") {
                drawer.close()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                    showNotes = true
                }
            }

            menuRow(icon: "square.grid.2x2.fill", title: "Apps") {
                drawer.close()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                    showApps = true
                }
            }

            menuRow(icon: "point.3.connected.trianglepath.dotted", title: "Thinking Map") {
                drawer.close()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                    showThinkingMap = true
                }
            }

            Spacer()

            VStack(alignment: .leading, spacing: 4) {
                Text("About")
                    .font(.footnote.weight(.semibold))
                    .foregroundColor(theme.secondaryTextColor)
                Text(appVersion)
                    .font(.footnote)
                    .foregroundColor(theme.secondaryTextColor)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(20)
        }
    }

    private func menuRow(icon: String, title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 14) {
                Image(systemName: icon)
                    .font(.system(size: 18))
                    .frame(width: 26)
                    .foregroundColor(theme.accentColor)
                Text(title)
                    .font(.body)
                    .foregroundColor(theme.textColor)
                Spacer()
            }
            .contentShape(Rectangle())
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
        }
        .buttonStyle(.plain)
    }
}

private struct AudioNoteItem: Codable, Identifiable, Equatable {
    let noteID: String
    let provider: String
    let usedFallback: Bool
    let capturedAt: String
    let sourceSurface: String
    let transcript: String?
    let durationMS: Int?
    let mimeType: String
    let notePath: String
    let audioPath: String
    let openURL: String?
    let bytes: Int

    var id: String { noteID }

    enum CodingKeys: String, CodingKey {
        case noteID = "note_id"
        case provider
        case usedFallback = "used_fallback"
        case capturedAt = "captured_at"
        case sourceSurface = "source_surface"
        case transcript
        case durationMS = "duration_ms"
        case mimeType = "mime_type"
        case notePath = "note_path"
        case audioPath = "audio_path"
        case openURL = "open_url"
        case bytes
    }
}

private struct AudioNotePage: Codable {
    let items: [AudioNoteItem]
    let offset: Int
    let limit: Int
    let total: Int
    let hasMore: Bool

    enum CodingKeys: String, CodingKey {
        case items, offset, limit, total
        case hasMore = "has_more"
    }
}

struct AudioNotePageRequest: Equatable {
    let offset: Int
    let query: String
    let normalizedQuery: String
    let resetsItems: Bool
}

struct AudioNotePaginationState: Equatable {
    private(set) var nextOffset = 0
    private(set) var loadedQuery = ""

    func request(reset: Bool, query rawQuery: String) -> AudioNotePageRequest {
        let query = rawQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        let normalizedQuery = query.lowercased()
        let resetsItems = reset || normalizedQuery != loadedQuery
        return AudioNotePageRequest(
            offset: resetsItems ? 0 : nextOffset,
            query: query,
            normalizedQuery: normalizedQuery,
            resetsItems: resetsItems
        )
    }

    mutating func accept(_ request: AudioNotePageRequest, pageOffset: Int, pageCount: Int) {
        loadedQuery = request.normalizedQuery
        nextOffset = pageOffset + pageCount
    }

    mutating func removeLoadedItem() {
        nextOffset = max(0, nextOffset - 1)
    }
}

@MainActor
private final class AudioNotesViewModel: ObservableObject {
    @Published private(set) var items: [AudioNoteItem] = []
    @Published private(set) var total = 0
    @Published private(set) var hasMore = false
    @Published private(set) var isLoading = false
    @Published var errorMessage: String?

    private let pageSize = 20
    private var pagination = AudioNotePaginationState()

    func load(reset: Bool = true, query: String = "") async {
        guard !isLoading else { return }
        isLoading = true
        defer { isLoading = false }
        // Editing search text and pressing Load more must not reuse the previous
        // result set's offset, which would silently skip the first page.
        let pageRequest = pagination.request(reset: reset, query: query)
        var components = URLComponents(
            url: MagicianAccess.baseURL.appendingPathComponent("/api/magician/v2/notes/audio"),
            resolvingAgainstBaseURL: false
        )
        var queryItems = [
            URLQueryItem(name: "offset", value: String(pageRequest.offset)),
            URLQueryItem(name: "limit", value: String(pageSize))
        ]
        if !pageRequest.query.isEmpty {
            queryItems.append(URLQueryItem(name: "q", value: pageRequest.query))
        }
        components?.queryItems = queryItems
        guard let url = components?.url else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
                throw URLError(.badServerResponse)
            }
            let page = try JSONDecoder().decode(AudioNotePage.self, from: data)
            if pageRequest.resetsItems {
                items = page.items
            } else {
                let existingIDs = Set(items.map(\.id))
                items.append(contentsOf: page.items.filter { !existingIDs.contains($0.id) })
            }
            pagination.accept(
                pageRequest,
                pageOffset: page.offset,
                pageCount: page.items.count
            )
            total = page.total
            hasMore = page.hasMore
            errorMessage = nil
        } catch {
            errorMessage = "Audio Notes could not be loaded: \(error.localizedDescription)"
        }
    }

    func delete(_ note: AudioNoteItem) async {
        let url = MagicianAccess.baseURL.appendingPathComponent(
            "/api/magician/v2/notes/audio/\(note.noteID)"
        )
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        MagicianAccess.authorize(&request)
        do {
            let (_, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse,
                  http.statusCode == 204 || http.statusCode == 404 else {
                throw URLError(.badServerResponse)
            }
            let removedLoadedItem = items.contains { $0.id == note.id }
            items.removeAll { $0.id == note.id }
            if removedLoadedItem {
                pagination.removeLoadedItem()
                total = max(0, total - 1)
            }
            errorMessage = nil
        } catch {
            errorMessage = "The Audio Note could not be deleted: \(error.localizedDescription)"
        }
    }
}

@MainActor
private final class AudioNotePlayer: NSObject, ObservableObject, AVAudioPlayerDelegate {
    @Published private(set) var loadingID: String?
    @Published private(set) var playingID: String?
    @Published var errorMessage: String?
    private var player: AVAudioPlayer?

    func toggle(_ note: AudioNoteItem) async {
        if playingID == note.id {
            player?.stop()
            player = nil
            playingID = nil
            return
        }
        loadingID = note.id
        defer { loadingID = nil }
        let url = MagicianAccess.baseURL.appendingPathComponent(
            "/api/magician/v2/notes/audio/\(note.noteID)/recording"
        )
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
                throw URLError(.badServerResponse)
            }
            try play(data: data, id: note.id)
            errorMessage = nil
        } catch {
            errorMessage = "The recording could not be played: \(error.localizedDescription)"
        }
    }

    func toggle(_ note: AudioNoteOutboxStatus, queue: AudioNoteUploadQueue) {
        if playingID == note.id {
            player?.stop()
            player = nil
            playingID = nil
            return
        }
        loadingID = note.id
        queue.loadRecording(id: note.id) { [weak self] result in
            guard let self else { return }
            self.loadingID = nil
            do {
                try self.play(data: result.get(), id: note.id)
                self.errorMessage = nil
            } catch {
                self.errorMessage = "The local recording could not be played: \(error.localizedDescription)"
            }
        }
    }

    private func play(data: Data, id: String) throws {
        try AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio)
        try AVAudioSession.sharedInstance().setActive(true)
        let next = try AVAudioPlayer(data: data)
        next.delegate = self
        next.prepareToPlay()
        guard next.play() else { throw URLError(.cannotDecodeContentData) }
        player?.stop()
        player = next
        playingID = id
    }

    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor in
            self.player = nil
            self.playingID = nil
        }
    }
}

private enum AudioNoteDeleteTarget: Identifiable {
    case saved(AudioNoteItem)
    case outbox(AudioNoteOutboxStatus)

    var id: String {
        switch self {
        case .saved(let note): return "saved-\(note.id)"
        case .outbox(let note): return "outbox-\(note.id)"
        }
    }
}

/// Human control surface for both durable server notes and private local
/// outbox records. Pending and failed local recordings remain playable, with
/// explicit retry/discard controls whenever no upload is already in flight.
/// Internal (not private) so Observe → Notes can open it too.
@MainActor
struct AudioNotesView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var theme = ThemeManager.shared
    @ObservedObject private var outbox = AudioNoteUploadQueue.shared
    @StateObject private var model = AudioNotesViewModel()
    @StateObject private var player = AudioNotePlayer()
    @State private var search = ""
    @State private var deleteTarget: AudioNoteDeleteTarget?

    var body: some View {
        NavigationStack {
            List {
                if !outbox.statuses.isEmpty {
                    Section("On this iPhone") {
                        ForEach(outbox.statuses) { item in
                            VStack(alignment: .leading, spacing: 7) {
                                HStack {
                                    Button {
                                        player.toggle(item, queue: outbox)
                                    } label: {
                                        if player.loadingID == item.id {
                                            ProgressView().controlSize(.small)
                                        } else {
                                            Image(systemName: player.playingID == item.id
                                                ? "stop.circle.fill"
                                                : "play.circle.fill")
                                        }
                                    }
                                    .buttonStyle(.borderless)
                                    Label(item.state, systemImage: item.state == "Needs attention"
                                        ? "exclamationmark.triangle.fill"
                                        : "arrow.up.circle")
                                        .font(.themed(13, weight: .semibold))
                                        .foregroundColor(item.state == "Needs attention"
                                            ? theme.warningColor
                                            : theme.accentColor)
                                    Spacer()
                                    if item.state == "Needs attention" {
                                        Button("Retry") { outbox.retry(id: item.id) }
                                            .buttonStyle(.borderless)
                                    }
                                }
                                Text(item.transcript ?? "Audio-only note")
                                    .font(.themed(13))
                                    .foregroundColor(theme.textColor)
                                    .lineLimit(3)
                                if let detail = item.detail {
                                    Text(detail)
                                        .font(.themed(11))
                                        .foregroundColor(theme.secondaryTextColor)
                                }
                            }
                            .swipeActions {
                                if item.canDiscard {
                                    Button(role: .destructive) { deleteTarget = .outbox(item) } label: {
                                        Label("Discard", systemImage: "trash")
                                    }
                                }
                            }
                        }
                    }
                }

                Section("Saved") {
                    if model.items.isEmpty && !model.isLoading {
                        Text("No saved Audio Notes yet.")
                            .foregroundColor(theme.secondaryTextColor)
                    }
                    ForEach(model.items) { note in
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(spacing: 10) {
                                Button {
                                    Task { await player.toggle(note) }
                                } label: {
                                    if player.loadingID == note.id {
                                        ProgressView().controlSize(.small)
                                    } else {
                                        Image(systemName: player.playingID == note.id
                                            ? "stop.circle.fill"
                                            : "play.circle.fill")
                                            .font(.title2)
                                    }
                                }
                                .buttonStyle(.borderless)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(Self.displayDate(note.capturedAt))
                                        .font(.themed(14, weight: .semibold))
                                        .foregroundColor(theme.textColor)
                                    Text(note.provider.replacingOccurrences(of: "_", with: " "))
                                        .font(.themed(11))
                                        .foregroundColor(theme.secondaryTextColor)
                                }
                            }
                            Text(note.transcript ?? "No transcript was available.")
                                .font(.themed(13))
                                .foregroundColor(theme.textColor)
                                .lineLimit(5)
                        }
                        .padding(.vertical, 4)
                        .swipeActions {
                            Button(role: .destructive) { deleteTarget = .saved(note) } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                    if model.hasMore {
                        Button(model.isLoading ? "Loading…" : "Load more") {
                            Task { await model.load(reset: false, query: search) }
                        }
                        .disabled(model.isLoading)
                    }
                }
            }
            .searchable(text: $search, prompt: "Search transcripts")
            .onSubmit(of: .search) { Task { await model.load(query: search) } }
            .refreshable { await model.load(query: search) }
            .overlay {
                if model.isLoading && model.items.isEmpty { ProgressView("Loading Audio Notes…") }
            }
            .navigationTitle("Audio Notes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    Button { Task { await model.load(query: search) } } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .disabled(model.isLoading)
                }
            }
            .task { await model.load() }
            .onChange(of: outbox.statuses) { previous, current in
                let previousIDs = Set(previous.map(\.id))
                let currentIDs = Set(current.map(\.id))
                if !previousIDs.subtracting(currentIDs).isEmpty {
                    Task { await model.load(query: search) }
                }
            }
            .alert("Delete Audio Note?", isPresented: Binding(
                get: { deleteTarget != nil },
                set: { if !$0 { deleteTarget = nil } }
            )) {
                Button("Delete", role: .destructive) {
                    guard let target = deleteTarget else { return }
                    deleteTarget = nil
                    switch target {
                    case .saved(let note): Task { await model.delete(note) }
                    case .outbox(let note): outbox.discard(id: note.id)
                    }
                }
                Button("Cancel", role: .cancel) { deleteTarget = nil }
            } message: {
                Text("This permanently removes the recording and transcript. This cannot be undone.")
            }
            .alert("Audio Notes", isPresented: Binding(
                get: { model.errorMessage != nil || player.errorMessage != nil },
                set: { if !$0 { model.errorMessage = nil; player.errorMessage = nil } }
            )) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(model.errorMessage ?? player.errorMessage ?? "Unknown error")
            }
        }
        .preferredColorScheme(theme.forcedColorScheme)
    }

    private static func displayDate(_ value: String) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = formatter.date(from: value) ?? {
            formatter.formatOptions = [.withInternetDateTime]
            return formatter.date(from: value)
        }()
        guard let date else { return value }
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}
