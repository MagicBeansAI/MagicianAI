<script lang="ts">
	/**
	 * CommandPalette — global ⌘K surface for navigation, threads, tasks,
	 * sessions, workspace switching, and one-shot actions.
	 *
	 * Sections (in order):
	 *   • Jump to       — primary destinations (Today / Square / Chat / Tasks)
	 *   • Actions       — verbs that work from any context (New task, Open Debug)
	 *   • Manage        — every Control-Panel destination
	 *   • Apps          — installed apps + views
	 *   • Threads       — current threadStore + "New thread" action
	 *   • Recent sessions — last 5 from chatStore.sessions
	 *   • Recent tasks    — last 5 from taskStore.tasks
	 *
	 * Built on bits-ui's `Command` primitive — fuzzy filter, keyboard nav,
	 * a11y, type-ahead and empty/group hiding are delegated to the library.
	 * This file keeps the data wiring (stores → items) and overlay coordinator
	 * integration; the rest is markup.
	 *
	 * Wires to overlayCoordinator at priority `commandPalette` (6).
	 * Higher-priority overlays close it automatically.
	 */
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onDestroy, tick } from 'svelte';
	import { fly, fade } from 'svelte/transition';
	import { Command } from 'bits-ui';

	import { openHitlPrompt } from '$lib/attention';
	import { chatStore } from '$lib/stores/chatStore';
	import { approveTask, taskStore } from '$lib/stores/taskStore';
	import type { Task } from '$lib/stores/taskStore';
	import { INTERNAL_TASKS_ROUTE } from '$lib/magician/tasks/taskRoutes';
	import { threadStore } from '$lib/stores/threadStore';
	import { attentionStore } from '$lib/stores/attentionStore';
	import {
		approvalHitlOpenTarget,
		loadApprovals,
		pendingApprovals,
		type ApprovalSummary
	} from '$lib/stores/approvalStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { agentList, loadAgents } from '$lib/stores/agentStore';
	import { refreshNotesSettings } from '$lib/stores/notesSettingsStore';
	import {
		commandFrequencyStore,
		frecencyScore
	} from '$lib/stores/commandFrequencyStore';
	import {
		OVERLAY_PRIORITIES,
		release,
		requestFocus
	} from '$lib/shell/overlayCoordinator';
	import { toggleEventsConsole } from '$lib/stores/eventsConsoleStore';
	import { commandFilter } from '$lib/shell/commandMatch';
	import { showSuccess, showError } from '$lib/shared/stores/notifications';
	import { SOTA_FIXTURES, sotaDisplayName, sotaGoalForFixture } from '$lib/data/sotaFixtures';
	import { timedFetch } from '$lib/shared/fetch';
	import {
		MAX_STEER_MESSAGE_BYTES,
		applyExecutionControl,
		executionControlInvalidations,
		getExecutionControlState,
		steerMessageByteLength,
		type ExecutionControlState
	} from '$lib/magician/execution/controlClient';
	import {
		fetchAppDirectory,
		recordAppDirectoryLaunch,
		type AppDirectoryEntry
	} from '$lib/apps/appDirectory';
	// Gate S3: an app's declared first-party navigation is a destination, not a
	// launcher row, so it joins "Jump to" beside the shell's own destinations
	// rather than the "Apps" section that lists views of an installation.
	import type { AppMountedNavigation } from '$lib/apps/appNavigation';
	import { appNavigationEntries } from '$lib/stores/appNavigationStore';

	export let open = false;

	const COORDINATOR_ID = 'command-palette';

	// Internal agent threads that should never surface in the UI thread list.
	// They got created as side-effects of agent runtime activity and aren't
	// user-facing conversations.
	const HIDDEN_THREAD_IDS = new Set(['agent-personal-assistant', 'system-meta-agent']);

	let query = '';
	let appDirectoryEntries: AppDirectoryEntry[] = [];
	let appDirectoryRequest = 0;
	let appDirectorySearchTimer: ReturnType<typeof setTimeout> | null = null;
	let appliedAppDirectoryScopeKey = '';

	$: sessions = $chatStore.sessions ?? [];
	$: tasks = $taskStore.tasks ?? [];
	$: threads = ($threadStore.threads ?? []).filter((t) => !HIDDEN_THREAD_IDS.has(t.id));
	$: attentionCounts = $attentionStore.counts;
	$: approvals = $pendingApprovals ?? [];
	$: scope = $scopeIdentityStore;
	$: appDirectoryScopeKey = JSON.stringify([scope.principal, scope.workspace]);

	$: currentPath = $page.url.pathname;
	$: activeSessionId = $chatStore.activeSessionId;

	async function openApprovalPrompt(approval: ApprovalSummary): Promise<void> {
		const result = await openHitlPrompt(approvalHitlOpenTarget(approval));
		if (result.status === 'error') showError(result.error);
	}

	// Sub-page state — palette can step into focused flows (new-task,
	// send-message, pick-target, schedule-task, loop-task, task-action)
	// while staying open. Escape pops back to root; submit / pick closes
	// (or transitions to next page).
	type PalettePage =
		| 'root'
		| 'new-task'
		| 'send-message'
		| 'pick-target'
		| 'schedule-task'
		| 'loop-task'
		| 'task-action'
		| 'steer-task'
		| 'sota-list'
		| 'browser-flow'
		| 'debug-flow'
		| 'delegate-pick-agent'
		| 'delegate-instruct';
	let palettePage: PalettePage = 'root';
	let targetSessionId: string | null = null;
	let targetThreadId: string | null = null;
	let actionTaskId: string | null = null;
	let actionControlState: ExecutionControlState | null = null;
	let actionControlExecutionId: string | null = null;
	let actionControlLoadedRevision = -1;
	let actionControlLoadId = 0;
	let delegateAgentId: string | null = null;
	let submitting = false;
	$: agents = $agentList ?? [];
	// The user's primary Personal agent — used as the default owner for
	// normal task-creation flows (New task / Create task inline /
	// Schedule / Loop). Falls back to 'personal-assistant' if the agent
	// list hasn't loaded yet, which matches the backend default.
	$: primaryAgentId =
		agents.find((agent) => agent.is_primary)?.agent_id ?? 'personal-assistant';
	// Max-runs selection for schedule-task / loop-task sub-pages.
	// `null` = no limit (indefinite). Resets when the sub-page closes.
	let scheduleMaxRuns: number | null = null;
	const MAX_RUNS_OPTIONS: Array<number | null> = [null, 1, 3, 5, 10, 25];

	// Cron presets for the Schedule / Loop sub-pages. Calendar-style entries
	// live in SCHEDULE_PRESETS (fire at named times). Interval entries live
	// in LOOP_PRESETS (fire repeatedly at fixed cadence). Both reduce to a
	// task with `schedule: { cron, timezone }`.
	const SCHEDULE_PRESETS: Array<{ label: string; cron: string }> = [
		{ label: 'Every day at 9:00 AM', cron: '0 9 * * *' },
		{ label: 'Every day at 6:00 PM', cron: '0 18 * * *' },
		{ label: 'Every weekday at 9:00 AM', cron: '0 9 * * 1-5' },
		{ label: 'Every Monday at 9:00 AM', cron: '0 9 * * 1' },
		{ label: 'Every Friday at 5:00 PM', cron: '0 17 * * 5' },
		{ label: 'Every Sunday at 9:00 AM', cron: '0 9 * * 0' },
		{ label: 'Every day at midnight', cron: '0 0 * * *' },
		{ label: 'First of every month at 9:00 AM', cron: '0 9 1 * *' }
	];

	const LOOP_PRESETS: Array<{ label: string; cron: string }> = [
		{ label: 'Every 5 minutes', cron: '*/5 * * * *' },
		{ label: 'Every 15 minutes', cron: '*/15 * * * *' },
		{ label: 'Every 30 minutes', cron: '*/30 * * * *' },
		{ label: 'Every hour', cron: '0 * * * *' },
		{ label: 'Every 2 hours', cron: '0 */2 * * *' },
		{ label: 'Every 4 hours', cron: '0 */4 * * *' },
		{ label: 'Every 6 hours', cron: '0 */6 * * *' },
		{ label: 'Every 12 hours', cron: '0 */12 * * *' }
	];

	function localTimezone(): string {
		try {
			return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
		} catch {
			return 'UTC';
		}
	}

	// Short "23s ago" / "5m ago" / "2h ago" / "3d ago" formatter for the
	// Recent executions hint. Uses elapsed time relative to now.
	function relativeTimeFrom(epochMs: number): string {
		const elapsed = Math.max(0, Date.now() - epochMs);
		const s = Math.floor(elapsed / 1000);
		if (s < 60) return `${s}s ago`;
		const m = Math.floor(s / 60);
		if (m < 60) return `${m}m ago`;
		const h = Math.floor(m / 60);
		if (h < 24) return `${h}h ago`;
		const d = Math.floor(h / 24);
		return `${d}d ago`;
	}

	interface CommandItem {
		id: string;
		section: string;
		label: string;
		hint?: string;
		kbd?: string;
		icon?: string;
		/** Underlying entity id (task_*, sess_*, thread name) — included
		 *  in the bits-ui search value so users can fuzzy-match by ID. */
		entityId?: string;
		/** When set, frecency tracking records this id instead of `id`.
		 *  Used for the "Recent" section so duplicated items still
		 *  contribute to their original command's frecency counter. */
		recordAs?: string;
		/** When true, the palette stays open after activation — used for
		 *  actions that step into a sub-page rather than navigate away. */
		keepOpen?: boolean;
		run: () => void | Promise<void>;
	}

	function attentionHint(): string | undefined {
		const pending =
			(attentionCounts?.requests ?? 0) +
			(attentionCounts?.approvals ?? 0) +
			(attentionCounts?.escalations ?? 0);
		return pending > 0 ? `${pending} pending` : undefined;
	}

	// Keep Apps as an explicit reactive input. Svelte cannot infer dependencies
	// that are read only inside an indirectly called helper, so a zero-argument
	// builder left newly fetched directory entries invisible until some unrelated
	// palette state happened to invalidate the item list.
	$: items = buildItems(appDirectoryEntries, $appNavigationEntries);

	// Keyboard shortcut convention — keep all new entries on this scheme:
	//   • `G <letter>` — Go to: pure navigation, no side effects
	//   • `N <letter>` — New / Create / Delegate: opens compose or sub-page
	//   • `⌘ <key>`    — Global modal toggles only (palette, quick-chat, events console)
	// Slash-style commands (`/loop`, `/schedule`) belong in chat input, never
	// the palette — the palette IS the command surface; typing the name
	// (`loop`, `schedule`) fuzzy-matches the corresponding action.
	function buildItems(directoryEntries: AppDirectoryEntry[], declaredNavigation: AppMountedNavigation[]): CommandItem[] {
		const result: CommandItem[] = [];

		// Jump to
		result.push(
			{ id: 'go-today', section: 'Jump to', label: 'Today', hint: 'morning edition newspaper & swipe triage', kbd: 'G D', icon: '📰', run: () => goto('/today') },
			{ id: 'go-square', section: 'Jump to', label: 'Square', hint: 'agent office + social feed', icon: '🏛️', run: () => goto('/square') },
			{ id: 'go-chat', section: 'Jump to', label: 'Chat', kbd: 'G C', icon: '◇', run: () => goto('/chat') },
			{ id: 'go-tasks', section: 'Jump to', label: 'Tasks', kbd: 'G T', icon: '◷', run: () => goto('/tasks') },
			{ id: 'go-monitors', section: 'Jump to', label: 'Open monitors', hint: 'recurring watches + change history', icon: '◉', run: () => goto('/tasks?type=monitors') },
			{ id: 'go-observe', section: 'Jump to', label: 'Observe', hint: 'meetings + screen capture', icon: '◉', run: () => goto('/observe') },
			{ id: 'go-claims-review', section: 'Jump to', label: 'Claims Review', hint: 'review Envoy replies and external statements', icon: '✓', run: () => goto('/claims-review') },
			{ id: 'go-thinking-maps', section: 'Jump to', label: 'Thinking Maps', hint: 'live brainstorm boards — build as you talk', icon: '❖', run: () => goto('/thinking-maps') },
			{ id: 'go-evidence', section: 'Jump to', label: 'Evidence inbox', hint: 'review + correct captured work evidence', icon: '⊡', run: () => goto('/evidence') },
			{ id: 'go-evals', section: 'Jump to', label: 'Evals', hint: 'every eval lane: readiness, last result, cost', icon: '⚖', run: () => goto('/evals') },
			{ id: 'go-llm', section: 'Jump to', label: 'LLM observability', hint: 'live events + cost / latency', kbd: 'G L', icon: '◍', run: () => goto('/llm') },
			{ id: 'go-llm-queue', section: 'Jump to', label: 'LLM dispatch queue', hint: 'workers, retries, cancellations', kbd: 'G Q', icon: '⇋', run: () => goto('/llm/queue') },
			{ id: 'go-resurfacing', section: 'Jump to', label: 'Resurfacing', hint: 'what the resurfacing engine is doing', icon: '♻', run: () => goto('/resurfacing') },
			{ id: 'go-runtime-activity', section: 'Jump to', label: 'Runtime activity', hint: 'every span in flight: agent, background, llm, process', icon: '◈', run: () => goto('/runtime') },
			{ id: 'go-local-resources', section: 'Jump to', label: 'Local resources', hint: 'observe-only governor', icon: '▤', run: () => goto('/runtime/resources') },
			{ id: 'go-storage', section: 'Jump to', label: 'Storage', hint: 'sizes, retention + safe compaction', icon: '▥', run: () => goto('/storage') },
			{ id: 'go-storage-activation', section: 'Jump to', label: 'Storage activation', hint: 'readiness-qualified inventory and status only', icon: '▥', run: () => goto('/storage#activation') },
			{ id: 'go-warroom', section: 'Jump to', label: 'Warroom', hint: 'demo HUD', kbd: 'G W', icon: '◎', run: () => goto('/warroom') }
		);

		// Admitted app-declared destinations. `mountAppNavigation` has already
		// dropped anything from a non-enabled installation, anything rooted at a
		// first-party path, and any route two installations both claimed, so the
		// palette renders the list rather than re-deciding it.
		declaredNavigation.forEach((declared) => {
			result.push({
				id: `app-navigation-${declared.installation_id}-${declared.entry.id}`,
				section: 'Jump to',
				label: declared.entry.title,
				hint: 'app destination',
				icon: '◫',
				entityId: declared.installation_id,
				run: () => goto(declared.href)
			});
		});

		// App names and declared views are metadata-only launcher entries. App
		// records are intentionally absent; authorized record discovery belongs
		// to the separate app_data_search path.
		directoryEntries.forEach((entry) => {
			if (!entry.default_route || entry.status !== 'enabled') return;
			result.push({
				id: `app-${entry.installation_id}`,
				section: 'Apps',
				label: entry.name,
				hint: entry.description,
				icon: entry.icon.value,
				entityId: entry.installation_id,
				run: async () => {
					const defaultView = entry.views.find((view) => view.route === entry.default_route);
					if (defaultView) {
						void recordAppDirectoryLaunch(entry.installation_id, defaultView.view_id).catch(() => undefined);
					}
					await goto(entry.default_route!);
				}
			});
			entry.views.filter((view) => view.route !== entry.default_route).forEach((view) => {
				result.push({
					id: `app-view-${entry.installation_id}-${view.view_id}`,
					section: 'Apps',
					label: `${entry.name} · ${view.label}`,
					hint: 'App view',
					icon: '↗',
					entityId: entry.installation_id,
					run: async () => {
						void recordAppDirectoryLaunch(entry.installation_id, view.view_id).catch(() => undefined);
						await goto(view.route);
					}
				});
			});
		});

		// Threads — `#general` is the default thread and is always present,
		// even if the thread store hasn't materialised a row. All threads
		// (including general) navigate to /t/<id> to match TodoSidebar.
		const hasGeneral = threads.some((t) => t.id === 'general');
		if (!hasGeneral) {
			result.push({
				id: 'thread-general',
				section: 'Threads',
				label: 'general',
				icon: '#',
				run: () => goto('/t/general')
			});
		}
		threads.forEach((thread) => {
			result.push({
				id: `thread-${thread.id}`,
				section: 'Threads',
				label: thread.name || thread.id,
				icon: '#',
				entityId: thread.id,
				run: () => goto(`/t/${encodeURIComponent(thread.id)}`)
			});
		});
		result.push({
			id: 'new-thread',
			section: 'Threads',
			label: 'New thread',
			kbd: 'N H',
			icon: '+',
			run: () => goto('/t/new')
		});

		// Needs attention — pending approvals + paused/failed tasks. These
		// items are surfaced as their own section AND get a baseline boost
		// in `contextScore` so they bubble into Suggested whenever anything
		// is awaiting the user. Empty section auto-hides via SECTION_ORDER.
		approvals.slice(0, 5).forEach((approval) => {
			const count = approval.pending_action_count;
			result.push({
				id: `approval-${approval.approval_id}`,
				section: 'Needs attention',
				label: `Approval — ${approval.agent_id}`,
				icon: '⚠',
				hint: count > 0 ? `${count} action${count === 1 ? '' : 's'} pending` : approval.status,
				entityId: approval.approval_id,
				run: () => void openApprovalPrompt(approval)
			});
		});
		tasks
			.filter((t) => t.status === 'failed' || t.status === 'paused')
			.slice(0, 5)
			.forEach((task) => {
				result.push({
					id: `attention-task-${task.id}`,
					section: 'Needs attention',
					label: task.title || task.id,
					icon: task.status === 'failed' ? '✗' : '⏸',
					hint: task.status,
					entityId: task.id,
					keepOpen: true,
					run: () => openPage('task-action', { taskId: task.id })
				});
			});

		// Manage commands. Shortcuts follow the `G <letter>` convention
		// (G = "Go to") for route destinations; high-frequency surfaces get a kbd,
		// niche ones (Vault, Debug, Channels, Budget, Mirror) stay
		// search-only. The Debug page is reachable here for live execution
		// monitoring; the actual dispatch verbs live in Actions
		// (`SOTA…`, `Browser…`).
		const manage: Array<[string, string, string, string?]> = [
			['Attention', '/attention', '✅', 'G A'],
			// Long-tail destinations demoted from "Jump to": reachable by
			// search here, while the top of the palette stays action-first.
			['Briefing', '/briefing', '⊟'],
			['Apps', '/apps', '◫', 'G P'],
			['VibeDev', '/vibe', '▣', 'G V'],
			['VibeDev workbench', '/vibe?tab=workbench', '⌨'],
			['Reviews', '/reviews', '✦'],
			['LLM observability', '/llm', '◍', 'G L'],
			['LLM dispatch queue', '/llm/queue', '⇋', 'G Q'],
			['Resurfacing', '/resurfacing', '♻'],
			['Runtime activity', '/runtime', '◈'],
			['Local resources', '/runtime/resources', '▤'],
			['Storage', '/storage', '▥'],
			['Notes', '/notes', '♫'],
			['Crew', '/crew', '👥', 'G U'],
			['Vault', '/vault', '🔑'],
			['Memory', '/memory', '🧠', 'G M'],
			['Triggers', '/triggers', '⚡', 'G R'],
			['API Mining', '/api-mining', '🔧', 'G I'],
			['Skills', '/skills', '🧩', 'G K'],
			['Skill evolution', '/skills/evolution', '🌱'],
			['Channels', '/channels', '📡'],
			['Budget', '/budget', '💰'],
			['Dev sessions', '/devsessions', '⌨'],
			['Debug', '/debug', '🐞'],
			['Voice debug', '/debug/voice', '◉'],
			['Notify debug', '/debug/notify', '🔔'],
			['Internal Tasks', INTERNAL_TASKS_ROUTE, '🗂'],
			['Settings', '/settings', '⚙', 'G ;'],
			['Events', '/events', '⟿', 'G E'],
			['Feed', '/feed', '✦', 'G F'],
			['History', '/history', '◷', 'G H'],
			['Mirror', '/mirror', '◈']
		];
		manage.forEach(([label, href, icon, kbd]) => {
			const item: CommandItem = {
				id: `manage-${href}`,
				section: 'Manage',
				label,
				icon,
				run: () => goto(href)
			};
			if (kbd) item.kbd = kbd;
			if (label === 'Attention') {
				const hint = attentionHint();
				if (hint) item.hint = hint;
			}
			result.push(item);
		});

		// Recent sessions
		sessions.slice(0, 5).forEach((session) => {
			result.push({
				id: `session-${session.id}`,
				section: 'Recent sessions',
				label: session.title || session.id,
				icon: '◉',
				hint: session.ui_thread_id ? `#${session.ui_thread_id}` : undefined,
				entityId: session.id,
				run: () => {
					const threadId = session.ui_thread_id;
					if (threadId && threadId !== 'general') {
						goto(`/t/${encodeURIComponent(threadId)}?session=${encodeURIComponent(session.id)}`);
					} else {
						goto(`/chat?session=${encodeURIComponent(session.id)}`);
					}
				}
			});
		});

		// Recent tasks — Enter opens the task-action sub-page (which has
		// "Open task" as its first item so the legacy single-press
		// navigation still works in two presses).
		tasks.slice(0, 5).forEach((task) => {
			result.push({
				id: `task-${task.id}`,
				section: 'Recent tasks',
				label: task.title || task.id,
				icon: '◷',
				hint: task.status,
				entityId: task.id,
				keepOpen: true,
				run: () => openPage('task-action', { taskId: task.id })
			});
		});

		// Recent executions — flatten executionHistory across all tasks,
		// take the latest 10 by start time. Useful when one task has run
		// many times (e.g., recurring) and the user wants to jump to a
		// specific past run.
		const allExecutions: Array<{
			taskId: string;
			taskTitle: string;
			executionId: string;
			startedAt: number;
			status: string;
		}> = [];
		for (const task of tasks) {
			if (!task.executionHistory) continue;
			for (const exec of task.executionHistory) {
				allExecutions.push({
					taskId: task.id,
					taskTitle: task.title || task.id,
					executionId: exec.execution_id,
					startedAt: exec.started_at,
					status: exec.status
				});
			}
		}
		allExecutions
			.sort((a, b) => b.startedAt - a.startedAt)
			.slice(0, 10)
			.forEach((exec) => {
				const when = relativeTimeFrom(exec.startedAt);
				result.push({
					id: `exec-${exec.executionId}`,
					section: 'Recent executions',
					label: exec.taskTitle,
					icon: '▷',
					hint: `${exec.status} · ${when}`,
					entityId: exec.executionId,
					run: () => goto(`/tasks?selected=${encodeURIComponent(exec.taskId)}`)
				});
			});

		// Actions
		result.push(
			{
				id: 'action-new-task',
				section: 'Actions',
				label: 'New task',
				hint: 'open compose form',
				kbd: 'N T',
				icon: '+',
				run: () => goto('/tasks?compose=1')
			},
			{
				id: 'action-create-task-inline',
				section: 'Actions',
				label: 'Create task…',
				hint: 'inline title, no form',
				kbd: 'N I',
				icon: '✎',
				keepOpen: true,
				run: () => openPage('new-task')
			},
			{
				id: 'action-create-monitor',
				section: 'Actions',
				label: 'Create monitor',
				hint: 'watch a page/site on a schedule',
				kbd: 'N M',
				icon: '◉',
				run: () => goto('/tasks?type=monitors&compose=1')
			},
			{
				id: 'action-send-message',
				section: 'Actions',
				label: 'Chat…',
				hint: activeSessionId ? 'active session' : 'pick a target',
				kbd: 'N C',
				icon: '→',
				keepOpen: true,
				run: () => {
					if (activeSessionId) {
						openPage('send-message', { sessionId: activeSessionId });
					} else {
						openPage('pick-target');
					}
				}
			},
			{
				id: 'action-send-to-target',
				section: 'Actions',
				label: 'Chat in…',
				hint: 'pick a thread or session',
				icon: '↗',
				keepOpen: true,
				run: () => openPage('pick-target')
			},
			{
				id: 'action-schedule-task',
				section: 'Actions',
				label: 'Schedule…',
				hint: 'recurring at a specific time',
				kbd: 'N S',
				icon: '🗓',
				keepOpen: true,
				run: () => openPage('schedule-task')
			},
			{
				id: 'action-loop-task',
				section: 'Actions',
				label: 'Loop…',
				hint: 'recurring at fixed interval',
				kbd: 'N L',
				icon: '↻',
				keepOpen: true,
				run: () => openPage('loop-task')
			},
			{
				id: 'action-quick-chat',
				section: 'Actions',
				label: 'Quick chat',
				hint: 'toggle floating overlay',
				kbd: '⌘J',
				icon: '✦',
				run: () => chatStore.toggleBubble()
			},
			{
				id: 'action-event-stream',
				section: 'Actions',
				label: 'Events console',
				hint: 'toggle floating overlay',
				kbd: '⌘E',
				icon: '⟿',
				run: () => toggleEventsConsole()
			},
			{
				id: 'action-run-sota',
				section: 'Actions',
				label: 'SOTA…',
				hint: 'pick a fixture, run it directly',
				kbd: 'N R',
				icon: '🧪',
				keepOpen: true,
				run: () => openPage('sota-list')
			},
			{
				id: 'action-browser-flow',
				section: 'Actions',
				label: 'Browser…',
				hint: 'open a URL and do something',
				kbd: 'N B',
				icon: '🌐',
				keepOpen: true,
				run: () => openPage('browser-flow')
			},
			{
				id: 'action-debug-flow',
				section: 'Actions',
				label: 'Debug…',
				hint: 'delegate to the Internal Diagnostic Agent',
				kbd: 'N D',
				icon: '🔬',
				keepOpen: true,
				run: () => openPage('debug-flow')
			},
			{
				id: 'action-delegate',
				section: 'Actions',
				label: 'Delegate…',
				hint: 'pick any agent, hand off a task',
				kbd: 'N E',
				icon: '⇉',
				keepOpen: true,
				run: () => openPage('delegate-pick-agent')
			},

		);

		// Workspace
		result.push({
			id: 'workspace-current',
			section: 'Workspace',
			label: `${scope.principal} · ${scope.workspace}`,
			hint: 'current',
			icon: '⎈',
			run: () => goto('/settings#workspace')
		});

		return result;
	}

	const SUGGESTED_TOP_N = 5;
	const FRECENCY_WEIGHT = 1.0;
	const CONTEXT_WEIGHT = 0.8;
	const SECTION_ORDER = ['Suggested', 'Needs attention', 'Jump to', 'Actions', 'Manage', 'Apps', 'Threads', 'Recent sessions', 'Recent tasks', 'Recent executions', 'Workspace'];

	// Group items in stable section order. bits-ui auto-hides groups whose
	// items all fail the filter, so we pass the full unfiltered set. When
	// the input is empty, prepend a "Suggested" section with the top-N
	// items ranked by frecency × route-context relevance — so the most
	// useful next-action surfaces instantly even on the user's first open.
	$: groups = buildGroups(items, query, $commandFrequencyStore, currentPath);

	function buildGroups(
		list: CommandItem[],
		q: string,
		freqMap: Record<string, { count: number; lastUsed: number }>,
		path: string
	): Array<{ section: string; items: CommandItem[] }> {
		const base = groupItems(list);
		// Only show Suggested when not searching — when typing, fuzzy
		// results across the full catalog are what the user wants.
		if (q.trim() !== '') return base;

		const ranked = list
			.map((item) => ({
				item,
				score:
					frecencyScore(freqMap, item.id) * FRECENCY_WEIGHT +
					contextScore(item, path) * CONTEXT_WEIGHT
			}))
			.filter(({ score }) => score > 0)
			.sort((a, b) => b.score - a.score)
			.slice(0, SUGGESTED_TOP_N)
			.map(({ item }) => ({
				...item,
				id: `suggested-${item.id}`,
				section: 'Suggested',
				recordAs: item.id
			}));

		if (ranked.length === 0) return base;
		return [{ section: 'Suggested', items: ranked }, ...base];
	}

	function groupItems(list: CommandItem[]): Array<{ section: string; items: CommandItem[] }> {
		const result: Array<{ section: string; items: CommandItem[] }> = [];
		for (const section of SECTION_ORDER) {
			if (section === 'Suggested') continue; // handled separately
			const sectionItems = list.filter((i) => i.section === section);
			if (sectionItems.length > 0) result.push({ section, items: sectionItems });
		}
		return result;
	}

	// Route-context relevance score for an item, given the user's current
	// path. Higher = more relevant. Designed to combine additively with
	// frecency so route context boosts items even before any usage history
	// exists, but lets long-term-frequent commands still surface.
	function contextScore(item: CommandItem, path: string): number {
		// Anything in Needs attention always gets a baseline boost — by
		// definition the user has something blocked/failed/awaiting them.
		// Route-specific boosts below stack on top.
		if (item.section === 'Needs attention') {
			let s = 0.6;
			if (path.startsWith('/approvals') && item.id.startsWith('approval-')) s = 1.0;
			if (path.startsWith('/tasks') && item.id.startsWith('attention-task-')) s = 1.0;
			return s;
		}
		// Tasks surface area
		if (path.startsWith('/tasks')) {
			if (item.section === 'Recent tasks') return 1.0;
			if (item.id === 'action-new-task') return 0.8;
			if (item.id === 'go-tasks') return 0.4;
		}
		// Thread / chat surface area
		if (path === '/chat' || path.startsWith('/t/')) {
			if (item.section === 'Threads') return 0.7;
			if (item.section === 'Recent sessions') return 0.9;
			if (item.id === 'action-quick-chat') return 0.6;
			if (item.id === 'new-thread') return 0.5;
		}
		// Agent / crew surface area
		if (path.startsWith('/crew') || path.startsWith('/agents')) {
			if (item.id === 'manage-/crew') return 0.7;
		}
		// Events surface area
		if (path.startsWith('/events')) {
			if (item.id === 'manage-/events') return 0.9;
			if (item.id === 'action-event-stream') return 0.7;
		}
		// LLM observability vs dispatch queue. The queue page is operational
		// (workers / retries / cancellations); the /llm root is the
		// cost/latency dashboard. Boost the one matching the active route.
		if (path === '/llm/queue' || path.startsWith('/llm/queue/')) {
			if (item.id === 'go-llm-queue') return 0.9;
		} else if (path.startsWith('/llm')) {
			if (item.id === 'go-llm') return 0.9;
		}
		// Legacy Approvals URLs redirect to the global Attention surface.
		if (path.startsWith('/approvals')) {
			if (item.id === 'manage-/attention') return 0.9;
		}
		// Memory / API mining / Skills / Triggers / Vault / Channels /
		// Budget / Dev sessions / Settings / Feed / History / Mirror — for the "Manage"
		// destinations, when you're on one of them, boost the matching
		// entry so re-jumping is one keypress away.
		const matchingManage = `manage-${path}`;
		if (item.id === matchingManage) return 0.6;
		// Warroom
		if (path.startsWith('/warroom')) {
			if (item.id === 'go-warroom') return 0.7;
		}
		return 0;
	}

	// Searchable string bits-ui's fuzzy matcher scores against. Includes
	// label, section, hint, and any entity id so users can type e.g.
	// `task_8223` to find a task by partial ID.
	function searchValue(item: CommandItem): string {
		return `${item.label} ${item.section} ${item.hint ?? ''} ${item.entityId ?? ''}`;
	}

	function close(): void {
		open = false;
		query = '';
		palettePage = 'root';
		targetSessionId = null;
		targetThreadId = null;
		actionTaskId = null;
		actionControlState = null;
		actionControlExecutionId = null;
		actionControlLoadedRevision = -1;
		actionControlLoadId += 1;
		delegateAgentId = null;
		scheduleMaxRuns = null;
		submitting = false;
		release(COORDINATOR_ID);
	}

	async function run(item: CommandItem): Promise<void> {
		// Record frecency BEFORE close so the next palette open sees the
		// updated counts. Use recordAs when the item is a "Recent"
		// duplicate so the original command's counter is what increments.
		commandFrequencyStore.record(item.recordAs ?? item.id);
		// `keepOpen` items step into a sub-page or run an in-palette action;
		// don't close, just dispatch.
		if (item.keepOpen) {
			void item.run();
			return;
		}
		close();
		await tick();
		void item.run();
	}

	function handleEscape(event: KeyboardEvent): void {
		if (!open) return;
		if (event.key === 'Escape') {
			event.preventDefault();
			// On a sub-page, escape pops back to root instead of closing
			// the whole palette. Faster to correct course than to re-open.
			if (palettePage !== 'root') {
				popPage();
			} else {
				close();
			}
		}
	}

	function openPage(
		p: PalettePage,
		opts: {
			sessionId?: string;
			threadId?: string;
			taskId?: string;
			agentId?: string;
			executionId?: string;
		} = {}
	): void {
		palettePage = p;
		targetSessionId = opts.sessionId ?? null;
		targetThreadId = opts.threadId ?? null;
		actionTaskId = opts.taskId ?? null;
		actionControlState = null;
		actionControlExecutionId = opts.executionId ?? null;
		actionControlLoadedRevision = -1;
		actionControlLoadId += 1;
		if (p === 'task-action' && opts.taskId) {
			const selectedTask = tasks.find((task) => task.id === opts.taskId);
			if (selectedTask?.activeExecutionId) {
				void loadTaskControlState(selectedTask.activeExecutionId);
			}
		}
		// Delegate page carries the selected agent across the pick → instruct
		// transition; clear it on entry to other pages so stale picks don't
		// leak.
		if (p === 'delegate-pick-agent' || p === 'delegate-instruct') {
			delegateAgentId = opts.agentId ?? delegateAgentId;
		} else {
			delegateAgentId = null;
		}
		query = '';
		if (p === 'schedule-task' || p === 'loop-task') {
			scheduleMaxRuns = null;
		}
	}

	function popPage(): void {
		palettePage = 'root';
		targetSessionId = null;
		targetThreadId = null;
		actionTaskId = null;
		actionControlState = null;
		actionControlExecutionId = null;
		actionControlLoadedRevision = -1;
		actionControlLoadId += 1;
		delegateAgentId = null;
		scheduleMaxRuns = null;
		query = '';
	}

	async function submitNewTask(): Promise<void> {
		const title = query.trim();
		if (!title || submitting) return;
		submitting = true;
		try {
			await taskStore.createTask(title, '', { agentId: primaryAgentId });
			showSuccess('Task created', title);
			close();
		} catch (err) {
			console.error('[CommandPalette] createTask failed:', err);
			showError('Could not create task', err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	async function submitMessage(): Promise<void> {
		const text = query.trim();
		const sessionId = targetSessionId ?? activeSessionId;
		if (!text || !sessionId || submitting) return;
		submitting = true;
		try {
			await chatStore.sendMessage(sessionId, text);
			showSuccess('Message sent', text.length > 60 ? text.slice(0, 60) + '…' : text);
			close();
		} catch (err) {
			console.error('[CommandPalette] sendMessage failed:', err);
			showError('Could not send message', err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	// Task-action sub-page: items vary by the selected task's status,
	// approval, plan presence, and active execution.
	interface TaskActionItem {
		id: string;
		label: string;
		icon: string;
		hint?: string;
		opensPage?: boolean;
		/** Returns are discarded — taskStore methods often return the
		 *  updated task / null and we don't need that here. */
		run: () => unknown | Promise<unknown>;
	}

	$: actionTask = actionTaskId ? tasks.find((t) => t.id === actionTaskId) ?? null : null;
	$: currentActionExecutionId = actionTask?.activeExecutionId?.trim() || null;
	$: currentActionControlRevision = currentActionExecutionId
		? ($executionControlInvalidations.get(currentActionExecutionId) ?? 0)
		: 0;
	$: if (
		palettePage === 'task-action' &&
		(currentActionExecutionId !== actionControlExecutionId ||
			currentActionControlRevision !== actionControlLoadedRevision)
	) {
		if (currentActionExecutionId) {
			void loadTaskControlState(currentActionExecutionId, currentActionControlRevision);
		} else {
			actionControlExecutionId = null;
			actionControlLoadedRevision = -1;
			actionControlState = null;
			actionControlLoadId += 1;
		}
	}
	$: taskActionItems = actionTask ? buildTaskActions(actionTask, actionControlState) : [];

	async function loadTaskControlState(executionId: string, revision?: number): Promise<void> {
		const loadId = ++actionControlLoadId;
		actionControlExecutionId = executionId;
		actionControlLoadedRevision =
			revision ?? ($executionControlInvalidations.get(executionId) ?? 0);
		actionControlState = null;
		try {
			const next = await getExecutionControlState(executionId);
			if (
				loadId === actionControlLoadId &&
				actionTaskId &&
				actionTask?.activeExecutionId === executionId &&
				next.execution_id === executionId
			) {
				actionControlState = next;
			}
		} catch {
			if (loadId === actionControlLoadId) actionControlState = null;
		}
	}

	function buildTaskActions(
		task: Task,
		controlState: ExecutionControlState | null
	): TaskActionItem[] {
		const items: TaskActionItem[] = [];
		const executionId =
			task.activeExecutionId && controlState?.execution_id === task.activeExecutionId
				? task.activeExecutionId
				: null;
		// Navigation always available first — Enter on a Recent task → Enter
		// on the first action = the legacy "just open it" behavior.
		items.push({
			id: 'open',
			label: 'Open task',
			icon: '↗',
			run: () => goto(`/tasks?selected=${encodeURIComponent(task.id)}`)
		});

		if (executionId && controlState?.can_steer) {
			items.push({
				id: 'steer-execution',
				label: 'Steer execution',
				icon: '↗',
				hint: 'guide the next decision turn',
				opensPage: true,
				run: () => openPage('steer-task', { taskId: task.id, executionId })
			});
		}

		if (executionId && controlState?.can_pause) {
			items.push({
				id: 'pause-execution',
				label: 'Pause execution',
				icon: '⏸',
				run: async () => {
					await applyExecutionControl(executionId, 'pause');
					await taskStore.loadTasks();
				}
			});
		} else if (executionId && controlState?.can_resume) {
			items.push({
				id: 'resume-execution',
				label: 'Resume execution',
				icon: '▶',
				run: async () => {
					await applyExecutionControl(executionId, 'resume');
					await taskStore.loadTasks();
				}
			});
		}

		if (task.status !== 'completed' && task.status !== 'cancelled') {
			items.push({
				id: 'execute',
				label: task.status === 'running' ? 'Restart execution' : 'Run task',
				icon: '▶',
				hint: task.status === 'running' ? 'cancels current then re-runs' : undefined,
				run: () => taskStore.executeTask(task.id)
			});
		}

		if (executionId && controlState?.can_cancel) {
			items.push({
				id: 'cancel-exec',
				label: 'Cancel execution',
				icon: '⏹',
				run: async () => {
					await applyExecutionControl(executionId, 'cancel');
					await taskStore.loadTasks();
				}
			});
		}

		if (!task.hasPlan && task.status !== 'completed' && task.status !== 'cancelled') {
			items.push({
				id: 'plan',
				label: 'Plan task',
				icon: '◌',
				hint: 'generate execution plan',
				run: () => taskStore.planTask(task.id)
			});
		} else if (task.hasPlan && task.status !== 'completed' && task.status !== 'cancelled') {
			items.push({
				id: 'replan',
				label: 'Re-plan task',
				icon: '↺',
				hint: 'discard plan, regenerate',
				run: () => taskStore.replanTask(task.id)
			});
		}

		if (task.status === 'pending' && task.approved === false) {
			items.push({
				id: 'approve',
				label: 'Approve task',
				icon: '✓',
				run: () => approveTask(task.id)
			});
		}

		if (task.status !== 'completed') {
			items.push({
				id: 'complete',
				label: 'Mark complete',
				icon: '☑',
				run: () => taskStore.completeTask(task.id)
			});
		} else {
			items.push({
				id: 'uncomplete',
				label: 'Mark incomplete',
				icon: '☐',
				run: () => taskStore.uncompleteTask(task.id)
			});
		}

		// Schedule pause/resume — only when the task actually has a
		// cron schedule. Distinct from execution pause; toggles a flag
		// on `task.schedule.paused` via updateTask so the scheduler
		// hydration filter drops / re-includes it on the next tick.
		if (task.schedule?.cron) {
			const isPaused = task.schedule.paused === true;
			items.push({
				id: isPaused ? 'resume-schedule' : 'pause-schedule',
				label: isPaused ? 'Resume schedule' : 'Pause schedule',
				icon: isPaused ? '▶' : '⏸',
				hint: task.schedule.cron,
				run: () =>
					taskStore.updateTask(task.id, {
						schedule: { ...task.schedule!, paused: !isPaused }
					})
			});
		}

		items.push({
			id: 'delete',
			label: 'Delete task',
			icon: '🗑',
			hint: 'permanent',
			run: () => taskStore.deleteTask(task.id)
		});

		return items;
	}

	async function runTaskAction(item: TaskActionItem): Promise<void> {
		if (submitting) return;
		if (item.opensPage) {
			await Promise.resolve(item.run());
			return;
		}
		submitting = true;
		try {
			await Promise.resolve(item.run());
			// Navigation has its own visual feedback (route change); skip the
			// toast for "Open task" so it doesn't double up.
			if (item.id !== 'open' && actionTask) {
				showSuccess(item.label, actionTask.title || actionTask.id);
			}
			close();
		} catch (err) {
			console.error(`[CommandPalette] task action ${item.id} failed:`, err);
			showError(`${item.label} failed`, err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	async function submitTaskSteer(): Promise<void> {
		const message = query.trim();
		const executionId = actionControlExecutionId;
		if (!message || !executionId || submitting) return;
		if (actionTask?.activeExecutionId?.trim() !== executionId) {
			const taskId = actionTask?.id;
			showError('Active execution changed', 'Review the replacement run before steering it.');
			if (taskId) openPage('task-action', { taskId });
			return;
		}
		if (steerMessageByteLength(message) > MAX_STEER_MESSAGE_BYTES) {
			showError('Steer is too long', `Keep guidance within ${MAX_STEER_MESSAGE_BYTES} bytes.`);
			return;
		}
		submitting = true;
		try {
			await applyExecutionControl(executionId, 'steer', message);
			showSuccess('Steer queued', actionTask?.title || actionTask?.id || executionId);
			close();
		} catch (err) {
			showError('Could not steer execution', err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	// Direct command-palette runs are execution machinery, not durable user
	// commitments. Browser runs retain user provenance with `internal`; SOTA
	// fixtures use `debug`, which is both Internal and correctly system-framed.
	// Scope is propagated as a workspace-bound bearer claim.
	async function dispatchExecution(
		title: string,
		goal: string,
		classification: 'internal' | 'debug'
	): Promise<void> {
		const principalAtDispatch = scope.principal?.trim() ?? '';
		const workspaceAtDispatch = scope.workspace?.trim() ?? '';
		if (!principalAtDispatch || !workspaceAtDispatch) {
			throw new Error('Scope (principal/workspace) is not ready yet');
		}
		const headers: Record<string, string> = {
			'Content-Type': 'application/json',
		};
		const body: Record<string, unknown> = {
			title,
			initial_message: goal,
			skip_planning: true,
			max_iterations: 2000,
			[classification]: true
		};
		const response = await timedFetch('/api/magician/v2/executions', {
			method: 'POST',
			headers,
			body: JSON.stringify(body)
		});
		if (!response.ok) {
			throw new Error(
				`Execution dispatch failed: ${response.status} ${response.statusText}`
			);
		}
		// Workspace switched mid-flight — drop the success so the toast surfaces
		// in the originating scope only.
		if (
			scope.principal?.trim() !== principalAtDispatch ||
			scope.workspace?.trim() !== workspaceAtDispatch
		) {
			throw new Error('Scope changed during dispatch; result not applied to current workspace');
		}
	}

	async function submitSotaFixture(fixture: string): Promise<void> {
		if (submitting || !browser) return;
		submitting = true;
		try {
			const url = `${window.location.origin}/tests/sota-tests/${fixture}`;
			const goal = `Navigate to ${url} and then ${sotaGoalForFixture(fixture)}`;
			const title = `SOTA · ${sotaDisplayName(fixture)}`;
			await dispatchExecution(title, goal, 'debug');
			showSuccess('SOTA dispatched', sotaDisplayName(fixture));
			close();
		} catch (err) {
			console.error('[CommandPalette] SOTA dispatch failed:', err);
			showError('Could not dispatch SOTA', err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	async function submitBrowserFlow(): Promise<void> {
		const goal = query.trim();
		if (!goal || submitting) return;
		submitting = true;
		try {
			const title = `Browser · ${goal.length > 50 ? goal.slice(0, 50) + '…' : goal}`;
			await dispatchExecution(title, goal, 'internal');
			showSuccess('Browser dispatched', goal.length > 80 ? goal.slice(0, 80) + '…' : goal);
			close();
		} catch (err) {
			console.error('[CommandPalette] Browser dispatch failed:', err);
			showError('Could not dispatch browser', err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	// Direct-assign helper for Debug + Delegate. Creates a task with the
	// worker agent as primary owner (bypassing prompt-routing through the
	// personal agent), then executes immediately. Terminal retention keeps the
	// resulting task off the active list once it completes successfully.
	//
	// EXPERIMENT: this exercises the gap we audited — the v3 task endpoint
	// accepts worker agent_ids today. If the runtime takes the manifest at
	// face value and runs the worker directly, this is cleaner than the
	// prompt-routing approach. If it doesn't work end-to-end, we'll see
	// failed dispatches in toasts and revert to prompt-routing.
	async function dispatchAsAgent(
		title: string,
		description: string,
		agentId: string
	): Promise<void> {
		const created = await taskStore.createTask(title, description, {
			agentId
		});
		const newTaskId = (created as { id?: string } | null)?.id ?? null;
		if (!newTaskId) {
			throw new Error('Task creation returned no id');
		}
		try {
			await taskStore.executeTask(newTaskId);
		} catch (err) {
			// Execution never started — the task would otherwise linger
			// forever as "pending" because terminal retention only fires
			// after a terminal status. Best-effort cleanup so we don't
			// orphan rows on the task list.
			void taskStore.deleteTask(newTaskId).catch((deleteErr) => {
				console.warn('[CommandPalette] failed to clean up orphaned task', newTaskId, deleteErr);
			});
			throw err;
		}
	}

	async function submitDebugFlow(): Promise<void> {
		const question = query.trim();
		if (!question || submitting) return;
		submitting = true;
		try {
			const title = `Debug · ${question.length > 50 ? question.slice(0, 50) + '…' : question}`;
			await dispatchAsAgent(title, question, 'internal-system-analyst');
			showSuccess('Investigation dispatched', question.length > 80 ? question.slice(0, 80) + '…' : question);
			close();
		} catch (err) {
			console.error('[CommandPalette] Debug dispatch failed:', err);
			showError('Could not dispatch investigation', err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	async function submitDelegate(): Promise<void> {
		const instruction = query.trim();
		if (!instruction || !delegateAgentId || submitting) return;
		submitting = true;
		try {
			const agentLabel = delegateAgentId;
			const title = `Delegate · ${agentLabel} · ${instruction.length > 40 ? instruction.slice(0, 40) + '…' : instruction}`;
			await dispatchAsAgent(title, instruction, delegateAgentId);
			showSuccess(`Delegated to ${agentLabel}`, instruction.length > 80 ? instruction.slice(0, 80) + '…' : instruction);
			close();
		} catch (err) {
			console.error('[CommandPalette] Delegate dispatch failed:', err);
			showError('Could not delegate', err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	async function submitRecurringTask(cron: string): Promise<void> {
		const title = query.trim();
		if (!title || submitting) return;
		submitting = true;
		try {
			const schedule: { cron: string; timezone: string; max_runs?: number } = {
				cron,
				timezone: localTimezone()
			};
			if (scheduleMaxRuns !== null && scheduleMaxRuns > 0) {
				schedule.max_runs = scheduleMaxRuns;
			}
			await taskStore.createTask(title, '', { agentId: primaryAgentId, schedule });
			const cap = scheduleMaxRuns !== null ? ` · ${scheduleMaxRuns} runs` : '';
			showSuccess(
				palettePage === 'schedule-task' ? 'Scheduled task created' : 'Looping task created',
				`${title} · ${cron}${cap}`
			);
			close();
		} catch (err) {
			console.error('[CommandPalette] createTask (scheduled) failed:', err);
			showError('Could not create scheduled task', err instanceof Error ? err.message : undefined);
			submitting = false;
		}
	}

	$: shouldFilter =
		palettePage === 'root' ||
		palettePage === 'pick-target' ||
		palettePage === 'task-action' ||
		palettePage === 'sota-list' ||
		palettePage === 'delegate-pick-agent';

	$: placeholder = (() => {
		switch (palettePage) {
			case 'new-task':
				return 'New task title — press Enter to create';
			case 'send-message':
				return 'Type a message — press Enter to send';
			case 'pick-target':
				return 'Filter threads and sessions…';
			case 'schedule-task':
				return 'Task title — then pick a schedule below';
			case 'loop-task':
				return 'Task title — then pick an interval below';
			case 'task-action':
				return 'Filter actions for this task…';
			case 'steer-task':
				return 'Guide the next decision turn — press Enter to send';
			case 'sota-list':
				return 'Filter SOTA fixtures (number, name, or keyword)…';
			case 'browser-flow':
				return 'Open <URL> and do <something> — press Enter to dispatch';
			case 'debug-flow':
				return 'Ask the Internal Diagnostic Agent — press Enter to dispatch';
			case 'delegate-pick-agent':
				return 'Filter agents by name, id, or kind…';
			case 'delegate-instruct':
				return 'Instructions for the picked agent — press Enter to dispatch';
			default:
				return 'Search, run, jump to anywhere…';
		}
	})();

	$: targetLabel = (() => {
		if (palettePage === 'steer-task' && actionTask) {
			return `Steer · ${actionTask.title || actionTask.id}`;
		}
		if (palettePage === 'send-message') {
			const sid = targetSessionId ?? activeSessionId;
			if (sid) {
				const s = sessions.find((session) => session.id === sid);
				const isActive = sid === activeSessionId && !targetSessionId;
				const label = s?.title || sid;
				return `→ ${label}${isActive ? ' (active)' : ''}`;
			}
			if (targetThreadId) {
				const t = threads.find((thread) => thread.id === targetThreadId);
				return `→ #${t?.name || targetThreadId}`;
			}
			return '→ no active session — pick a target';
		}
		if (palettePage === 'task-action' && actionTask) {
			return `◷ ${actionTask.title || actionTask.id} · ${actionTask.status}`;
		}
		if (palettePage === 'delegate-instruct' && delegateAgentId) {
			const a = agents.find((agent) => agent.agent_id === delegateAgentId);
			const label = a?.name || delegateAgentId;
			return `⇉ ${label}`;
		}
		return null;
	})();

	function handleBackdropClick(event: MouseEvent): void {
		if (event.target === event.currentTarget) close();
	}

	$: if (browser) onOpenChange(open);
	$: if (browser && appDirectoryScopeKey !== appliedAppDirectoryScopeKey) {
		appliedAppDirectoryScopeKey = appDirectoryScopeKey;
		appDirectoryRequest += 1;
		appDirectoryEntries = [];
	}
	$: if (browser && open && palettePage === 'root') {
		scheduleAppDirectoryLauncher(query, appDirectoryScopeKey);
	}

	function onOpenChange(isOpen: boolean): void {
		if (isOpen) {
			void refreshPaletteData();
			requestFocus({
				id: COORDINATOR_ID,
				priority: OVERLAY_PRIORITIES.commandPalette,
				onClose: () => {
					open = false;
					query = '';
				}
			});
		} else {
			if (appDirectorySearchTimer) clearTimeout(appDirectorySearchTimer);
			appDirectorySearchTimer = null;
			appDirectoryRequest += 1;
			appDirectoryEntries = [];
			release(COORDINATOR_ID);
		}
	}

	function scheduleAppDirectoryLauncher(search: string, expectedScope: string): void {
		if (appDirectorySearchTimer) clearTimeout(appDirectorySearchTimer);
		appDirectorySearchTimer = setTimeout(() => {
			appDirectorySearchTimer = null;
			void loadAppDirectoryLauncher(search.trim(), expectedScope);
		}, search.trim() ? 160 : 0);
	}

	async function loadAppDirectoryLauncher(search: string, expectedScope: string): Promise<void> {
		const request = ++appDirectoryRequest;
		appDirectoryEntries = [];
		try {
			const page = await fetchAppDirectory({ section: 'installed', search, limit: 48 });
			if (request === appDirectoryRequest && expectedScope === appDirectoryScopeKey) {
				appDirectoryEntries = page.entries;
			}
		} catch {
			if (request === appDirectoryRequest && expectedScope === appDirectoryScopeKey) {
				appDirectoryEntries = [];
			}
		}
	}

	async function refreshPaletteData(): Promise<void> {
		// The shell keeps this component mounted while closed. Refresh on
		// opening so unrelated App navigation does not start these requests.
		// Stores keep their existing data and error state if a refresh fails.
		const results = await Promise.allSettled([
			threadStore.start?.(),
			chatStore.loadSessions?.(),
			taskStore.loadTasks?.(),
			loadApprovals({ clearError: true }),
			loadAgents(),
			refreshNotesSettings()
		]);
		const failures = results.filter((result): result is PromiseRejectedResult => result.status === 'rejected');
		if (open && failures.length > 0) {
			showError('Some command data could not be refreshed', 'Close and reopen the palette to retry.');
		}
	}

	onDestroy(() => {
		if (appDirectorySearchTimer) clearTimeout(appDirectorySearchTimer);
		appDirectoryRequest += 1;
		release(COORDINATOR_ID);
	});
</script>

<svelte:window on:keydown={handleEscape} />

{#if open}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="palette-shade"
		on:click={handleBackdropClick}
		transition:fade={{ duration: 120 }}
	>
		<div
			class="palette"
			role="dialog"
			aria-label="Command palette"
			aria-modal="true"
			transition:fly={{ y: 8, duration: 180 }}
		>
			<Command.Root loop shouldFilter={shouldFilter} filter={commandFilter}>
				<div class="palette-head">
					{#if palettePage === 'root'}
						<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" class="search-icon">
							<circle cx="11" cy="11" r="7" />
							<path d="M21 21l-4.3-4.3" />
						</svg>
					{:else}
						<button
							type="button"
							class="back-button"
							on:click={popPage}
							aria-label="Back to root"
							title="Back (Esc)"
						>←</button>
					{/if}
					<Command.Input
						bind:value={query}
						placeholder={placeholder}
						autocomplete="off"
						spellcheck={false}
						autofocus
					/>
					<kbd class="esc">esc</kbd>
				</div>

				{#if targetLabel}
					<div class="palette-context">{targetLabel}</div>
				{/if}

				<Command.List class="palette-list">
					{#if palettePage === 'root'}
						<Command.Empty class="empty">No commands match "{query}"</Command.Empty>
						<Command.Viewport>
							{#each groups as group (group.section)}
								<Command.Group class="palette-group">
									<Command.GroupHeading class="palette-section">{group.section}</Command.GroupHeading>
									<Command.GroupItems>
										{#each group.items as item (item.id)}
											<Command.Item
												class="palette-item"
												value={searchValue(item)}
												onSelect={() => run(item)}
											>
												<span class="ico">{item.icon ?? '·'}</span>
												<span class="label">
													{item.label}
													{#if item.hint}<span class="hint">— {item.hint}</span>{/if}
												</span>
												{#if item.kbd}<kbd class="kbd">{item.kbd}</kbd>{/if}
											</Command.Item>
										{/each}
									</Command.GroupItems>
								</Command.Group>
							{/each}
						</Command.Viewport>
					{:else if palettePage === 'new-task'}
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">New task</Command.GroupHeading>
								<Command.GroupItems>
									<Command.Item
										class="palette-item"
										value="create-task-submit"
										onSelect={submitNewTask}
									>
										<span class="ico">+</span>
										<span class="label">
											{query.trim() ? `Create "${query.trim()}"` : 'Type a title to create a task'}
										</span>
										<kbd class="kbd">↵</kbd>
									</Command.Item>
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'send-message'}
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">Chat</Command.GroupHeading>
								<Command.GroupItems>
									<Command.Item
										class="palette-item"
										value="send-message-submit"
										onSelect={submitMessage}
									>
										<span class="ico">→</span>
										<span class="label">
											{query.trim() ? `Send "${query.trim()}"` : 'Type a message to send'}
										</span>
										<kbd class="kbd">↵</kbd>
									</Command.Item>
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'schedule-task' || palettePage === 'loop-task'}
						{@const presets = palettePage === 'schedule-task' ? SCHEDULE_PRESETS : LOOP_PRESETS}
						{@const heading = palettePage === 'schedule-task' ? 'Schedule' : 'Interval'}
						<div class="palette-max-runs">
							<span class="max-runs-label">Max runs</span>
							{#each MAX_RUNS_OPTIONS as opt (opt ?? 'unlimited')}
								<button
									type="button"
									class="max-runs-pill"
									class:active={scheduleMaxRuns === opt}
									on:click={() => (scheduleMaxRuns = opt)}
									title={opt === null ? 'Run indefinitely' : `Stop after ${opt} run${opt === 1 ? '' : 's'}`}
								>{opt === null ? '∞' : opt}</button>
							{/each}
						</div>
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">{heading}</Command.GroupHeading>
								<Command.GroupItems>
									{#each presets as preset (preset.cron)}
										<Command.Item
											class="palette-item"
											value={preset.label}
											onSelect={() => submitRecurringTask(preset.cron)}
										>
											<span class="ico">{palettePage === 'schedule-task' ? '🗓' : '↻'}</span>
											<span class="label">
												{preset.label}
												{#if query.trim()}
													<span class="hint">— "{query.trim()}"{scheduleMaxRuns !== null ? ` × ${scheduleMaxRuns}` : ''}</span>
												{:else}
													<span class="hint">— type a title first</span>
												{/if}
											</span>
											<kbd class="kbd">{preset.cron}</kbd>
										</Command.Item>
									{/each}
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'sota-list'}
						<Command.Empty class="empty">No SOTA fixtures match "{query}"</Command.Empty>
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">SOTA fixtures ({SOTA_FIXTURES.length})</Command.GroupHeading>
								<Command.GroupItems>
									{#each SOTA_FIXTURES as fixture (fixture)}
										<Command.Item
											class="palette-item"
											value={`${fixture} ${sotaDisplayName(fixture)}`}
											onSelect={() => submitSotaFixture(fixture)}
										>
											<span class="ico">🧪</span>
											<span class="label">{sotaDisplayName(fixture)}</span>
											<kbd class="kbd">{fixture.replace('.html', '')}</kbd>
										</Command.Item>
									{/each}
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'browser-flow'}
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">Browser</Command.GroupHeading>
								<Command.GroupItems>
									<Command.Item
										class="palette-item"
										value="browser-flow-submit"
										onSelect={submitBrowserFlow}
									>
										<span class="ico">🌐</span>
										<span class="label">
											{query.trim() ? `Dispatch: "${query.trim()}"` : 'Type a goal — e.g. open https://flightaware.com and check status of UA123'}
										</span>
										<kbd class="kbd">↵</kbd>
									</Command.Item>
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'debug-flow'}
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">Investigate</Command.GroupHeading>
								<Command.GroupItems>
									<Command.Item
										class="palette-item"
										value="debug-flow-submit"
										onSelect={submitDebugFlow}
									>
										<span class="ico">🔬</span>
										<span class="label">
											{query.trim() ? `Investigate: "${query.trim()}"` : 'Ask the diagnostic agent — e.g. why did task_82236 fail at the canvas drag step?'}
										</span>
										<kbd class="kbd">↵</kbd>
									</Command.Item>
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'delegate-pick-agent'}
						<Command.Empty class="empty">No agents match "{query}"</Command.Empty>
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">Agents ({agents.length})</Command.GroupHeading>
								<Command.GroupItems>
									{#each agents.filter((a) => !a.disabled) as agent (agent.agent_id)}
										<Command.Item
											class="palette-item"
											value={`${agent.name ?? ''} ${agent.agent_id} ${agent.kind ?? ''} ${agent.description ?? ''}`}
											onSelect={() => openPage('delegate-instruct', { agentId: agent.agent_id })}
										>
											<span class="ico">{agent.kind === 'Worker' ? '⚙' : '◆'}</span>
											<span class="label">
												{agent.name ?? agent.agent_id}
												<span class="hint">— {agent.kind ?? 'agent'}{agent.description ? ` · ${agent.description.slice(0, 60)}` : ''}</span>
											</span>
											<kbd class="kbd">{agent.agent_id.slice(0, 24)}</kbd>
										</Command.Item>
									{/each}
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'delegate-instruct'}
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">Delegate</Command.GroupHeading>
								<Command.GroupItems>
									<Command.Item
										class="palette-item"
										value="delegate-submit"
										onSelect={submitDelegate}
									>
										<span class="ico">⇉</span>
										<span class="label">
											{query.trim() ? `Delegate: "${query.trim()}"` : `Type instructions for ${delegateAgentId ?? 'the picked agent'} — press Enter to dispatch`}
										</span>
										<kbd class="kbd">↵</kbd>
									</Command.Item>
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'steer-task'}
						<Command.Viewport>
							<Command.Group class="palette-group">
								<Command.GroupHeading class="palette-section">Execution control</Command.GroupHeading>
								<Command.GroupItems>
									<Command.Item
										class="palette-item"
										value="steer-submit"
										disabled={!query.trim() || steerMessageByteLength(query.trim()) > MAX_STEER_MESSAGE_BYTES || submitting}
										onSelect={submitTaskSteer}
									>
										<span class="ico">↗</span>
										<span class="label">
											{query.trim() ? `Send steer: "${query.trim()}"` : 'Type guidance for the active execution'}
											<span class="hint">— {steerMessageByteLength(query.trim())} / {MAX_STEER_MESSAGE_BYTES} bytes</span>
										</span>
										<kbd class="kbd">↵</kbd>
									</Command.Item>
								</Command.GroupItems>
							</Command.Group>
						</Command.Viewport>
					{:else if palettePage === 'task-action'}
						<Command.Empty class="empty">
							{actionTask ? `No actions match "${query}"` : 'Task not found in cache; reopen palette and pick again'}
						</Command.Empty>
						<Command.Viewport>
							{#if actionTask}
								<Command.Group class="palette-group">
									<Command.GroupHeading class="palette-section">Actions</Command.GroupHeading>
									<Command.GroupItems>
										{#each taskActionItems as item (item.id)}
											<Command.Item
												class="palette-item"
												value={`${item.label} ${item.hint ?? ''}`}
												onSelect={() => runTaskAction(item)}
											>
												<span class="ico">{item.icon}</span>
												<span class="label">
													{item.label}
													{#if item.hint}<span class="hint">— {item.hint}</span>{/if}
												</span>
											</Command.Item>
										{/each}
									</Command.GroupItems>
								</Command.Group>
							{/if}
						</Command.Viewport>
					{:else if palettePage === 'pick-target'}
						<Command.Empty class="empty">No threads or sessions match "{query}"</Command.Empty>
						<Command.Viewport>
							{#if threads.length > 0}
								<Command.Group class="palette-group">
									<Command.GroupHeading class="palette-section">Threads</Command.GroupHeading>
									<Command.GroupItems>
										{#each threads as thread (thread.id)}
											<Command.Item
												class="palette-item"
												value={`thread ${thread.name || thread.id} ${thread.id}`}
												onSelect={() => openPage('send-message', { threadId: thread.id })}
											>
												<span class="ico">#</span>
												<span class="label">{thread.name || thread.id}</span>
											</Command.Item>
										{/each}
									</Command.GroupItems>
								</Command.Group>
							{/if}
							{#if sessions.length > 0}
								<Command.Group class="palette-group">
									<Command.GroupHeading class="palette-section">Sessions</Command.GroupHeading>
									<Command.GroupItems>
										{#each sessions as session (session.id)}
											<Command.Item
												class="palette-item"
												value={`session ${session.title || session.id} ${session.id}`}
												onSelect={() => openPage('send-message', { sessionId: session.id })}
											>
												<span class="ico">◉</span>
												<span class="label">
													{session.title || session.id}
													{#if session.ui_thread_id}<span class="hint">— #{session.ui_thread_id}</span>{/if}
												</span>
											</Command.Item>
										{/each}
									</Command.GroupItems>
								</Command.Group>
							{/if}
						</Command.Viewport>
					{/if}
				</Command.List>
			</Command.Root>
		</div>
	</div>
{/if}

<style>
	.palette-shade {
		position: fixed;
		inset: 0;
		background: var(--bg-scrim);
		backdrop-filter: blur(4px);
		-webkit-backdrop-filter: blur(4px);
		z-index: 600;
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding-top: 14vh;
	}

	.palette {
		width: min(640px, calc(100% - 32px));
		background: var(--bg-elevated, #fff);
		color: var(--text-primary, #1a1a1a);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: var(--radius-lg, 14px);
		overflow: hidden;
		box-shadow: var(--shadow-lg, 0 20px 50px rgba(0, 0, 0, 0.15));
		display: flex;
		flex-direction: column;
		max-height: 70vh;
	}

	/* bits-ui `Command.Root` renders an unstyled <div> as the only direct
	 * child of `.palette`. Without flex sizing it expands to the natural
	 * height of its children, so the `flex: 1; overflow-y: auto` on
	 * `.palette-list` never bites and the entire list overflows the
	 * palette's `max-height: 70vh` clip box — looks like the menu "isn't
	 * scrollable". Make the bits-ui wrapper a constrained flex column so
	 * `.palette-list` can shrink and scroll. */
	.palette > :global(div) {
		display: flex;
		flex-direction: column;
		flex: 1 1 auto;
		min-height: 0;
	}

	.palette-head {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 14px 16px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
	}

	.search-icon {
		color: var(--text-muted, #999);
		flex-shrink: 0;
	}

	.back-button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 22px;
		height: 22px;
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--text-muted, #999);
		cursor: pointer;
		font-size: 16px;
		border-radius: var(--radius-sm, 4px);
		flex-shrink: 0;
		font-family: var(--font-primary);
		transition: background var(--transition-fast, 0.12s ease), color var(--transition-fast, 0.12s ease);
	}

	.back-button:hover {
		color: var(--text-primary, #1a1a1a);
		background: var(--bg-soft, #f0f0f0);
	}

	.palette-context {
		padding: 8px 16px;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted, #888);
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		background: var(--bg-soft, rgba(0, 0, 0, 0.02));
	}

	.palette-max-runs {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 10px 16px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		background: var(--bg-soft, rgba(0, 0, 0, 0.02));
	}

	.max-runs-label {
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.14em;
		color: var(--text-muted, #888);
		margin-right: 4px;
	}

	.max-runs-pill {
		min-width: 28px;
		height: 24px;
		padding: 0 8px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--text-secondary, #555);
		font-family: var(--font-mono);
		font-size: 12px;
		cursor: pointer;
		transition:
			background var(--transition-fast, 0.12s ease),
			color var(--transition-fast, 0.12s ease),
			border-color var(--transition-fast, 0.12s ease);
	}

	.max-runs-pill:hover {
		color: var(--text-primary, #1a1a1a);
		border-color: var(--text-muted, #999);
	}

	.max-runs-pill.active {
		background: var(--accent-primary, #c2502a);
		color: var(--text-on-accent, #fff);
		border-color: var(--accent-primary, #c2502a);
	}

	.palette-head :global(input) {
		flex: 1;
		background: transparent;
		border: 0;
		outline: 0;
		font-family: var(--font-primary);
		font-size: 15px;
		color: var(--text-primary, #1a1a1a);
		letter-spacing: -0.005em;
	}

	.palette-head :global(input::placeholder) {
		color: var(--text-faint, #999);
	}

	kbd {
		font-family: var(--font-mono);
		font-size: 10.5px;
		padding: 2px 6px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: var(--radius-sm, 4px);
		background: var(--bg-soft, #f0f0f0);
		color: var(--text-muted, #666);
	}

	/* bits-ui renders Command.List / Group / Item / Empty as descendant
	 * elements, so their classes need :global() to escape Svelte's CSS
	 * scoping. Keyboard-focused items carry `data-selected`. */
	:global(.palette-list) {
		flex: 1;
		overflow-y: auto;
		padding: 6px;
	}

	:global(.palette-section) {
		padding: 8px 10px 4px;
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.14em;
		color: var(--text-muted, #888);
	}

	:global(.palette-item) {
		display: grid;
		grid-template-columns: 22px 1fr auto;
		gap: 12px;
		align-items: center;
		padding: 8px 10px;
		border-radius: var(--radius-md, 8px);
		cursor: pointer;
		color: var(--text-secondary, #555);
		transition:
			background var(--transition-fast, 0.12s ease),
			color var(--transition-fast, 0.12s ease);
	}

	:global(.palette-item[data-selected]),
	:global(.palette-item:hover) {
		background: var(--accent-primary-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-primary, #1a1a1a);
	}

	:global(.palette-item .ico) {
		color: var(--text-muted, #999);
		font-size: 14px;
		text-align: center;
	}

	:global(.palette-item[data-selected] .ico),
	:global(.palette-item:hover .ico) {
		color: var(--accent-primary, #c2502a);
	}

	:global(.palette-item .label) {
		font-size: 13.5px;
		font-weight: 500;
	}

	:global(.palette-item .label .hint) {
		font-weight: 400;
		color: var(--text-muted, #999);
		margin-left: 6px;
		font-size: 12px;
	}

	:global(.palette-item .kbd) {
		font-family: var(--font-mono);
		font-size: 10.5px;
		padding: 2px 6px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: var(--radius-sm, 4px);
		background: var(--bg-soft, #f0f0f0);
		color: var(--text-muted, #666);
	}

	:global(.empty) {
		padding: 24px;
		text-align: center;
		font-size: 13px;
		color: var(--text-muted, #888);
	}
</style>
