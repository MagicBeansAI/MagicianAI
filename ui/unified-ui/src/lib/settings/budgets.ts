// Settings → Spending: model spend — the per-run limit (`/budgets/defaults`)
// and the window spend limit (`/budgets/envelope`, `/budgets/envelope/grant`)
// — and resource authority's action limits (`/budgets/limits`). Owner or
// paired phone only. Requests go through the same helper as the `/budget`
// slash command.
import { writable } from 'svelte/store';
import { BUDGET_API_UNAVAILABLE, budgetRequest } from '$lib/magician/chat/slashCommandActions';
import type { BudgetCap } from '$lib/magician/chat/composerCommands';
import { createSharedPoll } from '$lib/stores/sharedPoll';

export type { BudgetCap };

export interface BudgetDefaultsState {
	native_max_cost_usd: BudgetCap;
	harness_max_cost_usd: BudgetCap;
	/** `MAGICIAN_AGENTIC_MAX_COST_USD` is set and overrides the native limit. */
	native_env_override: boolean;
}

export interface SpendEnvelopeSettings {
	enabled: boolean;
	daily_usd: BudgetCap;
	total_usd: BudgetCap;
	window: string;
	reset_offset_minutes: number;
	warn_at_pct: number;
	unpriced: string;
}

export interface SpendEnvelopeStatus {
	enabled: boolean;
	window: string;
	resets_at_ms: number | null;
	daily_limit_usd: number | null;
	granted_usd: number;
	effective_limit_usd: number | null;
	spent_usd: number;
	remaining_usd: number | null;
	used_pct: number | null;
	warning: boolean;
	exhausted: boolean;
	exhausted_by: string | null;
	by_class: Array<{ class: string; spent_usd: number }>;
}

export interface SpendEnvelopeState {
	settings: SpendEnvelopeSettings;
	/** False where no envelope runs in this process (no live status). */
	installed: boolean;
	status: SpendEnvelopeStatus | null;
}

const DEFAULTS = '/api/magician/v2/budgets/defaults';
const ENVELOPE = '/api/magician/v2/budgets/envelope';
const LIMITS = '/api/magician/v2/budgets/limits';

function cap(value: unknown): BudgetCap | null {
	if (value === 'unlimited') return 'unlimited';
	if (typeof value === 'number' && Number.isFinite(value)) return value;
	return null;
}

