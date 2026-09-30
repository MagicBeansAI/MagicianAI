<script lang="ts">
	/**
	 * VOICE ORB — the deck's living centrepiece, drawn inside the ring assembly.
	 *
	 * A live orb driven by REAL microphone amplitude, read from the
	 * `AnalyserNode` the realtime voice client publishes on
	 * `voiceMicAnalyser`. Nothing here is simulated: when the mic is closed the
	 * core rests as a still ring and says VOICE OFFLINE. A writhing orb over a
	 * closed microphone is precisely the stub-derived motion this deck was
	 * rebuilt to remove.
	 *
	 * RENDERED TO CANVAS, DELIBERATELY. The amplitude loop runs at display rate
	 * and must never write Svelte state — the small `VoiceOrb` badge does that
	 * and is fine at 40px, but assigning reactive state 60×/second on a
	 * full-stage element re-runs the component's effects at that rate. Canvas
	 * keeps every frame off the reactive graph: the ONLY Svelte writes here are
	 * store-driven stage changes, which happen at conversational speed.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';

	import {
		voiceCallStore,
		voiceMicAnalyser,
		voiceTranscriptStore
	} from '$lib/media/voice/realtimeVoiceClient';
	import {
		amplitudeToDisplay,
		deriveVoiceStage,
		followEnvelope,
		pulseRings,
		organicRadialWaveform,
		pushHistory,
		rmsFromTimeDomain,
		stageIsLive,
		type VoiceStage
	} from './voiceViz';

	/** Externally forced stage — dictation has no call, but the orb must
	 *  still go green and move with the mic while recording. */
	export let overrideStage: VoiceStage | null = null;

	$: call = $voiceCallStore;
	$: transcript = $voiceTranscriptStore;
	$: stage =
		overrideStage ??
		deriveVoiceStage({
			callState: call.state,
			error: call.error,
			userSpeaking: transcript.userSpeaking,
			assistantSpeaking: transcript.assistantSpeaking
		});
	let canvas: HTMLCanvasElement | null = null;
	let shell: HTMLElement | null = null;
	let resizeObserver: ResizeObserver | null = null;

	// ── non-reactive render state ────────────────────────────────────────
	// Plain `let`s written every frame WOULD be reactive in Svelte, so all
	// per-frame mutable state lives on this object instead. Mutating an object
	// property is untracked, which is what keeps the render loop off the
	// reactive graph entirely.
	const rt = {
		envelope: 0,
		history: [] as number[],
		buffer: null as Uint8Array<ArrayBuffer> | null,
		analyser: null as AnalyserNode | null,
		stage: 'offline' as VoiceStage,
		colors: { glow: '#7fd7ff', bg: '#05070a', dim: '#5a6b7a', text: '#dff1ff' },
		colorAge: 0,
		w: 0,
		h: 0,
		dpr: 1,
		phase: 0,
		/** Seconds clock for the organic term; advanced per frame. */
		time: 0,
		lastFrameMs: 0,
		/** rAF handle. MUST live here, not in a reactive `let`: it is assigned
		 *  every frame, and any reactive statement reading it would then re-run
		 *  at display rate -- which is exactly what pinned the main thread. */
		raf: null as number | null
	};

	$: rt.stage = stage;
	$: rt.analyser = $voiceMicAnalyser;

	const reducedMotion = (): boolean =>
		browser && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

	/**
	 * Resolve a CSS custom property to a concrete colour.
	 *
	 * Canvas cannot be trusted with the raw token: several deck variables are
	 * `color-mix(...)` expressions. Assigning the expression to a probe
	 * element's `color` and reading the computed value back returns a plain
	 * `rgb()` the 2D context always understands, and it follows theme changes
	 * for free.
	 */
	function resolveColors(): void {
		if (!shell || !browser) return;
		const probe = document.createElement('span');
		probe.style.cssText = 'position:absolute;visibility:hidden;pointer-events:none';
		shell.appendChild(probe);
		const read = (expr: string, fallback: string): string => {
			probe.style.color = '';
			probe.style.color = expr;
			const value = getComputedStyle(probe).color;
			return value && value !== 'rgba(0, 0, 0, 0)' ? value : fallback;
		};
		rt.colors = {
			glow: read(stageColorExpr(rt.stage), '#7fd7ff'),
			bg: read('var(--deck-bg, var(--bg-base))', '#05070a'),
			dim: read('var(--deck-dim, var(--text-secondary))', '#5a6b7a'),
			text: read('var(--deck-text, var(--text-primary))', '#dff1ff')
		};
		probe.remove();
	}

	function stageColorExpr(s: VoiceStage): string {
		switch (s) {
			case 'error':
				return 'var(--color-error, #ff5f56)';
			case 'connecting':
				return 'var(--color-warning, #ffb454)';
			case 'you':
				return 'var(--color-success, #5fd08a)';
			case 'offline':
				return 'var(--deck-dim, var(--text-secondary))';
			case 'listening':
				// Dimmed accent: the turn colours (green = you, full glow =
				// agent) only read as a code if the resting state is visibly
				// quieter than both.
				return 'color-mix(in srgb, var(--deck-glow, var(--accent-primary)) 55%, var(--deck-bg, #05070a))';
			default:
				return 'var(--deck-glow, var(--accent-primary))';
		}
	}

	function sizeCanvas(): void {
		if (!canvas || !shell) return;
		const rect = shell.getBoundingClientRect();
		// Cap DPR at 2: a 3x retina buffer on a large stage costs real fill rate
		// for no perceptible gain on a glow-heavy render.
		rt.dpr = Math.min(2, browser ? window.devicePixelRatio || 1 : 1);
		rt.w = Math.max(1, Math.floor(rect.width));
		rt.h = Math.max(1, Math.floor(rect.height));
		canvas.width = Math.floor(rt.w * rt.dpr);
		canvas.height = Math.floor(rt.h * rt.dpr);
		canvas.style.width = `${rt.w}px`;
		canvas.style.height = `${rt.h}px`;
		draw();
	}

	function readAmplitude(): number {
		const analyser = rt.analyser;
		if (!analyser || !stageIsLive(rt.stage)) return 0;
		if (!rt.buffer || rt.buffer.length !== analyser.fftSize) {
			// Explicit ArrayBuffer: the WebAudio typings narrowed in TS 5.7+ to
			// reject SharedArrayBuffer-backed views.
			rt.buffer = new Uint8Array(new ArrayBuffer(analyser.fftSize));
		}
		analyser.getByteTimeDomainData(rt.buffer);
		return amplitudeToDisplay(rmsFromTimeDomain(rt.buffer));
	}

	function draw(): void {
		if (!canvas) return;
		const ctx = canvas.getContext('2d');
		if (!ctx) return;

		const { w, h, dpr, colors } = rt;
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.clearRect(0, 0, w, h);

		const cx = w / 2;
		const cy = h / 2;
		const unit = Math.min(w, h) / 2;
		const base = unit * 0.42;
		const amp = rt.envelope;
		const isLive = stageIsLive(rt.stage);

		// ── pulse rings: positions ARE the recent amplitude history ──────
		for (const ring of pulseRings(rt.history, 5, base * 1.28, unit * 0.09)) {
			ctx.beginPath();
			ctx.arc(cx, cy, ring.radius, 0, Math.PI * 2);
			ctx.strokeStyle = withAlpha(colors.glow, ring.alpha * (isLive ? 0.55 : 0.14));
			ctx.lineWidth = 1;
			ctx.stroke();
		}

		// ── organic waveform ribbon ──────────────────────────────────────
		// Alive = organic: layered integer-frequency noise whose gain tracks
		// the measured envelope, plus the real audio term. Offline = a plain
		// still ring (empty buffer, zero envelope path below).
		const POINTS = 168;
		const wave = isLive
			? organicRadialWaveform(rt.buffer, POINTS, base, unit * 0.2 * (0.3 + amp), rt.time, amp)
			: organicRadialWaveform(null, POINTS, base, 0, 0, 0);
		ctx.beginPath();
		for (let i = 0; i < POINTS; i += 1) {
			// Rotate slowly so the ribbon reads as a living surface rather than a
			// frozen scan. Speed tracks amplitude, so it is still a readout.
			const a = (i / POINTS) * Math.PI * 2 + rt.phase;
			const r = wave[i];
			const x = cx + Math.cos(a) * r;
			const y = cy + Math.sin(a) * r;
			if (i === 0) ctx.moveTo(x, y);
			else ctx.lineTo(x, y);
		}
		ctx.closePath();
		ctx.strokeStyle = withAlpha(colors.glow, isLive ? 0.85 : 0.3);
		ctx.lineWidth = 1.5;
		ctx.stroke();

		// Soft fill inside the ribbon
		const grad = ctx.createRadialGradient(cx, cy, base * 0.1, cx, cy, base * 1.3);
		grad.addColorStop(0, withAlpha(colors.glow, 0.22 + amp * 0.3));
		grad.addColorStop(1, withAlpha(colors.glow, 0));
		ctx.fillStyle = grad;
		ctx.fill();

		// ── core disc ────────────────────────────────────────────────────
		const coreR = base * (0.42 + amp * 0.22);
		ctx.beginPath();
		ctx.arc(cx, cy, coreR, 0, Math.PI * 2);
		ctx.fillStyle = withAlpha(colors.glow, 0.28 + amp * 0.45);
		ctx.fill();
		ctx.beginPath();
		ctx.arc(cx, cy, coreR, 0, Math.PI * 2);
		ctx.strokeStyle = withAlpha(colors.glow, 0.95);
		ctx.lineWidth = 1.5;
		ctx.stroke();

		// ── resting state is EXPLICIT, not empty ─────────────────────────
		if (!isLive) {
			ctx.beginPath();
			ctx.arc(cx, cy, base * 1.02, 0, Math.PI * 2);
			ctx.setLineDash([2, 9]);
			ctx.strokeStyle = withAlpha(colors.dim, 0.75);
			ctx.lineWidth = 2;
			ctx.stroke();
			ctx.setLineDash([]);
		}
	}

	/** rgb(…) / rgba(…) → rgba with the given alpha. */
	function withAlpha(color: string, alpha: number): string {
		const a = Math.max(0, Math.min(1, alpha));
		const m = color.match(/rgba?\(([^)]+)\)/);
		if (!m) return color;
		const [r, g, b] = m[1].split(',').map((v) => parseFloat(v));
		return `rgba(${r}, ${g}, ${b}, ${a})`;
	}

	function frame(): void {
		const nowMs = performance.now();
		const dt = rt.lastFrameMs > 0 ? Math.min(0.1, (nowMs - rt.lastFrameMs) / 1000) : 0.016;
		rt.lastFrameMs = nowMs;
		// The organic clock runs faster while someone speaks — the surface
		// literally moves at the pace of the audio driving it.
		const target = readAmplitude();
		rt.time += dt * (0.6 + rt.envelope * 2.4);
		rt.envelope = followEnvelope(rt.envelope, target);
		rt.history = pushHistory(rt.history, rt.envelope);
		rt.phase += 0.0016 + rt.envelope * 0.012;
		rt.colorAge += 1;
		// Re-resolve theme colours ~1×/second so a theme switch is picked up
		// without a getComputedStyle (a forced style recalc) every frame.
		if (rt.colorAge > 60) {
			rt.colorAge = 0;
			resolveColors();
		}
		draw();
		rt.raf = requestAnimationFrame(frame);
	}

	// Stage changes recolour immediately rather than waiting for the next
	// 1s sweep — the operator should see FAULT the instant it happens.
	// Recolour immediately on a stage change so a FAULT is visible at once.
	// Deliberately does NOT read `rt.raf` -- reading per-frame state here is
	// what caused the hang; an extra `draw()` while the loop runs is harmless.
	$: if (browser && stage) {
		resolveColors();
		draw();
	}

	onMount(() => {
		if (!browser) return;
		resolveColors();
		sizeCanvas();
		if (shell && 'ResizeObserver' in window) {
			resizeObserver = new ResizeObserver(() => sizeCanvas());
			resizeObserver.observe(shell);
		}
		// Reduced motion: render the true current state once and on every stage
		// change, but never animate. Nothing is lost -- the label and colour
		// already carry the reading.
		if (!reducedMotion()) rt.raf = requestAnimationFrame(frame);
	});

	onDestroy(() => {
		if (rt.raf !== null) cancelAnimationFrame(rt.raf);
		rt.raf = null;
		resizeObserver?.disconnect();
		resizeObserver = null;
		rt.buffer = null;
	});
</script>

<div class="orb-shell" bind:this={shell} data-stage={stage}>
	<canvas bind:this={canvas} aria-hidden="true"></canvas>
</div>

<style>
	.orb-shell {
		position: relative;
		width: 100%;
		height: 100%;
		display: grid;
		place-items: center;
	}

	canvas { position: absolute; inset: 0; width: 100%; height: 100%; }




</style>
