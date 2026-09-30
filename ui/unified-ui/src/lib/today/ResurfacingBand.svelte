<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { createEventDispatcher, onDestroy, tick } from 'svelte';

	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import AttentionActionabilityBadge from '$lib/attention/AttentionActionabilityBadge.svelte';
	import AttentionLearningHealthStrip from '$lib/attention/AttentionLearningHealthStrip.svelte';
	import AttentionBanditDiagnostic from '$lib/attention/AttentionBanditDiagnostic.svelte';
	import AttentionRoutingDiagnostic from '$lib/attention/AttentionRoutingDiagnostic.svelte';
	import {
		attentionFeedbackAttribution,
		verifiedAttentionVisibility
	} from '$lib/attention/attentionVisibility';
	import type { AttentionBanditHealth } from '$lib/attention/attentionBandit';
	import type { AttentionSemanticExtractionHealth } from '$lib/attention/attentionSemanticExtraction';
	import {
		optimisticAttentionMutationQueue,
		worthAttentionMutationKey
	} from '$lib/attention/optimisticAttentionMutationQueue';
	import {
		followUpRankDeltaLabel,
		parseChannelFollowUpLearningRank,
		type AttentionActionabilityPage,
		type AttentionActionabilityTrainingStatus,
		type ChannelFollowUpLearningHealth
	} from '$lib/channel/channelFollowUpLearning';
	import type { AttentionGroupingPage } from '$lib/attention/attentionGrouping';
	import type { AttentionRoutingPage } from '$lib/attention/attentionRouting';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';
	import { showError, showInfo, showSuccess, showWarning } from '$lib/shared/stores/notifications';
	import {
		OVERLAY_PRIORITIES,
		release,
		requestFocus
	} from '$lib/shell/overlayCoordinator';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import ResurfacingActionDialog from './ResurfacingActionDialog.svelte';
	import { presentResurfacingActionResult } from './resurfacingActionResult';
	import ResurfacingDetailPanel from './ResurfacingDetailPanel.svelte';
	import ResurfacingGroupPanel from './ResurfacingGroupPanel.svelte';
	import {
		actionLabel,
		fetchResurfacingDetail,
		fetchResurfacingOriginal,
		fetchResurfacingTodayPage,
		postResurfacingAction,
		postResurfacingContextualAction,
		postResurfacingRecommendationEvent,
		ResurfacingApiError,
		type ResurfacingAction,
		type ResurfacingActionKind,
		type ResurfacingCard,
		type ResurfacingDismissReason,
		type ResurfacingContextualActionResponse,
		type ResurfacingContextualActionResult,
		type ResurfacingCursor,
		type ResurfacingDetail,
		type ResurfacingPage,
		type ResurfacingCrossLaneReconciliationSucceeded
	} from './resurfacingQueries';
	import {
		isResurfacingDialogAction,
		mergeResurfacingCapabilities,
		resurfacingBrief,
		resurfacingConcreteSummary,
		resurfacingDisplayTitle,
		resurfacingHasMissingDetails,
		resurfacingPrimaryAction,
		resurfacingRowFacts,
		type ResurfacingPrimaryAction,
		type ResurfacingDialogActionKind
	} from './resurfacingPresentation';

	export let showEmpty = false;
	export let page = 1;
	/** Existing owner chat thread used by Ask Presto and action provenance. */
	export let chatThreadId: string | null = null;

	const PAGE_SIZE = 5;
	const ACTION_DIALOG_OVERLAY_ID = 'resurfacing-action-dialog';
	const dispatch = createEventDispatcher<{
		countchange: { total: number };
		pagechange: { page: number };
	}>();

	let cards: ResurfacingCard[] = [];
	let visibleCards: ResurfacingCard[] = [];
	let total = 0;
	let hasMore = false;
	let pageOffset = 0;
	let nextCursor: ResurfacingCursor | null = null;
	let cursorsByPage = new Map<number, ResurfacingCursor | null>([[1, null]]);
	let loadingPage = false;
	let loadError: string | null = null;
	let learningHealth: ChannelFollowUpLearningHealth | null = null;
	let actionability: AttentionActionabilityPage | null = null;
	let actionabilityTraining: AttentionActionabilityTrainingStatus | null = null;
	let grouping: AttentionGroupingPage | null = null;
	let routing: AttentionRoutingPage | null = null;
	let bandit: AttentionBanditHealth | null = null;
	let semanticExtraction: AttentionSemanticExtractionHealth | null = null;
	let crossLaneReconciliation: ResurfacingCrossLaneReconciliationSucceeded | null = null;
	let verifiedScopeKey = '';
	let semanticRankingEnabled = false;
	let loadedScopeKey = '';
	let loadedPage = 0;
	let renderedPage = 0;
	let requestId = 0;
	let scopeGeneration = 0;
	let busyIds = new Set<string>();

	let detailCard: ResurfacingCard | null = null;
	let detail: ResurfacingDetail | null = null;
	let detailLoading = false;
	let detailError: string | null = null;
	let detailRequestId = 0;
	let originalRequestId = 0;
	let originalLoading = false;
	let deeperSummaries = new Map<
		string,
		Extract<ResurfacingContextualActionResult, { kind: 'deeper_summary' }>
	>();
	let actionRetryKeys = new Map<string, string>();
	let presentedRecommendations = new Set<string>();

	let operationBusy: { candidateId: string; kind: ResurfacingActionKind } | null = null;
	let operationRequestId = 0;
	let menuCardId: string | null = null;
	let menuEl: HTMLDivElement | null = null;
	let menuTrigger: HTMLButtonElement | null = null;
	let menuTop = 0;
	let menuRight = 8;

	let dialogCard: ResurfacingCard | null = null;
	let dialogDetail: ResurfacingDetail | null = null;
	let dialogAction: ResurfacingDialogActionKind | null = null;
	let dialogError: string | null = null;
	let dialogIdempotencyKey = '';

	interface ScopeGuard {
		scopeKey: string;
		generation: number;
	}

	function captureScopeGuard(): ScopeGuard {
		return { scopeKey, generation: scopeGeneration };
	}

	function scopeGuardIsCurrent(guard: ScopeGuard): boolean {
		return guard.scopeKey === scopeKey && guard.generation === scopeGeneration;
	}

	function presentationKey(card: ResurfacingCard, primary: ResurfacingPrimaryAction): string {
		return [scopeKey, card.candidate_id, primary.contentRevision ?? '', primary.kind].join('\u0000');
	}

	async function recordRecommendationPresentation(
		card: ResurfacingCard,
		primary: ResurfacingPrimaryAction
	): Promise<void> {
		if (!primary.isRecommendation) return;
		const key = presentationKey(card, primary);
		if (presentedRecommendations.has(key)) return;
		presentedRecommendations = new Set(presentedRecommendations).add(key);
		const guard = captureScopeGuard();
		try {
			await postResurfacingRecommendationEvent(card.candidate_id, {
				kind: primary.kind,
				content_revision: primary.contentRevision,
				event: 'presented'
			});
		} catch (error) {
			if (!scopeGuardIsCurrent(guard)) return;
			const updated = new Set(presentedRecommendations);
			updated.delete(key);
			presentedRecommendations = updated;
			console.warn('[worth-a-look] recommendation presentation telemetry failed', error);
		}
	}

	function recommendationPresentation(
		node: HTMLElement,
		value: { card: ResurfacingCard; primary: ResurfacingPrimaryAction }
	) {
		void node;
		void recordRecommendationPresentation(value.card, value.primary);
		return {
			update(next: { card: ResurfacingCard; primary: ResurfacingPrimaryAction }) {
				void recordRecommendationPresentation(next.card, next.primary);
			}
		};
	}

	function updateTotal(nextTotal: number): void {
		total = Math.max(0, nextTotal);
		dispatch('countchange', { total });
	}

	function rememberNextCursor(pageNumber: number, cursor: ResurfacingCursor | null): void {
		const updated = new Map(cursorsByPage);
		for (const cachedPage of updated.keys()) {
			if (cachedPage > pageNumber + 1) updated.delete(cachedPage);
		}
		if (cursor) updated.set(pageNumber + 1, cursor);
		else updated.delete(pageNumber + 1);
		cursorsByPage = updated;
	}

	async function fetchPage(pageNumber: number, useCursor: boolean): Promise<ResurfacingPage> {
		const cursor = useCursor ? cursorsByPage.get(pageNumber) ?? null : null;
		return fetchResurfacingTodayPage({
			limit: PAGE_SIZE,
			...(pageNumber > 1 && cursor
				? { cursor }
				: { offset: (pageNumber - 1) * PAGE_SIZE })
		});
	}

	async function loadPage(pageNumber: number, mode: 'initial' | 'nav' = 'nav'): Promise<void> {
		const rid = ++requestId;
		const guard = captureScopeGuard();
		const safePage = Math.max(1, Math.floor(pageNumber));
		const nextOffset = (safePage - 1) * PAGE_SIZE;
		loadingPage = true;
		loadError = null;
		hasMore = false;
		nextCursor = null;
		try {
			const adjacent = renderedPage > 0 && Math.abs(safePage - renderedPage) === 1;
			const next = await fetchPage(safePage, adjacent);
			if (rid !== requestId || !scopeGuardIsCurrent(guard)) return;
			rememberNextCursor(safePage, next.next_cursor);
			if (next.limit === 0 && next.total === 0 && next.cards.length === 0) {
				if (mode === 'nav') showError("Couldn't load resurfaced items.");
				return;
			}
			cards = next.cards;
			learningHealth = next.health;
			actionability = next.actionability;
			actionabilityTraining = next.actionability_training;
			grouping = next.grouping;
			routing = next.routing;
			bandit = next.bandit;
			semanticExtraction = next.semantic_extraction;
			crossLaneReconciliation = next.cross_lane_reconciliation?.status === 'succeeded'
				? next.cross_lane_reconciliation
				: null;
			verifiedScopeKey = guard.scopeKey;
			semanticRankingEnabled = next.semantic_ranking_enabled;
			if (detailCard) {
				const current = next.cards.find((card) => card.candidate_id === detailCard?.candidate_id);
				if (current) detailCard = current;
				else closeDetail();
			}
			updateTotal(next.total);
			hasMore = next.has_more;
			nextCursor = next.next_cursor;
			pageOffset = nextOffset;
			renderedPage = safePage;
		} catch (error) {
			if (rid !== requestId || !scopeGuardIsCurrent(guard)) return;
			const message = error instanceof Error ? error.message : "Couldn't load resurfaced items.";
			loadError = message;
			const retainVerified = verifiedScopeKey === guard.scopeKey && crossLaneReconciliation !== null;
			if (!retainVerified) {
				cards = [];
				learningHealth = null;
				actionability = null;
				actionabilityTraining = null;
				grouping = null;
				routing = null;
				bandit = null;
				semanticExtraction = null;
				crossLaneReconciliation = null;
				semanticRankingEnabled = false;
				updateTotal(0);
			}
			showError(message);
		} finally {
			if (rid === requestId && scopeGuardIsCurrent(guard)) loadingPage = false;
		}
	}

	function reload(): Promise<void> {
		const safePage = Math.max(1, Math.floor(page));
		const retainVerified = verifiedScopeKey === scopeKey && crossLaneReconciliation !== null;
		if (!retainVerified) cards = [];
		closeDetail();
		closeMenu(false);
		closeActionDialog();
		if (!retainVerified) total = 0;
		hasMore = false;
		loadError = null;
		nextCursor = null;
		pageOffset = (safePage - 1) * PAGE_SIZE;
		return loadPage(safePage, 'initial');
	}

	function goToPage(pageNumber: number): void {
		if (loadingPage) return;
		const safePage = Math.min(pageCount, Math.max(1, Math.floor(pageNumber)));
		if (safePage === requestedPage) return;
		if (safePage === requestedPage + 1 && nextCursor) {
			const updated = new Map(cursorsByPage);
			updated.set(safePage, nextCursor);
			cursorsByPage = updated;
		}
		dispatch('pagechange', { page: safePage });
	}

	function onPagerPageChange(event: CustomEvent<{ page: number }>): void {
		goToPage(event.detail.page);
	}

	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: requestedPage = Math.max(1, Math.floor(page));
	$: if (browser && scopeKey && (loadedScopeKey !== scopeKey || loadedPage !== requestedPage)) {
		if (loadedScopeKey !== scopeKey) {
			scopeGeneration += 1;
			requestId += 1;
			detailRequestId += 1;
			originalRequestId += 1;
			operationRequestId += 1;
			cursorsByPage = new Map([[1, null]]);
			deeperSummaries = new Map();
			actionRetryKeys = new Map();
			presentedRecommendations = new Set();
			busyIds = new Set();
			cards = [];
			total = 0;
			hasMore = false;
			loadError = null;
			learningHealth = null;
			actionability = null;
			actionabilityTraining = null;
			grouping = null;
			routing = null;
			bandit = null;
			semanticExtraction = null;
			crossLaneReconciliation = null;
			verifiedScopeKey = '';
			semanticRankingEnabled = false;
			operationBusy = null;
			renderedPage = 0;
		}
		loadedScopeKey = scopeKey;
		loadedPage = requestedPage;
		void reload();
	}

	// Optional dismiss reasons (channel-assist vocabulary). "Bad match" reasons
	// (not_relevant/spam) teach the ranker to show fewer like it; "done" reasons
	// (handled/duplicate/delegated) just clear this card without penalizing similar.
	const DISMISS_REASONS: { value: ResurfacingDismissReason; label: string; hint: string }[] = [
		{ value: 'not_relevant', label: 'Not relevant', hint: 'Bad match — show fewer like this' },
		{ value: 'already_handled', label: 'Already handled', hint: 'Done — keeps showing similar' },
		{ value: 'duplicate', label: 'Duplicate', hint: 'Already seen — keeps showing similar' },
		{ value: 'delegated', label: 'Delegated', hint: 'Handed off — keeps showing similar' },
		{ value: 'spam', label: 'Spam', hint: 'Junk — quiet this source' }
	];

	async function act(
		card: ResurfacingCard,
		action: ResurfacingAction,
		reason?: ResurfacingDismissReason
	): Promise<void> {
		if (busyIds.has(card.candidate_id) || operationBusy?.candidateId === card.candidate_id) return;
		const guard = captureScopeGuard();
		const mutationScope = {
			principal: $scopeIdentityStore.principal,
			workspace: $scopeIdentityStore.workspace
		};
		const pageRid = requestId;
		const index = cards.findIndex((candidate) => candidate.candidate_id === card.candidate_id);
		if (index < 0) return;
		const removed = cards[index];
		const attribution = attentionFeedbackAttribution(
			card.decision_item ?? null,
			card.routing_page?.impression_policy ?? null,
			'worth_a_look'
		);
		busyIds = new Set(busyIds).add(card.candidate_id);
		cards = cards.filter((candidate) => candidate.candidate_id !== card.candidate_id);
		closeMenu(false);
		const result = await optimisticAttentionMutationQueue.enqueue(
			worthAttentionMutationKey(card.candidate_id, mutationScope),
			() => postResurfacingAction(
				card.candidate_id,
				action,
				reason,
				attribution
			)
		);
		if (!scopeGuardIsCurrent(guard)) return;
		busyIds = new Set([...busyIds].filter((id) => id !== card.candidate_id));
		if (pageRid !== requestId) return;
		if (!result.ok) {
			if (!cards.some((candidate) => candidate.candidate_id === removed.candidate_id)) {
				const at = Math.min(index, cards.length);
				cards = [...cards.slice(0, at), removed, ...cards.slice(at)];
			}
			showError(`Couldn't record that: ${result.error}`);
			return;
		}
		if (detailCard?.candidate_id === card.candidate_id) closeDetail();
		updateTotal(total - 1);
		cursorsByPage = new Map([[1, null]]);
		if (cards.length === 0 && requestedPage > 1) {
			dispatch('pagechange', { page: requestedPage - 1 });
		} else {
			void reload();
		}
	}

	async function loadDetail(card: ResurfacingCard, force = false): Promise<ResurfacingDetail | null> {
		if (!force && detail?.candidate_id === card.candidate_id) return detail;
		const rid = ++detailRequestId;
		const guard = captureScopeGuard();
		detailLoading = true;
		detailError = null;
		try {
			const loaded = await fetchResurfacingDetail(card.candidate_id);
			if (!scopeGuardIsCurrent(guard) || rid !== detailRequestId || detailCard?.candidate_id !== card.candidate_id) return null;
			detail = loaded;
			return loaded;
		} catch (error) {
			if (!scopeGuardIsCurrent(guard) || rid !== detailRequestId || detailCard?.candidate_id !== card.candidate_id) return null;
			detailError = error instanceof Error ? error.message : 'Current details are unavailable.';
			return null;
		} finally {
			if (scopeGuardIsCurrent(guard) && rid === detailRequestId) detailLoading = false;
		}
	}

	async function openDetail(card: ResurfacingCard, force = false): Promise<ResurfacingDetail | null> {
		if (detailCard?.candidate_id !== card.candidate_id) {
			detailRequestId += 1;
			originalRequestId += 1;
			detailCard = card;
			detail = null;
			detailError = null;
			originalLoading = false;
		}
		return loadDetail(card, force);
	}

	function closeDetail(): void {
		detailRequestId += 1;
		originalRequestId += 1;
		detailCard = null;
		detail = null;
		detailError = null;
		detailLoading = false;
		originalLoading = false;
	}

	async function showOriginal(card: ResurfacingCard): Promise<boolean> {
		const guard = captureScopeGuard();
		await openDetail(card);
		if (!scopeGuardIsCurrent(guard) || detailCard?.candidate_id !== card.candidate_id) return false;
		if (detail?.candidate_id === card.candidate_id && detail.original) return true;
		const rid = ++originalRequestId;
		originalLoading = true;
		detailError = null;
		try {
			const loaded = await fetchResurfacingOriginal(card.candidate_id);
			if (!scopeGuardIsCurrent(guard) || rid !== originalRequestId || detailCard?.candidate_id !== card.candidate_id) return false;
			detail = loaded;
			if (!loaded.original) {
				showWarning('Original content is unavailable', statusMessage(loaded.status));
				return false;
			}
			return true;
		} catch (error) {
			if (!scopeGuardIsCurrent(guard) || rid !== originalRequestId || detailCard?.candidate_id !== card.candidate_id) return false;
			detailError = error instanceof Error ? error.message : 'Original content is unavailable.';
			showError('Could not load the original', detailError);
			return false;
		} finally {
			if (scopeGuardIsCurrent(guard) && rid === originalRequestId) originalLoading = false;
		}
	}

	async function openSource(card: ResurfacingCard): Promise<boolean> {
		const guard = captureScopeGuard();
		const current = await openDetail(card);
		if (!scopeGuardIsCurrent(guard)) return false;
		const route = current?.open_url || current?.source_route;
		if (!route) {
			showWarning('Source cannot be opened', current ? statusMessage(current.status) : detailError ?? undefined);
			return false;
		}
		if (route.startsWith('/')) await goto(route);
		else window.open(route, '_blank', 'noopener,noreferrer');
		return true;
	}

	function statusMessage(status: ResurfacingDetail['status']): string {
		switch (status) {
			case 'offline': return 'The provider is offline.';
			case 'deleted': return 'The source item was deleted.';
			case 'suppressed': return 'Content policy restricts this source.';
			case 'unsupported': return 'This source does not support that operation.';
			case 'stale': return 'The source has changed.';
			default: return 'The source is currently unavailable.';
		}
	}

	function recommendationFor(card: ResurfacingCard) {
		const primary = resurfacingPrimaryAction(
			card,
			detailCard?.candidate_id === card.candidate_id ? detail : null
		);
		if (primary.kind !== 'ask_presto' || chatThreadId?.trim()) return primary;
		return {
			kind: 'view_details' as const,
			label: 'Details',
			rationale: 'Review the safe structured brief.',
			contentRevision:
				detailCard?.candidate_id === card.candidate_id
					? detail?.content_revision ?? null
					: card.content_revision,
			isRecommendation: false
		};
	}

	async function recordReadRecommendation(
		card: ResurfacingCard,
		kind: ResurfacingActionKind,
		event: 'selected' | 'completed'
	): Promise<boolean> {
		const guard = captureScopeGuard();
		const primary = recommendationFor(card);
		if (!primary.isRecommendation || primary.kind !== kind) return true;
		try {
			await postResurfacingRecommendationEvent(card.candidate_id, {
				kind,
				content_revision: primary.contentRevision,
				event
			});
			return scopeGuardIsCurrent(guard);
		} catch (error) {
			if (!scopeGuardIsCurrent(guard)) return false;
			if (error instanceof ResurfacingApiError && error.code === 'stale_revision') {
				await refreshStaleCard(card);
				return false;
			}
			console.warn('[worth-a-look] recommendation telemetry failed', error);
			return true;
		}
	}

	async function runReadAction(
		card: ResurfacingCard,
		kind: 'view_details' | 'show_original' | 'open_source',
		fromPrimary: boolean
	): Promise<void> {
		if (cardIsBusy(card.candidate_id)) return;
		const guard = captureScopeGuard();
		const operationId = ++operationRequestId;
		operationBusy = { candidateId: card.candidate_id, kind };
		let completed = false;
		try {
			if (fromPrimary && !(await recordReadRecommendation(card, kind, 'selected'))) return;
			if (!scopeGuardIsCurrent(guard) || operationId !== operationRequestId) return;
			if (kind === 'view_details') completed = (await openDetail(card)) !== null;
			else if (kind === 'show_original') completed = await showOriginal(card);
			else completed = await openSource(card);
		} finally {
			if (scopeGuardIsCurrent(guard) && operationId === operationRequestId) operationBusy = null;
		}
		if (completed && fromPrimary && scopeGuardIsCurrent(guard)) {
			void recordReadRecommendation(card, kind, 'completed');
		}
	}

	function contentRevisionFor(card: ResurfacingCard, kind: ResurfacingActionKind): string | null {
		const primary = recommendationFor(card);
		if (primary.isRecommendation && primary.kind === kind) return primary.contentRevision;
		if (detailCard?.candidate_id === card.candidate_id && detail) return detail.content_revision;
		return card.content_revision;
	}

	function uuid(): string {
		if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') return crypto.randomUUID();
		const bytes = new Uint8Array(16);
		crypto.getRandomValues(bytes);
		bytes[6] = (bytes[6] & 0x0f) | 0x40;
		bytes[8] = (bytes[8] & 0x3f) | 0x80;
		const hex = [...bytes].map((value) => value.toString(16).padStart(2, '0')).join('');
		return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
	}

	function contextualRetryKey(card: ResurfacingCard, kind: ResurfacingActionKind): string {
		return [scopeKey, card.candidate_id, contentRevisionFor(card, kind) ?? '', kind].join('\u0000');
	}

	function contextualIdempotencyKey(card: ResurfacingCard, kind: ResurfacingActionKind): string {
		const key = contextualRetryKey(card, kind);
		const existing = actionRetryKeys.get(key);
		if (existing) return existing;
		const idempotencyKey = uuid();
		actionRetryKeys = new Map(actionRetryKeys).set(key, idempotencyKey);
		return idempotencyKey;
	}

	function clearContextualRetryKey(card: ResurfacingCard, kind: ResurfacingActionKind): void {
		const updated = new Map(actionRetryKeys);
		updated.delete(contextualRetryKey(card, kind));
		actionRetryKeys = updated;
	}

	function retainIdempotencyKey(error: unknown): boolean {
		return (
			!(error instanceof ResurfacingApiError) ||
			error.code === 'in_progress' ||
			error.status >= 500
		);
	}

	function openActionDialog(card: ResurfacingCard, kind: ResurfacingDialogActionKind): void {
		closeMenu(false);
		const granted = requestFocus({
			id: ACTION_DIALOG_OVERLAY_ID,
			priority: OVERLAY_PRIORITIES.pageModal,
			onClose: closeActionDialog
		});
		if (!granted) return;
		dialogCard = card;
		dialogDetail = detailCard?.candidate_id === card.candidate_id ? detail : null;
		dialogAction = kind;
		dialogError = null;
		dialogIdempotencyKey = uuid();
	}

	function closeActionDialog(): void {
		dialogCard = null;
		dialogDetail = null;
		dialogAction = null;
		dialogError = null;
		dialogIdempotencyKey = '';
		release(ACTION_DIALOG_OVERLAY_ID);
	}

	async function submitDialogAction(
		event: CustomEvent<{ kind: ResurfacingDialogActionKind; input: Record<string, unknown> }>
	): Promise<void> {
		if (!dialogCard || operationBusy) return;
		const card = dialogCard;
		const kind = event.detail.kind;
		const guard = captureScopeGuard();
		const operationId = ++operationRequestId;
		operationBusy = { candidateId: card.candidate_id, kind };
		dialogError = null;
		try {
			const response = await postResurfacingContextualAction(card.candidate_id, {
				kind,
				idempotency_key: dialogIdempotencyKey,
				content_revision: contentRevisionFor(card, kind),
				input: event.detail.input
			});
			if (!scopeGuardIsCurrent(guard) || operationId !== operationRequestId || dialogCard?.candidate_id !== card.candidate_id) return;
			closeActionDialog();
			await handleContextualResult(card, response, guard);
		} catch (error) {
			if (!scopeGuardIsCurrent(guard) || operationId !== operationRequestId) return;
			if (error instanceof ResurfacingApiError && error.code === 'stale_revision') {
				closeActionDialog();
				await refreshStaleCard(card);
			} else {
				dialogError = error instanceof Error ? error.message : 'The action could not be completed.';
				if (!retainIdempotencyKey(error)) dialogIdempotencyKey = uuid();
			}
		} finally {
			if (scopeGuardIsCurrent(guard) && operationId === operationRequestId) operationBusy = null;
		}
	}

	async function runContextualAction(card: ResurfacingCard, kind: ResurfacingActionKind): Promise<void> {
		if (cardIsBusy(card.candidate_id)) return;
		if (kind === 'ask_presto' && !chatThreadId?.trim()) {
			showWarning('Ask Presto is unavailable', 'Open or create a chat thread in this workspace first.');
			return;
		}
		const guard = captureScopeGuard();
		const operationId = ++operationRequestId;
		const idempotencyKey = contextualIdempotencyKey(card, kind);
		operationBusy = { candidateId: card.candidate_id, kind };
		try {
			const input = kind === 'ask_presto' ? { ui_thread_id: chatThreadId } : {};
			const response = await postResurfacingContextualAction(card.candidate_id, {
				kind,
				idempotency_key: idempotencyKey,
				content_revision: contentRevisionFor(card, kind),
				input
			});
			if (!scopeGuardIsCurrent(guard) || operationId !== operationRequestId) return;
			clearContextualRetryKey(card, kind);
			await handleContextualResult(card, response, guard);
		} catch (error) {
			if (!scopeGuardIsCurrent(guard) || operationId !== operationRequestId) return;
			if (!retainIdempotencyKey(error)) clearContextualRetryKey(card, kind);
			if (error instanceof ResurfacingApiError && error.code === 'stale_revision') {
				await refreshStaleCard(card);
			} else {
				showError(
					`${actionLabel(kind)} failed`,
					error instanceof Error ? error.message : 'The action could not be completed.'
				);
			}
		} finally {
			if (scopeGuardIsCurrent(guard) && operationId === operationRequestId) operationBusy = null;
		}
	}

	async function handleContextualResult(
		card: ResurfacingCard,
		response: ResurfacingContextualActionResponse,
		guard: ScopeGuard
	): Promise<void> {
		if (!scopeGuardIsCurrent(guard)) return;
		// Toasts and Ask Presto navigation are shared with the canonical lane so
		// the confirmation cannot differ by surface; what is left is genuinely
		// band-specific — where a deeper summary is shown, and what "refresh"
		// means for a paginated list.
		const { shouldRefresh, deeperSummary } = await presentResurfacingActionResult(
			response.result
		);
		if (!scopeGuardIsCurrent(guard)) return;
		if (deeperSummary) {
			const updated = new Map(deeperSummaries);
			updated.set(contextualRetryKey(card, 'summarize_deeper'), deeperSummary);
			deeperSummaries = updated;
			await openDetail(card);
			return;
		}
		if (!shouldRefresh) return;
		cursorsByPage = new Map([[1, null]]);
		await loadPage(requestedPage, 'nav');
	}

	async function refreshStaleCard(card: ResurfacingCard): Promise<void> {
		const guard = captureScopeGuard();
		showWarning('Source changed', 'The brief was refreshed. Review it before trying the action again.');
		cursorsByPage = new Map([[1, null]]);
		await loadPage(requestedPage, 'nav');
		if (!scopeGuardIsCurrent(guard)) return;
		const current = cards.find((candidate) => candidate.candidate_id === card.candidate_id);
		if (current) await openDetail(current, true);
	}

	async function runCapability(
		card: ResurfacingCard,
		kind: ResurfacingActionKind,
		fromPrimary = false
	): Promise<void> {
		closeMenu(false);
		if (kind === 'view_details' || kind === 'show_original' || kind === 'open_source') {
			await runReadAction(card, kind, fromPrimary);
			return;
		}
		if (isResurfacingDialogAction(kind)) {
			openActionDialog(card, kind);
			return;
		}
		await runContextualAction(card, kind);
	}

	function cardIsBusy(candidateId: string): boolean {
		return busyIds.has(candidateId) || operationBusy?.candidateId === candidateId;
	}

	function sourceBadge(kind: string): string {
		const trimmed = kind.trim();
		if (!trimmed) return 'Memory';
		return trimmed.replace(/_/g, ' ').replace(/\b\w/g, (letter) => letter.toUpperCase());
	}

	function actionIcon(kind: ResurfacingActionKind): IconName {
		switch (kind) {
			case 'view_details': return 'file-text';
			case 'open_source': return 'arrow-up-right';
			case 'show_original': return 'eye';
			case 'ask_presto': return 'message';
			case 'create_task': return 'flag';
			case 'create_reminder': return 'calendar';
			case 'summarize_deeper': return 'sparkle';
			case 'save_to_memory': return 'archive';
			default: return 'arrow-right';
		}
	}

	function quickAction(card: ResurfacingCard): ResurfacingActionKind | null {
		const details = detailCard?.candidate_id === card.candidate_id ? detail : null;
		const primary = recommendationFor(card);
		const available = new Set(mergeResurfacingCapabilities(card, details).map((item) => item.kind));
		if (primary.kind !== 'view_details') return 'view_details';
		return available.has('open_source') ? 'open_source' : null;
	}

	function menuActions(card: ResurfacingCard): ReturnType<typeof mergeResurfacingCapabilities> {
		const details = detailCard?.candidate_id === card.candidate_id ? detail : null;
		const primary = recommendationFor(card);
		const quick = quickAction(card);
		return mergeResurfacingCapabilities(card, details).filter(
			(action) =>
				action.kind !== primary.kind &&
				action.kind !== quick &&
				action.kind !== 'view_details' &&
				(action.kind !== 'ask_presto' || Boolean(chatThreadId?.trim()))
		);
	}

	async function toggleMenu(card: ResurfacingCard, event: MouseEvent): Promise<void> {
		const guard = captureScopeGuard();
		event.stopPropagation();
		if (menuCardId === card.candidate_id) {
			closeMenu(true);
			return;
		}
		menuCardId = card.candidate_id;
		menuTrigger = event.currentTarget as HTMLButtonElement;
		const triggerRect = menuTrigger.getBoundingClientRect();
		menuTop = triggerRect.bottom + 4;
		menuRight = Math.max(8, window.innerWidth - triggerRect.right);
		await tick();
		if (!scopeGuardIsCurrent(guard) || menuCardId !== card.candidate_id) return;
		const menuRect = menuEl?.getBoundingClientRect();
		if (menuRect && menuRect.bottom > window.innerHeight - 8) {
			menuTop = Math.max(8, triggerRect.top - menuRect.height - 4);
		}
		menuEl?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
	}

	function closeMenu(refocus: boolean): void {
		const trigger = menuTrigger;
		menuCardId = null;
		menuEl = null;
		menuTrigger = null;
		if (refocus) void tick().then(() => trigger?.focus());
	}

	function handleWindowClick(event: MouseEvent): void {
		const target = event.target as Node;
		if (
			menuCardId &&
			!menuEl?.contains(target) &&
			!menuTrigger?.contains(target)
		) closeMenu(false);
	}

	function handleMenuKeydown(event: KeyboardEvent): void {
		if (!menuEl) return;
		const items = Array.from(menuEl.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)'));
		const index = items.indexOf(document.activeElement as HTMLButtonElement);
		let next = index;
		if (event.key === 'ArrowDown') next = (index + 1 + items.length) % items.length;
		else if (event.key === 'ArrowUp') next = (index - 1 + items.length) % items.length;
		else if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = items.length - 1;
		else if (event.key === 'Escape') {
			event.preventDefault();
			closeMenu(true);
			return;
		} else if (event.key === 'Tab') {
			closeMenu(false);
			return;
		} else return;
		event.preventDefault();
		items[next]?.focus();
	}

	function handleMenuTriggerKeydown(card: ResurfacingCard, event: KeyboardEvent): void {
		if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
		event.preventDefault();
		void toggleMenu(card, event as unknown as MouseEvent);
	}

	$: visibleCards = cards.filter(
		(card) =>
			!$optimisticAttentionMutationQueue.statusByKey.has(
				worthAttentionMutationKey(card.candidate_id, $scopeIdentityStore)
			)
	);
	$: hiddenMutationCount = cards.length - visibleCards.length;
	$: visibleTotal = Math.max(0, total - hiddenMutationCount);
	$: shownCount = visibleCards.length;
	$: allBusyIds = new Set([
		...busyIds,
		...(operationBusy ? [operationBusy.candidateId] : [])
	]);
	$: pageStart = visibleTotal === 0 ? 0 : pageOffset + 1;
	$: pageEnd = Math.min(visibleTotal, pageOffset + shownCount);
	$: pageNumber = requestedPage;
	$: pageCount = Math.max(1, Math.ceil(visibleTotal / PAGE_SIZE));
	$: showPagination = visibleTotal > PAGE_SIZE || requestedPage > 1 || hasMore;
	$: showBand =
		showEmpty ||
		visibleCards.length > 0 ||
		hasMore ||
		loadingPage ||
		loadError !== null ||
		learningHealth !== null ||
		actionability !== null ||
		grouping !== null ||
		routing !== null ||
		bandit !== null ||
		semanticExtraction !== null ||
		crossLaneReconciliation !== null;
	$: reconciliationDiagnostic = loadError
		? (crossLaneReconciliation
			? 'Cross-lane verification is unavailable. The last verified same-scope Worth list is retained.'
			: 'Cross-lane verification is unavailable. The uncertain Worth response was suppressed.')
		: crossLaneReconciliation
			? `Cross-lane verified · ${crossLaneReconciliation.duplicate_hidden_page_total} exact ${crossLaneReconciliation.duplicate_hidden_page_total === 1 ? 'duplicate' : 'duplicates'} hidden on this page.`
			: null;

	onDestroy(() => {
		scopeGeneration += 1;
		requestId += 1;
		detailRequestId += 1;
		originalRequestId += 1;
		operationRequestId += 1;
		release(ACTION_DIALOG_OVERLAY_ID);
	});
