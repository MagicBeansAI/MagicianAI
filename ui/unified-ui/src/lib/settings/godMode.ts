// God mode (`execution.god_mode`): owner-only settings endpoint.
import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { createSharedPoll } from '$lib/stores/sharedPoll';

export interface GodModeState {
	enabled: boolean;
	/** Approve god mode's asks without asking, except deletes, root and unseen code. */
	auto_approve: boolean;
	/** "Allow always" grants currently stored; cleared when the setting changes. */
	always_grants: number;
}

const ENDPOINT = '/api/magician/v2/settings/god-mode';

export function parseGodModeState(body: unknown): GodModeState {
	const raw = (body ?? {}) as Record<string, unknown>;
	if (typeof raw.enabled !== 'boolean') throw new Error('Malformed god mode response');
	return {
		enabled: raw.enabled,
		auto_approve: raw.auto_approve === true,
		always_grants: typeof raw.always_grants === 'number' ? raw.always_grants : 0
	};
}

async function request(path: string, init: RequestInit): Promise<GodModeState> {
	const response = await timedFetch(path, {
		...init,
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		cache: 'no-store'
	});
	const text = await response.text();
	let body: unknown = null;
	try {
		body = text ? JSON.parse(text) : null;
	} catch {
		body = null;
	}
	if (!response.ok) {
		const record = (body ?? {}) as Record<string, unknown>;
		const message = typeof record.message === 'string' ? record.message : text || `Request failed (${response.status})`;
		throw new Error(message);
	}
	return parseGodModeState(body);
}

export function fetchGodMode(): Promise<GodModeState> {
	return request(ENDPOINT, { method: 'GET' });
}

export function saveGodMode(enabled: boolean): Promise<GodModeState> {
	return request(ENDPOINT, { method: 'PUT', body: JSON.stringify({ enabled }) });
}

export function saveGodModeAutoApprove(autoApprove: boolean): Promise<GodModeState> {
	return request(ENDPOINT, { method: 'PUT', body: JSON.stringify({ auto_approve: autoApprove }) });
}

export function forgetGodModeGrants(): Promise<GodModeState> {
	return request(`${ENDPOINT}/grants`, { method: 'DELETE' });
}

/** One chat's god mode: what applies, its own choice, and the Settings default. */
export interface ChatGodModeState {
	enabled: boolean;
	override: boolean | null;
	default: boolean;
	/** Auto-approve for this chat: what applies, its own choice, the Settings default. */
	auto_approve: boolean;
	auto_approve_override: boolean | null;
	auto_approve_default: boolean;
}

export function parseChatGodModeState(body: unknown): ChatGodModeState {
	const raw = (body ?? {}) as Record<string, unknown>;
	if (typeof raw.enabled !== 'boolean') throw new Error('Malformed chat god mode response');
	return {
		enabled: raw.enabled,
		override: typeof raw.override === 'boolean' ? raw.override : null,
		default: raw.default === true,
		auto_approve: raw.auto_approve === true,
		auto_approve_override: typeof raw.auto_approve_override === 'boolean' ? raw.auto_approve_override : null,
		auto_approve_default: raw.auto_approve_default === true
	};
}

async function chatRequest(sessionId: string, init: RequestInit): Promise<ChatGodModeState> {
	const response = await timedFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/god-mode`, {
		...init,
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		cache: 'no-store'
	});
	const text = await response.text();
	let body: unknown = null;
	try {
		body = text ? JSON.parse(text) : null;
	} catch {
		body = null;
	}
	if (!response.ok) {
		const record = (body ?? {}) as Record<string, unknown>;
		throw new Error(typeof record.message === 'string' ? record.message : text || `Request failed (${response.status})`);
	}
	return parseChatGodModeState(body);
}

export function fetchChatGodMode(sessionId: string): Promise<ChatGodModeState> {
	return chatRequest(sessionId, { method: 'GET' });
}

/** `null` follows the Settings default again. */
export function saveChatGodMode(sessionId: string, enabled: boolean | null): Promise<ChatGodModeState> {
	return chatRequest(sessionId, { method: 'PUT', body: JSON.stringify({ enabled }) });
}

/** `null` follows the Settings default again. */
export function saveChatAutoApprove(sessionId: string, autoApprove: boolean | null): Promise<ChatGodModeState> {
	return chatRequest(sessionId, { method: 'PUT', body: JSON.stringify({ auto_approve: autoApprove }) });
}

/** Super god mode (`execution.super_god_mode`, debug): nothing asks while it is on. */
export interface SuperGodModeState {
	active: boolean;
	enabled: boolean;
	/** When it turns itself off (Unix ms); null = until turned off. */
	until_ms: number | null;
	remaining_secs: number | null;
}

export type SuperGodModeDuration = '1h' | 'forever';

const SUPER_ENDPOINT = '/api/magician/v2/settings/super-god-mode';

export function parseSuperGodModeState(body: unknown): SuperGodModeState {
	const raw = (body ?? {}) as Record<string, unknown>;
	if (typeof raw.active !== 'boolean') throw new Error('Malformed super god mode response');
	return {
		active: raw.active,
		enabled: raw.enabled === true,
		until_ms: typeof raw.until_ms === 'number' ? raw.until_ms : null,
		remaining_secs: typeof raw.remaining_secs === 'number' ? raw.remaining_secs : null
	};
}

async function superRequest(init: RequestInit): Promise<SuperGodModeState> {
	const response = await timedFetch(SUPER_ENDPOINT, {
		...init,
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		cache: 'no-store'
	});
	const text = await response.text();
	let body: unknown = null;
	try {
		body = text ? JSON.parse(text) : null;
	} catch {
		body = null;
	}
	if (!response.ok) {
		const record = (body ?? {}) as Record<string, unknown>;
		throw new Error(typeof record.message === 'string' ? record.message : text || `Request failed (${response.status})`);
	}
	return parseSuperGodModeState(body);
}

export function fetchSuperGodMode(): Promise<SuperGodModeState> {
	return superRequest({ method: 'GET' });
}

/** Turn it on for an hour or until turned off; `null` turns it off. */
export function saveSuperGodMode(duration: SuperGodModeDuration | null): Promise<SuperGodModeState> {
	const body = duration ? { enabled: true, duration } : { enabled: false };
	return superRequest({ method: 'PUT', body: JSON.stringify(body) });
}

/** The banner and the Settings panel share one poll. */
export const superGodModePoll = createSharedPoll<SuperGodModeState>({
	fetcher: fetchSuperGodMode,
	idleMs: 30_000,
	fastMs: 5_000
});

/** "52 min left", "until you turn it off". */
export function describeSuperGodModeEnd(state: SuperGodModeState): string {
	if (state.remaining_secs === null) return 'until you turn it off';
	const minutes = Math.max(1, Math.ceil(state.remaining_secs / 60));
	return `${minutes} min left`;
}
