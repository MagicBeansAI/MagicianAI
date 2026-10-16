<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { CitizenVM, LandmarkId } from '../engine/types';
	import type { FleetStateDelivery } from '../fleetState';
	import { capabilityCategory, capabilityLabel } from '../capabilityLabels';
	import { relativeTime } from '../derive';
	import { GameInspector, GameSection, GameStat } from '../ui';

	export let landmark: Extract<LandmarkId, 'hall' | 'armory'>;
	export let citizens: CitizenVM[] = [];
	export let deliveries: FleetStateDelivery[] = [];

	const dispatch = createEventDispatcher<{
		close: void;
		citizen: string;
		quest: void;
		reviewtask: string;
		followup: FleetStateDelivery;
	}>();

	$: capabilities = Array.from(
		citizens.reduce((index, citizen) => {
			for (const tool of citizen.tools) {
				const wielders = index.get(tool) ?? [];
				wielders.push(citizen);
				index.set(tool, wielders);
			}
			return index;
		}, new Map<string, CitizenVM[]>())
	).map(([id, wielders]) => ({ id, wielders, label: capabilityLabel(id), category: capabilityCategory(id) }));
	$: categories = Array.from(new Set(capabilities.map((capability) => capability.category)));
	$: activeCitizens = citizens.filter((citizen) => citizen.vibe === 'working').length;
	$: needsCitizens = citizens.filter((citizen) => citizen.vibe === 'needs').length;

	type DeliveryTone = 'success' | 'attention' | 'neutral';

	function deliveryTone(status: string): DeliveryTone {
		switch (status.trim().toLowerCase()) {
			case 'done':
				return 'success';
			case 'failed':
			case 'needs_action':
				return 'attention';
			default:
				return 'neutral';
		}
	}

	function deliveryStatusLabel(status: string): string {
		switch (status.trim().toLowerCase()) {
			case 'done':
				return 'Delivered';
			case 'failed':
				return 'Failed';
			case 'needs_action':
				return 'Needs review';
			case 'running':
				return 'In progress';
			default:
				return 'Update';
		}
	}

	function deliveryTodayHref(delivery: FleetStateDelivery): string {
		const params = new URLSearchParams({ tab: 'delivered', selected_item: delivery.id });
		return `/today?${params.toString()}`;
	}

	function deliveryCanUseFollowUp(delivery: FleetStateDelivery): boolean {
		const status = delivery.status.trim().toLowerCase();
		return Boolean(delivery.summary?.trim()) || status === 'failed' || status === 'needs_action';
	}

	function reviewDeliveryTask(delivery: FleetStateDelivery): void {
		if (delivery.task_id) dispatch('reviewtask', delivery.task_id);
	}
</script>

<GameInspector
	open
	title={landmark === 'hall' ? 'Task router' : 'Capabilities'}
	eyebrow={landmark === 'hall' ? 'Task assignment' : 'Crew tools'}
	subtitle={landmark === 'hall' ? 'Create tasks and review crew readiness' : 'Tools available across the crew'}
	width="wide"
	tone={landmark === 'hall' && needsCitizens > 0 ? 'attention' : 'neutral'}
	on:back={() => dispatch('close')}
