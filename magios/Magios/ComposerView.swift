import SwiftUI

enum ComposerMode: Equatable {
    case ask
    case acceptInScope
    case plan

    /// Wire value for `POST /chat/sessions/{id}/messages/stream`. Ask is omitted.
    var wire: String? {
        switch self {
        case .ask: return nil
        case .acceptInScope: return "accept_in_scope"
        case .plan: return "plan"
        }
    }
}

/// The permission posture carried by Do. It is separate from the active mode
/// so Plan can temporarily take over without forgetting whether Do should ask
/// before each edit or accept edits inside the workspace.
enum ComposerDoPermission: CaseIterable, Equatable {
    case ask
    case acceptInScope

    init?(mode: ComposerMode) {
        switch mode {
        case .ask: self = .ask
        case .acceptInScope: self = .acceptInScope
        case .plan: return nil
        }
    }

    var mode: ComposerMode {
        switch self {
        case .ask: return .ask
        case .acceptInScope: return .acceptInScope
        }
    }

    var label: String {
        switch self {
        case .ask: return "Ask"
        case .acceptInScope: return "Accept"
        }
    }

    var guidance: String {
        switch self {
        case .ask:
            return "Prompt before each file edit"
        case .acceptInScope:
            return "In-scope file edits, no prompt. Anything outside the workspace still asks."
        }
    }
}

private enum VoiceSettingsSection: String, CaseIterable, Identifiable {
    case replies
    case dictation
    case session

    var id: String { rawValue }

    var title: String {
        switch self {
        case .replies: return "Replies"
        case .dictation: return "Dictation"
        case .session: return "Live call"
        }
    }

    var icon: String {
        switch self {
        case .replies: return "speaker.wave.2"
        case .dictation: return "mic"
        case .session: return "dot.radiowaves.left.and.right"
        }
    }
}

/// Unified composer card (styled after the web MobileComposer): one rounded
/// bordered surface holding the (larger) multi-line input on top, a row of
/// staged-attachment preview chips, and a bottom bar with the tools on the left
/// and the Send button on the right — all inside the card.
struct ComposerView: View {
    static let cornerRadius: CGFloat = 12

    @Binding var text: String
    @Binding var mode: ComposerMode
    @Binding var doPermission: ComposerDoPermission
    var isThinking: Bool
    var profiles: [ChatProfile]
    @Binding var selectedProfile: String
    var chatHarnesses: [ChatHarnessOption]
    @Binding var selectedHarnessEngine: String
    @Binding var selectedHarnessModel: String
    var onSend: () -> Void
    var onStop: () -> Void
    var onAttach: () -> Void
    var onCamera: () -> Void
    var mentionItems: [ComposerMentionItem] = []
    var stagedAttachments: [StagedAttachment] = []
    var onRemoveAttachment: (UUID) -> Void = { _ in }
    var isRecording: Bool = false
    var isTranscribing: Bool = false
    /// Tap dictation has explicit start/finish callbacks. The gesture decides
    /// from the recording state captured at touch-down, so a stop tap cannot be
    /// reinterpreted as a fresh start while SwiftUI publishes state changes.
    var onTapDictationStart: () -> Void = {}
    var onTapDictationStop: () -> Void = {}
    /// Live (in-progress) dictation text shown above the big mic while recording.
    var partialTranscript: String = ""
    /// Voice-hero vs. text-composer layout. Default is voice-first; the text field
    /// takes over the moment the user opts into typing (or already has text).
    @Binding var isVoiceMode: Bool
    /// Hold-to-talk callbacks. Quick taps and deliberate holds share one control,
    /// but retain explicit start/finish intent through the whole gesture.
    var onHoldStart: () -> Void = {}
    var onHoldEnd: () -> Void = {}
    /// Starts the Phase-2 Live voice call. Invoked by the "Live" pill when a realtime
    /// voice provider is configured (`liveEnabled`); a no-op otherwise.
    var onLive: () -> Void = {}
    /// Whether a realtime voice provider is available. When false the "Live" pill is
    /// visibly dimmed and non-interactive.
    var liveEnabled: Bool = false
    /// Whether the cascaded STT -> chat -> TTS path is fully configured.
    var handsFreeAvailable: Bool = false
    /// A connected/connecting call has already latched scoped voice settings.
    var voiceCallActive: Bool = false
    var voiceQueue: AnyView? = nil
    var onBackground: (() -> Void)? = nil
    var onStopAndSend: (() -> Void)? = nil
    var queueMutationInFlight: Bool = false

    @StateObject private var themeManager = ThemeManager.shared
    @ObservedObject private var audio = AudioSettings.shared
    @ObservedObject private var primaryAgent = PrimaryAgentSiriAdvertiser.shared
    @FocusState private var isFocused: Bool
    /// Live `@…` matches for the picker (empty = picker hidden).
    @State private var mentionMatches: [ComposerMentionItem] = []
    /// True after a deliberate press has crossed the hold-to-talk threshold.
    @State private var holding = false
    /// Drives the animated recording pulse ring on the big mic.
    @State private var pulse = false
    /// When the Live pill is tapped while no realtime voice provider is configured,
    /// briefly surface an "unavailable" hint in place of the label.
    @State private var showLiveUnavailable = false
    @State private var showProfilePicker = false
    @State private var voiceSettingsSection: VoiceSettingsSection?

    private let voiceDockControlHeight: CGFloat = 42

    private var canSend: Bool {
        !text.trimmingCharacters(in: .whitespaces).isEmpty
            || stagedAttachments.contains { $0.remoteId != nil }
    }

    /// True when the active profile is empty/default — drives the chip's adaptive
    /// icon/border (a non-default tier reads as an accent-highlighted chip).
    private var isDefaultProfileSelected: Bool {
        if selectedHarnessEngine != "magician" && selectedHarnessEngine != "pi" {
            return selectedHarnessModel == "default"
        }
        if selectedProfile.isEmpty { return true }
        return profiles.first(where: { $0.name == selectedProfile })?.isDefault == true
    }

    private var activeChatProfile: ChatProfile? {
        profiles.first(where: { $0.name == selectedProfile })
            ?? profiles.first(where: { $0.isDefault == true })
            ?? profiles.first
    }

    /// Voice-hero is active only when the user is in voice mode AND hasn't started
    /// typing — any pending text collapses the mic back to the text composer so a
    /// half-written / dictated message is never hidden behind the big button.
    private var showVoiceHero: Bool {
        isVoiceMode && !voiceCallActive && text.trimmingCharacters(in: .whitespaces).isEmpty
    }

