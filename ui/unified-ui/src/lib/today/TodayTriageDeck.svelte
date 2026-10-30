<script context="module" lang="ts">
	import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';
	import type { ResurfacingCard } from './resurfacingQueries';
	import type { CanonicalAttentionItem } from '$lib/attention/canonicalAttentionProjection';

	export interface TriageCardData {
		id: string;
		title: string;
		category: string;
		summary: string;
		sender?: string;
		originLane?: 'dispatch' | 'reading_room';
		rawItem?: CanonicalAttentionItem;
		rawFollowUp?: ChannelFollowUp;
		rawWorth?: ResurfacingCard;
	}

	export type DeckTab = 'all' | 'for_you' | 'worth';
	/** Swipes, plus the two Act-menu entries that resolve the card in place:
	 *  `primary` is a Worth card's Open, `snooze` a follow-up's snooze. */
	export type DeckActionKind = 'useful' | 'acknowledge' | 'dismiss' | 'primary' | 'snooze';
</script>

<script lang="ts">
	import { createEventDispatcher, onDestroy, tick } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { ASSISTANT_FALLBACK_NAME } from '$lib/presentationIdentity';
	import { deckActEntries, openWorthLinkInGesture, type DeckActEntry } from './deckActMenu';
	import { readingRoomDateline } from './readingRoomMetadata';
	import TodayActMenu from './TodayActMenu.svelte';
	import TodayDeckDetail from './TodayDeckDetail.svelte';

	export let cards: TriageCardData[] = [];
	export let debug = false;
	export let activeTab: DeckTab = 'all';
	export let followUpTotal: number | null = null;
	export let worthTotal: number | null = null;
	export let hasMoreForYou = false;
	export let hasMoreWorth = false;
	export let forYouLoading = false;
	export let worthLoading = false;
	$: initialLoading = (activeTab !== 'worth' && forYouLoading) || (activeTab !== 'for_you' && worthLoading);
	export let loadingMore = false;
	export let loadError: string | null = null;
	export let pageRevision = 0;
	/** Primary agent's display name; `ask_presto` is shown as "Ask <name>". */
	export let agentName: string = ASSISTANT_FALLBACK_NAME;
	/** Existing chat thread Ask <agent> posts into; without one it is hidden. */
	export let chatThreadId: string | null = null;

	type SwipeKind = 'useful' | 'dismiss' | 'acknowledge';

	const dispatch = createEventDispatcher<{
		action: { card: TriageCardData; kind: DeckActionKind; complete?: (success: boolean) => void };
		/** The card's own detail-view action already resolved it server-side. */
		resolved: { card: TriageCardData; message: string };
		complete: void;
		needMore: DeckTab;
		switchView: void;
	}>();

	let triagedIds = new Map<string, boolean>();
    let swipeTimer: ReturnType<typeof setTimeout> | null = null;
    onDestroy(() => { if (swipeTimer !== null) clearTimeout(swipeTimer); });

	function isForYouCard(card: TriageCardData): boolean {
		return card.originLane === 'dispatch' || Boolean(card.rawFollowUp) || (card.category.toUpperCase().includes('FOR YOU') || card.category.toUpperCase().includes('DISPATCH'));
	}

	function isWorthCard(card: TriageCardData): boolean {
		return card.originLane === 'reading_room' || Boolean(card.rawWorth) || (card.category.toUpperCase().includes('WORTH A LOOK') || card.category.toUpperCase().includes('READING ROOM')) || card.category.toUpperCase().includes('NOTE') || card.category.toUpperCase().includes('MEMORY');
	}

	$: unhandledAll = cards.filter(c => !triagedIds.has(c.id));
	$: forYouCards = unhandledAll.filter(isForYouCard);
	$: worthCards = unhandledAll.filter(isWorthCard);

	$: currentStack = activeTab === 'for_you'
		? forYouCards
		: activeTab === 'worth'
			? worthCards
			: unhandledAll;

	$: triagedInTab = [...triagedIds.values()].filter(forYou => activeTab === 'all' || (activeTab === 'for_you' ? forYou : !forYou)).length;
	$: totalInitialInTab = currentStack.length + triagedInTab;

	let dragX = 0;
	let dragY = 0;
	let isDragging = false;
	let activePointerId: number | null = null;
	let pointerTravel = 0;
	// `resolve` leaves upward without a stamp: snooze is neither of the swipes.
	let animatingOut: SwipeKind | 'resolve' | null = null;
	let cardElement: HTMLElement | null = null;

	// Act menu + detail view for the top card.
	let actMenuOpen = false;
	let actMenu: TodayActMenu | null = null;
	let detailCardId: string | null = null;
	let detailRef: TodayDeckDetail | null = null;
	let detailWrapEl: HTMLElement | null = null;

	function selectTab(tab: DeckTab): void {
		if (activeTab === tab) return;
		actMenuOpen = false;
		activeTab = tab;
		dragX = 0;
		dragY = 0;
		animatingOut = null;
	}

	$: activeCard = currentStack[0] ?? null;
	$: dateline = activeCard ? readingRoomDateline(activeCard.rawFollowUp, activeCard.rawWorth, activeCard.rawItem) : null;
	$: nextCard = currentStack[1] ?? null;
	$: thirdCard = currentStack[2] ?? null;
	$: actEntries = activeCard ? deckActEntries(activeCard, { agentName, chatThreadId }) : [];
	$: hasDetails = Boolean(activeCard?.rawFollowUp || activeCard?.rawWorth);
	// The detail view follows the top card: once that card leaves (or the tab
	// changes it), the view closes rather than showing a card no longer on top.
	$: if (detailCardId && activeCard?.id !== detailCardId) detailCardId = null;
	$: if (actMenuOpen && !activeCard) actMenuOpen = false;
	$: detailCard = detailCardId !== null && activeCard?.id === detailCardId ? activeCard : null;
	$: remainingCount = currentStack.length;
	$: hasMore = activeTab === 'for_you' ? hasMoreForYou : activeTab === 'worth' ? hasMoreWorth : hasMoreForYou || hasMoreWorth;
	$: totalAvailable = activeTab === 'for_you' ? followUpTotal ?? forYouCards.length
		: activeTab === 'worth' ? worthTotal ?? worthCards.length
		: (followUpTotal ?? forYouCards.length) + (worthTotal ?? worthCards.length);
	$: isComplete = currentStack.length === 0 && totalInitialInTab > 0 && !hasMore && !initialLoading && !loadingMore;
	let requestKey: string | null = null;
	$: requestNextIfLow(activeTab, currentStack.length, hasMore, initialLoading, loadingMore, loadError, pageRevision);
	function requestNextIfLow(tab: DeckTab, count: number, more: boolean, initial: boolean, busy: boolean, error: string | null, revision: number): void {
		if (!more || initial || busy || error || count > 4) return;
		const key = `${tab}:${count}:${revision}`;
		if (requestKey === key) return;
		requestKey = key;
		dispatch('needMore', tab);
	}

	// Stamp opacities based on drag deltas; a menu-resolved exit carries none.
	$: stampScale = animatingOut === 'resolve' ? 0 : 1;
	$: usefulOpacity = stampScale * Math.max(0, Math.min(1, (dragX - 25) / 75));
	$: dismissOpacity = stampScale * Math.max(0, Math.min(1, (-dragX - 25) / 75));
	$: ackOpacity = stampScale * Math.max(0, Math.min(1, (-dragY - 25) / 65));

	function onPointerDown(e: PointerEvent): void {
		if (isComplete || animatingOut) return;
		// Don't drag if clicking buttons directly
		const target = e.target as HTMLElement;
		if (target.closest('button') || target.closest('a')) return;

		activePointerId = e.pointerId;
		isDragging = true;
		dragX = 0;
		dragY = 0;
		pointerTravel = 0;
		cardElement?.setPointerCapture?.(e.pointerId);
	}

	function onPointerMove(e: PointerEvent): void {
		if (!isDragging || e.pointerId !== activePointerId) return;
		dragX += e.movementX;
		dragY += e.movementY;
		pointerTravel += Math.abs(e.movementX) + Math.abs(e.movementY);
	}

	function onPointerUp(e: PointerEvent): void {
		if (!isDragging || e.pointerId !== activePointerId) return;
		isDragging = false;
		if (cardElement && cardElement.hasPointerCapture?.(e.pointerId)) {
			cardElement.releasePointerCapture(e.pointerId);
		}
		activePointerId = null;

		const SWIPE_THRESHOLD = 95;
		const VERT_THRESHOLD = 80;
		const TAP_TRAVEL = 6;

		if (e.type === 'pointerup' && pointerTravel < TAP_TRAVEL && activeCard && hasDetails) {
			// A plain tap opens the card's details; only a drag triages it.
			dragX = 0;
			dragY = 0;
			void openDetails(activeCard);
		} else if (dragX > SWIPE_THRESHOLD) {
			triggerSwipe('useful');
		} else if (dragX < -SWIPE_THRESHOLD) {
			triggerSwipe('dismiss');
		} else if (dragY < -VERT_THRESHOLD) {
			triggerSwipe('acknowledge');
		} else {
			// Spring back
			dragX = 0;
			dragY = 0;
		}
	}

	function triggerSwipe(kind: SwipeKind): void {
		exitCard(kind, kind);
	}

	/**
	 * The deck's one commit path: the card leaves, the action is dispatched
	 * with a `complete` callback, and a failed action puts the card back on
	 * top. Swipes and the Act menu's resolving entries both come through here.
	 */
	function exitCard(animation: SwipeKind | 'resolve', kind: DeckActionKind): void {
		if (!activeCard || animatingOut) return;
		actMenuOpen = false;
		animatingOut = animation;
		const swipedCard = activeCard;

		// Set target exit coordinates
		if (animation === 'useful') {
			dragX = typeof window !== 'undefined' && window.innerWidth > 600 ? 550 : 380;
			dragY = dragY * 0.5;
		} else if (animation === 'dismiss') {
			dragX = typeof window !== 'undefined' && window.innerWidth > 600 ? -550 : -380;
			dragY = dragY * 0.5;
		} else {
			dragY = -450;
			dragX = dragX * 0.4;
		}

		swipeTimer = setTimeout(() => {
            swipeTimer = null;
			if (swipedCard) {
				triagedIds.set(swipedCard.id, isForYouCard(swipedCard));
				triagedIds = triagedIds;
				dispatch('action', { card: swipedCard, kind, complete: success => {
                    if (!success) { triagedIds.delete(swipedCard.id); triagedIds = triagedIds; }
                } });
			}
			dragX = 0;
			dragY = 0;
			animatingOut = null;
			if (currentStack.length <= 1) {
				dispatch('complete');
			}
		}, 260);
	}

	async function openDetails(card: TriageCardData): Promise<void> {
		actMenuOpen = false;
		detailCardId = card.id;
		await tick();
		detailWrapEl?.scrollIntoView?.({ block: 'nearest', behavior: 'smooth' });
	}

	/**
	 * Only `snooze` and Open resolve the card from the menu itself. Every other
	 * entry opens the detail view and starts there, so an instruction, a draft
	 * or a form comes first and the card stays on top until that flow resolves.
	 */
	async function chooseEntry(entry: DeckActEntry): Promise<void> {
		const card = activeCard;
		if (!card) return;
		if (entry.kind === 'snooze') {
			exitCard('resolve', 'snooze');
			return;
		}
		if (entry.kind === 'open') {
			openWorthLinkInGesture(card.rawWorth);
			exitCard('useful', 'primary');
			return;
		}
		// The menu item is the link; it has already opened the thread.
		if (entry.kind === 'open_provider') return;
		await openDetails(card);
		if (entry.kind !== 'details') detailRef?.start(entry);
	}

	function onDetailResolved(card: TriageCardData, message: string): void {
		detailCardId = null;
		triagedIds.set(card.id, isForYouCard(card));
		triagedIds = triagedIds;
		dispatch('resolved', { card, message });
		if (currentStack.length <= 1) {
			dispatch('complete');
		}
	}

	/** Keys typed into a field or a dialog belong there, not to the deck. */
	function keyBelongsElsewhere(target: EventTarget | null): boolean {
		if (!(target instanceof HTMLElement)) return false;
		return target.isContentEditable || Boolean(target.closest('input, textarea, select, [role="dialog"]'));
	}

	function handleKeydown(e: KeyboardEvent): void {
		// An open Act menu owns ↑/↓/Esc; Enter activates its focused item.
		if (actMenuOpen) return;
		if (isComplete || animatingOut || keyBelongsElsewhere(e.target)) return;
		if (e.key === 'ArrowRight' || e.key === 'l' || e.key === 'L') {
			e.preventDefault();
			triggerSwipe('useful');
		} else if (e.key === 'ArrowLeft' || e.key === 'h' || e.key === 'H') {
			e.preventDefault();
			triggerSwipe('dismiss');
		} else if (e.key === 'ArrowUp' || e.key === 'k' || e.key === 'K') {
			e.preventDefault();
			triggerSwipe('acknowledge');
		} else if (e.key === 'Enter') {
			// A focused control keeps its own Enter.
			const target = e.target instanceof HTMLElement ? e.target : null;
			if (target?.closest('button, a, [role="tab"]')) return;
			if (actEntries.length > 0) {
				e.preventDefault();
				void actMenu?.openMenu();
			}
		}
	}

	function restartDeck(): void {
		actMenuOpen = false;
		detailCardId = null;
		triagedIds.clear();
		triagedIds = triagedIds;
		dragX = 0;
		dragY = 0;
		animatingOut = null;
	}
