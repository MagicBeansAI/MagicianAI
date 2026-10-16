import ActivityKit
import SwiftUI
import WidgetKit

/// The ambient orb: proof that a listening window is open, and its controls —
/// Stop and Extend run without launching the app; Open's entire action is
/// launching it.
///
/// **There is no APP render loop out here, which is not the same as no motion.**
/// Live Activity views are archived SwiftUI rendered out-of-process by WidgetKit,
/// so `repeatForever` on an animation this file owns never runs — a continuously
/// breathing orb driven that way is not expensive, it is impossible. Three
/// mechanisms do work, and the difference between them is entirely about who
/// renders and who pays:
///
/// 1. **The system's cross-dissolve of inserted/removed views.** Free, and
///    narrower than "transitions between content states": device verification
///    settled that a SAME-IDENTITY view whose properties change is redrawn as a
///    hard cut — explicit `.animation(_:value:)` easing on it is ignored out
///    here. What the system fades is an outgoing view replaced by an incoming
///    one. So the orb changes IDENTITY with its palette (`.id` keyed on the
///    gradient stops), turning every phase change into the insert/remove pair
///    the system cross-dissolves — and `AmbientOrbAppearance` still varies
///    palette AND scale so the change is visibly a change. The `.animation`
///    modifiers survive only as ≤2s timing hints iOS 17+ may honor on the swap.
/// 2. **Time-driven views.** `Text(timerInterval:)` and
///    `ProgressView(timerInterval:)` derive their own progress from a date range
///    and tick natively, so they animate continuously for **zero** ActivityKit
///    updates. The rate limit applies to *pushes*, not to a view that already
///    knows where it is going. This is what draws the leash countdown — the
///    digits, the depleting ring around the orb, the compact slot's
///    conversing ring, and the connect wait's give-up gauge — and the reply's
///    progress.
/// 3. **SF Symbol effects.** System-rendered rather than app-rendered, so a
///    repeating one was at least plausible out here — until device verification
///    settled half of it: the halo's repeating pulse is inert out-of-process.
///    The treatment underneath was built to read correctly without it, and does
///    — the static glow carries the phase. The indeterminate `ellipsis` effect
///    is still unverified either way (see `AmbientOrbMotionView`), so every
///    motion this surface is *counted on* to show is time-driven, and anything
///    a symbol effect adds is a bonus, never a dependency.
///
/// The orb itself is the layered aurora rendering from `AuroraOrbView` — halo,
/// gradient body (an organic per-phase blob at 22pt and up, a circle below),
/// highlight, rim — shared with every surface that draws one.
/// Which revises the motion doctrine by one clause: decorative system-rendered
/// motion (the halo's pulse) is permitted where it cannot misstate the phase —
/// it runs only while conversing, and the platform *does* render it inert out
/// here, so the halo is the static glow that degradation priced in: weaker but
/// not false (`docs/archive/plans/2026-07-29-ambient-aurora-brand-kit-design.md`).
///
/// What is deliberately still absent: anything reacting to microphone level,
/// and any motion that asserts a fact the app does not know. The rich,
/// microphone-reactive orb is `VoiceCallPanel`, for when the user has the app
/// open anyway — which is exactly where the Open control leads.
struct AmbientLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AmbientActivityAttributes.self) { context in
            AmbientOrbLockScreenView(context: context)
                .activityBackgroundTint(Color.black.opacity(0.85))
                .activitySystemActionForegroundColor(Color.white)

        } dynamicIsland: { context in
            let appearance = ambientOrbAppearance(context)
            let isOver = ambientWindowIsOver(context)
            let expiresAt = context.state.effectiveExpiresAt(
                fallback: context.attributes.expiresAt
            )
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    // The same ring-or-glyph pair the lock screen draws, at
                    // island scale. The island's expanded rows are short: if
                    // the ring clips on device, drop this branch to the bare
                    // glyph — a remedy that costs nothing, because the digit
                    // timer in the trailing slot already carries the countdown.
                    if isOver {
                        AmbientOrbGlyph(appearance: appearance, diameter: 26)
                    } else {
                        AmbientLeashRing(armedAt: context.attributes.armedAt,
                                         expiresAt: expiresAt,
                                         tint: appearance.fill.opacity(0.85),
                                         diameter: 36) {
                            AmbientOrbGlyph(appearance: appearance, diameter: 26)
                        }
                    }
                }
                DynamicIslandExpandedRegion(.trailing) {
                    if !isOver {
                        AmbientRemainingLeashText(expiresAt: expiresAt)
                            .font(.caption.monospacedDigit())
                            .foregroundColor(.white.opacity(0.8))
                            .frame(maxWidth: 54)
                    }
                }
                DynamicIslandExpandedRegion(.center) {
                    VStack(spacing: 5) {
                        AmbientOrbStatusText(appearance: appearance, font: .subheadline.weight(.semibold))
                        // The one presentation with room for it, so the only one
                        // that gets it. `minimal` and `compactLeading` are a
                        // single dot; adding a second element there would cost
                        // the resting orb the visual quiet that makes a
                        // conversation stand out from it.
                        if !isOver {
                            AmbientOrbMotionView(
                                motion: appearance.motion,
                                span: context.state.speakingSpan,
                                tint: appearance.fill
                            )
                        }
                    }
                }
                DynamicIslandExpandedRegion(.bottom) {
                    if isOver {
                        // Alone, not inside the live row's HStack: the receipt
                        // is nil in the realistic stale-kill case (no honest
                        // end recorded), and an ended row must then render
                        // nothing at all rather than an empty spacer row.
                        AmbientReceiptLine(context: context)
                            .padding(.horizontal, 6)
                    } else {
                        HStack {
                            AmbientTranscriptLine(caption: context.state.caption,
                                                  role: context.state.captionRole,
                                                  agentName: context.attributes.agentName)
                            Spacer()
                            AmbientExtendControl(
                                armedAt: context.attributes.armedAt,
                                expiresAt: expiresAt
                            )
                            AmbientOpenControl()
                            AmbientDisarmControl()
                        }
                        .padding(.horizontal, 6)
                    }
                }
            } compactLeading: {
                // The word rides HERE, beside the orb (owner decisions,
                // 2026-07-30) — not out in the trailing slot across the sensor
                // — and it comes in TWO regimes, folded by
                // `compactWord(showingPhaseWord:)` so this view cannot combine
                // them wrongly. The connect's "Starting…" is continuous: a
                // caption on the orb for a wait that needs explaining. At
                // connect it yields to the first pulsed conversing word while
                // mic + ring arrive trailing — that swap is the "now actually
                // listening" signal. The conversing words —
                // Listening/Thinking/Speaking, the expanded line's exact
                // vocabulary — are PULSED: a few seconds in every ten, on the
                // app's publish schedule, because nothing out here can
                // schedule a hide — every show AND hide is a publish
                // (`AmbientController.resetPhaseWordPulse`). Each rides the
                // sanctioned insert/remove crossfade, and the island's width
                // breathing at the cadence is the accepted cost — the motion
                // pass owes it an eyeball. A dropped hide-publish leaves a
                // TRUE word up longer, never a wrong one: the word is derived
                // from the phase on both sides of the wire. Leading width
                // stays the device risk: the word scales down before it clips.
                HStack(spacing: 3) {
                    AmbientOrbGlyph(appearance: appearance, diameter: 11)
                    if let word = appearance.compactWord(showingPhaseWord: context.state.showsPhaseWord) {
                        Text(word)
                            .font(.caption2.weight(.semibold))
                            .foregroundColor(appearance.fill)
                            .lineLimit(1)
                            .minimumScaleFactor(0.8)
                    }
                }
            } compactTrailing: {
                // Empty while the window is merely armed — and that covers ALL
                // FOUR elements. The absence is the signal: anything appearing
                // here means a conversation is actually under way, so a resting
                // microphone never borrows the visual weight of an active one.
                // The glyph inherits the rule through `compactGlyph` being nil
                // until the session is live, the connect gauge through
                // `connectingSince` being nil outside heard/connecting, the
                // ring through its own `isConversing` gate, and an HStack with
                // nothing in it lays out at zero size — the armed slot stays
                // genuinely empty rather than reserving island width.
                //
                // Which also makes this the one place a *transition* rather than
                // an animation is needed: everything here is inserted and
                // removed, not changed, so it has to grow into the slot instead
                // of appearing fully formed. Rendering it always at zero opacity
                // would animate more cheaply and is wrong — it would reserve
                // island width for a resting window, which is the whole thing
                // the empty slot buys.
                //
                // During the connect wait the slot shows the give-up gauge — a
                // ring filling over the one deadline the app genuinely enforces
                // during a connect, the per-attempt give-up window
                // (`AmbientConnectAttemptWindow`; owner decision, 2026-07-30,
                // chosen over an elapsed counter because a deadline is a fact
                // and a counter is a shrug). The retry republishes a fresh
                // `connectingSince`, so the gauge honestly restarts with the
                // attempt it depicts; a payload with no clock (an old app
                // build) renders NOTHING here rather than a guessed gauge. The
                // leash ring still yields the slot for the wait: a countdown
                // matters less mid-connect than whether this attempt is going
                // to make it. At connect everything swaps — gauge out, mic +
                // ring in, the word leaving the leading slot in the same
                // breath — all inserts and removes, the sanctioned crossfade.
                HStack(spacing: 3) {
                    if appearance.compactWord != nil {
                        AmbientConnectRing(appearance: appearance,
                                           connectingSince: context.state.connectingSince)
                    } else {
                        // The glyph says WHAT (mic/thinking/speaking); the ring
                        // says something is under way and how much window
                        // remains. Symbol content changes are a sanctioned
                        // out-of-process transition (unlike same-identity
                        // colour mutations), so the mic→ellipsis→speaker swap
                        // animates on the phase publish that carried it,
                        // costing nothing extra.
                        if let glyph = appearance.compactGlyph {
                            Image(systemName: glyph)
                                .font(.system(size: 9, weight: .bold))
                                .foregroundColor(appearance.fill)
                                .contentTransition(.symbolEffect(.replace))
                        }
                        AmbientConversingRing(appearance: appearance,
                                              armedAt: context.attributes.armedAt,
                                              expiresAt: expiresAt)
                    }
                }
            } minimal: {
                AmbientOrbGlyph(appearance: appearance, diameter: 10)
            }
            // The island's keyline wears the phase's flat fill, so even the
            // minimal presentation carries the state: purple listening, amber
            // thinking, green speaking — and while merely armed the fill is
            // white at 35%, which keeps the keyline as quiet as the ember dot
            // it frames. An ended window's gray fill mutes it the same way.
            // On a phase flip this tint hard-cuts, accepted: the keyline is a
            // system-managed border, not a view this file can identity-swap,
            // and the orb inside the frame is what cross-dissolves.
            .keylineTint(appearance.fill)
        }
    }
}

