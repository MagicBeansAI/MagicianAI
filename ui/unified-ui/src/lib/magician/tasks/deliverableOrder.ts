/**
 * Which of a task's outputs the reader sees first, and what the task's own
 * bookkeeping files are called on screen.
 *
 * A finished delegated task can hand back three kinds of file:
 *
 * - **Deliverables the run wrote** (role `user_attachment`, or promoted
 *   `user_media`). One of them may be marked the **primary** deliverable
 *   (`primary: true` on the outputs endpoint row and on the chat file block).
 * - **The task's summary**, `out_task_user_<task>_<exec>.md` (role
 *   `primary_task_user`): the agent's short write-up of what it did.
 * - **Agent-facing state**: the continuation context
 *   (`out_task_continuation_…`) and the retired agent copy
 *   (`out_task_agent_…`). Never a deliverable.
 *
 * The order is primary deliverable, other deliverables, then the summary —
 * labelled `Summary`, never by its internal file name. A task from before the
 * `primary` marker keeps its order except that the summary drops behind the
 * files the run wrote. One module, so the chat card and the task panel cannot
 * disagree about either rule.
 *
 * See `docs/components/unified-ui/unified-task-panel.md` (Output act).
 */

/** What the task's summary output is called wherever it is listed. */
export const SUMMARY_OUTPUT_LABEL = 'Summary';

/** The last path segment, ignoring a trailing slash. */
export function outputBasename(path: string | null | undefined): string {
	const raw = typeof path === 'string' ? path.trim() : '';
	return raw.split(/[\\/]/).filter(Boolean).pop() ?? '';
}

/** `out_task_user_<task>_<exec>.md` — the task summary's generated file name. */
export function isTaskSummaryFileName(path: string | null | undefined): boolean {
	return /^out_task_user_/i.test(outputBasename(path));
}

/**
 * The continuation context and the legacy agent copy, by their generated file
 * names: agent-facing state that never belongs in a list of results.
 */
export function isAgentFacingTaskFileName(path: string | null | undefined): boolean {
	return /^out_task_(?:continuation|agent)_/i.test(outputBasename(path));
}

/**
 * `0` the primary deliverable, `1` any other deliverable, `2` the summary.
 * Sorting by it is stable, so equal ranks keep the order the server sent.
 */
export function deliverableRank(entry: { primary?: boolean | null; summary?: boolean }): 0 | 1 | 2 {
	if (entry.primary === true) return 0;
	return entry.summary === true ? 2 : 1;
}

/**
 * `items` with the ranked ones reordered by rank **within the slots they
 * already occupied**. An item whose rank is `null` keeps its index, so a list
 * that mixes deliverables with rows ranked elsewhere (technical details, run
 * outputs) only moves its deliverables among themselves.
 */
export function orderWithinSlots<T>(items: readonly T[], rankOf: (item: T) => number | null): T[] {
	const slots: number[] = [];
	const ranked: Array<{ item: T; rank: number; at: number }> = [];
	items.forEach((item, at) => {
		const rank = rankOf(item);
		if (rank === null) return;
		slots.push(at);
		ranked.push({ item, rank, at });
	});
	ranked.sort((a, b) => a.rank - b.rank || a.at - b.at);
	const out = [...items];
	slots.forEach((slot, i) => {
		out[slot] = ranked[i].item;
	});
	return out;
}

/** The fields of a chat content block this module reads and writes. */
interface OutputBlockLike {
	type: string;
	relative_path?: string;
	display_name?: string;
	label?: string;
	primary?: boolean;
}

/**
 * A finished task's chat file blocks as the card shows them: agent-facing
 * state dropped, the summary labelled `Summary`, and the primary deliverable,
 * the other files, then the summary, in that order. Text and link blocks rank
 * with the other files, so they keep their place relative to them.
 *
 * `display_name` is left alone: it is still the name a download is saved as.
 */
export function presentTaskOutputBlocks<T extends OutputBlockLike>(blocks: readonly T[]): T[] {
	const visible: T[] = [];
	for (const block of blocks) {
		if (block.type !== 'file') {
			visible.push(block);
			continue;
		}
		const path = block.relative_path || block.display_name;
		if (isAgentFacingTaskFileName(path)) continue;
		visible.push(isTaskSummaryFileName(path) ? { ...block, label: SUMMARY_OUTPUT_LABEL } : block);
	}
	return orderWithinSlots(visible, (block) =>
		deliverableRank({
			primary: block.type === 'file' && block.primary === true,
			summary: block.type === 'file' && isTaskSummaryFileName(block.relative_path || block.display_name)
		})
	);
}
