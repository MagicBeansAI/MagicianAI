<!--
	The claim, made once, right after the hero names the visitor as many
	people and before anything is asked to prove it.

	The hero used to open on a bare category claim ("Superintelligence +
	you") that any AI company could print — that is why it was retired from
	there (see LandingHero's own header comment). It belongs here instead,
	on the far side of the hero's turn: the visitor has just been told their
	AI has only met one of them, and THIS is the promise that answers it —
	made before the day proves it and before Trust explains who it answers
	to, not among them. TrustReveal's kicker ("A superintelligence with one
	user.") deliberately reprises the word once the day has earned it; this
	is the declaration, that is the payment.

	The kicker plants the "not rented" half of the claim early and small —
	a single ownership beat: yours, not borrowed from a company that also
	answers to its shareholders — without repeating TrustReveal's fuller
	argument (the vault, on-device intelligence, the approval gate) ahead of
	its own proof.

	DYNAMISM. A static line dropped between the hero and the day read as
	inert — the same complaint as the page generally. This section is
	therefore never a still paragraph: the aurora behind it drifts on its
	own slow clock the instant it mounts, and the line itself arrives on
	scroll rather than sitting there pre-resolved. Deliberately NOT
	`MoteField` (the retired film's particle/thread field): that module is
	built tightly around `MovieTrack`'s multi-station camera and world
	geometry, and half-adapting a coupled system under time pressure is
	exactly the mistake that broke this page earlier today. This is a
	self-contained CSS layer with no dependency on the film at all.
-->
<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { motionEnabled } from '$lib/motion';

	$: reduced = !$motionEnabled;

	let el: HTMLElement | null = null;
	// Resolved immediately under reduced motion (no hold-and-reveal to skip);
	// otherwise arms once, the first time the section is actually on screen,
	// and never rearms — a visitor scrolling back up should not watch the
	// claim fade out and back in.
	let shown = false;
	let io: IntersectionObserver | null = null;

	onMount(() => {
		if (reduced || !el) {
			shown = true;
			return;
		}
		io = new IntersectionObserver(
			(entries) => {
				if (entries[0]?.isIntersecting) {
					shown = true;
					io?.disconnect();
					io = null;
				}
			},
			{ threshold: 0.4 }
		);
		io.observe(el);
	});

	onDestroy(() => io?.disconnect());
</script>

<section class="dc" aria-label="The claim" bind:this={el}>
	<div class="dc-aurora" class:still={reduced} aria-hidden="true"></div>
	<p class="dc-kicker">Not rented. Not shared.</p>
	<h2 class="dc-line" class:in={shown}>
		Magican is the <em>Personal Superintelligence</em><br />
		for everyone you are.
	</h2>
</section>

<style>
	.dc {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		padding: clamp(4.5rem, 13vh, 7.5rem) clamp(1.2rem, 5vw, 2.5rem);
		text-align: center;
		font-family: var(--lp-font);
		color: var(--text-primary);
	}

	/* Three blurred, colour-mixed blobs on the theme's own accent pair,
	   drifting on a slow independent clock — never in step with anything
	   scroll-driven, so it reads as ambient rather than as a cue. */
	.dc-aurora {
		position: absolute;
		inset: -25% -10%;
		z-index: 0;
		pointer-events: none;
		background:
			radial-gradient(
				38% 46% at 22% 28%,
				color-mix(in srgb, var(--lp-primary) 30%, transparent),
				transparent 70%
			),
			radial-gradient(
				34% 40% at 80% 64%,
				color-mix(in srgb, var(--lp-secondary) 26%, transparent),
				transparent 72%
			),
			radial-gradient(
				30% 34% at 46% 92%,
				color-mix(in srgb, var(--lp-primary) 16%, transparent),
				transparent 74%
			);
		filter: blur(48px);
		animation: dc-drift 24s ease-in-out infinite alternate;
	}
	.dc-aurora.still {
		animation: none;
	}

	.dc-kicker {
		position: relative;
		z-index: 1;
		margin: 0 0 0.9rem;
		font-family: var(--lp-mono);
		font-size: 0.68rem;
		font-weight: 650;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--landing-subtitle);
	}

	.dc-line {
		position: relative;
		z-index: 1;
		margin: 0 auto;
		max-width: 46rem;
		font-size: clamp(2rem, 5.6vw, 3.6rem);
		font-weight: 660;
		letter-spacing: -0.024em;
		line-height: 1.14;
		/* Arrives on scroll rather than sitting pre-resolved — see the
		   header comment on why this section cannot afford to be inert. */
		opacity: 0;
		transform: translateY(16px);
		transition:
			opacity 640ms cubic-bezier(0.16, 1, 0.3, 1),
			transform 640ms cubic-bezier(0.16, 1, 0.3, 1);
	}
	.dc-line.in {
		opacity: 1;
		transform: none;
	}
	.dc-line em {
		font-style: normal;
		background: var(
			--landing-title-gradient,
			linear-gradient(100deg, var(--text-primary), var(--lp-primary), var(--lp-secondary))
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}

	@media (prefers-reduced-motion: reduce) {
		.dc-aurora {
			animation: none;
		}
		.dc-line {
			opacity: 1;
			transform: none;
			transition: none;
		}
	}
</style>
