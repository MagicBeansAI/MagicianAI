<script lang="ts">
	/**
	 * Theme specimen sheet.
	 *
	 * Every shipped theme, light and dark side by side, rendering the same set
	 * of elements from its OWN rules. Each plate is its own document: the sheet
	 * embeds `?plate=<id>` in an iframe, and that document puts the theme on
	 * <html> before painting, so tokens, the `body`-scoped overrides twelve
	 * themes rely on, textures, and daisyUI's theme variables all apply exactly
	 * as in the app. A section-scoped `data-theme` could not reach the body
	 * rules, which is why the first cut mis-painted Risograph, Retro and Mario.
	 *
	 * Lives under /dev like the structured-response fixture: outside the
	 * `(app)` auth gate, no backend calls, nothing to persist. The root layout
	 * skips the theme store on this route so a plate's <html data-theme> is
	 * never republished as the user's choice.
	 */
	import { onMount } from 'svelte';
	import { ensureThemeFonts } from '$lib/shared/themeFonts';
	import { BRAND_MARK_PATH, BRAND_MARK_VIEWBOX } from '$lib/shared/brand/mark';
	import ThemePlate from './ThemePlate.svelte';
	import { families, isPlateId, plateIds, plateNumber, type Plate } from './plates';

	/** `null` until mount: the server renders the neutral frame only, never an
	 *  iframe, so a prerendered page can never embed itself recursively. */
	let mode = $state<'pending' | 'sheet' | { plate: Plate; number: string }>('pending');

	const plateHeights = $state<Record<string, number>>({});

	/** A plate document tells the sheet how tall it is. Measured by the child
	 *  after it has switched to its single plate (an iframe `load` event fires
	 *  while the child still shows the pre-hydration frame, which is 22
	 *  placeholders tall) and again whenever fonts or images settle. */
	const HEIGHT_MESSAGE = 'magican-plate-height';

	function reportHeight(id: string): void {
		if (window.parent === window) return;
		const height = document.documentElement.scrollHeight;
		window.parent.postMessage({ type: HEIGHT_MESSAGE, id, height }, window.location.origin);
	}

	onMount(() => {
		const wanted = new URLSearchParams(window.location.search).get('plate');
		if (isPlateId(wanted)) {
			let found: { plate: Plate; number: string } | null = null;
			families.forEach((family, familyIndex) =>
				family.plates.forEach((plate, plateIndex) => {
					if (plate.id === wanted) found = { plate, number: plateNumber(familyIndex, plateIndex) };
				})
			);
			if (found) {
				document.documentElement.setAttribute('data-theme', wanted);
				ensureThemeFonts(wanted);
				mode = found;
				const observer = new ResizeObserver(() => reportHeight(wanted));
				requestAnimationFrame(() => {
					reportHeight(wanted);
					observer.observe(document.body);
				});
				return () => observer.disconnect();
			}
		}
		mode = 'sheet';
		const onMessage = (event: MessageEvent) => {
			if (event.origin !== window.location.origin) return;
			const data = event.data as { type?: unknown; id?: unknown; height?: unknown } | null;
			if (!data || data.type !== HEIGHT_MESSAGE || typeof data.id !== 'string' || typeof data.height !== 'number') return;
			plateHeights[data.id] = Math.max(480, Math.ceil(data.height));
		};
		window.addEventListener('message', onMessage);
		return () => window.removeEventListener('message', onMessage);
	});
</script>

<svelte:head>
	<title>Magican · theme specimen sheet</title>
</svelte:head>

