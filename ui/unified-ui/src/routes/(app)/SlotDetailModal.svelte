<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import type { SlotRecord, ProvenanceRecord } from '$lib/types/mission_control';

	export let slot: SlotRecord | null = null;
	export let show = false;

	const dispatch = createEventDispatcher<{
		close: void;
	}>();

	const SLOT_TYPE_CONFIG: Record<string, { label: string; color: string; icon: string }> = {
		entity: { label: 'Entity', color: '#3b82f6', icon: '👤' },
		temporal: { label: 'Temporal', color: '#8b5cf6', icon: '⏰' },
		spatial: { label: 'Spatial', color: '#06b6d4', icon: '📍' },
		emotion: { label: 'Emotion', color: '#ec4899', icon: '💭' },
		action: { label: 'Action', color: '#10b981', icon: '⚡' },
		modifier: { label: 'Modifier', color: '#f59e0b', icon: '⚙️' },
		resource: { label: 'Resource', color: '#6366f1', icon: '📎' },
		status: { label: 'Status', color: '#14b8a6', icon: '📊' }
	};

	const PROVENANCE_CONFIG: Record<string, { label: string; icon: string; color: string }> = {
		LlmPrimary: { label: 'LLM Primary', icon: '🤖', color: '#3b82f6' },
		UserReply: { label: 'User Reply', icon: '💬', color: '#10b981' },
		ScreenshotInference: { label: 'Screenshot', icon: '📸', color: '#8b5cf6' },
		DeterministicCheck: { label: 'Deterministic', icon: '✓', color: '#06b6d4' },
		MemoryLookup: { label: 'Memory', icon: '🧠', color: '#ec4899' }
	};

	$: slotType = slot ? String(slot.slot_type).toLowerCase() : '';
	$: typeConfig = SLOT_TYPE_CONFIG[slotType] || { label: slotType, color: '#64748b', icon: '📝' };
	$: sortedProvenance = slot ? [...slot.provenance].sort((a, b) => a.timestamp - b.timestamp) : [];
	$: overallConfidence = slot?.confidence ?? 0;

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape' && show) {
			handleClose();
		}
	}

	function handleBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) {
			handleClose();
		}
	}

	function handleClose() {
		dispatch('close');
	}

	function formatTimestamp(timestamp: number): string {
		return new Date(timestamp).toLocaleString();
	}

	function formatPercent(value: number | null | undefined): string {
		if (value === null || value === undefined || Number.isNaN(value)) return '—';
		return `${(value * 100).toFixed(1)}%`;
	}

	function getConfidenceClass(confidence: number): string {
		if (confidence >= 0.8) return 'confidence-high';
		if (confidence >= 0.5) return 'confidence-medium';
		return 'confidence-low';
	}

	function getConfidenceLabel(confidence: number): string {
		if (confidence >= 0.8) return '🟢 High';
		if (confidence >= 0.5) return '🟡 Medium';
		return '🔴 Low';
	}

	function formatValue(value: any): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'object' && value !== null) {
			return JSON.stringify(value, null, 2);
		}
		return String(value);
	}

	onMount(() => {
		if (show) {
			document.addEventListener('keydown', handleKeydown);
			return () => {
				document.removeEventListener('keydown', handleKeydown);
			};
		}
	});

	$: if (show) {
		document.addEventListener('keydown', handleKeydown);
	} else {
		document.removeEventListener('keydown', handleKeydown);
	}
</script>

