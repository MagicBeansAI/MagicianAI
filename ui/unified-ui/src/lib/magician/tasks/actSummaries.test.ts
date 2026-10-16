import { describe, it, expect } from 'vitest';
import {
	planSummary,
	runSummary,
	outputSummary,
	type PlanSummaryInput,
	type RunSummaryInput
} from './actSummaries';
import type { TaskPlanStatus } from '$lib/stores/taskStore';

/** A real epoch, so an approval timestamp behind it is a real date rather than a negative one. */
const NOW = Date.parse('2026-07-29T12:00:00Z');

const plan: PlanSummaryInput = {
	status: 'draft',
	approvedAt: null,
	questions: [],
	now: NOW
};

const run: RunSummaryInput = {
	steps: 14,
	retries: 0,
	elapsedMs: null,
	currentStep: null,
	totalSteps: null,
	// The base case records no events, so every existing line below reads
	// exactly as it did before the segment existed. The cases that do carry
	// events set it, and their counts are deliberately unlike the step counts
	// beside them.
	events: 0
};

/**
 * Every status with the exact line it renders. This table is not the
 * exhaustiveness guard — a seventh `TaskPlanStatus` would not fail here, it
 * would fail to compile `PLAN_STATUS_COPY`, which is a
 * `Record<TaskPlanStatus, string>`. What it covers is the copy itself:
 * asserting that a status produces *a* line proves far less than asserting
 * which line, and any non-empty string passes the first.
 *
 * `planning` and `eliciting` are rewritten because neither word tells a reader
 * anything; the other four are ordinary English and keep theirs.
 */
const STATUS_LINES: Array<[TaskPlanStatus, string]> = [
	['planning', 'being planned'],
	['eliciting', 'gathering what it needs before planning'],
	['draft', 'draft, not yet approved'],
	['approved', 'approved'],
	['rejected', 'rejected, so it will not run'],
	['failed', 'planning failed']
];

describe('planSummary', () => {
	it('quotes the first waiting question instead of counting them', () => {
		expect(
			planSummary({ ...plan, questions: ['Which quarter?', 'Include forecasts?'] })
		).toBe('2 questions waiting — "Which quarter?"');
	});

	it('uses the singular noun when there is one question', () => {
		expect(planSummary({ ...plan, questions: ['Which quarter?'] })).toBe(
			'1 question waiting — "Which quarter?"'
		);
	});

	it('trims the quoted question rather than rendering its whitespace', () => {
		expect(planSummary({ ...plan, questions: ['  Which quarter?\n'] })).toBe(
			'1 question waiting — "Which quarter?"'
		);
	});

	it('falls back to the bare count when the question carries no content', () => {
		// The one sanctioned use of a count: there is genuinely nothing to quote,
		// so `2 questions waiting — ""` would be a worse line, not a fuller one.
		expect(planSummary({ ...plan, questions: ['   ', 'Include forecasts?'] })).toBe(
			'2 questions waiting'
		);
		expect(planSummary({ ...plan, questions: [''] })).toBe('1 question waiting');
	});

	it('says a draft is unapproved when nothing is being asked', () => {
		expect(planSummary(plan)).toBe('draft, not yet approved');
	});

	it('says when the plan was approved, relative to now', () => {
		expect(planSummary({ ...plan, status: 'approved', approvedAt: NOW - 6 * 60_000 })).toBe(
			'approved 6m ago'
		);
		expect(planSummary({ ...plan, status: 'approved', approvedAt: NOW - 3 * 3_600_000 })).toBe(
			'approved 3h ago'
		);
		expect(planSummary({ ...plan, status: 'approved', approvedAt: NOW - 2 * 86_400_000 })).toBe(
			'approved 2d ago'
		);
	});

	it('says "just now" rather than "now ago" for a fresh approval', () => {
		// `formatRelativeTime` returns `now` under a minute and clamps a future
		// timestamp to it, so both of these reach the same branch — and neither
		// composes with ` ago`.
		expect(planSummary({ ...plan, status: 'approved', approvedAt: NOW - 5_000 })).toBe(
			'approved just now'
		);
		expect(planSummary({ ...plan, status: 'approved', approvedAt: NOW + 60_000 })).toBe(
			'approved just now'
		);
	});

	it('names the date rather than an "ago" phrase once the approval is over a week old', () => {
		// The exact rendering is the browser locale's, so this asserts the rule —
		// a date takes a preposition, not ` ago` — rather than one locale's output.
		const old = planSummary({ ...plan, status: 'approved', approvedAt: NOW - 30 * 86_400_000 });
		expect(old).toMatch(/^approved on \S/);
		// The suffix, not the substring: ` ago` is only ever appended, and a
		// localised month name can contain those three letters — Spanish
		// `agosto` would fail a containment check for an August date.
		expect(old).not.toMatch(/ ago$/);
	});

	it('omits the approval time rather than inventing one when it cannot be read', () => {
		for (const approvedAt of [null, Number.NaN, Number.POSITIVE_INFINITY]) {
			expect(planSummary({ ...plan, status: 'approved', approvedAt })).toBe('approved');
		}
	});

	it('ranks a waiting question above the approval', () => {
		// The verdict line above has already said `Waiting on you`; a header
		// leading with `approved` would contradict it and point nowhere. The
		// approval is still true underneath and readable once the act is open.
		const summary = planSummary({
			...plan,
			status: 'approved',
			approvedAt: NOW - 6 * 60_000,
			questions: ['Which quarter?']
		});
		expect(summary).toBe('1 question waiting — "Which quarter?"');
		expect(summary).not.toContain('approved');
	});

	it('reports the current status even when the plan was approved earlier', () => {
		// `approvedAt` outliving the approval is a real shape — a plan can be
		// rejected or redrafted after it was approved — and the status is the
		// current fact, so a stale timestamp must not resurrect `approved`.
		expect(planSummary({ ...plan, status: 'rejected', approvedAt: NOW - 6 * 60_000 })).toBe(
			'rejected, so it will not run'
		);
		expect(planSummary({ ...plan, status: 'draft', approvedAt: NOW - 6 * 60_000 })).toBe(
			'draft, not yet approved'
		);
	});

	// `approved` reaches its copy here because the fixture has no `approvedAt`;
	// the timestamped form is asserted above.
	it.each(STATUS_LINES)("renders the %s status in the reader's own words", (status, expected) => {
		expect(planSummary({ ...plan, status })).toBe(expected);
	});

	it('renders nothing rather than a claim it cannot support when the status is unknown', () => {
		expect(planSummary({ ...plan, status: null })).toBe('');
		// A question still carries the line, because the ask does not need a status.
		expect(planSummary({ ...plan, status: null, questions: ['Which quarter?'] })).toBe(
			'1 question waiting — "Which quarter?"'
		);
	});
});

