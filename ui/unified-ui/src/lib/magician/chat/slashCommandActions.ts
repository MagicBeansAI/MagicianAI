// Runs what the composer's `/` picker chose (see composerCommands.ts) and the
// budget endpoints it calls. Every call is client-side: a slash command is
// never sent as a chat message. Contract: docs/plans/2026-10-03-slash-commands-
// and-run-budgets.md.

import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { chatHarnessPreferenceStore } from '$lib/stores/chatHarnessPreferenceStore';
import { setChatHarnessEngine, setRunHarnessEngine } from '$lib/plane/terminalGrants';
import {
	engineDisplayName,
	formatBudgetCap,
	type BudgetCap,
	type BudgetDefaults,
	type SlashAction,
	type TaskBudgetGrant
} from './composerCommands';

/** `POST|GET /tasks/{id}/budget` — the run's effective budget. */
export interface EffectiveBudget {
	execution_id?: string;
	cost_cap_usd: BudgetCap | null;
	cost_used_usd: number | null;
	tokens_limit: number | null;
	tokens_used: number | null;
	iterations_limit: number | null;
	iterations_used: number | null;
	duration_granted_secs?: number;
	resumed: boolean;
}

/** What a 404 means here: the server predates run budgets. */
export const BUDGET_API_UNAVAILABLE =
	'This Magician server does not offer run budgets yet — update the server and try again.';

function errorMessageFrom(body: unknown): string | null {
	if (body && typeof body === 'object') {
		const { message, error } = body as { message?: unknown; error?: unknown };
		if (typeof message === 'string' && message.trim()) return message.trim();
		if (typeof error === 'string' && error.trim()) return error.trim();
		if (error && typeof error === 'object') {
			const nested = (error as { message?: unknown }).message;
			if (typeof nested === 'string' && nested.trim()) return nested.trim();
		}
	}
	return null;
}

async function budgetRequest(path: string, init: RequestInit, fallback: string): Promise<unknown> {
	let response: Response;
	try {
		response = await fetch(path, {
			...init,
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				...(init.body ? { 'Content-Type': 'application/json' } : {})
			}),
			cache: 'no-store',
			redirect: 'error'
		});
	} catch {
		throw new Error(`${fallback} The server could not be reached.`);
	}
	const body = await response.json().catch(() => null);
	if (!response.ok) {
		const message = errorMessageFrom(body);
		// A 404 with a server message is a real answer (e.g. unknown task);
		// a bare 404 means the route itself is missing.
		if (response.status === 404 && !message) throw new Error(BUDGET_API_UNAVAILABLE);
		throw new Error(message ?? `${fallback} (HTTP ${response.status})`);
	}
	return body;
}

function parseCap(value: unknown): BudgetCap | null {
	if (value === 'unlimited') return 'unlimited';
	if (typeof value === 'number' && Number.isFinite(value)) return value;
	return null;
}

