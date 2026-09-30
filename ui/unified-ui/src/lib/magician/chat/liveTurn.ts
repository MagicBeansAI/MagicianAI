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
	resolveTurnId: ChatTurnIdResolver = (message) => message.chat_turn_id?.trim() || null
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
		if (TERMINAL_TASK_STATUSES.has(status)) continue;

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
