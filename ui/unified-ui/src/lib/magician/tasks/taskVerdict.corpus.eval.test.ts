/**
 * The verdict corpus eval: properties over a corpus of real task shapes.
 *
 * `taskVerdict.test.ts` proves `deriveVerdict` handles the cases someone thought
 * of. This asserts things that must hold for **every** shape either surface can
 * receive, and it runs each shape through the real adapter rather than
 * hand-building a `VerdictInput` — so it covers the seam as well as the
 * function. A defect that only appears when a particular wire status meets a
 * particular payload has nowhere to hide between the two suites.
 *
 * Why the lane exists, in one sentence: **`deriveVerdict`'s final `return` is an
 * unconditional fallthrough to `finished`**, so a status word nothing in the
 * chain checks renders as `Finished` — the loudest possible lie about a task
 * that has not started, and one the panel then compounds by treating `finished`
 * as settled and claiming `no output`.
 *
 * Run as `make test-task-verdict-eval`; see `docs/components/magician/eval-lanes.md`.
 */

import { describe, expect, it } from 'vitest';

import type { InternalTaskDetails, InternalTaskListItem } from '$lib/internalTasks/api';
import { toInternalTaskPanelModel } from '$lib/internalTasks/internalTaskPanelModel';
import { normalizeV3TaskStatus, parseLastProgressAt, type Task } from '$lib/stores/taskStore';

import corpus from './__fixtures__/taskStates.json';
import { outputFilesFrom, toTaskPanelModel, type TaskOutputRef } from './taskPanelModel';
import { deriveVerdict, type Verdict, type VerdictInput, type VerdictState } from './taskVerdict';

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

interface CapturedShape {
	/** This fixture's own name, used in every failure message. */
	id: string;
	/** What this shape is, in a sentence. */
	shape: string;
	/**
	 * The status **as the backend writes it**. `TaskListItemV3.status` is
	 * `TaskState.status` verbatim, so this is a wire vocabulary and not the
	 * client's `TaskStatus`; the loader below runs it through the store's own
	 * normaliser, which is the only place that translation is allowed to happen.
	 */
	wireStatus: string;
	/**
	 * The backend's own answer to "has this stopped" — `is_terminal_task_status`
	 * in `artifact_v2/service.rs`, which is `completed | failed | cancelled |
	 * archived`. Captured rather than derived from the verdict, because the whole
	 * point is to check the verdict's finality against something that is not the
	 * verdict.
	 */
	terminal: boolean;
	/** The ask blocking this shape, in a sentence, or `null` when nothing is. */
	attention: string | null;
}

interface CapturedTask extends CapturedShape {
	kind: 'task';
	/** A store `Task` minus `status` and with `lastProgressAt` still RFC3339. */
	task: Record<string, unknown>;
	/** `GET /tasks/{id}/outputs` rows, or `null` for a request that never answered. */
	outputs: TaskOutputRef[] | null;
}

interface CapturedInternalTask extends CapturedShape {
	kind: 'internal';
	/** An `/v3/tasks/internal` row minus `status`. */
	row: Record<string, unknown>;
	/** The `/details` payload, or `null` before it answers. */
	details: InternalTaskDetails | null;
}

type Captured = CapturedTask | CapturedInternalTask;

/**
 * The one cast in this file. `resolveJsonModule` types the fixture structurally,
 * and the two shape kinds have disjoint payload fields, so the inferred type is
 * a widened union that no narrowing can use. Asserted once, here, rather than at
 * every read.
 */
const CORPUS = corpus as unknown as { now: string; shapes: Captured[] };

/**
 * The clock every shape is measured against. Fixed rather than `Date.now()`:
 * three of the verdict's headlines carry a duration, and a moving clock would
 * make each of them a different sentence on every run.
 */
const NOW = Date.parse(CORPUS.now);
const SHAPES = CORPUS.shapes;

// ---------------------------------------------------------------------------
// The pipeline under test
// ---------------------------------------------------------------------------

/**
 * A store `Task`, built the way `taskStore` builds one: the wire status through
 * `normalizeV3TaskStatus`, the progress instant through `parseLastProgressAt`.
 * Neither is re-implemented here — a corpus that translated statuses itself
 * would be testing its own translation.
 */
function taskOf(state: CapturedTask): Task {
	const { lastProgressAt, ...rest } = state.task;
	return {
		...rest,
		status: normalizeV3TaskStatus(state.wireStatus),
		lastProgressAt: parseLastProgressAt(lastProgressAt)
	} as Task;
}

function rowOf(state: CapturedInternalTask): InternalTaskListItem {
	// No normalisation here on purpose: the internal adapter calls
	// `normalizeV3TaskStatus` itself, and normalising twice would hide a surface
	// that had stopped doing it.
	return { ...state.row, status: state.wireStatus } as InternalTaskListItem;
}