describe('runSummary', () => {
	it('summarises a finished run and folds retries into their step', () => {
		expect(runSummary({ ...run, retries: 2, elapsedMs: 192_000 })).toBe(
			'14 steps · 2 retries · 3m 12s'
		);
		// The hour tier proves this is `durationIfKnown` and not a second
		// formatter that agrees with it only in the minute bucket.
		expect(runSummary({ ...run, retries: 2, elapsedMs: 80 * 60_000 })).toBe(
			'14 steps · 2 retries · 1h 20m'
		);
	});

	it('omits retries when there were none', () => {
		expect(runSummary({ ...run, elapsedMs: 192_000 })).toBe('14 steps · 3m 12s');
	});

	it('uses the singular for one step and one retry', () => {
		expect(runSummary({ ...run, steps: 1, retries: 1, elapsedMs: 192_000 })).toBe(
			'1 step · 1 retry · 3m 12s'
		);
	});

	it('shows position instead of totals while live', () => {
		// `steps` and `currentStep` are deliberately different here and in every
		// live case below. Equal fixtures would pass just as happily if the line
		// rendered the recorded step count in the position's place, which is the
		// one substitution these tests exist to catch.
		const live = runSummary({ ...run, currentStep: 4, totalSteps: 7 });
		expect(live).toBe('step 4 of 7');
		expect(live).not.toContain('14');
	});

	it('drops the denominator while live when the run was never planned', () => {
		expect(runSummary({ ...run, currentStep: 4, totalSteps: null })).toBe('step 4');
	});

	it('keeps retries and elapsed visible while live, so a struggling run is not hidden', () => {
		// Only the step count is swapped for the position. A live line carrying
		// nothing but `step 4 of 7` would repeat the verdict headline above it.
		expect(
			runSummary({
				steps: 14,
				retries: 2,
				elapsedMs: 100_000,
				currentStep: 4,
				totalSteps: 7,
				events: 0
			})
		).toBe('step 4 of 7 · 2 retries · 1m 40s');
	});

	it('drops the elapsed segment rather than its separator when the time cannot be read', () => {
		for (const elapsedMs of [null, -1_000, Number.NaN]) {
			expect(runSummary({ ...run, retries: 2, elapsedMs })).toBe('14 steps · 2 retries');
		}
	});

	it('keeps an instant run distinct from one with no timing recorded', () => {
		expect(runSummary({ ...run, elapsedMs: 0 })).toBe('14 steps · 0s');
		expect(runSummary({ ...run, elapsedMs: null })).toBe('14 steps');
	});

	it('says a run has not started rather than rendering an empty line', () => {
		expect(
			runSummary({
				steps: 0,
				retries: 0,
				elapsedMs: null,
				currentStep: null,
				totalSteps: null,
				events: 0
			})
		).toBe('not started');
	});

	/**
	 * The timeline segment. Its count is deliberately unlike every other number
	 * in these cases — 217 events against 14 steps, 2 retries and step 4 of 7 —
	 * so a line that rendered the wrong field in this slot cannot pass, which is
	 * the substitution that would produce design §4's forbidden `step 143 of 217`.
	 */
	it('counts events under their own noun, beside the steps rather than instead of them', () => {
		expect(runSummary({ ...run, retries: 2, events: 217, elapsedMs: 192_000 })).toBe(
			'14 steps · 2 retries · 217 events · 3m 12s'
		);
		// Live, the step count becomes a position and the event count does not
		// move: they are different things and only one of them has a denominator.
		expect(runSummary({ ...run, currentStep: 4, totalSteps: 7, events: 217 })).toBe(
			'step 4 of 7 · 217 events'
		);
	});

	it('uses the singular for one event', () => {
		expect(runSummary({ ...run, events: 1, elapsedMs: null })).toBe('14 steps · 1 event');
	});

	it('drops the segment rather than rendering `0 events`', () => {
		// A count of zero drops out like every other empty segment here. The panel
		// passes zero both for a run that recorded nothing and for one whose events
		// nobody read — a distinction this line cannot express and the act body
		// can, which is why it lives there and not in this signature.
		expect(runSummary({ ...run, events: 0, elapsedMs: 192_000 })).toBe('14 steps · 3m 12s');
		expect(runSummary({ ...run, events: 0, elapsedMs: 192_000 })).not.toContain('event');
	});
});

