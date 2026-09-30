// The movie's world — stations, camera, and the thread's path.
//
// The 2026-08 rebuild replaced scene-swapping with ONE continuous space:
// every chapter is a STATION positioned in a single world on the app's
// theme ground, and a
// camera (translate3d + scale, compositor-only) flies between them along
// a waypoint dolly track driven by global scroll progress. The protagonist
// is the ASK — a living aurora thread whose path through that world is a
// p-keyed spline defined here. This module is the pure math half: station
// geometry, camera interpolation, and thread sampling, kept free of DOM so
// the choreography numbers are testable and reviewable in one place.
//
// The 2026-08-05 restructure gives the film ONE grievance. It used to carry
// two: the tools history and 1995 set up a LABOUR thesis ("every tool still
// required you"), and then the exodus changed the subject to OWNERSHIP
// ("your data became their assets") — so the reveal answered the first
// argument while the payoff line answered the second. The exodus is now
// re-aimed at WORK — and given NO VILLAIN: the same machinery (motes,
// receipts, an accumulating mass) now carries tabs, replies, forms and
// follow-ups arriving faster than anyone clears them, and it reads as
// fatigue rather than menace, because nobody did this to you. The privacy
// argument is not relocated, it is DELETED from the spine; the static trust
// section already answers "can I let it in", which is all privacy has to do.
//
// One beat was added to make the labour spine land, and one whole act left
// the main scroll:
//   · `life` — a contained montage of devices working untouched while
//     people live, and the closing line that answers the greeting.
//   · the DAY (knows → night) is no longer in the main scroll. It is road C
//     of the fork ("A day with it"), an opt-in exploration in the same beat
//     grammar as the other two roads.
//
// THE FREEDOM PAYOFF IS NOT IN HERE. "Love what you do." → the thread passes
// → "Do what you love." is its own scrubbed section after this film
// (Greeting.svelte), rather than a station with a weight. The film itself
// opens directly on the drowning now that `born` (1977) and `operate`
// (1995) — the history-of-computing arc — are cut;
// the two generated prologue reels remain in the repository but are not part
// of the root-page runtime or network path.
//
// The thread is the page's continuous visual motif. It now ghosts into the
// drowning, then converges on the TEAR's ignition and blooms into the fully
// alive protagonist the reveal explains. Treating the pre-reveal line as a
// quiet possibility rather than withholding it entirely keeps the
// three-screen film visually connected after the longer day sequence moved
// out of the main track and the two machine-era acts were cut. From the
// reveal it runs one leg into the montage, where it ties closed.
//
// Coordinate convention: station positions are declared in vw (x) and
// vh (y) so the world scales with the viewport; the component converts to
// px once per resize (x additionally scaled by a mobile lateral factor —
// phones travel a mostly-vertical dolly). All camera/thread math below
// operates in px.

import { boundsFromWeights, clamp, resolveScene } from './scrub';
import type { AuroraPhaseName } from './auroraPalette';
import { MOTIF_HANDOFF } from './motifFlow';

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

