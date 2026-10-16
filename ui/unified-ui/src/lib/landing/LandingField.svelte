<script lang="ts">
	// The town backdrop belongs to the proof sequence below the brand hero.
	// Its sticky viewport stays behind the slotted track; the negative margin
	// cancels that viewport's flow height so the first claim starts immediately.
	import { onDestroy, onMount } from 'svelte';
	import { motionEnabled } from '$lib/motion';
	import { overBackdrop } from './landingPhase';

	$: reduced = !$motionEnabled;

	let fieldEl: HTMLElement | null = null;
	let io: IntersectionObserver | null = null;
	let backdropPlaying = false;

	onMount(() => {
		overBackdrop.set(true);
		if (typeof IntersectionObserver === 'undefined' || !fieldEl) return;
		io = new IntersectionObserver((entries) => {
			overBackdrop.set(entries[0]?.isIntersecting ?? false);
		});
		io.observe(fieldEl);
	});

	onDestroy(() => {
		io?.disconnect();
		io = null;
	});
</script>

<div class="lfield" bind:this={fieldEl}>
	<div class="lfield-backdrop-wrap" aria-hidden="true">
		<!-- The still is the visible default even when motion is allowed. iOS
		     WebKit can delay or refuse muted autoplay (including in Chrome) and
		     otherwise paints a native start-playback button over the poster. The
		     video only replaces this image after it emits `playing`. -->
		<img
			class="lfield-backdrop lfield-backdrop-still"
			class:lfield-backdrop-still--hidden={!reduced && backdropPlaying}
			src="/landing/dusk-town.webp"
			alt=""
		/>
		{#if !reduced}
			<video
				class="lfield-backdrop lfield-backdrop-video"
				class:lfield-backdrop-video--playing={backdropPlaying}
				src="/landing/dusk-town-loop.mp4"
				poster="/landing/dusk-town.webp"
				preload="auto"
				autoplay
				muted
				loop
				playsinline
				controlslist="nodownload nofullscreen noremoteplayback"
				disablepictureinpicture
				tabindex="-1"
				on:playing={() => (backdropPlaying = true)}
				on:pause={() => (backdropPlaying = false)}
				on:error={() => (backdropPlaying = false)}
			></video>
		{/if}
	</div>

	<slot />
</div>

<style>
	.lfield {
		position: relative;
		/* The ground the vignette dissolves into. It has to be here rather
		   than on the tracks inside, or each track would paint its own cream
		   over the shared picture. */
		background: var(--landing-bg);
	}

	.lfield-backdrop-wrap {
		position: sticky;
		top: 0;
		height: 100svh;
		/* Clips the picture when it comes out taller than the viewport — see
		   `.lfield-backdrop`'s aspect-ratio note. */
		overflow: hidden;
		z-index: 0;
		/* See the header: cancels the slot a sticky first child would
		   otherwise occupy, so nothing below is displaced by a screen. */
		margin-bottom: -100svh;
		pointer-events: none;
		background-image: var(--landing-blob-2), var(--landing-blob-1), var(--landing-blob-3);
	}

	.lfield-backdrop {
		/* Landscape and square-ish viewports: width fills, height follows the
		   asset's 16:9. Wider-than-16:9 (ultrawide) comes out taller than the
		   wrap and clips symmetrically. The mask then sits on the media's
		   own box. */
		position: absolute;
		top: 50%;
		left: 0;
		width: 100%;
		aspect-ratio: 16 / 9;
		filter: var(--landing-backdrop-filter, grayscale(100%) brightness(3) contrast(0.7));
		opacity: var(--landing-backdrop-opacity, 0.35);
		transform: translateY(-50%);
		/* One ellipse, dissolving the picture into the field's cream at the
		   edges. An ellipse inscribed in a rectangle never reaches the
		   corners, so corners fade slightly more than the cardinal edges. */
		-webkit-mask-image: radial-gradient(
			ellipse 92% 94% at 50% 50%,
			black 0%,
			black 78%,
			transparent 100%
		);
		mask-image: radial-gradient(ellipse 92% 94% at 50% 50%, black 0%, black 78%, transparent 100%);
	}

	.lfield-backdrop-still,
	.lfield-backdrop-video {
		transition: opacity 120ms linear;
	}

	.lfield-backdrop-video,
	.lfield-backdrop-still--hidden {
		opacity: 0;
	}

	.lfield-backdrop-video--playing {
		opacity: var(--landing-backdrop-opacity, 0.35);
	}

	/* iPhone Chrome uses WebKit and may expose its start-playback overlay even
	   without a `controls` attribute. The video is decorative and never has an
	   interactive state, so suppress every native media-control surface too. */
	video.lfield-backdrop::-webkit-media-controls,
	video.lfield-backdrop::-webkit-media-controls-panel,
	video.lfield-backdrop::-webkit-media-controls-play-button,
	video.lfield-backdrop::-webkit-media-controls-start-playback-button {
		display: none !important;
		-webkit-appearance: none;
		opacity: 0;
		pointer-events: none;
	}

	@media (max-aspect-ratio: 16 / 9) {
		/* Portrait phones (and any viewport taller than 16:9): a width-locked
		   16:9 plate is a thin band with cream above and below. Cover the
		   sticky stage instead, cropping the landscape equally on both sides
		   so the dusk-town centre (sunset, water, skyline) fills the screen.
		   The mask now tracks the viewport box, which is the fade we want. */
		.lfield-backdrop {
			inset: 0;
			width: 100%;
			height: 100%;
			aspect-ratio: auto;
			object-fit: cover;
			object-position: center;
			transform: none;
		}
	}
</style>
