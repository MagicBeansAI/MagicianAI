<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { CitizenVM } from '../engine/types';
	import {
		fetchFleetRelations,
		type FleetRelations,
		type RelationEdge
	} from '../fleetRelations';
	import { GameWorkspace } from '../ui';

	export let citizens: CitizenVM[] = [];
	export let docked = false;

	const dispatch = createEventDispatcher<{ close: void; citizen: string }>();
	type SourceNode = {
		id: string;
		citizen: CitizenVM | null;
		edges: RelationEdge[];
		standingTargetIds: string[];
		handoffs: number;
	};

	let relations: FleetRelations | null = null;
	let loading = true;
	let activeSourceId = '';

	onMount(() => {
		void fetchFleetRelations().then((next) => {
			relations = next;
			loading = false;
		});
	});

	$: citizenById = new Map(citizens.map((citizen) => [citizen.id, citizen]));
	$: sourceNodes = buildSourceNodes(citizens, relations);
	$: if (
		sourceNodes.length > 0 &&
		!sourceNodes.some((source) => source.id === activeSourceId)
	) {
		activeSourceId = sourceNodes[0].id;
	}
	$: activeSource = sourceNodes.find((source) => source.id === activeSourceId) ?? null;
	$: standingOnly = activeSource
		? activeSource.standingTargetIds.filter(
				(targetId) => !activeSource?.edges.some((edge) => edge.to === targetId)
			)
		: [];
	$: totalHandoffs = relations?.edges.reduce((sum, edge) => sum + edge.count, 0) ?? 0;
	$: selectedResolved =
		activeSource?.edges.reduce((sum, edge) => sum + edge.ok + edge.fail, 0) ?? 0;
	$: selectedSuccessful = activeSource?.edges.reduce((sum, edge) => sum + edge.ok, 0) ?? 0;
	$: selectedReliability = selectedResolved
		? Math.round((selectedSuccessful / selectedResolved) * 100)
		: null;

	function buildSourceNodes(
		currentCitizens: CitizenVM[],
		currentRelations: FleetRelations | null
	): SourceNode[] {
		const observed = new Map<string, RelationEdge[]>();
		for (const edge of currentRelations?.edges ?? []) {
			observed.set(edge.from, [...(observed.get(edge.from) ?? []), edge]);
		}
		const currentById = new Map(currentCitizens.map((citizen) => [citizen.id, citizen]));
		const sourceIds = new Set(observed.keys());
		for (const citizen of currentCitizens) {
			if (citizen.delegationTargets.length > 0) sourceIds.add(citizen.id);
		}
		return [...sourceIds]
			.map((id) => {
				const edges = observed.get(id) ?? [];
				return {
					id,
					citizen: currentById.get(id) ?? null,
					edges,
					standingTargetIds: currentById.get(id)?.delegationTargets ?? [],
					handoffs: edges.reduce((sum, edge) => sum + edge.count, 0)
				};
			})
			.sort((a, b) => b.handoffs - a.handoffs || nameOf(a.id).localeCompare(nameOf(b.id)));
	}

	function nameOf(id: string): string {
		return citizenById.get(id)?.name ?? id;
	}

	function reliabilityOf(edge: RelationEdge): number | null {
		const resolved = edge.ok + edge.fail;
		return resolved ? Math.round((edge.ok / resolved) * 100) : null;
	}

	function edgeTone(edge: RelationEdge): 'active' | 'success' | 'attention' | 'danger' {
		const reliability = reliabilityOf(edge);
		if (reliability != null && reliability < 60) return 'danger';
		if (edge.fail > 0 || reliability == null) return 'attention';
		return edge.ok > 0 ? 'success' : 'active';
	}

</script>

<GameWorkspace
	open
	title="Delegation"
	subtitle="How work moves between crew members"
	presentation={docked ? 'docked' : 'overlay'}
	dismissible={!docked}
	showNavigation={!docked && sourceNodes.length > 0}
	navigationLabel="Return to Town Square"
	busy={loading && sourceNodes.length === 0}
	on:back={() => dispatch('close')}
