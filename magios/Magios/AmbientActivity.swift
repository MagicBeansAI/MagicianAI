import ActivityKit
import Foundation

/// The concrete ambient orb: the Live Activity on the lock screen and in the
/// Dynamic Island. Rendered by `AmbientLiveActivity` in `MagiosWidgets`.
///
/// Shaped like `ObservationActivity` — one held `Activity`, updates posted from
/// a `Task` because `Activity.update` is async while the seam is not — with two
/// deliberate divergences, both because this orb is the DISARM CONTROL rather
/// than a status readout:
///
/// - **`start` reports failure instead of swallowing it.** See `start`.
/// - **A surviving activity is ended, never adopted.** See `endOrphans`.
@MainActor
final class AmbientActivity: AmbientActivitySink {

    /// How long an ended orb stays on the lock screen when it has a reason to
    /// show. Ambient mode is for someone who is not looking at their phone, so
    /// "your microphone turned off, and here is why" has to survive until they
    /// next pick it up — but not so long that it becomes clutter they dismiss by
    /// habit, because the next thing this surface tells them is that a
    /// microphone is live.
    private static let endedReasonLinger: TimeInterval = 120

    private var activity: Activity<AmbientActivityAttributes>?

    /// The last published content. `update(phase:announcing:)` and `updateCaption(_:role:)`
    /// each own their OWN fields of a `ContentState` that has to be republished
    /// whole, so without these, publishing a phase would blank the caption and
    /// publishing a caption would rewind the phase. The seam splits the two verbs
    /// precisely because captions move far more often than the phase does. The
    /// role travels with the caption because it describes the caption — a role
    /// that outlived its line would attribute the next writer's words.
    private var lastPhase: AmbientOrbPhase = .armed
    private var lastCaption = ""
    private var lastRole: AmbientCaptionRole?

    /// Completed exchanges this window — one per distinct agent transcript line,
    /// counted in `updateCaption` where the publish happens. It counts PUBLISHED
    /// agent lines, so a window whose caption the power warning monopolises
    /// counts none — see `AmbientController.orbCaption`. Carried on every
    /// republish for the reason the fields above are, and stamped onto the final
    /// state so the receipt can render it after the window ends.
    private var exchangeCount = 0

    /// The reply audio's span, held for the same reason the two above are: it is a
    /// third field of a `ContentState` that has to be republished whole.
    private var lastSpeakingSpan: AmbientSpeakingSpan?

    /// When the current connect attempt began, held like the fields above so a
    /// caption publish mid-connect cannot blank the island's give-up gauge.
    /// Stamped by `connectClock(for:)` on every phase publish.
    private var lastConnectingSince: Date?

    /// Whether the compact phase word is currently shown, held like the fields
    /// above so a caption publish mid-pulse cannot blank a word the cadence
    /// put up. Seeded by `phaseWordOnPublish(for:)` on every phase publish —
    /// a transition INTO a conversing phase shows the word on the publish the
    /// transition was already spending — and flipped mid-phase only by
    /// `setPhaseWordVisible`, the controller's cadence.
    private var lastShowsPhaseWord = false

    /// The mutable counterpart to `AmbientActivityAttributes.expiresAt`.
    /// Attributes seed the first render but cannot be changed by ActivityKit;
    /// every later publish carries this value in ContentState and uses it as the
    /// stale date, so rings, digits, and the system cutoff move together.
    private var lastExpiresAt: Date?

    /// Whether a phase publish shows the compact word, as a rule rather than
    /// an inline branch: the three conversing phases show their word on the
    /// transition that announces them (the pulse's first show is free — it
    /// rides that publish); `armed` has no conversation to caption and
    /// `heard`'s word is the continuous connect regime, not the pulse. Pure so
    /// the rule is assertable without ActivityKit.
    nonisolated static func phaseWordOnPublish(for phase: AmbientOrbPhase) -> Bool {
        switch phase {
        case .listening, .thinking, .speaking: return true
        case .armed, .heard: return false
        }
    }

