import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import SurfaceAudioProfileControl from './SurfaceAudioProfileControl.svelte';
import { audioSettingsStore } from './audioSettings';
import { mediaPreferencesStore } from './preferences';
import { mediaProvidersStore } from './providers';

const stageOption = {
	option_id: 'cloud-stt:cloud-model',
	stage: 'streaming_stt',
	provider_id: 'cloud-stt',
	engine_id: 'online',
	model_id: 'cloud-model',
	label: 'Cloud transcription',
	capabilities: {
		streaming: true,
		recording: false,
		word_timestamps: false,
		end_of_utterance: true,
		speaker_attribution: false,
		voice_cloning: false
	},
	availability: 'available'
};

const profiles = {
	'meeting-default': {
		surface: 'meeting',
		turn_boundary: 'stt_eou',
		vad: { enabled: false, required: false, providers: [] },
		recording_stt: { enabled: false, required: false, providers: [] },
		streaming_stt: { enabled: true, required: true, providers: ['cloud-stt'] },
		diarization: { enabled: false, required: false, providers: [] },
		tts: { enabled: false, required: false, providers: [] }
	},
	'meeting-local': {
		surface: 'meeting',
		turn_boundary: 'stt_eou',
		vad: { enabled: false, required: false, providers: [] },
		recording_stt: { enabled: false, required: false, providers: [] },
		streaming_stt: { enabled: true, required: true, providers: ['cloud-stt'] },
		diarization: { enabled: false, required: false, providers: [] },
		tts: { enabled: false, required: false, providers: [] }
	}
};

beforeEach(() => {
	audioSettingsStore.reset();
	mediaPreferencesStore.reset();
	mediaProvidersStore.reset();
});

describe('SurfaceAudioProfileControl', () => {
	it('renders only backend-advertised choices and persists a scoped profile override', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse({ agents: [] })
			},
			{
				method: 'GET',
				match: '/media/providers',
				handle: () =>
					jsonResponse({
						tts: null,
						stt: null,
						realtime_voice: null,
						stages: { streaming_stt: [stageOption] },
						surface_profiles: profiles,
						default_surface_profiles: { meeting: 'meeting-default' },
						engines: {},
						resolved: true
					})
			},
			{
				method: 'GET',
				match: '/media/preferences?',
					handle: () =>
					jsonResponse({
						schema_version: 3,
						auto_speak: false,
						voice_mode: 'recording',
						require_voice_prefix: true,
						surface_profiles: {},
						surface_stage_options: {}
					})
			},
			{
				method: 'GET',
				match: '/media/audio-settings',
				handle: () =>
					jsonResponse({
						revision: 'r1',
						default_profiles: { meeting: 'meeting-default' },
						profiles,
						stages: { streaming_stt: [stageOption] },
						engines: {},
						models: {
							[stageOption.option_id]: {
								...stageOption,
								state: 'ready',
								resident: true
							}
						},
						requires_session_restart: true
					})
			},
			{
				method: 'GET',
				match: '/media/surfaces/meeting/resolved?',
				handle: () =>
					jsonResponse({
						surface: 'meeting',
						profile_id: 'meeting-default',
						revision: 'r1',
						source: 'configured_default',
						turn_boundary: 'stt_eou',
						stages: {},
						degradations: []
					})
			},
			{
				method: 'PUT',
				match: '/media/preferences',
					handle: ({ init }) => {
						const body = JSON.parse(String(init?.body));
						return jsonResponse({
							schema_version: 3,
							auto_speak: false,
							voice_mode: 'recording',
							require_voice_prefix: true,
						surface_profiles: { meeting: body.surface_profiles.meeting },
						surface_stage_options: {}
					});
				}
			}
		]);
		scopeIdentityStore.observe('alice', 'work');
		const user = userEvent.setup();
		render(SurfaceAudioProfileControl, {
			surface: 'meeting',
			compact: true,
			showStages: true
		});

		const profile = await screen.findByRole('combobox', { name: 'Meeting audio profile' });
		await waitFor(() => expect(profile).toHaveTextContent('Meeting Default'));
		expect(profile).toHaveTextContent('Meeting Local');
		expect(screen.queryByText('Browser')).not.toBeInTheDocument();
		await user.selectOptions(profile, 'meeting-local');

			await waitFor(() => expect(calls.some((call) => call.method === 'PUT')).toBe(true));
			const save = calls.find((call) => call.method === 'PUT');
			expect(new Headers(save?.init?.headers).get('X-Principal')).toBeNull();
			const body = JSON.parse(String(save?.init?.body));
			expect(body).toMatchObject({ surface_profiles: { meeting: 'meeting-local' } });
			expect(body).not.toHaveProperty('workspace');
		});
	});
