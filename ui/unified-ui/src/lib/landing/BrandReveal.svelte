<!--
	The opening beat (2026-08-17, extended same day): a scroll-choreographed
	brand lockup replaces the role-cycle hero and the standalone Declaration
	section in one continuous move, then hands off to a second, timed beat.

	SCROLL-DRIVEN (u, the track's own progress):
	   1. "Magican" alone, dead centre, huge, with an aurora simmering INSIDE
	      the letters — a shimmering gradient clipped to the text, not a
	      background layer behind it.
	   2. Scrolling shrinks "Magican" and lifts it while "Personal
	      Superintelligence" grows into the centre it just vacated, then
	      shrinks again to join it as a second line — a compact two-line
	      lockup, settled by u≈0.42.
	   3. Further scroll brings up "for the" below the lockup — big, then
	      shrinking continuously as u advances, gone by the time its
	      opacity reaches zero, a real VANISH rather than a settle (unlike
	      Magican/PS above, which shrink and then persist). The persona group
	      takes over the exact same spot as "for the" clears it: a big
	      cycling word — ~100 of them now, not ten, re-grammared for "for
	      the ___ in you" — with a small, static, never-cycling "in you"
	      caption fixed underneath it. The motif thread (lifted from the
	      retired LandingHero) fades up alongside the persona group and
	      continues downward, carrying the eye into whatever comes next.

	TIMED, NOT SCROLLED: once the persona group has actually arrived on
	screen (u crosses ARRIVE, latched one-way — scrolling back up does not
	un-arm it, matching LifeTrack's convention), the word keeps cycling
	through the ~100 roles on its OWN clock, not the scrollbar's — each
	change is its own small "comes big, settles" pop (`in:scale`/`out:scale`
	on the `{#key personaIndex}` block, not a scroll-driven transform; see
	paint()'s own comment on why only the cycling word gets this treatment,
	not "for the" and not "in you"). Each hold is shorter than the last —
	`holdMsFor` decays smoothly across the WHOLE lap now, not a ten-value
	table repeating ten times — the cadence itself argues "you are all of
	these, and more of them than you have time to read" — then resets slow
	for the next lap rather than settling on one.

	Both words in the lockup share ONE grid cell (`grid-area: 1 / 1`, the
	RoleCycle stacking trick already used elsewhere in this codebase) so
	they start and move from the exact same centre point; only a JS-driven
	translate+scale on each separates them.

	`LandingHero.svelte`/`RoleCycle.svelte`/`Declaration.svelte` stay on
	disk, unmounted, exactly like the earlier film cut — deletion is a
	separate pass.
-->
<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { scale } from 'svelte/transition';
	import { cubicIn, cubicOut } from 'svelte/easing';
	import { motionEnabled } from '$lib/motion';
	import { clamp, createScrubber } from './scrub';
	import { roadBounds, roadStations, roadTrackSvh } from './roadTrack';
	// import ProofSwitcher from './ProofSwitcher.svelte';
	import {
		HEAD_SETTLED,
		actIndex,
		actLocal,
		heroPhase,
		overBackdrop,
		roadActive,
		roadPhase
	} from './landingPhase';

	$: reduced = !$motionEnabled;

	// Room for both beats: the lockup morph (was 280svh alone) compressed
	// into its first 42%, the phrase hand-off filling the rest.
	/** The hero's own beats. */
	const HERO_VH = 380;
	/** The promise road's scroll room, taken from its own geometry rather
	 *  than guessed — it must be exactly what RoadTrack would have given
	 *  itself standing alone, or the camera runs out of track or overruns. */
	const ROAD_VH = 0; // roadTrackSvh(roadStations(3, 3, false));
	/** ONE track for both. The stations used to live in a second sticky
	 *  section after this one, and two adjacent sticky sections always read
	 *  as two places: the first unpins and scrolls away, the second pins.
	 *  Reported live, four times, in four different words. One track with one
	 *  stage that changes what it holds is the only arrangement that reads as
	 *  staying put. */
	const TRACK_VH = HERO_VH + ROAD_VH;
	/** Where the hero's share ends and the road's begins, in u. */
	const ROAD_START = HERO_VH / TRACK_VH;
	/** Acts on the promise road, and where each one ACTUALLY begins in the
	 *  road's own progress. Not thirds: `roadBounds` derives these from
	 *  station weights, and the last beat weighs half what the other eight do,
	 *  so the true edges are 0, 6/17, 12/17, 1. Assuming thirds put act three
	 *  at [0.667, 1] while the road had it at [0.706, 1], and the camera flew
	 *  past the final card without ever centring it. */
	const PROMISE_ACTS = 3;
	const BEATS_PER_ACT = 3;
	const ACT_EDGES = ((): number[] => {
		const b = roadBounds(roadStations(3, 3, false));
		return Array.from({ length: PROMISE_ACTS + 1 }, (_, k) =>
			k === PROMISE_ACTS ? 1 : (b[k * BEATS_PER_ACT] ?? k / PROMISE_ACTS)
		);
	})();
	/** Fraction of each act spent on its title alone, before the stations
	 *  appear at all. Must exceed LandingChrome's own HEAD_RISE_END (0.26),
	 *  or the card is on screen while the heading is still flying in — which
	 *  is exactly what 0.16 did. */
	const TITLE_HOLD = 0.38;
	/** Where the lockup morph finishes and the phrase beat's own window
	 *  begins — see `paint()`. */
	const MORPH_END = 0.42;
	/** Once u crosses this, the persona group has fully arrived and its
	 *  timer arms. "for the" folding into the group as a small static
	 *  caption (rather than being its own separate scroll beat first)
	 *  moved the group's own arrival earlier still — see paint()'s comment
	 *  on `personaGroupOpacity` (now smoothstep(0.34, 0.46, u)); ARRIVE
	 *  sits just after that window closes, same relationship as before. */
	const ARRIVE = 0.48;

	/** Where the hero's own centre content begins dissolving so the road can
	 *  take the screen without anything scrolling off it first. Sits inside
	 *  LandingChrome's own FORM_AT (0.9) so the corners are already pulling
	 *  out of the lockup as the lockup goes. */
	const EXIT_START = 0.88;

	// Lockup anchor points, vh offset from the stage's own centre.
	const MAGICAN_FINAL_Y = -10; /* ~60-80px */
	const PS_FINAL_Y = 4; /* tucked closely under Magican as tagline */
	const MAGICAN_FINAL_SCALE = 1; /* keep original size */
	const PS_START_SCALE = 0.7;
	// Hand-off timing, in p1 (the morph's own 0..1 progress). Both Magican's
	// exit and PS's entrance now start at p1=0 together — reported live as
	// "PS should appear simultaneously with Magican, not after it has
	// settled" — but they do NOT move at the same RATE, and that
	// difference is still what keeps them from colliding (see the
	// screenshot-verified bug this whole file's second pass fixed). PS's
	// growth rides `smoothstep`, whose derivative is ~0 at p1=0 — it barely
	// grows for the first stretch — while Magican's exit rides
	// `easeOutCubic`, front-loaded to move fast immediately. By the time PS
	// has grown enough to matter, Magican is already most of the way clear.
	const MAGICAN_OUT_END = 0.32;
	const PS_IN_START = 0;
	const PS_HOLD_END = 0.62;
	const PS_OUT_START = 0.72;
	const PS_FINAL_SCALE = 0.35;

	// The retired hook's ten roles, re-grammared for "for the ___ in you" —
	// grown to ~100 (2026-08-17, fifth pass), same grammar throughout: a
	// bare noun that reads naturally after "for the" and before "in you".
	// Deliberately NOT roleCycle.ts's ROLES: those read "a professional" /
	// "somebody's kid", grammar built for "You are ___". The original ten
	// stay first, in their original order, so nothing that was already
	// tuned (HOLD_MS's old pacing, any external reference to "the first
	// ten") silently shifts; everything after them is new.

	const PERSONA_COLORS = [
		'#1e3a8a', /* deep blue */
		'#064e3b', /* deep emerald */
		'#4c1d95', /* deep purple */
		'#7f1d1d', /* deep red */
		'#7c2d12', /* rust / orange-brown */
		'#0f172a', /* slate */
		'#831843', /* deep pink/rose */
		'#3f6212', /* olive */
		'#111827', /* very dark grey */
		'#581c87', /* plum */
	];

	const PERSONAS = [
		'professional',
		'parent',
		'partner',
		'kid',
		'music lover',
		'movie buff',
		'sports enthusiast',
		'tinkerer',
		'storyteller',
		'score-checker',
		'founder',
		'freelancer',
		'night owl',
		'early riser',
		'home cook',
		'bookworm',
		'traveler',
		'homebody',
		'gamer',
		'runner',
		'cyclist',
		'hiker',
		'gardener',
		'photographer',
		'collector',
		'crafter',
		'builder',
		'maker',
		'dreamer',
		'planner',
		'overthinker',
		'multitasker',
		'introvert',
		'extrovert',
		'optimist',
		'realist',
		'minimalist',
		'list-maker',
		'note-taker',
		'plant parent',
		'dog person',
		'cat person',
		'coffee snob',
		'tea drinker',
		'wine lover',
		'foodie',
		'baker',
		'student',
		'teacher',
		'researcher',
		'engineer',
		'designer',
		'writer',
		'artist',
		'musician',
		'coach',
		'mentor',
		'volunteer',
		'caregiver',
		'sibling',
		'grandparent',
		'new parent',
		'newlywed',
		'empty nester',
		'investor',
		'saver',
		'side-hustler',
		'business owner',
		'manager',
		'consultant',
		'lawyer',
		'doctor',
		'nurse',
		'therapist',
		'accountant',
		'analyst',
		'developer',
		'marketer',
		'salesperson',
		'recruiter',
		'lifelong learner',
		'history buff',
		'trivia champion',
		'puzzle solver',
		'DIYer',
		'fixer',
		'organizer',
		'declutterer',
		'adventurer',
		'map nerd',
		'podcast listener',
		'playlist curator',
		'insomniac',
		'early bird',
		'weekend warrior',
		'meal-prepper',
		'spreadsheet person',
		'worrier',
		'optimizer',
		'completionist',
		'collector of hobbies',
		// The lap's answer, and the only term that is not a facet. Everything
		// before it accelerates until the words stop being readable
		// individually — which is the point: you stop counting them and start
		// feeling that there are too many to count. Then this one lands and
		// holds, long enough to be read on purpose, before the lap starts
		// over slow. It is the page's own title arriving at the end of its
		// own argument.
		'everyone'
	] as const;

	/** The index of that closing term — the one hold that does not follow
	 *  the acceleration curve. */
	const EVERYONE_INDEX = PERSONAS.length - 1;
	/** Shortest and longest hold, ms — see `holdMsFor` for the curve
	 *  between them. */
	const HOLD_MS_START = 1000;
	const HOLD_MS_END = 160;
	/** One hold per persona, shortening smoothly across the WHOLE lap —
	 *  "slowly, then rapidly" has to span all of PERSONAS now that there
	 *  are ~100 of them, not just ten, or the old fixed ten-value table
	 *  would sawtooth (slow-fast-slow-fast every ten words) instead of
	 *  reading as one continuous acceleration. Quadratic, not linear, so
	 *  the slow opening holds its "read me" pace for longer before the
	 *  back half starts blurring past — then the lap restarts from the
	 *  top, slow again. */
	/** How long "everyone" sits before the lap restarts. Long enough to read
	 *  deliberately and register as an arrival rather than another item in
	 *  the list — the acceleration before it only means something if
	 *  something stops at the end of it. */
	const EVERYONE_HOLD_MS = 2800;

	function holdMsFor(index: number): number {
		// The closing term breaks the curve on purpose: it is the slowest
		// hold of the lap, arriving immediately after the fastest ones.
		if (index === EVERYONE_INDEX) return EVERYONE_HOLD_MS;
		// The curve spans the facets only. Dividing by `PERSONAS.length - 1`
		// would let "everyone" claim the fast end of the ramp and leave the
		// last real facet a step short of it.
		const t = clamp(index / (EVERYONE_INDEX - 1), 0, 1);
		return Math.round(HOLD_MS_END + (HOLD_MS_START - HOLD_MS_END) * (1 - t) ** 2);
	}

	function easeOutCubic(t: number): number {
		const p = clamp(t, 0, 1);
		return 1 - (1 - p) ** 3;
	}
	/** 0 before `edge0`, 1 after `edge1`, eased between. */
	function smoothstep(edge0: number, edge1: number, x: number): number {
		const t = clamp((x - edge0) / (edge1 - edge0), 0, 1);
		return t * t * (3 - 2 * t);
	}

	const scrub = createScrubber([0, 1]);
	const scrubState = scrub.state;

	let trackEl: HTMLElement | null = null;
	let magicanEl: HTMLElement | null = null;
	let psEl: HTMLElement | null = null;
	let personaGroupEl: HTMLElement | null = null;
	let motifEl: SVGSVGElement | null = null;
	let gridEl: HTMLElement | null = null;
	/** Handed down to the road mounted inside this stage. */
	let roadProgress = 0;
	/** 0..1 — the road layer's own opacity, see the title hold in paint(). */
	let roadVeil = 0;



	let personaIndex = 0;
	/** One-way latch — see the header comment. */
	let arrived = false;
	let personaTimer: ReturnType<typeof setTimeout> | null = null;

	function armPersonaCycle(): void {
		if (arrived) return;
		arrived = true;
		scheduleNext();
	}
	function scheduleNext(): void {
		personaTimer = setTimeout(() => {
			personaIndex = (personaIndex + 1) % PERSONAS.length;
			scheduleNext();
		}, holdMsFor(personaIndex));
	}
	function stopPersonaCycle(): void {
		if (personaTimer !== null) {
			clearTimeout(personaTimer);
			personaTimer = null;
		}
	}

	/**
	 * The whole scroll-driven half of the choreography, from one number.
	 * Called every frame the rAF loop is running, and once more directly
	 * (at u=1) under reduced motion to resolve straight to the settled
	 * phrase — no hold-and-reveal to skip, the same convention every other
	 * scrubbed beat on this page follows. Does NOT touch the persona word
	 * itself; that is `armPersonaCycle`'s job once arrived, on its own
	 * clock.
	 */
	function paint(u: number): void {
		// `u` now spans the hero AND the road on one track. Every hero beat
		// below is written in the hero's own 0..1, so remap once here rather
		// than rescaling a dozen constants that already read correctly.
		const hu = clamp(u / ROAD_START, 0, 1);
		const ru = clamp((u - ROAD_START) / (1 - ROAD_START), 0, 1);
		heroPhase.set(hu);
		roadPhase.set(ru);
		roadActive.set(u >= ROAD_START);
		// Whether the section is still on screen at all. `u` clamps to 1 and
		// stays there once the track is behind us, so progress alone cannot
		// answer this — which is why the last promise heading kept labelling
		// acts over every section below.
		if (trackEl) {
			const r = trackEl.getBoundingClientRect();
			overBackdrop.set(r.bottom > window.innerHeight * 0.5);
		}
		// THE TITLE HOLD. The act's card used to be on screen before its
		// heading had arrived, because both ran off the same progress from
		// the act's first pixel — the heading takes a quarter of the act to
		// fly in from centre and settle, and the station was simply there
		// already. So the road is HELD at each act's opening position for the
		// first slice of that act while the heading lands, and only then does
		// the camera start moving. `roadPhase` still publishes the RAW value,
		// so LandingChrome's heading animates during the hold rather than
		// waiting through it with everything else.
		let actIdx = 0;
		while (actIdx < PROMISE_ACTS - 1 && ru >= ACT_EDGES[actIdx + 1]) actIdx++;
		const a0 = ACT_EDGES[actIdx];
		const a1 = ACT_EDGES[actIdx + 1];
		const span = a1 - a0 || 1;
		const local = clamp((ru - a0) / span, 0, 1);
		const afterTitle = local < TITLE_HOLD ? 0 : (local - TITLE_HOLD) / (1 - TITLE_HOLD);
		// Map back into the act's OWN range, not into a third.
		roadProgress = a0 + afterTitle * span;
		actIndex.set(actIdx);
		actLocal.set(local);

		// Holding the CAMERA still is not the same as hiding the card: the
		// act's first station sits at its opening position from the act's
		// first pixel, so freezing progress left it fully visible under a
		// heading that had not arrived yet. The road layer therefore fades in
		// across the BACK half of the title hold — the heading gets the
		// screen to itself, lands, and only then do the stations appear
		// beneath it. It fades again at the next act's title, which makes the
		// act break read as a break rather than a card swap.
		roadVeil = local >= TITLE_HOLD ? 1 : smoothstep(HEAD_SETTLED, TITLE_HOLD, local);

		const p1 = clamp(hu / MORPH_END, 0, 1);

		// Magican: shrink and rise driven off ONE shared progress fraction, so
		// they can never desync — see the header note dated 2026-08-17
		// (fourth pass) for why that matters. The prior version rose on
		// `easeInOutCubic(p1)` (slow to start, spanning the FULL p1 range)
		// while its shrink resolved on a faster, separate curve
		// (`p1 / 0.55`) — at p1=0.4 the wordmark had already shrunk to
		// ~24% scale but had risen only ~26% of the way to its resting
		// spot, so a small-but-still-centred "Magican" sat directly under a
		// "Personal Superintelligence" that had, by that same p1, already
		// grown to full size in the exact same spot: a literal text
		// collision, reported live as "font sizing and spacing" gone wrong
		// mid-scroll. MAGICAN_OUT_END is deliberately short and both scale and
		// Y ride the same eased fraction of it, so the wordmark is fully
		// clear — small AND risen — well before PS is given any room to
		// grow (see PS's own comment below for the margin that buys).
		const magicanT = easeOutCubic(clamp(p1 / MAGICAN_OUT_END, 0, 1));
		const magicanScale = 1 - magicanT * (1 - MAGICAN_FINAL_SCALE);
		const magicanY = magicanT * MAGICAN_FINAL_Y;
		if (magicanEl) {
			magicanEl.style.transform = `translateY(${magicanY.toFixed(2)}vh) scale(${magicanScale.toFixed(4)})`;
		}

		// Personal Superintelligence: starts growing into the vacated centre
		// only once Magican is most of the way clear — PS_IN_START sits well
		// after MAGICAN_OUT_END, not at or before it (at p1=PS_IN_START, Magican's
		// own progress is already ~97%) — holds fully visible, then shrinks
		// into line two once PS_HOLD_END has been reached, not before.
		const inT = smoothstep(PS_IN_START, PS_HOLD_END, p1);
		const outT = smoothstep(PS_OUT_START, 1, p1);
		const grownScale = PS_START_SCALE + inT * (1 - PS_START_SCALE);
		const psScale = grownScale - outT * (grownScale - PS_FINAL_SCALE);
		const psY = outT * PS_FINAL_Y;
		if (psEl) {
			psEl.style.opacity = inT.toFixed(3);
			psEl.style.transform = `translateY(${psY.toFixed(2)}vh) scale(${psScale.toFixed(4)})`;
		}

		// The persona group — "for the" (small, static now), the cycling
		// word, and "in you" (small, static) — arrives as ONE unit, fading
		// in shortly after the lockup above finishes settling (PS's own
		// shrink-to-line-two, PS_OUT_START=0.72 in p1-space, finishes at
		// u≈0.30). "for the" used to be its own separate scroll-driven beat
		// here — big, then continuously shrinking to nothing — reported
		// live as wanting it small and bracketing the cycling word instead,
		// the same treatment "in you" already had, not a beat of its own.
		// It is now a plain static child of `.br-persona-group` (see the
		// markup) with no independent opacity/transform: it simply arrives
		// and leaves with the rest of the group. Neither it nor "in you"
		// gets a scroll-driven transform — the cycling word's own "comes
		// big, settles" motion is TIMED, one pop per word change (see the
		// markup's `in:scale`/`out:scale`), and the two captions never move
		// or resize at all — see the header comment for why only the word
		// cycling counts as "the switching/morphing animation".
		const personaGroupOpacity = smoothstep(0.34, 0.46, hu);

		// THE HAND-OFF. Reported live: the stations should appear while we
		// are still in this section, before scrolling out of it. They did
		// not — this stage stayed fully painted until its track ended, then
		// slid up and away and only THEN did the road pin, so a visitor
		// watched the hero leave before anything arrived to replace it.
		//
		// Nothing here moves to fix that; it fades in place instead. Over
		// the last stretch of the track the lockup and the persona group
		// dissolve where they stand, so by the time this stage actually
		// unpins there is nothing left on it to scroll away — the shared
		// backdrop (LandingField) carries straight through, LandingChrome's
		// corners have just formed out of this same lockup, and the road
		// pins onto a picture that never went anywhere. The seam is still
		// there in layout; there is simply nothing visible crossing it.
		const exitT = smoothstep(EXIT_START, 1, hu);
		const exit = 1; /* keep solid, let them scroll away naturally */
		if (gridEl) gridEl.style.opacity = exit.toFixed(3);
		if (personaGroupEl) personaGroupEl.style.opacity = (personaGroupOpacity * exit).toFixed(3);

		// The motif thread: fades up alongside the phrase and then just
		// holds — it is the handoff into whatever the page does next, not
		// something that itself resolves. Shifted earlier with the rest of
		// the phrase beat.
		const motifOpacity = smoothstep(0.34, 0.5, hu);
		if (motifEl) motifEl.style.opacity = motifOpacity.toFixed(3);

		if (hu >= ARRIVE) armPersonaCycle();
	}

	let u = 0;
	$: u = $scrubState.local;

	const rt: { raf: number; running: boolean } = { raf: 0, running: false };
	let mounted = false;
	let visible = false;

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
		// Re-arm on a live reduced-motion toggle, and resolve straight to
		// the settled phrase (persona cycling included) the instant motion
		// is turned off.
		if (mounted && reduced) {
			paint(1);
			stopPersonaCycle();
		}
		syncLoop();
	}

	function frame(): void {
		if (!rt.running) return;
		rt.raf = requestAnimationFrame(frame);
		paint(u);
	}

	let io: IntersectionObserver | null = null;
	let onResize: (() => void) | null = null;

	onMount(() => {
		mounted = true;
		if (reduced) paint(1);
		onResize = () => {
			paint(reduced ? 1 : u);
		};
		window.addEventListener('resize', onResize, { passive: true });
		if (trackEl) {
			io = new IntersectionObserver(
				(entries) => {
					visible = entries[0]?.isIntersecting ?? false;
					syncLoop();
				},
				{ threshold: 0, rootMargin: '0px 0px -1px 0px' }
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
		stopPersonaCycle();
	});
</script>

<div
	id="brand-reveal-track"
	class="br-track"
	class:static={reduced}
	style={reduced ? '' : `height:${TRACK_VH}svh`}
	use:scrub.track
	bind:this={trackEl}
>
	<div class="br-stage" class:static={reduced}>
		<!-- The dusk-town backdrop is NOT here any more: it lives on
		     `LandingField`, one level out, so one picture spans this hero
		     AND the promises road instead of stopping at this track's
		     edge. This stage is transparent over it. -->
		<div class="br-grid" bind:this={gridEl}>
			<h1 class="br-word br-magican" bind:this={magicanEl}>Magican</h1>
			<p class="br-word br-ps" bind:this={psEl}>Personal Superintelligence</p>
		</div>

		<!-- The hand-off phrase: one anchor point below the settled lockup,
		     one arrival (see paint()'s own comment). "for the" and "in you"
		     bracket the cycling word — both small, static captions that
		     never move or resize, one above the word and one below, the
		     literal shape of the sentence "for the ___ in you" made
		     visible. Reported live: "for the" used to be its own separate
		     scroll beat, big then vanishing, before this; asked to become
		     small and sit above the word instead, matching "in you"'s
		     existing treatment rather than having a beat of its own. -->
		<div class="br-phrase br-persona-group" class:on={reduced} bind:this={personaGroupEl}>
			<p class="br-for-the">for the</p>
			<!-- The outgoing and incoming word coexist in the DOM for the
			     length of the crossfade (Svelte runs in: and out: at the same
			     time) — without a shared grid cell they're two separate
			     normal-flow items and visibly stack/overlap mid-swap, exactly
			     the bug the Magican/PS lockup above already solved once with
			     `grid-area: 1 / 1`. Same fix, same reason, here. -->
			<div class="br-persona-slot">
				{#if reduced}
					<p class="br-persona-word" style="color: {PERSONA_COLORS[0]}">{PERSONAS[0]}</p>
				{:else}
					{#key personaIndex}
						<p
							class="br-persona-word"
							in:scale={{ duration: 460, start: 1.4, opacity: 0, easing: cubicOut }}
							out:scale={{ duration: 240, start: 1, opacity: 0, easing: cubicIn }}
						 style="color: {PERSONA_COLORS[personaIndex % PERSONA_COLORS.length]}">
							{PERSONAS[personaIndex]}
						</p>
					{/key}
				{/if}
			</div>
			<p class="br-in-you">in you</p>
		</div>

		<!-- The motif thread, lifted from the retired LandingHero: one loose
		     departure that the rest of the page picks up. -->
		<svg
			class="br-motif"
			class:on={reduced}
			bind:this={motifEl}
			viewBox="-500 0 1000 700"
			preserveAspectRatio="none"
			aria-hidden="true"
		>
			<defs>
				<linearGradient id="br-motif-paint" x1="0" y1="0" x2="0.58" y2="1">
					<stop offset="0" stop-color="var(--br-a)" />
					<stop offset="0.58" stop-color="var(--br-b)" />
					<stop offset="1" stop-color="var(--br-a)" stop-opacity="0.2" />
				</linearGradient>
			</defs>
			<path
				class="br-motif-line"
				pathLength="1"
				d="M 0 0 C 44 72 164 102 142 184 C 116 282 -132 304 -104 424 C -76 542 94 574 74 700"
			/>
			<path
				class="br-motif-glint"
				pathLength="1"
				d="M 0 0 C 44 72 164 102 142 184 C 116 282 -132 304 -104 424 C -76 542 94 574 74 700"
			/>
		</svg>

		<!-- THE ROAD, inside this stage rather than in a section after it.
		     `progress` hands it the back share of this track's own scrub, so it
		     never owns scroll, never pins again, and never becomes a second
		     place to scroll into — which is the whole point. It sits above the
		     backdrop and below nothing: the hero's own words have already faded
		     out by the time this has anything to show. -->
		<!-- <div class="br-road" style="opacity:{roadVeil}" class:on={roadVeil > 0.01}>
			<ProofSwitcher progress={roadProgress} />
		</div> -->
	</div>
</div>

<style>
	.br-track {
		/* Reported live: with the inner mask ellipse gone (see `.br-backdrop`
		   above), Magican/PS/the persona word now sit directly on the dusk
		   image instead of a cream cutout, and the page's usual ink-blue →
		   teal accent pair was called out specifically — the teal end reads
		   as green, and green is the one hue that never appears anywhere in
		   this artwork's palette (navy, purple, orange, amber lamplight).
		   It read as a foreign sticker rather than type sitting IN the
		   scene. Swapped to a pair pulled FROM the image's own palette
		   instead: a warm lamplight gold through to a bright warm ivory —
		   the same hue family as every lit window in the picture. Gold
		   carries strong contrast against the navy/purple upper sky where
		   the wordmark actually sits; ivory (brighter than the page's own
		   cream) carries it the rest of the way so the brightest stop still
		   reads clearly even where the sky warms toward the horizon. Scoped
		   here, not touched at the `--accent-primary`/`--accent-secondary`
		   level — this is this hero's own image-matched treatment, not a
		   sitewide accent change. */
		--br-a: #888888;
		--br-b: #ffffff;
		/* `.br-magican`'s shimmer (below) slides a third, darker stop through
		   the middle of this gold/ivory pair for the sense of movement.
		   That stop used to be `--text-primary` (`#1a1612`, near-black) —
		   fine against the old ink-blue/teal pair, but blending near-black
		   with gold/ivory lands on a desaturated warm-brown midpoint that,
		   seen live against this image's blue-purple sky, read as a muddy
		   olive smear crossing whichever letter it was under (simultaneous
		   contrast — the same illusion noted on `.br-backdrop`'s own inner
		   ellipse earlier, a blue-adjacent surround pushing a warm neutral
		   toward green). A copper/amber mid-stop instead keeps the whole
		   shimmer in one hue family — it still dips in lightness for the
		   same sense of movement, but never desaturates into the zone
		   vulnerable to that illusion. */
		--br-mid: #111111;
		/* `.br-ps` ("Personal Superintelligence") asks for
		   `var(--landing-title-gradient, <local fallback>)` — but
		   `--landing-title-gradient` is a PAGE-LEVEL token (`+page.svelte`),
		   set there for every other landing section that still uses it
		   (PathFork, WhatItTakes, Declaration, RoleCycle, the old
		   LandingHero), and a `var()` with a fallback still uses the
		   REAL value when one is inherited, fallback or not. That page
		   token is ~76% flat `--text-primary` (`#1a1612` near-black,
		   0-38% and 62-100%) with only a sliver of colour at the very
		   centre — which is exactly why PS was reported live as "current
		   black and not working well" regardless of what `--br-a`/`--br-b`
		   were set to above: PS was never touching those tokens, it was
		   inheriting the page's mostly-black gradient the whole time.
		   Overridden here, scoped to `.br-track`'s own subtree only — the
		   other landing sections keep the page-level token untouched, this
		   hero gets its own copper-to-gold-to-ivory version instead of
		   near-black (copper/amber stands in for the "brown" suggested
		   live, and reads as one family with `--br-mid` above and
		   `--br-a`/`--br-b` below rather than a fourth, unrelated colour). */
		--landing-title-gradient: linear-gradient(104deg, var(--br-mid) 12%, var(--br-a) 62%, var(--br-b));
		position: relative;
	}
	.br-track.static {
		height: auto;
	}
	.br-stage {
		position: sticky;
		top: 0;
		height: 100svh;
		overflow: hidden;
		display: grid;
		place-items: center;
		/* No background — the shared picture on `LandingField` shows
		   through. Cream here was right while the backdrop was a child of
		   this stage, and occludes it the moment it moved out. */
	}
	/* PROTOTYPE backdrop — see the markup's own comment for the full history.
	   `.br-stage` is already `position: sticky` (a valid containing block,
	   same as `relative`) and already `overflow: hidden`, so the wrap needs
	   nothing extra to stay clipped to the stage. z-index: -1, not 0 —
	   `.br-grid` and the phrase content are NOT themselves positioned, and
	   CSS paints non-positioned in-flow content before a z-index:0 (or
	   auto) positioned sibling regardless of DOM order, which would put the
	   backdrop ON TOP of the text. A negative z-index paints unambiguously
	   first, before any non-positioned content, so it stays behind
	   everything without requiring every sibling to also be repositioned. */
	.br-stage.static {
		position: static;
		height: auto;
		padding: clamp(4rem, 12vh, 7rem) clamp(1.2rem, 5vw, 3rem);
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.8rem;
	}

	/* Both lockup words occupy the SAME cell while scrubbed — the RoleCycle
	   stacking trick — so they share one centre point and only ever move
	   apart by transform, never by independent layout. Under reduced
	   motion there is no morph to stage, so the grid collapses to a plain
	   stacked block instead: `grid-area: 1 / 1` on both words would
	   otherwise print them on top of each other. */
	.br-grid {
		display: grid;
		grid-template: 1fr / 1fr;
		place-items: center;
		width: min(94vw, 68rem);
		text-align: center;
	}
	.br-stage.static .br-grid {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.6rem;
	}
	.br-word {
		grid-area: 1 / 1;
		margin: 0;
		font-family: var(--lp-font, 'Outfit', ui-sans-serif, system-ui, sans-serif);
		will-change: transform;
	}
	.br-stage.static .br-word {
		will-change: auto;
	}
	.br-stage:not(.static) .br-word {
		position: relative;
	}

	.br-magican {
		/* Repainted twice, same day (2026-08-17). First pass: was weight 780
		   at this same size — a near-black slab — dropped to 400 on
		   Bricolage Grotesque, whose 200-800 variable axis carries a
		   wordmark this large just fine at a genuinely light weight. Second
		   pass: Bricolage itself was reported "not good enough"; Outfit
		   replaced it after a live six-way comparison at /font-preview.
		   Outfit's own regular (400) already reads visibly lighter than
		   Bricolage's did at the same number — its 100-900 axis was tuned
		   toward thin from the start — so 400 here would now read heavier
		   than intended; dropped to 300 to land back at the same "confident
		   but airy" impression the comparison page validated. */
		font-size: clamp(4.4rem, 17vw, 13rem);
		font-weight: 300;
		letter-spacing: -0.03em;
		/* Not 1 — see .br-persona-word's own comment for why a gradient-
		   clipped line this tight culls descenders (reported live as "the
		   'g' is getting culled"). "Magican" itself has none today, but this
		   stays consistent with the other two gradient-clipped lines rather
		   than being a silent trap for whatever text lands here next. */
		line-height: 1.15;
		white-space: nowrap;
		/* SLEEK BREATHE: Solid stark dark, with a slow pulsing blur/scale effect */
		color: #111111;
		animation: br-breathe 8s ease-in-out infinite;
	}
	.br-ps {
		font-size: clamp(2rem, 6.6vw, 4.9rem);
		font-weight: 320;
		letter-spacing: -0.015em;
		/* 1.08 was still tight enough to clip "Superintelligence"'s own p/g
		   descenders against the gradient-clip box — see .br-persona-word's
		   comment for the mechanism. */
		line-height: 1.15;
		max-width: 90vw;
		color: #000000;
		opacity: 0;
	}
	.br-stage.static .br-ps {
		opacity: 1;
		margin-top: 0.5rem;
	}

	/* The hand-off phrase: "for the" and the persona group are a RELAY, not
	   a pair — both pinned to the SAME point below the lockup (`.br-stage`
	   is itself a positioning context, sticky still counts, so
	   `left/top/translate` here anchors to the STAGE, not to `.br-grid`'s
	   cell), one taking over from the other rather than sitting side by
	   side, so the hand-off never shifts layout. */
	.br-phrase {
		margin: 0;
		font-family: var(--lp-font, 'Outfit', ui-sans-serif, system-ui, sans-serif);
		font-weight: 380;
		text-align: center;
		opacity: 0;
		will-change: opacity;
	}
	.br-stage.static .br-phrase {
		position: static;
		transform: none;
		will-change: auto;
	}
	.br-stage:not(.static) .br-phrase {
		position: absolute;
		left: 50%;
		top: 50%;
		width: max-content;
		max-width: 90vw;
		/* The phrase used to anchor at the EXACT point Magican/PS both emerge
		   from — plain centring, no extra offset. Reported live: "'for the'
		   can come down a bit more" — by the time this group is visible,
		   PS has already docked small near that same centre point, and
		   "for the" (the group's own top edge) was sitting right up against
		   it. A flat downward nudge on top of the centring, not a
		   percentage (percentages in `translate()` resolve against the
		   element's OWN box, not the stage, so they'd fight its height
		   changing as the persona word cycles between short and long
		   terms). */
		transform: translate(-50%, calc(-50% + 18vh));
	}
	/* Both captions bracket the cycling word, above and below — see the
	   markup's own comment. Plain flex children now, not their own
	   `.br-phrase` beat: no absolute positioning, no independent
	   opacity/transform, they just arrive and sit still with the rest of
	   `.br-persona-group`. */
	.br-persona-group {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0rem;
	}
	.br-persona-group.on {
		opacity: 1;
	}
	/* The outgoing and incoming persona word share this one grid cell — the
	   same `grid-area: 1 / 1` stacking trick `.br-grid` uses for Magican/PS —
	   so a crossfade overlaps them at the exact same spot instead of the
	   two coexisting normal-flow elements Svelte's `in:`/`out:` briefly
	   leaves in the DOM together stacking or shoving "in you" around. */
	.br-persona-slot {
		display: grid;
		grid-template: 1fr / 1fr;
		place-items: center;
		width: 100%;
	}
	.br-persona-word {
		grid-area: 1 / 1;
		margin: 0;
		font-size: clamp(2.6rem, 7.6vw, 5.2rem);
		font-weight: 400;
		letter-spacing: -0.025em;
		/* Reported live: "the g is getting culled" — many of the ~100
		   PERSONAS terms have descenders ("gamer", "designer", "playlist
		   curator") that this line's own gradient-clip was cutting off.
		   `line-height: 1` sets the line box to (very close to) the font's
		   own em-square, and Outfit's descender extends past it; the
		   background-clip: text gradient only paints within that box, so
		   anything below it — the tail of a g, a y, a p — never gets
		   painted and reads as missing rather than merely tight. 1.15 gives
		   the box enough room below the baseline to cover the full glyph. */
		line-height: 1.15;

	}
	/* "for the" — very small on purpose, reported live: a label above the
	   word it introduces, not a headline of its own the way it used to be.
	   Same casing and register as "in you" below — a matched bracket pair,
	   not a caption plus a shout. */
	.br-for-the {
		margin: 0 0 -1.6rem 0;
		font-size: clamp(0.7rem, 1.4vw, 0.85rem);
		letter-spacing: 0.02em;
		color: #666666;
	}
	/* Quiet on purpose — this is the piece of the phrase that never cycles,
	   never resizes and never vanishes; it just waits underneath whichever
	   word is currently having its moment. Pulled smaller again, reported
	   live alongside "for the" — was clamp(0.92rem, 2vw, 1.15rem). */
	.br-in-you {
		margin: 0;
		font-size: clamp(0.78rem, 1.6vw, 0.95rem);
		letter-spacing: 0.02em;
		color: #666666;
	}

	/* The motif: one loose departure, picking up the phrase line and
	   carrying the eye into the rest of the page. */
	.br-motif {
		position: absolute;
		z-index: -1;
		left: 50%;
		top: 72%;
		width: 100vw;
		height: max(55svh, 25rem);
		transform: translateX(-50%);
		overflow: visible;
		pointer-events: none;
		opacity: 0;
	}
	.br-motif.on {
		opacity: 0.8;
	}
	.br-stage.static .br-motif {
		display: none;
	}
	.br-motif-line,
	.br-motif-glint {
		fill: none;
		stroke: url(#br-motif-paint);
		stroke-linecap: round;
		vector-effect: non-scaling-stroke;
	}
	.br-motif-line {
		stroke-width: 2.4;
		opacity: 0.8;
		filter: drop-shadow(0 0 9px color-mix(in srgb, var(--br-a) 62%, transparent));
	}
	.br-motif-glint {
		stroke-width: 3.2;
		stroke-dasharray: 0.11 0.89;
		stroke-dashoffset: 0;
		opacity: 0.92;
		filter: drop-shadow(0 0 13px var(--br-b));
		animation: br-motif-travel 7s linear infinite;
	}

	@keyframes br-shimmer {
		0%,
		100% {
			background-position: 0% 50%;
		}
		50% {
			background-position: 100% 50%;
		}
	}
	@keyframes br-breathe {
		0%, 100% { opacity: 0.95; filter: drop-shadow(0 0 0 rgba(0,0,0,0)); }
		50% { opacity: 0.6; filter: drop-shadow(0 8px 16px rgba(0,0,0,0.15)); }
	}
	@keyframes br-motif-travel {
		to {
			stroke-dashoffset: -1;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.br-magican {
			animation: none;
			transform: none;
		}
		.br-motif-glint {
			animation: none;
		}
	}
</style>
