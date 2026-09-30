import { parseNumericDuckDbValue } from '$lib/magician/components/generative/chartUtil';

export interface LlmUsageOverviewData {
	spendUsd: number | null;
	calls: number | null;
	retryRate: number | null;
	avgLatencyMs: number | null;
}

function numberOrNull(value: unknown): number | null {
	return parseNumericDuckDbValue(value) ?? null;
}

export function buildLlmUsageOverviewSql(where: string): string {
	return (
		`SELECT SUM(cost_usd) AS spend_usd, COUNT(*) AS calls, ` +
		`SUM(CASE WHEN attempt > 1 THEN 1 ELSE 0 END) * 1.0 / NULLIF(COUNT(*), 0) AS retry_rate, ` +
		`AVG(latency_ms) AS avg_latency_ms FROM llm_calls WHERE ${where}`
	);
}

export function llmUsageWindowWhere(days: number): string {
	const safeDays = Number.isFinite(days) ? Math.max(1, Math.floor(days)) : 7;
	return `timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL ${safeDays} DAYS)`;
}

export function parseLlmUsageOverview(
	record: Record<string, unknown> | undefined
): LlmUsageOverviewData {
	if (!record) {
		return { spendUsd: null, calls: null, retryRate: null, avgLatencyMs: null };
	}
	return {
		spendUsd: numberOrNull(record.spend_usd),
		calls: numberOrNull(record.calls),
		retryRate: numberOrNull(record.retry_rate),
		avgLatencyMs: numberOrNull(record.avg_latency_ms)
	};
}
