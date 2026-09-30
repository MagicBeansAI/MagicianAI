import { get } from 'svelte/store';
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import { createChatStore } from './chatStore';

function session(id: string, status: 'active' | 'archived') {
	return {
		id,
		principal: 'anonymous',
		workspace: 'default',
		agent_id: 'personal-assistant',
		ui_thread_id: 'general',
		title: id,
		origin_channel: { channel_type: 'web' },
		status,
		created_at: 1,
		updated_at: 2
	};
}

function detailResponse(id: string, status: 'active' | 'archived'): Response {
	return new Response(JSON.stringify({
		session: session(id, status),
		messages: [{
			id: `message-${id}`,
			session_id: id,
			direction: 'assistant',
			content: { type: 'text', text: `Transcript for ${id}` },
			created_at: 3
		}]
	}), {
		status: 200,
		headers: { 'Content-Type': 'application/json' }
	});
}

afterEach(() => {
	localStorage.clear();
	vi.unstubAllGlobals();
});

describe('chatStore exact session selection', () => {
	it('switches between multiple active sessions by exact ID and keeps archived sessions read-only', async () => {
		localStorage.setItem('chat_principal', 'anonymous');
		localStorage.setItem('chat_channel_address', 'browser-test');

		const fetchMock = vi.fn(async (request: string | URL | Request) => {
			const url = String(request);
			if (url.includes('/chat/enroll/status?')) {
				return new Response(JSON.stringify({ enrolled: true, principal: 'anonymous' }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			if (url.includes('/chat/sessions/active-one?')) return detailResponse('active-one', 'active');
			if (url.includes('/chat/sessions/active-two?')) return detailResponse('active-two', 'active');
			if (url.includes('/chat/sessions/archived-one?')) return detailResponse('archived-one', 'archived');
			return new Response('not found', { status: 404 });
		});
		vi.stubGlobal('fetch', fetchMock);

		const store = createChatStore();
		await store.openSession('active-one');
		expect(get(store)).toMatchObject({
			activeSessionId: 'active-one',
			viewingArchivedId: null
		});

		await store.openSession('active-two');
		expect(get(store)).toMatchObject({
			activeSessionId: 'active-two',
			viewingArchivedId: null,
			messages: [expect.objectContaining({ session_id: 'active-two' })]
		});

		await store.openSession('archived-one');
		expect(get(store)).toMatchObject({
			activeSessionId: 'active-two',
			viewingArchivedId: 'archived-one',
			messages: [expect.objectContaining({ session_id: 'archived-one' })]
		});

		const detailUrls = fetchMock.mock.calls
			.map(([request]) => String(request))
			.filter((url) => url.includes('/chat/sessions/'));
		expect(detailUrls).toHaveLength(3);
		expect(detailUrls[1]).toContain('/chat/sessions/active-two?');
	});
});

describe('original answer navigation', () => {
    beforeEach(() => { localStorage.setItem('chat_principal', 'anonymous'); localStorage.setItem('chat_channel_address', 'browser-test'); });
    it('keeps a linked execution selected when ordinary history intentionally omits it', async () => {
        vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
            const url = String(request);
            if (url.includes('/chat/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
            if (url.includes('/chat/sessions?')) return Response.json({ sessions: [session('parent', 'active')] });
            if (url.includes('/chat/sessions/branch?')) return detailResponse('branch', 'active');
            return detailResponse('parent', 'active');
        }));
        const store = createChatStore();
        await store.openSession('branch', { messageId: 'message-branch' });
        await store.loadSessions('general');
        expect(get(store).activeSessionId).toBe('branch');
        expect(get(store).messages[0].id).toBe('message-branch');
        expect(get(store).sessions.find(row => row.id === 'branch')?.title).toBe('branch');
    });
    it('loads beyond the newest page and selects an exact answer without the voice ledger', async () => {
        const rows = Array.from({ length: 200 }, (_, index) => ({
            id: `later-${index}`, session_id: 'branch', direction: 'assistant',
            content: { type: 'text', text: 'Later answer' }, created_at: 100 + index
        }));
        const original = { id: 'answer', session_id: 'branch', direction: 'assistant',
            content: { type: 'text', text: 'Original answer' }, created_at: 42 };
        const calls: string[] = [];
        vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
            const url = String(request); calls.push(url);
            if (url.includes('/chat/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
            if (url.includes('/branch/messages?')) {
                expect(url).toContain('before=later-0');
                return Response.json({ messages: [original], has_more: false });
            }
            return Response.json({ session: session('branch', 'active'), messages: rows });
        }));
        const store = createChatStore();
        await store.openSession('branch', { messageId: 'answer' });
        expect(get(store).focusedMessageId).toBe('answer');
        expect(get(store).messages).toHaveLength(201);
        expect(get(store).hasMoreMessages).toBe(false);
        expect(calls.some(url => url.includes('/media/voice'))).toBe(false);
    });
    it('does not overwrite a newer conversation when an older source page arrives late', async () => {
        let releasePage!: (response: Response) => void;
        let pageRequested!: () => void;
        const paging = new Promise<void>(resolve => { pageRequested = resolve; });
        vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
            const url = String(request);
            if (url.includes('/chat/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
            if (url.includes('/branch/messages?')) {
                pageRequested();
                return new Promise<Response>(resolve => { releasePage = resolve; });
            }
            if (url.includes('/branch?')) return Response.json({ session: session('branch', 'active'),
                messages: Array.from({ length: 200 }, (_, index) => ({ id: `later-${index}`, session_id: 'branch',
                    direction: 'assistant', content: { type: 'text', text: 'Later' }, created_at: index })) });
            return detailResponse('newer', 'active');
        }));
        const store = createChatStore();
        const oldNavigation = store.openSession('branch', { messageId: 'answer' });
        await paging;
        await store.openSession('newer');
        releasePage(Response.json({ messages: [], has_more: false }));
        await oldNavigation;
        expect(get(store).activeSessionId).toBe('newer');
        expect(get(store).messages[0].id).toBe('message-newer');
        expect(get(store).error).toBeNull();
    });
    it('reports a removed answer without substituting another answer from the turn', async () => {
        vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) =>
            String(request).includes('/chat/enroll/status')
                ? Response.json({ enrolled: true, principal: 'anonymous' })
                : detailResponse('branch', 'active')));
        const store = createChatStore();
        await store.openSession('branch', { messageId: 'deleted' });
        expect(get(store).focusedMessageId).toBeNull();
        expect(get(store).error).toContain('no longer available');
    });
});


