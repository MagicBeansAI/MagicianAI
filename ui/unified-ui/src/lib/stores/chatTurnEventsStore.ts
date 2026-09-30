/**
 * Per-chat-turn event store.
 *
 * Both modes (live SSE + REST refresh) consume the same per-turn
 * projection that `ChatTurnEventSink` writes on the backend, so the
 * two views agree by construction.
 *
 *   1. **Live (SSE)** — for the in-flight turn. Opens
 *      `/api/magician/v2/chat/sessions/{sid}/turns/{cid}/events/stream`
 *      via `subscribeToChatTurn`. The backend handler replays the
 *      on-disk projection first, then forwards the live broadcast,
 *      with `event_id` dedupe between the two phases.
 *
 *   2. **REST refresh** — for historical turns. Card mounts →
 *      `fetchEventsPage` pulls the full per-turn log from
 *      `/api/magician/v2/chat/sessions/{sid}/turns/{cid}/events`.
 *      No pagination cursor — the per-turn file is already bounded
 *      by what one turn actually emitted.
 *
 * Why two modes (and not just SSE everywhere):
 *   - Live: low-latency for the in-flight turn that hasn't fully
 *     finished writing to the file yet.
 *   - Historical: scrolling chat history with 50 prior turns
 *     shouldn't open 50 long-lived SSE connections — one REST call
 *     per turn on mount is cheaper.
 *
 * Same `Map<chat_turn_id, RawTurnEvent[]>` backs both modes. The
 * `RequestActivityCard` is mode-agnostic; it just reads from the
 * store and decides whether to mount the SSE based on liveness.
 */
import { writable, derived, get, type Readable } from 'svelte/store';
import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
import { createBackoff } from '$lib/realtime/backoff';

/** Opaque "raw event" type. The shape is whatever `/events` SSE writes
 *  on each line — we don't pre-parse here so consumers stay free to
 *  evolve their own classification (today: card coalesces by event
 *  type; tomorrow another surface may want the full tree). */
export type RawTurnEvent = Record<string, unknown>;

const eventsByTurnId = writable<Map<string, RawTurnEvent[]>>(new Map());

/** Reactive view: chat_turn_id → list of raw events (insert order). */
export const chatTurnEventsStore: Readable<Map<string, RawTurnEvent[]>> = derived(
    eventsByTurnId,
    ($map) => $map
);

interface Connection {
    abort: AbortController;
    refCount: number;
    /** Scope + ids captured at subscribe time. The watchdog needs them
     *  to call `fetchEventsPage` on the same scope the SSE was opened
     *  with, even after the original subscriber unmounts. Storing the
     *  split ids here (rather than re-deriving from the connection key)
     *  avoids any ambiguity if a future id format ever contains `::`. */
    scope: SubscribeScope;
    sessionId: string;
    chatTurnId: string;
    /** Epoch-ms of the last event appended for this (sessionId, turnId).
     *  Initialised to subscribe time so a freshly-opened connection
     *  isn't considered idle until the first watchdog window elapses. */
    lastEventAt: number;
    /** True while this connection owns its reconnect loop. An unsubscribed
     *  connection is aborted and removed; a later subscriber gets a new one. */
    loopRunning: boolean;
}
/**
 * Keyed by `${sessionId}::${chatTurnId}`. Connections are now
 * session-scoped because the new per-turn SSE URL embeds sessionId —
 * a `(connection_key, fetched URL)` mismatch would otherwise let one
 * session's connection serve a different session's subscriber (rare
 * in practice; required for correctness).
 */
const connections = new Map<string, Connection>();

