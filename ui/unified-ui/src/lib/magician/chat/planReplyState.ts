import type { PlanComposerMode } from '$lib/stores/planModeStore';

export interface PlanReplyTarget {
	taskId: string;
	taskTitle: string;
	questionId: string;
	questionText: string;
	threadId: string;
}

export interface PlanReplyIntent<Attachment> extends PlanReplyTarget {
	returnMode: PlanComposerMode;
	returnDraft: string;
	returnAttachments: Attachment[];
}

export interface PlanReplyComposerState<Attachment> {
	mode: PlanComposerMode;
	draft: string;
	attachments: Attachment[];
	intent: PlanReplyIntent<Attachment> | null;
}

export type BeginPlanReplyResult<Attachment> =
	| { ok: true; state: PlanReplyComposerState<Attachment> }
	| { ok: false; reason: 'reply_in_progress' };

export function beginPlanReply<Attachment>(
	state: PlanReplyComposerState<Attachment>,
	target: PlanReplyTarget
): BeginPlanReplyResult<Attachment> {
	const current = state.intent;
	if (
		current
		&& current.taskId === target.taskId
		&& current.questionId === target.questionId
	) {
		return { ok: true, state };
	}

	if (current && (state.draft.trim().length > 0 || state.attachments.length > 0)) {
		return { ok: false, reason: 'reply_in_progress' };
	}

	const intent: PlanReplyIntent<Attachment> = current
		? {
				...target,
				returnMode: current.returnMode,
				returnDraft: current.returnDraft,
				returnAttachments: [...current.returnAttachments]
			}
		: {
				...target,
				returnMode: state.mode,
				returnDraft: state.draft,
				returnAttachments: [...state.attachments]
			};

	return {
		ok: true,
		state: {
			mode: 'plan',
			draft: '',
			attachments: [],
			intent
		}
	};
}

export function finishPlanReply<Attachment>(
	state: PlanReplyComposerState<Attachment>,
	modeOverride?: PlanComposerMode
): PlanReplyComposerState<Attachment> {
	if (!state.intent) return state;
	return {
		mode: modeOverride ?? state.intent.returnMode,
		draft: state.intent.returnDraft,
		attachments: [...state.intent.returnAttachments],
		intent: null
	};
}
