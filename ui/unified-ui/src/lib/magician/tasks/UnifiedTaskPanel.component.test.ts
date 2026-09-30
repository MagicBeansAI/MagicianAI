import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { HitlOpenTarget, HitlSource } from '$lib/hitl/types';

import UnifiedTaskPanelHarness from '../../../test/fixtures/UnifiedTaskPanelHarness.svelte';
import type { TaskAskState } from './taskAsk';
import type { TaskFilePreview } from './taskFilePreview';
import type { ProvenanceEntry } from './TaskActSection.svelte';
import { VERDICT_MARKER } from './TaskVerdictLine.svelte';
import UnifiedTaskPanel, {
	type TaskPanelModel,
	type TaskPanelOutput,
	type TaskPanelPlan,
	type TaskPanelRun
} from './UnifiedTaskPanel.svelte';
import type { TaskPanelRuns } from './taskRuns';
import type { ActId } from './taskCapabilities';
import { timelineClock, type TimelineEntry } from './taskTimeline';
import { deriveVerdict } from './taskVerdict';

afterEach(cleanup);

/**
 * One clock for every fixture, so every duration below is a deliberate
 * difference rather than an accident of `Date.now()`.
 */
const NOW = 1_000_000;

/**
 * The numbers that could stand in for one another are all different, because a
 * fixture where two of them match cannot tell a correct panel from one wiring
 * the wrong field through:
 *
 * | | |
 * |---|---|
 * | recorded run steps | 4 |
 * | retries across those steps | 5 |
 * | the live step | 3 |
 * | the planned total | 7 |
 *
 * The same rule applies to the timestamps — `3m 12s` elapsed, `6m` since
 * approval, `4m` since the ask was raised, `2m` since the last successful load
 * — and to provenance, where all six values across the three acts are distinct
 * so an act rendering another act's ids cannot pass.
 */
const PLAN: TaskPanelPlan = {
	status: 'approved',
	approvedAt: NOW - 6 * 60_000,
	steps: ['Read the quarterly exports', 'Summarise revenue by region'],
	questions: [],
	provenance: [
		{ label: 'Plan id', value: 'pl_9f2c4e' },
		{ label: 'Revision', value: 'r3' }
	]
};

const RUN: TaskPanelRun = {
	steps: [
		{
			label: 'Read 3 files',
			durationMs: 3_000,
			retries: 0,
			status: 'completed',
			origin: 'step',
			capability: 'read_file',
			delegate: null,
			blocking: false
		},
		{
			label: 'Searched memory',
			durationMs: 12_000,
			retries: 3,
			status: 'failed',
			origin: 'step',
			capability: 'memory_search',
			delegate: 'research-agent',
			blocking: false
		},
		{
			label: 'Wrote the summary',
			durationMs: 45_000,
			retries: 0,
			status: 'in_progress',
			origin: 'step',
			capability: null,
			delegate: 'writer-agent',
			blocking: false
		},
		// No duration recorded: the omit-don't-guess rule has to be visible in the
		// body too, and a fixture where every step is timed cannot show it. The
		// status is `null` for the same reason — a record that never said how a
		// step ended is a shape the rows have to survive. Neither the capability
		// nor the agent is recorded either, which is the row that proves the
		// attribution line is absent rather than blank.
		{
			label: 'Rendered the chart',
			durationMs: null,
			retries: 2,
			status: null,
			origin: 'step',
			capability: null,
			delegate: null,
			blocking: false
		}
	],
	// The base fixture is a run nothing observed the events of, so every case
	// below that is not about the timeline renders exactly as it did before the
	// slice existed. `TIMELINE` is opted into per case.
	timeline: null,
	// The base fixture is a run that delegated nothing, so the responsibility
	// block is absent from every case that does not opt in — which is the shape
	// every task in the suite had before it existed.
	responsibility: null,
	provenance: [
		{ label: 'Execution id', value: 'ex_7b1d20' },
		{ label: 'Worker', value: 'runner-04' }
	]
};

/**
 * Three files whose *four* per-row facts all differ, so a row wiring one field
 * through to another cannot pass: one has a size and no mime, one has a mime and
 * no size, and one has neither. Only the two images could carry a thumbnail, and
 * only one of them has a URL — the third row is the caller that could not mint
 * one, which every affordance has to survive.
 */
const OUTPUT: TaskPanelOutput = {
	files: [
		{
			name: 'report.md',
			kind: 'document',
			path: 'q3/report.md',
			mediaType: 'text/markdown',
			sizeBytes: null,
			url: '/api/magician/v3/tasks/task_alpha/outputs/q3/report.md'
		},
		{
			name: 'revenue.png',
			kind: 'image',
			path: 'charts/revenue.png',
			mediaType: null,
			sizeBytes: 20_480,
			url: '/api/magician/v3/tasks/task_alpha/outputs/charts/revenue.png'
		},
		{
			name: 'regions.png',
			kind: 'image',
			path: 'charts/regions.png',
			mediaType: null,
			sizeBytes: null,
			url: null
		}
	],
	summary: null,
	provenance: [
		{ label: 'Output id', value: 'out_51aa9c' },
		{ label: 'Directory', value: '/tasks/9f2c/out' }
	]
};

const BASE: TaskPanelModel = {
	id: 'task_alpha',
	status: 'running',
	queuedFor: null,
	attention: null,
	ask: null,
	error: null,
	currentStep: 3,
	totalSteps: 7,
	currentStepLabel: 'Searching memory for "quarterly plan"',
	elapsedMs: 192_000,
	lastProgressAt: NOW - 30_000,
	plan: PLAN,
	run: RUN,
	output: OUTPUT,
	// The common case: one execution, so no picker. Every existing case below
	// therefore asserts the control's *absence* as a side effect, which is what
	// makes "one run renders no control" hard to regress.
	runs: null
};

const makeTask = (overrides: Partial<TaskPanelModel> = {}): TaskPanelModel => ({
	...BASE,
	...overrides
});

/**
 * The two task kinds, as the panel is allowed to know them: **by what they
 * have.** `PLANNED` stands for a normal task and `UNPLANNED` for an internal
 * one, and they differ in exactly one field — whether a Plan act arrived. Every
 * other value is shared, so any difference the panel renders between them is a
 * difference in declared capability and nothing else.
 */
const PLANNED = makeTask();
const UNPLANNED = makeTask({ plan: null });

const emptyOutput = (): TaskPanelOutput => ({ files: [], summary: null, provenance: [] });

const sections = (container: HTMLElement) =>
	Array.from(container.querySelectorAll<HTMLElement>('section[data-act]'));

const actOrder = (container: HTMLElement) =>
	sections(container).map((section) => section.dataset.act);

/** The header button of an act section: its first element child. */
const headerOf = (section: HTMLElement) =>
	(Array.from(section.children).find((el) => el.tagName === 'BUTTON') as HTMLButtonElement) ?? null;

/** Which acts are expanded. A list rather than a flag, so "two open" fails loudly. */
const openActs = (container: HTMLElement) =>
	sections(container)
		.filter((section) => headerOf(section)?.getAttribute('aria-expanded') === 'true')
		.map((section) => section.dataset.act);

const verdictRoot = (container: HTMLElement) =>
	container.querySelector<HTMLElement>('[data-verdict-state]');

const verdictDetail = (container: HTMLElement) =>
	container.querySelector('.verdict__detail')?.textContent ?? null;

const summaryOf = (container: HTMLElement, act: ActId) => {
	const section = sections(container).find((candidate) => candidate.dataset.act === act);
	return section?.querySelector('.act__summary')?.textContent ?? null;
};

function renderPanel(props: {
	task?: TaskPanelModel | null;
	loadError?: string | null;
	lastLoadedAt?: number | null;
	now?: number;
	answerAsk?: boolean;
	askState?: TaskAskState | null;
}) {
	return render(UnifiedTaskPanel, {
		props: {
			task: props.task ?? null,
			loadError: props.loadError ?? null,
			lastLoadedAt: props.lastLoadedAt ?? null,
			now: props.now ?? NOW,
			answerAsk: props.answerAsk ?? false,
			askState: props.askState ?? null
		}
	});
}

/**
 * One model per verdict state, plus both halves of the `waiting` split, with the
 * act each one should open. The `opens` column varies across all three acts, so
 * a panel that opened one act always — or that ignored the attention source —
 * fails rather than coincidentally matching.
 */
const STATE_CASES: Array<{ label: string; task: TaskPanelModel; opens: ActId }> = [
	{
		label: 'queued',
		task: makeTask({ status: 'queued', currentStep: null, lastProgressAt: null }),
		opens: 'plan'
	},
	{ label: 'running', task: makeTask(), opens: 'run' },
	{ label: 'stalled', task: makeTask({ lastProgressAt: NOW - 6 * 60_000 }), opens: 'run' },
	{ label: 'paused', task: makeTask({ status: 'paused' }), opens: 'run' },
	{
		label: 'failed',
		task: makeTask({ status: 'failed', error: "Couldn't read revenue.csv" }),
		opens: 'run'
	},
	{ label: 'cancelled', task: makeTask({ status: 'cancelled' }), opens: 'run' },
	{ label: 'archived', task: makeTask({ status: 'archived' }), opens: 'output' },
	{ label: 'finished', task: makeTask({ status: 'finished' }), opens: 'output' },
	{
		label: 'waiting on a plan-time ask',
		task: makeTask({
			attention: { source: 'plan_approval', summary: null, raisedAt: NOW - 4 * 60_000 }
		}),
		opens: 'plan'
	},
	{
		label: 'waiting on a mid-run ask',
		task: makeTask({
			attention: { source: 'diff_approval', summary: null, raisedAt: NOW - 4 * 60_000 }
		}),
		opens: 'run'
	}
];

describe('UnifiedTaskPanel — one panel, both task kinds', () => {
	it('never asks what kind of task this is', () => {
		// Design §2 makes this the acceptance test for the whole design rather than
		// a style preference, so it is executed rather than left to review: "if
		// anyone ever needs `if (internal)` inside the panel, the unification has
		// failed". Asserted against the source because there is no render that can
		// observe a branch nobody took.
		const source = readFileSync(
			// From the project root rather than `import.meta.url`, which vite rewrites
			// to a non-`file:` scheme this cannot read.
			resolve(process.cwd(), 'src/lib/magician/tasks/UnifiedTaskPanel.svelte'),
			'utf8'
		);

		// Identity first: `readFileSync` throws on a missing path but not on the
		// wrong one, and a `not.toMatch` against some other file would pass while
		// asserting nothing.
		expect(source).toContain('TaskVerdictLine');
		expect(source).not.toMatch(/internal/i);
	});

	it('renders both task kinds through the same component', () => {
		for (const task of [PLANNED, UNPLANNED]) {
			const { container, unmount } = renderPanel({ task });
			expect(screen.getByRole('status')).toBeTruthy();
			expect(verdictRoot(container)).toHaveAttribute('data-verdict-state', 'running');
			unmount();
		}
	});

	it('renders the acts each task declares, and only those', () => {
		const { container: planned, unmount } = renderPanel({ task: PLANNED });
		expect(actOrder(planned)).toEqual(['plan', 'run', 'output']);
		unmount();

		const { container: unplanned } = renderPanel({ task: UNPLANNED });
		expect(actOrder(unplanned)).toEqual(['run', 'output']);
	});

	it('omits the Plan act for a task without one, rather than disabling it', () => {
		const { container } = renderPanel({ task: UNPLANNED });

		// Absent from the document, not present-and-greyed: a disabled act is the
		// type check sneaking back in through styling (design §4).
		expect(screen.queryByRole('button', { name: /^Plan/ })).toBeNull();
		expect(container.querySelector('section[data-act="plan"]')).toBeNull();
		expect(container.querySelector('[disabled]')).toBeNull();
	});

	it('renders everything else identically for both kinds, so only the capability differs', () => {
		const { container: planned, unmount } = renderPanel({ task: PLANNED });
		const plannedRun = summaryOf(planned, 'run');
		const plannedOutput = summaryOf(planned, 'output');
		const plannedVerdict = verdictRoot(planned)?.textContent;
		unmount();

		const { container: unplanned } = renderPanel({ task: UNPLANNED });

		expect(summaryOf(unplanned, 'run')).toBe(plannedRun);
		expect(summaryOf(unplanned, 'output')).toBe(plannedOutput);
		expect(verdictRoot(unplanned)?.textContent).toBe(plannedVerdict);
	});
});

/**
 * The shape the migration's last two surfaces hand it: an **execution**, which
 * has a run and nothing else.
 *
 * `/crew/<id>`'s agent cycle carries no plan and no outputs endpoint that will
 * answer for its `agent-cycle:` id, so `toExecutionPanelModel` yields a model
 * with one act. The plan for that migration says a panel showing one act
 * honestly beats a second panel existing — which is only true if the panel does
 * something sensible with one, so it is asserted rather than assumed.
 */
describe('UnifiedTaskPanel — a model that has only a run', () => {
	const RUN_ONLY = makeTask({ plan: null, output: null });

	it('renders the verdict and the one act, with nothing standing in for the others', () => {
		const { container } = renderPanel({ task: RUN_ONLY });

		expect(actOrder(container)).toEqual(['run']);
		// Absent, not empty and not disabled: an empty Output act would answer
		// `no output` about a run nobody read the outputs of.
		expect(screen.queryByRole('button', { name: /^Plan/ })).toBeNull();
		expect(screen.queryByRole('button', { name: /^Output/ })).toBeNull();
		expect(container.querySelector('[disabled]')).toBeNull();
		// The verdict still leads: it is what the reader opened the panel to find
		// out, and it does not depend on any act being present.
		expect(verdictRoot(container)).toHaveAttribute('data-verdict-state', 'running');
	});

	it('opens the only act it has, whatever act the state is about', () => {
		// `finished` wants Output and `queued` wants Plan; neither exists, and
		// `defaultOpenAct`'s directional fallback has to land on Run for both
		// rather than leaving the panel with every act shut.
		for (const status of ['queued', 'running', 'finished'] as const) {
			const { container, unmount } = renderPanel({
				task: makeTask({ plan: null, output: null, status })
			});
			expect(openActs(container), status).toEqual(['run']);
			unmount();
		}
	});
});

