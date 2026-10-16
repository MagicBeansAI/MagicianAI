import { parseNumericDuckDbValue } from '$lib/magician/components/generative/chartUtil';

/** The common call ledger owns costs once; this is a filtered view of it. */
export function decisionModelSql(where: string): string {
	return `SELECT provider, model, COUNT(*) AS calls, ` +
		`SUM(CASE WHEN success THEN 1 ELSE 0 END) AS succeeded, ` +
		`SUM(cost_usd) AS cost_usd, COUNT(cost_usd) AS priced_calls, ` +
		`SUM(input_tokens) AS input_tokens, SUM(output_tokens) AS output_tokens, ` +
		`COUNT(input_tokens) AS usage_calls, SUM(cache_read_tokens) AS cache_read_tokens, ` +
		`COUNT(cache_read_tokens) AS cache_read_calls, SUM(cache_creation_tokens) AS cache_creation_tokens, ` +
		`COUNT(cache_creation_tokens) AS cache_write_calls, ` +
		`AVG(latency_ms) AS latency_ms, quantile_cont(latency_ms, 0.95) AS p95_ms ` +
		`FROM llm_calls WHERE (${where}) AND provider LIKE 'decision:%' ` +
		`GROUP BY provider, model ORDER BY calls DESC LIMIT 100`;
}

export function modelCost(value: unknown): string {
	if (value == null || value === '') return 'Unknown';
	const cost = parseNumericDuckDbValue(value);
	if (cost == null || cost < 0) return 'Unknown';
	if (cost === 0) return '$0';
	if (cost < 0.00000001) return '<$0.00000001';
	return `$${cost.toFixed(cost < 0.0001 ? 8 : cost < 0.01 ? 6 : cost < 1 ? 4 : 2)}`;
}
