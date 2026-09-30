<script lang="ts">
	// The greeting — the page's emotional payoff after the product proof.
	//
	// A compact, theme-native transition: "Love what you do." holds. Then
	// the aurora thread crosses the frame sprinkling light, and as it passes
	// the first and last words TRADE PLACES, resolving to "Do what you
	// love." The thread carries on and leaves through bottom-centre. This is
	// an earned payoff inside the page, not a second full-screen introduction.
	//
	// WHY THE SWAP IS THE ARGUMENT: both phrases use the SAME FOUR WORDS.
	// "Love what you do" is what you tell someone who cannot change their
	// situation. "Do what you love" is what someone says who can. Magican is
	// literally the thing that reorders them. Nothing is added — no fifth
	// word, no subtitle, no explanation. Any version that introduces one
	// loses the point, so this component deliberately has no room for copy
	// beyond the four words. The root page now names the
	// proposition before its film; this beat is what that capability gives back.
	//
	// SCROLL-DRIVEN, like everything else on this page. It was time-based
	// for its first cut, on the argument that the greeting happens before
	// any scroll exists and that scrubbing it would ask the visitor to
	// perform the swap themselves — when the swap is the thing being done
	// FOR them. Owner call, 2026-08-06: reverted. Two things were wrong with
	// the argument. A timed open cannot be re-read, re-entered or slowed
	// down, so anyone who blinked has missed the only statement of the
	// page's thesis and has no way back to it. And a page whose first beat
	// runs on a clock and whose every later beat runs on scroll teaches the
	// visitor the wrong control in the first three seconds.
	//
	// So the pass and swap are both scrubbed off one progress
	// value. The visitor is not performing the swap — they are advancing
	// the film, and the swap happens to them at the point in it where it
	// belongs, at whatever pace they read.
	//
	// It is still not a station: the film's stations are positions in a
	// scrubbed world with weights and a camera. This is an ordinary compact
	// section whose viewport passage supplies progress without pinning it.
	import { onDestroy, onMount } from 'svelte';
	import { motionEnabled } from '$lib/motion';
	import { mixStops, rgba, type AuroraStop } from './auroraPalette';
	import {
		parseThemeColor,
		themeThreadPalette,
		type ThemeThreadPalette
	} from './themeThreadPalette';
	import {
		greetingMotifPoint,
		motifBodyAlpha,
		motifHeadRadius,
		motifStrokeRecipe,
		motifTrailLengthPx,
		motifWaveOffset,
		sampleMotifTrail
	} from './motifFlow';
	import { createScrubber } from './scrub';

	$: reduced = !$motionEnabled;

	// ── The beat, mapped onto the track's own progress ───────────────────
	//
	// One place, so the pass and swap can be re-paced together
	// rather than hunted for. All values are fractions of THIS section's
	// scroll, not of the page's.
	/** The line holds alone before anything moves. */
	const G_PASS_IN = 0;
	/** …and the thread has left through bottom-centre by here. */
	const G_PASS_OUT = 0.78;
	/**
	 * The swap starts when the thread REACHES the words, not when the hold
	 * ends: it has to look caused. The pass crosses the phrase between
	 * u ≈ 0.35 and u ≈ 0.75 of its own length, which is this window.
	 * Any re-pacing of the pass has to move these with it.
	 */
	const G_SWAP = 0.34;
	const G_SWAPPED = 0.61;
	const scrub = createScrubber([0, 1], { geometry: 'viewport' });
	const gstate = scrub.state;
	$: gp = reduced ? 1 : $gstate.p;

	const seg = (v: number, a: number, b: number): number =>
		b <= a ? (v >= b ? 1 : 0) : Math.min(1, Math.max(0, (v - a) / (b - a)));

	/** How far the thread is along its pass. */
	$: u = seg(gp, G_PASS_IN, G_PASS_OUT);
	/** How far the two words are through their flight. */
	$: sw = seg(gp, G_SWAP, G_SWAPPED);
	// The phase still exists, but only to pick which of the three cell
	// widths applies; the motion itself is scrubbed off `sw`.
	$: phase = sw <= 0 ? 'in' : sw >= 1 ? 'done' : 'swapping';

	let trackEl: HTMLElement | null = null;
	let wrapEl: HTMLElement | null = null;
	let slotAEl: HTMLElement | null = null;
	let slotBEl: HTMLElement | null = null;
	let canvasEl: HTMLCanvasElement | null = null;

	/**
	 * Every number the swap needs, measured rather than guessed — the words
	 * are different lengths at every viewport and every font size.
	 *
	 * THE CELL WIDTHS ARE THE FIDDLY PART, and getting them wrong is
	 * visible: a cell has to hold two words of different widths, so whichever
	 * one it is sized for, the other sits in it with slack. Sized for the
	 * wider word throughout, the finished line reads "Do⎵⎵what you love."
	 * — the complaint that produced this pass.
	 *
	 * So a cell is TWO widths. It holds the wider of its pair for the whole
	 * flight, which is what keeps the flight's geometry exact (see the
	 * markup), and then SETTLES to the width of the word that actually
	 * landed in it. The settle is a real width transition and it re-centres
	 * the line, so the sentence visibly tightens into its final spacing a
	 * beat after the words arrive — which reads as the phrase composing
	 * itself rather than as a layout correction.
	 */
	function measure(): void {
		if (!wrapEl || !slotAEl || !slotBEl) return;
		const holds: string[] = [];
		for (const slot of [slotAEl, slotBEl]) {
			const go = slot.querySelector<HTMLElement>('.gr-go');
			const come = slot.querySelector<HTMLElement>('.gr-come');
			if (!go || !come) continue;
			const wGo = go.getBoundingClientRect().width;
			const wCome = come.getBoundingClientRect().width;
			if (wGo === 0 || wCome === 0) continue;
			const hold = `${Math.max(wGo, wCome).toFixed(1)}px`;
			holds.push(hold);
			slot.style.setProperty('--w-go', `${wGo.toFixed(1)}px`);
			slot.style.setProperty('--w-hold', hold);
			slot.style.setProperty('--w-end', `${wCome.toFixed(1)}px`);
		}
		// --travel is measured with both cells FORCED to their flight width,
		// because that is the only state the flight is ever in. Measured at
		// the resting widths instead it comes out short by half the slack
		// the cells are about to open, and the two arcs stop landing on each
		// other. One forced layout, at mount and on resize.
		if (holds.length === 2) {
			slotAEl.style.width = holds[0];
			slotBEl.style.width = holds[1];
		}
		const a = slotAEl.getBoundingClientRect();
		const b = slotBEl.getBoundingClientRect();
		slotAEl.style.width = '';
		slotBEl.style.width = '';
		if (a.width === 0 || b.width === 0) return;
		const d = b.left + b.width / 2 - (a.left + a.width / 2);
		wrapEl.style.setProperty('--travel', `${d.toFixed(1)}px`);
		// The arc's height scales with the throw, so a phone's short trip
		// does not get a desktop's tall lob.
		wrapEl.style.setProperty('--arc', `${Math.min(46, Math.max(16, d * 0.09)).toFixed(1)}px`);
	}

	// ── The thread's pass ────────────────────────────────────────────────
	//
	// The film's protagonist, drawn with the film's own stroke recipe (four
	// passes, luminance-raised theme accents, a bloomed head) so the visitor
	// meets the same object here that carries the rest of the page. It is a
	// separate canvas from MovieTrack's because it is a separate section
	// with its own track; sharing one would mean threading a second
	// progress value through the film's rAF for the sake of one beat.
	const rt: {
		ctx: CanvasRenderingContext2D | null;
		w: number;
		h: number;
		vh: number;
		t: number;
		raf: number;
		running: boolean;
		visible: boolean;
		theme: ThemeThreadPalette;
		foreground: AuroraStop;
		onLight: boolean;
	} = {
		ctx: null,
		w: 0,
		h: 0,
		vh: 0,
		t: 0,
		raf: 0,
		running: false,
		visible: false,
		theme: themeThreadPalette('', ''),
		foreground: { r: 1, g: 1, b: 1 },
		onLight: true
	};

	function fit(): void {
		if (!canvasEl || !wrapEl) return;
		const r = canvasEl.getBoundingClientRect();
		if (r.width === 0) return;
		const dpr = Math.min(2, window.devicePixelRatio || 1);
		rt.w = r.width;
		rt.h = r.height;
		rt.vh = window.innerHeight;
		canvasEl.width = Math.round(r.width * dpr);
		canvasEl.height = Math.round(r.height * dpr);
		rt.ctx = canvasEl.getContext('2d');
		rt.ctx?.setTransform(dpr, 0, 0, dpr, 0, 0);
		const style = getComputedStyle(wrapEl);
		rt.theme = themeThreadPalette(
			style.getPropertyValue('--accent-primary'),
			style.getPropertyValue('--accent-secondary')
		);
		rt.foreground = parseThemeColor(
			style.getPropertyValue('--text-primary'),
			rt.theme.light
		);
		rt.onLight =
			0.2126 * rt.foreground.r + 0.7152 * rt.foreground.g + 0.0722 * rt.foreground.b < 0.5;
	}

	/**
	 * The path: in through top-centre from MovieTrack's bottom-centre handoff,
	 * a shallow rise across the words, then round and out through BOTTOM-CENTRE. The
	 * exit point is load-bearing — it is where the page wants the eye to go
	 * next, so the thread leaves by the door it is pointing at.
	 *
	 * ONE CUBIC BÉZIER, not two pieces. It used to be a sine across and a
	 * separate eased turn down, joined at u = 0.72. The join matched in
	 * POSITION but not in tangent, so the head visibly kinked at the corner —
	 * a snake with an elbow. A single cubic is smooth in its first and
	 * second derivative by construction, so there is no corner to smooth
	 * out; the curvature just rolls through the turn.
	 */
	function pathAt(u: number): { x: number; y: number } {
		return greetingMotifPoint(u, rt.w, rt.h, rt.vh);
	}

	/**
	 * Paint the pass at `u`. Called from a reactive statement rather than
	 * from a rAF loop of its own: the scrubber already coalesces scroll
	 * through one frame, so a second loop would repaint an unchanged canvas
	 * sixty times a second for the whole time this section is on screen.
	 */
	function draw(u: number): void {
		if (!rt.ctx) return;
		const ctx = rt.ctx;
		ctx.clearRect(0, 0, rt.w, rt.h);
		if (u <= 0) return;

		// Match MovieTrack by physical screen length. Timeline-length tails made
		// this section restart as a different, much thicker comet at the seam.
		const targetLength = motifTrailLengthPx(rt.w);
		const trail = sampleMotifTrail(0, u, targetLength, pathAt);
		const K = trail.length - 1;
		if (K < 1) return;
		const pts = trail.map(({ x, y }) => ({ x, y }));
		for (let i = 0; i <= K; i++) {
			const g = i / K;
			const before = pts[Math.max(0, i - 1)];
			const after = pts[Math.min(K, i + 1)];
			const dx = after.x - before.x;
			const dy = after.y - before.y;
			const length = Math.hypot(dx, dy) || 1;
			const offset = motifWaveOffset(g, rt.t, 1);
			pts[i].x += (-dy / length) * offset;
			pts[i].y += (dx / length) * offset;
		}
		const halo = rt.theme.primary;
		const glowHalo = rt.theme.glowPrimary;
		const glowCompanion = rt.theme.glowSecondary;
		// The thread's hot core follows the theme's foreground instead of
		// assuming every theme sits on a black stage.
		const core = rt.onLight ? rt.theme.ink : rt.theme.light;
		const pass = (base: typeof halo, width: number, alpha: number, tint: number): void => {
			const col = mixStops(base, core, tint);
			for (let i = 1; i <= K; i++) {
				const g = i / K;
				const a = motifBodyAlpha(g) * alpha;
				if (a < 0.01) continue;
				ctx.beginPath();
				ctx.moveTo(pts[i - 1].x, pts[i - 1].y);
				ctx.lineTo(pts[i].x, pts[i].y);
				ctx.strokeStyle = rgba(col, a);
				ctx.lineWidth = width;
				ctx.lineCap = 'butt';
				ctx.stroke();
			}
		};
		const strokes = motifStrokeRecipe(rt.onLight, 1);
		pass(glowCompanion, strokes.outer.width, strokes.outer.alpha, strokes.outer.tint);
		pass(glowHalo, strokes.glow.width, strokes.glow.alpha, strokes.glow.tint);
		pass(halo, strokes.filament.width, strokes.filament.alpha, strokes.filament.tint);
		pass(glowHalo, strokes.core.width, strokes.core.alpha, strokes.core.tint);

		// The sprinkle: light shed along the pass, brightest right behind
		// the head. This is the "sprinkling light" the beat asks for, and it
		// is what makes the words look changed BY the thread rather than at
		// the same time as it.
		for (let i = 0; i < 3; i++) {
			const phase = (rt.t * 0.105 + i / 3) % 1;
			const at = Math.min(K, Math.max(0, Math.round((0.12 + phase * 0.76) * K)));
			const point = pts[at];
			ctx.fillStyle = rgba(glowCompanion, Math.sin(Math.PI * phase) * 0.5);
			ctx.beginPath();
			ctx.arc(point.x, point.y, 1.4, 0, Math.PI * 2);
			ctx.fill();
		}

		const head = pts[K];
		const hr = motifHeadRadius(1);
		const bloom = ctx.createRadialGradient(head.x, head.y, 0, head.x, head.y, hr);
		bloom.addColorStop(0, rgba(glowHalo, 0.9));
		bloom.addColorStop(0.4, rgba(glowHalo, 0.26));
		bloom.addColorStop(0.76, rgba(glowCompanion, 0.06));
		bloom.addColorStop(1, rgba(halo, 0));
		ctx.fillStyle = bloom;
		ctx.beginPath();
		ctx.arc(head.x, head.y, hr, 0, Math.PI * 2);
		ctx.fill();
		ctx.fillStyle = rgba(core, 0.96);
		ctx.beginPath();
		ctx.arc(head.x, head.y, 1.25, 0, Math.PI * 2);
		ctx.fill();

		// NOTE: no self-termination. The timed cut stopped its own rAF at
		// u = 1 and cleared the canvas for good, which is right for a
		// one-shot and wrong for a scrub — scrolling back up has to bring
		// the thread back with it.
	}

	let onResize: (() => void) | null = null;
	let themeObserver: MutationObserver | null = null;
	let observer: IntersectionObserver | null = null;

	function syncAnimation(): void {
		const shouldRun = rt.visible && !reduced;
		if (shouldRun && !rt.running) {
			rt.running = true;
			rt.raf = requestAnimationFrame(animate);
		} else if (!shouldRun && rt.running) {
			rt.running = false;
			if (rt.raf) cancelAnimationFrame(rt.raf);
			rt.raf = 0;
		}
	}

	function animate(now: number): void {
		if (!rt.running) return;
		rt.raf = requestAnimationFrame(animate);
		rt.t = now / 1000;
		draw(u);
	}

	// A live reduced-motion preference change must stop the ambient repaint
	// immediately; restoring motion resumes only while this beat is visible.
	$: if (rt.ctx) {
		reduced;
		syncAnimation();
	}

	onMount(() => {
		measure();
		document.fonts?.ready.then(() => measure());
		onResize = () => {
			measure();
			fit();
			draw(u);
		};
		window.addEventListener('resize', onResize, { passive: true });
		if (reduced) return;
		fit();
		draw(u);
		if (trackEl) {
			observer = new IntersectionObserver((entries) => {
				rt.visible = entries[0]?.isIntersecting ?? false;
				syncAnimation();
			});
			observer.observe(trackEl);
		}
		themeObserver = new MutationObserver(() => {
			fit();
			draw(u);
		});
		themeObserver.observe(document.documentElement, {
			attributes: true,
			attributeFilter: ['data-theme']
		});
	});

	// The one line that makes the pass scroll-driven: progress changes, the
	// canvas repaints. Guarded on `mounted` so it cannot fire against a
	// canvas that has no context yet.
	$: if (rt.ctx && !reduced) draw(u);

	onDestroy(() => {
		rt.running = false;
		if (rt.raf) cancelAnimationFrame(rt.raf);
		observer?.disconnect();
		if (typeof window !== 'undefined' && onResize) window.removeEventListener('resize', onResize);
		themeObserver?.disconnect();
	});
