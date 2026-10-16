<script lang="ts" context="module">
	let gameTooltipSequence = 0;

	function nextGameTooltipId(): string {
		gameTooltipSequence += 1;
		return `game-tooltip-${gameTooltipSequence}`;
	}
</script>

<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import '../game-chrome.css';

	export let content = '';
	export let position: 'top' | 'bottom' | 'left' | 'right' = 'top';
	export let shortcut = '';
	export let disabled = false;
	export let display: 'inline' | 'block' = 'inline';
	export let delay = 350;

	const tooltipId = nextGameTooltipId();
	let rootEl: HTMLSpanElement | null = null;
	let describedElement: HTMLElement | null = null;
	let tooltipEl: HTMLSpanElement | null = null;
	let triggerObserver: MutationObserver | null = null;
	let visible = false;
	let mounted = false;
	let showTimer: ReturnType<typeof setTimeout> | null = null;
	let hideTimer: ReturnType<typeof setTimeout> | null = null;
	let suppressFocusTooltip = false;

	$: safePosition = position === 'bottom' || position === 'left' || position === 'right' ? position : 'top';
	$: hasContent = !disabled && content.trim().length > 0;
	$: if (!hasContent) visible = false;
	$: if (mounted) syncDescriptionTarget(hasContent);
	$: if (mounted) syncBubble(content, shortcut, safePosition, visible && hasContent);

	function clearTimers(): void {
		if (showTimer) clearTimeout(showTimer);
		if (hideTimer) clearTimeout(hideTimer);
		showTimer = null;
		hideTimer = null;
	}

	function show(immediate = false): void {
		if (!hasContent) return;
		clearTimers();
		if (immediate || delay <= 0) {
			visible = true;
			schedulePosition();
			return;
		}
		showTimer = setTimeout(() => {
			visible = true;
			schedulePosition();
		}, delay);
	}

	function hide(): void {
		clearTimers();
		hideTimer = setTimeout(() => (visible = false), 60);
	}

	function dismissImmediately(): void {
		clearTimers();
		visible = false;
	}

	function dismissForActivation(): void {
		suppressFocusTooltip = true;
		dismissImmediately();
	}

	function handleMouseEnter(): void {
		suppressFocusTooltip = false;
		show(false);
	}

	function handleFocusIn(): void {
		if (suppressFocusTooltip) {
			dismissImmediately();
			return;
		}
		show(true);
	}

	function detachDescription(): void {
		if (!describedElement) return;
		const ids = (describedElement.getAttribute('aria-describedby') ?? '')
			.split(/\s+/)
			.filter((id) => id && id !== tooltipId);
		if (ids.length > 0) describedElement.setAttribute('aria-describedby', ids.join(' '));
		else describedElement.removeAttribute('aria-describedby');
		describedElement = null;
	}

	function syncDescriptionTarget(enabled: boolean): void {
		const candidate = rootEl?.firstElementChild;
		const next = enabled && candidate instanceof HTMLElement ? candidate : null;
		if (next === describedElement) return;
		detachDescription();
		if (!next) return;
		const ids = (next.getAttribute('aria-describedby') ?? '').split(/\s+/).filter(Boolean);
		if (!ids.includes(tooltipId)) ids.push(tooltipId);
		next.setAttribute('aria-describedby', ids.join(' '));
		describedElement = next;
	}

	function syncBubble(
		bubbleContent: string,
		bubbleShortcut: string,
		bubblePosition: typeof safePosition,
		isVisible: boolean
	): void {
		if (!tooltipEl) return;
		tooltipEl.replaceChildren(document.createTextNode(bubbleContent));
		if (bubbleShortcut) {
			const shortcutEl = document.createElement('span');
			shortcutEl.className = 'game-ui-tooltip__shortcut';
			shortcutEl.textContent = bubbleShortcut;
			tooltipEl.append(shortcutEl);
		}
		tooltipEl.dataset.position = bubblePosition;
		tooltipEl.dataset.visible = String(isVisible);
		if (isVisible) tooltipEl.removeAttribute('aria-hidden');
		else tooltipEl.setAttribute('aria-hidden', 'true');
	}

	function schedulePosition(): void {
		if (typeof requestAnimationFrame === 'undefined') return;
		requestAnimationFrame(positionBubble);
	}

	function positionBubble(): void {
		if (!visible || !hasContent || !rootEl || !tooltipEl) return;
		const anchor = rootEl.getBoundingClientRect();
		const bubble = tooltipEl.getBoundingClientRect();
		const gap = 8;
		const margin = 8;
		let left = anchor.left + (anchor.width - bubble.width) / 2;
		let top = anchor.top - bubble.height - gap;
		if (safePosition === 'bottom') top = anchor.bottom + gap;
		if (safePosition === 'left') {
			left = anchor.left - bubble.width - gap;
			top = anchor.top + (anchor.height - bubble.height) / 2;
		}
		if (safePosition === 'right') {
			left = anchor.right + gap;
			top = anchor.top + (anchor.height - bubble.height) / 2;
		}
		left = Math.max(margin, Math.min(left, window.innerWidth - bubble.width - margin));
		top = Math.max(margin, Math.min(top, window.innerHeight - bubble.height - margin));
		tooltipEl.style.left = `${Math.round(left)}px`;
		tooltipEl.style.top = `${Math.round(top)}px`;
	}

	function handleFocusOut(event: FocusEvent): void {
		if (event.relatedTarget instanceof Node && rootEl?.contains(event.relatedTarget)) return;
		hide();
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || !visible) return;
		event.preventDefault();
		event.stopPropagation();
		visible = false;
		clearTimers();
	}

	function handleDocumentKeydown(event: KeyboardEvent): void {
		if (event.key === 'Tab') suppressFocusTooltip = false;
	}

	onMount(() => {
		tooltipEl = document.createElement('span');
		tooltipEl.id = tooltipId;
		tooltipEl.className = 'game-ui-tooltip';
		tooltipEl.setAttribute('role', 'tooltip');
		tooltipEl.setAttribute('aria-hidden', 'true');
		tooltipEl.style.left = '-10000px';
		tooltipEl.style.top = '-10000px';
		// The bubble is appended to <body>, so it leaves the surface that owns
		// it and would otherwise render in app chrome while its anchor sits on
		// a skinned plate. Carry the surface's skin across with it.
		const skin = rootEl?.closest('[data-game-skin]')?.getAttribute('data-game-skin');
		if (skin) tooltipEl.setAttribute('data-game-skin', skin);
		document.body.append(tooltipEl);
		triggerObserver = new MutationObserver(() => syncDescriptionTarget(hasContent));
		if (rootEl) triggerObserver.observe(rootEl, { childList: true });
		mounted = true;
		syncDescriptionTarget(hasContent);
		syncBubble(content, shortcut, safePosition, visible && hasContent);
		window.addEventListener('resize', positionBubble);
		document.addEventListener('scroll', positionBubble, true);
		document.addEventListener('keydown', handleDocumentKeydown, true);
	});

	onDestroy(() => {
		clearTimers();
		detachDescription();
		if (typeof window !== 'undefined') {
			window.removeEventListener('resize', positionBubble);
			document.removeEventListener('scroll', positionBubble, true);
			document.removeEventListener('keydown', handleDocumentKeydown, true);
		}
		triggerObserver?.disconnect();
		triggerObserver = null;
		tooltipEl?.remove();
		tooltipEl = null;
	});
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<span
	class="game-ui-tooltip-anchor"
	data-display={display}
	bind:this={rootEl}
	on:mouseenter={handleMouseEnter}
	on:mouseleave={hide}
	on:focusin={handleFocusIn}
	on:focusout={handleFocusOut}
	on:keydown={handleKeydown}
	on:pointerdown|capture={dismissForActivation}
	on:click|capture={dismissForActivation}
>
	<slot />
</span>
