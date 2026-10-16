<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import LlmUsageOverview from '$lib/magician/llm/LlmUsageOverview.svelte';
	import { llmUsageWindowWhere } from '$lib/magician/llm/overview';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { CitizenVM } from '../engine/types';
	import { GameStat, GameWorkspace } from '../ui';

	export let citizens: CitizenVM[] = [];
	export let docked = false;

	const dispatch = createEventDispatcher<{ close: void }>();
	const sevenDayWhere = llmUsageWindowWhere(7);

	$: spendReporters = citizens.filter((citizen) => citizen.spendUsd7d != null);
	$: reportedSpend = spendReporters.length > 0
		? spendReporters.reduce((total, citizen) => total + (citizen.spendUsd7d ?? 0), 0)
		: null;
	$: missingReports = Math.max(0, citizens.length - spendReporters.length);
</script>

<GameWorkspace
	open
	title="Usage & cost"
	subtitle="Canonical LLM analytics and budget authority"
	navigation="close"
	navigationLabel="Close Usage & cost"
	presentation={docked ? 'docked' : 'overlay'}
	dismissible={!docked}
	showContext={false}
	showNavigation={false}
	className="treasury-workspace"
	on:back={() => dispatch('close')}
>
	<svelte:fragment slot="actions">
		<a class="ts__full-page" href="/llm">
			<Icon name="arrow-up-right" size={15} />
			Full LLM usage
		</a>
		<a class="ts__full-page" href="/budget">
			<Icon name="arrow-up-right" size={15} />
			Full Budget
		</a>
	</svelte:fragment>

	<section class="ts__signals" aria-label="Town Square usage signals">
		<GameStat
			label="Fleet-reported 7d spend"
			value={reportedSpend == null ? 'Unavailable' : `$${reportedSpend.toFixed(2)}`}
			detail={reportedSpend == null ? 'No crew spend reports' : `${spendReporters.length} of ${citizens.length} crew reporting`}
			tone={reportedSpend == null ? 'neutral' : 'active'}
		>
			<Icon slot="icon" name="zap" size={18} />
		</GameStat>
		<GameStat
			label="Reporting exceptions"
			value={citizens.length === 0 ? 'Unavailable' : missingReports}
			detail={citizens.length === 0 ? 'Crew state has not loaded' : missingReports === 0 ? 'No seven-day report gaps' : `${missingReports} crew without a spend report`}
			tone={missingReports > 0 ? 'attention' : 'success'}
		>
			<Icon slot="icon" name={missingReports > 0 ? 'alert' : 'check'} size={18} />
		</GameStat>
	</section>

	<section class="ts__detail" aria-live="polite">
		<header class="ts__detail-header">
			<div>
				<h2>LLM usage</h2>
				<p>Last seven days</p>
			</div>
			<span>7 days</span>
		</header>

		<LlmUsageOverview
			where={sevenDayWhere}
			rangeLabel="7d"
			compact
			ariaLabel="Town Square LLM usage overview"
		/>
	</section>
</GameWorkspace>

<style>
	:global(.treasury-workspace .game-ui-workspace__body) {
		--overview-surface: var(--game-material-raised);
		--overview-text: var(--game-text);
		--overview-muted: var(--game-text-muted);
		--overview-border: var(--game-border);
		--overview-radius: var(--game-radius-sm);
		--overview-shadow: none;
		--overview-gap: var(--game-space-3);
		--overview-attention: var(--game-state-attention);
		--overview-critical: var(--game-state-danger);
		display: flex;
		flex-direction: column;
		gap: var(--game-space-4);
	}

	.ts__signals {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: var(--game-space-3);
	}

	.ts__detail {
		min-width: 0;
		padding-top: var(--game-space-4);
		border-top: 1px solid var(--game-border);
	}

	.ts__detail-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--game-space-4);
		margin-bottom: var(--game-space-3);
	}

	.ts__detail-header h2,
	.ts__detail-header p { margin: 0; }
	.ts__detail-header h2 {
		font-size: var(--game-type-4);
		letter-spacing: 0;
	}
	.ts__detail-header p,
	.ts__detail-header > span {
		margin-top: var(--game-space-1);
		color: var(--game-text-muted);
		font-size: var(--game-type-2);
	}
	.ts__detail-header > span {
		padding: 0.25rem 0.45rem;
		border: 1px solid var(--game-border);
		border-radius: var(--game-radius-sm);
		background: var(--game-material-muted);
		white-space: nowrap;
	}

	.ts__full-page {
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

	.ts__full-page:hover {
		border-color: var(--game-border-strong);
		background: var(--game-material-selected);
	}
</style>
