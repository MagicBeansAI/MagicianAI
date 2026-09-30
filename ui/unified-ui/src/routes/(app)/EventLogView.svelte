<script lang="ts">
	import { onDestroy, tick } from 'svelte';
	import type { V2WebSocketEvent } from '$lib/realtime/v2-websocket';
	import Tabs from '$lib/magician/components/native/Tabs.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Checkbox from '$lib/magician/components/native/Checkbox.svelte';
	import Table from '$lib/magician/components/native/Table.svelte';
	import Tag from '$lib/magician/components/native/Tag.svelte';
	import EmptyState from '$lib/magician/components/native/EmptyState.svelte';
	import Tooltip from '$lib/magician/components/native/Tooltip.svelte';

	export let events: V2WebSocketEvent[] = [];
	export let activeLLMCalls: { correlationId: string; provider: string; startTime: number; queryLength: number }[] = [];
	export let llmCallsCompleted = 0;
	export let llmCallsFailed = 0;

	const LLM_EVENT_TYPES = new Set([
		'LLMAnalysisStarted', 'LLMAnalysisCompleted', 'LLMAnalysisFailed',
		'LLMRequestSent', 'LLMResponseReceived'
	]);

	let activeTabIndex = 0;
	let eventFilter: 'all' | 'llm' = 'all';
	let autoScroll = true;
	let displayEvents: V2WebSocketEvent[] = [];
	let displayCleared = false;
	let scrollContainer: HTMLElement;
	let now = Date.now();
	let timerInterval: ReturnType<typeof setInterval> | null = null;

	const tabs = [
		{ label: 'Events' },
		{ label: 'LLM Calls' }
	];

	// Sync display events from props (unless user cleared)
	$: if (!displayCleared) {
		displayEvents = eventFilter === 'llm'
			? events.filter(e => LLM_EVENT_TYPES.has(e.event_type))
			: [...events];
	}

	// Active LLM timer
	$: if (activeLLMCalls.length > 0 && !timerInterval) {
		timerInterval = setInterval(() => { now = Date.now(); }, 1000);
	}
	$: if (activeLLMCalls.length === 0 && timerInterval) {
		clearInterval(timerInterval); timerInterval = null;
	}

	// Auto-scroll on new events
	$: if (displayEvents.length && autoScroll && scrollContainer) {
		tick().then(() => {
			if (scrollContainer) scrollContainer.scrollTop = scrollContainer.scrollHeight;
		});
	}

	// Recent LLM events (computed once for the LLM Calls tab)
	$: recentLLMEvents = events.filter(e => LLM_EVENT_TYPES.has(e.event_type)).slice(-50);

	// Table rows for active LLM calls
	$: llmTableRows = activeLLMCalls.map(call => ({
		provider: call.provider,
		queryLength: String(call.queryLength),
		elapsed: formatDuration(now - call.startTime)
	}));

	function clearDisplay() { displayEvents = []; displayCleared = true; }

	function formatDuration(ms: number): string {
		if (ms < 1000) return `${ms}ms`;
		const s = Math.floor(ms / 1000);
		if (s < 60) return `${s}s`;
		return `${Math.floor(s / 60)}m ${s % 60}s`;
	}

	function getEventTag(type: string): { text: string; color: 'success' | 'error' | 'warning' | 'info' | 'default' } {
		if (type.includes('Completed') || type.includes('success')) return { text: type, color: 'success' };
		if (type.includes('Failed') || type.includes('Error')) return { text: type, color: 'error' };
		if (type.includes('Warning') || type.includes('Dropped')) return { text: type, color: 'warning' };
		if (type.includes('LLM')) return { text: type, color: 'info' };
		return { text: type, color: 'default' };
	}

	function getEventDetail(event: V2WebSocketEvent): string {
		const d = event.data as Record<string, unknown>;
		if (d.provider) return `Provider: ${d.provider}`;
		if (d.capability) return `Capability: ${d.capability}`;
		if (d.reason) return String(d.reason);
		if (d.decision_summary) return String(d.decision_summary);
		if (d.request_summary) return String(d.request_summary);
		if (d.error_message) return String(d.error_message);
		return '';
	}

	onDestroy(() => {
		if (timerInterval) { clearInterval(timerInterval); timerInterval = null; }
	});
</script>

