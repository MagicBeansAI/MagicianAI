import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';

import TodayPulseBand from './TodayPulseBand.svelte';
import { installFetchMock, jsonResponse } from '../../test/browser';

describe('TodayPulseBand mounted analytics', () => {
	it('renders each available pulse slice as a link to its owning workflow', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/analytics/llm_calls/query',
				handle: () =>
					jsonResponse({
						columns: ['section', 'k', 'model', 'v1', 'v2'],
						rows: [
							['today_total', 'all', null, 1.25, 42],
							['yesterday_total', 'all', null, 2.5, 77],
							['today_provider', 'openai', 'gpt-5.6', 1.25, 42]
						]
					})
			},
			{
				method: 'POST',
				match: '/analytics/query',
				handle: () =>
					jsonResponse({
						columns: ['section', 'n'],
						rows: [['coding_runs_today', 2]]
					})
			},
			{
				method: 'POST',
				match: '/analytics/memory_events/query',
				handle: () =>
					jsonResponse({
						columns: ['section', 'n', 'passes'],
						rows: [
							['memories_today', 3, 0],
							['evals_today', 2, 1]
						]
					})
			}
		]);

		render(TodayPulseBand);

		expect(await screen.findByRole('region', { name: "Today's pulse" })).toBeInTheDocument();
		expect(await screen.findByRole('link', { name: /LLM spend: \$1\.25/ })).toHaveAttribute(
			'href',
			'/llm#today'
		);
		expect(screen.getByRole('link', { name: /Top model: gpt-5\.6/ })).toBeInTheDocument();
		expect(screen.getByRole('link', { name: 'Coding runs: 2 today' })).toHaveAttribute(
			'href',
			'/vibe'
		);
		expect(screen.getByRole('link', { name: 'Memories learned: 3 today' })).toHaveAttribute(
			'href',
			'/memory'
		);
		expect(screen.getByRole('link', { name: 'Evals: 2 cases today, 50 percent passing' })).toBeInTheDocument();
	});
});
