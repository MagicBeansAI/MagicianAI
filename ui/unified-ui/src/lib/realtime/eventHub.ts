/**
 * The event hub: the app's ONE realtime connection.
 *
 * Contract: `docs/archive/plans/2026-10-01-single-event-hub.md`. Features never open a
 * realtime socket or a live `/v3/events` / chat-turn stream of their own; they
 * `subscribe` here and keep their history as one REST request per mount (and
 * one more per resync).
 *
 * The hub owns the `/api/magician/v2/realtime/ws` socket:
 *   - reconnects forever with jittered backoff capped at 30 s, and at once when
 *     the page becomes visible again or the browser comes back online;
 *   - resumes with `resume_epoch` / `resume_seq` when the server has said hello;
 *   - dedupes frames by `(epoch, seq)`;
 *   - fans `hub.resync`, and any reconnect that did not resume, out to every
 *     subscriber's `onResync`.
 *
 * It also works against a server that sends none of the hub fields: no
 * `hub.hello` means no resume, so every reconnect resyncs; `categories`,
 * `user_relevant` and `refs` are computed here from the payload with the same
 * rules `/v3/events` uses (`magician-api/src/events_api.rs`).
 *
 * `v2Events` (`v2-websocket.ts`) is a frame listener on this hub and keeps
 * routing frames into the app's stores. A separate webview (the notify
 * overlay) loads this module without `v2-websocket.ts` and so gets its own hub
 * and its own single socket.
 */
import { writable, type Readable } from 'svelte/store';
import {
	scopeIdentityStore,
	getCurrentScopeCredentialRevision,
	scopedMagicianWebSocketUrl,
	scopedWebSocketProtocols
} from '$lib/stores/scopeIdentityStore';
import { createBackoff, type Backoff } from './backoff';
import { EVENT_TAXONOMY } from './event-taxonomy';

export const MAGICIAN_REALTIME_WEBSOCKET_PROTOCOL = 'magician-events-v2';
const REALTIME_WS_PATH = '/api/magician/v2/realtime/ws';

// ─── Event model ────────────────────────────────────────────────────────────

export interface HubRefs {
	task_id?: string;
	execution_id?: string;
	chat_turn_id?: string;
	agent_id?: string;
	session_id?: string;
}

export type HubRefKey = keyof HubRefs;

/** `AgentEvent` envelope, unwrapped from `data.event`. */
export interface HubAgentEnvelope {
	event_type: string;
	agent_id?: string;
	payload: Record<string, unknown> | undefined;
	[key: string]: unknown;
}

export interface HubEvent {
	/** The frame exactly as received (`{event_type, data, …}`). */
	readonly raw: Record<string, unknown>;
	/** Outer `event_type` (`AgentEvent` for envelopes). */
	readonly eventType: string;
	/** Effective type: the inner `event.event_type` for `AgentEvent`, else `eventType`. */
	readonly type: string;
	readonly data: Record<string, unknown> | undefined;
	/** `data.event` for `AgentEvent` frames. */
	readonly inner: HubAgentEnvelope | undefined;
	/** `inner.payload` for envelopes, else `data`. */
	readonly payload: Record<string, unknown> | undefined;
	readonly seq: number | undefined;
	readonly epoch: string | undefined;
	/** Category tokens. Empty when the event type is not classified. */
	readonly categories: readonly string[];
	/** False when neither the server nor the taxonomy classifies the type:
	 *  category / user-relevance filters then soft-pass, as `/v3/events` does. */
	readonly classified: boolean;
	readonly userRelevant: boolean | undefined;
	readonly refs: HubRefs;
	/** Same as `refs[key]`, read lazily. */
	ref(key: HubRefKey): string | undefined;
}

export interface HubFilter {
	categories?: readonly string[];
	/** Prefix match on the outer `event_type` and on the inner envelope type. */
	eventTypes?: readonly string[];
	taskId?: string | readonly string[];
	executionId?: string | readonly string[];
	chatTurnId?: string | readonly string[];
	agentId?: string | readonly string[];
	sessionId?: string | readonly string[];
	userRelevant?: boolean;
}

export type HubPredicate = (event: HubEvent) => boolean;
export type ResyncReason = 'gap' | 'lagged' | 'epoch' | 'reconnect' | 'scope' | string;

function asRecord(value: unknown): Record<string, unknown> | undefined {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: undefined;
}

function nonEmptyString(value: unknown): string | undefined {
	return typeof value === 'string' && value.length > 0 ? value : undefined;
}

