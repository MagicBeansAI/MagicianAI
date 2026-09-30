<script lang="ts">
	// THE CHROME — what the lockup becomes once it has finished arriving.
	//
	// `BrandReveal` takes "Magican" and "Personal Superintelligence" from a huge
	// centred wordmark down to a settled two-line lockup, and then that lockup
	// has nowhere to go: it scrolls away with its own track, and the promises
	// road that follows has no brand on it at all. Owner call: once the
	// promise headings need the centre, Magican moves to the top LEFT and
	// Personal Superintelligence to the top RIGHT, and both stay there through
	// the rest of the page.
	//
	// So this layer is `position: fixed`, not sticky — sticky would end with
	// whatever container held it, and "the rest of the page" means past the
	// field, past the montage, all the way down. It fades in as the lockup
	// finishes and then never leaves.
	//
	// It also carries the promise heading. Each of "Knows you" / "Acts for
	// you" / "Belongs to you" arrives the way Personal Superintelligence did —
	// growing into the centre — then rises to sit under the corners and HOLDS
	// there for the whole of its own act, three stations, before handing off
	// to the next. The heading is the one thing on this layer that changes;
	// the corners are fixed furniture from the moment they form.
	//
	// COUPLING, stated rather than hidden: this reads `.br-track` and
	// `.rt-track` out of the DOM on mount. Both are stable class names owned
	// by BrandReveal and RoadTrack respectively, and the alternative — routing
	// refs down through LandingField's slot — is not expressible in Svelte
	// slots without prop-drilling through components that have no other reason
	// to know about each other. If either class is renamed, this layer goes
	// quiet rather than breaking: every phase falls back to "not started".
	import { motionEnabled } from '$lib/motion';
	import { clamp } from './scrub';
	import {
		HEAD_SETTLED,
		actIndex as actIndexStore,
		actLocal as actLocalStore,
		heroPhase,
		overBackdrop,
		roadActive
	} from './landingPhase';

	$: reduced = !$motionEnabled;

	/** The three acts, in road order. Matches PathPromise's own act signs. */
	const PROMISES = ['Knows you', 'Acts for you', 'Belongs to you'] as const;

	/** Where in BrandReveal's own track the lockup hands off to the corners.
	 *  Reported live: the corners were visible from the beginning and should
	 *  only appear as the stations do. So this sits at the very END of the
	 *  hero's track — the lockup holds at top centre for the whole persona
	 *  beat and only splits outward on the last stretch before the road
	 *  pins, which is the moment the first station arrives. */
	const FORM_AT = 0.9;
	const FORM_TO = 1;
	// NOTE: these are in HERO phase, not whole-track progress. `heroPhase`
	// normalises the hero's own share to 0..1 and pins at 1 once the road
	// begins, so 0.9-1.0 still means "the last stretch before the stations".

	/** How far in from its corner each word starts, in vw. At formed=0 the
	 *  pair sits together near centre — reading as the lockup BrandReveal
	 *  just settled — and slides apart to the corners as it forms. A
	 *  crossfade between two centred and two cornered copies would read as a
	 *  swap; this reads as the move the owner actually described. */
	const SPLIT_VW = 0;

	/** What the pair is scaled to at formed=0 — roughly the settled lockup's
	 *  own size, so the words LEAVE at the size they were and arrive small.
	 *  Reported live: the first version faded small corner text in while the
	 *  big lockup faded out somewhere else, which reads as two elements
	 *  swapping rather than one travelling. Size is most of what sells the
	 *  move; the translate alone never did. */
	const SPLIT_START_SCALE = 2.4;

	/** Per-act local progress: the heading grows in, then rises, then holds
	 *  for the remainder of its act. Holding is most of the act by design —
	 *  the heading is a label for three stations, not a beat of its own. */
	const HEAD_IN_END = 0.14;
	const HEAD_RISE_END = HEAD_SETTLED;

	/** Where it starts: big, and low enough that "big" reads as the centre of
	 *  the screen rather than a large word near the top. Its settled place is
	 *  ~7vh from the top, so ~40vh of travel puts the arrival at mid-screen. */
	const HEAD_START_SCALE = 2.8;
	const HEAD_TRAVEL_VH = 40;



	function smoothstep(edge0: number, edge1: number, x: number): number {
		const t = clamp((x - edge0) / (edge1 - edge0), 0, 1);
		return t * t * (3 - 2 * t);
	}

	// Purely reactive off the stores BrandReveal publishes each frame — no
	// scroll listener, no rAF, no DOM queries. The earlier version measured
	// `.br-track` and `.rt-track` itself, which stopped working the moment the
	// road moved inside the hero's pinned stage: an absolutely positioned
	// track has a rect that no longer moves with scroll. Reading the phase
	// the hero already computed is both correct and less machinery.
	$: formed = smoothstep(FORM_AT, FORM_TO, $heroPhase);
	// Read, never recomputed. Dividing road progress by three here is what put
	// this component's idea of "act three" out of step with the road's own —
	// the acts are weight-derived, not equal thirds.
	$: actIndex = -1; // disabled for now
	$: headIn = actIndex < 0 ? 0 : smoothstep(0, HEAD_IN_END, $actLocalStore);
	$: headRise = actIndex < 0 ? 0 : smoothstep(HEAD_IN_END * 0.4, HEAD_RISE_END, $actLocalStore);

	// Reduced motion resolves straight to the formed state — the corners are
	// wayfinding, not decoration, so they should be present rather than
	// animated in. The heading still tracks its act (that is information, not
	// motion); only the grow/rise easing collapses.
	$: cornerOpacity = reduced ? 1 : formed;
	/** Positive pushes Magican right (toward centre); PS mirrors it. */
	$: splitIn = reduced ? 0 : (1 - formed) * SPLIT_VW;
	$: splitScale = reduced ? 1 : SPLIT_START_SCALE - formed * (SPLIT_START_SCALE - 1);
	// The promise heading now provides the entrance the removed PathActSign
	// used to: it ARRIVES big at the centre of the screen and then shrinks and
	// rises into its settled place above the stations, rather than fading up
	// small from just below it. Scale and travel ride the same `headRise` so
	// they are one movement — the earlier version grew on `headIn` while
	// travelling on `headRise`, which is exactly the desync that made the
	// wordmark collide with itself earlier in this component's life.
	$: headScale = reduced ? 1 : HEAD_START_SCALE - headRise * (HEAD_START_SCALE - 1);
	$: headY = reduced ? 0 : (1 - headRise) * HEAD_TRAVEL_VH;
	$: headOpacity = reduced ? 1 : headIn;
