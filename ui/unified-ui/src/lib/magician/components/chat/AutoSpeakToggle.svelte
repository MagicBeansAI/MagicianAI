<!--
  Toolbar toggle for "speak every assistant reply" (selected TTS provider).

  Renders nothing on surfaces without `speechSynthesis`. Active state
  uses the same accent treatment as SpeakButton's "currently speaking"
  state so the two affordances feel like a single feature.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { saveMediaPreferences } from '$lib/media/preferences';
	import { isBrowserTtsAvailable } from '$lib/media/tts/browserTts';
	import { mediaProvidersStore } from '$lib/media/providers';
	import { ttsStore } from '$lib/media/tts/store';
	import { isVoiceCallCaptureState, voiceCallStore } from '$lib/media/voice/realtimeVoiceClient';

	export let compact: boolean = true;

	let browserAvailable = false;
	$: providerTts = $mediaProvidersStore.tts;
	$: available = providerTts !== null || browserAvailable;
	$: prefs = $ttsStore.prefs;
	$: autoSpeak = prefs.autoSpeak;
	$: temporarilyMuted = isVoiceCallCaptureState($voiceCallStore.state);
	$: effectiveAutoSpeak = autoSpeak && !temporarilyMuted;

	onMount(() => {
		browserAvailable = isBrowserTtsAvailable();
	});

	function toggleAutoSpeak(): void {
		void saveMediaPreferences({ auto_speak: !autoSpeak }).catch((error) => {
			console.warn('Failed to save auto-speak preference:', error);
		});
	}
</script>

{#if available}
	<button
		type="button"
		class="auto-speak-toggle"
		class:auto-speak-toggle--compact={compact}
		class:auto-speak-toggle--active={effectiveAutoSpeak}
		aria-pressed={effectiveAutoSpeak}
		disabled={temporarilyMuted}
		title={temporarilyMuted
			? 'Replies are temporarily muted during the voice session'
			: autoSpeak ? 'Auto-speak replies: ON' : 'Auto-speak replies: OFF'}
		aria-label={temporarilyMuted
			? 'Auto-speaking replies temporarily muted during voice session'
			: autoSpeak ? 'Disable auto-speaking assistant replies' : 'Enable auto-speaking assistant replies'}
		on:click={toggleAutoSpeak}
	>
		<svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
			<path
				d="M3 6 v4 h3 l4 3 V3 L6 6 H3z"
				fill={effectiveAutoSpeak ? 'currentColor' : 'none'}
				stroke="currentColor"
				stroke-width="1.4"
				stroke-linejoin="round"
				stroke-linecap="round"
			/>
			{#if effectiveAutoSpeak}
				<path
					d="M11.5 5 a3.5 3.5 0 0 1 0 6"
					fill="none"
					stroke="currentColor"
					stroke-width="1.4"
					stroke-linecap="round"
				/>
			{/if}
		</svg>
	</button>
{/if}

<style>
	.auto-speak-toggle {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		padding: 0;
		border-radius: 8px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		background: transparent;
		color: var(--theme-color-foreground-muted, #6b7280);
		cursor: pointer;
		transition:
			color 120ms ease,
			border-color 120ms ease,
			background-color 120ms ease;
	}
	.auto-speak-toggle--compact {
		width: 28px;
		height: 28px;
	}
	.auto-speak-toggle:hover:not(:disabled) {
		color: var(--theme-color-foreground, #111827);
		border-color: var(--theme-color-foreground-muted, #6b7280);
	}
	.auto-speak-toggle--active {
		color: var(--theme-color-accent, #2563eb);
		border-color: var(--theme-color-accent, #2563eb);
		background: color-mix(in srgb, var(--theme-color-accent, #2563eb) 12%, transparent);
	}
	.auto-speak-toggle svg {
		width: 14px;
		height: 14px;
		display: block;
	}
</style>
