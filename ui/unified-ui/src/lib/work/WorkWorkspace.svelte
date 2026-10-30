<script lang="ts">
	import { page } from '$app/stores';
	import WorkFrame from './WorkFrame.svelte';
	import TasksWorkspace from '$lib/magician/tasks/TasksWorkspace.svelte';
	import InternalTasksWorkspace from '$lib/internalTasks/InternalTasksWorkspace.svelte';
	import MonitorsWorkspace from '$lib/monitors/MonitorsWorkspace.svelte';
	import CrewMemberWorkspace from '$lib/magician/crew/CrewMemberWorkspace.svelte';
	import CrewWorkspace from '$lib/magician/crew/CrewWorkspace.svelte';
	import AttentionWorkspace from '$lib/attention/AttentionWorkspace.svelte';
	import RelentlessWorkspace from '$lib/relentless/RelentlessWorkspace.svelte';
	import OperationsWorkspace from './OperationsWorkspace.svelte';
	import ProgramsWorkspace from './ProgramsWorkspace.svelte';
	import WorkHelp from './WorkHelp.svelte';
	import { WORK_HELP } from './help';
	import GoalsWorkspace from './GoalsWorkspace.svelte';
	import { resolveWorkView, workViewHref, workKindHref, workReturnHref, workSelectionKey, workThreadHref, type WorkView } from './routes';
	import { resolveTasksRouteView } from '$lib/magician/tasks/taskRoutes';

	let creating = false;
	$: view = resolveWorkView($page.url.searchParams);
	$: kind = resolveTasksRouteView($page.url.searchParams);
	$: if (view !== 'all' || kind !== 'tasks') creating = false;
	$: selectionKey = workSelectionKey($page.url.searchParams, kind);
	$: returnHref = workReturnHref($page.url.searchParams.get('from'));
	$: threadContextId = view === 'all' && kind === 'tasks' ? $page.url.searchParams.get('thread')?.trim().toLowerCase() || null : null;
	$: createHref = threadContextId ? workThreadHref(threadContextId, new URLSearchParams({ compose: '1' })) : '/work?compose=1';
</script>

<WorkFrame {view} {creating} {createHref} compact={view === 'all'} scrollable={true} {returnHref}>
		<!-- Outside the keyed block: switching Tasks, Monitors and System work must not remount
		     (and re-read) the Goals and Playbooks cards. -->
		{#if view === 'all' && !creating && !threadContextId && !$page.url.searchParams.has('compose')}
			<section class="strategic-deck" aria-label="Goals and Playbooks">
				<GoalsWorkspace compact />
				<ProgramsWorkspace compact />
			</section>
		{/if}
		{#key selectionKey}
			{#if view === 'all'}
				{#if !creating}
				<nav class="kind-filter" aria-label="Work type">
					{#each ['tasks','monitors','internal'] as entry}
						<a href={workKindHref(entry as 'tasks'|'monitors'|'internal')} title={WORK_HELP[entry as 'tasks'|'monitors'|'internal'].description} aria-current={kind === entry ? 'page' : undefined}>
							{WORK_HELP[entry as 'tasks'|'monitors'|'internal'].title}
						</a>
					{/each}
					<WorkHelp topic={WORK_HELP[kind]} />
				</nav>
				{/if}
				<div class="existing-workspace">
					{#if kind === 'internal'}<InternalTasksWorkspace />{:else if kind === 'monitors'}<MonitorsWorkspace routeBase="/work" />{:else}<TasksWorkspace routeBase="/work" workContext {threadContextId} bind:composeExpanded={creating} />{/if}
				</div>
			{:else if view === 'needs-you'}
				<AttentionWorkspace />
			{:else if view === 'crew'}{#if $page.url.searchParams.get('agent')}<CrewMemberWorkspace workContext />{:else}<CrewWorkspace />{/if}
			{:else if view === 'goals'}<GoalsWorkspace closed={$page.url.searchParams.get('goals') === 'closed'} />
			{:else if view === 'programs'}<ProgramsWorkspace />
			{:else if view === 'quality'}<RelentlessWorkspace />
			{:else}<OperationsWorkspace />{/if}
		{/key}
</WorkFrame>

<style>
	.strategic-deck {
		display: grid;
		grid-template-columns: 1fr;
		gap: var(--space-sm);
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		box-sizing: border-box;
		margin: 0 auto;
		padding: var(--space-xs) 1.45rem var(--space-sm);
		flex-shrink: 0;
	}
	@media (min-width: 900px) {
		.strategic-deck {
			grid-template-columns: 1fr 1fr;
		}
	}
	.kind-filter {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		flex-shrink: 0;
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		box-sizing: border-box;
		margin: 0 auto;
		padding: 0 1.45rem var(--space-sm);
		overflow-x: auto;
	}
	.kind-filter a {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		min-height: 36px;
		padding: 6px 14px;
		font-size: var(--text-xs);
		font-weight: 600;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		color: var(--text-secondary);
		background: linear-gradient(
			180deg,
			var(--bg-card) 0%,
			color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft, var(--bg-base))) 100%
		);
		text-decoration: none;
		white-space: nowrap;
		box-sizing: border-box;
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.03);
		transition: all 0.15s ease;
	}
	.kind-filter a[aria-current] {
		color: var(--text-primary);
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary) 15%, var(--bg-card)) 0%,
			color-mix(in srgb, var(--accent-primary) 6%, var(--bg-card)) 100%
		);
		border-color: color-mix(in srgb, var(--accent-primary) 50%, var(--border-soft));
		box-shadow: 0 2px 6px color-mix(in srgb, var(--accent-primary) 12%, transparent);
	}
	.kind-filter a:hover:not([aria-current]) {
		color: var(--text-primary);
		border-color: var(--border-strong, var(--border-soft));
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--accent-primary) 4%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
	}
	.kind-filter a:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: 3px;
	}
	.existing-workspace {
		flex: 1 0 auto;
		min-width: 0;
		min-height: 0;
	}
	.existing-workspace :global(.tasks-page-layout) {
		height: auto;
		min-height: 480px;
		overflow: visible;
	}
	.existing-workspace :global(.tasks-page-main) {
		overflow-y: visible;
		height: auto;
	}
	@media (max-width: 600px) {
		.strategic-deck,
		.kind-filter {
			padding-left: var(--space-sm);
			padding-right: var(--space-sm);
		}
	}
</style>
