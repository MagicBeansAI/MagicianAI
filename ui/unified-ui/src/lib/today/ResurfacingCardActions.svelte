<script lang="ts">
	/**
	 * The contextual actions for one Worth-a-look card.
	 *
	 * These used to live inline in `ResurfacingBand`, which meant the canonical
	 * attention lane could only offer the six server-declared verbs — silently
	 * dropping create reminder, create task, ask Presto, save to memory, and
	 * share draft for anyone served by that lane. Owning them here lets both
	 * surfaces present the identical set.
	 *
	 * The component owns the action menu, the dialog, and execution. Read actions
	 * (`view_details`, `show_original`, `open_source`) are emitted instead: they
	 * drive surface-specific chrome — a detail panel in the band, nothing yet in
	 * the lane — so the host decides what they mean.
	 */
	import { createEventDispatcher, tick } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import { showWarning } from '$lib/shared/stores/notifications';
	import ResurfacingActionDialog from './ResurfacingActionDialog.svelte';
	import { presentResurfacingActionResult } from './resurfacingActionResult';
	import {
		isResurfacingDialogAction,
		mergeResurfacingCapabilities,
		type ResurfacingDialogActionKind
	} from './resurfacingPresentation';
	import {
		postResurfacingContextualAction,
		ResurfacingApiError,
		type ResurfacingActionKind,
		type ResurfacingCard,
		type ResurfacingDetail
	} from './resurfacingQueries';

	export let card: ResurfacingCard;
	export let detail: ResurfacingDetail | null = null;
	export let chatThreadId: string | null = null;
	export let disabled = false;
	/** Denser controls for tight rows, matching the follow-up lane. */
	export let compact = false;

	const dispatch = createEventDispatcher<{
		read: { kind: string };
		refresh: Record<string, never>;
		deepersummary: { summary: unknown };
	}>();

	const READ_KINDS = new Set(['view_details', 'show_original', 'open_source']);

	let open = false;
	let busy = false;
	let dialogKind: ResurfacingDialogActionKind | null = null;
	let dialogError: string | null = null;
	let dialogIdempotencyKey = '';
	let menuEl: HTMLDivElement | null = null;
	let triggerEl: HTMLButtonElement | null = null;
	let menuTop = 0;
	let menuRight = 8;
	let menuPositioned = false;

	// The menu is viewport-anchored (`position: fixed`), not absolute in the
	// row: the canonical lane carries `overflow: hidden` for its rounded
	// border, which culls an in-row menu at the lane's bottom edge — the
	// dismiss reasons were unreachable on every card that opened low. Fixed
	// coordinates escape that clipping, flip above the trigger when the
	// viewport bottom is closer than the menu is tall, and cap their height
	// with an internal scroll when neither side fits.
	async function openMenu(): Promise<void> {
		menuPositioned = false;
		open = true;
		await tick();
		if (!open || !menuEl || !triggerEl) return;
		const triggerRect = triggerEl.getBoundingClientRect();
		const menuWidth = menuEl.offsetWidth || 176;
		const menuHeight = menuEl.offsetHeight || 0;
		menuRight = Math.max(
			8,
			Math.min(window.innerWidth - triggerRect.right, window.innerWidth - menuWidth - 8)
		);
		const belowTop = triggerRect.bottom + 4;
		const spaceBelow = window.innerHeight - 8 - belowTop;
		const spaceAbove = triggerRect.top - 8 - 4;
		if (menuHeight <= spaceBelow) {
			menuTop = belowTop;
		} else if (menuHeight <= spaceAbove) {
			menuTop = triggerRect.top - menuHeight - 4;
		} else if (spaceAbove > spaceBelow) {
			menuTop = 8;
			menuEl.style.maxHeight = `${Math.max(160, triggerRect.top - 12)}px`;
		} else {
			menuTop = belowTop;
			menuEl.style.maxHeight = `${Math.max(160, spaceBelow)}px`;
		}
		menuPositioned = true;
	}

	// An open menu has to close on the next click anywhere else, not just on a
	// second press of its own trigger. The listener is on the window rather than
	// the row so a click landing on any other card — or on chrome outside the
	// lane entirely — dismisses it too.
	function handleWindowClick(event: MouseEvent): void {
		if (!open) return;
		const target = event.target as Node;
		if (menuEl?.contains(target) || triggerEl?.contains(target)) return;
		open = false;
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (!open || event.key !== 'Escape') return;
		event.preventDefault();
		open = false;
		triggerEl?.focus();
	}

	// A viewport-anchored menu detaches from its anchor the moment the page or
	// any inner scroller moves; closing mirrors the Worth-a-look overflow menu.
	function handleWindowScroll(): void {
		if (open) open = false;
	}

	// Ask Presto needs a thread to post into; offering it without one produces a
	// failure the user cannot act on, so it is filtered out rather than shown.
	$: capabilities = mergeResurfacingCapabilities(card, detail).filter(
		(capability) =>
			!READ_KINDS.has(capability.kind) &&
			(capability.kind !== 'ask_presto' || Boolean(chatThreadId?.trim()))
	);

	function label(kind: string): string {
		return kind.replace(/_/g, ' ').replace(/\b\w/g, (letter) => letter.toUpperCase());
	}

	function newKey(): string {
		return crypto.randomUUID();
	}

	// Both call sites already carry the union — a capability's own `kind` and
	// the dialog's typed submit event — so widening these to `string` only
	// hid that the API request field wants the union too.
	async function run(kind: ResurfacingActionKind): Promise<void> {
		open = false;
		if (READ_KINDS.has(kind)) {
			dispatch('read', { kind });
			return;
		}
		if (isResurfacingDialogAction(kind as ResurfacingDialogActionKind)) {
			dialogKind = kind as ResurfacingDialogActionKind;
			dialogError = null;
			dialogIdempotencyKey = newKey();
			return;
		}
		await submit(kind, kind === 'ask_presto' ? { ui_thread_id: chatThreadId } : {}, newKey());
	}

	async function submit(
		kind: ResurfacingActionKind,
		input: Record<string, unknown>,
		idempotencyKey: string
	): Promise<void> {
		if (busy) return;
		busy = true;
		dialogError = null;
		try {
			const response = await postResurfacingContextualAction(card.candidate_id, {
				kind,
				idempotency_key: idempotencyKey,
				content_revision: card.content_revision ?? null,
				input
			});
			dialogKind = null;
			const { shouldRefresh, deeperSummary } = await presentResurfacingActionResult(
				response.result
			);
			if (deeperSummary) dispatch('deepersummary', { summary: deeperSummary });
			if (shouldRefresh) dispatch('refresh', {});
		} catch (error) {
			const message =
				error instanceof Error ? error.message : 'The action could not be completed.';
			if (dialogKind) {
				dialogError = message;
				// A retryable failure keeps the key so the server replays rather than
				// applying the action twice; anything else starts a fresh attempt.
				const retryable =
					!(error instanceof ResurfacingApiError) ||
					error.code === 'in_progress' ||
					error.status >= 500;
				if (!retryable) dialogIdempotencyKey = newKey();
			} else {
				showWarning('Action could not be completed', message);
			}
		} finally {
			busy = false;
		}
	}
