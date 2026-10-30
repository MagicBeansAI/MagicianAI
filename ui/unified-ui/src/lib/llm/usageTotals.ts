import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export interface UsageTotals {
	calls: number;
	input: number | null;
	output: number | null;
	cacheRead: number | null;
	cacheWrite: number | null;
	cost: number | null;
	unpricedCalls: number;
}

export interface UsageModelTotals extends UsageTotals { provider: string; model: string; }

/** Cached daily rollups preserve measured-call and availability counters. Chunk summaries describe child calls;
 * harness aggregates are billable estimates and must not be discarded merely
 * because their provider-attempt count is zero. No event-tail sums are added. */
export function usageTotalsSql(kind: 'chat' | 'task', id: string, byModel = false): string {
	if (!id.trim() || id.length > 256) throw new Error('Usage target is invalid');
	const literal = `'${id.replaceAll("'", "''")}'`;
	const where = kind === 'chat' ? `chat_session_id = ${literal}`
		: `(task_id = ${literal} OR root_execution_id IN (SELECT DISTINCT root_execution_id FROM llm_cost_daily WHERE task_id = ${literal} AND root_execution_id IS NOT NULL))`;
    return `SELECT ${byModel ? 'provider, model, ' : ''}SUM(measured_calls) AS calls,
CASE WHEN SUM(token_calls) > 0 THEN SUM(input_tokens) END AS input_tokens,
CASE WHEN SUM(token_calls) > 0 THEN SUM(output_tokens) END AS output_tokens,
CASE WHEN SUM(cache_read_calls) > 0 THEN SUM(cache_read_tokens) END AS cache_read_tokens,
CASE WHEN SUM(cache_write_calls) > 0 THEN SUM(cache_creation_tokens) END AS cache_creation_tokens,
CASE WHEN SUM(priced_calls) > 0 THEN SUM(cost_usd) END AS cost_usd,
SUM(measured_calls) - SUM(priced_calls) AS unpriced_calls
FROM llm_cost_daily WHERE ${where}${byModel ? ' GROUP BY provider, model ORDER BY cost_usd DESC NULLS LAST, calls DESC' : ''}`;
}

async function readUsage(kind: 'chat' | 'task', id: string, headers: Headers) {
	const started = performance.now();
	for (let attempt = 0; attempt < 3; attempt++) {
		const remaining = Math.ceil(150_000 - (performance.now() - started));
		if (remaining <= 0) throw new Error('Usage unavailable (timed out)');
		const response = await timedFetch('/api/magician/v2/analytics/llm_calls/query', {
			method: 'POST', headers, timeoutMs: remaining,
			body: JSON.stringify({ sql: usageTotalsSql(kind, id, true) })
		});
		if (!response.ok) {
			const failure = await response.json().catch(() => null);
			// Guard waits already back off server-side. Retry only this
			// transient refusal, within the original client deadline.
			if (attempt < 2 && response.status === 400 && typeof failure?.error === 'string'
				&& failure.error.startsWith('analytics DuckDB is busy;')) continue;
			throw new Error(`Usage unavailable (${response.status})`);
		}
		const body = await response.json();
		if (!Array.isArray(body.columns) || !Array.isArray(body.rows) || body.rows.some((row: unknown) => !Array.isArray(row))) throw new Error('Usage unavailable');
		return { columns: body.columns as string[], rows: body.rows as unknown[][] };
	}
	throw new Error('Usage unavailable');
}

function parseTotals(columns: string[], row: unknown[]): UsageTotals {
	const number = (column: string): number | null => {
		const value = row[columns.indexOf(column)];
		return typeof value === 'number' && Number.isFinite(value) ? value : null;
	};
	return { calls: number('calls') ?? 0, input: number('input_tokens'), output: number('output_tokens'),
		cacheRead: number('cache_read_tokens'), cacheWrite: number('cache_creation_tokens'),
		cost: number('cost_usd'), unpricedCalls: number('unpriced_calls') ?? 0 };
}

/** Share only in-flight work, keyed by the captured authenticated scope. */
const inFlightModels = new Map<string, Promise<UsageModelTotals[]>>();

export async function readUsageTotals(kind: 'chat' | 'task', id: string): Promise<UsageTotals> {
	const rows = await readUsageModels(kind, id);
	const sum = (key: 'input' | 'output' | 'cacheRead' | 'cacheWrite' | 'cost'): number | null => {
		const known = rows.map(row => row[key]).filter((value): value is number => value !== null);
		return known.length ? known.reduce((total, value) => total + value, 0) : null;
	};
	return { calls: rows.reduce((total, row) => total + row.calls, 0), input: sum('input'), output: sum('output'),
		cacheRead: sum('cacheRead'), cacheWrite: sum('cacheWrite'), cost: sum('cost'),
		unpricedCalls: rows.reduce((total, row) => total + row.unpricedCalls, 0) };
}

export function readUsageModels(kind: 'chat' | 'task', id: string): Promise<UsageModelTotals[]> {
	const headers = scopedRequestHeaders({ 'Content-Type': 'application/json' });
	const key = JSON.stringify([kind, id, Array.from(headers.entries()).sort()]);
	const pending = inFlightModels.get(key);
	if (pending) return pending;
	const request = readUsage(kind, id, headers).then(body => body.rows.map(row => ({ ...parseTotals(body.columns, row),
		provider: String(row[body.columns.indexOf('provider')] ?? 'Unknown provider'),
		model: String(row[body.columns.indexOf('model')] ?? 'Unknown model') })));
	inFlightModels.set(key, request);
	void request.finally(() => { if (inFlightModels.get(key) === request) inFlightModels.delete(key); }).catch(() => {});
	return request;
}
