<script lang="ts">
	import { replaceState } from '$app/navigation';
	import { page } from '$app/stores';
	import { get } from 'svelte/store';
	import { onDestroy, onMount, tick } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { attentionPromptStore, resolveAttentionPrompt } from '$lib/stores/attentionPromptStore';
	import { attentionStore } from '$lib/stores/attentionStore';
	import { ensurePendingHitlBridge } from '$lib/stores/pendingHitlStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		OVERLAY_IDS,
		OVERLAY_PRIORITIES,
		focusedOverlay,
		release,
		requestFocus
	} from '$lib/shell/overlayCoordinator';
	import AttentionInboxSurface from './AttentionInboxSurface.svelte';
	import AttentionCategoryTabs from './AttentionCategoryTabs.svelte';
	import {
		attentionFeedCanLoadMore,
		attentionFeedLaneNeedsAdvance,
		attentionFrontierSignature,
		attentionItemLaunchFailure,
		attentionKnownTotal,
		attentionSourceFrontiers,
		compactAttentionFeedback,
		planAttentionNextPage
	} from './centerOrchestration';
	import {
		attentionCenterState,
		clearAttentionSelection,
		closeAttentionCenter,
		findAttentionRowByAlias,
		openAttentionItem,
		returnToAttentionCenter,
		syncAttentionCenterUrl
	} from './centerState';
	import { createAttentionItemController } from './controller';
	import {
		attentionRows,
		attentionCategoryCountsFromTotals,
		attentionFeedRow,
		attentionRowAliases,
		filterAttentionRowsByCategory,
		mergeAttentionRows,
		type AttentionCategory,
		type AttentionDisplayRow,
		type SkillEvolutionGateAction,
		type SkillEvolutionRollbackDecision
	} from './model';
	import {
		ATTENTION_CENTER_PAGE_SIZE,
		attentionAggregateBufferCapped,
		attentionFrontiersBlockedByCap,
		attentionFrontiersProven,
		attentionGloballyProvenRowCount,
		attentionJumpLandingIndex,
		attentionPageRows,
		attentionPagerView,
		clampAttentionPageIndex,
		createBalancedLifecycle,
		createGenerationGuard,
		createSerializedCursorLoader,
		planAttentionReviewHistory
	} from './pagination';

	const MAX_FEED_ITEMS = 200;
	const MAX_FRONTIER_LOADS = 80;
	const FOCUSABLE_SELECTOR = [
		'a[href]',
		'button:not([disabled])',
		'input:not([disabled])',
		'select:not([disabled])',
		'textarea:not([disabled])',
		'[tabindex]:not([tabindex="-1"])'
	].join(',');

	type ItemLaunchState = 'idle' | 'hydrating' | 'opening' | 'not-found' | 'load-error';

	const itemController = createAttentionItemController();
	const itemControllerState = itemController.state;
	const resolutionJournal = attentionStore.resolutions;
	const storeLifecycle = createBalancedLifecycle(
		() => attentionStore.start(),
		() => attentionStore.stop()
	);
	const sessionGenerations = createGenerationGuard();

	let mounted = false;
	let sessionActive = false;
	let sessionLoading = false;
	let sessionReady: { generation: number; promise: Promise<void> } | null = null;
	let sessionScopeKey = '';
	let dialogEl: HTMLElement | null = null;
	let returnFocusEl: HTMLElement | null = null;
	let previousBodyOverflow = '';
	let pageIndex = 0;
	let activeCategory: AttentionCategory = 'all';
	let pageLoading = false;
	let pageLoadingOwner: { generation: number; scopeKey: string } | null = null;
	let chronologyReady = false;
	let pagingNotice: string | null = null;
	let pagingAnnouncement = '';
	let launchedSelectionKey = '';
	let launchToken = 0;
	let activeSelectionId: string | null = null;
	let activeSelectionScope = '';
	let activeSelectionDetached = false;
	let activeSelectionAliases = new Set<string>();
	let activeSelectionResolutionRevision = 0;
	let promptRequestIdAtSelection: number | null = null;
	let launchedPromptRequestId: number | null = null;
	let itemLaunchState: ItemLaunchState = 'idle';

	$: centerOpen = $attentionCenterState.open;
	$: selectedItemId = $attentionCenterState.itemId;
	$: nativeAttentionWindow = $page.url.searchParams.get('native_attention') === '1';
	// `centerOpen` drives the session/item machinery (always-mounted component);
	// `listVisible` drives whether the LIST modal renders. A direct single-item
	// open has `centerOpen` (to resolve the item + open its prompt) WITHOUT
	// `listVisible`, so no list shows behind it and there's no open/close flash.
	$: listVisible = $attentionCenterState.listRequested;
	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: feedRows = $attentionRows.slice(0, MAX_FEED_ITEMS);
	$: combinedRows = feedRows;
	$: categoryRows = filterAttentionRowsByCategory(combinedRows, activeCategory);
	$: sourceFrontiers = attentionSourceFrontiers(
		$attentionStore,
		activeCategory,
		MAX_FEED_ITEMS
	);
	$: provenRowCount = attentionGloballyProvenRowCount(sourceFrontiers, categoryRows.length);
	$: provenRows = categoryRows.slice(0, provenRowCount);
	$: visibleRows = chronologyReady ? attentionPageRows(provenRows, pageIndex) : [];
	$: canLoadMore = attentionFeedCanLoadMore($attentionStore, activeCategory, MAX_FEED_ITEMS);
	$: nextPageStart = (pageIndex + 1) * ATTENTION_CENTER_PAGE_SIZE;
	$: nextPageBlockedByCap = attentionFrontiersBlockedByCap(
		sourceFrontiers,
		nextPageStart + 1
	);
	$: feedAggregateCapped = attentionAggregateBufferCapped(
		$attentionRows.length,
		feedRows.length,
		MAX_FEED_ITEMS
	);
	$: cappedMoreAvailable =
		feedAggregateCapped ||
		sourceFrontiers.some((frontier) =>
			frontier.hasMore && frontier.loadedCount >= frontier.bufferLimit
		);
	$: capacityNotice = cappedMoreAvailable
		? 'More items are available beyond the compact chronology limit. Open full Attention to continue.'
		: null;
	$: activeChildOverlay = $focusedOverlay?.id === OVERLAY_IDS.attentionPrompt;
	$: canGoNext =
		chronologyReady &&
		!nextPageBlockedByCap &&
		(nextPageStart < provenRows.length || canLoadMore);
	// The compact chronology holds a bounded buffer, so pages beyond the cap are
	// unreachable here by design — advertise only what this surface can serve and
	// let `capacityNotice` point the rest at full Attention.
	$: centerPager = attentionPagerView({
		pageIndex,
		pageSize: ATTENTION_CENTER_PAGE_SIZE,
		visibleRowCount: visibleRows.length,
		provenRowCount: provenRows.length,
		categoryTotal: categoryCounts[activeCategory] ?? 0,
		hasServerMore: canLoadMore,
		maxReachablePage: nextPageBlockedByCap ? pageIndex + 1 : undefined
	});
	$: allKnownTotal = attentionKnownTotal(
		combinedRows.length,
		$attentionRows.length,
		$attentionStore.counts.needs_action,
		$attentionStore.counts.failed
	);
	$: categoryCounts = attentionCategoryCountsFromTotals($attentionStore.totals);
	$: feedback = compactAttentionFeedback(
		$itemControllerState.error,
		$itemControllerState.notice,
		pagingNotice,
		capacityNotice
	);

	const cursorLoader = createSerializedCursorLoader(
		() => attentionFeedCanLoadMore(get(attentionStore), activeCategory, MAX_FEED_ITEMS),
		loadNextCursorPages,
		() => sessionGenerations.current()
	);

	$: if (mounted) {
		if (centerOpen && !sessionActive) {
			void startSession();
		} else if (!centerOpen && sessionActive) {
			stopSession();
		}
	}

	$: if (mounted && sessionActive && scopeKey !== sessionScopeKey) {
		void resetForScope(scopeKey);
	}

	$: if (mounted && sessionActive && selectedItemId) {
		void launchSelectedItem(selectedItemId, scopeKey);
	}

	$: if (!selectedItemId && itemLaunchState !== 'idle') {
		itemLaunchState = 'idle';
		launchedSelectionKey = '';
	}

	$: if (sessionActive) {
		const clampedPage = clampAttentionPageIndex(pageIndex, provenRows.length);
		if (clampedPage !== pageIndex) {
			pageIndex = clampedPage;
			pagingAnnouncement = `Page ${pageIndex + 1} loaded.`;
		}
	}

	$: if (activeSelectionId && $attentionPromptStore.request) {
		observeLaunchedPromptRequest($attentionPromptStore.request.id);
	}

	$: if (activeSelectionId && !$attentionPromptStore.request) {
		launchedPromptRequestId = null;
	}

	$: if (
		activeSelectionId &&
		$attentionPromptStore.request &&
		launchedPromptRequestId === $attentionPromptStore.request.id &&
		(activeSelectionScope !== scopeKey ||
			selectedItemId !== activeSelectionId ||
			(!activeSelectionDetached && !findAttentionRowByAlias(combinedRows, activeSelectionId)))
	) {
		cancelLaunchedPrompt();
	}

	$: if (
		activeSelectionId &&
		launchedPromptRequestId !== null &&
		$resolutionJournal.notices.some(
			(notice) =>
				notice.revision > activeSelectionResolutionRevision &&
				activeSelectionAliases.has(notice.correlationId)
		)
	) {
		// Detached exact rows are not present in the capped feed state, so a
		// cross-surface HitlResolved cannot remove their row. The lifecycle
		// journal carries that resolution independently and closes the same
		// singleton prompt before it can submit a stale response.
		cancelLaunchedPrompt();
	}

	function sessionIsCurrent(generation: number, requestedScope: string): boolean {
		return sessionActive &&
			sessionGenerations.isCurrent(generation) &&
			requestedScope === sessionScopeKey &&
			requestedScope === scopeKey;
	}

	function allSourceFrontiersProven(requiredRows: number): boolean {
		return attentionFrontiersProven(
			attentionSourceFrontiers(get(attentionStore), activeCategory, MAX_FEED_ITEMS),
			requiredRows
		);
	}

	function sourceFrontierBlockedByCap(requiredRows: number): boolean {
		return attentionFrontiersBlockedByCap(
			attentionSourceFrontiers(get(attentionStore), activeCategory, MAX_FEED_ITEMS),
			requiredRows
		);
	}

	async function loadNextCursorPages(): Promise<void> {
		const generation = sessionGenerations.current();
		const requestedScope = sessionScopeKey;
		if (!sessionIsCurrent(generation, requestedScope)) return;
		const before = currentCombinedRows().length;
		if (!attentionFeedCanLoadMore(get(attentionStore), activeCategory, MAX_FEED_ITEMS)) {
			return;
		}
		await attentionStore.loadMore();
		if (!sessionIsCurrent(generation, requestedScope)) return;
		const attentionError = get(attentionStore).error;
		if (attentionError) pagingNotice = `Attention feed unavailable: ${attentionError}`;
		if (currentCombinedRows().length === before && attentionError) {
			throw new Error(attentionError || 'Cursor page did not load.');
		}
	}

	function currentCombinedRows(): AttentionDisplayRow[] {
		return mergeAttentionRows(get(attentionRows).slice(0, MAX_FEED_ITEMS), []);
	}

	function currentCategoryRows(): AttentionDisplayRow[] {
		return filterAttentionRowsByCategory(currentCombinedRows(), activeCategory);
	}

	async function advanceGlobalFrontiers(
		requiredRows: number,
		generation: number,
		requestedScope: string
	): Promise<void> {
		let attempts = 0;
		while (
			sessionIsCurrent(generation, requestedScope) &&
			!allSourceFrontiersProven(requiredRows) &&
			attempts < MAX_FRONTIER_LOADS
		) {
			const before = attentionFrontierSignature(
				get(attentionStore),
				activeCategory
			);
			const state = get(attentionStore);
			if (
				attentionFeedLaneNeedsAdvance(
					state,
					requiredRows,
					activeCategory,
					MAX_FEED_ITEMS
				) && attentionFeedCanLoadMore(state, activeCategory, MAX_FEED_ITEMS)
			) {
				await attentionStore.loadMore();
				if (!sessionIsCurrent(generation, requestedScope)) return;
			}
			attempts += 1;
			if (
				attentionFrontierSignature(
					get(attentionStore),
					activeCategory
				) === before
			) break;
		}
	}

	async function ensureRowsForWindow(
		requiredRows: number,
		generation = sessionGenerations.current(),
		requestedScope = sessionScopeKey
	): Promise<boolean> {
		let proofTarget = requiredRows;
		let attempts = 0;
		while (sessionIsCurrent(generation, requestedScope) && attempts < MAX_FRONTIER_LOADS) {
			await advanceGlobalFrontiers(proofTarget, generation, requestedScope);
			if (!sessionIsCurrent(generation, requestedScope)) return false;
			const rowCount = currentCategoryRows().length;
			if (rowCount >= requiredRows && allSourceFrontiersProven(requiredRows)) return true;
			if (sourceFrontierBlockedByCap(proofTarget)) return false;
			if (!moreSourcesAvailable()) return allSourceFrontiersProven(rowCount);
			proofTarget += ATTENTION_CENTER_PAGE_SIZE;
			attempts += 1;
		}
		return false;
	}

	function moreSourcesAvailable(category: AttentionCategory = activeCategory): boolean {
		return attentionFeedCanLoadMore(get(attentionStore), category, MAX_FEED_ITEMS);
	}

	function observeLaunchedPromptRequest(requestId: number): void {
		if (requestId === promptRequestIdAtSelection) return;
		launchedPromptRequestId = requestId;
	}

	function cancelLaunchedPrompt(): void {
		const request = get(attentionPromptStore).request;
		if (request && request.id === launchedPromptRequestId) {
			resolveAttentionPrompt(null);
		}
		launchedPromptRequestId = null;
	}

	function beginSessionHydration(generation: number, requestedScope: string): Promise<void> {
		sessionLoading = true;
		let promise: Promise<void>;
		promise = (async () => {
			await attentionStore.refresh();
			if (!sessionIsCurrent(generation, requestedScope)) return;
			await ensureRowsForWindow(ATTENTION_CENTER_PAGE_SIZE, generation, requestedScope);
			if (sessionIsCurrent(generation, requestedScope)) chronologyReady = true;
		})()
			.catch((error) => {
				if (!sessionIsCurrent(generation, requestedScope)) return;
				pagingNotice = error instanceof Error ? error.message : 'Attention inbox failed to load.';
				chronologyReady = true;
			})
			.finally(() => {
				if (
					sessionReady?.generation === generation &&
					sessionReady.promise === promise &&
					sessionIsCurrent(generation, requestedScope)
				) {
					sessionLoading = false;
				}
			});
		sessionReady = { generation, promise };
		return promise;
	}

	async function startSession(): Promise<void> {
		if (sessionActive) return;
		sessionActive = true;
		sessionScopeKey = scopeKey;
		const generation = sessionGenerations.advance();
		const requestedScope = sessionScopeKey;
		pageIndex = 0;
		activeCategory = 'all';
		pageLoading = false;
		pageLoadingOwner = null;
		chronologyReady = false;
		pagingNotice = null;
		pagingAnnouncement = '';
		returnFocusEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		const focusGranted = requestFocus({
			id: OVERLAY_IDS.attentionCenter,
			priority: OVERLAY_PRIORITIES.attentionCenter,
			allowedChildOverlayIds: [OVERLAY_IDS.attentionPrompt],
			onClose: closeAttentionCenter
		});
		if (!focusGranted) {
			sessionActive = false;
			sessionGenerations.advance();
			sessionLoading = false;
			return;
		}
		previousBodyOverflow = document.body.style.overflow;
		document.body.style.overflow = 'hidden';
		ensurePendingHitlBridge();
		storeLifecycle.setActive(true);
		beginSessionHydration(generation, requestedScope);
		await focusDialog();
	}

	function stopSession(): void {
		if (!sessionActive) return;
		sessionActive = false;
		sessionGenerations.advance();
		launchToken += 1;
		activeSelectionId = null;
		activeSelectionScope = '';
		activeSelectionDetached = false;
		activeSelectionAliases = new Set();
		activeSelectionResolutionRevision = 0;
		promptRequestIdAtSelection = null;
		launchedSelectionKey = '';
		itemLaunchState = 'idle';
		pageLoading = false;
		pageLoadingOwner = null;
		cancelLaunchedPrompt();
		release(OVERLAY_IDS.attentionCenter);
		storeLifecycle.setActive(false);
		chronologyReady = false;
		sessionLoading = false;
		sessionReady = null;
		document.body.style.overflow = previousBodyOverflow;
		const target = returnFocusEl;
		returnFocusEl = null;
		void tick().then(() => {
			const fallback = document.querySelector<HTMLElement>(
				'[data-attention-trigger], main:not([inert]) button:not([disabled]), main:not([inert]) a[href]'
			);
			const candidate = target?.isConnected && !target.closest('[inert]') ? target : fallback;
			if (candidate?.isConnected && !candidate.closest('[inert]')) candidate.focus();
		});
	}

	async function resetForScope(nextScopeKey: string): Promise<void> {
		const generation = sessionGenerations.advance();
		sessionScopeKey = nextScopeKey;
		launchToken += 1;
		activeSelectionId = null;
		activeSelectionScope = '';
		promptRequestIdAtSelection = null;
		launchedSelectionKey = '';
		itemLaunchState = 'idle';
		cancelLaunchedPrompt();
		if (selectedItemId) returnToAttentionCenter('replace');
		pageIndex = 0;
		pageLoading = false;
		pageLoadingOwner = null;
		chronologyReady = false;
		await beginSessionHydration(generation, nextScopeKey);
	}

	async function focusDialog(): Promise<void> {
		await tick();
		dialogEl?.focus();
	}

	async function consumeReviewHistoryEntry(): Promise<void> {
		const currentPage = get(page);
		const plan = planAttentionReviewHistory(currentPage.url, currentPage.state);
		if (plan.kind === 'replace') {
			replaceState(plan.url, plan.state);
			syncAttentionCenterUrl(plan.url);
			return;
		}
		await new Promise<void>((resolve) => {
			window.addEventListener('popstate', () => resolve(), { once: true });
			window.history.back();
		});
	}

	function trapFocus(event: KeyboardEvent): void {
		if (
			!centerOpen ||
			$focusedOverlay?.id !== OVERLAY_IDS.attentionCenter ||
			event.key !== 'Tab' ||
			!dialogEl
		) return;
		const focusable = Array.from(dialogEl.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
			(element) => element.offsetParent !== null && !element.closest('[inert]')
		);
		if (focusable.length === 0) {
			event.preventDefault();
			dialogEl.focus();
			return;
		}
		const first = focusable[0];
		const last = focusable[focusable.length - 1];
		const active = document.activeElement;
		if (!dialogEl.contains(active) || active === dialogEl) {
			event.preventDefault();
			(event.shiftKey ? last : first).focus();
		} else if (event.shiftKey && active === first) {
			event.preventDefault();
			last.focus();
		} else if (!event.shiftKey && document.activeElement === last) {
			event.preventDefault();
			first.focus();
		}
	}

	function recaptureFocus(event: FocusEvent): void {
		if (
			!centerOpen ||
			$focusedOverlay?.id !== OVERLAY_IDS.attentionCenter ||
			!dialogEl ||
			dialogEl.contains(event.target as Node | null)
		) return;
		dialogEl.focus();
	}

	async function launchSelectedItem(itemId: string, launchScope: string): Promise<void> {
		const selectionKey = `${launchScope}:${itemId}`;
		if (launchedSelectionKey === selectionKey) return;
		if (activeCategory !== 'all') {
			activeCategory = 'all';
			pageIndex = 0;
		}
		launchedSelectionKey = selectionKey;
		const token = ++launchToken;
		const selectionResolutionRevision = get(resolutionJournal).revision;
		itemLaunchState = 'hydrating';
		const ready = sessionReady;
		if (ready) await ready.promise;
		if (token !== launchToken || launchScope !== scopeKey || selectedItemId !== itemId) return;

		let row = findAttentionRowByAlias(currentCombinedRows(), itemId);
		let resolvedOutsideCurrentRows = false;
		try {
			if (!row) {
				const exact = await attentionStore.fetchItem(itemId);
				if (exact) {
					row = attentionFeedRow(exact);
					resolvedOutsideCurrentRows = true;
				}
			}
		} catch (error) {
			if (token !== launchToken || launchScope !== scopeKey) return;
			pagingNotice = error instanceof Error ? error.message : 'Attention item lookup failed.';
			itemLaunchState = 'load-error';
			// A direct single-item open (no list) that errored must not strand the
			// user behind an invisible frozen overlay — clear the selection.
			if (!listVisible) clearAttentionSelection();
			return;
		}

		if (token !== launchToken || launchScope !== scopeKey || selectedItemId !== itemId) return;
		if (!row) {
			itemLaunchState = attentionItemLaunchFailure(get(attentionStore).error);
			// A DIRECT single-item open (no list shown) that can't resolve must not
			// strand the user behind an invisible, click-blocking frozen overlay —
			// clear the selection so the center releases focus + unfreezes the body.
			if (!listVisible) clearAttentionSelection();
			return;
		}

		itemLaunchState = 'opening';
		if (row.review_href) await consumeReviewHistoryEntry();
		activeSelectionId = itemId;
		activeSelectionScope = launchScope;
		// An exact deep link may intentionally target an item beyond the capped
		// inbox page. Keep that prompt alive even though the row is not present in
		// `combinedRows`; list-backed selections still cancel if realtime removes
		// their row while the prompt is open.
		activeSelectionDetached = resolvedOutsideCurrentRows;
		activeSelectionAliases = attentionRowAliases(row);
		activeSelectionResolutionRevision = selectionResolutionRevision;
		promptRequestIdAtSelection = get(attentionPromptStore).request?.id ?? null;
		launchedPromptRequestId = null;
		const activation = itemController.activate(row);
		const immediatePrompt = get(attentionPromptStore).request;
		if (immediatePrompt) observeLaunchedPromptRequest(immediatePrompt.id);
		const result = await activation;
		activeSelectionId = null;
		activeSelectionScope = '';
		activeSelectionDetached = false;
		activeSelectionAliases = new Set();
		activeSelectionResolutionRevision = 0;
		promptRequestIdAtSelection = null;
		launchedPromptRequestId = null;
		if (result.status === 'opened' && row.review_href) return;
		if (token !== launchToken || launchScope !== scopeKey) return;
		if (result.status === 'stale') {
			pagingNotice = 'That attention item changed scope or was resolved elsewhere.';
		}
		returnToAttentionCenter();
		await focusDialog();
	}

	function activateRow(event: CustomEvent<{ row: AttentionDisplayRow }>): void {
		openAttentionItem(event.detail.row.key);
	}

	async function runSkillAction(
		event: CustomEvent<{ row: AttentionDisplayRow; action: SkillEvolutionGateAction }>
	): Promise<void> {
		await itemController.runSkillEvolutionGate(event.detail.row, event.detail.action);
	}

	async function runRollbackDecision(
		event: CustomEvent<{
			row: AttentionDisplayRow;
			decision: SkillEvolutionRollbackDecision;
		}>
	): Promise<void> {
		await itemController.runRollbackRecommendationDecision(
			event.detail.row,
			event.detail.decision
		);
	}

	function retrySelectedItem(): void {
		if (!selectedItemId || itemLaunchState !== 'load-error') return;
		launchedSelectionKey = '';
		pagingNotice = null;
		void launchSelectedItem(selectedItemId, scopeKey);
	}

	async function selectCategory(event: CustomEvent<{ category: AttentionCategory }>): Promise<void> {
		const category = event.detail.category;
		if (category === activeCategory || pageLoading) return;
		activeCategory = category;
		pageIndex = 0;
		pagingNotice = null;
		pagingAnnouncement = '';
		chronologyReady = false;
		pageLoading = true;
		const generation = sessionGenerations.current();
		const requestedScope = sessionScopeKey;
		try {
			await ensureRowsForWindow(ATTENTION_CENTER_PAGE_SIZE, generation, requestedScope);
		} catch (error) {
			if (sessionIsCurrent(generation, requestedScope)) {
				pagingNotice = error instanceof Error ? error.message : 'Attention category failed to load.';
			}
		} finally {
			if (sessionIsCurrent(generation, requestedScope)) {
				chronologyReady = true;
				pageLoading = false;
				pagingAnnouncement = `${categoryCounts[category]} items in this category.`;
				await focusDialog();
			}
		}
	}

	/**
	 * Jump to any 1-based page of the active tab. Backwards is buffered and free;
	 * forwards runs the same frontier advance Next always did, just aimed further
	 * out, so First/Last stay honest against the cursor sources.
	 */
	async function goToPage(target: number): Promise<void> {
		const targetPage = Math.max(0, Math.floor(target) - 1);
		if (targetPage === pageIndex || pageLoading) return;
		if (targetPage < pageIndex) {
			pageIndex = targetPage;
			pagingNotice = null;
			pagingAnnouncement = `Page ${pageIndex + 1} loaded.`;
			void focusPaginationControl('previous');
			return;
		}
		if (!canGoNext) return;
		const generation = sessionGenerations.current();
		const requestedScope = sessionScopeKey;
		const owner = { generation, scopeKey: requestedScope };
		pageLoadingOwner = owner;
		pageLoading = true;
		pagingNotice = null;
		const targetLength = (targetPage + 1) * ATTENTION_CENTER_PAGE_SIZE;
		try {
			await ensureRowsForWindow(targetLength, generation, requestedScope);
		} catch (error) {
			if (sessionIsCurrent(generation, requestedScope) && pageLoadingOwner === owner) {
				pagingNotice = error instanceof Error ? error.message : 'Attention page failed to load.';
			}
		}
		if (!sessionIsCurrent(generation, requestedScope) || pageLoadingOwner !== owner) return;
		const rows = currentCategoryRows();
		const safeRowCount = attentionGloballyProvenRowCount(
			attentionSourceFrontiers(get(attentionStore), activeCategory, MAX_FEED_ITEMS),
			rows.length
		);
		const plan = planAttentionNextPage(
			targetPage,
			ATTENTION_CENTER_PAGE_SIZE,
			safeRowCount,
			sourceFrontierBlockedByCap(targetPage * ATTENTION_CENTER_PAGE_SIZE + 1),
			moreSourcesAvailable()
		);
		// A multi-page jump that outran the frontiers still lands on the furthest
		// proven page instead of stranding the user where they clicked from.
		const landedIndex =
			plan.pageIndex ??
			attentionJumpLandingIndex(
				pageIndex,
				targetPage,
				safeRowCount,
				ATTENTION_CENTER_PAGE_SIZE
			);
		if (landedIndex !== pageIndex) {
			pageIndex = landedIndex;
			pagingAnnouncement = `Page ${pageIndex + 1} loaded.`;
		}
		if (plan.notice) pagingNotice = plan.notice;
		pageLoadingOwner = null;
		pageLoading = false;
		await focusPaginationControl('next');
	}

	async function focusPaginationControl(direction: 'previous' | 'next'): Promise<void> {
		await tick();
		const label = direction === 'previous' ? 'Previous page' : 'Next page';
		const preferred = dialogEl?.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
		if (preferred && !preferred.disabled) preferred.focus();
		else dialogEl?.focus();
	}

	function closeFromBackdrop(event: MouseEvent): void {
		if (event.target === event.currentTarget && $focusedOverlay?.id === OVERLAY_IDS.attentionCenter) {
			closeAttentionCenter();
		}
	}

	onMount(() => {
		mounted = true;
		if (centerOpen) void startSession();
	});

	onDestroy(() => {
		mounted = false;
		if (sessionActive) stopSession();
		storeLifecycle.destroy();
	});
