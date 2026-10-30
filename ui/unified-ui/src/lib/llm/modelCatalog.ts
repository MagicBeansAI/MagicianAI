/** Server-owned model controls shared by chat and operation assignment. */
export type ModelFeedback = 'fast' | 'standard' | 'premium';

export interface ModelSelection {
	model_id: string;
	thinking: string;
}

export interface ModelChoice {
	id: string;
	provider: string;
	connection: string;
	model: string;
	label: string;
	/** Decorative catalog hint; absent only on an older server. */
	feedback?: ModelFeedback;
	installed?: boolean;
	capabilities: {
		vision: boolean;
		tools: boolean;
		computer_use: boolean;
		structured_output: boolean;
		streaming: boolean;
		web_search: boolean;
	};
	limits: {
		context_window_tokens: number;
		max_input_tokens?: number | null;
		min_output_tokens: number;
		max_output_tokens: number;
		default_output_tokens?: number | null;
		shared_context_window: boolean;
	} | null;
	thinking: {
		mode: 'none' | 'effort' | 'toggle' | 'budget';
		levels: string[];
		default: string | null;
	};
	choices: Record<string, string>;
	/** Concrete route handles for background and agent assignments. */
	operation_choices?: Record<string, string>;
	/** Runtime discovery; absent on an older server. `unavailable` = the
	 * source no longer offers it; `catalogued: false` = offered but unpriced. */
	availability?: ModelAvailability | null;
}

export interface ModelAvailability {
	state: 'available' | 'unavailable' | 'unknown';
	source: string;
	catalogued: boolean;
	checked_at_ms?: number;
}

/** Pickers hide models their source no longer offers, except the one already chosen. */
export function offeredModels<T extends Pick<ModelChoice, 'id' | 'availability'>>(models: readonly T[], selectedId?: string | null): T[] {
	return models.filter(model => model.availability?.state !== 'unavailable' || model.id === selectedId);
}

export function modelProviderLabel(provider: string): string {
	return ({ 'openai': 'OpenAI', 'anthropic': 'Anthropic', 'gemini': 'Gemini', 'deepseek': 'DeepSeek', 'minimax': 'MiniMax', 'xai': 'xAI', 'sarvam': 'Sarvam', 'ollama': 'Ollama', 'yutori': 'Yutori', 'openrouter': 'OpenRouter', 'harness-claude_code': 'Claude Code', 'harness-codex': 'Codex', 'harness-grok': 'Grok', 'harness-agy': 'Antigravity', 'harness-pi': 'Pi' } as Record<string, string>)[provider] ?? provider;
}

export function thinkingLabel(level: string): string {
	return ({ auto: 'Auto', none: 'Off', on: 'On', minimal: 'Minimal', low: 'Low', medium: 'Medium', high: 'High', xhigh: 'Extra high', max: 'Max', ultra: 'Ultra' } as Record<string, string>)[level] ?? level;
}

export function selectionForRoute(
	models: readonly ModelChoice[],
	profiles: readonly { name: string; model_selection?: ModelSelection | null }[],
	route: string | null | undefined
): ModelSelection | null {
	const saved = profiles.find(profile => profile.name === route)?.model_selection;
	if (saved && models.some(model => model.id === saved.model_id)) return saved;
	for (const model of models) {
		const thinking = [...Object.entries(model.choices), ...Object.entries(model.operation_choices ?? {})].find(([, choice]) => choice === route)?.[0];
		if (thinking) return { model_id: model.id, thinking };
	}
	return null;
}

export function routeForSelection(models: readonly ModelChoice[], selection: ModelSelection): string | null {
	return models.find(model => model.id === selection.model_id)?.choices[selection.thinking] ?? null;
}

export function selectModel(models: readonly ModelChoice[], modelId: string, previous: ModelSelection | null): ModelSelection {
	const model = models.find(model => model.id === modelId);
	const thinking = previous && model?.choices[previous.thinking] ? previous.thinking : 'auto';
	return { model_id: modelId, thinking };
}

export function contextLabel(tokens: number): string {
	return tokens >= 1_000_000 ? `${(tokens / 1_000_000).toFixed(1).replace(/\.0$/, '')}M` : `${Math.round(tokens / 1_000)}K`;
}

export function routeForOperationSelection(models: readonly ModelChoice[], selection: ModelSelection): string | null {
	const model = models.find(model => model.id === selection.model_id);
	return (model?.operation_choices ?? model?.choices)?.[selection.thinking] ?? null;
}
