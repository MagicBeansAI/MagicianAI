import { describe, expect, it } from 'vitest';

import { HOW_SHOT_FRAMES, HOW_SHOT_HOLD_MS, howShotFrames, shotCycleIndex } from './howShot';

describe('how-shot frames', () => {
	it('opens the superapp on Today, then Chat, then Tasks', () => {
		expect(HOW_SHOT_FRAMES.superapp.map((f) => f.id)).toEqual(['today', 'chat', 'tasks']);
		expect(HOW_SHOT_FRAMES.superapp.map((f) => f.chrome)).toEqual(['Today', 'Chat', 'Tasks']);
		expect(HOW_SHOT_FRAMES.superapp.map((f) => f.icon)).toEqual(['calendar', 'message', 'check']);
	});

	it('divides machine memory by user, agent, and task episode, then synthesis', () => {
		expect(HOW_SHOT_FRAMES.local.map((f) => f.id)).toEqual(['tiers', 'synthesize']);
	});

	it('cycles enterprise-grade between the password vault and sealed cards', () => {
		expect(HOW_SHOT_FRAMES.security.map((f) => f.id)).toEqual(['password', 'card']);
	});

	it('shows resource authority as ceilings then spend tokens', () => {
		expect(HOW_SHOT_FRAMES.bounds.map((f) => f.id)).toEqual(['ceilings', 'tokens']);
		expect(HOW_SHOT_FRAMES.bounds[0].chrome).toBe('Resource Authority');
	});

	it('adds a crew roster that can work on its own', () => {
		expect(HOW_SHOT_FRAMES.crew.map((f) => f.id)).toEqual(['roster', 'working']);
	});

	it('looks up frames by claim id', () => {
		expect(howShotFrames('superapp')).toBe(HOW_SHOT_FRAMES.superapp);
	});
});

describe('shotCycleIndex', () => {
	it('holds the first frame for the full window', () => {
		expect(shotCycleIndex(0, 3)).toBe(0);
		expect(shotCycleIndex(HOW_SHOT_HOLD_MS - 1, 3)).toBe(0);
	});

	it('advances and wraps', () => {
		expect(shotCycleIndex(HOW_SHOT_HOLD_MS, 3)).toBe(1);
		expect(shotCycleIndex(HOW_SHOT_HOLD_MS * 2, 3)).toBe(2);
		expect(shotCycleIndex(HOW_SHOT_HOLD_MS * 3, 3)).toBe(0);
	});

	it('stays on the only frame when a shot does not cycle', () => {
		expect(shotCycleIndex(HOW_SHOT_HOLD_MS * 4, 1)).toBe(0);
		expect(shotCycleIndex(HOW_SHOT_HOLD_MS * 4, 0)).toBe(0);
	});
});
