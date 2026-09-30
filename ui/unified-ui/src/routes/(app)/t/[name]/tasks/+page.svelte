<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onDestroy, onMount } from 'svelte';
	import { get } from 'svelte/store';
	import ExecutionPlanInspector from '../../../ExecutionPlanInspector.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import ExportMenu from '$lib/magician/components/ExportMenu.svelte';
	import TaskPanelDrawer from '$lib/magician/tasks/TaskPanelDrawer.svelte';
	import { toTaskPanelModel, type PanelOutputFile } from '$lib/magician/tasks/taskPanelModel';
	import { fetchTaskOutputFiles, taskOutputUrl } from '$lib/magician/tasks/taskOutputs';
	import { readTaskRunState } from '$lib/magician/tasks/taskAttention';
	import {
		createTaskPanelPoll,
		panelPollCadence,
		type PanelPollTarget
	} from '$lib/magician/tasks/taskPanelPoll';
	import type { ExecutionPanelState } from '$lib/types/executionPanel';
	// Task-scoped despite the module name: both post to
	// `/v3/tasks/{id}/outputs/open-file|open-folder`.
	import {
		openInternalTaskOutputFile,
		revealInternalTaskOutputFile
	} from '$lib/internalTasks/api';
	import TaskCardMenu from '$lib/magician/components/TaskCardMenu.svelte';
	import TaskFilterToolbar, {
		computeStatusChips
	} from '$lib/magician/components/TaskFilterToolbar.svelte';
	import NativeTasksSurface from '$lib/magician/tasks/NativeTasksSurface.svelte';
	import TaskCreateForm from '$lib/magician/tasks/TaskCreateForm.svelte';
	import type {
		AgentPickerOption,
		ParsedTaskAction,
		ParsedTaskCompletionChange,
		TaskCreateSubmitValues
	} from '$lib/magician/tasks/types';
	import { showError, showInfo, showSuccess } from '$lib/shared/stores/notifications';
	import { personalAgentList } from '$lib/stores/agentStore';
	import { requestAttentionInput } from '$lib/stores/attentionPromptStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		taskStore,
		type Task,
		type TaskOutputMode,
		type TaskPriority
	} from '$lib/stores/taskStore';
	import { threadStore } from '$lib/stores/threadStore';
	import type { TaskSchedule } from '$lib/types/agents';
	import { normalizeThreadId } from '$lib/threads/normalizeThreadId';
	import { getThreadPageContext } from '$lib/threads/threadPageContext';
	import { isValidCronExpression } from '$lib/utils/cron';

	type ThreadTaskBucket = 'running' | 'needs_action' | 'ready' | 'pending' | 'done';

	// Tasks tab of the thread workspace. The list/cards and create form use the
	// same native task primitives as `/tasks`, and so does the task panel: this
	// route holds a real store `Task`, so it reuses `toTaskPanelModel` and
	// `TaskPanelDrawer` unchanged rather than carrying a second panel. Thread
	// identity/load comes from the shell (../+layout.svelte) via context; a
	// `?selected=<taskId>` deep-link opens that panel.
	const threadPage = getThreadPageContext();
	$: threadName = $threadPage.threadName;
	$: threadDisplayName = $threadPage.threadDisplayName;
	$: loadingThread = $threadPage.loadingThread;

	let interactionBusy = false;
	let threadComposeExpanded = false;
	let threadScheduleExpanded = false;
	let threadActiveFilter = 'all';
	/** Page-level ⋯ overflow menu (TaskCardMenu), anchored to the native card trigger. */
	let threadTaskMenuTaskId: string | null = null;
	let threadTaskMenuAnchor: HTMLElement | null = null;
	let threadActiveTagEditorTaskId: string | null = null;
	let threadActiveScheduleEditorTaskId: string | null = null;
	let threadCreateTitle = '';
	let threadCreateDescription = '';
	let threadCreateOutputMode: TaskOutputMode = 'accumulate';
	let threadSelectedAgentId = '';
	let threadCreateScheduleCron = '';
	let threadCreateScheduleTimezone =
		browser ? Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' : 'UTC';

	let isPanelOpen = false;
	let selectedTaskId: string | null = null;
	let isPlanSheetOpen = false;
	let planSheetTaskId: string | null = null;
	let appliedRouteSelection: string | null = null;
	let lastThreadName = '';

	/*
	 * The unified task panel's inputs. Same three loads `/tasks` performs, for
	 * the same reasons: the outputs list and the run behind the panel are fetched
	 * per selected task and each carries the id it describes, so a slow reply for
	 * one task can never render under another. See `TasksWorkspace.svelte`.
	 */
	let panelOutputs: PanelOutputFile[] | null = null;
	let panelOutputsTaskId: string | null = null;
	let panelOutputsRequestId = 0;
	let panelRunState: ExecutionPanelState | null = null;
	let panelRunStateTaskId: string | null = null;
	/**
	 * Which of the task's runs the reader asked to read, or `null` for the one the
	 * task record points at. Same shape and same reason as `TasksWorkspace.svelte`
	 * — reader intent rather than loaded state, keyed on the task it was chosen
	 * about so it cannot be read under another one.
	 */
	let panelRunSelectionId: string | null = null;
	let panelRunSelectionTaskId: string | null = null;
	let panelNow = Date.now();
	let panelClockHandle: ReturnType<typeof setInterval> | null = null;
	const PANEL_CLOCK_INTERVAL_MS = 60_000;
	/** When this route last reloaded the task list — the panel's "as of" line. */
	let lastLoadedAt: number | null = null;
	let panelLoadFailure: string | null = null;

	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: threadTasks = sortThreadTasks(
		$taskStore.tasks.filter((task) => normalizeThreadId(task.uiThreadId) === threadName)
	);
	$: threadTags = [
		...new Set(
			threadTasks
				.flatMap((task) => task.tags.map((tag) => (typeof tag === 'string' ? tag : tag.name)))
				.filter(Boolean)
		)
	];
	$: threadFilterCounts = (() => {
		const today = new Date();
		today.setHours(0, 0, 0, 0);
		const todayStr = today.toISOString().split('T')[0];
		return {
			all: threadTasks.filter((task) => task.status !== 'completed').length,
			inbox: threadTasks.filter((task) => task.tags.length === 0 && task.status === 'pending')
				.length,
			today: threadTasks.filter((task) => task.dueDate?.startsWith(todayStr)).length,
			overdue: threadTasks.filter(
				(task) => task.dueDate && task.dueDate < todayStr && task.status !== 'completed'
			).length,
			running: threadTasks.filter((task) => task.status === 'running' || task.status === 'paused')
				.length,
			completed: threadTasks.filter((task) => task.status === 'completed').length
		};
	})();
	$: selectedTask =
		selectedTaskId ? $taskStore.tasks.find((task) => task.id === selectedTaskId) ?? null : null;
	$: planSheetTask =
		planSheetTaskId ? $taskStore.tasks.find((task) => task.id === planSheetTaskId) ?? null : null;
	$: routeSelectedTaskId = ($page.url.searchParams.get('selected') || '').trim() || null;

	/*
	 * The panel's "as of", stamped from the store rather than from each refresh
	 * call site. The list on screen is replaced by the store — including by its
	 * own poll, which no call site can see.
	 */
	$: $taskStore.tasks, (lastLoadedAt = Date.now());

	const panelOutputsGuard = { key: '' };
	$: {
		const taskId = browser && isPanelOpen ? (selectedTask?.id ?? null) : null;
		const key = taskId === null ? '' : `${taskId}:${selectedTask?.status ?? ''}`;
		if (key !== panelOutputsGuard.key) {
			panelOutputsGuard.key = key;
			void loadPanelOutputs(taskId);
		}
	}

	$: panelPollTarget =
		browser && isPanelOpen && selectedTask
			? {
					taskId: selectedTask.id,
					executionId:
						panelRunSelectionTaskId === selectedTask.id ? panelRunSelectionId : null
				}
			: null;
	$: panelPoll.aim(panelPollTarget, panelPollCadence(selectedTask?.status ?? null));

	/**
	 * The panel's whole input, through the same adapter `/tasks` uses — this
	 * route already holds a real store `Task`, so there is nothing to translate
	 * twice. The two id checks are what keep an output list or an ask belonging
	 * to the task the reader just left from rendering under this one.
	 */
	$: panelModel =
		selectedTask === null
			? null
			: toTaskPanelModel(
					selectedTask,
					panelOutputsTaskId === selectedTask.id ? panelOutputs : null,
					panelRunStateTaskId === selectedTask.id ? panelRunState : null,
					// The same id check, third time: a run id is meaningless against
					// another task.
					panelRunSelectionTaskId === selectedTask.id ? panelRunSelectionId : null,
					(path) =>
						taskOutputUrl(
							selectedTask.id,
							path,
							$scopeIdentityStore.principal,
							$scopeIdentityStore.workspace
						)
				);

	/**
	 * Whether Escape is the drawer's to take. This route stacks a plan sheet, a
	 * card overflow menu, a tag editor and a schedule editor over the list; the
	 * innermost one wins, and the drawer only closes when none of them is up.
	 */
	$: escapeBelongsToPanel =
		!isPlanSheetOpen
		&& threadTaskMenuTaskId === null
		&& threadActiveTagEditorTaskId === null
		&& threadActiveScheduleEditorTaskId === null;

	$: threadPersonalAgents = ($personalAgentList || []).map(
		(agent): AgentPickerOption => ({
			agent_id: agent.agent_id,
			name: agent.name || agent.agent_id
		})
	);
	$: threadActiveTag = threadActiveFilter.startsWith('tag:') ? threadActiveFilter.slice(4) : null;
	$: threadFilteredTasks = filterThreadTasks(threadTasks, threadActiveFilter);
	$: if (threadSelectedAgentId && !threadPersonalAgents.some((agent) => agent.agent_id === threadSelectedAgentId)) {
		threadSelectedAgentId = '';
	}
	$: if (!threadSelectedAgentId && threadPersonalAgents.length > 0) {
		threadSelectedAgentId = threadPersonalAgents[0].agent_id;
	}

	const TAG_COLOR_FALLBACKS = [
		'#ff6b6b',
		'#4ecdc4',
		'#4d9de0',
		'#ffe66d',
		'#00bb7f',
		'#ff8c8c',
		'#4ecdc4',
		'#ffe66d'
	] as const;

	type ThreadPageToken = {
		scopeKey: string;
		threadId: string;
	};

	function captureThreadPageToken(): ThreadPageToken {
		return {
			scopeKey: currentScopeKey,
			threadId: threadName
		};
	}

	function isStaleThreadPageToken(token: ThreadPageToken): boolean {
		return token.scopeKey !== currentScopeKey || token.threadId !== threadName;
	}

	function titleCase(value: string): string {
		return value.replace(/_/g, ' ').replace(/\b\w/g, (match) => match.toUpperCase());
	}

	function resetTasksLocalState(): void {
		closeAllTaskMenus();
		selectedTaskId = null;
		isPanelOpen = false;
		isPlanSheetOpen = false;
		planSheetTaskId = null;
		appliedRouteSelection = null;
		threadComposeExpanded = false;
		threadScheduleExpanded = false;
		threadActiveFilter = 'all';
		interactionBusy = false;
		threadCreateTitle = '';
		threadCreateDescription = '';
		threadCreateOutputMode = 'accumulate';
		threadCreateScheduleCron = '';
		threadCreateScheduleTimezone =
			browser ? Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' : 'UTC';
	}

	function filterThreadTasks(tasks: Task[], activeFilter: string): Task[] {
		const today = new Date();
		today.setHours(0, 0, 0, 0);
		const todayStr = today.toISOString().split('T')[0];

		if (activeFilter.startsWith('tag:')) {
			const tagName = activeFilter.slice(4);
			return tasks.filter((task) =>
				task.tags.some((tag) => (typeof tag === 'string' ? tag : tag.name) === tagName)
			);
		}

		switch (activeFilter) {
			case 'inbox':
				return tasks.filter((task) => task.tags.length === 0 && task.status === 'pending');
			case 'today':
				return tasks.filter((task) => task.dueDate?.startsWith(todayStr));
			case 'overdue':
				return tasks.filter(
					(task) => task.dueDate && task.dueDate < todayStr && task.status !== 'completed'
				);
			case 'running':
				return tasks.filter((task) => task.status === 'running' || task.status === 'paused');
			case 'completed':
				return tasks.filter((task) => task.status === 'completed');
			default:
				return tasks.filter((task) => task.status !== 'completed');
		}
	}

	function taskBucket(task: Task): ThreadTaskBucket {
		if (task.status === 'running' || task.status === 'planning') return 'running';
		if (task.status === 'paused' || task.status === 'failed') return 'needs_action';
		if (task.status === 'ready') return 'ready';
		if (task.status === 'completed') return 'done';
		return 'pending';
	}

	function bucketRank(bucket: ThreadTaskBucket): number {
		switch (bucket) {
			case 'running':
				return 0;
			case 'needs_action':
				return 1;
			case 'ready':
				return 2;
			case 'pending':
				return 3;
			case 'done':
				return 4;
		}
	}

	function sortThreadTasks(tasks: Task[]): Task[] {
		return [...tasks].sort((left, right) => {
			const bucketDelta = bucketRank(taskBucket(left)) - bucketRank(taskBucket(right));
			if (bucketDelta !== 0) return bucketDelta;
			const leftUpdated = Date.parse(left.updatedAt || left.createdAt || '') || 0;
			const rightUpdated = Date.parse(right.updatedAt || right.createdAt || '') || 0;
			return rightUpdated - leftUpdated;
		});
	}

	function resolveAbortTarget(task: Task): string | null {
		const executionId =
			typeof task.activeExecutionId === 'string' ? task.activeExecutionId.trim() : '';
		return executionId.length > 0 ? executionId : null;
	}

	function closeAllTaskMenus(): void {
		threadTaskMenuTaskId = null;
		threadTaskMenuAnchor = null;
		threadActiveTagEditorTaskId = null;
		threadActiveScheduleEditorTaskId = null;
	}

	function openNativeThreadTaskMenu(taskId: string, anchor: HTMLElement): void {
		const wasOpen = threadTaskMenuTaskId === taskId;
		closeAllTaskMenus();
		if (!wasOpen) {
			threadTaskMenuTaskId = taskId;
			threadTaskMenuAnchor = anchor;
		}
	}

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value);
		}
		return '';
	}

	function todayIsoDate(): string {
		return new Date().toISOString().slice(0, 10);
	}

	function isoDateWithOffset(days: number): string {
		const date = new Date();
		date.setHours(0, 0, 0, 0);
		date.setDate(date.getDate() + days);
		return date.toISOString().slice(0, 10);
	}

	function parseTaskOutputMode(value: unknown): TaskOutputMode {
		return typeof value === 'string' && value.trim().toLowerCase() === 'overwrite'
			? 'overwrite'
			: 'accumulate';
	}

	function tagColorForName(name: string): string {
		let hash = 0;
		for (let index = 0; index < name.length; index += 1) {
			hash = (hash * 31 + name.charCodeAt(index)) | 0;
		}
		const paletteIndex = Math.abs(hash) % 8;
		if (browser) {
			const resolved = getComputedStyle(document.documentElement)
				.getPropertyValue(`--theme-chart-color-${paletteIndex}`)
				.trim();
			if (resolved) return resolved;
		}
		return TAG_COLOR_FALLBACKS[paletteIndex];
	}

	async function refreshThreadTasks(token: ThreadPageToken): Promise<void> {
		await taskStore.loadTasks();
		if (isStaleThreadPageToken(token)) return;
		await threadStore.ensureThread(threadName);
	}

	/**
	 * Load the open task's outputs. The request id guards the *reply*, not the
	 * request: a fetch already in flight cannot be recalled, so a late answer for
	 * the task the reader left is discarded rather than rendered.
	 */
	async function loadPanelOutputs(taskId: string | null): Promise<void> {
		if (taskId === null) {
			panelOutputsRequestId += 1;
			panelOutputs = null;
			panelOutputsTaskId = null;
			return;
		}

		const requestId = ++panelOutputsRequestId;
		if (panelOutputsTaskId !== taskId) {
			panelOutputs = null;
			panelOutputsTaskId = null;
		}
		const scope = get(scopeIdentityStore);
		const files = await fetchTaskOutputFiles(taskId, scope.principal, scope.workspace);
		if (requestId !== panelOutputsRequestId) return;
		panelOutputs = files;
		panelOutputsTaskId = taskId;
	}

	const panelPoll = createTaskPanelPoll<ExecutionPanelState | null>({
		read: async (target: PanelPollTarget) => {
			const scope = get(scopeIdentityStore);
			const [, run] = await Promise.all([
				taskStore.refreshTask(target.taskId),
				readTaskRunState(
					target.taskId,
					scope.principal,
					scope.workspace,
					target.executionId
				)
			]);
			if (!run.ok) throw new Error(run.reason);
			return run.state;
		},
		onSnapshot: (target, state, at) => {
			if (target.taskId !== (selectedTask?.id ?? null)) return;
			panelRunState = state;
			panelRunStateTaskId = target.taskId;
			lastLoadedAt = at;
			panelLoadFailure = null;
		},
		onFailure: (target, message) => {
			if (target.taskId !== (selectedTask?.id ?? null)) return;
			panelLoadFailure = message;
		}
	});

	/**
	 * The reader picked a different run to read — re-read `/execution-panel` for it.
	 * The selection is recorded before the fetch and never rolled back; a failure
	 * leaves the Run act's timeline absent, which is design §6's partial load and
	 * what the panel's Retry re-reads. See `TasksWorkspace.svelte`.
	 */
	function handlePanelSelectRun(event: CustomEvent<{ executionId: string }>): void {
		const taskId = selectedTask?.id ?? null;
		if (taskId === null) return;
		const executionId = event.detail?.executionId?.trim() ?? '';
		if (!executionId) return;
		panelRunSelectionId = executionId;
		panelRunSelectionTaskId = taskId;
	}

	async function handlePanelOpenFile(
		event: CustomEvent<{ file: PanelOutputFile; index: number }>
	): Promise<void> {
		const taskId = selectedTask?.id;
		const path = event.detail.file.path;
		if (!taskId || !path) return;
		const scope = get(scopeIdentityStore);
		try {
			await openInternalTaskOutputFile(taskId, path, scope.principal, scope.workspace);
		} catch (error) {
			showError(error instanceof Error ? error.message : `Couldn't open ${event.detail.file.name}`);
		}
	}

	async function handlePanelRevealFile(
		event: CustomEvent<{ file: PanelOutputFile; index: number }>
	): Promise<void> {
		const taskId = selectedTask?.id;
		const path = event.detail.file.path;
		if (!taskId || !path) return;
		const scope = get(scopeIdentityStore);
		try {
			await revealInternalTaskOutputFile(taskId, path, scope.principal, scope.workspace);
		} catch (error) {
			showError(
				error instanceof Error ? error.message : `Couldn't reveal ${event.detail.file.name}`
			);
		}
	}

	/**
	 * The panel's Retry (design §6, case 2): reload the task, its outputs and the
	 * ask blocking it. Clearing the two keys is what happens in the meantime —
	 * what is on screen was loaded before a failure, so it is unknown until the
	 * replies land rather than shown as current.
	 */
	async function handlePanelRetry(): Promise<void> {
		panelOutputsTaskId = null;
		panelOutputsGuard.key = '';
		panelRunStateTaskId = null;
		panelLoadFailure = null;
		panelPoll.refreshNow();
		await refreshThreadTasks(captureThreadPageToken());
	}

	/**
	 * The controls the drawer's header carries.
	 *
	 * The drawer is modal over the list, so a card's own actions are unreachable
	 * while it is open. These route through `handleThreadTaskAction`, the same
	 * entry point the cards use, so a card and the panel cannot mean different
	 * things by `Stop`. Pending work also keeps the canonical Plan / Run-now
	 * choice rather than collapsing those two materially different actions.
	 */
	function panelActions(task: Task): Array<{ action: ParsedTaskAction['action']; label: string }> {
		if (task.status === 'running' || task.status === 'planning') {
			return [{ action: 'abort', label: 'Stop' }];
		}
		if (task.status === 'paused' || task.status === 'failed' || task.status === 'cancelled') {
			return task.executionId ? [{ action: 'reset', label: 'Reset to Ready' }] : [];
		}
		if (task.status === 'ready') {
			return [{ action: 'doit', label: 'Run' }];
		}
		if (task.status === 'pending') {
			return [
				{ action: 'doit', label: 'Plan' },
				{ action: 'doit_direct', label: 'Run now' }
			];
		}
		return [];
	}

	onMount(() => {
		if (!browser) return;
		panelNow = Date.now();
		panelClockHandle = setInterval(() => (panelNow = Date.now()), PANEL_CLOCK_INTERVAL_MS);
	});

	onDestroy(() => {
		if (panelClockHandle !== null) {
			clearInterval(panelClockHandle);
			panelClockHandle = null;
		}
		panelPoll.stop();
	});

	function clearThreadCreateForm(): void {
		threadCreateTitle = '';
		threadCreateDescription = '';
		threadCreateOutputMode = 'accumulate';
		threadCreateScheduleCron = '';
		threadCreateScheduleTimezone =
			browser ? Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' : 'UTC';
		threadScheduleExpanded = false;
	}

	async function handleThreadCreateTask(values: TaskCreateSubmitValues): Promise<void> {
		if (interactionBusy) return;
		const token = captureThreadPageToken();
		interactionBusy = true;
		try {
			const title = asString(values.task_title).trim();
			if (!title) throw new Error('Task title is required');
			const description = asString(values.task_description).trim();
			if (!description) throw new Error('Description is required');
			const outputMode = parseTaskOutputMode(values.task_output_mode);
			const agentId =
				asString(values.task_agent).trim() ||
				threadSelectedAgentId ||
				threadPersonalAgents[0]?.agent_id ||
				'';
			if (!agentId) throw new Error('No personal agent available.');
			const agentName =
				threadPersonalAgents.find((agent) => agent.agent_id === agentId)?.name || agentId;

			let schedule: TaskSchedule | undefined;
			const cronValue = asString(values.schedule_cron).trim();
			if (threadScheduleExpanded && cronValue) {
				if (!isValidCronExpression(cronValue)) {
					throw new Error('Invalid cron expression. Must be 5 space-separated fields (e.g. "0 9 * * *").');
				}
				schedule = { cron: cronValue };
				const timezone = asString(values.schedule_timezone).trim();
				if (timezone) schedule.timezone = timezone;
			}

			await taskStore.createTask(title, description, {
				agentId,
				agentName,
				createdBy: 'user',
				outputMode,
				uiThreadId: threadName,
				...(schedule ? { schedule } : {})
			});
			if (isStaleThreadPageToken(token)) return;
			showSuccess('Task created');
			threadComposeExpanded = false;
			clearThreadCreateForm();
			await refreshThreadTasks(token);
		} catch (error) {
			if (isStaleThreadPageToken(token)) return;
			showError(error instanceof Error ? error.message : 'Failed to create task');
		} finally {
			if (!isStaleThreadPageToken(token)) {
				interactionBusy = false;
			}
		}
	}

	async function openTaskPanel(taskId: string): Promise<void> {
		const token = captureThreadPageToken();
		let task = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		if (!task) {
			await taskStore.loadTasks();
			if (isStaleThreadPageToken(token)) return;
			task = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		}
		if (isStaleThreadPageToken(token)) return;
		if (!task) {
			await goto(`/tasks?filter=all&selected=${encodeURIComponent(taskId)}`, {
				replaceState: false,
				noScroll: true
			});
			return;
		}
		selectedTaskId = task.id;
		isPanelOpen = true;
		await taskStore.selectTask(task.id);
	}

	async function openPlanSheet(taskId: string): Promise<void> {
		const token = captureThreadPageToken();
		let task = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		if (!task) {
			await taskStore.loadTasks();
			if (isStaleThreadPageToken(token)) return;
			task = get(taskStore).tasks.find((candidate) => candidate.id === taskId) ?? null;
		}
		if (isStaleThreadPageToken(token)) return;
		if (!task) {
			showError('Task not found.');
			return;
		}
		planSheetTaskId = task.id;
		isPlanSheetOpen = true;
	}

	function closePlanSheet(): void {
		isPlanSheetOpen = false;
		planSheetTaskId = null;
	}

	function closeTaskPanel(): void {
		isPanelOpen = false;
		selectedTaskId = null;
		panelRunSelectionId = null;
		panelRunSelectionTaskId = null;
		void taskStore.selectTask(null);
		if ($page.url.searchParams.has('selected')) {
			const params = new URLSearchParams($page.url.searchParams);
			params.delete('selected');
			const nextUrl = params.toString()
				? `${$page.url.pathname}?${params.toString()}`
				: $page.url.pathname;
			void goto(nextUrl, { replaceState: true, noScroll: true });
		}
	}

	async function startTaskWithCurrentState(task: Task, direct = false): Promise<void> {
		const token = captureThreadPageToken();
		if (direct) {
			showInfo('Starting direct execution (no plan)...');
			await taskStore.executeTaskDirect(task.id);
			if (isStaleThreadPageToken(token)) return;
			showSuccess('Direct execution started.');
			await refreshThreadTasks(token);
			if (isStaleThreadPageToken(token)) return;
			await openTaskPanel(task.id);
			return;
		}

		if (task.planStatus === 'planning') {
			showInfo('Planning is still in progress for this task.');
			await openTaskPanel(task.id);
			return;
		}
		if (task.planStatus === 'eliciting') {
			showInfo(
				task.pendingQuestion
					? 'This plan is waiting for your answer before it can continue.'
					: 'This plan is waiting for clarification before it can continue.'
			);
			await openTaskPanel(task.id);
			return;
		}
		if (task.planStatus === 'draft') {
			showInfo('Review and approve the plan before execution.');
			await openTaskPanel(task.id);
			return;
		}
		if (task.planStatus === 'approved') {
			if (task.status !== 'ready') {
				showInfo('This approved plan is not ready to run yet.');
				await openTaskPanel(task.id);
				return;
			}
			showInfo(`Starting execution for "${task.title}"...`);
			await taskStore.executeTask(task.id);
			if (isStaleThreadPageToken(token)) return;
			showSuccess('Execution started.');
			await refreshThreadTasks(token);
			if (isStaleThreadPageToken(token)) return;
			await openTaskPanel(task.id);
			return;
		}
		if (task.status === 'pending') {
			showInfo(`Generating plan for "${task.title}"...`);
			if (task.planStatus === 'rejected' || task.planStatus === 'failed') {
				await taskStore.replanTask(task.id);
				if (isStaleThreadPageToken(token)) return;
				showSuccess('Planning restarted.');
			} else {
				await taskStore.planTask(task.id);
				if (isStaleThreadPageToken(token)) return;
				showSuccess('Plan generated. Review and execute when ready.');
			}
			await refreshThreadTasks(token);
			if (isStaleThreadPageToken(token)) return;
			await openTaskPanel(task.id);
			return;
		}
		if (task.status === 'ready') {
			showInfo(`Starting execution for "${task.title}"...`);
			await taskStore.executeTask(task.id);
			if (isStaleThreadPageToken(token)) return;
			showSuccess('Execution started.');
			await refreshThreadTasks(token);
			if (isStaleThreadPageToken(token)) return;
			await openTaskPanel(task.id);
			return;
		}
		throw new Error('Task is not in a state for planning or execution.');
	}

	function toggleScheduleEditor(taskId: string): void {
		const wasOpen = threadActiveScheduleEditorTaskId === taskId;
		closeAllTaskMenus();
		threadActiveScheduleEditorTaskId = wasOpen ? null : taskId;
	}

	async function handleThreadTaskAction(action: ParsedTaskAction): Promise<void> {
		if (action.action === 'menu') {
			return;
		}
		if (interactionBusy) return;
		const token = captureThreadPageToken();
		interactionBusy = true;
		try {
			const task =
				threadTasks.find((candidate) => candidate.id === action.taskId) ??
				$taskStore.tasks.find((candidate) => candidate.id === action.taskId);
			if (!task) {
				throw new Error('Task was not found');
			}

			switch (action.action) {
				case 'open':
					await openTaskPanel(action.taskId);
					break;
				case 'doit':
					await startTaskWithCurrentState(task);
					break;
				case 'doit_direct':
					await startTaskWithCurrentState(task, true);
					break;
				case 'abort': {
					const abortTarget = resolveAbortTarget(task);
					if (!abortTarget) {
						throw new Error('Task has no active execution.');
					}
					const nextStatus = await taskStore.abortTask(action.taskId);
					if (isStaleThreadPageToken(token)) return;
					showSuccess(nextStatus === 'ready' ? 'Task aborted. Ready to re-execute.' : 'Task aborted.');
					await refreshThreadTasks(token);
					break;
				}
				case 'delete':
				case 'menu_delete':
					await taskStore.deleteTask(action.taskId);
					if (isStaleThreadPageToken(token)) return;
					showInfo('Task deleted.');
					await refreshThreadTasks(token);
					break;
				case 'reset': {
					const resetStatus = await taskStore.resetTaskToReady(action.taskId);
					if (isStaleThreadPageToken(token)) return;
					showInfo(resetStatus === 'ready' ? 'Task reset to ready.' : 'Task reset to pending.');
					await refreshThreadTasks(token);
					break;
				}
				case 'cancel':
					// Cancel via the task-status endpoint — works even when the
					// task is stuck in planning/clarification with a ghost execution.
					await taskStore.updateTaskStatus(action.taskId, 'cancelled');
					if (isStaleThreadPageToken(token)) return;
					showSuccess('Task cancelled.');
					await refreshThreadTasks(token);
					break;
				case 'schedule_today':
					await taskStore.updateTask(action.taskId, { dueDate: todayIsoDate() });
					if (isStaleThreadPageToken(token)) return;
					showInfo('Due date set to today.');
					await refreshThreadTasks(token);
					break;
				case 'schedule_tomorrow': {
					const dueDate = isoDateWithOffset(1);
					await taskStore.updateTask(action.taskId, { dueDate });
					if (isStaleThreadPageToken(token)) return;
					showInfo(`Due date set to ${dueDate}.`);
					await refreshThreadTasks(token);
					break;
				}
				case 'schedule_next_week': {
					const dueDate = isoDateWithOffset(7);
					await taskStore.updateTask(action.taskId, { dueDate });
					if (isStaleThreadPageToken(token)) return;
					showInfo(`Due date set to ${dueDate}.`);
					await refreshThreadTasks(token);
					break;
				}
				case 'schedule_clear':
					await taskStore.updateTask(action.taskId, { dueDate: null });
					if (isStaleThreadPageToken(token)) return;
					showInfo('Due date removed.');
					await refreshThreadTasks(token);
					break;
				case 'priority_clear':
					await taskStore.updateTask(action.taskId, { priority: null });
					if (isStaleThreadPageToken(token)) return;
					showInfo('Priority removed.');
					await refreshThreadTasks(token);
					break;
				case 'priority_p1':
				case 'priority_p2':
				case 'priority_p3':
				case 'priority_p4': {
					const priority = action.action.slice('priority_'.length) as TaskPriority;
					await taskStore.updateTask(action.taskId, { priority });
					if (isStaleThreadPageToken(token)) return;
					showInfo(`Priority set to ${priority.toUpperCase()}.`);
					await refreshThreadTasks(token);
					break;
				}
				case 'schedule_edit':
					toggleScheduleEditor(action.taskId);
					break;
				case 'edit_description': {
					const result = await requestAttentionInput({
						title: 'Edit task description',
						body: task.title,
						kind: 'multiline',
						defaultValue: task.description || '',
						placeholder: 'Task description... (Cmd/Ctrl+Enter to save)',
						confirmLabel: 'Save'
					});
					if (
						result &&
						(result.kind === 'multiline' || result.kind === 'text') &&
						result.value !== task.description
					) {
						await taskStore.updateTask(action.taskId, { description: result.value });
						if (isStaleThreadPageToken(token)) return;
						showSuccess('Description updated.');
						await refreshThreadTasks(token);
					}
					break;
				}
			}
		} catch (error) {
			if (isStaleThreadPageToken(token)) return;
			showError(error instanceof Error ? error.message : 'Task action failed');
		} finally {
			if (!isStaleThreadPageToken(token)) {
				interactionBusy = false;
			}
		}
	}

	async function handleThreadTaskCompletionChange(
		change: ParsedTaskCompletionChange
	): Promise<void> {
		if (interactionBusy) return;
		const token = captureThreadPageToken();
		interactionBusy = true;
		try {
			const task =
				threadTasks.find((candidate) => candidate.id === change.taskId) ??
				$taskStore.tasks.find((candidate) => candidate.id === change.taskId);
			if (!task) {
				throw new Error('Task was not found');
			}
			if (change.checked) {
				if (task.status === 'running' || task.status === 'paused') {
					throw new Error('Cannot complete a running task.');
				}
				await taskStore.completeTask(change.taskId);
				if (isStaleThreadPageToken(token)) return;
				showSuccess(`Marked "${task.title}" as complete.`);
			} else {
				await taskStore.uncompleteTask(change.taskId);
				if (isStaleThreadPageToken(token)) return;
				showInfo(`Moved "${task.title}" back to pending.`);
			}
		} catch (error) {
			if (isStaleThreadPageToken(token)) return;
			showError(error instanceof Error ? error.message : 'Failed to update task completion');
		} finally {
			if (!isStaleThreadPageToken(token)) {
				interactionBusy = false;
			}
		}
	}

	async function handleThreadAddTag(taskId: string, tagNameRaw: string): Promise<void> {
		if (!browser || interactionBusy) return;
		const token = captureThreadPageToken();
		const task =
			threadTasks.find((candidate) => candidate.id === taskId) ??
			$taskStore.tasks.find((candidate) => candidate.id === taskId);
		if (!task) {
			showError('Task was not found');
			return;
		}
		const tagName = tagNameRaw.trim().replace(/^#/, '');
		if (!tagName) return;
		const hasExisting = task.tags.some(
			(tag) => asString(typeof tag === 'string' ? tag : tag.name).trim().toLowerCase() === tagName.toLowerCase()
		);
		if (hasExisting) {
			showInfo(`Tag "${tagName}" already exists.`);
			threadActiveTagEditorTaskId = null;
			return;
		}
		interactionBusy = true;
		try {
			await taskStore.updateTask(taskId, {
				tags: [...task.tags, { name: tagName, color: tagColorForName(tagName) }]
			});
			if (isStaleThreadPageToken(token)) return;
			showSuccess(`Tag "${tagName}" added.`);
			await refreshThreadTasks(token);
		} catch (error) {
			if (isStaleThreadPageToken(token)) return;
			showError(error instanceof Error ? error.message : 'Failed to add tag');
		} finally {
			if (!isStaleThreadPageToken(token)) {
				threadActiveTagEditorTaskId = null;
				interactionBusy = false;
			}
		}
	}

	async function handleThreadRemoveTag(taskId: string, tagName: string): Promise<void> {
		if (!browser || interactionBusy) return;
		const token = captureThreadPageToken();
		const task =
			threadTasks.find((candidate) => candidate.id === taskId) ??
			$taskStore.tasks.find((candidate) => candidate.id === taskId);
		if (!task) {
			showError('Task was not found');
			return;
		}
		const sanitized = tagName.toLowerCase();
		const nextTags = task.tags.filter(
			(tag) => asString(typeof tag === 'string' ? tag : tag.name).toLowerCase() !== sanitized
		);
		if (nextTags.length === task.tags.length) return;
		interactionBusy = true;
		try {
			await taskStore.updateTask(taskId, { tags: nextTags });
			if (isStaleThreadPageToken(token)) return;
			showSuccess(`Tag "${tagName}" removed.`);
			await refreshThreadTasks(token);
		} catch (error) {
			if (isStaleThreadPageToken(token)) return;
			showError(error instanceof Error ? error.message : 'Failed to remove tag');
		} finally {
			if (!isStaleThreadPageToken(token)) {
				interactionBusy = false;
			}
		}
	}

	async function handleThreadNativeScheduleSubmit(
		event: CustomEvent<{
			taskId: string;
			values: {
				schedule_cron: string;
				schedule_timezone: string;
				schedule_retention_max_records: string;
				schedule_retention_max_days: string;
			};
		}>
	): Promise<void> {
		if (interactionBusy) return;
		const token = captureThreadPageToken();
		interactionBusy = true;
		closeAllTaskMenus();
		threadActiveScheduleEditorTaskId = null;
		try {
			const { taskId, values } = event.detail;
			const cronValue = asString(values.schedule_cron).trim();
			const tzValue = asString(values.schedule_timezone).trim();
			const retentionMaxRecords = asString(values.schedule_retention_max_records).trim();
			const retentionMaxDays = asString(values.schedule_retention_max_days).trim();
			if (!cronValue) {
				await taskStore.updateTask(taskId, { schedule: null });
				if (isStaleThreadPageToken(token)) return;
				showInfo('Schedule removed.');
			} else {
				if (!isValidCronExpression(cronValue)) {
					throw new Error('Invalid cron expression. Must be 5 space-separated fields (e.g. "0 9 * * *").');
				}
				const schedule: TaskSchedule = { cron: cronValue };
				if (tzValue) schedule.timezone = tzValue;
				const maxRec = retentionMaxRecords ? parseInt(retentionMaxRecords, 10) : undefined;
				const maxDays = retentionMaxDays ? parseInt(retentionMaxDays, 10) : undefined;
				if (retentionMaxRecords && (maxRec === undefined || isNaN(maxRec) || maxRec < 1)) {
					throw new Error('Max runs must be a positive number.');
				}
				if (retentionMaxDays && (maxDays === undefined || isNaN(maxDays) || maxDays < 1)) {
					throw new Error('Max days must be a positive number.');
				}
				if ((maxRec !== undefined && maxRec > 0) || (maxDays !== undefined && maxDays > 0)) {
					schedule.execution_history_retention = {};
					if (maxRec && maxRec > 0) schedule.execution_history_retention.max_records = maxRec;
					if (maxDays && maxDays > 0) schedule.execution_history_retention.max_age_days = maxDays;
				}
				await taskStore.updateTask(taskId, { schedule });
				if (isStaleThreadPageToken(token)) return;
				showInfo('Schedule updated.');
			}
			await refreshThreadTasks(token);
		} catch (error) {
			if (isStaleThreadPageToken(token)) return;
			showError(error instanceof Error ? error.message : 'Failed to update schedule');
		} finally {
			if (!isStaleThreadPageToken(token)) {
				interactionBusy = false;
			}
		}
	}

	// ── Page-level ⋯ overflow menu (TaskCardMenu) ──
	// A filter switch (tag filters fold into `tag:<name>` values of the same
	// variable) re-renders the surface, so an open menu's anchor (and the
	// inline editors' cards) may no longer exist — close them. Runs once
	// harmlessly on init (everything starts null).
	$: threadActiveFilter, closeAllTaskMenus();
	$: threadTaskMenuTask = threadTaskMenuTaskId
		? threadTasks.find((next) => next.id === threadTaskMenuTaskId) ??
			$taskStore.tasks.find((next) => next.id === threadTaskMenuTaskId) ??
			null
		: null;
	$: threadTaskMenuCanEdit = threadTaskMenuTask
		? threadTaskMenuTask.source === 'task' && threadTaskMenuTask.readOnly !== true
		: false;
	$: threadTaskMenuCanCancel =
		threadTaskMenuCanEdit &&
		threadTaskMenuTask != null &&
		!['completed', 'failed', 'cancelled'].includes(threadTaskMenuTask.status);
	$: threadTaskMenuPriority = threadTaskMenuTask
		? asString(threadTaskMenuTask.priority).trim().toUpperCase()
		: '';
	$: threadTaskMenuDueDateRaw = threadTaskMenuTask
		? asString(threadTaskMenuTask.dueDate).trim().slice(0, 10)
		: '';

	async function setThreadTaskDueDate(taskId: string, dueDate: string): Promise<void> {
		if (interactionBusy) return;
		const token = captureThreadPageToken();
		interactionBusy = true;
		try {
			if (!/^\d{4}-\d{2}-\d{2}$/.test(dueDate)) {
				throw new Error('Choose a valid due date.');
			}
			await taskStore.updateTask(taskId, { dueDate });
			if (isStaleThreadPageToken(token)) return;
			showInfo(`Due date set to ${dueDate}.`);
			await refreshThreadTasks(token);
		} catch (error) {
			if (isStaleThreadPageToken(token)) return;
			showError(error instanceof Error ? error.message : 'Failed to set due date');
		} finally {
			if (!isStaleThreadPageToken(token)) {
				interactionBusy = false;
			}
		}
	}

	async function handleThreadTaskMenuSelect(event: CustomEvent<{ action: string }>): Promise<void> {
		const taskId = threadTaskMenuTaskId;
		closeAllTaskMenus();
		if (!taskId) return;
		await handleThreadTaskAction({
			taskId,
			action: event.detail.action as ParsedTaskAction['action']
		});
	}

	async function handleThreadTaskMenuDueDate(event: CustomEvent<{ value: string }>): Promise<void> {
		const taskId = threadTaskMenuTaskId;
		closeAllTaskMenus();
		if (!taskId) return;
		await setThreadTaskDueDate(taskId, event.detail.value);
	}

	// Reset task-local UI state when the active thread changes (the shell
	// reloads the data; this clears stale selection/menus/filter for the new
	// thread). Guarded so it doesn't fire on first render.
	$: if (lastThreadName && threadName !== lastThreadName) {
		lastThreadName = threadName;
		resetTasksLocalState();
	} else if (!lastThreadName) {
		lastThreadName = threadName;
	}

	// `?selected=<taskId>` opens the execution panel (set by deep-links + the
	// bare-/t/<id> redirect, and round-tripped by openTaskPanel/closeTaskPanel).
	$: if (browser && routeSelectedTaskId && routeSelectedTaskId !== appliedRouteSelection) {
		appliedRouteSelection = routeSelectedTaskId;
		void openTaskPanel(routeSelectedTaskId);
	}

	$: if (!routeSelectedTaskId && appliedRouteSelection) {
		appliedRouteSelection = null;
	}
</script>

<svelte:window
	on:keydown={(event) => {
		if (event.key === 'Escape' && isPlanSheetOpen) {
			closePlanSheet();
		}
	}}
/>

<div class="thread-tasks-pane presto-spells-page presto-gaui-page">
	<TaskFilterToolbar
		activeFilter={threadActiveFilter}
		counts={threadFilterCounts}
		tags={threadTags}
		activeTag={threadActiveTag}
		totalCount={threadTasks.length}
		statusChips={computeStatusChips(threadTasks)}
		on:filter={(event) => {
			threadActiveFilter = event.detail.value;
		}}
		on:tag={(event) => {
			const tag = event.detail.value;
			threadActiveFilter = tag ? `tag:${tag}` : 'all';
		}}
	/>
	<div class="thread-tasks-content">
		<div
			class:presto-spells-compose-collapsed={!threadComposeExpanded}
			class="thread-task-compose presto-spells-compose-card"
		>
			<button
				class:open={threadComposeExpanded}
				class="thread-task-compose-toggle"
				type="button"
				on:click={() => (threadComposeExpanded = !threadComposeExpanded)}
			>
				{threadComposeExpanded ? 'Hide' : '+ New Task'}
			</button>
			{#if threadComposeExpanded}
				<TaskCreateForm
					bind:title={threadCreateTitle}
					bind:description={threadCreateDescription}
					bind:outputMode={threadCreateOutputMode}
					bind:selectedAgentId={threadSelectedAgentId}
					bind:scheduleExpanded={threadScheduleExpanded}
					bind:scheduleCron={threadCreateScheduleCron}
					bind:scheduleTimezone={threadCreateScheduleTimezone}
					agentOptions={threadPersonalAgents}
					threadOptions={[{ id: threadName, name: threadDisplayName }]}
					selectedThreadId={threadName}
					threadLocked={true}
					defaultTimezone={threadCreateScheduleTimezone || 'UTC'}
					disabled={interactionBusy}
					descriptionLabel="Description"
					descriptionPlaceholder={`Describe the task for #${threadDisplayName}`}
					on:submit={(event) => handleThreadCreateTask(event.detail.values)}
					on:clear={clearThreadCreateForm}
				/>
			{/if}
		</div>
		<NativeTasksSurface
			tasks={threadFilteredTasks}
			selectedTaskId={selectedTaskId}
			isLoading={loadingThread}
			{interactionBusy}
			pageError={null}
			activeFilter={threadActiveFilter}
			activeTagEditorTaskId={threadActiveTagEditorTaskId}
			activeScheduleEditorTaskId={threadActiveScheduleEditorTaskId}
			on:action={(event) => handleThreadTaskAction(event.detail)}
			on:complete={(event) => handleThreadTaskCompletionChange(event.detail)}
			on:tagAdd={(event) => handleThreadAddTag(event.detail.taskId, event.detail.tagName)}
			on:tagRemove={(event) => handleThreadRemoveTag(event.detail.taskId, event.detail.tagName)}
			on:toggleTagEditor={(event) => {
				const taskId = event.detail.taskId;
				const editorWasOpen = threadActiveTagEditorTaskId === taskId;
				closeAllTaskMenus();
				threadActiveTagEditorTaskId = editorWasOpen ? null : taskId;
			}}
			on:toggleScheduleEditor={(event) => {
				const taskId = event.detail.taskId;
				const next = threadActiveScheduleEditorTaskId === taskId ? null : taskId;
				closeAllTaskMenus();
				threadActiveScheduleEditorTaskId = next;
			}}
			on:scheduleSubmit={handleThreadNativeScheduleSubmit}
			on:executionChanged={() => void taskStore.loadTasks()}
			on:openMenu={(event) => openNativeThreadTaskMenu(event.detail.taskId, event.detail.anchor)}
			on:compose={() => (threadComposeExpanded = true)}
		/>
		{#if threadTaskMenuTask && threadTaskMenuAnchor}
			<!-- Keyed on the task id: the menu manages position/anchor-ARIA in
			     onMount/onDestroy, so switching to another card's trigger
			     without an intervening close must remount it. -->
			{#key threadTaskMenuTaskId}
				<TaskCardMenu
					anchor={threadTaskMenuAnchor}
					taskTitle={threadTaskMenuTask.title}
					canEditTask={threadTaskMenuCanEdit}
					canCancel={threadTaskMenuCanCancel}
					priority={threadTaskMenuPriority}
					dueDateRaw={threadTaskMenuDueDateRaw}
					hasDueDate={threadTaskMenuDueDateRaw !== ''}
					disabled={interactionBusy}
					on:select={handleThreadTaskMenuSelect}
					on:dueDate={handleThreadTaskMenuDueDate}
					on:close={closeAllTaskMenus}
				/>
			{/key}
		{/if}
	</div>
</div>

{#if isPlanSheetOpen && planSheetTaskId}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="thread-plan-sheet-backdrop" on:click={closePlanSheet}>
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div
			class="thread-plan-sheet"
			role="dialog"
			aria-modal="true"
			aria-label="Task plan inspector"
			tabindex="-1"
			on:click|stopPropagation
		>
			<header class="thread-plan-sheet__header">
				<div class="thread-plan-sheet__copy">
					<span class="thread-plan-sheet__eyebrow">Plan Inspector</span>
					<h2>{planSheetTask?.title || 'Task plan'}</h2>
					<p>
						{#if planSheetTask?.planStatus}
							{titleCase(planSheetTask.planStatus)} plan for #{threadName}.
						{:else}
							Graph, waterfall, history, and persisted task plan for #{threadName}.
						{/if}
					</p>
				</div>
				<Button label="Close" variant="outline" size="sm" on:click={closePlanSheet} />
			</header>
			<div class="thread-plan-sheet__body">
				<ExecutionPlanInspector taskId={planSheetTaskId} modal={true} />
			</div>
		</div>
	</div>
{/if}

<!--
	The unified task panel, in the shared drawer. The chrome — scrim, dialog
	role, focus capture and restore, Escape, loading skeleton, header — is
	`TaskPanelDrawer`'s and is not restated here. What belongs to this route is
	the action ladder below: the drawer is modal over the thread's task list, so
	a card's own controls are unreachable while it is open.
-->
{#if isPanelOpen && selectedTask}
	{@const drawerTask = selectedTask}
	<TaskPanelDrawer
		task={panelModel}
		title={drawerTask.title}
		description={drawerTask.description ?? null}
		threadId={drawerTask.uiThreadId ?? 'general'}
		loadError={$taskStore.error ?? panelLoadFailure}
		lastLoadedAt={lastLoadedAt}
		now={panelNow}
		outputActions={true}
		closeOnEscape={escapeBelongsToPanel}
		on:close={closeTaskPanel}
		on:openFile={handlePanelOpenFile}
		on:revealFile={handlePanelRevealFile}
		on:retry={handlePanelRetry}
		on:selectRun={handlePanelSelectRun}
	>
		<svelte:fragment slot="actions">
			{#each panelActions(drawerTask) as action (action.action)}
				<button
					type="button"
					class="task-panel__action"
					disabled={interactionBusy}
					on:click={() =>
						void handleThreadTaskAction({ taskId: drawerTask.id, action: action.action })}
				>
					{action.label}
				</button>
			{/each}
			<ExportMenu taskId={drawerTask.id} />
		</svelte:fragment>
	</TaskPanelDrawer>
{/if}

<style>
	.thread-tasks-pane {
		flex: 1;
		min-height: 0;
		min-width: 0;
		max-width: 1320px;
		width: 100%;
		margin: 0 auto;
		overflow-y: auto;
		padding-top: 0;
		padding-bottom: var(--attention-bar-offset, 0px);
	}

	.thread-tasks-content {
		padding: 0 0.25rem;
		/* Positioning context for the page-level TaskCardMenu (absolute within
		   this box → the open menu scrolls with the task list). */
		position: relative;
	}

	.thread-task-compose {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		margin: 0 0 0.85rem;
		border: 1px solid var(--border-soft, #e5e7eb);
		border-radius: 8px;
		background: var(--bg-card, #fff);
		padding: 0.8rem;
		box-shadow: var(--shadow-sm, 0 1px 2px rgb(0 0 0 / 0.06));
	}

	.thread-task-compose.presto-spells-compose-collapsed {
		align-items: flex-start;
		padding: 0.65rem 0.8rem;
	}

	.thread-task-compose-toggle {
		align-self: stretch;
		width: 100%;
		min-height: 1.8rem;
		border: 1px solid var(--accent-primary, #c2502a);
		border-radius: 6px;
		background: var(--accent-primary, #c2502a);
		color: var(--button-primary-color, #fff);
		cursor: pointer;
		font: inherit;
		font-size: 0.76rem;
		font-weight: 820;
		line-height: 1;
		padding: 0.38rem 0.7rem;
	}

	.thread-task-compose-toggle.open {
		border-color: var(--border-soft, #d8d2c8);
		background: transparent;
		color: var(--text-secondary, #5f6668);
	}

	.thread-plan-sheet-backdrop {
		position: fixed;
		inset: 0;
		z-index: 980;
		background: rgba(33, 37, 41, 0.34);
		backdrop-filter: blur(2px);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 1rem;
	}

	.thread-plan-sheet {
		width: min(1040px, calc(100vw - 1.8rem));
		max-height: calc(100dvh - 2rem);
		display: flex;
		flex-direction: column;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: 1.2rem;
		box-shadow: 0 22px 56px rgba(26, 32, 44, 0.18);
		overflow: hidden;
	}

	.thread-plan-sheet__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		padding: 1rem 1.05rem 0.85rem;
		border-bottom: 1px solid var(--border-soft, #eee4dc);
		background: linear-gradient(
			180deg,
			color-mix(
					in srgb,
					var(--accent-secondary-soft, rgba(78, 205, 196, 0.12)) 72%,
					var(--bg-card, #ffffff)
				)
				0%,
			var(--bg-card, #ffffff) 100%
		);
	}

	.thread-plan-sheet__copy {
		min-width: 0;
	}

	.thread-plan-sheet__eyebrow {
		display: inline-block;
		font-size: 0.68rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted, #8f9799);
		margin-bottom: 0.25rem;
	}

	.thread-plan-sheet__copy h2 {
		margin: 0;
		font-size: 1rem;
		font-weight: 700;
		color: var(--text-primary, #2d3436);
	}

	.thread-plan-sheet__copy p {
		margin: 0.25rem 0 0;
		font-size: 0.76rem;
		line-height: 1.35;
		color: var(--text-muted, #7f8c8d);
	}

	.thread-plan-sheet__body {
		flex: 1;
		min-height: 0;
		overflow: auto;
		padding: 0.9rem 1rem 1rem;
		background: var(--bg-base, #fffdf8);
	}

	@media (max-width: 900px) {
		.thread-plan-sheet-backdrop {
			padding: 0.4rem;
		}

		.thread-plan-sheet {
			width: 100%;
			max-height: calc(100dvh - 0.8rem);
			border-radius: 1rem;
		}

		.thread-plan-sheet__header {
			padding: 0.85rem 0.9rem 0.75rem;
		}

		.thread-plan-sheet__body {
			padding: 0.75rem 0.85rem 0.85rem;
		}
	}
</style>
