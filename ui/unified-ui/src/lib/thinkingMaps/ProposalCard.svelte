<!--
  ProposalCard — a staged restructure proposal awaiting the owner's decision.

  `consolidate(id)` stages a pending `RestructureProposal`; this card renders the
  FIRST proposal still in `state === 'proposed'`. It shows the rationale and a
  one-line summary of blast radius, then:
    · Confirm — `decideProposal(id, proposal_id, 'confirm')` — applies the bundle
                (its reorganized / new nodes then appear on the next poll).
    · Reject  — `decideProposal(id, proposal_id, 'reject')`.

  Provisional (dashed / accent) styling keeps it visually consistent with the
  AI-suggested nodes on the canvas.
-->
<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { RestructureProposal } from '$lib/types/thinkingMap';
	import Button from '$lib/magician/components/native/Button.svelte';

	/** The first proposed proposal (chosen on the page). */
	export let proposal: RestructureProposal;
	export let busy = false;
	/** Page handler: confirm/reject via `decideProposal`. Throws on failure. */
	export let decide: (proposalId: string, decision: 'confirm' | 'reject') => Promise<void>;

	const dispatch = createEventDispatcher<{ mutated: void }>();

	let localError: string | null = null;

	async function onDecide(decision: 'confirm' | 'reject'): Promise<void> {
		if (busy) return;
		localError = null;
		try {
			await decide(proposal.proposal_id, decision);
			dispatch('mutated');
		} catch (err) {
			localError = err instanceof Error ? err.message : String(err);
		}
	}

	$: affected = proposal.affected_node_ids?.length ?? 0;
	$: changes = proposal.operations?.length ?? 0;
</script>

<section class="pr" aria-label="Restructure proposal">
	<header class="pr__head">
		<span class="pr__mark" aria-hidden="true">✦</span>
		<h2>Suggested restructure</h2>
	</header>

	{#if proposal.rationale}
		<p class="pr__rationale">{proposal.rationale}</p>
	{/if}

	<p class="pr__summary">
		Affects {affected} node{affected === 1 ? '' : 's'} · {changes} change{changes === 1 ? '' : 's'}
	</p>

	<div class="pr__actions">
		<Button
			variant="primary"
			size="sm"
			label={busy ? 'Working…' : 'Confirm'}
			interactive={!busy}
			on:click={() => onDecide('confirm')}
		/>
		<Button
			variant="outline"
			size="sm"
			label="Reject"
			interactive={!busy}
			on:click={() => onDecide('reject')}
		/>
	</div>

	{#if localError}
		<div class="pr__error" role="alert">{localError}</div>
	{/if}
</section>

<style>
	.pr {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		padding: 0.85rem;
		border: 1px dashed color-mix(in srgb, var(--accent-secondary, currentColor) 50%, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--accent-secondary, transparent) 7%, var(--bg-card));
		box-shadow: var(--shadow-sm);
		animation: pr-in var(--transition-base, 0.25s) var(--ease-settle, ease) both;
	}

	@keyframes pr-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.pr {
			animation: none;
		}
	}

	.pr__head {
		display: flex;
		align-items: center;
		gap: 0.45rem;
	}

	.pr__mark {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.35rem;
		height: 1.35rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-secondary) 18%, transparent);
		color: var(--accent-secondary);
		font-size: 0.78rem;
	}

	.pr__head h2 {
		margin: 0;
		font-size: 0.78rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-secondary);
	}

	.pr__rationale {
		margin: 0;
		font-size: 0.85rem;
		line-height: 1.45;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.pr__summary {
		margin: 0;
		font-size: 0.75rem;
		color: var(--text-secondary);
	}

	.pr__actions {
		display: flex;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.pr__error {
		padding: 0.4rem 0.6rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--color-error, var(--status-failed));
		font-size: 0.78rem;
	}
</style>