// Three stations, one argument. `born` (1977, era `crt`) and `operate`
// (1995, era `grey`) — the history-of-computing arc — are CUT: the owner's
// call was that "the whole orchestra of the machines is not useful anymore",
// so the two acts that argued from that history are gone. The film now
// opens directly on the drowning, inverts that relationship, and closes on
// the montage. `drown` was written to conclude a labour argument the cut
// acts had been building since 1977 ("every tool still required you"); it
// now opens the film cold, on the same premise stated nowhere else. That is
// a deliberate, unresolved seam left for the owner — see the cut's plan
// notes, not a rewrite made here. The former `still` station repeated what
// "You became the glue between every app" had already established, so the
// pile-up flows directly into the implosion.
//
// The declaration is NOT a station. "Your computer, yours again." used to
// hold a beat of its own; it became the reveal's second movement, and in
// the 2026-08-05 restructure that movement changed register with the rest
// of the film: the payoff is TOOL → STAFF, not ownership. The period that
// smolders and flies into the rip sits on the drowning's closing line.
export const STATIONS: Station[] = [
	// THE DROWNING — the old exodus's machinery, aimed at the right
	// grievance. What multiplies is not what they learn about you, it is
	// what is left FOR you: tabs, replies, forms, follow-ups, half-finished
	// work arriving faster than anyone clears it.
	//
	// AND IT HAS NO VILLAIN. That is a design rule, not a nicety. The
	// exodus had an antagonist and a cold platform blue to paint it in;
	// this station is `grey` and reads as FATIGUE, because the honest
	// version of the story is that nobody did this to you — the same
	// capability that made the machine powerful is what buries you. An
	// invented enemy would also hand the trust section an argument it
	// should never have had to make.
	//
	// The motes reverse with it: they fly INTO the person and pile up
	// instead of streaming away toward an edge mass.
	//
	// THE HEAVIEST STATION IN ACT I, at weight 6.4, and the weight IS the
	// argument. This beat has to be exhausting to scroll, not merely
	// described as exhausting — nine surfaces open and close in it, 28 tabs
	// crowd into one strip, eight counters climb and ten kinds of leaving
	// arrive, and every one of those rates is eased so the last third lands
	// faster than anyone can read it. At 3.2 the whole thing was over before
	// it could accumulate, which made it a list rather than a pile-up.
	// x = 0, and the tear shares it. The station's layer is a full 100vw
	// wide with pieces pinned to both its edges — the arrivals on the left,
	// the backlog on the right — so any lateral offset pushes one of them
	// off the viewport. Centred, both edges are on screen, and the tear that
	// happens in this same spot breaks the frame down its actual middle.
	{ id: 'drown', clock: 'Today', title: 'You became the glue between every app', phase: 'graphite', era: 'grey', label: 'the drowning', weight: 6.4, x: 0, y: 434 },
	// THE IMPLOSION — the relationship reverses, and the quiet motif BLOOMS. The
	// pile-up does not get torn open or cleared away: it FALLS INTO the
	// single point it has been converging on all beat, flashes, and the new
	// relationship comes back out of that point. One thing becoming another, which is
	// the argument; two earlier cuts tore the frame instead, and a tear is
	// something done to the picture from outside it. Then the
	// reveal, in TWO movements: “Your computer / stopped waiting.”, then the
	// lockup closing into “Your computer stopped waiting. / Now it works for
	// you.” Movement one has to be allowed to just STAND there and be read.
	// SAME WORLD POINT AS THE DROWNING, deliberately. The camera does not
	// travel here: the screen the visitor is already looking at is the one
	// that collapses, so there is no leg to fly and `drown` never departs
	// (TRAVEL_START 1). MovieTrack collapses the drowning's own scene toward
	// the ignition point across this station's first third, because two
	// stations at one point are otherwise both fully lit and stacked.
	{ id: 'tear', clock: '', title: '', aria: 'Your computer stopped waiting. Now it works for you.', phase: 'violetSurge', weight: 4.4, x: 0, y: 434 },
	// The answer to the greeting: a CONTAINED montage (never full-bleed) of
	// devices working untouched while people live — device sharp in the
	// foreground, people blurred behind, and nobody touching or looking at a
	// screen in any frame — and then the closing line. Generated footage
	// lands in static/prologue/reel-life/; until it does, the closing line
	// carries the beat alone rather than standing over an empty rectangle.
	// The two share a station because the line has to be read in the same
	// breath as the picture; separating them later is a data edit.
	// WEIGHT 25.2 — nine times what it started at, and tripled twice on the
	// same reasoning. The montage is 30.5 seconds of footage across six
	// scenes, each running a different surface of the product through several
	// jobs; at 2.8 it went past in about the time the first plate took to
	// establish itself, and at 8.4 the screens were readable but the scenes
	// still had to be caught rather than watched.
	//
	// A station's weight IS its scroll duration, and this station is the
	// film's PAYOFF — it is allowed to be the longest thing in it. At 25.2 it
	// is roughly half the whole track, which is the correct proportion for the
	// beat every other station has been building toward: six lives, six
	// surfaces, and the only stretch of the film a visitor is meant to dwell
	// in rather than travel through.
	{ id: 'life', clock: '', title: '', aria: 'Devices working while people live, and the closing line', phase: 'tealSpeaking', weight: 25.2, x: 0, y: 658 }
];

