import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import type { Task } from './taskStore';
import {
	backendTask,
	laneCounts,
	matchesLane,
	type TaskBackendRecord
} from '../../test/taskAttentionBackend';
import {
	applyPendingCompletionOverlay,
	applyTaskPlanPayload,
	applyTaskPlanSteps,
	clearTaskPlanState,
	computeFilteredTasks,
	deriveTaskStatusFromPlan,
	deriveTaskStatusWithoutPlan,
	extractPlanStepsFromExecutionPanel,
	extractPlanStepsFromPlanGraph,
	extractSchedule,
	firstPendingPlanQuestion,
	flattenV3TaskRecord,
	isTaskPlanVersionConflict,
	normalizeCreatedBy,
	normalizeExecutionPlanStepStatus,
	normalizeTaskPlanStatus,
	normalizeV3TaskStatus,
	parseLastProgressAt,
	parseTaskPlanExecutionGateError,
	parseTimestampToIso,
	pendingPlanQuestions,
	readerLocalDate,
	readTaskApiError,
	readTaskLaneCounts,
	resolveTaskActiveExecutionId,
	resolveTaskExecutionId,
	serializeCreatedByForApi,
	serializeScheduleForApi,
	taskApiErrorMessage,
	taskCounts,
	taskHasExecutablePlan,
	taskResetStatus,
	taskStore
} from './taskStore';

function task(overrides: Partial<Task> = {}): Task {
	return {
		id: 'task-1',
		title: 'Prepare report',
		description: 'Prepare the weekly report',
		status: 'pending',
		tags: [],
		source: 'task',
		approved: true,
		isBlocked: false,
		createdAt: '2026-07-10T00:00:00.000Z',
		updatedAt: '2026-07-10T00:00:00.000Z',
		...overrides
	};
}

describe('task API schedule and provenance normalization', () => {
	it('serializes cron schedules with backend policies and optional controls', () => {
		expect(serializeScheduleForApi({
			cron: '0 9 * * 1-5',
			timezone: 'Asia/Kolkata',
			execution_history_retention: { max_records: 4 },
			max_runs: 0,
			paused: false
		})).toEqual({
			kind: { Cron: { expression: '0 9 * * 1-5', timezone: 'Asia/Kolkata' } },
			timezone: 'Asia/Kolkata',
			missed_fire_policy: 'skip',
			concurrent_execution_policy: 'skip',
			execution_history_retention: { max_records: 4 },
			max_runs: 0,
			paused: false
		});
	});

	it('serializes missing timezone as null', () => {
		expect(serializeScheduleForApi({ cron: '@daily' })).toMatchObject({
			kind: { Cron: { expression: '@daily', timezone: null } },
			timezone: null
		});
	});

	it.each([
		['user', { type: 'user' }],
		['system', { type: 'system' }],
		['autonomous', { type: 'agent', agent_id: 'system' }],
		['delegation', { type: 'delegation', parent_task_id: '', delegating_agent_id: '' }]
	] as const)('serializes %s provenance', (input, expected) => {
		expect(serializeCreatedByForApi(input)).toEqual(expected);
	});

	it('extracts frontend schedule shape without losing explicit zero and false', () => {
		expect(extractSchedule({
			cron: '*/5 * * * *',
			timezone: 'UTC',
			max_runs: 0,
			paused: false
		})).toEqual({
			cron: '*/5 * * * *',
			timezone: 'UTC',
			execution_history_retention: undefined,
			max_runs: 0,
			paused: false
		});
	});

	it('extracts backend Cron shape and falls back to variant timezone', () => {
		expect(extractSchedule({
			kind: { Cron: { expression: '0 0 * * *', timezone: 'Europe/London' } }
		})).toEqual({
			cron: '0 0 * * *',
			timezone: 'Europe/London',
			execution_history_retention: undefined,
			max_runs: undefined,
			paused: undefined
		});
	});

	it('rejects malformed and unsupported schedules', () => {
		const warning = vi.spyOn(console, 'warn').mockImplementation(() => {});
		expect(extractSchedule(null)).toBeUndefined();
		expect(extractSchedule({ kind: { Interval: { seconds: 10 } } })).toBeUndefined();
		expect(warning).toHaveBeenCalledWith(
			'extractSchedule: unsupported schedule kind "Interval", ignoring'
		);
	});

	it.each([
		['agent', 'autonomous'],
		['autonomous', 'autonomous'],
		[' delegation ', 'delegation'],
		['system', 'system'],
		['unknown', 'user'],
		[null, 'user']
	] as const)('normalizes created_by %j to %s', (input, expected) => {
		expect(normalizeCreatedBy(input)).toBe(expected);
	});
});

