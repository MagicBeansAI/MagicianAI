<script lang="ts">
	/**
	 * Voice state visualization — a glowing orb with state-driven animations.
	 *
	 * States:
	 *   - idle       — soft slow breath (gentle baseline pulse)
	 *   - listening  — animated gradient sweep (system ready, waiting for user
	 *                  to start talking)
	 *   - capturing  — live audio-reactive scale + glow (driven by `analyser`
	 *                  if provided; falls back to a tighter listening animation
	 *                  when no analyser is wired)
	 *   - playing    — gentle TTS-side pulse (system is speaking back)
	 *
	 * The orb is purely visual chrome — it doesn't own audio, permissions, or
	 * connection state. Consumers (MicCaptureButton, VoiceCallButton,
	 * SpeakButton) decide which state to render and optionally pass a Web
	 * Audio `AnalyserNode` for live amplitude modulation.
	 *
	 * Renders the same way at every viewport — `size` is a px prop so callers
	 * can scale to their layout context (32px inline button, 56px composer
	 * highlight, 96px modal centerpiece).
	 */
	import { onDestroy } from 'svelte';

	export let state: 'idle' | 'listening' | 'capturing' | 'playing' = 'idle';
	export let analyser: AnalyserNode | null = null;
	export let size: number = 40;

	// Live scale + glow intensity (0..1) driven by the analyser when state is
	// `capturing`. Lerped towards target each frame so it doesn't jitter on
	// every micro-amplitude change.
	let intensity = 0;
	let scale = 1;
	let raf: number | null = null;
	// Backed by an explicit ArrayBuffer (not ArrayBufferLike) so
	// `getByteFrequencyData` accepts it — the WebAudio API typings
	// narrowed in TS 5.7+ to disallow SharedArrayBuffer-backed views.
	let buffer: Uint8Array<ArrayBuffer> | null = null;

	$: if (typeof window !== 'undefined') {
		if (analyser && state === 'capturing') startLoop();
		else stopLoop();
	}

	function startLoop(): void {
		if (raf !== null || !analyser) return;
		buffer = new Uint8Array(new ArrayBuffer(analyser.frequencyBinCount));
		const tick = (): void => {
			if (!analyser || !buffer) return;
			analyser.getByteFrequencyData(buffer);
			// RMS-ish average across all bins (0..255 each)
			let sum = 0;
			for (let i = 0; i < buffer.length; i++) sum += buffer[i] * buffer[i];
			const rms = Math.sqrt(sum / buffer.length) / 255;
			// Lerp towards target so quiet→loud transitions feel smooth.
			const target = Math.min(1, rms * 2.4);
			intensity += (target - intensity) * 0.28;
			scale = 1 + intensity * 0.45;
			raf = requestAnimationFrame(tick);
		};
		raf = requestAnimationFrame(tick);
	}

	function stopLoop(): void {
		if (raf !== null) cancelAnimationFrame(raf);
		raf = null;
		buffer = null;
		intensity = 0;
		scale = 1;
	}

	onDestroy(stopLoop);

	$: glowAlpha = Math.min(0.65, 0.2 + intensity * 0.6);
</script>

<div
	class="orb orb--{state}"
	style="--orb-size: {size}px; --orb-scale: {scale}; --orb-glow-alpha: {glowAlpha};"
	role="img"
	aria-label={
		state === 'capturing' ? 'Recording audio' :
		state === 'playing' ? 'Playing audio' :
		state === 'listening' ? 'Listening' :
		'Voice idle'
	}
>
	<span class="orb__halo" aria-hidden="true"></span>
	<span class="orb__core" aria-hidden="true"></span>
	<span class="orb__sheen" aria-hidden="true"></span>
</div>

