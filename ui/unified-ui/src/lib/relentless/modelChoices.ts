import { selectionForRoute, type ModelChoice, type ModelSelection } from '$lib/llm/modelCatalog';
import type { RoutingProfile } from '$lib/stores/modelRoutingStore';
import type { Profile } from './api';

/** Only the Quality owner's bounded, priced routes can be sealed in a draft. */
export function qualityModels(models: ModelChoice[], routes: Profile[], catalogRoutes: RoutingProfile[], excludedFamily?: string): ModelChoice[] {
	const eligible = new Set(routes.filter(route => !excludedFamily || (route.family !== excludedFamily && route.supports_json !== false)).map(route => route.name));
	return models.filter(model => !model.provider.startsWith('harness-')).map(model => {
		const choices = Object.fromEntries(Object.entries(model.operation_choices ?? model.choices).filter(([, route]) => eligible.has(route)));
		let defaultLevel = model.thinking.default;
		// A bounded workload may qualify an assignment without qualifying the
		// general model route. Auto follows that assignment's sealed effort.
		if (!choices.auto) {
			const assignment = catalogRoutes.find(route => eligible.has(route.name) && route.model_selection?.model_id === model.id);
			if (assignment?.model_selection) {
				choices.auto = assignment.name;
				const level = assignment.model_selection.thinking;
				if (model.thinking.levels.includes(level)) { choices[level] = assignment.name; defaultLevel = level; }
			}
		}
		return { ...model, operation_choices: undefined, choices,
			thinking: { ...model.thinking, default: defaultLevel, levels: model.thinking.levels.filter(level => Boolean(choices[level])) } };
	}).filter(model => Boolean(model.choices.auto));
}

export function qualitySelection(models: ModelChoice[], routes: RoutingProfile[], route: string): ModelSelection | null {
	const automatic = models.find(model => model.choices.auto === route);
	return automatic ? {model_id: automatic.id, thinking: 'auto'} : selectionForRoute(models, routes, route);
}
