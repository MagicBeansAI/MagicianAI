import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/environment', () => ({ browser: true }));

function storageMock() {
	const values = new Map<string, string>();
	return {
		getItem: (key: string) => values.get(key) ?? null,
		setItem: (key: string, value: string) => values.set(key, value),
		removeItem: (key: string) => values.delete(key),
		clear: () => values.clear()
	};
}

beforeEach(() => {
	vi.resetModules();
	vi.stubGlobal('localStorage', storageMock());
	vi.stubGlobal('sessionStorage', storageMock());
	vi.stubGlobal('window', {
		location: {
			origin: 'http://localhost:5173',
			href: 'http://localhost:5173/',
			pathname: '/',
			search: ''
		}
	});
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('web login shared across tabs', () => {
	it('keeps a login bearer for a later tab in the same browser profile', async () => {
		const first = await import('./scopeIdentityStore');
		first.setCurrentScopeBearerToken('mag_shared_session');
		expect(localStorage.getItem('magician:auth:scoped-bearer')).toBe('mag_shared_session');
		expect(sessionStorage.getItem('magician:auth:scoped-bearer')).toBeNull();

		vi.resetModules();
		const second = await import('./scopeIdentityStore');
		expect(second.getCurrentScopeBearerToken()).toBe('mag_shared_session');
	});

	it('adopts a tab-local bearer into the shared store', async () => {
		sessionStorage.setItem('magician:auth:scoped-bearer', 'mag_legacy_tab');
		const store = await import('./scopeIdentityStore');
		expect(store.getCurrentScopeBearerToken()).toBe('mag_legacy_tab');
		expect(localStorage.getItem('magician:auth:scoped-bearer')).toBe('mag_legacy_tab');
		expect(sessionStorage.getItem('magician:auth:scoped-bearer')).toBeNull();
	});

	it('sign-out clears the shared bearer', async () => {
		const store = await import('./scopeIdentityStore');
		store.setCurrentScopeBearerToken('mag_shared_session');
		store.scopeIdentityStore.reset();
		expect(store.getCurrentScopeBearerToken()).toBe('');
		expect(localStorage.getItem('magician:auth:scoped-bearer')).toBeNull();
		expect(sessionStorage.getItem('magician:auth:scoped-bearer')).toBeNull();
	});
});

it('notifies scoped resources of credential replacement without exposing the bearer', async () => {
 const store = await import('./scopeIdentityStore');
 store.scopeIdentityStore.observe('owner','work');
 const revisions: number[] = [], snapshots: unknown[] = [];
 const unsubscribe = store.scopeIdentityStore.subscribe(value => { snapshots.push(value); revisions.push(store.getCurrentScopeCredentialRevision()); });
 store.setCurrentScopeBearerToken('first'); store.setCurrentScopeBearerToken('replacement'); store.setCurrentScopeBearerToken('replacement');
 unsubscribe();
 expect(revisions).toHaveLength(3); expect(revisions[2]).toBe(revisions[1]+1);
 expect(snapshots[2]).toEqual({principal:'owner',workspace:'work',isResolved:true});
 expect(JSON.stringify(snapshots)).not.toContain('replacement');
});
