import { describe, expect, it } from 'vitest';

import type { ChatMessage, ChatMessageContent } from '$lib/stores/chatStore';
import type { HitlOpenTarget } from '$lib/hitl/types';
import {
	dockAskReplyContent,
	escalationCardOptions,
	escalationOneClickResult,
	isTypedAnswerableEscalation,
	pendingEscalationReplyTarget,
	typedEscalationAnswer
} from './escalationReply';
import { buildPersistedChatHitlTarget } from './ChatPanel.svelte';

// The shape a delegated run's `need_user_input(input_type="choice")` card has
// in chat: one generic Respond on the card, the ask's options in its schema.
function choiceContent(overrides: Partial<ChatMessageContent> = {}): ChatMessageContent {
	return {
		type: 'escalation',
		question: 'The notes are drafted. What should I do?',
		escalation_type: 'user_input',
		input_type: 'choice',
		execution_id: 'exec-1',
		pause_state_id: 'pause-1',
		options: [{ id: 'respond', label: 'Respond', requires_input: true }],
		input_schema: {
			type: 'choice',
			options: [
				{ id: 'approve_notes', label: 'Approve notes' },
				{ id: 'approve_all', label: 'Approve everything' },
				{ id: 'change', label: 'Change…' }
			],
			allow_other: false
		},
		...overrides
	};
}

function card(
	id: string,
	content: ChatMessageContent,
	sessionId = 'session-1'
): ChatMessage {
	return {
		id,
		session_id: sessionId,
		direction: 'system',
		created_at: 1,
		content
	} as unknown as ChatMessage;
}

const actionable = (content: ChatMessageContent) => !content.resolved && !content.stale;

describe('escalation card options', () => {
	it('offers a pending agentic choice its own options instead of one Respond', () => {
		expect(escalationCardOptions(choiceContent()).map((option) => option.label)).toEqual([
			'Approve notes',
			'Approve everything',
			'Change…'
		]);
	});

	it('keeps the backend options for a clarification', () => {
		const content = choiceContent({ escalation_type: 'clarification' });
		expect(escalationCardOptions(content).map((option) => option.id)).toEqual(['respond']);
	});

	it('answers a listed option in one click, and leaves Change… to the prompt', () => {
		const content = choiceContent();
		expect(escalationOneClickResult(content, { id: 'approve_notes', label: 'Approve notes' })).toEqual({
			kind: 'choice',
			choiceId: 'approve_notes'
		});
		expect(escalationOneClickResult(content, { id: 'change', label: 'Change…' })).toBeNull();
		expect(escalationOneClickResult(content, { id: 'respond', label: 'Respond', requires_input: true })).toBeNull();
	});

	it('answers a plain confirmation in one click, but not a destructive one', () => {
		const confirmation = choiceContent({
			input_type: 'confirmation',
			escalation_type: 'confirmation',
			options: [
				{ id: 'approve', label: 'Approve' },
				{ id: 'deny', label: 'Deny' }
			],
			input_schema: { type: 'confirmation', destructive: false }
		});
		expect(escalationOneClickResult(confirmation, { id: 'deny', label: 'Deny' })).toEqual({
			kind: 'choice',
			choiceId: 'deny'
		});
		const destructive = { ...confirmation, input_schema: { type: 'confirmation', destructive: true } };
		expect(escalationOneClickResult(destructive, { id: 'approve', label: 'Approve' })).toBeNull();
	});

	describe('confirmations outside the allow-list keep the modal', () => {
		// The shapes the backend publishes for confirmations that are not the
		// agent's own `need_user_input` question.
		const outside: Array<[string, Record<string, unknown>]> = [
			[
				'an agentic tool-action approval',
				{ action_type: 'shell', action_summary: 'Run rm -rf build/', reason: 'Clean the build' }
			],
			['a protected app confirmation', { protected: true, action_type: 'shell' }],
			['a confirmation with no destructive flag', { type: 'confirmation' }],
			[
				'an action approval that also says destructive: false',
				{ type: 'confirmation', destructive: false, action_type: 'pack' }
			]
		];
		for (const [name, inputSchema] of outside) {
			it(`for ${name}`, () => {
				const content = choiceContent({
					input_type: 'confirmation',
					escalation_type: 'confirmation',
					options: [
						{ id: 'approve', label: 'Approve' },
						{ id: 'deny', label: 'Deny' }
					],
					input_schema: inputSchema
				});
				expect(escalationOneClickResult(content, { id: 'approve', label: 'Approve' })).toBeNull();
				expect(typedEscalationAnswer(content, 'yes')).toBeNull();
				expect(isTypedAnswerableEscalation(content)).toBe(false);
				expect(pendingEscalationReplyTarget([card('esc-1', content)], 'session-1', actionable)).toBeNull();
			});
		}
	});

	it('shows a repeated option id once', () => {
		const content = choiceContent({
			input_schema: {
				type: 'choice',
				options: [
					{ id: 'approve_notes', label: 'Approve notes' },
					{ id: 'approve_notes', label: 'Approve notes again' },
					{ id: 'change', label: 'Change…' }
				]
			}
		});
		expect(escalationCardOptions(content).map((option) => option.id)).toEqual(['approve_notes', 'change']);
		const backendDuplicates = choiceContent({
			escalation_type: 'clarification',
			options: [
				{ id: 'respond', label: 'Respond' },
				{ id: 'respond', label: 'Respond' }
			]
		});
		expect(escalationCardOptions(backendDuplicates)).toHaveLength(1);
	});

	it('never answers a user-request ask in one click', () => {
		const content = choiceContent({ request_id: 'request-1' });
		expect(escalationOneClickResult(content, { id: 'approve_notes', label: 'Approve notes' })).toBeNull();
	});
});

