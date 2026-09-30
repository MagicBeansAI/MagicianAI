// The DAY's world — the eleven stations of "a day with it", and the camera
// and thread that fly through them.
//
// THIS IS AN EXTRACTION, NOT A REWRITE. Every station position, weight,
// travel point and thread keypoint below is the day exactly as it played in
// the main film up to commit 7a4087161, lifted whole. The 2026-08-05 second
// cut took the day out of the main scroll — the film became one argument and
// the day became the PROOF you opt into — and for a while road C carried a
// static prose summary of it instead, which is not the same artefact: the
// day IS the scroll-scrubbed camera walk, the aurora thread, the five-phase
// orb, the lock-screen wake and the Pythagoras overlay.
//
// What changed in the lift, and only this:
//   · y is rebased by −546 so the day starts at the top of its own world
//     rather than 546vh down the film's.
//   · TRAVEL_START keeps the day's own eleven values (the film's slice 8..18).
//   · The thread is not BORN here. In the film it ignites at the implosion
//     and the day is downstream of that, so `marks.ignite` is 0 — already
//     lit when the road opens — and the return station's ignition keypoints
//     and the travel signage that pointed INTO the day are both gone.
//
// Coordinate convention is the film's: x in vw, y in vh, converted to px
// once per resize by the component.

import { boundsFromWeights, clamp, resolveScene } from './scrub';
import type { AuroraPhaseName } from './auroraPalette';

export interface Vec {
	x: number;
	y: number;
}

export interface Station {
	id: string;
	clock: string;
	title: string;
	/** Accessible name for titleless (image-only) stations. */
	aria?: string;
	phase: AuroraPhaseName;
	/**
	 * Act I era treatment — overrides the aurora accent vars with era truth
	 * (phosphor green, platform grey, cold platform blue). Day stations
	 * carry no era: their phase remaps the selected app theme's accent pair.
	 */
	era?: 'crt' | 'grey' | 'platform';
	/** Kicker label; falls back to the station id. */
	label?: string;
	/** Relative scroll duration — weight 2 holds the viewport twice weight 1. */
	weight: number;
	/** World position: x in vw (pre mobile-factor), y in vh. */
	x: number;
	y: number;
}

// The day, 07:30 → 22:30. Eleven stations, each of them a thing that
// actually happens: it already knows you, delegate from anywhere, it has
// hands, it sits in the meeting so you can leave it, it remembers your goals
// over lunch, messy in and finished out, say it out loud, even from the lock
// screen, it teaches the way a person would, even apps it has never met, and
// it works while you sleep.
export const STATIONS: Station[] = [
	{ id: 'knows', clock: '7:30', title: 'It already knows you', phase: 'armedEmber', weight: 1.4, x: 14, y: 0 },
	{ id: 'ask', clock: '9:04', title: 'Delegate from anywhere', phase: 'violetSurge', label: 'a professional', weight: 1.8, x: -16, y: 112 },
	{ id: 'hands', clock: '9:05', title: 'It has hands', phase: 'violetSurge', label: 'a professional', weight: 1.8, x: 18, y: 224 },
	{ id: 'meeting', clock: '10:30', title: 'It sits in the meeting so you can leave it', phase: 'calmAurora', label: 'a professional', weight: 1.4, x: -14, y: 332 },
	{ id: 'lunch', clock: '13:00', title: 'It remembers your goals', phase: 'amberThinking', weight: 1.8, x: -2, y: 442 },
	{ id: 'thinks', clock: '15:00', title: 'Messy in, finished out', phase: 'amberThinking', weight: 2.0, x: 16, y: 552 },
	{ id: 'orb', clock: '18:00', title: 'Say it out loud', phase: 'violetSurge', weight: 3.0, x: 0, y: 662 },
	{ id: 'pocket', clock: '21:00', title: 'Even from the lock screen', phase: 'violetSurge', weight: 1.8, x: -13, y: 774 },
	{ id: 'tutor', clock: '21:30', title: 'It teaches the way a person would', phase: 'calmAurora', label: 'a parent', weight: 2.0, x: 16, y: 886 },
	{ id: 'zepto', clock: '22:00', title: 'Even apps it’s never met', phase: 'violetSurge', label: 'a parent', weight: 2.0, x: -14, y: 998 },
	{ id: 'night', clock: '22:30', title: 'It works while you sleep', phase: 'tealSpeaking', weight: 1.9, x: 3, y: 1116 }
];