</script>

<!-- The words are ONE sentence in the accessibility tree, and it is the
     resolved one: a screen reader must not be handed a phrase that is about
     to be contradicted, and it has no way to perceive a swap. Everything
     that moves is aria-hidden; the sentence beneath it is the truth. -->
<!-- THE TRACK. Unlike a station, this track is only the section's natural
     height. Its progress comes from entering and leaving the viewport, so
     the swap remains reversible without occupying an extra screen. -->
<div
	class="gr-track"
	class:gr-still={reduced}
	use:scrub.track
	bind:this={trackEl}
	style={reduced ? '' : `--sw:${sw.toFixed(4)}`}
>
<!-- THE SCRUBBED VARS LIVE ON THE TRACK, NOT ON THIS SECTION, and that is
     load-bearing. `measure()` writes --travel and --arc onto this element as
     inline custom properties; Svelte owns the `style` ATTRIBUTE of whatever
     it renders a style directive on, and rewrites it wholesale every time the
     value changes. Put both on one element and every scroll frame wipes the
     measurements — which is precisely what happened: --travel fell back to
     its 240px default and the two words flew a third of the distance between
     their cells, landing nowhere near each other. The track is Svelte's, this
     section is JS's, and both inherit down to the words. -->
<section class="gr" class:gr-still={reduced} aria-label="Do what you love" bind:this={wrapEl}>
	<h2 class="gr-sr">Do what you love.</h2>

	{#if reduced}
		<!-- No hold, no swap, no thread: the resolved line, stated once. -->
		<p class="gr-line" aria-hidden="true">Do what you love.</p>
	{:else}
		<canvas class="gr-canvas" bind:this={canvasEl} aria-hidden="true"></canvas>
		<p class="gr-line gr-live" data-phase={phase} aria-hidden="true">
			<span class="gr-crt" aria-hidden="true"></span>
			<!-- Two movers and two fixed words. The movers each carry BOTH of
			     their glyphs, stacked in one cell and cross-faded at the arc's
			     apex — a span cannot morph "Love" into "Do", and swapping the
			     text at the midpoint of a 700ms flight, under the thread's own
			     light, is indistinguishable from one that could. What the eye
			     is tracking is the MOTION, and the motion is honest. -->
			<!-- THE CELLS DO NOT MOVE; the words do. Each cell holds the two
			     words that will ever stand in ITS position — first cell
			     "Love" then "Do", last cell "do." then "love." — stacked in
			     one grid area, so the cell is as wide as the wider of the two
			     and the sentence's geometry never changes under its own
			     swap.

			     Moving the CELLS instead is the obvious implementation and it
			     is wrong: the two cells are different widths (a cell holding
			     "love." is nearly twice one holding "Do"), so after a
			     centre-to-centre trade the finished line has a hole on one
			     side and touching words on the other. Measured on screen
			     before this was rewritten: "Do⎵⎵what youlove."

			     The handoff between the leaving glyph and the arriving one is
			     exact rather than approximate. "Love" leaves cell one along
			     an arc to +travel; "love." arrives at cell four from
			     −travel along the SAME arc — and since each word is centred
			     in its own cell and the cells are exactly `travel` apart,
			     the two are at the identical screen point at every instant
			     of the flight. The cross-fade between them therefore cannot
			     be seen; what is seen is one word crossing the line. -->
			<span class="gr-slot gr-slot-1" bind:this={slotAEl}>
				<i class="gr-w gr-go">Love</i><i class="gr-w gr-come">Do</i>
			</span>
			<!-- The two words that never move still have to CHANGE FONT, and a
			     font cannot tween. Each carries its modern twin as an overlay
			     (`::after`, from `data-w`) cross-faded on the same --sw the
			     flight rides, so the whole line arrives in one register even
			     though only half of it is travelling. The box stays sized by
			     the terminal face, which is the wider of the two, so nothing
			     reflows under the morph. -->
			<span class="gr-fixed" data-w="what">what</span>
			<span class="gr-fixed" data-w="you">you</span>
			<span class="gr-slot gr-slot-4" bind:this={slotBEl}>
				<i class="gr-w gr-go">do.</i><i class="gr-w gr-come">love.</i>
			</span>
		</p>
	{/if}
</section>
</div>

<style>
	/* This section is deliberately NOT a tall sticky track. Its ordinary
	   passage through the viewport supplies progress to the scrubber, so the
	   payoff takes only the space its four words deserve. */
	.gr-track {
		position: relative;
		height: auto;
	}
	.gr-track.gr-still {
		height: auto;
	}
	.gr {
		--gr-ink: var(--text-primary);
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: clamp(20rem, 54svh, 34rem);
		padding: clamp(3rem, 8vw, 6rem) clamp(1rem, 5vw, 3rem);
		overflow: hidden;
		/* A restrained theme wash separates the payoff without opening a second
		   visual world. The selected theme owns both ground and ink. */
		background:
			radial-gradient(
				70% 82% at 50% 50%,
				color-mix(in srgb, var(--accent-primary, #9e59ff) 7%, transparent),
				transparent 72%
			),
			var(--landing-bg);
		color: var(--gr-ink);
		font-family: var(--lp-font, 'Geist', system-ui, sans-serif);
	}

	/* The sentence the assistive tree gets: resolved, once, and never
	   animated. Visually hidden rather than display:none — it is the
	   section's real heading; the hero owns the page's h1. */
	.gr-sr {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}

	.gr-canvas {
		position: absolute;
		inset: 0;
		z-index: 1;
		width: 100%;
		height: 100%;
		pointer-events: none;
	}

	.gr-line {
		position: relative;
		z-index: 2;
		display: flex;
		flex-wrap: nowrap;
		align-items: baseline;
		justify-content: center;
		gap: 0.28em;
		margin: 0;
		max-width: 100%;
		/* SIZED FOR ITS WIDEST MOMENT, which is mid-flight and not either end.
		   A monospaced face is far wider than the display one, and each cell
		   holds the WIDER of its two words for the whole flight — so the line
		   peaks in the middle of the swap, not at rest. At 7.4vw it fitted on
		   one line at both ends and broke to three in between. */
		font-size: clamp(1.6rem, 5.4vw, 4.2rem);
		/* And it never wraps: four words on one line IS the idea, and a line
		   that reflows mid-morph is a different sentence every frame. */
		white-space: nowrap;
		font-weight: 620;
		letter-spacing: -0.042em;
		line-height: 1.04;
		text-align: center;
		/* SOLID INK, deliberately — not the gradient the rest of the page
		   uses for display type. `background-clip: text` paints the
		   gradient at the LINE and clips it to every descendant glyph,
		   including a word held at opacity 0, so both halves of a swap
		   print on top of each other. The colour has to live on the glyphs
		   themselves here, and the thread supplies the aurora anyway. */
		color: var(--gr-ink);
		text-shadow: 0 0 46px color-mix(in srgb, var(--accent-primary, #9e59ff) 30%, transparent);
	}

	/* The arrival: the whole phrase fades up once, then holds. */
	.gr-live {
		animation: gr-in 800ms cubic-bezier(0.22, 1, 0.36, 1) both;
	}
	@keyframes gr-in {
		from {
			opacity: 0;
			transform: translateY(16px);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}

	/* A slot is a fixed cell holding both of its words, sized by whichever
	   is wider, so the line's geometry never changes — only what is inside
	   the cells, and where the cells' contents are. Sizing on the wider word
	   is what stops the sentence reflowing under its own swap. */
	/* A cell holds the WIDER of its two words for the whole flight, then
	   settles to the width of the one that landed in it. Both are measured;
	   the fallbacks only matter for the frame before measure() runs. */
	.gr-slot {
		position: relative;
		display: inline-grid;
		/* The track must be the BOX, not the content. Left implicit, the
		   single column sizes to max-content — the wider of the two words —
		   so setting `width` narrows the box while the track keeps its old
		   size and overflows it, and a centred child ends up offset by half
		   the difference. Measured: the settled "Do" sat 39px right of its
		   own cell, into the word after it. */
		grid-template-columns: minmax(0, 1fr);
		align-items: baseline;
		justify-items: center;
		/* THREE widths, not two, and the third one is the bug fix. The cell
		   used to sit at --w-hold (the wider of its pair) for the whole
		   opening hold, so in "Love what you do." the last cell was as wide
		   as "love." with "do." centred in it — a visible pocket of space on
		   either side of the shortest word in the sentence, before anything
		   had happened. Owner caught it.

		   So: the cell rests at the width of the word ACTUALLY IN IT, opens
		   to the wider of the pair for the flight, and settles to the word
		   that landed. The open and the close are scrubbed into the first
		   and last eighth of the swap, which leaves the cell centres fixed
		   for the middle three-quarters — and fixed centres are what the
		   flight's geometry depends on, since --travel is one measured
		   number and the two arcs assume the cells stay that far apart. */
		--w-open: clamp(0, calc(var(--sw, 0) / 0.12), 1);
		--w-close: clamp(0, calc((var(--sw, 0) - 0.88) / 0.12), 1);
		width: calc(
			var(--w-go, auto) + (var(--w-hold) - var(--w-go)) * var(--w-open) +
				(var(--w-end) - var(--w-hold)) * var(--w-close)
		);
	}
	/* ── MACHINE → HUMAN, IN THE TYPE ─────────────────────────────────
	   The unresolved line is the machine's voice: theme accent, monospaced,
	   glowing through restrained scanlines and the flicker of a tube.
	   The resolved one is the page's own display face. So the greeting is the
	   film's whole argument compressed into four words changing register —
	   the same machine that later types READY, learning to say something
	   worth saying.

	   A FONT CANNOT TWEEN, so nothing tries to. The two moving words already
	   cross-fade through their flight — `.gr-go` leaves, `.gr-come` arrives —
	   which means giving each a different face makes the morph free and
	   exact: at the apex both are half-present and neither register is
	   legible enough to catch. The two fixed words get the same treatment
	   through an overlaid twin. */
	.gr-go {
		font-family: var(--lp-mono, ui-monospace, 'Courier New', monospace);
		font-weight: 500;
		letter-spacing: 0.02em;
		color: var(--accent-primary, #9e59ff);
		text-shadow:
			0 0 1px color-mix(in srgb, var(--accent-primary, #9e59ff) 90%, transparent),
			0 0 12px color-mix(in srgb, var(--accent-primary, #9e59ff) 52%, transparent),
			0 0 34px color-mix(in srgb, var(--accent-secondary, #4fd1c5) 25%, transparent);
	}
	.gr-come {
		font-family: var(--lp-font, 'Geist', system-ui, sans-serif);
	}

	/* The fixed words: BOTH faces are pseudo-elements and the element's own
	   text is invisible. It sizes the box (the terminal face is the wider of
	   the two, so nothing reflows) and nothing more.

	   The obvious shape — real text faded out, modern face overlaid in
	   ::after — cannot work: `opacity` applies to the whole SUBTREE, so
	   fading the element takes its own overlay with it. Measured, "what you"
	   simply disappeared from the resolved line. */
	.gr-fixed {
		position: relative;
		font-family: var(--lp-mono, ui-monospace, 'Courier New', monospace);
		font-weight: 500;
		letter-spacing: 0.02em;
		color: transparent;
	}
	.gr-fixed::before,
	.gr-fixed::after {
		content: attr(data-w);
		position: absolute;
		left: 50%;
		top: 0;
		transform: translateX(-50%);
		white-space: pre;
	}
	/* The machine's voice, leaving. */
	.gr-fixed::before {
		font-family: inherit;
		font-weight: inherit;
		letter-spacing: inherit;
		color: var(--accent-primary, #9e59ff);
		text-shadow:
			0 0 1px color-mix(in srgb, var(--accent-primary, #9e59ff) 90%, transparent),
			0 0 12px color-mix(in srgb, var(--accent-primary, #9e59ff) 52%, transparent),
			0 0 34px color-mix(in srgb, var(--accent-secondary, #4fd1c5) 25%, transparent);
		opacity: calc(1 - clamp(0, calc((var(--sw, 0) - 0.44) / 0.12), 1));
	}
	/* The page's own voice, arriving. A SHORT SWAP AT THE APEX, not a long
	   cross-fade: these two words do not move, so while both faces are
	   part-present they smear — two widths centred on each other at half
	   opacity. In a narrow window at the apex it is nearly invisible, because
	   that is the exact moment the eye is tracking the two words that ARE
	   moving and their own twins are mid-cross-fade too. */
	.gr-fixed::after {
		font-family: var(--lp-font, 'Geist', system-ui, sans-serif);
		font-weight: 620;
		letter-spacing: -0.042em;
		color: var(--gr-ink);
		text-shadow: 0 0 46px color-mix(in srgb, var(--accent-primary, #9e59ff) 30%, transparent);
		opacity: clamp(0, calc((var(--sw, 0) - 0.44) / 0.12), 1);
	}

	/* THE TUBE'S ARTIFACTS. Scanlines and a slow flicker stay on the line,
	   not the section: the words are the machine speaking. Both leave with
	   the accent voice, and neither exists once the line has resolved. */
	.gr-crt {
		position: absolute;
		inset: -0.35em -0.6em;
		z-index: 3;
		pointer-events: none;
		border-radius: 0.2em;
		opacity: calc((1 - clamp(0, calc((var(--sw, 0) - 0.28) / 0.34), 1)) * 0.5);
		background: repeating-linear-gradient(
			0deg,
			color-mix(in srgb, var(--text-primary) 18%, transparent) 0,
			transparent 1.6px,
			color-mix(in srgb, var(--text-primary) 18%, transparent) 3.2px
		);
		mix-blend-mode: soft-light;
		animation: gr-flicker 5.4s steps(1, end) infinite;
	}
	@keyframes gr-flicker {
		0%,
		92%,
		100% {
			filter: none;
		}
		93% {
			filter: brightness(1.5);
		}
		95% {
			filter: brightness(0.72);
		}
		97% {
			filter: brightness(1.22);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.gr-crt {
			animation: none;
		}
	}
	/* Reduced motion states the resolved line only, so there is no machine
	   voice to leave and nothing to flicker. */
	.gr-still .gr-crt {
		display: none;
	}

	.gr-w {
		grid-area: 1 / 1;
		font-style: normal;
		white-space: pre;
	}
	/* Resting state: the word that is here now, and the word that has not
	   arrived yet. */
	.gr-come {
		opacity: 0;
	}

	/* THE SWAP — two arcs, and the words that share one are the same word.
	   "Love" leaves cell one and "love." arrives at cell four on the UPPER
	   arc; "do." leaves cell four and "Do" arrives at cell one on the LOWER
	   one, so the two streams pass each other instead of through each other.
	   --travel and --arc are measured by measure(); everything else here is
	   a fixed curve. */
	/* SCRUBBED, not played. Each flight is its own keyframe set held PAUSED
	   with a negative delay proportional to --sw, which seeks the animation
	   to exactly that point and holds it there — so scroll position, and
	   nothing else, decides where the words are. Scrolling back up runs them
	   backwards for free.

	   The duration is 1s and the timing function is LINEAR on purpose: the
	   easing that used to live here shaped the flight in TIME, and there is
	   no time here any more. The visitor's scroll is the timing function. */
	.gr-live .gr-slot-1 .gr-go {
		animation: gr-up-out 1s linear both paused;
		animation-delay: calc(var(--sw, 0) * -1s);
	}
	.gr-live .gr-slot-4 .gr-come {
		animation: gr-up-in 1s linear both paused;
		animation-delay: calc(var(--sw, 0) * -1s);
	}
	.gr-live .gr-slot-4 .gr-go {
		animation: gr-down-out 1s linear both paused;
		animation-delay: calc(var(--sw, 0) * -1s);
	}
	.gr-live .gr-slot-1 .gr-come {
		animation: gr-down-in 1s linear both paused;
		animation-delay: calc(var(--sw, 0) * -1s);
	}
	/* The delay is repeated in each rule rather than hoisted into one
	   `.gr-live .gr-w`, and that is not redundancy. The `animation`
	   shorthand RESETS animation-delay to 0, and these four selectors are
	   more specific than a shared one — so the hoisted version lost to its
	   own siblings and every word sat frozen at frame zero. Measured: --sw
	   scrubbed correctly all the way to 1 while "Love" stayed fully opaque
	   and "Do" never appeared. */
	/* The four keyframes are two curves stated twice: an -out ends held at
	   the far end and invisible, an -in starts held at the far end and
	   invisible, and their opacity windows meet at the apex. Opacity is on
	   the same keyframe set as the transform, so a leaving glyph and its
	   arriving twin cannot drift apart under a slow frame. */
	@keyframes gr-up-out {
		0%,
		32% {
			opacity: 1;
		}
		68%,
		100% {
			opacity: 0;
		}
		0% {
			transform: none;
		}
		50% {
			transform: translate(calc(var(--travel, 240px) * 0.5), calc(var(--arc, 40px) * -1));
		}
		100% {
			transform: translateX(var(--travel, 240px));
		}
	}
	@keyframes gr-up-in {
		0%,
		32% {
			opacity: 0;
		}
		68%,
		100% {
			opacity: 1;
		}
		0% {
			transform: translateX(calc(var(--travel, 240px) * -1));
		}
		50% {
			transform: translate(calc(var(--travel, 240px) * -0.5), calc(var(--arc, 40px) * -1));
		}
		100% {
			transform: none;
		}
	}
	@keyframes gr-down-out {
		0%,
		32% {
			opacity: 1;
		}
		68%,
		100% {
			opacity: 0;
		}
		0% {
			transform: none;
		}
		50% {
			transform: translate(calc(var(--travel, 240px) * -0.5), var(--arc, 40px));
		}
		100% {
			transform: translateX(calc(var(--travel, 240px) * -1));
		}
	}
	@keyframes gr-down-in {
		0%,
		32% {
			opacity: 0;
		}
		68%,
		100% {
			opacity: 1;
		}
		0% {
			transform: translateX(var(--travel, 240px));
		}
		50% {
			transform: translate(calc(var(--travel, 240px) * 0.5), var(--arc, 40px));
		}
		100% {
			transform: none;
		}
	}

	/* Reduced motion: the same compact, theme-native section with the resolved
	   line standing still. */
	.gr-still.gr {
		position: relative;
		height: clamp(15rem, 38svh, 24rem);
		min-height: 0;
	}
	.gr-still .gr-line {
		animation: none;
	}

	@media (max-width: 640px) {
		.gr-line {
			gap: 0.22em;
			letter-spacing: -0.035em;
		}
	}
</style>
