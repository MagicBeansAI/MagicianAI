<script lang="ts">
	/**
	 * Studio rail — the cockpit's left column. New build (+ project switcher,
	 * attach-repo / clone-from-GitHub), the active project, a RUNS status board
	 * (each run a card with a status badge + relative time), and CHECKPOINTS
	 * (one node per checks-passing applied change; each git-anchored node has a
	 * hover rewind that returns the project to that known-good state).
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import {
		createLiveRewirer,
		setupLiveDataSource
	} from '$lib/magician/dashboard/useLiveDataSource';
	import { stripRunTitlePrefix } from '$lib/shell/vibe/conversation/submit';
	import type { VibeDevProject } from '$lib/stores/vibeDevProjectStore';
	import type { Task } from '$lib/stores/taskStore';
	import type { VibeCheckpoint } from '$lib/stores/vibeCheckpointsStore';
	import {
		buildVibeDevProjectAttributions,
		buildVibeDevProjectRollupSql,
		buildVibeDevRunRollupSql,
		costByProject,
		costByRun,
		emptyProjectCost,
		emptyRunCost,
		formatCompactNumber,
		formatUsd,
		modelLabel,
		totalTokens,
		type VibeDevProjectCost,
		type VibeDevRunAttribution,
		type VibeDevRunCost
	} from '$lib/vibedev/llmCost';

	export let projects: VibeDevProject[] = [];
	export let activeProject: VibeDevProject | null = null;
	export let runs: Task[] = [];
	export let llmTasks: Task[] = [];
	export let runCostTaskIds: Record<string, string[]> = {};
	export let activeTaskId: string | null = null;
	/** The active run's major checkpoints (one per checks-passing applied change), newest first. */
	export let checkpoints: VibeCheckpoint[] = [];
	export let expanded = true;
	export let busy = false;

	const dispatch = createEventDispatcher<{
		newRun: void;
		selectRun: { taskId: string };
		selectCheckpoint: { taskId: string; checkpointId: string };
		deleteRun: { taskId: string };
		selectProject: { projectId: string };
		createProject: { name: string; repoPath: string };
		cloneRepo: { url: string; name: string };
		startStarter: void;
		openSettings: void;
		toggleRail: void;
	}>();

	let menuOpen = false;
	let createOpen = false;
	let cloneOpen = false;
	let nameDraft = '';
	let repoDraft = '';
	let cloneUrl = '';
	let costMounted = false;
	let costLoading = false;
	let costError: string | null = null;
	let costsByProject = new Map<string, VibeDevProjectCost>();
	let runCostLoading = false;
	let runCostError: string | null = null;
	let costsByRun = new Map<string, VibeDevRunCost>();
	let scheduledCompletionKey = '';
	let postCompletionRefreshTimers: Array<ReturnType<typeof setTimeout>> = [];

	const costRewirer = createLiveRewirer((source) =>
		setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				costLoading = true;
				costError = null;
			},
			onRows: ({ records }) => {
				costsByProject = costByProject(projects, records);
				costError = null;
			},
			onError: (error) => {
				costError = error.message;
			},
			onSettled: () => {
				costLoading = false;
			}
		})
	);
	const runCostRewirer = createLiveRewirer((source) =>
		setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				runCostLoading = true;
				runCostError = null;
			},
			onRows: ({ records }) => {
				costsByRun = costByRun(completedBuildRunIds, records);
				runCostError = null;
			},
			onError: (error) => {
				runCostError = error.message;
			},
			onSettled: () => {
				runCostLoading = false;
			}
		})
	);

	onMount(() => {
		costMounted = true;
	});
	onDestroy(() => {
		costRewirer.destroy();
		runCostRewirer.destroy();
		clearPostCompletionRefreshTimers();
	});

	$: projectCostAttributions = buildVibeDevProjectAttributions(projects, llmTasks);
	$: projectCostSql = buildVibeDevProjectRollupSql(projectCostAttributions, 0);
	$: completedBuildRunIds = runs.filter(isCompletedBuildRun).map((task) => task.id);
	$: runCostTasksById = new Map([...llmTasks, ...runs].map((task) => [task.id, task]));
	$: runCostAttributions = completedBuildRunIds.map<VibeDevRunAttribution>((runId) => ({
		runId,
		taskIds: runCostTaskIds[runId] ?? [runId]
	}));
	$: completedBuildRunKey = runCostAttributions
		.map((attribution) => {
			const taskKey = attribution.taskIds
				.map((taskId) => {
					const task = runCostTasksById.get(taskId);
					return `${taskId}:${task?.status ?? 'missing'}:${task?.updatedAt ?? ''}`;
				})
				.join(',');
			return `${attribution.runId}:${taskKey}`;
		})
		.join('\n');
	$: runCostSql = buildVibeDevRunRollupSql(runCostAttributions, 0);
	$: if (costMounted) {
		costRewirer.sync(projectCostSql ? { kind: 'llm_calls_sql', sql: projectCostSql } : null);
		runCostRewirer.sync(runCostSql ? { kind: 'llm_calls_sql', sql: runCostSql } : null);
		syncPostCompletionCostRefresh(completedBuildRunKey);
	}
	$: activeProjectCost = activeProject
		? costsByProject.get(activeProject.project_id) ?? emptyProjectCost(activeProject.project_id)
		: null;

	function projectCost(project: VibeDevProject): VibeDevProjectCost {
		return costsByProject.get(project.project_id) ?? emptyProjectCost(project.project_id);
	}

	function runCost(task: Task): VibeDevRunCost {
		return costsByRun.get(task.id) ?? emptyRunCost(task.id);
	}

	function runLlmTitle(cost: VibeDevRunCost): string {
		return [
			`Spend: ${formatUsd(cost.costUsd)}`,
			`Calls: ${cost.calls.toLocaleString()}`,
			`Input tokens: ${formatCompactNumber(cost.inputTokens)}`,
			`Output tokens: ${formatCompactNumber(cost.outputTokens)}`,
			`Total tokens: ${formatCompactNumber(totalTokens(cost))}`,
			`Top model: ${modelLabel(cost)}`
		].join('\n');
	}

	function clearPostCompletionRefreshTimers(): void {
		for (const timer of postCompletionRefreshTimers) clearTimeout(timer);
		postCompletionRefreshTimers = [];
	}

	function syncPostCompletionCostRefresh(completedRunKey: string): void {
		if (completedRunKey === scheduledCompletionKey) return;
		scheduledCompletionKey = completedRunKey;
		clearPostCompletionRefreshTimers();
		if (!completedRunKey) return;
		postCompletionRefreshTimers = [35_000, 75_000].map((delayMs) =>
			setTimeout(() => {
				void costRewirer.controller?.refresh();
				void runCostRewirer.controller?.refresh();
			}, delayMs)
		);
	}

	// §13.3 #20 — durable provenance: a reverse link to the meeting/chat this project was seeded
	// from. Meetings deep-link to their thread; chat has no per-session route, so it's a chip.
	$: projectSource = activeProject?.source_meeting_thread_id
		? { href: `/meetings/${activeProject.source_meeting_thread_id}`, label: 'from meeting' }
		: activeProject?.source_chat_session_id
			? { href: null, label: 'from chat' }
			: null;

	type RunStatus = 'building' | 'attention' | 'done' | 'failed' | 'idle';
	// A run still flagged running/synthesizing but with no task update for this long is
	// treated as stalled, not live — covers a stuck `synthesis_pending` and never-terminal
	// re-delegation loops the cockpit otherwise shows as a frozen "Building"/"Synthesizing".
	const RUN_STALE_MS = 10 * 60 * 1000;
	function isRunStale(task: Task): boolean {
		const ms = Date.parse(task.updatedAt);
		return Number.isFinite(ms) && Date.now() - ms > RUN_STALE_MS;
	}
	function runStatus(task: Task): { kind: RunStatus; label: string } {
		// A Discuss/plan run produces a plan, not code — say "Planning", not "Building".
		const planning = isPlanRun(task);
		const terminal = ['completed', 'failed', 'cancelled', 'canceled', 'skipped'].includes(
			task.status
		);
		// An orphaned `synthesis_pending` on an already-terminal task (e.g. a child whose
		// synthesis was orphaned by a failed parent) must NOT render "Synthesizing" forever —
		// fall through to the terminal status below. Stale-but-non-terminal → "Stalled".
		if (task.synthesisPending && !terminal) {
			return isRunStale(task)
				? { kind: 'idle', label: 'Stalled' }
				: { kind: 'building', label: 'Synthesizing' };
		}
		switch (task.status) {
			case 'pending':
			case 'planning':
			case 'running':
				// Running but no update for >10 min (and no live stream) → stalled, not building.
				if (isRunStale(task)) return { kind: 'idle', label: 'Stalled' };
				return { kind: 'building', label: planning ? 'Planning' : 'Building' };
			case 'completed':
				return { kind: 'done', label: planning ? 'Planned' : 'Done' };
			case 'failed':
				return { kind: 'failed', label: 'Failed' };
			case 'cancelled':
				return { kind: 'idle', label: 'Cancelled' };
			default:
				return { kind: 'idle', label: task.status };
		}
	}
	function relTime(iso: string): string {
		const ms = Date.parse(iso);
		if (!Number.isFinite(ms)) return '';
		const diff = Date.now() - ms;
		const m = Math.round(diff / 60_000);
		if (m < 1) return 'now';
		if (m < 60) return `${m}m`;
		const h = Math.round(m / 60);
		if (h < 48) return `${h}h`;
		return `${Math.round(h / 24)}d`;
	}

	// Phase 5 legibility: a Discuss/plan run carries the `plan` tag (canonical:
	// VIBEDEV_PLAN_TAG in conversation/submit.ts). Number plan runs chronologically
	// (Plan v1, v2, …) so the refine chain reads at a glance; build runs stay unmarked
	// (the default), so plan runs stand out instead of every row carrying a label.
	function isPlanRun(task: Task): boolean {
		return (task.tags ?? []).some((t) => t.name.toLowerCase() === 'plan');
	}
	function isCompletedBuildRun(task: Task): boolean {
		return task.status === 'completed' && !isPlanRun(task);
	}
	// The user's ORIGINAL request — the prompt lines after the "VibeDev … request:"
	// intro, up to the first context block. This is what they asked, NOT the
	// engineer's `run_coding_task` relay (which the run-header's prompt_preview
	// shows, e.g. "PLAN-ONLY / READ-ONLY REQUEST …"). Falls back to the title.
	function runPrompt(task: Task): string {
		const lines = (task.description ?? '').split('\n');
		const start = /^VibeDev .*(request|follow-up):/i.test((lines[0] ?? '').trim()) ? 1 : 0;
		const body: string[] = [];
		for (let i = start; i < lines.length; i++) {
			if (lines[i].trim() === '') break;
			body.push(lines[i]);
		}
		const prompt = body.join('\n').trim();
		return prompt || stripRunTitlePrefix(task.title);
	}
	$: planVersions = (() => {
		const map = new Map<string, number>();
		runs
			.filter(isPlanRun)
			.slice()
			.sort(
				(a, b) =>
					Date.parse(a.createdAt || a.updatedAt) - Date.parse(b.createdAt || b.updatedAt)
			)
			.forEach((task, i) => map.set(task.id, i + 1));
		return map;
	})();

	function submitCreate(): void {
		const name = nameDraft.trim();
		if (!name) return;
		dispatch('createProject', { name, repoPath: repoDraft.trim() });
		createOpen = false;
		nameDraft = '';
		repoDraft = '';
	}
	function submitClone(): void {
		const url = cloneUrl.trim();
		if (!url) return;
		const name = nameDraft.trim() || url.split('/').pop()?.replace(/\.git$/, '') || 'Cloned repo';
		dispatch('cloneRepo', { url, name });
		cloneOpen = false;
		cloneUrl = '';
		nameDraft = '';
	}
