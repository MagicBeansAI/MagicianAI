import { describe, expect, it } from 'vitest';
import type { ModelChoice } from '$lib/llm/modelCatalog';
import {
	budgetTaskCandidates,
	EMPTY_SLASH_CONTEXT,
	filterSlashItems,
	formatBudgetCap,
	isSlashCommandInput,
	keepThinking,
	resolveSlashInput,
	thinkingChoices,
	slashPrefix,
	slashViewFor,
	type SlashCommandContext,
	type SlashItem,
	type SlashStep
} from './composerCommands';

const BUDGET: SlashStep = { id: 'budget', token: 'budget', label: '/budget', command: 'budget' };
const ENGINE: SlashStep = { id: 'engine', token: 'engine', label: '/engine', command: 'engine' };
const RUN: SlashStep = { id: 'run-engine', token: 'run-engine', label: '/run-engine', command: 'run-engine' };
const VOICE: SlashStep = { id: 'voice-mode', token: 'voice-mode', label: '/voice-mode', command: 'voice-mode' };
const TASK: SlashStep = { id: 't-1234567890', token: 't-123456', label: 'Research ACME', taskId: 't-1234567890' };
const engineStep = (engine: string, label = engine): SlashStep => ({ id: engine, token: engine, label, engine });
const modelStep = (choice: ModelChoice, engine: string): SlashStep => ({
	id: choice.id,
	token: choice.id,
	label: choice.label,
	engine,
	model: choice.id
});

/** A catalog row as `GET /chat/models` sends it; no `levels` = no thinking control. */
function model(id: string, label: string, provider: string, levels: string[] = [], fallback: string | null = null): ModelChoice {
	return {
		id,
		provider,
		connection: 'api',
		model: id,
		label,
		capabilities: { vision: false, tools: true, computer_use: false, structured_output: false, streaming: true, web_search: false },
		limits: null,
		thinking: { mode: levels.length > 0 ? 'effort' : 'none', levels, default: fallback },
		choices: Object.fromEntries((levels.length > 0 ? ['auto', ...levels] : ['auto']).map((level) => [level, `${id}@${level}`]))
	};
}

const SOL = model('openai:gpt-6.1-sol', 'GPT-6.1 Sol', 'openai', ['low', 'high']);
const SONNET = model('anthropic:claude-sonnet', 'Claude Sonnet', 'anthropic', ['low', 'medium'], 'medium');
const HAIKU = model('anthropic:claude-haiku', 'Claude Haiku', 'anthropic');

const CTX: SlashCommandContext = {
	engines: [
		{ name: 'magician', installed: true },
		{ name: 'pi', installed: true },
		{ name: 'claude_code', installed: true, models: ['default', 'opus', 'sonnet'] },
		{ name: 'codex', installed: true, models: ['default'] },
		{ name: 'grok', installed: false, models: ['grok-5'] }
	],
	chatEngine: { engine: 'magician', model: 'default' },
	serverChatEngine: { engine: 'magician', model: 'default' },
	runEngine: { engine: 'claude_code', model: 'opus' },
	chatModels: [SOL, SONNET, HAIKU],
	chatModelsEngine: 'magician',
	chatModelSelection: { model_id: SOL.id, thinking: 'low' },
	piModels: [SONNET, HAIKU],
	loopModels: [SOL, SONNET],
	loopSelection: { model_id: SOL.id, thinking: 'high' },
	voiceMode: 'hands_free',
	tasks: [
		{ id: 't-1234567890', title: 'Research ACME', status: 'paused' },
		{ id: 't-abcdef0000', title: 'Draft memo', status: 'running' }
	],
	budgetDefaults: { harness_max_cost_usd: 100, native_max_cost_usd: 'unlimited' }
};

