<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { scale } from 'svelte/transition';
	import { cubicIn, cubicOut } from 'svelte/easing';
	import { motionEnabled } from '$lib/motion';
	import { colorAt, CYCLE, holdMsFor, labelAt, nextIndex } from './lifeCycle';
	import { heroPhase } from './landingPhase';

	$: reduced = !$motionEnabled;

	let index = 0;
	$: verb = CYCLE[index] ?? labelAt(index);
	$: verbColor = colorAt(index);
	let sectionEl: HTMLElement | null = null;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let io: IntersectionObserver | null = null;

	function stop(): void {
		if (timer !== null) {
			clearTimeout(timer);
			timer = null;
		}
	}

	function arm(): void {
		stop();
		if (reduced) return;
		timer = setTimeout(() => {
			index = nextIndex(index);
			arm();
		}, holdMsFor(index));
	}

	function onVisibility(): void {
		if (document.hidden) {
			stop();
		} else if (!reduced) {
			arm();
		}
	}

	$: if (reduced) stop();

	onMount(() => {
		heroPhase.set(0);
		if (!reduced && !document.hidden) arm();
		document.addEventListener('visibilitychange', onVisibility);
		if (typeof IntersectionObserver !== 'undefined' && sectionEl) {
			io = new IntersectionObserver((entries) => {
				heroPhase.set(entries[0]?.isIntersecting ? 0 : 1);
			});
			io.observe(sectionEl);
		}
	});

	onDestroy(() => {
		stop();
		if (typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', onVisibility);
		}
		io?.disconnect();
		io = null;
	});
</script>

<section class="hs" bind:this={sectionEl} aria-labelledby="hs-title">
	<div class="hs-copy">
		<h1 id="hs-title" class="hs-title" class:static={reduced}>magican</h1>
		<p class="hs-tag">Superpowers for Work, Play and</p>
		<div class="hs-verb-slot">
			{#if reduced}
				<p class="hs-verb" style="color: {verbColor}">{verb}</p>
			{:else}
				{#key index}
					<p
						class="hs-verb"
						style="color: {verbColor}"
						in:scale={{ duration: 460, start: 1.4, opacity: 0, easing: cubicOut }}
						out:scale={{ duration: 240, start: 1, opacity: 0, easing: cubicIn }}
					>
						{verb}
					</p>
				{/key}
			{/if}
		</div>
	</div>
</section>

<style>
	.hs {
		position: relative;
		min-height: 100svh;
		box-sizing: border-box;
		display: grid;
		place-items: center;
		padding: clamp(1.5rem, 6vw, 4rem);
		font-family: var(--lp-font);
		color: var(--text-primary, inherit);
	}

	.hs-copy {
		width: min(100%, 46rem);
		text-align: center;
	}

	.hs-title {
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(3.6rem, 10vw, 6.4rem);
		font-weight: 700;
		letter-spacing: -0.04em;
		/* background-clip: text only fills the line box. Magican's g sits
		   below Outfit's em-square; 1.15 still sheared the tail.
		   Extra leading plus padding-bottom keeps the clip region around
		   the full glyph. */
		line-height: 1.28;
		padding-bottom: 0.16em;
		overflow: visible;
		/* Near-black type with a softer sunset sweep glint (Brand Coral #FF6B6B to Sunset Peach #FFA07A).
		   Clip to the letters so the wash sits in the word, not behind it. */
		background-image: linear-gradient(
			105deg,
			#1a1612 0%,
			#1a1612 34%,
			#FF6B6B 48%,
			#FFA07A 54%,
			#1a1612 66%,
			#1a1612 100%
		);
		background-size: 220% 100%;
		background-position: 100% 50%;
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		-webkit-text-fill-color: transparent;
		animation: hs-shimmer 11s ease-in-out infinite;
	}

	.hs-title.static {
		animation: none;
		background-image: none;
		color: #1a1612;
		-webkit-text-fill-color: #1a1612;
	}

	@keyframes hs-shimmer {
		0%,
		100% {
			background-position: 100% 50%;
		}
		50% {
			background-position: 0% 50%;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.hs-title {
			animation: none;
			background-image: none;
			color: #1a1612;
			-webkit-text-fill-color: #1a1612;
		}
	}

	.hs-tag {
		/* Sit close under Magican. The title's padding-bottom is the g's
		   clip room — don't pull this up into that. */
		margin: 0.12em 0 0.35em;
		font-family: var(--lp-font);
		font-size: clamp(1.15rem, 2.4vw, 1.55rem);
		font-weight: 500;
		line-height: 1.35;
		color: inherit;
	}

	.hs-verb-slot {
		display: grid;
		grid-template: 1fr / 1fr;
		justify-items: center;
		align-items: start;
	}

	.hs-verb {
		grid-area: 1 / 1;
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(2rem, 5vw, 3.2rem);
		font-weight: 500;
		letter-spacing: -0.02em;
		line-height: 1.15;
	}

</style>
