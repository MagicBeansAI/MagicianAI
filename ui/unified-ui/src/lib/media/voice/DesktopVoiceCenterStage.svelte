<!--
  DesktopVoiceCenterStage — when realtime voice is active on desktop,
  the chat shell pivots: a large voice orb / call surface takes the
  center column with live captions, while the chat ledger collapses
  to a side rail (parent decides whether to render it alongside or
  hide it).

  Per the plan, Phase 5 ("Desktop Voice Mode") — the voice surface is
  promoted from a small floating overlay to the active chat surface,
  with the ledger remaining the durable audit trail beside or below.

  This component renders ONLY the center-stage panel. Parent pages
  conditionally render it next to (or instead of) the message list
  when `$voiceCallStore.state` is live. Layout decision (split vs
  full-bleed) lives in the parent.
-->
<script lang="ts">
	import { onDestroy } from 'svelte';
	import {
		voiceCallStore,
		voiceTranscriptStore,
		stopVoiceCall,
		pushToTalkMode,
		setPushToTalkMode,
		engagePushToTalk,
		releasePushToTalk,
	} from '$lib/media/voice/realtimeVoiceClient';

	$: call = $voiceCallStore;
	$: transcript = $voiceTranscriptStore;
	$: ptt = $pushToTalkMode;

	let nowMs = Date.now();
	let timerId: ReturnType<typeof setInterval> | null = null;

	$: {
		// Keep the timer running through `rotating` too (otherwise
		// it visually freezes / restarts during the periodic session
		// refresh; see ChatPanel's matching `voiceCallLive` change).
		const live =
			call.state === 'connecting'
			|| call.state === 'connected'
			|| call.state === 'reconnecting'
			|| call.state === 'rotating';
		if (live && timerId === null) {
			timerId = setInterval(() => {
				nowMs = Date.now();
			}, 1000);
		} else if (!live && timerId !== null) {
			clearInterval(timerId);
			timerId = null;
		}
	}

	onDestroy(() => {
		if (timerId !== null) clearInterval(timerId);
	});

	$: elapsedMs = call.connectedAt != null ? nowMs - call.connectedAt : null;
	$: elapsedLabel = elapsedMs == null ? '' : formatElapsed(elapsedMs);
	$: stateLabel = (() => {
		switch (call.state) {
			case 'connecting':
				return 'Connecting…';
			case 'reconnecting':
				return 'Reconnecting…';
			case 'rotating':
				// Token rotation is a transient ~2s state during a
				// live call; presenting it as a distinct state
				// causes the label to flicker. Keep the same idle
				// label as `connected` so the user doesn't notice.
				return transcript.assistantSpeaking
					? 'Speaking…'
					: transcript.userSpeaking
						? 'Listening…'
						: ptt
							? 'Push to talk'
							: call.addressingRequired && call.activationPhrase
								? `Listening — start with “${call.activationPhrase}”`
								: 'Listening';
			case 'connected':
				return transcript.assistantSpeaking
					? 'Speaking…'
					: transcript.userSpeaking
						? 'Listening…'
						: ptt
							? 'Push to talk'
							: call.addressingRequired && call.activationPhrase
								? `Listening — start with “${call.activationPhrase}”`
								: 'Listening';
			case 'error':
				return 'Voice error';
			default:
				return 'Idle';
		}
	})();

	function formatElapsed(ms: number): string {
		const total = Math.max(0, Math.floor(ms / 1000));
		const m = Math.floor(total / 60);
		const s = total % 60;
		return `${m}:${s.toString().padStart(2, '0')}`;
	}
</script>

<!--
  Compact one-row voice-call panel — designed to sit just above the
  composer (see `ChatPanel.svelte`). Replaces the previous full
  center-stage layout. Contents (left → right):
    orb · state label · timer · PTT toggle · (Hold-to-talk if PTT on) ·
    End call
  Full transcript / captions still live in the main chat ledger;
  the panel intentionally doesn't duplicate them.
-->
<section
	class="voice-pill"
	class:state-connecting={call.state === 'connecting'}
	class:state-reconnecting={call.state === 'reconnecting'}
	class:state-error={call.state === 'error'}
	aria-label="Realtime voice session"
