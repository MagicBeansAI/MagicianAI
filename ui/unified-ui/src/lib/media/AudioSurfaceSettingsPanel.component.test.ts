import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import AudioSurfaceSettingsPanel from './AudioSurfaceSettingsPanel.svelte';
import {
	audioSettingsStore,
	type AudioSettingsResponse,
	type AudioStageOption,
	type AudioSurfaceProfileConfig
} from './audioSettings';
import { mediaProvidersStore } from './providers';

const option: AudioStageOption = {
	option_id: 'fluid-qwen3-asr-f32:FluidInference/qwen3-asr-0.6b-coreml',
	stage: 'recording_stt',
	provider_id: 'fluid-qwen3-asr-f32',
	engine_id: 'fluid_audio',
	model_id: 'FluidInference/qwen3-asr-0.6b-coreml',
	label: 'FluidAudio Qwen',
	capabilities: {
		streaming: false,
		recording: true,
		word_timestamps: false,
		end_of_utterance: false,
		speaker_attribution: false,
		voice_cloning: false
	},
	availability: 'available'
};

const macosSpeechOption: AudioStageOption = {
	...option,
	option_id: 'macos_speech',
	provider_id: 'macos_speech',
	engine_id: 'macos_system',
	model_id: 'macos_speech',
	label: 'macOS Speech',
	availability: 'unavailable'
};

const macosSystemVoiceOption: AudioStageOption = {
	...option,
	option_id: 'macos_tts',
	stage: 'tts',
	provider_id: 'macos_tts',
	engine_id: 'macos_system',
	model_id: 'av_speech_synthesizer',
	label: 'macOS system voice',
	availability: 'unavailable'
};

const profile: AudioSurfaceProfileConfig = {
	surface: 'dictation',
	turn_boundary: 'push_to_talk',
	vad: { enabled: false, required: false, providers: [] },
	recording_stt: { enabled: true, required: true, providers: [option.provider_id] },
	streaming_stt: { enabled: false, required: false, providers: [] },
	diarization: { enabled: false, required: false, providers: [] },
	tts: { enabled: false, required: false, providers: [] }
};

function settings(resident: boolean, enabled = true): AudioSettingsResponse {
	return {
		revision: 'r1',
		default_profiles: { dictation: 'dictation-local' },
		profiles: { 'dictation-local': profile },
		stages: { recording_stt: [option] },
		engines: {
			fluid_audio: {
				engine_id: 'fluid_audio',
				label: 'FluidAudio',
				enabled,
				available: enabled,
				healthy: resident ? true : null,
				can_manage_models: enabled,
				resident_models: resident ? 1 : 0,
				active_sessions: 0,
				max_resident_models: 4,
				max_streaming_sessions: 4,
				model_idle_secs: 300,
				start_count: resident ? 1 : 0,
				restart_count: 0
			}
		},
		models: {
			[option.option_id]: {
				...option,
				state: enabled ? (resident ? 'ready' : 'download_required') : 'unavailable',
				resident,
				active_sessions: 0,
				can_load: enabled && !resident,
				can_unload: enabled && resident
			}
		},
		requires_session_restart: true
	};
}

function macSettings(systemVoiceAvailable: boolean): AudioSettingsResponse {
	const snapshot = settings(false);
	snapshot.stages = {
		...snapshot.stages,
		recording_stt: [macosSpeechOption],
		tts: [
			{
				...macosSystemVoiceOption,
				availability: systemVoiceAvailable ? 'available' : 'unavailable'
			}
		]
	};
	return snapshot;
}

beforeEach(() => {
	audioSettingsStore.reset();
	mediaProvidersStore.reset();
});

afterEach(() => {
	vi.restoreAllMocks();
});

describe('AudioSurfaceSettingsPanel model controls', () => {
	it('disables FluidAudio through the shared revisioned settings API', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/media/audio-settings',
				handle: () => jsonResponse(settings(false))
			},
			{
				method: 'PUT',
				match: '/media/audio-settings',
				handle: () => jsonResponse(settings(false, false))
			},
			{
				method: 'GET',
				match: '/media/providers',
				handle: () => jsonResponse({})
			}
		]);
		const user = userEvent.setup();
		render(AudioSurfaceSettingsPanel);

		const toggle = await screen.findByRole('switch', { name: 'Disable FluidAudio engine' });
		await user.click(toggle);

		await screen.findByRole('switch', { name: 'Enable FluidAudio engine' });
		expect(screen.getByText(/FluidAudio.*Off/)).toBeInTheDocument();
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
			expected_revision: 'r1',
			engines: { fluid_audio: { enabled: false } }
		});
	});

	it('prewarms a backend-advertised local model and adopts returned runtime state', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/media/audio-settings',
				handle: () => jsonResponse(settings(false))
			},
			{
				method: 'POST',
				match: '/media/audio-engines/fluid_audio/models/prewarm',
				handle: () =>
					jsonResponse({
						engine_id: 'fluid_audio',
						action: 'prewarm',
						model_ids: [option.provider_id],
						settings: settings(true)
					})
			}
		]);
		const user = userEvent.setup();
		render(AudioSurfaceSettingsPanel);

		await screen.findByText('FluidAudio Qwen');
		await user.click(screen.getByRole('button', { name: 'Advanced' }));
		await user.click(screen.getByRole('button', { name: 'Load FluidAudio Qwen' }));

		await waitFor(() => expect(screen.getByText('Ready')).toBeInTheDocument());
		expect(screen.getByRole('button', { name: 'Unload FluidAudio Qwen' })).toBeInTheDocument();
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
			model_ids: [option.provider_id]
		});
	});

	it('explains unavailable Mac speech providers and opens the Speech Recognition pane', async () => {
		vi.spyOn(window.navigator, 'platform', 'get').mockReturnValue('MacIntel');
		installFetchMock([
			{
				method: 'GET',
				match: '/media/audio-settings',
				handle: () => jsonResponse(macSettings(false))
			}
		]);

		render(AudioSurfaceSettingsPanel);

		await screen.findByText('Mac speech services need Magican Desktop');
		expect(
			screen.getByText(/macOS System Voice does not need Speech Recognition permission/)
		).toBeInTheDocument();
		expect(screen.getByRole('link', { name: 'Open Speech Recognition Settings' })).toHaveAttribute(
			'href',
			'x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition'
		);
	});

	it('identifies Speech Recognition permission when the Mac system voice is available', async () => {
		vi.spyOn(window.navigator, 'platform', 'get').mockReturnValue('MacIntel');
		installFetchMock([
			{
				method: 'GET',
				match: '/media/audio-settings',
				handle: () => jsonResponse(macSettings(true))
			}
		]);

		render(AudioSurfaceSettingsPanel);

		await screen.findByText(
			'Magican Desktop is available, but macOS Speech still needs permission for its native Speech helper.'
		);
		expect(screen.queryByText(/macOS System Voice does not need/)).not.toBeInTheDocument();
		expect(
			screen.getByRole('link', { name: 'Open Speech Recognition Settings' })
		).toBeInTheDocument();
	});
});
