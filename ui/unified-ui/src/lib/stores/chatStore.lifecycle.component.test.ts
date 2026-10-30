import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createChatStore } from './chatStore';
import { scopeIdentityStore } from './scopeIdentityStore';

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => { resolve = done; });
	return { promise, resolve };
}

function message(sessionId: string, index: number) {
	return { id: `${sessionId}-${index}`, session_id: sessionId, direction: 'assistant',
		content: { type: 'text', text: `Reply ${index}` }, created_at: index };
}

function detail(id: string, status = 'active', count = 1) {
	return Response.json({ session: { id, status, principal: 'anonymous', workspace: 'default',
		agent_id: 'personal-assistant', ui_thread_id: 'general', title: id,
		origin_channel: { channel_type: 'web' }, created_at: 1, updated_at: 2 },
		messages: Array.from({ length: count }, (_, index) => message(id, index + 100)) });
}

beforeEach(() => {
	scopeIdentityStore.reset();
	localStorage.setItem('chat_principal', 'anonymous');
	localStorage.setItem('chat_channel_address', 'browser-test');
});
afterEach(() => { scopeIdentityStore.reset(); localStorage.clear(); vi.unstubAllGlobals(); });

function mockApi(handle: (url: string, method?: string) => Response | Promise<Response> | undefined) {
	const fetchMock = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
		const url = String(input);
		if (url.includes('/chat/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
		return await handle(url, init?.method) ?? new Response('not found', { status: 404 });
	});
	vi.stubGlobal('fetch', fetchMock);
	return fetchMock;
}

describe('chat deletion lifecycle', () => {
	it('shows pending immediately, shares repeat requests, and removes only after acknowledgement', async () => {
		const deletion = deferred<Response>();
		const fetchMock = mockApi((url, method) => method === 'DELETE' ? deletion.promise : detail('one'));
		const store = createChatStore();
		await store.openSession('one');
		const first = store.deleteSession('one');
		const repeated = store.deleteSession('one');
		expect(first).toBe(repeated);
		expect(get(store).deletingSessionIds).toEqual(['one']);
		expect(get(store).sessions.map(s => s.id)).toEqual(['one']);
		await vi.waitFor(() => expect(fetchMock.mock.calls.filter(([, init]) => init?.method === 'DELETE')).toHaveLength(1));
		deletion.resolve(Response.json({ deleted: true }));
		await first;
		expect(get(store)).toMatchObject({ deletingSessionIds: [], sessions: [], messages: [], activeSessionId: null });
	});

	it('propagates the server error and leaves the session available to retry', async () => {
		mockApi((_url, method) => method === 'DELETE'
			? Response.json({ error: 'Session cleanup failed' }, { status: 500 }) : detail('one'));
		const store = createChatStore();
		await store.openSession('one');
		await expect(store.deleteSession('one')).rejects.toThrow('Session cleanup failed');
		expect(get(store)).toMatchObject({ deletingSessionIds: [], activeSessionId: 'one' });
		expect(get(store).messages).toHaveLength(1);
	});

	it('keeps another conversation intact when deletion finishes after switching', async () => {
		const deletion = deferred<Response>();
		mockApi((url, method) => method === 'DELETE' ? deletion.promise : detail(url.includes('/two?') ? 'two' : 'one'));
		const store = createChatStore();
		await store.openSession('one');
		const pending = store.deleteSession('one');
		await store.openSession('two');
		deletion.resolve(Response.json({ deleted: true }));
		await pending;
		expect(get(store)).toMatchObject({ activeSessionId: 'two', messages: [expect.objectContaining({ session_id: 'two' })] });
		expect(get(store).sessions.map(s => s.id)).toEqual(['two']);
	});

	it('clears a deleted archived overlay without deleting its selected active conversation', async () => {
		mockApi((url, method) => method === 'DELETE' ? Response.json({ deleted: true })
			: detail(url.includes('/old?') ? 'old' : 'one', url.includes('/old?') ? 'archived' : 'active'));
		const store = createChatStore();
		await store.openSession('one');
		await store.openSession('old');
		await store.deleteSession('old');
		expect(get(store)).toMatchObject({ activeSessionId: 'one', viewingArchivedId: null, messages: [], hasMoreMessages: false });
	});

	it('does not apply an old deletion to a different workspace', async () => {
		const deletion = deferred<Response>();
		const fetchMock = mockApi((_url, method) => method === 'DELETE' ? deletion.promise : detail('one'));
		const store = createChatStore();
		await store.openSession('one');
		const pending = store.deleteSession('one');
		await vi.waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === 'DELETE')).toBe(true));
		scopeIdentityStore.observe('anonymous', 'other-workspace');
		deletion.resolve(Response.json({ deleted: true }));
		await pending;
		expect(get(store)).toMatchObject({ activeSessionId: null, sessions: [], deletingSessionIds: [] });
	});
});

describe('chat history paging', () => {
	it('opens just the newest page and only fetches one older page when asked', async () => {
		const fetchMock = mockApi(url => url.includes('/messages?')
			? Response.json({ messages: [message('one', 50)], has_more: true }) : detail('one', 'active', 200));
		const store = createChatStore();
		await store.openSession('one');
		expect(get(store).hasMoreMessages).toBe(true);
		expect(fetchMock.mock.calls.filter(([url]) => String(url).includes('/messages?'))).toHaveLength(0);
		await store.loadOlderMessages('one');
		expect(get(store).messages).toHaveLength(201);
		expect(get(store).hasMoreMessages).toBe(true);
		expect(fetchMock.mock.calls.filter(([url]) => String(url).includes('/messages?'))).toHaveLength(1);
	});

	it('discards a slow older page after switching and releases the loading state', async () => {
		const older = deferred<Response>();
		const fetchMock = mockApi(url => url.includes('/messages?') ? older.promise
			: detail(url.includes('/two?') ? 'two' : 'one', 'active', 200));
		const store = createChatStore();
		await store.openSession('one');
		const pending = store.loadOlderMessages('one');
		await vi.waitFor(() => expect(fetchMock.mock.calls.some(([url]) => String(url).includes('/messages?'))).toBe(true));
		await store.openSession('two');
		expect(get(store).isLoadingOlder).toBe(false);
		older.resolve(Response.json({ messages: [message('one', 50)], has_more: false }));
		await pending;
		expect(get(store).messages.every(m => m.session_id === 'two')).toBe(true);
		expect(get(store).hasMoreMessages).toBe(true);
	});

	it('never uses the current conversation cursor to load a different session', async () => {
		const fetchMock = mockApi(() => detail('one', 'active', 200));
		const store = createChatStore();
		await store.openSession('one');
		await store.loadOlderMessages('two');
		expect(fetchMock.mock.calls.some(([url]) => String(url).includes('/messages?'))).toBe(false);
	});
});
