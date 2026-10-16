import type { AttentionStoreState } from '$lib/stores/attentionStore';
import {
	attentionFeedLanesForCategory,
	type AttentionCategory,
	type AttentionInboxFeedback
} from './model';
import { attentionFrontiersProven, type AttentionSourceFrontier } from './pagination';

export type AttentionItemLaunchFailure = 'load-error' | 'not-found';

export function compactAttentionFeedback(
	...messages: Array<string | null>
): AttentionInboxFeedback[] {
	return messages
		.filter((message): message is string => Boolean(message))
		.map((message) => ({
			kind: /failed|unavailable|error/i.test(message) ? 'error' : 'notice',
			message
		}));
}

export function attentionFeedCanLoadMore(
	state: AttentionStoreState,
	category: AttentionCategory,
	maxFeedItems: number
): boolean {
	return attentionFeedLanesForCategory(category).some(
		(lane) => state.pages[lane].has_more && state[lane].length < maxFeedItems
	);
}

export function attentionSourceFrontiers(
	state: AttentionStoreState,
	category: AttentionCategory,
	maxFeedItems: number
): AttentionSourceFrontier[] {
	return attentionFeedLanesForCategory(category).map((lane) => ({
		loadedCount: state[lane].length,
		hasMore: state.pages[lane].has_more,
		bufferLimit: maxFeedItems
	}));
}

export function attentionFeedLaneNeedsAdvance(
	state: AttentionStoreState,
	requiredRows: number,
	category: AttentionCategory,
	maxFeedItems: number
): boolean {
	return attentionFeedLanesForCategory(category).some((lane) =>
		!attentionFrontiersProven(
			[
				{
					loadedCount: state[lane].length,
					hasMore: state.pages[lane].has_more,
					bufferLimit: maxFeedItems
				}
			],
			requiredRows
		)
	);
}

export function attentionFrontierSignature(
	state: AttentionStoreState,
	category: AttentionCategory
): string {
	return attentionFeedLanesForCategory(category)
		.flatMap((lane) => [state[lane].length, state.pages[lane].has_more])
		.join(':');
}

export function attentionKnownTotal(
	combinedRowCount: number,
	attentionRowCount: number,
	needsActionCount: number,
	failedCount: number
): number {
	return Math.max(
		combinedRowCount,
		Math.max(needsActionCount + failedCount, attentionRowCount)
	);
}

export function attentionItemLaunchFailure(
	attentionError: string | null
): AttentionItemLaunchFailure {
	return attentionError ? 'load-error' : 'not-found';
}

export interface AttentionNextPagePlan {
	pageIndex: number | null;
	notice: string | null;
}

export function planAttentionNextPage(
	targetPage: number,
	pageSize: number,
	safeRowCount: number,
	blockedByCap: boolean,
	moreSourcesAvailable: boolean
): AttentionNextPagePlan {
	if (targetPage * pageSize < safeRowCount) {
		return { pageIndex: targetPage, notice: null };
	}
	if (blockedByCap) {
		return { pageIndex: null, notice: 'More items are available in full Attention.' };
	}
	if (!moreSourcesAvailable) {
		return { pageIndex: null, notice: 'You have reached the end of the inbox.' };
	}
	return { pageIndex: null, notice: null };
}
