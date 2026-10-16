import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	appendCurrentScopeQuery,
	getCurrentScopeBearerToken,
	getCurrentScopeCredentialRevision,
	loginScopeSession,
	refreshScopeSession,
	isAuthVerdictPath,
	isSameOriginMagicianApiUrl,
	scopeCredentialOwnsRequestVerdict,
	scopeCredentialIdentityIsCurrent,
	scopeIdentityStore,
	scopedRequestHeaders,
	scopedWebSocketProtocols,
	setCurrentScopeBearerToken
} from './scopeIdentityStore';

afterEach(() => {
	vi.unstubAllGlobals();
	scopeIdentityStore.reset();
});

describe('bearer-bound scope transport', () => {
	it('carries a freshly minted login bearer into session verification', async () => {
		const session = {
			identity: { name: 'owner', display_name: 'Owner' },
			method: 'password',
			principal: 'anonymous',
			workspace: 'default',
			workspaces: [{
				id: 'default',
				display_name: 'Personal',
				is_default: true
			}]
		};
		const fetchMock = vi.fn()
			.mockResolvedValueOnce(new Response(JSON.stringify({
				token: 'mag_fresh_session',
				principal: 'anonymous',
				workspace: 'default',
				identity: session.identity
			}), { status: 201, headers: { 'content-type': 'application/json' } }))
			.mockImplementationOnce((_input: RequestInfo | URL, init?: RequestInit) => {
				expect(new Headers(init?.headers).get('Authorization'))
					.toBe('Bearer mag_fresh_session');
				return Promise.resolve(new Response(JSON.stringify(session), {
					status: 200,
					headers: { 'content-type': 'application/json' }
				}));
			});
		vi.stubGlobal('fetch', fetchMock);

		await expect(loginScopeSession({
			username: 'owner',
			password: 'correct horse battery staple'
		})).resolves.toEqual(session);
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(getCurrentScopeBearerToken()).toBe('mag_fresh_session');
	});

	// A phone keyboard and an autofill entry both like to leave a trailing
	// space on the username, and the server matches identity names exactly.
	// The password must survive untouched — a space is a legitimate character.
	it('trims the username it sends and leaves the password exactly as typed', async () => {
		const session = {
			identity: { name: 'owner', display_name: 'Owner' },
			method: 'password',
			principal: 'anonymous',
			workspace: 'default',
			workspaces: [{ id: 'default', display_name: 'Personal', is_default: true }]
		};
		let sentBody: { username?: string; password?: string } = {};
		const fetchMock = vi.fn()
			.mockImplementationOnce((_input: RequestInfo | URL, init?: RequestInit) => {
				sentBody = JSON.parse(String(init?.body));
				return Promise.resolve(new Response(JSON.stringify({
					token: 'mag_trimmed',
					principal: 'anonymous',
					workspace: 'default',
					identity: session.identity
				}), { status: 201, headers: { 'content-type': 'application/json' } }));
			})
			.mockResolvedValueOnce(new Response(JSON.stringify(session), {
				status: 200,
				headers: { 'content-type': 'application/json' }
			}));
		vi.stubGlobal('fetch', fetchMock);

		await loginScopeSession({ username: '  owner  ', password: '  spaced pass  ' });

		expect(sentBody.username).toBe('owner');
		expect(sentBody.password).toBe('  spaced pass  ');
	});

	it('does not let an older session 401 erase a newly minted bearer', async () => {
		let answerProbe!: (response: Response) => void;
		vi.stubGlobal('fetch', vi.fn(() => new Promise<Response>((resolve) => {
			answerProbe = resolve;
		})));
		setCurrentScopeBearerToken('mag_stale_session');
		const staleProbe = refreshScopeSession();
		await vi.waitFor(() => expect(answerProbe).toBeTypeOf('function'));

		setCurrentScopeBearerToken('mag_new_session');
		answerProbe(new Response(JSON.stringify({ error: 'authentication_required' }), {
			status: 401,
			headers: { 'content-type': 'application/json' }
		}));

		await expect(staleProbe).resolves.toBeNull();
		expect(getCurrentScopeBearerToken()).toBe('mag_new_session');
	});

	it('does not assign a delayed API 401 to a replacement bearer', () => {
		setCurrentScopeBearerToken('mag_old_session');
		const oldRevision = getCurrentScopeCredentialRevision();
		const oldAuthorization = 'Bearer mag_old_session';
		expect(scopeCredentialOwnsRequestVerdict(
			'mag_old_session',
			oldRevision,
			oldAuthorization
		)).toBe(true);

		setCurrentScopeBearerToken('mag_new_session');
		expect(scopeCredentialOwnsRequestVerdict(
			'mag_old_session',
			oldRevision,
			oldAuthorization
		)).toBe(false);
		expect(scopeCredentialOwnsRequestVerdict(
			'mag_new_session',
			getCurrentScopeCredentialRevision(),
			'Bearer caller_supplied_token'
		)).toBe(false);
	});

	it('advances the in-process authority fence only when bearer material rotates', () => {
		scopeIdentityStore.reset();
		const initial = getCurrentScopeCredentialRevision();
		expect(scopeCredentialIdentityIsCurrent(initial)).toBe(true);
		setCurrentScopeBearerToken('mag_pat_one');
		expect(getCurrentScopeCredentialRevision()).toBe(initial + 1);
		expect(scopeCredentialIdentityIsCurrent(initial + 1)).toBe(false);
		setCurrentScopeBearerToken('mag_pat_one');
		expect(getCurrentScopeCredentialRevision()).toBe(initial + 1);
		setCurrentScopeBearerToken('mag_pat_two');
		expect(getCurrentScopeCredentialRevision()).toBe(initial + 2);
	});

	it('strips caller scope assertions and attaches only the installed bearer', () => {
		setCurrentScopeBearerToken('mag_pat_test');
		const headers = scopedRequestHeaders({
			'X-Principal': 'forged-principal',
			'X-Workspace': 'forged-workspace',
			Accept: 'application/json'
		});

		expect(headers.get('X-Principal')).toBeNull();
		expect(headers.get('X-Workspace')).toBeNull();
		expect(headers.get('Authorization')).toBe('Bearer mag_pat_test');
		expect(headers.get('Accept')).toBe('application/json');
	});

	it('removes legacy query selectors while preserving domain filters', () => {
		const params = appendCurrentScopeQuery(new URLSearchParams({
			principal: 'forged-principal',
			workspace: 'forged-workspace',
			limit: '25'
		}));

		expect(params.toString()).toBe('limit=25');
	});

	it('does not read a resource-authority admin-key 401 as a session verdict', () => {
		// RA is gated by RESOURCE_AUTHORITY_API_KEY, compared byte-for-byte
		// against whatever Authorization carries; with that key set the
		// session bearer the scoped wrapper attaches is always "wrong" there.
		// Its 401 must not wipe a valid session (the app shell probes
		// /resource-authority/freeze on every mount).
		expect(isAuthVerdictPath('/api/magician/v2/resource-authority/freeze')).toBe(true);
		expect(isAuthVerdictPath('/api/magician/v2/resource-authority/budgets/x')).toBe(true);
		expect(isAuthVerdictPath('/api/magician/v2/auth/session')).toBe(true);
		expect(isAuthVerdictPath('/api/magician/v2/tasks')).toBe(false);
		expect(isAuthVerdictPath('/api/magician/v2/ui/preferences')).toBe(false);
	});

	it('never classifies a cross-origin lookalike path as a Magician API request', () => {
		expect(isSameOriginMagicianApiUrl(
			'/api/magician/v2/tasks',
			'https://ui.example.test'
		)).toBe(true);
		expect(isSameOriginMagicianApiUrl(
			'https://attacker.example/api/magician/v2/tasks',
			'https://ui.example.test'
		)).toBe(false);
		expect(isSameOriginMagicianApiUrl(
			'https://user:password@ui.example.test/api/magician/v2/tasks',
			'https://ui.example.test'
		)).toBe(false);
	});

	it('replaces stale websocket auth protocols instead of sending two bearers', () => {
		setCurrentScopeBearerToken('mag_pat_current');
		expect(scopedWebSocketProtocols([
			'magician-events-v2',
			'magician-bearer.mag_pat_stale'
		])).toEqual([
			'magician-events-v2',
			'magician-bearer.mag_pat_current'
		]);
	});
});