    var body: some View {
        VStack(spacing: 0) {
            // @-mention picker floats above the card.
            if !mentionMatches.isEmpty { mentionPicker }

            // The composer card.
            VStack(spacing: 0) {
                if let voiceQueue { voiceQueue }
                VStack(alignment: .leading, spacing: 10) {
                    if !stagedAttachments.isEmpty { attachmentStrip }

                    // Primary input area: the big tap-or-hold mic (voice-hero) OR the
                    // existing multi-line text field + Send/Stop (text mode).
                    if showVoiceHero {
                        voiceHero
                    } else {
                        textComposerRow
                    }

                    // Second row: secondary tools, reachable in BOTH modes.
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 10) {
                            modeToggle
                            Divider().frame(height: 16).background(themeManager.secondaryTextColor.opacity(0.3))
                            toolButton("paperclip", action: onAttach)
                            toolButton("camera", action: onCamera)
                            // Voice controls (mute/auto-speak, Live, mic) live in the voice
                            // dock above in voice mode. In text mode they're gone from this
                            // row entirely: the mic moves into the trailing send slot (mic
                            // when the field is empty, Send once you type), and mute/Live are
                            // hidden — they're voice-only concerns.
                            profileChip
                        }
                    }
                }
                .padding(12)
            }
            .background(themeManager.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: Self.cornerRadius))
            .overlay(
                RoundedRectangle(cornerRadius: Self.cornerRadius)
                    .stroke(mode == .ask ? themeManager.secondaryTextColor.opacity(0.2) : themeManager.accentColor.opacity(0.4), lineWidth: 1)
            )
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
        }
        .background(mode == .ask ? themeManager.backgroundColor : themeManager.accentColor.opacity(0.05))
        .sheet(isPresented: $showProfilePicker) {
            ChatProfilePickerSheet(
                profiles: profiles,
                selectedProfile: $selectedProfile,
                chatHarnesses: chatHarnesses,
                selectedHarnessEngine: $selectedHarnessEngine,
                selectedHarnessModel: $selectedHarnessModel,
                theme: themeManager
            )
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
            .presentationBackground(themeManager.backgroundColor)
        }
        .sheet(item: $voiceSettingsSection) { section in
            VoiceSettingsSheet(
                initialSection: section,
                liveEnabled: liveEnabled,
                handsFreeAvailable: handsFreeAvailable,
                voiceCallActive: voiceCallActive,
                onStartCall: startSelectedLiveMode,
                audio: audio,
                theme: themeManager
            )
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
            .presentationBackground(themeManager.backgroundColor)
        }
    }

    // MARK: - Voice-hero (primary in voice mode)

    /// The dominant tap-or-hold mic. A quick tap toggles dictation; a deliberate
    /// hold preserves walkie-talkie capture and sends when released.
    private var voiceHero: some View {
        VStack(spacing: 10) {
            // Live transcript / status line above the mic.
            if isRecording && !partialTranscript.isEmpty {
                Text(partialTranscript)
                    .font(themeManager.font(15))
                    .foregroundColor(themeManager.textColor)
                    .multilineTextAlignment(.center)
                    .lineLimit(3)
                    .frame(maxWidth: .infinity)
                    .transition(.opacity)
            } else if isTranscribing {
                Text("Transcribing…")
                    .font(.themed(13, weight: .medium))
                    .foregroundColor(themeManager.accentColor)
            }
            if audio.archiveChatDictation {
                Label(
                    isRecording || isTranscribing
                        ? "Saving this dictation in Audio Notes"
                        : "Keep dictation recordings is on",
                    systemImage: "waveform.badge.plus"
                )
                    .font(.themed(11, weight: .medium))
                    .foregroundColor(themeManager.accentColor)
                    .accessibilityIdentifier("chat-audio-note-disclosure")
            }

            // The big mic sits dead-center in the dock. It lives in its own centered
            // layer while the flanking controls (mute left, Live right) are overlaid
            // on top, so their differing widths can never shift the mic off-center.
            ZStack {
                // Centered big mic.
                ZStack {
                    // Animated pulse ring while recording.
                    if isRecording {
                        Circle()
                            .stroke(themeManager.dangerColor.opacity(0.35), lineWidth: 3)
                            .frame(width: 84, height: 84)
                            .scaleEffect(pulse ? 1.25 : 0.95)
                            .opacity(pulse ? 0.0 : 0.8)
                            .animation(.easeOut(duration: 1.0).repeatForever(autoreverses: false), value: pulse)
                    }
                    Circle()
                        .fill(isRecording ? themeManager.dangerColor : themeManager.accentColor)
                        .frame(width: 72, height: 72)
                        .shadow(color: (isRecording ? themeManager.dangerColor : themeManager.accentColor).opacity(0.35), radius: 10, y: 3)
                    Image(systemName: isTranscribing ? "waveform" : "mic.fill")
                        .font(.system(size: 28, weight: .semibold))
                        .foregroundColor(themeManager.onAccentColor)
                        .symbolEffect(.pulse, isActive: isRecording)
                }
                .scaleEffect(holding ? 0.92 : 1.0)
                .animation(.spring(response: 0.25, dampingFraction: 0.6), value: holding)
                .contentShape(Circle())
                // Once capture is active this is only a stop control. Keeping the
                // dual tap/hold recognizer attached while recording makes an
                // externally started capture depend on gesture-local press state.
                // A direct tap gives widget, Shortcut, and in-app starts the same
                // deterministic finalization action.
                .modifier(ActiveDictationInteraction(
                    holding: $holding,
                    isRecording: isRecording,
                    onTapStart: onTapDictationStart,
                    onTapStop: onTapDictationStop,
                    onHoldStart: onHoldStart,
                    onHoldEnd: onHoldEnd
                ))
                .disabled(isTranscribing)
                .accessibilityLabel(isRecording ? "Stop dictation" : "Talk")
                .accessibilityHint(isRecording
                    ? "Tap to finish dictation."
                    : "Tap or hold to dictate. Release after holding to send.")
                .accessibilityAddTraits(.startsMediaSession)
                .accessibilityIdentifier("chat-voice-mic")
                .onChange(of: isRecording) { _, rec in pulse = rec }

                // Flanking controls overlaid so they never push the mic off-center.
                HStack {
                    voiceOptionsControl
                    Spacer()
                    liveButton
                }
            }
            .frame(maxWidth: .infinity)

            // Hint + a lightweight "type instead" affordance.
            HStack(spacing: 4) {
                Text(micInteractionHint)
                    .font(.themed(12, weight: .medium))
                    .foregroundColor(themeManager.secondaryTextColor)
                if !isRecording {
                    Text("·").foregroundColor(themeManager.secondaryTextColor)
                    Button(action: { withAnimation { isVoiceMode = false }; isFocused = true }) {
                        Text("type instead")
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(themeManager.accentColor)
                    }
                    .accessibilityIdentifier("chat-type-instead")
                }
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 6)
    }

    private var micInteractionHint: String {
        if holding { return "Listening — release to send" }
        if isRecording { return "Listening — tap to finish" }
        return audio.archiveChatDictation
            ? "Tap or hold to dictate · recording is saved"
            : "Tap or hold to dictate"
    }

    /// The existing text composer: multi-line field + Send/Stop, unchanged, plus a
    /// small mic affordance that flips back to voice-hero.
    private var textComposerRow: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField(placeholder, text: $text, axis: .vertical)
                .accessibilityIdentifier("chat-composer")
                .lineLimit(1...5)
                .font(themeManager.font(16))
                .foregroundColor(themeManager.textColor)
                .focused($isFocused)
                // (No keyboard-accessory "hide" button — the keyboard has its own
                // dismiss key, and tapping the chat area dismisses it too.)
                .onChange(of: text) { recomputeMentions() }
                .padding(.horizontal, 10)
                // Match the Send button's height so a single-line field reads
                // as tall as (and lines up with) the button; the text stays
                // vertically centered and the field still grows to 5 lines.
                .frame(minHeight: 36, alignment: .center)
            if canSend, mode != .plan { sendOptionsMenu }
            trailingComposerButton
        }
    }

    private var sendOptionsMenu: some View {
        Menu {
            Button(isThinking ? "Queue message" : "Send message", action: onSend)
            if isThinking, let onStopAndSend { Button("Stop & send", action: onStopAndSend) }
            if stagedAttachments.isEmpty, let onBackground {
                Button("Run in parallel", action: onBackground).accessibilityIdentifier("chat-background-send")
            }
        } label: {
            Image(systemName: "chevron.down")
                .font(.system(size: 12, weight: .semibold))
                .foregroundColor(themeManager.secondaryTextColor)
                .frame(width: 30, height: 36)
                .contentShape(Rectangle())
        }
        .tint(themeManager.accentColor)
        .disabled(queueMutationInFlight)
        .accessibilityLabel("Send options")
        .accessibilityIdentifier("chat-send-options")
    }

    // MARK: - Bottom-bar pieces

    private var placeholder: String {
        let assistantName = primaryAgent.preferredName ?? MagicianAccess.productName
        switch mode {
        case .ask: return "Ask \(assistantName)..."
        case .acceptInScope: return "Accept in-scope edits..."
        case .plan: return "Plan with \(assistantName)..."
        }
    }

    private var modeToggle: some View {
        HStack(spacing: 0) {
            doModeButton
            modeButton("PLAN", .plan, fill: themeManager.accentColor, onFill: themeManager.onAccentColor)
        }
        .padding(2)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(themeManager.softBackgroundColor)
        )
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Execution mode")
    }

    /// Do is a split control: its face restores the remembered permission and
    /// only its caret opens the Ask/Accept picker. Accept is a Do permission,
    /// not a third top-level mode.
    private var doModeButton: some View {
        let selected = mode != .plan
        let askFill = themeManager.isDark ? themeManager.accentColor.opacity(0.18) : themeManager.textColor
        let askForeground = themeManager.isDark ? themeManager.textColor : themeManager.backgroundColor
        let fill = doPermission == .ask ? askFill : themeManager.accentColor
        let onFill = doPermission == .ask ? askForeground : themeManager.onAccentColor
        let foreground = selected ? onFill : themeManager.secondaryTextColor

        return HStack(spacing: 0) {
            Button(action: activateRememberedDoMode) {
                HStack(spacing: 3) {
                    Text("DO")
                    Text("· \(doPermission.label.uppercased())")
                        .opacity(0.82)
                }
                .font(.themedMono(10, weight: .semibold))
                .tracking(0.6)
                .padding(.leading, 8)
                .padding(.trailing, 5)
                .padding(.vertical, 4)
                .foregroundColor(foreground)
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(selected ? .isSelected : [])
            .accessibilityLabel("Do · \(doPermission.label)")

            Rectangle()
                .fill(foreground.opacity(0.28))
                .frame(width: 1, height: 12)
                .accessibilityHidden(true)

            Menu {
                ForEach(ComposerDoPermission.allCases, id: \.self) { permission in
                    doPermissionMenuItem(permission)
                }
            } label: {
                Image(systemName: "chevron.down")
                    .font(.system(size: 8, weight: .bold))
                    .foregroundColor(foreground)
                    .frame(width: 22, height: 24)
            }
            .accessibilityLabel("Choose what Do asks before editing files")
        }
        .background(selected ? fill : Color.clear)
        .clipShape(RoundedRectangle(cornerRadius: 4))
    }

    private func activateRememberedDoMode() {
        withAnimation { mode = doPermission.mode }
    }

    private func doPermissionMenuItem(_ permission: ComposerDoPermission) -> some View {
        Button {
            withAnimation {
                doPermission = permission
                mode = permission.mode
            }
        } label: {
            Label {
                VStack(alignment: .leading, spacing: 2) {
                    Text(permission.label)
                    Text(permission.guidance)
                        .font(.caption)
                }
            } icon: {
                Image(systemName: doPermission == permission ? "checkmark" : "circle")
            }
        }
        .accessibilityLabel("\(permission.label). \(permission.guidance)")
        .accessibilityAddTraits(doPermission == permission ? .isSelected : [])
    }

    private func modeButton(_ label: String, _ value: ComposerMode, fill: Color, onFill: Color) -> some View {
        Button(action: { withAnimation { mode = value } }) {
            Text(label)
                .font(.themedMono(10, weight: .semibold))
                .tracking(0.6)
                .padding(.horizontal, 8).padding(.vertical, 4)
                .background(mode == value ? fill : Color.clear)
                .foregroundColor(mode == value ? onFill : themeManager.secondaryTextColor)
                .clipShape(RoundedRectangle(cornerRadius: 4))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(mode == value ? .isSelected : [])
        .accessibilityLabel(label)
    }

    private func toolButton(_ system: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: system)
                .font(.system(size: 16))
                .foregroundColor(themeManager.secondaryTextColor)
                .frame(width: 30, height: 30)
        }
    }

    /// Mic: tap to dictate → transcript fills the composer → auto-send countdown.
    /// Recording pulses red; a transient waveform shows while transcribing.
    private var micButton: some View {
        Button(action: isRecording ? onTapDictationStop : onTapDictationStart) {
            Image(systemName: isRecording ? "mic.fill" : (isTranscribing ? "waveform" : "mic"))
                .font(.system(size: 16))
                .foregroundColor(isRecording ? themeManager.dangerColor : (isTranscribing ? themeManager.accentColor : themeManager.secondaryTextColor))
                .frame(width: 30, height: 30)
                .background(isRecording ? themeManager.dangerColor.opacity(0.12) : Color.clear)
                .clipShape(Circle())
                .symbolEffect(.pulse, isActive: isRecording)
        }
        .disabled(isTranscribing)
    }

    private var selectedRealtimeProfile: RealtimeVoiceProfileOption? {
        audio.realtimeVoiceProfiles.first(where: { $0.id == audio.realtimeVoiceProfile })
    }

    private var selectedLiveModeAvailable: Bool {
        switch audio.liveVoiceEngine {
        case .realtime:
            return liveEnabled && selectedRealtimeProfile?.available == true
        case .handsFree:
            return handsFreeAvailable
        }
    }

    private var selectedLiveModeLabel: String {
        audio.liveVoiceEngine == .realtime ? "Live" : "Hands-free"
    }

    /// Split Live control: the primary action starts the locally selected setup;
    /// the chevron opens the iOS-only voice settings sheet at call setup.
    private var liveButton: some View {
        HStack(spacing: 0) {
            Button(action: startSelectedLiveMode) {
                HStack(spacing: 4) {
                    Image(systemName: audio.liveVoiceEngine == .realtime
                        ? "dot.radiowaves.left.and.right"
                        : "waveform")
                        .font(.system(size: 11))
                    Text(showLiveUnavailable ? "Unavailable" : selectedLiveModeLabel)
                        .font(.themed(11, weight: .semibold))
                }
                .padding(.leading, 8)
                .padding(.trailing, 6)
                .frame(height: voiceDockControlHeight)
            }
            .disabled(voiceCallActive)

            Rectangle()
                .fill(themeManager.secondaryTextColor.opacity(0.22))
                .frame(width: 1, height: 18)

            Button {
                voiceSettingsSection = .session
            } label: {
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .bold))
                    .frame(width: 27, height: voiceDockControlHeight)
            }
            .disabled(voiceCallActive)
            .accessibilityLabel("Live call settings")
            .accessibilityHint("Opens mode, model, and turn-taking settings")
            .accessibilityIdentifier("voice-settings-session")
        }
        .buttonStyle(.plain)
        .foregroundColor(themeManager.secondaryTextColor)
        .background(themeManager.secondaryTextColor.opacity(0.1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(themeManager.secondaryTextColor.opacity(0.25), lineWidth: 1)
        )
        .opacity(selectedLiveModeAvailable ? 1.0 : 0.55)
        .frame(height: voiceDockControlHeight)
        .accessibilityElement(children: .contain)
    }

    private func startSelectedLiveMode() {
        guard selectedLiveModeAvailable, !voiceCallActive else {
            withAnimation { showLiveUnavailable = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.8) {
                withAnimation { showLiveUnavailable = false }
            }
            return
        }
        onLive()
    }

    /// Split reply-audio control. The primary speaker button directly toggles
    /// reply speech; the chevron owns the less-frequent voice settings.
    private var voiceOptionsControl: some View {
        let repliesEnabled = audio.speakReplies && !voiceCallActive
        return HStack(spacing: 0) {
            Button {
                audio.speakReplies.toggle()
            } label: {
                Image(systemName: repliesEnabled ? "speaker.wave.2.fill" : "speaker.slash")
                    .font(.system(size: 15))
                    .frame(width: 35, height: voiceDockControlHeight)
            }
            .buttonStyle(.plain)
            .disabled(voiceCallActive)
            .accessibilityLabel(repliesEnabled ? "Mute spoken replies" : "Unmute spoken replies")
            .accessibilityHint(voiceCallActive
                ? "Replies are temporarily muted during the voice session."
                : "Changes the iOS reply speech preference.")

            Rectangle()
                .fill(themeManager.secondaryTextColor.opacity(0.22))
                .frame(width: 1, height: 22)

            Button {
                voiceSettingsSection = .replies
            } label: {
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .bold))
                    .frame(width: 27, height: voiceDockControlHeight)
            }
            .accessibilityLabel("More voice settings")
            .accessibilityHint("Opens reply, dictation, and voice settings")
            .accessibilityIdentifier("voice-settings-replies")
        }
        .foregroundColor(repliesEnabled
            ? themeManager.accentColor
            : themeManager.secondaryTextColor)
        .background(repliesEnabled
            ? themeManager.accentColor.opacity(0.12)
            : themeManager.secondaryTextColor.opacity(0.1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(
                    repliesEnabled
                        ? themeManager.accentColor.opacity(0.4)
                        : themeManager.secondaryTextColor.opacity(0.25),
                    lineWidth: 1
                )
        )
        .frame(height: voiceDockControlHeight)
    }

    private var profileChip: some View {
        Button(action: { showProfilePicker = true }) {
            HStack(spacing: 4) {
                Image(systemName: isDefaultProfileSelected ? "sparkles" : "sparkles.rectangle.stack.fill")
                    .font(.system(size: 12))
                    .foregroundColor(isDefaultProfileSelected ? themeManager.textColor : themeManager.accentColor)
                if (selectedHarnessEngine == "magician" || selectedHarnessEngine == "pi"), activeChatProfile?.isAdaptive == true {
                    ChatProfileTag(text: "Adaptive", color: themeManager.accentColor)
                }
                if (selectedHarnessEngine == "magician" || selectedHarnessEngine == "pi"), let tier = activeChatProfile?.adaptiveTier, !tier.isEmpty {
                    ChatProfileTag(text: tier, color: chatProfileTierColor(tier, theme: themeManager))
                }
                Text("\(selectedHarnessEngine == "claude_code" ? "Claude Code" : selectedHarnessEngine.replacingOccurrences(of: "_", with: " ")) · \(selectedHarnessEngine == "magician" || selectedHarnessEngine == "pi" ? (activeChatProfile?.name ?? "Default") : selectedHarnessModel)")
                    .font(.themedMono(10, weight: .medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 180)
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 10))
            }
            .padding(.horizontal, 8).padding(.vertical, 6)
            .background(themeManager.surfaceColor)
            .foregroundColor(themeManager.textColor)
            .cornerRadius(12)
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .stroke((isDefaultProfileSelected ? themeManager.secondaryTextColor.opacity(0.2) : themeManager.accentColor.opacity(0.5)), lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Chat engine: \(selectedHarnessEngine); choice: \(selectedHarnessEngine == "magician" || selectedHarnessEngine == "pi" ? (activeChatProfile?.name ?? "Default") : selectedHarnessModel)")
        .accessibilityHint("Opens the chat engine and profile picker")
    }

    /// Trailing action in text mode. Thinking → Stop; something to send → Send;
    /// otherwise a mic occupies the same slot and flips to the voice-hero
    /// (WhatsApp-style mic⇄send). Mute/Live never appear in text mode.
    @ViewBuilder
    private var trailingComposerButton: some View {
        if isThinking && !canSend {
            Button(action: onStop) {
                Image(systemName: "square.fill")
                    .font(.system(size: 14))
                    .foregroundColor(themeManager.contrastingTextColor(for: themeManager.dangerColor))
                    .frame(width: 36, height: 36)
                    .background(themeManager.dangerColor)
                    .cornerRadius(12)
            }
            .accessibilityLabel("Stop")
            .accessibilityIdentifier("chat-stop")
        } else if canSend {
            Button(action: onSend) {
                Image(systemName: "arrow.up")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundColor(themeManager.onAccentColor)
                    .frame(width: 36, height: 36)
                    .background(themeManager.accentColor)
                    .cornerRadius(12)
            }
            .disabled(queueMutationInFlight)
            .accessibilityLabel(isThinking ? "Queue message" : "Send")
            .accessibilityIdentifier("chat-send")
        } else if !voiceCallActive {
            // Empty field: the mic takes the send slot and flips to the voice-hero.
            Button(action: { isFocused = false; withAnimation { isVoiceMode = true } }) {
                Image(systemName: "mic.fill")
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundColor(themeManager.onAccentColor)
                    .frame(width: 36, height: 36)
                    .background(themeManager.accentColor)
                    .cornerRadius(12)
            }
            .accessibilityLabel("Voice")
            .accessibilityIdentifier("chat-mic")
        }
    }

    // MARK: - Attachment preview chips

    private var attachmentStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(stagedAttachments) { att in attachmentChip(att) }
            }
            .padding(.horizontal, 2)
        }
    }

    private func attachmentChip(_ att: StagedAttachment) -> some View {
        ZStack(alignment: .topTrailing) {
            Group {
                if let data = att.thumbnail, let ui = UIImage(data: data) {
                    Image(uiImage: ui).resizable().scaledToFill().frame(width: 54, height: 54).clipped()
                } else {
                    VStack(spacing: 3) {
                        Image(systemName: "doc.fill").font(.system(size: 18)).foregroundColor(themeManager.accentColor)
                        Text(att.filename).font(.themed(8)).lineLimit(1).foregroundColor(themeManager.secondaryTextColor)
                    }
                    .frame(width: 54, height: 54)
                    .padding(2)
                }
            }
            .background(themeManager.backgroundColor)
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(themeManager.secondaryTextColor.opacity(0.2), lineWidth: 1))
            .overlay {
                if att.uploading {
                    ZStack { Color.black.opacity(0.25); ProgressView().scaleEffect(0.7).tint(.white) }
                        .clipShape(RoundedRectangle(cornerRadius: 10))
                } else if att.failed {
                    ZStack { Color.black.opacity(0.35); Image(systemName: "exclamationmark.triangle.fill").foregroundColor(.yellow) }
                        .clipShape(RoundedRectangle(cornerRadius: 10))
                }
            }

            Button(action: { onRemoveAttachment(att.id) }) {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 16))
                    .foregroundColor(.white)
                    .background(Circle().fill(Color.black.opacity(0.55)))
            }
            .offset(x: 6, y: -6)
        }
        .padding(.top, 6).padding(.trailing, 6)
    }

    // MARK: - @-mention picker

    private var mentionPicker: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(mentionMatches) { item in
                        Button(action: { commitMention(item) }) {
                            HStack(spacing: 10) {
                                Text(mentionKindLabel(item.kind).uppercased())
                                    .font(.themed(9, weight: .bold))
                                    .padding(.horizontal, 6).padding(.vertical, 3)
                                    .background(themeManager.accentColor.opacity(0.15))
                                    .foregroundColor(themeManager.accentColor)
                                    .cornerRadius(5)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(item.chipLabel ?? item.label)
                                        .font(.themed(14, weight: .medium))
                                        .foregroundColor(themeManager.textColor)
                                        .lineLimit(1)
                                    if let detail = item.detail, !detail.isEmpty {
                                        Text(detail)
                                            .font(.themed(11))
                                            .foregroundColor(themeManager.secondaryTextColor)
                                            .lineLimit(1)
                                    }
                                }
                                Spacer()
                            }
                            .padding(.horizontal, 14).padding(.vertical, 8)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        Divider().background(themeManager.secondaryTextColor.opacity(0.08))
                    }
                }
            }
            .frame(maxHeight: 220)
        }
        .background(themeManager.surfaceColor)
        .cornerRadius(14)
        .overlay(
            RoundedRectangle(cornerRadius: 14)
                .stroke(themeManager.secondaryTextColor.opacity(0.15), lineWidth: 1)
        )
        .padding(.horizontal, 12)
        .padding(.bottom, 6)
    }

    /// Recompute `@…` matches from the text before the caret (treated as the whole
    /// text — the common type-to-filter case).
    private func recomputeMentions() {
        if let trigger = detectMentionTrigger(text) {
            mentionMatches = mentionMatchesFor(mentionItems, query: trigger.query)
        } else {
            mentionMatches = []
        }
    }

    /// Splice the picked mention's serialized token in place of the `@…` trigger —
    /// the exact string the web chip emits, so the backend routes identically.
    private func commitMention(_ item: ComposerMentionItem) {
        guard let trigger = detectMentionTrigger(text) else { mentionMatches = []; return }
        let base = String(text.dropLast(trigger.consume))
        text = base + item.serialized + " "
        mentionMatches = []
    }
}

