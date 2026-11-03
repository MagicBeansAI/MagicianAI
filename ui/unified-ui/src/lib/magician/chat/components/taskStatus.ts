// Pure task-status helpers shared by TaskStatusCard and ChatPanel
// (ChatPanel still needs isTerminalTaskStatus for escalation labels and
// isRunExpanded for the toggle handler that owns the overrides map).
// Moved verbatim from ChatPanel.svelte during the Stage E decomposition.

import type {
	ChatMessage,
	ChatMessageContent,
	ChatRenderTaskExecutionGroup
} from '$lib/stores/chatStore';
import { timeSpan, toEpochMs, type CardTimes } from '$lib/shared/cardTimes';
import { stripMarkdownPreview } from '$lib/shared/markdownPreview';
import { threadHref } from '$lib/threads/threadLinks';

// `archived` is final too: an archived task never runs again, and a card whose
// last status update never arrived must settle on it instead of reading
// "Running" forever.
export function isTerminalTaskStatus(status: string | undefined): boolean {
	return (
		status === 'completed' || status === 'failed' || status === 'cancelled' || status === 'archived'
	);
}

// The reason a task card shows under its title: the task_status_update
// `summary` (markdown), whatever the status. The backend writes the live
// line while running, the pause reason while waiting, the failure reason on
// failure and the completion summary on success, so one field covers every
// state. Blank summaries count as none. Shared by TaskStatusCard and the
// chat-next LiveTaskCard so both show the same text.
export function taskStatusReason(content: Pick<ChatMessageContent, 'summary'>): string | undefined {
	return content.summary?.trim() || undefined;
}

// Status-driven visual treatment for the task_status_update card.
// Each `tone` maps to a `.chat-status-alert--<tone>` CSS class with
// distinct border + background + accent + icon color. The icon name
// drives which inline SVG to render (see the card template).
// Colors follow the app-wide status language (`--status-*` tokens,
// see $lib/shared/statusTone): info "new task", info "in flight"
// (spinner icon carries the motion), green check "done", red
// "failed", muted "cancelled".
export type TaskStatusTone = 'created' | 'running' | 'completed' | 'failed' | 'cancelled' | 'neutral';
export type TaskStatusIcon = 'sparkle' | 'spinner' | 'check' | 'alert' | 'pause' | 'clock';

export function taskStatusVisual(
	status: string | undefined,
	synthesisPending = false,
	completionKind?: 'full' | 'partial'
): { tone: TaskStatusTone; icon: TaskStatusIcon; verb: string } {
	// The task has finished executing but its final result is still being
	// synthesized (the backend holds the visible status at "running" during
	// this window). Show a distinct "preparing" state so the card doesn't
	// read as just running or prematurely done.
	if (synthesisPending) {
		return { tone: 'running', icon: 'spinner', verb: 'Preparing final result' };
	}
	switch (status) {
		case 'created':
			return { tone: 'created', icon: 'sparkle', verb: 'Created' };
		case 'planning':
			return { tone: 'running', icon: 'spinner', verb: 'Planning' };
		// Not started. Approval does not start a run and nothing queues one, so
		// neither may read as working: no spinner, no "Planning".
		case 'ready':
			return { tone: 'neutral', icon: 'clock', verb: 'Plan approved, not started' };
		case 'pending':
			return { tone: 'neutral', icon: 'clock', verb: 'Not started' };
		case 'draft':
			return { tone: 'neutral', icon: 'pause', verb: 'Plan ready for review' };
		case 'rejected':
			return { tone: 'neutral', icon: 'clock', verb: 'Plan rejected' };
		case 'running':
		case 'in_progress':
			return { tone: 'running', icon: 'spinner', verb: 'Running' };
		case 'paused':
			return { tone: 'neutral', icon: 'pause', verb: 'Paused' };
		case 'completed':
			if (completionKind === 'partial') {
				return { tone: 'neutral', icon: 'alert', verb: 'Partially completed' };
			}
			return { tone: 'completed', icon: 'check', verb: 'Completed' };
		case 'failed':
			return { tone: 'failed', icon: 'alert', verb: 'Failed' };
		case 'cancelled':
			return { tone: 'cancelled', icon: 'pause', verb: 'Cancelled' };
		case 'archived':
			return { tone: 'neutral', icon: 'check', verb: 'Archived' };
		default:
			return { tone: 'neutral', icon: 'clock', verb: status ?? 'Updated' };
	}
}

