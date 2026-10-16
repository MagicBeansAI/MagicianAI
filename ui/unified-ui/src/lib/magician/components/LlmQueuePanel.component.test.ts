import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import LlmQueuePanel from './LlmQueuePanel.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('LlmQueuePanel provider-capacity timing', () => {
	it('renders provider wait independently from lane wait and execution', async () => {
		const snapshot = {
			workers_total: 2,
			workers_busy: 0,
			depth_high: 0,
			depth_normal: 0,
			depth_background: 0,
			capacity_high: 8,
			capacity_normal: 8,
			capacity_background: 8,
			waiting_for_provider: 2,
			waiting_for_provider_high: 0,
			waiting_for_provider_normal: 0,
			waiting_for_provider_background: 0,
			waiting_for_local_prep: 0,
			registry: {
				pending: [],
				in_flight: [],
				failed: [],
				tombstoned: [],
				completed: [
					{
						job_id: 'job-provider-wait',
						priority: 'background',
						origin: { operation: 'memory_temperature_utility_review' },
						provider: 'ollama',
						model: 'gemma4:12b',
						state: 'completed',
						submitted_at_ms: 1,
						wait_ms: 7,
						provider_wait_ms: 125,
						execution_ms: 340,
						attempts: 1
					}
				]
			}
		};
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue({ ok: true, json: async () => snapshot })
		);

		render(LlmQueuePanel, { baseUrl: '/test' });

		expect(await screen.findByRole('columnheader', { name: 'provider wait' })).toBeInTheDocument();
		expect(await screen.findByText('7ms')).toBeInTheDocument();
		expect(await screen.findByText('125ms')).toBeInTheDocument();
		expect(await screen.findByText('340ms')).toBeInTheDocument();
		expect(await screen.findByTestId('provider-wait-count')).toHaveTextContent('2');
		expect(await screen.findByTestId('local-prep-wait-count')).toHaveTextContent('0');
	});
});
