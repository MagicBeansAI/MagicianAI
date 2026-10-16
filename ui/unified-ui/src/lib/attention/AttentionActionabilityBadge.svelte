<script lang="ts">
	import {
		actionabilityPresentation,
		type AttentionActionabilityCard
	} from '$lib/channel/channelFollowUpLearning';

	export let metadata: AttentionActionabilityCard | null = null;

	$: presentation = actionabilityPresentation(metadata);
	$: auditTitle = metadata
		? [
				metadata.explanation?.code ? `Explanation ${metadata.explanation.code}` : null,
				metadata.model_version ? `Model ${metadata.model_version}` : null,
				metadata.snapshot_id ? `Snapshot ${metadata.snapshot_id}` : null
			]
				.filter(Boolean)
				.join(' · ')
		: '';
</script>

{#if presentation}
	<div
		class="attention-actionability"
		class:attention-actionability--active={presentation.active}
		class:attention-actionability--degraded={presentation.degraded}
		title={auditTitle || undefined}
		aria-label={auditTitle ? `${presentation.label} · ${auditTitle}` : presentation.label}
		data-testid="attention-actionability"
	>
		<span aria-hidden="true">{presentation.active ? '◆' : presentation.degraded ? '△' : '◇'}</span>
		{presentation.label}
	</div>
{/if}

<style>
	.attention-actionability {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		align-self: flex-start;
		padding: 0.2rem 0.5rem;
		border: 1px solid color-mix(in srgb, var(--color-info, #4d9de0) 30%, var(--border-soft));
		border-radius: 999px;
		background: color-mix(in srgb, var(--color-info, #4d9de0) 7%, var(--bg-card));
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-variant-numeric: tabular-nums;
	}

	.attention-actionability--active {
		border-color: color-mix(in srgb, var(--color-success, #2f8f5b) 38%, var(--border-soft));
		background: color-mix(in srgb, var(--color-success, #2f8f5b) 10%, var(--bg-card));
		color: var(--color-success, #2f8f5b);
	}

	.attention-actionability--degraded {
		border-color: color-mix(in srgb, var(--color-warning, #d28b1a) 35%, var(--border-soft));
		background: color-mix(in srgb, var(--color-warning, #d28b1a) 8%, var(--bg-card));
		color: var(--color-warning, #9a6410);
	}
</style>