// MARK: - What to draw

/// Adapters, and deliberately nothing more.
///
/// Both decisions live in `AmbientOrbAppearance` — including the one that used
/// to be a literal here: an ENDED activity keeps rendering its final content for
/// the whole dismissal window, and `context.isStale` covers the case no `end`
/// can reach, a process killed past the hard cap that never published one. Both
/// mean the window is over, so both replace the status word rather than sitting
/// beside it. The stale sentence has to be the same one the cap timer publishes
/// from the app, and it is now the same *value* rather than two copies that
/// happened to match.
///
/// `ActivityViewContext` is the only WidgetKit thing in either decision, so
/// unwrapping it here is what leaves the decisions themselves testable.
private func ambientOrbAppearance(
    _ context: ActivityViewContext<AmbientActivityAttributes>
) -> AmbientOrbAppearance {
    .forWindow(
        phase: context.state.phase,
        endedReason: context.state.endedReason,
        isStale: context.isStale,
        agentName: context.attributes.agentName
    )
}

private func ambientWindowIsOver(_ context: ActivityViewContext<AmbientActivityAttributes>) -> Bool {
    AmbientOrbAppearance.windowIsOver(endedReason: context.state.endedReason, isStale: context.isStale)
}

// MARK: - Motion

/// The ≤2s timing hint the system may honor when a phase change swaps view
/// identities. Device truth demoted this from mechanism to hint: same-identity
/// property easing is ignored out-of-process, so this modifier animates nothing
/// by itself — the identity swap in `AmbientOrbGlyph` is what actually produces
/// the fade; this shapes its duration where iOS 17+ honors an explicit hint.
/// `.auroraPhaseEase`, the shared constant, so a phase change cannot land
/// differently here than on any other aurora surface. `nil` under Reduce
/// Motion, matching `VoiceCallPanel` — the identity swap still cross-fades
/// then, deliberately: a dissolve is the reduced form of a phase change, not
/// motion.
private func ambientPhaseAnimation(reduceMotion: Bool) -> Animation? {
    reduceMotion ? nil : .auroraPhaseEase
}

