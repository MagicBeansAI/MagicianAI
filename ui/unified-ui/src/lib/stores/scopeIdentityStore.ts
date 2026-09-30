import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';

export interface ScopeIdentityState {
	principal: string;
	workspace: string;
	isResolved: boolean;
}

export interface AuthenticatedScopeSession {
	identity: { name: string; display_name: string };
	method: string;
	principal: string;
	workspace: string;
	workspaces: Array<{
		id: string;
		display_name: string;
		description?: string | null;
		is_default: boolean;
	}>;
}

const PRINCIPAL_STORAGE_KEY = 'magician:scope:principal';
const WORKSPACE_STORAGE_KEY = 'magician:scope:workspace';
const BEARER_STORAGE_KEY = 'magician:auth:scoped-bearer';
const LEGACY_CHAT_PRINCIPAL_KEY = 'chat_principal';
const DEFAULT_SCOPE_PRINCIPAL = 'anonymous';
const DEFAULT_SCOPE_WORKSPACE = 'default';

function normalizeStoredPrincipal(value: string | null | undefined): string {
	const trimmed = value?.trim();
	if (!trimmed || trimmed.length === 0 || trimmed === 'default') {
		return DEFAULT_SCOPE_PRINCIPAL;
	}
	return trimmed;
}

function normalizeStoredWorkspace(value: string | null | undefined): string {
	const trimmed = value?.trim();
	if (!trimmed || trimmed.length === 0) {
		return DEFAULT_SCOPE_WORKSPACE;
	}
	return trimmed;
}

function loadInitialScope(): ScopeIdentityState {
	if (!browser) {
		return {
			principal: DEFAULT_SCOPE_PRINCIPAL,
			workspace: DEFAULT_SCOPE_WORKSPACE,
			isResolved: false
		};
	}

	try {
		// A persisted display scope without its session bearer is not authority.
		// This also prevents a logout followed by reload from resurrecting the
		// previous owner's scope in storage keys and event guards.
		if (!loadBearerToken()) {
			return {
				principal: DEFAULT_SCOPE_PRINCIPAL,
				workspace: DEFAULT_SCOPE_WORKSPACE,
				isResolved: false
			};
		}
		const principal = normalizeStoredPrincipal(
			localStorage.getItem(PRINCIPAL_STORAGE_KEY)
			|| localStorage.getItem(LEGACY_CHAT_PRINCIPAL_KEY)
		);
		const workspace = normalizeStoredWorkspace(localStorage.getItem(WORKSPACE_STORAGE_KEY));
		return {
			principal,
			workspace,
			isResolved:
				localStorage.getItem(PRINCIPAL_STORAGE_KEY) !== null
				|| localStorage.getItem(WORKSPACE_STORAGE_KEY) !== null
		};
	} catch {
		return {
			principal: DEFAULT_SCOPE_PRINCIPAL,
			workspace: DEFAULT_SCOPE_WORKSPACE,
			isResolved: false
		};
	}
}

function clearPersistedScope(): void {
	if (!browser) return;
	try {
		localStorage.removeItem(PRINCIPAL_STORAGE_KEY);
		localStorage.removeItem(WORKSPACE_STORAGE_KEY);
		localStorage.removeItem(LEGACY_CHAT_PRINCIPAL_KEY);
	} catch {
		// The in-memory reset still takes effect when localStorage is unavailable.
	}
}

const { subscribe, set, update } = writable<ScopeIdentityState>(loadInitialScope());

function persistScope(principal: string, workspace: string): void {
	if (!browser) return;
	try {
		localStorage.setItem(PRINCIPAL_STORAGE_KEY, principal);
		localStorage.setItem(WORKSPACE_STORAGE_KEY, workspace);
	} catch {
		// ignore localStorage failures
	}
}

function normalizeScopeValue(value: string | null | undefined): string | null {
	if (!value) return null;
	const trimmed = value.trim();
	return trimmed.length > 0 ? trimmed : null;
}

export const scopeIdentityStore = {
	subscribe,
	reset(): void {
		setCurrentScopeBearerToken(null);
		confirmCurrentScopeCredentialIdentity();
		clearPersistedScope();
		set({
			principal: DEFAULT_SCOPE_PRINCIPAL,
			workspace: DEFAULT_SCOPE_WORKSPACE,
			isResolved: false
		});
	},
	observe(principal?: string | null, workspace?: string | null): void {
		const nextPrincipal = normalizeScopeValue(principal);
		const nextWorkspace = normalizeScopeValue(workspace);
		if (!nextPrincipal || !nextWorkspace) return;

		persistScope(nextPrincipal, nextWorkspace);
		update((state) => {
			if (
				state.principal === nextPrincipal
				&& state.workspace === nextWorkspace
				&& state.isResolved
			) {
				return state;
			}
			return {
				principal: nextPrincipal,
				workspace: nextWorkspace,
				isResolved: true
			};
		});
	}
};

