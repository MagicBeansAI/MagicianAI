<script lang="ts">
	import { notifications, type Notification } from '$lib/shared/stores/notifications';
	import { fly } from 'svelte/transition';

	function getSymbolForType(type: Notification['type']): string {
		switch (type) {
			case 'error':
				return '!';
			case 'warning':
				return '~';
			case 'info':
				return 'i';
			case 'success':
				return '+';
			default:
				return '*';
		}
	}
</script>

<div class="toast-container">
	{#each $notifications.notifications as notification (notification.id)}
		<div
			class="toast toast-{notification.type}"
			transition:fly={{ y: 10, duration: 200 }}
		>
			<span class="toast-symbol">[{getSymbolForType(notification.type)}]</span>
			<span class="toast-text">
				{notification.title}{#if notification.message}{': '}{notification.message}{/if}
			</span>
			{#if notification.action}
				<a class="toast-action" href={notification.action.href}>{notification.action.label}</a>
			{/if}
			<button class="toast-close" on:click={() => notifications.removeNotification(notification.id)}>
				x
			</button>
		</div>
	{/each}
</div>

<style>
	.toast-container {
		position: fixed;
		bottom: 0.75rem;
		right: 0.75rem;
		z-index: 10000;
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
		max-width: 320px;
		pointer-events: none;
	}

	.toast {
		pointer-events: auto;
		display: flex;
		align-items: center;
		gap: 0.375rem;
		padding: 0.375rem 0.5rem;
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-default, #ddd8cf);
		border-radius: var(--radius-sm, 6px);
		box-shadow: var(--shadow-sm, 0 2px 8px rgba(0,0,0,0.06));
		font-family: var(--font-mono);
		font-size: 0.75rem;
		line-height: 1.3;
		color: var(--text-primary, #2d2a26);
	}

	.toast-symbol {
		flex-shrink: 0;
		font-weight: 700;
	}

	.toast-error .toast-symbol {
		color: var(--color-error, #d4574a);
	}

	.toast-warning .toast-symbol {
		color: var(--color-warning, #d4a34a);
	}

	.toast-info .toast-symbol {
		color: var(--accent-primary, #3b82f6);
	}

	.toast-success .toast-symbol {
		color: var(--color-success, #5fa67a);
	}

	.toast-text {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.toast-close {
		flex-shrink: 0;
		background: none;
		border: none;
		color: var(--text-muted, #8a847a);
		cursor: pointer;
		padding: 0 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		line-height: 1;
	}

	.toast-action {
		flex-shrink: 0;
		color: var(--accent-primary, #3b82f6);
		font-weight: 700;
		text-decoration: none;
		white-space: nowrap;
	}

	.toast-action:hover {
		text-decoration: underline;
	}

	.toast-close:hover {
		color: var(--text-primary, #2d2a26);
	}

	@media (max-width: 640px) {
		.toast-container {
			left: 0.75rem;
			right: 0.75rem;
			max-width: none;
		}
	}
</style>
