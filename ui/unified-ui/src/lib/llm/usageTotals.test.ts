import { afterEach, describe, expect, it, vi } from 'vitest';
import { readUsageTotals, readUsageModels, usageTotalsSql } from './usageTotals';

afterEach(() => vi.restoreAllMocks());

describe('cumulative usage ledger', () => {
	it('uses the full chat lineage rather than loaded messages or summed task totals', () => {
		const sql = usageTotalsSql('chat', "chat'one");
		expect(sql).toContain("chat_session_id = 'chat''one'");
		expect(sql).toContain('SUM(measured_calls)');
		expect(sql).toContain('FROM llm_cost_daily');
		expect(sql).not.toContain('LIMIT');
	});
	it('includes task descendants and every execution, without accepting an empty scope', () => {
		expect(usageTotalsSql('task', 'task-1')).toContain('root_execution_id IN');
		expect(() => usageTotalsSql('task', '')).toThrow();
	});
	it('preserves unreported cache and cost instead of displaying invented zeros', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(JSON.stringify({
			columns: ['calls','input_tokens','output_tokens','cache_read_tokens','cache_creation_tokens','cost_usd','unpriced_calls'],
			rows: [[2, 100, 10, null, 0, null, 2]]
		})));
		expect(await readUsageTotals('chat', 'session')).toEqual({ calls:2, input:100, output:10, cacheRead:null, cacheWrite:0, cost:null, unpricedCalls:2 });
	});
	it('reports a failed read rather than returning an empty total', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('', {status:503}));
		await expect(readUsageTotals('task','task-1')).rejects.toThrow('Usage unavailable');
	});
	it('groups recorded usage by provider and model without filtering out decision or voice calls', async () => {
		vi.spyOn(globalThis, 'fetch').mockResolvedValue(Response.json({
			columns: ['provider','model','calls','input_tokens','output_tokens','cache_read_tokens','cache_creation_tokens','cost_usd','unpriced_calls'],
			rows: [['decision:typesafe','jev-1.13',2,1000,0,null,null,0.001,0], ['openai','realtime',1,500,100,200,0,0.02,0]]
		}));
		const rows = await readUsageModels('chat', 'chat-1');
		expect(rows).toHaveLength(2);
		expect(rows[0]).toMatchObject({ provider:'decision:typesafe', model:'jev-1.13', cacheRead:null });
		expect(rows[1]).toMatchObject({ provider:'openai', model:'realtime', cacheRead:200 });
		const sql = JSON.parse(String(vi.mocked(fetch).mock.calls[0][1]?.body)).sql;
		expect(sql).toContain("chat_session_id = 'chat-1'");
		expect(sql).toContain('GROUP BY provider, model');
		expect(sql).not.toMatch(/provider\s*=/);
	});
});


describe('shared usage reads', () => {
	it('fetches models and header once and sums only reported measurements', async () => {
		const fetcher = vi.spyOn(globalThis, 'fetch').mockResolvedValue(Response.json({
			columns:['provider','model','calls','input_tokens','output_tokens','cache_read_tokens','cache_creation_tokens','cost_usd','unpriced_calls'],
			rows:[['one','a',2,100,10,null,0,null,2],['two','b',1,200,20,50,null,0.01,0]]
		}));
		const [total, models] = await Promise.all([readUsageTotals('chat','shared-chat'),readUsageModels('chat','shared-chat')]);
		expect(fetcher).toHaveBeenCalledTimes(1);
		expect(models).toHaveLength(2);
		expect(total).toEqual({calls:3,input:300,output:30,cacheRead:50,cacheWrite:0,cost:0.01,unpricedCalls:2});
	});
	it('retries a failed shared request and never shares different targets', async () => {
		const fetcher = vi.spyOn(globalThis, 'fetch').mockImplementation(async () => new Response('',{status:503}));
		await Promise.allSettled([readUsageTotals('chat','retry-chat'),readUsageModels('chat','retry-chat')]);
		expect(fetcher).toHaveBeenCalledTimes(1);
		await Promise.allSettled([readUsageModels('chat','retry-chat'),readUsageModels('task','retry-chat')]);
		expect(fetcher).toHaveBeenCalledTimes(3);
	});
});


describe('temporary analytics contention', () => {
	it('shares a bounded retry when the query guard is temporarily busy', async () => {
		const fetcher = vi.spyOn(globalThis,'fetch')
			.mockResolvedValueOnce(Response.json({error:'analytics DuckDB is busy; timed out waiting for query guard after 5s'},{status:400}))
			.mockResolvedValueOnce(Response.json({columns:['provider','model','calls'],rows:[['p','m',1]]}));
		const [totals, models] = await Promise.all([readUsageTotals('chat','busy-chat'),readUsageModels('chat','busy-chat')]);
		expect(fetcher).toHaveBeenCalledTimes(2);
		expect(totals.calls).toBe(1);
		expect(models).toHaveLength(1);
		expect(fetcher.mock.calls[0][1]?.headers).toBe(fetcher.mock.calls[1][1]?.headers);
	});
	it('does not retry an invalid query or an authentication failure', async () => {
		for (const status of [400,401]) {
			const fetcher = vi.spyOn(globalThis,'fetch').mockResolvedValue(Response.json({error:'Invalid request'},{status}));
			await expect(readUsageModels('chat','invalid-'+status)).rejects.toThrow('Usage unavailable');
			expect(fetcher).toHaveBeenCalledTimes(1);
			fetcher.mockRestore();
		}
	});
});
