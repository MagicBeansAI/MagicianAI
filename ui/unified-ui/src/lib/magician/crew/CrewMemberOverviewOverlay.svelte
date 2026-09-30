<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { getAgentSnapshot, loadAgent } from '$lib/stores/agentStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { GameWorkspace } from '$lib/magician/square/ui';
	import { displayNameOf } from '$lib/magician/square/derive';
	import CrewMemberOverview from './CrewMemberOverview.svelte';
	import {
		crewMemberOverviewFromSummary,
		type CrewMemberOverviewModel
	} from './overview';

	export let agentId: string;

	let overview: CrewMemberOverviewModel | null = null;
	let displayName = agentId;
	let loading = true;
	let error: string | null = null;
	let mounted = false;
	let loadKey = '';
	let requestId = 0;

	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: if (mounted && agentId) {
		const nextLoadKey = `${scopeKey}:${agentId}`;
		if (nextLoadKey !== loadKey) {
			loadKey = nextLoadKey;
			void loadOverview(agentId, scopeKey);
		}
	}

	async function loadOverview(requestedAgentId: string, requestedScope: string): Promise<void> {
		const currentRequestId = ++requestId;
		loading = true;
		error = null;
		overview = null;
		try {
			const summary = await loadAgent(requestedAgentId);
			if (currentRequestId !== requestId || requestedAgentId !== agentId || requestedScope !== scopeKey) return;
			const resolved = summary ?? getAgentSnapshot(requestedAgentId);
			if (!resolved) throw new Error(`Crew member "${requestedAgentId}" was not found`);
			displayName = displayNameOf(resolved);
			overview = crewMemberOverviewFromSummary(resolved);
		} catch (cause) {
			if (currentRequestId !== requestId || requestedAgentId !== agentId || requestedScope !== scopeKey) return;
			overview = null;
			error = cause instanceof Error ? cause.message : 'Failed to load crew overview';
		} finally {
			if (currentRequestId === requestId && requestedAgentId === agentId && requestedScope === scopeKey) {
				loading = false;
			}
		}
	}

	onMount(() => {
		mounted = true;
		return () => {
			requestId += 1;
		};
	});
</script>

<GameWorkspace
	open
	title={displayName}
	subtitle={agentId}
	navigation="close"
	navigationLabel="Close crew overview"
	showContext={false}
	showNavigation={false}
	presentation="overlay"
	className="crew-overview-overlay"
	on:back
>
	<svelte:fragment slot="actions">
		<a class="crew-overview-overlay__full-record" href={`/crew/${encodeURIComponent(agentId)}`}>
			<Icon name="arrow-up-right" size={15} />
			Open full crew record
		</a>
	</svelte:fragment>

	<div class="crew-overview-overlay__body" aria-live="polite">
		{#if loading}
			<p class="crew-overview-overlay__state" role="status">Loading crew overview...</p>
		{:else if error}
			<p class="crew-overview-overlay__state crew-overview-overlay__state--error" role="alert">{error}</p>
		{:else if overview}
			<CrewMemberOverview {overview} idNamespace="town-square-crew-overview" />
		{/if}
	</div>
</GameWorkspace>

<style>
	:global(.crew-overview-overlay .game-ui-workspace__body) {
		--overview-text: var(--game-text);
		--overview-muted: var(--game-text-muted);
		--overview-border: var(--game-border);
		--overview-surface: var(--game-material-raised);
		--overview-shadow: none;
	}

	.crew-overview-overlay__body {
		min-width: 0;
		padding: var(--game-space-4);
	}

	.crew-overview-overlay__state {
		display: grid;
		min-height: 12rem;
		place-items: center;
		margin: 0;
		color: var(--game-text-muted);
	}

	.crew-overview-overlay__state--error {
		color: var(--game-state-danger);
	}

	.crew-overview-overlay__full-record {
		display: inline-flex;
		align-items: center;
		gap: var(--game-space-2);
		min-height: var(--game-target-md);
		padding: 0 var(--game-space-3);
		border: 1px solid var(--game-border);
		border-radius: var(--game-radius-sm);
		background: var(--game-material-raised);
		color: var(--game-text);
		font-size: var(--game-type-2);
		font-weight: 700;
		text-decoration: none;
	}

	.crew-overview-overlay__full-record:hover {
		border-color: var(--game-border-strong);
		background: var(--game-material-selected);
	}
</style>
