<script lang="ts">
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { statusToneVar } from '$lib/shared/statusTone';
	import ChatContentBlocks from '$lib/magician/components/chat/ChatContentBlocks.svelte';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import RequestActivityCard from '$lib/magician/components/RequestActivityCard.svelte';
	import ExecutionControls from '$lib/magician/components/execution/ExecutionControls.svelte';
	import {
		getMessageContentBlocks,
		type ChatMessage,
		type ChatMessageContent,
		type ChatRenderTaskExecutionGroup
	} from '$lib/stores/chatStore';
	import {
		buildArchivedTaskHref,
		buildThreadTaskHref,
		isRunExpanded,
		isTerminalTaskStatus,
		taskExecutionLabel,
		taskRunCollapsedLine,
		taskStatusVisual
	} from '$lib/magician/chat/components/taskStatus';

	export let message: ChatMessage;
	// Collapsed run groups for this card (ChatRenderMessage.taskExecutionGroups).
	export let taskExecutionGroups: ChatRenderTaskExecutionGroup[] = [];
	// Expansion overrides map, owned + reassigned by ChatPanel so run
	// rows re-evaluate when the user toggles one (see isRunExpanded).
	export let runExpansionOverrides: Map<string, boolean> = new Map();
	export let activeSessionId: string | null = null;
	export let tailedTaskId: string | null = null;
	// Stateful flows stay in ChatPanel and arrive as callbacks.
	export let onToggleRun: (group: ChatRenderTaskExecutionGroup, total: number) => void;
	// A task-status card represents the durable task, so its run affordance
	// opens the canonical task-details panel. The embedded Steps card below
	// owns execution-only inspection through onInspectFromIds.
	export let onOpenTask: (content: ChatMessageContent) => void;
	export let onInspectFromIds: (
		detail: { taskId: string; executionId: string } | undefined
	) => void;
	export let onWatchLive: (sessionId: string, taskId: string) => void;
	export let onStopWatching: (sessionId: string) => void;
</script>

