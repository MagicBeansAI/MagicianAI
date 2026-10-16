import { derived, type Readable } from 'svelte/store';

import { attentionStore } from '$lib/stores/attentionStore';
import { pendingHitlCount } from '$lib/stores/pendingHitlStore';

/**
 * Canonical count for every global "Needs you" / Attention badge.
 * Pending HITL and feed needs-action rows overlap, so the larger projection
 * wins; non-HITL failed rows are then added once.
 */
export function resolveAttentionBadgeCount(
	pendingHitl: number,
	feedNeedsAction: number,
	feedFailed: number
): number {
	return Math.max(0, pendingHitl, feedNeedsAction) + Math.max(0, feedFailed);
}

export const attentionBadgeCount: Readable<number> = derived(
	[pendingHitlCount, attentionStore],
	([$pendingHitlCount, $attentionStore]) =>
		resolveAttentionBadgeCount(
			$pendingHitlCount,
			$attentionStore.counts?.needs_action ?? 0,
			$attentionStore.counts?.failed ?? 0
		)
);
