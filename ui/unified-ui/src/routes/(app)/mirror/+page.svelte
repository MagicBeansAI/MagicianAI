<script lang="ts">
	import { onDestroy } from 'svelte';
	import { browser } from '$app/environment';
	import { page } from '$app/stores';
	import { v2Events, type V2WebSocketEvent, getV2EventSequence, type ConnectionStatus } from '$lib/realtime/v2-websocket';
	import { matchesEventFilter, type EventFilterContext } from '$lib/realtime/event-filter';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import EmptyState from '$lib/magician/components/generative/EmptyState.svelte';
	import EventLogView from '../EventLogView.svelte';

	// Connection status for loading indicator
	let connectionStatus: ConnectionStatus = 'disconnected';
	const unsubStatus = v2Events.connectionStatus.subscribe(s => connectionStatus = s);

	const PAGE_HISTORY_LIMIT = 5000;

	// From URL params
	$: executionId = $page.url.searchParams.get('execution_id') || '';
	$: agentId = $page.url.searchParams.get('agent_id') || undefined;
	$: goalId = $page.url.searchParams.get('goal_id') || undefined;
	$: cycleId = $page.url.searchParams.get('cycle_id') || undefined;
	$: currentMirrorScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;

	// Valid filter = either a real execution id or a complete agent triple
	$: hasValidFilter = !!executionId || (!!agentId && !!goalId && !!cycleId);

	// Shared filter context — same logic as ExecutionPanel.processExecutionEvents
	$: filterCtx = { executionId, agentId, goalId, cycleId } as EventFilterContext;

	// Event accumulation state
	let allEvents: V2WebSocketEvent[] = [];
	let activeLLMCalls: { correlationId: string; provider: string; startTime: number; queryLength: number }[] = [];
	let llmCallsCompleted = 0;
	let llmCallsFailed = 0;
	let lastProcessedSeq = 0;
	let eventUnsubscribe: (() => void) | null = null;
	// Key used to force-remount EventLogView on clearAll (resets internal displayCleared)
	let viewKey = 0;

	// Track what we connected/subscribed for so we can detect param changes
	let connectedExecutionId = '';
	let connectedAgentId: string | undefined;
	let connectedGoalId: string | undefined;
	let connectedCycleId: string | undefined;
	let connectedScopeKey = '';

	function teardown() {
		if (eventUnsubscribe) { eventUnsubscribe(); eventUnsubscribe = null; }
	}

	function resetAccumulators() {
		allEvents = [];
		activeLLMCalls = [];
		llmCallsCompleted = 0;
		llmCallsFailed = 0;
		lastProcessedSeq = 0;
		viewKey++;
	}

	function setupSubscription(
		tid: string,
		aid: string | undefined,
		gid: string | undefined,
		cid: string | undefined,
		scopeKey: string
	) {
		teardown();
		resetAccumulators();
		connectedExecutionId = tid;
		connectedAgentId = aid;
		connectedGoalId = gid;
		connectedCycleId = cid;
		connectedScopeKey = scopeKey;

		// Connect scoped to this execution so the server re-emits pending
		// paused-execution events on reconnect. Safe to call unconditionally:
		// in a new tab the singleton is fresh; connect() is internally
		// idempotent for the same target.
		v2Events.connect(tid || undefined, aid);

		eventUnsubscribe = v2Events.subscribe((events) => {
			if (scopeKey !== currentMirrorScopeKey) {
				return;
			}
			const newEvents = events.filter(e =>
				getV2EventSequence(e) > lastProcessedSeq && matchesEventFilter(e, filterCtx)
			);
			if (newEvents.length === 0) return;
			lastProcessedSeq = Math.max(lastProcessedSeq, ...newEvents.map(getV2EventSequence));

			// Track active LLM calls
			for (const event of newEvents) {
				if (event.event_type === 'LLMAnalysisStarted') {
					activeLLMCalls = [...activeLLMCalls, {
						correlationId: event.data.correlation_id,
						provider: event.data.provider,
						startTime: event.data.timestamp || Date.now(),
						queryLength: event.data.query_length
					}];
				} else if (event.event_type === 'LLMAnalysisCompleted') {
					activeLLMCalls = activeLLMCalls.filter(
						c => c.correlationId !== event.data.correlation_id
					);
					llmCallsCompleted++;
				} else if (event.event_type === 'LLMAnalysisFailed') {
					activeLLMCalls = activeLLMCalls.filter(
						c => c.correlationId !== event.data.correlation_id
					);
					llmCallsFailed++;
				}
			}

			// Accumulate all execution-matched events, not just LLM activity.
			allEvents = [...allEvents, ...newEvents];
			if (allEvents.length > PAGE_HISTORY_LIMIT) {
				allEvents = allEvents.slice(-PAGE_HISTORY_LIMIT);
			}
		});
	}

	// Reactive: (re-)subscribe when any filter param changes
	$: if (browser && hasValidFilter && (
		currentMirrorScopeKey !== connectedScopeKey
		|| executionId !== connectedExecutionId
		|| agentId !== connectedAgentId
		|| goalId !== connectedGoalId
		|| cycleId !== connectedCycleId
	)) {
		setupSubscription(executionId, agentId, goalId, cycleId, currentMirrorScopeKey);
	}

	$: if (browser && !hasValidFilter && (
		connectedScopeKey !== ''
		|| connectedExecutionId !== ''
		|| connectedAgentId !== undefined
		|| connectedGoalId !== undefined
		|| connectedCycleId !== undefined
	)) {
		teardown();
		resetAccumulators();
		connectedExecutionId = '';
		connectedAgentId = undefined;
		connectedGoalId = undefined;
		connectedCycleId = undefined;
		connectedScopeKey = '';
	}

	onDestroy(() => {
		teardown();
		unsubStatus();
		// Do NOT disconnect v2Events — other components share the singleton
	});

	function clearAll() {
		// Teardown + re-setup: the new subscription starts with lastProcessedSeq=0
		// but then immediately processes the current store snapshot, advancing the
		// high-water mark. This means only truly NEW events after this point appear.
		// Actually — resetAccumulators sets lastProcessedSeq=0 and the Svelte store
		// fires immediately on subscribe with buffered events. To skip those, record
		// the current high-water mark first, then reset display state.
		const highWater = lastProcessedSeq;
		allEvents = [];
		activeLLMCalls = [];
		llmCallsCompleted = 0;
		llmCallsFailed = 0;
		lastProcessedSeq = highWater;
		// Force-remount EventLogView to reset its internal displayCleared flag
		viewKey++;
	}

	// Display label: show execution id if it's a real ID, otherwise show agent context
	$: displayLabel = executionId || (agentId ? `agent: ${agentId}` : '');