/// The status word, cross-faded rather than swapped.
///
/// `.contentTransition(.opacity)` and not `.numericText()`: these are words, and
/// the words differ in length (a connect sentence → "Listening" → an ended
/// sentence), so a digit-rolling transition would be animating the wrong property
/// of the wrong kind of value.
///
/// `.minimumScaleFactor(0.8)` is what absorbs that spread, and the widest status
/// this renders is the ended sentence rather than any phase word — so the connect
/// wording, which is longer than every other phase's, is still not the constraint.
private struct AmbientOrbStatusText: View {
    let appearance: AmbientOrbAppearance
    let font: Font
    var color: Color?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Text(appearance.status)
            .font(font)
            .foregroundColor(color)
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            .contentTransition(.opacity)
            .animation(ambientPhaseAnimation(reduceMotion: reduceMotion), value: appearance.status)
    }
}

/// The continuous motion a phase is entitled to, and nothing more.
///
/// **Both branches are time-driven or system-rendered, so neither costs an
/// ActivityKit update.** That is the entire reason this view can exist on a
/// surface whose updates are budgeted: the rate limit applies to *pushes*, and a
/// view that derives its own progress from a date range is not a push.
///
/// The two branches differ because what the app knows differs, which is
/// `AmbientOrbMotion`'s whole subject:
///
/// - `.elapsing` draws a real deadline and draws NOTHING without one. A reply's
///   span arrives once the audio queue stops growing; until then there is no
///   honest range, so there is no bar. A bar over a guessed range that completed
///   while the assistant was still talking would be this feature's characteristic
///   failure wearing a progress indicator.
///
///   Note what is NOT attempted: a render-time `Date()` check that a published
///   span has not already elapsed. A process killed mid-reply does leave a
///   completed bar under a `speaking` orb until `staleDate` fires — but that is
///   the PHASE's existing exposure, which is why every publish carries
///   `staleDate: expiresAt`, and the guard would not close it. This view is
///   evaluated when the content state CHANGES, and after the process is gone
///   nothing changes it again, so the check would only ever run at a moment the
///   span was fresh.
/// - `.indeterminate` must not imply an end, because neither phase that gets it has
///   one: `thinking` has no deadline by design — a response timeout would cut off a
///   long tool call — and `heard` is the connect wait, measured at 11–13 s and
///   varying by 2.5 s between consecutive runs, so nothing on the device knows when
///   the backend will be ready. A variable-colour `ellipsis` says "working" and says
///   nothing about when. It matters most on `heard`: that is the longest single wait
///   in the feature, and a motionless glyph held across it reads as a crash.
///
/// **The repeating symbol effect is not asserted to work here.** Symbol effects
/// are rendered by the system rather than by an app render loop, so a repeating
/// one was plausible out of process — and the halo's `.pulse`, the sibling
/// repeating effect, has since been device-confirmed inert on these surfaces,
/// which makes the static reading the expected rendering here rather than the
/// contingency. This feature's documentation has been wrong before by asserting
/// platform behaviour instead of checking it, so the static reading is made to
/// carry the phase on its own: the orb beside this is already coloured, scaled
/// and captioned for the phase it belongs to. If the
/// effect turns out to be inert on device, this degrades to a static tinted
/// `ellipsis`, which is a weaker signal but not a false one, and there is no
/// fallback to fall back TO — a `ProgressView(timerInterval:)` is not available
/// without a deadline, so the honest answer is no motion at all rather than an
/// invented range.
///
/// **`heard` is what makes checking it urgent rather than tidy.** `thinking`
/// carried this open question over a wait nobody had measured; `heard` carries it
/// over a measured 11–13 s, which is long enough that the static fallback — a
/// still purple orb under a sentence showing the conversation is starting — is the
/// reading the user actually gets for the longest wait in the feature.
private struct AmbientOrbMotionView: View {
    let motion: AmbientOrbMotion
    let span: AmbientSpeakingSpan?
    let tint: Color
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        switch motion {
        case .still:
            EmptyView()
        case .indeterminate:
            Image(systemName: "ellipsis")
                .font(.caption2.weight(.bold))
                .foregroundColor(tint)
                // Same idiom as `ThinkingMapPrototypeView`: the effect is
                // declared once and Reduce Motion downgrades it to a single
                // non-repeating pass rather than removing the glyph.
                .symbolEffect(
                    .variableColor.iterative.reversing,
                    options: reduceMotion ? .nonRepeating : .repeating
                )
                .frame(height: 6)
                // The status word above already says what is happening; a second
                // announcement of the same fact is noise to VoiceOver.
                .accessibilityHidden(true)
        case .elapsing:
            if let span {
                // Kept under Reduce Motion deliberately. This is not decorative
                // movement, it is the remaining length of the reply — the same
                // category as the interval-timer leash, which that setting also leaves
                // running. Reduce Motion suppresses gratuitous animation, not
                // information that happens to change over time.
                ProgressView(timerInterval: span.from ... span.until, countsDown: false) {
                    EmptyView()
                } currentValueLabel: {
                    EmptyView()
                }
                .progressViewStyle(.linear)
                .tint(tint)
                .frame(height: 6)
                .accessibilityHidden(true)
            }
        }
    }
}

