<script context="module" lang="ts">
	import type { ChannelFollowUp } from '$lib/stores/channelNeedsYouStore';
	import type { ResurfacingCard } from './resurfacingQueries';
	import type { CanonicalAttentionItem } from '$lib/attention/canonicalAttentionProjection';

	export interface TriageCardData {
		id: string;
		title: string;
		category: string;
		summary: string;
		primaryLabel?: string;
		sender?: string;
		originLane?: 'dispatch' | 'reading_room';
		rawItem?: CanonicalAttentionItem;
		rawFollowUp?: ChannelFollowUp;
		rawWorth?: ResurfacingCard;
	}

	export type DeckTab = 'all' | 'for_you' | 'worth';
</script>

<script lang="ts">
	import { createEventDispatcher, onMount, onDestroy } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';

	export let cards: TriageCardData[] = [];
	export let debug = false;
	export let activeTab: DeckTab = 'all';

	const dispatch = createEventDispatcher<{
		action: { card: TriageCardData; kind: 'useful' | 'acknowledge' | 'dismiss' | 'primary' };
		complete: void;
		switchView: void;
	}>();

	let triagedIds = new Set<string>();

	function isForYouCard(card: TriageCardData): boolean {
		return card.originLane === 'dispatch' || Boolean(card.rawFollowUp) || card.category.toUpperCase().includes('DISPATCH');
	}

	function isWorthCard(card: TriageCardData): boolean {
		return card.originLane === 'reading_room' || Boolean(card.rawWorth) || card.category.toUpperCase().includes('READING ROOM') || card.category.toUpperCase().includes('NOTE') || card.category.toUpperCase().includes('MEMORY');
	}

	$: unhandledAll = cards.filter(c => !triagedIds.has(c.id));
	$: forYouCards = unhandledAll.filter(isForYouCard);
	$: worthCards = unhandledAll.filter(isWorthCard);

	$: currentStack = activeTab === 'for_you'
		? forYouCards
		: activeTab === 'worth'
			? worthCards
			: unhandledAll;

	$: totalInitialInTab = cards.filter(c => {
		if (activeTab === 'for_you') return isForYouCard(c);
		if (activeTab === 'worth') return isWorthCard(c);
		return true;
	}).length;

	$: triagedInTab = cards.filter(c => triagedIds.has(c.id) && (
		activeTab === 'for_you' ? isForYouCard(c) : activeTab === 'worth' ? isWorthCard(c) : true
	)).length;

	let dragX = 0;
	let dragY = 0;
	let isDragging = false;
	let activePointerId: number | null = null;
	let animatingOut: 'useful' | 'dismiss' | 'acknowledge' | null = null;
	let cardElement: HTMLElement | null = null;

	function selectTab(tab: DeckTab): void {
		if (activeTab === tab) return;
		activeTab = tab;
		dragX = 0;
		dragY = 0;
		animatingOut = null;
	}

	$: activeCard = currentStack[0] ?? null;
	$: nextCard = currentStack[1] ?? null;
	$: thirdCard = currentStack[2] ?? null;
	$: remainingCount = currentStack.length;
	$: isComplete = currentStack.length === 0 && totalInitialInTab > 0;

	// Stamp opacities based on drag deltas
	$: usefulOpacity = Math.max(0, Math.min(1, (dragX - 25) / 75));
	$: dismissOpacity = Math.max(0, Math.min(1, (-dragX - 25) / 75));
	$: ackOpacity = Math.max(0, Math.min(1, (-dragY - 25) / 65));

	function onPointerDown(e: PointerEvent): void {
		if (isComplete || animatingOut) return;
		// Don't drag if clicking buttons directly
		const target = e.target as HTMLElement;
		if (target.closest('button') || target.closest('a')) return;

		activePointerId = e.pointerId;
		isDragging = true;
		dragX = 0;
		dragY = 0;
		cardElement?.setPointerCapture(e.pointerId);
	}

	function onPointerMove(e: PointerEvent): void {
		if (!isDragging || e.pointerId !== activePointerId) return;
		dragX += e.movementX;
		dragY += e.movementY;
	}

	function onPointerUp(e: PointerEvent): void {
		if (!isDragging || e.pointerId !== activePointerId) return;
		isDragging = false;
		if (cardElement && cardElement.hasPointerCapture(e.pointerId)) {
			cardElement.releasePointerCapture(e.pointerId);
		}
		activePointerId = null;

		const SWIPE_THRESHOLD = 95;
		const VERT_THRESHOLD = 80;

		if (dragX > SWIPE_THRESHOLD) {
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

	function triggerSwipe(kind: 'useful' | 'dismiss' | 'acknowledge'): void {
		if (!activeCard || animatingOut) return;
		animatingOut = kind;
		const swipedCard = activeCard;

		// Set target exit coordinates
		if (kind === 'useful') {
			dragX = typeof window !== 'undefined' && window.innerWidth > 600 ? 550 : 380;
			dragY = dragY * 0.5;
		} else if (kind === 'dismiss') {
			dragX = typeof window !== 'undefined' && window.innerWidth > 600 ? -550 : -380;
			dragY = dragY * 0.5;
		} else if (kind === 'acknowledge') {
			dragY = -450;
			dragX = dragX * 0.4;
		}

		setTimeout(() => {
			if (swipedCard) {
				triagedIds.add(swipedCard.id);
				triagedIds = triagedIds;
				dispatch('action', { card: swipedCard, kind });
			}
			dragX = 0;
			dragY = 0;
			animatingOut = null;
			if (currentStack.length <= 1) {
				dispatch('complete');
			}
		}, 260);
	}

	function handlePrimary(): void {
		if (!activeCard) return;
		dispatch('action', { card: activeCard, kind: 'primary' });
	}

	function handleKeydown(e: KeyboardEvent): void {
		if (isComplete || animatingOut) return;
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
			if (activeCard?.primaryLabel) {
				e.preventDefault();
				handlePrimary();
			}
		}
	}

	function restartDeck(): void {
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
				{#if unhandledAll.length > 0}
					<span class="tab-badge">{unhandledAll.length}</span>
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
				{#if forYouCards.length > 0}
					<span class="tab-badge">{forYouCards.length}</span>
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
				{#if worthCards.length > 0}
					<span class="tab-badge">{worthCards.length}</span>
				{/if}
			</button>
		</div>
	</div>

	<!-- The Card Stage -->
	<div class="np-deck-stage">
		{#if isComplete || currentStack.length === 0}
			<!-- All Cleared / Empty State -->
			<div class="np-deck-empty">
				<div class="np-coffee-icon">☕</div>
				<h3 class="np-empty-title">
					{#if totalInitialInTab === 0}
						No Dispatches in This Stack
					{:else}
						All Dispatches Cleared
					{/if}
				</h3>
				<p class="np-empty-desc">
					{#if totalInitialInTab === 0}
						There are currently no {activeTab === 'for_you' ? 'For You' : activeTab === 'worth' ? 'Worth a Look' : ''} cards in today's deck.
					{:else}
						You've triaged all {totalInitialInTab} items in this stack. Your slate is completely clean.
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

				<h3 class="np-deck-card__title">
					{activeCard.title}
				</h3>

				<p class="np-deck-card__summary">
					{activeCard.summary}
				</p>

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

			<!-- Primary Action -->
			{#if activeCard.primaryLabel}
				<button
					type="button"
					class="np-deck-btn np-deck-btn--primary"
					title="Press Enter to execute primary action"
					on:click={handlePrimary}
				>
					<span>⚡ {activeCard.primaryLabel}</span>
				</button>
			{/if}
		</div>

		<!-- Keyboard Navigation Hints -->
		<div class="np-deck-shortcuts" aria-hidden="true">
			<span><kbd>←</kbd> Dismiss</span>
			<span><kbd>↑</kbd> Seen</span>
			<span><kbd>→</kbd> Useful</span>
			{#if activeCard.primaryLabel}
				<span><kbd>↵</kbd> {activeCard.primaryLabel}</span>
			{/if}
		</div>
	{/if}
</div>

<style>
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

	.np-deck-card__link {
		margin-top: auto;
		padding-top: 0.5rem;
		font-size: 0.82rem;
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
		background: var(--accent-primary, #b45309);
		border-color: transparent;
		color: #ffffff;
	}

	.np-deck-btn--primary:hover {
		background: color-mix(in srgb, var(--accent-primary, #b45309) 88%, #000);
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
		background: var(--accent-primary);
		color: #ffffff;
		border-color: transparent;
	}

	.np-btn--outline {
		background: transparent;
	}
</style>
