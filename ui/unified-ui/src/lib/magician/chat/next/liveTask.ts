// View-model for the chat-next live task card (collapsed header and its
// expanded details).
//
// Built from the same inputs TaskStatusCard reads: the promoted
// `task_status_update` message and its coalesced run groups
// (ChatRenderMessage.taskExecutionGroups). Status wording comes from
// `taskStatusVisual`, run labels from `taskExecutionLabel` and terminal
// detection from `isTerminalTaskStatus`, so both renderers agree.

import {
	getMessageContentBlocks,
	type ChatMessage,
	type ChatRenderTaskExecutionGroup,
	type ContentBlockRecord
} from '$lib/stores/chatStore';
import {
	isTerminalTaskStatus,
	taskExecutionLabel,
	taskStatusReason,
	taskStatusVisual
} from '$lib/magician/chat/components/taskStatus';
import { labelOrShortIdTitle } from '$lib/magician/tasks/taskPanelTitle';

export type LiveTaskStatus =
	| 'queued'
	| 'running'
	| 'waiting'
	| 'completed'
	| 'partial'
	| 'failed'
	| 'cancelled';
/** Same vocabulary as TurnStage rows. */
export type LiveTaskStepStatus = 'running' | 'done' | 'failed' | 'waiting';

export interface LiveTaskStep {
	key: string;
	label: string;
	status: LiveTaskStepStatus;
}

/** One run's content blocks (output files/images), as TaskStatusCard shows them. */
export interface LiveTaskOutput {
	key: string;
	/** Run label, only when the task has more than one run. */
	label?: string;
	sessionId: string | null;
	blocks: ContentBlockRecord[];
}

export interface LiveTaskView {
	id: string;
	/** The real task id (absent for malformed status messages). */
	taskId?: string;
	/** Latest run's execution id, for Inspect. */
	executionId?: string;
	title: string;
	status: LiveTaskStatus;
	/** Human status word from taskStatusVisual ("Running", "Preparing final result", …);
	 *  "Partially completed" for a partial success. */
	verb: string;
	terminal: boolean;
	/** Plan approved and not started: the card can offer Run. */
	startable: boolean;
	/** A drafted plan waiting for the reader's approval. Unlike a pause, this
	 *  status is itself an ask, so it needs the reader with no ask listed. */
	awaitingApproval: boolean;
	/** One step per run group, in render order. */
	steps: LiveTaskStep[];
	/** Non-terminal only, and only when the reason does not already say it. */
	currentStep?: string;
	/** Epoch ms. */
	startedAt?: number;
	finishedAt?: number;
	/** Why the task is where it is (markdown), the same text TaskStatusCard
	 *  shows: taskStatusReason(content), i.e. the trimmed `content.summary`. */
	reason?: string;
	/** Per-run content blocks, runs without blocks omitted. */
	outputs: LiveTaskOutput[];
}

function isWaitingStatus(status: string | undefined): boolean {
	// `draft`: a plan waiting for the reader's review.
	return status === 'paused' || status === 'draft' || /^(waiting|awaiting)/.test(status ?? '');
}

/** Partial success: a completed task whose completion kind is `partial`, or
 *  the yield disposition `partial_success` used as a status. */
function isPartialCompletion(status: string | undefined, completionKind: string | undefined): boolean {
	return status === 'partial_success' || (status === 'completed' && completionKind === 'partial');
}

/** Not started: `pending` (never planned) and `ready` (plan approved). No run
 *  exists, so the card must not tick, offer Cancel or read as working. */
function isNotStartedStatus(status: string | undefined): boolean {
	return status === 'ready' || status === 'pending' || status === 'rejected';
}

/**
 * The status to show: the card's latest message, unless the task store knows
 * the task has no active run while the message still reports an earlier state. The
 * chat posts no update for every transition (a plan approved before this
 * build, for one), and a card left at `draft` read as working forever.
 */
function effectiveStatus(
	messageStatus: string | undefined,
	storeStatus: string | undefined
): string | undefined {
	if (!isTerminalTaskStatus(messageStatus) && isNotStartedStatus(storeStatus)) return storeStatus;
	// The run ended but its terminal status update never reached the chat:
	// the card would read running forever (the native cards read the task's
	// status and show it settled). `storeStatusForCard` already dropped a
	// terminal status that belongs to an earlier run.
	if (isTerminalTaskStatus(storeStatus) && !isTerminalTaskStatus(messageStatus)) return storeStatus;
	return messageStatus;
}

