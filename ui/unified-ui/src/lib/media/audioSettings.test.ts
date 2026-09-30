import { beforeEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import {
	controlAudioEngineModels,
	fetchAudioSettings,
	fetchResolvedAudioSurface,
	updateAudioSettings
} from './audioSettings';

beforeEach(() => {
	scopeIdentityStore.observe('alice', 'work');
});

describe('audio settings API', () => {
	it('reads and revision-patches the backend-owned catalog', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/media/audio-settings',
				handle: () =>
					jsonResponse({
						revision: 'r1',
						default_profiles: {},
						profiles: {},
						stages: {},
						engines: {},
						models: {},
						requires_session_restart: true
					})
			},
			{
				method: 'PUT',
				match: '/media/audio-settings',
				handle: () =>
					jsonResponse({
						revision: 'r2',
						default_profiles: { meeting: 'meeting-local' },
						profiles: {},
						stages: {},
						engines: {},
						models: {},
						requires_session_restart: true
					})
			}
		]);

		expect((await fetchAudioSettings()).revision).toBe('r1');
		const updated = await updateAudioSettings({
			expected_revision: 'r1',
			default_profiles: { meeting: 'meeting-local' }
		});
		expect(updated.revision).toBe('r2');
		expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({
			expected_revision: 'r1',
			default_profiles: { meeting: 'meeting-local' }
		});
	});

	it('sends profile and stage option hints without client scope selectors or an allowlist', async () => {
		const { calls } = installFetchMock([
			{
				match: '/media/surfaces/meeting/resolved?',
				handle: () =>
					jsonResponse({
						surface: 'meeting',
						profile_id: 'meeting-local',
						revision: 'r1',
						source: 'explicit_request',
						turn_boundary: 'stt_eou',
						stages: {},
						degradations: []
					})
			}
		]);

		await fetchResolvedAudioSurface('meeting', {
			profile: 'meeting-local',
			stageOptions: {
				streaming_stt: 'provider/from-server',
				diarization: 'off'
			}
		});

		const url = new URL(calls[0]?.url ?? '', 'http://localhost');
		expect(url.searchParams.has('principal')).toBe(false);
		expect(url.searchParams.has('workspace')).toBe(false);
		expect(url.searchParams.get('profile')).toBe('meeting-local');
		expect(url.searchParams.getAll('stage_option')).toEqual([
			'streaming_stt:provider/from-server',
			'diarization:off'
		]);
	});

	it('uses the generic engine control contract for explicit model lifecycle actions', async () => {
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: '/media/audio-engines/fluid_audio/models/prewarm',
				handle: () =>
					jsonResponse({
						engine_id: 'fluid_audio',
						action: 'prewarm',
						model_ids: ['fluid-silero-v6'],
						settings: { revision: 'r1', profiles: {}, stages: {}, engines: {}, models: {} }
					})
			}
		]);

		const result = await controlAudioEngineModels('fluid_audio', 'prewarm', [
			'fluid-silero-v6'
		]);
		expect(result.model_ids).toEqual(['fluid-silero-v6']);
		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
			model_ids: ['fluid-silero-v6']
		});
	});
});
