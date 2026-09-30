<!--
  NetworkStatusBanner — surfaces an offline indicator at the top of
  the app whenever `navigator.onLine === false`. Reappears on
  reconnect briefly to confirm recovery, then disappears.

  Per the plan, Phase 7 / "Offline And Poor-Connection UX":
  PWA users expect graceful degradation when the network is bad. A
  visible online/offline indicator is the minimum bar — durable
  offline send queueing is a separate, idempotency-keyed follow-up.
-->
<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';

	let online = browser ? navigator.onLine : true;
	let showReconnected = false;
	let reconnectedTimer: ReturnType<typeof setTimeout> | null = null;

	function handleOnline(): void {
		online = true;
		showReconnected = true;
		if (reconnectedTimer != null) clearTimeout(reconnectedTimer);
		reconnectedTimer = setTimeout(() => {
			showReconnected = false;
		}, 2500);
	}

	function handleOffline(): void {
		online = false;
		showReconnected = false;
		if (reconnectedTimer != null) {
			clearTimeout(reconnectedTimer);
			reconnectedTimer = null;
		}
	}

	onMount(() => {
		if (!browser) return;
		window.addEventListener('online', handleOnline);
		window.addEventListener('offline', handleOffline);
	});

	onDestroy(() => {
		if (!browser) return;
		window.removeEventListener('online', handleOnline);
		window.removeEventListener('offline', handleOffline);
		if (reconnectedTimer != null) clearTimeout(reconnectedTimer);
	});
</script>

{#if !online}
	<div class="banner offline" role="status" aria-live="polite">
		<span class="dot" aria-hidden="true"></span>
		<span>You're offline — the assistant will reconnect when the network comes back.</span>
	</div>
{:else if showReconnected}
	<div class="banner reconnected" role="status" aria-live="polite">
		<span class="dot" aria-hidden="true"></span>
		<span>Back online.</span>
	</div>
{/if}

<style>
	.banner {
		position: fixed;
		top: max(8px, env(safe-area-inset-top));
		left: 50%;
		transform: translateX(-50%);
		z-index: 96;
		display: inline-flex;
		align-items: center;
		gap: 8px;
		padding: 7px 14px;
		font-family: var(--font-primary);
		font-size: 12.5px;
		border-radius: var(--radius-full);
		max-width: calc(100vw - 24px);
		box-shadow: var(--shadow-md);
		animation: drop 220ms cubic-bezier(0.22, 1, 0.36, 1);
	}
	@keyframes drop {
		from { transform: translate(-50%, -20%); opacity: 0; }
		to { transform: translate(-50%, 0); opacity: 1; }
	}
	.offline {
		background: var(--color-warning-soft);
		color: var(--color-warning);
		border: 1px solid color-mix(in srgb, var(--color-warning) 32%, transparent);
	}
	.reconnected {
		background: var(--accent-secondary-soft);
		color: var(--accent-secondary);
		border: 1px solid color-mix(in srgb, var(--accent-secondary) 30%, transparent);
	}
	.dot {
		width: 7px;
		height: 7px;
		border-radius: var(--radius-full);
		background: currentColor;
		animation: pulse 1.4s ease-in-out infinite;
	}
	@keyframes pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.45; }
	}
</style>
