<script lang="ts">
	/**
	 * VibeDev "Living Studio" cockpit shell.
	 *
	 * `rail | conversation | stage` — the conversation is the spine. Owns the
	 * reactive wiring that used to live in the `+page.svelte` monolith, but
	 * delegates the data to the extracted stores (`codingSpineStore`,
	 * `vibeHitlStore`, `vibeStudioStore`, `submit.ts`). Reused as-is:
	 * `VibeComposer`, `VibePreviewPanel` (in the stage), `DiffStrip`.
	 */
	import { onDestroy, onMount, tick } from 'svelte';
	import { get } from 'svelte/store';
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';

	import Checkbox from '$lib/magician/components/generative/Checkbox.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { clickOutside } from '$lib/shared/clickOutside';
	import { createMenuKeydown, menuFocusableItems } from '$lib/shared/menuKeydown';
	import { agentDisplayName } from '$lib/presentationIdentity';
	import VibeComposer from '$lib/shell/VibeComposer.svelte';
	import { buildTaskMentionItems } from '$lib/magician/chat/composerMentions';
	import StudioRail from '$lib/shell/vibe/rail/StudioRail.svelte';
	import AppOptionsPanel from '$lib/shell/vibe/AppOptionsPanel.svelte';
	import ConversationSpine from '$lib/shell/vibe/conversation/ConversationSpine.svelte';
	import StuckBanner from '$lib/shell/vibe/conversation/StuckBanner.svelte';
	import StudioStage from '$lib/shell/vibe/stage/StudioStage.svelte';
	import TerminalDrawer from '$lib/shell/vibe/stage/TerminalDrawer.svelte';

	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { loadAgents, primaryAgent } from '$lib/stores/agentStore';
	import { codingProfileStore } from '$lib/stores/codingProfileStore';
	import { chatStore, type UploadedAttachment } from '$lib/stores/chatStore';
	import { taskStore, type Task } from '$lib/stores/taskStore';
	import { threadStore } from '$lib/stores/threadStore';
	import {
		vibeDevProjectStore,
		type VibeDevDeploySettings,
		type VibeDevProject
	} from '$lib/stores/vibeDevProjectStore';
	import { ensurePendingHitlBridge } from '$lib/stores/pendingHitlStore';
	import { codingSpineStore } from '$lib/stores/codingSpineStore';
	import { vibeStudioStore } from '$lib/stores/vibeStudioStore';
	import {
		vibeCheckpoints,
		fetchVibeCheckpointsForChain,
		revertVibeCheckpoint
	} from '$lib/stores/vibeCheckpointsStore';
	import { runCheck } from './stage/checks';
	import {
		vibeDiffRows,
		vibeOtherRows,
		hitlActionState,
		buildVibeRunChainIds,
		sortRowsForActiveRun,
		filterRowsForActiveRun,
		buildChangedFiles,
		isProposalDiff,
		projectIdFromDescription,
		parentTaskIdFromDescription,
		resolveDiff,
		applyDiffFile,
		rejectDiffFile,
		approveAll,
		respondToVibeRow,
		autoApplyEligibleDiffs,
		resetAutoApplyMemo,
		historicalProposalRows,
		hydrateRunProposals,
		mergeDiffRowsByProposal,
		type VibeRow
	} from '$lib/stores/vibeHitlStore';
	import {
		submitCodingRun,
		resolveActiveProject,
		taskAcceptsFollowUp,
		bareTitle,
		stripRunTitlePrefix,
		VIBEDEV_THREADED_TAG,
		type SubmitContext
	} from '$lib/shell/vibe/conversation/submit';
	import { selectCardsForRun, detectStuck } from '$lib/shell/vibe/conversation/spineModel';
	import type { RunMeta } from '$lib/shell/vibe/conversation/spineModel';
	import { controlRun } from '$lib/shell/vibe/conversation/control';
	import { takeSeed } from '$lib/stores/vibeSeedStore';
	import {
		fetchProjectInfo,
		type CheckResult,
		type CheckSpec,
		type ProjectInfo
	} from '$lib/shell/vibe/stage/checks';
	import type { HitlRequest } from '$lib/hitl/types';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { requestConfirmation } from '$lib/stores/confirmationStore';

	const VIBEDEV_THREAD_ID = 'vibedev';
	// A run with no activity in this window is NOT "live" — guards a hydrated /
	// finished / stuck (non-terminal but stale) run from showing the "Building /
	// steer / Stop" bar just because the scope event tail is connected. Generous
	// (10 min) so a genuinely active run paused on a HITL diff isn't misjudged;
	// a hydrated historical run is hours/days old, so it's still caught.
	const LIVE_RECENCY_MS = 10 * 60 * 1000;
	const VIBEDEV_THREAD_NAME = 'VibeDev';
	const EXAMPLE_CHIPS = ['a habit-tracker PWA', 'add dark mode', 'wire up auth', 'a landing page with pricing'];

	// ── lifecycle ──────────────────────────────────────────────────────────────
	let prompt = '';
	let composerEl: VibeComposer | null = null;
	let stagedAttachments: UploadedAttachment[] = [];
	// `@`-mention candidates for the composer: COMPLETED persistent tasks only.
	// Internal/VibeDev runs (a separate feed) are intentionally excluded — the
	// agreed "no internal in the dropdown" rule — and the backend dependency
	// validator rejects non-completed reference ids, so completed-only keeps the
	// structured continuation edge valid.
	$: vibeTaskMentionItems = buildTaskMentionItems(
		$taskStore.tasks.filter((task) => task.status === 'completed')
	);
	let attachmentUploading = false;
	let attachmentBatchVersion = 0;
	let voicePrefix: string | null = null;
	let dispatching = false;
	// The just-dispatched composer follow-up turn (threaded onto the in-view run) whose first
	// coding event hasn't landed yet — drives the spine's "next step starting…" loader. Cleared
	// when its first card streams in (or it settles without one).
	let pendingTurnTaskId: string | null = null;
	let bootstrapKey = '';
	let ensurePromise: Promise<string | null> | null = null;
	let threadError = '';
	let fetchedTask: Task | null = null;
	let fetchKey = '';
	let fetchInFlight = false;

	$: scope = $scopeIdentityStore;
	$: newBuildPlaceholder = $primaryAgent
		? `Ask ${agentDisplayName($primaryAgent)} to build, fix, or dream something up…`
		: 'What should we make today?';
	$: routeProjectId = ($page.url.searchParams.get('project') || '').trim() || null;
	$: routeTaskId = ($page.url.searchParams.get('task') || '').trim() || null;

	onMount(() => {
		threadStore.start();
		taskStore.start();
		void taskStore.loadTasks();
		void loadAgents().catch(() => {
			// Keep the neutral placeholder when the personal-agent list is unavailable.
		});
		ensurePendingHitlBridge();
		void codingProfileStore.load();
		window.addEventListener('message', onPreviewMessage);
		// Legacy `/vibe?tab=workbench` link → open the CLI Agent in the cockpit
		// (the canonical standalone full-screen Workbench is /dev).
		if (get(page).url.searchParams.get('tab') === 'workbench') {
			vibeStudioStore.toggleTerminal(true);
		}
		// M4 cross-surface ingress: `/vibe?seed=<id>` → hydrate the seed (one-shot,
		// so a refresh can't re-inject), show the chip, pre-fill the ask, strip the param.
		const seedId = get(page).url.searchParams.get('seed');
		if (seedId) {
			const draft = takeSeed(seedId);
			if (draft) {
				seedContent = draft.content;
				seedLabel = draft.label;
				seedSource = draft.source;
				seedSourceId = draft.sourceId ?? null;
				if (draft.suggestedPrompt && !prompt.trim()) prompt = draft.suggestedPrompt;
			}
			const params = new URLSearchParams(get(page).url.searchParams);
			params.delete('seed');
			const query = params.toString();
			void goto(query ? `/vibe?${query}` : '/vibe', {
				replaceState: true,
				keepFocus: true,
				noScroll: true
			});
		}
	});
	onDestroy(() => {
		codingSpineStore.stop();
		threadStore.stop();
		taskStore.stop();
		if (browser) window.removeEventListener('message', onPreviewMessage);
	});

	// Scope-level live coding stream.
	$: codingSpineStore.start(scope);
	// Load projects when scope/route changes (no auto-create — that's submit-time).
	$: void maybeBootstrap(scope?.principal, scope?.workspace);

	async function maybeBootstrap(
		principal: string | undefined | null,
		workspace: string | undefined | null
	): Promise<void> {
		if (!browser || !principal || !workspace) return;
		const key = `${principal}::${workspace}`;
		if (key === bootstrapKey) return;
		bootstrapKey = key;
		// Scope changed: the task store wipes + reloads its list (its scope
		// bridge), so "tasks have loaded" no longer holds — re-arm the
		// first-run latch until the new scope's first load completes.
		tasksEverLoaded = false;
		await threadStore.createThread(VIBEDEV_THREAD_NAME, VIBEDEV_THREAD_ID).catch(() => null);
		await vibeDevProjectStore.load();
		await chatStore.loadSessions(VIBEDEV_THREAD_ID).catch(() => {});
	}

	// ── project / task derivations ───────────────────────────────────────────
	$: projects = $vibeDevProjectStore.projects;
	$: activeVibeProject =
		projects.find((p) => p.project_id === routeProjectId) ??
		projects.find((p) => p.project_id === $vibeDevProjectStore.activeProjectId) ??
		projects.find((p) => p.chat_session_status === 'active') ??
		projects.find((p) => !p.archived) ??
		null;
	$: activeProjectId = activeVibeProject?.project_id ?? null;
	$: sessionId = activeVibeProject?.chat_session_id ?? null;

	function isVibeDevTask(task: Task): boolean {
		if (task.uiThreadId === VIBEDEV_THREAD_ID) return true;
		return task.tags.some((tag) => tag.name.toLowerCase() === 'vibedev');
	}
	function isForProject(task: Task, projectId: string | null): boolean {
		if (!isVibeDevTask(task)) return false;
		const tid = projectIdFromDescription(task.description);
		if (!tid) return true;
		return Boolean(projectId && tid === projectId);
	}
	function taskUpdatedAtMs(task: Task): number {
		const u = Date.parse(task.updatedAt);
		if (Number.isFinite(u)) return u;
		const c = Date.parse(task.createdAt);
		return Number.isFinite(c) ? c : 0;
	}

	// Run history merges BOTH lanes: persistent runs the user chose to "Save as
	// task" (in `$taskStore.tasks`) and the default Internal runs (in
	// `$taskStore.vibedevInternalTasks`, loaded from `/v3/tasks/internal` so they
	// stay off the global /tasks feed). `vibeTasksById` below dedups by id
	// (last-wins), so ordering internal AFTER persistent lets a fresh internal poll
	// beat a stale optimistic entry, and `fetchedTask` last keeps the selected run
	// freshest.
	$: allLoadedVibeDevTasks = [
		...$taskStore.tasks.filter(isVibeDevTask),
		...$taskStore.vibedevInternalTasks.filter(isVibeDevTask),
		...(fetchedTask && isVibeDevTask(fetchedTask) ? [fetchedTask] : [])
	];
	$: allVibeDevTasks = [
		...allLoadedVibeDevTasks.filter((t) => isForProject(t, activeProjectId))
	];
	// A composer follow-up is THREADED: tagged `vibedev-threaded` so the rail folds it into the
	// in-view run (its chain root) rather than listing it as its own run. Run-button follow-ups
	// are NOT threaded, so they keep their own row.
	function isThreadedTask(task: Task | null | undefined): boolean {
		return !!task && (task.tags ?? []).some((tag) => tag.name === VIBEDEV_THREADED_TAG);
	}
	$: vibeTasksById = new Map(allVibeDevTasks.map((t) => [t.id, t]));
	$: runs = Array.from(vibeTasksById.values())
		// Fold a threaded follow-up turn into the rail ONLY when its parent run is present, so an
		// orphaned threaded turn (e.g. its root run was deleted) still shows a row rather than
		// vanishing (which would also leave the rail with no highlight for it).
		.filter(
			(t) => !(isThreadedTask(t) && vibeTasksById.has(parentTaskIdFromDescription(t.description) ?? ''))
		)
		.sort((a, b) => taskUpdatedAtMs(b) - taskUpdatedAtMs(a));
	$: runCostTaskIds = (() => {
		const byId = new Map(allVibeDevTasks.map((task) => [task.id, task]));
		const visibleRunIds = new Set(runs.map((task) => task.id));
		const out: Record<string, string[]> = {};
		for (const run of runs) out[run.id] = [run.id];
		for (const task of allVibeDevTasks) {
			let current: Task | null | undefined = task;
			const seen = new Set<string>();
			while (current && isThreadedTask(current) && !seen.has(current.id)) {
				seen.add(current.id);
				const parentId = parentTaskIdFromDescription(current.description);
				current = parentId ? byId.get(parentId) ?? null : null;
			}
			const runId = current?.id ?? task.id;
			if (!visibleRunIds.has(runId)) continue;
			out[runId] ??= [runId];
			if (!out[runId].includes(task.id)) out[runId].push(task.id);
		}
		return out;
	})();

	$: activeTaskId = routeTaskId;
	$: activeVibeTask =
		(activeTaskId ? $taskStore.tasks.find((t) => t.id === activeTaskId) ?? null : null) ??
		(fetchedTask?.id === activeTaskId ? fetchedTask : null);
	$: activeTaskLoading = Boolean(activeTaskId && !activeVibeTask && ($taskStore.isLoading || fetchInFlight));
	$: activeTaskMissing = Boolean(activeTaskId && !activeVibeTask && !activeTaskLoading);
	// The rail row to highlight: when the active task is a threaded turn (a composer follow-up,
	// folded out of the rail), highlight its chain ROOT — the nearest non-threaded ancestor (the
	// run it folds into). Walk up `Parent task:` links, skipping threaded turns. Otherwise the
	// active task is its own row.
	$: activeRunRowId = (() => {
		const byId = new Map(allVibeDevTasks.map((t) => [t.id, t]));
		let current: Task | null | undefined = activeVibeTask;
		const seen = new Set<string>();
		while (current && isThreadedTask(current) && !seen.has(current.id)) {
			seen.add(current.id);
			const parentId = parentTaskIdFromDescription(current.description);
			current = parentId ? byId.get(parentId) ?? null : null;
		}
		return (current ?? activeVibeTask)?.id ?? activeTaskId;
	})();
	$: void maybeFetchActiveTask(activeTaskId, activeVibeTask);
	// Major checkpoints for the active run — refetched when the run updates (a minted checkpoint
	// bumps its updatedAt), so the rail's checkpoint nodes track checks-passing applied changes.
	$: {
		// Checkpoints for the whole run-CHAIN (root + threaded follow-up turns), not just the
		// active turn — otherwise an earlier turn's checkpoint looks "overwritten" once a
		// follow-up turn mints its own. Falls back to the route id while the task object loads.
		const _chainIds =
			runChainIds.size > 0 ? Array.from(runChainIds) : activeTaskId ? [activeTaskId] : [];
		const _checkpointRefreshKey = `${_chainIds.join(',')}:${activeVibeTask?.updatedAt ?? ''}`;
		void _checkpointRefreshKey;
		void fetchVibeCheckpointsForChain(_chainIds);
	}

	// Auto-checkpoint on settle: a major checkpoint mints on a PASSING project check,
	// but nothing runs a check when a coding run completes — so a completed run sits
	// with an applied-but-unchecked proposal and the rail shows no checkpoint until the
	// user clicks Tests. When a coding run settles to `completed`, run a project check
	// ONCE (the `/check` endpoint mints via `mint_checkpoint_for_repo` on green), then
	// refetch. Guarded once-per-task; skips plan/Discuss runs (they apply no code, so a
	// check is pure waste); best-effort (never blocks). Mint self-gates on there being an
	// applied, not-yet-checkpointed proposal, so a no-change completion mints nothing.
	const autoCheckedRuns = new Set<string>();
	$: maybeAutoCheckOnSettle(activeVibeTask, activeProjectId);
	function maybeAutoCheckOnSettle(task: Task | null, projectId: string | null): void {
		if (!browser || !task || !projectId) return;
		if (task.status !== 'completed' || task.synthesisPending) return;
		if ((task.tags ?? []).some((t) => t.name?.toLowerCase() === 'plan')) return;
		if (autoCheckedRuns.has(task.id)) return;
		autoCheckedRuns.add(task.id);
		void (async () => {
			await runCheck(projectId, 'all');
			// Re-broaden to the whole chain (this settled turn + its chain siblings), so a
			// follow-up turn's mint doesn't hide the earlier turns' checkpoints.
			void fetchVibeCheckpointsForChain(Array.from(buildVibeRunChainIds(task, allVibeDevTasks)));
		})();
	}

	function maybeFetchActiveTask(taskId: string | null, storeTask: Task | null): void {
		if (!browser || !taskId) return;
		if (storeTask) {
			if (fetchedTask?.id === taskId) fetchedTask = null;
			fetchKey = '';
			fetchInFlight = false;
			return;
		}
		if (fetchedTask?.id === taskId || fetchKey === taskId) return;
		fetchKey = taskId;
		fetchInFlight = true;
		void (async () => {
			const fetched = await taskStore.fetchTaskRecordById(taskId);
			if (fetchKey !== taskId) return;
			fetchedTask = fetched;
			fetchInFlight = false;
		})();
	}

	$: runChainIds = buildVibeRunChainIds(activeVibeTask, allVibeDevTasks);

	// ── HITL (run-scoped) ──────────────────────────────────────────────────────
	$: action = $hitlActionState;
	// Merge live pending diffs with durable historical proposal rows (a finished
	// run whose resolved diffs left the pending store, hydrated on open). Live
	// wins per proposal. DISPLAY-only — `proposalDiffRows` (auto-apply) stays
	// live-only so resolved proposals are never re-applied.
	$: allDiffRows = mergeDiffRowsByProposal($historicalProposalRows, $vibeDiffRows);
	$: orderedDiffRows = sortRowsForActiveRun(allDiffRows, runChainIds);
	$: orderedOtherRows = sortRowsForActiveRun($vibeOtherRows, runChainIds);
	$: scopedDiffRows = filterRowsForActiveRun(orderedDiffRows, runChainIds);
	$: diffRowsByProposal = new Map<string, VibeRow>(
		allDiffRows
			.filter((row) => row.request.schema.proposal_id)
			.map((row) => [row.request.schema.proposal_id as string, row])
	);
	$: changedFiles = buildChangedFiles(
		scopedDiffRows.length > 0 ? scopedDiffRows : orderedDiffRows,
		runChainIds
	);
	$: proposalDiffRows = $vibeDiffRows.filter((row) => isProposalDiff(row.request));

	// Spine diff-card review route: focus the stage Diff tab on the proposal's
	// first file (reuses StudioStage's path-keyed scroll). Absent row → plain
	// tab switch. Cleared off the Diff tab, mirroring StudioStage, so a repeat
	// click on the same proposal re-triggers the scroll.
	let stageFocusDiffPath: string | null = null;
	$: if (studioState.stageTab !== 'diff' && stageFocusDiffPath) stageFocusDiffPath = null;
	function reviewDiffInStage(proposalId: string | null): void {
		const row = proposalId ? diffRowsByProposal.get(proposalId) : undefined;
		stageFocusDiffPath = row?.request.schema.files?.[0]?.path ?? null;
		vibeStudioStore.setStageTab('diff');
	}

	// auto-apply effect (when ON). Skipped in Autopilot — the agent applies its
	// own proposals there, so the cockpit must not race it.
	$: studioState = $vibeStudioStore;
	$: if (
		browser &&
		studioState.autoApplyCodeProposals &&
		studioState.mode !== 'autopilot' &&
		proposalDiffRows.length > 0
	) {
		void autoApplyEligibleDiffs(proposalDiffRows);
	}

	// ── spine cards (run-scoped) + meta ─────────────────────────────────────────
	$: spine = $codingSpineStore;
	$: chainCards = selectCardsForRun(spine.cards, runChainIds);
	// Composer follow-up loader (1b): clear the pending-turn marker once the new turn's first
	// card lands in the chain (loading done) OR the turn settled without one (e.g. failed early).
	$: if (
		pendingTurnTaskId &&
		(chainCards.some((c) => c.taskId === pendingTurnTaskId) ||
			allVibeDevTasks.some(
				(t) =>
					t.id === pendingTurnTaskId &&
					['completed', 'failed', 'cancelled', 'skipped', 'deferred'].includes(t.status)
			))
	) {
		pendingTurnTaskId = null;
	}
	$: loadingNewTurn = Boolean(pendingTurnTaskId);
	$: testCards = chainCards.filter((c) => c.kind === 'test');
	// Null when no present run so the right-panel telemetry (cost/ctx/tokens,
	// fed from the persistent coding-spine store) doesn't show a deleted/previous
	// run's stale numbers once no real run is selected.
	$: runMeta = activeVibeTask ? pickMeta(spine.meta, chainCards.map((c) => c.shadowId)) : null;
	// Newest event time across the run's cards (`ts` is the event's own
	// timestamp, so a hydrated/old run reads stale here — see LIVE_RECENCY_MS).
	$: runLatestTs = chainCards.reduce((max, c) => (c.ts > max ? c.ts : max), 0);
	// "Live" requires the run to be non-terminal AND genuinely recent — not just
	// that the scope tail is connected. Without the recency + `!taskTerminal`
	// gate, a finished or stuck (never-terminal) run hydrated from history showed
	// the "Building — type to steer it" bar + Stop, looking like it was running.
	$: runLive =
		!taskTerminal &&
		chainCards.some((c) => c.shadowId && runMeta && !runMeta.terminal) &&
		spine.streamState === 'live' &&
		runLatestTs > 0 &&
		Date.now() - runLatestTs < LIVE_RECENCY_MS;
	// Stream-health banner (Stage F): the scope event tail dropped MID-run —
	// reconnect/backoff is in flight ('connecting'/'closed'/'error') while the
	// run itself still looks live (non-terminal + recent activity). `runLive`
	// can't drive this: it REQUIRES streamState === 'live', so it clears the
	// instant the tail drops. Reuses the store's existing published streamState
	// (no store contract change). No banner for idle/terminal/synthesizing runs.
	$: streamReconnecting =
		!taskTerminal &&
		!runSynthesizing &&
		Boolean(runMeta && !runMeta.terminal) &&
		['connecting', 'closed', 'error'].includes(spine.streamState) &&
		runLatestTs > 0 &&
		Date.now() - runLatestTs < LIVE_RECENCY_MS;
	// A terminal (finished) task has no live build stream to connect to, so the
	// ConversationSpine empty-state must not show the live "connecting…/error"
	// copy (the stuck-banner bug on deep-linked finished runs like task_21b7).
	$: taskTerminal =
		!!activeVibeTask &&
		['completed', 'failed', 'cancelled', 'canceled', 'skipped'].includes(activeVibeTask.status);

	// FIX #6b — a fresh terminal run that engaged NO coding pipeline (the RCA
	// coordinator self-serve case, which the server-side success-gate now
	// terminates as `failed`): terminal + finished hydrating + zero coding cards.
	// Distinguishes "the coordinator never delegated" from the aged-out
	// "stream predates the history window" case, so the spine empty-state can
	// show accurate copy instead of the misleading aged-out message.
	$: coordinatorTerminal =
		taskTerminal &&
		!activeTaskLoading &&
		chainCards.length === 0 &&
		activeVibeTask?.status === 'failed';

	// History hydrate (B2): when a selected run shows no spine cards — its live
	// events have aged out of the scope tail's 24h backfill window — pull its
	// recorded timeline once from the durable log with a wide `since`, so a
	// refreshed/finished run isn't blank. Guarded per-task so it fires at most
	// once per run; if the run genuinely has no recorded events it stays empty
	// (the spine then shows its historical empty-state copy).
	let lastHydratedTaskId: string | null = null;
	$: maybeHydrateRunHistory(activeVibeTask, activeTaskLoading, chainCards.length);
	function maybeHydrateRunHistory(task: Task | null, loading: boolean, cardCount: number): void {
		if (!browser || !task || loading || cardCount > 0) return;
		if (lastHydratedTaskId === task.id) return;
		lastHydratedTaskId = task.id;
		const startMs = task.createdAt ? Date.parse(task.createdAt) : NaN;
		void codingSpineStore.hydrateRun(task.id, Number.isFinite(startMs) ? startMs - 60_000 : undefined);
	}

	// Durable diff/code/tests hydrate (A): for a TERMINAL run, pull its persisted
	// proposals so the Diff/Code panels repopulate on refresh even after its
	// resolved diffs left the pending store. Gated on `taskTerminal` so a LIVE
	// run keeps its actionable live HITL rows (no read-only historical overlay);
	// passing null clears the historical rows. Self-guards per-task internally.
	$: void hydrateRunProposals(taskTerminal ? activeVibeTask?.id ?? null : null);

	// The live run's control id — the newest non-terminal card's task (or
	// shadow) id; null when nothing is in flight. The backend registered this
	// id (scope-qualified) for the turn's lifetime.
	$: liveRunId = (() => {
		for (let i = chainCards.length - 1; i >= 0; i -= 1) {
			const card = chainCards[i];
			const meta = spine.meta.get(card.shadowId);
			if (meta && !meta.terminal) return card.taskId ?? card.shadowId;
		}
		return null;
	})();
	$: canSteer = runLive && Boolean(liveRunId);
	// Stop must work for ANY non-terminal selected run, not only while it's
	// "live". A stuck/looping run stops emitting events so `runLive` ages out
	// (>10min), and synthesis / stream-drops clear it too — yet the backend can
	// still cancel it via the root task id (the control endpoint resolves
	// run_id → execution and has a runtime-gone fallback to a terminal cancel).
	// So gate Stop on `canStop` (non-terminal selected run) and fall back from
	// the live control id to the PRESENT run's id. Use `activeVibeTask?.id`, NOT
	// the route id `activeTaskId` — the latter stays set after a run is deleted
	// (stale URL) with no run object behind it, which would wrongly show
	// "Run in progress" + Stop (and the spine's "Starting…") for a gone run.
	// A run is only genuinely "synthesizing" if synthesis is pending AND the run
	// wasn't CANCELLED. A cancelled run abandons its synthesis but can still carry a
	// STALE `synthesisPending` — e.g. a restart kills a run mid-synthesis and it's
	// force-cancelled before the flag clears, so the backend keeps reporting
	// `synthesis_pending: true` on a `cancelled` task. Without this guard that stale
	// flag overrides terminality (canStop's `|| runSynthesizing`), freezing the
	// "Synthesizing" label + a Stop control that can only no-op. NOTE: `failed` is
	// NOT excluded — a Step-1 terminal run legitimately stays synthesis-pending while
	// async finalization attaches its outputs, and Stop must stay available there.
	$: runSynthesizing =
		(activeVibeTask?.synthesisPending ?? false) &&
		!['cancelled', 'canceled'].includes(activeVibeTask?.status ?? '');
	$: stopRunId = liveRunId ?? activeVibeTask?.id ?? null;
	// A run can report a terminal task status while a synthesis-pending execution
	// is still outstanding (genuine end-of-run synthesis, or a stuck one) — the
	// rail shows "Synthesizing" for it, so Stop must stay available there too.
	$: canStop = Boolean(stopRunId) && (!taskTerminal || runSynthesizing);
	// A SELECTED non-terminal run with NO live execution, NO attached execution id, and not
	// mid-synthesis — i.e. a run that was created but never started (a pending orphan, e.g. if
	// dispatch failed). Stop would no-op and Cancel is hard-disabled (no executionId), so the
	// only honest control is Discard (the dismiss action). Mutually exclusive with a real
	// live/synthesizing run.
	$: canDismissRun =
		Boolean(activeVibeTask) &&
		!taskTerminal &&
		!runSynthesizing &&
		liveRunId === null &&
		!activeVibeTask?.executionId;

	let controlling = false;

	// Foreground budget backstop (M2): if the live run's cost crosses the budget
	// while the tab is open, stop it (once). The agent's own self-limit directive
	// covers the laptop-closed case.
	let budgetStoppedFor: string | null = null;
	$: if (
		browser &&
		studioState.costBudgetUsd != null &&
		runMeta?.costTotal != null &&
		liveRunId &&
		runMeta.costTotal >= studioState.costBudgetUsd &&
		budgetStoppedFor !== liveRunId
	) {
		budgetStoppedFor = liveRunId;
		void stopRun();
		showError('Cost budget reached', 'Stopped the run — review the diff and raise the budget to continue.');
	}

	// "■ Stop" (the split-button's primary) is a STEER-level action: it aborts the
	// in-flight Pi turn so you can redirect, but a re-delegating coordinator
	// immediately spins up a fresh turn. For a runaway/looping run you want
	// cancelRun() below (the split-button's menu item), which is terminal.
	async function stopRun(): Promise<void> {
		if (!stopRunId || controlling) return;
		controlling = true;
		try {
			const result = await controlRun(stopRunId, 'stop');
			if (result.delivered) showSuccess('Stopping step', 'Asked the coding agent to wrap up the current step.');
			else showError('Could not stop the step', result.error || 'The run is not live right now.');
		} finally {
			controlling = false;
		}
	}

	// "Cancel entire run" (in the Stop split-button's menu) is TERMINAL: it cancels
	// the whole execution tree at the root (the coordinator), so it cannot
	// re-delegate. This is the real "make it stop" for a looping/runaway run —
	// the step-level stopRun() above only kills one turn.
	async function cancelRun(): Promise<void> {
		const execId = activeVibeTask?.executionId;
		if (!execId || controlling) return;
		controlling = true;
		try {
			await taskStore.cancelExecution(execId);
			showSuccess('Cancelling run', 'Stopping the whole run — it will not re-delegate.');
		} catch (e) {
			showError('Could not cancel the run', e instanceof Error ? e.message : String(e));
		} finally {
			controlling = false;
		}
	}

	// "✕ Discard" (the split-button's whole surface when `canDismissRun` — no menu) is for a run
	// that was prepared but NEVER started (a pending orphan): there is no
	// live execution to Stop and no execution tree to Cancel. SOFT terminal path — mark it
	// `cancelled` (keeps an auditable row, drops it from the live feed). NOT the hard-delete rail
	// ✕ (which removes files + diagnostics). update_task_status allows pending→cancelled, and
	// start_execution rejects a cancelled task, so this can't race a late-arriving dispatch.
	async function dismissRun(): Promise<void> {
		const id = activeVibeTask?.id;
		if (!id || controlling) return;
		controlling = true;
		try {
			await taskStore.updateTaskStatus(id, 'cancelled');
			showSuccess('Run discarded', 'This run was never started — marked it cancelled.');
			if (id === activeTaskId) void goto('/vibe', { keepFocus: true, noScroll: true });
		} catch (e) {
			showError('Could not discard the run', e instanceof Error ? e.message : String(e));
		} finally {
			controlling = false;
		}
	}

	// Stop split-button menu (chevron → "Cancel entire run"). Shared menu
	// mechanics: `createMenuKeydown` for Arrow/Home/End/Escape, `clickOutside`
	// to close on outside pointerdown — same pattern as ExportMenu/TaskCardMenu.
	let stopMenuOpen = false;
	let stopMenuTriggerEl: HTMLButtonElement | null = null;
	let stopMenuEl: HTMLDivElement | null = null;
	async function toggleStopMenu(): Promise<void> {
		stopMenuOpen = !stopMenuOpen;
		if (stopMenuOpen) {
			await tick();
			menuFocusableItems(stopMenuEl)[0]?.focus();
		}
	}
	function closeStopMenu(refocusTrigger: boolean): void {
		stopMenuOpen = false;
		if (refocusTrigger) stopMenuTriggerEl?.focus();
	}
	const handleStopMenuKeydown = createMenuKeydown({ getMenuEl: () => stopMenuEl, close: closeStopMenu });
	// Drop a stale open flag when the split-button unmounts (run went terminal /
	// switched to the orphan "Discard" presentation) so it can't reopen unbidden.
	$: if (canDismissRun || !(runLive || canStop)) stopMenuOpen = false;

	async function steerRun(message: string): Promise<boolean> {
		if (!liveRunId || !message.trim()) return false;
		const result = await controlRun(liveRunId, 'steer', message.trim());
		if (result.delivered) {
			showSuccess('Steering coding agent', 'Your redirect was sent to the live run.');
			return true;
		}
		return false;
	}

	// Stuck detection (no-progress): same tool call repeated ≥3× on a live run.
	let dismissedStuckSig = '';
	$: stuck = runLive ? detectStuck(chainCards) : null;
	$: stuckSig = stuck ? `${stuck.toolName}::${stuck.args}` : '';
	$: showStuck = Boolean(stuck) && stuckSig !== dismissedStuckSig;

	async function redirectStuck(): Promise<void> {
		if (!stuck) return;
		dismissedStuckSig = stuckSig;
		const hint = `It looks stuck repeating ${stuck.toolName || 'the same step'}. Try a different approach.`;
		const steered = await steerRun(hint);
		if (!steered) prompt = prompt.trim().length > 0 ? prompt : `${hint} `;
	}
	function dismissStuck(): void {
		dismissedStuckSig = stuckSig;
	}

	// ── click-to-edit (U7) ───────────────────────────────────────────────────
	interface SelectedElement {
		tag: string;
		id: string | null;
		classes: string;
		text: string;
		loc: string | null;
		selector: string;
	}
	let editMode = false;
	let selectedEl: SelectedElement | null = null;
	let elementChange = '';
	// Autopilot: schedule this run nightly instead of running it now.
	let nightly = false;
	// Intent gate: VibeDev runs are internal (cockpit-only) by default; checking
	// this saves the run to the user-visible task list (/tasks).
	let saveAsRunTask = false;
	// M4 seed: context (a meeting transcript / chat thread) this build was started from.
	let seedContent: string | null = null;
	let seedLabel = '';
	let seedPreviewOpen = false;
	// Provenance (§13.3 #20): the source surface + its id, persisted onto the project the seeded
	// build lands in (first-source-wins) so the origin is reverse-linkable after a reload.
	let seedSource: import('$lib/stores/vibeSeedStore').VibeSeedSource | null = null;
	let seedSourceId: string | null = null;

	function broadcastEditMode(on: boolean): void {
		if (!browser) return;
		try {
			for (let i = 0; i < window.frames.length; i += 1) {
				try {
					window.frames[i]?.postMessage(
						{ source: 'vibe-host', type: 'edit-mode', on },
						window.location.origin
					);
				} catch {
					/* cross-origin frame — not ours */
				}
			}
		} catch {
			/* ignore */
		}
	}

	function toggleEditMode(): void {
		editMode = !editMode;
		if (!editMode) selectedEl = null;
		broadcastEditMode(editMode);
	}

	function elementContext(el: SelectedElement): string {
		const bits = [`<${el.tag}>`];
		if (el.id) bits.push(`#${el.id}`);
		bits.push(`selector \`${el.selector}\``);
		if (el.loc) bits.push(`source ${el.loc}`);
		return bits.join(' ');
	}

	async function applyElementEdit(): Promise<void> {
		if (!selectedEl) return;
		const change = elementChange.trim();
		if (!change) return;
		const el = selectedEl;
		const instruction = `In the running app, edit the ${elementContext(el)}${el.text ? ` (currently showing "${el.text}")` : ''}: ${change}. Make the smallest focused change to the source.`;
		selectedEl = null;
		elementChange = '';
		await dispatchPrompt(instruction, []);
	}

	function handleInlineTextEdit(el: SelectedElement, oldText: string, newText: string): void {
		if (!newText || oldText === newText) return;
		const instruction = `In the running app, change the visible text of the ${elementContext(el)} from "${oldText}" to "${newText}". Edit only that text.`;
		void dispatchPrompt(instruction, []);
	}

	function onPreviewMessage(event: MessageEvent): void {
		const data = event.data as
			| { source?: string; type?: string; element?: SelectedElement; oldText?: string; newText?: string }
			| null;
		if (!data || data.source !== 'vibe-edit') return;
		if (data.type === 'ready') {
			if (editMode) broadcastEditMode(true);
		} else if (data.type === 'select' && data.element) {
			selectedEl = data.element;
			elementChange = '';
		} else if (data.type === 'text-edit' && data.element) {
			handleInlineTextEdit(data.element, data.oldText ?? '', data.newText ?? '');
		}
	}

	function pickMeta(metaMap: Map<string, RunMeta>, shadowIds: string[]): RunMeta | null {
		let latest: RunMeta | null = null;
		const seen = new Set(shadowIds);
		for (const [shadow, meta] of metaMap) {
			if (seen.size > 0 && !seen.has(shadow)) continue;
			if (!latest || meta.updatedAt >= latest.updatedAt) latest = meta;
		}
		return latest;
	}

	// ── profiles ─────────────────────────────────────────────────────────────
	$: profileState = $codingProfileStore;
	$: selectedProfile = profileState.profiles.find((p) => p.id === profileState.selected) ?? null;
	$: supportsImages = selectedProfile?.supports_user_image_inputs === true;

	// ── project info: preview availability (S2) + self-heal checks (S1) ─────────
	let projectInfo: ProjectInfo | null = null;
	let projectChecks: CheckSpec[] = [];
	let infoKey = '';
	$: void maybeFetchProjectInfo(activeProjectId);
	$: previewAvailable = projectInfo?.previewable ?? true;

	// §13.3 #20 — once the seeded build's project is known, stamp its origin (first-source-wins).
	// One-shot: clears the pending source so it never re-fires or overwrites an existing origin.
	$: void maybePersistSeedSource(activeVibeProject);
	async function maybePersistSeedSource(project: VibeDevProject | null): Promise<void> {
		if (!project || !seedSourceId || !seedSource) return;
		if (project.source_meeting_thread_id || project.source_chat_session_id) {
			seedSourceId = null;
			seedSource = null;
			return;
		}
		const patch =
			seedSource === 'meeting'
				? { source_meeting_thread_id: seedSourceId }
				: seedSource === 'chat'
					? { source_chat_session_id: seedSourceId }
					: null;
		seedSource = null;
		seedSourceId = null; // one-shot before the await
		if (patch) await vibeDevProjectStore.updateProject(project.project_id, patch);
	}

	async function maybeFetchProjectInfo(id: string | null): Promise<void> {
		if (!browser || !id) {
			projectInfo = null;
			projectChecks = [];
			infoKey = '';
			return;
		}
		if (id === infoKey) return;
		infoKey = id;
		const info = await fetchProjectInfo(id);
		if (infoKey !== id) return;
		projectInfo = info;
		projectChecks = info?.checks ?? [];
	}

	function attemptFix(result: CheckResult): void {
		const status = result.timed_out
			? ' (timed out)'
			: result.exit_code != null
				? ` (exit ${result.exit_code})`
				: '';
		const instruction = `The \`${result.command}\` ${result.kind} check failed${status}. Diagnostics:\n\n${result.output_tail}\n\nFix the root cause so the check passes, then keep the change focused.`;
		void dispatchPrompt(instruction, []);
	}

	// ── composer state ──────────────────────────────────────────────────────────
	$: composerMode = (activeVibeTask ? 'follow_up' : 'fresh') as 'fresh' | 'follow_up';
	// Pass every input EXPLICITLY: a `$:` that only *calls* a function does not track that
	// function's internal reactive reads, so `submitBlocker` would compute once at init
	// (empty prompt → "Enter a coding request") and never update as you type — leaving Send
	// permanently disabled. Naming the deps here makes Svelte recompute on each change.
	$: submitBlocker = computeSubmitBlocker(
		prompt,
		dispatching,
		$vibeDevProjectStore.isLoading,
		attachmentUploading,
		canSteer,
		Boolean($taskStore.executingTask),
		activeTaskLoading,
		activeTaskMissing,
		activeVibeTask
	);
	// Show the inviting hero ("What should we build?") whenever no specific run is
	// in view — fresh load, a new/empty project, or after "New run". `activeTaskId`
	// (= routeTaskId) is null exactly in those states and is set the moment a run is
	// selected or deep-linked, so ConversationSpine owns EVERY with-a-run state
	// (active stream, historical/finished, connecting/error, and its own idle empty).
	// Replaces the old cold-start gate (`!activeVibeProject`), which hid the hero
	// forever once any project auto-resolved → the dry "No build activity yet"
	// appeared instead of the invitation.
	// Key on the present run object — NOT the route id `activeTaskId`, which
	// lingers after a run is deleted — so a deleted/missing run falls back to the
	// hero ("What should we build?" + chips), not the spine's "no activity" text.
	// `!activeTaskLoading` avoids flashing the hero while a real run is loading.
	$: showHero = !activeVibeTask && !activeTaskLoading && chainCards.length === 0;

	// Latch: true once the task list has completed ≥1 load for the current
	// scope. `isLoading === false` implies a load has settled — the store boots
	// with `isLoading: true` and only clears it when a load finishes (or
	// errors). Reset on scope change in `maybeBootstrap` (the existing
	// scope-change gate), where the task store also wipes + reloads its list;
	// project switches within a scope reuse the already-loaded scope-level
	// list, so the latch stays valid across them.
	let tasksEverLoaded = false;
	$: if (!$taskStore.isLoading) tasksEverLoaded = true;

	// First-run stage narrative (Stage F): with NO run in view AND no runs at all
	// for the active project, the stage's full chrome (tab strip, "Dev server not
	// running.", disabled Merge/Deploy, empty chip bar) is dead weight — swap it
	// for a single narrative card + example prompts. `showHero` already covers
	// "no run in view"; `runs.length === 0` is the cheapest reliable "first run
	// hasn't happened" signal (the rail's own list). Gate on the
	// `tasksEverLoaded` LATCH, not live `$taskStore.isLoading`: loadTasks flips
	// `isLoading` true on EVERY refresh (realtime debounce + the 15–20s
	// backstop poll), so keying off the live flag flashed the chrome back
	// mid-session on a first-run project. Trade-off: on a cold boot the latch
	// is false until the FIRST load completes, so the normal chrome shows
	// briefly instead of the narrative — the lesser evil vs recurring
	// mid-session flapping.
	$: stageFirstRun = showHero && runs.length === 0 && tasksEverLoaded;

	// Pure over its params (distinct names — no shadowing of the component vars) so the
	// reactive caller above can list them as dependencies. See the comment there.
	function computeSubmitBlocker(
		promptText: string,
		isDispatching: boolean,
		projectLoading: boolean,
		uploadingAttachments: boolean,
		steering: boolean,
		anotherExecuting: boolean,
		loadingRun: boolean,
		runMissing: boolean,
		run: Task | null | undefined
	): string | null {
		if (isDispatching) return 'Dispatching coding task.';
		if (projectLoading) return 'Preparing VibeDev project.';
		if (uploadingAttachments) return 'Uploading attachments.';
		if (promptText.trim().length === 0) return 'Enter a coding request.';
		// While a turn is live the composer steers it — submission is allowed
		// even though a task is executing.
		if (steering) return null;
		if (anotherExecuting) return 'Another task is executing.';
		if (loadingRun) return 'Loading selected run.';
		if (runMissing) return 'Selected run was not found. Start fresh.';
		if (run && !taskAcceptsFollowUp(run)) {
			if (run.synthesisPending) return 'Selected run is still synthesizing.';
			return 'Selected run is still active. Finish or resolve it first.';
		}
		return null;
	}

	$: activeRunSummary = activeVibeTask
		? {
				label: 'Follow-up context',
				title: activeVibeTask.title,
				meta: [activeVibeTask.synthesisPending ? 'synthesizing' : activeVibeTask.status, activeVibeTask.id].filter(
					Boolean
				) as string[],
				blocked: Boolean(submitBlocker && prompt.trim().length > 0)
			}
		: null;

	// ── navigation ───────────────────────────────────────────────────────────
	function vibePath(projectId: string | null, taskId?: string | null): string {
		const params = new URLSearchParams();
		if (projectId) params.set('project', projectId);
		if (taskId) params.set('task', taskId);
		const query = params.toString();
		return query ? `/vibe?${query}` : '/vibe';
	}

	function defaultProjectName(): string {
		const count = projects.length + 1;
		return count <= 1 ? 'VibeDev Project' : `VibeDev Project ${count}`;
	}

	// ── session ensure (auto-creates a default project on first submit) ─────────
	async function ensureSession(): Promise<string | null> {
		if (!browser) return null;
		const proj = activeVibeProject;
		if (proj?.chat_session_id && proj.chat_session_status === 'active') return proj.chat_session_id;
		if (ensurePromise) return ensurePromise;
		ensurePromise = (async () => {
			try {
				const thread = await threadStore.createThread(VIBEDEV_THREAD_NAME, VIBEDEV_THREAD_ID);
				if (!thread) throw new Error('Could not create #vibedev thread');
				await vibeDevProjectStore.load();
				let project = resolveActiveProject(routeProjectId);
				if (!project) {
					project = await vibeDevProjectStore.createProject({ name: defaultProjectName() });
					if (!project) {
						throw new Error(get(vibeDevProjectStore).error ?? 'Could not create a VibeDev project');
					}
				} else if (project.archived || project.chat_session_status !== 'active') {
					project = await vibeDevProjectStore.activateProject(project.project_id);
				}
				if (!project?.chat_session_id) throw new Error('Could not prepare a VibeDev project session');
				await chatStore.loadSessions(VIBEDEV_THREAD_ID);
				threadError = '';
				return project.chat_session_id;
			} catch (error) {
				threadError = error instanceof Error ? error.message : String(error);
				return null;
			} finally {
				ensurePromise = null;
			}
		})();
		return ensurePromise;
	}

	// ── submit ─────────────────────────────────────────────────────────────────
	/**
	 * Route a prompt to the run: steer the in-flight turn if one is live
	 * (falling back to a new task if it just settled), else create + dispatch a
	 * coding task. Returns true if it was dispatched/steered.
	 */
	/** Completed-task ids referenced via `@` chips in the composer — deduped and
	 *  filtered to tasks that still exist AND are completed (the backend rejects
	 *  non-completed refs). Attached to the run as structured continuation refs
	 *  alongside the parent. Only meaningful for a composer submit (the chips live
	 *  in the composer field), so instruction/steer dispatches pass none. */
	function composerReferenceTaskIds(): string[] {
		const completed = new Set(
			$taskStore.tasks.filter((task) => task.status === 'completed').map((task) => task.id)
		);
		const seen = new Set<string>();
		const ids: string[] = [];
		for (const token of composerEl?.chipTokens() ?? []) {
			if (token.kind !== 'task') continue;
			const id = token.slug;
			if (!id || seen.has(id) || !completed.has(id)) continue;
			seen.add(id);
			ids.push(id);
		}
		return ids;
	}

	async function dispatchPrompt(
		text: string,
		attachments: UploadedAttachment[],
		threadOntoActiveRun = false,
		referenceTaskIds: string[] = []
	): Promise<boolean> {
		const trimmed = text.trim();
		if (!trimmed || dispatching) return false;
		const parentTask = activeVibeTask;
		dispatching = true;
		try {
			if (canSteer) {
				const steered = await steerRun(trimmed);
				if (steered) return true;
			}
			const sid = await ensureSession();
			if (!sid) throw new Error(threadError || 'Could not prepare #vibedev session');
			const project = resolveActiveProject(routeProjectId);
			if (!project?.project_id) throw new Error('Could not prepare a VibeDev project');
			// Nightly schedule only applies to a fresh Autopilot run (not a follow-up/steer).
			const schedule =
				studioState.mode === 'autopilot' && nightly && !parentTask
					? { cron: '0 2 * * *', timezone: 'UTC' }
					: undefined;
			const ctx: SubmitContext = {
				project,
				parentTask,
				profile: selectedProfile ? { id: selectedProfile.id, label: selectedProfile.label } : null,
				// Project is plausibly web/visual? Gates the visual self-correction directive
				// so it doesn't ride along on non-web projects. `previewAvailable` is the
				// backend `previewable` signal for the active project (defaults true when unknown).
				projectIsVisual: previewAvailable || Boolean(project.preview_url),
				// Composer Send threads into the in-view run (rail folds it); the Run button does not.
				threaded: threadOntoActiveRun && Boolean(parentTask),
				stagedAttachments: attachments,
				sessionId: project.chat_session_id,
				schedule,
				seedContent: seedContent ?? undefined,
				seedLabel: seedLabel || undefined,
				// Intent gate: off → Internal (cockpit-only); on → user-visible task.
				saveAsTask: saveAsRunTask,
				// `@task` chips in the composer → structured continuation refs (the
				// SERVER merges the parent ref in). Empty for instruction/steer.
				referenceTaskIds
			};
			const { taskId, isFollowUp, scheduled } = await submitCodingRun(trimmed, ctx);
			// The seed is consumed by this run.
			seedContent = null;
			seedLabel = '';
			if (scheduled) {
				showSuccess('Autopilot scheduled', 'Runs nightly at 2am UTC; review the branch in the morning.');
			} else {
				// A composer follow-up is tagged `vibedev-threaded` (via ctx.threaded) so the rail
				// FOLDS it into the in-view run rather than listing it as a separate run.
				// Navigating to the child keeps the run lifecycle (terminal / live / auto-check /
				// checkpoint) keyed on the live turn, while the rail highlights the chain root.
				// `pendingTurnTaskId` drives the loading footer until the turn's first card lands.
				// (The Run button is NOT threaded → opens its own run row, as before.)
				if (threadOntoActiveRun && isFollowUp) pendingTurnTaskId = taskId;
				// Toast shows the run's human title (same truncation as the rail row),
				// not the raw task id — ids are meaningless jargon here.
				showSuccess(
					isFollowUp ? 'VibeDev follow-up started' : 'VibeDev task started',
					bareTitle(trimmed)
				);
				void goto(vibePath(project.project_id, taskId), { keepFocus: true, noScroll: true });
			}
			return true;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			showError('Could not start VibeDev task', message);
			return false;
		} finally {
			dispatching = false;
		}
	}

	async function handleSubmit(): Promise<void> {
		if (submitBlocker) return;
		// `true` = thread a follow-up into the in-view run rather than spawning a separate
		// run (the user-chosen behavior for composer submits; the Run button stays as-is).
		// Chip-referenced completed tasks ride along as structured continuation refs.
		const dispatched = await dispatchPrompt(
			prompt,
			stagedAttachments,
			true,
			composerReferenceTaskIds()
		);
		if (dispatched) {
			prompt = '';
			stagedAttachments = [];
			attachmentBatchVersion += 1;
		}
	}

	function applyChip(chip: string): void {
		prompt = prompt.trim().length > 0 ? `${prompt.trim()} ${chip}` : chip;
	}

	// ── attachments + mic ──────────────────────────────────────────────────────
	function isImageFile(file: File): boolean {
		if (file.type.startsWith('image/')) return true;
		return /\.(png|jpe?g|gif|webp|avif|bmp|svg)$/i.test(file.name);
	}
	async function uploadFiles(files: File[]): Promise<void> {
		const accepted = supportsImages ? files : files.filter((f) => !isImageFile(f));
		if (accepted.length < files.length) {
			showError('Images need a vision-capable coding profile', `Skipped ${files.length - accepted.length} image(s).`);
		}
		if (accepted.length === 0 || attachmentUploading) return;
		const sid = await ensureSession();
		if (!sid) {
			showError('Could not prepare #vibedev attachments', threadError || 'No active #vibedev session.');
			return;
		}
		const batch = ++attachmentBatchVersion;
		attachmentUploading = true;
		try {
			for (const file of accepted) {
				try {
					const uploaded = await chatStore.uploadAttachment(sid, file);
					if (batch !== attachmentBatchVersion) break;
					stagedAttachments = [...stagedAttachments, { ...uploaded, label: file.name }];
				} catch (error) {
					showError('Attachment upload failed', error instanceof Error ? `${file.name}: ${error.message}` : `Could not upload ${file.name}.`);
				}
			}
		} finally {
			if (batch === attachmentBatchVersion) attachmentUploading = false;
		}
	}
	function removeAttachment(id: string): void {
		stagedAttachments = stagedAttachments.filter((a) => a.attachment_id !== id);
	}
	function onMicTranscribeDelta(transcript: string): void {
		if (voicePrefix === null) voicePrefix = prompt;
		prompt = voicePrefix.length > 0 ? `${voicePrefix.trim()} ${transcript}` : transcript;
	}
	function onMicTranscribe(transcript: string): void {
		const prefix = voicePrefix ?? prompt;
		voicePrefix = null;
		const t = transcript.trim();
		if (!t) return;
		prompt = prefix.length > 0 ? `${prefix.trim()} ${t}` : t;
	}

	// ── rail/stage handlers ──────────────────────────────────────────────────
	function selectRun(taskId: string): void {
		void goto(vibePath(activeProjectId, taskId), { keepFocus: true, noScroll: true });
	}
	// Rewind the project's working tree to a checkpoint (a known-good, checks-passing state). The
	// backend snapshots the current tree first (undoable) and queues the checkpoint's Pi session to
	// resume on the next turn, so the agent's context rewinds with the code.
	async function rewindToCheckpoint(taskId: string, checkpointId: string): Promise<void> {
		const cp = $vibeCheckpoints.find((c) => c.id === checkpointId);
		if (cp && !cp.git_sha) {
			showError('Checkpoint is not rewindable', 'This project was not a git repo when the checkpoint was captured.');
			return;
		}
		const label = cp?.name ?? 'this checkpoint';
		const ok = await requestConfirmation({
			title: 'Rewind to checkpoint?',
			message: `Restore files to "${label}". Changes made after that point are discarded — your files as they are now are saved first, so the rewind itself can be undone. The next coding turn resumes from this point.`,
			confirmLabel: 'Rewind',
			cancelLabel: 'Keep current files'
		});
		if (!ok) return;
		const result = await revertVibeCheckpoint(taskId, checkpointId);
		if (!result.ok) {
			showError('Could not rewind to checkpoint', result.error || 'The revert failed.');
			return;
		}
		showSuccess('Rewound to checkpoint', 'The next coding turn resumes from this point. The pre-rewind state was saved.');
		// revertVibeCheckpoint refetched by the single task id; re-broaden to the whole chain so
		// all turns' rewind points stay visible (the in-chain rewind case doesn't re-navigate).
		void fetchVibeCheckpointsForChain(Array.from(runChainIds));
		if (taskId !== activeTaskId) selectRun(taskId);
	}
	function startFresh(): void {
		void goto(vibePath(activeProjectId), { keepFocus: true, noScroll: true });
	}
	async function selectProject(projectId: string): Promise<void> {
		if (!projectId || projectId === activeProjectId) return;
		const project = await vibeDevProjectStore.activateProject(projectId);
		if (!project) {
			showError('Could not switch VibeDev project', get(vibeDevProjectStore).error ?? 'Activation failed.');
			return;
		}
		void goto(vibePath(project.project_id, project.active_root_task_id ?? null), { keepFocus: true, noScroll: true });
	}
	async function createProject(name: string, repoPath: string): Promise<void> {
		const project = await vibeDevProjectStore.createProject({ name, repo_path: repoPath || undefined });
		if (!project) {
			showError('Could not create VibeDev project', get(vibeDevProjectStore).error ?? 'Creation failed.');
			return;
		}
		void goto(vibePath(project.project_id), { keepFocus: true, noScroll: true });
	}

	// Project settings — the rail's ⚙ button dispatches `openSettings`. The new
	// Living Studio never wired it up (the binding + panel were dropped in the
	// rewrite), so the gear was a no-op. Edit the active project's name + preview
	// URL and PATCH via the store (mirrors the legacy studio's settings flow).
	let appOptionsOpen = false;
	let settingsOpen = false;
	let settingsNameDraft = '';
	let settingsPreviewDraft = '';
	let settingsSaving = false;
	let deploySettings: VibeDevDeploySettings | null = null;
	let deploySettingsLoading = false;
	let deploySettingsSaving = false;
	let deploySettingsChecking = false;
	let deployEnabledDraft = false;
	let deployAccountDraft = '';
	let deployTokenDraft = '';
	function openProjectSettings(): void {
		if (!activeVibeProject) return;
		settingsNameDraft = activeVibeProject.name ?? '';
		settingsPreviewDraft = activeVibeProject.preview_url ?? '';
		settingsSaving = false;
		deploySettings = null;
		deployEnabledDraft = false;
		deployAccountDraft = '';
		deployTokenDraft = '';
		settingsOpen = true;
		void loadDeploySettingsPanel();
	}
	function closeProjectSettings(): void {
		settingsOpen = false;
	}
	function applyDeploySettingsToDraft(settings: VibeDevDeploySettings): void {
		deploySettings = settings;
		deployEnabledDraft = settings.enabled;
		deployAccountDraft = settings.account_id ?? '';
		deployTokenDraft = '';
	}
	async function loadDeploySettingsPanel(): Promise<void> {
		if (deploySettingsLoading) return;
		deploySettingsLoading = true;
		try {
			const settings = await vibeDevProjectStore.loadDeploySettings();
			if (settings) applyDeploySettingsToDraft(settings);
		} finally {
			deploySettingsLoading = false;
		}
	}
	function deployTokenStatus(settings: VibeDevDeploySettings | null): string {
		if (!settings) return 'Not loaded';
		if (settings.pages_token_present) {
			return `Pages token saved${settings.pages_token_source ? ` (${settings.pages_token_source})` : ''}`;
		}
		if (settings.generic_token_present) {
			return `Generic token saved${settings.generic_token_source ? ` (${settings.generic_token_source})` : ''}`;
		}
		return 'No token saved';
	}
	function deployAccountStatus(settings: VibeDevDeploySettings | null): string {
		if (!settings?.account_id) return 'No account id saved';
		return settings.account_id_source ? `Saved in ${settings.account_id_source}` : 'Saved';
	}
	function deployCheckMessage(settings: VibeDevDeploySettings | null): string {
		const check = settings?.check;
		if (!check) return '';
		const output = check.output_tail.trim();
		const prefix = check.ok
			? 'Preflight passed.'
			: `Preflight failed${check.error ? `: ${check.error}` : '.'}`;
		return output ? `${prefix}\n${output}` : prefix;
	}
	function deploySettingsRequest(
		options: { clearCredentials?: boolean; clearToken?: boolean } = {}
	) {
		const accountId = deployAccountDraft.trim();
		const token = deployTokenDraft.trim();
		return {
			enabled: options.clearCredentials ? false : deployEnabledDraft,
			account_id: options.clearCredentials ? null : accountId || null,
			pages_api_token: options.clearCredentials || options.clearToken ? null : token || null,
			clear_token: options.clearToken ?? false,
			clear_credentials: options.clearCredentials ?? false
		};
	}
	async function saveDeploySettingsPanel(options: {
		clearCredentials?: boolean;
		clearToken?: boolean;
	} = {}): Promise<void> {
		if (deploySettingsSaving) return;
		deploySettingsSaving = true;
		try {
			const settings = await vibeDevProjectStore.saveDeploySettings(
				deploySettingsRequest(options)
			);
			if (!settings) {
				showError(
					'Could not save deploy settings',
					get(vibeDevProjectStore).error ?? 'Deploy settings update failed.'
				);
				return;
			}
			applyDeploySettingsToDraft(settings);
			showSuccess(
				options.clearCredentials ? 'Deploy credentials cleared' : 'Deploy settings saved',
				settings.enabled ? 'VibeDev publishing is enabled.' : 'VibeDev publishing is disabled.'
			);
		} finally {
			deploySettingsSaving = false;
		}
	}
	async function clearDeployCredentials(): Promise<void> {
		const ok = await requestConfirmation({
			title: 'Clear Cloudflare deploy credentials?',
			message:
				'This removes the Cloudflare account id and token from the active runtime env file and disables VibeDev publishing.',
			confirmLabel: 'Clear credentials',
			destructive: true
		});
		if (!ok) return;
		await saveDeploySettingsPanel({ clearCredentials: true });
	}
	async function checkDeploySettingsPanel(): Promise<void> {
		if (deploySettingsChecking) return;
		deploySettingsChecking = true;
		try {
			deploySettingsSaving = true;
			const saved = await vibeDevProjectStore.saveDeploySettings(deploySettingsRequest());
			deploySettingsSaving = false;
			if (!saved) {
				showError(
					'Could not save deploy settings',
					get(vibeDevProjectStore).error ?? 'Deploy settings update failed.'
				);
				return;
			}
			applyDeploySettingsToDraft(saved);
			const settings = await vibeDevProjectStore.checkDeploySettings();
			if (!settings) {
				showError(
					'Could not check deploy settings',
					get(vibeDevProjectStore).error ?? 'Preflight failed to start.'
				);
				return;
			}
			applyDeploySettingsToDraft(settings);
			if (settings.check?.ok) {
				showSuccess('Cloudflare preflight passed', 'Wrangler can reach Cloudflare Pages.');
			} else {
				showError('Cloudflare preflight failed', settings.check?.error ?? 'See the settings panel output.');
			}
		} finally {
			deploySettingsSaving = false;
			deploySettingsChecking = false;
		}
	}
	async function saveProjectSettings(): Promise<void> {
		const project = activeVibeProject;
		if (!project || settingsSaving) return;
		const name = settingsNameDraft.trim();
		const previewUrl = settingsPreviewDraft.trim();
		const patch: { name?: string; preview_url?: string } = {};
		if (name && name !== project.name) patch.name = name;
		if (previewUrl !== (project.preview_url ?? '')) patch.preview_url = previewUrl;
		if (Object.keys(patch).length === 0) {
			settingsOpen = false;
			return;
		}
		settingsSaving = true;
		try {
			const updated = await vibeDevProjectStore.updateProject(project.project_id, patch);
			if (!updated) {
				showError(
					'Could not save project settings',
					get(vibeDevProjectStore).error ?? 'Settings update failed.'
				);
				return;
			}
			settingsOpen = false;
			showSuccess('Project settings saved', updated.name);
		} finally {
			settingsSaving = false;
		}
	}

	// Repo + delete actions exposed in the settings panel.
	let deletingProject = false;
	let confirmDeleteProject = false;
	$: if (!settingsOpen) confirmDeleteProject = false;
	$: projectRunCount = activeVibeProject
		? runs.filter((run) => projectIdFromDescription(run.description) === activeVibeProject!.project_id)
				.length
		: 0;
	async function openRepoFolder(): Promise<void> {
		const project = activeVibeProject;
		if (!project) return;
		const ok = await vibeDevProjectStore.openRepo(project.project_id);
		if (ok)
			showSuccess('Opened repo folder', project.repo_absolute_path ?? project.repo_path ?? '');
		else showError('Could not open the repo folder', 'The server could not reveal the directory.');
	}
	async function deleteRunTask(taskId: string): Promise<void> {
		const runTitle = stripRunTitlePrefix(vibeTasksById.get(taskId)?.title ?? '').trim();
		const ok = await requestConfirmation({
			title: 'Delete this run?',
			message: `${runTitle ? `"${runTitle}"` : 'This run'} and its files, diffs, and logs are permanently removed. This cannot be undone.`,
			confirmLabel: 'Delete run',
			destructive: true
		});
		if (!ok) return;
		await taskStore.deleteTask(taskId, { removeFiles: true });
		if (taskId === activeTaskId) void goto('/vibe', { keepFocus: true, noScroll: true });
	}
	async function deleteProjectCascade(): Promise<void> {
		const project = activeVibeProject;
		if (!project || deletingProject) return;
		deletingProject = true;
		try {
			// Cascade: delete the project's runs first (each removes its task dir +
			// durable coding log + progress-channel log via the backend cleanup),
			// then the project record + chat session.
			// Use the UNFOLDED task set (`allVibeDevTasks`): `runs` now folds threaded follow-up
			// turns out of the rail, so deleting by `runs` would ORPHAN those tasks' dirs/logs.
			const projectRuns = allVibeDevTasks.filter(
				(run) => projectIdFromDescription(run.description) === project.project_id
			);
			for (const run of projectRuns) {
				await taskStore.deleteTask(run.id, { removeFiles: true });
			}
			const result = await vibeDevProjectStore.deleteProject(project.project_id);
			if (!result) {
				showError(
					'Could not delete the project',
					get(vibeDevProjectStore).error ?? 'Delete failed.'
				);
				return;
			}
			showSuccess(
				'Project deleted',
				`${project.name}${projectRuns.length ? ` + ${projectRuns.length} run(s)` : ''}`
			);
			settingsOpen = false;
			confirmDeleteProject = false;
			void goto('/vibe', { keepFocus: true, noScroll: true });
		} finally {
			deletingProject = false;
		}
	}
	async function startStarter(): Promise<void> {
		// Blessed starter (S3): create a project and seed it with a SvelteKit +
		// Vite + Tailwind scaffold prompt (Pi runs the create + install). A
		// pre-warmed COW template for instant greenfield preview is the
		// optimization → plan §13.3 #7.
		const project = await vibeDevProjectStore.createProject({ name: 'SvelteKit Starter' });
		if (!project) {
			showError('Could not create starter project', get(vibeDevProjectStore).error ?? 'Creation failed.');
			return;
		}
		await goto(vibePath(project.project_id), { keepFocus: true, noScroll: true });
		prompt =
			'Scaffold a new SvelteKit app (Vite + TypeScript + Tailwind CSS) in this project using the official skeleton template, install dependencies, add a clean landing page, and make sure `npm run dev` works.';
	}

	async function cloneRepo(url: string, name: string): Promise<void> {
		// Buildable-now path: create a default project, then send a first Pi prompt
		// that clones the repo via bash (a dedicated repo_git_url field is a
		// fast-follow — plan ledger §13.3).
		const project = await vibeDevProjectStore.createProject({ name });
		if (!project) {
			showError('Could not create project for clone', get(vibeDevProjectStore).error ?? 'Creation failed.');
			return;
		}
		await goto(vibePath(project.project_id), { keepFocus: true, noScroll: true });
		prompt = `Clone ${url} into the project repo (git clone), then install dependencies and report the project structure.`;
	}

	// HITL action handlers
	const onApplyDiff = (e: CustomEvent<{ request: HitlRequest }>) => void resolveDiff(e.detail.request, 'apply');
	const onRejectDiff = (e: CustomEvent<{ request: HitlRequest }>) => void resolveDiff(e.detail.request, 'reject');
	const onApplyFile = (e: CustomEvent<{ request: HitlRequest; path: string }>) => void applyDiffFile(e.detail.request, e.detail.path);
	const onRejectFile = (e: CustomEvent<{ request: HitlRequest; path: string }>) => void rejectDiffFile(e.detail.request, e.detail.path);
	const onApproveAll = () => void approveAll(orderedDiffRows);
	const onRespond = (e: CustomEvent<{ row: VibeRow }>) => void respondToVibeRow(e.detail.row);
	function onToggleAutoApply(e: CustomEvent<{ value: boolean }>): void {
		vibeStudioStore.setAutoApply(e.detail.value);
		if (e.detail.value) resetAutoApplyMemo();
	}
