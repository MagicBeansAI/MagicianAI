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
/// the mm:ss timer, and compact microphone controls alongside a red end button.
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
        .padding(16)
        .frame(maxWidth: 420)
        .background(
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .fill(theme.cardColor)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .stroke(theme.cardBorderColor, lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.22), radius: 18, y: 8)
        .padding(.horizontal, 12)
        .padding(.bottom, 12)
        .transition(.move(edge: .bottom).combined(with: .opacity))
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
                Text(viewModel.statusText)
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

    private var controlRow: some View {
        HStack(spacing: 10) {
            listeningModeControl
            Spacer(minLength: 0)
            if viewModel.pttOn {
                holdToTalkButton
            } else {
                muteButton
            }
        }
    }

    private var listeningModeControl: some View {
        let selectedProfile = audioSettings.realtimeVoiceProfiles.first {
            $0.id == viewModel.realtimeProfile
        }
        let holdAvailable = viewModel.voiceEngine == .handsFree
            || selectedProfile?.isTranslation != true
        let canToggle = viewModel.pttOn || holdAvailable
        return Button {
            viewModel.setHoldToTalk(!viewModel.pttOn)
        } label: {
            HStack(spacing: 6) {
                Label(
                    viewModel.pttOn ? "Hold to talk" : "Open mic",
                    systemImage: viewModel.pttOn ? "hand.tap.fill" : "mic.fill"
                )
                Image(systemName: "arrow.left.arrow.right")
                    .font(.system(size: 9, weight: .bold))
            }
            .font(.themed(12, weight: .medium))
            .foregroundColor(theme.secondaryTextColor)
            .padding(.horizontal, 12)
            .frame(height: 36)
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .stroke(theme.secondaryTextColor.opacity(0.3), lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .disabled(!canToggle)
        .opacity(canToggle ? 1 : 0.5)
        .accessibilityLabel("Microphone mode: \(viewModel.pttOn ? "Hold to talk" : "Open mic")")
        .accessibilityHint(viewModel.pttOn
            ? "Switches to Open mic"
            : "Switches to Hold to talk")
    }

    /// Direct microphone state for Open mic and Hands-free sessions.
    private var muteButton: some View {
        Button(action: { viewModel.toggleMute() }) {
            HStack(spacing: 6) {
                Image(systemName: viewModel.muted ? "mic.slash.fill" : "mic.fill")
                    .font(.system(size: 13))
                Text(viewModel.muted ? "Muted" : "Mic on")
                    .font(.themed(12, weight: .medium))
            }
            .foregroundColor(viewModel.muted ? theme.onAccentColor : theme.textColor)
            .padding(.horizontal, 14)
            .frame(height: 36)
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(viewModel.muted ? theme.accentColor : theme.surfaceColor)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .stroke(theme.secondaryTextColor.opacity(0.2), lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .accessibilityLabel(viewModel.muted ? "Unmute microphone" : "Mute microphone")
    }

    /// Hold-to-talk control. Press-and-hold opens the mic gate;
    /// release commits the turn. `DragGesture(minimumDistance: 0)` gives us a
    /// reliable press/release pair (an onLongPress firing only after a delay
    /// would swallow the leading edge of speech).
    private var holdToTalkButton: some View {
        HStack(spacing: 8) {
            Image(systemName: "mic.fill")
                .font(.system(size: 14))
            Text(viewModel.muted ? "Hold to talk" : "Release to send")
                .font(.themed(12, weight: .semibold))
        }
        .foregroundColor(viewModel.muted ? theme.textColor : theme.onAccentColor)
        .padding(.horizontal, 14)
        .frame(height: 36)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(viewModel.muted ? theme.surfaceColor : theme.accentColor)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(theme.accentColor.opacity(viewModel.muted ? 0.3 : 0), lineWidth: 1)
        )
        .contentShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
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
        .accessibilityLabel("Hold to talk")
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
                         ? "Live · \(viewModel.pttOn ? "Hold to talk" : viewModel.muted ? "Muted" : "Mic on")"
                         : viewModel.statusText)
                        .font(.themed(12)).lineLimit(1)
                        .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                        .contentShape(Rectangle())
                }.accessibilityLabel("Open voice call controls")
                    .accessibilityIdentifier("live-composer-status")
                    .accessibilityValue(viewModel.client.phase == .ready ? "Connected" : viewModel.statusText)
                Button(action: { if viewModel.pttOn { onOpen() } else { viewModel.toggleMute() } }) {
                    Image(systemName: viewModel.muted || viewModel.pttOn ? "mic.slash" : "mic")
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
