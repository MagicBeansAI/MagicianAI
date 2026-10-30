import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { writable } from 'svelte/store';

const select = vi.fn();
const adoptServer = vi.fn();
const notifyDefault = vi.fn();
const setChatHarnessEngine = vi.fn();
const setRunHarnessEngine = vi.fn();
const setOperationModel = vi.fn();
const saveMediaPreferences = vi.fn();

vi.mock('$lib/stores/scopeIdentityStore', () => ({
	scopedRequestHeaders: (headers?: HeadersInit) => new Headers(headers)
}));
vi.mock('$lib/stores/chatHarnessPreferenceStore', () => ({
	chatHarnessPreferenceStore: { select: (...args: unknown[]) => select(...args), adoptServer: (...args: unknown[]) => adoptServer(...args) },
	notifyChatHarnessDefaultChanged: (...args: unknown[]) => notifyDefault(...args)
}));
vi.mock('$lib/plane/terminalGrants', () => ({
	setChatHarnessEngine: (...args: unknown[]) => setChatHarnessEngine(...args),
	setRunHarnessEngine: (...args: unknown[]) => setRunHarnessEngine(...args)
}));
vi.mock('$lib/stores/modelRoutingStore', () => ({
	setOperationModel: (...args: unknown[]) => setOperationModel(...args)
}));
vi.mock('$lib/media/preferences', () => ({
	saveMediaPreferences: (...args: unknown[]) => saveMediaPreferences(...args)
}));

import {
	BUDGET_API_UNAVAILABLE,
	describeEffectiveBudget,
	fetchBudgetDefaults,
	grantTaskBudget,
	runSlashAction,
	type SlashActionDeps
} from './slashCommandActions';

/** A chat panel's model store: the catalog `ensureLoaded` brings in. */
function panelProfiles(modelIds: string[], selectionError: string | null = null) {
	const state = writable({ models: modelIds.map((id) => ({ id })), selectionError });
	const store = { subscribe: state.subscribe, ensureLoaded: vi.fn(async () => {}), selectModel: vi.fn() };
	return { store, deps: store as unknown as NonNullable<SlashActionDeps['chatProfiles']> };
}

