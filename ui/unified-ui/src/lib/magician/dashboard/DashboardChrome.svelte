<!--
  DashboardChrome — wraps every published dashboard with consistent header,
  container, atmosphere, and motion regardless of the body's source format.

  Props:
    title:        Dashboard title (header bar)
    summary:      Optional one-line subtitle
    lastRefreshed: Timestamp the surface was last computed (string or ms)
    publishedBy:  Optional agent attribution chip
    permalink:    Optional URL to copy via the "Share" button

  Slots:
    default — the rendered body (markdown / html / muij / etc.) placed
    inside the container. The chrome staggers direct children for the
    load animation via `--theme-motion-load-stagger`.

  Events:
    `magician:dashboard-refresh` — bubbled on click of the Refresh button.
    Data-source-bound chart components listen for this event on their
    ancestor and re-fetch.
-->
<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import AtmosphereLayer from './AtmosphereLayer.svelte';
	import type { DashboardTheme } from '$lib/stores/themeStore';

	export let title: string = '';
	export let summary: string = '';
	export let lastRefreshed: number | string | null = null;
	export let publishedBy: string | null = null;
	export let permalink: string | null = null;
	export let theme: DashboardTheme | null = null;

	const dispatch = createEventDispatcher<{ refresh: void }>();

	let chromeEl: HTMLElement | null = null;
	let refreshedLabel = '';

	$: refreshedLabel = formatRefreshed(lastRefreshed);

	function formatRefreshed(value: number | string | null): string {
		if (value === null || value === undefined) return '';
		let ms: number;
		if (typeof value === 'number') {
			ms = value;
		} else {
			const parsed = Date.parse(value);
			if (!Number.isFinite(parsed)) return value;
			ms = parsed;
		}
		const delta = Date.now() - ms;
		if (delta < 60_000) return 'just now';
		if (delta < 3_600_000) return `${Math.round(delta / 60_000)}m ago`;
		if (delta < 86_400_000) return `${Math.round(delta / 3_600_000)}h ago`;
		return new Date(ms).toLocaleString();
	}

	function handleRefresh(): void {
		dispatch('refresh');
		// Broadcast a DOM event so data-source-bound components anywhere
		// in the chrome subtree (chart blocks, tables, KPI tiles) re-fetch
		// in lock-step. Bubbles so the listener can sit on the chrome root.
		chromeEl?.dispatchEvent(
			new CustomEvent('magician:dashboard-refresh', { bubbles: true })
		);
	}

	async function handleCopyLink(): Promise<void> {
		if (!permalink) return;
		try {
			await navigator.clipboard.writeText(permalink);
		} catch {
			// Clipboard API may be unavailable in non-secure contexts. Fall
			// back to a synthetic textarea copy.
			const ta = document.createElement('textarea');
			ta.value = permalink;
			ta.style.position = 'fixed';
			ta.style.opacity = '0';
			document.body.appendChild(ta);
			ta.focus();
			ta.select();
			try {
				document.execCommand('copy');
			} catch {
				/* swallow */
			}
			ta.remove();
		}
	}

	onMount(() => {
		// Refresh the relative-time label every 30s while mounted.
		const interval = setInterval(() => {
			refreshedLabel = formatRefreshed(lastRefreshed);
		}, 30_000);
		return () => clearInterval(interval);
	});
</script>

<article
	class="dashboard-chrome"
	bind:this={chromeEl}
	style="--motion-stagger: var(--theme-motion-load-stagger, 60ms);"
