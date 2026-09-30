import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import TaskPanelDrawerHarness from '../../../test/fixtures/TaskPanelDrawerHarness.svelte';
import TaskPanelDrawer from './TaskPanelDrawer.svelte';
import type { TaskPanelModel } from './UnifiedTaskPanel.svelte';

const NOW = 1_000_000;

const TASK: TaskPanelModel = {
	id: 'task_alpha',
	status: 'running',
	attention: null,
	ask: null,
	error: null,
	queuedFor: null,
	currentStep: 3,
	totalSteps: 7,
	currentStepLabel: 'Searching memory',
	elapsedMs: 192_000,
	lastProgressAt: NOW - 30_000,
	plan: null,
	run: { steps: [], timeline: null, responsibility: null, provenance: [] },
	output: null,
	runs: null
};

/**
 * A real trigger outside the drawer, focused before it opens — which is what a
 * reader activating a task row leaves behind. `document.body` would prove
 * nothing: the drawer's restoration deliberately skips it, so a test that never
 * focused anything would pass whether or not restoration existed.
 */
let trigger: HTMLButtonElement;

function openTrigger(): HTMLButtonElement {
	const button = document.createElement('button');
	button.textContent = 'Open the task';
	document.body.appendChild(button);
	button.focus();
	return button;
}

afterEach(() => {
	cleanup();
	trigger?.remove();
});

describe('TaskPanelDrawer takes focus and gives it back', () => {
	/**
	 * The drawer declares `aria-modal="true"`, which hides the list behind it
	 * from assistive tech. Leaving the caret out there is the failure that
	 * declaration creates: the reader's next Tab walks a list their screen
	 * reader has just been told is not there.
	 */
	it('moves focus into the drawer on open and back to the trigger on close', async () => {
		trigger = openTrigger();

		const { unmount } = render(TaskPanelDrawer, {
			props: { task: TASK, title: 'Reindex the corpus', now: NOW }
		});

		const dialog = screen.getByRole('dialog', { name: 'Task panel' });
		expect(document.activeElement).toBe(dialog);

		unmount();
		expect(document.activeElement).toBe(trigger);
	});

	/**
	 * A card that re-rendered under a poll, or a task that dropped out of the
	 * filter, takes its trigger with it. Restoring onto a detached node is a
	 * no-op in a browser and would be an error to reason about here, so the
	 * drawer checks — and this is the case that proves it checks.
	 */
	it('does not throw when the trigger has left the document', async () => {
		trigger = openTrigger();

		const { unmount } = render(TaskPanelDrawer, {
			props: { task: TASK, title: 'Reindex the corpus', now: NOW }
		});
		trigger.remove();

		expect(() => unmount()).not.toThrow();
	});
});

describe('TaskPanelDrawer is never blank', () => {
	/**
	 * The ordinary loading case: a task asked for, nothing back yet, and no
	 * failure to report. The panel renders nothing without a model — that is
	 * deliberate — so before this the surfaces showed a titled header over an
	 * empty body.
	 */
	it('shows a skeleton while the task is loading, not an empty body', async () => {
		render(TaskPanelDrawer, {
			props: { task: null, loadError: null, title: 'Reindex the corpus', now: NOW }
		});

		const dialog = screen.getByRole('dialog', { name: 'Task panel' });
		expect(dialog.querySelectorAll('.native-skeleton').length).toBeGreaterThan(0);
		// The header is still there, so the assertion above is about a body that
		// filled rather than about a drawer that never rendered.
		expect(screen.getByRole('heading', { level: 2 })).toHaveTextContent('Reindex the corpus');
	});

	/**
	 * The other half of `task === null`. A failure has something to say, and the
	 * panel says it — so the skeleton must stand down, or `Can't load this task`
	 * would be replaced by a shimmer that claims a load is still in flight.
	 */
	it('yields to the panel\'s load failure rather than shimmering over it', async () => {
		render(TaskPanelDrawer, {
			props: { task: null, loadError: 'network unreachable', title: null, now: NOW }
		});

		const dialog = screen.getByRole('dialog', { name: 'Task panel' });
		expect(dialog.querySelectorAll('.native-skeleton').length).toBe(0);
		expect(screen.getByText("Can't load this task")).toBeInTheDocument();
	});

	it('renders the panel once a task arrives, and no skeleton beside it', async () => {
		render(TaskPanelDrawer, { props: { task: TASK, title: 'Reindex the corpus', now: NOW } });

		const dialog = screen.getByRole('dialog', { name: 'Task panel' });
		expect(dialog.querySelector('[data-verdict-state]')).toHaveAttribute(
			'data-verdict-state',
			'running'
		);
		expect(dialog.querySelectorAll('.native-skeleton').length).toBe(0);
	});
});

