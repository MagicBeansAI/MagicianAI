<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { get } from 'svelte/store';
	import { scopeIdentityStore, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
	import GoalRow from './GoalRow.svelte';
	import { noteHref } from '$lib/notes/noteLinks';
	import GoalCreateForm from './GoalCreateForm.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { listPrograms } from './programs';
	import { buildGoalTree, cachedGoalWorkspaces, closedGoals, COMPACT_GOAL_READ_LIMITS, forgetGoalWorkspaces, GOAL_READ_LIMITS, goalKey, isClosedGoal, loadGoalWorkspaces, openGoalTree, orphanNote, rememberGoalWorkspaces, showsInClosedList, showsInOpenList, subGoalsNotRead, updateGoalStatus, type Goal, type GoalNode, type GoalReadLimits, type GoalTree, type GoalWorkspace } from './goals';

	/** Target dates, Mark done and Reopen go through `PATCH /notes/front-matter`. New folder and
	 * New goal write a complete note; other edits stay in chat or the note. `tree` holds only open
	 * goals; done and dropped goals are listed in `closed`. */
	type Shown = GoalWorkspace & { tree: GoalTree; closed: Goal[] };
	/** How much of each workspace's Goals/ folder to read. The compact landing card reads only
	 * what it shows (`COMPACT_GOAL_READ_LIMITS`) and reuses a read from the last minute. */
	export let limits: GoalReadLimits | undefined = undefined;
	export let compact = false;
	/** Show the Closed list (`/work?view=goals&goals=closed`) instead of the open goal trees. */
	export let closed = false;
	$: readLimits = limits ?? (compact ? COMPACT_GOAL_READ_LIMITS : GOAL_READ_LIMITS);
	let workspaces: Shown[] = [];
	let loading = true, error = '', notice = '';
	let mounted = false, generation = 0, currentScope = '';
	let controller = new AbortController();
	let collapsedPaths = new Set<string>();
	let createMode: 'goal' | 'folder' | null = null;
	let playbooks: Map<string, string> | null = null, playbooksError = '';

	$: scopeKey = JSON.stringify([$scopeIdentityStore.principal, $scopeIdentityStore.workspace, getCurrentScopeCredentialRevision()]);
	$: if (mounted && scopeKey !== currentScope) { currentScope = scopeKey; createMode = null; void reload(); }

	$: openGoals = workspaces.flatMap(space => space.goals.filter(goal => !isClosedGoal(goal)));
	$: closedCount = workspaces.reduce((sum, space) => sum + space.closed.length, 0);
	$: rootNodes = workspaces.flatMap(space => space.tree.roots);
	$: orphanNodes = workspaces.flatMap(space => space.tree.orphans);
	$: compactNodes = (rootNodes.length ? [...rootNodes, ...orphanNodes] : openGoals.map(goal => ({ goal, children: [] }))).slice(0, 3);
	/** The compact read stops early, so its goal count is a floor when anything was left out. */
	$: partial = workspaces.some(space => space.notShown > 0 || space.foldersNotRead > 0);
	$: openSpaces = workspaces.filter(showsInOpenList);
	$: closedSpaces = workspaces.filter(showsInClosedList);
	$: spaceOf = new Map(workspaces.flatMap(space => space.goals.map(goal => [goal.path, space] as const)));
	const subGoalCount = (node: GoalNode) => {
		const space = spaceOf.get(node.goal.path);
		return node.children.length + (space ? subGoalsNotRead(space, node.goal) : 0);
	};

	function goalsScopeKey(): string {
		const scope = get(scopeIdentityStore);
		return JSON.stringify([scope.principal, scope.workspace, getCurrentScopeCredentialRevision()]);
	}
	function current(g: number, scope: string): boolean {
		return mounted && g === generation && scope === goalsScopeKey();
	}

	function toggleCollapse(path: string) {
		const next = new Set(collapsedPaths);
		if (next.has(path)) {
			next.delete(path);
		} else {
			next.add(path);
		}
		collapsedPaths = next;
	}

	function expandAll() {
		collapsedPaths = new Set();
	}

	function collectCollapsible(nodes: GoalNode[], acc: Set<string>) {
		for (const node of nodes) {
			if (node.children && node.children.length > 0) {
				acc.add(node.goal.path);
				collectCollapsible(node.children, acc);
			}
		}
	}

	function collapseAll() {
		const acc = new Set<string>();
		for (const space of workspaces) {
			collectCollapsible(space.tree.roots, acc);
			collectCollapsible(space.tree.orphans, acc);
		}
		collapsedPaths = acc;
	}

	const shown = (result: GoalWorkspace[]): Shown[] => result.map(space => ({ ...space, tree: openGoalTree(buildGoalTree(space.goals)), closed: closedGoals(space.goals) }));

	async function checkPlaybooks(g: number, scope: string) {
		try {
			const result = await listPrograms(controller.signal);
			if (current(g, scope)) playbooks = new Map(result.map(item => [item.name, item.title]));
		} catch (cause) {
			if (current(g, scope)) playbooksError = cause instanceof Error ? cause.message : String(cause);
		}
	}

	/** `fresh` (Refresh, Retry, a refused edit) skips the compact card's one-minute reuse. */
	async function reload(fresh = false) {
		controller.abort(); controller = new AbortController();
		const g = ++generation, scope = goalsScopeKey(), using = readLimits;
		if (fresh) forgetGoalWorkspaces();
		const cached = compact && !fresh ? cachedGoalWorkspaces(scope, using) : null;
		if (cached) { workspaces = shown(cached); error = ''; loading = false; return; }
		workspaces = []; error = ''; loading = true; playbooks = null; playbooksError = '';
		try {
			const result = await loadGoalWorkspaces(controller.signal, using);
			if (current(g, scope)) {
				workspaces = shown(result);
				if (compact) rememberGoalWorkspaces(scope, using, result);
				else if (result.some(space => space.goals.some(goal => goal.program))) void checkPlaybooks(g, scope);
			}
		} catch (e) { if (current(g, scope)) error = e instanceof Error ? e.message : String(e); }
		finally { if (current(g, scope)) loading = false; }
	}
	function refresh() { notice = ''; void reload(true); }

	/** A date edit was refused: the note changed since this screen read it. */
	function goalChanged(goal: Goal) {
		notice = `“${goal.title}” changed since this screen loaded, so its date was not saved. The goals have been reloaded; set the date again if it is still needed.`;
		void reload(true);
	}

	/** Mark done moves the goal under Closed; Reopen brings it back as active. Both reload, since
	 * closing or reopening a goal can move the open goals under it. */
	async function setStatus(goal: Goal, status: 'done' | 'active') {
		notice = '';
		const result = await updateGoalStatus(goal, status);
		const done = status === 'done';
		if (result.ok) notice = done ? `“${goal.title}” is marked done. It is listed under Closed.` : `“${goal.title}” is open again.`;
		else if (result.reason === 'conflict') notice = `“${goal.title}” changed since this screen loaded, so it was not ${done ? 'marked done' : 'reopened'}. The goals have been reloaded; try again if it is still needed.`;
		else { notice = `Could not ${done ? 'mark' : 'reopen'} “${goal.title}”${done ? ' done' : ''}: ${result.message}`; return; }
		await reload(true);
	}
	const markDone = (goal: Goal) => setStatus(goal, 'done');
	const parentTitle = (space: Shown, goal: Goal) => goal.parent ? space.tree.byKey.get(goalKey(goal.parent))?.title ?? goal.parent : null;

	const plural = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;
	const notShownNote = (count: number) => `${plural(count, 'more note', 'more notes')} in Goals/ not shown: this screen reads the first ${readLimits.notes} notes of a workspace. Open the notes to see the rest.`;
	const foldersNotReadNote = (count: number) => `${plural(count, 'folder', 'folders')} in Goals/ not read: this screen reads folders up to ${plural(readLimits.folderDepth, 'level', 'levels')} deep, and at most ${readLimits.folders} of them.`;

	onMount(() => { mounted = true; });
	onDestroy(() => { mounted = false; ++generation; controller.abort(); });
</script>

<section aria-label="Goals" class="goals" class:compact>
	{#if compact}
		<header class="compact-header">
			<div class="compact-title-group">
				<span class="compact-icon" aria-hidden="true">🎯</span>
				<div>
					<h3 class="compact-title">Goals</h3>
					<span class="compact-subtitle">Key targets and outcomes</span>
				</div>
			</div>
			<div class="compact-header-actions">
				<button type="button" class="compact-refresh-btn" on:click={refresh} disabled={loading} aria-label="Refresh goals" title="Refresh goals">
					<Icon name="rotate-ccw" size={12} />
				</button>
				<a href="/work?view=goals" class="compact-view-all">All goals <span class="compact-counter">{openGoals.length}{partial ? '+' : ''}</span> →</a>
			</div>
		</header>
		{#if error}
			<div role="alert" class="compact-alert">{error} <button on:click={refresh} class="compact-retry-btn">Retry</button></div>
		{:else if loading}
			<div role="status" class="compact-loading">
				<span class="compact-spinner" aria-hidden="true"></span>
				<span>Loading goals…</span>
			</div>
		{:else if !workspaces.length}
			<div class="compact-empty">
				<p class="compact-empty-text muted">No goal workspaces yet.</p>
				<a href="/chat" class="compact-action-link">Ask Magican in chat to set one up →</a>
			</div>
		{:else if !openGoals.length && closedCount}
			<div class="compact-empty">
				<p class="compact-empty-text muted">No open goals: every goal read here is done or dropped.</p>
				<a href="/work?view=goals&goals=closed" class="compact-action-link">See closed goals →</a>
			</div>
		{:else if !openGoals.length}
			<div class="compact-empty">
				<p class="compact-empty-text muted">{workspaces.length === 1 ? `No goals in ${workspaces[0].name} yet.` : 'No goals in your goal workspaces yet.'}</p>
				<a href="/chat" class="compact-action-link">Ask Magican in chat to add one →</a>
			</div>
		{:else}
			<div class="compact-card-list">
				{#each compactNodes as node (node.goal.path)}
					<article class="compact-item-card compact-goal-card" aria-label={node.goal.title}>
						<div class="compact-goal-main">
							<a href={noteHref(node.goal.path, $scopeIdentityStore.workspace)} class="compact-item-title" title={`Open ${node.goal.title} note`}>
								{node.goal.title}
							</a>
							<div class="compact-item-tags">
								<span class="compact-item-badge status-{node.goal.status}">{node.goal.status}</span>
								{#if node.goal.owner}
									<span class="compact-meta-chip">{node.goal.owner === 'user' ? 'You' : node.goal.owner}</span>
								{/if}
								{#if subGoalCount(node) > 0}
									<span class="compact-meta-chip compact-meta-chip--subgoals">{subGoalCount(node)} sub-goal{subGoalCount(node) === 1 ? '' : 's'}</span>
								{/if}
								{#if node.goal.target}
									<span class="compact-meta-target">{node.goal.target}</span>
								{/if}
							</div>
						</div>
						{#if node.children && node.children.length > 0}
							<div class="compact-subgoals-box" aria-label={`Sub-goals of ${node.goal.title}`}>
								<div class="compact-subgoals-list">
									{#each node.children.slice(0, 2) as child (child.goal.path)}
										<div class="compact-subgoal-row">
											<span class="compact-subgoal-connector" aria-hidden="true">└─</span>
											<a href={noteHref(child.goal.path, $scopeIdentityStore.workspace)} class="compact-subgoal-link" title={`Open ${child.goal.title} note`}>
												<span class="compact-subgoal-name">{child.goal.title}</span>
											</a>
											<span class="compact-item-badge compact-subgoal-badge status-{child.goal.status}">{child.goal.status}</span>
										</div>
									{/each}
									{#if subGoalCount(node) > 2}
										<div class="compact-subgoal-row compact-subgoal-row--more">
											<span class="compact-subgoal-connector" aria-hidden="true">└─</span>
											<a href="/work?view=goals" class="compact-subgoal-more-link">
												+{subGoalCount(node) - 2} more sub-goal{subGoalCount(node) - 2 === 1 ? '' : 's'} →
											</a>
										</div>
									{/if}
								</div>
							</div>
						{/if}
					</article>
				{/each}
			</div>
		{/if}
	{:else}
		<header class="goals-masthead">
			<div class="goals-masthead__identity">
				<div class="goals-masthead__badge-row">
					<span class="masthead-badge">Goal workspaces</span>
					<span class="masthead-count">{openGoals.length} open goal{openGoals.length === 1 ? '' : 's'}</span>
				</div>
				<h2>Goals</h2>
				<p>Your open goals, the sub-goals under them and who owns each. Create a folder or goal here, set target dates and mark goals done; done and dropped goals are listed under Closed. Other changes stay in chat or in the goal's note.</p>
				<nav class="goals-switch" aria-label="Open or closed goals">
					<a href="/work?view=goals" aria-current={closed ? undefined : 'page'}>Open <span class="goals-switch__count">{openGoals.length}</span></a>
					<a href="/work?view=goals&goals=closed" aria-current={closed ? 'page' : undefined}>Closed <span class="goals-switch__count">{closedCount}</span></a>
				</nav>
			</div>
			<div class="goals-masthead__actions">
				<button type="button" class="action-btn" disabled={loading || !!error} on:click={() => createMode = 'goal'}>New goal</button>
				<button type="button" class="action-btn" disabled={loading || !!error} on:click={() => createMode = 'folder'}>New folder</button>
				<button type="button" class="action-btn" on:click={expandAll} title="Expand all goal trees">
					<Icon name="chevron-down" size={13} />
					<span>Expand all</span>
				</button>
				<button type="button" class="action-btn" on:click={collapseAll} title="Collapse all goal trees">
					<Icon name="chevron-up" size={13} />
					<span>Collapse all</span>
				</button>
				<button type="button" class="action-btn" on:click={refresh} disabled={loading} title="Reload goal notes">
					<Icon name="rotate-ccw" size={13} />
					<span>{loading ? 'Refreshing...' : 'Refresh'}</span>
				</button>
			</div>
		</header>
		{#if notice}<p class="notice" role="status">{notice}</p>{/if}
		{#if playbooksError}<p class="notice">Playbooks could not be checked. Goal notes are still shown. <button type="button" on:click={refresh}>Retry playbooks</button></p>{/if}
		{#if createMode}
			{#key `${currentScope}:${createMode}`}
				<GoalCreateForm mode={createMode} {workspaces} on:cancel={() => createMode = null} on:created={() => { createMode = null; void reload(); }} />
			{/key}
		{/if}
		{#if error}<div role="alert">{error} <button on:click={refresh}>Retry</button></div>
		{:else if loading}<p role="status">Loading goals…</p>
		{:else if !workspaces.length}<p class="empty">No goal workspaces yet. Create a folder and goal above, or <a href="/chat">Ask Magican in chat</a> to set one up from a template.</p>
		{:else if closed}
			{#if !closedSpaces.length}<p class="empty">No closed goals yet. A goal marked done or dropped is listed here.</p>{/if}
			{#each closedSpaces as space (space.path)}
				<section class="workspace" aria-label={`${space.name} closed goals`}>
					<h3>{space.name}</h3>
					{#if space.problems.length}<details class="problems"><summary>{space.problems.length === 1 ? 'One item could not be read' : `${space.problems.length} items could not be read`}</summary>
						<ul>{#each space.problems as problem}<li>{problem}</li>{/each}</ul>
					</details>{/if}
					{#if space.notShown}<p class="muted limit">{notShownNote(space.notShown)}</p>{/if}
					{#if space.foldersNotRead}<p class="muted limit">{foldersNotReadNote(space.foldersNotRead)}</p>{/if}
					{#if space.closed.length}
						<ul class="closed-list">
							{#each space.closed as goal (goal.path)}
								<li class="closed-goal" aria-label={goal.title}>
									<a class="closed-goal__title" href={noteHref(goal.path, $scopeIdentityStore.workspace)}>{goal.title}</a>
									<span class="compact-item-badge status-{goal.status}">{goal.status}</span>
									{#if parentTitle(space, goal)}<span class="muted">Under {parentTitle(space, goal)}</span>{/if}
									{#if goal.closed}<span class="muted">Closed {goal.closed}</span>{/if}
									<button type="button" class="closed-goal__reopen" on:click={() => setStatus(goal, 'active')} aria-label={`Reopen ${goal.title}`}>Reopen</button>
								</li>
							{/each}
						</ul>
					{:else}<p class="muted">No closed goals in this workspace.</p>{/if}
				</section>
			{/each}
		{:else}
			{#if !openSpaces.length}<p class="empty">No open goals: every goal is done or dropped. <a href="/work?view=goals&goals=closed">See closed goals →</a></p>{/if}
			{#each openSpaces as space (space.path)}
				<section class="workspace" aria-label={`${space.name} goals`}>
					<h3>{space.name}</h3>
					{#if space.problems.length}<details class="problems"><summary>{space.problems.length === 1 ? 'One item could not be read' : `${space.problems.length} items could not be read`}</summary>
						<ul>{#each space.problems as problem}<li>{problem}</li>{/each}</ul>
					</details>{/if}
					{#if space.notShown}<p class="muted limit">{notShownNote(space.notShown)}</p>{/if}
					{#if space.foldersNotRead}<p class="muted limit">{foldersNotReadNote(space.foldersNotRead)}</p>{/if}
					{#if space.tree.roots.length}
						<ul class="tree">
							{#each space.tree.roots as node (node.goal.path)}
								<GoalRow {node} goals={space.tree.byKey} {playbooks} {playbooksError} {collapsedPaths} onToggleCollapse={toggleCollapse} onStale={goalChanged} onMarkDone={markDone} />
							{/each}
						</ul>
					{:else if !space.tree.orphans.length}<p class="muted">{space.closed.length ? 'No open goals in this workspace.' : 'No goals in this workspace yet.'}</p>{/if}
					{#if space.tree.orphans.length}
						<section class="orphans" aria-label={`${space.name} goals not linked`}>
							<h4>Not linked</h4>
							<p class="muted">These goals, and the sub-goals under them, do not lead up to a main goal.</p>
							<ul class="tree">
								{#each space.tree.orphans as orphan (orphan.goal.path)}
									<GoalRow node={orphan} note={orphan.closedParent ? '' : orphanNote(orphan)} goals={space.tree.byKey} {playbooks} {playbooksError} {collapsedPaths} onToggleCollapse={toggleCollapse} onStale={goalChanged} onMarkDone={markDone} />
								{/each}
							</ul>
						</section>
					{/if}
				</section>
			{/each}
		{/if}
	{/if}
</section>

<style>
	.goals {
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		box-sizing: border-box;
		padding: var(--space-md) 1.45rem;
		color: var(--text-primary);
		min-width: 0;
	}
	.goals.compact {
		width: auto;
		max-width: none;
		margin: 0;
		padding: var(--space-sm) var(--space-md);
		background: linear-gradient(
			145deg,
			color-mix(in srgb, var(--accent-primary) 6%, var(--bg-card)) 0%,
			color-mix(in srgb, var(--bg-soft, var(--bg-card)) 30%, var(--bg-card)) 60%,
			var(--bg-card) 100%
		);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 20%, var(--border-soft));
		border-radius: var(--radius-lg, 12px);
		box-shadow: 0 4px 16px color-mix(in srgb, var(--accent-primary) 4%, rgba(0, 0, 0, 0.03));
		display: flex;
		flex-direction: column;
		gap: var(--space-xs);
		box-sizing: border-box;
		min-width: 0;
		transition: border-color 0.18s ease, box-shadow 0.18s ease;
	}
	.goals.compact:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
		box-shadow: 0 6px 20px color-mix(in srgb, var(--accent-primary) 8%, rgba(0, 0, 0, 0.05));
	}
	.compact-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-sm);
		padding-bottom: var(--space-2xs, 4px);
		border-bottom: 1px solid color-mix(in srgb, var(--accent-primary) 14%, var(--border-soft));
	}
	.compact-title-group {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		min-width: 0;
	}
	.compact-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 26px;
		height: 26px;
		border-radius: var(--radius-md, 6px);
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary) 18%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		font-size: 13px;
		flex-shrink: 0;
	}
	.compact-title {
		font-family: var(--font-display);
		font-size: var(--text-sm, 0.85rem);
		font-weight: 700;
		margin: 0;
		color: var(--text-primary);
		line-height: 1.2;
	}
	.compact-subtitle {
		font-size: var(--text-2xs, 0.72rem);
		color: var(--text-muted);
		line-height: 1.2;
		display: block;
	}
	.compact-header-actions {
		display: flex;
		align-items: center;
		gap: 6px;
		flex-shrink: 0;
	}
	.compact-refresh-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		padding: 0;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		color: var(--text-secondary);
		cursor: pointer;
	}
	.compact-refresh-btn:hover:not(:disabled) {
		color: var(--text-primary);
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
	}
	.compact-refresh-btn:disabled {
		opacity: 0.5;
		cursor: default;
	}
	.notice {
		margin: 0 0 var(--space-sm);
		padding: var(--space-xs) var(--space-sm);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-secondary);
		font-size: var(--text-sm);
	}
	.compact-view-all {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		font-size: var(--text-xs, 0.78rem);
		font-weight: 600;
		color: var(--text-secondary);
		text-decoration: none;
		padding: 4px 8px;
		border-radius: var(--radius-md, 8px);
		background: linear-gradient(
			180deg,
			var(--bg-card) 0%,
			color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft, var(--bg-base))) 100%
		);
		border: 1px solid var(--border-soft);
		white-space: nowrap;
		transition: all 0.15s ease;
		flex-shrink: 0;
	}
	.compact-view-all:hover {
		color: var(--text-primary);
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary) 14%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
	}
	.compact-counter {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 1px 5px;
		border-radius: var(--radius-sm, 4px);
		background: var(--bg-card);
		color: var(--text-primary);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
		border: 1px solid var(--border-soft);
		line-height: 1;
	}
	.compact-card-list {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 180px), 1fr));
		gap: 6px;
		align-items: stretch;
	}
	.compact-item-card {
		display: flex;
		flex-direction: column;
		justify-content: flex-start;
		gap: 6px;
		padding: 9px 12px;
		background: linear-gradient(
			180deg,
			var(--bg-card) 0%,
			color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft, var(--bg-base))) 100%
		);
		border: 1px solid color-mix(in srgb, var(--border-soft) 85%, transparent);
		border-radius: var(--radius-md, 8px);
		color: var(--text-primary);
		min-width: 0;
		min-height: 68px;
		overflow-wrap: anywhere;
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.02);
		transition: border-color 0.15s ease, box-shadow 0.15s ease, background 0.15s ease;
	}
	.compact-item-card:hover {
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--accent-primary) 5%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border-color: color-mix(in srgb, var(--accent-primary) 45%, var(--border-soft));
		box-shadow: 0 4px 14px color-mix(in srgb, var(--accent-primary) 8%, rgba(0, 0, 0, 0.05));
	}
	.compact-item-card:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: 2px;
	}
	.compact-goal-main {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.compact-item-title {
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		color: var(--text-primary);
		line-height: 1.3;
		margin: 0;
		text-decoration: none;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.compact-item-title:hover {
		color: var(--accent-primary);
		text-decoration: underline;
	}
	.compact-subgoals-box {
		margin-top: 2px;
		padding: 6px 8px;
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary) 7%, var(--bg-card)) 0%,
			color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft, var(--bg-base))) 100%
		);
		border-left: 2px solid var(--accent-primary);
		border-radius: 0 var(--radius-sm, 6px) var(--radius-sm, 6px) 0;
	}
	.compact-subgoals-list {
		display:flex;
		flex-direction:column;
		gap:3px;
	}
	.compact-subgoal-row {
		display:flex;
		align-items:center;
		gap:5px;
		min-width:0;
		font-size:var(--text-2xs, 0.72rem);
	}
	.compact-subgoal-connector {
		color:var(--text-muted);
		font-family:monospace;
		font-size:11px;
		line-height:1;
		flex-shrink:0;
	}
	.compact-subgoal-link {
		color:var(--text-secondary);
		text-decoration:none;
		min-width:0;
		flex:1;
		display:flex;
		overflow:hidden;
	}
	.compact-subgoal-link:hover {
		color:var(--text-primary);
		text-decoration:underline;
	}
	.compact-subgoal-name {
		white-space:nowrap;
		overflow:hidden;
		text-overflow:ellipsis;
		font-weight:500;
	}
	.compact-subgoal-badge {
		font-size:9px;
		padding:0 4px;
		line-height:1.4;
		flex-shrink:0;
		border-radius:var(--radius-sm, 4px);
	}
	.compact-subgoal-more-link {
		font-size:var(--text-2xs, 0.72rem);
		color:var(--accent-primary);
		text-decoration:none;
		font-weight:600;
	}
	.compact-subgoal-more-link:hover {
		text-decoration:underline;
	}
	.compact-meta-chip--subgoals {
		font-weight:600;
		color:var(--accent-primary);
	}
	.compact-item-tags {
		display:flex;
		align-items:center;
		flex-wrap:wrap;
		gap:4px;
		margin-top:auto;
		padding-top:2px;
	}
	.compact-item-badge {
		display:inline-flex;
		align-items:center;
		padding:1px 5px;
		border-radius:var(--radius-sm, 6px);
		font-size:var(--text-2xs, 0.72rem);
		font-weight:600;
		text-transform:capitalize;
	}
	.compact-item-badge.status-active {
		background:var(--color-success-soft, rgba(0,187,127,0.12));
		color:var(--color-success, #00bb7f);
		border:1px solid rgba(0,187,127,0.25);
	}
	.compact-item-badge.status-blocked {
		background:var(--color-error-soft, rgba(255,107,107,0.12));
		color:var(--color-error, #ff6b6b);
		border:1px solid rgba(255,107,107,0.25);
	}
	.compact-item-badge.status-done {
		background:var(--color-info-soft, rgba(77,157,224,0.12));
		color:var(--color-info, #4d9de0);
		border:1px solid rgba(77,157,224,0.25);
	}
	.compact-item-badge.status-proposed {
		background:var(--bg-card);
		color:var(--text-secondary);
		border:1px solid var(--border-soft);
	}
	.compact-item-badge.status-dropped {
		background:var(--bg-card);
		color:var(--text-muted);
		text-decoration:line-through;
		border:1px solid var(--border-soft);
	}
	.compact-meta-chip {
		font-size:var(--text-2xs, 0.72rem);
		color:var(--text-secondary);
		padding:1px 5px;
		border-radius:var(--radius-sm, 6px);
		background:var(--bg-card);
		border:1px solid var(--border-soft);
	}
	.compact-meta-target {
		font-size:var(--text-2xs, 0.72rem);
		color:var(--text-muted);
		padding:1px 3px;
	}
	.compact-empty {
		padding:var(--space-md) var(--space-sm);
		text-align:center;
		display:flex;
		flex-direction:column;
		align-items:center;
		justify-content:center;
		gap:6px;
		color:var(--text-secondary);
		font-size:var(--text-xs);
		border:1px dashed var(--border-soft);
		border-radius:var(--radius-md, 8px);
		background:var(--bg-surface, var(--bg-base));
	}
	.compact-empty p { margin:0; }
	.compact-action-link {
		color:var(--text-primary);
		font-weight:600;
		text-decoration:none;
	}
	.compact-action-link:hover {
		text-decoration:underline;
	}
	.compact-loading, .compact-alert {
		padding:var(--space-sm);
		font-size:var(--text-xs);
		border-radius:var(--radius-md, 8px);
		display:flex;
		align-items:center;
		gap:var(--space-sm);
	}
	.compact-loading {
		color:var(--text-muted);
	}
	.compact-alert {
		color:var(--color-error);
		background:var(--color-error-soft);
		border:1px solid rgba(255,107,107,0.2);
	}
	.compact-retry-btn {
		min-height:26px;
		padding:2px 8px;
		font-size:var(--text-2xs);
		border-radius:var(--radius-sm, 6px);
		cursor:pointer;
	}
	.goals-masthead {
		align-items: flex-start;
		background:
			radial-gradient(ellipse at top right, color-mix(in srgb, var(--accent-primary) 12%, transparent), transparent 60%),
			linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 96%, var(--bg-soft) 4%), var(--bg-card));
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		display: flex;
		flex-wrap: wrap;
		gap: 1.25rem;
		justify-content: space-between;
		padding: 1.25rem 1.5rem;
		margin-bottom: var(--space-md);
		box-sizing: border-box;
	}

	.goals-masthead__identity {
		max-width: min(48rem, 100%);
		min-width: 0;
	}

	.goals-masthead__badge-row {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem;
		margin-bottom: 0.4rem;
	}

	.masthead-badge {
		align-items: center;
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		border-radius: var(--radius-chip, 6px);
		color: var(--accent-primary);
		display: inline-flex;
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		padding: 0.2rem 0.55rem;
		text-transform: uppercase;
	}

	.masthead-count {
		color: var(--text-muted);
		font-size: 0.76rem;
		font-weight: 500;
	}

	.goals-masthead h2 {
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.85rem;
		font-weight: 700;
		letter-spacing: -0.02em;
		line-height: 1.15;
		margin: 0.15rem 0 0.4rem;
		color: var(--text-primary);
	}

	.goals-masthead p {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.45;
		margin: 0;
	}

	.goals-masthead__actions {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		align-self: flex-start;
	}

	.goals-masthead__actions .action-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		min-height: 1.85rem;
		padding: 0.32rem 0.75rem;
		font-size: 0.78rem;
		font-weight: 600;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-chip, 6px);
		color: var(--text-secondary);
		background: var(--bg-card);
		cursor: pointer;
		box-sizing: border-box;
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
		transition: all 0.15s ease;
	}
	.goals-masthead__actions .action-btn:hover:not(:disabled) {
		color: var(--text-primary);
		border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
		background: var(--bg-soft);
		box-shadow: 0 2px 6px color-mix(in srgb, var(--accent-primary) 8%, rgba(0, 0, 0, 0.04));
	}
	.goals-masthead__actions .action-btn:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}
	h2,h3,h4 { font-family:var(--font-display); margin:0 0 var(--space-sm); } p { line-height:1.5; }
	.muted { color:var(--text-secondary); font-size:var(--text-sm); }
	.workspace {
		margin-top: var(--space-lg);
		min-width: 0;
		padding: var(--space-md);
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--bg-soft, var(--bg-card)) 18%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 12%, var(--border-soft));
		border-radius: var(--radius-lg, 12px);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--accent-primary) 3%, rgba(0, 0, 0, 0.02));
	}
	.workspace h3 {
		font-family: var(--font-display);
		margin: 0 0 var(--space-sm);
		font-size: var(--text-base);
		font-weight: 700;
		color: var(--text-primary);
		padding-bottom: var(--space-xs);
		border-bottom: 1px solid color-mix(in srgb, var(--accent-primary) 14%, var(--border-soft));
	}
	.orphans {
		margin-top: var(--space-md);
		padding: var(--space-md);
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--accent-primary) 4%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border: 1px dashed color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		border-radius: var(--radius-lg, 12px);
	}
	.limit { margin:0 0 var(--space-sm); overflow-wrap:anywhere; }
	.tree { display:grid; gap:var(--space-sm); margin:0; padding:0; }
	.problems { margin-bottom:var(--space-sm); color:var(--text-secondary); font-size:var(--text-sm); overflow-wrap:anywhere; } summary { padding:var(--space-sm) 0; cursor:pointer; }
	a { color:var(--text-primary); }
	button { min-height:36px; padding:var(--space-xs) var(--space-sm); border:1px solid var(--border-soft); border-radius:var(--radius-md, 8px); color:var(--text-primary); background:var(--bg-card); font:inherit; cursor:pointer; } button:disabled { opacity:.6; cursor:default; }
	a:focus-visible,button:focus-visible,summary:focus-visible { outline:2px solid var(--accent-primary); outline-offset:3px; }
	.goals-switch { display:flex; gap:var(--space-xs); margin-top:var(--space-sm); flex-wrap:wrap; }
	.goals-switch a {
		display:inline-flex; align-items:center; gap:6px;
		padding:4px 12px; border:1px solid var(--border-soft); border-radius:999px;
		color:var(--text-secondary); text-decoration:none; font-size:var(--text-sm);
	}
	.goals-switch a[aria-current='page'] { border-color:var(--accent-primary); color:var(--text-primary); background:color-mix(in srgb, var(--accent-primary) 10%, transparent); }
	.goals-switch__count { font-variant-numeric:tabular-nums; color:var(--text-muted); }
	.closed-list { display:grid; gap:var(--space-xs); margin:0; padding:0; }
	.closed-goal {
		list-style:none; display:flex; align-items:center; flex-wrap:wrap; gap:var(--space-sm);
		padding:var(--space-xs) var(--space-sm); border:1px solid var(--border-soft); border-radius:var(--radius-md, 8px);
		background:var(--bg-card); font-size:var(--text-sm); min-width:0;
	}
	.closed-goal__title { font-weight:600; color:var(--text-primary); overflow-wrap:anywhere; }
	.closed-goal__reopen {
		margin-left:auto; padding:2px 10px; border:1px solid var(--border-soft); border-radius:var(--radius-sm, 6px);
		background:var(--bg-card); color:var(--text-secondary); font-size:var(--text-xs, 0.78rem); cursor:pointer;
	}
	.closed-goal__reopen:hover { border-color:var(--accent-primary); color:var(--text-primary); }
</style>
