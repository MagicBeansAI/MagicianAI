import { beforeEach, describe, expect, it, vi } from 'vitest';
import { timedFetch } from '$lib/shared/fetch';
import { readTaskCallHistory } from './taskCallHistory';

vi.mock('$lib/shared/fetch', () => ({ timedFetch: vi.fn() }));
beforeEach(() => vi.mocked(timedFetch).mockReset());

describe('readTaskCallHistory', () => {
	it('loads auxiliary calls and preserves unreported measurements', async () => {
		vi.mocked(timedFetch).mockResolvedValue({ ok: true, json: async () => ({
			columns: ['cost_usd', 'output_tokens', 'llm_call_id', 'input_tokens'],
			rows: [[0.02, 200, 'primary', 1000], [0.001, 50, 'judge', 500], [null, null, 'unknown', null]]
		}) } as Response);
		const calls = await readTaskCallHistory('exec-1', 'anonymous', 'default');
		expect(calls?.get('judge')).toEqual({ tokens: 550, costUsd: 0.001, executionId: 'exec-1' });
		expect(calls?.get('unknown')).toEqual({ tokens: null, costUsd: null, executionId: 'exec-1' });
		expect(calls?.size).toBe(3);
	});

	it('scopes the read to the run and descendants, excluding bookkeeping aggregates', async () => {
		vi.mocked(timedFetch).mockResolvedValue({ ok: true, json: async () => ({ columns: ['llm_call_id'], rows: [] }) } as Response);
		await readTaskCallHistory("exec-'1", 'owner name', 'my workspace');
		const [url, options] = vi.mocked(timedFetch).mock.calls[0];
		expect(String(url)).toContain('principal=owner+name&workspace=my+workspace');
		const { sql } = JSON.parse(options?.body as string);
		expect(sql).toContain("root_execution_id = 'exec-''1' OR execution_id = 'exec-''1'");
		expect(sql).toContain('COALESCE(provider_attempt_count, 1) <> 0');
		expect(sql).not.toContain('prompt');
	});

	it('lets activity totals remain available when telemetry fails', async () => {
		vi.mocked(timedFetch).mockRejectedValue(new Error('offline'));
		expect(await readTaskCallHistory('exec-1', 'anonymous', 'default')).toBeNull();
		vi.mocked(timedFetch).mockResolvedValue({ ok: false } as Response);
		expect(await readTaskCallHistory('exec-1', 'anonymous', 'default')).toBeNull();
	});
});