</script>

<svelte:window on:click={handleWindowClick} on:keydown={handleWindowKeydown} on:scroll|capture={handleWindowScroll} />

{#if capabilities.length > 0}
	<div class="rca" class:compact>
		<button
			type="button"
			class="rca-btn"
			disabled={disabled || busy}
			aria-haspopup="menu"
			aria-expanded={open}
			aria-label="More actions"
			title="More actions"
			bind:this={triggerEl}
			on:click={() => (open ? (open = false) : void openMenu())}
		>
			<Icon name="dots-horizontal" size={14} />
		</button>
		{#if open}
			<div
				class="rca-menu"
				role="menu"
				aria-label="Worth a look actions"
				bind:this={menuEl}
				style={`top:${menuTop}px;right:${menuRight}px;visibility:${menuPositioned ? 'visible' : 'hidden'};`}
			>
				{#each capabilities as capability (capability.kind)}
					<button
						type="button"
						class="rca-menu-item"
						role="menuitem"
						disabled={disabled || busy}
						on:click={() => void run(capability.kind)}
					>
						{capability.label ?? label(capability.kind)}
					</button>
				{/each}
			</div>
		{/if}
	</div>
{/if}

<ResurfacingActionDialog
	open={dialogKind !== null}
	kind={dialogKind}
	{card}
	{detail}
	busy={busy}
	serverError={dialogError}
	uiThreadId={chatThreadId}
	on:cancel={() => (dialogKind = null)}
	on:submit={(event) =>
		void submit(event.detail.kind, event.detail.input, dialogIdempotencyKey)}
/>

<style>
	.rca {
		position: relative;
		display: inline-flex;
	}

	.rca-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0.25rem 0.4rem;
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		background: var(--bg-card);
		color: var(--text-secondary);
		cursor: pointer;
	}

	.rca.compact .rca-btn {
		padding: 0.15rem 0.3rem;
	}

	.rca-btn:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.rca-menu {
		position: fixed;
		z-index: 30;
		min-width: 11rem;
		display: grid;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		box-shadow: 0 6px 18px rgb(0 0 0 / 0.18);
		overflow-y: auto;
	}

	.rca-menu-item {
		text-align: left;
		padding: 0.4rem 0.6rem;
		border: 0;
		background: transparent;
		color: var(--text-primary);
		cursor: pointer;
	}

	.rca-menu-item:hover:not(:disabled) {
		background: var(--bg-soft);
	}

	.rca-menu-item:disabled {
		opacity: 0.5;
		cursor: default;
	}
</style>