// ─── Reconciliation watchdog ────────────────────────────────────────
//
// SSE is a best-effort live tail. Flaky networks, broadcast-buffer
// overflow on the server, or a transient mid-stream disconnect can
// drop `task.status_changed: completed` (or any other terminal event)
// without warning — the card then shows "active" forever even though
// the task long since finished.
//
// Two layers of recovery:
//
//   1. **SSE auto-reconnect** (`openStream` loop). On any disconnect
//      we re-open the stream with exponential backoff. The backend
//      replays the per-turn on-disk projection before forwarding live
//      events, so a 5s blip → reconnect → dedup catches everything
//      we already had → only the genuinely-missed rows land. This is
//      the primary recovery path and handles transient blips
//      (sub-second to ~tens of seconds) within one backoff window.
//
//   2. **REST reconciliation watchdog** (this section). For the case
//      where the SSE *thinks* it's healthy but the server actually
//      dropped an event mid-stream (broadcaster lag, fan-out skip,
//      bug), we periodically poll the REST endpoint and merge new
//      rows in via the same dedup. Every `WATCHDOG_CHECK_INTERVAL_MS`
//      we scan every live subscription; any connection that hasn't
//      appended an event for longer than `WATCHDOG_IDLE_THRESHOLD_MS`
//      triggers a one-shot REST refetch via `fetchEventsPage`. The
//      card's reactive `$chatTurnEventsStore` subscription picks up
//      the appended events and the leaf statuses settle.
//
// Together they bound the worst-case latency at ~60-90s for a missed
// terminal event. SSE handles fast paths (sub-second on a clean
// disconnect); REST closes the residual gap on dirty disconnects
// where the server thinks the event was delivered but it wasn't.
const WATCHDOG_CHECK_INTERVAL_MS = 30_000;
const WATCHDOG_IDLE_THRESHOLD_MS = 60_000;
let watchdogTimer: ReturnType<typeof setInterval> | null = null;

function ensureWatchdogRunning(): void {
    if (watchdogTimer !== null) return;
    watchdogTimer = setInterval(runWatchdogTick, WATCHDOG_CHECK_INTERVAL_MS);
}

function runWatchdogTick(): void {
    const now = Date.now();
    for (const conn of connections.values()) {
        // Only actively watched turns need REST reconciliation.
        if (conn.refCount === 0) continue;
        if (now - conn.lastEventAt < WATCHDOG_IDLE_THRESHOLD_MS) continue;
        // Reset BEFORE the fetch so an in-flight watchdog doesn't
        // trigger again 30s later just because the previous fetch
        // hadn't returned yet. If the fetch fails the next tick
        // crosses the threshold again and we retry.
        conn.lastEventAt = now;
        void fetchEventsPage(conn.scope, conn.sessionId, conn.chatTurnId);
    }
}

function connectionKey(sessionId: string, chatTurnId: string): string {
    return `${sessionId}::${chatTurnId}`;
}

interface SubscribeScope {
    principal: string;
    workspace: string;
}

/**
 * Reference-counted subscribe. The first caller for a given
 * (sessionId, turnId) pair opens the SSE connection; later callers
 * just bump the ref count. Returns an unsubscribe function the caller
 * MUST call on cleanup (typically in `onDestroy`). When the last subscriber
 * leaves, abort the HTTP stream immediately and remove its connection entry.
 * Cached events remain available; a later subscription opens a fresh stream
 * and deduplicates the backend replay against that cache. Waiting for a live
 * stream to end before noticing refCount=0 exhausts HTTP/1.1 connections after
 * several voice turns and blocks new request admission.
 *
 * To free a turn id explicitly (drop events, abort outstanding fetch),
 * call `releaseChatTurn(id)` (e.g. when a session is archived).
 */
