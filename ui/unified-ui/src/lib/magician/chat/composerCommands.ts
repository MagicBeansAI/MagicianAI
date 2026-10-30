// ─── Composer slash commands ────────────────────────────────────────────────
// Pure logic for the `/` command picker (docs/plans/2026-10-03-slash-commands-
// and-run-budgets.md). The composer owns only the UI wiring — the trigger, the
// level the picker is on, filtering and the option rows all live here so web
// behaviour is testable without a DOM.
//
// The picker is text-driven: the composer value is always
// `/<token> <token> … <query>`, where each token is a level the user already
// chose (see `slashPrefix`). Deleting into a token (Backspace) therefore steps
// back a level on its own — `resolveSlashInput` pops every step whose prefix
// the text no longer carries. Commands never reach the model: choosing an
// option yields a `SlashAction` the host runs client-side.
//
// `/engine`, `/run-engine` and `/voice-mode` offer what the composer's engine/
// model dropdown, Settings → Agentic flows and the voice switch already offer,
// through the same APIs (docs/plans/2026-10-30-slash-engine-model-voice-
// design.md). iOS and Android keep the same rows and wording.
import { withCredentials } from '$lib/settings/harnessAuth';
import {
	modelProviderLabel,
	offeredModels,
	thinkingLabel,
	type ModelChoice,
	type ModelSelection
} from '$lib/llm/modelCatalog';
import type { VoiceMode } from '$lib/media/voice/wakeWord';

export type SlashCommandId = 'budget' | 'budget-default' | 'engine' | 'run-engine' | 'voice-mode';

/** A level the user chose. `token` is what the composer text shows for it. */
export interface SlashStep {
	id: string;
	token: string;
	label: string;
	command?: SlashCommandId;
	/** `/budget` → the chosen task. */
	taskId?: string;
	/** `/engine` → "make default everywhere" sub-level. */
	scope?: 'everywhere';
	/** `/engine` / `/run-engine` → the engine whose models are listed. */
	engine?: string;
	/** … → the catalog model whose thinking levels are listed. */
	model?: string;
}

export type BudgetCap = number | 'unlimited';

export interface BudgetDefaults {
	native_max_cost_usd: BudgetCap;
	harness_max_cost_usd: BudgetCap;
}

export interface TaskBudgetGrant {
	add_cost_usd?: number;
	add_tokens?: number;
	add_iterations?: number;
	add_duration_secs?: number;
	unlimited?: boolean;
}

/** `selection` is a Magician/Pi catalog pick (the `model_selection` the
 *  composer or the run engine sends); `modelLabel` names it in the toast.
 *  On `run-engine`, `selection: null` clears the coding agent's run model
 *  (its own default), as Settings' "Use default" does. */
export type SlashAction =
	| {
			type: 'chat-engine';
			engine: string;
			model: string;
			scope: 'device' | 'everywhere';
			selection?: ModelSelection;
			modelLabel?: string;
	  }
	| { type: 'run-engine'; engine: string; model: string; selection?: ModelSelection | null; modelLabel?: string }
	/** Magician's loop model: the `agentic_decision` mapping. `pinMagician`
	 *  first pins the run engine when runs do not resolve to Magician. */
	| { type: 'run-loop-model'; selection: ModelSelection; modelLabel: string; pinMagician: boolean }
	| { type: 'voice-mode'; mode: VoiceMode }
	| { type: 'task-budget'; taskId: string; taskLabel: string; grant: TaskBudgetGrant; grantLabel: string }
	| { type: 'budget-default'; field: keyof BudgetDefaults; value: BudgetCap };

export interface SlashItem {
	id: string;
	label: string;
	detail?: string;
	/** Short uppercase badge, like the `@` picker's kind badge. */
	badge: string;
	/** The value in force right now (marked in the list). */
	current?: boolean;
	searchText?: string;
	/** Choosing it descends a level … */
	next?: SlashStep;
	/** … or runs it. One of `next` / `action` is set, unless `disabled`. */
	action?: SlashAction;
	/** Shown but not pickable (e.g. a voice mode this browser lacks). */
	disabled?: boolean;
}

export interface SlashEngineRow {
	name: string;
	installed: boolean;
	models?: string[];
	/** Whose credentials a launch uses (`login`, `magician_key`, `none`). */
	credentials?: string;
}

export interface SlashEngineChoice {
	engine: string;
	model: string;
}

/** A task `/budget` can extend: running or paused, in this chat. */
export interface SlashBudgetTask {
	id: string;
	title: string;
	status: string;
}

