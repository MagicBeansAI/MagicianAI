<script lang="ts">
	import { onMount } from 'svelte';
	import { motionEnabled } from '$lib/motion';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import BrandMascot from './BrandMascot.svelte';
	import HeroBackground from './HeroBackground.svelte';
	import { FILM_HASH } from './film';
	import {
		HERO_EXAMPLE_CATEGORIES,
		HERO_EXAMPLE_INTERVAL_MS,
		HERO_EXAMPLE_SEQUENCE
	} from './heroExamples';

	export let actionLabel = 'Request setup';
	export let actionHref = '#landing-cta';
	export let dark = false;

	let position = 0;
	let greeted = false;
	let paused = false;
	let hovered = false;
	let focused = false;
	let mounted = false;
	let visible = true;
	let pageVisible = true;
	let announcement = '';
	let exampleEl: HTMLDivElement;
	let timer: ReturnType<typeof setTimeout> | null = null;

	$: selected = HERO_EXAMPLE_SEQUENCE[position].categoryIndex;
	$: exampleIndex = HERO_EXAMPLE_SEQUENCE[position].exampleIndex;
	$: category = HERO_EXAMPLE_CATEGORIES[selected];
	$: example = category.examples[exampleIndex];
	$: mood = greeted ? category.mood : 'hi' as const;
	$: cycling = mounted && visible && pageVisible && !paused && !hovered && !focused && $motionEnabled;
	$: if (cycling) arm(); else stop();

	function stop(): void {
		if (timer !== null) clearTimeout(timer);
		timer = null;
	}

	function arm(): void {
		stop();
		if (!cycling) return;
		timer = setTimeout(() => {
			advance(1);
			arm();
		}, HERO_EXAMPLE_INTERVAL_MS);
	}

	function announce(): void {
		const entry = HERO_EXAMPLE_SEQUENCE[position];
		const nextCategory = HERO_EXAMPLE_CATEGORIES[entry.categoryIndex];
		const nextExample = nextCategory.examples[entry.exampleIndex];
		announcement = `${nextCategory.label} example. ${nextExample.ask} ${nextExample.result}`;
	}

	function advance(direction: number, manual = false): void {
		const total = HERO_EXAMPLE_SEQUENCE.length;
		position = (position + direction + total) % total;
		greeted = true;
		if (manual) announce();
	}

	function step(direction: number): void {
		advance(direction, true);
		arm();
	}

	function select(index: number): void {
		position = HERO_EXAMPLE_SEQUENCE.findIndex((entry) => entry.categoryIndex === index);
		greeted = true;
		announce();
		arm();
	}

	onMount(() => {
		pageVisible = !document.hidden;
		const onVisibility = () => (pageVisible = !document.hidden);
		document.addEventListener('visibilitychange', onVisibility);
		let observer: IntersectionObserver | null = null;
		if (typeof IntersectionObserver !== 'undefined') {
			visible = false;
			observer = new IntersectionObserver(([entry]) => {
				visible = !!entry?.isIntersecting && entry.intersectionRatio >= 0.15;
			}, { threshold: 0.15 });
			observer.observe(exampleEl);
		}
		mounted = true;
		return () => {
			mounted = false;
			stop();
			observer?.disconnect();
			document.removeEventListener('visibilitychange', onVisibility);
		};
	});
</script>