// Progressive disclosure for per-run sub-shells: default expansion +
// user override. The overrides map is passed in (rather than read from
// module state) so template call sites re-evaluate when it's reassigned.
export function isRunExpanded(
	overrides: Map<string, boolean>,
	group: ChatRenderTaskExecutionGroup,
	total: number
): boolean {
	const override = overrides.get(group.id);
	if (override !== undefined) return override;
	// Default open when watching matters (non-terminal run) or when this
	// is the card's only rendered run — the showGroupShell gate already
	// dropped empty single-run shells, so a lone shell has content and
	// collapsing it would just add a pointless extra click.
	return !isTerminalTaskStatus(group.message.content.status) || total === 1;
}

// One-line teaser for a collapsed run row: first sentence of the
// per-run summary when the card has multiple runs; a lone run's
// summary already renders at card level (the anti-echo rule), so it
// falls through to the status text instead of repeating it.
export function taskRunCollapsedLine(
	group: ChatRenderTaskExecutionGroup,
	total: number
): string {
	const summary = total > 1 ? group.message.content.summary?.trim() : undefined;
	if (summary) {
		// stripMarkdownPreview also drops image syntax and reduces
		// links to their label — the old marker-only regex leaked
		// `](http://…)` fragments into the teaser.
		const flattened = stripMarkdownPreview(summary);
		return flattened.match(/^.*?[.!?](?=\s|$)/)?.[0] ?? flattened;
	}
	return group.message.content.status?.replace(/_/g, ' ') ?? 'update';
}

export function taskExecutionLabel(
	group: ChatRenderTaskExecutionGroup,
	index: number,
	total: number
): string {
	const executionId = group.message.content.execution_id?.trim();
	if (!executionId) {
		return total > 1 ? `Primary run` : 'Run';
	}
	return total > 1 ? `Run ${index + 1}` : 'Run';
}

export function buildThreadTaskHref(content: ChatMessageContent): string | null {
	const taskId = content.task_id?.trim();
	if (!taskId) return null;
	const threadId = content.ui_thread_id?.trim();
	// Only link when we have a REAL thread context. Most chat-spawned
	// tasks are internal (lifecycle=Internal) and get pruned when their
	// chat session is cleared — linking to /tasks?selected=<id> would
	// land on an empty state. 'general' is the sentinel default for
	// sessions not bound to a named thread; an internal delegate run is
	// not listed under /t/general either, so that link dead-ends too.
	// In both cases render no link — the card's "Inspect run →" deep
	// panel is the right affordance for inspecting an internal run.
	if (threadId && threadId !== 'general') {
		const params = new URLSearchParams({ selected: taskId });
		return threadHref(threadId, `?${params.toString()}`);
	}
	return null;
}

export function buildArchivedTaskHref(content: ChatMessageContent): string | null {
	const taskId = content.task_id?.trim();
	if (!taskId) return null;
	// Same policy as buildThreadTaskHref: a real named thread only; skip
	// the 'general' sentinel (chat-internal tasks dead-end there).
	const threadId = content.ui_thread_id?.trim();
	if (threadId && threadId !== 'general') {
		const params = new URLSearchParams({ selected: taskId, filter: 'all' });
		return threadHref(threadId, `?${params.toString()}`);
	}
	return null;
}

/** The task-store fields a task card's times read (`Task` has them). */
export interface TaskRecordTimes {
	/** ISO. */
	createdAt?: string;
	/** Epoch ms of the run's last real step. */
	lastProgressAt?: number;
}

/**
 * When a task card's task was created and last updated, for its
 * "Created … · Updated …" line. Created is the task record's creation time when
 * the task store has the task, else the card's earliest update; never later
 * than that update (the store stamps a record that arrived without a creation
 * time as created "now"). Updated is the later of the card's latest update and
 * the record's last progress (`lastProgressAt`). The record's `updatedAt` is
 * never used: it moves on heartbeats and local edits, and the store stamps a
 * record without one as "now", so an idle task would read "Updated now".
 * Chat-started tasks are often absent from the store, so the card's own
 * updates carry them.
 */
export function taskCardTimes(
	message: ChatMessage,
	taskExecutionGroups: ChatRenderTaskExecutionGroup[] = [],
	record?: TaskRecordTimes
): CardTimes {
	const updates = timeSpan(
		[message, ...taskExecutionGroups.flatMap((group) => group.updates)].map((m) => m.created_at)
	);
	const recordCreated = toEpochMs(record?.createdAt);
	const createdAt =
		recordCreated !== undefined && updates.createdAt !== undefined
			? Math.min(recordCreated, updates.createdAt)
			: (recordCreated ?? updates.createdAt);
	const { updatedAt } = timeSpan([updates.updatedAt, record?.lastProgressAt]);
	return { createdAt, updatedAt };
}