/** Everything the option lists read. `null` fields are still loading. */
export interface SlashCommandContext {
	engines: SlashEngineRow[] | null;
	/** Why the roster could not be read (the list then offers Magician only). */
	enginesError?: string | null;
	/** This browser's chat engine (`chatHarnessPreferenceStore`). */
	chatEngine: SlashEngineChoice | null;
	/** The server's chat default (roster `chat_current` / `chat_model`). */
	serverChatEngine: SlashEngineChoice | null;
	/** The run engine (roster `current` / `run_model`). */
	runEngine: SlashEngineChoice | null;
	/** Agentic flows follow the chat engine (roster `follows_chat`). */
	runFollowsChat?: boolean;
	/** The run engine's catalog pick (roster `run_model_selection`, Pi). */
	runModelSelection?: ModelSelection | null;
	/** `GET /chat/models?engine=<chatModelsEngine>` rows (the composer
	 *  dropdown's list); `null` while loading. */
	chatModels?: ModelChoice[] | null;
	chatModelsEngine?: string | null;
	chatModelsError?: string | null;
	/** The composer's model pick when its engine is Magician or Pi. */
	chatModelSelection?: ModelSelection | null;
	/** Pi catalog for `/run-engine pi`. */
	piModels?: ModelChoice[] | null;
	piModelsError?: string | null;
	/** The `agentic_decision` operation's models and current pick
	 *  (`GET /llm/routing`), for `/run-engine magician`. */
	loopModels?: ModelChoice[] | null;
	loopSelection?: ModelSelection | null;
	loopModelsError?: string | null;
	/** The voice switch's mode (`voiceModeStore`); `null` before it loads. */
	voiceMode?: VoiceMode | null;
	/** Modes this surface can start; `null`/absent means all of them. */
	voiceModesAvailable?: readonly VoiceMode[] | null;
	tasks: SlashBudgetTask[];
	budgetDefaults: BudgetDefaults | null;
	budgetDefaultsError?: string | null;
}

export const EMPTY_SLASH_CONTEXT: SlashCommandContext = {
	engines: null,
	chatEngine: null,
	serverChatEngine: null,
	runEngine: null,
	tasks: [],
	budgetDefaults: null
};

export interface SlashCommandDef {
	id: SlashCommandId;
	token: string;
	label: string;
	detail: string;
}

export const SLASH_COMMANDS: readonly SlashCommandDef[] = [
	{
		id: 'budget',
		token: 'budget',
		label: '/budget',
		detail: 'Add budget to a running or paused task in this chat'
	},
	{
		id: 'budget-default',
		token: 'budget default',
		label: '/budget default',
		detail: 'Default cost cap for new runs'
	},
	{
		id: 'engine',
		token: 'engine',
		label: '/engine',
		detail: 'Engine that answers this chat'
	},
	{
		id: 'run-engine',
		token: 'run-engine',
		label: '/run-engine',
		detail: 'Engine that drives task runs'
	},
	{
		id: 'voice-mode',
		token: 'voice-mode',
		label: '/voice-mode',
		detail: 'What the voice button starts'
	}
];

/** Engines whose model is a catalog pick (`GET /chat/models?engine=`) sent
 *  as `model_selection`; every other engine takes a roster model name. */
export const CATALOG_ENGINES: readonly string[] = ['magician', 'pi'];

/** The voice switch's modes, in picker order. Wire values are the web's
 *  `VoiceMode` (`recording` is Dictate). */
export const VOICE_MODE_OPTIONS: ReadonlyArray<{ mode: VoiceMode; label: string; detail: string; searchText: string }> = [
	{ mode: 'realtime', label: 'Live', detail: 'The voice button starts a live voice call', searchText: 'call realtime' },
	{ mode: 'recording', label: 'Dictate', detail: 'Tap to record, hold to talk', searchText: 'dictation recording' },
	{ mode: 'hands_free', label: 'Hands-free', detail: 'Local listening and speech, no tap per turn', searchText: 'hands free' }
];

export function voiceModeLabel(mode: VoiceMode): string {
	return VOICE_MODE_OPTIONS.find((option) => option.mode === mode)?.label ?? mode;
}

export const HARNESS_DEFAULT_CAPS: readonly BudgetCap[] = [50, 100, 250, 'unlimited'];
export const NATIVE_DEFAULT_CAPS: readonly BudgetCap[] = [5, 15, 50, 'unlimited'];

export const TASK_BUDGET_GRANTS: ReadonlyArray<{ id: string; label: string; detail: string; grant: TaskBudgetGrant }> = [
	{ id: 'cost-25', label: '+$25', detail: 'Raise the cost cap by $25', grant: { add_cost_usd: 25 } },
	{ id: 'cost-100', label: '+$100', detail: 'Raise the cost cap by $100', grant: { add_cost_usd: 100 } },
	{ id: 'tokens-1m', label: '+1M tokens', detail: 'Raise the token limit by 1,000,000', grant: { add_tokens: 1_000_000 } },
	{ id: 'duration-24h', label: '+24 hours', detail: 'Add 24 hours of active time on the next resume', grant: { add_duration_secs: 86400 } },
	{ id: 'iterations-50', label: '+50 iterations', detail: 'Allow 50 more iterations', grant: { add_iterations: 50 } },
	{ id: 'unlimited', label: 'Unlimited', detail: 'Remove the cost cap and token meter for this run', grant: { unlimited: true } }
];

