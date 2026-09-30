import { get } from 'svelte/store';
import { beforeEach, describe, expect, it } from 'vitest';

import { scopeIdentityStore } from './scopeIdentityStore';
import { installFetchMock, jsonResponse } from '../../test/browser';
import {
	vibeDevProjectStore,
	type VibeDevProject
} from './vibeDevProjectStore';

function project(
	id: string,
	overrides: Partial<VibeDevProject> = {}
): VibeDevProject {
	return {
		project_id: id,
		name: `Project ${id}`,
		chat_thread_id: `thread-${id}`,
		chat_session_id: `session-${id}`,
		deployments: [],
		deploy_targets: [],
		created_at_ms: 1,
		updated_at_ms: 1,
		archived: false,
		chat_session_status: 'active',
		...overrides
	};
}

beforeEach(() => {
	vibeDevProjectStore.clear();
	scopeIdentityStore.observe('alice', 'workspace-a');
});

describe('vibeDevProjectStore', () => {
	it('loads scoped projects and honors the backend active project', async () => {
		const active = project('active', { updated_at_ms: 20 });
		const archived = project('archived', {
			archived: true,
			chat_session_status: 'archived',
			updated_at_ms: 10
		});
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/vibedev/projects',
				handle: () =>
					jsonResponse({
						projects: [active, archived],
						active_project_id: 'active',
						workspace_display_path: '~/VibeDev',
						workspace_absolute_path: '/notes/VibeDev'
					})
			}
		]);

		await vibeDevProjectStore.load();

		expect(get(vibeDevProjectStore)).toMatchObject({
			activeProjectId: 'active',
			workspaceDisplayPath: '~/VibeDev',
			workspaceAbsolutePath: '/notes/VibeDev',
			isLoading: false,
			error: null
		});
		expect(calls[0]?.url).not.toContain('principal=');
		expect(calls[0]?.url).not.toContain('workspace=');
	});

	it('clears the previous workspace immediately while a new scope loads', async () => {
		installFetchMock([
			{
				match: '/vibedev/projects',
				handle: () => jsonResponse({ projects: [project('old')], active_project_id: 'old' })
			}
		]);
		await vibeDevProjectStore.load();

		let resolveNew!: (response: Response) => void;
		installFetchMock([
			{
				match: '/vibedev/projects',
				handle: () => new Promise<Response>((resolve) => (resolveNew = resolve))
			}
		]);
		scopeIdentityStore.observe('bob', 'workspace-b');
		const loading = vibeDevProjectStore.load();

		expect(get(vibeDevProjectStore)).toMatchObject({
			projects: [],
			activeProjectId: null,
			isLoading: true,
			scopeKey: 'bob::workspace-b'
		});
		resolveNew(jsonResponse({ projects: [project('new')], active_project_id: 'new' }));
		await loading;
		expect(get(vibeDevProjectStore).projects.map((item) => item.project_id)).toEqual([
			'new'
		]);
	});

	it('creates a project with its repository path and makes it active', async () => {
		const created = project('created', { repo_path: './demo', updated_at_ms: 30 });
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: '/vibedev/projects',
				handle: () => jsonResponse({ project: created })
			}
		]);

		expect(
			await vibeDevProjectStore.createProject({ name: 'Demo', repo_path: './demo' })
		).toEqual(created);
		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
			name: 'Demo',
			repo_path: './demo'
		});
		expect(get(vibeDevProjectStore).activeProjectId).toBe('created');
	});

	it('activates one project and archives the previously active chat session', async () => {
		const first = project('first', { updated_at_ms: 20 });
		const second = project('second', {
			chat_session_status: 'archived',
			updated_at_ms: 10
		});
		installFetchMock([
			{
				match: '/vibedev/projects',
				handle: () => jsonResponse({ projects: [first, second], active_project_id: 'first' })
			}
		]);
		await vibeDevProjectStore.load();
		const activated = { ...second, chat_session_status: 'active', updated_at_ms: 40 };
		installFetchMock([
			{
				method: 'POST',
				match: '/projects/second/activate',
				handle: () => jsonResponse({ project: activated })
			}
		]);

		await vibeDevProjectStore.activateProject('second');

		const state = get(vibeDevProjectStore);
		expect(state.activeProjectId).toBe('second');
		expect(
			state.projects.find((item) => item.project_id === 'first')?.chat_session_status
		).toBe('archived');
	});

	it('removes a deleted active project and selects the next available project', async () => {
		const first = project('first', { updated_at_ms: 20 });
		const second = project('second', {
			chat_session_status: 'archived',
			updated_at_ms: 10
		});
		installFetchMock([
			{
				match: '/vibedev/projects',
				handle: () => jsonResponse({ projects: [first, second], active_project_id: 'first' })
			}
		]);
		await vibeDevProjectStore.load();
		installFetchMock([
			{
				method: 'DELETE',
				match: '/projects/first',
				handle: () =>
					jsonResponse({
						deleted: true,
						project_id: 'first',
						chat_session_id: 'session-first',
						chat_session_deleted: true
					})
			}
		]);

		await vibeDevProjectStore.deleteProject('first');

		expect(get(vibeDevProjectStore).projects.map((item) => item.project_id)).toEqual([
			'second'
		]);
		expect(get(vibeDevProjectStore).activeProjectId).toBe('second');
	});
});
