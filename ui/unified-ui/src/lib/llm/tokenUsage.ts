/**
 * What model calls cost, summed — the one place that arithmetic lives.
 *
 * **It existed twice before this module did, and the two copies were the same
 * formula written from two different vocabularies.** The chat activity card sums
 * `input_tokens` / `cache_read_tokens` off raw turn events to draw its cache
 * chip; the task panel's Run act sums the same four figures off the timeline it
 * already built. Both had to know the one non-obvious fact in the whole
 * calculation — *this provider's `input_tokens` already includes the cached
 * portion*, so the hit rate is `cacheRead / input` and never
 * `cacheRead / (input + cacheRead)`, which reports roughly half the real rate on
 * a warm call. A third copy of that fact is a third chance for one surface to
 * report half of what another reports about the same run, with nothing on screen
 * able to notice.
 *
 * So the callers keep their own reader — only they know which events count and
 * where the fields live on their payload — and hand over a list of
 * `TokenUsage`. What is shared is the summation and the rate.
 *
 * **Absence and zero are not one value, and that is the whole of the contract.**
 * A provider decides which figures it reports: a call with no cache
 * participation carries no `cache_read_tokens` at all, and `0` there would be a
 * claim that the cache was consulted and missed. So every total is separately
 * nullable and stays `null` until some record reports that field, and a run
 * nothing reported usage for totals to `null` rather than to a record of four
 * zeros. Callers render nothing for a `null`; a zeroed record would render
 * `0 tok · 0% cached` about a run nobody measured, which is the fabrication the
 * task panel and `/evals` both exist to remove.
 */

/**
 * What one model call cost, as its caller read it off its own payload.
 *
 * Every field is separately nullable for the reason above: which of the four a
 * call reports is the provider's decision, not a property of the call.
 */
export interface TokenUsage {
	/** Stable logical call ID when two transports deliver the same terminal usage. */
	callId?: string;
	input: number | null;
	output: number | null;
	cacheRead: number | null;
	cacheCreation: number | null;
}

/**
 * What a set of calls cost together.
 *
 * Each total is `null` when **no** record in the set reported that field, and a
 * number when at least one did — so the four are sums over four possibly
 * different subsets of the calls, which is the honest answer when a provider
 * reports a field for some calls and not others. `12k → 380` built that way is
 * the same claim a per-call line makes, one level up.
 */
export interface UsageTotals {
	input: number | null;
	output: number | null;
	cacheRead: number | null;
	cacheCreation: number | null;
	/**
	 * How many records reported any usage at all — the denominator behind every
	 * figure above. Never `0`: a set where nothing reported usage has no totals,
	 * and `totalUsage` answers `null` for it.
	 */
	calls: number;
	/** See `cachedPercentOf`. `null` when there was no cache participation to rate. */
	cachedPercent: number | null;
}

/** A finite number, or `null`. Anything else is not a measurement. */
function measured(value: number | null | undefined): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

/**
 * `total + value`, leaving `total` alone for a value nothing reported — and
 * leaving it `null` until something does.
 *
 * The null-preserving add is what keeps "no record reported an output count"
 * distinguishable from "every record reported zero output tokens". A plain
 * `total += value ?? 0` collapses those two into one number and there is no
 * later point at which they can be told apart.
 */
function add(total: number | null, value: number | null | undefined): number | null {
	const next = measured(value);
	if (next === null) return total;
	return (total ?? 0) + next;
}

/**
 * The prompt-cache hit rate, as a whole percentage, or `null`.
 *
 * `cacheRead / input`, because **this provider's `input_tokens` already includes
 * the cached portion** — see the module note. Rounded, because a percentage
 * carrying a decimal invites a comparison between two runs that the measurement
 * cannot support, and clamped at 100 so a provider reporting more cache reads
 * than prompt tokens renders an impossible-but-bounded figure rather than
 * `130%`.
 *
 * `null` rather than `0` for both no-prompt and no-cache-read, and those are the
 * same answer for the same reason: neither is a run whose cache was consulted
 * and missed. A caller that renders `0% cached` for a run with no cache
 * participation has invented the miss.
 */
export function cachedPercentOf(
	cacheRead: number | null | undefined,
	input: number | null | undefined
): number | null {
	const served = measured(cacheRead) ?? 0;
	const prompt = measured(input) ?? 0;
	if (prompt <= 0 || served <= 0) return null;
	return Math.min(100, Math.round((served / prompt) * 100));
}

/**
 * What these calls cost together, or `null` when none of them reported a bill.
 *
 * A record counts as a call when **any** of its four figures is a finite number
 * — including an explicit zero, which is a provider saying "measured, and it was
 * nothing" rather than saying nothing. A record whose every field is absent
 * contributes to no total and is not counted, because there is no evidence it
 * was ever billed.
 *
 * Takes an iterable of possibly-`null` records so a caller can map its own event
 * list straight through without pre-filtering it — the filter it would write is
 * this one.
 */
export function totalUsage(
	records: Iterable<TokenUsage | null | undefined>
): UsageTotals | null {
	let input: number | null = null;
	let output: number | null = null;
	let cacheRead: number | null = null;
	let cacheCreation: number | null = null;
	let calls = 0;
	let cacheRateComplete = true;
	const seenCalls = new Set<string>();

	for (const record of records) {
		if (!record) continue;
		if (record.callId && seenCalls.has(record.callId)) continue;
		const reported =
			measured(record.input) !== null ||
			measured(record.output) !== null ||
			measured(record.cacheRead) !== null ||
			measured(record.cacheCreation) !== null;
		if (!reported) continue;
		if (record.callId) seenCalls.add(record.callId);
		cacheRateComplete &&= measured(record.input) !== null && measured(record.cacheRead) !== null;
		input = add(input, record.input);
		output = add(output, record.output);
		cacheRead = add(cacheRead, record.cacheRead);
		cacheCreation = add(cacheCreation, record.cacheCreation);
		calls += 1;
	}

	if (calls === 0) return null;
	return {
		input,
		output,
		cacheRead,
		cacheCreation,
		calls,
		// A partial sum of cache reads cannot rate all of the prompt tokens.
		cachedPercent: cacheRateComplete ? cachedPercentOf(cacheRead, input) : null
	};
}
