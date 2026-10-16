<script lang="ts">
	// The mote field — the film's depth, in real WebGL.
	//
	// Three layers of luminous dust hang in the world at three depths; the
	// camera's dolly moves each layer at its own parallax rate, which is
	// what turns "cards on paper" into a space the viewer flies THROUGH.
	// The same particles are Act I's cast: during the taken station they
	// stream AWAY from the person's device toward the piles at the edges
	// (the work multiplying), and at the return station the
	// reversal pulls them back into one point — the spark the aurora
	// thread is born from.
	//
	// three.js is already a dependency (the /square world); no new deps.
	// House rules honored: ONE render loop owns cadence — MovieTrack's rAF
	// calls tick() here, so there is no second loop to fall out of sync;
	// non-reactive plain-object state; DPR capped; a missing WebGL context
	// degrades to nothing, silently (the film still works without depth).
	import { onDestroy, onMount } from 'svelte';
	import * as THREE from 'three';
	import { resolveScene } from './scrub';
	import { AURORA, mixStops } from './auroraPalette';
	import { themeThreadPalette, type ThemeThreadPalette } from './themeThreadPalette';
	import { BOUNDS, SI, ignitionPoint, type Camera, type Vec } from './worldTrack';

	interface Layer {
		points: THREE.Points;
		geo: THREE.BufferGeometry;
		pos: Float32Array;
		col: Float32Array;
		n: number;
		baseOpacity: number;
		depth: Float32Array;
		bx: Float32Array;
		by: Float32Array;
		seed: Float32Array;
		/** Exodus stream progress per mote, advanced only while the era runs. */
		u: Float32Array;
		/** Return (pull-back) stream progress per mote. */
		v: Float32Array;
		/** Which edge mass this mote streams toward. */
		cluster: Uint8Array;
		/** 1 = joins the pile-up/return choreography, 0 = stays ambient. */
		gate: Float32Array;
	}

	let canvasEl: HTMLCanvasElement;

	const rt: {
		renderer: THREE.WebGLRenderer | null;
		scene: THREE.Scene | null;
		cam: THREE.OrthographicCamera | null;
		layers: Layer[];
		w: number;
		h: number;
		positions: Vec[];
		light: boolean;
		theme: ThemeThreadPalette;
		last: number;
	} = {
		renderer: null,
		scene: null,
		cam: null,
		layers: [],
		w: 0,
		h: 0,
		positions: [],
		light: true,
		theme: themeThreadPalette('', ''),
		last: 0
	};

	// A tiny deterministic PRNG so the field is stable across rebuilds.
	function mulberry(seed: number): () => number {
		let a = seed >>> 0;
		return () => {
			a |= 0;
			a = (a + 0x6d2b79f5) | 0;
			let t = Math.imul(a ^ (a >>> 15), 1 | a);
			t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
			return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
		};
	}

	function softSprite(): THREE.Texture {
		const c = document.createElement('canvas');
		c.width = 64;
		c.height = 64;
		const g = c.getContext('2d')!;
		const grad = g.createRadialGradient(32, 32, 0, 32, 32, 32);
		grad.addColorStop(0, 'rgba(255,255,255,1)');
		grad.addColorStop(0.4, 'rgba(255,255,255,0.55)');
		grad.addColorStop(1, 'rgba(255,255,255,0)');
		g.fillStyle = grad;
		g.fillRect(0, 0, 64, 64);
		const tex = new THREE.CanvasTexture(c);
		tex.needsUpdate = true;
		return tex;
	}

	function makeLayer(n: number, size: number, opacity: number, depthBase: number, depthVar: number, seedBase: number, tex: THREE.Texture): Layer {
		const rand = mulberry(seedBase);
		const pos = new Float32Array(n * 3);
		const col = new Float32Array(n * 3);
		const geo = new THREE.BufferGeometry();
		geo.setAttribute('position', new THREE.BufferAttribute(pos, 3));
		geo.setAttribute('color', new THREE.BufferAttribute(col, 3));
		const mat = new THREE.PointsMaterial({
			size,
			map: tex,
			transparent: true,
			opacity,
			vertexColors: true,
			depthWrite: false,
			depthTest: false,
			sizeAttenuation: false
		});
		const layer: Layer = {
			points: new THREE.Points(geo, mat),
			geo,
			pos,
			col,
			n,
			baseOpacity: opacity,
			depth: new Float32Array(n),
			bx: new Float32Array(n),
			by: new Float32Array(n),
			seed: new Float32Array(n),
			u: new Float32Array(n),
			v: new Float32Array(n),
			cluster: new Uint8Array(n),
			gate: new Float32Array(n)
		};
		layer.points.frustumCulled = false;
		for (let i = 0; i < n; i++) {
			layer.bx[i] = rand();
			layer.by[i] = rand();
			layer.seed[i] = rand();
			layer.depth[i] = depthBase + (rand() - 0.5) * 2 * depthVar;
			layer.u[i] = rand();
			layer.v[i] = rand();
			layer.cluster[i] = Math.floor(rand() * 4);
			layer.gate[i] = rand() < 0.72 ? 1 : 0;
		}
		return layer;
	}

	/** Called by MovieTrack on mount/resize/theme-flip — geometry, ground + palette. */
	export function configure(
		w: number,
		h: number,
		positions: Vec[],
		light: boolean,
		theme: ThemeThreadPalette = rt.theme
	): void {
		rt.w = w;
		rt.h = h;
		rt.positions = positions;
		rt.light = light;
		rt.theme = theme;
		if (!rt.renderer || !rt.cam) return;
		rt.renderer.setSize(w, h, false);
		rt.cam.right = w;
		rt.cam.bottom = h;
		rt.cam.updateProjectionMatrix();
	}

	const mix = (a: number, b: number, t: number): number => a + (b - a) * t;
	const clamp01 = (v: number): number => Math.min(1, Math.max(0, v));
	const smooth = (v: number): number => v * v * (3 - 2 * v);
	const easeOut = (v: number): number => 1 - (1 - v) * (1 - v);

	// The pile-up's own ink. It was a bright sky blue, chosen when the
	// drowning played on a scrim that dimmed the page toward night — against
	// which a light blue reads. The gloom is gone and the beat now plays in
	// full daylight, where that same blue is barely a tint on white. A deep
	// indigo has the contrast the ground actually offers.
	// Ambient dust, the detonation and the reversal use the active theme pair.
	const COLD = { r: 0.14, g: 0.2, b: 0.52 };

	/**
	 * Advance + render one frame. Fed the film camera by MovieTrack's rAF —
	 * this module never owns a loop.
	 */
	export function tick(cam: Camera, p: number, t: number): void {
		if (!rt.renderer || !rt.scene || !rt.cam || rt.w === 0 || rt.positions.length === 0) return;
		const dt = rt.last ? Math.min(0.06, t - rt.last) : 0.016;
		rt.last = t;
		const w = rt.w;
		const h = rt.h;
		const tileW = w * 1.7;
		const tileH = h * 1.7;

		// Era weights from the playhead: the WORK streaming off the device
		// during the pile-up and the pull-back at return. Both are zero
		// everywhere else, so the rest of the film never pays.
		const { scene, local } = resolveScene(BOUNDS, p);
		let wEx = 0;
		let wRet = 0;
		if (scene === SI.drown) {
			// The stream builds, holds through the pile-up, and dies out on the
			// station's own tail, so the field is empty by the time the camera
			// leaves for the reversal. What flies is no longer data on its way
			// to an advertiser: it is work leaving the device faster than
			// anyone can clear it, and the masses it lands in are the backlog.
			wEx = smooth(clamp01(local / 0.26)) * smooth(clamp01((1 - local) / 0.2));
		} else if (scene === SI.tear) {
			wRet = smooth(clamp01(local / 0.08)) * smooth(clamp01((0.5 - local) / 0.14));
		}

		// Screen anchors: the person's device (the source), the four piles
		// it spills into (its sinks), and the projected ignition point the
		// return converges on.
		const dev = { x: 0.5 * w, y: 0.58 * h };
		const cl = [
			{ x: 0.05 * w, y: 0.3 * h },
			{ x: 0.05 * w, y: 0.74 * h },
			{ x: 0.95 * w, y: 0.28 * h },
			{ x: 0.95 * w, y: 0.72 * h }
		];
		const ignW = ignitionPoint(rt.positions, h);
		const ign = { x: w / 2 + (ignW.x - cam.x) * cam.z, y: h / 2 + (ignW.y - cam.y) * cam.z };

		// Ambient dust colour source (phase 3, 2026-08-17): warm gold — dust
		// in a sunbeam — replaces the visitor's theme thread here (aurora
		// violet on cool/dark themes, rust/olive on the pinned warm one) with
		// AURORA.amberThinking's own two stops (auroraPalette.ts), so no new
		// colour number is invented. The pile-up's COLD ink and the reversal's
		// `protagonist` glow keep their own narrative meaning below and are
		// untouched.
		const inkA = AURORA.amberThinking.stops[0];
		const inkB = mixStops(AURORA.amberThinking.stops[1], AURORA.amberThinking.stops[0], 0.3);
		const protagonist = rt.theme.primary;

		for (const L of rt.layers) {
			// The pile-up and the pull-back are LUMINOUS events: the dust
			// brightens while it moves, then settles. Lower opacity variance
			// (phase 3, 2026-08-17): the beats still brighten the dust, just
			// gently — a flash reads as urgency, and this field's job now is
			// restraint.
			(L.points.material as THREE.PointsMaterial).opacity =
				L.baseOpacity * (1 + wEx * 1.1 + wRet * 0.8);
			for (let i = 0; i < L.n; i++) {
				const d = L.depth[i];
				const s = L.seed[i];
				// Ambient: a wrapped starfield with per-depth parallax and a
				// slow personal drift — the space the camera flies through.
				// Vertical drift is deliberately the slower of the two axes
				// (phase 3, 2026-08-17): dust in a sunbeam barely falls, it
				// mostly just hangs and turns.
				const driftX = Math.sin(t * (0.1 + s * 0.16) + s * 31) * 14;
				const driftY = Math.cos(t * (0.04 + s * 0.06) + s * 47) * 8;
				let x = (((L.bx[i] * tileW + driftX - cam.x * d) % tileW) + tileW) % tileW - (tileW - w) / 2;
				let y = (((L.by[i] * tileH + driftY - cam.y * d) % tileH) + tileH) % tileH - (tileH - h) / 2;

				let r = mix(inkA.r, inkB.r, s < 0.18 ? 1 : 0);
				let g = mix(inkA.g, inkB.g, s < 0.18 ? 1 : 0);
				let b = mix(inkA.b, inkB.b, s < 0.18 ? 1 : 0);

				if (wEx > 0.02 && L.gate[i] > 0) {
					// The pile-up: streams from the four masses INTO the
					// device, wiggling slightly, respawning out at the mass —
					// a continuous arrival that never lets up.
					//
					// This ran the other way for most of the film's life,
					// because the station it was written for was the data
					// exodus: light bleeding OUT of the person's machine
					// toward somebody else's warehouse. That story is gone.
					// What is left is the opposite claim — everything is
					// coming AT you — and a field still streaming outward
					// quietly argued against every other thing on the
					// station, which all arrives.
					L.u[i] += dt * (0.16 + 0.5 * s) * wEx;
					if (L.u[i] > 1) L.u[i] -= 1;
					const uu = easeOut(L.u[i]);
					const c = cl[L.cluster[i]];
					const px = -(c.y - dev.y);
					const py = c.x - dev.x;
					const plen = Math.hypot(px, py) || 1;
					// Wiggle decays as it CLOSES now, so the arrival tightens
					// onto the device instead of spraying off it.
					const wig = Math.sin(uu * 9 + s * 40) * 16 * (1 - uu);
					const ex = mix(c.x, dev.x, uu) + (px / plen) * wig;
					const ey = mix(c.y, dev.y, uu) + (py / plen) * wig;
					const k = wEx * L.gate[i];
					x = mix(x, ex, k);
					y = mix(y, ey, k);
					// Deepening with travel: faint out at the mass, full ink by
					// the time it reaches the person. The floor is 0.55, not
					// 0.25 — on a light ground a quarter-strength ink is
					// indistinguishable from the paper, so the far half of
					// every stream simply was not there.
					const lit = 0.55 + 0.45 * uu;
					r = mix(r, COLD.r * lit, k);
					g = mix(g, COLD.g * lit, k);
					b = mix(b, COLD.b * lit, k);
				}
				if (wRet > 0.02) {
					// The reversal: the same light streams BACK from the
					// masses into the ignition point, taking on the selected accent — the
					// aurora is made of everything that was taken. EVERY mote
					// answers the pull; the spark is made of all of it.
					L.v[i] += dt * (0.5 + 0.8 * s) * wRet;
					if (L.v[i] > 1) L.v[i] -= 1;
					const vv = easeOut(L.v[i]);
					const c = cl[L.cluster[i]];
					const wig = Math.sin(vv * 7 + s * 33) * 22 * (1 - vv);
					const px = -(ign.y - c.y);
					const py = ign.x - c.x;
					const plen = Math.hypot(px, py) || 1;
					const rx = mix(c.x, ign.x, vv) + (px / plen) * wig;
					const ry = mix(c.y, ign.y, vv) + (py / plen) * wig;
					const k = wRet * (0.55 + 0.45 * L.gate[i]);
					x = mix(x, rx, k);
					y = mix(y, ry, k);
					const glow = 0.55 + 0.65 * vv;
					r = mix(r, protagonist.r * glow, k);
					g = mix(g, protagonist.g * glow, k);
					b = mix(b, protagonist.b * glow, k);
				}

				const j = i * 3;
				L.pos[j] = x;
				L.pos[j + 1] = y;
				L.pos[j + 2] = 0;
				L.col[j] = r;
				L.col[j + 1] = g;
				L.col[j + 2] = b;
			}
			(L.geo.attributes.position as THREE.BufferAttribute).needsUpdate = true;
			(L.geo.attributes.color as THREE.BufferAttribute).needsUpdate = true;
		}
		rt.renderer.render(rt.scene, rt.cam);
	}

	onMount(() => {
		try {
			rt.renderer = new THREE.WebGLRenderer({
				canvas: canvasEl,
				alpha: true,
				antialias: false,
				powerPreference: 'high-performance'
			});
		} catch {
			// No WebGL — the film simply runs without the depth field.
			rt.renderer = null;
			return;
		}
		rt.renderer.setPixelRatio(Math.min(1.5, window.devicePixelRatio || 1));
		rt.renderer.setClearColor(0x000000, 0);
		rt.scene = new THREE.Scene();
		rt.cam = new THREE.OrthographicCamera(0, 100, 0, 100, -1, 1);
		const tex = softSprite();
		// far / mid / near — counts and sizes tuned for a mid-range GPU:
		// ~1300 points, JS-updated buffers, one draw call per layer.
		rt.layers = [
			makeLayer(720, 2.2, 0.3, 0.32, 0.12, 11, tex),
			makeLayer(420, 4.2, 0.26, 0.68, 0.16, 23, tex),
			makeLayer(170, 7.5, 0.15, 1.26, 0.2, 37, tex)
		];
		for (const L of rt.layers) rt.scene.add(L.points);
		if (rt.w > 0) configure(rt.w, rt.h, rt.positions, rt.light);
		return () => {};
	});

	onDestroy(() => {
		for (const L of rt.layers) {
			L.geo.dispose();
			(L.points.material as THREE.Material).dispose();
		}
		rt.renderer?.dispose();
		rt.renderer = null;
	});
</script>

<canvas class="mf" bind:this={canvasEl} aria-hidden="true"></canvas>

<style>
	.mf {
		position: absolute;
		inset: 0;
		z-index: 1;
		width: 100%;
		height: 100%;
		pointer-events: none;
	}
</style>