</script>

<div class="studio" class:studio--rail-collapsed={!studioState.railExpanded}>
	<StudioRail
		{projects}
		activeProject={activeVibeProject}
		{runs}
		llmTasks={allLoadedVibeDevTasks}
		runCostTaskIds={runCostTaskIds}
		activeTaskId={activeRunRowId}
		checkpoints={$vibeCheckpoints}
		expanded={studioState.railExpanded}
		busy={$vibeDevProjectStore.isLoading}
		on:newRun={startFresh}
		on:selectCheckpoint={(e) => rewindToCheckpoint(e.detail.taskId, e.detail.checkpointId)}
		on:selectRun={(e) => selectRun(e.detail.taskId)}
		on:deleteRun={(e) => void deleteRunTask(e.detail.taskId)}
		on:selectProject={(e) => void selectProject(e.detail.projectId)}
		on:createProject={(e) => void createProject(e.detail.name, e.detail.repoPath)}
		on:cloneRepo={(e) => void cloneRepo(e.detail.url, e.detail.name)}
		on:startStarter={() => void startStarter()}
		on:openSettings={openProjectSettings}
		on:toggleRail={() => vibeStudioStore.toggleRail()}
	/>

	<section class="conversation" aria-label="Conversation">
		{#if showHero}
			<div class="hero">
				<h1 class="hero__title">What should we build?</h1>
				<p class="hero__sub">Describe it. Watch your build take shape live, and steer it as it grows.</p>
				<div class="hero__chips">
					{#each EXAMPLE_CHIPS as chip}
						<button type="button" class="hero__chip" on:click={() => applyChip(chip)}>{chip}</button>
					{/each}
				</div>
			</div>
		{:else}
			<ConversationSpine
				cards={chainCards}
				runKey={activeVibeTask?.id ?? ''}
				live={runLive}
				streamState={spine.streamState}
				reconnecting={streamReconnecting}
				historical={taskTerminal}
				{coordinatorTerminal}
				synthesizing={runSynthesizing}
				loadingTurn={loadingNewTurn}
				{diffRowsByProposal}
				{sessionId}
				on:reviewDiff={(e) => reviewDiffInStage(e.detail.proposalId)}
			/>
		{/if}

		{#if showStuck && stuck}
			<StuckBanner
				toolName={stuck.toolName}
				args={stuck.args}
				on:redirect={redirectStuck}
				on:dismiss={dismissStuck}
			/>
		{/if}

		{#if runLive || canStop || canDismissRun}
			<div class="conversation__live-bar">
				<span class="conversation__live-dot" aria-hidden="true"></span>
				<span class="conversation__live-text">{canDismissRun
					? 'Prepared — never started'
					: canSteer
						? 'Building — type to steer it'
						: runLive
							? 'Building…'
							: runSynthesizing
								? 'Synthesizing…'
								: 'Run in progress'}</span>
				{#if canDismissRun}
					<!-- Never-started orphan: the one control IS the dismiss action — no menu. -->
					<button
						type="button"
						class="conversation__cancel"
						on:click={dismissRun}
						disabled={controlling}
						aria-label="Discard this run"
						title="This run was prepared but never started. Discard it (marks it cancelled — no files removed)."
					>✕ Discard</button>
				{:else}
					<div class="conversation__stop-group" role="group" aria-label="Stop controls">
						<button
							type="button"
							class="conversation__stop conversation__stop--primary"
							on:click={stopRun}
							disabled={controlling || !stopRunId}
							aria-label="Stop the current step"
							title="Stop the current step — the coding agent wraps up the in-flight step so you can redirect"
						>■ Stop</button>
						<button
							type="button"
							class="conversation__stop conversation__stop--more"
							bind:this={stopMenuTriggerEl}
							on:click|stopPropagation={toggleStopMenu}
							aria-haspopup="menu"
							aria-expanded={stopMenuOpen}
							aria-label="More stop options"
						><Icon name="chevron-down" size={11} /></button>
						{#if stopMenuOpen}
							<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
							<div
								bind:this={stopMenuEl}
								class="conversation__stop-menu"
								role="menu"
								aria-orientation="vertical"
								aria-label="Stop options"
								tabindex="-1"
								use:clickOutside={{ handler: () => closeStopMenu(false), exclude: [stopMenuTriggerEl] }}
								on:keydown={handleStopMenuKeydown}
							>
								<button
									type="button"
									role="menuitem"
									class="conversation__stop-menu-item conversation__stop-menu-item--danger"
									disabled={controlling || !activeVibeTask?.executionId}
									on:click={() => {
										closeStopMenu(false);
										void cancelRun();
									}}
								>
									<span class="conversation__stop-menu-label">✕ Cancel entire run</span>
									<span class="conversation__stop-menu-hint"
										>Terminal — stops the whole execution tree so it cannot re-delegate.</span
									>
								</button>
							</div>
						{/if}
					</div>
				{/if}
			</div>
		{/if}

		{#if seedContent}
			<div class="conversation__seed">
				<span class="conversation__seed-icon" aria-hidden="true"><Icon name="git-branch" size={13} /></span>
				<span class="conversation__seed-text">Seeded from {seedLabel || 'another surface'}</span>
				<button
					type="button"
					class="conversation__seed-toggle"
					on:click={() => (seedPreviewOpen = !seedPreviewOpen)}
					aria-expanded={seedPreviewOpen}
				>{seedPreviewOpen ? 'Hide' : 'Preview'}</button>
				<button
					type="button"
					class="conversation__seed-x"
					on:click={() => { seedContent = null; seedLabel = ''; }}
					aria-label="Remove seeded context"
				>✕</button>
			</div>
			{#if seedPreviewOpen}
				<pre class="conversation__seed-preview">{seedContent}</pre>
			{/if}
		{/if}

		<div class="conversation__composer">
			{#if studioState.mode === 'autopilot'}
				<div class="conversation__autopilot">
					<span class="conversation__autopilot-icon" aria-hidden="true"><Icon name="zap" size={14} /></span>
					<span class="conversation__autopilot-text"
						>Autopilot — runs unattended on a branch, pings you when done.</span
					>
					<label class="conversation__budget" title="Stop and report rather than spend past this (USD)">
						<span aria-hidden="true">$</span>
						<input
							type="number"
							min="0"
							step="1"
							placeholder="budget"
							value={studioState.costBudgetUsd ?? ''}
							on:change={(e) =>
								vibeStudioStore.setCostBudget(Number((e.currentTarget as HTMLInputElement).value) || null)}
							aria-label="Cost budget in USD"
						/>
					</label>
					<div class="conversation__nightly">
						<Checkbox
							label="Run nightly"
							checked={nightly}
							on:change={(event) => (nightly = event.detail.checked)}
						/>
					</div>
				</div>
			{/if}
			<div class="conversation__save-row">
				<button
					type="button"
					class="conversation__app-options"
					on:click={() => (appOptionsOpen = true)}
					title="Pick tools, agents, personalities and procedures from the same catalog as magician app tools list"
				>
					App options
				</button>
				<div
					class="conversation__save-as-task"
					title="Off: this run stays in the cockpit only (internal). On: save it to your task list (/tasks)."
				>
					<Checkbox
						label="Save as task"
						checked={saveAsRunTask}
						on:change={(event) => (saveAsRunTask = event.detail.checked)}
					/>
				</div>
				<span class="conversation__save-hint">Saved runs appear in /tasks and survive this studio.</span>
			</div>
			<VibeComposer
				bind:this={composerEl}
				bind:value={prompt}
				mentionItems={vibeTaskMentionItems}
				mode={composerMode}
				studioMode={studioState.mode}
				placeholder={canSteer
					? 'Steer the live build…'
					: composerMode === 'follow_up'
						? 'Ask for the next change on this run…'
						: newBuildPlaceholder}
				profiles={profileState.profiles}
				blockedProfiles={profileState.blocked}
				selectedProfileId={profileState.selected}
				{selectedProfile}
				profileLocked={Boolean(activeVibeTask && !taskTerminal)}
				profileError={profileState.error}
				{supportsImages}
				{stagedAttachments}
				submitDisabled={Boolean(submitBlocker)}
				{submitBlocker}
				submitting={dispatching}
				uploading={attachmentUploading}
				activeRun={activeRunSummary}
				on:submit={handleSubmit}
				on:selectProfile={(e) => codingProfileStore.select(e.detail.id)}
				on:attachFiles={(e) => void uploadFiles(e.detail.files)}
				on:removeAttachment={(e) => removeAttachment(e.detail.attachmentId)}
				on:newRun={startFresh}
				on:setStudioMode={(e) => vibeStudioStore.setMode(e.detail)}
				on:input={() => (voicePrefix = null)}
				on:micCapture={(e) => void uploadFiles([e.detail.file])}
				on:micTranscribe={(e) => onMicTranscribe(e.detail.transcript)}
				on:micTranscribeDelta={(e) => onMicTranscribeDelta(e.detail.transcript)}
			/>
		</div>
	</section>

	<StudioStage
		firstRun={stageFirstRun}
		projectId={activeProjectId}
		activeProject={activeVibeProject}
		pinnedUrl={activeVibeProject?.preview_url ?? null}
		{previewAvailable}
		{editMode}
		on:toggleEdit={toggleEditMode}
		on:seedPrompt={(e) => {
			prompt = e.detail.text;
			// Seeding is an invitation to edit/send — put the caret in the composer.
			composerEl?.focus();
		}}
		checks={projectChecks}
		on:attemptFix={(e) => attemptFix(e.detail.result)}
		meta={runMeta}
		{changedFiles}
		{testCards}
		{orderedDiffRows}
		{orderedOtherRows}
		focusDiffPath={stageFocusDiffPath}
		bulkApplying={action.bulkApplying}
		actingKeys={action.actingKeys}
		on:approveAll={onApproveAll}
		on:toggleAutoApply={onToggleAutoApply}
		on:applyDiff={onApplyDiff}
		on:rejectDiff={onRejectDiff}
		on:applyFile={onApplyFile}
		on:rejectFile={onRejectFile}
		on:respond={onRespond}
	/>

	<TerminalDrawer
		open={studioState.terminalOpen}
		on:close={() => vibeStudioStore.toggleTerminal(false)}
	/>

	{#if selectedEl}
		<div class="eledit" role="dialog" aria-label="Edit selected element">
			<header class="eledit__head">
				<span class="eledit__tag">&lt;{selectedEl.tag}&gt;</span>
				{#if selectedEl.loc}
					<span class="eledit__loc" title="Source location">{selectedEl.loc}</span>
				{:else}
					<span class="eledit__loc eledit__loc--pi" title="No source map — routed to the coding agent">→ Coding agent</span>
				{/if}
				<button type="button" class="eledit__close" on:click={() => (selectedEl = null)} aria-label="Close">✕</button>
			</header>
			{#if selectedEl.text}
				<p class="eledit__text">“{selectedEl.text}”</p>
			{/if}
			<p class="eledit__selector">{selectedEl.selector}</p>
			<textarea
				class="eledit__input"
				bind:value={elementChange}
				rows="2"
				placeholder="Describe the change (e.g. make it teal, larger, say ‘Buy now’)…"
				on:keydown={(e) => {
					if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
						e.preventDefault();
						void applyElementEdit();
					}
				}}
			></textarea>
			<div class="eledit__actions">
				<span class="eledit__hint">Tip: double-click any element to edit its text inline.</span>
				<button type="button" class="vbtn vbtn--primary" disabled={!elementChange.trim() || dispatching} on:click={() => void applyElementEdit()}>
					{dispatching ? 'Sending…' : 'Apply change'}
				</button>
			</div>
		</div>
	{/if}

	<AppOptionsPanel
		open={appOptionsOpen}
		on:close={() => (appOptionsOpen = false)}
		on:insert={(event) => {
			const block = event.detail.text.trim();
			if (!block) return;
			prompt = prompt.trim() ? `${prompt.trim()}\n\n${block}` : block;
			appOptionsOpen = false;
			composerEl?.focus();
		}}
	/>

	{#if settingsOpen && activeVibeProject}
		<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
		<div
			class="settings-overlay"
			role="dialog"
			aria-modal="true"
			aria-label="Project settings"
			tabindex="-1"
			on:click|self={closeProjectSettings}
		>
			<div class="settings-panel">
				<h2 class="settings-title">Project settings</h2>
				<label class="settings-field">
					<span>Name</span>
					<input type="text" bind:value={settingsNameDraft} placeholder="Project name" />
				</label>
				<label class="settings-field">
					<span>Preview URL</span>
					<input
						type="url"
						bind:value={settingsPreviewDraft}
						placeholder="http://localhost:5173"
					/>
				</label>
				<div class="settings-repo">
					<div class="settings-repo__head">
						<span class="settings-repo__label">Repo folder</span>
						<button type="button" class="settings-link" on:click={() => void openRepoFolder()}
							>Open folder ↗</button
						>
					</div>
					<code class="settings-repo__path"
						>{activeVibeProject.repo_absolute_path ||
							activeVibeProject.repo_path ||
							'.'}</code
					>
					{#if activeVibeProject.repo_display_path && activeVibeProject.repo_display_path !== activeVibeProject.repo_absolute_path}
						<span class="settings-repo__hint">{activeVibeProject.repo_display_path}</span>
					{/if}
				</div>

				<div class="settings-deploy">
					<div class="settings-deploy__head">
						<div>
							<h3 class="settings-section-title">Deploy</h3>
							<span class="settings-deploy__status">
								{deploySettingsLoading ? 'Loading Cloudflare settings…' : deployTokenStatus(deploySettings)}
							</span>
						</div>
						<span
							class:settings-deploy__badge--on={deploySettings?.enabled}
							class="settings-deploy__badge"
						>
							{deploySettings?.enabled ? 'Enabled' : 'Disabled'}
						</span>
					</div>
					<Checkbox
						label="Enable VibeDev publishing"
						checked={deployEnabledDraft}
						disabled={deploySettingsLoading || deploySettingsSaving}
						idBase="vibedev-deploy-enabled"
						on:change={(event) => (deployEnabledDraft = event.detail.checked)}
					/>
					<label class="settings-field">
						<span>Cloudflare account id</span>
						<input
							type="text"
							bind:value={deployAccountDraft}
							placeholder="587bc897538997f43e63f15180fa1642"
							autocomplete="off"
							disabled={deploySettingsLoading || deploySettingsSaving}
						/>
						<small>{deployAccountStatus(deploySettings)}</small>
					</label>
					<label class="settings-field">
						<span>Cloudflare Pages token</span>
						<input
							type="password"
							bind:value={deployTokenDraft}
							placeholder={deploySettings?.pages_token_present || deploySettings?.generic_token_present
								? 'Leave blank to keep current token'
								: 'Paste Pages API token'}
							autocomplete="new-password"
							disabled={deploySettingsLoading || deploySettingsSaving}
						/>
						<small>{deployTokenStatus(deploySettings)}</small>
					</label>
					<div class="settings-deploy__meta">
						<span title={deploySettings?.config_path ?? ''}>Config: {deploySettings?.config_path ?? 'not loaded'}</span>
						<span title={deploySettings?.env_target_path ?? ''}>Writes to: {deploySettings?.env_target_path ?? 'not loaded'}{deploySettings?.env_target_mode ? ` (${deploySettings.env_target_mode})` : ''}</span>
						<span title={deploySettings?.env_development_path ?? ''}>Development env: {deploySettings?.env_development_path ?? 'not loaded'}</span>
						<span title={deploySettings?.env_path ?? ''}>Production env: {deploySettings?.env_path ?? 'not loaded'}</span>
					</div>
					{#if deploySettings?.check}
						<pre
							class:settings-deploy__check--ok={deploySettings.check.ok}
							class="settings-deploy__check">{deployCheckMessage(deploySettings)}</pre>
					{/if}
					<div class="settings-deploy__actions">
						<button
							type="button"
							class="settings-btn"
							disabled={deploySettingsLoading || deploySettingsSaving}
							on:click={() => void saveDeploySettingsPanel()}
						>
							{deploySettingsSaving ? 'Saving deploy…' : 'Save deploy'}
						</button>
						<button
							type="button"
							class="settings-btn"
							disabled={deploySettingsLoading || deploySettingsChecking || deploySettingsSaving}
							on:click={() => void checkDeploySettingsPanel()}
						>
							{deploySettingsChecking ? 'Checking…' : 'Test Cloudflare'}
						</button>
						<button
							type="button"
							class="settings-link settings-link--danger"
							disabled={deploySettingsLoading || deploySettingsSaving}
							on:click={() => void clearDeployCredentials()}
						>
							Clear credentials
						</button>
					</div>
				</div>

				<div class="settings-danger">
					{#if confirmDeleteProject}
						<span class="settings-danger__warn">
							Delete “{activeVibeProject.name}”{projectRunCount
								? ` and its ${projectRunCount} run${projectRunCount === 1 ? '' : 's'}`
								: ''}? This can't be undone.
						</span>
						<div class="settings-danger__actions">
							<button type="button" class="settings-btn" on:click={() => (confirmDeleteProject = false)}
								>Keep</button
							>
							<button
								type="button"
								class="settings-btn settings-btn--danger"
								disabled={deletingProject}
								on:click={() => void deleteProjectCascade()}
							>
								{deletingProject ? 'Deleting…' : 'Delete everything'}
							</button>
						</div>
					{:else}
						<button
							type="button"
							class="settings-link settings-link--danger"
							on:click={() => (confirmDeleteProject = true)}
						>
							Delete project{projectRunCount
								? ` + ${projectRunCount} run${projectRunCount === 1 ? '' : 's'}`
								: ''}
						</button>
					{/if}
				</div>

				<div class="settings-actions">
					<button type="button" class="settings-btn" on:click={closeProjectSettings}>Cancel</button>
					<button
						type="button"
						class="settings-btn settings-btn--primary"
						disabled={settingsSaving}
						on:click={() => void saveProjectSettings()}
					>
						{settingsSaving ? 'Saving…' : 'Save'}
					</button>
				</div>
			</div>
		</div>
	{/if}
</div>

<style>
	.settings-overlay {
		position: fixed;
		inset: 0;
		z-index: 60;
		display: grid;
		place-items: center;
		padding: 1.5rem;
		background: color-mix(in srgb, var(--bg-surface, #000) 68%, transparent);
		animation: studio-rise 0.12s ease;
	}
	.settings-panel {
		width: min(560px, 100%);
		max-height: min(88vh, 48rem);
		overflow: auto;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		padding: 1.25rem;
		border-radius: 14px;
		border: 1px solid var(--vibe-border-strong, var(--vibe-border));
		background: var(--vibe-surface);
		color: var(--vibe-text);
		box-shadow: 0 24px 60px rgba(0, 0, 0, 0.35);
	}
	.settings-title {
		margin: 0;
		font-size: 1.05rem;
		font-weight: 650;
	}
	.settings-field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.82rem;
		color: var(--vibe-text-muted);
	}
	.settings-field input {
		padding: 0.5rem 0.65rem;
		border-radius: 9px;
		border: 1px solid var(--vibe-border);
		background: var(--vibe-page-surface, var(--vibe-surface));
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.9rem;
	}
	.settings-field input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 70%, transparent);
		outline-offset: 1px;
	}
	.settings-field small {
		font-size: 0.72rem;
		line-height: 1.35;
		color: var(--vibe-text-muted);
		overflow-wrap: anywhere;
	}
	.settings-repo {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		padding: 0.6rem 0.7rem;
		border: 1px solid var(--vibe-border);
		border-radius: 9px;
		background: var(--vibe-page-surface, var(--vibe-surface));
	}
	.settings-repo__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}
	.settings-repo__label {
		font-size: 0.78rem;
		font-weight: 600;
		color: var(--vibe-text-muted);
	}
	.settings-repo__path {
		font-size: 0.82rem;
		color: var(--vibe-text);
		word-break: break-all;
		user-select: all;
	}
	.settings-repo__hint {
		font-size: 0.74rem;
		color: var(--vibe-text-muted);
	}
	.settings-link {
		border: none;
		background: none;
		padding: 0;
		font: inherit;
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--vibe-accent);
		cursor: pointer;
	}
	.settings-link:hover {
		text-decoration: underline;
	}
	.settings-link:disabled {
		opacity: 0.55;
		cursor: not-allowed;
		text-decoration: none;
	}
	.settings-link--danger {
		color: var(--color-danger, #d9534f);
		align-self: flex-start;
	}
	.settings-section-title {
		margin: 0;
		font-size: 0.9rem;
		font-weight: 650;
		color: var(--vibe-text);
	}
	.settings-deploy {
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
		padding: 0.75rem;
		border: 1px solid var(--vibe-border);
		border-radius: 9px;
		background: var(--vibe-page-surface, var(--vibe-surface));
	}
	.settings-deploy__head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.75rem;
	}
	.settings-deploy__head > div {
		min-width: 0;
	}
	.settings-deploy__status {
		display: block;
		margin-top: 0.15rem;
		font-size: 0.74rem;
		color: var(--vibe-text-muted);
		overflow-wrap: anywhere;
	}
	.settings-deploy__badge {
		flex: 0 0 auto;
		padding: 0.12rem 0.45rem;
		border: 1px solid var(--vibe-border);
		border-radius: 999px;
		background: color-mix(in srgb, var(--vibe-text) 6%, transparent);
		color: var(--vibe-text-muted);
		font-size: 0.68rem;
		font-weight: 700;
		text-transform: uppercase;
	}
	.settings-deploy__badge--on {
		border-color: color-mix(in srgb, var(--vibe-success) 55%, transparent);
		background: color-mix(in srgb, var(--vibe-success) 12%, transparent);
		color: var(--vibe-success);
	}
	.settings-deploy__meta {
		display: grid;
		gap: 0.18rem;
		font-size: 0.7rem;
		color: var(--vibe-text-muted);
	}
	.settings-deploy__meta span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.settings-deploy__check {
		max-height: 8rem;
		overflow: auto;
		margin: 0;
		padding: 0.55rem 0.65rem;
		border: 1px solid color-mix(in srgb, var(--vibe-error) 35%, var(--vibe-border));
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-error) 7%, transparent);
		color: var(--vibe-text);
		font: 0.72rem/1.45 var(--font-mono, monospace);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.settings-deploy__check--ok {
		border-color: color-mix(in srgb, var(--vibe-success) 35%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-success) 7%, transparent);
	}
	.settings-deploy__actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: 0.55rem;
	}
	.settings-danger {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding-top: 0.25rem;
		border-top: 1px solid var(--vibe-border);
	}
	.settings-danger__warn {
		font-size: 0.8rem;
		color: var(--vibe-text);
	}
	.settings-danger__actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}
	.settings-btn--danger {
		border-color: color-mix(in srgb, var(--color-danger, #d9534f) 60%, transparent);
		background: var(--color-danger, #d9534f);
		color: #fff;
	}
	.settings-btn--danger:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	.settings-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 0.25rem;
	}
	.settings-btn {
		min-height: 2rem;
		padding: 0.4rem 0.85rem;
		border-radius: 9px;
		border: 1px solid var(--vibe-border-strong, var(--vibe-border));
		background: transparent;
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.85rem;
		font-weight: 600;
		cursor: pointer;
	}
	.settings-btn:hover:not(:disabled) {
		background: color-mix(in srgb, var(--vibe-text) 8%, transparent);
	}
	.settings-btn:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	.settings-btn--primary {
		border-color: color-mix(in srgb, var(--vibe-accent) 72%, transparent);
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
	}
	.settings-btn--primary:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}

	.studio {
		/* --vibe-* aliases map to the real app.css house tokens so the reused
		   VibeComposer / VibePreviewPanel / DiffStrip render correctly without
		   the monolith's admin-dashboard overrides. */
		--vibe-accent: var(--accent-primary);
		--vibe-surface: var(--bg-card);
		--vibe-page-surface: var(--bg-surface);
		--vibe-text: var(--text-primary);
		--vibe-text-muted: var(--text-muted);
		--vibe-border: var(--border-soft);
		--vibe-border-strong: var(--border-default);
		--vibe-success: var(--color-success);
		--vibe-warning: var(--color-warning);
		--vibe-error: var(--color-error);

		display: grid;
		grid-template-columns: minmax(13rem, 17rem) minmax(26rem, 1.25fr) minmax(24rem, 1fr);
		height: calc(100vh - var(--app-header-height, 48px));
		min-height: 0;
		width: 100%;
		max-width: none;
		background: var(--bg-surface);
		color: var(--text-primary);
		overflow: hidden;
	}
	.studio--rail-collapsed {
		grid-template-columns: 3.2rem minmax(26rem, 1.4fr) minmax(24rem, 1fr);
	}

	/* Consistent keyboard focus ring across every cockpit control, including
	   the reused VibeComposer / VibePreviewPanel / DiffStrip. (U6 a11y bar.) */
	.studio :global(button:focus-visible),
	.studio :global(a:focus-visible),
	.studio :global(input:focus-visible),
	.studio :global(select:focus-visible),
	.studio :global(textarea:focus-visible),
	.studio :global([tabindex]:focus-visible) {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 70%, transparent);
		outline-offset: 2px;
	}

	.conversation {
		display: flex;
		flex-direction: column;
		min-height: 0;
		min-width: 0;
		background: var(--bg-card);
		border-left: 1px solid var(--border-soft);
		border-right: 1px solid var(--border-soft);
	}
	.conversation__composer {
		border-top: 1px solid var(--border-soft);
		padding: 0.7rem 0.8rem;
		background: color-mix(in srgb, var(--bg-surface) 40%, var(--bg-card));
	}

	.conversation__seed {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin: 0 0.8rem;
		padding: 0.4rem 0.75rem;
		border: 1px solid color-mix(in srgb, var(--accent-secondary, var(--accent-primary)) 30%, var(--border-soft));
		border-radius: var(--radius-md, 18px);
		background: color-mix(in srgb, var(--accent-secondary, var(--accent-primary)) 7%, var(--bg-card));
		font-size: 0.78rem;
		color: var(--text-secondary);
	}
	.conversation__seed-icon {
		display: inline-flex;
		align-items: center;
		color: var(--accent-secondary, var(--accent-primary));
	}
	.conversation__seed-text {
		flex: 1;
		min-width: 0;
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.conversation__seed-toggle {
		border: 1px solid var(--border-default);
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--text-secondary);
		font: inherit;
		font-size: 0.72rem;
		font-weight: 600;
		padding: 0.12rem 0.6rem;
		cursor: pointer;
	}
	.conversation__seed-x {
		border: 0;
		background: transparent;
		color: var(--text-muted);
		font: inherit;
		cursor: pointer;
		padding: 0.1rem 0.3rem;
	}
	.conversation__seed-preview {
		margin: 0.3rem 0.8rem 0;
		max-height: 12rem;
		overflow: auto;
		padding: 0.6rem 0.75rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 10px);
		background: var(--bg-surface);
		color: var(--text-secondary);
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		line-height: 1.5;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.conversation__autopilot {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin: 0 0 0.55rem;
		padding: 0.45rem 0.75rem;
		border: 1px solid color-mix(in srgb, var(--color-success) 30%, var(--border-soft));
		border-radius: var(--radius-md, 18px);
		background: color-mix(in srgb, var(--color-success) 7%, var(--bg-card));
		font-size: 0.78rem;
		color: var(--text-secondary);
	}
	.conversation__autopilot-icon {
		display: inline-flex;
		align-items: center;
		color: var(--color-success);
	}
	.conversation__autopilot-text {
		flex: 1;
		min-width: 0;
		line-height: 1.35;
	}
	.conversation__nightly {
		display: inline-flex;
		align-items: center;
		font-weight: 600;
		color: var(--text-primary);
		white-space: nowrap;
	}

	:global(.conversation__nightly .muij-checkbox),
	:global(.conversation__save-as-task .muij-checkbox) {
		font-size: inherit;
		font-weight: inherit;
		color: inherit;
	}

	.conversation__budget {
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		font-weight: 600;
		color: var(--text-secondary);
		white-space: nowrap;
	}
	.conversation__budget input {
		width: 4.5rem;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-sm, 8px);
		background: var(--bg-surface);
		color: var(--text-primary);
		font: inherit;
		font-size: 0.74rem;
		padding: 0.15rem 0.35rem;
	}

	.conversation__save-row {
		display: flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: 0.45rem;
		margin: 0 0 0.4rem;
	}
	.conversation__app-options {
		border: 1px solid var(--vibe-border, var(--border-soft));
		background: transparent;
		color: var(--text-secondary);
		border-radius: 999px;
		padding: 0.15rem 0.65rem;
		font-size: var(--text-xs);
		font-weight: 650;
		cursor: pointer;
	}
	.conversation__save-as-task {
		display: inline-flex;
		align-items: center;
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--text-secondary);
		white-space: nowrap;
	}

	.conversation__save-hint {
		font-size: var(--text-2xs);
		color: var(--text-muted);
	}

	.conversation__live-bar {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin: 0 0.8rem;
		padding: 0.4rem 0.7rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 7%, var(--bg-card));
		font-size: 0.76rem;
		font-weight: 600;
		color: var(--text-secondary);
	}
	.conversation__live-dot {
		width: 0.55rem;
		height: 0.55rem;
		border-radius: 999px;
		background: var(--color-success);
		animation: studio-pulse 1.4s ease-in-out infinite;
	}
	.conversation__live-text {
		flex: 1;
		min-width: 0;
	}
	/* Stop split-button: primary "■ Stop" (step-level steer, soft) + a chevron
	   opening the menu that carries the terminal "Cancel entire run". */
	.conversation__stop-group {
		position: relative;
		display: inline-flex;
		align-items: stretch;
	}
	.conversation__stop {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		background: var(--bg-card);
		color: var(--text-secondary);
		font: inherit;
		font-size: 0.74rem;
		font-weight: 600;
		padding: 0.2rem 0.7rem;
		cursor: pointer;
	}
	.conversation__stop:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	.conversation__stop--primary {
		border-radius: var(--radius-full, 999px) 0 0 var(--radius-full, 999px);
		border-right-width: 0;
	}
	.conversation__stop--more {
		display: inline-flex;
		align-items: center;
		border-radius: 0 var(--radius-full, 999px) var(--radius-full, 999px) 0;
		padding: 0.2rem 0.45rem 0.2rem 0.35rem;
	}
	.conversation__stop--more:hover,
	.conversation__stop--more[aria-expanded='true'] {
		background: var(--bg-soft);
	}
	/* Opens UPWARD so it never covers the composer's type-to-steer input. */
	.conversation__stop-menu {
		position: absolute;
		bottom: calc(100% + 0.35rem);
		right: 0;
		z-index: 30;
		min-width: 15.5rem;
		display: flex;
		flex-direction: column;
		padding: 0.25rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.6rem;
		background: var(--bg-card);
		box-shadow: var(--shadow-lg, 0 8px 24px rgba(0, 0, 0, 0.25));
	}
	.conversation__stop-menu-item {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		width: 100%;
		text-align: left;
		background: none;
		border: none;
		border-radius: 0.4rem;
		padding: 0.4rem 0.5rem;
		cursor: pointer;
		font: inherit;
	}
	.conversation__stop-menu-item:hover:not(:disabled),
	.conversation__stop-menu-item:focus-visible {
		background: color-mix(in srgb, var(--text-primary) 8%, transparent);
		outline: none;
	}
	.conversation__stop-menu-item--danger:hover:not(:disabled),
	.conversation__stop-menu-item--danger:focus-visible {
		background: color-mix(in srgb, var(--color-error) 10%, transparent);
	}
	.conversation__stop-menu-item:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	.conversation__stop-menu-label {
		font-size: 0.78rem;
		font-weight: 600;
	}
	.conversation__stop-menu-item--danger .conversation__stop-menu-label {
		color: var(--color-error);
	}
	.conversation__stop-menu-hint {
		font-size: var(--text-2xs, 0.68rem);
		font-weight: 400;
		line-height: 1.35;
		color: var(--text-muted);
		white-space: normal;
	}
	/* Red pill — the never-started orphan's "✕ Discard" (soft cancel, no menu). */
	.conversation__cancel {
		border: 1px solid color-mix(in srgb, var(--color-error) 50%, transparent);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--color-error) 12%, var(--bg-card));
		color: var(--color-error);
		font: inherit;
		font-size: 0.74rem;
		font-weight: 600;
		padding: 0.2rem 0.7rem;
		cursor: pointer;
	}
	.conversation__cancel:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	@keyframes studio-pulse {
		0%,
		100% {
			opacity: 0.4;
		}
		50% {
			opacity: 1;
		}
	}

	.hero {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.7rem;
		padding: 2rem 1.5rem;
		text-align: center;
	}
	.hero__title {
		margin: 0;
		font-family: var(--font-display, inherit);
		font-size: clamp(1.4rem, 3vw, 2rem);
		font-weight: 600;
		color: var(--text-primary);
	}
	.hero__sub {
		margin: 0;
		max-width: 30rem;
		color: var(--text-muted);
		font-size: 0.95rem;
		line-height: 1.5;
	}
	.hero__chips {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		justify-content: center;
		margin-top: 0.5rem;
	}
	.hero__chip {
		border: 1px solid var(--border-default);
		border-radius: var(--radius-full, 999px);
		background: var(--bg-card);
		color: var(--text-secondary);
		font: inherit;
		font-size: 0.82rem;
		padding: 0.4rem 0.9rem;
		cursor: pointer;
		transition: border-color 0.15s ease, color 0.15s ease, transform 0.15s ease;
	}
	.hero__chip:hover {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
		transform: translateY(-1px);
	}

	.eledit {
		position: fixed;
		right: 1.1rem;
		bottom: 1.1rem;
		z-index: 90;
		width: min(22rem, calc(100vw - 2rem));
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary) 32%, var(--border-default));
		border-radius: var(--radius-lg, 28px);
		background: var(--bg-card);
		padding: 0.85rem 0.95rem;
		box-shadow: var(--shadow-lg, 0 20px 48px -18px rgba(0, 0, 0, 0.4));
		animation: studio-rise 0.2s var(--ease-settle, ease);
	}
	.eledit__head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}
	.eledit__tag {
		font-family: var(--font-mono, monospace);
		font-weight: 600;
		font-size: 0.84rem;
		color: var(--accent-primary);
	}
	.eledit__loc {
		font-family: var(--font-mono, monospace);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		padding: 0.05rem 0.45rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 11rem;
	}
	.eledit__loc--pi {
		color: var(--accent-secondary, var(--accent-primary));
	}
	.eledit__close {
		margin-left: auto;
		border: 0;
		background: transparent;
		color: var(--text-muted);
		font: inherit;
		cursor: pointer;
		padding: 0.1rem 0.3rem;
	}
	.eledit__text {
		margin: 0;
		font-size: 0.84rem;
		line-height: 1.35;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}
	.eledit__selector {
		margin: 0;
		font-family: var(--font-mono, monospace);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.eledit__input {
		width: 100%;
		box-sizing: border-box;
		resize: vertical;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 10px);
		background: var(--bg-surface);
		color: var(--text-primary);
		font: inherit;
		font-size: 0.86rem;
		padding: 0.5rem 0.6rem;
	}
	.eledit__input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 60%, transparent);
		outline-offset: 1px;
	}
	.eledit__actions {
		display: flex;
		align-items: center;
		gap: 0.6rem;
	}
	.eledit__hint {
		flex: 1;
		font-size: var(--text-2xs);
		color: var(--text-muted);
		line-height: 1.3;
	}
	.eledit .vbtn {
		min-height: 2rem;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-sm, 10px);
		background: var(--bg-card);
		color: var(--text-primary);
		font: inherit;
		font-size: 0.78rem;
		font-weight: 600;
		padding: 0.35rem 0.7rem;
		cursor: pointer;
		flex-shrink: 0;
	}
	.eledit .vbtn--primary {
		border-color: color-mix(in srgb, var(--accent-primary) 72%, transparent);
		background: var(--accent-primary);
		color: var(--button-primary-color, #fff);
	}
	.eledit .vbtn:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	@keyframes studio-rise {
		from {
			opacity: 0;
			transform: translateY(8px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	@media (max-width: 1280px) {
		.studio {
			grid-template-columns: minmax(13rem, 17rem) minmax(0, 1fr);
			grid-template-rows: minmax(28rem, 3fr) minmax(22rem, 2fr);
			min-height: 50rem;
			flex-shrink: 0;
			overflow: visible;
		}
		.studio--rail-collapsed {
			grid-template-columns: 3.2rem minmax(0, 1fr);
		}
		.studio :global(.rail) {
			grid-column: 1;
			grid-row: 1 / 3;
			position: sticky;
			top: 0;
			align-self: start;
			height: calc(100vh - var(--app-header-height, 48px));
		}
		.conversation {
			grid-column: 2;
			grid-row: 1;
			border-left: 0;
			border-right: 0;
			border-bottom: 1px solid var(--border-soft);
		}
		.studio :global(.stage) {
			grid-column: 2;
			grid-row: 2;
			border-left: 0;
		}
		.hero {
			min-height: 0;
			overflow-y: auto;
		}
	}
	@media (max-width: 800px) {
		.studio {
			grid-template-columns: 12rem minmax(0, 1fr);
		}
		.studio--rail-collapsed {
			grid-template-columns: 3.2rem minmax(0, 1fr);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.hero__chip {
			transition: none;
		}
		.conversation__live-dot {
			animation: none;
		}
		.eledit {
			animation: none;
		}
	}
</style>
