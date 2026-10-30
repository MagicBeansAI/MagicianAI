<script lang="ts">
	/**
	 * Mic-mode segmented control for a live voice call:
	 * [mic "Open mic" | hand "Hold to talk"].
	 *
	 * Same vocabulary and shape as iOS and Android. "Hands-free" is the name
	 * of a voice engine, never of a mic mode, so it does not appear here.
	 *
	 * The active segment is filled with the accent; the other is a plain
	 * label. Tapping the inactive segment switches the mode LIVE through
	 * `setPushToTalkMode` (no reconnect); the active segment is inert.
	 * Keyboard: a radiogroup with a roving tab stop — arrow keys switch.
	 */
	import { pushToTalkMode, setPushToTalkMode } from '$lib/media/voice/realtimeVoiceClient';

	export let disabled = false;
	/** Smaller segments for the one-row desktop call pill. */
	export let compact = false;

	type MicMode = 'open' | 'hold';
	const MODES: ReadonlyArray<{ id: MicMode; label: string }> = [
		{ id: 'open', label: 'Open mic' },
		{ id: 'hold', label: 'Hold to talk' }
	];

	$: current = ($pushToTalkMode ? 'hold' : 'open') as MicMode;

	let segmentEls: Record<MicMode, HTMLButtonElement | null> = { open: null, hold: null };

	function select(mode: MicMode): void {
		if (disabled || mode === current) return;
		setPushToTalkMode(mode === 'hold');
	}

	function onKeydown(event: KeyboardEvent): void {
		if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
		event.preventDefault();
		// Two options: every arrow moves to the other one.
		const next: MicMode = current === 'open' ? 'hold' : 'open';
		select(next);
		segmentEls[next]?.focus();
	}
</script>

<div
	class="mic-mode"
	class:mic-mode--compact={compact}
	class:mic-mode--disabled={disabled}
	role="radiogroup"
	aria-label="Mic mode"
>
	{#each MODES as mode (mode.id)}
		<button
			type="button"
			role="radio"
			class="mic-mode__seg"
			class:mic-mode__seg--active={current === mode.id}
			aria-checked={current === mode.id}
			aria-disabled={disabled}
			tabindex={current === mode.id ? 0 : -1}
			bind:this={segmentEls[mode.id]}
			on:click={() => select(mode.id)}
			on:keydown={onKeydown}
		>
			{#if mode.id === 'open'}
				<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
					<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z" />
					<path d="M19 10v2a7 7 0 0 1-14 0v-2" />
					<line x1="12" y1="19" x2="12" y2="22" />
				</svg>
			{:else}
				<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
					<path d="M18 11V6a2 2 0 0 0-4 0" />
					<path d="M14 10V4a2 2 0 0 0-4 0v2" />
					<path d="M10 10.5V6a2 2 0 0 0-4 0v8" />
					<path d="M18 8a2 2 0 1 1 4 0v6a8 8 0 0 1-8 8h-2c-2.8 0-4.5-.86-5.99-2.34l-3.6-3.6a2 2 0 0 1 2.83-2.82L7 15" />
				</svg>
			{/if}
			<span>{mode.label}</span>
		</button>
	{/each}
</div>

<style>
	.mic-mode {
		display: inline-flex;
		align-items: stretch;
		gap: 2px;
		padding: 3px;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: var(--radius-full, 999px);
	}

	.mic-mode__seg {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 6px 12px;
		background: transparent;
		border: none;
		border-radius: var(--radius-full, 999px);
		color: var(--text-secondary, #5f6668);
		font: 600 12px var(--font-primary);
		white-space: nowrap;
		cursor: pointer;
		transition: background 120ms ease, color 120ms ease;
	}

	.mic-mode__seg:hover:not(.mic-mode__seg--active) {
		color: var(--text-primary, #2d3436);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
	}

	.mic-mode__seg:focus-visible {
		outline: 2px solid var(--accent-primary, #c2502a);
		outline-offset: 1px;
	}

	/* `--accent-fill` + `--text-on-accent` is the theme's accent-as-fill pair:
	   white on the deep fills, dark where a bright accent would fail 4.5:1. */
	.mic-mode__seg--active {
		background: var(--accent-fill, var(--accent-primary, #c2502a));
		color: var(--text-on-accent, #ffffff);
		cursor: default;
	}

	.mic-mode--disabled .mic-mode__seg {
		cursor: not-allowed;
	}

	.mic-mode--disabled {
		opacity: 0.6;
	}

	.mic-mode--compact {
		padding: 2px;
	}

	.mic-mode--compact .mic-mode__seg {
		padding: 4px 10px;
		font-size: 11.5px;
	}
</style>