/**
 * One shape, as `deriveVerdict` receives it after the real adapter has run.
 *
 * The four panel-only fields are dropped by **destructuring**, not by listing
 * the verdict's fields back out: `TaskPanelModel` is declared as
 * `Omit<VerdictInput, 'now'>` plus those four, so this stays exhaustive when a
 * field is added to `VerdictInput`, whereas a hand-written listing would quietly
 * stop forwarding it and every property below would go on passing.
 */
function verdictInputOf(state: Captured): VerdictInput {
	const model =
		state.kind === 'task'
			? toTaskPanelModel(taskOf(state), state.outputs === null ? null : outputFilesFrom(state.outputs))
			: toInternalTaskPanelModel(rowOf(state), state.details, NOW);
	const { id: _id, plan: _plan, run: _run, output: _output, runs: _runs, ...verdict } = model;
	return { ...verdict, now: NOW };
}

function verdictOf(state: Captured): Verdict {
	return deriveVerdict(verdictInputOf(state));
}

/** Every shape, paired with the verdict it produces. Built once. */
const VERDICTS: Array<{ state: Captured; input: VerdictInput; verdict: Verdict }> = SHAPES.map(
	(state) => {
		const input = verdictInputOf(state);
		return { state, input, verdict: deriveVerdict(input) };
	}
);

/** `<fixture id> — <what it is>`, so a failure names the shape and not an index. */
function where(state: Captured): string {
	return `${state.id} — ${state.shape}`;
}

// ---------------------------------------------------------------------------
// Vocabulary the properties are written against
// ---------------------------------------------------------------------------

/**
 * Copy that points the reader somewhere else instead of telling them what
 * happened. Word-boundaried, unlike the design plan's draft: a bare `/tab/i`
 * fires on `unstable` and `table`, both of which appear in real error text, and
 * a lane that cries wolf on a genuine shape is a lane the next person deletes a
 * corpus entry to silence.
 */
const SIGNPOST = /\bsee\b|\btabs?\b|\bpanels?\b|\bdetails\b/i;

/**
 * An opaque identifier — `exec_71d0c3ae`, `plan_2ce70b4d`. L0 is one sentence
 * answering "is this task okay"; an id answers nothing and belongs in an act's
 * provenance at L3, which is exactly the defect design §1 blames for the panel
 * this replaces.
 */
const OPAQUE_ID = /\b[a-z]{2,6}_[0-9a-f]{6,}\b/;

/**
 * **Every** `HitlSource`, not the four the design plan's draft listed.
 *
 * The draft named `plan_approval`, `diff_approval`, `bot_auth` and
 * `user_request` — and neither adapter can produce any of them. `taskPanelModel`
 * hard-codes `clarification` and `internalTaskPanelModel` hard-codes
 * `escalation`, both as documented routing choices, so a leak of the source
 * these surfaces really carry would have passed the check that was supposed to
 * catch it.
 */
const HITL_SOURCE =
	/\b(agentic|user_request|approval|plan_approval|clarification|escalation|diff_approval|bot_auth)\b/;

/**
 * Verdict states that say the task is still moving. Deliberately **not** a copy
 * of the panel's `SETTLED` map, which asks a different question (may an output
 * claim be made) and answers it differently (`waiting` is not settled there, and
 * is perfectly legitimate on a stopped task here).
 */
const STILL_GOING = new Set<VerdictState>(['running', 'queued', 'stalled']);

/**
 * Wire statuses the client has no row for, each with what it does instead.
 *
 * A status here is a **known defect, recorded**, not an exemption granted for
 * convenience. Two assertions keep it honest: a status that is unrecognised and
 * *not* listed fails, and a listed status that has since been learned (or that
 * no shape carries any more) fails as stale — the same shape as
 * `ALLOWED_UNANNOTATED` in `magician/tests/eval_lane_contract.rs`.
 */
const KNOWN_UNMODELLED_WIRE_STATUS: Record<string, string> = {};

const DISTINCT_WIRE_STATUSES = [...new Set(SHAPES.map((state) => state.wireStatus))];

/** Whether the store's normaliser recognises this status, or defaulted it. */
function recognisedByTheStore(wireStatus: string): boolean {
	// `pending` is both a real status and the normaliser's default for an
	// unrecognised one, so it is the single value this cannot distinguish — and
	// the only one that needs naming as an exception rather than being detected.
	return wireStatus === 'pending' || normalizeV3TaskStatus(wireStatus) !== 'pending';
}

// ---------------------------------------------------------------------------
// The corpus itself
// ---------------------------------------------------------------------------

