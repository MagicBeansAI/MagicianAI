import type { AttentionFeedbackReceipt } from '$lib/channel/channelFollowUpLearning';
import type { AttentionFeedbackAttribution } from './attentionBandit';
import {
	acknowledgeChannelFollowUp,
	approveChannelFollowUp,
	dismissChannelFollowUp,
	dismissChannelFollowUpWithReason,
	snoozeChannelFollowUp,
	usefulChannelFollowUp,
	type ChannelFollowUpActionResult
} from '$lib/stores/channelNeedsYouStore';
import {
	postResurfacingAction,
	type ResurfacingActionResult
} from '$lib/today/resurfacingQueries';
import type {
	CanonicalAttentionItem,
	CanonicalAttentionOriginAction
} from './canonicalAttentionProjection';

export type CanonicalAttentionActionResult =
	| {
			ok: true;
			resolved: true;
			message: string;
			feedbackReceipt: AttentionFeedbackReceipt | null;
	  }
	| { ok: false; error: string };

export interface CanonicalAttentionActionDependencies {
	approveFollowUp(id: string, attribution?: AttentionFeedbackAttribution | null): Promise<ChannelFollowUpActionResult>;
	acknowledgeFollowUp(id: string, attribution?: AttentionFeedbackAttribution | null): Promise<ChannelFollowUpActionResult>;
	usefulFollowUp(id: string, attribution?: AttentionFeedbackAttribution | null): Promise<ChannelFollowUpActionResult>;
	dismissFollowUp(id: string, attribution?: AttentionFeedbackAttribution | null): Promise<ChannelFollowUpActionResult>;
	dismissFollowUpWrongLane(
		id: string,
		attribution?: AttentionFeedbackAttribution | null
	): Promise<ChannelFollowUpActionResult>;
	snoozeFollowUp(id: string, attribution?: AttentionFeedbackAttribution | null): Promise<ChannelFollowUpActionResult>;
	actOnWorth(
		id: string,
		action: 'open' | 'acknowledge' | 'dismiss' | 'owner_work',
		attribution?: AttentionFeedbackAttribution | null
	): Promise<ResurfacingActionResult>;
}

const defaultDependencies: CanonicalAttentionActionDependencies = {
	approveFollowUp: (id, attribution) => approveChannelFollowUp(id, undefined, attribution),
	acknowledgeFollowUp: (id, attribution) => acknowledgeChannelFollowUp(id, attribution),
	usefulFollowUp: (id, attribution) => usefulChannelFollowUp(id, attribution),
	dismissFollowUp: (id, attribution) => dismissChannelFollowUp(id, attribution),
	dismissFollowUpWrongLane: (id, attribution) =>
		dismissChannelFollowUpWithReason(id, 'wrong_classification', attribution),
	snoozeFollowUp: (id, attribution) => snoozeChannelFollowUp(id, attribution),
	actOnWorth: (id, action, attribution) => postResurfacingAction(id, action, undefined, attribution)
};

function actionBelongsToItem(
	item: CanonicalAttentionItem,
	action: CanonicalAttentionOriginAction
): boolean {
	return item.actions.some(
		(candidate) =>
			candidate.id === action.id &&
			candidate.kind === action.kind &&
			candidate.label === action.label &&
			candidate.method === action.method &&
			candidate.href === action.href &&
			candidate.requires_confirmation === action.requires_confirmation
	);
}

function successMessage(item: CanonicalAttentionItem, action: CanonicalAttentionOriginAction): string {
	if (action.kind === 'useful') return 'Marked useful';
	if (action.kind === 'approve') return 'Follow-up started';
	if (action.kind === 'acknowledge') return 'Acknowledged';
	if (action.kind === 'dismiss') return 'Dismissed';
	if (action.kind === 'owner_work') return 'Moved toward For you — similar cards will follow';
	if (action.kind === 'wrong_lane') return "Dismissed — shouldn't have been flagged";
	return 'Snoozed';
}

/**
 * Dispatch a canonical card through its origin lifecycle. The destination lane
 * is intentionally never inspected, and descriptor hrefs are not executed as
 * generic requests. That keeps a rerouted Worth card on Worth endpoints and a
 * rerouted Follow-up card on annotation endpoints.
 */
export async function executeCanonicalAttentionAction(
	item: CanonicalAttentionItem,
	action: CanonicalAttentionOriginAction,
	dependencies: CanonicalAttentionActionDependencies = defaultDependencies,
	attribution: AttentionFeedbackAttribution | null = null
): Promise<CanonicalAttentionActionResult> {
	if (!actionBelongsToItem(item, action)) {
		return { ok: false, error: 'Action does not belong to this projection item.' };
	}
	if (action.method !== 'post' || action.kind === 'open_source') {
		return { ok: false, error: 'Open links must be navigated, not posted.' };
	}

	if (item.origin_lane === 'follow_up') {
		const id = item.origin.annotation_id;
		let result: ChannelFollowUpActionResult;
		if (action.kind === 'approve') result = attribution
			? await dependencies.approveFollowUp(id, attribution) : await dependencies.approveFollowUp(id);
		else if (action.kind === 'useful') result = attribution
			? await dependencies.usefulFollowUp(id, attribution) : await dependencies.usefulFollowUp(id);
		else if (action.kind === 'acknowledge') result = attribution
			? await dependencies.acknowledgeFollowUp(id, attribution) : await dependencies.acknowledgeFollowUp(id);
		else if (action.kind === 'dismiss') result = attribution
			? await dependencies.dismissFollowUp(id, attribution) : await dependencies.dismissFollowUp(id);
		else if (action.kind === 'wrong_lane') result = attribution
			? await dependencies.dismissFollowUpWrongLane(id, attribution)
			: await dependencies.dismissFollowUpWrongLane(id);
		else if (action.kind === 'snooze') result = attribution
			? await dependencies.snoozeFollowUp(id, attribution) : await dependencies.snoozeFollowUp(id);
		else return { ok: false, error: 'Unsupported Follow-up action.' };
		return result.ok
			? {
					ok: true,
					resolved: true,
					message: successMessage(item, action),
					feedbackReceipt: result.feedbackReceipt
			  }
			: result;
	}

	if (
		action.kind !== 'useful' &&
		action.kind !== 'acknowledge' &&
		action.kind !== 'dismiss' &&
		action.kind !== 'owner_work'
	) {
		return { ok: false, error: 'Unsupported Worth-a-look action.' };
	}
	const worthAction = action.kind === 'useful' ? 'open' : action.kind;
	const result = attribution
		? await dependencies.actOnWorth(item.origin.candidate_id, worthAction, attribution)
		: await dependencies.actOnWorth(item.origin.candidate_id, worthAction);
	return result.ok
		? {
				ok: true,
				resolved: true,
				message: successMessage(item, action),
				feedbackReceipt: result.feedbackReceipt
		  }
		: result;
}
