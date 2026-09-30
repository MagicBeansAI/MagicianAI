<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { v2Events, type V2WebSocketEvent, getV2EventSequence } from '$lib/realtime/v2-websocket';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

	export let executionId: string | null = null;

	let extractionStarted = false;
	let slotsExtracted = 0;
	let enrichmentRunning = false;
	let enrichmentComplete = false;
	let clarifiedTaskReady = false;
	let messageLength = 0;
	let enricherCount = 0;
	let slotsChanged = 0;
	let totalSlots = 0;
	let previousExecutionId: string | null = null;
	let previousScopeKey = '';

	let unsubscribe: (() => void) | null = null;
	let lastProcessedEventSequence = 0;
	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;

	function resetState() {
		extractionStarted = false;
		slotsExtracted = 0;
		enrichmentRunning = false;
		enrichmentComplete = false;
		clarifiedTaskReady = false;
		messageLength = 0;
		enricherCount = 0;
		slotsChanged = 0;
		totalSlots = 0;
	}

	function takeUnprocessedEvents(events: V2WebSocketEvent[]): V2WebSocketEvent[] {
		if (events.length === 0) {
			return [];
		}
		const nextEvents = events.filter((event) => getV2EventSequence(event) > lastProcessedEventSequence);
		if (nextEvents.length > 0) {
			lastProcessedEventSequence = getV2EventSequence(nextEvents[nextEvents.length - 1]);
		}
		return nextEvents;
	}

	onMount(() => {
		// Subscribe to V2 WebSocket events
		unsubscribe = v2Events.subscribe((events: V2WebSocketEvent[]) => {
			const nextEvents = takeUnprocessedEvents(events);
			if (nextEvents.length === 0) return;
			// Filter events for current execution
			const executionEvents = nextEvents.filter(
				(event) => 'execution_id' in event.data && event.data.execution_id === executionId
			);

			for (const event of executionEvents) {
				switch (event.event_type) {
					case 'SlotExtractionStarted':
						extractionStarted = true;
						slotsExtracted = 0;
						messageLength = event.data.message_length;
						enrichmentRunning = false;
						enrichmentComplete = false;
						clarifiedTaskReady = false;
						break;

					case 'SlotExtracted':
						slotsExtracted++;
						break;

					case 'SlotEnrichmentStarted':
						enrichmentRunning = true;
						enricherCount = event.data.enricher_count;
						totalSlots = event.data.total_slots;
						break;

					case 'SlotEnrichmentCompleted':
						enrichmentRunning = false;
						enrichmentComplete = true;
						slotsChanged = event.data.slots_changed;
						break;

					case 'ClarifiedTaskReady':
						clarifiedTaskReady = true;
						break;
				}
			}
		});
	});

	onDestroy(() => {
		if (unsubscribe) {
			unsubscribe();
		}
	});

	$: if (executionId !== previousExecutionId || currentScopeKey !== previousScopeKey) {
		resetState();
		previousExecutionId = executionId;
		previousScopeKey = currentScopeKey;
		lastProcessedEventSequence = 0;
	}

	$: hasActivity = extractionStarted || enrichmentRunning || enrichmentComplete || clarifiedTaskReady;
</script>

{#if hasActivity}
	<div class="slot-progress-panel" class:active={extractionStarted || enrichmentRunning}>
		<div class="progress-header">
			<span class="header-icon">🔍</span>
			<span class="header-text">Slot Extraction Progress</span>
		</div>

		<div class="progress-indicators">
			{#if extractionStarted}
				<div class="indicator" class:active={true}>
					<div class="indicator-icon pulse">🔍</div>
					<div class="indicator-content">
						<div class="indicator-label">Extracting slots from message</div>
						<div class="indicator-meta">
							{slotsExtracted} slot(s) extracted · {messageLength} chars processed
						</div>
					</div>
				</div>
			{/if}

			{#if enrichmentRunning}
				<div class="indicator" class:active={true}>
					<div class="indicator-icon pulse">⚙️</div>
					<div class="indicator-content">
						<div class="indicator-label">Enriching slots</div>
						<div class="indicator-meta">
							{enricherCount} enricher(s) running · {totalSlots} slot(s)
						</div>
					</div>
				</div>
			{/if}

			{#if enrichmentComplete}
				<div class="indicator" class:complete={true}>
					<div class="indicator-icon">✅</div>
					<div class="indicator-content">
						<div class="indicator-label">Enrichment complete</div>
						<div class="indicator-meta">{slotsChanged} slot(s) changed</div>
					</div>
				</div>
			{/if}

			{#if clarifiedTaskReady}
				<div class="indicator" class:complete={true}>
					<div class="indicator-icon">✨</div>
					<div class="indicator-content">
						<div class="indicator-label">Clarified task generated</div>
					</div>
				</div>
			{/if}
		</div>
	</div>
{/if}

<style>
	.slot-progress-panel {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 0.75rem 0.85rem;
		background: rgba(59, 130, 246, 0.06);
		border: 1px solid rgba(59, 130, 246, 0.2);
		border-radius: 10px;
		transition: all 0.3s ease;
	}

	.slot-progress-panel.active {
		background: rgba(59, 130, 246, 0.1);
		border-color: rgba(59, 130, 246, 0.3);
		box-shadow: 0 2px 8px rgba(59, 130, 246, 0.15);
	}

	.progress-header {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.header-icon {
		font-size: 1rem;
	}

	.header-text {
		font-size: 0.85rem;
		font-weight: 600;
		color: #0f172a;
	}

	.progress-indicators {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.indicator {
		display: flex;
		align-items: flex-start;
		gap: 0.6rem;
		padding: 0.6rem 0.75rem;
		background: #ffffff;
		border: 1px solid #e2e8f0;
		border-radius: 8px;
		opacity: 0.7;
		transition: all 0.3s ease;
	}

	.indicator.active {
		opacity: 1;
		border-color: rgba(59, 130, 246, 0.4);
		background: rgba(59, 130, 246, 0.05);
	}

	.indicator.complete {
		opacity: 1;
		border-color: rgba(34, 197, 94, 0.3);
		background: rgba(34, 197, 94, 0.05);
	}

	.indicator-icon {
		font-size: 1.1rem;
		flex-shrink: 0;
		width: 24px;
		height: 24px;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.indicator-icon.pulse {
		animation: pulse 2s ease-in-out infinite;
	}

	@keyframes pulse {
		0%,
		100% {
			transform: scale(1);
			opacity: 1;
		}
		50% {
			transform: scale(1.1);
			opacity: 0.8;
		}
	}

	.indicator-content {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.indicator-label {
		font-size: 0.85rem;
		font-weight: 600;
		color: #0f172a;
	}

	.indicator-meta {
		font-size: 0.75rem;
		color: #64748b;
	}

	@media (max-width: 640px) {
		.slot-progress-panel {
			padding: 0.65rem;
		}

		.indicator {
			padding: 0.5rem 0.6rem;
		}

		.header-text {
			font-size: 0.8rem;
		}

		.indicator-label {
			font-size: 0.8rem;
		}

		.indicator-meta {
			font-size: 0.7rem;
		}
	}
</style>
