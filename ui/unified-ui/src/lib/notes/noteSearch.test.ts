import { describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { highlightSegments, NoteSearchError, searchNotes } from './noteSearch';

const results = {
	hits: [
		{
			provider: 'local_markdown',
			relative_path: 'Inbox/2026-08-09.md',
			source_ref: 'notes:local_markdown:Inbox/2026-08-09.md',
			title: 'Budget',
			modified_at_ms: 1_754_700_000_000,
			open_url: 'file:///notes/Inbox/2026-08-09.md',
			matched_in_title: true,
			match_count: 4,
			matches: [{ line: 3, text: 'the budget meeting decision' }]
		}
	],
	scanned_notes: 128,
	scan_truncated: false,
	query_terms: ['budget', 'meeting'],
	more_available: true
};

describe('searchNotes', () => {
	it('posts the query without client scope authority', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/notes/search', handle: () => jsonResponse(results) }
		]);

		const page = await searchNotes('budget meeting', { limit: 10 });

		expect(page.hits[0].title).toBe('Budget');
		expect(page.more_available).toBe(true);
		const body = JSON.parse(String(calls[0].init?.body));
		expect(body).toMatchObject({
			query: 'budget meeting',
			limit: 10
		});
		expect(body).not.toHaveProperty('principal');
		expect(body).not.toHaveProperty('workspace');
	});

	it('reports a server rejection rather than returning an empty result', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/notes/search',
				handle: () => new Response('notes provider is disabled', { status: 403 })
			}
		]);

		// An empty list would read as "you have written nothing about this",
		// which is a different and wrong answer.
		await expect(searchNotes('budget')).rejects.toBeInstanceOf(NoteSearchError);
	});
});

describe('highlightSegments', () => {
	it('marks each term and leaves the rest of the line intact', () => {
		const segments = highlightSegments('the budget meeting decision', ['budget', 'meeting']);

		expect(segments.map((segment) => segment.text).join('')).toBe(
			'the budget meeting decision'
		);
		// Two spans, not one: the space between them matched nothing.
		expect(segments.filter((segment) => segment.match).map((segment) => segment.text)).toEqual([
			'budget',
			'meeting'
		]);
	});

	it('marks a term that differs only by case', () => {
		const segments = highlightSegments('The INVOICES were filed', ['invoice']);

		expect(segments.filter((segment) => segment.match).map((segment) => segment.text)).toEqual([
			'INVOICE'
		]);
	});

	it('merges overlapping terms into one span', () => {
		// "note" sits inside "notes"; nesting the marks would double-wrap the text.
		const segments = highlightSegments('my notes today', ['note', 'notes']);

		expect(segments.filter((segment) => segment.match).map((segment) => segment.text)).toEqual([
			'notes'
		]);
	});

	it('returns the line untouched when there is nothing to mark', () => {
		expect(highlightSegments('plain line', [])).toEqual([{ text: 'plain line', match: false }]);
	});
});
