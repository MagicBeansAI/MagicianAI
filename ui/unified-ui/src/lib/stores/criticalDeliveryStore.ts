/**
 * Critical-request delivery — Settings surface for `hitl.critical_delivery`
 * (secure HITL plan §6.1, P5).
 *
 * Reads and writes the one section that decides which of the owner's
 * verified channels get a critical-request alert, in what order, and when;
 * reads the value-free delivery status; and triggers the owner's test alert.
 * Opening or saving the page sends nothing — only the test action does.
 */
import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type CriticalDeliveryPolicy = 'simultaneous' | 'staged';

export interface QuietHoursSettings {
	start: string;
	end: string;
	timezone: string;
	interrupt_for_time_bound: boolean;
}

export interface CriticalDeliverySettings {
	enabled_channels: string[];
	policy: CriticalDeliveryPolicy;
	staged_fallback_secs: number;
	push_enabled: boolean;
	quiet_hours: QuietHoursSettings | null;
}

export interface CriticalDeliveryChannel {
	channel_type: string;
	owner_addresses: string[];
	has_owner: boolean;
}

export interface CriticalDeliveryEnvelope {
	settings_path: string;
	settings: CriticalDeliverySettings;
	channels: CriticalDeliveryChannel[];
	available_channels: string[];
	public_origin_configured: boolean;
	warnings: string[];
	reload_applied?: boolean;
	reload_error?: string;
}

export interface CriticalDeliveryRow {
	id: string;
	correlation_id: string;
	kind: string;
	destination: string;
	channel_type: string | null;
	state: string;
	attempts: number;
	requested_at_ms: number;
	enqueued_at_ms: number;
	accepted_at_ms: number | null;
	updated_at_ms: number;
	reason: string | null;
	registrations: number | null;
}

export interface CriticalDeliveryPercentiles {
	samples: number;
	p50_ms: number | null;
	p95_ms: number | null;
}

