<script lang="ts">
	// The prologue reel — the generated film, scrubbed by the scroll.
	//
	// The 2026-08-04 owner verdict retired the hand-drawn SVG prologue: the
	// real Act 0 is AI-generated footage, exported as ONE contiguous frame
	// sequence and driven frame-exact by scroll position. This layer is the
	// Apple product-page technique — a frame sequence blitted to a canvas —
	// chosen over <video>.currentTime because seeking a video under scrub
	// hunts keyframes and stutters, worst on Safari, while a frame sequence
	// gives an exact image for every scroll position in BOTH directions.
	//
	// House rules honored, the same ones MoteField lives by:
	//   · ONE render loop owns cadence — MovieTrack's rAF calls tick(); this
	//     module never requests a frame of its own.
	//   · Non-reactive plain-object runtime state; no Svelte churn per frame.
	//   · DPR capped; no allocation in the draw path; the blit is skipped
	//     entirely when the mapped frame has not changed (a 12fps reel under
	//     a 120Hz rAF repeats the same source frame ~10 times in a row).
	//   · A missing manifest degrades to NOTHING, silently: MovieTrack keeps
	//     its CSS/SVG prologue beats and the film is whole without the film.
	//
	// Decode discipline: frames are fetched and decoded through
	// createImageBitmap (off the main thread), inside a rolling window around
	// the playhead, with a hard cap on live bitmaps. We never hold all 610 —
	// that would be ~1.1GB of decoded surface. When the wanted frame is not
	// ready yet the nearest decoded neighbour is drawn instead, so the reel
	// degrades to a lower frame rate under a fast scrub rather than blocking.
	import { onDestroy } from 'svelte';
	import type { ReelCaption } from './landingReel';

	interface Rect {
		x: number;
		y: number;
		w: number;
		h: number;
	}
	type Triple = [number, number, number];

	/**
	 * The MATCH-CUT CONTRACT — everything the page must know about a film in
	 * order to hand off to the DOM at its last frame. Declared in FRAME
	 * PIXELS and divided by the manifest's own width/height at load, so the
	 * numbers are readable next to the frames they were measured on and the
	 * manifest describes itself at any resolution.
	 */
	interface MatchCut {
		/** The tube inside the bezel: the rect the DOM CRT must land on. */
		screen: Rect;
		/** The whole machine's case — the thing a phone crop must not cut. */
		case: Rect;
		/** What lies outside the picture: the letterbox band's colour, and
		    the ground the film hands over to at the cut. */
		surround: Triple;
		/** The tube's own light, which the DOM CRT's phosphor picks against. */
		screenTint: Triple;
	}

	interface Manifest {
		frames: number;
		width: number;
		height: number;
		pattern: string;
		fps: number;
		/** Bumped whenever any frame's CONTENT changes under a stable name;
		    appended to every frame URL so a rebuilt beat is never served
		    from a stale cache. Absent = no busting (pre-versioned reels). */
		version?: string | number;
		/** Absent on a reel cut before this field existed — the photoreal
		    film's own numbers stand in, which is what they always were. */
		matchCut?: MatchCut;
		/** The whisper lines, in THIS reel's frames. MovieTrack renders them. */
		captions?: ReelCaption[];
		/** Station id → accessible label: the prologue stations describe the
		    beats of whichever film is playing. */
		aria?: Record<string, string>;
	}

	/** Optional callback: fired once when the reel becomes (un)available. */
	export let onState: (on: boolean) => void = () => {};

	/**
	 * Which film. The directory holds the frame sequence AND the manifest
	 * that describes it; nothing about a reel is known outside it.
	 *
	 * CHANGING IT MID-RIDE IS SUPPORTED, and it is a full re-cut: two films
	 * agree on nothing but their aspect: different match-cut rects, different
	 * surround, different caption frames, different bytes behind identical
	 * frame numbers. So the swap closes every decoded bitmap, drops the
	 * geometry and re-fetches the manifest from the new directory — and
	 * `era` fences the work already in flight, or a frame decoded from the
	 * old film would land in the new film's cache under the same index and
	 * the reel would play two movies at once.
	 */
	export let base = '/prologue/reel/';
	/** ± frames kept decoded around the playhead. */
	const WINDOW = 12;
	/**
	 * Live-bitmap budget in BYTES, not frames — the thing that actually
	 * matters is resident memory, and a decoded frame costs w·h·4. A fixed
	 * frame count silently doubles the footprint the moment a reel ships at a
	 * higher resolution (960×540 → 1280×720 took 36 frames from ~71MB to
	 * ~126MB, which desktop shrugs off and an older phone does not).
	 * ~72MB leaves 960×540 exactly as it was (36 frames, ~71MB) and trims a
	 * 1280×720 reel from 36 frames to the floor — ~126MB down to ~91MB.
	 *
	 * The floor WINS above ~720p, and that is deliberate: holding fewer than
	 * the working window would thrash the decoder on every scrub, which is
	 * worse than the memory. So this budget trims fat, it does not guarantee
	 * a ceiling — a 1080p reel would need the window itself narrowed.
	 */
	const BITMAP_BUDGET_BYTES = 72 * 1024 * 1024;
	/** Never drop below the working window, whatever the resolution costs. */
	const MIN_CAP = WINDOW * 2 + 2;
	/** Concurrent fetches — browsers cap ~6 per host anyway. */
	const MAX_INFLIGHT = 6;
	/** How far to search for a decoded neighbour before drawing nothing. */
	const NEIGHBOUR_REACH = 40;

	/** The match cut as the draw path wants it: fractions, and CSS colour. */
	interface NormalCut {
		screen: Rect;
		case: Rect;
		surround: string;
		screenTint: Triple;
	}

	// The photoreal film's own match-cut contract, already normalized — the
	// stand-in for a manifest cut before `matchCut` existed. Every number is
	// measured off that reel's delivered final frame (f610): the dark tube
	// inside the beige bezel, the machine's case, the black room around it,
	// and the tube's grey-teal light. A reel that declares its own overrides
	// all of it; nothing here is true of any film but that one.
	const FALLBACK_CUT: NormalCut = {
		screen: { x: 259 / 960, y: 60 / 540, w: 492 / 960, h: 357 / 540 },
		case: { x: 177 / 960, y: 14 / 540, w: 619 / 960, h: 526 / 540 },
		surround: 'rgb(0, 0, 0)',
		screenTint: [36, 42, 42]
	};

	/**
	 * Framing rule. Wide viewports get a true cover crop — the footage fills
	 * the frame and the sides spill off, which is what makes it read as a
	 * film rather than a video embedded in a page. Tall viewports (phones)
	 * cannot: a full cover on 390×844 would show only the middle 26% of a
	 * 16:9 composition and would slice the match-cut CRT in half. So the
	 * crop is BOUNDED — never lose more than this fraction of frame width —
	 * and what is left over becomes a band of the film's own surround, which
	 * reads as widescreen.
	 *
	 * The bound is never a round number and is never chosen: it is exactly
	 * what the machine's case demands. A centred window must reach whichever
	 * of the case's two edges lies further from frame centre, or the last
	 * shot loses a corner of the thing the whole film has been travelling
	 * toward — so the bound falls out of the case rect the reel declares.
	 */
	/**
	 * How much of the stage the film occupies, before the feather.
	 *
	 * Tuned against the two constraints that actually bind: the frames are
	 * 1280 wide and want a downscale rather than an upscale, and the feather
	 * needs real margin to fade across or it reads as a blur on the picture
	 * instead of an edge dissolving.
	 */
	const INSET = 0.62;
	/**
	 * Phones get their own framing, because the inset does nothing there.
	 *
	 * The bounded-cover rule makes the film WIDER than a narrow viewport and
	 * crops the sides — at 390px the picture is still ~930px across even
	 * inset, so it runs edge to edge and there is no margin for a feather to
	 * live in. Scaling it further only crops less; it never pulls the sides in.
	 *
	 * So a narrow viewport fits the whole frame instead and insets that. The
	 * film becomes a properly framed band with margin on all four sides, which
	 * is what lets the same soft edge read on a phone. It uses less of the
	 * screen than a crop would — that is the trade, and it is the right one for
	 * a beat the visitor is looking AT rather than being inside.
	 */
	const NARROW_W = 640;
	const NARROW_INSET = 0.88;

	function minVisibleW(c: Rect): number {
		return Math.max(2 * Math.abs(0.5 - c.x), 2 * Math.abs(c.x + c.w - 0.5));
	}

	/** Frame pixels → frame fractions: the manifest describes its own scale. */
	function normalize(m: MatchCut, w: number, h: number): NormalCut {
		const rect = (r: Rect): Rect => ({ x: r.x / w, y: r.y / h, w: r.w / w, h: r.h / h });
		return {
			screen: rect(m.screen),
			case: rect(m.case),
			surround: `rgb(${m.surround.join(', ')})`,
			screenTint: m.screenTint
		};
	}

	let canvasEl: HTMLCanvasElement;

	const rt: {
		ctx: CanvasRenderingContext2D | null;
		man: Manifest | null;
		prefix: string;
		suffix: string;
		pad: number;
		bust: string;
		cut: NormalCut;
		minW: number;
		bmp: Map<number, ImageBitmap>;
		inflight: Set<number>;
		asked: boolean;
		failed: boolean;
		idx: number;
		drawn: number;
		dir: number;
		dx: number;
		dy: number;
		dw: number;
		dh: number;
		letterbox: boolean;
		cw: number;
		ch: number;
		shown: boolean;
		alpha: number;
	} = {
		ctx: null,
		man: null,
		prefix: '',
		suffix: '',
		bust: '',
		cut: FALLBACK_CUT,
		minW: minVisibleW(FALLBACK_CUT.case),
		pad: 3,
		bmp: new Map(),
		inflight: new Set(),
		asked: false,
		failed: false,
		idx: 1,
		drawn: -1,
		dir: 1,
		dx: 0,
		dy: 0,
		dw: 0,
		dh: 0,
		letterbox: false,
		cw: 0,
		ch: 0,
		shown: false,
		alpha: -1
	};

	// The geometry MovieTrack reads to place the DOM CRT and the caption
	// band. Mutated in place — never reallocated — so reading it costs
	// nothing and creates no garbage.
	const g = {
		ok: false,
		bandTop: 0,
		bandBottom: 0,
		bandLeft: 0,
		bandRight: 0,
		/** Screen-space rect of the footage's CRT tube at the match cut. */
		screenCx: 0,
		screenCy: 0,
		screenW: 0,
		screenH: 0,
		/**
		 * The footage's own BEZEL at the cut — half the difference between the
		 * photographed case and the photographed screen.
		 *
		 * The DOM machine used to grow its bezel from ZERO across the handoff,
		 * on the reasoning that the footage supplies the case until the machine
		 * takes over. But the footage FADES on its own schedule, so there was a
		 * window with the photographed bezel already gone and the drawn one not
		 * yet arrived — a screen floating with no case around it, which is
		 * exactly what a match cut must never show. Published so the DOM bezel
		 * can start where the photograph's ended instead of at nothing.
		 */
		bezelX: 0,
		bezelY: 0
	};

	/** The reel's live geometry — valid once configure() has run. */
	export function geom(): typeof g {
		return g;
	}

	/**
	 * What the loaded manifest says about THIS film, for the page around it:
	 * how many frames its caption windows are counted in, the whisper lines
	 * and station labels it wants (null = it declares none, so the caller's
	 * own defaults stand), the colour outside the picture, and the tube's
	 * light. Null until a manifest resolves.
	 */
	export function meta(): {
		frames: number;
		captions: ReelCaption[] | null;
		aria: Record<string, string> | null;
		surround: string;
		screenTint: Triple;
	} | null {
		const man = rt.man;
		if (!man) return null;
		return {
			frames: man.frames,
			captions: man.captions?.length ? man.captions : null,
			aria: man.aria ?? null,
			surround: rt.cut.surround,
			screenTint: rt.cut.screenTint
		};
	}

	/** Whether the footage is available; false keeps MovieTrack's fallback. */
	export function isOn(): boolean {
		return rt.man !== null;
	}

	/**
	 * Which film's work is current. Every fetch carries the era it was asked
	 * in and drops itself if the film has changed since; a swap bumps it.
	 */
	let era = 0;
	/** The film `rt` currently holds. Drives the re-cut when `base` moves. */
	let playing = '';

	$: if (base !== playing) cutTo(base);

	/**
	 * Re-cut to `next`. Everything the old film owned goes: its decoded
	 * frames (closed, not merely dropped — an ImageBitmap holds native
	 * memory), its manifest, its match cut, its printf split, its playhead.
	 * `rt.asked = false` is what re-arms the fetch; the next tick asks the
	 * new directory for its manifest and `onState` fires again with it, so
	 * the page re-reads captions, labels, tube tint and CRT geometry from
	 * the film that is actually playing.
	 *
	 * The canvas is deliberately NOT cleared: it holds the old film's last
	 * frame across the fetch, which is a beat of stale footage rather than a
	 * flash of empty stage on a swap that takes one local request.
	 */
	function cutTo(next: string): void {
		playing = next;
		era++;
		for (const bm of rt.bmp.values()) bm.close();
		rt.bmp.clear();
		rt.inflight.clear();
		rt.man = null;
		rt.asked = false;
		rt.failed = false;
		rt.idx = 1;
		rt.dir = 1;
		rt.drawn = -1;
		rt.prefix = '';
		rt.suffix = '';
		rt.bust = '';
		rt.pad = 3;
		rt.cut = FALLBACK_CUT;
		rt.minW = minVisibleW(FALLBACK_CUT.case);
		g.ok = false;
	}

	function framePath(i: number): string {
		return rt.prefix + String(i).padStart(rt.pad, '0') + rt.suffix + rt.bust;
	}

	async function loadManifest(): Promise<void> {
		if (rt.asked) return;
		rt.asked = true;
		const mine = era;
		try {
			// The manifest is the INDEX and must never be served stale: frame
			// URLs are stable while their content is not (a regenerated beat
			// reuses f184.webp), so a force-cached manifest would pin the
			// browser to a retired cut of the film forever — which is exactly
			// what it did after the evolution beats were rebuilt. Revalidate
			// this one small file; the frames stay force-cached and are busted
			// by the manifest's `version`.
			const res = await fetch(base + 'manifest.json', { cache: 'no-cache' });
			if (mine !== era) return;
			if (!res.ok) throw new Error(String(res.status));
			const man = (await res.json()) as Manifest;
			// A swap landed while this was in flight: this manifest describes a
			// film nobody is watching any more, and the swap has already
			// re-armed the fetch for the one that is.
			if (mine !== era) return;
			if (!man?.frames || !man.pattern || !man.width || !man.height) throw new Error('shape');
			// Split the printf pattern ONCE — the draw path never parses.
			const token = man.pattern.match(/%0(\d+)d/);
			if (!token || token.index === undefined) throw new Error('pattern');
			rt.pad = Number(token[1]);
			rt.prefix = base + man.pattern.slice(0, token.index);
			rt.suffix = man.pattern.slice(token.index + token[0].length);
			rt.bust = man.version === undefined ? '' : '?v=' + String(man.version);
			// A half-written match cut is not a reason to lose the whole film:
			// the frames are still good, so the photoreal contract stands in
			// and the cut lands where it always did.
			const mc = man.matchCut;
			if (mc?.screen?.w && mc.case?.w && mc.surround?.length === 3 && mc.screenTint?.length === 3) {
				rt.cut = normalize(mc, man.width, man.height);
				rt.minW = minVisibleW(rt.cut.case);
			}
			rt.man = man;
			layout();
			onState(true);
		} catch {
			if (mine !== era) return;
			// No footage (not generated yet, or a bad deploy): the film keeps
			// its hand-drawn prologue beats and nobody sees a broken frame.
			rt.failed = true;
			onState(false);
		}
	}

	function request(i: number): void {
		if (!rt.man) return;
		if (i < 1 || i > rt.man.frames) return;
		if (rt.bmp.has(i) || rt.inflight.has(i)) return;
		if (rt.inflight.size >= MAX_INFLIGHT) return;
		rt.inflight.add(i);
		const mine = era;
		fetch(framePath(i), { cache: 'force-cache' })
			.then((r) => (r.ok ? r.blob() : Promise.reject(new Error(String(r.status)))))
			.then((b) => createImageBitmap(b))
			.then((bm) => {
				// A teardown or a swap may have landed while this was in flight.
				// Frame numbers are shared across films, so caching this one now
				// would put the other movie's image at this index — the swap
				// already cleared the slot, and this must not refill it.
				if (mine !== era || !rt.man) {
					bm.close();
					return;
				}
				rt.inflight.delete(i);
				rt.bmp.set(i, bm);
				evict();
			})
			.catch(() => {
				if (mine === era) rt.inflight.delete(i);
			});
	}

	/** Frames we can hold at this reel's resolution inside the byte budget. */
	function capFor(): number {
		const w = rt.man?.width ?? 960;
		const h = rt.man?.height ?? 540;
		const perFrame = Math.max(1, w * h * 4);
		return Math.max(MIN_CAP, Math.floor(BITMAP_BUDGET_BYTES / perFrame));
	}

	function evict(): void {
		// Farthest-from-the-playhead first: a scrub reverses often enough
		// that pure LRU would throw away frames we are about to want again.
		while (rt.bmp.size > capFor()) {
			let worst = -1;
			let worstD = -1;
			for (const k of rt.bmp.keys()) {
				const d = Math.abs(k - rt.idx);
				if (d > worstD) {
					worstD = d;
					worst = k;
				}
			}
			if (worst < 0) return;
			rt.bmp.get(worst)?.close();
			rt.bmp.delete(worst);
		}
	}

	function pump(): void {
		// Fill the rolling window: the playhead first, then the whole span
		// AHEAD of it, and only then the span behind. Interleaving the two
		// directions spends half of a scarce six-request budget on frames
		// the playhead has already passed, which halves the lead time and
		// shows as a coarser reel under a fast scrub. Behind still gets
		// filled — a scrub reverses — but it queues second.
		request(rt.idx);
		for (let k = 1; k <= WINDOW; k++) {
			if (rt.inflight.size >= MAX_INFLIGHT) return;
			request(rt.idx + k * rt.dir);
		}
		for (let k = 1; k <= WINDOW; k++) {
			if (rt.inflight.size >= MAX_INFLIGHT) return;
			request(rt.idx - k * rt.dir);
		}
	}

	/** Canvas sizing + the cover/band transform. Called on mount and resize. */
	export function configure(cw: number, ch: number): void {
		rt.cw = cw;
		rt.ch = ch;
		if (!canvasEl || cw === 0 || ch === 0) return;
		const dpr = Math.min(2, window.devicePixelRatio || 1);
		canvasEl.width = Math.round(cw * dpr);
		canvasEl.height = Math.round(ch * dpr);
		rt.ctx = canvasEl.getContext('2d', { alpha: false });
		rt.ctx?.setTransform(dpr, 0, 0, dpr, 0, 0);
		rt.drawn = -1;
		layout();
	}

	function layout(): void {
		const man = rt.man;
		if (!man || rt.cw === 0 || rt.ch === 0) return;
		const cover = Math.max(rt.cw / man.width, rt.ch / man.height);
		const contain = Math.min(rt.cw / man.width, rt.ch / man.height);
		// Bounded cover: never crop past the case's own visible-width demand.
		const capped = rt.cw / (man.width * rt.minW);
		// INSET, so the film can feather instead of ending at a hard edge.
		//
		// A full-bleed reel has nowhere to fade INTO: its picture runs to the
		// stage's own edges, so any vignette would eat the picture rather than
		// the margin. Pulling it in leaves the margin the feather needs.
		//
		// It also buys resolution, which is the better half of the argument.
		// The plates are 1280 wide; a cover crop on a 1440 stage UPSCALES them
		// about 1.13×, and the film has been paying that softness at every
		// desktop width. At this inset the same frames land near 1080px — a
		// downscale, so the picture gets sharper by being smaller.
		//
		// The case guarantee survives for free. `capped` is an UPPER bound on
		// scale (it stops the crop eating the machine's case); shrinking only
		// ever reveals more frame, so a smaller scale cannot violate it — and
		// the narrow branch, which fits rather than crops, reveals the most of
		// all.
		const narrow = rt.cw < NARROW_W;
		const scale = narrow
			? contain * NARROW_INSET
			: Math.max(contain, Math.min(cover, capped)) * INSET;
		rt.dw = man.width * scale;
		rt.dh = man.height * scale;
		rt.dx = (rt.cw - rt.dw) / 2;
		rt.dy = (rt.ch - rt.dh) / 2;
		rt.letterbox = rt.dh < rt.ch - 0.5 || rt.dw < rt.cw - 0.5;
		g.bandTop = rt.dy;
		g.bandBottom = rt.dy + rt.dh;
		g.bandLeft = rt.dx;
		g.bandRight = rt.dx + rt.dw;
		const s = rt.cut.screen;
		g.screenW = s.w * rt.dw;
		g.screenH = s.h * rt.dh;
		g.screenCx = rt.dx + (s.x + s.w / 2) * rt.dw;
		g.screenCy = rt.dy + (s.y + s.h / 2) * rt.dh;
		const c = rt.cut.case;
		g.bezelX = Math.max(0, (c.w * rt.dw - g.screenW) / 2);
		g.bezelY = Math.max(0, (c.h * rt.dh - g.screenH) / 2);
		g.ok = true;
		// The band beside the picture is the FILM's surround, not black: black
		// is only true of a film that ends in an unlit room.
		if (canvasEl) {
			canvasEl.style.background = rt.cut.surround;
			// THE FEATHER FOLLOWS THE PICTURE, not the canvas. The canvas still
			// covers the whole stage — moving it would move every coordinate the
			// match cut is measured in — so the mask is centred and sized on the
			// drawn rect instead. Radii run past the picture's own half-extent
			// so the solid core reaches its corners and only the surround fades.
			// AN ELLIPSE IS THE WRONG SHAPE FOR A RECTANGLE, which the first pass
			// learned the hard way. To cover a rectangle's corners an ellipse
			// has to reach 1.41× its half-extents — so the EDGE MIDPOINTS sit at
			// only ~71% of that radius and barely fade at all. Corners eaten, or
			// edges hard: an ellipse cannot give both.
			//
			// Two linear gradients, intersected, feather all four edges evenly —
			// the same construction the montage's frame uses. The stops are in
			// PIXELS of the drawn rect, not percentages of the canvas, because
			// the canvas covers the whole stage and the picture is a box inside
			// it.
			const fade = Math.min(rt.dw, rt.dh) * 0.16;
			const px = (n: number) => `${n.toFixed(1)}px`;
			canvasEl.style.setProperty('--pr-x0', px(rt.dx));
			canvasEl.style.setProperty('--pr-x1', px(rt.dx + fade));
			canvasEl.style.setProperty('--pr-x2', px(rt.dx + rt.dw - fade));
			canvasEl.style.setProperty('--pr-x3', px(rt.dx + rt.dw));
			canvasEl.style.setProperty('--pr-y0', px(rt.dy));
			canvasEl.style.setProperty('--pr-y1', px(rt.dy + fade));
			canvasEl.style.setProperty('--pr-y2', px(rt.dy + rt.dh - fade));
			canvasEl.style.setProperty('--pr-y3', px(rt.dy + rt.dh));
			// AND A THIRD LAYER, for the corners. The two linear feathers give
			// even edges but a rectangular silhouette; intersecting a generous
			// ellipse rounds it without touching the edges. The radii are 0.75
			// of each side, which puts the edge MIDPOINTS at 0.67 of the radius
			// — inside the solid core — while the CORNERS land at 0.94 and so
			// take the fade. That is the same arithmetic that made an ellipse
			// alone impossible, used the other way round.
			canvasEl.style.setProperty('--pr-cx', px(rt.dx + rt.dw / 2));
			canvasEl.style.setProperty('--pr-cy', px(rt.dy + rt.dh / 2));
			canvasEl.style.setProperty('--pr-rx', px(rt.dw * 0.75));
			canvasEl.style.setProperty('--pr-ry', px(rt.dh * 0.75));
		}
		rt.drawn = -1;
	}

	/**
	 * Advance the reel. `u` is the prologue span's progress (0..1); anything
	 * outside that hides the layer. `alpha` is the handoff fade. Called from
	 * MovieTrack's rAF — this module owns no loop.
	 */
	export function tick(u: number, alpha: number): void {
		if (!canvasEl) return;
		// The handoff fade decides visibility past the end — the last frame
		// is HELD under the DOM machine while it takes over, so the upper
		// bound here is only a guard against painting deep into the day.
		const live = alpha > 0.001 && u >= -0.02 && u <= 1.5;
		if (live !== rt.shown) {
			rt.shown = live;
			canvasEl.style.visibility = live ? 'visible' : 'hidden';
		}
		if (!live) return;
		if (alpha !== rt.alpha) {
			rt.alpha = alpha;
			canvasEl.style.opacity = alpha >= 1 ? '1' : alpha.toFixed(3);
		}
		if (!rt.man) {
			if (!rt.failed) void loadManifest();
			return;
		}
		const n = rt.man.frames;
		const next = 1 + Math.round(Math.min(1, Math.max(0, u)) * (n - 1));
		if (next !== rt.idx) {
			rt.dir = next >= rt.idx ? 1 : -1;
			rt.idx = next;
		}
		pump();
		draw();
	}

	/** Warm the manifest and the head of the reel before it is on screen. */
	export function prime(): void {
		if (!rt.man) {
			if (!rt.failed) void loadManifest();
			return;
		}
		pump();
	}

	function draw(): void {
		const ctx = rt.ctx;
		if (!ctx) return;
		let pick = -1;
		if (rt.bmp.has(rt.idx)) pick = rt.idx;
		else {
			// Not decoded yet: show the nearest frame we do have rather than
			// a hole. Under a fast scrub this reads as a lower frame rate.
			for (let k = 1; k <= NEIGHBOUR_REACH; k++) {
				if (rt.bmp.has(rt.idx - k)) {
					pick = rt.idx - k;
					break;
				}
				if (rt.bmp.has(rt.idx + k)) {
					pick = rt.idx + k;
					break;
				}
			}
		}
		// Nothing decoded at all — keep whatever is already on the canvas.
		if (pick < 0) return;
		if (pick === rt.drawn) return;
		const bm = rt.bmp.get(pick);
		if (!bm) return;
		if (rt.letterbox) {
			ctx.fillStyle = rt.cut.surround;
			ctx.fillRect(0, 0, rt.cw, rt.ch);
		}
		ctx.drawImage(bm, rt.dx, rt.dy, rt.dw, rt.dh);
		rt.drawn = pick;
	}

	onDestroy(() => {
		for (const bm of rt.bmp.values()) bm.close();
		rt.bmp.clear();
		rt.man = null;
	});
