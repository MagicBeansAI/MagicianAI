import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import AttentionLearningHealthStrip from './AttentionLearningHealthStrip.svelte';

const rankStatus = {
	schema_version: 1,
	enabled: true,
	paused: false,
	pause_reason: null,
	queue: {
		pending: 2,
		in_flight: 1,
		retry: 0,
		succeeded: 331,
		stale: 64,
		dead: 0,
		next_retry_at: null,
		oldest_pending_at: 1_000
	},
	worker: {
		batch_size: 20,
		concurrency: 2,
		interval_secs: 60,
		max_retries: 5,
		lease_secs: 120,
		retention_days: 30
	}
};

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('AttentionLearningHealthStrip rank queue', () => {
	it('loads the shared rank queue when the strip is opened', async () => {
		const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
			const url = String(input);
			if (url.endsWith('/attention-learning/rank-recompute/status')) {
				return new Response(JSON.stringify(rankStatus), {
					status: 200,
					headers: { 'content-type': 'application/json' }
				});
			}
			throw new Error(`unexpected request: ${url}`);
		});
		vi.stubGlobal('fetch', fetchMock);

		render(AttentionLearningHealthStrip, {
			props: {
				semanticRankingEnabled: true,
				health: {
					total_active: 4,
					source_family_counts: {},
					embedded_candidates: 4,
					embedding_coverage: 1,
					learned_rank_changes: 0
				}
			}
		});

		expect(screen.queryByText('Rank queue pending / active / retry')).not.toBeInTheDocument();
		expect(fetchMock).not.toHaveBeenCalled();

		await fireEvent.click(screen.getByRole('button', { name: /System health/ }));

		expect(await screen.findByText('2 / 1 / 0')).toBeInTheDocument();
		expect(screen.getByText('331 / 64 / 0')).toBeInTheDocument();
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});
});
