<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	import AttentionActionabilityBadge from '$lib/attention/AttentionActionabilityBadge.svelte';
	import AttentionBanditDiagnostic from '$lib/attention/AttentionBanditDiagnostic.svelte';
	import AttentionRoutingDiagnostic from '$lib/attention/AttentionRoutingDiagnostic.svelte';
	import {
		followUpAttentionMutationKey,
		optimisticAttentionMutationQueue
	} from '$lib/attention/optimisticAttentionMutationQueue';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import type { AttentionFeedbackReceipt } from '$lib/channel/channelFollowUpLearning';
	import {
		createPairCorrectionEventId,
		groupingAffordanceLabel,
		groupingMayCollapse,
		postAttentionPairCorrection,
		type AttentionPairCorrectionReceipt,
		type AttentionPairLabel
	} from '$lib/attention/attentionGrouping';
	import {
		channelLabelText,
		channelLaneText,
		channelProviderText,
		fetchChannelFollowUpGroupMembers,
		type ChannelFollowUp
	} from '$lib/stores/channelNeedsYouStore';
	import ChannelFollowUpActions from './ChannelFollowUpActions.svelte';

	export let followUp: ChannelFollowUp;

	const dispatch = createEventDispatcher<{
		resolved: { id: string; message: string; feedbackReceipt: AttentionFeedbackReceipt | null };
		failed: { id: string; error: string };
		groupchanged: { receipt: AttentionPairCorrectionReceipt };
	}>();

	let expanded = false;
	let loading = false;
	let error: string | null = null;
	let members: ChannelFollowUp[] = [];
	let memberTotal = 0;
	let correctionBusyId: string | null = null;
	let correctionReceipt: AttentionPairCorrectionReceipt | null = null;

	$: metadata = followUp.grouping ?? null;
	$: groupingPage = followUp.grouping_page ?? null;
	$: affordance = metadata ? groupingAffordanceLabel(metadata, groupingPage) : null;
	$: representative = metadata
		? members.find((member) => member.candidate_id === metadata?.representative_id) ??
			(followUp.candidate_id === metadata.representative_id ? followUp : null)
		: null;
	$: orderedMembers = (metadata
		? [...members].sort((left, right) => {
				const leftRep = left.candidate_id === metadata?.representative_id ? 1 : 0;
				const rightRep = right.candidate_id === metadata?.representative_id ? 1 : 0;
				return rightRep - leftRep || (right.received_at ?? 0) - (left.received_at ?? 0);
		  })
		: members
	).filter((member) =>
		!$optimisticAttentionMutationQueue.statusByKey.has(
			followUpAttentionMutationKey(member.annotation_id, $scopeIdentityStore)
		)
	);
	$: expansionComplete =
		metadata !== null &&
		memberTotal === metadata.member_count &&
		members.length === memberTotal &&
		members.every(
			(member) =>
				typeof member.candidate_id === 'string' &&
				member.candidate_id.length > 0 &&
				(member.source_revision === null ||
					(typeof member.source_revision === 'string' &&
						member.source_revision.trim().length > 0))
		);

	async function loadMembers(): Promise<void> {
		if (!metadata || loading) return;
		loading = true;
		error = null;
		const result = await fetchChannelFollowUpGroupMembers(
			metadata.cluster_id,
			followUp.semantic_ranking_enabled === true,
			groupingPage,
			followUp.routing_page ?? null
		);
		loading = false;
		if (!result.ok) {
			error = result.error ?? 'Could not load every related update.';
			return;
		}
		members = result.items;
		memberTotal = result.total;
	}

	function toggle(): void {
		expanded = !expanded;
		if (expanded && members.length === 0) void loadMembers();
	}

	function rankLabel(member: ChannelFollowUp): string {
		if (member.baseline_rank == null && member.learned_rank == null) return 'Rank unavailable';
		return `Rank ${member.baseline_rank ?? '—'} baseline → ${member.learned_rank ?? '—'} learned`;
	}

	function correctionAvailable(member: ChannelFollowUp): boolean {
		const representativeRevisionValid =
			representative?.source_revision === null ||
			(typeof representative?.source_revision === 'string' &&
				representative.source_revision.trim().length > 0);
		const memberRevisionValid =
			member.source_revision === null ||
			(typeof member.source_revision === 'string' && member.source_revision.trim().length > 0);
		return Boolean(
			representative?.candidate_id &&
			member.candidate_id &&
			member.candidate_id !== representative.candidate_id &&
			representativeRevisionValid &&
			memberRevisionValid &&
			expansionComplete
		);
	}

	async function correct(member: ChannelFollowUp, label: AttentionPairLabel): Promise<void> {
		if (!representative?.candidate_id || !member.candidate_id || !correctionAvailable(member)) {
			return;
		}
		correctionBusyId = `${member.candidate_id}:${label}`;
		error = null;
		const result = await postAttentionPairCorrection({
			event_id: createPairCorrectionEventId(),
			surface: 'follow_up',
			left: {
				candidate_id: representative.candidate_id,
				source_revision: representative.source_revision ?? null
			},
			right: {
				candidate_id: member.candidate_id,
				source_revision: member.source_revision ?? null
			},
			label
		});
		correctionBusyId = null;
		if (!result.ok) {
			error = result.error;
			return;
		}
		correctionReceipt = result.receipt;
		dispatch('groupchanged', { receipt: result.receipt });
	}