/** Station index by id — the ONE way any code names a station. Numeric
 *  literals rot the moment a station is inserted; these never do. */
export const SI: Record<string, number> = Object.fromEntries(
	STATIONS.map((s, i) => [s.id, i])
);

export const BOUNDS = boundsFromWeights(STATIONS.map((s) => s.weight));
export const TRACK_SVH = STATIONS.reduce((sum, s) => sum + s.weight, 0) * 100;

// Where, inside each station's local 0..1, the camera stops dwelling and
// begins the leg to the next station.
//
// DROWN HAS NO LEG. drown and tear stand at ONE world point — born and
// operate, which used to share it and age there, are cut — so drown's
// entry is 1: the camera never travels off it, because the implosion
// happens to the exact frame the visitor is already looking at, not to a
// new one flown in from elsewhere.
//
// THE IMPLOSION STILL DEPARTS: `life` is the only station left that is
// somewhere else (y 658 against drown/tear's 434), so tear → life is a real
// journey. Set to 1 with drown, the camera would sit still through the
// reveal and the montage would simply replace it — the reveal's last line
// and the film's answer to it would arrive as a cut. At 0.92 the camera
// holds for both movements of the reveal, then flies, which is also the
// only pull-back left to resolve the dwell push.
export const TRAVEL_START: number[] = [1, 0.92, 1];

// Lateral bow (vw) applied to a leg's midpoint. All legs run straight
// since the tutor became a real station; the hook stays for any future
// curved pass.
export const LEG_BOW: number[] = STATIONS.map(() => 0);

// Travel signage: road signs the camera flies past. Only ONE survives — the
// invitation out of the reveal and into the montage that earns the later
// freedom payoff. The born→operate sign
// ("It answered. It never helped.") was a bridge across a leg, and Act I no
// longer has legs to bridge; its argument moved into the machine itself,
// which ages in place while the person keeps operating it.
export const SIGNAGE: Vec[] = [{ x: 3, y: 606 }];

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
		// THE DWELL PUSH ONLY EXISTS IF THERE IS A LEG TO RESOLVE IT. A
		// station the camera flies INTO eases 5% closer while it dwells, and
		// the leg out pulls that back — one breath in, one out.
		//
		// Act I's stations have no leg (TRAVEL_START 1, all at one point), so
		// the push had nothing to undo it: z ramped 1 → 1.05 across a
		// station and SNAPPED back to 1 at the next one. Measured at the
		// born → operate hand-over, the machine jumped 4.7% — read as the
		// next computer arriving smaller than the last, when in fact the
		// camera had lurched. A station that never departs simply holds.
		const holds = ts >= 1;
		const d = holds ? 0 : ts > 0 ? Math.min(1, local / ts) : 1;
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

/** The world point the pile-up pulls back INTO — the motif's bloom point. */
export function ignitionPoint(positions: Vec[], h: number): Vec {
	return { x: positions[SI.tear].x - 20, y: positions[SI.tear].y - 0.12 * h };
}

