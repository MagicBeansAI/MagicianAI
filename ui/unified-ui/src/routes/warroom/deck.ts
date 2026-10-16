/**
 * Pure logic for the `/warroom` OPS DECK.
 *
 * Design law of the deck: NO MOTION WITHOUT INFORMATION. Every function
 * here converts a real measurement into a display parameter — event
 * severity, event-rate EMA (drives the core's breathing period), deck
 * mode (drives the room's colour state). Nothing in this module invents
 * data, and the page renders literal dashes when a source is offline
 * rather than synthesizing life.
 *
 * Kept framework-free so vitest covers it without mounting Svelte.
 */

export type Severity = 'info' | 'success' | 'warn' | 'error' | 'hitl';

export interface TapeRow {
	id: number;
	event_type: string;
	ts: number;
	agent_id: string | null;
	task_id: string | null;
	severity: Severity;
}

/** The deck's global state. Order matters: fault outranks attention. */
export type DeckMode = 'nominal' | 'attention' | 'fault';

/**
 * Severity classification ported verbatim in spirit from the previous
 * warroom (it was one of the few honest parts). Matches both PascalCase
 * typed `RuntimeTransportEvent` variants and dot-namespaced GAUI emits —
 * without the dotted branches half the runtime's events land in `info`
 * and the deck goes grey during real activity.
 */
export function classifySeverity(eventType: string): Severity {
	if (eventType.startsWith('Hitl') || eventType.endsWith('WaitingForUser')) return 'hitl';
	if (
		eventType.endsWith('Failed') ||
		eventType.endsWith('Error') ||
		eventType.endsWith('.failed') ||
		eventType.endsWith('.error') ||
		eventType === 'ProcessingError'
	) {
		return 'error';
	}
	if (
		eventType.endsWith('Completed') ||
		eventType.endsWith('.completed') ||
		eventType === 'FeedItemCreated'
	) {
		return 'success';
	}
	if (
		eventType.endsWith('Paused') ||
		eventType.endsWith('.paused') ||
		eventType.endsWith('Retrying') ||
		eventType.endsWith('.retrying')
	) {
		return 'warn';
	}
	return 'info';
}

/**
 * Normalize one NDJSON frame from `/api/magician/v3/events` into a tape
 * row. Handles the `AgentEvent` envelope (`{event_type:"AgentEvent",
 * data:{event:{agent_id, event_type, payload, timestamp}}}`) whose ids
 * live one level deeper than the typed variants — the bulk of agent
 * activity arrives through that envelope.
 *
 * Returns null for unparseable frames and `__events_*` control frames.
 */
export function normalizeEventFrame(serialized: string, id: number): TapeRow | null {
	let parsed: Record<string, unknown>;
	try {
		parsed = JSON.parse(serialized) as Record<string, unknown>;
	} catch {
		return null;
	}
	if (typeof parsed !== 'object' || parsed === null) return null;
	let event_type = String(parsed.event_type ?? 'unknown');
	if (event_type.startsWith('__events_')) return null;

	const data = asRecord(parsed.data) ?? {};
	const dataEvent = asRecord(data.event) ?? {};
	// Surface the inner event type for envelopes: `AgentEvent` alone says
	// nothing; `AgentEvent:cycle.completed` is a reading.
	const innerType = typeof dataEvent.event_type === 'string' ? dataEvent.event_type : null;
	if (event_type === 'AgentEvent' && innerType) event_type = innerType;

	const ts = extractTimestamp(parsed, data, dataEvent) ?? Date.now();
	const agent_id = stringOrNull(data.agent_id ?? parsed.agent_id ?? dataEvent.agent_id);
	const item = asRecord(data.item);
	const payload = asRecord(dataEvent.payload) ?? {};
	const task_id = stringOrNull(
		data.task_id ?? parsed.task_id ?? item?.task_id ?? dataEvent.task_id ?? payload.task_id
	);
	return { id, event_type, ts, agent_id, task_id, severity: classifySeverity(event_type) };
}

/**
 * Timestamp extraction with the literal-zero guard: legacy variants with
 * `#[serde(default)] i64` serialize 0 when unset, and accepting that
 * anchors every age display at epoch zero. Mirrors
 * `transport_log::has_structural_timestamp`.
 */
export function extractTimestamp(
	parsed: Record<string, unknown>,
	data: Record<string, unknown>,
	dataEvent: Record<string, unknown>
): number | null {
	const candidates = [
		parsed.timestamp_ms,
		parsed.timestamp,
		data.timestamp_ms,
		data.timestamp,
		dataEvent.timestamp,
		dataEvent.timestamp_ms
	];
	for (const v of candidates) {
		if (typeof v === 'number' && Number.isFinite(v) && v !== 0) return v;
		if (typeof v === 'string') {
			const at = Date.parse(v);
			if (Number.isFinite(at) && at !== 0) return at;
		}
	}
	return null;
}

