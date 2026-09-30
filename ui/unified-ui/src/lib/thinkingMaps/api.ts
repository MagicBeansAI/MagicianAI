/**
 * Live Thinking Map — scoped web API client.
 *
 * Mirrors the pattern of `$lib/internalTasks/api.ts` (a `timedFetch`-backed,
 * `requireOk`-guarded client), but scopes requests via the shared identity
 * store. Every request is authorized by the workspace-bound bearer; query
 * parameters below are endpoint filters only.
 *
 * Base path mounts at `/api/magician/v2` + `/thinking-maps` (see
 * `magician_v2::api::thinking_maps_api::configure`).
 *
 * Errors: non-2xx responses throw an `Error` carrying the JSON body's `error`
 * field when present (falling back to `message`, raw text, then the status).
 */

import { timedFetch } from '$lib/shared/fetch';
import {
	appendCurrentScopeQuery,
	scopedRequestHeaders
} from '$lib/stores/scopeIdentityStore';
import type {
	ApplyOutcome,
	InterpretIntent,
	MapEvent,
	MapLifecycle,
	MapOperation,
	MapSummary,
	PromotionKind,
	ThinkingMap,
	ThinkingMapSource
} from '$lib/types/thinkingMap';

const API_BASE = '/api/magician/v2/thinking-maps';

// ── Error handling ────────────────────────────────────────────────────────────

/**
 * Extract a human-readable error from a failed response body. Prefers the
 * backend's `{"error":"..."}` field (e.g. `not_found`, `validation_failed`,
 * `revision_conflict`), then `message`, then raw text, then the status line.
 */
async function readThinkingMapApiError(response: Response): Promise<string> {
	try {
		const contentType = response.headers.get('content-type') ?? '';
		if (contentType.includes('application/json')) {
			const body = (await response.json()) as { error?: unknown; message?: unknown };
			const message =
				typeof body.error === 'string'
					? body.error
					: typeof body.message === 'string'
						? body.message
						: '';
			if (message.trim()) return message.trim();
		} else {
			const text = await response.text();
			if (text.trim()) return text.trim();
		}
	} catch {
		// Fall through to status below.
	}
	return `HTTP ${response.status}`;
}

async function requireOk(response: Response): Promise<Response> {
	if (!response.ok) throw new Error(await readThinkingMapApiError(response));
	return response;
}

// ── Request helpers ────────────────────────────────────────────────────────────

/** Percent-encoded URL for a GET/DELETE request. */
function queryUrl(path: string, params?: URLSearchParams): string {
	const query = appendCurrentScopeQuery(params).toString();
	return `${API_BASE}${path}${query ? `?${query}` : ''}`;
}

/** Bearer-authorized GET. */
async function getJson<T>(path: string, params?: URLSearchParams): Promise<T> {
	const response = await requireOk(
		await timedFetch(queryUrl(path, params), {
			headers: scopedRequestHeaders({ Accept: 'application/json' })
		})
	);
	return response.json() as Promise<T>;
}

/** Scope-in-header body request (POST/PATCH). */
async function sendJson<T>(
	path: string,
	method: 'POST' | 'PATCH',
	body?: unknown
): Promise<T> {
	const response = await requireOk(
		await timedFetch(`${API_BASE}${path}`, {
			method,
			headers: scopedRequestHeaders({
				'Content-Type': 'application/json',
				Accept: 'application/json'
			}),
			body: body === undefined ? undefined : JSON.stringify(body)
		})
	);
	return response.json() as Promise<T>;
}

/** Scope-in-query DELETE. */
async function deleteJson<T>(path: string, params?: URLSearchParams): Promise<T> {
	const response = await requireOk(
		await timedFetch(queryUrl(path, params), {
			method: 'DELETE',
			headers: scopedRequestHeaders({ Accept: 'application/json' })
		})
	);
	return response.json() as Promise<T>;
}

// ── Endpoints ──────────────────────────────────────────────────────────────────

/** GET / — list every visible (non-deleted) map, most recently updated first. */
export async function listMaps(): Promise<MapSummary[]> {
	return getJson<MapSummary[]>('');
}

/** One page of a map lifecycle view; `total` is the server-filtered count. */
export interface MapSummaryPage {
	maps: MapSummary[];
	total: number;
	offset: number;
	limit: number;
}

/**
 * GET /?limit=&offset=[&lifecycle=] — server-paginated list, most recently
 * updated first (limit is server-clamped to 1..=200). With no lifecycle, the
 * server returns the normal visible list; `deleted` selects tombstones.
 */
export async function listMapsPage(
	limit: number,
	offset = 0,
	lifecycle?: MapLifecycle
): Promise<MapSummaryPage> {
	const params = new URLSearchParams({ limit: String(limit), offset: String(offset) });
	if (lifecycle) params.set('lifecycle', lifecycle);
	return getJson<MapSummaryPage>('', params);
}

