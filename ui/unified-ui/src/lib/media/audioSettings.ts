import { browser } from '$app/environment';
import { writable } from 'svelte/store';

import { appendCurrentScopeQuery } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';

export type AudioSurface = 'dictation' | 'meeting' | 'listening' | 'hands_free';
export type AudioStage = 'vad' | 'recording_stt' | 'streaming_stt' | 'diarization' | 'tts';
export type TurnBoundaryAuthority = 'push_to_talk' | 'vad' | 'stt_eou' | 'provider_server';
export type VadMode = 'gate_only' | 'turn_authority';
export type ProviderAvailability = 'available' | 'unavailable' | 'disabled';
export type AudioModelLifecycleState =
	| 'download_required'
	| 'loading'
	| 'ready'
	| 'degraded'
	| 'unavailable';

export interface AudioStageCapabilities {
	streaming: boolean;
	recording: boolean;
	word_timestamps: boolean;
	end_of_utterance: boolean;
	speaker_attribution: boolean;
	voice_cloning: boolean;
	voices?: string[];
	formats?: string[];
	languages?: string[];
	features?: string[];
}

export interface AudioStageOption {
	option_id: string;
	stage: AudioStage;
	provider_id: string;
	engine_id: string;
	model_id: string;
	variant?: string | null;
	label: string;
	capabilities: AudioStageCapabilities;
	availability: ProviderAvailability;
	unavailable_reason?: string | null;
}

export interface AudioStageProfileConfig {
	enabled: boolean;
	required: boolean;
	mode?: VadMode | null;
	providers?: string[];
	threshold?: number | null;
	min_speech_ms?: number | null;
	min_silence_ms?: number | null;
	pre_roll_ms?: number | null;
	hangover_ms?: number | null;
	max_utterance_ms?: number | null;
}

export interface AudioSurfaceProfileConfig {
	surface: AudioSurface;
	turn_boundary: TurnBoundaryAuthority;
	vad: AudioStageProfileConfig;
	recording_stt: AudioStageProfileConfig;
	streaming_stt: AudioStageProfileConfig;
	diarization: AudioStageProfileConfig;
	tts: AudioStageProfileConfig;
}

export interface AudioEngineStatus {
	engine_id: string;
	label: string;
	enabled: boolean;
	available: boolean;
	healthy?: boolean | null;
	owned?: boolean | null;
	endpoint?: string | null;
	startup_policy?: 'disabled' | 'external' | 'lazy' | null;
	download_policy?: 'disabled' | 'on_demand' | 'prewarm' | null;
	offline?: boolean | null;
	process_idle_secs?: number | null;
	model_idle_secs?: number | null;
	max_resident_models?: number | null;
	max_streaming_sessions?: number | null;
	resident_models?: number | null;
	active_sessions?: number | null;
	process_id?: number | null;
	start_count?: number | null;
	restart_count?: number | null;
	last_started_at_ms?: number | null;
	last_error?: string | null;
	can_manage_models: boolean;
}

export interface AudioModelRuntimeStatus {
	option_id: string;
	stage: AudioStage;
	provider_id: string;
	engine_id: string;
	model_id: string;
	state: AudioModelLifecycleState;
	resident: boolean;
	active_sessions: number;
	can_load: boolean;
	can_unload: boolean;
	message?: string | null;
}

export type AudioEngineModelAction = 'prewarm' | 'unload';

export interface AudioEngineModelControlResponse {
	engine_id: string;
	action: AudioEngineModelAction;
	model_ids: string[];
	settings: AudioSettingsResponse;
}

export interface AudioSettingsResponse {
	revision: string;
	default_profiles: Partial<Record<AudioSurface, string>>;
	profiles: Record<string, AudioSurfaceProfileConfig>;
	stages: Partial<Record<AudioStage, AudioStageOption[]>>;
	engines: Record<string, AudioEngineStatus>;
	models: Record<string, AudioModelRuntimeStatus>;
	requires_session_restart: boolean;
}

export interface AudioStageSettingsPatch {
	enabled?: boolean;
	providers?: string[];
}

export interface AudioProfileSettingsPatch {
	turn_boundary?: TurnBoundaryAuthority;
	stages?: Partial<Record<AudioStage, AudioStageSettingsPatch>>;
}

export interface AudioEngineSettingsPatch {
	enabled?: boolean;
}

export interface AudioSettingsPatch {
	expected_revision: string;
	engines?: Record<string, AudioEngineSettingsPatch>;
	default_profiles?: Partial<Record<AudioSurface, string>>;
	profiles?: Record<string, AudioProfileSettingsPatch>;
}

