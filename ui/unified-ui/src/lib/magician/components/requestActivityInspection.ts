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
 */
export function deriveTaskBackedInspectTarget(
	payloads: ReadonlyArray<InspectableActivityPayload | null | undefined>
): TaskBackedInspectTarget | null {
	let executionId: string | null = null;
	let taskId: string | null = null;

	for (let i = payloads.length - 1; i >= 0; i -= 1) {
		const payload = payloads[i];
		if (!payload) continue;

		if (!executionId && typeof payload.execution_id === 'string') {
			const candidate = payload.execution_id.trim();
			if (candidate) executionId = candidate;
		}
		if (!taskId && typeof payload.task_id === 'string') {
			const candidate = payload.task_id.trim();
			if (candidate) taskId = candidate;
		}
		if (executionId && taskId) break;
	}

	return executionId && taskId ? { taskId, executionId } : null;
}
