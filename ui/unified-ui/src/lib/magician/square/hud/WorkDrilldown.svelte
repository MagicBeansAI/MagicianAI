<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { WorkDrilldownItem, WorkDrilldownModel } from '../fleetWorkDrilldown';
	import { GameObjectiveRow, GameSection } from '../ui';
	import type { GameTone } from '../ui/types';

	export let model: WorkDrilldownModel;
	export let title = 'Work drill-down';
	export let description = 'Canonical task state, bounded for this view';
	export let emptyLabel = 'No current work';

	const dispatch = createEventDispatcher<{ task: string }>();

	function stateLabel(item: WorkDrilldownItem): string {
		switch (item.state) {
			case 'awaiting_orders': return 'Needs direction';
			case 'succeeded': return 'Succeeded';
			case 'failed': return 'Failed';
			case 'cancelled': return 'Cancelled';
			case 'delivering': return 'Delivering';
			case 'planning': return 'Planning';
			case 'active': return 'Active';
			case 'blocked': return 'Blocked';
			case 'ready': return 'Ready';
			default: return item.status || 'Current';
		}
	}

	function toneOf(item: WorkDrilldownItem): GameTone {
		if (item.blocker || item.state === 'failed') return 'attention';
		if (item.state === 'succeeded') return 'success';
		if (['active', 'planning', 'delivering'].includes(item.state)) return 'active';
		return 'neutral';
	}

	function openTask(taskId: string | null): void {
		if (taskId) dispatch('task', taskId);
	}
</script>