/** Station index by id — the ONE way any code names a station. */
export const SI: Record<string, number> = Object.fromEntries(
	STATIONS.map((s, i) => [s.id, i])
);

export const BOUNDS = boundsFromWeights(STATIONS.map((s) => s.weight));
export const TRACK_SVH = STATIONS.reduce((sum, s) => sum + s.weight, 0) * 100;

// The film's TRAVEL_START, sliced to the day (its indices 8..18) and
// otherwise untouched: thinks holds through the branch-and-recall walk, the
// orb dwells through its whole five-phase walk, the tutor holds until the
// proof has labelled itself on BOTH screens, and the finale never departs.
export const TRAVEL_START: number[] = [
	0.74, 0.74, 0.74, 0.74, 0.74, 0.78, 0.82, 0.72, 0.8, 0.76, 1
];

// Lateral bow (vw) applied to a leg's midpoint. Every leg runs straight;
// the hook stays for any future curved pass.
export const LEG_BOW: number[] = STATIONS.map(() => 0);

/** Global p of a point inside station n's segment. */
export function pAt(n: number, local: number): number {
	return BOUNDS[n] + clamp(local, 0, 1) * (BOUNDS[n + 1] - BOUNDS[n]);
}

function easeInOutCubic(t: number): number {
	return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
}

export interface Camera {
	x: number;
	y: number;
	z: number;
}

/**
 * The dolly: dwell at a station with a slow push-in (1 → 1.05), then ease
 * to the next along a (possibly bowed) straight leg while the zoom dips —
 * pulling back mid-flight so both stations are briefly visible in one
 * frame, which is what makes the world read as continuous. z is exactly 1
 * at every arrival and 1.05 at every departure, so the curve never jumps.
 */
export function cameraAt(p: number, positions: Vec[], bowsPx: number[]): Camera {
	const { scene, local } = resolveScene(BOUNDS, p);
	const ts = TRAVEL_START[scene];
	const cur = positions[scene];
	if (scene >= positions.length - 1 || local <= ts) {
		const d = ts > 0 ? Math.min(1, local / ts) : 1;
		return { x: cur.x, y: cur.y, z: 1 + 0.05 * easeInOutCubic(d) };
	}
	const nxt = positions[scene + 1];
	const t = (local - ts) / (1 - ts);
	const e = easeInOutCubic(t);
	return {
		x: cur.x + (nxt.x - cur.x) * e + bowsPx[scene] * Math.sin(Math.PI * e),
		y: cur.y + (nxt.y - cur.y) * e,
		z: 1.05 + (1 - 1.05) * t - 0.33 * Math.sin(Math.PI * t)
	};
}

// ── The thread ─────────────────────────────────────────────────────────

export interface ThreadPt {
	p: number;
	x: number;
	y: number;
}

export interface Strand {
	/** Visible while the head is inside [p0, p1]; alpha fades at both ends. */
	p0: number;
	p1: number;
	pts: ThreadPt[];
}

/** A p-range where the thread passes BEHIND a station object. */
export interface DepthSpan {
	p0: number;
	p1: number;
}

export interface ThreadPath {
	main: ThreadPt[];
	strands: Strand[];
	/** Named p values the component keys interactions off — one source. */
	marks: Record<string, number>;
	/** Spans drawn on the under-canvas — the thread dives beneath objects. */
	behind: DepthSpan[];
}

/**
 * 0..1 behindness of the thread at p. Each span edge carries a short
 * feather (smoothstepped, capped at a third of the span) so the dive
 * under an object reads as a submerge, never a cut.
 */