export function subscribeToChatTurn(
    scope: SubscribeScope,
    sessionId: string,
    chatTurnId: string
): () => void {
    if (!scope?.principal || !scope?.workspace || !sessionId || !chatTurnId) {
        return () => {};
    }

    const key = connectionKey(sessionId, chatTurnId);
    const existing = connections.get(key);
    if (existing) {
        existing.refCount += 1;
        // Multiple cards watching one live turn share its stream.
        ensureStreamLoop(existing);
    } else {
        const abort = new AbortController();
        const conn: Connection = {
            abort,
            refCount: 1,
            scope: { principal: scope.principal, workspace: scope.workspace },
            sessionId,
            chatTurnId,
            lastEventAt: Date.now(),
            loopRunning: false,
        };
        connections.set(key, conn);
        ensureWatchdogRunning();
        ensureStreamLoop(conn);
    }

    const subscribedConnection = connections.get(key);
    let released = false;
    return () => {
        if (released) return;
        released = true;
        const conn = connections.get(key);
        if (!conn || conn !== subscribedConnection) return;
        conn.refCount = Math.max(0, conn.refCount - 1);
        if (conn.refCount === 0) {
            conn.abort.abort();
            connections.delete(key);
            if (connections.size === 0 && watchdogTimer !== null) {
                clearInterval(watchdogTimer);
                watchdogTimer = null;
            }
        }
    };
}

/**
 * Explicit teardown for a single (sessionId, turnId) — closes the SSE
 * connection and drops accumulated events. Pass `sessionId` so the
 * right per-session connection is torn down. `chatTurnId` alone may
 * refer to multiple sessions (rare, but possible across aggregate
 * views). Events in `eventsByTurnId` are still keyed by turn id only
 * because the activity card only ever inspects one session at a time.
 */
export function releaseChatTurn(
    sessionId: string,
    chatTurnId: string
): void {
    const key = connectionKey(sessionId, chatTurnId);
    const conn = connections.get(key);
    if (conn) {
        conn.abort.abort();
        connections.delete(key);
    }
    eventsByTurnId.update((current) => {
        if (!current.has(chatTurnId)) return current;
        const next = new Map(current);
        next.delete(chatTurnId);
        return next;
    });
}

// ─── SSE auto-reconnect tuning ──────────────────────────────────────
//
// On a clean disconnect (server end-of-stream, mid-stream error, or
// any network blip) we re-open the SSE with exponential backoff,
// capped at `RECONNECT_MAX_BACKOFF_MS`. Backoff resets to the initial
// delay every time we successfully append at least one event after
// reconnecting — so a healthy stream that hits one transient blip
// doesn't drift into 30s reconnect intervals forever.
//
// `AbortController.signal.aborted` is the single exit condition.
// `releaseChatTurn` calls `abort()`, which surfaces here as either
// an AbortError from `timedFetch` or `reader.read()`, OR — after a
// successful disconnect — as `signal.aborted === true` when the
// `while` loop checks before sleeping. Either way the loop exits.
//
// The backend SSE handler replays the on-disk per-turn projection
// before forwarding live events; the store's `eventDedupeKey` dedup
// ensures the replay doesn't double-insert rows we already had.
// Concretely: a 5s disconnect → reconnect → backend replays full
// projection → frontend dedups everything pre-disconnect → only the
// genuinely-missed-while-offline rows land.
const RECONNECT_INITIAL_BACKOFF_MS = 1_000;
const RECONNECT_MAX_BACKOFF_MS = 30_000;

async function delay(ms: number, signal: AbortSignal): Promise<void> {
    if (signal.aborted) return;
    return new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
            signal.removeEventListener('abort', onAbort);
            resolve();
        }, ms);
        const onAbort = () => {
            clearTimeout(timer);
            signal.removeEventListener('abort', onAbort);
            resolve();
        };
        signal.addEventListener('abort', onAbort, { once: true });
    });
}

function ensureStreamLoop(conn: Connection): void {
    if (conn.loopRunning || conn.abort.signal.aborted) return;
    conn.loopRunning = true;
    void openStream(conn).finally(() => {
        conn.loopRunning = false;
    });
}

