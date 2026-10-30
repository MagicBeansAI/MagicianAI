import { describe, expect, it } from 'vitest';
import { chatModelRows, typedChatModelRequest } from './chatModelSelection';
import type { ModelChoice } from './modelCatalog';

const model: ModelChoice = {
    id: 'provider/model', label: 'Model', provider: 'provider', connection: 'provider', model: 'physical-model',
    capabilities: { vision: true, tools: true, streaming: true, structured_output: false, web_search: false, computer_use: false },
    limits: null, thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
    choices: { auto: 'opaque-auto', low: 'opaque-low', high: 'opaque-high' }
};

describe('typed chat model catalog and admission', () => {
    it('derives picker choices and the configured default without server profile rows', () => {
        const rows = chatModelRows([model], { model_id: model.id, thinking: 'auto' });
        expect(rows.map(row => row.name)).toEqual(['opaque-auto', 'opaque-low', 'opaque-high']);
        expect(rows.filter(row => row.is_default).map(row => row.name)).toEqual(['opaque-auto']);
        expect(rows.filter(row => row.model_choice)).toHaveLength(1);
        expect(chatModelRows([model], null).some(row => row.is_default)).toBe(false);
    });
    it('sends the exact model and thinking with an explicit engine and no profile/harness model', () => {
        const rows = chatModelRows([model], null);
        for (const engine of ['magician', 'pi']) {
            for (const thinking of ['auto', 'high']) {
                const body = typedChatModelRequest([model], rows, model.choices[thinking], engine, 'stale-native-model');
                expect(body).toEqual({ harness_engine: engine, model_selection: { model_id: model.id, thinking } });
            }
        }
    });
    it('refuses missing/removed choices and preserves native CLI model identity', () => {
        const rows = chatModelRows([model], null);
        for (const route of [null, 'old-profile', 'removed-route']) {
            expect(() => typedChatModelRequest([model], rows, route, 'magician')).toThrow('unavailable');
        }
        expect(typedChatModelRequest([model], rows, 'opaque-high', 'claude_code', 'claude-opus-5'))
            .toEqual({ harness_engine: 'claude_code', harness_model: 'claude-opus-5' });
    });
});