<div class="event-log-view">
	<Tabs {tabs} bind:activeIndex={activeTabIndex}>
		<svelte:fragment let:activeIndex>
			{#if activeIndex === 0}
				<!-- Events Tab -->
				<div class="events-tab">
					<!-- Controls bar -->
					<div class="controls-bar">
						<div class="flex items-center gap-2">
							<div class="flex items-center rounded-lg p-0.5" style="background: var(--bg-soft);">
								<Button label="All" variant={eventFilter === 'all' ? 'secondary' : 'outline'} size="sm"
									on:click={() => { eventFilter = 'all'; displayCleared = false; }} />
								<Button label="LLM" variant={eventFilter === 'llm' ? 'secondary' : 'outline'} size="sm"
									on:click={() => { eventFilter = 'llm'; displayCleared = false; }} />
							</div>
						</div>
						<div class="flex items-center gap-3">
							<Checkbox label="Auto-scroll" checked={autoScroll}
								on:change={(e) => autoScroll = e.detail.checked} />
							<Button label="CLR" variant="outline" size="sm" on:click={clearDisplay} />
						</div>
					</div>

					<!-- Event list -->
					<div class="event-list" bind:this={scrollContainer}>
						{#if displayEvents.length === 0}
							<EmptyState title="No events" description="Events will appear here as execution progresses" />
						{:else}
							{#each displayEvents as event}
								{@const tag = getEventTag(event.event_type)}
								{@const detail = getEventDetail(event)}
								<div class="event-row">
									<span class="event-time">{new Date(((event.data as Record<string, unknown>).timestamp as number) || Date.now()).toLocaleTimeString()}</span>
									<Tag text={tag.text} color={tag.color} />
									{#if detail}
										<Tooltip content={detail} position="right">
											<span class="event-detail">{detail}</span>
										</Tooltip>
									{/if}
								</div>
							{/each}
						{/if}
					</div>
				</div>

			{:else if activeIndex === 1}
				<!-- LLM Calls Tab -->
				<div class="llm-tab">
					<!-- Summary badges -->
					<div class="flex items-center gap-2 mb-3">
						<Badge text="{llmCallsCompleted} completed" color="success" />
						{#if llmCallsFailed > 0}
							<Badge text="{llmCallsFailed} failed" color="error" />
						{/if}
						{#if activeLLMCalls.length > 0}
							<Badge text="{activeLLMCalls.length} active" color="warning" />
						{/if}
					</div>

					<!-- Active LLM calls table -->
					{#if activeLLMCalls.length > 0}
						<h3 class="section-heading">Active Calls</h3>
						<Table
							columns={[
								{ key: 'provider', label: 'Provider' },
								{ key: 'queryLength', label: 'Query Length' },
								{ key: 'elapsed', label: 'Elapsed' }
							]}
							rows={llmTableRows}
							presorted={true}
						/>
					{/if}

					<!-- Recent LLM events from deep history -->
					<h3 class="section-heading">Recent LLM Events</h3>
					{#if recentLLMEvents.length === 0}
						<EmptyState title="No LLM events" description="LLM call events will appear here" />
					{:else}
						<div class="event-list llm-event-list">
							{#each recentLLMEvents as event}
								{@const tag = getEventTag(event.event_type)}
								{@const detail = getEventDetail(event)}
								<div class="event-row">
									<span class="event-time">{new Date(((event.data as Record<string, unknown>).timestamp as number) || Date.now()).toLocaleTimeString()}</span>
									<Tag text={tag.text} color={tag.color} />
									{#if detail}
										<span class="event-detail">{detail}</span>
									{/if}
								</div>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
		</svelte:fragment>
	</Tabs>
</div>

<style>
	.event-log-view {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}
	/* Override tabs panel padding so events-tab controls its own padding */
	.event-log-view :global(.native-tabs) {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.event-log-view :global(.native-tabs__panel) {
		flex: 1;
		min-height: 0;
		overflow: hidden;
		padding: 0;
	}
	.events-tab, .llm-tab {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		padding: 12px 20px;
	}
	.controls-bar {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 12px;
		flex-shrink: 0;
	}
	.event-list {
		flex: 1;
		overflow-y: auto;
		min-height: 0;
		font-family: var(--font-mono);
		font-size: 0.8rem;
	}
	.llm-event-list {
		max-height: 400px;
		flex: unset;
	}
	.event-row {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 4px 8px;
		border-bottom: 1px solid var(--border-soft);
	}
	.event-row:hover {
		background: var(--bg-soft);
	}
	.event-time {
		color: var(--text-muted);
		font-size: 0.75rem;
		flex-shrink: 0;
		width: 80px;
	}
	.event-detail {
		color: var(--text-secondary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
	}
	.section-heading {
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--text-primary);
		margin: 16px 0 8px;
	}
</style>
