<script lang="ts">
	/**
	 * Permanently docked command panel beside the /square world pane.
	 * Fleet-wide by default; selecting a crew member swaps the header and tabs.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import type { AgentSummary } from '$lib/stores/agentStore';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import { agentTarget, parseTarget } from '../engine/types';
	import type { CitizenVM, GuildVM } from '../engine/types';
	import { chooseFleetObjective } from '../fleetObjectives';
	import {
		fetchFleetState,
		fleetStateQuests,
		type FleetSectionAvailability,
		type FleetStateAvailability,
		type FleetStateSnapshot
	} from '../fleetState';
	import { groupNeedsByAgent } from '../attentionGlue';
	import { buildOfficeCrew } from '../office/officeCrew';
	import { GameWorkspace } from '../ui';
	import AlertFeed from './AlertFeed.svelte';
	import AgentKit from './AgentKit.svelte';
	import CitizenDetail from './CitizenDetail.svelte';
	import CouncilSheet from './CouncilSheet.svelte';
	import CrewBoardSheet from './CrewBoardSheet.svelte';
	import ObjectiveTracker from './ObjectiveTracker.svelte';
	import QuestJournal from './QuestJournal.svelte';
	import TreasurySheet from './TreasurySheet.svelte';
	import '../game-chrome.css';

	type FleetTab = 'activity' | 'crew' | 'work' | 'delegation' | 'spend';
	type AgentTab = 'work' | 'activity' | 'command' | 'capabilities' | 'delegation';

	export let agents: AgentSummary[] = [];
	export let selectedTarget: string | null = null;
	/** Parent asks the dock to show a fleet-wide tab (HUD bar clicks). */
	export let openSection: FleetTab | null = null;
	export let citizens: CitizenVM[] = [];
	export let guilds: GuildVM[] = [];
	export let availability: FleetStateAvailability | null = null;

	const dispatch = createEventDispatcher<{
		select: { target: string | null };
		hide: void;
	}>();
	let crewDetailAgentId: string | null = null;
	let crewDetailModule: Promise<
		typeof import('$lib/magician/crew/CrewMemberOverviewOverlay.svelte')
	> | null = null;

	function openCrewDetail(agentId: string): void {
		crewDetailAgentId = agentId;
		crewDetailModule = import('$lib/magician/crew/CrewMemberOverviewOverlay.svelte');
	}

	function closeCrewDetail(): void {
		crewDetailAgentId = null;
		crewDetailModule = null;
	}

	const FLEET_TABS: Array<{ id: FleetTab; label: string; section: keyof FleetStateAvailability }> = [
		{ id: 'activity', label: 'Activity', section: 'attention' },
		{ id: 'crew', label: 'Crew', section: 'citizens' },
		{ id: 'work', label: 'Work', section: 'quests' },
		{ id: 'delegation', label: 'Delegation', section: 'handoffs' },
		{ id: 'spend', label: 'Spend', section: 'economy' }
	];
	const AGENT_TABS: Array<{ id: AgentTab; label: string }> = [
		{ id: 'work', label: 'Work' },
		{ id: 'activity', label: 'Activity' },
		{ id: 'command', label: 'Command' },
		{ id: 'capabilities', label: 'Capabilities' },
		{ id: 'delegation', label: 'Delegation' }
	];

	let fleetTab: FleetTab = 'crew';
	let agentTab: AgentTab = 'work';
	let fleetState: FleetStateSnapshot | null = null;
	let backendDown = false;
	let timers: Array<ReturnType<typeof setInterval>> = [];
	let unsubStatus: (() => void) | null = null;
	let tablistEl: HTMLElement | undefined;

	$: selectedParsed = parseTarget(selectedTarget ?? '');
	$: selectedId = selectedParsed?.kind === 'agent' ? selectedParsed.id : null;
	$: needsByAgent = groupNeedsByAgent($attentionStore);
	$: built = buildOfficeCrew(agents, fleetState, needsByAgent, { backendDown });
	$: roster = citizens.length > 0 ? citizens : built.citizens;
	$: programs = guilds.length > 0 ? guilds : built.guilds;
	$: selectedCitizen = selectedId ? roster.find((c) => c.id === selectedId) ?? null : null;
	$: fleetWide = selectedCitizen == null;
	$: headerName = selectedCitizen?.name ?? 'Fleet';
	$: headerSub = selectedCitizen
		? selectedCitizen.title
		: `${roster.length} crew`;
	$: sections = availability ?? fleetState?.availability ?? null;
	$: fleetObjective = chooseFleetObjective(fleetStateQuests(fleetState));
	$: visibleFleetTabs = FLEET_TABS;
	$: if (openSection && visibleFleetTabs.some((tab) => tab.id === openSection)) {
		fleetTab = openSection;
	}
	$: if (fleetWide && !visibleFleetTabs.some((tab) => tab.id === fleetTab)) fleetTab = 'crew';
	$: if (!fleetWide && !AGENT_TABS.some((tab) => tab.id === agentTab)) agentTab = 'work';

	function sectionStatus(key: keyof FleetStateAvailability): FleetSectionAvailability | null {
		return sections?.[key] ?? null;
	}

	function isUnavailable(key: keyof FleetStateAvailability): boolean {
		return sectionStatus(key)?.status === 'unavailable';
	}

	function unavailableCopy(key: keyof FleetStateAvailability): string {
		const section = sectionStatus(key);
		const why = section?.limitations[0];
		return why
			? `This view is unavailable — ${why}`
			: 'This view is unavailable. The snapshot did not serve it.';
	}

	async function refresh(): Promise<void> {
		const next = await fetchFleetState();
		if (next) fleetState = next;
	}

	function onTabKey(event: KeyboardEvent, ids: string[], current: string, set: (id: string) => void): void {
		if (event.key !== 'ArrowRight' && event.key !== 'ArrowLeft' && event.key !== 'Home' && event.key !== 'End') {
			return;
		}
		event.preventDefault();
		const i = ids.indexOf(current);
		let next = i;
		if (event.key === 'ArrowRight') next = (i + 1) % ids.length;
		if (event.key === 'ArrowLeft') next = (i - 1 + ids.length) % ids.length;
		if (event.key === 'Home') next = 0;
		if (event.key === 'End') next = ids.length - 1;
		const id = ids[next];
		if (id) {
			set(id);
			const btn = tablistEl?.querySelector<HTMLButtonElement>(`[data-tab="${id}"]`);
			btn?.focus();
		}
	}

	onMount(() => {
		attentionStore.start();
		void refresh();
		timers = [setInterval(() => void refresh(), 30_000)];
		unsubStatus = v2Events.connectionStatus.subscribe((s) => {
			backendDown = s === 'disconnected';
		});
	});

	onDestroy(() => {
		for (const timer of timers) clearInterval(timer);
		unsubStatus?.();
		attentionStore.stop();
	});