describe('composer queue admission', () => {
    it.each([false, true])('refreshes one concurrent result in place without replaying stale copies (streaming=%s)', async (streaming) => {
        localStorage.setItem('chat_principal', 'anonymous');
        localStorage.setItem('chat_channel_address', 'browser-test');
        let stream!: ReadableStreamDefaultController<Uint8Array>;
        vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
            const url = String(request);
            if (url.includes('/chat/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
            if (url.includes('/messages/stream?')) return new Response(new ReadableStream({ start(controller) { stream = controller; } }));
            return detailResponse('parent', 'active');
        }));
        const store = createChatStore();
        await store.openSession('parent');
        const result = (version: number, text: string) => ({
            id: 'voice-task-result-one-result', session_id: 'parent', direction: 'assistant',
            content: { type: 'text', text }, created_at: version,
            context_origin: { ui_thread_id: 'general', session_id: 'branch', request_id: 'voice-task-result-one',
                message_id: `answer-${version}`, result_created_at: version }
        });
        store.handleChatMessageReceived('parent', result(10, 'Execution completed.'));
        let sending: Promise<unknown> | undefined;
        if (streaming) {
            sending = store.sendMessageStreaming('parent', 'A different question');
            await vi.waitFor(() => expect(stream).toBeDefined());
        }
        store.handleChatMessageReceived('parent', result(30, 'The final article is ready.'));
        store.handleChatMessageReceived('parent', result(30, 'The final article is ready.'));
        store.handleChatMessageReceived('parent', result(20, 'An older summary'));
        if (streaming) {
            stream.enqueue(new TextEncoder().encode(`event: done\ndata: ${JSON.stringify({ assistant_message: {
                id: 'foreground-answer', session_id: 'parent', direction: 'assistant',
                content: { type: 'text', text: 'Separate answer' }, created_at: 40
            } })}\n\n`));
            stream.close();
            await sending;
        }
        const copies = get(store).messages.filter(message => message.id === 'voice-task-result-one-result');
        expect(copies).toHaveLength(1);
        expect(copies[0]).toMatchObject({ created_at: 10, content: { text: 'The final article is ready.' },
            context_origin: { message_id: 'answer-30', result_created_at: 30 } });
    });

    it('keeps a foreground reply streaming while background admission refreshes saved questions', async () => {
        localStorage.setItem('chat_principal', 'anonymous');
        localStorage.setItem('chat_channel_address', 'browser-test');
        let stream!: ReadableStreamDefaultController<Uint8Array>;
        const encoder = new TextEncoder();
        const emit = (event: string, data: unknown) => stream.enqueue(encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`));
        vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
            const url = String(request);
            if (url.includes('/chat/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
            if (url.includes('/messages/stream?')) return new Response(new ReadableStream({ start(controller) { stream = controller; } }));
            if (url.includes('/messages?')) return Response.json({ messages: [
                { id: 'saved-question', session_id: 'parent', direction: 'user', content: { type: 'text', text: 'Original question' }, created_at: 4 },
                { id: 'parallel-question', session_id: 'parent', direction: 'user', content: { type: 'text', text: 'Parallel question' }, created_at: 5 },
            ] });
            return detailResponse('parent', 'active');
        }));
        const store = createChatStore();
        await store.openSession('parent');
        const sending = store.sendMessageStreaming('parent', 'Original question');
        await vi.waitFor(() => expect(stream).toBeDefined());
        emit('token', { text: 'First part' });
        await vi.waitFor(() => expect(get(store).messages.some(message => message.content.text === 'First part')).toBe(true));
        await store.loadMessages('parent', { preserveLive: true });
        expect(get(store).isSendingMessage).toBe(true);
        expect(get(store).messages.filter(message => message.content.text === 'Original question')).toHaveLength(1);
        expect(get(store).messages.some(message => message.id === 'parallel-question')).toBe(true);
        emit('token', { text: ' still streaming' });
        await vi.waitFor(() => expect(get(store).messages.some(message => message.content.text === 'First part still streaming')).toBe(true));
        emit('done', { assistant_message: { id: 'answer', session_id: 'parent', direction: 'assistant',
            content: { type: 'text', text: 'First part still streaming' }, created_at: 6 } });
        stream.close();
        await sending;
        expect(get(store).isSendingMessage).toBe(false);
        expect(get(store).messages.some(message => message.id === 'parallel-question')).toBe(true);
    });

    it('preserves transcript and selection while separately admitting queued text', async () => {
        localStorage.setItem('chat_principal', 'anonymous');
        localStorage.setItem('chat_channel_address', 'browser-test');
        const fetchMock = vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
            const url = String(request);
            if (url.includes('/chat/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
            if (url.includes('/queue?') && init?.method === 'POST') {
                expect(JSON.parse(String(init.body))).toMatchObject({ text: 'Follow-up', mode: 'accept_in_scope', harness_engine: 'pi' });
                return Response.json({ queued: { id: 'queued-1' } }, { status: 202 });
            }
            if (url.includes('/queue/queued-1/action')) return Response.json({ ok: true });
            return detailResponse('parent', 'active');
        });
        vi.stubGlobal('fetch', fetchMock);
        const store = createChatStore();
        await store.openSession('parent');
        const before = get(store);
        const id = await store.queueMessage('parent', 'Follow-up', null, [], { mode: 'accept_in_scope', harnessEngine: 'pi' });
        expect(id).toBe('queued-1');
        expect(get(store).messages).toEqual(before.messages);
        expect(get(store).activeSessionId).toBe('parent');
        expect(get(store).isSendingMessage).toBe(before.isSendingMessage);
        await store.actOnQueuedMessage('parent', id, 'parallel');
        expect(fetchMock.mock.calls.some(([url, init]) => String(url).includes('/run') && init?.method === 'DELETE')).toBe(false);
    });
});
