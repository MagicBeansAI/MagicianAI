<script lang="ts">
	/**
	 * Floating, draggable + resizable Events Console.
	 *
	 * Mounts the same `<EventStreamCard />` as the `/events` route but
	 * keeps the operator on whichever surface they were already on.
	 * Toggled by `⌘E / ⌃E` (defined in `(app)/+layout.svelte`); `Esc`
	 * closes when focused.
	 *
	 * Position + size are persisted in `eventsConsoleStore` (localStorage
	 * key `magician:events-console:layout`). Layout is clamped into the
	 * viewport at open time so a position saved from a wider monitor
	 * doesn't strand the window off-screen.
	 *
	 * Z-index sits below modal backdrops (≥980) so HITL prompts and
	 * other modals still occlude it, and above the ExecutionPanel
	 * slide-out so the console is reachable while a panel is open.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { fade } from 'svelte/transition';

	import EventStreamCard from '$lib/realtime/EventStreamCard.svelte';
	import {
		clampLayoutToViewport,
		closeEventsConsole,
		eventsConsoleLayout,
		eventsConsoleVisible,
		EVENTS_CONSOLE_MIN_HEIGHT,
		EVENTS_CONSOLE_MIN_WIDTH
	} from '$lib/stores/eventsConsoleStore';

	let dragState: { pointerId: number; offsetX: number; offsetY: number } | null = null;
	let resizeState:
		| { pointerId: number; startX: number; startY: number; startW: number; startH: number }
		| null = null;
	let frameEl: HTMLDivElement | null = null;
	let headerEl: HTMLElement | null = null;
	let resizeEl: HTMLDivElement | null = null;
	// rAF-coalesced layout-set token. `pointermove` fires at the
	// platform's pointer-event rate (~60–240 Hz depending on display
	// + browser); each `eventsConsoleLayout.set(...)` triggers Svelte
	// reactivity through every subscriber. We coalesce all
	// move-driven writes into a single per-frame write by stashing
	// the latest target layout and only setting the store on the
	// next animation frame. The 250 ms localStorage debounce in the
	// store already protects disk; this throttle protects the in-
	// memory reactive graph from a hot loop on slow hardware.
	let pendingLayoutWrite: typeof $eventsConsoleLayout | null = null;
	let pendingLayoutFrame: number | null = null;
	function scheduleLayoutWrite(next: typeof $eventsConsoleLayout): void {
		pendingLayoutWrite = next;
		if (pendingLayoutFrame !== null) return;
		pendingLayoutFrame = requestAnimationFrame(() => {
			pendingLayoutFrame = null;
			if (pendingLayoutWrite) {
				eventsConsoleLayout.set(pendingLayoutWrite);
				pendingLayoutWrite = null;
			}
		});
	}

	$: layout = $eventsConsoleLayout;

	function handleHeaderPointerDown(event: PointerEvent): void {
		// Ignore drags that start on a button (close button, etc.) so
		// header buttons keep their semantics.
		const target = event.target as HTMLElement | null;
		if (target?.closest('button')) return;
		event.preventDefault();
		dragState = {
			pointerId: event.pointerId,
			offsetX: event.clientX - layout.x,
			offsetY: event.clientY - layout.y
		};
		// Capture on the stable header element rather than `event.target`
		// (which is often an inner span and loses capture as the cursor
		// crosses children — most painful in Safari). Window-level
		// pointerup/cancel below also closes the drag if capture is
		// dropped by the browser.
		headerEl?.setPointerCapture?.(event.pointerId);
	}

	function handleHeaderPointerMove(event: PointerEvent): void {
		if (!dragState || dragState.pointerId !== event.pointerId) return;
		const next = clampLayoutToViewport({
			...layout,
			x: event.clientX - dragState.offsetX,
			y: event.clientY - dragState.offsetY
		});
		scheduleLayoutWrite(next);
	}

	function endHeaderPointer(event: PointerEvent): void {
		if (dragState && dragState.pointerId === event.pointerId) {
			headerEl?.releasePointerCapture?.(event.pointerId);
			dragState = null;
			// Flush any rAF-pending layout so the final-position write
			// to localStorage debounces from the true end state. Without
			// this, releasing the pointer between two move events could
			// leave the persisted position one frame behind the visible.
			flushPendingLayout();
		}
	}

	function handleResizePointerDown(event: PointerEvent): void {
		event.preventDefault();
		event.stopPropagation();
		resizeState = {
			pointerId: event.pointerId,
			startX: event.clientX,
			startY: event.clientY,
			startW: layout.width,
			startH: layout.height
		};
		// Same stable-capture pattern as the header drag: capture on the
		// fixed resize element, not the event target, so capture survives
		// children + nwse-drag excursions.
		resizeEl?.setPointerCapture?.(event.pointerId);
	}

	function handleResizePointerMove(event: PointerEvent): void {
		if (!resizeState || resizeState.pointerId !== event.pointerId) return;
		const dx = event.clientX - resizeState.startX;
		const dy = event.clientY - resizeState.startY;
		const next = clampLayoutToViewport({
			...layout,
			width: Math.max(EVENTS_CONSOLE_MIN_WIDTH, resizeState.startW + dx),
			height: Math.max(EVENTS_CONSOLE_MIN_HEIGHT, resizeState.startH + dy)
		});
		scheduleLayoutWrite(next);
	}

	function endResizePointer(event: PointerEvent): void {
		if (resizeState && resizeState.pointerId === event.pointerId) {
			resizeEl?.releasePointerCapture?.(event.pointerId);
			resizeState = null;
			flushPendingLayout();
		}
	}

	function flushPendingLayout(): void {
		if (pendingLayoutFrame !== null) {
			cancelAnimationFrame(pendingLayoutFrame);
			pendingLayoutFrame = null;
		}
		if (pendingLayoutWrite) {
			eventsConsoleLayout.set(pendingLayoutWrite);
			pendingLayoutWrite = null;
		}
	}

	// Backstop window listeners — if the browser drops pointer capture
	// (Safari does this when focus leaves the document, iframes,
	// devtools, etc.) the per-element pointerup never fires and
	// `dragState`/`resizeState` would hang, leaving the next click on
	// the header jumping the panel by a stale offset. These
	// window-scoped handlers run while a drag is active and clear the
	// state regardless of where the pointer was released.
	function handleWindowPointerUp(event: PointerEvent): void {
		if (dragState && dragState.pointerId === event.pointerId) {
			dragState = null;
		}
		if (resizeState && resizeState.pointerId === event.pointerId) {
			resizeState = null;
		}
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (!$eventsConsoleVisible) return;
		if (event.key === 'Escape') {
			// Don't swallow Escape if focus is in a form field inside
			// the console — the operator might be cancelling a search.
			const target = event.target as HTMLElement | null;
			const inField =
				target &&
				(target.tagName === 'INPUT' ||
					target.tagName === 'TEXTAREA' ||
					target.tagName === 'SELECT' ||
					target.isContentEditable);
			if (inField) return;
			event.preventDefault();
			closeEventsConsole();
		}
	}

	function handleViewportResize(): void {
		// Flush any rAF-pending drag/resize write first so the
		// post-resize clamp operates on the *latest* target layout,
		// not the stale pre-drag one. Without this, a viewport
		// resize during a drag would clamp the old layout, then the
		// pending rAF callback would overwrite with an unclamped
		// pending value and the console could end up off-screen.
		flushPendingLayout();
		eventsConsoleLayout.update((current) => clampLayoutToViewport(current));
	}

	$: if ($eventsConsoleVisible) {
		// Reclamp on every open in case the viewport changed since last
		// open or the saved layout was from a different monitor.
		eventsConsoleLayout.update((current) => clampLayoutToViewport(current));
	}

	onMount(() => {
		window.addEventListener('resize', handleViewportResize);
		window.addEventListener('pointerup', handleWindowPointerUp);
		window.addEventListener('pointercancel', handleWindowPointerUp);
	});
	onDestroy(() => {
		if (typeof window !== 'undefined') {
			window.removeEventListener('resize', handleViewportResize);
			window.removeEventListener('pointerup', handleWindowPointerUp);
			window.removeEventListener('pointercancel', handleWindowPointerUp);
		}
		// Drop any rAF-pending layout write so the next mount doesn't
		// inherit a stale handle, and flush the latest pending value
		// to the store so the persisted geometry matches the visible
		// state at teardown.
		flushPendingLayout();
	});
</script>

<svelte:window on:keydown={handleKeydown} />

{#if $eventsConsoleVisible}
	<div
		bind:this={frameEl}
		class="events-console"
		style:left="{layout.x}px"
		style:top="{layout.y}px"
		style:width="{layout.width}px"
		style:height="{layout.height}px"
		role="dialog"
		aria-label="Events console"
		transition:fade={{ duration: 120 }}
	>
		<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
		<header
			bind:this={headerEl}
			class="events-console__header"
			role="group"
			aria-label="Events console title bar"
			on:pointerdown={handleHeaderPointerDown}
			on:pointermove={handleHeaderPointerMove}
			on:pointerup={endHeaderPointer}
			on:pointercancel={endHeaderPointer}
			on:lostpointercapture={endHeaderPointer}
		>
			<div class="events-console__title">
				<span class="events-console__eyebrow">Live</span>
				<span class="events-console__name">Events Console</span>
				<span class="events-console__hint">⌘E to toggle · drag header · resize ↘</span>
			</div>
			<button
				type="button"
				class="events-console__close"
				aria-label="Close events console"
				title="Close (Esc)"
				on:click={closeEventsConsole}
			>×</button>
		</header>
		<div class="events-console__body">
			<EventStreamCard
				title=""
				showTitle={false}
				density="full"
				maxHeight="100%"
			/>
		</div>
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div
			bind:this={resizeEl}
			class="events-console__resize"
			role="presentation"
			aria-label="Resize"
			title="Resize"
			on:pointerdown={handleResizePointerDown}
			on:pointermove={handleResizePointerMove}
			on:pointerup={endResizePointer}
			on:pointercancel={endResizePointer}
			on:lostpointercapture={endResizePointer}
		></div>
	</div>
{/if}

<style>
	.events-console {
		position: fixed;
		display: flex;
		flex-direction: column;
		background: var(--bg-base, #fffdf8);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 90%, transparent);
		border-radius: 12px;
		box-shadow: var(--shadow-lg);
		/* Above ExecutionPanel scrim (60), below modal backdrops (980+).
		 * Lets HITL prompts and other modals still occlude the console
		 * while the panel slide-out doesn't. */
		z-index: 200;
		overflow: hidden;
		min-width: 0;
	}

	.events-console__header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0.5rem 0.75rem;
		gap: 0.5rem;
		border-bottom: 1px solid color-mix(in srgb, var(--border-soft) 60%, transparent);
		background: color-mix(in srgb, var(--bg-card, #f6f1e8) 75%, transparent);
		cursor: grab;
		user-select: none;
		touch-action: none;
	}

	.events-console__header:active {
		cursor: grabbing;
	}

	.events-console__title {
		display: flex;
		align-items: baseline;
		gap: 0.6rem;
		min-width: 0;
		overflow: hidden;
	}

	.events-console__eyebrow {
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.62rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.18em;
		color: var(--accent-primary);
	}

	.events-console__name {
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--text-primary);
	}

	.events-console__hint {
		font-family: var(--font-mono, monospace);
		font-size: 0.66rem;
		color: var(--text-muted);
		opacity: 0.8;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.events-console__close {
		appearance: none;
		background: transparent;
		border: 1px solid transparent;
		border-radius: 8px;
		width: 1.6rem;
		height: 1.6rem;
		font-size: 1rem;
		line-height: 1;
		cursor: pointer;
		color: var(--text-secondary);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		transition: background 140ms ease, border-color 140ms ease, color 140ms ease;
	}

	.events-console__close:hover {
		background: var(--bg-soft);
		border-color: var(--accent-primary);
		color: var(--text-primary);
	}

	.events-console__body {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}

	.events-console__body :global(.esc) {
		flex: 1;
		min-height: 0;
	}

	.events-console__resize {
		position: absolute;
		bottom: 0;
		right: 0;
		/* Bumped from 16x16 → 22x22 so it's easier to grab; the
		 * EventStreamCard's full-density rows have their own pointer
		 * handlers (click-to-expand JSON peek), which means a 16px
		 * target sat almost entirely on top of a row's last 16px and
		 * the row often won the click. */
		width: 22px;
		height: 22px;
		cursor: nwse-resize;
		touch-action: none;
		/* Sit above the EventStreamCard body. Below the close button on
		 * the header (which is in a separate stacking row) but above
		 * everything inside the body so the handle is always grabbable
		 * even when content fills the bottom-right cell. */
		z-index: 2;
		background:
			linear-gradient(
				135deg,
				transparent 0,
				transparent 40%,
				color-mix(in srgb, var(--text-muted) 70%, transparent) 40%,
				color-mix(in srgb, var(--text-muted) 70%, transparent) 50%,
				transparent 50%,
				transparent 60%,
				color-mix(in srgb, var(--text-muted) 70%, transparent) 60%,
				color-mix(in srgb, var(--text-muted) 70%, transparent) 70%,
				transparent 70%
			);
	}

	.events-console__resize:hover {
		background:
			linear-gradient(
				135deg,
				transparent 0,
				transparent 35%,
				var(--accent-primary) 35%,
				var(--accent-primary) 50%,
				transparent 50%,
				transparent 60%,
				var(--accent-primary) 60%,
				var(--accent-primary) 75%,
				transparent 75%
			);
	}
</style>
