export type ObservationCadence = 'hourly' | 'twice_daily' | 'daily';
export type ObservableReadiness = 'eligible' | 'needs_setup' | 'unavailable';
export type ObservationSubscriptionState = 'enabled' | 'paused' | 'backoff';

export interface ObservationProfileLimits {
	max_candidates_per_run: number;
	max_selected_per_run: number;
}

export interface ObservableSourceOffer {
	offer_id: string;
	source_id: string;
	source_revision: string;
	profile_id: string;
	profile_revision: string;
	display_name: string;
	category: string;
	description: string;
	readiness: ObservableReadiness;
	unavailable_reason?: string | null;
	subscribed: boolean;
	supported_cadence: ObservationCadence[];
	default_cadence: ObservationCadence;
	limits: ObservationProfileLimits;
	targets: string[];
	action_bindings: Array<{ action_id: string; adapter_id: string }>;
}

export interface ObservableSourceOfferPage {
	items: ObservableSourceOffer[];
	total: number;
	next_cursor?: string | null;
	catalog_revision: string;
	manifest_issues: Array<{ source_id: string; error_class: string }>;
}

export interface ObservationSubscription {
	subscription_id: string;
	source_id: string;
	source_revision: string;
	profile_id: string;
	profile_revision: string;
	display_name: string;
	category: string;
	enabled: boolean;
	custom: boolean;
	action_id: string;
	cadence: ObservationCadence;
	supported_cadence: ObservationCadence[];
	next_run_at_ms: number;
	intent?: string | null;
	max_candidates_per_run: number;
	max_selected_per_run: number;
	targets: string[];
	last_run_started_at_ms?: number | null;
	last_success_at_ms?: number | null;
	consecutive_failures: number;
	last_error_class?: string | null;
	revision: number;
}

export interface ObservationSubscriptionPage {
	items: ObservationSubscription[];
	total: number;
	next_cursor?: string | null;
}

export type ObservationRunTrigger = 'scheduled' | 'manual';
export type ObservationRunStatus = 'succeeded' | 'failed' | 'cancelled';

export interface ObservationRunRecord {
	schema_version: number;
	run_id: string;
	subscription_id: string;
	source_id: string;
	profile_id: string;
	action_id: string;
	trigger: ObservationRunTrigger;
	status: ObservationRunStatus;
	started_at_ms: number;
	finished_at_ms: number;
	duration_ms: number;
	discovered: number;
	deduped: number;
	selected: number;
	handed_off: number;
	cursor_advanced: boolean;
	modified_targets: number;
	not_modified_targets: number;
	response_bytes: number;
	error_class?: string | null;
	cost?: { commodity: string; amount_microunits: number } | null;
}

export interface ObservationSourceObservabilitySummary {
	subscription_id: string;
	source_id: string;
	profile_id: string;
	display_name: string;
	category: string;
	action_id: string;
	enabled: boolean;
	cadence: ObservationCadence;
	next_run_at_ms: number;
	consecutive_failures: number;
	last_error_class?: string | null;
	runs: number;
	succeeded: number;
	failed: number;
	cancelled: number;
	candidates_discovered: number;
	candidates_deduped: number;
	candidates_selected: number;
	enrichment_handoffs: number;
	enrichment_processed: number;
	enrichment_failed: number;
	cursor_advances: number;
	modified_targets: number;
	not_modified_targets: number;
	response_bytes: number;
	total_latency_ms: number;
	cost_microunits: Record<string, number>;
	last_run?: ObservationRunRecord | null;
}

export interface ObservationObservabilityTotals {
	subscriptions: number;
	enabled: number;
	healthy: number;
	degraded: number;
	never_run: number;
	runs: number;
	succeeded: number;
	failed: number;
	cancelled: number;
	candidates_discovered: number;
	candidates_deduped: number;
	candidates_selected: number;
	enrichment_handoffs: number;
	enrichment_processed: number;
	enrichment_failed: number;
	modified_targets: number;
	not_modified_targets: number;
	response_bytes: number;
	total_latency_ms: number;
	cost_microunits: Record<string, number>;
}

export interface ObservableSourceRuntimeMetrics {
	offer_projections: number;
	offers_eligible: number;
	offers_needs_setup: number;
	offers_unavailable: number;
	runs_started: number;
	runs_succeeded: number;
	runs_failed: number;
	runs_throttled: number;
	leases_skipped: number;
	policy_denials: number;
	candidates_discovered: number;
	candidates_deduped: number;
	candidates_selected: number;
	enrichment_handoffs: number;
	enrichment_processed: number;
	enrichment_failed: number;
	cursor_advances: number;
	modified_targets: number;
	not_modified_targets: number;
	response_bytes: number;
	cost_microunits: Record<string, number>;
	total_latency_ms: number;
}

