import { afterEach, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { chatProfileStore } from './chatProfileStore';
import { createChatStore } from './chatStore';
import type { ModelChoice } from '$lib/llm/modelCatalog';

const model: ModelChoice = {
    id: 'custom/model', label: 'Custom model', model: 'physical-model', provider: 'custom', connection: 'custom',
    limits: null, capabilities: { vision: true, tools: true, streaming: true, computer_use: false, structured_output: false, web_search: false },
    thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
    choices: { auto: 'opaque-auto', high: 'opaque-high' }
};

afterEach(() => { localStorage.clear(); vi.unstubAllGlobals(); });

it('restores typed preferences and sends identical model/thinking on ordinary, stream and queue admissions', async () => {
    localStorage.setItem('chat_principal', 'anonymous');
    localStorage.setItem('chat_channel_address', 'browser-test');
    localStorage.setItem('magician.chat.selectedProfile', 'retired-alias');
    localStorage.setItem('magician.chat.modelSelection', JSON.stringify({ model_id: model.id, thinking: 'high' }));
    const posted: { url: string; body: Record<string, unknown> }[] = [];
    const fetch = vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
        const url = String(request);
        if (url.includes('/chat/models?engine=magician')) return Response.json({ models: [model], default_selection: { model_id: model.id, thinking: 'auto' } });
        if (url.includes('/chat/enroll/status')) return Response.json({ enrolled: true, principal: 'anonymous' });
        if (init?.method === 'POST') {
            const body = JSON.parse(String(init.body));
            posted.push({ url, body });
            if (url.includes('/queue?')) return Response.json({ queued: { id: 'queued-typed' } }, { status: 202 });
            const answer = { id: 'answer', session_id: 'parent', direction: 'assistant', content: { type: 'text', text: 'Answer' }, created_at: 1 };
            if (url.includes('/messages/stream?')) return new Response(`event: done\ndata: ${JSON.stringify({ assistant_message: answer })}\n\n`, { headers: { 'Content-Type': 'text/event-stream' } });
            return Response.json({ assistant_message: answer });
        }
        return Response.json({ messages: [] });
    });
    vi.stubGlobal('fetch', fetch);
    await chatProfileStore.load('magician');
    expect(get(chatProfileStore)).toMatchObject({ selected: 'opaque-high', selectionError: null });
    expect(localStorage.getItem('magician.chat.selectedProfile')).toBeNull();
    const store = createChatStore();
    await store.sendMessage('parent', 'Normal', null, [], { harnessEngine: 'magician' });
    await store.sendMessageStreaming('parent', 'Stream', null, [], { harnessEngine: 'magician' });
    await store.queueMessage('parent', 'Queued', null, [], { harnessEngine: 'magician' });
    expect(posted).toHaveLength(3);
    for (const { body } of posted) {
        expect(body).toMatchObject({ harness_engine: 'magician', model_selection: { model_id: model.id, thinking: 'high' } });
        expect(body).not.toHaveProperty('profile');
        expect(body).not.toHaveProperty('harness_model');
    }
    expect(fetch.mock.calls.some(([url]) => String(url).includes('/chat/profiles'))).toBe(false);

    // Neither an absent endpoint nor a server failure can restore retired transport.
    for (const status of [404, 503]) {
        fetch.mockImplementation(async () => new Response('unavailable', { status }));
        fetch.mockClear();
        await chatProfileStore.load('magician');
        expect(fetch).toHaveBeenCalledTimes(1);
        expect(get(chatProfileStore).selectionError).toContain('could not be loaded');
        expect(() => chatProfileStore.requestSelection(null, 'magician')).toThrow('could not be loaded');
    }
});
