<script lang="ts">
	// Agent-presence orb beside the chat-next composer. Pure CSS; every
	// animation runs on duration tokens and stops under reduced motion.
	import { createEventDispatcher, onDestroy } from 'svelte';
	import { presenceLabel, type PresenceState } from './presence';

	export let state: PresenceState = 'idle';
	/** God mode (`execution.god_mode`) for this chat; `null` = not known yet. */
	export let godMode: boolean | null = null;
	/** The orb is the chat's god-mode switch: clicking asks to toggle it. */
	export let godToggleable = false;

	const dispatch = createEventDispatcher<{ toggleGodMode: void }>();
	$: tipText = godMode ? `${presenceLabel(state)} · God mode on` : presenceLabel(state);

	/** How long the tooltip announces a state change on its own. */
	const AUTO_TIP_MS = 2500;

	let hovering = false;
	let announcing = false;
	let announceTimer: ReturnType<typeof setTimeout> | undefined;
	let lastState: PresenceState | undefined;

	// Pop the tooltip briefly whenever the state changes (not on first mount),
	// so a switch to working / needs-you is noticed without hovering.
	$: if (state !== lastState) {
		if (lastState !== undefined) {
			announcing = true;
			clearTimeout(announceTimer);
			announceTimer = setTimeout(() => (announcing = false), AUTO_TIP_MS);
		}
		lastState = state;
	}

	onDestroy(() => clearTimeout(announceTimer));
</script>

<!-- The orb doubles as the chat's god-mode switch (click to toggle). -->
<button
	type="button"
	class="cn-presence-switch"
	disabled={!godToggleable}
	aria-pressed={godMode === true}
	aria-label={godMode ? 'God mode is on for this chat. Turn it off' : 'God mode is off for this chat. Turn it on'}
	title={godToggleable ? (godMode ? 'God mode on: click to turn off' : 'Click to turn on god mode for this chat') : undefined}
	data-testid="god-mode-switch"
	on:click={() => dispatch('toggleGodMode')}
>
<span
	class="cn-presence"
	data-testid="presence-orb"
	data-state={state}
	data-god={godMode ? 'on' : 'off'}
	role="status"
	on:mouseenter={() => (hovering = true)}
	on:mouseleave={() => (hovering = false)}
>
	<!-- Visual tooltip; screen readers get the live-region label below. -->
	<span class="cn-presence__tip" class:cn-presence__tip--open={hovering || announcing} aria-hidden="true">
		{tipText}
	</span>
	<span class="cn-presence__ring" aria-hidden="true"></span>
	<span class="cn-presence__core" aria-hidden="true"></span>
	<!-- The live region announces its text content; an aria-label change is
	     not reliably announced. -->
	<span class="cn-presence__label">{presenceLabel(state)}</span>
</span>
</button>