describe('slash trigger', () => {
	it('fires only on a single-line draft that starts with /', () => {
		expect(isSlashCommandInput('/')).toBe(true);
		expect(isSlashCommandInput('/bud')).toBe(true);
		expect(isSlashCommandInput(' /bud')).toBe(false);
		expect(isSlashCommandInput('hello /budget')).toBe(false);
		expect(isSlashCommandInput('/budget\nmore')).toBe(false);
		expect(isSlashCommandInput('')).toBe(false);
	});

	it('builds the prefix from the chosen tokens', () => {
		expect(slashPrefix([])).toBe('/');
		expect(slashPrefix([BUDGET])).toBe('/budget ');
		expect(slashPrefix([BUDGET, TASK])).toBe('/budget t-123456 ');
	});

	it('keeps chosen levels while the draft carries their prefix', () => {
		expect(resolveSlashInput('/budget t-123456 +$', [BUDGET, TASK])).toEqual({
			open: true,
			path: [BUDGET, TASK],
			query: '+$'
		});
	});

	it('steps back a level when the user deletes into a chosen token', () => {
		expect(resolveSlashInput('/budget t-123456', [BUDGET, TASK])).toEqual({
			open: true,
			path: [BUDGET],
			query: 't-123456'
		});
		expect(resolveSlashInput('/budget', [BUDGET])).toEqual({ open: true, path: [], query: 'budget' });
	});

	it('reads NBSPs from the contenteditable as spaces', () => {
		expect(resolveSlashInput('/budget ', [BUDGET]).path).toEqual([BUDGET]);
	});

	it('closes when the draft stops being a command', () => {
		expect(resolveSlashInput('budget', [BUDGET])).toEqual({ open: false, path: [], query: '' });
	});
});

describe('slash option lists', () => {
	it('lists the five commands and filters them as you type', () => {
		const all = slashViewFor([], '', CTX).items.map((item) => item.label);
		expect(all).toEqual(['/budget', '/budget default', '/engine', '/run-engine', '/voice-mode']);
		expect(slashViewFor([], 'eng', CTX).items.map((item) => item.label)).toEqual(['/engine', '/run-engine']);
		// a prefix match ranks first
		expect(slashViewFor([], 'run', CTX).items[0].label).toBe('/run-engine');
		expect(slashViewFor([], 'budget d', CTX).items[0].label).toBe('/budget default');
		expect(slashViewFor([], 'voice', CTX).items.map((item) => item.label)).toEqual(['/voice-mode']);
		expect(slashViewFor([], 'zzz', CTX).items).toEqual([]);
	});

	it('every command descends into its options', () => {
		for (const item of slashViewFor([], '', CTX).items) {
			expect(item.next?.command).toBeTruthy();
			expect(item.action).toBeUndefined();
		}
	});

	it('says engines are loading until the roster arrives', () => {
		const view = slashViewFor([ENGINE], '', EMPTY_SLASH_CONTEXT);
		expect(view.items).toEqual([]);
		expect(view.emptyText).toMatch(/Loading engines/);
	});
});