/// The compact trailing slot's conversation indicator: a mini leash ring that
/// is inserted and removed rather than changed, so it needs a transition AND
/// carries its own motion. Time-driven like the big ring — the one channel
/// device verification confirmed animates out-of-process (the halo's symbol
/// pulse does not) — so a conversing island is visibly alive at a glance.
/// The emptiness rule is unchanged: nothing here while merely armed, which is
/// exactly what lets motion here MEAN a conversation.
///
/// Depleting progress reads as an instrument beside the orb rather than as a
/// second orb, which is the role its capsule predecessor held. It wears the
/// flat fill — the channel `AmbientOrbAppearance` keeps for the consumers a
/// gradient cannot tint — so the ring and the orb beside it still cannot
/// disagree about the phase. If the built-in circular gauge refuses
/// 16pt in this slot on device, the fallback is the old gradient capsule;
/// that visual check is owed.
private struct AmbientConversingRing: View {
    let appearance: AmbientOrbAppearance
    let armedAt: Date
    let expiresAt: Date
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Group {
            if appearance.isConversing {
                AmbientLeashRing(armedAt: armedAt,
                                 expiresAt: expiresAt,
                                 tint: appearance.fill,
                                 diameter: 16) {
                    EmptyView()
                }
                // Kept under Reduce Motion: like the digit timer, this is the
                // window's remaining time, not decoration. Scale is still what
                // that setting drops from the insertion; whether the remaining
                // opacity transition renders as a fade or a cut out-of-process
                // under that setting is unverified — either reading is honest,
                // so the device pass owes it an eyeball, not a redesign.
                .transition(reduceMotion ? .opacity : .scale.combined(with: .opacity))
            }
        }
        .animation(ambientPhaseAnimation(reduceMotion: reduceMotion), value: appearance.isConversing)
    }
}

