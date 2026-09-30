<script lang="ts">
	/**
	 * <ActivityView /> — the unified runtime activity view.
	 *
	 * One live surface for every unit of work in flight: agent steps,
	 * background distillation/classification/consolidation, LLM dispatch,
	 * governed child processes, capability invocation. Before this, agent
	 * work was the only visible family and everything else surfaced as
	 * counts after the fact.
	 *
	 * BUS — same transport every other event surface here uses:
	 * `GET /api/magician/v3/events` (NDJSON over chunked HTTP), filtered
	 * server-side with `category=activity`. Follows
	 * `$lib/realtime/EventStreamCard.svelte`'s connect/ingest/reconnect
	 * shape — `timedFetch` + `AbortController` + a byte-line reader, with
	 * the backfill-then-live-tail overlap and dedupe the shared stream lines.
	 * There is no polling here and none is needed: the endpoint pushes.
	 * The half of the shared poll/backoff convention that DOES apply is
	 * `$lib/realtime/backoff.ts` — one jittered `createBackoff` instance
	 * per connection, so a backend restart does not have every open tab
	 * reconnecting in lockstep. EventStreamCard's fixed 1s retry predates
	 * that module; new surfaces are expected to use it.
	 *
	 * ONE STREAM — the operator's own scope, and nothing else.
	 *
	 * This used to open a second stream to system/system, because a span
	 * that declared no scope was filed there and was otherwise invisible.
	 * That subscription was the problem: system/system is shared across
	 * principals, so every viewer had to read a shared bucket to see
	 * ordinary background work, and no client-side gate could fix that
	 * (scope is asserted by the caller, so anyone could open the same
	 * stream directly).
	 *
	 * Undeclared activity now falls back to the default scope
	 * (`anonymous`/`default`) instead — the runtime's normal home for work
	 * with no declared owner — so it arrives on this stream like anything
	 * else and the shared subscription is simply gone. Genuinely
	 * runtime-wide passes declare `system`/`system` positively and are
	 * deliberately NOT shown here; they are not this operator's work.
	 *
	 * If background work goes missing from this view, the span that emitted
	 * it is not declaring its scope. Instrument the span — do not reach for
	 * a second stream.
	 *
	 * Plan: `docs/plans/2026-08-14-unified-runtime-activity-view.md` (Task 7).
	 */
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { createBackoff, type Backoff } from '$lib/realtime/backoff';
	import { LONG_FETCH_TIMEOUT_MS, timedFetch } from '$lib/shared/fetch';
	import EmptyState from '$lib/magician/components/native/EmptyState.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';

	/** Max spans held in memory. Oldest root subtrees are evicted whole.
	 *
	 *  This is the retention window, and it is the cap that decides how far
	 *  back the view can look. Measured on a live runtime 2026-08-15: 11.4
	 *  spans/sec sustained, so the previous 1200 held about 105 seconds. That
	 *  is enough to watch work in flight and not enough to open the page
	 *  *because* something looked wrong — by then the span had been evicted.
	 *  10000 holds roughly fifteen minutes at that rate.
	 *
	 *  Cost is memory AND per-flush CPU, which is the part that binds first.
	 *  These are Map entries, never DOM — most carry no progress rows at all
	 *  (progress was 3% of the measured stream), so the per-node footprint is a
	 *  few hundred bytes and the realistic total is single-digit MB. But three
	 *  derivations walk the whole map on every 150ms flush — the row tree, the
	 *  lanes and the header — so the retained count is also a 6.7Hz O(n) cost.
	 *  Raising this again means measuring the flush, not just the heap.
	 *  `MAX_RENDERED_ROWS` is what protects the DOM. */
	const MAX_NODES = 10000;
	/** Max log lines kept under a single span. */
	const MAX_PROGRESS_PER_NODE = 300;
	/** Max unspanned (span-floor) log lines kept at the root. */
	const MAX_LOOSE_PROGRESS = 400;
	/** DOM guard — rows past this are not rendered.
	 *
	 *  Raised with `MAX_NODES`, but deliberately by much less. Every row past
	 *  this point is a real DOM node and there is no virtualization here, so
	 *  this trades directly against scroll and render cost in a way the node
	 *  cap does not. It also only binds when spans are *expanded*: `pushSpan`
	 *  skips the children of a collapsed span, so the common case renders far
	 *  fewer rows than the tree holds. If this ever becomes the limit people
	 *  actually hit, virtualize the list rather than raising it again. */
	const MAX_RENDERED_ROWS = 6000;
	/** Dedup set cap; cleared wholesale on overflow (see `seenSerialized`). */
	const MAX_SEEN = 50_000;
	/** Costs parked waiting for a span that has not arrived. Small on purpose:
	 *  a cost normally lands moments after its own `ActivityFinished`, so a
	 *  large backlog means the spans are gone, not late. */
	const MAX_PENDING_COSTS = 500;
	/** Ingest → render coalescing window. A burst of 8 events/sec must not
	 *  mean 8 full tree rebuilds per second. */
	const FLUSH_MS = 150;
	/** History window for one-shot durable backfill on mount/scope switch. */
	const RUNTIME_HISTORY_LOOKBACK_MS = 24 * 60 * 60 * 1000;
	/** Keep a little overlap to absorb clock and flush drift at the seam. */
	const RUNTIME_HISTORY_OVERLAP_MS = 60_000;
	/** The maximum number of activity rows to hydrate in one backfill call. */
	const RUNTIME_HISTORY_MAX_ROWS = 10_000;
	const ACTIVITY_HISTORY_ENDPOINT = '/api/magician/v2/analytics/activity_rows/query';

	const ACTIVITY_EVENT_TYPES = new Set([
		'ActivityStarted',
		'ActivityFinished',
		'ActivityProgress',
		'ActivityCost'
	]);

	/**
	 * The closed `kind` set from `analytics/runtime_activity_layer.rs`
	 * (`ACTIVITY_KINDS`), plus `loose` — this view's own bucket for
	 * progress rows below the span floor.
	 *
	 * Each family gets BOTH a hue and a distinct glyph shape. An operator
	 * has to be able to tell a background distillation from an agent step
	 * without reading the label, and colour alone fails that for anyone
	 * who cannot separate the hues.
	 */
	interface KindMeta {
		label: string;
		glyph: string;
		color: string;
	}
	const KIND_META: Record<string, KindMeta> = {
		agent: { label: 'agent', glyph: '◆', color: 'var(--color-info)' },
		background: { label: 'background', glyph: '●', color: 'var(--accent-sage)' },
		capability: { label: 'capability', glyph: '▲', color: 'var(--color-success)' },
		llm: { label: 'llm', glyph: '✦', color: 'var(--accent-primary)' },
		process: { label: 'process', glyph: '■', color: 'var(--text-primary)' },
		runtime: { label: 'runtime', glyph: '○', color: 'var(--text-muted)' },
		loose: { label: 'unspanned', glyph: '—', color: 'var(--text-faint)' }
	};
	const FILTER_KINDS = Object.keys(KIND_META);
	const LOOSE_KIND = 'loose';

	function kindMeta(kind: string): KindMeta {
		return KIND_META[kind] ?? KIND_META.runtime;
	}

	interface ProgressRow {
		key: string;
		activityId: string | null;
		/** Severity of THIS line. The taxonomy severity of `ActivityProgress`
		 *  is a static `info` — a per-variant table cannot vary per instance —
		 *  so rows colour from the payload's `level`, never from
		 *  `taxonomyFor('ActivityProgress').severity`. Documented at the
		 *  variant in `realtime_events.rs`; it is an easy trap. */
		level: string;
		message: string;
		target: string;
		principal: string | null;
		workspace: string | null;
		timestampMs: number | null;
		seq: number;
	}

	interface SpanCost {
		/** Millionths of one unit of `commodity`. Integer, because these are
		 *  summed across many rows and float error accumulates into a figure
		 *  read as money. */
		microunits: number;
		/** `usd`, `local`, or whatever else a provider prices in. Never assume
		 *  currency: commodities are NOT interchangeable and must never be
		 *  added together. `local` means the work ran on this machine and money
		 *  was never the unit — distinct from a vendor charging zero. */
		commodity: string;
		inputTokens: number | null;
		outputTokens: number | null;
	}

	interface SpanNode {
		id: string;
		parentId: string | null;
		name: string;
		target: string;
		kind: string;
		/** What this work is *for*, as opposed to what it is. `kind` says a
		 *  span is an LLM call; this says whether anyone is waiting on it.
		 *
		 *  `null` means UNDECLARED and is a real, renderable state — never
		 *  fold it into a declared class. A root that forgot to declare is an
		 *  instrumentation gap the view is meant to expose, and inventing a
		 *  plausible class for it would hide exactly that. */
		workloadClass: string | null;
		agentId: string | null;
		threadId: string | null;
		taskId: string | null;
		/** LLM spans only. Absent everywhere else, and absence renders as
		 *  absent rather than as a placeholder. */
		model: string | null;
		/** What this span asked for — the `LLMOperation` name, on the spans
		 *  that dispatch something.
		 *
		 *  The one dimension here that does NOT inherit, and the only thing
		 *  that tells two `llm_dispatch` rows apart: the span name is the
		 *  boundary's name and is identical on every dispatch by construction,
		 *  so without this a hundred model calls render as a hundred copies of
		 *  the same line. Absence is not an instrumentation gap here — a span
		 *  that declares no operation performed none. */
		operation: string | null;
		/** What this span cost, once the providers said so — one entry per
		 *  commodity, accumulated.
		 *
		 *  **A list, not a value.** A single span routinely makes more than one
		 *  model call: an agent step that retries, a chunked summarisation, a
		 *  call that falls back to a second provider. Each publishes its own
		 *  `ActivityCost`. Holding one value meant the last arrival overwrote
		 *  every earlier one, so a span that spent four times reported the
		 *  price of its fourth call — silently, and always low. It also made
		 *  two commodities mutually exclusive: a span that ran one paid call
		 *  and one local call could only show whichever landed last.
		 *
		 *  Accumulated per commodity, never across them: `usd` and `local` and
		 *  `tavily_credit` count different things and a sum of them is a
		 *  confident wrong number.
		 *
		 *  Empty is the honest state for "not priced yet, or never priced" —
		 *  costs arrive AFTER `ActivityFinished`, so a finished row is legitimately
		 *  empty for a moment. It must never render as a zero, which would say
		 *  the work was free. */
		costs: SpanCost[];
		principal: string | null;
		workspace: string | null;
		startedMs: number | null;
		finishedMs: number | null;
		durationMs: number | null;
		/** `closed` by default — the layer watched a span END, not succeed.
		 *  `error` is inferred from an ERROR line inside the span. Never
		 *  render `closed` as a success tone. */
		outcome: string | null;
		finished: boolean;
		seq: number;
		progress: ProgressRow[];
	}

	interface ActivityRowsQueryResponse {
		columns: string[];
		rows: Array<Array<unknown>>;
		row_count: number;
		inventory_complete?: boolean;
	}

	interface ActivityRowRecord {
		activityId: string;
		parentActivityId: string | null;
		name: string;
		target: string;
		kind: string;
		workloadClass: string | null;
		principal: string | null;
		workspace: string | null;
		agentId: string | null;
		threadId: string | null;
		taskId: string | null;
		model: string | null;
		startedAtMs: number | null;
		durationMs: number | null;
		outcome: string | null;
	}

	type FlatRow =
		| {
				type: 'span';
				key: string;
				depth: number;
				node: SpanNode;
				childCount: number;
				lineCount: number;
				expandable: boolean;
				expanded: boolean;
				orphan: boolean;
		  }
		| {
				type: 'progress';
				key: string;
				depth: number;
				row: ProgressRow;
				loose: boolean;
		  };

	// ─── Ingest state (mutated outside Svelte reactivity; `dataVersion`
	//     is the single reactive trigger, bumped on a coalescing flush) ───
	const nodes = new Map<string, SpanNode>();
	/** Progress lines whose `activity_id` names no span we hold — either
	 *  emitted outside any instrumented span (the span floor), or a span
	 *  whose `ActivityStarted` was evicted. Rendered at the root, never
	 *  dropped. */
	let looseProgress: ProgressRow[] = [];
	/** Costs whose span has not arrived yet, keyed by activity id.
	 *
	 *  The mirror of `looseProgress`: a progress line can land BEFORE the span
	 *  that owns it, and a cost lands AFTER the span finishes — but backfill
	 *  and the live tail overlap, so either can arrive in either order. Both
	 *  need somewhere to wait.
	 *
	 *  Unlike a loose progress row, a homeless cost is NEVER rendered at the
	 *  root: a log line still reads as something on its own, whereas "$0.004"
	 *  attached to nothing is noise. It waits here, and is dropped if its span
	 *  never comes.
	 *
	 *  A list per id, for the same reason `SpanNode.costs` is one: several
	 *  costs can land for one span before that span arrives. Accumulating here
	 *  also makes the eviction order honest — a `Map` keeps a key's original
	 *  insertion position, so the entry's place in the queue is the age of its
	 *  FIRST cost, which is exactly what "longest waiting" should mean. */
	const pendingCosts = new Map<string, SpanCost[]>();
	let seenSerialized = new Set<string>();
	let seq = 0;
	let dataVersion = 0;
	let flushTimer: ReturnType<typeof setTimeout> | null = null;

	let expanded = new Set<string>();
	let kindEnabled: Record<string, boolean> = Object.fromEntries(
		FILTER_KINDS.map((kind) => [kind, true])
	);
	let paused = false;
	let now = Date.now();
	let clockTimer: ReturnType<typeof setInterval> | null = null;

	/** Process-cumulative count of activity records the layer's bounded
	 *  queue evicted before they could be emitted. Monotonic and stamped at
	 *  drain time, so the NEWEST row carries the running total — tracked by
	 *  timestamp rather than by max, because the count restarts at zero with
	 *  the runtime and backfill replays older, smaller totals out of order
	 *  against the live tail. See the note at the assignment site. */
	let droppedTotal = 0;
	/** Timestamp of the row `droppedTotal` was read from, so an older row
	 *  arriving later cannot overwrite a newer total. */
	let droppedAsOfMs = -Infinity;
	/** Transport-level gap: the broadcast channel dropped events between
	 *  the runtime and this tab. Different failure from `droppedTotal`
	 *  (which is the layer's own queue), same obligation to say so. */
	let laggedNotice: { skipped: number; message: string } | null = null;
	let historyNotice: string | null = null;
	let historyInventoryComplete = true;
	let historyAbort: AbortController | null = null;
	let lanesCollapsed = false;

	// ─── Streams ───────────────────────────────────────────────────────
	interface StreamHandle {
		/** Only `scope` today. Kept as a named union so a future stream has
		 *  somewhere to declare itself rather than being an untyped string. */
		id: 'scope';
		principal: string;
		workspace: string;
		state: 'idle' | 'connecting' | 'live' | 'error' | 'closed';
		message: string;
		controller: AbortController | null;
		backoff: Backoff;
		retryTimer: ReturnType<typeof setTimeout> | null;
	}
	let streams: StreamHandle[] = [];
	let mounted = false;
	let lastScopeKey = '';

	function stopHistoryBackfill(): void {
		if (historyAbort) {
			historyAbort.abort();
			historyAbort = null;
		}
	}

	$: scope = $scopeIdentityStore;
	$: if (mounted && browser && scope.principal && scope.workspace) {
		const nextKey = `${scope.principal}::${scope.workspace}`;
		if (nextKey !== lastScopeKey) {
			lastScopeKey = nextKey;
			void reopenStreams(scope.principal, scope.workspace);
		}
	}

	function newStream(
		id: 'scope',
		principal: string,
		workspace: string
	): StreamHandle {
		return {
			id,
			principal,
			workspace,
			state: 'idle',
			message: '',
			controller: null,
			// One backoff instance per connection — state is per-instance.
			backoff: createBackoff({ initialMs: 1_000, maxMs: 60_000 }),
			retryTimer: null
		};
	}

	async function reopenStreams(principal: string, workspace: string): Promise<void> {
		closeStreams();
		stopHistoryBackfill();
		resetBuffers();
		historyNotice = 'Loading recent activity history…';
		// One stream, the operator's own scope. Undeclared activity falls back
		// to the default scope server-side rather than to system/system, so
		// there is no shared bucket left to subscribe to. See the note at the
		// top of this file.
		await loadActivityHistory(principal, workspace).catch((error) => {
			if (!mounted) return;
			if (error instanceof DOMException && error.name === 'AbortError') return;
			historyNotice = `Failed to load historical activity: ${error instanceof Error ? error.message : String(error)}`;
		});
		if (!mounted) return;
		streams = [newStream('scope', principal, workspace)];
		for (const handle of streams) void connect(handle);
	}

	function endpointUrl(handle: StreamHandle): string {
		const params = new URLSearchParams();
		params.set('category', 'activity');
		return `/api/magician/v3/events?${params.toString()}`;
	}

	async function loadActivityHistory(principal: string, workspace: string): Promise<void> {
		historyNotice = 'Loading recent activity history…';
		historyInventoryComplete = true;
		const nowMs = Date.now();
		const fromMs = nowMs - RUNTIME_HISTORY_LOOKBACK_MS - RUNTIME_HISTORY_OVERLAP_MS;
		const sql = `SELECT activity_id, parent_activity_id, name, target, kind, workload_class, principal, workspace, agent_id, thread_id, task_id, model, started_at_ms, duration_ms, outcome FROM activity_rows WHERE started_at_ms >= ${fromMs} ORDER BY started_at_ms DESC LIMIT ${RUNTIME_HISTORY_MAX_ROWS}`;
		const body = JSON.stringify({ sql });
		const controller = new AbortController();
		historyAbort = controller;
		try {
			const response = await timedFetch(ACTIVITY_HISTORY_ENDPOINT, {
				method: 'POST',
				headers: {
					'Content-Type': 'application/json',
				},
				body,
				signal: controller.signal,
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			});
			if (!mounted || historyAbort !== controller) return;
			if (!response.ok) {
				historyNotice = `Activity history load failed (${response.status})`;
				return;
			}
			const payload = (await response.json()) as ActivityRowsQueryResponse;
			if (!(payload && typeof payload === 'object')) {
				historyNotice = 'Activity history payload was not recognized';
				return;
			}
			if (Array.isArray(payload.columns) && Array.isArray(payload.rows)) {
				const records = parseActivityRowsPayload(payload);
				ingestActivityRows(records);
				historyInventoryComplete = payload.inventory_complete ?? true;
				historyNotice = null;
			} else if (Array.isArray(payload.rows)) {
				// Defensive: keep payload parity explicit even if columns are missing.
				historyNotice = 'Activity history payload was missing expected columns';
			} else {
				historyNotice = 'Activity history payload was not recognized';
			}
		} catch (error) {
			if (error instanceof DOMException && error.name === 'AbortError') return;
			throw error;
		} finally {
			if (historyAbort === controller) historyAbort = null;
		}
	}

	function parseActivityRowsPayload(payload: ActivityRowsQueryResponse): ActivityRowRecord[] {
		const columnIndex = new Map<string, number>();
		for (let index = 0; index < payload.columns.length; index += 1) {
			const name = payload.columns[index];
			if (typeof name === 'string') columnIndex.set(name, index);
		}
		const startedAtIndex = columnIndex.get('started_at_ms');
		if (startedAtIndex === undefined) return [];

		const out: ActivityRowRecord[] = [];
		for (const row of payload.rows) {
			if (!Array.isArray(row)) continue;
			const activityId = stringOrNull(row[columnIndex.get('activity_id') ?? -1]);
			if (!activityId) continue;
			const startedAtMs = numberOrNull(row[startedAtIndex]);
			const durationMs = numberOrNull(row[columnIndex.get('duration_ms') ?? -1]);
			out.push({
				activityId,
				parentActivityId: stringOrNull(row[columnIndex.get('parent_activity_id') ?? -1]),
				name: stringOrNull(row[columnIndex.get('name') ?? -1]) ?? '(unnamed span)',
				target: stringOrNull(row[columnIndex.get('target') ?? -1]) ?? '',
				kind: stringOrNull(row[columnIndex.get('kind') ?? -1]) ?? 'runtime',
				workloadClass: stringOrNull(row[columnIndex.get('workload_class') ?? -1]),
				principal: stringOrNull(row[columnIndex.get('principal') ?? -1]),
				workspace: stringOrNull(row[columnIndex.get('workspace') ?? -1]),
				agentId: stringOrNull(row[columnIndex.get('agent_id') ?? -1]),
				threadId: stringOrNull(row[columnIndex.get('thread_id') ?? -1]),
				taskId: stringOrNull(row[columnIndex.get('task_id') ?? -1]),
				model: stringOrNull(row[columnIndex.get('model') ?? -1]),
				startedAtMs,
				durationMs,
				outcome: stringOrNull(row[columnIndex.get('outcome') ?? -1])
			});
		}
		out.reverse();
		return out;
	}

	function ingestActivityRows(rows: ActivityRowRecord[]): void {
		for (const row of rows) {
			const existing = nodes.get(row.activityId);
			if (existing) {
				if (row.startedAtMs !== null) {
					existing.startedMs = row.startedAtMs;
				}
				if (!existing.name || existing.name === '(unnamed span)') {
					existing.name = row.name;
				}
				existing.target = row.target || existing.target;
				existing.kind = normalizeKind(row.kind);
				existing.workloadClass = row.workloadClass ?? existing.workloadClass;
				existing.agentId = row.agentId ?? existing.agentId;
				existing.threadId = row.threadId ?? existing.threadId;
				existing.taskId = row.taskId ?? existing.taskId;
				existing.model = row.model ?? existing.model;
				existing.principal = row.principal;
				existing.workspace = row.workspace;
				if (row.durationMs !== null && row.startedAtMs !== null) {
					existing.finished = true;
				}
				existing.finishedMs = row.startedAtMs !== null && row.durationMs !== null
					? row.startedAtMs + row.durationMs
					: existing.finishedMs;
				existing.durationMs = row.durationMs ?? existing.durationMs;
				existing.outcome = row.outcome ?? existing.outcome;
				existing.seq = seq++;
				continue;
			}
			const startedMs = row.startedAtMs;
			const durationMs = row.durationMs;
			const finishedMs = startedMs !== null && durationMs !== null ? startedMs + durationMs : null;
			nodes.set(row.activityId, {
				id: row.activityId,
				parentId: row.parentActivityId,
				name: row.name,
				target: row.target,
				kind: normalizeKind(row.kind),
				workloadClass: row.workloadClass,
				costs: [],
				agentId: row.agentId,
				threadId: row.threadId,
				taskId: row.taskId,
				model: row.model,
				operation: null,
				principal: row.principal,
				workspace: row.workspace,
				startedMs,
				finishedMs,
				durationMs,
				outcome: row.outcome ?? 'closed',
				finished: true,
				seq: seq++,
				progress: []
			});
		}
		evictIfOverCap();
		rehomeLooseProgress();
		rehomePendingCosts();
		dataVersion += 1;
	}

	async function connect(handle: StreamHandle): Promise<void> {
		if (!browser || !mounted) return;
		abortStream(handle);
		const connection = new AbortController();
		handle.controller = connection;
		setStreamState(handle, 'connecting', 'connecting…');
		try {
			const response = await timedFetch(endpointUrl(handle), {
				headers: {
					// Header wins over the query param in
					// `scope::resolve_required_scope` — the system stream must
					// carry it here, not only in the URL.
				},
				signal: connection.signal,
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			});
			if (handle.controller !== connection) return;
			if (!response.ok) {
				setStreamState(handle, 'error', `${response.status} ${response.statusText}`);
				scheduleReconnect(handle, connection);
				return;
			}
			if (!response.body) {
				setStreamState(handle, 'error', 'no response body');
				scheduleReconnect(handle, connection);
				return;
			}
			setStreamState(handle, 'live', '');
			handle.backoff.reset();
			const reader = response.body.getReader();
			const decoder = new TextDecoder('utf-8');
			let buffer = '';
			for (;;) {
				const { done, value } = await reader.read();
				if (handle.controller !== connection) return;
				if (done) {
					setStreamState(handle, 'closed', 'reconnecting');
					scheduleReconnect(handle, connection);
					return;
				}
				buffer += decoder.decode(value, { stream: true });
				let newlineIndex: number;
				while ((newlineIndex = buffer.indexOf('\n')) !== -1) {
					const line = buffer.slice(0, newlineIndex);
					buffer = buffer.slice(newlineIndex + 1);
					// Blank lines are the server's 15s keepalive.
					if (line.trim()) ingestLine(line);
				}
			}
		} catch (error) {
			if (handle.controller !== connection) return;
			if (connection.signal.aborted || isBenignStreamAbort(error)) {
				setStreamState(handle, 'closed', mounted ? 'reconnecting' : 'disconnected');
				if (mounted) scheduleReconnect(handle, connection);
				return;
			}
			setStreamState(handle, 'error', error instanceof Error ? error.message : String(error));
			scheduleReconnect(handle, connection);
		}
	}

	function setStreamState(
		handle: StreamHandle,
		state: StreamHandle['state'],
		message: string
	): void {
		handle.state = state;
		handle.message = message;
		streams = streams;
	}

	function scheduleReconnect(handle: StreamHandle, expectedConnection: AbortController): void {
		if (!mounted || handle.controller !== expectedConnection || handle.retryTimer) return;
		// Jittered, doubling. A backend restart drops every client at the
		// same instant; a fixed delay reconnects them in lockstep waves.
		const delay = handle.backoff.nextMs();
		handle.retryTimer = setTimeout(() => {
			handle.retryTimer = null;
			if (mounted && handle.controller === expectedConnection) void connect(handle);
		}, delay);
	}

	function isBenignStreamAbort(error: unknown): boolean {
		// `AbortSignal.timeout` (timedFetch's read deadline) rejects with
		// `TimeoutError`, not `AbortError`, and does not propagate to
		// `controller.signal` — treat both as benign so a healthy long-lived
		// stream does not flash an error banner before reconnecting.
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

	function abortStream(handle: StreamHandle): void {
		if (handle.retryTimer) {
			clearTimeout(handle.retryTimer);
			handle.retryTimer = null;
		}
		if (handle.controller) {
			handle.controller.abort();
			handle.controller = null;
		}
	}

	function closeStreams(): void {
		for (const handle of streams) abortStream(handle);
		streams = [];
	}

	// ─── Ingest ────────────────────────────────────────────────────────
	function ingestLine(serialized: string): void {
		// Pause freezes the RENDER, not the ingest — see `scheduleFlush`.
		// Dropping lines while paused would make the view lie by omission,
		// which is the one thing this surface is not allowed to do.
		let parsed: Record<string, unknown>;
		try {
			parsed = JSON.parse(serialized) as Record<string, unknown>;
		} catch {
			return;
		}
		const eventType = String(parsed.event_type ?? '');
		if (eventType === '__events_lagged__') {
			laggedNotice = {
				skipped: numberOr(parsed.skipped, 0),
				message: String(parsed.message ?? '')
			};
			return;
		}
		if (!ACTIVITY_EVENT_TYPES.has(eventType)) return;

		// `RuntimeTransportEvent` is `#[serde(tag = "event_type", content = "data")]`.
		const data = asRecord(parsed.data) ?? parsed;

		// Backfill and the live tail overlap by design and deliver the same
		// event twice. Deduplicate on the server's `seq` — stamped once when
		// the event is emitted, so both legs carry the same value.
		//
		// Deliberately NOT the serialized line. Two identical log messages
		// inside the same span in the same millisecond serialize identically,
		// so a content key cannot tell them apart from one event delivered
		// twice, and silently dropped the second. `seq` is per-emission, so
		// distinct events stay distinct. It is also a number rather than a
		// full JSON line, which is what makes holding `MAX_SEEN` of them
		// affordable.
		// A decimal string, not a number — the counter is seeded from a
		// 63-bit random base and a JSON number past 2^53 would lose
		// precision here, rounding two distinct events onto one key.
		// Named `eventSeq`, not `seq`: the component already has a mutable
		// `seq` counter for local row ordering, and shadowing it here would
		// break the `seq++` arrival ordering below.
		const eventSeq = stringOrNull(data.seq);
		// A row with no `seq` predates the field; fall back to the old
		// content key so replaying an older on-disk window still dedups.
		const dedupKey = eventSeq ? `${eventType}:${eventSeq}` : serialized;
		if (seenSerialized.has(dedupKey)) return;
		// Evict the OLDEST key, one at a time — a `Set` iterates in insertion
		// order, so this keeps the most recent `MAX_SEEN` keys at a fixed
		// ceiling. Clearing the set wholesale instead would forget the entire
		// overlap window at once, and every backfill row still to arrive
		// would read as new.
		if (seenSerialized.size >= MAX_SEEN) {
			const oldest = seenSerialized.values().next().value;
			if (oldest !== undefined) seenSerialized.delete(oldest);
		}
		seenSerialized.add(dedupKey);
		const timestampMs = timestampOf(data, parsed);

		// `dropped` is process-cumulative and monotonic within ONE runtime
		// process, and the backend stamps it at drain time so the newest
		// event always carries the current total. Neither a max nor a
		// decrease test can read it correctly here: backfill replays older
		// rows carrying SMALLER totals interleaved with live rows carrying
		// larger ones, so a max latches a restarted runtime's stale high
		// water mark forever, while "a decrease means a restart" fires on
		// every backfill row that lands after a live one. Take the value
		// from the newest row instead — that is the one the contract says is
		// current, and it falls back to the new process's counter naturally
		// after a restart.
		// A row with no parseable timestamp cannot be ordered against the
		// rows already seen, so it never overwrites a total that may be
		// newer. Every activity variant carries a required `timestamp`, so
		// this is the unreachable-in-practice branch rather than a real
		// source of staleness.
		const dropped = numberOr(data.dropped, 0);
		if (timestampMs !== null && timestampMs >= droppedAsOfMs) {
			droppedTotal = dropped;
			droppedAsOfMs = timestampMs;
		}

		if (eventType === 'ActivityStarted') {
			const id = stringOrNull(data.activity_id);
			if (!id) return;
			const existing = nodes.get(id);
			if (existing) {
				// Re-delivery after a dedup-set reset. Idempotent by id.
				if (existing.startedMs === null) existing.startedMs = timestampMs;
				scheduleFlush();
				return;
			}
			nodes.set(id, {
				id,
				parentId: stringOrNull(data.parent_activity_id),
				name: stringOrNull(data.name) ?? '(unnamed span)',
				target: stringOrNull(data.target) ?? '',
				kind: normalizeKind(stringOrNull(data.kind)),
				// Absent stays null. The layer already narrowed `workload_class`
				// onto its closed set server-side, so anything that arrives here
				// is either one of the nine or absent — no client-side
				// re-validation, which would only drift from the server's list.
				workloadClass: stringOrNull(data.workload_class),
				costs: [],
				agentId: stringOrNull(data.agent_id),
				threadId: stringOrNull(data.thread_id),
				taskId: stringOrNull(data.task_id),
				model: stringOrNull(data.model),
				operation: stringOrNull(data.operation),
				principal: stringOrNull(data.principal),
				workspace: stringOrNull(data.workspace),
				startedMs: timestampMs,
				finishedMs: null,
				durationMs: null,
				outcome: null,
				finished: false,
				seq: seq++,
				progress: []
			});
			scheduleFlush();
			return;
		}

		if (eventType === 'ActivityCost') {
			const id = stringOrNull(data.activity_id);
			if (!id) return;
			const commodity = stringOrNull(data.commodity);
			// A cost with no commodity cannot be displayed or totalled — the
			// number alone does not say what it counts. Dropped rather than
			// guessed at as dollars.
			if (!commodity) return;
			const cost: SpanCost = {
				microunits: numberOr(data.cost_microunits, 0),
				commodity,
				inputTokens: numberOrNull(data.input_tokens),
				outputTokens: numberOrNull(data.output_tokens)
			};
			const owner = nodes.get(id);
			if (owner) {
				accumulateCost(owner.costs, cost);
			} else {
				const parked = pendingCosts.get(id);
				if (parked) {
					accumulateCost(parked, cost);
				} else {
					pendingCosts.set(id, [cost]);
					if (pendingCosts.size > MAX_PENDING_COSTS) {
						// Oldest-first eviction: Map preserves insertion order,
						// so the first key is the longest-waiting. A cost still
						// homeless after this many arrivals is for a span that
						// was evicted before we saw it and never will be.
						// Checked only on a NEW key, because that is the only
						// branch that can grow the map.
						const oldest = pendingCosts.keys().next().value;
						if (oldest !== undefined) pendingCosts.delete(oldest);
					}
				}
			}
			scheduleFlush();
			return;
		}

		if (eventType === 'ActivityFinished') {
			const id = stringOrNull(data.activity_id);
			if (!id) return;
			const node = nodes.get(id);
			if (!node) {
				// The opening `ActivityStarted` never reached us (evicted by
				// the layer's bounded queue, or older than the backfill
				// window). Synthesise a finished-only row rather than drop
				// the evidence that the work ran.
				nodes.set(id, {
					id,
					parentId: null,
					name: '(span start not received)',
					target: '',
					kind: 'runtime',
					// `ActivityFinished` carries no dimensions — they ride the
					// start, which is exactly the event that never arrived. Null
					// is the honest value: this span lands in the undeclared lane
					// because nothing is known about it, and guessing a class
					// from its siblings would invent the one fact the missing
					// event was carrying.
					workloadClass: null,
					costs: [],
					agentId: null,
					threadId: null,
					taskId: null,
					model: null,
					operation: null,
					principal: stringOrNull(data.principal),
					workspace: stringOrNull(data.workspace),
					startedMs: null,
					finishedMs: timestampMs,
					durationMs: numberOr(data.duration_ms, 0),
					outcome: stringOrNull(data.outcome),
					finished: true,
					seq: seq++,
					progress: []
				});
				scheduleFlush();
				return;
			}
			node.finished = true;
			node.finishedMs = timestampMs;
			node.durationMs = numberOr(data.duration_ms, 0);
			node.outcome = stringOrNull(data.outcome);
			scheduleFlush();
			return;
		}

		// ActivityProgress — a log line, inside a span or below the span floor.
		const activityId = stringOrNull(data.activity_id);
		const row: ProgressRow = {
			key: `p${seq}`,
			activityId,
			level: (stringOrNull(data.level) ?? 'info').toLowerCase(),
			message: stringOrNull(data.message) ?? '',
			target: stringOrNull(data.target) ?? '',
			principal: stringOrNull(data.principal),
			workspace: stringOrNull(data.workspace),
			timestampMs,
			seq: seq++
		};
		const owner = activityId ? nodes.get(activityId) : undefined;
		if (owner) {
			owner.progress.push(row);
			if (owner.progress.length > MAX_PROGRESS_PER_NODE) {
				owner.progress.splice(0, owner.progress.length - MAX_PROGRESS_PER_NODE);
			}
		} else {
			// No `activity_id`, or an id we hold no span for. Both render at
			// the root. The design accepts a span floor and the honesty IS
			// showing these — do not filter them out to tidy the tree.
			looseProgress.push(row);
			if (looseProgress.length > MAX_LOOSE_PROGRESS) {
				looseProgress = looseProgress.slice(-MAX_LOOSE_PROGRESS);
			}
		}
		scheduleFlush();
	}

	/** Pause freezes the RENDER, never the housekeeping.
	 *
	 *  Ingest deliberately keeps running while paused, and the per-node and
	 *  loose-progress caps are enforced inline at ingest — but `nodes` is
	 *  capped only by `evictIfOverCap`, which used to live behind the same
	 *  early return as the render. A paused tab therefore accumulated span
	 *  nodes without any ceiling for as long as it stayed paused: on a busy
	 *  runtime, pausing and walking away was an unbounded leak.
	 *
	 *  So the timer always runs and always evicts; only `dataVersion` — the
	 *  single value the rendered rows derive from — is withheld while
	 *  paused, which is what actually holds the view still. */
	function scheduleFlush(): void {
		if (flushTimer) return;
		flushTimer = setTimeout(() => {
			flushTimer = null;
			rehomeLooseProgress();
			rehomePendingCosts();
			evictIfOverCap();
			if (!paused) dataVersion += 1;
		}, FLUSH_MS);
	}

	/** Add one arriving cost to a per-commodity total.
	 *
	 *  Commodities are accumulated separately and NEVER added together: `usd`
	 *  is money, `local` is machine time, `tavily_credit` is a vendor's own
	 *  unit. Overwriting instead of accumulating is what made a span that spent
	 *  four times report the price of its fourth call. */
	function accumulateCost(into: SpanCost[], cost: SpanCost): void {
		const existing = into.find((entry) => entry.commodity === cost.commodity);
		if (existing) {
			existing.microunits += cost.microunits;
			existing.inputTokens = addTokens(existing.inputTokens, cost.inputTokens);
			existing.outputTokens = addTokens(existing.outputTokens, cost.outputTokens);
			return;
		}
		into.push(cost);
		// Sorted by name so a row does not reshuffle as a second commodity
		// arrives — matching the header's ordering rule.
		into.sort((left, right) => left.commodity.localeCompare(right.commodity));
	}

	/** Sum two possibly-unknown token counts.
	 *
	 *  Absent stays absent only while BOTH are absent. One provider reporting
	 *  tokens and another not is "at least this many", which is worth showing;
	 *  treating the unreported one as 0 and the pair as exact is not. */
	function addTokens(left: number | null, right: number | null): number | null {
		if (left === null) return right;
		if (right === null) return left;
		return left + right;
	}

	/** Attach costs whose span has since arrived. Mirrors
	 *  `rehomeLooseProgress`; runs on the same flush. */
	function rehomePendingCosts(): void {
		if (pendingCosts.size === 0) return;
		for (const [id, costs] of pendingCosts) {
			const owner = nodes.get(id);
			if (!owner) continue;
			for (const cost of costs) accumulateCost(owner.costs, cost);
			pendingCosts.delete(id);
		}
	}

	/** A progress line can land before the `ActivityStarted` that owns it —
	 *  the two variants are independent records on the same queue. Such a
	 *  line parks in `looseProgress`; once its span shows up, move it under
	 *  the span instead of leaving it stranded at the root (or, worse,
	 *  filtering it out of both places at render time). Lines whose span
	 *  never arrives stay loose forever, which is the correct outcome. */
	function rehomeLooseProgress(): void {
		if (looseProgress.length === 0) return;
		let moved = false;
		const stillLoose: ProgressRow[] = [];
		for (const row of looseProgress) {
			const owner = row.activityId ? nodes.get(row.activityId) : undefined;
			if (!owner) {
				stillLoose.push(row);
				continue;
			}
			owner.progress.push(row);
			if (owner.progress.length > MAX_PROGRESS_PER_NODE) {
				owner.progress.splice(0, owner.progress.length - MAX_PROGRESS_PER_NODE);
			}
			moved = true;
		}
		if (moved) looseProgress = stillLoose;
	}

	/** Drop whole root subtrees, oldest first, until under the node cap.
	 *  Evicting a parent while keeping its children would manufacture
	 *  orphans that look like dropped-parent evidence. */
	function evictIfOverCap(): void {
		if (nodes.size <= MAX_NODES) return;
		const childIds = new Map<string, string[]>();
		for (const node of nodes.values()) {
			if (node.parentId && nodes.has(node.parentId)) {
				const bucket = childIds.get(node.parentId);
				if (bucket) bucket.push(node.id);
				else childIds.set(node.parentId, [node.id]);
			}
		}
		const roots = [...nodes.values()]
			.filter((node) => !node.parentId || !nodes.has(node.parentId))
			.sort((left, right) => left.seq - right.seq);
		for (const root of roots) {
			if (nodes.size <= MAX_NODES) break;
			const stack = [root.id];
			while (stack.length > 0) {
				const id = stack.pop() as string;
				const kids = childIds.get(id);
				if (kids) stack.push(...kids);
				nodes.delete(id);
				expanded.delete(id);
			}
		}
		expanded = expanded;
	}

	/** The one subject the view is pinned to, or `null` for everything.
	 *
	 *  Applied at the single point every tier reads from — `spanVisible` is
	 *  called by `buildLanes`, `headerStats` and the row builder, so lanes,
	 *  header and stream cannot disagree about what is being shown. A filter
	 *  that applied to only one tier would be worse than none: the header would
	 *  report totals for work the stream below was not showing.
	 *
	 *  Inherited ids make this meaningful — pinning an agent keeps every span
	 *  that agent's run opened, not just the root that declared it.
	 *
	 *  Declared above `resetBuffers`, its first use, rather than beside
	 *  `matchesSubject` where it reads most naturally. `let` bindings are in the
	 *  temporal dead zone until their declaration runs, so a `resetBuffers` call
	 *  during instance setup — one `$:` statement moved above it, one eager
	 *  subscription — would throw instead of clearing the pin. It is safe today
	 *  only because every caller is an event handler. */
	let subjectFilter: { kind: 'agent' | 'thread' | 'task'; id: string } | null = null;

	/** Empty every buffer that outlives a window.
	 *
	 *  Both callers wipe the whole retained window — the Clear button, and a
	 *  scope switch — so anything left behind here belongs to spans that no
	 *  longer exist.
	 *
	 *  `pendingCosts` was the one that was missed, and it is the one that could
	 *  produce a wrong number rather than a stale one: a cost parked before the
	 *  reset stayed parked, and `rehomePendingCosts` attached it to the next
	 *  span that happened to reuse the id — spend from the old window, reported
	 *  against a new node, in a different scope after a scope switch.
	 *
	 *  `subjectFilter` goes too. A pin names an agent, thread or task that the
	 *  reset has just removed every row for, so leaving it set renders an empty
	 *  view that looks like a quiet runtime. */
	function resetBuffers(): void {
		nodes.clear();
		looseProgress = [];
		pendingCosts.clear();
		seenSerialized = new Set();
		expanded = new Set();
		subjectFilter = null;
		droppedTotal = 0;
		droppedAsOfMs = -Infinity;
		laggedNotice = null;
		historyNotice = null;
		historyInventoryComplete = true;
		stopHistoryBackfill();
		seq = 0;
		dataVersion += 1;
	}

	function normalizeKind(kind: string | null): string {
		if (!kind) return 'runtime';
		const lowered = kind.toLowerCase();
		return lowered in KIND_META && lowered !== LOOSE_KIND ? lowered : 'runtime';
	}

	function asRecord(value: unknown): Record<string, unknown> | null {
		if (value === null || typeof value !== 'object' || Array.isArray(value)) return null;
		return value as Record<string, unknown>;
	}

	function stringOrNull(value: unknown): string | null {
		if (typeof value !== 'string') return null;
		const trimmed = value.trim();
		return trimmed.length > 0 ? trimmed : null;
	}

	function numberOr(value: unknown, fallback: number): number {
		return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
	}

	/** Absent stays absent. Distinct from `numberOr(v, 0)`: a token count the
	 *  provider did not report is unknown, and rendering it as 0 would claim
	 *  the call consumed nothing. */
	function numberOrNull(value: unknown): number | null {
		return typeof value === 'number' && Number.isFinite(value) ? value : null;
	}

	/** Activity events stamp `data.timestamp` in epoch millis
	 *  (`runtime_activity_layer::now_ms`). Backfilled lines can also carry a
	 *  top-level `timestamp_ms`. */
	function timestampOf(
		data: Record<string, unknown>,
		envelope: Record<string, unknown>
	): number | null {
		for (const candidate of [data.timestamp, data.timestamp_ms, envelope.timestamp_ms]) {
			if (typeof candidate === 'number' && Number.isFinite(candidate) && candidate !== 0) {
				return candidate < 10_000_000_000 ? candidate * 1000 : candidate;
			}
		}
		return null;
	}

	// ─── Tree build ────────────────────────────────────────────────────
	$: flatRows = buildRows(dataVersion, expanded, kindEnabled);
	// `nodes` is a plain Map mutated outside Svelte's reactivity, so every
	// derived count reads through `dataVersion` — the one reactive trigger.
	$: heldSpans = countHeld(dataVersion);
	$: anyLive = streams.some((handle) => handle.state === 'live');
	// Lanes and header take `kindEnabled` as an argument, not just to re-derive
	// when a chip flips, but because they MUST apply it — see `spanVisible`.
	$: lanes = buildLanes(dataVersion, kindEnabled);
	$: stats = headerStats(dataVersion, kindEnabled);

	/** Per-lane cap on the finished tail. The lane answers "what is happening",
	 *  the stream below is the complete record — a lane that grows to match it
	 *  stops being glanceable and starts being a second stream. */
	const LANE_RECENT_LIMIT = 5;

	/** Lane order: declared classes in the server's own order, then undeclared
	 *  last. Fixed rather than sorted by volume, because a lane that moves when
	 *  traffic shifts cannot be found by muscle memory.
	 *
	 *  The keys are the **wire values**, snake_case, exactly as
	 *  `ACTIVITY_WORKLOAD_CLASSES` in `analytics/runtime_activity_layer.rs`
	 *  spells them — which is in turn exactly what `LlmWorkloadClass::as_str()`
	 *  returns, so a lane key and the `workload_class` parquet column are the
	 *  same string. Display text is derived from the key by `laneLabel` rather
	 *  than kept in a second table, so there is no label list to drift against
	 *  this one. */
	const LANE_ORDER = [
		'foreground_chat',
		'interactive_task',
		'comms_assist',
		'autonomous_task',
		'scheduled',
		'ambient',
		'memory',
		'evaluation',
		'system'
	];

	const UNDECLARED_LANE = '(undeclared)';

	function laneLabel(key: string): string {
		if (key === UNDECLARED_LANE) return 'Undeclared';
		const spaced = key.replace(/_/g, ' ');
		return spaced.charAt(0).toUpperCase() + spaced.slice(1);
	}

	function matchesSubject(node: SpanNode): boolean {
		if (!subjectFilter) return true;
		const held =
			subjectFilter.kind === 'agent'
				? node.agentId
				: subjectFilter.kind === 'thread'
					? node.threadId
					: node.taskId;
		return held === subjectFilter.id;
	}

	/** THE ONE GATE. Every tier asks this and nothing else.
	 *
	 *  The kind chips used to be stream-only: `buildRows` read `kindEnabled`
	 *  and `buildLanes` and `headerStats` did not. Turning off the `llm` chip
	 *  therefore removed those spans from the stream while the header went on
	 *  reporting their spend, their p95 and their error count, and the lanes
	 *  went on listing them — the page's own documented invariant ("all three
	 *  read the same retained window and apply the same filters") broken by the
	 *  most visible control on it.
	 *
	 *  Both halves live here so a third filter cannot be added to one tier
	 *  again. */
	function spanVisible(node: SpanNode, enabled: Record<string, boolean>): boolean {
		return matchesSubject(node) && enabled[node.kind] !== false;
	}

	function pinSubject(kind: 'agent' | 'thread' | 'task', id: string | null): void {
		if (!id) return;
		subjectFilter =
			subjectFilter && subjectFilter.kind === kind && subjectFilter.id === id
				? null
				: { kind, id };
		// `nodes` is mutated outside Svelte's reactivity, so every derived
		// value reads through `dataVersion` — bump it or the filter changes
		// nothing on screen.
		dataVersion += 1;
	}

	interface Lane {
		key: string;
		label: string;
		undeclared: boolean;
		running: SpanNode[];
		recent: SpanNode[];
	}

	/** Group retained spans into lanes by workload class.
	 *
	 *  Reads through `dataVersion` like every other derived value — `nodes` is
	 *  a plain Map mutated outside Svelte's reactivity.
	 *
	 *  A lane is emitted for every class that has *any* span in the window,
	 *  running or finished, plus undeclared when present. An empty lane whose
	 *  class exists is kept rather than dropped: "nothing is running in
	 *  Ambient right now" is information, and a lane that vanishes makes the
	 *  row reflow under the pointer.
	 */
	function buildLanes(_version: number, enabled: Record<string, boolean>): Lane[] {
		const running = new Map<string, SpanNode[]>();
		const recent = new Map<string, SpanNode[]>();

		for (const node of nodes.values()) {
			if (!spanVisible(node, enabled)) continue;
			const key = node.workloadClass ?? UNDECLARED_LANE;
			const bucket = node.finished ? recent : running;
			let list = bucket.get(key);
			if (!list) {
				list = [];
				bucket.set(key, list);
			}
			list.push(node);
		}

		// Kind is a sort key inside the lane, not a second level of nesting.
		//
		// The plan called for grouping by workload class "then kind". Nesting
		// was the wrong shape for it: measured concurrency is ~8 open spans
		// across all lanes, so sub-headers would wrap lists of one to three
		// items in a hierarchy taller than its contents. Ordering by kind
		// clusters the same families together and keeps the lane one flat,
		// scannable list. Revisit if a single lane routinely holds enough
		// rows that clustering stops being legible on its own.
		//
		// Oldest-first within a kind: a span open a long time is the one worth
		// looking at, and newest-first would bury it under whatever just
		// started.
		for (const list of running.values()) {
			list.sort(
				(a, b) =>
					a.kind.localeCompare(b.kind) || (a.startedMs ?? 0) - (b.startedMs ?? 0)
			);
		}
		// Newest-first among the finished, truncated to five — the tail is
		// context, not history.
		//
		// Selected rather than sorted. A lane can hold thousands of finished
		// spans out of the retained window and this runs on every 150ms flush,
		// so a full O(n log n) sort to display five was paying for 99.9% of an
		// ordering nobody sees.
		for (const [key, list] of recent) {
			recent.set(key, newestFinished(list, LANE_RECENT_LIMIT));
		}

		const present = new Set([...running.keys(), ...recent.keys()]);
		const ordered = LANE_ORDER.filter((key) => present.has(key));
		// A class the server knows and this list does not still gets a lane,
		// after the known ones and before undeclared. Dropping it would hide
		// live work behind a client-side list that had drifted — silent loss,
		// which is the one thing this surface is not allowed to do. Sorted so
		// the extra lanes at least hold still between renders.
		ordered.push(
			...[...present]
				.filter((key) => key !== UNDECLARED_LANE && !LANE_ORDER.includes(key))
				.sort()
		);
		if (present.has(UNDECLARED_LANE)) ordered.push(UNDECLARED_LANE);

		return ordered.map((key) => ({
			key,
			label: laneLabel(key),
			undeclared: key === UNDECLARED_LANE,
			running: running.get(key) ?? [],
			recent: recent.get(key) ?? []
		}));
	}

	/** The `limit` most recently finished spans, newest first.
	 *
	 *  A bounded insertion rather than a sort: `limit` is 5 and the input can
	 *  be thousands, so this is O(n·limit) with a tiny constant against a full
	 *  comparison sort, on a path that reruns 6.7 times a second. */
	function newestFinished(list: SpanNode[], limit: number): SpanNode[] {
		const top: SpanNode[] = [];
		for (const node of list) {
			const key = node.finishedMs ?? 0;
			if (top.length === limit && key <= (top[limit - 1].finishedMs ?? 0)) continue;
			let index = top.length;
			while (index > 0 && (top[index - 1].finishedMs ?? 0) < key) index -= 1;
			top.splice(index, 0, node);
			if (top.length > limit) top.pop();
		}
		return top;
	}

	/** Spans held in memory, deliberately NOT filtered at all: this is the
	 *  retention figure — how full the window is — not a count of the work on
	 *  screen. `stats.running` is the filtered one. */
	function countHeld(_version: number): number {
		return nodes.size;
	}

	interface HeaderStats {
		running: number;
		p50Ms: number | null;
		p95Ms: number | null;
		errors: number;
		/** Spans that ended without declaring an outcome. Counted apart from
		 *  errors AND apart from successes on purpose — see below. */
		closed: number;
		/** Total per commodity, kept SEPARATE. `usd` and `local` and
		 *  `tavily_credit` are not addable — one is money, one is machine time,
		 *  one is a vendor's own unit. A single "total spend" figure would have
		 *  to pick one and silently misreport the rest. */
		spend: Array<{ commodity: string; microunits: number }>;
		eventsPerSec: number | null;
	}

	/** Header aggregates, one pass over the retained window.
	 *
	 *  Everything here derives from data already on the wire: `duration_ms`
	 *  and `outcome` arrive on `ActivityFinished`, so none of this costs a
	 *  request.
	 *
	 *  **`closed` is never counted as success.** It is the honest default,
	 *  meaning the layer watched a span end without the span declaring how it
	 *  went — the event's own documentation says treating it as healthy would
	 *  make every abandoned unit of work look fine. It gets its own number so
	 *  a rising count of undeclared endings is visible rather than absorbed
	 *  into a reassuring success rate. For the same reason there is no
	 *  "success rate" here at all: any such figure would have to decide what
	 *  `closed` means, and the honest answer is that it does not know.
	 */
	function headerStats(_version: number, enabled: Record<string, boolean>): HeaderStats {
		const durations: number[] = [];
		const spend = new Map<string, number>();
		let matched = 0;
		let running = 0;
		let errors = 0;
		let closed = 0;
		let earliest: number | null = null;
		let latest: number | null = null;

		for (const node of nodes.values()) {
			// Same gate the lanes and the stream use — subject pin AND kind
			// chips — so the header can never report totals for work the rows
			// below are hiding.
			if (!spanVisible(node, enabled)) continue;
			matched += 1;
			if (!node.finished) {
				running += 1;
			} else {
				if (node.durationMs !== null) durations.push(node.durationMs);
				if (node.outcome === 'error') errors += 1;
				else if (node.outcome === 'closed' || node.outcome === null) closed += 1;
			}
			for (const entry of node.costs) {
				spend.set(entry.commodity, (spend.get(entry.commodity) ?? 0) + entry.microunits);
			}
			if (node.startedMs !== null) {
				if (earliest === null || node.startedMs < earliest) earliest = node.startedMs;
				if (latest === null || node.startedMs > latest) latest = node.startedMs;
			}
		}

		durations.sort((a, b) => a - b);

		// Rate over the window the retained spans actually cover, not over
		// wall-clock since mount: a view opened during a quiet minute would
		// otherwise report a rate diluted by time it never observed.
		//
		// Numerator and window must come from the SAME set of spans. Dividing
		// the whole map by a pinned subject's window reported a rate for work
		// the rest of the header was excluding — and the narrower the pin, the
		// more wildly it overstated.
		const spanMs = earliest !== null && latest !== null ? latest - earliest : 0;
		const eventsPerSec = spanMs > 1000 ? (matched / spanMs) * 1000 : null;

		return {
			running,
			p50Ms: percentile(durations, 0.5),
			p95Ms: percentile(durations, 0.95),
			errors,
			closed,
			// Sorted by name, not by size: a row that reorders as spend shifts
			// cannot be read at a glance.
			spend: [...spend.entries()]
				.map(([commodity, microunits]) => ({ commodity, microunits }))
				.sort((a, b) => a.commodity.localeCompare(b.commodity)),
			eventsPerSec
		};
	}

	/** Microunits are millionths. `local` renders as a dash rather than a fake
	 *  price — it has no monetary value and showing "0.000000" would imply one.
	 *
	 *  **A non-zero amount never renders as zero.** A flat `toFixed(4)` turned
	 *  every real cost below $0.00005 into `0.0000`, which is the one thing
	 *  this view is not allowed to say: a zero claims the work was free, and
	 *  "priced but tiny" is not free. Microunits are integers, so six decimals
	 *  is exact for any value the wire can carry — the smallest non-zero amount
	 *  is 1 microunit and renders as `0.000001`. A genuine zero is written `0`,
	 *  short and unmistakable, so the two cannot be confused for each other. */
	function formatSpend(entry: { commodity: string; microunits: number }): string {
		if (entry.commodity === 'local') return '—';
		if (entry.microunits === 0) return '0';
		const units = entry.microunits / 1_000_000;
		if (units >= 1) return units.toFixed(2);
		if (units >= 0.0001) return units.toFixed(4);
		return units.toFixed(6);
	}

	/** Hover text for one commodity's total on a span.
	 *
	 *  Where the token counts surface. They were parsed off the wire and stored
	 *  and then never rendered anywhere, which made them cost without paying:
	 *  the one question a price alone cannot answer is whether it was a large
	 *  call or an expensive model, and the tokens are the answer. Absent counts
	 *  stay absent — a provider that reported none is unknown, not zero. */
	function costTitle(entry: SpanCost): string {
		const parts = [
			entry.commodity === 'local'
				? 'ran on local hardware — no vendor, no price'
				: `${formatSpend(entry)} ${entry.commodity}`
		];
		if (entry.inputTokens !== null) parts.push(`${entry.inputTokens.toLocaleString()} in`);
		if (entry.outputTokens !== null) parts.push(`${entry.outputTokens.toLocaleString()} out`);
		return parts.join(' · ');
	}

	/** Nearest-rank percentile over an already-sorted array. */
	function percentile(sorted: number[], fraction: number): number | null {
		if (sorted.length === 0) return null;
		const rank = Math.ceil(fraction * sorted.length);
		return sorted[Math.min(sorted.length - 1, Math.max(0, rank - 1))];
	}

	function buildRows(
		_version: number,
		expandedSet: Set<string>,
		enabled: Record<string, boolean>
	): FlatRow[] {
		const childIds = new Map<string, SpanNode[]>();
		const roots: SpanNode[] = [];
		for (const node of nodes.values()) {
			// The same gate the lanes and header apply — subject pin AND kind
			// chips. Because ids are inherited, pinning an agent keeps that
			// agent's whole subtree rather than orphaning its children: a child
			// holds the id its root declared, so it passes this test on its own.
			if (!spanVisible(node, enabled)) continue;
			const parent = node.parentId ? nodes.get(node.parentId) : undefined;
			// A row whose parent is present AND also in view nests under it. A
			// row naming a parent we do not hold — or one a filter took away —
			// renders at the root rather than disappearing.
			//
			// The parent check is what stops the loss, and it is what lets the
			// kind chips be per-span rather than per-subtree: turning off
			// `agent` no longer hides an agent step's LLM calls, it promotes
			// them to the root. The previous rule (show a root when ANY kind in
			// its subtree is enabled) avoided that loss by leaving every
			// descendant of a shown root rendered regardless of its own kind,
			// which is precisely what made the chips disagree with the header.
			if (parent && spanVisible(parent, enabled)) {
				const bucket = childIds.get(parent.id);
				if (bucket) bucket.push(node);
				else childIds.set(parent.id, [node]);
			} else {
				roots.push(node);
			}
		}
		for (const bucket of childIds.values()) bucket.sort(orderAscending);

		// `rehomeLooseProgress` has already moved everything it could; what
		// remains named a span that never arrived, or named none at all.
		// Both belong at the root — that is the span floor, and showing it
		// is the point.
		//
		// Except while a subject is pinned. A loose row carries no agent,
		// thread or task at all, so it can never be the pinned subject's work;
		// leaving it in put rows in the stream that the header and the lanes
		// had already excluded, which is the disagreement the pin exists to
		// avoid. Nothing is hidden — clearing the pin brings them straight
		// back, and the pin chip is always visible while one is set.
		const looseRows = subjectFilter ? [] : looseProgress;

		type RootEntry =
			| { type: 'span'; node: SpanNode; sortKey: number }
			| { type: 'progress'; row: ProgressRow; sortKey: number };
		const entries: RootEntry[] = [];
		for (const node of roots) {
			entries.push({ type: 'span', node, sortKey: node.startedMs ?? node.finishedMs ?? 0 });
		}
		if (enabled[LOOSE_KIND] !== false) {
			for (const row of looseRows) {
				entries.push({ type: 'progress', row, sortKey: row.timestampMs ?? 0 });
			}
		}
		// Newest first at the root — in-flight work stays at the top.
		entries.sort((left, right) => {
			if (right.sortKey !== left.sortKey) return right.sortKey - left.sortKey;
			return entrySeq(right) - entrySeq(left);
		});

		const out: FlatRow[] = [];
		for (const entry of entries) {
			if (out.length >= MAX_RENDERED_ROWS) break;
			if (entry.type === 'progress') {
				out.push({
					type: 'progress',
					key: entry.row.key,
					depth: 0,
					row: entry.row,
					loose: true
				});
			} else {
				pushSpan(out, entry.node, 0, childIds, expandedSet);
			}
		}
		return out;
	}

	function entrySeq(
		entry: { type: 'span'; node: SpanNode } | { type: 'progress'; row: ProgressRow }
	): number {
		return entry.type === 'span' ? entry.node.seq : entry.row.seq;
	}

	function orderAscending(left: SpanNode, right: SpanNode): number {
		const leftKey = left.startedMs ?? 0;
		const rightKey = right.startedMs ?? 0;
		if (leftKey !== rightKey) return leftKey - rightKey;
		return left.seq - right.seq;
	}

	function pushSpan(
		out: FlatRow[],
		node: SpanNode,
		depth: number,
		childIds: Map<string, SpanNode[]>,
		expandedSet: Set<string>
	): void {
		if (out.length >= MAX_RENDERED_ROWS) return;
		const children = childIds.get(node.id) ?? [];
		const expandable = children.length > 0 || node.progress.length > 0;
		const isExpanded = expandedSet.has(node.id);
		out.push({
			type: 'span',
			key: node.id,
			depth,
			node,
			childCount: children.length,
			lineCount: node.progress.length,
			expandable,
			expanded: isExpanded,
			orphan: node.parentId !== null && !nodes.has(node.parentId)
		});
		if (!expandable || !isExpanded) return;

		// Inside a span, read the trace in the order it happened —
		// child spans and log lines interleaved chronologically.
		const merged: Array<{ ts: number; seq: number; child?: SpanNode; line?: ProgressRow }> = [];
		for (const child of children) {
			merged.push({ ts: child.startedMs ?? 0, seq: child.seq, child });
		}
		for (const line of node.progress) {
			merged.push({ ts: line.timestampMs ?? 0, seq: line.seq, line });
		}
		merged.sort((left, right) => (left.ts - right.ts) || (left.seq - right.seq));
		for (const item of merged) {
			if (out.length >= MAX_RENDERED_ROWS) return;
			if (item.child) {
				pushSpan(out, item.child, depth + 1, childIds, expandedSet);
			} else if (item.line) {
				out.push({
					type: 'progress',
					key: item.line.key,
					depth: depth + 1,
					row: item.line,
					loose: false
				});
			}
		}
	}

	// ─── Interaction ───────────────────────────────────────────────────
	function toggleExpanded(id: string): void {
		if (expanded.has(id)) expanded.delete(id);
		else expanded.add(id);
		expanded = expanded;
	}

	function expandAll(): void {
		const next = new Set<string>();
		for (const node of nodes.values()) next.add(node.id);
		expanded = next;
	}

	function collapseAll(): void {
		expanded = new Set();
	}

	function toggleKind(kind: string): void {
		kindEnabled = { ...kindEnabled, [kind]: kindEnabled[kind] === false };
	}

	function clearRows(): void {
		resetBuffers();
	}

	function togglePaused(): void {
		paused = !paused;
		// Everything that arrived while frozen is already in the buffers;
		// resuming just lets the tree redraw. The clock is caught up in the
		// same breath, so a resumed view does not show durations frozen at the
		// moment of the pause until the next tick.
		if (!paused) {
			now = Date.now();
			scheduleFlush();
		}
	}

	// ─── Formatting ────────────────────────────────────────────────────
	function formatClock(timestampMs: number | null): string {
		if (timestampMs === null) return '--:--:--';
		return new Date(timestampMs).toLocaleTimeString(undefined, {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit',
			hour12: false
		});
	}

	function formatDuration(ms: number): string {
		if (ms < 1000) return `${Math.round(ms)}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
		const totalSeconds = Math.floor(ms / 1000);
		return `${Math.floor(totalSeconds / 60)}m ${totalSeconds % 60}s`;
	}

	function spanTiming(node: SpanNode, nowMs: number): string {
		if (node.finished && node.durationMs !== null) return formatDuration(node.durationMs);
		if (node.startedMs !== null) return `${formatDuration(Math.max(0, nowMs - node.startedMs))}…`;
		return '—';
	}

	/** `closed` is the DEFAULT outcome: the layer saw the span end, not
	 *  succeed. It must not read as a success. */
	function outcomeTone(outcome: string | null): 'error' | 'warn' | 'success' | 'neutral' {
		if (!outcome) return 'neutral';
		const lowered = outcome.toLowerCase();
		if (lowered.includes('error') || lowered.includes('fail')) return 'error';
		if (lowered.includes('cancel') || lowered.includes('abort')) return 'warn';
		if (lowered.includes('success') || lowered.includes('ok')) return 'success';
		return 'neutral';
	}

	function levelTone(level: string): 'error' | 'warn' | 'info' | 'debug' {
		const lowered = level.toLowerCase();
		if (lowered.startsWith('error') || lowered.startsWith('fatal')) return 'error';
		if (lowered.startsWith('warn')) return 'warn';
		if (lowered.startsWith('debug') || lowered.startsWith('trace')) return 'debug';
		return 'info';
	}

	/** Every activity row carries a scope now — the layer falls back to the
	 *  default scope rather than emitting nothing — so a missing pair means
	 *  a row persisted before that change. Label it as unknown rather than
	 *  guessing a bucket for it; naming the wrong one is how the old
	 *  system/system conflation read as fact. */
	function scopeLabel(principal: string | null, workspace: string | null): string {
		if (!principal && !workspace) return 'unknown';
		return `${principal ?? '?'}/${workspace ?? '?'}`;
	}

	function shortTarget(target: string): string {
		if (!target) return '';
		const parts = target.split('::');
		return parts.length <= 2 ? target : `…::${parts.slice(-2).join('::')}`;
	}

	onMount(() => {
		mounted = true;
		clockTimer = setInterval(() => {
			// Frozen while paused, for two reasons.
			//
			// CORRECTNESS FIRST. `flatRows` is held still while paused, but the
			// `SpanNode` objects inside those rows keep being mutated by ingest.
			// `now` is the only thing that re-evaluates a row's expressions, so
			// a tick would re-run `spanTiming(row.node, now)` — reading the
			// span's *final* duration off the mutated node — while the
			// `{#if row.node.finished}` beside it stayed on the frozen value and
			// went on saying `running`. Pause is supposed to hold a picture
			// still; it was leaking half of a later one.
			//
			// And cost: this re-evaluates every rendered row's expressions, up
			// to `MAX_RENDERED_ROWS` of them, once a second — for a view whose
			// whole point at that moment is that it is not moving.
			if (paused) return;
			now = Date.now();
		}, 1000);
	});

	onDestroy(() => {
		mounted = false;
		closeStreams();
		stopHistoryBackfill();
		if (flushTimer) {
			clearTimeout(flushTimer);
			flushTimer = null;
		}
		if (clockTimer) {
			clearInterval(clockTimer);
			clockTimer = null;
		}
	});
