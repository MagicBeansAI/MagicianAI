<!--
  Per-message "speak/stop" affordance for the chat assistant bubble.

  Renders nothing when the browser doesn't support `speechSynthesis`
  (server-rendered HTML, older browsers, locked-down WebViews). Toggles
  between speak ▶ / stop ◼ depending on whether THIS message is the
  active utterance — clicking on a different message's speak button
  silently cancels the prior one (handled by `browserTts.speak`).
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import {
		cancelCurrent as cancelBrowserTts,
		isBrowserTtsAvailable,
		speak as speakBrowser
	} from '$lib/media/tts/browserTts';
	import { cancelProviderSpeak, providerSpeakBlocks } from '$lib/media/tts/providerTts';
	import { resolveTtsProviderChoice } from '$lib/media/tts/providerChoices';
	import { resolveSpeechBlocks, type SpeechBlock } from '$lib/media/tts/speechTags';
	import { mediaProvidersStore } from '$lib/media/providers';
	import { ttsStore } from '$lib/media/tts/store';
	import { tutorAudioFocusStore } from '$lib/media/tts/tutorAudioFocus';
	import { isVoiceCallCaptureState, voiceCallStore } from '$lib/media/voice/realtimeVoiceClient';
	import VoiceOrb from '$lib/media/VoiceOrb.svelte';

	export let messageId: string;
	export let text: string;
	/**
	 * Server-parsed `<speech>` segments off the message envelope. When
	 * present we use them as-is; when absent (no voice-origin, or
	 * message pre-dates the field) we fall back to client-side parsing
	 * of `text` — that still works for typed messages because
	 * `resolveSpeechBlocks` returns one block with the trimmed body
	 * when no tags are present.
	 */
	export let speechSegments: SpeechBlock[] | undefined = undefined;
	export let compact: boolean = false;

	let browserAvailable = false;
	$: prefs = $ttsStore.prefs;
	$: ttsSelection = resolveTtsProviderChoice($mediaProvidersStore, browserAvailable);
	$: hasProvider = ttsSelection.mode === 'backend';
	$: isActive = $ttsStore.activeMessageId === messageId;
	$: voiceCallActive = isVoiceCallCaptureState($voiceCallStore.state);
	$: available = ttsSelection.mode !== 'none' && !$tutorAudioFocusStore && !voiceCallActive;
	// Prefer server-parsed segments; fall back to regex on the raw body
	// only when none are present (legacy messages, manual speak on a
	// typed-turn reply that the LLM never tagged).
	$: speechBlocks = resolveSpeechBlocks(speechSegments, text);
	$: spokenFallback = speechBlocks.map((b) => b.text).join(' ');
	$: hasContent = spokenFallback.length > 0;

	onMount(() => {
		browserAvailable = isBrowserTtsAvailable();
	});

	function cancelEither(reason: 'user' | 'replaced' = 'user'): void {
		cancelBrowserTts(reason);
		cancelProviderSpeak(reason);
	}

	function handleClick(event: MouseEvent): void {
		event.preventDefault();
		event.stopPropagation();
		ttsStore.markUserInteracted();
		if (isActive) {
			cancelEither('user');
			ttsStore.setActive(null);
			return;
		}
		if ($tutorAudioFocusStore || voiceCallActive) return;
		if (!hasContent) return;
		ttsStore.setActive(messageId);
		const onEnd = (status: 'completed' | 'cancelled' | 'error') => {
			// `cancelled` already cleared the active id; only completion
			// / error needs to flip the button back to its idle state.
			if (status !== 'cancelled') {
				ttsStore.setActive(null);
			}
		};
		if (hasProvider) {
			// Browser voices are device-local and must not be sent to the
			// configured backend provider chain.
			void providerSpeakBlocks(
				{
					messageId,
					blocks: speechBlocks,
					provider: null,
					voice: null,
					model: null,
					rate: prefs.rate
				},
				onEnd
			);
			return;
		}
		// Browser TTS doesn't carry expression hints — collapse blocks
		// into one text and speak the whole thing in one call.
		speakBrowser(
			{
				messageId,
				text: spokenFallback,
				voiceName: prefs.voiceName,
				rate: prefs.rate,
				pitch: prefs.pitch
			},
			onEnd
		);
	}
</script>

{#if available && hasContent}
	<button
		type="button"
		class="speak-btn"
		class:speak-btn--compact={compact}
		class:speak-btn--active={isActive}
		aria-pressed={isActive}
		aria-label={isActive ? 'Stop reading message aloud' : 'Read message aloud'}
		title={isActive ? 'Stop' : 'Speak'}
		on:click={handleClick}
	>
		{#if isActive}
			<!-- Pulsing orb while TTS is reading this message aloud.
			     Click target stays the same — tap to stop. -->
			<VoiceOrb state="playing" size={compact ? 14 : 16} />
		{:else}
			<svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
				<path
					d="M3 6 v4 h3 l4 3 V3 L6 6 H3z M11.5 5 a3.5 3.5 0 0 1 0 6"
					fill="none"
					stroke="currentColor"
					stroke-width="1.4"
					stroke-linejoin="round"
					stroke-linecap="round"
				/>
			</svg>
		{/if}
	</button>
{/if}

<style>
	.speak-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		padding: 0;
		margin: 0;
		border-radius: 6px;
		border: 1px solid color-mix(in srgb, currentColor 20%, transparent);
		background: transparent;
		/* The message surface owns its readable foreground. In particular,
		 * user bubbles use --text-on-accent in light themes, which is not
		 * necessarily dark. Keep the icon and all interaction states tied to
		 * that resolved foreground instead of reintroducing global theme text. */
		color: inherit;
		cursor: pointer;
		transition:
			background-color 120ms ease,
			color 120ms ease,
			border-color 120ms ease;
	}
	.speak-btn--compact {
		width: 22px;
		height: 22px;
		border-radius: 4px;
	}
	.speak-btn:hover,
	.speak-btn:focus-visible {
		color: inherit;
		border-color: currentColor;
	}
	.speak-btn--active {
		color: inherit;
		border-color: currentColor;
		background: color-mix(in srgb, currentColor 12%, transparent);
	}
	.speak-btn svg {
		width: 14px;
		height: 14px;
		display: block;
	}
	.speak-btn--compact svg {
		width: 11px;
		height: 11px;
	}
</style>
