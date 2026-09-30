import { describe, it, expect } from 'vitest';
import type { HitlSource } from '$lib/hitl/types';
import { ACT_TITLES, deriveActs, defaultOpenAct, type ActId } from './taskCapabilities';
import type { VerdictState } from './taskVerdict';

/**
 * Every act, and every subset of them. Both are derived rather than listed, so
 * a fourth act cannot leave the sweeps below quietly covering three.
 */
const ALL: ActId[] = deriveActs({ hasPlanAct: true, hasRunAct: true, hasOutputAct: true });
const SUBSETS: ActId[][] = ALL.reduce<ActId[][]>(
	(subsets, act) => [...subsets, ...subsets.map((subset) => [...subset, act])],
	[[]]
);

/**
 * This array is not the exhaustiveness guard — an eighth `VerdictState` would
 * not fail here, it would fail to compile `ACT_FOR_STATE`, which is a
 * `Record<VerdictState, ActId>`. What it covers is the rendering rule: every
 * state opens an act the task actually has.
 */
const STATES: VerdictState[] = [
	'waiting',
	'failed',
	'stalled',
	'running',
	'paused',
	'cancelled',
	'queued',
	'archived',
	'finished'
];

describe('deriveActs', () => {
	it('gives a planned task all three acts in fixed order', () => {
		expect(deriveActs({ hasPlanAct: true, hasRunAct: true, hasOutputAct: true })).toEqual([
			'plan',
			'run',
			'output'
		]);
	});

	it('omits an act the task does not have, rather than disabling it', () => {
		expect(deriveActs({ hasPlanAct: false, hasRunAct: true, hasOutputAct: true })).toEqual([
			'run',
			'output'
		]);
	});

	it('keeps order fixed regardless of which acts exist', () => {
		expect(deriveActs({ hasPlanAct: true, hasRunAct: false, hasOutputAct: true })).toEqual([
			'plan',
			'output'
		]);
	});

	it('returns nothing for a task with no acts yet', () => {
		expect(deriveActs({ hasPlanAct: false, hasRunAct: false, hasOutputAct: false })).toEqual([]);
	});

	it('maps the four combinations the cases above leave out', () => {
		expect(deriveActs({ hasPlanAct: true, hasRunAct: false, hasOutputAct: false })).toEqual([
			'plan'
		]);
		expect(deriveActs({ hasPlanAct: false, hasRunAct: true, hasOutputAct: false })).toEqual(['run']);
		expect(deriveActs({ hasPlanAct: false, hasRunAct: false, hasOutputAct: true })).toEqual([
			'output'
		]);
		expect(deriveActs({ hasPlanAct: true, hasRunAct: true, hasOutputAct: false })).toEqual([
			'plan',
			'run'
		]);
	});

	it('returns a fresh array, so no caller can mutate the shared order', () => {
		const caps = { hasPlanAct: true, hasRunAct: true, hasOutputAct: true };
		const first = deriveActs(caps);
		expect(deriveActs(caps)).not.toBe(first);

		first.push('plan');
		expect(deriveActs(caps)).toEqual(['plan', 'run', 'output']);
	});
});

describe('ACT_TITLES', () => {
	it('names every act, pinned against literals', () => {
		// Completeness is the compiler's job — `Record<ActId, string>` fails to
		// build if an act joins `ORDER` unnamed. What a `Record` cannot catch is a
		// title that is empty or attached to the wrong act, and every other test in
		// this suite reads the titles through this map, so the words are pinned
		// here once against literals rather than compared to themselves.
		expect(ACT_TITLES).toEqual({ plan: 'Plan', run: 'Run', output: 'Output' });
	});
});

/**
 * Every HITL source, and the act its ask is answered in. Literals rather than
 * the module's own map, which would assert only that the map equals itself.
 * Six of the eight are run-time asks — that imbalance is the whole reason the
 * source is a parameter, so a sweep that ignored it would fail six ways.
 */
