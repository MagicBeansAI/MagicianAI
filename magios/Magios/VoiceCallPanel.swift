import SwiftUI

struct VoiceCaptionViewportPolicy {
    static let height: CGFloat = 72

    static func shouldFollowLatest(contentHeight: CGFloat) -> Bool {
        contentHeight > height + 1
    }
}

private struct VoiceCaptionContentHeightKey: PreferenceKey {
    static var defaultValue: CGFloat = 0

    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

/// The floating Live-call card (Task 5) — the iOS mirror of the web
/// `VoiceCallOverlay.svelte`. Pinned near the bottom of the screen over the
/// chat (not full-screen), so the thread stays visible behind it.
///
/// Renders a mic-reactive engine switch, a fixed-height rolling caption view,
/// the mm:ss timer, a red end button, and the mic controls: an
/// [Open mic | Hold to talk] segmented control above a mic button shaped by
/// the selected mode.
/// All styling reads the shared `ThemeManager` so the card matches the active
/// theme.
///
/// This is device-verified UI: verification in CI is "compiles + existing tests
/// stay green". The wiring that *starts* a call lives in Task 6 (the composer
/// "Live" pill); this panel only renders while a call is active.
struct VoiceCallPanel: View {
    @ObservedObject var viewModel: VoiceCallViewModel
    /// From the `/media/providers` probe: false means the backend has no
    /// complete cascaded pipeline, so the Hands-free engine is not offered.
    var handsFreeAvailable: Bool = true
    var onTypeMessage: (() -> Void)? = nil
    /// In the chat the panel takes the composer's slot (the
    /// transcript lifts; the card's queue / concurrent rows stay visible), on
    /// the card's inset and radius. Floating elsewhere.
    var inFlow = false
    var topRows: AnyView? = nil
    @ObservedObject private var theme = ThemeManager.shared
    @ObservedObject private var audioSettings = AudioSettings.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var beaconPulsing = false
    @State private var captionContentHeight: CGFloat = 0
    private var assistantName: String {
        let name = PrimaryAgentSiriIdentityStore.load()?.name
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return name.isEmpty ? MagicianAccess.assistantFallbackName : name
    }

    var body: some View {
        VStack(spacing: 12) {
            if let topRows { topRows }
            header
            if let error = viewModel.errorMessage {
                errorRow(error)
            }
            if let notice = viewModel.guidedFlowNoticeMessage {
                errorRow(notice)
            }
            if !viewModel.captions.isEmpty {
                captionsList
            } else {
                hint
            }
            controlRow
        }
        .padding(inFlow ? 12 : 16)
        .frame(maxWidth: inFlow ? .infinity : 420)
        .background(
            RoundedRectangle(cornerRadius: inFlow ? Lively.composerRadius : 18, style: .continuous)
                .fill(theme.cardColor)
        )
        .overlay(
            RoundedRectangle(cornerRadius: inFlow ? Lively.composerRadius : 18, style: .continuous)
                .stroke(theme.cardBorderColor, lineWidth: 1)
        )
        .shadow(color: .black.opacity(inFlow ? 0 : 0.22), radius: 18, y: 8)
        .padding(.horizontal, inFlow ? ComposerView.horizontalInset : 12)
        .padding(.bottom, inFlow ? 6 : 12)
        .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
    }

    // MARK: - Header (orb + status + timer + end)

    private var header: some View {
        HStack(spacing: 12) {
            orb
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Text("\(viewModel.voiceEngine.displayName) voice")
                        .font(.themed(14, weight: .bold))
                        .foregroundColor(theme.textColor)
                    Text(viewModel.elapsedText)
                        .font(.themedMono(11, weight: .medium))
                        .foregroundColor(theme.secondaryTextColor)
                }
                Text(statusText)
                    .font(.themed(12))
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if let onTypeMessage {
                Button(action: onTypeMessage) {
                    Image(systemName: "keyboard").font(.system(size: 17))
                        .foregroundColor(theme.secondaryTextColor).frame(width: 34, height: 34)
                }.buttonStyle(.plain).accessibilityLabel("Type a message")
                    .accessibilityIdentifier("live-type-message")
            }
            endButton
                .offset(y: -4)
        }
    }

