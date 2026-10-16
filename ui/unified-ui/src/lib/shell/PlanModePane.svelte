<script lang="ts">
	/**
	 * PlanModePane — surfaces the plan-mode toggle state for the
	 * current thread and the count of items currently waiting on the
	 * operator.
	 *
	 * Phase H consolidation (unified-ui v0.0.331+): the inline
	 * Approve / Reject list this pane used to render is gone. All HITL
	 * response UX is consolidated in the global Attention center. This
	 * pane now shows only:
	 *   1. Whether plan-mode is ON for this thread (so the operator
	 *      knows their actions are being gated).
	 *   2. How many items are currently waiting on a response, with a
	 *      CTA that opens the center in place.
	 *
	 * The old subscription filter (`HitlRequested` with
	 * `kind: "pre_action_approval"`) never matched any real emit site
	 * — no backend code ever stamped that kind — so the list was
	 * effectively decorative. Removing it loses nothing functional.
	 */
	import {
		attentionCenterState,
		hitlOpenTargetFromPendingEntry,
		openAttentionCenter,
		openHitlPrompt
	} from '$lib/attention';
	import { showError } from '$lib/shared/stores/notifications';
	import { pendingHitlCount, pendingHitlEntries } from '$lib/stores/pendingHitlStore';

	/// null = the unscoped /dev workbench (no thread label).
	export let threadId: string | null = null;
	export let planModeEnabled: boolean;

	async function openPendingHitl(): Promise<void> {
		if ($pendingHitlEntries.length !== 1) {
			openAttentionCenter();
			return;
		}
		const target = hitlOpenTargetFromPendingEntry($pendingHitlEntries[0]);
		if (!target) {
			openAttentionCenter();
			return;
		}
		const result = await openHitlPrompt(target);
		if (result.status === 'error') showError(result.error);
	}
</script>

{#if planModeEnabled || $pendingHitlCount > 0}
	<section class="plan-pane" aria-label="Plan-mode status">
		<header class="plan-pane__head">
			<span class="plan-pane__title">Plan mode</span>
			{#if planModeEnabled}
				<span class="plan-pane__badge plan-pane__badge--on">ON</span>
			{:else}
				<span class="plan-pane__badge">off</span>
			{/if}
			{#if threadId}<span class="plan-pane__thread">#{threadId}</span>{/if}
		</header>

		{#if $pendingHitlCount > 0}
			<button
				type="button"
				class="plan-pane__cta"
				aria-haspopup="dialog"
				aria-expanded={$attentionCenterState.open}
				on:click={() => void openPendingHitl()}
			>
				<span>
					<strong>{$pendingHitlCount}</strong>
					{$pendingHitlCount === 1 ? 'item is' : 'items are'} waiting on you
				</span>
				<span class="plan-pane__cta-arrow">Open Attention →</span>
			</button>
		{:else}
			<p class="plan-pane__empty">
				{planModeEnabled
					? 'No actions awaiting approval.'
					: 'Plan mode is off. Toggle plan-mode to gate the agent on each destructive action.'}
			</p>
		{/if}
	</section>
{/if}

<style>
	.plan-pane {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 10px 12px;
		margin-top: 8px;
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 6px;
	}

	.plan-pane__head {
		display: flex;
		align-items: baseline;
		gap: 8px;
	}

	.plan-pane__title {
		font-family: var(--font-primary);
		font-size: 11px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-primary, #1a1a1a);
	}

	.plan-pane__badge {
		font-family: var(--font-mono);
		font-size: 10px;
		padding: 1px 6px;
		border-radius: 999px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-muted, #888);
	}

	.plan-pane__badge--on {
		color: var(--accent-primary, #c2502a);
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.12));
		font-weight: 600;
	}

	.plan-pane__thread {
		margin-left: auto;
		font-family: var(--font-mono);
		font-size: 10.5px;
		color: var(--text-muted, #888);
	}

	.plan-pane__empty {
		margin: 0;
		font-size: 12px;
		color: var(--text-muted, #888);
	}

	.plan-pane__cta {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		padding: 8px 10px;
		background: var(--accent-primary-soft, color-mix(in srgb, var(--accent-primary) 12%, transparent));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 35%, transparent);
		border-radius: 5px;
		color: var(--text-primary, #1a1a1a);
		font-size: 12.5px;
		cursor: pointer;
		text-align: left;
		font-family: inherit;
		transition: background 120ms ease, transform 120ms ease;
	}

	.plan-pane__cta:hover {
		background: color-mix(in srgb, var(--accent-primary) 20%, transparent);
		transform: translateY(-1px);
	}

	.plan-pane__cta-arrow {
		color: var(--accent-primary, #c2502a);
		font-weight: 600;
		font-size: 11.5px;
		white-space: nowrap;
	}
</style>
