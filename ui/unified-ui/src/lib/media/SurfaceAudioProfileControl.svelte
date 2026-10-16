<script lang="ts">
	import { onMount } from 'svelte';

	import {
		audioSettingsStore,
		fetchResolvedAudioSurface,
		type AudioStage,
		type AudioStageOption,
		type AudioSurface,
		type AudioSurfaceProfileConfig,
		type ResolvedAudioProfile
	} from './audioSettings';
	import { mediaPreferencesStore, saveMediaPreferences } from './preferences';
	import { mediaProvidersStore } from './providers';
	import VoicePersonaPicker from './VoicePersonaPicker.svelte';
	import AudioChoicePicker from './AudioChoicePicker.svelte';

	export let surface: AudioSurface;
	export let compact = false;
	export let showStages = true;
	export let disabled = false;
	export let ariaLabel: string | undefined = undefined;

	const STAGES: AudioStage[] = [
		'vad',
		'recording_stt',
		'streaming_stt',
		'diarization',
		'tts'
	];
	const SURFACE_LABELS: Record<AudioSurface, string> = {
		dictation: 'Dictation',
		meeting: 'Meeting',
		listening: 'Listening',
		hands_free: 'Hands-free'
	};
	const STAGE_LABELS: Record<AudioStage, string> = {
		vad: 'Voice activity',
		recording_stt: 'Transcription',
		streaming_stt: 'Live transcription',
		diarization: 'Speakers',
		tts: 'Speech model'
	};

	let saving = false;
	let error: string | null = null;
	let resolved: ResolvedAudioProfile | null = null;
	let mounted = false;
	let lastResolutionKey = '';

	$: profiles = Object.entries($mediaProvidersStore.surface_profiles)
		.filter(([, profile]) => profile.surface === surface)
		.sort(([left], [right]) => profileLabel(left).localeCompare(profileLabel(right)));
	$: configuredDefault = $mediaProvidersStore.default_surface_profiles[surface] ?? '';
	$: selectedProfile = $mediaPreferencesStore.preferences.surface_profiles[surface] ?? '';
	$: activeProfileId = selectedProfile || configuredDefault;
	$: activeProfile = $mediaProvidersStore.surface_profiles[activeProfileId] ?? null;
	$: stageOverrides = $mediaPreferencesStore.preferences.surface_stage_options[surface] ?? {};
	$: effectiveStages = STAGES.filter((stage) => stageConfig(activeProfile, stage)?.enabled);
	$: ttsOptions = activeProfile ? optionsForStage('tts') : [];
	$: ttsOptionId = stageOverrides.tts || (resolved?.profile_id === activeProfileId
		? resolved?.stages?.tts?.selected?.option_id : undefined);
	$: ttsOption = ttsOptions.find((option) => option.option_id === ttsOptionId)
		?? ttsOptions.find((option) => option.availability === 'available');
	$: resolutionKey = `${surface}:${selectedProfile}:${JSON.stringify(stageOverrides)}`;
	$: if (mounted && resolutionKey !== lastResolutionKey) {
		lastResolutionKey = resolutionKey;
		void refreshResolved();
	}

	onMount(() => {
		mounted = true;
		void mediaProvidersStore.refresh();
		void mediaPreferencesStore.refresh();
		void audioSettingsStore.refresh().catch(() => null);
		return () => {
			mounted = false;
		};
	});

	function profileLabel(profileId: string): string {
		return profileId
			.replace(/^compat-/, '')
			.replace(/-v\d+$/, '')
			.replaceAll('-', ' ')
			.replace(/\b\w/g, (letter) => letter.toUpperCase());
	}

	function stageConfig(
		profile: AudioSurfaceProfileConfig | null,
		stage: AudioStage
	): AudioSurfaceProfileConfig[AudioStage] | null {
		return profile?.[stage] ?? null;
	}

	function optionsForStage(stage: AudioStage): AudioStageOption[] {
		const configured = stageConfig(activeProfile, stage)?.providers ?? [];
		const byProvider = new Map(
			($mediaProvidersStore.stages[stage] ?? []).map((option) => [option.provider_id, option])
		);
		return configured.flatMap((providerId) => {
			const option = byProvider.get(providerId);
			return option ? [option] : [];
		});
	}

	function optionState(optionId: string): string | null {
		return $audioSettingsStore.settings?.models?.[optionId]?.state ?? null;
	}

	function optionStateLabel(option: AudioStageOption): string {
		const state = optionState(option.option_id);
		if (state === 'loading') return 'loading';
		if (state === 'download_required') return 'downloads on use';
		if (state === 'degraded') return 'degraded';
		if (state === 'unavailable' || option.availability !== 'available') return 'unavailable';
		return 'ready';
	}

	async function refreshResolved(): Promise<void> {
		try {
			resolved = await fetchResolvedAudioSurface(surface);
			error = null;
		} catch (cause) {
			resolved = null;
			error = cause instanceof Error ? cause.message : 'Failed to resolve audio profile';
		}
	}

	async function changeProfile(profileId: string): Promise<void> {
		const clears = Object.fromEntries(STAGES.map((stage) => [stage, ''])) as Record<
			AudioStage,
			string
		>;
		saving = true;
		error = null;
		try {
			await saveMediaPreferences({
				surface_profiles: { [surface]: profileId },
				surface_stage_options: { [surface]: clears }
			});
			await refreshResolved();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to save audio profile';
		} finally {
			saving = false;
		}
	}

	async function changeStage(stage: AudioStage, optionId: string): Promise<void> {
		saving = true;
		error = null;
		try {
			await saveMediaPreferences({
				surface_stage_options: { [surface]: { [stage]: optionId } }
			});
			await refreshResolved();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Failed to save audio provider';
		} finally {
			saving = false;
		}
	}
