<script lang="ts">
	import GoalRow from './GoalRow.svelte';
	import { goalKey, goalProgramHref, goalProgramName, updateGoalTarget, type Goal, type GoalNode } from './goals';
	import { workAgentHref } from './routes';
	import { noteHref as canonicalNoteHref } from '$lib/notes/noteLinks';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

	/** Very deep chains stop at 8 levels; the rest are counted. */
	const MAX_DEPTH = 8;
	const VISIBILITY = {
		private: 'Shown only to you; never in an outward draft.',
		internal: 'Agents may use it; never stated outward.',
		public: 'May be stated outward'
	};
	export let node: GoalNode;
	/** Goals in the same workspace by `goalKey`, for peer links. */
	export let goals: Map<string, Goal>;
	/** null means the scoped catalog has not been checked; it never means a missing document. */
	export let playbooks: ReadonlyMap<string, string> | null = null;
	export let playbooksError = '';
	export let depth = 0;
	/** Why this goal heads a "Not linked" tree. */
	export let note = '';
	export let collapsedPaths: Set<string> = new Set();
	export let onToggleCollapse: (path: string) => void = () => {};

	$: goal = node.goal;
	$: programName = goal.program ? goalProgramName(goal.program) : '';
	$: id = `goal-${encodeURIComponent(goal.path)}`;
	$: peers = goal.peers.map(name => ({ name, goal: goals.get(goalKey(name)) ?? null }));
	$: noteHref = (path: string) => canonicalNoteHref(path, $scopeIdentityStore.workspace);

	$: hasChildren = !!node.children && node.children.length > 0;
	$: isCollapsed = hasChildren && collapsedPaths.has(goal.path);

	/** Called when a target-date edit was refused because the note changed since it was read;
	 * the screen reloads the goals and says why. */
	export let onStale: (goal: Goal) => void = () => {};
	/** Marks the goal done; the screen saves it and reloads, which moves it under Closed.
	 * Without it the row offers no Mark done. */
	export let onMarkDone: ((goal: Goal) => Promise<void>) | null = null;

	let markingDone = false;
	async function markDone() {
		if (!onMarkDone || markingDone) return;
		markingDone = true;
		try { await onMarkDone(goal); } finally { markingDone = false; }
	}

	let updatingTarget = false;
	let targetError = '';

	/** Saves through the notes front-matter API, which changes only `target:` and refuses an
	 * edit made against an older revision of the note. */
	async function saveTarget(next: string | null, input?: HTMLInputElement) {
		const previous = goal.target;
		if (next === previous) return;
		updatingTarget = true;
		targetError = '';
		goal.target = next;
		try {
			const result = await updateGoalTarget(goal, next);
			if (result.ok) {
				if (result.revision) goal.revision = result.revision;
				return;
			}
			goal.target = previous;
			if (input) input.value = previous ?? '';
			if (result.reason === 'conflict') onStale(goal);
			else targetError = result.reason === 'invalid' ? result.message : `${next ? 'Could not save date' : 'Could not clear date'}: ${result.message}`;
		} finally {
			updatingTarget = false;
		}
	}

	function handleTargetChange(e: Event) {
		const input = e.currentTarget as HTMLInputElement;
		void saveTarget(input.value?.trim() || null, input);
	}

	function openTargetPicker(event: MouseEvent | KeyboardEvent) {
		if (event instanceof KeyboardEvent && event.key !== 'Enter' && event.key !== ' ') return;
		const input = event.currentTarget as HTMLInputElement;
		if (input.disabled || typeof input.showPicker !== 'function') return;
		try {
			input.showPicker();
			if (event instanceof KeyboardEvent) event.preventDefault();
		} catch {
			// Keep native input activation available where showPicker is restricted.
		}
	}

	function handleClearTarget(e: MouseEvent) {
		e.stopPropagation();
		e.preventDefault();
		if (goal.target) void saveTarget(null);
	}
</script>

