<!--
  MarkdownDashboard — editorial-quality renderer for `text/markdown` task
  user-output. Wraps the existing low-level `<Markdown>` component (which
  handles parsing) with a styled container that adopts theme tokens.

  Custom rules applied via CSS on the scope:
    H1/H2 get an accent rule beneath
    H3 gets a numbered marker
    Tables look like cards (rounded, zebra rows, shadow)
    Blockquotes get accent border-left
    Inline code gets a pill background
    Code blocks get the mono font + theme surface
    Lists get hanging indent for readability

  KPI auto-extract (separate component KpiTileRow) lives in
  MarkdownDashboardKpis.svelte and is invoked by the dispatcher when the
  body has KPI-shaped lines.
-->
<script lang="ts">
	import Markdown from '$lib/magician/components/generative/Markdown.svelte';
	import MarkdownDashboardKpis from './MarkdownDashboardKpis.svelte';

	export let content: string = '';
	/**
	 * When true, scan the body for KPI-shaped lines (e.g. "Total: $47.23 (KPI)")
	 * and lift them to a tile row above the markdown body. Default true —
	 * the renderer is opt-out via the dispatcher.
	 */
	export let extractKpis: boolean = true;
</script>

<section class="markdown-dashboard">
	{#if extractKpis}
		<MarkdownDashboardKpis source={content} />
	{/if}
	<div class="md-prose">
		<Markdown {content} />
	</div>
</section>

<style>
	.markdown-dashboard {
		display: flex;
		flex-direction: column;
		gap: var(--theme-block-gap, 32px);
	}

	.md-prose {
		font-family: var(--theme-font-body, system-ui, sans-serif);
		color: var(--theme-color-foreground, #1A1814);
		line-height: var(--theme-prose-line-height, 1.65);
	}

	/* Headings — use display font, scale via theme heading_scale. */
	.md-prose :global(h1),
	.md-prose :global(h2),
	.md-prose :global(h3),
	.md-prose :global(h4),
	.md-prose :global(h5),
	.md-prose :global(h6) {
		font-family: var(--theme-font-display, var(--theme-font-body));
		color: var(--theme-color-foreground);
		font-weight: 600;
		line-height: 1.15;
		letter-spacing: -0.005em;
		margin: 1.5em 0 0.5em;
	}

	.md-prose :global(h1) {
		font-size: calc(1rem * pow(var(--theme-heading-scale, 1.25), 4));
		margin-top: 0;
		padding-bottom: 0.4em;
		border-bottom: 2px solid var(--theme-color-accent, #C24E1B);
	}
	.md-prose :global(h2) {
		font-size: calc(1rem * pow(var(--theme-heading-scale, 1.25), 3));
		padding-bottom: 0.3em;
		border-bottom: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
	}
	.md-prose :global(h3) {
		font-size: calc(1rem * pow(var(--theme-heading-scale, 1.25), 2));
		color: var(--theme-color-accent);
	}
	.md-prose :global(h4) { font-size: calc(1rem * var(--theme-heading-scale, 1.25)); }

	/* Paragraphs */
	.md-prose :global(p) {
		margin: 0 0 1em;
		max-width: 70ch;
	}

	/* Lists — hanging indent */
	.md-prose :global(ul),
	.md-prose :global(ol) {
		padding-left: 1.4em;
		margin: 0 0 1em;
	}
	.md-prose :global(li) {
		margin-bottom: 0.25em;
	}

	/* Inline code */
	.md-prose :global(code) {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-size: 0.92em;
		padding: 0.15em 0.4em;
		border-radius: 4px;
		background-color: var(--theme-color-surface, rgba(255, 255, 255, 0.7));
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.06));
		color: var(--theme-color-foreground);
	}

	/* Code blocks */
	.md-prose :global(pre) {
		font-family: var(--theme-font-mono);
		background-color: var(--theme-color-surface);
		border: 1px solid var(--theme-color-border);
		border-radius: 8px;
		padding: 16px 18px;
		overflow-x: auto;
		margin: 1em 0;
		font-size: 0.92em;
		line-height: 1.55;
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
	}
	.md-prose :global(pre code) {
		padding: 0;
		border: none;
		background: transparent;
	}

	/* Blockquotes */
	.md-prose :global(blockquote) {
		margin: 1em 0;
		padding: 4px 18px;
		border-left: 3px solid var(--theme-color-accent);
		color: var(--theme-color-foreground-muted, #666);
		font-style: italic;
		font-size: 1.04em;
	}

	/* Tables — card-like */
	.md-prose :global(table) {
		width: 100%;
		border-collapse: separate;
		border-spacing: 0;
		margin: 1.4em 0;
		font-size: 0.92em;
		border: 1px solid var(--theme-color-border);
		border-radius: 8px;
		overflow: hidden;
		box-shadow: 0 1px 3px var(--theme-color-shadow);
		background-color: var(--theme-color-surface);
	}
	.md-prose :global(th),
	.md-prose :global(td) {
		padding: 10px 14px;
		text-align: left;
		border-bottom: 1px solid var(--theme-color-border);
	}
	.md-prose :global(thead th) {
		background-color: var(--theme-color-background);
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-weight: 600;
		color: var(--theme-color-foreground);
		text-transform: uppercase;
		letter-spacing: 0.04em;
		font-size: 0.78em;
	}
	.md-prose :global(tbody tr:nth-child(even) td) {
		background-color: color-mix(in srgb, var(--theme-color-background) 50%, transparent);
	}
	.md-prose :global(tbody tr:last-child td) {
		border-bottom: none;
	}

	/* Links */
	.md-prose :global(a) {
		color: var(--theme-color-accent);
		text-decoration: underline;
		text-decoration-thickness: 1px;
		text-underline-offset: 2px;
	}
	.md-prose :global(a:hover) {
		text-decoration-thickness: 2px;
	}

	/* Horizontal rule */
	.md-prose :global(hr) {
		margin: 2em 0;
		border: none;
		border-top: 1px solid var(--theme-color-border);
	}

	/* Images */
	.md-prose :global(img) {
		max-width: 100%;
		height: auto;
		border-radius: 6px;
		display: block;
		margin: 1em 0;
	}
</style>
