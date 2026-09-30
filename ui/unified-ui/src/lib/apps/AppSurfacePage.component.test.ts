import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/environment', () => ({ browser: true, dev: true, building: false }));
vi.mock('$lib/realtime/v2-websocket', () => ({
	getV2EventSequence: () => 0,
	v2Events: {
		subscribe: () => () => {},
		connectionStatus: { subscribe: () => () => {} },
		getConnectionState: () => 'OPEN',
		connectGlobal: vi.fn()
	}
}));
vi.mock('./appSurfaceRuntime', async (original) => ({
	...await original<typeof import('./appSurfaceRuntime')>(),
	fetchAppSurface: vi.fn()
}));
vi.mock('./appScriptedSurface', async (original) => ({
	...await original<typeof import('./appScriptedSurface')>(),
	fetchScriptedSurfaceHost: vi.fn()
}));
vi.mock('./appCustomSurface', async (original) => ({
	...await original<typeof import('./appCustomSurface')>(),
	fetchCustomSurfaceHost: vi.fn()
}));
vi.mock('./appDirectory', () => ({ fetchAppDirectory: vi.fn() }));
vi.mock('./appWidgets', async (original) => ({
	...await original<typeof import('./appWidgets')>(),
	appSurfaceSlotPage: () => null
}));

import AppSurfacePage from './AppSurfacePage.svelte';
import { AppSurfaceClientError, fetchAppSurface, type AppSurfaceHydration } from './appSurfaceRuntime';
import { fetchScriptedSurfaceHost } from './appScriptedSurface';
import { fetchCustomSurfaceHost } from './appCustomSurface';
import { fetchAppDirectory, type AppDirectoryEntry } from './appDirectory';

function directoryEntry(): AppDirectoryEntry {
	return {
		installation_id: 'install_fixture', name: 'Meetings', description: '',
		icon: { kind: 'monogram', value: 'M' }, package_version: '1.0.0',
		package_revision_ref: 'package:meetings:1', installation_generation: 1, status: 'enabled',
		views: [{ view_id: 'sessions', label: 'Sessions', route: '/apps/install_fixture', pinned: false }], actions: [],
		navigation: [{ id: 'meetings', title: 'Meetings', route: '/meetings-console',
			placement: { kind: 'route' }, surface: { kind: 'custom_surface', entry_point: '/console', fallback_view: 'sessions' } }],
		storage: { record_count: 0, revision_count: 0, payload_bytes: 0, attachment_bytes: 0 },
		record_count: 0, payload_bytes: 0
	};
}

function fallbackHydration(): AppSurfaceHydration {
	return {
		binding: { installation_id: 'install_fixture', surface_revision: 1, view_id: 'sessions', app_local_route: '/' },
		surface: {
			installation_id: 'install_fixture', surface_revision: 1, view_id: 'sessions', interaction_mode: 'app',
			muij_document: { muij_version: '1', agent_id: 'app:install_fixture', generated_at: '2026-09-27T00:00:00Z', layout: [{
				id: 'app-sessions', component_type: 'EntityGrid', source: 'app_entity_query_v1', query: 'view:sessions',
				props: { rows: [], viewBinding: { entity: 'session', viewKind: 'table', fields: ['title'],
					fieldBindings: [{ field: 'title', kind: 'text', required: true, nullable: false, sortable: true }] } }
			}] }
		},
		route_parameters: {}, change_sequence: 0,
		page: { envelope: { installation_id: 'install_fixture', schema_revision: 1, value: [] } }
	};
}

beforeEach(() => {
	vi.resetAllMocks();
	vi.mocked(fetchAppDirectory).mockResolvedValue({ entries: [], has_more: false });
});
afterEach(() => cleanup());