>
	<AtmosphereLayer atmosphere={theme?.atmosphere} />

	<header class="chrome-header">
		<div class="header-text">
			{#if title}
				<h1 class="chrome-title">{title}</h1>
			{/if}
			{#if summary}
				<p class="chrome-summary">{summary}</p>
			{/if}
		</div>
		<div class="header-meta">
			{#if publishedBy}
				<span class="chip">{publishedBy}</span>
			{/if}
			{#if refreshedLabel}
				<span class="refreshed" title={typeof lastRefreshed === 'number' ? new Date(lastRefreshed).toISOString() : String(lastRefreshed)}>
					{refreshedLabel}
				</span>
			{/if}
			<button class="chrome-btn" type="button" on:click={handleRefresh} aria-label="Refresh dashboard">
				↻ Refresh
			</button>
			{#if permalink}
				<button class="chrome-btn ghost" type="button" on:click={handleCopyLink} aria-label="Copy permalink">
					Copy link
				</button>
			{/if}
		</div>
	</header>

	<div class="chrome-body">
		<slot />
	</div>
</article>

<style>
	.dashboard-chrome {
		position: relative;
		/* Match the app shell's content width (see routes/(app)/+layout.svelte
		   — `1320px`). The previous `960px` cap painted the chrome as a
		   narrow column even when the viewport is much wider, leaving
		   visible side-margins. */
		max-width: var(--theme-container-max-width, 1320px);
		margin: 0 auto;
		padding: var(--theme-container-padding-y, 48px) var(--theme-container-padding-x, 32px);
		/* Transparent — the app shell already paints `--bg-base`. Painting
		   `--theme-color-background` here lays a same-colour rectangle on
		   top of the shell, which on themes that diverge between
		   `--bg-base` and `--theme-color-background` reads as a separate
		   panel. Let the chrome inherit. */
		background-color: transparent;
		color: var(--theme-color-foreground, #1A1814);
		font-family: var(--theme-font-body, system-ui, sans-serif);
		line-height: var(--theme-prose-line-height, 1.65);
		box-sizing: border-box;
	}

	.chrome-header {
		position: relative;
		display: flex;
		justify-content: space-between;
		align-items: flex-end;
		gap: 24px;
		padding-bottom: 24px;
		margin-bottom: var(--theme-block-gap, 32px);
		border-bottom: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		flex-wrap: wrap;
		animation: chrome-fade-in var(--theme-motion-load-duration, 480ms)
			var(--theme-motion-load-easing, ease-out) both;
	}

	.header-text {
		min-width: 0;
		flex: 1 1 auto;
	}

	.chrome-title {
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: clamp(1.75rem, 1.4rem + 1.5vw, 2.75rem);
		font-weight: 600;
		line-height: 1.1;
		margin: 0;
		letter-spacing: -0.01em;
		color: var(--theme-color-foreground);
	}

	.chrome-summary {
		margin: 8px 0 0;
		font-size: 1.0625rem;
		color: var(--theme-color-foreground-muted, #666);
		max-width: 60ch;
	}

	.header-meta {
		display: flex;
		align-items: center;
		gap: 12px;
		flex-wrap: wrap;
		flex-shrink: 0;
	}

	.chip {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-size: 0.75rem;
		padding: 4px 10px;
		border-radius: 999px;
		background-color: var(--theme-color-surface, #fff);
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		color: var(--theme-color-foreground-muted);
		text-transform: lowercase;
		letter-spacing: 0.02em;
	}

	.refreshed {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-size: 0.8125rem;
		color: var(--theme-color-foreground-muted);
		cursor: help;
	}

	.chrome-btn {
		font-family: var(--theme-font-body);
		font-size: 0.875rem;
		font-weight: 500;
		padding: 8px 14px;
		border-radius: 6px;
		border: 1px solid var(--theme-color-border);
		background-color: var(--theme-color-surface);
		color: var(--theme-color-foreground);
		cursor: pointer;
		transition: transform 120ms ease-out, box-shadow 120ms ease-out, background-color 120ms ease-out;
	}

	.chrome-btn:hover {
		transform: translateY(calc(-1 * var(--theme-motion-hover-lift, 1px)));
		box-shadow: 0 2px 8px var(--theme-color-shadow, rgba(0, 0, 0, 0.06));
		background-color: var(--theme-color-accent);
		color: var(--theme-color-background, #fff);
		border-color: var(--theme-color-accent);
	}

	.chrome-btn.ghost {
		/* Subtle themed wash instead of fully transparent so the button
		   stays visually rooted in the active theme (paper / scanline /
		   gradient backgrounds would otherwise let the page texture
		   bleed straight through and the button would read as plain
		   bordered text). 65% surface gives a hint of the theme tint
		   while keeping the visual weight below the primary button. */
		background-color: color-mix(in srgb, var(--theme-color-surface) 65%, transparent);
		color: var(--theme-color-foreground-muted, var(--theme-color-foreground));
	}

	.chrome-body {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: var(--theme-block-gap, 32px);
	}

	/* Stagger direct children of the body using CSS variables — no JS
	   needed. The :nth-child trick gives each slot child a delay that
	   compounds by --motion-stagger. Caps at 12 children before falling
	   to a default delay. */
	.chrome-body > :global(*) {
		animation: block-rise var(--theme-motion-load-duration, 480ms)
			var(--theme-motion-load-easing, ease-out) both;
	}
	.chrome-body > :global(*:nth-child(1)) { animation-delay: calc(var(--motion-stagger) * 1); }
	.chrome-body > :global(*:nth-child(2)) { animation-delay: calc(var(--motion-stagger) * 2); }
	.chrome-body > :global(*:nth-child(3)) { animation-delay: calc(var(--motion-stagger) * 3); }
	.chrome-body > :global(*:nth-child(4)) { animation-delay: calc(var(--motion-stagger) * 4); }
	.chrome-body > :global(*:nth-child(5)) { animation-delay: calc(var(--motion-stagger) * 5); }
	.chrome-body > :global(*:nth-child(6)) { animation-delay: calc(var(--motion-stagger) * 6); }
	.chrome-body > :global(*:nth-child(7)) { animation-delay: calc(var(--motion-stagger) * 7); }
	.chrome-body > :global(*:nth-child(8)) { animation-delay: calc(var(--motion-stagger) * 8); }

	@keyframes chrome-fade-in {
		from { opacity: 0; transform: translateY(-4px); }
		to   { opacity: 1; transform: translateY(0); }
	}
	@keyframes block-rise {
		from { opacity: 0; transform: translateY(8px); }
		to   { opacity: 1; transform: translateY(0); }
	}

	@media (prefers-reduced-motion: reduce) {
		.chrome-header,
		.chrome-body > :global(*) {
			animation: none !important;
		}
	}
</style>
