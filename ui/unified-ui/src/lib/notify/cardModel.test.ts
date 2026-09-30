import { describe, it, expect } from 'vitest';
import { hitlEventToCard } from './cardModel';

describe('hitlEventToCard', () => {
	it('maps a confirmation HitlRequested into an actionable card', () => {
		const card = hitlEventToCard({
			event_type: 'HitlRequested',
			data: {
				correlation_id: 'c1',
				source: 'approval',
				input_type: 'confirmation',
				prompt: 'Send iMessage to Alice?',
				hint: 'Hi Alice',
				input_schema: { confirm_label: 'Send', deny_label: 'Cancel' },
				approval_id: 'c1',
				principal: 'owner',
				workspace: 'default',
				execution_id: 'execution-1'
			}
		});
		expect(card).toMatchObject({
			id: 'c1',
			kind: 'actionable',
			correlationId: 'c1',
			source: 'approval',
			prompt: 'Send iMessage to Alice?',
			inputType: 'confirmation',
			hint: 'Hi Alice'
		});
		expect(card).not.toHaveProperty('hitlTarget');
	});

	it('maps non-confirmation input types to the same actionable launcher', () => {
		const card = hitlEventToCard({
			event_type: 'HitlRequested',
			data: {
				correlation_id: 'c2',
				source: 'clarification',
				input_type: 'text',
				prompt: 'Clarify?',
				principal: 'owner',
				workspace: 'default',
				task_id: 'task-2',
				execution_id: 'planexec-2'
			}
		});
		expect(card).toMatchObject({
			kind: 'actionable',
			correlationId: 'c2',
			inputType: 'text'
		});
	});

	it('returns null for non-HitlRequested', () => {
		expect(
			hitlEventToCard({ event_type: 'HitlResolved', data: { correlation_id: 'c1' } })
		).toBeNull();
	});

	it('returns null when correlation_id is missing', () => {
		expect(
			hitlEventToCard({
				event_type: 'HitlRequested',
				data: { source: 'agentic', input_type: 'confirmation', prompt: 'No id?' }
			})
		).toBeNull();
	});

	it('keeps an unroutable historical event on the ID-only compatibility path', () => {
		const card = hitlEventToCard({
			event_type: 'HitlRequested',
			data: {
				pause_state_id: 'pause-legacy',
				source: 'unknown_source',
				input_type: 'confirmation',
				prompt: 'Approve?'
			}
		});

		expect(card).toMatchObject({
			kind: 'actionable',
			correlationId: 'pause-legacy',
			source: 'unknown_source'
		});
	});

	it('defaults source to agentic and omits hint when absent', () => {
		const card = hitlEventToCard({
			event_type: 'HitlRequested',
			data: { correlation_id: 'c3', input_type: 'text', prompt: 'What next?' }
		});
		expect(card).toMatchObject({ source: 'agentic', inputType: 'text' });
		// `hint` only exists on the actionable variant; the union narrows here.
		expect(card?.kind === 'actionable' ? card.hint : undefined).toBeUndefined();
	});

	it('keeps an envoy guest request (source:user_request) actionable (T17)', () => {
		const card = hitlEventToCard({
			event_type: 'HitlRequested',
			data: {
				correlation_id: 'g1',
				source: 'user_request',
				input_type: 'confirmation',
				prompt: 'A guest is asking to join. Allow?',
				principal: 'owner',
				workspace: 'default'
			}
		});
		expect(card).toMatchObject({
			id: 'g1',
			kind: 'actionable',
			source: 'user_request'
		});
	});
});