{#if message.content.type === 'task_status_update'}
	{@const content = message.content}
	{@const taskVisual = taskStatusVisual(content.status, content.synthesis_pending)}
	<div class="chat-status-alert chat-status-alert--{taskVisual.tone}">
		{#if taskVisual.icon === 'check'}
			<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="9 12 11 14 15 10"/></svg>
		{:else if taskVisual.icon === 'sparkle'}
			<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1"/></svg>
		{:else if taskVisual.icon === 'spinner'}
			<svg xmlns="http://www.w3.org/2000/svg" class="chat-status-spinner" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-6.2-8.55"/></svg>
		{:else if taskVisual.icon === 'alert'}
			<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="13"/><line x1="12" y1="16.5" x2="12.01" y2="16.5"/></svg>
		{:else if taskVisual.icon === 'pause'}
			<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="10" y1="9" x2="10" y2="15"/><line x1="14" y1="9" x2="14" y2="15"/></svg>
		{:else}
			<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
		{/if}
		<div class="chat-status-content">
			<!-- Card diet (Stage E): the card keeps exactly
			     verb+title, ONE status treatment (the tone
			     class on the alert — border/bg/icon), ONE
			     summary block, run rows, and the actions row.
			     The old Live pill and "N runs" / "Live
			     progress · N updates" meta lines are gone:
			     liveness reads from the spinner + the embedded
			     RequestActivityCard (the single live surface),
			     and the run rows below are countable. -->
			<span class="chat-status-label">
				{taskVisual.verb}: {content.display_label ?? content.task_id}
			</span>
			{#if content.summary}
				<div class="chat-status-summary">
					<ChatMarkdown content={content.summary} sessionId={message.session_id} />
				</div>
			{/if}
			<div class="chat-status-runs">
				{#each taskExecutionGroups as group, index (group.id)}
					{@const groupBlocks = getMessageContentBlocks(group.message.content)}
					{@const archivedTaskHref = buildArchivedTaskHref(group.message.content)}
					{@const groupHasActions = Boolean(group.message.content.execution_id || (isTerminalTaskStatus(group.message.content.status) && archivedTaskHref))}
					{@const showGroupShell = taskExecutionGroups.length > 1 || groupBlocks.length > 0 || groupHasActions}
					{#if showGroupShell}
						{@const runCount = taskExecutionGroups.length}
						{@const runExpanded = isRunExpanded(runExpansionOverrides, group, runCount)}
						<div class="chat-status-run">
							<!-- Progressive disclosure (Stage E): the run's
							     collapsed face is one line — tone dot + run
							     label + latest-line teaser + chevron. The full
							     shell (state/meta/summary/blocks/actions) only
							     renders when expanded. Live runs and a card's
							     lone content-bearing run start open (the
							     showGroupShell gate above still decides WHETHER
							     a shell exists at all). -->
							<button
								type="button"
								class="chat-status-run-toggle"
								aria-expanded={runExpanded}
								on:click={() => onToggleRun(group, runCount)}
							>
								<span
									class="chat-status-run-dot"
									style:background={statusToneVar(group.message.content.status)}
									aria-hidden="true"
								></span>
								<span class="chat-status-run-label">
									{taskExecutionLabel(group, index, runCount)}
								</span>
								<span class="chat-status-run-line">
									{taskRunCollapsedLine(group, runCount)}
								</span>
								<span
									class="chat-status-run-chevron"
									class:chat-status-run-chevron--open={runExpanded}
									aria-hidden="true"
								>
									<Icon name="chevron-right" size={12} />
								</span>
							</button>
							{#if runExpanded}
								<div class="chat-status-run-body">
									<!-- Single-run cards already show status/updates/summary in the card header; only repeat per-run when there are multiple runs (kills the "completed / 2 updates / Execution completed" echo). The run label lives on the toggle line above. Content blocks + actions still render below. -->
									{#if runCount > 1}
									<div class="chat-status-run-header">
										<span class="chat-status-run-state">{group.message.content.status}</span>
										{#if group.updates.length > 1}
											<span class="chat-status-run-meta">
												{group.updates.length} updates
											</span>
										{/if}
									</div>
									{#if group.message.content.summary}
										<div class="chat-status-run-summary">
											<ChatMarkdown content={group.message.content.summary} sessionId={group.message.session_id} />
										</div>
									{/if}
									{/if}
									<ChatContentBlocks
										sessionId={group.message.session_id}
										blocks={groupBlocks}
									/>
									{#if groupHasActions}
										<div class="chat-status-run-actions">
											{#if group.message.content.execution_id}
												<button
													type="button"
													class="chat-status-action"
													on:click={() => onOpenTask(group.message.content)}
												>
													Inspect run →
												</button>
												{#if !isTerminalTaskStatus(group.message.content.status)}
													<ExecutionControls
														executionId={group.message.content.execution_id}
														label={group.message.content.display_label ?? group.message.content.task_id ?? 'chat task'}
														variant="compact"
														showCancel={true}
														refreshKey={group.message.content.status}
													/>
												{/if}
											{/if}
											{#if isTerminalTaskStatus(group.message.content.status) && archivedTaskHref}
												<a href={archivedTaskHref} class="chat-status-action">
													Open archive debug →
												</a>
											{/if}
										</div>
									{/if}
								</div>
							{/if}
						</div>
					{/if}
				{/each}
			</div>
			<div class="chat-status-actions">
				{#if buildThreadTaskHref(content)}
					<a href={buildThreadTaskHref(content) ?? '#'} class="chat-status-action">
						{content.ui_thread_id ? 'Open thread task' : 'View task'} →
					</a>
				{/if}
				<!-- Phase 3.5a follow-up — Watch live / Stop
				     watching affordances. Non-terminal tasks
				     not currently tailed by this chat get a
				     "Watch live →" button (POSTs to
				     /tailed-task, subscribing without a chat
				     turn). The task this chat IS tailing
				     gets a "Stop watching" link (DELETE).
				     Terminal tasks get neither — no live
				     stream to attach. -->
				{#if !isTerminalTaskStatus(content.status) && content.task_id && activeSessionId}
					{#if tailedTaskId === content.task_id}
						<button
							type="button"
							class="chat-status-action chat-status-action--quiet"
							on:click={() => activeSessionId && onStopWatching(activeSessionId)}
						>
							Stop watching
						</button>
					{:else}
						<button
							type="button"
							class="chat-status-action"
							on:click={() =>
								activeSessionId &&
								content.task_id &&
								onWatchLive(activeSessionId, content.task_id)}
						>
							Watch live →
						</button>
					{/if}
				{/if}
			</div>
			<!-- Phase 1 — embedded per-task activity card.
			     While the task is non-terminal, render
			     the activity rows (tool calls / LLM
			     turns / reasoning / HITL pauses) live,
			     keyed on the synthetic `chat-task-<task_id>-<session_id>`
			     turn id that `dispatch_create_task`
			     stamps on every event via the task-id
			     chat fanout (see realtime_events.rs
			     `chat_fanout_by_task` + the canonical architecture doc
			     at docs/components/magician/chat-mode.md).
			     Terminal tasks drop the card — the
			     TaskStatusUpdate summary + output_files
			     above are sufficient and the underlying
			     jsonl stays on disk if anyone needs to
			     drill in. -->
			{#if !isTerminalTaskStatus(content.status)}
				<RequestActivityCard
					sessionId={message.session_id}
					chatTurnId={`chat-task-${content.task_id}-${message.session_id}`}
					controlExecutionId={content.execution_id ?? null}
					controlTaskId={content.task_id ?? null}
					showExecutionStop={false}
					live={true}
					embedded={true}
					on:inspect={(e) => onInspectFromIds(e.detail)}
				/>
			{/if}
		</div>
	</div>
{/if}

<style>
	/* Stage E card diet — exactly two type sizes inside the card:
	   var(--text-sm) for body (verb+title, summary) and var(--text-2xs)
	   for meta (run rows, states, actions). Nothing below 2xs. */
	.chat-status-alert {
		display: flex;
		max-width: min(100%, 500px);
		min-width: 0;
		font-size: var(--text-sm);
		border-radius: var(--radius-sm);
		padding: 0.6rem 0.85rem;
		gap: 0.5rem;
		align-items: flex-start;
		background: var(--accent-secondary-soft);
		border: 1px solid var(--accent-secondary);
		color: var(--text-primary, #2d3436);
	}

	/* Status-driven tone variants. Each shifts border / background /
	   icon accent so the lifecycle stage is glanceable at a card
	   glance rather than requiring the reader to parse the label.
	   Colors come from the app-wide status language (`--status-*` /
	   `--color-*` tokens — see $lib/shared/statusTone), so they theme
	   with the palette instead of hardcoded Tailwind hues. `created`
	   and `running` share the info hue (activity family); the sparkle
	   vs. spinner icon carries the distinction. */
	.chat-status-alert--created {
		background: color-mix(in srgb, var(--color-info) 8%, transparent);
		border-color: color-mix(in srgb, var(--color-info) 32%, transparent);
	}
	.chat-status-alert--created > svg {
		color: var(--color-info);
	}

	.chat-status-alert--running {
		background: color-mix(in srgb, var(--status-running) 8%, transparent);
		border-color: color-mix(in srgb, var(--status-running) 32%, transparent);
	}
	.chat-status-alert--running > svg {
		color: var(--status-running);
	}

	.chat-status-alert--completed {
		background: color-mix(in srgb, var(--status-completed) 10%, transparent);
		border-color: color-mix(in srgb, var(--status-completed) 38%, transparent);
	}
	.chat-status-alert--completed > svg {
		color: var(--status-completed);
	}

	.chat-status-alert--failed {
		background: color-mix(in srgb, var(--status-failed) 8%, transparent);
		border-color: color-mix(in srgb, var(--status-failed) 38%, transparent);
	}
	.chat-status-alert--failed > svg {
		color: var(--status-failed);
	}

	.chat-status-alert--cancelled {
		background: color-mix(in srgb, var(--text-muted, #7b8588) 8%, transparent);
		border-color: color-mix(in srgb, var(--text-muted, #7b8588) 28%, transparent);
		opacity: 0.85;
	}
	.chat-status-alert--cancelled > svg {
		color: var(--text-muted, #7b8588);
	}

	/* Spinner: continuous rotation for the in-flight icon. */
	.chat-status-spinner {
		animation: chat-status-spin 1.1s linear infinite;
		transform-origin: center;
	}
	@keyframes chat-status-spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}
	@media (prefers-reduced-motion: reduce) {
		.chat-status-spinner {
			animation: none;
		}
		.chat-status-run-chevron {
			transition: none;
		}
	}

	.chat-status-content {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		/* Let the one-line run teasers ellipsize instead of forcing the
		   card wider (flex items default to min-width: auto). */
		min-width: 0;
		flex: 1;
	}

	.chat-status-label {
		font-size: var(--text-sm);
		font-weight: 600;
		color: var(--text-primary, #2d3436);
	}

	.chat-status-summary {
		font-size: var(--text-sm);
		color: var(--text-secondary, #5f6668);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.chat-status-runs {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		margin-top: 0.45rem;
	}

	/* No shells rendered (showGroupShell dropped them all) — don't let
	   the empty container add phantom spacing to the dieted card. */
	.chat-status-runs:empty {
		display: none;
	}

	.chat-status-run {
		display: flex;
		flex-direction: column;
		border-radius: 0.85rem;
		border: 1px solid color-mix(in srgb, var(--accent-secondary, #4ecdc4) 22%, transparent);
		background: color-mix(in srgb, var(--bg-card, #ffffff) 92%, var(--accent-secondary-soft, rgba(78, 205, 196, 0.12)));
		overflow: hidden;
	}

	/* Collapsed run face: a real button spanning the row — dot, label,
	   one-line teaser, chevron. */
	.chat-status-run-toggle {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		width: 100%;
		min-width: 0;
		padding: 0.45rem 0.7rem;
		border: none;
		background: transparent;
		font: inherit;
		color: inherit;
		text-align: left;
		cursor: pointer;
	}

	.chat-status-run-toggle:hover {
		background: color-mix(in srgb, var(--accent-secondary, #4ecdc4) 10%, transparent);
	}

	.chat-status-run-dot {
		flex: none;
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 50%;
	}

	.chat-status-run-line {
		flex: 1;
		min-width: 0;
		font-size: var(--text-2xs);
		color: var(--text-muted, #7b8588);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.chat-status-run-chevron {
		flex: none;
		display: inline-flex;
		color: var(--text-muted, #7b8588);
		transition: transform 0.15s ease;
	}

	.chat-status-run-chevron--open {
		transform: rotate(90deg);
	}

	.chat-status-run-body {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		padding: 0 0.7rem 0.6rem;
	}

	.chat-status-run-header {
		display: flex;
		align-items: center;
		gap: 0.6rem;
	}

	.chat-status-run-label {
		flex: none;
		font-size: var(--text-2xs);
		font-weight: 700;
		color: var(--text-primary, #2d3436);
	}

	.chat-status-run-state {
		font-size: var(--text-2xs);
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted, #7b8588);
	}

	.chat-status-run-meta {
		font-size: var(--text-2xs);
		font-weight: 600;
		color: var(--text-muted, #7b8588);
	}

	.chat-status-run-summary {
		font-size: var(--text-2xs);
		color: var(--text-secondary, #5f6668);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.chat-status-run-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem;
		margin-top: 0.15rem;
	}

	.chat-status-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.65rem;
		margin-top: 0.25rem;
	}

	.chat-status-action {
		padding: 0;
		border: none;
		background: transparent;
		font-size: var(--text-2xs);
		font-weight: 600;
		color: var(--accent-primary, #ff6b6b);
		cursor: pointer;
		text-decoration: none;
	}

	.chat-status-action:hover {
		text-decoration: underline;
	}

	/* Phase 3.5a follow-up — "Stop watching" is a quieter
	   detach affordance vs the primary "Watch live →" call to
	   action. Muted color, lighter weight; underline-on-hover
	   shared with the base class. */
	.chat-status-action--quiet {
		color: var(--text-muted, #7b8588);
		font-weight: 500;
	}
</style>
