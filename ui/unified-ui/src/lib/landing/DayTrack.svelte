<!--
  THE DAY — "a day with it", as road C of the fork.

  THIS IS AN EXTRACTION, NOT A REWRITE. Everything below is the day exactly
  as it played inside the film up to commit 7a4087161 (the parent of the
  second cut): the same eleven scrubbed stations, the same camera dolly, the
  same aurora thread with its coil and its closing knot, the same five-phase
  voice orb, the same lock-screen wake, the same Pythagoras overlay. It was
  lifted by DELETING everything that was not the day — the reel and its match
  cut, the CRT, 1995, the exodus, the reversal, the era scrim, the ember, the
  mote field's Act I choreography and the travel signage — rather than by
  being written again, which is why the choreography still lands frame for
  frame.

  Why it lives here at all: the 2026-08-05 second cut made the film ONE
  argument (every tool still needed you; this one does not), and eleven
  worked examples after the payoff is not that argument — it is proof of it.
  So the argument stays in the film and the proof became a road you opt into.
  For a while road C carried a static prose summary of the day instead, which
  is a description of the artefact rather than the artefact.

  Known limitation, and the plan of record
  (docs/archive/plans/2026-08-06-landing-roads-as-scrubbed-modals.md): this mounts
  INLINE in the fork's panel, so opening road C makes the page ~2090svh
  taller. That is exactly how the day behaved when it was in the main scroll,
  and `scrub` derives p from this element's own rect against the viewport, so
  it is correct — but the better home is a modal with its own scroll
  container, shared with roads A and B on one extracted engine.