describe('task plan version conflict reconciliation', () => {
	afterEach(() => {
		taskStore.reset();
		vi.unstubAllGlobals();
	});

	it.each(['approvePlan', 'rejectPlan'] as const)(
		'refreshes authoritative task and plan state before %s surfaces the conflict',
		async (action) => {
			let taskListReads = 0;
			const calls: string[] = [];
			vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
				const url = typeof input === 'string' ? input : input.toString();
				calls.push(`${(init?.method ?? 'GET').toUpperCase()} ${url}`);
				if (url.includes('/tasks/internal')) {
					return new Response(JSON.stringify({ tasks: [] }), { status: 200 });
				}
				if (url === '/api/magician/v3/tasks') {
					taskListReads += 1;
					const current = taskListReads > 1;
					return new Response(JSON.stringify({
						tasks: [{
							id: 'task-1',
							title: 'Prepare report',
							description: 'Prepare the weekly report',
							status: 'ready',
							tags: [],
							has_plan: true,
							plan_status: current ? 'approved' : 'draft',
							latest_plan_id: current ? 'plan-current' : 'plan-stale',
							created_at: '2026-07-10T00:00:00Z',
							updated_at: '2026-07-11T00:00:00Z'
						}]
					}), { status: 200 });
				}
				if (url.endsWith('/tasks/task-1/plan')) {
					return new Response(JSON.stringify({
						plan: { plan_id: 'plan-current', status: 'approved' }
					}), { status: 200 });
				}
				if (url.includes(`/tasks/task-1/plan/${action === 'approvePlan' ? 'approve' : 'reject'}`)) {
					return new Response(JSON.stringify({
						error: 'task_plan_version_mismatch:task-1:expected=plan-stale:current=plan-current'
					}), { status: 400 });
				}
				throw new Error(`Unhandled request: ${url}`);
			}));

			await taskStore.loadTasks();
			await expect(taskStore[action]('task-1', 'plan-stale')).rejects.toThrow(
				'task_plan_version_mismatch:'
			);

			const postIndex = calls.findIndex((call) => call.startsWith('POST '));
			const reloadIndex = calls.findIndex((call, index) => index > postIndex && call === 'GET /api/magician/v3/tasks');
			const planIndex = calls.findIndex((call, index) => index > reloadIndex && call.endsWith('/tasks/task-1/plan'));
			expect(postIndex).toBeGreaterThanOrEqual(0);
			expect(reloadIndex).toBeGreaterThan(postIndex);
			expect(planIndex).toBeGreaterThan(reloadIndex);
			expect(get(taskStore).tasks[0]).toMatchObject({
				latestPlanId: 'plan-current',
				planStatus: 'approved'
			});
		}
	);

	it('recognizes both mismatch and already-resolved conflict envelopes', () => {
		expect(isTaskPlanVersionConflict({ error: 'task_plan_version_mismatch:task-1' })).toBe(true);
		expect(isTaskPlanVersionConflict({ error: 'task_plan_already_resolved:task-1' })).toBe(true);
		expect(isTaskPlanVersionConflict({ error: 'unrelated' })).toBe(false);
	});
});

describe('task record and error normalization', () => {
	it('prefers active, then latest, then completed execution ids', () => {
		expect(resolveTaskExecutionId({
			active_root_execution_id: 'active',
			latest_root_execution_id: 'latest',
			last_completed_root_execution_id: 'completed'
		})).toBe('active');
		expect(resolveTaskExecutionId({ latest_root_execution_id: 'latest' })).toBe('latest');
		expect(resolveTaskExecutionId({ last_completed_root_execution_id: 'completed' })).toBe('completed');
		expect(resolveTaskExecutionId({ latest_root_execution_id: '' })).toBeUndefined();
	});

	it('never treats a historical execution id as an active control target', () => {
		expect(resolveTaskActiveExecutionId({
			active_root_execution_id: ' active ',
			latest_root_execution_id: 'latest',
			last_completed_root_execution_id: 'completed'
		})).toBe('active');
		expect(resolveTaskActiveExecutionId({ latest_root_execution_id: 'latest' })).toBeUndefined();
		expect(resolveTaskActiveExecutionId({ last_completed_root_execution_id: 'completed' })).toBeUndefined();
		expect(resolveTaskActiveExecutionId({ active_root_execution_id: '   ' })).toBeUndefined();
	});

	it('flattens the V3 manifest/state envelope without inventing fields', () => {
		const flattened = flattenV3TaskRecord({
			manifest: {
				task_id: 'task-9',
				title: 'Ship it',
				agent_id: 'forge',
				chat_session_id: 'session-9',
				lifecycle: 'internal',
				sync_mode: 'deferred',
				tags: [{ name: 'release', color: '#fff' }],
				created_at: 'created'
			},
			state: {
				status: 'running',
				active_root_execution_id: 'exec-1',
				synthesis_pending: true,
				updated_at: 'updated'
			},
			current_step_title: 'Compile'
		});
		expect(flattened).toMatchObject({
			id: 'task-9',
			title: 'Ship it',
			agent_id: 'forge',
			chat_session_id: 'session-9',
			lifecycle: 'internal',
			sync_mode: 'deferred',
			status: 'running',
			active_root_execution_id: 'exec-1',
			synthesis_pending: true,
			currentStepTitle: 'Compile',
			created_at: 'created',
			updated_at: 'updated'
		});
	});

	it.each([
		[{ error: ' direct failure ' }, 'direct failure'],
		[{ message: ' message failure ' }, 'message failure'],
		[{ details: { reason: ' nested failure ' } }, 'nested failure'],
		[{ error: '   ', details: { reason: 'fallback' } }, 'fallback'],
		[null, null],
		['failure', null]
	])('extracts task API error from %j', (payload, expected) => {
		expect(taskApiErrorMessage(payload)).toBe(expected);
	});

	it('includes response status and parsed backend reason in API errors', async () => {
		const error = await readTaskApiError(
			new Response(JSON.stringify({ details: { reason: 'plan still running' } }), {
				status: 409,
				headers: { 'content-type': 'application/json' }
			}),
			'Unable to execute task'
		);
		expect(error.message).toBe('Unable to execute task (409): plan still running');
	});

	it('falls back cleanly when an error response is not JSON', async () => {
		const error = await readTaskApiError(new Response('nope', { status: 500 }), 'Load failed');
		expect(error.message).toBe('Load failed (500)');
	});
});

