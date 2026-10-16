/**
 * The visible notice for an answer that arrived after its ask closed.
 *
 * `postHitlResponse` treats 404/409/410 on respond as a soft success so the
 * stale card drops, but a silent dismissal reads as "sent". Every surface
 * that posts through the adapter gets this one info toast instead; bursts
 * (bulk apply, double-clicks) coalesce into one.
 */
import { showInfo } from '$lib/shared/stores/notifications';

export const HITL_ALREADY_CLOSED_NOTICE = 'This question was already answered or closed.';
const COALESCE_MS = 3_000;

let lastShownAt = Number.NEGATIVE_INFINITY;

export function noticeHitlAlreadyClosed(now: number = Date.now()): void {
	if (now - lastShownAt < COALESCE_MS) return;
	lastShownAt = now;
	showInfo(HITL_ALREADY_CLOSED_NOTICE);
}

/** Test seam: let the next call show the notice again. */
export function resetHitlAlreadyClosedNoticeForTests(): void {
	lastShownAt = Number.NEGATIVE_INFINITY;
}
