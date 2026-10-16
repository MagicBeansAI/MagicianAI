<script lang="ts">
	// The Magican star, the brand board's 3D mascot, as the app's mark (top bar, login). Images come from
	// static/brand (built from the launch-video repo's assets/brand/icons). On a dark theme it shows the
	// glowing star with a gold ball and dashes, as the brand board does. Dark is read from the text colour
	// around it (light text = a dark theme), so every theme is covered without a list of theme ids, and it
	// is checked again whenever the theme changes.
	import { onMount } from 'svelte';
	import { themeStore } from '$lib/shared/stores/themeStore';

	export let size = 30;

	let el: HTMLSpanElement;
	let dark = false;

	function check(): void {
		if (!el) return;
		const rgb = getComputedStyle(el).color.match(/[\d.]+/g)?.slice(0, 3).map(Number);
		if (!rgb) return;
		dark = (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]) / 255 > 0.55;
	}

	onMount(() => themeStore.subscribe(() => requestAnimationFrame(check)));
</script>

<span class="brand-star" bind:this={el} style="width: {size}px; height: {size}px;" aria-hidden="true">
	{#if dark}
		<!-- the dark star's image is 1.24x the star, to hold its glow -->
		<img src="/brand/star-dark.png" alt="" style="width: {size * 1.24}px; height: {size * 1.24}px; margin: {-size * 0.12}px;" />
	{:else}
		<img src="/brand/star-light.png" alt="" style="width: {size}px; height: {size}px;" />
	{/if}
</span>

<style>
	.brand-star {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		overflow: visible;
	}

	.brand-star img {
		display: block;
		max-width: none;
		pointer-events: none;
		user-select: none;
	}
</style>
