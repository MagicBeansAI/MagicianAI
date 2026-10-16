<script lang="ts">
	export let children: string = '';
	export let variant: string = 'body';
	export let className: string = '';
	export let title: string = '';
</script>

{#if variant === 'code'}
	<code class={`muij-text muij-text-code ${className}`.trim()} title={title || undefined}>{children}</code>
{:else if variant === 'overline'}
	<span class={`muij-text muij-text-overline ${className}`.trim()} title={title || undefined}>{children}</span>
{:else if variant === 'caption'}
	<span class={`muij-text muij-text-caption ${className}`.trim()} title={title || undefined}>{children}</span>
{:else}
	<p class={`muij-text muij-text-body ${className}`.trim()} title={title || undefined}>{children}</p>
{/if}

<style>
	.muij-text {
		overflow-wrap: anywhere;
	}

	.muij-text-body {
		font-family: var(--font-primary);
		font-size: 0.875rem;
		font-weight: 400;
		line-height: 1.6;
		color: var(--text-primary);
		margin: 0;
	}

	.muij-text-caption {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 400;
		line-height: 1.4;
		color: var(--text-muted);
	}

	.muij-text-overline {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		font-weight: 600;
		line-height: 1.2;
		color: var(--text-muted);
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	.muij-text-code {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 400;
		line-height: 1.6;
		color: var(--text-primary);
		background: var(--bg-soft);
		border-radius: var(--radius-sm);
		padding: 2px 6px;
		width: fit-content; /* R30: prevent full-width stretch in flex/grid */
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-text {
		font-family: var(--font-mono) !important;
		text-transform: uppercase;
	}

	:global([data-theme^="retro-16bit"]) .muij-text-body,
	:global([data-theme^="retro-16bit"]) .muij-text-caption,
	:global([data-theme^="retro-16bit"]) .muij-text-overline {
		color: var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-text-code {
		border-radius: 0;
		border: 1px dashed var(--text-primary);
		background: var(--bg-base);
	}
</style>
