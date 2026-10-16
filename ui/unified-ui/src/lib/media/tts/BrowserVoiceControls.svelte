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
	import AudioChoicePicker from '../AudioChoicePicker.svelte';
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

</script>

{#if effectiveBrowserTts}
	<div class="browser-voice">
		<div class="bv-field">
			<span class="bv-label">On-device voice</span>
			<AudioChoicePicker label="On-device voice" value={voiceName}
				choices={[{ id: '', label: 'System default' }, ...voices.map((voice) => ({ id: voice.name, label: voice.name, detail: voice.lang }))]}
				on:select={(event) => ttsStore.setVoice(event.detail.id || null)} />
		</div>
		<div class="bv-pair">
			<div class="bv-field">
				<span class="bv-label">Speed</span>
				<AudioChoicePicker label="Speed" value={String(rate)} choices={[{id:'0.8',label:'Slow'},{id:'1',label:'Normal'},{id:'1.2',label:'Fast'},{id:'1.5',label:'Faster'}]} on:select={(event) => ttsStore.setRate(Number(event.detail.id))} />
			</div>
			<div class="bv-field">
				<span class="bv-label">Pitch</span>
				<AudioChoicePicker label="Pitch" value={String(pitch)} choices={[{id:'0.8',label:'Low'},{id:'1',label:'Normal'},{id:'1.2',label:'High'}]} on:select={(event) => ttsStore.setPitch(Number(event.detail.id))} />
			</div>
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

	.bv-pair {
		display: flex;
		gap: 6px;
	}
</style>