describe('UnifiedTaskPanel — fixed order, one act open', () => {
	it('keeps the acts in lifecycle order whatever the state', () => {
		for (const { label, task } of STATE_CASES) {
			const { container, unmount } = renderPanel({ task });
			expect(actOrder(container), label).toEqual(['plan', 'run', 'output']);
			unmount();
		}
	});

	it('opens the act the state is about, and never more than one', () => {
		for (const { label, task, opens } of STATE_CASES) {
			const { container, unmount } = renderPanel({ task });
			expect(openActs(container), label).toEqual([opens]);
			unmount();
		}
	});

	it('closes the act that was open when the reader opens another', async () => {
		const { container } = renderPanel({ task: PLANNED });
		expect(openActs(container)).toEqual(['run']);

		await fireEvent.click(screen.getByRole('button', { name: /^Plan/ }));

		expect(openActs(container)).toEqual(['plan']);
	});

	it('closes the open act when the reader clicks its own header', async () => {
		const { container } = renderPanel({ task: PLANNED });

		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

		// A disclosure button whose click does nothing is broken, so the invariant
		// the panel owns is "never two open" rather than "always exactly one".
		expect(openActs(container)).toEqual([]);
	});

	it('keeps the act the reader opened across a refresh', async () => {
		const { container, rerender } = renderPanel({ task: PLANNED });

		await fireEvent.click(screen.getByRole('button', { name: /^Output/ }));
		expect(openActs(container)).toEqual(['output']);

		// A poll advancing the clock must not throw away the reader's choice —
		// which is what recomputing the open act from state alone would do, on
		// every tick, invisibly.
		await rerender({ now: NOW + 1_000 });

		expect(openActs(container)).toEqual(['output']);
	});

	/**
	 * The pair below is the whole identity rule, and neither half means anything
	 * without the other: one mechanism must both survive a poll and end at a task
	 * boundary, and a panel that only ever resets — or only ever keeps — passes
	 * exactly one of them.
	 *
	 * Both swap the model directly, so no reload, no remount and no caller is
	 * involved. Whatever else clears panel state elsewhere, these fail if
	 * `TaskPanelModel.id` stops being what the panel keys the choice on.
	 */
	it('drops the act the reader chose when a different task arrives', async () => {
		const { container, rerender } = renderPanel({ task: makeTask({ id: 'task_alpha' }) });

		await fireEvent.click(screen.getByRole('button', { name: /^Plan/ }));
		expect(openActs(container)).toEqual(['plan']);

		// The two models differ in **exactly one field**, and it is the id — same
		// status, same acts, same everything rendered. So nothing but identity can
		// drop the choice here: Plan is still present, so the "the act they chose
		// is gone" fallback cannot fire, and a second task in the same state is the
		// shape where carrying a choice over looks entirely correct.
		await rerender({ task: makeTask({ id: 'task_beta' }) });

		expect(openActs(container)).toEqual(['run']);
	});

	it('keeps it when the same task arrives again, however much else moved', async () => {
		const { container, rerender } = renderPanel({ task: makeTask({ id: 'task_alpha' }) });

		await fireEvent.click(screen.getByRole('button', { name: /^Plan/ }));
		expect(openActs(container)).toEqual(['plan']);

		// A poll: a whole new model object for the same task, with the run moved on
		// and the clock advanced. A reset keyed on the prop rather than on the id
		// passes the test above and fails this one.
		await rerender({
			task: makeTask({ id: 'task_alpha', currentStep: 4, lastProgressAt: NOW + 25_000 }),
			now: NOW + 30_000
		});

		expect(openActs(container)).toEqual(['plan']);
	});

	it('follows the state again once the act the reader chose is gone', async () => {
		const { container, rerender } = renderPanel({ task: PLANNED });

		await fireEvent.click(screen.getByRole('button', { name: /^Output/ }));
		expect(openActs(container)).toEqual(['output']);

		await rerender({ task: makeTask({ output: null }) });

		// Their choice cannot be honoured, so the panel opens what the state asks
		// for rather than leaving the column shut with nothing to read.
		expect(actOrder(container)).toEqual(['plan', 'run']);
		expect(openActs(container)).toEqual(['run']);
	});

	it('moves the open act as the state moves, while the reader has chosen nothing', async () => {
		const { container, rerender } = renderPanel({ task: PLANNED });
		expect(openActs(container)).toEqual(['run']);

		await rerender({ task: makeTask({ status: 'finished' }) });

		expect(openActs(container)).toEqual(['output']);
	});

	it('opens the nearest earlier act when the one the state is about failed to load', () => {
		// Run 404s, leaving Plan and Output either side of the gap (design §6).
		const { container } = renderPanel({ task: makeTask({ run: null }) });

		expect(actOrder(container)).toEqual(['plan', 'output']);
		expect(openActs(container)).toEqual(['plan']);
	});
});

describe('UnifiedTaskPanel — where an ask is answered', () => {
	/**
	 * Literals rather than the module's own map: a test importing it would assert
	 * only that the map equals itself. Six of the eight are run-time asks, which
	 * is why keying the open act on the verdict state alone — `waiting` → Plan —
	 * is wrong for most blocked tasks.
	 */
	const ACT_FOR_SOURCE: Record<HitlSource, ActId> = {
		plan_approval: 'plan',
		clarification: 'plan',
		agentic: 'run',
		user_request: 'run',
		approval: 'run',
		escalation: 'run',
		diff_approval: 'run',
		service_health: 'run',
	bot_auth: 'run'
	};

	it('opens the act each HITL source is answered in', () => {
		for (const [source, act] of Object.entries(ACT_FOR_SOURCE) as Array<[HitlSource, ActId]>) {
			const { container, unmount } = renderPanel({
				task: makeTask({ attention: { source, summary: null, raisedAt: NOW - 4 * 60_000 } })
			});

			expect(verdictRoot(container), source).toHaveAttribute('data-verdict-state', 'waiting');
			expect(openActs(container), source).toEqual([act]);
			unmount();
		}
	});

	it('falls back when the act an ask lives in is absent', () => {
		// A mid-run ask on a task whose Run act failed to load: nearest earlier.
		const { container: noRun, unmount } = renderPanel({
			task: makeTask({
				run: null,
				attention: { source: 'diff_approval', summary: null, raisedAt: NOW - 4 * 60_000 }
			})
		});
		expect(openActs(noRun)).toEqual(['plan']);
		unmount();

		// A plan-time ask on a task that was never planned: nothing earlier exists,
		// so the earliest later act opens.
		const { container: noPlan } = renderPanel({
			task: makeTask({
				plan: null,
				attention: { source: 'plan_approval', summary: null, raisedAt: NOW - 4 * 60_000 }
			})
		});
		expect(openActs(noPlan)).toEqual(['run']);
	});
});

describe('UnifiedTaskPanel — `no output` is a verdict a live task has not earned', () => {
	const outputSummaryFor = (task: TaskPanelModel) => {
		const { container, unmount } = renderPanel({ task });
		const line = summaryOf(container, 'output');
		unmount();
		return line;
	};

	it('says nothing yet while the task can still produce something', () => {
		for (const task of [
			makeTask({ output: emptyOutput() }),
			makeTask({ status: 'queued', currentStep: null, output: emptyOutput() }),
			// Stalled is still `running`: it may yet advance, so it has not earned
			// the finality claim either.
			makeTask({ lastProgressAt: NOW - 6 * 60_000, output: emptyOutput() })
		]) {
			expect(outputSummaryFor(task)).toBe('—');
		}
	});

	it('says no output once the task has stopped with nothing to show', () => {
		for (const status of ['finished', 'failed', 'cancelled']) {
			expect(outputSummaryFor(makeTask({ status, error: 'boom', output: emptyOutput() }))).toBe(
				'no output'
			);
		}
	});

	it('reads the lifecycle rather than the verdict, so an ask cannot hide that a task is done', () => {
		// The pair that separates the two readings. The verdict for both is
		// `waiting` — an ask outranks everything (design §3) — while the tasks
		// underneath are finished and running, and the honest Output summary
		// differs.
		const ask = { source: 'plan_approval' as const, summary: null, raisedAt: NOW - 4 * 60_000 };

		expect(
			outputSummaryFor(makeTask({ status: 'finished', attention: ask, output: emptyOutput() }))
		).toBe('no output');
		expect(
			outputSummaryFor(makeTask({ status: 'running', attention: ask, output: emptyOutput() }))
		).toBe('—');
	});

	it('names the files whatever the state, once there are any', () => {
		expect(outputSummaryFor(makeTask())).toBe('report.md and 2 images');
		expect(outputSummaryFor(makeTask({ status: 'finished' }))).toBe('report.md and 2 images');
	});

	it('lists every file in the body, where the header names only the first', () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });

		// The header's `report.md and 2 images` is L1 and counts the rest; the body
		// is L2 and has to name them, or opening the act tells the reader nothing
		// the closed header did not.
		expect(openActs(container)).toEqual(['output']);
		expect(
			Array.from(container.querySelectorAll('.output-file__name')).map((file) => file.textContent)
		).toEqual(['report.md', 'revenue.png', 'regions.png']);
	});

	/**
	 * The split the row draws, and it is between **who performs the action**, not
	 * between kinds of control. Open and reveal are OS actions the panel cannot
	 * perform, so they wait for a caller that says it can; opening a tab and
	 * saving a file are authenticated browser actions owned by the panel, so they
	 * need no caller and appear as soon as the URL is known.
	 */
	it('withholds the OS actions until the caller says it can act on them', () => {
		const { container, queryByRole } = renderPanel({ task: makeTask({ status: 'finished' }) });

		// **Named, not counted by tag.** This asserted `no <button> in a row`
		// until an image row grew a Preview control — which is also caller-free,
		// because the browser loads an `<img>` — and the assertion failed while
		// the property it was written for still held. "Is a button" was a proxy
		// for "is an OS action" and the two stopped coinciding.
		expect(queryByRole('button', { name: 'Open report.md' })).toBeNull();
		expect(queryByRole('button', { name: 'Reveal report.md' })).toBeNull();
		expect(queryByRole('button', { name: 'In tab report.md' })).toBeInTheDocument();
		expect(queryByRole('button', { name: 'Download report.md' })).toBeInTheDocument();
		expect(container.querySelectorAll('a.output-file__action')).toHaveLength(0);
	});

	it('offers no controls at all on a row whose URL the caller could not mint', () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });

		// `regions.png` is the fixture's URL-less row. Absent, not disabled — the
		// same rule the acts follow, applied one level down.
		const rows = Array.from(container.querySelectorAll<HTMLElement>('.output-file'));
		const withoutUrl = rows.find((row) => row.textContent?.includes('regions.png'));
		expect(withoutUrl?.querySelectorAll('.output-file__action')).toHaveLength(0);
		expect(withoutUrl?.querySelector('.output-file__thumb')).toBeNull();
	});

	it('names each file in its controls, so several rows of buttons stay distinct', async () => {
		const { getByRole } = render(UnifiedTaskPanel, {
			props: { task: makeTask({ status: 'finished' }), now: NOW, outputActions: true }
		});

		// Every row's controls are reachable by the file they act on — not by
		// position, and not by a shared `Open`.
		for (const name of ['report.md', 'revenue.png', 'regions.png']) {
			expect(getByRole('button', { name: `Open ${name}` })).toBeTruthy();
			expect(getByRole('button', { name: `Reveal ${name}` })).toBeTruthy();
		}
	});

	it('reports which row was acted on, because a display name cannot locate a file', async () => {
		const { getByRole, getByTestId } = render(UnifiedTaskPanelHarness, {
			props: { task: makeTask({ status: 'finished' }), now: NOW, outputActions: true }
		});

		// The third row and then the second, never the first: an index hard-coded
		// to 0, or read off a list the panel rebuilt, passes on row one and fails
		// on both of these.
		await fireEvent.click(getByRole('button', { name: 'Open regions.png' }));
		await fireEvent.click(getByRole('button', { name: 'Reveal revenue.png' }));

		expect(getByTestId('panel-file-events').textContent).toBe(
			'open:regions.png@2 reveal:revenue.png@1'
		);
	});
});

/**
 * The largest single loss in the swap, and the reason this component now depends
 * on another one at all. For a task whose deliverable *is* a written report,
 * `Finished · Wrote report.md` plus a filename is not the deliverable — it is a
 * receipt for it.
 */
/**
 * What the row list lost against the panel it replaced: the old Output card
 * handed its files to the chat block renderer, which gave a thumbnail, a size, a
 * mime, a download and an open-in-tab. The row list showed a name and two OS
 * actions, so an image output was a filename and nothing else.
 */
describe('UnifiedTaskPanel — an output row is more than a filename', () => {
	const rowFor = (container: HTMLElement, name: string) =>
		Array.from(container.querySelectorAll<HTMLElement>('.output-file')).find((row) =>
			row.textContent?.includes(name)
		) ?? null;

	it('shows a bearer-fetched thumbnail for an addressable image, and for nothing else', () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });

		// `revenue.png` is the image with a URL; `report.md` is a document with
		// one, and `regions.png` an image without. All three matter: a rule keyed
		// on the kind alone would give the third a broken `<img>`, and one keyed
		// on the URL alone would put a 2.5rem preview beside a markdown file.
		const thumb = rowFor(container, 'revenue.png')?.querySelector('img');
		expect(thumb?.getAttribute('src')).not.toBe(
			'/api/magician/v3/tasks/task_alpha/outputs/charts/revenue.png'
		);
		expect(thumb?.getAttribute('alt')).toBe('revenue.png');
		expect(rowFor(container, 'report.md')?.querySelector('.output-file__thumb')).toBeNull();
		expect(rowFor(container, 'regions.png')?.querySelector('.output-file__thumb')).toBeNull();
	});

	it('carries the size and the mime, and only the ones that were recorded', () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });

		// One row has a mime and no size, one a size and no mime, one neither —
		// so a meta line joining two segments unconditionally leaves a stray
		// separator on two of the three.
		expect(rowFor(container, 'report.md')?.querySelector('.output-file__meta')?.textContent).toBe(
			'text/markdown'
		);
		expect(rowFor(container, 'revenue.png')?.querySelector('.output-file__meta')?.textContent).toBe(
			'20 KB'
		);
		expect(rowFor(container, 'regions.png')?.querySelector('.output-file__meta')).toBeNull();
	});

	it('offers the two authenticated browser actions, named for the file they act on', () => {
		const { getByRole } = render(UnifiedTaskPanel, {
			props: { task: makeTask({ status: 'finished' }), now: NOW }
		});

		const tab = getByRole('button', { name: 'In tab report.md' });
		const download = getByRole('button', { name: 'Download report.md' });
		expect(tab).toHaveAttribute('type', 'button');
		expect(download).toHaveAttribute('type', 'button');
		expect(tab).not.toHaveAttribute('href');
		expect(download).not.toHaveAttribute('href');
	});
});

/**
 * Reading an output **in place**. The row list could say what a task wrote and
 * not what it wrote; every control on it took the reader somewhere else.
 *
 * The fixture is one file per branch of the render, and every row differs in
 * more than the branch it exercises — a different name, a different path, a
 * different mime — so a panel keying off the wrong field lands on the wrong
 * renderer rather than coincidentally on the right one.
 */
