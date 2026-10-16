import { describe, expect, it } from 'vitest';
import type { AgentSummary } from '$lib/stores/agentStore';
import type { AgentHealthProjection } from './health';
import { crewLeaderboardRowsFromAgents } from './leaderboard';

function agent(agentId: string, name: string): AgentSummary {
	return {
		agent_id: agentId,
		name,
		status: 'idle',
		updated_at: 1
	} as AgentSummary;
}

function health(agentId: string, calls: number, spendUsd: number): AgentHealthProjection {
	return {
		agent_id: agentId,
		overall: {
			score: 80,
			inputs: { last_activity_at_ms: 2 }
		},
		rolling_7d: {
			calls,
			spend_usd: spendUsd,
			success_rate: 0.9,
			score_average: 75,
			score_delta: 2
		}
	} as AgentHealthProjection;
}

describe('crew leaderboard projection', () => {
	it('ranks by seven-day calls, then spend, and assigns stable numeric ranks', () => {
		const agents = [agent('alpha', 'Alpha'), agent('beta', 'Beta'), agent('gamma', 'Gamma')];
		const healthMap = new Map([
			['alpha', health('alpha', 2, 20)],
			['beta', health('beta', 5, 10)],
			['gamma', health('gamma', 5, 30)]
		]);

		const rows = crewLeaderboardRowsFromAgents(agents, healthMap);

		expect(rows.map((row) => row.id)).toEqual(['gamma', 'beta', 'alpha']);
		expect(rows.map((row) => row.rank)).toEqual([1, 2, 3]);
	});

	it('keeps unknown activity below observed zero activity', () => {
		const rows = crewLeaderboardRowsFromAgents(
			[agent('unknown', 'Unknown'), agent('observed', 'Observed')],
			new Map([['observed', health('observed', 0, 0)]])
		);

		expect(rows.map((row) => row.id)).toEqual(['observed', 'unknown']);
		expect(rows[1].calls7d).toBeNull();
	});

	it('uses canonical Attention identity when resolving dashboard status', () => {
		const rows = crewLeaderboardRowsFromAgents(
			[agent('alpha', 'Alpha')],
			null,
			null,
			new Set(['alpha'])
		);

		expect(rows[0].vibe).toBe('needs');
	});
});
