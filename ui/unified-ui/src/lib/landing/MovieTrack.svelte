<script lang="ts">
	// The landing movie — ONE CONTINUOUS FILM, and now ONE ARGUMENT.
	//
	// The 2026-08 re-choreography: every chapter is a STATION planted in
	// one continuous world on the app's own theme ground; one camera
	// (compositor transforms only) dollies between them along a waypoint
	// track driven by global scroll progress. The scrub machinery
	// (scrub.ts) is unchanged; the world/camera/thread math lives in
	// worldTrack.ts.
	//
	// The 2026-08-05 restructure fixed the film's incoherence: it used to
	// argue LABOUR (tools history, 1977, 1995 — "every tool still required
	// you") and then switch, mid-film, to OWNERSHIP ("your data became
	// their assets"), so the reveal answered one grievance and the payoff
	// line answered the other. Now:
	//   · the root hero names the visitor as many people; this film proves why
	//     the old operator relationship has to invert;
	//   · the pile-up station carries WORK multiplying, not data leaving;
	//   · the pile-up's “You became the glue between every app” closes the
	//     labour case and moves directly into the inversion;
	//   · the reveal states the inversion directly: the computer stopped
	//     waiting and now works for the person;
	//   · the day-in-the-life is gone from the main scroll — it is road C
	//     of the fork, opt-in, in the roads' own beat grammar.
	// The privacy argument the pile-up gave up is picked up by TrustReveal,
	// where it answers "can I trust it" instead of "why should this exist".
	//
	// The protagonist thread begins at the hero's `+`, enters this film at its
	// top edge, and takes one loose directional S through the history. It
	// blooms at the reveal's spark, then passes beneath the montage without
	// orbiting it. One journey, not a loading ring redrawn around every screen.
	//
	// Reduced motion is the house rule, not an afterthought: the track
	// collapses to auto height, the camera and thread never mount, and ALL
	// stations render stacked as a readable document with identical copy.
	import { onDestroy, onMount } from 'svelte';
	import { createScrubber, resolveScene } from './scrub';
	import { mixStops, rgba, type AuroraPhaseName } from './auroraPalette';
	import { themeThreadPalette } from './themeThreadPalette';
	import {
		motifBodyAlpha,
		motifHeadRadius,
		motifStrokeRecipe,
		motifTrailLengthPx,
		motifWaveOffset,
		sampleMotifTrail
	} from './motifFlow';
	import { motionEnabled } from '$lib/motion';
	import LifeReel from './LifeReel.svelte';
	import LifeDevice from './LifeDevice.svelte';
	import {
		BOUNDS,
		LEG_BOW,
		SI,
		SIGNAGE,
		STATIONS,
		TRACK_SVH,
		behindAt,
		buildThread,
		cameraAt,
		ignitionPoint,
		pAt,
		sampleThread,
		type Camera,
		type Station,
		type ThreadPath,
		type Vec
	} from './worldTrack';

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

	// The 1977 tube's phosphor. The root film now begins on this native
	// monitor, so it always uses the dark-tube recipe that originally shipped.
	const PHOSPHOR: Phosphor = {
		a: '#3dff7c',
		a2: '#18a352',
		ha: '#3dff7c',
		ah: 'rgba(61, 255, 124, 0.13)',
		ahs: 'rgba(61, 255, 124, 0.32)',
		g1: 'rgba(61, 255, 124, 0.06)',
		g2: 'rgba(61, 255, 124, 0.04)'
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
	const phosphor: Phosphor = PHOSPHOR;

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

	// ── The drowning: one window, and everything anyone ever left in it ───
	//
	// The station used to be a phone handing off between three apps. It is a
	// BROWSER now, and the argument is the tab strip: every tab is a piece of
	// work somebody opened and did not close, they arrive as you scroll, and
	// they get NARROWER as they multiply — which is the thing every person
	// reading this has watched happen to their own window. Nothing here is
	// anybody doing anything to you; it is just what accumulates.
	$: pu = sceneLocals[SI.drown];

	const TABS = [
		'Inbox (47)',
		'Re: Q3 approvals',
		'Invoice #4471',
		'Kyoto — fares',
		'Standup notes',
		'Design review',
		'Expense form',
		'WhatsApp Web',
		'Slack',
		'spec_v7.doc',
		'Bank — verify',
		'Renewal due',
		'Reset password',
		'Calendar',
		'Sprint 34 — board',
		'Q3_actuals.xlsx',
		'Feed',
		'Notifications (12)',
		'Untitled design',
		'Payroll — approve',
		'Vendor onboarding',
		'Re: Re: Re: budget',
		'Docs — offsite plan',
		'2FA — new device',
		'Recruiter — reply?',
		'Order #88213',
		'Insurance — claim',
		'Tickets assigned (9)'
	] as const;
	/** The strip fills across the dwell — ACCELERATING. A linear fill made the
	 *  station feel orderly, which is the opposite of the point: the last
	 *  third has to arrive faster than anyone can read it. Squared, half the
	 *  tabs land in the final quarter of the scroll. */
	$: tabsOn = reduced
		? TABS.length
		: Math.round(seg(pu, 0.03, 0.92) ** 2 * TABS.length);

	// Which tab is being WORKED. The strip is not decoration: the window
	// switches to each new tab as it lands and shows what is waiting in it,
	// so the reader watches the same person go round and round — and the
	// rounds get shorter. Nine surfaces, because four read as "a few apps"
	// and the felt experience is that there is no bottom to it. Each is
	// drawn to its own SHAPE, never its branding: the shapes are what people
	// recognise, and the logos are not ours to draw.
	const TAB_VIEWS = [
		'inbox',
		'chat',
		'board',
		'sheet',
		'feed',
		'form',
		'network',
		'canvas',
		'pay'
	] as const;
	// Also accelerating, and on the same curve as the strip, so a new surface
	// and a burst of new tabs land together rather than taking turns.
	$: tabView =
		TAB_VIEWS[
			Math.min(
				TAB_VIEWS.length - 1,
				Math.floor(seg(pu, 0.04, 0.94) ** 1.7 * TAB_VIEWS.length)
			)
		];
	/** Gmail-shaped inbox rows: sender, subject, snippet, time. */
	const MAIL_ROWS = [
		['Payroll', 'Q3 fare approvals', 'Three of these are still waiting on', '09:12'],
		['Meridian Labs', 'Re: contract v4', 'Attaching the redline — can you look', '08:47'],
		['IT Helpdesk', 'Action required: verify', 'Your session expires in 24 hours', '08:02'],
		['Priya', 'the deposit', 'Did we ever hear back about', 'Tue'],
		['Zepto', 'Order delivered', 'Rate your experience', 'Tue'],
		['Insurance', 'Renewal due 14 Nov', 'Auto-debit could not be completed', 'Mon']
	] as const;

	// The kanban board's columns. Declared here rather than inline in the
	// markup because an inline array of mixed tuples widens to
	// `string | number | string[]` and the inner {#each} then has nothing
	// iterable to hold on to.
	const BOARD_COLS: { col: string; n: number; cards: string[] }[] = [
		{ col: 'To do', n: 9, cards: ['Vendor onboarding', 'Q3 actuals', 'Offsite plan', 'Access review'] },
		{ col: 'In progress', n: 4, cards: ['Contract redline', 'Budget v4'] },
		{ col: 'Blocked on you', n: 6, cards: ['Design sign-off', 'Payroll approve', 'Claim #2210'] },
		{ col: 'Done', n: 1, cards: ['Kickoff deck'] }
	];

	// The counters that only ever go up. They are NOT in the window any more:
	// a number sitting inside a browser chrome reads as that app's own badge
	// and is easy to skip. Loose on the page, arriving from the right while
	// the leavings arrive from the left, they are the other half of the
	// squeeze — the person is being closed in on from both sides.
	const COUNTS = [
		// `rx`/`ry` are the RESTING places, and they are a COLUMN on fixed
		// rows, not a scatter. Scattered, chips of very different widths
		// overlapped each other, the headline and the window, and the
		// crowding read as a rendering fault rather than as a pile.
		//
		// `rx` is one number for the whole column — 19vw out from centre —
		// because the chip is anchored by its INNER edge (see .mt-badge-c's
		// transform), so the column has one straight margin no matter how
		// long the labels get. Rows 5.4vh apart clear a chip's height at
		// every viewport the film supports, and ±19.4vh keeps the column
		// clear of the title above and the closing line below.
		{ app: 'Mail', label: 'unread', from: 6, to: 214, t: 0.06, v: 1.09, fx: 46, fy: -34, rx: 19, ry: -19.4 },
		{ app: 'WhatsApp', label: 'unread', from: 2, to: 96, t: 0.14, v: 1.19, fx: 54, fy: -22, rx: 19, ry: -14 },
		{ app: 'Slack', label: 'mentions', from: 1, to: 63, t: 0.22, v: 1.32, fx: 44, fy: -8, rx: 19, ry: -8.6 },
		{ app: 'Tickets', label: 'assigned', from: 0, to: 31, t: 0.3, v: 1.47, fx: 52, fy: 6, rx: 19, ry: -3.2 },
		{ app: 'Payments', label: 'due', from: 0, to: 14, t: 0.38, v: 1.67, fx: 43, fy: 20, rx: 19, ry: 2.2 },
		{ app: 'Calendar', label: 'conflicts', from: 0, to: 9, t: 0.46, v: 1.92, fx: 55, fy: 34, rx: 19, ry: 7.6 },
		{ app: 'Docs', label: 'awaiting you', from: 1, to: 27, t: 0.54, v: 2.27, fx: 38, fy: 46, rx: 19, ry: 13 },
		{ app: 'Approvals', label: 'pending', from: 0, to: 18, t: 0.62, v: 2.78, fx: 60, fy: 30, rx: 19, ry: 18.4 }
	] as const;
	// The settled chip columns sit beside the laptop, never under it. The
	// machine's base is a little wider than its lid, so `placeMachine` derives
	// the live silhouette edge and adds this small optical gutter on both
	// sides. Keeping the final gap in pixels makes it read as deliberate
	// spacing rather than pushing the chips visibly away on large screens.
	const PILE_MACHINE_GUTTER_PX = 8;
	// THE NUMBERS ACCELERATE. Linearly-climbing counters read as a progress
	// bar — orderly, and finishing. Cubed, each one crawls for most of its
	// life and then runs away in its last third, which is the actual felt
	// shape of a backlog: it was fine, it was fine, and then it was 214.
	//
	// Every `v` is set so its counter is STILL CLIMBING at the station's end.
	// The first pass had them finish early — Mail hit 214 at local 0.66 and
	// then sat there — and a counter that has stopped is a counter that has
	// been dealt with. The implosion has to land while all eight are still
	// going, because what collapses is a thing still getting worse.
	$: countNow = COUNTS.map((c) => {
		const inAt = reduced ? 1 : Math.min(1, Math.max(0, (pu - c.t) * c.v));
		return Math.round(c.from + inAt ** 3 * (c.to - c.from));
	});

	// What each arrival is carrying, and how much of it. The COUNT climbs
	// with the chip's own approach, so a thing that is still far away is
	// still small and the one landing on you is the big one.
	const ARRIVALS = [
		// Same column discipline as COUNTS above, mirrored: ten fixed rows
		// 4.3vh apart, bounded to ±19.4vh, all sharing one inner margin at
		// −19vw because the chip is anchored by its right edge.
		{ label: 'tabs you meant to read', t: 0.03, v: 1.05, ax: -48, ay: -40, rx: -19, ry: -19.4, n0: 2, n1: 28 },
		{ label: 'messages unanswered', t: 0.1, v: 1.14, ax: -56, ay: -30, rx: -19, ry: -15.1, n0: 3, n1: 61 },
		{ label: 'forms half-filled', t: 0.17, v: 1.23, ax: -44, ay: -18, rx: -19, ry: -10.8, n0: 1, n1: 9 },
		{ label: 'logins to redo', t: 0.24, v: 1.35, ax: -50, ay: -6, rx: -19, ry: -6.5, n0: 1, n1: 7 },
		{ label: 'receipts to file', t: 0.31, v: 1.49, ax: -34, ay: 6, rx: -19, ry: -2.2, n0: 2, n1: 23 },
		{ label: 'threads you left mid-sentence', t: 0.38, v: 1.67, ax: -58, ay: 18, rx: -19, ry: 2.1, n0: 1, n1: 16 },
		{ label: 'files named final_v3', t: 0.45, v: 1.89, ax: -40, ay: 30, rx: -19, ry: 6.4, n0: 2, n1: 12 },
		{ label: 'invites you have not answered', t: 0.52, v: 2.17, ax: -62, ay: 40, rx: -19, ry: 10.7, n0: 1, n1: 15 },
		{ label: 'notifications you swiped away', t: 0.59, v: 2.56, ax: -36, ay: 50, rx: -19, ry: 15, n0: 4, n1: 88 },
		{ label: 'things you will do at the weekend', t: 0.66, v: 3.13, ax: -64, ay: 24, rx: -19, ry: 19.3, n0: 1, n1: 19 }
	] as const;
	// Same acceleration as the counters, so both halves of the pile grow in
	// one rhythm before collapsing into the inversion.
	$: arriveN = ARRIVALS.map((a) => {
		const inAt = reduced ? 1 : Math.min(1, Math.max(0, (pu - a.t) * a.v));
		return Math.round(a.n0 + inAt ** 3 * (a.n1 - a.n0));
	});


	// ── The thread-map rail: the journey's shape as the playhead ───────────
	const MAP_W = 18;
	const MAP_H = 280;
	const mapXs = STATIONS.map((s) => s.x);
	const MAP_MIN_X = Math.min(...mapXs);
	const MAP_SPAN_X = Math.max(...mapXs) - MAP_MIN_X;
	// The current track is vertically aligned, so a zero lateral span is a
	// valid geometry rather than a divide-by-zero: pin the map to its centre.
	const mapYs = STATIONS.map((s) => s.y);
	const MAP_MIN_Y = Math.min(...mapYs);
	const MAP_SPAN_Y = Math.max(...mapYs) - MAP_MIN_Y;
	const mapPt = (s: Station): Vec => ({
		x: MAP_SPAN_X > 0 ? ((s.x - MAP_MIN_X) / MAP_SPAN_X) * MAP_W : MAP_W / 2,
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
	let mapHeadEl: HTMLElement | null = null;
	let periodEl: HTMLElement | null = null;
	let sceneEls: HTMLElement[] = [];
	let machineEl: HTMLElement | null = null;
	type MoteFieldComponent = (typeof import('./MoteField.svelte'))['default'];
	type MoteFieldHandle = {
		configure: (
			w: number,
			h: number,
			positions: Vec[],
			light: boolean,
			theme: ReturnType<typeof themeThreadPalette>
		) => void;
		tick: (camera: Camera, progress: number, elapsed: number) => void;
	};
	let MoteFieldView: MoteFieldComponent | null = null;
	let moteField: MoteFieldHandle | null = null;
	let moteFieldLoad: Promise<void> | null = null;

	/**
	 * Three.js is visual depth for the film, not a dependency of the opening
	 * proposition. Keep its 560 kB decoded module out of the first-page graph
	 * and fetch it only when the film actually enters the viewport. A failed
	 * optional effect remains a no-op; the narrative, DOM stations and 2D
	 * thread are the durable rendering path.
	 */
	function ensureMoteField(): void {
		if (reduced || MoteFieldView || moteFieldLoad) return;
		moteFieldLoad = import('./MoteField.svelte')
			.then(({ default: component }) => {
				if (!rt.mounted) return;
				MoteFieldView = component;
				requestAnimationFrame(() => {
					moteField?.configure(rt.w, rt.h, rt.positions, rt.onLight, threadTheme);
				});
			})
			.catch(() => {
				// Optional WebGL must never strand the film. Permit a later entry
				// to retry a transient chunk failure without creating a hot loop.
				moteFieldLoad = null;
			});
	}

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
		onLight: boolean;
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
		onLight: true
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

	// ── The closing montage's slot ────────────────────────────────────────
	//
	// The footage that answers "Do what you love." is generated separately
	// and may not exist yet. `lifeOn` is the one answer to "is it there",
	// and it is false until a manifest actually resolves — so the station's
	// frame has no box, no border and no reserved space until there is
	// something to put in it, and the words stand alone as the ending. One
	// request settles it either way.
	let lifeReel: LifeReel | null = null;
	// The DOM half of the composite. The plate no longer carries a device at
	// all — see LifeDevice — so this is the laptop and the phone, placed onto
	// the footage by an authored quad and lit into it.
	let lifeDevice: LifeDevice | null = null;
	let lifeFrameEl: HTMLElement | null = null;
	let lifeOn = false;
	// The payoff is more than twenty screens below the opening. Do not put its
	// 1.6 MB laptop composite or manifest into the cold-load waterfall. Mount
	// it near the end of the reveal, still several viewports before it is seen.
	let lifeMounted = false;
	// How fast the montage's frame comes up, as a multiple of the station's own
	// progress: full picture a fiftieth of the way in. It is deliberately steep,
	// because this station carries nearly half the film's scroll and a ramp
	// expressed as a FRACTION of it is a ramp that grows every time the station
	// does. In scroll distance this is what the old `× 5` bought back when the
	// station weighed 2.8 — roughly half a weight unit, a flick of the wheel.
	const LIFE_IN = 45;

	function onLifeState(on: boolean): void {
		lifeOn = on;
		if (!on) return;
		// The frame takes its box only once the footage exists, so its size
		// is not knowable before this. Measure it on the next frame, after
		// Svelte has applied the class that gives it one.
		requestAnimationFrame(() => configureLife());
	}

	function configureLife(): void {
		const r = lifeFrameEl?.getBoundingClientRect();
		if (!r || r.width === 0) return;
		lifeReel?.configure(r.width, r.height);
	}

	// ── The exodus's payoff: the period's ember DETACHES as the camera
	// leaves the loss and flies into the return, where it STRIKES THE
	// IGNITION — the full stop that ends "Your computer stopped being
	// yours." literally becomes the spark the quiet motif blooms from. The selected
	// theme's accent carries it end to end. (It used to launch from the declaration station's own
	// full stop; that station was folded into the reveal, so the burn moved
	// to the line that now closes the era. Same lineage, same landing.)
	const EMBER_P0 = pAt(SI.drown, 0.86);
	const EMBER_P1 = pAt(SI.tear, 0.26);
	const EMBER_PF = pAt(SI.tear, 0.34);
	let emberFrom: Vec | null = null;
	let emberTo: Vec | null = null;

	/** World position of a point inside station n's DOM, ratio-measured so
	 *  the camera/DoF scale on the section cancels out. */
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
		emberFrom = null;
		emberTo = null;
		fitCanvas();
		moteField?.configure(rt.w, rt.h, rt.positions, rt.onLight, threadTheme);
		configureLife();
	}

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
		moteField?.tick(cam, rt.p, rt.t);
		driveLife();
		placeDepth(cam);
		drawThread(cam);
		placeMapHead(cam);
		placeMachine();
	}

	/** Size and publish the one machine shared by all Act I stations. */
	function placeMachine(): void {
		const st = stageEl;
		if (!st) return;
		// `--era` used to age 0 (1977) → 1 (1995) → 2 (today) across three
		// stations; born and operate are cut, so drown is now Act I's only
		// entry and there is no more history left to morph through — the
		// machine simply IS today's laptop (era 2) from the moment it is on
		// screen. It still runs BACKWARDS during the implosion: `tear`'s
		// first TUNNEL_ERAS of local counts era back down toward 0, so the
		// casing still recedes through its own shape as the frame collapses.
		// This runs inside the rAF, off the same `rt.p` as the camera, so the
		// two cannot disagree.
		const era =
			reduced || scene !== SI.tear
				? 2
				: 2 - 2 * Math.min(1, sceneLocals[SI.tear] / TUNNEL_ERAS);
		// THE MACHINE'S OUTER SIZE IS THE CONSTANT — not its screen.
		//
		// It was the other way round, and that is what produced the jolt
		// between eras: with the screen pinned and the bezel thinning, the
		// BODY shrank by an eighth on every hand-over, so each machine
		// arrived visibly smaller than the one it replaced. Reversed, the
		// object never changes size and the SCREEN grows into the bezel it
		// is reclaiming — which is also the honest version of fifty years of
		// industrial design: the monitor on your desk stayed about the same
		// size, and the picture ate the plastic around it.
		//
		// 0.46 of the frame width puts the body at ~50% of the height, which
		// leaves the room the pull-back exists to reveal.
		const bodyW = Math.min(rt.cw * 0.46, 660);
		// The bezel, as a share of the BODY, and the screen is what is left.
		// The aspect widens with it: a tube is nearly square, a laptop is
		// not, and that is the same fifty years said a second way.
		const e1 = Math.min(1, era);
		const e2 = Math.max(0, era - 1);
		const bez = bodyW * (0.115 - 0.07 * e1 - 0.035 * e2);
		const natW = bodyW - bez * 2;
		const natH = natW / (1.34 + 0.12 * e1 + 0.2 * e2);
		// The laptop deck becomes the widest part of the silhouette in today's
		// era. Both pile columns were previously pinned at +/-19vw, which let
		// their inner edges slip a little under that deck. Publish only the
		// extra distance needed to clear the widest machine part plus the small
		// fixed gutter; the chip flight paths consume it symmetrically below.
		const deckW = natW * (0.52 + 0.12 * e1 + 0.46 * e2);
		const machineHalfW = Math.max(bodyW, deckW) / 2;
		const pileInnerEdge = rt.cw * 0.19;
		const pileClearance = Math.max(
			0,
			machineHalfW - pileInnerEdge + PILE_MACHINE_GUTTER_PX
		);
		st.style.setProperty('--pile-clearance', `${pileClearance.toFixed(2)}px`);
		publishScreen(natW, natH, 0, 0, bez, era, 1);
	}

	/**
	 * The screen's geometry, published in ONE place for the machine, all three
	 * Act I stations and every era's content.
	 *
	 * `--scr-k` is the reason this is JS rather than a CSS `min()`. The eras'
	 * content is laid out at a CONSTANT design size and scaled into whatever
	 * the screen currently is — which is how three pieces of choreography
	 * written for a full page end up inside a 720px screen without any of
	 * their numbers being re-derived, and how they follow the match cut's
	 * changing rect for free. A scale factor has to be a unitless number, and
	 * CSS cannot divide one length by another to get one.
	 */
	function publishScreen(
		w: number,
		h: number,
		dx: number,
		dy: number,
		bez: number,
		era: number,
		settle: number
	): void {
		const st = stageEl;
		if (!st) return;
		st.style.setProperty('--bez', `${bez.toFixed(2)}px`);
		st.style.setProperty('--era', era.toFixed(4));
		st.style.setProperty('--e1', Math.min(1, era).toFixed(4));
		st.style.setProperty('--e2', Math.max(0, era - 1).toFixed(4));
		st.style.setProperty('--scr-w', `${w.toFixed(2)}px`);
		st.style.setProperty('--scr-h', `${h.toFixed(2)}px`);
		const screenScale = Math.max(w / SCREEN_DESIGN_W, 0.001);
		st.style.setProperty('--scr-k', screenScale.toFixed(5));
		// The design box tracks the screen's ASPECT, not just its width. The
		// aspect widens across the eras (a tube is nearly square, a laptop is
		// not), so a fixed-height design box scaled by width letterboxes:
		// measured, the 1995 desktop's teal filled the glass across and left
		// black bands above and below it.
		st.style.setProperty('--scr-dh', `${(SCREEN_DESIGN_W / (w / h)).toFixed(1)}px`);
		st.style.setProperty('--mach-dx', `${dx.toFixed(2)}px`);
		st.style.setProperty('--mach-dy', `${dy.toFixed(2)}px`);
		// THE MACHINE ONLY DROPS BELOW CENTRE ONCE IT HAS ARRIVED. The offset
		// exists to leave room for the era caption, and there is no caption
		// during the match cut — but there IS a rectangle that has to land on
		// the footage's screen exactly. Applied at the cut it put the DOM
		// machine 63px (7vh) below the frame it was matching, which is the
		// size-and-place mismatch at the hand-over. It ramps in with the
		// pull-back instead, so by the time the caption appears it is there.
		st.style.setProperty('--mach-oy', `${(rt.ch * 0.07 * settle).toFixed(2)}px`);
	}

	/**
	 * The width every era's screen content is authored against.
	 *
	 * It is a ZOOM CONTROL as much as a layout number: the screen is a fixed
	 * rect, so a narrower design box means the same content is scaled up
	 * inside it. At 1100 the windows and the desktops read as a machine seen
	 * from across a room; 820 puts them at the size you would actually sit
	 * in front of.
	 */
	const SCREEN_DESIGN_W = 820;

	/**
	 * The closing montage. Its own station IS its timeline — the frame
	 * sequence plays across the life station's local, which is what makes
	 * the montage scrub like the rest of the film rather than autoplay past
	 * a reader who stopped. The layer fades up on the leg in, so it arrives
	 * with the camera instead of switching on at the boundary.
	 */
	function driveLife(): void {
		const a = BOUNDS[SI.life];
		const b = BOUNDS[SI.life + 1];
		const u = (rt.p - a) / (b - a);
		if (!lifeMounted) {
			if (u > -0.12) {
				lifeMounted = true;
				requestAnimationFrame(() => {
					configureLife();
					lifeReel?.prime();
				});
			}
			return;
		}
		if (!lifeOn) {
			// One manifest request, asked near the end of the reveal; a 404
			// settles the words-only ending for good.
			lifeReel?.prime();
			return;
		}
		// THE FRAME'S OWN ARRIVAL, and it is the one that counts. The device
		// lives INSIDE this frame, so what a visitor can actually see is the
		// frame's opacity TIMES the plate's — and the frame was fading in on a
		// `--local * 5` ramp in CSS, tuned when this station weighed 2.8. At
		// 25.2 that same ramp stretched to about five weight units of scroll, so
		// the montage crawled into view while the laptop, gated on the plate
		// alone, had already ticked three steps off behind a frame at two
		// percent. It is driven here now, on the station's own clock, because
		// two clocks for one fade is what let them disagree in the first place.
		const frameIn = clamp01(u * LIFE_IN);
		if (lifeFrameEl) lifeFrameEl.style.opacity = String(frameIn);
		const lifeAlpha = clamp01((u + 0.15) / 0.17);
		lifeReel?.tick(u, lifeAlpha);
		// AFTER the reel, and from the reel's own geometry: the device has to
		// be told where the plate actually landed, because the plate is
		// contain-fitted and that fit is the reel's private arithmetic. One
		// clock, one source of truth for where the picture is.
		// AND THE PLATE'S OWN FADE. The device was mounted at full opacity the
		// moment the reel had geometry, so the screen was already working over
		// a picture that had barely started coming up — a laptop hanging in
		// empty paper, ticking tasks off, before the room it stands in exists.
		// It rides the same alpha the footage does.
		// Two numbers, deliberately. The device RENDERS on the plate's alpha, so
		// it never double-dims inside a frame that is already fading; it WAITS
		// on the composite, which is the only honest answer to "can this be
		// seen yet".
		lifeDevice?.place(lifeReel?.geom() ?? null, lifeAlpha, frameIn * lifeAlpha);
	}

	function placeDepth(cam: Camera): void {
		// Depth of field: stations are flown INTO, not presented — each one
		// scales, sharpens and brightens as the camera closes on it, and
		// recedes into soft blur as the camera leaves. Compositor-friendly:
		// transform + opacity always; blur only while actually soft.
		//
		// THE IMPLOSION HAPPENS IN PLACE. The drowning and the implosion stand
		// at the same world point — there is no leg between them, because the
		// thing collapsing is the frame the visitor is already looking at, and
		// a camera that flew somewhere else first would be showing them some
		// other screen fall in. Proximity alone therefore leaves both stations
		// fully lit and stacked, so the drowning is collapsed explicitly
		// across the implosion's own opening.
		//
		// The pile-up does not fade out and it is not torn open. It falls into
		// the single point it has been converging on all beat — the ignition
		// point, which is where every arrival has been landing — and Magican
		// comes out of that point. One thing becoming another, rather than one
		// thing being removed so another can be shown.
		const rs = resolveScene(BOUNDS, rt.p);
		const imp =
			rs.scene > SI.tear ? 1 : rs.scene === SI.tear ? clamp01(rs.local / IMPLODE_SPAN) : 0;
		// THE ACT I STACK. drown and tear stand at ONE world point — born and
		// operate, which used to share it, are cut — because Act I is one
		// place where a machine ages rather than a tour of subjects.
		// Proximity therefore cannot tell them apart, so inside the stack the
		// OWNING SCENE decides, not the distance.
		//
		// Neighbours cross-fade at the boundary so one beat of the machine
		// hands over to the next rather than cutting. Kept SHORT for now:
		// until each era's content lives inside the screen, a long dissolve
		// shows the next station's header floating over the current machine.
		// Widen this once the eras are nested.
		const FADE = 0.05;
		const stackOpacity = (i: number): number => {
			if (i < SI.drown || i > SI.tear) return -1; // not in the stack
			if (i === rs.scene) return 1;
			// the station just ahead fades IN across this one's tail
			if (i === rs.scene + 1) return clamp01((rs.local - (1 - FADE)) / FADE);
			// the station just behind fades OUT across this one's head
			if (i === rs.scene - 1) return clamp01(1 - rs.local / FADE);
			return 0;
		};
		// ease-IN: the collapse starts as a drift and finishes as a fall. An
		// eased-OUT implosion reads as the frame being politely put away.
		const impE = imp * imp * imp;
		for (let i = 0; i < STATIONS.length; i++) {
			const el = sceneEls[i];
			if (!el) continue;
			const pos = rt.positions[i];
			const dx = (pos.x - cam.x) / (rt.w * 0.85);
			const dy = (pos.y - cam.y) / (rt.h * 0.9);
			const d = Math.hypot(dx, dy);
			const prox = Math.max(0, 1 - d);
			const e = prox * prox * (3 - 2 * prox);
			// THE SCENE THAT RECEDES IS THE ONE THE VISITOR WAS JUST LOOKING
			// AT: the drowning. There is no intervening summary card; the pile-up
			// itself falls into the point and becomes the inversion.
			// `>= SI.tear`, not `impE > 0`: at the implosion's very first frames
			// the fall has not started, so an `impE`-gated branch handed the
			// drowning back to the stack — which blacks any screen that is not
			// the owning scene's, and the picture flickered out and back.
			if (i === SI.drown && rs.scene >= SI.tear) {
				// RECEDING, not collapsing inward from the edges. The frame
				// flies AWAY from the viewer toward the ignition point — the
				// same (−20px, −12vh) offset `ignitionPoint` uses, so it
				// vanishes into the exact place the light comes back out of.
				// THE SCREEN KEEPS SHOWING WHAT IT WAS SHOWING, and simply
				// goes with it. An earlier cut took the picture out across the
				// first third of the fall, on the argument that the case is
				// rewinding to 1977 underneath and a modern screen inside a
				// beige tube reads as a mistake — but a machine that blacks
				// out and THEN recedes has two moves in it, and the black
				// frame is the one you notice. Fading on the recession's own
				// curve, the picture is still there while there is anything
				// left to see and gone by the time there is not.
				el.style.opacity = (1 - impE).toFixed(3);
				// The screen content is gated on the OWNING scene, so without
				// this it is blacked by CSS no matter what the scene's own
				// opacity says.
				el.classList.add('mt-receding');
				el.style.transform = recedeTransform(impE, '-50%, -50%');
				el.style.filter = 'none';
				continue;
			}
			el.classList.remove('mt-receding');
			const stacked = stackOpacity(i);
			if (stacked >= 0) {
				// Inside the stack there is no depth to model: the camera is
				// not moving, so a proximity blur would be blurring a station
				// the camera is sitting exactly on.
				el.style.opacity = stacked.toFixed(3);
				el.style.transform = 'translate(-50%, -50%)';
				el.style.filter = 'none';
				continue;
			}
			el.style.opacity = (0.3 + 0.7 * e).toFixed(3);
			el.style.transform = `translate(-50%, -50%) scale(${(0.9 + 0.1 * e).toFixed(4)})`;
			el.style.filter = e > 0.96 ? 'none' : `blur(${((1 - e) * 3.2).toFixed(2)}px)`;
		}
		// The machine goes with the picture it is showing, on the identical
		// curve — anything else and the casing and its own screen separate
		// on the way out.
		if (machineEl) {
			machineEl.style.transform = impE > 0 ? recedeTransform(impE, '0, 0') : '';
			machineEl.style.opacity = impE > 0 ? (1 - clamp01((impE - 0.9) / 0.1)).toFixed(3) : '';
		}
	}

	/**
	 * THE RECESSION. One curve, used by everything that falls into the
	 * vanishing point, so the machine and the picture on its screen cannot
	 * drift apart on the way out.
	 *
	 * `q` is already eased-IN by the caller: the fall starts as a drift and
	 * finishes fast, which is what makes it read as distance rather than as
	 * a shrinking box.
	 */
	function recedeTransform(q: number, base: string): string {
		const s = Math.max(0.001, 1 - 0.999 * q);
		const tx = (-20 * q).toFixed(1);
		// EVERYTHING FALLS INTO ONE POINT, and that point is the thread's
		// ignition — (−20px, −12vh) from the station's centre — because it is
		// where the motes converge, where the flare opens and where the
		// wordmark comes back out. The machine and its picture both sit
		// `--mach-oy` BELOW that centre (the offset that leaves room for the
		// era caption), so they have that much further to travel. Left out,
		// the frame receded to a vanishing point 7vh under the one the light
		// was arriving at, and the beat had two centres.
		const ty = ((-0.12 * rt.h - rt.ch * 0.07) * q).toFixed(1);
		return `translate(${base}) translate(${tx}px, ${ty}px) scale(${s.toFixed(4)})`;
	}


	/** How much of the implosion station's local the collapse takes. The
	 *  flare and the emergence are timed against this by hand in the CSS;
	 *  if it moves, they move with it. */
	const IMPLODE_SPAN = 0.3;
	/** How far into the implosion the machine finishes running back through
	 *  its own three eras. The recession used to hand over to a drawn stone
	 *  tool and a flame after this point; both are gone. They were carrying
	 *  the lineage in a register nothing else on the page uses — two flat
	 *  silhouettes among photographed footage and real interfaces — and the
	 *  machine rewinding through fifty years already says the thing they
	 *  were there to say. */
	const TUNNEL_ERAS = 0.24;

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

	function drawEmber(cam: Camera): void {
		// The period → ignition handoff. Outside its window the anchor cache
		// is dropped, so every pass re-measures against the live layout; the
		// target is the ignition point itself — the ember STRIKES the spark
		// the motif blooms from.
		const ctx = rt.ctx;
		if (!ctx) return;
		if (rt.p <= EMBER_P0 || rt.p >= EMBER_PF) {
			emberFrom = null;
			emberTo = null;
			return;
		}
		if (!emberFrom || !emberTo) {
			if (!periodEl) return;
			emberFrom = anchorWorld(periodEl, SI.drown, 0.5, 0.6);
			emberTo = ignitionPoint(rt.positions, rt.h);
			if (!emberFrom || !emberTo) return;
		}
		const S = emberFrom;
		const E = emberTo;
		const t = clamp01((rt.p - EMBER_P0) / (EMBER_P1 - EMBER_P0));
		const e = t * t * (3 - 2 * t);
		// A gentle outward bow so the fall reads as thrown, not teleported.
		const C = { x: (S.x + E.x) / 2 - 0.09 * rt.w, y: (S.y + E.y) / 2 + 0.03 * rt.h };
		const bez = (u: number): Vec => {
			const v = 1 - u;
			return {
				x: v * v * S.x + 2 * v * u * C.x + u * u * E.x,
				y: v * v * S.y + 2 * v * u * C.y + u * u * E.y
			};
		};
		// Theme accent end to end — this ember IS the coming thread's first light.
		const cr = Math.round(threadTheme.primary.r * 255);
		const cg = Math.round(threadTheme.primary.g * 255);
		const cb = Math.round(threadTheme.primary.b * 255);
		const flash = rt.p > EMBER_P1 ? clamp01((rt.p - EMBER_P1) / (EMBER_PF - EMBER_P1)) : 0;
		const fade = 1 - flash;
		// Trail: sparks strung behind the head along the same fall.
		for (let i = 7; i >= 1; i--) {
			const u = e - i * 0.045;
			if (u <= 0) continue;
			const q = project(cam, bez(u));
			ctx.fillStyle = `rgba(${cr}, ${cg}, ${cb}, ${(0.34 * (1 - i / 8) * fade).toFixed(3)})`;
			ctx.beginPath();
			ctx.arc(q.x, q.y, 3.4 - i * 0.34, 0, Math.PI * 2);
			ctx.fill();
		}
		// The head: swollen at release, COLLAPSED to cursor-size at landing.
		const hp = project(cam, bez(e));
		const r = (8 - 5.2 * e) * cam.z;
		const br = (34 - 16 * e) * cam.z;
		const bloom = ctx.createRadialGradient(hp.x, hp.y, 0, hp.x, hp.y, br);
		bloom.addColorStop(0, `rgba(${cr}, ${cg}, ${cb}, ${(0.55 * fade).toFixed(3)})`);
		bloom.addColorStop(1, `rgba(${cr}, ${cg}, ${cb}, 0)`);
		ctx.fillStyle = bloom;
		ctx.beginPath();
		ctx.arc(hp.x, hp.y, br, 0, Math.PI * 2);
		ctx.fill();
		ctx.fillStyle = `rgba(${cr}, ${cg}, ${cb}, ${(0.95 * fade).toFixed(3)})`;
		ctx.beginPath();
		ctx.arc(hp.x, hp.y, Math.max(1.4, r), 0, Math.PI * 2);
		ctx.fill();
		// Landing: one theme-colored shock ring out of the strike point — the DOM
		// spark blooms in the same instant, and the thread comes fully alive.
		if (flash > 0) {
			const ep = project(cam, E);
			ctx.strokeStyle = rgba(threadTheme.primary, 0.6 * (1 - flash));
			ctx.lineWidth = 1.6;
			ctx.beginPath();
			ctx.arc(ep.x, ep.y, 4 + flash * 46 * cam.z, 0, Math.PI * 2);
			ctx.stroke();
		}
	}

	function drawThread(cam: Camera): void {
		const ctx = rt.ctx;
		const path = rt.thread;
		if (!ctx || !path) return;
		ctx.clearRect(0, 0, rt.cw, rt.ch);
		rt.uctx?.clearRect(0, 0, rt.cw, rt.ch);
		drawSpecks(cam);
		drawEmber(cam);
		const m = path.marks;
		const motifIn = m.motifIn ?? m.ignite;
		const visible = clamp01((rt.p - motifIn) / 0.012);
		const revealBloom = clamp01((rt.p - m.ignite) / 0.02);
		if (visible <= 0) {
			return;
		}

		// Keep a consistent, generous BODY in physical pixels. A fixed timeline
		// span produced a long snake in short chapters and a tiny tail in the
		// 25.2-weight payoff — exactly the tadpole/sperm silhouette this motif
		// must never make. Uniform arc-length samples also remove scroll-speed
		// kinks from the canvas passes below.
		const targetLength = motifTrailLengthPx(rt.cw);
		const trail = sampleMotifTrail(
			motifIn,
			rt.p,
			targetLength,
			(pp) => project(cam, sampleThread(path.main, pp))
		);
		const K = trail.length - 1;
		if (K < 1) return;
		const pts: Vec[] = trail.map(({ x, y }) => ({ x, y }));
		// Per-sample depth: 0 = in front (the usual grammar), 1 = diving
		// behind a station object — those segments render on the under-
		// canvas beneath the world instead.
		const bs: number[] = [];
		for (let i = 0; i <= K; i++) {
			const g = i / K;
			const before = pts[Math.max(0, i - 1)];
			const after = pts[Math.min(K, i + 1)];
			const dx = after.x - before.x;
			const dy = after.y - before.y;
			const length = Math.hypot(dx, dy) || 1;
			// One coherent wave along the path normal. The ends remain anchored;
			// time changes only the phase, so the body breathes instead of jittering.
			// The authored Catmull-Rom curve provides the large gesture; this slow,
			// coherent travelling ripple supplies organic life. It is visible enough
			// to read as a living ribbon, while the anchored envelope prevents either
			// end from whipping or detaching from the authored path.
			const offset = motifWaveOffset(g, rt.t, revealBloom);
			pts[i].x += (-dy / length) * offset;
			pts[i].y += (dx / length) * offset;
			bs.push(rt.uctx ? behindAt(path.behind, trail[i].progress) : 0);
		}

		const halo = threadTheme.primary;
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
			// Each pass is split into tapered-alpha segments. Round-capping every
			// one doubles their junction alpha and makes the ribbon look beaded;
			// butt caps meet cleanly under the wider glow passes.
			c.lineCap = 'butt';
			c.stroke();
		};
		const pass = (base: typeof halo, width: number, alpha: number, tint: number): void => {
			const col = mixStops(base, core, tint);
			for (let i = 1; i <= K; i++) {
				const g = i / K;
				// Keep most of the body present and taper only its last quarter. The
				// old g^1.6 falloff erased the first half and recreated a short tail
				// even after the geometric trail itself had been lengthened.
				const a = motifBodyAlpha(g) * alpha * visible;
				if (a < 0.01) continue;
				// Split each segment across the two canvases by behindness —
				// the alpha feather at span edges makes the dive read smooth.
				const b = bs[i];
				if (b < 0.99) strokeSeg(ctx, i, width, rgba(col, a * (1 - b)));
				if (rt.uctx && b > 0.01) strokeSeg(rt.uctx, i, width, rgba(col, a * b));
			}
		};
		const strokes = motifStrokeRecipe(rt.onLight, revealBloom);
		pass(glowCompanion, strokes.outer.width, strokes.outer.alpha, strokes.outer.tint);
		pass(glowHalo, strokes.glow.width, strokes.glow.alpha, strokes.glow.tint);
		pass(halo, strokes.filament.width, strokes.filament.alpha, strokes.filament.tint);
		pass(glowHalo, strokes.core.width, strokes.core.alpha, strokes.core.tint);

		// The head is only the ribbon's leading light, never a separate orb. It
		// obeys the same depth as its segment: while diving it renders on
		// the under-canvas, split-alpha through the feather.
		const head = pts[K];
		const hr = visible * motifHeadRadius(revealBloom);
		// Greeting becomes the leading edge at the section seam. Retain this
		// canvas's body as the departing tail, but dissolve its head across the
		// final pixel-scale approach so two bright leaders never appear on one ribbon.
		const handoff = clamp01(
			(rt.p - pAt(SI.life, 0.9999)) / (pAt(SI.life, 1) - pAt(SI.life, 0.9999))
		);
		const headPresence = 1 - handoff;
		const bHead = bs[K];
		const drawHead = (c: CanvasRenderingContext2D, k: number): void => {
			const headBloom = c.createRadialGradient(head.x, head.y, 0, head.x, head.y, hr);
			headBloom.addColorStop(0, rgba(glowHalo, 0.88 * k));
			headBloom.addColorStop(0.4, rgba(glowHalo, 0.26 * k));
			headBloom.addColorStop(0.76, rgba(glowCompanion, 0.06 * k));
			headBloom.addColorStop(1, rgba(halo, 0));
			c.fillStyle = headBloom;
			c.beginPath();
			c.arc(head.x, head.y, hr, 0, Math.PI * 2);
			c.fill();
			// The face: white-hot star on deep ground, ink star on paper.
			c.fillStyle = rt.onLight
				? rgba(mixStops(glowHalo, threadTheme.ink, 0.28), 0.98 * visible * k)
				: rgba(threadTheme.light, 0.95 * visible * k);
			c.beginPath();
			c.arc(head.x, head.y, 1.25, 0, Math.PI * 2);
			c.fill();
		};
		if (hr > 1 && headPresence > 0) {
			if (bHead < 0.99) drawHead(ctx, (1 - bHead) * headPresence);
			if (rt.uctx && bHead > 0.01) drawHead(rt.uctx, bHead * headPresence);
		}

		// Magic travels THROUGH the body. Three tiny glints advance along the
		// arc at different phases; none orbit the head, so they read as flow
		// rather than a face with satellites.
		for (let i = 0; i < 3; i++) {
			const phase = (rt.t * 0.105 + i / 3) % 1;
			const g = 0.12 + phase * 0.76;
			const at = Math.min(K, Math.max(0, Math.round(g * K)));
			const point = pts[at];
			const alpha = Math.sin(Math.PI * phase) * visible * (0.38 + 0.14 * revealBloom);
			const b = bs[at];
			const drawGlint = (c: CanvasRenderingContext2D, k: number): void => {
				c.fillStyle = rgba(rt.onLight ? mixStops(glowHalo, core, 0.24) : threadTheme.light, alpha * k);
				c.beginPath();
				c.arc(point.x, point.y, 1.05 + 0.35 * revealBloom, 0, Math.PI * 2);
				c.fill();
			};
			if (b < 0.99) drawGlint(ctx, 1 - b);
			if (rt.uctx && b > 0.01) drawGlint(rt.uctx, b);
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

	}

	function placeMapHead(cam: Camera): void {
		if (!mapHeadEl) return;
		const vwPx = (rt.w / 100) * rt.xf;
		const vhPx = (rt.h / 100) * rt.yf;
		const xvw = vwPx > 0 ? cam.x / vwPx : 0;
		const yvh = vhPx > 0 ? cam.y / vhPx : 0;
		const mx = MAP_SPAN_X > 0 ? ((xvw - MAP_MIN_X) / MAP_SPAN_X) * MAP_W : MAP_W / 2;
		const my = ((yvh - MAP_MIN_Y) / MAP_SPAN_Y) * MAP_H;
		mapHeadEl.style.transform = `translate3d(${mx.toFixed(1)}px, ${my.toFixed(1)}px, 0) translate(-50%, -50%)`;
	}

	let io: IntersectionObserver | null = null;
	let themeObs: MutationObserver | null = null;
	let onResize: (() => void) | null = null;

	onMount(() => {
		rt.mounted = true;
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
		if (trackEl) {
			io = new IntersectionObserver(
				(entries) => {
					rt.visible = entries[0]?.isIntersecting ?? false;
					if (rt.visible) ensureMoteField();
					syncLoop();
				},
				// A zero-width edge touch at the end of the hero is not a visit to
				// the film. Pull the bottom edge in by one pixel so Three.js starts
				// only after the visitor has actually scrolled into this section.
				{ threshold: 0, rootMargin: '0px 0px -1px 0px' }
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
		}
		io?.disconnect();
		themeObs?.disconnect();
	});
</script>

<div
	id="movie-track"
	class="mt-track"
	class:mt-onlight={groundLight}
	class:static={reduced}
	style={reduced ? '' : `height:${TRACK_SVH}svh`}
	use:scrub.track
	bind:this={trackEl}
>
	<div class="mt-stage" class:static={reduced} bind:this={stageEl}>
		{#if !reduced}
			<!-- The WebGL depth field: parallax dust the camera flies through
			     and Act I's pile-up / pull-back choreography. Its Three.js chunk
			     is loaded only once this below-the-fold film reaches the viewport. -->
			{#if MoteFieldView}
				<svelte:component this={MoteFieldView} bind:this={moteField} />
			{/if}
			<!-- The thread's under-canvas: behind-flagged segments render here,
			     BENEATH the stations, so the thread can dive under objects. -->
			<canvas class="mt-thread mt-thread-under" bind:this={underEl} aria-hidden="true"></canvas>
		{/if}
		<div class="mt-camera" class:static={reduced} bind:this={cameraEl}>
			<div class="mt-world" class:static={reduced} bind:this={worldEl}>
				{#if !reduced}
					<!-- The continuous field: hue mapped to world position. The
					     operate era gets none — grey office light is the point. -->
					{#each STATIONS as station (station.id)}
						{#if station.era !== 'grey'}
							<div
								class="mt-glow"
								style="left:{station.x * xf}vw;top:{station.y * yf}vh;{glowStyle(
									station,
									phosphor
								)}"
								aria-hidden="true"
							></div>
						{/if}
					{/each}
				{/if}

				<!-- THE MACHINE. One object, standing at Act I's single world
				     point — today's laptop, born and operate having been cut,
				     so there is no more history for it to age through outside
				     the implosion's own backward run. It is rendered ONCE,
				     outside the station loop, because it is not a station's
				     prop — it is the thing drown and tear are both about, and
				     giving each beat its own copy is what made them feel like
				     separate subjects.

				     It paints UNDER the scenes, so each era's screen content
				     (`.mt-inscreen`) lands inside its glass. -->
				{#if !reduced}
					<div
						class="mt-machine"
						bind:this={machineEl}
						class:mt-mach-on={scene >= SI.drown && scene <= SI.tear}
						style="left:{STATIONS[SI.drown].x * xf}vw;top:{STATIONS[SI.drown].y * yf}vh"
						aria-hidden="true"
					>
						<div class="mt-mach-body">
							<div class="mt-mach-well"></div>
							<div class="mt-mach-glass"></div>
							<div class="mt-mach-vents"><i></i><i></i><i></i></div>
						</div>
						<div class="mt-mach-neck"></div>
						<div class="mt-mach-deck"></div>
					</div>
				{/if}

				{#each STATIONS as station, n (station.id)}
					<section
						class="mt-scene mt-s-{station.id}"
						class:on={reduced || scene === n}
						class:static={reduced}
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
						{#if station.clock}
							<header class="mt-chapter-head">
								<span class="mt-kicker" aria-hidden="true">
									<span class="mt-clock">{station.clock}</span>
									<span class="mt-kicker-rule"></span>
									<span class="mt-chapname">{station.label ?? station.id}</span>
								</span>
								<h3 class="mt-title">
									{#each station.title.split(' ') as word, wi}<span
											class="mt-w"
											style="--wi:{wi}">{word}</span
										>{#if wi < station.title.split(' ').length - 1}{' '}{/if}{/each}
								</h3>
							</header>
						{/if}

						{#if station.id === 'drown'}
							<!-- THE DROWNING — the old exodus's machinery, aimed at
							     the right grievance. What multiplies here is not what
							     they learn about you, it is WHAT IS LEFT FOR YOU: every
							     ordinary act on the device leaves something behind that
							     still needs a person, and it arrives faster than any
							     person clears it.

							     TWO THINGS INVERTED from the exodus, and both matter.
							     (1) DIRECTION. Nothing streams away to an edge any more;
							     everything travels INWARD and lands, because the point
							     is not that something was taken from you, it is that
							     something was left with you. (2) VILLAIN — there is
							     none. The old station had an antagonist and a cold
							     platform blue to paint it in; this one is grey and reads
							     as fatigue, because nobody did this to you: the same
							     capability that made the machine powerful is what buries
							     you. An invented enemy would also hand the trust section
							     an argument it should never have had to make. -->
							<!-- The two stacks START out at the edges and CLOSE IN as
							     the scroll advances — the further the day goes, the less
							     room is left. `--spread` is that closing, not a spread;
							     the name is kept because the whole layer's geometry is
							     keyed to it. -->
							<div
								class="mt-exodus"
								style="--spread:{reduced ? 1 : seg(pu, 0.02, 0.72).toFixed(4)}"
							>
								<!-- The browser is what is ON today's screen, so it plays in
								     the machine's glass like the two eras before it. The
								     COUNTERS and the leavings stay outside it, because they
								     are not on the screen — they are what the screen has left
								     lying around you. -->
								<div class="mt-you mt-inscreen mt-osmac" aria-hidden="true">
									<!-- Today's desktop: a menu bar, a light wallpaper and a
									     dock. The browser is a window ON a machine, not the
									     whole machine — and the dock is where the apps that
									     are burying you actually live. -->
									<span class="mt-osmac-menu" aria-hidden="true">
										<b></b><i>File</i><i>Edit</i><i>View</i><i>Window</i>
										<u class="mt-mono">Tue 22:41</u>
									</span>
									<span class="mt-osmac-dock" aria-hidden="true">
										{#each COUNTS as c (c.app)}<i></i>{/each}<i></i><i></i>
									</span>
									<span class="mt-you-glow"></span>
									<!-- ONE WINDOW, and the strip along the top of it. Each tab is
								     a piece of work that was opened and not closed; they arrive
								     as the scroll advances and they get NARROWER as they
								     multiply, because that is what a real window does and it is
								     the single most recognisable picture of this problem there
								     is. Under it, the three counters everybody has, climbing. -->
								<div class="mt-brow">
									<div class="mt-tabs">
										<!-- The traffic lights. Three dots is the whole cost of
										     turning a rounded rectangle into A WINDOW ON A MAC —
										     and the green one is quietly the joke, because this
										     window has not been closed in weeks and will not be
										     today either. They sit INSIDE the strip rather than in
										     a title bar of their own: this is a browser in tab-bar
										     mode, which is what everyone's actually looks like. -->
										<span class="mt-lights" aria-hidden="true">
											<i class="mt-light mt-light-r"></i>
											<i class="mt-light mt-light-y"></i>
											<i class="mt-light mt-light-g"></i>
										</span>
										{#each TABS as tab, ti (tab)}
											<span class="mt-tab" class:on={reduced || tabsOn > ti}>{tab}</span>
										{/each}
									</div>
									<div class="mt-brow-body">
										{#if reduced || tabView === 'inbox'}
											<!-- A Gmail-shaped inbox with the branding taken out:
											     checkbox, star, bold sender, subject then a dimmed
											     snippet on the same line, time on the right. The
											     shape is what everyone recognises; the logo is not
											     ours to draw. -->
											<!-- THE FURNITURE IS WHAT NAMES IT. Rows on a rectangle
											     could be anything; a mail client is a folder rail, a
											     toolbar and a list, and the eye reads that three-part
											     shape before it reads a single subject line. -->
											<div class="mt-mailapp">
											<div class="mt-mrail">
												<span class="mt-mcompose">Compose</span>
												{#each [['Inbox', '47'], ['Starred', ''], ['Snoozed', '4'], ['Sent', ''], ['Drafts', '11'], ['Spam', '2'], ['Archive', '']] as [f, n], fi (f)}
													<span class="mt-mfold" class:on={fi === 0}>{f}<i>{n}</i></span>
												{/each}
											</div>
											<div class="mt-mmain">
											<div class="mt-mtop">
												<span class="mt-msearch mt-mono">search mail</span>
												<span class="mt-mtools" aria-hidden="true"><i></i><i></i><i></i></span>
											</div>
											<div class="mt-mail">
												{#each MAIL_ROWS as [who, subj, snip, when] (subj)}
													<div class="mt-mrow">
														<span class="mt-mbox"></span>
														<span class="mt-mstar">☆</span>
														<span class="mt-mwho">{who}</span>
														<span class="mt-msubj"
															>{subj}<i class="mt-msnip"> — {snip}…</i></span
														>
														<span class="mt-mwhen mt-mono">{when}</span>
													</div>
												{/each}
											</div>
											</div>
											</div>
										{/if}
										{#if reduced || tabView === 'chat'}
											<!-- A LIST OF NAMES AND LINES IS NOT AN APP. It could have
											     been mail, a task list, a CRM — nobody could tell, and
											     a screen nobody can name does no work in a montage
											     about too many apps. It is a MESSAGING app: a
											     conversation rail on the left with unread counts, and
											     an open thread on the right with bubbles that lean the
											     way the sender does. That silhouette is unmistakable at
											     a glance, which is the whole requirement. -->
											<div class="mt-msg">
												<div class="mt-msg-rail">
													{#each [['Design', '2'], ['Ops', '5'], ['Priya', '1'], ['Family', ''], ['Vendors', '3'], ['Standup', '']] as [who, n], ci (who)}
														<span class="mt-msg-conv" class:on={ci === 2}>
															<i class="mt-cava"></i>{who}{#if n}<b>{n}</b>{/if}
														</span>
													{/each}
												</div>
												<div class="mt-msg-thread">
													<div class="mt-msg-head"><span class="mt-cava"></span>Priya<i class="mt-mono">online</i></div>
													{#each [['in', 'did you see my message from Tuesday'], ['in', 'the redline needs approving before the call'], ['out', 'sorry — looking now'], ['in', 'also the vendor is asking again 🙃']] as [dir, line], mi (mi)}
														<span class="mt-msg-b mt-msg-{dir}">{line}</span>
													{/each}
													<div class="mt-msg-in mt-mono">Type a message…</div>
												</div>
											</div>
										{/if}
										{#if reduced || tabView === 'form'}
											<div class="mt-form-app">
											<div class="mt-ah mt-ah-form"><span class="mt-ah-mark">✦</span><span class="mt-ah-t">Expenses · Claim #4471</span><span class="mt-ah-r mt-mono">Save draft   Submit</span></div>
											<div class="mt-form">
												<div class="mt-frow"><span>Cost centre</span><i></i></div>
												<div class="mt-frow"><span>Receipt #</span><i class="mt-fempty"></i></div>
												<div class="mt-frow"><span>Justification</span><i class="mt-fempty"></i></div>
												<span class="mt-fnote mt-mono">2 required fields left · draft saved</span>
											</div>
											</div>
										{/if}
										{#if reduced || tabView === 'pay'}
											<div class="mt-pay-app">
											<div class="mt-ah mt-ah-pay"><span class="mt-ah-mark">◈</span><span class="mt-ah-t">Accounts · ••4471</span><span class="mt-ah-r mt-mono">Pay   Transfer   Statements</span></div>
											<div class="mt-pay">
												<!-- Three rows is a receipt. An account you are behind on
												     is a WALL of them, and the wall is the point. -->
												{#each [['Insurance renewal', '₹18,400', 'overdue'], ['Card — statement', '₹42,180', 'due today'], ['Vendor · Sharma & Co', '₹84,000', 'due in 2d'], ['Electricity — Nov', '₹3,240', 'due in 3d'], ['Pune Logistics', '₹12,450', 'due in 4d'], ['Broadband — annual', '₹9,600', 'autopay'], ['Society maintenance', '₹7,800', 'overdue'], ['GST — Q3', '₹1,14,200', 'due in 6d'], ['Vendor · Kale & Sons', '₹22,900', 'in review'], ['Card — minimum due', '₹4,220', 'due today']] as [what, amt, when] (what)}
													<div class="mt-prow">
														<span class="mt-pwhat">{what}</span>
														<span class="mt-pamt mt-mono">{amt}</span>
														<span class="mt-pwhen mt-mono">{when}</span>
													</div>
												{/each}
												<!-- A bills screen always ends in things to press, and
												     the buttons are half of why it reads as a REAL one
												     — a list you cannot act on is a statement, and a
												     statement is not what is pulling at you. -->
											</div>
											<!-- OUTSIDE the list, not the last row of it. Inside, the
											     ten bills pushed the buttons past the pane's clip and
											     they were never on screen — a footer has to be a
											     sibling of the thing it is a footer to. -->
											<div class="mt-pacts">
												<span class="mt-pact">Review all</span>
												<span class="mt-pact">Download</span>
												<span class="mt-pact">Save draft</span>
												<span class="mt-pact mt-pact-go">Pay selected</span>
											</div>
										</div>
										{/if}
										{#if reduced || tabView === 'board'}
											<!-- A KANBAN BOARD, and the column that matters is the
											     first one. Every card in it is assigned to the same
											     person; "Done" has one card in it and it is old. -->
											<div class="mt-board-app">
											<div class="mt-ah mt-ah-board"><span class="mt-ah-mark">▦</span><span class="mt-ah-t">Sprint 34 · Platform</span><span class="mt-ah-r mt-mono">Board  ▾   Filter   ⋯</span></div>
											<div class="mt-board">
												{#each BOARD_COLS as { col, n, cards } (col)}
													<div class="mt-bcol">
														<span class="mt-bcolh mt-mono">{col} <i>{n}</i></span>
														{#each cards as card (card)}
															<span class="mt-bcard">{card}</span>
														{/each}
													</div>
												{/each}
											</div>
											</div>
										{/if}
										{#if reduced || tabView === 'sheet'}
											<!-- A SPREADSHEET, mid-reconciliation: a column of
											     numbers that do not add up, one cell still being
											     edited, and an error nobody has looked at. -->
											<div class="mt-sheet-app">
											<div class="mt-ah mt-ah-sheet"><span class="mt-ah-mark">▤</span><span class="mt-ah-t">Q3_actuals.xlsx</span><span class="mt-ah-r mt-mono">File  Edit  View  Insert</span></div>
											<div class="mt-sheet">
												<div class="mt-srow mt-shead mt-mono">
													<span></span><span>A</span><span>B</span><span>C</span><span>D</span>
												</div>
												{#each [['1', 'Travel', '18,400', '18,400', 'ok'], ['2', 'Vendors', '84,000', '81,220', '#REF!'], ['3', 'Payroll', '4,12,900', '4,12,900', 'ok'], ['4', 'Misc', '', '2,780', ''], ['5', 'Total', '5,15,300', '5,15,300', '≠']] as [n, a, b, c, d] (n)}
													<div class="mt-srow mt-mono" class:mt-serr={d === '#REF!' || d === '≠'}>
														<span class="mt-sn">{n}</span>
														<span>{a}</span>
														<span>{b}</span>
														<span>{c}</span>
														<span>{d}</span>
													</div>
												{/each}
												<span class="mt-scell" aria-hidden="true"></span>
											</div>
											</div>
										{/if}
										{#if reduced || tabView === 'feed'}
											<!-- A PHOTO FEED. The only surface here that is not
											     work, which is exactly why it belongs: the day
											     leaks into it and it leaks back. -->
											<div class="mt-feedy">
												<div class="mt-ah mt-ah-feed"><span class="mt-ah-mark">◎</span><span class="mt-ah-t">Feed</span><span class="mt-ah-r mt-mono">♡  ✉  ⊕</span></div>
												<div class="mt-fystories">
													{#each [0, 1, 2, 3, 4, 5, 6] as s (s)}
														<span class="mt-fystory"></span>
													{/each}
												</div>
												<div class="mt-fypost">
													<span class="mt-fyava"></span>
													<span class="mt-fyname mt-mono">someone_you_met_once</span>
												</div>
												<span class="mt-fyimg"></span>
												<div class="mt-fyacts">
													<span class="mt-fyact">♥<b>1,204</b></span>
													<span class="mt-fyact">💬<b>86</b></span>
													<span class="mt-fyact">↗<b>31</b></span>
												</div>
												<!-- A POST WITHOUT A THREAD IS A CARD. The comments are
												     what make it social, and they are also the thing
												     that is pulling at you — which is the station's
												     whole argument. -->
												<div class="mt-fycmts">
													{#each [['maya.k', 'this is unreal 😍'], ['dev_r', 'where is this?'], ['anush', 'we were literally just talking about this']] as [who, txt] (who)}
														<span class="mt-fycmt"><b>{who}</b> {txt}</span>
													{/each}
												</div>
												<div class="mt-fyacts-old" hidden aria-hidden="true">
													<i></i><i></i><i></i>
												</div>
												<span class="mt-fycap">and 1,204 others liked this</span>
											</div>
										{/if}
										{#if reduced || tabView === 'network'}
											<!-- A PROFESSIONAL NETWORK: three notifications, none
											     of which is about you, and one message you have
											     been meaning to answer for a week. -->
											<div class="mt-net-app">
											<div class="mt-ah mt-ah-net"><span class="mt-ah-mark">in</span><span class="mt-ah-t">Home  My Network  Jobs</span><span class="mt-ah-r mt-mono">Search   Me ▾</span></div>
											<!-- THREE COLUMNS, because that is the silhouette. A
											     professional network is a profile card on the left, a
											     feed down the middle and news on the right — and the
											     middle column was missing entirely, which is why it
											     read as a notification list rather than a site anyone
											     would recognise. -->
											<div class="mt-net">
												<div class="mt-net-me">
													<span class="mt-net-cover"></span>
													<span class="mt-nava mt-net-pic"></span>
													<b>You</b>
													<i class="mt-nsub">Ops lead · Meridian</i>
													<span class="mt-net-stat"><span>Profile views</span><b>142</b></span>
													<span class="mt-net-stat"><span>Post impressions</span><b>1,204</b></span>
													<span class="mt-net-stat"><span>Search appearances</span><b>14</b></span>
												</div>
												<div class="mt-net-feed">
													<div class="mt-net-composer mt-mono">Start a post…</div>
													{#each [['Anika R.', 'Ops lead · 2h', 'Thrilled to share that after 5 years I am moving on to…', '86 · 12 comments'], ['Meridian Careers', 'Promoted · 4h', 'We are hiring 14 roles across platform and ops. Tag someone who…', '214 · 41 comments'], ['Dev K.', '1st · 6h', 'Unpopular opinion: most standups could be a message. Here is what we changed…', '1,102 · 308 comments']] as [who, meta, body, react] (who)}
														<div class="mt-net-post">
															<span class="mt-nava"></span>
															<span class="mt-net-who">{who}<i class="mt-nsub">{meta}</i></span>
															<p>{body}</p>
															<span class="mt-net-react mt-mono">👍 ❤️ {react}</span>
															<span class="mt-net-acts mt-mono">Like · Comment · Repost · Send</span>
														</div>
													{/each}
												</div>
												<div class="mt-net-side">
													<b class="mt-mono">News</b>
													{#each ['Freight costs climb again', 'The 4-day week, 2 years on', 'Hiring slows in platform', 'What the new rules mean'] as n (n)}
														<span class="mt-net-news">{n}</span>
													{/each}
													<b class="mt-mono">Add to your feed</b>
													{#each ['Priya S.', 'Vendor Weekly'] as sug (sug)}
														<span class="mt-net-sug"><i class="mt-nava"></i>{sug}<em>+</em></span>
													{/each}
												</div>
											</div>
										</div>
										{/if}
										{#if reduced || tabView === 'canvas'}
											<!-- A DESIGN TOOL, open on an untitled document that is
											     four rectangles and a placeholder. The template
											     rail is full; the artboard is not. -->
											<div class="mt-canv-app">
											<div class="mt-ah mt-ah-canv"><span class="mt-ah-mark">◆</span><span class="mt-ah-t">Untitled design — 1080×1080</span><span class="mt-ah-r mt-mono">Share   ⤓</span></div>
											<!-- EMPTY READS AS CALM, and calm is the wrong feeling
											     here. A design tool open on four rectangles looks
											     restful; a real one is a wall of templates, a tool
											     rail, a layers list and a properties panel all
											     competing at once. The clutter IS the argument. -->
											<div class="mt-canv">
												<div class="mt-cvtools">
													{#each ['▣', '✎', 'T', '◯', '⌗', '⤢'] as t (t)}
														<span class="mt-cvtool">{t}</span>
													{/each}
												</div>
												<div class="mt-cvrail">
													<span class="mt-cvsearch mt-mono">Templates</span>
													{#each [0, 1, 2, 3, 4, 5] as sq (sq)}
														<span class="mt-cvthumb" style="--t:{sq % 4}"></span>
													{/each}
												</div>
												<!-- THE ARTBOARD IS THE APP. The first pass gave it a
												     twelve-template wall and a six-row layers list and
												     left the canvas itself the smallest thing on the
												     screen — which is not what a design tool looks
												     like, it is what a file browser looks like. The
												     document dominates now, and it carries actual
												     SHAPES and an image, because an empty artboard is
												     what made it read as broken rather than busy. -->
												<div class="mt-cvart">
													<span class="mt-cvimg"></span>
													<span class="mt-cvtitle"></span>
													<span class="mt-cvsub"></span>
													<span class="mt-cvcirc"></span>
													<span class="mt-cvbar"></span>
													<i class="mt-cvh mt-cvh1"></i><i class="mt-cvh mt-cvh2"></i>
													<i class="mt-cvh mt-cvh3"></i><i class="mt-cvh mt-cvh4"></i>
													<span class="mt-cvph mt-mono">Untitled design · unsaved</span>
												</div>
											</div>
										</div>
										{/if}
									</div>
								</div>
								<span class="mt-you-label mt-mono">you</span>
								</div>
								<!-- What each ordinary act LEFT BEHIND. These are the
								     motion the exodus had, REVERSED: each is born far
								     out at --ax/--ay and decelerates IN to a resting
								     place beside the person (--rx/--ry), at its own rate
								     (--v). And none of them fades at the end. That is
								     the entire beat: it is not that something was taken
								     from you, it is that something was left with you,
								     and it is still there. -->
								<!-- THE COUNTERS, loose on the page and arriving from the
							     RIGHT while the leavings arrive from the left. Two streams
							     converging is what makes it read as being closed in on
							     rather than as a list being written. -->
							<div class="mt-badges" aria-hidden="true">
								{#each COUNTS as c, ci (c.app)}
									<span
										class="mt-badge-c"
										style="--t:{c.t};--v:{c.v};--ax:{c.fx}vw;--ay:{c.fy}vh;--rx:{c.rx}vw;--ry:{c.ry}vh"
									>
										<i class="mt-badge-n mt-mono">{countNow[ci]}</i>
										<i class="mt-badge-l">{c.app} {c.label}</i>
									</span>
								{/each}
							</div>
							<div class="mt-exo-acts" aria-hidden="true">
									{#each ARRIVALS as a, ai (a.label)}
									<span
										class="mt-exo-act mt-mono"
										style="--t:{a.t};--v:{a.v};--ax:{a.ax}vw;--ay:{a.ay}vh;--rx:{a.rx}vw;--ry:{a.ry}vh"
										>+{arriveN[ai]} {a.label}</span
									>
								{/each}
								</div>
								<!-- THE BACKLOG IS GONE, and it was redundant rather than
								     wrong. It read "17 tabs you meant to read / 4 replies you
								     owe / a form you half-filled / the follow-up you promised"
								     under a "still waiting on you" header — and the arrivals
								     column three feet to its left already says "+28 tabs you
								     meant to read", in the SAME WORDS with a DIFFERENT NUMBER.
								     Two counts of one thing on one screen is not emphasis; it
								     is a contradiction the reader has to stop and resolve.
								
								     It made sense when it was the only list here — it is the old
								     harvest dossier, re-aimed from "what they learned about you"
								     to "what is still owed by you". Then the arrivals column grew
								     from five chips to ten, with counts that climb, and took that
								     job over. Two columns say it; a third only crowded the window,
								     which is the thing the beat is actually about. -->
								<!-- the flow, inward: it arrives, and then it arrives again -->
								{#each [0.11, 0.25, 0.47, 0.61, 0.71] as t0 (t0)}
									<span class="mt-exo-dot" style="--t0:{t0}" aria-hidden="true"></span>
								{/each}
								{#each [0.3, 0.52, 0.76] as t0 (t0)}
									<span
										class="mt-exo-dot mt-exo-dot2"
										style="--t0:{t0}"
										aria-hidden="true"
									></span>
								{/each}
								<!-- THE DEFERRAL, played out under the device where such a
								     prompt actually appears. The mouse drives in and
								     presses "Later" — the one moment in the station where
								     YOU are the mechanism — and the dialog falls away
								     into nothing, which is exactly the lie: the box
								     stopped existing, the work did not.

								     It is the old consent sheet's gesture with its
								     villain removed. "Allow all" on a blue button was
								     somebody doing something to you; "Later" on a plain
								     one is nobody's fault at all, which is the harder and
								     truer version. The cursor is a CHILD of the button,
								     pinned at its centre, so its tip lands on the control
								     by construction rather than by a tuned coordinate. -->
								<div class="mt-darkmodal" aria-hidden="true">
									<p>Finish setting this up?</p>
									<div class="mt-darkmodal-row">
										<span class="mt-dm-accept"
											>Later{#if !reduced}<span class="mt-exo-cursor"></span>{/if}</span
										>
										<span class="mt-dm-later">not now</span>
									</div>
								</div>
							</div>
							<!-- The station's closing line is where the ember lives: this
							     full stop detaches, flies the leg to the reversal, and
							     STRIKES the spark the thread is born from — one ember
							     lineage from the first fire to the aurora. The line
							     itself changed with the station's grievance: the loss
							     being named is a labour loss now, and it is stated as
							     arithmetic because that is what makes it undeniable. -->
							<p class="mt-caption"
								>The work multiplied. You didn’t<span
									class="mt-period"
									bind:this={periodEl}>.</span
								></p
							>
						{:else if station.id === 'tear'}
							<!-- The reversal — the pile-up is pulled back and the aurora
							     quiet thread blooms from the spark. The hero already introduced
							     Magican, so this beat spends no time introducing it again. It
							     resolves the history in TWO movements: “Your computer /
							     stopped waiting.” closes onto one line, then “Now it works
							     for you.” arrives as the consequence. -->
							<!-- THE IMPLOSION. Nothing is torn and nothing arrives from
							     outside the frame. The pile-up falls into the single
							     point it has been converging on all beat — placeDepth
							     collapses the drowning scene toward it — and the inversion
							     comes back out of that point. The whole argument in one
							     gesture: it is not that the work was taken away, it is
							     that all of it became one thing that handles it.

							     Everything is pinned to the same place (ignitionPoint =
							     −20px, −12vh from the station centre) so the collapse,
							     the canvas ember's landing and the thread's first stroke
							     are ONE point:

							       · .mt-ignite    — the seed already sitting at the point
							       · .mt-imp-core  — what the frame falls into: a hard
							                         bright singularity that draws in, snaps
							                         to a flash, and is gone
							       · .mt-imp-flare — what comes back out, and what the
							                         wordmark emerges from

							     They are SIBLINGS of the composition, not children: the
							     reveal below is position:relative for its own layout and
							     would otherwise capture them and move the point off its
							     mark. There is no era scrim here any more — the beat
							     plays in full daylight, so the light does not "return",
							     the ATTENTION does. -->
							<span class="mt-imp-flare" aria-hidden="true"></span>
							<span class="mt-ignite" aria-hidden="true"></span>
							<span class="mt-imp-ring" aria-hidden="true"></span>
							<span class="mt-imp-core" aria-hidden="true"></span>
							<!-- ONE LOCKUP, two states. The opening hero has already
							     introduced Magican, so this station no longer repeats “meet”.
							     It names the inversion the history earned: first “Your
							     computer / stopped waiting.”, then the same words close
							     onto one baseline as “Now it works for you.” arrives. -->
							<div class="mt-return" aria-hidden="true">
								<div class="mt-lock-1">
									<p class="mt-return-brand">Your computer</p>
									<p class="mt-return-makes">stopped waiting.</p>
								</div>
								<div class="mt-lock-2">
									<p class="mt-return-built">stopped waiting.</p>
									<p class="mt-return-yours">Now it works for you.</p>
								</div>
								<div class="mt-return-devices" aria-hidden="true">
									<span class="mt-dev"><i class="mt-dev-mac"></i> on your Mac</span>
									<span class="mt-dev"><i class="mt-dev-iphone"></i> on your iPhone</span>
								</div>
							</div>
						{:else if station.id === 'life'}
							<!-- THE PAYOFF. The greeting's line, answered — and the
							     only way to earn it is to SHOW it: devices working
							     untouched while people are somewhere else, doing the
							     thing the page opened by naming.

							     CONTAINED, never full-bleed: the montage is a picture
							     the page is holding, not a world the camera is inside.
							     The footage's own rule is the argument — device SHARP
							     in the foreground, people BLURRED behind, and nobody
							     touching, holding or looking at a screen in any frame.

							     THE ABSENCE CONTRACT. The footage is generated
							     separately. Until it lands, the frame is not in the tree
							     at all — no placeholder, no empty box, no broken image —
							     and the closing line stands alone on the paper, which is
							     a quiet ending rather than a missing one. The same rule
							     the reduced-motion still already follows for a reel that
							     has not shipped. Reduced motion is words-only by
							     construction: no canvas ever mounts there. -->
							{#if !reduced && lifeMounted}
								<div class="mt-lifeframe" class:on={lifeOn} bind:this={lifeFrameEl}>
									<LifeReel bind:this={lifeReel} onState={onLifeState} />
									<LifeDevice bind:this={lifeDevice} />
								</div>
							{/if}
							<!-- THE CLOSING LINE. It answers the greeting and stands
							     ALONE — no sub-line, because a second sentence here
							     would explain a beat that has just been shown.
							     PLACEHOLDER, by the plan's own account: the wording is
							     still open and this is the current favourite. It speaks
							     in the same voice as the lock-screen notification the
							     product already uses — a promise without an overclaim. -->
							<div class="mt-life" class:mt-life-bare={reduced || !lifeOn}>
								<h2 class="mt-life-line">Go. It’s handled.</h2>
							</div>
						{/if}
					</section>

					{#if station.id === 'tear'}
					<!-- The invitation into the montage, a road sign on the leg out
					     of the reveal — the later freedom payoff, about to be earned. -->
						<p
							class="mt-sign mt-sign-invite"
							class:static={reduced}
							style={reduced ? '' : `left:${SIGNAGE[0].x * xf}vw;top:${SIGNAGE[0].y * yf}vh`}
						>
							So you can:
						</p>
					{/if}

				{/each}
			</div>
		</div>

		{#if !reduced}
			<canvas class="mt-thread" bind:this={canvasEl} aria-hidden="true"></canvas>
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
	.mt-period {
		display: inline-block;
		--sw: clamp(0, calc((var(--local) - 0.6) * 5), 1);
		--rel: clamp(0, calc((var(--local) - 0.84) * 9), 1);
		--swr: calc(var(--sw) * (1 - var(--rel)));
		transform: scale(calc(1 + var(--swr) * 0.45));
		transform-origin: 50% 60%;
		background: none;
		-webkit-background-clip: initial;
		background-clip: initial;
		/* At full swell the glyph burns with the SATURATED theme accent — it is an
		   ember, not copy; the glow below carries the burn on any ground. */
		color: color-mix(
			in srgb,
			var(--film-ink) calc(100% - var(--swr) * 100%),
			color-mix(in srgb, var(--th) 82%, var(--film-ink))
		);
		text-shadow:
			0 0 calc(var(--swr) * 8px) var(--th),
			0 0 calc(var(--swr) * 22px) var(--th),
			0 0 calc(var(--swr) * 52px) var(--th-glow);
	}
	.mt-scene.static .mt-period {
		transform: none;
		color: inherit;
		text-shadow: none;
	}
	/* ── travel signage: road signs planted in the world ─────────── */
	.mt-sign {
		position: absolute;
		transform: translate(-50%, -50%);
		width: min(88vw, 42rem);
		margin: 0;
		text-align: center;
		font-family: var(--lp-font);
		font-size: clamp(1.3rem, 2.6vw, 2rem);
		font-weight: 300;
		line-height: 1.35;
		letter-spacing: -0.015em;
		color: var(--film-ink);
		text-wrap: balance;
	}
	.mt-sign-invite {
		font-family: var(--lp-mono);
		font-size: clamp(1rem, 1.8vw, 1.5rem);
		font-weight: 450;
		letter-spacing: 0.42em;
		text-transform: uppercase;
		color: color-mix(in srgb, var(--film-ink) 62%, var(--th));
		text-shadow: 0 0 32px var(--th-soft);
	}
	.mt-sign-era {
		/* the born→operate bridge: quiet, era-grey — a caption in the road */
		font-size: clamp(1.15rem, 2.2vw, 1.7rem);
		font-weight: 320;
		font-style: italic;
		letter-spacing: 0;
		color: var(--film-dim);
	}



	.mt-sign.static {
		position: static;
		transform: none;
		margin: clamp(1.5rem, 5vh, 3rem) auto;
	}


	/* ── THE MACHINE — fifty years, one object ────────────────────────
	   `--era` runs 0 (1977) → 1 (1995) → 2 (today) and `--e1`/`--e2` are its
	   two halves, so any property is stated once as

	       V0 + (V1 − V0)·e1 + (V2 − V1)·e2

	   and morphs continuously through all three. This is a TRUE morph, not
	   three drawings cross-fading: at era 0.5 there is one machine whose
	   bezel is half as deep as a CRT's and whose corners have tightened
	   halfway to a 1995 box. Nothing swaps, because the argument is that it
	   is the same machine the whole time and you are still operating it.

	   Every one of these is a paint-only property on ONE element, so the
	   morph costs a style recalc of the machine and nothing else. */
	/* THE MACHINE IS ANCHORED ON ITS SCREEN, not on its silhouette. It is a
	   zero-size point with the body centred on it and the neck and deck hung
	   below — so the well is ALWAYS exactly the `--scr-w × --scr-h` box that
	   `.mt-inscreen` targets, by construction rather than by a fudge factor.

	   Laid out as a flex column instead (body, then neck, then deck), the
	   column's centre sits below the body's centre by half the furniture
	   underneath it, and the content lands high: measured, the 1977 machine's
	   first typed line rendered on the bezel ABOVE the glass. */
	.mt-machine {
		position: absolute;
		z-index: 0;
		width: 0;
		height: 0;
		pointer-events: none;
		opacity: 0;
		transition: opacity 700ms ease;

		/* The screen geometry is NOT declared here. It is published on the
		   stage, so the machine and all three Act I stations read one number
		   and cannot drift — see `.mt-stage`. */
		/* THE MACHINE SITS BELOW CENTRE. Dead-centred, a 52%-tall body leaves
		   24% of the frame above it, and a two-line title plus its kicker
		   needs more than that — measured, "You became the glue between every
		   app" cleared the case and went off the top of the screen instead.
		   The offset is shared with `.mt-inscreen` so the picture goes with
		   the machine. */
		translate: var(--mach-dx, 0px) calc(var(--mach-dy, 0px) + var(--mach-oy, 7vh));

		/* `--bez` is published by publishScreen, not derived here: the body's
		   outer size is the constant and the bezel is what the screen is
		   growing INTO, so the two have to be computed together. */
		/* The casing's own colour: warm 1977 beige → cool 1995 putty →
		   aluminium. Nested color-mix, so the colour interpolates on the same
		   two ramps as the geometry. */
		--case: color-mix(
			in srgb,
			color-mix(in srgb, #b9a488, #cfcabc calc(var(--e1) * 100%)),
			#d7dade calc(var(--e2) * 100%)
		);
		--case-dk: color-mix(
			in srgb,
			color-mix(in srgb, #6d5c46, #8e8a7e calc(var(--e1) * 100%)),
			#9aa0a6 calc(var(--e2) * 100%)
		);
	}
	.mt-machine.mt-mach-on {
		opacity: 1;
	}

	/* The body: bezel + screen well. Its size is the screen plus the bezel on
	   every side, so as the bezel thins the whole machine tightens onto the
	   picture — which is most of what "fifty years of industrial design" is. */
	.mt-mach-body {
		position: absolute;
		left: 50%;
		top: 50%;
		transform: translate(-50%, -50%);
		width: calc(var(--scr-w) + var(--bez) * 2);
		height: calc(var(--scr-h) + var(--bez) * 2);
		border-radius: var(--rad);
		/* LIT FROM UPPER LEFT, and lit like a BOX rather than a card.
		   `--k` is how much of that treatment applies: full on the 1977 tube,
		   gone by the laptop, because a moulded beige case has form and a
		   milled aluminium lid essentially does not.

		   Four layers, in the order a renderer would put them: a broad key
		   falloff from the upper-left corner; the ambient occlusion that
		   collects along the bottom and right where a box turns away from the
		   room; a specular sheen along the top face; and only then the base
		   colour ramp. Any one of them alone still reads flat — it is having
		   both a lit side AND a turned-away side that makes it an object. */
		--k: calc(1 - var(--e2) * 0.86);
		background:
			radial-gradient(
				118% 96% at 20% 4%,
				rgba(255, 255, 255, calc(0.26 * var(--k))),
				transparent 56%
			),
			radial-gradient(
				126% 108% at 96% 104%,
				rgba(18, 12, 6, calc(0.42 * var(--k))),
				transparent 62%
			),
			linear-gradient(
				178deg,
				rgba(255, 255, 255, calc(0.3 * var(--k))) 0,
				transparent calc(6% + 4% * var(--k))
			),
			linear-gradient(
				160deg,
				color-mix(in srgb, var(--case) 88%, #fff),
				var(--case) 46%,
				var(--case-dk)
			);
		/* The CRT's depth reads as a heavy inner chamfer; a laptop lid has
		   almost none, so the inset shadow closes as the era advances.
		   The cast shadow is TWO shadows, which is the other half of why this
		   read flat: one tight and dark for the contact the object makes with
		   the room, one wide and soft for the light it blocks. A single
		   mid-range shadow reads as a glow behind a sticker. */
		box-shadow:
			inset 0 calc(2px + 6px * (1 - var(--e2))) calc(6px + 18px * (1 - var(--e2)))
				rgba(255, 255, 255, 0.34),
			inset 0 calc(-4px - 10px * (1 - var(--e2))) calc(10px + 26px * (1 - var(--e2)))
				rgba(0, 0, 0, 0.28),
			inset calc(-3px * var(--k)) 0 calc(14px * var(--k)) rgba(20, 14, 8, 0.3),
			inset calc(3px * var(--k)) 0 calc(10px * var(--k)) rgba(255, 255, 255, 0.16),
			0 calc(10px + 6px * var(--e2)) calc(16px + 8px * var(--e2)) calc(-8px)
				rgba(20, 14, 8, calc(0.5 * var(--k) + 0.18)),
			0 calc(34px + 20px * var(--e2)) calc(62px + 30px * var(--e2))
				calc(-22px) rgba(24, 20, 16, 0.46);
	}
	/* THE BEZEL IS NOT ONE PLANE. A 1977 case has a flat outer face, then a
	   CHAMFER that turns inward, and only then the glass — which sits behind
	   both. Drawn as one flat surround it reads as a sticker of a monitor.

	   `::before` is the chamfer: a ring whose gradient runs light on the top
	   face and dark on the bottom, which is what an inward-turning bevel does
	   under a ceiling light. It flattens out with the era, because a modern
	   lid genuinely is one plane.  */
	.mt-mach-body::before {
		content: '';
		position: absolute;
		inset: calc(var(--bez) * 0.46);
		border-radius: calc(var(--rad) * 0.6 + 4px);
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--case) 74%, #fff),
			var(--case) 38%,
			color-mix(in srgb, var(--case-dk) 86%, #000)
		);
		/* The chamfer takes the same key as the face it is cut into: the top
		   and left of the ring catch the light, the bottom and right take the
		   occlusion. Symmetric shading is what made this read as a printed
		   ring rather than a turned edge. */
		box-shadow:
			inset 0 2px 3px rgba(255, 255, 255, 0.4),
			inset 2px 0 4px rgba(255, 255, 255, 0.22),
			inset 0 -3px 5px rgba(0, 0, 0, 0.36),
			inset -3px 0 6px rgba(0, 0, 0, 0.28),
			0 1px 2px rgba(255, 255, 255, 0.18);
		opacity: calc(1 - var(--e2) * 0.82);
	}

	/* The well the picture sits in — and it sits BEHIND the bezel, not level
	   with it. The depth is the drop shadow cast by the chamfer onto the
	   glass: heavy from above in 1977, almost nothing on a laptop. */
	.mt-mach-well {
		position: absolute;
		inset: var(--bez);
		border-radius: calc(var(--rad) * 0.42 + 4px);
		background: #07090b;
		box-shadow:
			inset 0 calc(6px + 16px * (1 - var(--e2))) calc(10px + 22px * (1 - var(--e2)))
				rgba(0, 0, 0, 0.85),
			inset 0 calc(-3px - 7px * (1 - var(--e2))) calc(8px + 16px * (1 - var(--e2)))
				rgba(0, 0, 0, 0.6),
			0 0 0 1px rgba(0, 0, 0, 0.55);
	}
	/* The tube's own light: curvature bloom and scanlines, both of which
	   belong to 1977 and are gone by the time it is a laptop. */
	/* THE 1977 TUBE IS CURVED. CSS cannot bend a rectangle, but a curved
	   picture tube has three tells that it can draw, and together they are
	   enough: corners that are far rounder than the case's, a bright barrel
	   highlight sitting up and left of centre where the glass bulges toward
	   the ceiling light, and corners that fall off dark because the phosphor
	   is further away there. All three straighten out as the era advances,
	   which is exactly what happened to real screens. */
	.mt-mach-glass {
		position: absolute;
		inset: var(--bez);
		/* MODEST rounding — the corner radius is not the curvature. Pushed to
		   42px it just read as a rounded rectangle, which is a shape, not a
		   surface. A tube announces itself through LIGHT: a broad specular
		   bloom where the glass swells toward the room, a bright rim where
		   the face turns away at the edges, and a vignette in the corners
		   where the phosphor sits furthest from the viewer. Those are the
		   three below, and they do the work the radius was failing to do. */
		border-radius: calc((var(--rad) * 0.42 + 4px) + 14px * (1 - var(--e1)));
		pointer-events: none;
		z-index: 3;
		background:
			/* scanlines, 1977 only */
			repeating-linear-gradient(
					0deg,
					rgba(4, 12, 6, calc(0.4 * (1 - var(--e1)))) 0,
					rgba(4, 12, 6, 0) 1.7px,
					rgba(4, 12, 6, calc(0.4 * (1 - var(--e1)))) 3.4px
				),
			/* the bulge */
				radial-gradient(
					58% 44% at 34% 26%,
					rgba(255, 255, 255, calc(0.14 * (1 - var(--e2)))),
					transparent 70%
				),
			/* the rim: the face turning away at the very edge, which is the
			   single strongest tell that a surface is convex */
				radial-gradient(
					112% 112% at 50% 50%,
					transparent 78%,
					rgba(255, 255, 255, calc(0.16 * (1 - var(--e1)))) 92%,
					transparent 100%
				),
			/* corner falloff — the phosphor furthest from the viewer */
				radial-gradient(
					82% 82% at 50% 46%,
					transparent 44%,
					rgba(0, 0, 0, calc(0.55 * (1 - var(--e1)) + 0.12)) 100%
				);
	}
	/* The 1977 case's vents and lamp. They shrink away rather than vanishing,
	   which is what keeps the morph continuous. */
	.mt-mach-vents {
		position: absolute;
		right: calc(var(--bez) * 0.9);
		bottom: calc(var(--bez) * 0.28);
		display: flex;
		gap: 4px;
		opacity: calc(1 - var(--e2));
	}
	.mt-mach-vents i {
		width: calc(var(--scr-w) * 0.03 * (1 - var(--e2) * 0.6));
		height: 3px;
		border-radius: 2px;
		background: rgba(0, 0, 0, 0.22);
	}
	.mt-mach-vents i:last-child {
		background: color-mix(in srgb, var(--a) 70%, transparent);
	}

	/* The neck: a CRT's plinth, then a 1995 base, then nothing — a laptop's
	   lid meets its deck directly. It collapses to zero height rather than
	   being removed, so the machine settles onto its deck continuously. */
	.mt-mach-neck {
		position: absolute;
		left: 50%;
		top: calc(50% + var(--scr-h) / 2 + var(--bez));
		transform: translateX(-50%);
		width: calc(var(--scr-w) * (0.34 + 0.1 * var(--e1)));
		height: calc(var(--scr-w) * 0.045 * (1 - var(--e2)));
		border-radius: 0 0 6px 6px;
		background: linear-gradient(180deg, var(--case-dk), color-mix(in srgb, var(--case-dk) 70%, #000));
	}
	/* The deck: a detached keyboard slab in 1977 and 1995, which WIDENS and
	   comes forward until it is the base of a laptop. The perspective tilt
	   arrives with it — a keyboard lying on a desk is seen at an angle; a
	   laptop deck is part of the same object. */
	.mt-mach-deck {
		position: absolute;
		left: 50%;
		top: calc(
			50% + var(--scr-h) / 2 + var(--bez) + var(--scr-w) * 0.045 * (1 - var(--e2))
		);
		transform: translateX(-50%) perspective(700px) rotateX(calc(var(--e2) * 46deg));
		width: calc(var(--scr-w) * (0.52 + 0.12 * var(--e1) + 0.46 * var(--e2)));
		height: calc(var(--scr-w) * (0.05 + 0.004 * var(--e1) + 0.012 * var(--e2)));
		margin-top: calc(var(--scr-w) * 0.03 * (1 - var(--e2)));
		border-radius: calc(4px + 6px * var(--e2)) calc(4px + 6px * var(--e2))
			calc(3px + 9px * var(--e2)) calc(3px + 9px * var(--e2));
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--case) 92%, #fff),
			var(--case-dk)
		);
		box-shadow: 0 10px 22px -12px rgba(24, 20, 16, 0.5);
		transform-origin: top center;
	}

	/* THE ERA CAPTION STANDS ABOVE THE MACHINE, never on it. The scene is a
	   centred flex column, so left in flow the header renders at the scene's
	   centre — which is now exactly where the screen is, and "1995 · the
	   operate era" printed itself across the working day it was labelling.
	   Lifted out of flow and hung off the same screen geometry the machine
	   is built from, so it clears the bezel at every viewport. */
	.mt-s-drown .mt-chapter-head {
		position: absolute;
		left: 50%;
		top: 50%;
		width: min(92vw, 60rem);
		/* Anchored by its own BOTTOM edge, not its centre. Centred, a
		   one-line kicker and a two-line title clear the machine by very
		   different amounts, and "You became the glue between every app"
		   ran straight over the screen it was labelling.

		   The bezel has to be in the sum too: the body is the screen PLUS its
		   surround, and that surround is thickest in 1977 and thinnest today.
		   Left out, the 1995 header cleared the glass and still sat 13px
		   inside the case. It uses the REAL `--bez` rather than a worst-case
		   share of the width — with the latter the year floated far above a
		   thin-bezelled machine and stopped reading as its label. */
		transform: translate(
			-50%,
			calc(
				-100% + var(--mach-oy, 7vh) - var(--scr-h) / 2 - var(--bez, 0px) -
					clamp(0.5rem, 2vh, 1.4rem)
			)
		);
		z-index: 3;
	}

	/* ONE ERA AT A TIME ON THE GLASS, AND ONE CAPTION OVER IT. The Act I
	   stack cross-fades its stations so a hand-over is a dissolve rather than
	   a cut — right for the machine, wrong for these two. Overlapping
	   captions do not read as a dissolve, they read as broken text: measured
	   at the born→operate boundary, "1977 · the machine arrives" and "1995 ·
	   the operate era" printed through each other as "197795 —
	   THEHMACHINETARRIVES". And two desktops at once put a Windows task bar
	   along the bottom of a 1977 tube.
	   
	   `.on` is the OWNING scene, so both simply belong to whichever era owns
	   the scroll, with a short fade of their own for softness. The casing
	   keeps morphing continuously underneath — which is the thing that was
	   supposed to carry the hand-over all along. */
	.mt-scene:not(.on):not(.mt-receding) .mt-chapter-head,
	.mt-scene:not(.on):not(.mt-receding) .mt-inscreen {
		opacity: 0;
	}
	.mt-scene .mt-chapter-head,
	.mt-scene .mt-inscreen {
		transition: opacity 700ms ease;
	}
	.mt-scene.static .mt-chapter-head,
	.mt-scene.static .mt-inscreen {
		opacity: 1;
		transition: none;
	}

	/* WHERE EACH ERA'S CONTENT PLAYS. One rule, used by all three, so the
	   picture is in exactly the same box in every era and the machine reads
	   as one screen showing fifty years rather than three screens. */
	/* `.mt-scene .mt-inscreen`, not `.mt-inscreen`: the three eras' content
	   blocks each carry their own sizing from when they were free-standing
	   props on the page, and a single-class rule loses to them. Measured
	   before this: 1995's desk sat 525px right and 439px down of the glass at
	   its own size, and today's browser 373px right. */
	.mt-scene .mt-inscreen {
		position: absolute;
		left: 50%;
		top: 50%;
		/* AUTHORED AT A CONSTANT SIZE, SCALED INTO THE SCREEN. Every era's
		   content was choreographed against a full page — the 1995 desk flies
		   six windows across it, the drowning sizes a browser in vw — and
		   re-deriving all of that against a 720px screen would be a rewrite
		   of three beats. Laying out at the design size and scaling by
		   `--scr-k` keeps the choreography exactly as it shipped, and makes
		   it follow the match cut's changing rect for nothing. */
		width: 820px;
		height: var(--scr-dh, 492px);
		transform: translate(-50%, calc(-50% + var(--mach-oy, 7vh))) scale(var(--scr-k, 0.65));
		/* The glass CLIPS. Whatever an era puts on the screen is bounded by
		   the screen, which is the one rule that makes three very different
		   pieces of content read as one machine showing them. */
		overflow: hidden;
		/* THE SAME CORNERS AS THE WELL. At a flat 6px the well's own much
		   rounder corners showed through as four dark wedges, and the
		   desktop read as a square picture inside a rounded hole. Stated
		   once here in the well's own terms, divided by the scale this box
		   is about to be multiplied by, so it lands on the well exactly. */
		border-radius: calc((var(--rad) * 0.42 + 4px) / var(--scr-k, 0.65));
		z-index: 2;
		display: flex;
		flex-direction: column;
		align-items: stretch;
		justify-content: flex-start;
		/* A margin off the glass edge, in the design's own units — the scale
		   above carries it to whatever the screen currently is. */
		padding: 48px;
		gap: 14px;
	}
	/* THE SCREEN IS ONE NUMBER, and it lives on the stage so the machine, the
	   four Act I stations and the match cut cannot drift apart. The reel
	   overrides it per-frame during the handover (see applyMachineMatch) to
	   the rect the footage's own screen occupies; everywhere else this is it. */
	.mt-stage {
		--scr-w: min(62vw, 720px);
		--scr-h: calc(var(--scr-w) * 0.6);
		/* Corner radius: a fat rounded tube, then a squarer box, then the
		   soft rectangle of a laptop lid. Declared HERE, not on the machine,
		   because the screen's content has to round to the same curve — and
		   `.mt-inscreen` lives in a station, which is the machine's SIBLING.
		   On the machine the var simply did not reach it, the calc failed,
		   and both desktops kept square corners inside a rounded well. */
		--rad: calc(46px + -26px * var(--e1, 0) + 2px * var(--e2, 0));
	}
	/* The professional network, in its own unmistakable three columns. */
	.mt-net {
		display: grid;
		/* The side columns carry a profile card and a news list, and at 5.6em
		   neither had room to be either — they read as gutters. Wide enough
		   that the three-column shape is the first thing the eye gets. */
		grid-template-columns: 11.6em 1fr 10.2em;
		gap: 0.4em;
		padding: 0.4em;
		min-height: 0;
		overflow: hidden;
		font-size: 0.94em;
	}
	.mt-net-me {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.1em;
		padding-bottom: 0.4em;
		border: 1px solid var(--film-rule, rgba(0, 0, 0, 0.1));
		border-radius: 0.4em;
		overflow: hidden;
		font-size: 0.54em;
		text-align: center;
	}
	.mt-net-cover {
		width: 100%;
		height: 1.6em;
		background: linear-gradient(100deg, #0a66c2, #7ec8e3);
		flex: none;
	}
	.mt-net-pic {
		margin-top: -0.9em;
		width: 1.7em;
		height: 1.7em;
		border: 2px solid #fff;
	}
	.mt-net-me b {
		font-size: 1.05em;
	}
	.mt-net-stat {
		display: flex;
		justify-content: space-between;
		width: 100%;
		padding: 0.1em 0.5em;
		font-size: 0.86em;
		opacity: 0.7;
	}
	.mt-net-stat b {
		color: #0a66c2;
	}
	.mt-net-feed {
		display: flex;
		flex-direction: column;
		gap: 0.35em;
		min-height: 0;
		overflow: hidden;
	}
	.mt-net-composer {
		padding: 0.3em 0.6em;
		border: 1px solid var(--film-rule, rgba(0, 0, 0, 0.12));
		border-radius: 999px;
		font-size: 0.52em;
		opacity: 0.55;
		flex: none;
	}
	.mt-net-post {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 0.1em 0.35em;
		padding: 0.35em 0.45em;
		border: 1px solid var(--film-rule, rgba(0, 0, 0, 0.1));
		border-radius: 0.4em;
	}
	.mt-net-who {
		font-size: 0.54em;
		font-weight: 600;
		display: flex;
		flex-direction: column;
	}
	.mt-net-post p {
		grid-column: 1 / -1;
		margin: 0.15em 0 0;
		font-size: 0.5em;
		line-height: 1.4;
		opacity: 0.82;
	}
	.mt-net-react,
	.mt-net-acts {
		grid-column: 1 / -1;
		font-size: 0.44em;
		opacity: 0.55;
	}
	.mt-net-acts {
		padding-top: 0.18em;
		border-top: 1px solid var(--film-rule, rgba(0, 0, 0, 0.08));
	}
	.mt-net-side {
		display: flex;
		flex-direction: column;
		gap: 0.16em;
		padding: 0.35em;
		border: 1px solid var(--film-rule, rgba(0, 0, 0, 0.1));
		border-radius: 0.4em;
		font-size: 0.46em;
		overflow: hidden;
	}
	.mt-net-side b {
		font-size: 1.05em;
		opacity: 0.6;
	}
	.mt-net-news {
		opacity: 0.78;
		line-height: 1.3;
	}
	.mt-net-sug {
		display: flex;
		align-items: center;
		gap: 0.3em;
	}
	.mt-net-sug i {
		width: 0.9em;
		height: 0.9em;
	}
	.mt-net-sug em {
		margin-left: auto;
		font-style: normal;
		opacity: 0.6;
	}

	/* The design tool: tool rail · template wall · artboard · properties.
	   EMPTY READS AS CALM, and calm is the wrong feeling — four rectangles on
	   white looked restful when the station is about being overwhelmed. */
	/* The bills screen's actions. */
	.mt-pacts {
		display: flex;
		gap: 0.3em;
		margin-top: auto;
		padding: 0.4em 0.5em 0.1em;
		border-top: 1px solid var(--film-rule, rgba(0, 0, 0, 0.1));
		flex: none;
	}
	.mt-pact {
		padding: 0.22em 0.6em;
		border-radius: 0.3em;
		border: 1px solid var(--film-rule, rgba(0, 0, 0, 0.18));
		font-size: 0.5em;
		opacity: 0.72;
	}
	.mt-pact-go {
		margin-left: auto;
		border-color: transparent;
		background: #1b3a6b;
		color: #fff;
		opacity: 1;
	}

	/* The design tool: a slim tool rail, a short template strip, and then the
	   ARTBOARD, which is the app. The first pass buried it behind a
	   twelve-template wall and a layers list, so the document ended up the
	   smallest thing on screen — a file browser, not a design tool. */
	.mt-canv {
		display: grid;
		grid-template-columns: 1.4em 4.4em 1fr;
		gap: 0.35em;
		padding: 0.35em;
		min-height: 0;
		overflow: hidden;
	}
	.mt-cvtools {
		display: flex;
		flex-direction: column;
		gap: 0.2em;
	}
	.mt-cvtool {
		display: grid;
		place-items: center;
		height: 1.25em;
		border-radius: 0.25em;
		background: rgba(0, 0, 0, 0.06);
		font-size: 0.55em;
		opacity: 0.6;
	}
	.mt-cvsearch {
		padding: 0.14em 0.4em;
		border-radius: 0.25em;
		background: rgba(0, 0, 0, 0.06);
		font-size: 0.44em;
		opacity: 0.55;
	}
	/* The document, with things actually ON it. An empty artboard read as
	   broken rather than busy — a design in progress has a picture, a
	   headline, a couple of shapes, and selection handles round whatever you
	   last touched. */
	.mt-cvart {
		position: relative;
		border: 1px solid var(--film-rule, rgba(0, 0, 0, 0.14));
		border-radius: 0.2em;
		background: #fff;
		overflow: hidden;
	}
	.mt-cvimg {
		position: absolute;
		left: 6%;
		top: 8%;
		width: 44%;
		height: 46%;
		border-radius: 0.2em;
		background:
			linear-gradient(150deg, #9fd3c7 0 42%, #7fb8c9 42% 70%, #d9e6ea 70%),
			#cfe3e6;
	}
	.mt-cvtitle {
		position: absolute;
		left: 55%;
		top: 14%;
		width: 38%;
		height: 8%;
		border-radius: 0.1em;
		background: rgba(0, 0, 0, 0.72);
	}
	.mt-cvsub {
		position: absolute;
		left: 55%;
		top: 26%;
		width: 30%;
		height: 4.5%;
		border-radius: 0.1em;
		background: rgba(0, 0, 0, 0.26);
	}
	.mt-cvcirc {
		position: absolute;
		right: 8%;
		bottom: 14%;
		width: 18%;
		aspect-ratio: 1;
		border-radius: 50%;
		background: #f4b942;
	}
	.mt-cvbar {
		position: absolute;
		left: 6%;
		bottom: 12%;
		width: 40%;
		height: 7%;
		border-radius: 999px;
		background: #7a5af8;
	}
	/* Selection handles on the picture — the one detail that says a person is
	   in the middle of moving something. */
	.mt-cvh {
		position: absolute;
		width: 0.32em;
		height: 0.32em;
		background: #fff;
		border: 1px solid #4a7cf7;
		border-radius: 1px;
	}
	.mt-cvh1 {
		left: calc(6% - 0.16em);
		top: calc(8% - 0.16em);
	}
	.mt-cvh2 {
		left: calc(50% - 0.16em);
		top: calc(8% - 0.16em);
	}
	.mt-cvh3 {
		left: calc(6% - 0.16em);
		top: calc(54% - 0.16em);
	}
	.mt-cvh4 {
		left: calc(50% - 0.16em);
		top: calc(54% - 0.16em);
	}

	/* EVERY SCREEN GETS A HEADER, because that is the first thing the eye uses
	   to decide what it is looking at. A pane of content with no top bar reads
	   as a fragment — and seven fragments in a row read as one undifferentiated
	   smear, which is exactly what the station must not be: the argument is
	   that these are DIFFERENT products, each demanding you.

	   One shape, tinted per app. The mark and the accent do the identifying;
	   the right-hand chrome (menus, actions, nav) does the rest. */
	.mt-ah {
		display: flex;
		align-items: center;
		gap: 0.45em;
		padding: 0.28em 0.55em;
		flex: none;
		border-bottom: 1px solid var(--film-rule, rgba(0, 0, 0, 0.12));
		background: color-mix(in srgb, var(--ah, #6b8afd) 12%, transparent);
		font-size: 0.92em;
	}
	.mt-ah-mark {
		display: grid;
		place-items: center;
		width: 1.15em;
		height: 1.15em;
		border-radius: 0.28em;
		background: var(--ah, #6b8afd);
		color: #fff;
		font-size: 0.62em;
		font-weight: 700;
		flex: none;
	}
	.mt-ah-t {
		font-size: 0.6em;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.mt-ah-r {
		margin-left: auto;
		font-size: 0.5em;
		opacity: 0.55;
		white-space: nowrap;
	}
	.mt-ah-feed {
		--ah: #d6249f;
	}
	.mt-ah-board {
		--ah: #4a7cf7;
	}
	.mt-ah-sheet {
		--ah: #1e8e3e;
	}
	.mt-ah-pay {
		--ah: #1b3a6b;
	}
	.mt-ah-form {
		--ah: #7a5af8;
	}
	.mt-ah-net {
		--ah: #0a66c2;
	}
	.mt-ah-canv {
		--ah: #00c4cc;
	}
	/* Each app is a column: its header, then its content taking the rest. The
	   wrappers exist only to hold that stack, so they inherit the pane's box. */
	.mt-board-app,
	.mt-sheet-app,
	.mt-pay-app,
	.mt-form-app,
	.mt-net-app,
	.mt-canv-app {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		overflow: hidden;
	}

	/* ── THE GLUE STATION'S SCREENS ────────────────────────────────────────
	   Same rule as the 1995 windows: what names an app is its FURNITURE, and
	   density is what makes it look used. A three-part mail layout, a
	   conversation rail beside a thread, a post with a comment thread under
	   it — each of those silhouettes is recognisable before a word is read,
	   which is all these get, at a second each behind a caption. */

	/* Mail: rail · toolbar · list. Rows on a rectangle could be anything. */
	.mt-mailapp {
		display: grid;
		grid-template-columns: 6.2em 1fr;
		height: 100%;
		min-height: 0;
		font-size: 0.94em;
	}
	.mt-mrail {
		display: flex;
		flex-direction: column;
		gap: 0.12em;
		padding: 0.4em 0.35em;
		border-right: 1px solid var(--film-rule, rgba(0, 0, 0, 0.1));
		overflow: hidden;
	}
	.mt-mcompose {
		margin-bottom: 0.35em;
		padding: 0.28em 0.5em;
		border-radius: 999px;
		background: color-mix(in srgb, var(--ah-strong, #6b8afd) 22%, transparent);
		font-size: 0.62em;
		text-align: center;
	}
	.mt-mfold {
		display: flex;
		justify-content: space-between;
		padding: 0.16em 0.45em;
		border-radius: 0 999px 999px 0;
		font-size: 0.6em;
		opacity: 0.66;
	}
	.mt-mfold i {
		font-style: normal;
		opacity: 0.7;
	}
	.mt-mfold.on {
		background: color-mix(in srgb, var(--ah-strong, #6b8afd) 18%, transparent);
		font-weight: 600;
		opacity: 1;
	}
	.mt-mmain {
		display: flex;
		flex-direction: column;
		min-height: 0;
		overflow: hidden;
	}
	.mt-mtop {
		display: flex;
		align-items: center;
		gap: 0.5em;
		padding: 0.3em 0.5em;
		border-bottom: 1px solid var(--film-rule, rgba(0, 0, 0, 0.1));
		flex: none;
	}
	.mt-msearch {
		flex: 1;
		padding: 0.16em 0.6em;
		border-radius: 999px;
		background: rgba(0, 0, 0, 0.06);
		font-size: 0.56em;
		opacity: 0.6;
	}
	.mt-mtools {
		display: flex;
		gap: 0.3em;
	}
	.mt-mtools i {
		width: 0.5em;
		height: 0.5em;
		border-radius: 2px;
		background: rgba(0, 0, 0, 0.22);
	}

	/* Messaging: a rail of conversations and one open thread. */
	.mt-msg {
		display: grid;
		grid-template-columns: 6.4em 1fr;
		height: 100%;
		min-height: 0;
	}
	.mt-msg-rail {
		display: flex;
		flex-direction: column;
		border-right: 1px solid var(--film-rule, rgba(0, 0, 0, 0.1));
		overflow: hidden;
	}
	.mt-msg-conv {
		display: flex;
		align-items: center;
		gap: 0.35em;
		padding: 0.3em 0.4em;
		font-size: 0.6em;
		border-bottom: 1px solid var(--film-rule, rgba(0, 0, 0, 0.07));
	}
	.mt-msg-conv.on {
		background: rgba(0, 0, 0, 0.06);
		font-weight: 600;
	}
	.mt-msg-conv b {
		margin-left: auto;
		min-width: 1.25em;
		height: 1.25em;
		display: grid;
		place-items: center;
		border-radius: 999px;
		background: #34c759;
		color: #fff;
		font-size: 0.8em;
	}
	.mt-msg-thread {
		display: flex;
		flex-direction: column;
		gap: 0.22em;
		padding: 0.35em 0.5em;
		min-height: 0;
		overflow: hidden;
	}
	.mt-msg-head {
		display: flex;
		align-items: center;
		gap: 0.35em;
		padding-bottom: 0.3em;
		border-bottom: 1px solid var(--film-rule, rgba(0, 0, 0, 0.1));
		font-size: 0.62em;
		font-weight: 600;
		flex: none;
	}
	.mt-msg-head i {
		margin-left: auto;
		font-style: normal;
		font-size: 0.8em;
		opacity: 0.55;
		font-weight: 400;
	}
	/* The bubbles lean the way the sender does — the one detail that makes a
	   stack of lines read as a conversation rather than a list. */
	.mt-msg-b {
		max-width: 76%;
		padding: 0.22em 0.5em;
		border-radius: 0.9em;
		font-size: 0.56em;
		line-height: 1.35;
	}
	.mt-msg-in {
		align-self: flex-start;
		background: rgba(0, 0, 0, 0.08);
		border-bottom-left-radius: 0.2em;
	}
	.mt-msg-out {
		align-self: flex-end;
		background: color-mix(in srgb, #34c759 40%, transparent);
		border-bottom-right-radius: 0.2em;
	}
	.mt-msg-thread .mt-msg-in.mt-mono {
		align-self: stretch;
		margin-top: auto;
		background: rgba(0, 0, 0, 0.05);
		border-radius: 999px;
		padding: 0.22em 0.7em;
		font-size: 0.52em;
		opacity: 0.55;
		flex: none;
	}

	/* The feed's interactions and its thread — a post without one is a card. */
	.mt-fyacts {
		display: flex;
		gap: 0.9em;
		padding: 0.3em 0.5em 0.15em;
		font-size: 0.58em;
	}
	.mt-fyact {
		display: inline-flex;
		align-items: center;
		gap: 0.28em;
		opacity: 0.85;
	}
	.mt-fyact b {
		font-weight: 500;
		font-size: 0.86em;
		opacity: 0.7;
	}
	.mt-fycmts {
		display: flex;
		flex-direction: column;
		gap: 0.12em;
		padding: 0 0.5em 0.35em;
		font-size: 0.52em;
		line-height: 1.4;
		opacity: 0.78;
	}
	.mt-fycmt b {
		font-weight: 600;
		opacity: 0.9;
	}

	.mt-wcopy {
		font-size: 0.62rem;
		color: #6b7284;
	}
	/* ── the six windows' insides ─────────────────────────────────── */
	.mt-orow {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		font-size: 0.68rem;
		color: #4a5164;
		padding: 0.18rem 0;
		border-bottom: 1px solid rgba(120, 128, 145, 0.22);
	}
	.mt-odot {
		flex: none;
		width: 7px;
		height: 7px;
		border-radius: 2px;
		background: #7f88a0;
	}
	.mt-ogrid {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 3px;
	}
	.mt-ocell {
		height: 15px;
		border: 1px solid #b3b9c6;
		background: #fff;
	}
	.mt-ocell.on {
		background: #d7dEEA;
	}
	.mt-obar {
		font-size: 0.6rem;
		color: #4a5164;
		border: 1px solid #b3b9c6;
		background: #fff;
		border-radius: 3px;
		padding: 0.2rem 0.4rem;
	}
	.mt-oline {
		display: block;
		height: 7px;
		border-radius: 2px;
		background: #c9cfdb;
		width: 0;
		transition: width 800ms ease calc(var(--li) * 0.09s);
	}
	.mt-oline.on {
		width: calc(var(--w) * 100%);
	}

	/* ── ACT I · 3 · THE DROWNING — the work multiplying, honestly ──
	   The station keeps the PAGE'S OWN INK the whole way down. It used to
	   cross-fade to a light ink as the ground darkened, which is right for a
	   beat that goes to night and wrong for one that only goes dull: the
	   cross-fade's midpoint put mid-grey type on a mid-grey ground and the
	   whole station greyed out for a third of its length. With the gloom
	   capped at 0.3 there is nothing to compensate for. */
	.mt-s-drown {
		--film-ink: var(--text-primary);
		--film-dim: color-mix(in srgb, var(--film-ink) 62%, transparent);
		color: var(--film-ink);
	}
	/* In document mode there is no dimming ground to carry the mood, so the
	   station takes a plate of its own — but a DULL one, not a night: the
	   page's own paper, pushed down and desaturated. */
	.mt-scene.static.mt-s-drown {
		background:
			radial-gradient(85% 65% at 50% 40%, color-mix(in srgb, var(--text-primary) 7%, transparent), transparent 74%),
			color-mix(in srgb, var(--text-primary) 12%, var(--landing-bg));
		border-radius: 24px;
		margin: clamp(1rem, 3vh, 2rem) auto;
		max-width: min(94vw, 64rem);
	}
	.mt-scene.static .mt-exodus {
		width: 100%;
		/* The document stacks the day's three apps into one tall column; the
		   ride's fixed plate height then cuts it off and drops the closing
		   line on top of it. The document has no camera to fit, so the plate
		   takes the height its content actually needs. */
		height: auto;
		min-height: min(48vh, 30rem);
		padding-block: 1.5rem;
	}
	.mt-exodus {
		/* STATIC, so it is not a containing block. Everything in this beat —
		   the screen, the counters, the leavings — then anchors to the
		   SCENE's centre, which is the one point the machine is also built
		   around. As `relative` it became the containing block for the
		   screen and offset it by its own 23px of layout, so today's desktop
		   sat higher in the glass than 1977's and 1995's did. */
		position: static;
		width: 100vw;
		height: min(48vh, 30rem);
		display: grid;
		place-items: center;
	}
	.mt-you {
		position: relative;
		display: grid;
		justify-items: center;
		gap: 0.5rem;
	}
	.mt-you-glow {
		position: absolute;
		left: 50%;
		top: 50%;
		width: 210px;
		height: 250px;
		transform: translate(-50%, -55%);
		border-radius: 60px;
		/* A warm bloom, not a slab. It used to sit on a black night, where a
		   near-solid centre read as a halo; on the drowning's much lighter
		   floor the same alpha renders as a white rectangle behind the
		   phone, so the core comes down and the falloff starts sooner. */
		background: radial-gradient(
			closest-side,
			rgba(255, 236, 205, 0.2),
			rgba(255, 220, 160, 0.08) 46%,
			transparent 74%
		);
		/* the person's own light goes out of them as the pile grows — the
		   one place the station is allowed to be about a feeling */
		opacity: calc(1 - clamp(0, calc(var(--local) * 1.1), 1) * 0.62);
	}
	.mt-scene.static .mt-you-glow {
		opacity: 1;
	}
	.mt-you-label {
		position: relative;
		color: color-mix(in srgb, var(--film-ink) 84%, transparent);
		letter-spacing: 0.22em;
		font-size: 0.66rem;
	}
	/* ── THE WINDOW ─────────────────────────────────────────────────
	   Deliberately a browser and not a phone. The phone version handed off
	   between three apps, which reads as "a busy day"; a tab strip that
	   keeps growing reads as the thing itself, because the reader has
	   watched it happen to their own window. */
	.mt-brow {
		position: relative;
		z-index: 1;
		/* Taller and wider than the phone it replaced: an inbox with six rows
		   in it is the point, and six rows need the height. */
		width: min(84vw, 460px);
		border-radius: 12px;
		overflow: hidden;
		background: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 94%, transparent);
		border: 1px solid color-mix(in srgb, var(--text-primary) 16%, transparent);
		box-shadow: 0 26px 60px -26px color-mix(in srgb, var(--text-primary) 40%, transparent);
	}
	/* THE STRIP. `flex: 1 1 0` with `min-width: 0` is the whole trick: every
	   tab takes an equal share of a fixed width, so each new one makes all of
	   them narrower — exactly what a real browser does, and the reason the
	   strip alone tells the story without a single word of copy. */
	.mt-tabs {
		display: flex;
		gap: 2px;
		padding: 5px 5px 0;
		background: color-mix(in srgb, var(--text-primary) 8%, transparent);
	}
	/* macOS traffic lights. `flex: none` matters: the strip hands every tab
	   an equal share of a fixed width, and anything in there that can be
	   squeezed would shrink as the tabs multiply — these must not. */
	.mt-lights {
		flex: none;
		display: flex;
		align-items: center;
		gap: 4px;
		padding: 0 7px 0 3px;
	}
	.mt-light {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		/* A hairline inner edge is what keeps them from reading as flat
		   stickers on a light ground. */
		box-shadow: inset 0 0 0 0.5px rgba(0, 0, 0, 0.16);
	}
	.mt-light-r {
		background: #ff5f57;
	}
	.mt-light-y {
		background: #febc2e;
	}
	.mt-light-g {
		background: #28c840;
	}

	.mt-tab {
		flex: 1 1 0;
		min-width: 0;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		font-family: var(--lp-font);
		font-size: 0.5rem;
		line-height: 1;
		padding: 0.34rem 0.3rem;
		border-radius: 5px 5px 0 0;
		background: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 90%, transparent);
		color: color-mix(in srgb, var(--text-primary) 62%, transparent);
		/* A tab that has not arrived takes no share of the strip, so the ones
		   that have are full width until the next lands. */
		flex-grow: 0;
		flex-basis: 0;
		padding-inline: 0;
		opacity: 0;
		transition:
			flex-grow 700ms cubic-bezier(0.22, 1, 0.36, 1),
			opacity 700ms ease,
			padding-inline 700ms cubic-bezier(0.22, 1, 0.36, 1);
	}
	.mt-tab.on {
		flex-grow: 1;
		padding-inline: 0.3rem;
		opacity: 1;
	}
	.mt-brow-body {
		/* Whichever tab is open, the window is the same size: a browser that
		   resized every time you changed tab would be the one unrealistic
		   thing in the shot. */
		min-height: 216px;
		padding: 0.5rem 0.6rem 0.7rem;
		font-family: var(--lp-font);
		color: var(--text-primary);
	}
	/* ── the inbox: Gmail's SHAPE, none of its branding ── */
	.mt-mail,
	.mt-chat,
	.mt-form,
	.mt-pay {
		display: grid;
		gap: 1px;
		/* The list takes what is left after the header and the footer, and
		   clips — it is the FOOTER that must always be visible. */
		min-height: 0;
		overflow: hidden;
	}

	/* ── FIVE MORE SURFACES ───────────────────────────────────────────
	   The station had four, which reads as "a few apps" — and the felt
	   experience being argued for is that there is no bottom to it. Each of
	   these is drawn to its own SHAPE and nothing else: the silhouette of a
	   kanban board, a spreadsheet, a photo feed, a professional network and a
	   design tool are all instantly legible at 200px with no logo, no brand
	   colour and no name, which is both the honest way to do it and the only
	   way we are entitled to. Every one of them is the same person's day. */

	/* A KANBAN BOARD. The argument is the column widths: "Blocked on you" is
	   full and "Done" has one old card in it. */
	.mt-board {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 0.3rem;
		font-size: 0.5rem;
	}
	.mt-bcol {
		display: grid;
		align-content: start;
		gap: 0.22rem;
		padding: 0.26rem;
		border-radius: 5px;
		background: color-mix(in srgb, var(--text-primary) 4%, transparent);
	}
	.mt-bcolh {
		display: flex;
		align-items: center;
		justify-content: space-between;
		font-size: 0.44rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: color-mix(in srgb, var(--text-primary) 52%, transparent);
	}
	.mt-bcolh i {
		font-style: normal;
		padding: 0 0.24rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--text-primary) 10%, transparent);
	}
	.mt-bcard {
		padding: 0.26rem 0.3rem;
		border-radius: 4px;
		background: var(--surface-1, #fff);
		border: 1px solid color-mix(in srgb, var(--text-primary) 10%, transparent);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* A SPREADSHEET, mid-reconciliation. The cell borders do the recognising;
	   the two error rows do the arguing. */
	.mt-sheet {
		position: relative;
		display: grid;
		gap: 0;
		font-size: 0.5rem;
	}
	.mt-srow {
		display: grid;
		grid-template-columns: 1.4rem repeat(4, 1fr);
	}
	.mt-srow > span {
		padding: 0.22rem 0.3rem;
		border-right: 1px solid color-mix(in srgb, var(--text-primary) 10%, transparent);
		border-bottom: 1px solid color-mix(in srgb, var(--text-primary) 10%, transparent);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mt-shead > span,
	.mt-sn {
		background: color-mix(in srgb, var(--text-primary) 6%, transparent);
		color: color-mix(in srgb, var(--text-primary) 54%, transparent);
		text-align: center;
	}
	.mt-serr > span:last-child {
		color: #b4341f;
		font-weight: 680;
	}
	/* The cell somebody is still in. It is the only thing on this surface
	   that is alive, and it is not being typed into. */
	.mt-scell {
		position: absolute;
		left: calc(1.4rem + (100% - 1.4rem) * 0.5);
		top: 3.32rem;
		width: calc((100% - 1.4rem) / 4);
		height: 1.05rem;
		border: 1.5px solid var(--th);
		box-shadow: 0 0 0 1px color-mix(in srgb, var(--th) 30%, transparent);
		pointer-events: none;
	}

	/* A PHOTO FEED — the only surface here that is not work, which is exactly
	   why it belongs: the day leaks into it and it leaks back out. */
	.mt-feedy {
		display: grid;
		gap: 0.3rem;
	}
	.mt-fystories {
		display: flex;
		gap: 0.3rem;
	}
	.mt-fystory {
		width: 20px;
		height: 20px;
		flex: none;
		border-radius: 50%;
		background: color-mix(in srgb, var(--text-primary) 8%, transparent);
		border: 1.5px solid color-mix(in srgb, var(--th) 46%, transparent);
	}
	.mt-fypost {
		display: flex;
		align-items: center;
		gap: 0.3rem;
	}
	.mt-fyava {
		width: 14px;
		height: 14px;
		flex: none;
		border-radius: 50%;
		background: color-mix(in srgb, var(--text-primary) 14%, transparent);
	}
	.mt-fyname {
		font-size: 0.46rem;
		color: color-mix(in srgb, var(--text-primary) 72%, transparent);
	}
	.mt-fyimg {
		height: 88px;
		border-radius: 4px;
		background: linear-gradient(
			152deg,
			color-mix(in srgb, var(--text-primary) 13%, transparent),
			color-mix(in srgb, var(--text-primary) 5%, transparent)
		);
	}
	.mt-fyacts {
		display: flex;
		gap: 0.34rem;
	}
	.mt-fyacts i {
		width: 11px;
		height: 11px;
		border-radius: 3px;
		background: color-mix(in srgb, var(--text-primary) 16%, transparent);
	}
	.mt-fycap {
		font-size: 0.48rem;
		color: color-mix(in srgb, var(--text-primary) 60%, transparent);
	}

	/* A PROFESSIONAL NETWORK. Three notifications that are not about you, and
	   one message you have been meaning to answer for six days. */
	/* `.mt-net` is defined ONCE, above, with the three-column layout. The rule
	   that used to live here was the notification-list version and, being
	   later in the sheet, its `gap` and `font-size` quietly beat the new one —
	   which is why widening the columns kept appearing to do nothing. */
	.mt-nrow,
	.mt-nmsg {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.32rem 0.3rem;
		border-bottom: 1px solid color-mix(in srgb, var(--text-primary) 8%, transparent);
	}
	.mt-nava {
		width: 16px;
		height: 16px;
		flex: none;
		border-radius: 50%;
		background: color-mix(in srgb, var(--text-primary) 13%, transparent);
	}
	.mt-nh {
		flex: 1;
		min-width: 0;
		display: grid;
		gap: 1px;
		font-weight: 620;
		overflow: hidden;
	}
	.mt-nsub {
		font-style: normal;
		font-weight: 400;
		font-size: 0.46rem;
		color: color-mix(in srgb, var(--text-primary) 54%, transparent);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mt-nmsg {
		border-bottom: none;
		background: color-mix(in srgb, var(--th) 7%, transparent);
	}
	.mt-ndot {
		width: 6px;
		height: 6px;
		flex: none;
		border-radius: 50%;
		background: var(--th);
	}

	/* A DESIGN TOOL, open on an untitled document. The template rail is full;
	   the artboard is four rectangles and a placeholder. */
	.mt-canv {
		display: grid;
		grid-template-columns: 34px 1fr;
		gap: 0.4rem;
	}
	.mt-cvrail {
		display: grid;
		gap: 0.24rem;
		align-content: start;
	}
	.mt-cvthumb {
		height: 22px;
		border-radius: 3px;
		background: color-mix(in srgb, var(--text-primary) 9%, transparent);
	}
	.mt-cvart {
		display: grid;
		align-content: start;
		gap: 0.34rem;
		padding: 0.5rem;
		border-radius: 4px;
		border: 1px solid color-mix(in srgb, var(--text-primary) 12%, transparent);
		background: color-mix(in srgb, var(--text-primary) 3%, transparent);
	}
	.mt-cvtitle {
		height: 12px;
		width: 62%;
		border-radius: 3px;
		background: color-mix(in srgb, var(--text-primary) 18%, transparent);
	}
	.mt-cvsub {
		height: 7px;
		width: 40%;
		border-radius: 3px;
		background: color-mix(in srgb, var(--text-primary) 11%, transparent);
	}
	.mt-cvbox {
		height: 46px;
		border-radius: 3px;
		background: color-mix(in srgb, var(--text-primary) 7%, transparent);
	}
	.mt-cvph {
		font-size: 0.44rem;
		letter-spacing: 0.06em;
		color: color-mix(in srgb, var(--text-primary) 48%, transparent);
	}
	.mt-mrow {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.32rem 0.3rem;
		border-bottom: 1px solid color-mix(in srgb, var(--text-primary) 8%, transparent);
		font-size: 0.56rem;
		line-height: 1.2;
	}
	.mt-mbox {
		width: 8px;
		height: 8px;
		flex: none;
		border-radius: 2px;
		border: 1px solid color-mix(in srgb, var(--text-primary) 30%, transparent);
	}
	.mt-mstar {
		flex: none;
		font-size: 0.6rem;
		color: color-mix(in srgb, var(--text-primary) 30%, transparent);
	}
	.mt-mwho {
		flex: none;
		width: 5.4em;
		font-weight: 680;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	/* Subject bold, snippet dimmed, both on ONE line that clips — the single
	   most recognisable thing about an inbox row. */
	.mt-msubj {
		flex: 1;
		min-width: 0;
		font-weight: 640;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mt-msnip {
		font-style: normal;
		font-weight: 400;
		color: color-mix(in srgb, var(--text-primary) 52%, transparent);
	}
	.mt-mwhen {
		flex: none;
		font-size: 0.5rem;
		color: color-mix(in srgb, var(--text-primary) 58%, transparent);
	}
	/* ── the other three tabs, each the shape of its own kind of work ── */
	.mt-crow {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		padding: 0.42rem 0.3rem;
		border-bottom: 1px solid color-mix(in srgb, var(--text-primary) 8%, transparent);
		font-size: 0.56rem;
	}
	.mt-cava {
		width: 16px;
		height: 16px;
		flex: none;
		border-radius: 50%;
		background: color-mix(in srgb, var(--text-primary) 14%, transparent);
	}
	.mt-cwho {
		flex: none;
		width: 4.6em;
		font-weight: 620;
	}
	.mt-cline {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: color-mix(in srgb, var(--text-primary) 62%, transparent);
	}
	.mt-cdot {
		width: 7px;
		height: 7px;
		flex: none;
		border-radius: 50%;
		background: var(--ha);
	}
	.mt-frow {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.42rem 0.3rem;
		font-size: 0.56rem;
	}
	.mt-frow span {
		flex: none;
		width: 6.4em;
		color: color-mix(in srgb, var(--text-primary) 62%, transparent);
	}
	.mt-frow i {
		flex: 1;
		height: 15px;
		border-radius: 4px;
		background: color-mix(in srgb, var(--text-primary) 9%, transparent);
	}
	.mt-fempty {
		border: 1px dashed color-mix(in srgb, var(--text-primary) 26%, transparent);
		background: transparent !important;
	}
	.mt-fnote {
		padding: 0.3rem;
		font-size: 0.5rem;
		color: color-mix(in srgb, var(--text-primary) 54%, transparent);
	}
	.mt-prow {
		display: flex;
		align-items: baseline;
		gap: 0.5rem;
		padding: 0.42rem 0.3rem;
		border-bottom: 1px solid color-mix(in srgb, var(--text-primary) 8%, transparent);
		font-size: 0.56rem;
	}
	.mt-pwhat {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.mt-pamt {
		font-weight: 620;
	}
	.mt-pwhen {
		flex: none;
		width: 5.2em;
		text-align: right;
		font-size: 0.5rem;
		color: color-mix(in srgb, var(--text-primary) 58%, transparent);
	}
	/* ── the badges: counters, loose, arriving from the right ── */
	.mt-badges {
		position: absolute;
		inset: 0;
		z-index: 1;
		pointer-events: none;
	}
	.mt-badge-c {
		position: absolute;
		left: 50%;
		top: 50%;
		display: inline-flex;
		align-items: baseline;
		gap: 0.4rem;
		white-space: nowrap;
		padding: 0.24rem 0.6rem;
		border-radius: 999px;
		border: 1px solid color-mix(in srgb, var(--film-ink) 26%, transparent);
		background: color-mix(in srgb, var(--film-ink) 10%, transparent);
		--in: clamp(0, calc((var(--local) - var(--t)) * var(--v)), 1);
		--ie: calc(1 - (1 - var(--in)) * (1 - var(--in)));
		--rest-x: calc(var(--rx) + var(--pile-clearance, 0px));
		opacity: clamp(0, calc(var(--in) * 4), 1);
		/* LEFT-ALIGNED, the mirror of .mt-exo-act's right-alignment: the
		   counters' inner edge is the straight one, and their ragged edge
		   falls outward. */
		transform: translate(0, -50%)
			translate(
				calc(var(--rest-x) + (var(--ax) - var(--rest-x)) * (1 - var(--ie))),
				calc(var(--ry) + (var(--ay) - var(--ry)) * (1 - var(--ie)))
			)
			scale(calc(0.84 + var(--ie) * 0.16));
	}
	.mt-badge-n {
		font-style: normal;
		font-size: 0.9rem;
		font-weight: 640;
		letter-spacing: -0.02em;
		color: var(--film-ink);
		font-variant-numeric: tabular-nums;
	}
	.mt-badge-l {
		font-style: normal;
		font-family: var(--lp-font);
		font-size: 0.6rem;
		color: color-mix(in srgb, var(--film-ink) 74%, transparent);
	}
	/* The document has no arrivals to read, so the badges stack in a column
	   beside the window rather than piling on its centre. */
	.mt-scene.static .mt-badges {
		inset: 50% 8% auto auto;
		transform: translateY(-50%);
		display: grid;
		gap: 0.4rem;
		justify-items: end;
	}
	.mt-scene.static .mt-badge-c {
		position: static;
		transform: none;
		opacity: 1;
	}
	.mt-scene.static .mt-tab {
		flex-grow: 1;
		padding-inline: 0.3rem;
		opacity: 1;
		transition: none;
	}

	/* What each act LEFT BEHIND, arriving. The layer spans the station so
	   the chips have somewhere to travel from; each is born far out at
	   --ax/--ay and DECELERATES into a resting place at --rx/--ry beside the
	   person. Nothing fades out at the end of its flight — that is the
	   inversion the whole station turns on. --v is still the spread of
	   rates, so five things arriving read as arrivals rather than as a row
	   of captions fading up in unison. */
	.mt-exo-acts {
		position: absolute;
		inset: 0;
		z-index: 1;
		pointer-events: none;
	}
	.mt-exo-act {
		position: absolute;
		left: 50%;
		top: 50%;
		white-space: nowrap;
		font-size: 0.6rem;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: color-mix(in srgb, var(--film-ink) 80%, transparent);
		border: 1px solid color-mix(in srgb, var(--film-ink) 26%, transparent);
		background: color-mix(in srgb, var(--film-ink) 10%, transparent);
		border-radius: 999px;
		padding: 0.26rem 0.62rem;
		--in: clamp(0, calc((var(--local) - var(--t)) * var(--v)), 1);
		/* ease-out: fast out of nowhere, gentle into the pile */
		--ie: calc(1 - (1 - var(--in)) * (1 - var(--in)));
		--rest-x: calc(var(--rx) - var(--pile-clearance, 0px));
		opacity: clamp(0, calc(var(--in) * 4), 1);
		/* RIGHT-ALIGNED, not centred, and this is what keeps the column off
		   the window. These labels run from 14 to 33 characters; centring
		   each on its own --rx makes the column's INNER edge a function of
		   label length, so the long ones reach across the gutter and sit
		   under the browser frame. Anchoring the chip's right edge gives the
		   column one straight inner margin at --rx and puts the ragged edge
		   on the outside, where there is nothing to collide with. */
		transform: translate(-100%, -50%)
			translate(
				calc(var(--rest-x) + (var(--ax) - var(--rest-x)) * (1 - var(--ie))),
				calc(var(--ry) + (var(--ay) - var(--ry)) * (1 - var(--ie)))
			)
			scale(calc(0.84 + var(--ie) * 0.16));
	}
	/* The document has no arrival to read, so the chips stack as a list —
	   but the layer stays OUT OF FLOW. The plate is a fixed-height grid
	   whose other pieces are all absolute; drop a real column into it and it
	   opens a second row that overflows the plate and lands the chips on the
	   station below. */
	.mt-scene.static .mt-exo-acts {
		inset: 50% auto auto 8%;
		transform: translateY(-50%);
		display: grid;
		gap: 0.5rem;
		justify-items: start;
	}
	.mt-scene.static .mt-exo-act {
		position: static;
		opacity: 1;
		transform: none;
	}
	/* The flow, scrub-driven and INWARD: two streams converging on the
	   person from both edges of the frame. They used to run the other way
	   and had somewhere to go; these only have somewhere to land. */
	.mt-exo-dot {
		--d: clamp(0, calc((var(--local) - var(--t0)) / 0.16), 1);
		position: absolute;
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: color-mix(in srgb, var(--film-ink) 72%, transparent);
		box-shadow: 0 0 10px color-mix(in srgb, var(--film-ink) 36%, transparent);
		left: calc(4vw + 42vw * var(--d));
		top: calc(28% + var(--d) * 20%);
		opacity: calc(clamp(0, calc(var(--d) * 8), 1) - clamp(0, calc((var(--d) - 0.85) * 6.7), 1));
	}
	.mt-exo-dot2 {
		left: calc(96vw - 42vw * var(--d));
		top: calc(74% - var(--d) * 22%);
	}
	.mt-scene.static .mt-exo-dot {
		display: none;
	}
	/* Directly under the device, because that is where such a prompt
	   actually appears — over the thing you were using, not off in a corner.
	   --clicked is the whole second half of its life: the dialog is pressed,
	   and then it FALLS AWAY INTO OBLIVION (down, small, gone) rather than
	   politely dismissing, because that is the lie the gesture tells — the
	   box stopped existing, the work did not. */
	.mt-darkmodal {
		position: absolute;
		left: 50%;
		bottom: 3%;
		/* An ELEVATED surface with the page's own dark ink, not the station's
		   light-on-gloom ink: the station reads white-on-dim, and a dialog
		   painted in that recipe is white text on a near-white card. A
		   dialog is the one thing on a dulled page that is still fully lit,
		   which is also true of the real ones. */
		/* Fully opaque, not 96%: it sits directly over the phone's dark
		   screen, and any transparency at all puts a black rectangle behind
		   its own question. */
		background: var(--bg-elevated, var(--landing-bg));
		border: 1px solid var(--film-card-border);
		border-radius: 12px;
		padding: 0.65rem 0.9rem;
		color: var(--text-primary);
		font-size: 0.78rem;
		font-family: var(--lp-font);
		box-shadow: 0 24px 50px -18px rgba(0, 0, 0, 0.75);
		--dmin: clamp(0, calc((var(--local) - 0.5) * 14), 1);
		--clicked: clamp(0, calc((var(--local) - 0.7) / 0.1), 1);
		opacity: calc(var(--dmin) * (1 - var(--clicked)));
		transform: translateX(-50%)
			translateY(calc((1 - var(--dmin)) * 14px + var(--clicked) * 40px))
			scale(calc(1 - var(--clicked) * 0.34));
		filter: blur(calc(var(--clicked) * 5px));
	}
	.mt-scene.static .mt-darkmodal {
		position: static;
		margin: 0.6rem auto 0;
		width: max-content;
		opacity: 1;
		transform: none;
		filter: none;
	}
	.mt-dm-accept {
		position: relative;
	}
	/* The station's one moving part: a mouse that has to be driven, which is
	   the film's whole thesis restated as a gesture. It is a CHILD of the
	   button and pinned at 50%/50%, so the tip (the glyph's own 0,0) is on
	   the control's centre by construction. It only flies IN. */
	.mt-exo-cursor {
		position: absolute;
		left: 50%;
		top: 50%;
		z-index: 4;
		width: 13px;
		height: 19px;
		/* It lives ON the dialog, so it takes the dialog's ink, not the
		   station's — a white pointer on a white card is not a pointer. */
		background: var(--text-primary);
		clip-path: polygon(0 0, 100% 62%, 55% 62%, 72% 100%, 55% 100%, 42% 68%, 0 86%);
		filter: drop-shadow(0 2px 4px rgba(0, 0, 0, 0.3));
		--ap: clamp(0, calc((var(--local) - 0.56) / 0.14), 1);
		--ape: calc(1 - (1 - var(--ap)) * (1 - var(--ap)) * (1 - var(--ap)));
		--squash: clamp(0, calc(1 - (var(--local) - 0.7) / 0.03), 1);
		opacity: calc(clamp(0, calc(var(--ap) * 6), 1) * (1 - clamp(0, calc((var(--local) - 0.74) * 14), 1)));
		transform: translate(calc((1 - var(--ape)) * 15vw), calc((1 - var(--ape)) * -13vh))
			scale(calc(1 - var(--ap) * var(--squash) * 0.24));
		transform-origin: 0 0;
	}
	.mt-scene.static .mt-exo-cursor {
		display: none;
	}
	.mt-darkmodal p {
		margin: 0 0 0.45rem;
	}
	.mt-darkmodal-row {
		display: flex;
		align-items: center;
	}
	/* An ordinary button, deliberately. A glowing blue "Allow all" was a
	   villain's control; "Later" is the plainest possible one, because
	   nobody is doing anything to you here. */
	.mt-dm-accept {
		background: color-mix(in srgb, var(--text-primary) 10%, transparent);
		border: 1px solid color-mix(in srgb, var(--text-primary) 24%, transparent);
		color: var(--text-primary);
		border-radius: 8px;
		padding: 0.28rem 1.1rem;
		font-size: 0.72rem;
	}
	.mt-dm-later {
		color: color-mix(in srgb, var(--text-primary) 42%, transparent);
		font-size: 0.62rem;
		margin-left: 0.7rem;
	}

	@media (max-width: 640px) {
		/* THE DROWNING ON A PHONE. The layer's geometry is declared in vw, and
		   at 390px the frame is narrower than the chips are wide — so
		   arrivals that LAND (rather than leaving, as the exodus's did) end
		   up parked on top of the device and on top of the title. Three
		   moves, all of them subtraction:

		     · the two stacks go. At this width they are 60px slivers whose
		       job — bulk closing in — cannot be read anyway, and they are the
		       only piece here that is atmosphere rather than argument.
		     · the arrivals stop travelling and become a column at the left
		       edge, fading in one at a time on their own --v. The motion is
		       lost; the accumulation, which is the point, is not.
		     · the backlog pins to the right edge and drops a size.

		   The result is the same beat told in two columns around the device
		   instead of a field converging on it. */
		.mt-exo-acts {
			inset: 14% auto auto 2vw;
			display: grid;
			gap: 0.3rem;
			justify-items: start;
		}
		.mt-exo-act {
			position: static;
			transform: none;
			font-size: 0.5rem;
			letter-spacing: 0.08em;
			padding: 0.18rem 0.44rem;
			opacity: clamp(0, calc((var(--local) - var(--t)) * var(--v) * 3), 1);
		}
		/* The device steps aside rather than the other way round. */
		.mt-you {
			transform: translateX(-4vw);
		}
		.mt-feed {
			width: 104px;
			height: 152px;
		}

		/* THE REBUILT DROWNING, ON A PHONE. Everything above was written for
		   the exodus this beat replaced, and it names none of the pieces that
		   are actually here now — a full-bleed inbox and counters that fly in
		   and LAND. Measured at 390px: the window spans 0–343 of a 375px
		   frame, and the badges (207–358), the leavings (−8–161) and the
		   backlog (188–319) all landed on top of it. Three layers and the
		   window occupying one 200px column, so the beat read as mush.

		   The desktop composition works because the window is narrower than
		   the stage and everything else lands in the margins. A phone has no
		   margins, so the field has to become a column — same doctrine as
		   the rules above, subtraction first:

		     · the LEAVINGS go, and only them. They and the backlog say the
		       same thing in nearly the same words — "+11 tabs you meant to
		       read" against "17 tabs you meant to read" — so on a phone one
		       of the two is pure duplication. The backlog is the one that
		       stays, because it is the one that names what is still OWED.
		       (The window's body is a third thing and keeps its own room:
		       it lists money with dates, which nothing else here does.)
		     · the counters queue UNDER the window instead of over it, and
		       the exodus stops being a fixed-height stage so the column can
		       have the room. They still arrive one at a time on their own
		       --v, which is the only part of the choreography carrying the
		       argument — the flight was never the point, the pile-up is. */
		.mt-exo-acts {
			display: none;
		}
		.mt-exodus {
			height: auto;
			min-height: min(48vh, 30rem);
			align-content: center;
			/* The three rows size to their own content, so without this the
			   counters' second row and the backlog's first line end up seven
			   pixels apart and read as one block. */
			row-gap: 0.7rem;
		}
		.mt-you {
			transform: none;
		}
		.mt-badges {
			position: static;
			display: flex;
			flex-wrap: wrap;
			justify-content: center;
			gap: 0.3rem 0.36rem;
			margin-top: 0.7rem;
		}
		.mt-badge-c {
			position: static;
			/* The travel terms are gone with the absolute placement; the
			   settle is not, because it is what makes each one ARRIVE. */
			transform: scale(calc(0.84 + var(--ie) * 0.16));
			padding: 0.2rem 0.5rem;
		}
		.mt-badge-n {
			font-size: 0.78rem;
		}
		.mt-badge-l {
			font-size: 0.54rem;
		}
	}

	/* ── ACT I · 4 · RETURN — the reversal; the thread blooms; and the
	   grand entry, in two movements ────────────────────────────────── */
	.mt-return {
		position: relative;
		display: grid;
		justify-items: center;
		align-content: center;
		width: min(94vw, 66rem);
		min-height: clamp(17rem, 46vh, 26rem);
		/* THE SHIFT — one number, shared by every part of the lockup: the
		   subject shrinks on it, the trailing half of line one opens, and line
		   two swaps the setup for the inversion's consequence. */
		--sh: clamp(0, calc((var(--local) - 0.6) / 0.14), 1);
		/* THE EMERGENCE. The lockup does not fade up on the page — it comes
		   OUT of the point the frame just fell into, which is the second half
		   of the implosion and the only reason the collapse means anything.
		   It starts small and at the core's own place (−20px, −12vh from
		   centre, the same offset everything else on this station is pinned
		   to) and grows into its own position as the flare opens. */
		--em: clamp(0, calc((var(--local) - 0.3) / 0.11), 1);
		--eme: calc(1 - (1 - var(--em)) * (1 - var(--em)) * (1 - var(--em)));
		opacity: var(--em);
		transform: translate(calc((1 - var(--eme)) * -20px), calc((1 - var(--eme)) * -12vh))
			scale(calc(0.34 + var(--eme) * 0.66));
	}
	/* Reduced motion has no collapse to emerge from, so the lockup is simply
	   there. */
	.mt-scene.static .mt-return {
		opacity: 1;
		transform: none;
	}
	/* LINE ONE — the subject and verb phrase share ONE baseline, so the
	   finished frame reads “Your computer stopped waiting.” as one line.
	   In movement one the phrase is clamped to zero width, so the row IS
	   just the mark and the grid's centring keeps it dead centre; as --sh
	   opens the clamp, the row grows to the right and re-centres itself,
	   and that re-centring is what carries the mark leftward. Nothing is
	   positioned by hand, so the lockup lands correctly at any width. */
	.mt-lock-1 {
		display: flex;
		align-items: baseline;
		/* a WORD SPACE, not a layout gap — once the mark is the same size and
		   weight as the phrase, anything wider stops reading as a sentence */
		gap: calc(var(--sh) * clamp(0.26rem, 0.6vw, 0.5rem));
		max-width: 100%;
	}
	.mt-return-makes {
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(1.15rem, 2.6vw, 2rem);
		font-weight: 340;
		letter-spacing: -0.012em;
		line-height: 1;
		white-space: nowrap;
		color: var(--rdim);
		/* The clamp IS the choreography: at 0 the phrase does not exist and
		   takes no room; at 1 it is wider than the phrase can ever be, so it
		   sizes to its own content. Opacity trails the opening so the words
		   arrive after the space has been made, never mid-wipe. */
		max-width: calc(var(--sh) * 40rem);
		overflow: hidden;
		--mk: clamp(0, calc((var(--sh) - 0.42) * 2.6), 1);
		opacity: var(--mk);
	}
	/* LINE TWO — one cell, two lines, cross-faded. “stopped waiting.” holds
	   the frame through movement one; “Now it works for you.” takes its spot
	   completes. Sharing the cell is what keeps the mark optically centred in
	   BOTH states. */
	.mt-lock-2 {
		display: grid;
		justify-items: center;
		align-items: start;
		margin-top: clamp(0.4rem, 1.4vh, 0.9rem);
	}
	.mt-return-yours {
		grid-area: 1 / 1;
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(2.2rem, 5.8vw, 4.6rem);
		font-weight: 700;
		letter-spacing: -0.035em;
		line-height: 1.02;
		text-align: center;
		white-space: nowrap;
		background: linear-gradient(
			100deg,
			var(--rink) 22%,
			color-mix(in srgb, var(--th) 70%, var(--rink)) 88%
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		--yr: clamp(0, calc((var(--local) - 0.78) * 10), 1);
		opacity: var(--yr);
		transform: translateY(calc((1 - var(--yr)) * 10px));
	}
	.mt-ignite {
		/* Pinned to the thread's true birthplace (ignitionPoint: −20px,
		   −12vh from station center) so the mote convergence, the DOM spark
		   and the canvas thread's first stroke are ONE point. */
		position: absolute;
		left: calc(50% - 20px);
		top: calc(38% );
		width: 12px;
		height: 12px;
		border-radius: 50%;
		background: var(--th);
		--ig: clamp(0, calc((var(--local) - 0.14) * 5), 1);
		/* Full through the strike (0.32), gone by 0.44 — the frame has to be
		   clear of the spark's bloom before the wordmark owns it. */
		--igout: clamp(0, calc((0.44 - var(--local)) * 10), 1);
		opacity: min(var(--ig), var(--igout));
		transform: translate(-50%, -50%) scale(calc(0.4 + var(--ig) * 1.4));
		box-shadow:
			0 0 calc(var(--ig) * 30px) var(--th),
			0 0 calc(var(--ig) * 110px) 20px var(--th-glow);
	}


	/* ── THE TWO DESKTOPS ─────────────────────────────────────────────
	   A screen is not a void with cards floating in it. 1995 is that teal and
	   a task bar that fills up as the day does; today is a menu bar, a light
	   wallpaper and a dock. Both are drawn to the SHAPE of the thing and
	   nothing else — no logos, no wordmarks, no brand colours beyond the ones
	   everybody already has in their head. */
	.mt-osmac {
		justify-content: stretch;
		align-items: stretch;
		/* The menu bar and the dock are absolutely placed, so the padding is
		   what keeps the window clear of them — and the wallpaper runs the
		   full height of the glass underneath both. */
		background:
			radial-gradient(120% 90% at 20% 0%, #cfe0f0, transparent 60%),
			linear-gradient(170deg, #b9c9dd, #8fa6c2 60%, #6f8aa8);
		padding: 40px 0 48px;
	}
	.mt-osmac-menu {
		position: absolute;
		left: 0;
		right: 0;
		top: 0;
		height: 34px;
		display: flex;
		align-items: center;
		gap: 20px;
		padding: 0 16px;
		background: rgba(255, 255, 255, 0.62);
		backdrop-filter: blur(8px);
		font-family: var(--lp-font);
		font-size: 15px;
		color: rgba(0, 0, 0, 0.78);
	}
	.mt-osmac-menu b {
		width: 13px;
		height: 15px;
		border-radius: 50% 50% 46% 46%;
		background: rgba(0, 0, 0, 0.72);
	}
	.mt-osmac-menu i {
		font-style: normal;
	}
	.mt-osmac-menu u {
		margin-left: auto;
		text-decoration: none;
		font-size: 13px;
	}
	.mt-osmac-dock {
		position: absolute;
		left: 50%;
		bottom: 7px;
		transform: translateX(-50%);
		display: flex;
		gap: 6px;
		padding: 5px 8px;
		border-radius: 12px;
		background: rgba(255, 255, 255, 0.42);
		border: 1px solid rgba(255, 255, 255, 0.6);
		backdrop-filter: blur(10px);
	}
	.mt-osmac-dock i {
		/* SMALLER than a real dock's proportion, deliberately. The browser
		   window fills nearly the whole screen — as it should, that is what
		   a machine in use looks like — so a full-size dock is mostly hidden
		   behind it and reads as a row of half-cropped shapes. At this size
		   the whole row clears the window's bottom edge. */
		width: 26px;
		height: 26px;
		border-radius: 7px;
		background: linear-gradient(160deg, rgba(255, 255, 255, 0.9), rgba(120, 140, 170, 0.75));
		box-shadow: 0 4px 10px -4px rgba(0, 0, 0, 0.4);
	}
	.mt-scene.static .mt-osmac-menu,
	.mt-scene.static .mt-osmac-dock {
		display: none;
	}


	/* THE BROWSER FILLS THE MAC, minus the two pieces of the OS that are not
	   it. It used to be a 460px card floating in the middle of a desktop,
	   which reads as a screenshot of a browser rather than as your machine.
	   A real window is nearly the whole screen and the dock still shows. */
	.mt-osmac .mt-brow {
		width: auto;
		align-self: stretch;
		flex: 1 1 auto;
		min-height: 0;
		margin: 0 30px;
		display: flex;
		flex-direction: column;
	}
	.mt-osmac .mt-brow-body {
		flex: 1 1 auto;
		min-height: 0;
		overflow: hidden;
	}
	/* The station's own label sits under the window, not over the dock. */
	.mt-osmac .mt-you-label {
		display: none;
	}


	/* ── THE IMPLOSION ───────────────────────────────────────────────
	   Two earlier cuts tried to TEAR this frame open — first a bright bar
	   clipped to a torn silhouette, then the gloom itself splitting into two
	   halves carried apart and rotated. The second one was geometrically
	   correct and still did not read, and the reason turned out to be the
	   idea rather than the execution: a tear is something done TO the picture
	   from outside it, and this beat is about the picture BECOMING something
	   else. It also needed a gloom to tear, which is the only reason the
	   drowning was dimming at all.

	   So the frame implodes instead. `placeDepth` collapses the whole
	   drowning scene toward the ignition point on an ease-IN, so it begins as
	   a drift and ends as a fall; `.mt-imp-core` is what it falls into;
	   `.mt-imp-flare` is what comes back out, and the wordmark emerges from
	   that. Nothing is removed so that something else can be shown — the
	   pile-up turns into the thing that handles it, which is the argument.

	   All transform and opacity on four small elements. The only per-frame JS
	   is the collapse, which placeDepth was already writing for every station
	   anyway. */

	/* WHAT THE FRAME FALLS INTO. It draws IN — the opposite of every other
	   reveal on this page — getting smaller, harder and brighter until it is
	   a point, then snaps once and is gone. */
	.mt-imp-core {
		position: absolute;
		left: calc(50% - 20px);
		top: 38%;
		width: 34vmin;
		height: 34vmin;
		z-index: 3;
		pointer-events: none;
		border-radius: 50%;
		--ic: clamp(0, calc(var(--local) / 0.3), 1);
		--flash: clamp(0, calc((var(--local) - 0.285) / 0.02), 1);
		--gone: clamp(0, calc((var(--local) - 0.305) / 0.05), 1);
		background: radial-gradient(
			closest-side,
			#fff,
			var(--th-bright) 26%,
			color-mix(in srgb, var(--th2-bright) 62%, transparent) 52%,
			transparent 72%
		);
		/* cubed: dim and diffuse for most of the fall, then almost all of the
		   brightness arrives in the last moment, with the frame */
		opacity: calc((0.06 + 0.94 * var(--ic) * var(--ic) * var(--ic)) * (1 - var(--gone)));
		transform: translate(-50%, -50%)
			scale(calc((0.44 - 0.4 * var(--ic)) + var(--flash) * 0.62));
		/* soft while it is still gathering, hard the instant it is a point */
		filter: blur(calc((1 - var(--ic)) * (1 - var(--ic)) * 9px));
	}

	/* THE CLOSING RING. Without something travelling INWARD a shrinking frame
	   reads as a zoom-out, not a collapse — so this contracts onto the point
	   while the frame falls into it, and is gone by the flash. */
	.mt-imp-ring {
		position: absolute;
		left: calc(50% - 20px);
		top: 38%;
		width: 96vmin;
		height: 96vmin;
		z-index: 2;
		pointer-events: none;
		border-radius: 50%;
		border: 1.5px solid color-mix(in srgb, var(--th) 58%, transparent);
		--ir: clamp(0, calc(var(--local) / 0.3), 1);
		/* in at the start, out before it reaches the middle */
		opacity: calc(var(--ir) * (1 - var(--ir)) * 3.4);
		transform: translate(-50%, -50%) scale(calc(1 - 0.97 * var(--ir)));
	}

	/* WHAT COMES BACK OUT, and what the wordmark emerges from. It blooms out
	   of the flash, floods the frame, and then SETTLES rather than clearing —
	   the ground the film goes on with is brighter than the one it arrived
	   on, which is the promise of the beat stated in light instead of words. */
	.mt-imp-flare {
		position: absolute;
		left: calc(50% - 20px);
		top: 38%;
		width: 190vw;
		height: 165vh;
		z-index: 0;
		pointer-events: none;
		--if: clamp(0, calc((var(--local) - 0.295) / 0.055), 1);
		/* Settles EARLY, and it has to. The flare is what the wordmark comes
		   out of, so a flare still at full bloom while the mark is arriving
		   is a white wash over the one thing the beat exists to show —
		   measured at local 0.40, the mark was emerging into 0.96 of white.
		   Up fast, down before the mark is solid, and it never clears
		   completely: what is left is the brighter ground the film goes on
		   with, which is the promise of the beat stated in light. */
		--ifall: clamp(0, calc((var(--local) - 0.34) * 5.5), 1);
		opacity: calc(var(--if) * (0.94 - var(--ifall) * 0.82));
		background: radial-gradient(
			closest-side,
			rgba(255, 255, 255, 0.82),
			var(--th-soft) 20%,
			color-mix(in srgb, var(--th2-bright) 34%, transparent) 38%,
			transparent 64%
		);
		transform: translate(-50%, -50%) scale(calc(0.06 + var(--if) * 0.94));
	}
	.mt-scene.static .mt-imp-core,
	.mt-scene.static .mt-imp-ring,
	.mt-scene.static .mt-imp-flare {
		display: none;
	}
	.mt-ignite::after {
		/* the strike: one expanding shock ring where the tear starts */
		content: '';
		position: absolute;
		inset: 0;
		border-radius: 50%;
		border: 1.5px solid var(--th);
		--ring: clamp(0, calc((var(--local) - 0.3) * 4), 1);
		opacity: calc((1 - var(--ring)) * 0.9);
		transform: scale(calc(1 + var(--ring) * 22));
	}
	.mt-scene.static .mt-ignite {
		position: static;
		opacity: 0.9;
		transform: none;
		box-shadow: 0 0 22px var(--th-glow);
	}
	.mt-scene.static .mt-ignite::after {
		display: none;
	}
	.mt-s-tear {
		/* --dk (0..1, written by the rAF) is how much of the loss era's
		   night still covers the ground — the ink crossfades with it. */
		--rink: color-mix(in srgb, #eef0ff calc(var(--dk, 0) * 100%), var(--film-ink));
		--rdim: color-mix(in srgb, #dfe3ff calc(var(--dk, 0) * 100%), var(--film-dim));
	}
	/* MOVEMENT ONE: the subject blooms out of the spark; movement two closes
	   the sentence around it. */
	.mt-return-brand {
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(2.8rem, 7.6vw, 6.2rem);
		font-weight: 740;
		letter-spacing: -0.04em;
		line-height: 0.95;
		background: linear-gradient(
			100deg,
			var(--th) 8%,
			color-mix(in srgb, var(--rink) 55%, var(--th)) 48%,
			#38e1c9 96%
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		filter: drop-shadow(0 0 26px var(--th-soft));
		--bw: clamp(0, calc((var(--local) - 0.34) * 7), 1);
		opacity: var(--bw);
		transform: scale(calc(0.82 + var(--bw) * 0.18))
			translateY(calc((1 - var(--bw)) * 14px));
	}
	/* The mark SHRINKS BY FONT SIZE, not by transform, because line one is a
	   real baseline row: a scaled mark keeps its old layout box and the verb
	   phrase would sit a hundred pixels away from a wordmark that only looks
	   smaller. Interpolating the size instead lets the flex row close up
	   around it and the two halves meet as one line. One short word, one
	   bounded scroll window — the reflow is cheap and it is the only way the
	   lockup is honest. */
	/* The mark shrinks AND THINS onto the line. It arrives as a display
	   wordmark — 8rem, weight 740, tight tracking — and by the end of the
	   shift it is exactly the size, weight and tracking of "makes your
	   computer", because the finished frame is one sentence and a 740-weight
	   word sitting in a 340-weight line does not read as part of it. What
	   keeps the mark the mark is the aurora gradient, not the heft. Geist is
	   a 100–900 variable face, so the weight genuinely interpolates rather
	   than snapping between cut weights.
	   Size by font-size, not transform, because line one is a real baseline
	   row: a scaled mark keeps its old layout box and the phrase would sit
	   a hundred pixels off a wordmark that only looks smaller. */
	.mt-lock-1 .mt-return-brand {
		font-size: calc(
			clamp(2.8rem, 7.6vw, 6.2rem) - var(--sh) * (clamp(2.8rem, 7.6vw, 6.2rem) - clamp(1.15rem, 2.6vw, 2rem))
		);
		font-weight: calc(740 - var(--sh) * 400);
		letter-spacing: calc(-0.04em + var(--sh) * 0.028em);
		line-height: 1;
	}
	.mt-return-built {
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(1.1rem, 2.4vw, 1.8rem);
		font-weight: 340;
		letter-spacing: -0.015em;
		line-height: 1.1;
		text-wrap: balance;
		color: var(--rdim);
		grid-area: 1 / 1;
		--bu: clamp(0, calc((var(--local) - 0.42) * 8), 1);
		opacity: calc(var(--bu) * (1 - var(--sh)));
		transform: translateY(calc((1 - var(--bu)) * 12px));
	}
	/* The two devices sit under the whole composition, centred — a footnote
	   to the promise rather than a competitor for the right half. */
	.mt-return-devices {
		position: absolute;
		left: 50%;
		bottom: 0;
		transform: translateX(-50%);
		display: flex;
		gap: 0.9rem;
		white-space: nowrap;
		opacity: clamp(0, calc((var(--local) - 0.86) * 12), 1);
	}
	.mt-dev {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		font-family: var(--lp-mono);
		font-size: 0.72rem;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--rdim);
		border: 1px solid color-mix(in srgb, var(--th) 35%, transparent);
		border-radius: 999px;
		padding: 0.34rem 0.85rem;
		background: color-mix(in srgb, var(--th) 8%, transparent);
	}
	.mt-dev i {
		position: relative;
		display: inline-block;
	}
	.mt-dev-mac {
		width: 15px;
		height: 10px;
		border: 1.5px solid currentColor;
		border-radius: 2px;
	}
	.mt-dev-mac::after {
		content: '';
		position: absolute;
		left: -3px;
		right: -3px;
		bottom: -3.5px;
		height: 1.5px;
		background: currentColor;
		border-radius: 1px;
	}
	.mt-dev-iphone {
		width: 8px;
		height: 13px;
		border: 1.5px solid currentColor;
		border-radius: 2.5px;
	}
	/* The document has no shift to play: the two movements stack in reading
	   order, which is the same sentence with the choreography taken out. */
	.mt-scene.static .mt-return {
		min-height: 0;
		gap: clamp(0.7rem, 2vh, 1.2rem);
	}
	/* The document has no clamp to open and no cell to cross-fade: line one
	   wraps naturally and line two shows BOTH of its lines, stacked. */
	.mt-scene.static .mt-lock-1 {
		flex-wrap: wrap;
		justify-content: center;
		/* the ride opens this gap on --sh; the document has no --sh, and a
		   wordmark butted against the next word is not a sentence */
		gap: 0 0.5rem;
	}
	.mt-scene.static .mt-return-makes {
		max-width: none;
	}
	.mt-scene.static .mt-lock-2 {
		display: grid;
		gap: 0.4rem;
	}
	.mt-scene.static .mt-return-built,
	.mt-scene.static .mt-return-yours {
		grid-area: auto;
	}
	/* Reading order, not paint order: the sentence completes first — the mark,
	   then what it is not and what it does instead — and the line that
	   supports it follows. In the ride these two share a cell and the order
	   is time. */
	.mt-scene.static .mt-return-built {
		display: none;
	}
	.mt-scene.static .mt-lock-1 .mt-return-brand {
		font-size: clamp(2.8rem, 7.6vw, 6.2rem);
		font-weight: 740;
		letter-spacing: -0.04em;
	}
	.mt-scene.static .mt-return-devices {
		position: static;
		transform: none;
		justify-content: center;
	}
	.mt-scene.static .mt-return-brand,
	.mt-scene.static .mt-return-built,
	.mt-scene.static .mt-return-makes,
	.mt-scene.static .mt-return-yours,
	.mt-scene.static .mt-return-devices {
		opacity: 1;
		transform: none;
	}

	/* ── THE MONTAGE — the first line, paid ────────────────────────
	   The frame has NO BOX until there is footage for it. `.mt-lifeframe`
	   without `.on` is display:none, so an absent reel costs the station
	   nothing at all — not a border, not a reserved rectangle, not a gap in
	   the type. The words simply are the ending until the film lands, and
	   `.mt-life-bare` is what makes that a deliberate composition rather
	   than a smaller one waiting for something. */
	.mt-lifeframe {
		display: none;
	}
	.mt-lifeframe.on {
		display: block;
		position: relative;
		/* NEARLY FULL-BLEED, and the reference is the reason. The montage is the
		   film's payoff and its device is now the SUBJECT of the shot rather
		   than an object in a room — which only works if the screen is big
		   enough to carry real work on it. At the old 48rem cap the screen
		   landed at about 410 CSS px and its content became the mush the plan
		   had always warned about; the fix is not simpler content, it is a
		   bigger frame.

		   The width is ALSO capped by the height it implies. `aspect-ratio` alone
		   loses to a flex parent: the station is a 100svh column and the frame
		   was shrunk to fit beside the closing line, which broke the ratio and
		   pillarboxed the plate inside its own box. Capping width by
		   `62svh × 16/9` means the derived height always fits, and `flex: none`
		   stops the column taking it back. */
		width: min(94vw, 1360px, calc(62svh * 16 / 9));
		aspect-ratio: 16 / 9;
		flex: none;
		margin-bottom: clamp(1.2rem, 3.4vh, 2.2rem);
		overflow: hidden;

		/* NO CARD. A border, a radius and a drop shadow announce a WINDOW —
		   something the page is showing you — and the montage is the film's
		   payoff, not an exhibit inside it. The frame dissolves into the paper
		   instead: two linear masks, intersected, feather all four edges.
		   Nothing else changes; the plate keeps its box and its position.

		   THE FEATHER IS ASYMMETRIC, and it has to be. Left and top are pure
		   plate — sky, court, the person — so they can afford a generous fade.
		   Right and bottom are where the MACHINE is: it stands 4px off the
		   right edge and its deck runs past the bottom on purpose, and a fade
		   as deep as the left one would dissolve it. But it must still reach
		   TRANSPARENT: a fade that stops short — the first pass ended at 0.72
		   alpha — leaves a visible line exactly where the hard edge used to be,
		   which is the whole thing we were removing. So the right and bottom
		   fades are SHORT rather than partial, and they take the last sliver of
		   the machine's bezel and the tip of its deck with them — which reads
		   as the device settling into the paper rather than being cut out.

		   The left and bottom fades are also held back far enough to clear the
		   CAPTION, which lives in that corner and must not be eaten by the
		   thing that softens the corner. */
		--feather: linear-gradient(to right, transparent 0, #000 5.5%, #000 95%, transparent 100%),
			linear-gradient(to bottom, transparent 0, #000 10%, #000 95.5%, transparent 100%);
		-webkit-mask-image: var(--feather);
		mask-image: var(--feather);
		-webkit-mask-composite: source-in;
		mask-composite: intersect;
		/* Arrives with the camera, like every other product shot — but DRIVEN
		   FROM THE SCRIPT, not from `--local`, because the thing that waits on
		   this fade is a JS clock and the two have to be the same number. Zero
		   here so the frame never flashes at full before the first tick. */
		opacity: 0;
	}
	.mt-life {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: clamp(0.5rem, 1.6vh, 0.9rem);
		max-width: min(92vw, 44rem);
		text-align: center;
	}
	.mt-life-line {
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(1.9rem, 5vw, 3.6rem);
		font-weight: 640;
		letter-spacing: -0.04em;
		line-height: 1.03;
		background: linear-gradient(
			100deg,
			var(--film-ink) 16%,
			color-mix(in srgb, var(--th) 78%, var(--film-ink)) 62%,
			color-mix(in srgb, var(--th2) 72%, var(--film-ink))
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
		--ll: clamp(0, calc((var(--local) - 0.42) * 6), 1);
		opacity: var(--ll);
		transform: translateY(calc((1 - var(--ll)) * 12px));
	}
	/* Words-only: the type takes the room the frame was going to have, and
	   the beat closes on a statement instead of on an empty rectangle. */
	.mt-life-bare .mt-life-line {
		font-size: clamp(2.3rem, 6.4vw, 4.8rem);
	}
	/* The one interactive thing inside the film. It arrives after the line
	   it belongs to, and it is quiet — the ask below is still the page's
	   conversion, this is the door to the proof for people who want it. */
	.mt-scene.static .mt-life-line {
		opacity: 1;
		transform: none;
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
		.mt-mass {
			width: 56px;
			height: 40vh;
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
		.mt-feed {
			width: 120px;
			height: 158px;
		}
		.mt-exo-act {
			font-size: 0.48rem;
			padding: 0.2rem 0.45rem;
		}
		.mt-return-brand {
			font-size: clamp(2.2rem, 11vw, 3.2rem);
		}
		/* A phone has no room for "Magican makes your computer" on one line, so
		   line one WRAPS there — the mark keeps its own line and the verb
		   phrase drops under it — and the mark only steps back a little,
		   never to half itself, or the wordmark stops being the thing you
		   are being introduced to. */
		.mt-lock-1 {
			flex-direction: column;
			align-items: center;
			gap: calc(var(--sh) * 0.3rem);
		}
		.mt-lock-1 .mt-return-brand {
			font-size: calc(
				clamp(2.2rem, 11vw, 3.2rem) - var(--sh) *
					(clamp(2.2rem, 11vw, 3.2rem) - clamp(1.25rem, 5.6vw, 1.7rem))
			);
		}
		.mt-return-makes {
			max-width: calc(var(--sh) * 40rem);
			font-size: clamp(0.95rem, 4.4vw, 1.3rem);
		}
		.mt-return-yours {
			font-size: clamp(1.8rem, 9vw, 2.8rem);
		}
		.mt-return-devices {
			flex-wrap: wrap;
			justify-content: center;
		}
		.mt-exo-dot {
			left: calc(50vw - 66px + (66px - 42vw) * var(--d));
		}
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
		.mt-lc-you,
		.mt-lc-magican,
		.mt-caret {
			animation: none;
		}
	}
</style>
