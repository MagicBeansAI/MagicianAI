/**
 * Per-citizen actor state, owned outside the render pass.
 *
 * The floor re-derives from a fresh fleet-state snapshot every 30s. A walk
 * spans that. This store is keyed by citizen id and is *reconciled* against a
 * new roster rather than rebuilt from it, so an unchanged walker keeps their
 * `from`/`to`/`startedAt`/`until` and does not teleport mid-stride.
 *
 * No DOM, no unseeded chance, no clock of its own — the caller injects `now`.
 */

import type { FloorPlan, Room } from './floorPlan';

export type ActorState = 'at-desk' | 'walking' | 'away' | 'signalling';

export interface Point {
	x: number;
	y: number;
}

export interface Actor {
	state: ActorState;
	from: Point;
	to: Point;
	startedAt: number;
	until: number;
	desk: Point;
	/** Event-driven motion. Ambient must not pre-empt a busy actor. */
	busy: boolean;
}

export interface RosterEntry {
	id: string;
	desk: Point;
}

export interface Actors {
	reconcile(roster: readonly RosterEntry[]): void;
	walkTo(id: string, to: Point, durationMs: number, opts?: { busy?: boolean }): void;
	settle(id: string): void;
	signal(id: string): void;
	get(id: string): Actor | undefined;
	all(): Map<string, Actor>;
	position(id: string): Point | undefined;
}

export function createActors(options: { now?: () => number } = {}): Actors {
	const now = options.now ?? (() => Date.now());
	const actors = new Map<string, Actor>();

	const copy = (actor: Actor): Actor => ({
		...actor,
		from: { ...actor.from },
		to: { ...actor.to },
		desk: { ...actor.desk }
	});

	const finishWalk = (actor: Actor): void => {
		if (actor.state !== 'walking') return;
		if (now() < actor.until) return;
		actor.from = { ...actor.to };
		actor.state = near(actor.to, actor.desk) ? 'at-desk' : 'away';
		if (actor.state === 'at-desk') actor.busy = false;
	};

	const current = (actor: Actor): Point => {
		if (actor.state !== 'walking') return { ...actor.to };
		const span = actor.until - actor.startedAt;
		if (span <= 0) return { ...actor.to };
		const t = clamp((now() - actor.startedAt) / span, 0, 1);
		return lerp(actor.from, actor.to, t);
	};

	return {
		reconcile(roster) {
			const next = new Map<string, Actor>();
			for (const entry of roster) {
				const existing = actors.get(entry.id);
				if (existing) {
					finishWalk(existing);
					existing.desk = { ...entry.desk };
					if (existing.state === 'at-desk' || existing.state === 'signalling') {
						existing.from = { ...entry.desk };
						existing.to = { ...entry.desk };
					}
					next.set(entry.id, existing);
				} else {
					next.set(entry.id, {
						state: 'at-desk',
						from: { ...entry.desk },
						to: { ...entry.desk },
						startedAt: now(),
						until: 0,
						desk: { ...entry.desk },
						busy: false
					});
				}
			}
			actors.clear();
			for (const [id, actor] of next) actors.set(id, actor);
		},

		walkTo(id, to, durationMs, opts) {
			const actor = actors.get(id);
			if (!actor) return;
			finishWalk(actor);
			const from = current(actor);
			const startedAt = now();
			actor.state = 'walking';
			actor.from = from;
			actor.to = { ...to };
			actor.startedAt = startedAt;
			actor.until = startedAt + Math.max(0, durationMs);
			actor.busy = opts?.busy ?? true;
		},

		settle(id) {
			const actor = actors.get(id);
			if (!actor) return;
			actor.state = 'at-desk';
			actor.from = { ...actor.desk };
			actor.to = { ...actor.desk };
			actor.startedAt = now();
			actor.until = actor.startedAt;
			actor.busy = false;
		},

		signal(id) {
			const actor = actors.get(id);
			if (!actor) return;
			actor.state = 'signalling';
			actor.from = { ...actor.desk };
			actor.to = { ...actor.desk };
			actor.startedAt = now();
			actor.until = actor.startedAt;
			actor.busy = true;
		},

		get(id) {
			const actor = actors.get(id);
			if (!actor) return undefined;
			finishWalk(actor);
			return copy(actor);
		},

		all() {
			const out = new Map<string, Actor>();
			for (const [id, actor] of actors) {
				finishWalk(actor);
				out.set(id, copy(actor));
			}
			return out;
		},

		position(id) {
			const actor = actors.get(id);
			if (!actor) return undefined;
			finishWalk(actor);
			return current(actor);
		}
	};
}

const NEAR = 2;

function near(a: Point, b: Point): boolean {
	return Math.abs(a.x - b.x) <= NEAR && Math.abs(a.y - b.y) <= NEAR;
}

function clamp(v: number, lo: number, hi: number): number {
	return Math.max(lo, Math.min(hi, v));
}

