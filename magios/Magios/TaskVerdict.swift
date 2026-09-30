import Foundation

/// The verdict: one sentence answering "is this task okay?", derived from state,
/// attention and progress. Pure — no SwiftUI, no networking, no `Date()` of its
/// own. Every task surface renders what this decides.
///
/// iOS port of `ui/unified-ui/src/lib/magician/tasks/taskVerdict.ts`.
/// See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §3 and
/// `docs/components/magios/task-verdict.md`.
///
/// **Instants are `Date`, durations are `TimeInterval` (seconds).** The web
/// carries both as epoch-millisecond `number`s because that is what JSON hands
/// it; Swift has two distinct types for two distinct things and the panel
/// already parses every wire timestamp into a `Date`, so re-flattening them to
/// milliseconds would add a unit nothing else in this app uses. Every threshold
/// and every arithmetic below is therefore in seconds, and no call site can pass
/// an instant where a duration belongs.

// MARK: - HITL sources

/// Where a human-in-the-loop ask came from. Mirrors the web `HitlSource`
/// (`ui/unified-ui/src/lib/hitl/types.ts`) one for one, and the raw values are
/// the wire strings — so `HitlSource(rawValue:)` *is* the "do we model this
/// source" check the web spells out as a `KNOWN_SOURCES` set.
///
/// **This is an enum rather than a `String` alias on purpose.** It is what lets
/// `attentionCopy` (below) and `answeredIn` (in `TaskCapabilities.swift`) be
/// exhaustive `switch`es with no `default`, which is Swift's equivalent of the
/// web's `Record<HitlSource, T>` compile guard: a ninth source fails to build
/// until it is given copy and an act. A `[HitlSource: String]` dictionary is
/// **not** equivalent — it compiles with a source missing and answers `nil` at
/// the reader.
enum HitlSource: String, CaseIterable {
    case agentic
    case userRequest = "user_request"
    case approval
    case planApproval = "plan_approval"
    case clarification
    case escalation
    case diffApproval = "diff_approval"
    case botAuth = "bot_auth"
    case serviceHealth = "service_health"
}

extension HitlSource {
    /// What this source is asking for, in the user's words. Never render the
    /// enum itself — `escalation` means nothing to the person being asked.
    ///
    /// The `switch` has no `default`, and that absence is the guard. See the
    /// type's own note.
    var attentionCopy: String {
        switch self {
        case .agentic: return "The run needs an answer before it can continue"
        case .userRequest: return "The run is asking you something"
        case .approval: return "Approve this before it can continue"
        case .planApproval: return "Approve the plan before it can run"
        case .clarification: return "Answer a question so planning can finish"
        case .escalation: return "The run got stuck and needs a decision"
        case .diffApproval: return "Review the file changes before they are applied"
        case .serviceHealth: return "Check the service credentials, balance, or connection"
        case .botAuth: return "Sign in to the connected account to continue"
        }
    }
}

// MARK: - The verdict

/// Declared in the order `TaskVerdict.derive` checks them. See "Priority, not a
/// list" in the design: when more than one is true, the state demanding action
/// wins — a finished task with an unanswered question is *waiting on you*.
enum VerdictState: String, CaseIterable {
    case waiting
    case failed
    case stalled
    case running
    case paused
    case cancelled
    case queued
    case archived
    case finished
}

/// How loud the verdict is. The bands are **deliberately shared** — `waiting`
/// and `stalled` are both attention, `cancelled` and `queued` are both neutral —
/// because colour cannot carry every state and carries none at all in
/// greyscale. Identity is the marker glyph's job; this is only the wash.
///
/// `cancelled` is `neutral` rather than `failure`: a task-list chip answers
/// "what happened to it", this line answers "is it okay?", and a task you
/// stopped on purpose is okay.
enum VerdictSeverity {
    case attention
    case failure
    case progress
    case neutral
    case success
}

extension VerdictState {
    /// The marker glyph, an SF Symbol. **All are distinct**, because the
    /// glyph is the part that carries identity — the severity wash is shared by
    /// design and is unreadable in greyscale, in high contrast, or to a
    /// colour-blind reader.
    ///
    /// No `default`: a new state fails to compile until it is drawn.
    var marker: String {
        switch self {
        case .waiting: return "hand.raised.fill"
        case .failed: return "xmark.octagon.fill"
        case .stalled: return "exclamationmark.triangle.fill"
        case .running: return "arrow.triangle.2.circlepath"
        case .paused: return "pause.circle.fill"
        case .cancelled: return "stop.circle.fill"
        case .queued: return "clock.fill"
        case .archived: return "archivebox.fill"
        case .finished: return "checkmark.seal.fill"
        }
    }

