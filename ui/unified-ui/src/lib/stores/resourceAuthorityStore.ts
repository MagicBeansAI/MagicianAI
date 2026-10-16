// Resource Authority store — API integration + reactive state
// All data is fetched from the backend REST APIs; no hardcoded mock data.
//
// Types match the Rust API types in api_types.rs exactly.
import { writable, derived, get } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';

// ---------------------------------------------------------------------------
// Types (matching backend api_types.rs)
// ---------------------------------------------------------------------------

/** Matches `CeilingResponse` in Rust. */
export interface SystemCeiling {
	id: string;
	commodity: string;
	ceiling: number;
	relaxation: number;
	period: CeilingPeriod;
	carryover: CarryoverPolicy;
	period_start: string;
	spent_in_period: number;
	reserved_in_period: number;
	remaining_in_period: number;
}

export type CeilingPeriod = 'hourly' | 'daily' | 'weekly' | 'monthly' | 'quarterly' | 'annual' | 'total';

/** Matches Rust `CarryoverPolicy` enum — tagged with `"type"`, not `"kind"`. */
export type CarryoverPolicy =
	| { type: 'none' }
	| { type: 'full' }
	| { type: 'capped'; cap: number };

/** Matches `VelocityLimit` in Rust. */
export interface VelocityLimit {
	max_amount: number;
	window_seconds: number;
}

/** Matches `TokenSummaryResponse` in Rust. */
export interface SpendToken {
	id: string;
	issued_by: string;
	issued_to: string;
	commodity: string;
	ceiling: number;
	period: CeilingPeriod;
	carryover: CarryoverPolicy;
	period_start: string;
	spent_in_period: number;
	remaining_in_period: number;
	velocity_limit?: VelocityLimit;
	burn_rate_per_day: number;
	projected_exhaustion?: string;
	days_of_runway?: number;
	status: 'active' | 'revoked' | 'expired';
	expires_at?: string;
	conditions: string[];
	created_at: string;
}

/** Matches `TokenDetailResponse` in Rust — nested token + spend_history. */
export interface TokenDetailResponse {
	token: SpendToken;
	spend_history: JournalEntry[];
}

/** Matches `LedgerEntryResponse` in Rust (individual account line). */
export interface LedgerEntry {
	account: string;
	amount: number;
	commodity: string;
}

/** Matches `JournalEntryResponse` in Rust. */
export interface JournalEntry {
	id: string;
	timestamp: string;
	accrual_date: string;
	reference: string;
	agent_id: string;
	entries: LedgerEntry[];
	metadata: Record<string, string>;
}

/**
 * Flat transaction view derived from JournalEntry for UI rendering.
 * Each LedgerEntry within a JournalEntry becomes one FlatTransaction row.
 */
export interface FlatTransaction {
	id: string;
	timestamp: string;
	accrual_date: string;
	reference: string;
	agent_id: string;
	account: string;
	amount: number;
	commodity: string;
	metadata: Record<string, string>;
}

export interface ResourceAlert {
	id: string;
	severity: 'critical' | 'warning' | 'info';
	message: string;
	token_id?: string;
	reservation_id?: string;
	created_at: string;
}

/** Matches `FreezeStatusResponse` in Rust. */
export interface FreezeStatus {
	frozen: boolean;
	frozen_at?: string;
	frozen_by?: string;
	reason?: string;
}

/** Matches `AccountBalanceResponse` in Rust. */
export interface AccountBalance {
	account: string;
	commodity: string;
	balance: number;
}

/** Matches `AuditResponse` in Rust. */
export interface AuditResult {
	ok: boolean;
	message: string;
	imbalances?: AuditImbalance[];
}

/** Matches `AuditImbalanceResponse` in Rust. */
export interface AuditImbalance {
	commodity: string;
	expected: number;
	actual: number;
}