async function openStream(conn: Connection): Promise<void> {
    // Per-chat-turn SSE endpoint. Same projection the REST page handler
    // reads, surfaced as a live tail — both views agree by construction.
    // Replaces the legacy `/api/magician/v3/events?chat_turn_id=…` path
    // which applied chat_turn_id filtering at the SSE handler and was
    // prone to drift with the REST backfill.
    const { scope, sessionId, chatTurnId, abort } = conn;
    const params = new URLSearchParams();
    const url = `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/turns/${encodeURIComponent(chatTurnId)}/events/stream?${params.toString()}`;

    // Jittered so every turn stream on the page doesn't reconnect in
    // lockstep after a backend restart (see realtime/backoff.ts).
    const backoff = createBackoff({
        initialMs: RECONNECT_INITIAL_BACKOFF_MS,
        maxMs: RECONNECT_MAX_BACKOFF_MS,
    });

    while (!abort.signal.aborted) {
        // refCount === 0 means every card on this turn has unmounted
        // (e.g. user navigated away or the turn is no longer the
        // in-flight one). Exit cleanly; the next subscribe via
        // `ensureStreamLoop` will re-arm us and pull the backlog from
        // the on-disk projection on reconnect.
        if (conn.refCount === 0) return;

        let appendedAnyThisAttempt = false;
        try {
            const response = await timedFetch(url, {
                signal: abort.signal,
                timeoutMs: LONG_FETCH_TIMEOUT_MS,
            });
            if (!response.ok || !response.body) {
                // Non-2xx (404, 503, etc.) or empty body. Treat as a
                // transient failure — back off and retry. A genuine
                // permanent 404 (deleted turn) will just spin at 30s
                // intervals; the watchdog REST refetch keeps the
                // store accurate in the meantime.
                // No-op here; fall through to the backoff at loop tail.
            } else {
                const reader = response.body.getReader();
                const decoder = new TextDecoder('utf-8');
                let buffer = '';
                let readerDone = false;
                while (!readerDone) {
                    const { done, value } = await reader.read();
                    if (done) {
                        readerDone = true;
                        break;
                    }
                    buffer += decoder.decode(value, { stream: true });
                    let nl: number;
                    while ((nl = buffer.indexOf('\n')) !== -1) {
                        const line = buffer.slice(0, nl).trim();
                        buffer = buffer.slice(nl + 1);
                        if (!line) continue;
                        try {
                            const parsed = JSON.parse(line) as RawTurnEvent;
                            appendEvent(chatTurnId, parsed);
                            appendedAnyThisAttempt = true;
                        } catch {
                            // unparseable line — skip silently
                        }
                    }
                }
            }
        } catch {
            // network error / AbortError / timeout — fall through to
            // the abort-check + backoff below. The catch is a single
            // exit point for every failure mode the fetch + reader can
            // raise, including the user-initiated `abort()`.
        }

        if (abort.signal.aborted) return;

        // Reset backoff on any successful read — a long-lived stream
        // that hits one transient blip should reconnect quickly, not
        // creep up to 30s intervals forever.
        if (appendedAnyThisAttempt) {
            backoff.reset();
        }

        await delay(backoff.nextMs(), abort.signal);
    }
}

function appendEvent(chatTurnId: string, event: RawTurnEvent): void {
    let appended = false;
    eventsByTurnId.update((current) => {
        const existing = current.get(chatTurnId) ?? [];
        // SSE path dedupe: chat-fanout re-emits each event as a clone
        // (primary + chat-scoped copy), so without dedupe every event
        // appears twice in the store. See `fetchEventsPage` for the
        // detailed rationale; same logic, single-event variant.
        const key = eventDedupeKey(event);
        if (key) {
            for (const e of existing) {
                if (eventDedupeKey(e) === key) {
                    return current;
                }
            }
        }
        appended = true;
        const next = new Map(current);
        next.set(chatTurnId, [...existing, event]);
        return next;
    });
    if (appended) {
        // Only refresh the watchdog cursor for genuinely-new events
        // — duplicates from the dedup path don't prove the SSE is
        // still alive. Walk active connections matching this turn id
        // (a chat_turn_id is usually bound to one session, but
        // aggregate views can produce more than one).
        const now = Date.now();
        for (const conn of connections.values()) {
            if (conn.chatTurnId === chatTurnId) {
                conn.lastEventAt = now;
            }
        }
    }
}

