import { metricsRequest } from './decisionMetrics';

/** Mirrors `GET /api/magician/v2/plane/decision-metrics/health`. */
export interface DecisionItemCounts { offered: number; answered: number; eligible: number; failed: number; no_fitting_model: number; cancelled: number; not_started: number }
export interface DecisionModelHealth { model: string; answered: number; p50_ms: number | null; p95_ms: number | null }
export interface DecisionRouteHealth { model: string; issue: string | null; observed_ms: number }
export interface DecisionOperationHealth {
	operation: string;
	mode: string | null;
	cases: number;
	decided: number;
	unresolved: number;
	timeout: number;
	error: number;
	skipped: number;
	items: DecisionItemCounts;
	p50_ms: number | null;
	p95_ms: number | null;
	budget_ms: number | null;
	last_case_ms: number;
	last_error_ms: number | null;
	last_error: string | null;
	causes: Record<string, number>;
	models: DecisionModelHealth[];
	route_health: DecisionRouteHealth[];
}
export interface DecisionHealthReport {
	window_hours: number;
	since_ms: number;
	generated_at_ms: number;
	cases_scanned: number;
	unreadable_cases: number;
	truncated: boolean;
	operations: DecisionOperationHealth[];
	capture_dropped: number;
	write_errors: number;
	read_ms: number;
}

export type DecisionHealthStatus = 'failing' | 'degraded' | 'healthy' | 'idle';

/** Share of engine cases that did not produce an applied decision. */
export const DEGRADED_PROBLEM_RATE = 0.1;
export const FAILING_PROBLEM_RATE = 0.5;
/** Windows the backend accepts (`hours` 1..=168; cases are kept 7 days). */
export const HEALTH_WINDOWS = [1, 6, 24, 72, 168] as const;

export function fetchDecisionHealth(hours: number, signal?: AbortSignal): Promise<DecisionHealthReport> {
	return metricsRequest<DecisionHealthReport>(`/health?${new URLSearchParams({ hours: String(hours) })}`, signal);
}

/** Unresolved + timeout + error over engine cases; null when nothing ran. */
export function problemRate(op: Pick<DecisionOperationHealth, 'cases' | 'unresolved' | 'timeout' | 'error'>): number | null {
	if (!op.cases || op.cases <= 0) return null;
	return (op.unresolved + op.timeout + op.error) / op.cases;
}

export interface DecisionHealthVerdict { status: DecisionHealthStatus; reasons: string[] }

/**
 * failing: more than half of engine cases unresolved, timed out or errored.
 * degraded: more than 10%, p95 above the caller budget, or the engine last
 * reported a route model unhealthy. idle: no engine cases in the window.
 */
export function classifyOperation(op: DecisionOperationHealth): DecisionHealthVerdict {
	const rate = problemRate(op);
	if (rate === null) {
		return { status: 'idle', reasons: [op.skipped ? `${op.skipped} skipped before reaching the engine` : 'No decisions in this window'] };
	}
	const reasons: string[] = [];
	let status: DecisionHealthStatus = 'healthy';
	const pct = Math.round(rate * 100);
	if (rate > FAILING_PROBLEM_RATE) {
		status = 'failing';
		reasons.push(`${pct}% of decisions unresolved, timed out or errored`);
	} else if (rate > DEGRADED_PROBLEM_RATE) {
		status = 'degraded';
		reasons.push(`${pct}% of decisions unresolved, timed out or errored`);
	}
	if (op.budget_ms && op.p95_ms !== null && op.p95_ms > op.budget_ms) {
		if (status === 'healthy') status = 'degraded';
		reasons.push(`p95 ${formatMs(op.p95_ms)} exceeds the ${formatMs(op.budget_ms)} budget`);
	}
	const unhealthy = op.route_health.filter((route) => route.issue);
	if (unhealthy.length) {
		if (status === 'healthy') status = 'degraded';
		reasons.push(`Engine reported ${unhealthy.map((route) => `${route.model}: ${route.issue}`).join(', ')}`);
	}
	return { status, reasons };
}

const ORDER: Record<DecisionHealthStatus, number> = { failing: 0, degraded: 1, healthy: 2, idle: 3 };

/** Worst first, then busiest. */
export function sortByHealth(ops: DecisionOperationHealth[]): Array<{ op: DecisionOperationHealth; verdict: DecisionHealthVerdict }> {
	return ops
		.map((op) => ({ op, verdict: classifyOperation(op) }))
		.sort((a, b) => ORDER[a.verdict.status] - ORDER[b.verdict.status] || b.op.cases - a.op.cases || a.op.operation.localeCompare(b.op.operation));
}

export function overallStatus(ops: DecisionOperationHealth[]): DecisionHealthStatus {
	const sorted = sortByHealth(ops);
	return sorted.length ? sorted[0].verdict.status : 'idle';
}

export function formatMs(ms: number | null): string {
	if (ms === null || !Number.isFinite(ms)) return '—';
	return ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(ms < 10000 ? 1 : 0)} s`;
}

export function formatWindow(hours: number): string {
	return hours < 24 ? `${hours} h` : hours === 24 ? '24 h' : `${hours / 24} days`;
}
