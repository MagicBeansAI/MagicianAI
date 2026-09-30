<script lang="ts">
	import { onDestroy, onMount, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { page } from '$app/stores';
	import { scopeIdentityStore, getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';
	import Select from '$lib/magician/components/native/Select.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { pageAfterRemoval } from '$lib/shared/components/pageAfterRemoval';
	import ExecutionControls from '$lib/magician/components/execution/ExecutionControls.svelte';
	import TaskPanelDrawer from '$lib/magician/tasks/TaskPanelDrawer.svelte';
	import { toInternalTaskPanelModel } from '$lib/internalTasks/internalTaskPanelModel';
	// One minter for both routes. The internal `internalTaskOutputDownloadUrl`
	// below builds the same address but leaves the path unencoded, which is fine
	// for the download anchor it was written for and not for an `<img src>`; the
	// panel's rows use this one so a filename with a space still resolves.
	import {
		loadOutputPreview,
		openAuthenticatedTaskOutput,
		taskOutputUrl
	} from '$lib/magician/tasks/taskOutputs';
	import {
		createTaskPanelPoll,
		panelPollCadence,
		type PanelPollTarget
	} from '$lib/magician/tasks/taskPanelPoll';
	import type { TaskFilePreview } from '$lib/magician/tasks/taskFilePreview';
	import type { PanelOutputFile } from '$lib/magician/tasks/taskPanelModel';
	import type { ActId } from '$lib/magician/tasks/taskCapabilities';
	import { normalizeV3TaskStatus } from '$lib/stores/taskStore';
	import {
		cancelInternalExecution,
		deleteInternalTask,
		fetchInternalTaskDetails,
		fetchInternalTaskExecutionPanel,
		internalTaskOutputDownloadUrl,
		listInternalTasks,
		openInternalTaskOutputFile,
		revealInternalTaskOutputFile,
		retryInternalTaskSynthesis,
		type InternalExecutionDetails as ExecutionWithDetails,
		type InternalTaskDetails as TaskDetails,
		type InternalTaskListItem,
		type InternalTaskPagination as Pagination,
		type InternalTaskRefs as TaskRefs,
		type InternalTaskSortField as SortField,
		type InternalTaskSortOrder as SortOrder,
		type PersistedInternalTaskOutput as PersistedOutput
	} from '$lib/internalTasks/api';
	import type { ExecutionPanelState } from '$lib/types/executionPanel';

	interface MessagePopover {
		title: string;
		text: string;
		tone: 'summary' | 'error' | 'description';
		style: string;
	}

	let tasks: InternalTaskListItem[] = [];
	let pagination: Pagination = { total: 0, limit: 50, offset: 0, has_more: false };
	let isLoading = false;
	let errorMessage: string | null = null;

	let pageSize = 50;
	let pageOffset = 0;
	let sortField: SortField = 'updated_at';
	let sortOrder: SortOrder = 'desc';
	let filterAgent = '';
	let filterStatus = '';
	let filterQuery = '';
	let pendingFilterQuery = '';

	let detailsByTask: Record<string, TaskDetails> = {};
	let detailsLoadingByTask: Record<string, boolean> = {};
	let detailsErrorByTask: Record<string, string | null> = {};
	/** When each task's details were last read successfully — the panel's staleness age. */
	let detailsLoadedAtByTask: Record<string, number> = {};

	// ── the task panel ───────────────────────────────────────────────────
	/**
	 * Which task the panel is open on. Row click opens the shared panel drawer.
	 */
	let panelTaskId: string | null = null;
	/**
	 * The execution-panel state per (task, run), which is where this surface's
	 * event log comes from. Keyed by run because the log is per-execution: the
	 * reader's pick has to reach the wire, unlike `/details`, which carries every
	 * execution at once.
	 *
	 * A key present with `null` means "read, and the route had no panel state" —
	 * distinct from a key absent, which means "not read yet". The Run act renders
	 * "nothing observed" for both, but only the first is an answer.
	 */
	let panelStateByRun: Record<string, ExecutionPanelState | null> = {};

	function panelStateKey(taskId: string, executionId: string | null): string {
		return `${taskId}::${executionId ?? 'current'}`;
	}

	/** One snapshot of everything the panel's tick reads. */
	type InternalPanelSnapshot = {
		details: TaskDetails;
		panel: ExecutionPanelState | null;
	};
	/**
	 * Which of the task's runs the reader asked to read, or `null` for the one the
	 * row points at.
	 *
	 * **No request follows it here**, and that is the one real difference between
	 * this surface and `/tasks`: `/details` already carries every execution, so
	 * switching runs re-derives the model from a payload that is already in hand.
	 * The adapter takes the same argument on both routes and neither knows which
	 * one it is on — the request is the caller's business, and this caller has none
	 * to make.
	 *
	 * The task id beside it does the same job it does everywhere else on this
	 * surface: a run id belongs to one task, and read against another it would name
	 * an execution that task has never had.
	 */
	let panelRunSelectionId: string | null = null;
	let panelRunSelectionTaskId: string | null = null;
	/**
	 * The panel's clock, advanced once a minute — slower than anything polls.
	 * The verdict is a live region and two of its headlines carry a duration
	 * measured against this, so every advance re-announces the whole verdict;
	 * design §3 puts the fix on whoever owns the clock, and a minute is the
	 * resolution those lines are written at.
	 */
	let panelNow = Date.now();
	let panelClockHandle: ReturnType<typeof setInterval> | null = null;
	const PANEL_CLOCK_INTERVAL_MS = 60_000;

	let deletingTaskId: string | null = null;
	// Mass selection: id -> selected. Whole-object replacement on every
	// change because legacy-mode reactivity needs the assignment.
	let selectedIds: Record<string, boolean> = {};
	let bulkDeleting = false;
	let bulkProgress: { done: number; total: number } | null = null;
	let bulkFailures: Array<{ id: string; reason: string }> = [];
	let stoppingTaskId: string | null = null;
	let retryingTaskId: string | null = null;
	let execActiveTab: Record<string, 'outputs' | 'artifacts'> = {};
	let hoverTask: InternalTaskListItem | null = null;
	let hoverCardStyle = '';
	let hoverHideTimer: ReturnType<typeof setTimeout> | null = null;
	let messagePopover: MessagePopover | null = null;
	let appliedSelectedTaskId: string | null = null;

	$: scope = browser ? $scopeIdentityStore : { principal: 'anonymous', workspace: 'default' };
	$: taskPageCount = Math.max(1, Math.ceil(pagination.total / Math.max(1, pageSize)));
	$: taskCurrentPage = pagination.total === 0 ? 1 : Math.floor(pageOffset / Math.max(1, pageSize)) + 1;
	$: taskPageStart = pagination.total === 0 ? 0 : pageOffset + 1;
	$: taskPageEnd = pagination.total === 0 ? 0 : Math.min(pagination.total, pageOffset + tasks.length);
	$: selectedTaskIdFromQuery = browser ? ($page.url.searchParams.get('selected') || '').trim() : '';
	$: if (browser && selectedTaskIdFromQuery && selectedTaskIdFromQuery !== appliedSelectedTaskId) {
		appliedSelectedTaskId = selectedTaskIdFromQuery;
		void expandSelectedTaskFromRoute(selectedTaskIdFromQuery);
	}
	$: if (browser && !selectedTaskIdFromQuery && appliedSelectedTaskId) {
		appliedSelectedTaskId = null;
	}

	// ── what the panel renders ───────────────────────────────────────────
	// The row the panel is open on, or `null` once it has left the listed page.
	$: panelTask = panelTaskId === null ? null : (tasks.find((t) => t.id === panelTaskId) ?? null);
	// Keyed on the open task's id, so a details payload belonging to a task the
	// reader has left can never be read under this one.
	$: panelDetails = panelTaskId === null ? null : (detailsByTask[panelTaskId] ?? null);
	/**
	 * Where a panel output row's bytes are served. `null` with no open task,
	 * which is the shape the model reads as "no URL for this file" — so the
	 * thumbnail, the download and the open-in-tab are absent rather than pointing
	 * at a task-less address.
	 */
	$: panelOutputUrlFor = (path: string): string | null =>
		panelTaskId === null
			? null
			: taskOutputUrl(panelTaskId, path, scope.principal, scope.workspace);
	// Keyed on the open task's id for the reason the details payload above is: a
	// run chosen on one task must not be read under another.
	$: panelPollExecutionId =
		panelRunSelectionTaskId === panelTaskId ? panelRunSelectionId : null;
	$: panelExecutionPanel =
		panelTaskId === null
			? null
			: (panelStateByRun[panelStateKey(panelTaskId, panelPollExecutionId)] ?? null);
	$: panelModel =
		panelTask === null
			? null
			: toInternalTaskPanelModel(
					panelTask,
					panelDetails,
					panelNow,
					panelOutputUrlFor,
					panelPollExecutionId,
					panelExecutionPanel
				);
	/**
	 * The contents of the output row the reader expanded, or `null` when none is
	 * open. Unlike the details above it needs no per-task key: the panel drops its
	 * open row when the task id changes, and re-opening one writes a `loading`
	 * record here before the render that could have shown the previous file.
	 */
	let panelPreview: TaskFilePreview | null = null;
	let panelPreviewRequestId = 0;
	let panelPreferredAct: ActId | null = null;

	function allExecutionOutputs(details: TaskDetails): PersistedOutput[] {
		if (!Array.isArray(details.executions)) return [];
		return details.executions.flatMap((e) => executionOutputs(e as unknown as ExecutionWithDetails));
	}

	function allExecutionArtifacts(details: TaskDetails): unknown[] {
		if (!Array.isArray(details.executions)) return [];
		return details.executions.flatMap((e) => (Array.isArray(e.artifacts) ? e.artifacts : []));
	}

	function internalTaskHasResult(
		task: InternalTaskListItem,
		detailsMap: Record<string, TaskDetails> = detailsByTask
	): boolean {
		if (task.completion_artifact_names && task.completion_artifact_names.length > 0) {
			return true;
		}
		if (task.completion_summary?.trim()) return true;
		const details = detailsMap[task.id];
		if (details) {
			if (taskOutputs(details).length > 0) return true;
			if (allExecutionOutputs(details).length > 0) return true;
			if (allExecutionArtifacts(details).length > 0) return true;
			const taskSummary = (details.task as { completion_summary?: unknown })?.completion_summary;
			if (typeof taskSummary === 'string' && taskSummary.trim().length > 0) return true;
		}
		return false;
	}

	function isRecurringTask(
		task: InternalTaskListItem,
		detailsMap: Record<string, TaskDetails> = detailsByTask
	): boolean {
		if (task.tags?.some((t) => t.id === 'app_recurring' || t.name === 'app_recurring' || t.name?.toLowerCase().includes('recurring'))) {
			return true;
		}
		if (task.schedule) return true;
		const details = detailsMap[task.id];
		if (details?.recurring_schedule) return true;
		return false;
	}
	$: panelLoadError = panelTaskId === null ? null : (detailsErrorByTask[panelTaskId] ?? null);
	$: panelLastLoadedAt = panelTaskId === null ? null : (detailsLoadedAtByTask[panelTaskId] ?? null);

	async function prefetchVisibleTaskDetails(taskList: InternalTaskListItem[]): Promise<void> {
		if (!browser) return;
		const needed = taskList.filter(
			(t) =>
				t.status === 'completed' &&
				!t.completion_summary?.trim() &&
				(!t.completion_artifact_names || t.completion_artifact_names.length === 0) &&
				!detailsByTask[t.id] &&
				!detailsLoadingByTask[t.id]
		);
		if (needed.length === 0) return;
		const BATCH_SIZE = 5;
		for (let i = 0; i < needed.length; i += BATCH_SIZE) {
			const batch = needed.slice(i, i + BATCH_SIZE);
			await Promise.allSettled(batch.map((t) => loadTaskDetails(t.id)));
		}
	}

	async function loadTasks(): Promise<void> {
		if (!browser) return;
		isLoading = true;
		errorMessage = null;
		try {
			const body = await listInternalTasks({
				principal: scope.principal,
				workspace: scope.workspace,
				limit: pageSize,
				offset: pageOffset,
				sort: sortField,
				order: sortOrder,
				agent: filterAgent,
				status: filterStatus,
				query: filterQuery
			});
			tasks = body.tasks;
			pagination = body.pagination;
			void prefetchVisibleTaskDetails(tasks);
		} catch (err) {
			tasks = [];
			errorMessage = err instanceof Error ? err.message : String(err);
		} finally {
			isLoading = false;
		}
	}

	async function expandSelectedTaskFromRoute(taskId: string): Promise<void> {
		if (!browser) return;
		const selected = taskId.trim();
		if (!selected) return;
		if (!tasks.some((task) => task.id === selected)) {
			pendingFilterQuery = selected;
			filterQuery = selected;
			pageOffset = 0;
			await tick();
			await loadTasks();
		}
		if (!tasks.some((task) => task.id === selected)) {
			errorMessage = `Internal task ${selected} was not found in this scope.`;
			return;
		}
		// A deep link names one task, which is the same request the panel answers
		// on `/tasks` — so it opens the panel.
		panelTaskId = selected;
		if (!detailsByTask[selected] && !detailsLoadingByTask[selected]) {
			await loadTaskDetails(selected);
		}
	}

	async function loadTaskDetails(taskId: string): Promise<void> {
		if (!browser) return;
		detailsLoadingByTask = { ...detailsLoadingByTask, [taskId]: true };
		detailsErrorByTask = { ...detailsErrorByTask, [taskId]: null };
		try {
			const { principal, workspace } = getCurrentScopeIdentity();
			const body = await fetchInternalTaskDetails(taskId, principal, workspace);
			detailsByTask = { ...detailsByTask, [taskId]: body };
			detailsLoadedAtByTask = { ...detailsLoadedAtByTask, [taskId]: Date.now() };
		} catch (err) {
			detailsErrorByTask = {
				...detailsErrorByTask,
				[taskId]: err instanceof Error ? err.message : String(err)
			};
		} finally {
			detailsLoadingByTask = { ...detailsLoadingByTask, [taskId]: false };
		}
	}

	async function loadMoreExecutions(taskId: string): Promise<void> {
		const previous = detailsByTask[taskId];
		if (!previous?.next_execution_cursor || detailsLoadingByTask[taskId]) return;
		detailsLoadingByTask = { ...detailsLoadingByTask, [taskId]: true };
		try {
			const { principal, workspace } = getCurrentScopeIdentity();
			const page = await fetchInternalTaskDetails(taskId, principal, workspace, previous.next_execution_cursor);
			const current = detailsByTask[taskId] ?? previous;
			const seen = new Set(current.executions.map(executionId));
			detailsByTask = { ...detailsByTask, [taskId]: { ...page,
				executions: [...current.executions, ...page.executions.filter(run => !seen.has(executionId(run)))] } };
		} catch (error) {
			detailsErrorByTask = { ...detailsErrorByTask, [taskId]: String(error) };
		} finally {
			detailsLoadingByTask = { ...detailsLoadingByTask, [taskId]: false };
		}
	}

	function mergeExecutionRefresh(taskId: string, fresh: TaskDetails): TaskDetails {
		const previous = detailsByTask[taskId];
		if (!previous || previous.executions.length <= fresh.executions.length) return fresh;
		const seen = new Set(fresh.executions.map(executionId));
		// If a whole page arrived while this view was idle, retain the fresh
		// cursor so the intervening runs remain reachable by Load older.
		if (!previous.executions.some(run => seen.has(executionId(run)))) return fresh;
		return { ...fresh, next_execution_cursor: previous.next_execution_cursor,
			executions: [...fresh.executions, ...previous.executions.filter(run => !seen.has(executionId(run)))] };
	}

	async function deleteTask(taskId: string): Promise<void> {
		if (!browser) return;
		const confirmed = window.confirm(
			`Delete task ${taskId}? This permanently removes the task directory.`
		);
		if (!confirmed) return;
		deletingTaskId = taskId;
		try {
			const { principal, workspace } = getCurrentScopeIdentity();
			await deleteInternalTask(taskId, principal, workspace);
			tasks = tasks.filter((t) => t.id !== taskId);
			if (panelTaskId === taskId) panelTaskId = null;
			delete detailsByTask[taskId];
			detailsByTask = { ...detailsByTask };
			// The shared removal policy. Re-reading the same offset was only half
			// of it: delete the only row of the last page and that offset comes
			// back with nothing, leaving an empty table under a pager still
			// counting the page it is on — `Page 2 of 2 · 51-50 of 50`, Next
			// disabled and Previous alive. `pageAfterRemoval` steps back instead.
			await pageAfterRemoval(
				Math.floor(pageOffset / Math.max(1, pageSize)) + 1,
				async (page) => {
					pageOffset = (page - 1) * pageSize;
					await loadTasks();
					// A failed read is not evidence that the page ended — leave the
					// reader where they are and let them retry.
					return errorMessage === null ? tasks.length : 1;
				}
			);
		} catch (err) {
			errorMessage = err instanceof Error ? err.message : String(err);
		} finally {
			deletingTaskId = null;
		}
	}

	$: selectedCount = Object.values(selectedIds).filter(Boolean).length;
	// App-run rows are managed by the Apps lifecycle (their delete is denied
	// server-side), so they are not selectable for bulk delete.
	$: selectableTasks = tasks.filter((task) => !isAppRun(task));
	$: selectableVisibleIds = selectableTasks.map((task) => task.id);
	$: allVisibleSelected =
		selectableVisibleIds.length > 0 &&
		selectableVisibleIds.every((id) => selectedIds[id]);

	function toggleSelected(taskId: string): void {
		selectedIds = { ...selectedIds, [taskId]: !selectedIds[taskId] };
	}

	function toggleSelectAllVisible(): void {
		const next = { ...selectedIds };
		if (allVisibleSelected) {
			for (const id of selectableVisibleIds) next[id] = false;
		} else {
			for (const id of selectableVisibleIds) next[id] = true;
		}
		selectedIds = next;
	}

	function clearSelection(): void {
		selectedIds = {};
		bulkFailures = [];
	}

	async function deleteSelected(): Promise<void> {
		if (!browser || bulkDeleting) return;
		const ids = Object.entries(selectedIds)
			.filter(([, selected]) => selected)
			.map(([id]) => id);
		if (ids.length === 0) return;
		const confirmed = window.confirm(
			`Delete ${ids.length} internal task${ids.length === 1 ? '' : 's'}? ` +
				'This permanently removes each task directory.'
		);
		if (!confirmed) return;
		bulkDeleting = true;
		bulkProgress = { done: 0, total: ids.length };
		bulkFailures = [];
		const { principal, workspace } = getCurrentScopeIdentity();
		const succeeded: string[] = [];
		for (const taskId of ids) {
			try {
				await deleteInternalTask(taskId, principal, workspace);
				succeeded.push(taskId);
			} catch (err) {
				// One denial must not abort the batch — collect and continue
				// so a single app-workflow-bound task can't block cleanup.
				bulkFailures = [
					...bulkFailures,
					{ id: taskId, reason: err instanceof Error ? err.message : String(err) }
				];
			}
			bulkProgress = { done: bulkProgress.done + 1, total: ids.length };
		}
		if (succeeded.length > 0) {
			tasks = tasks.filter((task) => !succeeded.includes(task.id));
			if (panelTaskId && succeeded.includes(panelTaskId)) panelTaskId = null;
			detailsByTask = Object.fromEntries(
				Object.entries(detailsByTask).filter(([id]) => !succeeded.includes(id))
			);
			const nextSelected = { ...selectedIds };
			for (const id of succeeded) nextSelected[id] = false;
			selectedIds = nextSelected;
			// Same last-page policy as the single delete: deleting the only
			// rows of the last page must step the offset back, not strand an
			// empty page under a pager that still counts it.
			await pageAfterRemoval(
				Math.floor(pageOffset / Math.max(1, pageSize)) + 1,
				async (page) => {
					pageOffset = (page - 1) * pageSize;
					await loadTasks();
					return errorMessage === null ? tasks.length : 1;
				}
			);
			// One step-back absorbs a single row, not a bulk page wipe:
			// stranded on an empty page with rows still to show → page 1.
			if (tasks.length === 0 && pagination.total > 0) {
				pageOffset = 0;
				await loadTasks();
			}
		}
		bulkDeleting = false;
		bulkProgress = null;
	}

	async function stopTask(task: InternalTaskListItem): Promise<void> {
		if (!browser) return;
		const executionId = task.active_root_execution_id?.trim() ?? '';
		if (!executionId) {
			errorMessage = `Task ${task.id} has no active execution to cancel.`;
			return;
		}
		const confirmed = window.confirm(`Cancel execution ${executionId} for task ${task.id}?`);
		if (!confirmed) return;
		stoppingTaskId = task.id;
		try {
			const { principal, workspace } = getCurrentScopeIdentity();
			await cancelInternalExecution(executionId, principal, workspace);
			await loadTasks();
		} catch (err) {
			errorMessage = err instanceof Error ? err.message : String(err);
		} finally {
			stoppingTaskId = null;
		}
	}

	async function retrySynthesis(task: InternalTaskListItem): Promise<void> {
		if (!browser) return;
		const executionId = task.synthesis_failed_execution_id?.trim() ?? '';
		if (!executionId) {
			errorMessage = `Task ${task.id} has no synthesis_failed_execution_id to retry.`;
			return;
		}
		retryingTaskId = task.id;
		try {
			const { principal, workspace } = getCurrentScopeIdentity();
			// Read the response body to distinguish "we spawned synthesis"
			// from "coalesced with a concurrent retry". `coalesced=true`
			// is success — the OTHER caller's spawn is what produces the
			// result the operator wanted. We surface a soft notice
			// rather than an error.
			const body = await retryInternalTaskSynthesis(
				task.id,
				executionId,
				principal,
				workspace
			);
			if (body?.coalesced) {
				errorMessage =
					'Retry coalesced — a concurrent retry was already in flight. The result will land via that pipeline.';
			}
			// Whether scheduled or coalesced, the backend has updated
			// disk state (synthesis_failed cleared, vec contains exec_id)
			// and emitted projection events. Re-load the list so the
			// pill flips to "synthesizing…" immediately.
			await loadTasks();
		} catch (err) {
			errorMessage = err instanceof Error ? err.message : String(err);
		} finally {
			retryingTaskId = null;
		}
	}

	function isTaskRunning(task: InternalTaskListItem): boolean {
		return ['running', 'planning', 'paused', 'pending'].includes(task.status);
	}

	function controlExecutionId(task: InternalTaskListItem): string {
		return task.active_root_execution_id?.trim() ?? '';
	}

	function compactTitle(task: InternalTaskListItem): string {
		return (task.title || task.description || '(untitled)').replace(/\s+/g, ' ').trim();
	}

	function titleTooltip(task: InternalTaskListItem): string {
		const parts = [
			task.title?.trim() ? `Title:\n${task.title.trim()}` : null,
			task.description?.trim() ? `Description:\n${task.description.trim()}` : null,
			`Task ID: ${task.id}`,
			`Agent: ${task.agent_id || '—'}`,
			`Status: ${task.status || 'unknown'}`
		].filter(Boolean);
		return parts.join('\n\n');
	}

	function showTitleHover(event: MouseEvent | FocusEvent, task: InternalTaskListItem): void {
		if (!browser) return;
		cancelTitleHoverHide();
		const target = event.currentTarget as HTMLElement | null;
		if (!target) return;
		const rect = target.getBoundingClientRect();
		const maxWidth = Math.min(760, window.innerWidth - 32);
		const left = Math.max(16, Math.min(rect.left, window.innerWidth - maxWidth - 16));
		const preferredTop = rect.bottom + 8;
		const top =
			preferredTop + 360 <= window.innerHeight
				? preferredTop
				: Math.max(16, rect.top - 368);
		hoverTask = task;
		hoverCardStyle = `left: ${left}px; top: ${top}px; max-width: ${maxWidth}px;`;
	}

	function cancelTitleHoverHide(): void {
		if (hoverHideTimer) {
			clearTimeout(hoverHideTimer);
			hoverHideTimer = null;
		}
	}

	function scheduleTitleHoverHide(): void {
		cancelTitleHoverHide();
		hoverHideTimer = setTimeout(() => hideTitleHover(), 120);
	}

	function hideTitleHover(): void {
		cancelTitleHoverHide();
		hoverTask = null;
		hoverCardStyle = '';
	}

	function normalizedExecutionMessage(value: unknown): string {
		return typeof value === 'string' ? value.trim() : '';
	}

	function shouldShowMessageDetails(text: string): boolean {
		return text.length > 180 || text.includes('\n');
	}

	function openMessagePopover(
		event: MouseEvent,
		title: string,
		text: string,
		tone: MessagePopover['tone']
	): void {
		if (!browser) return;
		hideTitleHover();
		const target = event.currentTarget as HTMLElement | null;
		if (!target) return;
		const rect = target.getBoundingClientRect();
		const maxWidth = Math.min(860, window.innerWidth - 32);
		const left = Math.max(16, Math.min(rect.left - maxWidth + rect.width, window.innerWidth - maxWidth - 16));
		const preferredTop = rect.bottom + 8;
		const top =
			preferredTop + 420 <= window.innerHeight
				? preferredTop
				: Math.max(16, rect.top - 428);
		messagePopover = {
			title,
			text,
			tone,
			style: `left: ${left}px; top: ${top}px; max-width: ${maxWidth}px;`
		};
	}

	function closeMessagePopover(): void {
		messagePopover = null;
	}

	/**
	 * Open the task panel on a task, loading its details if they are not already
	 * read. Both acts the panel can render for an internal task come from that
	 * payload, so until it lands the panel shows the verdict alone rather than
	 * acts describing a run it has not seen.
	 */
	function openPanel(taskId: string, act: ActId | null = null): void {
		hideTitleHover();
		closeMessagePopover();
		// **No fetch here.** Setting the open task aims the poll, which reads
		// `/details` immediately — for a settled task exactly once, and for a live
		// one on the cadence. A read here as well would be a second writer of
		// `detailsByTask` racing the first for the same payload, and both would be
		// right, which is what makes that kind of duplicate hard to notice.
		panelTaskId = taskId;
		panelPreferredAct = act;
	}

	function closePanel(): void {
		panelTaskId = null;
		panelPreferredAct = null;
		// The closed panel forgets which run was being read, so re-opening the same
		// task lands on its current run rather than on an attempt chosen minutes ago.
		panelRunSelectionId = null;
		panelRunSelectionTaskId = null;
	}

	/**
	 * The reader picked a different run to read.
	 *
	 * No fetch: every execution is already on the `/details` payload this panel is
	 * rendering, so the model simply re-derives. The task id is pinned alongside so
	 * a choice cannot outlive the task it was made about.
	 */
	function handlePanelSelectRun(event: CustomEvent<{ executionId: string }>): void {
		if (panelTaskId === null) return;
		const executionId = event.detail?.executionId?.trim() ?? '';
		if (!executionId) return;
		panelRunSelectionId = executionId;
		panelRunSelectionTaskId = panelTaskId;
	}

	/**
	 * Re-read the open task's row, in place.
	 *
	 * **One row rather than the page**, and for the same reason `/tasks` refreshes
	 * one task rather than calling `loadTasks` again: the verdict over the panel is
	 * read off the *row* — its status, its stall clock, its synthesis state — so it
	 * has to advance with the events under it or the panel reads as broken rather
	 * than as merely behind. Re-listing the page four times a minute would re-sort
	 * the table under the reader and flip the header's Refresh button in and out of
	 * its busy state, neither of which is a thing they asked for.
	 *
	 * The list's own filters are deliberately **not** sent: the query names one
	 * task, and a status filter would hide the row at the exact moment its status
	 * changed — which is the one moment this exists for. The id is checked on the
	 * way back because `query` matches titles too.
	 */
	async function refreshPanelRow(taskId: string): Promise<void> {
		const { principal, workspace } = getCurrentScopeIdentity();
		const body = await listInternalTasks({
			principal,
			workspace,
			limit: 1,
			offset: 0,
			sort: sortField,
			order: sortOrder,
			query: taskId
		});
		const fresh = body.tasks.find((row) => row.id === taskId);
		if (!fresh) return;
		tasks = tasks.map((row) => (row.id === fresh.id ? fresh : row));
	}

	/**
	 * The panel's own poll: the open task's row, and the `/details` payload both of
	 * its acts are derived from, on one clock.
	 *
	 * The same module `/tasks` uses, with the same cadence rule — a finished task
	 * is not polled, a running one is polled fast — so neither surface can decide
	 * on its own what "live" means.
	 *
	 * **A read is now two payloads: `/details` and the execution panel.** They ride
	 * one tick deliberately. `/details` carries the steps, outputs and verdict but
	 * no event log at all; the panel route carries the log, and it resolves internal
	 * task ids because `workspace.task_dir` probes `internal_tasks/` first. Two
	 * clocks would let the run's events and the run's status disagree on screen,
	 * which is the one thing a live feed must not do.
	 *
	 * **The reader's chosen run IS part of the panel request**, unlike `/details`,
	 * which carries every execution and re-derives from a payload already in hand.
	 * The log is per-execution, so the choice has to reach the wire — see
	 * `panelPollExecutionId`.
	 */
	const panelPoll = createTaskPanelPoll<InternalPanelSnapshot>({
		read: async (target: PanelPollTarget) => {
			const { principal, workspace } = getCurrentScopeIdentity();
			const [, details, panel] = await Promise.all([
				// Not allowed to fail the tick: the row is the *list's*, the list has
				// its own error line, and losing the details refresh over it would cost
				// the panel the half this poll exists for.
				refreshPanelRow(target.taskId).catch(() => undefined),
				fetchInternalTaskDetails(target.taskId, principal, workspace),
				// Additive, and never allowed to fail the tick either: without a log
				// the Run act says "nothing observed", which is exactly what this
				// surface said before it read one. Losing the steps and outputs over a
				// missing event log would be the worse trade.
				fetchInternalTaskExecutionPanel(target.taskId, target.executionId).catch(
					() => null
				)
			]);
			return { details, panel };
		},
		onSnapshot: (target: PanelPollTarget, snapshot: InternalPanelSnapshot, at: number) => {
			if (target.taskId !== panelTaskId) return;
			detailsByTask = { ...detailsByTask, [target.taskId]: mergeExecutionRefresh(target.taskId, snapshot.details) };
			detailsLoadedAtByTask = { ...detailsLoadedAtByTask, [target.taskId]: at };
			detailsErrorByTask = { ...detailsErrorByTask, [target.taskId]: null };
			panelStateByRun = {
				...panelStateByRun,
				[panelStateKey(target.taskId, target.executionId)]: snapshot.panel
			};
		},
		onFailure: (target: PanelPollTarget, message: string) => {
			if (target.taskId !== panelTaskId) return;
			// Design §6, case 3. The details already read are kept and labelled — a
			// failed refresh is not evidence that the task changed — and
			// `detailsLoadedAtByTask` is left where it was so the line can say how old
			// what is on screen actually is.
			detailsErrorByTask = { ...detailsErrorByTask, [target.taskId]: message };
		}
	});

	$: panelPoll.aim(
		browser && panelTaskId !== null
			? { taskId: panelTaskId, executionId: panelPollExecutionId }
			: null,
		panelPollCadence(panelTask === null ? null : normalizeV3TaskStatus(panelTask.status))
	);

	/**
	 * The panel's Retry control — design §6's load failure, which on this surface
	 * is the details request. The list is refreshed too: a panel with no task at
	 * all is one whose row has left the listed page, and re-reading the details
	 * alone would not bring it back.
	 */
	async function retryPanelLoad(): Promise<void> {
		const taskId = panelTaskId;
		if (!taskId) return;
		await loadTasks();
		await loadTaskDetails(taskId);
	}

	onDestroy(() => {
		// The poll is parked by aiming it at `null` whenever the panel closes; this
		// is the case that gesture cannot cover — the page going away with the panel
		// still open.
		panelPoll.stop();
	});

	async function handlePanelOpenFile(
		event: CustomEvent<{ file: PanelOutputFile; index: number }>
	): Promise<void> {
		const path = event.detail.file.path;
		if (!panelTaskId || !path) return;
		await openOutputFile(panelTaskId, path);
	}

	async function handlePanelRevealFile(
		event: CustomEvent<{ file: PanelOutputFile; index: number }>
	): Promise<void> {
		const path = event.detail.file.path;
		if (!panelTaskId || !path) return;
		await revealOutputFile(panelTaskId, path);
	}

	/**
	 * Read one output file, because the reader expanded its row.
	 *
	 * **The same loader `/tasks` uses**, over the same GET route, because both
	 * surfaces mint the same address through `taskOutputUrl` — so what a preview
	 * refuses, how large is too large, and what a failure reads as cannot come out
	 * differently on the two routes.
	 *
	 * The `loading` record is written before the await, so the row says
	 * `Reading …` rather than holding the previous file's contents for a round
	 * trip. The request id is what stops a slow reply landing under another row.
	 */
	async function handlePanelPreviewFile(
		event: CustomEvent<{ file: PanelOutputFile; index: number }>
	): Promise<void> {
		const { file, index } = event.detail;
		const requestId = ++panelPreviewRequestId;
		panelPreview = { index, status: 'loading', text: null, detail: null };
		const preview = await loadOutputPreview(index, file);
		if (requestId !== panelPreviewRequestId) return;
		panelPreview = preview;
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Escape') return;
		if (messagePopover) {
			closeMessagePopover();
		}
	}

	/**
	 * Escape dismisses the innermost thing first: a message popover sits over the
	 * drawer, so it goes before the drawer does. Closing both on one keypress
	 * would lose the reader's place to a keystroke they meant for the popover.
	 *
	 * The drawer does the closing; this only says whether the keystroke is its to
	 * take. Which layers exist is this workspace's knowledge — every surface that
	 * opens the panel has a different set — so the ordering stays here while the
	 * closing lives once, in the shell.
	 */
	$: escapeBelongsToPanel = messagePopover === null;

	/**
	 * Match the normal task surface: clicking the informational part of a row
	 * opens the shared task panel. Native controls keep their own behavior — the
	 * disclosure, retry, execution controls and destructive actions must never
	 * also open a drawer merely because they live inside the row.
	 */
	function handleTaskRowClick(event: MouseEvent, taskId: string): void {
		const target = event.target instanceof Element ? event.target : null;
		if (target?.closest('button, a, input, select, textarea, [role="button"], [role="link"]')) {
			return;
		}
		openPanel(taskId);
	}

	// Selection is a property of the list on screen: any re-derivation
	// (sort, filter, page, page size) drops it, because a remembered id can
	// outlive its row — invisible deletes are how a bulk action goes wrong.
	function setSort(field: SortField): void {
		clearSelection();
		if (sortField === field) {
			sortOrder = sortOrder === 'asc' ? 'desc' : 'asc';
		} else {
			sortField = field;
			sortOrder = 'desc';
		}
		pageOffset = 0;
		void loadTasks();
	}

	function applyFilters(): void {
		clearSelection();
		filterQuery = pendingFilterQuery;
		pageOffset = 0;
		void loadTasks();
	}

	function resetFilters(): void {
		clearSelection();
		filterAgent = '';
		filterStatus = '';
		filterQuery = '';
		pendingFilterQuery = '';
		pageOffset = 0;
		void loadTasks();
	}

	function gotoTaskPage(pageNumber: number): void {
		clearSelection();
		const safePage = Math.min(taskPageCount, Math.max(1, Math.floor(pageNumber)));
		const nextOffset = (safePage - 1) * pageSize;
		if (nextOffset === pageOffset) return;
		pageOffset = nextOffset;
		void loadTasks();
	}

	function setPageSize(newSize: number): void {
		clearSelection();
		pageSize = newSize;
		pageOffset = 0;
		void loadTasks();
	}

	function formatTimestamp(value: string | null | undefined): string {
		if (!value) return '—';
		try {
			const d = new Date(value);
			if (Number.isNaN(d.getTime())) return value;
			return d.toLocaleString();
		} catch {
			return value;
		}
	}

	// Lifecycle collapsed to two buckets: `persistent` (user-visible) and
	// `internal` (chat/runtime-spawned). The internal task workspace still
	// finds it useful to flag WHERE an internal task came from, so derive
	// a badge from origin markers rather than a dedicated lifecycle:
	// `__system__` created_by → debug-page run; a chat_session_id →
	// chat-spawned transient; otherwise a plain internal/runtime task.
	// Legacy wire values (`ephemeral_owned_by_chat`/`internal_debug`) are
	// still treated as internal in case an un-re-serialized row arrives.
	function isAppRun(task: InternalTaskListItem): boolean {
		return /^task_app_[0-9a-f]{64}$/.test(task.id);
	}

	function lifecycleBadge(task: InternalTaskListItem): {
		label: string;
		kind: 'persistent' | 'internal' | 'chat' | 'debug';
	} {
		if (isAppRun(task)) return { label: 'app', kind: 'internal' };
		const isInternal =
			task.lifecycle === 'internal' ||
			task.lifecycle === 'ephemeral_owned_by_chat' ||
			task.lifecycle === 'internal_debug';
		if (!isInternal) return { label: 'persistent', kind: 'persistent' };
		if (task.created_by === '__system__') return { label: 'debug', kind: 'debug' };
		if (task.chat_session_id) return { label: 'chat', kind: 'chat' };
		return { label: 'internal', kind: 'internal' };
	}

	async function openOutputFile(taskId: string, relativePath: string): Promise<void> {
		if (!relativePath) return;
		const { principal, workspace } = getCurrentScopeIdentity();
		try {
			await openInternalTaskOutputFile(taskId, relativePath, principal, workspace);
		} catch (err) {
			errorMessage = err instanceof Error ? err.message : String(err);
		}
	}

	async function revealOutputFile(taskId: string, relativePath: string): Promise<void> {
		if (!relativePath) return;
		const { principal, workspace } = getCurrentScopeIdentity();
		try {
			await revealInternalTaskOutputFile(taskId, relativePath, principal, workspace);
		} catch (err) {
			errorMessage = err instanceof Error ? err.message : String(err);
		}
	}

	function outputDownloadUrl(taskId: string, relativePath: string): string {
		const { principal, workspace } = getCurrentScopeIdentity();
		return internalTaskOutputDownloadUrl(taskId, relativePath, principal, workspace);
	}

	async function viewOutputFile(taskId: string, relativePath: string): Promise<void> {
		try {
			await openAuthenticatedTaskOutput(outputDownloadUrl(taskId, relativePath));
		} catch (err) {
			errorMessage = err instanceof Error ? err.message : String(err);
		}
	}

	function formatBytes(n: number | undefined): string {
		if (!n || n <= 0) return '—';
		if (n < 1024) return `${n} B`;
		if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
		return `${(n / (1024 * 1024)).toFixed(1)} MB`;
	}

	function sortIndicator(field: SortField): string {
		if (sortField !== field) return '';
		return sortOrder === 'asc' ? ' ▲' : ' ▼';
	}

	function executionId(execution: ExecutionWithDetails): string {
		return (execution.state?.execution_id as string) ?? '—';
	}

	function executionStatus(execution: ExecutionWithDetails): string {
		return (execution.state?.status as string) ?? 'unknown';
	}

	function executionOutputs(execution: ExecutionWithDetails): PersistedOutput[] {
		// Backend canonical field is `output_refs`; older debug payloads
		// used `outputs`. Concatenate so both shapes render and so the
		// child-delegated outputs surface too.
		const direct = Array.isArray(execution.refs?.output_refs)
			? execution.refs!.output_refs!
			: Array.isArray(execution.refs?.outputs)
				? execution.refs!.outputs!
				: [];
		const child = Array.isArray(execution.refs?.child_output_refs)
			? execution.refs!.child_output_refs!
			: [];
		return [...direct, ...child];
	}

	function taskOutputs(details: TaskDetails): PersistedOutput[] {
		const refs = details.task?.refs as TaskRefs | undefined;
		return Array.isArray(refs?.outputs) ? refs.outputs : [];
	}

	function outputKey(output: PersistedOutput, index: number): string {
		return (
			output.output_id
			?? output.id
			?? output.relative_path
			?? output.artifact_path
			?? `output-${index}`
		);
	}

	function outputLabel(output: PersistedOutput): string {
		return (
			output.relative_path
			?? output.artifact_path
			?? output.output_id
			?? output.id
			?? '(unnamed output)'
		);
	}

	function outputKind(output: PersistedOutput): string {
		return output.role ?? output.class ?? output.audience ?? '—';
	}

	function outputFormat(output: PersistedOutput): string {
		return output.media_type ?? output.format ?? '—';
	}

	function executionArtifacts(execution: ExecutionWithDetails): Record<string, unknown>[] {
		return Array.isArray(execution.artifacts) ? execution.artifacts : [];
	}

	function artifactLabel(artifact: Record<string, unknown>): string {
		const name =
			(artifact.name as string)
			?? (artifact.artifact_id as string)
			?? (artifact.path as string)
			?? '(unnamed artifact)';
		return name;
	}

	onMount(() => {
		panelNow = Date.now();
		panelClockHandle = setInterval(() => (panelNow = Date.now()), PANEL_CLOCK_INTERVAL_MS);

		const selected = selectedTaskIdFromQuery;
		if (selected) {
			if (appliedSelectedTaskId !== selected) {
				appliedSelectedTaskId = selected;
				void expandSelectedTaskFromRoute(selected);
			}
			return;
		}
		void loadTasks();
	});

	onDestroy(() => {
		if (panelClockHandle !== null) {
			clearInterval(panelClockHandle);
			panelClockHandle = null;
		}
	});
</script>

<svelte:window on:keydown={handleWindowKeydown} />

<section class="page presto-gaui-page">
	<header class="page-header">
		<div>
			<h1>Internal Tasks</h1>
			<p class="subtitle">
				Non-user-visible task records (chat-inline delegations, system seeds, agent-created
				tasks). Scope:
				<code>{scope.principal}</code> / <code>{scope.workspace}</code>
			</p>
		</div>
		<button
			type="button"
			class="primary"
			on:click={() => void loadTasks()}
			disabled={isLoading}
		>
			{isLoading ? 'Refreshing…' : 'Refresh'}
		</button>
	</header>

	<div class="controls">
		<div class="control-group">
			<label>
				Agent
				<input
					type="text"
					placeholder="e.g. simple-data-analyst"
					bind:value={filterAgent}
					on:change={() => {
						pageOffset = 0;
						void loadTasks();
					}}
				/>
			</label>
			<label>
				Status
				<select
					bind:value={filterStatus}
					on:change={() => {
						pageOffset = 0;
						void loadTasks();
					}}
				>
					<option value="">Any</option>
					<option value="pending">pending</option>
					<option value="planning">planning</option>
					<option value="running">running</option>
					<option value="completed">completed</option>
					<option value="failed">failed</option>
					<option value="paused">paused</option>
				</select>
			</label>
			<label class="grow">
				Search
				<input
					type="text"
					placeholder="title, id, agent, status…"
					bind:value={pendingFilterQuery}
					on:keydown={(event) => {
						if (event.key === 'Enter') applyFilters();
					}}
				/>
			</label>
			<button type="button" on:click={applyFilters}>Apply</button>
			<button type="button" class="ghost" on:click={resetFilters}>Reset</button>
		</div>

		<div class="control-group control-group--trailing">
			<div class="internal-tasks-size">
				<Select
					label="Page size"
					value={String(pageSize)}
					options={[25, 50, 100, 250].map((size) => ({
						value: String(size),
						label: String(size)
					}))}
					interactive={true}
					on:change={(event) => setPageSize(parseInt(event.detail.value, 10))}
				/>
			</div>
			{#if pagination.total > 0}
				<div class="internal-tasks-pager">
					<ServerPager
						currentPage={taskCurrentPage}
						pageCount={taskPageCount}
						startItem={taskPageStart}
						endItem={taskPageEnd}
						totalItems={pagination.total}
						loading={isLoading}
						ariaLabel="Internal tasks pages, above the table"
						on:pagechange={(event) => gotoTaskPage(event.detail.page)}
					/>
				</div>
			{/if}
		</div>
	</div>

	{#if errorMessage}
		<div class="error">{errorMessage}</div>
	{/if}

	{#if selectedCount > 0}
		<div class="bulk-bar" role="toolbar" aria-label="Bulk internal task actions">
			<span class="bulk-count">
				{selectedCount} selected{bulkProgress ? ` · deleting ${bulkProgress.done}/${bulkProgress.total}…` : ''}
			</span>
			<button
				type="button"
				class="danger"
				on:click={() => void deleteSelected()}
				disabled={bulkDeleting}
			>
				{bulkDeleting ? 'Deleting…' : `Delete ${selectedCount} selected`}
			</button>
			<button type="button" on:click={clearSelection} disabled={bulkDeleting}>
				Clear selection
			</button>
			{#if bulkFailures.length > 0}
				<span class="bulk-failures">
					{bulkFailures.length} failed — first: {bulkFailures[0].id}
					({bulkFailures[0].reason})
				</span>
			{/if}
		</div>
	{/if}

	<div class="table-scroll">
		<table class="data-table">
			<thead>
				<tr>
					<th class="select-col">
						<input
							type="checkbox"
							aria-label="Select all internal tasks on this page for bulk delete"
							checked={allVisibleSelected}
							indeterminate={!allVisibleSelected && selectableVisibleIds.some((id) => selectedIds[id])}
							on:change={toggleSelectAllVisible}
							disabled={bulkDeleting || selectableVisibleIds.length === 0}
						/>
					</th>
					<th>
						<button class="sort-btn" on:click={() => setSort('title')}>
							Title{sortIndicator('title')}
						</button>
					</th>
					<th>
						<button class="sort-btn" on:click={() => setSort('agent_id')}>
							Agent{sortIndicator('agent_id')}
						</button>
					</th>
					<th>
						<button class="sort-btn" on:click={() => setSort('status')}>
							Status{sortIndicator('status')}
						</button>
					</th>
					<th>Lifecycle</th>
					<th>
						<button class="sort-btn" on:click={() => setSort('created_at')}>
							Created{sortIndicator('created_at')}
						</button>
					</th>
					<th>
						<button class="sort-btn" on:click={() => setSort('updated_at')}>
							Updated{sortIndicator('updated_at')}
						</button>
					</th>
					<th class="actions-col">Actions</th>
				</tr>
			</thead>
			<tbody>
			{#if tasks.length === 0 && !isLoading}
				<tr>
					<td colspan="8" class="empty">No internal tasks for this scope.</td>
				</tr>
			{/if}
			{#each tasks as task (task.id)}
				{@const badge = lifecycleBadge(task)}
				<!-- svelte-ignore a11y-click-events-have-key-events -->
				<tr
					class="task-row"
					on:click={(event) => handleTaskRowClick(event, task.id)}
				>
					<td class="select-col">
						{#if !isAppRun(task)}
							<input
								type="checkbox"
								aria-label={`Select ${task.id} for bulk delete`}
								checked={Boolean(selectedIds[task.id])}
								on:change={() => toggleSelected(task.id)}
								disabled={bulkDeleting}
							/>
						{/if}
					</td>
					<td class="title-cell">
						<!--
							The title opens the task panel, which is what clicking a task card
							does on `/tasks`.
						-->
						<div class="title-header">
							{#if isRecurringTask(task, detailsByTask)}
								<span class="recurring-icon" title="Recurring task schedule" aria-label="Recurring task">↻</span>
							{/if}
							<button
								type="button"
								class="title title-trigger"
								aria-describedby={hoverTask?.id === task.id ? 'internal-task-title-hover' : undefined}
								on:click={() => openPanel(task.id)}
								on:mouseenter={(event) => showTitleHover(event, task)}
								on:mouseleave={scheduleTitleHoverHide}
								on:focus={(event) => showTitleHover(event, task)}
								on:blur={scheduleTitleHoverHide}
							>{compactTitle(task)}</button>
						</div>
						<div class="task-id" title={task.id}>{task.id}</div>
					</td>
					<td>{task.agent_id || '—'}</td>
					<td>
						<span class="status-pill status-{task.status}">{task.status}</span>
						<!--
							Beside the status word because it is the *reason* for it: a run
							blocked on a diff arrives here as `paused`, which reads identically
							to a run the user paused. This surface is where runtime-spawned
							coding work shows up, so it is the surface most likely to hold one
							of these and the least likely to be watched — the row saying so is
							the only signal a reader gets without opening the panel.
						-->
						{#if task.awaiting_diff_approval}
							<span
								class="attention-pill attention-diff"
								title="Review the file changes before they are applied. Derived from the proposal store on every read, so it survives a restart."
							>
								review changes
							</span>
						{/if}
						{#if task.synthesis_pending}
							<span class="synthesis-pill synthesis-pending" title="Output synthesis is still running in the background. Downstream consumers wait for this to clear.">
								synthesizing…
							</span>
						{/if}
						{#if !isAppRun(task) && task.synthesis_failed_execution_id}
							<button
								type="button"
								class="synthesis-pill synthesis-failed synthesis-failed-button"
								title="Output synthesis exhausted its retries. Click to retry — re-spawns the synthesis pipeline (1.1/1.2/1.3) with fresh LLM round-trips."
								on:click|stopPropagation={() => retrySynthesis(task)}
								disabled={retryingTaskId === task.id}
							>
								{retryingTaskId === task.id ? 'retrying…' : 'synth failed · retry'}
							</button>
						{/if}
					</td>
					<td>
						<span class="lifecycle-pill" class:lifecycle-ephemeral={badge.kind === 'chat'} class:lifecycle-debug={badge.kind === 'debug'}>
							{badge.label}
						</span>
					</td>
					<td>{formatTimestamp(task.created_at)}</td>
					<td>{formatTimestamp(task.updated_at)}</td>
					<td class="actions-col">
						{#if isAppRun(task)}
							<a href="/apps" title="App run controls and retained results">Open Apps</a>
						{:else}
						{#if internalTaskHasResult(task, detailsByTask)}
							<button
								type="button"
								class="result-btn"
								title="View final result in task panel"
								on:click|stopPropagation={() => openPanel(task.id, 'output')}
							>
								Result
							</button>
						{/if}
						{#if isTaskRunning(task)}
							{#if controlExecutionId(task)}
								<ExecutionControls
									executionId={controlExecutionId(task)}
									label={task.title || task.id}
									variant="compact"
									showCancel={false}
									refreshKey={`${task.status}:${task.updated_at}`}
									on:changed={() => void loadTasks()}
								/>
							{/if}
							<button
								type="button"
								class="warn"
								on:click={() => void stopTask(task)}
								disabled={stoppingTaskId === task.id}
							>
								{stoppingTaskId === task.id ? 'Stopping…' : 'Stop'}
							</button>
						{/if}
						<button
							type="button"
							class="danger"
							on:click={() => void deleteTask(task.id)}
							disabled={deletingTaskId === task.id}
						>
							{deletingTaskId === task.id ? 'Deleting…' : 'Delete'}
						</button>
						{/if}
					</td>
				</tr>
			{/each}
			</tbody>
		</table>
	</div>

	{#if pagination.total > 0}
		<div class="internal-tasks-pager internal-tasks-pager--bottom">
			<ServerPager
				currentPage={taskCurrentPage}
				pageCount={taskPageCount}
				startItem={taskPageStart}
				endItem={taskPageEnd}
				totalItems={pagination.total}
				loading={isLoading}
				ariaLabel="Internal tasks pages, below the table"
				on:pagechange={(event) => gotoTaskPage(event.detail.page)}
			/>
		</div>
	{/if}

	{#if panelTaskId !== null}
		<!--
			The drawer chrome is `TaskPanelDrawer`'s, shared with every other surface
			that opens this panel. What is this workspace's is the actions it slots
			into that header — the ones the row behind it can no longer offer while
			this is modal over it.

			**This is where `retryInternalTaskSynthesis` lives**, beside Stop and the
			run controls, for the same reason those are here rather than inside the
			panel: they act on the task, not on anything the panel renders. The
			panel's own job is to say *why* the control is there — the adapter turns
			a synthesis failure into the verdict's ask, so the line above reads
			`Waiting on you` instead of `Finished · Produced no output`.

			It renders outside the row loop, and that is not incidental: inside the
			`{#each}`, switching rows would destroy and recreate the panel — a second
			mechanism resetting the reader's chosen act, beside the identity
			comparison that is supposed to be the only one.
		-->
		<!--
			**The title is the title and the description is the description**, now that
			the header has a row for each. `compactTitle` falls back to the description
			when a task has no title, which was right for a one-row header and would put
			the same sentence on two rows here — so the fallback goes and an untitled
			task simply renders no title row. No `threadId`: internal tasks are not in a
			thread, so the mover is absent rather than defaulted to one.
		-->
		<TaskPanelDrawer
			task={panelModel}
			title={panelTask?.title?.trim() || null}
			description={panelTask?.description?.trim() || null}
			loadError={panelLoadError}
			lastLoadedAt={panelLastLoadedAt}
			now={panelNow}
			outputActions={true}
			filePreviews={true}
			filePreview={panelPreview}
			closeOnEscape={escapeBelongsToPanel}
			preferredAct={panelPreferredAct}
			layer={1400}
			on:close={closePanel}
			on:openFile={handlePanelOpenFile}
			on:revealFile={handlePanelRevealFile}
			on:previewFile={handlePanelPreviewFile}
			on:retry={() => void retryPanelLoad()}
			on:selectRun={handlePanelSelectRun}
		>
			<svelte:fragment slot="actions">
				{#if panelTask}
					{@const openTask = panelTask}
					{#if detailsByTask[openTask.id]?.recurring_schedule}
						{@const schedule = detailsByTask[openTask.id].recurring_schedule!}
						<span>Every {Math.round(schedule.interval_seconds / 60)} minutes after completion · Latest: {schedule.latest_status ?? 'Not started'} · {schedule.waiting_for_settlement ? 'Waiting for this run to settle' : `Next eligible: ${formatTimestamp(schedule.next_due_at ?? undefined)}`}</span>
					{/if}
					{#if detailsByTask[openTask.id]?.next_execution_cursor}
						<button disabled={detailsLoadingByTask[openTask.id]} on:click={() => loadMoreExecutions(openTask.id)}>Load older executions</button>
					{/if}
					{#if isAppRun(openTask)}
						<a href="/apps">Open Apps</a>
					{:else if isTaskRunning(openTask) && controlExecutionId(openTask)}
						<ExecutionControls
							executionId={controlExecutionId(openTask)}
							label={openTask.title || openTask.id}
							variant="compact"
							showCancel={false}
							refreshKey={`${openTask.status}:${openTask.updated_at}`}
							on:changed={() => void loadTasks()}
						/>
						<button
							type="button"
							class="task-panel__action warn"
							on:click={() => void stopTask(openTask)}
							disabled={stoppingTaskId === openTask.id}
						>
							{stoppingTaskId === openTask.id ? 'Stopping…' : 'Stop'}
						</button>
					{/if}
					{#if !isAppRun(openTask) && openTask.synthesis_failed_execution_id}
						<button
							type="button"
							class="task-panel__action"
							on:click={() => void retrySynthesis(openTask)}
							disabled={retryingTaskId === openTask.id}
						>
							{retryingTaskId === openTask.id ? 'Retrying…' : 'Retry synthesis'}
						</button>
					{/if}
				{/if}
			</svelte:fragment>
		</TaskPanelDrawer>
	{/if}

	{#if hoverTask}
		<div
			id="internal-task-title-hover"
			class="title-hover-card"
			role="tooltip"
			style={hoverCardStyle}
			on:mouseenter={cancelTitleHoverHide}
			on:mouseleave={hideTitleHover}
		>
			<div class="title-hover-card__meta">
				<span>{hoverTask.agent_id || 'unknown agent'}</span>
				<span>{hoverTask.status || 'unknown'}</span>
				<span>{lifecycleBadge(hoverTask).label}</span>
			</div>
			<pre>{titleTooltip(hoverTask)}</pre>
		</div>
	{/if}

	{#if messagePopover}
		<div
			class="message-popover-card message-popover-card--{messagePopover.tone}"
			role="tooltip"
			style={messagePopover.style}
		>
			<header>
				<strong>{messagePopover.title}</strong>
				<button
					type="button"
					class="message-popover-card__close"
					aria-label="Close full execution message"
					on:click={closeMessagePopover}
				>×</button>
			</header>
			<pre>{messagePopover.text}</pre>
		</div>
	{/if}
</section>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 16px;
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		color: var(--text-primary, var(--text-color, #1a1a1a));
		font-family:
			ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto,
			'Helvetica Neue', Arial, sans-serif;
	}

	.page-header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 16px;
	}

	.page-header h1 {
		margin: 0 0 4px 0;
		font-size: 22px;
		font-weight: 600;
	}

	.subtitle {
		margin: 0;
		font-size: 13px;
		color: var(--text-muted, var(--text-secondary, #555));
	}

	.subtitle code {
		background: color-mix(in srgb, currentColor 10%, transparent);
		padding: 1px 6px;
		border-radius: 4px;
		font-size: 12px;
	}

	.controls {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.control-group {
		display: flex;
		align-items: flex-end;
		gap: 12px;
		flex-wrap: wrap;
	}

	/* Page size and the pager sit at the trailing edge, matching where the pager
	   below the table already is. `.controls` is a column, so each group is full
	   width and its contents were left-aligned by default — the pager's own
	   `justify-content: flex-end` could not reach past its wrapper, which is exactly
	   its content's width. */
	.control-group--trailing {
		justify-content: flex-end;
		align-items: center;
	}

	/* The same swap as the tasks tab: `native/Select.svelte` rather than a bare
	   `<select>` hand-styled at 13px/6px/radius-6. The two lanes drew three different
	   controls between them, and numbers alone could not have converged them — the
	   component carries per-theme treatments neither copy knew about.

	   Row rather than the component's default column: `.control-group label` stacks
	   its label above its field, which is right for the filter fields beside a table
	   and wrong for a two-word label on a trailing control. */
	.internal-tasks-size :global(.native-select) {
		flex-direction: row;
		align-items: center;
		gap: 8px;
	}

	.internal-tasks-size :global(.native-select__label) {
		white-space: nowrap;
	}

	.control-group label {
		display: flex;
		flex-direction: column;
		gap: 4px;
		font-size: 12px;
		color: var(--text-muted, var(--text-secondary, #555));
		min-width: 140px;
	}

	.control-group label.grow {
		flex: 1;
		min-width: 240px;
	}

	.control-group input,
	.control-group select {
		font-size: 13px;
		padding: 6px 8px;
		border: 1px solid var(--border-soft, color-mix(in srgb, currentColor 25%, transparent));
		border-radius: 6px;
		background: var(--bg-card, var(--input-bg, #fff));
		color: var(--text-primary, inherit);
	}

	.internal-tasks-pager {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		min-height: 31px;
	}

	.internal-tasks-pager--bottom {
		margin-top: -4px;
	}

	button {
		font-size: 13px;
		padding: 6px 12px;
		border-radius: 6px;
		border: 1px solid var(--border-soft, color-mix(in srgb, currentColor 25%, transparent));
		background: color-mix(in srgb, var(--bg-card, var(--input-bg, #fff)) 92%, transparent);
		color: var(--text-primary, inherit);
		cursor: pointer;
		transition:
			background 0.14s ease,
			border-color 0.14s ease,
			color 0.14s ease,
			transform 0.14s ease;
	}

	button:hover:not(:disabled) {
		background: var(--bg-soft, color-mix(in srgb, currentColor 6%, transparent));
		border-color: var(--border-default, color-mix(in srgb, currentColor 32%, transparent));
		transform: translateY(-1px);
	}

	button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	button.primary {
		background: var(--accent-primary, var(--accent, #2563eb));
		color: var(--text-on-accent, #fff);
		border-color: transparent;
	}

	button.primary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent-primary, var(--accent, #2563eb)) 88%, black);
	}

	button.danger {
		color: var(--color-error, var(--danger, #b91c1c));
		border-color: color-mix(in srgb, var(--color-error, var(--danger, #b91c1c)) 35%, transparent);
	}

	button.danger:hover:not(:disabled) {
		background: color-mix(in srgb, var(--color-error, var(--danger, #b91c1c)) 12%, transparent);
	}

	button.warn {
		color: var(--color-warning, var(--warning, #b45309));
		border-color: color-mix(in srgb, var(--color-warning, var(--warning, #b45309)) 35%, transparent);
	}

	button.warn:hover:not(:disabled) {
		background: color-mix(in srgb, var(--color-warning, var(--warning, #b45309)) 12%, transparent);
	}

	button.result-btn {
		color: var(--accent-primary, var(--accent, #2563eb));
		border-color: color-mix(in srgb, var(--accent-primary, var(--accent, #2563eb)) 40%, transparent);
		background: color-mix(in srgb, var(--accent-primary, var(--accent, #2563eb)) 10%, transparent);
		font-weight: 600;
	}

	button.result-btn:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent-primary, var(--accent, #2563eb)) 20%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary, var(--accent, #2563eb)) 65%, transparent);
	}

	button.ghost {
		background: transparent;
	}

	.error {
		padding: 8px 12px;
		background: color-mix(in srgb, var(--color-error, var(--danger, #b91c1c)) 12%, transparent);
		color: var(--color-error, var(--danger, #b91c1c));
		border-radius: 6px;
		font-size: 13px;
	}

	.table-scroll {
		width: 100%;
		max-width: 100%;
		overflow-x: auto;
		border: 1px solid var(--border-soft, color-mix(in srgb, currentColor 12%, transparent));
		border-radius: 10px;
		background: color-mix(in srgb, var(--bg-card, #fff) 88%, transparent);
	}

	.data-table {
		width: 100%;
		min-width: 860px;
		border-collapse: collapse;
		font-size: 13px;
		background: transparent;
	}

	.data-table th,
	.data-table td {
		border-bottom: 1px solid var(--border-soft, color-mix(in srgb, currentColor 12%, transparent));
		padding: 8px 10px;
		text-align: left;
		vertical-align: top;
	}

	.data-table th {
		background: color-mix(in srgb, var(--bg-soft, currentColor) 78%, transparent);
		font-weight: 600;
		font-size: 12px;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted, var(--text-secondary, #555));
	}

	.sort-btn {
		all: unset;
		cursor: pointer;
		font: inherit;
		color: inherit;
	}

	.sort-btn:hover {
		color: var(--accent-primary, var(--accent, #2563eb));
	}

	.select-col {
		width: 2.25rem;
		text-align: center;
	}

	.select-col input {
		appearance: none;
		display: inline-grid;
		place-content: center;
		vertical-align: middle;
		width: 1rem;
		height: 1rem;
		margin: 0;
		border: 1.5px solid var(--border-default, color-mix(in srgb, currentColor 32%, transparent));
		border-radius: 4px;
		background: color-mix(in srgb, var(--bg-card, #fff) 96%, transparent);
		color: var(--text-on-accent, #fff);
		cursor: pointer;
		box-shadow:
			inset 0 1px 0 color-mix(in srgb, #fff 40%, transparent),
			0 1px 2px color-mix(in srgb, #000 10%, transparent);
		transition:
			background 120ms ease,
			border-color 120ms ease,
			box-shadow 120ms ease;
	}

	.select-col input::before {
		content: '';
		width: 0.32rem;
		height: 0.56rem;
		border-right: 2px solid currentColor;
		border-bottom: 2px solid currentColor;
		transform: rotate(42deg) scale(0);
		transform-origin: center;
		transition: transform 120ms ease;
	}

	.select-col input:hover:not(:disabled) {
		border-color: var(--accent-primary, #2563eb);
		box-shadow:
			0 0 0 2px color-mix(in srgb, var(--accent-primary, #2563eb) 16%, transparent);
	}

	.select-col input:checked {
		border-color: var(--accent-primary, #2563eb);
		background: var(--accent-primary, #2563eb);
		box-shadow:
			0 0 0 2px color-mix(in srgb, var(--accent-primary, #2563eb) 18%, transparent);
	}

	.select-col input:checked::before {
		transform: rotate(42deg) scale(1);
	}

	.select-col input:indeterminate {
		border-color: var(--accent-primary, #2563eb);
		background: var(--accent-primary, #2563eb);
		box-shadow:
			0 0 0 2px color-mix(in srgb, var(--accent-primary, #2563eb) 18%, transparent);
	}

	.select-col input:indeterminate::before {
		width: 0.5rem;
		height: 2px;
		background: currentColor;
		border: none;
		transform: scale(1);
	}

	.select-col input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, #2563eb) 55%, transparent);
		outline-offset: 2px;
	}

	.select-col input:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.bulk-bar {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		flex-wrap: wrap;
		padding: 0.5rem 0.75rem;
		margin-bottom: 0.6rem;
		border: 1px solid var(--accent-primary);
		border-radius: 0.45rem;
		background: var(--bg-card);
	}

	.bulk-count {
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--text-primary);
		margin-right: auto;
	}

	.bulk-failures {
		font-size: 0.78rem;
		color: var(--text-danger, #c33);
		max-width: 28rem;
	}

	.actions-col {
		width: 140px;
		text-align: right;
		white-space: nowrap;
	}

	.actions-col button,
	.actions-col a {
		padding: 3px 8px;
		font-size: 11px;
		line-height: 1.2;
		border-radius: 4px;
	}

	.actions-col button + button {
		margin-left: 4px;
	}

	.title-cell {
		min-width: 220px;
		max-width: 380px;
		overflow: hidden;
	}

	.title-header {
		display: flex;
		align-items: flex-start;
		gap: 6px;
	}

	.title {
		font-weight: 600;
		line-height: 1.35;
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		overflow-wrap: anywhere;
	}

	.title-trigger {
		all: unset;
		box-sizing: border-box;
		width: 100%;
		font-weight: 600;
		line-height: 1.35;
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		overflow-wrap: anywhere;
		/* It opens the panel now, so it points rather than offering help. */
		cursor: pointer;
	}

	.title:focus {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, #2563eb) 55%, transparent);
		outline-offset: 2px;
		border-radius: 4px;
	}

	/* ── the drawer's action ladder ───────────────────────────────────────
	   The scrim, the dialog, the header and the body are `TaskPanelDrawer`'s;
	   it is handed `layer={1400}` so it sits above the hover card and the
	   message popover (z-index 1200 / 1300), both triggered from the rows
	   behind it. What stays here is the actions this surface slots into that
	   header, which take their look from the `button` rule above. */
	.task-panel__action {
		flex: none;
	}

	.title-hover-card {
		position: fixed;
		z-index: 1200;
		width: min(760px, calc(100vw - 32px));
		max-height: min(60vh, 420px);
		overflow: auto;
		padding: 12px 14px;
		border: 1px solid var(--border-default, color-mix(in srgb, currentColor 18%, transparent));
		border-radius: 10px;
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 96%, transparent),
				color-mix(in srgb, var(--bg-soft, #f8fafc) 94%, transparent)
			);
		color: var(--text-primary, #111827);
		box-shadow:
			0 18px 44px color-mix(in srgb, #000 18%, transparent),
			0 2px 10px color-mix(in srgb, #000 8%, transparent);
		backdrop-filter: blur(14px);
	}

	.title-hover-card__meta {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		margin-bottom: 10px;
	}

	.title-hover-card__meta span {
		display: inline-flex;
		align-items: center;
		min-height: 22px;
		padding: 2px 8px;
		border-radius: 999px;
		background: color-mix(in srgb, var(--accent-primary, currentColor) 10%, transparent);
		color: var(--text-muted, var(--text-secondary, #555));
		font-size: 11px;
		font-weight: 600;
	}

	.title-hover-card pre {
		margin: 0;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		font:
			12px/1.45 ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono',
			monospace;
	}

	.task-id {
		font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
		font-size: 11px;
		color: var(--text-muted, var(--text-secondary, #888));
		margin-top: 2px;
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.status-pill {
		display: inline-block;
		padding: 2px 8px;
		border-radius: 999px;
		font-size: 11px;
		font-weight: 500;
		background: color-mix(in srgb, currentColor 12%, transparent);
		text-transform: lowercase;
	}

	.recurring-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		width: 16px;
		height: 16px;
		margin-top: 1px;
		border-radius: 3px;
		font-size: 11px;
		font-weight: 700;
		line-height: 1;
		color: var(--accent-primary, #2563eb);
		background: color-mix(in srgb, var(--accent-primary, #2563eb) 14%, transparent);
	}

	/* Deliberately NOT a `.synthesis-pill` variant: that family says "the
	   machine is still busy, wait", and this one says "the machine stopped and
	   is waiting for you". Sharing the class would make the two swap styling by
	   accident the next time the synthesis pills are restyled. */
	.attention-pill {
		display: inline-block;
		margin-left: 6px;
		padding: 2px 8px;
		border-radius: 999px;
		font-size: 10px;
		font-weight: 600;
		text-transform: lowercase;
		vertical-align: middle;
	}

	.attention-pill.attention-diff {
		background: color-mix(in srgb, var(--status-attention, #d97706) 20%, transparent);
		color: color-mix(in srgb, var(--status-attention, #d97706) 80%, currentColor);
	}

	.synthesis-pill {
		display: inline-block;
		margin-left: 6px;
		padding: 2px 8px;
		border-radius: 999px;
		font-size: 10px;
		font-weight: 500;
		text-transform: lowercase;
		vertical-align: middle;
	}

	.synthesis-pill.synthesis-pending {
		background: color-mix(in srgb, #2563eb 18%, transparent);
		color: color-mix(in srgb, #1e3a8a 75%, currentColor);
		animation: synthesizing-pulse 1.6s ease-in-out infinite;
	}

	.synthesis-pill.synthesis-failed {
		background: color-mix(in srgb, #dc2626 20%, transparent);
		color: color-mix(in srgb, #7f1d1d 75%, currentColor);
	}

	.synthesis-pill.synthesis-failed-button {
		border: none;
		cursor: pointer;
		font-family: inherit;
	}

	.synthesis-pill.synthesis-failed-button:hover:not(:disabled) {
		background: color-mix(in srgb, #dc2626 30%, transparent);
	}

	.synthesis-pill.synthesis-failed-button:disabled {
		cursor: progress;
		opacity: 0.7;
	}

	@keyframes synthesizing-pulse {
		0%, 100% { opacity: 0.7; }
		50% { opacity: 1; }
	}

	.lifecycle-pill {
		display: inline-block;
		padding: 2px 8px;
		border-radius: 999px;
		font-size: 11px;
		font-weight: 500;
		background: color-mix(in srgb, currentColor 10%, transparent);
		color: var(--text-muted, var(--text-secondary, #555));
		text-transform: lowercase;
	}

	.lifecycle-pill.lifecycle-ephemeral {
		background: color-mix(in srgb, #7c3aed 18%, transparent);
		color: color-mix(in srgb, #4c1d95 75%, currentColor);
	}

	.status-completed {
		background: color-mix(in srgb, #16a34a 18%, transparent);
		color: color-mix(in srgb, #166534 75%, currentColor);
	}

	.status-failed {
		background: color-mix(in srgb, #dc2626 18%, transparent);
		color: color-mix(in srgb, #7f1d1d 75%, currentColor);
	}

	.status-running,
	.status-planning {
		background: color-mix(in srgb, #2563eb 18%, transparent);
		color: color-mix(in srgb, #1e3a8a 75%, currentColor);
	}

	.status-pending,
	.status-paused {
		background: color-mix(in srgb, #ca8a04 18%, transparent);
		color: color-mix(in srgb, #713f12 75%, currentColor);
	}

	.task-row {
		cursor: pointer;
	}

	.task-row:hover {
		background: color-mix(in srgb, var(--accent-primary, currentColor) 4%, transparent);
	}

	.empty {
		font-size: 12px;
		color: var(--text-muted, var(--text-secondary, #888));
		font-style: italic;
		margin: 4px 0;
	}

</style>
