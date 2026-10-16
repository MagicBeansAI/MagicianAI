<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import type { AnimationItem } from 'lottie-web';
	import { motionEnabled } from '$lib/motion';

	export let mood: 'hi' | 'thinking' | 'working' | 'on-it' | 'side-quest' = 'hi';
	export let dark = false;

	type Slot = 0 | 1;
	const CROSSFADE_MS = 360;
	let containers: HTMLDivElement[] = [];
	let figure: HTMLDivElement;
	let mounted = false;
	let visible = true;
	let active: Slot | null = null;
	let outgoing: Slot | null = null;
	const players: (AnimationItem | null)[] = [null, null];
	let pending: { slot: Slot; player: AnimationItem } | null = null;
	let observer: IntersectionObserver | null = null;
	let request: AbortController | null = null;
	let generation = 0;
	let fading: Promise<void> | null = null;
	let finishFade: () => void = () => {};

	function destroySlot(slot: Slot): void {
		players[slot]?.destroy();
		players[slot] = null;
	}

	function cancelPending(): void {
		if (pending && players[pending.slot] === pending.player) destroySlot(pending.slot);
		pending = null;
	}

	function syncPlayback(): void {
		players.forEach((player, slot) => {
			if (!player) return;
			if ((slot === active || slot === outgoing) && visible && !document.hidden && $motionEnabled) player.play();
			else player.pause();
		});
	}

	function crossfade(previous: Slot | null): void {
		outgoing = previous;
		fading = new Promise((resolve) => {
			const timer = setTimeout(() => finishFade(), CROSSFADE_MS);
			finishFade = () => {
				clearTimeout(timer);
				if (previous !== null) destroySlot(previous);
				outgoing = null;
				fading = null;
				finishFade = () => {};
				resolve();
			};
		});
	}

	async function show(nextMood: typeof mood, nextDark: boolean, reduced: boolean): Promise<void> {
		const version = ++generation;
		request?.abort();
		cancelPending();
		if (reduced) {
			finishFade();
			destroySlot(0);
			destroySlot(1);
			active = null;
			return;
		}

		const controller = new AbortController();
		request = controller;
		try {
			const [module, response] = await Promise.all([
				import('lottie-web'),
				fetch(`/brand/animations/${nextMood}${nextDark ? '-dark' : ''}.json`, {
					signal: controller.signal
				})
			]);
			if (!response.ok) return;
			const animationData = await response.json();
			// Keep both sides of an ongoing fade intact. Only the latest request
			// may reuse the free slot once that handover finishes.
			await fading;
			if (version !== generation || controller.signal.aborted) return;
			const slot: Slot = active === 0 ? 1 : 0;
			const player = module.default.loadAnimation({
				container: containers[slot],
				renderer: 'svg',
				loop: true,
				autoplay: false,
				animationData
			});
			players[slot] = player;
			pending = { slot, player };
			player.addEventListener('DOMLoaded', () => {
				if (version !== generation || pending?.player !== player) return;
				pending = null;
				const previous = active;
				active = slot;
				crossfade(previous);
				syncPlayback();
			});
		} catch (error) {
			// Retain the current animation on a failed switch; the static pose
			// covers initial loading/failure and reduced motion.
			if (!controller.signal.aborted) console.warn('Magican mascot animation unavailable', error);
		}
	}

	onMount(() => {
		mounted = true;
		document.addEventListener('visibilitychange', syncPlayback);
		if (typeof IntersectionObserver !== 'undefined') {
			observer = new IntersectionObserver(([entry]) => {
				visible = entry?.isIntersecting ?? false;
				syncPlayback();
			});
			observer.observe(figure);
		}
	});

	onDestroy(() => {
		mounted = false;
		generation++;
		request?.abort();
		observer?.disconnect();
		cancelPending();
		finishFade();
		destroySlot(0);
		destroySlot(1);
		if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', syncPlayback);
	});

	$: if (mounted) void show(mood, dark, !$motionEnabled);
</script>

<div
	class="brand-mascot"
	bind:this={figure}
	aria-hidden="true"
	class:ready={active !== null}
	class:reduced={!$motionEnabled}
	style={`--mascot-fade-duration: ${CROSSFADE_MS}ms`}
>
	<img
		class="mascot-still"
		src={`/brand/mascot-hi-${dark ? 'dark' : 'light'}.svg`}
		alt=""
		width="512"
		height="512"
		fetchpriority="high"
	/>
	<div class="mascot-motion" class:active={active === 0} bind:this={containers[0]}></div>
	<div class="mascot-motion" class:active={active === 1} bind:this={containers[1]}></div>
</div>

<style>
	.brand-mascot {
		position: relative;
		width: 100%;
		aspect-ratio: 1;
		filter: var(--brand-mascot-filter, none);
	}

	.mascot-still, .mascot-motion {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		transition: opacity var(--mascot-fade-duration) ease-in-out;
	}

	.mascot-motion, .ready .mascot-still { opacity: 0; }
	.mascot-motion.active { opacity: 1; }
	.reduced .mascot-still, .reduced .mascot-motion { transition: none; }

	.mascot-motion :global(svg) {
		display: block;
		width: 100%;
		height: 100%;
	}
</style>
