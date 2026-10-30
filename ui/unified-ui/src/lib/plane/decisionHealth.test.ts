import { describe, expect, it } from 'vitest';
import { classifyOperation, formatMs, overallStatus, problemRate, sortByHealth, type DecisionOperationHealth } from './decisionHealth';

function op(overrides: Partial<DecisionOperationHealth> = {}): DecisionOperationHealth {
	return {
		operation: 'action_alignment',
		mode: 'gate',
		cases: 100,
		decided: 100,
		unresolved: 0,
		timeout: 0,
		error: 0,
		skipped: 0,
		items: { offered: 100, answered: 100, eligible: 100, failed: 0, no_fitting_model: 0, cancelled: 0, not_started: 0 },
		p50_ms: 400,
		p95_ms: 900,
		budget_ms: 3000,
		last_case_ms: 0,
		last_error_ms: null,
		last_error: null,
		causes: {},
		models: [],
		route_health: [],
		...overrides
	};
}

describe('decision model health classification', () => {
	it('is healthy at or below 10% problems and inside the budget', () => {
		expect(classifyOperation(op({ decided: 90, unresolved: 5, timeout: 5 })).status).toBe('healthy');
	});

	it('is degraded above 10% unresolved + timeout + error', () => {
		const verdict = classifyOperation(op({ decided: 89, unresolved: 6, timeout: 5 }));
		expect(verdict.status).toBe('degraded');
		expect(verdict.reasons[0]).toMatch(/11%/);
	});

	it('counts errors toward the problem rate', () => {
		expect(problemRate(op({ decided: 80, error: 20 }))).toBeCloseTo(0.2);
		expect(classifyOperation(op({ decided: 80, error: 20 })).status).toBe('degraded');
	});

	it('is failing above 50%', () => {
		expect(classifyOperation(op({ cases: 10, decided: 4, timeout: 3, error: 3 })).status).toBe('failing');
		expect(classifyOperation(op({ cases: 10, decided: 5, timeout: 5 })).status).toBe('degraded');
	});

	it('is degraded when p95 exceeds the caller budget even with no failures', () => {
		const verdict = classifyOperation(op({ p95_ms: 3500, budget_ms: 3000 }));
		expect(verdict.status).toBe('degraded');
		expect(verdict.reasons.join(' ')).toMatch(/exceeds/);
	});

	it('ignores latency when the budget is unknown', () => {
		expect(classifyOperation(op({ p95_ms: 60000, budget_ms: null })).status).toBe('healthy');
	});

	it('is degraded when the engine last reported a route model unhealthy', () => {
		const verdict = classifyOperation(op({ route_health: [{ model: 'jev', issue: 'structured_provider_rate_limit', observed_ms: 1 }, { model: 'kev', issue: null, observed_ms: 1 }] }));
		expect(verdict.status).toBe('degraded');
		expect(verdict.reasons.join(' ')).toMatch(/jev: structured_provider_rate_limit/);
	});

	it('is idle with no engine cases; skipped invocations do not count as failures', () => {
		const verdict = classifyOperation(op({ cases: 0, decided: 0, skipped: 4 }));
		expect(verdict.status).toBe('idle');
		expect(verdict.reasons[0]).toMatch(/4 skipped/);
		expect(problemRate(op({ cases: 0, decided: 0 }))).toBeNull();
	});

	it('sorts worst first and reports the worst status overall', () => {
		const ops = [
			op({ operation: 'ok' }),
			op({ operation: 'idle', cases: 0, decided: 0 }),
			op({ operation: 'bad', cases: 10, decided: 1, timeout: 9 }),
			op({ operation: 'slow', p95_ms: 5000 })
		];
		expect(sortByHealth(ops).map((row) => row.op.operation)).toEqual(['bad', 'slow', 'ok', 'idle']);
		expect(overallStatus(ops)).toBe('failing');
		expect(overallStatus([])).toBe('idle');
	});

	it('formats latency compactly', () => {
		expect(formatMs(null)).toBe('—');
		expect(formatMs(412)).toBe('412 ms');
		expect(formatMs(3500)).toBe('3.5 s');
		expect(formatMs(30372)).toBe('30 s');
	});
});
