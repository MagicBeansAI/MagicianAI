import { describe, expect, it } from 'vitest';
import { beginPlanReply, finishPlanReply, type PlanReplyComposerState } from './planReplyState';

interface Attachment {
	id: string;
}

const target = {
	taskId: 'task-1',
	taskTitle: 'Ship the site',
	questionId: 'question-1',
	questionText: 'Which domain should be used?',
	threadId: 'website'
};

function state(
	overrides: Partial<PlanReplyComposerState<Attachment>> = {}
): PlanReplyComposerState<Attachment> {
	return {
		mode: 'ask',
		draft: 'ordinary chat draft',
		attachments: [{ id: 'attachment-1' }],
		intent: null,
		...overrides
	};
}

describe('plan reply composer state', () => {
	it('stashes the ordinary draft and restores it after the planning answer', () => {
		const entered = beginPlanReply(state(), target);
		expect(entered.ok).toBe(true);
		if (!entered.ok) return;

		expect(entered.state.mode).toBe('plan');
		expect(entered.state.draft).toBe('');
		expect(entered.state.attachments).toEqual([]);
		expect(entered.state.intent?.questionId).toBe('question-1');

		const restored = finishPlanReply({
			...entered.state,
			draft: 'Use example.com'
		});
		expect(restored).toEqual(state());
	});

	it('does not silently retarget an answer already being composed', () => {
		const entered = beginPlanReply(state({ draft: '', attachments: [] }), target);
		expect(entered.ok).toBe(true);
		if (!entered.ok) return;

		const retargeted = beginPlanReply(
			{ ...entered.state, draft: 'Use example.com' },
			{ ...target, questionId: 'question-2', questionText: 'Which region?' }
		);
		expect(retargeted).toEqual({ ok: false, reason: 'reply_in_progress' });
	});

	it('allows an empty reply to switch questions without losing the original draft', () => {
		const entered = beginPlanReply(state(), target);
		expect(entered.ok).toBe(true);
		if (!entered.ok) return;

		const retargeted = beginPlanReply(
			entered.state,
			{ ...target, questionId: 'question-2', questionText: 'Which region?' }
		);
		expect(retargeted.ok).toBe(true);
		if (!retargeted.ok) return;

		expect(retargeted.state.intent?.questionId).toBe('question-2');
		expect(finishPlanReply(retargeted.state)).toEqual(state());
	});

	it('honors an explicit mode choice when cancelling', () => {
		const entered = beginPlanReply(state({ mode: 'plan' }), target);
		expect(entered.ok).toBe(true);
		if (!entered.ok) return;

		expect(finishPlanReply(entered.state, 'ask').mode).toBe('ask');
	});
});