</script>

{#if metadata && affordance}
	<section
		class="follow-up-group"
		class:follow-up-group--preview={!groupingMayCollapse(groupingPage)}
		aria-label="Related Follow-up updates"
	>
		{#if metadata.is_representative}
			<button
				type="button"
				class="follow-up-group__toggle"
				aria-expanded={expanded}
				on:click={toggle}
			>
				<span aria-hidden="true">{expanded ? '▾' : '▸'}</span>
				{affordance}
				{#if groupingMayCollapse(groupingPage)}
					<span>· representative shown</span>
				{:else}
					<span>· current cards remain separate</span>
				{/if}
			</button>
		{:else if groupingPage?.mode === 'shadow'}
			<div class="follow-up-group__member-preview">
				{affordance} · representative {metadata.representative_id}
			</div>
		{/if}

		{#if expanded && metadata.is_representative}
			<div class="follow-up-group__expanded">
				<div class="follow-up-group__audit">
					<span>Cluster {metadata.cluster_id}</span>
					<span>{metadata.model_version ? `Model ${metadata.model_version}` : 'Deterministic fallback'}</span>
					<span>{metadata.merge_probability == null ? 'Pair probability unavailable' : `${Math.round(metadata.merge_probability * 100)}% merge probability`}</span>
				</div>
				{#if loading}
					<p role="status">Loading every related update…</p>
				{:else if error}
					<p class="follow-up-group__error" role="alert">{error}</p>
				{:else if members.length > 0}
					<p
						class:follow-up-group__error={!expansionComplete}
						data-testid="follow-up-group-reconciliation"
					>
						{expansionComplete
							? `All ${memberTotal} members loaded.`
							: `Member mismatch: expected ${metadata.member_count}, endpoint reported ${memberTotal}, rendered ${members.length}. Corrections are disabled.`}
					</p>
					{#each orderedMembers as member (member.annotation_id)}
					<article class="follow-up-group__member">
							<header>
								<div>
									<strong>{member.subject || member.sender || '(no subject)'}</strong>
									{#if member.candidate_id === metadata.representative_id}
										<span class="follow-up-group__representative">Representative</span>
									{/if}
								</div>
								<span>{member.sender || 'Unknown sender'}</span>
							</header>
							<div class="follow-up-group__metadata">
								<span>Source {channelProviderText(member.provider)} · {member.account_alias}</span>
								<span>Route {channelLaneText(member.lane)} · {channelLabelText(member.label)}</span>
								<span>{rankLabel(member)}</span>
								<span title={member.source_revision ?? 'No source revision'}>
									Revision {member.source_revision ?? 'unavailable'}
								</span>
								{#if member.grouping?.model_version}
									<span>Group model {member.grouping.model_version}</span>
								{/if}
								{#if member.open_url}
									<a href={member.open_url} target="_blank" rel="noreferrer">Open source</a>
								{/if}
							</div>
							<AttentionActionabilityBadge metadata={member.actionability ?? null} />
							<AttentionRoutingDiagnostic
								item={member.decision_item ?? null}
								page={member.routing_page ?? null}
							/>
							<AttentionBanditDiagnostic decision={member.bandit_decision ?? null} />
							<div class="follow-up-group__actions">
								<ChannelFollowUpActions
									followUp={member}
									compact
									on:resolved={(event) => dispatch('resolved', event.detail)}
									on:failed={(event) => dispatch('failed', event.detail)}
								/>
								{#if member.candidate_id !== metadata.representative_id}
									<div class="follow-up-group__pair-actions">
										<span>Compared with representative</span>
										<button
											type="button"
											disabled={!correctionAvailable(member) || correctionBusyId !== null}
											aria-label={`Duplicate of this: ${member.subject || member.candidate_id} versus representative`}
											on:click={() => correct(member, 'same_underlying_item')}
										>
											{correctionBusyId === `${member.candidate_id}:same_underlying_item` ? 'Saving…' : 'Duplicate of this'}
										</button>
										<button
											type="button"
											disabled={!correctionAvailable(member) || correctionBusyId !== null}
											aria-label={`Not duplicate: ${member.subject || member.candidate_id} versus representative`}
											on:click={() => correct(member, 'not_duplicate')}
										>
											{correctionBusyId === `${member.candidate_id}:not_duplicate` ? 'Saving…' : 'Not duplicate'}
										</button>
									</div>
								{/if}
							</div>
						</article>
					{/each}
				{/if}
				{#if correctionReceipt}
					<p class="follow-up-group__receipt" role="status">
						Pair correction recorded · generation {correctionReceipt.grouping_generation} · {correctionReceipt.affected_cluster_ids.length} affected cluster{correctionReceipt.affected_cluster_ids.length === 1 ? '' : 's'} reloaded.
					</p>
				{/if}
			</div>
		{/if}
	</section>
{/if}

<style>
	.follow-up-group {
		flex: 1 0 100%;
		width: 100%;
		min-width: 0;
		border: 1px solid color-mix(in srgb, var(--color-success, #2f8f5b) 30%, var(--border-soft));
		border-radius: 9px;
		background: color-mix(in srgb, var(--color-success, #2f8f5b) 5%, var(--bg-card));
	}

	.follow-up-group--preview {
		border-color: color-mix(in srgb, var(--color-info, #4d9de0) 28%, var(--border-soft));
		background: color-mix(in srgb, var(--color-info, #4d9de0) 4%, var(--bg-card));
	}

	.follow-up-group__toggle {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		width: 100%;
		padding: 0.5rem 0.65rem;
		border: 0;
		background: transparent;
		color: var(--text-secondary);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 650;
		text-align: left;
		cursor: pointer;
	}

	.follow-up-group__toggle span:last-child,
	.follow-up-group__member-preview {
		color: var(--text-muted, var(--text-secondary));
		font-weight: 500;
	}

	.follow-up-group__member-preview {
		padding: 0.45rem 0.65rem;
		font-size: 0.74rem;
	}

	.follow-up-group__expanded {
		display: grid;
		gap: 0.55rem;
		padding: 0 0.65rem 0.65rem;
	}

	.follow-up-group__audit,
	.follow-up-group__metadata {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem 0.75rem;
		color: var(--text-muted, var(--text-secondary));
		font-size: 0.7rem;
	}

	.follow-up-group__member {
		display: grid;
		gap: 0.45rem;
		padding: 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
	}

	.follow-up-group__member header {
		display: flex;
		justify-content: space-between;
		gap: 0.75rem;
		color: var(--text-secondary);
		font-size: 0.76rem;
	}

	.follow-up-group__member header strong {
		color: var(--text-primary);
	}

	.follow-up-group__representative {
		margin-left: 0.45rem;
		padding: 0.12rem 0.35rem;
		border-radius: 999px;
		background: var(--bg-soft);
		font-size: 0.66rem;
	}

	.follow-up-group__actions,
	.follow-up-group__pair-actions {
		display: flex;
		align-items: flex-start;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.follow-up-group__pair-actions {
		align-items: center;
		padding-top: 0.35rem;
		border-top: 1px dashed var(--border-soft);
		font-size: 0.7rem;
	}

	.follow-up-group__pair-actions button {
		padding: 0.28rem 0.48rem;
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		background: var(--bg-soft);
		color: var(--text-secondary);
		font: inherit;
		font-weight: 600;
		cursor: pointer;
	}

	.follow-up-group__pair-actions button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.follow-up-group__expanded p {
		margin: 0;
		font-size: 0.72rem;
		color: var(--text-muted, var(--text-secondary));
	}

	.follow-up-group__expanded .follow-up-group__error {
		color: var(--color-warning, #9a6410);
	}

	.follow-up-group__receipt {
		color: var(--color-success, #2f8f5b) !important;
	}
</style>