<GameSection {title} {description} compact>
	{#if model.focus}
		<div class="wd__focus" data-game-tone={toneOf(model.focus)}>
			<div class="wd__heading">
				<span>{stateLabel(model.focus)}</span>
				{#if model.activeCount > 1}<small>{model.activeCount} current tasks</small>{/if}
			</div>
			<strong>{model.focus.title}</strong>
			{#if model.focus.objective !== model.focus.title}
				<p>{model.focus.objective}</p>
			{/if}

			{#if model.focus.currentStep || model.focus.blocker}
				<dl class="wd__facts">
					{#if model.focus.currentStep}
						<div><dt>Current step</dt><dd>{model.focus.currentStep}</dd></div>
					{/if}
					{#if model.focus.blocker}
						<div class="wd__fact--attention"><dt>Blocker</dt><dd>{model.focus.blocker}</dd></div>
					{/if}
				</dl>
			{/if}

			{#if model.focus.artifacts.length > 0}
				<div class="wd__artifacts" aria-label="Current task artifacts">
					<span>Artifacts</span>
					<ul>
						{#each model.focus.artifacts as artifact}
							<li title={artifact}>{artifact}</li>
						{/each}
					</ul>
				</div>
			{/if}

			{#if model.focus.taskId}
				<button type="button" class="wd__open" on:click={() => openTask(model.focus?.taskId ?? null)}>
					<Icon name="file-text" size={14} /> Open task
				</button>
			{/if}
		</div>

		{#if model.additionalCurrent.length > 0}
			<div class="wd__more" aria-label="Additional current tasks">
				<span>Also in progress</span>
				{#each model.additionalCurrent as item (item.taskId ?? item.title)}
					<GameObjectiveRow
						objective={item.title}
						objectiveId={item.taskId ?? ''}
						detail={item.currentStep ?? item.blocker ?? item.objective}
						state={stateLabel(item)}
						tone={toneOf(item)}
						interactive={item.taskId != null}
						on:select={() => openTask(item.taskId)}
					/>
				{/each}
				{#if model.activeCount > model.additionalCurrent.length + 1}
					<small>{model.activeCount - model.additionalCurrent.length - 1} more available in Tasks</small>
				{/if}
			</div>
		{/if}
	{:else}
		<p class="wd__empty">{emptyLabel}</p>
	{/if}

	{#if model.latestOutcome}
		<div class="wd__outcome">
			<span>Latest outcome</span>
			<GameObjectiveRow
				objective={model.latestOutcome.title}
				objectiveId={model.latestOutcome.taskId ?? ''}
				detail={model.latestOutcome.outcome ?? model.latestOutcome.objective}
				state={stateLabel(model.latestOutcome)}
				tone={toneOf(model.latestOutcome)}
				complete={model.latestOutcome.state === 'succeeded'}
				interactive={model.latestOutcome.taskId != null}
				on:select={() => openTask(model.latestOutcome?.taskId ?? null)}
			/>
			{#if model.latestOutcome.artifacts.length > 0}
				<ul class="wd__outcome-artifacts" aria-label="Latest outcome artifacts">
					{#each model.latestOutcome.artifacts as artifact}
						<li title={artifact}>{artifact}</li>
					{/each}
				</ul>
			{/if}
		</div>
	{/if}
</GameSection>

<style>
	.wd__focus {
		display: grid;
		gap: 0.45rem;
		padding-left: 0.75rem;
		border-left: 3px solid var(--game-state-active);
	}
	.wd__focus[data-game-tone='attention'] { border-left-color: var(--game-state-attention); }
	.wd__focus[data-game-tone='success'] { border-left-color: var(--game-state-success); }
	.wd__focus[data-game-tone='neutral'] { border-left-color: var(--game-border-strong); }
	.wd__heading { display: flex; align-items: center; gap: 0.5rem; min-width: 0; }
	.wd__heading span,
	.wd__more > span,
	.wd__outcome > span,
	.wd__artifacts > span {
		color: var(--game-text-muted);
		font-size: var(--game-type-1);
		font-weight: 750;
		text-transform: uppercase;
	}
	.wd__heading small { margin-left: auto; color: var(--game-text-muted); font-size: var(--game-type-1); }
	.wd__focus > strong { overflow-wrap: anywhere; }
	.wd__focus > p { margin: 0; color: var(--game-text-muted); line-height: 1.45; overflow-wrap: anywhere; }
	.wd__facts { display: grid; gap: 0.4rem; margin: 0.15rem 0 0; }
	.wd__facts > div { display: grid; gap: 0.08rem; }
	.wd__facts dt { color: var(--game-text-muted); font-size: var(--game-type-1); font-weight: 700; }
	.wd__facts dd { margin: 0; line-height: 1.4; overflow-wrap: anywhere; }
	.wd__fact--attention dd { color: var(--game-state-attention); }
	.wd__artifacts { display: grid; gap: 0.3rem; margin-top: 0.1rem; }
	.wd__artifacts ul,
	.wd__outcome-artifacts { display: flex; flex-wrap: wrap; gap: 0.3rem; margin: 0; padding: 0; list-style: none; }
	.wd__artifacts li,
	.wd__outcome-artifacts li {
		max-width: 100%;
		overflow: hidden;
		padding: 0.2rem 0.38rem;
		border: 1px solid var(--game-border);
		border-radius: var(--game-radius-sm);
		background: var(--game-material-muted);
		color: var(--game-text-muted);
		font-size: var(--game-type-1);
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.wd__open {
		display: inline-flex;
		width: max-content;
		align-items: center;
		gap: 0.35rem;
		min-height: 2rem;
		margin-top: 0.1rem;
		padding: 0.35rem 0.55rem;
		border: 1px solid color-mix(in srgb, var(--game-state-active) 55%, var(--game-border));
		border-radius: var(--game-radius-sm);
		background: color-mix(in srgb, var(--game-state-active) 14%, var(--game-material-muted));
		color: var(--game-text);
		font: inherit;
		font-size: var(--game-type-2);
		font-weight: 700;
		cursor: pointer;
	}
	.wd__more,
	.wd__outcome { display: grid; gap: 0.4rem; margin-top: 0.75rem; }
	.wd__more > small { color: var(--game-text-muted); font-size: var(--game-type-1); text-align: right; }
	.wd__empty { margin: 0; color: var(--game-text-muted); font-size: var(--game-type-2); }

	/* This drill-down is hosted by two panels — the Citizen Inspector, which
	 * carries the crew skin, and the Guild Inspector, which does not. Gate the
	 * display face on the host rather than on this component so the two hosts
	 * stay visually honest about which system they belong to. */
	:global([data-game-skin='pixel']) .wd__heading span,
	:global([data-game-skin='pixel']) .wd__more > span,
	:global([data-game-skin='pixel']) .wd__outcome > span,
	:global([data-game-skin='pixel']) .wd__artifacts > span,
	:global([data-game-skin='pixel']) .wd__facts dt {
		font-family: var(--game-font-display);
		font-size: var(--game-display-sm);
		font-weight: 400;
		font-synthesis: none;
		letter-spacing: var(--game-display-track-sm);
		text-transform: uppercase;
	}
	/* Raw tone colours are fills, not text: --game-state-attention as a label
	 * on the opaque panel measured 4.07:1 on the default light theme and 3.37
	 * to 3.95 across the dark ones, so the blocker line takes the same pull
	 * toward body text that --game-tone-text applies everywhere else. That
	 * lands it at 5.17 to 7.32 over the same set. */
	:global([data-game-skin='pixel']) .wd__fact--attention dd {
		color: color-mix(in srgb, var(--game-state-attention) 60%, var(--game-text));
	}
</style>
