import type { FeedAttentionResponse, FeedItem } from '$lib/feed/types';
import type { ExecutionPanelAttentionItem } from '$lib/types/executionPanel';
import type { MockFetchCall, MockFetchRoute } from './browser';
import { jsonResponse, MockWebSocket } from './browser';

export interface TaskBackendRecord extends Record<string, unknown> {
	id: string;
	title: string;
	description: string;
	status: string;
	agent_id: string;
	ui_thread_id: string;
	created_at: string;
	updated_at: string;
	active_root_execution_id: string | null;
	latest_root_execution_id: string | null;
	last_completed_root_execution_id: string | null;
}

export interface HitlReply {
	status?: number;
	body?: Record<string, unknown>;
}

export interface TaskAttentionBackend {
	tasks: TaskBackendRecord[];
	attentionItems: FeedItem[];
	/** Optional first/cursor-page fixture; exact item reads still search attentionItems. */
	attentionListItems: FeedItem[] | null;
	/**
	 * The humanized event log `/execution-panel` reports for a task, by task id.
	 *
	 * Empty by default and opt-in per test, because a run's events are the one
	 * thing on that payload whose presence changes what the Run act *says* — a
	 * task with events reads `… · N events` where the same task without them says
	 * nothing about events at all. Handing every fixture a log would move act
	 * summaries in suites that are about something else entirely.
	 */
	activityByTask: Record<string, FeedItem[]>;
	/**
	 * The rows `/execution-panel` reports as needing a human, by task id.
	 *
	 * Beside `activityByTask` and for the same reason it is here rather than in a
	 * per-suite route: both come out of **one** response, and a hand-rolled route
	 * that answers only the branch a suite reads is a fixture asserting a shape
	 * the backend cannot produce. One such route already existed — it returned
	 * `{ run: { needs_attention: [...] } }` with no `overview` at all, and passed
	 * only because the client validated the one field it went on to read.
	 */
	attentionByTask: Record<string, ExecutionPanelAttentionItem[]>;
	hitlReplies: HitlReply[];
	hitlBodies: Record<string, unknown>[];
	cancelledExecutions: string[];
	onHitlResponse?: (body: Record<string, unknown>) => void;
	setTaskStatus(taskId: string, status: string, patch?: Partial<TaskBackendRecord>): void;
	routes(): MockFetchRoute[];
}

const DEFAULT_TIME = '2026-07-11T10:00:00.000Z';

/**
 * The six lanes `GET /v3/tasks?view=` serves, written out here rather than
 * imported from the store.
 *
 * A fixture that reused the client's own predicates would agree with it by
 * construction and could never catch the client reading a lane the server did
 * not apply — which is the entire failure this endpoint exists to remove. These
 * mirror `magician/src/magician_v2/api/task_lanes.rs`.
 */
export const TASK_LANE_NAMES = ['all', 'inbox', 'today', 'overdue', 'running', 'completed'];
export const DATE_LANES = ['today', 'overdue'];

export function matchesLane(lane: string, task: TaskBackendRecord, today: string): boolean {
	const status = String(task.status ?? '');
	const dueDate = typeof task.due_date === 'string' ? task.due_date : '';
	const tags = Array.isArray(task.tags) ? task.tags : [];
	switch (lane) {
		case 'all':
			return status !== 'completed';
		case 'inbox':
			return tags.length === 0 && status === 'pending';
		case 'today':
			return dueDate.startsWith(today);
		case 'overdue':
			return dueDate.length > 0 && dueDate < today && status !== 'completed';
		case 'running':
			return status === 'running' || status === 'paused';
		case 'completed':
			return status === 'completed';
		default:
			return true;
	}
}

/** Every lane's total, zeros included — an omitted key is a different claim. */
export function laneCounts(tasks: TaskBackendRecord[], today: string): Record<string, number> {
	const counts: Record<string, number> = {};
	for (const lane of TASK_LANE_NAMES) {
		counts[lane] = tasks.filter((task) => matchesLane(lane, task, today)).length;
	}
	return counts;
}

