/**
 * Ambient office life — pantry trips, cooler stops, a corridor crossing,
 * and standing about a step off the desk.
 *
 * Rate is a function of vibe: idle crew mill, working crew mostly stay put.
 * Position therefore becomes a soft status channel (away usually means not
 * working) but with licence, so the rule sometimes breaks and the floor does
 * not read as mechanical.
 *
 * Ambient must never pre-empt an event-driven walk (`actor.busy`), and the
 * caller must not tick at all while the tab is hidden.
 */

import type { CitizenVM } from '../engine/types';
import {
	coolerPoint,
	pantryPoint,
	type Actor,
	type Point
} from './actors';
import type { FloorPlan } from './floorPlan';

/**
 * Intended feel, one place.
 *
 * An 11-person idle floor should look like an office at low tide — someone
 * is usually on their feet, never a parade. A working person stays put
 * unless the licence roll fires, which is rare enough that it reads as a
 * person rather than a rule.
 *
 * Chances are per ambient tick (see `tickEveryMs`), not per minute.
 */
export const AMBIENT = {
	/** How often we consider a trip while the tab is visible. */
	tickEveryMs: 2_400,
	/** Idle crew: roughly one in four ticks, a trip is offered. */
	idleTripChance: 0.22,
	/** Working crew: almost never, unless the licence roll fires. */
	workingTripChance: 0.03,
	/** The rule-break: a working person still gets up sometimes. */
	workingLicence: 0.07,
	/** How long they stay at the cooler / pantry / corridor before coming back. */
	lingerMs: { min: 3_200, max: 7_400 }
} as const;

export function shouldTickAmbient(visibility: DocumentVisibilityState): boolean {
	return visibility === 'visible';
}

export function pickAmbientTrip(input: {
	citizen: CitizenVM;
	actor: Actor;
	plan: FloorPlan;
	rng: () => number;
}): Point | null {
	const { citizen, actor, plan, rng } = input;
	if (actor.busy) return null;
	if (actor.state === 'walking' || actor.state === 'signalling') return null;

	const roll = rng();
	if (citizen.vibe === 'working') {
		if (roll >= AMBIENT.workingTripChance && rng() >= AMBIENT.workingLicence) return null;
	} else if (citizen.vibe === 'needs' || citizen.vibe === 'offline') {
		return null;
	} else if (roll >= AMBIENT.idleTripChance) {
		return null;
	}

	const dests = [
		pantryPoint(plan),
		coolerPoint(plan),
		corridorCrossing(plan, rng),
		standAbout(actor.desk, rng)
	];
	const pick = dests[Math.floor(rng() * dests.length)] ?? dests[0];
	if (near(pick, actor.desk) && pick !== dests[3]) {
		return dests.find((p) => !near(p, actor.desk)) ?? dests[3];
	}
	return pick;
}

export function lingerDuration(rng: () => number): number {
	const span = AMBIENT.lingerMs.max - AMBIENT.lingerMs.min;
	return Math.round(AMBIENT.lingerMs.min + rng() * span);
}

function corridorCrossing(plan: FloorPlan, rng: () => number): Point {
	const { corridor } = plan;
	return {
		x: corridor.x + corridor.w * (0.25 + rng() * 0.5),
		y: corridor.y + corridor.h / 2
	};
}

/** A few steps off the desk — standing about, not a trip. */
function standAbout(desk: Point, rng: () => number): Point {
	return {
		x: desk.x + (rng() - 0.5) * 40,
		y: desk.y + 16 + rng() * 14
	};
}

function near(a: Point, b: Point): boolean {
	return Math.abs(a.x - b.x) < 8 && Math.abs(a.y - b.y) < 8;
}