export function behindAt(spans: readonly DepthSpan[], p: number): number {
	let b = 0;
	for (const s of spans) {
		const f = Math.min(0.002, (s.p1 - s.p0) / 3);
		if (f <= 0) continue;
		const rise = clamp((p - s.p0) / f, 0, 1);
		const fall = clamp((s.p1 - p) / f, 0, 1);
		const t = Math.min(rise, fall);
		b = Math.max(b, t * t * (3 - 2 * t));
	}
	return b;
}

function catmullRom(p0: Vec, p1: Vec, p2: Vec, p3: Vec, t: number): Vec {
	const t2 = t * t;
	const t3 = t2 * t;
	return {
		x:
			0.5 *
			(2 * p1.x + (-p0.x + p2.x) * t + (2 * p0.x - 5 * p1.x + 4 * p2.x - p3.x) * t2 + (-p0.x + 3 * p1.x - 3 * p2.x + p3.x) * t3),
		y:
			0.5 *
			(2 * p1.y + (-p0.y + p2.y) * t + (2 * p0.y - 5 * p1.y + 4 * p2.y - p3.y) * t2 + (-p0.y + 3 * p1.y - 3 * p2.y + p3.y) * t3)
	};
}

/** World position of the thread at progress p — Catmull-Rom over keypoints. */
export function sampleThread(pts: readonly ThreadPt[], p: number): Vec {
	const n = pts.length;
	if (n === 0) return { x: 0, y: 0 };
	if (p <= pts[0].p) return pts[0];
	if (p >= pts[n - 1].p) return pts[n - 1];
	let lo = 0;
	let hi = n - 2;
	while (lo < hi) {
		const mid = (lo + hi + 1) >> 1;
		if (pts[mid].p <= p) lo = mid;
		else hi = mid - 1;
	}
	const i = lo;
	const span = pts[i + 1].p - pts[i].p;
	const t = span > 0 ? (p - pts[i].p) / span : 1;
	return catmullRom(pts[Math.max(0, i - 1)], pts[i], pts[i + 1], pts[Math.min(n - 1, i + 2)], t);
}

/**
 * The thread's whole journey, keyed to global p. Offsets are declared as
 * (fraction of a card half-width, fraction of viewport height) so the same
 * choreography lands on phones and ultrawides: cards cap at ~34rem, so a
 * px-capped half-width is the one honest horizontal unit inside a station.
 *
 * The thread is BORN at the return station (Act I's reversal): everything
 * before marks.ignite is pre-aurora history and has no thread at all.
 */