    /// The severity band. No `default`, for the reason `marker` gives.
    ///
    /// **Do not key this off the panel's existing `statusColor` helper.** That
    /// maps *task statuses*, and three verdict states are not statuses:
    /// `stalled` and `finished` are not keys and would fall through to neutral,
    /// and `waiting` collides with the paused colour — rendering the loudest
    /// state in the union as one of the quietest, silently.
    var severity: VerdictSeverity {
        switch self {
        case .waiting, .stalled: return .attention
        case .failed: return .failure
        case .running: return .progress
        case .paused, .cancelled, .queued, .archived: return .neutral
        case .finished: return .success
        }
    }
}

/// The ask blocking a task, as much of it as the panel can read.
struct VerdictAttention: Equatable {
    let source: HitlSource
    /// The specific ask, when the backend supplies one. Always preferred over
    /// the generic per-source copy.
    let summary: String?
    /// When the ask was raised. Distinct from the run's elapsed time and from
    /// `lastProgressAt` — neither answers "how long has this been blocked on
    /// me", which is the difference between mild and abandoned.
    let raisedAt: Date?

    init(source: HitlSource, summary: String?, raisedAt: Date?) {
        self.source = source
        self.summary = summary
        self.raisedAt = raisedAt
    }
}

struct VerdictInput: Equatable {
    /// The task's own status word, straight off the wire. Deliberately a
    /// `String` and not an enum: the last branch of `derive` is an
    /// unconditional fall-through, so a status nothing checks by name still
    /// produces a verdict rather than failing to compile somewhere far away.
    var status: String
    var attention: VerdictAttention?
    var error: String?
    /// 1-based, as the reader counts. The panel's wire field is 0-based and the
    /// adapter adds the one.
    var currentStep: Int?
    var totalSteps: Int?
    var currentStepLabel: String?
    /// How long the run took, in seconds. Not an instant.
    var elapsed: TimeInterval?
    /// MUST be a real progress timestamp, never `updated_at`: `updated_at`
    /// moves on any write, so a wedged run would refresh its own liveness and
    /// never report stalled. See design §5.
    var lastProgressAt: Date?
    var now: Date
}

struct Verdict: Equatable {
    let state: VerdictState
    let headline: String
    let detail: String
}

// MARK: - Derivation

enum TaskVerdict {
    /// No stall is reported before this much silence.
    static let stallAfter: TimeInterval = 5 * 60

    /// `step 4 of 7`, degrading to `step 4` when the run was never planned and
    /// has no denominator to invent (design §5). Empty when there is no step at
    /// all, so one expression can both interpolate it and test it for presence.
    ///
    /// Exported for the same reason as `durationIfKnown`: the Run act's summary
    /// renders this exact phrase too, and a second copy would drift.
    static func stepPhrase(step: Int?, total: Int?) -> String {
        guard let step = step else { return "" }
        guard let total = total else { return "step \(step)" }
        return "step \(step) of \(total)"
    }

    /// The duration for a line that may not have one — `nil` means "render no
    /// duration at all", never a placeholder.
    ///
    /// Missing, negative, non-finite and out-of-range are one answer, not four:
    /// none of them is a short wait, all of them are an unknown one. Negative is
    /// the routine case rather than the theoretical one, because the instants
    /// come from the server while `now` comes from the device. Every tier test
    /// in `duration` reads a negative as seconds, turning a 90-minute skew into
    /// `-5400s`. A wrong number is worse than no number, exactly as the design
    /// says of an approximated one (§5).
    ///
    /// `0` still renders (`0s`): an instant stop or finish is a fact, distinct
    /// from no timing recorded.
    ///
    /// **This is the total function and the only way in.** `duration` below is
    /// the partial one; it takes whole seconds as an `Int`, so the one lossy
    /// step — `Double` to `Int` — happens here, once, through `Int(exactly:)`,
    /// which rejects non-finite and out-of-range in the same breath. In the web
    /// that conversion is a `Math.round` that cannot fail and prints `NaNh
    /// NaNm`; in Swift the same expression would trap, so the guard is not a
    /// nicety.
    static func durationIfKnown(_ seconds: TimeInterval?) -> String? {
        // `.nan >= 0` is false, so NaN leaves through this guard; `+.infinity`
        // passes it and leaves through `Int(exactly:)`.
        guard let seconds = seconds, seconds >= 0,
              let whole = Int(exactly: seconds.rounded()) else { return nil }
        return duration(seconds: whole)
    }