/** GET /{id} — load the current materialized map. */
export async function getMap(id: string): Promise<ThinkingMap> {
	return getJson<ThinkingMap>(`/${encodeURIComponent(id)}`);
}

/** POST / — create a new map. Returns the created map (HTTP 201). */
export async function createMap(body: {
	title: string;
	source?: ThinkingMapSource;
	map_id?: string;
}): Promise<ThinkingMap> {
	return sendJson<ThinkingMap>('', 'POST', body);
}

/** PATCH /{id} — owner metadata patch (title and/or lifecycle). */
export async function patchMap(
	id: string,
	body: { title?: string; lifecycle?: MapLifecycle }
): Promise<ApplyOutcome> {
	return sendJson<ApplyOutcome>(`/${encodeURIComponent(id)}`, 'PATCH', body);
}

export interface PermanentlyDeleteMapOutcome {
	deleted: boolean;
	map_id: string;
	detached_sessions: number;
	cleared_tutor_contexts: number;
}

/**
 * DELETE /{id}?confirm=permanent — permanently remove a soft-deleted map and
 * all of its durable history. The server rejects non-deleted maps.
 */
export async function permanentlyDeleteMap(id: string): Promise<PermanentlyDeleteMapOutcome> {
	return deleteJson<PermanentlyDeleteMapOutcome>(
		`/${encodeURIComponent(id)}`,
		new URLSearchParams({ confirm: 'permanent' })
	);
}

/** POST /{id}/operations — apply a batch of owner-authored operations. */
export async function applyOperations(
	id: string,
	body: {
		operations: MapOperation[];
		idempotency_key: string;
		base_revision: number;
		envelope_id?: string;
		utterance_id?: string;
	}
): Promise<ApplyOutcome> {
	return sendJson<ApplyOutcome>(`/${encodeURIComponent(id)}/operations`, 'POST', body);
}

/** POST /{id}/interpret — LLM-interpret one finalized utterance into the map. */
export async function interpret(
	id: string,
	body: {
		text: string;
		utterance_id?: string;
		thread_id?: string;
		intent?: InterpretIntent;
		focus_node_id?: string;
	}
): Promise<ApplyOutcome> {
	return sendJson<ApplyOutcome>(`/${encodeURIComponent(id)}/interpret`, 'POST', body);
}

/** POST /{id}/consolidate — LLM-stage a restructure proposal (nothing else changes). */
export async function consolidate(id: string): Promise<ApplyOutcome> {
	return sendJson<ApplyOutcome>(`/${encodeURIComponent(id)}/consolidate`, 'POST', {});
}

/** POST /{id}/proposals/{proposalId}/decision — owner confirm/reject a proposal. */
export async function decideProposal(
	id: string,
	proposalId: string,
	decision: 'confirm' | 'reject'
): Promise<ApplyOutcome> {
	return sendJson<ApplyOutcome>(
		`/${encodeURIComponent(id)}/proposals/${encodeURIComponent(proposalId)}/decision`,
		'POST',
		{ decision }
	);
}

/** Wire targets for `/promote` (the backend rejects `today`). */
export type PromoteTarget = Exclude<PromotionKind, 'today'>;

/** Result of `POST /{id}/nodes/{node_id}/promote`. `promoted: false` means an
 *  existing promotion link of that kind was returned (idempotent retry). */
export interface PromoteOutcome {
	promoted: boolean;
	object_kind: PromotionKind;
	object_id: string;
	resulting_revision?: number;
}

/** POST /{id}/nodes/{node_id}/promote — governed promotion of a node into a
 *  durable object (v3 task / review-gated memory candidate). Non-owner-asserted
 *  nodes throw `confirmation_required` (HTTP 409) unless `confirm` is true;
 *  rejected/superseded/contradicted/tombstoned nodes throw `not_promotable`. */
export async function promoteNode(
	id: string,
	nodeId: string,
	target: PromoteTarget,
	confirm = false
): Promise<PromoteOutcome> {
	return sendJson<PromoteOutcome>(
		`/${encodeURIComponent(id)}/nodes/${encodeURIComponent(nodeId)}/promote`,
		'POST',
		confirm ? { target, confirm: true } : { target }
	);
}

/** GET /{id}/events?after_seq= — event-log tail with `sequence > afterSeq`. */
export async function events(id: string, afterSeq = 0): Promise<MapEvent[]> {
	const params = new URLSearchParams({ after_seq: String(afterSeq) });
	return getJson<MapEvent[]>(`/${encodeURIComponent(id)}/events`, params);
}

/** GET /{id}/replay?at_seq= — the map as of sequence `atSeq`. */
export async function replay(id: string, atSeq: number): Promise<ThinkingMap> {
	const params = new URLSearchParams({ at_seq: String(atSeq) });
	return getJson<ThinkingMap>(`/${encodeURIComponent(id)}/replay`, params);
}

/** Inclusion controls for the Markdown export. Mirrors the backend defaults:
 *  provisional included, rejected/superseded/contradicted excluded (tombstoned
 *  content is never exported). */