/**
 * Is this frame LIVE traffic, or replayed history?
 *
 * The stream replays history on connect. Those frames are real and belong on
 * the tape, but they must not drive the rate gauge, the histogram or the core
 * flash — feeding replay into them made the deck read "65.8/s" while the
 * uplink was idle, which is precisely the lie this deck exists to avoid.
 *
 * Frames from the future (clock skew) count as live rather than being
 * discarded; skew is not the operator's problem and dropping them would
 * under-report a genuinely busy system.
 */
export function isLiveArrival(ts: number, nowMs: number, windowMs = 5_000): boolean {
	return nowMs - ts <= windowMs;
}

/**
 * Exponential moving average of event arrivals, in events/second.
 *
 * This single number drives the core's breathing: a quiet system breathes
 * slowly, a busy one visibly quickens. `halfLifeMs` controls how fast the
 * deck forgets — 20s means a burst decays within a minute rather than
 * leaving the core panting for an hour.
 */
export function nextEventRate(
	prevRate: number,
	elapsedMs: number,
	arrivals: number,
	halfLifeMs = 20_000
): number {
	if (elapsedMs <= 0) return prevRate;
	const instantaneous = (arrivals * 1000) / elapsedMs;
	const alpha = 1 - Math.pow(0.5, elapsedMs / halfLifeMs);
	const next = prevRate + alpha * (instantaneous - prevRate);
	return Number.isFinite(next) && next >= 0 ? next : 0;
}

/**
 * Breathing period for the core, seconds. Idle systems rest at 6s; a
 * saturated stream approaches 1.8s. The mapping is asymptotic so a burst
 * can never drive the period to zero and turn the core into a strobe.
 */
export function breathePeriodSeconds(eventsPerSecond: number): number {
	const period = 1.8 + 4.2 / (1 + eventsPerSecond * 1.4);
	return Math.round(period * 100) / 100;
}

/**
 * The room's state. Fault outranks attention outranks nominal:
 * - fault: the uplink itself is broken (health unreachable / unhealthy)
 *   or errors are arriving in a burst — the operator's first problem is
 *   the system, not the queue.
 * - attention: at least one human-input request is pending. The deck
 *   shifts amber because the system is, truthfully, waiting on YOU.
 */
export function deriveDeckMode(input: {
	healthOk: boolean | null; // null = not yet probed; only `false` faults
	pendingHitl: number;
	recentErrors: number; // errors on the tape inside the last minute
	errorBurstThreshold?: number;
}): DeckMode {
	const threshold = input.errorBurstThreshold ?? 3;
	if (input.healthOk === false || input.recentErrors >= threshold) return 'fault';
	if (input.pendingHitl > 0) return 'attention';
	return 'nominal';
}

/**
 * Ratio of today's figure against yesterday's same-time figure, clamped
 * to [0, 2] for the core arcs: 1.0 = tracking yesterday exactly, 2.0 =
 * double or better. Null when yesterday has no baseline — the arc
 * renders hollow rather than pretending a ratio exists.
 */
export function pacingRatio(today: number, yesterdaySameTime: number): number | null {
	if (!Number.isFinite(today) || today < 0) return null;
	if (!Number.isFinite(yesterdaySameTime) || yesterdaySameTime <= 0) return null;
	return Math.min(2, today / yesterdaySameTime);
}

/** Count tape errors newer than `windowMs` — the fault-burst signal. */
export function recentErrorCount(rows: TapeRow[], nowMs: number, windowMs = 60_000): number {
	let n = 0;
	for (const row of rows) {
		if (row.severity !== 'error') continue;
		if (nowMs - row.ts <= windowMs) n += 1;
	}
	return n;
}

// ── formatters ─────────────────────────────────────────────────────────

export function fmtUsd(value: number | null | undefined): string {
	if (value == null || !Number.isFinite(value)) return '—';
	return `$${value.toFixed(2)}`;
}

export function fmtCompact(value: number | null | undefined): string {
	if (value == null || !Number.isFinite(value)) return '—';
	if (Math.abs(value) >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
	if (Math.abs(value) >= 10_000) return `${Math.round(value / 1000)}K`;
	if (Math.abs(value) >= 1_000) return `${(value / 1000).toFixed(1)}K`;
	return String(Math.round(value));
}

/** Compact age: 12s · 4m · 2h · 3d. Never negative (clock skew → 0s). */
export function fmtAge(ts: number, nowMs: number): string {
	const s = Math.max(0, Math.round((nowMs - ts) / 1000));
	if (s < 60) return `${s}s`;
	const m = Math.floor(s / 60);
	if (m < 60) return `${m}m`;
	const h = Math.floor(m / 60);
	if (h < 24) return `${h}h`;
	return `${Math.floor(h / 24)}d`;
}

function asRecord(v: unknown): Record<string, unknown> | null {
	return v !== null && typeof v === 'object' && !Array.isArray(v)
		? (v as Record<string, unknown>)
		: null;
}

function stringOrNull(v: unknown): string | null {
	return typeof v === 'string' && v.length > 0 ? v : null;
}