describe('task progress instant', () => {
	const NOW = '2026-07-29T12:00:00.000Z';
	const PROGRESSED_AT = '2026-07-29T11:00:00Z';
	const UPDATED_AT = '2026-07-29T11:59:00Z';

	beforeEach(() => taskStore.reset());

	afterEach(() => {
		vi.useRealTimers();
		taskStore.reset();
		vi.unstubAllGlobals();
	});

	function installTaskFeed(record: Record<string, unknown>) {
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
			const url = typeof input === 'string' ? input : input.toString();
			if (url.includes('/tasks/internal')) {
				return new Response(JSON.stringify({ tasks: [] }), { status: 200 });
			}
			if (url === '/api/magician/v3/tasks') {
				return new Response(JSON.stringify({ tasks: [record] }), { status: 200 });
			}
			throw new Error(`Unhandled request: ${url}`);
		}));
	}

	// The two instants are deliberately an hour apart. A conversion reading
	// `updated_at` under the progress field's name would pass a fixture where
	// they matched, and the whole point of the field is that they do not.
	it('carries the progress instant the server sent, never updated_at', async () => {
		installTaskFeed(backendTask('wedged', 'running', {
			last_progress_at: PROGRESSED_AT,
			updated_at: UPDATED_AT
		}));

		await taskStore.loadTasks();

		const task = get(taskStore).tasks[0];
		expect(task.lastProgressAt).toBe(Date.parse(PROGRESSED_AT));
		expect(task.lastProgressAt).not.toBe(Date.parse(UPDATED_AT));
		expect(task.updatedAt).toBe(new Date(UPDATED_AT).toISOString());
	});

	// `parseTimestampToIso` — used for every other timestamp on the record —
	// returns *now* for anything it cannot read. Routing this field through it
	// would make a run with no recorded progress look as though it had just
	// advanced, so a wedged run could never be reported as stalled.
	it('leaves the progress instant absent rather than defaulting it to now', async () => {
		// `backendTask` sends no `last_progress_at`, which is exactly the wire
		// shape of a run that has not recorded a step transition.
		installTaskFeed(backendTask('never-ran', 'running', { updated_at: UPDATED_AT }));

		await taskStore.loadTasks();

		const task = get(taskStore).tasks[0];
		expect(task.lastProgressAt).toBeUndefined();
	});

	it('reduces missing, malformed and sentinel progress instants to absent', () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date(NOW));
		expect(parseLastProgressAt(PROGRESSED_AT)).toBe(Date.parse(PROGRESSED_AT));
		expect(parseLastProgressAt(undefined)).toBeUndefined();
		expect(parseLastProgressAt(null)).toBeUndefined();
		expect(parseLastProgressAt('')).toBeUndefined();
		expect(parseLastProgressAt('   ')).toBeUndefined();
		expect(parseLastProgressAt('not-a-date')).toBeUndefined();
		// The classic "never" sentinels, none of which is a timestamp.
		expect(parseLastProgressAt(0)).toBeUndefined();
		expect(parseLastProgressAt(-1)).toBeUndefined();
		expect(parseLastProgressAt(Number.NaN)).toBeUndefined();
		expect(parseLastProgressAt(Number.POSITIVE_INFINITY)).toBeUndefined();
		// Not `now` either — every unreadable input has to arrive at the same
		// answer the wire's own absence produces.
		expect(parseLastProgressAt('not-a-date')).not.toBe(Date.parse(NOW));
	});
});

/**
 * `awaiting_diff_approval` is **omitted from the wire whenever it is false**,
 * so the shape the client sees on an ordinary row is a missing key, not
 * `false`. These cases exist because the two spellings are easy to conflate in
 * a conversion and impossible to conflate in a fixture: a cast or a `??` would
 * pass the true case and hand every other row `undefined`, which is truthy
 * enough for nothing and distinguishable from `false` by anything that checks.
 */
describe('pending diff approval on the list row', () => {
	beforeEach(() => taskStore.reset());

	afterEach(() => {
		taskStore.reset();
		vi.unstubAllGlobals();
	});

	function installTaskFeed(records: Record<string, unknown>[]) {
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
			const url = typeof input === 'string' ? input : input.toString();
			if (url.includes('/tasks/internal')) {
				return new Response(JSON.stringify({ tasks: [] }), { status: 200 });
			}
			if (url === '/api/magician/v3/tasks') {
				return new Response(JSON.stringify({ tasks: records }), { status: 200 });
			}
			throw new Error(`Unhandled request: ${url}`);
		}));
	}

	it('carries an affirmative flag through to the row', async () => {
		installTaskFeed([backendTask('staged', 'paused', { awaiting_diff_approval: true })]);

		await taskStore.loadTasks();

		expect(get(taskStore).tasks[0].awaitingDiffApproval).toBe(true);
	});

	// `backendTask` sends no `awaiting_diff_approval`, which is exactly the wire
	// shape of every task that is not holding a staged diff — including, per the
	// server's own rule, a terminal task whose `Pending` proposal is orphaned.
	it('reads an absent key as false rather than undefined', async () => {
		installTaskFeed([
			backendTask('running-fine', 'running'),
			backendTask('done', 'completed')
		]);

		await taskStore.loadTasks();

		const [running, completed] = get(taskStore).tasks;
		expect(running.awaitingDiffApproval).toBe(false);
		expect(completed.awaitingDiffApproval).toBe(false);
	});

	// An explicit `false` and an omitted key are the same claim, and the server
	// is free to send either — `skip_serializing_if` is an optimisation, not a
	// contract the client should depend on for correctness.
	it('treats an explicit false the same as an absent key', async () => {
		installTaskFeed([backendTask('explicit', 'paused', { awaiting_diff_approval: false })]);

		await taskStore.loadTasks();

		expect(get(taskStore).tasks[0].awaitingDiffApproval).toBe(false);
	});
});

