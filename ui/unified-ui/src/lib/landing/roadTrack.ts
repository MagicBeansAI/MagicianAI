// A ROAD'S WORLD — the fork's continuations as scrubbed film, not documents.
//
// The film's stations are hand-placed because each one is a different subject
// at a different point in an argument. A road is not like that: it is three
// acts of three beats, the same shape every time, so its world is GENERATED
// rather than written down. That is the whole reason this file is thirty
// lines and `worldTrack.ts` is five hundred.
//
// THE ACT SIGNS ARE STATIONS OF THEIR OWN (owner decision, 2026-08-06), which
// is what makes a road twelve stations rather than nine. It matches the film's
// grammar, where signage is something the camera flies PAST between subjects
// rather than a heading printed on one — so an act break becomes a real pause
// in the journey instead of a larger label.

import { boundsFromWeights } from './scrub';

export interface RoadStation {
	kind: 'sign' | 'beat';
	/** Relative scroll duration — see the weights below. */
	weight: number;
	/** World position: x in vw, y in vh. */
	x: number;
	y: number;
}

/** A sign is a quick read, not a dwell — closer to a prologue beat than to a
 *  station with a mock in it. A beat has something to be studied in it. */
const SIGN_WEIGHT = 0.8;
const BEAT_WEIGHT = 2;
/**
 * The road's very last beat only (2026-08-17). Every other beat's weight of
 * 2 buys two things: dwell time to read the card AND reveal it (PathBeat's
 * own staggered rows keep animating in until local≈0.72-0.8), then a real
 * camera TRAVEL leg to the next station starting at local=0.8. `roadTravelStart`
 * gives the last beat nowhere to travel to (`ts = 1`, dwell for its ENTIRE
 * span), so at the ordinary weight its full 2 units were ALL dwell: the
 * staggered reveal still only needs local up to ~0.8 of it, so everything
 * past that — the last fifth of the beat's own 2 units, a full extra beat's
 * worth of scroll on top — sat frozen, unchanging, showing nothing new
 * before this station's tail ever let go into whatever comes next. Cut to
 * roughly the reveal's own scroll requirement: still enough room for every
 * staggered row to land at its own unhurried pace, not enough left over to
 * idle in after it has.
 */
const LAST_BEAT_WEIGHT = 1;

/**
 * Where inside a beat's own local the camera stops dwelling and starts moving
 * to the next one — so the fly-off gets `1 - this` of the slot.
 *
 * Was 0.8, giving the departure a fifth of the beat and making a card leave
 * the screen in a snap: measured, the final card went from centred to 700px
 * above the viewport inside one 2.5% step of the whole track. 0.62 nearly
 * doubles the travel without eating much of the dwell, which is where the
 * reading happens.
 */
const BEAT_TRAVEL_START = 0.62;

/** How far apart consecutive stations stand, in vh. */
const STEP_Y = 112;
/** How far a station sits off the centre line, in vw. Signs stand ON it —
 *  the camera passes straight through them — while beats alternate to either
 *  side, which is what gives the road its walk. */
const BEAT_X = 13;

/**
 * Twelve stations: three acts of `sign, beat, beat, beat`.
 *
 * Generated rather than declared so the two roads cannot drift apart, and so
 * a road that grows a fourth act costs one argument rather than a rewrite.
 */
/**
 * `withSigns: false` builds a road of beats alone.
 *
 * A sign station is a camera stop for a `PathActSign`, and a road whose slot
 * content no longer renders those must not still reserve stops for them: the
 * geometry would describe twelve stations while the markup supplied nine, so
 * the camera dwelt at three empty positions and the track carried 2.4 weight
 * -- several screens -- of scroll with nothing on it. PathPromise's acts are
 * now titled by the fixed chrome layer instead of by signs, so it builds
 * without them. PathLifecycle still has its own signs and keeps the default.
 */
export function roadStations(acts = 3, beatsPerAct = 3, withSigns = true): RoadStation[] {
	const out: RoadStation[] = [];
	for (let a = 0; a < acts; a++) {
		if (withSigns) out.push({ kind: 'sign', weight: SIGN_WEIGHT, x: 0, y: out.length * STEP_Y });
		for (let b = 0; b < beatsPerAct; b++) {
			const isLast = a === acts - 1 && b === beatsPerAct - 1;
			out.push({
				kind: 'beat',
				weight: isLast ? LAST_BEAT_WEIGHT : BEAT_WEIGHT,
				// Alternating, and continuing to alternate ACROSS acts rather
				// than resetting — a road that zig-zagged in the same order in
				// every act would read as three copies of one act.
				x: out.length % 2 === 0 ? -BEAT_X : BEAT_X,
				y: out.length * STEP_Y
			});
		}
	}
	return out;
}

export function roadBounds(stations: readonly RoadStation[]): number[] {
	return boundsFromWeights(stations.map((s) => s.weight));
}

export function roadTrackSvh(stations: readonly RoadStation[]): number {
	return stations.reduce((sum, s) => sum + s.weight, 0) * 100;
}

/**
 * Where, inside each station's local, the camera stops dwelling and leaves.
 *
 * A SIGN ALWAYS DEPARTS, and early — it is the one station type that is never
 * a destination, so the camera is already moving on while the visitor reads
 * it. A beat holds most of its own span so its mock can be studied, and the
 * last station never departs because there is nowhere left to go.
 */
export function roadTravelStart(stations: readonly RoadStation[]): number[] {
	return stations.map((s, i) =>
		i === stations.length - 1 ? 1 : s.kind === 'sign' ? 0.45 : BEAT_TRAVEL_START
	);
}
