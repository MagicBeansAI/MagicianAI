<script lang="ts">
	import { createEventDispatcher, onDestroy, tick } from 'svelte';

	export let open: boolean = false;
	export let title: string = 'Confirm';
	export let message: string = 'Are you sure?';
	export let confirmLabel: string = 'Confirm';
	export let cancelLabel: string = 'Cancel';

	const dispatch = createEventDispatcher<{ confirm: void; cancel: void }>();
	let overlayEl: HTMLDivElement | null = null;
	let dialogCardEl: HTMLDivElement | null = null;
	let cancelButtonEl: HTMLButtonElement | null = null;
	let confirmButtonEl: HTMLButtonElement | null = null;
	let previouslyFocused: HTMLElement | null = null;
	let renderedOpen = open;
	let previousOpenProp = open;
	let wasOpen = false;

	$: if (open !== previousOpenProp) {
		previousOpenProp = open;
		renderedOpen = open;
	}

	$: if (renderedOpen && !wasOpen) {
		wasOpen = true;
		void activateDialog();
	}

	$: if (!renderedOpen && wasOpen) {
		wasOpen = false;
		restoreFocus();
	}

	onDestroy(() => {
		restoreFocus();
	});

	async function activateDialog(): Promise<void> {
		if (typeof document === 'undefined') return;
		previouslyFocused = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		await tick();
		(cancelButtonEl || confirmButtonEl || firstFocusableElement())?.focus();
	}

	function restoreFocus(): void {
		if (!previouslyFocused) return;
		previouslyFocused.focus();
		previouslyFocused = null;
	}

	function getFocusableElements(): HTMLElement[] {
		if (!dialogCardEl) return [];
		const focusables = dialogCardEl.querySelectorAll<HTMLElement>(
			'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
		);
		return Array.from(focusables).filter((element) => !element.hasAttribute('disabled'));
	}

	function firstFocusableElement(): HTMLElement | null {
		const focusables = getFocusableElements();
		return focusables.length > 0 ? focusables[0] : null;
	}

	function closeWith(result: 'confirm' | 'cancel'): void {
		renderedOpen = false;
		dispatch(result);
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape') {
			event.preventDefault();
			closeWith('cancel');
			return;
		}
		if (event.key !== 'Tab') return;
		const focusables = getFocusableElements();
		if (focusables.length === 0) {
			event.preventDefault();
			return;
		}
		const first = focusables[0];
		const last = focusables[focusables.length - 1];
		const active = typeof document !== 'undefined' && document.activeElement instanceof HTMLElement
			? document.activeElement
			: null;
		if (event.shiftKey) {
			if (!active || active === first || !dialogCardEl?.contains(active)) {
				event.preventDefault();
				last.focus();
			}
			return;
		}
		if (active === last) {
			event.preventDefault();
			first.focus();
		}
	}

	function handleOverlayPointerDown(event: MouseEvent): void {
		if (event.target !== overlayEl) return;
		closeWith('cancel');
	}
</script>

{#if renderedOpen}
	<div
		class="muij-confirm-overlay"
		role="dialog"
		aria-modal="true"
		aria-label={title}
		tabindex="-1"
		bind:this={overlayEl}
		on:keydown={handleKeydown}
		on:mousedown={handleOverlayPointerDown}
	>
		<div class="muij-confirm-card" bind:this={dialogCardEl}>
			<div class="muij-confirm-title">{title}</div>
			<div class="muij-confirm-message">{message}</div>
			<div class="muij-confirm-actions">
				<button
					type="button"
					class="muij-confirm-cancel"
					bind:this={cancelButtonEl}
					on:click={() => closeWith('cancel')}
				>
					{cancelLabel}
				</button>
				<button
					type="button"
					class="muij-confirm-ok"
					bind:this={confirmButtonEl}
					on:click={() => closeWith('confirm')}
				>
					{confirmLabel}
				</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.muij-confirm-overlay {
		position: relative;
		display: grid;
		place-items: center;
		padding: 8px;
		background: color-mix(in srgb, var(--bg-soft) 70%, black 10%);
		border-radius: var(--radius-md);
	}

	.muij-confirm-card {
		width: min(320px, 100%);
		display: grid;
		gap: 8px;
		padding: 10px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
	}

	.muij-confirm-title {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		font-weight: 600;
		color: var(--text-primary);
	}

	.muij-confirm-message {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
		overflow-wrap: anywhere;
	}

	.muij-confirm-actions {
		display: flex;
		justify-content: flex-end;
		gap: 6px;
	}

	.muij-confirm-cancel,
	.muij-confirm-ok {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		padding: 6px 10px;
		cursor: pointer;
	}

	.muij-confirm-cancel {
		background: var(--bg-soft);
		color: var(--text-body);
	}

	.muij-confirm-ok {
		background: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}
</style>
