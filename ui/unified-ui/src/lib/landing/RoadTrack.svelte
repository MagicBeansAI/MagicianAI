<script lang="ts">
	// THE ROADS' ENGINE — the film's grammar, at a road's scale.
	//
	// It deliberately does NOT extract MovieTrack. The film's track carries a
	// reel, a match cut, an era scrim, a mote field, an aurora thread on two
	// canvases and a machine that ages; a road needs one sticky stage, a
	// camera that flies between twelve stations, and depth. Generalising the
	// film to cover both would have made the harder thing carry the easier
	// one's weight for no gain — this is the same PATTERN, stated at the size
	// the roads actually are.
	//
	// It takes no station markup of its own. Whatever is slotted in is walked
	// for `[data-station]` elements, and those are placed in the world in
	// document order — so `PathActSign` and `PathBeat` render exactly as they
	// always have and simply stop being in document flow.
	//
	// Reduced motion: no track height, no positioning, no listener. The
	// stations stay in flow and the road is the document it has always been,
	// which is also the accessibility contract the film keeps.
	import { onDestroy, onMount } from 'svelte';
	import { clamp, createScrubber, resolveScene } from './scrub';
	import { roadBounds, roadStations, roadTrackSvh, roadTravelStart } from './roadTrack';

	export let still = false;

	/**
	 * Externally-driven progress, 0..1. When supplied, this road stops owning
	 * its own scroll: no tall track of its own, no scrubber action, no sticky
	 * stage — a HOST above it provides all three and simply hands down where
	 * the camera should be.
	 *
	 * That exists because the landing page needs the promise stations to play
	 * INSIDE the hero's own pinned section rather than in a section after it.
	 * Two adjacent sticky tracks always read as two places, however carefully
	 * their backdrops are matched: the first unpins and scrolls away, the
	 * second pins. One pinned stage that changes what it holds reads as one
	 * place, which is what was asked for.
	 *
	 * Null keeps the original behaviour exactly — PathFork's own roads still
	 * mount this standalone and still own their scroll.
	 */
	export let progress: number | null = null;


	/** PathPromise titles its acts from the fixed chrome layer, so it builds
	 *  a road with no sign stops. See roadStations(). */
	export let signs = true;
	const stations = roadStations(3, 3, signs);
	const BOUNDS = roadBounds(stations);
	const TRAVEL = roadTravelStart(stations);
	const TRACK_SVH = roadTrackSvh(stations);

	const scrub = createScrubber(BOUNDS);
	const state = scrub.state;
	$: driven = progress !== null;
	$: p = still ? 0 : driven ? clamp(progress ?? 0, 0, 1) : $state.p;

	let trackEl: HTMLElement | null = null;
	let worldEl: HTMLElement | null = null;
	let els: HTMLElement[] = [];
	let w = 0;
	let h = 0;

	function collect(): void {
		if (!worldEl) return;
		els = [...worldEl.querySelectorAll<HTMLElement>('[data-station]')];
	}

	function measure(): void {
		const root = trackEl?.closest<HTMLElement>('[data-scrub-root]') ?? null;
		w = root ? root.clientWidth : window.innerWidth;
		h = root ? root.clientHeight : window.innerHeight;
	}

	const easeInOutCubic = (t: number): number =>
		t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;

	/** Camera, in px, from the same waypoint-dolly rule the film uses. */
	function camera(prog: number): { x: number; y: number; z: number } {
		const { scene, local } = resolveScene(BOUNDS, prog);
		const px = (s: (typeof stations)[number]) => ({ x: (s.x / 100) * w, y: (s.y / 100) * h });
		const cur = px(stations[scene]);
		const ts = TRAVEL[scene];
		if (scene >= stations.length - 1 || local <= ts) {
			// The dwell push only exists where there is a leg to spend it —
			// the same rule the film learned the hard way, where a station
			// that never departs snapped back 5% at every hand-over.
			const d = ts >= 1 ? 0 : ts > 0 ? Math.min(1, local / ts) : 1;
			return { ...cur, z: 1 + 0.04 * easeInOutCubic(d) };
		}
		const nxt = px(stations[scene + 1]);
		const t = (local - ts) / (1 - ts);
		const e = easeInOutCubic(t);
		return {
			x: cur.x + (nxt.x - cur.x) * e,
			y: cur.y + (nxt.y - cur.y) * e,
			z: 1.04 + (1 - 1.04) * t - 0.14 * Math.sin(Math.PI * t)
		};
	}

	/** Station positions in px — computed on resize, NOT per frame. */
	let pts: { x: number; y: number }[] = [];

	function layout(): void {
		pts = stations.map((s) => ({ x: (s.x / 100) * w, y: (s.y / 100) * h }));
		if (still) return;
		// LEFT/TOP ARE WRITTEN ONCE. They were being set every frame, and
		// `left`/`top` are LAYOUT properties: twelve of them per frame forced
		// a reflow of the whole road sixty times a second, which is exactly
		// the stutter this beat had. Only transform, opacity and filter move
		// now, and all three are compositor work.
		for (let i = 0; i < els.length; i++) {
			const q = pts[i];
			if (!q || !els[i]) continue;
			els[i].style.left = `${q.x.toFixed(1)}px`;
			els[i].style.top = `${q.y.toFixed(1)}px`;
		}
	}

	function place(): void {
		if (still || !worldEl || els.length === 0 || w === 0) return;
		const cam = camera(p);
		// TWO DIFFERENT CLOCKS, and conflating them was why every card on
		// every road appeared fully assembled. Proximity below is a DISTANCE —
		// and the camera does not approach a station gradually, it arrives and
		// then holds for the whole dwell, so proximity is pinned at 1 for
		// roughly nine tenths of a station's scroll. Anything keyed to it has
		// no ramp to animate along; it snaps on during the last stretch of the
		// approach and is finished before the card is even readable.
		//
		// A card's contents belong to the second clock: how far through its
		// OWN dwell the visitor is. That is what writes the rows in, counts
		// the meters and fills the wells, and it is the clock PathBeat's
		// reveal formulas were always written against.
		const rs = resolveScene(BOUNDS, p);
		// SCALE OUTSIDE THE TRANSLATE, and the order is the whole point.
		//
		// Transform functions apply right to left, so `translate(-cam) scale(z)`
		// scaled the world about its own origin FIRST and then moved it, which
		// leaves the camera's own point at `cam · (z - 1)` rather than at the
		// centre of the frame. That is not a rounding error: the dwell push
		// takes z from 1 to 1.04, and by the ninth station `cam.y` is past
		// 8000px, so every card slid a third of a viewport DOWNWARD while you
		// stood still reading it — carrying the caption underneath it off the
		// bottom of the frame. It also meant the DOM stations and the canvas
		// thread disagreed, since `drawThread` has always mapped
		// `(v - cam) * z` correctly.
		//
		// Translating first makes the camera point the fixed point of the
		// scale, so z becomes a pure, gentle zoom about the centre of the
		// frame — which is what a dwell push was always meant to be.
		worldEl.style.transform = `scale(${cam.z.toFixed(4)}) translate3d(${(-cam.x).toFixed(
			1
		)}px, ${(-cam.y).toFixed(1)}px, 0)`;
		for (let i = 0; i < els.length; i++) {
			const q = pts[i];
			if (!q || !els[i]) continue;
			const el = els[i];
			// Depth of field: a station sharpens and lifts as the camera
			// closes on it and recedes as it leaves — which is the whole
			// difference between "a scroll past cards" and "a journey".
			const d = Math.hypot((q.x - cam.x) / (w * 0.9), (q.y - cam.y) / (h * 0.85));
			const prox = Math.max(0, 1 - d);
			const e = prox * prox * (3 - 2 * prox);
			el.style.opacity = (0.06 + 0.94 * e).toFixed(3);
			el.style.transform = `translate(-50%, -50%) scale(${(0.9 + 0.1 * e).toFixed(4)})`;
			el.style.filter = e > 0.95 ? 'none' : `blur(${((1 - e) * 3).toFixed(2)}px)`;
			// The dwell clock. A station behind the camera stays assembled, one
			// ahead of it is still blank, and the current one writes itself
			// over the span the camera is actually parked in front of it.
			const ts = TRAVEL[i];
			const rv =
				i < rs.scene ? 1 : i > rs.scene ? 0 : ts > 0 ? Math.min(1, rs.local / ts) : 1;
			// The beat's own reveal formulas already speak `--local`; giving
			// each station its own keeps PathBeat working verbatim.
			el.style.setProperty('--local', rv.toFixed(3));
		}
		drawThread(cam);
	}

	$: if (p >= 0) place();

	function reset(): void {
		for (const el of els) {
			el.style.cssText = '';
			el.style.removeProperty('--local');
		}
		if (worldEl) worldEl.style.transform = '';
	}

	// ── THE THREAD, and the rail that shows where you are ────────────────
	//
	// The film's protagonist is an aurora thread that walks its whole world,
	// and a road without one is visibly a lesser thing — the same stations,
	// none of the continuity. This is the film's stroke recipe at a road's
	// scale: one canvas, a spline through the stations, a bloomed head at the
	// playhead. No second loop — `place()` already runs once per scroll frame
	// and paints this at the end of it.
	let canvasEl: HTMLCanvasElement | null = null;
	let ctx: CanvasRenderingContext2D | null = null;
	let cw = 0;
	let ch = 0;

	function fitCanvas(): void {
		if (!canvasEl) return;
		const r = canvasEl.getBoundingClientRect();
		if (r.width === 0) return;
		const dpr = Math.min(2, window.devicePixelRatio || 1);
		cw = r.width;
		ch = r.height;
		canvasEl.width = Math.round(r.width * dpr);
		canvasEl.height = Math.round(r.height * dpr);
		ctx = canvasEl.getContext('2d');
		ctx?.setTransform(dpr, 0, 0, dpr, 0, 0);
	}

	/** The thread's point at progress `t` across the whole road, in world px.
	 *  A Catmull-Rom through the stations, so it BENDS through them rather
	 *  than hinging — a polyline reads as a diagram, a spline as a path. */
	function threadAt(t: number): { x: number; y: number } {
		if (pts.length < 2) return { x: 0, y: 0 };
		const u = Math.max(0, Math.min(1, t)) * (pts.length - 1);
		const i = Math.min(pts.length - 2, Math.floor(u));
		const f = u - i;
		const p0 = pts[Math.max(0, i - 1)];
		const p1 = pts[i];
		const p2 = pts[i + 1];
		const p3 = pts[Math.min(pts.length - 1, i + 2)];
		const cr = (a: number, b: number, c: number, d: number): number =>
			0.5 *
			(2 * b + (-a + c) * f + (2 * a - 5 * b + 4 * c - d) * f * f + (-a + 3 * b - 3 * c + d) * f ** 3);
		return { x: cr(p0.x, p1.x, p2.x, p3.x), y: cr(p0.y, p1.y, p2.y, p3.y) };
	}

	function drawThread(cam: { x: number; y: number; z: number }): void {
		if (!ctx || cw === 0) return;
		ctx.clearRect(0, 0, cw, ch);
		const toScreen = (v: { x: number; y: number }) => ({
			x: cw / 2 + (v.x - cam.x) * cam.z,
			y: ch / 2 + (v.y - cam.y) * cam.z
		});
		// A TRAILING COMET, not a drawn line: the head at the playhead and a
		// fixed arc behind it, so the thread reads as one travelling object
		// rather than as a diagram of the route.
		const TAIL = 0.16;
		const K = 44;
		const a = Math.max(0, p - TAIL);
		const path: { x: number; y: number }[] = [];
		for (let i = 0; i <= K; i++) path.push(toScreen(threadAt(a + (p - a) * (i / K))));
		if (path.length < 2) return;
		// Four passes, widest and faintest first — the film's own recipe for
		// a stroke that glows rather than one that is merely coloured.
		for (const [width, alpha] of [
			[22, 0.05],
			[10, 0.1],
			[4, 0.3],
			[1.6, 0.85]
		] as [number, number][]) {
			ctx.beginPath();
			ctx.moveTo(path[0].x, path[0].y);
			for (let i = 1; i < path.length; i++) ctx.lineTo(path[i].x, path[i].y);
			ctx.strokeStyle = `rgba(94, 214, 178, ${alpha})`;
			ctx.lineWidth = width;
			ctx.lineCap = 'round';
			ctx.lineJoin = 'round';
			ctx.stroke();
		}
		const head = path[path.length - 1];
		const glow = ctx.createRadialGradient(head.x, head.y, 0, head.x, head.y, 26);
		glow.addColorStop(0, 'rgba(120, 236, 200, 0.9)');
		glow.addColorStop(1, 'rgba(120, 236, 200, 0)');
		ctx.fillStyle = glow;
		ctx.beginPath();
		ctx.arc(head.x, head.y, 26, 0, Math.PI * 2);
		ctx.fill();
	}

	/** The rail: where you are in the road, and how much is left. */
	const RAIL_H = 210;
	$: railPts = stations.map((s, i) => ({
		kind: s.kind,
		y: (i / Math.max(1, stations.length - 1)) * RAIL_H,
		x: 6 + (s.x / 13) * 5
	}));
	$: railPath = railPts
		.map((q, i) => `${i === 0 ? 'M' : 'L'} ${q.x.toFixed(1)} ${q.y.toFixed(1)}`)
		.join(' ');
	$: railHead = (() => {
		const u = Math.max(0, Math.min(1, p)) * (railPts.length - 1);
		const i = Math.min(railPts.length - 2, Math.floor(u));
		const f = u - i;
		const A = railPts[i];
		const B = railPts[i + 1] ?? A;
		return { x: A.x + (B.x - A.x) * f, y: A.y + (B.y - A.y) * f };
	})();

	let onResize: (() => void) | null = null;

	onMount(() => {
		collect();
		measure();
		fitCanvas();
		layout();
		place();
		onResize = () => {
			measure();
			fitCanvas();
			layout();
			place();
		};
		window.addEventListener('resize', onResize, { passive: true });
	});

	// Flipping the preference mid-session has to put the road back in flow
	// rather than leave twelve absolutely-positioned stations stacked.
	$: if (still) reset();

	onDestroy(() => {
		if (typeof window !== 'undefined' && onResize) window.removeEventListener('resize', onResize);
	});