    /// The status line names the mic mode ("Open mic · Listening",
    /// "Hold to talk · Ready") so the mode reads at a glance. Phases other
    /// than ready (connecting, errors) keep the view model's wording.
    private var statusText: String {
        let base = viewModel.statusText
        guard viewModel.client.phase == .ready else { return base }
        let mode = viewModel.pttOn ? "Hold to talk" : "Open mic"
        // In Hold to talk, muted == not held.
        if viewModel.pttOn && !viewModel.muted { return "\(mode) · Sending" }
        if viewModel.client.assistantWorking { return "\(mode) · \(base)" }
        if viewModel.pttOn {
            // Keep the address-phrase variant ("Hold, then say …").
            return viewModel.voiceAddressPhrase == nil ? "\(mode) · Ready" : "\(mode) · \(base)"
        }
        if viewModel.muted { return "\(mode) · Muted" }
        // "Listening" or "Listening for …".
        return "\(mode) · \(base)"
    }

    /// Continuously pulsing, mic-reactive engine switch, drawn from the shared
    /// aurora kit — the palette presets and `AuroraBlobShape` — so the in-app
    /// beacon matches every other brand surface, and the only one where the
    /// blob truly MORPHS: in-process there is a real render loop, so the
    /// silhouette drifts continuously and deepens with the microphone.
    /// The old rounded-square shape carried the button affordance; that now
    /// rides on the engine glyph and the call-UI convention of round controls.
    private var orb: some View {
        let level = CGFloat(viewModel.level)
        let blocked = !handsFreeAvailable && viewModel.voiceEngine == .realtime
        return Button {
            viewModel.setVoiceEngine(viewModel.voiceEngine == .realtime ? .handsFree : .realtime)
        } label: {
            ZStack {
                // In-process, so this surface gets what the Live Activity cannot: a
                // true repeatForever breathing halo and mic-level reactivity.
                Circle()
                    .fill(beaconPalette.halo)
                    .frame(width: 40, height: 40)
                    .blur(radius: 8)
                    .opacity(reduceMotion ? 0.5 : (beaconPulsing ? 0.85 : 0.4))
                    .scaleEffect(reduceMotion ? 1 : (beaconPulsing ? 1.15 : 0.9))
                    .animation(reduceMotion ? nil : .easeInOut(duration: 0.9).repeatForever(autoreverses: true),
                               value: beaconPulsing)
                // The one aurora surface with a real render loop, so the one
                // that earns a TRUE morph: `TimelineView(.animation)` runs only
                // while the panel is visible, and each frame costs one Path of
                // 180 points — cheap. The layered still orb the other surfaces
                // draw is replaced by the bare morphing blob on purpose: the
                // breathing halo above keeps the glow, the engine glyph keeps
                // its contrast (the palette luminance test holds every body
                // stop dark enough for white), and a sphere highlight or rim
                // would fight a silhouette that never stops moving.
                //
                // Reduce Motion pauses the schedule (no per-frame wakeups) and
                // freezes the drift at the still silhouette, but the mic-level
                // amplitude below keeps working: lobe depth is information,
                // not decoration.
                TimelineView(.animation(paused: reduceMotion)) { timeline in
                    // 0.6 rad/s: the lobes take ~10s to slide around the rim —
                    // a drift, not a spin.
                    let morph = reduceMotion
                        ? 0 : timeline.date.timeIntervalSinceReferenceDate * 0.6
                    // Voice deepens the lobes AND the whole orb still swells
                    // via the scaleEffect below — kept deliberately: the
                    // silhouette change is new, the swell is the reading this
                    // beacon already trained, and each is too small alone to
                    // carry the voice.
                    AuroraBlobShape(seed: beaconPalette.blobSeed,
                                    morph: morph,
                                    amplitude: 0.10 + Double(min(max(level, 0), 1)) * 0.15)
                        .fill(beaconPalette.bodyGradient)
                        .frame(width: 34, height: 34)
                }
                .opacity(0.82 + Double(min(level, 0.5)) * 0.36)
                .scaleEffect(1 + level * 0.16)
                .animation(.easeOut(duration: 0.12), value: viewModel.level)
                Image(systemName: viewModel.voiceEngine == .realtime ? "waveform" : "ear.fill")
                    .font(.system(size: 12, weight: .bold))
                    // White holds over every preset: the aurora body is never light.
                    .foregroundColor(.white)
                    .shadow(color: .black.opacity(0.35), radius: 1)
            }
            .frame(width: 44, height: 44)
            .contentShape(Circle())
            // Palette changes (mute toggles, connect → ready) land over the
            // kit's shared phase ease, like every other aurora surface. Colour
            // still carries the change; Reduce Motion drops only the ease,
            // matching the ambient glyph.
            .animation(reduceMotion ? nil : .auroraPhaseEase, value: beaconPalette)
        }
        .buttonStyle(.plain)
        .disabled(blocked)
        .opacity(blocked ? 0.55 : 1)
        .onAppear { beaconPulsing = true }
        .onDisappear { beaconPulsing = false }
        .accessibilityLabel("Voice engine: \(viewModel.voiceEngine.displayName)")
        .accessibilityHint(blocked
            ? "Hands-free voice is not configured on the backend"
            : "Switches to \(viewModel.voiceEngine == .realtime ? VoiceEngine.handsFree.displayName : VoiceEngine.realtime.displayName)")
    }

