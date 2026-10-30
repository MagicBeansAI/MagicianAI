/**
 * An escalation card is an ask, so it always carries a way to answer. A card
 * whose backend listed options offers them; one with none used to render no
 * control at all, and the ask could only be answered from another surface.
 */
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ChatMessage } from '$lib/stores/chatStore';
import EscalationCard from './EscalationCard.svelte';
import { ESCALATION_OPEN_PROMPT_ID } from '../escalationRouting';

afterEach(() => cleanup());

function escalation(options: Array<{ id: string; label: string }>): ChatMessage {
	return {
		id: 'msg-1',
		session_id: 'session-1',
		direction: 'assistant',
		created_at: 1,
		content: {
			type: 'escalation',
			question: 'Which account should I use?',
			options,
			execution_id: 'exec-1',
			pause_state_id: 'pause-1',
			escalation_type: 'clarification',
			input_type: 'text'
		}
	} as unknown as ChatMessage;
}

describe('EscalationCard', () => {
	it('offers an Answer control when the escalation lists no options', async () => {
		const onRespond = vi.fn();
		render(EscalationCard, {
			props: { message: escalation([]), statusLabel: 'Needs input', showActions: true, onRespond }
		});

		await fireEvent.click(screen.getByRole('button', { name: 'Answer' }));
		expect(onRespond).toHaveBeenCalledWith(
			'exec-1',
			'pause-1',
			{ id: ESCALATION_OPEN_PROMPT_ID, label: 'Answer' },
			'clarification',
			undefined,
			'text'
		);
	});

	it('offers the listed options, and no extra Answer, when there are some', () => {
		render(EscalationCard, {
			props: {
				message: escalation([
					{ id: 'approve', label: 'Approve' },
					{ id: 'deny', label: 'Deny' }
				]),
				statusLabel: 'Needs input',
				showActions: true
			}
		});

		expect(screen.getByRole('button', { name: 'Approve' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Deny' })).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Answer' })).not.toBeInTheDocument();
	});

	it('offers a pending agentic choice its own options, and reports the clicked one', async () => {
		const onRespond = vi.fn();
		const message = {
			...escalation([{ id: 'respond', label: 'Respond' }]),
			content: {
				type: 'escalation',
				question: 'The notes are drafted. What should I do?',
				options: [{ id: 'respond', label: 'Respond', requires_input: true }],
				execution_id: 'exec-1',
				pause_state_id: 'pause-1',
				escalation_type: 'user_input',
				input_type: 'choice',
				input_schema: {
					type: 'choice',
					options: [
						{ id: 'approve_notes', label: 'Approve notes' },
						{ id: 'approve_all', label: 'Approve everything' },
						{ id: 'change', label: 'Change…' }
					]
				}
			}
		} as unknown as ChatMessage;
		render(EscalationCard, {
			props: { message, statusLabel: 'Action Required', showActions: true, onRespond }
		});

		expect(screen.queryByRole('button', { name: 'Respond' })).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Approve everything' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Change…' })).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Approve notes' }));
		expect(onRespond).toHaveBeenCalledWith(
			'exec-1',
			'pause-1',
			{ id: 'approve_notes', label: 'Approve notes' },
			'user_input',
			undefined,
			'choice'
		);
	});

	it('shows why an answer did not land and keeps the options', () => {
		render(EscalationCard, {
			props: {
				message: escalation([{ id: 'approve', label: 'Approve' }]),
				statusLabel: 'Action Required',
				showActions: true,
				error: 'The run is not waiting for this answer.'
			}
		});

		expect(screen.getByText('The run is not waiting for this answer.')).toBeInTheDocument();
		// Announced once, by the error toast, not again by the card.
		expect(screen.queryByRole('alert')).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Approve' })).toBeInTheDocument();
	});

	it('renders a repeated option id once', () => {
		render(EscalationCard, {
			props: {
				message: escalation([
					{ id: 'approve', label: 'Approve' },
					{ id: 'approve', label: 'Approve again' },
					{ id: 'deny', label: 'Deny' }
				]),
				statusLabel: 'Action Required',
				showActions: true
			}
		});

		expect(screen.getByRole('button', { name: 'Approve' })).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Approve again' })).not.toBeInTheDocument();
		expect(screen.getAllByRole('button')).toHaveLength(2);
	});
});

describe('EscalationCard details', () => {
	function withHint(hint: string): ChatMessage {
		const message = escalation([{ id: 'approve_notes', label: 'Approve notes' }]);
		(message.content as { hint?: string }).hint = hint;
		return message;
	}

	it('shows a proposal sent as the hint under Details, as Markdown', () => {
		render(EscalationCard, {
			props: {
				message: withHint('## Goal tree\n\n- **Raise a round** (internal)\n- Accelerator path'),
				statusLabel: 'Needs input',
				showActions: true
			}
		});

		const details = screen.getByTestId('hitl-details');
		expect(details.querySelector('summary')?.textContent).toBe('Details');
		expect(details.querySelector('h2')?.textContent).toBe('Goal tree');
		expect(details.querySelector('strong')?.textContent).toBe('Raise a round');
	});

	it('keeps a one-line hint as a plain line', () => {
		render(EscalationCard, {
			props: { message: withHint('For example, billing'), statusLabel: 'Needs input', showActions: true }
		});

		expect(screen.queryByTestId('hitl-details')).toBeNull();
		expect(screen.getByText('For example, billing')).toBeInTheDocument();
	});
});