private struct VoiceSettingsSheet: View {
    @Environment(\.dismiss) private var dismiss

    let liveEnabled: Bool
    let handsFreeAvailable: Bool
    let voiceCallActive: Bool
    let onStartCall: () -> Void
    @ObservedObject var audio: AudioSettings
    @ObservedObject var theme: ThemeManager
    @State private var selectedSection: VoiceSettingsSection

    init(
        initialSection: VoiceSettingsSection,
        liveEnabled: Bool,
        handsFreeAvailable: Bool,
        voiceCallActive: Bool,
        onStartCall: @escaping () -> Void,
        audio: AudioSettings,
        theme: ThemeManager
    ) {
        self.liveEnabled = liveEnabled
        self.handsFreeAvailable = handsFreeAvailable
        self.voiceCallActive = voiceCallActive
        self.onStartCall = onStartCall
        self.audio = audio
        self.theme = theme
        _selectedSection = State(initialValue: initialSection)
    }

    private var selectedRealtimeProfile: RealtimeVoiceProfileOption? {
        audio.realtimeVoiceProfiles.first(where: { $0.id == audio.realtimeVoiceProfile })
    }

    private var selectedSetupAvailable: Bool {
        switch audio.liveVoiceEngine {
        case .realtime:
            return liveEnabled && selectedRealtimeProfile?.available == true
        case .handsFree:
            return handsFreeSetupAvailable
        }
    }