<li class="goal" aria-labelledby={id}>
	<div class="row">
		<div class="title">
			{#if hasChildren}
				<button
					type="button"
					class="tree-toggle-btn"
					aria-label={isCollapsed ? `Expand sub-goals of ${goal.title}` : `Collapse sub-goals of ${goal.title}`}
					aria-expanded={!isCollapsed}
					on:click={() => onToggleCollapse(goal.path)}
				>
					<span class="tree-toggle-icon" class:collapsed={isCollapsed} aria-hidden="true">▾</span>
				</button>
			{/if}
			<strong {id}>{goal.title}</strong>
			{#if goal.kind === 'peer'}<span class="chip">Peer goal</span>{/if}
			{#if hasChildren && isCollapsed}
				<button
					type="button"
					class="collapsed-count-badge"
					aria-label={`Expand ${node.children.length} sub-goal${node.children.length === 1 ? '' : 's'}`}
					on:click={() => onToggleCollapse(goal.path)}
					title="Click to expand sub-goals"
				>
					{node.children.length} sub-goal{node.children.length === 1 ? '' : 's'}
				</button>
			{/if}
		</div>
		{#if note}<div class="muted note">{note}</div>{/if}
		{#if node.closedParent}<div class="muted note">Its parent goal “{node.closedParent}” is closed.</div>{/if}
		<div class="meta">
			<span class="chip goal-status" data-status={goal.status}>{goal.status}</span>
			<span class="chip visibility" title={VISIBILITY[goal.visibility]}>{goal.visibility}</span>
			{#if goal.owner === 'user'}<span>Owner: you</span>
			{:else if goal.owner}<span>Owner: <a href={workAgentHref(goal.owner)}>{goal.owner}</a></span>{/if}

			{#if goal.target}
				<div class="deadline-wrapper">
					<label class="deadline-chip deadline-set" title="Click to edit target date">
						<span class="deadline-icon" aria-hidden="true">📅</span>
						<span class="deadline-text">Target {goal.target}</span>
						<input
							type="date"
							class="deadline-input"
							value={goal.target}
							on:click={openTargetPicker}
							on:keydown={openTargetPicker}
							on:change={handleTargetChange}
							disabled={updatingTarget}
							aria-label={`Target date for ${goal.title}`}
						/>
					</label>
					<button
						type="button"
						class="deadline-clear-btn"
						title="Remove target date"
						aria-label={`Remove target date for ${goal.title}`}
						on:click={handleClearTarget}
						disabled={updatingTarget}
					>×</button>
				</div>
			{:else}
				<div class="deadline-wrapper">
					<label class="deadline-chip deadline-unset" title="Click to set target date">
						<span class="deadline-icon" aria-hidden="true">📅</span>
						<span class="deadline-text">+ Set target date</span>
						<input
							type="date"
							class="deadline-input"
							value=""
							on:click={openTargetPicker}
							on:keydown={openTargetPicker}
							on:change={handleTargetChange}
							disabled={updatingTarget}
							aria-label={`Set target date for ${goal.title}`}
						/>
					</label>
				</div>
			{/if}
			{#if targetError}
				<span class="deadline-error" role="alert">{targetError}</span>
			{/if}
		</div>
		{#if peers.length}<div class="peers"><span class="muted">Peers</span>
			{#each peers as peer}{#if peer.goal}<a href={noteHref(peer.goal.path)}>{peer.goal.title}</a>{:else}<span>{peer.name}</span>{/if}{/each}
		</div>{/if}
		{#if goal.metrics.length}<div class="muted">Metrics: {goal.metrics.join(', ')}</div>{/if}
		<div class="links">
			{#if goal.program}
				<a href={goalProgramHref(goal.program)}>Playbook: {playbooks?.get(programName) ?? goal.program} →</a>
				{#if playbooks && !playbooks.has(programName)}<span class="deadline-error">Playbook missing</span>
				{:else if playbooksError}<span class="muted">Playbook could not be checked</span>{/if}
			{:else}<span class="muted">No playbook linked</span>{/if}
			<a href={noteHref(goal.path)}>Note →</a>
			{#if onMarkDone}
				<button type="button" class="mark-done-btn" on:click={markDone} disabled={markingDone} aria-label={`Mark ${goal.title} done`}>
					{markingDone ? 'Saving…' : 'Mark done'}
				</button>
			{/if}
		</div>
	</div>
	{#if hasChildren && !isCollapsed}
		{#if depth + 1 < MAX_DEPTH}
			<ul>
				{#each node.children as child (child.goal.path)}
					<GoalRow
						node={child}
						{goals}
						{playbooks}
						{playbooksError}
						depth={depth + 1}
						{collapsedPaths}
						{onToggleCollapse}
						{onStale}
						{onMarkDone}
					/>
				{/each}
			</ul>
		{:else}
			<p class="muted deeper">{node.children.length} deeper {node.children.length === 1 ? 'goal is' : 'goals are'} not shown here. Open the note to follow them.</p>
		{/if}
	{/if}
</li>

<style>
	.goal { list-style:none; min-width:0; }
	.row {
		display: grid;
		gap: var(--space-xs);
		padding: var(--space-sm) var(--space-md);
		background: linear-gradient(
			180deg,
			var(--bg-card) 0%,
			color-mix(in srgb, var(--bg-card) 94%, var(--bg-soft, var(--bg-base))) 100%
		);
		border: 1px solid color-mix(in srgb, var(--border-soft) 85%, transparent);
		border-radius: var(--radius-lg, 12px);
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.02);
		overflow-wrap: anywhere;
		transition: border-color 0.15s ease, box-shadow 0.15s ease;
	}
	.row:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
		box-shadow: 0 3px 10px color-mix(in srgb, var(--accent-primary) 6%, rgba(0, 0, 0, 0.04));
	}
	.title { display:flex; align-items:center; flex-wrap:wrap; gap:var(--space-sm); }
	.meta,.peers,.links { display:flex; align-items:center; flex-wrap:wrap; gap:var(--space-sm); font-size:var(--text-sm); color:var(--text-secondary); }
	.chip { display:inline-flex; align-items:center; padding:2px var(--space-sm); border:1px solid var(--border-soft); border-radius:var(--radius-md, 6px); font-size:var(--text-xs); color:var(--text-primary); }
	.goal-status[data-status="active"] { border-color:var(--accent-primary); }
	.goal-status[data-status="blocked"] { border-color:var(--status-failed); background:var(--status-failed-soft); }
	.goal-status[data-status="done"] { border-color:var(--status-completed); background:var(--status-completed-soft); }
	.goal-status[data-status="dropped"] { color:var(--text-secondary); text-decoration:line-through; }
	.visibility { color:var(--text-secondary); }
	.muted { color:var(--text-secondary); font-size:var(--text-sm); }
	a { color:var(--text-primary); } a:focus-visible { outline:2px solid var(--accent-primary); outline-offset:3px; }

	.tree-toggle-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 22px;
		height: 22px;
		min-height: 22px;
		padding: 0;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-surface, var(--bg-base));
		color: var(--text-secondary);
		cursor: pointer;
		line-height: 1;
		transition: all 0.15s ease;
		flex-shrink: 0;
	}
	.tree-toggle-btn:hover {
		color: var(--text-primary);
		border-color: var(--accent-primary);
		background: var(--accent-primary-soft, var(--bg-card));
	}
	.tree-toggle-icon {
		display: inline-block;
		transition: transform 0.15s ease;
		font-size: 11px;
	}
	.tree-toggle-icon.collapsed {
		transform: rotate(-90deg);
	}

	.collapsed-count-badge {
		display: inline-flex;
		align-items: center;
		padding: 1px 6px;
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 600;
		color: var(--accent-primary);
		background: var(--accent-primary-soft, rgba(0,0,0,0.04));
		border: 1px solid var(--accent-primary);
		border-radius: var(--radius-sm, 6px);
		cursor: pointer;
		min-height: 20px;
	}
	.collapsed-count-badge:hover {
		background: var(--accent-primary);
		color: var(--text-inverse, #fff);
	}

	.deadline-wrapper {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 3px;
	}
	.deadline-chip {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 5px;
		padding: 2px 8px;
		border-radius: var(--radius-md, 6px);
		font-size: var(--text-xs);
		line-height: 1.4;
		cursor: pointer;
		box-sizing: border-box;
		transition: all 0.15s ease;
	}
	.deadline-chip.deadline-set {
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
		color: var(--text-primary);
		font-weight: 500;
	}
	.deadline-chip.deadline-set:hover {
		border-color: var(--accent-primary);
		box-shadow: 0 1px 3px rgba(0,0,0,0.06);
	}
	.deadline-chip.deadline-unset {
		background: var(--bg-surface, var(--bg-base));
		border: 1px dashed var(--border-soft);
		color: var(--text-secondary);
	}
	.deadline-chip.deadline-unset:hover {
		border-color: var(--accent-primary);
		color: var(--text-primary);
		background: var(--accent-primary-soft, var(--bg-card));
	}
	.deadline-chip:focus-within { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	.deadline-icon {
		font-size: 11px;
		line-height: 1;
	}
	.deadline-text {
		line-height: 1.2;
	}
	.deadline-input {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		opacity: 0;
		cursor: pointer;
		margin: 0;
		padding: 0;
		border: none;
	}
	/* Browsers without showPicker still activate the native calendar across the chip. */
	.deadline-input::-webkit-calendar-picker-indicator {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		margin: 0;
		padding: 0;
		cursor: pointer;
	}
	.deadline-clear-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 18px;
		height: 18px;
		min-height: 18px;
		padding: 0;
		border: 1px solid transparent;
		border-radius: var(--radius-sm, 4px);
		background: transparent;
		color: var(--text-muted);
		font-size: 13px;
		line-height: 1;
		cursor: pointer;
		transition: all 0.12s ease;
	}
	.deadline-clear-btn:hover {
		color: var(--color-error, #ff6b6b);
		background: var(--color-error-soft, rgba(255,107,107,0.1));
		border-color: rgba(255,107,107,0.25);
	}
	.deadline-error {
		font-size: var(--text-2xs, 0.72rem);
		color: var(--color-error, #ff6b6b);
	}

	ul { display:grid; gap:var(--space-sm); margin:var(--space-sm) 0 0; padding:0 0 0 var(--space-lg); border-left:1px solid var(--border-soft); }
	.deeper { margin:var(--space-sm) 0 0 var(--space-lg); }
	@media(max-width:600px) { ul { padding-left:var(--space-sm); } .row { padding:var(--space-sm); } }
	.mark-done-btn {
		padding: 2px 8px;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-card);
		color: var(--text-secondary);
		font-size: var(--text-xs, 0.78rem);
		cursor: pointer;
	}
	.mark-done-btn:hover:not(:disabled) { border-color: var(--status-completed); color: var(--text-primary); }
	.mark-done-btn:disabled { opacity: 0.6; cursor: default; }
</style>
