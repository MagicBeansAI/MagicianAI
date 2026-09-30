/**
 * Cross-unmount carry-over for `<ChatTurnProgress />` rows.
 *
 * Problem: the live progress bubble unmounts when `isSendingMessage`
 * flips false, but the operator might want to come back hours later and
 * see what tools the assistant used. The rows already accumulated in
 * memory are lost when the component goes away.
 *
 * Shape: two stores keyed differently:
 *
 *   1. `pendingActivityBySession` — what the freshly-unmounted bubble
 *      left behind. Keyed by chat-session id. One entry at a time per
 *      session (a new turn evicts the prior pending).
 *
 *   2. `activityByMessageId` — claimed rows attached to a specific
 *      assistant message id. Survives so the message bubble can show
 *      a collapsible "Activity" detail.
 *
 * Flow per turn:
 *   ChatTurnProgress (on destroy)
 *     → captureCompletedTurnActivity(sessionId, rows)
 *     → pendingActivityBySession[sessionId] = rows
 *
 *   Chat page (reactive block, on new assistant message)
 *     → claimPendingActivityForMessage(sessionId, messageId)
 *     → activityByMessageId[messageId] = rows
 *     → pendingActivityBySession[sessionId] = null
 *
 *   AssistantActivitySection (mounted inside each assistant message)
 *     → getActivityForMessage(messageId)
 *     → renders collapsible "What happened" if rows exist.
 *
 * In-memory only — page reload drops the history. Persisting per-
 * message activity to disk is a follow-up (would need a backend
 * endpoint or local IndexedDB; v1 keeps it ephemeral).
 */
import { derived, writable, type Readable } from 'svelte/store';

export interface ChatTurnActivityRow {
	key: string;
	kind: 'llm' | 'reasoning' | 'tool' | 'step' | 'artifact' | 'pause';
	label: string;
	detail: string | null;
	status: 'running' | 'done' | 'failed' | 'waiting';
	tone: 'info' | 'tool' | 'error' | 'reasoning' | 'pause';
	startedAt: number;
	durationMs: number | null;
}

const pendingActivityBySession = writable<Map<string, ChatTurnActivityRow[]>>(new Map());
const activityByMessageId = writable<Map<string, ChatTurnActivityRow[]>>(new Map());

/**
 * Called by `<ChatTurnProgress />` when it unmounts (= the chat turn
 * completed or was cancelled). Replaces any prior pending rows for the
 * session — a new turn evicts the old.
 */
export function captureCompletedTurnActivity(
	sessionId: string,
	rows: ChatTurnActivityRow[]
): void {
	if (rows.length === 0) return;
	pendingActivityBySession.update((current) => {
		const next = new Map(current);
		next.set(sessionId, rows);
		return next;
	});
}

/**
 * Called by the chat page when a new assistant message lands in the
 * visible list. Moves any pending rows for this session onto the
 * message id, then clears the pending slot.
 *
 * No-op when no pending rows exist (e.g. the assistant message arrived
 * without any progress events in flight — short-circuit replies).
 */
export function claimPendingActivityForMessage(
	sessionId: string,
	messageId: string
): void {
	let claimedRows: ChatTurnActivityRow[] | null = null;
	pendingActivityBySession.update((current) => {
		const found = current.get(sessionId);
		if (!found) return current;
		claimedRows = found;
		const next = new Map(current);
		next.delete(sessionId);
		return next;
	});
	if (claimedRows) {
		const rowsToStore = claimedRows;
		activityByMessageId.update((current) => {
			const next = new Map(current);
			next.set(messageId, rowsToStore);
			return next;
		});
	}
}

/** Read store — true reactive subscription. Use with `$` in components. */
export const activityByMessageIdStore: Readable<Map<string, ChatTurnActivityRow[]>> =
	derived(activityByMessageId, ($map) => $map);

/** One-shot read for non-reactive callers. */
export function getActivityForMessage(messageId: string): ChatTurnActivityRow[] | null {
	let result: ChatTurnActivityRow[] | null = null;
	activityByMessageId.subscribe(($map) => {
		result = $map.get(messageId) ?? null;
	})();
	return result;
}