describe('typed answers', () => {
	it('matches a choice by its label, case-insensitively', () => {
		expect(typedEscalationAnswer(choiceContent(), '  approve EVERYTHING ')).toEqual({
			kind: 'choice',
			choiceId: 'approve_all'
		});
		expect(typedEscalationAnswer(choiceContent(), 'Approve notes.')).toEqual({
			kind: 'choice',
			choiceId: 'approve_notes'
		});
	});

	it('matches a choice by its id', () => {
		expect(typedEscalationAnswer(choiceContent(), 'approve_notes')).toEqual({
			kind: 'choice',
			choiceId: 'approve_notes'
		});
	});

	it('sends unmatched words as the change option with the words as its value', () => {
		expect(typedEscalationAnswer(choiceContent(), 'Rename the second heading')).toEqual({
			kind: 'choice',
			choiceId: 'change',
			input: 'Rename the second heading'
		});
	});

	it('does not answer a choice with no option that takes free text', () => {
		const content = choiceContent({
			input_schema: {
				type: 'choice',
				options: [
					{ id: 'a', label: 'Plan A' },
					{ id: 'b', label: 'Plan B' }
				]
			}
		});
		expect(typedEscalationAnswer(content, 'something else')).toBeNull();
	});

	it('answers text and guidance asks with the words as typed', () => {
		expect(typedEscalationAnswer(choiceContent({ input_type: 'text' }), 'Use staging')).toEqual({
			kind: 'text',
			value: 'Use staging'
		});
		expect(typedEscalationAnswer(choiceContent({ input_type: 'guidance' }), 'Try the API')).toEqual({
			kind: 'guidance',
			value: 'Try the API'
		});
	});

	it('reads yes and no for a plain confirmation, and nothing else', () => {
		const confirmation = choiceContent({
			input_type: 'confirmation',
			escalation_type: 'confirmation',
			options: [
				{ id: 'approve', label: 'Send it' },
				{ id: 'deny', label: 'Deny' }
			],
			input_schema: { type: 'confirmation', destructive: false }
		});
		expect(typedEscalationAnswer(confirmation, 'Yes')).toEqual({ kind: 'choice', choiceId: 'approve' });
		expect(typedEscalationAnswer(confirmation, 'send it')).toEqual({ kind: 'choice', choiceId: 'approve' });
		expect(typedEscalationAnswer(confirmation, 'no')).toEqual({ kind: 'choice', choiceId: 'deny' });
		expect(typedEscalationAnswer(confirmation, 'maybe later')).toBeNull();
	});

	it('never takes typed text for a protected app ask', () => {
		const content = choiceContent({
			input_type: 'text',
			input_schema: { protected: true, input_type: 'text' }
		});
		expect(typedEscalationAnswer(content, 'Use staging')).toBeNull();
		const guidance = choiceContent({
			input_type: 'guidance',
			input_schema: { protected: true, input_type: 'guidance' }
		});
		expect(typedEscalationAnswer(guidance, 'Try the API')).toBeNull();
	});

	it('matches a multi-choice label that holds an "and" before splitting', () => {
		const content = choiceContent({
			input_type: 'multi_choice',
			input_schema: {
				type: 'multi_choice',
				options: [
					{ id: 'notes_slides', label: 'Notes and slides' },
					{ id: 'notes', label: 'Notes' },
					{ id: 'slides', label: 'Slides' }
				]
			}
		});
		expect(typedEscalationAnswer(content, 'notes and slides')).toEqual({
			kind: 'multi_choice',
			choiceIds: ['notes_slides']
		});
		expect(typedEscalationAnswer(content, 'Notes, Slides')).toEqual({
			kind: 'multi_choice',
			choiceIds: ['notes', 'slides']
		});
		expect(typedEscalationAnswer(content, 'notes, video')).toBeNull();
	});

	it('never takes typed text for an ask that collects a secret', () => {
		const content = choiceContent({
			input_type: 'text',
			input_schema: { type: 'text', sensitive: { kind: 'password' } }
		});
		expect(typedEscalationAnswer(content, 'hunter2')).toBeNull();
	});
});

