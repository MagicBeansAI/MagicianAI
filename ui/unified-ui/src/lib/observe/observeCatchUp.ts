export type CatchUpPhase = 'waiting' | 'active' | 'completed' | 'disabled' | 'expired';
export type CatchUpReplayMode =
	| 'checkpointed_replay'
	| 'current_snapshot_only'
	| 'scheduled_window';

export interface ObserveCatchUpPolicy {
	schema_version: number;
	revision: number;
	enabled: boolean;
	lookback_days: number;
	max_items_per_source: number;
	max_total_items: number;
	max_duration_minutes: number;
}

export interface CatchUpSourceStatus {
	source_id: string;
	display_name: string;
	item_unit: string;
	replay_mode: CatchUpReplayMode;
	admitted: number;
	processed: number;
	runs: number;
	failures: number;
	last_error?: string;
	limitation: string;
}

export interface ObserveCatchUpStatus {
	policy: ObserveCatchUpPolicy;
	phase: CatchUpPhase;
	boot_id: string;
	boot_started_at_ms: number;
	catch_up_started_at_ms?: number;
	deadline_at_ms?: number;
	admitted_items: number;
	processed_items: number;
	reserved_items: number;
	remaining_items: number;
	sources: CatchUpSourceStatus[];
}

export interface ObserveCatchUpEnvelope {
	status: ObserveCatchUpStatus;
	warnings?: string[];
	options: {
		lookback_days: number[];
		max_items_per_source: number[];
		max_total_items: number[];
		max_duration_minutes: number[];
	};
}

const ENDPOINT = '/api/magician/v2/observe/catch-up';

async function responseError(response: Response): Promise<string> {
	const body = await response.json().catch(() => ({}));
	return typeof body?.error === 'string' ? body.error : `HTTP ${response.status}`;
}

export async function fetchObserveCatchUp(): Promise<ObserveCatchUpEnvelope> {
	let response: Response;
	try {
		response = await fetch(ENDPOINT, { headers: scopedRequestHeaders() });
	} catch {
		throw new Error('Magician is offline. Start it and try again.');
	}
	if (!response.ok) throw new Error(await responseError(response));
	return (await response.json()) as ObserveCatchUpEnvelope;
}

export async function saveObserveCatchUp(
	policy: ObserveCatchUpPolicy
): Promise<ObserveCatchUpEnvelope> {
	let response: Response;
	try {
		response = await fetch(ENDPOINT, {
			method: 'PUT',
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify({
				expected_revision: policy.revision,
				enabled: policy.enabled,
				lookback_days: policy.lookback_days,
				max_items_per_source: policy.max_items_per_source,
				max_total_items: policy.max_total_items,
				max_duration_minutes: policy.max_duration_minutes
			})
		});
	} catch {
		throw new Error('Magician is offline. Start it and try again.');
	}
	if (!response.ok) throw new Error(await responseError(response));
	return (await response.json()) as ObserveCatchUpEnvelope;
}
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
