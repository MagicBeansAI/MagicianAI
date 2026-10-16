import { describe, expect, it } from 'vitest';

import { planGaplessAudio } from './gaplessAudio';

describe('gapless provider TTS timeline', () => {
	it('schedules every decoded clip exactly at the previous clip end', () => {
		const slots = planGaplessAudio([1.25, 0.5, 2], 10.025);
		expect(slots).toEqual([
			{ startAt: 10.025, endAt: 11.275 },
			{ startAt: 11.275, endAt: 11.775 },
			{ startAt: 11.775, endAt: 13.775 }
		]);
		for (let index = 1; index < slots.length; index += 1) {
			expect(slots[index].startAt).toBe(slots[index - 1].endAt);
		}
	});

	it('cannot move the timeline backwards on invalid provider durations', () => {
		expect(planGaplessAudio([Number.NaN, -1, 0.25], 4)).toEqual([
			{ startAt: 4, endAt: 4 },
			{ startAt: 4, endAt: 4 },
			{ startAt: 4, endAt: 4.25 }
		]);
	});
});
