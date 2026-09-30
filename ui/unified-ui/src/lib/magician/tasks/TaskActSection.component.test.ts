import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import TaskActSectionHarness from '../../../test/fixtures/TaskActSectionHarness.svelte';
import TaskActSection, { type ProvenanceEntry } from './TaskActSection.svelte';
import { ACT_TITLES } from './taskCapabilities';

afterEach(cleanup);

/**
 * Every fixture below keeps four things apart that the markup could confuse, and
 * each pair is pinned by at least one assertion rather than left to coincidence:
 *
 * - `id` and `title` differ only in case (`plan` / `Plan`), so the accessible
 *   name is asserted as an exact string, which is case-sensitive.
 * - `title` and `summary` are both header text, so one test pins their order.
 * - a provenance `label` and its `value` are both L3 strings, so one test pins
 *   which element each lands in.
 * - `open` and the provenance disclosure are both "open" flags, which is exactly
 *   what the gate test below separates: an open act with closed provenance.
 */
const PLAN_ID = { label: 'Plan id', value: 'pl_9f2c4e' };

/**
 * Three rows, every label and value distinct. A one-row fixture cannot tell an
 * `{#each}` apart from a hardcoded `provenance[0]`, so every case that only
 * needs *some* provenance uses `PLAN_ID` above and this one exists to pin that
 * the list is a list.
 */
const PLAN_PROVENANCE: ProvenanceEntry[] = [
	PLAN_ID,
	{ label: 'Revision', value: 'r3' },
	{ label: 'Source', value: 'chat' }
];

describe('TaskActSection section', () => {
	it('marks the section with the act id, so a panel can address one act without wrapping it', () => {
		const { container } = render(TaskActSection, {
			props: { id: 'output', title: ACT_TITLES.output, summary: '' }
		});

		// Lowercase, where the header renders `Output`: the attribute carries the
		// id, not the title, and the two differ only in case.
		expect(container.querySelector('section')).toHaveAttribute('data-act', 'output');
	});
});

describe('TaskActSection header — L1', () => {
	it('keeps its summary on the collapsed header, so the story reads without expanding anything', () => {
		render(TaskActSection, {
			props: {
				id: 'run',
				title: ACT_TITLES.run,
				summary: '14 steps · 2 retries · 3m 12s',
				open: false
			}
		});

		expect(screen.getByText('14 steps · 2 retries · 3m 12s')).toBeInTheDocument();
	});

	it('names the act before its summary, so the act is what the eye lands on', () => {
		render(TaskActSection, {
			props: { id: 'plan', title: ACT_TITLES.plan, summary: 'approved 6m ago' }
		});

		// Exact and case-sensitive: it fails if the two are swapped, and it fails
		// if the header renders `id` where it should render `title`.
		expect(screen.getByRole('button', { name: 'Plan approved 6m ago' })).toBeInTheDocument();
	});

	it('renders the act title alone when the summary has nothing to claim', () => {
		render(TaskActSection, { props: { id: 'plan', title: ACT_TITLES.plan, summary: '' } });

		expect(screen.getByRole('button', { name: 'Plan' })).toBeInTheDocument();
	});

	it('is a real button that reports whether the act is open', async () => {
		const { rerender } = render(TaskActSection, {
			props: { id: 'output', title: ACT_TITLES.output, summary: 'no output', open: false }
		});

		const header = screen.getByRole('button', { name: 'Output no output' });
		expect(header.tagName).toBe('BUTTON');
		expect(header).toHaveAttribute('aria-expanded', 'false');

		await rerender({ open: true });
		expect(header).toHaveAttribute('aria-expanded', 'true');
	});

	it('asks the panel to open the act rather than opening itself, because only one act may be open', async () => {
		render(TaskActSectionHarness, { props: { id: 'run', open: false } });

		await fireEvent.click(screen.getByRole('button', { name: /^Run/ }));

		// The id, so a panel composing three of these needs no closure per section.
		expect(screen.getByTestId('act-toggles')).toHaveTextContent('run');
		// Still closed: the section reports the click, the panel decides.
		expect(screen.getByRole('button', { name: /^Run/ })).toHaveAttribute('aria-expanded', 'false');
	});
});

describe('TaskActSection body — L2', () => {
	it('renders the act body only once the act is open', async () => {
		const { rerender } = render(TaskActSectionHarness, { props: { id: 'run', open: false } });

		expect(screen.queryByText('Read 3 files')).toBeNull();

		await rerender({ open: true });
		expect(screen.getByText('Read 3 files')).toBeInTheDocument();
	});
});

