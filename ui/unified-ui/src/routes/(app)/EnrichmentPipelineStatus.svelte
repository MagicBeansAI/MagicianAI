<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { EnrichmentSummary, EnricherResult } from '$lib/types/mission_control';

	export let enrichmentSummary: EnrichmentSummary | null = null;
	export let collapsed = true;

	const dispatch = createEventDispatcher<{
		viewFullLog: void;
	}>();

	// Persist collapsed state to localStorage
	let storageKey = 'enrichmentPipelineCollapsed';

	// Load collapsed state from localStorage on mount
	if (typeof window !== 'undefined') {
		const stored = localStorage.getItem(storageKey);
		if (stored !== null) {
			collapsed = stored === 'true';
		}
	}

	$: if (typeof window !== 'undefined') {
		localStorage.setItem(storageKey, collapsed.toString());
	}

	// Compute enricher results from summary
	$: enricherResults = computeEnricherResults(enrichmentSummary);

	function computeEnricherResults(summary: EnrichmentSummary | null): EnricherResult[] {
		if (!summary) return [];

		// Group errors by enricher
		const errorsByEnricher = new Map<string, string[]>();
		for (const error of summary.errors) {
			const errors = errorsByEnricher.get(error.enricher) || [];
			errors.push(error.message);
			errorsByEnricher.set(error.enricher, errors);
		}

		// Create enricher results
		const results: EnricherResult[] = [];
		const enricherNames = new Set<string>();

		// Add enrichers with errors
		for (const [enricher, messages] of errorsByEnricher) {
			enricherNames.add(enricher);
			results.push({
				name: enricher,
				status: 'error',
				slots_changed: 0,
				error_message: messages.join(', ')
			});
		}

		// For successful enrichers (invocations > errors), estimate from summary
		// This is a simplified approach - ideally backend would provide per-enricher stats
		if (summary.enrichments_applied > 0 && enricherNames.size === 0) {
			// No errors, so assume successful enrichment
			results.push({
				name: 'enrichment_pipeline',
				status: 'success',
				slots_changed: summary.slots_changed,
				error_message: undefined
			});
		}

		return results;
	}

	function toggleCollapsed() {
		collapsed = !collapsed;
	}

	function handleViewFullLog() {
		dispatch('viewFullLog');
	}

	function getStatusIcon(status: string): string {
		switch (status) {
			case 'success':
				return '✅';
			case 'error':
				return '⚠️';
			case 'skipped':
				return '⏭️';
			default:
				return '•';
		}
	}
</script>

