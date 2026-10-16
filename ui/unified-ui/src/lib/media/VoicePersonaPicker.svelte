<script lang="ts">
	import { onDestroy } from 'svelte';
	import AudioChoicePicker from './AudioChoicePicker.svelte';
	import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import { mediaPreferencesStore, saveMediaPreferences } from './preferences';

	export let kind: 'tts' | 'realtime' = 'tts';
	export let bindingId: string;
	export let label = 'Voice';
	export let defaultVoice: string | null | undefined = undefined;
	export let choices: { id: string; label: string }[] = [];
	export let disabled = false;

	let field: 'tts_voices' | 'realtime_voices' = 'tts_voices';
	let saving = false;
	let previewing = false;
	let error = '';
	let audio: HTMLAudioElement | null = null;
	let audioUrl: string | null = null;
	let previewAbort: AbortController | null = null;
	$: field = kind === 'tts' ? 'tts_voices' : 'realtime_voices';
	$: selected = $mediaPreferencesStore.preferences[field]?.[bindingId] ?? '';
	$: if (bindingId) { error = ''; stopPreview(); }

	function stopPreview(): void {
		previewAbort?.abort();
		previewAbort = null;
		audio?.pause();
		audio = null;
		if (audioUrl) URL.revokeObjectURL(audioUrl);
		audioUrl = null;
		previewing = false;
	}
	onDestroy(stopPreview);

	async function choose(voice: string): Promise<void> {
		stopPreview();
		saving = true;
		error = '';
		try {
			await saveMediaPreferences({ [field]: { [bindingId]: voice } });
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not save voice';
		} finally { saving = false; }
	}

	async function preview(): Promise<void> {
		if (previewing) { stopPreview(); return; }
		const controller = new AbortController();
		previewAbort = controller;
		previewing = true;
		error = '';
		try {
			const response = await fetch('/api/magician/v2/media/tts/synthesize', {
				method: 'POST', headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
				body: JSON.stringify({ provider: bindingId, voice: selected || defaultVoice || null,
					text: 'Hello! This is a preview of your selected voice.' }), signal: controller.signal
			});
			if (!response.ok) throw new Error('Voice preview failed');
			if (response.headers.get('X-Tts-Fallback') === 'true') {
				throw new Error('This provider is unavailable. Try the preview again later.');
			}
			const blob = await response.blob();
			if (controller.signal.aborted) return;
			audioUrl = URL.createObjectURL(blob);
			audio = new Audio(audioUrl);
			audio.onended = stopPreview;
			audio.onerror = () => { stopPreview(); error = 'Could not play voice preview'; };
			await audio.play();
		} catch (cause) {
			if (controller.signal.aborted) return;
			stopPreview();
			error = cause instanceof Error ? cause.message : 'Voice preview failed';
		}
	}
</script>

<div class="voice-persona-picker" role="group" aria-label={label}>
	<div class="voice-field">
		<span>{label}</span>
		<AudioChoicePicker {label} value={selected} disabled={disabled || saving || choices.length === 0}
			choices={[
				{ id: '', label: 'Provider default', detail: defaultVoice ?? undefined },
				...(selected && !choices.some((voice) => voice.id === selected) ? [{ id: selected, label: selected, detail: 'No longer available', disabled: true }] : []),
				...choices
			]} on:select={(event) => choose(event.detail.id)} />
	</div>
	{#if kind === 'tts' && choices.length > 0}
		<button type="button" on:click={preview} disabled={disabled || saving}>{previewing ? 'Stop preview' : 'Preview voice'}</button>
	{:else if kind === 'realtime'}
		<small>Applies to your next call.</small>
	{/if}
	{#if error}<p role="alert">{error}</p>{/if}
</div>

<style>
	.voice-persona-picker { display: grid; gap: .35rem; min-width: 0; }
	span, small { color: var(--text-secondary); font-size: .75rem; }
	.voice-field { display: grid; align-items: center; grid-template-columns: minmax(4.8rem, auto) minmax(0, 1fr); gap: .4rem; min-width: 0; }
	button { justify-self: start; color: var(--accent-color, var(--text-primary)); font-size: .75rem; padding: .25rem 0; }
	p { color: var(--color-error, #dc2626); font-size: .78rem; margin: 0; }
</style>