-->
<script lang="ts">
	// The landing movie — ONE CONTINUOUS FILM, not twelve rooms.
	//
	// The 2026-08 re-choreography: every chapter is a STATION planted in
	// one continuous world on the app's own theme ground; one camera
	// (compositor transforms only) dollies
	// between them along a waypoint track driven by global scroll progress;
	// and the protagonist — the ASK, a living aurora thread — is born from
	// the opening sentence's final period and travels THROUGH every station
	// doing the work: it carries the ask out of the composer, picks up a
	// memory fragment that orbits it, splits into plan strands, drives the
	// browser, knocks at the spend gate, loops the meeting, coils into the
	// orb (the reveal: what you have followed IS Magican), and finally ties
	// the day closed around the receipt. The scrub machinery (scrub.ts) is
	// unchanged; the world/camera/thread math lives in worldTrack.ts.
	//
	// Reduced motion is the house rule, not an afterthought: the track
	// collapses to auto height, the camera and thread never mount, and ALL
	// stations render stacked as a readable document with identical copy.
	import { onDestroy, onMount } from 'svelte';
	import { createScrubber, resolveScene } from './scrub';
	import {
		AURORA,
		VOICE_ORB_LANDING_PHASE,
		VOICE_ORB_SEQUENCE,
		mixStops,
		rgb,
		rgba,
		type AuroraPhase,
		type AuroraPhaseName
	} from './auroraPalette';
	import { themeThreadPalette } from './themeThreadPalette';
	import { motionEnabled } from '$lib/motion';
	import AuroraOrbScene from './AuroraOrbScene.svelte';
	import {
		BOUNDS,
		LEG_BOW,
		SI,
		STATIONS,
		TRACK_SVH,
		behindAt,
		buildThread,
		cameraAt,
		pAt,
		sampleThread,
		type Camera,
		type Station,
		type ThreadPath,
		type Vec
	} from './dayTrack';

	const scrub = createScrubber(BOUNDS);
	const state = scrub.state;

	$: scene = $state.scene;
	$: local = $state.local;
	$: p = $state.p;
	$: reduced = !$motionEnabled;

	/** Scene N's local progress: live when owned, 1 when passed, 0 when ahead. */
	function localFor(n: number, currentScene: number, currentLocal: number): number {
		if (currentScene === n) return currentLocal;
		return currentScene > n ? 1 : 0;
	}

	/** Sub-progress within [a, b] of a local — the scene-choreography helper. */
	function seg(l: number, a: number, b: number): number {
		if (b <= a) return l >= b ? 1 : 0;
		return Math.min(1, Math.max(0, (l - a) / (b - a)));
	}

	$: sceneLocals = STATIONS.map((_, n) => (reduced ? 1 : localFor(n, scene, local)));

	// Act I era accents — scene-truth, not aurora: phosphor green for the
	// machine you owned, platform grey for the operate era, cold blue for
	// the exodus. Declared here (not in auroraPalette.ts) because that file
	// is pinned byte-for-byte to the Swift presets by its test.
	interface EraAccent {
		a: string;
		a2: string;
		ha: string;
		ah: string;
		ahs: string;
	}
	/** The birth era's accent also blooms the station's glow field. */
	interface Phosphor extends EraAccent {
		g1: string;
		g2: string;
	}
	interface ThemeAccent extends EraAccent {
		g1: string;
		g2: string;
		g3: string;
	}
	const ERA_ACCENTS: Record<string, EraAccent> = {
		grey: {
			a: '#97a0b4',
			a2: '#68707f',
			ha: '#aab3c4',
			ah: 'rgba(151, 160, 180, 0.14)',
			ahs: 'rgba(151, 160, 180, 0.3)'
		},
		platform: {
			a: '#4f8dff',
			a2: '#2a5fd9',
			ha: '#6ba4ff',
			ah: 'rgba(79, 141, 255, 0.16)',
			ahs: 'rgba(79, 141, 255, 0.42)'
		}
	};

	// The 1977 tube's phosphor. WHICH green reads is a property of the film,
	// not of the DOM: the photoreal reel hands over a near-black neutral
	// tube, where the hot green is right, but a reel whose tube still
	// carries light would let that green glare off its own glass. The reel's
	// manifest declares its screen tint and the pick follows from it — no
	// film, no manifest, or a dark tube all land on the same first recipe,
	// which is the one that shipped.
	const PHOSPHOR: Record<'dark' | 'lit', Phosphor> = {
		dark: {
			a: '#3dff7c',
			a2: '#18a352',
			ha: '#3dff7c',
			ah: 'rgba(61, 255, 124, 0.13)',
			ahs: 'rgba(61, 255, 124, 0.32)',
			g1: 'rgba(61, 255, 124, 0.06)',
			g2: 'rgba(61, 255, 124, 0.04)'
		},
		lit: {
			a: '#18a352',
			a2: '#0c6b33',
			ha: '#2fd06a',
			ah: 'rgba(24, 163, 82, 0.16)',
			ahs: 'rgba(24, 163, 82, 0.4)',
			g1: 'rgba(24, 163, 82, 0.07)',
			g2: 'rgba(24, 163, 82, 0.05)'
		}
	};

	// The day's emotional phases now speak in the selected app theme's own
	// pair. Phase remains meaningful through balance and intensity: wake leans
	// primary, calm and speaking lean secondary, thinking sits between them,
	// and graphite settles into the theme's neutral ink. Act I's historically
	// meaningful phosphor/cold-platform colors remain scene truth above.
	const THEME_PHASE_ACCENTS: Record<AuroraPhaseName, ThemeAccent> = {
		armedEmber: {
			a: 'color-mix(in srgb, var(--accent-primary) 68%, var(--text-primary))',
			a2: 'color-mix(in srgb, var(--accent-secondary) 52%, var(--accent-primary))',
			ha: 'var(--accent-primary)',
			ah: 'color-mix(in srgb, var(--accent-primary) 12%, transparent)',
			ahs: 'color-mix(in srgb, var(--accent-primary) 28%, transparent)',
			g1: 'color-mix(in srgb, var(--accent-primary) 12%, transparent)',
			g2: 'color-mix(in srgb, var(--accent-secondary) 7%, transparent)',
			g3: 'transparent'
		},
		violetSurge: {
			a: 'var(--accent-primary)',
			a2: 'var(--accent-secondary)',
			ha: 'var(--accent-primary)',
			ah: 'color-mix(in srgb, var(--accent-primary) 18%, transparent)',
			ahs: 'color-mix(in srgb, var(--accent-primary) 42%, transparent)',
			g1: 'color-mix(in srgb, var(--accent-primary) 18%, transparent)',
			g2: 'color-mix(in srgb, var(--accent-secondary) 11%, transparent)',
			g3: 'color-mix(in srgb, var(--accent-primary) 6%, transparent)'
		},
		calmAurora: {
			a: 'var(--accent-secondary)',
			a2: 'color-mix(in srgb, var(--accent-secondary) 58%, var(--accent-primary))',
			ha: 'var(--accent-secondary)',
			ah: 'color-mix(in srgb, var(--accent-secondary) 14%, transparent)',
			ahs: 'color-mix(in srgb, var(--accent-secondary) 32%, transparent)',
			g1: 'color-mix(in srgb, var(--accent-secondary) 14%, transparent)',
			g2: 'color-mix(in srgb, var(--accent-primary) 7%, transparent)',
			g3: 'transparent'
		},
		amberThinking: {
			a: 'color-mix(in srgb, var(--accent-primary) 58%, var(--accent-secondary))',
			a2: 'color-mix(in srgb, var(--accent-secondary) 62%, var(--accent-primary))',
			ha: 'color-mix(in srgb, var(--accent-primary) 52%, var(--accent-secondary))',
			ah: 'color-mix(in srgb, var(--accent-primary) 14%, transparent)',
			ahs: 'color-mix(in srgb, var(--accent-secondary) 34%, transparent)',
			g1: 'color-mix(in srgb, var(--accent-primary) 13%, transparent)',
			g2: 'color-mix(in srgb, var(--accent-secondary) 10%, transparent)',
			g3: 'transparent'
		},
		tealSpeaking: {
			a: 'var(--accent-secondary)',
			a2: 'color-mix(in srgb, var(--accent-secondary) 72%, var(--accent-primary))',
			ha: 'var(--accent-secondary)',
			ah: 'color-mix(in srgb, var(--accent-secondary) 17%, transparent)',
			ahs: 'color-mix(in srgb, var(--accent-secondary) 38%, transparent)',
			g1: 'color-mix(in srgb, var(--accent-secondary) 17%, transparent)',
			g2: 'color-mix(in srgb, var(--accent-primary) 8%, transparent)',
			g3: 'transparent'
		},
		graphite: {
			a: 'var(--text-secondary)',
			a2: 'var(--text-muted)',
			ha: 'var(--text-secondary)',
			ah: 'color-mix(in srgb, var(--text-secondary) 9%, transparent)',
			ahs: 'color-mix(in srgb, var(--text-secondary) 20%, transparent)',
			g1: 'color-mix(in srgb, var(--text-secondary) 7%, transparent)',
			g2: 'transparent',
			g3: 'transparent'
		}
	};
	let phosphor: Phosphor = PHOSPHOR.dark;

	function pickPhosphor(tint: [number, number, number]): Phosphor {
		const lum = 0.2126 * tint[0] + 0.7152 * tint[1] + 0.0722 * tint[2];
		return lum > 96 ? PHOSPHOR.lit : PHOSPHOR.dark;
	}

	function accentStyle(station: Station, l: number, phos: Phosphor): string {
		const era = station.era === 'crt' ? phos : station.era ? ERA_ACCENTS[station.era] : null;
		if (era) {
			return [
				`--local:${l.toFixed(4)}`,
				`--a:${era.a}`,
				`--a2:${era.a2}`,
				`--ha:${era.ha}`,
				`--ah:${era.ah}`,
				`--ah-strong:${era.ahs}`
			].join(';');
		}
		const ph = THEME_PHASE_ACCENTS[station.phase];
		return [
			`--local:${l.toFixed(4)}`,
			`--a:${ph.a}`,
			`--a2:${ph.a2}`,
			`--ha:${ph.ha}`,
			`--ah:${ph.ah}`,
			`--ah-strong:${ph.ahs}`
		].join(';');
	}

	function glowStyle(station: Station, phos: Phosphor): string {
		// The continuous gradient field: one themed phase bloom per station,
		// planted IN the world so the selected primary/secondary balance is
		// literally mapped to world position. Era stations bloom in
		// their era's ink: phosphor green over the birth, cold platform blue
		// over the exodus; the operate era gets no bloom at all (grey office
		// light is the point).
		if (station.era === 'crt') {
			return `--g1:${phos.g1};--g2:${phos.g2};--g3:transparent`;
		}
		if (station.era === 'platform') {
			return `--g1:rgba(79, 141, 255, 0.1);--g2:rgba(42, 95, 217, 0.08);--g3:rgba(79, 141, 255, 0.05)`;
		}
		const ph = THEME_PHASE_ACCENTS[station.phase];
		return [
			`--g1:${ph.g1}`,
			`--g2:${ph.g2}`,
			`--g3:${ph.g3}`
		].join(';');
	}

	// Canvas paint cannot consume CSS custom properties directly. Rebuild reads
	// the selected theme's primary/secondary tokens into this normalized palette;
	// the existing data-theme observer repeats that read on every live switch.
	let threadTheme = themeThreadPalette('', '');

	// Mobile factors: phones travel a mostly-vertical dolly (x squeezed) with
	// tighter station spacing (y squeezed) so travel legs never read empty.
	let xf = 1;
	let yf = 1;

	function jumpTo(n: number): void {
		const track = document.getElementById('movie-track');
		if (!track) return;
		const top =
			track.getBoundingClientRect().top +
			window.scrollY +
			BOUNDS[n] * (track.offsetHeight - window.innerHeight) +
			2;
		window.scrollTo({ top, behavior: reduced ? 'auto' : 'smooth' });
	}

	// ── Station 1 (born): THREE phosphor lines, typed at reading pace, and
	// they are the film's thesis stated before the loss — tools were
	// personal, this machine was the most personal of them, and it answered
	// exactly one person. Past tense on the last line is the point: it sets
	// up the exodus that takes it away. Typing only begins after the camera
	// has pulled back off the match cut and the whole system is in frame,
	// so nothing is typed at a monitor the viewer cannot see the shape of.
	const DELEGATE_ASK = 'book the Kyoto trip we discussed — same dates, aisle seat';
	$: delegateTyped = DELEGATE_ASK.slice(
		0,
		Math.round(seg(sceneLocals[SI.ask], 0.29, 0.58) * DELEGATE_ASK.length)
	);
	const PLAN = ['Find fares for 12–16 Nov', 'Book the aisle seat', 'Add to your calendar'];

	// ── Station 5 (knows): the pulse number ────────────────────────────────
	$: pulseCount = 3 + Math.round(seg(sceneLocals[SI.knows], 0.15, 0.6) * 4);

	// ── Station 9 (lunch): the Swiggy order, memory-guarded ────────────────
	// The ask lands, the healthy streak is recalled, the reconfirm question
	// is ASKED (and answered), and only then does the order exist.
	$: sw = sceneLocals[SI.lunch];

	// ── Station 10 (thinks): the messy ask, typed as you scroll — the graph
	// fans only after the question has finished forming ────────────────────
	const BRAINSTORM_ASK = '@brainstorm a second income, without quitting my job';
	$: brainstormTyped = BRAINSTORM_ASK.slice(
		0,
		Math.round(seg(sceneLocals[SI.thinks], 0.05, 0.22) * BRAINSTORM_ASK.length)
	);

	// ── Station 11 (orb): the phase walk (finishes before departure) ───────
	// Five equal segments across the enlarged dwell (weight 3.0, departure
	// at 0.82): each phase holds ~0.15 of local — nearly half a viewport of
	// scroll — so the word AND its palette get room to breathe. The same
	// 0..0.76 mapping feeds the canvas, so color and word agree everywhere.
	const ORB_WORDS = ['Armed', 'Waking…', 'Listening', 'Thinking', 'Speaking'];
	$: orbWalk = seg(sceneLocals[SI.orb], 0, 0.76);
	$: orbWord = ORB_WORDS[Math.min(4, Math.floor(orbWalk * 5))];
	$: orbNear = scene >= SI.thinks && scene <= SI.pocket;

	function voiceOrbPaintStyle(phase: AuroraPhase): string {
		const first = phase.stops[0];
		const last = phase.stops[phase.stops.length - 1];
		return [
			`--voice-orb-highlight:${rgb(mixStops(first, { r: 1, g: 1, b: 1 }, 0.28))}`,
			`--voice-orb-a:${rgb(first)}`,
			`--voice-orb-b:${rgb(last)}`,
			`--voice-orb-halo:${rgba(phase.halo, Math.max(0.12, phase.haloStrength * 0.35))}`,
			`--voice-orb-halo-strong:${rgba(phase.halo, Math.max(0.2, phase.haloStrength * 0.7))}`
		].join(';');
	}

	$: currentVoiceOrbPhase = VOICE_ORB_SEQUENCE[Math.min(4, Math.floor(orbWalk * 5))];
	$: voiceOrbStageStyle = voiceOrbPaintStyle(reduced ? AURORA.calmAurora : currentVoiceOrbPhase);
	const VOICE_ORB_LANDING_STYLE = voiceOrbPaintStyle(VOICE_ORB_LANDING_PHASE);

	// ── Station 12 (pocket): the lock-screen wake, beat by beat ────────────
	// The real contract from ambient-mode.md: armed → "hey presto" → the
	// island surges violet and expands (orb + word + leash ring) → the whole
	// conversation happens while the phone STAYS locked.
	$: pk = sceneLocals[SI.pocket];
	$: pocketWord = pk < 0.26 ? 'Waking…' : pk < 0.42 ? 'Listening' : pk < 0.58 ? 'Thinking' : 'Speaking';
	$: pocketVoiceOrbPhase = reduced
		? AURORA.calmAurora
		: pk < 0.26
			? AURORA.violetSurge
			: pk < 0.42
				? AURORA.calmAurora
				: pk < 0.58
					? AURORA.amberThinking
					: AURORA.tealSpeaking;
	$: pocketVoiceOrbStyle = voiceOrbPaintStyle(pocketVoiceOrbPhase);
	// The exchange is ONE lock-screen notification whose line mutates —
	// beat 1 "Listening", beat 2 your transcript, beat 3 Magican's reply.
	// Thresholds shared with the island word so card and island agree:
	// Listening while the island listens, transcript while it thinks,
	// the reply as it speaks.
	$: pocketBeat = pk < 0.42 ? 1 : pk < 0.58 ? 2 : 3;
	// The notification glyph is the departing orb's landing pad — and it
	// does NOT exist before the orb arrives: the card has no glyph at all
	// until touchdown, when the shrunken orb melts into a surging glyph in
	// the same spot (the surge window opens exactly as the carrier's fade
	// begins, local 0.975 vs 0.979). Settled icon once the wake owns the
	// screen.
	$: orbLanding = !reduced && ((scene === SI.orb && sceneLocals[SI.orb] > 0.975) || (scene === SI.pocket && pk < 0.22));
	// The flight must PAINT above the pocket station: the pocket scene is a
	// later sibling whose transform makes its own stacking context, so
	// without a z raise the shrinking orb slides BEHIND the phone screen.
	// Active for the whole leg; the carrier has fully faded (its opacity
	// curve ends at local 0.99) before the playhead flips to scene 12.
	$: orbFlying = !reduced && scene === SI.orb && local > 0.8;

	// ── Station 14 (zepto): the first-time MCP login, then the order ───────
	$: zp = sceneLocals[SI.zepto];

	const PYTH_CAPTION = 'what do the two small squares add up to?';

	// ── Lit-by-touch: elements the thread passes light in its hue ──────────
	let marks: Record<string, number> = {};
	$: litMem = !reduced && marks.pickup !== undefined && p >= marks.pickup - 0.004;
	$: litPhone = !reduced && marks.phone !== undefined && p >= marks.phone - 0.003;
	$: litComposer = !reduced && marks.composer !== undefined && p >= marks.composer - 0.003;
	$: litPlan = !reduced && marks.split !== undefined && p >= marks.split - 0.002;
	$: litGate = !reduced && marks.gate !== undefined && p >= marks.gate - 0.002;
	$: litTile = !reduced && marks.tile !== undefined && p >= marks.tile + 0.008;
	$: litHealthy = !reduced && marks.healthy !== undefined && p >= marks.healthy - 0.002;
	$: litConfirm = !reduced && marks.confirm !== undefined && p >= marks.confirm - 0.002;
	$: litBrief = !reduced && marks.brief !== undefined && p >= marks.brief - 0.002;
	$: litMcp = !reduced && marks.mcp !== undefined && p >= marks.mcp - 0.002;
	$: litZorder = !reduced && marks.zorder !== undefined && p >= marks.zorder - 0.002;
	$: litIsland = !reduced && marks.island !== undefined && p >= marks.island - 0.002;
	$: litReceipt = !reduced && marks.receipt !== undefined && p >= marks.receipt - 0.002;

	// ── The thread-map rail: the journey's shape as the playhead ───────────
	const MAP_W = 18;
	const MAP_H = 280;
	const mapXs = STATIONS.map((s) => s.x);
	const MAP_MIN_X = Math.min(...mapXs);
	const MAP_SPAN_X = Math.max(...mapXs) - MAP_MIN_X;
	// The world now starts BEFORE zero (the prologue lives at negative y),
	// so the map normalizes against the full min..max span, not 0..max.
	const mapYs = STATIONS.map((s) => s.y);
	const MAP_MIN_Y = Math.min(...mapYs);
	const MAP_SPAN_Y = Math.max(...mapYs) - MAP_MIN_Y;
	const mapPt = (s: Station): Vec => ({
		x: ((s.x - MAP_MIN_X) / MAP_SPAN_X) * MAP_W,
		y: ((s.y - MAP_MIN_Y) / MAP_SPAN_Y) * MAP_H
	});
	const MAP_POINTS = STATIONS.map(mapPt);
	const MAP_PATH = MAP_POINTS.map((v, i) => `${i === 0 ? 'M' : 'L'} ${v.x.toFixed(1)} ${v.y.toFixed(1)}`).join(' ');

	// ── Runtime (non-reactive, the VoiceOrbCanvas pattern): rAF owns the ───
	// camera, the thread canvas, the fragment chip and the map head; the
	// scroll store only feeds `target`. Easing target→p each frame is what
	// makes the whole film feel steered rather than dragged.
	let trackEl: HTMLElement | null = null;
	let stageEl: HTMLElement | null = null;
	let cameraEl: HTMLElement | null = null;
	let worldEl: HTMLElement | null = null;
	let canvasEl: HTMLCanvasElement | null = null;
	let underEl: HTMLCanvasElement | null = null;
	let fragEl: HTMLElement | null = null;
	let mapHeadEl: HTMLElement | null = null;
	let periodEl: HTMLElement | null = null;
	let sceneEls: HTMLElement[] = [];

	const rt: {
		ctx: CanvasRenderingContext2D | null;
		uctx: CanvasRenderingContext2D | null;
		raf: number;
		running: boolean;
		visible: boolean;
		mounted: boolean;
		reduced: boolean;
		target: number;
		p: number;
		snapped: boolean;
		last: number;
		t: number;
		w: number;
		h: number;
		cw: number;
		ch: number;
		xf: number;
		yf: number;
		positions: Vec[];
		bows: number[];
		thread: ThreadPath | null;
		fine: boolean;
		cx: number | null;
		cy: number | null;
		pullX: number;
		pullY: number;
		onLight: boolean;
		bornTxt: number;
	} = {
		ctx: null,
		uctx: null,
		raf: 0,
		running: false,
		visible: false,
		mounted: false,
		reduced: false,
		target: 0,
		p: 0,
		snapped: false,
		last: 0,
		t: 0,
		w: 0,
		h: 0,
		cw: 0,
		ch: 0,
		xf: 1,
		yf: 1,
		positions: [],
		bows: [],
		thread: null,
		fine: false,
		cx: null,
		cy: null,
		pullX: 0,
		pullY: 0,
		onLight: true,
		bornTxt: 1
	};

	// Foreground dust: a handful of large, soft specks drawn on the 2D
	// canvas ABOVE the cards at depth > 1 — they cross the frame faster
	// than the world, which is what sells the camera as being IN the space
	// rather than looking at it. Static seeds so reloads are stable.
	const SPECKS = (() => {
		let a = 987654321;
		const rand = (): number => {
			a ^= a << 13;
			a ^= a >>> 17;
			a ^= a << 5;
			return ((a >>> 0) % 10000) / 10000;
		};
		return Array.from({ length: 14 }, () => ({
			x: rand(),
			y: rand(),
			d: 1.12 + 0.42 * rand(),
			r: 3 + 6 * rand(),
			a: 0.025 + 0.035 * rand()
		}));
	})();

	// ── The prologue reel: generated footage, scrubbed ───────────────────
	//
	// The 2026-08-04 owner verdict retired the hand-drawn Act 0. One
	// continuous shot now covers the WHOLE prologue span — the four
	// prologue stations stop owning separate drawings and become aria/rail
	// stops riding the same footage. Frame 1 sits at the very top of the
	// film and the LAST frame lands exactly on the born station's arrival,
	// where the camera is at z = 1 and the station's depth scale is exactly
	// 1 — so a viewport pixel and a station pixel are the same pixel, and
	// the footage's beige CRT and the DOM CRT can be the same object.
	function anchorWorld(el: HTMLElement, n: number, ax: number, ay: number): Vec | null {
		const sceneEl = sceneEls[n];
		if (!sceneEl) return null;
		const sr = sceneEl.getBoundingClientRect();
		const er = el.getBoundingClientRect();
		if (sr.width === 0 || er.width === 0) return null;
		const s = sr.width / rt.w;
		return {
			x: rt.positions[n].x + (er.left + er.width * ax - (sr.left + sr.width / 2)) / s,
			y: rt.positions[n].y + (er.top + er.height * ay - (sr.top + sr.height / 2)) / s
		};
	}

	$: rt.target = p;
	$: {
		rt.reduced = reduced;
		syncLoop();
	}

	function syncLoop(): void {
		const should = rt.mounted && rt.visible && !rt.reduced;
		if (should && !rt.running) {
			rt.running = true;
			rt.last = 0;
			if (!rt.snapped) {
				// Land exactly where the scroll already is — no fly-in on a
				// mid-page reload.
				rt.p = rt.target;
				rt.snapped = true;
			}
			rt.raf = requestAnimationFrame(frame);
		} else if (!should && rt.running) {
			rt.running = false;
			if (rt.raf) cancelAnimationFrame(rt.raf);
			rt.raf = 0;
		}
	}

	function computeGeometry(): void {
		rt.w = window.innerWidth;
		rt.h = window.innerHeight;
		xf = rt.w <= 640 ? 0.35 : 1;
		yf = rt.w <= 640 ? 0.88 : 1;
		rt.xf = xf;
		rt.yf = yf;
		rt.positions = STATIONS.map((s) => ({
			x: (s.x * xf * rt.w) / 100,
			y: (s.y * yf * rt.h) / 100
		}));
		rt.bows = LEG_BOW.map((b) => (b * xf * rt.w) / 100);
	}

	function fitCanvas(): void {
		if (!canvasEl || !stageEl) return;
		const rect = stageEl.getBoundingClientRect();
		const dpr = Math.min(2, window.devicePixelRatio || 1);
		rt.cw = rect.width;
		rt.ch = rect.height;
		canvasEl.width = Math.round(rect.width * dpr);
		canvasEl.height = Math.round(rect.height * dpr);
		rt.ctx = canvasEl.getContext('2d');
		rt.ctx?.setTransform(dpr, 0, 0, dpr, 0, 0);
		// The under-canvas: same size, same projection, painted BENEATH the
		// stations so behind-flagged thread segments dive under objects.
		if (underEl) {
			underEl.width = canvasEl.width;
			underEl.height = canvasEl.height;
			rt.uctx = underEl.getContext('2d');
			rt.uctx?.setTransform(dpr, 0, 0, dpr, 0, 0);
		}
	}

	// Whether the theme ground is paper (light) or deep — mirrored into a
	// class so the CSS card chrome can branch too: some light themes zero
	// their --landing-task-card-* tokens, so the film owns its card chrome.
	let groundLight = true;

	function measureGround(): void {
		// The film paints on whatever ground the theme provides; the canvas
		// thread probes that ground's luminance once per rebuild to choose
		// its stroke recipe — ink-core on paper, light-core on deep themes.
		if (!trackEl) return;
		const m = getComputedStyle(trackEl).backgroundColor.match(/[\d.]+/g);
		if (!m || m.length < 3) return;
		rt.onLight = 0.2126 * +m[0] + 0.7152 * +m[1] + 0.0722 * +m[2] > 140;
		groundLight = rt.onLight;
	}

	function measureThreadTheme(): void {
		if (!trackEl) return;
		const style = getComputedStyle(trackEl);
		threadTheme = themeThreadPalette(
			style.getPropertyValue('--accent-primary'),
			style.getPropertyValue('--accent-secondary')
		);
	}

	function rebuild(): void {
		if (typeof window === 'undefined') return;
		computeGeometry();
		measureGround();
		measureThreadTheme();
		rt.thread = buildThread(rt.positions, rt.w, rt.h, rt.xf, rt.yf);
		marks = rt.thread.marks;
		fitCanvas();
	}

	/**
	 * Hand the born station the footage's own CRT geometry. The footage is
	 * the fixed thing: the DOM machine is sized and placed from the tube
	 * measured in the final frame, so the match cut lands on every viewport
	 * — including the phone, where the film runs as a bounded-crop band.
	 * Written on rebuild and when the manifest resolves; never per frame.
	 */
	function project(cam: Camera, v: Vec): Vec {
		return { x: rt.cw / 2 + (v.x - cam.x) * cam.z, y: rt.ch / 2 + (v.y - cam.y) * cam.z };
	}

	function frame(now: number): void {
		if (!rt.running) return;
		rt.raf = requestAnimationFrame(frame);
		const dt = rt.last ? Math.min(0.06, (now - rt.last) / 1000) : 0.016;
		rt.last = now;
		rt.t = now / 1000;
		rt.p += (rt.target - rt.p) * (1 - Math.exp(-dt * 9));
		if (Math.abs(rt.target - rt.p) < 0.0001) rt.p = rt.target;

		const cam = cameraAt(rt.p, rt.positions, rt.bows);
		if (cameraEl) cameraEl.style.transform = `scale(${cam.z.toFixed(4)})`;
		if (worldEl)
			worldEl.style.transform = `translate3d(${(-cam.x).toFixed(2)}px, ${(-cam.y).toFixed(2)}px, 0)`;
		placeDepth(cam);
		drawThread(cam);
		placeMapHead(cam);
	}

	function placeDepth(cam: Camera): void {
		// Depth of field: stations are flown INTO, not presented — each one
		// scales, sharpens and brightens as the camera closes on it, and
		// recedes into soft blur as the camera leaves. Compositor-friendly:
		// transform + opacity always; blur only while actually soft.
		//
		// One exception: the departing orb rides INSIDE scene 11, so that
		// scene's depth dimming would swallow the touchdown — floor its
		// depth term by the flight's own progress so the shrinking orb
		// stays bright and sharp all the way onto the notification glyph.
		const rs = resolveScene(BOUNDS, rt.p);
		const fly = rs.scene === SI.orb ? clamp01((rs.local - 0.8) / 0.19) : 0;
		for (let i = 0; i < STATIONS.length; i++) {
			const el = sceneEls[i];
			if (!el) continue;
			const pos = rt.positions[i];
			const dx = (pos.x - cam.x) / (rt.w * 0.85);
			const dy = (pos.y - cam.y) / (rt.h * 0.9);
			const d = Math.hypot(dx, dy);
			const prox = Math.max(0, 1 - d);
			let e = prox * prox * (3 - 2 * prox);
			if (i === SI.orb) e = Math.max(e, fly);
			el.style.opacity = ((0.3 + 0.7 * e)).toFixed(3);
			el.style.transform = `translate(-50%, -50%) scale(${(0.9 + 0.1 * e).toFixed(4)})`;
			el.style.filter = e > 0.96 ? 'none' : `blur(${((1 - e) * 3.2).toFixed(2)}px)`;
		}
	}

	const clamp01 = (v: number): number => Math.min(1, Math.max(0, v));

	function drawSpecks(cam: Camera): void {
		// The near-field dust layer, above the cards. Depth > 1: it crosses
		// faster than the world and puts the viewer INSIDE the atmosphere.
		const ctx = rt.ctx;
		if (!ctx) return;
		const W = rt.cw;
		const H = rt.ch;
		const core = rt.onLight ? '62, 44, 96' : '226, 216, 255';
		for (const s of SPECKS) {
			const sx = ((((s.x * W - cam.x * s.d) % W) + W) % W);
			const sy = ((((s.y * H - cam.y * s.d) % H) + H) % H);
			// Soft-edged: a hard circle reads as dirt on paper; a gradient
			// reads as dust in the air.
			const g = ctx.createRadialGradient(sx, sy, 0, sx, sy, s.r);
			g.addColorStop(0, `rgba(${core}, ${s.a.toFixed(3)})`);
			g.addColorStop(1, `rgba(${core}, 0)`);
			ctx.fillStyle = g;
			ctx.beginPath();
			ctx.arc(sx, sy, s.r, 0, Math.PI * 2);
			ctx.fill();
		}
	}

	function drawThread(cam: Camera): void {
		const ctx = rt.ctx;
		const path = rt.thread;
		if (!ctx || !path) return;
		ctx.clearRect(0, 0, rt.cw, rt.ch);
		rt.uctx?.clearRect(0, 0, rt.cw, rt.ch);
		drawSpecks(cam);
		const m = path.marks;
		const born = clamp01((rt.p - m.ignite) / 0.02);
		if (born <= 0) {
			if (fragEl) fragEl.style.opacity = '0';
			return;
		}

		const TRAIL = 0.035;
		let tail = Math.max(m.ignite, rt.p - TRAIL);
		// The finale must read as a CLOSED loop: once the head enters the
		// receipt circle, the tail pins at the circle's start so the whole
		// ring stays lit through the tie.
		if (m.loopStart !== undefined && rt.p > m.loopStart) {
			tail = Math.max(m.ignite, Math.min(tail, m.loopStart));
		}
		const K = 64;
		const pts: Vec[] = [];
		// Per-sample depth: 0 = in front (the usual grammar), 1 = diving
		// behind a station object — those segments render on the under-
		// canvas beneath the world instead.
		const bs: number[] = [];
		for (let i = 0; i <= K; i++) {
			const pp = tail + (rt.p - tail) * (i / K);
			const s = project(cam, sampleThread(path.main, pp));
			// Alive: a slow breathing sway, strongest at the head.
			const g = i / K;
			s.x += Math.sin(pp * 520 + rt.t * 1.7) * 2.6 * g;
			s.y += Math.cos(pp * 470 + rt.t * 1.3) * 2.1 * g;
			pts.push(s);
			bs.push(rt.uctx ? behindAt(path.behind, pp) : 0);
		}

		// Serendipity: the head leans a few px toward the cursor (desktop).
		if (rt.fine && rt.cx !== null && rt.cy !== null) {
			const head = pts[K];
			const tx = Math.max(-9, Math.min(9, (rt.cx - head.x) * 0.03));
			const ty = Math.max(-9, Math.min(9, (rt.cy - head.y) * 0.03));
			rt.pullX += (tx - rt.pullX) * 0.06;
			rt.pullY += (ty - rt.pullY) * 0.06;
		} else {
			rt.pullX *= 0.94;
			rt.pullY *= 0.94;
		}
		for (let i = 0; i <= K; i++) {
			const g = (i / K) ** 3;
			pts[i].x += rt.pullX * g;
			pts[i].y += rt.pullY * g;
		}

		// Inside the orb the thread hands its light over — the coil IS the
		// reveal, so the strokes dim while the orb blooms. Same handoff at
		// the pocket: while the locked phone talks, the island carries it.
		const inOrb =
			m.coilEnd !== undefined && rt.p > m.coilEnd && rt.p < m.emerge
				? 0.18
				: m.pocketIn !== undefined && rt.p > m.pocketIn && rt.p < m.pocketOut
					? 0.1
					: 1;

		const halo = threadTheme.primary;
		const companion = threadTheme.secondary;
		const glowHalo = threadTheme.glowPrimary;
		const glowCompanion = threadTheme.glowSecondary;
		// Four passes, one grammar, two grounds. The outer pair use luminance-
		// raised versions of the selected theme colors: proportional channel
		// scaling preserves hue (rather than washing dark themes toward pastel)
		// while finally making the path read as emitted light. The inner pair
		// retain a crisp theme-colored filament and the ink/light contrast core.
		const core = rt.onLight ? threadTheme.ink : threadTheme.light;
		const strokeSeg = (
			c: CanvasRenderingContext2D,
			i: number,
			width: number,
			color: string
		): void => {
			c.beginPath();
			c.moveTo(pts[i - 1].x, pts[i - 1].y);
			c.lineTo(pts[i].x, pts[i].y);
			c.strokeStyle = color;
			c.lineWidth = width;
			c.lineCap = 'round';
			c.stroke();
		};
		const pass = (base: typeof halo, width: number, alpha: number, tint: number): void => {
			const col = mixStops(base, core, tint);
			for (let i = 1; i <= K; i++) {
				const g = i / K;
				const a = Math.pow(g, 1.6) * alpha * born * inOrb;
				if (a < 0.01) continue;
				// Split each segment across the two canvases by behindness —
				// the alpha feather at span edges makes the dive read smooth.
				const b = bs[i];
				if (b < 0.99) strokeSeg(ctx, i, width, rgba(col, a * (1 - b)));
				if (rt.uctx && b > 0.01) strokeSeg(rt.uctx, i, width, rgba(col, a * b));
			}
		};
		if (rt.onLight) {
			pass(glowCompanion, 18, 0.18, 0);
			pass(glowHalo, 8, 0.3, 0.06);
			pass(halo, 3.2, 0.88, 0.12);
			pass(glowHalo, 1.1, 1, 0.5);
		} else {
			pass(glowCompanion, 20, 0.24, 0);
			pass(glowHalo, 9, 0.38, 0.03);
			pass(halo, 3.4, 0.92, 0.14);
			pass(glowHalo, 1.2, 1, 0.58);
		}

		// The head: a small star with a bloom — the protagonist's face. It
		// obeys the same depth as its segment: while diving it renders on
		// the under-canvas, split-alpha through the feather.
		const head = pts[K];
		const hr = 44 * born * inOrb;
		const bHead = bs[K];
		const drawHead = (c: CanvasRenderingContext2D, k: number): void => {
			const bloom = c.createRadialGradient(head.x, head.y, 0, head.x, head.y, hr);
			bloom.addColorStop(0, rgba(glowHalo, 0.88 * inOrb * k));
			bloom.addColorStop(0.3, rgba(glowHalo, 0.42 * inOrb * k));
			bloom.addColorStop(0.64, rgba(glowCompanion, 0.15 * inOrb * k));
			bloom.addColorStop(1, rgba(halo, 0));
			c.fillStyle = bloom;
			c.beginPath();
			c.arc(head.x, head.y, hr, 0, Math.PI * 2);
			c.fill();
			// The face: white-hot star on deep ground, ink star on paper.
			c.fillStyle = rt.onLight
				? rgba(mixStops(glowHalo, threadTheme.ink, 0.28), 0.98 * born * inOrb * k)
				: rgba(threadTheme.light, 0.95 * born * inOrb * k);
			c.beginPath();
			c.arc(head.x, head.y, 2.6, 0, Math.PI * 2);
			c.fill();
			for (let s = 0; s < 2; s++) {
				const a = rt.t * (1.6 + s * 0.7) + s * Math.PI;
				c.fillStyle = rgba(
					rt.onLight ? mixStops(glowCompanion, threadTheme.ink, 0.2) : glowCompanion,
					0.55 * born * inOrb * k
				);
				c.beginPath();
				c.arc(head.x + Math.cos(a) * 9, head.y + Math.sin(a) * 7, 1.3, 0, Math.PI * 2);
				c.fill();
			}
		};
		if (hr > 1) {
			if (bHead < 0.99) drawHead(ctx, 1 - bHead);
			if (rt.uctx && bHead > 0.01) drawHead(rt.uctx, bHead);
		}

		// The plan's other two strands, alive only around the split.
		for (const st of path.strands) {
			if (rt.p < st.p0 || rt.p > st.p1) continue;
			const env =
				Math.min(clamp01((rt.p - st.p0) / 0.006), clamp01((st.p1 - rt.p) / 0.02)) * 0.7;
			const reach = clamp01((rt.p - st.p0) / (st.p1 - st.p0 - 0.015));
			ctx.beginPath();
			for (let i = 0; i <= 20; i++) {
				const s = project(cam, sampleThread(st.pts, (i / 20) * reach));
				if (i === 0) ctx.moveTo(s.x, s.y);
				else ctx.lineTo(s.x, s.y);
			}
			ctx.strokeStyle = rgba(
				rt.onLight ? mixStops(glowHalo, threadTheme.ink, 0.32) : glowHalo,
				env
			);
			ctx.lineWidth = 2;
			ctx.stroke();
		}

		placeFrag(cam, head, m, bHead);
	}

	function placeFrag(cam: Camera, head: Vec, m: Record<string, number>, behind: number): void {
		// The memory fragment: picked up at the knows station, orbiting the
		// head until it snaps into the plan (the moment plan line 2 lights).
		if (!fragEl || m.pickup === undefined) return;
		const on = rt.p > m.pickup && rt.p < m.split + 0.004;
		if (!on) {
			fragEl.style.opacity = '0';
			return;
		}
		const fadeIn = clamp01((rt.p - m.pickup) / 0.008);
		const fadeOut = clamp01((m.split + 0.004 - rt.p) / 0.01);
		const a = rt.t * 1.5;
		// Tighter orbit on phones so the chip never blankets card copy.
		const or = rt.w <= 640 ? 16 : 30;
		const x = head.x + Math.cos(a) * or;
		const y = head.y + (Math.sin(a) * or) / 1.9;
		// The chip is DOM above the world — it cannot dive, so it dims out
		// while the head it orbits is behind an object.
		fragEl.style.opacity = String(Math.min(fadeIn, fadeOut) * (1 - behind));
		fragEl.style.transform = `translate3d(${x.toFixed(1)}px, ${y.toFixed(1)}px, 0) translate(-50%, -130%)`;
	}

	function placeMapHead(cam: Camera): void {
		if (!mapHeadEl) return;
		const vwPx = (rt.w / 100) * rt.xf;
		const vhPx = (rt.h / 100) * rt.yf;
		const xvw = vwPx > 0 ? cam.x / vwPx : 0;
		const yvh = vhPx > 0 ? cam.y / vhPx : 0;
		const mx = ((xvw - MAP_MIN_X) / MAP_SPAN_X) * MAP_W;
		const my = ((yvh - MAP_MIN_Y) / MAP_SPAN_Y) * MAP_H;
		mapHeadEl.style.transform = `translate3d(${mx.toFixed(1)}px, ${my.toFixed(1)}px, 0) translate(-50%, -50%)`;
	}

	let io: IntersectionObserver | null = null;
	let themeObs: MutationObserver | null = null;
	let onResize: (() => void) | null = null;
	let onPointer: ((e: PointerEvent) => void) | null = null;

	onMount(() => {
		rt.mounted = true;
		// The film is settled here, before anything asks for a frame: the reel
		// only fetches its manifest once the rAF is running, which is strictly
		// after this. Reading the URL and storage on the client alone also
		// keeps the server's render and the first client render identical.
		// Idempotent — the chrome's selector asks for the same thing on mount.
		rebuild();
		document.fonts?.ready.then(() => rebuild());
		onResize = () => rebuild();
		window.addEventListener('resize', onResize, { passive: true });
		// The ThemeSwitcher sits on this very page: a live theme flip changes
		// the ground under the film, so the canvas must re-probe it.
		themeObs = new MutationObserver(() => rebuild());
		themeObs.observe(document.documentElement, {
			attributes: true,
			attributeFilter: ['data-theme']
		});
		rt.fine = window.matchMedia('(pointer: fine)').matches;
		if (rt.fine) {
			onPointer = (e: PointerEvent) => {
				rt.cx = e.clientX;
				rt.cy = e.clientY;
			};
			window.addEventListener('pointermove', onPointer, { passive: true });
		}
		if (trackEl) {
			io = new IntersectionObserver(
				(entries) => {
					rt.visible = entries[0]?.isIntersecting ?? false;
					syncLoop();
				},
				{ threshold: 0 }
			);
			io.observe(trackEl);
		} else {
			rt.visible = true;
			syncLoop();
		}
	});

	onDestroy(() => {
		rt.mounted = false;
		rt.running = false;
		if (rt.raf) cancelAnimationFrame(rt.raf);
		if (typeof window !== 'undefined') {
			if (onResize) window.removeEventListener('resize', onResize);
			if (onPointer) window.removeEventListener('pointermove', onPointer);
		}
		io?.disconnect();
		themeObs?.disconnect();
	});
