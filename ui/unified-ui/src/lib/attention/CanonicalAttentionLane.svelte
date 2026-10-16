<script lang="ts">
	import { createEventDispatcher, onDestroy } from 'svelte';

	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import ChannelFollowUpActions from '$lib/channel/ChannelFollowUpActions.svelte';
	import ResurfacingCardActions from '$lib/today/ResurfacingCardActions.svelte';
	import type { ResurfacingCard } from '$lib/today/resurfacingQueries';
	import {
		channelDueLabel,
		channelLabelText,
		channelProviderText,
		channelReceivedLabel,
		type ChannelFollowUp
	} from '$lib/stores/channelNeedsYouStore';
	import { resurfacingRowFacts } from '$lib/today/resurfacingPresentation';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { attentionDeliveryStore } from '$lib/stores/attentionDeliveryStore';
	import { executeCanonicalAttentionAction } from './canonicalAttentionActions';
	import {
		followUpAttentionMutationKey,
		optimisticAttentionMutationQueue,
		worthAttentionMutationKey
	} from './optimisticAttentionMutationQueue';
	import type { AttentionFeedbackAttribution } from './attentionBandit';
	import { attentionFeedbackAttribution } from './attentionVisibility';
	import {
		attentionDeliveryFeedbackAttribution,
		verifiedAttentionDeliveryVisibility
	} from './attentionDeliveryVisibility';
	import type {
		AttentionDeliveredItem,
		AttentionDeliveryPageResponse
	} from './attentionDelivery';
	import CanonicalProjectionDiagnostic from './CanonicalProjectionDiagnostic.svelte';
	import {
		canonicalAttentionLaneItems,
		canonicalItemDecisionItem,
		type CanonicalAttentionItem,
		type CanonicalAttentionOriginAction,
		type CanonicalAttentionOriginLane,
		type CanonicalAttentionProjection
	} from './canonicalAttentionProjection';

	export let projection: CanonicalAttentionProjection;
	export let lane: CanonicalAttentionOriginLane;
	export let page = 1;
	export let pageSize = 5;
	export let showEmpty = true;
	export let showDiagnostic = true;
	/**
	 * Legacy follow-up rows, joined by annotation id.
	 *
	 * The canonical projection owns the ordering and the delivery binding, but
	 * its payload does not carry `proposed_action`, `available_actions`, or the
	 * evidence identifiers that the rich controls need — so a canonical-only
	 * lane loses Show message, dismiss-with-reason, and the contextual actions.
	 * Joining here keeps canonical ordering while restoring the full control
	 * set; rows without a match fall back to the server-declared buttons.
	 */
	export let channelFollowUps: ChannelFollowUp[] = [];
	/**
	 * Legacy Worth-a-look cards, joined by candidate id.
	 *
	 * Same reason as the follow-up join: the canonical payload carries no
	 * capability list, so without the legacy card the lane can only offer the
	 * server-declared verbs and silently loses create reminder, create task,
	 * ask Presto, save to memory, and share draft.
	 */
	export let worthCards: ResurfacingCard[] = [];
	/**
	 * A lookup that supplies the rich control set is still loading. Rows whose
	 * origin has such a control hold their action space instead of rendering the
	 * reduced fallback that would be replaced moments later.
	 */
	export let richLookupPending = false;
	export let chatThreadId: string | null = null;

	$: followUpByAnnotationId = new Map(
		channelFollowUps.map((followUp) => [followUp.annotation_id, followUp])
	);
	$: worthByCandidateId = new Map(worthCards.map((card) => [card.candidate_id, card]));

	// Passed explicitly for the same reason as the follow-up map: template
	// dependencies are tracked syntactically, so a helper closing over the map
	// would not re-render when the join resolves after first paint.
	function richWorthCard(
		item: CanonicalAttentionItem,
		byCandidateId: Map<string, ResurfacingCard>
	): ResurfacingCard | null {
		return item.origin.kind === 'worth_a_look'
			? byCandidateId.get(item.origin.candidate_id) ?? null
			: null;
	}
	/** Both origins are served by a lookup, so both can be mid-join on first paint. */
	function awaitsRichControl(item: CanonicalAttentionItem): boolean {
		return item.origin.kind === 'follow_up' || item.origin.kind === 'worth_a_look';
	}
	// The lookup map is passed in explicitly rather than closed over: Svelte
	// tracks template dependencies syntactically, so a helper that reads the map
	// from scope would not re-render when the lookup resolves after first paint —
	// leaving every row on the reduced button set.
	function richFollowUp(
		item: CanonicalAttentionItem,
		byAnnotationId: Map<string, ChannelFollowUp>
	): ChannelFollowUp | null {
		return item.origin.kind === 'follow_up'
			? byAnnotationId.get(item.origin.annotation_id) ?? null
			: null;
	}

	const dispatch = createEventDispatcher<{
		pagechange: { page: number };
		resolved: {
			item: CanonicalAttentionItem;
			action: CanonicalAttentionOriginAction;
			message: string;
		};
		failed: { item: CanonicalAttentionItem; action: CanonicalAttentionOriginAction; error: string };
	}>();

	let busyKeys = new Set<string>();
	let resolvedIds = new Set<string>();
	let projectionIdentity = '';
	let resolvedScopeKey = '';
	let requestedDeliveryKey = '';
	const committedReleaseTimers = new Map<string, ReturnType<typeof setTimeout>>();

	type LaneEntry = {
		item: CanonicalAttentionItem;
		delivery: AttentionDeliveredItem | null;
		page: AttentionDeliveryPageResponse | null;
	};

	$: if (projectionIdentity !== projection.projection_id || resolvedScopeKey !== scopeKey) {
		const scopeChanged = resolvedScopeKey !== scopeKey;
		projectionIdentity = projection.projection_id;
		resolvedScopeKey = scopeKey;
		const currentIds = new Set(
			canonicalAttentionLaneItems(projection, lane).map((item) => item.canonical_id)
		);
		if (scopeChanged) {
			for (const timer of committedReleaseTimers.values()) clearTimeout(timer);
			committedReleaseTimers.clear();
			resolvedIds = new Set();
			busyKeys = new Set();
		} else {
			resolvedIds = new Set([...resolvedIds].filter((id) => currentIds.has(id)));
			busyKeys = new Set([...busyKeys].filter((key) =>
				[...currentIds].some((id) => key.startsWith(`${id}:`))
			));
		}
	}
	$: scopeKey = [$scopeIdentityStore.principal, $scopeIdentityStore.workspace].join('\u0000');
	$: deliveryState = $attentionDeliveryStore[lane];
	$: deliveryActive = Boolean(
		deliveryState.root &&
		deliveryState.projection?.projection_id === projection.projection_id &&
		deliveryState.projection?.universe_digest === projection.universe_digest &&
		deliveryState.scopeKey === scopeKey
	);
	$: deliveryRequestKey = [scopeKey, lane, projection.projection_id, projection.universe_digest].join('\u0000');
	$: if (typeof window !== 'undefined' && requestedDeliveryKey !== deliveryRequestKey) {
		requestedDeliveryKey = deliveryRequestKey;
		void attentionDeliveryStore.ensure(lane, scopeKey, projection, pageSize);
	}
	$: sourceItems = canonicalAttentionLaneItems(projection, lane);
	$: waitingForDelivery = !deliveryActive && !deliveryState.fallbackReason;
	function entriesFromDelivery(
		items: AttentionDeliveredItem[],
		pages: AttentionDeliveryPageResponse[]
	): LaneEntry[] {
		return items.map((delivery): LaneEntry => ({
			item: delivery.item,
			delivery,
			page: pages.find((candidate) =>
				candidate.page.page_index >= 0 &&
				delivery.position >= candidate.page.page_start + 1 &&
				delivery.position <= candidate.page.page_start + candidate.items.length
			) ?? null
		}));
	}
	$: sourceEntries = deliveryActive
		? entriesFromDelivery(deliveryState.items, deliveryState.pages)
		: waitingForDelivery
			? entriesFromDelivery(deliveryState.items, deliveryState.pages)
			: sourceItems.map((item): LaneEntry => ({ item, delivery: null, page: null }));
	// Preserve server order and routing. The only local omission is a card with
	// an accepted optimistic lifecycle mutation. This now applies equally to a
	// frozen delivery: the user action wins immediately, while failure removes
	// the tombstone and restores the exact server-owned position.
	$: entries = sourceEntries.filter((entry) =>
		!resolvedIds.has(entry.item.canonical_id) &&
		!$optimisticAttentionMutationQueue.statusByKey.has(mutationKey(entry.item))
	);
	$: safePageSize = Math.max(1, Math.floor(pageSize));
	// A frozen delivery loads incrementally, so the count it can page over is the
	// server's universe rather than what has arrived so far — otherwise the pager
	// would claim one page and grow a page at a time as the user reads. Items
	// hidden by an accepted optimistic mutation are discounted so the total does
	// not keep promising a row that was just acted on.
	$: hiddenCount = sourceEntries.length - entries.length;
	$: deliveryTotal = Math.max(
		entries.length,
		(deliveryState.root?.universe_size ?? 0) - hiddenCount
	);
	$: totalCount =
		deliveryActive && deliveryState.hasMore ? deliveryTotal : entries.length;
	$: pageCount = Math.max(1, Math.ceil(totalCount / safePageSize));
	$: safePage = Math.min(pageCount, Math.max(1, Math.floor(page)));
	$: pageStart = totalCount === 0 ? 0 : (safePage - 1) * safePageSize + 1;
	$: pageEnd = Math.min(totalCount, safePage * safePageSize);
	// Seeking past what the delivery has fetched pulls the next page in; the
	// statement re-runs as items arrive, so a jump to a far page walks forward
	// rather than silently showing an empty one.
	$: if (
		deliveryActive &&
		deliveryState.hasMore &&
		!deliveryState.isLoading &&
		safePage * safePageSize > entries.length
	) {
		void attentionDeliveryStore.loadMore(lane);
	}
	$: visibleEntries = entries.slice(pageStart === 0 ? 0 : pageStart - 1, pageEnd);

	function deliveryVisibility(node: HTMLElement, entry: LaneEntry) {
		let active: ReturnType<typeof verifiedAttentionDeliveryVisibility> | null = null;
		function configure(next: LaneEntry): void {
			if (!next.delivery || !next.page) {
				active?.destroy();
				active = null;
				return;
			}
			const options = {
				response: next.page,
				delivery: next.delivery,
				surface: lane,
				scope: {
					principal: $scopeIdentityStore.principal,
					workspace: $scopeIdentityStore.workspace
				}
			};
			if (active) active.update(options);
			else active = verifiedAttentionDeliveryVisibility(node, options);
		}
		configure(entry);
		return {
			update: configure,
			destroy: () => active?.destroy()
		};
	}

	function actionKey(item: CanonicalAttentionItem, action: CanonicalAttentionOriginAction): string {
		return `${item.canonical_id}:${action.id}`;
	}

	function mutationKey(item: CanonicalAttentionItem): string {
		const scope = {
			principal: $scopeIdentityStore.principal,
			workspace: $scopeIdentityStore.workspace
		};
		return item.origin_lane === 'follow_up'
			? followUpAttentionMutationKey(item.origin.annotation_id, scope)
			: worthAttentionMutationKey(item.origin.candidate_id, scope);
	}

	function suppress(itemId: string): void {
		resolvedIds = new Set(resolvedIds).add(itemId);
	}

	function restore(itemId: string): void {
		const next = new Set(resolvedIds);
		next.delete(itemId);
		resolvedIds = next;
	}

	function releaseCommittedSuppression(itemId: string): void {
		const prior = committedReleaseTimers.get(itemId);
		if (prior !== undefined) clearTimeout(prior);
		committedReleaseTimers.set(itemId, setTimeout(() => {
			committedReleaseTimers.delete(itemId);
			restore(itemId);
		}, 120_000));
	}

	function routeLabel(item: CanonicalAttentionItem): string | null {
		if (item.origin_lane === lane) return null;
		return item.origin_lane === 'follow_up' ? 'From Follow-up' : 'From Worth a look';
	}

	function title(item: CanonicalAttentionItem): string {
		if (item.origin_lane === 'follow_up') return item.payload.subject?.trim() || '(no subject)';
		return item.payload.line.trim() || item.payload.source_title.trim() || 'Worth a look';
	}

	function summary(item: CanonicalAttentionItem): string | null {
		if (item.origin_lane === 'follow_up') {
			return item.payload.summary?.trim() || item.payload.reason?.trim() || null;
		}
		return item.payload.summary.trim() || item.payload.why_now.trim() || null;
	}

	function context(item: CanonicalAttentionItem): string {
		if (item.origin_lane === 'follow_up') {
			return `${channelProviderText(item.origin.provider)} · ${channelLabelText(item.payload.label)}`;
		}
		return item.payload.source_kind.replace(/_/g, ' ').replace(/\b\w/g, (value) => value.toUpperCase());
	}

	function epochMs(value: number | null | undefined): number | null {
		if (value == null || !Number.isFinite(value) || value <= 0) return null;
		return value < 1e12 ? Math.round(value * 1000) : Math.round(value);
	}

	function clockLabel(value: number | null | undefined): string | null {
		const ms = epochMs(value);
		const label = channelReceivedLabel(ms);
		return label || null;
	}

	function isoDatetime(value: number | null | undefined): string | undefined {
		const ms = epochMs(value);
		if (ms == null) return undefined;
		try {
			return new Date(ms).toISOString();
		} catch {
			return undefined;
		}
	}

	interface LaneWhenBit {
		key: string;
		label: string;
		value: string;
		datetime?: string;
	}

	function whenBits(
		item: CanonicalAttentionItem,
		followUp: ChannelFollowUp | null,
		worth: ResurfacingCard | null
	): LaneWhenBit[] {
		const bits: LaneWhenBit[] = [];
		if (item.payload.kind === 'follow_up') {
			const received = clockLabel(followUp?.received_at ?? item.payload.received_at);
			if (received) {
				bits.push({
					key: 'received',
					label: 'Received',
					value: received,
					datetime: isoDatetime(followUp?.received_at ?? item.payload.received_at)
				});
			}
			const dueText = item.payload.due_text?.trim() || (followUp ? channelDueLabel(followUp) : null);
			const dueAt = clockLabel(item.payload.due_at);
			if (dueText) {
				bits.push({
					key: 'due',
					label: 'Due',
					value: dueAt && !dueText.includes(dueAt) ? `${dueText} · ${dueAt}` : dueText,
					datetime: isoDatetime(item.payload.due_at)
				});
			} else if (dueAt) {
				bits.push({ key: 'due', label: 'Due', value: dueAt, datetime: isoDatetime(item.payload.due_at) });
			}
			return bits;
		}
		const when = clockLabel(worth?.temporal_anchor_at ?? item.payload.temporal_anchor_at);
		if (when) {
			bits.push({
				key: 'when',
				label: 'When',
				value: when,
				datetime: isoDatetime(worth?.temporal_anchor_at ?? item.payload.temporal_anchor_at)
			});
		}
		for (const fact of resurfacingRowFacts(worth?.brief ?? null, 4).filter((row) => row.kind === 'date')) {
			if (bits.some((bit) => bit.value === fact.value)) continue;
			bits.push({ key: fact.id, label: fact.label, value: fact.value });
		}
		return bits;
	}

	function why(
		item: CanonicalAttentionItem,
		byCandidateId: Map<string, ResurfacingCard>
	): string | null {
		if (item.payload.kind === 'worth_a_look') {
			const joined = richWorthCard(item, byCandidateId)?.why_now?.trim();
			if (joined) return joined;
			return item.payload.why_now.trim() || null;
		}
		return item.payload.reason?.trim() || null;
	}

	function external(href: string): boolean {
		return /^https?:\/\//i.test(href);
	}

	async function runAction(
		entry: LaneEntry,
		action: CanonicalAttentionOriginAction
	): Promise<void> {
		const item = entry.item;
		if (action.method !== 'post') return;
		if (action.requires_confirmation && !window.confirm(`${action.label}?`)) return;
		const key = actionKey(item, action);
		const actionScopeKey = scopeKey;
		const queuedMutationKey = mutationKey(item);
		if (busyKeys.has(key) || optimisticAttentionMutationQueue.isSuppressed(queuedMutationKey)) return;
		busyKeys = new Set(busyKeys).add(key);
		// Suppress before the network promise is even queued so the interaction
		// never waits for transport or server latency to feel complete.
		suppress(item.canonical_id);
		let attribution: AttentionFeedbackAttribution | null = entry.delivery && entry.page
			? attentionDeliveryFeedbackAttribution(entry.page, entry.delivery, lane, {
					principal: $scopeIdentityStore.principal,
					workspace: $scopeIdentityStore.workspace
			  })
			: null;
		if (!attribution) {
			const projected = canonicalItemDecisionItem(item, projection.diagnostics);
			const followUp = item.origin.kind === 'follow_up'
				? richFollowUp(item, followUpByAnnotationId)
				: null;
			const worth = item.origin.kind === 'worth_a_look'
				? richWorthCard(item, worthByCandidateId)
				: null;
			const joined = followUp?.decision_item ?? worth?.decision_item ?? null;
			const policy =
				followUp?.routing_page?.impression_policy ??
				worth?.routing_page?.impression_policy ??
				projection.diagnostics?.routing?.impression_policy ??
				null;
			attribution = attentionFeedbackAttribution(
				projected ?? joined,
				policy,
				item.origin.kind === 'follow_up' ? 'follow_up' : 'worth_a_look'
			);
		}
		const result = await optimisticAttentionMutationQueue.enqueue(
			queuedMutationKey,
			() => executeCanonicalAttentionAction(item, action, undefined, attribution)
		);
		if (scopeKey !== actionScopeKey) return;
		const nextBusy = new Set(busyKeys);
		nextBusy.delete(key);
		busyKeys = nextBusy;
		if (!result.ok) {
			restore(item.canonical_id);
			dispatch('failed', { item, action, error: result.error });
			return;
		}
		releaseCommittedSuppression(item.canonical_id);
		dispatch('resolved', { item, action, message: result.message });
	}

	function followUpKind(
		action: string
	): CanonicalAttentionOriginAction['kind'] | null {
		if (
			action === 'useful' ||
			action === 'approve' ||
			action === 'acknowledge' ||
			action === 'dismiss' ||
			action === 'snooze'
		) {
			return action;
		}
		return null;
	}

	function onJoinedFollowUpResolved(
		item: CanonicalAttentionItem,
		event: CustomEvent<{ action: string; message: string }>
	): void {
		const kind = followUpKind(event.detail.action);
		const action =
			(kind ? item.actions.find((candidate) => candidate.kind === kind) : null) ??
			item.actions.find((candidate) => candidate.method === 'post') ??
			null;
		if (!action) return;
		suppress(item.canonical_id);
		releaseCommittedSuppression(item.canonical_id);
		dispatch('resolved', { item, action, message: event.detail.message });
	}

	function onJoinedFollowUpFailed(
		item: CanonicalAttentionItem,
		event: CustomEvent<{ action: string; error: string }>
	): void {
		const kind = followUpKind(event.detail.action);
		const action =
			(kind ? item.actions.find((candidate) => candidate.kind === kind) : null) ??
			item.actions.find((candidate) => candidate.method === 'post') ??
			null;
		if (!action) return;
		restore(item.canonical_id);
		dispatch('failed', { item, action, error: event.detail.error });
	}

	function goToPage(next: number): void {
		const target = Math.min(pageCount, Math.max(1, Math.floor(next)));
		if (target !== safePage) dispatch('pagechange', { page: target });
	}

	onDestroy(() => {
		for (const timer of committedReleaseTimers.values()) clearTimeout(timer);
		committedReleaseTimers.clear();
	});
