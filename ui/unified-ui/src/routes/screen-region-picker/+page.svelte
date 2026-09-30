<script lang="ts">
	import { invoke } from '@tauri-apps/api/core';
	import { listen } from '@tauri-apps/api/event';
	import { onDestroy, onMount } from 'svelte';

	interface SelectionBox {
		x: number;
		y: number;
		width: number;
		height: number;
	}

	let dragging = $state(false);
	let submitting = $state(false);
	let startX = 0;
	let startY = 0;
	let box = $state<SelectionBox | null>(null);
	let unlistenReset: (() => void) | null = null;

	function resetSelection() {
		dragging = false;
		submitting = false;
		startX = 0;
		startY = 0;
		box = null;
	}

	function normalizedBox(x1: number, y1: number, x2: number, y2: number): SelectionBox {
		return {
			x: Math.min(x1, x2),
			y: Math.min(y1, y2),
			width: Math.abs(x2 - x1),
			height: Math.abs(y2 - y1)
		};
	}

	async function cancelSelection() {
		resetSelection();
		try {
			await invoke('cancel_screen_region_selection');
		} catch (error) {
			console.warn('[screen-region-picker] cancel failed:', error);
		}
	}

	async function completeSelection(selection: SelectionBox) {
		if (submitting) return;
		if (selection.width < 4 || selection.height < 4) {
			await cancelSelection();
			return;
		}
		submitting = true;
		try {
			await invoke('complete_screen_region_selection', {
				selection: {
					x: selection.x,
					y: selection.y,
					width: selection.width,
					height: selection.height,
					viewportWidth: window.innerWidth,
					viewportHeight: window.innerHeight
				}
			});
		} catch (error) {
			console.warn('[screen-region-picker] complete failed:', error);
			await cancelSelection();
		} finally {
			resetSelection();
		}
	}

	function onPointerDown(event: PointerEvent) {
		if (submitting) return;
		event.preventDefault();
		const target = event.currentTarget as HTMLElement;
		target.setPointerCapture(event.pointerId);
		dragging = true;
		startX = event.clientX;
		startY = event.clientY;
		box = normalizedBox(startX, startY, startX, startY);
	}

	function onPointerMove(event: PointerEvent) {
		if (!dragging || submitting) return;
		event.preventDefault();
		box = normalizedBox(startX, startY, event.clientX, event.clientY);
	}

	async function onPointerUp(event: PointerEvent) {
		if (!dragging || submitting) return;
		event.preventDefault();
		const target = event.currentTarget as HTMLElement;
		if (target.hasPointerCapture(event.pointerId)) {
			target.releasePointerCapture(event.pointerId);
		}
		dragging = false;
		const selection = normalizedBox(startX, startY, event.clientX, event.clientY);
		box = selection;
		await completeSelection(selection);
	}

	function onContextMenu(event: MouseEvent) {
		event.preventDefault();
		void cancelSelection();
	}

	function onKeyDown(event: KeyboardEvent) {
		if (event.key === 'Escape') {
			event.preventDefault();
			void cancelSelection();
		}
	}

	onMount(async () => {
		resetSelection();
		window.addEventListener('keydown', onKeyDown);
		try {
			unlistenReset = await listen('screen-region-picker-reset', resetSelection);
		} catch (error) {
			console.warn('[screen-region-picker] reset listener failed:', error);
		}
	});

	onDestroy(() => {
		window.removeEventListener('keydown', onKeyDown);
		unlistenReset?.();
	});
</script>

<svelte:head>
	<title>Region Picker</title>
</svelte:head>

<main
	class="picker"
	class:picker--dragging={dragging}
	class:picker--submitting={submitting}
	onpointerdown={onPointerDown}
	onpointermove={onPointerMove}
	onpointerup={onPointerUp}
	oncontextmenu={onContextMenu}
	role="application"
	aria-label="Screen region picker"
>
	<div class="picker__scrim" aria-hidden="true"></div>
	{#if box}
		<div
			class="picker__selection"
			style={`left:${box.x}px;top:${box.y}px;width:${box.width}px;height:${box.height}px;`}
			aria-hidden="true"
		></div>
	{/if}
	<div class="picker__hint" aria-hidden="true">
		<span>{submitting ? 'Capturing' : 'Select area'}</span>
		<kbd>Esc</kbd>
	</div>
</main>

<style>
	:global(html),
	:global(body) {
		margin: 0;
		width: 100%;
		height: 100%;
		background: transparent !important;
		color: transparent;
		overflow: hidden;
		user-select: none;
		transition: none !important;
	}

	:global(body) {
		cursor: crosshair;
	}

	:global(body::before),
	:global(body::after) {
		content: none !important;
		display: none !important;
		background: transparent !important;
	}

	:global(body > div),
	:global(#svelte) {
		background: transparent !important;
	}

	.picker {
		position: fixed;
		inset: 0;
		width: 100vw;
		height: 100vh;
		overflow: hidden;
		background: transparent;
		color: rgba(255, 255, 255, 0.92);
		font-family:
			Inter,
			ui-sans-serif,
			system-ui,
			-apple-system,
			BlinkMacSystemFont,
			'Segoe UI',
			sans-serif;
	}

	.picker__scrim {
		position: absolute;
		inset: 0;
		background: rgba(3, 7, 18, 0.08);
		backdrop-filter: none;
	}

	.picker__selection {
		position: absolute;
		box-sizing: border-box;
		border: 1px solid rgba(255, 255, 255, 0.95);
		background:
			linear-gradient(rgba(255, 255, 255, 0.14), rgba(255, 255, 255, 0.08)),
			rgba(59, 130, 246, 0.18);
		box-shadow:
			0 0 0 9999px rgba(3, 7, 18, 0.22),
			0 16px 48px rgba(0, 0, 0, 0.28),
			inset 0 0 0 1px rgba(59, 130, 246, 0.55);
		pointer-events: none;
	}

	.picker__hint {
		position: fixed;
		left: 50%;
		top: max(22px, calc(env(safe-area-inset-top) + 22px));
		display: inline-flex;
		align-items: center;
		gap: 8px;
		padding: 7px 10px;
		border: 1px solid rgba(255, 255, 255, 0.18);
		border-radius: 8px;
		background: rgba(8, 13, 24, 0.86);
		box-shadow:
			0 8px 28px rgba(0, 0, 0, 0.24),
			0 0 18px rgba(96, 165, 250, 0.2);
		color: rgba(255, 255, 255, 0.94);
		font-size: 12px;
		line-height: 1;
		transform: translateX(-50%);
		pointer-events: none;
	}

	.picker__hint kbd {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 26px;
		height: 20px;
		padding: 0 6px;
		border: 1px solid rgba(255, 255, 255, 0.22);
		border-radius: 5px;
		background: rgba(255, 255, 255, 0.1);
		color: rgba(255, 255, 255, 0.82);
		font: inherit;
		font-size: 11px;
	}

	.picker--submitting {
		cursor: wait;
	}
</style>
