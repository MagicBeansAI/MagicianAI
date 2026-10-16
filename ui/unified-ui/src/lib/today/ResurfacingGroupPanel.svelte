<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	import AttentionActionabilityBadge from '$lib/attention/AttentionActionabilityBadge.svelte';
	import AttentionBanditDiagnostic from '$lib/attention/AttentionBanditDiagnostic.svelte';
	import AttentionRoutingDiagnostic from '$lib/attention/AttentionRoutingDiagnostic.svelte';
	import {
		attentionFeedbackAttribution,
		verifiedAttentionVisibility
	} from '$lib/attention/attentionVisibility';
	import {
		createPairCorrectionEventId,
		groupingAffordanceLabel,
		groupingMayCollapse,
		postAttentionPairCorrection,
		type AttentionPairCorrectionReceipt,
		type AttentionPairLabel
	} from '$lib/attention/attentionGrouping';
	import { feedbackReceiptMessage } from '$lib/channel/channelFollowUpLearning';
	import {
		fetchResurfacingGroupMembers,
		postResurfacingAction,
		type ResurfacingAction,
		type ResurfacingCard
	} from './resurfacingQueries';

	export let card: ResurfacingCard;

	const dispatch = createEventDispatcher<{
		groupchanged: { receipt?: AttentionPairCorrectionReceipt; candidateId?: string };
	}>();

	let expanded = false;
	let loading = false;
	let error: string | null = null;
	let members: ResurfacingCard[] = [];
	let memberTotal = 0;
	let busyId: string | null = null;
	let notice: string | null = null;

	$: metadata = card.grouping ?? null;
	$: groupingPage = card.grouping_page ?? null;
	$: affordance = metadata ? groupingAffordanceLabel(metadata, groupingPage) : null;
	$: representative = metadata
		? members.find((member) => member.candidate_id === metadata?.representative_id) ??
			(card.candidate_id === metadata.representative_id ? card : null)
		: null;
	$: orderedMembers = metadata
		? [...members].sort((left, right) => {
				const leftRep = left.candidate_id === metadata?.representative_id ? 1 : 0;
				const rightRep = right.candidate_id === metadata?.representative_id ? 1 : 0;
				return rightRep - leftRep || (right.temporal_anchor_at ?? 0) - (left.temporal_anchor_at ?? 0);
			})
		: members;
	$: expansionComplete =
		metadata !== null &&
		memberTotal === metadata.member_count &&
		members.length === memberTotal &&
		members.every(
			(member) =>
				member.candidate_id.length > 0 &&
				(member.source_revision === null ||
					(typeof member.source_revision === 'string' &&
						member.source_revision.trim().length > 0))
		);

	async function loadMembers(): Promise<void> {
		if (!metadata || loading) return;
		loading = true;
		error = null;
		try {
			const result = await fetchResurfacingGroupMembers(
				metadata.cluster_id,
				card.routing_page ?? null
			);
			members = result.items.map((member) => ({
				...member,
				...(groupingPage ? { grouping_page: groupingPage } : {})
			}));
			memberTotal = result.total;
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			loading = false;
		}
	}

	function toggle(): void {
		expanded = !expanded;
		if (expanded && members.length === 0) void loadMembers();
	}

	function correctionAvailable(member: ResurfacingCard): boolean {
		const representativeRevisionValid =
			representative?.source_revision === null ||
			(typeof representative?.source_revision === 'string' &&
				representative.source_revision.trim().length > 0);
		const memberRevisionValid =
			member.source_revision === null ||
			(typeof member.source_revision === 'string' && member.source_revision.trim().length > 0);
		return Boolean(
			representative?.candidate_id &&
			member.candidate_id !== representative.candidate_id &&
			representativeRevisionValid &&
			memberRevisionValid &&
			expansionComplete
		);
	}

	async function correct(member: ResurfacingCard, label: AttentionPairLabel): Promise<void> {
		if (!representative || !correctionAvailable(member)) return;
		busyId = `${member.candidate_id}:${label}`;
		error = null;
		const result = await postAttentionPairCorrection({
			event_id: createPairCorrectionEventId(),
			surface: 'worth_a_look',
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
		busyId = null;
		if (!result.ok) {
			error = result.error;
			return;
		}
		notice = `Pair correction recorded · generation ${result.receipt.grouping_generation} · ${result.receipt.affected_cluster_ids.length} affected cluster${result.receipt.affected_cluster_ids.length === 1 ? '' : 's'} reloaded.`;
		dispatch('groupchanged', { receipt: result.receipt });
	}

	async function act(member: ResurfacingCard, action: ResurfacingAction): Promise<void> {
		if (busyId) return;
		busyId = `${member.candidate_id}:${action}`;
		error = null;
		const attribution = attentionFeedbackAttribution(
			member.decision_item ?? null,
			member.routing_page?.impression_policy ?? null,
			'worth_a_look'
		);
		const result = await postResurfacingAction(
			member.candidate_id,
			action,
			undefined,
			attribution
		);
		busyId = null;
		if (!result.ok) {
			error = result.error;
			return;
		}
		const base =
			action === 'open' ? 'Marked useful' : action === 'acknowledge' ? 'Acknowledged' : 'Dismissed';
		notice = feedbackReceiptMessage(base, result.feedbackReceipt);
		dispatch('groupchanged', { candidateId: member.candidate_id });
	}

	function rankLabel(member: ResurfacingCard): string {
		if (member.baseline_rank == null && member.learned_rank == null) return 'Rank unavailable';
		return `Rank ${member.baseline_rank ?? '—'} baseline → ${member.learned_rank ?? '—'} learned`;
	}
</script>

{#if metadata && affordance}
	<section
		class="resurfacing-group"
		class:resurfacing-group--preview={!groupingMayCollapse(groupingPage)}
		aria-label="Related Worth-a-look updates"
	>
		{#if metadata.is_representative}
			<button
				type="button"
				class="resurfacing-group__toggle"
				aria-expanded={expanded}
				on:click={toggle}
			>
				<span aria-hidden="true">{expanded ? '▾' : '▸'}</span>
				{affordance}
				<span>
					· {groupingMayCollapse(groupingPage) ? 'representative shown' : 'current cards remain separate'}
				</span>
			</button>
		{:else if groupingPage?.mode === 'shadow'}
			<div class="resurfacing-group__member-preview">
				{affordance} · representative {metadata.representative_id}
			</div>
		{/if}

		{#if expanded && metadata.is_representative}
			<div class="resurfacing-group__expanded">
				<div class="resurfacing-group__audit">
					<span>Cluster {metadata.cluster_id}</span>
					<span>{metadata.model_version ? `Model ${metadata.model_version}` : 'Deterministic fallback'}</span>
					<span>{metadata.merge_probability == null ? 'Pair probability unavailable' : `${Math.round(metadata.merge_probability * 100)}% merge probability`}</span>
				</div>
				{#if loading}
					<p role="status">Loading every related update…</p>
				{:else if error}
					<p class="resurfacing-group__error" role="alert">{error}</p>
				{:else if members.length > 0}
					<p
						class:resurfacing-group__error={!expansionComplete}
						data-testid="resurfacing-group-reconciliation"
					>
						{expansionComplete
							? `All ${memberTotal} members loaded.`
							: `Member mismatch: expected ${metadata.member_count}, endpoint reported ${memberTotal}, rendered ${members.length}. Corrections are disabled.`}
					</p>
					{#each orderedMembers as member (member.candidate_id)}
					<article
						class="resurfacing-group__member"
						use:verifiedAttentionVisibility={{
							decision_item: member.decision_item ?? null,
							impression_policy: member.routing_page?.impression_policy ?? null,
							surface: 'worth_a_look'
						}}
					>
							<header>
								<div>
									<strong>{member.line || member.source_title || member.candidate_id}</strong>
									{#if member.candidate_id === metadata.representative_id}
										<span class="resurfacing-group__representative">Representative</span>
									{/if}
								</div>
								<span>{member.why_now || 'No timing reason'}</span>
							</header>
							<div class="resurfacing-group__metadata">
								<span>Source {member.source_kind || 'unknown'} · {member.source_ref || 'unknown'}</span>
								<span>Route {member.source_route ?? 'unavailable'}</span>
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
							<div class="resurfacing-group__actions">
								<button
									type="button"
									disabled={busyId !== null}
									on:click={() => act(member, 'open')}
								>
									Useful
								</button>
								<button
									type="button"
									disabled={busyId !== null}
									on:click={() => act(member, 'acknowledge')}
								>
									Acknowledge
								</button>
								<button
									type="button"
									disabled={busyId !== null}
									on:click={() => act(member, 'dismiss')}
								>
									Dismiss
								</button>
								{#if member.candidate_id !== metadata.representative_id}
									<span class="resurfacing-group__pair-label">Compared with representative</span>
									<button
										type="button"
										disabled={!correctionAvailable(member) || busyId !== null}
										aria-label={`Duplicate of this: ${member.line || member.candidate_id} versus representative`}
										on:click={() => correct(member, 'same_underlying_item')}
									>
										Duplicate of this
									</button>
									<button
										type="button"
										disabled={!correctionAvailable(member) || busyId !== null}
										aria-label={`Not duplicate: ${member.line || member.candidate_id} versus representative`}
										on:click={() => correct(member, 'not_duplicate')}
									>
										Not duplicate
									</button>
								{/if}
							</div>
						</article>
					{/each}
				{/if}
				{#if notice}<p class="resurfacing-group__notice" role="status">{notice}</p>{/if}
			</div>
		{/if}
	</section>
{/if}

<style>
	.resurfacing-group {
		margin-top: 0.45rem;
		border: 1px solid color-mix(in srgb, var(--color-success, #2f8f5b) 30%, var(--border-soft));
		border-radius: 8px;
		background: color-mix(in srgb, var(--color-success, #2f8f5b) 5%, var(--bg-card));
	}

	.resurfacing-group--preview {
		border-color: color-mix(in srgb, var(--color-info, #4d9de0) 28%, var(--border-soft));
		background: color-mix(in srgb, var(--color-info, #4d9de0) 4%, var(--bg-card));
	}

	.resurfacing-group__toggle {
		display: flex;
		align-items: center;
		gap: 0.3rem;
		width: 100%;
		padding: 0.45rem 0.55rem;
		border: 0;
		background: transparent;
		color: var(--text-secondary);
		font: inherit;
		font-size: 0.7rem;
		font-weight: 650;
		text-align: left;
		cursor: pointer;
	}

	.resurfacing-group__toggle span:last-child,
	.resurfacing-group__member-preview {
		color: var(--text-muted, var(--text-secondary));
		font-weight: 500;
	}

	.resurfacing-group__member-preview {
		padding: 0.4rem 0.55rem;
		font-size: 0.68rem;
	}

	.resurfacing-group__expanded {
		display: grid;
		gap: 0.5rem;
		padding: 0 0.55rem 0.55rem;
	}

	.resurfacing-group__audit,
	.resurfacing-group__metadata,
	.resurfacing-group__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.3rem 0.65rem;
		font-size: 0.66rem;
		color: var(--text-muted, var(--text-secondary));
	}

	.resurfacing-group__member {
		display: grid;
		gap: 0.4rem;
		padding: 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		background: var(--bg-card);
	}

	.resurfacing-group__member header {
		display: flex;
		justify-content: space-between;
		gap: 0.5rem;
		font-size: 0.7rem;
	}

	.resurfacing-group__representative {
		margin-left: 0.35rem;
		padding: 0.1rem 0.3rem;
		border-radius: 999px;
		background: var(--bg-soft);
		font-size: 0.62rem;
	}

	.resurfacing-group__actions button {
		padding: 0.25rem 0.4rem;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		background: var(--bg-soft);
		color: var(--text-secondary);
		font: inherit;
		font-weight: 600;
		cursor: pointer;
	}

	.resurfacing-group__actions button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.resurfacing-group__pair-label {
		padding-left: 0.35rem;
		border-left: 1px dashed var(--border-soft);
	}

	.resurfacing-group__expanded p {
		margin: 0;
		font-size: 0.68rem;
		color: var(--text-muted, var(--text-secondary));
	}

	.resurfacing-group__expanded .resurfacing-group__error {
		color: var(--color-warning, #9a6410);
	}

	.resurfacing-group__notice {
		color: var(--color-success, #2f8f5b) !important;
	}
</style>
