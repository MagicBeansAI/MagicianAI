<script lang="ts">
	import { onMount } from 'svelte';
	import type { ShaderInstance } from 'shaders/js';
	import { motionEnabled } from '$lib/motion';
	import BrandBling from './BrandBling.svelte';

	export let dark = false;

	let host: HTMLDivElement;
	let canvas: HTMLCanvasElement;
	let shader: ShaderInstance | null = null;
	let mounted = false;
	let visible = false;
	let pageVisible = true;
	let loading = false;
	let failed = false;
	let ready = false;
	let generation = 0;
	let finePointer = false;

	$: palette = dark
		? { colorA: '#191b22', colorB: '#59517d', colorC: '#725d36', colorD: '#734936' }
		: { colorA: '#faf8f5', colorB: '#c6b9ed', colorC: '#ffdf8c', colorD: '#ffc6b0' };
	$: running = mounted && visible && pageVisible && $motionEnabled;
	$: interactive = running && finePointer;
	$: if (running && !shader && !loading && !failed) void initialize();
	$: syncPlayback(running, shader);
	$: shader?.update('brand-flow', palette);

	function syncPlayback(play: boolean, instance: ShaderInstance | null): void {
		if (play) instance?.resume();
		else instance?.pause();
	}

	function resize(): void {
		if (!shader) return;
		const { width, height } = host.getBoundingClientRect();
		// Soft color bands need fewer pixels than the foreground's crisp text.
		const scale = Math.min(.65, 1000 / Math.max(width, 1));
		shader.resize(Math.max(1, Math.round(width * scale)), Math.max(1, Math.round(height * scale)));
	}

	async function initialize(): Promise<void> {
		if (!('gpu' in navigator)) {
			failed = true;
			return;
		}
		const version = ++generation;
		loading = true;
		try {
			const { createShader } = await import('shaders/js');
			if (!mounted || version !== generation || !running) return;
			const instance = await createShader(canvas, {
				components: [
					{
						type: 'FlowingGradient',
						id: 'brand-flow',
						props: { ...palette, speed: .16, distortion: .65, seed: 7 }
					}
				]
			}, {
				disableTelemetry: true,
				observeElement: false,
				onReady: () => {
					if (mounted && version === generation) ready = true;
				},
				onError: () => {
					if (mounted && version === generation) {
						ready = false;
						failed = true;
					}
				}
			});
			if (!mounted || version !== generation) {
				instance.destroy();
				return;
			}
			shader = instance;
			if (instance.getFailureReason()) {
				ready = false;
				failed = true;
			}
			resize();
			syncPlayback(running, instance);
		} catch {
			if (mounted && version === generation) failed = true;
		} finally {
			if (version === generation) loading = false;
		}
	}

	onMount(() => {
		pageVisible = !document.hidden;
		const onVisibility = () => (pageVisible = !document.hidden);
		document.addEventListener('visibilitychange', onVisibility);
		const pointerMedia = matchMedia('(hover: hover) and (pointer: fine)');
		const onPointerCapability = () => (finePointer = pointerMedia.matches);
		onPointerCapability();
		pointerMedia.addEventListener('change', onPointerCapability);
		const intersection = typeof IntersectionObserver !== 'undefined'
			? new IntersectionObserver(([entry]) => (visible = entry?.isIntersecting ?? false))
			: null;
		intersection?.observe(host);
		if (!intersection) visible = true;
		const size = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(resize) : null;
		size?.observe(host);
		mounted = true;
		return () => {
			mounted = false;
			generation++;
			pointerMedia.removeEventListener('change', onPointerCapability);
			intersection?.disconnect();
			size?.disconnect();
			document.removeEventListener('visibilitychange', onVisibility);
			shader?.destroy();
		};
	});
</script>