export function buildThread(
	positions: Vec[],
	w: number,
	h: number,
	xf: number,
	yf: number
): ThreadPath {
	const vw = w / 100;
	const vh = h / 100;
	const half = Math.min(0.46 * w, 272);
	const pts: ThreadPt[] = [];
	const marks: Record<string, number> = {};

	/** Keypoint at station n: p from local, offsets from station center. */
	const k = (n: number, local: number, fx: number, fy: number): void => {
		pts.push({ p: pAt(n, local), x: positions[n].x + fx * half, y: positions[n].y + fy * h });
	};
	/** Keypoint at an absolute world position (vw/vh units, mobile-scaled). */
	const kw = (p: number, xvw: number, yvh: number): void => {
		pts.push({ p, x: xvw * xf * vw, y: yvh * yf * vh });
	};

	// THE THREAD SIMPLY BEGINS. In the film it is BORN at the implosion and
	// everything before that is pre-aurora history; the day is downstream of
	// that birth, so there is nothing here to ignite — it is already lit when
	// the visitor opens the road, and `marks.ignite` is 0 so every consumer
	// that asks "has the thread been born yet" gets yes.
	marks.ignite = 0;

	// ── 1 · KNOWS — sweep the morning, dive into memory, take a fragment ─
	k(SI.knows, 0.06, -0.85, -0.16);
	k(SI.knows, 0.16, 0.75, -0.09);
	k(SI.knows, 0.26, -0.7, -0.02);
	k(SI.knows, 0.36, 0.5, 0.04);
	marks.memcard = pAt(SI.knows, 0.46);
	k(SI.knows, 0.46, 0.15, 0.13);
	marks.pickup = pAt(SI.knows, 0.55);
	k(SI.knows, 0.55, 0.35, 0.165);
	k(SI.knows, 0.66, -0.35, 0.21);
	k(SI.knows, 0.88, -0.6, 0.5);

	// ── 2 · ASK — doc, then the PHONE (the same ask raised by a long-
	// press), then the desktop composer: "from anywhere", drawn literally
	// as one thread stitching the devices together before it splits.
	k(SI.ask, 0.05, -0.6, -0.2);
	marks.doc = pAt(SI.ask, 0.12);
	k(SI.ask, 0.12, 0.35, -0.16);
	k(SI.ask, 0.16, 0.92, -0.2);
	marks.phone = pAt(SI.ask, 0.2);
	k(SI.ask, 0.2, 1.08, -0.04);
	marks.composer = pAt(SI.ask, 0.27);
	k(SI.ask, 0.27, -0.95, -0.005);
	k(SI.ask, 0.36, -0.4, 0.005);
	k(SI.ask, 0.52, 0.5, 0.005);
	marks.split = pAt(SI.ask, 0.62);
	k(SI.ask, 0.62, 0.1, 0.14);
	k(SI.ask, 0.7, -0.25, 0.185);
	k(SI.ask, 0.9, 0.2, 0.5);

	// ── 3 · HANDS — the fields, the tool line, the knock at the gate ─────
	k(SI.hands, 0.05, -0.5, -0.19);
	k(SI.hands, 0.14, -0.35, -0.125);
	k(SI.hands, 0.26, 0.3, -0.08);
	k(SI.hands, 0.38, -0.25, -0.035);
	k(SI.hands, 0.46, 0, 0.08);
	marks.gate = pAt(SI.hands, 0.5);
	k(SI.hands, 0.5, 0, 0.19);
	k(SI.hands, 0.53, 0, 0.224);
	k(SI.hands, 0.56, 0, 0.196);
	k(SI.hands, 0.59, 0, 0.224);
	marks.approved = pAt(SI.hands, 0.66);
	k(SI.hands, 0.66, 0.05, 0.224);
	k(SI.hands, 0.71, 0.9, 0.23);
	k(SI.hands, 0.9, 0.4, 0.5);

	// ── 4 · MEETING — one loop around the Magican tile, then the decisions ──
	k(SI.meeting, 0.05, 0.5, -0.2);
	marks.tile = pAt(SI.meeting, 0.14);
	const tile = { x: positions[SI.meeting].x + 0.93 * half, y: positions[SI.meeting].y - 0.035 * h };
	const loopR = { x: 0.22 * half, y: 0.05 * h };
	for (let i = 0; i <= 5; i++) {
		const a = -Math.PI / 2 + (i / 5) * Math.PI * 2;
		pts.push({
			p: pAt(SI.meeting, 0.14 + (i / 5) * 0.2),
			x: tile.x + Math.cos(a) * loopR.x,
			y: tile.y + Math.sin(a) * loopR.y
		});
	}
	k(SI.meeting, 0.44, -0.5, 0.0);
	k(SI.meeting, 0.56, 0.1, 0.1);
	k(SI.meeting, 0.66, -0.2, 0.16);
	k(SI.meeting, 0.9, 0.3, 0.5);

	// ── 5 · LUNCH — the Swiggy order runs through memory: the thread
	// crosses the ask, lights the healthy-streak recall, then crosses the
	// reconfirm question before the order is allowed to land ─────────────
	k(SI.lunch, 0.05, -0.55, -0.2);
	k(SI.lunch, 0.15, 0.5, -0.13);
	k(SI.lunch, 0.26, -0.45, -0.06);
	marks.healthy = pAt(SI.lunch, 0.36);
	k(SI.lunch, 0.36, -0.05, 0.02);
	marks.confirm = pAt(SI.lunch, 0.48);
	k(SI.lunch, 0.48, 0.5, 0.07);
	k(SI.lunch, 0.6, -0.3, 0.13);
	k(SI.lunch, 0.72, 0.15, 0.19);
	k(SI.lunch, 0.9, -0.2, 0.5);

	// ── 6 · THINKS — one walk: cross the question, dive into the graph,
	// then down past the cohort evidence to the conclusion ───────────────
	k(SI.thinks, 0.05, -0.7, -0.18);
	k(SI.thinks, 0.16, 0.7, -0.15);
	k(SI.thinks, 0.3, 0, -0.05);
	k(SI.thinks, 0.44, -0.45, 0.06);
	marks.graph = pAt(SI.thinks, 0.52);
	k(SI.thinks, 0.58, 0.55, 0.12);
	// The cited note lights as the head passes the evidence row.
	marks.brief = pAt(SI.thinks, 0.64);
	k(SI.thinks, 0.68, 0, 0.18);
	k(SI.thinks, 0.9, -0.3, 0.5);

	// ── 7 · ORB — the coil: what you have followed IS Magican ──────────────
	k(SI.orb, 0.04, 0, -0.26);
	k(SI.orb, 0.1, -0.3, -0.14);
	marks.coilStart = pAt(SI.orb, 0.18);
	const orbC = { x: positions[SI.orb].x, y: positions[SI.orb].y + 0.01 * h };
	const TURNS = 2.25;
	for (let i = 0; i <= 11; i++) {
		const a = i / 11;
		const ang = -Math.PI / 2 + a * TURNS * Math.PI * 2;
		const r = (1 - a) * 0.155 * h + 4;
		pts.push({
			p: pAt(SI.orb, 0.18 + a * 0.24),
			x: orbC.x + Math.cos(ang) * r * 1.25,
			y: orbC.y + Math.sin(ang) * r
		});
	}
	marks.coilEnd = pAt(SI.orb, 0.42);
	k(SI.orb, 0.5, 0, 0.02);
	marks.emerge = pAt(SI.orb, 0.56);
	k(SI.orb, 0.56, 0, 0.17);
	k(SI.orb, 0.85, -0.3, 0.5);

	// ── 8 · POCKET — the thread dives into the locked phone's island ────
	k(SI.pocket, 0.05, 0, -0.36);
	marks.island = pAt(SI.pocket, 0.12);
	k(SI.pocket, 0.12, 0.02, -0.245);
	marks.pocketIn = pAt(SI.pocket, 0.16);
	k(SI.pocket, 0.2, 0.05, -0.24);
	// Dormant while the phone talks — the island carries the light.
	k(SI.pocket, 0.5, -0.05, -0.235);
	marks.pocketOut = pAt(SI.pocket, 0.56);
	// Exit BESIDE the phone, never through the lock screen.
	k(SI.pocket, 0.62, 0.8, -0.12);
	k(SI.pocket, 0.7, -0.45, 0.14);
	k(SI.pocket, 0.9, 0.3, 0.5);

	// ── 9 · TUTOR — the thread circles the web-and-phone pair like a
	// patient teacher: down the browser's left flank while Magican is
	// summoned, an underline pass beneath the page as the overlay draws,
	// up past the arriving phone, and on into the night. Always OUTSIDE
	// the figure — the proof is the star.
	k(SI.tutor, 0.05, -0.9, -0.27);
	k(SI.tutor, 0.16, -1.3, -0.06);
	k(SI.tutor, 0.32, -1.15, 0.18);
	marks.chalk = pAt(SI.tutor, 0.46);
	k(SI.tutor, 0.46, 0, 0.33);
	k(SI.tutor, 0.6, 0.95, 0.26);
	k(SI.tutor, 0.7, 1.42, -0.02);
	k(SI.tutor, 0.8, 0.7, -0.32);
	k(SI.tutor, 0.92, 0.3, 0.5);

	// ── 10 · ZEPTO — the craving: the thread crosses the ask, lights the
	// first-time MCP connect card, waits out the authorize, then touches
	// the order as it lands ──────────────────────────────────────────────
	k(SI.zepto, 0.05, -0.5, -0.2);
	k(SI.zepto, 0.16, 0.45, -0.12);
	marks.mcp = pAt(SI.zepto, 0.3);
	k(SI.zepto, 0.3, -0.4, -0.04);
	k(SI.zepto, 0.44, 0.05, 0.03);
	marks.zorder = pAt(SI.zepto, 0.58);
	k(SI.zepto, 0.58, -0.25, 0.11);
	k(SI.zepto, 0.72, 0.2, 0.17);
	k(SI.zepto, 0.9, -0.2, 0.5);

	// ── 11 · NIGHT — the deck, then the loop ties: the day, done ─────────
	k(SI.night, 0.05, -0.5, -0.19);
	k(SI.night, 0.14, 0.5, -0.14);
	k(SI.night, 0.24, 0, -0.05);
	marks.receipt = pAt(SI.night, 0.42);
	const rc = { x: positions[SI.night].x, y: positions[SI.night].y + 0.12 * h };
	const er = { x: Math.min(0.3 * w, 340), y: 0.2 * h };
	marks.loopStart = pAt(SI.night, 0.46);
	for (let i = 0; i <= 9; i++) {
		const a = i / 9;
		const ang = Math.PI + a * Math.PI * 2;
		pts.push({
			p: pAt(SI.night, 0.46 + a * 0.34),
			x: rc.x + Math.cos(ang) * er.x,
			y: rc.y + Math.sin(ang) * er.y
		});
	}
	marks.loopEnd = pAt(SI.night, 0.8);
	// Tie the knot where the loop began — the ends meet at the left vertex,
	// in the open space beside the card, and the head rests there.
	pts.push({ p: pAt(SI.night, 0.9), x: rc.x - er.x + 14, y: rc.y - 10 });
	pts.push({ p: 1, x: rc.x - er.x + 4, y: rc.y });

	// Strictly increasing p — coincident keypoints would divide by zero in
	// the sampler; a nudge is invisible at these densities.
	for (let i = 1; i < pts.length; i++) {
		if (pts[i].p <= pts[i - 1].p) pts[i].p = pts[i - 1].p + 1e-4;
	}

	// The plan's three strands: the main thread is one; two more diverge at
	// the split and fade as the chosen strand (the main path) carries on.
	const split = sampleThread(pts, marks.split);
	const strand = (sx: number, sy: number): Strand => ({
		p0: marks.split - 0.004,
		p1: marks.split + 0.05,
		pts: [
			{ p: 0, x: split.x, y: split.y },
			{ p: 0.5, x: split.x + sx * 0.55 * half, y: split.y + sy * 0.4 * h * 0.14 },
			{ p: 1, x: split.x + sx * half, y: split.y + 0.11 * h }
		]
	});

	// Per-segment depth: where the thread dives BEHIND a station object
	// (drawn on the under-canvas). Four tasteful dives: behind the ask
	// station's mini iPhone, under the thinks station's income chart,
	// behind the pocket phone's top edge on approach (the island dive and
	// the orb landing beat stay IN FRONT), and the receipt loop's far arc
	// passing behind the card it circles.
	const behind: DepthSpan[] = [
		{ p0: pAt(SI.ask, 0.165), p1: pAt(SI.ask, 0.24) },
		{ p0: pAt(SI.thinks, 0.56), p1: pAt(SI.thinks, 0.7) },
		{ p0: pAt(SI.pocket, 0.02), p1: pAt(SI.pocket, 0.115) },
		{ p0: pAt(SI.night, 0.46), p1: pAt(SI.night, 0.63) }
	];

	return { main: pts, strands: [strand(1, 1), strand(-1.15, 1)], marks, behind };
}