/// The compact trailing slot's connect indicator: a mini gauge FILLING over the
/// per-attempt give-up window — "time until this attempt gives up", the one
/// deadline the app genuinely enforces during a connect. (The give-up window is
/// the sink's 46 s ready-wait wall clock, not the 45 s per-SOCKET watchdog it
/// wraps — `AmbientConnectAttemptWindow` draws that line.) The conversing ring's
/// sibling in every mechanical respect: time-driven (`ProgressView(timerInterval:)`
/// costs zero ActivityKit updates and is the one channel device verification
/// confirmed animates out-of-process), 16pt, wearing the flat fill.
///
/// Filling rather than depleting, the conversing ring's mirror on purpose: that
/// ring drains what remains of a window the user owns, this one fills toward a
/// give-up the machine owes — progress toward a verdict, not consumption of a
/// grant. The range is `connectingSince` plus the SHARED give-up window
/// (`AmbientConnectAttemptWindow.attemptSeconds`), the same value the client
/// enforces, so the gauge cannot drift from the deadline it depicts; the retry
/// republishes a fresh `connectingSince`, so a second attempt visibly restarts
/// it. Two bounded skews are accepted rather than hidden: the stamp leads the
/// sink's clock by the handoff overhead (sub-second against a 46 s range), and
/// a dead first attempt's gauge fills on through the ~1.5 s settle pause until
/// the retry restamps it. A nil clock — an old app build's payload — renders
/// NOTHING: the widget
/// must never guess when an attempt began. Accessibility-hidden like the leash
/// ring's gauge chain: the expanded status line carries the words.
private struct AmbientConnectRing: View {
    let appearance: AmbientOrbAppearance
    let connectingSince: Date?

