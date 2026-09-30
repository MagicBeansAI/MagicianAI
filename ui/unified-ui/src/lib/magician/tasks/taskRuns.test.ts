import { describe, expect, it } from 'vitest';

import { runOptionLabel, runStamp, runsSliceOf, type RunRecord } from './taskRuns';

/**
 * A fixed instant with a known local wall clock, and every other instant in this
 * file is an offset from it. **Local rather than UTC on purpose**: `runStamp`
 * renders the reader's own wall clock, so a test that asserted UTC digits would
 * pass or fail on the runner's timezone rather than on the function.
 *
 * So the assertions below never spell a date out — they compare against
 * `stampOf`, which derives the expected string from the same `Date` the function
 * reads. That checks the *shape* (`D Mon HH:MM`, zero-padded clock, no seconds,
 * no year) and the *identity* (this instant, not another) without pinning a
 * locale.
 */
const NOW = new Date(2026, 6, 2, 14, 32, 7).getTime();
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

function stampOf(at: number): string {
	const when = new Date(at);
	const pad = (value: number) => String(value).padStart(2, '0');
	return `${when.getDate()} ${MONTHS[when.getMonth()]} ${pad(when.getHours())}:${pad(when.getMinutes())}`;
}

function record(executionId: string, startedAt: number | null, status: string | null): RunRecord {
	return { executionId, startedAt, status };
}

describe('runStamp — day, month and clock, and nothing invented', () => {
	it('renders the day, the short month and a zero-padded clock', () => {
		expect(runStamp(NOW)).toBe(stampOf(NOW));
		// The shape, stated independently of the locale-free derivation above, so a
		// `stampOf` that drifted could not make this pass by agreeing with itself.
		expect(runStamp(NOW)).toMatch(/^\d{1,2} [A-Z][a-z]{2} \d{2}:\d{2}$/);
	});

	it('pads a single-digit hour and minute', () => {
		const early = new Date(2026, 0, 5, 9, 4, 0).getTime();
		expect(runStamp(early)).toMatch(/ 09:04$/);
	});

	it('carries no seconds and no year', () => {
		const stamp = runStamp(NOW) as string;
		expect(stamp).not.toContain('2026');
		// Three colon-separated groups would be `HH:MM:SS`; the picker's options are
		// compared across runs, and nobody picks a run by its second.
		expect(stamp.split(':')).toHaveLength(2);
	});

	it('is null for every spelling of "nothing timed this"', () => {
		// Zero is a sentinel rather than an instant — the rule every timestamp reader
		// in this directory applies. Rendering it would read `1 Jan 1970`, which is a
		// confident answer to a question nothing recorded.
		expect(runStamp(0)).toBeNull();
		expect(runStamp(null)).toBeNull();
		expect(runStamp(undefined)).toBeNull();
		expect(runStamp(Number.NaN)).toBeNull();
		expect(runStamp(Number.POSITIVE_INFINITY)).toBeNull();
	});
});

describe('runOptionLabel — segments that have nothing to say are dropped', () => {
	it('states the ordinal, when it ran and how it ended', () => {
		expect(runOptionLabel(3, NOW, 'failed')).toBe(`#3 · ${stampOf(NOW)} · failed`);
	});

	it('drops the instant rather than filling it', () => {
		expect(runOptionLabel(3, null, 'failed')).toBe('#3 · failed');
	});

	it('drops the status rather than filling it', () => {
		expect(runOptionLabel(3, NOW, null)).toBe(`#3 · ${stampOf(NOW)}`);
	});

	it('is the ordinal alone when neither arrived, and never empty', () => {
		expect(runOptionLabel(3, null, null)).toBe('#3');
		expect(runOptionLabel(3, null, '   ')).toBe('#3');
	});

	it('never spells absence as a word', () => {
		const label = runOptionLabel(1, null, null);
		for (const invented of ['unknown', 'undefined', 'null', 'NaN', 'Invalid']) {
			expect(label).not.toContain(invented);
		}
	});
});

