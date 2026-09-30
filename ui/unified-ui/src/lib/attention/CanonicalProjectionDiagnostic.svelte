<script lang="ts">
	import type { CanonicalAttentionProjection } from './canonicalAttentionProjection';

	export let projection: CanonicalAttentionProjection;
	export let compact = true;

	$: baseline = projection.policy.mode === 'baseline' ||
		projection.status === 'baseline_fallback' ||
		(projection.policy.mode === 'canary' && projection.policy.canary_fraction === 0);
	$: modeLabel = baseline
		? 'Learned routing inactive'
		: projection.policy.mode === 'shadow'
			? 'Learned routing preview'
			: 'Learned routing canary';
</script>

<div
	class="canonical-projection-diagnostic"
	class:canonical-projection-diagnostic--compact={compact}
	data-projection-id={projection.projection_id}
	data-projection-status={projection.status}
	data-policy-mode={projection.policy.mode}
	role="status"
>
	<strong>{modeLabel}</strong>
	<span aria-hidden="true">·</span>
	<span>
		Exact union {projection.integrity.materialized_total} unique/{projection.integrity.source_total} raw
	</span>
	{#if projection.integrity.duplicate_hidden_total > 0}
		<span aria-hidden="true">·</span>
		<span data-testid="canonical-duplicate-diagnostic">
			{projection.integrity.duplicate_hidden_total} exact
			{projection.integrity.duplicate_hidden_total === 1 ? ' duplicate' : ' duplicates'} hidden
		</span>
	{/if}
	{#if projection.policy.snapshot_id}
		<span aria-hidden="true">·</span>
		<span title={projection.policy.snapshot_id}>snapshot {projection.policy.snapshot_id.slice(0, 8)}</span>
	{/if}
</div>

<style>
	.canonical-projection-diagnostic {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		margin: 0.5rem 0;
		padding: 0.45rem 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: color-mix(in srgb, var(--bg-card) 88%, var(--color-info, #4d9de0));
		color: var(--text-secondary);
		font-size: 0.75rem;
	}

	.canonical-projection-diagnostic strong {
		color: var(--text-primary);
		font-weight: 650;
	}

	.canonical-projection-diagnostic--compact {
		margin-block: 0.35rem;
		padding-block: 0.35rem;
	}
</style>
