/**
 * Channel Assist pipeline stats — client for `/channel-assist/stats` (aggregate
 * funnel + LLM usage) and `/channel-assist/sync/status` (per-account detail).
 * Powers the `/observe/stats` observability page.
 */

export interface ChannelAssistStatAccount {
	provider: string;
	account_alias: string;
	lane: 'user_assist' | 'envoy';
	connected: boolean;
	thread_count: number;
	message_count: number;
	last_synced_at: number | null;
	last_error: string | null;
}

export interface ChannelAssistSyncStatus {
	enabled: boolean;
	suppress_sensitive: boolean;
	history_lookback_days?: number;
	pending_distill: number;
	distill_queue?: ChannelDistillQueueCounts;
	pending_classify: number;
	workers?: ChannelAssistWorkerRuntime;
	accounts: ChannelAssistStatAccount[];
}

/** The profile/provider/model an op is CONFIGURED to use — resolved live from
 *  the operation router (never hardcoded in the UI). */
export interface ChannelAssistOpBinding {
	operation: string;
	bound: boolean;
	profile?: string | null;
	provider?: string | null;
	model?: string | null;
}

export interface ChannelAssistStats {
	totals: {
		threads: number;
		messages: number;
		by_provider: Record<string, number>;
		by_lane: Record<string, number>;
	};
	distill: {
		pending: number;
		queue?: ChannelDistillQueueCounts;
		done: number;
		suppressed: number;
		skipped: number;
		by_state: Record<string, number>;
		briefs?: ChannelBriefCoverage;
	};
	classify: {
		by_state: Record<string, number>;
		by_label: Record<string, number>;
		needs_approval: number;
		needs_approval_by_source_family?: Record<string, number>;
		retrying: number;
		failed: number;
	};
	funnel: { synced: number; distilled: number; classified: number; needs_approval: number };
	llm: { distill_calls: number; classify_calls: number; cost_usd: number; note: string };
	runtime?: ChannelAssistRuntime;
	/** Config-resolved LLM binding per op (distill/classify). */
	ops?: { distill: ChannelAssistOpBinding; classify: ChannelAssistOpBinding };
}

export interface ChannelDistillQueueCounts {
	history_floor_ms: number;
	pending: number;
	retryable: number;
	outside_history: number;
	expired: number;
	retry_exhausted: number;
}

/** A waiting queue is not proof that a worker is currently processing it. */
export function distillQueueView(stats: ChannelAssistStats | null, status: ChannelAssistSyncStatus | null) {
	const queue = stats?.distill.queue ?? status?.distill_queue;
	return {
		queued: queue ? queue.pending + queue.retryable : (status?.pending_distill ?? stats?.distill.pending ?? 0),
		outsideHistory: queue?.outside_history ?? 0,
		expired: stats?.distill.by_state?.expired ?? queue?.expired ?? 0,
		retryExhausted: queue?.retry_exhausted ?? 0
	};
}

/** Count successful summaries and mechanical skips/coalescing. Queue retirement
 * and history-window changes must never appear as distillation throughput. */
export function distillProcessedTotal(stats: ChannelAssistStats | null): number {
	return (stats?.distill.done ?? 0) + (stats?.distill.skipped ?? stats?.distill.by_state?.skipped ?? 0);
}

export interface ChannelAssistWorkerConfig {
	enabled: boolean;
	batch: number;
	concurrency: number;
	interval_secs: number;
	coalesce_threads?: boolean;
}

export interface ChannelBriefCoverage {
	done: number;
	v2: number;
	legacy: number;
	complete: number;
	partial: number;
	source_omits_details: number;
}

export interface ChannelDistillBackfillRuntime {
	paused: boolean;
	manual_requested: boolean;
	manual_requests: number;
	manual_completed: number;
	runs: number;
	selected: number;
	distilled: number;
	failed: number;
	yielded_pending: number;
	yielded_dispatch_pressure: number;
	last_run_at_ms: number | null;
}

export interface ChannelDistillBackfillStatus {
	enabled: boolean;
	lookback_days: number;
	batch_size: number;
	surfaced_first: boolean;
	backlog: { total: number; ready: number; cooling: number };
	metrics: ChannelDistillBackfillRuntime;
}

