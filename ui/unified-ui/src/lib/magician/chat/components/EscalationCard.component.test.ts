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
});
