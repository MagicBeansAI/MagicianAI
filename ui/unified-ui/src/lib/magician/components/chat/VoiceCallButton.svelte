<!--
  Composer button that starts / stops a realtime voice call.

  Only renders when (a) the backend has a realtime voice provider
  registered AND (b) the current surface advertises mic capability —
  there's no point offering a voice call to a surface that can't
  produce audio.
-->
<script lang="ts">
	import { mediaProvidersStore, realtimeVoiceProfileStore } from '$lib/media/providers';
	import { showError } from '$lib/shared/stores/notifications';
	import { mediaSessionStore } from '$lib/media/store';
	import {
		isVoiceCallCaptureState,
		startVoiceCall,
		stopVoiceCall,
		setPushToTalkMode,
		voiceCallStore,
		voiceMicAnalyser
	} from '$lib/media/voice/realtimeVoiceClient';
	import VoiceOrb from '$lib/media/VoiceOrb.svelte';

	export let compact: boolean = true;
	export let disabled: boolean = false;
	/** Kept for callers; concurrent voice leaves an active typed turn running. */
	export let chatTurnActive: boolean = false;
	export let mode: 'realtime' | 'hands_free' = 'realtime';

	$: providers = $mediaProvidersStore;
	$: mediaSession = $mediaSessionStore.session;
	$: call = $voiceCallStore;
	$: handsFreeConfigured = providers.hands_free_voice === true;
	$: selectedRealtimeProfile = providers.realtime_voice_profiles?.find(
		(profile) => profile.profile_id === $realtimeVoiceProfileStore
	);
	$: providerMissing = mode === 'hands_free' ? !handsFreeConfigured : providers.realtime_voice === null;
	$: micMissing = !Boolean(mediaSession?.capabilities.mic);
	$: available = !providerMissing && !micMissing;
	$: connected = isVoiceCallCaptureState(call.state);
	// Surfaced in the title tooltip so users understand why a click
	// won't open a call instead of the button silently vanishing.
	$: unavailableReason = providerMissing
		? mode === 'hands_free'
			? 'Hands-free VAD, streaming STT, and TTS are not configured on the backend'
			: 'Realtime voice provider is not configured on the backend'
		: micMissing
			? "Mic permission isn't granted on this surface"
			: null;

	function toggle(): void {
		if (!available) {
			// Never fail silently: the button is normally disabled when
			// unavailable, so reaching here means something let the click through
			// (a stale availability read, a surface without the disabled state).
			// Saying why beats a click that appears to do nothing.
			showError(
				'Voice unavailable',
				unavailableReason ?? 'Voice calling is unavailable on this surface.'
			);
			return;
		}
		if (connected) {
			stopVoiceCall();
		} else {
			// Translation is continuous by definition. Assistant profiles preserve
			// the user's PTT choice instead of silently resetting it on every call.
			if (mode === 'realtime' && selectedRealtimeProfile?.mode === 'translation') {
				setPushToTalkMode(false);
			}
			void startVoiceCall({
				threadId: mediaSession?.thread_id ?? null,
				mode,
				realtimeProfile: mode === 'realtime' ? $realtimeVoiceProfileStore : null
			});
		}
	}
</script>

<button
	type="button"
	class="voice-call-btn"
	class:voice-call-btn--compact={compact}
	class:voice-call-btn--active={connected}
	class:voice-call-btn--error={call.state === 'error'}
	class:voice-call-btn--unavailable={!available}
	on:click|preventDefault={toggle}
	disabled={disabled || !available}
	title={unavailableReason
		? `${mode === 'hands_free' ? 'Hands-free' : 'Realtime voice'} unavailable — ${unavailableReason}`
		: call.error
			? `Voice unavailable: ${call.error}`
			: connected
				? `End ${mode === 'hands_free' ? 'hands-free session' : 'voice call'}`
				: `Start ${mode === 'hands_free' ? 'hands-free' : 'voice call'}${chatTurnActive ? ' — current text keeps running' : ''}`}
	aria-label={connected ? 'End voice session' : `Start ${mode === 'hands_free' ? 'hands-free' : 'voice call'}`}
