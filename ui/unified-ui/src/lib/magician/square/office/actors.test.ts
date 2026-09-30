import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

import { createActors, loungePoint, socialVenuePoint, walkRoute, type Point } from './actors';
import { buildFloorPlan } from './floorPlan';
import type { CitizenVM, GuildVM } from '../engine/types';

const here = dirname(fileURLToPath(import.meta.url));

const desk = (x: number, y: number): Point => ({ x, y });

const c = (id: string, at: Point = desk(0, 0)) => ({ id, desk: at });

describe('createActors', () => {
	it('keeps a walking actor mid-walk when the roster re-derives', () => {
		const s = createActors({ now: () => 1_000 });
		s.reconcile([c('atlas', desk(4, 8))]);
		s.walkTo('atlas', { x: 10, y: 20 }, 4_000);
		const before = s.get('atlas');
		s.reconcile([c('atlas', desk(4, 8))]);
		expect(s.get('atlas')).toEqual(before);
		expect(s.get('atlas')?.state).toBe('walking');
		expect(s.get('atlas')?.from).toEqual({ x: 4, y: 8 });
		expect(s.get('atlas')?.to).toEqual({ x: 10, y: 20 });
	});

	it('drops actors for departed citizens and seats arrivals at their desk', () => {
		const s = createActors({ now: () => 0 });
		s.reconcile([c('atlas', desk(1, 1)), c('nova', desk(2, 2))]);
		s.walkTo('atlas', { x: 9, y: 9 }, 3_000);
		s.reconcile([c('nova', desk(2, 2)), c('oriole', desk(5, 6))]);
		expect(s.get('atlas')).toBeUndefined();
		expect(s.get('nova')).toMatchObject({
			state: 'at-desk',
			from: { x: 2, y: 2 },
			to: { x: 2, y: 2 }
		});
		expect(s.get('oriole')).toEqual({
			state: 'at-desk',
			from: { x: 5, y: 6 },
			to: { x: 5, y: 6 },
			startedAt: 0,
			until: 0,
			desk: { x: 5, y: 6 },
			busy: false
		});
	});

	it('never leaves an actor walking past its `until`', () => {
		let now = 0;
		const s = createActors({ now: () => now });
		s.reconcile([c('atlas', desk(0, 0))]);
		s.walkTo('atlas', { x: 10, y: 0 }, 4_000);
		now = 4_000;
		expect(s.get('atlas')?.state).not.toBe('walking');
		expect(s.get('atlas')?.to).toEqual({ x: 10, y: 0 });
		now = 9_000;
		expect(s.get('atlas')?.state).not.toBe('walking');
	});

	it('settle returns a walker to their desk', () => {
		const s = createActors({ now: () => 50 });
		s.reconcile([c('atlas', desk(3, 4))]);
		s.walkTo('atlas', { x: 8, y: 8 }, 2_000);
		s.settle('atlas');
		expect(s.get('atlas')).toMatchObject({
			state: 'at-desk',
			from: { x: 3, y: 4 },
			to: { x: 3, y: 4 },
			busy: false
		});
	});

	it('signal stands the actor without inventing a walk', () => {
		const s = createActors({ now: () => 10 });
		s.reconcile([c('atlas', desk(1, 2))]);
		s.signal('atlas');
		expect(s.get('atlas')).toMatchObject({
			state: 'signalling',
			from: { x: 1, y: 2 },
			to: { x: 1, y: 2 },
			busy: true
		});
	});

	it('holds no DOM and no Math.random', () => {
		const src = readFileSync(join(here, 'actors.ts'), 'utf8');
		expect(src).not.toMatch(/\bdocument\b/);
		expect(src).not.toMatch(/\bwindow\b/);
		expect(src).not.toMatch(/Math\.random/);
	});
});

describe('walkRoute', () => {
	const citizen = (id: string, guildId: string): CitizenVM =>
		({
			id,
			name: id,
			guildId,
			guildIds: [guildId],
			vibe: 'idle'
		}) as unknown as CitizenVM;

	const guild = (id: string, memberIds: string[]): GuildVM => ({
		id,
		name: id,
		memberIds,
		activeQuestCount: 0,
		blockedQuestCount: 0,
		deliveredQuestCount: 0
	});

	it('goes desk → own door → corridor → target door → target', () => {
		const plan = buildFloorPlan(
			[citizen('atlas', 'eng'), citizen('nova', 'ops')],
			[guild('eng', ['atlas']), guild('ops', ['nova'])],
			1.75
		);
		const from = plan.seats.find((s) => s.citizenId === 'atlas');
		const to = plan.seats.find((s) => s.citizenId === 'nova');
		const fromRoom = plan.rooms.find((r) => r.id === from?.roomId);
		const toRoom = plan.rooms.find((r) => r.id === to?.roomId);
		expect(from && to && fromRoom && toRoom).toBeTruthy();
		if (!from || !to || !fromRoom || !toRoom) return;

		const route = walkRoute(plan, { x: from.personX, y: from.personY }, { x: to.personX, y: to.personY });
		expect(route.length).toBeGreaterThanOrEqual(3);
		expect(route[0]).toEqual({ x: from.personX, y: from.personY });
		expect(route[route.length - 1]).toEqual({ x: to.personX, y: to.personY });
		const corridorHits = route.filter(
			(p) =>
				p.y >= plan.corridor.y &&
				p.y <= plan.corridor.y + plan.corridor.h &&
				p.x >= plan.corridor.x &&
				p.x <= plan.corridor.x + plan.corridor.w
		);
		expect(corridorHits.length).toBeGreaterThan(0);
		expect(fromRoom.id).not.toBe(toRoom.id);
	});

	it('stays inside the room when the target is in the same room', () => {
		const plan = buildFloorPlan(
			[citizen('atlas', 'commons'), citizen('nova', 'commons')],
			[guild('commons', ['atlas', 'nova'])],
			1.75
		);
		const a = plan.seats.find((s) => s.citizenId === 'atlas');
		const b = plan.seats.find((s) => s.citizenId === 'nova');
		expect(a && b).toBeTruthy();
		if (!a || !b) return;
		const route = walkRoute(plan, { x: a.personX, y: a.personY }, { x: b.personX, y: b.personY });
		expect(route).toEqual([
			{ x: a.personX, y: a.personY },
			{ x: b.personX, y: b.personY }
		]);
	});

	it('pins social venues to the lounge sofa, pantry, and corridor cooler', () => {
		const plan = buildFloorPlan(
			[citizen('atlas', 'commons')],
			[guild('commons', ['atlas'])],
			1.75
		);
		const lounge = plan.rooms.find((r) => r.kind === 'lounge');
		expect(lounge).toBeTruthy();
		if (!lounge) return;
		const atLounge = loungePoint(plan);
		expect(atLounge.x).toBeGreaterThanOrEqual(lounge.x);
		expect(atLounge.x).toBeLessThanOrEqual(lounge.x + lounge.w);
		expect(atLounge.y).toBeGreaterThanOrEqual(lounge.y);
		expect(atLounge.y).toBeLessThanOrEqual(lounge.y + lounge.h);
		expect(socialVenuePoint(plan, 'lounge')).toEqual(atLounge);
		expect(socialVenuePoint(plan, 'pantry')).not.toEqual(atLounge);
		expect(socialVenuePoint(plan, 'cooler')).not.toEqual(atLounge);
	});
});
