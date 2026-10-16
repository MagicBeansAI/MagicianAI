<script lang="ts">
	import type {
		AttentionDecisionItem,
		AttentionRoute,
		AttentionRoutingPage
	} from './attentionRouting';

	export let item: AttentionDecisionItem | null = null;
	export let page: AttentionRoutingPage | null = null;

	$: applied = item?.routing_mode === 'canary' && item.route_applied === true;
	$: stateLabel = !item
		? null
		: item.routing_mode === 'shadow'
			? 'Route preview'
			: applied
				? 'Canary route active'
				: item.routing_mode === 'canary'
					? 'Canary retained baseline'
					: 'Baseline route';
	$: auditTitle = item
		? [
				`Decision ${item.decision_id}`,
				`Candidate ${item.candidate_id}`,
				`Revision ${item.source_revision ?? 'null'}`,
				`Served ${routeLabel(item.served_route)}`,
				`Reason ${item.route_reason}`,
				item.routing_snapshot_id ? `Snapshot ${item.routing_snapshot_id}` : 'No routing snapshot',
				item.routing_model_version ? `Model ${item.routing_model_version}` : 'No routing model',
				item.utility_margin === null ? 'Utility margin unavailable' : `Utility margin ${item.utility_margin}`
			].join(' · ')
		: '';

	function routeLabel(route: AttentionRoute): string {
		if (route === 'follow_up') return 'Follow-up';
		if (route === 'worth_a_look') return 'Worth a look';
		return 'Non-surfaced';
	}

	function reasonLabel(reason: string): string {
		return reason.replace(/_/g, ' ');
	}
</script>

{#if item && stateLabel}
	<div
		class="attention-routing"
		class:attention-routing--active={applied}
		class:attention-routing--preview={item.routing_mode === 'shadow'}
		title={auditTitle}
		aria-label="Lane-routing decision"
	>
		<strong>{stateLabel}</strong>
		<span>{routeLabel(item.baseline_route)} → {routeLabel(item.learned_route)}</span>
		<span>Served {routeLabel(item.served_route)}</span>
		{#if item.learned_route_confidence !== null}
			<span>{Math.round(item.learned_route_confidence * 100)}% confidence</span>
		{/if}
		<span>{reasonLabel(item.route_reason)}</span>
		<span>{item.selected ? 'Selected' : 'All candidates only'}</span>
		{#if page?.health.all_candidates_path}
			<a href={page.health.all_candidates_path}>All candidates</a>
		{/if}
	</div>
{/if}

<style>
	.attention-routing {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.25rem 0.55rem;
		margin-top: 0.35rem;
		color: var(--text-muted, var(--text-secondary));
		font-size: 0.68rem;
	}

	.attention-routing strong {
		padding: 0.12rem 0.35rem;
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-secondary);
	}

	.attention-routing--preview strong {
		color: var(--color-info, #4d9de0);
	}

	.attention-routing--active strong {
		color: var(--color-success, #2f8f5b);
	}

	.attention-routing a {
		color: var(--accent-primary);
		font-weight: 650;
	}
</style>