/**
 * The thread's whole journey, keyed to global p. Offsets are declared as
 * (fraction of a card half-width, fraction of viewport height) so the same
 * choreography lands on phones and ultrawides: cards cap at ~34rem, so a
 * px-capped half-width is the one honest horizontal unit inside a station.
 *
 * The motif has already left the hero's `+` when this path begins. It ghosts
 * into the drowning — the film's first station now that `born` and `operate`
 * are cut — converges on `marks.ignite`, then crosses the proof once. It
 * must not orbit each screen or close a decorative ring around the montage:
 * repetition makes intelligence look like a loading indicator instead of a
 * continuing presence.
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
	/** Keypoint authored in the current camera's normalized screen space. */
	const ks = (n: number, local: number, point: Vec): void => {
		pts.push({
			p: pAt(n, local),
			x: positions[n].x + (point.x - 0.5) * w,
			y: positions[n].y + (point.y - 0.5) * h
		});
	};

	// ── THE DROWNING — the hero's one line continues into the film ───────
	// born and operate used to carry a broad S across their act before the
	// thread ever reached here; both are cut, so the thread now simply
	// becomes visible as it ghosts into the film's first station. `motifIn`
	// and `pile` are therefore the same gesture, not two.
	marks.motifIn = pAt(SI.drown, 0.03);
	marks.pile = marks.motifIn;
	k(SI.drown, 0.03, 0.56, 0.2);
	k(SI.drown, 0.48, 0.82, -0.06);
	k(SI.drown, 0.9, 0.35, -0.25);
	k(SI.drown, 0.96, 0.18, -0.18);

	// ── RETURN — the pile-up strikes the point and the motif blooms ──────
	const ign = ignitionPoint(positions, h);
	marks.ignite = pAt(SI.tear, 0.32);
	k(SI.tear, 0.08, -0.45, -0.02);
	k(SI.tear, 0.2, -0.2, -0.1);
	pts.push({ p: marks.ignite, x: ign.x, y: ign.y });
	pts.push({ p: pAt(SI.tear, 0.38), x: ign.x + 0.4 * half, y: ign.y + 0.045 * h });
	pts.push({ p: pAt(SI.tear, 0.45), x: ign.x - 0.35 * half, y: ign.y + 0.115 * h });
	pts.push({ p: pAt(SI.tear, 0.55), x: positions[SI.tear].x + 0.3 * half, y: positions[SI.tear].y + 0.21 * h });
	// The leg: past the "So you can:" board. The return departs at 0.92 (it
	// plays two movements), so the sign keypoint rides after.
	kw(pAt(SI.tear, 0.96), SIGNAGE[0].x - 3, SIGNAGE[0].y + 6);

	// ── LIFE — pass the proof; do not decorate it ─────────────────────────
	// The bright line leaves the inversion, drops beneath the montage, and
	// settles into the quiet channel BETWEEN the montage and its closing line,
	// and continues forward. Keeping the visible arc in that channel is
	// deliberate: the earlier path jumped from far above the montage to far
	// below it, so a screen-length trail became a fishing line through both the
	// picture and headline. A former ellipse circled the screen and tied itself
	// closed; visually it repeated the same orbit grammar the operator-era
	// screens had already used and made the motif feel procedural.
	k(SI.life, 0.05, -1.0, -0.18);
	k(SI.life, 0.16, -0.9, 0.12);
	k(SI.life, 0.3, -0.62, 0.21);
	marks.montage = pAt(SI.life, 0.42);
	k(SI.life, 0.42, -0.2, 0.23);
	k(SI.life, 0.64, 0.35, 0.22);
	ks(SI.life, 0.84, { x: 0.72, y: 0.68 });
	ks(SI.life, 0.92, MOTIF_HANDOFF.moviePenultimate);
	ks(SI.life, 1, MOTIF_HANDOFF.movieExit);

	// Strictly increasing p — coincident keypoints would divide by zero in
	// the sampler; a nudge is invisible at these densities.
	for (let i = 1; i < pts.length; i++) {
		if (pts[i].p <= pts[i - 1].p) pts[i].p = pts[i - 1].p + 1e-4;
	}

	// The line now clears each object instead of circling and submerging under
	// it. Keep the depth contract for future genuinely occluded passages, but
	// this root-film path has none.
	const behind: DepthSpan[] = [];

	// No strands: the plan's fan-out belonged to the ask station, and the
	// ask station is road C now. The type stays — a future beat may want
	// diverging strands again — and an empty list costs the draw path a
	// loop that never runs.
	return { main: pts, strands: [], marks, behind };
}
