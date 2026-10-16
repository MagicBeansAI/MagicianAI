import { get } from 'svelte/store';
import { beforeEach, describe, expect, it } from 'vitest';

import { scopeIdentityStore } from './scopeIdentityStore';
import { installFetchMock, jsonResponse } from '../../test/browser';
import {
	notesSettingsStore,
	refreshNotesSettings,
	saveNotesSettings,
	type NotesProviderSettings
} from './notesSettingsStore';

const settings: NotesProviderSettings = {
	enabled: true,
	default_provider: 'silverbullet',
	fallback_provider: 'local_markdown',
	local_markdown: { root: '/notes' },
	silverbullet: {
		space_path: '/notes/space',
		local_url: 'http://127.0.0.1:3021',
		server_url: 'http://127.0.0.1:3021',
		public_origin: 'https://notes.example.com'
	},
	task_publishing: {
		auto_publish_completed: false,
		default_mode: 'standard',
		include_assets: true
	}
};

function envelope() {
	return {
		principal: 'alice',
		workspace: 'work',
		settings_path: '/notes/scopes/alice/work/settings.yaml',
		settings,
		resolved: {
			default_provider: 'silverbullet',
			fallback_provider: 'local_markdown',
			local_markdown_root: '/notes',
			silverbullet_space_path: '/notes/space',
			silverbullet_write_safe: true,
			silverbullet_local_url: 'http://127.0.0.1:3021',
			silverbullet_public_origin: 'https://notes.example.com',
			silverbullet_server_url: 'https://notes.example.com'
		},
		warnings: []
	};
}

function status() {
	return {
		principal: 'alice',
		workspace: 'work',
		enabled: true,
		active_provider: 'silverbullet',
		fallback_provider: 'local_markdown',
		providers: [
			{
				id: 'silverbullet_space',
				label: 'SilverBullet',
				configured: true,
				available: true,
				writable: true,
				root: '/notes/space',
				message: 'ready'
			}
		],
		warnings: []
	};
}

beforeEach(() => {
	scopeIdentityStore.observe('alice', 'work');
});

describe('notesSettingsStore browser workflows', () => {
	it('loads scoped settings and provider health together', async () => {
		const { calls } = installFetchMock([
			{ match: '/notes/settings', handle: () => jsonResponse(envelope()) },
			{ match: '/notes/providers/status', handle: () => jsonResponse(status()) }
		]);

		const result = await refreshNotesSettings();

		expect(result?.settings.default_provider).toBe('silverbullet');
		expect(get(notesSettingsStore).status?.providers[0]).toMatchObject({
			id: 'silverbullet',
			available: true,
			writable: true
		});
		expect(calls.every((call) => !call.url.includes('principal='))).toBe(true);
		expect(calls.every((call) => !call.url.includes('workspace='))).toBe(true);
	});

	it('saves without client scope authority and refreshes authoritative provider state', async () => {
		const { calls } = installFetchMock([
			{
				method: 'PUT',
				match: '/notes/settings',
				handle: () => jsonResponse(envelope())
			},
			{ method: 'GET', match: '/notes/settings', handle: () => jsonResponse(envelope()) },
			{ match: '/notes/providers/status', handle: () => jsonResponse(status()) }
		]);

		await saveNotesSettings(settings);

		const saveCall = calls.find((call) => call.method === 'PUT');
		expect(saveCall).toBeDefined();
		expect(JSON.parse(String(saveCall?.init?.body))).toMatchObject({
			default_provider: 'silverbullet',
			task_publishing: {
				auto_publish_completed: false,
				default_mode: 'standard',
				include_assets: true
			}
		});
		expect(JSON.parse(String(saveCall?.init?.body))).not.toHaveProperty('principal');
		expect(JSON.parse(String(saveCall?.init?.body))).not.toHaveProperty('workspace');
		expect(get(notesSettingsStore)).toMatchObject({
			isSaving: false,
			error: null
		});
	});

	it('defaults historical settings to safe opt-in task publishing', async () => {
		const { task_publishing: _legacyOmission, ...legacySettings } = settings;
		const historical = {
			...envelope(),
			settings: legacySettings as NotesProviderSettings
		};
		installFetchMock([
			{ match: '/notes/settings', handle: () => jsonResponse(historical) },
			{ match: '/notes/providers/status', handle: () => jsonResponse(status()) }
		]);

		const loaded = await refreshNotesSettings();

		expect(loaded?.settings.task_publishing).toEqual({
			auto_publish_completed: false,
			default_mode: 'standard',
			include_assets: true
		});
	});

	it('restores the prior envelope when save fails', async () => {
		installFetchMock([
			{ match: '/notes/settings', handle: () => jsonResponse(envelope()) },
			{ match: '/notes/providers/status', handle: () => jsonResponse(status()) }
		]);
		await refreshNotesSettings();
		const previous = get(notesSettingsStore).envelope;

		installFetchMock([
			{
				method: 'PUT',
				match: '/notes/settings',
				handle: () => jsonResponse({ message: 'settings are read-only' }, { status: 409 })
			}
		]);
		await expect(saveNotesSettings(settings)).rejects.toThrow('settings are read-only');

		expect(get(notesSettingsStore).envelope).toEqual(previous);
		expect(get(notesSettingsStore)).toMatchObject({
			isSaving: false,
			error: 'settings are read-only'
		});
	});

	it('rejects malformed settings without replacing the current envelope', async () => {
		const previous = get(notesSettingsStore).envelope;
		installFetchMock([
			{ match: '/notes/settings', handle: () => jsonResponse({ settings: {} }) },
			{ match: '/notes/providers/status', handle: () => jsonResponse(status()) }
		]);

		await expect(refreshNotesSettings()).rejects.toThrow('Malformed notes settings response');
		expect(get(notesSettingsStore).envelope).toEqual(previous);
		expect(get(notesSettingsStore).isLoading).toBe(false);
	});
});