/**
 * **The ladder is three levels here, not four, and every case below was written
 * against the fourth.**
 *
 * L3 shipped as a `Details` disclosure over this list. It was retired once the
 * content was measured: the Output act's held four rows of which three had their
 * own value as their label, and the Run act's held two identifiers. A toggle, a
 * label promising more, and a container, for one fact — which is the `Questions: 2`
 * defect the design opens by naming, a control whose only information is that
 * information exists elsewhere.
 *
 * So the gate these cases were about is gone, and the act's own open state is the
 * only gate left. What survived the rewrite is every claim that was really about
 * the *content*: which element a label lands in, that the list is a list, that a
 * closed act shows none of it, and that a level which is not showing is absent from
 * the document rather than hidden with CSS.
 */
describe('TaskActSection provenance — L3', () => {
	it('renders a provenance value with the act’s body, and nothing before it', async () => {
		const { rerender } = render(TaskActSection, {
			props: {
				id: 'plan',
				title: ACT_TITLES.plan,
				summary: 'approved 6m ago',
				open: false,
				provenance: [PLAN_ID]
			}
		});

		// Closed is the gate now. It used to be closed *and* a second click.
		expect(screen.queryByText('pl_9f2c4e')).toBeNull();

		await rerender({ open: true });

		// **No click between those two assertions**, which is the whole of what
		// retiring the disclosure changed. Pinned here as well as by the absence
		// below, because this is the case whose name used to promise the opposite.
		expect(screen.getByText('pl_9f2c4e')).toBeInTheDocument();
	});

	it('offers no control to reveal what it is already showing', () => {
		render(TaskActSection, {
			props: {
				id: 'plan',
				title: ACT_TITLES.plan,
				summary: 'approved 6m ago',
				open: true,
				provenance: PLAN_PROVENANCE
			}
		});

		// The retirement, asserted rather than left implied by the cases that stopped
		// clicking it. `queryAll` so this reads as a count of zero rather than
		// throwing the way a `getBy` would.
		expect(screen.queryAllByRole('button', { name: /details/i })).toHaveLength(0);
		// One button in the act, and it is the header.
		expect(screen.getAllByRole('button')).toHaveLength(1);
		expect(screen.getByRole('button', { name: /^Plan/ })).toHaveAttribute('aria-expanded', 'true');
	});

	it('takes the values out of the document when the act closes, rather than hiding them', async () => {
		const { container, rerender } = render(TaskActSection, {
			props: {
				id: 'plan',
				title: ACT_TITLES.plan,
				summary: 'approved 6m ago',
				open: true,
				provenance: [PLAN_ID]
			}
		});

		expect(container.querySelectorAll('dl')).toHaveLength(1);

		await rerender({ open: false });

		// The element itself is gone, not merely unreadable: `queryByText` alone
		// passes just as happily against a `display: none` that assistive tech and a
		// text search disagree about. This is the same rule the acts follow for any
		// level that is not showing.
		expect(container.querySelectorAll('dl')).toHaveLength(0);
		expect(screen.queryByText('pl_9f2c4e')).toBeNull();
	});

	it('renders every provenance row, in order, rather than only the first', () => {
		const { container } = render(TaskActSection, {
			props: {
				id: 'plan',
				title: ACT_TITLES.plan,
				summary: 'approved 6m ago',
				open: true,
				provenance: PLAN_PROVENANCE
			}
		});

		const rendered = Array.from(container.querySelectorAll('dl > div')).map((row) => [
			row.querySelector('dt')?.textContent,
			row.querySelector('dd')?.textContent
		]);

		// Pairs rather than two flat lists, so this fails three separate ways: a
		// row dropped, the rows reordered, and a label rendered against another
		// row's value.
		expect(rendered).toEqual(PLAN_PROVENANCE.map(({ label, value }) => [label, value]));
	});

	it('names the value rather than rendering a second one beside it', () => {
		render(TaskActSection, {
			props: {
				id: 'plan',
				title: ACT_TITLES.plan,
				summary: 'approved 6m ago',
				open: true,
				provenance: [PLAN_ID]
			}
		});

		// Both are L3 strings; only their positions say which is the opaque one.
		expect(screen.getByText('Plan id').tagName).toBe('DT');
		expect(screen.getByText('pl_9f2c4e').tagName).toBe('DD');
	});

	it('keeps every provenance element out of a collapsed act, chrome included', () => {
		const { container } = render(TaskActSection, {
			props: {
				id: 'plan',
				title: ACT_TITLES.plan,
				summary: 'approved 6m ago',
				open: false,
				provenance: [PLAN_ID]
			}
		});

		// The label and the value were always the point of this case; the missing
		// `Details` button used to be a third absence and is now every act's, which
		// makes it no test of *this* one. The container takes its place — a closed act
		// with an empty `<dl>` in it would satisfy both text assertions.
		expect(container.querySelector('dl')).toBeNull();
		expect(screen.queryByText('Plan id')).toBeNull();
		expect(screen.queryByText('pl_9f2c4e')).toBeNull();
	});

	it('renders no provenance chrome at all when the act has none to show', () => {
		const { container } = render(TaskActSection, {
			props: {
				id: 'output',
				title: ACT_TITLES.output,
				summary: 'no output',
				open: true,
				provenance: []
			}
		});

		// This asked for the absence of the `Details` button, which is now absent from
		// every act however much provenance it has — so the case passed while saying
		// nothing. What it was protecting is that an act with no provenance renders no
		// *container* either: an empty `<dl>` carries this component's top margin and
		// would put unexplained air under a body that has nothing more to say.
		expect(container.querySelector('dl')).toBeNull();
		expect(container.querySelector('.act__body')).not.toBeNull();
	});

	it('shows the values again when the act reopens, because there is no second gate', async () => {
		const { rerender } = render(TaskActSection, {
			props: {
				id: 'plan',
				title: ACT_TITLES.plan,
				summary: 'approved 6m ago',
				open: true,
				provenance: [PLAN_ID]
			}
		});

		expect(screen.getByText('pl_9f2c4e')).toBeInTheDocument();

		await rerender({ open: false });
		await rerender({ open: true });

		// **This assertion inverted when the disclosure was retired, and that is the
		// point of keeping the case.** It used to read "reopening the act does not
		// reveal it": the act remembered that the reader had not asked for L3, so a
		// poll that closed and reopened the act cost them their click. There is no
		// such state to remember now, and the reader who reopens an act gets the same
		// act back rather than a partly-collapsed one.
		expect(screen.getByText('pl_9f2c4e')).toBeInTheDocument();
	});
});

