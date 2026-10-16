import { describe, expect, it } from 'vitest';
import {
	budgetTaskCandidates,
	EMPTY_SLASH_CONTEXT,
	engineChoiceRows,
	filterSlashItems,
	formatBudgetCap,
	isSlashCommandInput,
	resolveSlashInput,
	slashPrefix,
	slashViewFor,
	type SlashCommandContext,
	type SlashStep
} from './composerCommands';

const BUDGET: SlashStep = { id: 'budget', token: 'budget', label: '/budget', command: 'budget' };
const ENGINE: SlashStep = { id: 'engine', token: 'engine', label: '/engine', command: 'engine' };
const TASK: SlashStep = { id: 't-1234567890', token: 't-123456', label: 'Research ACME', taskId: 't-1234567890' };

const CTX: SlashCommandContext = {
	engines: [
		{ name: 'magician', installed: true },
		{ name: 'claude_code', installed: true, models: ['default', 'opus', 'sonnet'] },
		{ name: 'codex', installed: false, models: ['gpt-5'] }
	],
	chatEngine: { engine: 'claude_code', model: 'opus' },
	serverChatEngine: { engine: 'magician', model: 'default' },
	runEngine: { engine: 'claude_code', model: 'default' },
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
	it('lists the four commands and filters them as you type', () => {
		const all = slashViewFor([], '', CTX).items.map((item) => item.label);
		expect(all).toEqual(['/budget', '/budget default', '/engine', '/run-engine']);
		expect(slashViewFor([], 'eng', CTX).items.map((item) => item.label)).toEqual(['/engine', '/run-engine']);
		// a prefix match ranks first
		expect(slashViewFor([], 'run', CTX).items[0].label).toBe('/run-engine');
		expect(slashViewFor([], 'budget d', CTX).items[0].label).toBe('/budget default');
		expect(slashViewFor([], 'zzz', CTX).items).toEqual([]);
	});

	it('every command descends into its options', () => {
		for (const item of slashViewFor([], '', CTX).items) {
			expect(item.next?.command).toBeTruthy();
			expect(item.action).toBeUndefined();
		}
	});

	it('offers installed engine × model rows with the current one marked', () => {
		expect(engineChoiceRows(CTX.engines ?? [])).toEqual([
			{ engine: 'magician', model: 'default' },
			{ engine: 'claude_code', model: 'default' },
			{ engine: 'claude_code', model: 'opus' },
			{ engine: 'claude_code', model: 'sonnet' }
		]);
		const view = slashViewFor([ENGINE], '', CTX);
		const current = view.items.filter((item) => item.current);
		expect(current.map((item) => item.label)).toEqual(['Claude Code · opus']);
		expect(current[0].action).toEqual({ type: 'chat-engine', engine: 'claude_code', model: 'opus', scope: 'device' });
		expect(view.items.some((item) => item.label.startsWith('Codex'))).toBe(false);
		// the last row descends into the everywhere variant
		const everywhere = view.items[view.items.length - 1];
		expect(everywhere.next?.scope).toBe('everywhere');
		const evView = slashViewFor([ENGINE, everywhere.next as SlashStep], '', CTX);
		expect(evView.items.find((item) => item.current)?.label).toBe('Magician · default model');
		expect(evView.items[0].action).toMatchObject({ type: 'chat-engine', scope: 'everywhere' });
	});

	it('/run-engine marks the run engine and sets it', () => {
		const step: SlashStep = { id: 'run-engine', token: 'run-engine', label: '/run-engine', command: 'run-engine' };
		const view = slashViewFor([step], 'claude', CTX);
		expect(view.items.find((item) => item.current)?.action).toEqual({
			type: 'run-engine',
			engine: 'claude_code',
			model: 'default'
		});
	});

	it('says engines are loading until the roster arrives', () => {
		const view = slashViewFor([ENGINE], '', EMPTY_SLASH_CONTEXT);
		expect(view.items).toEqual([]);
		expect(view.emptyText).toMatch(/Loading engines/);
	});

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

	it('filters on every word', () => {
		const items = slashViewFor([ENGINE], '', CTX).items;
		expect(filterSlashItems(items, 'claude son').map((item) => item.label)).toEqual(['Claude Code · sonnet']);
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
