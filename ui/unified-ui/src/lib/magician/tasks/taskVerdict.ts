/**
 * The verdict: one sentence answering "is this task okay?", derived from state,
 * attention and progress. Pure — every task surface renders what this decides.
 *
 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §3.
 */

import type { HitlSource } from '$lib/hitl/types';

/** Declared in the order `deriveVerdict` checks them. See "Priority, not a list". */
export type VerdictState =
	| 'waiting'
	| 'failed'
	| 'stalled'
	| 'running'
	| 'paused'
	| 'cancelled'
	| 'queued'
	| 'archived'
	| 'finished';

export interface VerdictAttention {
	source: HitlSource;
	/**
	 * The specific ask, when the backend supplies one. Always preferred over
	 * generic copy. Empty and whitespace-only count as *not supplied* — see
	 * `deriveVerdict`.
	 */
	summary: string | null;
	/**
	 * When the ask was raised. Distinct from `elapsedMs` (run duration) and
	 * `lastProgressAt` (progress) — neither answers "how long has this been
	 * blocked on me", which is the difference between mild and abandoned.
	 */
	raisedAt: number | null;
}

/**
 * Why a task that has not started has not started.
 *
 * The verdict keeps **one** state for all of them — `queued`, which is the only
 * state that says "has not started" without claiming finality, progress or
 * failure (design §9). What was never deliberate is the second line:
 * `Waiting for a free slot` was a constant, and it is true only of a task
 * genuinely waiting for a runner. A task whose planner is working right now, and
 * a monitor asleep until its next fire, are both `queued`, and **neither is
 * waiting for capacity** — the line was not imprecise about them, it was false.
 *
 * So the state stays one and the detail becomes a function of the status —
 * design §9's candidate 2. The split is the one `VerdictAttention.source`
 * already draws: the input carries the fact, this module owns the sentence, and
 * the enum never reaches a reader.
 */
export type QueuedReason = 'capacity' | 'plan' | 'schedule';

export interface VerdictInput {
	status: string;
	attention: VerdictAttention | null;
	/**
	 * Why this task has not started, for the `queued` row's second line. `null`
	 * for every other status, and for a caller that cannot say — see the `queued`
	 * branch, which then says less rather than guessing.
	 */
	queuedFor: QueuedReason | null;
	error: string | null;
	currentStep: number | null;
	totalSteps: number | null;
	currentStepLabel: string | null;
	elapsedMs: number | null;
	/** MUST be a real progress timestamp, never `updated_at`. See design §5. */
	lastProgressAt: number | null;
	now: number;
}

export interface Verdict {
	state: VerdictState;
	headline: string;
	detail: string;
}

/** No stall is reported before this much silence. */
export const STALL_AFTER_MS = 5 * 60_000;

/**
 * What each HITL source is asking for, in the user's words. Never render the
 * enum itself — `escalation` means nothing to the person being asked.
 */
const ATTENTION_COPY: Record<HitlSource, string> = {
	agentic: 'The run needs an answer before it can continue',
	user_request: 'The run is asking you something',
	approval: 'Approve this before it can continue',
	plan_approval: 'Approve the plan before it can run',
	clarification: 'Answer a question so planning can finish',
	escalation: 'The run got stuck and needs a decision',
	diff_approval: 'Review the file changes before they are applied',
	service_health: 'Check service credentials, balance, or connection',
	bot_auth: 'Sign in to the connected account to continue'
};

/**
 * What a queued task is actually waiting for, in the reader's words.
 *
 * A `Record<QueuedReason, string>`, so a fourth reason is a compile error here
 * rather than a status that silently inherits somebody else's sentence — which
 * is the exact defect this map replaces.
 */
const QUEUED_COPY: Record<QueuedReason, string> = {
	capacity: 'Waiting for a free slot',
	plan: 'Working out a plan before it starts',
	schedule: 'Scheduled to start later'
};

/**
 * `step 4 of 7`, degrading to `step 4` when the run was never planned and has
 * no denominator to invent (design §5). Empty when there is no step at all, so
 * one expression can both interpolate it and test it for presence.
 *
 * Exported for the same reason as `durationIfKnown`: the Run act's summary
 * renders this exact phrase too, and a second copy would drift.
 */
export function stepPhrase(step: number | null, total: number | null): string {
	if (step === null) return '';
	return total === null ? `step ${step}` : `step ${step} of ${total}`;
}

/**
 * Scales to the largest useful unit. The minute bucket drops a zero seconds
 * component so `4m` reads cleanly; the hour bucket drops seconds entirely, so
 * `1h 20m 5s` renders as `1h 20m` and no duration is ever three units — at that
 * scale the seconds change no decision. The hour bucket earns its place on the
 * waiting line: `180m` does not communicate "abandoned" at a glance, `3h` does.
 *
 * Callers must not pass a negative or non-finite elapsed time; see
 * `durationIfKnown`, which is the total function and the one to export.
 */
function duration(ms: number): string {
	const s = Math.round(ms / 1000);
	if (s < 60) return `${s}s`;

	const m = Math.floor(s / 60);
	if (m < 60) {
		const rem = s % 60;
		return rem === 0 ? `${m}m` : `${m}m ${rem}s`;
	}

	const h = Math.floor(m / 60);
	const remM = m % 60;
	return remM === 0 ? `${h}h` : `${h}h ${remM}m`;
}

