import Foundation
import SwiftUI

/// The Observe tab — the iOS "Command Deck" (port of the web /observe console).
/// Top to bottom:
///  - **Command header** — one row: live status line + Refresh (the title is
///    the navigation bar's "Observe").
///  - **KPI grid** — Now & Live · Sources on · Audio Profiles · Notes & Recents;
///    the cards are the only view switchers (remembered per device, settable via
///    `magican://observe?pane=now|sources|audio|notes`).
///  - **Live** — every live / starting capture, above whichever view is shown
///    (`ObserveLiveBlock`: in-app hero with waveform, timer, summary, live
///    transcript, pause/resume, Stop; other server sessions; meeting-ended card).
///  - **Views** — Now (`ObserveNowPane`: capture launchpad, upcoming, widget
///    slots, recent), Sources (`ObserveSourcesPane`), Audio
///    (`ObserveAudioPane`), Notes (audio notes + Published Notes).
/// Live transcript + summaries land in a meeting thread openable via
/// `magican://thread/{id}`.
struct ObserveView: View {
    @ObservedObject private var listen = ListenController.shared
    @StateObject private var meetings = MeetingsViewModel()
    @StateObject private var transcript = MeetingTranscriptViewModel()
    @StateObject private var publishedNotes = PublishedTaskNotesViewModel()
    @StateObject private var recent = ObserveRecentViewModel()
    @StateObject private var sources = ObserveSourcesViewModel()
    @StateObject private var audio = ObserveAudioProfilesViewModel()
    @StateObject private var device = ObserveDeviceStatus()
    @StateObject private var deck = ObserveDeckState()
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.openURL) private var openURL
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @AppStorage(ObservePane.storageKey) private var paneRaw = ObservePane.now.rawValue
    @State private var showThinkingMap = false
    @State private var refreshing = false

    private var pane: ObservePane { ObservePane(rawValue: paneRaw) ?? .now }

    private var metrics: ObserveDeckMetrics {
        ObserveDeckMetrics(
            activeSessionIds: meetings.active.map(\.sessionId),
            inAppSessionId: listen.activeSessionId,
            inAppActive: ObserveLiveBlock.isInAppActive(listen.state),
            liveMeetingCount: meetings.upcoming.filter(\.liveNow).count,
            sourcesOn: sources.enabledCount,
            audioSurfaces: audio.configuredSurfaces,
            recentCount: recent.block.value?.count,
            publishedNotesTotal: recent.block.settled && recent.block.value == nil ? publishedNotes.total : nil
        )
    }

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ObserveCommandHeader(
                        statusLine: metrics.statusLine,
                        live: metrics.anyActive,
                        refreshing: refreshing
                    ) { Task { await refreshAll() } }
                    ObserveKPIGrid(metrics: metrics, selected: pane) { next in
                        if reduceMotion { paneRaw = next.rawValue } else {
                            withAnimation(.easeInOut(duration: 0.18)) { paneRaw = next.rawValue }
                        }
                    }
                    ObserveLiveBlock(
                        listen: listen,
                        meetings: meetings,
                        transcript: transcript,
                        deck: deck,
                        openThread: openThread
                    )
                    paneContent
                }
                .padding(.horizontal, 16)
                .padding(.top, 4)
                .padding(.bottom, 96)
            }
            .refreshable { await refreshAll() }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Observe")
            .navigationBarTitleDisplayMode(.inline)
            .hamburgerToolbar()
            .fullScreenCover(isPresented: $showThinkingMap) {
                ThinkingMapView()
            }
            .sheet(isPresented: $deck.showAudioNotes) {
                AudioNotesView()
            }
            .task {
                await meetings.refresh()
                meetings.start()
                // Reconnect to a session armed before the app was backgrounded.
                // A broadcast arm is the extension's capture: keep it shown as
                // prepared, never start the in-app mic for it.
                if let broadcast = await listen.reattachIfArmed(activeSessions: meetings.active) {
                    meetings.adoptArmedBroadcast(sessionId: broadcast)
                }
                // Resume the transcript if a session is already live on appear.
                if case .listening(let t) = listen.state { transcript.start(thread: t) }
                else if case .paused(let t) = listen.state { transcript.start(thread: t) }
                await loadDeck()
            }
            .onDisappear { meetings.stop(); transcript.stop() }
            // Feed the in-app session's rolling summary to the Live Activity.
            .onChange(of: meetings.active) {
                listen.reconcileServerPresence(activeSessions: meetings.active)
                if let summary = inAppSummary { ObservationActivity.shared.updateSummary(summary) }
            }
            // Stream the live transcript into the cockpit while a session is live.
            .onChange(of: listen.state) {
                if case .listening(let thread) = listen.state {
                    transcript.start(thread: thread)
                } else if case .paused(let thread) = listen.state {
                    transcript.start(thread: thread)
                } else {
                    transcript.stop()
                }
            }
            // Permission switches flipped in Settings show up on return.
            .onChange(of: scenePhase) {
                if scenePhase == .active { Task { await device.refresh() } }
            }
        }
    }

    @ViewBuilder
    private var paneContent: some View {
        switch pane {
        case .now:
            ObserveNowPane(
                listen: listen,
                meetings: meetings,
                recent: recent,
                deck: deck,
                openThread: openThread,
                openBrainstorm: { showThinkingMap = true }
            )
        case .sources:
            ObserveSourcesPane(sources: sources, device: device)
        case .audio:
            ObserveAudioPane(audio: audio)
        case .notes:
            ObserveNotesPane(publishedNotes: publishedNotes, deck: deck)
        }
    }

    private var inAppSummary: String? {
        guard let sid = listen.activeSessionId else { return nil }
        return meetings.active.first { $0.sessionId == sid }?.latestSummary
    }

    private func openThread(_ thread: String) {
        if let url = URL(string: "magican://thread/\(thread)") { openURL(url) }
    }

    /// First load of everything the KPI cards count (sources, audio, recent).
    private func loadDeck() async {
        async let s: Void = sources.reloadAll()
        async let a: Void = audio.reload()
        async let r: Void = recent.reload()
        async let d: Void = device.refresh()
        _ = await (s, a, r, d)
        // Notes KPI falls back to the published-notes total when Recent failed.
        if recent.block.value == nil { await publishedNotes.loadIfNeeded() }
    }

    /// The header Refresh / pull-to-refresh: reload every deck input, bypassing
    /// the server's calendar cache.
    private func refreshAll() async {
        guard !refreshing else { return }
        refreshing = true
        defer { refreshing = false }
        async let m: Void = meetings.refresh(force: true)
        async let n: Void = publishedNotes.reload()
        async let rest: Void = loadDeck()
        _ = await (m, n, rest)
    }
}