/** The layers `/v3/events` and the turn sink read ids from, in order. */
function refLayers(frame: Record<string, unknown>): Array<Record<string, unknown> | undefined> {
	const data = asRecord(frame.data);
	const event = asRecord(data?.event);
	const message = asRecord(data?.message);
	return [
		frame,
		data,
		asRecord(data?.item),
		event,
		asRecord(event?.payload),
		message,
		asRecord(message?.metadata),
		asRecord(data?.payload),
		asRecord(frame.payload)
	];
}

const REF_ALIASES: Record<HubRefKey, readonly string[]> = {
	task_id: ['task_id'],
	execution_id: ['execution_id'],
	chat_turn_id: ['chat_turn_id'],
	agent_id: ['agent_id'],
	session_id: ['session_id', 'chat_session_id']
};

/** Bounded deep scan, the client twin of the turn sink's `find_string_field`. */
function deepFindString(root: unknown, name: string, maxDepth = 8): string | undefined {
	const stack: Array<{ node: unknown; depth: number }> = [{ node: root, depth: 0 }];
	while (stack.length > 0) {
		const { node, depth } = stack.pop()!;
		if (Array.isArray(node)) {
			if (depth < maxDepth) for (const child of node) stack.push({ node: child, depth: depth + 1 });
			continue;
		}
		const record = asRecord(node);
		if (!record) continue;
		const hit = nonEmptyString(record[name]);
		if (hit) return hit;
		if (depth < maxDepth) {
			for (const child of Object.values(record)) {
				if (typeof child === 'object' && child !== null) stack.push({ node: child, depth: depth + 1 });
			}
		}
	}
	return undefined;
}

/**
 * Read one ref from the payload when the server did not send it. Agent id
 * prefers the envelope's own (`unwrap_agent_id`); chat turn id falls back to a
 * deep scan, as the turn sink does, because it is a payload property that can
 * sit anywhere.
 */
export function extractRefFromPayload(frame: Record<string, unknown>, key: HubRefKey): string | undefined {
	const layers = refLayers(frame);
	if (key === 'agent_id') {
		const envelopeAgent = nonEmptyString(asRecord(asRecord(frame.data)?.event)?.agent_id);
		if (envelopeAgent) return envelopeAgent;
	}
	for (const alias of REF_ALIASES[key]) {
		for (const layer of layers) {
			const value = nonEmptyString(layer?.[alias]);
			if (value) return value;
		}
	}
	if (key === 'chat_turn_id') return deepFindString(frame.data, 'chat_turn_id');
	return undefined;
}

/** Inner type for `AgentEvent` envelopes, mirroring `events_api::unwrap_event_type`. */
export function unwrapEventType(frame: Record<string, unknown>): string {
	const outer = typeof frame.event_type === 'string' ? frame.event_type : '';
	if (outer === 'AgentEvent') {
		const inner = asRecord(asRecord(frame.data)?.event)?.event_type;
		if (typeof inner === 'string' && inner.length > 0) return inner;
	}
	return outer;
}

class HubEventImpl implements HubEvent {
	readonly raw: Record<string, unknown>;
	readonly eventType: string;
	readonly type: string;
	readonly data: Record<string, unknown> | undefined;
	readonly inner: HubAgentEnvelope | undefined;
	readonly payload: Record<string, unknown> | undefined;
	readonly seq: number | undefined;
	readonly epoch: string | undefined;
	readonly categories: readonly string[];
	readonly classified: boolean;
	readonly userRelevant: boolean | undefined;
	private readonly serverRefs: Record<string, unknown> | undefined;
	private readonly refCache = new Map<HubRefKey, string | undefined>();

	constructor(frame: Record<string, unknown>) {
		this.raw = frame;
		this.eventType = typeof frame.event_type === 'string' ? frame.event_type : '';
		this.type = unwrapEventType(frame);
		this.data = asRecord(frame.data);
		const innerRecord = this.eventType === 'AgentEvent' ? asRecord(this.data?.event) : undefined;
		this.inner =
			innerRecord && typeof innerRecord.event_type === 'string'
				? ({ ...innerRecord, payload: asRecord(innerRecord.payload) } as HubAgentEnvelope)
				: undefined;
		this.payload = this.inner ? this.inner.payload : this.data;
		this.seq = typeof frame.seq === 'number' && Number.isFinite(frame.seq) ? frame.seq : undefined;
		this.epoch = nonEmptyString(frame.epoch);
		this.serverRefs = asRecord(frame.refs);

		const serverCategories = Array.isArray(frame.categories)
			? frame.categories.filter((c): c is string => typeof c === 'string' && c.length > 0)
			: undefined;
		if (serverCategories && serverCategories.length > 0) {
			this.categories = serverCategories;
			this.classified = true;
			this.userRelevant =
				typeof frame.user_relevant === 'boolean'
					? frame.user_relevant
					: EVENT_TAXONOMY[this.type]?.user_relevant;
		} else {
			// Fallback classifier — `events_api::serialize_event_if_passes`:
			// the unwrapped type is looked up in the shared taxonomy; an
			// unrecognised type is "unclassified" and soft-passes the taxonomy
			// filters instead of being silently dropped by them.
			const taxonomy = EVENT_TAXONOMY[this.type];
			this.categories = taxonomy ? [taxonomy.category] : [];
			this.classified = !!taxonomy;
			this.userRelevant =
				typeof frame.user_relevant === 'boolean' ? frame.user_relevant : taxonomy?.user_relevant;
		}
	}

