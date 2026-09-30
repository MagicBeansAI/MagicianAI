<script lang="ts">
	import { createEventDispatcher, onDestroy, onMount, tick } from 'svelte';
	import {
		GAME_SURFACE_PRIORITY,
		captureActiveElement,
		focusGameSurface,
		isTopmostGameSurface,
		restoreGameFocus
	} from './surfaceFocus';
	import type { GameDismissSource } from './types';
	import '../game-chrome.css';

	export let visible = false;
	export let label = '';
	export let detail = '';
	export let ariaLabel = 'Context commands';
	export let dismissible = true;
	export let reserveInspector = true;
	export let inspectorWidth: 'default' | 'wide' = 'default';
	export let autofocus = false;
	export let restoreFocus = true;
	export let className = '';

	const dispatch = createEventDispatcher<{ dismiss: { source: GameDismissSource } }>();
	let commandBarEl: HTMLElement | null = null;
	let previouslyFocused: HTMLElement | null = null;
	let mounted = false;
	let wasVisible = false;

	$: hasContext = Boolean(label || detail || $$slots.context);
	$: if (mounted) syncVisibleState(visible);

	function syncVisibleState(nextVisible: boolean): void {
		if (nextVisible === wasVisible) return;
		wasVisible = nextVisible;
		if (nextVisible) {
			previouslyFocused = captureActiveElement();
			if (autofocus) void activate();
			return;
		}
		if (restoreFocus) restoreGameFocus(previouslyFocused);
		previouslyFocused = null;
	}

	async function activate(): Promise<void> {
		await tick();
		if (visible) focusGameSurface(commandBarEl, 'first');
	}

	function requestDismiss(source: GameDismissSource): void {
		if (dismissible) dispatch('dismiss', { source });
	}

	function focusableCommands(): HTMLElement[] {
		if (!commandBarEl) return [];
		return Array.from(
			commandBarEl.querySelectorAll<HTMLElement>(
				'button:not([disabled]), [href], [role="button"][tabindex]:not([tabindex="-1"])'
			)
		).filter((element) => element.getAttribute('aria-hidden') !== 'true');
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape') {
			if (!dismissible) return;
			event.preventDefault();
			requestDismiss('escape');
			return;
		}
		if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
		const commands = focusableCommands();
		if (commands.length === 0) return;
		const current = document.activeElement instanceof HTMLElement
			? commands.indexOf(document.activeElement)
			: -1;
		let next = current;
		if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = commands.length - 1;
		else if (event.key === 'ArrowRight') next = current < 0 ? 0 : (current + 1) % commands.length;
		else next = current <= 0 ? commands.length - 1 : current - 1;
		event.preventDefault();
		commands[next]?.focus();
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (
			!visible ||
			!dismissible ||
			event.defaultPrevented ||
			event.isComposing ||
			event.key !== 'Escape' ||
			!isTopmostGameSurface(commandBarEl)
		) return;
		event.preventDefault();
		requestDismiss('escape');
	}

	onMount(() => {
		mounted = true;
		syncVisibleState(visible);
	});

	onDestroy(() => {
		if (restoreFocus && wasVisible) restoreGameFocus(previouslyFocused);
	});
</script>

<svelte:window on:keydown={handleWindowKeydown} />

{#if visible}
	<div
		{...$$restProps}
		class={`game-ui-command-bar ${className}`.trim()}
		data-context={hasContext}
		data-reserve-inspector={reserveInspector}
		data-inspector-width={inspectorWidth}
		data-game-dismiss-priority={GAME_SURFACE_PRIORITY.commandBar}
		role="toolbar"
		aria-label={ariaLabel}
		aria-orientation="horizontal"
		aria-keyshortcuts={dismissible ? 'Escape' : undefined}
		tabindex="-1"
		bind:this={commandBarEl}
		on:keydown={handleKeydown}
	>
		{#if hasContext}
			<div class="game-ui-command-bar__context">
				{#if $$slots.context}<slot name="context" />{:else}
					{#if label}<span class="game-ui-command-bar__label">{label}</span>{/if}
					{#if detail}<span class="game-ui-command-bar__detail">{detail}</span>{/if}
				{/if}
			</div>
		{/if}
		<div class="game-ui-command-bar__commands"><slot /></div>
	</div>
{/if}