</script>

<div class:compact class="surface-audio-control" data-surface={surface}>
	<div class="profile-field">
		<span>{compact ? SURFACE_LABELS[surface] : `${SURFACE_LABELS[surface]} profile`}</span>
		<AudioChoicePicker label={ariaLabel ?? `${SURFACE_LABELS[surface]} audio profile`}
			value={selectedProfile} disabled={disabled || saving || profiles.length === 0}
			choices={[{ id: '', label: 'Use configured default' }, ...profiles.map(([id]) => ({ id, label: profileLabel(id) }))]}
			on:select={(event) => changeProfile(event.detail.id)} />
	</div>

	{#if showStages && activeProfile}
		<div class="stage-controls" aria-label={`${SURFACE_LABELS[surface]} audio stages`}>
			{#each effectiveStages as stage (stage)}
				{@const options = optionsForStage(stage)}
				{#if options.length > 0}
					<div class="stage-field">
						<span>{STAGE_LABELS[stage]}</span>
						<AudioChoicePicker label={`${STAGE_LABELS[stage]} provider`} value={stageOverrides[stage] ?? ''}
							disabled={disabled || saving}
							choices={[{ id: '', label: 'Use profile order' }, ...options.map((option) => ({
								id: option.option_id, label: option.label, detail: optionStateLabel(option), disabled: option.availability !== 'available'
							}))]} on:select={(event) => changeStage(stage, event.detail.id)} />
					</div>
				{/if}
			{/each}
		</div>
		{#if activeProfile.tts?.enabled && ttsOption}
			<VoicePersonaPicker bindingId={ttsOption.provider_id} label="Speaking voice"
				defaultVoice={ttsOption.variant} choices={(ttsOption.capabilities.voices ?? []).map((id) => ({ id, label: id.replaceAll('_', ' ') }))}
				disabled={disabled || saving || ttsOption.availability !== 'available'} />
		{/if}
	{/if}

	{#if !compact && resolved}
		<div class="resolved-line">
			<span class:degraded={resolved.degradations.length > 0} class="state-dot"></span>
			<span>{profileLabel(resolved.profile_id)}</span>
			<span class="resolved-source">{resolved.source.replaceAll('_', ' ')}</span>
		</div>
	{/if}
	{#if error}
		<p class="control-error" role="alert">{error}</p>
	{/if}
</div>

<style>
	.surface-audio-control {
		display: grid;
		gap: 0.7rem;
		min-width: 0;
	}

	.profile-field,
	.stage-field {
		display: grid;
		gap: 0.32rem;
		min-width: 0;
	}

	.profile-field > span,
	.stage-field > span {
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 650;
	}


	.stage-controls {
		display: grid;
		gap: 0.55rem;
		grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr));
	}

	.resolved-line {
		align-items: center;
		color: var(--text-secondary);
		display: flex;
		font-size: 0.74rem;
		gap: 0.42rem;
		min-width: 0;
	}

	.resolved-source {
		color: var(--text-muted);
		margin-left: auto;
		text-transform: capitalize;
	}

	.state-dot {
		background: var(--status-success, #2f8f5b);
		border-radius: 50%;
		height: 0.45rem;
		width: 0.45rem;
	}

	.state-dot.degraded {
		background: var(--status-warning, #b9770e);
	}

	.control-error {
		color: var(--status-error, #b42318);
		font-size: 0.72rem;
		line-height: 1.35;
		margin: 0;
	}

	.surface-audio-control.compact {
		gap: 0.42rem;
		width: 100%;
	}

	.compact .profile-field,
	.compact .stage-field {
		align-items: center;
		display: grid;
		gap: 0.4rem;
		grid-template-columns: minmax(4.8rem, auto) minmax(0, 1fr);
	}

	.compact .stage-controls {
		display: grid;
		gap: 0.35rem;
		grid-template-columns: 1fr;
	}

</style>