    /// Scales to the largest useful unit. The minute bucket drops a zero
    /// seconds component so `4m` reads cleanly; the hour bucket drops seconds
    /// entirely, so `1h 20m 5s` renders as `1h 20m` and no duration is ever
    /// three units — at that scale the seconds change no decision. The hour
    /// bucket earns its place on the waiting line: `180m` does not communicate
    /// "abandoned" at a glance, `3h` does.
    ///
    /// Private, and takes whole non-negative seconds. Callers reach it through
    /// `durationIfKnown`, which is the total function; that split is the same
    /// one the web draws, and here `private` enforces it — `@testable import`
    /// raises `internal` to public but never reaches `private`.
    private static func duration(seconds s: Int) -> String {
        if s < 60 { return "\(s)s" }

        let m = s / 60
        if m < 60 {
            let rem = s % 60
            return rem == 0 ? "\(m)m" : "\(m)m \(rem)s"
        }

        let h = m / 60
        let remM = m % 60
        return remM == 0 ? "\(h)h" : "\(h)h \(remM)m"
    }

    static func derive(_ input: VerdictInput) -> Verdict {
        let step = stepPhrase(step: input.currentStep, total: input.totalSteps)

        // Priority, not a list. The state that demands action wins.
        if let attention = input.attention {
            let blocked = durationIfKnown(
                attention.raisedAt.map { input.now.timeIntervalSince($0) }
            )
            return Verdict(
                state: .waiting,
                headline: blocked.map { "Waiting on you · \($0)" } ?? "Waiting on you",
                detail: attention.summary ?? attention.source.attentionCopy
            )
        }

        if input.status == "failed" {
            return Verdict(
                state: .failed,
                headline: step.isEmpty ? "Failed" : "Failed · at \(step)",
                detail: input.error ?? "No error message was recorded"
            )
        }

        if input.status == "running", let last = input.lastProgressAt {
            let silent = input.now.timeIntervalSince(last)
            // The threshold needs no skew guard: clearing a positive threshold
            // is already the stronger check, so a skewed clock reports
            // "running", not a negative silence. It does need the *measurable*
            // guard that `durationIfKnown` carries, because `+.infinity` clears
            // any threshold — which the web renders as `Infinityh` and Swift
            // would trap on. An unmeasurable silence is not a reportable stall.
            if silent >= stallAfter, let silentText = durationIfKnown(silent) {
                // Without the denominator on purpose: `of 7` frames the step as
                // progress, which is the opposite of what this line reports. A
                // live step title with no index is a real shape — unplanned runs
                // have one — so it names the step it cannot number rather than
                // interpolating one.
                let numbered = stepPhrase(step: input.currentStep, total: nil)
                let detail: String
                if let label = input.currentStepLabel {
                    detail = "Still on \(numbered.isEmpty ? "this step" : numbered): \(label)"
                } else {
                    detail = "The run has not advanced"
                }
                return Verdict(
                    state: .stalled,
                    headline: "Stalled · no progress for \(silentText)",
                    detail: detail
                )
            }
        }

        if input.status == "running" {
            return Verdict(
                state: .running,
                headline: step.isEmpty ? "Running" : "Running · \(step)",
                detail: input.currentStepLabel ?? "Working"
            )
        }

        if input.status == "paused" {
            return Verdict(
                state: .paused,
                headline: "Paused",
                detail: "Ready to resume when you are"
            )
        }

        if input.status == "cancelled" {
            let ran = durationIfKnown(input.elapsed)
            return Verdict(
                state: .cancelled,
                headline: ran.map { "Cancelled · after \($0)" } ?? "Cancelled",
                detail: step.isEmpty ? "You stopped this" : "You stopped this at \(step)"
            )
        }

        if input.status == "queued" {
            return Verdict(state: .queued, headline: "Queued", detail: "Waiting for a free slot")
        }

        if input.status == "archived" {
            return Verdict(state: .archived, headline: "Archived", detail: "No longer active")
        }

        // Unconditional: any status the chain above does not model finishes
        // here. An allowlist would turn every unmodelled status into a hole
        // somewhere else; new statuses join the chain, not a guard.
        let took = durationIfKnown(input.elapsed)
        return Verdict(
            state: .finished,
            headline: took.map { "Finished · \($0)" } ?? "Finished",
            // Filled by the surface from its own output summary — the verdict
            // never sees a file list.
            detail: ""
        )
    }
}
