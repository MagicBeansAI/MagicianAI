<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import lottie, { type AnimationItem } from 'lottie-web';
	import { CYCLE } from './lifeCycle';
	import { buildLifePortraitLottie } from './lifePortrait';

	export let index = 0;
	export let reduced = false;

	let container: HTMLDivElement;
	let anim: AnimationItem | null = null;

	function clampIndex(i: number): number {
		if (!Number.isFinite(i)) return 0;
		return Math.max(0, Math.min(CYCLE.length - 1, Math.floor(i)));
	}

	function showIndex(i: number) {
		if (!anim) return;
		// Cycle layers occupy sequential 1-frame windows; silhouette+frame span the whole comp.
		anim.goToAndStop(clampIndex(i), true);
	}

	onMount(() => {
		anim = lottie.loadAnimation({
			container,
			renderer: 'svg',
			loop: false,
			autoplay: false,
			animationData: buildLifePortraitLottie()
		});
		showIndex(reduced ? 0 : index);
	});

	onDestroy(() => {
		anim?.destroy();
		anim = null;
	});

	$: if (anim) showIndex(reduced ? 0 : index);
</script>

<div class="life-portrait" bind:this={container} aria-hidden="true"></div>

<style>
	.life-portrait {
		width: 9cm;
		max-width: 42vw;
		aspect-ratio: 3 / 4;
	}
	.life-portrait :global(svg) {
		display: block;
		width: 100%;
		height: 100%;
	}
	@media (max-width: 720px) {
		.life-portrait {
			width: 6cm;
			max-width: 70vw;
			margin-inline: auto;
		}
	}
</style>
