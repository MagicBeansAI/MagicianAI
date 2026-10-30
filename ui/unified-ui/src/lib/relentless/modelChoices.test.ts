import { expect, it } from 'vitest';
import { qualityModels, qualitySelection } from './modelChoices';
import type { ModelChoice } from '$lib/llm/modelCatalog';
import type { RoutingProfile } from '$lib/stores/modelRoutingStore';

it('exposes bounded workload assignments even when general model routes are not eligible', () => {
	const model = (id: string): ModelChoice => ({id, provider: 'openai', connection: id, model: id, label: id,
		capabilities: {vision: false, tools: false, computer_use: false, structured_output: true, streaming: true, web_search: false}, limits: null,
		thinking: {mode: 'effort', levels: ['low', 'medium', 'high'], default: 'medium'},
		choices: {auto: `chat/${id}/auto`}, operation_choices: {auto: `model/${id}/auto`, low: `model/${id}/low`, high: `model/${id}/high`}});
	const routes = [{name: 'operation/actor', family: 'a', model: 'a', maximum_request_microusd: 1},
		{name: 'operation/review', family: 'b', model: 'b', maximum_request_microusd: 1}];
	const metadata: RoutingProfile[] = routes.map(route => ({name: route.name, provider: 'openai', model: route.model,
		class: 'api', installed: true, model_selection: {model_id: route.model, thinking: route.family === 'a' ? 'high' : 'low'}}));
	const models = qualityModels([model('a'), model('b'), model('unqualified')], routes, metadata);
	expect(models.map(model => model.id)).toEqual(['a', 'b']);
	expect(models[0].choices).toEqual({auto: 'operation/actor', high: 'operation/actor'});
	expect(models[0].thinking).toEqual({mode: 'effort', levels: ['high'], default: 'high'});
	expect(models[1].thinking.default).toBe('low');
	expect(qualitySelection(models, metadata, 'operation/actor')).toEqual({model_id: 'a', thinking: 'auto'});
	expect(qualitySelection(models, metadata, 'retired-choice')).toBeNull();
	expect(qualityModels([model('a'), model('b')], routes, metadata, 'a').map(model => model.id)).toEqual(['b']);
	const noJson = routes.map(route => ({...route, supports_json: false}));
	expect(qualityModels([model('a'), model('b')], noJson, metadata).map(model => model.id)).toEqual(['a', 'b']);
	expect(qualityModels([model('a'), model('b')], noJson, metadata, 'a')).toEqual([]);
});
