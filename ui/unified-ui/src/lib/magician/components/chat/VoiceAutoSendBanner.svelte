<!--
  Sticky banner over the chat composer that counts down the auto-send
  timer after a voice transcript lands. Tap (or any composer input)
  cancels.

  The host page owns timing + cancellation — this component is pure
  presentation. Pass `remainingMs` to drive the bar; null hides it.
-->
<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let remainingMs: number | null = null;
	export let totalMs: number = 2000;

	const dispatch = createEventDispatcher<{ cancel: void }>();

	$: visible = remainingMs !== null;
	$: progress = visible ? Math.max(0, Math.min(1, (remainingMs ?? 0) / totalMs)) : 0;
	$: secondsLeft = visible ? Math.max(0, Math.ceil((remainingMs ?? 0) / 1000)) : 0;
</script>

{#if visible}
	<button
		type="button"
		class="voice-autosend"
		aria-live="polite"
		on:click={() => dispatch('cancel')}
	>
		<div class="voice-autosend__bar" style={`transform: scaleX(${progress});`}></div>
		<div class="voice-autosend__body">
			<span class="voice-autosend__dot" aria-hidden="true"></span>
			<span class="voice-autosend__text">
				Sending in <strong>{secondsLeft}s</strong> — tap or type to cancel
			</span>
			<span class="voice-autosend__cancel" aria-hidden="true">
				Cancel
			</span>
		</div>
	</button>
{/if}

<style>
	.voice-autosend {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 0;
		width: 100%;
		padding: 0;
		margin: 0 0 0.4rem;
		border-radius: 10px;
		overflow: hidden;
		background: color-mix(in srgb, var(--theme-color-accent, #2563eb) 12%, transparent);
		border: 1px solid var(--theme-color-accent, #2563eb);
		font: inherit;
		text-align: left;
		cursor: pointer;
		animation: voice-autosend-slide-in 180ms ease-out;
	}
	.voice-autosend__bar {
		height: 3px;
		width: 100%;
		background: var(--theme-color-accent, #2563eb);
		transform-origin: left;
		transition: transform 80ms linear;
	}
	.voice-autosend__body {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.45rem 0.75rem;
	}
	.voice-autosend__dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--theme-color-accent, #2563eb);
		flex-shrink: 0;
		animation: voice-autosend-pulse 1s ease-in-out infinite;
	}
	.voice-autosend__text {
		flex: 1;
		font-size: 0.85rem;
		color: var(--theme-color-foreground, #111827);
	}
	.voice-autosend__text strong {
		font-variant-numeric: tabular-nums;
		font-weight: 600;
	}
	.voice-autosend__cancel {
		appearance: none;
		border: 1px solid var(--theme-color-accent, #2563eb);
		background: transparent;
		color: var(--theme-color-accent, #2563eb);
		font-size: 0.78rem;
		font-weight: 500;
		padding: 0.2rem 0.6rem;
		border-radius: 4px;
		cursor: pointer;
		transition:
			background-color 120ms ease,
			color 120ms ease;
	}
	.voice-autosend__cancel:hover {
		background: var(--theme-color-accent, #2563eb);
		color: var(--theme-color-on-accent, #fff);
	}
	@keyframes voice-autosend-slide-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}
	@keyframes voice-autosend-pulse {
		0%,
		100% {
			transform: scale(1);
			opacity: 1;
		}
		50% {
			transform: scale(1.4);
			opacity: 0.6;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.voice-autosend {
			animation: none;
		}
		.voice-autosend__dot {
			animation: none;
		}
		.voice-autosend__bar {
			transition: none;
		}
	}
</style>
