import { describe, it, expect } from 'vitest';
import { decisionModelSql, modelCost } from './decisionModels';

describe('Decision Model costs', () => {
	it('keeps a small Jev charge distinct from free and unknown', () => {
		expect(modelCost(0.000042)).toBe('$0.00004200');
		expect(modelCost(0.00000084)).toBe('$0.00000084');
		expect(modelCost('Decimal(0.000042)')).toBe('$0.00004200');
		expect(modelCost(0)).toBe('$0');
		expect(modelCost(null)).toBe('Unknown');
		expect(modelCost(undefined)).toBe('Unknown');
		expect(modelCost(-1)).toBe('Unknown');
	});
	it('uses the deduplicated shared ledger and reports measurement coverage', () => {
		const sql = decisionModelSql("timestamp_ms >= 100 AND task_id = 'task'");
		expect(sql).toContain('FROM llm_calls');
		expect(sql).toContain("provider LIKE 'decision:%'");
		expect(sql).toContain("task_id = 'task'");
		expect(sql).toContain('COUNT(cost_usd) AS priced_calls');
		expect(sql).toContain('COUNT(cache_read_tokens) AS cache_read_calls');
		expect(sql).not.toContain('COALESCE');
	});
});
