import { describe, expect, it } from 'vitest';

import {
	STRUCTURED_RESPONSE_ROLLOUT_PERCENT_STEPS,
	STRUCTURED_RESPONSE_ROLLOUT_PERCENT_SAMPLE_BASE,
	hashRolloutSeed,
	shouldEnableStructuredResponseRollout,
	parseRolloutPercent
} from './rollout';

describe('structured response rollout percent parser', () => {
	it('accepts only documented phase-10 rollout stages', () => {
		expect(parseRolloutPercent('0')).toBe(0);
		expect(parseRolloutPercent('1')).toBe(0.01);
		expect(parseRolloutPercent('5%')).toBe(0.05);
		expect(parseRolloutPercent('25')).toBe(0.25);
		expect(parseRolloutPercent('50')).toBe(0.5);
		expect(parseRolloutPercent('100')).toBe(1);
		expect(parseRolloutPercent('10')).toBe(null);
		expect(parseRolloutPercent('foo')).toBe(null);
	});

	it('parses percent values consistently for all defined steps', () => {
		for (const step of STRUCTURED_RESPONSE_ROLLOUT_PERCENT_STEPS) {
			expect(parseRolloutPercent(String(step))).toBe(step / 100);
		}
	});

	it('hashes rollout seeds deterministically for bucketing decisions', () => {
		expect(hashRolloutSeed('stable-seed')).toBe(hashRolloutSeed('stable-seed'));
		expect(hashRolloutSeed('a')).not.toBe(hashRolloutSeed('b'));
	});

	it('short-circuits rollout when percentage is zero', () => {
		expect(shouldEnableStructuredResponseRollout({ seed: 'stable-seed', percent: 0 })).toBe(false);
		expect(shouldEnableStructuredResponseRollout({ seed: 'stable-seed', percent: -0.1 })).toBe(false);
	});

	it('maps percentage to deterministic bucket windows consistently', () => {
		const seed = 'scope:workspace:text:message';
		const sampleBase = 16;
		const percent = 0.25;
		const bucket = hashRolloutSeed(seed) % sampleBase;
		expect(shouldEnableStructuredResponseRollout({ seed, percent, sampleBase })).toBe(bucket < 4);
	});

	it('saturates percentages above 100% and keeps 100% fully enabled', () => {
		expect(shouldEnableStructuredResponseRollout({
			seed: 'any-seed',
			percent: 1,
			sampleBase: STRUCTURED_RESPONSE_ROLLOUT_PERCENT_SAMPLE_BASE,
		})).toBe(true);
		expect(shouldEnableStructuredResponseRollout({
			seed: 'any-seed',
			percent: 1.4,
			sampleBase: STRUCTURED_RESPONSE_ROLLOUT_PERCENT_SAMPLE_BASE,
		})).toBe(true);
	});
});
