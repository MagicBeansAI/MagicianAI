export interface InspectableActivityPayload {
	task_id?: unknown;
	execution_id?: unknown;
}

export interface TaskBackedInspectTarget {
	taskId: string;
	executionId: string;
}

/**
 * Resolve the durable execution represented by a chat activity stream.
 *
 * Inline chat, Tutor, and Thinking Map model calls deliberately use the chat
 * turn id as their telemetry `execution_id`; that id has no execution-panel
 * record. A real inspectable delegated run always contributes a `task_id`,
 * although the task and execution ids may arrive on different events. Requiring
 * both keeps inline activity in the activity card instead of sending users to a
 * task-only inspector that can only return 404.
 *
 * An event that carries both ids names one run, so the newest such pair wins
 * over ids gathered from separate events. A turn that delegates and then
 * answers inline ends with the mouth's own calls, stamped with the chat-turn
 * id; picking each id independently paired that telemetry id with the
 * delegated task and the inspector could find neither.
 */
export function deriveTaskBackedInspectTarget(
	payloads: ReadonlyArray<InspectableActivityPayload | null | undefined>
): TaskBackedInspectTarget | null {
	let executionId: string | null = null;
	let taskId: string | null = null;

	for (let i = payloads.length - 1; i >= 0; i -= 1) {
		const payload = payloads[i];
		if (!payload) continue;

		const eventExecutionId = nonBlank(payload.execution_id);
		const eventTaskId = nonBlank(payload.task_id);
		if (eventExecutionId && eventTaskId) {
			return { taskId: eventTaskId, executionId: eventExecutionId };
		}
		executionId ??= eventExecutionId;
		taskId ??= eventTaskId;
	}

	return executionId && taskId ? { taskId, executionId } : null;
}

function nonBlank(value: unknown): string | null {
	if (typeof value !== 'string') return null;
	const trimmed = value.trim();
	return trimmed ? trimmed : null;
}
