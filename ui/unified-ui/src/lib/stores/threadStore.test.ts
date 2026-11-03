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

describe('thread archive and delete timeouts', () => {
	it('keeps an archive or delete open long enough to finish every session', async () => {
		const timeouts: number[] = [];
		const timeout = vi.spyOn(AbortSignal, 'timeout').mockImplementation((ms: number) => {
			timeouts.push(ms);
			return new AbortController().signal;
		});
		try {
			installFetchMock([
				{ method: 'PATCH', match: `${path}/research`, handle: () => jsonResponse({ thread: { ...thread, archived: true } }) },
				{ method: 'DELETE', match: `${path}/research`, handle: () => new Response(null, { status: 204 }) },
				{ method: 'GET', match: path, handle: () => jsonResponse({ threads: [thread] }) }
			]);

			expect(await threadStore.updateThread('research', { archived: true })).toMatchObject({ id: 'research', archived: true });
			expect(await threadStore.updateThread('research', { name: 'Research notes' })).toMatchObject({ id: 'research' });
			expect(await threadStore.deleteThread('research')).toBe(true);
			expect(timeouts).toEqual([600_000, 30_000, 30_000, 30_000, 600_000, 30_000]);
		} finally {
			timeout.mockRestore();
		}
	});
});

describe('thread keys: slugs, aliases and the reserved `new` route', () => {
	it('never creates a thread for the reserved `new` key', async () => {
		const { calls } = installFetchMock([
			{ method: 'GET', match: `${path}/new`, handle: () => jsonResponse({ error: 'missing' }, { status: 404 }) },
			{ method: 'POST', match: path, handle: () => jsonResponse(thread) }
		]);
		expect(await threadStore.ensureThread('new')).toBeNull();
		expect(calls.filter((call) => call.method === 'POST')).toHaveLength(0);
	});

	it('resolves a slug or alias key to the canonical thread id', async () => {
		const renamed = { ...thread, slug: 'trip-planning', name: 'Trip planning' };
		const { calls } = installFetchMock([
			{ method: 'GET', match: `${path}/trip-planning`, handle: () => jsonResponse({ thread: renamed }) }
		]);
		const resolved = await threadStore.ensureThread('trip-planning');
		expect(resolved).toMatchObject({ id: 'research', slug: 'trip-planning' });
		expect(get(threadStore).activeThread?.id).toBe('research');
		expect(get(threadStore).threads.find((candidate) => candidate.id === 'research')?.slug).toBe('trip-planning');
		expect(calls.filter((call) => call.method === 'POST')).toHaveLength(0);
	});

	it('creates a named thread without an id so the server derives one', async () => {
		const created = { ...thread, id: 'research-2', slug: 'research-2' };
		const { calls } = installFetchMock([
			{ method: 'POST', match: path, handle: () => jsonResponse(created) },
			{ method: 'GET', match: path, handle: () => jsonResponse({ threads: [thread, created] }) }
		]);
		expect(await threadStore.createThread('Research')).toMatchObject({ id: 'research-2', slug: 'research-2' });
		const post = calls.find((call) => call.method === 'POST');
		expect(JSON.parse(String(post?.init?.body))).toEqual({ id: null, name: 'Research' });
	});
});
