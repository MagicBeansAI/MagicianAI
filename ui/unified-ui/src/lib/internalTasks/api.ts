import { timedFetch } from '$lib/shared/fetch';
import { coordinateExecutionControl } from '$lib/magician/execution/controlClient';
import type { ExecutionPanelState } from '$lib/types/executionPanel';

const API_BASE = '/api/magician/v3';

export interface InternalTaskListItem {
	id: string;
	title: string;
	description: string;
	agent_id: string;
	status: string;
	priority?: string;
	created_at: string;
	updated_at: string;
	tags?: Array<{ id: string; name: string; color?: string | null }>;
	due_date?: string | null;
	created_by?: string | null;
	active_root_execution_id?: string | null;
	latest_root_execution_id?: string | null;
	chat_session_id?: string | null;
	lifecycle?: 'persistent' | 'internal' | string;
	synthesis_pending?: boolean;
	synthesis_failed_execution_id?: string | null;
	/**
	 * The run is holding a staged code-change proposal that is still `Pending`,
	 * so it is waiting on a human to approve or reject the diff.
	 *
	 * **Optional because the server omits the key while it is false**, not
	 * because old servers might not send it — so readers must treat absence as
	 * `false` and never as "unknown". Server-derived from the proposal store on
	 * every read, which is why it survives a restart that would have lost the
	 * HITL event, and why it is already `false` for a terminal task whose
	 * pending proposal is merely orphaned.
	 *
	 * Declared here for the same reason as the two fields below it: these rows
	 * are the same `TaskListItemV3` the normal task list sends, so the field is
	 * on the wire either way; it is named now because this surface renders it.
	 */
	awaiting_diff_approval?: boolean;
	/**
	 * The run's own written account of what it did, as markdown.
	 *
	 * Declared here for the same reason as `last_progress_at` below it: these
	 * rows are the same `TaskListItemV3` the normal task list sends, so the field
	 * has always been on the wire, and the task panel is the first thing on this
	 * surface to read it. It is the **task-level** summary — the one the last
	 * completed root execution left — which is why the panel reads it here rather
	 * than off whichever execution the Run act happens to be describing.
	 */
	completion_summary?: string | null;
	/**
	 * Output artifact names produced upon completion, mirrored from TaskListItemV3.
	 */
	completion_artifact_names?: string[];
	/**
	 * Terminal outcome classifier (e.g. 'completed', 'failed', 'cancelled').
	 */
	completion_outcome?: string | null;
	/**
	 * Optional schedule definition (cron/interval) present when the internal task is recurring.
	 */
	schedule?: Record<string, unknown> | null;
	/**
	 * RFC3339 instant at which the run last actually advanced — a step started or
	 * finished — and nothing else. Deliberately not `updated_at`, which moves on
	 * every write and would let a wedged run refresh its own liveness. The rows
	 * this endpoint returns are the same `TaskListItemV3` the normal task list
	 * sends, so the field has always been on the wire here; it is declared now
	 * because the task panel reads it for stall detection.
	 */
	last_progress_at?: string | null;
}

export interface InternalTaskPagination {
	total: number;
	limit: number;
	offset: number;
	has_more: boolean;
}

export interface PersistedInternalTaskOutput {
	id?: string;
	output_id?: string;
	artifact_path?: string;
	relative_path?: string;
	class?: string;
	role?: string;
	audience?: string;
	format?: string;
	media_type?: string;
	size_bytes?: number;
	body_snippet?: string;
	created_at?: string;
	source_execution_id?: string | null;
}

export interface InternalExecutionState {
	execution_id?: string;
	status?: string;
	started_at?: string;
	ended_at?: string | null;
	error_message?: string | null;
	completion_summary?: string | null;
	completion_outcome?: string | null;
	[key: string]: unknown;
}

export interface InternalExecutionRefs {
	output_refs?: PersistedInternalTaskOutput[];
	child_output_refs?: PersistedInternalTaskOutput[];
	outputs?: PersistedInternalTaskOutput[];
	artifacts?: unknown[];
	[key: string]: unknown;
}

export interface InternalExecutionDetails {
	state: InternalExecutionState;
	refs: InternalExecutionRefs;
	artifacts: Record<string, unknown>[];
}

export interface InternalTaskDetails {
	next_execution_cursor?: string;
	execution_total?: number;
	recurring_schedule?: {
		behavior_id: string;
		interval_seconds: number;
		next_due_at: string | null;
		latest_status: string | null;
		waiting_for_settlement: boolean;
	};
	task: Record<string, unknown>;
	executions: InternalExecutionDetails[];
}

export interface InternalTaskRefs {
	outputs?: PersistedInternalTaskOutput[];
	[key: string]: unknown;
}

export type InternalTaskSortField =
	| 'updated_at'
	| 'created_at'
	| 'title'
	| 'agent_id'
	| 'status';
export type InternalTaskSortOrder = 'asc' | 'desc';

export interface InternalTaskListQuery {
	principal: string;
	workspace: string;
	limit: number;
	offset: number;
	sort: InternalTaskSortField;
	order: InternalTaskSortOrder;
	agent?: string;
	status?: string;
	query?: string;
}

export function buildInternalTaskQuery(query: InternalTaskListQuery): URLSearchParams {
	const params = new URLSearchParams();
	params.set('limit', String(query.limit));
	params.set('offset', String(query.offset));
	params.set('sort', query.sort);
	params.set('order', query.order);
	if (query.agent?.trim()) params.set('agent_id', query.agent.trim());
	if (query.status?.trim()) params.set('status', query.status.trim());
	if (query.query?.trim()) params.set('query', query.query.trim());
	return params;
}

