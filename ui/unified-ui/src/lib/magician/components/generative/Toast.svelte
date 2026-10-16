<script lang="ts">
	import { createEventDispatcher, onDestroy } from 'svelte';

	export let message: string = '';
	export let duration: number = 4000;
	export let actionLabel: string = '';
	export let updateToken: unknown = undefined;

	const dispatch = createEventDispatcher<{ action: void; close: void }>();
	let visible = true;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let closeEmitted = false;
	let toastSignature = '';
	let lastUpdateToken: unknown = undefined;

	$: safeDuration = Number.isFinite(duration) && duration > 0 ? duration : 0;
	$: {
		const nextSignature = `${message}:${actionLabel}:${safeDuration}`;
		const tokenChanged = updateToken !== lastUpdateToken;
		if (tokenChanged || nextSignature !== toastSignature) {
			toastSignature = nextSignature;
			lastUpdateToken = updateToken;
			visible = true;
			closeEmitted = false;
			clearTimer();
		}
	}
	$: {
		clearTimer();
		if (safeDuration > 0 && visible) {
			timer = setTimeout(() => {
				hideToast();
			}, safeDuration);
		}
	}

	onDestroy(() => {
		clearTimer();
	});

	function closeToast(): void {
		hideToast();
	}

	function hideToast(): void {
		if (!visible) {
			emitCloseOnce();
			return;
		}
		visible = false;
		clearTimer();
		emitCloseOnce();
	}

	function triggerAction(): void {
		dispatch('action');
	}

	function clearTimer(): void {
		if (!timer) return;
		clearTimeout(timer);
		timer = null;
	}

	function emitCloseOnce(): void {
		if (closeEmitted) return;
		closeEmitted = true;
		dispatch('close');
	}
</script>

{#if visible}
	<div class="muij-toast" role="status" aria-live="polite">
		<span class="muij-toast-message">{message}</span>
		<div class="muij-toast-actions">
			{#if actionLabel.trim().length > 0}
				<button type="button" class="muij-toast-action" on:click={triggerAction}>{actionLabel}</button>
			{/if}
			<button type="button" class="muij-toast-close" aria-label="Dismiss toast" on:click={closeToast}>×</button>
		</div>
	</div>
{/if}

<style>
	.muij-toast {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 10px;
		padding: 8px 10px;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
	}

	.muij-toast-message {
		overflow-wrap: anywhere;
	}

	.muij-toast-actions {
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}

	.muij-toast-action,
	.muij-toast-close {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-xs);
		background: var(--bg-card);
		color: var(--text-body);
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		padding: 4px 6px;
		cursor: pointer;
	}
</style>
