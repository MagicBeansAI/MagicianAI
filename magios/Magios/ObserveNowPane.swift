import ReplayKit
import SwiftUI

/// Observe → Now: the capture launchpad (Listen · Join as agent · Share screen ·
/// Brainstorm), the owner's upcoming meetings, the /observe app widget slots and
/// the Recent captures list.
struct ObserveNowPane: View {
    @ObservedObject var listen: ListenController
    @ObservedObject var meetings: MeetingsViewModel
    @ObservedObject var recent: ObserveRecentViewModel
    @ObservedObject var deck: ObserveDeckState
    let openThread: (String) -> Void
    let openBrainstorm: () -> Void
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.openURL) private var openURL

    private var inAppActive: Bool { ObserveLiveBlock.isInAppActive(listen.state) }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            launchpad
            upcomingSection
            AppNativeSlotPageRegion(
                page: "/observe",
                regions: ["reviews"],
                accessibilityLabel: "Observe review widgets"
            )
            recentSection
        }
        .onChange(of: inAppActive) {
            // A local capture owns the mic: close the forms that would start another.
            if inAppActive, deck.openTile == .listen || deck.openTile == .broadcast { deck.openTile = nil }
        }
    }

    // MARK: Launchpad

    private var launchpad: some View {
        ObserveCard {
            HStack(spacing: 10) {
                Image(systemName: "play.fill")
                    .font(.system(size: 12, weight: .bold))
                    .foregroundColor(theme.accentColor)
                    .frame(width: 26, height: 26)
                    .background(theme.accentColor.opacity(0.12))
                    .clipShape(RoundedRectangle(cornerRadius: 7))
                VStack(alignment: .leading, spacing: 1) {
                    Text("Capture launchpad")
                        .font(.themedDisplay(17, weight: .semibold))
                        .foregroundColor(theme.textColor)
                    Text("Instant triggers to listen, join as agent, or share screen")
                        .font(.caption)
                        .foregroundColor(theme.secondaryTextColor)
                }
            }
            LazyVGrid(columns: [GridItem(.flexible(), spacing: 8), GridItem(.flexible(), spacing: 8)], spacing: 8) {
                tile(.listen, title: "Listen", meta: inAppActive ? "Capture live" : "Audio & mic capture",
                     icon: "ear.badge.waveform", disabled: inAppActive)
                tile(.join, title: "Join as agent", meta: "Agent attendee", icon: "person.wave.2.fill", disabled: false)
                tile(.broadcast, title: "Share screen",
                     meta: meetings.armedBroadcastSessionId != nil ? "Session ready" : "Screen + audio journal",
                     icon: "rectangle.on.rectangle", disabled: inAppActive)
                tile(.brainstorm, title: "Brainstorm", meta: "Idea space", icon: "waveform.and.magnifyingglass",
                     disabled: false)
            }
            if let err = deck.actionError, deck.openTile == .broadcast {
                Text(err).font(.caption).foregroundColor(theme.dangerColor)
            }
            switch deck.openTile {
            case .listen where !inAppActive: listenForm
            case .join: joinForm
            case .broadcast where !inAppActive: broadcastForm
            default: EmptyView()
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("observe-launchpad")
    }

    private func tile(_ t: ObserveDeckState.Tile, title: String, meta: String, icon: String, disabled: Bool) -> some View {
        let open = deck.openTile == t
        return Button {
            if t == .brainstorm { openBrainstorm() } else { deck.toggle(t) }
        } label: {
            HStack(spacing: 9) {
                Image(systemName: icon)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundColor(open ? theme.onAccentColor : theme.accentColor)
                    .frame(width: 30, height: 30)
                    .background(open ? theme.accentColor : theme.accentColor.opacity(0.12))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.subheadline.weight(.semibold))
                        .foregroundColor(theme.textColor)
                        .lineLimit(1)
                        .minimumScaleFactor(0.85)
                    Text(meta)
                        .font(.caption2)
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(1)
                        .minimumScaleFactor(0.85)
                }
                Spacer(minLength: 0)
            }
            .padding(9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(open ? theme.accentColor.opacity(0.08) : theme.controlColor.opacity(0.55))
            .clipShape(RoundedRectangle(cornerRadius: 11))
            .overlay {
                RoundedRectangle(cornerRadius: 11)
                    .stroke(open ? theme.accentColor : theme.cardBorderColor, lineWidth: 1)
            }
            .opacity(disabled ? 0.5 : 1)
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .accessibilityLabel("\(title), \(meta)")
        .accessibilityHint(t == .brainstorm ? "Opens the voice-first branching idea space" : "Shows the \(title) form")
        .accessibilityAddTraits(open ? .isSelected : [])
        .accessibilityIdentifier("observe-tile-\(t.rawValue)")
    }

    /// Listen → "Listen to this room" (no calendar event needed).
    private var listenForm: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Listen to this room", systemImage: "ear.badge.waveform")
                .font(.headline)
            Text("No calendar event? Put your phone on the table and Magican transcribes the room into a meeting thread — no Mac needed.")
                .font(.subheadline).foregroundColor(theme.secondaryTextColor)
            TextField("Title (optional)", text: $deck.meetingTitle)
                .textFieldStyle(.roundedBorder)
            Button {
                Task {
                    await deck.startListen(
                        title: ObserveDeckState.trimmedOrNil(deck.meetingTitle),
                        url: nil, listen: listen, meetings: meetings
                    )
                }
            } label: {
                Label("Listen here", systemImage: "mic.fill").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .foregroundColor(theme.onAccentColor)
            .disabled(deck.actionBusy)
            Text("Captures the whole room. A meeting app's mute doesn't stop it, and nearby conversations are transcribed too.")
                .font(.caption).foregroundColor(theme.secondaryTextColor)
        }
        .padding(.top, 4)
    }

    /// Join as agent → "Join a Google Meet as the bot".
    private var joinForm: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Join a Google Meet as the bot", systemImage: "person.wave.2.fill")
                .font(.headline)
            Text("Send the agent to attend a call directly (Google Meet) and take notes.")
                .font(.subheadline).foregroundColor(theme.secondaryTextColor)
            TextField("meet.google.com/…", text: $deck.meetUrl)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
            if let joinError = deck.joinError {
                Text(joinError).font(.caption).foregroundColor(theme.dangerColor)
            }
            let empty = deck.meetUrl.trimmingCharacters(in: .whitespaces).isEmpty
            HStack(spacing: 10) {
                Button {
                    // Join it yourself.
                    let u = deck.meetUrl.trimmingCharacters(in: .whitespaces)
                    if let url = URL(string: u), !u.isEmpty { openURL(url) }
                } label: {
                    Label("Join (Me)", systemImage: "video.fill").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .disabled(empty)
                Button {
                    Task { await deck.joinAsBot(meetings: meetings, open: openThread) }
                } label: {
                    Label(deck.joining ? "Sending…" : "Send bot", systemImage: "arrow.right.circle.fill")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .foregroundColor(theme.onAccentColor)
                .disabled(deck.joining || empty)
            }
        }
        .padding(.top, 4)
    }

    /// Share screen → dual-source capture via the ReplayKit broadcast extension:
    /// system/app audio → the diarized room track, your mic → the "You" track,
    /// and screen keyframes into the same meeting thread.
    private var broadcastForm: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Share screen, system audio + my mic", systemImage: "waveform.badge.plus")
                .font(.headline)
            Text("Capture the meeting's audio (what you hear) as the room track, your microphone as a separate \"You\" track, AND your screen — slides and shared docs get noted into the same meeting thread. Uses an iOS screen broadcast.")
                .font(.subheadline).foregroundColor(theme.secondaryTextColor)
            if meetings.armedBroadcastSessionId != nil {
                Text("Session ready — tap below, turn Microphone on, and Start Broadcast. If you don't start within ~10 minutes, prepare again.")
                    .font(.caption).foregroundColor(theme.secondaryTextColor)
                BroadcastPicker()
                    .frame(height: 52)
                    .frame(maxWidth: .infinity)
            } else {
                TextField("Session title (optional)", text: $deck.broadcastTitle)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("observe-broadcast-title")
                Button {
                    Task { await deck.prepareBroadcast(meetings: meetings) }
                } label: {
                    Label("Prepare session", systemImage: "bolt.fill").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .foregroundColor(theme.onAccentColor)
                .disabled(deck.actionBusy)
            }
            Text("Doesn't capture protected call audio (FaceTime / VoIP), by iOS design.")
                .font(.caption).foregroundColor(theme.secondaryTextColor)
        }
        .padding(.top, 4)
    }

    // MARK: Upcoming

    private var upcomingSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            ObserveSectionHeader(title: "Upcoming") {
                if meetings.loading { ProgressView().scaleEffect(0.8) }
                Button { Task { await meetings.refresh(force: true) } } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .disabled(meetings.loading)
                .accessibilityLabel("Refresh upcoming meetings")
            }
            if let err = deck.actionError, deck.openTile != .broadcast {
                ObserveBanner(text: err, color: theme.dangerColor)
            }
            if let err = meetings.upcomingError {
                ObserveBanner(text: err, color: theme.dangerColor) {
                    Task { await meetings.refresh(force: true) }
                }
            }
            if !meetings.upcomingLoaded {
                HStack(spacing: 8) {
                    ProgressView().scaleEffect(0.8)
                    Text("Loading your calendar…")
                        .font(.subheadline).foregroundColor(theme.secondaryTextColor)
                }
                .accessibilityIdentifier("observe-upcoming-loading")
            } else if meetings.upcoming.isEmpty {
                if meetings.upcomingError == nil {
                    Text("No meetings on your calendar in the next few hours.")
                        .font(.subheadline).foregroundColor(theme.secondaryTextColor)
                }
            } else {
                ForEach(meetings.upcoming) { upcomingRow($0) }
            }
            if !meetings.upcomingErrors.isEmpty {
                Text("Some calendars couldn't be read — check your Google sign-in.")
                    .font(.caption).foregroundColor(theme.warningColor)
            }
        }
    }

    private func upcomingRow(_ ev: UpcomingMeeting) -> some View {
        ObserveCard(spacing: 10) {
            HStack(alignment: .top, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(ev.title).font(.subheadline.weight(.semibold)).lineLimit(2)
                    HStack(spacing: 6) {
                        Text(Self.windowText(ev)).font(.caption).foregroundColor(theme.secondaryTextColor)
                        if let a = ev.account {
                            Text("· \(a)").font(.caption).foregroundColor(theme.secondaryTextColor).lineLimit(1)
                        }
                    }
                }
                Spacer()
                if ev.liveNow {
                    Text("now")
                        .font(.caption2.weight(.bold))
                        .padding(.horizontal, 7).padding(.vertical, 3)
                        .background(theme.dangerColor.opacity(0.14))
                        .foregroundColor(theme.dangerColor)
                        .clipShape(Capsule())
                }
            }
            if let active = activeSession(for: ev) {
                HStack(spacing: 8) {
                    Image(systemName: active.isBot ? "person.wave.2.fill" : "ear.badge.waveform")
                        .foregroundColor(theme.accentColor)
                    Text(active.isBot ? "Bot joined" : "Listening now")
                        .font(.caption).foregroundColor(theme.secondaryTextColor)
                    Spacer()
                    if let t = active.threadId {
                        Button("Open") { openThread(t) }
                            .font(.caption).buttonStyle(.bordered).controlSize(.small)
                    }
                }
            } else {
                upcomingActions(ev)
            }
        }
    }

    private func upcomingActions(_ ev: UpcomingMeeting) -> some View {
        HStack(spacing: 8) {
            if ev.isJoinable {
                // Join it yourself, as a human — just open the meeting link.
                Button {
                    if let u = ev.meetUrl, let url = URL(string: u) { openURL(url) }
                } label: {
                    Label("Join (Me)", systemImage: "video.fill")
                }
                .buttonStyle(.borderedProminent).controlSize(.small)
                .foregroundColor(theme.onAccentColor)
            }
            Button {
                Task { await deck.startListen(title: ev.title, url: ev.meetUrl, listen: listen, meetings: meetings) }
            } label: {
                Label("Listen", systemImage: "ear.badge.waveform")
            }
            .buttonStyle(.bordered).controlSize(.small)
            .disabled(deck.actionBusy || inAppActive)
            if ev.isJoinable {
                Button {
                    Task { await deck.sendBot(ev, meetings: meetings, open: openThread) }
                } label: {
                    Label("Send bot", systemImage: "person.wave.2.fill")
                }
                .buttonStyle(.bordered).controlSize(.small)
                .disabled(deck.actionBusy)
            }
        }
    }

    private func activeSession(for ev: UpcomingMeeting) -> ActiveMeeting? {
        guard let mu = ev.meetUrl?.trimmingCharacters(in: .whitespaces).lowercased(), !mu.isEmpty else {
            return nil
        }
        return meetings.active.first {
            ($0.url ?? "").trimmingCharacters(in: .whitespaces).lowercased() == mu
        }
    }

    static func windowText(_ ev: UpcomingMeeting) -> String {
        guard let s = ev.start else { return "" }
        let cal = Calendar.current
        let time = DateFormatter(); time.timeStyle = .short; time.dateStyle = .none
        var dayPrefix = ""
        if cal.isDateInToday(s) {
            dayPrefix = ""
        } else if cal.isDateInTomorrow(s) {
            dayPrefix = "Tomorrow "
        } else {
            let d = DateFormatter(); d.dateFormat = "EEE d MMM"; dayPrefix = d.string(from: s) + " "
        }
        if let e = ev.end {
            return "\(dayPrefix)\(time.string(from: s))–\(time.string(from: e))"
        }
        return "\(dayPrefix)\(time.string(from: s))"
    }

    // MARK: Recent

    private var recentSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            ObserveSectionHeader(title: "Recent") {
                if recent.block.loading { ProgressView().scaleEffect(0.8) }
            }
            if let err = recent.block.error {
                ObserveBanner(text: err, color: theme.dangerColor) { Task { await recent.reload() } }
            }
            if let rows = recent.block.value {
                if rows.isEmpty {
                    Text("Nothing captured yet")
                        .font(.subheadline).foregroundColor(theme.secondaryTextColor)
                } else {
                    VStack(spacing: 0) {
                        ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                            if index > 0 { Divider().padding(.leading, 44) }
                            recentRow(row)
                        }
                    }
                    .background(theme.cardColor)
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                    .overlay { RoundedRectangle(cornerRadius: 14).stroke(theme.cardBorderColor, lineWidth: 1) }
                }
            } else if !recent.block.settled {
                Text("Loading recent captures…")
                    .font(.subheadline).foregroundColor(theme.secondaryTextColor)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("observe-recent")
    }

    private func recentRow(_ row: RecentCapture) -> some View {
        Button { openThread(row.threadId) } label: {
            HStack(spacing: 10) {
                Image(systemName: row.kind == .meeting ? "person.2.wave.2" : "rectangle.on.rectangle")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundColor(row.kind == .meeting ? theme.successColor : theme.infoColor)
                    .frame(width: 26, height: 26)
                    .background((row.kind == .meeting ? theme.successColor : theme.infoColor).opacity(0.12))
                    .clipShape(RoundedRectangle(cornerRadius: 7))
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.title)
                        .font(.subheadline.weight(.medium))
                        .foregroundColor(theme.textColor)
                        .lineLimit(1)
                    Text([row.kind == .meeting ? "Meeting" : "Screen observation", ObserveTime.stamp(row.updatedAt)]
                        .filter { !$0.isEmpty }.joined(separator: " · "))
                        .font(.caption)
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.bold))
                    .foregroundColor(theme.secondaryTextColor)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// Presents the iOS system broadcast picker, pre-targeting our upload extension
/// and offering the microphone toggle (the extension captures `.audioMic` only
/// when the user enables it).
private struct BroadcastPicker: UIViewRepresentable {
    func makeUIView(context: Context) -> RPSystemBroadcastPickerView {
        let picker = RPSystemBroadcastPickerView(frame: CGRect(x: 0, y: 0, width: 220, height: 52))
        picker.preferredExtension = "com.magicbeans100x.magican.Broadcast"
        picker.showsMicrophoneButton = true
        return picker
    }

    func updateUIView(_ uiView: RPSystemBroadcastPickerView, context: Context) {}
}