    private var handsFreeSetupAvailable: Bool {
        audio.selectedNativeProfileAvailable(for: .handsFree) ?? handsFreeAvailable
    }

    private var handsFreeModeAvailable: Bool {
        audio.hasAvailableNativeProfile(for: .handsFree) ?? handsFreeAvailable
    }

    private var callButtonTitle: String {
        if voiceCallActive { return "Call in progress" }
        if !selectedSetupAvailable { return "Selected setup unavailable" }
        return audio.liveVoiceEngine == .realtime ? "Start live call" : "Start hands-free"
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                sectionPicker
                    .padding(.horizontal, 16)
                    .padding(.top, 10)
                    .padding(.bottom, 8)

                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        Group {
                            switch selectedSection {
                            case .replies: replySettings
                            case .dictation: dictationSettings
                            case .session: sessionSettings
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                }

                if selectedSection == .session {
                    callAction
                }
            }
            .background(theme.backgroundColor)
            .navigationTitle("Voice settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .foregroundColor(theme.accentColor)
                }
            }
        }
        .preferredColorScheme(theme.forcedColorScheme)
        .sensoryFeedback(.selection, trigger: selectedSection)
        .accessibilityIdentifier("voice-settings-sheet")
    }

    private var sectionPicker: some View {
        HStack(spacing: 4) {
            ForEach(VoiceSettingsSection.allCases) { section in
                Button {
                    withAnimation(.easeInOut(duration: 0.16)) {
                        selectedSection = section
                    }
                } label: {
                    Label(section.title, systemImage: section.icon)
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(selectedSection == section
                            ? theme.onAccentColor
                            : theme.secondaryTextColor)
                        .frame(maxWidth: .infinity, minHeight: 38)
                        .background(selectedSection == section
                            ? theme.accentColor
                            : Color.clear)
                        .clipShape(RoundedRectangle(cornerRadius: 7))
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selectedSection == section ? .isSelected : [])
                .accessibilityIdentifier("voice-settings-tab-\(section.rawValue)")
            }
        }
        .padding(3)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(theme.softBackgroundColor)
        )
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(theme.cardBorderColor, lineWidth: 1)
        )
    }

    private var replySettings: some View {
        VStack(alignment: .leading, spacing: 20) {
            VStack(spacing: 0) {
                toggleRow(
                    title: "Speak replies",
                    subtitle: voiceCallActive
                        ? "Temporarily muted during the active call"
                        : "Read assistant replies aloud",
                    icon: "speaker.wave.2",
                    isOn: $audio.speakReplies,
                    disabled: voiceCallActive
                )
            }
            .modifier(VoiceSettingsGroupStyle(theme: theme))

            ttsSelection
        }
    }

    private var dictationSettings: some View {
        VStack(alignment: .leading, spacing: 20) {
            VStack(alignment: .leading, spacing: 8) {
                sectionHeader("Audio Notes", icon: "waveform.badge.plus")
                VStack(spacing: 0) {
                    toggleRow(
                        title: "Keep dictation recordings",
                        subtitle: "Save the original audio and transcript as a durable Audio Note. Off by default.",
                        icon: "archivebox",
                        isOn: $audio.archiveChatDictation,
                        disabled: false
                    )
                }
                .modifier(VoiceSettingsGroupStyle(theme: theme))
                Text("When enabled, the mic shows a saving disclosure. Cancelling message auto-send does not delete the Audio Note; manage saved and pending recordings from Audio Notes in the app menu.")
                    .font(.themed(11))
                    .foregroundColor(theme.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            sttSelection
            audioSurfaceSelection(.dictation)
        }
    }

    private var sttSelection: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader("Dictation", icon: "mic")
            VStack(spacing: 0) {
                ForEach(Array(STTSource.allCases.enumerated()), id: \.element.id) { index, source in
                    selectionRow(
                        title: source.label,
                        selected: audio.sttSource == source,
                        enabled: true
                    ) {
                        audio.sttSource = source
                    }
                    if index < STTSource.allCases.count - 1 { settingsDivider }
                }
            }
            .modifier(VoiceSettingsGroupStyle(theme: theme))
        }
    }

    private var ttsSelection: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader("Reply voice", icon: "waveform")
            VStack(spacing: 0) {
                ForEach(Array(TTSEngine.allCases.enumerated()), id: \.element.id) { index, engine in
                    selectionRow(
                        title: engine.label,
                        selected: audio.ttsEngine == engine,
                        enabled: true
                    ) {
                        audio.ttsEngine = engine
                    }
                    if index < TTSEngine.allCases.count - 1 { settingsDivider }
                }
            }
            .modifier(VoiceSettingsGroupStyle(theme: theme))
        }
    }

    private func audioSurfaceSelection(_ surface: NativeAudioSurface) -> some View {
        let profiles = audio.profiles(for: surface)
        let activeProfile = audio.selectedProfile(for: surface)
        let stages = NativeAudioStage.allCases.filter {
            activeProfile?.enabledStages.contains($0) == true
        }
        return VStack(alignment: .leading, spacing: 8) {
            sectionHeader(
                "\(surface.label) pipeline",
                icon: surface == .dictation ? "slider.horizontal.3" : "waveform.badge.mic"
            )
            if profiles.isEmpty {
                Text("No \(surface.label.lowercased()) profiles are available.")
                    .font(.themed(13))
                    .foregroundColor(theme.secondaryTextColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(14)
                    .modifier(VoiceSettingsGroupStyle(theme: theme))
            } else {
                VStack(spacing: 0) {
                    Menu {
                        ForEach(profiles) { profile in
                            Button {
                                audio.selectNativeProfile(profile)
                            } label: {
                                if profile.id == activeProfile?.id {
                                    Label(profile.label, systemImage: "checkmark")
                                } else {
                                    Text(profile.label)
                                }
                            }
                            .disabled(!audio.isNativeProfileAvailable(profile))
                        }
                    } label: {
                        audioPickerLabel(
                            title: surface.label,
                            value: activeProfile?.label ?? "Unavailable",
                            icon: "slider.horizontal.3"
                        )
                    }
                    .buttonStyle(.plain)

                    ForEach(stages, id: \.rawValue) { stage in
                        settingsDivider
                        stageSelection(stage, surface: surface, profile: activeProfile)
                            .accessibilityIdentifier("voice-\(surface.rawValue)-\(stage.rawValue)")
                    }
                }
                .modifier(VoiceSettingsGroupStyle(theme: theme))
            }
        }
    }

    private func stageSelection(
        _ stage: NativeAudioStage,
        surface: NativeAudioSurface,
        profile: NativeAudioProfileOption?
    ) -> some View {
        let options = profile.map { audio.stageOptions(for: stage, profile: $0) } ?? []
        let selectedID = audio.selectedNativeStageOptions[surface]?[stage]
        let selectedLabel = options.first(where: { $0.id == selectedID })?.displayLabel ?? "Profile order"
        return Menu {
            Button {
                audio.selectNativeStageOption(nil, stage: stage, surface: surface)
            } label: {
                if selectedID == nil {
                    Label("Profile order", systemImage: "checkmark")
                } else {
                    Text("Profile order")
                }
            }
            ForEach(options) { option in
                Button {
                    audio.selectNativeStageOption(option.id, stage: stage, surface: surface)
                } label: {
                    if option.id == selectedID {
                        Label(option.displayLabel, systemImage: "checkmark")
                    } else {
                        Text(option.displayLabel)
                    }
                }
                .disabled(!option.available)
            }
        } label: {
            audioPickerLabel(
                title: stage.label,
                value: selectedLabel,
                icon: audioStageIcon(stage)
            )
        }
        .buttonStyle(.plain)
        .disabled(options.isEmpty)
        .opacity(options.isEmpty ? 0.55 : 1)
    }

    private func audioPickerLabel(title: String, value: String, icon: String) -> some View {
        HStack(spacing: 10) {
            Image(systemName: icon)
                .font(.system(size: 14, weight: .semibold))
                .foregroundColor(theme.secondaryTextColor)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 3) {
                Text(title)
                    .font(.themed(13, weight: .semibold))
                    .foregroundColor(theme.textColor)
                Text(value)
                    .font(.themed(11))
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            Image(systemName: "chevron.up.chevron.down")
                .font(.system(size: 10, weight: .semibold))
                .foregroundColor(theme.secondaryTextColor)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .contentShape(Rectangle())
    }

    private func audioStageIcon(_ stage: NativeAudioStage) -> String {
        switch stage {
        case .vad: return "waveform.badge.mic"
        case .recordingSTT: return "text.bubble"
        case .streamingSTT: return "captions.bubble"
        case .diarization: return "person.2.wave.2"
        case .tts: return "speaker.wave.2"
        }
    }

    private var sessionSettings: some View {
        VStack(alignment: .leading, spacing: 20) {
            if voiceCallActive {
                Label("Call setup is locked while the current call is active.", systemImage: "lock.fill")
                    .font(.themed(12, weight: .medium))
                    .foregroundColor(theme.secondaryTextColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background(theme.warningColor.opacity(0.1))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            }

            VStack(alignment: .leading, spacing: 8) {
                sectionHeader("Call mode", icon: "waveform")
                modePicker
            }

            if audio.liveVoiceEngine == .realtime {
                realtimeProfileSelection
            } else {
                audioSurfaceSelection(.handsFree)
            }
            liveMicrophoneSelection
        }
        .disabled(voiceCallActive)
        .opacity(voiceCallActive ? 0.65 : 1)
    }

    private var modePicker: some View {
        HStack(spacing: 4) {
            modeButton(.realtime, available: liveEnabled)
            modeButton(.handsFree, available: handsFreeModeAvailable)
        }
        .padding(3)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(theme.softBackgroundColor)
        )
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(theme.cardBorderColor, lineWidth: 1)
        )
    }

    private func modeButton(_ engine: VoiceEngine, available: Bool) -> some View {
        let selected = audio.liveVoiceEngine == engine
        return Button {
            guard available else { return }
            audio.liveVoiceEngine = engine
        } label: {
            HStack(spacing: 6) {
                Image(systemName: engine == .realtime
                    ? "dot.radiowaves.left.and.right"
                    : "waveform")
                Text(engine.displayName)
                if !available {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 9))
                }
            }
            .font(.themed(12, weight: .semibold))
            .foregroundColor(selected ? theme.onAccentColor : theme.secondaryTextColor)
            .frame(maxWidth: .infinity, minHeight: 40)
            .background(selected ? theme.accentColor : Color.clear)
            .clipShape(RoundedRectangle(cornerRadius: 7))
        }
        .buttonStyle(.plain)
        .disabled(!available)
        .opacity(available ? 1 : 0.5)
        .accessibilityLabel("\(engine.displayName), \(available ? "available" : "unavailable")")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var realtimeProfileSelection: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader("Realtime model", icon: "cpu")
            if audio.realtimeVoiceProfiles.isEmpty {
                Text("No native realtime profiles are available.")
                    .font(.themed(13))
                    .foregroundColor(theme.secondaryTextColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(14)
                    .modifier(VoiceSettingsGroupStyle(theme: theme))
            } else {
                VStack(spacing: 0) {
                    ForEach(Array(audio.realtimeVoiceProfiles.enumerated()), id: \.element.id) { index, profile in
                        realtimeProfileRow(profile)
                        if index < audio.realtimeVoiceProfiles.count - 1 { settingsDivider }
                    }
                }
                .modifier(VoiceSettingsGroupStyle(theme: theme))
            }
        }
    }

    private func realtimeProfileRow(_ profile: RealtimeVoiceProfileOption) -> some View {
        let selected = profile.id == audio.realtimeVoiceProfile
        return Button {
            _ = audio.selectRealtimeVoiceProfile(profile)
        } label: {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: profile.isTranslation ? "captions.bubble.fill" : "waveform")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundColor(selected ? theme.accentColor : theme.secondaryTextColor)
                    .frame(width: 20, height: 22)

                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 6) {
                        Text(profile.label)
                            .font(.themed(14, weight: .semibold))
                            .foregroundColor(theme.textColor)
                            .lineLimit(2)
                        Spacer(minLength: 8)
                        if selected {
                            Image(systemName: "checkmark.circle.fill")
                                .font(.system(size: 17, weight: .semibold))
                                .foregroundColor(theme.accentColor)
                        }
                    }
                    HStack(spacing: 6) {
                        VoiceSettingsTag(text: providerLabel(profile.provider), color: theme.infoColor)
                        if profile.isTranslation {
                            VoiceSettingsTag(text: "Translate", color: theme.discoveryColor)
                        }
                    }
                    Text(profile.model)
                        .font(.themedMono(10))
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let transcription = profile.transcriptionLabel {
                        Text(transcription)
                            .font(.themed(11, weight: .medium))
                            .foregroundColor(theme.secondaryTextColor)
                            .lineLimit(1)
                    }
                    if !profile.available, let reason = profile.unavailableReason, !reason.isEmpty {
                        Text(reason)
                            .font(.themed(11))
                            .foregroundColor(theme.warningColor)
                            .lineLimit(2)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 11)
            .background(selected ? theme.accentColor.opacity(0.08) : Color.clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!profile.available)
        .opacity(profile.available ? 1 : 0.55)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var liveMicrophoneSelection: some View {
        VStack(alignment: .leading, spacing: 8) {
            sectionHeader("Microphone", icon: "mic")
            HStack(spacing: 4) {
                turnTakingButton(pttOn: false, title: "Open mic", icon: "mic.fill")
                turnTakingButton(pttOn: true, title: "Hold to talk", icon: "hand.tap.fill")
            }
            .padding(3)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(theme.softBackgroundColor)
            )
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(theme.cardBorderColor, lineWidth: 1)
            )
            if audio.liveVoiceEngine == .realtime && selectedRealtimeProfile?.isTranslation == true {
                Text("Translation uses open mic for continuous two-way audio.")
                    .font(.themed(11))
                    .foregroundColor(theme.secondaryTextColor)
            } else {
                Text(audio.liveVoicePttOn
                    ? "The microphone opens only while you hold the talk control."
                    : "The microphone stays on; you can mute it during the call.")
                    .font(.themed(11))
                    .foregroundColor(theme.secondaryTextColor)
            }
        }
    }

    private func turnTakingButton(pttOn: Bool, title: String, icon: String) -> some View {
        let selected = audio.liveVoicePttOn == pttOn
        let available = !(pttOn
            && audio.liveVoiceEngine == .realtime
            && selectedRealtimeProfile?.isTranslation == true)
        return Button {
            audio.liveVoicePttOn = AudioSettings.resolveLiveVoicePttOn(
                requested: pttOn,
                engine: audio.liveVoiceEngine,
                profile: selectedRealtimeProfile
            )
        } label: {
            Label(title, systemImage: icon)
                .font(.themed(12, weight: .semibold))
                .foregroundColor(selected ? theme.onAccentColor : theme.secondaryTextColor)
                .frame(maxWidth: .infinity, minHeight: 40)
                .background(selected ? theme.accentColor : Color.clear)
                .clipShape(RoundedRectangle(cornerRadius: 7))
        }
        .buttonStyle(.plain)
        .disabled(!available)
        .opacity(available ? 1 : 0.5)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var callAction: some View {
        Button {
            dismiss()
            DispatchQueue.main.async { onStartCall() }
        } label: {
            Label(callButtonTitle, systemImage: audio.liveVoiceEngine == .realtime
                ? "dot.radiowaves.left.and.right"
                : "waveform")
                .font(.themed(15, weight: .semibold))
                .foregroundColor(theme.onAccentColor)
                .frame(maxWidth: .infinity, minHeight: 46)
                .background(theme.accentColor)
                .clipShape(RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .disabled(voiceCallActive || !selectedSetupAvailable)
        .opacity(voiceCallActive || !selectedSetupAvailable ? 0.5 : 1)
        .padding(.horizontal, 16)
        .padding(.top, 10)
        .padding(.bottom, 12)
        .background(theme.backgroundColor)
        .overlay(alignment: .top) {
            Rectangle()
                .fill(theme.cardBorderColor)
                .frame(height: 1)
        }
        .accessibilityIdentifier("voice-settings-start-call")
    }

    private func toggleRow(
        title: String,
        subtitle: String,
        icon: String,
        isOn: Binding<Bool>,
        disabled: Bool
    ) -> some View {
        HStack(spacing: 12) {
            Image(systemName: icon)
                .font(.system(size: 15, weight: .semibold))
                .foregroundColor(theme.accentColor)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.themed(14, weight: .semibold))
                    .foregroundColor(theme.textColor)
                Text(subtitle)
                    .font(.themed(11))
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(2)
            }
            Spacer(minLength: 8)
            Toggle("", isOn: isOn)
                .labelsHidden()
                .tint(theme.accentColor)
                .disabled(disabled)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 11)
        .opacity(disabled ? 0.6 : 1)
    }

    private func selectionRow(
        title: String,
        selected: Bool,
        enabled: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Text(title)
                    .font(.themed(13, weight: selected ? .semibold : .regular))
                    .foregroundColor(theme.textColor)
                    .multilineTextAlignment(.leading)
                Spacer(minLength: 8)
                if selected {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundColor(theme.accentColor)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 11)
            .background(selected ? theme.accentColor.opacity(0.08) : Color.clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func sectionHeader(_ title: String, icon: String) -> some View {
        Label(title, systemImage: icon)
            .font(.themed(11, weight: .bold))
            .foregroundColor(theme.secondaryTextColor)
            .textCase(.uppercase)
            .padding(.horizontal, 4)
    }

    private var settingsDivider: some View {
        Divider()
            .background(theme.cardBorderColor)
            .padding(.leading, 44)
    }

    private func providerLabel(_ provider: String) -> String {
        provider
            .replacingOccurrences(of: "_backend", with: "")
            .replacingOccurrences(of: "_", with: " ")
            .capitalized
    }
}

private struct VoiceSettingsGroupStyle: ViewModifier {
    @ObservedObject var theme: ThemeManager

    func body(content: Content) -> some View {
        content
            .background(theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(theme.cardBorderColor, lineWidth: 1)
            )
    }
}

private struct VoiceSettingsTag: View {
    let text: String
    let color: Color

    var body: some View {
        Text(text.uppercased())
            .font(.system(size: 9, weight: .semibold))
            .foregroundColor(color)
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(color.opacity(0.12))
            .clipShape(Capsule())
            .overlay(Capsule().stroke(color.opacity(0.32), lineWidth: 1))
    }
}

private struct ChatProfilePickerSheet: View {
    @Environment(\.dismiss) private var dismiss

    let profiles: [ChatProfile]
    @Binding var selectedProfile: String
    let chatHarnesses: [ChatHarnessOption]
    @Binding var selectedHarnessEngine: String
    @Binding var selectedHarnessModel: String
    @ObservedObject var theme: ThemeManager

    private var adaptiveProfiles: [ChatProfile] {
        profiles.filter { $0.isAdaptive == true }
    }

    private var standardProfiles: [ChatProfile] {
        profiles.filter { $0.isAdaptive != true }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 20) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Engine").font(.themed(11, weight: .bold)).foregroundColor(theme.secondaryTextColor)
                        ForEach(chatHarnesses) { engine in
                            Button {
                                selectedHarnessEngine = engine.name
                            } label: {
                                HStack {
                                    Text(engine.name.replacingOccurrences(of: "_", with: " "))
                                    Spacer()
                                    if engine.name == selectedHarnessEngine { Image(systemName: "checkmark.circle.fill") }
                                }
                                .foregroundColor(theme.textColor)
                                .padding(12)
                                .background(theme.surfaceColor)
                                .clipShape(RoundedRectangle(cornerRadius: 8))
                            }
                        }
                    }
                    if selectedHarnessEngine == "magician" || selectedHarnessEngine == "pi" {
                        if !adaptiveProfiles.isEmpty {
                            profileGroup(title: "Adaptive", profiles: adaptiveProfiles)
                        }
                        if !standardProfiles.isEmpty {
                            profileGroup(title: "Standard", profiles: standardProfiles)
                        }
                    } else {
                        let models = chatHarnesses.first(where: { $0.name == selectedHarnessEngine })?.availableModels ?? ["default"]
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Model").font(.themed(11, weight: .bold)).foregroundColor(theme.secondaryTextColor)
                            ForEach(models, id: \.self) { model in
                                Button {
                                    selectedHarnessModel = model
                                    dismiss()
                                } label: {
                                    HStack {
                                        Text(model == "default" ? "Harness default" : model)
                                        Spacer()
                                        if model == selectedHarnessModel { Image(systemName: "checkmark.circle.fill") }
                                    }
                                    .foregroundColor(theme.textColor)
                                    .padding(12)
                                    .background(theme.surfaceColor)
                                    .clipShape(RoundedRectangle(cornerRadius: 8))
                                }
                            }
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }
            .background(theme.backgroundColor)
            .navigationTitle("Chat engine and profile")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .foregroundColor(theme.accentColor)
                }
            }
        }
        .preferredColorScheme(theme.forcedColorScheme)
        .sensoryFeedback(.selection, trigger: selectedProfile)
    }

    private func profileGroup(title: String, profiles: [ChatProfile]) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.themed(11, weight: .bold))
                .foregroundColor(theme.secondaryTextColor)
                .textCase(.uppercase)
                .padding(.horizontal, 4)

            VStack(spacing: 0) {
                ForEach(profiles.indices, id: \.self) { index in
                    profileRow(profiles[index])
                    if index < profiles.count - 1 {
                        Divider().background(theme.cardBorderColor)
                    }
                }
            }
            .background(theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(theme.cardBorderColor, lineWidth: 1)
            )
        }
    }

    private func profileRow(_ profile: ChatProfile) -> some View {
        let selected = profile.name == selectedProfile
            || (selectedProfile.isEmpty && profile.isDefault == true)

        return Button {
            selectedProfile = profile.name
            dismiss()
        } label: {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 6) {
                        Text(profile.name)
                            .font(.themed(15, weight: .semibold))
                            .foregroundColor(theme.textColor)
                            .lineLimit(1)
                        if profile.isDefault == true {
                            Image(systemName: "star.fill")
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundColor(theme.warningColor)
                                .accessibilityLabel("Default")
                        }
                        Spacer(minLength: 8)
                        if selected {
                            Image(systemName: "checkmark.circle.fill")
                                .font(.system(size: 18, weight: .semibold))
                                .foregroundColor(theme.accentColor)
                                .accessibilityHidden(true)
                        }
                    }

                    if profile.isAdaptive == true || profile.adaptiveTier?.isEmpty == false {
                        HStack(spacing: 6) {
                            if profile.isAdaptive == true {
                                ChatProfileTag(text: "Adaptive", color: theme.accentColor)
                            }
                            if let tier = profile.adaptiveTier, !tier.isEmpty {
                                ChatProfileTag(text: tier, color: chatProfileTierColor(tier, theme: theme))
                            }
                        }
                    }

                    if let model = profile.model, !model.isEmpty {
                        Text(model)
                            .font(.themedMono(11))
                            .foregroundColor(theme.secondaryTextColor)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    if let description = profile.adaptiveDescription, !description.isEmpty {
                        Text(description)
                            .font(.themed(12))
                            .foregroundColor(theme.secondaryTextColor)
                            .lineLimit(2)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .background(selected ? theme.accentColor.opacity(0.08) : Color.clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(profileAccessibilityLabel(profile))
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func profileAccessibilityLabel(_ profile: ChatProfile) -> String {
        [
            profile.name,
            profile.isDefault == true ? "default" : nil,
            profile.isAdaptive == true ? "adaptive" : nil,
            profile.adaptiveTier,
            profile.model
        ]
        .compactMap { $0 }
        .joined(separator: ", ")
    }
}

private struct ChatProfileTag: View {
    @ObservedObject private var theme = ThemeManager.shared

    let text: String
    let color: Color

    var body: some View {
        Text(text.uppercased())
            .font(theme.font(9, weight: .semibold))
            .foregroundColor(color)
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(color.opacity(0.12))
            .clipShape(Capsule())
            .overlay(Capsule().stroke(color.opacity(0.36), lineWidth: 1))
    }
}

private func chatProfileTierColor(_ tier: String, theme: ThemeManager) -> Color {
    switch tier.lowercased() {
    case "instant": return theme.successColor
    case "normal": return theme.infoColor
    case "advanced": return theme.discoveryColor
    default: return theme.secondaryTextColor
    }
}

enum MicInteractionAction: Equatable {
    case none
    case startTapDictation
    case finishTapDictation
    case startHold
    case finishHold
}

/// Pure interaction decisions used by the SwiftUI gesture and unit tests. In
/// particular, an already-running tap-to-dictate capture must never be restarted
/// as a hold when the user presses the mic to stop it.
enum MicInteractionPolicy {
    static let holdDelay: TimeInterval = 0.22

    static func usesDirectStop(isRecording: Bool, holdActive: Bool) -> Bool {
        isRecording && !holdActive
    }

    static func delayedPressAction(
        pressInProgress: Bool,
        recordingAtPressStart: Bool,
        holdActive: Bool
    ) -> MicInteractionAction {
        guard pressInProgress, !recordingAtPressStart, !holdActive else { return .none }
        return .startHold
    }

    static func releaseAction(
        holdActive: Bool,
        recordingAtPressStart: Bool
    ) -> MicInteractionAction {
        if holdActive { return .finishHold }
        return recordingAtPressStart ? .finishTapDictation : .startTapDictation
    }
}

/// One mic, two intentional gestures: a quick tap toggles dictation while a
/// deliberate hold starts after a short threshold and finishes on release.
/// VoiceOver stays tap-only because its activation gesture cannot express this
/// direct-touch hold reliably.
private struct MicInteraction: ViewModifier {
    @Binding var holding: Bool
    let isRecording: Bool
    let onTapStart: () -> Void
    let onTapStop: () -> Void
    let onHoldStart: () -> Void
    let onHoldEnd: () -> Void

    @State private var pressInProgress = false
    @State private var recordingAtPressStart = false
    @State private var pendingHold: DispatchWorkItem?

    func body(content: Content) -> some View {
        if UIAccessibility.isVoiceOverRunning {
            content.onTapGesture { performTap(recordingAtPressStart: isRecording) }
        } else {
            content.gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { _ in beginPressIfNeeded() }
                    .onEnded { _ in finishPress() }
            )
            .onDisappear { cancelPress() }
        }
    }

    private func beginPressIfNeeded() {
        guard !pressInProgress else { return }
        pressInProgress = true
        recordingAtPressStart = isRecording

        let work = DispatchWorkItem {
            guard MicInteractionPolicy.delayedPressAction(
                pressInProgress: pressInProgress,
                recordingAtPressStart: recordingAtPressStart,
                holdActive: holding
            ) == .startHold else { return }
            holding = true
            onHoldStart()
        }
        pendingHold = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + MicInteractionPolicy.holdDelay,
            execute: work
        )
    }

    private func finishPress() {
        pendingHold?.cancel()
        pendingHold = nil
        pressInProgress = false

        switch MicInteractionPolicy.releaseAction(
            holdActive: holding,
            recordingAtPressStart: recordingAtPressStart
        ) {
        case .startTapDictation:
            onTapStart()
        case .finishTapDictation:
            onTapStop()
        case .finishHold:
            holding = false
            onHoldEnd()
        case .none, .startHold:
            break
        }
    }

    private func performTap(recordingAtPressStart: Bool) {
        switch MicInteractionPolicy.releaseAction(
            holdActive: false,
            recordingAtPressStart: recordingAtPressStart
        ) {
        case .startTapDictation: onTapStart()
        case .finishTapDictation: onTapStop()
        case .none, .startHold, .finishHold: break
        }
    }

    private func cancelPress() {
        pendingHold?.cancel()
        pendingHold = nil
        pressInProgress = false
        if holding {
            holding = false
            onHoldEnd()
        }
    }
}

/// Recording is an explicit state boundary: the mic becomes a plain stop button
/// until finalization begins. Idle capture retains the tap-versus-hold gesture.
private struct ActiveDictationInteraction: ViewModifier {
    @Binding var holding: Bool
    let isRecording: Bool
    let onTapStart: () -> Void
    let onTapStop: () -> Void
    let onHoldStart: () -> Void
    let onHoldEnd: () -> Void

    func body(content: Content) -> some View {
        if MicInteractionPolicy.usesDirectStop(
            isRecording: isRecording,
            holdActive: holding
        ) {
            content.onTapGesture(perform: onTapStop)
        } else {
            content.modifier(MicInteraction(
                holding: $holding,
                isRecording: false,
                onTapStart: onTapStart,
                onTapStop: onTapStop,
                onHoldStart: onHoldStart,
                onHoldEnd: onHoldEnd
            ))
        }
    }
}
