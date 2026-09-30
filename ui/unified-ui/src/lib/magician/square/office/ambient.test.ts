import { describe, expect, it } from 'vitest';

import { createActors } from './actors';
import { pickAmbientTrip, shouldTickAmbient, AMBIENT } from './ambient';
import { buildFloorPlan } from './floorPlan';
import type { CitizenVM, GuildVM } from '../engine/types';

const citizen = (id: string, vibe: CitizenVM['vibe'] = 'idle'): CitizenVM =>
	({
		id,
		name: id,
		guildId: 'commons',
		guildIds: ['commons'],
		vibe
	}) as unknown as CitizenVM;

const guild = (id: string, memberIds: string[]): GuildVM => ({
	id,
	name: id,
	memberIds,
	activeQuestCount: 0,
	blockedQuestCount: 0,
	deliveredQuestCount: 0
});

describe('ambient', () => {
	const plan = buildFloorPlan(
		[citizen('atlas'), citizen('nova')],
		[guild('commons', ['atlas', 'nova'])],
		1.75
	);

	it('never pre-empts an event-driven walk', () => {
		const actors = createActors({ now: () => 0 });
		actors.reconcile([{ id: 'atlas', desk: { x: 10, y: 10 } }]);
		actors.walkTo('atlas', { x: 40, y: 10 }, 4_000, { busy: true });
		const trip = pickAmbientTrip({
			citizen: citizen('atlas'),
			actor: actors.get('atlas')!,
			plan,
			rng: () => 0
		});
		expect(trip).toBeNull();
	});

	it('lets idle crew trip and usually leaves working crew put', () => {
		const idle = pickAmbientTrip({
			citizen: citizen('atlas', 'idle'),
			actor: {
				state: 'at-desk',
				from: { x: 1, y: 1 },
				to: { x: 1, y: 1 },
				startedAt: 0,
				until: 0,
				desk: { x: 1, y: 1 },
				busy: false
			},
			plan,
			rng: () => 0
		});
		const working = pickAmbientTrip({
			citizen: citizen('nova', 'working'),
			actor: {
				state: 'at-desk',
				from: { x: 2, y: 2 },
				to: { x: 2, y: 2 },
				startedAt: 0,
				until: 0,
				desk: { x: 2, y: 2 },
				busy: false
			},
			plan,
			rng: () => 0.5
		});
		expect(idle).not.toBeNull();
		expect(working).toBeNull();
	});

	it('pauses entirely when the tab is hidden', () => {
		expect(shouldTickAmbient('hidden')).toBe(false);
		expect(shouldTickAmbient('visible')).toBe(true);
	});

	it('keeps rate constants in one place', () => {
		expect(AMBIENT.idleTripChance).toBeGreaterThan(AMBIENT.workingTripChance);
		expect(AMBIENT.workingLicence).toBeGreaterThan(0);
		expect(AMBIENT.workingLicence).toBeLessThan(0.2);
	});
});
