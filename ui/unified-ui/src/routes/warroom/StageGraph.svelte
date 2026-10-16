<script lang="ts">
	/**
	 * STAGE GRAPH — the run graph painted behind the voice stage.
	 *
	 * Ambient but honest: every node is a real uplink frame or a live task
	 * from the task store (see `runGraph.ts` for the growth rules). New nodes
	 * are BORN AT THEIR PARENT'S position and ease outward to their radial
	 * home — which is what makes a running task read as a limb growing out
	 * of the core rather than dots teleporting in.
	 *
	 * Render discipline (the deck's hard-won rules):
	 * - the loop never writes Svelte state — all per-frame values live on the
	 *   untracked `rt` object;
	 * - theme colours resolve through a probe element ~1×/s, never per frame
	 *   (getComputedStyle forces a style recalc);
	 * - reduced motion pins nodes to their targets — the graph still GROWS
	 *   (that is data), it just doesn't drift.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';

	import { inCluster, nodeLife, prune, ROOT_ID, type RunGraphState } from './runGraph';

	/** The page-owned graph. A plain object — the canvas polls it per frame,
	 *  so no reactivity is needed (or wanted) on the data path. */
	export let graph: RunGraphState;
	/** Task cluster to spotlight; everything else dims. Null = show all. */
	export let focusTaskId: string | null = null;
	/** FRONT mode: the focused run takes over the stage — re-centred on the
	 *  task, scaled up, every node labelled — for actual exploration. */
	export let front = false;
	export let onExit: () => void = () => {};

	let canvas: HTMLCanvasElement | null = null;
	let shell: HTMLElement | null = null;
	let resizeObserver: ResizeObserver | null = null;

	const rt = {
		raf: null as number | null,
		w: 0,
		h: 0,
		dpr: 1,
		/** Eased screen positions, keyed by node id. */
		pos: new Map<string, { x: number; y: number }>(),
		colors: { bg: '#05070a', glow: '#7fd7ff', dim: '#5a6b7a', ok: '#3ddc97', warn: '#ffb454', err: '#ff5c5c', hitl: '#ffd166' },
		colorAge: 999,
		lastPruneMs: 0,
		reduced: false,
		focus: null as string | null,
		front: false
	};

	$: rt.focus = focusTaskId;
	$: rt.front = front && !!focusTaskId;

	function resolveColors(): void {
		if (!shell || !browser) return;
		const probe = document.createElement('span');
		probe.style.cssText = 'position:absolute;visibility:hidden;pointer-events:none';
		shell.appendChild(probe);
		const read = (expr: string, fallback: string): string => {
			probe.style.color = '';
			probe.style.color = expr;
			const v = getComputedStyle(probe).color;
			return v && v !== 'rgba(0, 0, 0, 0)' ? v : fallback;
		};
		rt.colors = {
			bg: read('var(--deck-bg, var(--bg-base))', '#05070a'),
			glow: read('var(--deck-glow, var(--accent-primary))', '#7fd7ff'),
			dim: read('var(--deck-dim, var(--text-secondary))', '#5a6b7a'),
			ok: read('var(--sev-ok, var(--color-success))', '#3ddc97'),
			warn: read('var(--sev-warn, var(--color-warning))', '#ffb454'),
			err: read('var(--sev-err, var(--color-error))', '#ff5c5c'),
			hitl: read('var(--sev-hitl, var(--color-warning))', '#ffd166')
		};
		probe.remove();
	}

	function sizeCanvas(): void {
		if (!canvas || !shell) return;
		const rect = shell.getBoundingClientRect();
		rt.dpr = Math.min(2, browser ? window.devicePixelRatio || 1 : 1);
		rt.w = Math.max(1, Math.floor(rect.width));
		rt.h = Math.max(1, Math.floor(rect.height));
		canvas.width = Math.floor(rt.w * rt.dpr);
		canvas.height = Math.floor(rt.h * rt.dpr);
		canvas.style.width = `${rt.w}px`;
		canvas.style.height = `${rt.h}px`;
	}

	function withAlpha(color: string, alpha: number): string {
		const a = Math.max(0, Math.min(1, alpha));
		const m = color.match(/rgba?\(([^)]+)\)/);
		if (!m) return color;
		const [r, g, b] = m[1].split(',').map((v) => parseFloat(v));
		return `rgba(${r}, ${g}, ${b}, ${a})`;
	}

	function severityColor(sev: string | null): string {
		switch (sev) {
			case 'error':
				return rt.colors.err;
			case 'warn':
				return rt.colors.warn;
			case 'hitl':
				return rt.colors.hitl;
			case 'success':
				return rt.colors.ok;
			default:
				return rt.colors.glow;
		}
	}

	function frame(): void {
		draw();
		rt.raf = requestAnimationFrame(frame);
	}

	function draw(): void {
		if (!canvas || !graph) return;
		const ctx = canvas.getContext('2d');
		if (!ctx) return;
		const nowMs = Date.now();

		// Housekeeping at 0.5 Hz, not per frame.
		rt.colorAge += 1;
		if (rt.colorAge > 60) {
			rt.colorAge = 0;
			resolveColors();
		}
		if (nowMs - rt.lastPruneMs > 2_000) {
			rt.lastPruneMs = nowMs;
			prune(graph, nowMs);
			for (const id of rt.pos.keys()) if (!graph.nodes.has(id)) rt.pos.delete(id);
		}

		const { w, h, dpr } = rt;
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.clearRect(0, 0, w, h);

		const isFront = rt.front && rt.focus;
		if (isFront) {
			// Scrim: the stage behind fades so the run owns the room.
			ctx.fillStyle = withAlpha(rt.colors.bg, 0.9);
			ctx.fillRect(0, 0, w, h);
		}

		const cx = w / 2;
		// The core sits in the upper half of the stage; anchor the graph there
		// so limbs radiate from BEHIND the orb. In FRONT mode the focused
		// task is the origin: its home vector is subtracted from its
		// cluster's, re-centring the run mid-stage and magnifying it.
		const cy = isFront ? h * 0.5 : h * 0.42;
		const unit = Math.min(w, h) * 0.52;
		const focusTask = isFront ? graph.nodes.get(`task:${rt.focus}`) : null;

		// FRONT lays the run out as a LAYERED DAG, not the ambient radial: a
		// plan is depth-structured (depends_on), and radial packing piled the
		// late steps into a label heap. Depth = longest dependency chain from
		// the task; siblings spread across the row; rows fill the stage.
		const frontPos = new Map<string, { x: number; y: number }>();
		if (isFront && focusTask) {
			const cluster: typeof focusTask[] = [];
			for (const node of graph.nodes.values()) {
				if (node.clusterId === rt.focus || node.id === focusTask.id) cluster.push(node);
			}
			// Longest-path depth via relaxation (forks take max of parents).
			const depth = new Map<string, number>([[focusTask.id, 0]]);
			for (let pass = 0; pass < cluster.length; pass += 1) {
				let changed = false;
				for (const node of cluster) {
					if (node.id === focusTask.id) continue;
					const pd = node.parentId ? depth.get(node.parentId) : undefined;
					const next = (pd ?? 0) + 1;
					if ((depth.get(node.id) ?? -1) < next) {
						depth.set(node.id, next);
						changed = true;
					}
				}
				if (!changed) break;
			}
			const maxDepth = Math.max(1, ...depth.values());
			const rows = new Map<number, typeof cluster>();
			for (const node of cluster) {
				const d = depth.get(node.id) ?? 1;
				const row = rows.get(d) ?? [];
				row.push(node);
				rows.set(d, row);
			}
			const top = h * 0.12;
			const rowGap = Math.min(72, (h * 0.74) / maxDepth);
			for (const [d, row] of rows) {
				// Stable order within a row: by home angle, so nodes don't swap
				// columns between frames.
				row.sort((a, b) => a.angle - b.angle);
				const span = Math.min(w * 0.78, Math.max(1, row.length - 1) * 190);
				row.forEach((node, i) => {
					const x =
						row.length === 1 ? cx : cx - span / 2 + (span * i) / (row.length - 1);
					frontPos.set(node.id, { x, y: top + d * rowGap });
				});
			}
		}

		// ── ease every node toward its radial home ───────────────────────
		for (const node of graph.nodes.values()) {
			let tx: number;
			let ty: number;
			if (isFront && focusTask) {
				const home = frontPos.get(node.id);
				tx = home?.x ?? cx;
				ty = home?.y ?? cy;
			} else {
				tx = cx + Math.cos(node.angle) * node.radius * unit;
				ty = cy + Math.sin(node.angle) * node.radius * unit;
			}
			let p = rt.pos.get(node.id);
			if (!p) {
				// Born at the parent's current position → visible outward growth.
				const parent = node.parentId ? rt.pos.get(node.parentId) : null;
				p = { x: parent?.x ?? cx, y: parent?.y ?? cy };
				rt.pos.set(node.id, p);
			}
			if (rt.reduced) {
				p.x = tx;
				p.y = ty;
			} else {
				p.x += (tx - p.x) * 0.06;
				p.y += (ty - p.y) * 0.06;
			}
		}

		// ── edges first, then nodes ──────────────────────────────────────
		for (const node of graph.nodes.values()) {
			// The graph is per-task now: everything outside the selected
			// cluster is skipped outright (front and background alike).
			if (rt.focus && !(node.clusterId === rt.focus || node.id === `task:${rt.focus}`)) continue;
			if (!node.parentId || node.kind === 'root') continue;
			const p = rt.pos.get(node.id);
			const q = rt.pos.get(node.parentId);
			if (!p || !q) continue;
			const life = nodeLife(node, nowMs);
			if (life === 0) continue;
			const focused = inCluster(node, rt.focus);
			const alpha = isFront ? 0.55 : (0.16 + 0.24 * life) * (focused ? (rt.focus ? 1.6 : 1) : 0.15);
			ctx.beginPath();
			ctx.moveTo(q.x, q.y);
			ctx.lineTo(p.x, p.y);
			ctx.strokeStyle = withAlpha(node.kind === 'event' ? severityColor(node.severity) : rt.colors.glow, alpha);
			ctx.lineWidth = node.kind === 'task' ? 1 : 0.75;
			ctx.stroke();
		}

		for (const node of graph.nodes.values()) {
			if (node.kind === 'root') continue;
			if (rt.focus && !(node.clusterId === rt.focus || node.id === `task:${rt.focus}`)) continue;
			const p = rt.pos.get(node.id);
			if (!p) continue;
			const life = nodeLife(node, nowMs);
			if (life === 0) continue;
			const focused = inCluster(node, rt.focus);
			const dimFactor = isFront ? 1.8 : focused ? (rt.focus ? 1.5 : 1) : 0.12;
			// A just-born node flares briefly — the graph's own event flash.
			const fresh = !rt.reduced && nowMs - node.bornMs < 900 ? 1.8 - (nowMs - node.bornMs) / 900 : 1;

			if (node.kind === 'task') {
				const alpha = Math.min(0.85, (0.45 + 0.3 * life) * dimFactor);
				ctx.beginPath();
				ctx.arc(p.x, p.y, (isFront ? 9 : 5.5) * fresh, 0, Math.PI * 2);
				// The ring wears the task's OUTCOME: green = completed,
				// red = failed, amber = cancelled, glow = still going.
				ctx.strokeStyle = withAlpha(node.severity ? severityColor(node.severity) : rt.colors.glow, alpha);
				ctx.lineWidth = 1.25;
				ctx.stroke();
				if (node.label) {
					ctx.font = isFront ? '600 12px ui-monospace, monospace' : '600 8.5px ui-monospace, monospace';
					ctx.fillStyle = withAlpha(rt.colors.dim, Math.min(0.9, 0.7 * dimFactor + 0.1 * life));
					ctx.textAlign = p.x > cx ? 'left' : 'right';
					ctx.fillText(node.label, p.x + (p.x > cx ? (isFront ? 14 : 9) : (isFront ? -14 : -9)), p.y + 3);
				}
			} else if (node.kind === 'agent') {
				const alpha = Math.min(0.7, (0.32 + 0.25 * life) * dimFactor);
				ctx.beginPath();
				ctx.arc(p.x, p.y, 3.5 * fresh, 0, Math.PI * 2);
				ctx.strokeStyle = withAlpha(rt.colors.dim, alpha);
				ctx.lineWidth = 1;
				ctx.stroke();
			} else {
				const alpha = Math.min(0.75, (0.28 + 0.4 * life) * dimFactor);
				ctx.beginPath();
				ctx.arc(p.x, p.y, (isFront ? 4.5 : 2.4) * fresh, 0, Math.PI * 2);
				// Null severity = a pending/skipped seeded step: dim, not glow.
				ctx.fillStyle = withAlpha(node.severity ? severityColor(node.severity) : rt.colors.dim, alpha);
				ctx.fill();
				// Step labels only under focus — ambient events stay unlabelled
				// noise; a spotlit run reads as an annotated plan.
				if (node.label && rt.focus && node.clusterId === rt.focus) {
					if (isFront) {
						// Layered rows: the label sits CENTERED BELOW its dot —
						// side-on labels collided with row siblings.
						ctx.font = '400 10.5px ui-monospace, monospace';
						ctx.fillStyle = withAlpha(rt.colors.glow, Math.min(0.95, 0.7 * dimFactor));
						ctx.textAlign = 'center';
						ctx.fillText(node.label.slice(0, 20), p.x, p.y + 16);
					} else {
						ctx.font = '400 8px ui-monospace, monospace';
						ctx.fillStyle = withAlpha(rt.colors.dim, Math.min(0.85, 0.7 * dimFactor));
						ctx.textAlign = p.x > cx ? 'left' : 'right';
						ctx.fillText(node.label, p.x + (p.x > cx ? 6 : -6), p.y + 3);
					}
				}
			}
		}
	}

	function frontKeydown(event: KeyboardEvent): void {
		if (event.code === 'Escape' && front) {
			event.stopPropagation();
			onExit();
		}
	}

	onMount(() => {
		if (!browser) return;
		window.addEventListener('keydown', frontKeydown, true);
		rt.reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		resolveColors();
		sizeCanvas();
		if (shell && 'ResizeObserver' in window) {
			resizeObserver = new ResizeObserver(() => sizeCanvas());
			resizeObserver.observe(shell);
		}
		rt.raf = requestAnimationFrame(frame);
	});

	onDestroy(() => {
		if (browser) window.removeEventListener('keydown', frontKeydown, true);
		if (rt.raf !== null) cancelAnimationFrame(rt.raf);
		rt.raf = null;
		resizeObserver?.disconnect();
		resizeObserver = null;
	});
