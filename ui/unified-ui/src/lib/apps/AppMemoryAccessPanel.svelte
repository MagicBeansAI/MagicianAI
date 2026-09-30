<script lang="ts">
	import { onDestroy } from 'svelte';
	import {
		fetchAppMemoryAccess,
		updateAppMemoryAccess,
		type AppMemoryAccess
	} from './appMemoryAccess';
	import type { AppMemoryReadSelection } from './installationReview';

	export let installationId: string;

	let open = false;
	let loading = false;
	let saving = false;
	let error = '';
	let message = '';
	let access: AppMemoryAccess | null = null;
	let interactive: AppMemoryReadSelection = { user_tiers: [], agents: [] };
	let background: AppMemoryReadSelection = { user_tiers: [], agents: [] };
	let controller: AbortController | null = null;

	$: sensitive = new Set(
		(access?.user_tier_catalog ?? [])
			.filter((tier) => tier.readability === 'sensitive')
			.map((tier) => tier.name)
	);
	$: changed =
		access !== null &&
		JSON.stringify([interactive, background]) !==
			JSON.stringify([
				access.effective?.interactive ?? { user_tiers: [], agents: [] },
				access.effective?.background ?? { user_tiers: [], agents: [] }
			]);

	function adopt(next: AppMemoryAccess): void {
		access = next;
		interactive = structuredClone(next.effective?.interactive ?? { user_tiers: [], agents: [] });
		background = structuredClone(next.effective?.background ?? { user_tiers: [], agents: [] });
	}

	async function load(): Promise<void> {
		controller?.abort();
		controller = new AbortController();
		loading = true;
		error = '';
		try {
			adopt(await fetchAppMemoryAccess(installationId, controller.signal));
		} catch (cause) {
			if (!controller.signal.aborted) {
				error = cause instanceof Error ? cause.message : 'Memory access could not be loaded.';
			}
		} finally {
			loading = false;
		}
	}

	function toggle(mode: 'interactive' | 'background', kind: 'user_tiers' | 'agents', name: string): void {
		const current = mode === 'interactive' ? interactive : background;
		const values = new Set(current[kind]);
		if (values.has(name)) values.delete(name);
		else values.add(name);
		const next = { ...current, [kind]: [...values].sort() };
		if (mode === 'interactive') interactive = next;
		else background = next;
		message = '';
	}

	function revokeAll(): void {
		interactive = { user_tiers: [], agents: [] };
		background = { user_tiers: [], agents: [] };
		message = '';
	}

	async function save(): Promise<void> {
		if (!access) return;
		saving = true;
		error = '';
		message = '';
		try {
			adopt(
				await updateAppMemoryAccess(
					installationId,
					access.edit_revision,
					interactive,
					background
				)
			);
			message = 'Saved. The app uses this on its next memory read.';
		} catch (cause) {
			error = `${cause instanceof Error ? cause.message : 'Memory access could not be saved.'} Reload to see the current access.`;
		} finally {
			saving = false;
		}
	}

	onDestroy(() => controller?.abort());
</script>

<details
	class="memory-access"
	bind:open
	on:toggle={() => {
		if (open && !access && !loading) void load();
	}}
>
	<summary>Memory access</summary>
	{#if loading}
		<p class="memory-access__copy">Loading…</p>
	{:else if error && !access}
		<p class="memory-access__error" role="alert">{error}</p>
		<button type="button" on:click={() => void load()}>Retry</button>
	{:else if access && !access.request}
		<p class="memory-access__copy">This app does not ask to read your memory.</p>
	{:else if access && access.request}
		<p class="memory-access__copy">{access.request.purpose}</p>
		{#if !access.installation_enabled}
			<p class="memory-access__copy">The app is not enabled, so it can read nothing right now.</p>
		{/if}
		<table>
			<thead>
				<tr><th scope="col">Memory</th><th scope="col">While you use it</th><th scope="col">In the background</th></tr>
			</thead>
			<tbody>
				{#each access.request.user_tiers as tier}
					<tr>
						<th scope="row">{tier.replaceAll('_', ' ')}{#if sensitive.has(tier)} <span class="memory-access__sensitive">sensitive</span>{/if}</th>
						<td><input type="checkbox" aria-label={`${tier} while you use it`} checked={interactive.user_tiers.includes(tier)} on:change={() => toggle('interactive', 'user_tiers', tier)} /></td>
						<td><input type="checkbox" aria-label={`${tier} in the background`} checked={background.user_tiers.includes(tier)} on:change={() => toggle('background', 'user_tiers', tier)} /></td>
					</tr>
				{/each}
				{#each access.request.agents as agent}
					<tr>
						<th scope="row">What {agent} has learned</th>
						<td><input type="checkbox" aria-label={`${agent} while you use it`} checked={interactive.agents.includes(agent)} on:change={() => toggle('interactive', 'agents', agent)} /></td>
						<td><input type="checkbox" aria-label={`${agent} in the background`} checked={background.agents.includes(agent)} on:change={() => toggle('background', 'agents', agent)} /></td>
					</tr>
				{/each}
			</tbody>
		</table>
		<p class="memory-access__copy">Never shared: memory from client engagements or meetings, and memory other apps added.</p>
		<div class="memory-access__actions">
			<button type="button" disabled={saving || !changed} on:click={() => void save()}>{saving ? 'Saving…' : 'Save'}</button>
			<button type="button" class="danger" disabled={saving} on:click={revokeAll}>Untick all</button>
		</div>
		{#if message}<p class="memory-access__copy" role="status">{message}</p>{/if}
		{#if error}<p class="memory-access__error" role="alert">{error}</p>{/if}
	{/if}
</details>

<style>
	.memory-access summary {
		cursor: pointer;
	}
	.memory-access table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.82rem;
		margin: 6px 0;
	}
	.memory-access th,
	.memory-access td {
		padding: 5px 6px;
		text-align: left;
		border-top: 1px solid var(--border-soft, rgba(127, 127, 127, 0.2));
	}
	.memory-access td {
		text-align: center;
		width: 9rem;
	}
	.memory-access__copy {
		font-size: 0.8rem;
		color: var(--text-secondary);
	}
	.memory-access__error {
		font-size: 0.8rem;
		color: var(--accent-danger, #dc2626);
	}
	.memory-access__sensitive {
		margin-left: 6px;
		padding: 1px 6px;
		border-radius: 999px;
		font-size: 0.7rem;
		background: color-mix(in srgb, var(--accent-warning, #d97706) 18%, transparent);
	}
	.memory-access__actions {
		display: flex;
		gap: 8px;
	}
</style>
