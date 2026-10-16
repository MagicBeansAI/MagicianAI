<script lang="ts">
	import AgentUpdateCard from '$lib/magician/components/agent-updates/AgentUpdateCard.svelte';
	import { fixtureEvents } from '$lib/magician/components/agent-updates/fixtures';
	import { hasDedicatedCard } from '$lib/magician/components/agent-updates/registry';

	const coverage = {
		total: fixtureEvents.length,
		dedicated: fixtureEvents.filter((e) => hasDedicatedCard(e.kind)).length
	};
</script>

<svelte:head>
	<title>Debug · Update cards</title>
</svelte:head>

<div class="preview-root">
	<header class="preview-header">
		<h1>Agent update cards</h1>
		<p class="preview-summary">
			{coverage.dedicated} of {coverage.total} variants have dedicated cards. The rest fall back
			to <code>UnknownCard</code>.
		</p>
	</header>

	<div class="preview-grid">
		{#each fixtureEvents as event (event.id)}
			<section class="preview-item" class:preview-fallback={!hasDedicatedCard(event.kind)}>
				<div class="preview-label">
					<span class="preview-kind">{event.kind}</span>
					{#if !hasDedicatedCard(event.kind)}
						<span class="preview-badge-fallback">fallback</span>
					{/if}
				</div>
				<AgentUpdateCard {event} />
			</section>
		{/each}
	</div>
</div>

<style>
	.preview-root {
		padding: var(--space-lg, 24px);
		display: flex;
		flex-direction: column;
		gap: var(--space-lg, 24px);
		max-width: 1200px;
		margin: 0 auto;
	}

	.preview-header h1 {
		font-family: var(--font-primary);
		font-size: 1.5rem;
		margin: 0 0 4px 0;
	}

	.preview-summary {
		color: var(--text-secondary);
		font-size: 0.9rem;
		margin: 0;
	}

	.preview-summary code {
		background: var(--bg-soft);
		padding: 2px 6px;
		border-radius: 4px;
		font-family: var(--font-mono);
	}

	.preview-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
		gap: var(--space-md, 16px);
	}

	.preview-item {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.preview-label {
		display: flex;
		gap: 8px;
		align-items: center;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--text-muted, #888);
	}

	.preview-kind::before {
		content: '•';
		margin-right: 6px;
		color: var(--color-info, #3b82f6);
	}

	.preview-fallback .preview-kind::before {
		color: var(--text-muted, #888);
	}

	.preview-badge-fallback {
		font-size: 0.65rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 1px 6px;
		border-radius: 4px;
		background: var(--bg-soft);
		color: var(--text-secondary);
	}
</style>
