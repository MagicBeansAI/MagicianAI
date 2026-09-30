<script lang="ts">
	/**
	 * <EventStreamCard /> — reusable live event tail.
	 *
	 * Subscribes to `GET /api/magician/v3/events` (NDJSON over chunked HTTP).
	 * Used by `/events` (full density, no scope filter) and by ExecutionPanel
	 * Activity tab + Internals drawer (compact density, scoped to a single
	 * execution). All streaming, filter, and selection state lives here so
	 * the host route only has to pass scope + defaults.
	 *
	 * See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md` Phase 2.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		EVENT_CATEGORIES,
		EVENT_SEVERITIES,
		taxonomyFor,
		type EventCategory,
		type EventSeverity
	} from '$lib/realtime/event-taxonomy';
	import { LONG_FETCH_TIMEOUT_MS, timedFetch } from '$lib/shared/fetch';

	export let executionId: string | null = null;
	export let taskId: string | null = null;
	/**
	 * When the card is mounted with an `executionId` or `taskId` scope,
	 * setting `allowScopeBroadening` to `true` adds a `Show workspace
	 * context` toggle to the filter strip. Toggling it on drops both
	 * scope params from the request — the card then streams the full
	 * workspace feed (same surface as the global `/events` route) until
	 * the operator toggles it back. Off by default; surfaces opt in
	 * (e.g. ExecutionPanel's Activity card) when broader context is
	 * useful but the default scope is narrower.
	 */
	export let allowScopeBroadening = false;
	export let defaultAgentId: string | null = null;
	export let defaultEventType = '';
	export let defaultSearch = '';
	export let defaultUserRelevant: 'any' | 'true' | 'false' = 'any';
	export let defaultCategories: EventCategory[] = [];
	export let defaultSeverities: EventSeverity[] = [];
	export let density: 'compact' | 'full' = 'full';
	export let title = 'Event stream';
	export let showTitle = true;
	export let showFilters = true;
	export let showPeek = true;
	export let maxHeight = 'calc(100vh - 18rem)';
	export let maxRows = 5000;
	/** Optional CSS class hook for the outer wrapper. */
	export let className = '';
	/**
	 * Time anchor (ms epoch). When set, after the first batch of rows
	 * lands, the row whose `timestamp_ms` is closest to this value is
	 * scrolled into view and briefly highlighted. Used by Phase 4
	 * "Open in stream" deep-links from feed/step/attention surfaces.
	 */
	export let anchorTimestampMs: number | null = null;

	interface StreamRow {
		id: number;
		event_type: string;
		timestamp_ms: number | null;
		category: EventCategory;
		severity: EventSeverity;
		user_relevant: boolean;
		raw: Record<string, unknown>;
		serialized: string;
	}

	let rows: StreamRow[] = [];
	let nextId = 0;
	let paused = false;
	// Dedup set keyed on the serialized NDJSON line. Backfill (from
	// `events.jsonl`) and the live broadcaster overlap by design — when a
	// new SSE connection opens, the backend's `tokio::broadcast` channel
	// has buffered the most-recent events, AND those same events are
	// already on disk. Both paths emit byte-identical lines for the same
	// logical event, so the same serialized string arrives twice and the
	// row buffer used to grow with duplicates. Reset on every reconnect
	// alongside `rows = []`.
	let seenSerialized: Set<string> = new Set();
	// When true, requests skip the `executionId`/`taskId` scope params
	// so the card streams the full workspace feed. Toggled by the
	// `Show workspace context` button in the filter strip; only
	// rendered when `allowScopeBroadening` is set on the host.
	let broadenedScope = false;
	$: hasInheritedScope = !!(executionId || taskId);
	$: scopeBroadeningActive = allowScopeBroadening && hasInheritedScope && broadenedScope;
	let connection: AbortController | null = null;
	let mounted = false;
	let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	let connectionState: 'idle' | 'connecting' | 'live' | 'error' | 'closed' = 'idle';
	let connectionMessage = '';
	let selectedRow: StreamRow | null = null;
	let partialNotice: { scanned: number; scan_cap: number; message: string } | null = null;
	// Surfaced when the live broadcaster lags behind the consumer, dropping
	// events. Server emits a `__events_lagged__` synthetic NDJSON sentinel
	// each time `tokio::sync::broadcast::error::RecvError::Lagged(N)` fires
	// on the live tail; without this banner the operator silently sees a
	// stream with no indication that events were dropped between rows.
	let laggedNotice: { skipped: number; message: string; at: number } | null = null;
	// Phase 4 — anchor row management. `anchoredRowId` highlights the row
	// closest to `anchorTimestampMs` after backfill arrives; `anchorPending`
	// guards against re-resolving on every batch (we resolve once when the
	// first batch lands, then leave the user in control).
	let anchoredRowId: number | null = null;
	let anchorPending = false;
	let anchorTimer: ReturnType<typeof setTimeout> | null = null;

	let filterCategories: Set<EventCategory> = new Set(defaultCategories);
	let filterSeverities: Set<EventSeverity> = new Set(defaultSeverities);
	let filterUserRelevant: 'any' | 'true' | 'false' = defaultUserRelevant;
	let filterEventType = defaultEventType;
	let filterAgentId = defaultAgentId ?? '';
	let filterSearch = defaultSearch;

	let textFilterDebounce: ReturnType<typeof setTimeout> | null = null;
	function scheduleTextFilterReconnect(): void {
		if (textFilterDebounce) clearTimeout(textFilterDebounce);
		textFilterDebounce = setTimeout(() => {
			textFilterDebounce = null;
			void connect();
		}, 300);
	}

	$: scope = $scopeIdentityStore;
	$: filteredRows = applyClientFilters(rows);
	$: rowCount = filteredRows.length;

	// Reset the anchor request whenever the desired anchor changes — an
	// operator clicking a fresh deep-link should re-resolve, not stay
	// pinned on the prior target.
	let lastAnchorMs: number | null = null;
	$: if (anchorTimestampMs !== lastAnchorMs) {
		lastAnchorMs = anchorTimestampMs;
		anchoredRowId = null;
		anchorPending = anchorTimestampMs !== null;
	}

	// Resolve the anchor as soon as a non-empty backfill batch lands.
	// We pin to the closest row by absolute timestamp delta. After the
	// first resolve, leave anchorPending false — the operator owns
	// scroll position from that point on.
	$: if (anchorPending && filteredRows.length > 0 && anchorTimestampMs !== null) {
		anchorPending = false;
		const target = anchorTimestampMs;
		let bestId: number | null = null;
		let bestDelta = Infinity;
		for (const row of filteredRows) {
			if (row.timestamp_ms === null) continue;
			const delta = Math.abs(row.timestamp_ms - target);
			if (delta < bestDelta) {
				bestDelta = delta;
				bestId = row.id;
			}
		}
		if (bestId !== null) {
			anchoredRowId = bestId;
			scrollToAnchoredRow();
		}
	}

	function scrollToAnchoredRow(): void {
		if (anchoredRowId === null) return;
		// Defer one frame so the highlighted row is in the DOM after
		// Svelte flushes the reactive update.
		setTimeout(() => {
			const el = document.querySelector<HTMLElement>(`[data-row-id="${anchoredRowId}"]`);
			el?.scrollIntoView({ block: 'center', behavior: 'smooth' });
			if (anchorTimer) clearTimeout(anchorTimer);
			// Fade the highlight after a few seconds — long enough to
			// register, short enough not to be permanent visual noise.
			anchorTimer = setTimeout(() => {
				anchoredRowId = null;
				anchorTimer = null;
			}, 4000);
		}, 16);
	}

	function applyClientFilters(input: StreamRow[]): StreamRow[] {
		const eventTypeNeedle = filterEventType.trim().toLowerCase();
		const searchNeedle = filterSearch.trim().toLowerCase();
		const agentNeedle = filterAgentId.trim();
		return input.filter((row) => {
			if (filterCategories.size > 0 && !filterCategories.has(row.category)) return false;
			if (filterSeverities.size > 0 && !filterSeverities.has(row.severity)) return false;
			if (filterUserRelevant === 'true' && !row.user_relevant) return false;
			if (filterUserRelevant === 'false' && row.user_relevant) return false;
			if (eventTypeNeedle && !row.event_type.toLowerCase().includes(eventTypeNeedle)) return false;
			if (agentNeedle) {
				const agentId = String(row.raw?.agent_id ?? '');
				if (agentId !== agentNeedle) return false;
			}
			if (searchNeedle && !row.serialized.toLowerCase().includes(searchNeedle)) return false;
			return true;
		});
	}

	function buildEndpointUrl(): string {
		const params = new URLSearchParams();
		if (executionId && !scopeBroadeningActive) params.set('execution_id', executionId);
		if (taskId && !scopeBroadeningActive) params.set('task_id', taskId);
		if (filterCategories.size > 0) params.set('category', [...filterCategories].join(','));
		if (filterSeverities.size > 0) params.set('severity', [...filterSeverities].join(','));
		if (filterUserRelevant !== 'any') params.set('user_relevant', filterUserRelevant);
		if (filterEventType.trim()) params.set('event_type', filterEventType.trim());
		if (filterAgentId.trim()) params.set('agent_id', filterAgentId.trim());
		if (filterSearch.trim()) params.set('search', filterSearch.trim());
		return `/api/magician/v3/events?${params.toString()}`;
	}

	async function connect(): Promise<void> {
		if (!browser) return;
		await disconnect();
		// Drop the previous row buffer. `connect()` is invoked on every
		// filter change (Clear, Reset, chip toggle, search input, etc.),
		// and the backend re-sends a backfill batch each time. Without
		// this reset the new backfill accumulates on top of the old —
		// the count appears to grow on each Clear/Reset even though the
		// latest event hasn't changed (it's still the prior newest row,
		// pinned at the top by the newest-first ordering). Resetting
		// makes the buffer match exactly what the new connection
		// returns, so the count tracks reality.
		rows = [];
		nextId = 0;
		seenSerialized = new Set();
		selectedRow = null;
		anchoredRowId = null;
		const nextConnection = new AbortController();
		connection = nextConnection;
		connectionState = 'connecting';
		connectionMessage = 'connecting…';
		partialNotice = null;
		laggedNotice = null;
		try {
			const response = await timedFetch(buildEndpointUrl(), {
				headers: {
				},
				signal: nextConnection.signal,
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			});
			if (connection !== nextConnection) return;
			if (!response.ok) {
				connectionState = 'error';
				connectionMessage = `${response.status} ${response.statusText}`;
				return;
			}
			if (!response.body) {
				connectionState = 'error';
				connectionMessage = 'no response body';
				return;
			}
			connectionState = 'live';
			connectionMessage = '';
			const reader = response.body.getReader();
			const decoder = new TextDecoder('utf-8');
			let buffer = '';
			while (true) {
				const { done, value } = await reader.read();
				if (connection !== nextConnection) return;
				if (done) {
					connectionState = 'closed';
					connectionMessage = 'reconnecting';
					scheduleReconnect(nextConnection);
					return;
				}
				buffer += decoder.decode(value, { stream: true });
				let newlineIndex: number;
				while ((newlineIndex = buffer.indexOf('\n')) !== -1) {
					const line = buffer.slice(0, newlineIndex);
					buffer = buffer.slice(newlineIndex + 1);
					if (!line.trim()) continue;
					ingestLine(line);
				}
			}
		} catch (err) {
			if (connection !== nextConnection) return;
			if (nextConnection.signal.aborted || isBenignStreamAbort(err)) {
				connectionState = 'closed';
				connectionMessage = mounted ? 'reconnecting' : 'disconnected';
				if (mounted) scheduleReconnect(nextConnection);
				return;
			}
			connectionState = 'error';
			connectionMessage = err instanceof Error ? err.message : String(err);
			scheduleReconnect(nextConnection);
		}
	}

	function scheduleReconnect(expectedConnection: AbortController): void {
		if (!mounted || connection !== expectedConnection || reconnectTimer) return;
		reconnectTimer = setTimeout(() => {
			reconnectTimer = null;
			if (mounted && connection === expectedConnection) void connect();
		}, 1000);
	}

	function isBenignStreamAbort(error: unknown): boolean {
		// `AbortSignal.timeout` (used by timedFetch for the LONG_FETCH_TIMEOUT_MS
		// read deadline) rejects with a DOMException whose name is
		// 'TimeoutError', NOT 'AbortError' — and `AbortSignal.any` does not
		// propagate the abort to `connection.signal`, so a fired read deadline
		// would otherwise flash a spurious error banner on this healthy
		// long-lived stream before the 1s reconnect recovers it. Treat both
		// timeout- and abort-shaped rejections as benign (mirrors
		// chatStore.ts's TimeoutError handling) and reconnect cleanly.
		if (
			error instanceof DOMException &&
			(error.name === 'AbortError' || error.name === 'TimeoutError')
		) {
			return true;
		}
		const message = error instanceof Error ? error.message : String(error);
		return /BodyStreamBuffer.*aborted|operation was aborted|request was aborted|signal timed out|(the )?operation timed out|timed out/i.test(
			message
		);
	}

	async function disconnect(): Promise<void> {
		if (connection) {
			connection.abort();
			connection = null;
		}
	}

	function ingestLine(serialized: string): void {
		if (paused) return;
		let parsed: Record<string, unknown>;
		try {
			parsed = JSON.parse(serialized);
		} catch {
			return;
		}
		const event_type = String(parsed.event_type ?? 'unknown');
		if (event_type === '__events_partial__') {
			const scanned = typeof parsed.scanned === 'number' ? parsed.scanned : 0;
			const scan_cap = typeof parsed.scan_cap === 'number' ? parsed.scan_cap : 0;
			const message = String(parsed.message ?? '');
			partialNotice = { scanned, scan_cap, message };
			return;
		}
		// Live-tail lag — server's broadcast channel dropped `skipped`
		// events because the consumer (this stream task) couldn't drain
		// fast enough. Surface as a banner so the operator knows the
		// stream has gaps.
		if (event_type === '__events_lagged__') {
			const skipped = typeof parsed.skipped === 'number' ? parsed.skipped : 0;
			const message = String(parsed.message ?? '');
			laggedNotice = { skipped, message, at: Date.now() };
			return;
		}
		// Drop duplicates that arrived from both backfill and live tail.
		// The serialized line is byte-identical across the two paths for
		// the same logical event, so a Set keyed on `serialized` catches
		// every overlap without per-event-type custom logic. When the
		// row buffer evicts at `maxRows`, also evict from this set so we
		// don't grow unboundedly across long-running connections.
		if (seenSerialized.has(serialized)) {
			return;
		}
		seenSerialized.add(serialized);
		const taxonomy = taxonomyFor(event_type);
		const timestamp_ms = extractTimestamp(parsed);
		const row: StreamRow = {
			id: nextId++,
			event_type,
			timestamp_ms,
			category: taxonomy.category,
			severity: taxonomy.severity,
			user_relevant: taxonomy.user_relevant,
			raw: parsed,
			serialized
		};
		const next = [row, ...rows];
		if (next.length > maxRows) {
			const evicted = next.slice(maxRows);
			for (const evictedRow of evicted) {
				seenSerialized.delete(evictedRow.serialized);
			}
		}
		rows = next.slice(0, maxRows);
	}

	function extractTimestamp(raw: Record<string, unknown>): number | null {
		// Scan a layered set of candidate locations because event timestamps
		// live in different shapes depending on the wire path:
		//   - Canonical-event backfill (events.jsonl): top-level
		//     `timestamp_ms` / `timestamp`.
		//   - Live `RuntimeTransportEvent::AgentEvent` envelope:
		//     `data.event.timestamp` (the inner `AgentEventEnvelope.timestamp`
		//     field; serde-tagged with `tag = "event_type", content = "data"`).
		//   - Live typed `RuntimeTransportEvent` variant:
		//     `data.timestamp` (or `data.timestamp_ms`).
		//   - Some payloads carry `started_at` / `finished_at` / `ts` instead
		//     of `timestamp` — those are useful row anchors when no canonical
		//     timestamp is set, so try them last.
		const data = (raw as { data?: unknown }).data;
		const dataEvent =
			data !== null && typeof data === 'object'
				? (data as { event?: unknown }).event
				: undefined;
		const dataObj = data !== null && typeof data === 'object'
			? (data as Record<string, unknown>)
			: undefined;
		const dataEventObj =
			dataEvent !== null && typeof dataEvent === 'object'
				? (dataEvent as Record<string, unknown>)
				: undefined;
		const candidates: unknown[] = [
			raw.timestamp_ms,
			raw.timestamp,
			raw.timestamp_ms_int,
			dataEventObj?.timestamp,
			dataEventObj?.timestamp_ms,
			dataObj?.timestamp,
			dataObj?.timestamp_ms,
			dataObj?.started_at,
			dataObj?.finished_at,
			dataObj?.ended_at,
			dataObj?.ts
		];
		for (const candidate of candidates) {
			// Treat literal-zero numeric values as "missing" to mirror
			// `transport_log::has_structural_timestamp`: legacy variants
			// with `#[serde(default)] i64` (e.g. `FeedItemCreated.timestamp`
			// before its explicit field landed) serialize 0 when unset,
			// and accepting that would anchor the row at epoch zero
			// instead of falling through to a later candidate or
			// `Date.now()` at the caller.
			if (typeof candidate === 'number' && Number.isFinite(candidate) && candidate !== 0) {
				return candidate;
			}
			if (typeof candidate === 'string') {
				const parsed = Date.parse(candidate);
				if (Number.isFinite(parsed)) return parsed;
			}
		}
		return null;
	}

	// `MMM DD HH:MM:SS.mmm` in the user's local time zone for the row
	// time cell. Compact enough for a sortable log column but includes
	// the date so operators reading older backfilled events don't have
	// to hover the tooltip to know the day. Year is omitted from the
	// row to keep the column narrow — the verbose tooltip
	// (`formatTimestampTooltip`) carries the year + tz name for full
	// disambiguation.
	function formatTimestamp(ts: number | null): string {
		if (ts === null) return '—';
		const d = new Date(ts);
		const datePart = d.toLocaleDateString(undefined, {
			month: 'short',
			day: '2-digit'
		});
		const timePart = d.toLocaleTimeString(undefined, { hour12: false });
		const ms = String(d.getMilliseconds()).padStart(3, '0');
		return `${datePart} ${timePart}.${ms}`;
	}

	// Verbose local-time tooltip for the row time cell — full date,
	// time, and time-zone abbreviation so the operator never has to
	// guess whether `19:30:45` is local or UTC. Used as `title=` on the
	// time cell.
	function formatTimestampTooltip(ts: number | null): string {
		if (ts === null) return '';
		try {
			return new Date(ts).toLocaleString(undefined, {
				year: 'numeric',
				month: 'short',
				day: '2-digit',
				hour: '2-digit',
				minute: '2-digit',
				second: '2-digit',
				hour12: false,
				timeZoneName: 'short'
			});
		} catch {
			return new Date(ts).toString();
		}
	}

	function categoryLabel(c: EventCategory): string {
		return c.toUpperCase();
	}

	// ISO 8601 with Z or ±HH:MM offset — what the backend stamps into
	// event payloads (`finished_at`, `published_at`, etc.). Microsecond
	// precision (6 fractional digits) is supported; JS's Date only
	// keeps milliseconds, so we round-trip with whatever resolution
	// `new Date()` parses out and reformat as ms.
	const ISO_TIMESTAMP_RE =
		/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})$/;

	// Convert a UTC ISO string to local-zone ISO with explicit offset
	// (e.g. `2026-05-09T21:32:15.521988+00:00` → `2026-05-09T14:32:15.521-07:00`).
	// Returns the input unchanged when the value isn't an ISO timestamp
	// or when parsing fails.
	function localizeIsoString(s: string): string {
		if (!ISO_TIMESTAMP_RE.test(s)) return s;
		const d = new Date(s);
		if (Number.isNaN(d.getTime())) return s;
		const pad = (n: number, w = 2): string => String(n).padStart(w, '0');
		const offsetMinutes = -d.getTimezoneOffset();
		const offsetSign = offsetMinutes >= 0 ? '+' : '-';
		const absOffset = Math.abs(offsetMinutes);
		return (
			`${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}` +
			`T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}` +
			`.${pad(d.getMilliseconds(), 3)}` +
			`${offsetSign}${pad(Math.floor(absOffset / 60))}:${pad(absOffset % 60)}`
		);
	}

	// Recursively walk a JSON value and rewrite every ISO-8601 string
	// to local-zone ISO. Used to render the detail pane's raw JSON
	// dump in the operator's local time. The original payload is
	// untouched — only the displayed copy is rewritten.
	function localizeJsonTimestamps(value: unknown): unknown {
		if (typeof value === 'string') return localizeIsoString(value);
		if (Array.isArray(value)) return value.map(localizeJsonTimestamps);
		if (value !== null && typeof value === 'object') {
			const out: Record<string, unknown> = {};
			for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
				out[k] = localizeJsonTimestamps(v);
			}
			return out;
		}
		return value;
	}

	// Same ISO-8601 detector as `ISO_TIMESTAMP_RE` but as a global match
	// against an arbitrary string — for rewriting UTC ISO timestamps
	// embedded inside the row preview's raw NDJSON text. Matches both
	// `Z`-suffixed and `±HH:MM` / `±HHMM` variants, with optional
	// fractional seconds (the backend emits microseconds).
	const ISO_TIMESTAMP_RE_GLOBAL =
		/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})/g;

	// Rewrite every UTC ISO timestamp embedded in `text` to its local-zone
	// ISO equivalent. Used to localise the row preview cell so the
	// abbreviated NDJSON snippet doesn't display UTC strings even though
	// the row's `time` cell is already local. Source string (`row.serialized`)
	// stays raw — only the rendered slice is rewritten — so the search
	// filter still matches UTC substrings the user types.
	function localizePreview(text: string): string {
		return text.replace(ISO_TIMESTAMP_RE_GLOBAL, (match) => localizeIsoString(match));
	}

	function toggleCategory(c: EventCategory): void {
		const next = new Set(filterCategories);
		if (next.has(c)) next.delete(c);
		else next.add(c);
		filterCategories = next;
		void connect();
	}

	function toggleSeverity(s: EventSeverity): void {
		const next = new Set(filterSeverities);
		if (next.has(s)) next.delete(s);
		else next.add(s);
		filterSeverities = next;
		void connect();
	}

	function clearFilters(): void {
		// `Clear` means "show every event" — the equivalent of the old
		// standalone Raw Event Stream drawer card. Operators get an empty
		// category/severity set (the row filter treats `size > 0 && !has()`
		// as the gate, so empty = no constraint), `user_relevant=any`, and
		// no text filters. Use `Reset` to return to the props this card was
		// mounted with.
		filterCategories = new Set();
		filterSeverities = new Set();
		filterUserRelevant = 'any';
		filterEventType = '';
		filterAgentId = '';
		filterSearch = '';
		void connect();
	}

	function resetFiltersToDefaults(): void {
		// `Reset` returns to the curated state the card was mounted with.
		// Pairs with `Clear` (which goes fully unfiltered): once an
		// operator has wiped filters to inspect the raw tail, `Reset` is
		// the way back to the mount-time view without remembering the
		// exact default props (e.g. the Run-tab Activity card mounts with
		// `defaultUserRelevant='true'`).
		filterCategories = new Set(defaultCategories);
		filterSeverities = new Set(defaultSeverities);
		filterUserRelevant = defaultUserRelevant;
		filterEventType = defaultEventType;
		filterAgentId = defaultAgentId ?? '';
		filterSearch = defaultSearch;
		void connect();
	}

	$: hasNonDefaultFilters =
		filterCategories.size !== defaultCategories.length ||
		!defaultCategories.every((cat) => filterCategories.has(cat)) ||
		filterSeverities.size !== defaultSeverities.length ||
		!defaultSeverities.every((sev) => filterSeverities.has(sev)) ||
		filterUserRelevant !== defaultUserRelevant ||
		filterEventType !== defaultEventType ||
		filterAgentId !== (defaultAgentId ?? '') ||
		filterSearch !== defaultSearch;

	function togglePause(): void {
		paused = !paused;
	}

	function selectRow(row: StreamRow): void {
		selectedRow = selectedRow?.id === row.id ? null : row;
	}

	function downloadJsonl(): void {
		const lines = filteredRows.map((r) => r.serialized).join('\n');
		const blob = new Blob([lines], { type: 'application/x-ndjson' });
		const url = URL.createObjectURL(blob);
		const a = document.createElement('a');
		a.href = url;
		a.download = `events-${Date.now()}.jsonl`;
		a.click();
		URL.revokeObjectURL(url);
	}

	onMount(() => {
		mounted = true;
		void connect();
	});

	onDestroy(() => {
		mounted = false;
		if (reconnectTimer) {
			clearTimeout(reconnectTimer);
			reconnectTimer = null;
		}
		if (textFilterDebounce) {
			clearTimeout(textFilterDebounce);
			textFilterDebounce = null;
		}
		void disconnect();
	});

	let lastScopeKey = '';
	$: if (browser) {
		// Include `scopeBroadeningActive` in the key so toggling the
		// workspace-context affordance triggers a reconnect with the
		// updated query params.
		const effectiveExecution = scopeBroadeningActive ? '' : (executionId ?? '');
		const effectiveTask = scopeBroadeningActive ? '' : (taskId ?? '');
		const nextKey = `${scope.principal}::${scope.workspace}::${effectiveExecution}::${effectiveTask}`;
		if (nextKey !== lastScopeKey) {
			lastScopeKey = nextKey;
			void connect();
		}
	}

	function toggleScopeBroadening(): void {
		const turningOn = !broadenedScope;
		broadenedScope = turningOn;
		// `Workspace ON` is named for the operator intent: "give me the
		// view the global `/events` route would show." That route mounts
		// with no `user_relevant` gate, no category/severity prefilters,
		// no scope. The card's mount-time defaults are usually narrower
		// (e.g. ExecutionPanel's Activity card mounts with
		// `defaultUserRelevant='true'`), so just dropping the scope leaves
		// half the workspace events filtered out and the count diverges
		// from `/events`. Match the route by also clearing the curated
		// filters when broadening; restore mount-time filters when
		// re-narrowing. Free-text fields (event_type, agent_id, search)
		// are operator-typed input so we leave them alone in either
		// direction.
		if (turningOn) {
			filterCategories = new Set();
			filterSeverities = new Set();
			filterUserRelevant = 'any';
		} else {
			filterCategories = new Set(defaultCategories);
			filterSeverities = new Set(defaultSeverities);
			filterUserRelevant = defaultUserRelevant;
		}
		// Reconnect happens via the reactive scope-key block above; no
		// explicit `connect()` here.
	}

	$: hasSelection = selectedRow !== null && showPeek;