describe('TaskPanelDrawer chrome', () => {
	/** One panel, one name. `/tasks` announced itself as the panel it replaced. */
	it('names itself once, for both surfaces', async () => {
		render(TaskPanelDrawer, { props: { task: TASK, title: 'Reindex the corpus', now: NOW } });

		expect(screen.getByRole('dialog', { name: 'Task panel' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Close task panel' })).toBeInTheDocument();
	});

	/**
	 * **No title row rather than the word `Task`**, which reverses the earlier
	 * behaviour on purpose.
	 *
	 * The header used to be one row and the title was the only thing in it, so it
	 * had to say *something*. It is now four rows with a description row of its own
	 * (the row the owner called mandatory), and `Task` over a real description is a
	 * placeholder standing between the reader and the sentence that answers them.
	 * The dialog is still named — `aria-label="Task panel"` on the dialog itself —
	 * so nothing loses its accessible name.
	 */
	it('renders no title row when the surface has no title, rather than a placeholder', async () => {
		render(TaskPanelDrawer, { props: { task: null, title: null, now: NOW } });

		expect(screen.queryByRole('heading', { level: 2 })).not.toBeInTheDocument();
		expect(screen.getByRole('dialog', { name: 'Task panel' })).toBeInTheDocument();
	});

	/**
	 * Row 3 of the header, and the row the owner called mandatory: the task in its
	 * own words, under a title that only names it.
	 */
	it('renders the description as its own row under the title', async () => {
		render(TaskPanelDrawer, {
			props: {
				task: TASK,
				title: 'Reindex the corpus',
				description: 'Rebuild the embedding index from the current corpus and report drift.',
				now: NOW
			}
		});

		const heading = screen.getByRole('heading', { level: 2 });
		expect(heading).toHaveTextContent('Reindex the corpus');
		expect(
			screen.getByText('Rebuild the embedding index from the current corpus and report drift.')
		).toBeInTheDocument();
	});

	/**
	 * Absent, not empty. A surface with no description shows the title over the
	 * chips rather than a band of nothing — the absent-not-greyed rule this feature
	 * follows everywhere.
	 */
	it('renders no description row when the surface has none', async () => {
		const { container } = render(TaskPanelDrawer, {
			props: { task: TASK, title: 'Reindex the corpus', description: null, now: NOW }
		});

		expect(container.querySelector('.task-panel__description')).toBeNull();
	});

	/**
	 * Row 4. Derived from the model rather than passed in, so all seven surfaces get
	 * it without wiring — see `headerChips`, which owns the choice of which two.
	 */
	it('renders the status as chips, derived from the task rather than passed in', async () => {
		render(TaskPanelDrawer, { props: { task: TASK, title: 'Reindex', now: NOW } });

		expect(screen.getByText('Running')).toBeInTheDocument();
	});

	/**
	 * **The chips must not be live regions.** The verdict line is the panel's one
	 * `status` region and already reads the state as a sentence; a chip announcing
	 * the same change means the reader hears it twice, in an order nothing controls.
	 * This is how the row first showed up — as `getByRole('status')` finding two
	 * elements in a panel documented to have one.
	 */
	it('leaves the verdict as the panel’s only live region', async () => {
		render(TaskPanelDrawer, { props: { task: TASK, title: 'Reindex', now: NOW } });

		const live = screen.getAllByRole('status');
		expect(live).toHaveLength(1);
		expect(live[0]).toHaveClass('verdict');
	});

	it('renders the surface\'s own actions in the header', async () => {
		render(TaskPanelDrawerHarness, {
			props: { task: TASK, title: 'Reindex the corpus', now: NOW, actionLabel: 'Retry synthesis' }
		});

		// The actions have a row of their own now, above the title — so the assertion
		// is that they are in the header rather than beside the heading.
		const header = screen.getByRole('heading', { level: 2 }).closest('header') as HTMLElement;
		expect(header).toContainElement(screen.getByRole('button', { name: 'Retry synthesis' }));
	});

	/**
	 * **Row 1: the controls, on a row of their own, close control included.** The
	 * header used to mix them with the title, which is why the title competed with
	 * whatever the surface slotted beside it.
	 */
	it('puts the surface’s actions and the close control on one row above the title', async () => {
		const { container } = render(TaskPanelDrawerHarness, {
			props: { task: TASK, title: 'Reindex', now: NOW, actionLabel: 'Retry synthesis' }
		});

		const actions = container.querySelector('.task-panel__actions') as HTMLElement;
		expect(actions).toContainElement(screen.getByRole('button', { name: 'Retry synthesis' }));
		expect(actions).toContainElement(screen.getByRole('button', { name: 'Close task panel' }));

		// And the title is a *later* sibling of that row, not inside it.
		const heading = screen.getByRole('heading', { level: 2 });
		expect(actions.contains(heading)).toBe(false);
		expect(actions.compareDocumentPosition(heading) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
	});
});

describe('TaskPanelDrawer condenses its header on scroll', () => {
	/**
	 * The listener is on the body and the class lands on the header, which is its
	 * **sibling** — a header inside the scroll container would move with the content
	 * it is trying to stay above. Pinned structurally, because the mechanism is
	 * invisible from the class alone.
	 */
	it('keeps the header outside the box that scrolls', async () => {
		const { container } = render(TaskPanelDrawer, {
			props: { task: TASK, title: 'Reindex', description: 'A brief.', now: NOW }
		});

		const header = container.querySelector('.task-panel__header') as HTMLElement;
		const body = container.querySelector('.task-panel__body') as HTMLElement;
		expect(body.contains(header)).toBe(false);
		expect(header.parentElement).toBe(body.parentElement);
	});

	/**
	 * **The oscillation guard, end to end.** Condensing removes the description row,
	 * which shortens the content and lets scroll anchoring pull the body back up —
	 * often across the very line that fired. The middle step here is the regression
	 * this hysteresis exists for: at 40px, below the 72px that condensed it, the
	 * header must still be condensed.
	 */
	it('condenses at the threshold, stays condensed above the floor, and expands at the top', async () => {
		const { container } = render(TaskPanelDrawer, {
			props: { task: TASK, title: 'Reindex', description: 'A brief.', now: NOW }
		});

		const header = container.querySelector('.task-panel__header') as HTMLElement;
		const body = container.querySelector('.task-panel__body') as HTMLElement;
		const scrollTo = async (value: number) => {
			Object.defineProperty(body, 'scrollTop', { configurable: true, value });
			await fireEvent.scroll(body);
		};

		expect(header).not.toHaveClass('task-panel__header--condensed');
		expect(screen.getByText('A brief.')).toBeInTheDocument();

		await scrollTo(72);
		expect(header).toHaveClass('task-panel__header--condensed');
		// The description is gone from the document, not merely hidden.
		expect(screen.queryByText('A brief.')).not.toBeInTheDocument();

		await scrollTo(40);
		expect(header).toHaveClass('task-panel__header--condensed');

		await scrollTo(12);
		expect(header).not.toHaveClass('task-panel__header--condensed');
		expect(screen.getByText('A brief.')).toBeInTheDocument();
	});

	/**
	 * A different task is a different header. Without the reset, opening a second
	 * task from a scrolled panel lands the reader on a condensed header over a body
	 * that is back at the top, with the description missing for no reason they can
	 * see.
	 */
	it('expands again when the drawer is given another task, but not on a poll of the same one', async () => {
		const props = { task: TASK, title: 'Reindex', description: 'A brief.', now: NOW };
		const { container, rerender } = render(TaskPanelDrawer, { props });

		const header = container.querySelector('.task-panel__header') as HTMLElement;
		const body = container.querySelector('.task-panel__body') as HTMLElement;
		Object.defineProperty(body, 'scrollTop', { configurable: true, value: 200 });
		await fireEvent.scroll(body);
		expect(header).toHaveClass('task-panel__header--condensed');

		// **A poll first.** A new object for the same task arrives several times a
		// minute; a reset keyed on the prop rather than on the identity would pop the
		// header open under a reader who had scrolled, over and over.
		await rerender({ ...props, task: { ...TASK, elapsedMs: 200_000 }, now: NOW + 8_000 });
		expect(header).toHaveClass('task-panel__header--condensed');

		await rerender({ ...props, task: { ...TASK, id: 'task_beta' } });
		expect(header).not.toHaveClass('task-panel__header--condensed');
	});
});

describe('TaskPanelDrawer moves a task to another thread', () => {
	/**
	 * **Absent rather than defaulted.** `TaskPanelModel` carries no thread, so the
	 * drawer cannot derive one — a surface that does not know passes nothing and gets
	 * no control, rather than one that would move the task to `general`. The two
	 * internal surfaces are in exactly that position.
	 */
	it('offers no mover when the surface cannot say which thread the task is in', async () => {
		render(TaskPanelDrawer, { props: { task: TASK, title: 'Reindex', now: NOW } });

		expect(screen.queryByRole('combobox', { name: 'Move task to thread' })).not.toBeInTheDocument();
	});

	/**
	 * One thread is not a choice, and a select over it would promise one. The store
	 * is empty in this environment, so this is also the honest default state for a
	 * surface whose threads have not loaded yet.
	 */
	it('offers no mover when there is nowhere to move the task to', async () => {
		render(TaskPanelDrawer, {
			props: { task: TASK, title: 'Reindex', threadId: 'general', now: NOW }
		});

		expect(screen.queryByRole('combobox', { name: 'Move task to thread' })).not.toBeInTheDocument();
	});
});

describe('TaskPanelDrawer closes', () => {
	it('asks to close on the close control and on the scrim', async () => {
		const { container } = render(TaskPanelDrawerHarness, {
			props: { task: TASK, title: 'Reindex the corpus', now: NOW }
		});

		await fireEvent.click(screen.getByRole('button', { name: 'Close task panel' }));
		expect(screen.getByTestId('drawer-closes')).toHaveTextContent('1');

		const backdrop = container.querySelector('.task-panel-backdrop') as HTMLElement;
		await fireEvent.click(backdrop);
		expect(screen.getByTestId('drawer-closes')).toHaveTextContent('2');

		// A click inside the drawer is not a click on the scrim.
		await fireEvent.click(screen.getByRole('dialog', { name: 'Task panel' }));
		expect(screen.getByTestId('drawer-closes')).toHaveTextContent('2');
	});

	/**
	 * Escape closes the drawer — once, from the shell, for every surface. The
	 * surfaces keep only the ordering: `closeOnEscape` is how one says a layer of
	 * its own is open over the drawer, so a keystroke meant for a menu does not
	 * also lose the reader's place.
	 */
	it('closes on Escape, unless the surface says a layer of its own is open', async () => {
		const { rerender } = render(TaskPanelDrawerHarness, {
			props: { task: TASK, title: 'Reindex the corpus', now: NOW, closeOnEscape: false }
		});

		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(screen.getByTestId('drawer-closes')).toHaveTextContent('0');

		await rerender({ task: TASK, title: 'Reindex the corpus', now: NOW, closeOnEscape: true });
		await fireEvent.keyDown(window, { key: 'Escape' });
		expect(screen.getByTestId('drawer-closes')).toHaveTextContent('1');

		// Only Escape. Any other key reaching the same listener would close it too.
		await fireEvent.keyDown(window, { key: 'Enter' });
		expect(screen.getByTestId('drawer-closes')).toHaveTextContent('1');
	});
});