	ref(key: HubRefKey): string | undefined {
		if (this.refCache.has(key)) return this.refCache.get(key);
		const value = nonEmptyString(this.serverRefs?.[key]) ?? extractRefFromPayload(this.raw, key);
		this.refCache.set(key, value);
		return value;
	}

	get refs(): HubRefs {
		const out: HubRefs = {};
		for (const key of Object.keys(REF_ALIASES) as HubRefKey[]) {
			const value = this.ref(key);
			if (value) out[key] = value;
		}
		return out;
	}
}

/** Build the hub's view of one frame. Exported for consumers that ingest REST
 *  backfill rows through the same code path as live events. */
export function toHubEvent(frame: Record<string, unknown>): HubEvent {
	return new HubEventImpl(frame);
}

function asList(value: string | readonly string[] | undefined): readonly string[] | undefined {
	if (value === undefined) return undefined;
	return typeof value === 'string' ? [value] : value;
}

const ID_FILTERS: Array<[keyof HubFilter, HubRefKey]> = [
	['taskId', 'task_id'],
	['executionId', 'execution_id'],
	['chatTurnId', 'chat_turn_id'],
	['agentId', 'agent_id'],
	['sessionId', 'session_id']
];

export function matchesHubFilter(event: HubEvent, filter: HubFilter): boolean {
	if (filter.eventTypes && filter.eventTypes.length > 0) {
		const hit = filter.eventTypes.some(
			(prefix) => event.eventType.startsWith(prefix) || event.type.startsWith(prefix)
		);
		if (!hit) return false;
	}
	if (event.classified) {
		if (filter.categories && filter.categories.length > 0) {
			if (!filter.categories.some((c) => event.categories.includes(c))) return false;
		}
		if (filter.userRelevant !== undefined && event.userRelevant !== undefined) {
			if (event.userRelevant !== filter.userRelevant) return false;
		}
	}
	for (const [filterKey, refKey] of ID_FILTERS) {
		const wanted = asList(filter[filterKey] as string | readonly string[] | undefined);
		if (!wanted || wanted.length === 0) continue;
		const got = event.ref(refKey);
		if (!got || !wanted.includes(got)) return false;
	}
	return true;
}

/**
 * The `/v3/events` live-tail filters (`events_api::CompiledFilters` +
 * `serialize_event_if_passes`) as a hub predicate, for surfaces that used to
 * pass them as query params. Taxonomy filters (category, severity,
 * user_relevant) apply only to classified types — an unknown type soft-passes
 * them, as on the server. `eventType` and `search` are case-insensitive
 * substring matches (on the unwrapped type and on the serialized event).
 * `agentId` matches the envelope's agent, else a top-level `agent_id`
 * (`unwrap_agent_id`). Like the server's live tail, this never filters by
 * task or execution: those params only bounded the backfill.
 */
export interface V3EventsFilter {
	categories?: readonly string[];
	severities?: readonly string[];
	userRelevant?: boolean;
	eventType?: string;
	agentId?: string;
	search?: string;
}

