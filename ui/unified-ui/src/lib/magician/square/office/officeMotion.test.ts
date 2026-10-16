import { describe, expect, it } from 'vitest';

import { createActors } from './actors';
import { spawnToken, startRoute } from './officeMotion';
import { buildFloorPlan } from './floorPlan';
import type { CitizenVM, GuildVM } from '../engine/types';

const citizen = (id: string, guildId: string): CitizenVM =>
	({
		id,
		name: id,
		guildId,
		guildIds: [guildId],
		vibe: 'idle'
	}) as unknown as CitizenVM;

describe('officeMotion', () => {
	it('replaces a token with the same id rather than stacking it', () => {
		const first = spawnToken([], {
			id: 'task:1',
			kind: 'task',
			title: 'A',
			from: { x: 0, y: 0 },
			to: { x: 10, y: 0 },
			durationMs: 400,
			now: 1
		});
		const next = spawnToken(first, {
			id: 'task:1',
			kind: 'task',
			title: 'B',
			from: { x: 0, y: 0 },
			to: { x: 20, y: 0 },
			durationMs: 400,
			now: 2
		});
		expect(next).toHaveLength(1);
		expect(next[0].title).toBe('B');
		expect(next[0].startedAt).toBe(2);
	});

	it('walks a route hop by hop and arrives', () => {
		const plan = buildFloorPlan(
			[citizen('atlas', 'eng'), citizen('nova', 'ops')],
			[
				{ id: 'eng', name: 'eng', memberIds: ['atlas'], activeQuestCount: 0, blockedQuestCount: 0, deliveredQuestCount: 0 },
				{ id: 'ops', name: 'ops', memberIds: ['nova'], activeQuestCount: 0, blockedQuestCount: 0, deliveredQuestCount: 0 }
			] satisfies GuildVM[],
			1.75
		);
		const from = plan.seats.find((s) => s.citizenId === 'atlas');
		const to = plan.seats.find((s) => s.citizenId === 'nova');
		expect(from && to).toBeTruthy();
		if (!from || !to) return;

		let now = 0;
		const actors = createActors({ now: () => now });
		actors.reconcile([{ id: 'atlas', desk: { x: from.personX, y: from.personY } }]);
		const hops: number[] = [];
		let arrived = false;
		startRoute(
			actors,
			plan,
			'atlas',
			{ x: to.personX, y: to.personY },
			(fn, ms) => {
				hops.push(ms);
				now += ms;
				fn();
			},
			{ onArrive: () => { arrived = true; } }
		);
		expect(arrived).toBe(true);
		expect(hops.length).toBeGreaterThan(0);
		expect(actors.get('atlas')?.state).not.toBe('walking');
	});
});
