import type { ChatMessage } from '$lib/stores/chatStore';

const TERMINAL_TASK_STATUSES = new Set(['completed', 'failed', 'cancelled']);

export type ChatTurnIdResolver = (message: ChatMessage) => string | null;

/**
 * Return the chat turn whose task activity still needs a live SSE tail.
 *
 * Task progress is an append-only history, so an old `running` row must not
 * keep a later chat turn alive after the same task emitted a terminal row.
 * We first reduce to the latest row per task, then consider only rows that are
 * non-terminal and have explicit task-local turn provenance. A later update
 * may reuse an earlier explicit turn id for the same task, but uncorrelated
 * legacy task cards are never attached to the session's newest user turn.
 */
export function latestActiveTaskTurnId(
	messages: ChatMessage[],
	resolveTurnId: ChatTurnIdResolver = (message) => message.chat_turn_id?.trim() || null,
	/** True when another source (the task store) knows this task's run ended
	 *  although its latest chat row still reads non-terminal. */
	isSettledElsewhere: (message: ChatMessage) => boolean = () => false
): string | null {
	const latestByTask = new Map<string, ChatMessage>();
	const latestExplicitTurnByTask = new Map<string, { turnId: string; createdAt: number }>();

	for (const message of messages) {
		if (message.content.type !== 'task_status_update') continue;
		const taskId = message.content.task_id?.trim();
		if (!taskId) continue;
		const explicitTurnId = resolveTurnId(message)?.trim();
		if (explicitTurnId) {
			const previousTurn = latestExplicitTurnByTask.get(taskId);
			if (!previousTurn || message.created_at >= previousTurn.createdAt) {
				latestExplicitTurnByTask.set(taskId, {
					turnId: explicitTurnId,
					createdAt: message.created_at
				});
			}
		}

		const previous = latestByTask.get(taskId);
		if (!previous || message.created_at >= previous.created_at) {
			latestByTask.set(taskId, message);
		}
	}

	let latest: { turnId: string; createdAt: number } | null = null;
	for (const [taskId, message] of latestByTask.entries()) {
		const status = message.content.status?.trim().toLowerCase() ?? '';
		if (TERMINAL_TASK_STATUSES.has(status) || isSettledElsewhere(message)) continue;

		// Task provenance is stable across its append-only status history.
		// Some older and cross-surface producers omitted correlation on a
		// later status message even though an earlier row for this exact task
		// carried it. Reuse only that task-local explicit provenance; never
		// infer a turn from the newest user message or another task.
		const turnId =
			resolveTurnId(message)?.trim() ?? latestExplicitTurnByTask.get(taskId)?.turnId;
		if (!turnId) continue;
		if (!latest || message.created_at >= latest.createdAt) {
			latest = { turnId, createdAt: message.created_at };
		}
	}

	return latest?.turnId ?? null;
}

/** How long an unanswered user row may still read as a turn in flight. */
export const OPEN_USER_TURN_MAX_AGE_MS = 10 * 60 * 1000;

const REALTIME_VOICE_SURFACES = new Set(['realtime_voice', 'authenticated_realtime_voice']);

/**
 * A row mirrored from a realtime voice session (a Live transcript fragment,
 * or a voice-delegated request). The realtime engine owns that reply, and
 * its transcript pins the assistant row to whichever user fragment is
 * current when the reply lands — so an earlier fragment of a split
 * utterance, a barge-in, or a plain "Bye." never gets an assistant row of
 * its own. An unanswered voice row is therefore no evidence of a chat turn
 * in flight.
 */
function isRealtimeVoiceRow(message: ChatMessage, turnId: string): boolean {
	if (turnId.startsWith('voice-turn-') || turnId.startsWith('voice-request-')) return true;
	return REALTIME_VOICE_SURFACES.has((message.source_surface ?? '').trim());
}

export interface OpenUserTurnOptions {
	resolveTurnId?: ChatTurnIdResolver;
	/** Turns whose spawned task already reached a terminal status. */
	turnsWithTerminalTask?: ReadonlySet<string>;
	/** Wall clock; pass a ticking value so the age cutoff can expire. */
	now?: number;
	maxAgeMs?: number;
}

/**
 * The chat turn the newest typed (non-realtime-voice) user row opened, while
 * it still has no reply: no assistant row on that turn, no terminal task for
 * it, and younger than `maxAgeMs`. This covers sends whose optimistic user
 * row lands before the sending state or activity events catch up (HUD/Tauri
 * attachment sends).
 *
 * Only the newest typed user row is considered. An older unanswered row is
 * superseded by any later one — the session answers in order — so it must
 * not keep a turn "live" after the conversation moved on.
 */
export function latestOpenUserTurnId(
	messages: readonly ChatMessage[],
	options: OpenUserTurnOptions = {}
): string | null {
	const resolveTurnId =
		options.resolveTurnId ?? ((message: ChatMessage) => message.chat_turn_id?.trim() || null);
	const now = options.now ?? Date.now();
	const maxAgeMs = options.maxAgeMs ?? OPEN_USER_TURN_MAX_AGE_MS;

	for (let i = messages.length - 1; i >= 0; i -= 1) {
		const message = messages[i];
		if (message?.direction !== 'user') continue;
		const turnId = resolveTurnId(message)?.trim();
		if (!turnId) return null;
		if (isRealtimeVoiceRow(message, turnId)) continue;
		if (now - message.created_at > maxAgeMs) return null;
		if (options.turnsWithTerminalTask?.has(turnId)) return null;
		const answered = messages.some(
			(candidate) =>
				candidate.direction === 'assistant' &&
				!candidate.id.startsWith('streaming-') &&
				resolveTurnId(candidate)?.trim() === turnId
		);
		return answered ? null : turnId;
	}
	return null;
}