export function backendTask(
	id: string,
	status: string,
	overrides: Partial<TaskBackendRecord> = {}
): TaskBackendRecord {
	const historicalExecutionId = ['running', 'waiting_for_user', 'paused', 'failed'].includes(status)
		? `execution-${id}`
		: null;
	const activeExecutionId = ['running', 'waiting_for_user', 'paused'].includes(status)
		? historicalExecutionId
		: null;
	return {
		id,
		title: `Task ${id}`,
		description: `Complete ${id}`,
		status,
		priority: 'p2',
		due_date: null,
		tags: [],
		agent_id: 'personal-assistant',
		ui_thread_id: 'general',
		created_by: 'user',
		lifecycle: 'persistent',
		output_mode: 'accumulate',
		approved: true,
		is_blocked: false,
		has_plan: false,
		plan_status: null,
		pending_questions: status === 'waiting_for_user'
			? [{ question_id: `pause-${id}`, question_text: 'Which environment should I use?' }]
			: [],
		active_root_execution_id: activeExecutionId,
		latest_root_execution_id: historicalExecutionId,
		last_completed_root_execution_id: null,
		created_at: DEFAULT_TIME,
		updated_at: DEFAULT_TIME,
		...overrides
	};
}

export function clarificationAttentionItem(
	task: TaskBackendRecord,
	overrides: Partial<FeedItem> = {}
): FeedItem {
	const pauseId = `pause-${task.id}`;
	return {
		id: `v3:attention:${pauseId}`,
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'escalation',
		title: 'Which environment should I use?',
		summary: `Task ${task.title} is waiting for input.`,
		status: 'needs_action',
		created_at: Date.parse(DEFAULT_TIME),
		updated_at: Date.parse(DEFAULT_TIME),
		actions: [],
		metadata: {
			attention_kind: 'input.requested',
			source: 'clarification',
			input_type: 'text',
			input_schema: { type: 'text', placeholder: 'Environment or account' },
			questions: ['Which environment should I use?'],
			pause_state_id: pauseId,
			correlation_id: pauseId,
			task_id: task.id,
			execution_id: task.active_root_execution_id
		},
		...overrides
	};
}

function requestUrl(call: MockFetchCall): URL {
	return new URL(call.url, 'http://localhost');
}

function taskIdFromPath(pathname: string): string | null {
	const match = pathname.match(/^\/api\/magician\/v3\/tasks\/([^/]+)/);
	return match ? decodeURIComponent(match[1]) : null;
}

/**
 * Whether one attention row answers to an item id.
 *
 * The feed's own `id` is one of several names a row has: callers reach an item
 * by its correlation id, its request id or the pause state it belongs to, none
 * of which the feed id has to equal — `v3:attention:pause-1` and
 * `runtime:hitl:bot_auth:…` are both prefixed spellings of a correlation id.
 * The exact endpoint resolves any of them, so this fixture does too.
 */
function attentionItemMatchesAlias(item: FeedItem, itemId: string): boolean {
	if (!itemId) return false;
	if (item.id === itemId) return true;
	const metadata = (item.metadata ?? {}) as Record<string, unknown>;
	return ['correlation_id', 'request_id', 'pause_state_id'].some(
		(key) => typeof metadata[key] === 'string' && metadata[key] === itemId
	);
}

function attentionPayload(items: FeedItem[]): FeedAttentionResponse {
	const limit = 25;
	const emptyPage = {
		total: 0,
		limit,
		cursor: null,
		next_cursor: null,
		has_more: false
	};
	return {
		counts: {
			requests: items.length,
			approvals: 0,
			escalations: 0,
			needs_action: items.length,
			failed: 0,
			running: 0
		},
		totals: {
			requests: items.length,
			approvals: 0,
			escalations: 0,
			failed: 0,
			running: 0
		},
		pages: {
			requests: { ...emptyPage, total: items.length },
			approvals: { ...emptyPage },
			escalations: { ...emptyPage },
			failed: { ...emptyPage },
			running: { ...emptyPage }
		},
		requests: items,
		approvals: [],
		escalations: [],
		failed: [],
		running: []
	};
}

