// Pure formatting rules for the /evals surface. No DOM, no network.
//
// The first describe block is the one that matters: an unknown cost and a
// genuine zero are DIFFERENT FACTS and must never render the same. A lane
// that spent money must not look free because a ledger query failed.
import { describe, expect, it } from 'vitest';

import {
	aggregateCost,
	aggregateCostTitle,
	formatAggregateCost,
	formatCost,
	formatDuration,
	formatRequirement,
	formatRunStatus,
	formatTimestamp,
	normalizeCost,
	runDisabledReason,
	runStatusTone,
	trendBarHeightPercent,
	UNKNOWN
} from './format';
import type { EvalLane } from './api';

function lane(overrides: Partial<EvalLane> = {}): EvalLane {
	return {
		id: 'test-agentic-compaction-eval',
		target: 'test-agentic-compaction-eval',
		kind: 'harness',
		requires: [],
		report_dir: 'evals/agentic-compaction',
		desc: 'Compaction fidelity',
		parse_error: null,
		line: 2383,
		readiness: { ready: true, missing: [] },
		last_run: null,
		...overrides
	};
}

describe('cost formatting', () => {
	it('renders an unknown cost as an em dash, never as zero', () => {
		expect(formatCost({ kind: 'unknown' })).toBe('—');
	});
	it('renders a genuine zero as $0.00', () => {
		expect(formatCost({ kind: 'known', usd: 0 })).toBe('$0.00');
	});
	it('renders cents at two decimals', () => {
		expect(formatCost({ kind: 'known', usd: 0.834 })).toBe('$0.83');
	});

	it('groups thousands so a large spend cannot be misread', () => {
		expect(formatCost({ kind: 'known', usd: 1234.5 })).toBe('$1,234.50');
		expect(formatCost({ kind: 'known', usd: 1_234_567.891 })).toBe('$1,234,567.89');
	});

	// A sub-cent spend rounds to "$0.00", which reads as free — the same lie
	// the unknown-vs-zero rule exists to prevent, one order of magnitude down.
	it('distinguishes a spend that merely rounds to zero from a real zero', () => {
		expect(formatCost({ kind: 'known', usd: 0.004 })).toBe('<$0.01');
		expect(formatCost({ kind: 'known', usd: 0.000001 })).toBe('<$0.01');
		expect(formatCost({ kind: 'known', usd: 0 })).toBe('$0.00');
	});

	it('keeps the sign on a negative amount rather than hiding it', () => {
		expect(formatCost({ kind: 'known', usd: -0.5 })).toBe('-$0.50');
	});

	// A number we cannot render is a number we do not know. It must degrade to
	// unknown, never to zero.
	it('treats an unusable known amount as unknown, not as zero', () => {
		expect(formatCost({ kind: 'known', usd: Number.NaN })).toBe('—');
		expect(formatCost({ kind: 'known', usd: Number.POSITIVE_INFINITY })).toBe('—');
		expect(formatCost(null)).toBe('—');
		expect(formatCost(undefined)).toBe('—');
	});

	it('exports the unknown marker so callers cannot invent their own', () => {
		expect(UNKNOWN).toBe('—');
	});
});