<div class="hero-background" class:dark class:running class:ready bind:this={host} aria-hidden="true">
	<div class="hb-fallback">
		<div class="hb-band hb-lilac"></div>
		<div class="hb-band hb-peach"></div>
		<div class="hb-glow"></div>
	</div>
	<canvas bind:this={canvas} class="hb-shader" style="width: 100%; height: 100%;"></canvas>
	<div class="hb-veil"></div>
	<BrandBling enabled={interactive} {dark} />
</div>

<style>
	.hero-background {
		--hb-lilac: #c6b9ed;
		--hb-peach: #ffc6b0;
		--hb-gold: #ffdf8c;
		position: absolute;
		inset: 0;
		z-index: -1;
		overflow: hidden;
		pointer-events: none;
		background: var(--landing-bg);
	}
	.dark {
		--hb-lilac: #59517d;
		--hb-peach: #734936;
		--hb-gold: #725d36;
	}
	.hb-fallback, .hb-shader, .hb-veil {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
	}
	.hb-shader {
		width: 100% !important;
		height: 100% !important;
		opacity: 0;
		transition: opacity 700ms ease;
	}
	.ready .hb-shader { opacity: .82; }
	.ready .hb-fallback { opacity: 0; }
	.hb-fallback { transition: opacity 700ms ease; }
	.hb-band {
		position: absolute;
		width: 120%;
		height: 70%;
		left: 5%;
		border-radius: 50%;
		filter: blur(30px);
		animation-play-state: paused;
	}
	.hb-lilac {
		top: -20%;
		background: radial-gradient(ellipse at center, var(--hb-lilac), transparent 68%);
		opacity: .7;
		transform: rotate(-22deg);
		animation: hb-lilac-drift 26s ease-in-out infinite alternate;
		animation-play-state: paused;
	}
	.hb-peach {
		top: 30%;
		left: 15%;
		background: radial-gradient(ellipse at center, var(--hb-peach), transparent 68%);
		opacity: .65;
		transform: rotate(18deg);
		animation: hb-peach-drift 32s ease-in-out infinite alternate;
		animation-play-state: paused;
	}
	.hb-glow {
		position: absolute;
		inset: 5% -15% 0 40%;
		background: radial-gradient(ellipse, color-mix(in srgb, var(--hb-gold) 60%, transparent), transparent 65%);
		animation: hb-gold-drift 20s ease-in-out infinite alternate;
		animation-play-state: paused;
	}
	.running .hb-band, .running .hb-glow { animation-play-state: running; }
	.ready .hb-band, .ready .hb-glow { animation-play-state: paused; }
	.hb-veil {
		background: linear-gradient(90deg,
			color-mix(in srgb, var(--landing-bg) 64%, transparent) 0%,
			color-mix(in srgb, var(--landing-bg) 38%, transparent) 38%,
			transparent 75%),
			linear-gradient(0deg, color-mix(in srgb, var(--landing-bg) 70%, transparent), transparent 35%);
	}
	@keyframes hb-lilac-drift {
		from { transform: translate(-8%, 0) rotate(-22deg) scale(.95); }
		to { transform: translate(6%, 18%) rotate(-10deg) scale(1.15); }
	}
	@keyframes hb-peach-drift {
		from { transform: translate(6%, 10%) rotate(18deg) scale(1.1); }
		to { transform: translate(-10%, -12%) rotate(5deg) scale(.95); }
	}
	@keyframes hb-gold-drift {
		from { transform: translate(4%, -5%) scale(.9); }
		to { transform: translate(-8%, 12%) scale(1.15); }
	}
	@media (max-width: 767px) {
		.hb-band { width: 160%; left: -10%; filter: blur(20px); }
		.hb-glow { inset: 10% -30% 15% 0; }
		.hb-veil {
			background: linear-gradient(180deg,
				color-mix(in srgb, var(--landing-bg) 68%, transparent),
				transparent 48%,
				color-mix(in srgb, var(--landing-bg) 60%, transparent));
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.hb-band, .hb-glow { animation: none; }
		.hb-shader, .hb-fallback { transition: none; }
	}
</style>
