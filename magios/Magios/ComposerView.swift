import SwiftUI

private extension SystemVoiceLaunchMode {
    var composerLabel: String { self == .dictation ? "Dictation" : label }
    var composerIcon: String {
        switch self {
        case .dictation: "mic.fill"
        case .realtime: "waveform"
        case .handsFree: "headphones"
        }
    }
}

/// Shared card and field styling for Chat and the VibeDev companion.
struct ComposerCard<Content: View>: View {
    var accented = false
    var privilegeLabel: String?
    let content: Content
    @ObservedObject private var theme = ThemeManager.shared

    init(accented: Bool = false, privilegeLabel: String? = nil, @ViewBuilder content: () -> Content) {
        self.accented = accented
        self.privilegeLabel = privilegeLabel
        self.content = content()
    }

    var body: some View {
        VStack(spacing: 0) {
            content
        }
            .background(theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: Lively.composerRadius))
            .background {
                if privilegeLabel != nil {
                    RoundedRectangle(cornerRadius: Lively.composerRadius + 4)
                        .fill(LinearGradient(colors: [theme.accentColor.opacity(0.34), theme.discoveryColor.opacity(0.26), theme.infoColor.opacity(0.24)], startPoint: .topLeading, endPoint: .bottomTrailing))
                        .padding(-4).blur(radius: 14).allowsHitTesting(false)
                }
            }
            .overlay(RoundedRectangle(cornerRadius: Lively.composerRadius)
                .stroke(accented || privilegeLabel != nil ? theme.accentColor.opacity(0.4) : theme.secondaryTextColor.opacity(0.2), lineWidth: 1))
            .padding(.horizontal, 10).padding(.top, 2).padding(.bottom, 6)
    }
}

struct ComposerInputStyle: ViewModifier {
    @ObservedObject private var theme = ThemeManager.shared

    func body(content: Content) -> some View {
        content.lineLimit(1...5).font(theme.font(16)).foregroundColor(theme.textColor)
            .keyboardType(.default).textInputAutocapitalization(.sentences).submitLabel(.return)
            .padding(.horizontal, 10).frame(minHeight: 36, alignment: .center)
    }
}

struct ComposerKeyboardToolbar: ToolbarContent {
    var onDismiss: () -> Void = {}
    var body: some ToolbarContent {
        ToolbarItemGroup(placement: .keyboard) {
            Spacer()
            Button {
                onDismiss()
                UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
            } label: { Label("Done", systemImage: "keyboard.chevron.compact.down") }
            .accessibilityLabel("Hide keyboard")
        }
    }
}

struct ComposerSelectorLabel: View {
    let title: String
    var icon = "sparkles"
    var highlighted = false
    var maxWidth: CGFloat = 180
    var showsCaret = true
    var truncationMode: Text.TruncationMode = .middle
    /// Selected thinking level tag (web `pp-badge`), e.g. "Auto" or "High"; nil hides it.
    var thinking: String? = nil
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: icon).font(.system(size: 12))
                .foregroundColor(highlighted ? theme.accentColor : theme.textColor)
            Text(title).font(.themedMono(10, weight: .medium)).lineLimit(1)
                .truncationMode(truncationMode).frame(maxWidth: maxWidth)
            if let thinking {
                Text(thinking.uppercased()).font(.themed(8, weight: .semibold)).tracking(0.3).lineLimit(1)
                    .foregroundColor(theme.accentColor)
                    .padding(.horizontal, 4)
                    .background(theme.accentColor.opacity(0.12))
                    .clipShape(RoundedRectangle(cornerRadius: 3, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 3, style: .continuous)
                        .stroke(theme.accentColor.opacity(0.28), lineWidth: 1))
                    .fixedSize()
            }
            if showsCaret { Image(systemName: "chevron.up.chevron.down").font(.system(size: 10)) }
        }
        .padding(.horizontal, 8).padding(.vertical, 6)
        .background(theme.surfaceColor).foregroundColor(theme.textColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
            .stroke(highlighted ? theme.accentColor.opacity(0.5) : theme.secondaryTextColor.opacity(0.2), lineWidth: 1))
    }
}

struct ComposerVoiceControlStyle: ViewModifier {
    var active = false
    @ObservedObject private var theme = ThemeManager.shared
    func body(content: Content) -> some View {
        content.foregroundColor(active ? theme.accentColor : theme.secondaryTextColor)
            .background(active ? theme.accentColor.opacity(0.12) : theme.secondaryTextColor.opacity(0.1))
            .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                .stroke(active ? theme.accentColor.opacity(0.4) : theme.secondaryTextColor.opacity(0.25), lineWidth: 1))
            .frame(height: 42)
    }
}

/// The same dictation/typing dock is used by Chat and VibeDev.
struct ComposerVoiceHero: View {
    let isRecording: Bool
    let isTranscribing: Bool
    let partialTranscript: String
    var inputMode: SystemVoiceLaunchMode = .dictation
    var available = true
    var archiveDictation = false
    var holdToSend = true
    var micIdentifier = "chat-voice-mic"
    var typingIdentifier = "chat-type-instead"
    let onTapDictationStart: () -> Void
    let onTapDictationStop: () -> Void
    let onHoldStart: () -> Void
    let onHoldEnd: () -> Void
    let onKeyboard: () -> Void
    let leading: AnyView
    let trailing: AnyView
    @ObservedObject private var themeManager = ThemeManager.shared
    @State private var holding = false
    @State private var pulse = false

