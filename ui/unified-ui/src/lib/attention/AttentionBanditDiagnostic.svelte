<script lang="ts">
	import type { AttentionBanditDecision } from './attentionBandit';

	export let decision: AttentionBanditDecision | null = null;

	$: active = decision?.mode === 'canary' && decision.applied;
	$: label = active
		? 'Personal canary active'
		: decision?.mode === 'shadow'
			? 'Personal ranking preview'
			: decision?.mode === 'canary'
				? 'Canary baseline retained'
				: 'Personal ranking disabled';
	$: degradation = decision?.degradation_reason?.replace(/_/g, ' ') ?? null;
</script>

{#if decision}
	<div
		class="attention-bandit"
		class:attention-bandit--active={active}
		class:attention-bandit--preview={decision.mode === 'shadow'}
		class:attention-bandit--warning={!decision.support || degradation !== null}
		data-testid="attention-bandit-diagnostic"
	>
		<span class="attention-bandit__label">{label}</span>
		<span>Served position {decision.served_position}</span>
		{#if decision.proposed_position !== null}
			<span>Proposed {decision.proposed_position}</span>
		{/if}
		<span>Propensity {decision.served_propensity.toFixed(3)}</span>
		<span>Posterior v{decision.posterior_version}</span>
		<span>Posterior draws {decision.posterior_draw_count}</span>
		{#if decision.posterior_uncertainty !== null}
			<span>Uncertainty {decision.posterior_uncertainty.toFixed(3)}</span>
		{/if}
		{#if decision.exploration}<span>Exploration</span>{/if}
		<span>{decision.support ? 'Supported' : 'Unsupported'}</span>
		{#if decision.policy_snapshot_id}
			<span title={decision.policy_snapshot_id}>Snapshot {decision.policy_snapshot_id}</span>
		{/if}
		{#if decision.policy_model_version}
			<span title={decision.policy_model_version}>Model {decision.policy_model_version}</span>
		{/if}
		<span title={decision.seed_identity}>Replay seed {decision.seed_identity}</span>
		{#if degradation}<span>Degraded · {degradation}</span>{/if}
		<span class="attention-bandit__server-note">Server-served order · browser display only</span>
	</div>
{/if}

<style>
	.attention-bandit {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem 0.55rem;
		margin-top: 0.35rem;
		color: var(--text-muted, var(--text-secondary));
		font-size: 0.68rem;
		font-variant-numeric: tabular-nums;
	}

	.attention-bandit__label {
		color: var(--text-secondary);
		font-weight: 700;
	}

	.attention-bandit--active .attention-bandit__label {
		color: var(--color-success, #2f8f5b);
	}

	.attention-bandit--preview .attention-bandit__label {
		color: var(--color-info, #4d9de0);
	}

	.attention-bandit--warning .attention-bandit__label {
		color: var(--color-warning, #9a6410);
	}

	.attention-bandit__server-note {
		font-style: italic;
	}
</style>