</script>

<div class="lc" class:on={cornerOpacity > 0.01} class:onlight={!$overBackdrop}>
	<span
		class="lc-corner lc-magican"
		aria-hidden="true"
		style="opacity:{cornerOpacity}; transform: translateX({splitIn}vw) scale({splitScale})"
		>magican</span
	>
	<a class="lc-corner lc-manifesto" href="/manifesto">Manifesto</a>

	{#if actIndex >= 0}
		{#key actIndex}
			<p
				class="lc-promise"
				style="opacity:{headOpacity}; transform: translate(-50%, {headY}vh) scale({headScale})"
			>
				{PROMISES[actIndex]}
			</p>
		{/key}
	{/if}
</div>

<style>
	/* Fixed, not sticky: "the rest of the page" outlives any container this
	   could sit inside. `pointer-events: none` throughout — this layer is
	   furniture and must never intercept a click meant for the page under it.
	   The left Magican mark is still decorative (the h1 is the name). Manifesto
	   is a real control, so the layer is not aria-hidden — only the mark is. */
	.lc {
		position: fixed;
		inset: 0;
		z-index: 3;
		pointer-events: none;
		font-family: var(--lp-font);
	}

	/* Cream page, no dusk sky: corners are page ink. `.onlight` is the
	   default while overBackdrop is false. */
	.lc.onlight .lc-corner {
		color: var(--text-primary, #1a1612);
		background: none;
	}
	.lc-corner {
		transition:
			opacity 420ms cubic-bezier(0.33, 1, 0.68, 1),
			color 520ms ease,
			background 520ms ease;
		position: absolute;
		top: clamp(0.9rem, 2.4vh, 1.6rem);
		font-weight: 320;
		letter-spacing: -0.01em;
		font-size: clamp(0.82rem, 1.5vw, 1.05rem);
		color: var(--text-primary, #1a1612);
	}
	.lc-magican {
		transform-origin: 0% 50%;
		left: clamp(1rem, 3vw, 2rem);
		font-weight: 420;
		letter-spacing: 0.02em;
	}
	.lc-manifesto {
		transform-origin: 100% 50%;
		right: clamp(1rem, 3vw, 2rem);
		pointer-events: auto;
		text-decoration: none;
		color: var(--text-primary, #1a1612);
	}
	.lc-manifesto:hover,
	.lc-manifesto:focus-visible {
		text-decoration: none;
		opacity: 0.72;
	}

	/* The promise heading: centred horizontally, sitting under the corners.
	   `translate(-50%, …)` carries the centring, so the inline transform
	   above must always restate it — a bare `translateY` here would knock it
	   half a width to the right. */
	.lc-promise {
		position: absolute;
		left: 50%;
		top: clamp(4.2rem, 11vh, 7rem);
		margin: 0;
		white-space: nowrap;
		font-size: clamp(1.5rem, 4.2vw, 3rem);
		font-weight: 380;
		letter-spacing: -0.02em;
		line-height: 1.15;
		color: var(--text-primary, #1a1612);
		transform-origin: 50% 50%;
	}

	@media (prefers-reduced-motion: reduce) {
		.lc-corner {
			transition: none;
		}
	}
</style>