</script>

<div class="esc {density} {className}" class:has-selection={hasSelection}>
	<header class="esc-header">
		<div class="esc-title-row">
			{#if showTitle}
				<h2 class="esc-title">{title}</h2>
			{/if}
			<span class="esc-state state-{connectionState}">
				<span class="state-dot"></span>
				{connectionState === 'live' ? 'Live' : connectionState}
				{#if connectionMessage}<span class="state-msg">· {connectionMessage}</span>{/if}
			</span>
			<span class="esc-count">{rowCount} {rowCount === 1 ? 'event' : 'events'}</span>
			<div class="esc-actions">
				<button type="button" class="action-btn" on:click={togglePause}>
					{paused ? '▶ Resume' : '⏸ Pause'}
				</button>
				{#if density === 'full'}
					<button
						type="button"
						class="action-btn"
						on:click={downloadJsonl}
						title="Download visible events as NDJSON"
					>
						⤓ jsonl
					</button>
				{/if}
			</div>
		</div>

		{#if showFilters}
			<div class="esc-filters">
				<div class="filter-group">
					<span class="filter-label">Category</span>
					{#each EVENT_CATEGORIES as cat (cat)}
						<button
							type="button"
							class="chip"
							class:active={filterCategories.has(cat)}
							on:click={() => toggleCategory(cat)}
						>
							{cat}
						</button>
					{/each}
				</div>

				<div class="filter-group">
					<span class="filter-label">Severity</span>
					{#each EVENT_SEVERITIES as sev (sev)}
						<button
							type="button"
							class="chip chip-sev sev-{sev}"
							class:active={filterSeverities.has(sev)}
							on:click={() => toggleSeverity(sev)}
						>
							{sev}
						</button>
					{/each}
				</div>

				<div class="filter-group">
					<label class="inline-label">
						User relevant
						<select bind:value={filterUserRelevant} on:change={() => void connect()}>
							<option value="any">any</option>
							<option value="true">yes</option>
							<option value="false">no</option>
						</select>
					</label>
					<input
						type="text"
						placeholder="event_type contains…"
						bind:value={filterEventType}
						on:input={scheduleTextFilterReconnect}
					/>
					{#if density === 'full'}
						<input
							type="text"
							placeholder="agent_id"
							bind:value={filterAgentId}
							on:input={scheduleTextFilterReconnect}
						/>
					{/if}
					<input
						type="text"
						placeholder="search payload…"
						bind:value={filterSearch}
						on:input={scheduleTextFilterReconnect}
					/>
					<button type="button" class="action-btn" on:click={clearFilters}>Clear</button>
					{#if hasNonDefaultFilters}
						<button
							type="button"
							class="action-btn"
							on:click={resetFiltersToDefaults}
							title="Restore the filters this card was mounted with"
						>Reset</button>
					{/if}
					{#if allowScopeBroadening && hasInheritedScope}
						<button
							type="button"
							class="action-btn"
							class:action-btn--active={scopeBroadeningActive}
							aria-pressed={scopeBroadeningActive}
							on:click={toggleScopeBroadening}
							title={
								scopeBroadeningActive
									? 'Re-scope to this execution/task only'
									: 'Drop the execution/task scope and stream the whole workspace'
							}
						>{scopeBroadeningActive ? 'Workspace ✓' : 'Workspace'}</button>
					{/if}
				</div>
			</div>
		{/if}
	</header>

	{#if partialNotice}
		<div class="partial-banner" role="status">
			<span class="partial-icon" aria-hidden="true">⚠</span>
			<span>
				Scan cap reached after {partialNotice.scanned.toLocaleString()} of {partialNotice.scan_cap.toLocaleString()}
				events.
				{partialNotice.message}
				Tighten the time window, narrow filters, or scope to a single execution to see older history.
			</span>
		</div>
	{/if}

	{#if laggedNotice}
		<div class="partial-banner" role="status">
			<span class="partial-icon" aria-hidden="true">⚠</span>
			<span>
				Live stream dropped {laggedNotice.skipped.toLocaleString()} events because the broadcast
				buffer was exhausted.
				{laggedNotice.message}
			</span>
		</div>
	{/if}

	<div class="esc-body" style:--esc-max-height={maxHeight}>
		<div class="esc-list" role="log" aria-live="polite">
			<!-- Sticky column headers — mirror the row's grid-template-columns
			     so the labels line up with the cells below. Uppercase muted
			     text reads as chrome rather than content; sticky positioning
			     keeps the headers visible while operators scroll. Hidden
			     when the empty-state placeholder is the only thing in the
			     list to avoid an orphan header strip. -->
			{#if filteredRows.length > 0}
				<div class="row-header" role="presentation">
					<span class="cell time">Time</span>
					<span class="cell cat">Category</span>
					<span class="cell type">Event type</span>
					{#if density === 'full'}
						<span class="cell preview">Preview</span>
					{/if}
				</div>
			{/if}
			{#if filteredRows.length === 0}
				<div class="empty">
					{#if connectionState === 'live'}
						Waiting for events… (filters may be too restrictive)
					{:else if connectionState === 'connecting'}
						Connecting…
					{:else}
						No events.
					{/if}
				</div>
			{/if}
			{#each filteredRows as row (row.id)}
				<button
					type="button"
					class="row sev-{row.severity}"
					class:active={selectedRow?.id === row.id}
					class:anchored={anchoredRowId === row.id}
					data-row-id={row.id}
					on:click={() => selectRow(row)}
				>
					<span class="cell time" title={formatTimestampTooltip(row.timestamp_ms)}
						>{formatTimestamp(row.timestamp_ms)}</span
					>
					<span class="cell cat">{categoryLabel(row.category)}</span>
					<span class="cell type">{row.event_type}</span>
					{#if density === 'full'}
						<span class="cell preview">{localizePreview(row.serialized)}</span>
					{/if}
				</button>
			{/each}
		</div>

		{#if hasSelection && selectedRow !== null}
			<aside class="esc-peek">
				<header>
					<strong>{selectedRow.event_type}</strong>
					<button
						type="button"
						class="action-btn"
						on:click={() => (selectedRow = null)}
						aria-label="Close peek pane"
					>
						✕
					</button>
				</header>
				<dl>
					<dt>category</dt>
					<dd>{selectedRow.category}</dd>
					<dt>severity</dt>
					<dd>{selectedRow.severity}</dd>
					<dt>user_relevant</dt>
					<dd>{selectedRow.user_relevant}</dd>
					<dt>time (local)</dt>
					<dd>{formatTimestampTooltip(selectedRow.timestamp_ms)}</dd>
					<dt>timestamp_ms</dt>
					<dd>{selectedRow.timestamp_ms ?? '—'}</dd>
				</dl>
				<p class="esc-peek-note">
					Timestamps below converted to local time —
					<code>±HH:MM</code> offset is your zone.
				</p>
				<pre>{JSON.stringify(localizeJsonTimestamps(selectedRow.raw), null, 2)}</pre>
			</aside>
		{/if}
	</div>
</div>

<style>
	.esc {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
		min-height: 0;
	}

	.esc-header {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	.esc-title-row {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		flex-wrap: wrap;
	}

	.esc-title {
		margin: 0;
		font-size: 1rem;
		font-weight: 700;
		color: var(--text-primary);
		font-family: var(--font-display, var(--font-primary));
	}

	.esc.compact .esc-title {
		font-size: 0.85rem;
	}

	.esc-state {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font-size: 0.74rem;
		color: var(--text-muted);
		font-family: var(--font-mono);
	}

	.state-dot {
		display: inline-block;
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 999px;
		background: var(--text-muted);
	}

	.state-live .state-dot {
		background: var(--accent-primary);
		animation: esc-pulse 1.4s ease-in-out infinite;
	}

	.state-error .state-dot {
		background: var(--color-error);
	}

	@keyframes esc-pulse {
		0%,
		100% {
			opacity: 1;
		}
		50% {
			opacity: 0.4;
		}
	}

	.state-msg {
		opacity: 0.7;
	}

	.esc-count {
		font-family: var(--font-mono);
		font-size: 0.74rem;
		color: var(--text-muted);
	}

	.esc-actions {
		margin-left: auto;
		display: inline-flex;
		gap: 0.35rem;
	}

	.action-btn {
		font-family: var(--font-primary);
		font-size: 0.74rem;
		padding: 0.28rem 0.6rem;
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-primary);
		border-radius: 8px;
		cursor: pointer;
		transition: background 140ms ease, border-color 140ms ease;
	}

	.action-btn:hover {
		background: var(--bg-soft);
		border-color: var(--accent-primary);
	}

	.action-btn--active {
		background: var(--accent-primary-soft, var(--bg-soft));
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	.esc-filters {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		padding: 0.55rem 0.7rem;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 12px;
	}

	.esc.compact .esc-filters {
		padding: 0.45rem 0.55rem;
	}

	.filter-group {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		flex-wrap: wrap;
	}

	.filter-label {
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.58rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.12em;
		color: var(--text-muted);
		opacity: 0.75;
		min-width: 4rem;
	}

	.chip {
		font-family: var(--font-mono);
		font-size: 0.66rem;
		padding: 0.18rem 0.5rem;
		border: 1px solid var(--border-soft);
		background: transparent;
		color: var(--text-primary);
		border-radius: 999px;
		cursor: pointer;
		transition: background 140ms ease, border-color 140ms ease;
	}

	.chip:hover {
		background: var(--bg-soft);
	}

	.chip.active {
		background: var(--accent-primary-soft);
		border-color: var(--accent-primary);
		font-weight: 600;
	}

	.chip-sev.active {
		background: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}

	.inline-label {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font-family: var(--font-primary);
		font-size: 0.74rem;
		color: var(--text-muted);
	}

	.inline-label select,
	.esc-filters input[type='text'] {
		font-family: var(--font-mono);
		font-size: 0.74rem;
		padding: 0.24rem 0.45rem;
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-primary);
		border-radius: 6px;
	}

	.esc-filters input[type='text'] {
		min-width: 11rem;
	}

	.partial-banner {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		padding: 0.55rem 0.85rem;
		background: var(--accent-primary-soft);
		border: 1px solid var(--accent-primary);
		border-radius: 10px;
		font-family: var(--font-primary);
		font-size: 0.78rem;
		color: var(--text-primary);
	}

	.partial-icon {
		font-size: 1rem;
		line-height: 1;
	}

	.esc-body {
		flex: 1;
		display: grid;
		grid-template-columns: 1fr;
		gap: 1rem;
		min-height: 0;
	}

	.esc.has-selection .esc-body {
		grid-template-columns: 1fr 28rem;
	}

	.esc-list {
		display: flex;
		flex-direction: column;
		gap: 1px;
		overflow-y: auto;
		/* Allow horizontal scroll when a row's preview content is wider
		   than the viewport — long log lines, formatted JSON snippets,
		   etc. would otherwise truncate via `text-overflow: ellipsis`
		   with no escape hatch. The sticky header carries the same
		   `min-width` as the rows so the column labels stay aligned
		   while the operator scrolls horizontally. */
		overflow-x: auto;
		max-height: var(--esc-max-height, calc(100vh - 18rem));
		font-family: var(--font-mono);
		font-size: 0.74rem;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		/* No top padding so the sticky `.row-header` lays flush with the
		   scroll container's top edge — `position: sticky` pins at the
		   padding-box top, so any padding-top here would leave a strip
		   of background visible above the header during scroll. The
		   header itself carries internal padding for breathing room. */
		padding: 0 0.35rem 0.35rem;
	}

	.empty {
		padding: 1.5rem;
		text-align: center;
		color: var(--text-muted);
		font-family: var(--font-primary);
	}

	.row {
		display: grid;
		/* Column widths:
		   - Time `11rem` → fits "MMM DD HH:MM:SS.mmm" (≈18 chars in a
		     mono font) without truncation.
		   - Category `6.5rem` → fits the longest label ("OBSERVABILITY"
		     uppercase) at 0.66rem.
		   - Event type `auto`, min-content respected — keeps short types
		     compact and lets long types like `agent.execution.mapping`
		     extend.
		   - Preview `auto` → sized to its content (cells no longer
		     ellipsis), so when the line is long, the row's total width
		     exceeds the container and `.esc-list { overflow-x: auto }`
		     surfaces a horizontal scrollbar. */
		grid-template-columns: 11rem 6.5rem auto auto;
		gap: 0.6rem;
		padding: 0.28rem 0.5rem;
		border-radius: 6px;
		border: 1px solid transparent;
		border-left: 3px solid var(--text-muted);
		background: transparent;
		text-align: left;
		cursor: pointer;
		color: var(--text-primary);
		transition: background 120ms ease;
		/* Keep the row at least the viewport width so empty / short
		   events don't shrink under the scrollbar; long lines extend
		   past this and trigger horizontal scroll naturally. */
		min-width: 100%;
		width: max-content;
	}

	.esc.compact .row {
		grid-template-columns: 11rem 5rem auto;
	}

	/* Column headers — same grid as the rows so labels line up with the
	   cells. The 3px transparent border-left mirrors `.row`'s severity
	   stripe so the header lays in the same horizontal lane as the rows
	   beneath it (otherwise the header text would sit 3px to the left of
	   each row's first cell). Sticky to the top of the scroll
	   container with a card-bg fill so rows don't bleed through during
	   scroll. */
	.row-header {
		display: grid;
		/* Mirror the row's grid + width so column labels stay
		   horizontally aligned with the cells while the operator
		   scrolls left/right. `width: max-content` lets the header
		   extend with the widest row (instead of compressing to
		   viewport while rows extend past) so labels don't drift. */
		grid-template-columns: 11rem 6.5rem auto auto;
		min-width: 100%;
		width: max-content;
		gap: 0.6rem;
		padding: 0.35rem 0.5rem 0.25rem;
		border: 1px solid transparent;
		border-left: 3px solid transparent;
		border-bottom: 1px solid var(--border-soft);
		background: var(--bg-card);
		position: sticky;
		top: 0;
		z-index: 2;
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted);
		pointer-events: none;
	}

	.esc.compact .row-header {
		grid-template-columns: 11rem 5rem auto;
	}

	/* The row-header reuses the row's `.cell` modifiers (`.time`, `.cat`,
	   `.type`, `.preview`) for column alignment, but those modifiers
	   carry colour overrides aimed at the data rows (e.g. `.cell.preview`
	   → `--text-muted`). Force the muted-uppercase header colour
	   regardless of which modifier the cell carries. */
	.row-header .cell,
	.row-header .cell.cat,
	.row-header .cell.type,
	.row-header .cell.preview {
		color: var(--text-muted);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	.row:hover {
		background: var(--bg-soft);
	}

	.row.active {
		background: var(--accent-primary-soft);
		border-color: var(--accent-primary);
	}

	.row.anchored {
		animation: esc-anchor-flash 4s ease-out;
		box-shadow: 0 0 0 2px var(--accent-primary);
	}

	@keyframes esc-anchor-flash {
		0% {
			background: var(--accent-primary);
			color: var(--text-on-accent, #fff);
		}
		25% {
			background: var(--accent-primary-soft);
		}
		100% {
			background: transparent;
		}
	}

	.row.sev-info {
		border-left-color: var(--text-muted);
	}
	.row.sev-warn {
		border-left-color: var(--color-warning);
	}
	.row.sev-error {
		border-left-color: var(--color-error);
	}
	.row.sev-decision {
		border-left-color: var(--accent-secondary);
	}
	.row.sev-attention {
		border-left-color: var(--accent-primary);
	}

	.cell {
		/* Single-line, no wrap. We deliberately drop `overflow: hidden`
		   and `text-overflow: ellipsis` so long preview content extends
		   past the row's other column widths and pushes the row's total
		   width past the container — that's what triggers the parent
		   `.esc-list { overflow-x: auto }` horizontal scrollbar so the
		   operator can read the rest of the event line. */
		white-space: nowrap;
	}

	.cell.cat {
		color: var(--text-muted);
		font-weight: 600;
		font-size: 0.66rem;
		letter-spacing: 0.06em;
	}

	.cell.type {
		font-weight: 600;
	}

	.cell.preview {
		color: var(--text-muted);
	}

	.esc-peek {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		padding: 0.75rem;
		overflow-y: auto;
		max-height: var(--esc-max-height, calc(100vh - 18rem));
	}

	.esc-peek header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.82rem;
	}

	.esc-peek dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.18rem 0.6rem;
		font-family: var(--font-mono);
		font-size: 0.72rem;
		margin: 0;
	}

	.esc-peek dt {
		color: var(--text-muted);
	}

	.esc-peek dd {
		margin: 0;
		color: var(--text-primary);
		word-break: break-word;
	}

	.esc-peek pre {
		margin: 0;
		padding: 0.55rem;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		font-family: var(--font-mono);
		font-size: 0.7rem;
		line-height: 1.5;
		overflow-x: auto;
		color: var(--text-primary);
	}

	.esc-peek-note {
		margin: 0 0 0.4rem;
		font-size: 0.7rem;
		color: var(--text-muted);
		line-height: 1.35;
	}

	.esc-peek-note code {
		font-family: var(--font-mono);
		font-size: 0.66rem;
		padding: 0 0.18rem;
		border-radius: 3px;
		background: var(--bg-soft);
	}
</style>