    /// The connect gauge's clock, as a rule rather than an inline branch: the
    /// island draws a gauge filling toward "this attempt gives up" from this
    /// instant, so it exists exactly while a connect attempt does — a fresh
    /// stamp on every publish INTO `heard` (the wake's own publish and the
    /// retry's re-publish both land there, so the gauge honestly restarts with
    /// each attempt), nil for every other phase. Pure, `now` injected, so the
    /// existence rule is assertable without ActivityKit.
    nonisolated static func connectClock(for phase: AmbientOrbPhase, now: Date = Date()) -> Date? {
        phase == .heard ? now : nil
    }

    /// Read at `start` rather than stored at `init`, because the primary agent
    /// can change while the app is running and the armed status line is entirely
    /// the agent's name. Injected rather than hardcoded for the reason
    /// `AmbientActivityAttributes.agentName` exists at all — the roster is
    /// scope-dependent, so a build constant would be wrong for most scopes. A
    /// blank result is handled by the appearance mapping, not papered over with
    /// an invented name.
    private let agentName: @MainActor () -> String

    init(agentName: @escaping @MainActor () -> String = { PrimaryAgentSiriAdvertiser.shared.preferredName ?? "" }) {
        self.agentName = agentName
    }

    /// Returns false if the orb could not be shown, and the controller unwinds
    /// the whole window when it does.
    ///
    /// Both failures are real: the user can turn Live Activities off entirely,
    /// and `Activity.request` throws. `ObservationActivity` swallows both and is
    /// right to — its activity reports a server-side session that can be stopped
    /// from inside the app. Here the orb is the only disarm control reachable
    /// without opening the app, so a swallowed failure is a live microphone with
    /// no proof of life and no way to stop it. No visible indicator, no armed
    /// mic. The specific error is logged here, where it is understood, rather
    /// than widened into a return type the orb was never going to show.
    func start(phase: AmbientOrbPhase, armedAt: Date, expiresAt: Date) -> Bool {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else {
            print("[Ambient] Live Activities are disabled; refusing to arm without an orb")
            return false
        }
        // Sweep first. See `endOrphans`: a surviving ambient activity always
        // belongs to a window that no longer exists, and adopting one would put
        // the PREVIOUS window's `armedAt`/`expiresAt` behind this window's leash
        // timer — a live microphone under a countdown that has already run out.
        endOrphans()
        lastPhase = phase
        lastCaption = ""
        lastRole = nil
        lastSpeakingSpan = nil
        lastConnectingSince = Self.connectClock(for: phase)
        lastShowsPhaseWord = Self.phaseWordOnPublish(for: phase)
        lastExpiresAt = expiresAt
        exchangeCount = 0
        let attributes = AmbientActivityAttributes(
            agentName: agentName(),
            armedAt: armedAt,
            expiresAt: expiresAt
        )
        do {
            activity = try Activity.request(
                attributes: attributes,
                // The derived fields ride the request too, so the first
                // content cannot disagree with the held state a later
                // republish carries. Both are their resting values for the
                // armed start every window actually makes; passing the held
                // fields keeps that a fact rather than a coincidence.
                content: content(phase: phase, caption: "", role: nil, exchangeCount: 0,
                                 connectingSince: lastConnectingSince,
                                 showsPhaseWord: lastShowsPhaseWord,
                                 expiresAt: expiresAt),
                pushType: nil
            )
            return true
        } catch {
            print("[Ambient] Activity.request failed: \(error)")
            return false
        }
    }

    /// Also drops any speaking span, in the same published update.
    ///
    /// A span describes one continuous stretch of reply audio, so *any* phase
    /// change ends it — including a change back INTO `speaking`, which by
    /// construction only reaches here as a transition and therefore always means a
    /// new reply rather than more of the old one. Clearing it here is what keeps
    /// the cost of the whole feature to one extra update per reply: the phase
    /// change the orb was going to publish anyway carries the clear, so the only
    /// added publish is the one that delivers a span the app has actually measured.
    ///
    /// An announcing update carries the wake alert — see `wakeAlert` — and rides
    /// the same publish: the auto-expanded island costs nothing beyond the phase
    /// update the wake was already spending.
    func update(phase: AmbientOrbPhase, announcing: Bool) {
        guard let activity else { return }
        lastPhase = phase
        lastSpeakingSpan = nil
        lastConnectingSince = Self.connectClock(for: phase)
        lastShowsPhaseWord = Self.phaseWordOnPublish(for: phase)
        republish(on: activity, alert: announcing ? wakeAlert(for: activity) : nil)
    }

