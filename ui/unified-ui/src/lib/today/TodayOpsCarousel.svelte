<script lang="ts">
	/**
	 * TodayOpsCarousel.svelte
	 *
	 * Generic fixed-height carousel card for the Today "Operations" ledger.
	 * Slides are plain `{ id, title }` entries; their content comes from the
	 * `slide` snippet, so adding a slide is one entry plus one snippet branch.
	 *
	 * - Header shows the current slide's serif title (cross-fades on change).
	 * - Dots under the content jump to a slide.
	 * - Auto-advances every `intervalMs` (wrapping). Held while hovered, while
	 *   focus is inside, while dragging, and for `manualPauseMs` after any manual
	 *   navigation. No auto-advance and instant changes under reduced motion.
	 * - Pointer / touch swipe on the viewport; ← / → when focus is inside.
	 */
	import { onMount, type Snippet } from 'svelte';
	import {
		autoAdvanceDelay,
		OPS_CAROUSEL_INTERVAL_MS,
		OPS_CAROUSEL_MANUAL_PAUSE_MS,
		slideLabel,
		stepSlideIndex,
		type OpsCarouselSlide
	} from '$lib/today/opsCarousel';

	interface Props {
		slides: OpsCarouselSlide[];
		slide: Snippet<[OpsCarouselSlide, number]>;
		/** Accessible name of the carousel region. */
		label?: string;
		/** Optional trailing header content (byline). */
		aside?: Snippet;
		intervalMs?: number;
		manualPauseMs?: number;
	}

	let {
		slides,
		slide,
		label = 'Operations',
		aside,
		intervalMs = OPS_CAROUSEL_INTERVAL_MS,
		manualPauseMs = OPS_CAROUSEL_MANUAL_PAUSE_MS
	}: Props = $props();

	let current = $state(0);
	let hovering = $state(false);
	let focusWithin = $state(false);
	let dragging = $state(false);
	let dragDx = $state(0);
	let pausedUntil = $state(0);
	let reducedMotion = $state(false);
	let viewportEl: HTMLDivElement | undefined = $state();

	const count = $derived(slides.length);
	const index = $derived(count > 0 ? Math.min(current, count - 1) : 0);
	const interacting = $derived(hovering || focusWithin || dragging);
	const autoEnabled = $derived(!reducedMotion && count > 1);
	// Announce slide changes the user made; stay quiet while auto-rotating.
	const liveMode = $derived(autoEnabled && !interacting && pausedUntil === 0 ? 'off' : 'polite');

	onMount(() => {
		if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
		const query = window.matchMedia('(prefers-reduced-motion: reduce)');
		reducedMotion = Boolean(query?.matches);
		const onChange = (event: MediaQueryListEvent) => (reducedMotion = event.matches);
		query?.addEventListener?.('change', onChange);
		return () => query?.removeEventListener?.('change', onChange);
	});

	$effect(() => {
		// Re-armed whenever the slide, the interaction state or the pause changes.
		const at = index;
		const busy = interacting;
		const until = pausedUntil;
		const total = count;
		if (!autoEnabled) return;
		let timer: ReturnType<typeof setTimeout> | undefined;
		const check = () => {
			const wait = autoAdvanceDelay({ nowMs: Date.now(), pausedUntilMs: until, interacting: busy, intervalMs });
			if (wait > 0) {
				timer = setTimeout(check, wait);
				return;
			}
			current = stepSlideIndex(at, total, 1);
		};
		timer = setTimeout(check, intervalMs);
		return () => clearTimeout(timer);
	});

	function goTo(target: number) {
		if (count === 0) return;
		current = stepSlideIndex(target, count, 0);
		pausedUntil = Date.now() + manualPauseMs;
	}

	function step(delta: number) {
		goTo(stepSlideIndex(index, count, delta));
	}

	function onKeydown(event: KeyboardEvent) {
		const target = event.target as HTMLElement | null;
		if (target?.closest('input, textarea, select, [contenteditable="true"]')) return;
		if (event.key === 'ArrowRight') {
			event.preventDefault();
			step(1);
		} else if (event.key === 'ArrowLeft') {
			event.preventDefault();
			step(-1);
		}
	}

	// Hold rotation for keyboard focus only: a mouse click on a dot leaves focus
	// on it, and that should fall under the 15 s manual pause, not stop rotation.
	function onFocusIn(event: FocusEvent) {
		const target = event.target as HTMLElement | null;
		let keyboard = true;
		try {
			keyboard = target?.matches(':focus-visible') ?? true;
		} catch {
			keyboard = true;
		}
		focusWithin = keyboard;
	}

	function onFocusOut(event: FocusEvent) {
		const root = event.currentTarget as HTMLElement;
		const next = event.relatedTarget as Node | null;
		if (!next || !root.contains(next)) focusWithin = false;
	}

	// --- Pointer / touch swipe ---
	const DRAG_START_PX = 8;
	let pointerId: number | null = null;
	let startX = 0;
	let startY = 0;
	let suppressClick = false;

	function onPointerDown(event: PointerEvent) {
		if (count < 2 || (event.pointerType === 'mouse' && event.button !== 0)) return;
		pointerId = event.pointerId;
		startX = event.clientX;
		startY = event.clientY;
		dragDx = 0;
		suppressClick = false;
	}

	function onPointerMove(event: PointerEvent) {
		if (pointerId !== event.pointerId) return;
		const dx = event.clientX - startX;
		const dy = event.clientY - startY;
		if (!dragging) {
			if (Math.abs(dx) < DRAG_START_PX || Math.abs(dx) <= Math.abs(dy)) return;
			dragging = true;
			suppressClick = true;
			viewportEl?.setPointerCapture?.(event.pointerId);
		}
		dragDx = dx;
	}

	function endDrag(event: PointerEvent, commit: boolean) {
		if (pointerId !== event.pointerId) return;
		pointerId = null;
		if (dragging) {
			const width = viewportEl?.clientWidth || 320;
			const threshold = Math.min(60, width * 0.15);
			if (commit && Math.abs(dragDx) >= threshold) step(dragDx < 0 ? 1 : -1);
			viewportEl?.releasePointerCapture?.(event.pointerId);
		}
		dragging = false;
		dragDx = 0;
	}

	function onClickCapture(event: MouseEvent) {
		if (!suppressClick) return;
		suppressClick = false;
		event.preventDefault();
		event.stopPropagation();
	}
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section
	class="ops-carousel"
	class:is-reduced={reducedMotion}
	aria-roledescription="carousel"
	aria-label={label}
	onmouseenter={() => (hovering = true)}
	onmouseleave={() => (hovering = false)}
	onfocusin={onFocusIn}
	onfocusout={onFocusOut}
	onkeydown={onKeydown}
