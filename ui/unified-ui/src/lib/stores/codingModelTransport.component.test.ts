import { afterEach, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { codingChoiceFromSelection, codingProfileStore } from './codingProfileStore';
import type { CodingModelCatalog } from '$lib/llm/codingModelSelection';

const engines = ['pi', 'claude_code', 'codex_app_server', 'grok_acp', 'agy_cli'];
const catalog: CodingModelCatalog = {
    models: engines.map(engine => ({
        id: engine === 'pi' ? 'openai/gpt-6.1-sol' : `native/${engine}/provider/model`, engine,
        provider: 'provider', connection: 'provider', model: 'physical-model', label: 'Model', limits: null,
        capabilities: { vision: true, tools: true, streaming: true, computer_use: false, web_search: false, structured_output: false },
        thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
        choices: { auto: `${engine}/auto`, high: `${engine}/high` }
    })),
    engines: engines.map(id => ({ id, selectable: true, readiness: 'ready' })),
    default_selection: { kind: 'model', engine: 'pi', model_id: 'openai/gpt-6.1-sol', thinking: 'auto' }
};

afterEach(() => { localStorage.clear(); vi.unstubAllGlobals(); });

it('migrates an offline alias then sends engine/model/thinking for every engine and distinct Auto', async () => {
    localStorage.setItem('magician.coding.selectedProfile', 'coding-premium');
    const fetch = vi.fn(async () => Response.json(catalog));
    vi.stubGlobal('fetch', fetch);
    await codingProfileStore.load();
    expect(fetch.mock.calls[0]).toEqual(['/api/magician/v2/coding/models', expect.anything()]);
    expect(get(codingProfileStore)).toMatchObject({ selected: 'pi/high', error: null });
    expect(localStorage.getItem('magician.coding.selectedProfile')).toBeNull();
    expect(JSON.parse(localStorage.getItem('magician.coding.modelSelection')!)).toEqual({ kind: 'model', engine: 'pi', model_id: 'openai/gpt-6.1-sol', thinking: 'high' });
    for (const model of catalog.models) {
        for (const thinking of ['auto', 'high']) {
            expect(codingChoiceFromSelection(model.choices[thinking])).toEqual({ kind: 'model', engine: model.engine, model_id: model.id, thinking });
        }
    }
    expect(codingChoiceFromSelection('auto')).toEqual({ kind: 'auto' });
    expect(() => codingChoiceFromSelection('missing')).toThrow('available');

    for (const status of [404, 503]) {
        fetch.mockImplementation(async () => new Response('unavailable', { status }));
        fetch.mockClear();
        await codingProfileStore.load();
        expect(fetch).toHaveBeenCalledTimes(1);
        expect(() => codingChoiceFromSelection('pi/high')).toThrow(String(status));
    }
});