/** Matches `ReservationResponse` in Rust. */
export interface Reservation {
	id: string;
	token_id: string;
	commodity: string;
	amount: number;
	agent_id: string;
	created_at: string;
	max_duration_secs: number;
	is_stale: boolean;
}

/** Matches `PeriodCloseEntryResponse` in Rust. */
export interface PeriodCloseEntry {
	commodity: string;
	period_end: string;
	closed_at: string;
	closed_by: string;
}

/** Matches `LedgerQueryParams` in Rust. */
export interface TransactionFilter {
	commodity?: string;
	agent_id?: string;
	token_id?: string;
	since?: string;
	limit?: number;
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

export interface ResourceAuthorityState {
	ceilings: SystemCeiling[];
	tokens: SpendToken[];
	transactions: FlatTransaction[];
	alerts: ResourceAlert[];
	freezeStatus: FreezeStatus;
	reservations: Reservation[];
	loading: boolean;
	error: string | null;
}

const initialState: ResourceAuthorityState = {
	ceilings: [],
	tokens: [],
	transactions: [],
	alerts: [],
	freezeStatus: { frozen: false },
	reservations: [],
	loading: false,
	error: null
};

export const resourceAuthorityStore = writable<ResourceAuthorityState>(initialState);

// ---------------------------------------------------------------------------
// Derived stores
// ---------------------------------------------------------------------------

export const isFrozen = derived(resourceAuthorityStore, ($s) => $s.freezeStatus.frozen);
export const activeTokens = derived(resourceAuthorityStore, ($s) =>
	$s.tokens.filter((t) => t.status === 'active')
);
export const staleReservations = derived(resourceAuthorityStore, ($s) =>
	$s.reservations.filter((r) => r.is_stale)
);

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

const API_BASE = '/api/magician/v2/resource-authority';

// Operator-entered resource-authority API key (server's RESOURCE_AUTHORITY_API_KEY).
// SCOPED to this admin API only — never a global fetch interceptor (which would leak the
// key onto every request, including third parties). The key is a SERVER secret; storing it
// in the browser is acceptable only because this is a same-origin, operator-only admin
// dashboard that must NOT be publicly exposed.
const RESOURCE_AUTHORITY_KEY_STORAGE = 'resourceAuthorityApiKey';

function resourceAuthorityApiKey(): string | null {
	try {
		return typeof localStorage !== 'undefined'
			? localStorage.getItem(RESOURCE_AUTHORITY_KEY_STORAGE)
			: null;
	} catch {
		return null;
	}
}

/** Read the operator-entered resource-authority API key (empty if unset). */
export function getResourceAuthorityApiKey(): string {
	return resourceAuthorityApiKey() ?? '';
}

/** Persist (or clear, when empty) the operator-entered resource-authority API key. */
export function setResourceAuthorityApiKey(key: string): void {
	try {
		const trimmed = key.trim();
		if (trimmed) localStorage.setItem(RESOURCE_AUTHORITY_KEY_STORAGE, trimmed);
		else localStorage.removeItem(RESOURCE_AUTHORITY_KEY_STORAGE);
	} catch {
		/* ignore (SSR / storage disabled) */
	}
}

// Dev-only convenience: seed the operator key from the build-time env so local
// development doesn't require pasting it. This is DEV-gated AND the VITE_ var
// lives only in the gitignored .env.development (loaded only in dev mode), so
// the secret can never reach a production bundle or the public origin. Never
// overwrites an operator-entered key.
if (import.meta.env.DEV) {
	try {
		const seed = (import.meta.env as Record<string, string | undefined>)
			.VITE_RESOURCE_AUTHORITY_API_KEY;
		if (seed && !resourceAuthorityApiKey()) {
			setResourceAuthorityApiKey(seed);
		}
	} catch {
		/* ignore (SSR / storage disabled) */
	}
}

async function apiFetch<T>(path: string, options?: RequestInit): Promise<T> {
	const url = `${API_BASE}${path}`;
	const key = resourceAuthorityApiKey();
	const response = await timedFetch(url, {
		headers: {
			'Content-Type': 'application/json',
			...(key ? { Authorization: `Bearer ${key}` } : {}),
			...options?.headers
		},
		...options
	});
	if (!response.ok) {
		let message = `Request failed (${response.status})`;
		try {
			const body = await response.json() as Record<string, unknown>;
			if (typeof body.message === 'string') message = body.message;
			else if (typeof body.error === 'string') message = body.error;
		} catch { /* best effort */ }
		throw new Error(message);
	}
	if (response.status === 204) return undefined as unknown as T;
	return (await response.json()) as T;
}

// ---------------------------------------------------------------------------
// Transformers — flatten backend shapes for UI consumption
// ---------------------------------------------------------------------------

/** Flatten a JournalEntry (with nested entries) into one FlatTransaction per account line. */
function flattenJournalEntry(je: JournalEntry): FlatTransaction[] {
	return je.entries.map((le) => ({
		id: je.id,
		timestamp: je.timestamp,
		accrual_date: je.accrual_date,
		reference: je.reference,
		agent_id: je.agent_id,
		account: le.account,
		amount: le.amount,
		commodity: le.commodity,
		metadata: je.metadata
	}));
}

function flattenJournalEntries(entries: JournalEntry[]): FlatTransaction[] {
	return entries.flatMap(flattenJournalEntry);
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

export async function loadCeilings(): Promise<void> {
	resourceAuthorityStore.update((s) => ({ ...s, loading: true, error: null }));
	try {
		// Backend returns { ceilings: [...] }
		const resp = await apiFetch<{ ceilings: SystemCeiling[] }>('/ceilings');
		resourceAuthorityStore.update((s) => ({ ...s, ceilings: resp.ceilings, loading: false }));
	} catch (err) {
		resourceAuthorityStore.update((s) => ({
			...s,
			loading: false,
			error: err instanceof Error ? err.message : String(err)
		}));
	}
}

export async function saveCeiling(ceiling: {
	id?: string;
	commodity: string;
	ceiling: number;
	period: CeilingPeriod;
	carryover: CarryoverPolicy;
	relaxation: number;
}): Promise<void> {
	await apiFetch('/ceilings', {
		method: 'POST',
		body: JSON.stringify(ceiling)
	});
	await loadCeilings();
}

export async function deleteCeiling(id: string): Promise<void> {
	await apiFetch(`/ceilings/${encodeURIComponent(id)}`, { method: 'DELETE' });
	await loadCeilings();
}

export async function loadTokens(): Promise<void> {
	resourceAuthorityStore.update((s) => ({ ...s, loading: true, error: null }));
	try {
		// Backend returns { tokens: [...] }
		const resp = await apiFetch<{ tokens: SpendToken[] }>('/tokens');
		resourceAuthorityStore.update((s) => ({ ...s, tokens: resp.tokens, loading: false }));
	} catch (err) {
		resourceAuthorityStore.update((s) => ({
			...s,
			loading: false,
			error: err instanceof Error ? err.message : String(err)
		}));
	}
}

export async function loadTokenDetail(tokenId: string): Promise<TokenDetailResponse> {
	// Backend returns { token: {...}, spend_history: [...] }
	return apiFetch<TokenDetailResponse>(`/tokens/${encodeURIComponent(tokenId)}`);
}

export async function revokeToken(tokenId: string): Promise<void> {
	await apiFetch(`/tokens/${encodeURIComponent(tokenId)}/revoke`, { method: 'POST' });
	await loadTokens();
}

export async function bootstrapAgent(params: {
	agent_id: string;
	commodity: string;
	amount: number;
}): Promise<void> {
	await apiFetch('/bootstrap', { method: 'POST', body: JSON.stringify(params) });
	await loadDashboard();
}

export async function withdrawFunds(params: {
	agent_id: string;
	commodity: string;
	amount: number;
}): Promise<void> {
	await apiFetch('/withdraw', { method: 'POST', body: JSON.stringify(params) });
	await loadDashboard();
}

export async function issueRefund(params: {
	token_id: string;
	amount: number;
	reason: string;
}): Promise<void> {
	await apiFetch('/refund', { method: 'POST', body: JSON.stringify(params) });
	await loadDashboard();
}

export async function addVendorCredit(params: {
	agent_id: string;
	commodity: string;
	amount: number;
	reason: string;
}): Promise<void> {
	await apiFetch('/credit', { method: 'POST', body: JSON.stringify(params) });
	await loadDashboard();
}

export async function loadTransactions(filter?: TransactionFilter): Promise<FlatTransaction[]> {
	const params = new URLSearchParams();
	if (filter?.commodity) params.set('commodity', filter.commodity);
	if (filter?.agent_id) params.set('agent_id', filter.agent_id);
	if (filter?.token_id) params.set('token_id', filter.token_id);
	if (filter?.since) params.set('since', filter.since);
	if (filter?.limit != null) params.set('limit', String(filter.limit));
	const qs = params.toString();
	// Backend returns { entries: [...], total_count: N }
	const resp = await apiFetch<{ entries: JournalEntry[]; total_count: number }>(`/ledger${qs ? `?${qs}` : ''}`);
	return flattenJournalEntries(resp.entries);
}

export async function loadBalances(): Promise<AccountBalance[]> {
	// Backend returns { balances: [...] }
	const resp = await apiFetch<{ balances: AccountBalance[] }>('/ledger/balances');
	return resp.balances;
}

export async function loadAudit(): Promise<AuditResult> {
	// Backend returns { ok, message, imbalances? }
	return apiFetch<AuditResult>('/ledger/audit');
}

export async function loadReservations(): Promise<void> {
	try {
		// Backend returns { reservations: [...] }
		const resp = await apiFetch<{ reservations: Reservation[] }>('/reservations');
		resourceAuthorityStore.update((s) => ({ ...s, reservations: resp.reservations }));
	} catch (err) {
		resourceAuthorityStore.update((s) => ({
			...s,
			error: err instanceof Error ? err.message : String(err)
		}));
	}
}

export async function commitReservation(id: string): Promise<void> {
	await apiFetch(`/reservations/${encodeURIComponent(id)}/commit`, { method: 'POST' });
	await loadReservations();
}

export async function rollbackReservation(id: string): Promise<void> {
	await apiFetch(`/reservations/${encodeURIComponent(id)}/rollback`, { method: 'POST' });
	await loadReservations();
}

export async function flagReservation(id: string): Promise<void> {
	await apiFetch(`/reservations/${encodeURIComponent(id)}/flag`, { method: 'POST' });
	await loadReservations();
}

export async function freezeAll(reason: string): Promise<void> {
	await apiFetch('/freeze', { method: 'POST', body: JSON.stringify({ reason }) });
	await loadFreezeStatus();
}

export async function unfreeze(reason: string): Promise<void> {
	// Backend expects UnfreezeRequest { reason }
	await apiFetch('/unfreeze', { method: 'POST', body: JSON.stringify({ reason }) });
	await loadFreezeStatus();
}

export async function loadFreezeStatus(): Promise<void> {
	try {
		const freezeStatus = await apiFetch<FreezeStatus>('/freeze');
		resourceAuthorityStore.update((s) => ({ ...s, freezeStatus }));
	} catch (err) {
		resourceAuthorityStore.update((s) => ({
			...s,
			error: err instanceof Error ? err.message : String(err)
		}));
	}
}

export async function closePeriod(params: {
	commodity: string;
	period: CeilingPeriod;
	period_end: string;
}): Promise<void> {
	await apiFetch('/period-close', { method: 'POST', body: JSON.stringify(params) });
}

export async function loadPeriodCloses(): Promise<PeriodCloseEntry[]> {
	// Backend returns { period_closes: [...] }
	const resp = await apiFetch<{ period_closes: PeriodCloseEntry[] }>('/period-closes');
	return resp.period_closes;
}

// ---------------------------------------------------------------------------
// Composite loaders
// ---------------------------------------------------------------------------

export async function loadDashboard(): Promise<void> {
	resourceAuthorityStore.update((s) => ({ ...s, loading: true, error: null }));
	try {
		const [ceilingsResp, tokensResp, ledgerResp, freezeStatus, reservationsResp] = await Promise.all([
			apiFetch<{ ceilings: SystemCeiling[] }>('/ceilings'),
			apiFetch<{ tokens: SpendToken[] }>('/tokens'),
			apiFetch<{ entries: JournalEntry[]; total_count: number }>('/ledger'),
			apiFetch<FreezeStatus>('/freeze'),
			apiFetch<{ reservations: Reservation[] }>('/reservations')
		]);
		const ceilings = ceilingsResp.ceilings;
		const tokens = tokensResp.tokens;
		const transactions = flattenJournalEntries(ledgerResp.entries);
		const reservations = reservationsResp.reservations;
		// Derive alerts from data
		const alerts = deriveAlerts(tokens, reservations, ceilings);
		resourceAuthorityStore.update((s) => ({
			...s,
			ceilings,
			tokens,
			transactions,
			alerts,
			freezeStatus,
			reservations,
			loading: false
		}));
	} catch (err) {
		resourceAuthorityStore.update((s) => ({
			...s,
			loading: false,
			error: err instanceof Error ? err.message : String(err)
		}));
	}
}

// ---------------------------------------------------------------------------
// Alert derivation
// ---------------------------------------------------------------------------

function deriveAlerts(
	tokens: SpendToken[],
	reservations: Reservation[],
	_ceilings: SystemCeiling[]
): ResourceAlert[] {
	const alerts: ResourceAlert[] = [];
	const now = new Date().toISOString();

	for (const token of tokens) {
		if (token.status !== 'active') continue;
		const pct = token.ceiling > 0 ? (token.spent_in_period / token.ceiling) * 100 : 0;
		if (pct >= 80) {
			alerts.push({
				id: `burn-${token.id}`,
				severity: pct >= 95 ? 'critical' : 'warning',
				message: `${token.id} at ${Math.round(pct)}% spend`,
				token_id: token.id,
				created_at: now
			});
		}
	}

	for (const res of reservations) {
		if (res.is_stale) {
			alerts.push({
				id: `stale-${res.id}`,
				severity: 'warning',
				message: `Stale reservation: ${res.id}, ${formatAmount(res.amount, res.commodity)}`,
				reservation_id: res.id,
				created_at: now
			});
		}
	}

	return alerts;
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

export function formatAmount(value: number, commodity: string): string {
	if (commodity === 'USD') return `$${value.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
	return `${value.toLocaleString()} ${commodity}`;
}

export function formatPeriod(period: CeilingPeriod): string {
	const labels: Record<CeilingPeriod, string> = {
		hourly: 'Hourly',
		daily: 'Daily',
		weekly: 'Weekly',
		monthly: 'Monthly',
		quarterly: 'Quarterly',
		annual: 'Annual',
		total: 'Total'
	};
	return labels[period] || period;
}

export function formatCarryover(policy: CarryoverPolicy): string {
	switch (policy.type) {
		case 'none': return 'None';
		case 'full': return 'Full';
		case 'capped': return `Cap ${policy.cap != null ? `$${policy.cap.toLocaleString()}` : ''}`;
		default: return String((policy as { type: string }).type);
	}
}

export function usagePercent(usage: number, ceiling: number): number {
	if (ceiling <= 0) return 0;
	return Math.min(100, (usage / ceiling) * 100);
}

export function usageColor(pct: number): string {
	if (pct < 50) return 'var(--color-success, #5fa67a)';
	if (pct < 80) return '#d4a843';
	return 'var(--color-error, #e85d5d)';
}
