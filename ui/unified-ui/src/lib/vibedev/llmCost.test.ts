import { describe, expect, it } from 'vitest';

import {
	buildVibeDevProjectRollupSql,
	buildVibeDevModelBreakdownSql,
	buildVibeDevRunRollupSql,
	buildVibeDevTotalRollupSql
} from './llmCost';

// One project attributing a chat session + two tasks => three key rows.
const attributions = [
	{ projectId: 'proj-1', chatSessionId: 'chat-1', taskIds: ['task-a', 'task-b'] }
] as Parameters<typeof buildVibeDevProjectRollupSql>[0];

describe('vibedev llmCost SQL builders', () => {
	it('emits a SELECT/UNION ALL key set, never a VALUES clause', () => {
		// The governed legacy-LLM read guard rejects VALUES-shaped query bodies;
		// a rejected query in the dashboard batch takes down every widget.
		for (const sql of [
			buildVibeDevProjectRollupSql(attributions, 7),
			buildVibeDevModelBreakdownSql(attributions, 7),
			buildVibeDevTotalRollupSql(attributions, 7)
		]) {
			expect(sql).toBeTruthy();
			expect(sql).not.toMatch(/\bVALUES\b/i);
			expect(sql).toContain('UNION ALL');
			expect(sql).toContain('WITH project_keys(project_id, key_type, key_value) AS (SELECT');
		}
	});

	it('returns null when there are no attributions (nothing enters the batch)', () => {
		expect(buildVibeDevProjectRollupSql([], 7)).toBeNull();
	});

	it('reads the daily spend summary, summing its call counts, when asked', () => {
		const lifetime = buildVibeDevProjectRollupSql(attributions, 0, [], 'daily') ?? '';
		expect(lifetime).toContain('FROM llm_cost_daily WHERE TRUE');
		expect(lifetime).not.toMatch(/\bllm_calls\b/);
		expect(lifetime).toContain('SUM(calls) AS calls');
		expect(lifetime).not.toContain('COUNT(*)');

		const runs =
			buildVibeDevRunRollupSql([{ runId: 'run-1', taskIds: ['task-a'] }], 3, [], 'daily') ?? '';
		expect(runs).toMatch(/FROM llm_cost_daily WHERE dt >= '\d{4}-\d{2}-\d{2}'/);
		expect(runs).not.toMatch(/\bllm_calls\b/);
	});

	it('keeps reading every call by default', () => {
		const sql = buildVibeDevProjectRollupSql(attributions, 7) ?? '';
		expect(sql).toContain('FROM llm_calls WHERE');
		expect(sql).toContain('1 AS calls');
	});
});
