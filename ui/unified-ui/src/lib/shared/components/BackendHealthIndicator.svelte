<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { browser } from '$app/environment';
	import { v2Events, type ConnectionStatus } from '$lib/realtime/v2-websocket';

	/** When true, render an inline pill suitable for placement in the v5
	 * TopBar (next to the brand) instead of the floating absolute banner.
	 * The compact form is height-bounded to fit a 48px topbar row. */
	export let compact = false;

	let connectionStatus: ConnectionStatus = 'disconnected';
	let unsubscribe: (() => void) | null = null;

	// Derived state
	$: isHealthy = connectionStatus === 'connected';

	onMount(() => {
		if (browser) {
			// Subscribe to connection status
			unsubscribe = v2Events.connectionStatus.subscribe((status) => {
				connectionStatus = status;
			});

			// Initiate global WebSocket connection if not already connected
			if (!v2Events.isConnected) {
				v2Events.connectGlobal();
			}
		}
	});

	onDestroy(() => {
		if (unsubscribe) {
			unsubscribe();
		}
		// Note: We don't disconnect on destroy as other components may be using the connection
	});
</script>

{#if !isHealthy}
	<div class="health-banner" class:health-banner--compact={compact}>
		<div class="health-content">
			<svg class="health-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<circle cx="12" cy="12" r="10" />
				<line x1="12" x2="12" y1="8" y2="12" />
				<line x1="12" x2="12.01" y1="16" y2="16" />
			</svg>
			<span class="health-message">
				Realtime unavailable
			</span>
		</div>
	</div>
{/if}

<style>
	.health-banner {
		position: absolute;
		top: 0.75rem;
		left: 50%;
		transform: translateX(-50%);
		z-index: 9999;
		background: rgba(220, 38, 38, 0.25);
		backdrop-filter: blur(2px);
		color: white;
		padding: 0.35rem 1rem;
		border-radius: 9999px;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
		animation: slideDown 0.4s cubic-bezier(0.16, 1, 0.3, 1);
		border: 1px solid rgba(255, 255, 255, 0.1);
	}

	/* Compact mode — inline pill for the v5 TopBar. Strips the absolute
	   positioning, slide-in animation, and viewport-anchoring; renders as
	   a small static pill that flows next to the brand. */
	.health-banner--compact {
		position: static;
		top: auto;
		left: auto;
		transform: none;
		z-index: auto;
		background: var(--color-error-soft, rgba(220, 38, 38, 0.12));
		backdrop-filter: none;
		color: var(--color-error, #d4453a);
		padding: 4px 10px;
		border-radius: 999px;
		box-shadow: none;
		animation: none;
		border: 1px solid color-mix(in srgb, var(--color-error, #d4453a) 28%, transparent);
		display: inline-flex;
		align-items: center;
		height: 26px;
	}

	.health-banner--compact .health-message {
		font-size: 11.5px;
	}

	.health-banner--compact .health-icon {
		width: 12px;
		height: 12px;
	}

	@keyframes slideDown {
		from {
			transform: translate(-50%, -200%);
			opacity: 0;
		}
		to {
			transform: translate(-50%, 0);
			opacity: 1;
		}
	}

	.health-content {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		white-space: nowrap;
	}

	.health-icon {
		width: 14px;
		height: 14px;
		flex-shrink: 0;
	}

	.health-message {
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.01em;
	}

	/* Retro Theme Overrides */
	:global([data-theme^="retro-16bit"]) .health-banner {
		background: var(--bg-base) !important;
		border: 2px solid var(--text-primary) !important;
		border-radius: 0 !important;
		color: var(--text-primary) !important;
		box-shadow: 4px 4px 0 var(--text-muted) !important;
		backdrop-filter: none !important;
	}

	:global([data-theme^="retro-16bit"]) .health-message {
		font-family: var(--font-mono) !important;
		text-transform: uppercase !important;
	}
</style>