describe('UnifiedTaskPanel — an output you can read without leaving', () => {
	const previewFile = (
		name: string,
		overrides: Partial<TaskPanelOutput['files'][number]> = {}
	): TaskPanelOutput['files'][number] => ({
		name,
		kind: 'document',
		path: `out/${name}`,
		mediaType: null,
		sizeBytes: null,
		url: `/api/magician/v3/tasks/task_alpha/outputs/out/${name}`,
		...overrides
	});

	const PREVIEWABLE: TaskPanelOutput = {
		files: [
			previewFile('report.md', { mediaType: 'text/markdown' }),
			previewFile('rows.csv', { mediaType: 'text/csv' }),
			previewFile('run.json', { mediaType: 'application/json' }),
			previewFile('build.log', { mediaType: 'text/plain' }),
			previewFile('chart.png', { kind: 'image', mediaType: 'image/png' }),
			// Nothing this panel can draw: the row keeps its four controls and
			// offers no fifth.
			previewFile('slides.pdf', { mediaType: 'application/pdf' }),
			// Previewable by kind, unaddressable in fact.
			previewFile('orphan.md', { mediaType: 'text/markdown', url: null })
		],
		summary: null,
		provenance: []
	};

	const withPreviews = (overrides: Partial<TaskPanelModel> = {}) =>
		makeTask({ status: 'finished', output: PREVIEWABLE, ...overrides });

	const rowFor = (container: HTMLElement, name: string) =>
		Array.from(container.querySelectorAll<HTMLElement>('.output-file')).find((row) =>
			row.querySelector('.output-file__name')?.textContent === name
		) ?? null;

	const previewToggle = (container: HTMLElement, name: string) =>
		rowFor(container, name)?.querySelector<HTMLButtonElement>('button[aria-expanded]') ?? null;

	const previewBody = (container: HTMLElement, name: string) =>
		rowFor(container, name)?.querySelector<HTMLElement>('.output-preview') ?? null;

	const openBodies = (container: HTMLElement) =>
		Array.from(container.querySelectorAll<HTMLElement>('.output-preview')).map(
			(body) => body.dataset.previewKind
		);

	const ready = (index: number, text: string) => ({
		index,
		status: 'ready' as const,
		text,
		detail: null
	});

	function renderPreviews(props: {
		task?: TaskPanelModel;
		filePreviews?: boolean;
		filePreview?: TaskFilePreview | null;
	} = {}) {
		return render(UnifiedTaskPanelHarness, {
			props: {
				task: props.task ?? withPreviews(),
				now: NOW,
				outputActions: true,
				filePreviews: props.filePreviews ?? true,
				filePreview: props.filePreview ?? null
			}
		});
	}

	/**
	 * Three things must be true and they fail for different reasons: an address,
	 * a way to draw the bytes, and — for everything but an image — a caller
	 * willing to fetch them. Absent, never disabled.
	 */
	it('offers a preview only where all three of its preconditions hold', () => {
		const { container } = renderPreviews();

		for (const name of ['report.md', 'rows.csv', 'run.json', 'build.log', 'chart.png']) {
			expect(previewToggle(container, name)).not.toBeNull();
		}
		// Nothing to draw, and an address that does not exist.
		expect(previewToggle(container, 'slides.pdf')).toBeNull();
		expect(previewToggle(container, 'orphan.md')).toBeNull();
	});

	/**
	 * The same split the row's other controls draw, one level down: what the
	 * browser performs needs no caller, what needs a request does. An `<img>` is
	 * loaded by the browser, so an image row previews wherever the URL is known.
	 */
	it('previews an image without a caller, and nothing else without one', () => {
		const { container } = renderPreviews({ filePreviews: false });

		expect(previewToggle(container, 'chart.png')).not.toBeNull();
		for (const name of ['report.md', 'rows.csv', 'run.json', 'build.log']) {
			expect(previewToggle(container, name)).toBeNull();
		}
	});

	it('starts collapsed, and asks for nothing until a row is expanded', async () => {
		const { container, getByTestId } = renderPreviews();

		// Rule 3, and the reason it is worth a test: five previewable rows that
		// each cost a request on render is five requests to look at a file list.
		expect(container.querySelectorAll('.output-preview')).toHaveLength(0);
		expect(getByTestId('panel-file-events').textContent).toBe('');

		await fireEvent.click(previewToggle(container, 'rows.csv')!);
		expect(getByTestId('panel-file-events').textContent).toBe('preview:rows.csv@1');
	});

	it('costs no request for an image, because the browser fetches it', async () => {
		const { container, getByTestId } = renderPreviews();

		await fireEvent.click(previewToggle(container, 'chart.png')!);

		expect(getByTestId('panel-file-events').textContent).toBe('');
		// It still opened — the image is there, it just did not come through a
		// caller.
		expect(previewBody(container, 'chart.png')?.querySelector('img')).not.toBeNull();
	});

	it('keeps one preview open at a time, so a file list stays a list', async () => {
		const { container } = renderPreviews();

		await fireEvent.click(previewToggle(container, 'report.md')!);
		expect(openBodies(container)).toEqual(['markdown']);

		await fireEvent.click(previewToggle(container, 'run.json')!);
		// Never two: six files must not become six documents stacked in one act.
		expect(openBodies(container)).toEqual(['json']);
	});

	it('closes the open one when its own control is clicked again', async () => {
		const { container } = renderPreviews();
		const toggle = previewToggle(container, 'report.md')!;

		await fireEvent.click(toggle);
		expect(toggle.getAttribute('aria-expanded')).toBe('true');

		await fireEvent.click(toggle);
		expect(toggle.getAttribute('aria-expanded')).toBe('false');
		expect(container.querySelectorAll('.output-preview')).toHaveLength(0);
	});

	it('renders markdown as the document it is, not as its source', async () => {
		const { container } = renderPreviews({
			filePreview: ready(0, '## Revenue by region\n\n- North rose 12%\n')
		});

		await fireEvent.click(previewToggle(container, 'report.md')!);

		const body = previewBody(container, 'report.md')!;
		expect(body.querySelector('h2')?.textContent).toBe('Revenue by region');
		expect(body.querySelectorAll('li')).toHaveLength(1);
		// The raw `##` would be the whole failure: a report rendered as source is
		// the receipt again, one level in.
		expect(body.textContent).not.toContain('##');
	});

	it('indents JSON, so a one-line payload is readable', async () => {
		const { container } = renderPreviews({
			filePreview: ready(2, '{"region":"north","revenue":120}')
		});

		await fireEvent.click(previewToggle(container, 'run.json')!);

		expect(previewBody(container, 'run.json')?.querySelector('pre')?.textContent).toBe(
			'{\n  "region": "north",\n  "revenue": 120\n}'
		);
	});

	it('shows JSON that will not parse as the text it is, rather than not at all', async () => {
		const { container } = renderPreviews({ filePreview: ready(2, '{"a":1\n{"a":2') });

		await fireEvent.click(previewToggle(container, 'run.json')!);

		// Still readable, and never dressed up as formatted output.
		expect(previewBody(container, 'run.json')?.querySelector('pre')?.textContent).toBe(
			'{"a":1\n{"a":2'
		);
	});

	it('draws a CSV as a table, with the quoted delimiter inside its own cell', async () => {
		const { container } = renderPreviews({
			filePreview: ready(1, 'region,revenue\n"Acme, Inc.",120\nSouth,98\n')
		});

		await fireEvent.click(previewToggle(container, 'rows.csv')!);

		const body = previewBody(container, 'rows.csv')!;
		expect(Array.from(body.querySelectorAll('th')).map((cell) => cell.textContent)).toEqual([
			'region',
			'revenue'
		]);
		expect(
			Array.from(body.querySelectorAll('tbody tr')).map((row) =>
				Array.from(row.querySelectorAll('td')).map((cell) => cell.textContent)
			)
		).toEqual([
			['Acme, Inc.', '120'],
			['South', '98']
		]);
	});

	it('counts the rows it did not draw rather than cutting the table in silence', async () => {
		const rows = ['region,revenue', ...Array.from({ length: 205 }, (_, i) => `r${i},${i}`)];
		const { container } = renderPreviews({ filePreview: ready(1, rows.join('\n')) });

		await fireEvent.click(previewToggle(container, 'rows.csv')!);

		const body = previewBody(container, 'rows.csv')!;
		expect(body.querySelectorAll('tbody tr')).toHaveLength(199);
		expect(body.querySelector('.output-preview__note')?.textContent).toContain('6 more rows');
	});

	it('keeps a log’s whitespace, which is the one thing it cannot be read without', async () => {
		const { container } = renderPreviews({
			filePreview: ready(3, 'INFO  start\n    indented detail\nWARN  slow\n')
		});

		await fireEvent.click(previewToggle(container, 'build.log')!);

		const pre = previewBody(container, 'build.log')?.querySelector('pre');
		expect(pre?.textContent).toBe('INFO  start\n    indented detail\nWARN  slow\n');
	});

	it('says so when the read failed, and leaves the row’s other controls working', async () => {
		const { container, getByRole } = renderPreviews({
			filePreview: { index: 0, status: 'failed', text: null, detail: 'HTTP 404' }
		});

		await fireEvent.click(previewToggle(container, 'report.md')!);

		const body = previewBody(container, 'report.md')!;
		// **Never an empty block.** That is indistinguishable from a file that is
		// genuinely empty, which is the panel asserting a fact about the task out
		// of a fact about the network.
		expect(body.textContent).toContain("Couldn't read report.md");
		expect(body.textContent).toContain('HTTP 404');
		expect(body.querySelector('pre')).toBeNull();
		expect(getByRole('button', { name: 'Open report.md' })).toBeTruthy();
	});

	it('states both numbers when the file is over the ceiling, and truncates nothing', async () => {
		const { container, getByRole } = renderPreviews({
			filePreview: {
				index: 0,
				status: 'too-large',
				text: null,
				detail: '4.0 MB is over the 256 KB preview limit'
			}
		});

		await fireEvent.click(previewToggle(container, 'report.md')!);

		const body = previewBody(container, 'report.md')!;
		expect(body.textContent).toContain('4.0 MB');
		expect(body.textContent).toContain('256 KB');
		expect(body.querySelector('pre')).toBeNull();
		expect(getByRole('button', { name: 'Open report.md' })).toBeTruthy();
	});

	it('says a file is empty rather than rendering nothing at all', async () => {
		const { container } = renderPreviews({ filePreview: ready(3, '   \n') });

		await fireEvent.click(previewToggle(container, 'build.log')!);

		expect(previewBody(container, 'build.log')?.textContent).toContain('This file is empty');
	});

	it('reads as loading while the caller has not answered, not as an empty file', async () => {
		const { container } = renderPreviews({ filePreview: null });

		await fireEvent.click(previewToggle(container, 'report.md')!);

		const body = previewBody(container, 'report.md')!;
		expect(body.dataset.previewStatus).toBe('loading');
		expect(body.textContent).toContain('Reading report.md');
	});

	/**
	 * The guard that a fixture with one file cannot show. The caller answers
	 * asynchronously and the reader can have moved rows in between; every value
	 * in the stale reply is individually valid, so nothing downstream catches it.
	 */
	it('never renders one row’s contents under another', async () => {
		const { container } = renderPreviews({
			filePreview: ready(0, '## Revenue by region')
		});

		await fireEvent.click(previewToggle(container, 'build.log')!);

		const body = previewBody(container, 'build.log')!;
		expect(body.textContent).not.toContain('Revenue by region');
		expect(body.dataset.previewStatus).toBe('loading');
	});

	it('is L2 — nothing of it exists while the act is closed', async () => {
		const { container } = renderPreviews();

		await fireEvent.click(previewToggle(container, 'report.md')!);
		expect(container.querySelectorAll('.output-preview')).toHaveLength(1);

		// Closing the act must take the preview with it, or the panel is holding
		// an observable the reader was told is put away — the shape of defect an
		// act-scoped test cannot see from outside.
		const output = sections(container).find((section) => section.dataset.act === 'output')!;
		await fireEvent.click(headerOf(output)!);
		expect(container.querySelectorAll('.output-preview')).toHaveLength(0);
		expect(container.querySelectorAll('button[aria-expanded]')).toHaveLength(3);
	});

	it('closes the open row when a different task arrives', async () => {
		const { container, rerender } = renderPreviews({
			filePreview: ready(0, '## Revenue by region')
		});

		await fireEvent.click(previewToggle(container, 'report.md')!);
		expect(openBodies(container)).toEqual(['markdown']);

		await rerender({
			task: withPreviews({ id: 'task_beta' }),
			now: NOW,
			outputActions: true,
			filePreviews: true,
			filePreview: ready(0, '## Revenue by region')
		});

		// A different task is a different reader, and the open row belongs to the
		// task it was opened on exactly as the open act does.
		expect(container.querySelectorAll('.output-preview')).toHaveLength(0);
	});

	it('keeps the open row across a poll of the same task', async () => {
		const { container, rerender } = renderPreviews({ filePreview: ready(2, '{"a":1}') });

		await fireEvent.click(previewToggle(container, 'run.json')!);

		await rerender({
			// A new object for the same task, as every poll delivers.
			task: withPreviews({ elapsedMs: 400_000 }),
			now: NOW + 60_000,
			outputActions: true,
			filePreviews: true,
			filePreview: ready(2, '{"a":1}')
		});

		expect(openBodies(container)).toEqual(['json']);
	});

	it('closes a preview when a poll moves a different file into its numeric position', async () => {
		const { container, rerender } = renderPreviews({ filePreview: ready(2, '{"a":1}') });
		await fireEvent.click(previewToggle(container, 'run.json')!);
		expect(openBodies(container)).toEqual(['json']);

		await rerender({
			task: withPreviews({
				output: {
					...PREVIEWABLE,
					files: [previewFile('new.md', { mediaType: 'text/markdown' }), ...PREVIEWABLE.files]
				}
			}),
			now: NOW,
			outputActions: true,
			filePreviews: true,
			filePreview: ready(2, '{"a":1}')
		});

		expect(container.querySelectorAll('.output-preview')).toHaveLength(0);
	});

	it('names each preview control for its file, and stays a prefix of what is on screen', () => {
		const { getByRole } = renderPreviews();

		// Five identically-labelled `Preview` buttons is the card grid's defect
		// heard rather than seen; the visible word stays `Preview` in both states
		// so voice control still reaches it by what it says.
		for (const name of ['report.md', 'rows.csv', 'run.json', 'build.log', 'chart.png']) {
			expect(getByRole('button', { name: `Preview ${name}` }).textContent?.trim()).toBe('Preview');
		}
	});
});

