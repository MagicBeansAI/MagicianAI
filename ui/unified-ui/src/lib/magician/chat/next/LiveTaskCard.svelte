<script lang="ts" context="module">
	let detailsSeq = 0;
</script>

<script lang="ts">
	import { get as getStore } from 'svelte/store';
	import { loadTaskUsageSnapshot, taskUsage } from './taskUsage';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { pendingHitlTaskIds } from '$lib/stores/pendingHitlStore';
	import { formatUsd } from '$lib/magician/tasks/taskTimeline';
	// Chat-next live task card. Same props as TaskStatusCard so ChatPanel can
	// swap it in; the card reads message + taskExecutionGroups (through
	// toLiveTaskView). The collapsed card shows a status dot, the title, the
	// status verb and the reason (the same summary TaskStatusCard renders,
	// clamped to three lines). A finished task's outputs render in the card body
	// (previews on by default; the first OUTPUT_PREVIEW_LIMIT, "+N more" for the
	// rest). Clicking the card expands extras in place: live steps (the host's
	// `activity` slot, once it has rendered something) or the run list when it
	// carries information, and a running task's outputs. With none of those
	// the card is not expandable: no chevron, no toggle. The title row holds
	// the Inspect chip and, while the task runs, a Cancel chip that confirms in
	// a small popover. Props it does not use are accepted for a drop-in swap.
	import { afterUpdate, onDestroy, onMount, tick } from 'svelte';
	import ChatContentBlocks from '$lib/magician/components/chat/ChatContentBlocks.svelte';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import type {
		ChatMessage,
		ChatMessageContent,
		ChatRenderTaskExecutionGroup
	} from '$lib/stores/chatStore';
	import {
		formatElapsed,
		toLiveTaskView,
		type LiveTaskOutput,
		type LiveTaskStatus,
		type LiveTaskStepStatus
	} from './liveTask';
	import { shortTaskId } from '$lib/magician/tasks/taskPanelTitle';
	import { tintVars } from './tint';
	import PlanGlyph from '$lib/magician/tasks/PlanGlyph.svelte';
	import { liveTaskHasPlan } from '$lib/magician/tasks/taskHasPlan';
	import { planStepPoll } from './planStepPoll';
	import {
		planStepAccessibleSummary,
		planStepBar,
		planStepRowsFromSteps,
		planStepSummary
	} from './planStepProgress';

	export let message: ChatMessage;
	/** The task's status in the task store, which wins over a message that lags it. */
	export let storeStatus: string | undefined = undefined;
	/** The task's plan as the task store has it: its steps, for the preview. */
	export let plan: { status: string | null; steps: string[] } | null = null;
	/** Start an approved, not-started task's run. No Run chip without it. */
	export let onRun: ((taskId: string) => void | Promise<void>) | undefined = undefined;
	/** Open the plan graph (nodes and edges, hierarchy, editing). */
	export let onOpenPlan: ((taskId: string) => void) | undefined = undefined;
	/** Ask the host to load the plan's steps, once, when the card needs them. */
	export let onNeedPlan: ((taskId: string) => void) | undefined = undefined;
	export let taskExecutionGroups: ChatRenderTaskExecutionGroup[] = [];
	export let runExpansionOverrides: Map<string, boolean> = new Map();
	export let activeSessionId: string | null = null;
	export let tailedTaskId: string | null = null;
	export let onToggleRun: (group: ChatRenderTaskExecutionGroup, total: number) => void;
	/** Opens the full task; the card element is passed as the morph source. */
	export let onOpenTask: (content: ChatMessageContent, sourceEl?: HTMLElement) => void;
	export let onInspectFromIds: (
		detail: { taskId: string; executionId: string } | undefined
	) => void;
	export let onWatchLive: (sessionId: string, taskId: string) => void;
	export let onStopWatching: (sessionId: string) => void;
	/** Injected by the host (the existing task abort helper). Omitted when the
	 *  host has no real task id to cancel: the card then offers no Cancel. */
	export let onCancel: ((taskId: string) => void | Promise<void>) | undefined = undefined;

	// Drop-in props this card does not use.
	// A planned task leads its title with the plan glyph (same rule on every surface).
	$: planned = liveTaskHasPlan({
		messageStatus: message.content.status,
		storeStatus,
		storePlan: plan ? { planStatus: plan.status } : null,
		planSteps: plan?.steps
	});
	$: void [runExpansionOverrides, activeSessionId, tailedTaskId, onToggleRun, onWatchLive, onStopWatching];

	const STEP_STATUS_TEXT: Record<LiveTaskStepStatus, string> = {
		running: 'running',
		done: 'done',
		failed: 'failed',
		waiting: 'waiting'
	};

	/** Output blocks a collapsed finished card shows before "+N more". */
	const OUTPUT_PREVIEW_LIMIT = 2;

	const seq = ++detailsSeq;
	const regionId = `cn-live-task-details-${seq}`;
	const confirmId = `cn-live-task-confirm-${seq}`;
	const confirmTitleId = `cn-live-task-confirm-title-${seq}`;
	let cardEl: HTMLDivElement;
	let expanded = false;
	let confirming = false;
	let cancelling = false;
	let cancelWrapEl: HTMLSpanElement | undefined;
	let cancelButtonEl: HTMLButtonElement | undefined;
	let keepButtonEl: HTMLButtonElement | undefined;
	// The activity slot stays mounted (hidden) while the task is live, so the
	// card knows whether it has anything to show before offering to expand.
	let activityEl: HTMLElement | undefined;
	let activityHasContent = false;
	let activityObserver: MutationObserver | null = null;
	let observedActivityEl: HTMLElement | undefined;
	let now = Date.now();
	let timer: ReturnType<typeof setInterval> | null = null;
	let flourish = false;
	let lastStatus: LiveTaskStatus | null = null;
	let reasonEl: HTMLDivElement | undefined;
	let reasonOverflows = false;
	let reasonObserver: ResizeObserver | null = null;

	$: view = toLiveTaskView(message, taskExecutionGroups, storeStatus);
	// The task's model usage so far (calls, tokens, cost), from its scoped
	// activity stream: the run and its delegated children.
	$: usage = view.taskId ? $taskUsage.get(view.taskId) : undefined;
	// The stored figures (planning, and the run after a reload), read once per
	// phase: when the plan is done, while it runs, and when it has finished.
	$: usagePhase = view.terminal ? 'done' : notStarted || view.status === 'waiting' ? 'planned' : 'running';
	$: if (view.taskId) {
		const scope = getStore(scopeIdentityStore);
		void loadTaskUsageSnapshot(view.taskId, scope.principal, scope.workspace, usagePhase);
	}
	$: usageCost = usage ? formatUsd(usage.costUsd) : null;
	$: usageTokens = usage
		? usage.tokens >= 1000
			? `${(usage.tokens / 1000).toFixed(1)}k`
			: String(usage.tokens)
		: null;
	// A task that has not started has no run to time, stop or stream.
	$: notStarted = view.status === 'queued';
	// "Needs you" says the reader is asked something: an ask pending for this
	// task, or a drafted plan awaiting approval. A paused or merely waiting
	// status alone is not an ask, and a running task can be blocked on one.
	$: needsYou =
		!view.terminal &&
		(view.awaitingApproval || (view.taskId !== undefined && $pendingHitlTaskIds.has(view.taskId)));
	$: syncTicker(!view.terminal && !notStarted && view.startedAt !== undefined);
	$: trackStatus(view.status);
	$: elapsed =
		view.startedAt === undefined || notStarted
			? null
			: formatElapsed((view.finishedAt ?? now) - view.startedAt);
	// Accessible name: stable parts only (the ticking elapsed time would make
	// screen readers re-announce the card every second).
	$: fullTaskId = message.content.task_id?.trim() || null;
	$: shortId = shortTaskId(fullTaskId);
	let idCopied = false;
	let idCopiedTimer: ReturnType<typeof setTimeout> | undefined;
	async function copyTaskId(): Promise<void> {
		if (!fullTaskId) return;
		try {
			await navigator.clipboard.writeText(fullTaskId);
			idCopied = true;
			clearTimeout(idCopiedTimer);
			idCopiedTimer = setTimeout(() => (idCopied = false), 1400);
		} catch {
			/* clipboard unavailable — the full id is still in the tooltip */
		}
	}
	onDestroy(() => clearTimeout(idCopiedTimer));

	$: ariaLabel = `${view.title}, ${view.verb} — show details`;
	// The step line only when it adds something the verb does not already say
	// (the view-model already drops it when the reason covers it).
	$: stepLine = view.currentStep && view.currentStep !== view.verb ? view.currentStep : undefined;
	// A planned task's run: the plan's steps with their live status (the current
	// run's `step_statuses`, polled only while it runs), as a step bar and a
	// "Step 3 of 6 · name" line in place of the generic step line. Same strings
	// and segment rule as the iOS and Android cards. No plan steps, no change.
	$: watchSteps = planned && view.status === 'running' && !view.terminal && Boolean(view.taskId);
	$: stepPollScope = watchSteps ? getStore(scopeIdentityStore) : null;
	$: stepPoll =
		watchSteps && view.taskId && stepPollScope
			? planStepPoll(view.taskId, stepPollScope.principal, stepPollScope.workspace)
			: null;
	$: planRows = watchSteps && $stepPoll ? planStepRowsFromSteps($stepPoll, view.terminal) : [];
	$: stepBar = planStepBar(planRows);
	$: planSummary = planStepSummary(planRows);
	$: planSummaryLabel = planStepAccessibleSummary(planRows);
	// Live steps stream only while the task runs and has a real id (the host
	// fills the slot with a task-scoped ChatTurnProgress); otherwise the run list.
	// The activity slot used to hold a long-lived `/v3/events` stream, and one
	// per unfinished card exhausted the browser's ~6 connections per host on a
	// busy chat. It now subscribes to the app's event hub (one socket for the
	// whole app) and makes one REST backfill per mount, so it holds no
	// connection; a card still mounts it only while its task is running, or
	// while the reader has it open, to keep the backfill requests few.
	$: showActivity =
		$$slots.activity &&
		!view.terminal &&
		!notStarted &&
		Boolean(view.taskId) &&
		(view.status === 'running' || expanded);
	function inspect(): void {
		if (inspectIds && onInspectFromIds) onInspectFromIds(inspectIds);
		else onOpenTask(message.content, cardEl);
	}

	$: inspectIds =
		view.taskId && view.executionId
			? { taskId: view.taskId, executionId: view.executionId }
			: null;
	$: canCancel = !view.terminal && !notStarted && Boolean(onCancel);
	$: canRun = view.startable && Boolean(onRun) && Boolean(view.taskId);
	let running = false;
	async function run(): Promise<void> {
		if (!onRun || !view.taskId || running) return;
		running = true;
		try {
			await onRun(view.taskId);
		} finally {
			running = false;
		}
	}

	// The plan preview, before a run starts: an approved or drafted plan's
	// steps, like a finished task's outputs are its preview. Once a run starts,
	// the live activity says more.
	$: planSteps = plan?.steps ?? [];
	$: planPhase = notStarted || view.status === 'waiting';
	$: showPlan = !view.terminal && planPhase && planSteps.length > 0;
	let planRequestedFor: string | null = null;
	$: if (
		planPhase &&
		!view.terminal &&
		view.taskId &&
		planSteps.length === 0 &&
		onNeedPlan &&
		planRequestedFor !== view.taskId
	) {
		planRequestedFor = view.taskId;
		onNeedPlan(view.taskId);
	}
	$: if (!canCancel) confirming = false;
	// The run list only when it says something: while running (the fallback
	// when there is no live activity slot), or a finished task whose runs
	// ended differently. Never a lone "Run · done".
	$: distinctStepStatuses = new Set(view.steps.map((step) => step.status)).size;
	$: loneDoneRun = view.steps.length === 1 && view.steps[0].status === 'done';
	$: showRuns =
		!showActivity &&
		view.steps.length > 0 &&
		!loneDoneRun &&
		(!view.terminal || (view.steps.length >= 2 && distinctStepStatuses >= 2));
	// A finished task's outputs are the answer: they sit in the card body, not
	// behind the expand. A running task's partial outputs stay in the details.
	$: outputsInBody = view.terminal && view.outputs.length > 0;
	$: totalOutputBlocks = view.outputs.reduce((n, output) => n + output.blocks.length, 0);
	$: hiddenOutputs = expanded ? 0 : Math.max(0, totalOutputBlocks - OUTPUT_PREVIEW_LIMIT);
	$: bodyOutputs = expanded ? view.outputs : limitOutputs(view.outputs, OUTPUT_PREVIEW_LIMIT);
	$: if (!showActivity) activityHasContent = false;
	// What the expanded region would hold. Cancel is not part of it: it lives
	// in the title row.
	$: hasDetails =
		(showActivity && activityHasContent) ||
		showPlan ||
		showRuns ||
		(!outputsInBody && view.outputs.length > 0);
	// Expandable only when expanding reveals something: details, the clamped
	// reason's overflow, or outputs behind "+N more". Once open it stays a
	// toggle so it can close.
	// An unfinished card that is not streaming yet may have activity to show
	// once opened, so it stays expandable.
	$: activityOnOpen =
		$$slots.activity && !view.terminal && !notStarted && Boolean(view.taskId) && !showActivity;
	$: expandable = expanded || hasDetails || activityOnOpen || reasonOverflows || hiddenOutputs > 0;

	function limitOutputs(outputs: LiveTaskOutput[], limit: number): LiveTaskOutput[] {
		let left = limit;
		const shown: LiveTaskOutput[] = [];
		for (const output of outputs) {
			if (left <= 0) break;
			const blocks = output.blocks.slice(0, left);
			left -= blocks.length;
			shown.push({ ...output, blocks });
		}
		return shown;
	}

	function syncTicker(on: boolean): void {
		if (on && !timer) {
			now = Date.now();
			timer = setInterval(() => (now = Date.now()), 1000);
		} else if (!on && timer) {
			clearInterval(timer);
			timer = null;
		}
	}

	// Flourish only on a live transition into completed, never for a card
	// that mounts already completed (history).
	function trackStatus(status: LiveTaskStatus): void {
		if (lastStatus !== null && lastStatus !== 'completed' && status === 'completed') {
			flourish = true;
		}
		lastStatus = status;
	}

	function handleDotAnimationEnd(event: AnimationEvent): void {
		if (event.target === event.currentTarget) flourish = false;
	}

	// "more" shows only when the clamped reason actually hides lines.
	function measureReason(): void {
		const next = !expanded && reasonEl ? reasonEl.scrollHeight > reasonEl.clientHeight + 1 : false;
		if (next !== reasonOverflows) reasonOverflows = next;
	}

	onMount(() => {
		if (typeof ResizeObserver === 'undefined') return;
		reasonObserver = new ResizeObserver(() => measureReason());
	});

	afterUpdate(() => {
		if (reasonObserver && reasonEl) reasonObserver.observe(reasonEl);
		measureReason();
		watchActivity();
	});

	// The slot's content renders in a child component whose updates do not
	// reach this card's afterUpdate, so a MutationObserver re-measures it.
	function measureActivity(): void {
		const next = Boolean(activityEl?.textContent?.trim());
		if (next !== activityHasContent) activityHasContent = next;
	}

	function watchActivity(): void {
		if (activityEl !== observedActivityEl) {
			activityObserver?.disconnect();
			activityObserver = null;
			observedActivityEl = activityEl;
			if (activityEl && typeof MutationObserver !== 'undefined') {
				activityObserver = new MutationObserver(() => measureActivity());
				activityObserver.observe(activityEl, {
					childList: true,
					subtree: true,
					characterData: true
				});
			}
		}
		measureActivity();
	}

	async function openConfirm(): Promise<void> {
		confirming = true;
		await tick();
		keepButtonEl?.focus();
	}

	function closeConfirm(returnFocus: boolean): void {
		if (!confirming) return;
		confirming = false;
		if (returnFocus) cancelButtonEl?.focus();
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (confirming && event.key === 'Escape') {
			event.preventDefault();
			closeConfirm(true);
		}
	}

	function handleWindowPointerdown(event: PointerEvent): void {
		if (!confirming || cancelling) return;
		const target = event.target as Node | null;
		if (target && cancelWrapEl?.contains(target)) return;
		closeConfirm(false);
	}

	async function confirmCancel(): Promise<void> {
		if (!onCancel) return;
		cancelling = true;
		try {
			await onCancel(view.id);
		} finally {
			cancelling = false;
			closeConfirm(true);
		}
	}

	onDestroy(() => {
		syncTicker(false);
		reasonObserver?.disconnect();
		activityObserver?.disconnect();
	});
