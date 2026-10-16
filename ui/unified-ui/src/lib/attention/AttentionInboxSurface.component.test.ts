import { render, screen, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import AttentionInboxHarness from '../../test/fixtures/AttentionInboxHarness.svelte';
import type { AttentionDisplayRow } from './model';

const rows: AttentionDisplayRow[] = [
	{
		key: 'approval-1',
		source: 'approval',
		prompt: 'Approve deployment to production',
		hint: 'Release 42',
		scope: { task_id: 'task-1' },
		at: Date.now(),
		correlation_id: 'approval-1',
		origin: 'feed',
		request: null
	},
	{
		key: 'failed-1',
		source: 'agentic',
		prompt: 'Build failed after dependency update',
		scope: { task_id: 'task-2' },
		at: Date.now() - 60_000,
		correlation_id: 'failed-1',
		origin: 'feed',
		request: null,
		failed: true,
		feed_item_id: 'feed-failed-1'
	},
	{
		key: 'skill-1',
		source: 'approval',
		prompt: 'Review generated deployment skill',
		scope: {},
		at: Date.now() - 120_000,
		correlation_id: 'skill-1',
		origin: 'feed',
		request: null,
		skillEvolution: {
			gate: 'proposal_review',
			action: 'approve',
			actionEnabled: true,
			candidateId: 'candidate-1'
		}
	}
];

describe('AttentionInboxSurface interactions', () => {
	it('filters visible rows using the user search field', async () => {
		const user = userEvent.setup();
		render(AttentionInboxHarness, { rows });

		await user.type(screen.getByRole('searchbox'), 'dependency');

		expect(screen.getByTestId('attention-search')).toHaveTextContent('dependency');
		expect(screen.getByText('Build failed after dependency update')).toBeInTheDocument();
		expect(screen.queryByText('Approve deployment to production')).not.toBeInTheDocument();
	});

	it('switches source filters and preserves the matching rows', async () => {
		const user = userEvent.setup();
		render(AttentionInboxHarness, { rows });

		await user.click(screen.getByRole('button', { name: 'Failed 1' }));

		expect(screen.getByTestId('attention-filter')).toHaveTextContent('failed');
		expect(screen.getByText('Build failed after dependency update')).toBeInTheDocument();
		expect(screen.queryByText('Approve deployment to production')).not.toBeInTheDocument();
	});

	it('activates the exact row selected by the user', async () => {
		const user = userEvent.setup();
		render(AttentionInboxHarness, { rows });

		const row = screen.getByText('Approve deployment to production').closest('li');
		expect(row).not.toBeNull();
		await user.click(within(row as HTMLElement).getByRole('button'));

		expect(screen.getByTestId('attention-action')).toHaveTextContent('activate:approval-1');
	});

	it('dispatches skill approval and rejection independently from row activation', async () => {
		const user = userEvent.setup();
		render(AttentionInboxHarness, { rows });
		const row = screen.getByText('Review generated deployment skill').closest('li');
		expect(row).not.toBeNull();

		await user.click(within(row as HTMLElement).getByRole('button', { name: 'Approve' }));
		expect(screen.getByTestId('attention-action')).toHaveTextContent('skill:skill-1:approve');
		await user.click(within(row as HTMLElement).getByRole('button', { name: 'Reject' }));
		expect(screen.getByTestId('attention-action')).toHaveTextContent('skill:skill-1:reject');
	});

	it('renders a fixed skeleton set before the first page arrives', () => {
		render(AttentionInboxHarness, { rows: [], initialLoading: true });

		const loadingList = screen.getByRole('list', { name: 'Loading attention items' });
		expect(within(loadingList).getAllByRole('listitem', { hidden: true })).toHaveLength(6);
		expect(screen.queryByText('Inbox zero.')).not.toBeInTheDocument();
	});

	it('requests the next server frontier and blocks duplicate requests while busy', async () => {
		const user = userEvent.setup();
		const { rerender } = render(AttentionInboxHarness, { rows, canShowMore: true });
		await user.click(screen.getByRole('button', { name: 'Show more' }));
		expect(screen.getByTestId('attention-action')).toHaveTextContent('showmore');

		await rerender({ showMoreBusy: true });
		expect(screen.getByRole('button', { name: /Loading/ })).toBeDisabled();
	});
});
