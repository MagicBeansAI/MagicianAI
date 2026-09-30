import { describe, expect, it } from 'vitest';

import {
	DEFAULT_MEDIA_PREFERENCES,
	applyMediaPreferencesPatch,
	normalizeMediaPreferences
} from './preferences';

describe('media preference compatibility', () => {
	it('defaults voice mode to recording when the backend response is absent or invalid', () => {
		expect(normalizeMediaPreferences(undefined).voice_mode).toBe('recording');
		expect(normalizeMediaPreferences({ voice_mode: undefined }).voice_mode).toBe('recording');
		expect(normalizeMediaPreferences({ voice_mode: 'invalid' as never }).voice_mode).toBe(
			'recording'
		);
		expect(DEFAULT_MEDIA_PREFERENCES.voice_mode).toBe('recording');
		expect(DEFAULT_MEDIA_PREFERENCES.require_voice_prefix).toBe(true);
		expect(normalizeMediaPreferences(undefined).require_voice_prefix).toBe(true);
	});

	it('preserves explicit realtime and normalizes scoped audio choices', () => {
		const normalized = normalizeMediaPreferences({
			voice_mode: 'realtime',
			surface_profiles: {
				meeting: ' meeting-local '
			},
			surface_stage_options: {
				meeting: {
					streaming_stt: ' local-stt ',
					tts: '   '
				}
			}
		});

		expect(normalized.voice_mode).toBe('realtime');
		expect(normalized.surface_profiles).toEqual({ meeting: 'meeting-local' });
		expect(normalized.surface_stage_options).toEqual({
			meeting: { streaming_stt: 'local-stt' }
		});
	});

	it('preserves the independent hands-free voice mode', () => {
		expect(normalizeMediaPreferences({ voice_mode: 'hands_free' }).voice_mode).toBe(
			'hands_free'
		);
	});

	it('preserves an explicit prefix-control opt out', () => {
		expect(normalizeMediaPreferences({ require_voice_prefix: false }).require_voice_prefix).toBe(
			false
		);
	});

	it('merges a live-call voice without dropping other engine voices', () => {
		const previous = normalizeMediaPreferences({
			realtime_voices: {
				voice_realtime_default: 'marin',
				voice_realtime_gemini_live: 'Puck'
			}
		});
		const next = applyMediaPreferencesPatch(previous, {
			realtime_voices: { voice_realtime_gpt_live_1: 'willow', voice_realtime_gemini_live: '' }
		});
		expect(next.realtime_voices).toEqual({
			voice_realtime_default: 'marin',
			voice_realtime_gpt_live_1: 'willow'
		});
	});

	it('merges one surface patch without dropping choices for other surfaces', () => {
		const previous = normalizeMediaPreferences({
			surface_profiles: { meeting: 'meeting-cloud', listening: 'listening-local' },
			surface_stage_options: {
				meeting: { streaming_stt: 'cloud-stt' },
				listening: { vad: 'local-vad' }
			}
		});

		const next = applyMediaPreferencesPatch(previous, {
			surface_profiles: { meeting: 'meeting-local' },
			surface_stage_options: { meeting: { streaming_stt: '', diarization: 'local-speakers' } }
		});

		expect(next.surface_profiles).toEqual({
			meeting: 'meeting-local',
			listening: 'listening-local'
		});
		expect(next.surface_stage_options).toEqual({
			meeting: { diarization: 'local-speakers' },
			listening: { vad: 'local-vad' }
		});
	});
});
