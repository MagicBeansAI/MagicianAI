<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onDestroy, onMount, tick } from 'svelte';
	import { get } from 'svelte/store';
	import {
		taskStore,
		taskCounts as taskCountsStore,
		type Task,
		type TaskOutputMode,
		type TaskPriority
	} from '$lib/stores/taskStore';
	import { threadStore } from '$lib/stores/threadStore';
	import { personalAgentList, primaryAgent } from '$lib/stores/agentStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import type { TaskSchedule } from '$lib/types/agents';
	import { isValidCronExpression } from '$lib/utils/cron';
	import TaskCardMenu from '$lib/magician/components/TaskCardMenu.svelte';
	import Select from '$lib/magician/components/native/Select.svelte';
	import TaskFilterToolbar, {
		computeStatusChips
	} from '$lib/magician/components/TaskFilterToolbar.svelte';
	import NativeTasksSurface from '$lib/magician/tasks/NativeTasksSurface.svelte';
	import TaskCreateForm from '$lib/magician/tasks/TaskCreateForm.svelte';
	import MentionTextarea from '$lib/magician/chat/MentionTextarea.svelte';
	import MentionPicker from '$lib/magician/chat/MentionPicker.svelte';
	import {
		type ComposerMentionItem,
		buildTaskMentionItems
	} from '$lib/magician/chat/composerMentions';
	import { buildTaskLookup } from '$lib/components/taskMention';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Checkbox from '$lib/magician/components/generative/Checkbox.svelte';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import MonitorComposer from '$lib/monitors/MonitorComposer.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import {
		canConvertTaskToMonitor,
		convertFormFromTask,
		keptScheduleSummary
	} from '$lib/monitors/convert';
	import type { MonitorFormValue } from '$lib/monitors/specForm';
	import ExportMenu from '$lib/magician/components/ExportMenu.svelte';
	import TaskPanelDrawer from '$lib/magician/tasks/TaskPanelDrawer.svelte';
	import type { ActId } from '$lib/magician/tasks/taskCapabilities';
	import { toTaskPanelModel, type PanelOutputFile } from '$lib/magician/tasks/taskPanelModel';
	import { answerTaskAsk, type TaskAskState } from '$lib/magician/tasks/taskAsk';
	import type { HitlOpenTarget } from '$lib/hitl/types';
	import type { AttentionPromptResult } from '$lib/stores/attentionPromptStore';
	import type { TaskFilePreview } from '$lib/magician/tasks/taskFilePreview';
	import {
		fetchTaskOutputFiles,
		loadOutputPreview,
		taskOutputUrl
	} from '$lib/magician/tasks/taskOutputs';
	import { readTaskRunState } from '$lib/magician/tasks/taskAttention';
	import {
		createTaskPanelPoll,
		panelPollCadence,
		type PanelPollTarget
	} from '$lib/magician/tasks/taskPanelPoll';
	import type { ExecutionPanelState } from '$lib/types/executionPanel';
	// Task-scoped despite the module name: both post to
	// `/v3/tasks/{id}/outputs/open-file|open-folder`, which exists because a task
	// panel renders outputs with no chat session to hang them off.
	import {
		openInternalTaskOutputFile,
		revealInternalTaskOutputFile
	} from '$lib/internalTasks/api';
	import type {
		AgentPickerOption,
		ParsedTaskAction,
		ParsedTaskCompletionChange,
		TaskCreateSubmitValues
	} from '$lib/magician/tasks/types';
	import { showError, showInfo, showSuccess } from '$lib/shared/stores/notifications';
	import { requestAttentionInput } from '$lib/stores/attentionPromptStore';
	import { publishTaskToNotes } from '$lib/notes/taskNotes';

	// The v5 ("modern") shell is the only shell — the legacy TaskFilterBar
	// rail layout was removed.
	type TasksNavigationMode = 'route' | 'local';

	/** Route mode mirrors task state to /tasks query params; local mode owns it in-memory. */
	export let navigationMode: TasksNavigationMode = 'route';
	export let initialSelectedTaskId: string | null = null;

	const FILTER_LABELS: Record<string, string> = {
		all: 'All',
		inbox: 'Inbox',
		today: 'Today',
		overdue: 'Overdue',
		running: 'Running',
		completed: 'Completed'
	};

	function applyFilterClick(filter: string): void {
		const f = getActiveFilter(filter);
		taskStore.setFilter(f);
		updateFilterInUrl(f, $taskStore.selectedTaskId, null);
	}

	const FILTER_OPTIONS = ['all', 'inbox', 'today', 'overdue', 'running', 'completed'] as const;
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
	type PresetTaskFilter = (typeof FILTER_OPTIONS)[number];
	const FILTER_OPTION_SET = new Set<string>(FILTER_OPTIONS);

	let interactionBusy = false;
	let pageError: string | null = null;
	let taskRefreshToken = 0;
	let isPanelOpen = false;
	/** Guard: when user explicitly closes the panel, prevent the URL reactive from re-selecting. */
	let userDeselected = false;
	let selectedTask: Task | null = null;

	// ── the task panel ───────────────────────────────────────────────────
	/**
	 * The panel's clock, advanced on its own cadence rather than read at render.
	 *
	 * Two verdict headlines tick against it — `Waiting on you · 4m` and
	 * `Stalled · no progress for 6m` — and the verdict is a live region, so every
	 * advance re-announces it. Design §3 names the fix and puts it here: whoever
	 * owns `now` advances it **more slowly than it polls**. A minute is the
	 * resolution those two lines are written at, so a finer tick would announce
	 * the same words again.
	 */
	let panelNow = Date.now();
	let panelClockHandle: ReturnType<typeof setInterval> | null = null;
	const PANEL_CLOCK_INTERVAL_MS = 60_000;
	/**
	 * The selected task's output files, `null` while unknown — which is both
	 * "not fetched yet" and "the fetch failed". Design §6 treats the two the
	 * same on purpose: neither is evidence about what the task produced, so the
	 * Output act is absent rather than empty in both.
	 */
	let panelOutputs: PanelOutputFile[] | null = null;
	/** Which task `panelOutputs` describes, so another task's files can never render under this one. */
	let panelOutputsTaskId: string | null = null;
	let panelOutputsRequestId = 0;
	/**
	 * The selected task's current run, as `/execution-panel` reports it, or
	 * `null` — which here means both "we have not asked" and "the request did not
	 * answer". Unlike the outputs above, those two need not be told apart: every
	 * slice derived from this renders nothing when it is missing, so neither can
	 * make the panel claim anything.
	 *
	 * **Two slices come out of it and one request pays for both.** The ask
	 * blocking the run, without which a task list row cannot say a task is
	 * blocked mid-run and the panel reads `Queued · Waiting for a free slot`
	 * about a task waiting on the reader; and the Run act's timeline, which is
	 * the log of what the run actually did. The payload is handed to
	 * `toTaskPanelModel` whole rather than split here, because deriving either
	 * one needs to know which execution the Run act names, and only the adapter
	 * holds the task row that says.
	 */
	let panelRunState: ExecutionPanelState | null = null;
	/** Which task `panelRunState` describes, for the reason the outputs have an id. */
	let panelRunStateTaskId: string | null = null;
	/**
	 * Which of the task's runs the reader asked to read, or `null` for the one the
	 * task record points at.
	 *
	 * **Reader intent, not loaded state**, which is why it is a separate variable
	 * from `panelRunState` rather than something read back off it. The payload can
	 * only say which run it *describes*, and between the click and the reply it
	 * still describes the previous one — so a control driven off the payload would
	 * spring back to the old option for a round trip, which reads as the panel
	 * refusing the click. It is driven off this instead, and the two slices that
	 * would otherwise be wrong in the meantime carry their own execution-id guards
	 * and render absent (see `runOf`).
	 *
	 * It has a task id beside it for the same reason the two payloads above do: a
	 * run id belongs to one task, and carried onto another it would pin that task's
	 * panel to a run it has never had — a request the endpoint answers with
	 * `execution panel state not found`, so the Run act would simply never load.
	 */
	let panelRunSelectionId: string | null = null;
	let panelRunSelectionTaskId: string | null = null;
	let panelPreferredAct: ActId | null = null;
	/**
	 * When the panel's own state was last read successfully, and why the last read
	 * failed — the two halves of design §6's third case.
	 *
	 * **Panel-scoped rather than the list's last refresh.** The staleness line
	 * answers "is what I am looking at current", and what the reader is looking at
	 * is this task's run, refreshed on its own cadence. The list's own refresh
	 * time — which this used to be — dates the panel by something that is not its
	 * source, and on a settled task, whose panel deliberately stops polling, it
	 * would go on counting up under a run that cannot change.
	 */
	let panelLoadedAt: number | null = null;
	let panelLoadFailure: string | null = null;
	/**
	 * The contents of the output row the reader expanded, or `null` when none is
	 * open — **the one fetch on this panel that is not made on open.** The others
	 * pay for a task; this one pays for a file, and only for a file the reader
	 * asked to read. Listing ten outputs still costs the one outputs request.
	 *
	 * It needs no task id beside it, unlike the two above, and that is a property
	 * of the panel rather than an omission: the panel drops its open row whenever
	 * the task id changes, and re-opening one dispatches, which writes a `loading`
	 * record here **before** the render that would have shown the old one. A
	 * second id guard would be a second mechanism for something already covered,
	 * and the one that stopped working would be the one doing the job.
	 */
	let panelPreview: TaskFilePreview | null = null;
	let panelPreviewRequestId = 0;
	/**
	 * Where the answer to the panel's ask has got to, or `null` when nothing is in
	 * flight.
	 *
	 * It carries the ask's id and the panel checks it, for the reason the preview
	 * above carries an index: this list polls, and the ask on screen can change
	 * while a post is in the air. **There is deliberately no state meaning
	 * `answered`** — a successful post clears this and re-reads the task, so the
	 * ask disappears because the server stopped listing it. Anything else would be
	 * this surface deciding a question had been answered on the strength of having
	 * asked.
	 */
	let panelAsk: TaskAskState | null = null;
	let panelAskRequestId = 0;

	/** Page-level ⋯ overflow menu (TaskCardMenu), anchored to the native card trigger. */
	let taskMenuTaskId: string | null = null;
	let taskMenuAnchor: HTMLElement | null = null;
	let activeTagEditorTaskId: string | null = null;

	// ── list pagination ──────────────────────────────────────────────────
	// Client-side over the loaded pool. Two of the three reasons this had to be
	// so are now gone; the third has not moved. Verified against `list_tasks` in
	// `task_api_v3.rs`, so the next reader does not re-litigate:
	//
	//  1. GONE — the six lanes ARE expressible now. `GET /v3/tasks?view=<lane>`
	//     applies one server-side, over the whole scoped pool, so a lane is no
	//     longer a search of whatever page is loaded. `taskStore` asks for the
	//     active lane and keeps the answer; see `splitTasksByLane`.
	//  2. GONE — the response carries `counts`, every lane's total taken before
	//     the lane filter, so the six filter badges have a server-side source.
	//     They read it (`taskCounts`), adjusted only by the grace-period overlay
	//     that also moves the rows. The status ledger and "N total" still count
	//     the loaded pool, which is why it must stay whole — see below.
	//  3. UNCHANGED, and decisive — `taskStore.tasks` is the app's task CORPUS,
	//     not this list's model. The thread tasks route, the command palette,
	//     the vibe cockpit and chat's `@`-mention picker all read it whole.
	//     Paging it would silently shrink those: measured on the live pool, a
	//     50-row first page carries 7 of 73 completed tasks, which is what the
	//     mention picker offers.
	//
	// So: slice the filtered list here, and keep the pool whole. A filter change
	// resets to page one. Phase 1 of the pagination design is where the list
	// stops being sliced from a corpus at all.
	/**
	 * How many cards render at once, and the reader's to choose.
	 *
	 * A `let`, not a const, because the internal-tasks tab on this route has offered
	 * this for as long as it has had a pager and the two lanes should not disagree
	 * about whether page size is a preference. The options match that table's, so
	 * switching tabs does not switch vocabularies.
	 *
	 * **Changing it resets to page one**, for the same reason a filter change does:
	 * page 3 of 25 is not page 3 of 100, so keeping the number would land the reader
	 * somewhere they did not ask for — and on a shorter list, somewhere that does not
	 * exist. `setTasksPageSize` is the only writer, so that reset cannot be forgotten
	 * at a call site.
	 */
	const TASKS_PAGE_SIZES = [25, 50, 100, 250];
	let TASKS_PAGE_SIZE = 25;
	let taskPageOffset = 0;

	function setTasksPageSize(next: number): void {
		if (!Number.isFinite(next) || next <= 0) return;
		TASKS_PAGE_SIZE = next;
		taskPageOffset = 0;
	}
	// Untracked guard: a reactive key read+written in one `$:` block
	// self-invalidates forever.
	const taskPageGuard = { key: '' };
	$: {
		const key = `${activeFilter}|${activeTag}|${$taskStore.searchQuery ?? ''}`;
		if (key !== taskPageGuard.key) {
			taskPageGuard.key = key;
			taskPageOffset = 0;
		}
	}
	// Clamp when the pool shrinks under the current page (converges: the
	// write flips the condition false, so the re-run stops).
	$: if (taskPageOffset > 0 && taskPageOffset >= $taskStore.filteredTasks.length) {
		taskPageOffset = Math.max(
			0,
			Math.floor(Math.max(0, $taskStore.filteredTasks.length - 1) / TASKS_PAGE_SIZE) * TASKS_PAGE_SIZE
		);
	}
	$: pagedTasks = $taskStore.filteredTasks.slice(taskPageOffset, taskPageOffset + TASKS_PAGE_SIZE);
	$: taskPageTotal = $taskStore.filteredTasks.length;
	$: taskPageStart = taskPageTotal === 0 ? 0 : taskPageOffset + 1;
	$: taskPageEnd = Math.min(taskPageOffset + TASKS_PAGE_SIZE, taskPageTotal);
	// Same page math the internal-tasks table feeds ServerPager, so the two tabs
	// on this route can't drift in how they count.
	$: taskPageCount = Math.max(1, Math.ceil(taskPageTotal / TASKS_PAGE_SIZE));
	$: taskCurrentPage = taskPageTotal === 0 ? 1 : Math.floor(taskPageOffset / TASKS_PAGE_SIZE) + 1;

	/**
	 * The pager's numbers, derived once and spread into both copies.
	 *
	 * There are two pagers — above the list and below it — and they must never be
	 * able to disagree. Two hand-written prop lists reading the same five reactive
	 * values would agree today and drift the first time one of them is edited, and
	 * the failure would be a control asserting a page the other control denies. One
	 * object means a change reaches both or neither.
	 *
	 * `loading` and `on:pagechange` stay at each call site: the first is read live
	 * from the store, and an event handler is not a prop.
	 */
	$: taskPagerProps = {
		currentPage: taskCurrentPage,
		pageCount: taskPageCount,
		startItem: taskPageStart,
		endItem: taskPageEnd,
		totalItems: taskPageTotal
	};

	function gotoTaskPage(pageNumber: number): void {
		const safePage = Math.min(taskPageCount, Math.max(1, Math.floor(pageNumber)));
		taskPageOffset = (safePage - 1) * TASKS_PAGE_SIZE;
	}
	let activeScheduleEditorTaskId: string | null = null;
	let pendingDeleteTask: Task | null = null;
	let deleteTaskRemoveFiles = true;
	let deleteTaskSubmitting = false;
	/** Convert-to-monitor modal state (Phase 7): the task + prefilled form. */
	let convertTask: Task | null = null;
	let convertForm: MonitorFormValue | null = null;
	let scheduleExpanded = false;
	let composeExpanded = false;
	let tasksMounted = false;
	let tasksReady = false;
	let currentTasksScopeKey = '';
	let lastTasksScopeKey = '';
	let localActiveFilter: PresetTaskFilter = 'all';
	let localActiveTag = '';
	let lastInitialSelectedTaskId: string | null | undefined;
	let createTitle = '';
	let createOutputMode: TaskOutputMode = 'accumulate';
	let selectedThreadId = 'general';

	// The device's IANA timezone (e.g. "Asia/Kolkata") — used as the default for the
	// schedule timezone field. Computed client-side only (empty during SSR; the
	// schedule card is collapsed by default so there's no hydration mismatch).
	const deviceTimezone = browser
		? Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'
		: '';

	// A short curated list of common IANA timezones for the schedule timezone dropdown.
	const COMMON_TIMEZONES = [
		'UTC',
		'America/Los_Angeles',
		'America/Denver',
		'America/Chicago',
		'America/New_York',
		'America/Sao_Paulo',
		'Europe/London',
		'Europe/Paris',
		'Europe/Berlin',
		'Europe/Moscow',
		'Africa/Johannesburg',
		'Asia/Dubai',
		'Asia/Kolkata',
		'Asia/Singapore',
		'Asia/Shanghai',
		'Asia/Tokyo',
		'Australia/Sydney',
		'Pacific/Auckland'
	];
	// Dropdown options: the device timezone first (labelled, selected by default),
	// then the common zones — deduped so the device tz isn't listed twice.
	const scheduleTimezoneOptions = (() => {
		const seen = new Set<string>();
		const options: Array<{ value: string; label: string }> = [];
		if (deviceTimezone) {
			seen.add(deviceTimezone);
			options.push({ value: deviceTimezone, label: `${deviceTimezone} (device)` });
		}
		for (const tz of COMMON_TIMEZONES) {
			if (seen.has(tz)) continue;
			seen.add(tz);
			options.push({ value: tz, label: tz });
		}
		return options;
	})();

	// Track selected agent and schedule values from the form
	let selectedAgentId = '';
	let scheduleCron = '';
	let scheduleTimezone = deviceTimezone;

	// New-task composer description + its @-mention task references, on the
	// standardized MentionTextarea (shared with chat + the vibe cockpit; replaced
	// the bespoke TaskMentionInput). The chips ARE the source of truth now:
	// `mentionDescription` is the serialized text (chips emit inline `task:<id>`
	// tokens — precise + LLM-legible), and `linkedTaskIds` is DERIVED from the chip
	// set after every edit (deduped + stale-pruned) → sent as the task's structured
	// `depends_on` edge.
	let mentionDescription = '';
	let linkedTaskIds: string[] = [];
	let mentionEl: MentionTextarea | null = null;
	let mentionOpen = false;
	let mentionMatches: ComposerMentionItem[] = [];
	let mentionActiveIndex = 0;
	// Offer only COMPLETED persistent tasks: internal/VibeDev runs live in a separate
	// feed and are intentionally excluded (the agreed "no internal in the dropdown"
	// rule), and the backend dependency validator also rejects non-completed ids.
	$: taskMentionItems = buildTaskMentionItems(
		$taskStore.tasks.filter((task) => task.status === 'completed')
	);

	// Recompute the structured dependency ids from the chips after every edit. The
	// chip set is authoritative (text + ids can't drift), deduped first-seen, and
	// pruned to tasks that still exist so a task deleted mid-compose can't leave a
	// dead id in `depends_on`.
	function refreshLinkedTaskIds(): void {
		const lookup = buildTaskLookup($taskStore.tasks);
		const seen = new Set<string>();
		const ids: string[] = [];
		for (const token of mentionEl?.chipTokens() ?? []) {
			if (token.kind !== 'task') continue;
			const id = token.slug;
			if (!id || seen.has(id) || !lookup.has(id)) continue;
			seen.add(id);
			ids.push(id);
		}
		linkedTaskIds = ids;
	}

	$: usesRouteNavigation = navigationMode === 'route';
	$: activeFilter = usesRouteNavigation
		? getActiveFilter($page.url.searchParams.get('filter'))
		: localActiveFilter;
	$: activeTag = usesRouteNavigation
		? asString($page.url.searchParams.get('tag')).trim()
		: localActiveTag;
	$: selectedTaskIdFromQuery = usesRouteNavigation
		? asString($page.url.searchParams.get('selected')).trim()
		: '';
	// Distinct tag set for the v5 task-filter toolbar. Computed here (not as a
	// template {@const}) because the toolbar lives directly under a plain
	// <div>, where Svelte disallows {@const}.
	$: v5Tags = [
		...new Set(
			$taskStore.tasks
				.flatMap((t) => t.tags.map((tag) => (typeof tag === 'string' ? tag : tag.name)))
				.filter(Boolean)
		)
	];

	// `?compose=1` opens the New Task composer, scrolls it into view, and
	// focuses the title input. Used by the command palette's "New task"
	// action and the global `N` shortcut. Reactive (not just onMount) so
	// it fires when the user is already on /tasks and the URL only changes
	// via query string. Strip the param afterwards so a refresh doesn't
	// re-trigger the composer.
	async function openComposerFromQuery(): Promise<void> {
		composeExpanded = true;
		const params = new URLSearchParams($page.url.searchParams);
		params.delete('compose');
		const nextUrl = params.toString() ? `/tasks?${params.toString()}` : '/tasks';
		await goto(nextUrl, { replaceState: true, noScroll: true, keepFocus: true });

		// Wait for the composer to render after the reactive update, then
		// scroll it to the top of the viewport so as much of the (often
		// tall) pane is visible as possible. Falls back to the mention
		// input area below if the inline-compose form isn't rendered yet.
		// `scroll-margin-top` clears the 48px sticky TopBar plus 16px of
		// headroom so the composer's heading isn't hidden underneath.
		await tick();
		const target =
			document.querySelector<HTMLElement>('.presto-spells-page .native-task-compose')
			?? document.querySelector<HTMLElement>('.presto-spells-mention-input');
		if (target) {
			target.style.scrollMarginTop = '64px';
			target.scrollIntoView({ behavior: 'smooth', block: 'start' });
		}

		const input = document.querySelector(
			'.presto-spells-page .native-task-compose .task-create-title-input'
		) as HTMLInputElement | null;
		input?.focus();
	}

	$: if (browser && usesRouteNavigation && $page.url.searchParams.has('compose')) {
		void openComposerFromQuery();
	}
	$: currentTasksScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: if (activeTag) {
		if (!(typeof $taskStore.filter === 'object' && 'custom' in $taskStore.filter && $taskStore.filter.custom === activeTag)) {
			taskStore.setFilter({ custom: activeTag });
		}
	} else if (activeFilter && $taskStore.filter !== activeFilter) {
		taskStore.setFilter(activeFilter);
	}
	$: {
		if (selectedTaskIdFromQuery && !userDeselected) {
			if ($taskStore.selectedTaskId !== selectedTaskIdFromQuery) {
				taskStore.selectTask(selectedTaskIdFromQuery);
			}
		}
		// Reset guard once the URL catches up (selected param removed)
		if (!selectedTaskIdFromQuery) {
			userDeselected = false;
		}
	}
	$: if ($taskStore.selectedTaskId) {
		const task = $taskStore.tasks.find((next) => next.id === $taskStore.selectedTaskId) ?? null;
		selectedTask = task;
		isPanelOpen = task !== null;
	} else {
		selectedTask = null;
		isPanelOpen = false;
	}

	/*
	 * The per-task loads behind the panel.
	 *
	 * **Every one of these re-runs on every store write**, and the comment here
	 * used to say they did not. Passing `selectedTask?.id` looks like it keys the
	 * statement on the id, and it does not: Svelte invalidates on assignment, and
	 * `safe_not_equal` reports every object assignment as a change, so the store
	 * replacing `selectedTask` with an equal record re-runs them. That is measured,
	 * in `ThreadTasksPanel.component.test.ts`, rather than reasoned about.
	 *
	 * It used to be what made the Run act's timeline advance at all, which is why
	 * it was left alone: the panel had no clock of its own and rode the task
	 * store's 15–20s list backstop. It has one now (`panelPoll`), and the store
	 * write is a poll's *result* rather than its cause — so re-running these on it
	 * would spend a request to learn what the request that caused it just said.
	 * Both are therefore keyed on something that actually changed.
	 */
	/**
	 * The outputs refetch, keyed on the task **and its status**.
	 *
	 * The id alone is not enough — a task's files appear when it finishes, and a
	 * panel held open across that moment would keep showing `—`. The status is the
	 * event that changes the answer, so it is what triggers the read; the panel's
	 * clock is not, because files do not arrive on a clock and re-reading them four
	 * times a minute would spend a request to learn the same thing.
	 */
	// Untracked guard, the same one the pager uses: a reactive key read and
	// written in one `$:` block would self-invalidate forever.
	const panelOutputsGuard = { key: '' };
	$: {
		const outputsTaskId = browser && isPanelOpen ? (selectedTask?.id ?? null) : null;
		const outputsKey =
			outputsTaskId === null ? '' : `${outputsTaskId}:${selectedTask?.status ?? ''}`;
		if (outputsKey !== panelOutputsGuard.key) {
			panelOutputsGuard.key = outputsKey;
			void loadPanelOutputs(outputsTaskId);
		}
	}

	/**
	 * The run behind the panel — its ask and its event log, in one response — and
	 * the task record the verdict above them is read off.
	 *
	 * **One poll, aimed.** The target carries the run the reader chose, so a poll
	 * can never snap the Run act back to the newest attempt while they are reading
	 * an earlier one; the cadence comes from the task's own status, so a finished
	 * run — whose events cannot change — is not polled at all. Both live in
	 * `taskPanelPoll.ts` with the reasoning.
	 */
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
	 * The panel's whole input. One expression, so nothing renders from a task and
	 * an output list belonging to two different tasks: the outputs are handed over
	 * only once they are known to describe **this** task, and read as unknown
	 * until then.
	 */
	$: panelModel =
		selectedTask === null
			? null
			: toTaskPanelModel(
					selectedTask,
					// The id check, not just the clearing in `loadPanelOutputs`: the
					// files reach the panel only once they are known to describe *this*
					// task, so a list belonging to the task we just left is unknown here
					// rather than merely stale — and that holds however the two
					// assignments end up ordered.
					panelOutputsTaskId === selectedTask.id ? panelOutputs : null,
					// The same id check for the same reason, and it matters more here:
					// this payload carries both the ask and the run's event log, so one
					// carried over from the task the reader just left would put
					// `Waiting on you` above a task nothing is asking about *and* list
					// another task's calls under this one's Run act.
					panelRunStateTaskId === selectedTask.id ? panelRunState : null,
					// The same id check, third time and last: a run id is meaningless
					// against another task, and handing one over would point this panel's
					// Run act at an execution this task never had.
					panelRunSelectionTaskId === selectedTask.id ? panelRunSelectionId : null,
					(path) =>
						taskOutputUrl(
							selectedTask!.id,
							path,
							$scopeIdentityStore.principal,
							$scopeIdentityStore.workspace
						)
				);

	/**
	 * Design §6, case 2: a task the reader asked for that the panel could not
	 * load. Gated on the store reporting an actual load failure, not merely on
	 * the id being absent from the list — an id that is simply unknown is not
	 * evidence of a failure, and `Can't load this task` would be the fabricated
	 * verdict §6 exists to forbid, aimed at the network instead of the task.
	 */
	$: panelUnloadable =
		tasksReady
		&& !userDeselected
		&& $taskStore.selectedTaskId !== null
		&& selectedTask === null
		&& $taskStore.error !== null;
	$: panelVisible = isPanelOpen || panelUnloadable;

	// Derive personal agents for the agent picker
	$: personalAgents = ($personalAgentList || []).map((agent): AgentPickerOption => ({
		agent_id: agent.agent_id,
		name: agent.name || agent.agent_id
	}));
	$: taskThreadOptions = $threadStore.threads
		.filter((thread) => !thread.archived)
		.map((thread) => ({ id: thread.id, name: thread.name }));

	// Default the agent picker to the PRIMARY (default) personal agent — e.g. Presto —
	// falling back to the first personal agent. Clears a stale selection if the agent
	// is no longer in the list.
	$: if (selectedAgentId && !personalAgents.some((agent) => agent.agent_id === selectedAgentId)) {
		selectedAgentId = '';
	}
	$: if (personalAgents.length > 0 && !selectedAgentId) {
		const primaryId = $primaryAgent?.agent_id;
		selectedAgentId =
			primaryId && personalAgents.some((agent) => agent.agent_id === primaryId)
				? primaryId
				: personalAgents[0].agent_id;
	}
	$: if (selectedThreadId && taskThreadOptions.length > 0 && !taskThreadOptions.some((thread) => thread.id === selectedThreadId)) {
		selectedThreadId = 'general';
	}

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value);
		}
		return '';
	}

	function isFilter(value: string | null): value is PresetTaskFilter {
		return value !== null && FILTER_OPTION_SET.has(value);
	}

	function getActiveFilter(raw: string | null): PresetTaskFilter {
		return isFilter(raw) ? raw : 'all';
	}

	function tasksScopeKey(): string {
		const scope = get(scopeIdentityStore);
		return `${scope.principal}:${scope.workspace}`;
	}

	async function clearScopedTaskSelectionFromUrl(): Promise<void> {
		if (!browser || !usesRouteNavigation || !$page.url.searchParams.has('selected')) return;
		const params = new URLSearchParams($page.url.searchParams);
		params.delete('selected');
		const nextUrl = params.toString() ? `/tasks?${params.toString()}` : '/tasks';
		await goto(nextUrl, { replaceState: true, noScroll: true });
	}

	function clearScopedTaskPageState(): void {
		taskRefreshToken += 1;
		interactionBusy = false;
		pageError = null;
		isPanelOpen = false;
		userDeselected = true;
		selectedTask = null;
		selectedAgentId = '';
		scheduleCron = '';
		scheduleTimezone = deviceTimezone;
		createTitle = '';
		createOutputMode = 'accumulate';
		selectedThreadId = 'general';
		linkedTaskIds = [];
		mentionDescription = '';
		taskMenuTaskId = null;
		taskMenuAnchor = null;
		activeTagEditorTaskId = null;
		activeScheduleEditorTaskId = null;
		pendingDeleteTask = null;
		deleteTaskRemoveFiles = true;
		deleteTaskSubmitting = false;
		convertTask = null;
		convertForm = null;
		void taskStore.selectTask(null);
	}

	function captureTasksPageScopeKey(): string {
		return currentTasksScopeKey;
	}

	function isStaleTasksPageScope(scopeKey: string): boolean {
		return scopeKey !== currentTasksScopeKey;
	}

	function resolveAbortTarget(task: Task): string | null {
		const executionId = asString(task.activeExecutionId).trim();
		return executionId || null;
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

	function closeAllMenus(): void {
		taskMenuTaskId = null;
		taskMenuAnchor = null;
		activeTagEditorTaskId = null;
		activeScheduleEditorTaskId = null;
	}

	function openNativeTaskMenu(taskId: string, anchor: HTMLElement): void {
		const wasOpen = taskMenuTaskId === taskId;
		closeAllMenus();
		if (!wasOpen) {
			taskMenuTaskId = taskId;
			taskMenuAnchor = anchor;
		}
	}

	function handleGlobalKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape') {
			if (pendingDeleteTask && !deleteTaskSubmitting) {
				closeDeleteTaskDialog();
				return;
			}
			closeAllMenus();
		}
	}

	/**
	 * Innermost first: one Escape dismisses one thing. A menu or the delete
	 * dialog is open **over** the drawer, so it goes before the drawer does —
	 * closing both on one keypress loses the reader's place to a keystroke they
	 * meant for the menu.
	 *
	 * The drawer does the closing; this only says whether the keystroke is its
	 * to take. Which layers exist is this workspace's knowledge, and every
	 * surface that opens the panel has a different set — so the ordering stays
	 * here while the closing lives once, in the shell. One mechanism each,
	 * rather than two things racing to close one drawer.
	 *
	 * Read on the keystroke, so it still holds the pre-keypress layer state
	 * while `handleGlobalKeydown` above is dismissing that layer.
	 */
	$: escapeBelongsToPanel =
		!(pendingDeleteTask !== null && !deleteTaskSubmitting)
		&& taskMenuTaskId === null
		&& activeTagEditorTaskId === null
		&& activeScheduleEditorTaskId === null;

	function openDeleteTaskDialog(task: Task): void {
		pendingDeleteTask = task;
		deleteTaskRemoveFiles = true;
		deleteTaskSubmitting = false;
	}

	/**
	 * Convert-to-monitor (Phase 7): open the existing MonitorComposer in
	 * `convert` mode, prefilled from the task (objective ← description,
	 * title ← title). User-explicit only — the menu item is the ONLY entry,
	 * eligibility is `canConvertTaskToMonitor`, and the backend re-checks.
	 */
	function openConvertToMonitor(task: Task): void {
		if (!canConvertTaskToMonitor(task)) {
			showInfo('This task cannot be converted to a monitor.');
			return;
		}
		convertForm = convertFormFromTask(task);
		convertTask = task;
	}

	function closeConvertToMonitor(): void {
		convertTask = null;
		convertForm = null;
	}

	async function handleConvertSaved(
		event: CustomEvent<{ taskId: string; monitorRevision: number }>
	): Promise<void> {
		void event;
		closeConvertToMonitor();
		showSuccess('Converted to monitor. Manage it under Tasks → Monitors.');
		await refreshTasksData(false);
	}

	function closeDeleteTaskDialog(): void {
		if (deleteTaskSubmitting) return;
		pendingDeleteTask = null;
		deleteTaskRemoveFiles = true;
	}

	async function confirmDeleteTask(): Promise<void> {
		const task = pendingDeleteTask;
		if (!task || deleteTaskSubmitting) return;
		const requestScopeKey = captureTasksPageScopeKey();
		deleteTaskSubmitting = true;
		interactionBusy = true;
		pageError = null;
		try {
			await taskStore.deleteTask(task.id, { removeFiles: deleteTaskRemoveFiles });
			if (isStaleTasksPageScope(requestScopeKey)) return;
			if (selectedTaskIdFromQuery === task.id) {
				await clearScopedTaskSelectionFromUrl();
			}
			pendingDeleteTask = null;
			showInfo(deleteTaskRemoveFiles ? 'Task deleted and folder removed.' : 'Task deleted. Folder kept.');
			await refreshTasksData(false);
		} catch (error) {
			if (isStaleTasksPageScope(requestScopeKey)) return;
			const message = error instanceof Error ? error.message : 'Task deletion failed';
			pageError = message;
			showError(message);
			await refreshTasksData(false);
		} finally {
			if (isStaleTasksPageScope(requestScopeKey)) return;
			deleteTaskSubmitting = false;
			interactionBusy = false;
		}
	}

	async function refreshTasksData(touchBusy = false): Promise<void> {
		const token = ++taskRefreshToken;
		const priorBusy = interactionBusy;
		if (touchBusy && !interactionBusy) {
			interactionBusy = true;
		}
		pageError = null;
		try {
			await taskStore.loadTasks();
		} catch (error) {
			if (token === taskRefreshToken) {
				console.error('Failed to refresh tasks:', error);
				pageError = error instanceof Error ? error.message : 'Failed to refresh tasks';
				showError(pageError);
			}
		} finally {
			if (touchBusy && priorBusy === false) {
				interactionBusy = false;
			}
		}
	}

	function updateFilterInUrl(
		filter: PresetTaskFilter,
		selectedTaskId: string | null = $taskStore.selectedTaskId,
		tag: string | null = activeTag || null
	): void {
		if (!browser) return;
		if (!usesRouteNavigation) {
			localActiveFilter = filter;
			localActiveTag = tag ?? '';
			return;
		}
		const params = new URLSearchParams($page.url.searchParams);
		params.set('filter', filter);
		if (tag) {
			params.set('tag', tag);
		} else {
			params.delete('tag');
		}
		const selected = asString(selectedTaskId).trim();
		if (selected) {
			params.set('selected', selected);
		} else {
			params.delete('selected');
		}
		void goto(`/tasks?${params.toString()}`, {
			replaceState: true,
			noScroll: true
		});
	}

	function clearCreateTaskForm(): void {
		mentionDescription = '';
		linkedTaskIds = [];
		scheduleCron = '';
		scheduleTimezone = deviceTimezone;
		scheduleExpanded = false;
		createTitle = '';
		createOutputMode = 'accumulate';
		selectedThreadId = 'general';
	}

	async function createTask(values: TaskCreateSubmitValues): Promise<void> {
		if (interactionBusy) return;
		interactionBusy = true;
		pageError = null;
		try {
			const title = asString(values.task_title).trim();
			if (!title) {
				throw new Error('Task title is required');
			}
			// The description is the native @-mention field (the form no longer carries
			// a separate `task_description` textarea — it was a confusing duplicate).
			const description = mentionDescription.trim();
			if (!description) {
				throw new Error('Description is required');
			}

			// Resolve agent_id: use form selection or fall back to first personal agent.
			// agent_id is compulsory — every task must be owned by a Personal agent.
			const agentId =
				asString(values.task_agent).trim() ||
				selectedAgentId ||
				(personalAgents.length > 0 ? personalAgents[0].agent_id : '');
			if (!agentId) {
				throw new Error('No personal agent available. Please wait for agents to load or create one first.');
			}
			const agentName = personalAgents.find((a) => a.agent_id === agentId)?.name || agentId;

			// Build schedule if cron is provided
			let schedule: TaskSchedule | undefined;
			if (scheduleExpanded && scheduleCron.trim()) {
				if (!isValidCronExpression(scheduleCron.trim())) {
					throw new Error('Invalid cron expression. Must be 5 space-separated fields (e.g. "0 9 * * *").');
				}
				schedule = { cron: scheduleCron.trim() };
				if (scheduleTimezone.trim()) {
					schedule.timezone = scheduleTimezone.trim();
				}
			}

			// Re-derive the dependency ids from the chips at submit time, NOT just on
			// the last keystroke: a referenced task can be dropped from $taskStore.tasks
			// by a realtime event / background poll after it was chipped, and without a
			// further edit `on:input` wouldn't re-prune it — sending a dead id as
			// depends_on (which the backend validator rejects). This makes the prune
			// deterministic regardless of whether a keystroke followed the deletion.
			if (mentionEl) refreshLinkedTaskIds();

			const finalDescription = description;
			const rawOutputMode = asString(values.task_output_mode).trim().toLowerCase();
			const outputMode: TaskOutputMode =
				rawOutputMode === 'overwrite' ? 'overwrite' : 'accumulate';

			const threadId = asString(values.task_thread).trim() || selectedThreadId || 'general';

			await taskStore.createTask(title, finalDescription, {
				agentId,
				agentName: agentName || undefined,
				schedule,
				createdBy: 'user',
				outputMode,
				dependsOn: linkedTaskIds.length > 0 ? linkedTaskIds : undefined,
				uiThreadId: threadId
			});
			showSuccess('Task created');

			// Close the compose panel and reset schedule state
			composeExpanded = false;
			clearCreateTaskForm();

			await refreshTasksData(false);
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to create task';
			pageError = message;
			showError(message);
		} finally {
			interactionBusy = false;
		}
	}

	/**
	 * Palette-derived tag color: pick --theme-chart-color-{0..7} by a stable
	 * hash of the tag name. Tag colors ARE persisted to the backend (taskStore
	 * sends `color` on create/update), so stored values must stay literal
	 * colors — a `var(...)` string would not render for non-CSS consumers.
	 * Resolve the hash-picked token to its computed value at write time.
	 *
	 * Staleness caveat: persisted tag colors are frozen at creation-time
	 * palette — a later theme switch does not retint existing tags, and the
	 * same tag name created under different themes stores different literals.
	 * Also, getPropertyValue may return non-hex syntax (oklch(...) etc.),
	 * which is fine for CSS consumers.
	 */
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
		// Root chart palette fallback when the token cannot be resolved (SSR/tests).
		return TAG_COLOR_FALLBACKS[paletteIndex];
	}

	async function handleAddTag(taskId: string, tagNameRaw: string): Promise<void> {
		if (!browser || interactionBusy) return;
		const task = $taskStore.tasks.find((next) => next.id === taskId);
		if (!task) {
			showError('Task was not found');
			return;
		}
		const tagName = tagNameRaw.trim().replace(/^#/, '');
		if (!tagName) return;
		const hasExisting = task.tags.some((tag) => asString(tag.name).trim().toLowerCase() === tagName.toLowerCase());
		if (hasExisting) {
			showInfo(`Tag "${tagName}" already exists.`);
			activeTagEditorTaskId = null;
			return;
		}
		interactionBusy = true;
		pageError = null;
		try {
			const nextTags = [...task.tags, { name: tagName, color: tagColorForName(tagName) }];
			await taskStore.updateTask(taskId, { tags: nextTags });
			showSuccess(`Tag "${tagName}" added.`);
			await refreshTasksData(false);
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to add tag';
			pageError = message;
			showError(message);
		} finally {
			activeTagEditorTaskId = null;
			interactionBusy = false;
		}
	}

	async function handleRemoveTag(taskId: string, tagName: string): Promise<void> {
		if (!browser || interactionBusy) return;
		const task = $taskStore.tasks.find((next) => next.id === taskId);
		if (!task) {
			showError('Task was not found');
			return;
		}
		const sanitized = tagName.toLowerCase();
		const nextTags = task.tags.filter((t) => t.name.toLowerCase() !== sanitized);
		if (nextTags.length === task.tags.length) return; // tag not found
		interactionBusy = true;
		pageError = null;
		try {
			await taskStore.updateTask(taskId, { tags: nextTags });
			showSuccess(`Tag "${tagName}" removed.`);
			await refreshTasksData(false);
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to remove tag';
			pageError = message;
			showError(message);
		} finally {
			interactionBusy = false;
		}
	}

	async function handleTaskAction(action: ParsedTaskAction): Promise<void> {
		if (action.action === 'menu') {
			closeAllMenus();
			return;
		}
		if (action.action === 'schedule_edit') {
			const next = activeScheduleEditorTaskId === action.taskId ? null : action.taskId;
			closeAllMenus();
			activeScheduleEditorTaskId = next;
			return;
		}
		if (action.action === 'convert_monitor') {
			// UI-open action (no API call here): the composer's review step
			// owns the POST /monitors/{id}/convert.
			closeAllMenus();
			const task = $taskStore.tasks.find((next) => next.id === action.taskId);
			if (!task) {
				showError('Task was not found');
				return;
			}
			openConvertToMonitor(task);
			return;
		}
		if (interactionBusy) {
			return;
		}
		const requestScopeKey = captureTasksPageScopeKey();
		closeAllMenus();
		interactionBusy = true;
		pageError = null;
		try {
			const task = $taskStore.tasks.find((next) => next.id === action.taskId);
			if (!task) {
				throw new Error('Task was not found');
			}

			if (action.action === 'open') {
				panelPreferredAct = null;
				taskStore.selectTask(action.taskId);
				updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
			} else if (action.action === 'view_result') {
				panelPreferredAct = 'output';
				taskStore.selectTask(action.taskId);
				updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
			} else if (action.action === 'doit') {
				if (task.planStatus === 'planning') {
					showInfo('Planning is still in progress for this task.');
					taskStore.selectTask(action.taskId);
					updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
				} else if (task.planStatus === 'eliciting') {
					showInfo(
						task.pendingQuestion
							? 'This plan is waiting for your answer before it can continue.'
							: 'This plan is waiting for clarification before it can continue.'
					);
					taskStore.selectTask(action.taskId);
					updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
				} else if (task.planStatus === 'draft') {
					showInfo('Review and approve the plan before execution.');
					taskStore.selectTask(action.taskId);
					updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
				} else if (task.planStatus === 'approved') {
					if (task.status !== 'ready') {
						showInfo('This approved plan is not ready to run yet.');
						taskStore.selectTask(action.taskId);
						updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
						} else {
							showInfo(`Starting execution for "${task.title}"...`);
							await taskStore.executeTask(action.taskId);
							if (isStaleTasksPageScope(requestScopeKey)) return;
							taskStore.selectTask(action.taskId);
							updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
							showSuccess('Execution started.');
					}
				} else if (task.status === 'pending') {
					showInfo(`Generating plan for "${task.title}"...`);
					if (task.planStatus === 'rejected' || task.planStatus === 'failed') {
						await taskStore.replanTask(action.taskId);
						if (isStaleTasksPageScope(requestScopeKey)) return;
						showSuccess('Planning restarted.');
					} else {
						await taskStore.planTask(action.taskId);
						if (isStaleTasksPageScope(requestScopeKey)) return;
						showSuccess('Plan generated. Review and execute when ready.');
					}
					taskStore.selectTask(action.taskId);
					updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
				} else if (task.status === 'ready') {
					showInfo(`Starting execution for "${task.title}"...`);
					await taskStore.executeTask(action.taskId);
					if (isStaleTasksPageScope(requestScopeKey)) return;
					taskStore.selectTask(action.taskId);
					updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
					showSuccess('Execution started.');
				} else {
					throw new Error('Task is not in a state for planning or execution.');
				}
				} else if (action.action === 'doit_direct') {
					showInfo('Starting direct execution (no plan)...');
					await taskStore.executeTaskDirect(action.taskId);
					if (isStaleTasksPageScope(requestScopeKey)) return;
					taskStore.selectTask(action.taskId);
					updateFilterInUrl(activeFilter, action.taskId, activeTag || null);
					showSuccess('Direct execution started.');
			} else if (action.action === 'abort') {
				const abortTarget = resolveAbortTarget(task);
				if (!abortTarget) {
					throw new Error('Task has no active execution.');
				}
				const nextStatus = await taskStore.abortTask(action.taskId);
				if (isStaleTasksPageScope(requestScopeKey)) return;
				showSuccess(nextStatus === 'ready' ? 'Task aborted. Ready to re-execute.' : 'Task aborted.');
				} else if (action.action === 'cancel') {
					// Cancel via the task-status endpoint (works even when the task is
					// stuck in planning/clarification with a ghost execution). v0.6.938
					// resolves any pending clarification HITL as part of the transition.
					await taskStore.updateTaskStatus(action.taskId, 'cancelled');
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showSuccess('Task cancelled.');
				} else if (action.action === 'schedule_today') {
					const dueDate = todayIsoDate();
					await taskStore.updateTask(action.taskId, { dueDate });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo(`Due date set to ${dueDate}.`);
				} else if (action.action === 'schedule_tomorrow') {
					const dueDate = isoDateWithOffset(1);
					await taskStore.updateTask(action.taskId, { dueDate });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo(`Due date set to ${dueDate}.`);
				} else if (action.action === 'schedule_next_week') {
					const dueDate = isoDateWithOffset(7);
					await taskStore.updateTask(action.taskId, { dueDate });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo(`Due date set to ${dueDate}.`);
				} else if (action.action === 'schedule_clear') {
					await taskStore.updateTask(action.taskId, { dueDate: null });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo('Due date removed.');
				} else if (action.action === 'priority_clear') {
					await taskStore.updateTask(action.taskId, { priority: null });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo('Priority removed.');
				} else if (action.action === 'priority_p1') {
					const priority: TaskPriority = 'p1';
					await taskStore.updateTask(action.taskId, { priority });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo('Priority set to P1.');
				} else if (action.action === 'priority_p2') {
					const priority: TaskPriority = 'p2';
					await taskStore.updateTask(action.taskId, { priority });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo('Priority set to P2.');
				} else if (action.action === 'priority_p3') {
					const priority: TaskPriority = 'p3';
					await taskStore.updateTask(action.taskId, { priority });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo('Priority set to P3.');
				} else if (action.action === 'priority_p4') {
					const priority: TaskPriority = 'p4';
					await taskStore.updateTask(action.taskId, { priority });
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo('Priority set to P4.');
				} else if (action.action === 'delete' || action.action === 'menu_delete') {
					openDeleteTaskDialog(task);
					return;
				} else if (action.action === 'edit_description') {
					const result = await requestAttentionInput({
						title: 'Edit task description',
						body: task.title,
						kind: 'multiline',
						defaultValue: task.description || '',
						placeholder: 'Task description… (⌘/Ctrl+Enter to save)',
						confirmLabel: 'Save'
					});
					if (result && (result.kind === 'multiline' || result.kind === 'text') && result.value !== task.description) {
						await taskStore.updateTask(action.taskId, { description: result.value });
						if (isStaleTasksPageScope(requestScopeKey)) return;
						showSuccess('Description updated.');
					}
				} else if (action.action === 'reset') {
					const resetStatus = await taskStore.resetTaskToReady(action.taskId);
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo(resetStatus === 'ready' ? 'Task reset to ready.' : 'Task reset to pending.');
				} else if (action.action === 'publish_notes') {
					const note = await publishTaskToNotes(action.taskId);
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showSuccess(
						note.used_fallback
							? `Published to ${note.provider.replaceAll('_', ' ')} (fallback)`
							: 'Published to Notes'
					);
				}

				await refreshTasksData(false);
			} catch (error) {
				if (isStaleTasksPageScope(requestScopeKey)) return;
				const message = error instanceof Error ? error.message : 'Task action failed';
				pageError = message;
				showError(message);
				await refreshTasksData(false);
			} finally {
				if (isStaleTasksPageScope(requestScopeKey)) return;
				interactionBusy = false;
			}
		}

	async function handleTaskCompletionChange(change: ParsedTaskCompletionChange): Promise<void> {
		if (interactionBusy) {
			return;
		}
		const requestScopeKey = captureTasksPageScopeKey();
		interactionBusy = true;
		pageError = null;
		try {
			const task = $taskStore.tasks.find((next) => next.id === change.taskId);
			if (!task) {
				throw new Error('Task was not found');
			}
				if (change.checked) {
					if (task.status === 'running' || task.status === 'paused') {
						throw new Error('Cannot complete a running task.');
					}
					await taskStore.completeTask(change.taskId);
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showSuccess(`Marked "${task.title}" as complete.`);
				} else {
					await taskStore.uncompleteTask(change.taskId);
					if (isStaleTasksPageScope(requestScopeKey)) return;
					showInfo(`Moved "${task.title}" back to pending.`);
				}
			} catch (error) {
				if (isStaleTasksPageScope(requestScopeKey)) return;
				const message = error instanceof Error ? error.message : 'Failed to update task completion';
				pageError = message;
				showError(message);
			} finally {
				if (isStaleTasksPageScope(requestScopeKey)) return;
				interactionBusy = false;
			}
		}

	// ── Page-level ⋯ overflow menu (TaskCardMenu) ──
	// A filter/tag switch re-renders the surface, so an open menu's anchor
	// (and the inline editors' cards) may no longer exist — close them.
	// Runs once harmlessly on init (everything starts null).
	$: activeFilter, activeTag, closeAllMenus();
	// Menu-item props derived from the live task; the menu closes before any
	// action dispatch so surface re-renders can't strand it mid-flight.
	$: taskMenuTask = taskMenuTaskId
		? $taskStore.tasks.find((next) => next.id === taskMenuTaskId) ?? null
		: null;
	$: taskMenuCanEdit = taskMenuTask ? taskMenuTask.source === 'task' && taskMenuTask.readOnly !== true : false;
	$: taskMenuCanConvert = taskMenuTask ? canConvertTaskToMonitor(taskMenuTask) : false;
	$: taskMenuCanCancel =
		taskMenuCanEdit &&
		taskMenuTask != null &&
		!['completed', 'failed', 'cancelled'].includes(taskMenuTask.status);
	$: taskMenuPriority = taskMenuTask ? asString(taskMenuTask.priority).trim().toUpperCase() : '';
	$: taskMenuDueDateRaw = taskMenuTask ? asString(taskMenuTask.dueDate).trim().slice(0, 10) : '';

	async function setTaskDueDate(taskId: string, dueDate: string): Promise<void> {
		if (interactionBusy) return;
		interactionBusy = true;
		pageError = null;
		try {
			if (!/^\d{4}-\d{2}-\d{2}$/.test(dueDate)) {
				throw new Error('Choose a valid due date.');
			}
			await taskStore.updateTask(taskId, { dueDate });
			showInfo(`Due date set to ${dueDate}.`);
			await refreshTasksData(false);
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to set due date';
			pageError = message;
			showError(message);
		} finally {
			interactionBusy = false;
		}
	}

	async function handleTaskMenuSelect(event: CustomEvent<{ action: string }>): Promise<void> {
		const taskId = taskMenuTaskId;
		closeAllMenus();
		if (!taskId) return;
		await handleTaskAction({ taskId, action: event.detail.action as ParsedTaskAction['action'] });
	}

	async function handleTaskMenuDueDate(event: CustomEvent<{ value: string }>): Promise<void> {
		const taskId = taskMenuTaskId;
		closeAllMenus();
		if (!taskId) return;
		await setTaskDueDate(taskId, event.detail.value);
	}

	async function handleNativeScheduleSubmit(
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
		interactionBusy = true;
		pageError = null;
		closeAllMenus();
		activeScheduleEditorTaskId = null;
		try {
			const { taskId, values } = event.detail;
			const cronValue = asString(values.schedule_cron).trim();
			const tzValue = asString(values.schedule_timezone).trim();
			const retentionMaxRecords = asString(values.schedule_retention_max_records).trim();
			const retentionMaxDays = asString(values.schedule_retention_max_days).trim();
			if (!cronValue) {
				await taskStore.updateTask(taskId, { schedule: null });
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
				showInfo('Schedule updated.');
			}
			await refreshTasksData(false);
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to update schedule';
			pageError = message;
			showError(message);
		} finally {
			interactionBusy = false;
		}
	}

	/**
	 * Load the selected task's outputs. The request id is what makes a slow
	 * response for task A unable to land under task B — the guard is on the reply,
	 * not on the request, because a fetch already in flight cannot be recalled.
	 */
	async function loadPanelOutputs(taskId: string | null): Promise<void> {
		if (taskId === null) {
			panelOutputsRequestId += 1;
			panelOutputs = null;
			panelOutputsTaskId = null;
			return;
		}

		const requestId = ++panelOutputsRequestId;
		// Cleared first: until the reply lands, what is on screen belongs to the
		// task we just left, and the Output act must be absent rather than wrong.
		panelOutputs = null;
		panelOutputsTaskId = null;
		const scope = get(scopeIdentityStore);
		const files = await fetchTaskOutputFiles(taskId, scope.principal, scope.workspace);
		if (requestId !== panelOutputsRequestId) return;
		panelOutputs = files;
		panelOutputsTaskId = taskId;
	}

	/**
	 * The panel's own poll: the run behind the open task, and the task record the
	 * verdict is read off, on one clock.
	 *
	 * **Two reads per tick and not one**, because the panel's two halves come from
	 * two places: the Run act's timeline is in the `/execution-panel` payload, and
	 * every word of the verdict above it — the status, the step, the stall clock,
	 * the error — is on the task record. Refreshing only the first is the failure
	 * design §5 warns about wearing new clothes: live events streaming under a
	 * headline frozen at `step 4 of 7` does not read as stale, it reads as broken.
	 *
	 * The task record goes through the store rather than into a local copy, so the
	 * card behind the drawer advances with the panel over it and there is no second
	 * account of one task on one page.
	 *
	 * **Only the run read can fail the tick**, and the asymmetry is not laziness.
	 * This payload is the panel's own source and nothing else reports on it, so a
	 * failed read is what design §6's staleness line exists for. The task row has a
	 * reporter already: it is the *list's* row, the list refreshes it on the store's
	 * own backstop, and a failure there sets `taskStore.error`, which reaches this
	 * panel as `loadError` by a route that predates this poll. Failing the tick on
	 * it would double-report one fact and cost the timeline a refresh it could have
	 * had.
	 */
	const panelPoll = createTaskPanelPoll<ExecutionPanelState | null>({
		read: async (target: PanelPollTarget) => {
			const scope = get(scopeIdentityStore);
			const [, run] = await Promise.all([
				taskStore.refreshTask(target.taskId),
				// The reader's chosen run, or `null` for the task's current one. The
				// request omits the parameter entirely for `null`, so the default path is
				// byte-identical to the one this surface made before the picker existed.
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
		onSnapshot: (target: PanelPollTarget, state: ExecutionPanelState | null, at: number) => {
			// The last line of the id discipline this file keeps everywhere: the poll
			// drops a reply for a target it is no longer aimed at, and this drops one
			// for a task the reader has already left.
			if (target.taskId !== (selectedTask?.id ?? null)) return;
			panelRunState = state;
			panelRunStateTaskId = target.taskId;
			panelLoadedAt = at;
			panelLoadFailure = null;
		},
		onFailure: (target: PanelPollTarget, message: string) => {
			if (target.taskId !== (selectedTask?.id ?? null)) return;
			// Design §6, case 3. **What is on screen is kept and labelled**, never
			// blanked: a failed refresh is not evidence that the run stopped, and an
			// emptied Run act would be a claim about the task made out of a fact about
			// the network. `panelLoadedAt` is left where it was, so the line can say how
			// old what they are looking at actually is.
			panelLoadFailure = message;
		}
	});

	/**
	 * Forget the open task's run state, so nothing belonging to the task the reader
	 * just left renders under the one they opened.
	 *
	 * **Only on close and on a genuine task switch.** Clearing it per tick would
	 * empty the Run act's timeline and the verdict's ask four times a minute and
	 * refill them a round trip later, which is the flicker the poll exists to remove
	 * rather than to introduce.
	 */
	function clearPanelRunState(forgetSelection: boolean): void {
		panelRunState = null;
		panelRunStateTaskId = null;
		panelLoadedAt = null;
		panelLoadFailure = null;
		if (!forgetSelection) return;
		// The closed panel forgets which run was being read, so re-opening the same
		// task lands on its current run rather than on an attempt chosen minutes ago.
		panelRunSelectionId = null;
		panelRunSelectionTaskId = null;
	}

	// A task swap, or the panel closing. Both must drop the previous task's run
	// before the next reply lands; only the second forgets the chosen run, because
	// the first also fires on the very first load after a selection and clearing
	// there would discard the choice the request it is about to make was made for.
	$: if (panelRunStateTaskId !== null && panelRunStateTaskId !== (selectedTask?.id ?? null)) {
		clearPanelRunState(!isPanelOpen);
	}

	/**
	 * The reader picked a different run to read.
	 *
	 * **It records the choice and makes no request.** The chosen run is part of
	 * what the poll is aimed at, so recording it re-aims the poll — which reads
	 * that run immediately and keeps asking for that one until the reader picks
	 * another. That is also what makes the choice survive polling: the run is
	 * asked for by name every tick rather than re-derived from whatever the last
	 * reply happened to describe, so nothing can snap the Run act back to the
	 * newest attempt while an earlier one is being read.
	 *
	 * **The selection is never rolled back on a failure**, which is deliberate: it
	 * is what the reader asked for, and a failed request is a fact about the
	 * network rather than a reason to move the control back under their cursor.
	 * The failure surfaces as the staleness line instead.
	 *
	 * The task id is pinned here rather than in the poll, so a reply that lands
	 * after the reader has moved to another task cannot make that task's panel ask
	 * for this task's run.
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
	 * Read one output file, because the reader expanded its row.
	 *
	 * The `loading` record is written **before** the await, which is what makes
	 * the row say `Reading …` instead of holding the last file's contents for a
	 * round trip. Same request-id discipline as the two loaders above, and the
	 * same reason: a slow reply for one row must not land under another.
	 *
	 * The file arrives on the event rather than being looked up by index, because
	 * for a preview the row is enough — the address, the mime and the size are all
	 * on it. Open and reveal now use the same row object too, so a selected-run
	 * artifact can never be mistaken for the task-level row at the same index.
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

	/**
	 * Answer the ask blocking the open task.
	 *
	 * The panel renders the ask and collects the reader's answer; posting it is a
	 * request against a scope, which is this surface's job and not the panel's —
	 * the same split `previewFile` draws over a file's bytes.
	 *
	 * **Success is a re-read, never a local edit.** `answerTaskAsk` answering `ok`
	 * clears the in-flight state and refreshes the task and its run, and the ask
	 * then leaves the panel because the *server* stopped listing it. Nothing here
	 * removes it. On a failure the state stays, carrying the reason, and the panel
	 * renders the ask exactly as it was with the message under it — which is the
	 * only honest rendering of "you answered and it did not land".
	 *
	 * The `requestId` guard is the one every loader here carries: a slow reply for
	 * the ask the reader was looking at must not land on the one they are looking
	 * at now.
	 */
	async function handlePanelAnswer(
		event: CustomEvent<{ ask: HitlOpenTarget; result: AttentionPromptResult | null; cancel?: boolean }>
	): Promise<void> {
		const { ask, result, cancel } = event.detail;
		const requestId = ++panelAskRequestId;
		panelAsk = { id: ask.id, status: 'sending', message: null };

		const scope = get(scopeIdentityStore);
		const outcome = await answerTaskAsk(ask, result, scope, {}, { cancel: cancel === true });
		if (requestId !== panelAskRequestId) return;

		if (outcome.ok) {
			panelAsk = null;
			// One refresh, because the poll reads both places the answered ask could
			// have lived — the task row carries a plan clarification, the run payload a
			// mid-run block — and `refreshNow` reads them whatever the cadence. That
			// last part is not a detail: a task blocked on an ask can be one the panel
			// has stopped polling, which is exactly the case a cadence-gated refresh
			// would miss.
			panelPoll.refreshNow();
			return;
		}
		if (outcome.cancelled) {
			// Dismissed without answering. Nothing was posted and nothing changed,
			// so there is nothing to report — an error line here would tell the
			// reader their own gesture had failed.
			panelAsk = null;
			return;
		}
		panelAsk = { id: ask.id, status: 'failed', message: outcome.message };
		showError(outcome.message);
	}

	/**
	 * The controls the panel's header carries.
	 *
	 * The panel is modal over the list, so a task card's own actions are
	 * unreachable while it is open — a panel that could not stop a running task
	 * would be a step back from the one it replaces. They route through
	 * `handleTaskAction`, the same entry point the cards use, so the panel and a
	 * card cannot mean different things by `Stop`.
	 *
	 * Deliberately a **subset** of `NativeTasksSurface`'s ladder rather than a
	 * copy of it: status-keyed controls with none of the card-only editing
	 * branches. Pending work keeps Plan and Run-now distinct because collapsing
	 * them would change execution semantics.
	 */
	function panelActions(task: Task): Array<{ action: ParsedTaskAction['action']; label: string }> {
		if (task.status === 'running' || task.status === 'planning') {
			return [{ action: 'abort', label: 'Stop' }];
		}
		if (task.status === 'paused') {
			return task.executionId ? [{ action: 'reset', label: 'Reset to Ready' }] : [];
		}
		if (task.status === 'failed' || task.status === 'cancelled') {
			const actions: Array<{ action: ParsedTaskAction['action']; label: string }> = [];
			if (task.executionId) actions.push({ action: 'reset', label: 'Reset to Ready' });
			actions.push({ action: 'publish_notes', label: 'Publish to Notes' });
			return actions;
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
		if (task.status === 'completed') {
			return [{ action: 'publish_notes', label: 'Publish to Notes' }];
		}
		return [];
	}

	/**
	 * The panel's Retry control (design §6, case 2): reload the task, its outputs
	 * and the run behind it.
	 *
	 * Clearing the two keys is what happens in the meantime: what is on screen was
	 * loaded before a failure, so it is treated as unknown until the replies land
	 * rather than shown as current. Clearing `panelOutputsGuard` is what makes the
	 * outputs actually re-read — they are keyed on the task and its status, and a
	 * retry changes neither.
	 *
	 * `refreshNow` rather than waiting for the next tick, and it matters most in
	 * the case this control exists for: the states that park the poll — `failed`,
	 * `cancelled`, `completed` — are exactly the ones whose reader reaches for
	 * Retry, and a cadence-gated refresh would do nothing at all for them.
	 */
	async function handlePanelRetry(): Promise<void> {
		panelOutputsTaskId = null;
		panelOutputsGuard.key = '';
		panelRunStateTaskId = null;
		panelLoadFailure = null;
		panelPoll.refreshNow();
		await refreshTasksData(false);
	}

	function handleClosePanel(): void {
		userDeselected = true;
		isPanelOpen = false;
		panelPreferredAct = null;
		selectedTask = null;
		taskStore.selectTask(null);
		updateFilterInUrl(activeFilter, null, activeTag || null);
	}

	async function handleExecuteTask(event: CustomEvent<{ taskId: string }>): Promise<void> {
		await handleTaskAction({
			taskId: event.detail.taskId,
			action: 'doit'
		});
	}

	async function handleExecuteDirectTask(event: CustomEvent<{ taskId: string }>): Promise<void> {
		const { taskId } = event.detail;
		try {
			showInfo('Starting direct execution (no plan)...');
			await taskStore.executeTaskDirect(taskId);
			taskStore.selectTask(taskId);
			updateFilterInUrl(activeFilter, taskId, activeTag || null);
			showSuccess('Direct execution started.');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Failed to start direct execution');
		}
	}

	async function handleAbortTask(event: CustomEvent<{ taskId: string }>): Promise<void> {
		await handleTaskAction({
			taskId: event.detail.taskId,
			action: 'abort'
		});
	}

	async function handleResetToReadyTask(event: CustomEvent<{ taskId: string }>): Promise<void> {
		const { taskId } = event.detail;
		try {
			showInfo('Resetting task status...');
			const nextStatus = await taskStore.resetTaskToReady(taskId);
			taskStore.selectTask(taskId);
			updateFilterInUrl(activeFilter, taskId, activeTag || null);
			showSuccess(nextStatus === 'ready' ? 'Task reset to ready.' : 'Task reset to pending.');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Failed to reset task');
		}
	}

	async function handleEditDescription(event: CustomEvent<{ taskId: string; currentDescription: string }>): Promise<void> {
		const { taskId, currentDescription } = event.detail;
		const result = await requestAttentionInput({
			title: 'Edit task description',
			body: '',
			kind: 'multiline',
			defaultValue: currentDescription,
			placeholder: 'Task description… (⌘/Ctrl+Enter to save)',
			confirmLabel: 'Save'
		});
		if (!result || (result.kind !== 'multiline' && result.kind !== 'text')) return;
		if (result.value === currentDescription) return;
		try {
			await taskStore.updateTask(taskId, { description: result.value });
			showSuccess('Description updated.');
			await refreshTasksData(false);
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Failed to update description');
		}
	}

	onMount(async () => {
		if (!browser) {
			return;
		}
		tasksMounted = true;
		lastTasksScopeKey = tasksScopeKey();

		window.addEventListener('keydown', handleGlobalKeydown);
		panelNow = Date.now();
		panelClockHandle = setInterval(() => (panelNow = Date.now()), PANEL_CLOCK_INTERVAL_MS);
		taskStore.start();

		await refreshTasksData(true);
		tasksReady = true;
		taskStore.setFilter(activeFilter);

		if (!usesRouteNavigation) {
			lastInitialSelectedTaskId = initialSelectedTaskId;
			const initialTaskId = initialSelectedTaskId
				&& $taskStore.tasks.some((task) => task.id === initialSelectedTaskId)
				? initialSelectedTaskId
				: null;
			await taskStore.selectTask(initialTaskId);
		}

		// After tasks are loaded, re-trigger selectTask if one was pre-selected from URL.
		// The reactive block (line ~65) may have fired before tasks were loaded, causing
		// selectTask to skip plan fetching (no executionId found in empty task list).
		if (selectedTaskIdFromQuery && $taskStore.selectedTaskId === selectedTaskIdFromQuery) {
			taskStore.selectTask(selectedTaskIdFromQuery);
		}
	});

	onDestroy(() => {
		tasksMounted = false;
		tasksReady = false;
		if (browser) {
			window.removeEventListener('keydown', handleGlobalKeydown);
		}
		if (panelClockHandle !== null) {
			clearInterval(panelClockHandle);
			panelClockHandle = null;
		}
		// The panel's poll is parked by aiming it at `null` whenever the drawer
		// closes; this is the case that gesture cannot cover — the page going away
		// with the drawer still open.
		panelPoll.stop();
		taskStore.stop();
	});

	$: if (browser && tasksMounted && currentTasksScopeKey !== lastTasksScopeKey) {
		lastTasksScopeKey = currentTasksScopeKey;
		clearScopedTaskPageState();
		void clearScopedTaskSelectionFromUrl();
	}

	$: if (
		browser
		&& tasksReady
		&& !usesRouteNavigation
		&& initialSelectedTaskId !== lastInitialSelectedTaskId
	) {
		lastInitialSelectedTaskId = initialSelectedTaskId;
		const selectedId = initialSelectedTaskId
			&& $taskStore.tasks.some((task) => task.id === initialSelectedTaskId)
			? initialSelectedTaskId
			: null;
		void taskStore.selectTask(selectedId);
	}
</script>

<div
	class="tasks-page-layout tasks-page-layout--v5"
	class:tasks-page-layout--embedded={!usesRouteNavigation}
>
	<div class="tasks-page-main presto-spells-page presto-gaui-page">
		<div class="tasks-list-column">
		<TaskFilterToolbar
			activeFilter={activeFilter}
			activeTag={activeTag || null}
			counts={$taskCountsStore}
			tags={v5Tags}
			labels={FILTER_LABELS}
			totalCount={$taskStore.tasks.length}
			statusChips={computeStatusChips($taskStore.tasks)}
			on:filter={(event) => applyFilterClick(event.detail.value)}
			on:tag={(event) => {
				const tag = event.detail.value;
				if (tag) {
					taskStore.setFilter({ custom: tag });
					updateFilterInUrl(activeFilter, $taskStore.selectedTaskId, tag);
				} else {
					taskStore.setFilter(activeFilter);
					updateFilterInUrl(activeFilter, $taskStore.selectedTaskId, null);
				}
			}}
		/>
	<!-- Relative layer around the task list: the page-level TaskCardMenu is
	     positioned absolute in here (anchored under the card's ⋯ trigger),
	     so it scrolls with the list — no position:fixed hacks. -->
	<div class="tasks-surface-layer">
	<div class:presto-spells-compose-collapsed={!composeExpanded} class="native-task-compose presto-spells-compose-card">
		<button
			class:open={composeExpanded}
			class="native-task-compose-toggle"
			type="button"
			on:click={() => (composeExpanded = !composeExpanded)}
		>
			{composeExpanded ? 'Hide' : '+ New Task'}
		</button>
		{#if composeExpanded}
			<TaskCreateForm
				bind:title={createTitle}
				bind:outputMode={createOutputMode}
				bind:selectedAgentId
				bind:selectedThreadId
				bind:scheduleExpanded
				bind:scheduleCron
				bind:scheduleTimezone
				agentOptions={personalAgents}
				threadOptions={taskThreadOptions}
				{scheduleTimezoneOptions}
				defaultTimezone={deviceTimezone}
				disabled={interactionBusy}
				showDescriptionField={false}
				on:submit={(event) => createTask(event.detail.values)}
				on:clear={clearCreateTaskForm}
			>
				<!-- The description is the New-Task primary input and stays native so the
				     chip editor keeps focus across task polling/realtime updates. -->
				<div slot="description" class="presto-spells-mention-input">
					<div class="task-compose-desc-label">
						Describe what you want done — type <strong>@</strong> to reference a completed task
					</div>
					<MentionTextarea
						bind:this={mentionEl}
						class="task-mention-field"
						bind:value={mentionDescription}
						placeholder="Describe what you want done... Use @ to reference completed tasks"
						disabled={interactionBusy}
						minHeight={68}
						maxHeight={220}
						mentionItems={taskMentionItems}
						allowSpacesInQuery={true}
						bind:mentionOpen
						bind:mentionMatches
						bind:mentionActiveIndex
						on:input={refreshLinkedTaskIds}
					/>
					{#if mentionOpen}
						<div class="mention-dock">
							<MentionPicker
								matches={mentionMatches}
								activeIndex={mentionActiveIndex}
								on:select={(e) => mentionEl?.applyMention(e.detail)}
							/>
						</div>
					{/if}
				</div>
			</TaskCreateForm>
		{/if}
	</div>
	<!--
		**A second pager above the list**, which is what the internal-tasks tab on this
		route already has. On a full page the bottom one is below 25 cards, so paging
		from the top of the list means scrolling to a control you cannot see to get
		back to where you already were.

		Same condition as the bottom one, so the two appear and disappear together — a
		surface with one pager and not the other reads as a rendering fault. And the
		numbers come from `taskPagerProps`, so they cannot disagree.
	-->
	{#if taskPageTotal > TASKS_PAGE_SIZES[0]}
		<div class="tasks-pager tasks-pager--top">
			<!--
				**Shown from the smallest page size, not the current one.** With 30 tasks at
				a page size of 50 there is nothing to page — but lowering the size to 25
				creates two pages, so a selector gated on the pager's own condition would
				hide the one control that could produce it. The pager inside still comes and
				goes with the current size, and still comes and goes with the bottom one.
			-->
			<div class="tasks-pager__size">
				<Select
					label="Page size"
					value={String(TASKS_PAGE_SIZE)}
					options={TASKS_PAGE_SIZES.map((size) => ({ value: String(size), label: String(size) }))}
					interactive={true}
					on:change={(event) => setTasksPageSize(parseInt(event.detail.value, 10))}
				/>
			</div>
			{#if taskPageTotal > TASKS_PAGE_SIZE}
				<ServerPager
					{...taskPagerProps}
					ariaLabel="Task pages, above the list"
					loading={$taskStore.isLoading}
					on:pagechange={(event) => gotoTaskPage(event.detail.page)}
				/>
			{/if}
		</div>
	{/if}
	<NativeTasksSurface
		tasks={pagedTasks}
		selectedTaskId={$taskStore.selectedTaskId}
		isLoading={$taskStore.isLoading}
		{interactionBusy}
		{pageError}
		activeFilter={activeTag ? `tag:${activeTag}` : activeFilter}
		{activeTagEditorTaskId}
		{activeScheduleEditorTaskId}
		on:action={(event) => handleTaskAction(event.detail)}
		on:complete={(event) => handleTaskCompletionChange(event.detail)}
		on:tagAdd={(event) => handleAddTag(event.detail.taskId, event.detail.tagName)}
		on:tagRemove={(event) => handleRemoveTag(event.detail.taskId, event.detail.tagName)}
		on:toggleTagEditor={(event) => {
			const taskId = event.detail.taskId;
			const editorWasOpen = activeTagEditorTaskId === taskId;
			closeAllMenus();
			activeTagEditorTaskId = editorWasOpen ? null : taskId;
		}}
		on:toggleScheduleEditor={(event) => {
			const taskId = event.detail.taskId;
			const next = activeScheduleEditorTaskId === taskId ? null : taskId;
			closeAllMenus();
			activeScheduleEditorTaskId = next;
		}}
		on:scheduleSubmit={handleNativeScheduleSubmit}
		on:executionChanged={() => void taskStore.loadTasks()}
		on:openMenu={(event) => openNativeTaskMenu(event.detail.taskId, event.detail.anchor)}
		on:compose={() => (composeExpanded = true)}
	/>
	{#if taskPageTotal > TASKS_PAGE_SIZE}
		<div class="tasks-pager tasks-pager--bottom">
			<ServerPager
				{...taskPagerProps}
				ariaLabel="Task pages, below the list"
				loading={$taskStore.isLoading}
				on:pagechange={(event) => gotoTaskPage(event.detail.page)}
			/>
		</div>
	{/if}
	{#if taskMenuTask && taskMenuAnchor}
		<!-- Keyed on the task id: the menu manages position/anchor-ARIA in
		     onMount/onDestroy, so switching to another card's trigger without
		     an intervening close must remount it. -->
		{#key taskMenuTaskId}
			<TaskCardMenu
				anchor={taskMenuAnchor}
				taskTitle={taskMenuTask.title}
				canEditTask={taskMenuCanEdit}
				canCancel={taskMenuCanCancel}
				canConvertToMonitor={taskMenuCanConvert}
				priority={taskMenuPriority}
				dueDateRaw={taskMenuDueDateRaw}
				hasDueDate={taskMenuDueDateRaw !== ''}
				disabled={interactionBusy}
				on:select={handleTaskMenuSelect}
				on:dueDate={handleTaskMenuDueDate}
				on:close={closeAllMenus}
			/>
		{/key}
	{/if}
	</div>
		</div>
		{#if panelVisible}
			<!--
				The drawer chrome is `TaskPanelDrawer`'s, shared with every other
				surface that opens this panel. What is this workspace's is the action
				ladder below: because the drawer is modal over the list, a task card's
				own actions are unreachable while it is open, and a panel that could not
				stop a running task would be a step back from the one it replaces. They
				route through `handleTaskAction`, the same entry point the cards use, so
				the panel and a card cannot mean different things by `Stop`.
			-->
			<TaskPanelDrawer
				task={panelModel}
				title={selectedTask?.title ?? null}
				description={selectedTask?.description ?? null}
				threadId={selectedTask?.uiThreadId ?? 'general'}
				loadError={$taskStore.error ?? panelLoadFailure}
				lastLoadedAt={panelLoadedAt}
				now={panelNow}
				outputActions={true}
				filePreviews={true}
				filePreview={panelPreview}
				answerAsk={true}
				askState={panelAsk}
				closeOnEscape={escapeBelongsToPanel}
				preferredAct={panelPreferredAct}
				on:close={handleClosePanel}
				on:openFile={handlePanelOpenFile}
				on:revealFile={handlePanelRevealFile}
				on:previewFile={handlePanelPreviewFile}
				on:answer={handlePanelAnswer}
				on:retry={handlePanelRetry}
				on:selectRun={handlePanelSelectRun}
			>
				<svelte:fragment slot="actions">
					{#if selectedTask}
						{@const panelTask = selectedTask}
						{#each panelActions(panelTask) as action (action.action)}
							<button
								type="button"
								class="task-panel__action"
								disabled={interactionBusy}
								on:click={() =>
									void handleTaskAction({ taskId: panelTask.id, action: action.action })}
							>
								{action.label}
							</button>
						{/each}
						<ExportMenu taskId={panelTask.id} />
					{/if}
				</svelte:fragment>
			</TaskPanelDrawer>
		{/if}
	</div>
	<Modal
		open={convertTask !== null}
		title="Convert to monitor"
		size="lg"
		closable
		on:close={closeConvertToMonitor}
	>
		{#if convertTask && convertForm}
			<!-- The EXISTING monitor composer in convert mode: prefilled from
			     the task, review-before-activate, POST /monitors/{id}/convert.
			     The task keeps its id, schedule, and history. -->
			<MonitorComposer
				mode="convert"
				taskId={convertTask.id}
				form={convertForm}
				keptCadence={keptScheduleSummary(convertTask.schedule)}
				showHeader={false}
				on:close={closeConvertToMonitor}
				on:saved={handleConvertSaved}
			/>
		{/if}
	</Modal>
	<Modal
		open={pendingDeleteTask !== null}
		title="Delete task"
		size="md"
		closable={!deleteTaskSubmitting}
		on:close={closeDeleteTaskDialog}
	>
		{#if pendingDeleteTask}
			<div class="task-delete-content">
				<div class="task-delete-header">
					<p class="task-delete-eyebrow">Permanent cleanup option</p>
					<h3>{pendingDeleteTask.title}</h3>
					<p>
						This removes the task from the active task list. Keep the option selected to also remove
						its backing folder from task storage.
					</p>
				</div>

				<div class="task-delete-option">
					<Checkbox
						label="Remove task folder"
						checked={deleteTaskRemoveFiles}
						disabled={deleteTaskSubmitting}
						idBase={`delete-task-${pendingDeleteTask.id}`}
						on:change={(event) => (deleteTaskRemoveFiles = event.detail.checked)}
					/>
					<p>Deletes saved execution state, plans, and task outputs for this task.</p>
				</div>

				<div class="task-delete-actions">
					<Button
						label="Cancel"
						variant="outline"
						disabled={deleteTaskSubmitting}
						on:click={closeDeleteTaskDialog}
					/>
					<Button
						label={deleteTaskSubmitting ? 'Deleting...' : 'Delete task'}
						variant="primary"
						className="task-delete-danger"
						disabled={deleteTaskSubmitting}
						on:click={confirmDeleteTask}
					/>
				</div>
			</div>
		{/if}
	</Modal>
</div>

<style>
	.tasks-page-layout {
		display: flex;
		height: 100%;
		min-height: 0;
		overflow: hidden;
	}

	.tasks-page-layout--embedded {
		width: 100%;
		min-height: 100%;
	}

	.tasks-page-layout--embedded .tasks-page-main {
		width: 100%;
		max-width: none;
		margin: 0;
	}

	/* `display: flex` on the layout gives `.tasks-page-main` a proper height
	   via `flex: 1` so its `overflow-y: auto` actually scrolls. There is no
	   side rail anymore (the legacy TaskFilterBar was removed in the v5-only
	   migration), so the single main column just flex-fills on its own. */
	.tasks-page-main {
		flex: 1;
		min-width: 0;
		max-width: 1320px;
		margin: 0 auto;
		overflow-y: auto;
	}

	.tasks-page-layout--v5 .tasks-page-main {
		/* Centered, capped width — full-bleed felt sparse on wide displays. */
		max-width: var(--app-content-max, 1320px);
		/* Drop top padding so the sticky toolbar sits flush at the scroll viewport
		   top — otherwise the 1.35rem padding-top from .presto-gaui-page becomes
		   a transparent gap above the toolbar where scrolled content bleeds through. */
		padding-top: 0;
		padding-bottom: var(--attention-bar-offset, 0px);
	}

	.tasks-list-column {
		display: contents;
	}

	/* Positioning context for the page-level TaskCardMenu (absolute within
	   this layer → the open menu scrolls with the task list). */
	.tasks-surface-layer {
		position: relative;
	}

	.native-task-compose {
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

	.native-task-compose.presto-spells-compose-collapsed {
		align-items: flex-start;
		padding: 0.65rem 0.8rem;
	}

	.native-task-compose-toggle {
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

	.native-task-compose-toggle.open {
		border-color: var(--border-soft, #d8d2c8);
		background: transparent;
		color: var(--text-secondary, #6b7280);
	}

	/* The description block now lives inside the compose panel (just before the
	   title), so it inherits the card's padding — no page padding here. */
	.presto-spells-mention-input {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		margin-bottom: 0.6rem;
		scroll-margin-top: 64px;
	}

	/* The description is the PRIMARY New-Task input (title is secondary) — label it
	   so the @-reference affordance is obvious. */
	.task-compose-desc-label {
		font-size: 0.74rem;
		font-weight: 700;
		letter-spacing: 0.01em;
		color: var(--text-secondary, #6b7280);
	}

	.task-compose-desc-label strong {
		color: var(--accent-primary, #e85d5d);
		font-weight: 800;
	}

	/* The standardized MentionTextarea, styled to match the old bordered task
	   textarea. The `class` lands on the outer `.chip-textarea-wrap`; the
	   contenteditable + its placeholder live inside, so reach them via :global and
	   keep the placeholder aligned with the padded text. */
	.presto-spells-mention-input :global(.task-mention-field) {
		width: 100%;
		box-sizing: border-box;
		border-radius: var(--radius-md);
		border: 1px solid var(--input-border, var(--border-soft));
		background: var(--input-bg, var(--bg-card));
		transition:
			border-color 0.15s ease,
			box-shadow 0.15s ease,
			background 0.15s ease;
	}

	.presto-spells-mention-input :global(.task-mention-field:focus-within) {
		background: var(--input-focus-bg, var(--bg-card));
		border-color: var(--input-focus-border, var(--accent-primary));
		box-shadow: var(--input-focus-shadow, none);
	}

	.presto-spells-mention-input :global(.task-mention-field .chip-textarea) {
		padding: 8px 10px;
	}

	.presto-spells-mention-input :global(.task-mention-field .chip-textarea__placeholder) {
		top: 8px;
		left: 10px;
	}

	.presto-spells-mention-input .mention-dock {
		margin-top: 4px;
	}

	.task-delete-content {
		display: grid;
		gap: 1rem;
		color: var(--text-body, var(--text-secondary, #4b5563));
	}

	.task-delete-eyebrow {
		margin: 0 0 0.35rem;
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.09em;
		text-transform: uppercase;
		color: var(--color-error, var(--accent-primary, #e85d5d));
	}

	.task-delete-header h3 {
		margin: 0;
		font-size: 1.1rem;
		line-height: 1.2;
		color: var(--text-primary, #111827);
	}

	.task-delete-header p {
		margin: 0.6rem 0 0;
		color: var(--text-secondary, #6b7280);
		line-height: 1.5;
	}

	.task-delete-option {
		display: grid;
		gap: 0.45rem;
		padding: 0.85rem;
		border: 1px solid color-mix(in srgb, var(--color-error, #e85d5d) 24%, var(--border-soft, #e5e7eb));
		border-radius: var(--radius-md, 0.75rem);
		background: color-mix(in srgb, var(--color-error-soft, rgba(255, 107, 107, 0.14)) 72%, var(--bg-card, #fff));
	}

	.task-delete-option p {
		margin: 0 0 0 1.55rem;
		font-size: 0.78rem;
		color: var(--text-muted, var(--text-secondary, #6b7280));
		line-height: 1.4;
	}

	.task-delete-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.65rem;
	}

	:global(.task-delete-danger.muij-button) {
		background: var(--color-error, #e85d5d);
		color: var(--text-on-accent, #fff);
		border-color: var(--color-error, #e85d5d);
	}

	:global(.task-delete-danger.muij-button:hover:not(:disabled)) {
		background: color-mix(in srgb, var(--color-error, #e85d5d) 88%, black);
	}
	/* Layout only — the control itself is ServerPager, shared with the
	   internal-tasks table so both tabs on this route render one pager. */
	/* Right-aligned, matching the internal-tasks tab on this same route — which
	   already was, so centring here was the last visible way the two lanes drew the
	   same control differently. `4ae31574a` made them the same *component*; this makes
	   them sit in the same place. */
	.tasks-pager {
		display: flex;
		align-items: center;
		justify-content: flex-end;
	}

	/* Asymmetric on purpose. The top one separates the toolbar from the list; the
	   bottom one closes the list and needs no gap under it. The internal tab's pair
	   does the same thing with its own `--bottom` nudge. */
	.tasks-pager--top {
		gap: 12px;
		padding: 4px 0 8px;
	}

	/* **`native/Select.svelte`, not a bare `<select>`.** The two lanes on this route
	   drew three different controls between them: this one unstyled, the internal
	   tab's hand-rolled at 13px/6px/radius-6, and the component the panel's own run
	   picker uses. Numbers alone could never have converged them — the component also
	   carries per-theme treatments neither hand-rolled copy knew about, and
	   `retro-16bit` squares its corners where a literal `border-radius: 6px` cannot.

	   `:global` because the component owns those classes. Row rather than the
	   component's default column, so the label sits beside the control instead of
	   above it — the label is two words and stacking it doubles the row's height for
	   nothing. */
	.tasks-pager__size :global(.native-select) {
		flex-direction: row;
		align-items: center;
		gap: 8px;
	}

	.tasks-pager__size :global(.native-select__label) {
		white-space: nowrap;
	}

	.tasks-pager--bottom {
		padding: 10px 0 4px;
	}
	/* ── the drawer's action ladder ───────────────────────────────────────
	   The scrim, the dialog, the header and the body are `TaskPanelDrawer`'s.
	   What stays here is what only this surface has: the actions it slots into
	   that header, styled beside the cards they mirror. */
	/*
	 * `.task-panel__action` is styled by `TaskPanelDrawer`, not here. The buttons
	 * are this workspace's — the shell has no idea what `Reset to Ready` means —
	 * but the row they sit in is the shell's header, and five more surfaces are
	 * queued onto that header. One rule there beats a copy per surface.
	 */

</style>