export function v3EventsPredicate(filter: V3EventsFilter): HubPredicate {
	const categories = filter.categories?.filter(Boolean) ?? [];
	const severities = filter.severities?.filter(Boolean) ?? [];
	const typeNeedle = filter.eventType?.trim().toLowerCase() || '';
	const searchNeedle = filter.search?.trim().toLowerCase() || '';
	const agentWanted = filter.agentId?.trim() || '';
	return (event) => {
		const taxonomy = EVENT_TAXONOMY[event.type];
		if (taxonomy) {
			if (categories.length > 0 && !categories.includes(taxonomy.category)) return false;
			if (severities.length > 0 && !severities.includes(taxonomy.severity)) return false;
			if (filter.userRelevant !== undefined && taxonomy.user_relevant !== filter.userRelevant) {
				return false;
			}
		}
		if (typeNeedle && !event.type.toLowerCase().includes(typeNeedle)) return false;
		if (agentWanted) {
			const agent =
				nonEmptyString(asRecord(event.data?.event)?.agent_id) ?? nonEmptyString(event.raw.agent_id);
			if (agent !== agentWanted) return false;
		}
		if (searchNeedle && !canonicalEventLine(event.raw).toLowerCase().includes(searchNeedle)) {
			return false;
		}
		return true;
	};
}

/**
 * The event serialized as the `/v3/events` NDJSON line (`{"event_type","data"}`
 * in that order), without the hub's frame fields. Live frames and backfill
 * rows of the same event produce the same string, so surfaces that dedupe
 * backfill against live by line keep working.
 */
export function canonicalEventLine(frame: Record<string, unknown>): string {
	if (!('event_type' in frame) || !('data' in frame)) return JSON.stringify(frame);
	const extra = Object.keys(frame).filter(
		(key) =>
			key !== 'event_type' &&
			key !== 'data' &&
			key !== 'seq' &&
			key !== 'epoch' &&
			key !== 'categories' &&
			key !== 'user_relevant' &&
			key !== 'refs'
	);
	if (extra.length === 0) return JSON.stringify({ event_type: frame.event_type, data: frame.data });
	const out: Record<string, unknown> = { event_type: frame.event_type, data: frame.data };
	for (const key of extra) out[key] = frame[key];
	return JSON.stringify(out);
}

// ─── Hub ────────────────────────────────────────────────────────────────────

export type ConnectionStatus = 'connected' | 'connecting' | 'disconnected';

interface Subscriber {
	match: HubPredicate;
	onEvent: (event: HubEvent) => void;
	onResync?: (reason: ResyncReason) => void;
}

export type FrameListener = (frame: Record<string, unknown>) => void;

export interface EventHubOptions {
	/** WebSocket constructor; defaults to the global one at dial time. */
	webSocket?: () => typeof WebSocket | undefined;
	/** Full socket URL for the given query string. */
	url?: (query: string) => string;
	protocols?: () => string[];
	/** How long to wait for `hub.hello` on a reconnect before treating the
	 *  server as pre-hub and resyncing. */
	helloTimeoutMs?: number;
	maxBackoffMs?: number;
	/** Bind visibility/online listeners and the scope store (browser only). */
	bindEnvironment?: boolean;
	dedupeWindow?: number;
}

function defaultUrl(query: string): string {
	const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
	const host = window.location.hostname;
	const port = window.location.port || (protocol === 'wss:' ? '443' : '80');
	// In dev, dial the backend (3002) directly: SvelteKit's dev server
	// intercepts WebSocket upgrades before Vite's proxy can forward them.
	const backendPort = import.meta.env.DEV ? '3002' : port;
	return scopedMagicianWebSocketUrl(`${REALTIME_WS_PATH}${query}`, `${protocol}//${host}:${backendPort}`);
}

export class EventHub {
	private ws: WebSocket | null = null;
	private subscribers = new Set<Subscriber>();
	private frameListeners = new Set<FrameListener>();
	private openListeners = new Set<() => void>();

	private readonly status = writable<ConnectionStatus>('disconnected');
	readonly connectionStatus: Readable<ConnectionStatus> = { subscribe: this.status.subscribe };
	isConnected = false;

	private wanted = false;
	private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	private pingTimer: ReturnType<typeof setInterval> | null = null;
	private helloTimer: ReturnType<typeof setTimeout> | null = null;
	private readonly backoff: Backoff;
	private attempts = 0;

	// Resume / dedupe state.
	private epoch: string | null = null;
	private lastSeq: number | null = null;
	private seen = new Set<string>();
	private seenOrder: string[] = [];
	private hasOpened = false;
	private awaitingHello = false;
	private connectionIsReconnect = false;
	private suppressNextServerResync = false;
	private resumeRequested = false;

	/** Pending-pause replay target: the server re-emits a paused execution's
	 *  `WaitingForUser` events on connect when the dial names it. */
	private replayExecutionId: string | null = null;
	private replayAgentId: string | null = null;

	private scopeKey = '';
	private environmentBound = false;
	private readonly opts: Required<Omit<EventHubOptions, 'webSocket' | 'url' | 'protocols'>> &
		Pick<EventHubOptions, 'webSocket' | 'url' | 'protocols'>;

