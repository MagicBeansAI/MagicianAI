import { timedFetch } from '$lib/shared/fetch';

export interface ProgramSummary { name: string; title: string }
export interface ProgramDocument extends ProgramSummary {
	content: string;
	missions_section: string | null;
	/** Hex SHA-256 of `content`; sent back as the base of an owner save. */
	sha256: string;
	history: Array<{ snapshot: string; modified: string }>;
}
export interface SavedProgram {
	saved: boolean;
	program: string;
	created: boolean;
	unchanged: boolean;
	history_snapshot: string | null;
	sha256: string;
}

/** The document changed, appeared or vanished after it was loaded (HTTP 409). */
export class ProgramChangedError extends Error {
	constructor(message: string, readonly currentSha256: string | null) {
		super(message);
		this.name = 'ProgramChangedError';
	}
}

async function programRequest<T>(path: string, signal: AbortSignal, body?: unknown, method = 'POST'): Promise<T> {
	const response = await timedFetch(`/api/magician/v2/programs${path}`, {
		signal,
		...(body === undefined ? {} : { method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
	});
	if (!response.ok) {
		const message = await response.json().catch(() => null) as { error?: string; message?: string; current_sha256?: string | null } | null;
		if (response.status === 409 && message?.error === 'program_changed') {
			throw new ProgramChangedError(message.message || 'This program changed after you opened it.', message.current_sha256 ?? null);
		}
		throw new Error(message?.message || message?.error || `Program request failed (${response.status}).`);
	}
	return response.json() as Promise<T>;
}

export async function listPrograms(signal: AbortSignal): Promise<ProgramSummary[]> {
	const result = await programRequest<{ programs: ProgramSummary[] }>('', signal);
	return result.programs;
}

export function readProgram(name: string, signal: AbortSignal): Promise<ProgramDocument> {
	return programRequest(`/${encodeURIComponent(name)}`, signal);
}

export function restoreProgram(name: string, snapshot: string, signal: AbortSignal): Promise<unknown> {
	return programRequest(`/${encodeURIComponent(name)}/revert`, signal, { snapshot });
}

/** Owner save: `baseSha256` is the `sha256` of the version being edited. */
export function saveProgram(name: string, content: string, baseSha256: string, signal: AbortSignal): Promise<SavedProgram> {
	return programRequest(`/${encodeURIComponent(name)}`, signal, { content, base_sha256: baseSha256 }, 'PUT');
}