<section class="hs" id="landing-hero" tabindex="-1" aria-labelledby="hs-title">
	<HeroBackground {dark} />
	<div class="hs-main">
		<div class="hs-copy">
			<h1 id="hs-title">magican</h1>
			<p class="hs-tagline">More You</p>
			<p class="hs-description">Your personal superpowers</p>
			<div class="hs-actions">
				<nav class="hs-start" aria-label="Get started with Magican">
					<a class="hs-primary" href={actionHref}>{actionLabel} <span aria-hidden="true">→</span></a>
				</nav>
				<!-- outside the get-started nav, which keeps exactly one access action -->
				<a id="film" class="hs-film" href={FILM_HASH}>
					<!-- a few silent seconds of the film itself (0:46–0:50, played there and back) -->
					<span class="hs-film-thumb" aria-hidden="true">
						<!-- with reduced motion it stays on its poster frame -->
						<video src="/landing/film-loop.mp4" poster="/landing/film-poster.jpg" autoplay={$motionEnabled} muted loop playsinline preload={$motionEnabled ? 'auto' : 'none'}></video>
						<span class="hs-film-play">▶</span>
					</span>
					<span class="hs-film-label"><span class="hs-film-desktop">Watch the film</span><span class="hs-film-mobile">Film</span> <span class="hs-film-len">8 min</span></span>
				</a>
			</div>
		</div>
		<div class="hs-sidekick">
			<BrandMascot {mood} {dark} />
		</div>
	</div>
	<div
		class="hs-example"
		role="region"
		aria-label="Example ideas"
		aria-roledescription="carousel"
		bind:this={exampleEl}
		on:pointerenter={(event) => (hovered = event.pointerType === 'mouse')}
		on:pointerleave={() => (hovered = false)}
		on:focusin={(event) => (focused = event.target instanceof Element && event.target.matches(':focus-visible'))}
		on:focusout={(event) => (focused = event.relatedTarget instanceof Element && exampleEl.contains(event.relatedTarget) && event.relatedTarget.matches(':focus-visible'))}
	>
		<div class="hs-choices" role="group" aria-label="Explore an example">
			{#each HERO_EXAMPLE_CATEGORIES as item, index}
				<button type="button" aria-pressed={selected === index} on:click={() => select(index)}>{item.label}</button>
			{/each}
		</div>
		<div class="hs-example-copy">
			<div class="hs-example-top">
				<p class="hs-example-label">
					Try asking
					<span class="hs-example-count">{category.label} · {exampleIndex + 1}/{category.examples.length}</span>
				</p>
				<div class="hs-example-controls" role="group" aria-label="Example playback">
					<button type="button" aria-label="Previous example" on:click={() => step(-1)}><Icon name="chevron-left" size={16} /></button>
					{#if $motionEnabled}
						<button type="button" class="hs-example-pause" aria-label={paused ? 'Resume examples' : 'Pause examples'} on:click={() => (paused = !paused)}>{paused ? 'Resume' : 'Pause'}</button>
					{/if}
					<button type="button" aria-label="Next example" on:click={() => step(1)}><Icon name="chevron-right" size={16} /></button>
				</div>
			</div>
			{#key `${selected}:${exampleIndex}`}
				<div class="hs-example-text" class:animate={$motionEnabled}>
					<p class="hs-ask">“{example.ask}”</p>
					<p class="hs-result">{example.result}</p>
				</div>
			{/key}
		</div>
		<!-- Announce deliberate navigation, never every automatic rotation. -->
		<p class="hs-announcement" role="status" aria-live="polite" aria-atomic="true">{announcement}</p>
	</div>
	<a class="hs-bridge" href="#landing-proof">Explore Magican <span aria-hidden="true">↓</span></a>
</section>

<style>
	.hs {
		position: relative;
		display: grid;
		grid-template-rows: minmax(0, 1fr) auto auto;
		height: calc(100svh - var(--landing-nav-height));
		scroll-margin-top: var(--landing-nav-height);
		isolation: isolate;
		overflow: hidden;
		font-family: var(--lp-font);
		color: var(--text-primary);
		background: var(--landing-bg);
	}

	.hs-main {
		width: min(100%, 74rem);
		margin: 0 auto;
		min-height: 0;
		display: grid;
		grid-template-columns: minmax(0, 1.08fr) minmax(0, 1fr);
		grid-template-rows: minmax(0, 1fr);
		align-items: center;
		gap: 2rem;
		padding: clamp(1rem, 3svh, 2.5rem) clamp(1.5rem, 5vw, 4.5rem);
	}

	.hs h1 {
		margin: 0;
		padding-bottom: .04em;
		font-family: var(--lp-font);
		font-size: clamp(3.8rem, min(8vw, 11svh), 6.8rem);
		font-weight: 700;
		letter-spacing: -.055em;
		line-height: 1.15;
		color: var(--text-primary);
	}

	.hs-tagline {
		margin: 0 0 clamp(.65rem, 2svh, 1.6rem);
		font-family: var(--lp-font);
		font-size: clamp(2.8rem, min(6vw, 8.4svh), 5rem);
		font-weight: 500;
		font-style: normal;
		letter-spacing: -.045em;
		line-height: 1.12;
	}

	.hs-description {
		max-width: 23rem;
		margin: 0 0 clamp(.75rem, 2svh, 1.6rem);
		font-size: clamp(1.1rem, 1.65vw, 1.3rem);
		line-height: 1.5;
		color: var(--text-secondary);
		text-wrap: pretty;
	}

	.hs-actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: .7rem;
	}

	.hs-actions a {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 1.1rem;
		min-height: 3rem;
		padding: .7rem 1.25rem;
		border: 1px solid var(--border-default);
		border-radius: 999px;
		font-size: .9rem;
		font-weight: 550;
		text-decoration: none;
		transition: transform 180ms ease, background 180ms ease;
	}

	.hs-actions a:hover {
		transform: translateY(-2px);
	}

	.hs-primary {
		background: var(--button-primary-bg);
		color: var(--text-on-accent);
		border-color: var(--button-primary-bg) !important;
	}

	.hs-start {
		display: flex;
	}

	/* the film opens full screen (FilmDialog listens for #film) */
	.hs-actions .hs-film {
		gap: .65rem;
		color: var(--text-primary);
		background: color-mix(in srgb, var(--landing-bg, #fff) 70%, transparent);
	}

	.hs-actions a.hs-film {
		padding-left: .4rem;
	}

	.hs-film-thumb {
		position: relative;
		flex: none;
		width: 4.4rem;
		aspect-ratio: 16 / 9;
		border-radius: 999px 0.55rem 0.55rem 999px;
		overflow: hidden;
		background: #1d1410;
	}

	.hs-film-thumb video {
		display: block;
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	.hs-film-play {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		padding-left: 0.12rem;
		color: #fff;
		font-size: .62rem;
		text-shadow: 0 1px 4px rgb(0 0 0 / 0.6);
		background: radial-gradient(circle, rgb(0 0 0 / 0.35) 0 0.75rem, transparent 0.8rem);
	}

	.hs-film-mobile {
		display: none;
	}

	.hs-film-label {
		white-space: nowrap;
	}

	.hs-film-len {
		color: var(--text-secondary);
		font-weight: 450;
	}

	.hs-sidekick {
		position: relative;
		margin: 0;
		width: 100%;
		max-width: min(31rem, 52svh);
		justify-self: center;
	}

	.hs-sidekick:before {
		content: '';
		position: absolute;
		inset: 5% -8%;
		z-index: -1;
		background: radial-gradient(ellipse, var(--brand-mascot-wash), transparent 65%);
	}

	.hs-example {
		position: relative;
		width: min(calc(100% - 3rem), 65rem);
		margin: 0 auto;
		display: grid;
		grid-template-columns: minmax(0, .9fr) minmax(0, 1.5fr);
		align-items: center;
		gap: 2rem;
		padding: clamp(.75rem, 2svh, 1.4rem) 0;
		border-top: 1px solid var(--border-soft);
	}

	.hs-choices {
		display: flex;
		flex-wrap: wrap;
		gap: .45rem;
	}

	.hs-choices button {
		min-height: 2.75rem;
		padding: .55rem 1rem;
		border-radius: 999px;
		border: 1px solid var(--border-default);
		background: var(--bg-card);
		color: var(--text-primary);
		font-family: inherit;
		font-size: .82rem;
		font-weight: 500;
		cursor: pointer;
	}

	.hs-choices button[aria-pressed='true'] {
		background: var(--brand-gold);
		border-color: var(--brand-gold);
		color: var(--brand-gold-ink);
	}

	.hs-example-label {
		margin: 0;
		font-size: .64rem;
		font-weight: 550;
		letter-spacing: .12em;
		text-transform: uppercase;
		color: var(--text-secondary);
	}

	.hs-example-top {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: .5rem;
		margin-bottom: .65rem;
	}

	.hs-example-count {
		display: block;
		margin-top: .35rem;
		font-size: .7rem;
		font-weight: 400;
		letter-spacing: 0;
		text-transform: none;
	}

	.hs-example-controls {
		display: flex;
		flex-shrink: 0;
		gap: .2rem;
	}

	.hs-example-controls button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 2.75rem;
		min-height: 2.75rem;
		padding: .45rem .6rem;
		border: 1px solid transparent;
		border-radius: 999px;
		background: transparent;
		color: var(--text-secondary);
		font-family: inherit;
		font-size: .75rem;
		cursor: pointer;
	}

	.hs-example-controls button:hover {
		background: var(--bg-card);
		border-color: var(--border-soft);
		color: var(--text-primary);
	}

	.hs-example-controls .hs-example-pause {
		min-width: 4rem;
	}

	.hs-example-text {
		min-height: 4.5rem;
	}

	.hs-example-text.animate {
		animation: hs-example-arrive 220ms ease-out;
	}

	@keyframes hs-example-arrive {
		from { opacity: 0; }
		to { opacity: 1; }
	}

	.hs-announcement {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}

	.hs-ask {
		margin: 0 0 .3rem;
		font-size: .97rem;
		line-height: 1.4;
		font-weight: 450;
	}

	.hs-result {
		margin: 0;
		font-size: .8rem;
		line-height: 1.4;
		color: var(--text-secondary);
	}

	.hs-bridge {
		display: flex;
		width: 100%;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		border-top: 1px solid var(--border-soft);
		margin: 0 auto;
		min-height: 2.75rem;
		padding: clamp(.7rem, 1.5svh, 1.35rem) max(1.5rem, calc((100% - 65rem) / 2));
		color: var(--text-primary);
		text-decoration: none;
		font-size: clamp(1.05rem, 1.6vw, 1.3rem);
		font-weight: 500;
		letter-spacing: -.02em;
	}

	@media (min-width: 768px) and (max-height: 740px) {
		.hs-main {
			padding-block: .75rem;
		}
		.hs h1 {
			font-size: clamp(3.4rem, 9.5svh, 5rem);
			line-height: 1.05;
		}
		.hs-tagline {
			font-size: clamp(2.5rem, 7.2svh, 3.8rem);
			line-height: 1.08;
			margin-bottom: .6rem;
		}
		.hs-description {
			font-size: 1rem;
			line-height: 1.4;
			margin-bottom: .75rem;
		}
		.hs-actions {
			gap: .5rem;
		}
		.hs-actions a {
			min-height: 2.75rem;
			gap: .75rem;
			padding: .6rem 1rem;
			font-size: .84rem;
		}
		.hs-sidekick {
			max-width: min(27rem, 44svh);
		}
		.hs-example-text {
			min-height: 5.5rem;
		}
	}

	@media (max-width: 767px), (max-height: 540px) {
		.hs {
			height: auto;
			min-height: calc(100svh - var(--landing-nav-height));
			grid-template-rows: auto auto auto;
		}
	}

	@media (max-width: 767px) {
		.hs-film-desktop { display: none; }
		.hs-film-mobile { display: inline; }
		.hs-film-thumb { width: 2.25rem; }
		.hs-start { flex-shrink: 0; }
		.hs-primary { white-space: nowrap; }
		.hs-main {
			grid-template-columns: minmax(0, 1fr);
			grid-template-rows: auto auto;
			min-height: 0;
			gap: .25rem;
			padding: 1rem 1.5rem .5rem;
		}
		.hs h1 {
			font-size: clamp(2.9rem, 13vw, 4rem);
			line-height: 1.08;
		}
		.hs-tagline {
			font-size: clamp(2.2rem, 9.5vw, 3rem);
			margin-bottom: .8rem;
		}
		.hs-description {
			max-width: 22rem;
			font-size: 1rem;
			line-height: 1.45;
			margin-bottom: .9rem;
		}
		.hs-actions {
			gap: .5rem;
		}
		.hs-actions a {
			min-height: 2.75rem;
			gap: .65rem;
			padding: .6rem .9rem;
			font-size: .8rem;
		}
		.hs-sidekick {
			width: min(100%, clamp(8.5rem, 20svh, 13rem));
			max-width: 13rem;
		}
		.hs-example {
			grid-template-columns: minmax(0, 1fr);
			gap: .65rem;
			padding: .85rem 0;
		}
		.hs-example-top {
			margin-bottom: .35rem;
		}
		.hs-example-text {
			min-height: 5.5rem;
		}
		.hs-bridge {
			padding-block: .8rem;
			font-size: .95rem;
		}
	}

	@media (max-width: 360px) {
		.hs-main {
			padding-inline: 1.15rem;
		}
		.hs-actions a {
			padding-inline: .75rem;
			font-size: .78rem;
		}
		.hs-choices button {
			padding-inline: .85rem;
		}
		.hs-example-text {
			min-height: 6.5rem;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.hs-example-text.animate {
			animation: none;
		}
		.hs-actions a {
			transition: none;
		}
		.hs-actions a:hover {
			transform: none;
		}
	}
</style>