describe('UnifiedTaskPanel — the written report', () => {
	const REPORT = [
		'## Revenue by region',
		'',
		'- North rose 12%',
		'- South fell 4%',
		'',
		'See `q3/report.md` for the workings.'
	].join('\n');

	const withReport = (summary: string | null = REPORT) =>
		makeTask({ status: 'finished', output: { ...OUTPUT, summary } });

	const outputBody = (container: HTMLElement) =>
		sections(container).find((section) => section.dataset.act === 'output')
			?.querySelector<HTMLElement>('.act__body') ?? null;

	it('renders the summary as markdown rather than as a paragraph of source', () => {
		const { container } = renderPanel({ task: withReport() });

		const body = outputBody(container);
		// The three shapes that separate rendered markdown from printed text: a
		// heading element, list items, and no leftover syntax. A component
		// interpolating the string would show `## Revenue by region` verbatim and
		// produce none of these.
		expect(body?.querySelector('.output-summary h2')?.textContent).toBe('Revenue by region');
		expect(
			Array.from(body?.querySelectorAll('.output-summary li') ?? []).map((li) => li.textContent)
		).toEqual(['North rose 12%', 'South fell 4%']);
		expect(body?.querySelector('.output-summary code')?.textContent).toBe('q3/report.md');
		expect(body?.textContent).not.toContain('## Revenue');
	});

	it('puts the report above the files, because for those tasks the files are the appendix', () => {
		const { container } = renderPanel({ task: withReport() });

		const body = outputBody(container);
		const summary = body?.querySelector('.output-summary');
		const files = body?.querySelector('.output-files');
		expect(summary).toBeTruthy();
		expect(files).toBeTruthy();
		// `DOCUMENT_POSITION_FOLLOWING` — the file list comes after the report.
		// Both are asserted truthy above, so narrow rather than assert — a `!`
		// here would survive the day one of those expectations is loosened.
		if (!summary || !files) throw new Error('report and file list must both render');
		expect(summary.compareDocumentPosition(files) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
	});

	it('renders nothing where there is no report, rather than an empty block', () => {
		const { container } = renderPanel({ task: withReport(null) });

		expect(outputBody(container)?.querySelector('.output-summary')).toBeNull();
	});

	it('keeps the report at L2, so it is absent while the act is closed', () => {
		// The act the panel opens for a *running* task is Run, so this one is shut.
		const { container } = renderPanel({ task: makeTask({ output: { ...OUTPUT, summary: REPORT } }) });

		expect(openActs(container)).toEqual(['run']);
		expect(container.querySelector('.output-summary')).toBeNull();
		expect(container.textContent).not.toContain('Revenue by region');
	});

	/**
	 * The interaction the restoration had to survive. A report and a file list are
	 * different observations, and the L1 line summarises **files** — so a task
	 * that wrote prose and no artifacts still reads `no output` above and
	 * `Produced no output` in the verdict, both of which stay true. Letting the
	 * report change either would make two tasks with identical file lists read
	 * differently at L1, off a field the header never claimed to be about.
	 */
	it('never lets the report change what the summary line or the verdict claims', () => {
		const withFiles = renderPanel({ task: withReport() });
		expect(summaryOf(withFiles.container, 'output')).toBe('report.md and 2 images');
		expect(verdictDetail(withFiles.container)).toBe('Wrote report.md and 2 images');
		withFiles.unmount();

		const proseOnly = renderPanel({
			task: makeTask({ status: 'finished', output: { files: [], summary: REPORT, provenance: [] } })
		});
		expect(summaryOf(proseOnly.container, 'output')).toBe('no output');
		expect(verdictDetail(proseOnly.container)).toBe('Produced no output');
		// …and the report is still readable, which is the whole point: the claim
		// about files is unchanged and the prose is on the page.
		expect(proseOnly.container.querySelector('.output-summary h2')?.textContent).toBe(
			'Revenue by region'
		);
	});
});

describe('UnifiedTaskPanel — output ownership', () => {
	it('visually separates task deliverables from the selected run, delegation, and persisted evidence', () => {
		const grouped: TaskPanelOutput = {
			...OUTPUT,
			taskFilesKnown: true,
			selectedExecutionId: 'exec_1234567890abcdef',
			runArtifactsKnown: true,
			files: [
				{ ...OUTPUT.files[0], scope: 'task' },
				{
					...OUTPUT.files[0],
					name: 'attempt.md',
					path: 'executions/exec_1234567890abcdef/outputs/attempt.md',
					scope: 'execution'
				},
				{
					...OUTPUT.files[0],
					name: 'evidence.md',
					path: 'executions/exec_child/outputs/evidence.md',
					scope: 'delegated'
				},
				{
					...OUTPUT.files[1],
					name: 'capture.png',
					path: 'executions/exec_1234567890abcdef/outputs/capture.png',
					scope: 'artifact',
					artifactId: 'artifact-file',
					artifactType: 'screen_capture'
				}
			],
			artifacts: [
				{
					id: 'artifact-structured',
					name: 'Browser observation',
					artifactType: 'browser_observation',
					contentType: 'application/json',
					producedAt: '2026-07-13T00:02:00Z',
					sourceExecutionId: 'exec_1234567890abcdef'
				}
			]
		};
		const { container } = renderPanel({
			task: makeTask({ status: 'finished', output: grouped })
		});

		const outputAct = sections(container).find((section) => section.dataset.act === 'output');
		expect(outputAct?.querySelector('.output-scope-heading--task')?.textContent).toContain(
			'Task deliverables'
		);
		expect(outputAct?.querySelector('.output-scope-heading--run')?.textContent).toContain(
			'Selected run'
		);
		expect(
			Array.from(outputAct?.querySelectorAll('.output-group-heading strong') ?? []).map(
				(node) => node.textContent
			)
		).toEqual(['Direct outputs', 'Delegated outputs', 'Persisted artifacts']);
		expect(
			Array.from(outputAct?.querySelectorAll('[data-output-scope]') ?? []).map((node) =>
				node.getAttribute('data-output-scope')
			)
		).toEqual(['task', 'execution', 'delegated', 'artifact', 'artifact']);
		expect(outputAct?.textContent).toContain('artifact-structured');
		const intermediates = outputAct?.querySelector('details.output-intermediates') as HTMLDetailsElement | null;
		expect(intermediates).not.toBeNull();
		expect(intermediates?.open).toBe(false);
		expect(intermediates?.querySelector('.output-intermediates__title')?.textContent).toContain(
			'Intermediate artifacts & evidence'
		);
		expect(intermediates?.querySelector('.output-intermediates__count')?.textContent).toContain('4 items');
		// L1 and the finished verdict summarize stable task deliverables only; a
		// historical run's evidence must not inflate the task's claimed result.
		expect(summaryOf(container, 'output')).toBe('report.md');
		expect(verdictDetail(container)).toBe('Wrote report.md');
	});
});

describe('UnifiedTaskPanel — the finished verdict detail', () => {
	it('composes the sentence design §3 asks for from the output summary', () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });

		expect(verdictRoot(container)).toHaveAttribute('data-verdict-state', 'finished');
		expect(verdictDetail(container)).toBe('Wrote report.md and 2 images');
	});

	it('never writes `Wrote no output`, which is not a sentence', () => {
		const { container } = renderPanel({
			task: makeTask({ status: 'finished', output: emptyOutput() })
		});

		const detail = verdictDetail(container);
		expect(detail).toBe('Produced no output');
		expect(detail).not.toContain('Wrote');
	});

	it('keeps a terminal verdict silent while task outputs are still synthesizing, even when run evidence exists', () => {
		const runFile = {
			...OUTPUT.files[0],
			name: 'evidence.md',
			path: 'executions/exec-1/outputs/evidence.md',
			scope: 'execution' as const
		};
		const { container } = renderPanel({
			task: makeTask({
				status: 'finished',
				output: {
					files: [runFile],
					summary: null,
					provenance: [],
					taskFilesKnown: true,
					taskFilesPending: true,
					selectedExecutionId: 'exec-1',
					runArtifactsKnown: true,
					artifacts: []
				}
			})
		});

		expect(summaryOf(container, 'output')).toBe('—');
		expect(verdictDetail(container)).toBe('');
		expect(container.textContent).toContain('Task-level deliverables are still being synthesized.');
		expect(container.textContent).toContain('evidence.md');
	});

	it('says nothing about output it never saw', () => {
		// The Output act failed to load, so the panel knows nothing about what the
		// task produced. Reporting an absence it did not observe is design §6's
		// fabricated verdict in miniature.
		const { container } = renderPanel({ task: makeTask({ status: 'finished', output: null }) });

		expect(actOrder(container)).toEqual(['plan', 'run']);
		expect(verdictDetail(container)).toBe('');
	});

	it('leaves every other verdict its own second line', () => {
		const { container: running, unmount } = renderPanel({ task: PLANNED });
		expect(verdictDetail(running)).toBe('Searching memory for "quarterly plan"');
		unmount();

		// A finished task with an unanswered question reads `waiting`, and the
		// output summary must not overwrite what the reader has to do.
		const { container: waiting } = renderPanel({
			task: makeTask({
				status: 'finished',
				attention: { source: 'plan_approval', summary: null, raisedAt: NOW - 4 * 60_000 }
			})
		});
		expect(verdictDetail(waiting)).toBe('Approve the plan before it can run');
	});

	it('keeps a failed task’s error, the one sentence composition could eat', () => {
		// Design §6, case 1: a failed task's verdict carries its error text.
		// `deriveVerdict` pins that at its level and `TaskVerdictLine` pins the
		// rendering at its own, but composition sits between them and is the only
		// place in this feature that overwrites a detail — so it is the only place
		// the error can be lost, and it was unpinned.
		//
		// The fixture has files, so a composition that reached this state would
		// replace "what went wrong" with "what it wrote".
		const { container } = renderPanel({
			task: makeTask({ status: 'failed', error: "Couldn't read revenue.csv — file not found" })
		});

		expect(verdictRoot(container)).toHaveAttribute('data-verdict-state', 'failed');
		expect(verdictDetail(container)).toBe("Couldn't read revenue.csv — file not found");
		expect(verdictDetail(container)).not.toContain('Wrote');
	});

	it('composes the finished row and forwards every other state verbatim', () => {
		// The same seam pinned across the whole union rather than at the one state
		// a reader happens to try. `deriveVerdict` owns six of the seven sentences;
		// the panel's job for those is to hand them through untouched, and only the
		// `finished` row — the one left empty on purpose — is its to write.
		for (const { label, task } of STATE_CASES) {
			const expected = deriveVerdict({ ...task, now: NOW });
			if (expected.state === 'finished') continue;

			const { container, unmount } = renderPanel({ task });
			expect(verdictDetail(container), label).toBe(expected.detail);
			unmount();
		}
	});
});

describe('UnifiedTaskPanel — the Run act', () => {
	it('counts the header from the very list the body shows', async () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });

		// 4 steps and 5 retries, neither of which is the live step (3) or the
		// planned total (7).
		expect(summaryOf(container, 'run')).toBe('4 steps · 5 retries · 3m 12s');

		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

		const steps = Array.from(container.querySelectorAll('.run-step'));
		expect(steps).toHaveLength(4);
		// Retries nest under their step rather than becoming rows of their own, so
		// "5 retries" is marks on two steps and not five extra lines to read.
		expect(steps.map((step) => step.querySelector('.run-step__retries')?.textContent ?? null)).toEqual(
			[null, '3 retries', null, '2 retries']
		);
	});

	it('swaps the step count for the position while the run can still move', () => {
		const { container } = renderPanel({ task: PLANNED });

		// Not the whole line: retries and elapsed still apply, and hiding a
		// struggling run's retries while it is live is when they matter most.
		expect(summaryOf(container, 'run')).toBe('step 3 of 7 · 5 retries · 3m 12s');
	});

	it('omits a step duration it has no timing for', async () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });
		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

		const steps = Array.from(container.querySelectorAll('.run-step'));
		expect(
			steps.map((step) => step.querySelector('.run-step__duration')?.textContent ?? null)
		).toEqual(['3s', '12s', '45s', null]);
	});

	it('marks the live step and only the live step', () => {
		// No click: a running task opens Run already, and clicking the open act
		// closes it.
		const { container } = renderPanel({ task: PLANNED });

		const current = Array.from(container.querySelectorAll('.run-step')).map((step) =>
			step.classList.contains('run-step--current')
		);
		expect(current).toEqual([false, false, true, false]);
	});

	/**
	 * Who ran it. The capability the plan named and the agent it routed the step
	 * to, on the row itself — "Searched memory" is less useful than knowing which
	 * agent searched.
	 *
	 * L2 and not L3: an agent name and a capability are the row's own words about
	 * work the reader is already looking at, exactly as a timeline row's model and
	 * token bill are. The identifiers behind Details are untouched.
	 */
	it('says what each step used and which agent ran it, and nothing where it does not know', async () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });
		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

		const steps = Array.from(container.querySelectorAll('.run-step'));
		expect(steps.map((step) => step.querySelector('.run-step__who')?.textContent ?? null)).toEqual([
			// Capability alone, agent alone, and both — three shapes, so a renderer
			// that joined the wrong pair or dropped one cannot pass.
			'read_file',
			'memory_search · via research-agent',
			'via writer-agent',
			// Neither recorded: no element at all, rather than an empty one or a
			// stray separator.
			null
		]);
	});

	it('prefixes the agent so half an answer is still unambiguous', async () => {
		// `research-agent` and `memory_search` are the same shape of word. A reader
		// shown one bare token has no way to tell which fact they are being told,
		// and most rows carry only one of the two.
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });
		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

		const who = Array.from(container.querySelectorAll('.run-step__who')).map(
			(node) => node.textContent ?? ''
		);
		expect(who.filter((line) => line.includes('research-agent'))).toEqual([
			'memory_search · via research-agent'
		]);
		expect(who.some((line) => line === 'research-agent')).toBe(false);
	});

	it('marks no step once the task has stopped', async () => {
		const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });
		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

		expect(container.querySelector('.run-step--current')).toBeNull();
	});

	/**
	 * The Run act was a list of labels and a duration: every row read the same
	 * whether it had succeeded, failed or never started, and the one fact a
	 * reader scanning a run wants — *which* step went wrong — was the one the row
	 * did not carry.
	 */
	describe('per-step status', () => {
		const ALL_STATUSES = [
			'pending',
			'in_progress',
			'waiting',
			'completed',
			'failed',
			'skipped',
			'cancelled'
		] as const;

		const everyStatus = () =>
			makeTask({
				status: 'finished',
				run: {
					steps: ALL_STATUSES.map((status) => ({
						label: `Step ${status}`,
						durationMs: null,
						retries: 0,
						status,
						origin: 'step' as const,
						capability: null,
						delegate: null,
						blocking: false
					})),
					timeline: null,
					responsibility: null,
					provenance: []
				}
			});

		const markers = (container: HTMLElement) =>
			Array.from(container.querySelectorAll('.run-step__marker')).map(
				(marker) => marker.textContent ?? ''
			);

		it('gives every status a mark of its own', async () => {
			const { container } = renderPanel({ task: everyStatus() });
			await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

			const shown = markers(container);
			expect(shown).toHaveLength(ALL_STATUSES.length);
			// As a **set**: six equalities would still pass with two statuses
			// sharing a glyph, which is the whole property being asserted here.
			expect(new Set(shown).size).toBe(ALL_STATUSES.length);
			expect(shown.every((mark) => mark.length > 0)).toBe(true);
		});

		it('reuses the verdict line\'s own marks for the states both vocabularies have', async () => {
			const { container } = renderPanel({ task: everyStatus() });
			await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

			const shown = markers(container);
			const byStatus = Object.fromEntries(ALL_STATUSES.map((status, i) => [status, shown[i]]));
			// One vocabulary of glyphs down the whole panel: a completed step and a
			// finished task are the same claim about different scopes. Asserted
			// against the verdict line's own map rather than against a literal, so
			// this fails if either level's mark moves without the other — which is
			// the property, and a literal on both sides would not have it.
			expect(byStatus.completed).toBe(VERDICT_MARKER.finished);
			expect(byStatus.failed).toBe(VERDICT_MARKER.failed);
			expect(byStatus.in_progress).toBe(VERDICT_MARKER.running);
			expect(byStatus.cancelled).toBe(VERDICT_MARKER.cancelled);
			expect(byStatus.pending).toBe(VERDICT_MARKER.queued);
			// The seventh, added for work that has stopped until something acts on
			// it — a delegated child blocked on the reader, a step paused mid-flight.
			// It borrows too, so `!` means the same thing at every level of the
			// panel.
			expect(byStatus.waiting).toBe(VERDICT_MARKER.waiting);
			// The one with nothing to borrow: no verdict state means "deliberately
			// not run", so it must not be any of them.
			expect(Object.values(VERDICT_MARKER)).not.toContain(byStatus.skipped);
		});

		it('bands the failed, the live and the waiting step, so a long list keeps a signal', async () => {
			const { container } = renderPanel({ task: everyStatus() });
			await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

			// Three tones out of seven statuses. `pending`, `completed`, `skipped`
			// and `cancelled` are history and take the body colour: a list in which
			// every row is coloured has no signal left in it.
			expect(
				Array.from(container.querySelectorAll('.run-step')).map(
					(step) => (step as HTMLElement).dataset.stepTone
				)
			).toEqual(['idle', 'running', 'waiting', 'idle', 'failed', 'idle', 'idle']);
		});

		it('renders no mark at all for a step whose status was never recorded', async () => {
			// The fixture's fourth step. An empty gutter rather than a neutral
			// glyph: `⋯` would claim the step has not started, which is a fact
			// nothing recorded.
			const { container } = renderPanel({ task: makeTask({ status: 'finished' }) });
			await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

			expect(markers(container)).toEqual(['✓', '✕', '⟳', '']);
			expect(
				Array.from(container.querySelectorAll('.run-step')).map(
					(step) => (step as HTMLElement).dataset.stepStatus
				)
			).toEqual(['completed', 'failed', 'in_progress', 'unknown']);
		});
	});
});

/**
 * Delegated work, read as delegated work.
 *
 * **The trap this whole block exists to hold shut is positional.** Every
 * assertion above indexes `.run-step` and every one of them was written when
 * that selector meant "a plan step". Delegated rows are `.run-step` too — they
 * are one list, and splitting them into two would give the act a third list to
 * scroll — so `origin` is what carries the distinction, and the counts that used
 * to be `steps.length` have to filter on it. A test that counted rows as a proxy
 * for counting steps stops coinciding with the property the moment a run
 * delegates anything.
 */
