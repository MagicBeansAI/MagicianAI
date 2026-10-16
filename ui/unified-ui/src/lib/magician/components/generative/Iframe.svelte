<!--
  Iframe — embedded URL container for marimo notebooks and other external
  interactive surfaces.

  Agents that want fully interactive analytics (sliders, dropdowns,
  time-range pickers) can run a marimo notebook, get a served URL, and
  emit MUI-JSON with:

    { "type": "Iframe", "url": "http://localhost:7000/notebook", "height": 600 }

  Sandbox defaults to `allow-scripts allow-same-origin` — enough for
  marimo's WebSocket protocol; can be tightened/loosened per embed.

  Responsive: defaults to a fixed pixel height. Pass `aspectRatio` for a
  responsive aspect-ratio container instead.
-->
<script lang="ts">
	import { sanitizeCssValue } from './cssUtil';

	export let url: string = '';
	export let height: number = 600;
	export let aspectRatio: string | undefined = undefined;
	/** Iframe sandbox attribute. Default permits scripts + same-origin
	 * which marimo needs for its WebSocket. Set to "" for a stricter
	 * sandbox; set to undefined to omit the sandbox attribute entirely. */
	export let sandbox: string | undefined = 'allow-scripts allow-same-origin';
	export let title: string = 'Embedded notebook';

	function isSafeUrl(u: string): boolean {
		const t = u.trim().toLowerCase();
		return (
			t.startsWith('https://') ||
			t.startsWith('http://localhost') ||
			t.startsWith('http://127.0.0.1') ||
			t.startsWith('/')
		);
	}

	$: safeUrl = isSafeUrl(url) ? url : '';
	$: containerStyle = aspectRatio
		? `aspect-ratio: ${sanitizeCssValue(aspectRatio)}; width: 100%;`
		: `height: ${Number.isFinite(height) ? height : 600}px; width: 100%;`;
</script>

{#if !safeUrl}
	<div class="iframe-error">⚠ Unsafe iframe URL blocked (must be https, localhost, or relative path).</div>
{:else}
	<div class="iframe-container" style={containerStyle}>
		<iframe
			src={safeUrl}
			title={title}
			sandbox={sandbox}
			loading="lazy"
			referrerpolicy="no-referrer"
		></iframe>
	</div>
{/if}

<style>
	.iframe-container {
		display: block;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 8px;
		overflow: hidden;
		background-color: var(--theme-color-surface, #fff);
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
	}

	.iframe-container iframe {
		width: 100%;
		height: 100%;
		border: none;
		display: block;
	}

	.iframe-error {
		padding: 16px;
		border: 1px solid var(--theme-color-accent, #c00);
		border-radius: 6px;
		background-color: var(--theme-color-surface);
		color: var(--theme-color-accent);
		font-family: var(--theme-font-mono, monospace);
		font-size: 0.85rem;
	}
</style>
