<script lang="ts">
	import { motionEnabled } from '$lib/motion';
	import { boundsFromWeights, createScrubber } from './scrub';
	import HowShot from './HowShot.svelte';
	import { HOW_CLAIMS, HOW_TRACK_VH, HOW_WEIGHTS, claimMotion } from './howTrack';

	$: reduced = !$motionEnabled;

	const bounds = boundsFromWeights(HOW_WEIGHTS);
	const scrub = createScrubber(bounds);
	const scrubState = scrub.state;
	$: scene = $scrubState.scene;
	$: local = $scrubState.local;
</script>

<div
	class="ht-track"
	class:static={reduced}
	style={reduced ? '' : `height:${HOW_TRACK_VH}svh`}
	use:scrub.track
	data-scene={scene}
	data-local={local.toFixed(3)}
>
	<div class="ht-stage" class:static={reduced}>
		{#each HOW_CLAIMS as claim, i (claim.id)}
			{@const m = reduced ? { opacity: 1, mediaX: 0, copyX: 0 } : claimMotion(i, scene, local)}
			<article
				class="ht-claim"
				style="opacity:{m.opacity}"
				aria-hidden={!reduced && m.opacity < 0.04}
			>
				<div class="ht-media" style="transform: translate3d(calc({m.mediaX} * var(--ht-slide)), 0, 0)">
					<HowShot kind={claim.id} active={!reduced && i === scene} />
				</div>
				<div class="ht-copy" style="transform: translate3d(calc({m.copyX} * var(--ht-slide)), 0, 0)">
					<p class="ht-kicker">{claim.kicker}</p>
					<h2>{claim.title}</h2>
					<p class="ht-line">{claim.line}</p>
				</div>
			</article>
		{/each}
	</div>
</div>

<style>
	.ht-track {
		position: relative;
		z-index: 1;
	}

	.ht-track.static {
		height: auto;
	}

	.ht-stage {
		position: sticky;
		top: 0;
		z-index: 2;
		height: 100svh;
		display: grid;
		place-items: center;
		padding: clamp(1.2rem, 5vw, 3.5rem);
		box-sizing: border-box;
		font-family: var(--lp-font);
		color: var(--text-primary, inherit);
		overflow: hidden;
	}

	.ht-stage.static {
		position: relative;
		height: auto;
		display: flex;
		flex-direction: column;
		gap: clamp(2.4rem, 7vh, 4rem);
		padding-top: clamp(2rem, 6vh, 3.5rem);
		padding-bottom: clamp(2.5rem, 8vh, 4.5rem);
		overflow: visible;
	}

	.ht-claim {
		--ht-slide: 46%;
		grid-area: 1 / 1;
		width: min(100%, 58rem);
		display: grid;
		grid-template-columns: minmax(0, 1.12fr) minmax(0, 0.88fr);
		grid-template-areas: 'media copy';
		gap: clamp(0.5rem, 1.6vw, 1.15rem);
		align-items: center;
		pointer-events: none;
	}

	.ht-stage.static .ht-claim {
		grid-area: auto;
		opacity: 1 !important;
	}

	.ht-stage.static .ht-media,
	.ht-stage.static .ht-copy {
		transform: none !important;
	}

	.ht-media,
	.ht-copy {
		will-change: transform;
		min-width: 0;
	}

	.ht-media {
		grid-area: media;
	}

	.ht-copy {
		grid-area: copy;
		text-align: left;
		min-width: 0;
	}

	.ht-kicker {
		margin: 0 0 0.65rem;
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.72rem;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		opacity: 0.55;
	}

	.ht-claim h2 {
		margin: 0;
		font-size: clamp(1.7rem, 4.2vw, 2.9rem);
		font-weight: 500;
		letter-spacing: -0.035em;
		line-height: 1.15;
		text-wrap: balance;
	}

	.ht-line {
		margin: 0.8rem 0 0;
		font-size: clamp(1rem, 1.8vw, 1.18rem);
		font-weight: 400;
		line-height: 1.45;
		text-wrap: pretty;
		opacity: 0.86;
	}

	@media (max-width: 820px) {
		.ht-stage {
			align-items: center;
			justify-items: stretch;
			padding: 3.25rem max(1rem, env(safe-area-inset-right, 0px))
				max(1.15rem, env(safe-area-inset-bottom, 0px)) max(1rem, env(safe-area-inset-left, 0px));
		}

		.ht-claim {
			--ht-slide: 10%;
			width: 100%;
			grid-template-columns: minmax(0, 1fr);
			grid-template-areas:
				'copy'
				'media';
			gap: 0.75rem;
			justify-items: stretch;
			text-align: center;
		}

		.ht-copy {
			text-align: center;
		}

		.ht-kicker {
			margin-bottom: 0.4rem;
		}

		.ht-claim h2 {
			font-size: clamp(1.35rem, 6.4vw, 1.9rem);
		}

		.ht-line {
			margin-top: 0.45rem;
			font-size: 0.95rem;
		}
	}

	@media (max-height: 560px) {
		.ht-stage {
			padding: 2.4rem 0.85rem 0.7rem;
		}

		.ht-claim {
			--ht-slide: 10%;
			width: 100%;
			height: 100%;
			max-height: 100%;
			grid-template-columns: minmax(0, 1.15fr) minmax(0, 0.95fr);
			grid-template-areas: 'media copy';
			grid-template-rows: minmax(0, 1fr);
			gap: 0.55rem;
			align-items: center;
		}

		.ht-media {
			min-height: 0;
			height: 100%;
			display: flex;
			align-items: center;
		}

		.ht-copy {
			text-align: left;
		}

		.ht-claim h2 {
			font-size: clamp(1.15rem, 3.6vh, 1.45rem);
		}

		.ht-line {
			font-size: 0.85rem;
		}
	}
</style>
