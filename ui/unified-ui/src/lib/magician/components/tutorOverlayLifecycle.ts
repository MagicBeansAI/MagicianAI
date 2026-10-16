export type TutorOverlayStatusTransition = 'working' | 'idle' | null;

/**
 * Reduce one Tutor activity event into an overlay status transition.
 *
 * Historical/non-live activity must never reopen an overlay. Terminal events
 * are different: a fire-and-forget chat card can stop being `live` before its
 * final Tutor event arrives, so completion/failure must still close the matching
 * overlay. The draw-overlay itself rejects an idle transition for a different
 * active session, which keeps delayed history from closing a newer run.
 */
export function tutorOverlayTransitionForEvent(
	eventType: string,
	runId: string,
	isLive: boolean,
	terminalRunIds: Set<string>
): TutorOverlayStatusTransition {
	if (eventType === 'tutor.run.completed' || eventType === 'tutor.run.failed') {
		terminalRunIds.add(runId);
		return 'idle';
	}

	if (!isLive) return null;
	if (eventType === 'tutor.run.started') {
		terminalRunIds.delete(runId);
		return 'working';
	}
	return terminalRunIds.has(runId) ? null : 'working';
}