export interface CriticalDeliveryStatus {
	deliveries: CriticalDeliveryRow[];
	latency: {
		request_to_enqueue: CriticalDeliveryPercentiles;
		enqueue_to_acceptance: CriticalDeliveryPercentiles;
	};
	channels_last_claimed_ms: Record<string, number>;
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(record: Record<string, unknown>, field: string): string | null {
	const value = record[field];
	return typeof value === 'string' ? value : null;
}

function readNumber(record: Record<string, unknown>, field: string): number | null {
	const value = record[field];
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function readBoolean(record: Record<string, unknown>, field: string): boolean | null {
	const value = record[field];
	return typeof value === 'boolean' ? value : null;
}

function readStringArray(record: Record<string, unknown>, field: string): string[] {
	const value = record[field];
	if (!Array.isArray(value)) return [];
	return value.filter((item): item is string => typeof item === 'string');
}

export function defaultCriticalDeliverySettings(): CriticalDeliverySettings {
	return {
		enabled_channels: [],
		policy: 'simultaneous',
		staged_fallback_secs: 45,
		push_enabled: true,
		quiet_hours: null
	};
}

export function parseCriticalDeliverySettings(raw: unknown): CriticalDeliverySettings {
	const record = asRecord(raw);
	const defaults = defaultCriticalDeliverySettings();
	if (!record) return defaults;
	const quiet = asRecord(record.quiet_hours);
	return {
		enabled_channels: readStringArray(record, 'enabled_channels'),
		policy: readString(record, 'policy') === 'staged' ? 'staged' : 'simultaneous',
		staged_fallback_secs: readNumber(record, 'staged_fallback_secs') ?? defaults.staged_fallback_secs,
		push_enabled: readBoolean(record, 'push_enabled') ?? true,
		quiet_hours: quiet
			? {
					start: readString(quiet, 'start') ?? '22:00',
					end: readString(quiet, 'end') ?? '07:00',
					timezone: readString(quiet, 'timezone') ?? 'UTC',
					interrupt_for_time_bound: readBoolean(quiet, 'interrupt_for_time_bound') ?? true
				}
			: null
	};
}

function parseChannel(raw: unknown): CriticalDeliveryChannel | null {
	const record = asRecord(raw);
	if (!record) return null;
	const channelType = readString(record, 'channel_type');
	if (!channelType) return null;
	return {
		channel_type: channelType,
		owner_addresses: readStringArray(record, 'owner_addresses'),
		has_owner: readBoolean(record, 'has_owner') ?? false
	};
}

export function parseCriticalDeliveryEnvelope(raw: unknown): CriticalDeliveryEnvelope | null {
	const root = asRecord(raw);
	if (!root) return null;
	const settingsPath = readString(root, 'settings_path');
	if (!settingsPath) return null;
	return {
		settings_path: settingsPath,
		settings: parseCriticalDeliverySettings(root.settings),
		channels: Array.isArray(root.channels)
			? root.channels.map(parseChannel).filter((c): c is CriticalDeliveryChannel => c !== null)
			: [],
		available_channels: readStringArray(root, 'available_channels'),
		public_origin_configured: readBoolean(root, 'public_origin_configured') ?? false,
		warnings: readStringArray(root, 'warnings'),
		reload_applied: readBoolean(root, 'reload_applied') ?? undefined,
		reload_error: readString(root, 'reload_error') ?? undefined
	};
}

function parseRow(raw: unknown): CriticalDeliveryRow | null {
	const record = asRecord(raw);
	if (!record) return null;
	const id = readString(record, 'id');
	const correlationId = readString(record, 'correlation_id');
	if (!id || !correlationId) return null;
	return {
		id,
		correlation_id: correlationId,
		kind: readString(record, 'kind') ?? 'request',
		destination: readString(record, 'destination') ?? '—',
		channel_type: readString(record, 'channel_type'),
		state: readString(record, 'state') ?? 'unknown',
		attempts: readNumber(record, 'attempts') ?? 0,
		requested_at_ms: readNumber(record, 'requested_at_ms') ?? 0,
		enqueued_at_ms: readNumber(record, 'enqueued_at_ms') ?? 0,
		accepted_at_ms: readNumber(record, 'accepted_at_ms'),
		updated_at_ms: readNumber(record, 'updated_at_ms') ?? 0,
		reason: readString(record, 'reason'),
		registrations: readNumber(record, 'registrations')
	};
}

function parsePercentiles(raw: unknown): CriticalDeliveryPercentiles {
	const record = asRecord(raw);
	return {
		samples: record ? (readNumber(record, 'samples') ?? 0) : 0,
		p50_ms: record ? readNumber(record, 'p50_ms') : null,
		p95_ms: record ? readNumber(record, 'p95_ms') : null
	};
}

export function parseCriticalDeliveryStatus(raw: unknown): CriticalDeliveryStatus | null {
	const root = asRecord(raw);
	if (!root) return null;
	const latency = asRecord(root.latency);
	const lastClaimed = asRecord(root.channels_last_claimed_ms) ?? {};
	return {
		deliveries: Array.isArray(root.deliveries)
			? root.deliveries.map(parseRow).filter((row): row is CriticalDeliveryRow => row !== null)
			: [],
		latency: {
			request_to_enqueue: parsePercentiles(latency?.request_to_enqueue),
			enqueue_to_acceptance: parsePercentiles(latency?.enqueue_to_acceptance)
		},
		channels_last_claimed_ms: Object.fromEntries(
			Object.entries(lastClaimed).filter((entry): entry is [string, number] => typeof entry[1] === 'number')
		)
	};
}

async function readApiError(response: Response): Promise<string> {
	let message = `Request failed (${response.status})`;
	try {
		const text = await response.text();
		if (!text) return message;
		try {
			const parsed = JSON.parse(text) as unknown;
			const root = asRecord(parsed);
			const errorText = root ? readString(root, 'error') : undefined;
			const messageText = root ? readString(root, 'message') : undefined;
			message = `Request failed (${response.status}): ${messageText || errorText || text}`;
		} catch {
			message = `Request failed (${response.status}): ${text}`;
		}
	} catch {
		// Best effort only.
	}
	return message;
}

export async function fetchCriticalDelivery(): Promise<CriticalDeliveryEnvelope> {
	const response = await timedFetch('/api/magician/v2/settings/critical-delivery', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	if (!response.ok) throw new Error(await readApiError(response));
	const envelope = parseCriticalDeliveryEnvelope(await response.json());
	if (!envelope) throw new Error('Malformed critical delivery settings response');
	return envelope;
}

export async function saveCriticalDelivery(
	settings: CriticalDeliverySettings
): Promise<CriticalDeliveryEnvelope> {
	const response = await timedFetch('/api/magician/v2/settings/critical-delivery', {
		method: 'PUT',
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		body: JSON.stringify({
			critical_delivery: {
				...settings,
				quiet_hours: settings.quiet_hours ?? undefined
			}
		}),
		timeoutMs: LONG_FETCH_TIMEOUT_MS
	});
	if (!response.ok) throw new Error(await readApiError(response));
	const envelope = parseCriticalDeliveryEnvelope(await response.json());
	if (!envelope) throw new Error('Malformed critical delivery save response');
	return envelope;
}

export async function fetchCriticalDeliveryStatus(): Promise<CriticalDeliveryStatus> {
	const response = await timedFetch('/api/magician/v2/hitl/deliveries', {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	if (!response.ok) throw new Error(await readApiError(response));
	const status = parseCriticalDeliveryStatus(await response.json());
	if (!status) throw new Error('Malformed delivery status response');
	return status;
}

/** The owner's explicit test: a real alert through every enabled destination. */
export async function sendCriticalDeliveryTest(): Promise<{ correlation_id: string | null; destinations: number }> {
	const response = await timedFetch('/api/magician/v2/settings/critical-delivery/test', {
		method: 'POST',
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		timeoutMs: LONG_FETCH_TIMEOUT_MS
	});
	if (!response.ok) throw new Error(await readApiError(response));
	const root = asRecord(await response.json());
	return {
		correlation_id: root ? readString(root, 'correlation_id') : null,
		destinations: root ? (readNumber(root, 'destinations') ?? 0) : 0
	};
}
