/**
 * What the task *has*, and therefore which acts the panel renders. Pure, no
 * component. There is deliberately no task-kind parameter anywhere in this
 * module: if a future change needs one, the unification has failed. Internal
 * tasks gaining planning changes what `hasPlanAct` returns, not this.
 *
 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §2 and §4.
 */

import type { HitlSource } from '$lib/hitl/types';

import type { VerdictState } from './taskVerdict';

/**
 * The acts, in lifecycle order. Order is fixed so the eye learns where things
 * live; only which act is open moves (design §2). `ActId` is derived from this
 * array rather than declared beside it, so an act cannot exist without a
 * position in the order and `ORDER.indexOf` can never miss.
 */
const ORDER = ['plan', 'run', 'output'] as const;

export type ActId = (typeof ORDER)[number];

/**
 * What each act is called. A `Record<ActId, string>`, so adding an act to
 * `ORDER` fails to compile here until it is named — the alternative is a section
 * rendering with a blank header.
 *
 * The titles live beside the type they map, not in the markup that shows them,
 * for the reason `ATTENTION_COPY` gives in `taskVerdict.ts`: a title written as
 * a literal in a component becomes a second enumeration of `ActId` typed against
 * nothing.
 */
export const ACT_TITLES: Record<ActId, string> = {
	plan: 'Plan',
	run: 'Run',
	output: 'Output'
};

export interface ActCapabilities {
	/** The task has a Plan act: false when it was never planned, not when the plan is empty. */
	hasPlanAct: boolean;
	/** The task has a Run act: false when there is no execution, not when it has yet to move. */
	hasRunAct: boolean;
	/**
	 * The task has an Output act: **true with zero files** — that act renders and
	 * summarises `no output` (design §4). False means no Output act at all, which
	 * is what one that failed to load must be: empty asserts "no output" (§6).
	 */
	hasOutputAct: boolean;
}

/**
 * The acts this task has, in lifecycle order. An act the task does not have is
 * absent from the result, never present-and-disabled — a disabled act is the
 * type check sneaking back in through styling (design §4).
 */
export function deriveActs(caps: ActCapabilities): ActId[] {
	// A `Record` over the union, so adding an act to `ORDER` fails to compile
	// here until it is given a capability rather than silently never rendering.
	const present: Record<ActId, boolean> = {
		plan: caps.hasPlanAct,
		run: caps.hasRunAct,
		output: caps.hasOutputAct
	};
	return ORDER.filter((id) => present[id]);
}

/**
 * The act each verdict state is about — where the reader's question is
 * answered, not where the task is in its lifecycle. `failed` and `stalled` open
 * the run because the story stops there.
 *
 * The `waiting` row is no longer the live answer for a blocked task:
 * `ACT_FOR_ATTENTION` below decides that, from the ask rather than the state.
 * It stays because the map is total over `VerdictState`, and it is reachable
 * only through a caller holding a `waiting` verdict with no source — a pairing
 * `deriveVerdict` cannot produce, since it reports `waiting` if and only if an
 * ask is present. Plan is the answer that was right for the two plan-time
 * sources before the source was available, so it is the safe one to keep.
 */
const ACT_FOR_STATE: Record<VerdictState, ActId> = {
	waiting: 'plan',
	failed: 'run',
	stalled: 'run',
	running: 'run',
	paused: 'run',
	cancelled: 'run',
	queued: 'plan',
	archived: 'output',
	finished: 'output'
};

/**
 * Where each ask is answered. `waiting` is **eight** HITL sources
 * (`src/lib/hitl/types.ts`), and the act the reader has to reach differs
 * between them: two are raised while the plan is being settled, six by a run
 * that is already going. Keying on the state alone sends a mid-run
 * `diff_approval` to the Plan act while the thing it needs sits in Run — a bug
 * that is hard to attribute later, because nothing looks broken.
 *
 * The three the design left open — `approval`, `agentic`, `user_request` — are
 * resolved from the enum's own documentation rather than guessed: `agentic` is
 * an `ExecutionPauseKind`, `user_request` resolves against an execution pause
 * state, and `approval` is raised by a run against its own resolve endpoint.
 * All three are raised during execution, so all three are Run.
 *
 * `clarification` is Plan on the same evidence — the enum documents it as the
 * planning-side `ClarificationQueued`. Its schema does carry an execution-cycle
 * `stage`, which would move it; that is a finer signal than the source and
 * `VerdictAttention` does not carry it today, so it is noted rather than
 * pretended.
 *
 * A `Record<HitlSource, ActId>`, so a ninth source is a compile error rather
 * than an ask that opens a defensible-looking wrong act.
 */
const ACT_FOR_ATTENTION: Record<HitlSource, ActId> = {
	plan_approval: 'plan',
	clarification: 'plan',
	agentic: 'run',
	user_request: 'run',
	approval: 'run',
	escalation: 'run',
	diff_approval: 'run',
	service_health: 'run',
	bot_auth: 'run'
};

/**
 * Which act opens by default. `null` only when the task has no acts at all.
 *
 * `attention` is the source of the ask blocking this task, or `null` when
 * nothing is. It is **required rather than optional** on purpose: a caller that
 * forgets it would get a plausible wrong act with nothing to notice, which is
 * the exact failure this parameter exists to remove.
 *
 * An ask outranks the lifecycle, the same priority `deriveVerdict` applies when
 * it ranks `waiting` above every status. In practice the two are one case seen
 * from both sides, since a task with an ask always reports `waiting`.
 *
 * When the act it lands on is absent, the nearest **earlier** act opens — or,
 * if there is none, the earliest later one. Directional rather than a distance:
 * an act later in the lifecycle than the task has reached is empty by
 * definition, so anything behind beats anything ahead however far (design §2).
 * The fallback runs for an ask's act exactly as it does for a state's, so an
 * ask raised against an act that failed to load still opens something real.
 */
export function defaultOpenAct(
	state: VerdictState,
	acts: readonly ActId[],
	attention: HitlSource | null
): ActId | null {
	const wanted = attention === null ? ACT_FOR_STATE[state] : ACT_FOR_ATTENTION[attention];
	if (acts.includes(wanted)) return wanted;

	const from = ORDER.indexOf(wanted);
	for (let i = from - 1; i >= 0; i--) {
		if (acts.includes(ORDER[i])) return ORDER[i];
	}
	for (let i = from + 1; i < ORDER.length; i++) {
		if (acts.includes(ORDER[i])) return ORDER[i];
	}
	return null;
}