export interface ChannelAssistWorkerRuntime {
	distill: ChannelAssistWorkerConfig;
	classify: ChannelAssistWorkerConfig;
}

export interface ChannelAssistRuntime extends ChannelAssistWorkerRuntime {
	history_lookback_days: number;
	distill: ChannelAssistWorkerConfig & {
		brief_contract_version?: number;
		summary_max_chars?: number;
		backfill?: ChannelDistillBackfillStatus;
	};
}

/** Human label for a config-resolved op binding, e.g. `gemma4:26b-a4b-it-qat · ollama`.
 *  Falls back gracefully when a field or the whole binding is missing so the
 *  UI never shows a hardcoded model name. */
export function bindingLabel(binding: ChannelAssistOpBinding | undefined | null): string {
	if (!binding) return '—';
	if (!binding.bound && !binding.model) return 'not bound';
	const model = binding.model || 'unknown model';
	const provider = binding.provider || 'unknown provider';
	return `${model} · ${provider}`;
}

async function getJson<T>(path: string): Promise<T | null> {
	try {
		const res = await fetch(`/api/magician/v2${path}`);
		if (!res.ok) return null;
		return (await res.json()) as T;
	} catch {
		return null;
	}
}

export const fetchChannelAssistStats = () => getJson<ChannelAssistStats>('/channel-assist/stats');
export const fetchChannelAssistSyncStatus = () =>
	getJson<ChannelAssistSyncStatus>('/channel-assist/sync/status');

/** One entry in the live distillation feed — the message that was just
 *  distilled (input identity: subject/sender, metadata only) paired with the
 *  local model's derived output (summary/intent) + latency. */
export interface ChannelRecentDistillEntry {
	provider: string;
	account_alias: string;
	thread_id: string;
	message_id: string;
	subject?: string | null;
	from_name?: string | null;
	from_address?: string | null;
	summary: string;
	intent: string;
	/** When distillation completed (wall-clock ms). */
	at_ms: number;
	/** When the message ARRIVED (epoch ms) — the real received time. */
	received_at?: number | null;
	latency_ms?: number | null;
}

/** Local date + time, e.g. "Jul 6, 2:14 PM". Empty for null. */
export function localDateTime(ms: number | null | undefined): string {
	if (!ms) return '';
	try {
		return new Date(ms).toLocaleString(undefined, {
			month: 'short',
			day: 'numeric',
			hour: 'numeric',
			minute: '2-digit'
		});
	} catch {
		return '';
	}
}

/** Realtime input→output feed for distillation (in-memory ring on the backend,
 *  newest first). */
export async function fetchChannelRecentDistill(
	limit = 12
): Promise<ChannelRecentDistillEntry[]> {
	const res = await getJson<{ items: ChannelRecentDistillEntry[] }>(
		`/channel-assist/distill/recent?limit=${limit}`
	);
	return res?.items ?? [];
}

/** @deprecated use fetchChannelRecentDistill. */
export const fetchRecentDistill = fetchChannelRecentDistill;

/** "Alice Smith" ‹alice@x.com› → a compact sender label. */
export function senderLabel(e: ChannelRecentDistillEntry): string {
	const name = (e.from_name ?? '').trim();
	const addr = (e.from_address ?? '').trim();
	if (name && addr) return `${name} ‹${addr}›`;
	return name || addr || 'unknown sender';
}