    private var endButton: some View {
        Button(action: { viewModel.hangUp() }) {
            Image(systemName: "xmark")
                .font(.system(size: 15, weight: .bold))
                .foregroundColor(theme.contrastingTextColor(for: theme.dangerColor))
                .frame(width: 34, height: 34)
                .background(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .fill(theme.dangerColor)
                )
        }
        .buttonStyle(.plain)
        .accessibilityLabel("End voice call")
    }

    // MARK: - Error

    private func errorRow(_ message: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 11))
            Text(message)
                .font(.themed(12))
                .lineLimit(2)
        }
        .foregroundColor(theme.dangerColor)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(theme.dangerColor.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    }

    // MARK: - Captions

    private var captionsList: some View {
        // Keep enough scrollback for a quick glance while limiting the visible
        // viewport to roughly four text lines. The durable transcript remains
        // in the chat thread behind this panel.
        let recent = Array(viewModel.captions.suffix(24))
        return ScrollViewReader { proxy in
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 5) {
                    ForEach(recent) { caption in
                        (Text("\(caption.role == .user ? "You" : assistantName)  ")
                            .font(.system(size: 10, weight: .bold))
                            .foregroundColor(caption.role == .user
                                             ? theme.accentColor
                                             : theme.secondaryTextColor)
                         + Text(caption.text)
                            .font(.themed(13))
                            .foregroundColor(theme.textColor)
                        )
                        .opacity(caption.isFinal ? 1 : 0.72)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .id(caption.id)
                    }
                }
                .padding(.vertical, 2)
                .frame(
                    minHeight: VoiceCaptionViewportPolicy.height,
                    alignment: .top
                )
                .background(
                    GeometryReader { geometry in
                        Color.clear.preference(
                            key: VoiceCaptionContentHeightKey.self,
                            value: geometry.size.height
                        )
                    }
                )
            }
            .frame(height: VoiceCaptionViewportPolicy.height)
            .onAppear {
                // A new call must not inherit overflow state from the prior
                // transcript and immediately pin its first caption to bottom.
                captionContentHeight = 0
            }
            .onChange(of: viewModel.captions.count) {
                guard VoiceCaptionViewportPolicy.shouldFollowLatest(
                    contentHeight: captionContentHeight
                ) else { return }
                DispatchQueue.main.async {
                    scrollCaptionsToLatest(recent, proxy: proxy, animated: true)
                }
            }
            .onChange(of: viewModel.captions.last?.text) {
                guard VoiceCaptionViewportPolicy.shouldFollowLatest(
                    contentHeight: captionContentHeight
                ) else { return }
                DispatchQueue.main.async {
                    scrollCaptionsToLatest(recent, proxy: proxy, animated: false)
                }
            }
            .onPreferenceChange(VoiceCaptionContentHeightKey.self) { height in
                captionContentHeight = height
                guard VoiceCaptionViewportPolicy.shouldFollowLatest(
                    contentHeight: height
                ) else { return }
                DispatchQueue.main.async {
                    scrollCaptionsToLatest(recent, proxy: proxy, animated: false)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func scrollCaptionsToLatest(
        _ captions: [RealtimeVoiceCaption],
        proxy: ScrollViewProxy,
        animated: Bool
    ) {
        guard let lastID = captions.last?.id else { return }
        if animated {
            withAnimation(.easeOut(duration: 0.2)) {
                proxy.scrollTo(lastID, anchor: .bottom)
            }
        } else {
            proxy.scrollTo(lastID, anchor: .bottom)
        }
    }

    private var hint: some View {
        Text(hintText)
            .font(.themed(12))
            .foregroundColor(theme.secondaryTextColor)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .frame(height: VoiceCaptionViewportPolicy.height)
    }

    private var hintText: String {
        switch viewModel.client.phase {
        case .connecting:
            return "Setting up voice…"
        case .reconnecting:
            return "Restoring the voice connection…"
        case .rotating:
            return "Refreshing the voice session — hang on a moment."
        default:
            // Shown for an untrusted call only, and deliberately not actionable:
            // there is no control here to raise the boundary, because a shared
            // room cannot ask to be trusted. It leads because it changes what
            // the call IS, which matters more than how to address it.
            if let boundary = viewModel.client.boundary, boundary.isUntrusted {
                return "Shared room — answering as the outward agent, without access to your private context."
            }
            if let phrase = viewModel.voiceAddressPhrase {
                return viewModel.pttOn
                    ? "Hold the button, then start with \"\(phrase)\"."
                    : "Start with \"\(phrase)\". The address phrase is left out of the message."
            }
            return viewModel.pttOn ? "Hold the button and talk." : "Start talking. Captions will appear here."
        }
    }

    /// The in-app beacon's palette follows the call, coarsely: connecting states
    /// surge, a dead call goes graphite, muted rests, live listens. Same presets
    /// as every other surface.
    private var beaconPalette: AuroraPalette {
        switch viewModel.client.phase {
        case .connecting, .reconnecting, .rotating: return .violetSurge
        // Graphite's halo is `.clear`, so the breathing overlay fills a fully
        // transparent colour — opacity multiplies an alpha of zero — and the
        // glow genuinely extinguishes rather than merely dimming.
        case .failed: return .graphite
        default: return viewModel.muted ? .armedEmber : .calmAurora
        }
    }

    // MARK: - Controls

    /// Mic mode on top, the mode-shaped mic control centred under it, so the
    /// active mode is always visible and the button's shape agrees with it:
    /// a round mute toggle for Open mic, a wide hold capsule for Hold to talk.
    private var controlRow: some View {
        VStack(spacing: 10) {
            micModeControl
            VoiceActivityWaveform {
                let active = viewModel.client.phase == .ready || viewModel.client.phase == .rotating
                return active ? max(viewModel.playbackLevel, viewModel.muted ? 0 : viewModel.level) : 0
            }
            if viewModel.pttOn {
                holdToTalkButton
            } else {
                openMicButton
            }
        }
        .frame(maxWidth: .infinity)
    }

    /// Translation profiles need server VAD, so Hold to talk is shown but
    /// disabled there; Open mic always stays selectable.
    private var holdToTalkAvailable: Bool {
        let selectedProfile = audioSettings.realtimeVoiceProfiles.first {
            $0.id == viewModel.realtimeProfile
        }
        return viewModel.voiceEngine == .handsFree
            || selectedProfile?.isTranslation != true
    }

    /// Two-segment mic-mode control, same vocabulary on iOS, Android and web:
    /// [Open mic | Hold to talk]. The active segment is accent-filled; tapping
    /// it does nothing, tapping the other one switches mode.
    private var micModeControl: some View {
        let holdEnabled = viewModel.pttOn || holdToTalkAvailable
        return HStack(spacing: 2) {
            micModeSegment(
                "Open mic", icon: "mic.fill",
                selected: !viewModel.pttOn, enabled: true,
                hint: "Switches to Open mic",
                identifier: "live-mic-mode-open"
            ) { viewModel.setHoldToTalk(false) }
            micModeSegment(
                "Hold to talk", icon: "hand.tap.fill",
                selected: viewModel.pttOn, enabled: holdEnabled,
                hint: holdEnabled
                    ? "Switches to Hold to talk"
                    : "Unavailable: the selected translation voice needs Open mic",
                identifier: "live-mic-mode-hold"
            ) { viewModel.setHoldToTalk(true) }
        }
        .padding(2)
        .frame(maxWidth: 300)
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(theme.secondaryTextColor.opacity(0.3), lineWidth: 1)
        )
        .animation(reduceMotion ? nil : .easeOut(duration: 0.15), value: viewModel.pttOn)
    }

    private func micModeSegment(
        _ title: String,
        icon: String,
        selected: Bool,
        enabled: Bool,
        hint: String,
        identifier: String,
        action: @escaping () -> Void
    ) -> some View {
        Button {
            guard !selected else { return }
            action()
        } label: {
            Label(title, systemImage: icon)
                .font(.themed(12, weight: selected ? .semibold : .medium))
                .lineLimit(1)
                .foregroundColor(selected ? theme.onAccentColor : theme.secondaryTextColor)
                .frame(maxWidth: .infinity)
                .frame(height: 32)
                .background(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .fill(selected ? theme.accentFillColor : Color.clear)
                )
                .contentShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
        .accessibilityLabel(title)
        .accessibilityHint(selected ? "" : hint)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityIdentifier(identifier)
    }

    /// Open mic: a round mic button whose ring swells with the mic level
    /// (flat while muted). Tap toggles mute; the caption says what a tap does.
    private var openMicButton: some View {
        let muted = viewModel.muted
        let level = CGFloat(min(max(viewModel.level, 0), 1))
        return VStack(spacing: 6) {
            Button(action: { viewModel.toggleMute() }) {
                ZStack {
                    Circle()
                        .stroke(theme.accentColor.opacity(muted ? 0 : 0.25 + Double(level) * 0.5),
                                lineWidth: 3)
                        .frame(width: 64, height: 64)
                        .scaleEffect(muted ? 1 : 1 + level * 0.18)
                        .animation(.easeOut(duration: 0.12), value: viewModel.level)
                    Circle()
                        .fill(muted ? theme.surfaceColor : theme.accentFillColor)
                        .overlay(
                            Circle().stroke(theme.secondaryTextColor.opacity(muted ? 0.3 : 0), lineWidth: 1)
                        )
                        .frame(width: 56, height: 56)
                    Image(systemName: muted ? "mic.slash.fill" : "mic.fill")
                        .font(.system(size: 22, weight: .semibold))
                        .foregroundColor(muted ? theme.textColor : theme.onAccentColor)
                }
                .frame(width: 76, height: 76)
                .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(muted ? "Unmute microphone" : "Mute microphone")
            .accessibilityIdentifier("live-open-mic")
            Text(muted ? "Muted · tap to unmute" : "Listening · tap to mute")
                .font(.themed(11))
                .foregroundColor(theme.secondaryTextColor)
                .accessibilityHidden(true)
        }
    }

    /// Hold-to-talk control. Press-and-hold opens the mic gate;
    /// release commits the turn. `DragGesture(minimumDistance: 0)` gives us a
    /// reliable press/release pair (an onLongPress firing only after a delay
    /// would swallow the leading edge of speech).
    private var holdToTalkButton: some View {
        // muted == not held in Hold to talk.
        let held = !viewModel.muted
        return HStack(spacing: 8) {
            Image(systemName: "hand.tap.fill")
                .font(.system(size: 15, weight: .semibold))
            Text(held ? "Release to send" : "Hold to talk")
                .font(.themed(14, weight: .semibold))
        }
        .foregroundColor(held ? theme.onAccentColor : theme.textColor)
        .frame(maxWidth: 280)
        .frame(height: 52)
        .background(
            Capsule(style: .continuous)
                .fill(held ? theme.accentFillColor : theme.surfaceColor)
        )
        .overlay(
            Capsule(style: .continuous)
                .stroke(theme.accentColor.opacity(held ? 0 : 0.35), lineWidth: 1)
        )
        .scaleEffect(held && !reduceMotion ? 0.97 : 1)
        .animation(reduceMotion ? nil : .easeOut(duration: 0.12), value: held)
        .contentShape(Capsule(style: .continuous))
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { _ in
                    // Fire the "down" edge only once per hold (muted == not held).
                    if viewModel.muted { viewModel.pttDown() }
                }
                .onEnded { _ in
                    viewModel.pttUp()
                }
        )
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(held ? "Release to send" : "Hold to talk")
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("live-hold-to-talk")
    }
}

/// A short history of measured audio, including speaker playback after the
/// provider has finished generating. Silence stays flat; no simulated speech.
struct VoiceActivityWaveform: View {
    var level: () -> Float
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var samples = Array(repeating: CGFloat.zero, count: 25)

    var body: some View {
        let color = theme.accentColor
        // Read history in the body so every sample invalidates the native bars.
        let history = samples
        HStack(alignment: .center, spacing: 3) {
            ForEach(Array(history.enumerated()), id: \.offset) { _, value in
                Capsule()
                    .fill(LinearGradient(colors: [color.opacity(0.45), color, color.opacity(0.45)],
                                         startPoint: .top, endPoint: .bottom))
                    .frame(width: 3, height: 2 + min(1, max(0, value)) * 22)
                    .shadow(color: color.opacity(0.28), radius: 3)
            }
        }
        .frame(width: 148, height: 24)
        .animation(reduceMotion ? nil : .easeOut(duration: 0.08), value: history)
        .accessibilityHidden(true)
        .task(id: reduceMotion) {
            // A publisher created in this View's initializer is replaced by
            // every microphone update and can be cancelled before its first tick.
            // The task belongs to the view identity, not the changing level.
            while !Task.isCancelled {
                let value = CGFloat(min(1, max(0, level())))
                if reduceMotion { samples = Array(repeating: value, count: 25) }
                else { samples.removeFirst(); samples.append(value) }
                do { try await Task.sleep(for: .milliseconds(50)) }
                catch { return }
            }
        }
    }
}

/// The call stays connected while the ordinary chat composer is used.
struct LiveCallComposerStrip: View {
    @ObservedObject var viewModel: VoiceCallViewModel
    var onOpen: () -> Void
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Button(action: onOpen) {
                    Text(viewModel.client.phase == .ready
                         ? "Live · \(viewModel.pttOn ? "Hold to talk" : viewModel.muted ? "Muted" : "Open mic")"
                         : viewModel.statusText)
                        .font(.themed(12)).lineLimit(1)
                        .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                        .contentShape(Rectangle())
                }.accessibilityLabel("Open voice call controls")
                    .accessibilityIdentifier("live-composer-status")
                    .accessibilityValue(viewModel.client.phase == .ready ? "Connected" : viewModel.statusText)
                Button(action: { if viewModel.pttOn { onOpen() } else { viewModel.toggleMute() } }) {
                    // The icon names the real state: Hold to talk is not "muted".
                    Image(systemName: viewModel.pttOn
                          ? "hand.tap.fill"
                          : viewModel.muted ? "mic.slash.fill" : "mic.fill")
                        .frame(width: 32, height: 36)
                }.accessibilityLabel(viewModel.pttOn ? "Open voice call controls" : viewModel.muted ? "Unmute microphone" : "Mute microphone")
                Button(action: viewModel.hangUp) {
                    Image(systemName: "xmark").frame(width: 32, height: 36).foregroundColor(theme.dangerColor)
                }.accessibilityLabel("End voice call")
            }
            .buttonStyle(.plain).foregroundColor(theme.secondaryTextColor).padding(.horizontal, 14)
            Rectangle().fill(theme.secondaryTextColor.opacity(0.2)).frame(height: 0.5).accessibilityHidden(true)
        }
    }
}