</script>

<div class="graph-shell" class:graph-shell--front={front && !!focusTaskId} bind:this={shell} aria-hidden={!front}>
	<canvas bind:this={canvas}></canvas>
	{#if front && focusTaskId}
		<div class="front-chrome">
			<span class="front-title">RUN GRAPH</span>
			<button class="front-close" on:click={onExit}>ESC · CLOSE ✕</button>
		</div>
	{/if}
</div>

<style>
	/* Full-bleed background layer of the stage; everything else stacks on
	   top of it in normal flow. Never intercepts the core's gestures. */
	.graph-shell {
		position: absolute;
		inset: 0;
		pointer-events: none;
	}

	/* FRONT: the run owns the stage — above the orb, interactive. */
	.graph-shell--front {
		z-index: 6;
		pointer-events: auto;
	}

	.front-chrome {
		position: absolute;
		top: 14px;
		left: 0;
		right: 0;
		display: flex;
		justify-content: space-between;
		padding: 0 46px;
	}

	.front-title {
		font: 700 10px/1 var(--font-display, monospace);
		letter-spacing: 0.3em;
		color: var(--deck-dim);
	}

	.front-close {
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		font: 600 10px/1 var(--font-data, monospace);
		letter-spacing: 0.14em;
		color: var(--deck-dim);
	}
	.front-close:hover { color: var(--deck-glow); }

	canvas {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
	}
</style>
