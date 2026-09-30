<script lang="ts">
	import { createEventDispatcher, onMount, onDestroy } from 'svelte';
	import { v2Events, type V2WebSocketEvent, getV2EventSequence } from '$lib/realtime/v2-websocket';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Select from '$lib/magician/components/native/Select.svelte';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import SlotDetailModal from './SlotDetailModal.svelte';
	import EnrichmentPipelineStatus from './EnrichmentPipelineStatus.svelte';
	import SlotTimelineView from './SlotTimelineView.svelte';
	import type {
		SlotRecord,
		ProvenanceRecord,
		ConfidenceSummary,
		EnrichmentSummary
	} from '$lib/types/mission_control';

	export let slots: SlotRecord[] = [];
	export let viewMode: 'friendly' | 'researcher' = 'friendly';
	export let confidenceSummary: ConfidenceSummary | null = null;
	export let enrichmentSummary: EnrichmentSummary | null = null;
	export let executionId: string | null = null;
	export let forcedView: 'grid' | 'timeline' | null = null; // Force a specific view (hides toggle)
	export let compact: boolean = false; // Compact mode for smaller elements

	const dispatch = createEventDispatcher<{
		viewSlotDetail: { slot: SlotRecord };
	}>();

	let selectedSlot: SlotRecord | null = null;
	let showModal = false;
	let displayView: 'grid' | 'timeline' = 'grid'; // Default to grid view
	let eventTypeFilter = 'all';
	let slotTypeFilter = 'modifier';

	// Use forced view if provided, otherwise use internal state
	$: activeView = forcedView || displayView;

	type SlotType =
		| 'entity'
		| 'temporal'
		| 'spatial'
		| 'emotion'
		| 'action'
		| 'modifier'
		| 'resource'
		| 'status';
	type ProvenanceSource =
		| 'LlmPrimary'
		| 'UserReply'
		| 'ScreenshotInference'
		| 'DeterministicCheck'
		| 'MemoryLookup';

	type SlotGroup = {
		type: string;
		slots: SlotRecord[];
		count: number;
		avgConfidence: number;
	};

	// Simplified slot type config - theme-coherent, no icons
	const SLOT_TYPE_CONFIG: Record<string, { label: string }> = {
		entity: { label: 'Entity' },
		temporal: { label: 'Temporal' },
		spatial: { label: 'Spatial' },
		emotion: { label: 'Emotion' },
		action: { label: 'Action' },
		modifier: { label: 'Modifier' },
		resource: { label: 'Resource' },
		status: { label: 'Status' }
	};

	const PROVENANCE_CONFIG: Record<ProvenanceSource, { label: string; icon: string }> = {
		LlmPrimary: { label: 'LLM Primary', icon: '🤖' },
		UserReply: { label: 'User Reply', icon: '💬' },
		ScreenshotInference: { label: 'Screenshot', icon: '📸' },
		DeterministicCheck: { label: 'Deterministic', icon: '✓' },
		MemoryLookup: { label: 'Memory', icon: '🧠' }
	};

	let expandedTypes = new Set<string>();

	// Track recently extracted/updated slots for animations
	let recentlyExtracted = new Set<string>();
	let recentlyUpdated = new Set<string>();
	let unsubscribe: (() => void) | null = null;
	let timeoutIds: ReturnType<typeof setTimeout>[] = [];
	let previousExecutionId: string | null = null;
	let previousScopeKey = '';
	let lastProcessedEventSequence = 0;
	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;

	$: isFriendlyView = viewMode === 'friendly';
	$: isResearcherView = viewMode === 'researcher';
	$: hasSlots = slots.length > 0;
	$: overallConfidence = confidenceSummary?.overall ?? 0;
	$: unresolvedCount = confidenceSummary?.unresolved_slots?.length ?? 0;

	$: slotGroups = computeSlotGroups(slots);

	// Auto-expand all groups in researcher mode
	$: if (isResearcherView) {
		expandedTypes = new Set(slotGroups.map((g) => g.type));
	} else if (isFriendlyView) {
		expandedTypes = new Set();
	}

	function computeSlotGroups(slots: SlotRecord[]): SlotGroup[] {
		const groups = new Map<string, SlotRecord[]>();

		for (const slot of slots) {
			const slotType = String(slot.slot_type).toLowerCase();
			const existing = groups.get(slotType) || [];
			existing.push(slot);
			groups.set(slotType, existing);
		}

		const result: SlotGroup[] = [];
		for (const [type, typeSlots] of groups.entries()) {
			const avgConfidence = typeSlots.reduce((sum, s) => sum + s.confidence, 0) / typeSlots.length;
			result.push({
				type,
				slots: typeSlots,
				count: typeSlots.length,
				avgConfidence
			});
		}

		// Sort by count descending
		result.sort((a, b) => b.count - a.count);
		return result;
	}

	function toggleGroup(type: string) {
		if (expandedTypes.has(type)) {
			expandedTypes.delete(type);
		} else {
			expandedTypes.add(type);
		}
		expandedTypes = expandedTypes; // Trigger reactivity
	}

	function getConfidenceClass(confidence: number): string {
		if (confidence >= 0.8) return 'confidence-high';
		if (confidence >= 0.5) return 'confidence-medium';
		return 'confidence-low';
	}

	function getConfidenceLabel(confidence: number): string {
		// Just return the percentage - CSS class handles the color styling
		return formatPercent(confidence);
	}

	function formatPercent(value: number | null | undefined): string {
		if (value === null || value === undefined || Number.isNaN(value)) return '—';
		return `${(value * 100).toFixed(1)}%`;
	}

	function formatTimestamp(timestamp: number): string {
		return new Date(timestamp).toLocaleString();
	}

	function formatValue(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'object' && value !== null) {
			return JSON.stringify(value, null, 2);
		}
		return String(value);
	}

	function handleSlotClick(slot: SlotRecord) {
		selectedSlot = slot;
		showModal = true;
		dispatch('viewSlotDetail', { slot });
	}

	function handleModalClose() {
		showModal = false;
		// Keep selectedSlot for a smooth fade-out
		setTimeout(() => {
			if (!showModal) {
				selectedSlot = null;
			}
		}, 300);
	}

	function clearHighlights() {
		recentlyExtracted = new Set<string>();
		recentlyUpdated = new Set<string>();
	}

	function clearTimeouts() {
		for (const timeoutId of timeoutIds) {
			clearTimeout(timeoutId);
		}
		timeoutIds = [];
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

	// Subscribe to WebSocket events for animations
	onMount(() => {
		unsubscribe = v2Events.subscribe((events: V2WebSocketEvent[]) => {
			const nextEvents = takeUnprocessedEvents(events);
			if (nextEvents.length === 0) return;
			const executionEvents = nextEvents.filter(
				(event) => 'execution_id' in event.data && event.data.execution_id === executionId
			);

			for (const event of executionEvents) {
				switch (event.event_type) {
					case 'SlotExtracted': {
						// Mark slot as recently extracted
						const slotKey = event.data.slot_id ?? event.data.slot_name;
						recentlyExtracted.add(slotKey);
						recentlyExtracted = recentlyExtracted; // Trigger reactivity

						// Remove animation after 3 seconds
						const timeoutId = setTimeout(() => {
							recentlyExtracted.delete(slotKey);
							recentlyExtracted = recentlyExtracted;
							timeoutIds = timeoutIds.filter((id) => id !== timeoutId);
						}, 3000);
						timeoutIds.push(timeoutId);
						break;
					}

					case 'SlotConfidenceUpdated': {
						// Mark slot as recently updated
						const slotKey = event.data.slot_id ?? event.data.slot_name;
						recentlyUpdated.add(slotKey);
						recentlyUpdated = recentlyUpdated; // Trigger reactivity

						// Remove animation after 2 seconds
						const timeoutId = setTimeout(() => {
							recentlyUpdated.delete(slotKey);
							recentlyUpdated = recentlyUpdated;
							timeoutIds = timeoutIds.filter((id) => id !== timeoutId);
						}, 2000);
						timeoutIds.push(timeoutId);
						break;
					}
				}
			}
		});
	});

	onDestroy(() => {
		if (unsubscribe) {
			unsubscribe();
		}
		clearTimeouts();
	});

	$: if (executionId !== previousExecutionId || currentScopeKey !== previousScopeKey) {
		clearTimeouts();
		clearHighlights();
		showModal = false;
		selectedSlot = null;
		previousExecutionId = executionId;
		previousScopeKey = currentScopeKey;
		lastProcessedEventSequence = 0;
	}

	function isSlotRecentlyExtracted(slotId: string): boolean {
		return recentlyExtracted.has(slotId);
	}

	function isSlotRecentlyUpdated(slotId: string): boolean {
		return recentlyUpdated.has(slotId);
	}

	function expandAll() {
		expandedTypes = new Set(slotGroups.map((g) => g.type));
	}

	function collapseAll() {
		expandedTypes = new Set();
	}
</script>

<div class="slot-graph-inspector">
	{#if !hasSlots}
		<div class="empty">
			<p>No slots extracted yet. Slots will appear once the agent processes your request.</p>
		</div>
	{:else}
		<!-- View Toggle (only shown in researcher mode and when view is not forced) -->
		{#if !isFriendlyView && !forcedView}
			<div class="view-toggle-bar">
				<div class="view-toggle">
					<Button
						label="Grid"
						variant={displayView === 'grid' ? 'primary' : 'outline'}
						size="sm"
						on:click={() => (displayView = 'grid')}
						ariaLabel="Grid view"
					/>
					<Button
						label="Timeline"
						variant={displayView === 'timeline' ? 'primary' : 'outline'}
						size="sm"
						on:click={() => (displayView = 'timeline')}
						ariaLabel="Timeline view"
					/>
				</div>

				{#if activeView === 'grid'}
					<div class="expand-controls">
						<Button label="Expand All" variant="outline" size="sm" title="Expand all groups" on:click={expandAll} />
						<Button label="Collapse All" variant="outline" size="sm" title="Collapse all groups" on:click={collapseAll} />
					</div>
				{:else if activeView === 'timeline'}
					<div class="timeline-filters">
						<Select
							interactive
							options={[
								{value: 'all', label: 'All Events'},
								{value: 'extraction', label: 'Extraction'},
								{value: 'enrichment', label: 'Enrichment'},
								{value: 'confidence_update', label: 'Confidence Updates'}
							]}
							value={eventTypeFilter}
							on:change={(e) => eventTypeFilter = e.detail.value}
							ariaLabel="Filter by event type"
						/>
						<Select
							interactive
							options={[
								{value: 'all', label: 'All Types'},
								...Object.keys(SLOT_TYPE_CONFIG).map(type => ({value: type, label: SLOT_TYPE_CONFIG[type].label}))
							]}
							value={slotTypeFilter}
							on:change={(e) => slotTypeFilter = e.detail.value}
							ariaLabel="Filter by slot type"
						/>
					</div>
				{/if}
			</div>
		{/if}

		<!-- Expand/Collapse controls for forced grid view -->
		{#if !isFriendlyView && forcedView === 'grid'}
			<div class="expand-controls-standalone">
				<Button label="Expand All" variant="outline" size="sm" title="Expand all groups" on:click={expandAll} />
				<Button label="Collapse All" variant="outline" size="sm" title="Collapse all groups" on:click={collapseAll} />
			</div>
		{/if}

		{#if isFriendlyView}
		<!-- Friendly View: Summary only -->
		<div class="friendly-summary">
			<div class="metric-card">
				<span class="label">Slots</span>
				<span class="value">{slots.length}</span>
			</div>
			<div class="metric-card">
				<span class="label">Confidence</span>
				<span class="value {getConfidenceClass(overallConfidence)}">
					{formatPercent(overallConfidence)}
				</span>
			</div>
			{#if unresolvedCount > 0}
				<div class="metric-card warning">
					<span class="label">Unresolved</span>
					<span class="value">{unresolvedCount}</span>
				</div>
			{/if}
		</div>
	{:else if activeView === 'grid'}
		<!-- Operator/Researcher View: Grid - Grouped by type -->
		<div class="slot-groups">
			{#each slotGroups as group (group.type)}
				<div class="group-card">
					<button
						class="group-header"
						on:click={() => toggleGroup(group.type)}
						aria-expanded={expandedTypes.has(group.type)}
					>
						<div class="group-title">
							<span class="type-label">
								{SLOT_TYPE_CONFIG[group.type]?.label || group.type}
							</span>
							<Badge text={String(group.count)} />
						</div>
						<div class="group-meta">
							<span class="avg-confidence {getConfidenceClass(group.avgConfidence)}">
								Avg {formatPercent(group.avgConfidence)}
							</span>
							<span class="expand-icon">
								{expandedTypes.has(group.type) ? '▼' : '▶'}
							</span>
						</div>
					</button>

					{#if expandedTypes.has(group.type)}
						<div class="group-content">
							{#each group.slots as slot (slot.id)}
								<button
									class="slot-card"
									class:slot-extracted={isSlotRecentlyExtracted(slot.id)}
									class:slot-updated={isSlotRecentlyUpdated(slot.id)}
									on:click={() => handleSlotClick(slot)}
								>
									<div class="slot-header">
										<div class="slot-id">{slot.id.split('::')[1] || slot.id}</div>
										<span
											class="confidence-badge {getConfidenceClass(slot.confidence)}"
											class:badge-flash={isSlotRecentlyUpdated(slot.id)}
										>
											{getConfidenceLabel(slot.confidence)}
										</span>
									</div>
									<div class="slot-value">
										{formatValue(slot.value)}
									</div>
									{#if isResearcherView}
										<div class="slot-provenance">
											<div class="provenance-label">Provenance ({slot.provenance.length}):</div>
											{#each slot.provenance as prov, idx (idx)}
												{@const sourceKey = prov.source as ProvenanceSource}
												<div class="provenance-item">
													<span class="prov-icon">{PROVENANCE_CONFIG[sourceKey]?.icon || '📋'}</span>
													<span class="prov-source">{PROVENANCE_CONFIG[sourceKey]?.label || prov.source}</span>
												</div>
											{/each}
										</div>
										{#if slot.evidence_links.length > 0}
											<div class="slot-evidence">
												<div class="evidence-label">Evidence ({slot.evidence_links.length}):</div>
												{#each slot.evidence_links as link, idx (idx)}
													<div class="evidence-item">{link}</div>
												{/each}
											</div>
										{/if}
										<div class="slot-timestamps">
											<span class="timestamp">Created: {formatTimestamp(slot.created_at)}</span>
											<span class="timestamp">Updated: {formatTimestamp(slot.updated_at)}</span>
										</div>
									{/if}
								</button>
							{/each}
						</div>
					{/if}
				</div>
			{/each}
		</div>

		<!-- Enrichment Pipeline Status (Grid view only) -->
		<EnrichmentPipelineStatus {enrichmentSummary} on:viewFullLog={() => console.log('View full log')} />
	{:else}
		<!-- Timeline View -->
		<div class="timeline-container" class:compact>
			<SlotTimelineView
				{slots}
				{enrichmentSummary}
				{eventTypeFilter}
				{slotTypeFilter}
				{compact}
			/>
		</div>
	{/if}
{/if}
</div>

<SlotDetailModal slot={selectedSlot} show={showModal} on:close={handleModalClose} />

<style>
	.slot-graph-inspector {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		min-width: 0;
	}

	.view-toggle-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.65rem 0.75rem;
		background: var(--bg-surface);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		margin-bottom: 0.5rem;
		flex-wrap: wrap;
	}

	.view-toggle {
		display: flex;
		gap: 0.35rem;
		background: var(--bg-elevated);
		padding: 0.25rem;
		border-radius: 6px;
		border: 1px solid var(--border-soft);
	}

	.toggle-btn {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.4rem 0.75rem;
		background: transparent;
		border: none;
		border-radius: 4px;
		cursor: pointer;
		font-size: 0.85rem;
		font-weight: 500;
		color: var(--text-muted);
		transition: all 0.2s ease;
	}

	.toggle-btn:hover {
		background: var(--bg-surface);
		color: var(--text-secondary);
	}

	.toggle-btn.active {
		background: var(--accent-primary);
		color: var(--text-on-accent);
		box-shadow: 0 1px 3px var(--accent-primary-soft);
	}


	.expand-controls,
	.expand-controls-standalone {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.expand-controls-standalone {
		padding: 0.65rem 0.75rem;
		background: var(--bg-surface);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		margin-bottom: 0.5rem;
	}

	.control-btn {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.4rem 0.75rem;
		background: var(--bg-elevated);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		cursor: pointer;
		font-size: 0.8rem;
		font-weight: 500;
		color: var(--text-secondary);
		transition: all 0.2s ease;
	}

	.control-btn:hover {
		background: var(--bg-surface);
		border-color: var(--border-default);
		color: var(--text-primary);
	}

	.control-btn:active {
		transform: scale(0.98);
	}


	.timeline-filters {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}


	.empty {
		background: var(--bg-surface);
		border-radius: 8px;
		border: 1px dashed var(--border-default);
		padding: 1rem;
		font-size: 0.85rem;
		color: var(--text-muted);
	}

	.friendly-summary {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
		gap: 0.5rem;
	}

	.metric-card {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		padding: 0.65rem 0.75rem;
		background: var(--bg-surface);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
	}

	.metric-card.warning {
		background: var(--color-warning-soft);
		border-color: var(--color-warning);
	}

	.metric-card .label {
		font-size: 0.7rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		font-weight: 600;
	}

	.metric-card .value {
		font-size: 1rem;
		font-weight: 700;
		color: var(--text-primary);
	}

	.slot-groups {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.group-card {
		background: var(--bg-elevated);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		overflow: hidden;
	}

	.group-header {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0.5rem 0.65rem;
		background: var(--bg-surface);
		border: none;
		cursor: pointer;
		transition: background 0.2s ease;
	}

	.group-header:hover {
		background: var(--bg-elevated);
	}

	.group-title {
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}

	.type-label {
		font-size: 0.7rem;
		font-weight: 600;
		color: var(--text-primary);
	}


	.group-meta {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.6rem;
	}

	.avg-confidence {
		font-weight: 600;
	}

	.expand-icon {
		color: var(--text-muted);
		font-size: 0.6rem;
	}

	.group-content {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		padding: 0.5rem;
		background: var(--bg-elevated);
	}

	.slot-card {
		width: 100%;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		padding: 0.5rem;
		background: var(--bg-surface);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		cursor: pointer;
		transition: all 0.2s ease;
		text-align: left;
	}

	.slot-card:hover {
		background: var(--bg-elevated);
		border-color: var(--border-default);
		box-shadow: var(--shadow-sm);
	}

	.slot-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.slot-id {
		font-size: 0.7rem;
		font-weight: 600;
		color: var(--text-secondary);
		font-family: var(--font-mono);
	}

	.confidence-badge {
		display: inline-flex;
		align-items: center;
		padding: 0.15rem 0.35rem;
		border-radius: 999px;
		font-size: 0.6rem;
		font-weight: 600;
	}

	.confidence-high {
		background: var(--color-success-soft);
		color: var(--color-success);
	}

	.confidence-medium {
		background: var(--color-warning-soft);
		color: var(--color-warning);
	}

	.confidence-low {
		background: var(--color-error-soft);
		color: var(--color-error);
	}

	.slot-value {
		font-size: 0.7rem;
		color: var(--text-primary);
		padding: 0.35rem;
		background: var(--bg-elevated);
		border-radius: 4px;
		border: 1px solid var(--border-soft);
		white-space: pre-wrap;
		word-break: break-word;
		max-height: 60px;
		overflow-y: auto;
	}

	.slot-provenance {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		padding: 0.35rem;
		background: var(--bg-surface);
		border-radius: 4px;
	}

	.provenance-label {
		font-size: 0.55rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		font-weight: 600;
	}

	.provenance-item {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		font-size: 0.6rem;
	}

	.prov-icon {
		font-size: 0.7rem;
	}

	.prov-source {
		color: var(--text-secondary);
		flex: 1;
	}

	.prov-confidence {
		font-weight: 600;
		color: var(--text-primary);
	}

	.slot-evidence {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		padding: 0.35rem;
		background: var(--bg-surface);
		border-radius: 4px;
	}

	.evidence-label {
		font-size: 0.55rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		font-weight: 600;
	}

	.evidence-item {
		font-size: 0.6rem;
		color: var(--text-secondary);
		font-family: var(--font-mono);
	}

	.slot-timestamps {
		display: flex;
		justify-content: space-between;
		gap: 0.35rem;
		padding-top: 0.35rem;
		border-top: 1px solid var(--border-soft);
	}

	.timestamp {
		font-size: 0.55rem;
		color: var(--text-muted);
	}

	/* Animation classes for newly extracted/updated slots */
	.slot-card.slot-extracted {
		animation: slot-extract 0.6s ease-out;
		border-color: var(--accent-primary);
		box-shadow: 0 4px 12px var(--accent-primary-soft);
	}

	.slot-card.slot-updated {
		animation: slot-update 0.5s ease-out;
		border-color: var(--color-success);
		box-shadow: 0 2px 8px var(--color-success-soft);
	}

	.confidence-badge.badge-flash {
		animation: badge-flash 0.8s ease-in-out;
	}

	@keyframes slot-extract {
		0% {
			opacity: 0;
			transform: translateY(-10px) scale(0.95);
			border-color: transparent;
		}
		40% {
			opacity: 1;
			transform: translateY(0) scale(1.02);
			border-color: color-mix(in srgb, var(--accent-primary) 60%, transparent);
		}
		100% {
			transform: scale(1);
			border-color: color-mix(in srgb, var(--accent-primary) 50%, transparent);
		}
	}

	@keyframes slot-update {
		0% {
			background: var(--color-success-soft);
			border-color: color-mix(in srgb, var(--color-success) 50%, transparent);
			box-shadow: 0 4px 12px color-mix(in srgb, var(--color-success) 30%, transparent);
		}
		100% {
			background: var(--bg-surface);
			border-color: color-mix(in srgb, var(--color-success) 40%, transparent);
			box-shadow: 0 2px 8px color-mix(in srgb, var(--color-success) 15%, transparent);
		}
	}

	@keyframes badge-flash {
		0%,
		100% {
			transform: scale(1);
			box-shadow: none;
		}
		25% {
			transform: scale(1.15);
			box-shadow: 0 0 8px color-mix(in srgb, var(--color-success) 40%, transparent);
		}
		50% {
			transform: scale(1);
		}
		75% {
			transform: scale(1.1);
			box-shadow: 0 0 6px color-mix(in srgb, var(--color-success) 30%, transparent);
		}
	}

	/* Timeline container - fixed height with scrolling */
	.timeline-container {
		width: 100%;
		height: 220px;
		overflow-x: auto;
		overflow-y: auto;
	}

	.timeline-container.compact {
		height: auto;
		max-height: none;
		overflow-x: auto;
		overflow-y: hidden;
	}

	/* Slim horizontal scrollbar in compact mode */
	.timeline-container.compact::-webkit-scrollbar {
		height: 4px;
	}

	.timeline-container.compact::-webkit-scrollbar-track {
		background: transparent;
	}

	.timeline-container.compact::-webkit-scrollbar-thumb {
		background: color-mix(in srgb, var(--text-primary) 15%, transparent);
		border-radius: 2px;
	}

	.timeline-container.compact::-webkit-scrollbar-thumb:hover {
		background: color-mix(in srgb, var(--text-primary) 25%, transparent);
	}

	/* ── Retro 16-bit Dark Theme ── */
	:global([data-theme="retro-16bit"]) .slot-graph-inspector {
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit"]) .view-toggle-bar,
	:global([data-theme="retro-16bit"]) .expand-controls-standalone,
	:global([data-theme="retro-16bit"]) .view-toggle,
	:global([data-theme="retro-16bit"]) .group-card,
	:global([data-theme="retro-16bit"]) .slot-card,
	:global([data-theme="retro-16bit"]) .slot-value,
	:global([data-theme="retro-16bit"]) .slot-provenance,
	:global([data-theme="retro-16bit"]) .slot-evidence,
	:global([data-theme="retro-16bit"]) .metric-card,
	:global([data-theme="retro-16bit"]) .empty,
	:global([data-theme="retro-16bit"]) .confidence-badge {
		border-radius: 0;
	}

	:global([data-theme="retro-16bit"]) .toggle-btn,
	:global([data-theme="retro-16bit"]) .control-btn {
		border-radius: 0;
		font-family: var(--font-mono);
		text-transform: uppercase;
		border: 1px solid var(--border-default);
		color: var(--text-primary);
		background: var(--bg-base);
	}

	:global([data-theme="retro-16bit"]) .toggle-btn:hover,
	:global([data-theme="retro-16bit"]) .control-btn:hover {
		background: var(--accent-primary);
		color: var(--text-on-accent);
	}

	:global([data-theme="retro-16bit"]) .toggle-btn.active {
		background: var(--accent-primary);
		color: var(--text-on-accent);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit"]) .group-header {
		border-radius: 0;
		font-family: var(--font-mono);
		text-transform: uppercase;
	}

	/* ── Retro 16-bit Light Theme ── */
	:global([data-theme="retro-16bit-light"]) .slot-graph-inspector {
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit-light"]) .view-toggle-bar,
	:global([data-theme="retro-16bit-light"]) .expand-controls-standalone,
	:global([data-theme="retro-16bit-light"]) .view-toggle,
	:global([data-theme="retro-16bit-light"]) .group-card,
	:global([data-theme="retro-16bit-light"]) .slot-card,
	:global([data-theme="retro-16bit-light"]) .slot-value,
	:global([data-theme="retro-16bit-light"]) .slot-provenance,
	:global([data-theme="retro-16bit-light"]) .slot-evidence,
	:global([data-theme="retro-16bit-light"]) .metric-card,
	:global([data-theme="retro-16bit-light"]) .empty,
	:global([data-theme="retro-16bit-light"]) .confidence-badge {
		border-radius: 0;
	}

	:global([data-theme="retro-16bit-light"]) .toggle-btn,
	:global([data-theme="retro-16bit-light"]) .control-btn {
		border-radius: 0;
		font-family: var(--font-mono);
		text-transform: uppercase;
		border: 2px solid var(--border-default);
		color: var(--text-primary);
		background: var(--bg-base);
	}

	:global([data-theme="retro-16bit-light"]) .toggle-btn:hover,
	:global([data-theme="retro-16bit-light"]) .control-btn:hover {
		background: var(--accent-primary);
		color: var(--text-on-accent);
	}

	:global([data-theme="retro-16bit-light"]) .toggle-btn.active {
		background: var(--accent-primary);
		color: var(--text-on-accent);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit-light"]) .group-header {
		border-radius: 0;
		font-family: var(--font-mono);
		text-transform: uppercase;
	}
</style>
