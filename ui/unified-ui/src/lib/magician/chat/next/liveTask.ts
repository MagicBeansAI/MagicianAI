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
 * the task is not started while the message still says planning or draft. The
 * chat posts no update for every transition (a plan approved before this
 * build, for one), and a card left at `draft` read as working forever.
 */
function effectiveStatus(
	messageStatus: string | undefined,
	storeStatus: string | undefined
): string | undefined {
	const behind =
		messageStatus === undefined || messageStatus === 'planning' || messageStatus === 'draft';
	if (behind && isNotStartedStatus(storeStatus)) return storeStatus;
	// The run ended but its terminal status update never reached the chat:
	// the card would read running forever (the native cards read the task's
	// status and show it settled). `storeStatusForCard` already dropped a
	// terminal status that belongs to an earlier run.
	if (isTerminalTaskStatus(storeStatus) && !isTerminalTaskStatus(messageStatus)) return storeStatus;
	return messageStatus;
}

/** The task-store fields `storeStatusForCard` reads. */
export interface LiveTaskStoreTask {
	status?: string;
	executionId?: string;
	activeExecutionId?: string;
}

/**
 * The task store's status for a card, or undefined when it cannot speak for
 * the card's run: a terminal store status from an EARLIER execution (the card
 * shows a rerun the store has not caught up with) must not end the new run.
 * A card message without an execution id cannot be told apart, so the store
 * (the fresher source) is taken as is.
 */
export function storeStatusForCard(
	task: LiveTaskStoreTask | undefined,
	messageExecutionId: string | undefined
): string | undefined {
	if (!task?.status) return undefined;
	const runId = messageExecutionId?.trim();
	if (!runId || !isTerminalTaskStatus(task.status)) return task.status;
	const storeRuns = [task.executionId, task.activeExecutionId].map((id) => id?.trim()).filter(Boolean);
	return storeRuns.length === 0 || storeRuns.includes(runId) ? task.status : undefined;
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
	storeStatus?: string
): LiveTaskView {
	const content = message.content;
	const contentStatus = effectiveStatus(content.status, storeStatus);
	const visual = taskStatusVisual(contentStatus, content.synthesis_pending);
	const partial =
		!content.synthesis_pending && isPartialCompletion(contentStatus, content.completion_kind);
	// A synthesizing task stays live even if its status already reads terminal.
	const terminal =
		!content.synthesis_pending && (partial || isTerminalTaskStatus(contentStatus));
	const status: LiveTaskStatus = content.synthesis_pending
		? 'running'
		: partial
			? 'partial'
			: toLiveStatus(contentStatus);
	const verb = partial ? 'Partially completed' : visual.verb;
	// The message's summary describes the status it carried; when the store's
	// status replaced that one, the summary is about a state the task left.
	const reason = contentStatus === content.status ? taskStatusReason(content) : undefined;

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
		if (content.synthesis_pending) currentStep = visual.verb;
		// The reason already says what is happening; with several runs, still
		// name the live one.
		else if (reason) currentStep = total > 1 ? running?.label : undefined;
		else currentStep = running?.label ?? visual.verb;
	}

	const times = [message, ...taskExecutionGroups.flatMap((group) => group.updates)]
		.map((m) => toEpochMs(m.created_at))
		.filter((t): t is number => t !== undefined);

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
		startable: !content.synthesis_pending && contentStatus === 'ready',
		awaitingApproval: !content.synthesis_pending && contentStatus === 'draft',
		steps,
		currentStep,
		startedAt: times.length ? Math.min(...times) : undefined,
		finishedAt: terminal && times.length ? Math.max(...times) : undefined,
		reason,
		outputs
	};
}

/** `m:ss`, or `h:mm:ss` past an hour. */
export function formatElapsed(ms: number): string {
	const total = Math.max(0, Math.floor(ms / 1000));
	const h = Math.floor(total / 3600);
	const m = Math.floor((total % 3600) / 60);
	const s = String(total % 60).padStart(2, '0');
	return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`;
}
