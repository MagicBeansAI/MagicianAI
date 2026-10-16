<script lang="ts">
	// The closing montage — devices working untouched while people live.
	//
	// This proof is what EARNS the later “Do what you love.” payoff, and a
	// claim like that cannot be paid by a diagram. This layer is the slot the
	// footage lands in: a frame sequence in `static/prologue/reel-life/`,
	// blitted to a canvas by MovieTrack's own rAF, exactly the technique
	// PrologueReel uses for Act 0 and for the same reason (seeking a
	// <video> under scrub hunts keyframes and stutters).
	//
	// It is a SEPARATE component from PrologueReel on purpose. That one
	// carries the match-cut contract — screen rect, case rect, surround,
	// tube tint, caption windows, film switching, the era fence that keeps
	// two cuts from interleaving — and none of it means anything here: this
	// montage hands off to nothing, and there is only ever one of it. A
	// shared component would be PrologueReel plus a mode flag, and the
	// mode flag would be the beginning of two films' worth of branching in
	// the one place the landing cannot afford a mistake.
	//
	// THE ABSENCE CONTRACT. The footage is generated separately and may not
	// be there yet. When the manifest 404s this component reports `false`
	// once and never asks again — MovieTrack drops the frame from the tree
	// entirely and the montage station's words carry the beat alone. There
	// is no placeholder box, no spinner and no broken image: the same rule
	// the reduced-motion still already follows for a reel that has not
	// landed.
	import { onDestroy } from 'svelte';
	import type { LifeGeom, LifeScene } from './lifeComposite';

	/** Fired once when the footage resolves — or fails to. */
	export let onState: (on: boolean) => void = () => {};
	/** Directory holding the frame sequence and its manifest.json. */
	export let base = '/prologue/reel-life/';

	interface Manifest {
		frames: number;
		width: number;
		height: number;
		pattern: string;
		fps: number;
		version?: string | number;
		scenes?: LifeScene[];
	}

	/** ± frames kept decoded around the playhead. */
	const WINDOW = 10;
	/** Live-bitmap budget in BYTES — a decoded frame costs w·h·4. */
	const BITMAP_BUDGET_BYTES = 48 * 1024 * 1024;
	const MIN_CAP = WINDOW * 2 + 2;
	const MAX_INFLIGHT = 6;
	const NEIGHBOUR_REACH = 40;

	let canvasEl: HTMLCanvasElement;

	const rt: {
		ctx: CanvasRenderingContext2D | null;
		man: Manifest | null;
		prefix: string;
		suffix: string;
		pad: number;
		bust: string;
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
		cw: number;
		ch: number;
		shown: boolean;
		alpha: number;
	} = {
		ctx: null,
		man: null,
		prefix: '',
		suffix: '',
		pad: 3,
		bust: '',
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
		cw: 0,
		ch: 0,
		shown: false,
		alpha: -1
	};

	/** Whether the footage is available; false keeps the words-only beat. */
	export function isOn(): boolean {
		return rt.man !== null;
	}

	/**
	 * Where the plate is actually drawn, and which frame is showing.
	 *
	 * `LifeDevice` composites a DOM device onto this canvas, and it can only do
	 * that if it can convert the manifest's FRAME pixels into CSS pixels. The
	 * plate is `contain`-fitted, so that conversion is this component's private
	 * arithmetic — publishing it is what keeps the device registered with the
	 * picture at every aspect instead of guessing at a fit it cannot see.
	 *
	 * Returns null until the manifest lands, which is also the signal that
	 * there is nothing to composite onto.
	 */
	export function geom(): LifeGeom | null {
		if (!rt.man) return null;
		return {
			dx: rt.dx,
			dy: rt.dy,
			dw: rt.dw,
			dh: rt.dh,
			nw: rt.man.width,
			nh: rt.man.height,
			frame: rt.idx,
			scenes: rt.man.scenes ?? []
		};
	}

	function framePath(i: number): string {
		return rt.prefix + String(i).padStart(rt.pad, '0') + rt.suffix + rt.bust;
	}

	async function loadManifest(): Promise<void> {
		if (rt.asked) return;
		rt.asked = true;
		try {
			// Revalidated, like Act 0's: frame URLs are stable while their
			// content is not, so a force-cached index would pin the browser to
			// a retired cut. The frames themselves stay force-cached and are
			// busted by the manifest's own `version`.
			const res = await fetch(base + 'manifest.json', { cache: 'no-cache' });
			if (!res.ok) throw new Error(String(res.status));
			const man = (await res.json()) as Manifest;
			if (!man?.frames || !man.pattern || !man.width || !man.height) throw new Error('shape');
			const token = man.pattern.match(/%0(\d+)d/);
			if (!token || token.index === undefined) throw new Error('pattern');
			rt.pad = Number(token[1]);
			rt.prefix = base + man.pattern.slice(0, token.index);
			rt.suffix = man.pattern.slice(token.index + token[0].length);
			rt.bust = man.version === undefined ? '' : '?v=' + String(man.version);
			rt.man = man;
			layout();
			onState(true);
		} catch {
			// Not generated yet, or a bad deploy. One request, one verdict,
			// and the station is a typographic beat instead of a broken one.
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
		fetch(framePath(i), { cache: 'force-cache' })
			.then((r) => (r.ok ? r.blob() : Promise.reject(new Error(String(r.status)))))
			.then((b) => createImageBitmap(b))
			.then((bm) => {
				// Teardown may have landed while this was in flight; an
				// ImageBitmap holds native memory and must be closed, not dropped.
				if (!rt.man) {
					bm.close();
					return;
				}
				rt.inflight.delete(i);
				rt.bmp.set(i, bm);
				evict();
			})
			.catch(() => rt.inflight.delete(i));
	}

	function capFor(): number {
		const w = rt.man?.width ?? 960;
		const h = rt.man?.height ?? 540;
		return Math.max(MIN_CAP, Math.floor(BITMAP_BUDGET_BYTES / Math.max(1, w * h * 4)));
	}

	function evict(): void {
		// Farthest-from-the-playhead first: a scrub reverses often enough that
		// pure LRU would throw away frames we are about to want again.
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

	/** Canvas sizing. Called on mount and on every resize. */
	export function configure(cw: number, ch: number): void {
		rt.cw = cw;
		rt.ch = ch;
		if (!canvasEl || cw === 0 || ch === 0) return;
		const dpr = Math.min(2, window.devicePixelRatio || 1);
		canvasEl.width = Math.round(cw * dpr);
		canvasEl.height = Math.round(ch * dpr);
		rt.ctx = canvasEl.getContext('2d');
		rt.ctx?.setTransform(dpr, 0, 0, dpr, 0, 0);
		rt.drawn = -1;
		layout();
	}

	function layout(): void {
		const man = rt.man;
		if (!man || rt.cw === 0 || rt.ch === 0) return;
		// CONTAIN, not cover. Act 0 crops to the frame edge because it is the
		// world the camera is inside; this one is a picture the montage plays
		// in, framed by the station's own paper — so nothing is ever cut off
		// and the words underneath keep their room at every aspect.
		const scale = Math.min(rt.cw / man.width, rt.ch / man.height);
		rt.dw = man.width * scale;
		rt.dh = man.height * scale;
		rt.dx = (rt.cw - rt.dw) / 2;
		rt.dy = (rt.ch - rt.dh) / 2;
		rt.drawn = -1;
	}

	/**
	 * Advance the montage. `u` is the station's own progress (0..1);
	 * `alpha` fades the layer in as the camera arrives. Called from
	 * MovieTrack's rAF — this module owns no loop.
	 */
	export function tick(u: number, alpha: number): void {
		if (!canvasEl) return;
		const live = alpha > 0.001 && u >= -0.25 && u <= 1.25;
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

	/** Warm the manifest before the station is on screen. */
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
			// Not decoded yet: the nearest frame we do have, rather than a
			// hole. Under a fast scrub this reads as a lower frame rate.
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
		if (pick < 0 || pick === rt.drawn) return;
		const bm = rt.bmp.get(pick);
		if (!bm) return;
		ctx.clearRect(0, 0, rt.cw, rt.ch);
		ctx.drawImage(bm, rt.dx, rt.dy, rt.dw, rt.dh);
		rt.drawn = pick;
	}

	onDestroy(() => {
		for (const bm of rt.bmp.values()) bm.close();
		rt.bmp.clear();
		rt.man = null;
	});
</script>

<canvas class="lr" bind:this={canvasEl} aria-hidden="true"></canvas>

<style>
	.lr {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		visibility: hidden;
		pointer-events: none;
		/* Transparent, unlike Act 0's opaque room: this montage plays INSIDE
		   the station's frame on the theme's own paper, so the frame's
		   rounded corners and its shadow have to keep showing through. */
		background: transparent;
	}
</style>
