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

	it('opens the card when its title is clicked, without requiring the Respond label', async () => {
		const user = userEvent.setup();
		render(AttentionInboxHarness, { rows });

		const row = screen.getByText('Approve deployment to production').closest('li');
		expect(row).not.toBeNull();
		await user.click(screen.getByText('Approve deployment to production'));

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

	describe('one-click answers', () => {
		// A delegated run's `need_user_input(input_type="choice")` pause.
		const choiceRow: AttentionDisplayRow = {
			key: 'pause-7',
			source: 'agentic',
			prompt: 'The notes are drafted. What should I do?',
			scope: { execution_id: 'execution-7' },
			at: Date.now(),
			correlation_id: 'pause-7',
			origin: 'bus',
			request: {
				id: 'pause-7',
				source: 'agentic',
				input_type: 'choice',
				schema: {
					options: [
						{ id: 'approve_notes', label: 'Approve notes' },
						{ id: 'approve_all', label: 'Approve everything' },
						{ id: 'change', label: 'Change…' }
					]
				},
				prompt: 'The notes are drafted. What should I do?',
				scope: { execution_id: 'execution-7' },
				identifiers: { correlation_id: 'pause-7', pause_state_id: 'pause-7' }
			}
		};

		it('offers the options on the row and dispatches the clicked one', async () => {
			const user = userEvent.setup();
			render(AttentionInboxHarness, { rows: [choiceRow], quickChoices: true });
			const answers = screen.getByRole('group', { name: 'Answers: The notes are drafted. What should I do?' });

			await user.click(within(answers).getByRole('button', { name: 'Approve everything' }));
			expect(screen.getByTestId('attention-action')).toHaveTextContent('choose:pause-7:approve_all');
		});

		it('opens the prompt for Change… instead of posting it', async () => {
			const user = userEvent.setup();
			render(AttentionInboxHarness, { rows: [choiceRow], quickChoices: true });

			await user.click(screen.getByRole('button', { name: 'Change…' }));
			expect(screen.getByTestId('attention-action')).toHaveTextContent('activate:pause-7');
		});

		it('marks the pending answer busy, keeping its name, and holds every button', () => {
			render(AttentionInboxHarness, {
				rows: [choiceRow],
				quickChoices: true,
				choosingKey: 'pause-7:approve_notes'
			});
			const answers = screen.getByRole('group', { name: 'Answers: The notes are drafted. What should I do?' });

			const sending = within(answers).getByRole('button', { name: 'Approve notes' });
			expect(sending).toBeDisabled();
			expect(sending).toHaveAttribute('aria-busy', 'true');
			const other = within(answers).getByRole('button', { name: 'Approve everything' });
			expect(other).toBeDisabled();
			expect(other).not.toHaveAttribute('aria-busy');
		});

		it('renders a repeated option id once', () => {
			const duplicated: AttentionDisplayRow = {
				...choiceRow,
				request: {
					...choiceRow.request!,
					schema: {
						options: [
							{ id: 'approve_notes', label: 'Approve notes' },
							{ id: 'approve_notes', label: 'Approve notes again' }
						]
					}
				}
			};
			render(AttentionInboxHarness, { rows: [duplicated], quickChoices: true });
			const answers = screen.getByRole('group', { name: /^Answers:/ });

			expect(within(answers).getAllByRole('button')).toHaveLength(1);
		});

		it('offers no buttons for an escalation-source pause', () => {
			const escalation: AttentionDisplayRow = {
				...choiceRow,
				source: 'escalation',
				request: { ...choiceRow.request!, source: 'escalation' }
			};
			render(AttentionInboxHarness, { rows: [escalation], quickChoices: true });
			expect(screen.queryByRole('group', { name: /^Answers:/ })).not.toBeInTheDocument();
		});

		it('shows no answer buttons unless the host asks for them', () => {
			render(AttentionInboxHarness, { rows: [choiceRow] });
			expect(screen.queryByRole('group', { name: /^Answers:/ })).not.toBeInTheDocument();
		});
	});
});