>
		{#if connected}
			<!-- Live audio-reactive orb during the call. The realtime
			     voice client publishes the mic AnalyserNode on
			     `voiceMicAnalyser`; passing it through with the
			     `capturing` state pulses the orb with the user's voice
			     level. Falls back to `listening` (CSS animation only)
			     during `connecting`. -->
			<VoiceOrb
				state={call.state === 'connecting' ? 'idle' : 'capturing'}
				analyser={$voiceMicAnalyser}
				size={compact ? 22 : 26}
			/>
		{:else}
			<!-- The two modes get DIFFERENT glyphs: they sit in the same slot of
			     the composer's split button, so a shared phone icon made
			     Hands-free and Call indistinguishable at rest. Headset = the
			     always-listening cascade; handset = a call you place and end. -->
			{#if mode === 'hands_free'}
				<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
					<path d="M4 13a8 8 0 0 1 16 0" />
					<path d="M4 13v5a2 2 0 0 0 2 2h2v-7H4z" />
					<path d="M20 13v5a2 2 0 0 1-2 2h-2v-7h4z" />
				</svg>
			{:else}
				<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
					<path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72c.13.96.37 1.9.72 2.81a2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45c.91.35 1.85.59 2.81.72A2 2 0 0 1 22 16.92z" />
				</svg>
			{/if}
			{#if !available}
				<!-- Slash overlay marks the button as unavailable so the
				     reason isn't only discoverable through the tooltip.
				     Click still focuses the button so the title surfaces
				     on hover. Theme-driven muted glyph colour. -->
				<svg
					class="voice-call-btn__slash"
					width="14"
					height="14"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					aria-hidden="true"
					focusable="false"
				>
					<line x1="3" y1="3" x2="21" y2="21" />
				</svg>
			{/if}
		{/if}
	</button>

<style>
	/* No silent-fallback `var(--x, #hex)` literals — themes own the
	   palette (same discipline as the DiffStrip overhaul commit). */
	.voice-call-btn {
		position: relative;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		padding: 0;
		border-radius: var(--radius-sm, 8px);
		border: 1px solid var(--border-default);
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		transition:
			color 120ms ease,
			border-color 120ms ease,
			background-color 120ms ease,
			opacity 120ms ease;
	}
	.voice-call-btn--compact {
		width: 28px;
		height: 28px;
	}
	.voice-call-btn:hover:not(:disabled) {
		color: var(--text-primary);
		border-color: var(--text-muted);
	}
	.voice-call-btn--active {
		color: var(--color-error);
		border-color: var(--color-error);
		background: color-mix(in srgb, var(--color-error) 14%, transparent);
	}
	.voice-call-btn--error {
		color: var(--color-warning);
		border-color: var(--color-warning);
		background: color-mix(in srgb, var(--color-warning) 10%, transparent);
	}
	/* Unavailable = provider not configured OR mic permission missing.
	   Stay visible + clickable for the title tooltip to surface the
	   reason; the diagonal slash overlay reads as "off" at a glance. */
	.voice-call-btn--unavailable {
		opacity: 0.55;
	}
	.voice-call-btn--unavailable:hover {
		color: var(--text-muted);
		border-color: var(--border-default);
	}
	.voice-call-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	/* Disabled-because-unavailable keeps the slightly-stronger 0.55
	   opacity so the slash glyph stays legible. */
	.voice-call-btn--unavailable:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}
	.voice-call-btn svg {
		width: 16px;
		height: 16px;
		display: block;
	}
	.voice-call-btn--compact svg {
		width: 14px;
		height: 14px;
	}
	.voice-call-btn__slash {
		position: absolute;
		inset: 0;
		margin: auto;
		color: var(--text-muted);
		pointer-events: none;
	}
</style>