export function getCurrentScopeIdentity(): { principal: string; workspace: string } {
	const scope = get(scopeIdentityStore);
	return {
		principal: normalizeStoredPrincipal(scope.principal),
		workspace: normalizeStoredWorkspace(scope.workspace)
	};
}

function loadBearerToken(): string {
	if (!browser) return '';
	try {
		return sessionStorage.getItem(BEARER_STORAGE_KEY)?.trim() ?? '';
	} catch {
		return '';
	}
}

let currentBearerToken = loadBearerToken();
// In-process authority fence for UI work that must not cross a bearer
// rotation before the visible principal/workspace store catches up.
let currentScopeCredentialRevision = 1;
// A persisted/native bearer is not paired with its visible identity until the
// authenticated session endpoint confirms those claims in this process.
let confirmedScopeCredentialRevision = currentBearerToken ? 0 : currentScopeCredentialRevision;

function confirmCurrentScopeCredentialIdentity(): void {
	confirmedScopeCredentialRevision = currentScopeCredentialRevision;
}

// One session-loss redirect per token loss; rearmed when a token is installed.
let sessionLossRedirectArmed = true;
let nativeBearerReady: Promise<void> | null = null;
let nativeBearerOrigin: string | null = null;
let nativeBearerRevision: number | null = null;

function persistCurrentBearerToken(): void {
	if (!browser) return;
	try {
		if (currentBearerToken) sessionStorage.setItem(BEARER_STORAGE_KEY, currentBearerToken);
		else sessionStorage.removeItem(BEARER_STORAGE_KEY);
	} catch {
		// The in-memory token remains usable when sessionStorage is unavailable.
	}
}