describe('the corpus', () => {
	it('is a set of distinct shapes measured against one readable clock', () => {
		expect(Number.isFinite(NOW)).toBe(true);
		expect(SHAPES.length).toBeGreaterThan(0);

		const ids = SHAPES.map((state) => state.id);
		expect(ids).toEqual([...new Set(ids)]);
	});

	it('covers both task kinds, because they reach the verdict down different paths', () => {
		// The two adapters share a status vocabulary and nothing else: one reads a
		// store `Task`, the other a wire row plus a `/details` payload. A corpus of
		// one kind would leave the other's seam unmeasured.
		const kinds = new Set(SHAPES.map((state) => state.kind));
		expect([...kinds].sort()).toEqual(['internal', 'task']);
	});

	it('reaches every verdict state, so no property below is silently unexercised', () => {
		// Without this, a corpus that stopped producing `stalled` — or a pipeline
		// that collapsed every shape onto one state — would leave most of the
		// properties below asserting nothing while staying green. A
		// `Record<VerdictState, …>`, so a new state is a compile error here and
		// has to be given a shape rather than joining untested.
		const EVERY_STATE: Record<VerdictState, true> = {
			waiting: true,
			failed: true,
			stalled: true,
			running: true,
			paused: true,
			cancelled: true,
			queued: true,
			archived: true,
			finished: true
		};
		const reached = new Set(VERDICTS.map(({ verdict }) => verdict.state));
		expect([...reached].sort()).toEqual(Object.keys(EVERY_STATE).sort());
	});

	it('carries every wire status the store normaliser names, so it cannot silently shrink', () => {
		// The normaliser is the record of what actually arrives, and a `switch`
		// cannot be enumerated at runtime — so this list is a claim, and the
		// assertion under it is what stops the claim from drifting: every entry
		// must still be one the normaliser recognises.
		const NAMED_BY_THE_NORMALISER = [
			'pending',
			'planning',
			'ready',
			'running',
			'paused',
			'archived',
			'completed',
			'failed',
			'cancelled',
			'deferred',
			'waiting_for_user',
			'waiting_for_confirmation',
			'paused_by_user',
			'waiting_for_children',
			'sleeping'
		];

		expect(NAMED_BY_THE_NORMALISER.filter((status) => !recognisedByTheStore(status))).toEqual([]);
		expect(
			NAMED_BY_THE_NORMALISER.filter((status) => !DISTINCT_WIRE_STATUSES.includes(status))
		).toEqual([]);
	});
});

// ---------------------------------------------------------------------------
// The properties. Each one is a way the verdict line could mislead.
// ---------------------------------------------------------------------------

describe('no verdict misleads about what happened', () => {
	it('says `waiting` for a blocked shape, and only for a blocked shape', () => {
		// Both directions on purpose. Asserting only the first would pass against a
		// pipeline that marked everything `waiting`, which mislabels every running
		// task as blocked on the reader — a worse lie than the one being guarded.
		for (const { state, verdict } of VERDICTS) {
			if (state.attention) {
				expect(verdict.state, `${where(state)} (blocked on: ${state.attention})`).toBe('waiting');
			} else {
				expect(verdict.state, where(state)).not.toBe('waiting');
			}
		}
	});

	it('carries a failure’s own reason rather than pointing elsewhere', () => {
		for (const { state, verdict } of VERDICTS) {
			if (verdict.state !== 'failed') continue;
			expect(verdict.detail, where(state)).not.toMatch(SIGNPOST);
			expect(verdict.detail.length, where(state)).toBeGreaterThan(0);
		}
	});

	it('never renders an opaque identifier at L0', () => {
		// Not failable from inside `deriveVerdict`, which is handed no identifier at
		// all — it is failable at the seam above it, which is where the defect
		// actually lives: an adapter that reached for `activeExecutionId` or a
		// `completed_step_id` when it had no step label would put an id on the
		// verdict line, and every shape here carries opaque ids for it to find.
		for (const { state, verdict } of VERDICTS) {
			expect(verdict.headline + ' ' + verdict.detail, where(state)).not.toMatch(OPAQUE_ID);
		}
	});

	it('always produces a headline, so the line never collapses', () => {
		// The detail deliberately has no matching assertion: a finished task's is
		// empty by design, filled by the panel from the output summary.
		for (const { state, verdict } of VERDICTS) {
			expect(verdict.headline, where(state)).toBeTruthy();
		}
	});

	/**
	 * **The `queued` row's second line, per wire status.**
	 *
	 * Four wire statuses reach one verdict state, and for a long time they reached
	 * one *sentence*: `Waiting for a free slot`. Two of them are not waiting for a
	 * runner and a free one would start neither — the planner is working on one,
	 * and the other is asleep until a scheduled instant. The line was not
	 * imprecise about them, it was false, and nothing on screen contradicted it.
	 *
	 * Asserted in both directions on purpose. The forward one alone passes against
	 * a pipeline that never says `Waiting for a free slot` at all, which would be
	 * a different lie about the two statuses that genuinely are queued for a
	 * runner. The wire statuses are named here rather than read off the adapter's
	 * map: this lane's job is to state what is true of a real task, and a copy of
	 * the implementation would agree with it by construction.
	 */
	it('claims a task is waiting for capacity only when it is', () => {
		const WAITS_FOR_A_RUNNER = new Set(['pending', 'ready']);

		for (const { state, verdict } of VERDICTS) {
			if (verdict.state !== 'queued') continue;
			expect(
				verdict.detail === 'Waiting for a free slot',
				`${where(state)} — wire status \`${state.wireStatus}\` reads \`${verdict.detail}\``
			).toBe(WAITS_FOR_A_RUNNER.has(state.wireStatus));
		}
	});

	it('gives every queued shape a second line, and never a blank one', () => {
		// Blank is the one detail §3 reserves for `finished`, which the panel fills
		// from the output summary. A `queued` row rendering an empty second line is
		// how a missing reason would look on screen — indistinguishable from a
		// finished task's row at a glance, and the reason this is asserted rather
		// than left to the per-reason copy above.
		for (const { state, verdict } of VERDICTS) {
			if (verdict.state !== 'queued') continue;
			expect(verdict.detail.trim().length, where(state)).toBeGreaterThan(0);
		}
	});

	it('never leaks a HITL source enum to the reader', () => {
		for (const { state, verdict } of VERDICTS) {
			expect(verdict.detail, where(state)).not.toMatch(HITL_SOURCE);
		}
	});
});

