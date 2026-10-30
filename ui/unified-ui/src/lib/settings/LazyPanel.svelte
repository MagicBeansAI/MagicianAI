<script lang="ts">
	// One Settings panel, loaded when the page's scheduler reaches it. Until
	// then a quiet card-shaped placeholder holds its place.
	import type { PanelLoader, PanelScheduler } from './panelLoading';
	import SettingsSkeleton from './SettingsSkeleton.svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

	export let scheduler: PanelScheduler;
	export let category: string;
	export let load: PanelLoader;
	/** Props for the panel, passed through unchanged. */
	export let props: Record<string, unknown> = {};

	let component: any = null;
	let failed = false;

	scheduler.load(category, load).then(
		(module) => (component = module.default),
		() => (failed = true)
	);
</script>

{#if component}
	{#key JSON.stringify([$scopeIdentityStore.principal, $scopeIdentityStore.workspace, $scopeIdentityStore.isResolved])}
		<svelte:component this={component} {...props} />
	{/key}
{:else if failed}
	<div class="lazy-panel lazy-panel--failed" role="alert">This section didn't load. Reload Settings to try again.</div>
{:else}
	<div aria-busy="true" aria-label="Loading"><SettingsSkeleton variant="card" /></div>
{/if}

<style>
	.lazy-panel--failed {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-secondary);
		font-size: 0.8125rem;
		padding: 1rem;
	}
</style>
