<script lang="ts">
	import { onMount } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		audioSettingsStore,
		controlAudioEngineModels,
		type AudioModelLifecycleState,
		type AudioModelRuntimeStatus,
		type AudioSettingsResponse,
		type AudioStage,
		type AudioStageOption,
		type AudioSurface,
		type AudioSurfaceProfileConfig
	} from './audioSettings';
	import { mediaProvidersStore } from './providers';

	const SURFACES: AudioSurface[] = ['dictation', 'meeting', 'listening', 'hands_free'];
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
		recording_stt: 'Recording STT',
		streaming_stt: 'Streaming STT',
		diarization: 'Diarization',
		tts: 'Text to speech'
	};

	let activeSurface: AudioSurface = 'dictation';
	let advanced = false;
	let saving = false;
	let operatingModels: string[] = [];
	let isMac = false;

	$: settings = $audioSettingsStore.settings;
	$: profiles = settings
		? Object.entries(settings.profiles)
				.filter(([, profile]) => profile.surface === activeSurface)
				.sort(([left], [right]) => profileLabel(left).localeCompare(profileLabel(right)))
		: [];
	$: defaultProfileId = settings?.default_profiles[activeSurface] ?? '';
	$: activeProfile = (settings?.profiles[defaultProfileId] ?? null) as AudioSurfaceProfileConfig | null;
	$: visibleStages = STAGES.filter((stage) => supportsStage(activeSurface, stage));
	$: macosSpeechOption = settings?.stages.recording_stt?.find(
		(option) => option.provider_id === 'macos_speech'
	) ?? null;
	$: macosSystemVoiceOption = settings?.stages.tts?.find(
		(option) => option.provider_id === 'macos_tts'
	) ?? null;
	$: macosSpeechUnavailable = macosSpeechOption !== null && macosSpeechOption.availability !== 'available';
	$: macosSystemVoiceUnavailable = macosSystemVoiceOption !== null && macosSystemVoiceOption.availability !== 'available';

	onMount(() => {
		isMac =
			navigator.platform.toLowerCase().includes('mac') && (navigator.maxTouchPoints ?? 0) < 2;
		void audioSettingsStore.refresh().catch((error) => {
			showError(error instanceof Error ? error.message : 'Failed to load voice and audio settings');
		});
		const statusTimer = window.setInterval(() => {
			if (!saving) void audioSettingsStore.refresh().catch(() => null);
		}, 5_000);
		return () => window.clearInterval(statusTimer);
	});

	function profileLabel(profileId: string): string {
		return profileId
			.replace(/^compat-/, '')
			.replace(/-v\d+$/, '')
			.replaceAll('-', ' ')
			.replace(/\b\w/g, (letter) => letter.toUpperCase());
	}

	function supportsStage(surface: AudioSurface, stage: AudioStage): boolean {
		if (surface === 'dictation') return stage === 'recording_stt' || stage === 'tts';
		return stage !== 'recording_stt';
	}

	function profileStage(stage: AudioStage) {
		return activeProfile?.[stage] ?? null;
	}

	function catalogOptions(stage: AudioStage): AudioStageOption[] {
		return settings?.stages[stage] ?? [];
	}

	function optionByProvider(stage: AudioStage, providerId: string): AudioStageOption | null {
		return catalogOptions(stage).find((option) => option.provider_id === providerId) ?? null;
	}

	function availableAdditions(stage: AudioStage): AudioStageOption[] {
		const selected = new Set(profileStage(stage)?.providers ?? []);
		return catalogOptions(stage).filter(
			(option) => !selected.has(option.provider_id) && option.availability !== 'disabled'
		);
	}

	function lifecycleState(
		option: AudioStageOption | null,
		snapshot: AudioSettingsResponse | null
	): AudioModelLifecycleState {
		if (!option) return 'unavailable';
		return snapshot?.models?.[option.option_id]?.state ??
			(option.availability === 'available' ? 'ready' : 'unavailable');
	}

	function runtimeStatus(
		option: AudioStageOption | null,
		snapshot: AudioSettingsResponse | null
	): AudioModelRuntimeStatus | null {
		if (!option) return null;
		return snapshot?.models?.[option.option_id] ?? null;
	}

	function lifecycleLabel(state: AudioModelLifecycleState): string {
		if (state === 'download_required') return 'Download on use';
		return state.replaceAll('_', ' ').replace(/\b\w/g, (letter) => letter.toUpperCase());
	}

	async function savePatch(patch: Parameters<typeof audioSettingsStore.save>[0]): Promise<void> {
		if (!settings || saving) return;
		saving = true;
		try {
			await audioSettingsStore.save(patch);
			await mediaProvidersStore.refresh();
			showSuccess('Voice and audio settings saved');
		} catch (error) {
			showError(error instanceof Error ? error.message : 'Failed to save voice and audio settings');
		} finally {
			saving = false;
		}
	}

	async function changeDefaultProfile(event: Event): Promise<void> {
		if (!settings) return;
		const profileId = (event.currentTarget as HTMLSelectElement).value;
		if (!profileId) return;
		await savePatch({
			expected_revision: settings.revision,
			default_profiles: { [activeSurface]: profileId }
		});
	}

	async function toggleStage(stage: AudioStage): Promise<void> {
		if (!settings || !activeProfile || !defaultProfileId) return;
		const config = profileStage(stage);
		if (!config || config.required) return;
		await savePatch({
			expected_revision: settings.revision,
			profiles: {
				[defaultProfileId]: { stages: { [stage]: { enabled: !config.enabled } } }
			}
		});
	}

	async function toggleEngine(engineId: string, enabled: boolean): Promise<void> {
		if (!settings) return;
		await savePatch({
			expected_revision: settings.revision,
			engines: { [engineId]: { enabled: !enabled } }
		});
	}

	async function moveProvider(stage: AudioStage, index: number, delta: -1 | 1): Promise<void> {
		if (!settings || !defaultProfileId) return;
		const providers = [...(profileStage(stage)?.providers ?? [])];
		const target = index + delta;
		if (target < 0 || target >= providers.length) return;
		[providers[index], providers[target]] = [providers[target], providers[index]];
		await savePatch({
			expected_revision: settings.revision,
			profiles: { [defaultProfileId]: { stages: { [stage]: { providers } } } }
		});
	}

	async function removeProvider(stage: AudioStage, providerId: string): Promise<void> {
		if (!settings || !defaultProfileId) return;
		const providers = (profileStage(stage)?.providers ?? []).filter((id) => id !== providerId);
		if (profileStage(stage)?.enabled && providers.length === 0) return;
		await savePatch({
			expected_revision: settings.revision,
			profiles: { [defaultProfileId]: { stages: { [stage]: { providers } } } }
		});
	}

	async function addProvider(stage: AudioStage, event: Event): Promise<void> {
		if (!settings || !defaultProfileId) return;
		const select = event.currentTarget as HTMLSelectElement;
		const providerId = select.value;
		if (!providerId) return;
		const providers = [...(profileStage(stage)?.providers ?? []), providerId];
		select.value = '';
		await savePatch({
			expected_revision: settings.revision,
			profiles: { [defaultProfileId]: { stages: { [stage]: { providers } } } }
		});
	}

	async function controlModel(
		option: AudioStageOption,
		action: 'prewarm' | 'unload'
	): Promise<void> {
		const operationKey = `${option.engine_id}:${option.provider_id}`;
		if (operatingModels.includes(operationKey)) return;
		operatingModels = [...operatingModels, operationKey];
		try {
			const response = await controlAudioEngineModels(option.engine_id, action, [option.provider_id]);
			audioSettingsStore.adopt(response.settings);
			showSuccess(action === 'prewarm' ? 'Audio model is ready' : 'Audio model unloaded');
		} catch (error) {
			showError(error instanceof Error ? error.message : `Failed to ${action} audio model`);
		} finally {
			operatingModels = operatingModels.filter((key) => key !== operationKey);
		}
	}
