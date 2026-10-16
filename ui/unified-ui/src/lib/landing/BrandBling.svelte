<script lang="ts">
	import { onMount } from 'svelte';
	import { BRAND_GOLD, BRAND_GOLD_INK } from './warmPalette';

	export let enabled = false;
	export let dark = false;

	interface Spark {
		x: number;
		y: number;
		born: number;
		life: number;
		size: number;
		turn: number;
		drift: number;
		companion: boolean;
	}
	let canvas: HTMLCanvasElement;
	let ctx: CanvasRenderingContext2D | null = null;
	let mounted = false;
	let frame = 0;
	let bounds: DOMRect;
	let sparks: Spark[] = [];
	let lastPoint: { x: number; y: number } | null = null;
	let serial = 0;
	let head: { x: number; y: number; at: number } | null = null;

	$: if (mounted && !enabled) clear();

	function clear(): void {
		if (frame) cancelAnimationFrame(frame);
		frame = 0;
		sparks = [];
		head = null;
		lastPoint = null;
		ctx?.clearRect(0, 0, bounds?.width ?? 0, bounds?.height ?? 0);
	}

	function resize(): void {
		bounds = canvas.getBoundingClientRect();
		const dpr = Math.min(2, window.devicePixelRatio || 1);
		canvas.width = Math.max(1, Math.round(bounds.width * dpr));
		canvas.height = Math.max(1, Math.round(bounds.height * dpr));
		ctx?.setTransform(dpr, 0, 0, dpr, 0, 0);
		clear();
	}

	function companion(size: number): void {
		if (!ctx) return;
		ctx.fillStyle = BRAND_GOLD_INK;
		ctx.strokeStyle = dark ? BRAND_GOLD : BRAND_GOLD_INK;
		ctx.lineWidth = Math.max(0.8, size * 0.16);
		ctx.beginPath();
		ctx.arc(0, 0, size * 0.4, 0, Math.PI * 2);
		ctx.fill();
		if (dark) ctx.stroke();
		ctx.lineCap = 'round';
		for (const angle of [-1.05, -0.4, 0.2]) {
			ctx.beginPath();
			ctx.moveTo(Math.cos(angle) * size * 0.83, Math.sin(angle) * size * 0.83);
			ctx.lineTo(Math.cos(angle) * size * 1.32, Math.sin(angle) * size * 1.32);
			ctx.stroke();
		}
	}

	function star(size: number, withCompanion = false): void {
		if (!ctx) return;
		ctx.beginPath();
		for (let i = 0; i < 10; i++) {
			const angle = -Math.PI / 2 + i * Math.PI / 5;
			const r = size * (i % 2 ? 0.46 : 1);
			const x = Math.cos(angle) * r;
			const y = Math.sin(angle) * r;
			if (i === 0) ctx.moveTo(x, y);
			else ctx.lineTo(x, y);
		}
		ctx.closePath();
		const gold = ctx.createLinearGradient(-size, -size, size, size);
		gold.addColorStop(0, '#fff1c7');
		gold.addColorStop(0.45, BRAND_GOLD);
		gold.addColorStop(1, '#e9a51b');
		ctx.fillStyle = gold;
		ctx.fill();
		ctx.strokeStyle = dark ? '#ffe6a0' : '#b47b0b';
		ctx.lineWidth = 0.55;
		ctx.stroke();
		if (withCompanion) {
			ctx.save();
			ctx.translate(size * 1.25, -size * 1.05);
			companion(size * 0.6);
			ctx.restore();
		}
	}

	function paint(now: number): void {
		frame = 0;
		if (!enabled || !ctx) return;
		ctx.clearRect(0, 0, bounds.width, bounds.height);
		sparks = sparks.filter((spark) => now - spark.born < spark.life);
		for (const spark of sparks) {
			const age = (now - spark.born) / spark.life;
			ctx.save();
			ctx.globalAlpha = Math.pow(1 - age, 1.6) * 0.85;
			ctx.translate(spark.x + spark.drift * age, spark.y - 13 * age);
			ctx.rotate(spark.turn * age);
			if (spark.companion) companion(spark.size * (1 - age * 0.3));
			else star(spark.size * (1 - age * 0.4));
			ctx.restore();
		}
		const headAlpha = head ? Math.max(0, 1 - (now - head.at) / 650) : 0;
		if (head && headAlpha > 0) {
			ctx.save();
			ctx.globalAlpha = headAlpha;
			ctx.translate(head.x + 9, head.y + 13);
			ctx.shadowColor = dark ? '#ffc93c' : 'rgba(255, 201, 60, .5)';
			ctx.shadowBlur = 9;
			star(9.5, true);
			ctx.restore();
		}
		if (sparks.length || headAlpha > 0) frame = requestAnimationFrame(paint);
	}

	function move(event: PointerEvent): void {
		if (!enabled || event.pointerType === 'touch' || !bounds.width) return;
		if (!ctx) {
			ctx = canvas.getContext('2d');
			if (!ctx) return;
			const dpr = Math.min(2, window.devicePixelRatio || 1);
			ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		}
		const x = event.clientX - bounds.left;
		const y = event.clientY - bounds.top;
		const now = performance.now();
		head = { x, y, at: now };
		const distance = lastPoint ? Math.hypot(x - lastPoint.x, y - lastPoint.y) : 0;
		const count = Math.min(8, Math.floor(distance / 22));
		if (count && lastPoint) {
			for (let i = 1; i <= count; i++) {
				serial++;
				const t = i / count;
				sparks.push({
					x: lastPoint.x + (x - lastPoint.x) * t,
					y: lastPoint.y + (y - lastPoint.y) * t,
					born: now,
					life: 800 + serial % 4 * 100,
					size: 4.4 + serial % 3 * 0.9,
					turn: Math.sin(serial * 2.4) * 0.7,
					drift: Math.sin(serial * 1.8) * 12,
					companion: serial % 3 === 0
				});
			}
			sparks = sparks.slice(-48);
		}
		if (!lastPoint || count) lastPoint = { x, y };
		if (!frame) frame = requestAnimationFrame(paint);
	}

	onMount(() => {
		resize();
		const section = canvas.closest('.hs');
		const leave = () => {
			lastPoint = null;
			if (head) head.at = performance.now() - 430;
		};
		section?.addEventListener('pointermove', move as EventListener, { passive: true });
		section?.addEventListener('pointerleave', leave);
		const observer = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(resize) : null;
		observer?.observe(canvas);
		window.addEventListener('resize', resize, { passive: true });
		// Cached coordinates need refreshing after an ordinary page scroll.
		const scroll = () => { bounds = canvas.getBoundingClientRect(); };
		window.addEventListener('scroll', scroll, { passive: true });
		mounted = true;
		return () => {
			mounted = false;
			clear();
			observer?.disconnect();
			window.removeEventListener('resize', resize);
			section?.removeEventListener('pointermove', move as EventListener);
			section?.removeEventListener('pointerleave', leave);
			window.removeEventListener('scroll', scroll);
		};
	});
</script>

<canvas class="brand-bling" bind:this={canvas} aria-hidden="true"></canvas>

<style>
	.brand-bling {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		pointer-events: none;
	}
</style>
