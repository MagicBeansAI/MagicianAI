<script lang="ts">
	import { onMount } from 'svelte';
	import { mediaProvidersStore } from './providers';
	import { mediaPreferencesStore } from './preferences';
	import VoicePersonaPicker from './VoicePersonaPicker.svelte';
	$: providers = $mediaProvidersStore.stages.tts ?? [];
	onMount(() => { void mediaProvidersStore.refresh(); void mediaPreferencesStore.refresh(); });
</script>

<section class="tts-voices" aria-label="Text to speech voices">
	<div class="tts-header">
		<p class="eyebrow">Speech</p>
		<h2>Text to speech voices</h2>
	</div>
	<p>Choose a voice for each speech model, including local engines. These choices are shared across your devices. Select the model in an audio profile to use it.</p>
	{#each providers as provider (provider.option_id)}
		<VoicePersonaPicker bindingId={provider.provider_id} label={provider.label}
			defaultVoice={provider.variant} choices={(provider.capabilities.voices ?? []).map((id) => ({ id, label: id.replaceAll('_', ' ') }))}
			disabled={provider.availability !== 'available'} />
		{#if provider.unavailable_reason}<small>{provider.unavailable_reason}</small>{/if}
	{/each}
	{#if $mediaProvidersStore.resolved && providers.length === 0}<p>No speech models are configured.</p>{/if}
</section>

<style>
	.tts-voices { display: grid; gap: .85rem; padding: 1rem; border: 1px solid var(--border-soft); border-radius: 8px; background: var(--bg-card); }
	h2 { font-size: 1.05rem; font-weight: 600; margin: 0; color: var(--text-primary); }
	p, small { font-size: .82rem; color: var(--text-secondary); margin: 0; }
	.tts-header { display: grid; gap: .2rem; }
	.eyebrow { font-size: .75rem; font-weight: 700; letter-spacing: 0; text-transform: uppercase; }
</style>