/**
 * Stable per-logical-event identity. `RuntimeTransportEvent`
 * serializes with serde tag/content so every variant's body lives
 * under `data.*`. The id lives in a variant-specific spot:
 *   • `AgentEvent` envelopes: `data.event.payload.event_id`
 *   • `ProgressEvent` envelopes: `data.message.id` (the Rust
 *     `ProgressMessage` struct names its unique id `id`, not
 *     `event_id`)
 *   • Other variants that set their own id: `data.event_id`
 * Falls back to a composite of inner event_type + timestamp + agent_id
 * when no explicit id is reachable (older or hand-crafted events).
 */
export function eventDedupeKey(raw: RawTurnEvent): string | null {
    const root = raw as Record<string, unknown>;
    const data = root.data as Record<string, unknown> | undefined;
    const innerEvent = data?.event as Record<string, unknown> | undefined;
    const payload = innerEvent?.payload as Record<string, unknown> | undefined;
    const agentId = payload?.event_id;
    if (typeof agentId === 'string' && agentId.length > 0) return agentId;

    const message = data?.message as Record<string, unknown> | undefined;
    const progressId = message?.id;
    if (typeof progressId === 'string' && progressId.length > 0) return progressId;

    const topLevelId = data?.event_id;
    if (typeof topLevelId === 'string' && topLevelId.length > 0) return topLevelId;

    // Typed transport variants (`LLMRequestSent`, `LLMResponseReceived`,
    // `AgenticActionExecuted`, …) have NO inner envelope — their fields live
    // directly under `data`, and they carry no `event_id`/inner `event_type`.
    // The old `if (!innerType) return null` dropped EVERY one of them at
    // merge time (null key → never pushed), so a delegated agent's steps
    // never reached the activity card. Key them off the OUTER `event_type`
    // plus the event's own data discriminators (execution_id + iteration/
    // step + timestamp) so distinct per-iteration events stay distinct.
    const innerType = (innerEvent?.event_type as string | undefined) ?? '';
    const outerType = (root.event_type as string | undefined) ?? '';
    const type = innerType || outerType;
    if (!type) return null;
    const ts = eventTimestampMs(raw) ?? 0;
    const agent =
        (innerEvent?.agent_id as string | undefined) ??
        (data?.agent_id as string | undefined) ??
        '';
    const execId = (data?.execution_id as string | undefined) ?? '';
    const disc =
        (data?.iteration as number | string | undefined) ??
        (data?.step_index as number | string | undefined) ??
        (data?.call_id as string | undefined) ??
        '';
    return `${agent}|${type}|${execId}|${disc}|${ts}`;
}

/**
 * Resolves the real event timestamp from a wrapped envelope. The top-
 * level `timestamp_ms` is undefined; the value lives at
 * `data.event.payload.timestamp_ms` (or `data.event.timestamp` as
 * fallback for older shapes).
 */
export function eventTimestampMs(raw: RawTurnEvent): number | null {
    const data = (raw as Record<string, unknown>).data as
        | Record<string, unknown>
        | undefined;
    const innerEvent = data?.event as Record<string, unknown> | undefined;
    const payload = innerEvent?.payload as Record<string, unknown> | undefined;
    const candidates: unknown[] = [
        payload?.timestamp_ms,
        innerEvent?.timestamp,
        innerEvent?.timestamp_ms,
        // Typed transport variants (no inner envelope) put their own time
        // directly under `data` (`data.timestamp` / `data.timestamp_ms`).
        data?.timestamp_ms,
        data?.timestamp,
        (raw as Record<string, unknown>).timestamp_ms,
    ];
    for (const c of candidates) {
        if (typeof c === 'number' && Number.isFinite(c)) return c;
    }
    return null;
}

/**
 * Non-reactive read for one-shot consumers (debug, export, etc.).
 * Reactive consumers should use `$chatTurnEventsStore.get(id)`.
 */