// ─── Trigger + level resolution ─────────────────────────────────────────────

/** `/` + every chosen token, each followed by one space. Root is `/`. */
export function slashPrefix(path: readonly SlashStep[]): string {
	return '/' + path.map((step) => `${step.token} `).join('');
}

/** contenteditable may hand back NBSPs for typed spaces. */
function normalizeSpaces(value: string): string {
	return value.replace(/ /g, ' ');
}

/** Is this composer value a slash command (a `/` first, a single line)? */
export function isSlashCommandInput(value: string): boolean {
	return value.startsWith('/') && !value.includes('\n');
}

/** Read the composer value against the levels chosen so far: drop every level
 *  whose prefix the text lost (the user deleted into it), and return the
 *  remaining text as the query for the current level. */
export function resolveSlashInput(
	value: string,
	path: readonly SlashStep[]
): { open: boolean; path: SlashStep[]; query: string } {
	const text = normalizeSpaces(value);
	if (!isSlashCommandInput(text)) return { open: false, path: [], query: '' };
	let kept = path.slice();
	while (kept.length > 0 && !text.startsWith(slashPrefix(kept))) kept = kept.slice(0, -1);
	return { open: true, path: kept, query: text.slice(slashPrefix(kept).length) };
}

// ─── Filtering ──────────────────────────────────────────────────────────────

/** Every whitespace word of the query must appear in the row's text; rows whose
 *  label starts with the query rank first, otherwise list order holds. */