    var body: some View {
        if let connectingSince {
            ProgressView(
                timerInterval: connectingSince ... connectingSince.addingTimeInterval(AmbientConnectAttemptWindow.attemptSeconds),
                countsDown: false
            ) {
                EmptyView()
            } currentValueLabel: {
                EmptyView()
            }
            .progressViewStyle(.circular)
            .tint(appearance.fill)
            .frame(width: 16, height: 16)
            .accessibilityHidden(true)
        }
    }
}

// MARK: - Pieces

/// A system-driven countdown whose visible contract is a compact digital
/// duration (`mm:ss`, with minutes allowed to exceed 59), never localized prose
/// such as “28 minutes”. `Text(date, style: .timer)` is allowed to choose that
/// prose on the lock screen / Always-On presentation, where the embedded space
/// can wrap the leash onto a second line. The interval initializer with hours
/// suppressed keeps the colon-delimited two-field presentation while retaining
/// the important zero-push ActivityKit ticking behavior.
private struct AmbientRemainingLeashText: View {
    let expiresAt: Date

    var body: some View {
        let now = Date.now
        Text(
            timerInterval: now ... max(now, expiresAt),
            countsDown: true,
            showsHours: false
        )
        .lineLimit(1)
        .minimumScaleFactor(0.7)
    }
}

/// The orb itself, drawn by `AuroraOrbView`: halo, gradient body, highlight,
/// rim. Its vocabulary is no longer only fill plus scale — the palette carries
/// the body, the glow and the edge light — but colour remains the one channel
/// that survives the minimal presentation, and scale still drops under Reduce
/// Motion. Two device checks ride the small sizes, asserted nowhere: at the
/// 10–11 pt the compact slots render, the halo's blur radius is roughly
/// 2.2–2.4 pt — whether a blur that small survives out-of-process rendering is
/// unverified — and the halo deliberately overflows the fixed footprint (it is
/// a glow), which an island slot may clip.
private struct AmbientOrbGlyph: View {
    let appearance: AmbientOrbAppearance
    let diameter: CGFloat
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        AuroraOrbView(palette: appearance.palette, diameter: diameter, isActive: appearance.isConversing)
            // Scale is the one thing Reduce Motion drops, matching
            // `VoiceCallPanel`: the identity swap below cross-fades every phase
            // change, so a differing scale reads as movement while a differing
            // palette reads as a fade. Colour still carries every state on its
            // own, which is exactly why scale was never allowed to be the sole
            // difference.
            .scaleEffect(reduceMotion ? 1 : appearance.scale)
            // The identity key, and the actual phase animation. Device truth:
            // out-of-process, a same-identity view whose colours change is
            // redrawn as a HARD CUT — explicit easing on it is ignored — and
            // the one crossfade the system honours is the fade between a
            // removed view and an inserted one. Keyed on the palette's gradient
            // stops because they change exactly when the visual body changes
            // (every phase, including ended's graphite — so the fade to
            // graphite is intended), and NOT on `status`, which can differ
            // without the body changing (the armed line embeds the agent's
            // name). Above the scale so the whole layered orb swaps as one
            // view, and on the glyph rather than the leash ring around it: the
            // ring's gauge must keep its identity or its time-driven fade
            // restarts on every phase. Kept under Reduce Motion, deliberately —
            // this crossfade IS the reduced form, a dissolve rather than
            // motion.
            .id(appearance.palette.coreStops)
            // Demoted from mechanism to timing hint by the same device truth:
            // this eases nothing by itself out here — the identity swap above
            // is what produces the fade — but it is the ≤2s hint iOS 17+ may
            // honor for the swap's duration, and it keeps every presentation
            // hinting the same `.auroraPhaseEase` instead of the orb landing a
            // phase change differently in `minimal` than in the expanded
            // island. Still keyed on the whole appearance: colours and scale
            // belong to one swap, never two.
            .animation(ambientPhaseAnimation(reduceMotion: reduceMotion), value: appearance)
            // The status text renders beside this glyph wherever there is room
            // for text, so VoiceOver hears the status twice there — the same
            // duplication this file has always shipped. Parity, not a
            // regression.
            .accessibilityLabel(appearance.status)
    }
}

/// The caption as a mini-transcript line: who said it, then what they said.
/// Role nil (the power warning, or an old-build payload) renders the plain
/// line exactly as before — a system sentence has no speaker chip.
private struct AmbientTranscriptLine: View {
    let caption: String
    let role: AmbientCaptionRole?
    let agentName: String
    var lineLimit = 1