describe('TaskActSection keeps focus somewhere when the body goes', () => {
	const openPlan = () => ({
		id: 'plan' as const,
		title: ACT_TITLES.plan,
		summary: 'approved 6m ago',
		open: true,
		provenance: [PLAN_ID]
	});

	/**
	 * The act that is open is not always the act the reader opened. With no
	 * choice made the panel follows the task's state, so a poll that moves the
	 * verdict — or a different task arriving — closes this body under a reader
	 * standing in it, and every control at L2 and L3 is inside it.
	 *
	 * `document.body` is where focus falls when a focused node is removed, and
	 * that is nowhere: the reader's next Tab restarts from the top of a document
	 * the drawer has just declared modal. The header is the sensible landing
	 * place rather than a merely valid one — it owns the body that went away, it
	 * is still on screen, and it is the one control that brings the body back.
	 *
	 * **Through the harness, because the component no longer renders a focusable
	 * thing of its own.** This case stood on the act's `Details` button, which was
	 * the only one a bare `TaskActSection` produced; retiring that disclosure left
	 * the rescue with nothing to rescue in a prop-only render. The slotted control
	 * is the more faithful setup anyway — in the panel the caret is on an output
	 * row's Open, a preview toggle, or an ask's submit, and every one of those
	 * arrives through this slot.
	 */
	it('hands focus back to the header when the act closes under the reader', async () => {
		const { rerender } = render(TaskActSectionHarness, { props: { id: 'plan', open: true } });

		const inBody = screen.getByRole('button', { name: 'Open output file' });
		inBody.focus();
		expect(document.activeElement).toBe(inBody);

		await rerender({ open: false });

		expect(document.activeElement).toBe(screen.getByRole('button', { name: /^Plan/ }));
		// Named as well as compared, because `<body>` is what this replaces and a
		// wrong-element failure and a nowhere failure should not read alike.
		expect(document.activeElement).not.toBe(document.body);
	});

	/**
	 * The other half, and the one that makes the rule "rescue focus" rather than
	 * "take focus": a reader whose caret was never in this act does not have it
	 * dragged here because a poll closed a body they were not standing in.
	 */
	it('leaves focus where it was when it was never inside the body', async () => {
		const elsewhere = document.createElement('button');
		document.body.appendChild(elsewhere);

		const { rerender } = render(TaskActSection, { props: openPlan() });

		elsewhere.focus();
		await rerender({ open: false });

		expect(document.activeElement).toBe(elsewhere);
		elsewhere.remove();
	});
});
