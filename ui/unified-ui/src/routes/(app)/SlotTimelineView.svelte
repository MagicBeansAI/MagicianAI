<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import type { SlotRecord, EnrichmentSummary } from '$lib/types/mission_control';
	import Badge from '$lib/magician/components/native/Badge.svelte';

	export let slots: SlotRecord[] = [];
	export let enrichmentSummary: EnrichmentSummary | null = null;
	export let eventTypeFilter: string = 'all'; // 'all', 'extraction', 'enrichment', 'confidence'
	export let slotTypeFilter: string = 'all'; // 'all' or specific slot type
	export let compact: boolean = false;

	// Tooltip state
	let tooltipEl: HTMLDivElement | null = null;
	let tooltipVisible = false;

	onMount(() => {
		// Create tooltip element appended to body (avoids overflow clipping)
		tooltipEl = document.createElement('div');
		tooltipEl.className = 'slot-timeline-tooltip';
		const theme = document.documentElement.getAttribute('data-theme') || '';
		const isRetroDark = theme === 'retro-16bit';
		const isRetroLight = theme === 'retro-16bit-light';
		const rootStyles = getComputedStyle(document.documentElement);
		const token = (name: string, fallback: string): string =>
			rootStyles.getPropertyValue(name).trim() || fallback;
		const bg = token('--bg-elevated', token('--bg-card', 'Canvas'));
		const fg = token('--text-primary', 'CanvasText');
		const radius = isRetroDark || isRetroLight ? '0' : token('--radius-sm', '6px');
		const shadow = isRetroDark || isRetroLight
			? `2px 2px 0 ${token('--border-default', 'currentColor')}`
			: token('--shadow-md', `0 4px 12px color-mix(in srgb, ${token('--text-primary', 'currentColor')} 22%, transparent)`);
		tooltipEl.style.cssText = `
			position: fixed;
			z-index: 99999;
			background: ${bg};
			color: ${fg};
			padding: 6px 10px;
			border-radius: ${radius};
			font-size: 0.65rem;
			max-width: 250px;
			line-height: 1.4;
			pointer-events: none;
			opacity: 0;
			transition: opacity 0.15s ease;
			box-shadow: ${shadow};
			white-space: pre-wrap;
			word-wrap: break-word;
			font-family: ${isRetroDark || isRetroLight ? 'var(--font-mono)' : 'inherit'};
		`;
		document.body.appendChild(tooltipEl);
	});

	onDestroy(() => {
		if (tooltipEl && tooltipEl.parentNode) {
			tooltipEl.parentNode.removeChild(tooltipEl);
		}
	});

	function showTooltip(event: MouseEvent, text: string) {
		if (!tooltipEl || !compact) return;

		tooltipEl.textContent = text;
		tooltipEl.style.opacity = '1';
		tooltipVisible = true;

		// Position tooltip near cursor
		const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
		const tooltipRect = tooltipEl.getBoundingClientRect();

		let left = rect.left + (rect.width / 2) - (tooltipRect.width / 2);
		let top = rect.bottom + 8;

		// Keep within viewport
		if (left < 10) left = 10;
		if (left + tooltipRect.width > window.innerWidth - 10) {
			left = window.innerWidth - tooltipRect.width - 10;
		}
		if (top + tooltipRect.height > window.innerHeight - 10) {
			top = rect.top - tooltipRect.height - 8;
		}

		tooltipEl.style.left = `${left}px`;
		tooltipEl.style.top = `${top}px`;
	}

	function hideTooltip() {
		if (!tooltipEl) return;
		tooltipEl.style.opacity = '0';
		tooltipVisible = false;
	}

	type TimelineEvent = {
		id: string;
		timestamp: number;
		eventType: 'extraction' | 'enrichment' | 'confidence_update';
		icon: string;
		title: string;
		description: string;
		metadata: Record<string, any>;
		slotId?: string;
		slotType?: string;
	};

	$: timelineEvents = buildTimelineEvents(slots, enrichmentSummary);
	$: filteredEvents = filterEvents(timelineEvents, eventTypeFilter, slotTypeFilter);

	function buildTimelineEvents(
		slots: SlotRecord[],
		enrichmentSummary: EnrichmentSummary | null
	): TimelineEvent[] {
		const events: TimelineEvent[] = [];

		// Add extraction events for each slot
		for (const slot of slots) {
			events.push({
				id: `extract-${slot.id}`,
				timestamp: slot.created_at,
				eventType: 'extraction',
				icon: '🔍',
				title: 'Slot Extracted',
				description: `${getSlotTypeLabel(slot.slot_type)}: ${formatValue(slot.value)}`,
				metadata: {
					slotId: slot.id,
					slotType: slot.slot_type,
					confidence: slot.confidence,
					provenance: slot.provenance
				},
				slotId: slot.id,
				slotType: String(slot.slot_type)
			});

			// Add confidence update events if slot was updated
			if (slot.updated_at > slot.created_at) {
				events.push({
					id: `update-${slot.id}`,
					timestamp: slot.updated_at,
					eventType: 'confidence_update',
					icon: '📈',
					title: 'Confidence Updated',
					description: `${getSlotTypeLabel(slot.slot_type)} confidence boosted`,
					metadata: {
						slotId: slot.id,
						slotType: slot.slot_type,
						confidence: slot.confidence
					},
					slotId: slot.id,
					slotType: String(slot.slot_type)
				});
			}
		}

		// Add enrichment event
		if (enrichmentSummary && enrichmentSummary.invocations > 0) {
			// Use the average of all slot created_at times for enrichment timestamp
			const avgTimestamp =
				slots.reduce((sum, slot) => sum + slot.created_at, 0) / slots.length || Date.now();
			events.push({
				id: 'enrichment-pipeline',
				timestamp: avgTimestamp + 100, // Slightly after extraction
				eventType: 'enrichment',
				icon: '⚙️',
				title: 'Enrichment Pipeline',
				description: `${enrichmentSummary.invocations} enricher(s) ran, ${enrichmentSummary.slots_changed} slot(s) changed`,
				metadata: {
					invocations: enrichmentSummary.invocations,
					slotsChanged: enrichmentSummary.slots_changed,
					totalSlots: enrichmentSummary.total_slots,
					errors: enrichmentSummary.errors
				}
			});
		}

		// Sort by timestamp (most recent first for timeline display)
		events.sort((a, b) => b.timestamp - a.timestamp);

		return events;
	}

	function filterEvents(
		events: TimelineEvent[],
		eventType: string,
		slotType: string
	): TimelineEvent[] {
		return events.filter((event) => {
			// Event type filter
			if (eventType !== 'all' && event.eventType !== eventType) {
				return false;
			}

			// Slot type filter
			if (slotType !== 'all' && event.slotType !== slotType) {
				return false;
			}

			return true;
		});
	}

	function getSlotTypeLabel(slotType: any): string {
		const typeMap: Record<string, string> = {
			entity: 'Entity',
			temporal: 'Temporal',
			spatial: 'Spatial',
			emotion: 'Emotion',
			action: 'Action',
			modifier: 'Modifier',
			resource: 'Resource',
			status: 'Status'
		};
		return typeMap[String(slotType).toLowerCase()] || String(slotType);
	}

	function formatValue(value: any): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'object' && value !== null) {
			return JSON.stringify(value);
		}
		return String(value);
	}

	function formatTimestamp(timestamp: number): string {
		const date = new Date(timestamp);
		return date.toLocaleTimeString('en-US', {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	function formatDate(timestamp: number): string {
		const date = new Date(timestamp);
		return date.toLocaleDateString('en-US', {
			month: 'short',
			day: 'numeric'
		});
	}

	/* getEventColor removed — now using CSS classes .marker-{eventType} */

	function getConfidenceClass(confidence: number): string {
		if (confidence >= 0.8) return 'confidence-high';
		if (confidence >= 0.5) return 'confidence-medium';
		return 'confidence-low';
	}

	function formatPercent(value: number): string {
		return `${(value * 100).toFixed(1)}%`;
	}

	function buildTooltip(event: TimelineEvent): string {
		const lines: string[] = [];
		lines.push(event.title);
		lines.push(event.description);
		if (event.metadata?.confidence !== undefined) {
			lines.push(`Confidence: ${formatPercent(event.metadata.confidence)}`);
		}
		if (event.metadata?.provenance?.length > 0) {
			lines.push(`Sources: ${event.metadata.provenance.length}`);
		}
		lines.push(formatTimestamp(event.timestamp));
		return lines.join(' | ');
	}
</script>

<div class="timeline-view" class:compact>
	{#if filteredEvents.length === 0}
		<div class="empty" class:compact>
			<p>No events to display. Adjust filters or wait for slot extraction.</p>
		</div>
	{:else}
		<div class="timeline" class:compact>
			{#each filteredEvents as event, idx (event.id)}
				{@const isFirst = idx === 0}
				{@const isLast = idx === filteredEvents.length - 1}
				{@const prevDate = idx > 0 ? formatDate(filteredEvents[idx - 1].timestamp) : ''}
				{@const currentDate = formatDate(event.timestamp)}
				{@const showDateSeparator = isFirst || prevDate !== currentDate}

				{#if showDateSeparator}
					<div class="date-separator" class:compact>
						<span class="date-label" class:compact>{currentDate}</span>
					</div>
				{/if}

				<div class="timeline-item" class:first={isFirst} class:last={isLast} class:compact>
					<div class="timeline-marker marker-{event.eventType}" class:compact>
						<span class="marker-icon" class:compact>{event.icon}</span>
					</div>

					<div
						class="timeline-content"
						class:compact
						on:mouseenter={(e) => showTooltip(e, buildTooltip(event))}
						on:mouseleave={hideTooltip}
						role="button"
						tabindex="0"
					>
						<div class="event-header" class:compact>
							<h4 class="event-title" class:compact>{event.title}</h4>
							<span class="event-time" class:compact>{formatTimestamp(event.timestamp)}</span>
						</div>

						<div class="event-description" class:compact>{event.description}</div>

						{#if event.eventType === 'extraction' && event.metadata}
							<div class="event-metadata" class:compact>
								<div class="metadata-row" class:compact>
									<Badge
										text={formatPercent(event.metadata.confidence)}
										color={event.metadata.confidence >= 0.8 ? 'success' : event.metadata.confidence >= 0.5 ? 'warning' : 'error'}
									/>
									{#if event.metadata.provenance && event.metadata.provenance.length > 0}
										<Badge text="{event.metadata.provenance.length} src" />
									{/if}
								</div>
								{#if !compact}
									<div class="metadata-item">
										<span class="label">Confidence:</span>
										<span class="value">{formatPercent(event.metadata.confidence)}</span>
									</div>
									{#if event.metadata.provenance && event.metadata.provenance.length > 0}
										<div class="metadata-item">
											<span class="label">Sources:</span>
											<span class="value">{event.metadata.provenance.length} source(s)</span>
										</div>
									{/if}
								{/if}
							</div>
						{/if}

						{#if event.eventType === 'enrichment' && event.metadata}
							<div class="event-metadata" class:compact>
								<div class="metadata-grid" class:compact>
									<div class="metadata-item" class:compact>
										<span class="label">Invocations:</span>
										<span class="value">{event.metadata.invocations}</span>
									</div>
									<div class="metadata-item" class:compact>
										<span class="label">Changed:</span>
										<span class="value">{event.metadata.slotsChanged} slot(s)</span>
									</div>
									<div class="metadata-item" class:compact>
										<span class="label">Total:</span>
										<span class="value">{event.metadata.totalSlots} slot(s)</span>
									</div>
									{#if event.metadata.errors && event.metadata.errors.length > 0}
										<div class="metadata-item error" class:compact>
											<span class="label">Errors:</span>
											<span class="value">{event.metadata.errors.length}</span>
										</div>
									{/if}
								</div>
							</div>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.timeline-view {
		display: flex;
		flex-direction: column;
		gap: 0;
		width: 100%;
	}

	.timeline-view.compact {
		display: inline-flex;
		flex-direction: row;
		width: max-content;
		min-width: 100%;
	}

	.timeline.compact {
		display: flex;
		flex-direction: row;
		gap: 0.5rem;
		padding: 0;
		width: max-content;
	}

	.empty {
		padding: 2rem 1rem;
		text-align: center;
		color: var(--text-muted);
		font-size: 0.85rem;
		background: var(--bg-surface);
		border-radius: var(--radius-md);
		border: 1px dashed var(--border-soft);
	}

	.timeline {
		position: relative;
		padding: 0;
		width: 100%;
	}

	.date-separator {
		display: flex;
		align-items: center;
		margin: 1.5rem 0 1rem 0;
		position: relative;
	}

	.date-separator::before {
		content: '';
		flex: 1;
		height: 1px;
		background: linear-gradient(to right, transparent, var(--border-soft), transparent);
		margin-right: 1rem;
	}

	.date-separator::after {
		content: '';
		flex: 1;
		height: 1px;
		background: linear-gradient(to left, transparent, var(--border-soft), transparent);
		margin-left: 1rem;
	}

	.date-label {
		font-size: 0.75rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		padding: 0.25rem 0.75rem;
		background: var(--bg-surface);
		border-radius: var(--radius-full);
		border: 1px solid var(--border-soft);
	}

	.timeline-item {
		position: relative;
		display: flex;
		gap: 1rem;
		padding-left: 0;
		padding-bottom: 1.5rem;
	}

	.timeline-item::before {
		content: '';
		position: absolute;
		left: 19px;
		top: 40px;
		bottom: -10px;
		width: 2px;
		background: linear-gradient(to bottom, var(--border-soft) 0%, var(--border-soft) 80%, transparent 100%);
	}

	.timeline-item.last::before {
		background: linear-gradient(to bottom, var(--border-soft) 0%, transparent 100%);
		bottom: 0;
	}

	.timeline-marker {
		flex-shrink: 0;
		width: 38px;
		height: 38px;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		background-color: var(--text-muted);
		border: 3px solid var(--bg-elevated);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--text-muted) 25%, transparent);
		z-index: 1;
		position: relative;
	}

	.marker-icon {
		font-size: 1rem;
	}

	.timeline-content {
		flex: 1;
		background: var(--bg-elevated);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		padding: 0.85rem 1rem;
		box-shadow: var(--shadow-sm);
		transition: all 0.2s ease;
		min-width: 300px;
	}

	.timeline-content:hover {
		border-color: var(--border-default);
		box-shadow: var(--shadow-md);
	}

	.event-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		margin-bottom: 0.4rem;
	}

	.event-title {
		font-size: 0.9rem;
		font-weight: 600;
		color: var(--text-primary);
		margin: 0;
	}

	.event-time {
		font-size: 0.75rem;
		color: var(--text-muted);
		font-family: var(--font-mono);
	}

	.event-description {
		font-size: 0.85rem;
		color: var(--text-secondary);
		line-height: 1.5;
		margin-bottom: 0.5rem;
		word-wrap: break-word;
		overflow-wrap: break-word;
	}

	.event-metadata {
		margin-top: 0.75rem;
		padding-top: 0.75rem;
		border-top: 1px solid var(--border-soft);
	}

	.metadata-item {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.8rem;
		margin-bottom: 0.35rem;
	}

	.metadata-item:last-child {
		margin-bottom: 0;
	}

	.metadata-item .label {
		color: var(--text-muted);
		font-weight: 500;
	}

	.metadata-item .value {
		color: var(--text-primary);
		font-weight: 600;
		word-wrap: break-word;
		overflow-wrap: break-word;
	}

	.metadata-item.error .value {
		color: var(--color-error);
	}

	.metadata-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
		gap: 0.5rem;
	}


	@media (max-width: 640px) {
		.timeline-item {
			gap: 0.75rem;
		}

		.timeline-marker {
			width: 32px;
			height: 32px;
		}

		.marker-icon {
			font-size: 0.9rem;
		}

		.timeline-content {
			padding: 0.75rem;
		}

		.metadata-grid {
			grid-template-columns: 1fr;
		}
	}

	/* Compact mode styles - horizontal layout */
	.empty.compact {
		padding: 0.5rem;
		font-size: 0.65rem;
	}

	.date-separator.compact {
		display: none;
	}

	.timeline-item.compact {
		flex-direction: column;
		gap: 0.25rem;
		padding-bottom: 0;
		flex-shrink: 0;
	}

	.timeline-item.compact::before {
		display: none;
	}

	.timeline-marker.compact {
		width: 20px;
		height: 20px;
		border-width: 2px;
		box-shadow: none;
	}

	.marker-icon.compact {
		font-size: 0.55rem;
	}

	.timeline-content.compact {
		padding: 0.35rem 0.5rem;
		border-radius: 4px;
		min-width: 120px;
		max-width: 140px;
	}

	.event-header.compact {
		margin-bottom: 0.15rem;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.1rem;
	}

	.event-title.compact {
		font-size: 0.6rem;
	}

	.event-time.compact {
		font-size: 0.5rem;
	}

	.event-description.compact {
		font-size: 0.55rem;
		line-height: 1.2;
		margin-bottom: 0;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}

	.event-metadata.compact {
		margin-top: 0.2rem;
		padding-top: 0.2rem;
		border-top: 1px solid var(--border-soft);
	}

	.metadata-item.compact {
		gap: 0.2rem;
		font-size: 0.5rem;
		margin-bottom: 0;
	}

	.metadata-grid.compact {
		gap: 0.2rem;
	}

	.metadata-row {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		flex-wrap: wrap;
	}

	.metadata-row.compact {
		gap: 0.25rem;
	}

	/* Theme-adaptive overrides */
	.empty {
		color: var(--text-muted);
		background: var(--bg-surface);
		border-color: var(--border-soft);
	}

	.date-label {
		color: var(--text-muted);
		background: var(--bg-surface);
		border-color: var(--border-soft);
	}

	.date-separator::before,
	.date-separator::after {
		background: linear-gradient(to right, transparent, var(--border-soft), transparent);
	}

	.timeline-item::before {
		background: linear-gradient(to bottom, var(--border-soft) 0%, var(--border-soft) 80%, transparent 100%);
	}

	.timeline-item.last::before {
		background: linear-gradient(to bottom, var(--border-soft) 0%, transparent 100%);
	}

	.timeline-marker {
		border-color: var(--bg-elevated);
	}

	.timeline-content {
		background: var(--bg-elevated);
		border-color: var(--border-soft);
	}

	.timeline-content:hover {
		border-color: var(--border-default);
	}

	.event-title {
		color: var(--text-primary);
	}

	.event-time {
		color: var(--text-muted);
	}

	.event-description {
		color: var(--text-secondary);
	}

	.event-metadata {
		border-top-color: var(--border-soft);
	}

	.metadata-item .label {
		color: var(--text-muted);
	}

	.metadata-item .value {
		color: var(--text-primary);
	}

	.metadata-item.error .value {
		color: var(--color-error);
	}

	/* ── Event type marker colors (replaces inline getEventColor) ── */
	.marker-extraction {
		background-color: var(--accent-primary);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--accent-primary) 25%, transparent);
	}

	.marker-enrichment {
		background-color: var(--color-success);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--color-success) 25%, transparent);
	}

	.marker-confidence_update {
		background-color: var(--color-warning);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--color-warning) 25%, transparent);
	}

	/* ── Retro 16-bit Dark Theme ── */
	:global([data-theme="retro-16bit"]) .timeline-view {
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit"]) .empty {
		border-radius: 0;
		background: var(--bg-base);
		border-color: var(--border-soft);
		color: var(--text-muted);
	}

	:global([data-theme="retro-16bit"]) .date-label {
		border-radius: 0;
		background: var(--bg-base);
		border-color: var(--border-soft);
		color: var(--text-muted);
	}

	:global([data-theme="retro-16bit"]) .date-separator::before,
	:global([data-theme="retro-16bit"]) .date-separator::after {
		background: linear-gradient(to right, transparent, var(--border-soft), transparent);
	}

	:global([data-theme="retro-16bit"]) .timeline-item::before {
		background: linear-gradient(to bottom, var(--border-soft) 0%, var(--border-soft) 80%, transparent 100%);
	}

	:global([data-theme="retro-16bit"]) .timeline-item.last::before {
		background: linear-gradient(to bottom, var(--border-soft) 0%, transparent 100%);
	}

	:global([data-theme="retro-16bit"]) .timeline-marker {
		border-color: var(--bg-base);
		border-radius: 0;
	}

	:global([data-theme="retro-16bit"]) .marker-extraction {
		background-color: var(--accent-primary);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit"]) .marker-enrichment {
		background-color: var(--color-success);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit"]) .marker-confidence_update {
		background-color: var(--color-warning);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit"]) .timeline-content {
		border-radius: 0;
		background: var(--bg-card);
		border-color: var(--border-soft);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit"]) .timeline-content:hover {
		border-color: var(--border-default);
	}

	:global([data-theme="retro-16bit"]) .event-title {
		color: var(--text-primary);
	}

	:global([data-theme="retro-16bit"]) .event-time {
		color: var(--text-muted);
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit"]) .event-description {
		color: var(--text-secondary);
	}

	:global([data-theme="retro-16bit"]) .event-metadata {
		border-top-color: var(--border-soft);
	}

	:global([data-theme="retro-16bit"]) .metadata-item .label {
		color: var(--text-muted);
	}

	:global([data-theme="retro-16bit"]) .metadata-item .value {
		color: var(--text-primary);
	}

	:global([data-theme="retro-16bit"]) .metadata-item.error .value {
		color: var(--color-error);
	}

	:global([data-theme="retro-16bit"]) .timeline-content.compact {
		border-radius: 0;
	}

	/* ── Retro 16-bit Light Theme ── */
	:global([data-theme="retro-16bit-light"]) .timeline-view {
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit-light"]) .empty {
		border-radius: 0;
		background: var(--bg-base);
		border-color: var(--border-soft);
		color: var(--text-muted);
	}

	:global([data-theme="retro-16bit-light"]) .date-label {
		border-radius: 0;
		background: var(--bg-base);
		border-color: var(--border-soft);
		color: var(--text-muted);
	}

	:global([data-theme="retro-16bit-light"]) .date-separator::before,
	:global([data-theme="retro-16bit-light"]) .date-separator::after {
		background: linear-gradient(to right, transparent, var(--border-soft), transparent);
	}

	:global([data-theme="retro-16bit-light"]) .timeline-item::before {
		background: linear-gradient(to bottom, var(--border-soft) 0%, var(--border-soft) 80%, transparent 100%);
	}

	:global([data-theme="retro-16bit-light"]) .timeline-item.last::before {
		background: linear-gradient(to bottom, var(--border-soft) 0%, transparent 100%);
	}

	:global([data-theme="retro-16bit-light"]) .timeline-marker {
		border-color: var(--bg-base);
		border-radius: 0;
	}

	:global([data-theme="retro-16bit-light"]) .marker-extraction {
		background-color: var(--accent-primary);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit-light"]) .marker-enrichment {
		background-color: var(--color-success);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit-light"]) .marker-confidence_update {
		background-color: var(--color-warning);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit-light"]) .timeline-content {
		border-radius: 0;
		background: var(--bg-card);
		border-color: var(--border-soft);
		box-shadow: 2px 2px 0 var(--border-default);
	}

	:global([data-theme="retro-16bit-light"]) .timeline-content:hover {
		border-color: var(--border-default);
	}

	:global([data-theme="retro-16bit-light"]) .event-title {
		color: var(--text-primary);
	}

	:global([data-theme="retro-16bit-light"]) .event-time {
		color: var(--text-muted);
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit-light"]) .event-description {
		color: var(--text-secondary);
	}

	:global([data-theme="retro-16bit-light"]) .event-metadata {
		border-top-color: var(--border-soft);
	}

	:global([data-theme="retro-16bit-light"]) .metadata-item .label {
		color: var(--text-muted);
	}

	:global([data-theme="retro-16bit-light"]) .metadata-item .value {
		color: var(--text-primary);
	}

	:global([data-theme="retro-16bit-light"]) .metadata-item.error .value {
		color: var(--color-error);
	}

	:global([data-theme="retro-16bit-light"]) .timeline-content.compact {
		border-radius: 0;
	}
</style>
