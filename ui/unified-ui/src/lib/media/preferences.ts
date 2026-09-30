import { browser } from '$app/environment';
import { writable } from 'svelte/store';

import { getCurrentScopeIdentity, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { ttsStore } from './tts/store';
import { hydrateVoiceModePreference, type VoiceMode } from './voice/wakeWord';
import type { AudioStage, AudioSurface } from './audioSettings';

export interface MediaPreferences {
	schema_version: number;
	auto_speak: boolean;
	voice_mode: VoiceMode;
	require_voice_prefix: boolean;
	surface_profiles: Partial<Record<AudioSurface, string>>;
	surface_stage_options: Partial<Record<AudioSurface, Partial<Record<AudioStage, string>>>>;
	realtime_voices: Record<string, string>;
}

export interface MediaPreferencesState {
	preferences: MediaPreferences;
	resolved: boolean;
	error: string | null;
}

export const DEFAULT_MEDIA_PREFERENCES: MediaPreferences = {
	schema_version: 3,
	auto_speak: false,
	voice_mode: 'recording',
	require_voice_prefix: true,
	surface_profiles: {},
	surface_stage_options: {},
	realtime_voices: {}
};

const DEFAULT_STATE: MediaPreferencesState = {
	preferences: { ...DEFAULT_MEDIA_PREFERENCES },
	resolved: false,
	error: null
};

function normalizeVoiceMode(value: unknown): VoiceMode {
	return value === 'hands_free' ? 'hands_free' : value === 'realtime' ? 'realtime' : 'recording';
}

function normalizeStringRecord(value: unknown): Record<string, string> {
	if (!value || typeof value !== 'object' || Array.isArray(value)) return {};
	return Object.fromEntries(
		Object.entries(value as Record<string, unknown>)
			.filter((entry): entry is [string, string] =>
				typeof entry[1] === 'string' && entry[1].trim().length > 0
			)
			.map(([key, entry]) => [key, entry.trim()])
	);
}

function normalizeSurfaceStageOptions(
	value: unknown
): Partial<Record<AudioSurface, Partial<Record<AudioStage, string>>>> {
	if (!value || typeof value !== 'object' || Array.isArray(value)) return {};
	const normalized: Record<string, Record<string, string>> = {};
	for (const [surface, options] of Object.entries(value as Record<string, unknown>)) {
		const stages = normalizeStringRecord(options);
		if (Object.keys(stages).length > 0) normalized[surface] = stages;
	}
	return normalized as Partial<Record<AudioSurface, Partial<Record<AudioStage, string>>>>;
}

export function normalizeMediaPreferences(
	data: Partial<MediaPreferences> | null | undefined
): MediaPreferences {
	return {
		schema_version:
			typeof data?.schema_version === 'number' && data.schema_version > 0
				? data.schema_version
				: DEFAULT_MEDIA_PREFERENCES.schema_version,
		auto_speak:
			typeof data?.auto_speak === 'boolean'
				? data.auto_speak
				: DEFAULT_MEDIA_PREFERENCES.auto_speak,
		voice_mode: normalizeVoiceMode(data?.voice_mode),
		require_voice_prefix:
			typeof data?.require_voice_prefix === 'boolean'
				? data.require_voice_prefix
				: DEFAULT_MEDIA_PREFERENCES.require_voice_prefix,
		surface_profiles: normalizeStringRecord(data?.surface_profiles) as Partial<
			Record<AudioSurface, string>
		>,
		surface_stage_options: normalizeSurfaceStageOptions(data?.surface_stage_options),
		realtime_voices: normalizeStringRecord(data?.realtime_voices)
	};
}

export function applyMediaPreferencesPatch(
	previous: MediaPreferences,
	patch: Partial<MediaPreferences>
): MediaPreferences {
	const surfaceProfiles = { ...previous.surface_profiles };
	for (const [surface, profileId] of Object.entries(patch.surface_profiles ?? {})) {
		if (!profileId || ['auto', 'default'].includes(profileId.trim().toLowerCase())) {
			delete surfaceProfiles[surface as AudioSurface];
		} else {
			surfaceProfiles[surface as AudioSurface] = profileId.trim();
		}
	}

	const surfaceStageOptions = Object.fromEntries(
		Object.entries(previous.surface_stage_options).map(([surface, stages]) => [
			surface,
			{ ...stages }
		])
	) as Partial<Record<AudioSurface, Partial<Record<AudioStage, string>>>>;
	for (const [surface, stagePatch] of Object.entries(patch.surface_stage_options ?? {})) {
		const key = surface as AudioSurface;
		const nextStages = { ...(surfaceStageOptions[key] ?? {}) };
		for (const [stage, optionId] of Object.entries(stagePatch ?? {})) {
			if (!optionId || ['auto', 'default'].includes(optionId.trim().toLowerCase())) {
				delete nextStages[stage as AudioStage];
			} else {
				nextStages[stage as AudioStage] = optionId.trim();
			}
		}
		if (Object.keys(nextStages).length > 0) surfaceStageOptions[key] = nextStages;
		else delete surfaceStageOptions[key];
	}

	const realtimeVoices = { ...previous.realtime_voices };
	for (const [profileId, voice] of Object.entries(patch.realtime_voices ?? {})) {
		if (!voice || ['auto', 'default'].includes(voice.trim().toLowerCase())) {
			delete realtimeVoices[profileId];
		} else {
			realtimeVoices[profileId] = voice.trim();
		}
	}

	return normalizeMediaPreferences({
		...previous,
		...patch,
		surface_profiles: surfaceProfiles,
		surface_stage_options: surfaceStageOptions,
		realtime_voices: realtimeVoices
	});
}

function hydrateRuntimeStores(preferences: MediaPreferences): void {
	ttsStore.hydratePrefs({
		autoSpeak: preferences.auto_speak
	});
	hydrateVoiceModePreference(preferences.voice_mode);
}

function createStore() {
	const { subscribe, set } = writable<MediaPreferencesState>(DEFAULT_STATE);
	let currentState: MediaPreferencesState = DEFAULT_STATE;
	let inflight: Promise<MediaPreferences> | null = null;
	// Bumped when a save starts. A load that began earlier must not apply its
	// response afterwards, or a slow GET puts the previous auto-speak and
	// surface choices back on screen.
	let preferenceEpoch = 0;

	function apply(preferences: MediaPreferences, error: string | null = null): MediaPreferences {
		hydrateRuntimeStores(preferences);
		currentState = { preferences, resolved: true, error };
		set(currentState);
		return preferences;
	}

	async function refresh(): Promise<MediaPreferences> {
		if (!browser) return DEFAULT_MEDIA_PREFERENCES;
		if (inflight) return inflight;
		const seenEpoch = preferenceEpoch;
		inflight = (async () => {
			try {
				const response = await fetch('/api/magician/v2/media/preferences');
				if (!response.ok) throw new Error(`media preferences fetch ${response.status}`);
				const preferences = normalizeMediaPreferences(
					(await response.json()) as Partial<MediaPreferences>
				);
				if (seenEpoch !== preferenceEpoch) return currentState.preferences;
				return apply(preferences);
			} catch (error) {
				if (seenEpoch !== preferenceEpoch) return currentState.preferences;
				const preferences = { ...DEFAULT_MEDIA_PREFERENCES };
				return apply(
					preferences,
					error instanceof Error ? error.message : 'failed to load media preferences'
				);
			} finally {
				inflight = null;
			}
		})();
		return inflight;
	}

	async function save(patch: Partial<MediaPreferences>): Promise<MediaPreferences> {
		if (!browser) return DEFAULT_MEDIA_PREFERENCES;
		const epoch = ++preferenceEpoch;
		const previous = currentState.preferences;
		const optimistic = applyMediaPreferencesPatch(previous, patch);
		apply(optimistic);
		try {
			const response = await fetch('/api/magician/v2/media/preferences', {
				method: 'PUT',
				headers: scopedRequestHeaders({ 'content-type': 'application/json' }),
				body: JSON.stringify(patch)
			});
			if (!response.ok) throw new Error(`media preferences save ${response.status}`);
			const saved = normalizeMediaPreferences(
				(await response.json()) as Partial<MediaPreferences>
			);
			if (epoch !== preferenceEpoch) return currentState.preferences;
			// A load that started while this save was in flight captured the
			// same epoch. Bump again so its later response cannot replace the
			// saved preferences.
			preferenceEpoch += 1;
			return apply(saved);
		} catch (error) {
			if (epoch === preferenceEpoch) {
				apply(
					previous,
					error instanceof Error ? error.message : 'failed to save media preferences'
				);
			}
			throw error;
		}
	}

	return {
		subscribe,
		refresh,
		save,
		apply,
		reset(): void {
			currentState = DEFAULT_STATE;
			set(currentState);
			hydrateRuntimeStores(DEFAULT_MEDIA_PREFERENCES);
		}
	};
}

export const mediaPreferencesStore = createStore();
export const refreshMediaPreferences = mediaPreferencesStore.refresh;
export const saveMediaPreferences = mediaPreferencesStore.save;

export function handleMediaPreferencesUpdatedEnvelope(envelope: {
	event_type: string;
	principal?: string | null;
	workspace?: string | null;
	payload: unknown;
}): void {
	if (envelope.event_type !== 'media.preferences.updated') return;
	const scope = getCurrentScopeIdentity();
	if (
		envelope.principal
		&& envelope.workspace
		&& (envelope.principal !== scope.principal || envelope.workspace !== scope.workspace)
	) {
		return;
	}
	const payload = envelope.payload && typeof envelope.payload === 'object'
		? envelope.payload as Record<string, unknown>
		: null;
	const preferences = payload?.preferences && typeof payload.preferences === 'object'
		? payload.preferences as Partial<MediaPreferences>
		: null;
	if (!preferences) return;
	mediaPreferencesStore.apply(normalizeMediaPreferences(preferences));
}