    var body: some View {
        if !caption.isEmpty {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                if let role {
                    Text(role == .user ? "You" : displayName)
                        .font(.caption2.weight(.bold))
                        // The agent's chip wears the halo — the surge's softer
                        // tone — rather than the control accent, so a name
                        // never reads louder than a button.
                        .foregroundColor(role == .user ? .white.opacity(0.9) : AuroraPalette.violetSurge.halo)
                        // A long agent name truncates rather than wrapping
                        // against the caption's baseline, and keeps its width
                        // before the caption takes the rest of the line.
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                Text(caption)
                    .font(.caption)
                    .foregroundColor(.white.opacity(0.72))
                    .lineLimit(lineLimit)
            }
            // Cross-fades on the system's content-state timing while the
            // status word eases on `.auroraPhaseEase` — deliberate: captions
            // change on caption publishes rather than phase changes, so the
            // two rarely animate together.
            .contentTransition(.opacity)
            // One VoiceOver stop, not two: the chip and the caption are a
            // single sentence — who said it, then what they said.
            .accessibilityElement(children: .combine)
        }
    }

    private var displayName: String {
        let name = agentName.trimmingCharacters(in: .whitespacesAndNewlines)
        return name.isEmpty ? "Agent" : name
    }
}

/// What the window did, shown only once it is over. `AmbientReceipt` returns
/// nil rather than inventing a duration, and this renders exactly nothing then.
private struct AmbientReceiptLine: View {
    let context: ActivityViewContext<AmbientActivityAttributes>

    var body: some View {
        if let line = AmbientReceipt.line(armedAt: context.attributes.armedAt,
                                          endedAt: context.state.endedAt,
                                          exchangeCount: context.state.exchangeCount) {
            Text(line)
                .font(.caption2)
                .foregroundColor(.white.opacity(0.55))
        }
    }
}

/// Reveal the still-running ambient window in the app. A `Link` rather than a
/// `Button(intent:)`: the entire action IS launching the app. Its URL is
/// intentionally inert — using the compatibility voice URL here would start a
/// fresh turn when the user asked only to see the already-running window.
private struct AmbientOpenControl: View {
    var body: some View {
        Link(destination: SharedActions.ambientURL) {
            Label("Open", systemImage: "arrow.up.forward.app.fill")
                .font(.caption.weight(.semibold))
        }
        .tint(AuroraPalette.controlAccent)
    }
}

/// Add 30 minutes without launching the app. The intent reaches the app-owned
/// timer through the same App Group + Darwin boundary as Stop; the widget never
/// repaints optimistically. Hidden at the eight-hour ActivityKit ceiling, where
/// an "Extend" action could not truthfully move the deadline.
private struct AmbientExtendControl: View {
    let armedAt: Date
    let expiresAt: Date

    var body: some View {
        if AmbientExtensionPolicy.extendedExpiry(
            armedAt: armedAt,
            currentExpiry: expiresAt
        ) != nil {
            Button(intent: ExtendAmbientIntent()) {
                Label("+30 min", systemImage: "clock.badge.plus")
                    .font(.caption.weight(.semibold))
                    .accessibilityLabel("Extend listening by 30 minutes")
            }
            .tint(AuroraPalette.controlAccent)
        }
    }
}

/// The disarm control — the only action here that runs without launching the
/// app.
///
/// Shaped like `ObservationLiveActivity`'s Stop button — `Button(intent:)`, which
/// runs in this process without launching the app — but what happens behind it is
/// not the same: stopping an observation ends a server session that the app trips
/// over later, while an armed ambient window is a purely local microphone tap and
/// the intent has to reach the app process itself. `DisarmAmbientIntent` carries
/// that difference; this control only has to be honestly labelled for it, which
/// it now can be.
private struct AmbientDisarmControl: View {
    var body: some View {
        Button(intent: DisarmAmbientIntent()) {
            Label("Stop", systemImage: "stop.fill")
                .font(.caption.weight(.semibold))
                .accessibilityLabel("Stop listening")
        }
        .tint(.red)
    }
}

// MARK: - Lock screen / banner

private struct AmbientOrbLockScreenView: View {
    let context: ActivityViewContext<AmbientActivityAttributes>