export interface ResolvedAudioStage {
	stage: AudioStage;
	enabled: boolean;
	required: boolean;
	mode?: VadMode | null;
	providers: AudioStageOption[];
	selected?: AudioStageOption | null;
	degraded_reason?: string | null;
}

export interface ResolvedAudioProfile {
	surface: AudioSurface;
	profile_id: string;
	revision: string;
	source: 'explicit_request' | 'scoped_preference' | 'configured_default' | 'compiled_compatibility';
	turn_boundary: TurnBoundaryAuthority;
	stages: Partial<Record<AudioStage, ResolvedAudioStage>>;
	degradations: string[];
}

async function readJson<T>(response: Response): Promise<T> {
	if (response.ok) return (await response.json()) as T;
	const body = await response.text().catch(() => '');
	throw new Error(`audio settings request ${response.status}: ${body}`);
}

export async function fetchAudioSettings(): Promise<AudioSettingsResponse> {
	return readJson<AudioSettingsResponse>(
		await timedFetch('/api/magician/v2/media/audio-settings')
	);
}

export async function updateAudioSettings(
	patch: AudioSettingsPatch
): Promise<AudioSettingsResponse> {
	return readJson<AudioSettingsResponse>(
		await timedFetch('/api/magician/v2/media/audio-settings', {
			method: 'PUT',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify(patch)
		})
	);
}

export async function controlAudioEngineModels(
	engineId: string,
	action: AudioEngineModelAction,
	modelIds: string[] = []
): Promise<AudioEngineModelControlResponse> {
	return readJson<AudioEngineModelControlResponse>(
		await timedFetch(
			`/api/magician/v2/media/audio-engines/${encodeURIComponent(engineId)}/models/${action}`,
			{
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ model_ids: modelIds })
			}
		)
	);
}

export interface AudioSettingsState {
	settings: AudioSettingsResponse | null;
	loading: boolean;
	error: string | null;
}

function createAudioSettingsStore() {
	const { subscribe, set } = writable<AudioSettingsState>({
		settings: null,
		loading: false,
		error: null
	});
	let current: AudioSettingsState = { settings: null, loading: false, error: null };
	let inflight: Promise<AudioSettingsResponse> | null = null;

	function apply(next: AudioSettingsState): void {
		current = next;
		set(next);
	}

	async function refresh(): Promise<AudioSettingsResponse> {
		if (!browser) throw new Error('audio settings are only available in the browser');
		if (inflight) return inflight;
		apply({ ...current, loading: true, error: null });
		inflight = fetchAudioSettings()
			.then((settings) => {
				apply({ settings, loading: false, error: null });
				return settings;
			})
			.catch((error) => {
				const message = error instanceof Error ? error.message : 'failed to load audio settings';
				apply({ ...current, loading: false, error: message });
				throw error;
			})
			.finally(() => {
				inflight = null;
			});
		return inflight;
	}

	async function save(patch: AudioSettingsPatch): Promise<AudioSettingsResponse> {
		apply({ ...current, loading: true, error: null });
		try {
			const settings = await updateAudioSettings(patch);
			apply({ settings, loading: false, error: null });
			return settings;
		} catch (error) {
			const message = error instanceof Error ? error.message : 'failed to update audio settings';
			apply({ ...current, loading: false, error: message });
			throw error;
		}
	}

	return {
		subscribe,
		refresh,
		save,
		adopt(settings: AudioSettingsResponse): void {
			apply({ settings, loading: false, error: null });
		},
		reset(): void {
			apply({ settings: null, loading: false, error: null });
		}
	};
}

export const audioSettingsStore = createAudioSettingsStore();

export function handleMediaConfigUpdatedEnvelope(envelope: { event_type: string }): void {
	if (envelope.event_type !== 'media.config.updated') return;
	void audioSettingsStore.refresh().catch(() => null);
}

export async function fetchResolvedAudioSurface(
	surface: AudioSurface,
	options: { profile?: string; stageOptions?: Partial<Record<AudioStage, string>> } = {}
): Promise<ResolvedAudioProfile> {
	const query = appendCurrentScopeQuery();
	if (options.profile) query.set('profile', options.profile);
	for (const [stage, optionId] of Object.entries(options.stageOptions ?? {})) {
		if (optionId) query.append('stage_option', `${stage}:${optionId}`);
	}
	return readJson<ResolvedAudioProfile>(
		await timedFetch(
			`/api/magician/v2/media/surfaces/${encodeURIComponent(surface)}/resolved?${query.toString()}`
		)
	);
}