describe('task abort ordering and reconciliation', () => {
	beforeEach(() => taskStore.reset());
	afterEach(() => {
		taskStore.reset();
		vi.unstubAllGlobals();
	});

	function installAbortBackend(options: { cancelStatus?: number; taskStatus?: number } = {}) {
		const record = backendTask('abort-order', 'running', { title: 'Abort in order' });
		const calls: string[] = [];
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = typeof input === 'string' ? input : input.toString();
			const method = (init?.method ?? 'GET').toUpperCase();
			calls.push(`${method} ${url}`);
			if (url.endsWith('/tasks/internal')) {
				return new Response(JSON.stringify({ tasks: [], pagination: { total: 0 } }), { status: 200 });
			}
			if (url === '/api/magician/v3/tasks') {
				return new Response(JSON.stringify({ tasks: [record] }), { status: 200 });
			}
			if (url.endsWith('/executions/execution-abort-order/cancel')) {
				return new Response(
					JSON.stringify(options.cancelStatus ? { error: 'cancel rejected' } : { cancelled: true }),
					{ status: options.cancelStatus ?? 200, headers: { 'Content-Type': 'application/json' } }
				);
			}
			if (url.endsWith('/tasks/abort-order/status')) {
				return new Response(
					JSON.stringify(options.taskStatus ? { error: 'status rejected' } : { updated: true }),
					{ status: options.taskStatus ?? 200, headers: { 'Content-Type': 'application/json' } }
				);
			}
			throw new Error(`Unhandled request: ${method} ${url}`);
		}));
		return { calls };
	}

	it('cancels the active root before resetting task status', async () => {
		const { calls } = installAbortBackend();
		await taskStore.loadTasks();

		await expect(taskStore.abortTask('abort-order')).resolves.toBe('ready');

		const cancelIndex = calls.findIndex((call) => call.includes('/executions/execution-abort-order/cancel'));
		const statusIndex = calls.findIndex((call) => call.includes('/tasks/abort-order/status'));
		expect(cancelIndex).toBeGreaterThanOrEqual(0);
		expect(statusIndex).toBeGreaterThan(cancelIndex);
		expect(get(taskStore).tasks[0]).toMatchObject({
			status: 'ready',
			activeExecutionId: undefined
		});
	});

	it('does not change task status when cancellation is rejected', async () => {
		const { calls } = installAbortBackend({ cancelStatus: 409 });
		await taskStore.loadTasks();

		await expect(taskStore.abortTask('abort-order')).rejects.toThrow('cancel rejected');

		expect(calls.some((call) => call.includes('/tasks/abort-order/status'))).toBe(false);
		expect(get(taskStore).tasks[0]?.status).toBe('running');
	});

	it('refreshes authoritative task state when reset fails after cancellation', async () => {
		const { calls } = installAbortBackend({ taskStatus: 503 });
		await taskStore.loadTasks();
		const readsBeforeAbort = calls.filter((call) => call === 'GET /api/magician/v3/tasks').length;

		await expect(taskStore.abortTask('abort-order')).rejects.toThrow(
			'Execution stopped, but the task status could not be updated: Failed to update task status to ready (503): status rejected'
		);

		expect(calls.filter((call) => call === 'GET /api/magician/v3/tasks')).toHaveLength(
			readsBeforeAbort + 1
		);
		expect(get(taskStore).tasks[0]?.status).toBe('running');
	});

	it('rejects non-success generic status updates without optimistic drift', async () => {
		installAbortBackend({ taskStatus: 500 });
		await taskStore.loadTasks();

		await expect(taskStore.updateTaskStatus('abort-order', 'cancelled')).rejects.toThrow(
			'status rejected'
		);
		expect(get(taskStore).tasks[0]?.status).toBe('running');
	});
});

describe('chat panel actions for internal tasks', () => {
	beforeEach(() => taskStore.reset());
	afterEach(() => {
		taskStore.reset();
		vi.unstubAllGlobals();
	});

	function internalTaskEnvelope(
		status: 'ready' | 'running' | 'failed',
		activeExecutionId: string | null = null
	) {
		return {
			manifest: {
				task_id: 'internal-task',
				title: 'Delegated research',
				description: 'Research from chat',
				agent_id: 'web-researcher',
				ui_thread_id: 'general',
				created_by: 'chat_delegate',
				approved: true,
				tags: [],
				depends_on: [],
				output_mode: 'accumulate',
				chat_session_id: 'chat-session',
				lifecycle: 'internal',
				sync_mode: 'deferred',
				created_at: '2026-07-30T00:00:00Z',
				updated_at: '2026-07-30T00:00:00Z'
			},
			state: {
				task_id: 'internal-task',
				status,
				active_root_execution_id: activeExecutionId,
				latest_root_execution_id: activeExecutionId ?? 'exec-old',
				updated_at: '2026-07-30T00:01:00Z'
			}
		};
	}

	it('resets an internal failed task even though it is absent from the normal task list', async () => {
		const calls: string[] = [];
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = typeof input === 'string' ? input : input.toString();
			const method = (init?.method ?? 'GET').toUpperCase();
			calls.push(`${method} ${url}`);
			if (method === 'GET' && url.endsWith('/tasks/internal-task')) {
				return new Response(JSON.stringify({ task: internalTaskEnvelope('failed') }), { status: 200 });
			}
			if (method === 'PUT' && url.endsWith('/tasks/internal-task/status')) {
				return new Response(JSON.stringify({ task: internalTaskEnvelope('ready') }), { status: 200 });
			}
			throw new Error(`Unhandled request: ${method} ${url}`);
		}));

		await expect(taskStore.resetTaskToReady('internal-task')).resolves.toBe('ready');
		expect(calls).toEqual([
			'GET /api/magician/v3/tasks/internal-task',
			'PUT /api/magician/v3/tasks/internal-task/status'
		]);
		expect(get(taskStore).tasks).toEqual([]);
	});

	it('starts and stops internal task executions through the same panel actions', async () => {
		let canonicalStatus: 'ready' | 'running' = 'ready';
		const calls: string[] = [];
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = typeof input === 'string' ? input : input.toString();
			const method = (init?.method ?? 'GET').toUpperCase();
			calls.push(`${method} ${url}`);
			if (method === 'GET' && url.endsWith('/tasks/internal-task')) {
				return new Response(JSON.stringify({
					task: internalTaskEnvelope(
						canonicalStatus,
						canonicalStatus === 'running' ? 'exec-new' : null
					)
				}), { status: 200 });
			}
			if (method === 'POST' && url.endsWith('/tasks/internal-task/execute')) {
				canonicalStatus = 'running';
				return new Response(JSON.stringify({ execution: { state: { execution_id: 'exec-new' } } }), { status: 200 });
			}
			if (method === 'POST' && url.endsWith('/executions/exec-new/cancel')) {
				return new Response(JSON.stringify({ cancelled: true }), { status: 200 });
			}
			if (method === 'PUT' && url.endsWith('/tasks/internal-task/status')) {
				return new Response(JSON.stringify({ task: internalTaskEnvelope('ready') }), { status: 200 });
			}
			throw new Error(`Unhandled request: ${method} ${url}`);
		}));

		await expect(taskStore.executeTask('internal-task')).resolves.toMatchObject({
			execution: { state: { execution_id: 'exec-new' } }
		});
		taskStore.reset();
		await expect(taskStore.abortTask('internal-task')).resolves.toBe('ready');
		expect(calls).toContain('POST /api/magician/v3/tasks/internal-task/execute');
		expect(calls).toContain('POST /api/magician/v3/executions/exec-new/cancel');
		expect(calls).toContain('PUT /api/magician/v3/tasks/internal-task/status');
	});
});