</script>

<div
	id="day-track"
	class="mt-track"
	class:mt-onlight={groundLight}
	class:static={reduced}
	style={reduced ? '' : `height:${TRACK_SVH}svh`}
	use:scrub.track
	bind:this={trackEl}
>
	<div class="mt-stage" class:static={reduced} bind:this={stageEl}>
		{#if !reduced}
			<!-- The thread's under-canvas: behind-flagged segments render here,
			     BENEATH the stations, so the thread can dive under objects. -->
			<canvas class="mt-thread mt-thread-under" bind:this={underEl} aria-hidden="true"></canvas>
		{/if}
		<div class="mt-camera" class:static={reduced} bind:this={cameraEl}>
			<div class="mt-world" class:static={reduced} bind:this={worldEl}>
				{#if !reduced}
					<!-- The continuous field: hue mapped to world position. The
					     operate era gets none — grey office light is the point. -->
					{#each STATIONS as station, gi (station.id)}
						{#if station.era !== 'grey'}
							<div
								class="mt-glow"
								class:mt-glow-pro={gi < SI.born}
								style="left:{station.x * xf}vw;top:{station.y * yf}vh;{glowStyle(
									station,
									phosphor
								)}"
								aria-hidden="true"
							></div>
						{/if}
					{/each}
				{/if}

				{#each STATIONS as station, n (station.id)}
					<section
						class="mt-scene mt-s-{station.id}"
						class:on={reduced || scene === n}
						class:static={reduced}
						class:mt-depart={n === SI.orb && orbFlying}
						style="{reduced ? '' : `left:${station.x * xf}vw;top:${station.y * yf}vh;`}{accentStyle(
							station,
							sceneLocals[n],
							phosphor
						)}"
						aria-label={station.clock
							? `${station.clock} — ${station.title}`
							: station.title || station.aria}
						bind:this={sceneEls[n]}
					>
						<!-- Born prints its OWN label pair, on the tube, in the
						     machine's font — so the generic head is suppressed
						     there and nowhere else. -->
						{#if station.clock && station.id !== 'born'}
							<header class="mt-chapter-head">
								<span class="mt-kicker" aria-hidden="true">
									<span class="mt-clock">{station.clock}</span>
									<span class="mt-kicker-rule"></span>
									<!-- No `?? station.id` fallback. That was safe while NO station
									     carried a label and the id read as a kicker; now that some
									     stations name the role the scene actually shows and the rest
									     deliberately name nothing, falling back would print internal
									     ids — `knows`, `orb`, `pocket` — beside real copy. A station
									     with nothing true to say says nothing. -->
									{#if station.label}
										<span class="mt-chapname">{station.label}</span>
									{/if}
								</span>
								<h3 class="mt-title">
									{#each station.title.split(' ') as word, wi}<span
											class="mt-w"
											style="--wi:{wi}">{word}</span
										>{#if wi < station.title.split(' ').length - 1}{' '}{/if}{/each}
								</h3>
							</header>
						{/if}

						{#if station.id === 'knows'}
							<div class="mt-panel mt-today">
								<div class="mt-panel-head">
									<span class="mt-live-dot" aria-hidden="true"></span> Today
									<span class="mt-pulse-chip">{pulseCount} moving</span>
								</div>
								{#each ['Standup notes filed — two decisions affect you', 'Visa appointment window opened — earliest is Thursday', 'The Kyoto fare you watched dropped 12%', 'Priya replied — draft answer ready in your tone'] as row, i}
									<div class="mt-row" style="--i:{i}">
										<span class="mt-row-dot" aria-hidden="true"></span>
										<span class="mt-row-text">{row}</span>
										{#if i === 3}<span class="mt-row-tag">draft ready</span>{/if}
									</div>
								{/each}
							</div>
							<div class="mt-memcard" class:mt-lit={litMem}>
								<span class="mt-memcard-kicker">remembered · 3 weeks ago</span>
								Prefers the aisle seat — noted once, never asked again.
							</div>
							<p class="mt-caption">It read the morning so you don’t have to.</p>
						{:else if station.id === 'ask'}
							<div class="mt-delegate">
								<div class="mt-doc">
									<p>
										Flights are holding steady this week, but
										<mark class="mt-selection">the Kyoto trip we discussed</mark>
										still isn’t booked and the dates are drifting closer.
									</p>
									<!-- the browser's own right-click: the Chrome contextual
									     helper's menu over the selection — delegation lives in
									     the surface you were already using. -->
									<div class="mt-ctxmenu" aria-hidden="true">
										<span class="mt-ctx-item mt-ctx-ask"><i>✦</i> Ask Magican</span>
										<span class="mt-ctx-item">Rewrite</span>
										<span class="mt-ctx-item">Draft reply</span>
									</div>
								</div>
								<!-- "anywhere" means the phone too: the Magican keyboard — normal
								     iOS typing until the ✦ key reveals the agentic row, and Ask
								     raises the same ask right there. The thread carries it INTO
								     the desktop composer below. -->
								<div class="mt-miniphone" aria-hidden="true">
									<div class="mt-mini-screen" class:mt-lit={litPhone}>
										<span class="mt-mini-island"></span>
										<p class="mt-mini-field">book the Kyoto trip we discussed…</p>
										<div class="mt-mini-actions">
											<span class="mt-mini-act mt-mini-act-on">Ask</span><span
												class="mt-mini-act">Rewrite</span
											><span class="mt-mini-act">Add task</span>
										</div>
										<div class="mt-mini-kbd">
											{#each [10, 9, 7] as cols, ri (ri)}
												<div class="mt-kbd-row">
													{#each Array(cols) as _, ki (ki)}<span class="mt-key"></span>{/each}
												</div>
											{/each}
											<div class="mt-kbd-row">
												<span class="mt-key mt-key-star">✦</span>
												<span class="mt-key mt-key-space"></span>
												<span class="mt-key mt-key-go"></span>
											</div>
										</div>
									</div>
									<span class="mt-mini-tag mt-mono">or the ✦ key on your phone</span>
								</div>
								<div class="mt-composer" class:mt-lit={litComposer}>
									<span class="mt-prompt" aria-hidden="true">›</span>
									<span class="mt-mono">{reduced ? DELEGATE_ASK : delegateTyped}<span
											class="mt-caret"
											aria-hidden="true"
										></span></span>
								</div>
								<div class="mt-taskcard" class:mt-lit={litPlan}>
									<div class="mt-taskcard-head">
										Kyoto booking <span class="mt-badge">planning</span>
									</div>
									<ol class="mt-plan">
										{#each PLAN as step, i}
											<li class:mt-plan-mem={(litPlan || reduced) && i === 1}>{step}</li>
										{/each}
									</ol>
									<p class="mt-caption">Yours to edit before a single click.</p>
								</div>
							</div>
						{:else if station.id === 'hands'}
							<div class="mt-hands">
								<div class="mt-browser">
									<div class="mt-browser-bar">
										<span></span><span></span><span></span>
										<span class="mt-browser-url mt-mono">fare.book / checkout</span>
									</div>
									<div class="mt-browser-body">
										<div class="mt-field" class:filled={sceneLocals[SI.hands] > 0.14}>
											<span class="mt-field-label">From</span><span class="mt-field-value">BLR</span>
										</div>
										<div class="mt-field" class:filled={sceneLocals[SI.hands] > 0.26}>
											<span class="mt-field-label">To</span><span class="mt-field-value">KIX</span>
										</div>
										<div class="mt-field" class:filled={sceneLocals[SI.hands] > 0.38}>
											<span class="mt-field-label">Dates</span><span class="mt-field-value"
												>12–16 Nov</span
											>
										</div>
									</div>
								</div>
								<div class="mt-mono mt-toolline">browser › fare.compare(BLR → KIX, 12–16 Nov)</div>
								<div class="mt-gate" class:approved={sceneLocals[SI.hands] > 0.66} class:mt-lit={litGate}>
									<div class="mt-gate-ask">
										<strong>₹1,840</strong> of your <strong>₹2,500</strong> cap — place the order?
									</div>
									<div class="mt-gate-actions">
										<span class="mt-gate-approve">Approve</span>
										<span class="mt-gate-stamp" aria-hidden="true">✓ placed</span>
									</div>
								</div>
								<p class="mt-caption">Your cap, your call.</p>
							</div>
						{:else if station.id === 'meeting'}
							<div class="mt-meeting">
								<div class="mt-tiles">
									{#each ['You', 'Design', 'Infra', 'Magican'] as who, i}
										<div
											class="mt-tile"
											class:mt-tile-orb={i === 3}
											class:mt-lit={i === 3 && litTile}
											style="--i:{i}"
										>
											<span class="mt-avatar" aria-hidden="true"
												>{i === 3 ? '' : who.slice(0, 1)}</span
											>
											<span class="mt-tile-name">{who}</span>
											{#if i === 3}<span class="mt-tile-dot" aria-hidden="true"></span>{/if}
										</div>
									{/each}
								</div>
								<div class="mt-transcript">
									<p class="mt-tline" style="--i:0">“…so we ship the migration Friday.”</p>
								</div>
								<div class="mt-decision">
									<span class="mt-badge">decision</span> Migration ships Friday
								</div>
								<div class="mt-owe">
									<span class="mt-badge mt-badge-warm">you owe</span> the API draft — filed to your list
								</div>
							</div>
						{:else if station.id === 'lunch'}
							<!-- 13:00 — lunch from Swiggy, routed through your OWN memory:
							     the healthy streak is recalled, the choice is RECONFIRMED,
							     and only then does the order exist. -->
							<div class="mt-lunch">
								<div class="mt-composer">
									<span class="mt-prompt" aria-hidden="true">›</span>
									<span class="mt-mono">“order lunch from swiggy”</span>
								</div>
								<div class="mt-memcard" class:mt-lit={litHealthy}>
									<span class="mt-memcard-kicker">remembered · your goal</span>
									Eating healthy — 12 days in.
								</div>
								<div class="mt-gate mt-reconfirm" class:approved={reduced || sw > 0.6} class:mt-lit={litConfirm}>
									<div class="mt-gate-ask">
										Still going healthy? Your usual place has the <strong>grilled paneer bowl</strong>.
									</div>
									<div class="mt-gate-actions">
										<span class="mt-gate-approve">keep it healthy</span>
										<span class="mt-choice-alt">not today</span>
										<span class="mt-gate-stamp" aria-hidden="true">✓ kept</span>
									</div>
								</div>
								<div class="mt-decision">
									<span class="mt-badge">ordered</span> Swiggy — grilled paneer bowl · 25 min
								</div>
							</div>
							<p class="mt-caption">It asked before it ordered.</p>
						{:else if station.id === 'thinks'}
							<div class="mt-research">
								<!-- @brainstorm — ONE story: a messy question branches; the
								     MOUSE picks a branch; the AI works, branches it further,
								     and shows its homework (cohort-pricing sources, an
								     income-scenarios chart, a cited note); memory recollects
								     down the line; the walk lands on a conclusion. -->
								<div class="mt-graphwrap">
									<div class="mt-bsummon" class:on={reduced || sceneLocals[SI.thinks] > 0.05} aria-hidden="true">
										<span class="mt-prompt">›</span>
										<span class="mt-mono">{brainstormTyped}</span>
										<span class="mt-caret"></span>
									</div>
									<div class="mt-reason">
										<div class="mt-graph" aria-hidden="true">
											<svg class="mt-graph-edges" viewBox="0 0 340 170">
												<path d="M62 85 C 110 50, 135 38, 180 32" pathLength="100" class="mt-edge" style="--et:0.26" />
												<path d="M62 85 C 115 82, 140 84, 186 85" pathLength="100" class="mt-edge" style="--et:0.3" />
												<path d="M62 85 C 110 120, 135 132, 180 138" pathLength="100" class="mt-edge" style="--et:0.34" />
												<path d="M186 85 C 225 60, 242 52, 278 48" pathLength="100" class="mt-edge" style="--et:0.52" />
												<path d="M186 85 C 228 88, 246 92, 282 95" pathLength="100" class="mt-edge" style="--et:0.55" />
												<path d="M186 85 C 225 115, 242 128, 274 140" pathLength="100" class="mt-edge" style="--et:0.58" />
											</svg>
											<span class="mt-gnode mt-gnode-root" class:pop={reduced || sceneLocals[SI.thinks] > 0.22} style="--gx:62px;--gy:85px">second income?</span>
											<span class="mt-gnode" class:pop={reduced || sceneLocals[SI.thinks] > 0.3} style="--gx:180px;--gy:32px">freelance</span>
											<span
												class="mt-gnode"
												class:pop={reduced || sceneLocals[SI.thinks] > 0.34}
												class:picked={reduced || sceneLocals[SI.thinks] > 0.45}
												style="--gx:186px;--gy:85px">teach online</span
											>
											<span class="mt-gnode" class:pop={reduced || sceneLocals[SI.thinks] > 0.38} style="--gx:180px;--gy:138px">rent the studio</span>
											<span class="mt-gnode mt-gnode-l2" class:pop={reduced || sceneLocals[SI.thinks] > 0.56} style="--gx:278px;--gy:48px">weekend cohort</span>
											<span class="mt-gnode mt-gnode-l2" class:pop={reduced || sceneLocals[SI.thinks] > 0.59} style="--gx:282px;--gy:95px">recorded course</span>
											<span class="mt-gnode mt-gnode-l2" class:pop={reduced || sceneLocals[SI.thinks] > 0.62} style="--gx:274px;--gy:140px">1:1 tutoring</span>
											<span
												class="mt-gwork mt-mono"
												class:on={!reduced && sceneLocals[SI.thinks] > 0.46 && sceneLocals[SI.thinks] < 0.56}
												>expanding…</span
											>
											{#if !reduced}
												<span class="mt-gcursor" aria-hidden="true"></span>
											{/if}
										</div>
										<!-- down the line, memory recollects what it knows of you -->
										<div class="mt-recalls" aria-hidden="true">
											<div class="mt-recall" class:on={reduced || sceneLocals[SI.thinks] > 0.56}>
												<span class="mt-recall-k mt-mono">remembered</span>
												you taught design at NID — 2019
											</div>
											<div class="mt-recall" class:on={reduced || sceneLocals[SI.thinks] > 0.64}>
												<span class="mt-recall-k mt-mono">remembered</span>
												weekends free after 11:00 — calendar
											</div>
											<div class="mt-recall" class:on={reduced || sceneLocals[SI.thinks] > 0.7}>
												<span class="mt-recall-k mt-mono">remembered</span>
												₹40k set aside for gear — your ledger
											</div>
										</div>
									</div>
									<!-- the expansion's homework — the evidence the conclusion
									     stands on, in the SAME story: what teaching pays -->
									<div class="mt-sources">
										{#each [['0.92', 'cohort pricing'], ['0.88', 'platform fees'], ['0.71', 'tutor rates']] as [score, src], i}
											<div class="mt-source" style="--i:{i}">
												<span class="mt-mono">{score}</span> {src}
											</div>
										{/each}
									</div>
									<div class="mt-gaui">
										<div class="mt-metric"><span class="mt-mono">₹8k</span><small>a seat · going rate</small></div>
										<div class="mt-chart" aria-hidden="true">
											<!-- three income scenarios — course, tutoring, cohort -->
											{#each [0.45, 0.62, 0.95] as h, i}
												<span style="--h:{h};--i:{i}"></span>
											{/each}
										</div>
										<div class="mt-brief" class:mt-lit={litBrief}>
											<span class="mt-badge">cited</span> Weekend cohorts out-earn courses.
										</div>
									</div>
									<div class="mt-conclusion" class:on={reduced || sceneLocals[SI.thinks] > 0.74}>
										<span class="mt-badge">conclusion</span> Pilot a weekend cohort in March.
									</div>
								</div>
								<p class="mt-caption">The question wandered. The answer didn’t.</p>
							</div>
						{:else if station.id === 'orb'}
							<div class="mt-orbstage" style={voiceOrbStageStyle}>
								<!-- Past the reveal the orb DEPARTS with the camera — it
								     shrinks toward the pocket station and lands as the next
								     screen's notification glyph. -->
								<div class="mt-orb-carrier" aria-hidden="true">
									{#if reduced}
										<div class="mt-orb-static" aria-hidden="true"></div>
									{:else if orbNear}
										<!-- The blob walks the FULL five-phase sequence, driven by
										     the same 0..0.76 mapping as the word: ember armed, violet
										     wake, calm listening, amber thinking, teal speaking —
										     each phase's true palette, in step with its word. -->
										<AuroraOrbScene local={orbWalk} active={scene === SI.orb} />
									{/if}
								</div>
								<p class="mt-orb-word mt-mono" aria-hidden="true">
									{#if reduced}Listening{:else}{#key orbWord}<span class="mt-orb-word-in">{orbWord}</span>{/key}{/if}
								</p>
								<p class="mt-orb-reveal">
									The thread you’ve followed since dawn — <strong>that’s Magican.</strong>
								</p>
							</div>
						{:else if station.id === 'pocket'}
							<!-- The lock-screen wake: the iOS app answers while the phone
							     stays LOCKED — island truth from ambient-mode.md (compact
							     orb dot → expanded orb + word + leash ring). -->
							<div class="mt-pocketstage">
								<p class="mt-heypresto mt-mono" class:onair={reduced || (pk > 0.06 && pk < 0.3)}>
									“hey presto”
								</p>
								<div class="mt-phone" class:awake={reduced || pk > 0.16}>
									<div class="mt-phone-screen">
									<div class="mt-pisland" class:mt-lit={litIsland} style={pocketVoiceOrbStyle}>
											<span class="mt-island-orb"></span>
											{#if reduced || pk > 0.16}
												<span class="mt-island-word">{reduced ? 'Listening' : pocketWord}</span>
												<span class="mt-island-ring"></span>
											{/if}
										</div>
										<div class="mt-lock-clock">21:07</div>
										<div class="mt-lock-date">Tuesday 4 November</div>
										<svg class="mt-lock-padlock" viewBox="0 0 16 16" aria-hidden="true">
											<rect x="3" y="7" width="10" height="7" rx="1.6" />
											<path d="M5 7 V5 a3 3 0 0 1 6 0 V7" fill="none" />
										</svg>
										<!-- ONE iOS notification card, mutating in place: the small
										     aurora orb is the app glyph, "Magican" the app name, and the
										     line walks Listening → your transcript → the reply. The
										     card never multiplies; only its line changes — while the
										     padlock above stays shut. -->
										<div class="mt-lock-notif" class:on={reduced || pk > 0.26}>
											<span
												class="mt-notif-orb"
												class:mt-landing={orbLanding}
												class:mt-lit={reduced || scene >= SI.pocket}
												style={VOICE_ORB_LANDING_STYLE}
												aria-hidden="true"
											></span>
											<div class="mt-notif-body">
												<p class="mt-notif-app">Magican <span class="mt-notif-when">now</span></p>
												<div class="mt-notif-swap">
													<p class="mt-notif-line mt-mono" class:on={pocketBeat === 1}>Listening</p>
													<p class="mt-notif-line" class:on={pocketBeat === 2}>
														<strong>You</strong> — “Chase the deposit refund”
													</p>
													<p class="mt-notif-line" class:on={pocketBeat === 3}>
														<strong>Magican</strong> — On it. Request drafted and sent.
													</p>
												</div>
											</div>
										</div>
									</div>
								</div>
							</div>
							<p class="mt-caption">The phone never unlocked.</p>
						{:else if station.id === 'tutor'}
							<!-- The tutor's real story: a Pythagoras webpage on YOUR
							     screen; Magican summoned right on the page (@tutor); the
							     tutor draws the proof OVER the page's own triangle,
							     Socratically — and the same lesson stands on the phone
							     beside it. One tutor, web and mobile. -->
							<div class="mt-tutorstage">
								<div class="mt-tbrowser">
									<div class="mt-browser-bar">
										<span></span><span></span><span></span>
										<span class="mt-browser-url mt-mono">mathsphere.io / pythagoras</span>
									</div>
									<div class="mt-tpage">
										<h4 class="mt-tpage-h">The Pythagorean Theorem</h4>
										<div class="mt-tpage-lines" aria-hidden="true"></div>
										<svg viewBox="66 8 232 234" aria-hidden="true" class="mt-chalk-pyth">
											<!-- the page's OWN figure: a plain right triangle -->
											<path d="M150 148 L150 88 L230 148 Z" class="mt-tpage-tri" />
											<path d="M150 144 h6 v4" class="mt-tpage-tri" style="stroke-width:1.2" />
											<!-- what the tutor draws on top of the page -->
											<path d="M150 88 L90 88 L90 148 L150 148" pathLength="100" class="mt-stroke mt-py-a" />
											<path d="M150 148 L150 228 L230 228 L230 148" pathLength="100" class="mt-stroke mt-py-b" />
											<path d="M150 88 L198 24 L278 84 L230 148" pathLength="100" class="mt-stroke mt-py-c" />
											<text x="112" y="122" class="mt-py-label mt-py-la">a²</text>
											<text x="182" y="192" class="mt-py-label mt-py-lb">b²</text>
											<text x="208" y="90" class="mt-py-label mt-py-lc">c²</text>
										</svg>
										<!-- Magican, summoned ON the page you were already reading -->
										<div class="mt-tsummon" aria-hidden="true">
											<span class="mt-prompt">›</span>
											<span class="mt-mono">@tutor explain this figure</span>
											<span class="mt-caret"></span>
										</div>
										<div class="mt-tanswer" aria-hidden="true">
											<p class="mt-tutorline mt-mono">{PYTH_CAPTION}</p>
											<p class="mt-py-eq mt-mono">
												<span class="mt-py-ea">a²</span><span class="mt-py-plus"> + </span><span
													class="mt-py-eb">b²</span
												><span class="mt-py-equals"> = </span><span class="mt-py-ec">c²</span>
											</p>
										</div>
									</div>
								</div>
								<!-- the same tutor, the same stroke, in your pocket -->
								<div class="mt-tphone" aria-hidden="true">
									<div class="mt-mini-screen">
										<span class="mt-mini-island"></span>
										<svg viewBox="66 8 232 234" class="mt-tphone-fig">
											<path d="M150 148 L150 88 L230 148 Z" class="mt-tpage-tri" />
											<path d="M150 88 L90 88 L90 148 L150 148" pathLength="100" class="mt-stroke mt-py-a" />
											<path d="M150 148 L150 228 L230 228 L230 148" pathLength="100" class="mt-stroke mt-py-b" />
											<path d="M150 88 L198 24 L278 84 L230 148" pathLength="100" class="mt-stroke mt-py-c" />
											<text x="112" y="122" class="mt-py-label mt-py-la">a²</text>
											<text x="182" y="192" class="mt-py-label mt-py-lb">b²</text>
											<text x="208" y="90" class="mt-py-label mt-py-lc">c²</text>
										</svg>
										<span class="mt-tphone-eq mt-mono">a² + b² = c²</span>
									</div>
									<span class="mt-mini-tag mt-mono">and on your phone</span>
								</div>
							</div>
							<p class="mt-caption">One tutor — on the web and in your pocket.</p>
						{:else if station.id === 'zepto'}
							<!-- 22:00 — quick commerce through an app Magican has never used:
							     the FIRST-TIME MCP authorization happens on camera, then
							     the Zepto order lands. -->
							<div class="mt-zepto">
								<div class="mt-composer">
									<span class="mt-prompt" aria-hidden="true">›</span>
									<span class="mt-mono">“egg fried rice and chilli paneer”</span>
								</div>
								<div class="mt-gate mt-mcpcard" class:approved={reduced || zp > 0.5} class:mt-lit={litMcp}>
									<div class="mt-gate-ask">
										<span class="mt-mcp-row"
											><span class="mt-badge">MCP</span> <strong>Connect Zepto</strong>
											<span class="mt-mcp-first mt-mono">first time</span></span
										>
										Magican wants to order on Zepto for you — authorize once?
									</div>
									<div class="mt-gate-actions">
										<span class="mt-gate-approve">Authorize</span>
										<span class="mt-gate-stamp" aria-hidden="true">✓ connected</span>
									</div>
								</div>
								<!-- Zepto is on-demand GROCERY, not a restaurant: the ask is
								     a dish, so what comes back is the INGREDIENT LIST for
								     it — and the interesting part is that the basket is not
								     a foregone conclusion. Two lines arrive already
								     unticked and greyed, because Magican checked the pantry
								     first: the rice was bought two days ago and the chilli
								     sauce is still in the door. You are being shown a
								     basket it has already argued you out of half of. -->
								<div class="mt-zbasket" class:mt-lit={litZorder}>
									<div class="mt-zbasket-head">
										<span class="mt-badge">basket</span> Zepto — for egg fried rice + chilli paneer
									</div>
									{#each [['Basmati rice, 1 kg', '₹142', 0, 'bought 2 days ago — reorder?'], ['Eggs, 6', '₹64', 1, ''], ['Spring onion, 100 g', '₹28', 1, ''], ['Paneer, 200 g', '₹92', 1, ''], ['Chilli sauce, 200 ml', '₹78', 0, 'still in your pantry'], ['Soy sauce, 200 ml', '₹96', 1, '']] as [item, price, on, note], zi (item)}
										{@const shown = reduced || zp > 0.6 + zi * 0.028}
										<div class="mt-zline" class:off={!on} class:in={shown}>
											<span class="mt-zcheck" class:on={on && shown}></span>
											<span class="mt-zitem">{item}</span>
											{#if note}<span class="mt-znote mt-mono">{note}</span>{/if}
											<span class="mt-zprice mt-mono">{price}</span>
										</div>
									{/each}
									<div class="mt-zfoot mt-mono">
										<span>4 of 6 · ₹280</span>
										<span class="mt-zeta">arriving in 9 min</span>
									</div>
								</div>
							</div>
							<p class="mt-caption">One login. Now it’s a skill.</p>
						{:else if station.id === 'night'}
							<!-- The night, receipted: real overnight work — a monitor
							     catching the fare it was watching, the inbox triaged, a
							     scheduled routine, and the morning brief that station
							     7:30 will hand you. Concrete lines, not panel names. -->
							<div class="mt-warroom" class:needs-you={!reduced && sceneLocals[SI.night] > 0.16 && sceneLocals[SI.night] < 0.4}>
								{#each [['23:41', 'monitor', 'Kyoto fares dropped again — rebook drafted'], ['00:58', 'inbox', '14 mails triaged; 2 held for morning'], ['02:30', 'routine', 'weekly expense report filed'], ['05:50', 'brief', 'your morning brief assembled']] as [tt, kind, line], i (kind)}
									<div class="mt-nightrow" style="--i:{i}">
										<span class="mt-night-t mt-mono">{tt}</span>
										<span class="mt-night-k mt-mono">{kind}</span>
										<span class="mt-night-line">{line}</span>
									</div>
								{/each}
								<div class="mt-needcard">
									<span class="mt-badge mt-badge-amber">needs you</span> One question — answered, back to
									work.
								</div>
							</div>
							<div class="mt-receipt-card" class:mt-lit={litReceipt}>
								<div class="mt-receipt-head">Run receipt <span class="mt-mono">#0231</span></div>
								{#each ['Compared 14 fares', 'Booked the aisle seat', 'Filed the calendar hold', 'Chased the deposit refund'] as step, i}
									<div class="mt-receipt-row" style="--i:{i}"><span class="mt-check">✓</span>{step}</div>
								{/each}
								<div class="mt-receipt-meta mt-mono">₹212.40 · 2 models · 3 artifacts</div>
								<div class="mt-receipt-mem">memory: “prefers aisle, hates layovers over 2h” — filed</div>
							</div>
							<p class="mt-caption">Every action visible. Every rupee accounted for.</p>
						{/if}
					</section>

				{/each}
			</div>
		</div>

		{#if !reduced}
			<canvas class="mt-thread" bind:this={canvasEl} aria-hidden="true"></canvas>
			<div class="mt-frag mt-mono" bind:this={fragEl} aria-hidden="true">
				aisle seat — 3 weeks ago
			</div>
			<nav class="mt-rail" aria-label="Chapters">
				<svg
					class="mt-map"
					viewBox="-4 -4 {MAP_W + 8} {MAP_H + 8}"
					width={MAP_W + 8}
					height={MAP_H + 8}
					aria-hidden="true"
				>
					<path class="mt-map-base" d={MAP_PATH} pathLength="1" />
					<path class="mt-map-done" d={MAP_PATH} pathLength="1" style="stroke-dashoffset:{1 - p}" />
				</svg>
				{#each STATIONS as station, n (station.id)}
					<button
						class="mt-dot"
						class:active={scene === n}
						style="left:{MAP_POINTS[n].x + 4}px;top:{MAP_POINTS[n].y + 4}px"
						aria-label={station.clock ? `${station.clock} ${station.title}` : station.title}
						aria-current={scene === n ? 'true' : undefined}
						on:click={() => jumpTo(n)}
					></button>
				{/each}
				<span class="mt-maphead" bind:this={mapHeadEl} aria-hidden="true"></span>
			</nav>
			<a class="mt-skip" href="#landing-cta">skip to the ask ↓</a>
		{/if}
	</div>
</div>

<style>
	.mt-track {
		position: relative;
		/* The movie runs on the app's own ground: the theme's --landing-*
		   tokens, so the film and the sections after it are ONE continuous
		   paper world — no lights-up seam. The protagonist thread reads as
		   saturated ink-and-light on that paper; dark survives only as
		   scene-truth INSIDE genuinely dark product screens (the warroom
		   night deck, the tutor board, the Dynamic Island pill). Deep app
		   themes keep a deep film through the same tokens. */
		--film-ink: var(--text-primary, #2d3436);
		--film-dim: color-mix(in srgb, var(--film-ink) 72%, transparent);
		--th: var(--accent-primary, #9e59ff);
		--th2: var(--accent-secondary, var(--th));
		--th-bright: color-mix(in srgb, var(--th) 66%, white);
		--th2-bright: color-mix(in srgb, var(--th2) 66%, white);
		--th-ink: color-mix(in srgb, var(--th) 55%, var(--film-ink));
		--th-soft: color-mix(in srgb, var(--th-bright) 54%, transparent);
		--th-glow: color-mix(in srgb, var(--th-bright) 88%, transparent);
		--th-glow-wide: color-mix(in srgb, var(--th2-bright) 66%, transparent);
		/* Neutral station chrome belongs to the app theme too. Product-truth
		   scenes (the historical desktop, warroom and lock screen) retain their
		   own contained palettes, while ordinary cards inherit the selected
		   theme's elevation, borders, radius character and shadow language. */
		--film-card: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 82%, transparent);
		--film-card-border: var(--border-default, color-mix(in srgb, var(--film-ink) 14%, transparent));
		--film-card-shadow: var(--shadow-lg, var(--landing-task-card-shadow));
		--film-line: var(--border-soft, color-mix(in srgb, var(--film-ink) 13%, transparent));
		background: var(--landing-bg);
	}
	.mt-track.mt-onlight {
		--film-card: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 88%, transparent);
	}
	.mt-track:not(.static)::after {
		/* The last inches of film dissolve into the ground the next section
		   already stands on — the handoff has no seam to see. */
		content: '';
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		height: 14svh;
		z-index: 5;
		pointer-events: none;
		background: linear-gradient(180deg, transparent, var(--landing-bg));
	}
	.mt-track.static {
		height: auto;
	}

	/* Act I's loss: the paper's light, stolen and returned. Driven per-frame
	   by the rAF; sits above the ground and the mote field's z, below the
	   camera's world so the era cards stand as objects in the dark. */
	.mt-eradark {
		position: absolute;
		inset: 0;
		z-index: 1;
		pointer-events: none;
		opacity: 0;
		/* cold platform NIGHT, not neutral black: the crossfade over paper
		   must tint blue on its way down, never dead grey */
		background:
			radial-gradient(85% 65% at 50% 42%, rgba(30, 48, 96, 0.6), transparent 74%),
			linear-gradient(180deg, #060a16, #04060e);
		will-change: opacity;
	}

	.mt-stage {
		position: sticky;
		top: 0;
		height: 100svh;
		overflow: hidden;
		background-color: var(--landing-bg);
		background-image:
			var(--landing-blob-1, none),
			var(--landing-blob-2, none),
			var(--landing-blob-3, none);
		background-position:
			8% 16%,
			92% 62%,
			50% 104%;
		background-size:
			min(58rem, 74vw) min(58rem, 74vw),
			min(52rem, 68vw) min(52rem, 68vw),
			min(44rem, 60vw) min(44rem, 60vw);
		background-repeat: no-repeat;
		background-blend-mode: var(--landing-blob-blend, normal);
	}
	.mt-stage.static {
		position: static;
		height: auto;
		overflow: visible;
	}
	.mt-stage:not(.static)::before {
		/* soft paper shading at the edges — no flat solid voids ever */
		content: '';
		position: absolute;
		inset: 0;
		z-index: 1;
		pointer-events: none;
		background: radial-gradient(
			120% 92% at 50% 44%,
			transparent 58%,
			color-mix(in srgb, var(--film-ink) 14%, transparent) 100%
		);
	}
	.mt-stage:not(.static)::after {
		/* one shared SVG-noise grain over the whole stage */
		content: '';
		position: absolute;
		inset: 0;
		z-index: 4;
		pointer-events: none;
		opacity: 0.05;
		background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='160' height='160'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='2' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='160' height='160' filter='url(%23n)'/%3E%3C/svg%3E");
		background-size: 160px 160px;
	}

	/* ── the one continuous world ─────────────────────────────── */
	.mt-camera {
		position: absolute;
		left: 50%;
		top: 50%;
		z-index: 2;
		width: 0;
		height: 0;
		will-change: transform;
	}
	.mt-world {
		position: absolute;
		left: 0;
		top: 0;
		width: 0;
		height: 0;
		will-change: transform;
	}
	.mt-camera.static,
	.mt-world.static {
		position: static;
		width: auto;
		height: auto;
		will-change: auto;
		/* !important: the rAF writes inline transforms; when the reduced-
		   motion preference flips mid-session those leftovers must lose. */
		transform: none !important;
	}
	.mt-glow {
		position: absolute;
		width: 130vw;
		height: 125vh;
		transform: translate(-50%, -50%);
		pointer-events: none;
		background:
			radial-gradient(42% 36% at 42% 44%, var(--g1), transparent 72%),
			radial-gradient(36% 30% at 62% 58%, var(--g2), transparent 70%),
			radial-gradient(46% 40% at 50% 34%, var(--g3), transparent 74%);
	}

	/* Off-stage stations must not tick their infinite CSS loops for the
	   page's lifetime — paused unless the station owns the playhead. The
	   static document keeps no loops at all via the media query below. */
	.mt-track:not(.static) .mt-scene:not(.on) :global(*),
	.mt-track:not(.static) .mt-scene:not(.on) {
		animation-play-state: paused;
	}

	.mt-scene {
		position: absolute;
		width: 100vw;
		height: 100svh;
		transform: translate(-50%, -50%);
		font-family: var(--lp-font);
		color: var(--text-primary);
		/* Accent as INK: aurora hues pulled toward the theme's text ink so
		   accent COPY holds contrast on paper (and lifts on deep themes);
		   raw --a stays for dots, bars and glows. */
		--a-ink: color-mix(in srgb, var(--a) 60%, var(--film-ink));
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: clamp(0.9rem, 2.2vh, 1.6rem);
		padding: clamp(1rem, 4vw, 3rem);
		pointer-events: none;
	}
	.mt-scene.on {
		pointer-events: auto;
	}
	.mt-scene.mt-depart {
		/* the orb's flight: scenes paint in DOM order, so the pocket phone
		   (a later sibling) would cover the carrier — raised while flying,
		   the orb lands ON the notification glyph, in front of the phone */
		z-index: 2;
	}
	.mt-scene.static {
		position: static;
		width: auto;
		height: auto;
		min-height: 0;
		/* !important: the rAF's depth-of-field writes inline transform,
		   opacity and blur; a mid-session reduced-motion flip must win. */
		transform: none !important;
		opacity: 1 !important;
		filter: none !important;
		padding-block: clamp(2rem, 6vh, 4rem);
	}

	.mt-chapter-head {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: clamp(0.7rem, 1.6vh, 1.1rem);
		text-align: center;
		max-width: min(94vw, 62rem);
	}
	.mt-kicker {
		display: inline-flex;
		align-items: center;
		gap: 0.9em;
		font-family: var(--lp-mono);
		font-size: clamp(0.66rem, 0.9vw, 0.76rem);
		font-weight: 500;
		letter-spacing: 0.34em;
		text-transform: uppercase;
		color: var(--a-ink);
	}
	.mt-clock {
		color: var(--film-ink);
		opacity: 0.85;
	}
	.mt-kicker-rule {
		width: 2.4em;
		height: 1px;
		background: linear-gradient(90deg, transparent, var(--a));
		opacity: 0.7;
	}
	.mt-title {
		font-family: var(--lp-font);
		font-weight: 620;
		font-size: clamp(1.9rem, 4.4vw, 3.4rem);
		letter-spacing: -0.03em;
		line-height: 1.02;
		margin: 0;
		color: var(--film-ink);
		text-wrap: balance;
	}
	.mt-w {
		display: inline-block;
		opacity: clamp(0, calc((var(--local) - var(--wi) * 0.016) * 10), 1);
		transform: translateY(
			calc((1 - clamp(0, calc((var(--local) - var(--wi) * 0.016) * 10), 1)) * 0.35em)
		);
	}
	.mt-scene.static .mt-w {
		opacity: 1;
		transform: none;
	}
	.mt-caption {
		font-family: var(--lp-font);
		color: var(--film-dim);
		font-size: clamp(0.9rem, 1.6vw, 1.02rem);
		max-width: 34rem;
		text-align: center;
		margin: 0;
		/* Captions are each station's closing line — they arrive after the
		   station has built, comfortably before the camera departs. */
		opacity: clamp(0, calc((var(--local) - 0.5) * 6), 1);
		transform: translateY(calc((1 - clamp(0, calc((var(--local) - 0.5) * 6), 1)) * 8px));
	}
	.mt-scene.static .mt-caption {
		opacity: 1;
		transform: none;
	}
	.mt-mono {
		font-family: var(--lp-mono);
		font-size: 0.86em;
	}
	.mt-badge {
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		text-transform: uppercase;
		letter-spacing: 0.12em;
		padding: 0.15rem 0.5rem;
		border-radius: 999px;
		background: var(--ah);
		color: var(--a-ink);
	}
	/* warm rides light-world cards (ink-mixed); amber lives inside the
	   warroom's scene-truth dark deck and keeps its phosphor. */
	.mt-badge-warm {
		background: rgba(255, 180, 84, 0.22);
		color: color-mix(in srgb, #ffb454 55%, var(--film-ink));
	}
	.mt-badge-amber {
		background: rgba(255, 180, 84, 0.18);
		color: #ffb454;
	}

	/* ── lit-by-touch: the thread's hue stays on what it touched ── */
	.mt-lit {
		border-color: var(--th-soft) !important;
		box-shadow:
			inset 0 1px 0 rgba(255, 255, 255, 0.08),
			0 0 0 1px color-mix(in srgb, var(--th) 18%, transparent),
			0 12px 44px -10px var(--th-glow);
		transition:
			border-color 0.6s ease,
			box-shadow 0.6s ease;
	}

	/* Wow #1 — the period smolders into an ember on the exodus's closing
	   line, then PAYS OFF: at departure (--rel) the burn leaves the glyph,
	   the canvas ember detaches, flies the leg, and strikes the ignition
	   the aurora thread is born from; the glyph settles back to an ordinary
	   full stop. Scrub-driven via --local; static mode holds a plain
	   period. The windows sit late in the station because the caption it
	   ends only arrives at local 0.5. */
	.mt-scrollcue {
		position: absolute;
		bottom: 7%;
		left: 50%;
		transform: translateX(-50%);
		font-size: 0.68rem;
		letter-spacing: 0.3em;
		color: var(--film-dim);
		/* whispers, then leaves as the film starts moving */
		opacity: calc(0.8 * clamp(0, calc((0.18 - var(--local)) * 10), 1));
		animation: mt-breathe 3s ease-in-out infinite;
	}

	/* ── shared panel chrome: paper glass, 1px border, and a shadow
	   that bleeds each station's aurora hue onto the ground ─────── */
	.mt-panel,
	.mt-taskcard,
	.mt-browser,
	.mt-memcard,
	.mt-brief,
	.mt-gaui,
	.mt-decision,
	.mt-owe,
	.mt-gate,
	.mt-receipt-card {
		width: min(92vw, 34rem);
		border: 1px solid var(--film-card-border);
		background:
			linear-gradient(180deg, rgba(255, 255, 255, 0.04), transparent 40%),
			var(--film-card);
		box-shadow:
			inset 0 1px 0 rgba(255, 255, 255, 0.07),
			var(--film-card-shadow),
			0 24px 80px -28px var(--ah-strong);
		border-radius: 18px;
		padding: 0.9rem 1.1rem;
		font-family: var(--lp-font);
		color: var(--text-primary);
		backdrop-filter: blur(6px);
	}

	/* ── 1 · KNOWS ────────────────────────────────────────────── */
	.mt-panel-head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-weight: 600;
		margin-bottom: 0.6rem;
	}
	.mt-live-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--a);
		box-shadow: 0 0 0 0 var(--ah-strong);
		animation: mt-pulse 2.2s infinite;
	}
	.mt-pulse-chip {
		margin-left: auto;
		font-family: var(--lp-mono);
		font-size: 0.72rem;
		color: var(--a-ink);
		border: 1px solid var(--ah-strong);
		border-radius: 999px;
		padding: 0.1rem 0.55rem;
	}
	.mt-row {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.42rem 0;
		border-top: 1px solid var(--film-line);
		opacity: clamp(0, calc((var(--local) - var(--i) * 0.1) * 5), 1);
		transform: translateY(calc((1 - clamp(0, calc((var(--local) - var(--i) * 0.1) * 5), 1)) * 8px));
	}
	.mt-scene.static .mt-row {
		opacity: 1;
		transform: none;
	}
	.mt-row-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--a);
		flex: none;
	}
	.mt-row-text {
		font-size: 0.92rem;
	}
	.mt-row-tag {
		margin-left: auto;
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		color: var(--a-ink);
		white-space: nowrap;
	}
	.mt-memcard {
		border-left: 3px solid var(--th);
		opacity: clamp(0, calc((var(--local) - 0.36) * 5), 1);
	}
	.mt-memcard-kicker {
		display: block;
		font-family: var(--lp-mono);
		font-size: 0.64rem;
		text-transform: uppercase;
		letter-spacing: 0.22em;
		color: var(--th-ink);
		margin-bottom: 0.3rem;
	}
	.mt-scene.static .mt-memcard {
		opacity: 1;
	}

	/* ── 2 · ASK — the doc, the PHONE, the composer, the plan ──── */
	.mt-delegate {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		grid-template-areas:
			'doc phone'
			'composer phone'
			'task phone';
		gap: 0.9rem;
		width: min(92vw, 44rem);
		align-items: start;
	}
	.mt-doc {
		grid-area: doc;
	}
	.mt-composer {
		grid-area: composer;
	}
	.mt-taskcard {
		grid-area: task;
		/* the shared panel chrome pins a 34rem width; inside the delegate
		   grid the column is the honest width — never underlap the phone */
		width: auto;
	}
	/* The iPhone as a first-class delegation surface: the same doc under a
	   long-press selection, the ask raised right on the phone — one thread,
	   two devices. Frame language borrowed from the pocket station. */
	.mt-miniphone {
		grid-area: phone;
		position: relative;
		align-self: center;
		justify-self: end;
		display: grid;
		gap: 0.5rem;
		justify-items: center;
		opacity: clamp(0, calc((var(--local) - 0.04) * 8), 1);
	}
	.mt-mini-screen {
		position: relative;
		width: clamp(7.6rem, 12vw, 8.6rem);
		aspect-ratio: 9 / 18;
		border-radius: 22px;
		background: #101014;
		border: 1px solid #2a2a33;
		box-shadow:
			0 26px 60px -22px rgba(10, 6, 24, 0.55),
			0 16px 56px -26px var(--th-glow);
		overflow: hidden;
	}
	.mt-mini-screen::before {
		/* the lit display — the phone is AWAKE and in the doc, unlike the
		   pocket station's locked night screen */
		content: '';
		position: absolute;
		inset: 5px;
		border-radius: 17px;
		background:
			linear-gradient(180deg, var(--ah), transparent 34%),
			var(--bg-base, #f8f5ef);
	}
	.mt-mini-island {
		position: absolute;
		left: 50%;
		top: 11px;
		transform: translateX(-50%);
		width: 32px;
		height: 9px;
		border-radius: 999px;
		background: #000;
	}
	.mt-mini-field {
		/* the ask, typed in the Magican keyboard's host field */
		position: relative;
		margin: 30px 6px 0;
		padding: 0.32rem 0.42rem;
		border-radius: 6px;
		border: 1px solid var(--border-default);
		background: var(--bg-elevated);
		font-family: var(--lp-font);
		font-size: 0.55rem;
		line-height: 1.5;
		color: var(--text-primary);
		text-align: left;
	}
	/* the agentic row the ✦ key reveals: Ask / Rewrite / Add task */
	.mt-mini-actions {
		position: absolute;
		left: 6px;
		right: 6px;
		bottom: 33%;
		display: flex;
		gap: 3px;
		justify-content: center;
		--ar: clamp(0, calc((var(--local) - 0.2) * 9), 1);
		opacity: var(--ar);
		transform: translateY(calc((1 - var(--ar)) * 4px));
	}
	.mt-mini-act {
		font-family: var(--lp-mono);
		font-size: 0.44rem;
		letter-spacing: 0.02em;
		padding: 0.15rem 0.34rem;
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-secondary);
		white-space: nowrap;
	}
	.mt-mini-act-on {
		background: var(--th);
		color: var(--text-on-accent, #fff);
		box-shadow: 0 3px 10px -2px var(--th-glow);
	}
	/* the Magican keyboard: a real iOS deck — three letter rows, then the
	   bottom row where the ✦ brand key lives (glowing as the thread
	   arrives), the spacebar, and the return key */
	.mt-mini-kbd {
		position: absolute;
		left: 5px;
		right: 5px;
		bottom: 5px;
		height: 30%;
		border-radius: 7px 7px 15px 15px;
		background: #dcd9e4;
		padding: 4px 3px 3px;
		display: grid;
		gap: 3px;
	}
	.mt-kbd-row {
		display: flex;
		gap: 2.5px;
		justify-content: center;
		min-height: 0;
	}
	.mt-key {
		flex: 1 1 0;
		max-width: 10px;
		border-radius: 2.5px;
		background: #fff;
		box-shadow: 0 1px 0 rgba(60, 55, 75, 0.3);
	}
	.mt-key-star {
		flex: 0 0 auto;
		width: 15px;
		max-width: none;
		display: grid;
		place-items: center;
		font-size: 0.48rem;
		line-height: 1;
		color: var(--th-ink);
		--sp: clamp(0, calc((var(--local) - 0.17) * 10), 1);
		box-shadow:
			0 1px 0 rgba(60, 55, 75, 0.3),
			0 0 calc(var(--sp) * 9px) var(--th-glow);
	}
	.mt-key-space {
		flex: 1 1 auto;
		max-width: none;
	}
	.mt-key-go {
		flex: 0 0 auto;
		width: 15px;
		max-width: none;
		background: var(--th);
	}
	.mt-mini-tag {
		font-size: 0.6rem;
		letter-spacing: 0.18em;
		text-transform: uppercase;
		color: var(--th-ink);
		opacity: clamp(0, calc((var(--local) - 0.2) * 8), 1);
	}
	.mt-scene.static .mt-miniphone,
	.mt-scene.static .mt-mini-actions,
	.mt-scene.static .mt-mini-tag {
		opacity: 1;
		transform: none;
	}
	.mt-doc {
		position: relative;
		border-radius: 14px;
		border: 1px solid var(--landing-form-border);
		background: var(--landing-input-surface);
		padding: 1rem 1.2rem;
		font-family: var(--lp-font);
		color: var(--text-primary);
		line-height: 1.55;
	}
	.mt-selection {
		/* The TEXT is part of the sentence and must never disappear — only
		   the highlight sweep fades in with the station's local. */
		background: color-mix(in srgb, var(--ah) calc(clamp(0, var(--local) * 8, 1) * 100%), transparent);
		color: inherit;
		border-radius: 4px;
		padding: 0 0.15em;
	}
	.mt-ctxmenu {
		/* the browser's right-click, verbatim: the Chrome contextual
		   helper's menu raised over the selection */
		position: absolute;
		right: -1rem;
		top: -0.5rem;
		z-index: 1;
		min-width: 7.6rem;
		border-radius: 10px;
		background: var(--landing-form-surface, rgba(255, 255, 255, 0.96));
		border: 1px solid var(--landing-form-border);
		box-shadow: 0 18px 44px -14px rgba(20, 14, 40, 0.4);
		padding: 0.28rem;
		display: grid;
		gap: 1px;
		font-family: var(--lp-font);
		font-size: 0.7rem;
		--mm: clamp(0, calc((var(--local) - 0.07) * 9), 1);
		opacity: var(--mm);
		transform: translateY(calc((1 - var(--mm)) * 4px)) scale(calc(0.94 + 0.06 * var(--mm)));
		transform-origin: top right;
	}
	.mt-ctx-item {
		padding: 0.3rem 0.55rem;
		border-radius: 7px;
		color: var(--film-dim);
		text-align: left;
	}
	.mt-ctx-ask {
		color: var(--film-ink);
		background: var(--ah);
		font-weight: 600;
	}
	.mt-ctx-ask i {
		font-style: normal;
		color: var(--th-ink);
		margin-right: 0.25rem;
	}
	.mt-composer {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		border-radius: 12px;
		border: 1px solid var(--landing-form-border);
		background: var(--landing-form-surface);
		padding: 0.65rem 0.9rem;
		min-height: 2.6rem;
		opacity: clamp(0, calc((var(--local) - 0.16) * 6), 1);
	}
	.mt-prompt {
		color: var(--a-ink);
		font-family: var(--lp-mono);
	}
	.mt-caret {
		display: inline-block;
		width: 1px;
		height: 1em;
		background: var(--a);
		margin-left: 1px;
		vertical-align: -0.15em;
		animation: mt-blink 1s steps(1) infinite;
	}
	.mt-taskcard {
		opacity: clamp(0, calc((var(--local) - 0.5) * 6), 1);
		transform: translateY(calc((1 - clamp(0, calc((var(--local) - 0.5) * 6), 1)) * 10px));
	}
	.mt-scene.static .mt-taskcard,
	.mt-scene.static .mt-composer,
	.mt-scene.static .mt-ctxmenu {
		opacity: 1;
		transform: none;
	}
	.mt-scene.static .mt-selection {
		background: var(--ah);
	}
	.mt-taskcard-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
		font-weight: 600;
		margin-bottom: 0.4rem;
	}
	.mt-plan {
		margin: 0;
		padding: 0;
		list-style: none;
		counter-reset: plan;
		display: grid;
		gap: 0.35rem;
		font-size: 0.92rem;
	}
	.mt-plan li {
		counter-increment: plan;
		display: flex;
		align-items: baseline;
		gap: 0.6rem;
	}
	.mt-plan li::before {
		content: counter(plan, decimal-leading-zero);
		font-family: var(--lp-mono);
		font-size: 0.68rem;
		color: var(--film-dim);
		letter-spacing: 0.06em;
	}
	.mt-plan-mem {
		color: var(--th-ink);
		font-weight: 600;
	}
	.mt-plan-mem::after {
		content: ' ← remembered';
		font-family: var(--lp-mono);
		font-size: 0.7rem;
		opacity: 0.8;
	}
	.mt-taskcard .mt-caption {
		opacity: clamp(0, calc((var(--local) - 0.58) * 6), 1);
		text-align: left;
		font-size: 0.82rem;
		margin-top: 0.5rem;
	}
	.mt-scene.static .mt-taskcard .mt-caption {
		opacity: 1;
	}

	/* ── 3 · HANDS ────────────────────────────────────────────── */
	.mt-hands {
		display: grid;
		gap: 0.8rem;
		justify-items: center;
		width: min(92vw, 34rem);
	}
	.mt-browser {
		padding: 0;
		overflow: hidden;
		width: 100%;
	}
	.mt-browser-bar {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 0.55rem 0.8rem;
		border-bottom: 1px solid var(--film-line);
	}
	.mt-browser-bar > span:not(.mt-browser-url) {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		background: var(--film-line);
	}
	.mt-browser-url {
		margin-left: 0.8rem;
		font-size: 0.68rem;
		color: var(--film-dim);
		border: 1px solid var(--film-line);
		border-radius: 6px;
		padding: 0.15rem 0.6rem;
		letter-spacing: 0.02em;
	}
	.mt-browser-body {
		position: relative;
		padding: 1rem;
		display: grid;
		gap: 0.6rem;
	}
	.mt-field {
		display: flex;
		justify-content: space-between;
		border: 1px solid var(--landing-form-border);
		border-radius: 8px;
		padding: 0.45rem 0.7rem;
		font-size: 0.88rem;
	}
	.mt-field-value {
		font-family: var(--lp-mono);
		opacity: 0;
		transition: opacity 700ms ease;
	}
	.mt-field.filled .mt-field-value,
	.mt-scene.static .mt-field-value {
		opacity: 1;
	}
	.mt-toolline {
		color: var(--a-ink);
		font-size: 0.8rem;
		opacity: clamp(0, calc((var(--local) - 0.4) * 6), 1);
	}
	.mt-gate {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.8rem;
		opacity: clamp(0, calc((var(--local) - 0.44) * 6), 1);
		transform: translateY(calc((1 - clamp(0, calc((var(--local) - 0.44) * 6), 1)) * 10px));
	}
	.mt-scene.static .mt-gate,
	.mt-scene.static .mt-toolline {
		opacity: 1;
		transform: none;
	}
	.mt-gate-ask {
		font-size: 0.92rem;
	}
	.mt-gate-approve {
		font-family: var(--lp-mono);
		font-size: 0.78rem;
		border: 1px solid var(--ah-strong);
		color: var(--a-ink);
		border-radius: 8px;
		padding: 0.3rem 0.7rem;
	}
	.mt-gate-stamp {
		display: none;
		font-family: var(--lp-mono);
		font-size: 0.78rem;
		color: var(--a-ink);
	}
	.mt-gate.approved .mt-gate-approve {
		display: none;
	}
	.mt-gate.approved .mt-gate-stamp,
	.mt-scene.static .mt-gate-stamp {
		display: inline;
	}

	/* ── 4 · MEETING ──────────────────────────────────────────── */
	.mt-meeting {
		display: grid;
		gap: 0.8rem;
		justify-items: center;
		width: min(92vw, 40rem);
	}
	.mt-tiles {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 0.5rem;
		width: 100%;
	}
	.mt-tile {
		position: relative;
		aspect-ratio: 16 / 11;
		display: grid;
		place-items: center;
		border: 1px solid var(--film-card-border);
		background:
			radial-gradient(80% 80% at 50% 30%, rgba(255, 255, 255, 0.05), transparent 70%),
			var(--film-card);
		border-radius: 12px;
		box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.06);
		font-size: 0.78rem;
		font-family: var(--lp-font);
		color: var(--text-primary);
		opacity: clamp(0, calc((var(--local) - var(--i) * 0.04) * 8), 1);
	}
	.mt-scene.static .mt-tile {
		opacity: 1;
	}
	.mt-avatar {
		width: 2.4rem;
		height: 2.4rem;
		border-radius: 50%;
		display: grid;
		place-items: center;
		font-weight: 600;
		font-size: 0.9rem;
		color: var(--a-ink);
		background: var(--ah);
		border: 1px solid var(--ah-strong);
	}
	.mt-tile-orb .mt-avatar {
		background: radial-gradient(circle at 35% 30%, var(--a), var(--a2));
		border-color: var(--ah-strong);
		box-shadow: 0 0 18px var(--ah-strong);
	}
	.mt-tile-name {
		position: absolute;
		left: 0.6rem;
		bottom: 0.45rem;
		font-family: var(--lp-mono);
		font-size: 0.62rem;
		letter-spacing: 0.08em;
		color: var(--film-dim);
	}
	.mt-tile-orb {
		border-color: var(--ah-strong);
	}
	.mt-tile-dot {
		position: absolute;
		right: 8px;
		top: 8px;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: radial-gradient(circle at 35% 35%, var(--a), var(--a2));
		animation: mt-pulse 2.4s infinite;
	}
	.mt-transcript {
		width: 100%;
		display: grid;
		gap: 0.3rem;
	}
	.mt-tline {
		font-size: 0.9rem;
		color: var(--film-dim);
		margin: 0;
		opacity: clamp(0, calc((var(--local) - 0.15 - var(--i) * 0.14) * 5), 1);
	}
	.mt-decision {
		opacity: clamp(0, calc((var(--local) - 0.46) * 6), 1);
	}
	.mt-owe {
		opacity: clamp(0, calc((var(--local) - 0.58) * 6), 1);
	}
	.mt-scene.static .mt-tline,
	.mt-scene.static .mt-decision,
	.mt-scene.static .mt-owe {
		opacity: 1;
	}

	/* ── 13:00 · LUNCH — Swiggy, memory-guarded ───────────────── */
	/* Reuses the house cards wholesale: composer ask (reveals at 0.16),
	   memcard recall (0.36), gate question (0.44) — only the choice-alt
	   chip and the later order reveal are new. */
	.mt-lunch,
	.mt-zepto {
		display: grid;
		gap: 0.8rem;
		justify-items: center;
	}
	.mt-lunch .mt-composer,
	.mt-zepto .mt-composer {
		/* the composer ships a delegate-grid area name; inside these grids
		   that name would fabricate implicit tracks — neutralize it */
		grid-area: auto;
	}
	.mt-gate-actions {
		/* multi-chip gates (lunch, zepto) must not wrap the stamp under
		   the chips — one steady row, right-aligned */
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex: none;
		white-space: nowrap;
	}
	.mt-choice-alt {
		font-family: var(--lp-mono);
		font-size: 0.78rem;
		border: 1px solid var(--film-card-border);
		color: var(--film-dim);
		border-radius: 8px;
		padding: 0.3rem 0.7rem;
	}
	.mt-gate.approved .mt-choice-alt {
		opacity: 0.35;
	}
	.mt-lunch .mt-decision {
		/* the order may only exist AFTER the reconfirm is answered */
		opacity: clamp(0, calc((var(--local) - 0.68) * 6), 1);
	}
	.mt-scene.static .mt-lunch .mt-decision {
		opacity: 1;
	}

	/* ── 22:00 · ZEPTO — quick commerce, first-time MCP login ──── */
	.mt-mcp-row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 0.35rem;
	}
	.mt-mcp-first {
		margin-left: auto;
		font-size: 0.6rem;
		text-transform: uppercase;
		letter-spacing: 0.18em;
		color: var(--th-ink);
	}
	/* THE BASKET. Quick commerce for a dish is a LIST OF INGREDIENTS, and
	   the point of the beat is that two of them arrive already struck off:
	   Magican checked what you have before it filled the cart. So the greyed,
	   unticked lines are the argument — a basket you did not have to edit —
	   and the footer counts 4 of 6, not 6 of 6. */
	.mt-zbasket {
		display: grid;
		gap: 0.1rem;
		width: min(92vw, 30rem);
		padding: 0.7rem 0.9rem 0.6rem;
		border: 1px solid var(--film-card-border);
		border-radius: 14px;
		background: var(--film-card-bg);
		box-shadow: var(--film-card-shadow);
		opacity: clamp(0, calc((var(--local) - 0.56) * 7), 1);
	}
	.mt-scene.static .mt-zbasket {
		opacity: 1;
	}
	.mt-zbasket-head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 0.45rem;
		font-size: 0.86rem;
		color: var(--film-ink);
	}
	.mt-zline {
		display: grid;
		grid-template-columns: 15px 1fr auto;
		align-items: center;
		gap: 0.5rem;
		padding: 0.24rem 0;
		border-bottom: 1px solid color-mix(in srgb, var(--film-card-border) 55%, transparent);
		font-size: 0.8rem;
		color: var(--film-ink);
		/* each line lands as the basket fills, one after another */
		opacity: 0;
		transform: translateY(4px);
		transition:
			opacity 750ms ease,
			transform 750ms ease;
	}
	.mt-zline.in {
		opacity: 1;
		transform: none;
	}
	/* The two it decided against: struck through, dimmed, and their box is
	   plainly not tickable — this is a decision already made, not a control
	   waiting for you. */
	.mt-zline.off {
		color: var(--film-dim);
	}
	.mt-zline.off .mt-zitem,
	.mt-zline.off .mt-zprice {
		text-decoration: line-through;
		opacity: 0.62;
	}
	.mt-zcheck {
		grid-column: 1;
		grid-row: 1;
		width: 15px;
		height: 15px;
		border-radius: 4px;
		border: 1.5px solid color-mix(in srgb, var(--film-ink) 34%, transparent);
		position: relative;
		transition:
			background 700ms ease,
			border-color 700ms ease;
	}
	.mt-zcheck.on {
		background: var(--a);
		border-color: var(--a);
	}
	.mt-zcheck.on::after {
		content: '✓';
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		font-size: 10px;
		line-height: 1;
		color: var(--landing-bg, #fff);
	}
	.mt-zline.off .mt-zcheck {
		border-style: dashed;
		border-color: color-mix(in srgb, var(--film-ink) 18%, transparent);
	}
	.mt-zitem {
		grid-column: 2;
		grid-row: 1;
		min-width: 0;
	}
	.mt-znote {
		/* the reason, under the line it explains — every explicit, because
		   a two-row grid with one optional cell auto-places unpredictably */
		grid-column: 2 / 4;
		grid-row: 2;
		font-size: 0.62rem;
		color: var(--th-ink, var(--a-ink));
	}
	.mt-zprice {
		grid-column: 3;
		grid-row: 1;
		font-size: 0.74rem;
		color: var(--film-dim);
		white-space: nowrap;
	}
	.mt-zfoot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-top: 0.5rem;
		font-size: 0.7rem;
		color: var(--film-dim);
	}
	.mt-zeta {
		margin-left: 0.5rem;
		font-size: 0.7rem;
		color: var(--a-ink);
		white-space: nowrap;
	}

	/* ── 5 · THINKS ───────────────────────────────────────────── */
	.mt-research {
		display: grid;
		gap: 0.8rem;
		justify-items: center;
		/* wide enough that the @brainstorm graph and the memory recalls
		   stand SIDE BY SIDE on desktop */
		width: min(92vw, 42rem);
	}
	.mt-sources {
		display: flex;
		gap: 0.5rem;
		flex-wrap: wrap;
		justify-content: center;
	}
	.mt-source {
		border: 1px solid var(--landing-chip-border);
		background: var(--landing-chip-bg);
		border-radius: 10px;
		padding: 0.4rem 0.7rem;
		font-size: 0.8rem;
		font-family: var(--lp-font);
		color: var(--text-primary);
		/* the pricing sources land AFTER the picked branch expands —
		   they are the expansion's homework, not a prior exhibit */
		opacity: clamp(0, calc((var(--local) - 0.56 - var(--i) * 0.02) * 8), 1);
		transform: rotate(calc((var(--i) - 1.5) * (1 - clamp(0, calc((var(--local) - 0.56) * 5), 1)) * 6deg))
			translateY(calc((1 - clamp(0, calc((var(--local) - 0.56) * 5), 1)) * var(--i) * 6px));
	}
	.mt-source .mt-mono {
		color: var(--a-ink);
		margin-right: 0.3rem;
	}
	.mt-brief {
		opacity: clamp(0, calc((var(--local) - 0.62) * 6), 1);
	}
	.mt-gaui {
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: end;
		gap: 1rem;
		opacity: clamp(0, calc((var(--local) - 0.6) * 5), 1);
	}
	.mt-scene.static .mt-brief,
	.mt-scene.static .mt-gaui {
		opacity: 1;
	}
	.mt-metric {
		display: grid;
	}
	.mt-metric .mt-mono {
		font-size: 1.15rem;
		color: var(--a-ink);
	}
	.mt-metric small {
		color: var(--film-dim);
	}
	.mt-chart {
		display: flex;
		align-items: flex-end;
		gap: 5px;
		height: 54px;
	}
	.mt-chart span {
		flex: 1;
		min-width: 10px;
		border-radius: 3px 3px 0 0;
		background: linear-gradient(180deg, var(--a), var(--a2));
		height: calc(var(--h) * 100%);
		transform: scaleY(clamp(0, calc((var(--local) - 0.62 - var(--i) * 0.04) * 7), 1));
		transform-origin: bottom;
	}
	.mt-scene.static .mt-chart span {
		transform: none;
	}
	.mt-gaui .mt-brief {
		width: auto;
		max-width: 13rem;
		font-size: 0.8rem;
		padding: 0.6rem 0.8rem;
		align-self: center;
	}
	/* The living graph: a voice line becomes structure, then a WALK —
	   each branch opens as an ask, memory recollects beside it, the ask
	   becomes an answer, and the walk lands on a conclusion. */
	.mt-graphwrap {
		display: grid;
		justify-items: center;
		gap: 0.45rem;
	}
	.mt-reason {
		display: flex;
		align-items: center;
		justify-content: center;
		flex-wrap: wrap;
		gap: 0.4rem 1.1rem;
	}
	.mt-recalls {
		display: grid;
		gap: 0.5rem;
		max-width: 15.5rem;
	}
	.mt-recall {
		border: 1px solid var(--film-card-border);
		border-left: 3px solid var(--th);
		background:
			linear-gradient(180deg, rgba(255, 255, 255, 0.05), transparent 45%),
			var(--film-card);
		border-radius: 10px;
		padding: 0.4rem 0.65rem;
		font-family: var(--lp-font);
		font-size: 0.76rem;
		color: var(--text-primary);
		box-shadow: 0 10px 30px -14px var(--th-glow);
		opacity: 0;
		transform: translateX(12px);
		transition:
			opacity 800ms ease,
			transform 850ms cubic-bezier(0.22, 1, 0.36, 1);
	}
	.mt-recall.on {
		opacity: 1;
		transform: none;
	}
	.mt-recall-k {
		display: block;
		font-size: 0.54rem;
		text-transform: uppercase;
		letter-spacing: 0.22em;
		color: var(--th-ink);
		margin-bottom: 0.14rem;
	}
	.mt-conclusion {
		border: 1px solid var(--ah-strong);
		background:
			linear-gradient(180deg, rgba(255, 255, 255, 0.05), transparent 45%),
			var(--film-card);
		border-radius: 12px;
		padding: 0.5rem 0.9rem;
		font-family: var(--lp-font);
		font-size: 0.92rem;
		font-weight: 550;
		color: var(--text-primary);
		box-shadow: 0 14px 44px -16px var(--ah-strong);
		opacity: 0;
		transform: translateY(9px);
		transition:
			opacity 800ms ease,
			transform 800ms ease;
	}
	.mt-conclusion.on {
		opacity: 1;
		transform: none;
	}
	/* the summon: @brainstorm, asked in the house grammar */
	.mt-bsummon {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		border: 1px solid var(--landing-form-border);
		background: var(--landing-form-surface);
		border-radius: 10px;
		padding: 0.4rem 0.7rem;
		font-size: 0.78rem;
		color: var(--text-primary);
		opacity: 0;
		transform: translateY(6px);
		transition:
			opacity 750ms ease,
			transform 750ms ease;
	}
	.mt-bsummon.on {
		opacity: 1;
		transform: none;
	}
	.mt-graph {
		position: relative;
		width: min(88vw, 340px);
		height: 170px;
	}
	.mt-graph-edges {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		overflow: visible;
	}
	.mt-edge {
		fill: none;
		stroke: var(--ah-strong);
		stroke-width: 1.6;
		stroke-dasharray: 100;
		/* each edge carries its own start (--et): the root's fan first, the
		   picked branch's AI-grown fan later */
		stroke-dashoffset: calc(100 - clamp(0, calc((var(--local) - var(--et)) * 10), 1) * 100);
	}
	.mt-scene.static .mt-edge {
		stroke-dashoffset: 0;
	}
	.mt-gnode {
		position: absolute;
		left: var(--gx, 92px);
		top: var(--gy, 75px);
		transform: translate(-50%, -50%) scale(0.25);
		opacity: 0;
		border: 1px solid var(--ah-strong);
		background:
			linear-gradient(180deg, rgba(255, 255, 255, 0.05), transparent 55%),
			var(--film-card);
		color: var(--text-primary);
		font-family: var(--lp-font);
		font-size: 0.76rem;
		border-radius: 999px;
		padding: 0.28rem 0.7rem;
		white-space: nowrap;
		box-shadow:
			inset 0 1px 0 rgba(255, 255, 255, 0.08),
			0 8px 26px var(--ah);
		/* the spring: visible overshoot, then settle */
		transition:
			transform 850ms cubic-bezier(0.22, 1, 0.36, 1),
			opacity 700ms ease;
	}
	.mt-gnode.pop {
		opacity: 1;
		transform: translate(-50%, -50%) scale(1);
	}
	.mt-gnode-root {
		font-weight: 600;
		border-color: var(--a);
	}
	/* the picked branch: the mouse chose it, and it wears the ring */
	.mt-gnode.picked {
		border-color: var(--a);
		box-shadow:
			0 0 0 2px var(--ah-strong),
			0 8px 26px var(--ah);
	}
	/* the AI-grown second fan: smaller, springing off the picked branch */
	.mt-gnode-l2 {
		font-size: 0.68rem;
		padding: 0.22rem 0.55rem;
	}
	/* the working beat: the AI visibly expanding the picked branch */
	.mt-gwork {
		position: absolute;
		left: 196px;
		top: 62px;
		transform: translate(-50%, -100%);
		font-size: 0.5rem;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--a-ink);
		opacity: 0;
		transition: opacity 700ms ease;
	}
	.mt-gwork.on {
		opacity: 1;
		animation: mt-breathe 1.2s ease-in-out infinite;
	}
	/* the mouse: rides in, clicks "teach online", and leaves */
	.mt-gcursor {
		position: absolute;
		--cw: clamp(0, calc((var(--local) - 0.37) / 0.08), 1);
		left: calc(126px + 70px * var(--cw));
		top: calc(172px - 76px * var(--cw));
		width: 12px;
		height: 17px;
		background: var(--film-ink);
		clip-path: polygon(0 0, 100% 62%, 55% 62%, 72% 100%, 55% 100%, 42% 68%, 0 86%);
		filter: drop-shadow(0 2px 3px rgba(0, 0, 0, 0.3));
		opacity: calc(
			clamp(0, calc((var(--local) - 0.35) * 9), 1) -
				clamp(0, calc((var(--local) - 0.5) * 9), 1)
		);
	}
	.mt-scene.static .mt-gnode {
		transition: none;
	}

	/* ── 6 · ORB ──────────────────────────────────────────────── */
	.mt-orbstage {
		position: relative;
		width: 100%;
		height: min(64vh, 34rem);
		display: grid;
		place-items: center;
		/* the handoff: past the reveal the orb departs down-left toward the
		   pocket station — 0 while dwelling, 1 as the leg completes. The
		   window tracks TRAVEL_START[11] (0.82 of the 3.0-weight dwell):
		   same absolute flight as before the dwell grew. */
		--dep: clamp(0, calc((var(--local) - 0.83) / 0.16), 1);
		/* where the flight ENDS: the pocket notification's orb glyph. The
		   station delta (pocket − orb in world units) plus the glyph's
		   px offset inside the centered card — so the orb LANDS on the
		   glyph instead of vanishing mid-leg. Mobile factors (xf 0.35,
		   yf 0.88) re-tune this in the 640 media block. */
		--depx: calc(-13vw - 82px);
		--depy: calc(112vh + 9px);
	}
	.mt-orbstage::before {
		/* the money shot blooms past the canvas bounds — cheap CSS radial */
		content: '';
		position: absolute;
		left: 50%;
		top: 50%;
		width: min(88vw, 60rem);
		aspect-ratio: 1;
		transform: translate(-50%, -50%);
		border-radius: 50%;
		background: radial-gradient(
			circle,
			var(--voice-orb-halo-strong) 0%,
			var(--voice-orb-halo) 32%,
			transparent 68%
		);
		opacity: calc(0.8 * (1 - var(--dep)));
		pointer-events: none;
	}
	.mt-orb-carrier {
		/* fills the stage: the orb canvas sizes itself from its PARENT's
		   client box, so the carrier must be stage-sized, never content-sized */
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		/* fly the WHOLE station delta so the orb genuinely arrives at the
		   notification glyph (the camera travels with it, so mid-flight
		   the orb hugs the card's height and slides onto the pad); only
		   the last few percent fade, exactly as the glyph's landing surge
		   takes over — a handoff, not a vanish */
		transform: translate(calc(var(--dep) * var(--depx)), calc(var(--dep) * var(--depy)))
			scale(calc(1 - var(--dep) * 0.94));
		opacity: calc(1 - max(0, var(--dep) - 0.93) * 14);
	}
	.mt-scene.static .mt-orbstage::before {
		opacity: 0.8;
	}
	.mt-scene.static .mt-orb-carrier {
		transform: none;
		opacity: 1;
	}
	.mt-orb-static {
		width: 12rem;
		height: 12rem;
		border-radius: 46% 54% 52% 48% / 52% 46% 54% 48%;
		background: radial-gradient(
			circle at 35% 30%,
			var(--voice-orb-highlight),
			var(--voice-orb-a) 45%,
			var(--voice-orb-b)
		);
		box-shadow: 0 0 80px var(--voice-orb-halo-strong);
	}
	.mt-orb-word {
		position: absolute;
		top: 10%;
		margin: 0;
		font-size: 0.72rem;
		letter-spacing: 0.32em;
		text-transform: uppercase;
		color: var(--th-ink);
		opacity: calc(1 - var(--dep) * 2);
	}
	.mt-scene.static .mt-orb-word {
		position: static;
		opacity: 1;
	}
	.mt-orb-word-in {
		/* each phase word arrives breathing — a soft rise as it takes over */
		display: inline-block;
		animation: mt-word-in 850ms ease both;
	}
	@keyframes mt-word-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}
	.mt-island-orb {
		width: 11px;
		height: 11px;
		border-radius: 50%;
		background: radial-gradient(
			circle at 35% 30%,
			var(--voice-orb-highlight),
			var(--voice-orb-a) 45%,
			var(--voice-orb-b)
		);
		/* steady glow only — the LANDING now belongs to the notification
		   glyph at screen center, not the island */
		box-shadow:
			0 0 6px var(--voice-orb-halo-strong),
			0 0 13px var(--voice-orb-halo);
	}
	.mt-island-word {
		font-family: var(--lp-mono);
		font-size: 0.7rem;
		letter-spacing: 0.04em;
	}
	.mt-island-ring {
		width: 12px;
		height: 12px;
		border-radius: 50%;
		border: 2px solid var(--voice-orb-halo-strong);
		border-top-color: transparent;
	}
	/* Wow #2's caption — lands as the coil completes, replacing the intro
	   line in the same slot (a crossfade, never a pile-up). */
	.mt-orb-reveal {
		position: absolute;
		bottom: 7%;
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(1rem, 2vw, 1.35rem);
		font-weight: 340;
		color: var(--film-ink);
		text-align: center;
		white-space: nowrap;
		/* legibility over the orb bloom: a halo of the GROUND, not of night */
		text-shadow: 0 2px 18px var(--landing-bg);
		/* rises in as the coil completes; gone within the first third of the
		   departure — the line must not stripe across the shrinking blob */
		opacity: calc(
			clamp(0, calc((var(--local) - 0.44) * 6), 1) * clamp(0, calc(1 - var(--dep) * 3), 1)
		);
		transform: translateY(calc((1 - clamp(0, calc((var(--local) - 0.44) * 6), 1)) * 10px));
	}
	.mt-orb-reveal strong {
		font-weight: 700;
		background: linear-gradient(
			100deg,
			var(--th-ink),
			color-mix(in srgb, var(--a2) 70%, var(--film-ink))
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	.mt-scene.static .mt-orb-reveal {
		position: static;
		opacity: 1;
		transform: none;
		white-space: normal;
	}

	/* ── 11 · POCKET — the lock-screen wake ───────────────────── */
	.mt-pocketstage {
		display: grid;
		justify-items: center;
		gap: 0.8rem;
	}
	.mt-heypresto {
		margin: 0;
		font-size: 0.95rem;
		letter-spacing: 0.14em;
		color: var(--th-ink);
		opacity: 0;
		transform: translateY(6px);
		transition:
			opacity 800ms ease,
			transform 800ms ease;
	}
	.mt-heypresto.onair {
		opacity: 1;
		transform: none;
	}
	.mt-phone {
		position: relative;
		width: min(62vw, 250px);
		aspect-ratio: 9 / 18.5;
		border-radius: 44px;
		background: #101014;
		border: 1px solid #2a2a33;
		box-shadow:
			0 46px 90px -30px rgba(10, 6, 24, 0.65),
			0 24px 80px -28px var(--th-glow);
		padding: 9px;
	}
	.mt-phone-screen {
		position: relative;
		width: 100%;
		height: 100%;
		border-radius: 36px;
		overflow: hidden;
		/* the lock-screen depth wallpaper: a violet dusk with a horizon */
		background:
			radial-gradient(130% 66% at 50% -4%, rgba(96, 58, 178, 0.4), transparent 58%),
			radial-gradient(120% 50% at 50% 108%, rgba(34, 24, 78, 0.9), transparent 74%),
			linear-gradient(180deg, #0b0b18, #14142a 55%, #0a0a16);
		display: grid;
		justify-items: center;
		align-content: start;
		padding-top: 11px;
		color: #f5f2ff;
		font-family: var(--lp-font);
	}
	.mt-phone-screen::after {
		/* the wake surge: violet floods down from the island */
		content: '';
		position: absolute;
		inset: 0;
		background: radial-gradient(95% 42% at 50% 0%, rgba(158, 89, 255, 0.5), transparent 72%);
		opacity: 0;
		transition: opacity 0.7s ease;
		pointer-events: none;
	}
	.mt-phone.awake .mt-phone-screen::after {
		opacity: 1;
	}
	.mt-pisland {
		/* The Dynamic Island, ambient-mode truth: black glass; compact is a
		   bare orb dot, expanded is orb + word + leash ring. */
		position: relative;
		z-index: 1;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.4rem;
		background: #000;
		border-radius: 999px;
		min-height: 24px;
		padding: 0.28rem 0.6rem;
		color: #fff;
		box-shadow: 0 8px 22px -8px rgba(0, 0, 0, 0.9);
		transition: padding 750ms ease;
	}
	.mt-lock-clock {
		position: relative;
		z-index: 1;
		margin-top: clamp(0.8rem, 2.4vh, 1.6rem);
		font-weight: 200;
		font-size: clamp(2.4rem, 5.6vh, 3.3rem);
		letter-spacing: 0.02em;
		line-height: 1;
	}
	.mt-lock-date {
		position: relative;
		z-index: 1;
		font-size: 0.7rem;
		color: rgba(240, 238, 255, 0.62);
		margin-top: 0.2rem;
	}
	.mt-lock-padlock {
		position: relative;
		z-index: 1;
		width: 13px;
		margin-top: 0.55rem;
	}
	.mt-lock-padlock rect {
		fill: rgba(240, 238, 255, 0.72);
	}
	.mt-lock-padlock path {
		stroke: rgba(240, 238, 255, 0.72);
		stroke-width: 1.5;
		fill: none;
	}
	.mt-lock-notif {
		/* ONE iOS notification, CENTER of the lock screen. The container
		   never fades — its glyph is the departing orb's landing pad, so
		   it must exist from the first frame; the glass chrome (::before)
		   and the text body compose around the landed orb at wake time. */
		position: absolute;
		left: 5.5%;
		right: 5.5%;
		top: 46%;
		z-index: 1;
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.5rem 0.6rem;
	}
	.mt-lock-notif::before {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: 15px;
		background: rgba(248, 245, 255, 0.14);
		border: 1px solid rgba(255, 255, 255, 0.12);
		backdrop-filter: blur(12px) saturate(1.4);
		-webkit-backdrop-filter: blur(12px) saturate(1.4);
		box-shadow: 0 12px 30px -14px rgba(0, 0, 0, 0.6);
		opacity: 0;
		transform: scale(0.92);
		transition:
			opacity 800ms ease,
			transform 800ms ease;
	}
	.mt-lock-notif.on::before {
		opacity: 1;
		transform: none;
	}
	.mt-notif-orb {
		/* the app glyph IS the ambient orb — and it does not exist until
		   the flight delivers it: invisible before touchdown, a bright
		   surge as the carrier melts into it, then icon scale once the
		   wake owns the screen */
		position: relative;
		flex: none;
		width: 22px;
		height: 22px;
		border-radius: 50%;
		background: radial-gradient(
			circle at 35% 30%,
			var(--voice-orb-highlight),
			var(--voice-orb-a) 45%,
			var(--voice-orb-b)
		);
		opacity: 0;
		box-shadow:
			0 0 6px var(--voice-orb-halo-strong),
			inset 0 0 4px rgba(255, 255, 255, 0.35);
		transition:
			opacity 800ms ease,
			transform 800ms ease,
			box-shadow 800ms ease;
	}
	.mt-notif-orb.mt-landing {
		opacity: 1;
		transform: scale(1.5);
		box-shadow:
			0 0 30px var(--voice-orb-halo-strong),
			0 0 8px var(--voice-orb-halo-strong),
			inset 0 0 4px rgba(255, 255, 255, 0.35);
	}
	.mt-notif-orb.mt-lit {
		opacity: 1;
	}
	.mt-notif-body {
		position: relative;
		flex: 1;
		min-width: 0;
		opacity: 0;
		transform: translateY(6px);
		transition:
			opacity 800ms ease,
			transform 800ms ease;
	}
	.mt-lock-notif.on .mt-notif-body {
		opacity: 1;
		transform: none;
	}
	.mt-notif-app {
		margin: 0;
		display: flex;
		align-items: baseline;
		font-size: 0.62rem;
		font-weight: 650;
		letter-spacing: 0.01em;
		color: rgba(248, 246, 255, 0.95);
	}
	.mt-notif-when {
		margin-left: auto;
		font-weight: 400;
		font-size: 0.52rem;
		color: rgba(240, 238, 255, 0.55);
	}
	.mt-notif-swap {
		/* all three beats share one cell; the live one fades up. Height is
		   reserved for the two-line reply so the card never resizes, and
		   short beats sit centered in that reserve rather than floating
		   over emptiness. */
		display: grid;
		align-items: center;
		min-height: 2.9em;
		font-size: 0.64rem;
	}
	.mt-notif-line {
		grid-area: 1 / 1;
		margin: 0.1rem 0 0;
		line-height: 1.35;
		text-wrap: balance;
		color: rgba(243, 240, 255, 0.88);
		opacity: 0;
		transform: translateY(5px);
		transition:
			opacity 800ms ease,
			transform 800ms ease;
	}
	.mt-notif-line strong {
		font-weight: 650;
		color: #fff;
	}
	.mt-notif-line.on {
		opacity: 1;
		transform: none;
	}
	/* Document mode: the one card fully composed — all three beats
	   stacked, so the whole exchange reads without scroll. */
	.mt-scene.static .mt-notif-swap {
		min-height: 0;
	}
	.mt-scene.static .mt-notif-line {
		grid-area: auto;
		opacity: 1;
		transform: none;
	}

	/* ── 12 · TUTOR — a Pythagoras webpage; Magican summoned ON it; the
	   overlay draws over the page's own triangle; the phone arrives to
	   show the same lesson — web and mobile, one tutor. ── */
	.mt-tutorstage {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: clamp(0.8rem, 2vw, 1.6rem);
	}
	.mt-tbrowser {
		width: min(72vw, 27rem);
		border-radius: 16px;
		border: 1px solid var(--film-card-border);
		background: var(--film-card);
		box-shadow:
			var(--film-card-shadow),
			0 24px 80px -28px var(--ah-strong);
		overflow: hidden;
	}
	.mt-tpage {
		position: relative;
		background: var(--bg-elevated, var(--film-card));
		color: var(--text-primary);
		padding: 0.85rem 1.1rem 1rem;
		display: grid;
		gap: 0.55rem;
		justify-items: center;
		font-family: var(--lp-font);
	}
	.mt-tpage-h {
		margin: 0;
		justify-self: start;
		font-size: 0.95rem;
		font-weight: 650;
		letter-spacing: -0.01em;
	}
	.mt-tpage-lines {
		justify-self: stretch;
		height: 24px;
		background: repeating-linear-gradient(
			180deg,
			color-mix(in srgb, var(--text-primary) 16%, transparent) 0 3px,
			transparent 3px 11px
		);
		border-radius: 2px;
	}
	.mt-chalk-pyth {
		width: min(50vw, 232px);
		overflow: visible;
	}
	/* the page's own figure: plain textbook ink, already printed */
	.mt-tpage-tri {
		fill: none;
		stroke: var(--text-primary);
		stroke-width: 2;
		stroke-linecap: round;
		stroke-linejoin: round;
	}
	/* the tutor's hand: theme ink drawn OVER the page, stroke by stroke —
	   a² and b², the question, and only then c² answers it */
	.mt-stroke {
		fill: none;
		stroke: var(--a);
		stroke-width: 2.6;
		stroke-linecap: round;
		stroke-linejoin: round;
		stroke-dasharray: 100;
		filter: drop-shadow(0 0 4px var(--ah-strong));
	}
	.mt-py-a {
		stroke-dashoffset: calc(100 - clamp(0, calc((var(--local) - 0.28) * 8), 1) * 100);
	}
	.mt-py-b {
		stroke-dashoffset: calc(100 - clamp(0, calc((var(--local) - 0.4) * 8), 1) * 100);
	}
	.mt-py-c {
		stroke-width: 3;
		stroke-dashoffset: calc(100 - clamp(0, calc((var(--local) - 0.56) * 8), 1) * 100);
	}
	.mt-py-label {
		font-family: var(--lp-mono);
		font-size: 15px;
		fill: var(--a-ink);
	}
	.mt-py-la {
		opacity: clamp(0, calc((var(--local) - 0.34) * 12), 1);
	}
	.mt-py-lb {
		opacity: clamp(0, calc((var(--local) - 0.46) * 12), 1);
	}
	.mt-py-lc {
		fill: var(--a);
		opacity: clamp(0, calc((var(--local) - 0.64) * 12), 1);
	}
	/* the summon: Magican called up on the page you were already reading */
	.mt-tsummon {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		justify-self: stretch;
		border: 1px solid var(--th-soft);
		background: var(--bg-card, var(--bg-elevated));
		border-radius: 10px;
		padding: 0.4rem 0.65rem;
		font-size: 0.8rem;
		color: var(--text-primary);
		box-shadow: 0 8px 26px -10px var(--th-glow);
		--sw2: clamp(0, calc((var(--local) - 0.1) * 8), 1);
		opacity: var(--sw2);
		transform: translateY(calc((1 - var(--sw2)) * 6px));
	}
	.mt-tsummon .mt-prompt {
		color: var(--a);
	}
	.mt-tanswer {
		justify-self: stretch;
		display: grid;
		gap: 0.25rem;
		border-left: 3px solid var(--th);
		background: var(--ah);
		border-radius: 0 10px 10px 0;
		padding: 0.45rem 0.7rem;
		opacity: clamp(0, calc((var(--local) - 0.48) * 9), 1);
	}
	.mt-tutorline {
		color: var(--text-secondary);
		min-height: 1.3em;
		font-size: 0.8rem;
		margin: 0;
		opacity: clamp(0, calc((var(--local) - 0.5) * 10), 1);
	}
	.mt-scene.static .mt-tutorline {
		opacity: 1;
	}
	.mt-py-eq {
		margin: 0;
		font-size: 1.05rem;
		letter-spacing: 0.06em;
		color: var(--text-primary);
	}
	.mt-py-eq span {
		opacity: 0;
	}
	.mt-py-ea {
		opacity: clamp(0, calc((var(--local) - 0.6) * 16), 1) !important;
	}
	.mt-py-plus {
		opacity: clamp(0, calc((var(--local) - 0.63) * 16), 1) !important;
	}
	.mt-py-eb {
		opacity: clamp(0, calc((var(--local) - 0.655) * 16), 1) !important;
	}
	.mt-py-equals {
		opacity: clamp(0, calc((var(--local) - 0.68) * 16), 1) !important;
	}
	.mt-py-ec {
		color: var(--a);
		opacity: clamp(0, calc((var(--local) - 0.7) * 16), 1) !important;
	}
	/* the phone arrives once the lesson is under way: same figure, same
	   overlay, same stroke timings — one tutor on both screens */
	.mt-tphone {
		display: grid;
		gap: 0.5rem;
		justify-items: center;
		--pw: clamp(0, calc((var(--local) - 0.54) * 7), 1);
		opacity: var(--pw);
		transform: translateX(calc((1 - var(--pw)) * 26px));
	}
	.mt-scene.static .mt-tphone {
		opacity: 1;
		transform: none;
	}
	.mt-tphone .mt-mini-screen {
		display: grid;
		justify-items: center;
		align-content: start;
		background: var(--bg-elevated);
		color: var(--text-primary);
	}
	.mt-tphone-fig {
		position: relative;
		z-index: 1;
		width: 80%;
		margin-top: 36px;
		overflow: visible;
	}
	.mt-tphone-eq {
		position: relative;
		z-index: 1;
		margin-top: 0.3rem;
		font-size: 0.6rem;
		color: var(--text-primary);
		opacity: clamp(0, calc((var(--local) - 0.68) * 10), 1);
	}

	/* ── 7 · NIGHT + RECEIPT ──────────────────────────────────── */
	.mt-warroom {
		/* Scene-truth dark: the night deck IS a dark product screen, now a
		   dark object sitting in the light world — so it casts a real
		   shadow with a phosphor bleed instead of dissolving into ground. */
		--phos: #38e1c9;
		width: min(92vw, 34rem);
		background: #05080b;
		border-radius: 18px;
		border: 1px solid rgba(56, 225, 201, 0.18);
		padding: 0.9rem;
		display: grid;
		gap: 0.7rem;
		box-shadow:
			0 30px 70px -24px rgba(5, 8, 11, 0.45),
			0 24px 80px -30px rgba(56, 225, 201, 0.3);
		transition: border-color 800ms ease;
	}
	.mt-warroom.needs-you {
		--phos: #ffb454;
		border-color: rgba(255, 180, 84, 0.35);
	}
	.mt-nightrow {
		/* one overnight outcome, timestamped: the log ticks in line by
		   line as the night passes under the scrub */
		display: flex;
		align-items: baseline;
		gap: 0.55rem;
		border: 1px solid color-mix(in srgb, var(--phos) 18%, transparent);
		background: color-mix(in srgb, var(--phos) 4%, transparent);
		border-radius: 8px;
		padding: 0.42rem 0.6rem;
		--nr: clamp(0, calc((var(--local) - 0.05 - var(--i) * 0.06) * 7), 1);
		opacity: var(--nr);
		transform: translateY(calc((1 - var(--nr)) * 6px));
	}
	.mt-night-t {
		flex: none;
		color: var(--phos);
		font-size: 0.62rem;
		letter-spacing: 0.06em;
	}
	.mt-night-k {
		flex: none;
		color: var(--phos);
		font-size: 0.54rem;
		text-transform: uppercase;
		letter-spacing: 0.14em;
		opacity: 0.75;
	}
	.mt-night-line {
		min-width: 0;
		color: #d7e6e2;
		font-family: var(--lp-font);
		font-size: 0.78rem;
	}
	.mt-scene.static .mt-nightrow {
		opacity: 1;
		transform: none;
	}
	.mt-needcard {
		border: 1px solid rgba(255, 180, 84, 0.4);
		border-radius: 10px;
		padding: 0.5rem 0.8rem;
		color: #ffe1b8;
		font-family: var(--lp-font);
		font-size: 0.84rem;
		opacity: clamp(0, calc((var(--local) - 0.14) * 6), 1);
	}
	.mt-scene.static .mt-needcard {
		opacity: 1;
	}
	.mt-receipt-card {
		opacity: clamp(0, calc((var(--local) - 0.36) * 6), 1);
		transform: translateY(calc((1 - clamp(0, calc((var(--local) - 0.36) * 6), 1)) * 14px));
	}
	.mt-scene.static .mt-receipt-card {
		opacity: 1;
		transform: none;
	}
	.mt-receipt-head {
		display: flex;
		justify-content: space-between;
		font-weight: 600;
		margin-bottom: 0.5rem;
	}
	.mt-receipt-row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		padding: 0.28rem 0;
		font-size: 0.88rem;
		opacity: clamp(0, calc((var(--local) - 0.4 - var(--i) * 0.05) * 6), 1);
	}
	.mt-scene.static .mt-receipt-row {
		opacity: 1;
	}
	.mt-check {
		color: var(--a-ink);
		font-weight: 700;
	}
	.mt-receipt-meta {
		margin-top: 0.45rem;
		color: var(--a-ink);
		font-size: 0.78rem;
		opacity: clamp(0, calc((var(--local) - 0.62) * 6), 1);
	}
	.mt-receipt-mem {
		margin-top: 0.3rem;
		font-size: 0.8rem;
		color: var(--film-dim);
		opacity: clamp(0, calc((var(--local) - 0.68) * 6), 1);
	}
	.mt-scene.static .mt-receipt-meta,
	.mt-scene.static .mt-receipt-mem {
		opacity: 1;
	}
	.mt-s-night .mt-caption {
		/* the phosphor line steps OUT of the deck onto paper: teal as ink */
		color: color-mix(in srgb, #38e1c9 50%, var(--film-ink));
	}

	/* ── the thread layer ─────────────────────────────────────── */
	.mt-thread {
		position: absolute;
		inset: 0;
		z-index: 3;
		width: 100%;
		height: 100%;
		pointer-events: none;
	}
	.mt-thread-under {
		/* beneath the camera's world (z 2), above the ground layers: the
		   canvas the thread dives onto when a segment is behind-flagged */
		z-index: 1;
	}
	.mt-frag {
		position: absolute;
		left: 0;
		top: 0;
		z-index: 5;
		pointer-events: none;
		opacity: 0;
		will-change: transform, opacity;
		font-size: 0.66rem;
		letter-spacing: 0.04em;
		white-space: nowrap;
		color: var(--th-ink);
		background: var(--landing-form-surface, rgba(255, 255, 255, 0.74));
		border: 1px solid var(--th-soft);
		border-radius: 999px;
		padding: 0.22rem 0.6rem;
		box-shadow: 0 10px 30px -12px var(--th-glow);
		backdrop-filter: blur(4px);
	}

	/* ── playhead: the thread-map rail + skip ─────────────────── */
	.mt-rail {
		position: absolute;
		right: clamp(0.6rem, 2vw, 1.6rem);
		top: 50%;
		transform: translateY(-50%);
		z-index: 6;
		width: 26px;
		height: 288px;
	}
	.mt-map {
		position: absolute;
		left: 0;
		top: 0;
		overflow: visible;
	}
	.mt-map-base {
		fill: none;
		stroke: color-mix(in srgb, var(--film-ink) 25%, transparent);
		stroke-width: 1;
	}
	.mt-map-done {
		fill: none;
		stroke: var(--th-bright);
		stroke-width: 1.6;
		stroke-dasharray: 1;
		filter: drop-shadow(0 0 2px var(--th-bright)) drop-shadow(0 0 8px var(--th-glow))
			drop-shadow(0 0 15px var(--th-glow-wide));
	}
	.mt-dot {
		position: absolute;
		width: 7px;
		height: 7px;
		padding: 0;
		margin: 0;
		transform: translate(-50%, -50%);
		border-radius: 50%;
		border: 1px solid color-mix(in srgb, var(--film-ink) 40%, transparent);
		background: var(--landing-bg);
		cursor: pointer;
		transition:
			background 700ms ease,
			border-color 700ms ease,
			box-shadow 700ms ease;
	}
	.mt-dot.active {
		background: var(--th-bright);
		border-color: var(--th-bright);
		box-shadow:
			0 0 8px 2px var(--th-glow),
			0 0 20px 5px var(--th-glow-wide),
			0 0 34px 8px color-mix(in srgb, var(--th-glow-wide) 58%, transparent);
	}
	.mt-maphead {
		position: absolute;
		left: 4px;
		top: 4px;
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: var(--th-bright);
		border: 1px solid color-mix(in srgb, white 58%, var(--th));
		box-shadow:
			0 0 5px 2px var(--th-bright),
			0 0 14px 4px var(--th-glow),
			0 0 30px 8px var(--th-glow-wide),
			0 0 48px 11px color-mix(in srgb, var(--th-glow-wide) 48%, transparent);
		pointer-events: none;
		will-change: transform;
	}
	.mt-skip {
		position: absolute;
		bottom: 1.1rem;
		right: clamp(0.6rem, 2vw, 1.5rem);
		z-index: 6;
		font-family: var(--lp-mono);
		font-size: 0.7rem;
		letter-spacing: 0.06em;
		color: var(--film-dim);
		text-decoration: none;
		border: 1px solid var(--landing-chip-border, rgba(45, 52, 54, 0.08));
		background: var(--landing-chip-bg, rgba(255, 255, 255, 0.72));
		backdrop-filter: blur(8px);
		border-radius: 999px;
		padding: 0.32rem 0.8rem;
		transition:
			color 650ms ease,
			border-color 650ms ease;
	}
	.mt-skip:hover {
		color: var(--film-ink);
		border-color: color-mix(in srgb, var(--film-ink) 35%, transparent);
	}

	@keyframes mt-pulse {
		0%,
		100% {
			box-shadow: 0 0 0 0 var(--ah-strong);
		}
		50% {
			box-shadow: 0 0 0 7px transparent;
		}
	}
	@keyframes mt-blink {
		50% {
			opacity: 0;
		}
	}
	@keyframes mt-breathe {
		0%,
		100% {
			opacity: 0.75;
		}
		50% {
			opacity: 1;
		}
	}

	@media (max-width: 640px) {
		.mt-scene {
			gap: 0.7rem;
			padding: 0.9rem;
		}
		.mt-orb-reveal {
			white-space: normal;
			max-width: 90vw;
		}
		.mt-tiles {
			grid-template-columns: repeat(2, 1fr);
		}
		.mt-nightrow {
			/* narrow rows: the kind chip is enough — the timestamp yields */
			gap: 0.4rem;
		}
		.mt-night-line {
			font-size: 0.72rem;
		}
		.mt-rail {
			display: none;
		}
		.mt-desk {
			height: 240px;
		}
		.mt-operate-roles {
			gap: 0.22rem;
		}
		.mt-operate-roles span {
			grid-template-columns: 1fr;
			gap: 0.12rem;
			padding-inline: 0.28rem;
			text-align: center;
		}
		.mt-darkmodal {
			right: 6%;
		}
		.mt-phone {
			width: min(55vw, 216px);
		}
		.mt-orbstage {
			/* the landing pad's world offset under mobile factors
			   (xf 0.35, yf 0.88) and the narrower phone */
			--depx: calc(-4.6vw - 67px);
			--depy: calc(98.5vh + 28px);
		}
		.mt-gate {
			/* narrow cards: the chip row (lunch's choices, zepto's
			   authorize) drops below the question instead of cramping */
			flex-wrap: wrap;
		}
		.mt-mcp-first {
			white-space: nowrap;
		}
		.mt-delegate {
			/* phones: the plan takes the full row back — the phone made its
			   point up beside the doc; the plan-edit moment stays uncrowded */
			grid-template-areas:
				'doc phone'
				'composer phone'
				'task task';
		}
		.mt-mini-screen {
			width: 6.2rem;
		}
		.mt-tbrowser {
			width: 57vw;
		}
		.mt-chalk-pyth {
			width: 46vw;
		}
		.mt-tpage-h {
			font-size: 0.74rem;
		}
		.mt-tsummon {
			font-size: 0.62rem;
		}
		/* A phone has no room for "Magican makes your computer" on one line, so
		   line one WRAPS there — the mark keeps its own line and the verb
		   phrase drops under it — and the mark only steps back a little,
		   never to half itself, or the wordmark stops being the thing you
		   are being introduced to. */
		.mt-mini-field {
			font-size: 0.48rem;
			margin-top: 24px;
		}
		.mt-mini-act {
			font-size: 0.4rem;
			padding: 0.12rem 0.22rem;
		}
		.mt-mini-actions {
			gap: 2px;
		}
		.mt-mini-tag {
			font-size: 0.52rem;
			letter-spacing: 0.12em;
		}
		.mt-gaui {
			grid-template-columns: auto 1fr;
		}
		.mt-gaui .mt-brief {
			grid-column: 1 / -1;
			max-width: none;
		}
		/* the branch walk is tall; phones tighten it so the caption and
		   conclusion stay above the fold */
		.mt-chart {
			height: 40px;
		}
		.mt-recalls {
			gap: 0.35rem;
		}
		.mt-recall {
			font-size: 0.66rem;
			padding: 0.3rem 0.55rem;
		}
		.mt-conclusion {
			font-size: 0.8rem;
			padding: 0.4rem 0.7rem;
		}
		.mt-bsummon {
			font-size: 0.66rem;
			padding: 0.32rem 0.55rem;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.mt-live-dot,
		.mt-tile-dot,
		.mt-crt-cursor,
		.mt-crt-line,
		.mt-crt-screen::after,
		.mt-flame-g,
		.mt-fire-spark,
		.mt-lc-you,
		.mt-lc-magican,
		.mt-caret {
			animation: none;
		}
	}
</style>
