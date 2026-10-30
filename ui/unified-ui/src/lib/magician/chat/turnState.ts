import { writable, type Readable } from 'svelte/store';

/**
 * Server-side state of a chat turn (`chat::turn_state` in magician). The
 * session detail, `/chat/active` and `/queue` responses carry the session's
 * latest turn as `turn_state`; `chat.turn.state_changed` agent events carry
 * every change. A reloaded page reads it to show a turn that is still
 * running, or one that was stopped or failed, instead of guessing from the
 * transcript.
 */
export type ChatTurnStatus = 'running' | 'completed' | 'stopped' | 'failed';

export interface ChatTurnState {
	chat_turn_id: string;
	chat_session_id: string;
	status: ChatTurnStatus;
	started_at: number;
	ended_at?: number;
	/** `user_stop`, a watchdog reason (`turn_stalled`, …), `error` or `interrupted`. */
	reason?: string;
}

export const CHAT_TURN_STATE_EVENT = 'chat.turn.state_changed';
export const USER_STOP_REASON = 'user_stop';

const STATUSES = new Set<ChatTurnStatus>(['running', 'completed', 'stopped', 'failed']);

/** Realtime voice turns render through the call surface, not a chat card
 *  (`liveTurn.ts` skips their rows for the same reason). */
function isRealtimeVoiceTurn(turnId: string): boolean {
	return turnId.startsWith('voice-turn-') || turnId.startsWith('voice-request-');
}

/** Parse a wire `turn_state`; `sessionId` fills a payload that omits it. */
export function parseChatTurnState(raw: unknown, sessionId?: string): ChatTurnState | null {
	if (!raw || typeof raw !== 'object') return null;
	const value = raw as Record<string, unknown>;
	const turnId = typeof value.chat_turn_id === 'string' ? value.chat_turn_id.trim() : '';
	const status = value.status as ChatTurnStatus;
	const session =
		typeof value.chat_session_id === 'string' && value.chat_session_id.trim()
			? value.chat_session_id.trim()
			: (sessionId ?? '');
	if (!turnId || !session || !STATUSES.has(status)) return null;
	return {
		chat_turn_id: turnId,
		chat_session_id: session,
		status,
		started_at: typeof value.started_at === 'number' ? value.started_at : 0,
		...(typeof value.ended_at === 'number' ? { ended_at: value.ended_at } : {}),
		...(typeof value.reason === 'string' && value.reason ? { reason: value.reason } : {})
	};
}

/** Turn id → latest known state. */
const states = writable<Map<string, ChatTurnState>>(new Map());
export const chatTurnStateStore: Readable<Map<string, ChatTurnState>> = { subscribe: states.subscribe };

/**
 * Record one state. A settled state is never replaced by a stale `running`
 * snapshot of the same turn (a poll that raced the end event).
 */
export function recordChatTurnState(state: ChatTurnState | null): void {
	if (!state) return;
	states.update((current) => {
		const previous = current.get(state.chat_turn_id);
		if (previous && previous.status !== 'running' && state.status === 'running') return current;
		const next = new Map(current);
		next.set(state.chat_turn_id, state);
		return next;
	});
}

/** Record the `turn_state` field of a session/queue response. */
export function recordChatTurnStateFromResponse(data: unknown, sessionId: string): void {
	if (!data || typeof data !== 'object' || !('turn_state' in data)) return;
	const latest = parseChatTurnState((data as Record<string, unknown>).turn_state, sessionId);
	// The server names its latest turn (or none, after a restart): any other
	// turn of this session still marked running here is not running.
	states.update((current) => {
		let next: Map<string, ChatTurnState> | null = null;
		for (const [turnId, state] of current) {
			if (
				state.chat_session_id === sessionId &&
				state.status === 'running' &&
				turnId !== latest?.chat_turn_id
			) {
				next ??= new Map(current);
				next.delete(turnId);
			}
		}
		return next ?? current;
	});
	recordChatTurnState(latest);
}

export function handleChatTurnStateEnvelope(envelope: { event_type: string; payload: unknown }): void {
	if (envelope.event_type !== CHAT_TURN_STATE_EVENT) return;
	recordChatTurnState(parseChatTurnState(envelope.payload));
}

/** The turn the server says is running in `sessionId`, if any. */
export function runningTurnIdForSession(
	turnStates: ReadonlyMap<string, ChatTurnState>,
	sessionId: string | null | undefined
): string | null {
	if (!sessionId) return null;
	let latest: ChatTurnState | null = null;
	for (const state of turnStates.values()) {
		if (state.chat_session_id !== sessionId || state.status !== 'running') continue;
		if (isRealtimeVoiceTurn(state.chat_turn_id)) continue;
		if (!latest || state.started_at >= latest.started_at) latest = state;
	}
	return latest?.chat_turn_id ?? null;
}

/** Turn ids whose server state is settled; none of them is still live. */
export function settledTurnIds(turnStates: ReadonlyMap<string, ChatTurnState>): Set<string> {
	const settled = new Set<string>();
	for (const state of turnStates.values()) {
		if (state.status !== 'running') settled.add(state.chat_turn_id);
	}
	return settled;
}

/**
 * The status line for a settled turn that has no reply of its own. A
 * watchdog stop already has its notice in the transcript, and a completed
 * turn has its reply, so neither gets a line.
 */
export function turnStatusNotice(state: ChatTurnState | undefined): string | null {
	if (!state || isRealtimeVoiceTurn(state.chat_turn_id)) return null;
	if (state.status === 'stopped' && (state.reason ?? USER_STOP_REASON) === USER_STOP_REASON) {
		return 'Stopped';
	}
	if (state.status === 'failed') {
		return state.reason === 'interrupted'
			? 'Interrupted before it finished'
			: 'This turn failed before it could answer';
	}
	return null;
}

/** Test seam. */
export function resetChatTurnStates(): void {
	states.set(new Map());
}