function jsonResponse(status: number, body: unknown): Response {
	return new Response(body === undefined ? '' : JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
	fetchMock = vi.fn();
	vi.stubGlobal('fetch', fetchMock);
	select.mockReset();
	adoptServer.mockReset();
	notifyDefault.mockReset();
	setChatHarnessEngine.mockReset();
	setRunHarnessEngine.mockReset();
	setOperationModel.mockReset();
	saveMediaPreferences.mockReset();
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('budget endpoints', () => {
	it('reads the defaults (number or "unlimited")', async () => {
		fetchMock.mockResolvedValue(jsonResponse(200, { native_max_cost_usd: 15, harness_max_cost_usd: 'unlimited' }));
		await expect(fetchBudgetDefaults()).resolves.toEqual({
			native_max_cost_usd: 15,
			harness_max_cost_usd: 'unlimited'
		});
		expect(fetchMock).toHaveBeenCalledWith('/api/magician/v2/budgets/defaults', expect.objectContaining({ method: 'GET' }));
	});

	it('explains a bare 404 as a server without run budgets', async () => {
		fetchMock.mockResolvedValue(new Response('', { status: 404 }));
		await expect(fetchBudgetDefaults()).rejects.toThrow(BUDGET_API_UNAVAILABLE);
	});

	it('surfaces the server message on an error', async () => {
		fetchMock.mockResolvedValue(jsonResponse(404, { error: 'task not found' }));
		await expect(grantTaskBudget('t1', { add_cost_usd: 25 })).rejects.toThrow('task not found');
		fetchMock.mockResolvedValue(jsonResponse(409, { message: 'no execution to budget' }));
		await expect(grantTaskBudget('t1', { add_cost_usd: 25 })).rejects.toThrow('no execution to budget');
	});

	it('says when the server cannot be reached', async () => {
		fetchMock.mockRejectedValue(new TypeError('Failed to fetch'));
		await expect(fetchBudgetDefaults()).rejects.toThrow(/could not be reached/);
	});

	it('POSTs the grant to the task and parses the effective budget', async () => {
		fetchMock.mockResolvedValue(
			jsonResponse(200, {
				execution_id: 'e1',
				cost_cap_usd: 125,
				cost_used_usd: 98.2,
				tokens_limit: null,
				tokens_used: 1085936,
				iterations_limit: 60,
				iterations_used: 11,
				resumed: true
			})
		);
		const budget = await grantTaskBudget('task/1', { add_cost_usd: 25 });
		const [url, init] = fetchMock.mock.calls[0];
		expect(url).toBe('/api/magician/v2/tasks/task%2F1/budget');
		expect(init.method).toBe('POST');
		expect(JSON.parse(init.body)).toEqual({ add_cost_usd: 25 });
		expect(describeEffectiveBudget(budget)).toBe('Cost $98.20 of $125 · 11 of 60 iterations · resumed');
	});

	it('describes an unlimited run', () => {
		expect(
			describeEffectiveBudget({
				cost_cap_usd: 'unlimited',
				cost_used_usd: 3,
				tokens_limit: null,
				tokens_used: null,
				iterations_limit: null,
				iterations_used: null,
				resumed: false
			})
		).toBe('Cost $3.00 so far, no cap');
	});
});

describe('runSlashAction', () => {
	it('records a time grant and reports total additional time without hiding cost', async () => {
		fetchMock.mockResolvedValue(jsonResponse(200, {
			cost_cap_usd: 100, cost_used_usd: 42, tokens_limit: null, tokens_used: 10,
			iterations_limit: 50, iterations_used: 3, duration_granted_secs: 172800, resumed: true
		}));
		const result = await runSlashAction({ type: 'task-budget', taskId: 't1',
			taskLabel: 'Long task', grant: { add_duration_secs: 86400 }, grantLabel: '+24 hours' });
		expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ add_duration_secs: 86400 });
		expect(result.message).toBe('Cost $42.00 of $100 · 3 of 50 iterations · +48 hours granted · resumed');
	});

	it('/engine on this device only changes the browser choice', async () => {
		const result = await runSlashAction({ type: 'chat-engine', engine: 'claude_code', model: 'opus', scope: 'device' });
		expect(select).toHaveBeenCalledWith('claude_code', 'opus');
		expect(setChatHarnessEngine).not.toHaveBeenCalled();
		expect(result.title).toBe('Chat engine: Claude Code · opus');
	});

	it('/engine everywhere writes the server default and adopts it', async () => {
		setChatHarnessEngine.mockResolvedValue({ chat_current: 'codex', chat_model: 'default' });
		const result = await runSlashAction({ type: 'chat-engine', engine: 'codex', model: 'default', scope: 'everywhere' });
		expect(setChatHarnessEngine).toHaveBeenCalledWith('codex', 'default');
		expect(adoptServer).toHaveBeenCalledWith('codex', 'default');
		expect(result.title).toBe('Chat engine everywhere: Codex');
	});

	it('/engine reaches the issuing chat panel and the chats that inherit the browser pick', async () => {
		const panel = { select: vi.fn(), adoptServer: vi.fn() };
		await runSlashAction({ type: 'chat-engine', engine: 'codex', model: 'default', scope: 'device' }, { chatHarness: panel });
		expect(select).toHaveBeenCalledWith('codex', 'default');
		expect(panel.select).toHaveBeenCalledWith('codex', 'default');
		expect(notifyDefault).toHaveBeenCalledWith({ engine: 'codex', model: 'default' });

		setChatHarnessEngine.mockResolvedValue({ chat_current: 'claude_code', chat_model: 'opus' });
		await runSlashAction({ type: 'chat-engine', engine: 'claude_code', model: 'opus', scope: 'everywhere' }, { chatHarness: panel });
		expect(adoptServer).toHaveBeenCalledWith('claude_code', 'opus');
		expect(panel.adoptServer).toHaveBeenCalledWith('claude_code', 'opus');
		expect(notifyDefault).toHaveBeenCalledWith({ engine: 'claude_code', model: 'opus' });
	});

	it('/run-engine sets the run engine and rethrows the server error', async () => {
		setRunHarnessEngine.mockResolvedValue({ engine: 'claude_code', harness_model: 'default', model_selection: null });
		await expect(runSlashAction({ type: 'run-engine', engine: 'claude_code', model: 'default' })).resolves.toMatchObject({
			title: 'Agentic flows: Claude Code'
		});
		expect(setRunHarnessEngine).toHaveBeenCalledWith('claude_code', 'default');
		setRunHarnessEngine.mockRejectedValue(new Error('engine not installed'));
		await expect(runSlashAction({ type: 'run-engine', engine: 'grok', model: 'default' })).rejects.toThrow('engine not installed');
	});

	it('/run-engine "Same as Chat" follows the chat engine', async () => {
		setRunHarnessEngine.mockResolvedValue({ engine: 'chat', harness_model: 'default', model_selection: null });
		const result = await runSlashAction({ type: 'run-engine', engine: 'chat', model: 'default' });
		expect(setRunHarnessEngine).toHaveBeenCalledWith('chat', 'default');
		expect(result.title).toBe('Agentic flows: Same as Chat');
	});

	it('/run-engine pi sends the model selection', async () => {
		const selection = { model_id: 'anthropic:claude-sonnet', thinking: 'low' };
		setRunHarnessEngine.mockResolvedValue({ engine: 'pi', harness_model: 'default', model_selection: selection });
		const result = await runSlashAction({ type: 'run-engine', engine: 'pi', model: 'default', selection, modelLabel: 'Claude Sonnet' });
		expect(setRunHarnessEngine).toHaveBeenCalledWith('pi', 'default', selection);
		expect(result.title).toBe('Agentic flows: Coding agent · Claude Sonnet');
	});

	it('/run-engine pi "Default" clears the run model with an explicit null', async () => {
		setRunHarnessEngine.mockResolvedValue({ engine: 'pi', harness_model: 'default', model_selection: null });
		const result = await runSlashAction({ type: 'run-engine', engine: 'pi', model: 'default', selection: null, modelLabel: 'Default' });
		expect(setRunHarnessEngine).toHaveBeenCalledWith('pi', 'default', null);
		expect(result.title).toBe('Agentic flows: Coding agent · Default');
	});

	it('/run-engine magician saves the agentic_decision model, pinning Magician first when needed', async () => {
		const selection = { model_id: 'openai:gpt-6.1-sol', thinking: 'high' };
		setRunHarnessEngine.mockResolvedValue({ engine: 'magician', harness_model: 'default', model_selection: null });
		setOperationModel.mockResolvedValue(undefined);
		const result = await runSlashAction({ type: 'run-loop-model', selection, modelLabel: 'GPT-6.1 Sol', pinMagician: true });
		expect(setRunHarnessEngine).toHaveBeenCalledWith('magician');
		expect(setOperationModel).toHaveBeenCalledWith('agentic_decision', selection);
		expect(setRunHarnessEngine.mock.invocationCallOrder[0]).toBeLessThan(setOperationModel.mock.invocationCallOrder[0]);
		expect(result.title).toBe('Agentic flows: Magician · GPT-6.1 Sol');

		setRunHarnessEngine.mockClear();
		await runSlashAction({ type: 'run-loop-model', selection, modelLabel: 'GPT-6.1 Sol', pinMagician: false });
		expect(setRunHarnessEngine).not.toHaveBeenCalled();
		setOperationModel.mockRejectedValue(new Error('Model assignment failed (409)'));
		await expect(
			runSlashAction({ type: 'run-loop-model', selection, modelLabel: 'GPT-6.1 Sol', pinMagician: false })
		).rejects.toThrow('Model assignment failed (409)');
	});

	it('/engine with a catalog pick sets the engine, then the model in the panel store', async () => {
		const selection = { model_id: 'anthropic:claude-sonnet', thinking: 'low' };
		const panel = { select: vi.fn(), adoptServer: vi.fn() };
		const profiles = panelProfiles(['anthropic:claude-sonnet']);
		const result = await runSlashAction(
			{ type: 'chat-engine', engine: 'pi', model: 'default', scope: 'device', selection, modelLabel: 'Claude Sonnet' },
			{ chatHarness: panel, chatProfiles: profiles.deps }
		);
		expect(panel.select).toHaveBeenCalledWith('pi', 'default');
		expect(profiles.store.ensureLoaded).toHaveBeenCalledWith('pi');
		expect(profiles.store.selectModel).toHaveBeenCalledWith(selection);
		expect(panel.select.mock.invocationCallOrder[0]).toBeLessThan(profiles.store.selectModel.mock.invocationCallOrder[0]);
		expect(result.title).toBe('Chat engine: Coding agent · Claude Sonnet');
	});

	it('/engine fails when the picked model is not in the loaded catalog', async () => {
		const profiles = panelProfiles([], 'Model choices could not be loaded. Reconnect and try again.');
		await expect(
			runSlashAction(
				{
					type: 'chat-engine',
					engine: 'magician',
					model: 'default',
					scope: 'device',
					selection: { model_id: 'gone', thinking: 'auto' },
					modelLabel: 'Gone'
				},
				{ chatProfiles: profiles.deps }
			)
		).rejects.toThrow('Model choices could not be loaded');
		expect(profiles.store.selectModel).not.toHaveBeenCalled();
	});

	it('/voice-mode saves where the voice switch saves', async () => {
		saveMediaPreferences.mockResolvedValue({});
		const result = await runSlashAction({ type: 'voice-mode', mode: 'hands_free' });
		expect(saveMediaPreferences).toHaveBeenCalledWith({ voice_mode: 'hands_free' });
		expect(result.title).toBe('Voice mode: Hands-free');
		saveMediaPreferences.mockRejectedValue(new Error('media preferences save 500'));
		await expect(runSlashAction({ type: 'voice-mode', mode: 'realtime' })).rejects.toThrow('media preferences save 500');
	});

	it('/budget default PUTs both caps when the current ones are known', async () => {
		fetchMock.mockResolvedValue(jsonResponse(200, { native_max_cost_usd: 15, harness_max_cost_usd: 250 }));
		const result = await runSlashAction(
			{ type: 'budget-default', field: 'harness_max_cost_usd', value: 250 },
			{ budgetDefaults: { native_max_cost_usd: 15, harness_max_cost_usd: 100 } }
		);
		const [url, init] = fetchMock.mock.calls[0];
		expect(url).toBe('/api/magician/v2/budgets/defaults');
		expect(init.method).toBe('PUT');
		expect(JSON.parse(init.body)).toEqual({ native_max_cost_usd: 15, harness_max_cost_usd: 250 });
		expect(result.title).toBe('Harness run cap: $250');
	});

	it('/budget default sends only the changed cap when the current ones are unknown', async () => {
		fetchMock.mockResolvedValue(jsonResponse(200, {}));
		await runSlashAction({ type: 'budget-default', field: 'native_max_cost_usd', value: 'unlimited' });
		expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ native_max_cost_usd: 'unlimited' });
	});

	it('/budget confirms with the effective budget', async () => {
		fetchMock.mockResolvedValue(
			jsonResponse(200, { cost_cap_usd: 'unlimited', cost_used_usd: 4.5, tokens_limit: null, tokens_used: 10, iterations_limit: 50, iterations_used: 3, resumed: true })
		);
		const result = await runSlashAction({
			type: 'task-budget',
			taskId: 't1',
			taskLabel: 'Research ACME',
			grant: { unlimited: true },
			grantLabel: 'Unlimited'
		});
		expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ unlimited: true });
		expect(result).toEqual({
			title: 'Unlimited for “Research ACME”',
			message: 'Cost $4.50 so far, no cap · 3 of 50 iterations · resumed'
		});
	});
});
