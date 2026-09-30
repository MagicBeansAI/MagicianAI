<!--
  AtmosphereLayer — background ambience for DashboardChrome.

  Renders nothing for `flat`. Each non-flat kind is a positioned absolute
  layer behind the chrome body, pointer-events: none, opacity driven by
  theme.atmosphere.intensity (0..1).

  Kinds:
    paper_texture — SVG turbulence noise + warm tint, low intensity
    gradient_mesh — three radial gradients composed for organic mesh
    noise         — finer SVG turbulence with bias to coolness
    scanlines     — repeating linear gradient horizontal lines
    watercolor    — soft radial color blobs

  All effects are pure CSS / inline SVG — no images, no runtime libs.
-->
<script lang="ts">
	import type { ThemeAtmosphere } from '$lib/stores/themeStore';

	export let atmosphere: ThemeAtmosphere | null | undefined = null;

	$: kind = atmosphere?.kind ?? 'flat';
	$: intensity = atmosphere?.intensity ?? 0;
</script>

{#if kind !== 'flat' && intensity > 0}
	<div class="atmosphere" style="--atm-intensity: {intensity};" aria-hidden="true">
		{#if kind === 'paper_texture'}
			<svg class="layer paper" xmlns="http://www.w3.org/2000/svg" preserveAspectRatio="none">
				<filter id="paper-noise">
					<feTurbulence type="fractalNoise" baseFrequency="0.85" numOctaves="2" stitchTiles="stitch" />
					<feColorMatrix type="matrix" values="0 0 0 0 0.55  0 0 0 0 0.50  0 0 0 0 0.42  0 0 0 1 0" />
				</filter>
				<rect width="100%" height="100%" filter="url(#paper-noise)" opacity="var(--atm-intensity)" />
			</svg>
		{:else if kind === 'gradient_mesh'}
			<div class="layer mesh"></div>
		{:else if kind === 'noise'}
			<svg class="layer noise" xmlns="http://www.w3.org/2000/svg" preserveAspectRatio="none">
				<filter id="fine-noise">
					<feTurbulence type="fractalNoise" baseFrequency="1.6" numOctaves="3" stitchTiles="stitch" />
					<feColorMatrix type="matrix" values="0 0 0 0 0.30  0 0 0 0 0.32  0 0 0 0 0.35  0 0 0 1 0" />
				</filter>
				<rect width="100%" height="100%" filter="url(#fine-noise)" opacity="var(--atm-intensity)" />
			</svg>
		{:else if kind === 'scanlines'}
			<div class="layer scanlines"></div>
		{:else if kind === 'watercolor'}
			<div class="layer watercolor"></div>
		{/if}
	</div>
{/if}

<style>
	.atmosphere {
		position: absolute;
		inset: 0;
		pointer-events: none;
		overflow: hidden;
		z-index: 0;
	}

	.layer {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
	}

	.paper {
		mix-blend-mode: multiply;
	}

	.mesh {
		background:
			radial-gradient(at 20% 18%, var(--theme-color-accent, #5B7FFF) 0%, transparent 42%),
			radial-gradient(at 78% 26%, var(--theme-color-accent-alt, var(--theme-color-accent, #FF8A65)) 0%, transparent 38%),
			radial-gradient(at 50% 80%, var(--theme-color-accent, #5B7FFF) 0%, transparent 48%);
		opacity: var(--atm-intensity);
		filter: blur(40px);
	}

	.noise {
		mix-blend-mode: overlay;
	}

	.scanlines {
		background-image: repeating-linear-gradient(
			0deg,
			transparent 0px,
			transparent 2px,
			var(--theme-color-foreground, #3EE07F) 2px,
			var(--theme-color-foreground, #3EE07F) 3px
		);
		opacity: var(--atm-intensity);
		mix-blend-mode: overlay;
	}

	.watercolor {
		background:
			radial-gradient(circle at 15% 25%, var(--theme-color-accent, #D17A47) 0%, transparent 35%),
			radial-gradient(circle at 80% 30%, var(--theme-color-accent-alt, var(--theme-color-accent, #6A8B69)) 0%, transparent 28%),
			radial-gradient(circle at 65% 75%, var(--theme-color-accent, #D17A47) 0%, transparent 32%),
			radial-gradient(circle at 30% 85%, var(--theme-color-accent-alt, var(--theme-color-accent, #6A8B69)) 0%, transparent 30%);
		opacity: var(--atm-intensity);
		filter: blur(60px);
	}

	@media (prefers-reduced-motion: reduce) {
		/* atmospheres don't animate; nothing to disable */
	}
</style>