describe('task and plan status normalization', () => {
	it.each([
		['pending', 'pending'], ['planning', 'planning'], ['ready', 'ready'],
		['running', 'running'], ['paused', 'paused'], ['completed', 'completed'],
		['failed', 'failed'], ['cancelled', 'cancelled'], ['deferred', 'deferred'],
		['archived', 'archived'],
		['waiting_for_user', 'paused'], ['waiting_for_confirmation', 'paused'],
		['paused_by_user', 'paused'], ['waiting_for_children', 'running'],
		['sleeping', 'deferred'], ['unknown', 'pending'], [null, 'pending']
	])('normalizes V3 status %j to %s', (input, expected) => {
		expect(normalizeV3TaskStatus(input)).toBe(expected);
	});

	it.each([
		['planning', 'planning'], ['draft', 'draft'], ['eliciting', 'eliciting'],
		['approved', 'approved'], ['rejected', 'rejected'], ['failed', 'failed'],
		['unknown', undefined], [null, undefined]
	])('normalizes plan status %j', (input, expected) => {
		expect(normalizeTaskPlanStatus(input)).toBe(expected);
	});

	it.each([
		['running', 'in_progress'], ['executing', 'in_progress'], ['in_progress', 'in_progress'],
		['completed', 'completed'], ['failed', 'failed'], ['skipped', 'skipped'],
		['cancelled', 'cancelled'], ['pending', 'pending'], ['unknown', 'pending']
	])('normalizes execution step status %j to %s', (input, expected) => {
		expect(normalizeExecutionPlanStepStatus(input)).toBe(expected);
	});

	it('maps plan graph steps, metadata fallback, and edge dependencies', () => {
		expect(extractPlanStepsFromPlanGraph({
			steps: [
				{ id: 'a', task: 'Collect data', tool: 'search', confidence: 0.9 },
				{ id: 'b', metadata: { description: 'Write report' }, providing_agent_id: 'scribe' },
				{ id: 'c', depends_on: ['manual'] }
			],
			edges: [{ from: 'a', to: 'b' }, { from: 'bad' }, null]
		})).toEqual([
			{
				id: 'a', description: 'Collect data', status: 'pending', tool_name: 'search',
				providing_agent_id: undefined, confidence: 0.9, depends_on: []
			},
			{
				id: 'b', description: 'Write report', status: 'pending', tool_name: undefined,
				providing_agent_id: 'scribe', confidence: undefined, depends_on: ['a']
			},
			{
				id: 'c', description: 'Step c', status: 'pending', tool_name: undefined,
				providing_agent_id: undefined, confidence: undefined, depends_on: ['manual']
			}
		]);
	});

	it('maps execution panel steps and drops null optional values', () => {
		expect(extractPlanStepsFromExecutionPanel({
			debug: { selected_execution: { step_statuses: [
				{ number: 1, name: 'Research', status: 'executing', step_id: 's1', capability: 'web', confidence: 0.7, delegate_agent_id: 'sleuth' },
				{ number: 2, name: '', status: 'mystery', step_id: null, capability: null, confidence: null, delegate_agent_id: null }
			] } }
		})).toEqual([
			{ id: 's1', description: 'Research', status: 'in_progress', tool_name: 'web', providing_agent_id: 'sleuth', confidence: 0.7 },
			{ id: 'step-2', description: 'Step 2', status: 'pending', tool_name: undefined, providing_agent_id: undefined, confidence: undefined }
		]);
	});

	it('returns no steps for malformed projections', () => {
		expect(extractPlanStepsFromPlanGraph(null)).toEqual([]);
		expect(extractPlanStepsFromPlanGraph({ steps: [] })).toEqual([]);
		expect(extractPlanStepsFromExecutionPanel({})).toEqual([]);
	});

	it('normalizes single and multiple pending questions', () => {
		expect(firstPendingPlanQuestion({ question_id: 'q1', question_text: 'Which account?' })).toEqual({ id: 'q1', question: 'Which account?' });
		expect(firstPendingPlanQuestion({ id: 'q2', question: 'Which date?' })).toEqual({ id: 'q2', question: 'Which date?' });
		expect(firstPendingPlanQuestion({ id: 'q3' })).toBeUndefined();
		expect(pendingPlanQuestions([
			{ question_id: 'q1', question_text: 'One?' },
			{ nope: true },
			{ id: 'q2', question: 'Two?' }
		])).toEqual([
			{ id: 'q1', question: 'One?' },
			{ id: 'q2', question: 'Two?' }
		]);
	});
});