	constructor(options: EventHubOptions = {}) {
		this.opts = {
			helloTimeoutMs: options.helloTimeoutMs ?? 2_000,
			maxBackoffMs: options.maxBackoffMs ?? 30_000,
			bindEnvironment: options.bindEnvironment ?? typeof window !== 'undefined',
			dedupeWindow: options.dedupeWindow ?? 8_192,
			webSocket: options.webSocket,
			url: options.url,
			protocols: options.protocols
		};
		this.backoff = createBackoff({ initialMs: 1_000, maxMs: this.opts.maxBackoffMs });
	}

	// ── Subscriptions ──

	/**
	 * Receive every event that matches `filter` (or `predicate`). `onResync`
	 * runs when the hub may have missed events: re-read your state with one
	 * REST request. Opens the shared socket when it is not open yet.
	 */
	subscribe(
		filter: HubFilter | HubPredicate,
		onEvent: (event: HubEvent) => void,
		onResync?: (reason: ResyncReason) => void
	): () => void {
		const match: HubPredicate =
			typeof filter === 'function' ? filter : (event) => matchesHubFilter(event, filter);
		const subscriber: Subscriber = { match, onEvent, onResync };
		this.subscribers.add(subscriber);
		this.ensureConnected();
		return () => {
			this.subscribers.delete(subscriber);
		};
	}

	/** Raw frame tap (every frame after dedupe, control frames excluded). */
	addFrameListener(listener: FrameListener): () => void {
		this.frameListeners.add(listener);
		return () => this.frameListeners.delete(listener);
	}

	addOpenListener(listener: () => void): () => void {
		this.openListeners.add(listener);
		return () => this.openListeners.delete(listener);
	}

	subscriberCount(): number {
		return this.subscribers.size;
	}

	// ── Connection lifecycle ──

	/** Open the socket if it is not open, connecting or already scheduled. */
	ensureConnected(): void {
		this.bindEnvironment();
		this.wanted = true;
		if (this.reconnectTimer !== null) return;
		const state = this.ws?.readyState;
		if (state === 0 /* CONNECTING */ || state === 1 /* OPEN */) return;
		this.dial();
	}

	/**
	 * Name a paused execution / agent whose pending `WaitingForUser` events the
	 * server should re-emit. Never re-targets an open socket: the hint rides
	 * the next dial (a cold start or any reconnect). Pass nothing to clear.
	 */
	setReplayTarget(executionId?: string | null, agentId?: string | null): void {
		this.replayExecutionId = executionId || null;
		this.replayAgentId = agentId || null;
	}

	/** Drop the socket and dial again now (scope change, half-open socket). */
	reconnectNow(): void {
		if (!this.wanted) return;
		this.teardownSocket(4001, 'Client reconnect');
		this.clearReconnectTimer();
		this.dial();
	}

	/** Stop for good until the next `ensureConnected` / `subscribe`. */
	disconnect(): void {
		this.wanted = false;
		this.clearReconnectTimer();
		this.clearHelloTimer();
		this.teardownSocket(1000, 'Client disconnecting');
		this.attempts = 0;
		this.backoff.reset();
	}

	send(message: Record<string, unknown>): boolean {
		if (this.ws && this.ws.readyState === 1) {
			try {
				this.ws.send(JSON.stringify(message));
				return true;
			} catch {
				this.scheduleReconnect();
				return false;
			}
		}
		return false;
	}

	getConnectionState(): string {
		switch (this.ws?.readyState) {
			case 0:
				return 'CONNECTING';
			case 1:
				return 'OPEN';
			case 2:
				return 'CLOSING';
			default:
				return 'CLOSED';
		}
	}

	/** Resume position, for tests and diagnostics. */
	resumePosition(): { epoch: string | null; seq: number | null } {
		return { epoch: this.epoch, seq: this.lastSeq };
	}

	private bindEnvironment(): void {
		if (this.environmentBound || !this.opts.bindEnvironment || typeof window === 'undefined') return;
		this.environmentBound = true;
		const wake = () => {
			if (!this.wanted) return;
			if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;
			const state = this.ws?.readyState;
			if (state === 0 || state === 1) return;
			// Foreground / back online: retry now, from the fast end of the backoff.
			this.clearReconnectTimer();
			this.backoff.reset();
			this.dial();
		};
		if (typeof document !== 'undefined') document.addEventListener('visibilitychange', wake);
		window.addEventListener('online', wake);
		window.addEventListener('focus', wake);
		scopeIdentityStore.subscribe((scope) => {
			const next = `${scope.principal.trim()}:${scope.workspace.trim()}:${getCurrentScopeCredentialRevision()}`;
			if (!this.scopeKey) {
				this.scopeKey = next;
				return;
			}
			if (next === this.scopeKey) return;
			this.scopeKey = next;
			// A new scope is a new event space: no resume, every subscriber re-reads.
			this.epoch = null;
			this.lastSeq = null;
			this.clearSeen();
			if (this.wanted && (this.ws || this.reconnectTimer)) this.reconnectNow();
			this.fanOutResync('scope');
		});
	}

