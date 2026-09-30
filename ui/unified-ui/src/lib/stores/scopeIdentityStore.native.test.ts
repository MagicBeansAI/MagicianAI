import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('$app/environment', () => ({ browser: true }));

beforeEach(() => {
	vi.resetModules();
	invoke.mockReset();
	const values = new Map<string, string>();
	vi.stubGlobal('sessionStorage', {
		getItem: (key: string) => values.get(key) ?? null,
		setItem: (key: string, value: string) => values.set(key, value),
		removeItem: (key: string) => values.delete(key),
		clear: () => values.clear()
	});
	localStorage.clear();
	vi.stubGlobal('window', {
		__TAURI_INTERNALS__: {},
		location: { origin: 'http://localhost:5173', href: 'http://localhost:5173/hud', pathname: '/hud', search: '' },
		fetch: vi.fn().mockResolvedValue(new Response('{}'))
	});
});
afterEach(() => vi.unstubAllGlobals());

describe('native session custody', () => {
	it('restores native authority instead of publishing a stale webview token', async () => {
		sessionStorage.setItem('magician:auth:scoped-bearer', 'old-server-token');
		invoke.mockResolvedValue({ origin: 'http://127.0.0.1:13002', revision: 1, token: 'selected-server-token' });
		const store = await import('./scopeIdentityStore');
		expect(await store.hasHydratedScopeBearer()).toBe(true);
		expect(store.getCurrentScopeBearerToken()).toBe('selected-server-token');
		expect(invoke.mock.calls).toEqual([['get_magician_connection_auth']]);
	});

	it('does not resurrect a webview token after native logout', async () => {
		sessionStorage.setItem('magician:auth:scoped-bearer', 'old-token');
		invoke.mockResolvedValue({ origin: 'http://127.0.0.1:13002', revision: 1, token: null });
		const store = await import('./scopeIdentityStore');
		expect(await store.hasHydratedScopeBearer()).toBe(false);
		expect(sessionStorage.getItem('magician:auth:scoped-bearer')).toBeNull();
	});

	it('binds a new login to the native connection snapshot', async () => {
		invoke.mockResolvedValueOnce({ origin: 'http://127.0.0.1:13002', revision: 1, token: 'old' });
		const store = await import('./scopeIdentityStore');
		store.setCurrentScopeBearerToken('new');
		await store.hasHydratedScopeBearer();
		expect(store.getCurrentScopeBearerToken()).toBe('new');
		expect(invoke).toHaveBeenLastCalledWith('set_magician_bearer_token', {
			token: 'new', expectedOrigin: 'http://127.0.0.1:13002', expectedRevision: 1
		});
	});

	it('a delayed hydration cannot erase a newer login', async () => {
		let finish!: (value: unknown) => void;
		invoke.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
		const store = await import('./scopeIdentityStore');
		const hydration = store.hasHydratedScopeBearer();
		await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
		store.setCurrentScopeBearerToken('new');
		finish({ origin: 'http://127.0.0.1:13002', revision: 1, token: 'old' });
		await hydration;
		await store.hasHydratedScopeBearer();
		expect(store.getCurrentScopeBearerToken()).toBe('new');
	});

	it('a failed older hydration cannot erase a newer login', async () => {
		let fail!: (error: Error) => void;
		invoke.mockReturnValueOnce(new Promise((_resolve, reject) => { fail = reject; }));
		const store = await import('./scopeIdentityStore');
		const hydration = store.hasHydratedScopeBearer();
		await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
		store.setCurrentScopeBearerToken('new');
		fail(new Error('credential store locked'));
		await hydration;
		await store.hasHydratedScopeBearer();
		expect(store.getCurrentScopeBearerToken()).toBe('new');
		expect(invoke).not.toHaveBeenCalledWith('set_magician_bearer_token', expect.anything());
	});

	it('fails closed when the native credential store cannot be read', async () => {
		sessionStorage.setItem('magician:auth:scoped-bearer', 'unbound-token');
		invoke.mockRejectedValue(new Error('credential store locked'));
		const store = await import('./scopeIdentityStore');
		expect(await store.hasHydratedScopeBearer()).toBe(false);
		expect(store.getCurrentScopeBearerToken()).toBe('');
	});

	it('sends native-view API requests directly to the selected server instead of the Vite proxy', async () => {
		invoke.mockResolvedValue({ origin: 'http://127.0.0.1:13002', revision: 1, token: 'selected-token' });
		const fetch = window.fetch;
		const store = await import('./scopeIdentityStore');
		store.installScopedApiFetch();
		await window.fetch('/api/magician/v2/auth/session');
		const [url, init] = vi.mocked(fetch).mock.calls[0];
		expect(String(url)).toBe('http://127.0.0.1:13002/api/magician/v2/auth/session');
		expect(new Headers(init?.headers).get('Authorization')).toBe('Bearer selected-token');
		expect(init?.redirect).toBe('error');
		expect(store.scopedMagicianWebSocketUrl('/api/magician/v2/realtime/ws?execution_id=one', 'ws://localhost:3002'))
			.toBe('ws://127.0.0.1:13002/api/magician/v2/realtime/ws?execution_id=one');
	});

	it('preserves request bodies when routing a native upload to its selected backend', async () => {
		invoke.mockResolvedValue({ origin: 'http://127.0.0.1:13002', revision: 1, token: 'selected-token' });
		const fetch = window.fetch;
		const store = await import('./scopeIdentityStore');
		store.installScopedApiFetch();
		await window.fetch(new Request('http://localhost:5173/api/magician/v2/test-upload', {
			method: 'POST', body: 'exact-upload-bytes'
		}));
		const request = vi.mocked(fetch).mock.calls[0][0] as Request;
		expect(request.url).toBe('http://127.0.0.1:13002/api/magician/v2/test-upload');
		expect(request.method).toBe('POST');
		expect(await request.text()).toBe('exact-upload-bytes');
	});
});