describe('UnifiedTaskPanel — a delegated subtask reads as a subtask', () => {
	const delegated = (): TaskPanelModel =>
		makeTask({
			status: 'running',
			run: {
				...RUN,
				steps: [
					...RUN.steps,
					{
						label: 'Pull the regional exports',
						durationMs: null,
						retries: 0,
						status: 'in_progress',
						origin: 'delegated',
						capability: null,
						delegate: 'research-agent',
						// **Running and still holding the parent.** The row below is the
						// pair that makes the two facts separable: same fixture, one
						// blocking and one not, so a renderer keying the flag off the
						// status cannot pass.
						blocking: true
					},
					{
						label: 'Draft the regional summary',
						durationMs: null,
						retries: 0,
						status: 'waiting',
						origin: 'delegated',
						capability: null,
						delegate: 'writer-agent',
						blocking: false
					}
				],
				responsibility: {
					owner: 'research-agent',
					ownerChain: ['personal-assistant', 'research-agent'],
					state: 'Waiting on the work it delegated',
					blocking: 1,
					total: 2
				}
			}
		});

	const stepRows = (container: HTMLElement) =>
		Array.from(container.querySelectorAll<HTMLElement>('.run-step'));

	it('counts only the plan in the header, never the work the run delegated', () => {
		const { container } = renderPanel({ task: makeTask({ ...delegated(), status: 'finished' }) });

		// Six rows in the body, four steps in the header. The forbidden line is a
		// header that read `6 steps` beside a row reading `step 3 of 7` — two
		// numbers about one run, both plausible, neither checkable.
		expect(summaryOf(container, 'run')).toBe('4 steps · 5 retries · 3m 12s');
	});

	it('marks them as delegated and indents them under the run, not under a step', () => {
		const { container } = renderPanel({ task: delegated() });

		expect(stepRows(container).map((row) => row.dataset.stepOrigin)).toEqual([
			'step',
			'step',
			'step',
			'step',
			'delegated',
			'delegated'
		]);
	});

	it('never puts the live-step mark on a delegated row', () => {
		// The live position is an index into the plan, and the delegated rows sit
		// past its end — so a renderer that matched on position alone would need a
		// plan longer than this one to go wrong. The `origin` check is what makes
		// it wrong for *any* plan, and this pins it with the live step deliberately
		// set past every plan row.
		const { container } = renderPanel({
			task: makeTask({ ...delegated(), currentStep: 5, totalSteps: 7 })
		});

		expect(container.querySelectorAll('.run-step--current')).toHaveLength(0);
	});

	it('names the agent that owns each delegated run', () => {
		const { container } = renderPanel({ task: delegated() });

		expect(
			stepRows(container)
				.slice(4)
				.map((row) => row.querySelector('.run-step__who')?.textContent ?? null)
		).toEqual(['via research-agent', 'via writer-agent']);
	});

	it('gives a delegated run blocked on the reader a mark that says so', () => {
		// `waiting` is the status `RunStepStatus` gained for this: a child run
		// suspended until someone acts. `pending` would claim it never started.
		const { container } = renderPanel({ task: delegated() });

		const rows = stepRows(container);
		expect(rows[5].dataset.stepStatus).toBe('waiting');
		expect(rows[5].dataset.stepTone).toBe('waiting');
		expect(rows[5].querySelector('.run-step__marker')?.textContent).toBe(VERDICT_MARKER.waiting);
	});

	/**
	 * **Which child the run is still held by**, on the child's own row.
	 *
	 * The fixture's two delegated rows have different statuses *and* different
	 * blocking flags, arranged so neither can stand in for the other: the blocking
	 * one is `in_progress` and the non-blocking one is `waiting`, so a renderer
	 * keying the flag off "is this row still going" marks the wrong row and one
	 * keying it off "is this row waiting" marks the wrong row the other way.
	 */
	it('says which delegated child is still holding the run, and only that one', () => {
		const { container } = renderPanel({ task: delegated() });

		const flagged = stepRows(container).map(
			(row) => row.querySelector('.run-step__blocking') !== null
		);
		expect(flagged).toEqual([false, false, false, false, true, false]);
	});

	it('flags no row on a run whose children have all been let go', () => {
		// Absent rather than a `0`-shaped placeholder, on the act rule: an element
		// that renders on every row and says nothing on most of them is the column
		// this panel refuses to grow.
		const base = delegated();
		const { container } = renderPanel({
			task: makeTask({
				...base,
				run: {
					...base.run!,
					steps: base.run!.steps.map((step) => ({ ...step, blocking: false }))
				}
			})
		});

		expect(container.querySelectorAll('.run-step__blocking')).toHaveLength(0);
	});
});

/**
 * **Who holds a delegated multi-agent run.**
 *
 * Outside `/debug` this block is the only account of it. The Run act's rows say
 * what was handed out and to whom; nothing said who owns the run now, how
 * ownership reached them, or how much of it is still in other agents' hands —
 * and those are properties of the run rather than of any row, which is why they
 * are one block and not a sixth column.
 */
describe('UnifiedTaskPanel — the responsibility block', () => {
	/**
	 * A run that handed two sub-goals out and is still held by one of them. Its
	 * own fixture rather than the delegated-rows one above, because that block's
	 * `delegated` is scoped to it — and because the values here have to be
	 * distinct from each other in a way that block does not need: the owner is not
	 * the first link of the chain, and the blocking count is not the total.
	 */
	const delegated = (): TaskPanelModel =>
		makeTask({
			status: 'running',
			run: {
				...RUN,
				steps: [
					...RUN.steps,
					{
						label: 'Pull the regional exports',
						durationMs: null,
						retries: 0,
						status: 'in_progress',
						origin: 'delegated',
						capability: null,
						delegate: 'research-agent',
						blocking: true
					},
					{
						label: 'Draft the regional summary',
						durationMs: null,
						retries: 0,
						status: 'waiting',
						origin: 'delegated',
						capability: null,
						delegate: 'writer-agent',
						blocking: false
					}
				],
				responsibility: {
					owner: 'research-agent',
					ownerChain: ['personal-assistant', 'research-agent'],
					state: 'Waiting on the work it delegated',
					blocking: 1,
					total: 2
				}
			}
		});

	const rows = (container: HTMLElement) =>
		Array.from(container.querySelectorAll<HTMLElement>('.run-responsibility__row')).map((row) => [
			row.querySelector('dt')?.textContent?.trim() ?? '',
			row.querySelector('dd')?.textContent?.trim() ?? ''
		]);

	it('names the owner, the chain that reached them, and how much is still out', () => {
		const { container } = renderPanel({ task: delegated() });

		expect(rows(container)).toEqual([
			['Owner', 'personal-assistant → research-agent'],
			['Waiting on', 'Waiting on the work it delegated'],
			['Delegated', '2 delegated runs · 1 still running']
		]);
	});

	it('is absent entirely for a run that delegated nothing', () => {
		// Not an empty block, not a heading with nothing under it. The base fixture
		// is such a run, which is every run on most tasks.
		const { container } = renderPanel({ task: makeTask({ run: RUN }) });

		expect(container.querySelector('.run-responsibility')).toBeNull();
	});

	it('renders the owner alone when nothing handed the run over', () => {
		const base = delegated();
		const { container } = renderPanel({
			task: makeTask({
				...base,
				run: {
					...base.run!,
					responsibility: {
						owner: 'research-agent',
						ownerChain: ['research-agent'],
						state: 'Running',
						blocking: 0,
						total: 1
					}
				}
			})
		});

		// No arrow: a one-link chain drawn as `a → a`, or as `a →`, would claim a
		// handover that did not happen.
		expect(rows(container)[0]).toEqual(['Owner', 'research-agent']);
		expect(container.querySelector('.run-responsibility')?.textContent).not.toContain('→');
	});

	it('drops the waiting line rather than inventing a state for one it cannot name', () => {
		const base = delegated();
		const { container } = renderPanel({
			task: makeTask({
				...base,
				run: {
					...base.run!,
					responsibility: { ...base.run!.responsibility!, state: null }
				}
			})
		});

		expect(rows(container).map(([label]) => label)).toEqual(['Owner', 'Delegated']);
	});

	it('counts a single delegated run in the singular, and says when none is still going', () => {
		// `1 delegated runs` reads as a bug in the panel, which costs the reader
		// trust in the number beside it; `0 still running` and `none still running`
		// are the same fact and only one of them is read at a glance.
		const base = delegated();
		const { container } = renderPanel({
			task: makeTask({
				...base,
				run: {
					...base.run!,
					responsibility: { ...base.run!.responsibility!, blocking: 0, total: 1 }
				}
			})
		});

		expect(rows(container)[2]).toEqual(['Delegated', '1 delegated run · none still running']);
	});

	it('sits below the steps and above the timeline, inside the one Run act', () => {
		// The order is the argument for the placement: it is the account of the
		// rows the reader has just read, not a preamble to the ones they have not.
		// A single event is enough — this asserts where the block sits between the
		// act's two lists, not what either list contains.
		const base = delegated();
		const { container } = renderPanel({
			task: makeTask({
				...base,
				run: {
					...base.run!,
					timeline: [
						{
							id: 'ev_1',
							at: NOW - 60_000,
							kind: 'lifecycle',
							status: 'done',
							title: 'Run started',
							body: null,
							detail: null,
							latencyMs: null,
							model: null,
							costUsd: null,
							tokens: null,
							screenshot: false,
							executionId: null,
							agentId: null,
						}
					]
				}
			})
		});

		const body =
			container.querySelector<HTMLElement>('section[data-act="run"] .act__body') ?? container;
		const marks = Array.from(
			body.querySelectorAll('.run-steps, .run-responsibility, .run-timeline')
		).map((node) => node.className.split(' ')[0]);
		expect(marks).toEqual(['run-steps', 'run-responsibility', 'run-timeline']);
	});
});

/**
 * The timeline: the Run act's second list, and the capability the migration off
 * `DeepWorkPanel` cost.
 *
 * **Every value in `TIMELINE` differs from every other and from every number in
 * `RUN` above it** — six events against four steps, one latency of 4.2s against
 * step durations of 3s/12s/45s, and a token bill whose four counts are all
 * different. A fixture where any two agreed could not tell a correct row from
 * one rendering a neighbouring field.
 */