function isTauriRuntime(): boolean {
	return browser && typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

async function invokeNativeBearer(command: string, token?: string | null): Promise<void> {
	const { invoke } = await import('@tauri-apps/api/core');
	if (!nativeBearerOrigin || nativeBearerRevision === null) throw new Error('The desktop server connection is not ready.');
	nativeBearerRevision = await invoke<number>(command, { token, expectedOrigin: nativeBearerOrigin, expectedRevision: nativeBearerRevision });
}

/**
 * Restore the native origin-bound session. Browser sessionStorage may belong
 * to a previously selected backend and must never overwrite native custody.
 */
function ensureNativeBearerHydrated(restoreBearer = true): Promise<void> {
	if (!isTauriRuntime()) return Promise.resolve();
	if (nativeBearerReady) return nativeBearerReady;
	const hydrationCredentialRevision = currentScopeCredentialRevision;
	nativeBearerReady = (async () => {
		const { invoke } = await import('@tauri-apps/api/core');
		const snapshot = await invoke<{ origin: string; token: string | null; revision: number }>('get_magician_connection_auth');
		nativeBearerOrigin = snapshot.origin;
		nativeBearerRevision = snapshot.revision;
		const inherited = snapshot.token?.trim() ?? '';
		// A login/workspace rotation may install a newer browser credential while
		// the native read is in flight. Never let the older inherited value win;
		// synchronizeNativeBearer has already queued the newer token behind us.
		if (restoreBearer &&
			currentScopeCredentialRevision === hydrationCredentialRevision) {
			currentBearerToken = inherited;
			currentScopeCredentialRevision += 1;
			persistCurrentBearerToken();
		}
	})().catch(() => {
		// Without native custody, a restored browser token has no server binding.
		if (restoreBearer && currentScopeCredentialRevision === hydrationCredentialRevision) {
			currentBearerToken = '';
			currentScopeCredentialRevision += 1;
			persistCurrentBearerToken();
		}
	});
	return nativeBearerReady;
}

function synchronizeNativeBearer(token: string | null): void {
	if (!isTauriRuntime()) return;
	const previous = ensureNativeBearerHydrated(false);
	nativeBearerReady = previous
		.catch(() => undefined)
		.then(() => invokeNativeBearer('set_magician_bearer_token', token))
		.then(() => undefined)
		.catch(() => undefined);
}

/** Install the opaque bearer returned by login or session-scope rotation. */
export function setCurrentScopeBearerToken(token: string | null | undefined): void {
	const nextToken = token?.trim() ?? '';
	if (nextToken !== currentBearerToken) {
		currentBearerToken = nextToken;
		currentScopeCredentialRevision += 1;
	}
	persistCurrentBearerToken();
	synchronizeNativeBearer(currentBearerToken || null);
	// A fresh credential arms the next session-loss redirect.
	sessionLossRedirectArmed = true;
}

export function getCurrentScopeBearerToken(): string {
	return currentBearerToken;
}

/** Monotonic, process-local fence only; it never exposes bearer material. */
export function getCurrentScopeCredentialRevision(): number {
	return currentScopeCredentialRevision;
}

function scopeCredentialMatches(token: string, revision: number): boolean {
	return revision === currentScopeCredentialRevision
		&& token === getCurrentScopeBearerToken();
}

/** A response may change session state only when its request carried the
 * exact credential that is still current. Exported for the auth-race
 * regression contract; it does not expose credential material. */
export function scopeCredentialOwnsRequestVerdict(
	token: string,
	revision: number,
	authorization: string | null
): boolean {
	return token !== ''
		&& authorization === `Bearer ${token}`
		&& scopeCredentialMatches(token, revision);
}

/** True only after the visible principal/workspace was confirmed for this bearer. */
export function scopeCredentialIdentityIsCurrent(revision: number): boolean {
	return revision > 0 && revision === currentScopeCredentialRevision &&
		revision === confirmedScopeCredentialRevision;
}

/** Hydration-aware session presence for load-time gating: awaits the native
 *  credential store before answering, so a Tauri webview with a valid
 *  session is not misread as signed-out. Presence only — liveness is the
 *  mount-time session refresh and the 401 re-gate. */
export async function hasHydratedScopeBearer(): Promise<boolean> {
	await ensureNativeBearerHydrated();
	return getCurrentScopeBearerToken() !== '';
}

/** An auth failure that keeps its status and error code.
 *
 *  Carrying only the human message made two very different failures read the
 *  same to any caller: a wrong password (401 `invalid_credentials`) and a
 *  throttled door (429 `too_many_attempts`) both arrived as a bare `Error`,
 *  so nothing downstream could tell "try again" from "wait". */
export class AuthRequestError extends Error {
	readonly status: number;
	readonly code: string | null;

	constructor(message: string, status: number, code: string | null) {
		super(message);
		this.name = 'AuthRequestError';
		this.status = status;
		this.code = code;
	}
}

async function authError(response: Response): Promise<Error> {
	const fallback = `Authentication request failed (${response.status})`;
	try {
		const payload = await response.json() as { message?: unknown; error?: unknown };
		const code = typeof payload.error === 'string' && payload.error.trim() ? payload.error : null;
		if (typeof payload.message === 'string' && payload.message.trim()) {
			return new AuthRequestError(payload.message, response.status, code);
		}
		if (code) {
			return new AuthRequestError(code, response.status, code);
		}
	} catch {
		// Use the bounded status-derived message.
	}
	return new AuthRequestError(fallback, response.status, null);
}

/** Create and install a bearer bound to the requested owned workspace. */
export async function loginScopeSession(input: {
	username: string;
	password: string;
	workspace?: string;
}): Promise<AuthenticatedScopeSession> {
	const response = await fetch('/api/magician/v2/auth/login', {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({
			// Trimmed because a phone keyboard and an autofill entry both like to
			// leave a trailing space, and the server matches identity names
			// exactly. The password is never trimmed — leading and trailing
			// spaces are legitimate characters in one.
			username: input.username.trim(),
			password: input.password,
			workspace: input.workspace?.trim() || undefined
		})
	});
	if (!response.ok) throw await authError(response);
	const login = await response.json() as {
		token: string;
		principal: string;
		workspace: string;
		identity: { name: string; display_name: string };
	};
	setCurrentScopeBearerToken(login.token);
	scopeIdentityStore.observe(login.principal, login.workspace);
	const session = await refreshScopeSession();
	if (!session) throw new Error('The new session could not be verified.');
	return session;
}

/** Resolve the installed bearer and make its claims the visible UI scope. */
export async function refreshScopeSession(): Promise<AuthenticatedScopeSession | null> {
	// Native webviews start with empty sessionStorage; the bearer may live in
	// the native credential store. Await hydration BEFORE the synchronous
	// token check, or every Tauri-opened surface (attention window, overlays)
	// reads '' and presents as signed-out despite a valid native session.
	await ensureNativeBearerHydrated();
	const token = getCurrentScopeBearerToken();
	if (!token) return null;
	const credentialRevision = currentScopeCredentialRevision;
	const response = await fetch('/api/magician/v2/auth/session', {
		headers: scopedRequestHeaders()
	});
	// Session probes can overlap a login, workspace rotation, or logout. A
	// verdict belongs only to the credential that initiated it: an older 401
	// must never erase a newly minted bearer, and an older 200 must never
	// confirm its principal/workspace as the identity of a newer bearer.
	if (!scopeCredentialMatches(token, credentialRevision)) {
		return null;
	}
	if (response.status === 401) {
		scopeIdentityStore.reset();
		return null;
	}
	if (!response.ok) throw await authError(response);
	const session = await response.json() as AuthenticatedScopeSession;
	// JSON decoding yields too. Keep the same authority fence on both sides of
	// the asynchronous response body read.
	if (!scopeCredentialMatches(token, credentialRevision)) {
		return null;
	}
	scopeIdentityStore.observe(session.principal, session.workspace);
	confirmCurrentScopeCredentialIdentity();
	return session;
}

/** Rotate the session token so its claim, not a request selector, switches workspace. */
export async function switchScopeWorkspace(workspace: string): Promise<AuthenticatedScopeSession> {
	const response = await fetch('/api/magician/v2/auth/session/scope', {
		method: 'POST',
		headers: scopedRequestHeaders({ 'content-type': 'application/json' }),
		body: JSON.stringify({ workspace: workspace.trim() })
	});
	if (!response.ok) throw await authError(response);
	const rotated = await response.json() as { token: string; workspace: string };
	setCurrentScopeBearerToken(rotated.token);
	const session = await refreshScopeSession();
	if (!session) throw new Error('The rotated session could not be verified.');
	return session;
}

export async function logoutScopeSession(): Promise<void> {
	const token = getCurrentScopeBearerToken();
	if (token) {
		await fetch('/api/magician/v2/auth/logout', {
			method: 'POST',
			headers: scopedRequestHeaders()
		}).catch(() => undefined);
	}
	scopeIdentityStore.reset();
	await (nativeBearerReady ?? Promise.resolve());
}

/** Browser WebSockets cannot set Authorization; use an auth-only subprotocol. */
export function scopedWebSocketProtocols(protocols: string[] = []): string[] {
	const token = getCurrentScopeBearerToken();
	const applicationProtocols = protocols.filter(
		(protocol) => !protocol.trim().startsWith('magician-bearer.')
	);
	return token ? [...applicationProtocols, `magician-bearer.${token}`] : applicationProtocols;
}

/** Native views use the selected engine, even when their assets come from Vite.
 * Browser clients keep their existing same-origin/dev-proxy routing. */
export function scopedMagicianWebSocketUrl(path: string, browserOrigin: string): string {
	const origin = isTauriRuntime() ? nativeBearerOrigin : browserOrigin;
	if (!origin) throw new Error('The desktop server connection is not ready.');
	const url = new URL(path, origin);
	if (!path.startsWith('/api/magician/') || path.startsWith('//')) {
		throw new Error('Invalid Magician WebSocket path.');
	}
	url.protocol = url.protocol === 'https:' || url.protocol === 'wss:' ? 'wss:' : 'ws:';
	return url.href;
}

export function scopedRequestHeaders(headers?: HeadersInit): Headers {
	const next = new Headers(headers);
	// Scope is a bearer claim. Strip legacy assertions even when an older call
	// site supplied them explicitly, then attach the scoped credential once.
	next.delete('X-Principal');
	next.delete('X-Workspace');
	const token = getCurrentScopeBearerToken();
	if (token && !next.has('Authorization')) {
		next.set('Authorization', `Bearer ${token}`);
	}
	return next;
}

export function appendCurrentScopeQuery(params?: URLSearchParams): URLSearchParams {
	const next = params ? new URLSearchParams(params) : new URLSearchParams();
	next.delete('principal');
	next.delete('workspace');
	return next;
}

let scopedFetchInstalled = false;

export function isSameOriginMagicianApiUrl(rawUrl: string, pageOrigin: string): boolean {
	try {
		const page = new URL(pageOrigin);
		const url = new URL(rawUrl, page);
		return url.origin === page.origin
			&& !url.username
			&& !url.password
			&& url.pathname.startsWith('/api/magician/');
	} catch {
		return false;
	}
}

function isMagicianApiRequest(input: RequestInfo | URL): boolean {
	if (!browser) return false;
	const rawUrl =
		typeof input === 'string'
			? input
			: input instanceof URL
				? input.toString()
				: input.url;
	return isSameOriginMagicianApiUrl(rawUrl, window.location.origin);
}

function requestPath(input: RequestInfo | URL): string {
	try {
		const url = typeof input === 'string'
			? new URL(input, window.location.origin)
			: input instanceof URL
				? input
				: new URL(input.url, window.location.origin);
		return url.pathname;
	} catch {
		return '';
	}
}

/** Paths whose 401 is not a verdict on the session — they must not trigger
 *  the session-loss re-gate.
 *
 *  - Auth endpoints answer WITH 401s by design (a probe's stale token is an
 *    answer, not a verdict).
 *  - The resource-authority admin API is gated by its own key
 *    (`RESOURCE_AUTHORITY_API_KEY`), compared byte-for-byte against whatever
 *    `Authorization` carries. With that key set and no RA key in the browser,
 *    the scoped wrapper fills in the session bearer, RA rejects it with 401,
 *    and the re-gate read that as a revoked session — wiping a valid token
 *    and bouncing every sign-in back to /login from the app shell's
 *    `loadFreezeStatus()` on mount. Its doc says it "is not an authentication
 *    layer for the general magician API — do not treat it as one." */
export function isAuthVerdictPath(path: string): boolean {
	return path.startsWith('/api/magician/v2/auth/')
		|| path.startsWith('/api/magician/v2/resource-authority/');
}

export function installScopedApiFetch(): void {
	if (!browser || scopedFetchInstalled) return;
	const originalFetch = window.fetch.bind(window);
	window.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
		if (!isMagicianApiRequest(input)) {
			return originalFetch(input, init);
		}
		await ensureNativeBearerHydrated();
		const requestBearerToken = getCurrentScopeBearerToken();
		const requestCredentialRevision = currentScopeCredentialRevision;

		const mergedHeaders = new Headers(
			input instanceof Request ? input.headers : undefined
		);
		if (init?.headers) {
			const initHeaders = new Headers(init.headers);
			initHeaders.forEach((value, key) => {
				mergedHeaders.set(key, value);
			});
		}
		const headers = scopedRequestHeaders(mergedHeaders);

		let destination: RequestInfo | URL = input;
		if (isTauriRuntime()) {
			if (!nativeBearerOrigin) throw new Error('The desktop server connection is not ready.');
			const raw = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
			const source = new URL(raw, window.location.href);
			destination = new URL(source.pathname + source.search, nativeBearerOrigin);
		}
		const options = { ...init, headers, ...(isTauriRuntime() ? { redirect: 'error' as const } : {}) };
		const response = input instanceof Request
			? await originalFetch(isTauriRuntime()
				? new Request(destination, new Request(input, options))
				: new Request(input, options))
			: await originalFetch(destination, options);

		// Session-death re-gate: a 401 on a call that carried our bearer means
		// the session was revoked or expired after this surface mounted — the
		// mount-time gate already ran, so without this the surface degrades
		// into error toasts forever. Fires once per token loss (rearmed when a
		// new token is installed), never for auth endpoints' own verdicts, and
		// never while already on /login.
		if (
			response.status === 401
			&& scopeCredentialOwnsRequestVerdict(
				requestBearerToken,
				requestCredentialRevision,
				headers.get('Authorization')
			)
			&& !isAuthVerdictPath(requestPath(input))
			&& window.location.pathname !== '/login'
			&& sessionLossRedirectArmed
		) {
			sessionLossRedirectArmed = false;
			scopeIdentityStore.reset();
			const target = encodeURIComponent(
				window.location.pathname + window.location.search
			);
			void import('$app/navigation').then(({ goto }) =>
				goto(`/login?redirectTo=${target}`)
			);
		}
		return response;
	}) as typeof window.fetch;
	scopedFetchInstalled = true;
}

export function scopedUiStorageKey(namespace: string, ...parts: Array<string | null | undefined>): string {
	const scope = getCurrentScopeIdentity();
	const suffix = parts.map((part) => part ?? '').join(':');
	return `${namespace}:${scope.principal}:${scope.workspace}:${suffix}`;
}