>
	<svelte:fragment slot="context">
		<div class="co__context">
			<span><Icon name="git-branch" size={17} /> Delegation network</span>
			<strong>{sourceNodes.length} sources</strong>
			<strong>{totalHandoffs} handoffs</strong>
			<strong>{relations ? `${relations.tasksScanned} tasks sampled` : loading ? 'Loading activity' : 'Configured routes'}</strong>
		</div>
	</svelte:fragment>

	<svelte:fragment slot="navigation">
		<p class="co__nav-label">Crew members</p>
		{#each sourceNodes as source (source.id)}
			<button
				type="button"
				class="co__nav-item"
				class:co__nav-item--active={source.id === activeSourceId}
				aria-pressed={source.id === activeSourceId}
				on:click={() => (activeSourceId = source.id)}
			>
				<span>
					<strong>{nameOf(source.id)}</strong>
					<small>{source.citizen?.title ?? 'External source'}</small>
				</span>
				<b>{source.handoffs || source.standingTargetIds.length}</b>
			</button>
		{/each}
	</svelte:fragment>

	{#if loading && sourceNodes.length === 0}
		<div class="co__empty">Loading delegation routes...</div>
	{:else if sourceNodes.length === 0}
		<div class="co__empty">No observed handoffs or standing delegation routes are available.</div>
	{:else if activeSource}
		<header class="co__hero">
			<span class="co__hero-avatar" aria-hidden="true">{nameOf(activeSource.id).charAt(0).toUpperCase()}</span>
			<div>
				<p>Delegates from</p>
				<h2>{nameOf(activeSource.id)}</h2>
				<span>{activeSource.citizen?.role ?? 'Observed in recent execution records.'}</span>
			</div>
			{#if activeSource.citizen}
				<button type="button" on:click={() => dispatch('citizen', activeSource?.id ?? '')}>
					<Icon name="eye" size={16} /> Open crew member
				</button>
			{/if}
		</header>

		<section class="co__network" aria-label={`Delegation routes from ${nameOf(activeSource.id)}`}>
			<header class="co__network-summary">
				<div><strong>{activeSource.edges.length}</strong><span>active routes</span></div>
				<div><strong>{activeSource.handoffs}</strong><span>handoffs</span></div>
				<div><strong>{selectedReliability == null ? '—' : `${selectedReliability}%`}</strong><span>reliability</span></div>
			</header>
			{#if activeSource.edges.length === 0}
				<p class="co__none">No recent handoffs from this crew member.</p>
			{:else}
				<div class="co__routes">
					{#each activeSource.edges as edge (`${edge.from}->${edge.to}`)}
						<button type="button" class="co__route" data-tone={edgeTone(edge)} on:click={() => dispatch('citizen', edge.to)}>
							<span class="co__route-arrow"><Icon name="arrow-right" size={15} /></span>
							<span class="co__route-avatar" aria-hidden="true">{nameOf(edge.to).charAt(0).toUpperCase()}</span>
							<span class="co__route-person"><strong>{nameOf(edge.to)}</strong><small>{citizenById.get(edge.to)?.title ?? 'Crew member'}</small></span>
							<span class="co__route-count"><strong>{edge.count}</strong><small>handoffs</small></span>
							<span class="co__route-meter"><i style={`width:${reliabilityOf(edge) ?? 0}%`}></i></span>
							<span class="co__route-result">{edge.ok} delivered · {edge.fail} failed</span>
						</button>
					{/each}
				</div>
			{/if}
		</section>

		{#if standingOnly.length > 0}
			<section class="co__configured">
				<header><strong>Configured routes</strong><span>Available but not recently used</span></header>
				<div>
					{#each standingOnly as targetId (targetId)}
						<button type="button" on:click={() => dispatch('citizen', targetId)}><span>{nameOf(targetId).charAt(0).toUpperCase()}</span><strong>{nameOf(targetId)}</strong><Icon name="arrow-right" size={14} /></button>
					{/each}
				</div>
			</section>
		{/if}
	{/if}
</GameWorkspace>

<style>
	.co__context {
		display: flex;
		align-items: center;
		gap: 1.25rem;
		height: 100%;
		padding: 0 1.5rem;
		color: var(--game-text-muted);
	}
	.co__context span {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		margin-right: auto;
		color: var(--game-text);
		font-weight: 750;
	}
	.co__context strong,
	.co__nav-label {
		font-size: var(--game-type-2);
		text-transform: uppercase;
	}
	.co__nav-label {
		margin: 0 0 0.55rem;
		color: var(--game-text-muted);
		font-weight: 750;
	}
	.co__nav-item {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		width: 100%;
		min-height: 3.25rem;
		align-items: center;
		gap: 0.5rem;
		padding: 0.45rem 0.55rem;
		border: 0;
		border-left: 2px solid transparent;
		background: transparent;
		color: var(--game-text-muted);
		font: inherit;
		text-align: left;
		cursor: pointer;
	}
	.co__nav-item--active {
		border-left-color: var(--game-state-active);
		background: var(--game-material-selected);
		color: var(--game-text);
	}
	.co__nav-item span { min-width: 0; }
	.co__nav-item strong,
	.co__nav-item small {
		display: block;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.co__nav-item small {
		margin-top: 0.15rem;
		color: var(--game-text-muted);
		font-size: var(--game-type-1);
	}
	.co__nav-item b { font-size: var(--game-type-2); }
	.co__hero {
		display: flex;
		align-items: center;
		gap: 1rem;
		padding-bottom: 1rem;
		border-bottom: 2px solid var(--game-state-active);
	}
	.co__hero-avatar { display: grid; flex: 0 0 auto; width: 4rem; height: 4rem; place-items: center; border: 1px solid color-mix(in srgb, var(--game-state-active) 55%, var(--game-border)); border-radius: 50%; background: color-mix(in srgb, var(--game-state-active) 12%, var(--game-material-muted)); color: var(--game-state-active); font-size: var(--game-type-5); font-weight: 800; }
	.co__hero > div { min-width: 0; }
	.co__hero p {
		margin: 0 0 0.3rem;
		color: var(--game-state-active);
		font-size: var(--game-type-2);
		font-weight: 750;
		text-transform: uppercase;
	}
	.co__hero h2 {
		margin: 0;
		font-size: var(--game-type-5);
		letter-spacing: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.co__hero span {
		display: -webkit-box;
		max-width: 72ch;
		margin-top: 0.35rem;
		overflow: hidden;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		color: var(--game-text-muted);
		line-height: 1.45;
	}
	.co__hero button {
		display: inline-flex;
		flex: 0 0 auto;
		align-items: center;
		justify-content: center;
		gap: 0.35rem;
		min-height: 2.4rem;
		padding: 0.4rem 0.75rem;
		border: 1px solid var(--game-border);
		border-radius: var(--game-radius-sm);
		background: var(--game-material-raised);
		color: var(--game-text);
		font: inherit;
		font-weight: 700;
		cursor: pointer;
		margin-left: auto;
	}
	.co__network { padding: 1.25rem 0; }
	.co__network-summary { display: flex; gap: 2rem; margin-bottom: 1rem; }
	.co__network-summary div { display: grid; gap: 0.1rem; }
	.co__network-summary strong { font-size: var(--game-type-4); }
	.co__network-summary span { color: var(--game-text-muted); font-size: var(--game-type-1); text-transform: uppercase; }
	.co__routes { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 0.65rem; }
	.co__route { position: relative; display: grid; grid-template-columns: 2.75rem minmax(0, 1fr) auto; grid-template-rows: auto auto auto; align-items: center; gap: 0.45rem 0.65rem; min-width: 0; min-height: 8rem; padding: 0.75rem; border: 1px solid var(--game-border); border-top: 2px solid var(--route-tone, var(--game-state-active)); border-radius: var(--game-radius-md); background: var(--game-material-raised); color: var(--game-text); font: inherit; text-align: left; cursor: pointer; }
	.co__route[data-tone='success'] { --route-tone: var(--game-state-success); }
	.co__route[data-tone='attention'] { --route-tone: var(--game-state-attention); }
	.co__route[data-tone='danger'] { --route-tone: var(--game-state-danger); }
	.co__route:hover, .co__route:focus-visible { border-color: var(--route-tone); background: var(--game-material-selected); outline: 2px solid color-mix(in srgb, var(--route-tone) 42%, transparent); outline-offset: 1px; }
	.co__route-arrow { position: absolute; top: 0.45rem; left: -0.55rem; display: grid; width: 1.35rem; height: 1.35rem; place-items: center; border: 1px solid var(--game-border); border-radius: 50%; background: var(--game-material-workspace); color: var(--route-tone); }
	.co__route-avatar { display: grid; grid-row: 1 / span 2; width: 2.75rem; height: 2.75rem; place-items: center; border-radius: 50%; background: color-mix(in srgb, var(--route-tone) 13%, var(--game-material-muted)); color: var(--route-tone); font-size: var(--game-type-4); font-weight: 800; }
	.co__route-person { display: grid; min-width: 0; gap: 0.12rem; }
	.co__route-person strong, .co__route-person small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.co__route-person small, .co__route-count small, .co__route-result { color: var(--game-text-muted); font-size: var(--game-type-1); }
	.co__route-count { display: grid; text-align: right; }
	.co__route-meter { grid-column: 1 / -1; height: 0.28rem; overflow: hidden; border-radius: 1rem; background: var(--game-material-muted); }
	.co__route-meter i { display: block; height: 100%; background: var(--route-tone); }
	.co__route-result { grid-column: 1 / -1; }
	.co__configured { padding-top: 1rem; border-top: 1px solid var(--game-border); }
	.co__configured > header { display: grid; gap: 0.15rem; margin-bottom: 0.65rem; }
	.co__configured > header span { color: var(--game-text-muted); font-size: var(--game-type-1); }
	.co__configured > div { display: flex; flex-wrap: wrap; gap: 0.45rem; }
	.co__configured button { display: inline-flex; align-items: center; gap: 0.45rem; min-height: 2.4rem; padding: 0.35rem 0.6rem; border: 1px solid var(--game-border); border-radius: var(--game-radius-md); background: var(--game-material-raised); color: var(--game-text); font: inherit; cursor: pointer; }
	.co__configured button > span { display: grid; width: 1.55rem; height: 1.55rem; place-items: center; border-radius: 50%; background: var(--game-material-muted); color: var(--game-text-muted); font-size: var(--game-type-1); font-weight: 800; }
	.co__none {
		margin: 0;
		color: var(--game-text-muted);
	}
	.co__empty {
		display: grid;
		min-height: 20rem;
		place-items: center;
		color: var(--game-text-muted);
	}
</style>