// ---------------------------------------------------------------------------
// Finality: the claim the panel builds its output verdict on.
// ---------------------------------------------------------------------------

describe('the verdict agrees with the backend about whether the task stopped', () => {
	it('never reads `finished` for a task the backend has not finished', () => {
		// The lane's reason to exist. `deriveVerdict`'s last branch is
		// unconditional, so any status word the chain does not check lands here —
		// and the panel then treats `finished` as settled and answers `no output`
		// about a run that may still be going.
		for (const { state, verdict } of VERDICTS) {
			if (state.terminal) continue;
			expect(verdict.state, `${where(state)} — wire status \`${state.wireStatus}\``).not.toBe(
				'finished'
			);
		}
	});

	it('never reads as still going for a task the backend has finished', () => {
		for (const { state, verdict } of VERDICTS) {
			if (!state.terminal) continue;
			if (state.wireStatus in KNOWN_UNMODELLED_WIRE_STATUS) continue;
			expect(
				STILL_GOING.has(verdict.state),
				`${where(state)} — wire status \`${state.wireStatus}\` read as \`${verdict.state}\``
			).toBe(false);
		}
	});
});

// ---------------------------------------------------------------------------
// The unmodelled-status guards.
// ---------------------------------------------------------------------------

describe('every status the corpus reaches is one something models', () => {
	it('fails when the corpus contains a status the verdict does not model', () => {
		// The design plan wrote this against a hand-kept set of the five status
		// words. Derived instead, because the set would have been a second copy of
		// `deriveVerdict`'s own chain and the two would disagree the first time
		// either changed — and the derivation is strictly stronger, since it names
		// the offending word instead of just reporting that one exists.
		//
		// With the ask and the progress instant taken off, the chain's status
		// branches are the only thing left deciding: a status word it checks by
		// name yields a state of the same name, and one it does not falls through
		// to `finished`. So `state === status` IS "the verdict models this status".
		for (const { state, input } of VERDICTS) {
			const bare = deriveVerdict({ ...input, attention: null, lastProgressAt: null });
			expect(bare.state, `${where(state)} — status word \`${input.status}\``).toBe(input.status);
		}
	});

	it('recognises every wire status in the corpus, or names it as a known defect', () => {
		const unrecognised = DISTINCT_WIRE_STATUSES.filter(
			(status) => !recognisedByTheStore(status) && !(status in KNOWN_UNMODELLED_WIRE_STATUS)
		);
		expect(unrecognised).toEqual([]);
	});

	it('has no stale entries in the known-unmodelled ledger', () => {
		const stale = Object.keys(KNOWN_UNMODELLED_WIRE_STATUS).filter(
			(status) => !DISTINCT_WIRE_STATUSES.includes(status) || recognisedByTheStore(status)
		);
		expect(stale).toEqual([]);
	});
});
