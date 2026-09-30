/**
 * Presentation rules for the `/evals` surface — pure, DOM-free, unit-tested
 * (`format.test.ts`).
 *
 * ## The rule this module exists to enforce
 *
 * **An unknown cost renders `—`. Never `$0.00`.**
 *
 * `{kind: 'unknown'}` and `{kind: 'known', usd: 0}` are different facts:
 * the first means the LLM ledger did not answer, the second means the run
 * genuinely spent nothing. Collapsing them makes a lane that burned $2 look
 * free, which is the single failure this page must not have. Every path that
 * could produce a number — the wire boundary ([`normalizeCost`]), the single
 * value ([`formatCost`]), and the range total ([`formatAggregateCost`]) —
 * degrades towards *unknown*, never towards zero.
 *
 * Two smaller cases of the same rule are folded in here deliberately:
 * a spend that merely *rounds* to zero renders `<$0.01`, and a total that
 * omits unknown-cost runs renders as a floor (`≥$1.20`), not as a fact.
 */

import type { EvalCost, EvalLane, EvalRequirement, EvalRunStatus } from './api';

/** The one marker for "we do not know". Exported so nothing re-invents it. */
export const UNKNOWN = '—';

/** Badge tones the page maps a run status onto. Mirrors `Badge.svelte`. */
export type EvalTone = 'default' | 'success' | 'warning' | 'error' | 'info';

// ── Cost ──────────────────────────────────────────────────────────────────────

/**
 * Coerces a server-sent cost into the union, collapsing every malformed shape
 * to `unknown`.
 *
 * Applied at the wire boundary (`api.ts`) so a backend that changes its
 * serialisation cannot leak a shape the renderer would guess at. A numeric
 * string is accepted because a decimal serialised as `"0.5"` is unambiguous;
 * anything else — a missing `usd`, a non-finite one, an unrecognised `kind` —
 * becomes unknown rather than a fabricated zero.
 */
export function normalizeCost(raw: unknown): EvalCost {
	if (!raw || typeof raw !== 'object') return { kind: 'unknown' };
	const candidate = raw as { kind?: unknown; usd?: unknown };
	if (candidate.kind !== 'known') return { kind: 'unknown' };

	const amount =
		typeof candidate.usd === 'number'
			? candidate.usd
			: typeof candidate.usd === 'string' && candidate.usd.trim() !== ''
				? Number(candidate.usd)
				: Number.NaN;

	return Number.isFinite(amount) ? { kind: 'known', usd: amount } : { kind: 'unknown' };
}

/**
 * Renders one run's cost.
 *
 * `—` for unknown (including a "known" amount we cannot actually render, since
 * a number we can't read is a number we don't know), `$0.00` only for a
 * genuine zero, and `<$0.01` for a spend that would otherwise round into
 * looking free.
 */
export function formatCost(cost: EvalCost | null | undefined): string {
	if (!cost || cost.kind !== 'known' || !Number.isFinite(cost.usd)) return UNKNOWN;
	return formatUsdAmount(cost.usd);
}

function formatUsdAmount(usd: number): string {
	const sign = usd < 0 ? '-' : '';
	const magnitude = Math.abs(usd);
	const fixed = magnitude.toFixed(2);
	// Derived from the actual rounding rather than a magic threshold, so it
	// stays correct whatever `toFixed` does at the boundary.
	if (fixed === '0.00' && magnitude > 0) return `${sign}<$0.01`;

	const [whole, cents] = fixed.split('.');
	return `${sign}$${groupThousands(whole)}.${cents}`;
}

function groupThousands(digits: string): string {
	let out = '';
	for (let i = 0; i < digits.length; i += 1) {
		const fromEnd = digits.length - i;
		out += digits[i];
		if (fromEnd > 1 && fromEnd % 3 === 1) out += ',';
	}
	return out;
}