function num(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

export function parseBudgetDefaultsState(body: unknown): BudgetDefaultsState {
	const raw = (body ?? {}) as Record<string, unknown>;
	const native = cap(raw.native_max_cost_usd);
	const harness = cap(raw.harness_max_cost_usd);
	if (native === null || harness === null) throw new Error('The server answered without budget defaults.');
	return { native_max_cost_usd: native, harness_max_cost_usd: harness, native_env_override: raw.native_env_override === true };
}

export function parseSpendEnvelope(body: unknown): SpendEnvelopeState {
	const raw = (body ?? {}) as Record<string, unknown>;
	const s = (raw.settings ?? null) as Record<string, unknown> | null;
	if (!s || typeof s.enabled !== 'boolean') throw new Error('The server answered without spend limit settings.');
	const settings: SpendEnvelopeSettings = {
		enabled: s.enabled,
		daily_usd: cap(s.daily_usd) ?? 'unlimited',
		total_usd: cap(s.total_usd) ?? 'unlimited',
		window: typeof s.window === 'string' ? s.window : 'daily',
		reset_offset_minutes: num(s.reset_offset_minutes) ?? 0,
		warn_at_pct: num(s.warn_at_pct) ?? 80,
		unpriced: typeof s.unpriced === 'string' ? s.unpriced : ''
	};
	const st = (raw.status ?? null) as Record<string, unknown> | null;
	let status: SpendEnvelopeStatus | null = null;
	if (st && typeof st === 'object') {
		const byClass = (st.by_class ?? {}) as Record<string, unknown>;
		status = {
			enabled: st.enabled === true,
			window: typeof st.window === 'string' ? st.window : settings.window,
			resets_at_ms: num(st.resets_at_ms),
			daily_limit_usd: num(st.daily_limit_usd),
			granted_usd: num(st.granted_usd) ?? 0,
			effective_limit_usd: num(st.effective_limit_usd),
			spent_usd: num(st.spent_usd) ?? 0,
			remaining_usd: num(st.remaining_usd),
			used_pct: num(st.used_pct),
			warning: st.warning === true,
			exhausted: st.exhausted === true,
			exhausted_by: typeof st.exhausted_by === 'string' ? st.exhausted_by : null,
			by_class: Object.entries(byClass)
				.map(([klass, value]) => ({ class: klass, spent_usd: num(value) ?? 0 }))
				.filter((row) => row.spent_usd > 0)
				.sort((a, b) => b.spent_usd - a.spent_usd)
		};
	}
	return { settings, installed: raw.installed === true, status };
}

export async function fetchBudgetDefaultsState(): Promise<BudgetDefaultsState> {
	return parseBudgetDefaultsState(await budgetRequest(DEFAULTS, { method: 'GET' }, 'Budget defaults are unavailable.'));
}

/** Partial: a missing field stays as it is. */
export async function saveBudgetDefaultsState(
	update: Partial<Pick<BudgetDefaultsState, 'native_max_cost_usd' | 'harness_max_cost_usd'>>
): Promise<BudgetDefaultsState> {
	return parseBudgetDefaultsState(
		await budgetRequest(DEFAULTS, { method: 'PUT', body: JSON.stringify(update) }, 'The per-run limit could not be saved.')
	);
}

export async function fetchSpendEnvelope(): Promise<SpendEnvelopeState> {
	return parseSpendEnvelope(await budgetRequest(ENVELOPE, { method: 'GET' }, 'The daily spend limit is unavailable.'));
}

/** Partial: a missing field stays as it is. */
export async function saveSpendEnvelope(
	update: Partial<Pick<SpendEnvelopeSettings, 'enabled' | 'daily_usd' | 'warn_at_pct'>>
): Promise<SpendEnvelopeState> {
	return parseSpendEnvelope(
		await budgetRequest(ENVELOPE, { method: 'PUT', body: JSON.stringify(update) }, 'The daily spend limit could not be saved.')
	);
}

/** More money for the current window only; never unlimited. */
export async function grantSpendEnvelope(addCostUsd: number, note?: string): Promise<SpendEnvelopeState> {
	const body: Record<string, unknown> = { add_cost_usd: addCostUsd };
	if (note?.trim()) body.note = note.trim();
	return parseSpendEnvelope(
		await budgetRequest(`${ENVELOPE}/grant`, { method: 'POST', body: JSON.stringify(body) }, 'The top-up could not be added.')
	);
}

/** The last envelope fetch error, for the panel (the poll keeps the last good value). */
export const spendEnvelopeError = writable<string | null>(null);

/** Today's spend refreshes while the Spending panel is open. */
export const spendEnvelopePoll = createSharedPoll<SpendEnvelopeState>({
	fetcher: async () => {
		try {
			const state = await fetchSpendEnvelope();
			spendEnvelopeError.set(null);
			return state;
		} catch (caught) {
			spendEnvelopeError.set(caught instanceof Error ? caught.message : 'The daily spend limit is unavailable.');
			throw caught;
		}
	},
	idleMs: 60_000,
	fastMs: 15_000
});

/** A dollar amount typed by the owner: positive, finite, at most two decimals' worth. */
export function parseDollarInput(text: string): { value: number } | { error: string } {
	const trimmed = text.trim().replace(/^\$/, '').replace(/,/g, '');
	if (!trimmed) return { error: 'Enter an amount in dollars.' };
	if (!/^\d+(\.\d+)?$/.test(trimmed)) return { error: 'Use a plain dollar amount, like 100 or 12.50.' };
	const value = Number(trimmed);
	if (!Number.isFinite(value) || value <= 0) return { error: 'The amount must be more than $0.' };
	if (value > 1_000_000) return { error: 'The amount must be at most $1,000,000.' };
	return { value };
}

/** A whole percentage from 1 to 100. */
export function parsePercentInput(text: string): { value: number } | { error: string } {
	const trimmed = text.trim().replace(/%$/, '');
	if (!/^\d+$/.test(trimmed)) return { error: 'Use a whole percentage from 1 to 100.' };
	const value = Number(trimmed);
	if (value < 1 || value > 100) return { error: 'Use a whole percentage from 1 to 100.' };
	return { value };
}

/** One per-run limit when both run kinds share it; `null` when they differ. */
export function sharedPerRunLimit(defaults: BudgetDefaultsState): BudgetCap | null {
	return defaults.native_max_cost_usd === defaults.harness_max_cost_usd ? defaults.native_max_cost_usd : null;
}

export function formatUsd(amount: number): string {
	if (Number.isInteger(amount)) return `$${amount.toLocaleString('en-US')}`;
	return `$${amount.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

export function formatCap(value: BudgetCap): string {
	return value === 'unlimited' ? 'Unlimited' : formatUsd(value);
}

/** The text a cap shows in an input: blank for unlimited. */
export function capInputText(value: BudgetCap): string {
	return value === 'unlimited' ? '' : String(value);
}

/** "foreground_chat" → "Foreground chat". */
export function spendClassLabel(klass: string): string {
	const words = klass.replace(/_/g, ' ').trim();
	return words ? words[0].toUpperCase() + words.slice(1) : klass;
}

/** "in 3 h 20 min (05:30 local)". */
export function describeReset(resetsAtMs: number | null, nowMs = Date.now()): string {
	if (resetsAtMs === null) return 'unknown';
	const minutes = Math.max(0, Math.round((resetsAtMs - nowMs) / 60_000));
	const hours = Math.floor(minutes / 60);
	const rest = minutes % 60;
	const span = hours > 0 ? `${hours} h ${rest} min` : `${rest} min`;
	const clock = new Date(resetsAtMs).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	return `in ${span} (${clock} local)`;
}

/** "UTC day", "day (rolls at 07:00 UTC)", "UTC week", … (`reset_offset_minutes`
 *  is added to the UTC boundary: 420 rolls a day at 07:00 UTC). */
export function describeWindow(settings: Pick<SpendEnvelopeSettings, 'window' | 'reset_offset_minutes'>): string {
	const unit = ({ hourly: 'hour', daily: 'day', weekly: 'week', monthly: 'month' } as Record<string, string>)[settings.window] ?? settings.window;
	if (settings.window === 'hourly') return unit;
	const offset = settings.reset_offset_minutes;
	if (!offset) return `UTC ${unit}`;
	const minutes = ((offset % 1440) + 1440) % 1440;
	const hh = String(Math.floor(minutes / 60)).padStart(2, '0');
	const mm = String(minutes % 60).padStart(2, '0');
	return `${unit} (rolls at ${hh}:${mm} UTC)`;
}


// ---------------------------------------------------------------------------
// Action limits (`resource_authority.budgets`)
// ---------------------------------------------------------------------------

export type ActionLimitScope = 'principal' | 'agent' | 'tool';
export type ActionLimitPeriod = 'total' | 'hourly' | 'daily' | 'weekly' | 'monthly' | 'quarterly' | 'annual';

export const ACTION_LIMIT_SCOPES: ActionLimitScope[] = ['tool', 'agent', 'principal'];
export const ACTION_LIMIT_PERIODS: ActionLimitPeriod[] = ['hourly', 'daily', 'weekly', 'monthly', 'quarterly', 'annual', 'total'];

export interface ActionLimit {
	scope: ActionLimitScope;
	id: string;
	commodity: string;
	/** A decimal string, as the server keeps it ("50", "2000.5"). */
	ceiling: string;
	period: ActionLimitPeriod;
}

export type ActionLimitKey = Pick<ActionLimit, 'scope' | 'id' | 'commodity'>;

export interface ActionLimitsState {
	/** Resource authority's own switch: off means nothing is enforced. */
	enabled: boolean;
	limits: ActionLimit[];
	/** "on_restart": the dispatch gate reads its rows at boot. */
	applies: string;
}

function isScope(value: unknown): value is ActionLimitScope {
	return typeof value === 'string' && (ACTION_LIMIT_SCOPES as string[]).includes(value);
}

function isPeriod(value: unknown): value is ActionLimitPeriod {
	return typeof value === 'string' && (ACTION_LIMIT_PERIODS as string[]).includes(value);
}

export function parseActionLimits(body: unknown): ActionLimitsState {
	const raw = (body ?? {}) as Record<string, unknown>;
	if (!Array.isArray(raw.limits)) throw new Error('The server answered without action limits.');
	const limits: ActionLimit[] = [];
	for (const entry of raw.limits) {
		const row = (entry ?? {}) as Record<string, unknown>;
		const ceiling = typeof row.ceiling === 'number' ? String(row.ceiling) : row.ceiling;
		if (!isScope(row.scope) || !isPeriod(row.period)) continue;
		if (typeof row.id !== 'string' || typeof row.commodity !== 'string' || typeof ceiling !== 'string') continue;
		limits.push({ scope: row.scope, id: row.id, commodity: row.commodity, ceiling, period: row.period });
	}
	return { enabled: raw.enabled === true, limits, applies: typeof raw.applies === 'string' ? raw.applies : '' };
}

/** Null on a server that predates action limits (a bare 404). */
export async function fetchActionLimits(): Promise<ActionLimitsState | null> {
	try {
		return parseActionLimits(await budgetRequest(LIMITS, { method: 'GET' }, 'Action limits are unavailable.'));
	} catch (caught) {
		if (caught instanceof Error && caught.message === BUDGET_API_UNAVAILABLE) return null;
		throw caught;
	}
}

/** Adds a limit, or changes an existing one's ceiling and period. */
export async function saveActionLimit(limit: ActionLimit): Promise<ActionLimitsState> {
	const body = { scope: limit.scope, id: limit.id, commodity: limit.commodity, ceiling: limit.ceiling, period: limit.period };
	return parseActionLimits(
		await budgetRequest(LIMITS, { method: 'PUT', body: JSON.stringify(body) }, 'The action limit could not be saved.')
	);
}

export async function removeActionLimit(key: ActionLimitKey): Promise<ActionLimitsState> {
	const query = `scope=${encodeURIComponent(key.scope)}&id=${encodeURIComponent(key.id)}&commodity=${encodeURIComponent(key.commodity)}`;
	return parseActionLimits(
		await budgetRequest(`${LIMITS}?${query}`, { method: 'DELETE' }, 'The action limit could not be removed.')
	);
}

/** One row's identity, for keyed lists and drafts. */
export function actionLimitKey(key: ActionLimitKey): string {
	return `${key.scope}\u0000${key.id}\u0000${key.commodity.trim().toUpperCase()}`;
}

/** A ceiling typed by the owner: positive, plain decimal; `$`, `₹` and commas
 *  are dropped. The value is the decimal string sent to the server. */
export function parseLimitInput(text: string): { value: string } | { error: string } {
	const trimmed = text.trim().replace(/^[$₹]/, '').replace(/,/g, '').trim();
	if (!trimmed) return { error: 'Enter a limit.' };
	if (!/^\d+(\.\d+)?$/.test(trimmed)) return { error: 'Use a plain number, like 50 or 2000.' };
	if (!(Number(trimmed) > 0)) return { error: 'The limit must be more than 0.' };
	return { value: trimmed.replace(/^0+(?=\d)/, '') };
}

const CURRENCY_SYMBOLS: Record<string, string> = { USD: '$', INR: '₹' };

const COMMODITY_NAMES: Record<string, { name: string; unit: string }> = {
	EMAIL_SENDS: { name: 'Emails', unit: 'emails' },
	WHATSAPP_SENDS: { name: 'WhatsApp sends', unit: 'sends' }
};

const PERIOD_LABELS: Record<ActionLimitPeriod, string> = {
	total: 'in total',
	hourly: 'per hour',
	daily: 'per day',
	weekly: 'per week',
	monthly: 'per month',
	quarterly: 'per quarter',
	annual: 'per year'
};

/** "per day", "in total", … */
export function periodLabel(period: ActionLimitPeriod): string {
	return PERIOD_LABELS[period];
}

/** "Emails", "WhatsApp sends", "Spend in ₹"; an unknown commodity is its name. */
export function commodityLabel(commodity: string): string {
	const canonical = commodity.trim().toUpperCase();
	const symbol = CURRENCY_SYMBOLS[canonical];
	if (symbol) return `Spend in ${symbol}`;
	return COMMODITY_NAMES[canonical]?.name ?? commodity;
}

/** Who the limit binds: the tool's id, "all agents" for `agent *`, … */
function limitSubject(key: Pick<ActionLimit, 'scope' | 'id'>): string {
	const id = key.id.trim();
	if (id === '*') return key.scope === 'principal' ? 'everyone' : `all ${key.scope}s`;
	return key.scope === 'agent' ? `agent ${id}` : id;
}

/** "Emails (agentmail-send)", "Spend in ₹ (shop-a)". */
export function actionLimitLabel(limit: ActionLimitKey): string {
	return `${commodityLabel(limit.commodity)} (${limitSubject(limit)})`;
}

/** "50 emails", "₹2,000", "$12.50", "10 ORDERS". */
export function formatLimitCeiling(limit: Pick<ActionLimit, 'commodity' | 'ceiling'>): string {
	const canonical = limit.commodity.trim().toUpperCase();
	const amount = Number(limit.ceiling);
	const symbol = CURRENCY_SYMBOLS[canonical];
	if (!Number.isFinite(amount)) return `${limit.ceiling} ${limit.commodity}`;
	if (symbol) {
		const locale = canonical === 'INR' ? 'en-IN' : 'en-US';
		const digits = Number.isInteger(amount) ? 0 : 2;
		return `${symbol}${amount.toLocaleString(locale, { minimumFractionDigits: digits, maximumFractionDigits: Math.max(digits, 6) })}`;
	}
	const count = amount.toLocaleString('en-US', { maximumFractionDigits: 6 });
	return `${count} ${COMMODITY_NAMES[canonical]?.unit ?? limit.commodity}`;
}
