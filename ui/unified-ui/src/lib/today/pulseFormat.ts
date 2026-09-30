/**
 * Shared presentation helpers for Today's Pulse surfaces — the Today page's
 * pulse band and the /llm "Today vs yesterday" section. Pure formatting and
 * tone derivation: no fetches, no stores, no DOM.
 *
 * Tone contract: tones are derived from the RAW today/yesterday numbers,
 * never from formatted delta strings, so a rendering change in
 * `formatDelta` can't silently flip a chip's color. The neutrality
 * thresholds intentionally mirror `formatDelta`'s: a delta too small to
 * render is also too small to judge.
 */

import { CURRENCY_DELTA_NOISE_USD } from './pulseQueries';

/** Tint polarity for a chip's delta line. */
export type PulseTone = 'good' | 'bad' | 'neutral';

/** `$X.XX` at 2dp; from $100 up the cents are noise → 0dp. */
export function formatSpend(value: number): string {
	return value >= 100 ? `$${value.toFixed(0)}` : `$${value.toFixed(2)}`;
}

/**
 * INVERTED polarity, on purpose: MORE money spent than yesterday is the
 * "watch out" direction → 'bad'; spending less → 'good'. Spend appearing
 * where yesterday had none (formatDelta's "new today") is the more-spent
 * side → 'bad'. Sub-cent drift (formatDelta's currency noise band) and an
 * all-zero pair stay neutral.
 */
export function spendTone(today: number, yesterday: number): PulseTone {
	if (today === 0 && yesterday === 0) return 'neutral';
	if (yesterday === 0 && today > 0) return 'bad';
	const diff = today - yesterday;
	if (Math.abs(diff) < CURRENCY_DELTA_NOISE_USD) return 'neutral';
	return diff > 0 ? 'bad' : 'good';
}

/**
 * Normal polarity for counts: more than yesterday → 'good', fewer → 'bad'.
 * Neutral in exactly the cases `formatDelta(…, 'count')` renders empty —
 * an all-zero pair, or a delta that rounds to 0% (same Math.round of the
 * percentage). Activity appearing from a zero yesterday ("new today")
 * reads 'good'.
 */
export function countTone(today: number, yesterday: number): PulseTone {
	if (today === 0 && yesterday === 0) return 'neutral';
	if (yesterday === 0 && today > 0) return 'good';
	const pct = Math.round(((today - yesterday) / yesterday) * 100);
	if (pct === 0) return 'neutral';
	return pct > 0 ? 'good' : 'bad';
}

/** Display form of a count delta: "+18% vs yday" / "new today" / "". */
export function withYday(deltaText: string): string {
	if (deltaText === '' || deltaText === 'new today') return deltaText;
	return `${deltaText} vs yday`;
}

/** Accessible-name suffix for a delta line. */
export function deltaAria(deltaText: string): string {
	if (deltaText === '') return '';
	return deltaText === 'new today' ? ', none yesterday' : `, ${deltaText} versus yesterday`;
}