{#if typeof mode === 'object'}
	<main class="standalone">
		<ThemePlate item={mode.plate} number={mode.number} />
	</main>
{:else}
	<div class="gallery">
		<header class="frame-head">
			<div class="frame-brand">
				<span class="brand-tile frame-tile" aria-hidden="true">
					<svg width="18" height="18" viewBox={BRAND_MARK_VIEWBOX} fill="currentColor"><path d={BRAND_MARK_PATH} /></svg>
				</span>
				<div>
					<h1>Theme specimen sheet</h1>
					<p>Every shipped theme, light beside dark, each rendered as its own document so it looks exactly as it does in the app. The frame is neutral on purpose; the plates carry the colour.</p>
				</div>
			</div>
			<nav class="rail" aria-label="Families">
				{#each families as family, familyIndex}
					<a href={`#${family.plates[0].id}`}><span class="rail-no">{String(familyIndex + 1).padStart(2, '0')}</span>{family.name}</a>
				{/each}
			</nav>
		</header>

		{#each families as family, familyIndex}
			<div class="family">
				<h2 class="family-name"><span class="rail-no">{String(familyIndex + 1).padStart(2, '0')}</span>{family.name}</h2>
				<div class="pair">
					{#each family.plates as item, plateIndex}
						<div class="slot" id={item.id} style={`--slot-h: ${plateHeights[item.id] ?? 1180}px`}>
							<div class="slot-cap">
								<span class="rail-no">{plateNumber(familyIndex, plateIndex)}</span>{item.name}
								<a class="slot-open" href={`?plate=${item.id}`} target="_blank" rel="noopener">open alone ↗</a>
							</div>
							{#if mode === 'sheet'}
								<iframe title={`${item.name} plate`} src={`?plate=${item.id}`} loading="lazy"></iframe>
							{:else}
								<div class="slot-wait" aria-hidden="true"></div>
							{/if}
						</div>
					{/each}
				</div>
			</div>
		{/each}

		<footer class="frame-foot">
			<p>{plateIds.length} plates · {families.length} families · each plate is its own document with the theme on &lt;html&gt;</p>
		</footer>
	</div>
{/if}

<style>
	.standalone { padding: 1rem; }

	/* ── Frame: fixed neutral in the product's own voices, never themed ── */
	.gallery {
		--frame-bg: #e9e6df;
		--frame-ink: #1f1c18;
		--frame-muted: #6b645a;
		--frame-line: rgba(31, 28, 24, 0.14);
		background:
			radial-gradient(1200px 600px at 10% -10%, rgba(255, 107, 107, 0.10), transparent 60%),
			var(--frame-bg);
		color: var(--frame-ink);
		min-height: 100vh;
		padding: 2.5rem clamp(1rem, 4vw, 3.5rem) 4rem;
		font-family: 'Manrope', -apple-system, BlinkMacSystemFont, sans-serif;
	}
	.frame-head {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 2rem;
		align-items: end;
		padding-bottom: 1.5rem;
		border-bottom: 1px solid var(--frame-line);
		margin-bottom: 2.5rem;
	}
	.frame-brand { display: flex; gap: 1rem; align-items: flex-start; }
	.frame-head h1 {
		margin: 0 0 0.35rem;
		font-family: 'Outfit', sans-serif;
		font-weight: 700;
		font-size: clamp(1.6rem, 3vw, 2.4rem);
		letter-spacing: -0.02em;
		line-height: 1.05;
	}
	.frame-head p { margin: 0; max-width: 60ch; color: var(--frame-muted); line-height: 1.5; }
	.brand-tile {
		width: 30px; height: 30px; border-radius: 9px;
		background: var(--coral, #ff6b6b); color: #fff;
		display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0;
	}
	.frame-tile { width: 36px; height: 36px; border-radius: 11px; margin-top: 0.15rem; }
	.rail { display: flex; flex-wrap: wrap; gap: 0.35rem 0.9rem; max-width: 34rem; justify-content: flex-end; }
	.rail a {
		font-family: 'Geist Mono', 'JetBrains Mono', monospace;
		font-size: 0.72rem; letter-spacing: 0.02em;
		color: var(--frame-ink); text-decoration: none;
		border-bottom: 1px dashed transparent; padding-bottom: 1px;
	}
	.rail a:hover { border-bottom-color: var(--frame-ink); }
	.rail-no { color: var(--frame-muted); margin-right: 0.35rem; font-family: 'Geist Mono', 'JetBrains Mono', monospace; font-size: 0.72rem; }
	.family { margin-bottom: 3rem; }
	.family-name {
		font-family: 'Outfit', sans-serif; font-weight: 600; font-size: 1.05rem;
		letter-spacing: 0.01em; margin: 0 0 0.9rem; display: flex; align-items: baseline;
	}
	.pair { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 560px), 1fr)); gap: 1.25rem; align-items: start; }
	.slot { scroll-margin-top: 1rem; display: grid; gap: 0.4rem; }
	.slot-cap { display: flex; align-items: baseline; gap: 0.5rem; font-family: 'Outfit', sans-serif; font-weight: 600; font-size: 0.92rem; }
	.slot-open { margin-left: auto; font-family: 'Geist Mono', 'JetBrains Mono', monospace; font-weight: 400; font-size: 0.68rem; color: var(--frame-muted); text-decoration: none; }
	.slot-open:hover { color: var(--frame-ink); }
	.slot iframe, .slot-wait {
		width: 100%; height: var(--slot-h); border: 1px solid var(--frame-line); border-radius: 14px;
		background: #fff; display: block; box-shadow: 0 18px 40px -28px rgba(0, 0, 0, 0.45);
		transition: height 160ms ease-out;
	}
	/* Before mount (and in the SSR/prerendered HTML) the slots are short: a
	   plate document that had not yet switched modes must not inflate. */
	.slot-wait { height: 240px; }
	.frame-foot { margin-top: 3rem; padding-top: 1rem; border-top: 1px solid var(--frame-line); color: var(--frame-muted); font-family: 'Geist Mono', 'JetBrains Mono', monospace; font-size: 0.72rem; }

	@media (max-width: 640px) {
		.frame-head { grid-template-columns: 1fr; }
		.rail { justify-content: flex-start; }
	}
</style>