<style>
	.cn-presence-switch {
		all: unset;
		display: inline-grid;
		border-radius: 50%;
		cursor: pointer;
	}
	.cn-presence-switch:disabled {
		cursor: default;
	}
	.cn-presence-switch:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: 2px;
	}

	/* God mode on: a steady danger halo around the disc, independent of the
	   presence animations inside it. */
	.cn-presence[data-god='on'] {
		box-shadow:
			0 0 0 2px var(--danger),
			0 0 10px color-mix(in srgb, var(--danger) 45%, transparent);
	}

	.cn-presence {
		position: relative;
		display: inline-grid;
		place-items: center;
		flex: 0 0 auto;
		width: 28px;
		height: 28px;
		/* The "hole": a disc of the composer's own surface around the dot, so
		   the orb stays legible on the dock row's hover highlight, which in
		   the pastel themes is the same soft tint the idle core uses. */
		border-radius: 50%;
		background: var(--bg-elevated);
	}

	/* Tooltip with physics: springs up from the orb with overshoot
	   (--spring-bouncy) and drops back quickly without it (--spring-snappy).
	   Reduced motion collapses the durations, so it simply appears. */
	/* Right-anchored: the orb sits at the right end of the composer's dock row,
	   so a tip centred on it overhung the composer and was clipped on the right
	   for the longer states ("Magician is working"). It grows leftward over the
	   composer instead, with the arrow still under the orb's centre. */
	.cn-presence__tip {
		position: absolute;
		bottom: calc(100% + 8px);
		right: 0;
		z-index: 5;
		padding: 0.2rem 0.55rem;
		border-radius: var(--radius-full);
		background: var(--text-primary);
		color: var(--bg-base);
		font-family: var(--font-primary);
		font-size: 0.72rem;
		font-weight: 600;
		white-space: nowrap;
		pointer-events: none;
		box-shadow: var(--shadow-float);
		transform-origin: calc(100% - 14px) 100%;
		opacity: 0;
		transform: translate(0, 6px) scale(0.6);
		transition:
			transform var(--dur-quick) var(--spring-snappy),
			opacity var(--dur-quick) ease;
	}

	.cn-presence__tip::after {
		content: '';
		position: absolute;
		top: 100%;
		/* Under the orb's centre: half the orb's 28px from the right edge. */
		right: 14px;
		width: 8px;
		height: 8px;
		background: inherit;
		transform: translate(50%, -50%) rotate(45deg);
		border-radius: 1px;
	}

	.cn-presence__tip--open {
		opacity: 1;
		transform: translate(0, 0) scale(1);
		transition:
			transform var(--dur-slow) var(--spring-bouncy),
			opacity var(--dur-quick) ease;
	}

	.cn-presence__label {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip: rect(0 0 0 0);
		white-space: nowrap;
		border: 0;
	}

	.cn-presence__ring,
	.cn-presence__core {
		grid-area: 1 / 1;
		border-radius: 50%;
	}

	.cn-presence__ring {
		width: 100%;
		height: 100%;
		background: conic-gradient(
			from 0deg,
			var(--accent-primary),
			var(--tint-1),
			var(--tint-3),
			var(--tint-5),
			var(--accent-primary)
		);
		/* Hollow ring: keep a 3px band. */
		-webkit-mask: radial-gradient(circle, transparent 58%, #000 60%);
		mask: radial-gradient(circle, transparent 58%, #000 60%);
		opacity: 0;
		transition: opacity var(--dur-base) ease;
	}

	.cn-presence__core {
		width: 16px;
		height: 16px;
		background: radial-gradient(circle at 35% 30%, var(--tint-1), var(--tint-2));
		box-shadow: 0 0 0 0 transparent;
		transition:
			background var(--dur-base) ease,
			box-shadow var(--dur-base) ease,
			transform var(--dur-base) var(--spring-snappy);
	}

	/* idle: slow breathe. */
	[data-state='idle'] .cn-presence__core {
		animation: cn-presence-breathe calc(var(--dur-slow) * 6) ease-in-out infinite;
	}

	/* thinking: rotating conic ring around a softer core. */
	[data-state='thinking'] .cn-presence__ring {
		opacity: 1;
		animation: cn-presence-spin calc(var(--dur-slow) * 3) linear infinite;
	}
	[data-state='thinking'] .cn-presence__core {
		transform: scale(0.8);
		background: radial-gradient(circle at 35% 30%, var(--tint-2), var(--accent-primary-soft));
	}

	/* working: faster accent pulse glow. */
	[data-state='working'] .cn-presence__core {
		background: radial-gradient(
			circle at 35% 30%,
			var(--accent-primary-soft),
			var(--accent-primary)
		);
		animation: cn-presence-pulse calc(var(--dur-slow) * 2) ease-in-out infinite;
	}

	/* needs-you: warning fill, one bounce on entry, then steady. */
	[data-state='needs-you'] .cn-presence__core {
		background: var(--color-warning);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--color-warning) 28%, transparent);
		animation: cn-presence-bounce calc(var(--dur-slow) * 1.2) var(--spring-bouncy) 1 both;
	}

	@keyframes cn-presence-breathe {
		0%,
		100% {
			transform: scale(1);
		}
		50% {
			transform: scale(1.04);
		}
	}

	@keyframes cn-presence-spin {
		to {
			transform: rotate(1turn);
		}
	}

	@keyframes cn-presence-pulse {
		0%,
		100% {
			box-shadow: 0 0 0 0 color-mix(in srgb, var(--accent-primary) 45%, transparent);
		}
		50% {
			box-shadow: 0 0 0 5px color-mix(in srgb, var(--accent-primary) 0%, transparent);
		}
	}

	@keyframes cn-presence-bounce {
		from {
			transform: scale(0.6);
		}
		to {
			transform: scale(1);
		}
	}

	/* The --dur-* tokens collapse to 0ms here already; stop the loops
	   explicitly too, so no zero-length infinite animation is left spinning. */
	@media (prefers-reduced-motion: reduce) {
		.cn-presence__ring,
		.cn-presence__core {
			animation: none !important;
		}
	}
</style>