{#if enrichmentSummary && enrichmentSummary.invocations > 0}
	<div class="enrichment-status" class:collapsed>
		<button class="status-header" on:click={toggleCollapsed} aria-expanded={!collapsed}>
			<span class="expand-icon">{collapsed ? '▶' : '▾'}</span>
			<span class="status-summary">
				{enrichmentSummary.invocations} enricher{enrichmentSummary.invocations === 1
					? ''
					: 's'} ran · {enrichmentSummary.slots_changed} slot{enrichmentSummary.slots_changed === 1
					? ''
					: 's'} changed
			</span>
			{#if enrichmentSummary.errors.length > 0}
				<span class="error-badge">{enrichmentSummary.errors.length} error(s)</span>
			{/if}
		</button>

		{#if !collapsed}
			<div class="status-content">
				{#if enricherResults.length > 0}
					<ul class="enricher-list">
						{#each enricherResults as result (result.name)}
							<li class="enricher-item {result.status}">
								<span class="status-icon">{getStatusIcon(result.status)}</span>
								<span class="enricher-name">{result.name}</span>
								{#if result.status === 'success'}
									<span class="slots-changed">
										({result.slots_changed} slot{result.slots_changed === 1 ? '' : 's'} changed)
									</span>
								{:else if result.status === 'skipped'}
									<span class="slots-changed">(no matches)</span>
								{/if}
								{#if result.error_message}
									<details class="error-details">
										<summary>Error details</summary>
										<pre>{result.error_message}</pre>
									</details>
								{/if}
							</li>
						{/each}
					</ul>
				{:else}
					<div class="no-enrichers">
						<p>No enrichers ran for these slots.</p>
					</div>
				{/if}

				{#if enrichmentSummary.errors.length > 0}
					<button class="view-full-log" on:click={handleViewFullLog}>
						View Full Enrichment Log →
					</button>
				{/if}
			</div>
		{/if}
	</div>
{/if}

<style>
	.enrichment-status {
		background: #ffffff;
		border: 1px solid #e2e8f0;
		border-radius: 8px;
		overflow: hidden;
		margin-top: 0.75rem;
	}

	.status-header {
		width: 100%;
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.65rem 0.75rem;
		background: #f8fafc;
		border: none;
		cursor: pointer;
		font-size: 0.85rem;
		color: #475569;
		transition: background 0.2s ease;
		text-align: left;
	}

	.status-header:hover {
		background: #f1f5f9;
	}

	.expand-icon {
		font-size: 0.7rem;
		color: #64748b;
		min-width: 12px;
	}

	.status-summary {
		flex: 1;
		font-weight: 500;
	}

	.error-badge {
		display: inline-flex;
		align-items: center;
		padding: 0.15rem 0.4rem;
		background: rgba(239, 68, 68, 0.15);
		color: #b91c1c;
		border-radius: 999px;
		font-size: 0.7rem;
		font-weight: 600;
	}

	.status-content {
		padding: 0.75rem;
		background: #ffffff;
	}

	.enricher-list {
		list-style: none;
		padding: 0;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.enricher-item {
		display: flex;
		align-items: flex-start;
		gap: 0.5rem;
		padding: 0.5rem;
		background: #f8fafc;
		border-radius: 6px;
		font-size: 0.8rem;
		border: 1px solid #e2e8f0;
	}

	.enricher-item.success {
		background: rgba(34, 197, 94, 0.08);
		border-color: rgba(34, 197, 94, 0.2);
	}

	.enricher-item.error {
		background: rgba(239, 68, 68, 0.08);
		border-color: rgba(239, 68, 68, 0.2);
	}

	.enricher-item.skipped {
		background: rgba(148, 163, 184, 0.08);
		border-color: rgba(148, 163, 184, 0.2);
		opacity: 0.7;
	}

	.status-icon {
		font-size: 1rem;
		min-width: 20px;
	}

	.enricher-name {
		flex: 1;
		font-weight: 600;
		color: #1f2937;
		font-family: var(--font-mono);
	}

	.slots-changed {
		color: #64748b;
		font-size: 0.75rem;
	}

	.error-details {
		width: 100%;
		margin-top: 0.5rem;
	}

	.error-details summary {
		cursor: pointer;
		color: #b91c1c;
		font-size: 0.75rem;
		font-weight: 600;
		padding: 0.25rem 0;
	}

	.error-details summary:hover {
		text-decoration: underline;
	}

	.error-details pre {
		margin: 0.5rem 0 0 0;
		padding: 0.5rem;
		background: rgba(239, 68, 68, 0.05);
		border: 1px solid rgba(239, 68, 68, 0.15);
		border-radius: 4px;
		font-size: 0.7rem;
		color: #991b1b;
		white-space: pre-wrap;
		word-break: break-word;
	}

	.no-enrichers {
		padding: 1rem;
		text-align: center;
		color: #94a3b8;
		font-size: 0.85rem;
	}

	.view-full-log {
		margin-top: 0.75rem;
		padding: 0.5rem 0.75rem;
		background: #3b82f6;
		color: #ffffff;
		border: none;
		border-radius: 6px;
		font-size: 0.8rem;
		font-weight: 600;
		cursor: pointer;
		transition: all 0.2s ease;
	}

	.view-full-log:hover {
		background: #2563eb;
		box-shadow: 0 2px 8px rgba(59, 130, 246, 0.3);
	}
</style>
