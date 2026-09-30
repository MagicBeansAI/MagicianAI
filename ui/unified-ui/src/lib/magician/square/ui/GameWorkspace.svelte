<script lang="ts" context="module">
	let gameWorkspaceSequence = 0;

	function nextGameWorkspaceId(): string {
		gameWorkspaceSequence += 1;
		return `game-workspace-${gameWorkspaceSequence}`;
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
	import type { GameDismissSource, GameNavigationMode, GameSurfaceFocus } from './types';
	import '../game-chrome.css';

	export let open = false;
	export let title: string;
	export let subtitle = '';
	export let navigation: GameNavigationMode = 'back';
	export let navigationLabel = '';
	export let navigationAriaLabel = 'Workspace navigation';
	export let showContext = true;
	export let showNavigation = true;
	export let dismissible = true;
	export let initialFocus: GameSurfaceFocus = 'surface';
	export let restoreFocus = true;
	export let busy = false;
	export let className = '';
	export let presentation: 'fullscreen' | 'overlay' | 'docked' = 'fullscreen';

	const dispatch = createEventDispatcher<{ back: { source: GameDismissSource } }>();
	const titleId = nextGameWorkspaceId();
	let workspaceEl: HTMLElement | null = null;
	let previouslyFocused: HTMLElement | null = null;
	let mounted = false;
	let wasOpen = false;

	$: if (mounted) syncOpenState(open);
	$: effectiveNavigationLabel =
		navigationLabel || (navigation === 'close' ? 'Close workspace' : 'Back to Town Square');

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
		if (open) focusGameSurface(workspaceEl, initialFocus);
	}

	function requestBack(source: GameDismissSource): void {
		if (!dismissible) return;
		dispatch('back', { source });
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (
			!open ||
			!dismissible ||
			presentation === 'docked' ||
			event.defaultPrevented ||
			event.isComposing ||
			event.key !== 'Escape' ||
			!isTopmostGameSurface(workspaceEl)
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
	{#if presentation === 'overlay'}
		<button
			type="button"
			class="game-ui-workspace__backdrop"
			aria-label={`Close ${title}`}
			tabindex="-1"
			on:click={() => requestBack('backdrop')}
		></button>
	{/if}
	<section
		{...$$restProps}
		class={`game-ui-workspace ${className}`.trim()}
		data-context={showContext}
		data-presentation={presentation}
		data-game-dismiss-priority={GAME_SURFACE_PRIORITY.workspace}
		aria-labelledby={titleId}
		role={presentation === 'overlay' ? 'dialog' : undefined}
		aria-modal={presentation === 'overlay' ? 'true' : undefined}
		aria-busy={busy}
		aria-keyshortcuts={dismissible ? 'Escape' : undefined}
		tabindex="-1"
		bind:this={workspaceEl}
	>
		{#if showContext}<div class="game-ui-workspace__context"><slot name="context" /></div>{/if}
		<div class="game-ui-workspace__frame" data-navigation={showNavigation}>
			{#if showNavigation}
				<nav class="game-ui-workspace__nav" aria-label={navigationAriaLabel}><slot name="navigation" /></nav>
			{/if}
			<div class="game-ui-workspace__main">
				<header class="game-ui-workspace__header">
					{#if navigation !== 'none'}
						<GameHudButton
							label={effectiveNavigationLabel}
							tooltipPosition="bottom"
							disabled={!dismissible}
							on:click={() => requestBack('button')}
						>
							<Icon name={navigation === 'close' ? 'x' : 'chevron-left'} size={18} />
						</GameHudButton>
					{:else}<span></span>{/if}
					<div class="game-ui-workspace__heading">
						<h1 id={titleId} class="game-ui-workspace__title">{title}</h1>
						{#if subtitle}<p class="game-ui-workspace__subtitle">{subtitle}</p>{/if}
					</div>
					{#if $$slots.actions}<div class="game-ui-workspace__actions"><slot name="actions" /></div>{:else}<span></span>{/if}
				</header>
				<div class="game-ui-workspace__body"><slot /></div>
				{#if $$slots.footer}<footer class="game-ui-workspace__footer"><slot name="footer" /></footer>{/if}
			</div>
		</div>
	</section>
{/if}
