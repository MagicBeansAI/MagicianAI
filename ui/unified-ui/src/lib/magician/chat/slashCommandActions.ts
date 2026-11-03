// Runs what the composer's `/` picker chose (see composerCommands.ts) and the
// budget endpoints it calls. Every call is client-side: a slash command is
// never sent as a chat message. Contract: docs/plans/2026-10-03-slash-commands-
// and-run-budgets.md.

import { get } from 'svelte/store';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import {
	chatHarnessPreferenceStore,
	notifyChatHarnessDefaultChanged,
	type ChatHarnessPreferenceStore
} from '$lib/stores/chatHarnessPreferenceStore';
import { setChatHarnessEngine, setRunHarnessEngine } from '$lib/plane/terminalGrants';
import { setOperationModel } from '$lib/stores/modelRoutingStore';
import { saveMediaPreferences } from '$lib/media/preferences';
import type { ChatProfileStore } from '$lib/stores/chatProfileStore';
import {
	engineDisplayName,
	formatBudgetCap,
	voiceModeLabel,
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

export async function budgetRequest(path: string, init: RequestInit, fallback: string): Promise<unknown> {
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
	/** The issuing chat panel's own engine store. Its composer shows the
	 *  pick at once; the browser-wide store is updated as well. */
	chatHarness?: Pick<ChatHarnessPreferenceStore, 'select' | 'adoptServer'> | null;
	/** The issuing chat panel's model store (the one its dropdown writes). A
	 *  Magician/Pi pick from `/engine` lands there; without it only the
	 *  engine changes. */
	chatProfiles?: Pick<ChatProfileStore, 'subscribe' | 'ensureLoaded' | 'selectModel'> | null;
}

/** Magician's own loop decides with this Model routing operation; Settings →
 *  Agentic flows edits the same mapping (`EnginesPanel` `LOOP_OPERATION`). */
export const LOOP_OPERATION = 'agentic_decision';

function engineLabel(engine: string, model: string, modelLabel?: string): string {
	if (engine === 'chat') return 'Same as Chat';
	if (modelLabel) return `${engineDisplayName(engine)} · ${modelLabel}`;
	return `${engineDisplayName(engine)}${model && model !== 'default' ? ` · ${model}` : ''}`;
}

/** Run a picked option. Resolves with the confirmation to show; rejects with
 *  the server's message. */
export async function runSlashAction(action: SlashAction, deps: SlashActionDeps = {}): Promise<SlashConfirmation> {
	switch (action.type) {
		case 'chat-engine': {
			const composer = deps.chatHarness && deps.chatHarness !== chatHarnessPreferenceStore ? deps.chatHarness : null;
			if (action.scope === 'device') {
				// Check the model against that engine's catalog before writing
				// anything: a failed pick leaves the engine and model unchanged.
				const profiles = action.selection ? deps.chatProfiles : null;
				if (profiles && action.selection) {
					const selection = action.selection;
					await profiles.ensureLoaded(action.engine);
					if (!get(profiles).models.some((model) => model.id === selection.model_id)) {
						throw new Error(get(profiles).selectionError ?? 'That model is no longer offered. Choose another one.');
					}
				}
				chatHarnessPreferenceStore.select(action.engine, action.model);
				// Open chats that inherit the browser pick follow it; the chat
				// the command came from takes it as its own.
				notifyChatHarnessDefaultChanged({ engine: action.engine, model: action.model });
				composer?.select(action.engine, action.model);
				// As the composer dropdown does: the engine first, then the
				// model from that engine's catalog.
				if (profiles && action.selection) profiles.selectModel(action.selection);
				return {
					title: `Chat engine: ${engineLabel(action.engine, action.model, action.modelLabel)}`,
					message: 'For this browser. Other clients keep their own choice.'
				};
			}
			const saved = await setChatHarnessEngine(action.engine, action.model);
			const savedModel = saved.chat_model ?? action.model;
			chatHarnessPreferenceStore.adoptServer(saved.chat_current, savedModel);
			notifyChatHarnessDefaultChanged({ engine: saved.chat_current, model: savedModel });
			composer?.adoptServer(saved.chat_current, savedModel);
			return {
				title: `Chat engine everywhere: ${engineLabel(saved.chat_current, saved.chat_model ?? action.model)}`,
				message: 'Default for every client.'
			};
		}
		case 'run-engine': {
			// `selection: null` is sent as JSON null: it clears the coding
			// agent's run model. Absent, the body carries no `model_selection`.
			const saved = action.selection !== undefined
				? await setRunHarnessEngine(action.engine, action.model, action.selection)
				: await setRunHarnessEngine(action.engine, action.model);
			return {
				title: `Agentic flows: ${engineLabel(saved.engine, saved.harness_model, action.modelLabel)}`,
				message: 'New task runs use it.'
			};
		}
		case 'run-loop-model': {
			// Settings → Agentic flows: the loop model is the `agentic_decision`
			// mapping, which only steers runs that Magician drives.
			if (action.pinMagician) await setRunHarnessEngine('magician');
			await setOperationModel(LOOP_OPERATION, action.selection);
			return {
				title: `Agentic flows: Magician · ${action.modelLabel}`,
				message: action.pinMagician
					? 'New task runs think with Magician and decide with this model.'
					: 'New task runs decide with this model.'
			};
		}
		case 'voice-mode': {
			// Where the voice switch writes: the backend preference, applied to
			// `voiceModeStore` at once and rolled back if the save fails.
			await saveMediaPreferences({ voice_mode: action.mode });
			return {
				title: `Voice mode: ${voiceModeLabel(action.mode)}`,
				message: 'The voice button uses it.'
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
