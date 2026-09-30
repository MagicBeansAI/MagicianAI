export interface AudioNoteItem {
	note_id: string;
	provider: string;
	used_fallback: boolean;
	fallback_reason?: string | null;
	captured_at: string;
	source_surface: string;
	transcript?: string | null;
	duration_ms?: number | null;
	mime_type: string;
	note_path: string;
	audio_path: string;
	open_url?: string | null;
	bytes: number;
}

export interface AudioNotePage {
	items: AudioNoteItem[];
	offset: number;
	limit: number;
	total: number;
	has_more: boolean;
}

export interface AudioNoteReceipt {
	note_id: string;
	requested_provider: string;
	provider: string;
	used_fallback: boolean;
	fallback_reason?: string | null;
	captured_at: string;
	note_path: string;
	audio_path: string;
	bytes: number;
	content_hash: string;
}

export interface AudioNoteUpload {
	noteId: string;
	capturedAt: string;
	sourceSurface: string;
	transcript: string | null;
	originalFilename: string;
	mimeType: string;
	durationMs: number;
	audio: Blob;
}

export class AudioNoteRequestError extends Error {
	constructor(
		message: string,
		readonly status: number | null,
		readonly retryable: boolean
	) {
		super(message);
		this.name = 'AudioNoteRequestError';
	}
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return value && typeof value === 'object' && !Array.isArray(value)
		? value as Record<string, unknown>
		: null;
}

function durableReceipt(
	value: unknown,
	note: AudioNoteUpload,
	status: number
): AudioNoteReceipt {
	const receipt = asRecord(value);
	const noteId = receipt?.note_id;
	const requestedProvider = receipt?.requested_provider;
	const provider = receipt?.provider;
	const usedFallback = receipt?.used_fallback;
	const capturedAt = receipt?.captured_at;
	const notePath = receipt?.note_path;
	const audioPath = receipt?.audio_path;
	const bytes = receipt?.bytes;
	const contentHash = receipt?.content_hash;
	const expectedInstant = Date.parse(note.capturedAt);
	const receiptInstant = typeof capturedAt === 'string' ? Date.parse(capturedAt) : Number.NaN;
	const safePath = (path: unknown): path is string => {
		if (typeof path !== 'string' || !path.startsWith('Audio Notes/')) return false;
		return !path.startsWith('/') && !path.split('/').some((part) => part === '..' || part === '');
	};
	const matchingPair = safePath(notePath)
		&& safePath(audioPath)
		&& notePath.endsWith('.md')
		&& audioPath.startsWith(notePath.slice(0, -3))
		&& audioPath.length > notePath.length - 3;

	if (
		noteId !== note.noteId
		|| typeof requestedProvider !== 'string'
		|| !requestedProvider.trim()
		|| (provider !== 'local_markdown' && provider !== 'silverbullet')
		|| typeof usedFallback !== 'boolean'
		|| !Number.isFinite(expectedInstant)
		|| receiptInstant !== expectedInstant
		|| !matchingPair
		|| bytes !== note.audio.size
		|| typeof contentHash !== 'string'
		|| !/^blake3:[a-f0-9]{64}$/.test(contentHash)
	) {
		throw new AudioNoteRequestError(
			'The server did not return a matching durable Audio Notes receipt; the local recording was retained.',
			status,
			true
		);
	}

	return receipt as unknown as AudioNoteReceipt;
}

async function responseMessage(response: Response): Promise<string> {
	try {
		const payload = await response.json() as { message?: unknown; error?: unknown };
		if (typeof payload.message === 'string' && payload.message.trim()) return payload.message;
		if (typeof payload.error === 'string' && payload.error.trim()) return payload.error;
	} catch {
		// Fall through to the bounded status message.
	}
	return `Audio Notes request failed (${response.status})`;
}

function requestError(response: Response, message: string): AudioNoteRequestError {
	const retryable = response.status === 408 || response.status === 429 || response.status >= 500;
	return new AudioNoteRequestError(message, response.status, retryable);
}

export async function fetchAudioNotes(input: {
	offset: number;
	limit: number;
	query?: string;
	signal?: AbortSignal;
}): Promise<AudioNotePage> {
	const params = new URLSearchParams({
		offset: String(Math.max(0, Math.floor(input.offset))),
		limit: String(Math.max(1, Math.floor(input.limit)))
	});
	const query = input.query?.trim();
	if (query) params.set('q', query);
	const response = await fetch(`/api/magician/v2/notes/audio?${params}`, {
		signal: input.signal
	});
	if (!response.ok) throw requestError(response, await responseMessage(response));
	return await response.json() as AudioNotePage;
}

export async function uploadAudioNote(
	note: AudioNoteUpload,
	fetcher: typeof fetch = fetch
): Promise<AudioNoteReceipt> {
	const form = new FormData();
	form.append('file', new File([note.audio], note.originalFilename, { type: note.mimeType }));
	form.append('note_id', note.noteId);
	form.append('captured_at', note.capturedAt);
	form.append('source_surface', note.sourceSurface);
	form.append('duration_ms', String(Math.max(0, Math.floor(note.durationMs))));
	if (note.transcript?.trim()) form.append('transcript', note.transcript.trim());

	let response: Response;
	try {
		response = await fetcher(
			'/api/magician/v2/notes/audio',
			{ method: 'POST', body: form }
		);
	} catch (error) {
		throw new AudioNoteRequestError(
			error instanceof Error ? error.message : 'Audio Notes upload could not reach the server',
			null,
			true
		);
	}
	if (!response.ok) throw requestError(response, await responseMessage(response));
	if (response.redirected || !response.headers.get('content-type')?.toLowerCase().includes('application/json')) {
		throw new AudioNoteRequestError(
			'The Audio Notes upload reached an unexpected login or proxy response; the local recording was retained.',
			response.status,
			true
		);
	}
	let payload: unknown;
	try {
		payload = await response.json();
	} catch {
		throw new AudioNoteRequestError(
			'The server returned an invalid Audio Notes receipt; the local recording was retained.',
			response.status,
			true
		);
	}
	return durableReceipt(payload, note, response.status);
}

export async function deleteAudioNote(noteId: string): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/notes/audio/${encodeURIComponent(noteId)}`,
		{ method: 'DELETE' }
	);
	if (response.status === 404 || response.status === 204) return;
	if (!response.ok) throw requestError(response, await responseMessage(response));
}

export async function fetchAudioNoteRecording(noteId: string): Promise<Blob> {
	const response = await fetch(
		`/api/magician/v2/notes/audio/${encodeURIComponent(noteId)}/recording`
	);
	if (!response.ok) throw requestError(response, await responseMessage(response));
	return await response.blob();
}