</script>

<svelte:head><title>Event Log &middot; Magican</title></svelte:head>

<div class="mirror-page presto-gaui-page">
	{#if !hasValidFilter}
		<EmptyState title="No execution specified"
			description="Open this page from the execution log expand button" />
	{:else}
		<header class="mirror-header">
			<div class="flex items-center gap-3">
				<h1 class="mirror-title">Event Log</h1>
				{#if connectionStatus === 'connecting'}
					<Badge text="Connecting..." color="warning" />
				{:else if connectionStatus === 'connected'}
					<Badge text="{allEvents.length} events" color="info" />
				{:else}
					<Badge text="Disconnected" color="error" />
				{/if}
				{#if activeLLMCalls.length > 0}
					<Badge text="{activeLLMCalls.length} active LLM" color="warning" />
				{/if}
			</div>
			<div class="flex items-center gap-3">
				<span class="mirror-execution-id">{displayLabel}</span>
				<Button label="Clear All" variant="outline" size="sm" on:click={clearAll} />
			</div>
		</header>
		<div class="mirror-body">
			{#if connectionStatus === 'connecting' && allEvents.length === 0}
				<div class="mirror-connecting">
					<span class="connecting-dot"></span>
					<span class="connecting-text">Connecting to event stream...</span>
				</div>
			{:else}
				{#key viewKey}
					<EventLogView events={allEvents} {activeLLMCalls} {llmCallsCompleted} {llmCallsFailed} />
				{/key}
			{/if}
		</div>
	{/if}
</div>

<style>
	.mirror-page {
		display: flex;
		flex-direction: column;
		height: 100vh;
	}
	.mirror-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 16px 20px;
		border-bottom: 1px solid var(--border-soft);
		flex-shrink: 0;
		background: var(--bg-card);
	}
	.mirror-title {
		color: var(--text-primary);
		font-family: var(--font-display);
		font-size: 1.25rem;
		font-weight: 700;
		margin: 0;
	}
	.mirror-execution-id {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--text-muted);
		background: var(--bg-soft);
		padding: 4px 10px;
		border-radius: var(--radius-sm);
		user-select: all;
	}
	.mirror-body {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}
	.mirror-connecting {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 10px;
	}
	.connecting-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--accent-primary);
		animation: pulse 1.2s ease-in-out infinite;
	}
	.connecting-text {
		font-size: 0.85rem;
		color: var(--text-muted);
	}
	@keyframes pulse {
		0%, 100% { opacity: 0.3; }
		50% { opacity: 1; }
	}
</style>