/**
 * One row of a run's activity log, as the panel endpoint humanizes it.
 *
 * `item_type` is `'task'` because **`FeedItemType` has no `execution` member** —
 * a fixture asserting a shape the backend cannot produce passes vitest, which
 * transpiles without type-checking. That defect has already got through once in
 * this directory.
 */
export function activityItem(
	taskId: string,
	overrides: Partial<FeedItem> & { id: string; created_at: number }
): FeedItem {
	return {
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'task',
		task_id: taskId,
		title: 'Something happened',
		summary: null,
		status: 'done',
		updated_at: overrides.created_at,
		actions: [],
		metadata: {},
		...overrides
	};
}

/**
 * The `/execution-panel` payload for a task.
 *
 * Exported so a suite that needs to control the *timing* of the response still
 * gets its *shape* from here. A test that hand-rolled the body to hold it open
 * would be the third copy of this contract, and the one already deleted from
 * `TasksWorkspace.component.test.ts` had drifted to a shape the endpoint cannot
 * produce.
 */
export function taskPanelState(
	task: TaskBackendRecord,
	parts: { activity?: FeedItem[]; needsAttention?: ExecutionPanelAttentionItem[] } = {}
): Record<string, unknown> {
	return panelState(task, parts.activity ?? [], parts.needsAttention ?? []);
}

function panelState(
	task: TaskBackendRecord,
	activity: FeedItem[],
	needsAttention: ExecutionPanelAttentionItem[]
): Record<string, unknown> {
	const executionId = task.active_root_execution_id ?? task.latest_root_execution_id;
	return {
		default_tab: task.status === 'running' || task.status === 'waiting_for_user' ? 'run' : 'plan',
		overview: {
			task_id: task.id,
			execution_id: executionId,
			principal: 'anonymous',
			workspace: 'default',
			ui_thread_id: task.ui_thread_id,
			title: task.title,
			description: task.description,
			status: task.status,
			priority: task.priority ?? null,
			assigned_agent_id: task.agent_id,
			active_agent_id: task.agent_id,
			has_plan: false,
			progress: task.status === 'running' ? 10 : null,
			current_step: task.status === 'running' ? 'Starting' : null,
			created_at: Date.parse(task.created_at),
			updated_at: Date.parse(task.updated_at)
		},
		run: {
			summary: null,
			responsibility: null,
			pending_questions: task.pending_questions ?? [],
			needs_attention: needsAttention,
			recent_activity: [],
			activity_log: activity
		},
		output: { result: null, deliveries: [], recent_runs: [] },
		debug: {
			selected_execution: null,
			taskplan: null,
			timeline: [],
			observations: [],
			shell_entries: [],
			latest_error_message: null,
			history_count: 0,
			tags: []
		}
	};
}