</script>

<canvas class="pr" bind:this={canvasEl} aria-hidden="true"></canvas>

<style>
	.pr {
		position: absolute;
		inset: 0;
		/* Above the mote field and the era scrim, BELOW the camera's world:
		   the stations' whisper captions and the born station's CRT ride on
		   top of the footage, which is what makes the match cut possible. */
		z-index: 1;
		width: 100%;
		height: 100%;
		visibility: hidden;
		pointer-events: none;
		background: #000;

		/* NO HARD EDGE. The film used to run to the stage's own edges, which
		   reads as a video pasted into a page. It is inset now (see INSET) and
		   its picture dissolves into the surround over the outer sixth of its
		   own shorter side. `--pr-*` are published by layout(), in pixels,
		   because the canvas still covers the whole stage and only the drawn
		   rect moves — a percentage here would measure the wrong box. */
		--pr-fade: linear-gradient(
				to right,
				transparent var(--pr-x0, 0),
				#000 var(--pr-x1, 0),
				#000 var(--pr-x2, 100%),
				transparent var(--pr-x3, 100%)
			),
			linear-gradient(
				to bottom,
				transparent var(--pr-y0, 0),
				#000 var(--pr-y1, 0),
				#000 var(--pr-y2, 100%),
				transparent var(--pr-y3, 100%)
			),
			radial-gradient(
				var(--pr-rx, 75%) var(--pr-ry, 75%) at var(--pr-cx, 50%) var(--pr-cy, 50%),
				#000 0,
				#000 70%,
				rgba(0, 0, 0, 0.5) 88%,
				transparent 100%
			);
		-webkit-mask-image: var(--pr-fade);
		mask-image: var(--pr-fade);
		-webkit-mask-composite: source-in, source-in;
		mask-composite: intersect, intersect;
	}
</style>