describe('cost normalisation at the wire boundary', () => {
	it('passes a well-formed known cost through', () => {
		expect(normalizeCost({ kind: 'known', usd: 1.5 })).toEqual({ kind: 'known', usd: 1.5 });
		expect(normalizeCost({ kind: 'known', usd: 0 })).toEqual({ kind: 'known', usd: 0 });
	});

	it('passes an explicit unknown through', () => {
		expect(normalizeCost({ kind: 'unknown' })).toEqual({ kind: 'unknown' });
	});

	// Every malformed shape collapses to unknown. Defaulting any of them to
	// `{known, usd: 0}` would print "$0.00" for a lane that may have spent.
	it('collapses every malformed shape to unknown, never to a known zero', () => {
		expect(normalizeCost(undefined)).toEqual({ kind: 'unknown' });
		expect(normalizeCost(null)).toEqual({ kind: 'unknown' });
		expect(normalizeCost({})).toEqual({ kind: 'unknown' });
		expect(normalizeCost({ kind: 'known' })).toEqual({ kind: 'unknown' });
		expect(normalizeCost({ kind: 'known', usd: 'abc' })).toEqual({ kind: 'unknown' });
		expect(normalizeCost({ kind: 'known', usd: Number.NaN })).toEqual({ kind: 'unknown' });
		expect(normalizeCost({ kind: 'nonsense' })).toEqual({ kind: 'unknown' });
		expect(normalizeCost(0)).toEqual({ kind: 'unknown' });
		expect(normalizeCost('$1.00')).toEqual({ kind: 'unknown' });
	});

	// A decimal serialised as a string is unambiguous, so accept it rather than
	// blanking out a cost we can read perfectly well.
	it('accepts a numeric string amount', () => {
		expect(normalizeCost({ kind: 'known', usd: '0.5' })).toEqual({ kind: 'known', usd: 0.5 });
	});
});

describe('aggregate cost', () => {
	it('sums a range where every cost is known', () => {
		const agg = aggregateCost([
			{ kind: 'known', usd: 1.25 },
			{ kind: 'known', usd: 0.75 }
		]);
		expect(agg).toEqual({ usd: 2, knownCount: 2, unknownCount: 0 });
		expect(formatAggregateCost(agg)).toBe('$2.00');
	});

	// The unknown-never-zero rule, one level up: a total that silently omits
	// unknown runs under-reports spend. Mark it as a floor instead.
	it('marks a partially-known total as a floor', () => {
		const agg = aggregateCost([{ kind: 'known', usd: 1.2 }, { kind: 'unknown' }]);
		expect(agg).toEqual({ usd: 1.2, knownCount: 1, unknownCount: 1 });
		expect(formatAggregateCost(agg)).toBe('≥$1.20');
		expect(aggregateCostTitle(agg)).toContain('1 run of unknown cost');
	});

	it('renders a wholly unknown total as an em dash', () => {
		const agg = aggregateCost([{ kind: 'unknown' }, { kind: 'unknown' }]);
		expect(agg).toEqual({ usd: 0, knownCount: 0, unknownCount: 2 });
		expect(formatAggregateCost(agg)).toBe('—');
	});

	// No runs is genuinely no spend — that IS zero, and saying so is honest.
	it('renders an empty range as a real zero', () => {
		const agg = aggregateCost([]);
		expect(agg).toEqual({ usd: 0, knownCount: 0, unknownCount: 0 });
		expect(formatAggregateCost(agg)).toBe('$0.00');
	});
});

describe('duration formatting', () => {
	it('renders sub-minute in seconds', () => expect(formatDuration(18_000)).toBe('18s'));
	it('renders minutes and seconds', () => expect(formatDuration(252_000)).toBe('4m12s'));

	it('renders a zero duration as zero, not as unknown', () => {
		expect(formatDuration(0)).toBe('0s');
	});

	it('renders a sub-second run as under a second rather than as zero', () => {
		expect(formatDuration(1)).toBe('<1s');
		expect(formatDuration(999)).toBe('<1s');
		expect(formatDuration(1_000)).toBe('1s');
	});

	it('drops seconds once a run is measured in hours', () => {
		expect(formatDuration(3_600_000)).toBe('1h0m');
		expect(formatDuration(3_723_000)).toBe('1h2m');
		expect(formatDuration(59_999)).toBe('59s');
		expect(formatDuration(60_000)).toBe('1m0s');
	});

	it('renders an unusable duration as unknown', () => {
		expect(formatDuration(-1)).toBe('—');
		expect(formatDuration(Number.NaN)).toBe('—');
		expect(formatDuration(null)).toBe('—');
		expect(formatDuration(undefined)).toBe('—');
	});
});

