<script lang="ts">
	import TaskPanelDrawer from '$lib/magician/tasks/TaskPanelDrawer.svelte';
	import type { TaskPanelModel } from '$lib/magician/tasks/UnifiedTaskPanel.svelte';

	/**
	 * Exists for the two things a bare `render` cannot supply: a listener for
	 * `close`, and content for the `actions` slot. The drawer's own markup —
	 * the dialog, the skeleton, the title — is observable from a direct render.
	 */
	export let task: TaskPanelModel | null = null;
	export let title: string | null = null;
	export let loadError: string | null = null;
	export let lastLoadedAt: number | null = null;
	export let now = 0;
	export let outputActions = false;
	export let closeOnEscape = true;
	/** Rendered into the `actions` slot, so a surface's own buttons are testable there. */
	export let actionLabel: string | null = null;

	let closes = 0;
</script>

<TaskPanelDrawer
	{task}
	{title}
	{loadError}
	{lastLoadedAt}
	{now}
	{outputActions}
	{closeOnEscape}
	on:close={() => (closes += 1)}
>
	<svelte:fragment slot="actions">
		{#if actionLabel}
			<button type="button">{actionLabel}</button>
		{/if}
	</svelte:fragment>
</TaskPanelDrawer>

<output data-testid="drawer-closes">{closes}</output>