/**
 * A `synthesis_pending` flag whose task has been silent this long never
 * cleared (e.g. the backend died mid-synthesis). Past it the card stops
 * reading as live. Synthesis runs 10 s to 2 min, so this is well clear of it.
 */
export const SYNTHESIS_STALE_MS = 10 * 60 * 1000;

/** Whether the flag is still believable: false once the task's last update is
 *  older than `SYNTHESIS_STALE_MS`. An unknown update time keeps it live. */
function isCancelledStatus(status: string | undefined): boolean {
	return status === 'cancelled' || status === 'canceled';
}

function synthesisIsLive(lastUpdateMs: number | undefined, now: number): boolean {
	return lastUpdateMs === undefined || now - lastUpdateMs < SYNTHESIS_STALE_MS;
}

/** The task-store fields `storeStatusForCard` reads. */
export interface LiveTaskStoreTask {
	status?: string;
	executionId?: string;
	activeExecutionId?: string;
	/** The task's terminal status is on disk but its final answer is still
	 *  being written. */
	synthesisPending?: boolean;
	/** ISO timestamp of the task's last update, for the stale cutoff. */
	updatedAt?: string;
}

/**
 * The task store's status for a card, or undefined when it cannot speak for
 * the card's run: a terminal store status from an EARLIER execution (the card
 * shows a rerun the store has not caught up with) must not end the new run,
 * and a not-started status counts only when the store names the card's run.
 * A card message without an execution id cannot be told apart, so the store
 * (the fresher source) is taken as is.
 */
export function storeStatusForCard(
	task: LiveTaskStoreTask | undefined,
	messageExecutionId: string | undefined,
	now: number = Date.now()
): string | undefined {
	if (!task?.status) return undefined;
	// A terminal status with its final answer still being written is not the
	// end of the card: the store cannot speak for a settled state yet.
	if (
		task.synthesisPending === true &&
		isTerminalTaskStatus(task.status) &&
		!isCancelledStatus(task.status)
	) {
		const updated = task.updatedAt ? Date.parse(task.updatedAt) : Number.NaN;
		if (synthesisIsLive(Number.isFinite(updated) ? updated : undefined, now)) return undefined;
	}
	const runId = messageExecutionId?.trim();
	if (!runId || (!isTerminalTaskStatus(task.status) && !isNotStartedStatus(task.status))) return task.status;
	const storeRuns = [task.executionId, task.activeExecutionId].map((id) => id?.trim()).filter(Boolean);
	if (storeRuns.includes(runId)) return task.status;
	// A not-started store row speaks for the card's run only when it names
	// that run (a cancelled run whose task went back to `ready`). A freshly
	// created task reads `ready`/`pending` with no execution id until the
	// store catches up with the run the card already shows: it must not turn
	// a running card into "queued" (no Cancel) or settle the turn's presence.
	if (isNotStartedStatus(task.status)) return undefined;
	return storeRuns.length === 0 ? task.status : undefined;
}

function toLiveStatus(status: string | undefined): LiveTaskStatus {
	if (status === 'completed' || status === 'failed' || status === 'cancelled') return status;
	// Archived is settled but not a failure: muted, like a cancelled card.
	if (status === 'archived') return 'cancelled';
	if (isWaitingStatus(status)) return 'waiting';
	if (isNotStartedStatus(status)) return 'queued';
	return 'running';
}

function toStepStatus(status: string | undefined): LiveTaskStepStatus {
	if (status === 'completed' || status === 'partial_success') return 'done';
	if (status === 'failed' || status === 'cancelled') return 'failed';
	if (isWaitingStatus(status)) return 'waiting';
	return 'running';
}

/** Epoch ms; values below 1e11 are taken as epoch seconds. */
function toEpochMs(value: number | undefined): number | undefined {
	if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) return undefined;
	return value < 1e11 ? value * 1000 : value;
}

