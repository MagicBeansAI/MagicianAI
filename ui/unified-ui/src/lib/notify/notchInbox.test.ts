import { describe, expect, it, beforeEach } from 'vitest';
import {
	inlineKindFor,
	notchItemFromEvent,
	notchItemsForCards,
	answerNotchItem,
	rememberNotchEvent,
	resetNotchMemory,
	valueForNotchAnswer,
	requestForNotch
} from './notchInbox';
import type { NotifyCard } from './cardModel';

const confirmation = {
	event_type: 'HitlRequested',
	data: {
		correlation_id: 'c1',
		source: 'approval',
		input_type: 'confirmation',
		prompt: 'Send the note to Alice?',
		hint: 'Hi Alice',
		input_schema: { confirm_label: 'Send', deny_label: 'Keep' },
		approval_id: 'c1',
		principal: 'owner',
		workspace: 'default',
		execution_id: 'execution-1'
	}
};

describe('side-notch inbox', () => {
	beforeEach(() => resetNotchMemory());

	it('puts a confirmation question and its two answers on the notch', () => {
		const item = notchItemFromEvent(confirmation);
		expect(item).toMatchObject({
			id: 'c1',
			correlationId: 'c1',
			inline: 'confirmation',
			prompt: 'Send the note to Alice?',
			hint: 'Hi Alice',
			confirmLabel: 'Send',
			denyLabel: 'Keep'
		});
	});

	it('numbers choice options and refuses a secret in the notch', () => {
		expect(
			inlineKindFor('choice', [{ id: 'a', label: 'Three tiers' }], false)
		).toBe('choice');
		expect(inlineKindFor('password', [], false)).toBe('open');
		const secret = notchItemFromEvent({
			event_type: 'HitlRequested',
			data: {
				correlation_id: 'secret',
				source: 'agentic',
				input_type: 'text',
				prompt: 'What is the password?',
				input_schema: { sensitive: { kind: 'password' } },
				pause_state_id: 'secret',
				principal: 'owner',
				workspace: 'default',
				execution_id: 'execution-2'
			}
		});
		expect(secret?.inline).toBe('open');
	});

	it('maps a notch confirmation onto the same response the Attention page posts', () => {
		rememberNotchEvent(confirmation);
		const request = requestForNotch('c1');
		expect(request).toBeTruthy();
		expect(
			valueForNotchAnswer(request!, {
				id: 'c1',
				correlationId: 'c1',
				kind: 'confirmation',
				choiceId: 'confirm'
			})
		).toEqual({ type: 'confirmation', confirmed: true });
		expect(
			valueForNotchAnswer(request!, {
				id: 'c1',
				correlationId: 'c1',
				kind: 'confirmation',
				choiceId: 'deny'
			})
		).toEqual({ type: 'confirmation', confirmed: false });
	});

	it('forgets a resolved question so the notch does not keep a dead answer', () => {
		rememberNotchEvent(confirmation);
		const card: NotifyCard = {
			id: 'c1',
			kind: 'actionable',
			correlationId: 'c1',
			source: 'approval',
			inputType: 'confirmation',
			prompt: 'Send the note to Alice?'
		};
		expect(notchItemsForCards([card])).toHaveLength(1);
		rememberNotchEvent({
			event_type: 'HitlResolved',
			data: { correlation_id: 'c1' }
		});
		expect(notchItemsForCards([])).toEqual([]);
		expect(requestForNotch('c1')).toBeUndefined();
	});

	it('remembers a user-request text ask and maps the typed answer', () => {
		const id = '81972ea2-4413-4918-b924-84c5c8960081';
		const event = {
			event_type: 'HitlRequested',
			data: {
				correlation_id: id,
				source: 'user_request',
				input_type: 'text',
				principal: 'anonymous',
				workspace: 'default',
				execution_id: 'd4c46f9a-da6a-4397-b765-d6c1214684bb',
				prompt: 'Murga means chicken. What should the birthday menu include?',
				input_schema: {
					context: 'birthday',
					default_on_timeout: 'timeout',
					input_type: 'text',
					options: [],
					question: 'What should the birthday menu include?',
					request_type: 'need_user_input'
				}
			}
		};
		rememberNotchEvent(event);
		const request = requestForNotch(id);
		expect(request?.source).toBe('user_request');
		expect(request?.input_type).toBe('text');
		expect(request?.identifiers.request_id).toBe(id);
		expect(notchItemFromEvent(event)?.inline).toBe('text');
		expect(
			valueForNotchAnswer(request!, {
				id,
				correlationId: id,
				kind: 'text',
				text: ' biryani '
			})
		).toEqual({ type: 'text', value: 'biryani' });
	});

	it('asks the notch to drop a question it can no longer post', async () => {
		await expect(
			answerNotchItem({
				id: 'gone',
				correlationId: 'gone',
				kind: 'text',
				text: 'hi'
			})
		).resolves.toEqual({
			ok: false,
			drop: true,
			message: 'That question is no longer waiting.'
		});
	});
});