describe('run status', () => {
	it('labels the three terminal statuses', () => {
		expect(formatRunStatus('passed')).toBe('Passed');
		expect(formatRunStatus('failed')).toBe('Failed');
		expect(formatRunStatus('interrupted')).toBe('Interrupted');
	});

	// An unrecognised status must still be visible. Blanking it would make a
	// run look like it had no outcome at all.
	it('shows an unrecognised status verbatim rather than blanking it', () => {
		expect(formatRunStatus('weird')).toBe('weird');
		expect(formatRunStatus(null)).toBe('Unknown');
	});

	it('tones passed green, failed red, and interrupted as a warning', () => {
		expect(runStatusTone('passed')).toBe('success');
		expect(runStatusTone('failed')).toBe('error');
		expect(runStatusTone('interrupted')).toBe('warning');
		// Never green by default: an unknown outcome is not a pass.
		expect(runStatusTone('weird')).toBe('default');
		expect(runStatusTone(null)).toBe('default');
	});
});

describe('run gating', () => {
	it('allows a ready, well-formed lane', () => {
		expect(runDisabledReason(lane())).toBeNull();
	});

	it('names the missing services on an unready lane', () => {
		const reason = runDisabledReason(
			lane({
				kind: 'live',
				requires: ['ollama', 'magician'],
				readiness: { ready: false, missing: ['ollama', 'magician'] }
			})
		);
		expect(reason).toContain('Ollama');
		expect(reason).toContain('magician');
	});

	it('still explains an unready lane that did not say what is missing', () => {
		const reason = runDisabledReason(lane({ readiness: { ready: false, missing: [] } }));
		expect(reason).toBeTruthy();
		expect(reason).toContain('not ready');
	});

	// `kind: 'unknown'` means nobody successfully declared what this lane is,
	// so it may be the cost-bearing sort. It outranks readiness.
	it('refuses a lane of unknown kind even when everything it needs is up', () => {
		const reason = runDisabledReason(lane({ kind: 'unknown', parse_error: 'missing `kind=`' }));
		expect(reason).toContain('annotation');
		expect(runDisabledReason(lane({ kind: 'unknown', readiness: { ready: false, missing: ['ollama'] } })))
			.toBe(reason);
	});
});

describe('requirement labels', () => {
	it('labels the declared requirement kinds', () => {
		expect(formatRequirement('ollama')).toBe('Ollama');
		expect(formatRequirement('provider_keys')).toBe('provider keys');
		expect(formatRequirement('magician_binary')).toBe('magician binary');
	});

	// The backend may grow a requirement before this file learns about it.
	it('shows an unrecognised requirement verbatim', () => {
		expect(formatRequirement('kafka')).toBe('kafka');
	});
});

describe('trend bars', () => {
	it('scales a bar against the slowest run in the strip', () => {
		expect(trendBarHeightPercent(500, 1000)).toBe(50);
		expect(trendBarHeightPercent(1000, 1000)).toBe(100);
	});

	it('keeps a bar visible when it would otherwise vanish', () => {
		expect(trendBarHeightPercent(0, 1000)).toBe(6);
		expect(trendBarHeightPercent(1, 100_000)).toBe(6);
	});

	it('survives a degenerate strip without dividing by zero', () => {
		expect(trendBarHeightPercent(0, 0)).toBe(6);
		expect(trendBarHeightPercent(10, Number.NaN)).toBe(6);
	});
});

describe('timestamps', () => {
	// Locale-dependent output is deliberately untested; only the guard is.
	it('never renders an unusable timestamp as the epoch', () => {
		expect(formatTimestamp(null)).toBe('—');
		expect(formatTimestamp(Number.NaN)).toBe('—');
		expect(formatTimestamp(0)).toBe('—');
	});

	it('renders a real timestamp as something', () => {
		expect(formatTimestamp(1_700_000_000_000).length).toBeGreaterThan(0);
	});
});
