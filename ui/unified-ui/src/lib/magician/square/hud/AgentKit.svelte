<script lang="ts">
	/**
	 * Bounded capabilities / delegation for one selected crew member.
	 * Lives in the command dock so CitizenDetail can stay immediate-command-only.
	 */
	import { createEventDispatcher } from 'svelte';
	import type { CitizenVM } from '../engine/types';
	import { capabilityCategory, capabilityLabel } from '../capabilityLabels';
	import { GameSection } from '../ui';

	export let kind: 'capabilities' | 'delegation';
	export let citizen: CitizenVM;
	export let roster: CitizenVM[] = [];

	const dispatch = createEventDispatcher<{ select: string }>();

	$: capabilities = citizen.tools.map((id) => ({
		id,
		label: capabilityLabel(id),
		category: capabilityCategory(id)
	}));
	$: categories = Array.from(new Set(capabilities.map((item) => item.category)));
	$: targets = citizen.delegationTargets.map((id) => {
		const match = roster.find((member) => member.id === id);
		return { id, name: match?.name ?? id, title: match?.title ?? null };
	});
</script>

{#if kind === 'capabilities'}
	<GameSection
		title="Capabilities"
		description="Tools this crew member is equipped with"
		compact
	>
		{#if capabilities.length === 0}
			<p class="ak__empty">No tools are equipped on {citizen.name}.</p>
		{:else}
			{#each categories as category (category)}
				<h4 class="ak__cat">{category}</h4>
				<ul class="ak__list">
					{#each capabilities.filter((item) => item.category === category) as item (item.id)}
						<li title={item.id}>{item.label}</li>
					{/each}
				</ul>
			{/each}
		{/if}
	</GameSection>
{:else}
	<GameSection
		title="Delegation"
		description="Standing targets this crew member may hand work to"
		compact
	>
		{#if targets.length === 0}
			<p class="ak__empty">{citizen.name} has no standing delegation targets.</p>
		{:else}
			<ul class="ak__list">
				{#each targets as target (target.id)}
					<li>
						<button type="button" class="ak__link" on:click={() => dispatch('select', target.id)}>
							<strong>{target.name}</strong>
							{#if target.title}<span>{target.title}</span>{/if}
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	</GameSection>
{/if}

<style>
	.ak__empty {
		margin: 0;
		color: var(--game-text-muted);
		line-height: 1.5;
	}
	.ak__cat {
		margin: 0.7rem 0 0.25rem;
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm, 0.6875rem);
		font-weight: 400;
		font-synthesis: none;
		letter-spacing: var(--game-display-track-sm, 1px);
		text-transform: uppercase;
		color: var(--game-text-muted);
	}
	.ak__cat:first-child {
		margin-top: 0;
	}
	.ak__list {
		margin: 0;
		padding: 0;
		list-style: none;
		display: grid;
		gap: 0.3rem;
	}
	.ak__list li {
		padding: 0.25rem 0;
		border-bottom: 1px solid var(--game-border-soft, rgba(0, 0, 0, 0.08));
	}
	.ak__link {
		display: grid;
		gap: 0.05rem;
		width: 100%;
		padding: 0;
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
	}
	.ak__link span {
		color: var(--game-text-muted);
		font-size: 0.78rem;
	}
</style>