    var body: some View {
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
            if archiveDictation {
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
                        .fill(isRecording ? themeManager.dangerColor : themeManager.accentFillColor)
                        .frame(width: 72, height: 72)
                        .shadow(color: (isRecording ? themeManager.dangerColor : themeManager.accentColor).opacity(0.35), radius: 10, y: 3)
                    Image(systemName: isTranscribing ? "waveform" : inputMode.composerIcon)
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
                    dictation: inputMode == .dictation,
                    onTapStart: onTapDictationStart,
                    onTapStop: onTapDictationStop,
                    onHoldStart: onHoldStart,
                    onHoldEnd: onHoldEnd
                ))
                .disabled(isTranscribing || (!isRecording && !available))
                .accessibilityLabel(isRecording ? "Stop dictation" : inputMode == .dictation ? "Talk" : "Start \(inputMode.composerLabel)")
                .accessibilityHint(isRecording
                    ? "Tap to finish dictation."
                    : inputMode == .dictation ? "Tap or hold to dictate. Release after holding to finish." : "Starts the selected \(inputMode.composerLabel) conversation.")
                .accessibilityAddTraits(.startsMediaSession)
                .accessibilityIdentifier(micIdentifier)
                .onChange(of: isRecording) { _, rec in pulse = rec }

                // Flanking controls overlaid so they never push the mic off-center.
                HStack {
                    leading
                    Spacer()
                    trailing
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
                    Button(action: { onKeyboard() }) {
                        Text("type instead")
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(themeManager.accentColor)
                    }
                    .accessibilityIdentifier(typingIdentifier)
                }
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 6)
    }

    private var micInteractionHint: String {
        if holding { return holdToSend ? "Listening — release to send" : "Listening — release to finish" }
        if isRecording { return "Listening — tap to finish" }
        if !available { return "\(inputMode.composerLabel) is unavailable" }
        if inputMode != .dictation { return "Tap to start \(inputMode.composerLabel)" }
        return archiveDictation
            ? "Tap or hold to dictate · recording is saved"
            : "Tap or hold to dictate"
    }

}

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
    static let cornerRadius: CGFloat = Lively.composerRadius
    /// The card's side inset. Everything stacked on the card (the ask and
    /// planner docks, the mention picker, the in-flow call panel) uses it so
    /// they share the card's width and left edge.
    static let horizontalInset: CGFloat = 10

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
    var executionChatSessionID: String? = nil
    var showChatGodMode = true
    var mentionItems: [ComposerMentionItem] = []
    var stagedAttachments: [StagedAttachment] = []
    var onRemoveAttachment: (UUID) -> Void = { _ in }
    var isRecording: Bool = false
    var dictationLevel: () -> Float = { 0 }
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
    /// `/` commands: what their option rows read, what a picked option runs,
    /// and the confirmation it posted.
    var commandContext: ComposerCommandContext = ComposerCommandContext()
    var onCommand: (ComposerCommandAction) -> Void = { _ in }
    var onCommandOpened: (ComposerCommandId) -> Void = { _ in }
    /// An engine's models opened under `/engine` or `/run-engine`.
    var onCommandLevelOpened: (ComposerCommandLevel) -> Void = { _ in }
    var commandNotice: ComposerCommandNotice? = nil
    var onCommandNoticeDone: () -> Void = {}

    /// VibeDev supplies its run context and modes while retaining Chat's input,
    /// attachment previews and voice settings controls.
    var topPanel: AnyView? = nil
    var modeControl: AnyView? = nil
    var modelControl: AnyView? = nil
    var inputPlaceholder: String? = nil
    var sendEnabled = true
    var holdToSend = true
    var supportsChatCommands = true
    var focusRequest: UUID? = nil
    var focusOnAppear = false

    @StateObject private var themeManager = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var privilegeLabel: String?
    @ObservedObject private var audio = AudioSettings.shared
    @ObservedObject private var primaryAgent = PrimaryAgentSiriAdvertiser.shared
    @FocusState private var isFocused: Bool
    /// Live `@…` matches for the picker (empty = picker hidden).
    @State private var mentionMatches: [ComposerMentionItem] = []
    /// Where the `/` picker is (command list, or a command's options).
    @State private var commandState = ComposerCommandState()
    /// When the Live pill is tapped while no realtime voice provider is configured,
    /// briefly surface an "unavailable" hint in place of the label.
    @State private var showLiveUnavailable = false
    @State private var showProfilePicker = false
    @State private var voiceSettingsSection: VoiceSettingsSection?

    private let voiceDockControlHeight: CGFloat = 42

    private var canSend: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || stagedAttachments.contains { $0.remoteId != nil }
    }

    private var canSubmit: Bool {
        sendEnabled && !stagedAttachments.contains { $0.uploading || $0.failed }
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
            if let commandNotice { commandNoticeView(commandNotice) }
            // The `/` command picker, else the @-mention picker, floats above the card.
            if !commandRows.isEmpty { commandPicker } else if !mentionMatches.isEmpty { mentionPicker }

            // The composer card.
            ComposerCard(accented: mode != .ask, privilegeLabel: privilegeLabel) {
              VStack(spacing: 0) {
                if let topPanel { topPanel.padding(.horizontal, 12).padding(.top, 8).padding(.bottom, 6) }
                if let voiceQueue { voiceQueue }
                VStack(alignment: .leading, spacing: 6) {
                    if !stagedAttachments.isEmpty { attachmentStrip }
                    if isRecording {
                        HStack(spacing: 10) {
                            Text("Listening").font(.themed(12)).foregroundColor(themeManager.secondaryTextColor)
                            VoiceActivityWaveform(level: dictationLevel)
                        }.frame(maxWidth: .infinity)
                            .transition(.opacity)
                            .accessibilityIdentifier("dictation-feedback")
                    }

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
                            if let modeControl { modeControl } else { modeToggle }
                            Divider().frame(height: 16).background(themeManager.secondaryTextColor.opacity(0.3))
                            ComposerExecutionModes(sessionID: executionChatSessionID, showChatMode: showChatGodMode)
                            toolButton("paperclip", action: onAttach)
                            toolButton("camera", action: onCamera)
                            // Voice controls (mute/auto-speak, Live, mic) live in the voice
                            // dock above in voice mode. In text mode they're gone from this
                            // row entirely: the mic moves into the trailing send slot (mic
                            // when the field is empty, Send once you type), and mute/Live are
                            // hidden — they're voice-only concerns.
                            if let modelControl { modelControl } else { profileChip }
                        }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.top, 10)
                .padding(.bottom, 6)
              }
            }
        }
        // No band behind the card: the chat's wash shows around it. The card's
        // own border carries the mode.
        // A draft restored with a trailing `@…` opens the picker straight away.
        .animation(reduceMotion ? nil : .smooth(duration: 0.24), value: isRecording)
        .animation(reduceMotion ? nil : .smooth(duration: 0.24), value: isTranscribing)
        .animation(reduceMotion ? nil : .smooth(duration: 0.24), value: partialTranscript)
        .animation(reduceMotion ? nil : .smooth(duration: 0.24), value: showVoiceHero)
        .onPreferenceChange(ComposerPrivilegePreference.self) { privilegeLabel = $0 }
        .onAppear {
            recomputeMentions()
            if focusOnAppear { isFocused = true }
        }
        .onChange(of: focusRequest) { _, _ in isVoiceMode = false; isFocused = true }
        .toolbar { ComposerKeyboardToolbar { isFocused = false } }
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
            .presentationContentInteraction(.scrolls)
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
            .presentationContentInteraction(.scrolls)
            .presentationDragIndicator(.visible)
            .presentationBackground(themeManager.backgroundColor)
        }
    }

    // MARK: - Voice-hero (primary in voice mode)

    /// The dominant tap-or-hold mic. A quick tap toggles dictation; a deliberate
    /// hold preserves walkie-talkie capture and sends when released.
    private var voiceHero: some View {
        ComposerVoiceHero(isRecording: isRecording, isTranscribing: isTranscribing, partialTranscript: partialTranscript,
            inputMode: audio.composerVoiceMode, available: composerModeAvailable(audio.composerVoiceMode),
            archiveDictation: audio.archiveChatDictation, holdToSend: holdToSend,
            onTapDictationStart: startComposerVoice, onTapDictationStop: onTapDictationStop,
            onHoldStart: onHoldStart, onHoldEnd: onHoldEnd,
            onKeyboard: { withAnimation { isVoiceMode = false }; isFocused = true },
            leading: AnyView(voiceOptionsControl), trailing: AnyView(liveButton))
    }

    /// The existing text composer: multi-line field + Send/Stop, unchanged, plus a
    /// small mic affordance that flips back to voice-hero.
    private var textComposerRow: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField(inputPlaceholder ?? placeholder, text: $text, axis: .vertical)
                .accessibilityIdentifier("chat-composer")
                .focused($isFocused)
                .onChange(of: text) {
                    if supportsChatCommands { ComposerCommands.sync(text: text, state: &commandState) }
                    recomputeMentions()
                }
                .modifier(ComposerInputStyle())
            // A command draft has one way out, the picker: none of these may send it.
            if canSend, mode != .plan, !(supportsChatCommands && ComposerCommands.holdsSend(text, state: commandState)) {
                sendOptionsMenu
            }
            trailingComposerButton
        }
    }

    private var sendOptionsMenu: some View {
        Menu {
            Button(isThinking ? "Queue message" : "Send message", action: send)
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
        .disabled(queueMutationInFlight || !canSubmit)
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

    /// The visible height of a mode segment; each one still takes a 44 pt
    /// tall hit area (HIG) without growing the pill.
    private static let modeSegmentHeight: CGFloat = 26
    /// Segments sit 2 pt inside the track, so their corner is a hair tighter
    /// than the track's chip radius: rounded squares, never capsules.
    private static let modeSegmentRadius: CGFloat = Lively.chipRadius - 1.5

    private var modeToggle: some View {
        HStack(spacing: 2) {
            doModeButton
            modeButton("PLAN", .plan)
        }
        .padding(.horizontal, 2)
        .background(
            RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                .fill(themeManager.softBackgroundColor)
                .frame(height: Self.modeSegmentHeight + 4)
        )
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Execution mode")
    }

    /// Do is a split control: its face restores the remembered permission and
    /// only its caret opens the Ask/Accept picker. Accept is a Do permission,
    /// not a third top-level mode. Selected, the face and its caret are ONE
    /// accent pill (one fill, no box per half).
    private var doModeButton: some View {
        let selected = mode != .plan
        let foreground = selected ? themeManager.onAccentColor : themeManager.secondaryTextColor

        return HStack(spacing: 0) {
            Button(action: activateRememberedDoMode) {
                HStack(spacing: 3) {
                    Text("DO")
                    Text("· \(doPermission.label.uppercased())")
                        .opacity(0.82)
                }
                .font(.themedMono(10, weight: .semibold))
                .tracking(0.6)
                .padding(.leading, 10)
                .padding(.trailing, 5)
                .foregroundColor(foreground)
                .frame(height: Self.modeSegmentHeight + 4)
                // The row stays compact; the touch target still reaches ~44 pt.
                .contentShape(Rectangle().inset(by: -10))
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(selected ? .isSelected : [])
            .accessibilityLabel("Do · \(doPermission.label)")

            Rectangle()
                .fill(foreground.opacity(0.3))
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
                    .padding(.leading, 6)
                    .padding(.trailing, 9)
                    .frame(height: Self.modeSegmentHeight + 4)
                    .contentShape(Rectangle().inset(by: -10))
            }
            .accessibilityLabel("Choose what Do asks before editing files")
        }
        .background(
            RoundedRectangle(cornerRadius: Self.modeSegmentRadius, style: .continuous)
                .fill(selected ? themeManager.accentFillColor : Color.clear)
                .frame(height: Self.modeSegmentHeight)
        )
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

    private func modeButton(_ label: String, _ value: ComposerMode) -> some View {
        ComposerModeSegment(label: label, selected: mode == value) { withAnimation { mode = value } }
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

    /// The right-hand selector chooses what the central voice button starts.
    private var liveButton: some View {
        Menu {
            ForEach(SystemVoiceLaunchMode.allCases) { mode in
                Button {
                    audio.composerVoiceMode = mode
                    if mode != .dictation { audio.liveVoiceEngine = mode == .realtime ? .realtime : .handsFree }
                } label: {
                    Label(mode.composerLabel + (audio.composerVoiceMode == mode ? " ✓" : ""), systemImage: mode.composerIcon)
                }.disabled(!composerModeAvailable(mode))
            }
            Divider()
            Button {
                voiceSettingsSection = audio.composerVoiceMode == .dictation ? .dictation : .session
            } label: {
                Label("Settings", systemImage: "gearshape")
            }
            .accessibilityIdentifier("voice-mode-settings")
        } label: {
            HStack(spacing: 0) {
                HStack(spacing: 4) {
                    Image(systemName: audio.composerVoiceMode.composerIcon).font(.system(size: 11))
                    Text(audio.composerVoiceMode.composerLabel).font(.themed(10, weight: .semibold))
                }.padding(.leading, 8).padding(.trailing, 6).frame(height: voiceDockControlHeight)
                Rectangle().fill(themeManager.secondaryTextColor.opacity(0.22)).frame(width: 1, height: 18)
                Image(systemName: "chevron.down").font(.system(size: 9, weight: .bold))
                    .frame(width: 27, height: voiceDockControlHeight)
            }
            .contentShape(Rectangle())
        }
        .accessibilityLabel("Voice mode: \(audio.composerVoiceMode.composerLabel)")
        .accessibilityHint("Choose Live, Dictation, Hands-free or Settings")
        .accessibilityIdentifier("voice-mode-selector")
        .buttonStyle(.plain)
        .modifier(ComposerVoiceControlStyle())
        .disabled(voiceCallActive || isRecording || isTranscribing)
    }

    private func composerModeAvailable(_ mode: SystemVoiceLaunchMode) -> Bool {
        switch mode {
        case .dictation: true
        case .realtime: liveEnabled && selectedRealtimeProfile?.available == true
        case .handsFree: handsFreeAvailable
        }
    }

    private func startComposerVoice() {
        guard composerModeAvailable(audio.composerVoiceMode) else { return }
        if audio.composerVoiceMode == .dictation { onTapDictationStart() }
        else {
            audio.liveVoiceEngine = audio.composerVoiceMode == .realtime ? .realtime : .handsFree
            startSelectedLiveMode()
        }
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
        .modifier(ComposerVoiceControlStyle(active: repliesEnabled))
    }

    /// Web parity (`ChatPanel` `profileThinkingLabel`): the chip carries the
    /// selected thinking level for a catalog model that has thinking.
    private var profileChipThinking: String? {
        let selection = activeChatProfile?.modelSelection
        let model = profiles.compactMap(\.modelChoice).first(where: { $0.id == selection?.modelId })
        return ChatThinkingControl.chatChipLabel(engine: selectedHarnessEngine, model: model, selection: selection)
    }

    private var profileChip: some View {
        Button(action: { showProfilePicker = true }) {
            ComposerSelectorLabel(
                title: "\(selectedHarnessEngine == "claude_code" ? "Claude Code" : selectedHarnessEngine.replacingOccurrences(of: "_", with: " ")) · \(selectedHarnessEngine == "magician" || selectedHarnessEngine == "pi" ? (activeChatProfile?.model ?? "Default") : selectedHarnessModel)",
                icon: isDefaultProfileSelected ? "sparkles" : "sparkles.rectangle.stack.fill",
                highlighted: !isDefaultProfileSelected,
                thinking: profileChipThinking)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(ChatThinkingControl.chipAccessibilityLabel("Chat engine: \(selectedHarnessEngine); model: \(selectedHarnessEngine == "magician" || selectedHarnessEngine == "pi" ? (activeChatProfile?.model ?? "Default") : selectedHarnessModel)", thinking: profileChipThinking))
        .accessibilityHint("Opens the chat engine and model picker")
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
                    .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
            }
            .accessibilityLabel("Stop")
            .accessibilityIdentifier("chat-stop")
        } else if canSend {
            Button(action: send) {
                Image(systemName: "arrow.up")
                    .font(.system(size: 18, weight: .semibold))
                    .foregroundColor(themeManager.onAccentColor)
                    .frame(width: 36, height: 36)
                    .background(themeManager.accentFillColor)
                    .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
            }
            .disabled(queueMutationInFlight || !canSubmit)
            .accessibilityLabel(isThinking ? "Queue message" : "Send")
            .accessibilityIdentifier("chat-send")
        } else if !voiceCallActive {
            // Empty field: the mic takes the send slot and flips to the voice-hero.
            Button(action: { isFocused = false; withAnimation { isVoiceMode = true } }) {
                Image(systemName: audio.composerVoiceMode.composerIcon)
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundColor(themeManager.onAccentColor)
                    .frame(width: 36, height: 36)
                    .background(themeManager.accentFillColor)
                    .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
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
        let shape = RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
        return ZStack(alignment: .topTrailing) {
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
            .clipShape(shape)
            .overlay(shape.stroke(themeManager.controlBorderColor, lineWidth: 1))
            .overlay {
                // Theme scrims, never a fixed black/white: the chip reads in
                // every palette, light or dark.
                if att.uploading {
                    ZStack {
                        themeManager.backgroundColor.opacity(0.55)
                        ProgressView().scaleEffect(0.7).tint(themeManager.textColor)
                    }
                    .clipShape(shape)
                    .accessibilityLabel("Uploading \(att.filename)")
                } else if att.failed {
                    ZStack {
                        themeManager.backgroundColor.opacity(0.6)
                        Image(systemName: "exclamationmark.triangle.fill").foregroundColor(themeManager.warningColor)
                    }
                    .clipShape(shape)
                    .accessibilityLabel("\(att.filename) failed to upload")
                }
            }

            Button(action: { onRemoveAttachment(att.id) }) {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 16))
                    .symbolRenderingMode(.palette)
                    .foregroundStyle(themeManager.textColor, themeManager.surfaceColor)
                    .background(Circle().stroke(themeManager.controlBorderColor, lineWidth: 0.5))
                    // A 44 pt hit area around the small glyph.
                    .frame(width: Lively.tapMin, height: Lively.tapMin)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .offset(x: 6 + (Lively.tapMin - 16) / 2, y: -6 - (Lively.tapMin - 16) / 2)
            .accessibilityLabel("Remove \(att.filename)")
        }
        .padding(.top, 6).padding(.trailing, 6)
    }

    // MARK: - @-mention picker

    private var mentionPicker: some View {
        // A short list hugs its rows; a long one scrolls inside 220 pt. (A
        // scroll view with only a max height is flexible: the stack handed
        // it the full 220 even for one row.)
        ScrollView { mentionRows }
            .frame(height: mentionListHeight)
        .background(themeManager.surfaceColor)
        // The card's own shape and inset: the picker's left edge lines up
        // with the composer card under it.
        .clipShape(RoundedRectangle(cornerRadius: Self.cornerRadius))
        .overlay(
            RoundedRectangle(cornerRadius: Self.cornerRadius)
                .stroke(themeManager.secondaryTextColor.opacity(0.2), lineWidth: 1)
        )
        .padding(.horizontal, Self.horizontalInset)
        .padding(.bottom, 6)
    }

    /// Row heights scale with Dynamic Type.
    @ScaledMetric(relativeTo: .body) private var mentionRowHeight: CGFloat = 36
    @ScaledMetric(relativeTo: .body) private var mentionDetailHeight: CGFloat = 16

    private var mentionListHeight: CGFloat {
        let rows = mentionMatches.reduce(CGFloat(0)) { total, item in
            total + mentionRowHeight + ((item.detail ?? "").isEmpty ? 0 : mentionDetailHeight)
        }
        return min(220, rows)
    }

    private var mentionRows: some View {
        VStack(spacing: 0) {
            ForEach(mentionMatches) { item in
                Button(action: { commitMention(item) }) {
                    HStack(spacing: 10) {
                        Text(mentionKindLabel(item.kind).uppercased())
                            .font(.themed(9, weight: .bold))
                            .padding(.horizontal, 6).padding(.vertical, 3)
                            .background(themeManager.accentColor.opacity(0.15))
                            .foregroundColor(themeManager.accentColor)
                            .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius - 2, style: .continuous))
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
                if item.id != mentionMatches.last?.id { StepHairline(leadingInset: 14) }
            }
        }
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

// MARK: - `/` command picker

extension ComposerView {
    var commandRows: [ComposerCommandRow] {
        ComposerCommands.rows(text: text, state: commandState, context: commandRowsContext)
    }

    /// The host's context plus `/voice-mode`'s, which this view owns: the
    /// mode and the same availability the voice-mode menu uses.
    private var commandRowsContext: ComposerCommandContext {
        var context = commandContext
        context.voiceMode = ComposerVoiceMode(rawValue: audio.composerVoiceMode.rawValue)
        context.voiceModesAvailable = Set(SystemVoiceLaunchMode.allCases.filter(composerModeAvailable)
            .compactMap { ComposerVoiceMode(rawValue: $0.rawValue) })
        return context
    }

    /// Send, unless the composer holds exactly a command: that opens the
    /// command's options instead. An open level (`/engine pi son`) keeps the
    /// picker. Commands never reach the model.
    func send() {
        guard canSend && canSubmit else { return }
        if supportsChatCommands,
           let id = ComposerCommands.exactCommand(text) ?? ComposerCommands.partialCommand(text, state: commandState) {
            openCommand(id)
            return
        }
        if supportsChatCommands, ComposerCommands.holdsSend(text, state: commandState) { return }
        onSend()
    }

    func openCommand(_ id: ComposerCommandId) {
        text = ComposerCommands.open(id, state: &commandState)
        onCommandOpened(id)
    }

    func pickCommandRow(_ row: ComposerCommandRow) {
        switch row.action {
        case .open(let id):
            openCommand(id)
        case .openModels(let engine):
            text = ComposerCommands.openModels(engine: engine, state: &commandState)
            if let level = commandState.stack.last { onCommandLevelOpened(level) }
        case .openThinking(let engine, let modelId):
            text = ComposerCommands.openThinking(engine: engine, modelId: modelId, state: &commandState)
        case .back:
            text = ComposerCommands.back(state: &commandState)
        case .toggleEngineScope:
            commandState.engineEverywhere.toggle()
        case .pickBudgetTask(let taskId, let title):
            text = ComposerCommands.pickTask(taskId: taskId, title: title, state: &commandState)
        case .none:
            break
        case .setChatEngine, .setRunEngine, .setLoopModel, .setVoiceMode, .grantBudget, .setBudgetDefault:
            commandState = ComposerCommandState()
            text = ""
            onCommand(row.action)
        }
    }

    var commandPicker: some View {
        ScrollView { commandRowsView }
            .frame(height: commandListHeight)
            .background(themeManager.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: Self.cornerRadius))
            .overlay(
                RoundedRectangle(cornerRadius: Self.cornerRadius)
                    .stroke(themeManager.secondaryTextColor.opacity(0.2), lineWidth: 1)
            )
            .padding(.horizontal, Self.horizontalInset)
            .padding(.bottom, 6)
            .accessibilityIdentifier("composer-command-picker")
    }

    private var commandListHeight: CGFloat {
        let rows = commandRows.reduce(CGFloat(0)) { total, row in
            total + 36 + ((row.detail ?? "").isEmpty ? 0 : 16) + (row.badge == "Note" ? 18 : 0)
        }
        return min(260, rows)
    }

    private var commandRowsView: some View {
        let rows = commandRows
        return VStack(spacing: 0) {
            ForEach(rows) { row in
                Button(action: { pickCommandRow(row) }) {
                    HStack(spacing: 10) {
                        if row.action == .back {
                            Image(systemName: "chevron.left")
                                .font(.system(size: 11, weight: .bold))
                                .foregroundColor(themeManager.accentColor)
                                .frame(width: 22)
                        }
                        Text(row.badge.uppercased())
                            .font(.themed(9, weight: .bold))
                            .padding(.horizontal, 6).padding(.vertical, 3)
                            .background(themeManager.accentColor.opacity(0.15))
                            .foregroundColor(themeManager.accentColor)
                            .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius - 2, style: .continuous))
                        VStack(alignment: .leading, spacing: 1) {
                            Text(row.label)
                                .font(.themed(14, weight: .medium))
                                .foregroundColor(row.action == .none ? themeManager.secondaryTextColor : themeManager.textColor)
                                // A note ("Now: Magician · GPT-6.1 Sol …") may need a second line.
                                .lineLimit(row.badge == "Note" ? 2 : 1)
                            if let detail = row.detail, !detail.isEmpty {
                                Text(detail)
                                    .font(.themed(11))
                                    .foregroundColor(themeManager.secondaryTextColor)
                                    .lineLimit(1)
                            }
                        }
                        Spacer()
                        if row.current {
                            Image(systemName: "checkmark.circle.fill")
                                .foregroundColor(themeManager.accentColor)
                                .accessibilityLabel("Current")
                        }
                    }
                    .padding(.horizontal, 14).padding(.vertical, 8)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(row.action == .none)
                .accessibilityIdentifier("composer-command-\(row.id)")
                if row.id != rows.last?.id { StepHairline(leadingInset: 14) }
            }
        }
    }

    func commandNoticeView(_ notice: ComposerCommandNotice) -> some View {
        let tint = notice.isError ? themeManager.dangerColor : themeManager.accentColor
        return Text(notice.text)
            .font(.themed(13, weight: .semibold))
            .foregroundColor(themeManager.contrastingTextColor(for: tint))
            .multilineTextAlignment(.center)
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .background(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous).fill(tint))
            .padding(.bottom, 6)
            .padding(.horizontal, Self.horizontalInset)
            .transition(.opacity)
            .task(id: notice.id) {
                UIAccessibility.post(notification: .announcement, argument: notice.text)
                try? await Task.sleep(nanoseconds: notice.isError ? 4_000_000_000 : 2_400_000_000)
                // A newer notice replaced this one: its own task clears it.
                guard !Task.isCancelled else { return }
                onCommandNoticeDone()
            }
            .accessibilityIdentifier("composer-command-notice")
    }
}

private struct PushedOption: Identifiable {
    let id: String
    let label: String
    let available: Bool
}

/// Hands a pushed destination its own `dismiss`, so choosing an option pops
/// back to the settings page rather than closing the whole sheet.
private struct PopOnSelect<Content: View>: View {
    @Environment(\.dismiss) private var dismiss
    let content: (@escaping () -> Void) -> Content

    var body: some View { content { dismiss() } }
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
                            ? theme.accentFillColor
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
            if audio.ttsEngine == .magician {
                audioSurfaceSelection(.dictation)
            } else {
                DeviceSpeechVoicePicker()
            }
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
        VoiceSelectionPicker(title: "Transcription", selected: audio.sttSource.id,
            options: STTSource.allCases.map { VoiceSelectionOption(id: $0.id, label: $0.label) }) { id in
            if let source = STTSource.allCases.first(where: { $0.id == id }) { audio.sttSource = source }
        }
    }

    private var ttsSelection: some View {
        VoiceSelectionPicker(title: "Reply voice", selected: audio.ttsEngine.id,
            options: TTSEngine.allCases.map { VoiceSelectionOption(id: $0.id, label: $0.label) }) { id in
            if let engine = TTSEngine.allCases.first(where: { $0.id == id }) { audio.ttsEngine = engine }
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
                    NavigationLink {
                        PopOnSelect { pop in
                            pushedOptionList(
                                title: "\(surface.label) pipeline",
                                rows: profiles.map {
                                    PushedOption(id: $0.id, label: $0.label, available: audio.isNativeProfileAvailable($0))
                                },
                                selectedID: activeProfile?.id,
                                identifier: "voice-\(surface.rawValue)-profile-options"
                            ) { id in
                                if let profile = profiles.first(where: { $0.id == id }) {
                                    audio.selectNativeProfile(profile)
                                }
                                pop()
                            }
                        }
                    } label: {
                        audioPickerLabel(
                            title: surface.label,
                            value: activeProfile?.label ?? "Unavailable",
                            icon: "slider.horizontal.3",
                            accessory: "chevron.right"
                        )
                    }
                    .buttonStyle(.plain)

                    ForEach(stages, id: \.rawValue) { stage in
                        settingsDivider
                        stageSelection(stage, surface: surface, profile: activeProfile)
                            .accessibilityIdentifier("voice-\(surface.rawValue)-\(stage.rawValue)")
                        if stage == .tts, let option = audio.effectiveStageOption(for: .tts, surface: surface) {
                            VoicePersonaControls(showRealtime: false, ttsProviderID: option.providerID)
                                .disabled(!option.available || voiceCallActive)
                                .padding(10)
                        }
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
        return NavigationLink {
            PopOnSelect { pop in
                stageOptionList(stage, surface: surface, options: options, selectedID: selectedID, pop: pop)
            }
        } label: {
            audioPickerLabel(
                title: stage.label,
                value: selectedLabel,
                icon: audioStageIcon(stage),
                accessory: "chevron.right"
            )
        }
        .buttonStyle(.plain)
        .disabled(options.isEmpty)
        .opacity(options.isEmpty ? 0.55 : 1)
    }

    private func stageOptionList(
        _ stage: NativeAudioStage,
        surface: NativeAudioSurface,
        options: [NativeAudioStageOption],
        selectedID: String?,
        pop: @escaping () -> Void
    ) -> some View {
        let profileOrder = PushedOption(id: "", label: "Profile order", available: true)
        return pushedOptionList(
            title: stage.label,
            rows: [profileOrder] + options.map {
                PushedOption(id: $0.id, label: $0.displayLabel, available: $0.available)
            },
            selectedID: selectedID ?? "",
            identifier: "voice-\(surface.rawValue)-\(stage.rawValue)-options"
        ) { id in
            audio.selectNativeStageOption(id.isEmpty ? nil : id, stage: stage, surface: surface)
            pop()
        }
    }

    /// Every voice selector pushes this list instead of opening a `Menu`:
    /// provider catalogs outgrew popup menus, which cannot scroll to their
    /// lower entries when anchored in a medium-detent sheet.
    private func pushedOptionList(
        title: String,
        rows: [PushedOption],
        selectedID: String?,
        identifier: String,
        onSelect: @escaping (String) -> Void
    ) -> some View {
        VoiceSelectionList(title: title, selected: selectedID ?? "",
            options: rows.map { VoiceSelectionOption(id: $0.id, label: $0.label, available: $0.available) },
            dismissOnSelect: false) { id in onSelect(id) }
            .accessibilityIdentifier(identifier)
    }

    private func audioPickerLabel(
        title: String,
        value: String,
        icon: String,
        accessory: String = "chevron.up.chevron.down"
    ) -> some View {
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
            Image(systemName: accessory)
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
                VoicePersonaControls(showTTS: false, realtimeProfileID: audio.realtimeVoiceProfile)
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
            .background(selected ? theme.accentFillColor : Color.clear)
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
                VoiceSelectionPicker(title: "Realtime model", selected: audio.realtimeVoiceProfile,
                    options: audio.realtimeVoiceProfiles.map { VoiceSelectionOption(id: $0.id, label: $0.label, available: $0.available) }) { id in
                    if let profile = audio.realtimeVoiceProfiles.first(where: { $0.id == id }) { _ = audio.selectRealtimeVoiceProfile(profile) }
                }
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
                .background(selected ? theme.accentFillColor : Color.clear)
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
                .background(theme.accentFillColor)
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
            .clipShape(RoundedRectangle(cornerRadius: 3, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 3, style: .continuous).stroke(color.opacity(0.32), lineWidth: 1))
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

    private var models: [ChatModelChoice] { profiles.compactMap(\.modelChoice) }
    private var selection: ChatModelSelection? {
        profiles.first(where: { $0.name == selectedProfile })?.modelSelection
            ?? profiles.first(where: { $0.isDefault == true })?.modelSelection
    }
    private var activeModel: ChatModelChoice? { models.first(where: { $0.id == selection?.modelId }) }

    private func selectModel(_ id: String, thinking: String? = nil) {
        guard let model = models.first(where: { $0.id == id }) else { return }
        if let route = ComposerModelCatalogControls.route(for: model, selection: selection, thinking: thinking) { selectedProfile = route }
    }

    private var modelControls: some View {
        ComposerModelCatalogControls(models: models, selection: selection, theme: theme) { id, level in
            selectModel(id, thinking: level)
        }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ModelCatalogPicker(title: "Engine", selected: selectedHarnessEngine,
                        options: chatHarnesses.map { VoiceSelectionOption(id: $0.name, label: HarnessCredentialsLabel.engineName(
                            Self.engineLabel($0.name), engine: $0.name, credentials: $0.credentials)) }) {
                        selectedHarnessEngine = $0
                    }
                    if selectedHarnessEngine == "magician" || selectedHarnessEngine == "pi" {
                        if !models.isEmpty {
                            modelControls
                        } else {
                            Text("Model choices are unavailable.")
                                .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                        }
                    } else {
                        let models = chatHarnesses.first(where: { $0.name == selectedHarnessEngine })?.availableModels ?? ["default"]
                        ModelCatalogPicker(title: "Model", selected: selectedHarnessModel,
                            options: models.map { VoiceSelectionOption(id: $0, label: $0 == "default" ? "Engine default" : $0) }) {
                            selectedHarnessModel = $0
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }
            .background(theme.backgroundColor)
            .navigationTitle("Chat engine and model")
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

    private static func engineLabel(_ id: String) -> String {
        ["magician": "Magician", "pi": "Pi", "claude_code": "Claude Code",
         "codex": "Codex", "codex_app_server": "Codex App Server", "grok": "Grok", "agy": "Antigravity"][id]
            ?? id.replacingOccurrences(of: "_", with: " ")
    }
}

struct ComposerModelCatalogControls: View {
    let models: [ChatModelChoice]
    let selection: ChatModelSelection?
    @ObservedObject var theme: ThemeManager
    let onSelect: (String, String?) -> Void
    private var activeModel: ChatModelChoice? { models.first(where: { $0.id == selection?.modelId }) }
    static func route(for model: ChatModelChoice, selection: ChatModelSelection?, thinking: String? = nil) -> String? {
        model.choices[thinking ?? selection?.thinking ?? "auto"] ?? model.choices["auto"]
    }


    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ModelCatalogPicker(title: "Model", selected: activeModel?.id ?? "",
                options: models.map { VoiceSelectionOption(id: $0.id, label: $0.label, detail: $0.providerDisplayName, available: $0.installed != false) }) {
                onSelect($0, nil)
            }
            if let model = activeModel {
                Text("\(model.providerDisplayName)\(model.limits.map { " · \($0.contextWindowTokens / 1000)K context" } ?? "")")
                    .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                ModelThinkingControl(control: model.thinking, level: selection?.thinking ?? "auto", theme: theme) { onSelect(model.id, $0) }
            }
        }
        .padding(16).background(theme.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }

}

/// A themed box opens a full-height, searchable list. Freeze the choices when
/// opening it so parent catalog/selection refreshes cannot reset an active scroll.
struct ModelCatalogPicker: View {
    private struct PresentedChoices: Identifiable {
        let id = UUID()
        let selected: String
        let options: [VoiceSelectionOption]
    }

    let title: String
    let selected: String
    let options: [VoiceSelectionOption]
    var showsLabel = true
    let onSelect: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared
    @State private var presentedChoices: PresentedChoices?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if showsLabel {
                Text(title).font(.themed(11, weight: .bold)).foregroundColor(theme.secondaryTextColor)
            }
            Button {
                presentedChoices = PresentedChoices(selected: selected, options: options)
            } label: {
                HStack(spacing: 10) {
                    Text(options.first(where: { $0.id == selected })?.label ?? (selected.isEmpty ? "Choose \(title.lowercased())" : "Unavailable choice · saved"))
                        .font(.themed(14)).foregroundColor(theme.textColor).lineLimit(1)
                    Spacer(minLength: 8)
                    Image(systemName: "chevron.down").font(.system(size: 11, weight: .semibold))
                        .foregroundColor(theme.secondaryTextColor)
                }
                .padding(.horizontal, 12).frame(minHeight: 44)
                .background(theme.controlColor, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).stroke(theme.controlBorderColor, lineWidth: 1))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain).disabled(options.isEmpty)
            .accessibilityLabel(title)
            .accessibilityValue(options.first(where: { $0.id == selected })?.label ?? selected)
        }
        .sheet(item: $presentedChoices) { choices in
            NavigationStack {
                VoiceSelectionList(title: title, selected: choices.selected, options: choices.options) { id in
                    onSelect(id)
                }
                .toolbar { ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { presentedChoices = nil }
                        .foregroundColor(theme.accentColor)
                        .buttonStyle(.bordered)
                        .buttonBorderShape(.roundedRectangle(radius: 8))
                }.roundedSquareToolbarBackground() }
            }
            .presentationDetents([.large])
            .presentationContentInteraction(.scrolls)
            .presentationDragIndicator(.visible)
            .presentationBackground(theme.backgroundColor)
            .preferredColorScheme(theme.forcedColorScheme)
        }
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
    var dictation = true
    let onTapStart: () -> Void
    let onTapStop: () -> Void
    let onHoldStart: () -> Void
    let onHoldEnd: () -> Void

    func body(content: Content) -> some View {
        if !dictation && !isRecording {
            content.onTapGesture(perform: onTapStart)
        } else if MicInteractionPolicy.usesDirectStop(
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

/// Native stepped or binary control shared by chat and crew model assignments.
struct ModelThinkingControl: View {
    let control: ChatThinkingControl
    let level: String
    @ObservedObject var theme: ThemeManager
    let onSelect: (String) -> Void
    @State private var previewIndex: Double?
    private var selectedIndex: Int {
        control.levels.firstIndex(of: level == "auto" ? control.defaultLevel ?? "" : level) ?? 0
    }
    private var displayedLevel: String {
        guard let previewIndex, !control.levels.isEmpty else { return level }
        return control.levels[min(max(Int(previewIndex.rounded()), 0), control.levels.count - 1)]
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
                if control.mode != "none", !control.levels.isEmpty {
                    HStack {
                        Text("Thinking").font(.themed(13, weight: .semibold))
                        Spacer()
                        Button("Auto") { previewIndex = nil; onSelect("auto") }
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(displayedLevel == "auto" ? theme.accentColor : theme.textColor)
                            .padding(.horizontal, 10).padding(.vertical, 7)
                            .background(displayedLevel == "auto" ? theme.accentColor.opacity(0.14) : theme.controlColor,
                                        in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                            .overlay(RoundedRectangle(cornerRadius: 6, style: .continuous)
                                .stroke(displayedLevel == "auto" ? theme.accentColor : theme.controlBorderColor, lineWidth: 1))
                            .buttonStyle(.plain)
                            .accessibilityAddTraits(displayedLevel == "auto" ? .isSelected : [])
                    }
                    Text(displayedLevel == "auto" ? (control.defaultLevel.map { "Default · \(ChatThinkingControl.label($0))" } ?? "Engine default") : ChatThinkingControl.label(displayedLevel))
                        .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                    if control.mode == "toggle" {
                        Toggle("Thinking enabled", isOn: Binding(
                            get: { (level == "auto" ? control.defaultLevel : level) != "none" },
                            set: { onSelect($0 ? "on" : "none") }
                        )).tint(theme.accentColor)
                    } else if control.levels.count > 1 {
                        Slider(value: Binding(
                            get: { previewIndex ?? Double(selectedIndex) },
                            set: { previewIndex = $0 }
                        ), in: 0...Double(control.levels.count - 1), step: 1, onEditingChanged: { editing in
                            if !editing, previewIndex != nil {
                                onSelect(displayedLevel)
                                previewIndex = nil
                            }
                        })
                        .tint(theme.accentColor)
                        .accessibilityLabel("Thinking level")
                        .accessibilityValue(displayedLevel == "auto" ? (control.defaultLevel.map { "Auto · \(ChatThinkingControl.label($0))" } ?? "Auto · engine default") : ChatThinkingControl.label(displayedLevel))
                        .accessibilityAdjustableAction { direction in
                            let next = direction == .increment ? selectedIndex + 1 : selectedIndex - 1
                            previewIndex = nil
                            onSelect(control.levels[min(max(next, 0), control.levels.count - 1)])
                        }
                        HStack(spacing: 2) {
                            ForEach(control.levels, id: \.self) { option in
                                Button {
                                    previewIndex = nil
                                    onSelect(option)
                                } label: {
                                    Text(ChatThinkingControl.label(option))
                                        .font(.themed(11, weight: displayedLevel == option ? .semibold : .regular))
                                        .lineLimit(1).minimumScaleFactor(0.7)
                                        .frame(maxWidth: .infinity, minHeight: 36)
                                        .foregroundColor(displayedLevel == option ? theme.accentColor : theme.secondaryTextColor)
                                        .background(displayedLevel == option ? theme.accentColor.opacity(0.12) : Color.clear,
                                                    in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("Set thinking to \(ChatThinkingControl.label(option))")
                                .accessibilityAddTraits(displayedLevel == option ? .isSelected : [])
                            }
                        }
                    }
                    Text("Higher thinking can improve difficult answers and take more time.")
                        .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                }
        }
        .onChange(of: level) { _, _ in previewIndex = nil }
        .onChange(of: control.levels) { _, _ in previewIndex = nil }
    }
}

/// Shared dimensions, typography, selection and accessibility for composer modes.
struct ComposerModeSegment: View {
    let label: String
    let selected: Bool
    let onSelect: () -> Void
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        Button(action: onSelect) {
            Text(label).font(.themedMono(10, weight: .semibold)).tracking(0.6).padding(.horizontal, 10)
                .foregroundStyle(selected ? theme.onAccentColor : theme.secondaryTextColor)
                .background(RoundedRectangle(cornerRadius: Lively.chipRadius - 1.5, style: .continuous)
                    .fill(selected ? theme.accentFillColor : .clear).frame(height: 26))
                .frame(height: 30).contentShape(Rectangle().inset(by: -10))
        }.buttonStyle(.plain).accessibilityLabel(label).accessibilityAddTraits(selected ? .isSelected : [])
    }
}