</script>

<section class="audio-settings" aria-labelledby="audio-settings-title">
	<header class="audio-header">
		<div>
			<p class="eyebrow">Voice and audio</p>
			<h2 id="audio-settings-title">Surface profiles</h2>
		</div>
		<div class="header-actions">
			<button
				type="button"
				class:active={advanced}
				class="advanced-button"
				aria-pressed={advanced}
				on:click={() => (advanced = !advanced)}
			>
				<Icon name="settings" size={15} />
				Advanced
			</button>
			<button
				type="button"
				class="icon-button"
				title="Refresh voice and audio settings"
				aria-label="Refresh voice and audio settings"
				disabled={$audioSettingsStore.loading}
				on:click={() => audioSettingsStore.refresh()}
			>
				<Icon name="rotate-ccw" size={16} />
			</button>
		</div>
	</header>

	<nav class="surface-tabs" aria-label="Audio surfaces">
		{#each SURFACES as surface (surface)}
			<button
				type="button"
				class:active={activeSurface === surface}
				on:click={() => (activeSurface = surface)}
			>
				{SURFACE_LABELS[surface]}
			</button>
		{/each}
	</nav>

	{#if $audioSettingsStore.loading && !settings}
		<div class="audio-skeleton" aria-label="Loading voice and audio settings">
			<span></span><span></span><span></span>
		</div>
	{:else if settings}
		<div class="profile-row">
			<label>
				<span>Configured default</span>
				<select
					value={defaultProfileId}
					disabled={saving || profiles.length === 0}
					on:change={changeDefaultProfile}
				>
					{#if profiles.length === 0}
						<option value="">No profile available</option>
					{/if}
					{#each profiles as [profileId] (profileId)}
						<option value={profileId}>{profileLabel(profileId)}</option>
					{/each}
				</select>
			</label>
			{#if activeProfile}
				<div class="profile-meta">
					<span>{activeProfile.turn_boundary.replaceAll('_', ' ')}</span>
					<span>Revision {settings.revision.slice(0, 8)}</span>
				</div>
			{/if}
		</div>

		<div class="engine-strip" aria-label="Audio engine status">
			{#each Object.values(settings.engines) as engine (engine.engine_id)}
				<div class="engine-status" class:unavailable={!engine.available} class:degraded={engine.available && engine.healthy === false}>
					<span class="engine-label"><i aria-hidden="true"></i>{engine.label}{engine.enabled ? '' : ' · Off'}</span>
					{#if engine.engine_id === 'fluid_audio'}
						<button
							type="button"
							class:enabled={engine.enabled}
							class="engine-toggle"
							role="switch"
							aria-checked={engine.enabled}
							aria-label={`${engine.enabled ? 'Disable' : 'Enable'} ${engine.label} engine`}
							title={`${engine.enabled ? 'Disable' : 'Enable'} ${engine.label}`}
							disabled={saving}
							on:click={() => toggleEngine(engine.engine_id, engine.enabled)}
						><span></span></button>
					{/if}
				</div>
			{/each}
		</div>

		{#if isMac && (macosSpeechUnavailable || macosSystemVoiceUnavailable)}
			<div class="macos-speech-help" role="status">
				<div>
					<strong>Mac speech services need Magican Desktop</strong>
					{#if macosSystemVoiceUnavailable}
						<p>Magican Desktop's host speech gateway or helper was unavailable when Magician started. macOS System Voice does not need Speech Recognition permission. Start Magican Desktop, then restart Magician so the provider catalog can include it.</p>
					{:else}
						<p>Magican Desktop is available, but macOS Speech still needs permission for its native Speech helper.</p>
					{/if}
					{#if macosSpeechUnavailable}
						<p>Open Privacy &amp; Security → Speech Recognition, then use Magican Desktop's permission checklist to request access. Restart Magician after access is allowed.</p>
					{/if}
				</div>
				{#if macosSpeechUnavailable}
					<a
						class="macos-settings-link"
						href="x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition"
					>Open Speech Recognition Settings</a>
				{/if}
			</div>
		{/if}

		{#if advanced}
			<div class="engine-observability" aria-label="Audio engine observability">
				{#each Object.values(settings.engines).filter((engine) => engine.can_manage_models) as engine (engine.engine_id)}
					<div class="engine-runtime-row">
						<div class="engine-runtime-copy">
							<strong>{engine.label}</strong>
							<span>{engine.endpoint ?? 'Local engine'} · {engine.startup_policy?.replaceAll('_', ' ') ?? 'managed'}</span>
						</div>
						<dl>
							<div><dt>Resident</dt><dd>{engine.resident_models ?? 0}/{engine.max_resident_models ?? '–'}</dd></div>
							<div><dt>Sessions</dt><dd>{engine.active_sessions ?? 0}/{engine.max_streaming_sessions ?? '–'}</dd></div>
							<div><dt>Starts</dt><dd>{engine.start_count ?? 0}</dd></div>
							<div><dt>Restarts</dt><dd>{engine.restart_count ?? 0}</dd></div>
							<div><dt>Model idle</dt><dd>{engine.model_idle_secs ? `${engine.model_idle_secs}s` : '–'}</dd></div>
							<div><dt>Process</dt><dd>{engine.process_id ?? 'Idle'}</dd></div>
						</dl>
						{#if engine.last_error}<p role="status">{engine.last_error}</p>{/if}
					</div>
				{/each}
			</div>
		{/if}

		{#if activeProfile}
			<div class="stage-list">
				{#each visibleStages as stage (stage)}
					{@const config = profileStage(stage)}
					{#if config}
						{@const providers = config.providers ?? []}
						<article class:disabled={!config.enabled} class="stage-row">
							<div class="stage-heading">
								<div>
									<strong>{STAGE_LABELS[stage]}</strong>
									<span>{config.enabled ? `${providers.length} provider${providers.length === 1 ? '' : 's'}` : 'Off'}</span>
								</div>
								<button
									type="button"
									class:enabled={config.enabled}
									class="stage-toggle"
									role="switch"
									aria-checked={config.enabled}
									aria-label={`${config.enabled ? 'Disable' : 'Enable'} ${STAGE_LABELS[stage]}`}
									disabled={saving || config.required}
									on:click={() => toggleStage(stage)}
								><span></span></button>
							</div>

							{#if config.enabled || advanced}
								<div class="provider-chain">
									{#each providers as providerId, index (`${stage}:${providerId}`)}
										{@const option = optionByProvider(stage, providerId)}
										{@const state = lifecycleState(option, settings)}
										{@const runtime = runtimeStatus(option, settings)}
										<div class="provider-row">
											<span class="provider-order">{index + 1}</span>
											<div class="provider-copy">
												<strong>{option?.label ?? providerId}</strong>
												<span>{option?.model_id ?? 'Configured provider'}</span>
											</div>
											<span class={`model-state state-${state}`}>{lifecycleLabel(state)}</span>
											{#if advanced && option && runtime?.can_load}
												<button
													type="button"
													class="model-action"
													title="Load audio model"
													aria-label={`Load ${option.label}`}
													disabled={operatingModels.includes(`${option.engine_id}:${option.provider_id}`)}
													on:click={() => controlModel(option, 'prewarm')}
												><Icon name="play" size={13} /></button>
											{:else if advanced && option && runtime?.can_unload}
												<button
													type="button"
													class="model-action"
													title="Unload audio model"
													aria-label={`Unload ${option.label}`}
													disabled={operatingModels.includes(`${option.engine_id}:${option.provider_id}`)}
													on:click={() => controlModel(option, 'unload')}
												><Icon name="square" size={12} /></button>
											{/if}
											{#if advanced}
												<div class="provider-actions">
													<button type="button" title="Move provider earlier" aria-label="Move provider earlier" disabled={saving || index === 0} on:click={() => moveProvider(stage, index, -1)}><Icon name="chevron-up" size={14} /></button>
													<button type="button" title="Move provider later" aria-label="Move provider later" disabled={saving || index === providers.length - 1} on:click={() => moveProvider(stage, index, 1)}><Icon name="chevron-down" size={14} /></button>
													<button type="button" title="Remove provider" aria-label="Remove provider" disabled={saving || providers.length === 1} on:click={() => removeProvider(stage, providerId)}><Icon name="x" size={14} /></button>
												</div>
											{/if}
										</div>
									{/each}
								</div>
								{#if advanced && availableAdditions(stage).length > 0}
									<label class="add-provider">
										<span>Add fallback</span>
										<select value="" disabled={saving} on:change={(event) => addProvider(stage, event)}>
											<option value="">Select provider</option>
											{#each availableAdditions(stage) as option (option.option_id)}
												<option value={option.provider_id} disabled={option.availability !== 'available'}>{option.label}</option>
											{/each}
										</select>
									</label>
								{/if}
							{/if}
						</article>
					{/if}
				{/each}
			</div>
		{:else}
			<div class="empty-state">No configured profile for {SURFACE_LABELS[activeSurface]}.</div>
		{/if}
	{:else}
		<div class="empty-state" role="alert">{$audioSettingsStore.error ?? 'Voice and audio settings unavailable'}</div>
	{/if}
</section>

<style>
	.audio-settings {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: grid;
		gap: 1rem;
		padding: 1rem;
	}

	.audio-header,
	.header-actions,
	.profile-row,
	.profile-meta,
	.engine-strip,
	.stage-heading,
	.provider-row,
	.provider-actions {
		align-items: center;
		display: flex;
	}

	.audio-header,
	.profile-row,
	.stage-heading {
		justify-content: space-between;
	}

	.eyebrow,
	h2 {
		letter-spacing: 0;
		margin: 0;
	}

	.eyebrow {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}

	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		margin-top: 0.2rem;
	}

	.header-actions,
	.profile-meta,
	.engine-strip,
	.provider-actions {
		gap: 0.5rem;
	}

	.advanced-button,
	.icon-button,
	.surface-tabs button,
	.provider-actions button {
		background: var(--button-secondary-bg, var(--bg-elevated));
		border: 1px solid var(--border-soft);
		color: var(--text-secondary);
		cursor: pointer;
	}

	.engine-observability {
		display: grid;
		gap: 0.65rem;
	}

	.engine-runtime-row {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		display: grid;
		gap: 0.7rem;
		padding: 0.75rem;
	}

	.engine-runtime-copy strong,
	.engine-runtime-copy span {
		display: block;
	}

	.engine-runtime-copy strong { color: var(--text-primary); font-size: 0.82rem; }
	.engine-runtime-copy span { color: var(--text-secondary); font-size: 0.7rem; margin-top: 0.12rem; }

	.engine-runtime-row dl {
		display: grid;
		gap: 0.5rem;
		grid-template-columns: repeat(6, minmax(0, 1fr));
		margin: 0;
	}

	.engine-runtime-row dl div { min-width: 0; }
	.engine-runtime-row dt { color: var(--text-muted); font-size: 0.66rem; }
	.engine-runtime-row dd { color: var(--text-primary); font-size: 0.76rem; margin: 0.16rem 0 0; overflow: hidden; text-overflow: ellipsis; }
	.engine-runtime-row p { color: var(--color-warning, #9a6410); font-size: 0.72rem; margin: 0; }

	.advanced-button {
		align-items: center;
		border-radius: 6px;
		display: inline-flex;
		font-size: 0.78rem;
		gap: 0.38rem;
		height: 2rem;
		padding: 0 0.65rem;
	}

	.advanced-button.active,
	.surface-tabs button.active {
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-card));
		border-color: color-mix(in srgb, var(--accent-primary) 42%, var(--border-soft));
		color: var(--text-primary);
	}

	.icon-button,
	.provider-actions button {
		align-items: center;
		border-radius: 6px;
		display: inline-flex;
		height: 2rem;
		justify-content: center;
		padding: 0;
		width: 2rem;
	}

	.model-action {
		align-items: center;
		background: var(--button-secondary-bg, var(--bg-elevated));
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		flex: 0 0 1.65rem;
		height: 1.65rem;
		justify-content: center;
		padding: 0;
		width: 1.65rem;
	}

	.surface-tabs {
		display: flex;
		gap: 0.4rem;
		overflow-x: auto;
		padding-bottom: 0.12rem;
	}

	.surface-tabs button {
		border-radius: 6px;
		font-size: 0.8rem;
		font-weight: 620;
		height: 2rem;
		padding: 0 0.75rem;
		white-space: nowrap;
	}

	.profile-row {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		gap: 1rem;
		padding: 0.75rem;
	}

	.profile-row label,
	.add-provider {
		display: grid;
		gap: 0.32rem;
	}

	.profile-row label > span,
	.add-provider > span {
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 650;
	}

	select {
		background: var(--input-bg, var(--bg-elevated, var(--bg-card)));
		border: 1px solid var(--input-border, var(--border-soft));
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		font-size: 0.82rem;
		height: 2.25rem;
		max-width: 26rem;
		padding: 0 1.8rem 0 0.62rem;
	}

	.profile-meta {
		color: var(--text-secondary);
		font-size: 0.72rem;
		text-transform: capitalize;
	}

	.engine-strip {
		flex-wrap: wrap;
	}

	.engine-status,
	.engine-label {
		align-items: center;
		display: inline-flex;
	}

	.engine-status {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		color: var(--text-secondary);
		font-size: 0.7rem;
		gap: 0.5rem;
		padding: 0.2rem 0.28rem 0.2rem 0.55rem;
	}

	.engine-label {
		gap: 0.36rem;
	}

	.engine-strip i {
		background: var(--color-success, #2f8f5b);
		border-radius: 50%;
		height: 0.4rem;
		width: 0.4rem;
	}

	.engine-strip .degraded i { background: var(--color-warning, #b9770e); }
	.engine-strip .unavailable i { background: var(--text-muted); }

	.engine-toggle {
		background: var(--bg-muted);
		border: 0;
		border-radius: 999px;
		cursor: pointer;
		height: 1.15rem;
		padding: 0.13rem;
		width: 2rem;
	}

	.engine-toggle span {
		background: var(--bg-card);
		border-radius: 50%;
		display: block;
		height: 0.89rem;
		transform: translateX(0);
		transition: transform 0.15s ease;
		width: 0.89rem;
	}

	.engine-toggle.enabled { background: var(--accent-primary); }
	.engine-toggle.enabled span { transform: translateX(0.85rem); }

	.macos-speech-help {
		align-items: center;
		background: color-mix(in srgb, var(--color-warning, #b9770e) 9%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--color-warning, #b9770e) 36%, var(--border-soft));
		border-radius: 7px;
		display: flex;
		gap: 0.8rem;
		justify-content: space-between;
		padding: 0.72rem 0.8rem;
	}

	.macos-speech-help strong { color: var(--text-primary); font-size: 0.8rem; }
	.macos-speech-help p { color: var(--text-secondary); font-size: 0.72rem; margin: 0.2rem 0 0; }

	.macos-settings-link {
		background: var(--button-secondary-bg, var(--bg-elevated));
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		flex: 0 0 auto;
		font-size: 0.74rem;
		font-weight: 650;
		padding: 0.48rem 0.65rem;
		text-decoration: none;
	}

	.stage-list,
	.provider-chain {
		display: grid;
		gap: 0.65rem;
	}

	.stage-row {
		border-top: 1px solid var(--border-soft);
		display: grid;
		gap: 0.65rem;
		padding-top: 0.85rem;
	}

	.stage-row.disabled { opacity: 0.72; }

	.stage-heading strong,
	.provider-copy strong {
		color: var(--text-primary);
		display: block;
		font-size: 0.82rem;
	}

	.stage-heading span,
	.provider-copy span {
		color: var(--text-secondary);
		display: block;
		font-size: 0.7rem;
		margin-top: 0.12rem;
	}

	.stage-toggle {
		align-items: center;
		background: var(--bg-muted);
		border: 0;
		border-radius: 999px;
		box-sizing: border-box;
		cursor: pointer;
		display: inline-flex;
		flex: 0 0 auto;
		height: 1.25rem;
		justify-content: flex-start;
		line-height: 0;
		padding: 0 0.15rem;
		width: 2.2rem;
	}

	.stage-toggle span {
		background: var(--bg-card);
		border-radius: 50%;
		display: block;
		flex: 0 0 auto;
		height: 0.95rem;
		transform: translateX(0);
		transition: transform 0.15s ease;
		width: 0.95rem;
	}

	.stage-toggle.enabled { background: var(--accent-primary); }
	.stage-toggle.enabled span { transform: translateX(0.95rem); }

	.provider-row {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		gap: 0.65rem;
		min-width: 0;
		padding: 0.55rem 0.65rem;
	}

	.provider-order {
		color: var(--text-muted);
		font-size: 0.72rem;
		font-variant-numeric: tabular-nums;
		width: 1rem;
	}

	.provider-copy {
		min-width: 0;
	}

	.provider-copy strong,
	.provider-copy span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.model-state {
		border-radius: 999px;
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 650;
		margin-left: auto;
		padding: 0.22rem 0.48rem;
		white-space: nowrap;
	}

	.state-ready { background: color-mix(in srgb, var(--status-success, #2f8f5b) 13%, transparent); color: var(--status-success, #2f8f5b); }
	.state-loading,
	.state-download_required { background: color-mix(in srgb, var(--status-info, #2563eb) 12%, transparent); color: var(--status-info, #2563eb); }
	.state-degraded { background: color-mix(in srgb, var(--status-warning, #b9770e) 14%, transparent); color: var(--status-warning, #9a6208); }
	.state-unavailable { background: color-mix(in srgb, var(--text-muted) 14%, transparent); color: var(--text-secondary); }

	.provider-actions button {
		height: 1.65rem;
		width: 1.65rem;
	}

	button:disabled { cursor: default; opacity: 0.48; }

	.add-provider {
		justify-self: start;
	}

	.audio-skeleton {
		display: grid;
		gap: 0.65rem;
	}

	.audio-skeleton span {
		animation: pulse 1.3s ease-in-out infinite alternate;
		background: var(--bg-soft);
		border-radius: 6px;
		height: 2.8rem;
	}

	.empty-state {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-secondary);
		font-size: 0.82rem;
		padding: 0.85rem;
	}

	@keyframes pulse { to { opacity: 0.5; } }

	@media (max-width: 700px) {
		.macos-speech-help { align-items: flex-start; flex-direction: column; }
	}
</style>
