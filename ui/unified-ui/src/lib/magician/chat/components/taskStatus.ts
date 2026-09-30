// Pure task-status helpers shared by TaskStatusCard and ChatPanel
// (ChatPanel still needs isTerminalTaskStatus for escalation labels and
// isRunExpanded for the toggle handler that owns the overrides map).
// Moved verbatim from ChatPanel.svelte during the Stage E decomposition.

import type { ChatMessageContent, ChatRenderTaskExecutionGroup } from '$lib/stores/chatStore';
import { stripMarkdownPreview } from '$lib/shared/markdownPreview';

export function isTerminalTaskStatus(status: string | undefined): boolean {
	return status === 'completed' || status === 'failed' || status === 'cancelled';
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
	synthesisPending = false
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
		case 'ready':
			return { tone: 'running', icon: 'spinner', verb: 'Planning' };
		case 'running':
		case 'in_progress':
			return { tone: 'running', icon: 'spinner', verb: 'Running' };
		case 'paused':
			return { tone: 'neutral', icon: 'pause', verb: 'Paused' };
		case 'completed':
			return { tone: 'completed', icon: 'check', verb: 'Completed' };
		case 'failed':
			return { tone: 'failed', icon: 'alert', verb: 'Failed' };
		case 'cancelled':
			return { tone: 'cancelled', icon: 'pause', verb: 'Cancelled' };
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
		return `/t/${encodeURIComponent(threadId)}?${params.toString()}`;
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
		return `/t/${encodeURIComponent(threadId)}?${params.toString()}`;
	}
	return null;
}