describe('App surface host loading', () => {
	it.each([
		new AppSurfaceClientError('Registry busy', 503, 'app_registry_overloaded'),
		new AppSurfaceClientError('Sign in again', 401, 'app_scope_required'),
		new AppSurfaceClientError('Installation missing', 404, 'app_installation_not_found'),
		new AppSurfaceClientError('Surface changed', 409, 'app_surface_stale'),
		new TypeError('Connection dropped')
	])('does not probe unrelated hosts after %s', async (failure) => {
		vi.mocked(fetchAppSurface).mockRejectedValue(failure);
		const page = render(AppSurfacePage, { installationId: 'install_fixture' });
		await waitFor(() => expect(page.getByRole('alert').textContent).toContain(failure.message));
		expect(fetchAppSurface).toHaveBeenCalledTimes(1);
		expect(fetchScriptedSurfaceHost).not.toHaveBeenCalled();
		expect(fetchCustomSurfaceHost).not.toHaveBeenCalled();
	});

	it('keeps the legacy host available when native and scripted kinds are absent', async () => {
		vi.mocked(fetchAppSurface).mockRejectedValue(new AppSurfaceClientError('No native surface', 404, 'app_surface_not_found'));
		vi.mocked(fetchScriptedSurfaceHost).mockRejectedValue(new AppSurfaceClientError('No scripted surface', 404, 'app_custom_surface_unavailable'));
		vi.mocked(fetchCustomSurfaceHost).mockRejectedValue(new AppSurfaceClientError('No legacy surface', 404, 'app_custom_surface_not_found'));
		const page = render(AppSurfacePage, { installationId: 'install_fixture' });
		await waitFor(() => expect(page.getByRole('alert').textContent).toContain('No legacy surface'));
		expect(fetchAppSurface).toHaveBeenCalledTimes(1);
		expect(fetchScriptedSurfaceHost).toHaveBeenCalledTimes(1);
		expect(fetchCustomSurfaceHost).toHaveBeenCalledTimes(1);
	});

	it('stops at a scripted-host failure instead of starting a legacy session', async () => {
		vi.mocked(fetchAppSurface).mockRejectedValue(new AppSurfaceClientError('No native surface', 404, 'app_surface_not_found'));
		vi.mocked(fetchScriptedSurfaceHost).mockRejectedValue(new AppSurfaceClientError('Host unavailable', 503, 'app_custom_surface_unavailable'));
		const page = render(AppSurfacePage, { installationId: 'install_fixture' });
		await waitFor(() => expect(page.getByRole('alert').textContent).toContain('Host unavailable'));
		expect(fetchScriptedSurfaceHost).toHaveBeenCalledTimes(1);
		expect(fetchCustomSurfaceHost).not.toHaveBeenCalled();
	});

	it.each(['app_custom_surface_unavailable', 'app_custom_surface_not_found'])(
		'loads the declared standard view after %s and retries the interactive page on refresh', async (code) => {
			vi.mocked(fetchAppSurface).mockImplementation(async (_installation, path) => {
				if (path === '') return fallbackHydration();
				throw new AppSurfaceClientError('No native surface', 404, 'app_surface_not_found');
			});
			vi.mocked(fetchScriptedSurfaceHost).mockRejectedValue(new AppSurfaceClientError('Scripted page unavailable', 404, code));
			vi.mocked(fetchAppDirectory).mockResolvedValue({ entries: [directoryEntry()], has_more: false });
			const page = render(AppSurfacePage, { installationId: 'install_fixture', surfacePath: 'console' });
			await waitFor(() => expect(page.getByRole('heading', { name: 'Sessions' })).toBeTruthy());
			expect(page.getByRole('status').textContent).toContain('standard view');
			expect(fetchAppSurface).toHaveBeenLastCalledWith('install_fixture', '', expect.objectContaining({ signal: expect.any(AbortSignal) }));
			expect(fetchCustomSurfaceHost).not.toHaveBeenCalled();
			await fireEvent.click(page.getByRole('button', { name: 'Refresh' }));
			await waitFor(() => expect(fetchScriptedSurfaceHost).toHaveBeenCalledTimes(2));
			await waitFor(() => expect(page.getByRole('status').textContent).toContain('standard view'));
		}
	);

	it.each([401, 403, 409, 503])('does not load a fallback for a scripted host %s response', async (status) => {
		vi.mocked(fetchAppSurface).mockRejectedValue(new AppSurfaceClientError('No native surface', 404, 'app_surface_not_found'));
		vi.mocked(fetchScriptedSurfaceHost).mockRejectedValue(new AppSurfaceClientError('Host refused', status, 'app_custom_surface_unavailable'));
		const page = render(AppSurfacePage, { installationId: 'install_fixture', surfacePath: 'console' });
		await waitFor(() => expect(page.getByRole('alert').textContent).toContain('Host refused'));
		expect(fetchAppDirectory).not.toHaveBeenCalled();
		expect(fetchAppSurface).toHaveBeenCalledTimes(1);
	});

	it('keeps the scripted refusal when no fallback belongs to this installation', async () => {
		vi.mocked(fetchAppSurface).mockRejectedValue(new AppSurfaceClientError('No native surface', 404, 'app_surface_not_found'));
		vi.mocked(fetchScriptedSurfaceHost).mockRejectedValue(new AppSurfaceClientError('Entry point not granted', 404, 'app_custom_surface_not_found'));
		vi.mocked(fetchAppDirectory).mockResolvedValue({ entries: [{ ...directoryEntry(), installation_id: 'another_app' }], has_more: false });
		const page = render(AppSurfacePage, { installationId: 'install_fixture', surfacePath: 'console' });
		await waitFor(() => expect(page.getByRole('alert').textContent).toContain('Entry point not granted'));
		expect(fetchAppSurface).toHaveBeenCalledTimes(1);
		expect(fetchCustomSurfaceHost).not.toHaveBeenCalled();
	});
});