</script>

<svelte:window on:keydown={trapFocus} />
<svelte:document on:focusin={recaptureFocus} />

{#if listVisible}
	<div
		class="attention-center__backdrop global-layer-attention-center"
		on:click={closeFromBackdrop}
		aria-hidden={activeChildOverlay ? 'true' : undefined}
	>
		<div
			class="attention-center"
			class:attention-center--child-open={activeChildOverlay}
			role="dialog"
			aria-modal="true"
			aria-labelledby="attention-center-title"
			tabindex="-1"
			inert={activeChildOverlay}
			data-attention-center-dialog
			bind:this={dialogEl}
		>
			<header class="attention-center__header">
				<div class="attention-center__title-group">
					<span class="attention-center__title-icon"><Icon name="inbox" size={17} /></span>
					<div>
						<h2 id="attention-center-title">Attention</h2>
						<p>{allKnownTotal} {allKnownTotal === 1 ? 'item' : 'items'} waiting</p>
					</div>
				</div>
				<button
					type="button"
					class="attention-center__icon-button"
					on:click={closeAttentionCenter}
					aria-label="Close Attention"
					title="Close Attention"
				>
					<Icon name="x" size={17} />
				</button>
			</header>

			<div class="attention-center__tabs">
				<AttentionCategoryTabs
					active={activeCategory}
					counts={categoryCounts}
					disabled={pageLoading || sessionLoading}
					on:change={selectCategory}
				/>
			</div>

			<div class="attention-center__body">
				{#if selectedItemId && (itemLaunchState === 'not-found' || itemLaunchState === 'load-error')}
					<div class="attention-center__stale" role="status">
						<h3>{itemLaunchState === 'load-error' ? 'Item could not be loaded' : 'Item no longer available'}</h3>
						<p>
							{itemLaunchState === 'load-error'
								? pagingNotice ?? 'The Attention service is unavailable.'
								: 'It may have been resolved, removed, or belong to a different workspace.'}
						</p>
						<div class="attention-center__stale-actions">
							{#if itemLaunchState === 'load-error'}
								<button type="button" on:click={retrySelectedItem}>Retry</button>
							{/if}
							<button type="button" on:click={() => returnToAttentionCenter()}>
								Return to Attention
							</button>
						</div>
					</div>
				{:else if selectedItemId && (itemLaunchState === 'hydrating' || itemLaunchState === 'opening')}
					<div class="attention-center__launching" role="status">
						<span class="attention-center__spinner"></span>
						<span>{itemLaunchState === 'hydrating' ? 'Finding attention item...' : 'Opening prompt...'}</span>
					</div>
				{:else}
					<AttentionInboxSurface
						rows={visibleRows}
						pageLimit={ATTENTION_CENTER_PAGE_SIZE}
						initialLoading={sessionLoading || ($attentionStore.isLoading && combinedRows.length === 0)}
						emptyError={$attentionStore.error}
						{feedback}
						hydratingKey={$itemControllerState.hydratingKey}
						skillEvolutionActionKey={$itemControllerState.skillEvolutionActionKey}
						rollbackActionKey={$itemControllerState.rollbackActionKey}
						showSearch={false}
						showFilters={false}
						skeletonRows={ATTENTION_CENTER_PAGE_SIZE}
						on:activate={activateRow}
						on:skillaction={runSkillAction}
						on:rollbackdecision={runRollbackDecision}
					/>
				{/if}
			</div>

			<footer class="attention-center__footer">
				{#if !nativeAttentionWindow}
				<a class="attention-center__full-link" href="/attention">
					{cappedMoreAvailable ? 'More available in full Attention' : 'Open full Attention'}
				</a>
				{/if}
				<ServerPager
					currentPage={centerPager.currentPage}
					pageCount={centerPager.pageCount}
					startItem={centerPager.startItem}
					endItem={centerPager.endItem}
					totalItems={centerPager.totalItems}
					loading={pageLoading || sessionLoading || !chronologyReady}
					ariaLabel="Attention pages"
					on:pagechange={(event) => void goToPage(event.detail.page)}
				/>
				<span class="attention-center__sr-only" aria-live="polite" aria-atomic="true">
					{pagingAnnouncement}
				</span>
			</footer>
		</div>
	</div>
{/if}

<style>
	.attention-center__backdrop {
		position: fixed;
		inset: 0;
		z-index: 1200;
		display: grid;
		place-items: center;
		padding: 24px;
		background: rgba(8, 10, 12, 0.64);
	}

	.attention-center {
		width: min(560px, calc(100vw - 32px));
		height: min(680px, calc(100vh - 48px));
		max-height: calc(100vh - 48px);
		display: flex;
		flex-direction: column;
		border: 1px solid var(--border-default, #424a4f);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-elevated, #ffffff);
		color: var(--text-primary, #2d3436);
		box-shadow: 0 22px 70px rgba(0, 0, 0, 0.34);
		overflow: hidden;
	}

	.attention-center:focus {
		outline: none;
	}

	.attention-center--child-open {
		pointer-events: none;
	}

	.attention-center__header,
	.attention-center__footer {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		background: var(--bg-elevated, #ffffff);
	}

	.attention-center__header {
		padding: 14px 16px;
		border-bottom: 1px solid var(--border-soft, #eee4dc);
	}

	.attention-center__title-group {
		display: flex;
		align-items: center;
		gap: 10px;
		min-width: 0;
	}

	.attention-center__title-icon {
		width: 32px;
		height: 32px;
		display: inline-grid;
		place-items: center;
		flex: 0 0 auto;
		border-radius: var(--radius-md, 8px);
		background: var(--accent-primary-soft, rgba(255, 107, 107, 0.12));
		color: var(--accent-primary, #ff6b6b);
	}

	.attention-center h2,
	.attention-center h3,
	.attention-center p {
		margin: 0;
	}

	.attention-center h2 {
		font-size: var(--text-md, 0.95rem);
		font-weight: 700;
		letter-spacing: 0;
	}

	.attention-center__title-group p {
		margin-top: 2px;
		font-size: var(--text-xs, 0.78rem);
		color: var(--text-secondary, #5f6769);
	}

	.attention-center__body {
		flex: 1 1 auto;
		min-height: 0;
		padding: 12px;
		overflow-y: auto;
		background: var(--bg-soft, #f6f1e8);
	}

	.attention-center__tabs {
		flex: 0 0 auto;
		padding: 0 12px;
		background: var(--bg-elevated, #fff);
	}

	.attention-center__footer {
		padding: 10px 12px 10px 16px;
		border-top: 1px solid var(--border-soft, #eee4dc);
	}

	.attention-center__full-link {
		color: var(--accent-primary, #ff6b6b);
		font-size: var(--text-sm, 0.85rem);
		font-weight: 650;
		text-decoration: none;
	}

	/* The shared pager assumes a full-width page footer; inside a 560px dialog it
	   has to share the row with the full-Attention link without wrapping. */
	.attention-center__footer :global(.server-pager-shell) {
		flex: 0 1 auto;
		min-width: 0;
		padding: 0;
	}

	.attention-center__full-link:hover {
		text-decoration: underline;
	}

	.attention-center__icon-button {
		width: 32px;
		height: 32px;
		display: inline-grid;
		place-items: center;
		padding: 0;
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card, #ffffff);
		color: var(--text-primary, #2d3436);
		cursor: pointer;
	}

	.attention-center__icon-button:hover:not(:disabled) {
		border-color: var(--border-default, #ddd3ca);
		background: var(--bg-soft, #f6f1e8);
	}

	.attention-center__icon-button:focus-visible,
	.attention-center__full-link:focus-visible,
	.attention-center__stale-actions button:focus-visible {
		outline: 2px solid var(--accent-primary, #ff6b6b);
		outline-offset: 2px;
	}

	.attention-center__icon-button:disabled {
		cursor: default;
		opacity: 0.42;
	}

	.attention-center__launching,
	.attention-center__stale {
		min-height: 220px;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.attention-center__launching {
		gap: 10px;
		color: var(--text-secondary, #5f6769);
		font-size: var(--text-sm, 0.85rem);
	}

	.attention-center__stale {
		flex-direction: column;
		gap: 8px;
		padding: 28px;
		text-align: center;
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card, #ffffff);
	}

	.attention-center__stale h3 {
		font-size: var(--text-md, 0.95rem);
		letter-spacing: 0;
	}

	.attention-center__stale p {
		max-width: 380px;
		font-size: var(--text-sm, 0.85rem);
		line-height: 1.5;
		color: var(--text-secondary, #5f6769);
	}

	.attention-center__stale-actions {
		display: flex;
		align-items: center;
		justify-content: center;
		flex-wrap: wrap;
		gap: 12px;
		margin-top: 8px;
	}

	.attention-center__stale-actions button {
		min-height: 34px;
		padding: 0 12px;
		border: 1px solid var(--border-default, #ddd3ca);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-elevated, #ffffff);
		color: var(--text-primary, #2d3436);
		font: inherit;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 650;
		cursor: pointer;
	}

	.attention-center__spinner {
		width: 16px;
		height: 16px;
		border: 2px solid var(--border-default, #ddd3ca);
		border-top-color: var(--accent-primary, #ff6b6b);
		border-radius: 50%;
		animation: attention-center-spin 700ms linear infinite;
	}

	@keyframes attention-center-spin {
		to { transform: rotate(360deg); }
	}

	.attention-center__body :global(.attention-page__list) {
		gap: 6px;
	}

	.attention-center__body :global(.attention-page__row) {
		border-radius: var(--radius-md, 8px);
		box-shadow: none;
	}

	.attention-center__body :global(.attention-page__row-btn) {
		grid-template-columns: auto minmax(0, 1fr) auto;
		gap: 9px;
		padding: 10px;
	}

	.attention-center__body :global(.attention-page__source) {
		max-width: 92px;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.attention-center__body :global(.attention-page__prompt) {
		font-size: var(--text-sm, 0.85rem);
	}

	.attention-center__body :global(.attention-page__hint) {
		font-size: var(--text-xs, 0.78rem);
	}

	.attention-center__body :global(.attention-page__row-actions) {
		padding: 0 10px 10px;
	}

	.attention-center__body :global(.attention-page__empty) {
		padding: 34px 12px;
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card, #ffffff);
	}

	.attention-center__sr-only {
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

	@media (max-width: 640px) {
		.attention-center__backdrop {
			align-items: end;
			padding: 12px;
		}

		.attention-center {
			width: 100%;
			height: min(680px, calc(100vh - 24px));
			max-height: calc(100vh - 24px);
		}

		.attention-center__body :global(.attention-page__row-btn) {
			grid-template-columns: minmax(0, 1fr) auto;
		}

		.attention-center__body :global(.attention-page__source) {
			display: none;
		}

		/* Drop the "21-40 of 137" range before the page counter or the buttons. */
		.attention-center__footer :global(.server-pager__summary span) {
			display: none;
		}

		.attention-center__stale-actions {
			width: 100%;
			align-items: stretch;
			flex-direction: column;
		}

		.attention-center__stale-actions button {
			width: 100%;
			box-sizing: border-box;
			text-align: center;
		}
	}
</style>
