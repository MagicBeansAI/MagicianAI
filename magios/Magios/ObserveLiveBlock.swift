import SwiftUI

/// The deck's "LIVE" block — shown above every view whenever something is live
/// or starting: this phone's in-app capture (starting card, the listening hero
/// with waveform + timer + rolling summary + live transcript, or the paused card
/// with Resume), every other server-side session (bot attendee, host listener,
/// broadcast) with Open transcript + Stop, the Listen error banner, and the
/// meeting-ended card with "Listen to another".
struct ObserveLiveBlock: View {
    @ObservedObject var listen: ListenController
    @ObservedObject var meetings: MeetingsViewModel
    @ObservedObject var transcript: MeetingTranscriptViewModel
    @ObservedObject var deck: ObserveDeckState
    let openThread: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    static func isInAppActive(_ state: ListenController.State) -> Bool {
        switch state {
        case .starting, .listening, .paused: return true
        default: return false
        }
    }

    private var inAppActive: Bool { Self.isInAppActive(listen.state) }

    /// Server-side live sessions other than this phone's in-app one (deduped by
    /// session id so the in-app session isn't shown twice).
    private var otherOngoing: [ActiveMeeting] {
        meetings.active.filter { $0.sessionId != listen.activeSessionId }
    }

    private var hasLive: Bool { inAppActive || !otherOngoing.isEmpty }

    private var inAppSummary: String? {
        guard let sid = listen.activeSessionId else { return nil }
        return meetings.active.first { $0.sessionId == sid }?.latestSummary
    }

    var body: some View {
        let ended: (thread: String?, reason: String)? = {
            if case .ended(let t, let r) = listen.state { return (t, r) }
            return nil
        }()
        let errorText: String? = {
            if case .error(let m) = listen.state { return m }
            return nil
        }()
        if hasLive || ended != nil || errorText != nil || meetings.activeError != nil {
            VStack(alignment: .leading, spacing: 10) {
                if hasLive {
                    ObserveSectionHeader(title: "Live", color: theme.dangerColor) {
                        Text("\(otherOngoing.count + (inAppActive ? 1 : 0))")
                            .font(.themedMono(12, weight: .semibold))
                            .foregroundColor(theme.dangerColor)
                    }
                }
                if let errorText {
                    ObserveBanner(text: errorText, color: theme.dangerColor)
                }
                if let activeError = meetings.activeError {
                    ObserveBanner(text: activeError, color: theme.warningColor) {
                        Task { await meetings.refreshActive() }
                    }
                }
                if let ended { endedCard(thread: ended.thread, reason: ended.reason) }
                switch listen.state {
                case .starting:
                    statusCard(dotColor: theme.warningColor, title: "Starting…", detail: nil)
                case .listening(let thread):
                    activeHero(thread: thread)
                case .paused(let thread):
                    pausedCard(thread: thread)
                default:
                    EmptyView()
                }
                ForEach(otherOngoing) { ongoingServerCard($0) }
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("observe-live-block")
        }
    }

    // MARK: cards

    private func ongoingServerCard(_ m: ActiveMeeting) -> some View {
        ObserveCard {
            HStack(spacing: 10) {
                Circle().fill(m.paused ? theme.warningColor : theme.dangerColor).frame(width: 10, height: 10)
                Text(m.title ?? (m.isBot ? "Agent in a meeting" : "Listening"))
                    .font(.headline).lineLimit(1)
                Spacer()
                Text(sessionKindLabel(m))
                    .font(.caption).foregroundColor(theme.secondaryTextColor)
            }
            if let s = m.latestSummary, !s.isEmpty {
                Text(s).font(.subheadline).foregroundColor(theme.secondaryTextColor).lineLimit(3)
            }
            if let t = m.threadId { openThreadButton(t) }
            Button(role: .destructive) {
                Task { deck.actionBusy = true; await meetings.stopServer(m.sessionId); deck.actionBusy = false }
            } label: {
                Label("Stop", systemImage: "stop.fill").frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .disabled(deck.actionBusy)
        }
    }

    private func sessionKindLabel(_ m: ActiveMeeting) -> String {
        if m.isBot { return "Bot" }
        if m.sessionId == meetings.armedBroadcastSessionId { return "Screen share" }
        return "Listener"
    }

    /// The live "listening" hero: a mic-driven waveform + big elapsed timer +
    /// rolling summary + live transcript.
    private func activeHero(thread: String) -> some View {
        ObserveCard {
            HStack(spacing: 14) {
                WaveformView(level: CGFloat(listen.audioLevel), color: theme.dangerColor, animated: !reduceMotion)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 8) {
                        Circle().fill(theme.dangerColor).frame(width: 10, height: 10)
                        Text("Listening").font(.headline)
                        if listen.reused {
                            Text("· Reconnected").font(.caption).foregroundColor(theme.secondaryTextColor)
                        }
                    }
                    if let started = listen.startedAt {
                        Text(started, style: .timer)
                            .font(.themedMono(.title3))
                            .foregroundColor(theme.secondaryTextColor)
                            .monospacedDigit()
                    }
                }
                Spacer()
            }
            if let s = inAppSummary, !s.isEmpty {
                Text(s).font(.subheadline).foregroundColor(theme.secondaryTextColor).lineLimit(4)
            } else if transcript.lines.isEmpty {
                Text("Recording the room — the transcript and notes are building in the meeting thread.")
                    .font(.subheadline).foregroundColor(theme.secondaryTextColor)
            }
            if !transcript.lines.isEmpty { transcriptPanel }
            openThreadButton(thread)
            stopInAppButton
        }
    }