<style>
	.orb {
		position: relative;
		display: inline-block;
		width: var(--orb-size);
		height: var(--orb-size);
		flex: 0 0 auto;
	}

	.orb__halo,
	.orb__core,
	.orb__sheen {
		position: absolute;
		inset: 0;
		border-radius: 50%;
		pointer-events: none;
	}

	/* Soft outer glow that scales with audio intensity (or breathes in idle). */
	.orb__halo {
		background: radial-gradient(
			circle at 50% 50%,
			color-mix(in srgb, var(--accent-primary, #c2502a) 65%, transparent) 0%,
			color-mix(in srgb, var(--accent-primary, #c2502a) 30%, transparent) 40%,
			transparent 75%
		);
		opacity: var(--orb-glow-alpha, 0.3);
		transform: scale(calc(var(--orb-scale, 1) * 1.35));
		transition: opacity 80ms linear;
		filter: blur(6px);
	}

	/* Solid sphere — the visible "orb". Audio scaling lands here. */
	.orb__core {
		background: radial-gradient(
			circle at 35% 30%,
			color-mix(in srgb, #ffffff 55%, var(--accent-primary, #c2502a)) 0%,
			var(--accent-primary, #c2502a) 55%,
			color-mix(in srgb, var(--accent-primary, #c2502a) 70%, #000) 100%
		);
		box-shadow:
			inset 0 -4px 8px color-mix(in srgb, #000 30%, transparent),
			inset 0 2px 4px color-mix(in srgb, #fff 45%, transparent);
		transform: scale(var(--orb-scale, 1));
		transition: transform 60ms cubic-bezier(0.2, 0.8, 0.2, 1);
	}

	/* Tiny specular highlight on the upper-left — sells the sphere illusion. */
	.orb__sheen {
		background: radial-gradient(
			circle at 30% 25%,
			color-mix(in srgb, #ffffff 70%, transparent) 0%,
			transparent 35%
		);
		opacity: 0.85;
	}

	/* ── State animations ─────────────────────────────────────────── */

	/* Idle: slow, calm breath. Tells the user "voice is here but resting". */
	.orb--idle .orb__core {
		animation: orb-breath 3.6s ease-in-out infinite;
	}
	.orb--idle .orb__halo {
		animation: orb-halo-breath 3.6s ease-in-out infinite;
	}

	/* Listening: rotating gradient sweep + faster breath, so the user
	   knows the system is actively waiting for input. */
	.orb--listening .orb__core {
		animation: orb-breath 1.8s ease-in-out infinite;
		background: conic-gradient(
			from var(--orb-rotation, 0deg),
			color-mix(in srgb, var(--accent-primary, #c2502a) 80%, #fff) 0%,
			var(--accent-primary, #c2502a) 35%,
			color-mix(in srgb, var(--accent-primary, #c2502a) 60%, #000) 65%,
			color-mix(in srgb, var(--accent-primary, #c2502a) 80%, #fff) 100%
		);
		animation:
			orb-breath 1.8s ease-in-out infinite,
			orb-rotate 4s linear infinite;
	}
	.orb--listening .orb__halo {
		animation: orb-halo-breath 1.8s ease-in-out infinite;
	}

	/* Capturing: scale comes from JS analyser. Halo opacity already
	   bound via --orb-glow-alpha. No CSS animation is applied to the
	   core; `transform: scale(var(--orb-scale))` from the JS RAF loop
	   drives the response so it matches the actual audio amplitude. */
	.orb--capturing .orb__halo {
		animation: none;
	}

	/* Playing: smooth in/out pulse synced to a talking cadence. */
	.orb--playing .orb__core {
		animation: orb-talk 1.05s ease-in-out infinite;
	}
	.orb--playing .orb__halo {
		animation: orb-talk-halo 1.05s ease-in-out infinite;
	}

	@keyframes orb-breath {
		0%, 100% { transform: scale(0.94); }
		50% { transform: scale(1.06); }
	}

	@keyframes orb-halo-breath {
		0%, 100% { opacity: 0.22; transform: scale(1.25); }
		50% { opacity: 0.45; transform: scale(1.55); }
	}

	@keyframes orb-talk {
		0%, 100% { transform: scale(0.92); }
		20% { transform: scale(1.14); }
		40% { transform: scale(1.02); }
		60% { transform: scale(1.18); }
		80% { transform: scale(1.05); }
	}

	@keyframes orb-talk-halo {
		0%, 100% { opacity: 0.28; transform: scale(1.3); }
		20% { opacity: 0.55; transform: scale(1.6); }
		60% { opacity: 0.6; transform: scale(1.7); }
	}

	@keyframes orb-rotate {
		from { transform: scale(var(--orb-scale, 1)) rotate(0deg); }
		to   { transform: scale(var(--orb-scale, 1)) rotate(360deg); }
	}

	@media (prefers-reduced-motion: reduce) {
		.orb__core,
		.orb__halo {
			animation: none !important;
			transform: scale(1) !important;
		}
	}
</style>