export function getEventsForChatTurn(chatTurnId: string): RawTurnEvent[] {
    return get(eventsByTurnId).get(chatTurnId) ?? [];
}

/**
 * REST page fetch — pulls a slice of historical events for a chat turn.
 * Use for non-in-flight turns (historical chat). Merges into the same
 * store the SSE path writes to, so card readers don't care which
 * source the events came from.
 *
 * `before` paginates older: pass the earliest `timestamp_ms` you've
 * already shown to get the next page going back in time. Omit on
 * first load to get the latest N.
 *
 * Returns the earliest timestamp in the fetched page (useful for the
 * next `before` cursor), or `null` if the page was empty / failed.
 */
export async function fetchEventsPage(
    scope: SubscribeScope,
    sessionId: string,
    chatTurnId: string,
    _options: { limit?: number; before?: number } = {}
): Promise<{ count: number; earliestTs: number | null }> {
    if (!scope?.principal || !scope?.workspace || !sessionId || !chatTurnId) {
        return { count: 0, earliestTs: null };
    }
    // Per-chat-turn event store endpoint. Returns the FULL event log
    // for the turn (no pagination — typical turns are small enough; the
    // store file is per-turn so size is bounded by what the turn
    // actually emitted). The legacy `/api/magician/v3/events/page` was
    // deleted; the activity card no longer reads from the per-scope
    // events.jsonl. `_options` is retained for call-site stability but
    // unused — pagination isn't applicable to a per-turn projection.
    const params = new URLSearchParams();
    const url = `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/turns/${encodeURIComponent(chatTurnId)}/events?${params.toString()}`;
    try {
        const response = await timedFetch(url, {
        });
        if (!response.ok) return { count: 0, earliestTs: null };
        const body = (await response.json()) as {
            events?: RawTurnEvent[];
            count?: number;
            total?: number;
        };
        const fetched = body.events ?? [];
        if (fetched.length === 0) return { count: 0, earliestTs: null };
        let earliest: number | null = null;
        eventsByTurnId.update((current) => {
            const existing = current.get(chatTurnId) ?? [];
            // Dedupe + sort: every wrapped event has top-level
            // `event_type: "AgentEvent"` and NO top-level `timestamp_ms`
            // — the real values live at `data.event.event_type` /
            // `data.event.payload.timestamp_ms`. Using the wrapper
            // fields produced "AgentEvent|undefined" for every event,
            // so the Set matched the first row against every
            // subsequent row and the sort comparator always returned 0.
            // Net effect: events stayed in arrival order (which is NOT
            // chronological after fan-out re-emits) and a single REST
            // page could drop most rows or end up with `.succeeded`
            // before `.requested`, causing `deriveLeaves.patch` to
            // no-op silently and the card to render empty.
            //
            // The chat-fanout re-stamping ALSO duplicates each event
            // (one primary emit, one chat-scoped clone), so the dedupe
            // key needs a stable per-event id. `data.event.payload.event_id`
            // is unique per logical event; fall back to composite if
            // missing.
            const seen = new Set<string>();
            for (const e of existing) {
                const k = eventDedupeKey(e);
                if (k) seen.add(k);
            }
            const merged = [...existing];
            for (const e of fetched) {
                const k = eventDedupeKey(e);
                if (k && !seen.has(k)) {
                    seen.add(k);
                    merged.push(e);
                }
                const t = eventTimestampMs(e);
                if (t !== null && (earliest === null || t < earliest)) {
                    earliest = t;
                }
            }
            merged.sort((a, b) => {
                const ta = eventTimestampMs(a) ?? 0;
                const tb = eventTimestampMs(b) ?? 0;
                return ta - tb;
            });
            const next = new Map(current);
            next.set(chatTurnId, merged);
            return next;
        });
        return { count: fetched.length, earliestTs: earliest };
    } catch {
        return { count: 0, earliestTs: null };
    }
}
