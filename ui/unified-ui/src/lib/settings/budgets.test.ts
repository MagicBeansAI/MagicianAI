import { afterEach, describe, expect, it, vi } from 'vitest';
import { installFetchMock, jsonResponse, textResponse } from '../../test/browser';
import {
	actionLimitLabel,
	commodityLabel,
	describeWindow,
	fetchActionLimits,
	formatCap,
	formatLimitCeiling,
	parseActionLimits,
	parseBudgetDefaultsState,
	parseDollarInput,
	parseLimitInput,
	parsePercentInput,
	parseSpendEnvelope,
	periodLabel,
	removeActionLimit,
	saveActionLimit,
	sharedPerRunLimit,
	spendClassLabel
} from './budgets';

afterEach(() => vi.unstubAllGlobals());

describe('parseBudgetDefaultsState', () => {
	it('reads both caps and the env override', () => {
		const state = parseBudgetDefaultsState({ native_max_cost_usd: 100, harness_max_cost_usd: 'unlimited', native_env_override: true });
		expect(state).toEqual({ native_max_cost_usd: 100, harness_max_cost_usd: 'unlimited', native_env_override: true });
		expect(sharedPerRunLimit(state)).toBeNull();
		expect(sharedPerRunLimit({ ...state, harness_max_cost_usd: 100 })).toBe(100);
	});

	it('refuses a body without caps', () => {
		expect(() => parseBudgetDefaultsState({ native_max_cost_usd: 5 })).toThrow();
	});
});

describe('parseSpendEnvelope', () => {
	it('reads settings, status and a sorted per-class breakdown', () => {
		const state = parseSpendEnvelope({
			settings: { enabled: true, daily_usd: 250, total_usd: 'unlimited', window: 'daily', reset_offset_minutes: 0, warn_at_pct: 80, unpriced: 'estimate' },
			installed: true,
			status: {
				enabled: true,
				resets_at_ms: 1_000,
				daily_limit_usd: 250,
				granted_usd: 50,
				effective_limit_usd: 300,
				spent_usd: 120.5,
				remaining_usd: 179.5,
				used_pct: 40.2,
				warning: false,
				exhausted: false,
				exhausted_by: null,
				by_class: { memory: 0.5, autonomous_task: 120 }
			}
		});
		expect(state.settings.daily_usd).toBe(250);
		expect(state.status?.effective_limit_usd).toBe(300);
		expect(state.status?.by_class.map((row) => row.class)).toEqual(['autonomous_task', 'memory']);
	});

	it('keeps settings when no envelope runs in the process', () => {
		const state = parseSpendEnvelope({ settings: { enabled: false, daily_usd: 'unlimited', warn_at_pct: 90 }, installed: false, status: null });
		expect(state.installed).toBe(false);
		expect(state.status).toBeNull();
		expect(state.settings.daily_usd).toBe('unlimited');
		expect(state.settings.warn_at_pct).toBe(90);
	});
});

describe('input validation', () => {
	it('accepts plain dollar amounts and refuses the rest', () => {
		expect(parseDollarInput('100')).toEqual({ value: 100 });
		expect(parseDollarInput(' $12.50 ')).toEqual({ value: 12.5 });
		expect(parseDollarInput('1,000')).toEqual({ value: 1000 });
		for (const bad of ['', '0', '-5', 'abc', '1e3']) expect(parseDollarInput(bad)).toHaveProperty('error');
	});

	it('takes whole percentages from 1 to 100', () => {
		expect(parsePercentInput('80')).toEqual({ value: 80 });
		expect(parsePercentInput('80%')).toEqual({ value: 80 });
		for (const bad of ['0', '101', '7.5', '']) expect(parsePercentInput(bad)).toHaveProperty('error');
	});
});

describe('labels', () => {
	it('describes the window and its UTC roll time', () => {
		expect(describeWindow({ window: 'daily', reset_offset_minutes: 0 })).toBe('UTC day');
		expect(describeWindow({ window: 'daily', reset_offset_minutes: 420 })).toBe('day (rolls at 07:00 UTC)');
		expect(describeWindow({ window: 'daily', reset_offset_minutes: -60 })).toBe('day (rolls at 23:00 UTC)');
	});

	it('formats caps and class names', () => {
		expect(formatCap('unlimited')).toBe('Unlimited');
		expect(formatCap(250)).toBe('$250');
		expect(spendClassLabel('foreground_chat')).toBe('Foreground chat');
	});
});

const LIMITS_URL = '/api/magician/v2/budgets/limits';

const limitsBody = {
	enabled: true,
	limits: [
		{ scope: 'tool', id: 'agentmail-send', commodity: 'EMAIL_SENDS', ceiling: '50', period: 'daily' },
		{ scope: 'principal', id: 'shop-a', commodity: 'INR', ceiling: '2000', period: 'daily' }
	],
	applies: 'on_restart'
};

describe('parseActionLimits', () => {
	it('reads rows, the switch and when edits apply', () => {
		const state = parseActionLimits(limitsBody);
		expect(state.enabled).toBe(true);
		expect(state.applies).toBe('on_restart');
		expect(state.limits).toEqual(limitsBody.limits);
	});

	it('skips malformed rows and refuses a body without limits', () => {
		const state = parseActionLimits({
			enabled: false,
			limits: [{ scope: 'tool', id: 'x', commodity: 'EMAIL_SENDS', ceiling: 5, period: 'daily' }, { scope: 'planet', id: 'x', commodity: 'Y', ceiling: '1', period: 'daily' }],
			applies: 'on_restart'
		});
		expect(state.limits).toEqual([{ scope: 'tool', id: 'x', commodity: 'EMAIL_SENDS', ceiling: '5', period: 'daily' }]);
		expect(() => parseActionLimits({ enabled: true })).toThrow();
	});
});