export async function readInternalTaskApiError(response: Response): Promise<string> {
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
	if (!response.ok) throw new Error(await readInternalTaskApiError(response));
	return response;
}

export async function listInternalTasks(query: InternalTaskListQuery): Promise<{
	tasks: InternalTaskListItem[];
	pagination: InternalTaskPagination;
}> {
	const response = await requireOk(
		await timedFetch(`${API_BASE}/tasks/internal?${buildInternalTaskQuery(query).toString()}`, {
			headers: { Accept: 'application/json' }
		})
	);
	const body = (await response.json()) as {
		tasks?: unknown;
		pagination?: Partial<InternalTaskPagination>;
	};
	return {
		tasks: Array.isArray(body.tasks) ? (body.tasks as InternalTaskListItem[]) : [],
		pagination: {
			total: Number.isFinite(body.pagination?.total) ? Number(body.pagination?.total) : 0,
			limit: Number.isFinite(body.pagination?.limit)
				? Number(body.pagination?.limit)
				: query.limit,
			offset: Number.isFinite(body.pagination?.offset)
				? Number(body.pagination?.offset)
				: query.offset,
			has_more: body.pagination?.has_more === true
		}
	};
}

export async function fetchInternalTaskDetails(
	taskId: string,
	_principal: string,
	_workspace: string,
	cursor?: string
): Promise<InternalTaskDetails> {
	const response = await requireOk(
		await timedFetch(
			`${API_BASE}/tasks/${encodeURIComponent(taskId)}/details${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ''}`,
			{ headers: { Accept: 'application/json' } }
		)
	);
	return response.json() as Promise<InternalTaskDetails>;
}

/**
 * The execution-panel state for an internal task, which is where its event log
 * lives.
 *
 * **The task route, not `/executions/{id}/execution-panel`.** That one resolves
 * only under `tasks/` and answers 404 for an internal execution; the task route
 * resolves both roots (`workspace.task_dir` probes `internal_tasks/` first), so
 * it is the only one that can serve this surface. `execution_id` selects which
 * run, so the panel follows the run picker instead of always describing the
 * latest.
 *
 * `null` rather than a throw on 404: an internal task whose panel state has not
 * been projected is an ordinary absence, and the Run act renders "nothing
 * observed" for it. A throw here would take the whole drawer down over a
 * missing event log.
 */
export async function fetchInternalTaskExecutionPanel(
	taskId: string,
	executionId: string | null
): Promise<ExecutionPanelState | null> {
	const query = executionId
		? `?execution_id=${encodeURIComponent(executionId)}`
		: '';
	const response = await timedFetch(
		`${API_BASE}/tasks/${encodeURIComponent(taskId)}/execution-panel${query}`,
		{ headers: { Accept: 'application/json' } }
	);
	if (response.status === 404) return null;
	return (await requireOk(response)).json() as Promise<ExecutionPanelState>;
}

export async function deleteInternalTask(
	taskId: string,
	_principal: string,
	_workspace: string
): Promise<void> {
	await requireOk(
		await timedFetch(
			`${API_BASE}/tasks/internal/${encodeURIComponent(taskId)}`,
			{ method: 'DELETE', headers: { Accept: 'application/json' } }
		)
	);
}

export async function cancelInternalExecution(
	executionId: string,
	_principal: string,
	_workspace: string
): Promise<void> {
	await coordinateExecutionControl(executionId, async () => {
		await requireOk(
			await timedFetch(
				`${API_BASE}/executions/${encodeURIComponent(executionId)}/cancel`,
				{ method: 'POST', headers: { Accept: 'application/json' } }
			)
		);
	});
}

export async function retryInternalTaskSynthesis(
	taskId: string,
	executionId: string,
	_principal: string,
	_workspace: string
): Promise<{ synthesis_retry_scheduled?: boolean; coalesced?: boolean } | null> {
	const response = await requireOk(
		await timedFetch(
			`${API_BASE}/tasks/${encodeURIComponent(taskId)}/executions/${encodeURIComponent(executionId)}/retry-synthesis`,
			{ method: 'POST', headers: { Accept: 'application/json' } }
		)
	);
	try {
		return (await response.json()) as {
			synthesis_retry_scheduled?: boolean;
			coalesced?: boolean;
		};
	} catch {
		return null;
	}
}

async function openInternalTaskOutput(
	taskId: string,
	relativePath: string,
	_principal: string,
	_workspace: string,
	action: 'open-file' | 'open-folder'
): Promise<void> {
	await requireOk(
		await timedFetch(
			`${API_BASE}/tasks/${encodeURIComponent(taskId)}/outputs/${action}`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
				body: JSON.stringify({ relative_path: relativePath })
			}
		)
	);
}

export async function openInternalTaskOutputFile(
	taskId: string,
	relativePath: string,
	principal: string,
	workspace: string
): Promise<void> {
	await openInternalTaskOutput(taskId, relativePath, principal, workspace, 'open-file');
}

export async function revealInternalTaskOutputFile(
	taskId: string,
	relativePath: string,
	principal: string,
	workspace: string
): Promise<void> {
	await openInternalTaskOutput(taskId, relativePath, principal, workspace, 'open-folder');
}

export function internalTaskOutputDownloadUrl(
	taskId: string,
	relativePath: string,
	_principal: string,
	_workspace: string
): string {
	return `${API_BASE}/tasks/${encodeURIComponent(taskId)}/outputs/${relativePath}`;
}
