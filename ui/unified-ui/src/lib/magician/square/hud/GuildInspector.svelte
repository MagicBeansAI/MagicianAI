<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { CitizenVM, GuildVM } from '../engine/types';
	import type { ProgramMissions } from '../fleetPrograms';
	import type { Quest } from '../fleetQuests';
	import { buildGuildWorkDrilldown } from '../fleetWorkDrilldown';
	import { VIBE_LABEL } from '../derive';
	import { GameInspector, GameObjectiveRow, GameSection, GameStat } from '../ui';
	import WorkDrilldown from './WorkDrilldown.svelte';

	export let guild: GuildVM;
	export let citizens: CitizenVM[] = [];
	export let missions: ProgramMissions | null = null;
	export let quests: Quest[] = [];
	export let revertArmed = false;
	export let reverting = false;

	const dispatch = createEventDispatcher<{ close: void; citizen: string; task: string; revert: void }>();

	$: members = guild.memberIds
		.map((id) => citizens.find((citizen) => citizen.id === id))
		.filter((citizen): citizen is CitizenVM => citizen != null);
	$: working = members.filter((citizen) => citizen.vibe === 'working').length;
	$: needs = members.filter((citizen) => citizen.vibe === 'needs').length;
	$: workDrilldown = buildGuildWorkDrilldown(guild.memberIds, quests);
</script>

<GameInspector
	open
	title={guild.name}
	eyebrow="Program"
	subtitle={`${members.length} assigned crew member${members.length === 1 ? '' : 's'}`}
	tone={needs > 0 ? 'attention' : working > 0 ? 'active' : 'neutral'}
	width="wide"
	on:back={() => dispatch('close')}
>
	<svelte:fragment slot="icon"><span class="gi__sigil">{guild.name.charAt(0).toUpperCase()}</span></svelte:fragment>

	<GameSection title="Program status" compact>
		<div class="gi__stats">
			<GameStat label="Assigned" value={members.length} detail={`${working} mobilized`} />
			<GameStat label="Active tasks" value={guild.activeQuestCount} tone={guild.activeQuestCount > 0 ? 'active' : 'neutral'} />
			<GameStat label="Needs you" value={guild.blockedQuestCount || needs} tone={guild.blockedQuestCount > 0 || needs > 0 ? 'attention' : 'neutral'} />
			<GameStat label="Delivered" value={guild.deliveredQuestCount} tone={guild.deliveredQuestCount > 0 ? 'success' : 'neutral'} />
		</div>
	</GameSection>

	<GameSection title="Program goals" description="Standing outcomes assigned to this program" compact>
		{#if missions?.missions.length}
			<div class="gi__missions">
				{#each missions.missions as mission, index (`${mission.title}:${index}`)}
					<GameObjectiveRow
						objective={mission.title}
						meta={mission.priority ? `${mission.priority} priority` : ''}
						state="Goal"
						tone={mission.priority === 'high' || mission.priority === 'critical' ? 'attention' : 'active'}
						interactive={false}
					/>
				{/each}
			</div>
		{:else}
			<p class="gi__empty">No structured goals are active for this program.</p>
		{/if}
	</GameSection>

	<WorkDrilldown
		model={workDrilldown}
		title="Assigned crew work"
		description="Current and latest task outcomes for this program's crew"
		emptyLabel="No assigned crew work is active"
		on:task={(event) => dispatch('task', event.detail)}
	/>

	<GameSection title="Assigned crew" compact>
		<div class="gi__roster">
			{#each members as member (member.id)}
				<button type="button" on:click={() => dispatch('citizen', member.id)}>
					<span class="gi__state" data-vibe={member.vibe}></span>
					<span><strong>{member.name}</strong><small>{member.title} · {VIBE_LABEL[member.vibe] ?? member.vibe}</small></span>
					<Icon name="chevron-right" size={14} />
				</button>
			{/each}
			{#if members.length === 0}<p class="gi__empty">This program has no assigned crew yet.</p>{/if}
		</div>
	</GameSection>

	<svelte:fragment slot="footer">
		<div class="gi__footer">
			<span>Program history is reversible.</span>
			<button
				type="button"
				class:gi__revert--armed={revertArmed}
				disabled={reverting}
				on:click={() => dispatch('revert')}
			>
				<Icon name="rotate-ccw" size={14} />
				{reverting ? 'Restoring' : revertArmed ? 'Confirm restore' : 'Restore prior goals'}
			</button>
		</div>
	</svelte:fragment>
</GameInspector>

<style>
	.gi__sigil { display: grid; width: 2rem; height: 2rem; place-items: center; border: 1px solid var(--game-border-strong); color: var(--game-state-active); font-weight: 800; }
	.gi__stats { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 0.4rem; }
	.gi__missions, .gi__roster { display: grid; gap: 0.4rem; }
	.gi__roster button { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: 0.55rem; min-width: 0; padding: 0.55rem 0; border: 0; border-bottom: 1px solid var(--game-border); background: transparent; color: var(--game-text); font: inherit; text-align: left; cursor: pointer; }
	.gi__roster button > span:nth-child(2) { display: grid; min-width: 0; gap: 0.12rem; }
	.gi__roster strong, .gi__roster small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.gi__roster small { color: var(--game-text-muted); font-size: var(--game-type-2); }
	.gi__state { width: 0.55rem; height: 0.55rem; border-radius: 50%; background: var(--game-state-neutral); }
	.gi__state[data-vibe='working'] { background: var(--game-state-success); }
	.gi__state[data-vibe='needs'] { background: var(--game-state-attention); }
	.gi__state[data-vibe='paused'] { background: var(--game-state-paused); }
	.gi__empty { margin: 0; color: var(--game-text-muted); line-height: 1.5; }
	.gi__footer { display: flex; width: 100%; align-items: center; gap: 0.75rem; }
	.gi__footer > span { margin-right: auto; color: var(--game-text-muted); font-size: var(--game-type-2); }
	.gi__footer button { display: inline-flex; align-items: center; gap: 0.35rem; min-height: 2.25rem; padding: 0.4rem 0.65rem; border: 1px solid var(--game-border); border-radius: var(--game-radius-sm); background: var(--game-material-muted); color: var(--game-text); font: inherit; font-weight: 700; cursor: pointer; }
	.gi__footer button.gi__revert--armed { border-color: var(--game-state-danger); color: var(--game-state-danger); }
</style>