</script>

<nav class="rail" class:rail--collapsed={!expanded} aria-label="Studio rail">
	<div class="rail__top">
		<button type="button" class="rail__collapse" on:click={() => dispatch('toggleRail')} aria-label={expanded ? 'Collapse rail' : 'Expand rail'}>
			{expanded ? '⟨' : '⟩'}
		</button>
		{#if expanded}
			<button type="button" class="rail__new" disabled={busy} on:click={() => dispatch('newRun')}><Icon name="plus" size={13} /> New build</button>
		{/if}
	</div>

	{#if expanded}
		<!-- Project -->
		<div class="rail__project">
			<button type="button" class="rail__project-btn" on:click={() => (menuOpen = !menuOpen)} aria-expanded={menuOpen}>
				<span class="rail__project-name">{activeProject?.name ?? 'No project'}</span>
				<span class="rail__chev" aria-hidden="true"><Icon name="chevron-down" size={12} /></span>
			</button>
			<button type="button" class="rail__settings" title="Project settings" aria-label="Project settings" on:click={() => dispatch('openSettings')}><Icon name="settings" size={14} /></button>
		</div>
		{#if projectSource}
			{#if projectSource.href}
				<a class="rail__project-source" href={projectSource.href} title="Open the originating meeting">
					<span aria-hidden="true"><Icon name="git-branch" size={11} /></span> {projectSource.label}
				</a>
			{:else}
				<span class="rail__project-source" title="This build was started from a chat thread">
					<span aria-hidden="true"><Icon name="git-branch" size={11} /></span> {projectSource.label}
				</span>
			{/if}
		{/if}
		{#if activeProject && activeProjectCost}
			<div class="rail__cost" aria-label="Project lifetime LLM cost">
				<div class="rail__cost-head">
					<span>LLM lifetime</span>
					{#if costLoading}
						<span class="rail__cost-status">refreshing</span>
					{:else if costError}
						<span class="rail__cost-status rail__cost-status--error">error</span>
					{:else}
						<span class="rail__cost-status">{activeProjectCost.calls.toLocaleString()} calls</span>
					{/if}
				</div>
				<div class="rail__cost-main">
					<span class="rail__cost-spend">{formatUsd(activeProjectCost.costUsd)}</span>
					<span class="rail__cost-model" title={modelLabel(activeProjectCost)}>
						Top {modelLabel(activeProjectCost)}
					</span>
				</div>
				<div class="rail__cost-grid">
					<span>In {formatCompactNumber(activeProjectCost.inputTokens)}</span>
					<span>Out {formatCompactNumber(activeProjectCost.outputTokens)}</span>
					<span>Total {formatCompactNumber(totalTokens(activeProjectCost))}</span>
				</div>
			</div>
		{/if}
		{#if menuOpen}
			<div class="rail__menu">
				{#each projects as project (project.project_id)}
					{@const cost = projectCost(project)}
					<button
						type="button"
						class="rail__menu-item"
						class:active={project.project_id === activeProject?.project_id}
						on:click={() => { dispatch('selectProject', { projectId: project.project_id }); menuOpen = false; }}
					>
						<span class="rail__menu-name">{project.name}{project.archived ? ' (archived)' : ''}</span>
						{#if cost.calls > 0}
							<span class="rail__menu-cost">{formatUsd(cost.costUsd)}</span>
						{/if}
					</button>
				{/each}
				<div class="rail__menu-sep"></div>
				<button type="button" class="rail__menu-item" on:click={() => { createOpen = !createOpen; cloneOpen = false; menuOpen = false; }}><Icon name="plus" size={12} /> New project…</button>
				<button type="button" class="rail__menu-item" on:click={() => { cloneOpen = !cloneOpen; createOpen = false; menuOpen = false; }}>⤓ Clone from GitHub…</button>
				<button type="button" class="rail__menu-item" on:click={() => { dispatch('startStarter'); menuOpen = false; }}><Icon name="sparkle" size={12} /> SvelteKit starter</button>
			</div>
		{/if}

		{#if createOpen}
			<form class="rail__form" on:submit|preventDefault={submitCreate}>
				<input bind:value={nameDraft} placeholder="Project name" aria-label="Project name" />
				<input bind:value={repoDraft} placeholder="Repo folder (optional)" aria-label="Repo folder" />
				<div class="rail__form-actions">
					<button type="button" class="rail__mini" on:click={() => (createOpen = false)}>Cancel</button>
					<button type="submit" class="rail__mini rail__mini--primary">Create</button>
				</div>
			</form>
		{/if}
		{#if cloneOpen}
			<form class="rail__form" on:submit|preventDefault={submitClone}>
				<input bind:value={cloneUrl} placeholder="https://github.com/org/repo" aria-label="GitHub URL" />
				<input bind:value={nameDraft} placeholder="Project name (optional)" aria-label="Project name" />
				<div class="rail__form-actions">
					<button type="button" class="rail__mini" on:click={() => (cloneOpen = false)}>Cancel</button>
					<button type="submit" class="rail__mini rail__mini--primary">Clone</button>
				</div>
			</form>
		{/if}

		<!-- Runs -->
		<div class="rail__section">
			<div class="rail__section-head">Runs</div>
			{#if runs.length === 0}
				<p class="rail__hint">No runs yet.</p>
			{:else}
				<div class="rail__runs">
					{#each runs as task (task.id)}
						{@const info = runStatus(task)}
						<div class="run-row">
							<button
								type="button"
								class="run"
								class:active={task.id === activeTaskId}
								title={runPrompt(task)}
								on:click={() => dispatch('selectRun', { taskId: task.id })}
							>
								<span class="run__meta">
									<span class="run__badge run__badge--{info.kind}">{info.label}</span>
									{#if isPlanRun(task)}
										<span class="run__type" title="Plan run — read-only, produces a plan (not a code diff)"><Icon name="file-text" size={11} /> Plan v{planVersions.get(task.id) ?? 1}</span>
									{/if}
									<span class="run__time">{relTime(task.updatedAt || task.createdAt)}</span>
								</span>
								<span class="run__title">{stripRunTitlePrefix(task.title)}</span>
								{#if isCompletedBuildRun(task)}
									{@const llm = runCost(task)}
									{#if runCostError}
										<span class="run__llm run__llm--error" title={runCostError}>LLM unavailable</span>
									{:else if runCostLoading && llm.calls === 0}
										<span class="run__llm run__llm--muted">LLM loading</span>
									{:else}
										<span class="run__llm" title={runLlmTitle(llm)}>
											<span>LLM {formatUsd(llm.costUsd)}</span>
											<span>{llm.calls.toLocaleString()} calls</span>
											<span>{formatCompactNumber(totalTokens(llm))} tok</span>
										</span>
									{/if}
								{/if}
							</button>
							<button
								type="button"
								class="run__delete"
								title="Delete run"
								aria-label="Delete run"
								on:click|stopPropagation={() => dispatch('deleteRun', { taskId: task.id })}
							>
								<Icon name="x" size={13} />
							</button>
						</div>
					{/each}
				</div>
			{/if}
		</div>

		<!-- Checkpoints — one node per checks-passing applied change (a known-good, rewindable state). -->
		{#if checkpoints.length > 0}
			<div class="rail__section">
				<div class="rail__section-head">Checkpoints</div>
				<div class="rail__checkpoints">
					{#each checkpoints as cp (cp.id)}
						<div class="ckpt-row">
							<span
								class="ckpt"
								title={cp.applied_files?.length
									? `${cp.name} — ${cp.applied_files.length} file(s) changed`
									: cp.name}
							>
								<span class="ckpt__dot" aria-hidden="true"></span>
								<span class="ckpt__label">{cp.name}</span>
							</span>
							{#if cp.git_sha}
								<button
									type="button"
									class="ckpt__rewind"
									title="Rewind project to this checkpoint"
									aria-label="Rewind to this checkpoint"
									on:click|stopPropagation={() =>
										dispatch('selectCheckpoint', { taskId: cp.task_id, checkpointId: cp.id })}
								><Icon name="rotate-ccw" size={13} /></button>
							{/if}
						</div>
					{/each}
				</div>
			</div>
		{/if}
	{:else}
		<button type="button" class="rail__icon" title="New build" aria-label="New build" on:click={() => dispatch('newRun')}><Icon name="plus" size={16} /></button>
	{/if}
</nav>

<style>
	.rail {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		min-height: 0;
		overflow-y: auto;
		padding: 0.7rem 0.6rem;
		background: color-mix(in srgb, var(--vibe-page-surface) 35%, var(--vibe-surface));
		border-right: 1px solid var(--vibe-border);
	}
	.rail--collapsed {
		align-items: center;
	}

	.rail__top {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}
	.rail__collapse {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text-muted);
		font: inherit;
		width: 1.7rem;
		height: 1.7rem;
		cursor: pointer;
		flex-shrink: 0;
	}
	.rail__new {
		flex: 1;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.3rem;
		border: 0;
		border-radius: var(--radius-md, 18px);
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
		font: inherit;
		font-weight: 600;
		font-size: 0.84rem;
		padding: 0.5rem 0.7rem;
		cursor: pointer;
	}
	.rail__new:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}
	.rail__icon {
		display: grid;
		place-items: center;
		border: 0;
		border-radius: var(--radius-md, 18px);
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
		width: 2rem;
		height: 2rem;
		cursor: pointer;
	}

	.rail__project {
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}
	.rail__project-source {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		margin: 0.15rem 0 0 0.1rem;
		font-size: var(--text-2xs);
		color: var(--vibe-text-muted);
		text-decoration: none;
		width: fit-content;
	}
	.rail__project-source > span {
		display: inline-flex;
		align-items: center;
	}
	a.rail__project-source:hover {
		color: var(--vibe-accent);
		text-decoration: underline;
	}
	.rail__cost {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: color-mix(in srgb, var(--vibe-surface) 92%, var(--vibe-accent));
		padding: 0.5rem 0.55rem;
	}
	.rail__cost-head,
	.rail__cost-main,
	.rail__cost-grid {
		display: flex;
		align-items: center;
		min-width: 0;
	}
	.rail__cost-head {
		justify-content: space-between;
		gap: 0.4rem;
		color: var(--vibe-text-muted);
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}
	.rail__cost-status {
		font-weight: 600;
		letter-spacing: 0;
		text-transform: none;
	}
	.rail__cost-status--error {
		color: var(--vibe-error);
	}
	.rail__cost-main {
		justify-content: space-between;
		gap: 0.5rem;
	}
	.rail__cost-spend {
		color: var(--vibe-text);
		font-size: 1.05rem;
		font-weight: 750;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.rail__cost-model {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--vibe-text-muted);
		font-size: 0.72rem;
		text-align: right;
	}
	.rail__cost-grid {
		justify-content: space-between;
		gap: 0.35rem;
		color: var(--vibe-text-muted);
		font-size: 0.7rem;
		font-variant-numeric: tabular-nums;
	}
	.rail__cost-grid > span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.rail__project-btn {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.4rem;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-weight: 600;
		font-size: 0.82rem;
		padding: 0.4rem 0.55rem;
		cursor: pointer;
		min-width: 0;
	}
	.rail__project-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.rail__settings {
		display: grid;
		place-items: center;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text-muted);
		width: 1.9rem;
		height: 1.9rem;
		cursor: pointer;
		flex-shrink: 0;
	}
	.rail__chev {
		display: inline-flex;
		align-items: center;
		color: var(--vibe-text-muted);
		flex-shrink: 0;
	}

	.rail__menu,
	.rail__form {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		padding: 0.3rem;
	}
	.rail__menu-item {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		border: 0;
		border-radius: var(--radius-sm, 8px);
		background: transparent;
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.8rem;
		text-align: left;
		padding: 0.35rem 0.45rem;
		cursor: pointer;
	}
	.rail__menu-name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.rail__menu-cost {
		margin-left: auto;
		flex: 0 0 auto;
		color: var(--vibe-text-muted);
		font-size: 0.72rem;
		font-variant-numeric: tabular-nums;
	}
	.rail__menu-item:hover {
		background: color-mix(in srgb, var(--vibe-page-surface) 60%, transparent);
	}
	.rail__menu-item.active {
		color: var(--vibe-accent);
		font-weight: 600;
	}
	.rail__menu-sep {
		height: 1px;
		background: var(--vibe-border);
		margin: 0.15rem 0;
	}
	.rail__form input {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-sm, 8px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.8rem;
		padding: 0.35rem 0.45rem;
	}
	.rail__form-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.3rem;
	}
	.rail__mini {
		border: 1px solid var(--vibe-border-strong);
		border-radius: var(--radius-sm, 8px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.74rem;
		font-weight: 600;
		padding: 0.25rem 0.55rem;
		cursor: pointer;
	}
	.rail__mini--primary {
		border-color: transparent;
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
	}

	.rail__section {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
	}
	.rail__section-head {
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--vibe-text-muted);
		padding: 0 0.2rem;
	}
	.rail__hint {
		margin: 0;
		font-size: 0.76rem;
		color: var(--vibe-text-muted);
		padding: 0 0.2rem;
	}

	.rail__runs {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}
	.run-row {
		position: relative;
		display: flex;
	}
	.run-row .run {
		flex: 1;
		min-width: 0;
	}
	.run__delete {
		position: absolute;
		top: 50%;
		right: 0.3rem;
		transform: translateY(-50%);
		display: grid;
		place-items: center;
		width: 1.35rem;
		height: 1.35rem;
		border: none;
		border-radius: 6px;
		background: var(--vibe-surface);
		color: var(--vibe-text-muted);
		line-height: 1;
		cursor: pointer;
		opacity: 0;
		transition: opacity 0.12s ease;
	}
	.run-row:hover .run__delete,
	.run__delete:focus-visible {
		opacity: 1;
	}
	.run__delete:hover {
		color: var(--color-danger, #d9534f);
		background: color-mix(in srgb, var(--color-danger, #d9534f) 16%, var(--vibe-surface));
	}
	.run {
		/* Two lines: meta row (status + plan version + time) then the truncated title.
		   The plan chip used to be a 4th item in a 3-col grid, which wrapped the row. */
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		border: 1px solid transparent;
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		/* extra right padding leaves room for the hover-only delete button */
		padding: 0.4rem 1.9rem 0.4rem 0.45rem;
		cursor: pointer;
		text-align: left;
	}
	.run__meta {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		min-width: 0;
	}
	.run:hover {
		border-color: var(--vibe-border);
	}
	.run.active {
		border-color: color-mix(in srgb, var(--vibe-accent) 55%, transparent);
		background: color-mix(in srgb, var(--vibe-accent) 7%, var(--vibe-surface));
	}
	.run__badge {
		font-size: var(--text-2xs);
		font-weight: 700;
		border-radius: var(--radius-full, 999px);
		padding: 0.08rem 0.4rem;
		white-space: nowrap;
	}
	/* WCAG-AA (U6): the status pills are --text-2xs/700 (normal text → needs ≥4.5:1). Using the
	   full-saturation status hue as text over a 16% tint of the same hue failed badly on light
	   themes (e.g. light-theme warning #ffe66d on its tint ≈ 1.2:1). Fix: the pill text is the
	   status hue mixed 35% into `--vibe-text` (the theme's high-contrast text colour) — keeps the
	   hue identity but pulls it to a readable shade. Verified ≥4.5:1 across light + dark themes
	   (worst case ~4.7:1); tints unchanged. Token-only, no per-theme overrides. */
	.run__badge--building {
		background: var(--status-running-soft, color-mix(in srgb, var(--vibe-accent) 16%, transparent));
		color: color-mix(in srgb, var(--status-running, var(--vibe-accent)) 35%, var(--vibe-text));
	}
	.run__badge--done {
		background: color-mix(in srgb, var(--vibe-success) 16%, transparent);
		color: color-mix(in srgb, var(--vibe-success) 35%, var(--vibe-text));
	}
	.run__badge--failed {
		background: color-mix(in srgb, var(--vibe-error) 16%, transparent);
		color: color-mix(in srgb, var(--vibe-error) 35%, var(--vibe-text));
	}
	.run__badge--attention {
		background: color-mix(in srgb, var(--vibe-warning) 16%, transparent);
		color: color-mix(in srgb, var(--vibe-warning) 35%, var(--vibe-text));
	}
	.run__badge--idle {
		background: color-mix(in srgb, var(--vibe-text-muted) 14%, transparent);
		color: color-mix(in srgb, var(--vibe-text-muted) 35%, var(--vibe-text));
	}
	.run__type {
		/* amber, matching the `plan` task-tag colour (#f59e0b) */
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		font-size: var(--text-2xs);
		font-weight: 700;
		border-radius: var(--radius-full, 999px);
		padding: 0.08rem 0.4rem;
		white-space: nowrap;
		background: color-mix(in srgb, var(--vibe-warning, #f59e0b) 16%, transparent);
		color: color-mix(in srgb, var(--vibe-warning, #f59e0b) 35%, var(--vibe-text));
	}
	.run__title {
		font-size: var(--text-xs);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}
	.run__llm {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		min-width: 0;
		overflow: hidden;
		color: var(--vibe-text-muted);
		font-size: var(--text-2xs);
		line-height: 1.2;
	}
	.run__llm > span {
		white-space: nowrap;
	}
	.run__llm > span:not(:first-child) {
		padding-left: 0.35rem;
		border-left: 1px solid color-mix(in srgb, var(--vibe-border) 70%, transparent);
	}
	.run__llm--muted {
		color: color-mix(in srgb, var(--vibe-text-muted) 82%, transparent);
	}
	.run__llm--error {
		color: color-mix(in srgb, var(--vibe-error) 45%, var(--vibe-text));
	}
	.run__time {
		margin-left: auto; /* right end of the meta line */
		font-size: var(--text-2xs);
		color: var(--vibe-text-muted);
		font-family: var(--font-mono, monospace);
		white-space: nowrap;
	}

	.rail__checkpoints {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
		padding-left: 0.35rem;
		border-left: 1px solid var(--vibe-border);
		margin-left: 0.35rem;
	}
	.ckpt-row {
		display: flex;
		align-items: center;
		gap: 0.2rem;
	}
	.ckpt {
		flex: 1;
		display: flex;
		align-items: center;
		gap: 0.45rem;
		border: 0;
		background: transparent;
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.74rem;
		padding: 0.2rem 0.3rem;
		min-width: 0;
		text-align: left;
	}
	.ckpt-row:hover .ckpt {
		color: var(--vibe-text);
	}
	.ckpt__rewind {
		flex-shrink: 0;
		display: grid;
		place-items: center;
		border: 0;
		background: transparent;
		color: var(--vibe-text-muted);
		line-height: 1;
		padding: 0.15rem 0.3rem;
		border-radius: var(--radius-sm, 8px);
		cursor: pointer;
		opacity: 0;
	}
	.ckpt-row:hover .ckpt__rewind,
	.ckpt__rewind:focus-visible {
		opacity: 1;
	}
	.ckpt__rewind:hover {
		color: var(--vibe-accent);
		background: color-mix(in srgb, var(--vibe-accent) 14%, transparent);
	}
	.ckpt.active {
		color: var(--vibe-accent);
	}
	.ckpt__dot {
		width: 0.45rem;
		height: 0.45rem;
		border-radius: 999px;
		background: currentColor;
		flex-shrink: 0;
	}
	.ckpt__label {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* Touch has no hover — keep the hidden row affordances (delete / rewind)
	   discoverable at reduced opacity. Must come AFTER their base rules:
	   equal specificity, so source order decides. Hover/focus still lift to 1. */
	@media (pointer: coarse) {
		.run__delete,
		.ckpt__rewind {
			opacity: 0.5;
		}
	}
</style>
