<script lang="ts" context="module">
	let gameInspectorSequence = 0;

	function nextGameInspectorId(): string {
		gameInspectorSequence += 1;
		return `game-inspector-${gameInspectorSequence}`;
	}
</script>

<script lang="ts">
	import { createEventDispatcher, onDestroy, onMount, tick } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import GameHudButton from './GameHudButton.svelte';
	import {
		GAME_SURFACE_PRIORITY,
		captureActiveElement,
		focusGameSurface,
		isTopmostGameSurface,
		restoreGameFocus
	} from './surfaceFocus';
	import type {
		GameDismissSource,
		GameNavigationMode,
		GameSurfaceFocus,
		GameTone
	} from './types';
	import '../game-chrome.css';

	export let open = false;
	export let title: string;
	export let subtitle = '';
	export let eyebrow = '';
	export let tone: GameTone = 'neutral';
	export let width: 'default' | 'wide' = 'default';
	export let navigation: GameNavigationMode = 'close';
	export let navigationLabel = '';
	export let dismissible = true;
	export let initialFocus: GameSurfaceFocus = 'surface';
	export let restoreFocus = true;
	export let busy = false;
	export let className = '';

	const dispatch = createEventDispatcher<{ back: { source: GameDismissSource } }>();
	const titleId = nextGameInspectorId();
	let inspectorEl: HTMLElement | null = null;
	let previouslyFocused: HTMLElement | null = null;
	let mounted = false;
	let wasOpen = false;

	$: if (mounted) syncOpenState(open);
	$: effectiveNavigationLabel =
		navigationLabel || (navigation === 'back' ? 'Back' : 'Close inspector');

	function syncOpenState(nextOpen: boolean): void {
		if (nextOpen === wasOpen) return;
		wasOpen = nextOpen;
		if (nextOpen) {
			previouslyFocused = captureActiveElement();
			void activate();
			return;
		}
		if (restoreFocus) restoreGameFocus(previouslyFocused);
		previouslyFocused = null;
	}

	async function activate(): Promise<void> {
		await tick();
		if (open) focusGameSurface(inspectorEl, initialFocus);
	}

	function requestBack(source: GameDismissSource): void {
		if (!dismissible) return;
		dispatch('back', { source });
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (
			!open ||
			!dismissible ||
			event.defaultPrevented ||
			event.isComposing ||
			event.key !== 'Escape' ||
			!isTopmostGameSurface(inspectorEl)
		) return;
		event.preventDefault();
		requestBack('escape');
	}

	onMount(() => {
		mounted = true;
		syncOpenState(open);
	});

	onDestroy(() => {
		if (restoreFocus && wasOpen) restoreGameFocus(previouslyFocused);
	});
</script>

<svelte:window on:keydown={handleWindowKeydown} />

{#if open}
	<aside
		{...$$restProps}
		class={`game-ui-inspector ${className}`.trim()}
		data-game-tone={tone}
		data-width={width}
		data-game-dismiss-priority={GAME_SURFACE_PRIORITY.inspector}
		aria-labelledby={titleId}
		aria-busy={busy}
		aria-keyshortcuts={dismissible ? 'Escape' : undefined}
		tabindex="-1"
		bind:this={inspectorEl}
	>
		<header class="game-ui-inspector__header">
			<div class="game-ui-inspector__identity">
				{#if $$slots.icon}<div aria-hidden="true"><slot name="icon" /></div>{/if}
				<div class="game-ui-inspector__title-wrap">
					{#if eyebrow}<p class="game-ui-inspector__eyebrow">{eyebrow}</p>{/if}
					<h2 id={titleId} class="game-ui-inspector__title">{title}</h2>
					{#if subtitle}<p class="game-ui-inspector__subtitle">{subtitle}</p>{/if}
					{#if $$slots.status}<slot name="status" />{/if}
				</div>
			</div>
			<div class="game-ui-inspector__tools">
				{#if $$slots.tools}<slot name="tools" />{/if}
				{#if navigation !== 'none'}
					<GameHudButton
						label={effectiveNavigationLabel}
						tooltipPosition="left"
						size="compact"
						disabled={!dismissible}
						on:click={() => requestBack('button')}
					>
						<Icon name={navigation === 'back' ? 'chevron-left' : 'x'} size={16} />
					</GameHudButton>
				{/if}
			</div>
		</header>
		<div class="game-ui-inspector__body"><slot /></div>
		{#if $$slots.footer}<footer class="game-ui-inspector__footer"><slot name="footer" /></footer>{/if}
	</aside>
{/if}
