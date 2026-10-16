<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import CrewLeaderboard from '$lib/magician/crew/CrewLeaderboard.svelte';
	import { crewLeaderboardRowsFromCitizens } from '$lib/magician/crew/leaderboard';
	import type { CitizenVM } from '../engine/types';
	import { GameWorkspace } from '../ui';

	export let citizens: CitizenVM[] = [];
	export let docked = false;

	const dispatch = createEventDispatcher<{ close: void; citizen: string }>();
	type CrewLane = 'command' | 'moving' | 'ready' | 'all';
	const lanes: { id: CrewLane; label: string }[] = [
		{ id: 'command', label: 'Needs you' },
		{ id: 'moving', label: 'Working' },
		{ id: 'ready', label: 'Available' },
		{ id: 'all', label: 'Whole crew' }
	];
	let lane: CrewLane = 'all';

	$: ranked = crewLeaderboardRowsFromCitizens(citizens);
	$: commandCrew = ranked.filter((citizen) => citizen.vibe === 'needs');
	$: movingCrew = ranked.filter((citizen) => citizen.vibe === 'working');
	$: readyCrew = ranked.filter(
		(citizen) => citizen.vibe !== 'needs' && citizen.vibe !== 'working'
	);
	$: counts = {
		command: commandCrew.length,
		moving: movingCrew.length,
		ready: readyCrew.length,
		all: ranked.length
	};
	$: visibleRows =
		lane === 'command'
			? commandCrew
			: lane === 'moving'
				? movingCrew
				: lane === 'ready'
					? readyCrew
					: ranked;
</script>

<GameWorkspace
	open
	title="Crew"
	subtitle="Who is working, who is available, and who needs you"
	presentation={docked ? 'docked' : 'overlay'}
	data-game-skin="pixel"
	dismissible={!docked}
	showNavigation={!docked && citizens.length > 0}
	navigationLabel="Return to Town Square"
	on:back={() => dispatch('close')}
>
	<svelte:fragment slot="context">
		<div class="cb__context">
			<span><Icon name="inbox" size={17} /> Crew status</span>
			<strong class:cb__attention={commandCrew.length > 0}>{commandCrew.length} need you</strong>
			<strong>{movingCrew.length} working</strong>
			<strong>{readyCrew.length} ready</strong>
		</div>
	</svelte:fragment>

	<svelte:fragment slot="navigation">
		<p class="cb__nav-label">Crew views</p>
		{#each lanes as item (item.id)}
			<button
				type="button"
				class="cb__nav-item"
				class:cb__nav-item--active={lane === item.id}
				aria-pressed={lane === item.id}
				on:click={() => (lane = item.id)}
			>
				<span>{item.label}</span><strong>{counts[item.id]}</strong>
			</button>
		{/each}
	</svelte:fragment>

	<CrewLeaderboard
		rows={visibleRows}
		presentation="overlay"
		showHeader={false}
		emptyMessage={citizens.length === 0 ? 'No crew are assigned to this Town Square.' : 'No crew members are in this view.'}
		on:select={(event) => dispatch('citizen', event.detail)}
	/>
</GameWorkspace>

<style>
	.cb__context {
		display: flex;
		align-items: center;
		gap: 1.25rem;
		height: 100%;
		padding: 0 1.5rem;
		color: var(--game-text-muted);
	}
	.cb__context span {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		margin-right: auto;
		color: var(--game-text);
	}
	.cb__context span,
	.cb__context strong,
	.cb__nav-label,
	.cb__nav-item {
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm);
		font-weight: 400;
		font-synthesis: none;
		letter-spacing: var(--game-display-track-sm);
		text-transform: uppercase;
	}
	/* Raw tone colours are fills, not text: --game-state-attention as a label
	 * on this bar measures under 4.5:1, so it takes the same pull toward body
	 * text that --game-tone-text applies everywhere else. */
	.cb__attention { color: color-mix(in srgb, var(--game-state-attention) 60%, var(--game-text)); }
	.cb__nav-label {
		margin: 0 0 0.55rem;
		color: var(--game-text-muted);
	}
	.cb__nav-item {
		display: flex;
		width: 100%;
		min-height: 2.5rem;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		padding: 0 0.55rem;
		border: 1px solid transparent;
		border-left: 3px solid transparent;
		background: transparent;
		color: var(--game-text-muted);
		text-align: left;
		cursor: pointer;
	}
	.cb__nav-item:hover { border-color: var(--game-border); border-left-color: var(--game-border); }
	/* Hard selected state: the lane inverts against the nav's own background,
	 * so its label carries exactly the ratio body text has there. */
	.cb__nav-item--active,
	.cb__nav-item--active:hover {
		border-color: var(--game-text);
		border-left-color: var(--game-state-active);
		background: var(--game-text);
		color: var(--game-material-raised);
	}
	.cb__nav-item:focus-visible {
		outline: 2px solid var(--game-focus-color);
		outline-offset: 1px;
	}
	.cb__nav-item strong { font-weight: 400; }
</style>
