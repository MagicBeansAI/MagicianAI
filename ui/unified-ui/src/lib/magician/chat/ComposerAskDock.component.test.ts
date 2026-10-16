import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

const { answerTaskAsk } = vi.hoisted(() => ({
	answerTaskAsk: vi.fn(async () => ({ ok: true as const }))
}));
vi.mock('$lib/magician/tasks/taskAsk', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/magician/tasks/taskAsk')>()),
	answerTaskAsk
}));

import type { HitlOpenTarget } from '$lib/hitl/types';
import ComposerAskDock from './ComposerAskDock.svelte';

afterEach(cleanup);

function choice(id: string, prompt: string): HitlOpenTarget {
	return {
		id,
		source: 'agentic',
		input_type: 'choice',
		prompt,
		input_schema: {
			options: [
				{ id: 'q1', label: 'Q1' },
				{ id: 'q2', label: 'Q2' }
			]
		},
		identifiers: { pause_state_id: id, correlation_id: id },
		scope: { principal: 'anonymous', workspace: 'default', task_id: 'task-1' }
	} as HitlOpenTarget;
}

describe('ComposerAskDock', () => {
	it('shows the question with its answer options above the composer', () => {
		render(ComposerAskDock, { targets: [choice('p1', 'Which quarter?')] });
		expect(screen.getByTestId('composer-ask-dock')).toHaveTextContent('Waiting on you');
		expect(screen.getByText('Which quarter?')).toBeTruthy();
		expect(screen.getByText('Q1')).toBeTruthy();
		expect(screen.queryByText(/of 2/)).toBeNull();
	});

	it('switches between several questions without losing its place', async () => {
		render(ComposerAskDock, {
			targets: [choice('p1', 'Which quarter?'), choice('p2', 'Which region?')]
		});
		expect(screen.getByText('1 of 2')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Next question' }));
		expect(screen.getByText('Which region?')).toBeTruthy();
		expect(screen.getByText('2 of 2')).toBeTruthy();
	});

	it('offers approve/reject and the review link for a Relentless review, answered in place', async () => {
		const review: HitlOpenTarget = {
			id: 'req-review-1',
			source: 'user_request',
			input_type: 'choice',
			prompt: 'Relentless accepted a draft. Approve it?',
			input_schema: {
				request_type: 'relentless.accepted_review',
				options: [
					{ id: 'approve', label: 'Approve draft' },
					{ id: 'reject', label: 'Reject draft' }
				],
				review_href: '/work?view=quality&run=run-1&task=task-r',
				review_label: 'Review draft'
			},
			identifiers: { request_id: 'req-review-1', correlation_id: 'req-review-1' },
			scope: { principal: 'anonymous', workspace: 'default', task_id: 'task-r' }
		};
		render(ComposerAskDock, { targets: [review] });
		expect(screen.getByRole('link', { name: 'Review draft →' })).toHaveAttribute(
			'href',
			'/work?view=quality&run=run-1&task=task-r'
		);
		expect(screen.getByText('Reject draft')).toBeTruthy();
		const submit = screen.getByRole('button', { name: 'Answer' });
		expect(submit).toBeDisabled();
		await fireEvent.click(screen.getByRole('radio', { name: /Approve draft/ }));
		await fireEvent.click(submit);
		await waitFor(() => expect(answerTaskAsk).toHaveBeenCalledTimes(1));
		expect(answerTaskAsk).toHaveBeenCalledWith(
			review,
			expect.objectContaining({ kind: 'choice', choiceId: 'approve' }),
			expect.anything()
		);
	});

	it('renders nothing when nothing is waiting', () => {
		render(ComposerAskDock, { targets: [] });
		expect(screen.queryByTestId('composer-ask-dock')).toBeNull();
	});
});
