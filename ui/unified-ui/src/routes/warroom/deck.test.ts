import { describe, expect, it } from 'vitest';

import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

import {
	breathePeriodSeconds,
	classifySeverity,
	deriveDeckMode,
	fmtAge,
	fmtCompact,
	fmtUsd,
	isLiveArrival,
	nextEventRate,
	normalizeEventFrame,
	pacingRatio,
	recentErrorCount,
	type TapeRow
} from './deck';

describe('classifySeverity', () => {
	it('matches both PascalCase and dot-namespaced runtime emits', () => {
		// Without the dotted branches half the runtime's events fell into
		// `info` and the deck went grey during real activity.
		expect(classifySeverity('ExecutionFailed')).toBe('error');
		expect(classifySeverity('execution.failed')).toBe('error');
		expect(classifySeverity('ExecutionCompleted')).toBe('success');
		expect(classifySeverity('execution.completed')).toBe('success');
		expect(classifySeverity('ExecutionPaused')).toBe('warn');
		expect(classifySeverity('HitlRequested')).toBe('hitl');
		expect(classifySeverity('AgenticWaitingForUser')).toBe('hitl');
		expect(classifySeverity('ToolCallStarted')).toBe('info');
	});
});

describe('normalizeEventFrame', () => {
	it('unwraps the AgentEvent envelope and surfaces the inner type', () => {
		// The bulk of agent activity arrives as this envelope; without the
		// unwrap the tape reads an uninformative wall of "AgentEvent".
		const row = normalizeEventFrame(
			JSON.stringify({
				event_type: 'AgentEvent',
				data: {
					event: {
						event_type: 'cycle.completed',
						agent_id: 'agent-7',
						timestamp: 1_753_600_000_000,
						payload: { task_id: 'task-9' }
					}
				}
			}),
			1
		);
		expect(row).not.toBeNull();
		expect(row?.event_type).toBe('cycle.completed');
		expect(row?.agent_id).toBe('agent-7');
		expect(row?.task_id).toBe('task-9');
		expect(row?.severity).toBe('success');
		expect(row?.ts).toBe(1_753_600_000_000);
	});

	it('drops control frames and unparseable lines', () => {
		expect(normalizeEventFrame('{"event_type":"__events_ready"}', 1)).toBeNull();
		expect(normalizeEventFrame('not json', 1)).toBeNull();
	});

	it('treats literal-zero timestamps as missing', () => {
		// Legacy variants with `#[serde(default)] i64` serialize 0 when
		// unset; accepting that anchors every age display at epoch zero.
		const before = Date.now();
		const row = normalizeEventFrame(
			JSON.stringify({ event_type: 'ToolCallStarted', timestamp_ms: 0 }),
			1
		);
		expect(row?.ts).toBeGreaterThanOrEqual(before);
	});
});

describe('deck mode', () => {
	it('fault outranks attention outranks nominal', () => {
		expect(deriveDeckMode({ healthOk: false, pendingHitl: 5, recentErrors: 0 })).toBe('fault');
		expect(deriveDeckMode({ healthOk: true, pendingHitl: 1, recentErrors: 0 })).toBe('attention');
		expect(deriveDeckMode({ healthOk: true, pendingHitl: 0, recentErrors: 0 })).toBe('nominal');
	});

	it('an unprobed health check does not fault the deck', () => {
		// null = "not yet answered"; only a definitive failure goes red.
		expect(deriveDeckMode({ healthOk: null, pendingHitl: 0, recentErrors: 0 })).toBe('nominal');
	});

	it('an error burst faults the deck even with healthy components', () => {
		expect(deriveDeckMode({ healthOk: true, pendingHitl: 0, recentErrors: 3 })).toBe('fault');
	});
});

