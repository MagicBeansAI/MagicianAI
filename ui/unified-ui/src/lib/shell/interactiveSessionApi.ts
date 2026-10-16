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
	/** The program survives a Magician restart (tmux-hosted). */
	persistent?: boolean;
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

async function postStdin(sessionId: string, input: string): Promise<Response> {
	const response = await timedFetch(
		`/api/magician/v2/interactive-sessions/${encodeURIComponent(sessionId)}/stdin`,
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

type StdinBatch = {
	data: string;
	promise: Promise<Response>;
	resolve: (response: Response) => void;
	reject: (error: unknown) => void;
};

type StdinQueue = { inFlight: boolean; next: StdinBatch | null };

const stdinQueues = new Map<string, StdinQueue>();

function newBatch(): StdinBatch {
	let resolve!: (response: Response) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<Response>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { data: '', promise, resolve, reject };
}

async function drainStdinQueue(sessionId: string, queue: StdinQueue): Promise<void> {
	queue.inFlight = true;
	try {
		while (queue.next) {
			const batch = queue.next;
			queue.next = null;
			try {
				batch.resolve(await postStdin(sessionId, batch.data));
			} catch (error) {
				batch.reject(error);
			}
		}
	} finally {
		queue.inFlight = false;
		if (stdinQueues.get(sessionId) === queue && !queue.next) stdinQueues.delete(sessionId);
	}
}

/**
 * Write to a session's stdin, in order. Keystrokes used to be one
 * independent POST each, so two in flight could land reversed. Now one
 * request per session is in flight at a time, and whatever is typed while
 * it is out is sent as the next request — so a slow link costs one round
 * trip of latency, not one per key. Every caller whose bytes rode in a
 * request gets that request's response.
 */
export function writeInteractiveSessionStdin(sessionId: string, input: string): Promise<Response> {
	let queue = stdinQueues.get(sessionId);
	if (!queue) {
		queue = { inFlight: false, next: null };
		stdinQueues.set(sessionId, queue);
	}
	if (!queue.next) queue.next = newBatch();
	queue.next.data += input;
	const { promise } = queue.next;
	if (!queue.inFlight) void drainStdinQueue(sessionId, queue);
	return promise;
}

/**
 * Text submitted from the composer as one line of input. Multi-line text is
 * wrapped as a bracketed paste so a newline inside it is not an early Enter;
 * the trailing carriage return then submits it.
 */
export function composerSubmissionBytes(text: string): string {
	if (!/[\r\n]/.test(text)) return `${text}\r`;
	return `\x1b[200~${text}\x1b[201~\r`;
}

/** Set the PTY window to the viewer's grid. */
export async function resizeInteractiveSession(
	sessionId: string,
	rows: number,
	cols: number
): Promise<Response> {
	return timedFetch(`/api/magician/v2/interactive-sessions/${encodeURIComponent(sessionId)}/resize`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ rows, cols })
	});
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