export function toLiveTaskView(
	message: ChatMessage,
	taskExecutionGroups: ChatRenderTaskExecutionGroup[] = [],
	/** The task's status in the task store, when the host has it. */
	storeStatus?: string,
	/** The clock for the stale-synthesis cutoff; without it a flagged card
	 *  stays live however old its last update is. */
	now?: number
): LiveTaskView {
	const content = message.content;
	const resolvedStatus = effectiveStatus(content.status, storeStatus);
	const updateTimes = [message, ...taskExecutionGroups.flatMap((group) => group.updates)]
		.map((m) => toEpochMs(m.created_at))
		.filter((t): t is number => t !== undefined);
	// A synthesizing task stays live even if its status already reads terminal,
	// unless its last update is so old the flag can never have cleared.
	// A cancelled run abandons its synthesis: its flag is stale on arrival.
	const synthesisPending =
		content.synthesis_pending === true &&
		!isCancelledStatus(resolvedStatus) &&
		(now === undefined ||
			synthesisIsLive(updateTimes.length ? Math.max(...updateTimes) : undefined, now));
	// The flag is only ever set after the work finished, so one that went stale
	// settles the card as completed rather than leaving it running forever.
	const contentStatus =
		content.synthesis_pending === true && !synthesisPending && !isTerminalTaskStatus(resolvedStatus)
			? 'completed'
			: resolvedStatus;
	const visual = taskStatusVisual(contentStatus, synthesisPending);
	const partial = !synthesisPending && isPartialCompletion(contentStatus, content.completion_kind);
	const terminal = !synthesisPending && (partial || isTerminalTaskStatus(contentStatus));
	const status: LiveTaskStatus = synthesisPending
		? 'running'
		: partial
			? 'partial'
			: toLiveStatus(contentStatus);
	const verb = partial ? 'Partially completed' : visual.verb;
	// The message's summary describes the status it carried; when the store's
	// status replaced that one, the summary is about a state the task left.
	const reason = resolvedStatus === content.status ? taskStatusReason(content) : undefined;

	const total = taskExecutionGroups.length;
	// A finished task has no live run. A run whose own terminal status update
	// never reached the chat (the task's status, or the store's, already reads
	// terminal) would otherwise keep reading "running" after the task ended —
	// the native cards read it settled. Settle it with the task's outcome.
	const settledRun: LiveTaskStepStatus =
		(status === 'failed' || status === 'cancelled') && contentStatus !== 'archived'
			? 'failed'
			: 'done';
	const steps: LiveTaskStep[] = taskExecutionGroups.map((group, index) => {
		const runStatus = toStepStatus(group.message.content.status);
		return {
			key: group.id,
			label: taskExecutionLabel(group, index, total),
			status: terminal && (runStatus === 'running' || runStatus === 'waiting') ? settledRun : runStatus
		};
	});

	// No progress fraction: run groups are not a plan, so done ÷ runs is not
	// a meaningful percentage (one run is 0% then 100%).
	let currentStep: string | undefined;
	if (!terminal) {
		const running = [...steps].reverse().find((step) => step.status === 'running');
		if (synthesisPending) currentStep = visual.verb;
		// The reason already says what is happening; with several runs, still
		// name the live one.
		else if (reason) currentStep = total > 1 ? running?.label : undefined;
		else currentStep = running?.label ?? visual.verb;
	}

	const times = updateTimes;

	// The drawer's title rule: a real label as given, a routed prompt reduced
	// to its goal; never the raw id (`Task · 2679ce` instead). `summary` is the
	// reason line, not a title, so it is not used here.
	const title = labelOrShortIdTitle(content.display_label, content.task_id);

	// Same per-run derivation as TaskStatusCard (getMessageContentBlocks on each
	// run group's message).
	const outputs: LiveTaskOutput[] = taskExecutionGroups
		.map((group, index) => ({
			key: group.id,
			label: total > 1 ? taskExecutionLabel(group, index, total) : undefined,
			sessionId: group.message.session_id ?? null,
			blocks: getMessageContentBlocks(group.message.content)
		}))
		.filter((output) => output.blocks.length > 0);
	const latestGroup = taskExecutionGroups[taskExecutionGroups.length - 1];
	const executionId =
		latestGroup?.message.content.execution_id?.trim() || content.execution_id?.trim() || undefined;

	return {
		id: content.task_id?.trim() || message.id,
		taskId: content.task_id?.trim() || undefined,
		executionId,
		title,
		status,
		verb,
		terminal,
		startable: !synthesisPending && contentStatus === 'ready',
		awaitingApproval: !synthesisPending && contentStatus === 'draft',
		steps,
		currentStep,
		startedAt: times.length ? Math.min(...times) : undefined,
		finishedAt: terminal && times.length ? Math.max(...times) : undefined,
		reason,
		outputs
	};
}

/** `m:ss`, or `h:mm:ss` past an hour. Lives in `liveClock` beside the tick
 *  every running step's elapsed counter reads; re-exported for this card. */
export { formatElapsed } from './liveClock';