    /// The pulse's mid-phase flips. Deduplicated for the reason `updateSpeaking`
    /// is: a flip to the value already published would spend a rate-budgeted
    /// update saying nothing.
    func setPhaseWordVisible(_ visible: Bool) {
        guard let activity, visible != lastShowsPhaseWord else { return }
        lastShowsPhaseWord = visible
        republish(on: activity)
    }

    func updateExpiry(_ expiresAt: Date) {
        guard let activity, expiresAt != lastExpiresAt else { return }
        lastExpiresAt = expiresAt
        republish(on: activity)
    }

    /// The wake's `AlertConfiguration`: an update that carries one briefly
    /// auto-presents the EXPANDED island, which is the only presentation an app
    /// can request at all — Disarm and Open land in reach without a long-press,
    /// at the one moment the user just spoke to a lock screen.
    ///
    /// The title mirrors the armed status line's name-degradation doctrine: a
    /// blank roster lookup still produces a complete sentence. The copy is
    /// consistent with the activation-neutral connecting phase. Explicit Talk
    /// taps deliberately do not carry this alert; the app is already visible and
    /// ActivityKit cannot express a silent alert sound.
    ///
    /// Sound: `AlertConfiguration.AlertSound` offers `.default` and named files
    /// only — there is no silent case — so the intended "no sound" is not
    /// expressible. `.default` is the least assertive choice the API allows,
    /// and the sound belongs to the alert's paired-Watch presentation rather
    /// than to the island expansion itself.
    private func wakeAlert(for activity: Activity<AmbientActivityAttributes>) -> AlertConfiguration {
        let name = activity.attributes.agentName.trimmingCharacters(in: .whitespacesAndNewlines)
        let title = "Starting conversation"
        let body = name.isEmpty ? "Magican is connecting…" : "\(name) is connecting…"
        // Dynamic LocalizedStringResource keys render verbatim today (no strings
        // table ships); if localization arrives these need real table entries.
        return AlertConfiguration(title: "\(title)", body: "\(body)", sound: .default)
    }

    /// Preserves the current phase, and is deduplicated rather than filtered for
    /// emptiness: unlike a rolling observation summary, an ambient caption
    /// describes the CURRENT turn, so clearing it is a legitimate publish and a
    /// stale one left behind would misdescribe what the assistant just heard.
    func updateCaption(_ caption: String, role: AmbientCaptionRole?) {
        guard let activity, caption != lastCaption || role != lastRole else { return }
        // A distinct agent transcript line is a completed exchange — the dedup
        // guard above already drops a repeat of the same line, and an agent line
        // that echoes the user's words is still a reply. Counted before the
        // publish so the receipt's number rides the update that exists anyway.
        if role == .agent { exchangeCount += 1 }
        lastCaption = caption
        lastRole = role
        republish(on: activity)
    }

    /// Deduplicated, and that is load-bearing rather than tidy.
    ///
    /// The controller calls this on every reported turn — the span is nil for most
    /// of a conversation and identical across the turns of one reply — so without
    /// the guard this would spend a real ActivityKit update per turn to say nothing.
    /// With it, the cost of a truthful progress bar is exactly one update per reply:
    /// the one that carries a span the audio queue has actually settled on. The nil
    /// case is not even that, because `update(phase:announcing:)` already cleared the span in
    /// the phase change that ended the reply.
    func updateSpeaking(span: AmbientSpeakingSpan?) {
        guard let activity, span != lastSpeakingSpan else { return }
        lastSpeakingSpan = span
        republish(on: activity)
    }

