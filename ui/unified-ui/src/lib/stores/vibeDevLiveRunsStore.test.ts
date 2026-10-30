import { describe, expect, it } from 'vitest';

import { liveRunStalledForMs, parseLiveRun } from './vibeDevLiveRunsStore';

const NOW = Date.parse('2026-10-20T15:05:00Z');

function run(fields: Record<string, unknown>) {
	const parsed = parseLiveRun({
		run_id: 'run_a',
		project_id: 'proj',
		title: 'Build',
		state: 'working',
		status: 'running',
		updated_at: '2026-10-20T15:04:00Z',
		...fields
	});
	if (!parsed) throw new Error('row did not parse');
	return parsed;
}

describe('liveRunStalledForMs', () => {
	it('reports a working run with no progress past the stall rule', () => {
		// A parent waiting on a delegated child that never resumed: still
		// `running`, `last_progress_at` frozen twelve minutes ago.
		expect(liveRunStalledForMs(run({ last_progress_at: '2026-10-20T14:53:00Z' }), NOW)).toBe(
			12 * 60_000
		);
	});

	it('is not a stall inside five minutes, without a progress instant, or off a running turn', () => {
		expect(liveRunStalledForMs(run({ last_progress_at: '2026-10-20T15:01:00Z' }), NOW)).toBeNull();
		expect(liveRunStalledForMs(run({}), NOW)).toBeNull();
		expect(
			liveRunStalledForMs(run({ last_progress_at: 'not a time' }), NOW)
		).toBeNull();
		expect(
			liveRunStalledForMs(
				run({ state: 'paused', status: 'paused', last_progress_at: '2026-10-20T14:00:00Z' }),
				NOW
			)
		).toBeNull();
		expect(
			liveRunStalledForMs(
				run({ state: 'needs_you', last_progress_at: '2026-10-20T14:00:00Z' }),
				NOW
			)
		).toBeNull();
	});
});