/**
 * The duration for a line that may not have one — `null` means "render no
 * duration at all", never a placeholder.
 *
 * Missing, negative and non-finite are one answer, not three: none of them is a
 * short wait, all of them are an unknown one. Negative is the routine case
 * rather than the theoretical one, because `elapsedMs` and `raisedAt` come from
 * the server while `now` comes from the browser. Every tier test in `duration`
 * reads a negative as seconds, turning `-90m` into `-5400s`, and a non-finite
 * one into `NaNh NaNm`. A wrong number is worse than no number, exactly as the
 * design says of an approximated one (§5).
 *
 * `0` still renders (`0s`): an instant stop or finish is a fact, distinct from
 * no timing recorded.
 */
export function durationIfKnown(ms: number | null): string | null {
	if (ms === null || !Number.isFinite(ms) || ms < 0) return null;
	return duration(ms);
}

export function deriveVerdict(input: VerdictInput): Verdict {
	const step = stepPhrase(input.currentStep, input.totalSteps);

	// Priority, not a list. The state that demands action wins.
	if (input.attention) {
		const raisedAt = input.attention.raisedAt;
		const blocked = durationIfKnown(raisedAt === null ? null : input.now - raisedAt);
		// An empty or whitespace-only summary is an absent ask, not a silent one. A
		// nullish test alone renders a waiting row whose second line explains
		// nothing, and blank is the one detail §3 reserves for `finished`.
		//
		// The emptiness is tested explicitly rather than with `||`: truthiness
		// reads as though every falsy summary were the same case, and would keep
		// swallowing values without comment if this field's type ever widened.
		const summary = input.attention.summary;
		const detail =
			summary !== null && summary.trim().length > 0
				? summary
				: ATTENTION_COPY[input.attention.source];
		return {
			state: 'waiting',
			headline: blocked === null ? 'Waiting on you' : `Waiting on you · ${blocked}`,
			detail
		};
	}

	if (input.status === 'failed') {
		return {
			state: 'failed',
			headline: step ? `Failed · at ${step}` : 'Failed',
			detail: input.error ?? 'No error message was recorded'
		};
	}

	if (input.status === 'running' && input.lastProgressAt !== null) {
		const silent = input.now - input.lastProgressAt;
		// This subtraction needs no skew guard: clearing a positive threshold is
		// already the stronger check, so a skewed clock reports "running", not a
		// negative silence.
		if (silent >= STALL_AFTER_MS) {
			// Without the denominator on purpose: `of 7` frames the step as
			// progress, which is the opposite of what this line reports. A live
			// step title with no index is a real shape — unplanned runs have one —
			// so it names the step it cannot number rather than interpolating one.
			const numbered = stepPhrase(input.currentStep, null);
			return {
				state: 'stalled',
				headline: `Stalled · no progress for ${duration(silent)}`,
				detail: input.currentStepLabel
					? `Still on ${numbered || 'this step'}: ${input.currentStepLabel}`
					: 'The run has not advanced'
			};
		}
	}

	if (input.status === 'running') {
		return {
			state: 'running',
			headline: step ? `Running · ${step}` : 'Running',
			detail: input.currentStepLabel ?? 'Working'
		};
	}

	if (input.status === 'paused') {
		return {
			state: 'paused',
			headline: 'Paused',
			detail: 'Ready to resume when you are'
		};
	}

	if (input.status === 'cancelled') {
		const ran = durationIfKnown(input.elapsedMs);
		return {
			state: 'cancelled',
			headline: ran === null ? 'Cancelled' : `Cancelled · after ${ran}`,
			detail: step ? `You stopped this at ${step}` : 'You stopped this'
		};
	}

	if (input.status === 'queued') {
		return {
			state: 'queued',
			headline: 'Queued',
			// **`null` is a reason nobody stated, and it is not `capacity`.**
			// Defaulting to the capacity sentence is precisely what this change
			// exists to stop: it would put the one line that is false for half the
			// statuses reaching this row back on the row the moment a caller forgot
			// the field, and nothing on screen would contradict it. This says less
			// instead — still true of every queued shape, and distinguishable in a
			// test from all three mapped answers.
			//
			// Written as a lookup that can miss rather than as a `null` test, so it
			// is **total**: a reason this map has no row for reads as no reason at
			// all instead of as `undefined`, which the line would render as an empty
			// second sentence — the one detail §3 reserves for `finished`.
			detail: (input.queuedFor === null ? null : (QUEUED_COPY[input.queuedFor] ?? null)) ?? 'Waiting to start'
		};
	}

	if (input.status === 'archived') {
		return {
			state: 'archived',
			headline: 'Archived',
			detail: 'No longer active'
		};
	}

	// Unconditional: any status the chain above does not model finishes here.
	// An allowlist would duplicate the corpus eval, which asserts over real task
	// shapes that nothing goes unmodelled. New statuses join the chain, not a guard.
	const took = durationIfKnown(input.elapsedMs);
	return {
		state: 'finished',
		headline: took === null ? 'Finished' : `Finished · ${took}`,
		detail: '' // filled by the output summary — Task 3
	};
}
