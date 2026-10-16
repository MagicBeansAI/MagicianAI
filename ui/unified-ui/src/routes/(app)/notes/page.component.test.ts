import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse, type MockFetchCall } from '../../../test/browser';
import Page from './+page.svelte';

afterEach(() => cleanup());

function pageRoutes(searchHits = false) {
	return [
		{
			method: 'GET',
			match: '/notes/audio',
			handle: () => jsonResponse({ items: [], offset: 0, limit: 100, total: 0, has_more: false })
		},
		{
			method: 'GET',
			match: '/notes/settings',
			handle: () => jsonResponse({ settings: { enabled: true }, resolved: {} })
		},
		{
			method: 'GET',
			match: '/notes/providers/status',
			handle: () => jsonResponse({})
		},
		{
			method: 'GET',
			match: '/notes/tree?path=',
			handle: () =>
				jsonResponse({
					provider: 'local_markdown',
					path: '',
					entries: [
						{ name: 'Audio Notes', relative_path: 'Audio Notes', kind: 'dir' },
						{ name: 'Inbox', relative_path: 'Inbox', kind: 'dir' }
					]
				})
		},
		{
			method: 'POST',
			match: '/notes/search',
			handle: () =>
				jsonResponse({
					hits: searchHits
						? [
								{
									provider: 'local_markdown',
									relative_path: 'Samples/Garden/Beds/tomatoes.md',
									source_ref: 'notes:local_markdown:Samples/Garden/Beds/tomatoes.md',
									title: 'Tomatoes',
									modified_at_ms: 1,
									matched_in_title: true,
									match_count: 1,
									matches: [{ line: 1, text: 'Tomatoes need sun.' }]
								}
							]
						: [],
					scanned_notes: 1,
					scan_truncated: false,
					query_terms: ['tomato'],
					more_available: false
				})
		}
	];
}

function searchCalls(calls: MockFetchCall[]): MockFetchCall[] {
	return calls.filter((call) => call.url.includes('/notes/search'));
}

describe('notes page', () => {
	it('puts search in a short header and marks the transcription folder', async () => {
		installFetchMock(pageRoutes());

		render(Page);

		expect(screen.queryByRole('button', { name: 'Search' })).toBeNull();
		const title = screen.getByRole('heading', { level: 1, name: 'Notes' });
		const header = title.closest('header');
		expect(header?.querySelector('[aria-label="Search notes"]')).toBeTruthy();
		expect(header?.querySelector('[aria-label="Refresh"]')).toBeTruthy();
		expect(screen.getByText('Keep recordings')).toBeTruthy();
		expect(screen.queryByRole('heading', { name: /Audio archive/i })).toBeNull();
		expect(screen.queryByLabelText(/Search transcripts/i)).toBeNull();

		const transcripts = await screen.findByRole('button', { name: /Audio Notes/ });
		expect(transcripts.querySelector('svg[aria-label="Transcription"]')).toBeTruthy();
		expect(screen.getByRole('button', { name: /Inbox/ }).querySelector('svg')).toBeNull();
	});

	it('searches after a pause in typing instead of on each keystroke', async () => {
		const { calls } = installFetchMock(pageRoutes(true));
		const user = userEvent.setup({ delay: null });
		render(Page);

		await user.type(screen.getByLabelText('Search notes'), 'tomato');
		expect(searchCalls(calls)).toHaveLength(0);

		await waitFor(() => expect(searchCalls(calls).length).toBeGreaterThan(0));
		expect(await screen.findByRole('button', { name: 'tomatoes.md' })).toBeTruthy();
		expect(document.querySelector('.written-title')?.textContent).toBe('Tomatoes');
		expect(screen.getByText('Samples/Garden/Beds/')).toBeTruthy();
		expect(screen.getByText('1')).toBeTruthy();
		expect(screen.getByRole('status').textContent).toMatch(/1 result · \d+ ms/);
		expect(screen.queryByText('SilverBullet')).toBeNull();
	});
});
