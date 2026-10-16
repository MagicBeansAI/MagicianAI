<script lang="ts">
	// The movie's centerpiece: the ambient orb, drawn live on canvas.
	//
	// The silhouette is the Swift `AuroraBlobShape` idea restated — a circle
	// whose rim rides three sine frequencies, seeded per phase so every phase
	// owns a still form and the morph between phases is real shape change,
	// not just recolouring. The scroll position picks the phase; time only
	// breathes the rim. Canvas is used HERE AND ONLY HERE (house rule: the
	// non-reactive `rt` object pattern from VoiceOrbCanvas) — every other
	// scene is DOM, because only the orb needs a render loop.
	//
	// Under reduced motion this component is never mounted: MovieTrack
	// renders a static CSS-gradient orb instead, so jsdom and screenshot
	// environments never touch canvas at all.
	import { onMount } from 'svelte';
	import { VOICE_ORB_SEQUENCE, mixStops, rgb, rgba } from './auroraPalette';

	/** Chapter-local scroll progress, 0..1 — the phase scrubber. */
	export let local = 0;
	/** Whether the chapter owns the viewport; the loop parks otherwise. */
	export let active = false;

	// armed → surge (the wake) → calm (listening) → thinking → speaking:
	// the exact native sequence a real wake walks the island through. Paint,
	// silhouette and halo all stay product-authentic; the surrounding page may
	// change theme, but the voice identity does not.
	const SEQUENCE = VOICE_ORB_SEQUENCE;

	let canvas: HTMLCanvasElement;

	// Non-reactive render state: Svelte reactivity must never retrigger the
	// draw loop — rAF owns cadence, scroll owns phase, and the two meet in
	// this plain object (the VoiceOrbCanvas pattern).
	const rt: {
		ctx: CanvasRenderingContext2D | null;
		raf: number;
		running: boolean;
		local: number;
		t: number;
	} = { ctx: null, raf: 0, running: false, local: 0, t: 0 };

	$: rt.local = local;
	$: if (active) {
		start();
	} else {
		stop();
	}

	function start(): void {
		if (rt.running || !rt.ctx) return;
		rt.running = true;
		rt.raf = requestAnimationFrame(frame);
	}

	function stop(): void {
		rt.running = false;
		if (rt.raf) cancelAnimationFrame(rt.raf);
		rt.raf = 0;
	}

	function frame(now: number): void {
		if (!rt.running || !rt.ctx) return;
		rt.t = now / 1000;
		draw(rt.ctx);
		rt.raf = requestAnimationFrame(frame);
	}

	function draw(ctx: CanvasRenderingContext2D): void {
		const { width, height } = canvas;
		ctx.clearRect(0, 0, width, height);
		const cx = width / 2;
		const cy = height / 2;
		const base = Math.min(width, height) * 0.3;

		const scaled = Math.min(0.999, Math.max(0, rt.local)) * SEQUENCE.length;
		const index = Math.floor(scaled);
		const within = scaled - index;
		const next = SEQUENCE[Math.min(index + 1, SEQUENCE.length - 1)];
		const here = SEQUENCE[index];
		const blend = Math.max(0, (within - 0.75) * 4);

		const stopA = mixStops(here.stops[0], next.stops[0], blend);
		const stopB = mixStops(
			here.stops[here.stops.length - 1],
			next.stops[next.stops.length - 1],
			blend
		);
		const halo = mixStops(here.halo, next.halo, blend);
		const haloStrength = here.haloStrength + (next.haloStrength - here.haloStrength) * blend;
		const seed = here.blobSeed + (next.blobSeed - here.blobSeed) * blend;

		// Halo first — the glow under the body, strength straight from the
		// palette knob so graphite genuinely does not glow. Two passes: a wide
		// ambient wash and a tighter hot core, so the orb reads as a light
		// source on the ground rather than a flat disc.
		if (haloStrength > 0) {
			const wash = ctx.createRadialGradient(cx, cy, base * 0.3, cx, cy, base * 2.4);
			wash.addColorStop(0, rgba(halo, 0.55 * haloStrength));
			wash.addColorStop(0.55, rgba(halo, 0.18 * haloStrength));
			wash.addColorStop(1, rgba(halo, 0));
			ctx.fillStyle = wash;
			ctx.fillRect(0, 0, width, height);
			const core = ctx.createRadialGradient(cx, cy, base * 0.6, cx, cy, base * 1.25);
			core.addColorStop(0, rgba(halo, 0.35 * haloStrength));
			core.addColorStop(1, rgba(halo, 0));
			ctx.fillStyle = core;
			ctx.fillRect(0, 0, width, height);
		}

		// The rim: three sine frequencies, phase-seeded, gently breathing.
		const t = rt.t;
		ctx.beginPath();
		const STEPS = 120;
		for (let i = 0; i <= STEPS; i++) {
			const theta = (i / STEPS) * Math.PI * 2;
			const wobble =
				0.055 * Math.sin(3 * theta + seed * 2 + t * 0.7) +
				0.035 * Math.sin(5 * theta - seed * 3 - t * 0.9) +
				0.02 * Math.sin(7 * theta + seed + t * 0.5);
			const r = base * (1 + wobble);
			const x = cx + Math.cos(theta) * r;
			const y = cy + Math.sin(theta) * r;
			if (i === 0) ctx.moveTo(x, y);
			else ctx.lineTo(x, y);
		}
		ctx.closePath();
		// Body: an off-centre radial — lit from the upper left like a product
		// shot, falling to the deeper stop at the rim.
		const body = ctx.createRadialGradient(
			cx - base * 0.35,
			cy - base * 0.4,
			base * 0.1,
			cx,
			cy,
			base * 1.25
		);
		body.addColorStop(0, rgb(mixStops(stopA, { r: 1, g: 1, b: 1 }, 0.28)));
		body.addColorStop(0.45, rgb(stopA));
		body.addColorStop(1, rgb(stopB));
		ctx.fillStyle = body;
		ctx.fill();

		// Rim light: the halo hue, not hard white — a lit edge, not an outline.
		ctx.strokeStyle = rgba(mixStops(halo, { r: 1, g: 1, b: 1 }, 0.45), 0.5);
		ctx.lineWidth = Math.max(1, base * 0.025);
		ctx.stroke();

		// The leash ring: the hard cap as a depleting arc — honest motion,
		// because it depicts the chapter's own remaining scroll.
		const remaining = 1 - rt.local;
		if (remaining > 0.001) {
			ctx.beginPath();
			ctx.arc(cx, cy, base * 1.45, -Math.PI / 2, -Math.PI / 2 + Math.PI * 2 * remaining);
			ctx.strokeStyle = rgba(halo, Math.max(0.25, haloStrength) * 0.8);
			ctx.lineWidth = Math.max(1.5, base * 0.03);
			ctx.stroke();
		}
	}

	onMount(() => {
		// jsdom returns null here; the guard is what keeps this component
		// harmless anywhere without a real 2D context.
		rt.ctx = canvas.getContext('2d');
		const fit = (): void => {
			const parent = canvas.parentElement;
			if (!parent) return;
			const dpr = Math.min(2, window.devicePixelRatio || 1);
			canvas.width = parent.clientWidth * dpr;
			canvas.height = parent.clientHeight * dpr;
		};
		fit();
		window.addEventListener('resize', fit, { passive: true });
		if (active) start();
		return () => {
			stop();
			window.removeEventListener('resize', fit);
		};
	});
</script>

<canvas bind:this={canvas} aria-hidden="true"></canvas>

<style>
	canvas {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		/* The halo wash fills the canvas rect; without a feathered mask the
		   rect's edges print as seams against the vignette. */
		-webkit-mask-image: radial-gradient(closest-side, black 76%, transparent 100%);
		mask-image: radial-gradient(closest-side, black 76%, transparent 100%);
	}
</style>