	private buildQuery(): string {
		const params = new URLSearchParams();
		if (this.replayExecutionId) params.set('execution_id', this.replayExecutionId);
		if (this.replayAgentId) params.set('agent_id', this.replayAgentId);
		params.set('supports_structured_presentation', 'true');
		this.resumeRequested = false;
		if (this.epoch && this.lastSeq !== null) {
			params.set('resume_epoch', this.epoch);
			params.set('resume_seq', String(this.lastSeq));
			this.resumeRequested = true;
		}
		return `?${params.toString()}`;
	}

	private dial(): void {
		if (typeof window !== 'undefined' && (window as { __MAGICIAN_MISSING__?: unknown }).__MAGICIAN_MISSING__) {
			return;
		}
		const Ctor = this.opts.webSocket ? this.opts.webSocket() : (globalThis as { WebSocket?: typeof WebSocket }).WebSocket;
		if (!Ctor) return;
		// Never orphan a previous socket: its late onclose would schedule a
		// second reconnect and connections would multiply against a flapping
		// backend.
		this.teardownSocket(4002, 'Superseded by new connection');
		this.status.set('connecting');
		let socket: WebSocket;
		try {
			const query = this.buildQuery();
			const url = (this.opts.url ?? defaultUrl)(query);
			const protocols = this.opts.protocols
				? this.opts.protocols()
				: scopedWebSocketProtocols([MAGICIAN_REALTIME_WEBSOCKET_PROTOCOL]);
			socket = new Ctor(url, protocols);
		} catch (error) {
			console.warn('[eventHub] socket creation failed', error);
			this.status.set('disconnected');
			this.scheduleReconnect();
			return;
		}
		this.ws = socket;
		socket.onopen = () => this.handleOpen(socket);
		socket.onmessage = (event: MessageEvent) => this.handleMessage(socket, event);
		socket.onerror = () => {
			if (this.ws !== socket) return;
			this.isConnected = false;
			this.status.set('disconnected');
			// Some network failures fire onerror without onclose.
			if (this.wanted && this.reconnectTimer === null) this.scheduleReconnect();
		};
		socket.onclose = () => {
			if (this.ws !== socket) return;
			this.isConnected = false;
			this.status.set('disconnected');
			this.stopPing();
			this.clearHelloTimer();
			if (this.wanted) this.scheduleReconnect();
		};
	}

	private handleOpen(socket: WebSocket): void {
		if (this.ws !== socket) return;
		this.isConnected = true;
		this.status.set('connected');
		this.attempts = 0;
		this.backoff.reset();
		this.connectionIsReconnect = this.hasOpened;
		this.hasOpened = true;
		this.suppressNextServerResync = false;
		this.awaitingHello = true;
		this.startPing();
		if (this.connectionIsReconnect) {
			// A pre-hub server never says hello; do not wait forever to find out.
			this.clearHelloTimer();
			this.helloTimer = setTimeout(() => {
				this.helloTimer = null;
				if (this.ws === socket && this.awaitingHello) this.legacyServerDetected();
			}, this.opts.helloTimeoutMs);
		}
		for (const listener of [...this.openListeners]) {
			try {
				listener();
			} catch (error) {
				console.warn('[eventHub] open listener failed', error);
			}
		}
	}

	private legacyServerDetected(): void {
		this.awaitingHello = false;
		this.clearHelloTimer();
		// No hello → no resume position, so a reconnect may have lost events.
		this.epoch = null;
		this.lastSeq = null;
		if (this.connectionIsReconnect) this.fanOutResync('reconnect');
	}

	private handleMessage(socket: WebSocket, event: MessageEvent): void {
		if (this.ws !== socket) return;
		let message: Record<string, unknown>;
		try {
			const parsed = JSON.parse(typeof event.data === 'string' ? event.data : String(event.data));
			const record = asRecord(parsed);
			if (!record) return;
			message = record;
		} catch (error) {
			console.error('[eventHub] unparseable frame', error);
			return;
		}
		this.ingestFrame(message);
	}

