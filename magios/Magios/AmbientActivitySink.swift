import Foundation

/// The orb, behind a seam so the controller is testable without ActivityKit.
///
/// Deliberately exposes `updateCaption` separately from `update(phase:announcing:)`:
/// captions move far more often than the phase does, and
/// `AmbientState.orbPhaseChanged` covers only the phase, so gating both on one
/// signal would freeze the caption for a whole turn while the orb animated
/// correctly above it. Task 6 supplies the concrete `AmbientActivity`.
@MainActor
protocol AmbientActivitySink: AnyObject {
    /// Returns false if the orb could not be shown. Both failures are real and
    /// already guarded in `ObservationActivity`: `Activity.request` throws, and
    /// `ActivityAuthorizationInfo().areActivitiesEnabled` can be false because
    /// the user turned Live Activities off.
    ///
    /// **If the orb cannot be shown, arming fails.** That is a product decision,
    /// not a plumbing detail, and it is the reason this reports at all: the orb
    /// is not decoration, it is the DISARM CONTROL, and the only one reachable
    /// without opening the app. Swallowing the failure would leave a live
    /// microphone with no proof of life and no way to stop it — the same outcome
    /// as an unrenderable `ContentState`, reached through the front door. No
    /// visible indicator, no armed mic.
    ///
    /// `ObservationActivity` swallows both failures, and that is right *there* —
    /// its activity reports a session that lives on the server and can be stopped
    /// from inside the app. Do not copy the pattern here.
    ///
    /// Reporting rather than throwing keeps this symmetric with
    /// `AmbientCallSink.startCall`, for the same reason: the controller has
    /// exactly one response, and the specific ActivityKit error is logged where
    /// it is understood.
    func start(phase: AmbientOrbPhase, armedAt: Date, expiresAt: Date) -> Bool

    /// **Also ends any speaking span**, because a span belongs to exactly one
    /// continuous stretch of reply audio and a phase change is the end of that
    /// stretch by definition. Folded in here rather than left to the caller so the
    /// two travel in ONE published update instead of two, and so the one ordering
    /// that would be a lie — a bar still elapsing under a phase that has moved on —
    /// is unreachable rather than merely avoided.
    ///
    /// `announcing` marks the wake transition, the one update that carries an
    /// alert: an alert-carrying ActivityKit update briefly auto-presents the
    /// EXPANDED island, which no app can otherwise request. A flag on this verb
    /// rather than a verb of its own, so the alert rides the phase publish the
    /// wake was spending anyway — a separate verb would either cost a second
    /// rate-budgeted update or invite calling it INSTEAD of this one and losing
    /// the span clear above. The controller decides which transition announces;
    /// this seam only carries the decision.
    func update(phase: AmbientOrbPhase, announcing: Bool)

    /// `role` is nil for system lines (the power warning) and set for transcript
    /// lines. The sink counts an exchange per published agent transcript line —
    /// an exact immediate repeat dedups, an agent line echoing the user's words
    /// still counts — counted where the publish happens so the count and the
    /// caption travel in the SAME update rather than two.
    func updateCaption(_ caption: String, role: AmbientCaptionRole?)

    /// The reply audio's own span, or nil when there is none to show.
    ///
    /// Separate from `updateCaption` for the reason that one is separate from
    /// `update(phase:announcing:)`, and a sharper one: this is a DIFFERENT FIELD, not another
    /// writer of the caption line. The power-mode warning has to survive the whole
    /// window and `updateCaption` replaces the line, so a span published through
    /// the caption would blank it — the exact regression
    /// `AmbientController.publishOrbCaption` exists as a single composition point
    /// to prevent.
    ///
    /// Called on every reported turn, so the implementation must DEDUPLICATE: the
    /// span is nil for most of a conversation, and a publish per turn that says
    /// nothing new is a real ActivityKit update spent on nothing.
    func updateSpeaking(span: AmbientSpeakingSpan?)

    /// The phase-word pulse's hide and re-show — the controller's cadence
    /// timer speaks through this, because out-of-process nothing can schedule:
    /// every show AND hide of the compact word is a publish. The SHOW at a
    /// phase transition rides `update(phase:announcing:)` (the sink derives
    /// the flag from the phase, so the transition costs nothing extra); this
    /// verb carries only the mid-phase flips. Never an alert, and the
    /// implementation must DEDUPLICATE for the reason `updateSpeaking` must —
    /// a flip to the value already published is an update spent on nothing.
    func setPhaseWordVisible(_ visible: Bool)

    /// Move the window's one authoritative expiry. The concrete sink republishes
    /// both ContentState and `staleDate`; a countdown-only repaint would disagree
    /// with the controller timer and is therefore not a valid implementation.
    func updateExpiry(_ expiresAt: Date)
    func end(reason: String?)
    /// End every ambient activity, including one that outlived its process.
    func endOrphans()
}
