<script lang="ts">
	// The film, reduced to its closing montage — devices working untouched
	// while people live. `born`, `operate`, `drown` and `tear` are all cut;
	// this is what is left of the argument, and it is the payoff, standing
	// alone.
	//
	// This is a THIN HOST, not a new player. `LifeReel.svelte` already owns
	// every frame of decode, cache and draw — configure/tick/prime/isOn/geom,
	// an onState callback and a base prop, and nothing else. What it does not
	// own is a clock: it is driven, not self-running (unlike `DayTrack`, which
	// scrubs itself). So this file supplies exactly what MovieTrack used to —
	// a scrubbed station, a sticky stage and one rAF loop calling `tick` — and
	// nothing MovieTrack also supplied that this cut does not need: no
	// camera, no world, no thread.
	//
	// `LifeDevice.svelte` IS mounted alongside the reel, driven off the same
	// clock — it is what puts the captions and the composited laptop/phone
	// screens on the plate; without it the montage is just footage of a room,
	// which is not the payoff. (An earlier pass here left it unmounted,
	// reading "the montage" as the frame sequence alone; watching the result
	// live showed that reading was wrong — the caption and the superimposed
	// screen ARE the montage, not decoration on top of it.)
	//
	// THE DRIVE CONTRACT IS LIFTED, NOT REINVENTED. `driveLife()` in
	// MovieTrack.svelte computed a station-local `u` from its multi-station
	// camera position, then fed LifeReel and LifeDevice three numbers every
	// frame:
	//   frameIn   = clamp(u * LIFE_IN, 0, 1)         — the PLATE's own opacity
	//   lifeAlpha = clamp((u + 0.15) / 0.17, 0, 1)   — what `tick()` receives
	//   (frameIn * lifeAlpha)                        — what LifeDevice receives
	// as its `seen` — the device only starts ticking once the whole composite
	// is actually visible, not just the reel's own layer.
	// All three are reproduced verbatim below. Only where `u` comes from
	// changes: MovieTrack derived it from a smoothed, multi-station camera
	// chasing scroll across 36 weight-units of track; this station is the
	// whole track, so `u` is simply this scrubber's own `local` — already a
	// plain 0..1 read off scroll geometry, with no second easing layer over
	// it. (That is also why `u` never goes negative here the way MovieTrack's
	// could: there is no earlier station for the camera to still be leaving.
	// `lifeAlpha` starts already most of the way up as a result — a real
	// difference in the film's feel, but not a new curve; the same fraction
	// fed a different domain.)
	//
	// THE PAYOFF NOW BRACKETS THE MONTAGE (2026-08-17, retimed same day).
	// `Greeting.svelte`'s standalone "Love what you do." <-> "Do what you
	// love." swap used to pay off the argument well after Trust, on its own
	// separate section. It is retired from that position — see
	// +page.svelte's own comment — and this station now asks and answers it
	// itself:
	//   "Love what you do?" — a question, not the old statement, since
	//   nothing has been shown yet to have earned the flat claim.
	//   "Go. Do what you love." — arrives early (u > 0.2, roughly a fifth
	//   of the way into an 800svh station), so it is visible for most of
	//   the reel rather than only its last quarter, and stays lively
	//   (a slow text-clipped shimmer, the same technique BrandReveal's
	//   "Magican" uses) for as long as the reel keeps playing under it —
	//   the point is that the machine is still working while this is
	//   already true, not that it stops to say so.
	//
	// "LOVE WHAT YOU DO?" IS NOT PART OF THE STICKY STAGE (second fix, same
	// day — reported live as still too much dead space after the first
	// fix). It was, at first: absolutely positioned dead-center inside
	// `.lf-stage`'s own 100svh sticky box, faded on `entered`. That measured
	// badly. `scrub.ts`'s `trackProgress` holds this station's own `u` at
	// exactly zero for the FULL viewport-height a `position:sticky` box
	// spends approaching the pin, and a dead-center line inside that box
	// sits *another* half-viewport below that. A live probe at the exact
	// scroll offset where ProofSwitcher's last pixel left the viewport
	// measured the line at 414px below the fold, opacity still "0" —
	// nothing onscreen until roughly another half-viewport of scrolling.
	// Shrinking RoadTrack's own last-beat dwell (`roadTrack.ts`'s
	// `LAST_BEAT_WEIGHT`) narrowed the gap but could not close it: the
	// remaining distance was never dwell, it was this line's own position,
	// nested a half-viewport deep inside a box that had not yet arrived.
	//
	// The fix is structural, not timing. `.lf-open` is now a plain,
	// normal-flow block — the first child of `#life-track`, OUTSIDE
	// `.lf-stage` — not absolutely positioned, not centered in a sticky
	// box. It appears the ordinary way, by ordinary scrolling, immediately
	// adjacent to wherever ProofSwitcher's content ends, and leaves the
	// same way: carried off the top of the viewport by the visitor's own
	// continued scroll, once `.lf-stage` needs the room to begin sticking.
	// No `u`-driven fade-out is needed for it any more, only a CSS
	// `transition` on `entered` for the fade-in (see `paintBrackets`'s own
	// comment, which now speaks only to `.lf-close`).
	import { onDestroy, onMount } from 'svelte';
	import { motionEnabled } from '$lib/motion';
	import { clamp, createScrubber } from './scrub';
	import LifeReel from './LifeReel.svelte';
	import LifeDevice from './LifeDevice.svelte';

	$: reduced = !$motionEnabled;

	// The montage weighed 25.2 of the old film's 36 units. A first cut of
	// this host picked 240svh — inside the 200–300vh room the plan for this
	// change suggested — and watching it scroll live it was badly wrong: the
	// scenes and the case-by-case screen work (LifeDevice's `WORK_RATE`
	// cadence, each case's own arc) were AUTHORED against something closer to
	// the old station's pace, and 240svh ran the whole 369-frame reel past in
	// a few wheel-turns — reported live as "3-4x" too fast, which matches:
	// 240 × ~3.5 ≈ 840. 800svh keeps the montage a full scroll of its own
	// again — long enough for each scene's caption and case work to actually
	// land — while still well short of the old 2520svh (weight 25.2 of a
	// 3600svh whole film).
	const TRACK_VH = 800;

	// Lifted from MovieTrack's `driveLife()` — see the header comment.
	const LIFE_IN = 45;

	const scrub = createScrubber([0, 1]);
	const scrubState = scrub.state;

	let lifeReel: LifeReel | null = null;
	let lifeDevice: LifeDevice | null = null;
	let trackEl: HTMLElement | null = null;
	let stageEl: HTMLElement | null = null;
	let frameEl: HTMLElement | null = null;
	let closeEl: HTMLElement | null = null;

	// THE ABSENCE CONTRACT, same as MovieTrack's: the footage is generated
	// separately and may not exist yet. `lifeOn` only ever flips true once a
	// manifest actually resolves, and the frame has no box — no border, no
	// reserved rectangle — until then. The bracketing payoff line does NOT
	// wait on it (see `paintBrackets`) — an absent reel leaves the frame's
	// own span empty paper, but "Go. Do what you love." still lands on
	// schedule, because the visitor's own scroll is proof enough on its own.
	let lifeOn = false;

	// The reel is mounted once, the first time the station is actually
	// reachable, and never unmounted again — the same one-way latch
	// MovieTrack used (`lifeMounted`), so a visitor scrolling back and forth
	// across the boundary can't repeatedly tear down and rebuild the bitmap
	// cache. MovieTrack timed this off its own smoothed `u`, using a station
	// nobody could reach without 20 screens of prior scroll to prove intent;
	// this station is now the SECOND thing on the page, so there is no such
	// signal in scroll position. `visible`, the IntersectionObserver flag
	// this file already needs for the rAF loop below, says the same thing
	// more honestly: the station is on screen.
	let visible = false;
	let entered = false;

	$: if (visible && !entered) {
		entered = true;
		// Next frame: Svelte has patched the DOM by then, so `lifeReel` (the
		// component this `{#if entered}` block below just mounted) and
		// `frameEl`'s box are both real.
		requestAnimationFrame(() => {
			configure();
			lifeReel?.prime();
		});
	}

	let u = 0;
	$: u = $scrubState.local;

	function onLifeState(on: boolean): void {
		lifeOn = on;
		if (!on) return;
		// The frame has no box until `.on` gives it one (see the CSS); its
		// size is unknowable before that class has actually painted.
		requestAnimationFrame(() => configure());
	}

	function configure(): void {
		const r = frameEl?.getBoundingClientRect();
		if (!r || r.width === 0) return;
		lifeReel?.configure(r.width, r.height);
	}

	// ── ONE rAF loop, gated exactly like MovieTrack's own `syncLoop`/`frame`:
	// running only while mounted, on screen and motion is allowed. scrub.ts's
	// listener is its own, separate rAF that only coalesces scroll events
	// into `scrubState` — this loop is the one and only place `tick()` is
	// called, once per animation frame, off whatever `u` that store last
	// reported. No second frame source is added.
	const rt: { raf: number; running: boolean } = { raf: 0, running: false };
	let mounted = false;

	function syncLoop(): void {
		const should = mounted && visible && !reduced;
		if (should && !rt.running) {
			rt.running = true;
			rt.raf = requestAnimationFrame(frame);
		} else if (!should && rt.running) {
			rt.running = false;
			if (rt.raf) cancelAnimationFrame(rt.raf);
			rt.raf = 0;
		}
	}
	$: {
		// Re-arm whenever reduced-motion is toggled live.
		reduced;
		syncLoop();
	}

	/**
	 * The answer, "Go. Do what you love." — runs on `u` alone, independent
	 * of whether the footage ever loads: the payoff must not go missing
	 * just because a manifest 404s.
	 *
	 * The question, "Love what you do?", is not painted here any more — see
	 * the header comment for why it moved out of the sticky stage entirely.
	 * It fades on `entered` via a plain CSS `transition` driven straight
	 * from the markup (`class:in={entered}`), with no per-frame opacity
	 * write and no dependency on `u`.
	 */
	function paintBrackets(u: number): void {
		// Early — visible for most of the reel, not just its last quarter,
		// so "the montage is playing along" is true of most of the montage.
		const closeIn = clamp((u - 0.2) / 0.1, 0, 1);
		if (closeEl) closeEl.style.opacity = closeIn.toFixed(3);
	}

	function frame(): void {
		if (!rt.running) return;
		rt.raf = requestAnimationFrame(frame);
		paintBrackets(u);
		if (!lifeOn) {
			// One manifest request, asked as soon as the station is reachable;
			// a 404 settles the empty-paper ending for good.
			lifeReel?.prime();
			return;
		}
		const frameIn = clamp(u * LIFE_IN, 0, 1);
		if (frameEl) frameEl.style.opacity = String(frameIn);
		const alpha = clamp((u + 0.15) / 0.17, 0, 1);
		lifeReel?.tick(u, alpha);
		// AFTER the reel, and from the reel's own geometry — the device has to
		// be told where the plate actually landed, since the plate is
		// contain-fitted and that fit is the reel's private arithmetic.
		// `seen` (frameIn * alpha) is the WHOLE composite's visibility, not
		// just the plate's, so the device's own work-clock waits on both
		// fades rather than starting behind a frame that is still arriving.
		lifeDevice?.place(lifeReel?.geom() ?? null, alpha, frameIn * alpha);
	}

	let io: IntersectionObserver | null = null;
	let onResize: (() => void) | null = null;

	onMount(() => {
		mounted = true;
		onResize = () => configure();
		window.addEventListener('resize', onResize, { passive: true });
		if (trackEl) {
			io = new IntersectionObserver(
				(entries) => {
					visible = entries[0]?.isIntersecting ?? false;
					syncLoop();
				},
				// Extended 20% past the real viewport bottom (not pulled in),
				// on purpose: `.lf-open` is now the very first thing in this
				// track (see the header comment), and `entered` drives its
				// fade-in. Firing only once the track has actually crossed
				// into the literal viewport left the 320-420ms CSS transition
				// racing the scroll — the line was still visibly mid-fade by
				// the time it reached its resting position. Widening the
				// margin lets `entered` flip, and the fade start, shortly
				// before the track is on screen, so it reads as already-
				// arrived rather than caught arriving.
				{ threshold: 0, rootMargin: '0px 0px 20% 0px' }
			);
			io.observe(trackEl);
		} else {
			visible = true;
			syncLoop();
		}
	});

	onDestroy(() => {
		mounted = false;
		rt.running = false;
		if (rt.raf) cancelAnimationFrame(rt.raf);
		if (onResize && typeof window !== 'undefined') window.removeEventListener('resize', onResize);
		io?.disconnect();
	});