	/** Process one parsed frame. Public for tests and for in-process feeds. */
	ingestFrame(message: Record<string, unknown>): void {
		if (message.type === 'hub.hello') {
			this.handleHello(message);
			return;
		}
		if (message.type === 'hub.resync') {
			this.awaitingHello = false;
			this.clearHelloTimer();
			if (this.suppressNextServerResync) {
				this.suppressNextServerResync = false;
				return;
			}
			this.fanOutResync(typeof message.reason === 'string' ? message.reason : 'gap');
			return;
		}
		if (this.awaitingHello) {
			// First frame is not a hello: the server predates the hub.
			this.legacyServerDetected();
		}
		this.suppressNextServerResync = false;

		const seq = typeof message.seq === 'number' && Number.isFinite(message.seq) ? message.seq : undefined;
		const epoch = nonEmptyString(message.epoch);
		if (seq !== undefined && epoch) {
			const key = `${epoch}:${seq}`;
			if (this.seen.has(key)) return;
			this.remember(key);
			if (epoch === this.epoch || this.epoch === null) {
				this.epoch = epoch;
				if (this.lastSeq === null || seq > this.lastSeq) this.lastSeq = seq;
			}
		}

		for (const listener of [...this.frameListeners]) {
			try {
				listener(message);
			} catch (error) {
				console.error('[eventHub] frame listener failed', error);
			}
		}

		if (typeof message.event_type !== 'string' || message.data == null) return;
		if (this.subscribers.size === 0) return;
		const hubEvent = toHubEvent(message);
		for (const subscriber of [...this.subscribers]) {
			if (!this.subscribers.has(subscriber)) continue;
			let matched = false;
			try {
				matched = subscriber.match(hubEvent);
			} catch (error) {
				console.error('[eventHub] subscriber filter failed', error);
			}
			if (!matched) continue;
			try {
				subscriber.onEvent(hubEvent);
			} catch (error) {
				console.error('[eventHub] subscriber failed', error);
			}
		}
	}

	private handleHello(message: Record<string, unknown>): void {
		this.awaitingHello = false;
		this.clearHelloTimer();
		const epoch = nonEmptyString(message.epoch) ?? null;
		const seq = typeof message.seq === 'number' && Number.isFinite(message.seq) ? message.seq : null;
		const resumed = message.resumed === true;
		const epochChanged = epoch !== null && epoch !== this.epoch;
		if (epochChanged) this.clearSeen();
		if (!resumed || epochChanged) {
			this.epoch = epoch;
			this.lastSeq = seq;
		}
		if (this.connectionIsReconnect && !resumed) {
			this.fanOutResync(epochChanged && this.resumeRequested ? 'epoch' : 'reconnect');
			// The server follows an unsatisfied resume with its own hub.resync;
			// subscribers have already been told once.
			this.suppressNextServerResync = this.resumeRequested;
		}
	}

	private fanOutResync(reason: ResyncReason): void {
		for (const subscriber of [...this.subscribers]) {
			if (!subscriber.onResync) continue;
			try {
				subscriber.onResync(reason);
			} catch (error) {
				console.error('[eventHub] resync handler failed', error);
			}
		}
	}

	private remember(key: string): void {
		this.seen.add(key);
		this.seenOrder.push(key);
		if (this.seenOrder.length > this.opts.dedupeWindow) {
			const evicted = this.seenOrder.splice(0, this.seenOrder.length - this.opts.dedupeWindow);
			for (const old of evicted) this.seen.delete(old);
		}
	}

	private clearSeen(): void {
		this.seen.clear();
		this.seenOrder = [];
	}

	private scheduleReconnect(): void {
		if (!this.wanted) return;
		this.clearReconnectTimer();
		this.attempts += 1;
		const delay = this.backoff.nextMs();
		this.status.set('connecting');
		this.reconnectTimer = setTimeout(() => {
			this.reconnectTimer = null;
			if (!this.wanted) return;
			this.dial();
		}, delay);
	}

	/** Reconnect attempts since the last successful open (diagnostics). */
	reconnectAttempts(): number {
		return this.attempts;
	}

	private clearReconnectTimer(): void {
		if (this.reconnectTimer !== null) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
	}

	private clearHelloTimer(): void {
		if (this.helloTimer !== null) {
			clearTimeout(this.helloTimer);
			this.helloTimer = null;
		}
	}