</script>

<svelte:window on:keydown={handleWindowKeydown} on:pointerdown={handleWindowPointerdown} />

<div
	bind:this={cardEl}
	class="cn-live-task"
	class:cn-live-task--expanded={expanded}
	class:cn-live-task--confirming={confirming}
	role="group"
	aria-label={`${view.title}, ${view.verb}`}
	data-status={view.status}
	style={tintVars(view.id)}
>
	<!-- Header. When there is something to expand, the toggle is an empty
	     <button> stretched over the whole header (named by aria-label), so the
	     title, status and reason read as part of it, while the action chips,
	     the reason's links and "more" stay real sibling buttons/links layered
	     above it, never nested inside. With nothing to expand there is no
	     toggle at all. -->
	<div class="cn-live-task__head" class:cn-live-task__head--toggle={expandable}>
		{#if expandable}
			<button
				type="button"
				class="cn-live-task__toggle"
				aria-label={ariaLabel}
				aria-expanded={expanded}
				aria-controls={regionId}
				on:click={() => (expanded = !expanded)}
			></button>
		{/if}

		<!-- Decorative: the status word is in the text and the accessible name. -->
		<span
			class="cn-live-task__dot"
			class:cn-live-task__dot--pop={flourish}
			data-status={view.status}
			data-testid="live-task-dot"
			aria-hidden="true"
			on:animationend={handleDotAnimationEnd}
		></span>

		<div class="cn-live-task__body">
			<div class="cn-live-task__top">
				{#if planned}<PlanGlyph />{/if}
				{#if fullTaskId && shortId && view.title === `Task · ${shortId}`}
					<!-- An id standing in for a title: the short form is for scanning, the
					     click copies the whole id (hover shows it). -->
					<span class="cn-live-task__title">
						Task ·
						<button
							type="button"
							class="cn-live-task__id"
							title={`${fullTaskId} — click to copy`}
							aria-label={idCopied ? 'Task id copied' : `Copy task id ${fullTaskId}`}
							on:click={copyTaskId}
						>{idCopied ? 'Copied' : shortId}</button>
					</span>
				{:else}
					<span class="cn-live-task__title">{view.title}</span>
				{/if}
				<!-- One chip. "Open task" and "Inspect" opened the same drawer (whole
				     task vs. one run — the same thing for a single-run task), so the
				     card inspects its latest run, and falls back to the task view
				     when it has no run id. Matches "Inspect run →" under turns.
				     It sits beside the title, not the status/elapsed line. -->
				<button
					type="button"
					class="cn-live-task__chip mg-chip-action"
					on:click={inspect}
				>
					Inspect →
				</button>
				{#if canRun}
					<!-- Approved and not started: approval does not start a run, so the
					     card offers it where the reader is looking. -->
					<button
						type="button"
						class="cn-live-task__chip cn-live-task__run-chip mg-chip-action"
						disabled={running}
						on:click={run}
					>
						{running ? 'Starting…' : 'Run'}
					</button>
				{/if}
				{#if canCancel}
					<!-- Contextual Cancel: only while the task runs, confirmed in a
					     small popover anchored here (never window.confirm). -->
					<span class="cn-live-task__cancel-wrap" bind:this={cancelWrapEl}>
						<button
							type="button"
							class="cn-live-task__cancel mg-chip-action"
							bind:this={cancelButtonEl}
							aria-haspopup="dialog"
							aria-expanded={confirming}
							aria-controls={confirming ? confirmId : undefined}
							on:click={() => (confirming ? closeConfirm(false) : openConfirm())}
						>
							Cancel
						</button>
						{#if confirming}
							<div
								id={confirmId}
								class="cn-live-task__confirm"
								role="dialog"
								aria-modal="false"
								aria-labelledby={confirmTitleId}
							>
								<p id={confirmTitleId} class="cn-live-task__confirm-text">Cancel this task?</p>
								<div class="cn-live-task__confirm-actions">
									<button
										type="button"
										class="cn-live-task__btn"
										bind:this={keepButtonEl}
										disabled={cancelling}
										on:click={() => closeConfirm(true)}
									>
										Keep running
									</button>
									<button
										type="button"
										class="cn-live-task__btn cn-live-task__btn--danger"
										disabled={cancelling}
										on:click={confirmCancel}
									>
										Cancel task
									</button>
								</div>
							</div>
						{/if}
					</span>
				{/if}
				{#if expandable}<svg class="cn-live-task__chevron" viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4" /></svg>{/if}
			</div>
			<div class="cn-live-task__meta">
				{#if notStarted}
					<!-- First in the row: what kind of state this is, before the words. -->
					<span class="cn-live-task__pill cn-live-task__pill--quiet">Not started</span>
				{/if}
				<span class="cn-live-task__status">{view.verb}</span>
				{#if elapsed}
					<span class="cn-live-task__elapsed" data-testid="live-task-elapsed">{elapsed}</span>
				{/if}
				{#if usage && usage.calls > 0}
					<span
						class="cn-live-task__usage"
						data-testid="live-task-usage"
						title={[
							`${usage.calls} model call${usage.calls === 1 ? '' : 's'}, ${usage.tokens.toLocaleString()} tokens${usageCost ? `, ${usageCost}` : ''}`,
							usage.planning
								? `Planning: ${usage.planning.calls} call${usage.planning.calls === 1 ? '' : 's'}${formatUsd(usage.planning.costUsd) ? `, ${formatUsd(usage.planning.costUsd)}` : ''}`
								: null,
							usage.run
								? `Run (incl. delegated work): ${usage.run.calls} call${usage.run.calls === 1 ? '' : 's'}${formatUsd(usage.run.costUsd) ? `, ${formatUsd(usage.run.costUsd)}` : ''}`
								: null
						]
							.filter(Boolean)
							.join('\n')}
						>{usage.calls} call{usage.calls === 1 ? '' : 's'} · {usageTokens} tok{usageCost
							? ` · ${usageCost}`
							: ''}</span
					>
				{/if}
				{#if needsYou}
					<span class="cn-live-task__pill">Needs you</span>
				{/if}
			</div>
			{#if stepBar}
				<!-- The line, then the bar under it (the iOS/Android order). The bar
				     carries the accessible summary; the line repeats it visually. -->
				<span class="cn-live-task__step" aria-hidden="true" data-testid="live-task-step-line"
					>{planSummary}</span
				>
				<div
					class="cn-live-task__steps"
					role="img"
					aria-label={planSummaryLabel}
					data-testid="live-task-step-bar"
				>
					{#if stepBar.kind === 'segments'}
						{#each stepBar.marks as mark, index (index)}
							<span class="cn-live-task__seg" data-mark={mark}></span>
						{/each}
					{:else}
						<span class="cn-live-task__fill" style={`--cn-step-fill: ${stepBar.fraction}`}></span>
					{/if}
				</div>
			{:else if stepLine}
				<span class="cn-live-task__step">{stepLine}</span>
			{/if}
			{#if view.reason}
				<div
					bind:this={reasonEl}
					class="cn-live-task__reason"
					class:cn-live-task__reason--clamped={!expanded}
					data-testid="live-task-reason"
				>
					<ChatMarkdown content={view.reason} sessionId={message.session_id ?? null} />
				</div>
				{#if reasonOverflows && !expanded}
					<button
						type="button"
						class="cn-live-task__more"
						aria-controls={regionId}
						on:click={() => (expanded = true)}
					>
						Show more
						<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="6 9 12 15 18 9" /></svg>
					</button>
				{/if}
			{/if}
		</div>
	</div>

	{#if outputsInBody}
		<section class="cn-live-task__outputs cn-live-task__outputs--body" aria-label="Outputs">
			{#each bodyOutputs as run (run.key)}
				{#if run.label}<p class="cn-live-task__run-caption">{run.label}</p>{/if}
				<ChatContentBlocks sessionId={run.sessionId ?? message.session_id ?? null} blocks={run.blocks} />
			{/each}
			{#if hiddenOutputs > 0}
				<button
					type="button"
					class="cn-live-task__more-outputs mg-chip-action"
					aria-controls={regionId}
					on:click={() => (expanded = true)}
				>
					+{hiddenOutputs} more
				</button>
			{/if}
		</section>
	{/if}

	<div
		id={regionId}
		class="cn-live-task__details"
		role={expanded ? 'region' : undefined}
		aria-label={expanded ? `${view.title} details` : undefined}
	>
		<!-- The live activity slot stays mounted (hidden) while the task runs so
		     the card can tell whether it has anything to show. -->
		{#if showActivity || (expanded && hasDetails)}
			<div class="cn-live-task__details-inner" hidden={!(expanded && hasDetails)}>
				{#if showPlan}
					<section class="cn-live-task__plan" aria-label="Plan">
						<div class="cn-live-task__plan-head">
							<span class="cn-live-task__plan-title"
								>Plan · {planSteps.length} step{planSteps.length === 1 ? '' : 's'}</span
							>
							{#if onOpenPlan && view.taskId}
								{@const planTaskId = view.taskId}
								<button
									type="button"
									class="mg-chip-action"
									aria-label="Open plan graph"
									on:click={() => onOpenPlan?.(planTaskId)}
								>
									Plan graph
								</button>
							{/if}
						</div>
						<ol class="cn-live-task__plan-steps">
							{#each planSteps as step, index (index)}
								<li>{step}</li>
							{/each}
						</ol>
					</section>
				{/if}
				{#if showActivity}
					<section
						class="cn-live-task__activity"
						bind:this={activityEl}
						hidden={!activityHasContent}
					>
						<slot name="activity" taskId={view.taskId} executionId={view.executionId} />
					</section>
				{:else if showRuns}
					<ol class="cn-live-task__runs" aria-label="Runs">
						{#each view.steps as step (step.key)}
							<li class="cn-live-task__run" data-status={step.status}>
								<span class="cn-live-task__run-dot" aria-hidden="true"></span>
								<span class="cn-live-task__run-label">{step.label}</span>
								<span class="cn-live-task__run-status">{STEP_STATUS_TEXT[step.status]}</span>
							</li>
						{/each}
					</ol>
				{/if}

				{#if !outputsInBody && view.outputs.length > 0}
					<section class="cn-live-task__outputs" aria-label="Outputs">
						{#each view.outputs as run (run.key)}
							{#if run.label}<p class="cn-live-task__run-caption">{run.label}</p>{/if}
							<ChatContentBlocks sessionId={run.sessionId ?? message.session_id ?? null} blocks={run.blocks} />
						{/each}
					</section>
				{/if}
			</div>
		{/if}
	</div>
</div>

<style>
	.cn-live-task {
		--cn-live-accent: var(--status-running);
		--cn-live-pad: 0.9rem;
		--cn-live-dot: 10px;
		--cn-live-gap: 0.7rem;
		display: grid;
		width: min(100%, 420px);
		min-width: 0;
		border: 1px solid color-mix(in srgb, var(--cn-tint) 70%, var(--border-default));
		border-radius: var(--radius-md);
		background: color-mix(in srgb, var(--cn-tint) 35%, var(--bg-card));
		box-shadow: var(--shadow-float);
		color: var(--text-primary);
		transition:
			transform var(--dur-quick) var(--spring-snappy),
			box-shadow var(--dur-quick) ease;
	}

	.cn-live-task:not(.cn-live-task--expanded):not(.cn-live-task--confirming):hover {
		transform: translateY(-1px);
	}

	/* The confirm popover hangs below the card: lift the card over the
	   transcript rows that follow it while it is open. */
	.cn-live-task--confirming {
		position: relative;
		z-index: 5;
	}

	.cn-live-task:has(.cn-live-task__toggle:focus-visible) {
		box-shadow: var(--shadow-float), var(--ring-focus);
	}

	.cn-live-task__head {
		position: relative;
		display: flex;
		align-items: flex-start;
		gap: var(--cn-live-gap);
		min-width: 0;
		padding: 0.65rem var(--cn-live-pad);
	}

	.cn-live-task__head--toggle {
		cursor: pointer;
	}

	/* The toggle covers the whole header; the interactive bits below
	   (chips, reason links, "more") are positioned above it. */
	.cn-live-task__toggle {
		position: absolute;
		inset: 0;
		padding: 0;
		border: 0;
		border-radius: inherit;
		background: transparent;
		cursor: pointer;
	}

	.cn-live-task__chip,
	.cn-live-task__more,
	.cn-live-task__id,
	.cn-live-task__reason :global(a) {
		position: relative;
		z-index: 1;
	}

	/* Look comes from the shared `.mg-chip-action` (app.css), the same chip
	   as RequestActivityCard's "Inspect run →"; only placement here. */
	.cn-live-task__chip {
		flex: none;
	}

	/* Cancel: the shared chip in a danger tone, right before the chevron. */
	.cn-live-task__cancel-wrap {
		position: relative;
		z-index: 2;
		flex: none;
		margin-inline-start: auto;
	}

	.cn-live-task__cancel-wrap + .cn-live-task__chevron {
		margin-inline-start: 0;
	}

	.cn-live-task__cancel {
		border-color: color-mix(in srgb, var(--color-error) 55%, var(--border-soft));
		color: var(--color-error);
	}

	.cn-live-task__cancel:hover,
	.cn-live-task__cancel[aria-expanded='true'] {
		background: color-mix(in srgb, var(--color-error) 12%, transparent);
		color: var(--color-error);
	}

	.cn-live-task__confirm {
		position: absolute;
		top: calc(100% + 0.4rem);
		right: 0;
		z-index: 3;
		display: grid;
		gap: 0.55rem;
		width: max-content;
		max-width: min(16rem, 80vw);
		padding: 0.7rem 0.8rem;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-md);
		background: var(--bg-card);
		box-shadow: var(--shadow-float);
		color: var(--text-primary);
		cursor: default;
		transform-origin: top right;
		animation: cn-live-confirm-in var(--dur-quick) var(--spring-snappy);
	}

	.cn-live-task__confirm-actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.4rem;
	}

	@keyframes cn-live-confirm-in {
		from {
			opacity: 0;
			transform: translateY(-4px) scale(0.96);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}

	.cn-live-task__toggle:focus-visible {
		outline: none;
	}

	.cn-live-task__top {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		min-width: 0;
	}

	/* Decorative, and clicks pass through to the header toggle under it. The
	   expanded state's `rotate(180deg)` makes it a transformed element, which
	   paints above the absolutely positioned toggle and swallowed the click:
	   the up-chevron did not collapse the card. */
	.cn-live-task__chevron {
		flex: none;
		margin-inline-start: auto;
		pointer-events: none;
		color: var(--text-muted);
		transition: transform var(--dur-base) var(--spring-gentle);
	}

	.cn-live-task--expanded .cn-live-task__chevron {
		transform: rotate(180deg);
	}

	/* Status colours: the app-wide --status-* language (defined for every
	   theme in app.css). Partial and waiting share amber; waiting pulses,
	   partial is solid. */
	.cn-live-task[data-status='completed'] {
		--cn-live-accent: var(--status-completed);
	}

	.cn-live-task[data-status='failed'] {
		--cn-live-accent: var(--status-failed);
	}

	.cn-live-task[data-status='partial'] {
		--cn-live-accent: var(--color-warning);
	}

	.cn-live-task[data-status='waiting'] {
		--cn-live-accent: var(--status-paused);
	}

	.cn-live-task[data-status='cancelled'] {
		--cn-live-accent: var(--text-muted);
	}

	/* Not started: the same secondary accent as its tag. Muted grey read as
	   bad news for a task that is simply ready and waiting to be run. */
	.cn-live-task[data-status='queued'] {
		--cn-live-accent: var(--accent-secondary);
	}

	.cn-live-task__run-chip {
		border-color: color-mix(in srgb, var(--status-running) 55%, var(--border-soft));
		color: color-mix(in srgb, var(--status-running) 60%, var(--text-primary));
	}

	.cn-live-task__plan {
		display: grid;
		gap: 0.35rem;
	}

	.cn-live-task__plan-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.cn-live-task__plan-title {
		color: var(--text-secondary);
		font-size: var(--text-xs);
		font-weight: 600;
	}

	.cn-live-task__plan-steps {
		margin: 0;
		padding-inline-start: 1.2rem;
		color: var(--text-primary);
		font-size: var(--text-xs);
		line-height: 1.5;
	}

	.cn-live-task__dot {
		flex: none;
		width: var(--cn-live-dot);
		height: var(--cn-live-dot);
		/* Centre on the title's first line. */
		margin-top: calc((var(--text-sm) * 1.5 - var(--cn-live-dot)) / 2);
		border-radius: 50%;
		background: var(--cn-live-accent);
	}

	.cn-live-task__dot[data-status='running'] {
		animation: cn-live-breathe calc(var(--dur-slow) * 4) ease-in-out infinite;
	}

	.cn-live-task__dot[data-status='waiting'] {
		animation: cn-live-pulse calc(var(--dur-slow) * 3) ease-in-out infinite;
	}

	.cn-live-task__dot--pop {
		animation: cn-live-pop var(--dur-slow) var(--spring-bouncy);
	}

	.cn-live-task__body {
		display: grid;
		flex: 1;
		gap: 0.1rem;
		min-width: 0;
	}

	.cn-live-task__id {
		padding: 0 0.3rem;
		border: 1px dashed var(--border-default);
		border-radius: var(--radius-sm);
		background: transparent;
		color: inherit;
		font: inherit;
		font-family: var(--font-mono);
		font-size: 0.9em;
		cursor: copy;
	}

	.cn-live-task__id:hover {
		border-style: solid;
		background: var(--bg-soft);
	}

	.cn-live-task__id:focus-visible {
		outline: none;
		box-shadow: var(--ring-focus);
	}

	.cn-live-task__title {
		overflow: hidden;
		font-size: var(--text-sm);
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.cn-live-task__meta {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.45rem;
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	.cn-live-task__elapsed {
		font-variant-numeric: tabular-nums;
	}

	/* A status label: a rounded square (--radius-chip), never a pill. */
	.cn-live-task__pill {
		padding: 0.05rem 0.45rem;
		border-radius: var(--radius-chip);
		background: color-mix(in srgb, var(--color-warning) 30%, transparent);
		color: var(--text-primary);
		font-weight: 600;
	}

	/* "Not started": a small tag in the theme's secondary accent, a step below
	   the status line so it labels rather than shouts. Not amber (that is
	   "Needs you", which asks something of the reader) and not the running
	   blue. Text is mixed toward --text-primary so every theme can read it. */
	.cn-live-task__pill--quiet {
		padding: 0 0.35rem;
		border: 1px solid color-mix(in srgb, var(--accent-secondary) 45%, transparent);
		background: color-mix(in srgb, var(--accent-secondary) 14%, var(--bg-card));
		color: color-mix(in srgb, var(--accent-secondary) 45%, var(--text-primary));
		font-size: 0.66rem;
		line-height: 1.5;
	}

	.cn-live-task__usage {
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}

	.cn-live-task__step {
		overflow: hidden;
		color: var(--text-secondary);
		font-size: var(--text-xs);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* The plan-step bar: one rounded-square segment per step (up to 12), else
	   one continuous fill. Colours are the app-wide status tokens: done
	   completed, failed failed, the live step running, skipped muted, pending
	   the control border. */
	.cn-live-task__steps {
		display: flex;
		gap: 3px;
		margin-top: 0.3rem;
		min-width: 0;
		height: 5px;
		/* A third of the theme's small radius: at 5px tall the full radius
		   would round the segments into pills. */
		--cn-step-radius: calc(var(--radius-sm) / 3);
	}

	.cn-live-task__seg {
		flex: 1;
		min-width: 0;
		border-radius: var(--cn-step-radius);
		background: var(--border-default);
		transition: background-color var(--dur-base) ease;
	}

	.cn-live-task__seg[data-mark='done'] {
		background: var(--status-completed);
	}

	.cn-live-task__seg[data-mark='failed'] {
		background: var(--status-failed);
	}

	.cn-live-task__seg[data-mark='inProgress'] {
		background: var(--status-running);
	}

	.cn-live-task__seg[data-mark='skipped'] {
		background: color-mix(in srgb, var(--text-muted) 45%, transparent);
	}

	.cn-live-task__steps:has(.cn-live-task__fill) {
		overflow: hidden;
		border-radius: var(--cn-step-radius);
		background: var(--border-default);
	}

	.cn-live-task__fill {
		min-width: 5px;
		width: calc(var(--cn-step-fill) * 100%);
		border-radius: var(--cn-step-radius);
		background: var(--status-running);
		transition: width var(--dur-base) ease;
	}

	.cn-live-task__reason {
		margin-top: 0.15rem;
		min-width: 0;
		max-width: 100%;
		color: var(--text-secondary);
		font-size: var(--text-xs);
		overflow-wrap: anywhere;
	}

	.cn-live-task__reason :global(p),
	.cn-live-task__reason :global(ul),
	.cn-live-task__reason :global(ol) {
		margin: 0;
	}

	.cn-live-task__reason--clamped {
		display: -webkit-box;
		overflow: hidden;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 3;
		line-clamp: 3;
	}

	/* A control, not more reason text: smaller, accent-tinted (mixed toward
	   the ink so it keeps contrast at this size), with a chevron. */
	.cn-live-task__more {
		justify-self: start;
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		margin-top: 0.1rem;
		padding: 0;
		border: 0;
		background: transparent;
		color: color-mix(in srgb, var(--accent-primary) 70%, var(--text-primary));
		font: inherit;
		font-size: 0.7rem;
		font-weight: 600;
		letter-spacing: 0.01em;
		cursor: pointer;
	}

	.cn-live-task__more:hover {
		color: var(--accent-primary-hover, var(--accent-primary));
	}

	.cn-live-task__more:focus-visible {
		outline: none;
		border-radius: var(--radius-sm);
		box-shadow: var(--ring-focus);
	}

	.cn-live-task__details-inner {
		display: grid;
		gap: 0.75rem;
		min-width: 0;
		padding: 0.7rem 0.9rem 0.8rem;
		border-top: 1px solid color-mix(in srgb, var(--cn-tint) 60%, var(--border-default));
		/* Replays each time the region is shown (display none → grid). */
		animation: cn-live-reveal var(--dur-base) var(--spring-gentle);
	}

	.cn-live-task__details-inner[hidden],
	.cn-live-task__activity[hidden] {
		display: none;
	}

	.cn-live-task__activity {
		min-width: 0;
	}

	@keyframes cn-live-reveal {
		from {
			opacity: 0;
			transform: translateY(-4px);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}

	.cn-live-task__runs {
		display: grid;
		gap: 0.3rem;
		margin: 0;
		padding: 0;
		list-style: none;
		font-size: var(--text-xs);
	}

	.cn-live-task__run {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		min-width: 0;
	}

	.cn-live-task__run-dot {
		flex: none;
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 50%;
		background: var(--cn-tint);
		box-shadow: inset 0 0 0 1px var(--border-default);
	}

	.cn-live-task__run[data-status='done'] .cn-live-task__run-dot {
		background: var(--status-completed);
	}

	.cn-live-task__run[data-status='running'] .cn-live-task__run-dot {
		background: var(--status-running);
	}

	.cn-live-task__run[data-status='failed'] .cn-live-task__run-dot {
		background: var(--color-error);
	}

	.cn-live-task__run[data-status='waiting'] .cn-live-task__run-dot {
		background: var(--color-warning);
	}

	.cn-live-task__run-label {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.cn-live-task__run-status {
		flex: none;
		margin-inline-start: auto;
		color: var(--text-muted);
	}

	.cn-live-task__outputs {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		min-width: 0;
	}

	/* In the body, under the header: same inset as the header. */
	.cn-live-task__outputs--body {
		padding: 0 var(--cn-live-pad) 0.75rem;
	}

	.cn-live-task__outputs--body :global(.chat-rich-blocks) {
		margin-top: 0;
	}

	.cn-live-task__more-outputs {
		align-self: flex-start;
	}

	.cn-live-task__run-caption {
		margin: 0.2rem 0 0;
		color: var(--text-secondary);
		font-size: var(--text-xs);
	}

	.cn-live-task__confirm-text {
		margin: 0;
		font-size: var(--text-sm);
		font-weight: 600;
	}

	.cn-live-task__btn {
		padding: 0.3rem 0.8rem;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-chip);
		background: var(--bg-card);
		color: var(--text-primary);
		font: inherit;
		font-size: var(--text-xs);
		cursor: pointer;
	}

	.cn-live-task__btn:focus-visible {
		outline: none;
		box-shadow: var(--ring-focus);
	}

	.cn-live-task__btn:disabled {
		opacity: 0.6;
		cursor: default;
	}

	.cn-live-task__btn--danger {
		border-color: var(--color-error);
		background: color-mix(in srgb, var(--color-error) 14%, var(--bg-card));
		color: var(--color-error);
		font-weight: 600;
	}

	@media (prefers-reduced-motion: reduce) {
		.cn-live-task,
		.cn-live-task__chevron,
		.cn-live-task__seg,
		.cn-live-task__fill {
			transition: none;
		}

		.cn-live-task__confirm,
		.cn-live-task__details-inner {
			animation: none;
		}

		.cn-live-task__dot,
		.cn-live-task__dot[data-status='running'],
		.cn-live-task__dot[data-status='waiting'],
		.cn-live-task__dot--pop {
			animation: none;
		}
	}

	/* Running: a slow breath, scale plus a soft glow. */
	@keyframes cn-live-breathe {
		0%,
		100% {
			transform: scale(1);
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--cn-live-accent) 0%, transparent);
		}
		50% {
			transform: scale(1.2);
			box-shadow: 0 0 0 4px color-mix(in srgb, var(--cn-live-accent) 28%, transparent);
		}
	}

	/* Waiting on the user: a gentle opacity pulse with a ring. */
	@keyframes cn-live-pulse {
		0%,
		100% {
			opacity: 1;
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--cn-live-accent) 45%, transparent);
		}
		50% {
			opacity: 0.55;
			box-shadow: 0 0 0 5px color-mix(in srgb, var(--cn-live-accent) 0%, transparent);
		}
	}

	/* One-shot pop of the green dot on a live transition into completed. */
	@keyframes cn-live-pop {
		0% {
			transform: scale(1);
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--cn-live-accent) 60%, transparent);
		}
		45% {
			transform: scale(1.6);
			box-shadow: 0 0 0 6px color-mix(in srgb, var(--cn-live-accent) 30%, transparent);
		}
		100% {
			transform: scale(1);
			box-shadow: 0 0 0 10px color-mix(in srgb, var(--cn-live-accent) 0%, transparent);
		}
	}
</style>