    var body: some View {
        let appearance = ambientOrbAppearance(context)
        let isOver = ambientWindowIsOver(context)
        let expiresAt = context.state.effectiveExpiresAt(
            fallback: context.attributes.expiresAt
        )
        return HStack(alignment: .center, spacing: 14) {
            if isOver {
                // No ring once the window is over: a leash still depleting
                // around a closed window would be motion outliving the thing
                // it depicts. The aura below dies with it for the same reason.
                AmbientOrbGlyph(appearance: appearance, diameter: 40)
            } else {
                ZStack {
                    // The same leash drawn twice at two scales: the crisp
                    // instrument above, and under it this aura — larger,
                    // blurred, dim, wearing the phase halo — depleting over
                    // the same interval. Time-driven because that is the only
                    // channel that moves out here, and honest because it IS
                    // the same countdown, read as an energy glow instead of a
                    // gauge. Lock screen only: the island's rows are too
                    // short for a 72pt overflow.
                    AmbientLeashRing(armedAt: context.attributes.armedAt,
                                     expiresAt: expiresAt,
                                     tint: appearance.palette.halo,
                                     diameter: 72) {
                        EmptyView()
                    }
                    .blur(radius: 6)
                    .opacity(0.35)
                    // The aura overflows this footprint visually — it is a
                    // glow — but must not push layout, or the text column
                    // would shift 8pt whenever the window is live.
                    .frame(width: 56, height: 56)
                    AmbientLeashRing(armedAt: context.attributes.armedAt,
                                     expiresAt: expiresAt,
                                     tint: appearance.fill.opacity(0.85),
                                     diameter: 56) {
                        AmbientOrbGlyph(appearance: appearance, diameter: 40)
                    }
                }
            }
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 8) {
                    AmbientOrbStatusText(appearance: appearance,
                                         font: .headline.weight(.semibold),
                                         color: .white)
                    Spacer(minLength: 8)
                    if !isOver {
                        // The remaining leash on the hard cap. The interval
                        // timer is animated natively by the system, so it counts down
                        // without a single ActivityKit update — and the ring
                        // around the orb draws the same range beside it, never
                        // instead of it: the digits are the precise copy and
                        // the degradation if ring sizing misbehaves. The
                        // initial expiry comes from the fixed attribute;
                        // an Extend tap republishes its mutable ContentState
                        // successor and the same helper feeds every gauge.
                        AmbientRemainingLeashText(expiresAt: expiresAt)
                            .font(.subheadline.monospacedDigit())
                            .foregroundColor(.white.opacity(0.65))
                            .frame(maxWidth: 58, alignment: .trailing)
                    }
                }
                if !isOver {
                    // The same motion the expanded island gets, on the surface
                    // with the most room for it. Above the transcript rather
                    // than below, because it belongs to the status line it is
                    // describing.
                    AmbientOrbMotionView(motion: appearance.motion,
                                         span: context.state.speakingSpan,
                                         tint: appearance.fill)
                }
                if isOver {
                    AmbientReceiptLine(context: context)
                } else {
                    AmbientTranscriptLine(caption: context.state.caption,
                                          role: context.state.captionRole,
                                          agentName: context.attributes.agentName,
                                          lineLimit: 2)
                    HStack(spacing: 10) {
                        Spacer()
                        AmbientExtendControl(
                            armedAt: context.attributes.armedAt,
                            expiresAt: expiresAt
                        )
                        AmbientOpenControl()
                        AmbientDisarmControl()
                    }
                    .padding(.top, 2)
                }
            }
        }
        .padding()
        .background(
            // The phase-tinted wash: the whole card breathes with the phase, at an
            // opacity low enough that the near-black stays near-black. Strength
            // rides the palette's own halo knob, so an ended (graphite) card
            // washes to nothing rather than glowing on.
            RadialGradient(colors: [appearance.palette.halo.opacity(0.28 * appearance.palette.haloStrength), .clear],
                           center: UnitPoint(x: 0.12, y: 0.30),
                           startRadius: 0, endRadius: 260)
                // A same-identity gradient mutation hard-cuts out-of-process,
                // like the orb's — so the wash carries the SAME identity key as
                // the glyph: one key, one system cross-dissolve, orb and wash
                // fading together rather than the card recolouring in two
                // steps. On the background content only; the card itself must
                // keep its identity or the text would fade wholesale and fight
                // its own `.contentTransition`.
                .id(appearance.palette.coreStops)
        )
    }
}