/** 820ms / 1.4s / 34s — per-message distill latency. */
export function latencyLabel(ms: number | null | undefined): string {
	if (ms == null || ms <= 0) return '';
	if (ms < 1000) return `${Math.round(ms)}ms`;
	if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`;
	return `${Math.round(ms / 1000)}s`;
}

// ── Real per-operation LLM usage/cost from the analytics parquet lakehouse ──
// (the same `/analytics/llm_calls/query` SQL-over-parquet endpoint Today's
// Pulse and /llm use). This replaces the store-derived call counts + $0 with
// actual calls/tokens/priced cost tagged by operation — so a REMOTE classifier
// shows real cost, consistent with the rest of the system.

export const CHANNEL_ASSIST_OPS = ['channel_ingest_distill', 'channel_classify'] as const;

export interface ChannelAssistLlmOpUsage {
	operation: string;
	calls: number;
	cost_usd: number;
	input_tokens: number;
	output_tokens: number;
}

export interface ChannelAssistLlmUsage {
	by_op: Record<string, ChannelAssistLlmOpUsage>;
	total_calls: number;
	total_cost_usd: number;
	/** True when the telemetry query succeeded (even if 0 rows). */
	available: boolean;
}

/** Query real LLM usage for the channel-assist ops over the last 30 days. Numeric
 *  literals only in the SQL (no interpolated strings) — injection-safe. */
export async function fetchChannelAssistLlmUsage(): Promise<ChannelAssistLlmUsage> {
	const sinceMs = Date.now() - 30 * 86_400_000;
	const sql =
		`SELECT operation, COUNT(*) AS calls, COALESCE(SUM(cost_usd),0) AS cost_usd, ` +
		`COALESCE(SUM(input_tokens),0) AS input_tokens, COALESCE(SUM(output_tokens),0) AS output_tokens ` +
		`FROM llm_calls WHERE operation IN ('channel_ingest_distill','channel_classify') ` +
		`AND timestamp_ms >= ${sinceMs} GROUP BY operation`;
	const empty: ChannelAssistLlmUsage = {
		by_op: {},
		total_calls: 0,
		total_cost_usd: 0,
		available: false
	};
	try {
		const res = await fetch('/api/magician/v2/analytics/llm_calls/query', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ sql })
		});
		if (!res.ok) return empty;
		const payload = (await res.json()) as { columns?: string[]; rows?: unknown[][] };
		const cols = payload.columns ?? [];
		const rows = payload.rows ?? [];
		const at = (r: unknown[], name: string) => {
			const i = cols.indexOf(name);
			return i >= 0 ? Number(r[i]) || 0 : 0;
		};
		const opAt = (r: unknown[]) => {
			const i = cols.indexOf('operation');
			return i >= 0 ? String(r[i]) : '';
		};
		const by_op: Record<string, ChannelAssistLlmOpUsage> = {};
		let total_calls = 0;
		let total_cost_usd = 0;
		for (const r of rows) {
			const op = opAt(r);
			const u: ChannelAssistLlmOpUsage = {
				operation: op,
				calls: at(r, 'calls'),
				cost_usd: at(r, 'cost_usd'),
				input_tokens: at(r, 'input_tokens'),
				output_tokens: at(r, 'output_tokens')
			};
			by_op[op] = u;
			total_calls += u.calls;
			total_cost_usd += u.cost_usd;
		}
		return { by_op, total_calls, total_cost_usd, available: true };
	} catch {
		return empty;
	}
}

export function opLabel(operation: string): string {
	switch (operation) {
		case 'channel_ingest_distill':
			return 'Distill';
		case 'channel_classify':
			return 'Classify';
		default:
			return operation;
	}
}

/** Provider display label. */
export function providerLabel(provider: string): string {
	switch (provider) {
		case 'gmail':
			return 'Gmail';
		case 'agentmail':
			return 'AgentMail';
		case 'whatsapp':
			return 'WhatsApp';
		case 'whatsapp_kapso':
			return 'WhatsApp (Kapso)';
		default:
			return provider;
	}
}

export function laneLabel(lane: string): string {
	return lane === 'envoy' ? 'Presto' : 'You';
}

export function labelText(label: string): string {
	switch (label) {
		case 'needs_reply':
			return 'Needs reply';
		case 'follow_up':
			return 'Follow up';
		case 'fyi':
			return 'FYI';
		case 'no_action':
			return 'No action';
		case '(unlabeled)':
			return 'Unlabeled';
		default:
			return label;
	}
}

/** e.g. 4.2M · 12.3k · 340. */
export function compact(n: number): string {
	if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
	if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
	return String(n);
}

export function relativeTime(ms: number | null): string {
	if (!ms) return '—';
	const diff = Date.now() - ms;
	const mins = Math.floor(diff / 60000);
	if (mins < 1) return 'just now';
	if (mins < 60) return `${mins}m ago`;
	const hrs = Math.floor(mins / 60);
	if (hrs < 24) return `${hrs}h ago`;
	return `${Math.floor(hrs / 24)}d ago`;
}
