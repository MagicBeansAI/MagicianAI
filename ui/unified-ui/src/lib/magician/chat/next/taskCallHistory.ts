import { timedFetch } from '$lib/shared/fetch';

export interface CallUsage {
	tokens: number | null;
	costUsd: number | null;
	executionId: string | null;
}

/** Content-free ledger includes auxiliary/judge calls absent from the activity panel. */
export async function readTaskCallHistory(
	executionId: string,
	principal: string,
	workspace: string
): Promise<Map<string, CallUsage> | null> {
	const literal = `'${executionId.replace(/'/g, "''")}'`;
	const sql = `SELECT llm_call_id, input_tokens, output_tokens, cost_usd
FROM llm_calls WHERE (root_execution_id = ${literal} OR execution_id = ${literal})
AND COALESCE(provider_attempt_count, 1) <> 0`;
	const scope = new URLSearchParams({ principal, workspace });
	try {
		const response = await timedFetch(`/api/magician/v2/analytics/llm_calls/query?${scope}`, {
			method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ sql })
		});
		if (!response.ok) return null;
		const body = await response.json() as { columns?: string[]; rows?: unknown[][] };
		const columns = body.columns ?? [];
		if (!columns.includes('llm_call_id')) return null;
		const calls = new Map<string, CallUsage>();
		for (const row of body.rows ?? []) {
			const at = (column: string) => row[columns.indexOf(column)];
			const id = at('llm_call_id');
			if (typeof id !== 'string' || !id) continue;
			const input = numberOrNull(at('input_tokens'));
			const output = numberOrNull(at('output_tokens'));
			calls.set(id, {
				tokens: input === null && output === null ? null : (input ?? 0) + (output ?? 0),
				costUsd: numberOrNull(at('cost_usd')), executionId
			});
		}
		return calls;
	} catch {
		// Telemetry can be unavailable independently of the execution panel.
		return null;
	}
}

function numberOrNull(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}
