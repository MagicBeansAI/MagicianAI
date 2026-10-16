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

<WorkFrame {view} {creating} {createHref} compact={view === 'all'} scrollable={view !== 'all' || kind !== 'tasks'} {returnHref}>
		{#key selectionKey}
			{#if view === 'all'}
				{#if !creating && !threadContextId && !$page.url.searchParams.has('compose')}
					<div class="ongoing"><ProgramsWorkspace compact /></div>
				{/if}
				{#if !creating}
				<nav class="kind-filter" aria-label="Work type">
					{#each [['tasks','Tasks'],['monitors','Monitors'],['internal','Internal work']] as entry}<a href={workKindHref(entry[0] as 'tasks'|'monitors'|'internal')} aria-current={kind === entry[0] ? 'page' : undefined}>{entry[1]}</a>{/each}
				</nav>
				{/if}
				<div class="existing-workspace">
					{#if kind === 'internal'}<InternalTasksWorkspace />{:else if kind === 'monitors'}<MonitorsWorkspace routeBase="/work" />{:else}<TasksWorkspace routeBase="/work" workContext {threadContextId} bind:composeExpanded={creating} />{/if}
				</div>
			{:else if view === 'needs-you'}
				<div class="review-entry"><a href="/work?view=quality">Quality reviews and agent proposals →</a></div>
				<AttentionWorkspace />
			{:else if view === 'crew'}{#if $page.url.searchParams.get('agent')}<CrewMemberWorkspace workContext />{:else}<CrewWorkspace />{/if}
			{:else if view === 'programs'}<ProgramsWorkspace />
			{:else if view === 'quality'}<RelentlessWorkspace />
			{:else}<OperationsWorkspace />{/if}
		{/key}
</WorkFrame>

<style>
 .ongoing { max-width:var(--app-content-max,1320px); width:100%; box-sizing:border-box; margin:0 auto; padding:var(--space-sm) var(--space-lg); }
 .kind-filter { display:flex; gap:var(--space-sm); flex-shrink:0; width:100%; max-width:var(--app-content-max,1320px); box-sizing:border-box; margin:0 auto; padding:0 var(--space-lg) var(--space-sm); overflow:auto; }
 .kind-filter a { display:inline-flex; align-items:center; justify-content:center; min-height:36px; padding:var(--space-sm); font-size:var(--text-xs); font-weight:600; border:1px solid var(--border-soft); border-radius:var(--radius-md); color:var(--text-secondary); text-decoration:none; white-space:nowrap; box-sizing:border-box; }
 .kind-filter a[aria-current] { color:var(--text-primary); background:var(--accent-primary-soft); }
 .kind-filter a:hover { color:var(--text-primary); background:var(--bg-card); }
 .kind-filter a:focus-visible { outline:2px solid var(--accent-primary); outline-offset:3px; }
 .existing-workspace { flex:1; min-width:0; min-height:0; }
 .review-entry { padding:var(--space-sm) var(--space-lg); }
 .review-entry a { color:var(--text-primary); }
 @media(max-width:600px) { .ongoing,.kind-filter { padding-left:var(--space-sm); padding-right:var(--space-sm); } }
</style>