/** A cost total over a set of runs, keeping the unknowns countable. */
export interface CostAggregate {
	/** Sum of the amounts we actually know. */
	usd: number;
	knownCount: number;
	unknownCount: number;
}

/**
 * Sums costs without pretending the unknown ones were zero — the count is
 * carried alongside so the renderer can say the total is a floor.
 */
export function aggregateCost(costs: Array<EvalCost | null | undefined>): CostAggregate {
	let usd = 0;
	let knownCount = 0;
	let unknownCount = 0;
	for (const cost of costs) {
		if (cost && cost.kind === 'known' && Number.isFinite(cost.usd)) {
			usd += cost.usd;
			knownCount += 1;
		} else {
			unknownCount += 1;
		}
	}
	// Kill the float dust `0.1 + 0.2` leaves behind before it reaches a cell.
	return { usd: Math.round(usd * 1e6) / 1e6, knownCount, unknownCount };
}

/**
 * Renders a range total. Three distinct outcomes, deliberately:
 *
 * - every cost known → the total;
 * - some known, some not → `≥` the known part, because a bare sum would
 *   silently under-report spend;
 * - nothing known → `—`;
 * - nothing at all → `$0.00`, which is honest: no runs is no spend.
 */
export function formatAggregateCost(aggregate: CostAggregate): string {
	if (aggregate.unknownCount === 0) return formatCost({ kind: 'known', usd: aggregate.usd });
	if (aggregate.knownCount === 0) return UNKNOWN;
	return `≥${formatCost({ kind: 'known', usd: aggregate.usd })}`;
}

/** Long-form explanation of an aggregate, for a `title` attribute. */
export function aggregateCostTitle(aggregate: CostAggregate): string {
	const { knownCount, unknownCount } = aggregate;
	const unknownPhrase = `${unknownCount} run${unknownCount === 1 ? '' : 's'} of unknown cost`;
	if (knownCount === 0 && unknownCount === 0) return 'No runs in this range.';
	if (unknownCount === 0) {
		return `Cost of ${knownCount} run${knownCount === 1 ? '' : 's'}.`;
	}
	if (knownCount === 0) {
		return `${unknownPhrase}. The ledger did not answer, so this is not zero — it is unknown.`;
	}
	return `At least ${formatCost({ kind: 'known', usd: aggregate.usd })} across ${knownCount} run${
		knownCount === 1 ? '' : 's'
	}; ${unknownPhrase} is not included.`;
}

// ── Duration ──────────────────────────────────────────────────────────────────

/**
 * Compact wall-clock duration: `18s`, `4m12s`, `1h2m`.
 *
 * A run shorter than a second reads `<1s` rather than `0s`, so a genuinely
 * instantaneous record (a target that never started) stays distinguishable
 * from a fast one. A negative or non-finite measurement is not a duration and
 * renders unknown.
 */
export function formatDuration(ms: number | null | undefined): string {
	if (ms === null || ms === undefined || !Number.isFinite(ms) || ms < 0) return UNKNOWN;
	if (ms === 0) return '0s';
	if (ms < 1_000) return '<1s';

	const totalSeconds = Math.floor(ms / 1_000);
	if (totalSeconds < 60) return `${totalSeconds}s`;

	const totalMinutes = Math.floor(totalSeconds / 60);
	if (totalMinutes < 60) return `${totalMinutes}m${totalSeconds % 60}s`;

	return `${Math.floor(totalMinutes / 60)}h${totalMinutes % 60}m`;
}

// ── Status ────────────────────────────────────────────────────────────────────

const STATUS_LABELS: Record<EvalRunStatus, string> = {
	passed: 'Passed',
	failed: 'Failed',
	interrupted: 'Interrupted'
};

/**
 * Labels a run status. An unrecognised one is shown verbatim rather than
 * blanked — a run with an outcome nobody understood is still a run with an
 * outcome, and hiding it would read as "no result".
 */
