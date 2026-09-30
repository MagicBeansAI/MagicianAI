<script lang="ts">
	import type { PlanProvenance } from '$lib/types/plangraph';

	export let provenance: PlanProvenance;
	export let attemptNumber: number = 1;

	let isExpanded = false;

	// Get strategy badge color
	$: strategyColor =
		provenance.strategy === 'GuidedSearch'
			? 'purple'
			: provenance.strategy === 'AtomicComposition'
				? 'blue'
				: 'gray';

	// Format strategy name for display
	$: strategyDisplay =
		provenance.strategy === 'GuidedSearch' ? 'GuidedSearch' : provenance.strategy || 'Unknown';

	// Check if there are additional details to show
	$: hasDetails = provenance.generator || provenance.notes;
</script>

<div class="provenance-card" class:expanded={isExpanded}>
	<div class="card-header" on:click={() => (isExpanded = !isExpanded)} on:keypress={(e) => e.key === 'Enter' && (isExpanded = !isExpanded)} role="button" tabindex="0">
		<div class="header-left">
			<div class="strategy-badge {strategyColor}">
				{#if provenance.strategy === 'GuidedSearch'}
					<span class="badge-icon">🎯</span>
				{:else if provenance.strategy === 'AtomicComposition'}
					<span class="badge-icon">⚛️</span>
				{:else}
					<span class="badge-icon">🔧</span>
				{/if}
				<span class="badge-text">{strategyDisplay}</span>
			</div>

			<div class="attempt-info">
				<span class="attempt-label">Attempt {attemptNumber}</span>
			</div>
		</div>

		<div class="header-right">
			{#if hasDetails}
				<button class="expand-btn" class:rotated={isExpanded} aria-label={isExpanded ? 'Collapse' : 'Expand'}>
					<svg
						width="16"
						height="16"
						viewBox="0 0 16 16"
						fill="currentColor"
						class="chevron-icon"
					>
						<path
							fill-rule="evenodd"
							d="M4.293 5.293a1 1 0 011.414 0L8 7.586l2.293-2.293a1 1 0 111.414 1.414l-3 3a1 1 0 01-1.414 0l-3-3a1 1 0 010-1.414z"
							clip-rule="evenodd"
						/>
					</svg>
				</button>
			{/if}
		</div>
	</div>

	{#if isExpanded && hasDetails}
		<div class="card-details">
			{#if provenance.generator}
				<div class="detail-item">
					<span class="detail-label">Generator:</span>
					<span class="detail-value">{provenance.generator}</span>
				</div>
			{/if}

			{#if provenance.notes}
				<div class="detail-item">
					<span class="detail-label">Notes:</span>
					<div class="detail-notes">
						{provenance.notes}
					</div>
				</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.provenance-card {
		background: white;
		border: 1px solid #e2e8f0;
		border-radius: 8px;
		overflow: hidden;
		transition: all 0.2s ease;
	}

	.provenance-card:hover {
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.08);
	}

	.provenance-card.expanded {
		border-color: #cbd5e1;
	}

	/* Card Header */
	.card-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 0.75rem 1rem;
		cursor: pointer;
		user-select: none;
		transition: background 0.2s;
	}

	.card-header:hover {
		background: #f8fafc;
	}

	.header-left {
		display: flex;
		align-items: center;
		gap: 1rem;
		flex: 1;
	}

	.header-right {
		display: flex;
		align-items: center;
	}

	/* Strategy Badge */
	.strategy-badge {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.375rem 0.75rem;
		border-radius: 6px;
		font-size: 0.8rem;
		font-weight: 600;
		transition: all 0.2s;
	}

	.strategy-badge.purple {
		background: linear-gradient(135deg, #8b5cf6 0%, #7c3aed 100%);
		color: white;
		box-shadow: 0 2px 8px rgba(139, 92, 246, 0.3);
	}

	.strategy-badge.blue {
		background: linear-gradient(135deg, #3b82f6 0%, #2563eb 100%);
		color: white;
		box-shadow: 0 2px 8px rgba(59, 130, 246, 0.3);
	}

	.strategy-badge.gray {
		background: linear-gradient(135deg, #64748b 0%, #475569 100%);
		color: white;
		box-shadow: 0 2px 8px rgba(100, 116, 139, 0.3);
	}

	.badge-icon {
		font-size: 1rem;
		line-height: 1;
	}

	.badge-text {
		font-size: 0.75rem;
		letter-spacing: 0.025em;
		text-transform: uppercase;
	}

	/* Attempt Info */
	.attempt-info {
		display: flex;
		align-items: center;
	}

	.attempt-label {
		font-size: 0.75rem;
		font-weight: 500;
		color: #64748b;
		padding: 0.25rem 0.5rem;
		background: #f1f5f9;
		border-radius: 4px;
		border: 1px solid #cbd5e1;
	}

	/* Expand Button */
	.expand-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		background: transparent;
		border: none;
		color: #64748b;
		cursor: pointer;
		padding: 0.25rem;
		border-radius: 4px;
		transition: all 0.2s;
	}

	.expand-btn:hover {
		background: #f1f5f9;
		color: #1e293b;
	}

	.expand-btn.rotated .chevron-icon {
		transform: rotate(180deg);
	}

	.chevron-icon {
		transition: transform 0.2s ease;
	}

	/* Card Details */
	.card-details {
		padding: 0.75rem 1rem 1rem 1rem;
		background: #f8fafc;
		border-top: 1px solid #e2e8f0;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.detail-item {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.detail-label {
		font-size: 0.7rem;
		font-weight: 600;
		color: #64748b;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.detail-value {
		font-size: 0.8rem;
		color: #1e293b;
		font-family: var(--font-mono);
		background: white;
		padding: 0.375rem 0.5rem;
		border-radius: 4px;
		border: 1px solid #e2e8f0;
	}

	.detail-notes {
		font-size: 0.75rem;
		color: #475569;
		line-height: 1.5;
		background: white;
		padding: 0.5rem;
		border-radius: 4px;
		border: 1px solid #e2e8f0;
		white-space: pre-wrap;
		word-break: break-word;
	}

	/* Responsive */
	@media (max-width: 640px) {
		.header-left {
			flex-direction: column;
			align-items: flex-start;
			gap: 0.5rem;
		}

		.strategy-badge {
			font-size: 0.75rem;
			padding: 0.25rem 0.5rem;
		}

		.attempt-label {
			font-size: 0.7rem;
		}
	}
</style>
