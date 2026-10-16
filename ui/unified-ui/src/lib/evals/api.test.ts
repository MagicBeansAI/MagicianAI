import { afterEach, describe, expect, it, vi } from 'vitest';
import { runEvalLane } from './api';

const timedFetch = vi.hoisted(() => vi.fn());
vi.mock('$lib/shared/fetch', () => ({ timedFetch }));
afterEach(() => vi.clearAllMocks());

describe('Evals run parameters', () => {
	it('sends selected profiles and repeats with the lane start request', async () => {
		timedFetch.mockResolvedValue(new Response(JSON.stringify({ task_id: 'task', run_id: 'run' })));
		const options = { profiles: ['candidate', 'another'], repeats: 2, partition: 'validation' as const };
		expect(await runEvalLane('test-memory-lifecycle-live-eval', options)).toEqual({ task_id: 'task', run_id: 'run' });
		expect(timedFetch.mock.calls[0][0]).toContain('/test-memory-lifecycle-live-eval/run');
		expect(JSON.parse(timedFetch.mock.calls[0][1].body)).toEqual(options);
	});
	it('keeps existing lanes body-compatible and preserves backend refusal messages', async () => {
		timedFetch.mockResolvedValue(new Response(JSON.stringify({ message: 'Select at most three profiles' }), { status: 400 }));
		await expect(runEvalLane('existing-lane')).rejects.toThrow('Select at most three profiles');
		expect(JSON.parse(timedFetch.mock.calls[0][1].body)).toEqual({});
	});
});
