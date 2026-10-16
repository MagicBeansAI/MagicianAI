import { describe, expect, it } from 'vitest';
import {
	ACTIVITIES,
	CLOSER,
	CLOSER_INDEX,
	VERB_COLORS,
	colorAt,
	holdMsFor,
	nextIndex
} from './lifeCycle';

describe('landing activity cycle', () => {
	it('opens on a quiet verb and closes on all your side quests', () => {
		expect(ACTIVITIES[0]).toBe('singing');
		expect(ACTIVITIES).toHaveLength(36);
		expect(CLOSER).toBe('all your side quests');
		expect(ACTIVITIES).not.toContain(CLOSER);
	});

	it('accelerates then holds the closer longest', () => {
		expect(holdMsFor(0)).toBe(1000);
		expect(holdMsFor(CLOSER_INDEX - 1)).toBe(160);
		expect(holdMsFor(CLOSER_INDEX)).toBe(2800);
		expect(holdMsFor(12)).toBeGreaterThan(holdMsFor(20));
		expect(holdMsFor(12)).toBeLessThan(holdMsFor(0));
	});

	it('wraps after the closer', () => {
		expect(nextIndex(CLOSER_INDEX - 1)).toBe(CLOSER_INDEX);
		expect(nextIndex(CLOSER_INDEX)).toBe(0);
	});

	it('cycles the persona palette', () => {
		expect(colorAt(0)).toBe(VERB_COLORS[0]);
		expect(colorAt(VERB_COLORS.length)).toBe(VERB_COLORS[0]);
		expect(new Set(VERB_COLORS).size).toBe(10);
	});
});
