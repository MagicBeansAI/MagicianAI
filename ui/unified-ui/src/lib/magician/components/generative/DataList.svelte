<script lang="ts">
	export let items: Array<{id: string, key: string, value: string}> = [];
</script>

{#if items.length > 0}
	<dl class="muij-datalist">
		{#each items as item (item.id)}
			<dt class="muij-datalist-key">{item.key}</dt>
			<dd class="muij-datalist-value">{item.value}</dd>
		{/each}
	</dl>
{:else}
	<div class="muij-datalist-empty">
		<span class="muij-datalist-empty-text">No data</span>
	</div>
{/if}

<style>
	.muij-datalist {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 4px 12px;
		margin: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
	}

	.muij-datalist-key {
		color: var(--text-muted);
		font-weight: 500;
		overflow-wrap: anywhere;
	}

	.muij-datalist-value {
		color: var(--text-primary);
		margin: 0;
		overflow-wrap: anywhere;
	}

	.muij-datalist-empty {
		padding: 16px;
		text-align: center;
	}

	.muij-datalist-empty-text {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-muted);
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-datalist {
		font-family: var(--font-mono);
		gap: 8px 16px;
	}

	:global([data-theme^="retro-16bit"]) .muij-datalist-key {
		color: var(--text-primary);
		text-transform: uppercase;
		font-size: 0.75rem;
	}

	:global([data-theme^="retro-16bit"]) .muij-datalist-value {
		color: var(--text-secondary);
	}
</style>
