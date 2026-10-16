<script lang="ts">
	// One station's card in the Explore track: a short silent loop of the app
	// doing the thing (rendered with Remotion from the intro film's own screen
	// overlays, in the magican_launch_video project, src/landing/), over its
	// poster frame. Only the pinned station plays, and it starts from the top
	// each time it arrives. Nothing loads until a station is first pinned; with
	// reduced motion or Save-Data the poster stands alone.
	import { onMount, tick } from 'svelte';
	import { motionEnabled } from '$lib/motion';

	export let id: string;
	export let label: string;
	export let active = false;

	let video: HTMLVideoElement | undefined;
	let figure: HTMLElement;
	let armed = false;
	// station 0 counts as pinned before the track is reached; play only on
	// screen. Off-screen until the browser says otherwise, so the server render
	// (and the first paint) never names a video.
	let onScreen = false;

	onMount(() => {
		if (typeof IntersectionObserver === 'undefined') {
			onScreen = true;
			return;
		}
		const io = new IntersectionObserver(([entry]) => (onScreen = entry.isIntersecting), { threshold: 0.35 });
		io.observe(figure);
		return () => io.disconnect();
	});

	const saveData =
		typeof navigator !== 'undefined' &&
		Boolean((navigator as Navigator & { connection?: { saveData?: boolean } }).connection?.saveData);

	$: base = `/landing/how/${id}`;
	$: moving = $motionEnabled && !saveData;
	$: playing = active && moving && onScreen;
	$: if (playing) armed = true;
	$: sync(video, playing);

	async function sync(el: HTMLVideoElement | undefined, play: boolean): Promise<void> {
		if (!el) return;
		if (!play) {
			el.pause();
			return;
		}
		await tick(); // the src lands on the element first
		el.currentTime = 0;
		el.play().catch(() => {});
	}
</script>

<figure class="hc" role="img" aria-label={label} bind:this={figure}>
	{#if moving}
		<video
			bind:this={video}
			poster="{base}.webp"
			src={armed ? `${base}.mp4` : undefined}
			muted
			loop
			playsinline
			preload="none"
			aria-hidden="true"
		></video>
	{:else}
		<img src="{base}.webp" alt="" width="1280" height="800" loading="lazy" decoding="async" />
	{/if}
</figure>

<style>
	/* 16:10, as rendered (the web app at the back, the whole phone in front); never
	   taller than the stage leaves room for */
	.hc {
		margin: 0;
		width: 100%;
		max-width: min(100%, calc((100svh - 9rem) * 1.6));
		aspect-ratio: 8 / 5;
		border-radius: 1.4rem;
		overflow: hidden;
		background: color-mix(in srgb, var(--landing-bg, #fff) 70%, #e9e2f5);
		box-shadow: 0 30px 70px -36px rgb(40 20 60 / 0.55);
		justify-self: center;
	}

	.hc video,
	.hc img {
		display: block;
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	/* a phone: copy sits above, so the card takes what height is left */
	@media (max-width: 820px) {
		.hc {
			max-width: min(100%, 80svh);
			border-radius: 1.1rem;
		}
	}

	/* a phone on its side: the card shares the row with the copy */
	@media (max-height: 560px) {
		.hc {
			max-width: min(100%, calc((100svh - 6rem) * 1.6));
		}
	}
</style>