    /// Inline live transcript — the streamed meeting lines, "You" highlighted,
    /// auto-scrolling to the newest.
    private var transcriptPanel: some View {
        VStack(alignment: .leading, spacing: 6) {
            ObserveSectionHeader("Live transcript")
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        ForEach(transcript.lines) { transcriptLine($0).id($0.id) }
                        Color.clear.frame(height: 1).id("transcript-bottom")
                    }
                    .padding(.vertical, 2)
                }
                .frame(maxHeight: 220)
                .onChange(of: transcript.lines.count) {
                    if reduceMotion {
                        proxy.scrollTo("transcript-bottom", anchor: .bottom)
                    } else {
                        withAnimation(.easeOut(duration: 0.2)) {
                            proxy.scrollTo("transcript-bottom", anchor: .bottom)
                        }
                    }
                }
            }
        }
    }

    private func transcriptLine(_ line: TranscriptLine) -> some View {
        HStack(alignment: .top, spacing: 8) {
            if let speaker = line.speaker {
                Text(speaker)
                    .font(.caption.weight(.semibold))
                    .foregroundColor(line.isYou ? theme.accentColor : .secondary)
                    .frame(width: 62, alignment: .leading)
                    .lineLimit(1)
            }
            Text(line.text)
                .font(.subheadline)
                .foregroundColor(line.isYou ? theme.textColor : .secondary)
            Spacer(minLength: 0)
        }
    }

    private func pausedCard(thread: String) -> some View {
        ObserveCard {
            HStack(spacing: 10) {
                Circle().fill(theme.warningColor).frame(width: 12, height: 12)
                Text("Paused (interrupted)").font(.headline)
                Spacer()
            }
            Text("Capture paused — a call or another app took the microphone.")
                .font(.subheadline).foregroundColor(theme.secondaryTextColor)
            Button("Resume") { listen.resume() }
                .buttonStyle(.borderedProminent)
                .foregroundColor(theme.onAccentColor)
            if !transcript.lines.isEmpty { transcriptPanel }
            openThreadButton(thread)
            stopInAppButton
        }
    }

    private var stopInAppButton: some View {
        Button(role: .destructive) {
            Task {
                await listen.stop()
                await meetings.refreshActive()
            }
        } label: {
            Label("Stop", systemImage: "stop.fill").frame(maxWidth: .infinity)
        }
        .buttonStyle(.bordered)
    }

    private func endedCard(thread: String?, reason: String) -> some View {
        ObserveCard {
            Label("Meeting ended", systemImage: "checkmark.circle.fill").font(.headline)
            Text(reason == "ended_by_server"
                ? "The meeting session ended. Its summary is in the thread."
                : "Stopped. The final summary is in the thread.")
                .font(.subheadline).foregroundColor(theme.secondaryTextColor)
            if let thread { openThreadButton(thread) }
            Button("Listen to another") {
                deck.meetingTitle = ""
                listen.reset()
                deck.openTile = .listen
            }
            .buttonStyle(.bordered)
        }
    }

    private func statusCard(dotColor: Color, title: String, detail: String?) -> some View {
        ObserveCard {
            HStack(spacing: 10) {
                Circle().fill(dotColor).frame(width: 12, height: 12)
                Text(title).font(.headline)
                Spacer()
                ProgressView().scaleEffect(0.8)
            }
            if let detail { Text(detail).font(.subheadline).foregroundColor(theme.secondaryTextColor) }
        }
    }

    private func openThreadButton(_ thread: String) -> some View {
        Button { openThread(thread) } label: {
            Label("Open transcript", systemImage: "text.bubble").frame(maxWidth: .infinity)
        }
        .buttonStyle(.bordered)
    }
}

/// A compact VU-style waveform: five capsules whose heights track the live mic
/// level (center-weighted), animated so it feels alive without a timer/random.
struct WaveformView: View {
    var level: CGFloat   // 0…1
    var color: Color
    var animated = true

    private let multipliers: [CGFloat] = [0.45, 0.8, 1.0, 0.7, 0.5]

    var body: some View {
        HStack(spacing: 4) {
            ForEach(multipliers.indices, id: \.self) { i in
                Capsule()
                    .fill(color)
                    .frame(width: 5, height: max(6, 42 * level * multipliers[i]))
            }
        }
        .frame(width: 45, height: 46, alignment: .center)
        .animation(animated ? .easeOut(duration: 0.12) : nil, value: level)
        .accessibilityHidden(true)
    }
}
