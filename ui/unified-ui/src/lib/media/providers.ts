/**
 * Frontend snapshot of the backend media provider registry.
 *
 * Calls `GET /api/magician/v2/media/providers` once on session start
 * and caches the result so consumers (SpeakButton, RealtimeVoiceClient,
 * SttClient) can decide whether to use the backend provider path or
 * fall back to the browser-native path without re-querying.
 */

import { browser } from '$app/environment';
import { writable } from 'svelte/store';
import type {
	AudioEngineStatus,
	AudioStage,
	AudioStageOption,
	AudioSurface,
	AudioSurfaceProfileConfig
} from './audioSettings';

export interface ProviderInfo {
	id: string;
	label?: string;
	model: string;
	voice?: string;
	format?: string;
	voices?: string[];
	formats?: string[];
	streaming?: boolean;
}

export interface RealtimeVoiceChoiceInfo {
	id: string;
	label: string;
}

export interface RealtimeVoiceProfileInfo {
	profile_id: string;
	label: string;
	provider: string;
	model: string;
	topology: 'direct_peer_to_peer' | 'backend_proxied';
	mode: 'assistant' | 'translation';
	turn_detection_mode?: string;
	transcription_model?: string;
	transcription_fallback_model?: string;
	available: boolean;
	unavailable_reason?: string;
	translation_target_language?: string;
	voice?: string | null;
	voices?: RealtimeVoiceChoiceInfo[];
}

export interface ProviderSnapshot {
	tts: ProviderInfo | null;
	tts_fallbacks?: ProviderInfo[] | null;
	stt: ProviderInfo | null;
	stt_fallbacks?: ProviderInfo[] | null;
	realtime_voice: ProviderInfo | null;
	realtime_voice_profiles?: RealtimeVoiceProfileInfo[];
	realtime_voice_default_profile?: string | null;
	hands_free_voice?: boolean;
	audio_revision?: string | null;
	stages: Partial<Record<AudioStage, AudioStageOption[]>>;
	surface_profiles: Record<string, AudioSurfaceProfileConfig>;
	default_surface_profiles: Partial<Record<AudioSurface, string>>;
	engines: Record<string, AudioEngineStatus>;
	resolved: boolean;
}

const DEFAULT_SNAPSHOT: ProviderSnapshot = {
	tts: null,
	tts_fallbacks: null,
	stt: null,
	stt_fallbacks: null,
	realtime_voice: null,
	realtime_voice_profiles: [],
	realtime_voice_default_profile: null,
	hands_free_voice: false,
	audio_revision: null,
	stages: {},
	surface_profiles: {},
	default_surface_profiles: {},
	engines: {},
	resolved: false
};

function createStore() {
	const { subscribe, set } = writable<ProviderSnapshot>(DEFAULT_SNAPSHOT);
	let inflight: Promise<ProviderSnapshot> | null = null;

	async function refresh(): Promise<ProviderSnapshot> {
		if (!browser) return DEFAULT_SNAPSHOT;
		if (inflight) return inflight;
		inflight = (async () => {
			try {
				const response = await fetch('/api/magician/v2/media/providers');
				if (!response.ok) throw new Error(`providers fetch ${response.status}`);
				const data = (await response.json()) as Partial<ProviderSnapshot>;
				const next: ProviderSnapshot = {
					tts: data.tts ?? null,
					tts_fallbacks: data.tts_fallbacks ?? null,
					stt: data.stt ?? null,
					stt_fallbacks: data.stt_fallbacks ?? null,
					realtime_voice: data.realtime_voice ?? null,
					realtime_voice_profiles: data.realtime_voice_profiles ?? [],
					realtime_voice_default_profile: data.realtime_voice_default_profile ?? null,
					hands_free_voice: data.hands_free_voice === true,
					audio_revision: data.audio_revision ?? null,
					stages: data.stages ?? {},
					surface_profiles: data.surface_profiles ?? {},
					default_surface_profiles: data.default_surface_profiles ?? {},
					engines: data.engines ?? {},
					resolved: true
				};
				set(next);
				return next;
			} catch {
				const next = { ...DEFAULT_SNAPSHOT, resolved: true };
				set(next);
				return next;
			} finally {
				inflight = null;
			}
		})();
		return inflight;
	}

	return {
		subscribe,
		refresh,
		reset(): void {
			set(DEFAULT_SNAPSHOT);
		}
	};
}

export const mediaProvidersStore = createStore();

const REALTIME_PROFILE_STORAGE_KEY = 'magician.realtime_voice_profile';

function createRealtimeProfileStore() {
	const initial = browser ? window.localStorage.getItem(REALTIME_PROFILE_STORAGE_KEY) : null;
	const { subscribe, set } = writable<string | null>(initial || null);
	return {
		subscribe,
		set(value: string | null): void {
			const normalized = value?.trim() || null;
			if (browser) {
				if (normalized) window.localStorage.setItem(REALTIME_PROFILE_STORAGE_KEY, normalized);
				else window.localStorage.removeItem(REALTIME_PROFILE_STORAGE_KEY);
			}
			set(normalized);
		}
	};
}

/** Local surface preference. `null` means use the config-mapped GPT default. */
export const realtimeVoiceProfileStore = createRealtimeProfileStore();