>
	<header class="ops-carousel__header">
		<h2 class="ops-carousel__title">
			{#each slides as s, i (s.id)}
				<span class="ops-carousel__title-text" class:is-current={i === index} aria-hidden={i === index ? undefined : 'true'}>
					{s.title}
				</span>
			{/each}
		</h2>
		{#if aside}
			<div class="ops-carousel__aside">{@render aside()}</div>
		{/if}
	</header>

	<p class="ops-carousel__sr" aria-live={liveMode} aria-atomic="true">
		{#if count > 0}{slideLabel(index, count, slides[index].title)}{/if}
	</p>

	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="ops-carousel__viewport"
		class:is-dragging={dragging}
		bind:this={viewportEl}
		onpointerdown={onPointerDown}
		onpointermove={onPointerMove}
		onpointerup={(e) => endDrag(e, true)}
		onpointercancel={(e) => endDrag(e, false)}
		onclickcapture={onClickCapture}
		ondragstart={(e) => e.preventDefault()}
	>
		<div
			class="ops-carousel__track"
			style:transform={`translateX(calc(${-index * 100}% + ${dragDx}px))`}
		>
			{#each slides as s, i (s.id)}
				<div
					class="ops-carousel__slide"
					role="group"
					aria-roledescription="slide"
					aria-label={slideLabel(i, count, s.title)}
					aria-hidden={i === index ? undefined : 'true'}
					inert={i !== index}
					data-slide-id={s.id}
				>
					{@render slide(s, i)}
				</div>
			{/each}
		</div>
	</div>

	{#if count > 1}
		<div class="ops-carousel__dots">
			{#each slides as s, i (s.id)}
				<button
					type="button"
					class="ops-carousel__dot"
					class:is-current={i === index}
					aria-label={slideLabel(i, count, s.title)}
					aria-current={i === index ? 'true' : undefined}
					onclick={() => goTo(i)}
				></button>
			{/each}
		</div>
	{/if}
</section>

<style>
	.ops-carousel {
		--ops-carousel-height: 8.5rem;
		margin: 0;
		background: var(--bg-surface-raised, var(--bg-surface, #ffffff));
		border-top: 2px solid var(--text-primary, #1e293b);
		border-bottom: 2px solid var(--text-primary, #1e293b);
		border-left: 1px solid var(--border-soft, transparent);
		border-right: 1px solid var(--border-soft, transparent);
		border-radius: var(--radius-sm, 4px);
		padding: 0.85rem 1.25rem 0.6rem;
		box-sizing: border-box;
		min-width: 0;
	}

	@media (max-width: 860px) {
		.ops-carousel {
			--ops-carousel-height: 13.5rem;
			padding: 0.75rem 1rem 0.55rem;
		}
	}

	.ops-carousel__header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.5rem;
		padding-bottom: 0.45rem;
		border-bottom: 1px solid var(--border-color, rgba(128, 128, 128, 0.2));
		margin-bottom: 0.65rem;
		min-width: 0;
	}

	.ops-carousel__title {
		display: grid;
		margin: 0;
		min-width: 0;
		flex: 1 1 auto;
		font-family: 'Newsreader', 'Playfair Display', Georgia, serif;
		font-size: 1.25rem;
		font-weight: 700;
		color: var(--text-primary, #1e293b);
		line-height: 1.2;
	}

	/* Titles share one grid cell so they cross-fade in place. */
	.ops-carousel__title-text {
		grid-area: 1 / 1;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		opacity: 0;
		transition: opacity 0.35s ease;
	}

	.ops-carousel__title-text.is-current {
		opacity: 1;
	}

	.ops-carousel__aside {
		flex: 0 0 auto;
	}

	.ops-carousel__sr {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip: rect(0 0 0 0);
		white-space: nowrap;
		border: 0;
	}

	.ops-carousel__viewport {
		overflow: hidden;
		touch-action: pan-y;
		min-width: 0;
	}

	.ops-carousel__viewport.is-dragging {
		cursor: grabbing;
		user-select: none;
	}

	.ops-carousel__track {
		display: flex;
		transition: transform 0.45s cubic-bezier(0.22, 0.61, 0.36, 1);
		will-change: transform;
	}

	.ops-carousel__viewport.is-dragging .ops-carousel__track,
	.ops-carousel.is-reduced .ops-carousel__track,
	.ops-carousel.is-reduced .ops-carousel__title-text {
		transition: none;
	}

	.ops-carousel__slide {
		flex: 0 0 100%;
		min-width: 0;
		height: var(--ops-carousel-height);
		box-sizing: border-box;
		overflow: hidden;
	}

	.ops-carousel__dots {
		display: flex;
		justify-content: center;
		align-items: center;
		gap: 0.2rem;
		margin-top: 0.4rem;
	}

	/* 24px hit target around a 6×6 dot / 16×6 pill. */
	.ops-carousel__dot {
		position: relative;
		width: 24px;
		height: 20px;
		padding: 0;
		border: 0;
		background: transparent;
		cursor: pointer;
	}

	.ops-carousel__dot::after {
		content: '';
		position: absolute;
		top: 50%;
		left: 50%;
		width: 6px;
		height: 6px;
		border-radius: 999px;
		background: var(--border-color, rgba(128, 128, 128, 0.35));
		transform: translate(-50%, -50%);
		transition: width 0.25s ease, background-color 0.25s ease;
	}

	.ops-carousel__dot:hover::after {
		background: var(--text-muted, #64748b);
	}

	.ops-carousel__dot.is-current::after {
		width: 16px;
		background: var(--accent-primary, #6366f1);
	}

	.ops-carousel__dot:focus-visible {
		outline: 2px solid var(--accent-primary, #6366f1);
		outline-offset: -2px;
		border-radius: 4px;
	}
</style>