export function createTaskAttentionBackend(
	tasks: TaskBackendRecord[] = []
): TaskAttentionBackend {
	const backend: TaskAttentionBackend = {
		tasks,
		attentionItems: [],
		attentionListItems: null,
		activityByTask: {},
		attentionByTask: {},
		hitlReplies: [],
		hitlBodies: [],
		cancelledExecutions: [],
		setTaskStatus(taskId, status, patch = {}) {
			const task = backend.tasks.find((candidate) => candidate.id === taskId);
			if (!task) throw new Error(`Unknown test task: ${taskId}`);
			const active = ['running', 'waiting_for_user', 'paused'].includes(status);
			Object.assign(task, {
				status,
				active_root_execution_id: active
					? task.active_root_execution_id ?? `execution-${taskId}`
					: null,
				latest_root_execution_id:
					task.latest_root_execution_id ?? task.active_root_execution_id ?? `execution-${taskId}`,
				pending_questions: status === 'waiting_for_user'
					? [{ question_id: `pause-${taskId}`, question_text: 'Which environment should I use?' }]
					: [],
				updated_at: new Date().toISOString(),
				...patch
			});
		},
		routes() {
			return [
				{
					method: 'GET',
					match: (call) => requestUrl(call).pathname === '/api/magician/v3/tasks',
					handle: (call) => {
						const params = requestUrl(call).searchParams;
						// The real endpoint answers the pre-pagination `{tasks}`
						// shape when neither `limit` nor `offset` is given, and
						// only the paged branch knows about `view=`/`counts`.
						// A fixture that ignored that would let a client read a
						// lane the server never applied.
						if (!params.has('limit') && !params.has('offset')) {
							return jsonResponse({ tasks: backend.tasks });
						}
						const today = params.get('today');
						const view = params.get('view');
						if (view && !TASK_LANE_NAMES.includes(view)) {
							return jsonResponse({ error: 'unknown_task_view' }, { status: 400 });
						}
						if (view && DATE_LANES.includes(view) && !today) {
							return jsonResponse({ error: 'task_view_requires_today' }, { status: 400 });
						}
						// Counted over the whole pool BEFORE the lane filter —
						// counting after would make every badge report the lane
						// already on screen.
						const counts = today ? laneCounts(backend.tasks, today) : null;
						const pool = view
							? backend.tasks.filter((task) => matchesLane(view, task, today ?? ''))
							: backend.tasks;
						const limit = Number(params.get('limit') ?? pool.length) || pool.length;
						const offset = Number(params.get('offset') ?? 0) || 0;
						const page = pool.slice(offset, offset + limit);
						return jsonResponse({
							tasks: page,
							pagination: {
								total: pool.length,
								limit,
								offset,
								has_more: offset + page.length < pool.length
							},
							...(counts ? { counts } : {})
						});
					}
				},
				{
					method: 'GET',
					match: (call) => requestUrl(call).pathname === '/api/magician/v3/tasks/internal',
					handle: () => jsonResponse({ tasks: [], pagination: { total: 0 } })
				},
				{
					method: 'POST',
					match: (call) => requestUrl(call).pathname.endsWith('/execute'),
					handle: (call) => {
						const taskId = taskIdFromPath(requestUrl(call).pathname);
						if (!taskId) return jsonResponse({ error: 'task not found' }, { status: 404 });
						backend.setTaskStatus(taskId, 'running');
						return jsonResponse({
							execution: { state: { execution_id: `execution-${taskId}` } }
						});
					}
				},
				{
					method: 'PUT',
					match: (call) => requestUrl(call).pathname.endsWith('/status'),
					handle: (call) => {
						const taskId = taskIdFromPath(requestUrl(call).pathname);
						const body = JSON.parse(String(call.init?.body ?? '{}')) as { status?: string };
						if (!taskId || !body.status) {
							return jsonResponse({ error: 'invalid status request' }, { status: 400 });
						}
						backend.setTaskStatus(taskId, body.status);
						return jsonResponse({ task: { state: { status: body.status } } });
					}
				},
				{
					method: 'POST',
					match: (call) => requestUrl(call).pathname.endsWith('/cancel'),
					handle: (call) => {
						const match = requestUrl(call).pathname.match(/\/executions\/([^/]+)\/cancel$/);
						if (match) backend.cancelledExecutions.push(decodeURIComponent(match[1]));
						return jsonResponse({ cancelled: true });
					}
				},
				{
					method: 'GET',
					match: (call) => requestUrl(call).pathname.endsWith('/control-state'),
					handle: (call) => {
						const match = requestUrl(call).pathname.match(/\/executions\/([^/]+)\/control-state$/);
						const executionId = match ? decodeURIComponent(match[1]) : '';
						const task = backend.tasks.find(
							(candidate) => candidate.active_root_execution_id === executionId
						);
						if (!task) return jsonResponse({ error: 'execution not found' }, { status: 404 });
						const active = task.status === 'running' || task.status === 'planning';
						const paused = task.status === 'paused';
						const terminal = ['completed', 'failed', 'cancelled'].includes(task.status);
						return jsonResponse({
							execution_id: executionId,
							waiting_state: paused ? 'paused' : active ? 'executing' : 'waiting_user',
							paused_from_state: paused ? 'executing' : null,
							pause_kind: paused ? 'manual' : null,
							active,
							can_pause: active,
							can_resume: paused,
							can_steer: active,
							can_cancel: !terminal
						});
					}
				},
				{
					method: 'DELETE',
					match: (call) => /^\/api\/magician\/v3\/tasks\/[^/]+$/.test(requestUrl(call).pathname),
					handle: (call) => {
						const taskId = taskIdFromPath(requestUrl(call).pathname);
						backend.tasks = backend.tasks.filter((task) => task.id !== taskId);
						return jsonResponse({ deleted: true });
					}
				},
				{
					method: 'GET',
					match: (call) => requestUrl(call).pathname.endsWith('/plan'),
					handle: (call) => {
						const taskId = taskIdFromPath(requestUrl(call).pathname) ?? '';
						return jsonResponse({ error: `task_plan_not_found:${taskId}` }, { status: 404 });
					}
				},
				{
					method: 'GET',
					match: (call) => requestUrl(call).pathname.endsWith('/execution-panel'),
					handle: (call) => {
						const taskId = taskIdFromPath(requestUrl(call).pathname);
						const task = backend.tasks.find((candidate) => candidate.id === taskId);
						return task
							? jsonResponse(
									panelState(
										task,
										backend.activityByTask[task.id] ?? [],
										backend.attentionByTask[task.id] ?? []
									)
								)
							: jsonResponse({ error: 'task not found' }, { status: 404 });
					}
				},
				{
					method: 'GET',
					match: (call) => requestUrl(call).pathname.endsWith('/outputs'),
					handle: () => jsonResponse({ outputs: { outputs: [] } })
				},
				{
					method: 'GET',
					match: (call) => /^\/api\/magician\/v3\/tasks\/[^/]+$/.test(requestUrl(call).pathname),
					handle: (call) => {
						const taskId = taskIdFromPath(requestUrl(call).pathname);
						const task = backend.tasks.find((candidate) => candidate.id === taskId);
						return task
							? jsonResponse({ task })
							: jsonResponse({ error: 'task not found' }, { status: 404 });
					}
				},
				{
					method: 'GET',
					match: (call) =>
						requestUrl(call).pathname.startsWith('/api/magician/v2/feed/attention/'),
					handle: (call) => {
						const url = requestUrl(call);
						const encodedId = url.pathname.slice('/api/magician/v2/feed/attention/'.length);
						const itemId = decodeURIComponent(encodedId);
						const item = backend.attentionItems.find(
							(candidate) => attentionItemMatchesAlias(candidate, itemId)
						);
						return item
							? jsonResponse(item)
							: jsonResponse(
									{ error: 'attention_item_not_found', item_id: itemId },
									{ status: 404 }
								);
					}
				},
				{
					method: 'GET',
					match: '/feed/attention',
					// `attentionListItems` is the *page* the feed serves, which a test can
					// empty to prove an item was reached by the exact endpoint rather than
					// found in the list. `null` means "the page is everything there is".
					handle: () =>
						jsonResponse(attentionPayload(backend.attentionListItems ?? backend.attentionItems))
				},
				{
					method: 'GET',
					match: '/channel-assist/follow-ups',
					handle: () =>
						jsonResponse({
							items: [],
							total: 0,
							limit: 6,
							cursor: null,
							next_cursor: null,
							has_more: false
						})
				},
				{
					method: 'POST',
					match: /\/hitl\/[^/]+\/respond$/,
					handle: (call) => {
						const body = JSON.parse(String(call.init?.body ?? '{}')) as Record<string, unknown>;
						backend.hitlBodies.push(body);
						backend.onHitlResponse?.(body);
						const reply = backend.hitlReplies.shift() ?? { body: { resumed: true } };
						return jsonResponse(reply.body ?? {}, { status: reply.status ?? 200 });
					}
				},
				{
					match: () => true,
					handle: () => jsonResponse({ threads: [], active: [], sessions: [] })
				}
			];
		}
	};
	return backend;
}

export function emitV2Event(eventType: string, data: Record<string, unknown>): void {
	const socket = MockWebSocket.instances.at(-1);
	if (!socket) throw new Error('No V2 websocket is active in this test');
	socket.receive(JSON.stringify({ event_type: eventType, data }));
}

export function emitTaskUpdated(taskId: string): void {
	emitV2Event('TaskUpdated', {
		principal: 'anonymous',
		workspace: 'default',
		task_id: taskId
	});
}