</script>

{#if showDiagnostic}
	<CanonicalProjectionDiagnostic {projection} />
{/if}

{#if waitingForDelivery && entries.length === 0}
	<div class="canonical-lane__empty" role="status" data-testid="canonical-lane-loading">
		<strong>{lane === 'follow_up' ? 'Loading follow-ups…' : 'Loading Worth a look…'}</strong>
		<span>Keeping the ranked page, not the unfiltered list.</span>
	</div>
{:else if entries.length === 0}
	{#if showEmpty}
		<div class="canonical-lane__empty" role="status">
			<strong>{lane === 'follow_up' ? 'No canonical follow-ups.' : 'Nothing worth a look right now.'}</strong>
			<span>The exact union is complete; no items were assigned to this lane.</span>
		</div>
	{/if}
{:else}
	{#if pageCount > 1}
		<ServerPager
			currentPage={safePage}
			{pageCount}
			startItem={pageStart}
			endItem={pageEnd}
			totalItems={totalCount}
			loading={busyKeys.size > 0}
			ariaLabel={`${lane === 'follow_up' ? 'Follow-up' : 'Worth a look'} canonical pagination`}
			on:pagechange={(event) => goToPage(event.detail.page)}
		/>
	{/if}
	<ul class="canonical-lane" aria-label={lane === 'follow_up' ? 'Follow-ups' : 'Worth a look'}>
		{#each visibleEntries as entry (entry.item.canonical_id)}
			{@const item = entry.item}
			{@const followUp = richFollowUp(item, followUpByAnnotationId)}
			{@const worth = richWorthCard(item, worthByCandidateId)}
			{@const when = whenBits(item, followUp, worth)}
			<li
				class="canonical-lane__row"
				data-origin-lane={item.origin_lane}
				data-served-lane={item.served_lane}
				data-delivery-id={entry.page?.page.delivery_id}
				data-delivered-position={entry.delivery?.position}
				use:deliveryVisibility={entry}
			>
				<div class="canonical-lane__body">
					<div class="canonical-lane__eyebrow">
						<span>{context(item)}</span>
						{#if routeLabel(item)}
							<span class="canonical-lane__route">{routeLabel(item)}</span>
						{/if}
						{#if item.group.member_count > 1}
							<span title={item.group.cluster_id}>{item.group.member_count} related</span>
						{/if}
						{#if when.length}
							<span class="canonical-lane__when-list">
								{#each when as bit (bit.key)}
									{#if bit.datetime}
										<time class="canonical-lane__when" datetime={bit.datetime} title={`${bit.label}: ${bit.value}`}>
											{bit.label} {bit.value}
										</time>
									{:else}
										<span class="canonical-lane__when" title={`${bit.label}: ${bit.value}`}>
											{bit.label} {bit.value}
										</span>
									{/if}
								{/each}
							</span>
						{/if}
					</div>
					<strong class="canonical-lane__title">{title(item)}</strong>
					{#if summary(item)}<p>{summary(item)}</p>{/if}
					{#if why(item, worthByCandidateId)}<small>{why(item, worthByCandidateId)}</small>{/if}
				</div>
				<div class="canonical-lane__actions">
					<!--
						The two rich controls sit at different levels and must not be
						treated alike. `ChannelFollowUpActions` is a complete control set
						(Useful, Acknowledge, Dismiss with reasons, Show message), so it
						substitutes for the server-declared verbs. `ResurfacingCardActions`
						is only the overflow menu, so it SUPPLEMENTS them — rendering it in
						place of `item.actions` drops Useful/Acknowledge/Dismiss from every
						Worth-a-look row while still looking like it worked.
					-->
					{#if followUp}
						<ChannelFollowUpActions
							{followUp}
							compact
							on:resolved={(event) => onJoinedFollowUpResolved(item, event)}
							on:failed={(event) => onJoinedFollowUpFailed(item, event)}
						/>
					{:else if richLookupPending && awaitsRichControl(item)}
						<!--
							The lookup that carries this row's control set is still in
							flight. Rendering the fallback now only to swap it a moment
							later reads as the buttons changing shape under the cursor,
							so the row waits. On a cold backend that wait reaches tens of
							seconds, which an empty column would read as "no actions" —
							hence a visible skeleton rather than blank space.
						-->
						<span class="canonical-lane__actions-pending" role="status" aria-label="Loading actions">
							<span class="canonical-lane__actions-skeleton" aria-hidden="true"></span>
							<span class="canonical-lane__actions-skeleton" aria-hidden="true"></span>
							<span class="canonical-lane__actions-skeleton" aria-hidden="true"></span>
						</span>
					{:else}
					{#each item.actions as action (action.id)}
						{#if action.method === 'get'}
							<a
								class="canonical-lane__action"
								class:canonical-lane__action--compact={worth}
								class:canonical-lane__action--primary={!worth}
								href={action.href}
								target={external(action.href) ? '_blank' : undefined}
								rel={external(action.href) ? 'noopener noreferrer' : undefined}
							>
								{action.label} <Icon name="arrow-up-right" size={12} />
							</a>
						{:else}
							<button
								type="button"
								class="canonical-lane__action"
								class:canonical-lane__action--compact={worth}
								class:canonical-lane__action--primary={!worth && (action.kind === 'approve' || action.kind === 'useful')}
								disabled={busyKeys.has(actionKey(item, action))}
								on:click={() => void runAction(entry, action)}
							>
								{busyKeys.has(actionKey(item, action)) ? 'Working…' : action.label}
							</button>
						{/if}
					{/each}
					{#if worth}
						<ResurfacingCardActions
							card={worth}
							{chatThreadId}
							compact
							on:refresh
							on:read
						/>
					{/if}
					{/if}
				</div>
			</li>
		{/each}
	</ul>
	<!--
		A frozen delivery used to swap the pager for a Load-more button, which
		removed page controls entirely — and rendered nothing at all once
		hasMore went false. Both modes now page; delivery just fetches the next
		slice on demand.
	-->
	{#if pageCount > 1}
		<ServerPager
			currentPage={safePage}
			{pageCount}
			startItem={pageStart}
			endItem={pageEnd}
			totalItems={totalCount}
			loading={busyKeys.size > 0 || (deliveryActive && deliveryState.isLoading)}
			ariaLabel={`${lane === 'follow_up' ? 'Follow-up' : 'Worth a look'} canonical pagination`}
			on:pagechange={(event) => goToPage(event.detail.page)}
		/>
	{/if}
{/if}

<style>
	.canonical-lane {
		display: grid;
		gap: 0;
		margin: 0;
		padding: 0;
		list-style: none;
		border: 1px solid var(--border-soft);
		border-radius: 10px;
		overflow: hidden;
		background: var(--bg-card);
	}

	.canonical-lane__row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 1rem;
		align-items: center;
		padding: 0.9rem 1rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.canonical-lane__row:last-child { border-bottom: 0; }
	.canonical-lane__body { min-width: 0; }
	.canonical-lane__body p { margin: 0.3rem 0 0; color: var(--text-secondary); }
	.canonical-lane__body small { display: block; margin-top: 0.3rem; color: var(--text-tertiary); }

	.canonical-lane__eyebrow {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		margin-bottom: 0.25rem;
		color: var(--text-tertiary);
		font-size: 0.72rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.canonical-lane__route {
		padding: 0.05rem 0.35rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--color-info, #4d9de0) 15%, transparent);
		color: var(--color-info, #4d9de0);
	}

	.canonical-lane__when-list {
		display: inline-flex;
		flex-wrap: wrap;
		gap: 0.55rem;
		margin-left: auto;
		text-transform: none;
		letter-spacing: 0;
	}

	.canonical-lane__when {
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}

	.canonical-lane__title { color: var(--text-primary); }

	.canonical-lane__actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.4rem;
		max-width: 24rem;
	}

	.canonical-lane__action {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.4rem 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		background: var(--bg-card);
		color: var(--text-primary);
		font: inherit;
		font-size: 0.78rem;
		text-decoration: none;
		cursor: pointer;
	}

	/* Matches the Worth-a-look band's feedback density, so a row keeps the same
	   weight whichever surface renders it. */
	.canonical-lane__action--compact {
		padding: 0.18rem 0.42rem;
		font-size: 0.72rem;
		border-radius: 5px;
		color: var(--text-secondary);
	}

	.canonical-lane__action--compact:hover:not(:disabled) {
		color: var(--text-primary);
		background: var(--bg-soft);
	}

	/* Holds the action column while the lookup that owns this row's controls is
	   still in flight; sized to the compact row so nothing shifts on arrival. */
	.canonical-lane__actions-pending {
		display: inline-flex;
		gap: 0.25rem;
		min-height: 1.55rem;
		align-items: center;
	}

	.canonical-lane__actions-skeleton {
		display: inline-block;
		width: 3.1rem;
		height: 1.55rem;
		border-radius: 5px;
		background: var(--bg-soft);
		opacity: 0.75;
		animation: canonical-lane-pulse 1.4s ease-in-out infinite;
	}

	.canonical-lane__actions-skeleton:nth-child(2) { width: 4.4rem; animation-delay: 0.15s; }
	.canonical-lane__actions-skeleton:nth-child(3) { width: 3.6rem; animation-delay: 0.3s; }

	@keyframes canonical-lane-pulse {
		0%, 100% { opacity: 0.45; }
		50% { opacity: 0.85; }
	}

	@media (prefers-reduced-motion: reduce) {
		.canonical-lane__actions-skeleton { animation: none; }
	}

	.canonical-lane__action--primary {
		border-color: color-mix(in srgb, var(--accent-primary) 45%, var(--border-soft));
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card));
	}

	.canonical-lane__action:disabled { cursor: wait; opacity: 0.55; }

	.canonical-lane__empty {
		display: grid;
		gap: 0.2rem;
		padding: 1rem;
		border: 1px dashed var(--border-soft);
		border-radius: 10px;
		color: var(--text-secondary);
	}

	.canonical-lane__empty strong { color: var(--text-primary); }


	@media (max-width: 760px) {
		.canonical-lane__row { grid-template-columns: 1fr; }
		.canonical-lane__actions { justify-content: flex-start; max-width: none; }
	}
</style>