{#if show && slot}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<div
		class="modal-backdrop"
		on:click={handleBackdropClick}
		role="dialog"
		aria-modal="true"
		aria-labelledby="slot-detail-title"
		tabindex="-1"
	>
		<div class="modal-content">
			<!-- Header -->
			<div class="modal-header">
				<div class="header-left">
					<span class="type-icon" style="color: {typeConfig.color}">
						{typeConfig.icon}
					</span>
					<div class="title-section">
						<h2 id="slot-detail-title" class="modal-title">
							{typeConfig.label} Slot Detail
						</h2>
						<div class="slot-id-display">{slot.id}</div>
					</div>
				</div>
				<button class="close-btn" on:click={handleClose} aria-label="Close modal">
					✕
				</button>
			</div>

			<!-- Body -->
			<div class="modal-body">
				<!-- Confidence Overview -->
				<section class="confidence-section">
					<h3>Confidence Overview</h3>
					<div class="confidence-grid">
						<div class="confidence-card">
							<span class="label">Overall</span>
							<span class="value {getConfidenceClass(overallConfidence)}">
								{getConfidenceLabel(overallConfidence)}
							</span>
							<span class="numeric">{formatPercent(overallConfidence)}</span>
						</div>
						<div class="confidence-card">
							<span class="label">Base Confidence</span>
							<span class="numeric">{formatPercent(slot.confidence)}</span>
						</div>
					</div>
				</section>

				<!-- Slot Value -->
				<section class="value-section">
					<h3>Value</h3>
					<div class="value-display">
						<pre>{formatValue(slot.value)}</pre>
					</div>
				</section>

				<!-- Provenance Timeline -->
				<section class="provenance-section">
					<h3>Provenance Chain ({sortedProvenance.length})</h3>
					<div class="provenance-timeline">
						{#each sortedProvenance as prov, idx (idx)}
							<div class="timeline-item">
								<div class="timeline-marker" style="background-color: {PROVENANCE_CONFIG[prov.source]?.color || '#94a3b8'}">
									<span class="marker-icon">{PROVENANCE_CONFIG[prov.source]?.icon || '📋'}</span>
								</div>
								<div class="timeline-content">
									<div class="timeline-header">
										<span class="source-label">{PROVENANCE_CONFIG[prov.source]?.label || prov.source}</span>
									</div>
									<div class="timeline-timestamp">{formatTimestamp(prov.timestamp)}</div>
								</div>
							</div>
						{/each}
					</div>
				</section>

				<!-- Evidence Links -->
				{#if slot.evidence_links.length > 0}
					<section class="evidence-section">
						<h3>Evidence Links ({slot.evidence_links.length})</h3>
						<ul class="evidence-list">
							{#each slot.evidence_links as link, idx (idx)}
								<li class="evidence-item">
									<span class="evidence-icon">🔗</span>
									<span class="evidence-link">{link}</span>
								</li>
							{/each}
						</ul>
					</section>
				{/if}

				<!-- Metadata -->
				<section class="metadata-section">
					<h3>Metadata</h3>
					<div class="metadata-grid">
						<div class="metadata-row">
							<span class="metadata-label">Created</span>
							<span class="metadata-value">{formatTimestamp(slot.created_at)}</span>
						</div>
						<div class="metadata-row">
							<span class="metadata-label">Last Updated</span>
							<span class="metadata-value">{formatTimestamp(slot.updated_at)}</span>
						</div>
						<div class="metadata-row">
							<span class="metadata-label">Slot Type</span>
							<span class="metadata-value">{typeConfig.label}</span>
						</div>
						<div class="metadata-row">
							<span class="metadata-label">Provenance Sources</span>
							<span class="metadata-value">{sortedProvenance.length}</span>
						</div>
					</div>
				</section>
			</div>

			<!-- Footer -->
			<div class="modal-footer">
				<button class="btn-secondary" on:click={handleClose}>
					Close
				</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.modal-backdrop {
		position: fixed;
		top: 0;
		left: 0;
		right: 0;
		bottom: 0;
		background: rgba(15, 23, 42, 0.75);
		backdrop-filter: blur(4px);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 1000;
		padding: 1rem;
		animation: fadeIn 0.2s ease;
	}

	@keyframes fadeIn {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	.modal-content {
		background: #ffffff;
		border-radius: 16px;
		box-shadow: 0 24px 48px rgba(15, 23, 42, 0.25);
		width: 100%;
		max-width: 700px;
		max-height: 90vh;
		display: flex;
		flex-direction: column;
		animation: slideUp 0.3s ease;
	}

	@keyframes slideUp {
		from {
			opacity: 0;
			transform: translateY(20px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	.modal-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 1.5rem;
		border-bottom: 1px solid #e2e8f0;
	}

	.header-left {
		display: flex;
		align-items: center;
		gap: 1rem;
	}

	.type-icon {
		font-size: 2rem;
	}

	.title-section {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.modal-title {
		margin: 0;
		font-size: 1.25rem;
		font-weight: 600;
		color: #0f172a;
	}

	.slot-id-display {
		font-size: 0.75rem;
		font-family: var(--font-mono);
		color: #64748b;
	}

	.close-btn {
		width: 36px;
		height: 36px;
		display: flex;
		align-items: center;
		justify-content: center;
		border-radius: 8px;
		border: none;
		background: #f1f5f9;
		color: #475569;
		font-size: 1.25rem;
		cursor: pointer;
		transition: all 0.2s ease;
	}

	.close-btn:hover {
		background: #e2e8f0;
		color: #1e293b;
	}

	.modal-body {
		padding: 1.5rem;
		overflow-y: auto;
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
	}

	section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	section h3 {
		margin: 0;
		font-size: 0.95rem;
		font-weight: 600;
		color: #0f172a;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.confidence-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
		gap: 0.75rem;
	}

	.confidence-card {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		padding: 1rem;
		background: #f8fafc;
		border: 1px solid #e2e8f0;
		border-radius: 10px;
	}

	.confidence-card .label {
		font-size: 0.7rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: #64748b;
		font-weight: 600;
	}

	.confidence-card .value {
		font-size: 0.9rem;
		font-weight: 600;
	}

	.confidence-card .numeric {
		font-size: 1.1rem;
		font-weight: 700;
		color: #0f172a;
	}

	.confidence-high {
		color: #15803d;
	}

	.confidence-medium {
		color: #b45309;
	}

	.confidence-low {
		color: #b91c1c;
	}

	.value-display {
		background: #f8fafc;
		border: 1px solid #e2e8f0;
		border-radius: 10px;
		padding: 1rem;
	}

	.value-display pre {
		margin: 0;
		font-size: 0.85rem;
		color: #1f2937;
		white-space: pre-wrap;
		word-break: break-word;
		font-family: var(--font-mono);
	}

	.provenance-timeline {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		padding-left: 1rem;
	}

	.timeline-item {
		display: flex;
		gap: 1rem;
		position: relative;
	}

	.timeline-item:not(:last-child)::before {
		content: '';
		position: absolute;
		left: 19px;
		top: 40px;
		bottom: -16px;
		width: 2px;
		background: #e2e8f0;
	}

	.timeline-marker {
		width: 40px;
		height: 40px;
		border-radius: 10px;
		display: flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		background: rgba(59, 130, 246, 0.1);
		border: 2px solid currentColor;
	}

	.marker-icon {
		font-size: 1.2rem;
	}

	.timeline-content {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		padding-top: 0.25rem;
	}

	.timeline-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.source-label {
		font-size: 0.9rem;
		font-weight: 600;
		color: #0f172a;
	}

	.contribution {
		font-size: 0.85rem;
		font-weight: 600;
		color: #10b981;
	}

	.timeline-timestamp {
		font-size: 0.75rem;
		color: #94a3b8;
	}

	.evidence-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.evidence-item {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.75rem;
		background: #f8fafc;
		border: 1px solid #e2e8f0;
		border-radius: 8px;
	}

	.evidence-icon {
		font-size: 1rem;
	}

	.evidence-link {
		font-size: 0.85rem;
		color: #475569;
		font-family: var(--font-mono);
		word-break: break-all;
	}

	.metadata-grid {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.metadata-row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 0.75rem;
		background: #f8fafc;
		border: 1px solid #e2e8f0;
		border-radius: 8px;
	}

	.metadata-label {
		font-size: 0.85rem;
		font-weight: 600;
		color: #64748b;
	}

	.metadata-value {
		font-size: 0.85rem;
		color: #1f2937;
	}

	.modal-footer {
		padding: 1.5rem;
		border-top: 1px solid #e2e8f0;
		display: flex;
		justify-content: flex-end;
		gap: 0.75rem;
	}

	.btn-secondary {
		padding: 0.65rem 1.5rem;
		border-radius: 8px;
		border: 1px solid #e2e8f0;
		background: #ffffff;
		color: #1f2937;
		font-size: 0.9rem;
		font-weight: 600;
		cursor: pointer;
		transition: all 0.2s ease;
	}

	.btn-secondary:hover {
		background: #f8fafc;
		border-color: #cbd5e1;
	}

	@media (max-width: 640px) {
		.modal-content {
			max-width: 100%;
			max-height: 100vh;
			border-radius: 0;
		}

		.confidence-grid {
			grid-template-columns: 1fr;
		}
	}
</style>
