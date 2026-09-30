<!--
  HtmlDashboard — renders sanitized HTML inside a theme-cascaded scope.

  Agent emits raw HTML as the task user-output (`media_type: text/html`).
  We sanitize through `sanitizeHtml`, then drop the result inside a scoped
  container whose CSS variables inherit from the chrome. Inline styles
  from the agent are allowed and override the scope defaults — agents
  can position elements freely, but unstyled HTML still picks up theme
  typography automatically.

  Post-render: a `data-magician-source` attribute scanner (Task 14) walks
  the mounted scope and swaps placeholder elements with live-data charts.
  That work lives in HtmlDashboardLiveBindings.svelte to keep this
  component focused on the static-render path.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { sanitizeHtml } from './sanitizeHtml';
	import HtmlDashboardLiveBindings from './HtmlDashboardLiveBindings.svelte';

	export let html: string = '';

	let scopeEl: HTMLElement | null = null;
	let cleanHtml: string = '';

	$: cleanHtml = sanitizeHtml(html);

	onMount(() => {
		// scopeEl is bound; HtmlDashboardLiveBindings observes it.
	});
</script>

<div class="dashboard-html-scope" bind:this={scopeEl}>
	{@html cleanHtml}
</div>

{#if scopeEl}
	<HtmlDashboardLiveBindings scopeRoot={scopeEl} />
{/if}

<style>
	.dashboard-html-scope {
		/* Cascade theme tokens into the agent's HTML. Inline `style` attributes
		 * on the agent's elements override these (and are sanitized through the
		 * SAFE_STYLE_PROP allowlist), so agents can position freely while still
		 * inheriting the active theme's typography and palette by default. */
		font-family: var(--theme-font-body, system-ui, sans-serif);
		color: var(--theme-color-foreground, #1A1814);
		line-height: var(--theme-prose-line-height, 1.65);
	}

	.dashboard-html-scope :global(h1),
	.dashboard-html-scope :global(h2),
	.dashboard-html-scope :global(h3),
	.dashboard-html-scope :global(h4),
	.dashboard-html-scope :global(h5),
	.dashboard-html-scope :global(h6) {
		font-family: var(--theme-font-display, var(--theme-font-body));
		color: var(--theme-color-foreground);
		font-weight: 600;
		line-height: 1.15;
	}

	.dashboard-html-scope :global(a) {
		color: var(--theme-color-accent);
		text-decoration: underline;
	}

	.dashboard-html-scope :global(code),
	.dashboard-html-scope :global(pre) {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
	}

	.dashboard-html-scope :global(pre) {
		background-color: var(--theme-color-surface);
		border: 1px solid var(--theme-color-border);
		border-radius: 8px;
		padding: 16px;
		overflow-x: auto;
	}

	.dashboard-html-scope :global(table) {
		width: 100%;
		border-collapse: separate;
		border-spacing: 0;
		margin: 1em 0;
		border: 1px solid var(--theme-color-border);
		border-radius: 8px;
		overflow: hidden;
		background-color: var(--theme-color-surface);
	}

	.dashboard-html-scope :global(th),
	.dashboard-html-scope :global(td) {
		padding: 10px 14px;
		text-align: left;
		border-bottom: 1px solid var(--theme-color-border);
	}

	.dashboard-html-scope :global(thead th) {
		background-color: var(--theme-color-background);
		text-transform: uppercase;
		letter-spacing: 0.04em;
		font-size: 0.78em;
	}

	.dashboard-html-scope :global(blockquote) {
		margin: 1em 0;
		padding: 4px 18px;
		border-left: 3px solid var(--theme-color-accent);
		color: var(--theme-color-foreground-muted);
	}
</style>