</script>

<section class="activity-view">
	<header class="head">
		<div class="head__title">
			<p class="head-kicker">Runtime</p>
			<div class="head__title-row">
				<h1>Runtime activity</h1>
				<div class="streams" role="status">
					{#each streams as handle (handle.id)}
						<span class="stream stream--{handle.state}" title={handle.message}>
							<span class="stream__dot" aria-hidden="true"></span>
							<span class="stream__scope">{handle.principal}/{handle.workspace}</span>
							<span class="stream__state">{handle.state}</span>
						</span>
					{/each}
				</div>
			</div>
			<p class="head__sub">
				Every unit of work in flight — agent steps, background passes, LLM dispatch,
				governed child processes — in one tree.
			</p>
		</div>
		<div class="head__right">
			<div class="actions">
				<a
					class="action-button action-button--outline action-button--sm"
					href="/runtime/resources"
					title="View process-local pressure signals and resource limits"
				>
					<Icon name="sliders" size={13} />
					<span>Resources</span>
				</a>
				<button
					type="button"
					class="action-button action-button--outline action-button--sm"
					class:action-button--active={paused}
					on:click={togglePaused}
					title={paused ? 'Resume activity stream' : 'Pause activity stream'}
				>
					<Icon name={paused ? 'play' : 'pause'} size={13} />
					<span>{paused ? 'Resume' : 'Pause'}</span>
				</button>
				<button
					type="button"
					class="action-button action-button--outline action-button--sm"
					on:click={expandAll}
					title="Expand all tree rows"
				>
					<Icon name="chevron-down" size={13} />
					<span>Expand all</span>
				</button>
				<button
					type="button"
					class="action-button action-button--outline action-button--sm"
					on:click={collapseAll}
					title="Collapse all tree rows"
				>
					<Icon name="chevron-up" size={13} />
					<span>Collapse</span>
				</button>
				<button
					type="button"
					class="action-button action-button--outline action-button--sm"
					on:click={clearRows}
					title="Clear retained in-memory rows"
				>
					<Icon name="rotate-ccw" size={13} />
					<span>Clear</span>
				</button>
			</div>
		</div>
	</header>

	{#if droppedTotal > 0}
		<!-- Requirement, not decoration: the layer's queue is bounded and
		     drops oldest under load. A view that silently omits rows is
		     worse than one that admits it. -->
		<p class="notice notice--dropped">
			The runtime's bounded activity queue dropped
			<strong>{droppedTotal.toLocaleString()}</strong>
			{droppedTotal === 1 ? 'record' : 'records'} before
			{droppedTotal === 1 ? 'it' : 'they'} could be emitted. This tree is incomplete —
			spans may be missing their start, their finish, or their children.
		</p>
	{/if}

	{#if laggedNotice}
		<p class="notice notice--lagged">
			The live event channel dropped
			<strong>{laggedNotice.skipped.toLocaleString()}</strong>
			events in transit to this tab. {laggedNotice.message}
		</p>
	{/if}

	{#if historyNotice}
		<p class="notice notice--history">{historyNotice}</p>
	{/if}

	{#if !historyInventoryComplete && !historyNotice}
		<p class="notice notice--history">
			Recent activity history may be incomplete: durable activity partitions for the
			query window were not fully committed.
		</p>
	{/if}

	<div class="legend" role="group" aria-label="Filter by activity kind">
		{#each FILTER_KINDS as kind (kind)}
			{@const meta = kindMeta(kind)}
			<button
				type="button"
				class="chip"
				class:chip--off={kindEnabled[kind] === false}
				style="--chip-color: {meta.color}"
				aria-pressed={kindEnabled[kind] !== false}
				on:click={() => toggleKind(kind)}
			>
				<span class="chip__glyph" aria-hidden="true">{meta.glyph}</span>
				<span>{meta.label}</span>
			</button>
		{/each}
		<span class="legend__note">
			<strong>unspanned</strong> = log lines outside spans (span floor, shown at root).
		</span>
	</div>

	<div class="statbar">
		<!-- The FILTERED count. While a subject is pinned this must agree with
		     the rows below; the unfiltered total is `held`, which is about the
		     retention window rather than about what is on screen. -->
		<span class="metric">
			<span class="metric__value">{stats.running.toLocaleString()}</span>
			<span class="metric__label">running</span>
		</span>
		{#if stats.eventsPerSec !== null}
			<span class="metric">
				<span class="metric__value">{stats.eventsPerSec.toFixed(1)}</span>
				<span class="metric__label">spans/s</span>
			</span>
		{/if}
		{#if stats.p50Ms !== null}
			<span class="metric">
				<span class="metric__value">{formatDuration(stats.p50Ms)}</span>
				<span class="metric__label">p50</span>
			</span>
			<span class="metric">
				<span class="metric__value">{formatDuration(stats.p95Ms ?? stats.p50Ms)}</span>
				<span class="metric__label">p95</span>
			</span>
		{/if}
		{#each stats.spend as entry (entry.commodity)}
			<span class="metric" title="Total {entry.commodity} across the retained window — commodities are never added together">
				<span class="metric__value">{formatSpend(entry)}</span>
				<span class="metric__label">{entry.commodity}</span>
			</span>
		{/each}
		{#if stats.errors > 0}
			<span class="metric metric--error">
				<span class="metric__value">{stats.errors.toLocaleString()}</span>
				<span class="metric__label">errors</span>
			</span>
		{/if}
		{#if stats.closed > 0}
			<!-- Deliberately its own figure, never folded into a success rate:
			     `closed` means the span ended without saying how it went, and
			     showing it as healthy would make abandoned work look fine. -->
			<span
				class="metric metric--muted"
				title="Spans that ended without declaring an outcome — not failures, but not confirmed successes either"
			>
				<span class="metric__value">{stats.closed.toLocaleString()}</span>
				<span class="metric__label">undeclared end</span>
			</span>
		{/if}
		<span class="metric metric--muted">
			<span class="metric__value">{heldSpans.toLocaleString()}</span>
			<span class="metric__label">held</span>
		</span>
		{#if paused}<span class="metric metric--paused"><span class="metric__label">paused</span></span>{/if}
		{#if subjectFilter}
			<!-- Always visible while pinned. Without it a filtered view looks
			     like a quiet runtime, and the numbers beside it would be read
			     as totals. -->
			<button
				type="button"
				class="metric metric--pinned"
				title="Clear the filter"
				on:click={() => (subjectFilter = null, dataVersion += 1)}
			>
				<span class="metric__label">{subjectFilter.kind}</span>
				<span class="metric__value">{subjectFilter.id}</span>
				<span class="metric__label">✕</span>
			</button>
		{/if}
	</div>

	{#if lanes.length > 0}
		<div class="lanes-section">
			<div class="lanes-section__head">
				<div class="lanes-section__title">
					<span class="lanes-section__label">Workload lanes</span>
					<span class="lanes-section__count">{lanes.length}</span>
				</div>
				<button
					type="button"
					class="lanes-section__toggle"
					on:click={() => (lanesCollapsed = !lanesCollapsed)}
					title={lanesCollapsed ? 'Expand workload lanes' : 'Collapse workload lanes'}
				>
					<Icon name={lanesCollapsed ? 'chevron-down' : 'chevron-up'} size={12} />
					<span>{lanesCollapsed ? 'Show lanes' : 'Collapse'}</span>
				</button>
			</div>

			{#if !lanesCollapsed}
				<section class="lanes" aria-label="Work in flight, grouped by workload class">
					{#each lanes as lane (lane.key)}
						<article class="lane" class:lane--undeclared={lane.undeclared}>
							<header class="lane__head">
								<span class="lane__label">{lane.label}</span>
								<span class="lane__count">{lane.running.length}</span>
							</header>

							{#if lane.undeclared}
								<p
									class="lane__note"
									title="Roots that never declared a workload class. This is an instrumentation gap, not a category."
								>
									Instrumentation gap (undeclared roots)
								</p>
							{/if}

							<ul class="lane__list">
								{#each lane.running as node (node.id)}
									{@const meta = kindMeta(node.kind)}
									<li class="lane__item">
										<!-- Glyph as well as hue: the kind palette is documented as
										     needing a non-colour channel for readers who cannot
										     separate the hues. -->
										<span class="lane__glyph" style:color={meta.color} aria-hidden="true"
											>{meta.glyph}</span
										>
										{#if node.agentId}
											<button
												type="button"
												class="lane__subject"
												class:lane__subject--on={subjectFilter?.kind === 'agent' &&
													subjectFilter.id === node.agentId}
												title="Pin every tier to agent {node.agentId}"
												on:click={() => pinSubject('agent', node.agentId)}>{node.agentId}</button
											>
										{/if}
										<span class="lane__name" title={node.target}>{node.name}</span>
										{#if node.operation}<span class="lane__operation" title="operation: {node.operation}"
												>{node.operation}</span
											>{/if}
										{#if node.model}<span class="lane__model" title={node.model}>{node.model}</span>{/if}
										<!-- Ticks off the shared clock, so a stuck span visibly
										     climbs instead of sitting at its start time. -->
										<span class="lane__age"
											>{node.startedMs === null
												? '—'
												: formatDuration(Math.max(0, now - node.startedMs))}</span
										>
									</li>
								{/each}

								{#each lane.recent as node (node.id)}
									{@const meta = kindMeta(node.kind)}
									<li class="lane__item lane__item--done" class:lane__item--error={node.outcome === 'error'}>
										<span class="lane__glyph" style:color={meta.color} aria-hidden="true"
											>{meta.glyph}</span
										>
										<span class="lane__name" title={node.target}>{node.name}</span>
										{#if node.operation}<span class="lane__operation" title="operation: {node.operation}"
												>{node.operation}</span
											>{/if}
										{#if node.model}<span class="lane__model" title={node.model}>{node.model}</span>{/if}
										<!-- Only on finished rows, and only once priced. A running span
										     has no cost yet, and showing 0 would say free. One chip per
										     commodity, side by side — never added together. -->
										{#each node.costs as entry (entry.commodity)}
											<span class="lane__cost" title={costTitle(entry)}
												>{entry.commodity === 'local' ? 'local' : formatSpend(entry)}</span
											>
										{/each}
										<span class="lane__age"
											>{node.durationMs === null ? '—' : formatDuration(node.durationMs)}</span
										>
									</li>
								{/each}

								{#if lane.running.length === 0 && lane.recent.length === 0}
									<li class="lane__idle">idle</li>
								{/if}
							</ul>
						</article>
					{/each}
				</section>
			{/if}
		</div>
	{/if}

	<div class="stream-section">
		<div class="stream-section__head">
			<div class="stream-section__title">
				<span class="stream-section__label">Activity stream</span>
				<span class="stream-section__badge">{flatRows.length.toLocaleString()} rows</span>
				{#if anyLive}
					<span class="stream-section__live">
						<span class="stream-section__live-dot" aria-hidden="true"></span>
						Live updates
					</span>
				{/if}
			</div>
			<div class="stream-section__meta">
				{#if flatRows.length >= MAX_RENDERED_ROWS}
					<span class="stream-section__hint">Showing newest {MAX_RENDERED_ROWS.toLocaleString()} rows</span>
				{:else if flatRows.length > 0}
					<span class="stream-section__hint">Newest events first</span>
				{/if}
			</div>
		</div>

		<div class="rows">
		{#if flatRows.length === 0}
			<EmptyState
				title={anyLive ? 'No activity yet' : 'Not connected'}
				description={anyLive
					? 'Spans appear here as the runtime opens them. Background passes run on their own schedule, so a quiet moment is normal.'
					: 'Waiting for the event stream. Reconnects use a jittered backoff.'}
			/>
		{:else}
			{#each flatRows as row (row.key)}
				{#if row.type === 'span'}
					{@const meta = kindMeta(row.node.kind)}
					{@const scope = scopeLabel(row.node.principal, row.node.workspace)}
					<div
						class="row row--span"
						class:row--running={!row.node.finished}
						style="--row-indent: {row.depth}; --kind-color: {meta.color}"
					>
						<button
							type="button"
							class="row__disclosure"
							disabled={!row.expandable}
							aria-expanded={row.expanded}
							aria-label={row.expanded ? 'Collapse' : 'Expand'}
							on:click={() => toggleExpanded(row.node.id)}
						>
							{row.expandable ? (row.expanded ? '▾' : '▸') : ''}
						</button>
						<span class="row__glyph" title={meta.label} aria-hidden="true">{meta.glyph}</span>
						<span class="row__time">{formatClock(row.node.startedMs ?? row.node.finishedMs)}</span>
						<span class="row__name" title={row.node.target}>{row.node.name}</span>
						<!-- What this row actually did. The span name is the
						     BOUNDARY's name — every model call in the process is
						     `llm_dispatch` by construction — so without the
						     operation the stream is a wall of identical lines.
						     Placed immediately after the name because it reads
						     as part of the name: `llm_dispatch agentic_decision`. -->
						{#if row.node.operation}
							<span class="row__operation" title="operation: {row.node.operation}"
								>{row.node.operation}</span
							>
						{/if}
						<span class="row__kind">{meta.label}</span>
						{#if row.node.target}
							<span class="row__target" title={row.node.target}>{shortTarget(row.node.target)}</span>
						{/if}
						{#if row.orphan}
							<span
								class="tag tag--orphan"
								title="This span names a parent that never reached this view — most likely dropped. Shown at the root rather than hidden."
							>
								parent missing
							</span>
						{/if}
						{#if row.expandable && !row.expanded}
							<span class="row__counts">
								{#if row.childCount > 0}{row.childCount} span{row.childCount === 1
										? ''
										: 's'}{/if}{#if row.childCount > 0 && row.lineCount > 0}&nbsp;·&nbsp;{/if}{#if row.lineCount > 0}{row.lineCount}
									line{row.lineCount === 1 ? '' : 's'}{/if}
							</span>
						{/if}
						<span class="row__spacer"></span>
						<!-- Model and price, on the row that spent them. Both were
						     already on the client model and rendered only in the
						     lanes, which show a short live window; the stream is
						     where you go to read the record, and it was the one
						     tier that could not answer "which model, and what did
						     it cost". Same rules as the lanes: `local` is a word
						     not a number, commodities sit side by side and are
						     never added, and an unpriced call shows nothing at
						     all rather than a zero that would read as free. -->
						{#if row.node.model}
							<span class="row__model" title="model: {row.node.model}">{row.node.model}</span>
						{/if}
						{#each row.node.costs as entry (entry.commodity)}
							<span class="row__cost" title={costTitle(entry)}
								>{entry.commodity === 'local' ? 'local' : formatSpend(entry)}</span
							>
						{/each}
						<span class="row__scope" title={scope}>{scope}</span>
						{#if row.node.finished}
							<span
								class="tag tag--{outcomeTone(row.node.outcome)}"
								title={row.node.outcome === 'closed'
									? 'closed = the span ended. It is not a success claim — the layer watched the span end, nothing more.'
									: `outcome: ${row.node.outcome ?? 'unknown'}`}
							>
								{row.node.outcome ?? 'ended'}
							</span>
						{:else}
							<span class="tag tag--running">running</span>
						{/if}
						<span class="row__duration">{spanTiming(row.node, now)}</span>
					</div>
				{:else}
					{@const tone = levelTone(row.row.level)}
					<div
						class="row row--progress row--level-{tone}"
						class:row--loose={row.loose}
						style="--row-indent: {row.depth}"
					>
						<span class="row__disclosure" aria-hidden="true"></span>
						<span class="row__glyph row__glyph--line" aria-hidden="true">
							{row.loose ? KIND_META[LOOSE_KIND].glyph : '│'}
						</span>
						<span class="row__time">{formatClock(row.row.timestampMs)}</span>
						<span class="row__level">{row.row.level}</span>
						<span class="row__message">{row.row.message}</span>
						<span class="row__spacer"></span>
						{#if row.loose}
							<span
								class="tag tag--loose"
								title="Emitted outside any instrumented span. The design accepts a span floor; showing this row is the honesty."
							>
								unspanned
							</span>
						{/if}
						{#if row.row.target}
							<span class="row__target" title={row.row.target}>{shortTarget(row.row.target)}</span>
						{/if}
					</div>
				{/if}
			{/each}
			{#if flatRows.length >= MAX_RENDERED_ROWS}
				<p class="notice notice--cap">
					Showing the newest {MAX_RENDERED_ROWS.toLocaleString()} rows. Older rows are still held
					until the span cap evicts them.
				</p>
			{/if}
		{/if}
	</div>
	</div>
</section>

<style>
	.activity-view {
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 0.85rem 1.25rem 3.5rem;
		box-sizing: border-box;
	}

	.head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		flex-wrap: wrap;
		margin-bottom: 0.1rem;
	}
	.head__title {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		min-width: 0;
	}
	.head-kicker {
		margin: 0;
		font-size: 0.7rem;
		font-weight: 700;
		letter-spacing: 0.05em;
		text-transform: uppercase;
		color: var(--text-muted);
	}
	.head__title-row {
		display: flex;
		align-items: center;
		gap: 0.65rem;
		flex-wrap: wrap;
	}
	.head h1 {
		font-family: var(--font-display, inherit);
		font-size: 1.35rem;
		font-weight: 700;
		color: var(--text-primary);
		margin: 0;
		line-height: 1.2;
	}
	.head__sub {
		margin: 0;
		font-size: 0.8rem;
		color: var(--text-muted);
		max-width: 65ch;
		line-height: 1.35;
	}
	.head__right {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.streams {
		display: inline-flex;
		gap: 0.4rem;
		flex-wrap: wrap;
		align-items: center;
	}
	.stream {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.2rem 0.6rem;
		border-radius: 999px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		font-family: var(--font-mono);
		font-size: 0.72rem;
		color: var(--text-secondary);
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
	}
	.stream__dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--text-faint);
	}
	.stream--live .stream__dot {
		background: var(--color-success, #10b981);
		box-shadow: 0 0 6px color-mix(in srgb, var(--color-success, #10b981) 50%, transparent);
	}
	.stream--connecting .stream__dot,
	.stream--closed .stream__dot {
		background: var(--color-warning, #f59e0b);
	}
	.stream--error .stream__dot {
		background: var(--color-error, #ef4444);
	}
	.stream__state {
		color: var(--text-muted);
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	:global(.action-button) {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.38rem;
		max-width: 100%;
		min-height: 2.1rem;
		padding: 0.42rem 0.82rem;
		border: 1px solid transparent;
		border-radius: 8px;
		font: inherit;
		font-size: 0.82rem;
		font-weight: 500;
		line-height: 1.2;
		cursor: pointer;
		text-decoration: none;
		transition:
			background 0.15s ease,
			border-color 0.15s ease,
			color 0.15s ease,
			opacity 0.15s ease,
			transform 0.15s ease,
			box-shadow 0.15s ease;
	}
	:global(.action-button):hover:not(:disabled) {
		transform: translateY(-1px);
	}
	:global(.action-button):disabled {
		cursor: default;
		opacity: 0.56;
	}
	:global(.action-button--outline) {
		background: var(--bg-card);
		border-color: var(--border-soft);
		color: var(--text-secondary);
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
	}
	:global(.action-button--outline:hover:not(:disabled)) {
		background: var(--bg-soft);
		color: var(--text-primary);
		border-color: var(--border-default);
	}
	:global(.action-button--active) {
		background: color-mix(in srgb, var(--color-warning, #f59e0b) 12%, var(--bg-card));
		border-color: color-mix(in srgb, var(--color-warning, #f59e0b) 50%, var(--border-soft));
		color: var(--color-warning, #d97706);
	}
	:global(.action-button--sm) {
		min-height: 1.85rem;
		padding: 0.3rem 0.65rem;
		font-size: 0.76rem;
	}

	.notice {
		margin: 0;
		padding: 0.5rem 0.7rem;
		border-radius: 8px;
		font-size: var(--text-xs);
		line-height: var(--leading-normal);
	}
	.notice--dropped {
		background: var(--color-error-soft);
		color: var(--text-primary);
		border-left: 3px solid var(--color-error);
	}
	.notice--history {
		background: var(--color-warning-soft);
		color: var(--text-primary);
		border-left: 3px solid var(--color-warning);
	}
	.notice--lagged {
		background: var(--color-warning-soft);
		color: var(--text-primary);
		border-left: 3px solid var(--color-warning);
	}
	.notice--cap {
		background: var(--bg-soft);
		color: var(--text-muted);
	}

	.legend {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		flex-wrap: wrap;
		padding: 0.1rem 0;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		border: 1px solid color-mix(in srgb, var(--chip-color) 35%, var(--border-soft));
		background: var(--bg-card);
		color: var(--text-primary);
		border-radius: 999px;
		padding: 0.18rem 0.62rem;
		font-size: 0.74rem;
		font-weight: 500;
		font-family: inherit;
		cursor: pointer;
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
		transition: all 0.15s ease;
	}
	.chip:hover {
		background: var(--bg-soft);
		border-color: var(--chip-color);
		transform: translateY(-1px);
	}
	.chip__glyph {
		color: var(--chip-color);
		font-size: 0.85em;
	}
	.chip--off {
		opacity: 0.45;
		border-style: dashed;
		background: transparent;
		color: var(--text-muted);
		box-shadow: none;
	}
	.legend__note {
		font-size: 0.72rem;
		color: var(--text-muted);
		max-width: 52ch;
		line-height: 1.4;
		margin-left: auto;
	}

	.statbar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.35rem 1.1rem;
		padding: 0.42rem 0.85rem;
		border-radius: 8px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
		font-family: var(--font-mono);
	}

	/* Named `.metric`, not `.stat` — this app also loads a global daisyUI
	   build, which ships its OWN `.stat` component (`display: inline-grid;
	   width: 100%; grid-template-columns: repeat(1, 1fr)`, for its stats-card
	   widget). That rule is unscoped, but on this page it still won the
	   cascade over this component's scoped `.stat.<hash>` despite the scoped
	   selector's higher specificity — Svelte's compiled output and the
	   Tailwind/daisyUI build sit in different cascade layers, and layer order
	   beats specificity. The effect: every `.stat` item computed
	   `display: inline-grid; width: 100%`, so each metric filled the full row
	   width and the whole bar rendered as one metric per line — the
	   never-rendered layout's "statbar" reading as a stack of oversized
	   single-column cards. Confirmed live via CDP's
	   `CSS.getMatchedStylesForNode`, not guessed. Renaming out of daisyUI's
	   component vocabulary is the fix, not fighting the layer order. */
	.metric {
		display: inline-flex;
		align-items: baseline;
		gap: 0.35rem;
	}

	.metric__value {
		font-size: 0.88rem;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
		color: var(--text-primary);
	}

	.metric__label {
		font-size: 0.68rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted);
		font-weight: 500;
	}

	.metric--muted .metric__value {
		color: var(--text-muted);
	}

	.metric--error .metric__value {
		color: var(--color-danger, #b3261e);
	}

	.metric--paused .metric__label {
		color: var(--color-warning);
	}

	.metric--pinned {
		border: 1px solid var(--accent-primary);
		border-radius: 999px;
		padding: 0.1rem 0.55rem;
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card));
		cursor: pointer;
		font-family: inherit;
		transition: all 0.15s ease;
	}
	.metric--pinned:hover {
		background: color-mix(in srgb, var(--accent-primary) 18%, var(--bg-card));
	}

	.lane__subject {
		flex: none;
		max-width: 7rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		border: none;
		padding: 0;
		background: transparent;
		font-family: inherit;
		font-size: var(--text-2xs, 0.7rem);
		color: var(--text-muted);
		cursor: pointer;
		text-decoration: underline dotted;
	}

	.lane__subject--on {
		color: var(--accent-primary);
		text-decoration-style: solid;
	}

	/* A wrapping grid, not a horizontally-scrolling row. This used to be
	   `display: flex; overflow-x: auto`, reasoned as keeping the vertical
	   budget predictable: wrapping makes it depend on how many classes
	   happen to be active, so the stream below could jump down whenever a
	   new class appeared. Rendered against real data, that trade reads as
	   the bug this page was reported for, not a feature: at
	   `--app-content-max` width only ~5 of the up to 10 possible lanes fit
	   before the edge, the rest sat off-screen behind a hairline native
	   scrollbar with no other affordance — lanes were culled exactly the
	   way the operator described, just silently instead of visibly. The
	   lane key set is closed and small (9 declared classes + undeclared),
	   so the "unpredictable" growth this was avoiding is bounded too — at
	   most two short rows, never more — and it only changes between renders
	   when a workload class newly appears in the retained window, which is
	   rare next to the 150ms flush. A bounded, occasional two-row jump is a
	   small price for never hiding a lane. */
	.lanes-section {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	.lanes-section__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		padding: 0 0.1rem;
	}
	.lanes-section__title {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
	}
	.lanes-section__label {
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted);
	}
	.lanes-section__count {
		font-size: 0.68rem;
		font-family: var(--font-mono);
		font-weight: 600;
		padding: 0.05rem 0.4rem;
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-secondary);
	}
	.lanes-section__toggle {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.15rem 0.45rem;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		background: var(--bg-card);
		color: var(--text-muted);
		font-size: 0.7rem;
		font-family: inherit;
		cursor: pointer;
		transition: all 0.15s ease;
	}
	.lanes-section__toggle:hover {
		background: var(--bg-soft);
		color: var(--text-primary);
		border-color: var(--border-default);
	}

	.lanes {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(11.5rem, 1fr));
		gap: 0.45rem;
	}

	.lane {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		padding: 0.45rem 0.6rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
		transition: border-color 0.15s ease, box-shadow 0.15s ease;
	}
	.lane:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
	}

	/* Undeclared is a gap, not a category — dashed so it reads as unfinished
	   instrumentation rather than as a peer of the real classes. */
	.lane--undeclared {
		border-style: dashed;
	}

	.lane__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.4rem;
		padding-bottom: 0.25rem;
		border-bottom: 1px solid var(--border-soft);
		margin-bottom: 0.1rem;
	}

	.lane__label {
		font-size: 0.76rem;
		font-weight: 650;
		color: var(--text-primary);
	}

	.lane__count {
		font-size: 0.68rem;
		font-family: var(--font-mono);
		font-weight: 600;
		padding: 0.04rem 0.35rem;
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-secondary);
	}

	.lane__note {
		margin: 0;
		font-size: 0.68rem;
		line-height: 1.2;
		color: var(--text-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.lane__list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.12rem;
		max-height: 7.2rem;
		overflow-y: auto;
		scrollbar-width: thin;
	}

	.lane__item {
		display: flex;
		align-items: baseline;
		gap: 0.3rem;
		font-size: 0.72rem;
		min-width: 0;
	}

	/* Finished rows recede so the running ones read first. */
	.lane__item--done {
		color: var(--text-muted);
	}

	.lane__item--error {
		color: var(--color-danger, #b3261e);
	}

	.lane__glyph {
		flex: none;
		font-size: 0.7em;
	}

	.lane__name {
		flex: 1 1 auto;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.lane__cost {
		flex: none;
		font-size: var(--text-2xs, 0.7rem);
		font-variant-numeric: tabular-nums;
		color: var(--text-muted);
	}

	/* Bounded like `.lane__model` below and for the same reason. Toned one
	   step brighter than the model because it is what the span DID, not an
	   attribute of how it did it. */
	.lane__operation {
		flex: none;
		max-width: 9rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-2xs, 0.7rem);
		color: var(--text-secondary);
	}

	/* Bounded and ellipsised for the same reason `.row__target` is: model
	   identifiers are not length-limited, and a long one had nothing to stop
	   it overflowing past the card's own border into whatever sits next to
	   it — the lane card version of the row-culling bug. */
	.lane__model {
		flex: none;
		max-width: 8rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-2xs, 0.7rem);
		color: var(--text-muted);
	}

	.lane__age {
		flex: none;
		font-variant-numeric: tabular-nums;
		color: var(--text-muted);
	}

	.lane__idle {
		font-size: var(--text-xs);
		color: var(--text-faint, var(--text-muted));
	}

	.stream-section {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		width: 100%;
	}

	.stream-section__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		padding: 0 0.1rem;
	}

	.stream-section__title {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
	}

	.stream-section__label {
		font-size: 0.74rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted);
	}

	.stream-section__badge {
		font-size: 0.68rem;
		font-family: var(--font-mono);
		font-weight: 600;
		padding: 0.05rem 0.45rem;
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-secondary);
	}

	.stream-section__live {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font-size: 0.68rem;
		font-family: var(--font-mono);
		color: var(--color-success, #10b981);
	}

	.stream-section__live-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--color-success, #10b981);
		box-shadow: 0 0 6px color-mix(in srgb, var(--color-success, #10b981) 50%, transparent);
	}

	.stream-section__meta {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.stream-section__hint {
		font-size: 0.7rem;
		color: var(--text-muted);
	}

	.rows {
		display: flex;
		flex-direction: column;
		border: 1px solid var(--border-soft);
		border-radius: 9px;
		overflow-x: auto;
		overflow-y: auto;
		min-height: 380px;
		max-height: 68dvh;
		background: var(--bg-card);
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
		scrollbar-width: thin;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.22rem 0.6rem;
		padding-left: calc(0.6rem + var(--row-indent, 0) * 1.1rem);
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.05));
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		white-space: nowrap;
	}
	.row:last-child {
		border-bottom: none;
	}
	.row:hover {
		background: var(--bg-soft);
	}
	/* Kind is carried by BOTH the rail colour and the glyph shape, so the
	   families stay separable without colour vision. */
	.row--span {
		border-left: 3px solid var(--kind-color);
	}
	.row--running {
		background: var(--color-info-soft);
	}
	.row--progress {
		border-left: 3px solid transparent;
	}
	.row--loose {
		border-left: 3px dashed var(--text-faint);
	}

	.row__disclosure {
		width: 1rem;
		flex: none;
		border: none;
		background: none;
		color: var(--text-muted);
		cursor: pointer;
		padding: 0;
		font-size: 0.85em;
		text-align: center;
	}
	.row__disclosure:disabled {
		cursor: default;
		opacity: 0;
	}
	.row__glyph {
		flex: none;
		width: 1rem;
		text-align: center;
		color: var(--kind-color, var(--text-muted));
	}
	.row__glyph--line {
		color: var(--text-faint);
	}
	.row__time {
		flex: none;
		color: var(--text-faint);
	}
	.row__name {
		color: var(--text-primary);
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		/* Paired with the ellipsis, per this file's own convention (see
		   `.lane__item`): a flex child defaults to min-width:auto and, with
		   `.row`'s white-space:nowrap, that resolves to the text's full
		   unwrapped width — refusing to shrink at all. `min-width: 0` is
		   what lets the name give way to the fixed-width columns instead of
		   pushing them out of the row. */
		min-width: 0;
		flex: 1 1 auto;
	}
	.row__kind {
		flex: none;
		color: var(--kind-color);
	}
	/* Bounded and ellipsised for the same reason `.lane__subject` is: span
	   targets are dotted module paths and scope is `principal/workspace`,
	   both effectively unbounded in length. Left as bare `flex: none` text
	   with no width cap, either one could push the outcome tag and duration
	   off the end of the row — which is exactly what `.rows`'s horizontal
	   overflow used to crop invisibly. A title attribute on each carries
	   the untruncated value, matching `.row__name`'s own title. */
	.row__target,
	.row__scope {
		flex: none;
		max-width: 14rem;
		overflow: hidden;
		text-overflow: ellipsis;
		color: var(--text-faint);
	}
	/* The operation reads as part of the span's name, so it is toned between
	   `.row__name` and `.row__kind` rather than dropped to `--text-faint`
	   with the target: on an `llm_dispatch` row it is the only thing that
	   says what happened, and faint text would bury the one distinguishing
	   token on the line. Bounded like every other unbounded-length field
	   here — `LLMOperation::Other` carries a name chosen at the call site —
	   with the full value on the title. */
	.row__operation {
		flex: none;
		max-width: 14rem;
		overflow: hidden;
		text-overflow: ellipsis;
		color: var(--text-secondary);
	}
	/* Right-hand pair, matching `.lane__model` / `.lane__cost` so the same
	   fact reads the same in both tiers. Muted rather than secondary: they
	   are attributes of the row, not its subject. */
	.row__model {
		flex: none;
		max-width: 10rem;
		overflow: hidden;
		text-overflow: ellipsis;
		color: var(--text-muted);
	}
	.row__cost {
		flex: none;
		font-variant-numeric: tabular-nums;
		color: var(--text-muted);
	}
	.row__counts {
		flex: none;
		color: var(--text-muted);
	}
	.row__duration {
		flex: none;
		color: var(--text-secondary);
		min-width: 4.5rem;
		text-align: right;
	}
	.row__spacer {
		flex: 1 1 auto;
		min-width: 0.5rem;
	}
	.row__level {
		flex: none;
		text-transform: uppercase;
		font-size: 0.9em;
	}
	.row__message {
		color: var(--text-secondary);
		overflow: hidden;
		text-overflow: ellipsis;
		/* Flex children default to min-width:auto and refuse to shrink below
		   their content — without this the message column pushes the row
		   wider than the panel instead of ellipsing. */
		min-width: 0;
		flex: 0 1 auto;
	}
	/* Progress severity comes from the payload's own `level`, never from
	   the taxonomy (which is a static `info` for every ActivityProgress). */
	.row--level-error .row__level,
	.row--level-error .row__message {
		color: var(--color-error);
	}
	.row--level-warn .row__level,
	.row--level-warn .row__message {
		color: var(--color-warning);
	}
	.row--level-info .row__level {
		color: var(--text-muted);
	}
	.row--level-debug .row__level,
	.row--level-debug .row__message {
		color: var(--text-faint);
	}

	.tag {
		flex: none;
		border-radius: 999px;
		padding: 0.02rem 0.4rem;
		font-size: 0.9em;
		background: var(--bg-soft);
		color: var(--text-muted);
	}
	.tag--error {
		background: var(--color-error-soft);
		color: var(--color-error);
	}
	.tag--warn {
		background: var(--color-warning-soft);
		color: var(--text-primary);
	}
	.tag--success {
		background: var(--color-success-soft);
		color: var(--color-success);
	}
	.tag--running {
		background: var(--color-info-soft);
		color: var(--color-info);
	}
	.tag--orphan,
	.tag--loose {
		background: transparent;
		border: 1px dashed var(--text-faint);
		color: var(--text-muted);
	}
</style>
