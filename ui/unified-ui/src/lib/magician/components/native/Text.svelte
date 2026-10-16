<script lang="ts">
	export let children = '';
	export let variant = 'body';
	export let className = '';
	export let title = '';
</script>

{#if variant === 'code'}
	<code class={['native-text', 'native-text--code', className].filter(Boolean).join(' ')} title={title || undefined}>{children}</code>
{:else if variant === 'overline'}
	<span class={['native-text', 'native-text--overline', className].filter(Boolean).join(' ')} title={title || undefined}>{children}</span>
{:else if variant === 'caption'}
	<span class={['native-text', 'native-text--caption', className].filter(Boolean).join(' ')} title={title || undefined}>{children}</span>
{:else}
	<p class={['native-text', 'native-text--body', className].filter(Boolean).join(' ')} title={title || undefined}>{children}</p>
{/if}

<style>
	.native-text {
		overflow-wrap: anywhere;
	}

	.native-text--body {
		margin: 0;
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.875rem;
		line-height: 1.6;
	}

	.native-text--caption {
		color: var(--text-muted);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		line-height: 1.4;
	}

	.native-text--overline {
		color: var(--text-muted);
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		font-weight: 700;
		line-height: 1.2;
		text-transform: uppercase;
	}

	.native-text--code {
		width: fit-content;
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft);
		color: var(--text-primary);
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		line-height: 1.6;
		padding: 0.125rem 0.375rem;
	}

	:global([data-theme^='retro-16bit']) .native-text {
		font-family: var(--font-mono);
		text-transform: uppercase;
	}

	:global([data-theme^='retro-16bit']) .native-text--code {
		border-radius: 0;
		border: 1px dashed var(--text-primary);
		background: var(--bg-base);
	}
</style>