</script>

<div
	class="rt-track"
	class:still
	class:driven
	style={still || driven ? '' : `height:${TRACK_SVH}svh`}
	use:scrub.track
	bind:this={trackEl}
>
	<div class="rt-stage" class:still class:driven>
		{#if !still}
			<!-- A SIBLING of the world, not a child: the world carries the
			     camera's own transform, and a canvas inside it would be moved
			     twice — once by that transform and once by its own
			     projection. -->
			<canvas class="rt-thread" bind:this={canvasEl} aria-hidden="true"></canvas>
		{/if}
		<div class="rt-world" class:still bind:this={worldEl}>
			<slot />
		</div>
		{#if !still}
			<nav class="rt-rail" aria-label="Progress through this road">
				<svg viewBox="-4 -6 20 {RAIL_H + 12}" width="20" height={RAIL_H + 12}>
					<path d={railPath} fill="none" stroke="currentColor" stroke-width="0.7" opacity="0.28" />
					{#each railPts as q, i (i)}
						<circle
							cx={q.x}
							cy={q.y}
							r={q.kind === 'sign' ? 2.6 : 1.7}
							fill={q.kind === 'sign' ? 'currentColor' : 'none'}
							stroke="currentColor"
							stroke-width="0.7"
							opacity={q.kind === 'sign' ? 0.6 : 0.4}
						/>
					{/each}
					<circle cx={railHead.x} cy={railHead.y} r="3.4" class="rt-rail-head" />
				</svg>
			</nav>
		{/if}
	</div>
</div>

<style>
	.rt-track {
		position: relative;
	}
	.rt-track.still {
		height: auto;
	}
	.rt-stage {
		position: sticky;
		top: 0;
		height: 100svh;
		overflow: hidden;
	}
	/* Driven from outside: the host owns the pin and the scroll room, so this
	   fills its host rather than pinning again. A sticky box inside an
	   already-pinned stage has nothing left to stick to and would only add a
	   second containing block for the camera to fight. */
	/* `height: 100svh`, NOT `100%`. The host wrapper is positioned with
	   `inset: 0` and has no explicit height of its own, so a percentage here
	   has no definite parent to resolve against and collapses to zero — which
	   it did: the stage measured 0 tall and the camera, which centres against
	   the stage box, put every station a few hundred pixels above the
	   viewport. The cards were all rendering correctly and simply painting
	   off-screen. Viewport units side-step the whole resolution question and
	   give the camera exactly the box it gets standing alone. */
	.rt-track.driven {
		position: absolute;
		inset: 0;
		height: 100svh;
	}
	.rt-stage.driven {
		position: absolute;
		inset: 0;
		height: 100svh;
	}
	.rt-stage.still {
		position: static;
		height: auto;
		overflow: visible;
	}
	/* The world is a point the camera moves; its children are placed around
	   that point in px by `place()`. */
	.rt-world {
		position: absolute;
		left: 50%;
		top: 50%;
		width: 0;
		height: 0;
		z-index: 2;
		will-change: transform;
	}
	.rt-stage.still .rt-world {
		position: static;
		width: auto;
		height: auto;
		will-change: auto;
	}
	/* A station only leaves document flow when the track is live. `:global`
	   because the stations are slotted content — PathActSign and PathBeat own
	   their own markup and know nothing about this engine. */
	.rt-world:not(.still) :global([data-station]) {
		position: absolute;
		width: min(92vw, 62rem);
	}
	/* The acts are grouping markup, not layout: flattened so their signs and
	   beats are the world's own children in document order. */
	.rt-world:not(.still) :global(.pa-act) {
		display: contents;
	}

	/* UNDER the stations, not over them. Painted on top it drew a bright line
	   straight through whatever card the camera had stopped on — the thread
	   is the road these beats stand on, not something crossing in front of
	   them. */
	.rt-thread {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		z-index: 1;
		pointer-events: none;
	}
	/* The rail. Signs are filled and larger, beats hollow — so the three act
	   breaks are legible at a glance and the road's shape is readable before
	   it has been walked. */
	.rt-rail {
		position: absolute;
		right: clamp(0.8rem, 2.2vw, 2rem);
		top: 50%;
		transform: translateY(-50%);
		z-index: 4;
		color: var(--pf-ink, var(--text-primary));
		pointer-events: none;
	}
	.rt-rail-head {
		fill: var(--accent-primary, #5ed6b2);
		filter: drop-shadow(0 0 6px var(--accent-primary, #5ed6b2));
	}
</style>