function lerp(a: Point, b: Point, t: number): Point {
	return { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
}

/** Plan units per millisecond — a room-to-room walk lands in a few seconds. */
export const WALK_SPEED = 0.09;

export function hopDuration(from: Point, to: Point, speed = WALK_SPEED): number {
	const dx = to.x - from.x;
	const dy = to.y - from.y;
	const dist = Math.sqrt(dx * dx + dy * dy);
	return Math.max(280, Math.round(dist / speed));
}

/**
 * A route derived from the plan, not pathfound: desk → own door → corridor →
 * target door → target. Same-room trips stay inside the room.
 */
export function walkRoute(plan: FloorPlan, from: Point, to: Point): Point[] {
	const fromRoom = roomContaining(plan, from);
	const toRoom = roomContaining(plan, to);
	if (fromRoom && toRoom && fromRoom.id === toRoom.id) {
		return dedup([from, to]);
	}
	const points: Point[] = [{ ...from }];
	if (fromRoom) {
		const door = doorIntoCorridor(fromRoom);
		points.push(door);
		points.push(corridorAlign(plan, door));
	} else {
		points.push(corridorAlign(plan, from));
	}
	if (toRoom) {
		const door = doorIntoCorridor(toRoom);
		points.push(corridorAlign(plan, door));
		points.push(door);
	} else {
		points.push(corridorAlign(plan, to));
	}
	points.push({ ...to });
	return dedup(points);
}

export function deskPoint(plan: FloorPlan, citizenId: string): Point | null {
	const seat = plan.seats.find((s) => s.citizenId === citizenId);
	return seat ? { x: seat.personX, y: seat.personY } : null;
}

export function taskRouterPoint(plan: FloorPlan): Point {
	return {
		x: plan.wall + 28,
		y: plan.corridor.y + plan.corridor.h / 2
	};
}

export function meetingPoints(plan: FloorPlan): [Point, Point] {
	const room = plan.rooms.find((r) => r.kind === 'meeting');
	const table = room?.props.find((p) => p.kind === 'meeting-table');
	if (table) {
		return [
			{ x: table.x - table.w * 0.28, y: table.y },
			{ x: table.x + table.w * 0.28, y: table.y }
		];
	}
	if (room) {
		return [
			{ x: room.content.x + room.content.w * 0.35, y: room.content.y + room.content.h / 2 },
			{ x: room.content.x + room.content.w * 0.65, y: room.content.y + room.content.h / 2 }
		];
	}
	const mid = { x: plan.corridor.x + plan.corridor.w / 2, y: plan.corridor.y + plan.corridor.h / 2 };
	return [mid, { x: mid.x + 24, y: mid.y }];
}

export function pantryPoint(plan: FloorPlan): Point {
	const room = plan.rooms.find((r) => r.kind === 'pantry');
	const coffee = room?.props.find((p) => p.kind === 'coffee');
	if (coffee) return { x: coffee.x + 18, y: coffee.y + 16 };
	if (room) return { x: room.content.x + room.content.w / 2, y: room.content.y + room.content.h / 2 };
	return { x: plan.corridor.x + 80, y: plan.corridor.y + plan.corridor.h / 2 };
}

export function coolerPoint(plan: FloorPlan): Point {
	const cooler =
		plan.props.find((p) => p.kind === 'cooler') ??
		plan.rooms.flatMap((r) => r.props).find((p) => p.kind === 'cooler');
	if (cooler) return { x: cooler.x + 16, y: cooler.y };
	return { x: plan.corridor.x + plan.corridor.w / 2, y: plan.corridor.y + plan.corridor.h / 2 };
}

export function loungePoint(plan: FloorPlan): Point {
	const room = plan.rooms.find((r) => r.kind === 'lounge');
	const sofa = room?.props.find((p) => p.kind === 'sofa');
	if (sofa) return { x: sofa.x, y: sofa.y - 18 };
	if (room) {
		return { x: room.content.x + room.content.w / 2, y: room.content.y + room.content.h / 2 };
	}
	return { x: plan.corridor.x + plan.corridor.w * 0.72, y: plan.corridor.y + plan.corridor.h / 2 };
}

export type SocialVenue = 'cooler' | 'pantry' | 'lounge';

export function socialVenuePoint(plan: FloorPlan, venue: SocialVenue): Point {
	switch (venue) {
		case 'cooler':
			return coolerPoint(plan);
		case 'pantry':
			return pantryPoint(plan);
		case 'lounge':
			return loungePoint(plan);
	}
}

function roomContaining(plan: FloorPlan, point: Point): Room | null {
	for (const room of plan.rooms) {
		if (
			point.x >= room.x &&
			point.x <= room.x + room.w &&
			point.y >= room.y &&
			point.y <= room.y + room.h
		) {
			return room;
		}
	}
	return null;
}

function doorIntoCorridor(room: Room): Point {
	const x = room.door.x + room.door.w / 2;
	return room.band === 'north'
		? { x, y: room.door.y + 12 }
		: { x, y: room.door.y - 12 };
}

function corridorAlign(plan: FloorPlan, point: Point): Point {
	return { x: point.x, y: plan.corridor.y + plan.corridor.h / 2 };
}

function dedup(points: Point[]): Point[] {
	const out: Point[] = [];
	for (const point of points) {
		const prev = out[out.length - 1];
		if (!prev || !near(prev, point)) out.push(point);
	}
	return out.length > 0 ? out : points;
}