export function formatRunStatus(status: string | null | undefined): string {
	if (!status) return 'Unknown';
	return STATUS_LABELS[status as EvalRunStatus] ?? status;
}

/**
 * Badge tone for a status. Anything unrecognised is neutral, never `success`:
 * an outcome we cannot classify is not a pass.
 */
export function runStatusTone(status: string | null | undefined): EvalTone {
	switch (status) {
		case 'passed':
			return 'success';
		case 'failed':
			return 'error';
		case 'interrupted':
			return 'warning';
		default:
			return 'default';
	}
}

// ── Lanes ─────────────────────────────────────────────────────────────────────

const REQUIREMENT_LABELS: Record<string, string> = {
	ollama: 'Ollama',
	magician: 'magician server',
	magician_binary: 'magician binary',
	magicutor: 'magicutor',
	provider_keys: 'provider keys'
};

/** Human label for a declared requirement; unrecognised tokens pass through. */
export function formatRequirement(requirement: EvalRequirement): string {
	return REQUIREMENT_LABELS[requirement] ?? requirement;
}

/**
 * Why this lane cannot be started, or `null` when it can.
 *
 * The order matters. A lane whose `kind` did not parse is refused *before*
 * readiness is even consulted: nobody successfully declared what it is, so it
 * may well be the cost-bearing sort, and "ready" would be an answer to the
 * wrong question.
 */
export function runDisabledReason(lane: EvalLane): string | null {
	if (lane.kind === 'unknown') {
		return 'Unrunnable: this lane’s `## eval:` annotation is malformed, so nobody declared what kind of run this is.';
	}
	// A lane whose `kind` parsed can still carry a defect elsewhere in its
	// annotation — a misspelled `requires=` token, a duplicate id. The server
	// refuses those too (`runnable = parse_error.is_none() && kind.runnable()`),
	// so leaving the button enabled would offer a run that comes back 422. It is
	// checked AFTER `kind` so the unparseable-kind case keeps its own, more
	// specific wording.
	if (lane.parse_error) {
		return `Unrunnable: this lane’s \`## eval:\` annotation is malformed, so what it declares cannot be trusted — ${lane.parse_error}`;
	}
	if (lane.readiness?.ready) return null;

	const missing = (lane.readiness?.missing ?? []).filter((entry) => !!entry);
	if (missing.length === 0) {
		return 'This lane is not ready, and the probe did not report which service is missing.';
	}
	return `Not ready: missing ${missing.map(formatRequirement).join(', ')}.`;
}

// ── Misc ──────────────────────────────────────────────────────────────────────

/** Minimum bar height, so a fast run still draws something clickable. */
const MIN_TREND_BAR_PERCENT = 6;

/**
 * Height of one trend bar as a percentage of the slowest run in the strip.
 * Degenerate strips (a single zero-duration run, a NaN max) fall back to the
 * minimum rather than dividing by zero.
 */
export function trendBarHeightPercent(durationMs: number, maxDurationMs: number): number {
	if (!Number.isFinite(durationMs) || !Number.isFinite(maxDurationMs)) {
		return MIN_TREND_BAR_PERCENT;
	}
	if (durationMs <= 0 || maxDurationMs <= 0) return MIN_TREND_BAR_PERCENT;
	const scaled = Math.round((durationMs / maxDurationMs) * 100);
	return Math.min(100, Math.max(MIN_TREND_BAR_PERCENT, scaled));
}

/**
 * Absolute local timestamp for `title` attributes. Epoch zero and non-finite
 * values are treated as absent — rendering "1 Jan 1970" for a missing
 * timestamp is worse than admitting it is missing.
 */
export function formatTimestamp(epochMs: number | null | undefined): string {
	if (epochMs === null || epochMs === undefined || !Number.isFinite(epochMs) || epochMs <= 0) {
		return UNKNOWN;
	}
	return new Date(epochMs).toLocaleString();
}