export interface ExportMarkdownOptions {
	includeProvisional?: boolean;
	includeSuperseded?: boolean;
}

/** GET /{id}/export/markdown — deterministic Markdown render of the map
 *  (title header, parent_id Board tree, Connections, Open clarifications,
 *  Pending proposals). Returns the raw `text/markdown` body. */
export async function exportMarkdown(
	id: string,
	opts: ExportMarkdownOptions = {}
): Promise<string> {
	const params = new URLSearchParams();
	if (opts.includeProvisional !== undefined)
		params.set('include_provisional', String(opts.includeProvisional));
	if (opts.includeSuperseded !== undefined)
		params.set('include_superseded', String(opts.includeSuperseded));
	const response = await requireOk(
		await timedFetch(queryUrl(`/${encodeURIComponent(id)}/export/markdown`, params), {
			headers: scopedRequestHeaders({ Accept: 'text/markdown' })
		})
	);
	return response.text();
}

/** POST /{id}/restore — fork a branch at `at_sequence` into a new map (HTTP 201). */
export async function restore(
	id: string,
	body: { at_sequence: number; new_map_id: string; new_title: string }
): Promise<ThinkingMap> {
	return sendJson<ThinkingMap>(`/${encodeURIComponent(id)}/restore`, 'POST', body);
}

/** A chat-session summary for the "attach a conversation" picker. Covers every
 *  conversation surface that lands `ChatMessageReceived` turns — chat threads,
 *  the meeting bot's per-meeting threads, voice-call threads. */
export interface ChatSessionSummary {
	id: string;
	title?: string | null;
	ui_thread_id?: string | null;
	updated_at?: number | null;
}

/** GET /chat/sessions — recent conversations, newest first, for picking one to
 *  attach to a map (its user/meeting turns then auto-map via the ambient
 *  coordinator). Lives outside the thinking-maps base path, so the URL is
 *  built directly with the same scope idiom. */
export async function listChatSessions(): Promise<ChatSessionSummary[]> {
	const query = appendCurrentScopeQuery().toString();
	const response = await requireOk(
		await timedFetch(`/api/magician/v2/chat/sessions?${query}`, {
			headers: scopedRequestHeaders({ Accept: 'application/json' })
		})
	);
	const body = (await response.json()) as { sessions?: ChatSessionSummary[] };
	return (body.sessions ?? []).sort((a, b) => (b.updated_at ?? 0) - (a.updated_at ?? 0));
}

/** POST /{id}/sessions — attach a live source session for ambient auto-mapping. */
export async function attachSession(
	id: string,
	sourceSessionId: string
): Promise<{ attached: boolean }> {
	return sendJson<{ attached: boolean }>(`/${encodeURIComponent(id)}/sessions`, 'POST', {
		source_session_id: sourceSessionId
	});
}

/** DELETE /{id}/sessions/{sourceSessionId} — detach a live source session. */
export async function detachSession(
	id: string,
	sourceSessionId: string
): Promise<{ detached: boolean }> {
	return deleteJson<{ detached: boolean }>(
		`/${encodeURIComponent(id)}/sessions/${encodeURIComponent(sourceSessionId)}`
	);
}

/** Result of `POST /{id}/tutor-context` — a bounded, origin-annotated map
 *  digest was bound to the chat session's NEXT tutor run (TTL server-side). */
export interface TutorContextOutcome {
	registered: boolean;
	map_id: string;
	node_id?: string | null;
	session_id: string;
	expires_in_ms: number;
}

/** POST /{id}/tutor-context — bind this map (optionally one node's
 *  neighborhood via `nodeId`) as reference context for the chat session's next
 *  tutor run. Re-registration overwrites; the digest is background grounding
 *  only — the tutor keeps narration/storyboard ownership. */
export async function registerTutorContext(
	id: string,
	sessionId: string,
	nodeId?: string
): Promise<TutorContextOutcome> {
	return sendJson<TutorContextOutcome>(`/${encodeURIComponent(id)}/tutor-context`, 'POST', {
		session_id: sessionId,
		...(nodeId ? { node_id: nodeId } : {})
	});
}

/** GET /chat/active?ui_thread_id= — resolve the id of the user's ACTIVE chat
 *  session for a thread (the backend creates one when none exists). Used to
 *  target the tutor-context binding at the session `/chat` will show. Lives
 *  outside the thinking-maps base path, same scope idiom as
 *  `listChatSessions`. */
export async function activeChatSessionId(uiThreadId = 'general'): Promise<string> {
	const params = new URLSearchParams({ ui_thread_id: uiThreadId });
	const query = appendCurrentScopeQuery(params).toString();
	const response = await requireOk(
		await timedFetch(`/api/magician/v2/chat/active?${query}`, {
			headers: scopedRequestHeaders({ Accept: 'application/json' })
		})
	);
	const body = (await response.json()) as { session?: { id?: string }; id?: string };
	const id = body.session?.id ?? body.id;
	if (!id) throw new Error('No active chat session id in the response');
	return id;
}
