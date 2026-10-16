/**
 * The one summation two surfaces share.
 *
 * What these pin is the single thing the module exists to make impossible: a
 * total that reports a zero nobody measured. Every case below is a shape where
 * `total += value ?? 0` would produce a number and this must produce `null`.
 *
 * **The figures are deliberately unlike each other** — no count is a round
 * multiple of another and none matches a percentage — so a sum reading the wrong
 * field cannot coincide with the right answer. `input` *includes* `cacheRead` in
 * this provider's accounting, which is the fact the rate is built on: 9,600 of
 * 12,000 is 80%, and the wrong denominator gives 44%.
 */
import { describe, expect, it } from 'vitest';

import { cachedPercentOf, totalUsage, type TokenUsage } from './tokenUsage';

const INPUT = 12_000;
const OUTPUT = 384;
const CACHE_READ = 9_600;
const CACHE_CREATION = 1_150;

function usage(overrides: Partial<TokenUsage> = {}): TokenUsage {
	return {
		input: INPUT,
		output: OUTPUT,
		cacheRead: CACHE_READ,
		cacheCreation: CACHE_CREATION,
		...overrides
	};
}

/** A record that reported nothing at all — the shape a non-LLM event has. */
const NOTHING: TokenUsage = { input: null, output: null, cacheRead: null, cacheCreation: null };

describe('totalUsage — absence', () => {
	it('has no totals for a set where nothing reported a bill', () => {
		// Three spellings of the same absence, and none of them may answer with a
		// zeroed record: a caller renders `0 tok · 0% cached` from one, which is a
		// measurement the run never produced.
		expect(totalUsage([])).toBeNull();
		expect(totalUsage([NOTHING, NOTHING, NOTHING])).toBeNull();
		expect(totalUsage([null, undefined])).toBeNull();
	});

	it('leaves a field nothing reported at null while its siblings sum', () => {
		const totals = totalUsage([
			usage({ output: null, cacheRead: null, cacheCreation: null }),
			usage({ output: null, cacheRead: null, cacheCreation: null })
		]);
		expect(totals).toEqual({
			input: INPUT * 2,
			// Not `0`. No call in this set reported an output count, and a caller has
			// to be able to tell that from two calls that each emitted nothing.
			output: null,
			cacheRead: null,
			cacheCreation: null,
			calls: 2,
			cachedPercent: null
		});
	});

	it('counts a call that reported an explicit zero, because zero is a measurement', () => {
		const totals = totalUsage([{ input: 0, output: null, cacheRead: null, cacheCreation: null }]);
		expect(totals?.calls).toBe(1);
		expect(totals?.input).toBe(0);
	});

	it('ignores a figure that is not a number, rather than reading it as zero', () => {
		const totals = totalUsage([
			usage({ output: Number.NaN as unknown as number }),
			usage({ output: Number.POSITIVE_INFINITY as unknown as number })
		]);
		// Both calls reported *something*, so both count — but neither reported a
		// readable output count, so there is no output total.
		expect(totals?.calls).toBe(2);
		expect(totals?.output).toBeNull();
		expect(totals?.input).toBe(INPUT * 2);
	});
});

describe('totalUsage — sums', () => {
	it('counts live and journal delivery of the same call once', () => {
		const live = usage({ callId: 'call-1' });
		const journal = usage({ callId: 'call-1' });
		const nextCall = usage({ callId: 'call-2' });
		expect(totalUsage([live, journal, nextCall])?.input).toBe(INPUT * 2);
		expect(totalUsage([live, journal, nextCall])?.calls).toBe(2);
	});
	it('sums each figure over the calls that reported it and rates the whole set', () => {
		expect(totalUsage([usage(), usage(), usage()])).toEqual({
			input: INPUT * 3,
			output: OUTPUT * 3,
			cacheRead: CACHE_READ * 3,
			cacheCreation: CACHE_CREATION * 3,
			calls: 3,
			cachedPercent: 80
		});
	});

	it('sums over different subsets per figure, which is what a partial reporter produces', () => {
		const totals = totalUsage([
			usage({ cacheRead: null, cacheCreation: null }),
			// A warm call: same prompt size, and this time the cache figures arrived.
			usage()
		]);
		expect(totals?.input).toBe(INPUT * 2);
		expect(totals?.cacheRead).toBe(CACHE_READ);
		// The first call's cache usage is unknown; 40% would invent a cache miss.
		expect(totals?.cachedPercent).toBeNull();
	});

	it('skips the records that reported nothing without counting them as calls', () => {
		expect(totalUsage([NOTHING, usage(), NOTHING, usage(), NOTHING])?.calls).toBe(2);
	});
});

describe('cachedPercentOf', () => {
	it('divides by the input alone, because the input already includes the cached part', () => {
		// The trap: `cached / (input + cached)` gives 44% here, which is the wrong
		// side of the number an operator is watching for.
		expect(cachedPercentOf(CACHE_READ, INPUT)).toBe(80);
	});

	it('has no rate to report without both halves, and never reports a zero one', () => {
		expect(cachedPercentOf(null, INPUT)).toBeNull();
		expect(cachedPercentOf(CACHE_READ, null)).toBeNull();
		// A run with no cache participation must not read `0% cached`: nothing was
		// consulted, so nothing missed.
		expect(cachedPercentOf(0, INPUT)).toBeNull();
		expect(cachedPercentOf(CACHE_READ, 0)).toBeNull();
	});

	it('rounds to a whole percentage and cannot exceed the whole prompt', () => {
		expect(cachedPercentOf(1, 3)).toBe(33);
		expect(cachedPercentOf(2, 3)).toBe(67);
		expect(cachedPercentOf(INPUT * 2, INPUT)).toBe(100);
	});
});
