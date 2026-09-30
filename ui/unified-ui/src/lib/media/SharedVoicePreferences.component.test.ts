import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { get } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { mediaPreferencesStore, saveMediaPreferences } from './preferences';
import SharedVoicePreferences from './SharedVoicePreferences.svelte';

vi.mock('$lib/shared/stores/notifications', () => ({
	showError: vi.fn(),
	showSuccess: vi.fn()
}));

const PREFERENCES = {
	schema_version: 3,
	auto_speak: false,
	voice_mode: 'recording',
	require_voice_prefix: true,
	surface_profiles: {},
	surface_stage_options: {},
	realtime_voices: {}
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('SharedVoicePreferences', () => {
	it('saves auto-speak and lists every shared surface', async () => {
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/media/providers', handle: () => jsonResponse({ surface_profiles: {}, default_surface_profiles: {}, stages: {}, resolved: true }) },
			{ method: 'GET', match: '/media/preferences', handle: () => jsonResponse(PREFERENCES) },
			{ method: 'GET', match: '/media/audio-settings', handle: () => jsonResponse({ revision: 'r1', default_profiles: {}, profiles: {}, stages: {}, engines: {}, models: {} }) },
			{
				method: 'GET',
				match: '/media/surfaces/',
				handle: () =>
					jsonResponse({
						profile_id: 'default',
						revision: 'r1',
						source: 'configured_default',
						turn_boundary: 'push_to_talk',
						stages: {},
						degradations: []
					})
			},
			{
				method: 'PUT',
				match: '/media/preferences',
				handle: () => jsonResponse({ ...PREFERENCES, auto_speak: true })
			}
		]);
		const user = userEvent.setup();
		render(SharedVoicePreferences);

		expect(await screen.findByRole('heading', { name: 'Voice preferences' })).toBeTruthy();
		expect(screen.getByRole('combobox', { name: 'Dictation audio profile' })).toBeTruthy();
		expect(screen.getByRole('combobox', { name: 'Meeting audio profile' })).toBeTruthy();
		expect(screen.getByRole('combobox', { name: 'Listening audio profile' })).toBeTruthy();
		expect(screen.getByRole('combobox', { name: 'Hands-free audio profile' })).toBeTruthy();

		await user.click(screen.getByRole('switch', { name: 'Auto-speak assistant replies' }));
		await waitFor(() => expect(calls.some((call) => call.method === 'PUT')).toBe(true));
		const save = calls.find((call) => call.method === 'PUT');
		expect(JSON.parse(String(save?.init?.body))).toMatchObject({ auto_speak: true });
	});

	it('keeps a newer auto-speak save when an older preference load finishes later', async () => {
		mediaPreferencesStore.reset();
		let release = () => {};
		const gate = new Promise<void>((resolve) => {
			release = resolve;
		});
		installFetchMock([
			{
				method: 'GET',
				match: '/media/preferences',
				handle: async () => {
					await gate;
					return jsonResponse({ ...PREFERENCES, auto_speak: false });
				}
			},
			{
				method: 'PUT',
				match: '/media/preferences',
				handle: () => jsonResponse({ ...PREFERENCES, auto_speak: true })
			}
		]);

		const pendingRefresh = mediaPreferencesStore.refresh();
		await saveMediaPreferences({ auto_speak: true });
		release();
		await pendingRefresh;

		expect(get(mediaPreferencesStore).preferences.auto_speak).toBe(true);
		mediaPreferencesStore.reset();
	});
});