describe('/engine', () => {
	const labels = (items: SlashItem[]) => items.map((item) => item.label);

	it('shows the current choice and one row per installed engine', () => {
		const view = slashViewFor([ENGINE], '', CTX);
		expect(view.summary).toBe('Now: Magician · GPT-6.1 Sol · Low (this browser) · everywhere: Magician');
		expect(labels(view.items)).toEqual(['Magician', 'Coding agent', 'Claude Code', 'Codex', 'Make default everywhere…']);
		const current = view.items.filter((item) => item.current);
		expect(current.map((item) => [item.label, item.detail])).toEqual([['Magician', 'GPT-6.1 Sol · Low']]);
	});

	it('opens a model level for an engine with a choice and sets a default-only engine at once', () => {
		const items = slashViewFor([ENGINE], '', CTX).items;
		const byLabel = (label: string) => items.find((item) => item.label === label) as SlashItem;
		expect(byLabel('Magician').next).toMatchObject({ token: 'magician', engine: 'magician' });
		expect(byLabel('Coding agent').next).toMatchObject({ token: 'pi', engine: 'pi' });
		expect(byLabel('Claude Code').next).toMatchObject({ token: 'claude_code', engine: 'claude_code' });
		expect(byLabel('Codex').next).toBeUndefined();
		expect(byLabel('Codex').action).toEqual({ type: 'chat-engine', engine: 'codex', model: 'default', scope: 'device' });
	});

	it('/engine magician lists the dropdown catalog; a model with thinking levels opens them', () => {
		const view = slashViewFor([ENGINE, engineStep('magician', 'Magician')], '', CTX);
		expect(labels(view.items)).toEqual(['GPT-6.1 Sol', 'Claude Sonnet', 'Claude Haiku']);
		expect(view.items.filter((item) => item.current).map((item) => item.label)).toEqual(['GPT-6.1 Sol']);
		expect(view.items[0].detail).toBe('OpenAI · Low');
		expect(view.items[1].action).toBeUndefined();
		expect(view.items[1].next).toEqual({
			id: SONNET.id,
			token: SONNET.id,
			label: 'Claude Sonnet',
			engine: 'magician',
			model: SONNET.id
		});
		// Haiku has no thinking control: it is picked at once.
		expect(view.items[2].action).toEqual({
			type: 'chat-engine',
			engine: 'magician',
			model: 'default',
			scope: 'device',
			selection: { model_id: HAIKU.id, thinking: 'auto' },
			modelLabel: 'Claude Haiku'
		});
		expect(labels(filterSlashItems(view.items, 'anthropic haiku'))).toEqual(['Claude Haiku']);
	});

	it('/engine magician <model> lists its thinking levels and picks one', () => {
		const sonnet = slashViewFor([ENGINE, engineStep('magician', 'Magician'), modelStep(SONNET, 'magician')], '', CTX);
		expect(sonnet.title).toBe('/engine › Magician › Claude Sonnet · this browser');
		expect(labels(sonnet.items)).toEqual(['Auto', 'Low', 'Medium']);
		expect(sonnet.items[0].detail).toBe('Default · Medium');
		// Not the model in use: its default is marked.
		expect(sonnet.items.filter((item) => item.current).map((item) => item.label)).toEqual(['Medium']);
		expect(sonnet.items[1].action).toEqual({
			type: 'chat-engine',
			engine: 'magician',
			model: 'default',
			scope: 'device',
			selection: { model_id: SONNET.id, thinking: 'low' },
			modelLabel: 'Claude Sonnet · Low'
		});
		expect(sonnet.items[0].action).toMatchObject({ selection: { model_id: SONNET.id, thinking: 'auto' }, modelLabel: 'Claude Sonnet' });
		// The model in use marks its level.
		const sol = slashViewFor([ENGINE, engineStep('magician', 'Magician'), modelStep(SOL, 'magician')], '', CTX);
		expect(sol.items.filter((item) => item.current).map((item) => item.label)).toEqual(['Low']);
		expect(slashPrefix([ENGINE, engineStep('magician'), modelStep(SOL, 'magician')])).toBe(`/engine magician ${SOL.id} `);
	});

	it('/engine pi waits for its own catalog, then marks nothing while chat is on Magician', () => {
		const step = engineStep('pi', 'Coding agent');
		const loading = slashViewFor([ENGINE, step], '', CTX);
		expect(loading.items).toEqual([]);
		expect(loading.emptyText).toBe('Loading models…');
		const view = slashViewFor([ENGINE, step], '', { ...CTX, chatModels: [SONNET, HAIKU], chatModelsEngine: 'pi' });
		expect(labels(view.items)).toEqual(['Claude Sonnet', 'Claude Haiku']);
		expect(view.items.some((item) => item.current)).toBe(false);
		expect(view.items[0].next).toMatchObject({ engine: 'pi', model: SONNET.id });
		expect(view.items[1].action).toMatchObject({ engine: 'pi', selection: { model_id: HAIKU.id, thinking: 'auto' } });
	});

	it('says when the catalog could not be read and stays on the level', () => {
		const view = slashViewFor([ENGINE, engineStep('magician')], '', { ...CTX, chatModels: null, chatModelsError: 'HTTP 500' });
		expect(view.items).toEqual([]);
		expect(view.note).toBe('Models unavailable: HTTP 500');
	});

	it('/engine claude_code lists the roster models', () => {
		const ctx: SlashCommandContext = { ...CTX, chatEngine: { engine: 'claude_code', model: 'opus' } };
		expect(slashViewFor([ENGINE], '', ctx).summary).toBe('Now: Claude Code · opus (this browser) · everywhere: Magician');
		const view = slashViewFor([ENGINE, engineStep('claude_code', 'Claude Code')], '', ctx);
		expect(labels(view.items)).toEqual(['CLI default', 'opus', 'sonnet']);
		expect(view.items.find((item) => item.current)?.label).toBe('opus');
		expect(view.items[2].action).toEqual({ type: 'chat-engine', engine: 'claude_code', model: 'sonnet', scope: 'device' });
		expect(labels(filterSlashItems(view.items, 'son'))).toEqual(['sonnet']);
	});

	it('"default everywhere" lists engines only and keeps the model a browser choice', () => {
		const engineItems = slashViewFor([ENGINE], '', CTX).items;
		const everywhere = engineItems[engineItems.length - 1];
		expect(everywhere.next?.scope).toBe('everywhere');
		const view = slashViewFor([ENGINE, everywhere.next as SlashStep], '', CTX);
		expect(labels(view.items)).toEqual(['Magician', 'Coding agent', 'Claude Code', 'Codex']);
		expect(view.items.find((item) => item.current)?.label).toBe('Magician');
		expect(view.summary).toContain('The model stays a choice of this browser');
		expect(view.items[2].action).toEqual({ type: 'chat-engine', engine: 'claude_code', model: 'default', scope: 'everywhere' });
	});
});

