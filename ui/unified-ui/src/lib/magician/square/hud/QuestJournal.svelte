<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import TasksWorkspace from '$lib/magician/tasks/TasksWorkspace.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { GameWorkspace } from '../ui';

	export let initialSelectedTaskId: string | null = null;
	export let docked = false;

	const dispatch = createEventDispatcher<{ close: void }>();
	$: fullTasksHref = initialSelectedTaskId
		? `/tasks?selected=${encodeURIComponent(initialSelectedTaskId)}`
		: '/tasks';
</script>

<GameWorkspace
	open
	title="Tasks"
	navigation="close"
	navigationLabel="Close Tasks"
	presentation={docked ? 'docked' : 'overlay'}
	dismissible={!docked}
	showContext={false}
	showNavigation={false}
	className="qj-workspace"
	on:back={() => dispatch('close')}
>
	<svelte:fragment slot="actions">
		<a class="qj__full-page" href={fullTasksHref}>
			<Icon name="arrow-up-right" size={15} />
			Open full Tasks page
		</a>
	</svelte:fragment>

	<TasksWorkspace navigationMode="local" {initialSelectedTaskId} />
</GameWorkspace>

<style>
	:global(.qj-workspace .game-ui-workspace__body) {
		overflow: hidden;
		padding: 0;
	}

	.qj__full-page {
		display: inline-flex;
		align-items: center;
		gap: var(--game-space-2, 0.5rem);
		min-height: var(--game-target-md, 2.5rem);
		padding: 0 var(--game-space-3, 0.75rem);
		border: 1px solid var(--game-border);
		border-radius: var(--game-radius-sm, 0.25rem);
		background: var(--game-material-raised);
		color: var(--game-text);
		font-size: var(--game-type-2, 0.8rem);
		font-weight: 700;
		text-decoration: none;
	}

	.qj__full-page:hover {
		border-color: var(--game-border-strong);
		background: var(--game-material-selected);
	}
</style>