describe('task plan lifecycle derivation', () => {
	it('requires explicit approval when planStatus exists', () => {
		expect(taskHasExecutablePlan({ planStatus: 'approved', hasPlan: false, planSteps: [] })).toBe(true);
		expect(taskHasExecutablePlan({ planStatus: 'draft', hasPlan: true, planSteps: [{ id: 's', description: 'x', status: 'pending' }] })).toBe(false);
		expect(taskHasExecutablePlan({ hasPlan: true, planSteps: [] })).toBe(true);
		expect(taskHasExecutablePlan({ hasPlan: false, planSteps: [{ id: 's', description: 'x', status: 'pending' }] })).toBe(true);
		expect(taskHasExecutablePlan(null)).toBe(false);
	});

	it('resets direct executions to ready but unplanned tasks to pending', () => {
		expect(taskResetStatus({ executionId: 'exec', hasPlan: false, planSteps: [] })).toBe('ready');
		expect(taskResetStatus({ hasPlan: false, planSteps: [] })).toBe('pending');
		expect(taskResetStatus({ planStatus: 'approved', hasPlan: true, planSteps: [] })).toBe('ready');
	});

	it.each([
		['planning', 'planning'], ['eliciting', 'planning'], ['draft', 'pending'],
		['rejected', 'pending'], ['failed', 'failed']
	] as const)('derives task status from %s plan', (planStatus, expected) => {
		expect(deriveTaskStatusFromPlan(task(), planStatus)).toBe(expected);
	});

	it('only marks approved plans ready when task approval and dependencies allow it', () => {
		expect(deriveTaskStatusFromPlan(task({ approved: true, isBlocked: false }), 'approved')).toBe('ready');
		expect(deriveTaskStatusFromPlan(task({ approved: false }), 'approved')).toBe('pending');
		expect(deriveTaskStatusFromPlan(task({ isBlocked: true }), 'approved')).toBe('pending');
	});

	it('preserves terminal and active statuses without a plan', () => {
		for (const status of ['running', 'paused', 'completed', 'cancelled', 'deferred'] as const) {
			expect(deriveTaskStatusWithoutPlan(task({ status }))).toBe(status);
		}
		expect(deriveTaskStatusWithoutPlan(task({ status: 'failed', executionId: 'exec' }))).toBe('failed');
		expect(deriveTaskStatusWithoutPlan(task({ status: 'failed' }))).toBe('pending');
	});

	it('applies approved plan payload with questions, steps, timestamps, and ready state', () => {
		const result = applyTaskPlanPayload(task(), {
			status: 'approved',
			plan_id: 'plan-1',
			updated_at: '2026-07-11T10:00:00Z',
			pending_questions: [{ question_id: 'q1', question_text: 'Confirm scope?' }],
			plan_graph: { steps: [{ id: 's1', task: 'Execute' }] }
		});
		expect(result).toMatchObject({
			status: 'ready',
			hasPlan: true,
			planStatus: 'approved',
			latestPlanId: 'plan-1',
			pendingQuestion: { id: 'q1', question: 'Confirm scope?' },
			pendingQuestions: [{ id: 'q1', question: 'Confirm scope?' }],
			progress: 0,
			updatedAt: '2026-07-11T10:00:00.000Z'
		});
		expect(result.planSteps).toHaveLength(1);
		expect(result.planGeneratedAt).toBe(Date.parse('2026-07-11T10:00:00Z'));
	});

	it('preserves live and terminal execution states when late plan data arrives', () => {
		expect(applyTaskPlanPayload(task({ status: 'running', progress: 45 }), {
			status: 'approved', plan_graph: { steps: [{ id: 's1' }] }
		})).toMatchObject({ status: 'running', progress: 45 });
		expect(applyTaskPlanPayload(task({ status: 'failed', executionId: 'exec', errorMessage: 'boom' }), {
			status: 'approved', plan_graph: { steps: [{ id: 's1' }] }
		})).toMatchObject({ status: 'failed', errorMessage: 'boom' });
	});

	it('uses a backend planning error and clears stale errors after recovery', () => {
		expect(applyTaskPlanPayload(task({ errorMessage: 'old' }), {
			status: 'failed', error: 'provider unavailable'
		})).toMatchObject({ status: 'failed', errorMessage: 'provider unavailable' });
		expect(applyTaskPlanPayload(task({ errorMessage: 'old' }), {
			status: 'approved', plan_graph: { steps: [{ id: 's1' }] }
		}).errorMessage).toBeUndefined();
	});

	it('clears plan state while preserving active execution progress', () => {
		const result = clearTaskPlanState(task({
			status: 'running',
			executionId: 'exec',
			hasPlan: true,
			planStatus: 'approved',
			planSteps: [{ id: 's1', description: 'Run', status: 'in_progress' }],
			progress: 40,
			pendingQuestion: { id: 'q', question: 'Continue?' }
		}));
		expect(result.status).toBe('running');
		expect(result.planSteps).toHaveLength(1);
		expect(result.progress).toBe(40);
		expect(result.pendingQuestion).toBeUndefined();
		expect(result.planStatus).toBeUndefined();
	});

	it('computes progress from all settled step outcomes', () => {
		const result = applyTaskPlanSteps(task(), [
			{ id: '1', description: 'one', status: 'completed' },
			{ id: '2', description: 'two', status: 'failed' },
			{ id: '3', description: 'three', status: 'in_progress' },
			{ id: '4', description: 'four', status: 'skipped' }
		]);
		expect(result.hasPlan).toBe(true);
		expect(result.progress).toBe(75);
	});

	it.each([
		['task_plan_approval_required:task-1', 'This plan must be approved before execution.'],
		['task_plan_not_ready:task-1:planning', 'Planning is still in progress for this task.'],
		['task_plan_not_ready:task-1:eliciting', 'This plan is waiting for clarification before it can run.'],
		['task_plan_not_ready:task-1:approved', 'The approved plan is not ready to run yet.'],
		['task_plan_not_ready:task-1:draft', 'This plan is not ready for execution yet.'],
		['other', null]
	])('maps execution gate error %s', (code, expected) => {
		expect(parseTaskPlanExecutionGateError(code)).toBe(expected);
	});
});

