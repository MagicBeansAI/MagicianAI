/**
 * Consent surface for the account-based WEG connectors (email, calendar) — the
 * client half of `magician/src/magician_v2/api/observe_connectors_api.rs`. One
 * generic store parameterized by `producer` ('email' | 'calendar'); the
 * `/observe` page renders one card per producer over it. Nothing is captured
 * until the user enables a producer AND picks ≥1 connected account.
 */

export type ObserveProducer = 'email' | 'calendar';

export interface ObserveAccount {
	/** Alias the gmail/calendar skills take ('work' | 'personal' | 'business'). */
	name: string;
	/** Display email from operator-config (expected_email / email). */
	email: string;
	/** 'gmail' | 'agentmail' | 'calendar'. */
	account_type: string;
	/** OAuth/profile authenticated for this scope. */
	connected: boolean;
	/** 'user_assist' (the owner's own — "You") | 'envoy' (Presto's — "Presto"). */
	lane: 'user_assist' | 'envoy';
}

export interface ObserveConfig {
	enabled: boolean;
	accounts: string[];
	frequency: string; // 'daily' | 'twice-daily' | 'hourly'
	time: string; // 'HH:MM'
	suppress_sensitive: boolean;
	total_synced: number;
	last_sync_at: string | null;
	schedule_task_id: string | null;
}

export interface ObserveAccounts {
	email_accounts: ObserveAccount[];
	calendar_accounts: ObserveAccount[];
}

/** The accounts the cards can offer (authenticated; Presto's own identity excluded). */
export async function fetchObserveAccounts(): Promise<ObserveAccounts> {
	try {
		const res = await fetch('/api/magician/v2/observe/accounts');
		if (!res.ok) return { email_accounts: [], calendar_accounts: [] };
		const body = await res.json();
		return {
			email_accounts: Array.isArray(body?.email_accounts) ? body.email_accounts : [],
			calendar_accounts: Array.isArray(body?.calendar_accounts) ? body.calendar_accounts : []
		};
	} catch {
		return { email_accounts: [], calendar_accounts: [] };
	}
}

/** The persisted consent config for one producer (defaults to disabled). */
export async function fetchObserveStatus(producer: ObserveProducer): Promise<ObserveConfig | null> {
	try {
		const res = await fetch(`/api/magician/v2/observe/${producer}/status`);
		if (!res.ok) return null;
		return (await res.json()) as ObserveConfig;
	} catch {
		return null;
	}
}

export interface ObserveConfigUpdate {
	enabled: boolean;
	accounts: string[];
	frequency: string;
	time: string;
	suppress_sensitive: boolean;
}

/** Set consent/cadence. The backend intersects accounts with the connected ones
 *  and refuses to enable with none selected. Returns the saved config, or an
 *  `{ error }` shape the caller surfaces. */
export async function saveObserveConfig(
	producer: ObserveProducer,
	update: ObserveConfigUpdate
): Promise<{ ok: true; config: ObserveConfig } | { ok: false; error: string }> {
	try {
		const res = await fetch(`/api/magician/v2/observe/${producer}/config`, {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify(update)
		});
		const body = await res.json().catch(() => ({}));
		if (!res.ok) return { ok: false, error: body?.error || `HTTP ${res.status}` };
		return { ok: true, config: body as ObserveConfig };
	} catch (e) {
		return { ok: false, error: e instanceof Error ? e.message : String(e) };
	}
}