	private startPing(): void {
		this.stopPing();
		// Zombie-socket check plus an app-level ping the server counts as
		// liveness. It is answered with nothing.
		this.pingTimer = setInterval(() => {
			if (this.ws && this.ws.readyState !== 1) {
				this.reconnectNow();
				return;
			}
			this.send({ type: 'ping', ts: Date.now() });
		}, 30_000);
	}

	private stopPing(): void {
		if (this.pingTimer !== null) {
			clearInterval(this.pingTimer);
			this.pingTimer = null;
		}
	}

	private teardownSocket(code: number, reason: string): void {
		this.stopPing();
		const socket = this.ws;
		if (!socket) return;
		this.ws = null;
		socket.onopen = null;
		socket.onmessage = null;
		socket.onerror = null;
		socket.onclose = null;
		try {
			if (socket.readyState === 0 || socket.readyState === 1) socket.close(code, reason);
		} catch (error) {
			console.warn('[eventHub] socket close failed', error);
		}
		this.isConnected = false;
		this.status.set('disconnected');
	}
}

/** The app's hub. One per JS context (each Tauri webview gets its own). */
export const eventHub = new EventHub();

if (import.meta.hot) {
	import.meta.hot.dispose(() => eventHub.disconnect());
}

export function subscribeEvents(
	filter: HubFilter | HubPredicate,
	onEvent: (event: HubEvent) => void,
	onResync?: (reason: ResyncReason) => void
): () => void {
	return eventHub.subscribe(filter, onEvent, onResync);
}

// ─── Live + backfill helper ─────────────────────────────────────────────────

export interface LiveWithBackfillOptions {
	filter: HubFilter | HubPredicate;
	onEvent: (event: HubEvent) => void;
	/**
	 * Load history with ONE REST request and apply it. Runs at mount and again
	 * on every resync. Live events that arrive meanwhile are held and applied
	 * after it settles, so history never lands on top of newer live state.
	 */
	backfill?: (ctx: { signal: AbortSignal; reason: 'mount' | ResyncReason }) => Promise<void> | void;
	/** Extra resync hook, called before the re-backfill. */
	onResync?: (reason: ResyncReason) => void;
	hub?: EventHub;
}

export function subscribeWithBackfill(options: LiveWithBackfillOptions): () => void {
	const hub = options.hub ?? eventHub;
	let closed = false;
	let generation = 0;
	let buffering = false;
	let buffer: HubEvent[] = [];
	let controller: AbortController | null = null;

	const deliver = (event: HubEvent) => {
		try {
			options.onEvent(event);
		} catch (error) {
			console.error('[eventHub] onEvent failed', error);
		}
	};

	const run = (reason: 'mount' | ResyncReason) => {
		if (closed) return;
		if (!options.backfill) return;
		controller?.abort();
		const local = new AbortController();
		controller = local;
		const gen = ++generation;
		buffering = true;
		buffer = [];
		const finish = () => {
			if (closed || gen !== generation) return;
			buffering = false;
			const held = buffer;
			buffer = [];
			for (const event of held) deliver(event);
		};
		let result: Promise<void> | void;
		try {
			result = options.backfill({ signal: local.signal, reason });
		} catch (error) {
			console.warn('[eventHub] backfill failed', error);
			finish();
			return;
		}
		Promise.resolve(result)
			.catch((error) => {
				if (!local.signal.aborted) console.warn('[eventHub] backfill failed', error);
			})
			.finally(finish);
	};

	const unsubscribe = hub.subscribe(
		options.filter,
		(event) => {
			if (closed) return;
			if (buffering) buffer.push(event);
			else deliver(event);
		},
		(reason) => {
			if (closed) return;
			try {
				options.onResync?.(reason);
			} catch (error) {
				console.error('[eventHub] onResync failed', error);
			}
			run(reason);
		}
	);
	run('mount');

	return () => {
		closed = true;
		controller?.abort();
		buffer = [];
		unsubscribe();
	};
}

/** Parse an NDJSON body (a `/v3/events?backfill_only=true` response) into frames,
 *  dropping the `__events_*` control rows. */
export function parseNdjsonFrames(body: string): Record<string, unknown>[] {
	const out: Record<string, unknown>[] = [];
	for (const line of body.split('\n')) {
		const trimmed = line.trim();
		if (!trimmed) continue;
		try {
			const record = asRecord(JSON.parse(trimmed));
			if (!record) continue;
			const type = typeof record.event_type === 'string' ? record.event_type : '';
			if (type.startsWith('__events_')) continue;
			out.push(record);
		} catch {
			// skip unparseable rows
		}
	}
	return out;
}
