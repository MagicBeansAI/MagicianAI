import { describe, expect, it } from 'vitest';
import { llmStepUsageParts } from './llmStepUsage';

describe('measured model step usage', () => {
	it('shows classification input tokens and known zero cost without inventing cache usage', () => {
		expect(llmStepUsageParts({
			input_tokens: 1200, output_tokens: 0, cost: 0, cache_read_tokens: 0,
			usage_reported: true,
			correlation: { usage_availability: { tokens: true, cost: true, cache_read: false } }
		})).toEqual(['1.2k tok', '$0.000']);
	});

	it('does not display transport placeholder zeroes as measured usage', () => {
		expect(llmStepUsageParts({
			input_tokens: 0, output_tokens: 0, cost: 0, cache_read_tokens: 0,
			usage_reported: false,
			correlation: { usage_availability: { tokens: false, cost: false, cache_read: false } }
		})).toEqual([]);
	});

	it('keeps known cache share and cost for named model events', () => {
		expect(llmStepUsageParts({
			input_tokens: 1000, output_tokens: 250, cache_read_tokens: 800, cost_usd: 0.025
		})).toEqual(['1.3k tok', '80% cached', '$0.025']);
	});
});
