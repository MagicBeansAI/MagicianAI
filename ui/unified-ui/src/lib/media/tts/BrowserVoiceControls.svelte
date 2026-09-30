<script lang="ts">
	/**
	 * Browser (on-device) TTS voice tuning — voice, speed, and pitch for the
	 * `speechSynthesis` fallback. The OS voices available differ per device, so
	 * these prefs are device-local (the `ttsStore` persists them to localStorage,
	 * not the backend). Renders nothing where the platform has no speechSynthesis.
	 *
	 * Shared by the mobile action sheet and the desktop Voice popover so the
	 * voices-loading + handlers live in one place.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { ttsStore } from '$lib/media/tts/store';
	import {
		isBrowserTtsAvailable,
		listVoices,
		type VoiceDescriptor,
	} from '$lib/media/tts/browserTts';
	import { mediaProvidersStore } from '$lib/media/providers';
	import { resolveTtsProviderChoice } from '$lib/media/tts/providerChoices';

	let available = false;
	let voices: VoiceDescriptor[] = [];

	$: voiceName = $ttsStore.prefs.voiceName ?? '';
	$: rate = $ttsStore.prefs.rate;
	$: pitch = $ttsStore.prefs.pitch;

	// Only relevant when the effective TTS resolves to the browser fallback.
	// Backend provider/model selection belongs to the configured Dictation
	// profile, so browser tuning is hidden while backend TTS is available.
	$: effectiveBrowserTts =
		available
		&& resolveTtsProviderChoice($mediaProvidersStore, available).mode
			=== 'browser';

	function refresh(): void {
		voices = listVoices();
	}

	onMount(() => {
		available = isBrowserTtsAvailable();
		refresh();
		if (typeof window !== 'undefined' && 'speechSynthesis' in window) {
			window.speechSynthesis.addEventListener('voiceschanged', refresh);
		}
	});

	onDestroy(() => {
		if (typeof window !== 'undefined' && 'speechSynthesis' in window) {
			window.speechSynthesis.removeEventListener('voiceschanged', refresh);
		}
	});

	function onVoice(event: Event): void {
		ttsStore.setVoice((event.target as HTMLSelectElement).value || null);
	}
	function onRate(event: Event): void {
		ttsStore.setRate(Number((event.target as HTMLSelectElement).value));
	}
	function onPitch(event: Event): void {
		ttsStore.setPitch(Number((event.target as HTMLSelectElement).value));
	}
</script>

{#if effectiveBrowserTts}
	<div class="browser-voice">
		<label class="bv-field">
			<span class="bv-label">On-device voice</span>
			<select value={voiceName} on:change={onVoice}>
				<option value="">System default</option>
				{#each voices as voice (voice.name)}
					<option value={voice.name}>{voice.name}{voice.lang ? ` · ${voice.lang}` : ''}</option>
				{/each}
			</select>
		</label>
		<div class="bv-pair">
			<label class="bv-field">
				<span class="bv-label">Speed</span>
				<select value={String(rate)} on:change={onRate}>
					<option value="0.8">Slow</option>
					<option value="1">Normal</option>
					<option value="1.2">Fast</option>
					<option value="1.5">Faster</option>
				</select>
			</label>
			<label class="bv-field">
				<span class="bv-label">Pitch</span>
				<select value={String(pitch)} on:change={onPitch}>
					<option value="0.8">Low</option>
					<option value="1">Normal</option>
					<option value="1.2">High</option>
				</select>
			</label>
		</div>
	</div>
{/if}

<style>
	.browser-voice {
		display: flex;
		flex-direction: column;
		gap: 6px;
		width: 100%;
	}
	.bv-field {
		display: flex;
		flex-direction: column;
		gap: 2px;
		flex: 1;
		min-width: 0;
	}
	.bv-label {
		font-size: 9px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--text-muted);
	}
	/* Match the compact STT / TTS provider selects (height 26, 11px text). */
	.bv-field select {
		appearance: none;
		width: 100%;
		height: 26px;
		padding: 0 8px;
		border-radius: var(--radius-sm, 8px);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		background: var(--bg-elevated);
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 11px;
	}
	.bv-pair {
		display: flex;
		gap: 6px;
	}
</style>