describe('runsSliceOf — absent, not disabled', () => {
	it('is null at one run, which is the owner’s condition', () => {
		expect(runsSliceOf([record('exec-1', NOW, 'finished')], 'exec-1')).toBeNull();
	});

	it('is null with no runs at all', () => {
		expect(runsSliceOf([], 'exec-1')).toBeNull();
	});

	it('is null when the caller cannot say which run is shown', () => {
		const rows = [record('exec-1', NOW - HOUR, 'failed'), record('exec-2', NOW, 'finished')];
		expect(runsSliceOf(rows, null)).toBeNull();
		expect(runsSliceOf(rows, '')).toBeNull();
	});

	/**
	 * The case a task row that moved on between fetches produces. A `<select>`
	 * whose value is absent from its options renders the **first** one, so a
	 * control offered here would silently claim the reader is looking at the newest
	 * run — every value on screen individually correct and the reading false.
	 */
	it('is null when the selection is not one of the runs', () => {
		const rows = [record('exec-1', NOW - HOUR, 'failed'), record('exec-2', NOW, 'finished')];
		expect(runsSliceOf(rows, 'exec-9')).toBeNull();
	});

	it('offers a choice at two runs', () => {
		const rows = [record('exec-1', NOW - HOUR, 'failed'), record('exec-2', NOW, 'finished')];
		const slice = runsSliceOf(rows, 'exec-2');
		expect(slice).not.toBeNull();
		expect(slice?.options).toHaveLength(2);
		expect(slice?.selectedId).toBe('exec-2');
	});

	it('lists newest first', () => {
		const rows = [
			record('oldest', NOW - 2 * HOUR, 'failed'),
			record('newest', NOW, 'finished'),
			record('middle', NOW - HOUR, 'failed')
		];
		const slice = runsSliceOf(rows, 'newest');
		expect(slice?.options.map((option) => option.executionId)).toEqual([
			'newest',
			'middle',
			'oldest'
		]);
	});

	/**
	 * Ordinals count from the **oldest** run, which is the opposite direction from
	 * the listing. That is what makes a run's number stable: a reader who noticed
	 * `#2 failed` still finds `#2` after a fourth run lands.
	 */
	it('numbers from the oldest run, so a new run renumbers nothing', () => {
		const three = [
			record('a', NOW - 2 * HOUR, 'failed'),
			record('b', NOW - HOUR, 'failed'),
			record('c', NOW, 'finished')
		];
		const before = runsSliceOf(three, 'c');
		expect(before?.options.map((option) => `${option.executionId}#${option.ordinal}`)).toEqual([
			'c#3',
			'b#2',
			'a#1'
		]);

		const after = runsSliceOf([...three, record('d', NOW + HOUR, 'running')], 'd');
		const ordinalOf = (id: string) =>
			after?.options.find((option) => option.executionId === id)?.ordinal;
		expect(ordinalOf('a')).toBe(1);
		expect(ordinalOf('b')).toBe(2);
		expect(ordinalOf('c')).toBe(3);
		expect(ordinalOf('d')).toBe(4);
	});

	it('derives the order from the instants rather than trusting the payload', () => {
		// Deliberately scrambled: one wire sorts by `updated_at`, the other by
		// nothing at all, so an incoming order is a fact about the backend.
		const rows = [
			record('middle', NOW - HOUR, 'failed'),
			record('oldest', NOW - 2 * HOUR, 'cancelled'),
			record('newest', NOW, 'finished')
		];
		expect(runsSliceOf(rows, 'oldest')?.options.map((option) => option.ordinal)).toEqual([3, 2, 1]);
	});

	it('builds each option label from its own three facts', () => {
		const rows = [
			record('exec-1', NOW - HOUR, 'failed'),
			record('exec-2', NOW, 'finished')
		];
		const slice = runsSliceOf(rows, 'exec-2');
		expect(slice?.options.map((option) => option.label)).toEqual([
			`#2 · ${stampOf(NOW)} · finished`,
			`#1 · ${stampOf(NOW - HOUR)} · failed`
		]);
	});

	it('never renders a bare execution id as a label', () => {
		const rows = [
			record('exec_bc467fb2752c40b8bc8bd5773615813e', NOW - HOUR, 'failed'),
			record('exec_9d1e4a0c88b34f5f9a2c6e7d1b0f3a55', NOW, 'finished')
		];
		for (const option of runsSliceOf(rows, 'exec_9d1e4a0c88b34f5f9a2c6e7d1b0f3a55')?.options ?? []) {
			expect(option.label).not.toContain(option.executionId);
		}
	});

	it('drops a run with no id, because nothing could select it', () => {
		const rows = [
			record('exec-1', NOW - HOUR, 'failed'),
			record('', NOW, 'finished'),
			{ executionId: null, startedAt: NOW, status: 'finished' },
			record('exec-2', NOW, 'finished')
		];
		const slice = runsSliceOf(rows, 'exec-2');
		expect(slice?.options.map((option) => option.executionId)).toEqual(['exec-2', 'exec-1']);
	});

	it('is null once dropping the unusable rows leaves fewer than two', () => {
		expect(runsSliceOf([record('exec-1', NOW, 'finished'), record('', NOW, 'x')], 'exec-1')).toBeNull();
	});

	/**
	 * A duplicate would be two options that are the same run — indistinguishable
	 * to the reader, and it would shift every ordinal after it.
	 */
	it('collapses a duplicate id to one option', () => {
		const rows = [
			record('exec-1', NOW - HOUR, 'failed'),
			record('exec-1', NOW - HOUR, 'failed'),
			record('exec-2', NOW, 'finished')
		];
		const slice = runsSliceOf(rows, 'exec-1');
		expect(slice?.options).toHaveLength(2);
		expect(slice?.options.map((option) => option.ordinal)).toEqual([2, 1]);
	});

	it('keeps a run nothing timed, sorted oldest, with its status intact', () => {
		const rows = [record('untimed', null, 'cancelled'), record('timed', NOW, 'finished')];
		const slice = runsSliceOf(rows, 'timed');
		expect(slice?.options.map((option) => option.executionId)).toEqual(['timed', 'untimed']);
		const untimed = slice?.options.find((option) => option.executionId === 'untimed');
		expect(untimed?.ordinal).toBe(1);
		expect(untimed?.startedAt).toBeNull();
		expect(untimed?.label).toBe('#1 · cancelled');
	});

	it('carries the facts beside the label, so a reader is not the only consumer', () => {
		const rows = [record('exec-1', NOW - HOUR, 'failed'), record('exec-2', NOW, 'finished')];
		const [newest] = runsSliceOf(rows, 'exec-2')?.options ?? [];
		expect(newest).toMatchObject({
			executionId: 'exec-2',
			ordinal: 2,
			startedAt: NOW,
			status: 'finished'
		});
	});

	it('always selects one of the options it offers', () => {
		const rows = [
			record('exec-1', NOW - HOUR, 'failed'),
			record('exec-2', NOW, 'finished'),
			record('exec-3', NOW + HOUR, 'running')
		];
		for (const selected of ['exec-1', 'exec-2', 'exec-3']) {
			const slice = runsSliceOf(rows, selected);
			expect(slice?.selectedId).toBe(selected);
			expect(slice?.options.some((option) => option.executionId === selected)).toBe(true);
		}
	});
});
