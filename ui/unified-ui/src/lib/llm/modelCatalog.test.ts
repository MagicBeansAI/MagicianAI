import { describe, expect, it } from 'vitest';
import { offeredModels, routeForSelection, routeForOperationSelection, selectModel, selectionForRoute, thinkingLabel, type ModelChoice } from './modelCatalog';

function model(id: string, levels: string[]): ModelChoice {
	return {
		id, label: id, model: id, provider: 'example', connection: 'example', limits: null,
		capabilities: { vision: false, tools: true, streaming: true, computer_use: false, structured_output: false, web_search: false },
		thinking: { mode: 'effort', levels, default: levels[0] },
		choices: Object.fromEntries(['auto', ...levels].map(level => [level, `opaque-${id}-${level}`]))
	};
}

describe('model selection compatibility', () => {
	const models = [model('a', ['low', 'medium', 'high']), model('b', ['low', 'high'])];
	it('resolves a saved profile alias without interpreting its name', () => {
		expect(selectionForRoute(models, [{ name: 'old-profile-out64k', model_selection: { model_id: 'a', thinking: 'medium' } }], 'old-profile-out64k'))
			.toEqual({ model_id: 'a', thinking: 'medium' });
	});
	it('preserves supported effort when switching models and resets unsupported effort to Auto', () => {
		expect(selectModel(models, 'b', { model_id: 'a', thinking: 'high' }).thinking).toBe('high');
		expect(selectModel(models, 'b', { model_id: 'a', thinking: 'medium' }).thinking).toBe('auto');
	});
	it('cannot synthesize unsupported settings or revive removed models', () => {
		expect(routeForSelection(models, { model_id: 'b', thinking: 'medium' })).toBeNull();
		expect(selectionForRoute(models, [{ name: 'old', model_selection: { model_id: 'removed', thinking: 'high' } }], 'old')).toBeNull();
	});
	it('uses separate concrete assignment handles for Auto', () => {
		const model = { ...models[0], operation_choices: { auto: 'concrete-initial' } };
		expect(routeForOperationSelection([model], { model_id: 'a', thinking: 'auto' })).toBe('concrete-initial');
		expect(selectionForRoute([model], [], 'concrete-initial')).toEqual({ model_id: 'a', thinking: 'auto' });
		expect(routeForSelection([model], { model_id: 'a', thinking: 'auto' })).toBe('opaque-a-auto');
	});
	it('keeps opaque route IDs out of display labels', () => {
		expect(thinkingLabel('none')).toBe('Off');
		expect(thinkingLabel('xhigh')).toBe('Extra high');
	});
});

describe('runtime model availability', () => {
	it('hides models their source no longer offers unless already chosen', () => {
		const gone = { ...model('gone', []), availability: { state: 'unavailable' as const, source: 'harness-agy', catalogued: true } };
		const fresh = { ...model('fresh', []), availability: { state: 'available' as const, source: 'harness-agy', catalogued: false } };
		const legacy = model('legacy', []);
		expect(offeredModels([gone, fresh, legacy]).map(model => model.id)).toEqual(['fresh', 'legacy']);
		expect(offeredModels([gone, fresh], 'gone').map(model => model.id)).toEqual(['gone', 'fresh']);
	});
});
