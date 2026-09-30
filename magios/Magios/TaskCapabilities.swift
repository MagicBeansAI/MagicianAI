import Foundation

/// What the task *has*, and therefore which acts a task surface renders. Pure,
/// no view. There is deliberately no task-kind parameter anywhere in this file:
/// if a future change needs one, the unification has failed. Internal tasks
/// gaining planning changes what `hasPlanAct` returns, not this.
///
/// iOS port of `ui/unified-ui/src/lib/magician/tasks/taskCapabilities.ts`.
/// See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §2 and §4.

/// The acts, in lifecycle order. Order is fixed so the eye learns where things
/// live; only which act is open moves (design §2).
///
/// The web derives `ActId` from a separate `ORDER` array precisely so an act
/// cannot exist without a position in the order. Swift gets that for free and
/// strictly stronger: `CaseIterable.allCases` **is** the declaration order, so
/// there is no second list to fall out of sync with and no `indexOf` that can
/// miss.
enum ActId: String, CaseIterable {
    case plan
    case run
    case output
}

extension ActId {
    /// What each act is called. An exhaustive `switch` with no `default`, so
    /// adding a case fails to compile here until it is named — the alternative
    /// is a section rendering with a blank header.
    ///
    /// The titles live beside the type they describe, not in the view that
    /// shows them, for the reason `attentionCopy` gives in `TaskVerdict.swift`:
    /// a title written as a literal in a view becomes a second enumeration of
    /// `ActId` typed against nothing.
    var title: String {
        switch self {
        case .plan: return "Plan"
        case .run: return "Run"
        case .output: return "Output"
        }
    }
}

struct ActCapabilities: Equatable {
    /// The task has a Plan act: false when it was never planned, not when the
    /// plan is empty.
    let hasPlanAct: Bool
    /// The task has a Run act: false when there is no execution, not when it
    /// has yet to move.
    let hasRunAct: Bool
    /// The task has an Output act: **true with zero files** — that act renders
    /// and summarises `no output` (design §4). False means no Output act at
    /// all, which is what one that failed to load must be: empty asserts "no
    /// output" (§6).
    let hasOutputAct: Bool

    init(hasPlanAct: Bool, hasRunAct: Bool, hasOutputAct: Bool) {
        self.hasPlanAct = hasPlanAct
        self.hasRunAct = hasRunAct
        self.hasOutputAct = hasOutputAct
    }
}

/// The act each verdict state is about — where the reader's question is
/// answered, not where the task is in its lifecycle. `failed` and `stalled`
/// open the Run act because the story stops there.
///
/// The `waiting` row is no longer the live answer for a blocked task:
/// `answeredIn` below decides that, from the ask rather than the state. It stays
/// because the mapping is total over `VerdictState`, and it is reachable only
/// through a caller holding a `waiting` verdict with no source — a pairing
/// `TaskVerdict.derive` cannot produce, since it reports `waiting` if and only
/// if an ask is present. Plan was the right answer for the two plan-time
/// sources before the source was available, so it is the safe one to keep.
///
/// `fileprivate`, matching the web's module-private `ACT_FOR_STATE`: it is
/// asserted through `defaultOpenAct`, never against itself.
fileprivate extension VerdictState {
    var preferredAct: ActId {
        switch self {
        case .waiting: return .plan
        case .failed: return .run
        case .stalled: return .run
        case .running: return .run
        case .paused: return .run
        case .cancelled: return .run
        case .queued: return .plan
        case .archived: return .output
        case .finished: return .output
        }
    }
}

/// Where each ask is answered. `waiting` is **eight** HITL sources, and the act
/// the reader has to reach differs between them: two are raised while the plan
/// is being settled, six by a run that is already going. Keying on the state
/// alone sends a mid-run `diff_approval` to the Plan act while the thing it
/// needs sits in Run — a bug that is hard to attribute later, because nothing
/// looks broken.
///
/// The three the design first called ambiguous — `approval`, `agentic`,
/// `user_request` — are resolved from the enum's own documentation rather than
/// guessed: `agentic` is an `ExecutionPauseKind`, `user_request` resolves
/// against an execution pause state, and `approval` is raised by a run against
/// its own resolve endpoint. All three are raised during execution, so all
/// three are Run.
///
/// `clarification` is Plan on the same evidence — the enum documents it as the
/// planning-side `ClarificationQueued`. Its schema does carry an
/// execution-cycle `stage`, which would move it; that is a finer signal than
/// the source and `VerdictAttention` does not carry it today, so it is noted
/// rather than pretended.
///
/// No `default`: a ninth source is a compile error rather than an ask that
/// opens a defensible-looking wrong act.
fileprivate extension HitlSource {
    var answeredIn: ActId {
        switch self {
        case .planApproval: return .plan
        case .clarification: return .plan
        case .agentic: return .run
        case .userRequest: return .run
        case .approval: return .run
        case .escalation: return .run
        case .diffApproval: return .run
        case .botAuth, .serviceHealth: return .run
        }
    }
}

enum TaskCapabilities {
    /// The acts this task has, in lifecycle order. An act the task does not
    /// have is absent from the result, never present-and-disabled — a disabled
    /// act is the type check sneaking back in through styling (design §4).
    static func deriveActs(_ caps: ActCapabilities) -> [ActId] {
        ActId.allCases.filter { id in
            // An exhaustive `switch` over the union rather than a dictionary
            // lookup, so adding an act fails to compile here until it is given
            // a capability rather than silently never rendering. This is the
            // web's `Record<ActId, boolean>` guard.
            switch id {
            case .plan: return caps.hasPlanAct
            case .run: return caps.hasRunAct
            case .output: return caps.hasOutputAct
            }
        }
    }

    /// Which act opens by default. `nil` only when the task has no acts at all.
    ///
    /// `attention` is the source of the ask blocking this task, or `nil` when
    /// nothing is. It is **required rather than defaulted** on purpose: a
    /// caller that forgot it would get a plausible wrong act with nothing to
    /// notice, which is the exact failure this parameter exists to remove.
    ///
    /// An ask outranks the lifecycle, the same priority `TaskVerdict.derive`
    /// applies when it ranks `waiting` above every status. In practice the two
    /// are one case seen from both sides, since a task with an ask always
    /// reports `waiting`.
    ///
    /// When the act it lands on is absent, the nearest **earlier** act opens —
    /// or, if there is none, the earliest later one. Directional rather than a
    /// distance: an act later in the lifecycle than the task has reached is
    /// empty by definition, so anything behind beats anything ahead however far
    /// (design §2). The fallback runs for an ask's act exactly as it does for a
    /// state's, so an ask raised against an act that failed to load still opens
    /// something real.
    static func defaultOpenAct(
        state: VerdictState,
        acts: [ActId],
        attention: HitlSource?
    ) -> ActId? {
        let order = ActId.allCases
        let wanted = attention?.answeredIn ?? state.preferredAct
        if acts.contains(wanted) { return wanted }

        guard let from = order.firstIndex(of: wanted) else { return nil }
        var earlier = from - 1
        while earlier >= 0 {
            if acts.contains(order[earlier]) { return order[earlier] }
            earlier -= 1
        }
        var later = from + 1
        while later < order.count {
            if acts.contains(order[later]) { return order[later] }
            later += 1
        }
        return nil
    }
}
