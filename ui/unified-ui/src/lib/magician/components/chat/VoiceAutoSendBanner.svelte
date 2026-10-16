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
	/* A flush band inside the composer card, like the other in-card rows:
	   no box border or margin of its own — the card draws the edge and the
	   composer rounds the top corners when this is the first row. */
	.voice-autosend {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 0;
		width: 100%;
		padding: 0;
		margin: 0;
		border: 0;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		border-radius: 0;
		overflow: hidden;
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.1));
		color: var(--text-primary);
		font: inherit;
		text-align: left;
		cursor: pointer;
		animation: voice-autosend-slide-in 180ms ease-out;
	}
	.voice-autosend:focus-visible {
		outline: 2px solid var(--accent-primary, #c2502a);
		outline-offset: -2px;
	}
	.voice-autosend__bar {
		height: 2px;
		width: 100%;
		background: var(--accent-primary, #c2502a);
		transform-origin: left;
		transition: transform 80ms linear;
	}
	.voice-autosend__body {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.35rem 0.75rem 0.4rem 0.875rem;
	}
	.voice-autosend__dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--accent-primary, #c2502a);
		flex-shrink: 0;
		animation: voice-autosend-pulse 1s ease-in-out infinite;
	}
	.voice-autosend__text {
		flex: 1;
		font-size: 12.5px;
		color: var(--text-primary);
	}
	.voice-autosend__text strong {
		font-variant-numeric: tabular-nums;
		font-weight: 600;
	}
	.voice-autosend__cancel {
		flex: none;
		border: 1px solid var(--accent-primary, #c2502a);
		background: transparent;
		color: var(--accent-primary, #c2502a);
		font-size: 12px;
		font-weight: 500;
		line-height: 1.3;
		padding: 0.15rem 0.55rem;
		border-radius: var(--radius-chip, 6px);
		transition:
			background-color 120ms ease,
			color 120ms ease;
	}
	.voice-autosend:hover .voice-autosend__cancel,
	.voice-autosend:focus-visible .voice-autosend__cancel {
		background: var(--accent-fill, #c2502a);
		color: var(--text-on-accent, #fff);
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
