import { beforeEach, describe, expect, it, vi } from 'vitest';

import { AudioNoteRequestError, type AudioNoteReceipt } from './audioNotes';
import {
	AudioNoteOutboxQueue,
	type AudioNoteOutboxRecord,
	type AudioNoteOutboxStorage
} from './audioNoteOutbox';

class MemoryStorage implements AudioNoteOutboxStorage {
	rows = new Map<string, AudioNoteOutboxRecord>();
	async list() { return Array.from(this.rows.values()); }
	async get(id: string) { return this.rows.get(id) ?? null; }
	async put(row: AudioNoteOutboxRecord) { this.rows.set(row.noteId, structuredClone(row)); }
	async delete(id: string) { this.rows.delete(id); }
	async claim(id: string, owner: string, now: number, leaseMs: number) {
		const row = this.rows.get(id);
		if (!row || ((row.leaseUntil ?? 0) > now && row.leaseOwner !== owner)) return null;
		const claimed = { ...row, leaseOwner: owner, leaseUntil: now + leaseMs, updatedAt: now };
		this.rows.set(id, structuredClone(claimed));
		return claimed;
	}
}

class InitiallyUnavailableStorage extends MemoryStorage {
	private unavailable = true;
	override async list() {
		if (this.unavailable) {
			this.unavailable = false;
			throw new Error('storage temporarily unavailable');
		}
		return super.list();
	}
}

function response(noteId: string): AudioNoteReceipt {
	return {
		note_id: noteId, requested_provider: 'silverbullet',
		provider: 'silverbullet', used_fallback: false,
		captured_at: '2026-08-01T12:00:00.000Z',
		note_path: 'Audio Notes/note.md',
		audio_path: 'Audio Notes/note.webm', bytes: 5,
		content_hash: `blake3:${'a'.repeat(64)}`
	};
}

beforeEach(() => {
	vi.stubGlobal('crypto', { randomUUID: () => '11111111-1111-4111-8111-111111111111' });
});

describe('AudioNoteOutboxQueue', () => {
	it('persists before transcription and uploads the finalized transcript', async () => {
		const storage = new MemoryStorage();
		const uploader = vi.fn(async (note) => response(note.noteId));
		const queue = new AudioNoteOutboxQueue(storage, uploader);
		const noteId = await queue.stage({
			audio: new Blob(['audio']), originalFilename: 'note.webm',
			mimeType: 'audio/webm', durationMs: 1_200
		});
		expect((await storage.get(noteId))?.state).toBe('waiting_for_transcript');

		await queue.finalize(noteId, 'durable transcript');
		await vi.waitFor(() => expect(uploader).toHaveBeenCalledTimes(1));
		const upload = uploader.mock.calls[0][0];
		expect(upload).toMatchObject({
			transcript: 'durable transcript'
		});
		expect(upload).not.toHaveProperty('principal');
		expect(upload).not.toHaveProperty('workspace');
		await vi.waitFor(() => expect(storage.rows.size).toBe(0));
		queue.stop();
	});

	it('recovers an interrupted transcription as an audio-only upload', async () => {
		const storage = new MemoryStorage();
		storage.rows.set('recovered', {
			noteId: 'recovered',
			capturedAt: '2026-08-01T12:00:00.000Z', sourceSurface: 'web_chat_dictation',
			transcript: null, originalFilename: 'note.webm', mimeType: 'audio/webm',
			durationMs: 1_000, audio: new Blob(['audio']), createdAt: 1, updatedAt: 1,
			state: 'waiting_for_transcript', attempts: 0, nextAttemptAt: 0, lastError: null
		});
		const uploader = vi.fn(async (note) => response(note.noteId));
		const queue = new AudioNoteOutboxQueue(storage, uploader);

		await queue.start();
		await vi.waitFor(() => expect(uploader).toHaveBeenCalledTimes(1));
		expect(uploader.mock.calls[0][0].transcript).toBeNull();
		queue.stop();
	});

	it('isolates a permanent poison row for manual retry', async () => {
		const storage = new MemoryStorage();
		const queue = new AudioNoteOutboxQueue(storage, async () => {
			throw new AudioNoteRequestError('forbidden', 403, false);
		});
		const noteId = await queue.stage({
			audio: new Blob(['audio']), originalFilename: 'note.webm',
			mimeType: 'audio/webm', durationMs: 1_200
		});

		await queue.finalize(noteId, null);
		await vi.waitFor(async () => expect((await storage.get(noteId))?.state).toBe('failed'));
		expect((await storage.get(noteId))?.lastError).toBe('forbidden');
		queue.stop();
	});

	it('can start again after durable storage initialization fails', async () => {
		const queue = new AudioNoteOutboxQueue(new InitiallyUnavailableStorage(), vi.fn());
		await expect(queue.start()).rejects.toThrow('storage temporarily unavailable');
		await expect(queue.start()).resolves.toBeUndefined();
		queue.stop();
	});

	it('rejects a recording that the server can never accept', async () => {
		const queue = new AudioNoteOutboxQueue(new MemoryStorage(), vi.fn());
		await expect(queue.stage({
			audio: new Blob([new Uint8Array(24 * 1024 * 1024 + 1)]),
			originalFilename: 'oversized.webm', mimeType: 'audio/webm', durationMs: 1
		})).rejects.toThrow('24 MB');
		queue.stop();
	});

	it('does not recover another tab while its transcription lease is active', async () => {
		const storage = new MemoryStorage();
		const now = 1_000;
		storage.rows.set('active-transcript', {
			noteId: 'active-transcript',
			capturedAt: '2026-08-01T12:00:00.000Z', sourceSurface: 'web_chat_dictation',
			transcript: null, originalFilename: 'note.webm', mimeType: 'audio/webm',
			durationMs: 1_000, audio: new Blob(['audio']), createdAt: now, updatedAt: now,
			state: 'waiting_for_transcript', attempts: 0, nextAttemptAt: 0, lastError: null,
			transcriptDeadlineAt: now + 60_000
		});
		const uploader = vi.fn(async (note) => response(note.noteId));
		const queue = new AudioNoteOutboxQueue(storage, uploader, () => now, 'second-tab');

		await queue.start();
		expect(uploader).not.toHaveBeenCalled();
		expect((await storage.get('active-transcript'))?.state).toBe('waiting_for_transcript');
		queue.stop();
	});

	it('atomically claims one upload across browser tabs', async () => {
		const storage = new MemoryStorage();
		storage.rows.set('pending', {
			noteId: 'pending',
			capturedAt: '2026-08-01T12:00:00.000Z', sourceSurface: 'web_chat_dictation',
			transcript: 'one upload', originalFilename: 'note.webm', mimeType: 'audio/webm',
			durationMs: 1_000, audio: new Blob(['audio']), createdAt: 1, updatedAt: 1,
			state: 'pending', attempts: 0, nextAttemptAt: 0, lastError: null
		});
		const uploader = vi.fn(async (note) => response(note.noteId));
		const first = new AudioNoteOutboxQueue(storage, uploader, () => 10, 'tab-one');
		const second = new AudioNoteOutboxQueue(storage, uploader, () => 10, 'tab-two');

		await Promise.all([first.flush(), second.flush()]);
		expect(uploader).toHaveBeenCalledTimes(1);
		expect(storage.rows.size).toBe(0);
		first.stop();
		second.stop();
	});
});