describe('UnifiedTaskPanel — the Run act timeline', () => {
	const TIMELINE: TimelineEntry[] = [
		{
			id: 'tl-run-start',
			at: NOW - 300_000,
			kind: 'lifecycle',
			status: 'info',
			title: 'Run started',
			body: null,
			detail: null,
				latencyMs: null,
				model: null,
				costUsd: null,
				tokens: null,
			screenshot: false,
			executionId: null,
			agentId: null,
		},
		{
			id: 'tl-llm',
			at: NOW - 240_000,
			kind: 'llm',
			status: 'done',
			title: 'Thinking with research',
			body: null,
			detail: null,
				latencyMs: 4_200,
				model: 'claude-opus-4',
				costUsd: 0.00610875,
				tokens: { input: 12_000, output: 384, cacheRead: 9_600, cacheCreation: 1_150 },
			screenshot: false,
			executionId: null,
			agentId: null,
		},
		{
			id: 'tl-tool',
			at: NOW - 180_000,
			kind: 'tool',
			status: 'failed',
			title: 'memory_search failed',
			body: 'No index for the requested quarter.',
			detail: null,
				latencyMs: null,
				model: null,
				costUsd: null,
				tokens: null,
			screenshot: false,
			executionId: null,
			agentId: null,
		},
		{
			id: 'tl-shell',
			at: NOW - 120_000,
			kind: 'shell',
			status: 'running',
			title: 'cargo build --release',
			body: null,
			detail: 'Compiling magician\nFinished in 42s',
				latencyMs: null,
				model: null,
				costUsd: null,
				tokens: null,
			screenshot: false,
			executionId: null,
			agentId: null,
		},
		{
			id: 'tl-observation',
			at: NOW - 60_000,
			kind: 'observation',
			status: 'info',
			title: 'https://example.test/reports',
			body: null,
			detail: null,
				latencyMs: null,
				model: null,
				costUsd: null,
				tokens: null,
			screenshot: true,
			executionId: null,
			agentId: null,
		},
		{
			// The row whose status was never recorded, which has to render no
			// marker rather than a plausible one — the same rule the steps follow.
			id: 'tl-unknown',
			at: NOW - 30_000,
			kind: 'event',
			status: null,
			title: 'Something the client does not model',
			body: null,
			detail: null,
				latencyMs: null,
				model: null,
				costUsd: null,
				tokens: null,
			screenshot: false,
			executionId: null,
			agentId: null,
		}
	];

	const withTimeline = (
		timeline: readonly TimelineEntry[] | null,
		overrides: Partial<TaskPanelModel> = {}
	) => makeTask({ run: { ...RUN, timeline }, ...overrides });

	const rows = (container: HTMLElement) =>
		Array.from(container.querySelectorAll<HTMLElement>('.timeline-row'));

	const openRun = async () => {
		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));
	};

	it('renders no timeline at all when nothing observed the run\'s events', async () => {
		const { container } = renderPanel({ task: withTimeline(null, { status: 'finished' }) });
		await openRun();

		// Absent, not empty: `null` is "nobody looked", and a line saying the run
		// recorded nothing would be a claim about the run made out of a fact about
		// the payload.
		expect(container.querySelector('.run-timeline')).toBeNull();
		expect(container.querySelector('.run-timeline__empty')).toBeNull();
		// And the header says nothing about events either.
		expect(summaryOf(container, 'run')).toBe('4 steps · 5 retries · 3m 12s');
	});

	it('bounds a long feed to its latest 200 rows and discloses the exact omitted count', async () => {
		const longTimeline = Array.from({ length: 205 }, (_, index): TimelineEntry => ({
			...TIMELINE[0],
			id: `long-${index}`,
			at: NOW - 205_000 + index * 1_000,
			title: `Event ${index}`
		}));
		const { container } = renderPanel({
			task: withTimeline(longTimeline, { status: 'finished' })
		});
		await openRun();

		expect(container.querySelector('.run-timeline__window')).toHaveTextContent(
			'Showing the latest 200 of 205 events'
		);
		expect(rows(container)).toHaveLength(200);
		expect(rows(container)[0]).toHaveTextContent('Event 5');
		expect(rows(container).at(-1)).toHaveTextContent('Event 204');
		expect(
			rows(container).some(
				(row) => row.querySelector('.timeline-row__title')?.textContent === 'Event 4'
			)
		).toBe(false);
	});

	it('says a stopped run recorded nothing, and a live one has recorded nothing yet', async () => {
		const stopped = renderPanel({ task: withTimeline([], { status: 'finished' }) });
		await openRun();
		expect(stopped.container.querySelector('.run-timeline__empty')?.textContent).toBe(
			'No activity was recorded for this run'
		);
		// A count of zero drops out of the header like every other empty segment,
		// so the observed-empty case is not `0 events`.
		expect(summaryOf(stopped.container, 'run')).not.toContain('event');
		stopped.unmount();

		// The same list on a run that can still move. `No activity was recorded`
		// there would be the premature finality claim `no output` is on the Output
		// act, one act to the left.
		const live = renderPanel({ task: withTimeline([]) });
		expect(live.container.querySelector('.run-timeline__empty')?.textContent).toBe(
			'No activity recorded yet'
		);
	});

	it('counts events under their own noun beside the steps, never as steps', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });

		// Six events, four steps, five retries — three different numbers in one
		// line, so a header that counted the wrong list cannot pass. The forbidden
		// line is `step 143 of 217`: events must never reach the step segment.
		expect(summaryOf(container, 'run')).toBe('4 steps · 5 retries · 6 events · 3m 12s');
		await openRun();
		expect(rows(container)).toHaveLength(TIMELINE.length);
		expect(container.querySelectorAll('.run-step')).toHaveLength(RUN.steps.length);
	});

	it('is L2 — nothing of it exists while the act is closed', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });

		// A finished task opens Output, so Run starts closed. Absent from the DOM
		// rather than hidden with CSS, which is what the ladder means by a level.
		expect(openActs(container)).toEqual(['output']);
		expect(container.querySelector('.run-timeline')).toBeNull();
		expect(rows(container)).toHaveLength(0);

		await openRun();
		expect(rows(container)).toHaveLength(TIMELINE.length);
	});

	it('reads each row as what happened, in order', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
		await openRun();

		expect(
			rows(container).map((row) => row.querySelector('.timeline-row__title')?.textContent)
		).toEqual(TIMELINE.map((entry) => entry.title));
		expect(rows(container).map((row) => row.dataset.timelineKind)).toEqual([
			'lifecycle',
			'llm',
			'tool',
			'shell',
			'observation',
			'event'
		]);
		// The eyebrow is the operator's word, not the wire's namespace — and none
		// of them is `step`, which is the word the list above spends on plan
		// structure.
		const kinds = rows(container).map((row) => row.querySelector('.timeline-row__kind')?.textContent);
		expect(kinds).toEqual(['run', 'thinking', 'tool', 'shell', 'observation', 'event']);
		expect(kinds).not.toContain('step');
	});

	it('gives every recorded status a mark, and an unrecorded one none', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
		await openRun();

		const marks = rows(container).map(
			(row) => row.querySelector('.timeline-row__marker')?.textContent ?? ''
		);
		// The four that map to a verdict state borrow its mark by reference, so one
		// vocabulary of glyphs reads down the whole panel.
		expect(marks).toEqual(['·', VERDICT_MARKER.finished, VERDICT_MARKER.failed, VERDICT_MARKER.running, '·', '']);
		// Empty for the row whose status was never recorded — the gutter is still
		// there, so the titles keep one left edge.
		expect(rows(container).map((row) => row.dataset.timelineStatus)).toEqual([
			'info',
			'done',
			'failed',
			'running',
			'info',
			'unknown'
		]);
		// Two tones only: a failed row and the live one. A feed in which every row
		// is coloured has no signal left in it.
		expect(rows(container).map((row) => row.dataset.timelineTone)).toEqual([
			'idle',
			'idle',
			'failed',
			'running',
			'idle',
			'idle'
		]);
	});

	it('carries the model, the bill and the cache rate on the row that spent them', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
		await openRun();

		const metas = rows(container).map((row) => {
			const meta = row.querySelector('.timeline-row__meta');
			// The line is two elements now — an identifier and a run of figures — with
			// the layout's gap standing in for the `·` that used to join them, so the
			// markup's own indentation lands in `textContent`.
			return meta === null ? null : (meta.textContent ?? '').replace(/\s+/g, ' ').trim();
		});
		// Exactly one row has a bill, and the rows around it render no element at
		// all rather than an empty one.
		expect(metas).toEqual([null, 'claude-opus-4 12k → 384 tok · 80% cached', null, null, null, null]);

		/**
		 * **The model is an identifier and is set as one; the figures are not.** A
		 * provider's model id is a token to be matched against a config file, so it
		 * takes the mono face and a hairline container — the same treatment a
		 * provenance value gets. The token counts are numbers a reader compares down a
		 * column and stay tabular proportional text. Joined into one string they had to
		 * share one treatment, which made `claude-opus-4-20250514` read as prose.
		 */
		const billed = rows(container)[1];
		const identifier = billed.querySelector('.timeline-row__id');
		expect(identifier?.tagName).toBe('CODE');
		expect(identifier?.textContent).toBe('claude-opus-4');
		expect(billed.querySelector('.timeline-row__figures')?.textContent).toBe(
			'12k → 384 tok · 80% cached'
		);
		// And the figures are not inside the identifier's container.
		expect(identifier?.querySelector('.timeline-row__figures')).toBeNull();

		// Latency sits at the end of the title line, where a step's duration
		// already does: the same fact about a smaller thing.
		//
		// Read off the track element rather than the value inside it, so removing
		// the whole affordance still fails here; trimmed because the track also
		// holds the proportional bar, which contributes markup and no text.
		expect(
			rows(container).map(
				(row) => row.querySelector('.timeline-row__latency')?.textContent?.trim() ?? null
			)
		).toEqual([null, '4s', null, null, null, null]);
	});

	it('shows shell output as output, a body as prose, and a capture as a capture', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
		await openRun();

		/**
		 * **A code container, not a monospace paragraph.** `<figure>` wrapping
		 * `<pre><code>`: `<pre>` keeps the whitespace a log cannot be read without,
		 * `<code>` says the content is code rather than merely fixed-width, and the
		 * figure is what lets the block carry a control — a `<button>` inside the
		 * `<pre>` would sit in significant whitespace and land in the copied text.
		 */
		const shell = container.querySelector('.timeline-row__detail');
		expect(shell?.tagName).toBe('PRE');
		expect(shell?.firstElementChild?.tagName).toBe('CODE');
		expect(shell?.closest('figure')).toHaveClass('timeline-row__code');
		expect(shell?.textContent).toBe('Compiling magician\nFinished in 42s');
		expect(container.querySelectorAll('.timeline-row__detail')).toHaveLength(1);

		expect(container.querySelector('.timeline-row__body')?.textContent).toContain(
			'No index for the requested quarter.'
		);
		expect(container.querySelector('.timeline-row__capture')?.textContent).toBe(
			'screenshot captured'
		);
	});

	/**
	 * **The one inline affordance the timeline's data can support.**
	 *
	 * Copying stdout needs no caller, exactly as an output row's `In tab` and
	 * `Download` need none: the browser performs it. Everything else the owner asked
	 * for here — view, download, preview an artifact an event references — needs an
	 * address, and the only artifact reference a `TimelineEntry` carries is
	 * `screenshot: boolean`. There is no URL and no observation id on the entry, so
	 * a control would be one that could not work.
	 */
	it('offers to copy a row’s output, and offers nothing it could not perform', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		const clipboard = navigator.clipboard;
		Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });

		try {
			const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
			await openRun();

			const copy = screen.getByRole('button', { name: /^Copy output of/ });
			await fireEvent.click(copy);
			expect(writeText).toHaveBeenCalledWith('Compiling magician\nFinished in 42s');

			// It says so, on the block that was copied.
			expect(await screen.findByRole('button', { name: /^Copy output of/ })).toHaveTextContent(
				'Copied'
			);

			// One control, on the one row that has output — the capture row gets none,
			// because there is nothing to point it at.
			expect(container.querySelectorAll('.timeline-row__code-actions')).toHaveLength(1);
			const capture = container.querySelector('.timeline-row__capture')?.parentElement;
			expect(capture?.querySelector('.timeline-row__code-actions')).toBeNull();
		} finally {
			Object.defineProperty(navigator, 'clipboard', { configurable: true, value: clipboard });
		}
	});

	/**
	 * A clipboard that refuses — no secure context, or a document that is not focused
	 * — must leave the control saying `Copy`. `Copied` when nothing was copied is the
	 * class of claim this whole panel exists to remove.
	 */
	it('does not claim to have copied when the clipboard refused', async () => {
		const clipboard = navigator.clipboard;
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: { writeText: vi.fn().mockRejectedValue(new Error('not allowed')) }
		});

		try {
			renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
			await openRun();

			await fireEvent.click(screen.getByRole('button', { name: /^Copy output of/ }));
			expect(screen.getByRole('button', { name: /^Copy output of/ })).toHaveTextContent('Copy');
			expect(screen.queryByText('Copied')).not.toBeInTheDocument();
		} finally {
			Object.defineProperty(navigator, 'clipboard', { configurable: true, value: clipboard });
		}
	});

	/**
	 * The feed is its own scroll region, and that is what makes stick-to-bottom
	 * possible without dragging the whole drawer. Following the newest event in
	 * the drawer's own scroller would push the Output act off screen every time a
	 * tool returned.
	 *
	 * The scrolling itself is unassertable here — jsdom computes no layout, so
	 * every metric is zero — which is why the decision lives in `followsBottom`
	 * and is tested there over real numbers.
	 */
	it('is its own live region, announcing new entries rather than re-reading the feed', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
		await openRun();

		const feed = container.querySelector('.run-timeline');
		expect(feed).toHaveAttribute('role', 'log');
		expect(feed).toHaveAttribute('aria-label', 'Run activity');
		// The verdict keeps the panel's one `status` region: a feed announcing
		// itself as one would re-read the whole run on every push.
		expect(feed).not.toHaveAttribute('role', 'status');
	});

	/**
	 * **The panel renders the provenance it is handed and invents none.**
	 *
	 * This read "adds nothing to L3" and clicked a `Details` button, on the
	 * reasoning that a per-event token count is not an act-level identifier. Both
	 * halves have since moved: the disclosure was retired once its content was
	 * measured, and the run's *aggregate* cost — one figure about the whole act, not
	 * one per event — is now deliberately part of the Run act's provenance.
	 *
	 * What survives is the seam this test actually guards, and it is worth more than
	 * the rule it was written for: whether a figure belongs in provenance is the
	 * **adapter's** decision, made once in `taskPanelModel.ts`. The component must
	 * not add rows of its own, or two surfaces would disagree about what an act's
	 * identifiers are. The fixture here supplies provenance directly, so anything
	 * extra on screen came from the component.
	 */
	it('renders the provenance it is handed and adds no rows of its own', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
		await openRun();

		const labels = Array.from(container.querySelectorAll('.act__provenance dt')).map(
			(dt) => dt.textContent
		);
		expect(labels).toEqual(['Execution id', 'Worker']);
		const values = Array.from(container.querySelectorAll('.act__provenance dd')).map(
			(dd) => dd.textContent
		);
		expect(values.join(' ')).not.toContain('claude-opus-4');
		expect(values.join(' ')).not.toContain('cached');
	});

	/**
	 * Where the time went — the bar drawn to scale behind each latency.
	 *
	 * A duration per row is a number the reader compares by hand. What these pin
	 * is that the bar is never drawn where there is nothing to compare, and that
	 * it is drawn against the **longest** call rather than the total: the two
	 * denominators give different numbers for the same feed, so a swap cannot pass
	 * by coincidence.
	 */
	describe('the timing bars', () => {
		const timed = (...latencies: (number | null)[]): TimelineEntry[] =>
			latencies.map((latencyMs, index) => ({
				id: `tl-timed-${index}`,
				at: NOW - 300_000 + index * 1_000,
				kind: 'llm' as const,
				status: 'done' as const,
				title: `Call ${index}`,
				body: null,
				detail: null,
					latencyMs,
					model: null,
					costUsd: null,
					tokens: null,
				screenshot: false,
				executionId: null,
				agentId: null,
			}));

		const barWidths = (container: HTMLElement) =>
			rows(container).map(
				(row) => row.querySelector<HTMLElement>('.timeline-row__bar')?.style.width ?? null
			);

		it('draws nothing at all when only one row was timed', async () => {
			// The base fixture: one latency among six rows. A lone full-width bar
			// would mean "the longest of the one thing measured", which the eye reads
			// as "this took a long time" — a claim the feed does not contain.
			const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
			await openRun();

			expect(container.querySelectorAll('.timeline-row__bar')).toHaveLength(0);
			// The number itself is still there. Only the comparison is withheld.
			expect(
				rows(container).map(
					(row) => row.querySelector('.timeline-row__latency')?.textContent?.trim() ?? null
				)
			).toEqual([null, '4s', null, null, null, null]);
		});

		it('fills the track for the row that dominates and leaves the rest slivers', async () => {
			const { container } = renderPanel({
				task: withTimeline(timed(12_000, 700, 400, null), { status: 'finished' })
			});
			await openRun();

			// Against the longest: 100 / 6 / 3. Against the total (13.1s) it would be
			// 92 / 5 / 3 — a different first number, so the denominator is pinned.
			// The untimed row draws no bar, not a zero-width one.
			expect(barWidths(container)).toEqual(['100%', '6%', '3%', null]);
		});

		it('draws equal bars for equal calls, because nothing stood out', async () => {
			const { container } = renderPanel({
				task: withTimeline(timed(900, 900, 900), { status: 'finished' })
			});
			await openRun();

			// Full width rather than uniformly tiny: the bars are a relative claim,
			// and every call here really was as long as the longest. A reader
			// scanning for the slow one correctly finds no answer.
			expect(barWidths(container)).toEqual(['100%', '100%', '100%']);
		});

		it('draws a sliver for a real but tiny measurement, so it is not read as unmeasured', async () => {
			const { container } = renderPanel({
				task: withTimeline(timed(12_000, 1), { status: 'finished' })
			});
			await openRun();

			// 1ms of 12s rounds to 0%, and a zero-width bar is indistinguishable from
			// the no-bar an unmeasured row gets. Those are different facts.
			expect(barWidths(container)).toEqual(['100%', '2%']);
		});

		it('costs the feed no extra rows, whatever the bars say', async () => {
			// The act is held to "no body becomes something you scroll past to reach
			// the next act". The bar is the number's own underline, inside the
			// element that already held it — one element per row, not one line.
			const { container } = renderPanel({
				task: withTimeline(timed(12_000, 700, 400), { status: 'finished' })
			});
			await openRun();

			expect(rows(container)).toHaveLength(3);
			for (const row of rows(container)) {
				expect(row.querySelector('.timeline-row__latency')?.contains(
					row.querySelector('.timeline-row__bar')
				)).toBe(true);
			}
		});

		it('hides the bars from the screen reader, which hears the duration itself', async () => {
			const { container } = renderPanel({
				task: withTimeline(timed(12_000, 700), { status: 'finished' })
			});
			await openRun();

			// The same rule every marker in this panel follows: the graphic restates
			// a number that is already in the text, and a reader hearing it announced
			// would learn a width rather than a duration.
			for (const bar of container.querySelectorAll('.timeline-row__bar')) {
				expect(bar).toHaveAttribute('aria-hidden', 'true');
			}
		});
	});

	/**
	 * When each row happened — the fact the feed held and never rendered.
	 *
	 * `at` was fetched, sorted by and dropped. Both forms are on screen now and at
	 * the row's **leading** edge, because both answer *when*; the latency stays at
	 * the trailing edge because it answers *how long*. What jsdom can see is which
	 * facts are present and in which element; whether the resulting grid reads is a
	 * visual pass.
	 */
	describe('the time on each row', () => {
		const clocks = (container: HTMLElement) =>
			rows(container).map((row) => row.querySelector('.timeline-row__clock')?.textContent ?? null);

		const offsets = (container: HTMLElement) =>
			rows(container).map((row) => row.querySelector('.timeline-row__offset')?.textContent ?? null);

		it('renders the wall clock and the offset together, ahead of the mark and the title', async () => {
			const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
			await openRun();

			// The clock is the reader's own timezone, so the value is asserted through
			// the same formatter rather than as a literal — a fixture literal would
			// pass only in the zone it was written in. What is pinned here is that
			// every row has one and that they are not all the same string.
			expect(clocks(container)).toEqual(TIMELINE.map((entry) => timelineClock(entry.at)));
			expect(new Set(clocks(container)).size).toBe(TIMELINE.length);

			// The offsets are timezone-free and are pinned as literals: five minutes
			// of run, measured from its own start row.
			expect(offsets(container)).toEqual(['+0s', '+1m', '+2m', '+3m', '+4m', '+4m 30s']);

			// Leading edge: the `when` cluster precedes the marker, the kind and the
			// title in the row, and the latency comes after all of them. `when → what
			// → how long`, in document order, on every row.
			// `classList[0]` rather than `className`: svelte appends its own scope
			// class to every element, and the scope hash changes with any edit to the
			// stylesheet.
			const order = Array.from(rows(container)[1].children).map((child) => child.classList[0]);
			expect(order).toEqual([
				'timeline-row__when',
				'timeline-row__marker',
				'timeline-row__kind',
				'timeline-row__title',
				'timeline-row__latency',
				'timeline-row__meta'
			]);
		});

		it('makes the instant machine-readable and the offset plain text', async () => {
			const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
			await openRun();

			const clock = rows(container)[1].querySelector('.timeline-row__clock');
			expect(clock?.tagName).toBe('TIME');
			expect(clock).toHaveAttribute('datetime', new Date(TIMELINE[1].at).toISOString());
			// The offset is not a `<time>`: there is no `datetime` grammar for "1m 12s
			// after something else", and a wrong machine value is worse than none.
			expect(rows(container)[1].querySelector('.timeline-row__offset')?.tagName).toBe('SPAN');
		});

		it('holds the column empty for a row the wire gave no instant for', async () => {
			// `deriveTimeline` coalesces a missing timestamp to `0`, so this is the
			// routine shape rather than an exotic one. Neither half renders — a 1970
			// clock is an invented number that looks exactly like a real reading — and
			// the cluster itself stays, so this row's title keeps the same left edge
			// as every other row's.
			const untimed: TimelineEntry[] = [
				{ ...TIMELINE[0], id: 'tl-untimed', at: 0 },
				TIMELINE[1]
			];
			const { container } = renderPanel({ task: withTimeline(untimed, { status: 'finished' }) });
			await openRun();

			expect(clocks(container)).toEqual([null, timelineClock(TIMELINE[1].at)]);
			expect(offsets(container)).toEqual([null, '+0s']);
			expect(rows(container)[0].querySelector('.timeline-row__when')).not.toBeNull();
			expect(rows(container)[0].querySelector('.timeline-row__when')?.textContent?.trim()).toBe('');
		});

		it('omits every offset when no row in the feed carried an instant', async () => {
			// Nothing to be relative to. The clocks go too, for the same reason, and
			// the feed falls back to being what it always was: an ordered list.
			const untimed = TIMELINE.slice(0, 3).map((entry, index) => ({
				...entry,
				id: `tl-none-${index}`,
				at: 0
			}));
			const { container } = renderPanel({ task: withTimeline(untimed, { status: 'finished' }) });
			await openRun();

			expect(offsets(container)).toEqual([null, null, null]);
			expect(clocks(container)).toEqual([null, null, null]);
			// And the rows are still all there, which is the point of omitting rather
			// than of hiding.
			expect(rows(container)).toHaveLength(3);
		});
	});

	/**
	 * Which rows are bounded, and which are left flat.
	 *
	 * The border itself is unobservable here — jsdom applies no component CSS — so
	 * what this pins is the attribute the stylesheet selects on, and that it is
	 * driven by content rather than set on every row. `timelineIsolated` holds the
	 * decision itself over real entries.
	 */
	it('bounds the rows that are several lines tall and leaves the one-line rows flat', async () => {
		const { container } = renderPanel({ task: withTimeline(TIMELINE, { status: 'finished' }) });
		await openRun();

		// Run started, an LLM call with a bill, a failed tool with prose, a shell
		// command with stdout, an observation, an unmodelled event. Two of the six
		// carry a body or a detail; the LLM row's cost line is not one, or most of a
		// real feed would be framed.
		expect(rows(container).map((row) => row.dataset.timelineIsolated)).toEqual([
			'false',
			'false',
			'true',
			'true',
			'false',
			'false'
		]);
	});

	it('renders delegated executions as structured branch envelopes with time spans and return footer', async () => {
		const t1 = NOW - 100_000;
		const t2 = NOW - 60_000;
		const delegationEntries: TimelineEntry[] = [
			{
				...TIMELINE[0],
				id: 'child-1',
				at: t1,
				title: 'Child research thinking',
				executionId: 'exec-child-1',
				agentId: 'research-agent'
			},
			{
				...TIMELINE[1],
				id: 'child-2',
				at: t2,
				title: 'Child search complete',
				executionId: 'exec-child-1',
				agentId: 'research-agent'
			}
		];
		const mixedTimeline = [
			TIMELINE[0],
			...delegationEntries,
			TIMELINE[3]
		];
		const { container } = renderPanel({
			task: makeTask({
				run: {
					...RUN,
					timeline: mixedTimeline,
					delegations: [
						{
							execution_id: 'exec-child-1',
							agent_id: 'research-agent',
							status: 'completed',
							entry_count: 2,
							started_at: new Date(t1).toISOString(),
							completed_at: new Date(t2).toISOString()
						}
					]
				},
				status: 'finished'
			})
		});
		await openRun();

		// A branch envelope exists
		const envelope = container.querySelector('.timeline-delegation');
		expect(envelope).not.toBeNull();
		expect(envelope?.getAttribute('data-delegation-status')).toBe('completed');

		// Header displays agent, steps, and time span
		const summary = envelope?.querySelector('.timeline-delegation__summary');
		expect(summary?.querySelector('.timeline-delegation__agent')).toHaveTextContent(
			'Delegated to research-agent'
		);
		expect(summary?.querySelector('.timeline-delegation__meta')?.textContent).toContain(
			'2 steps · completed'
		);
		expect(summary?.querySelector('.timeline-delegation__meta')?.textContent).toContain('40s');

		// Footer displays return bar
		const footer = envelope?.querySelector('.timeline-delegation__footer');
		expect(footer?.textContent).toContain('Handed back results to main agent');
		expect(footer?.textContent).toContain('40s');

		// View mode toggle is present
		const modes = container.querySelector('.run-timeline__modes');
		expect(modes).not.toBeNull();
		const buttons = modes?.querySelectorAll('button');
		expect(buttons).toHaveLength(2);
		expect(buttons?.[0].textContent).toContain('Grouped');
		expect(buttons?.[1].textContent).toContain('Chronological');

		// Switch to Chronological mode
		await fireEvent.click(buttons![1]);

		// Now flat rows are rendered instead of .timeline-delegation
		expect(container.querySelector('.timeline-delegation')).toBeNull();
		expect(rows(container)).toHaveLength(mixedTimeline.length);
		const agentTags = container.querySelectorAll('.timeline-row__agent-tag');
		expect(agentTags.length).toBeGreaterThan(0);
		expect(agentTags[0].textContent).toBe('via research-agent');
	});

	it('renders view mode toggle on standard runs without delegations as well', async () => {
		const { container } = renderPanel({
			task: makeTask({
				run: {
					steps: [],
					responsibility: null,
					timeline: [
						{
							id: 'ev-1',
							at: NOW - 60_000,
							kind: 'llm',
							status: 'done',
							title: 'Thinking with chat',
							body: null,
							detail: null,
							latencyMs: 1200,
							model: 'claude-3-5-sonnet',
							costUsd: 0.01,
							tokens: null,
							screenshot: false,
							executionId: 'root-exec',
							agentId: 'main-agent'
						},
						{
							id: 'ev-2',
							at: NOW - 30_000,
							kind: 'tool',
							status: 'done',
							title: 'Calling bash',
							body: null,
							detail: null,
							latencyMs: 500,
							model: null,
							costUsd: null,
							tokens: null,
							screenshot: false,
							executionId: 'root-exec',
							agentId: 'main-agent'
						}
					],
					delegations: [],
					provenance: []
				}
			})
		});

		const modes = container.querySelector('.run-timeline__modes');
		expect(modes).not.toBeNull();
		const buttons = modes?.querySelectorAll('button');
		expect(buttons).toHaveLength(2);
		expect(buttons?.[0].textContent).toContain('Grouped');
		expect(buttons?.[1].textContent).toContain('Chronological');

		// Toggle to chronological and back to grouped
		await fireEvent.click(buttons![1]);
		expect(rows(container)).toHaveLength(2);
		await fireEvent.click(buttons![0]);
		expect(rows(container)).toHaveLength(2);
	});
});

