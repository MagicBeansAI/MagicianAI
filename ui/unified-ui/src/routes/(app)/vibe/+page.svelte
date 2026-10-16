<script lang="ts">
	import { PRODUCT_NAME } from '$lib/presentationIdentity';
	/**
	 * VibeDev route host. Renders the new "Living Studio" cockpit
	 * (`VibeStudio`) by default; falls back to the legacy monolith
	 * (`VibeLegacyStudio`) when the studio flag is off. Flip with
	 * `?studio=0` / `?studio=1` (persisted). See `vibeStudioStore.readStudioFlag`.
	 */
	import { page } from '$app/stores';
	import VibeStudio from '$lib/shell/vibe/VibeStudio.svelte';
	import { readStudioFlag } from '$lib/stores/vibeStudioStore';

	$: studio = readStudioFlag($page.url.searchParams);
</script>

<svelte:head>
	<title>VibeDev · {PRODUCT_NAME}</title>
</svelte:head>

{#if studio}
	<VibeStudio />
{:else}
	<!-- The 4k-line legacy monolith only loads when the studio flag is OFF —
	     the default studio=on path never pays its bundle/parse cost. -->
	{#await import('$lib/shell/vibe/VibeLegacyStudio.svelte')}
		<div class="legacy-pending" role="status">
			<span class="loading-spinner loading-spinner-sm" aria-hidden="true"></span>
			<span>Loading studio…</span>
		</div>
	{:then legacy}
		<legacy.default />
	{:catch}
		<div class="legacy-error">
			<p>Couldn't load the classic studio — reload to try again.</p>
			<a href={$page.url.pathname + $page.url.search} data-sveltekit-reload>Reload</a>
		</div>
	{/await}
{/if}

<style>
	.legacy-pending,
	.legacy-error {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 2rem;
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.legacy-error {
		flex-direction: column;
		align-items: flex-start;
		gap: 0.4rem;
	}

	.legacy-error a {
		color: var(--accent-primary);
	}
</style>