describe('live vs replayed frames', () => {
	it('counts only recent frames toward the live rate', () => {
		const now = 1_000_000;
		expect(isLiveArrival(now - 1_000, now)).toBe(true);
		// Backfill: the stream replays history on connect. Counting it made an
		// idle deck report 65.8 events/second.
		expect(isLiveArrival(now - 3_600_000, now)).toBe(false);
	});

	it('treats clock skew from the future as live', () => {
		const now = 1_000_000;
		expect(isLiveArrival(now + 2_000, now)).toBe(true);
	});
});

describe('event rate → breathing', () => {
	it('rate decays toward zero when arrivals stop', () => {
		let rate = 5;
		for (let i = 0; i < 60; i += 1) rate = nextEventRate(rate, 1_000, 0);
		expect(rate).toBeLessThan(0.7);
	});

	it('a quiet deck breathes slowly, a busy one quickens, never strobes', () => {
		expect(breathePeriodSeconds(0)).toBe(6);
		expect(breathePeriodSeconds(2)).toBeLessThan(4);
		expect(breathePeriodSeconds(1000)).toBeGreaterThan(1.7);
	});
});

describe('pacing + tape helpers', () => {
	it('pacingRatio clamps at 2 and refuses fabricated baselines', () => {
		expect(pacingRatio(5, 10)).toBe(0.5);
		expect(pacingRatio(30, 10)).toBe(2);
		// No baseline → null → the arc renders hollow, not full or zero.
		expect(pacingRatio(5, 0)).toBeNull();
		expect(pacingRatio(Number.NaN, 10)).toBeNull();
	});

	it('recentErrorCount only counts errors inside the window', () => {
		const now = 1_000_000;
		const rows: TapeRow[] = [
			{ id: 1, event_type: 'a.failed', ts: now - 10_000, agent_id: null, task_id: null, severity: 'error' },
			{ id: 2, event_type: 'b.failed', ts: now - 120_000, agent_id: null, task_id: null, severity: 'error' },
			{ id: 3, event_type: 'c.completed', ts: now - 5_000, agent_id: null, task_id: null, severity: 'success' }
		];
		expect(recentErrorCount(rows, now)).toBe(1);
	});
});

describe('formatters', () => {
	it('render dashes for missing data instead of inventing zeros', () => {
		expect(fmtUsd(null)).toBe('—');
		expect(fmtCompact(undefined)).toBe('—');
		expect(fmtUsd(2.4)).toBe('$2.40');
		expect(fmtCompact(1_234_567)).toBe('1.2M');
		expect(fmtCompact(43_210)).toBe('43K');
	});

	it('ages never go negative under clock skew', () => {
		expect(fmtAge(2_000, 1_000)).toBe('0s');
		expect(fmtAge(0, 90_000)).toBe('1m');
	});
});


describe('scheduler safety', () => {
	it('no deck component calls tick() from a reactive statement', () => {
		// Calling `tick()` inside `$:` re-enters Svelte's flush scheduler from
		// the microtask the statement just queued, which re-runs the statement,
		// which queues another tick() -- an endless microtask chain that pins
		// the main thread at 100% CPU.
		//
		// This does NOT fail loudly. `effect_update_depth_exceeded` only counts
		// re-runs within ONE flush, and each iteration here is a separate flush,
		// so the counter resets and the guard never trips. The page just stops
		// responding, with no error in the console. It cost a long debugging
		// session to find; the rule is cheap to enforce, so enforce it.
		//
		// Use requestAnimationFrame for post-render DOM work instead: it
		// schedules outside the scheduler and cannot re-enter it.
		const dir = __dirname;
		const offenders: string[] = [];
		for (const file of readdirSync(dir).filter((f) => f.endsWith('.svelte'))) {
			const src = readFileSync(join(dir, file), 'utf8');
			const script = src.split('</script>')[0] ?? '';
			// Reactive statement bodies: from `$:` to the end of that statement.
			for (const match of script.matchAll(/^\s*\$:[\s\S]*?(?=\n\s*(?:\$:|\}|<\/script>|$))/gm)) {
				if (/\btick\s*\(/.test(match[0])) offenders.push(file);
			}
		}
		expect(offenders).toEqual([]);
	});
});