describe('task timestamp and filter projection', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date('2026-07-11T12:00:00.000Z'));
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('normalizes numeric and parseable timestamps and uses now as fallback', () => {
		expect(parseTimestampToIso(0)).toBe('1970-01-01T00:00:00.000Z');
		expect(parseTimestampToIso('2026-01-02T03:04:05Z')).toBe('2026-01-02T03:04:05.000Z');
		expect(parseTimestampToIso('not-a-date')).toBe('2026-07-11T12:00:00.000Z');
	});

	it('projects active work for All and terminal work for Completed', () => {
		const tasks = [
			task({ id: 'pending', status: 'pending' }),
			task({ id: 'failed', status: 'failed' }),
			task({ id: 'done', status: 'completed' })
		];
		expect(computeFilteredTasks(tasks, 'all', '').map((entry) => entry.id)).toEqual(['pending', 'failed']);
		expect(computeFilteredTasks(tasks, 'completed', '').map((entry) => entry.id)).toEqual(['done']);
	});

	it('keeps freshly completed work out of Completed during its grace period', () => {
		const done = task({ id: 'done', status: 'completed' });
		expect(computeFilteredTasks([done], 'completed', '', new Set(['done']))).toEqual([]);
	});

	it('groups running and paused work together', () => {
		const tasks = [
			task({ id: 'run', status: 'running' }),
			task({ id: 'pause', status: 'paused' }),
			task({ id: 'ready', status: 'ready' })
		];
		expect(computeFilteredTasks(tasks, 'running', '').map((entry) => entry.id)).toEqual(['run', 'pause']);
	});

	it('filters today and overdue without treating completed work as overdue', () => {
		const todayStr = readerLocalDate();
		const tasks = [
			task({ id: 'today', dueDate: `${todayStr}T18:00:00Z` }),
			task({ id: 'late', dueDate: '2020-01-01', status: 'pending' }),
			task({ id: 'late-done', dueDate: '2020-01-01', status: 'completed' })
		];
		expect(computeFilteredTasks(tasks, 'today', '').map((entry) => entry.id)).toEqual(['today']);
		expect(computeFilteredTasks(tasks, 'overdue', '').map((entry) => entry.id)).toEqual(['late']);
	});

	it('defines Inbox as untagged pending work, including grace-period completions', () => {
		const tasks = [
			task({ id: 'inbox', status: 'pending' }),
			task({ id: 'tagged', tags: [{ name: 'ops', color: '' }] }),
			task({ id: 'done', status: 'completed' })
		];
		expect(computeFilteredTasks(tasks, 'inbox', '', new Set(['done'])).map((entry) => entry.id)).toEqual(['inbox', 'done']);
	});

	it('filters by exact tag and searches title or tag case-insensitively', () => {
		const tasks = [
			task({ id: 'one', title: 'Quarterly Review', tags: [{ name: 'Finance', color: '' }] }),
			task({ id: 'two', title: 'Other', tags: [{ name: 'ops', color: '' }] })
		];
		expect(computeFilteredTasks(tasks, { custom: 'Finance' }, '').map((entry) => entry.id)).toEqual(['one']);
		expect(computeFilteredTasks(tasks, 'all', 'quarter').map((entry) => entry.id)).toEqual(['one']);
		expect(computeFilteredTasks(tasks, 'all', 'FINANCE').map((entry) => entry.id)).toEqual(['one']);
	});
});

describe('the reader s local date', () => {
	it('names the local day rather than the UTC one it falls in', () => {
		// 00:30 on the 12th, local. The old derivation ran local midnight
		// through toISOString(), which renders it in UTC — so everywhere east
		// of Greenwich it answered the 11th, for the whole day.
		expect(readerLocalDate(new Date(2026, 6, 12, 0, 30))).toBe('2026-07-12');
		expect(readerLocalDate(new Date(2026, 0, 5, 23, 59))).toBe('2026-01-05');
	});
});

describe('server-answered lanes and the grace-period overlay', () => {
	it('keeps a just-ticked task in its lane even though the server no longer lists it', () => {
		// The server has answered `inbox` and, correctly, left out the task that
		// was completed a second ago. The reader has not seen their tick land yet.
		const serverPage = [task({ id: 't1', status: 'pending', tags: [] })];
		const pendingCompletions = new Set(['t2']);
		const held = [task({ id: 't2', status: 'completed', tags: [] })];

		const shown = applyPendingCompletionOverlay('inbox', serverPage, held, pendingCompletions);

		expect(shown.map((entry) => entry.id)).toEqual(['t1', 't2']);
	});

	it('holds a just-ticked task out of Completed until the grace period ends', () => {
		const serverPage = [task({ id: 't2', status: 'completed', tags: [] })];
		const shown = applyPendingCompletionOverlay('completed', serverPage, [], new Set(['t2']));
		expect(shown).toEqual([]);
	});

	it('leaves lanes with no grace-period rule untouched', () => {
		const serverPage = [task({ id: 't1', status: 'running', tags: [] })];
		const shown = applyPendingCompletionOverlay('running', serverPage, [], new Set(['t1']));
		expect(shown.map((entry) => entry.id)).toEqual(['t1']);
	});

	it('takes lane membership from the server rather than re-deriving it', () => {
		// Stored state says this task is running; the server put it in Inbox.
		// The server wins — that is the whole point of asking it.
		const tasks = [
			task({ id: 'server-says-inbox', status: 'running' }),
			task({ id: 'client-would-say-inbox', status: 'pending' })
		];
		const answer = { view: 'inbox' as const, ids: new Set(['server-says-inbox']) };
		expect(
			computeFilteredTasks(tasks, 'inbox', '', new Set(), answer).map((entry) => entry.id)
		).toEqual(['server-says-inbox']);
	});

	it('falls back to the mirror when the answer describes a different lane', () => {
		// A filter switch costs a round trip. The previous lane's answer must
		// never be applied to the new lane — the list would show rows the pill
		// above it does not claim.
		const tasks = [
			task({ id: 'run', status: 'running' }),
			task({ id: 'wait', status: 'pending' })
		];
		const staleAnswer = { view: 'inbox' as const, ids: new Set(['wait']) };
		expect(
			computeFilteredTasks(tasks, 'running', '', new Set(), staleAnswer).map((entry) => entry.id)
		).toEqual(['run']);
	});

	it('does not add a held task the lane already lists', () => {
		// The tick has not reached the server, so its Inbox answer still has the
		// task. Adding it back would render one task as two rows and count it twice.
		const rows = [task({ id: 't2', status: 'pending', tags: [] })];
		const shown = applyPendingCompletionOverlay('inbox', rows, rows, new Set(['t2']));
		expect(shown.map((entry) => entry.id)).toEqual(['t2']);
	});

	it('reads lane counts only for the lanes the server actually sent', () => {
		expect(readTaskLaneCounts(undefined)).toBeNull();
		expect(readTaskLaneCounts({})).toBeNull();
		expect(readTaskLaneCounts({ all: 4, inbox: 0, nonsense: 9 })).toEqual({ all: 4, inbox: 0 });
	});
});

