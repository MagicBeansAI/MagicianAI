import { beforeEach, describe, expect, it, vi } from 'vitest';

import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import {
	deleteAudioNote,
	fetchAudioNotes,
	uploadAudioNote
} from './audioNotes';

beforeEach(() => {
	scopeIdentityStore.observe('alice', 'research');
	vi.restoreAllMocks();
});

describe('Audio Notes API client', () => {
	it('requests a real server page without URL scope selectors', async () => {
		const fetchMock = vi.fn(async (_input: RequestInfo | URL) => new Response(JSON.stringify({
			items: [], offset: 20, limit: 20, total: 41, has_more: true
		}), { status: 200, headers: { 'content-type': 'application/json' } }));
		vi.stubGlobal('fetch', fetchMock);

		const page = await fetchAudioNotes({ offset: 20, limit: 20, query: 'launch' });

		const url = String(fetchMock.mock.calls[0][0]);
		expect(url).toContain('offset=20');
		expect(url).toContain('limit=20');
		expect(url).toContain('q=launch');
		expect(url).not.toContain('principal=');
		expect(url).not.toContain('workspace=');
		expect(page.total).toBe(41);
	});

	it('uploads the stable id without multipart scope selectors', async () => {
		const fetcher = vi.fn(async (_url: RequestInfo | URL, init?: RequestInit) => {
			const form = init?.body as FormData;
			expect(form.get('note_id')).toBe('11111111-1111-4111-8111-111111111111');
			expect(form.get('principal')).toBeNull();
			expect(form.get('workspace')).toBeNull();
			expect(form.get('transcript')).toBe('hello there');
			return new Response(JSON.stringify({
				note_id: String(form.get('note_id')),
				requested_provider: 'silverbullet',
				provider: 'silverbullet',
				used_fallback: false,
				captured_at: '2026-08-01T12:00:00.000Z',
				note_path: 'Audio Notes/2026-08-01/12-00-00-note.md',
				audio_path: 'Audio Notes/2026-08-01/12-00-00-note.webm',
				bytes: 5,
				content_hash: `blake3:${'a'.repeat(64)}`
			}), {
				status: 201,
				headers: { 'content-type': 'application/json' }
			});
		});

		await uploadAudioNote({
			noteId: '11111111-1111-4111-8111-111111111111',
			capturedAt: '2026-08-01T12:00:00.000Z',
			sourceSurface: 'web_chat_dictation', transcript: 'hello there',
			originalFilename: 'dictation.webm', mimeType: 'audio/webm', durationMs: 900,
			audio: new Blob(['audio'], { type: 'audio/webm' })
		}, fetcher as typeof fetch);

		expect(String(fetcher.mock.calls[0][0])).not.toContain('principal=');
	});

	it('retains audio when a successful response is not a matching durable receipt', async () => {
		const note = {
			noteId: '11111111-1111-4111-8111-111111111111',
			capturedAt: '2026-08-01T12:00:00.000Z',
			sourceSurface: 'web_chat_dictation', transcript: null,
			originalFilename: 'dictation.webm', mimeType: 'audio/webm', durationMs: 900,
			audio: new Blob(['audio'], { type: 'audio/webm' })
		};
		const htmlFetcher = vi.fn(async () => new Response('<html>sign in</html>', {
			status: 200,
			headers: { 'content-type': 'text/html' }
		}));
		await expect(uploadAudioNote(note, htmlFetcher as typeof fetch)).rejects.toMatchObject({
			retryable: true
		});

		const mismatchFetcher = vi.fn(async () => new Response(JSON.stringify({
			note_id: note.noteId,
			requested_provider: 'silverbullet',
			provider: 'silverbullet',
			used_fallback: false,
			captured_at: note.capturedAt,
			note_path: 'Audio Notes/2026-08-01/12-00-00-note.md',
			audio_path: 'Audio Notes/2026-08-01/12-00-00-note.webm',
			bytes: 999,
			content_hash: `blake3:${'a'.repeat(64)}`
		}), { status: 201, headers: { 'content-type': 'application/json' } }));
		await expect(uploadAudioNote(note, mismatchFetcher as typeof fetch)).rejects.toMatchObject({
			retryable: true
		});
	});

	it('classifies permanent and retryable failures', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response('{"message":"no"}', {
			status: 403,
			headers: { 'content-type': 'application/json' }
		})));
		await expect(deleteAudioNote('note-id')).rejects.toMatchObject({
			status: 403,
			retryable: false
		});
	});
});
