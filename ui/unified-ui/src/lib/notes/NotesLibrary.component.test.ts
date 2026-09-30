import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import NotesLibrary from './NotesLibrary.svelte';

afterEach(() => cleanup());

describe('NotesLibrary', () => {
	it('expands a folder and opens a note with in-file search', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/notes/tree?path=',
				handle: (call) => {
					const path = new URL(call.url, 'http://local').searchParams.get('path');
					if (!path) {
						return jsonResponse({
							provider: 'local_markdown',
							path: '',
							entries: [
								{ name: 'Audio Notes', relative_path: 'Audio Notes', kind: 'dir', has_children: true },
								{ name: 'Empty', relative_path: 'Empty', kind: 'dir' },
								{ name: 'Inbox', relative_path: 'Inbox', kind: 'dir', has_children: true },
								{ name: 'root.md', relative_path: 'root.md', kind: 'file' }
							]
						});
					}
					if (path === 'Empty') {
						return jsonResponse({ provider: 'local_markdown', path, entries: [] });
					}
					return jsonResponse({
						provider: 'local_markdown',
						path,
						entries: [{ name: 'hello.md', relative_path: 'Inbox/hello.md', kind: 'file' }]
					});
				}
			},
			{
				method: 'GET',
				match: '/notes/backlinks?path=',
				handle: () => jsonResponse({ provider: 'local_markdown', path: 'Inbox/hello.md', backlinks: [] })
			},
			{
				method: 'GET',
				match: '/notes/file?path=Inbox%2Fhello.md',
				handle: () =>
					jsonResponse({
						provider: 'local_markdown',
						relative_path: 'Inbox/hello.md',
						title: 'Hello',
						markdown: [
							'# Hello',
							'',
							'The invoice is due.',
							'',
							'- water daily',
							'',
							'1. Pick ripe fruit',
							'',
							'> Keep the soil damp.',
							'',
							'| Bed | Crop |',
							'| --- | --- |',
							'| North | Tomato |',
							'',
							'```js',
							'let n = 1',
							'```',
							'',
							'See [[Inbox]].'
						].join('\n')
					})
			}
		]);
		const user = userEvent.setup();
		render(NotesLibrary);

		const empty = await screen.findByRole('button', { name: 'Empty' });
		await user.click(empty);
		expect(empty.closest('li')?.querySelector('ul')).toBeNull();
		expect(screen.queryByRole('button', { name: /hello.md/ })).toBeNull();
		expect(screen.getByRole('button', { name: 'New note' })).toBeTruthy();
		const transcripts = await screen.findByRole('button', { name: /Audio Notes/ });
		expect(transcripts.querySelector('svg[aria-label="Transcription"]')).toBeTruthy();
		expect(screen.getByRole('button', { name: /Inbox/ }).querySelector('svg')).toBeNull();
		await user.click(screen.getByRole('button', { name: /Inbox/ }));
		await user.click(await screen.findByRole('button', { name: /hello.md/ }));
		expect(await screen.findByRole('heading', { name: 'Hello' })).toBeTruthy();
		expect(screen.queryByText('# Hello')).toBeNull();
		expect(screen.getByText(/The invoice is due/)).toBeTruthy();
		expect(screen.getByText('water daily').closest('li')).toBeTruthy();
		expect(screen.getByText('Pick ripe fruit').closest('ol')).toBeTruthy();
		expect(screen.queryByText('- water daily')).toBeNull();
		expect(screen.getByText('Keep the soil damp.')).toBeTruthy();
		expect(screen.getByRole('cell', { name: 'Tomato' })).toBeTruthy();
		expect(screen.getByText('let n = 1')).toBeTruthy();
		expect(screen.queryByText('```js')).toBeNull();
		expect(await screen.findByRole('link', { name: 'Inbox' })).toBeTruthy();
		expect(await screen.findByText('No other notes link here.')).toBeTruthy();

		await user.type(screen.getByLabelText('Find in this note'), 'invoice');
		await waitFor(() => expect(screen.getByText('1 match')).toBeTruthy());
		expect(screen.getByText('invoice').tagName).toBe('MARK');
	});
});