</script>

<!-- The outer track and sticky stage persist regardless of `reduced` — same
     shape DayTrack uses — so the scrub action and the observer are never torn
     down and rebuilt on a live motion-preference toggle. The reel, its frame
     and the opening flourish are conditional on motion; the payoff is not —
     it is real content, not decoration, so reduced motion still gets it,
     settled and static rather than gone. -->
<div
	id="life-track"
	class="lf-track"
	class:static={reduced}
	style={reduced ? '' : `height:${TRACK_VH}svh`}
	use:scrub.track
	bind:this={trackEl}
>
	{#if !reduced}
		<p class="lf-open" class:in={entered} aria-hidden="true">Love what you do?</p>
	{/if}
	<div class="lf-stage" class:static={reduced} bind:this={stageEl}>
		{#if !reduced}
			<div class="lf-frame" class:on={lifeOn} bind:this={frameEl}>
				{#if entered}
					<LifeReel bind:this={lifeReel} onState={onLifeState} />
					<LifeDevice bind:this={lifeDevice} />
				{/if}
			</div>
			<p class="lf-close" bind:this={closeEl}>Go. Do what you love.</p>
		{:else}
			<p class="lf-close static">Go. Do what you love.</p>
		{/if}
	</div>
</div>

<style>
	.lf-track {
		--lf-a: var(--accent-primary, #9e59ff);
		--lf-b: var(--accent-secondary, var(--lf-a));
		position: relative;
	}
	.lf-track.static {
		height: auto;
	}
	.lf-stage {
		position: sticky;
		top: 0;
		height: 100svh;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: clamp(1.4rem, 4vh, 2.4rem);
	}
	.lf-stage.static {
		position: static;
		height: auto;
		overflow: visible;
		padding: clamp(3rem, 10vh, 6rem) 1.5rem;
	}

	/* The question — see the header comment for why this is deliberately a
	   plain, normal-flow block and not part of `.lf-stage`'s sticky box. It
	   sits in ordinary document flow, directly above the sticky stage, so it
	   arrives and leaves by ordinary scrolling rather than by waiting on a
	   100svh box to travel into position. `entered` is a one-way latch (see
	   the script), so `.in` only ever turns on; nothing here needs to turn
	   it back off — continued scroll carries the line off the top of the
	   viewport on its own once `.lf-stage` starts sticking. */
	.lf-open {
		position: relative;
		margin: 0 auto;
		max-width: 90vw;
		padding: clamp(4rem, 16vh, 9rem) 1.5rem;
		text-align: center;
		font-family: var(--lp-font);
		font-size: clamp(1.8rem, 4.9vw, 3.4rem);
		font-weight: 400;
		letter-spacing: -0.015em;
		color: var(--text-primary);
		opacity: 0;
		transition: opacity 420ms ease;
	}
	.lf-open.in {
		opacity: 1;
	}

	/* The answer. Same text-clipped shimmer BrandReveal's "Magican" uses —
	   lifted, not reinvented — so it stays visibly alive for as long as the
	   reel keeps playing beneath it rather than reading as a finished
	   caption the moment it lands. */
	.lf-close {
		margin: 0;
		max-width: 90vw;
		text-align: center;
		font-family: var(--lp-font);
		font-size: clamp(1.9rem, 5.1vw, 3.6rem);
		font-weight: 440;
		letter-spacing: -0.015em;
		opacity: 0;
		background: linear-gradient(
			100deg,
			var(--lf-a) 0%,
			var(--lf-b) 30%,
			var(--text-primary) 55%,
			var(--lf-a) 80%,
			var(--lf-b) 100%
		);
		background-size: 260% 100%;
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		animation: lf-shimmer 8s ease-in-out infinite;
	}
	/* The reduced-motion fallback: resolved and static, same as every other
	   scrubbed beat on this page under `prefers-reduced-motion`. */
	.lf-close.static {
		opacity: 1;
	}

	@keyframes lf-shimmer {
		0%,
		100% {
			background-position: 0% 50%;
		}
		50% {
			background-position: 100% 50%;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.lf-close {
			animation: none;
		}
		.lf-open {
			transition: none;
		}
	}

	/* No box until there is footage to put in it — see the absence contract
	   in the script header. */
	.lf-frame {
		display: none;
	}
	.lf-frame.on {
		display: block;
		position: relative;
		width: min(94vw, 1360px, calc(62svh * 16 / 9));
		aspect-ratio: 16 / 9;
		overflow: hidden;
		/* No card: a border and a shadow would announce a window onto the
		   footage; a soft feather on all four edges lets the plate dissolve
		   into the page's own paper instead, lifted verbatim from
		   MovieTrack's `.mt-lifeframe.on`. ASYMMETRIC, and it has to be: left
		   and top are pure plate, so they fade generously; right and bottom
		   are where LifeDevice's machine and its caption corner sit, so
		   those fades are SHORT — deep enough to still reach fully
		   transparent, but not so deep they dissolve the device's own edge
		   or eat the caption type sitting at 7.6%/95% of the frame. */
		--feather: linear-gradient(to right, transparent 0, #000 5.5%, #000 95%, transparent 100%),
			linear-gradient(to bottom, transparent 0, #000 10%, #000 95.5%, transparent 100%);
		-webkit-mask-image: var(--feather);
		mask-image: var(--feather);
		-webkit-mask-composite: source-in;
		mask-composite: intersect;
		/* Driven from the script, on the same clock as `tick()`'s alpha —
		   zero here so the frame can never flash at full before the first
		   tick sets it. */
		opacity: 0;
	}
</style>