    func end(reason: String?) {
        guard let activity else { return }
        self.activity = nil
        // No span on the final content, whatever the window was doing when it
        // closed. An ended activity keeps rendering for the whole dismissal
        // window, so a bar still elapsing there would be motion outliving the
        // thing it depicts — the same lie as an ended orb still saying
        // "Listening", in a form that keeps moving. The receipt gets its honest
        // end the same way: the one publish that knows the window is over is
        // the one that stamps when.
        let state = AmbientActivityAttributes.ContentState(
            phase: lastPhase,
            caption: lastCaption,
            captionRole: lastRole,
            exchangeCount: exchangeCount,
            endedReason: reason,
            endedAt: Date(),
            expiresAt: lastExpiresAt
        )
        // A reason the user never sees is a reason not given. `.immediate` —
        // what `ObservationActivity` uses — removes the orb before the ended
        // content renders at all, which would make `ContentState.endedReason`
        // unreachable UI and silently discard the whole point of the field: the
        // cap expiring, the microphone being lost, permission being revoked. A
        // disarm with no reason is the user's own doing and needs no
        // explanation, so that one still goes immediately.
        let dismissal: ActivityUIDismissalPolicy = reason == nil
            ? .immediate
            : .after(Date().addingTimeInterval(Self.endedReasonLinger))
        Task {
            await activity.end(ActivityContent(state: state, staleDate: nil), dismissalPolicy: dismissal)
        }
    }

    /// End EVERY ambient activity, including one that outlived its process —
    /// which is what makes an orb collectable after a swipe-kill, when no
    /// in-memory reference to it survives.
    ///
    /// Ending rather than adopting is the whole point. An ambient window is a
    /// local microphone tap with no server-side existence, so it cannot survive
    /// termination; a surviving activity is therefore always garbage, never a
    /// session to resume. Left there it claims to be listening when nothing is,
    /// and offers a disarm control for a window that no longer exists.
    func endOrphans() {
        let survivors = Activity<AmbientActivityAttributes>.activities
        activity = nil
        guard !survivors.isEmpty else { return }
        Task {
            for survivor in survivors {
                await survivor.end(
                    ActivityContent(
                        // Dismissal is immediate, so this content should never
                        // reach the screen — but it is written honestly anyway,
                        // because the one frame it might get must not be the
                        // orb claiming the user is still being heard. Bare on
                        // purpose: an orphan sweep measured nothing, so it
                        // stamps no end and counts no exchanges.
                        state: AmbientActivityAttributes.ContentState(
                            phase: .armed,
                            endedReason: AmbientEndedReason.orphanCollected
                        ),
                        staleDate: nil
                    ),
                    dismissalPolicy: .immediate
                )
            }
        }
    }

    /// `alert` defaults to nil so every path that is not the wake — captions,
    /// spans, non-wake phase changes — is alert-free by construction: an island
    /// that auto-expanded per transcript line would spend the wake's one earned
    /// interruption on noise.
    private func republish(on activity: Activity<AmbientActivityAttributes>, alert: AlertConfiguration? = nil) {
        let next = content(
            phase: lastPhase,
            caption: lastCaption,
            role: lastRole,
            exchangeCount: exchangeCount,
            speakingSpan: lastSpeakingSpan,
            connectingSince: lastConnectingSince,
            showsPhaseWord: lastShowsPhaseWord,
            expiresAt: lastExpiresAt ?? activity.attributes.expiresAt
        )
        Task { await activity.update(next, alertConfiguration: alert) }
    }

    /// Every publish is marked stale at the hard cap, which is the one backstop
    /// no `end` can provide: a process killed past its cap never gets to publish
    /// one, and the launch sweep only runs when the user comes back. Past
    /// `expiresAt` the window is over by definition, so the system flips
    /// `context.isStale` for the view with nothing running at all.
    private func content(
        phase: AmbientOrbPhase,
        caption: String,
        role: AmbientCaptionRole?,
        exchangeCount: Int,
        speakingSpan: AmbientSpeakingSpan? = nil,
        connectingSince: Date? = nil,
        showsPhaseWord: Bool = false,
        expiresAt: Date
    ) -> ActivityContent<AmbientActivityAttributes.ContentState> {
        ActivityContent(
            state: AmbientActivityAttributes.ContentState(
                phase: phase,
                caption: caption,
                captionRole: role,
                exchangeCount: exchangeCount,
                speakingSpan: speakingSpan,
                connectingSince: connectingSince,
                expiresAt: expiresAt,
                showsPhaseWord: showsPhaseWord
            ),
            staleDate: expiresAt
        )
    }
}
