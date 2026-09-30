/**
 * Recurring Monitors — scoped web API client (Phase 4).
 *
 * Mirrors `$lib/thinkingMaps/api.ts`: a `timedFetch`-backed, `requireOk`-
 * guarded client. Every request is authorized by the workspace-bound bearer;
 * query parameters below are endpoint filters only.
 *
 * Routes are the Phase 1-3 `/api/magician/v3/monitors` surface
 * (`magician/src/magician_v2/api/monitors_api.rs`); wire shapes live in
 * `$lib/types/monitor.ts`, pinned by the canonical fixtures in
 * `magician/tests/fixtures/monitors/`.
 *
 * Errors: non-2xx responses throw an `Error` carrying the JSON body's
 * `error` field when present (e.g. `monitor_not_found`,
 * `monitor_sources_required`, `monitor_unscheduled`), then `message`, raw
 * text, then the status line.
 */

import { timedFetch } from '$lib/shared/fetch';
import { appendCurrentScopeQuery, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import type {
	ConvertMonitorRequestV1,
	ConvertMonitorResponseV1,
	CreateMonitorRequestV1,
	MonitorDetailV1,
	MonitorFeedbackRecordV1,
	MonitorFeedbackRequestV1,
	MonitorFeedbackResponseV1,
	MonitorItemsPageV1,
	MonitorListPageV1,
	MonitorMutationResponseV1,
	MonitorRunResultV1,
	MonitorUpdateDetailV1,
	UpdateMonitorRequestV1
} from '$lib/types/monitor';

const API_BASE = '/api/magician/v3';

// ── Error handling ────────────────────────────────────────────────────────────

async function readMonitorApiError(response: Response): Promise<string> {
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
	if (!response.ok) throw new Error(await readMonitorApiError(response));
	return response;
}

// ── Request helpers ────────────────────────────────────────────────────────────

function queryUrl(path: string, params?: URLSearchParams): string {
	const query = appendCurrentScopeQuery(params).toString();
	return `${API_BASE}${path}${query ? `?${query}` : ''}`;
}

async function getJson<T>(path: string, params?: URLSearchParams): Promise<T> {
	const response = await requireOk(
		await timedFetch(queryUrl(path, params), {
			headers: scopedRequestHeaders({ Accept: 'application/json' })
		})
	);
	return response.json() as Promise<T>;
}

/** Scope-in-header body request (POST/PATCH), like the thinking-maps client. */
async function sendJson<T>(path: string, method: 'POST' | 'PATCH', body?: unknown): Promise<T> {
	const response = await requireOk(
		await timedFetch(`${API_BASE}${path}`, {
			method,
			headers: scopedRequestHeaders({
				'Content-Type': 'application/json',
				Accept: 'application/json'
			}),
			body: body === undefined ? JSON.stringify({}) : JSON.stringify(body)
		})
	);
	return response.json() as Promise<T>;
}

async function deleteJson<T>(path: string): Promise<T> {
	const response = await requireOk(
		await timedFetch(queryUrl(path), {
			method: 'DELETE',
			headers: scopedRequestHeaders({ Accept: 'application/json' })
		})
	);
	return response.json() as Promise<T>;
}

// ── Endpoints ──────────────────────────────────────────────────────────────────

export type MonitorStateFilter = 'active' | 'paused';

/**
 * GET /monitors?limit=&cursor=&state= — cursor-paginated monitor list in the
 * canonical `{items, next_cursor, limit, total, offset}` envelope (limit
 * server-clamped to 1..=200; the cursor is opaque, emitted with the `cur_`
 * prefix). `total`/`offset` are additive: an older server omits them, and the
 * pager reads that as "no page count", not "no rows".
 */
export async function listMonitors(
	limit: number,
	cursor?: string | null,
	state?: MonitorStateFilter | null
): Promise<MonitorListPageV1> {
	const params = new URLSearchParams({ limit: String(limit) });
	if (cursor?.trim()) params.set('cursor', cursor.trim());
	if (state) params.set('state', state);
	return getJson<MonitorListPageV1>('/monitors', params);
}

/** GET /monitors/{task_id} — detail (404 `monitor_not_found` for plain tasks). */
export async function getMonitor(taskId: string): Promise<MonitorDetailV1> {
	return getJson<MonitorDetailV1>(`/monitors/${encodeURIComponent(taskId)}`);
}

/** POST /monitors — validated create; activation IS this call (plan §9.1). */
export async function createMonitor(
	body: CreateMonitorRequestV1
): Promise<MonitorMutationResponseV1> {
	return sendJson<MonitorMutationResponseV1>('/monitors', 'POST', body);
}

/**
 * POST /monitors/{task_id}/convert — explicit Phase 7 conversion of an
 * EXISTING eligible task into a monitor. The task keeps its id, schedule,
 * history, executions, and outputs; the body carries only `{spec, title?}`.
 * Errors surface the stable reasons (`task_not_found`,
 * `monitor_already_exists`, `task_not_eligible_for_monitor`, `monitor_*`).
 */
export async function convertTaskToMonitor(
	taskId: string,
	body: ConvertMonitorRequestV1
): Promise<ConvertMonitorResponseV1> {
	return sendJson<ConvertMonitorResponseV1>(
		`/monitors/${encodeURIComponent(taskId)}/convert`,
		'POST',
		body
	);
}

/** PATCH /monitors/{task_id} — title/spec/schedule edit; spec edits bump the revision. */
export async function updateMonitor(
	taskId: string,
	body: UpdateMonitorRequestV1
): Promise<MonitorMutationResponseV1> {
	return sendJson<MonitorMutationResponseV1>(
		`/monitors/${encodeURIComponent(taskId)}`,
		'PATCH',
		body
	);
}

/** DELETE /monitors/{task_id} — the canonical task archive path. */
export async function deleteMonitor(
	taskId: string
): Promise<{ ok: boolean; task_id: string; files_removed: boolean }> {
	return deleteJson(`/monitors/${encodeURIComponent(taskId)}`);
}

/** POST /monitors/{task_id}/pause — flips `TaskSchedule.paused` (409 when unscheduled). */
export async function pauseMonitor(taskId: string): Promise<{ task_id: string; state: string }> {
	return sendJson(`/monitors/${encodeURIComponent(taskId)}/pause`, 'POST');
}

/** POST /monitors/{task_id}/resume */
export async function resumeMonitor(taskId: string): Promise<{ task_id: string; state: string }> {
	return sendJson(`/monitors/${encodeURIComponent(taskId)}/resume`, 'POST');
}

/** POST /monitors/{task_id}/run — run-now via the existing execution path (202). */
export async function runMonitorNow(taskId: string): Promise<unknown> {
	return sendJson<unknown>(`/monitors/${encodeURIComponent(taskId)}/run`, 'POST');
}

/** GET /monitors/{task_id}/runs?limit= — accepted run results, newest first. */
export async function getMonitorRuns(
	taskId: string,
	limit: number
): Promise<MonitorItemsPageV1<MonitorRunResultV1>> {
	return getJson(
		`/monitors/${encodeURIComponent(taskId)}/runs`,
		new URLSearchParams({ limit: String(limit) })
	);
}

/** GET /monitors/{task_id}/updates?limit= — durable update records, newest first. */
export async function getMonitorUpdates(
	taskId: string,
	limit: number
): Promise<MonitorItemsPageV1<MonitorUpdateDetailV1>> {
	return getJson(
		`/monitors/${encodeURIComponent(taskId)}/updates`,
		new URLSearchParams({ limit: String(limit) })
	);
}

/** GET /monitor-updates?limit= — scope-wide update records, newest first. */
export async function listScopeMonitorUpdates(
	limit: number
): Promise<MonitorItemsPageV1<MonitorUpdateDetailV1>> {
	return getJson('/monitor-updates', new URLSearchParams({ limit: String(limit) }));
}

/**
 * POST /monitors/{task_id}/updates/{update_id}/feedback — record a
 * useful/not-relevant verdict against one material update (plan §10).
 * `recorded:false` = idempotent replay of the same verdict; the opposite
 * verdict replaces the stored one. 404 `monitor_not_found`/`update_not_found`,
 * 400 `monitor_feedback_verdict_invalid`.
 */
export async function submitMonitorUpdateFeedback(
	taskId: string,
	updateId: string,
	body: MonitorFeedbackRequestV1
): Promise<MonitorFeedbackResponseV1> {
	return sendJson<MonitorFeedbackResponseV1>(
		`/monitors/${encodeURIComponent(taskId)}/updates/${encodeURIComponent(updateId)}/feedback`,
		'POST',
		body
	);
}

/** GET /monitors/{task_id}/feedback?limit= — stored feedback records. */
export async function getMonitorFeedback(
	taskId: string,
	limit: number
): Promise<MonitorItemsPageV1<MonitorFeedbackRecordV1>> {
	return getJson(
		`/monitors/${encodeURIComponent(taskId)}/feedback`,
		new URLSearchParams({ limit: String(limit) })
	);
}
