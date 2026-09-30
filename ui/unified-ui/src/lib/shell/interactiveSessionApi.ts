import { get } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

export type InteractiveSessionBuffer = {
	session_id: string;
	program?: string | null;
	ui_thread_id?: string | null;
	bytes_b64: string;
	start_offset: number;
	end_offset: number;
	alive: boolean;
	exit_code?: number | null;
};

export type InteractiveSessionSummary = {
	session_id: string;
	program: string;
	ui_thread_id?: string | null;
	working_dir?: string | null;
	created_at_ms: number;
	last_output_at_ms?: number | null;
	last_input_at_ms?: number | null;
	replay_start_offset: number;
	replay_end_offset: number;
	alive: boolean;
	exit_code?: number | null;
};

export type InteractiveSessionList = {
	sessions?: InteractiveSessionSummary[];
};

export type LiveInteractiveSession = {
	id: string;
	program: string | null;
	uiThreadId: string | null;
	workingDir: string | null;
	createdAtMs: number;
	lastOutputAtMs: number | null;
	lastInputAtMs: number | null;
	replayStartOffset: number;
	replayEndOffset: number;
	alive: boolean;
	exitCode: number | null;
	lastSeenMs: number;
};

function encodeBytesToBase64(str: string): string {
	const bytes = new TextEncoder().encode(str);
	let binary = '';
	for (let i = 0; i < bytes.byteLength; i++) {
		binary += String.fromCharCode(bytes[i]);
	}
	return btoa(binary);
}

export async function writeInteractiveSessionStdin(
	sessionId: string,
	input: string
): Promise<Response> {
	const scope = get(scopeIdentityStore);
	const params = new URLSearchParams();
	const response = await timedFetch(
		`/api/magician/v2/interactive-sessions/${encodeURIComponent(sessionId)}/stdin?${params.toString()}`,
		{
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ bytes_b64: encodeBytesToBase64(input) })
		}
	);
	if (response.ok && typeof window !== 'undefined') {
		window.dispatchEvent(
			new CustomEvent('magician:interactive-session-touched', {
				detail: { sessionId, kind: 'input', timestampMs: Date.now() }
			})
		);
	}
	return response;
}

export async function fetchInteractiveSessionBuffer(
	sessionId: string
): Promise<InteractiveSessionBuffer> {
	const scope = get(scopeIdentityStore);
	const params = new URLSearchParams();
	const response = await timedFetch(
		`/api/magician/v2/interactive-sessions/${encodeURIComponent(sessionId)}/buffer?${params.toString()}`
	);
	const payload = (await response.json().catch(() => null)) as InteractiveSessionBuffer | null;
	if (!response.ok || !payload) {
		throw new Error((payload as { error?: string } | null)?.error || `server returned ${response.status}`);
	}
	return payload;
}

export function normalizeInteractiveSessionSummary(
	session: InteractiveSessionSummary,
	nowMs = Date.now()
): LiveInteractiveSession {
	return {
		id: session.session_id,
		program: session.program || null,
		uiThreadId: session.ui_thread_id ?? null,
		workingDir: session.working_dir ?? null,
		createdAtMs: session.created_at_ms || nowMs,
		lastOutputAtMs: session.last_output_at_ms ?? null,
		lastInputAtMs: session.last_input_at_ms ?? null,
		replayStartOffset: session.replay_start_offset ?? 0,
		replayEndOffset: session.replay_end_offset ?? 0,
		alive: session.alive !== false,
		exitCode: session.exit_code ?? null,
		lastSeenMs: nowMs
	};
}

export function scopedInteractiveSessionParams(uiThreadId?: string | null): URLSearchParams {
	const scope = get(scopeIdentityStore);
	const params = new URLSearchParams();
	if (uiThreadId?.trim()) params.set('ui_thread_id', uiThreadId.trim());
	return params;
}
