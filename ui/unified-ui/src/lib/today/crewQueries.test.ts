import { describe, expect, it, vi } from 'vitest';
import {
	agentCrewState,
	buildCrewLlmSql,
	buildCrewRows,
	crewTotals,
	fetchCrewActivity,
	formatPct,
	parseCrewLlm,
	parseCrewTaskPage,
	pctTone,
	shouldFetchNextTaskPage,
	timestampToMs,
	type CrewTaskRow
} from './crewQueries';

const NOW = Date.parse('2026-09-28T12:00:00Z');
const SINCE = NOW - 86_400_000;
const at = (hoursAgo: number) => NOW - hoursAgo * 3_600_000;

describe('crewQueries', () => {
	it('builds the grouped 24h SQL with a numeric literal', () => {
		const sql = buildCrewLlmSql(SINCE);
		expect(sql).toContain(`timestamp_ms >= ${SINCE}`);
		expect(sql).toContain('GROUP BY agent_id ORDER BY cost_usd DESC');
		expect(sql).toContain("response_kind = 'harness_aggregate'");
		expect(buildCrewLlmSql(Number.NaN)).toContain('timestamp_ms >= 0');
	});

	it('parses model-use rows by column name, coercing string numbers', () => {
		const rows = parseCrewLlm({
			columns: ['cost_usd', 'agent_id', 'calls', 'ok_calls', 'avg_latency_ms'],
			rows: [
				['0.063', 'pilot', '22', 21, null],
				[0, '  ', 3, 3, 10]
			]
		});
		expect(rows).toEqual([{ agentId: 'pilot', calls: 22, costUsd: 0.063, okCalls: 21, avgLatencyMs: null }]);
		expect(parseCrewLlm({ columns: [], rows: [[1]] })).toEqual([]);
	});

	it('parses task pages and normalises timestamps', () => {
		const page = parseCrewTaskPage({
			tasks: [
				{ agent_id: 'pilot', status: 'completed', updated_at: '2026-09-28T11:00:00Z' },
				{ agent_id: '', status: 'failed', updated_at: 1_790_000_000 }
			],
			pagination: { next_cursor: 'abc' }
		});
		expect(page.nextCursor).toBe('abc');
		expect(page.rows[0]).toEqual({ agentId: 'pilot', status: 'completed', updatedAtMs: at(1) });
		expect(page.rows[1].agentId).toBeNull();
		expect(page.rows[1].updatedAtMs).toBe(1_790_000_000_000);
		expect(timestampToMs('nope')).toBeNaN();
		expect(parseCrewTaskPage(null)).toEqual({ rows: [], nextCursor: null });
	});

	it('walks task pages only while the last row is inside the window', () => {
		const inside: CrewTaskRow[] = [{ agentId: 'a', status: 'completed', updatedAtMs: at(2) }];
		const outside: CrewTaskRow[] = [{ agentId: 'a', status: 'completed', updatedAtMs: at(25) }];
		expect(shouldFetchNextTaskPage(inside, 'c', SINCE, 1)).toBe(true);
		expect(shouldFetchNextTaskPage(outside, 'c', SINCE, 1)).toBe(false);
		expect(shouldFetchNextTaskPage(inside, null, SINCE, 1)).toBe(false);
		expect(shouldFetchNextTaskPage(inside, 'c', SINCE, 5)).toBe(false);
	});

	it('maps agent status to crew state', () => {
		expect(agentCrewState({ status: 'triggered' })).toBe('active');
		expect(agentCrewState({ status: 'idle', disabled: true })).toBe('off');
		expect(agentCrewState({ status: 'disabled' })).toBe('off');
		expect(agentCrewState({ status: 'completed' })).toBe('idle');
	});

	it('joins agents, model use and windowed tasks; active first, then cost, then tasks', () => {
		const rows = buildCrewRows({
			agents: [
				{ agent_id: 'pilot', name: 'Pilot', status: 'idle' },
				{ agent_id: 'scout', name: '', status: 'running' },
				{ agent_id: 'idle-one', name: 'Idle', status: 'idle' },
				{ agent_id: 'worker', name: 'Worker', status: 'idle' }
			],
			llm: [
				{ agentId: 'pilot', calls: 10, costUsd: 0.5, okCalls: 9, avgLatencyMs: 1 },
				{ agentId: 'ghost', calls: 2, costUsd: 0.01, okCalls: 2, avgLatencyMs: 1 }
			],
			tasks: [
				{ agentId: 'pilot', status: 'completed', updatedAtMs: at(1) },
				{ agentId: 'pilot', status: 'done', updatedAtMs: at(2) },
				{ agentId: 'pilot', status: 'failed', updatedAtMs: at(3) },
				{ agentId: 'pilot', status: 'failed', updatedAtMs: at(30) },
				{ agentId: 'worker', status: 'running', updatedAtMs: at(1) },
				{ agentId: 'idle-one', status: 'completed', updatedAtMs: at(48) },
				{ agentId: null, status: 'completed', updatedAtMs: at(1) }
			],
			sinceMs: SINCE
		});
		expect(rows.map((r) => r.agentId)).toEqual(['scout', 'worker', 'pilot', 'ghost']);
		const [scout, worker, pilot, ghost] = rows;
		expect(scout.name).toBe('scout');
		expect(scout.state).toBe('active');
		expect(scout.successPct).toBeNull();
		expect(scout.reliabilityPct).toBeNull();
		expect(worker.state).toBe('active');
		expect(pilot).toMatchObject({ done: 2, failed: 1, successPct: 67, reliabilityPct: 90, state: 'idle' });
		expect(ghost.name).toBe('ghost');

		const totals = crewTotals(rows, 4);
		expect(totals).toEqual({ activeNow: 2, totalAgents: 4, costUsd: 0.51, tasksDone: 2, reliabilityPct: 92 });
		expect(crewTotals([], 3).reliabilityPct).toBeNull();
	});

	it('tones and formats percentages', () => {
		expect(pctTone(null)).toBe('muted');
		expect(pctTone(95)).toBe('success');
		expect(pctTone(80)).toBe('warning');
		expect(pctTone(79)).toBe('danger');
		expect(formatPct(null)).toBe('—');
		expect(formatPct(100)).toBe('100%');
	});

	it('fetches model use and pages tasks until the window is covered', async () => {
		const calls: string[] = [];
		const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200 });
		const fetchImpl = vi.fn(async (url: string) => {
			calls.push(url);
			if (url.includes('llm_calls')) return json({ columns: ['agent_id', 'calls'], rows: [['pilot', 3]] });
			if (!url.includes('cursor=')) {
				return json({
					tasks: [{ agent_id: 'pilot', status: 'completed', updated_at: new Date(at(1)).toISOString() }],
					pagination: { next_cursor: 'p2' }
				});
			}
			return json({
				tasks: [
					{ agent_id: 'pilot', status: 'failed', updated_at: new Date(at(20)).toISOString() },
					{ agent_id: 'pilot', status: 'failed', updated_at: new Date(at(26)).toISOString() }
				],
				pagination: { next_cursor: 'p3' }
			});
		});
		const result = await fetchCrewActivity(SINCE, fetchImpl);
		expect(result.llm).toHaveLength(1);
		expect(result.tasks.map((t) => t.status)).toEqual(['completed', 'failed']);
		expect(calls.filter((u) => u.startsWith('/api/magician/v3/tasks'))).toEqual([
			'/api/magician/v3/tasks?limit=100&sort=updated_at&order=desc',
			'/api/magician/v3/tasks?limit=100&sort=updated_at&order=desc&cursor=p2'
		]);
	});

	it('rejects when a source fails so the slide can show Retry', async () => {
		const fetchImpl = vi.fn(async () => new Response('nope', { status: 500 }));
		await expect(fetchCrewActivity(SINCE, fetchImpl)).rejects.toThrow();
	});
});