export interface ObservationSourceObservabilityPage {
	items: ObservationSourceObservabilitySummary[];
	total: number;
	next_cursor?: string | null;
	totals: ObservationObservabilityTotals;
	handoff_backlog: number;
	failed_handoffs: number;
	run_history_retained: number;
	runtime_metrics: ObservableSourceRuntimeMetrics;
}

export interface ObservationRunRecordPage {
	items: ObservationRunRecord[];
	total: number;
	next_cursor?: string | null;
}

export interface PutObservationSubscription {
	source_id?: string;
	profile_id?: string;
	source_revision?: string;
	expected_revision?: number;
	enabled: boolean;
	cadence: ObservationCadence;
	intent?: string | null;
	max_candidates_per_run?: number;
	max_selected_per_run?: number;
	custom_rss?: { display_name: string; feed_url: string };
}

function queryString(values: Record<string, string | number | boolean | null | undefined>): string {
	const params = new URLSearchParams();
	for (const [key, value] of Object.entries(values)) {
		if (value !== undefined && value !== null && value !== '') params.set(key, String(value));
	}
	const encoded = params.toString();
	return encoded ? `?${encoded}` : '';
}

async function responseError(response: Response, fallback: string): Promise<Error> {
	const body = await response.json().catch(() => null);
	const error = new Error(body?.message ?? body?.error ?? fallback);
	Object.assign(error, { code: body?.error, status: response.status });
	return error;
}

export async function fetchObservableSources(options: {
	readiness?: ObservableReadiness;
	requiredAction?: string;
	subscribed?: boolean;
	cursor?: string | null;
	limit?: number;
} = {}): Promise<ObservableSourceOfferPage> {
	const response = await fetch(
		`/api/magician/v2/observe/sources${queryString({
			readiness: options.readiness,
			required_action: options.requiredAction,
			subscribed: options.subscribed,
			cursor: options.cursor,
			limit: options.limit ?? 5
		})}`
	);
	if (!response.ok) throw await responseError(response, `Sources failed (${response.status})`);
	return (await response.json()) as ObservableSourceOfferPage;
}

export async function fetchObservationSubscriptions(options: {
	state?: ObservationSubscriptionState;
	enabled?: boolean;
	cursor?: string | null;
	limit?: number;
} = {}): Promise<ObservationSubscriptionPage> {
	const response = await fetch(
		`/api/magician/v2/observe/subscriptions${queryString({
			state: options.state,
			enabled: options.enabled,
			cursor: options.cursor,
			limit: options.limit ?? 5
		})}`
	);
	if (!response.ok) throw await responseError(response, `Subscriptions failed (${response.status})`);
	return (await response.json()) as ObservationSubscriptionPage;
}

export async function fetchObservationSourceObservability(options: {
	cursor?: string | null;
	limit?: number;
} = {}): Promise<ObservationSourceObservabilityPage> {
	const response = await fetch(
		`/api/magician/v2/observe/sources/observability${queryString({
			cursor: options.cursor,
			limit: options.limit ?? 5
		})}`
	);
	if (!response.ok) throw await responseError(response, `Source stats failed (${response.status})`);
	return (await response.json()) as ObservationSourceObservabilityPage;
}

export async function fetchObservationRunHistory(
	subscriptionId: string,
	options: { cursor?: string | null; limit?: number } = {}
): Promise<ObservationRunRecordPage> {
	const response = await fetch(
		`/api/magician/v2/observe/subscriptions/${encodeURIComponent(subscriptionId)}/runs${queryString({
			cursor: options.cursor,
			limit: options.limit ?? 5
		})}`
	);
	if (!response.ok) throw await responseError(response, `Run history failed (${response.status})`);
	return (await response.json()) as ObservationRunRecordPage;
}

export async function putObservationSubscription(
	id: string,
	input: PutObservationSubscription
): Promise<ObservationSubscription> {
	const response = await fetch(
		`/api/magician/v2/observe/subscriptions/${encodeURIComponent(id)}`,
		{
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify(input)
		}
	);
	if (!response.ok) throw await responseError(response, `Update failed (${response.status})`);
	return (await response.json()) as ObservationSubscription;
}

export async function deleteObservationSubscription(id: string): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/observe/subscriptions/${encodeURIComponent(id)}`,
		{ method: 'DELETE' }
	);
	if (!response.ok) throw await responseError(response, `Stop failed (${response.status})`);
}

export async function runObservationSubscription(id: string): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/observe/subscriptions/${encodeURIComponent(id)}/run`,
		{ method: 'POST' }
	);
	if (!response.ok) throw await responseError(response, `Run failed (${response.status})`);
}
