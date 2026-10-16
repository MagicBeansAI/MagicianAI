<script lang="ts">
	/**
	 * Export menu — download a task's shareable outputs as a self-contained
	 * ZIP bundle or a single inlined HTML via
	 * `GET /v3/tasks/{id}/export?format=zip|single-html`.
	 *
	 * Mounted in the task drawer's `actions` slot by every surface that offers
	 * an export, so they all build identical bearer-authenticated export requests.
	 * Menu semantics follow TaskCardMenu:
	 * `role="menu"` with `role="menuitem"` buttons, ArrowUp/Down/Home/End
	 * focus movement, Escape closes and refocuses the trigger (shared
	 * `menuKeydown` mechanics), outside pointerdown closes (shared
	 * `clickOutside` action).
	 */
	import { tick } from 'svelte';
	import { get } from 'svelte/store';
	import { clickOutside } from '$lib/shared/clickOutside';
	import { createMenuKeydown, menuFocusableItems } from '$lib/shared/menuKeydown';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

	export let taskId: string;

	let open = false;
	let triggerEl: HTMLButtonElement | null = null;
	let menuEl: HTMLDivElement | null = null;

	function exportUrl(format: 'zip' | 'single-html'): string {
		const scope = get(scopeIdentityStore);
		const params = new URLSearchParams();
		params.set('format', format);
		return `/api/magician/v3/tasks/${encodeURIComponent(taskId ?? '')}/export?${params.toString()}`;
	}

	function doExport(format: 'zip' | 'single-html'): void {
		open = false;
		if (!taskId) return;
		// Server responds with `Content-Disposition: attachment`, so this
		// triggers a download rather than navigating.
		window.open(exportUrl(format), '_blank');
	}

	async function toggle(): Promise<void> {
		open = !open;
		if (open) {
			await tick();
			menuFocusableItems(menuEl)[0]?.focus();
		}
	}

	function close(refocusTrigger: boolean): void {
		open = false;
		if (refocusTrigger) triggerEl?.focus();
	}

	const handleMenuKeydown = createMenuKeydown({ getMenuEl: () => menuEl, close });
</script>

<div class="export-menu">
	<button
		type="button"
		class="export-menu-trigger"
		bind:this={triggerEl}
		on:click|stopPropagation={toggle}
		aria-haspopup="menu"
		aria-expanded={open}
		title="Download a shareable copy of this task's outputs"
	>
		Export <Icon name="chevron-down" size={12} />
	</button>
	{#if open}
		<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
		<div
			bind:this={menuEl}
			class="export-menu-list"
			role="menu"
			aria-orientation="vertical"
			aria-label="Export task outputs"
			tabindex="-1"
			use:clickOutside={{ handler: () => close(false), exclude: [triggerEl] }}
			on:keydown={handleMenuKeydown}
		>
			<button type="button" role="menuitem" on:click={() => doExport('zip')}>
				ZIP bundle <small>(all outputs)</small>
			</button>
			<button type="button" role="menuitem" on:click={() => doExport('single-html')}>
				Single HTML <small>(self-contained)</small>
			</button>
		</div>
	{/if}
</div>

<style>
	.export-menu {
		position: relative;
	}
	.export-menu-trigger {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		background: none;
		border: 1px solid var(--border-default, #2a2a33);
		border-radius: 0.45rem;
		color: var(--text-muted, #8f9799);
		cursor: pointer;
		font-size: 0.78rem;
		padding: 0.25rem 0.5rem;
		transition:
			color 0.12s ease,
			border-color 0.12s ease;
	}
	.export-menu-trigger:hover {
		color: var(--text-primary, #2d3436);
		border-color: var(--text-muted, #8f9799);
	}
	.export-menu-list {
		position: absolute;
		top: calc(100% + 0.3rem);
		right: 0;
		z-index: 20;
		min-width: 13rem;
		display: flex;
		flex-direction: column;
		padding: 0.25rem;
		border: 1px solid var(--border-default, #2a2a33);
		border-radius: 0.6rem;
		background: var(--bg-elevated, #1b1b22);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.25);
	}
	.export-menu-list button {
		display: flex;
		align-items: baseline;
		gap: 0.35rem;
		width: 100%;
		text-align: left;
		background: none;
		border: none;
		border-radius: 0.4rem;
		color: var(--text-primary, #2d3436);
		cursor: pointer;
		font-size: 0.82rem;
		padding: 0.4rem 0.5rem;
		transition: background 0.12s ease;
	}
	.export-menu-list button:hover,
	.export-menu-list button:focus-visible {
		background: color-mix(in srgb, var(--text-primary, #2d3436) 8%, transparent);
		outline: none;
	}
	.export-menu-list small {
		color: var(--text-muted, #8f9799);
		font-size: 0.72rem;
	}
</style>