export function filterSlashItems(items: readonly SlashItem[], query: string): SlashItem[] {
	const needle = normalizeSpaces(query).trim().toLowerCase();
	if (!needle) return items.slice();
	const words = needle.split(/\s+/);
	const matches = items.filter((item) => {
		const haystack = `${item.label} ${item.detail ?? ''} ${item.badge} ${item.searchText ?? ''}`.toLowerCase();
		return words.every((word) => haystack.includes(word));
	});
	const startsWith = (item: SlashItem) => item.label.toLowerCase().replace(/^\//, '').startsWith(needle.replace(/^\//, ''));
	return [...matches.filter(startsWith), ...matches.filter((item) => !startsWith(item))];
}

// ─── Option lists ───────────────────────────────────────────────────────────

const ENGINE_NAMES: Record<string, string> = {
	magician: 'Magician',
	pi: 'Coding agent',
	claude_code: 'Claude Code',
	codex: 'Codex',
	codex_app_server: 'Codex App Server',
	grok: 'Grok',
	agy: 'Antigravity'
};

export function engineDisplayName(name: string): string {
	return (
		ENGINE_NAMES[name] ??
		name
			.split(/[_\s]+/)
			.filter(Boolean)
			.map((word) => word.charAt(0).toUpperCase() + word.slice(1))
			.join(' ')
	);
}

export function formatBudgetCap(cap: BudgetCap): string {
	if (cap === 'unlimited') return 'Unlimited';
	return Number.isInteger(cap) ? `$${cap}` : `$${cap.toFixed(2)}`;
}

function isCatalogEngine(engine: string): boolean {
	return CATALOG_ENGINES.includes(engine);
}

/** The thinking level to send with a newly picked model: the current one when
 *  the model offers it, else the model's default. `levels` is the route map
 *  the pick resolves through (`choices`, or an operation's choices). */
export function keepThinking(
	choice: ModelChoice,
	current: ModelSelection | null | undefined,
	levels: Record<string, string> = choice.choices
): string {
	const offers = (level: string | null | undefined): level is string => !!level && level in levels;
	const kept = current?.thinking;
	if (offers(kept)) return kept;
	if (offers(choice.thinking.default)) return choice.thinking.default;
	if (offers('auto')) return 'auto';
	return Object.keys(levels)[0] ?? 'auto';
}

/** The thinking levels a model lets you choose, in `ThinkingControl`'s order
 *  (Auto, then the control's levels), limited to those `levels` routes. Empty
 *  when the model has no thinking control. */
export function thinkingChoices(choice: ModelChoice, levels: Record<string, string> = choice.choices): string[] {
	if (choice.thinking.mode === 'none') return [];
	return Array.from(new Set(['auto', ...choice.thinking.levels])).filter((level) => level in levels);
}

/** "GPT-6.1 Sol · Low": the model and its thinking level, which is left out
 *  for `auto` and for a model without a thinking control. */
function choiceLabel(choice: ModelChoice, thinking: string | null | undefined): string {
	return choice.thinking.mode === 'none' || !thinking || thinking === 'auto'
		? choice.label
		: `${choice.label} · ${thinkingLabel(thinking)}`;
}

/** The selection, named as `choiceLabel` does. `null` while the catalog that
 *  names it is not loaded. */
export function modelSelectionLabel(
	models: readonly ModelChoice[] | null | undefined,
	selection: ModelSelection | null | undefined
): string | null {
	if (!selection) return null;
	const choice = models?.find((model) => model.id === selection.model_id);
	return choice ? choiceLabel(choice, selection.thinking) : null;
}

function credentialsOf(ctx: SlashCommandContext, engine: string): string | undefined {
	return ctx.engines?.find((row) => row.name === engine)?.credentials;
}

function engineRowLabel(ctx: SlashCommandContext, engine: string): string {
	return withCredentials(engineDisplayName(engine), credentialsOf(ctx, engine));
}

/** An engine's roster models, `default` first. */
function rosterModels(ctx: SlashCommandContext, engine: string): string[] {
	const models = ctx.engines?.find((row) => row.name === engine)?.models ?? [];
	return ['default', ...models.filter((model) => model && model !== 'default')];
}

/** Does picking this engine open a model level? Magician/Pi always do (their
 *  catalog); a roster engine does when it lists more than `default`. */
function engineHasModelChoice(ctx: SlashCommandContext, engine: string): boolean {
	return isCatalogEngine(engine) || rosterModels(ctx, engine).length > 1;
}

function installedEngines(ctx: SlashCommandContext): SlashEngineRow[] {
	return (ctx.engines ?? []).filter((row) => row.installed);
}

/** One row per installed engine. An engine with a model choice descends a
 *  level; a default-only one runs `actionFor` at once. */
function engineLevelItems(
	ctx: SlashCommandContext,
	idPrefix: string,
	isCurrent: (engine: string) => boolean,
	currentDetail: (engine: string) => string | null,
	actionFor: (engine: string) => SlashAction
): SlashItem[] {
	return installedEngines(ctx).map((row) => {
		const current = isCurrent(row.name);
		const item: SlashItem = {
			id: `${idPrefix}:${row.name}`,
			label: engineRowLabel(ctx, row.name),
			badge: 'Engine',
			current,
			searchText: row.name
		};
		const detail = current ? currentDetail(row.name) : null;
		if (detail) item.detail = detail;
		if (engineHasModelChoice(ctx, row.name)) {
			item.next = { id: row.name, token: row.name, label: engineDisplayName(row.name), engine: row.name };
		} else {
			item.action = actionFor(row.name);
		}
		return item;
	});
}

/** A catalog level's inputs: the list, the pick in force (`current`, only
 *  when this engine is the one in use), the pick whose thinking level a new
 *  model keeps, and how a pick becomes an action. */
interface CatalogLevel {
	engine: string;
	idPrefix: string;
	current: ModelSelection | null | undefined;
	keepFrom: ModelSelection | null | undefined;
	toAction: (selection: ModelSelection, label: string) => SlashAction;
	levelsOf?: (choice: ModelChoice) => Record<string, string>;
}

const routesOf = (level: CatalogLevel, choice: ModelChoice): Record<string, string> =>
	level.levelsOf ? level.levelsOf(choice) : choice.choices;

/** Catalog rows (the dropdown's list; models the source dropped are hidden
 *  unless current). A model with a thinking choice opens its levels; any
 *  other acts at once, keeping the current thinking level when offered. */
function catalogModelItems(models: readonly ModelChoice[], level: CatalogLevel): SlashItem[] {
	const currentId = level.current?.model_id;
	return offeredModels(models, currentId).map((choice) => {
		const current = !!currentId && currentId === choice.id;
		const detail = [modelProviderLabel(choice.provider)];
		if (current && level.current && choice.thinking.mode !== 'none' && level.current.thinking !== 'auto') {
			detail.push(thinkingLabel(level.current.thinking));
		}
		const item: SlashItem = {
			id: `${level.idPrefix}:${choice.id}`,
			label: choice.label,
			detail: detail.join(' · '),
			badge: 'Model',
			current,
			searchText: `${choice.id} ${choice.model} ${choice.provider}`
		};
		if (thinkingChoices(choice, routesOf(level, choice)).length > 1) {
			item.next = { id: choice.id, token: choice.id, label: choice.label, engine: level.engine, model: choice.id };
		} else {
			const selection: ModelSelection = { model_id: choice.id, thinking: keepThinking(choice, level.keepFrom, routesOf(level, choice)) };
			item.action = level.toAction(selection, choiceLabel(choice, selection.thinking));
		}
		return item;
	});
}

/** A model's thinking levels (`ThinkingControl`'s labels and order). The
 *  level in force is marked when this is the model in use, else the model's
 *  default. */
function thinkingItems(choice: ModelChoice, level: CatalogLevel): SlashItem[] {
	const levels = thinkingChoices(choice, routesOf(level, choice));
	const current = level.current;
	const inUse = current && current.model_id === choice.id ? current.thinking : null;
	const marked = inUse && levels.includes(inUse)
		? inUse
		: choice.thinking.default && levels.includes(choice.thinking.default) ? choice.thinking.default : 'auto';
	return levels.map((thinking) => ({
		id: `${level.idPrefix}:${choice.id}:${thinking}`,
		label: thinkingLabel(thinking),
		detail:
			thinking === 'auto' && choice.thinking.default && choice.thinking.default !== 'auto'
				? `Default · ${thinkingLabel(choice.thinking.default)}`
				: undefined,
		badge: 'Thinking',
		current: thinking === marked,
		searchText: thinking,
		action: level.toAction({ model_id: choice.id, thinking }, choiceLabel(choice, thinking))
	}));
}

/** Roster model rows for an engine that is not catalog-driven. */
function rosterModelItems(
	ctx: SlashCommandContext,
	engine: string,
	isCurrent: (model: string) => boolean,
	idPrefix: string,
	toAction: (model: string) => SlashAction
): SlashItem[] {
	return rosterModels(ctx, engine).map((model) => ({
		id: `${idPrefix}:${engine}:${model}`,
		label: model === 'default' ? 'CLI default' : model,
		badge: 'Model',
		current: isCurrent(model),
		searchText: `${engine} ${model}`,
		action: toAction(model)
	}));
}

/** A roster model as the "Now" line names it: nothing for `default`. */
function rosterModelLabel(model: string | null | undefined): string | null {
	return model && model !== 'default' ? model : null;
}

/** The composer's model, named: a catalog label for Magician/Pi, else the
 *  roster model. */
function chatModelLabel(ctx: SlashCommandContext): string | null {
	const chat = ctx.chatEngine;
	if (!chat) return null;
	if (!isCatalogEngine(chat.engine)) return rosterModelLabel(chat.model);
	return modelSelectionLabel(ctx.chatModelsEngine === chat.engine ? ctx.chatModels : null, ctx.chatModelSelection);
}

/** The run engine's model, named: the loop mapping for Magician, the run's
 *  catalog pick for Pi (nothing for its own default), else the roster model. */
function runModelLabel(ctx: SlashCommandContext, engine: string): string | null {
	if (engine === 'magician') return modelSelectionLabel(ctx.loopModels, ctx.loopSelection);
	if (engine === 'pi') return modelSelectionLabel(ctx.piModels, ctx.runModelSelection);
	return rosterModelLabel(ctx.runEngine?.model);
}

/** Do agentic flows run on Magician's own loop? Its engine, or "Same as
 *  Chat" while the chat default is Magician (Settings' `resolvedRunEngine`). */
export function runResolvesToMagician(ctx: SlashCommandContext): boolean {
	return ctx.runFollowsChat === true ? ctx.serverChatEngine?.engine === 'magician' : ctx.runEngine?.engine === 'magician';
}

function joinLabel(engine: string, model: string | null): string {
	return model ? `${engineDisplayName(engine)} · ${model}` : engineDisplayName(engine);
}

function budgetDefaultItems(ctx: SlashCommandContext): SlashItem[] {
	const items: SlashItem[] = [];
	const add = (field: keyof BudgetDefaults, caps: readonly BudgetCap[], badge: string, what: string) => {
		for (const cap of caps) {
			const current = ctx.budgetDefaults ? ctx.budgetDefaults[field] === cap : false;
			items.push({
				id: `${field}:${cap}`,
				label: `${what} · ${formatBudgetCap(cap)}`,
				detail: current ? 'Current' : undefined,
				badge,
				current,
				searchText: `${field} ${cap}`,
				action: { type: 'budget-default', field, value: cap }
			});
		}
	};
	add('harness_max_cost_usd', HARNESS_DEFAULT_CAPS, 'Harness', 'Harness runs');
	add('native_max_cost_usd', NATIVE_DEFAULT_CAPS, 'Native', 'Native runs');
	return items;
}

export interface SlashView {
	/** Breadcrumb of the current level, e.g. `/budget › Research ACME`. */
	title: string;
	items: SlashItem[];
	/** Shown when `items` is empty (loading, nothing to pick, or no match). */
	emptyText: string;
	/** A note above the rows (e.g. the current defaults could not be read). */
	note?: string;
	/** The value in force, above the rows (e.g. `Now: Magician · GPT-6.1 Sol`). */
	summary?: string;
}

/** A model level: rows once the list is in, `Loading models…` before, and a
 *  note (the level stays) when it could not be read. */
function modelLevelView(
	title: string,
	query: string,
	models: readonly ModelChoice[] | null | undefined,
	error: string | null | undefined,
	build: (models: readonly ModelChoice[]) => SlashItem[]
): SlashView {
	const note = error ? `Models unavailable: ${error}` : undefined;
	if (!models) return { title, items: [], emptyText: error ? 'No models to choose' : 'Loading models…', note };
	return {
		title,
		items: filterSlashItems(build(models), query),
		emptyText: models.length === 0 ? 'No models to choose' : 'No matching option',
		note
	};
}

/** A catalog engine's model level, or — `modelStep` chosen — that model's
 *  thinking levels. */
function catalogLevelView(
	title: string,
	query: string,
	models: readonly ModelChoice[] | null | undefined,
	error: string | null | undefined,
	level: CatalogLevel,
	modelStep: SlashStep | undefined,
	prefix: SlashItem[] = []
): SlashView {
	const modelId = modelStep?.model;
	return modelLevelView(title, query, models, error, (list) => {
		if (!modelId) return [...prefix, ...catalogModelItems(list, level)];
		const choice = list.find((model) => model.id === modelId);
		return choice ? thinkingItems(choice, level) : [];
	});
}

/** `/engine`: engines (this browser), then the chosen engine's models (and a
 *  model's thinking levels), or the engines alone for "default everywhere". */
function chatEngineView(
	title: string,
	query: string,
	ctx: SlashCommandContext,
	rest: readonly SlashStep[],
	note: string | undefined
): SlashView {
	const noMatch = 'No matching option';
	const chat = ctx.chatEngine;
	const server = ctx.serverChatEngine;
	const step = rest[0];
	if (step?.scope === 'everywhere') {
		// The server stores an engine (and a roster model), never a Magician/Pi
		// catalog pick, so this level lists engines only.
		const modelFor = (engine: string): string => {
			if (isCatalogEngine(engine)) return 'default';
			if (chat?.engine === engine) return chat.model || 'default';
			if (server?.engine === engine) return server.model || 'default';
			return 'default';
		};
		const items: SlashItem[] = installedEngines(ctx).map((row) => {
			const current = server?.engine === row.name;
			const item: SlashItem = {
				id: `everywhere:${row.name}`,
				label: engineRowLabel(ctx, row.name),
				badge: 'Engine',
				current,
				searchText: row.name,
				action: { type: 'chat-engine', engine: row.name, model: modelFor(row.name), scope: 'everywhere' }
			};
			if (current && server && !isCatalogEngine(row.name) && server.model && server.model !== 'default') {
				item.detail = server.model;
			}
			return item;
		});
		const now = server ? `Now everywhere: ${engineDisplayName(server.engine)}. ` : '';
		return {
			title,
			items: filterSlashItems(items, query),
			emptyText: noMatch,
			note,
			summary: `${now}The model stays a choice of this browser.`
		};
	}
	if (step?.engine) {
		const engine = step.engine;
		const levelTitle = `${title} · this browser`;
		if (isCatalogEngine(engine)) {
			const own = ctx.chatModelsEngine === engine;
			return catalogLevelView(
				levelTitle,
				query,
				own ? ctx.chatModels : null,
				own ? ctx.chatModelsError : null,
				{
					engine,
					idPrefix: `device:${engine}`,
					current: chat?.engine === engine ? ctx.chatModelSelection : null,
					keepFrom: ctx.chatModelSelection,
					toAction: (selection, modelLabel) => ({
						type: 'chat-engine',
						engine,
						model: 'default',
						scope: 'device',
						selection,
						modelLabel
					})
				},
				rest[1]
			);
		}
		const items = rosterModelItems(
			ctx,
			engine,
			(model) => chat?.engine === engine && (chat.model || 'default') === model,
			'device',
			(model) => ({ type: 'chat-engine', engine, model, scope: 'device' })
		);
		return { title: levelTitle, items: filterSlashItems(items, query), emptyText: noMatch, note };
	}
	const items: SlashItem[] = [
		...engineLevelItems(
			ctx,
			'device',
			(engine) => chat?.engine === engine,
			() => chatModelLabel(ctx),
			(engine) => ({ type: 'chat-engine', engine, model: 'default', scope: 'device' })
		),
		{
			id: 'engine:everywhere',
			label: 'Make default everywhere…',
			detail: 'Set the chat engine for every client, not just this browser',
			badge: 'All clients',
			searchText: 'everywhere default all clients server',
			next: { id: 'everywhere', token: 'everywhere', label: 'Default everywhere', scope: 'everywhere' }
		}
	];
	const summary = chat
		? `Now: ${joinLabel(chat.engine, chatModelLabel(ctx))} (this browser)` +
			(server ? ` · everywhere: ${engineDisplayName(server.engine)}` : '')
		: undefined;
	return { title: `${title} · this browser`, items: filterSlashItems(items, query), emptyText: noMatch, note, summary };
}

/** `/run-engine` (Settings → Agentic flows): "Same as Chat" and the engines,
 *  then Magician's loop model, Pi's catalog or a roster engine's models. */
function runEngineView(
	title: string,
	query: string,
	ctx: SlashCommandContext,
	rest: readonly SlashStep[],
	note: string | undefined
): SlashView {
	const noMatch = 'No matching option';
	const run = ctx.runEngine;
	const follows = ctx.runFollowsChat === true;
	const pinned = (engine: string) => !follows && run?.engine === engine;
	const step = rest[0];
	if (step?.engine === 'magician') {
		const onMagician = runResolvesToMagician(ctx);
		return catalogLevelView(
			title,
			query,
			ctx.loopModels,
			ctx.loopModelsError,
			{
				engine: 'magician',
				idPrefix: 'loop',
				// The loop model is "current" only while runs think with Magician.
				current: onMagician ? ctx.loopSelection : null,
				keepFrom: ctx.loopSelection,
				toAction: (selection, modelLabel) => ({
					type: 'run-loop-model',
					selection,
					modelLabel,
					pinMagician: !onMagician
				}),
				levelsOf: (choice) => choice.operation_choices ?? choice.choices
			},
			rest[1]
		);
	}
	if (step?.engine === 'pi') {
		// Settings' "Use default": clear the run model (`model_selection: null`).
		const useDefault: SlashItem = {
			id: 'run:pi:default',
			label: 'Default',
			detail: "The coding agent's own default model",
			badge: 'Model',
			current: pinned('pi') && !ctx.runModelSelection,
			searchText: 'default',
			action: { type: 'run-engine', engine: 'pi', model: 'default', selection: null, modelLabel: 'Default' }
		};
		return catalogLevelView(
			title,
			query,
			ctx.piModels,
			ctx.piModelsError,
			{
				engine: 'pi',
				idPrefix: 'run:pi',
				current: pinned('pi') ? ctx.runModelSelection : null,
				keepFrom: ctx.runModelSelection,
				toAction: (selection, modelLabel) => ({ type: 'run-engine', engine: 'pi', model: 'default', selection, modelLabel })
			},
			rest[1],
			[useDefault]
		);
	}
	if (step?.engine) {
		const engine = step.engine;
		const items = rosterModelItems(
			ctx,
			engine,
			(model) => pinned(engine) && (run?.model || 'default') === model,
			'run',
			(model) => ({ type: 'run-engine', engine, model })
		);
		return { title, items: filterSlashItems(items, query), emptyText: noMatch, note };
	}
	const chatDefault = engineDisplayName(ctx.serverChatEngine?.engine ?? run?.engine ?? 'magician');
	const items: SlashItem[] = [
		{
			id: 'run:chat',
			label: 'Same as Chat',
			detail: `Follow the chat default (${chatDefault})`,
			badge: 'Engine',
			current: follows,
			searchText: 'same as chat follow default',
			action: { type: 'run-engine', engine: 'chat', model: 'default' }
		},
		...engineLevelItems(
			ctx,
			'run',
			pinned,
			(engine) => runModelLabel(ctx, engine),
			(engine) => ({ type: 'run-engine', engine, model: 'default' })
		)
	];
	let summary: string | undefined;
	if (follows) summary = `Now: Same as Chat (${chatDefault})`;
	else if (run) summary = `Now: ${joinLabel(run.engine, runModelLabel(ctx, run.engine))}`;
	return { title, items: filterSlashItems(items, query), emptyText: noMatch, note, summary };
}

/** The rows for the level `path` points at, filtered by `query`. */
export function slashViewFor(
	path: readonly SlashStep[],
	query: string,
	ctx: SlashCommandContext
): SlashView {
	const title = ['/', ...path.map((step) => step.label)].join(' › ').replace('/ › ', '');
	if (path.length === 0) {
		const items: SlashItem[] = SLASH_COMMANDS.map((command) => ({
			id: `command:${command.id}`,
			label: command.label,
			detail: command.detail,
			badge: 'Command',
			next: { id: command.id, token: command.token, label: command.label, command: command.id }
		}));
		return { title: 'Commands', items: filterSlashItems(items, query), emptyText: 'No matching command' };
	}
	const [root, ...rest] = path;
	const noMatch = 'No matching option';
	switch (root.command) {
		case 'engine':
		case 'run-engine': {
			if (ctx.engines === null) return { title, items: [], emptyText: 'Loading engines…' };
			const note = ctx.enginesError ? `Engine list unavailable: ${ctx.enginesError}` : undefined;
			if (root.command === 'run-engine') return runEngineView(title, query, ctx, rest, note);
			return chatEngineView(title, query, ctx, rest, note);
		}
		case 'voice-mode': {
			const available = ctx.voiceModesAvailable ?? null;
			const items: SlashItem[] = VOICE_MODE_OPTIONS.map((option) => {
				const usable = !available || available.includes(option.mode);
				const item: SlashItem = {
					id: `voice:${option.mode}`,
					label: option.label,
					detail: usable ? option.detail : 'Not available here',
					badge: 'Voice',
					current: ctx.voiceMode === option.mode,
					searchText: option.searchText
				};
				if (usable) item.action = { type: 'voice-mode', mode: option.mode };
				else item.disabled = true;
				return item;
			});
			const summary = ctx.voiceMode ? `Now: ${voiceModeLabel(ctx.voiceMode)}` : undefined;
			return { title, items: filterSlashItems(items, query), emptyText: noMatch, summary };
		}
		case 'budget': {
			const taskStep = rest[0];
			if (!taskStep?.taskId) {
				const items: SlashItem[] = ctx.tasks.map((task) => ({
					id: `task:${task.id}`,
					label: task.title,
					detail: `${task.status} · ${task.id.slice(0, 8)}`,
					badge: 'Task',
					searchText: task.id,
					next: { id: task.id, token: task.id.slice(0, 8), label: task.title, taskId: task.id }
				}));
				return {
					title,
					items: filterSlashItems(items, query),
					emptyText: ctx.tasks.length === 0 ? 'No running or paused task in this chat' : noMatch
				};
			}
			const items: SlashItem[] = TASK_BUDGET_GRANTS.map((option) => ({
				id: `grant:${option.id}`,
				label: option.label,
				detail: option.detail,
				badge: 'Budget',
				action: {
					type: 'task-budget',
					taskId: taskStep.taskId as string,
					taskLabel: taskStep.label,
					grant: option.grant,
					grantLabel: option.label
				}
			}));
			return { title, items: filterSlashItems(items, query), emptyText: noMatch };
		}
		case 'budget-default': {
			if (ctx.budgetDefaults === null && !ctx.budgetDefaultsError) {
				return { title, items: [], emptyText: 'Loading current defaults…' };
			}
			const note = ctx.budgetDefaultsError
				? `Current defaults unavailable: ${ctx.budgetDefaultsError}`
				: undefined;
			return { title, items: filterSlashItems(budgetDefaultItems(ctx), query), emptyText: noMatch, note };
		}
		default:
			return { title, items: [], emptyText: noMatch };
	}
}

// ─── Task candidates for /budget ────────────────────────────────────────────

const BUDGETABLE_STATUSES = new Set(['running', 'in_progress', 'paused']);

/** The chat's running/paused tasks, newest first: the union of the transcript's
 *  task cards (latest card per task wins) and this thread's `$taskStore` rows.
 *  The store's status is fresher than an old card, so it wins when both know a
 *  task. */
export function budgetTaskCandidates(
	cards: Iterable<{
		content: { type: string; task_id?: string; status?: string; display_label?: string; label?: string };
		created_at?: number;
	}>,
	tasks: ReadonlyArray<{ id: string; title?: string; status: string; createdAt?: string; updatedAt?: string }>
): SlashBudgetTask[] {
	const byId = new Map<string, { id: string; title: string; status: string; at: number }>();
	for (const card of cards) {
		if (card.content.type !== 'task_status_update') continue;
		const id = card.content.task_id?.trim();
		const status = card.content.status?.trim();
		if (!id || !status) continue;
		const prior = byId.get(id);
		byId.set(id, {
			id,
			title: card.content.display_label?.trim() || card.content.label?.trim() || prior?.title || '',
			status,
			at: Math.max(prior?.at ?? 0, card.created_at ?? 0)
		});
	}
	for (const task of tasks) {
		const prior = byId.get(task.id);
		const at = Date.parse(task.createdAt ?? '') || 0;
		byId.set(task.id, {
			id: task.id,
			title: task.title?.trim() || prior?.title || '',
			status: task.status,
			at: Math.max(prior?.at ?? 0, at)
		});
	}
	return [...byId.values()]
		.filter((task) => BUDGETABLE_STATUSES.has(task.status))
		.sort((a, b) => b.at - a.at)
		.map((task) => ({ id: task.id, title: task.title || `Task ${task.id.slice(0, 8)}`, status: task.status }));
}