>
	{#if landmark === 'hall'}
		<GameSection title="Crew readiness" compact>
			<div class="li__stats">
				<GameStat label="Crew" value={citizens.length} />
				<GameStat label="Working" value={activeCitizens} tone="active" />
				<GameStat label="Needs you" value={needsCitizens} tone={needsCitizens > 0 ? 'attention' : 'neutral'} />
			</div>
		</GameSection>
		<GameSection title="Create a task" description="Automatically assign it, or drag the task marker directly onto a crew member or program." compact>
			<button type="button" class="li__primary" on:click={() => dispatch('quest')}><Icon name="flag" size={15} /> Create task</button>
		</GameSection>
		<GameSection title="How work moves" compact>
			<p class="li__copy">The crew works autonomously until an approval, blocker, or strategic decision needs you. Completed work returns here for review.</p>
		</GameSection>
		<GameSection title="Recent outcomes" description="Completed work returned by the crew" compact>
			{#if deliveries.length > 0}
				<div class="li__deliveries">
					{#each deliveries.slice(0, 6) as delivery (delivery.id)}
						<article class="li__delivery-signal">
							<span class="li__delivery-tone" data-tone={deliveryTone(delivery.status)} aria-hidden="true"></span>
							<div class="li__delivery-copy">
								<strong>{delivery.title}</strong>
								<span class="li__delivery-meta">
									{deliveryStatusLabel(delivery.status)} - {relativeTime(delivery.updated_at || delivery.created_at)}
								</span>
								{#if delivery.summary}<p>{delivery.summary}</p>{/if}
							</div>
							<div class="li__delivery-actions">
								{#if delivery.task_id}
									<button type="button" on:click={() => reviewDeliveryTask(delivery)}>
										<Icon name="file-text" size={14} /> Review task
									</button>
								{:else}
									<a href={deliveryTodayHref(delivery)}>
										<Icon name="arrow-up-right" size={14} /> Open in Today
									</a>
								{/if}
								{#if deliveryCanUseFollowUp(delivery)}
									<button type="button" on:click={() => dispatch('followup', delivery)}>
										<Icon name="flag" size={14} /> Create follow-up
									</button>
								{/if}
							</div>
						</article>
					{/each}
				</div>
			{:else}<p class="li__copy">No completed work is available yet.</p>{/if}
		</GameSection>
	{:else}
		<GameSection title="Capability coverage" compact>
			<div class="li__stats">
				<GameStat label="Capabilities" value={capabilities.length} />
				<GameStat label="Categories" value={categories.length} />
				<GameStat label="Crew equipped" value={citizens.filter((citizen) => citizen.tools.length > 0).length} />
			</div>
		</GameSection>
		{#each categories as category (category)}
			<GameSection title={category} compact>
				<div class="li__capabilities">
					{#each capabilities.filter((capability) => capability.category === category) as capability (capability.id)}
						<div class="li__capability" title={capability.id}>
							<strong>{capability.label}</strong>
							<span>{capability.wielders.length} equipped</span>
							<div>{#each capability.wielders.slice(0, 4) as citizen (citizen.id)}<button type="button" on:click={() => dispatch('citizen', citizen.id)}>{citizen.name}</button>{/each}</div>
						</div>
					{/each}
				</div>
			</GameSection>
		{/each}
	{/if}
</GameInspector>

<style>
		.li__stats { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 0.4rem; }
		.li__copy { margin: 0; color: var(--game-text-muted); line-height: 1.6; }
		.li__primary { display: inline-flex; align-items: center; gap: 0.4rem; min-height: 2.5rem; padding: 0.5rem 0.8rem; border: 1px solid color-mix(in srgb, var(--game-state-active) 55%, var(--game-border)); border-radius: var(--game-radius-sm); background: var(--game-material-selected); color: var(--game-text); font: inherit; font-weight: 750; cursor: pointer; }
	.li__capabilities { display: grid; gap: 0.55rem; }
	.li__deliveries { display: grid; gap: 0.2rem; }
	.li__delivery-signal { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 0.4rem 0.55rem; min-width: 0; padding: 0.55rem 0.3rem; border-bottom: 1px solid var(--game-border); }
	.li__delivery-tone { width: 0.55rem; height: 0.55rem; margin-top: 0.3rem; border-radius: 50%; background: var(--game-text-muted); box-shadow: 0 0 0 2px color-mix(in srgb, var(--game-text-muted) 18%, transparent); }
	.li__delivery-tone[data-tone='success'] { background: var(--game-state-success); box-shadow: 0 0 0 2px color-mix(in srgb, var(--game-state-success) 20%, transparent); }
	.li__delivery-tone[data-tone='attention'] { background: var(--game-state-attention); box-shadow: 0 0 0 2px color-mix(in srgb, var(--game-state-attention) 20%, transparent); }
	.li__delivery-copy { display: grid; min-width: 0; gap: 0.12rem; }
	.li__delivery-copy strong, .li__delivery-copy p { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.li__delivery-copy p { margin: 0.12rem 0 0; color: var(--game-text-muted); font-size: var(--game-type-2); }
	.li__delivery-meta { color: var(--game-text-muted); font-size: var(--game-type-2); }
	.li__delivery-actions { grid-column: 2; display: flex; flex-wrap: wrap; gap: 0.35rem 0.75rem; }
	.li__delivery-actions button, .li__delivery-actions a { display: inline-flex; align-items: center; gap: 0.3rem; min-height: 1.75rem; padding: 0; border: 0; background: transparent; color: var(--game-state-active); font: inherit; font-size: var(--game-type-2); font-weight: 700; text-decoration: none; cursor: pointer; }
	.li__delivery-actions button:hover, .li__delivery-actions a:hover { color: var(--game-text); }
	.li__capability { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 0.15rem 0.75rem; min-width: 0; padding-bottom: 0.5rem; border-bottom: 1px solid var(--game-border); }
	.li__capability strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.li__capability > span { color: var(--game-text-muted); font-size: var(--game-type-2); }
	.li__capability > div { grid-column: 1 / -1; display: flex; flex-wrap: wrap; gap: 0.3rem; }
	.li__capability button { max-width: 10rem; padding: 0; border: 0; background: transparent; color: var(--game-state-active); font: inherit; font-size: var(--game-type-2); cursor: pointer; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
