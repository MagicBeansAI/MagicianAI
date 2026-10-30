// Harness credentials (`execution.harness_auth`): owner-only settings endpoint.
import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type HarnessAuthMode = 'login_first' | 'login_only' | 'magician';

export const HARNESS_AUTH_ENGINES = ['claude_code', 'codex', 'grok', 'agy', 'pi'] as const;
export type HarnessAuthEngine = (typeof HARNESS_AUTH_ENGINES)[number];

export type HarnessSignIn = 'signed_in' | 'expired' | 'signed_out';
/** How Magician's key can stand in when the CLI's own sign-in fails. */
export type HarnessKeyFallback = 'ask' | 'always' | 'never';

export interface HarnessAuthFailure {
	/** `authentication`, `credit`, `rate_limit` or `unavailable`. */
	class: string;
	/** The CLI's own reason (bounded, secrets redacted). */
	reason: string;
	remedy: string;
	at_ms: number | null;
}

export interface HarnessAuthEngineStatus {
	mode: HarnessAuthMode;
	signed_in: boolean;
	/** Older servers send only `signed_in`; it is derived from that then. */
	sign_in: HarnessSignIn;
	sign_in_detail: string | null;
	remedy: string | null;
	last_failure: HarnessAuthFailure | null;
	uses_magician_credentials: boolean;
	key_fallback: HarnessKeyFallback;
	magician_key_available: boolean;
}

export interface HarnessAuthState {
	mode: HarnessAuthMode;
	/** Per-engine overrides; an engine absent here follows `mode`. */
	engines: Partial<Record<HarnessAuthEngine, HarnessAuthMode>>;
	status: Partial<Record<HarnessAuthEngine, HarnessAuthEngineStatus>>;
}

const ENDPOINT = '/api/magician/v2/settings/harness-auth';
const MODES: HarnessAuthMode[] = ['login_first', 'login_only', 'magician'];

function isMode(value: unknown): value is HarnessAuthMode {
	return typeof value === 'string' && (MODES as string[]).includes(value);
}

const SIGN_INS: HarnessSignIn[] = ['signed_in', 'expired', 'signed_out'];
const KEY_FALLBACKS: HarnessKeyFallback[] = ['ask', 'always', 'never'];

function text(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value : null;
}

function parseFailure(value: unknown): HarnessAuthFailure | null {
	if (!value || typeof value !== 'object') return null;
	const record = value as Record<string, unknown>;
	const reason = text(record.reason);
	if (!reason) return null;
	return {
		class: text(record.class) ?? 'unknown',
		reason,
		remedy: text(record.remedy) ?? '',
		at_ms: typeof record.at_ms === 'number' ? record.at_ms : null
	};
}

function isEngine(value: string): value is HarnessAuthEngine {
	return (HARNESS_AUTH_ENGINES as readonly string[]).includes(value);
}

export function parseHarnessAuthState(body: unknown): HarnessAuthState {
	const raw = (body ?? {}) as Record<string, unknown>;
	if (!isMode(raw.mode)) throw new Error('Malformed harness credentials response');
	const engines: HarnessAuthState['engines'] = {};
	for (const [engine, mode] of Object.entries((raw.engines ?? {}) as Record<string, unknown>)) {
		if (isEngine(engine) && isMode(mode)) engines[engine] = mode;
	}
	const status: HarnessAuthState['status'] = {};
	for (const [engine, row] of Object.entries((raw.status ?? {}) as Record<string, unknown>)) {
		const record = (row ?? {}) as Record<string, unknown>;
		if (!isEngine(engine) || !isMode(record.mode)) continue;
		const signedIn = record.signed_in === true;
		const signIn = (SIGN_INS as unknown[]).includes(record.sign_in)
			? (record.sign_in as HarnessSignIn)
			: signedIn
				? 'signed_in'
				: 'signed_out';
		const keyFallback = (KEY_FALLBACKS as unknown[]).includes(record.key_fallback)
			? (record.key_fallback as HarnessKeyFallback)
			: record.mode === 'magician'
				? 'always'
				: record.mode === 'login_only'
					? 'never'
					: 'ask';
		status[engine] = {
			mode: record.mode,
			signed_in: signedIn,
			sign_in: signIn,
			sign_in_detail: text(record.sign_in_detail),
			remedy: text(record.remedy),
			last_failure: parseFailure(record.last_failure),
			uses_magician_credentials: record.uses_magician_credentials === true,
			key_fallback: keyFallback,
			magician_key_available: record.magician_key_available === true
		};
	}
	return { mode: raw.mode, engines, status };
}

async function request(init: RequestInit): Promise<HarnessAuthState> {
	const response = await timedFetch(ENDPOINT, {
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
	return parseHarnessAuthState(body);
}

export function fetchHarnessAuth(): Promise<HarnessAuthState> {
	return request({ method: 'GET' });
}

/** Change the default mode, or one engine's override (`null` follows the default). */
export function saveHarnessAuth(update: {
	mode?: HarnessAuthMode;
	engines?: Partial<Record<HarnessAuthEngine, HarnessAuthMode | null>>;
}): Promise<HarnessAuthState> {
	return request({ method: 'PUT', body: JSON.stringify(update) });
}

/** Engine-picker wording for an engine row's `credentials` field. */
export function credentialsLabel(value: string | null | undefined): string | null {
	switch (value) {
		case 'login':
			return 'your login';
		case 'magician_key':
			return 'Magician’s key';
		case 'expired':
			return 'session expired';
		case 'none':
			return 'not signed in';
		default:
			return null;
	}
}

/** The settings row's sign-in wording, e.g. "Session expired — run `grok login`". */
export function signInLabel(row: HarnessAuthEngineStatus | undefined): string {
	if (!row) return '—';
	switch (row.sign_in) {
		case 'signed_in':
			return 'Signed in';
		case 'expired':
			return row.remedy ? `Session expired — ${lowerFirst(row.remedy)}` : 'Session expired';
		default:
			return row.remedy ? `Not signed in — ${lowerFirst(row.remedy)}` : 'Not signed in';
	}
}

function lowerFirst(value: string): string {
	const trimmed = value.trim().replace(/\.$/, '');
	return trimmed ? trimmed[0].toLowerCase() + trimmed.slice(1) : trimmed;
}

/** `Codex · your login`; the bare name when there is no label. */
export function withCredentials(name: string, value: string | null | undefined): string {
	const label = credentialsLabel(value);
	return label ? `${name} · ${label}` : name;
}
