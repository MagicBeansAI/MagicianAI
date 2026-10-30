<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { scopeIdentityStore, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
	import { readUsageTotals, readUsageModels, type UsageTotals, type UsageModelTotals } from '$lib/llm/usageTotals';
	import { anchorPopup } from '$lib/shared/anchorPopup';
	import { rootPortal } from '$lib/shared/rootPortal';
	import { clickOutside } from '$lib/shared/clickOutside';
	export let kind: 'chat' | 'task';
	export let id: string;
	export let live = false;
	let totals: UsageTotals | null = null;
	let error = '';
	let key = '';
	let generation = 0;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let destroyed = false;
	let open = false;
	let models: UsageModelTotals[] | null = null;
	let modelsError = '', modelsReading = -1;
	let mounted = false;
	let reading = -1;
	let trigger: HTMLElement;
	function close(): void { open = false; trigger?.focus(); }
	const count = (value: number) => Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 }).format(value);
	$: target = JSON.stringify([kind, id, $scopeIdentityStore.principal, $scopeIdentityStore.workspace, getCurrentScopeCredentialRevision()]);
	$: if (target !== key) { key = target; generation++; clearTimeout(timer); totals = null; models = null; modelsError = ''; error = ''; open = false; if (mounted) void refresh(generation); }
	$: if (open && models === null && !modelsError) void loadModels(generation);
	$: if (mounted && live) void refresh(generation);
	async function refresh(version: number): Promise<void> {
		if (!mounted || document.hidden || !id || destroyed || version !== generation || reading === version) return;
		reading = version;
		clearTimeout(timer);
		try {
			const result = await readUsageTotals(kind, id);
			if (!destroyed && version === generation) { totals = result; error = ''; }
		} catch {
			if (!destroyed && version === generation) error = 'Usage could not be refreshed';
		} finally {
			if (reading === version) reading = -1;
			if (!destroyed && version === generation && !document.hidden) timer = setTimeout(() => void refresh(version), live || error ? 15000 : 30000);
		}
	}
	async function loadModels(version: number): Promise<void> {
		if (!id || destroyed || version !== generation || modelsReading === version) return;
		modelsReading = version;
		try {
			const rows = await readUsageModels(kind, id);
			if (!destroyed && version === generation) { models = rows; modelsError = ''; }
		} catch { if (!destroyed && version === generation) modelsError = 'Model breakdown could not be loaded.'; }
		finally { if (modelsReading === version) modelsReading = -1; }
	}
	onMount(() => {
		mounted = true;
		const wake = () => {
			clearTimeout(timer);
			if (document.hidden) return;
			void refresh(generation);
			if (open) void loadModels(generation);
		};
		wake();
		document.addEventListener('visibilitychange', wake);
		window.addEventListener('focus', wake);
		window.addEventListener('online', wake);
		return () => {
			mounted = false;
			document.removeEventListener('visibilitychange', wake);
			window.removeEventListener('focus', wake);
			window.removeEventListener('online', wake);
		};
	});
	onDestroy(() => { destroyed = true; generation += 1; clearTimeout(timer); });
</script>

<details class="usage-totals" bind:open>
	<summary bind:this={trigger} aria-label={`Total ${kind} usage`} title={error || `Recorded cumulative ${kind} usage, including linked delegated work`}>
		{#if totals && totals.calls > 0}
			{#if kind === 'task'}<span>{totals.calls} calls</span>{/if}
			<span>{totals.input !== null || totals.output !== null ? `${count((totals.input ?? 0) + (totals.output ?? 0))} tokens` : 'Tokens unreported'}</span>
			<span>{totals.cacheRead !== null ? `${count(totals.cacheRead)} cached` : 'Cache unreported'}</span>
			<span>{totals.cost !== null ? `${totals.unpricedCalls ? '≥ ' : ''}$${totals.cost.toFixed(totals.cost < 0.01 ? 4 : 2)}` : 'Cost unreported'}</span>
		{:else}<span>{error ? 'Usage unavailable' : 'Usage'}</span>{/if}
	</summary>
	{#if open}
	<div class="usage-totals__detail" role="dialog" tabindex="-1" aria-label={`Total ${kind} usage details`} use:rootPortal
		use:anchorPopup={{ getAnchor: () => trigger, placement: 'bottom' }}
		use:clickOutside={{ handler: () => open = false, exclude: [trigger] }}
		on:keydown={(event) => { if (event.key === 'Escape') close(); }}>
		<strong>Total {kind} usage</strong>
		{#if totals}
			<div>{totals.calls} measured calls</div>
			<div>Input: {totals.input?.toLocaleString() ?? 'unreported'} · Output: {totals.output?.toLocaleString() ?? 'unreported'}</div>
			<div>Cache read: {totals.cacheRead?.toLocaleString() ?? 'unreported'} · Cache write: {totals.cacheWrite?.toLocaleString() ?? 'unreported'}</div>
			<div>Cost: {totals.cost === null ? 'unreported' : `$${totals.cost.toFixed(4)}`}{totals.unpricedCalls ? ` (${totals.unpricedCalls} calls unpriced)` : ''}</div>
		{:else}<p>{error || 'No measured usage recorded yet.'}</p>{/if}
		{#if error && totals}<p>{error}; showing the last recorded total.</p>{/if}
		<strong>By model</strong>
		{#if modelsError}<p>{modelsError}</p>
		{:else if models === null}<p>Loading breakdown…</p>
		{:else if !models.length}<p>No model usage recorded yet.</p>
		{:else}
			{#each models as row}
				<div class="usage-totals__model"><b>{row.model}</b> <span>{row.provider}</span>
					<div>{row.input !== null || row.output !== null ? `${count((row.input ?? 0) + (row.output ?? 0))} tokens` : 'Tokens unreported'} · {row.cacheRead === null ? 'Cache unreported' : `${count(row.cacheRead)} cached`} · {row.cost === null ? 'Cost unreported' : `${row.unpricedCalls ? '≥ ' : ''}$${row.cost.toFixed(4)}`}</div>
				</div>
			{/each}
		{/if}
		<button type="button" on:click={() => { void refresh(generation); void loadModels(generation); }}>Refresh</button>
	</div>
	{/if}
</details>

<style>
	.usage-totals { position: relative; font-size: 12px; color: var(--text-secondary); }
	summary { cursor: pointer; display: flex; flex-wrap: wrap; gap: .5rem; list-style: none; padding: .35rem; }
	.usage-totals__detail { position: fixed; z-index: 40; width: max-content; max-width: min(28rem, calc(100vw - 2rem)); box-sizing: border-box; overflow-y: auto; padding: 1rem; border: 1px solid var(--border-soft); border-radius: .75rem; background: var(--bg-base, var(--surface-base)); color: var(--text-secondary); font-size: 12px; box-shadow: 0 8px 30px #0003; line-height: 1.8; overflow-wrap: anywhere; }
	button { margin-top: .5rem; text-decoration: underline; }
	.usage-totals__model { border-top: 1px solid var(--border-soft); padding-top: .35rem; margin-top: .35rem; }
	.usage-totals__model > span { opacity: .7; }
</style>