describe('/run-engine', () => {
	it('shows the run engine, then "Same as Chat" and the engines', () => {
		const view = slashViewFor([RUN], '', CTX);
		expect(view.summary).toBe('Now: Claude Code · opus');
		expect(view.items.map((item) => item.label)).toEqual(['Same as Chat', 'Magician', 'Coding agent', 'Claude Code', 'Codex']);
		expect(view.items[0].action).toEqual({ type: 'run-engine', engine: 'chat', model: 'default' });
		expect(view.items.filter((item) => item.current).map((item) => [item.label, item.detail])).toEqual([['Claude Code', 'opus']]);
	});

	it('says when runs follow the chat engine', () => {
		const view = slashViewFor([RUN], '', { ...CTX, runFollowsChat: true, runEngine: { engine: 'magician', model: 'default' } });
		expect(view.summary).toBe('Now: Same as Chat (Magician)');
		expect(view.items.filter((item) => item.current).map((item) => item.label)).toEqual(['Same as Chat']);
	});

	it('/run-engine magician saves the loop model and pins Magician only when runs resolve elsewhere', () => {
		const step = engineStep('magician', 'Magician');
		const view = slashViewFor([RUN, step], '', CTX);
		expect(view.items.map((item) => item.label)).toEqual(['GPT-6.1 Sol', 'Claude Sonnet']);
		// Runs think with Claude Code, so no loop model is in force.
		expect(view.items.some((item) => item.current)).toBe(false);
		expect(view.items[1].next).toMatchObject({ engine: 'magician', model: SONNET.id });
		const levels = slashViewFor([RUN, step, modelStep(SONNET, 'magician')], '', CTX);
		expect(levels.items.map((item) => item.label)).toEqual(['Auto', 'Low', 'Medium']);
		expect(levels.items[2].action).toEqual({
			type: 'run-loop-model',
			selection: { model_id: SONNET.id, thinking: 'medium' },
			modelLabel: 'Claude Sonnet · Medium',
			pinMagician: true
		});
		const onMagician: SlashCommandContext = { ...CTX, runEngine: { engine: 'magician', model: 'default' } };
		expect(slashViewFor([RUN, step], '', onMagician).items.find((item) => item.current)?.label).toBe('GPT-6.1 Sol');
		expect(slashViewFor([RUN, step, modelStep(SOL, 'magician')], '', onMagician).items.find((item) => item.current)?.label).toBe('High');
		expect(slashViewFor([RUN, step, modelStep(SONNET, 'magician')], '', onMagician).items[1].action).toMatchObject({
			type: 'run-loop-model',
			pinMagician: false
		});
		// "Same as Chat" with a Magician chat default resolves to Magician too.
		const follows: SlashCommandContext = { ...CTX, runFollowsChat: true };
		expect(slashViewFor([RUN, step], '', follows).items.find((item) => item.current)?.label).toBe('GPT-6.1 Sol');
		const failed = slashViewFor([RUN, step], '', { ...CTX, loopModels: null, loopModelsError: 'Routing overview failed (503)' });
		expect(failed.note).toBe('Models unavailable: Routing overview failed (503)');
	});

	it('/run-engine pi sends a model selection with the engine', () => {
		const ctx: SlashCommandContext = {
			...CTX,
			runEngine: { engine: 'pi', model: 'default' },
			runModelSelection: { model_id: HAIKU.id, thinking: 'auto' }
		};
		expect(slashViewFor([RUN], '', ctx).summary).toBe('Now: Coding agent · Claude Haiku');
		const view = slashViewFor([RUN, engineStep('pi', 'Coding agent')], '', ctx);
		expect(view.items.map((item) => item.label)).toEqual(['Default', 'Claude Sonnet', 'Claude Haiku']);
		expect(view.items.find((item) => item.current)?.label).toBe('Claude Haiku');
		expect(view.items[1].next).toMatchObject({ engine: 'pi', model: SONNET.id });
		expect(view.items[2].action).toEqual({
			type: 'run-engine',
			engine: 'pi',
			model: 'default',
			selection: { model_id: HAIKU.id, thinking: 'auto' },
			modelLabel: 'Claude Haiku'
		});
		expect(slashViewFor([RUN, engineStep('pi')], '', { ...ctx, piModels: null }).emptyText).toBe('Loading models…');
	});

	it('/run-engine pi "Default" clears the run model and is current while none is set', () => {
		const ctx: SlashCommandContext = { ...CTX, runEngine: { engine: 'pi', model: 'default' }, runModelSelection: null };
		expect(slashViewFor([RUN], '', ctx).summary).toBe('Now: Coding agent');
		const first = slashViewFor([RUN, engineStep('pi', 'Coding agent')], '', ctx).items[0];
		expect(first).toMatchObject({ label: 'Default', detail: "The coding agent's own default model", current: true });
		expect(first.action).toEqual({ type: 'run-engine', engine: 'pi', model: 'default', selection: null, modelLabel: 'Default' });
	});

	it('/run-engine claude_code sets a roster model', () => {
		const view = slashViewFor([RUN, engineStep('claude_code')], '', CTX);
		expect(view.items.find((item) => item.current)?.label).toBe('opus');
		expect(view.items[0].action).toEqual({ type: 'run-engine', engine: 'claude_code', model: 'default' });
	});
});

