<script lang="ts">
	/**
	 * Minimap — the RTS corner map: a 2D-canvas top-down of the realm (static
	 * layer rebuilt when the village changes; roads/plaza/buildings), live
	 * citizen dots (vibe-coloured), [!] pings, the current view rectangle, and
	 * a compass needle. Click / drag = fly the camera there. Redraws on a slow
	 * tick (250ms) — cheap by construction, no 3D render.
	 */
	import { onDestroy, onMount } from 'svelte';
	import type { FleetEngine } from '../engine/engine';

	export let engine: FleetEngine;
	/** Citizen ids that currently need the Mayor (drawn as pinging rings). */
	export let needyIds: string[] = [];

	$: needySet = new Set(needyIds);

	const SIZE = 148;
	const VIBE_COLOR: Record<string, string> = {
		working: '#2f9e44',
		needs: '#f5a623',
		paused: '#8e6cc0',
		offline: '#8b949e',
		idle: '#4aa3c0'
	};

	let canvas: HTMLCanvasElement;
	let staticLayer: HTMLCanvasElement | null = null;
	let staticForN = 0;
	let timer: ReturnType<typeof setInterval> | null = null;

	function worldToPx(n: number, wx: number, wz: number): [number, number] {
		return [((wx + n / 2) / n) * SIZE, ((wz + n / 2) / n) * SIZE];
	}

	function buildStatic(): void {
		const village = engine.minimapVillage();
		if (!village) return;
		const n = village.n;
		staticForN = n;
		const layer = document.createElement('canvas');
		layer.width = layer.height = SIZE;
		const ctx = layer.getContext('2d');
		if (!ctx) return;
		// terrain wash
		ctx.fillStyle = 'rgba(30, 40, 34, 0.22)';
		ctx.fillRect(0, 0, SIZE, SIZE);
		const cell = SIZE / n;
		for (let z = 0; z < n; z++) {
			for (let x = 0; x < n; x++) {
				const kind = village.kinds[z * n + x];
				if (kind === 'road' || kind === 'plaza') {
					ctx.fillStyle = kind === 'plaza' ? 'rgba(235, 225, 200, 0.42)' : 'rgba(220, 210, 190, 0.28)';
					ctx.fillRect(x * cell, z * cell, Math.ceil(cell), Math.ceil(cell));
				} else if (kind === 'building') {
					ctx.fillStyle = 'rgba(250, 250, 255, 0.6)';
					ctx.fillRect(x * cell, z * cell, Math.ceil(cell), Math.ceil(cell));
				}
			}
		}
		staticLayer = layer;
	}

	function draw(): void {
		const ctx = canvas?.getContext('2d');
		const village = engine.minimapVillage();
		if (!ctx || !village) return;
		if (!staticLayer || staticForN !== village.n) buildStatic();
		ctx.clearRect(0, 0, SIZE, SIZE);
		if (staticLayer) ctx.drawImage(staticLayer, 0, 0);
		const n = village.n;

		// citizens + [!] pings (pulse via time)
		const pulse = 3 + ((performance.now() / 90) % 14);
		for (const dot of engine.minimapDots()) {
			const [px, py] = worldToPx(n, dot.x, dot.z);
			ctx.fillStyle = VIBE_COLOR[dot.vibe] ?? VIBE_COLOR.idle;
			ctx.beginPath();
			ctx.arc(px, py, 2.1, 0, Math.PI * 2);
			ctx.fill();
			if (needySet.has(dot.id)) {
				ctx.strokeStyle = 'rgba(245, 166, 35, 0.9)';
				ctx.lineWidth = 1.4;
				ctx.beginPath();
				ctx.arc(px, py, pulse, 0, Math.PI * 2);
				ctx.stroke();
			}
		}

		// view rectangle
		const view = engine.viewInfo();
		const [vx, vy] = worldToPx(n, view.x, view.z);
		const half = (view.half / n) * SIZE;
		ctx.strokeStyle = 'rgba(255, 255, 255, 0.85)';
		ctx.lineWidth = 1.2;
		ctx.strokeRect(vx - half, vy - half, half * 2, half * 2);
	}

	let dragging = false;
	function jump(e: PointerEvent): void {
		const village = engine.minimapVillage();
		if (!village) return;
		const r = canvas.getBoundingClientRect();
		const n = village.n;
		const wx = ((e.clientX - r.left) / SIZE) * n - n / 2;
		const wz = ((e.clientY - r.top) / SIZE) * n - n / 2;
		engine.focusWorld(wx, wz);
	}
	function onPointerDown(e: PointerEvent): void {
		dragging = true;
		canvas.setPointerCapture(e.pointerId);
		jump(e);
	}
	function onPointerMove(e: PointerEvent): void {
		if (dragging) jump(e);
	}
	function onPointerUp(): void {
		dragging = false;
	}

	onMount(() => {
		draw();
		timer = setInterval(draw, 250);
	});
	onDestroy(() => {
		if (timer) clearInterval(timer);
	});

	// compass needle angle (CSS rotation) — north is world -z
	let compassDeg = 0;
	let compassTimer: ReturnType<typeof setInterval> | null = null;
	onMount(() => {
		compassTimer = setInterval(() => {
			compassDeg = (engine.cameraAzimuth() * 180) / Math.PI;
		}, 250);
	});
	onDestroy(() => {
		if (compassTimer) clearInterval(compassTimer);
	});

</script>

<div class="mm" aria-label="Minimap">
	<canvas
		bind:this={canvas}
		width={SIZE}
		height={SIZE}
		on:pointerdown={onPointerDown}
		on:pointermove={onPointerMove}
		on:pointerup={onPointerUp}
	></canvas>
	<span class="mm__compass" style={`transform: rotate(${compassDeg}deg)`} title="North" aria-hidden="true">▲</span>
</div>

<style>
	.mm {
		pointer-events: auto;
		position: absolute;
		right: 0.75rem;
		bottom: 0.75rem;
		z-index: 5;
		border-radius: 0.6rem;
		border: 1px solid var(--border-subtle, rgba(128, 128, 128, 0.35));
		background: rgba(20, 24, 28, 0.18);
		backdrop-filter: blur(1.5px);
		box-shadow: 0 8px 26px rgba(0, 0, 0, 0.22);
		overflow: hidden;
		line-height: 0;
	}
	.mm canvas {
		display: block;
		cursor: crosshair;
		touch-action: none;
	}
	.mm__compass {
		position: absolute;
		top: 0.28rem;
		left: 0.34rem;
		font-size: 0.62rem;
		line-height: 1;
		color: var(--text-primary, #fff);
		text-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
		transform-origin: 50% 55%;
		pointer-events: none;
	}
</style>
