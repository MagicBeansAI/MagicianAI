import { timedFetch } from '$lib/shared/fetch';

export interface ProgramSummary { name: string; title: string }
export interface ProgramDocument extends ProgramSummary {
	content: string;
	missions_section: string | null;
	history: Array<{ snapshot: string; modified: string }>;
}

async function programRequest<T>(path: string, signal: AbortSignal, body?: unknown): Promise<T> {
	const response = await timedFetch(`/api/magician/v2/programs${path}`, {
		signal,
		...(body === undefined ? {} : { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
	});
	if (!response.ok) {
		const message = await response.json().catch(() => null) as { error?: string } | null;
		throw new Error(message?.error || `Program request failed (${response.status}).`);
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
