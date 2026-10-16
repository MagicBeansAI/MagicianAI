/**
 * Settle the activity card's `task::<id>` row from the transcript.
 *
 * The row normally settles on a terminal `chat.delegate.status_changed` /
 * `task.status_changed` / `chat.delegate.output_ready` in the turn's event
 * tail. When the card stopped tailing before that event arrived (or a REST
 * read predates it) the row stayed `running` and the reply's step strip
 * spun forever after the task had failed. The transcript's latest
 * `task_status_update` card for the task is authoritative too: when it is
 * terminal, the row settles from it.
 */

export type SettledTaskRow = { status: 'done' | 'failed'; label: string };

const IN_FLIGHT_SUFFIX =
	/ (running|planning|executing|queued|pending|created|paused|waiting|in_progress|preparing final result)$/;

function rowStatus(status: string): 'done' | 'failed' | null {
	if (status === 'completed' || status === 'succeeded') return 'done';
	if (status === 'failed' || status === 'cancelled') return 'failed';
	return null;
}

/**
 * The settled status + label for a running task row, or `null` when the
 * transcript has no terminal card for this task (the row keeps running).
 */
export function settleTaskRowFromTranscript(
	taskId: string,
	currentLabel: string,
	statuses: ReadonlyMap<string, string>
): SettledTaskRow | null {
	const status = statuses.get(taskId)?.trim().toLowerCase();
	if (!status) return null;
	const settled = rowStatus(status);
	if (!settled) return null;
	const word = status === 'succeeded' ? 'completed' : status;
	const label = IN_FLIGHT_SUFFIX.test(currentLabel)
		? currentLabel.replace(IN_FLIGHT_SUFFIX, ` ${word}`)
		: `${currentLabel} — ${word}`;
	return { status: settled, label };
}

/**
 * task_id → status of the LATEST `task_status_update` per task in a
 * transcript (messages in render order; a later card for the same task
 * replaces an earlier one).
 */
export function latestTaskStatuses(
	messages: Iterable<{ content: { type: string; task_id?: string; status?: string } }>
): Map<string, string> {
	const out = new Map<string, string>();
	for (const message of messages) {
		if (message.content.type !== 'task_status_update') continue;
		const taskId = message.content.task_id?.trim();
		const status = message.content.status?.trim();
		if (taskId && status) out.set(taskId, status);
	}
	return out;
}
