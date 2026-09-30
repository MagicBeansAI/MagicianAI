<script lang="ts">
	import { page } from '$app/stores';
	import InternalTasksWorkspace from '$lib/internalTasks/InternalTasksWorkspace.svelte';
	import TasksWorkspace from '$lib/magician/tasks/TasksWorkspace.svelte';
	import MonitorsWorkspace from '$lib/monitors/MonitorsWorkspace.svelte';
	import {
		INTERNAL_TASKS_ROUTE,
		MONITORS_TASKS_ROUTE,
		resolveTasksRouteView,
		TASKS_ROUTE
	} from '$lib/magician/tasks/taskRoutes';

	$: taskView = resolveTasksRouteView($page.url.searchParams);
	$: pageTitle =
		taskView === 'internal' ? 'Internal Tasks' : taskView === 'monitors' ? 'Monitors' : 'Tasks';
</script>

<svelte:head>
	<title>{pageTitle} · Magican</title>
</svelte:head>

<div class="tasks-route-shell">
	<nav class="tasks-view-tabs" aria-label="Task type">
		<a
			class="tasks-view-tab"
			class:active={taskView === 'tasks'}
			href={TASKS_ROUTE}
			aria-current={taskView === 'tasks' ? 'page' : undefined}
		>Tasks</a>
		<a
			class="tasks-view-tab"
			class:active={taskView === 'monitors'}
			href={MONITORS_TASKS_ROUTE}
			aria-current={taskView === 'monitors' ? 'page' : undefined}
		>Monitors</a>
		<a
			class="tasks-view-tab"
			class:active={taskView === 'internal'}
			href={INTERNAL_TASKS_ROUTE}
			aria-current={taskView === 'internal' ? 'page' : undefined}
		>Internal</a>
	</nav>

	<div
		class="tasks-route-content"
		class:tasks-route-content--internal={taskView === 'internal' || taskView === 'monitors'}
	>
		{#if taskView === 'internal'}
			<InternalTasksWorkspace />
		{:else if taskView === 'monitors'}
			<MonitorsWorkspace />
		{:else}
			<TasksWorkspace navigationMode="route" />
		{/if}
	</div>
</div>

<style>
	.tasks-route-shell {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		height: 100%;
		overflow: hidden;
	}

	.tasks-view-tabs {
		display: flex;
		flex: 0 0 auto;
		gap: 0.35rem;
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 0.65rem 1.45rem 0.45rem;
		box-sizing: border-box;
		overflow-x: auto;
		scrollbar-width: thin;
	}

	.tasks-view-tab {
		display: inline-flex;
		align-items: center;
		min-height: 2rem;
		padding: 0 0.75rem;
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--bg-card, #fff) 88%, transparent);
		color: var(--text-secondary, #5f6668);
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		text-decoration: none;
		white-space: nowrap;
	}

	.tasks-view-tab:hover,
	.tasks-view-tab:focus-visible {
		border-color: color-mix(in srgb, var(--accent-primary, #ff6b6b) 42%, var(--border-soft));
		color: var(--text-primary, #2d3436);
	}

	.tasks-view-tab.active {
		border-color: color-mix(in srgb, var(--accent-primary, #ff6b6b) 48%, var(--border-soft));
		background: var(--accent-primary-soft, rgba(255, 107, 107, 0.12));
		color: var(--text-primary, #2d3436);
		box-shadow: inset 0 -2px 0 var(--accent-primary, #ff6b6b);
	}

	.tasks-route-content {
		flex: 1;
		min-width: 0;
		min-height: 0;
		overflow: hidden;
	}

	.tasks-route-content--internal {
		overflow-y: auto;
	}
</style>
