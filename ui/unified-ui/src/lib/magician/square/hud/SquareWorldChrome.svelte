<script lang="ts">
	/**
	 * Shared chrome on the world pane — Floor and Campus.
	 * Create task + Attention at the top-left; compact status centered;
	 * roster down the left edge.
	 */
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import { openAttentionCenter } from '$lib/attention';
	import { attentionBadgeCount } from '$lib/attention/attentionBadgeCount';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { AgentSummary } from '$lib/stores/agentStore';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import { fetchCrewHealthCached, healthByAgent } from '$lib/magician/crew/health';
	import { agentTarget } from '../engine/types';
	import { rosterOrder } from '../derive';
	import { groupNeedsByAgent } from '../attentionGlue';
	import { chooseFleetObjective } from '../fleetObjectives';
	import { fetchFleetState, fleetStateQuests, type FleetStateSnapshot } from '../fleetState';
	import { playGameCue } from '../gameAudio';
	import { buildOfficeCrew } from '../office/officeCrew';
	import { GameHudButton } from '../ui';
	import CompactCrewStatus from './CompactCrewStatus.svelte';
	import CreateTaskComposer from './CreateTaskComposer.svelte';
	import ObjectiveTracker from './ObjectiveTracker.svelte';
	import RosterStrip from './RosterStrip.svelte';

	export let agents: AgentSummary[] = [];
	export let selectedId: string | null = null;
	export let canFly = false;

	const dispatch = createEventDispatcher<{
		select: { target: string | null };
		fly: { citizenId: string };
		dock: { section: 'crew' | 'work' | 'spend' };
	}>();

	let composing = false;
	let summaryOpen = false;
	let fleetState: FleetStateSnapshot | null = null;
	let backendDown = false;
	let spendUsd: number | null = null;
	let timers: Array<ReturnType<typeof setInterval>> = [];
	let unsubStatus: (() => void) | null = null;

	$: needsByAgent = groupNeedsByAgent($attentionStore);
	$: crew = buildOfficeCrew(agents, fleetState, needsByAgent, { backendDown });
	$: attentionCount = $attentionBadgeCount;
	$: workingCount = crew.citizens.filter((citizen) => citizen.vibe === 'working').length;
	$: fleetObjective = chooseFleetObjective(fleetStateQuests(fleetState));

	function openDock(section: 'crew' | 'work' | 'spend'): void {
		playGameCue('command');
		dispatch('dock', { section });
	}

	async function refreshHealth(): Promise<void> {
		const next = healthByAgent(await fetchCrewHealthCached());
		if (!next || next.size === 0) {
			spendUsd = null;
			return;
		}
		spendUsd = [...next.values()].reduce((total, agent) => total + (agent.rolling_7d.spend_usd ?? 0), 0);
	}

	function openCreate(): void {
		playGameCue('command');
		composing = true;
	}

	function onRosterSelect(id: string): void {
		dispatch('select', { target: agentTarget(id) });
	}

	function onRosterFly(id: string): void {
		if (canFly) dispatch('fly', { citizenId: id });
		dispatch('select', { target: agentTarget(id) });
	}

	function onKey(event: KeyboardEvent): void {
		if (event.metaKey || event.ctrlKey || event.altKey) return;
		const tag = (event.target as HTMLElement | null)?.tagName;
		if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
		if (event.key >= '1' && event.key <= '9') {
			const frame = rosterOrder(crew.citizens)[Number(event.key) - 1];
			if (frame) dispatch('select', { target: agentTarget(frame.id) });
		}
	}

	onMount(() => {
		attentionStore.start();
		void fetchFleetState().then((next) => {
			if (next) fleetState = next;
		});
		void refreshHealth();
		timers = [
			setInterval(async () => {
				const next = await fetchFleetState();
				if (next) fleetState = next;
			}, 30_000),
			setInterval(() => void refreshHealth(), 120_000)
		];
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

<svelte:window on:keydown={onKey} />

<div class="world-chrome" data-game-skin="pixel">
	<nav class="world-chrome__actions" aria-label="World commands">
		<GameHudButton
			label="Create task"
			tooltip="Create a task and let the fleet assign it"
			tooltipPosition="right"
			tone="active"
			on:click={openCreate}
		>
			<Icon name="flag" size={17} />
		</GameHudButton>
		<GameHudButton
			label="Attention"
			tooltip="Open what needs you"
			tooltipPosition="right"
			tone={attentionCount > 0 ? 'attention' : 'neutral'}
			badge={attentionCount > 0 ? attentionCount : null}
			on:click={() => openAttentionCenter()}
		>
			<Icon name="alert" size={17} />
		</GameHudButton>
	</nav>

	<RosterStrip
		citizens={crew.citizens}
		{selectedId}
		on:select={(e) => onRosterSelect(e.detail)}
		on:fly={(e) => onRosterFly(e.detail)}
	/>

	<CompactCrewStatus
		working={workingCount}
		total={crew.citizens.length}
		{attentionCount}
		{backendDown}
		{spendUsd}
		{summaryOpen}
		on:summary={() => (summaryOpen = !summaryOpen)}
		on:attention={() => openAttentionCenter()}
		on:crew={() => openDock('crew')}
		on:usage={() => openDock('spend')}
	/>
	{#if summaryOpen}
		<ObjectiveTracker
			objective={fleetObjective}
			on:open={() => openDock('work')}
			on:close={() => (summaryOpen = false)}
		/>
	{/if}

	{#if composing}
		<CreateTaskComposer citizens={crew.citizens} on:close={() => (composing = false)} />
	{/if}
</div>

<style>
	.world-chrome {
		position: absolute;
		inset: 0;
		pointer-events: none;
		z-index: calc(var(--game-layer-command, 24) + 1);
	}
	.world-chrome__actions {
		pointer-events: auto;
		position: absolute;
		top: 0.75rem;
		left: 3.25rem;
		z-index: 7;
		display: flex;
		gap: 0.4rem;
	}
	.world-chrome :global(.rs) {
		top: 3.5rem;
	}
</style>