describe('filter badges', () => {
	/**
	 * The mock server, answering the paged branch the way the real one does —
	 * `view=` applied to the pool, `counts` taken over the whole pool *before*
	 * the lane, and the pre-pagination `{tasks}` shape when neither `limit` nor
	 * `offset` is sent. Its predicates come from the shared test backend, which
	 * writes them out rather than importing the client's, so the client cannot
	 * agree with it by construction.
	 */
	function installLaneBackend(rows: TaskBackendRecord[]): void {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = typeof input === 'string' ? input : input.toString();
				if (url.includes('/tasks/internal')) {
					return new Response(JSON.stringify({ tasks: [] }), { status: 200 });
				}
				const [path, queryString = ''] = url.split('?');
				if (path !== '/api/magician/v3/tasks') {
					throw new Error(`Unhandled request: ${url}`);
				}
				if (!queryString) {
					return new Response(JSON.stringify({ tasks: rows }), { status: 200 });
				}
				const params = new URLSearchParams(queryString);
				const today = params.get('today') ?? '';
				const view = params.get('view');
				const pool = view ? rows.filter((row) => matchesLane(view, row, today)) : rows;
				return new Response(
					JSON.stringify({
						tasks: pool,
						pagination: { total: pool.length, limit: pool.length, offset: 0, has_more: false },
						counts: laneCounts(rows, today)
					}),
					{ status: 200 }
				);
			})
		);
	}

	async function showLane(lane: 'inbox' | 'today' | 'completed'): Promise<void> {
		taskStore.setFilter(lane);
		await taskStore.loadTasks();
	}

	beforeEach(() => {
		// The grace period is a 5s timer; nothing here may be allowed to run it.
		vi.useFakeTimers();
		taskStore.reset();
	});

	afterEach(() => {
		vi.useRealTimers();
		taskStore.reset();
		vi.unstubAllGlobals();
	});

	/**
	 * The defect this guards: the badge counts the corpus and the rows carry a
	 * grace-period overlay the corpus knows nothing about, so a naive swap has
	 * Inbox reading `1` above two visible rows. Both go through
	 * `pendingCompletionLaneDelta`, and this is what says so.
	 */
	it('agrees with the rows on screen during a grace period, in every lane that has a rule', async () => {
		const rows = [
			backendTask('t1', 'pending'),
			backendTask('t2', 'pending', { due_date: readerLocalDate() }),
			backendTask('t3', 'completed')
		];
		installLaneBackend(rows);
		await taskStore.loadTasks();

		await taskStore.completeTask('t2');
		// The tick has landed on the server too — the case where the lane answer
		// and the reader's screen genuinely disagree, and the overlay earns its keep.
		rows[1].status = 'completed';

		for (const lane of ['inbox', 'today', 'completed'] as const) {
			await showLane(lane);
			const shownRows = get(taskStore).filteredTasks.length;
			expect(get(taskCounts)[lane], lane).toBe(shownRows);
		}

		// …and the numbers themselves, so agreement at zero would not pass.
		await showLane('inbox');
		expect(get(taskCounts).inbox).toBe(2); // t1, plus the ticked t2 lingering
		await showLane('today');
		expect(get(taskCounts).today).toBe(1); // t2, still due today
		await showLane('completed');
		expect(get(taskCounts).completed).toBe(1); // t3 only — t2 is still in its grace period
	});

	it('reports the other lanes as the server counted them, over the whole corpus', async () => {
		const rows = [
			backendTask('t1', 'pending'),
			backendTask('t2', 'running'),
			backendTask('t3', 'completed')
		];
		installLaneBackend(rows);
		await taskStore.loadTasks();

		expect(get(taskCounts)).toEqual({
			all: 2,
			inbox: 1,
			today: 0,
			overdue: 0,
			running: 1,
			completed: 1
		});
	});

	/**
	 * The badge describes the corpus, not the rows that happen to be loaded.
	 *
	 * Today the web client loads the whole corpus, so counting locally would
	 * give the same answer and this could be got wrong without anything looking
	 * wrong — until Phase 1 pages the list and every badge quietly starts
	 * describing page one. The fixture answers a corpus far larger than the rows
	 * it returns, which is the only way to tell the two apart from here.
	 */
	it('reports what the server counted, not what happens to be loaded', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = typeof input === 'string' ? input : input.toString();
				if (url.includes('/tasks/internal')) {
					return new Response(JSON.stringify({ tasks: [] }), { status: 200 });
				}
				const rows = [backendTask('t1', 'pending')];
				if (!url.includes('?')) {
					return new Response(JSON.stringify({ tasks: rows }), { status: 200 });
				}
				return new Response(
					JSON.stringify({
						tasks: rows,
						pagination: { total: 40, limit: 1, offset: 0, has_more: true },
						counts: { all: 33, inbox: 7, today: 4, overdue: 2, running: 5, completed: 11 }
					}),
					{ status: 200 }
				);
			})
		);

		await taskStore.loadTasks();

		expect(get(taskStore).tasks).toHaveLength(1);
		expect(get(taskCounts)).toEqual({
			all: 33,
			inbox: 7,
			today: 4,
			overdue: 2,
			running: 5,
			completed: 11
		});
	});

	it('renders no badge at all when the server reported no counts', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = typeof input === 'string' ? input : input.toString();
				if (url.includes('/tasks/internal')) {
					return new Response(JSON.stringify({ tasks: [] }), { status: 200 });
				}
				// No `counts` key — what an older server, or the legacy unpaged
				// branch, answers. A fabricated `0` would be a claim nobody made.
				return new Response(JSON.stringify({ tasks: [backendTask('t1', 'pending')] }), {
					status: 200
				});
			})
		);

		await taskStore.loadTasks();

		expect(get(taskStore).tasks).toHaveLength(1);
		expect(get(taskCounts)).toEqual({
			all: null,
			inbox: null,
			today: null,
			overdue: null,
			running: null,
			completed: null
		});
	});
});