describe('/voice-mode', () => {
	it('marks the current mode and shows unavailable modes without an action', () => {
		const view = slashViewFor([VOICE], '', { ...CTX, voiceModesAvailable: ['recording', 'hands_free'] });
		expect(view.summary).toBe('Now: Hands-free');
		expect(view.items.map((item) => item.label)).toEqual(['Live', 'Dictate', 'Hands-free']);
		expect(view.items.find((item) => item.current)?.label).toBe('Hands-free');
		expect(view.items[0]).toMatchObject({ disabled: true, detail: 'Not available here' });
		expect(view.items[0].action).toBeUndefined();
		expect(view.items[1].action).toEqual({ type: 'voice-mode', mode: 'recording' });
		expect(slashViewFor([VOICE], 'call', CTX).items.map((item) => item.label)).toEqual(['Live']);
	});

	it('offers every mode, none marked, before the preference loads', () => {
		const view = slashViewFor([VOICE], '', { ...CTX, voiceMode: null });
		expect(view.summary).toBeUndefined();
		expect(view.items.every((item) => !item.current && !!item.action)).toBe(true);
	});
});

describe('thinkingChoices', () => {
	it('lists Auto then the control levels, and nothing without a control', () => {
		expect(thinkingChoices(SONNET)).toEqual(['auto', 'low', 'medium']);
		expect(thinkingChoices(HAIKU)).toEqual([]);
	});
});

describe('keepThinking', () => {
	it('keeps the current level when offered, else the model default, else auto', () => {
		expect(keepThinking(SONNET, { model_id: SOL.id, thinking: 'low' })).toBe('low');
		expect(keepThinking(SONNET, { model_id: SOL.id, thinking: 'high' })).toBe('medium');
		expect(keepThinking(SOL, { model_id: SONNET.id, thinking: 'medium' })).toBe('auto');
		expect(keepThinking(HAIKU, null)).toBe('auto');
	});
});