describe('UnifiedTaskPanel — the Plan act', () => {
	it('leads with the ask rather than the approval that is also true', () => {
		const { container } = renderPanel({
			task: makeTask({
				plan: { ...PLAN, questions: [
						{ id: 'q1', question: 'Which quarter?' },
						{ id: 'q2', question: 'Include forecasts?' }
					] },
				attention: { source: 'clarification', summary: null, raisedAt: NOW - 4 * 60_000 }
			})
		});

		expect(summaryOf(container, 'plan')).toBe('2 questions waiting — "Which quarter?"');
	});

	it('lists the asks and the steps in the act the reader was sent to', () => {
		const { container } = renderPanel({
			task: makeTask({
				plan: { ...PLAN, questions: [{ id: 'q1', question: 'Which quarter?' }] },
				attention: { source: 'clarification', summary: null, raisedAt: NOW - 4 * 60_000 }
			})
		});

		expect(openActs(container)).toEqual(['plan']);
		expect(
			Array.from(container.querySelectorAll('.plan-ask')).map((ask) => ask.textContent)
		).toEqual(['Which quarter?']);
		expect(
			Array.from(container.querySelectorAll('.plan-step')).map((step) => step.textContent)
		).toEqual(['Read the quarterly exports', 'Summarise revenue by region']);
	});

	it('reports the plan status when nothing is waiting', () => {
		const { container } = renderPanel({ task: PLANNED });
		expect(summaryOf(container, 'plan')).toBe('approved 6m ago');
	});
});

describe('UnifiedTaskPanel — provenance stays at L3', () => {
	it('gives every act its own rows, not another act’s', async () => {
		// Opening one act and asserting the *other* acts' values are absent cannot
		// catch a panel that hands an act the wrong list: the wrong list belongs to
		// an act that is closed, so nothing renders either way. Each act has to be
		// opened and read against its own rows.
		const expected: Array<[ActId, ProvenanceEntry[]]> = [
			['plan', PLAN.provenance],
			['run', RUN.provenance],
			['output', OUTPUT.provenance]
		];

		for (const [act, rows] of expected) {
			const { container, unmount } = renderPanel({ task: makeTask({ status: 'finished' }) });

			const section = sections(container).find((candidate) => candidate.dataset.act === act);
			if (!section) throw new Error(`no ${act} act rendered`);
			const header = headerOf(section);
			if (!header) throw new Error(`no ${act} header rendered`);
			if (header.getAttribute('aria-expanded') !== 'true') await fireEvent.click(header);

			// No `Details` click: the disclosure was retired once its content was
			// measured, and provenance renders with the body it belongs to.
			const rendered = Array.from(section.querySelectorAll('dl > div')).map((row) => [
				row.querySelector('dt')?.textContent,
				row.querySelector('dd')?.textContent
			]);
			// Pairs rather than two flat lists, so a label rendered against another
			// row's value fails too.
			expect(rendered, act).toEqual(rows.map(({ label, value }) => [label, value]));
			unmount();
		}
	});

	it('shows the open act’s ids with the act, and no other act’s', async () => {
		renderPanel({ task: makeTask({ status: 'finished' }) });

		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

		// **The `Details` disclosure is gone, and this assertion inverted with it.**
		// It used to read "opening the body is not consent to see ids" and required a
		// second click. Measured on a real task, that disclosure held two identifiers
		// for a 70-event run — a toggle and a container for one fact — so the content
		// moved up rather than the button getting better copy.
		expect(screen.queryAllByRole('button', { name: /details/i })).toHaveLength(0);

		expect(screen.getByText('ex_7b1d20')).toBeInTheDocument();
		expect(screen.getByText('runner-04')).toBeInTheDocument();
		// Every act carries its own rows; a panel wiring one act's provenance into
		// another would pass a one-act assertion and fail this.
		expect(screen.queryByText('pl_9f2c4e')).toBeNull();
		expect(screen.queryByText('out_51aa9c')).toBeNull();
	});

	it('never puts an id on a collapsed act', () => {
		const { container } = renderPanel({ task: PLANNED });

		// `PLANNED` opens Run, so Run's own ids are on screen — that is the point of
		// retiring the disclosure. What must stay absent is every *closed* act's.
		expect(screen.getByText('ex_7b1d20')).toBeInTheDocument();

		for (const value of ['pl_9f2c4e', 'out_51aa9c']) {
			expect(screen.queryByText(value), value).toBeNull();
		}
		// One list, belonging to the one open act.
		expect(container.querySelectorAll('dl')).toHaveLength(1);
	});
});

describe('UnifiedTaskPanel — the three failures that look alike (design §6)', () => {
	it('does not render a load failure as a failed task', () => {
		const { container } = renderPanel({
			// Deliberately free of the word "failed": the assertions below have to
			// catch a fabricated verdict, not the error text quoting one.
			loadError: 'HTTP 503 from the task service'
		});

		expect(verdictRoot(container)).toBeNull();
		expect(actOrder(container)).toEqual([]);
		expect(screen.getByText("Can't load this task")).toBeInTheDocument();
		expect(screen.getByText('HTTP 503 from the task service')).toBeInTheDocument();
	});

	it('offers a way to retry rather than only the word', async () => {
		render(UnifiedTaskPanelHarness, {
			props: { task: null, loadError: 'HTTP 503 from the task service', now: NOW }
		});

		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));

		expect(screen.getByTestId('panel-retries')).toHaveTextContent('1');
	});

	it('says it is showing stale state instead of presenting it as current', () => {
		const { container } = renderPanel({
			task: PLANNED,
			loadError: 'network unreachable',
			lastLoadedAt: NOW - 2 * 60_000
		});

		const stale = screen.getByText('Showing last known state from 2m ago — reconnecting');
		expect(stale).toBeInTheDocument();
		// Its own live region, asserted rather than assumed. Folding this into the
		// verdict would announce nothing — the verdict's text has not changed, which
		// is exactly what is being reported — and found by its words alone this test
		// passes just as happily with the role deleted. The property is invisible on
		// screen, so nothing but an explicit assertion can catch losing it.
		expect(stale).toHaveAttribute('role', 'status');
		// Still the task's own verdict, still its acts — labelled, not withheld,
		// and not demoted to the load-failure block.
		expect(verdictRoot(container)).toHaveAttribute('data-verdict-state', 'running');
		expect(actOrder(container)).toEqual(['plan', 'run', 'output']);
		expect(screen.queryByText("Can't load this task")).toBeNull();
	});

	it('omits an age it cannot compute rather than approximating one', () => {
		renderPanel({ task: PLANNED, loadError: 'network unreachable', lastLoadedAt: null });

		const banner = screen.getByText(/Showing last known state/);
		expect(banner).toHaveTextContent('Showing last known state — reconnecting');
		expect(banner.textContent).not.toContain('ago');
	});

	it('says nothing about staleness while the data is current', () => {
		renderPanel({ task: PLANNED, lastLoadedAt: NOW - 2 * 60_000 });
		expect(screen.queryByText(/Showing last known state/)).toBeNull();
	});

	it('omits an act that failed to load rather than rendering it empty', () => {
		// The pair that separates the two claims. Same task, same everything, and
		// the only difference is whether the Output act arrived — one renders and
		// summarises what it found, the other is not there at all. An empty card
		// would assert `no output`, which may be false (design §6).
		const { container: absent, unmount } = renderPanel({
			task: makeTask({ status: 'finished', output: null })
		});
		expect(actOrder(absent)).toEqual(['plan', 'run']);
		unmount();

		const { container: empty } = renderPanel({
			task: makeTask({ status: 'finished', output: emptyOutput() })
		});
		expect(actOrder(empty)).toEqual(['plan', 'run', 'output']);
		expect(summaryOf(empty, 'output')).toBe('no output');
	});

	it('renders nothing at all while it has nothing to show', () => {
		const { container } = renderPanel({});
		expect(container.textContent?.trim()).toBe('');
	});
});

describe('UnifiedTaskPanel — the clock is a prop, not a capture', () => {
	it('reports a stall that only became one because time passed', async () => {
		const { container, rerender } = renderPanel({ task: PLANNED });
		expect(verdictRoot(container)).toHaveAttribute('data-verdict-state', 'running');

		await rerender({ now: NOW + 5 * 60_000 });

		expect(verdictRoot(container)).toHaveAttribute('data-verdict-state', 'stalled');
		expect(container.querySelector('.verdict__headline')?.textContent).toBe(
			'Stalled · no progress for 5m 30s'
		);
	});
});