</script>

<svelte:window on:keydown={handleKeydown} />

<div class="np-deck-wrap">
	<!-- Deck Status & Tabs -->
	<div class="np-deck-progress">
		<div class="np-deck-tabs" role="tablist" aria-label="Deck card filter">
			<button
				type="button"
				role="tab"
				class="deck-tab-btn"
				class:active={activeTab === 'all'}
				aria-selected={activeTab === 'all'}
				on:click={() => selectTab('all')}
			>
				<span>All</span>
				{#if (followUpTotal ?? forYouCards.length) + (worthTotal ?? worthCards.length) > 0}
					<span class="tab-badge">{(followUpTotal ?? forYouCards.length) + (worthTotal ?? worthCards.length)}</span>
				{/if}
			</button>

			<button
				type="button"
				role="tab"
				class="deck-tab-btn"
				class:active={activeTab === 'for_you'}
				aria-selected={activeTab === 'for_you'}
				on:click={() => selectTab('for_you')}
			>
				<span>For You</span>
				{#if (followUpTotal ?? forYouCards.length) > 0}
					<span class="tab-badge">{followUpTotal ?? forYouCards.length}</span>
				{/if}
			</button>

			<button
				type="button"
				role="tab"
				class="deck-tab-btn"
				class:active={activeTab === 'worth'}
				aria-selected={activeTab === 'worth'}
				on:click={() => selectTab('worth')}
			>
				<span>Worth a Look</span>
				{#if (worthTotal ?? worthCards.length) > 0}
					<span class="tab-badge">{worthTotal ?? worthCards.length}</span>
				{/if}
			</button>
		</div>
	</div>

	<p class="np-deck-count">{initialLoading ? 'Loading Reading Room…' : `${currentStack.length} loaded · ${totalAvailable} total`}</p>
	<!-- The Card Stage -->
	<div class="np-deck-stage">
		{#if currentStack.length === 0 && (initialLoading || loadingMore)}
			<div class="np-deck-empty" role="status"><p>{initialLoading ? 'Loading Reading Room…' : 'Loading more cards…'}</p></div>
		{:else if currentStack.length === 0 && (hasMore || loadError)}
			<div class="np-deck-empty"><p>{loadError || 'More cards are available.'}</p>
				<button type="button" class="np-btn np-btn--primary" on:click={() => dispatch('needMore', activeTab)}>Load more cards</button>
			</div>
		{:else if isComplete || currentStack.length === 0}
			<!-- All Cleared / Empty State -->
			<div class="np-deck-empty">
				<div class="np-coffee-icon">☕</div>
				<h3 class="np-empty-title">
					{#if totalInitialInTab === 0}
						No cards in this stack
					{:else}
						All cards reviewed
					{/if}
				</h3>
				<p class="np-empty-desc">
					{#if totalInitialInTab === 0}
						There are currently no {activeTab === 'for_you' ? 'For You' : activeTab === 'worth' ? 'Worth a Look' : ''} cards in today's deck.
					{:else}
						You've reviewed {triagedInTab} items in this stack.
					{/if}
				</p>
				<div class="np-empty-actions">
					<button
						type="button"
						class="np-btn np-btn--primary"
						on:click={() => dispatch('switchView')}
					>
						<span>📰 Open Broadsheet View</span>
					</button>
					{#if totalInitialInTab > 0}
						<button
							type="button"
							class="np-btn np-btn--outline"
							on:click={restartDeck}
						>
							<span>↻ Review Again</span>
						</button>
					{/if}
				</div>
			</div>
		{:else if activeCard}
			<!-- Card 3 (Bottom) -->
			{#if thirdCard}
				<div class="np-card-preview np-card-preview--third">
					<div class="np-preview-kicker">{thirdCard.category}</div>
					<div class="np-preview-title">{thirdCard.title}</div>
				</div>
			{/if}

			<!-- Card 2 (Middle) -->
			{#if nextCard}
				<div class="np-card-preview np-card-preview--second">
					<div class="np-preview-kicker">{nextCard.category}</div>
					<div class="np-preview-title">{nextCard.title}</div>
				</div>
			{/if}

			<!-- Card 1 (Active / Interactive Top Card) -->
			<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
			<article
				bind:this={cardElement}
				class="np-deck-card"
				class:np-deck-card--dragging={isDragging}
				class:np-deck-card--animating={animatingOut !== null}
				style={`transform: translate3d(${dragX}px, ${dragY}px, 0) rotate(${dragX * 0.07}deg); transition: ${isDragging ? 'none' : 'transform 0.26s cubic-bezier(0.2, 0.9, 0.3, 1)'};`}
				on:pointerdown={onPointerDown}
				on:pointermove={onPointerMove}
				on:pointerup={onPointerUp}
				on:pointercancel={onPointerUp}
			>
				<!-- Dynamic Ink Stamp Watermarks -->
				<div
					class="np-stamp np-stamp--useful"
					style={`opacity: ${animatingOut === 'useful' ? 1 : usefulOpacity};`}
				>
					USEFUL
				</div>
				<div
					class="np-stamp np-stamp--dismiss"
					style={`opacity: ${animatingOut === 'dismiss' ? 1 : dismissOpacity};`}
				>
					DISMISS
				</div>
				<div
					class="np-stamp np-stamp--ack"
					style={`opacity: ${animatingOut === 'acknowledge' ? 1 : ackOpacity};`}
				>
					ACKNOWLEDGED
				</div>

				<!-- Card Content -->
				<div class="np-deck-card__header">
					<span
						class="np-deck-category"
						class:is-dispatch={activeCard.originLane === 'dispatch'}
						class:is-reading={activeCard.originLane === 'reading_room'}
					>
						{activeCard.category}
					</span>
					{#if debug}
						<span class="np-deck-debug">DEBUG</span>
					{/if}
					{#if activeCard.sender}
						<span class="np-deck-sender">{activeCard.sender}</span>
					{/if}
				</div>

				{#if dateline}<time class="np-deck-date" datetime={dateline.iso}>{dateline.text}</time>{/if}
				<h3 class="np-deck-card__title">
					{activeCard.title}
				</h3>

				<p class="np-deck-card__summary">
					{activeCard.summary}
				</p>

				{#if activeCard.rawFollowUp?.open_url || hasDetails}
					<div class="np-deck-card__footer">
						{#if activeCard.rawFollowUp?.open_url}
							<div class="np-deck-card__link">
								<a
									href={activeCard.rawFollowUp.open_url}
									target="_blank"
									rel="noopener noreferrer"
									on:pointerdown|stopPropagation
								>
									View thread in {activeCard.rawFollowUp.provider || 'app'} <Icon name="arrow-up-right" size={12} />
								</a>
							</div>
						{/if}
						{#if hasDetails}
							<button
								type="button"
								class="np-deck-card__details"
								data-testid="today-deck-details"
								aria-expanded={detailCard !== null}
								on:click={() => activeCard && void openDetails(activeCard)}
							>
								Details ›
							</button>
						{/if}
					</div>
				{/if}
			</article>
		{:else}
			<div class="np-deck-empty">
				<p>No actionable dispatches in today's edition.</p>
			</div>
		{/if}
	</div>

	<!-- Triage Button Bar (Tactile desktop / tap controls) -->
	{#if !isComplete && activeCard}
		<div class="np-deck-controls">
			<!-- Dismiss -->
			<button
				type="button"
				class="np-deck-btn np-deck-btn--dismiss"
				title="Swipe left or press ← to dismiss"
				on:click={() => triggerSwipe('dismiss')}
			>
				<Icon name="x" size={18} />
				<span>Dismiss</span>
			</button>

			<!-- Acknowledge -->
			<button
				type="button"
				class="np-deck-btn np-deck-btn--ack"
				title="Swipe up or press ↑ to acknowledge"
				on:click={() => triggerSwipe('acknowledge')}
			>
				<Icon name="archive" size={18} />
				<span>Seen</span>
			</button>

			<!-- Useful -->
			<button
				type="button"
				class="np-deck-btn np-deck-btn--useful"
				title="Swipe right or press → to mark useful"
				on:click={() => triggerSwipe('useful')}
			>
				<Icon name="check" size={18} />
				<span>Useful</span>
			</button>

			<!-- Act: this card's own actions; nothing runs until one is chosen -->
			<TodayActMenu
				entries={actEntries}
				variant="deck"
				placement="up"
				testIdPrefix="today-deck-act"
				title="Press Enter to choose an action"
				disabled={animatingOut !== null}
				bind:open={actMenuOpen}
				bind:this={actMenu}
				on:choose={(event) => void chooseEntry(event.detail)}
			/>
		</div>

		<!-- Keyboard Navigation Hints -->
		<div class="np-deck-shortcuts" aria-hidden="true">
			<span><kbd>←</kbd> Dismiss</span>
			<span><kbd>↑</kbd> Seen</span>
			<span><kbd>→</kbd> Useful</span>
			{#if actEntries.length > 0}
				<span><kbd>↵</kbd> Act</span>
			{/if}
		</div>

		{#if detailCard}
			<div class="np-deck-detail-wrap" bind:this={detailWrapEl}>
				<TodayDeckDetail
					card={detailCard}
					{chatThreadId}
					bind:this={detailRef}
					on:close={() => (detailCardId = null)}
					on:resolved={(event) => detailCard && onDetailResolved(detailCard, event.detail.message)}
					on:open={() => exitCard('useful', 'primary')}
				/>
			</div>
		{/if}
	{/if}
</div>

<style>
	.np-deck-count { margin: 0 0 12px; color: var(--text-secondary); font-size: 12px; }
	.np-deck-date { display: block; font-size: 11px; color: var(--text-secondary); }
	.np-deck-wrap {
		display: flex;
		flex-direction: column;
		align-items: center;
		width: 100%;
		max-width: 100%;
		margin: 0;
		padding: 0.5rem 0 1.5rem;
		user-select: none;
		box-sizing: border-box;
	}

	.np-deck-progress {
		width: 100%;
		max-width: 40rem;
		display: flex;
		justify-content: center;
		margin-bottom: 0.85rem;
	}

	.np-deck-tabs {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.25rem;
		padding: 0.18rem;
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--bg-surface) 90%, var(--border-soft));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		box-sizing: border-box;
	}

	.deck-tab-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.76rem;
		font-weight: 600;
		line-height: 1;
		padding: 0.3rem 0.75rem;
		border-radius: 4px;
		border: none;
		background: transparent;
		color: var(--text-secondary, #52525b);
		cursor: pointer;
		transition: background 0.15s ease, color 0.15s ease, box-shadow 0.15s ease;
		white-space: nowrap;
	}

	.deck-tab-btn:hover {
		color: var(--text-primary, #18181b);
	}

	.deck-tab-btn.active {
		background: var(--bg-card, #ffffff);
		color: var(--text-primary, #18181b);
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.08);
	}

	.tab-badge {
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		font-weight: 700;
		padding: 0.08rem 0.35rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--text-muted, #71717a) 15%, transparent);
		color: var(--text-muted, #71717a);
	}

	.deck-tab-btn.active .tab-badge {
		background: color-mix(in srgb, var(--accent-primary, #b45309) 15%, transparent);
		color: var(--accent-primary, #b45309);
	}

	/* Stage where cards stack */
	.np-deck-stage {
		position: relative;
		width: 100%;
		max-width: 40rem;
		height: 23rem;
		display: flex;
		align-items: center;
		justify-content: center;
		margin-bottom: 1.5rem;
	}

	/* Card Stack Visual Layering */
	.np-card-preview {
		position: absolute;
		width: 100%;
		height: 22rem;
		padding: 1.5rem;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: var(--radius-lg, 12px);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.04);
		pointer-events: none;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		box-sizing: border-box;
	}

	.np-card-preview--second {
		transform: scale(0.96) translateY(12px);
		opacity: 0.88;
		z-index: 1;
	}

	.np-card-preview--third {
		transform: scale(0.92) translateY(24px);
		opacity: 0.6;
		z-index: 0;
	}

	.np-preview-kicker {
		font-family: var(--font-mono, monospace);
		font-size: 0.65rem;
		color: var(--text-muted);
		text-transform: uppercase;
	}

	.np-preview-title {
		font-family: var(--font-display, serif);
		font-size: 1.1rem;
		color: var(--text-secondary);
	}

	/* Active Top Card */
	.np-deck-card {
		position: absolute;
		top: 0;
		left: 0;
		width: 100%;
		height: 22rem;
		padding: 1.6rem 1.8rem;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.18));
		border-radius: var(--radius-lg, 12px);
		box-shadow: 0 10px 30px rgba(0, 0, 0, 0.08), 0 1px 3px rgba(0, 0, 0, 0.05);
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		z-index: 2;
		touch-action: none;
		cursor: grab;
		overflow: hidden;
		box-sizing: border-box;
	}

	.np-deck-card--dragging {
		cursor: grabbing;
	}

	.np-deck-card__header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--text-muted, #71717a);
	}

	.np-deck-category {
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		padding: 0.15rem 0.5rem;
		border-radius: 4px;
		background: color-mix(in srgb, var(--accent-primary, #b45309) 10%, transparent);
		color: var(--accent-primary, #b45309);
		border: 1px solid color-mix(in srgb, var(--accent-primary, #b45309) 25%, transparent);
	}

	.np-deck-category.is-reading {
		background: color-mix(in srgb, #6366f1 10%, transparent);
		color: #4f46e5;
		border-color: color-mix(in srgb, #6366f1 25%, transparent);
	}

	.np-deck-sender {
		font-weight: 500;
		color: var(--text-secondary);
		text-transform: none;
	}

	.np-deck-card__title {
		margin: 0;
		font-family: var(--font-display, 'Newsreader', serif);
		font-size: 1.45rem;
		font-weight: 600;
		line-height: 1.25;
		color: var(--text-primary, #18181b);
	}

	.np-deck-card__summary {
		margin: 0;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.95rem;
		line-height: 1.55;
		color: var(--text-secondary, #3f3f46);
		flex: 1;
		overflow-y: auto;
	}

	.np-deck-card__footer {
		margin-top: auto;
		padding-top: 0.5rem;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.np-deck-card__link {
		font-size: 0.82rem;
	}

	.np-deck-card__details {
		margin-left: auto;
		padding: 0;
		border: 0;
		background: transparent;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.82rem;
		font-weight: 600;
		color: var(--text-secondary, #52525b);
		cursor: pointer;
	}

	.np-deck-card__details:hover,
	.np-deck-card__details:focus-visible {
		color: var(--accent-primary, #b45309);
		text-decoration: underline;
	}

	.np-deck-card__link a {
		color: var(--accent-primary, #b45309);
		text-decoration: underline;
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
	}

	/* Ink Stamp Watermarks */
	.np-stamp {
		position: absolute;
		top: 1.8rem;
		font-family: var(--font-display, serif);
		font-size: 1.7rem;
		font-weight: 700;
		letter-spacing: 0.1em;
		padding: 0.25rem 0.85rem;
		border-radius: 6px;
		border: 3px solid currentColor;
		pointer-events: none;
		z-index: 10;
		transition: opacity 0.08s linear;
	}

	.np-stamp--useful {
		right: 2rem;
		transform: rotate(14deg);
		color: #059669;
		border-color: #059669;
		background: rgba(16, 185, 129, 0.08);
	}

	.np-stamp--dismiss {
		left: 2rem;
		transform: rotate(-14deg);
		color: #dc2626;
		border-color: #dc2626;
		background: rgba(239, 68, 68, 0.08);
	}

	.np-stamp--ack {
		top: 2rem;
		left: 50%;
		transform: translateX(-50%);
		color: #2563eb;
		border-color: #2563eb;
		background: rgba(37, 99, 235, 0.08);
	}

	/* Cleared State */
	.np-deck-empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		text-align: center;
		padding: 2rem 1.5rem;
		background: var(--bg-card, #ffffff);
		border: 1px dashed var(--border-soft, rgba(0, 0, 0, 0.15));
		border-radius: var(--radius-lg, 12px);
		width: 100%;
		height: 22rem;
	}

	.np-coffee-icon {
		font-size: 2.8rem;
		margin-bottom: 0.5rem;
	}

	.np-empty-title {
		font-family: var(--font-display, serif);
		font-size: 1.5rem;
		color: var(--text-primary);
		margin: 0 0 0.4rem;
	}

	.np-empty-desc {
		font-size: 0.9rem;
		color: var(--text-secondary);
		max-width: 24rem;
		margin: 0 0 1.5rem;
		line-height: 1.5;
	}

	.np-empty-actions {
		display: flex;
		gap: 0.75rem;
		flex-wrap: wrap;
		justify-content: center;
	}

	/* Tactile Action Buttons */
	.np-deck-controls {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
		justify-content: center;
		width: 100%;
		max-width: 40rem;
	}

	.np-deck-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.88rem;
		font-weight: 600;
		padding: 0.65rem 1.15rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.18));
		background: var(--bg-surface, #faf9f6);
		color: var(--text-primary, #18181b);
		cursor: pointer;
		box-shadow: 0 2px 4px rgba(0, 0, 0, 0.04);
		transition: transform 0.12s ease, background 0.15s ease, border-color 0.15s ease;
	}

	.np-deck-btn:active {
		transform: scale(0.96);
	}

	.np-deck-btn--dismiss:hover {
		border-color: #ef4444;
		color: #dc2626;
		background: rgba(239, 68, 68, 0.06);
	}

	.np-deck-btn--ack:hover {
		border-color: #2563eb;
		color: #2563eb;
		background: rgba(37, 99, 235, 0.06);
	}

	.np-deck-btn--useful:hover {
		border-color: #10b981;
		color: #059669;
		background: rgba(16, 185, 129, 0.06);
	}

	.np-deck-btn--primary {
		background: var(--accent-fill, #b45309);
		border-color: transparent;
		color: #ffffff;
	}

	.np-deck-btn--primary:hover {
		background: var(--accent-fill-hover);
	}

	.np-deck-detail-wrap {
		display: flex;
		justify-content: center;
		width: 100%;
	}

	.np-deck-shortcuts {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 1.25rem;
		margin-top: 1rem;
		font-size: 0.75rem;
		color: var(--text-muted);
		width: 100%;
		max-width: 40rem;
	}

	.np-deck-shortcuts kbd {
		display: inline-block;
		padding: 0.1rem 0.35rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		background: var(--bg-surface);
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		color: var(--text-secondary);
	}

	.np-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.85rem;
		font-weight: 600;
		padding: 0.5rem 1rem;
		border-radius: var(--radius-sm, 6px);
		border: 1px solid var(--border-soft);
		background: var(--bg-surface);
		color: var(--text-primary);
		cursor: pointer;
	}

	.np-btn--primary {
		background: var(--accent-fill);
		color: #ffffff;
		border-color: transparent;
	}

	.np-btn--outline {
		background: transparent;
	}
</style>
