import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import BrowserEngineObservability from './BrowserEngineObservability.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('BrowserEngineObservability', () => {
	it('renders typed work and drives shared controls with real server offsets', async () => {
		const calls: URL[] = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request) => {
				const url = new URL(String(request), 'http://localhost');
				calls.push(url);
				const limit = Number(url.searchParams.get('limit'));
				const offset = Number(url.searchParams.get('offset'));
				return {
					ok: true,
					status: 200,
					json: async () => ({
						items: [
							{
								id: `attempt-${offset}`,
								occurred_at_ms: 1_786_000_000_000,
								session_id: 'session-1',
								execution_id: null,
								task_id: null,
								work_kind: 'content_read',
								work_id: 'article-42',
								engine: 'lightpanda',
								fallback_from: null,
								connection_mode: 'headless',
								operation: 'open',
								url: 'https://example.test/news',
								success: false,
								elapsed_ms: 18,
								error_class: 'command_failed'
							}
						],
						total_count: 30,
						limit,
						offset,
						has_more: offset + limit < 30,
						summary: [
							{
								engine: 'lightpanda',
								attempts: 30,
								successes: 20,
								failures: 10,
								success_rate: 2 / 3,
								average_elapsed_ms: 18
							}
						]
					})
				};
			})
		);

		const user = userEvent.setup();
		render(BrowserEngineObservability);
		expect(await screen.findByText('content read · article-42')).toBeInTheDocument();
		expect(screen.getByRole('link', { name: 'https://example.test/news' })).toHaveAttribute(
			'href',
			'https://example.test/news'
		);
		expect(screen.getByText('command failed')).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Next page' }));
		await waitFor(() => expect(calls.at(-1)?.searchParams.get('offset')).toBe('25'));
		await waitFor(() => expect(screen.getByLabelText('Rows')).not.toBeDisabled());

		await user.selectOptions(screen.getByLabelText('Rows'), '50');
		await waitFor(() => {
			expect(calls.at(-1)?.searchParams.get('limit')).toBe('50');
			expect(calls.at(-1)?.searchParams.get('offset')).toBe('0');
		});

		await user.selectOptions(screen.getByLabelText('Outcome'), 'failure');
		await waitFor(() => expect(calls.at(-1)?.searchParams.get('outcome')).toBe('failure'));
	});
});