</script>

<svelte:window on:click={handleWindowClick} />

{#if showBand}
	<section class="worth-band" aria-label="Worth a look">
		<div class="worth-band__header">
			<h2>Worth a look</h2>
			<a class="worth-band__status" href="/resurfacing" title="See resurfacing engine health">
				engine <Icon name="arrow-up-right" size={12} />
			</a>
		</div>
		<AttentionLearningHealthStrip
			health={learningHealth}
			{actionability}
			{actionabilityTraining}
			{grouping}
			{routing}
			{bandit}
			{semanticExtraction}
			surfaceLabel="Worth a look"
			semanticRankingEnabled={semanticRankingEnabled}
		/>
		{#if reconciliationDiagnostic}
			<div
				class="worth-reconciliation"
				class:worth-reconciliation--warning={loadError !== null}
				role={loadError ? 'alert' : 'status'}
				data-testid="worth-cross-lane-reconciliation"
			>
				<strong>{loadError ? 'System reconciliation unavailable' : 'Exact cross-lane reconciliation'}</strong>
				<span>{reconciliationDiagnostic}</span>
			</div>
		{/if}
		{#if showPagination}
			<ServerPager
				currentPage={pageNumber}
				pageCount={pageCount}
				startItem={pageStart}
				endItem={pageEnd}
				totalItems={visibleTotal}
				loading={loadingPage}
				ariaLabel="Worth a look pagination"
				on:pagechange={onPagerPageChange}
			/>
		{/if}
		<div class="worth-table-wrap" on:scroll={() => closeMenu(false)}>
			<table class="worth-table">
				<thead>
					<tr>
						<th scope="col">Item</th>
						<th scope="col">Why now</th>
						<th scope="col">Source</th>
						<th scope="col" class="worth-table__actions-head">Next step</th>
					</tr>
				</thead>
				<tbody>
					{#if loadingPage && visibleCards.length === 0}
						{#each Array(3) as _}
							<tr class="worth-table__skeleton">
								<td><span></span><span></span><span></span></td>
								<td><span></span></td>
								<td><span></span></td>
								<td><span></span></td>
							</tr>
						{/each}
					{:else if loadError && visibleCards.length === 0}
						<tr>
							<td colspan="4">
								<div class="worth-table__empty" role="alert">
									<strong>Worth a look is unavailable.</strong>
									<span>{loadError}</span>
								</div>
							</td>
						</tr>
					{:else if visibleCards.length === 0}
						<tr>
							<td colspan="4">
								<div class="worth-table__empty">
									<strong>Nothing worth a look right now.</strong>
									<span>Resurfaced memory, task, and message items will appear here.</span>
								</div>
							</td>
						</tr>
					{:else}
						{#each visibleCards as card (card.candidate_id)}
							{@const activeDetail = detailCard?.candidate_id === card.candidate_id ? detail : null}
							{@const learningRank = parseChannelFollowUpLearningRank(card)}
							{@const rankDeltaLabel = followUpRankDeltaLabel(learningRank, semanticRankingEnabled)}
							{@const primary = recommendationFor(card)}
							{@const quick = quickAction(card)}
							{@const activeBrief = resurfacingBrief(card, activeDetail)}
							{@const facts = resurfacingRowFacts(activeBrief)}
							<tr
								class:worth-table__row-busy={allBusyIds.has(card.candidate_id)}
								use:verifiedAttentionVisibility={{
									decision_item: card.decision_item ?? null,
									impression_policy: card.routing_page?.impression_policy ?? null,
									surface: 'worth_a_look'
								}}
							>
								<td class="worth-table__item">
									<strong>{resurfacingDisplayTitle(card, activeDetail)}</strong>
									<p>{resurfacingConcreteSummary(card, activeDetail)}</p>
									{#if facts.length ||
										resurfacingHasMissingDetails(activeBrief) ||
										activeDetail?.source_updated ||
										(!activeDetail && card.source_updated) ||
										(!activeDetail && card.brief_status === 'legacy')}
										<div class="worth-facts" aria-label="Key facts">
											{#each facts as fact (fact.id)}
												<span class="worth-fact" title={`${fact.label}: ${fact.value}`}>
													<b>{fact.label}</b> {fact.value}
												</span>
											{/each}
											{#if resurfacingHasMissingDetails(activeBrief)}
												<span class="worth-fact worth-fact--warning">Details missing from source</span>
											{/if}
											{#if activeDetail?.source_updated || (!activeDetail && card.source_updated)}
												<span class="worth-fact worth-fact--warning">Source updated</span>
											{/if}
											{#if !activeDetail && card.brief_status === 'legacy'}
												<span class="worth-fact">Legacy brief · repair pending</span>
											{/if}
										</div>
									{/if}
									{#if rankDeltaLabel}
										<div class="worth-rank-delta" class:worth-rank-delta--active={semanticRankingEnabled}>
											<span aria-hidden="true">{(learningRank?.rank_delta ?? 0) > 0 ? '↑' : (learningRank?.rank_delta ?? 0) < 0 ? '↓' : '→'}</span>
											{rankDeltaLabel}
										</div>
									{/if}
									<AttentionActionabilityBadge metadata={card.actionability ?? null} />
									<AttentionRoutingDiagnostic
										item={card.decision_item ?? null}
										page={card.routing_page ?? null}
									/>
									<AttentionBanditDiagnostic decision={card.bandit_decision ?? null} />
									<ResurfacingGroupPanel {card} on:groupchanged={() => void reload()} />
								</td>
								<td class="worth-table__why">
									{activeDetail && !activeDetail.summary
										? statusMessage(activeDetail.status)
										: card.why_now || 'May be worth revisiting'}
								</td>
								<td><span class="worth-source-badge">{sourceBadge(card.source_kind)}</span></td>
								<td>
									<div class="worth-actions">
										<span id={`worth-rationale-${card.candidate_id}`} class="sr-only">
											{primary.rationale}
										</span>
										<button
											type="button"
										class="worth-btn worth-btn--primary"
										use:recommendationPresentation={{ card, primary }}
											disabled={allBusyIds.has(card.candidate_id)}
											aria-describedby={`worth-rationale-${card.candidate_id}`}
											title={primary.rationale || primary.label}
											on:click={() => void runCapability(card, primary.kind, true)}
										>
											<Icon name={actionIcon(primary.kind)} size={12} />
											{operationBusy?.candidateId === card.candidate_id && operationBusy.kind === primary.kind
												? 'Working…'
												: primary.label}
										</button>
										{#if quick}
											<button
												type="button"
												class="worth-icon-btn"
												disabled={allBusyIds.has(card.candidate_id)}
												title={actionLabel(quick)}
												aria-label={actionLabel(quick)}
												on:click={() => void runCapability(card, quick)}
											>
												<Icon name={actionIcon(quick)} size={13} />
											</button>
										{/if}
										<div class="worth-menu-wrap">
											<button
												type="button"
												class="worth-icon-btn"
												disabled={allBusyIds.has(card.candidate_id)}
												aria-label="More actions"
												aria-haspopup="menu"
												aria-expanded={menuCardId === card.candidate_id}
												title="More actions"
												on:click={(event) => void toggleMenu(card, event)}
												on:keydown={(event) => handleMenuTriggerKeydown(card, event)}
											>
												<Icon name="dots-horizontal" size={14} />
											</button>
											{#if menuCardId === card.candidate_id}
												<div
													class="worth-menu"
													role="menu"
													tabindex="-1"
													aria-label="Worth a look actions"
													style={`top: ${menuTop}px; right: ${menuRight}px;`}
													bind:this={menuEl}
													on:keydown={handleMenuKeydown}
												>
													{#each menuActions(card) as action (action.kind)}
														<button type="button" role="menuitem" on:click={() => void runCapability(card, action.kind)}>
															<Icon name={actionIcon(action.kind)} size={14} />
															<span>{action.label}</span>
														</button>
													{/each}
													<div class="worth-menu__separator" role="separator"></div>
													<button
														type="button"
														role="menuitem"
														title="More like this — nudges similar items up"
														on:click={() => void act(card, 'open')}
													>
														<Icon name="check" size={12} /><span>Mark useful</span>
													</button>
													<button
														type="button"
														role="menuitem"
														title="Seen — clear it without changing what's surfaced next"
														on:click={() => void act(card, 'acknowledge')}
													>
														<Icon name="archive" size={12} /><span>Acknowledge</span>
													</button>
													<button
														type="button"
														role="menuitem"
														title="Not interested — nudges similar items down"
														on:click={() => void act(card, 'dismiss')}
													>
														<Icon name="x" size={12} /><span>Dismiss</span>
													</button>
													<div class="worth-menu__reason-group" role="group" aria-label="Dismiss with a reason">
														<span class="worth-menu__reason-head">Dismiss because…</span>
														{#each DISMISS_REASONS as reason (reason.value)}
															<button
																type="button"
																role="menuitem"
																class="worth-menu__reason"
																title={reason.hint}
																on:click={() => void act(card, 'dismiss', reason.value)}
															>
																{reason.label}
															</button>
														{/each}
													</div>
												</div>
											{/if}
										</div>
										<!-- Second row: direct labeled feedback; the same options + dismiss reasons remain in the ⋯ menu -->
										<div class="worth-actions__break" aria-hidden="true"></div>
										<button
											type="button"
											class="worth-btn worth-btn--feedback worth-btn--useful"
											disabled={allBusyIds.has(card.candidate_id)}
											title="Useful — more like this. Stays in Worth a look."
											on:click={() => void act(card, 'open')}
										>
											<Icon name="check" size={12} /><span>Useful</span>
										</button>
										<button
											type="button"
											class="worth-btn worth-btn--feedback"
											disabled={allBusyIds.has(card.candidate_id)}
											title="This is my work — similar future cards should land in For you"
											on:click={() => void act(card, 'owner_work')}
										>
											<span>This needs me</span>
										</button>
										<button
											type="button"
											class="worth-btn worth-btn--feedback"
											disabled={allBusyIds.has(card.candidate_id)}
											title="Acknowledge — seen, no change to what is surfaced"
											on:click={() => void act(card, 'acknowledge')}
										>
											<Icon name="archive" size={12} /><span>Acknowledge</span>
										</button>
										<button
											type="button"
											class="worth-btn worth-btn--feedback worth-btn--dismiss"
											disabled={allBusyIds.has(card.candidate_id)}
											title="Dismiss — not interested"
											on:click={() => void act(card, 'dismiss')}
										>
											<Icon name="x" size={12} /><span>Dismiss</span>
										</button>
									</div>
								</td>
							</tr>
							{#if detailCard?.candidate_id === card.candidate_id}
								<tr class="worth-detail-row">
									<td class="worth-detail-cell" colspan="4">
										<ResurfacingDetailPanel
											{card}
											{detail}
											loading={detailLoading}
											loadError={detailError}
											{originalLoading}
											actionBusy={allBusyIds.has(card.candidate_id)}
											deeperSummary={deeperSummaries.get(contextualRetryKey(card, 'summarize_deeper')) ?? null}
											on:close={closeDetail}
											on:action={(event) => void runCapability(card, event.detail.kind)}
										/>
									</td>
								</tr>
							{/if}
						{/each}
					{/if}
				</tbody>
			</table>
		</div>
		{#if showPagination}
			<ServerPager
				currentPage={pageNumber}
				pageCount={pageCount}
				startItem={pageStart}
				endItem={pageEnd}
				totalItems={total}
				loading={loadingPage}
				ariaLabel="Worth a look pagination"
				on:pagechange={onPagerPageChange}
			/>
		{/if}
	</section>
{/if}

<ResurfacingActionDialog
	open={dialogAction !== null}
	kind={dialogAction}
	card={dialogCard}
	detail={dialogDetail}
	busy={operationBusy?.candidateId === dialogCard?.candidate_id}
	serverError={dialogError}
	uiThreadId={chatThreadId}
	on:cancel={closeActionDialog}
	on:submit={(event) => void submitDialogAction(event)}
/>

<style>
	.worth-band {
		display: grid;
		gap: 0.65rem;
		padding: clamp(0.85rem, 1.6vw, 1rem);
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		border-radius: var(--radius-md);
		background: var(--bg-card);
		box-shadow: var(--shadow-md);
	}

	.worth-band__header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 1rem;
		min-width: 0;
	}

	.worth-band__header h2 {
		margin: 0;
		color: var(--text-primary);
		font-size: var(--text-md);
		line-height: 1.2;
	}

	.worth-band__status {
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		flex: 0 0 auto;
		color: var(--text-muted);
		font-size: var(--text-xs);
		text-decoration: none;
		white-space: nowrap;
	}

	.worth-band__status:hover { color: var(--text-secondary); }

	.worth-reconciliation {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		margin: 0.35rem 0;
		padding: 0.35rem 0.55rem;
		border: 1px solid color-mix(in srgb, var(--color-success, #2f8f5b) 30%, var(--border-soft));
		border-radius: 8px;
		background: color-mix(in srgb, var(--color-success, #2f8f5b) 7%, var(--bg-card));
		color: var(--text-secondary);
		font-size: var(--text-2xs, 0.72rem);
	}

	.worth-reconciliation strong { color: var(--text-primary); }

	.worth-reconciliation--warning {
		border-color: color-mix(in srgb, var(--color-warning, #9a6410) 42%, var(--border-soft));
		background: color-mix(in srgb, var(--color-warning, #9a6410) 9%, var(--bg-card));
	}

	.worth-rank-delta {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		margin-top: 0.45rem;
		padding: 0.18rem 0.45rem;
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		background: var(--bg-soft);
		color: var(--text-muted, var(--text-secondary));
		font-size: var(--text-2xs, 0.72rem);
		font-variant-numeric: tabular-nums;
	}

	.worth-rank-delta--active {
		border-color: color-mix(in srgb, var(--color-success, #2f8f5b) 35%, var(--border-soft));
		background: color-mix(in srgb, var(--color-success, #2f8f5b) 9%, var(--bg-card));
		color: var(--color-success, #2f8f5b);
	}

	.worth-table-wrap {
		overflow-x: auto;
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--bg-card) 84%, transparent);
	}

	.worth-table {
		width: 100%;
		min-width: 900px;
		border-collapse: collapse;
		table-layout: fixed;
	}

	.worth-table th,
	.worth-table td {
		padding: 0.68rem;
		border-bottom: 1px solid color-mix(in srgb, var(--border-soft) 62%, transparent);
		text-align: left;
		vertical-align: top;
	}

	.worth-table th {
		color: var(--text-muted);
		font-size: var(--text-2xs);
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0;
		background: color-mix(in srgb, var(--bg-soft) 52%, transparent);
	}

	.worth-table tr:last-child td { border-bottom: 0; }
	.worth-table th:nth-child(1), .worth-table td:nth-child(1) { width: 45%; }
	.worth-table th:nth-child(2), .worth-table td:nth-child(2) { width: 18%; }
	.worth-table th:nth-child(3), .worth-table td:nth-child(3) { width: 10%; }
	.worth-table th:nth-child(4), .worth-table td:nth-child(4) { width: 27%; }
	.worth-table__actions-head { text-align: right; }
	.worth-table__row-busy { opacity: 0.72; }

	.worth-table__item strong {
		display: block;
		color: var(--text-primary);
		font-size: var(--text-sm);
		line-height: 1.35;
		overflow-wrap: anywhere;
	}

	.worth-table__item p {
		display: -webkit-box;
		margin: 0.2rem 0 0;
		color: var(--text-secondary);
		font-size: var(--text-xs);
		line-height: 1.42;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		overflow-wrap: anywhere;
	}

	.worth-facts {
		display: flex;
		gap: 0.28rem;
		flex-wrap: wrap;
		margin-top: 0.42rem;
	}

	.worth-fact,
	.worth-source-badge {
		display: inline-flex;
		align-items: center;
		max-width: 100%;
		padding: 0.16rem 0.4rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 72%, transparent);
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--bg-soft) 62%, transparent);
		color: var(--text-muted);
		font-size: var(--text-2xs);
		line-height: 1.3;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.worth-fact b {
		margin-right: 0.22rem;
		color: var(--text-secondary);
		font-weight: 700;
	}

	.worth-fact--warning {
		border-color: color-mix(in srgb, var(--color-warning) 38%, var(--border-soft));
		background: color-mix(in srgb, var(--color-warning) 9%, transparent);
		color: var(--color-warning);
	}

	.worth-source-badge {
		text-transform: capitalize;
	}

	.worth-table__why {
		color: var(--text-muted);
		font-size: var(--text-xs);
		line-height: 1.42;
		overflow-wrap: anywhere;
	}

	.worth-actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: 0.32rem;
		min-height: 2rem;
	}
	/* Force the feedback buttons onto their own row under the recommended action. */
	.worth-actions__break { flex-basis: 100%; height: 0; margin: 0; }

	.worth-btn,
	.worth-icon-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.24rem;
		min-height: 1.5rem;
		border: 1px solid var(--border-default, var(--border-soft));
		border-radius: var(--radius-sm);
		background: var(--bg-card);
		color: var(--text-secondary);
		font: inherit;
		font-size: var(--text-2xs, 0.72rem);
		line-height: 1;
		white-space: nowrap;
		cursor: pointer;
		transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease;
	}

	.worth-btn { max-width: 12.5rem; padding: 0.22rem 0.48rem; }
	.worth-icon-btn { width: 1.5rem; padding: 0; }
	.worth-btn--feedback { max-width: none; padding: 0.18rem 0.42rem; }
	.worth-btn:hover, .worth-icon-btn:hover { color: var(--text-primary); background: var(--bg-soft); }
	.worth-btn--useful:hover { color: var(--color-success, #16803b); border-color: color-mix(in srgb, var(--color-success, #16803b) 45%, transparent); }
	.worth-btn--dismiss:hover { color: var(--color-danger, #c0392b); border-color: color-mix(in srgb, var(--color-danger, #c0392b) 45%, transparent); }
	.worth-btn:disabled, .worth-icon-btn:disabled { opacity: 0.55; cursor: default; }

	.worth-btn--primary {
		border-color: transparent;
		background: var(--accent-primary);
		color: var(--accent-on-primary, #fff);
		font-weight: 650;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.worth-btn--primary:hover {
		background: color-mix(in srgb, var(--accent-primary) 88%, #000);
		color: var(--accent-on-primary, #fff);
	}

	.worth-menu-wrap { position: relative; flex: 0 0 auto; }

	.worth-menu {
		position: fixed;
		z-index: 30;
		display: grid;
		min-width: 13rem;
		padding: 0.3rem;
		border: 1px solid var(--border-default, var(--border-soft));
		border-radius: var(--radius-sm);
		background: var(--bg-elevated, var(--bg-card));
		box-shadow: var(--shadow-lg, var(--shadow-md));
	}

	.worth-menu button {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		width: 100%;
		padding: 0.48rem 0.55rem;
		border: 0;
		border-radius: var(--radius-sm);
		background: transparent;
		color: var(--text-secondary);
		font: inherit;
		font-size: var(--text-xs);
		line-height: 1.25;
		text-align: left;
		white-space: nowrap;
		cursor: pointer;
	}

	.worth-menu button:hover,
	.worth-menu button:focus-visible {
		outline: none;
		background: var(--bg-soft);
		color: var(--text-primary);
	}

	.worth-menu__separator {
		height: 1px;
		margin: 0.25rem 0.2rem;
		background: var(--border-soft);
	}

	.worth-menu__reason-group {
		display: flex;
		flex-direction: column;
		margin-top: 0.15rem;
		padding-top: 0.15rem;
		border-top: 1px solid var(--border-soft);
	}

	.worth-menu__reason-head {
		padding: 0.3rem 0.55rem 0.15rem;
		color: var(--text-tertiary, var(--text-secondary));
		font-size: var(--text-2xs, 0.72rem);
		letter-spacing: 0.02em;
		text-transform: uppercase;
	}

	.worth-menu__reason {
		padding-left: 1.5rem !important;
		color: var(--text-tertiary, var(--text-secondary));
	}

	.worth-detail-cell { padding: 0 0.68rem 0.68rem !important; }

	.worth-table__skeleton span {
		display: block;
		width: 100%;
		height: 0.8rem;
		margin-bottom: 0.35rem;
		border-radius: var(--radius-sm);
		background: linear-gradient(
			90deg,
			color-mix(in srgb, var(--bg-soft) 68%, transparent),
			color-mix(in srgb, var(--border-soft) 36%, transparent),
			color-mix(in srgb, var(--bg-soft) 68%, transparent)
		);
		background-size: 220% 100%;
		animation: worth-skeleton 1.15s ease-in-out infinite;
	}

	.worth-table__skeleton span:nth-child(2) { width: 82%; }
	.worth-table__skeleton span:nth-child(3) { width: 55%; }

	.worth-table__empty {
		display: grid;
		gap: 0.25rem;
		padding: 0.85rem 0.25rem;
		color: var(--text-muted);
	}

	.worth-table__empty strong { color: var(--text-primary); font-size: var(--text-sm); }
	.worth-table__empty span { font-size: var(--text-xs); line-height: 1.35; }

	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	@keyframes worth-skeleton {
		0% { background-position: 120% 0; }
		100% { background-position: -120% 0; }
	}

	@media (max-width: 740px) {
		.worth-band { padding: 0.7rem; }
		.worth-table { min-width: 780px; }
		.worth-table th, .worth-table td { padding: 0.58rem; }
		.worth-table th:nth-child(1), .worth-table td:nth-child(1) { width: 43%; }
		.worth-table th:nth-child(4), .worth-table td:nth-child(4) { width: 29%; }
	}
</style>