describe('slash option lists (budget)', () => {
	it('/budget lists the chat tasks, then the grants for the chosen one', () => {
		const tasks = slashViewFor([BUDGET], '', CTX);
		expect(tasks.items.map((item) => item.label)).toEqual(['Research ACME', 'Draft memo']);
		expect(tasks.items[0].next?.taskId).toBe('t-1234567890');
		const grants = slashViewFor([BUDGET, TASK], '', CTX);
		expect(grants.items.map((item) => item.label)).toEqual([
			'+$25',
			'+$100',
			'+1M tokens',
			'+24 hours',
			'+50 iterations',
			'Unlimited'
		]);
		expect(grants.items.map((item) => item.action)).toEqual([
			expect.objectContaining({ type: 'task-budget', taskId: 't-1234567890', grant: { add_cost_usd: 25 } }),
			expect.objectContaining({ grant: { add_cost_usd: 100 } }),
			expect.objectContaining({ grant: { add_tokens: 1_000_000 } }),
			expect.objectContaining({ grant: { add_duration_secs: 86400 } }),
			expect.objectContaining({ grant: { add_iterations: 50 } }),
			expect.objectContaining({ grant: { unlimited: true } })
		]);
	});

	it('/budget says when the chat has nothing to extend', () => {
		const view = slashViewFor([BUDGET], '', { ...CTX, tasks: [] });
		expect(view.items).toEqual([]);
		expect(view.emptyText).toBe('No running or paused task in this chat');
	});

	it('/budget default offers both caps with the current values marked', () => {
		const step: SlashStep = { id: 'budget-default', token: 'budget default', label: '/budget default', command: 'budget-default' };
		const view = slashViewFor([step], '', CTX);
		expect(view.items.map((item) => item.label)).toEqual([
			'Harness runs · $50',
			'Harness runs · $100',
			'Harness runs · $250',
			'Harness runs · Unlimited',
			'Native runs · $5',
			'Native runs · $15',
			'Native runs · $50',
			'Native runs · Unlimited'
		]);
		expect(view.items.filter((item) => item.current).map((item) => item.label)).toEqual([
			'Harness runs · $100',
			'Native runs · Unlimited'
		]);
		expect(slashViewFor([step], 'native', CTX).items).toHaveLength(4);
		// loading, then an error note that still lets the user pick
		expect(slashViewFor([step], '', { ...CTX, budgetDefaults: null }).emptyText).toMatch(/Loading/);
		const failed = slashViewFor([step], '', { ...CTX, budgetDefaults: null, budgetDefaultsError: 'HTTP 404' });
		expect(failed.items).toHaveLength(8);
		expect(failed.items.some((item) => item.current)).toBe(false);
		expect(failed.note).toContain('HTTP 404');
	});

	it('formats caps', () => {
		expect(formatBudgetCap(100)).toBe('$100');
		expect(formatBudgetCap(12.5)).toBe('$12.50');
		expect(formatBudgetCap('unlimited')).toBe('Unlimited');
	});
});

describe('budgetTaskCandidates', () => {
	const card = (task_id: string, status: string, created_at: number, display_label?: string) => ({
		content: { type: 'task_status_update', task_id, status, display_label },
		created_at
	});

	it('merges transcript cards with the thread task store, newest first, running/paused only', () => {
		const result = budgetTaskCandidates(
			[
				card('a', 'running', 1000, 'From card A'),
				card('b', 'running', 2000, 'Card B'),
				card('b', 'completed', 3000),
				card('c', 'paused', 4000),
				{ content: { type: 'text' }, created_at: 9000 }
			],
			[
				{ id: 'a', title: 'Store A', status: 'paused', createdAt: '1970-01-01T00:00:05.000Z' },
				{ id: 'd', title: 'Store D', status: 'in_progress', createdAt: '1970-01-01T00:00:00.500Z' },
				{ id: 'e', title: 'Store E', status: 'completed', createdAt: '1970-01-01T00:00:09.000Z' }
			]
		);
		expect(result).toEqual([
			{ id: 'a', title: 'Store A', status: 'paused' },
			{ id: 'c', title: 'Task c', status: 'paused' },
			{ id: 'd', title: 'Store D', status: 'in_progress' }
		]);
	});
});