</script>

<aside class="command-dock" data-game-skin="pixel" aria-label="Command dock">
	<header class="command-dock__header">
		<span class="command-dock__eyebrow">{fleetWide ? 'Command' : 'Crew member'}</span>
		<strong class="command-dock__name">{headerName}</strong>
		<span class="command-dock__sub">{headerSub}</span>
		<div class="command-dock__actions">
			{#if selectedCitizen}
				<button
					type="button"
					class="command-dock__clear"
					on:click={() => dispatch('select', { target: null })}
				>Fleet</button>
			{/if}
			<button
				type="button"
				class="command-dock__clear"
				on:click={() => dispatch('hide')}
				aria-label="Hide command dock"
			>Hide</button>
		</div>
	</header>

	{#if fleetWide}
		<div
			class="command-dock__tabs"
			role="tablist"
			tabindex="0"
			aria-label="Fleet command"
			bind:this={tablistEl}
			on:keydown={(event) =>
				onTabKey(
					event,
					visibleFleetTabs.map((tab) => tab.id),
					fleetTab,
					(id) => (fleetTab = id as FleetTab)
				)}
		>
			{#each visibleFleetTabs as tab (tab.id)}
				<button
					type="button"
					role="tab"
					data-tab={tab.id}
					id="dock-tab-{tab.id}"
					aria-selected={fleetTab === tab.id}
					aria-controls="dock-panel-{tab.id}"
					tabindex={fleetTab === tab.id ? 0 : -1}
					on:click={() => (fleetTab = tab.id)}
				>{tab.label}</button>
			{/each}
		</div>
		<div
			class="command-dock__panel"
			role="tabpanel"
			id="dock-panel-{fleetTab}"
			aria-labelledby="dock-tab-{fleetTab}"
		>
			{#if isUnavailable(visibleFleetTabs.find((tab) => tab.id === fleetTab)?.section ?? 'citizens')}
				<p class="command-dock__unavailable">
					{unavailableCopy(visibleFleetTabs.find((tab) => tab.id === fleetTab)?.section ?? 'citizens')}
				</p>
			{:else if fleetTab === 'activity'}
				<AlertFeed docked on:jump={(e) => dispatch('select', { target: agentTarget(e.detail) })} />
			{:else if fleetTab === 'crew'}
				<CrewBoardSheet
					docked
					citizens={roster}
					on:citizen={(e) => dispatch('select', { target: agentTarget(e.detail) })}
				/>
			{:else if fleetTab === 'work'}
				<ObjectiveTracker docked objective={fleetObjective} />
				<QuestJournal docked />
			{:else if fleetTab === 'delegation'}
				<CouncilSheet
					docked
					citizens={roster}
					on:citizen={(e) => dispatch('select', { target: agentTarget(e.detail) })}
				/>
			{:else if fleetTab === 'spend'}
				<TreasurySheet docked citizens={roster} />
			{/if}
		</div>
	{:else if selectedCitizen}
		<div
			class="command-dock__tabs"
			role="tablist"
			tabindex="0"
			aria-label="Crew member command"
			bind:this={tablistEl}
			on:keydown={(event) =>
				onTabKey(
					event,
					AGENT_TABS.map((tab) => tab.id),
					agentTab,
					(id) => (agentTab = id as AgentTab)
				)}
		>
			{#each AGENT_TABS as tab (tab.id)}
				<button
					type="button"
					role="tab"
					data-tab={tab.id}
					id="dock-agent-tab-{tab.id}"
					aria-selected={agentTab === tab.id}
					aria-controls="dock-agent-panel-{tab.id}"
					tabindex={agentTab === tab.id ? 0 : -1}
					on:click={() => (agentTab = tab.id)}
				>{tab.label}</button>
			{/each}
		</div>
		<div
			class="command-dock__panel"
			role="tabpanel"
			id="dock-agent-panel-{agentTab}"
			aria-labelledby="dock-agent-tab-{agentTab}"
		>
			{#if agentTab === 'capabilities' || agentTab === 'delegation'}
				<AgentKit
					kind={agentTab}
					citizen={selectedCitizen}
					{roster}
					on:select={(event) => dispatch('select', { target: agentTarget(event.detail) })}
				/>
			{:else}
				<CitizenDetail
					citizen={selectedCitizen}
					guildName={programs.find((g) => g.id === selectedCitizen.guildId)?.name}
					needsItems={needsByAgent.get(selectedCitizen.id) ?? []}
					quests={fleetStateQuests(fleetState)}
					section={agentTab}
					on:detail={() => openCrewDetail(selectedCitizen.id)}
				/>
			{/if}
		</div>
	{/if}

	{#if crewDetailAgentId && crewDetailModule}
		{#await crewDetailModule}
			<GameWorkspace
				open
				title="Crew overview"
				subtitle={crewDetailAgentId}
				navigation="close"
				showContext={false}
				showNavigation={false}
				presentation="overlay"
				on:back={closeCrewDetail}
			>
				<div role="status">Loading crew overview...</div>
			</GameWorkspace>
		{:then detailOverlay}
			<detailOverlay.default agentId={crewDetailAgentId} on:back={closeCrewDetail} />
		{:catch}
			<GameWorkspace
				open
				title="Crew overview unavailable"
				subtitle={crewDetailAgentId}
				navigation="close"
				showContext={false}
				showNavigation={false}
				presentation="overlay"
				on:back={closeCrewDetail}
			>
				<div role="alert">The crew overview could not be opened.</div>
			</GameWorkspace>
		{/await}
	{/if}
</aside>

<style>
	.command-dock {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-width: 0;
		background: var(--game-material-panel, var(--bg-elevated, #fff));
		color: var(--game-text, var(--text-primary, #1c1c1c));
	}
	.command-dock__header {
		display: grid;
		grid-template-columns: 1fr auto;
		gap: 0.1rem 0.5rem;
		padding: 0.7rem 0.8rem 0.55rem;
		border-bottom: 1px solid var(--game-border, rgba(0, 0, 0, 0.25));
	}
	.command-dock__eyebrow,
	.command-dock__sub {
		grid-column: 1;
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm, 0.6875rem);
		letter-spacing: var(--game-display-track-sm, 1px);
		text-transform: uppercase;
		color: var(--game-text-muted, var(--text-secondary, #667085));
	}
	.command-dock__name {
		grid-column: 1;
		font-family: var(--game-font-display);
		font-size: var(--game-display-md, 0.8rem);
		letter-spacing: var(--game-display-track-sm, 1px);
		text-transform: uppercase;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.command-dock__actions {
		grid-column: 2;
		grid-row: 1 / span 3;
		align-self: start;
		display: flex;
		gap: 0.3rem;
	}
	.command-dock__clear {
		border: 1px solid var(--game-border, rgba(0, 0, 0, 0.25));
		background: transparent;
		color: inherit;
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm, 0.6875rem);
		letter-spacing: var(--game-display-track-sm, 1px);
		text-transform: uppercase;
		padding: 0.2rem 0.4rem;
		cursor: pointer;
	}
	.command-dock__tabs {
		display: flex;
		gap: 0;
		border-bottom: 1px solid var(--game-border, rgba(0, 0, 0, 0.25));
		overflow-x: auto;
	}
	.command-dock__tabs button {
		flex: 1 0 auto;
		border: 0;
		border-bottom: 3px solid transparent;
		background: transparent;
		color: var(--game-text-muted, var(--text-secondary, #667085));
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm, 0.6875rem);
		letter-spacing: var(--game-display-track-sm, 1px);
		text-transform: uppercase;
		padding: 0.55rem 0.45rem 0.4rem;
		cursor: pointer;
	}
	.command-dock__tabs button[aria-selected='true'] {
		color: var(--game-text, inherit);
		border-bottom-color: var(--game-text, #1c1c1c);
	}
	.command-dock__panel {
		flex: 1 1 auto;
		min-height: 0;
		overflow: auto;
	}
	.command-dock__unavailable {
		margin: 0;
		padding: 1rem 0.85rem;
		color: var(--game-text-muted, var(--text-secondary, #667085));
		font-size: 0.84rem;
		line-height: 1.4;
	}
</style>