describe('pendingEscalationReplyTarget', () => {
	it('targets the newest open agentic ask in the session', () => {
		const older = card('esc-1', choiceContent({ pause_state_id: 'pause-0' }));
		const newer = card('esc-2', choiceContent());
		expect(pendingEscalationReplyTarget([older, newer], 'session-1', actionable)?.id).toBe('esc-2');
	});

	it('never targets a pause the card shows as resolved or no longer active', () => {
		const resolved = card('esc-1', choiceContent({ resolved: true }));
		const stale = card('esc-2', choiceContent({ pause_state_id: 'pause-2', stale: true }));
		expect(pendingEscalationReplyTarget([resolved, stale], 'session-1', actionable)).toBeNull();
		// The card's own actionability (e.g. its task finished) is honoured too.
		expect(pendingEscalationReplyTarget([card('esc-3', choiceContent())], 'session-1', () => false)).toBeNull();
	});

	it('falls past a resolved card to an older open one', () => {
		const open = card('esc-1', choiceContent({ pause_state_id: 'pause-0' }));
		const resolved = card('esc-2', choiceContent({ resolved: true }));
		expect(pendingEscalationReplyTarget([open, resolved], 'session-1', actionable)?.id).toBe('esc-1');
	});

	it('lets the newest open card decide when it is not typed-answerable', () => {
		const open = card('esc-1', choiceContent({ pause_state_id: 'pause-0' }));
		const grant = card(
			'esc-2',
			choiceContent({ escalation_type: 'tool_authorization', input_type: 'tool_authorization' })
		);
		expect(pendingEscalationReplyTarget([open, grant], 'session-1', actionable)).toBeNull();
	});

	it('ignores other sessions and a pause the owner talked past', () => {
		expect(
			pendingEscalationReplyTarget([card('esc-1', choiceContent(), 'session-2')], 'session-1', actionable)
		).toBeNull();
		expect(
			pendingEscalationReplyTarget([card('esc-1', choiceContent())], 'session-1', actionable, 'pause-1')
		).toBeNull();
	});
});

describe('persisted chat target for a choice', () => {
	it('offers the ask its own options in the prompt', () => {
		const content = choiceContent();
		const target = buildPersistedChatHitlTarget({
			executionId: 'exec-1',
			pauseStateId: 'pause-1',
			escalationType: content.escalation_type,
			inputType: content.input_type,
			question: content.question,
			options: content.options,
			inputSchema: content.input_schema,
			principal: 'anonymous',
			workspace: 'default'
		});
		expect(target?.input_type).toBe('choice');
		expect(target?.input_schema?.options?.map((option) => option.id)).toEqual([
			'approve_notes',
			'approve_all',
			'change'
		]);
	});
});

describe('dockAskReplyContent', () => {
	function dockAsk(overrides: Partial<HitlOpenTarget> = {}): HitlOpenTarget {
		return {
			id: 'pause-9',
			source: 'agentic',
			input_type: 'choice',
			prompt: 'Approve the goal tree?',
			hint: '## Tree\n- G1',
			input_schema: {
				type: 'choice',
				options: [
					{ id: 'approve_notes', label: 'Approve notes' },
					{ id: 'change', label: 'Change…' }
				]
			},
			identifiers: { pause_state_id: 'pause-9' },
			scope: { execution_id: 'exec-9' },
			...overrides
		} as HitlOpenTarget;
	}

	it('turns an agentic dock choice into an answerable card shape', () => {
		const content = dockAskReplyContent(dockAsk());
		expect(content).toMatchObject({
			type: 'escalation',
			execution_id: 'exec-9',
			pause_state_id: 'pause-9',
			escalation_type: 'user_input',
			input_type: 'choice',
			question: 'Approve the goal tree?',
			hint: '## Tree\n- G1'
		});
		expect(typedEscalationAnswer(content!, 'approve notes')).toEqual({
			kind: 'choice',
			choiceId: 'approve_notes'
		});
	});

	it('leaves non-agentic asks, grants and asks without an execution to their own surfaces', () => {
		expect(dockAskReplyContent(null)).toBeNull();
		expect(dockAskReplyContent(dockAsk({ source: 'plan_approval' as HitlOpenTarget['source'] }))).toBeNull();
		expect(dockAskReplyContent(dockAsk({ scope: {} }))).toBeNull();
		expect(
			dockAskReplyContent(
				dockAsk({
					input_type: 'confirmation',
					input_schema: { type: 'confirmation', escalation_trigger: 'tool_authorization' }
				} as Partial<HitlOpenTarget>)
			)
		).toBeNull();
	});

	it('keeps a tool-action confirmation out of typed answers', () => {
		expect(
			dockAskReplyContent(
				dockAsk({
					input_type: 'confirmation',
					input_schema: { type: 'confirmation', action_type: 'write_file' }
				} as Partial<HitlOpenTarget>)
			)
		).toBeNull();
	});
});

