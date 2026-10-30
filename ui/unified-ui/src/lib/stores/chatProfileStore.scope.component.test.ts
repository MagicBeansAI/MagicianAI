import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createChatProfileStore } from './chatProfileStore';
import { scopeIdentityStore } from './scopeIdentityStore';

const stores: ReturnType<typeof createChatProfileStore>[] = [];
function store() { const value = createChatProfileStore(); stores.push(value); return value; }
function catalog(scope: string) {
    return { models: [{ id: `test/${scope}`, label: scope, provider: 'openai', connection: 'openai', model: scope,
        limits: null, capabilities: { tools: true, vision: true },
        thinking: { mode: 'effort', levels: ['none'], default: 'none' }, choices: { auto: `${scope}/auto` } }],
        default_selection: { model_id: `test/${scope}`, thinking: 'auto' } };
}
beforeEach(() => { localStorage.clear(); scopeIdentityStore.observe('owner', 'A'); });
afterEach(() => { stores.splice(0).forEach(value => value.destroy()); scopeIdentityStore.reset(); vi.unstubAllGlobals(); });

it('refreshes a cached catalog after the workspace changes and refuses stale sends', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(Response.json(catalog('A'))).mockResolvedValue(Response.json(catalog('B')));
    vi.stubGlobal('fetch', fetch);
    const value = store();
    await value.ensureLoaded('magician');
    expect(get(value).models[0].id).toBe('test/A');
    scopeIdentityStore.observe('owner', 'B');
    expect(() => value.requestSelection(null, 'magician')).toThrow();
    await value.ensureLoaded('magician');
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(get(value).models[0].id).toBe('test/B');
});

it('does not join an old workspace request or accept its late response', async () => {
    let finish!: (response: Response) => void;
    const fetch = vi.fn().mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve; }))
        .mockResolvedValue(Response.json(catalog('B')));
    vi.stubGlobal('fetch', fetch);
    const value = store();
    const old = value.ensureLoaded('magician');
    scopeIdentityStore.observe('owner', 'B');
    const current = value.ensureLoaded('magician');
    try {
        expect(fetch).toHaveBeenCalledTimes(2);
        await current;
        expect(get(value).models[0].id).toBe('test/B');
    } finally {
        finish(Response.json(catalog('A')));
        await old;
        await current;
    }
    expect(get(value).models[0].id).toBe('test/B');
});