describe('action limit requests', () => {
	it('lists the limits', async () => {
		const { calls } = installFetchMock([{ method: 'GET', match: LIMITS_URL, handle: () => jsonResponse(limitsBody) }]);
		const state = await fetchActionLimits();
		expect(state?.limits).toHaveLength(2);
		expect(calls).toHaveLength(1);
		expect(calls[0].url).toBe(LIMITS_URL);
		expect(calls[0].method).toBe('GET');
	});

	it('answers null on a server without action limits (bare 404)', async () => {
		installFetchMock([{ method: 'GET', match: LIMITS_URL, handle: () => textResponse('', { status: 404 }) }]);
		await expect(fetchActionLimits()).resolves.toBeNull();
	});

	it('surfaces other failures with the server message', async () => {
		installFetchMock([{ method: 'GET', match: LIMITS_URL, handle: () => jsonResponse({ error: 'action_limits_unavailable', message: 'Could not read the action limits.' }, { status: 500 }) }]);
		await expect(fetchActionLimits()).rejects.toThrow('Could not read the action limits.');
	});

	it('upserts a limit with a decimal-string ceiling', async () => {
		const { calls } = installFetchMock([{ method: 'PUT', match: LIMITS_URL, handle: () => jsonResponse(limitsBody) }]);
		await saveActionLimit({ scope: 'tool', id: 'agentmail-send', commodity: 'EMAIL_SENDS', ceiling: '75', period: 'weekly' });
		expect(calls[0].url).toBe(LIMITS_URL);
		expect(JSON.parse(String(calls[0].init?.body))).toEqual({ scope: 'tool', id: 'agentmail-send', commodity: 'EMAIL_SENDS', ceiling: '75', period: 'weekly' });
		expect(new Headers(calls[0].init?.headers).get('Content-Type')).toBe('application/json');
	});

	it('shows the server message when a limit is refused', async () => {
		installFetchMock([{ method: 'PUT', match: LIMITS_URL, handle: () => jsonResponse({ error: 'invalid_action_limit', message: 'name the limit\'s id and commodity' }, { status: 400 }) }]);
		await expect(saveActionLimit({ scope: 'tool', id: ' ', commodity: 'EMAIL_SENDS', ceiling: '5', period: 'daily' })).rejects.toThrow("name the limit's id and commodity");
	});

	it('removes a limit, URL-encoding each key part', async () => {
		const { calls } = installFetchMock([{ method: 'DELETE', match: LIMITS_URL, handle: () => jsonResponse({ ...limitsBody, limits: [] }) }]);
		const state = await removeActionLimit({ scope: 'principal', id: 'shop a/b&c', commodity: 'INR' });
		expect(state.limits).toEqual([]);
		expect(calls[0].method).toBe('DELETE');
		expect(calls[0].url).toBe(`${LIMITS_URL}?scope=principal&id=shop%20a%2Fb%26c&commodity=INR`);
		expect(calls[0].init?.body).toBeUndefined();
	});
});

describe('action limit labels and input', () => {
	it('names commodities and who a limit binds', () => {
		expect(actionLimitLabel({ scope: 'tool', id: 'agentmail-send', commodity: 'EMAIL_SENDS' })).toBe('Emails (agentmail-send)');
		expect(commodityLabel('WHATSAPP_SENDS')).toBe('WhatsApp sends');
		expect(commodityLabel('inr')).toBe('Spend in ₹');
		expect(commodityLabel('USD')).toBe('Spend in $');
		expect(commodityLabel('ORDERS')).toBe('ORDERS');
		expect(actionLimitLabel({ scope: 'agent', id: '*', commodity: 'USD' })).toBe('Spend in $ (all agents)');
		expect(actionLimitLabel({ scope: 'agent', id: 'scout', commodity: 'ORDERS' })).toBe('ORDERS (agent scout)');
	});

	it('formats ceilings with their unit and the period', () => {
		expect(formatLimitCeiling({ commodity: 'EMAIL_SENDS', ceiling: '50' })).toBe('50 emails');
		expect(formatLimitCeiling({ commodity: 'WHATSAPP_SENDS', ceiling: '2000' })).toBe('2,000 sends');
		expect(formatLimitCeiling({ commodity: 'INR', ceiling: '2000' })).toBe('₹2,000');
		expect(formatLimitCeiling({ commodity: 'USD', ceiling: '12.5' })).toBe('$12.50');
		expect(formatLimitCeiling({ commodity: 'ORDERS', ceiling: '10' })).toBe('10 ORDERS');
		expect(periodLabel('daily')).toBe('per day');
		expect(periodLabel('annual')).toBe('per year');
		expect(periodLabel('total')).toBe('in total');
	});

	it('accepts positive plain numbers, dropping $, ₹ and commas', () => {
		expect(parseLimitInput('50')).toEqual({ value: '50' });
		expect(parseLimitInput(' ₹2,000 ')).toEqual({ value: '2000' });
		expect(parseLimitInput('$12.50')).toEqual({ value: '12.50' });
		expect(parseLimitInput('007')).toEqual({ value: '7' });
		for (const bad of ['', '0', '0.00', '-5', 'abc', '1e3']) expect(parseLimitInput(bad)).toHaveProperty('error');
	});
});