describe('UnifiedTaskPanel — a moving open act does not drop the reader', () => {
	/**
	 * Two things move the open act with no reader involved: a poll that changes
	 * the verdict while the reader has chosen nothing, and a different task
	 * arriving, which resets the choice by design. Both remove the body of the
	 * act that was open, and every control at L2 and L3 lives in that body.
	 *
	 * The section owns the rescue — it is the only thing that knows where its own
	 * body and header are — but the paths that trigger it are the panel's, and a
	 * seam tested from one side only is how this plan's recurring defect keeps
	 * arriving. So both are exercised here, through the same act-swap the panel
	 * really performs.
	 */
	/**
	 * Put focus somewhere inside the open act's body, and return the act.
	 *
	 * **This used to stand on the `Details` control**, which was the one focusable
	 * thing every act's body was guaranteed to have. Retiring that disclosure left
	 * some bodies with no focusable descendant at all — the Run act on a planned
	 * task renders a timeline and a definition list, neither of which takes focus.
	 *
	 * So: any real control if the body has one, and otherwise the body itself, made
	 * focusable for the duration. That is a faithful stand-in rather than a
	 * weakening — `releaseFocusFromBody` guards on `bodyEl.contains(activeElement)`,
	 * and a node contains itself, so this exercises exactly the branch a reader
	 * standing on a control would.
	 */
	function focusInsideTheOpenAct(container: HTMLElement): HTMLElement {
		const open = sections(container).find(
			(section) => headerOf(section)?.getAttribute('aria-expanded') === 'true'
		) as HTMLElement;
		const body = open.querySelector<HTMLElement>('.act__body');
		if (!body) throw new Error('the open act rendered no body to stand in');

		const control = body.querySelector<HTMLElement>('button, a[href], [tabindex]');
		const target = control ?? body;
		if (target === body) body.tabIndex = -1;
		target.focus();
		expect(document.activeElement).toBe(target);
		return open;
	}

	it('leaves focus on the header of the act a poll closed, not on the document', async () => {
		const { container, rerender } = renderPanel({ task: PLANNED });
		expect(openActs(container)).toEqual(['run']);

		const run = focusInsideTheOpenAct(container);

		// A poll, not a reader: same task, finished, so the panel follows the
		// state to Output and the Run body it was showing stops existing.
		await rerender({ task: makeTask({ status: 'finished' }) });
		expect(openActs(container)).toEqual(['output']);

		expect(document.activeElement).toBe(headerOf(run));
		expect(document.activeElement).not.toBe(document.body);
	});

	it('does the same when a different task resets the choice', async () => {
		const { container, rerender } = renderPanel({ task: makeTask({ id: 'task_alpha' }) });

		await fireEvent.click(screen.getByRole('button', { name: /^Plan/ }));
		expect(openActs(container)).toEqual(['plan']);

		const plan = focusInsideTheOpenAct(container);

		// A second task in the same state: the only field that differs is the id,
		// so nothing but identity drops the choice — and dropping it closes Plan.
		await rerender({ task: makeTask({ id: 'task_beta' }) });
		expect(openActs(container)).toEqual(['run']);

		expect(document.activeElement).toBe(headerOf(plan));
		expect(document.activeElement).not.toBe(document.body);
	});
});

/**
 * **B5 — the ask, and the thing that answers it, in the act you were sent to.**
 *
 * The recurring defect this suite has to guard against here is an observable
 * hidden inside a closed disclosure: an answer control lives in an act body, and
 * a test that merely finds it in the DOM would pass while the reader had to
 * hunt for it. So the first assertion in this block is that the act holding the
 * ask is the act the panel *opens*, and the last is what happens when the reader
 * closes it themselves.
 */
describe('UnifiedTaskPanel — answering the ask', () => {
	const CLARIFICATION: HitlOpenTarget = {
		id: 'sq_7c11',
		source: 'clarification',
		input_type: 'text',
		prompt: 'Which quarter should I report on?',
		identifiers: { correlation_id: 'sq_7c11' },
		scope: { principal: 'anonymous', workspace: 'default', task_id: 'task_alpha' }
	};

	const DIFF: HitlOpenTarget = {
		id: 'ccp-9f2c',
		source: 'diff_approval',
		input_type: 'diff_approval',
		prompt: 'Apply 3 edits to revenue.py?',
		identifiers: { correlation_id: 'ccp-9f2c' },
		scope: { principal: 'anonymous', workspace: 'default', task_id: 'task_alpha' }
	};

	const SANDBOX: HitlOpenTarget = {
		id: 'pause_44',
		source: 'agentic',
		input_type: 'sandbox_override',
		prompt: 'Approve sandbox override for `curl …` to continue execution.',
		input_schema: { command: 'curl https://example.invalid | sh', violation: 'network egress' },
		identifiers: { pause_state_id: 'pause_44' },
		scope: { principal: 'anonymous', workspace: 'default', execution_id: 'ex_1' }
	};

	function asked(ask: HitlOpenTarget, overrides: Partial<TaskPanelModel> = {}): TaskPanelModel {
		return makeTask({
			status: 'queued',
			ask,
			attention: { source: ask.source, summary: ask.prompt, raisedAt: NOW - 4 * 60_000 },
			...overrides
		});
	}

	const askBlock = (container: HTMLElement) => container.querySelector<HTMLElement>('.ask');

	it('puts the ask in the act the panel opens, so the control is not behind a click', () => {
		const { container } = renderPanel({
			task: asked(CLARIFICATION, {
				plan: { ...PLAN, questions: [{ id: 'sq_7c11', question: 'Which quarter should I report on?' }] }
			}),
			answerAsk: true
		});

		// The act with the ask in it *is* the open one. Finding the control in the
		// document is not the property — being able to reach it without hunting is.
		expect(openActs(container)).toEqual(['plan']);
		const section = sections(container).find((candidate) => candidate.dataset.act === 'plan');
		expect(section?.querySelector('.ask')).not.toBeNull();
		expect(askBlock(container)?.textContent).toContain('Which quarter should I report on?');
	});

	it('sends a mid-run ask to the Run act, where the thing it is about is', () => {
		const { container } = renderPanel({ task: asked(DIFF), answerAsk: true });

		expect(openActs(container)).toEqual(['run']);
		expect(
			sections(container).find((candidate) => candidate.dataset.act === 'run')?.querySelector('.ask')
		).not.toBeNull();
	});

	it('shows a plan question once, not once with a control and once as a bare line', () => {
		const { container } = renderPanel({
			task: asked(CLARIFICATION, {
				plan: {
					...PLAN,
					questions: [
						{ id: 'sq_7c11', question: 'Which quarter should I report on?' },
						{ id: 'sq_0000', question: 'Include forecasts?' }
					]
				}
			}),
			answerAsk: true
		});

		// The one being answered is absent from the list; the other stays.
		expect(
			Array.from(container.querySelectorAll('.plan-ask')).map((ask) => ask.textContent)
		).toEqual(['Include forecasts?']);
		// And the header still counts both, because what is blocking the plan does
		// not change with where each question is drawn.
		expect(summaryOf(container, 'plan')).toBe(
			'2 questions waiting — "Which quarter should I report on?"'
		);
	});

	it('renders the answer control in place for an ask the reader can just answer', async () => {
		const { container, getByTestId } = render(UnifiedTaskPanelHarness, {
			props: { task: asked(CLARIFICATION), now: NOW, answerAsk: true }
		});

		const field = screen.getByRole('textbox', { name: 'Response' });
		await fireEvent.input(field, { target: { value: 'Q3' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Answer' }));

		// The id travels with the answer, because the value alone cannot tell a
		// panel that answered the ask it was showing from one that answered
		// whichever ask it happened to hold.
		expect(getByTestId('panel-ask-events').textContent).toBe(
			'sq_7c11:{"kind":"text","value":"Q3"}'
		);
		// And the ask is still on screen: nothing here decides it was answered.
		expect(askBlock(container as HTMLElement)).not.toBeNull();
	});

	it('hands off the ones it should, with a control that says where it goes', async () => {
		for (const [target, label] of [
			[DIFF, 'Review the changes →'],
			[SANDBOX, 'Review and decide →']
		] as const) {
			const { getByTestId, unmount } = render(UnifiedTaskPanelHarness, {
				props: { task: asked(target), now: NOW, answerAsk: true }
			});

			await fireEvent.click(screen.getByRole('button', { name: label }));
			// `open` is "take me to the focused prompt": there is no answer to
			// carry, only a request to be taken somewhere that can collect one.
			expect(getByTestId('panel-ask-events').textContent).toBe(`${target.id}:open`);
			unmount();
		}
	});

	it('promises nothing on a surface that cannot post', () => {
		// The verdict line has already said what is being asked. A control here
		// with no listener behind it would be a promise with nothing behind it.
		const { container } = renderPanel({ task: asked(CLARIFICATION), answerAsk: false });
		expect(askBlock(container)).toBeNull();
	});

	it('keeps the ask open and says why when the answer did not land', () => {
		const { container } = renderPanel({
			task: asked(CLARIFICATION),
			answerAsk: true,
			askState: { id: 'sq_7c11', status: 'failed', message: 'Pause state not found' }
		});

		// The ask is still there — it was never answered — and the reason is with
		// it. An ask that looked answered and was not is the worst instance of the
		// failure this panel exists to prevent.
		expect(askBlock(container)?.textContent).toContain('Which quarter should I report on?');
		expect(screen.getByRole('alert').textContent).toBe('Pause state not found');
	});

	it('does not accept a second answer while the first is in flight', () => {
		renderPanel({
			task: asked(CLARIFICATION),
			answerAsk: true,
			askState: { id: 'sq_7c11', status: 'sending', message: null }
		});

		expect(screen.getByRole('button', { name: 'Sending…' })).toBeDisabled();
	});

	it('never renders one ask’s outcome under another', () => {
		// This surface polls, so the ask on screen can change while a post is in
		// the air. Every value in the failed state is valid; only the id says it
		// belongs somewhere else.
		const { container } = renderPanel({
			task: asked(CLARIFICATION),
			answerAsk: true,
			askState: { id: 'sq_0000', status: 'failed', message: 'A different ask failed' }
		});

		expect(container.querySelector('.ask__error')).toBeNull();
		expect(screen.getByRole('button', { name: 'Answer' })).toBeInTheDocument();
	});

	it('does not follow the reader into an act the ask has nothing to do with', async () => {
		// The ask belongs to the act its source routes to, not to whichever act
		// happens to be open. Rendering it wherever the reader is would put a diff
		// review inside the Output act, under a heading about files.
		const { container } = renderPanel({ task: asked(DIFF), answerAsk: true });
		expect(openActs(container)).toEqual(['run']);

		const output = sections(container).find((candidate) => candidate.dataset.act === 'output');
		await fireEvent.click(headerOf(output as HTMLElement));

		expect(openActs(container)).toEqual(['output']);
		expect(output?.querySelector('.ask')).toBeNull();
		expect(askBlock(container)).toBeNull();
	});

	it('lets the reader close the act it is in, and does not reopen it under them', async () => {
		const { container } = renderPanel({ task: asked(DIFF), answerAsk: true });
		expect(openActs(container)).toEqual(['run']);

		const header = sections(container)
			.find((candidate) => candidate.dataset.act === 'run')
			?.querySelector('button');
		await fireEvent.click(header as HTMLElement);

		// Closed by the reader's own gesture, so the control goes with the body.
		// The verdict above still says the task is waiting on them, which is the
		// thing that brings them back.
		expect(openActs(container)).toEqual([]);
		expect(askBlock(container)).toBeNull();
		expect(verdictRoot(container)?.dataset.verdictState).toBe('waiting');
	});
});

/**
 * The run picker, on screen.
 *
 * Three claims the pure modules cannot make: that the control is **absent** at one
 * run rather than greyed, that it sits **where its effect is** — immediately above
 * the act it scopes — and that picking a run reports it once and only when it
 * changed.
 */
const THREE_RUNS: TaskPanelRuns = {
	options: [
		{ executionId: 'ex_3', ordinal: 3, startedAt: NOW, status: 'failed', label: '#3 · 2 Jul 14:32 · failed' },
		{ executionId: 'ex_2', ordinal: 2, startedAt: NOW - 3_600_000, status: 'finished', label: '#2 · 2 Jul 11:08 · finished' },
		{ executionId: 'ex_1', ordinal: 1, startedAt: NOW - 7_200_000, status: 'failed', label: '#1 · 2 Jul 09:12 · failed' }
	],
	selectedId: 'ex_2'
};

const runPicker = (container: HTMLElement) => container.querySelector<HTMLElement>('.run-picker');

describe('choosing which run the acts describe', () => {
	it('renders no control at one run — absent, never disabled', () => {
		const { container } = renderPanel({ task: makeTask({ runs: null }), now: NOW });

		expect(runPicker(container)).toBeNull();
		// The stronger claim: not a disabled `<select>` anywhere, which would assert
		// there is a choice and then refuse it.
		expect(container.querySelector('select')).toBeNull();
	});

	it('renders the control when there is more than one run', () => {
		const { container } = renderPanel({ task: makeTask({ runs: THREE_RUNS }), now: NOW });

		const picker = runPicker(container);
		expect(picker).not.toBeNull();
		expect(picker?.querySelector('select')?.hasAttribute('disabled')).toBe(false);
	});

	/**
	 * **Placement is the claim about scope.** A control floating in the header would
	 * read as scoping the panel; one inside the Run act's body would vanish when the
	 * act collapsed. It sits between them, and the element order is what says so.
	 */
	it('sits immediately above the Run act, and after every act before it', () => {
		const { container } = renderPanel({ task: makeTask({ runs: THREE_RUNS }), now: NOW });

		const picker = runPicker(container) as HTMLElement;
		const run = sections(container).find((section) => section.dataset.act === 'run') as HTMLElement;
		const plan = sections(container).find((section) => section.dataset.act === 'plan') as HTMLElement;

		expect(picker.nextElementSibling).toBe(run);
		expect(plan.compareDocumentPosition(picker) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
		// Not inside any act, so collapsing one cannot hide it.
		expect(picker.closest('section[data-act]')).toBeNull();
	});

	it('says what it scopes, and states the total the options cannot', () => {
		const { container } = renderPanel({ task: makeTask({ runs: THREE_RUNS }), now: NOW });

		expect(runPicker(container)?.textContent).toContain('Run details for');
		expect(runPicker(container)?.textContent).toContain('of 3 runs');
	});

	it('lists the options the model built, in that order, and selects the shown run', () => {
		const { container } = renderPanel({ task: makeTask({ runs: THREE_RUNS }), now: NOW });

		const select = runPicker(container)?.querySelector('select') as HTMLSelectElement;
		expect(Array.from(select.options).map((option) => option.textContent?.trim())).toEqual(
			THREE_RUNS.options.map((option) => option.label)
		);
		expect(select.value).toBe('ex_2');
	});

	it('never puts a bare execution id in front of the reader', () => {
		const { container } = renderPanel({ task: makeTask({ runs: THREE_RUNS }), now: NOW });

		const text = runPicker(container)?.textContent ?? '';
		for (const option of THREE_RUNS.options) {
			expect(text).not.toContain(option.executionId);
		}
	});

	it('reports the run the reader picked, by id', async () => {
		const { container, getByTestId } = render(UnifiedTaskPanelHarness, {
			props: { task: makeTask({ runs: THREE_RUNS }), now: NOW }
		});

		const select = container.querySelector('select') as HTMLSelectElement;
		await fireEvent.change(select, { target: { value: 'ex_1' } });

		expect(getByTestId('panel-run-events').textContent).toBe('ex_1');
	});

	/**
	 * Re-picking the run already shown would spend a request to arrive back where it
	 * was, and blank the Run act's timeline for a round trip while doing it — which
	 * reads as the panel losing the run the reader just confirmed.
	 */
	it('reports nothing when the reader re-picks the run already shown', async () => {
		const { container, getByTestId } = render(UnifiedTaskPanelHarness, {
			props: { task: makeTask({ runs: THREE_RUNS }), now: NOW }
		});

		const select = container.querySelector('select') as HTMLSelectElement;
		await fireEvent.change(select, { target: { value: 'ex_2' } });

		expect(getByTestId('panel-run-events').textContent).toBe('');
	});

	it('has an accessible name that shares words with the label beside it', () => {
		const { container } = renderPanel({ task: makeTask({ runs: THREE_RUNS }), now: NOW });

		const select = container.querySelector('select') as HTMLSelectElement;
		const name = select.getAttribute('aria-label') ?? '';
		// A control whose accessible name shares no words with the visible text is
		// unaddressable by voice: the reader says "Run details" and nothing matches.
		expect(name).toContain('Run details for');
	});

	it('leaves the verdict alone — it is the task’s, not the selected run’s', () => {
		const finished = makeTask({ status: 'finished', runs: THREE_RUNS });
		const { container } = renderPanel({ task: finished, now: NOW });

		expect(verdictRoot(container)?.dataset.verdictState).toBe('finished');
		// And the failed run the reader is looking at is named right below it, which is
		// what makes the pair legible rather than contradictory.
		expect(runPicker(container)?.textContent).toContain('failed');
	});

	it('renders no control when the task has no Run act to scope', () => {
		const { container } = renderPanel({
			task: makeTask({ run: null, runs: THREE_RUNS }),
			now: NOW
		});

		expect(actOrder(container)).not.toContain('run');
		expect(runPicker(container)).toBeNull();
	});
});
