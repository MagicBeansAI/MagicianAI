import { cachedPercentOf } from '$lib/llm/tokenUsage';
import { formatTokens, formatUsd } from '$lib/magician/tasks/taskTimeline';

/** Named LLM and typed Decision Engine responses share measured usage fields. */
export function llmStepUsageParts(payload?: Record<string, unknown>): string[] {
	if (!payload) return [];
	const correlation = payload.correlation as Record<string, unknown> | undefined;
	const availability = (payload.usage_availability ?? correlation?.usage_availability) as
		| Record<string, boolean>
		| undefined;
	const number = (value: unknown) =>
		typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
	const input = number(payload.input_tokens);
	const output = number(payload.output_tokens);
	const tokens = availability?.tokens === false || payload.usage_reported === false ||
		(input === null && output === null) ? null : (input ?? 0) + (output ?? 0);
	const cache = availability?.cache_read === false
		? null : cachedPercentOf(number(payload.cache_read_tokens), input);
	const cost = availability?.cost === false
		? null : formatUsd(number(payload.cost_usd) ?? number(payload.cost));
	return [
		tokens !== null && (tokens > 0 || availability?.tokens === true || payload.usage_reported === true)
			? `${formatTokens(tokens)} tok` : null,
		cache === null ? null : `${cache}% cached`,
		cost
	].filter((part): part is string => part !== null);
}
