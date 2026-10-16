import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import TaskFilterToolbarHarness from '../../../test/fixtures/TaskFilterToolbarHarness.svelte';
import { computeStatusChips } from './TaskFilterToolbar.svelte';

describe('TaskFilterToolbar interactions', () => {
	it('reports cancelled work separately from actual failures', () => {
		render(TaskFilterToolbarHarness, {
			statusChips: computeStatusChips([
				{ status: 'failed' },
				{ status: 'cancelled' },
				{ status: 'cancelled' },
				{ status: 'completed' }
			])
		});

		const ledger = screen.getByRole('note', { name: 'Task counts by status' });
		expect(within(ledger).getByTitle('1 failed (status count)')).toHaveTextContent('1 Failed');
		expect(within(ledger).getByTitle('2 cancelled (status count)')).toHaveTextContent('2 Cancelled');
		expect(within(ledger).queryByTitle('3 failed (status count)')).not.toBeInTheDocument();
	});

	it('selects preset filters and clears an active tag', async () => {
		const user = userEvent.setup();
		render(TaskFilterToolbarHarness, { tags: ['ops'] });

		await user.click(screen.getByRole('button', { name: '#ops' }));
		expect(screen.getByTestId('task-filter-state')).toHaveTextContent('all:ops');

		await user.click(screen.getByRole('button', { name: 'Running' }));
		expect(screen.getByTestId('task-filter-state')).toHaveTextContent('running:');
	});

	it('toggles the selected tag off when clicked again', async () => {
		const user = userEvent.setup();
		render(TaskFilterToolbarHarness, { tags: ['qa'] });
		const tag = screen.getByRole('button', { name: '#qa' });

		await user.click(tag);
		await user.click(tag);

		expect(screen.getByTestId('task-filter-state')).toHaveTextContent('all:');
	});

	/**
	 * `null` and `0` are different claims. The server reports `counts` only
	 * alongside the reader's `today=`, and never on the legacy unpaged branch,
	 * so a missing lane is a real state — and a badge reading `0` above a lane
	 * nobody counted is the confident-looking wrong answer this whole change
	 * exists to stop serving.
	 */
	it('badges only the lanes that were actually counted', () => {
		render(TaskFilterToolbarHarness, { counts: { all: 4, inbox: 0, today: null } });

		expect(screen.getByRole('button', { name: 'All 4' })).toBeInTheDocument();
		// Zero is a count, and gets a badge.
		expect(screen.getByRole('button', { name: 'Inbox 0' })).toBeInTheDocument();
		// Reported as absent, and omitted entirely.
		expect(screen.getByRole('button', { name: 'Today' })).toBeInTheDocument();
		// Never mentioned at all — same treatment.
		expect(screen.getByRole('button', { name: 'Overdue' })).toBeInTheDocument();
	});

	it('renders compact tags first and keeps status counts informational', () => {
		render(TaskFilterToolbarHarness, {
			tags: ['follow-up', 'ops', 'qa'],
			totalCount: 9,
			statusChips: [{ key: 'paused', label: 'Paused', count: 2 }]
		});

		const tags = within(screen.getByLabelText('Task tags')).getAllByRole('button');
		expect(tags.map((button) => button.textContent?.trim())).toEqual(['#qa', '#ops', '#follow-up']);
		expect(screen.getByRole('note', { name: 'Task counts by status' })).toHaveTextContent(
			'9 total'
		);
		expect(screen.getByRole('note', { name: 'Task counts by status' })).toHaveTextContent(
			'2 Paused'
		);
	});
});
