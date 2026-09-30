<script lang="ts">
	import { browser } from '$app/environment';
	import { attentionModalPortal } from '$lib/shared/modalPortal';
	import { createEventDispatcher, onDestroy, tick } from 'svelte';
	const dispatch = createEventDispatcher();
	const FOCUSABLE_SELECTOR = [
		'a[href]',
		'button:not([disabled])',
		'input:not([disabled])',
		'select:not([disabled])',
		'textarea:not([disabled])',
		'[tabindex]:not([tabindex="-1"])'
	].join(',');

	export let open: boolean = false;
	export let title: string = '';
	export let size: 'sm' | 'md' | 'lg' | 'xl' | 'full' = 'lg';
	export let closable: boolean = true;
	export let layer: 'default' | 'attention-child' = 'default';
	export let initialFocusSelector: string = '';
	export let returnFocusSelector: string = '';
	export let idBase: string = '';

	let dialogEl: HTMLElement | null = null;
	let returnFocusEl: HTMLElement | null = null;
	let wasOpen = false;
	$: titleId = `${idBase || `muij-modal-${stableIdPart(title || 'dialog')}`}-title`;

	function stableIdPart(value: string): string {
		let hash = 0;
		for (let index = 0; index < value.length; index += 1) {
			hash = (hash * 31 + value.charCodeAt(index)) >>> 0;
		}
		return hash.toString(36);
	}

	$: if (browser && open && !wasOpen) {
		wasOpen = true;
		returnFocusEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		void focusInitial();
	}

	$: if (browser && !open && wasOpen) {
		wasOpen = false;
		void restoreFocus();
	}

	function close() {
		if (closable) {
			dispatch('close');
		}
	}

	function handleBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) {
			close();
		}
	}

	function focusableElements(): HTMLElement[] {
		if (!dialogEl) return [];
		return Array.from(dialogEl.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
			(element) => element.offsetParent !== null && !element.closest('[inert]')
		);
	}

	async function focusInitial(): Promise<void> {
		await tick();
		await tick();
		if (!open || !dialogEl) return;
		const preferred = initialFocusSelector
			? dialogEl.querySelector<HTMLElement>(initialFocusSelector)
			: null;
		if (preferred && !preferred.hasAttribute('disabled')) {
			preferred.focus();
			return;
		}
		(focusableElements()[0] ?? dialogEl).focus();
	}

	function validRestorationTarget(element: HTMLElement | null): element is HTMLElement {
		return Boolean(element?.isConnected && !element.closest('[inert]'));
	}

	async function restoreFocus(): Promise<void> {
		const previous = returnFocusEl;
		returnFocusEl = null;
		await tick();
		const fallback = returnFocusSelector
			? document.querySelector<HTMLElement>(returnFocusSelector)
			: null;
		const target = validRestorationTarget(previous)
			? previous
			: validRestorationTarget(fallback)
				? fallback
				: document.querySelector<HTMLElement>(
						'main:not([inert]) button:not([disabled]), main:not([inert]) a[href]'
					);
		if (validRestorationTarget(target)) target.focus();
	}

	function trapFocus(event: KeyboardEvent): void {
		if (!open || event.key !== 'Tab' || !dialogEl) return;
		const focusable = focusableElements();
		if (focusable.length === 0) {
			event.preventDefault();
			dialogEl.focus();
			return;
		}
		const first = focusable[0];
		const last = focusable[focusable.length - 1];
		const active = document.activeElement;
		if (!dialogEl.contains(active) || active === dialogEl) {
			event.preventDefault();
			(event.shiftKey ? last : first).focus();
		} else if (event.shiftKey && active === first) {
			event.preventDefault();
			last.focus();
		} else if (!event.shiftKey && active === last) {
			event.preventDefault();
			first.focus();
		}
	}

	function handleKeydown(event: KeyboardEvent) {
		trapFocus(event);
		if (event.defaultPrevented) return;
		if (event.key === 'Escape') {
			event.preventDefault();
			close();
		}
	}

	function handleFocusIn(event: FocusEvent): void {
		if (!open || !dialogEl || dialogEl.contains(event.target as Node | null)) return;
		(focusableElements()[0] ?? dialogEl).focus();
	}

	onDestroy(() => {
		if (browser && wasOpen) void restoreFocus();
	});
</script>

<svelte:window on:keydown={handleKeydown} />
<svelte:document on:focusin={handleFocusIn} />

{#if open}
	<!-- svelte-ignore a11y-no-static-element-interactions -->
	<!-- svelte-ignore a11y_click_events_have_key_events -->
		<div
			class="muij-modal-backdrop"
			class:global-layer-attention-child={layer === 'attention-child'}
			use:attentionModalPortal={layer === 'attention-child'}
			on:click={handleBackdropClick}
		>
			<div
				class="muij-modal muij-modal--{size}"
				data-modal-id={idBase || undefined}
				role="dialog"
				aria-modal="true"
				aria-labelledby={title ? titleId : undefined}
				aria-label={!title ? 'Dialog' : undefined}
				tabindex="-1"
				bind:this={dialogEl}
			>
				{#if title || closable}
					<div class="muij-modal-header">
						{#if title}
							<h2 class="muij-modal-title" id={titleId}>{title}</h2>
					{/if}
					{#if closable}
						<button type="button" class="muij-modal-close" on:click={close} aria-label="Close">&times;</button>
					{/if}
				</div>
			{/if}
			<div class="muij-modal-body">
				<slot />
			</div>
		</div>
	</div>
{/if}

<style>
	:global(body.attention-modal-scroll-lock) {
		overflow: hidden !important;
	}

	.muij-modal-backdrop {
		position: fixed;
		inset: 0;
		z-index: 1000;
		background: rgba(0, 0, 0, 0.5);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 24px;
		animation: muij-modal-fade-in 0.15s ease-out;
	}

	.muij-modal-backdrop.global-layer-attention-child {
		z-index: 1300;
	}

	@keyframes muij-modal-fade-in {
		from { opacity: 0; }
		to { opacity: 1; }
	}

	.muij-modal {
		background: var(--bg-surface, #fff);
		border-radius: 12px;
		box-shadow: 0 20px 60px rgba(0, 0, 0, 0.3);
		max-height: calc(100vh - 48px);
		display: flex;
		flex-direction: column;
		overflow: hidden;
		animation: muij-modal-slide-up 0.2s ease-out;
	}

	.muij-modal:focus {
		outline: none;
	}

	@keyframes muij-modal-slide-up {
		from { transform: translateY(10px); opacity: 0; }
		to { transform: translateY(0); opacity: 1; }
	}

	.muij-modal--sm { width: 400px; max-width: 90vw; }
	.muij-modal--md { width: 560px; max-width: 90vw; }
	.muij-modal--lg { width: 720px; max-width: 90vw; }
	.muij-modal--xl { width: 960px; max-width: 95vw; }
	.muij-modal--full { width: 95vw; height: 90vh; }

	.muij-modal-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 16px 20px;
		border-bottom: 1px solid var(--border-subtle, #e5e7eb);
		flex-shrink: 0;
	}

	.muij-modal-title {
		margin: 0;
		font-size: 16px;
		font-weight: 600;
		color: var(--text-primary, #111827);
	}

	.muij-modal-close {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		border: none;
		background: transparent;
		border-radius: 6px;
		font-size: 20px;
		color: var(--text-secondary, #6b7280);
		cursor: pointer;
		transition: background 0.15s;
	}

	.muij-modal-close:hover {
		background: var(--bg-hover, #f3f4f6);
	}

	.muij-modal-body {
		padding: 20px;
		overflow-y: auto;
		flex: 1;
	}
</style>