const ACT_FOR_SOURCE: Record<HitlSource, ActId> = {
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

const SOURCES = Object.keys(ACT_FOR_SOURCE) as HitlSource[];

describe('defaultOpenAct', () => {
	it('opens the act each verdict state is about', () => {
		expect(defaultOpenAct('waiting', ALL, null)).toBe('plan');
		expect(defaultOpenAct('queued', ALL, null)).toBe('plan');
		expect(defaultOpenAct('running', ALL, null)).toBe('run');
		expect(defaultOpenAct('stalled', ALL, null)).toBe('run');
		expect(defaultOpenAct('failed', ALL, null)).toBe('run');
		expect(defaultOpenAct('cancelled', ALL, null)).toBe('run');
		expect(defaultOpenAct('finished', ALL, null)).toBe('output');
	});

	it('falls back to an earlier act when the one the state is about is absent', () => {
		expect(defaultOpenAct('finished', ['plan', 'run'], null)).toBe('run');
		expect(defaultOpenAct('finished', ['plan'], null)).toBe('plan');
		expect(defaultOpenAct('running', ['plan'], null)).toBe('plan');
	});

	it('falls forward only when the task has reached nothing earlier', () => {
		expect(defaultOpenAct('waiting', ['run', 'output'], null)).toBe('run');
		expect(defaultOpenAct('queued', ['output'], null)).toBe('output');
	});

	it('prefers the earlier act when the missing one sits between two the task has', () => {
		// The only shape where the direction preference is observable: a Run act
		// that failed to load leaves Plan and Output around the gap (design §6).
		expect(defaultOpenAct('running', ['plan', 'output'], null)).toBe('plan');
		expect(defaultOpenAct('failed', ['plan', 'output'], null)).toBe('plan');
	});

	it('opens nothing when the task has no acts at all', () => {
		for (const state of STATES) {
			expect(defaultOpenAct(state, [], null), state).toBeNull();
			expect(defaultOpenAct(state, [], 'diff_approval'), state).toBeNull();
		}
	});

	it('never opens an act the task does not have, for any state', () => {
		for (const state of STATES) {
			for (const acts of SUBSETS) {
				if (acts.length === 0) continue; // the named case above
				expect(acts, `${state} · [${acts.join(', ')}]`).toContain(defaultOpenAct(state, acts, null));
			}
		}
	});
});

describe('defaultOpenAct — routing by the ask', () => {
	it('opens the act each HITL source is answered in', () => {
		for (const source of SOURCES) {
			// The state is `waiting` for all eight, which is exactly why the state
			// cannot decide this: `plan_approval` and `clarification` are settled in
			// the Plan act, and the other six are raised by a run already going.
			expect(defaultOpenAct('waiting', ALL, source), source).toBe(ACT_FOR_SOURCE[source]);
		}
	});

	it('lets the ask outrank the lifecycle, as the verdict itself does', () => {
		// `deriveVerdict` cannot produce this pairing — an ask always reports
		// `waiting` — but the rule is stated rather than left to that coincidence:
		// the act holding the ask beats the act holding the state.
		expect(defaultOpenAct('finished', ALL, 'diff_approval')).toBe('run');
		expect(defaultOpenAct('running', ALL, 'plan_approval')).toBe('plan');
	});

	it('falls back from the act an ask lives in by the same rule as from the state', () => {
		// A mid-run ask on a task whose Run act failed to load: nearest earlier.
		expect(defaultOpenAct('waiting', ['plan', 'output'], 'escalation')).toBe('plan');
		// A plan-time ask on a task that was never planned: nothing earlier, so
		// the earliest later act opens.
		expect(defaultOpenAct('waiting', ['run', 'output'], 'plan_approval')).toBe('run');
	});

	it('never opens an act the task does not have, for any source', () => {
		for (const source of SOURCES) {
			for (const acts of SUBSETS) {
				if (acts.length === 0) continue;
				expect(acts, `${source} · [${acts.join(', ')}]`).toContain(
					defaultOpenAct('waiting', acts, source)
				);
			}
		}
	});
});
