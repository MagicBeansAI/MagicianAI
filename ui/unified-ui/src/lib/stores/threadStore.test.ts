import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { scopeIdentityStore, setCurrentScopeBearerToken } from './scopeIdentityStore';
import { threadStore } from './threadStore';

vi.mock('$app/environment', () => ({ browser: true }));

const path = '/api/magician/v2/ui-threads';
const thread = {
	principal: 'reviewer', workspace: 'default', id: 'research', name: 'Research',
	archived: false, sort_order: 1, created_at: 1, updated_at: 1
};

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => { resolve = done; });
	return { promise, resolve };
}

beforeEach(() => {
	scopeIdentityStore.reset();
	setCurrentScopeBearerToken('initial-credential');
	scopeIdentityStore.observe('reviewer', 'default');
	threadStore.clearActiveThread();
});

afterEach(() => {
	scopeIdentityStore.reset();
	vi.unstubAllGlobals();
});

describe('thread creation authority across staged requests', () => {
	it('does not create a missing destination after a same-scope credential change', async () => {
		const response = deferred<Response>();
		const { calls } = installFetchMock([
			{ method: 'GET', match: `${path}/research`, handle: () => response.promise },
			{ method: 'POST', match: path, handle: () => jsonResponse(thread) },
			{ method: 'GET', match: path, handle: () => jsonResponse({ threads: [thread] }) }
		]);
		const pending = threadStore.ensureThread('research');
		setCurrentScopeBearerToken('replacement-credential');
		response.resolve(jsonResponse({ error: 'missing' }, { status: 404 }));
		expect(await pending).toBeNull();
		expect(calls.filter((call) => call.method === 'POST')).toHaveLength(0);
	});

	it('does not publish a thread loaded under replaced credentials', async () => {
		const response = deferred<Response>();
		installFetchMock([
			{ method: 'GET', match: `${path}/research`, handle: () => response.promise }
		]);
		const pending = threadStore.loadThread('research');
		setCurrentScopeBearerToken('replacement-credential');
		response.resolve(jsonResponse({ thread }));
		expect(await pending).toBeNull();
		expect(get(threadStore).activeThread).toBeNull();
	});

	it.each(['workspace', 'credentials'])('does not follow creation with another detail request after a %s change during catalog refresh', async (change) => {
		const catalog = deferred<Response>();
		const enteredCatalog = deferred<void>();
		const { calls } = installFetchMock([
			{ method: 'GET', match: `${path}/research`, handle: () => jsonResponse({ error: 'missing' }, { status: 404 }) },
			{ method: 'POST', match: path, handle: () => jsonResponse(thread) },
			{ method: 'GET', match: path, handle: () => { enteredCatalog.resolve(); return catalog.promise; } }
		]);
		const pending = threadStore.ensureThread('research');
		await enteredCatalog.promise;
		if (change === 'workspace') scopeIdentityStore.observe('reviewer', 'other');
		else setCurrentScopeBearerToken('replacement-credential');
		catalog.resolve(jsonResponse({ threads: [thread] }));
		expect(await pending).toBeNull();
		expect(calls.filter((call) => call.method === 'GET' && call.url === `${path}/research`)).toHaveLength(1);
	});

	it('still creates and loads a missing destination within the original authority', async () => {
		let detailReads = 0;
		const { calls } = installFetchMock([
			{ method: 'GET', match: `${path}/research`, handle: () => ++detailReads === 1
				? jsonResponse({ error: 'missing' }, { status: 404 }) : jsonResponse({ thread }) },
			{ method: 'POST', match: path, handle: () => jsonResponse(thread) },
			{ method: 'GET', match: path, handle: () => jsonResponse({ threads: [thread] }) }
		]);
		expect(await threadStore.ensureThread('research')).toEqual(thread);
		expect(calls.map((call) => call.method)).toEqual(['GET', 'POST', 'GET', 'GET']);
		expect(get(threadStore).activeThread?.id).toBe('research');
	});
});