describe('outputSummary', () => {
	it('names one file and counts the rest by kind', () => {
		expect(
			outputSummary([
				{ name: 'report.md', kind: 'document' },
				{ name: 'revenue.png', kind: 'image' },
				{ name: 'costs.png', kind: 'image' }
			])
		).toBe('report.md and 2 images');
	});

	it('counts the whole remainder, not just the two a three-file list leaves', () => {
		// Three files leave a remainder of 2, which is also the count every other
		// case here produces — so the arithmetic and the plural are both unproven
		// until one list is longer.
		expect(
			outputSummary([
				{ name: 'report.md', kind: 'document' },
				{ name: 'revenue.png', kind: 'image' },
				{ name: 'costs.png', kind: 'image' },
				{ name: 'margin.png', kind: 'image' }
			])
		).toBe('report.md and 3 images');
	});

	it('names both files when there are exactly two', () => {
		expect(
			outputSummary([
				{ name: 'report.md', kind: 'document' },
				{ name: 'notes.md', kind: 'document' }
			])
		).toBe('report.md and notes.md');
	});

	it('names a lone file on its own', () => {
		expect(outputSummary([{ name: 'report.md', kind: 'document' }])).toBe('report.md');
	});

	it('is explicit about having produced nothing', () => {
		expect(outputSummary([])).toBe('no output');
	});

	it('counts a same-kind remainder by its own word', () => {
		expect(
			outputSummary([
				{ name: 'revenue.png', kind: 'image' },
				{ name: 'report.md', kind: 'document' },
				{ name: 'notes.md', kind: 'document' }
			])
		).toBe('revenue.png and 2 documents');
	});

	it('falls back to a neutral plural when the rest do not share a kind', () => {
		expect(
			outputSummary([
				{ name: 'report.md', kind: 'document' },
				{ name: 'revenue.png', kind: 'image' },
				{ name: 'raw.bin', kind: 'other' }
			])
		).toBe('report.md and 2 other files');
	});

	it('gives unclassifiable files the same neutral plural, so there is one rule not two', () => {
		expect(
			outputSummary([
				{ name: 'report.md', kind: 'document' },
				{ name: 'raw.bin', kind: 'other' },
				{ name: 'dump.bin', kind: 'other' }
			])
		).toBe('report.md and 2 other files');
	});

	it('names the first file, so the header matches the order the act lists them in', () => {
		const files: Parameters<typeof outputSummary>[0] = [
			{ name: 'revenue.png', kind: 'image' },
			{ name: 'report.md', kind: 'document' },
			{ name: 'costs.png', kind: 'image' }
		];
		expect(outputSummary(files)).toBe('revenue.png and 2 other files');
		expect(outputSummary([...files].reverse())).toBe('costs.png and 2 other files');
	});
});