function parseNumber(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

export function parseBudgetDefaults(body: unknown): BudgetDefaults {
	const raw = (body ?? {}) as Record<string, unknown>;
	const native = parseCap(raw.native_max_cost_usd);
	const harness = parseCap(raw.harness_max_cost_usd);
	if (native === null || harness === null) {
		throw new Error('The server answered without budget defaults.');
	}
	return { native_max_cost_usd: native, harness_max_cost_usd: harness };
}

export async function fetchBudgetDefaults(): Promise<BudgetDefaults> {
	const body = await budgetRequest(
		'/api/magician/v2/budgets/defaults',
		{ method: 'GET' },
		'Budget defaults are unavailable.'
	);
	return parseBudgetDefaults(body);
}

/** Writes the live config. The contract's body carries both caps; when the
 *  current defaults are unknown only the changed one is sent. */
export async function saveBudgetDefaults(next: Partial<BudgetDefaults>): Promise<BudgetDefaults | null> {
	const body = await budgetRequest(
		'/api/magician/v2/budgets/defaults',
		{ method: 'PUT', body: JSON.stringify(next) },
		'The budget default could not be saved.'
	);
	try {
		return parseBudgetDefaults(body);
	} catch {
		return null;
	}
}

export function parseEffectiveBudget(body: unknown): EffectiveBudget {
	const raw = (body ?? {}) as Record<string, unknown>;
	return {
		execution_id: typeof raw.execution_id === 'string' ? raw.execution_id : undefined,
		cost_cap_usd: parseCap(raw.cost_cap_usd),
		cost_used_usd: parseNumber(raw.cost_used_usd),
		tokens_limit: parseNumber(raw.tokens_limit),
		tokens_used: parseNumber(raw.tokens_used),
		iterations_limit: parseNumber(raw.iterations_limit),
		iterations_used: parseNumber(raw.iterations_used),
		duration_granted_secs: parseNumber(raw.duration_granted_secs) ?? undefined,
		resumed: raw.resumed === true
	};
}

export async function grantTaskBudget(taskId: string, grant: TaskBudgetGrant): Promise<EffectiveBudget> {
	const body = await budgetRequest(
		`/api/magician/v2/tasks/${encodeURIComponent(taskId)}/budget`,
		{ method: 'POST', body: JSON.stringify(grant) },
		'The budget could not be added.'
	);
	return parseEffectiveBudget(body);
}

/** One line for the confirmation toast, e.g.
 *  `Cost $98.20 of $125 · 12 of 60 iterations · resumed`. */
export function describeEffectiveBudget(budget: EffectiveBudget): string {
	const parts: string[] = [];
	if (budget.cost_cap_usd !== null) {
		const used = budget.cost_used_usd !== null ? `$${budget.cost_used_usd.toFixed(2)}` : null;
		parts.push(
			budget.cost_cap_usd === 'unlimited'
				? `Cost ${used ? `${used} so far, ` : ''}no cap`
				: `Cost ${used ? `${used} of ` : ''}${formatBudgetCap(budget.cost_cap_usd)}`
		);
	}
	if (budget.tokens_limit !== null) {
		const used = budget.tokens_used !== null ? `${budget.tokens_used.toLocaleString('en-US')} of ` : '';
		parts.push(`${used}${budget.tokens_limit.toLocaleString('en-US')} tokens`);
	}
	if (budget.iterations_limit !== null) {
		const used = budget.iterations_used !== null ? `${budget.iterations_used} of ` : '';
		parts.push(`${used}${budget.iterations_limit} iterations`);
	}
	if (budget.duration_granted_secs) parts.push(`+${(budget.duration_granted_secs / 3600).toLocaleString('en-US', { maximumFractionDigits: 2 })} hours granted`);
	if (budget.resumed) parts.push('resumed');
	return parts.join(' · ');
}

export interface SlashConfirmation {
	title: string;
	message?: string;
}

export interface SlashActionDeps {
	/** Current defaults, so a PUT can carry both caps. */
	budgetDefaults?: BudgetDefaults | null;
}

function engineLabel(engine: string, model: string): string {
	return `${engineDisplayName(engine)}${model && model !== 'default' ? ` · ${model}` : ''}`;
}

/** Run a picked option. Resolves with the confirmation to show; rejects with
 *  the server's message. */
export async function runSlashAction(action: SlashAction, deps: SlashActionDeps = {}): Promise<SlashConfirmation> {
	switch (action.type) {
		case 'chat-engine': {
			if (action.scope === 'device') {
				chatHarnessPreferenceStore.select(action.engine, action.model);
				return {
					title: `Chat engine: ${engineLabel(action.engine, action.model)}`,
					message: 'For this browser. Other clients keep their own choice.'
				};
			}
			const saved = await setChatHarnessEngine(action.engine, action.model);
			chatHarnessPreferenceStore.adoptServer(saved.chat_current, saved.chat_model ?? action.model);
			return {
				title: `Chat engine: ${engineLabel(saved.chat_current, saved.chat_model ?? action.model)}`,
				message: 'Default for every client.'
			};
		}
		case 'run-engine': {
			const saved = await setRunHarnessEngine(action.engine, action.model);
			return {
				title: `Run engine: ${engineLabel(saved.engine, saved.harness_model)}`,
				message: 'New task runs use it.'
			};
		}
		case 'task-budget': {
			const budget = await grantTaskBudget(action.taskId, action.grant);
			const summary = describeEffectiveBudget(budget);
			return {
				title: `${action.grantLabel} for “${action.taskLabel}”`,
				message: summary || undefined
			};
		}
		case 'budget-default': {
			const next: Partial<BudgetDefaults> = deps.budgetDefaults
				? { ...deps.budgetDefaults, [action.field]: action.value }
				: { [action.field]: action.value };
			await saveBudgetDefaults(next);
			const which = action.field === 'harness_max_cost_usd' ? 'Harness' : 'Native';
			return {
				title: `${which} run cap: ${formatBudgetCap(action.value)}`,
				message: 'Default for new runs.'
			};
		}
	}
}