// MARK: - Deck UI state

/// Form + busy state shared by the launchpad, the upcoming rows and the live
/// block (e.g. "Listen to another" clears the Listen title).
@MainActor
final class ObserveDeckState: ObservableObject {
    enum Tile: String, CaseIterable, Identifiable {
        case listen, join, broadcast, brainstorm
        var id: String { rawValue }
    }

    /// The launchpad tile whose form is open (one at a time).
    @Published var openTile: Tile?
    /// "Listen to this room" title.
    @Published var meetingTitle = ""
    /// "Share screen" (broadcast) title — deliberately separate from Listen's.
    @Published var broadcastTitle = ""
    @Published var meetUrl = ""
    @Published var joining = false
    @Published var joinError: String?
    @Published var actionBusy = false
    /// Failure of an action with no form of its own (upcoming "Send bot",
    /// "Prepare session").
    @Published var actionError: String?
    @Published var showAudioNotes = false

    func toggle(_ tile: Tile) {
        openTile = openTile == tile ? nil : tile
    }

    static func trimmedOrNil(_ s: String) -> String? {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? nil : t
    }

    func startListen(title: String?, url: String?, listen: ListenController, meetings: MeetingsViewModel) async {
        actionBusy = true
        defer { actionBusy = false }
        await listen.start(title: title, url: url)
        await meetings.refreshActive()
    }

    func sendBot(_ ev: UpcomingMeeting, meetings: MeetingsViewModel, open: (String) -> Void) async {
        guard let u = ev.meetUrl else { return }
        actionBusy = true
        actionError = nil
        defer { actionBusy = false }
        if let thread = await meetings.sendBot(url: u, title: ev.title) {
            open(thread)
        } else {
            actionError = "Couldn't send the bot to \u{201C}\(ev.title)\u{201D}. Check your connection and try again."
        }
    }

    func joinAsBot(meetings: MeetingsViewModel, open: (String) -> Void) async {
        let url = meetUrl.trimmingCharacters(in: .whitespaces)
        guard !url.isEmpty else { return }
        joining = true
        joinError = nil
        defer { joining = false }
        if let thread = await meetings.sendBot(url: url, title: nil) {
            meetUrl = ""
            open(thread)
        } else {
            joinError = "Couldn't send the bot. Check the link and your connection."
        }
    }

    func prepareBroadcast(meetings: MeetingsViewModel) async {
        actionBusy = true
        actionError = nil
        defer { actionBusy = false }
        if !(await meetings.armBroadcast(title: Self.trimmedOrNil(broadcastTitle))) {
            actionError = "Couldn't prepare the screen share session. Check your connection and try again."
        }
    }
}

// MARK: - Notes view

/// Notes & Recents → Notes: an Audio Notes entry plus the full Published Notes
/// section (search, page size, paging, Publish next 25, Promote to memory,
/// Open in Notes).
private struct ObserveNotesPane: View {
    @ObservedObject var publishedNotes: PublishedTaskNotesViewModel
    @ObservedObject var deck: ObserveDeckState
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Button { deck.showAudioNotes = true } label: {
                HStack(spacing: 12) {
                    Image(systemName: "waveform.badge.plus")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundColor(theme.warningColor)
                        .frame(width: 34, height: 34)
                        .background(theme.warningColor.opacity(0.14))
                        .clipShape(RoundedRectangle(cornerRadius: 9))
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Audio notes")
                            .font(.subheadline.weight(.semibold))
                            .foregroundColor(theme.textColor)
                        Text("Voice memos recorded on this phone and their transcripts")
                            .font(.caption)
                            .foregroundColor(theme.secondaryTextColor)
                            .multilineTextAlignment(.leading)
                    }
                    Spacer(minLength: 4)
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.bold))
                        .foregroundColor(theme.secondaryTextColor)
                }
                .padding(14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(theme.cardColor)
                .clipShape(RoundedRectangle(cornerRadius: 14))
                .overlay {
                    RoundedRectangle(cornerRadius: 14).stroke(theme.cardBorderColor, lineWidth: 1)
                }
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("observe-audio-notes")

            PublishedTaskNotesSection(model: publishedNotes)
        }
    }
}