>
	<header class="stage-header">
		<div class="state-pill">
			<span class="state-dot" aria-hidden="true"></span>
			{#if elapsedLabel}
				<span class="timer" title="Call duration">{elapsedLabel}</span>
			{/if}
			<span class="state-label">{stateLabel}</span>
		</div>
	</header>
	{#if call.activationPhrase && call.state === 'connected'}
		<span class="address-hint">Start with “{call.activationPhrase}”</span>
	{/if}

	{#if call.error}
		<div class="error" role="alert">{call.error}</div>
	{/if}

	<footer class="controls">
		<button
			type="button"
			class="ctl"
			class:active={ptt}
			on:click={() => setPushToTalkMode(!ptt)}
			aria-pressed={ptt}
			title="Push-to-talk mode"
		>
			<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
				<path d="M19 10v2a7 7 0 0 1-14 0v-2" />
				<line x1="12" y1="19" x2="12" y2="23" />
			</svg>
			<span>{ptt ? 'PTT on' : 'PTT off'}</span>
		</button>

		{#if ptt}
			<button
				type="button"
				class="ctl ptt-press"
				on:pointerdown={engagePushToTalk}
				on:pointerup={releasePushToTalk}
				on:pointerleave={releasePushToTalk}
				on:pointercancel={releasePushToTalk}
				aria-label="Hold to talk"
			>
				<span>Hold to talk</span>
			</button>
		{/if}

		<button type="button" class="ctl danger" on:click={stopVoiceCall} title="End call">
			<svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
				<rect x="6" y="6" width="12" height="12" rx="1.5" />
			</svg>
			<span>End call</span>
		</button>
	</footer>
</section>

<style>
	/* Compact one-row pill that floats just above the composer when
	   a realtime call is live. Position: absolute so it does NOT
	   reserve flex space in the chat-main column — composer + chat
	   messages stay exactly where they are when a call starts. The
	   pill overlays on top of the chat-messages-area's lower edge,
	   right above the composer. Nearest positioned ancestor is
	   `.chat-main` (position: fixed in HUD, relative in browser
	   chat) so coordinates are local to the chat container. */
	/* Rounded-rectangle bar pinned above the composer — exactly
	   the visual language of the post-transcription preview row
	   (`.mic-capture--expanded .mic-capture__preview` in
	   MicCaptureButton.svelte): same composer width, rounded
	   corners, padding, accent-tinted background, soft border.
	   Reads as a natural extension of the composer rather than a
	   floating chip. Anchored to `.chat-main` (its nearest
	   positioned ancestor); composer is ~140px tall + 8px gap, so
	   `bottom: 148px`. Matches composer's `max-width: 720` so the
	   pill aligns with the composer edges (no centering offset). */
	/* The voice pill is now rendered INSIDE composer-shell via the
	   `banner` slot in FloatingComposer. So it's automatically:
	   - same width as composer (it's literally inside)
	   - top-aligned (flex column inside composer-shell, banner first)
	   - sharing the composer's rounded border at the top corners
	   No positioning math; the pill just takes its natural place.
	   Just an inline flex row with state + timer + controls. */
	.voice-pill {
		display: flex;
		flex-direction: row;
		flex-wrap: nowrap;
		align-items: center;
		gap: 10px;
		padding: 6px 4px 8px;
		font-family: var(--font-primary);
		font-size: 12.5px;
		color: var(--text-primary);
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.10));
		margin-bottom: 6px;
	}

	.state-label {
		flex: 0 0 auto;
	}
	.timer {
		flex: 0 0 auto;
		font-variant-numeric: tabular-nums;
		font-weight: 600;
		color: var(--text-primary);
	}

	.stage-header {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		flex: 0 0 auto;
	}
	.address-hint {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 11.5px;
		font-weight: 550;
		color: var(--text-muted);
	}
	.state-pill {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		padding: 6px 12px;
		border-radius: var(--radius-full);
		background: var(--accent-primary-soft);
		color: var(--accent-primary-hover, var(--accent-primary));
		font-size: 12.5px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}
	.state-connecting .state-pill,
	.state-reconnecting .state-pill {
		background: var(--accent-secondary-soft);
		color: var(--accent-secondary);
	}
	.state-error .state-pill {
		background: var(--color-warning-soft);
		color: var(--color-warning);
	}
	.state-dot {
		width: 8px;
		height: 8px;
		border-radius: var(--radius-full);
		background: currentColor;
		animation: pulse 1.6s ease-in-out infinite;
	}
	@keyframes pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}
	.timer {
		font-variant-numeric: tabular-nums;
		font-weight: 600;
		color: var(--text-primary);
	}

	.error {
		padding: 8px 12px;
		font-size: 13px;
		color: var(--color-warning);
		background: var(--color-warning-soft);
		border-radius: var(--radius-sm);
	}

	.controls {
		display: inline-flex;
		gap: 8px;
		align-items: center;
		flex: 1 0 auto;
		justify-content: flex-end;
		margin-left: auto;
	}
	.ctl {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 6px 12px;
		border-radius: var(--radius-full);
		border: 1px solid var(--border-default);
		background: transparent;
		color: var(--text-primary);
		font-family: inherit;
		font-size: 12.5px;
		cursor: pointer;
		transition: background-color 120ms ease, color 120ms ease, border-color 120ms ease;
	}
	.ctl:hover {
		border-color: var(--accent-primary);
	}
	.ctl.active {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
		background: var(--accent-primary-soft);
	}
	.ctl.danger {
		background: var(--color-error);
		color: var(--text-on-accent);
		border-color: var(--color-error);
	}
	.ctl.danger:hover {
		background: color-mix(in srgb, var(--color-error) 85%, black);
	}
	.ctl.ptt-press {
		background: var(--accent-primary);
		color: var(--text-on-accent);
		border-color: var(--accent-primary);
		user-select: none;
	}
	.ctl.ptt-press:active {
		transform: scale(0.96);
	}
</style>
