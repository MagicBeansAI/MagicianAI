import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import {
	createWorkspace,
	deleteWorkspace,
	isValidWorkspaceSlug,
	listWorkspaces,
	suggestWorkspaceSlug,
	updateWorkspace,
	WorkspaceRequestError
} from './workspacesStore';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('workspacesStore', () => {
	it('lists workspaces default-first and tolerates missing fields', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/workspaces',
				handle: () =>
					jsonResponse([
						{ id: 'zeta', display_name: 'Zeta', is_default: false, agent_count: 2, active_task_count: 1 },
						{ id: 'default', display_name: 'Personal', is_default: true },
						{ id: 'alpha', display_name: 'Alpha', is_default: false },
						{ display_name: 'no id — dropped' }
					])
			}
		]);
		const cards = await listWorkspaces();
		expect(cards.map((c) => c.id)).toEqual(['default', 'alpha', 'zeta']);
		expect(cards[0].agent_count).toBe(0);
		expect(cards[2]).toMatchObject({ agent_count: 2, active_task_count: 1 });
	});

	it('keeps the server status and error code on a failure', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/api/magician/v2/workspaces',
				handle: () =>
					jsonResponse(
						{ error: 'workspace_pending_purge', message: 'free again after restart' },
						{ status: 409 }
					)
			}
		]);
		const failure = await createWorkspace({ slug: 'eval', display_name: 'Eval' }).catch((e) => e);
		expect(failure).toBeInstanceOf(WorkspaceRequestError);
		expect(failure).toMatchObject({ status: 409, code: 'workspace_pending_purge' });
		expect(failure.message).toBe('free again after restart');
	});

	it('sends a trimmed create body and a null for an empty description', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/api/magician/v2/workspaces', handle: () => jsonResponse({}, { status: 201 }) }
		]);
		await createWorkspace({ slug: ' research ', display_name: '  Research ', description: '   ' });
		expect(JSON.parse(String(calls[0].init?.body))).toEqual({
			slug: 'research',
			display_name: 'Research',
			description: null
		});
	});

	it('distinguishes clearing a description from leaving it alone', async () => {
		const { calls } = installFetchMock([
			{ method: 'PATCH', match: '/api/magician/v2/workspaces/r', handle: () => jsonResponse({}) }
		]);
		await updateWorkspace('r', { display_name: 'R', description: null });
		await updateWorkspace('r', { display_name: 'R' });
		expect(JSON.parse(String(calls[0].init?.body))).toEqual({ display_name: 'R', description: null });
		expect(JSON.parse(String(calls[1].init?.body))).toEqual({ display_name: 'R' });
	});

	it('only asks the server to purge data when told to', async () => {
		const { calls } = installFetchMock([
			{ method: 'DELETE', match: '/api/magician/v2/workspaces/', handle: () => new Response(null, { status: 204 }) }
		]);
		expect(await deleteWorkspace('plain')).toEqual({ dataRemoval: 'none' });
		expect(await deleteWorkspace('with-data', { purge: true })).toEqual({
			dataRemoval: 'scheduled_for_next_start'
		});
		expect(calls.map((c) => c.url)).toEqual([
			'/api/magician/v2/workspaces/plain',
			'/api/magician/v2/workspaces/with-data?purge=true'
		]);
	});

	it('validates slugs exactly as the server does', () => {
		for (const ok of ['a', 'research', 'eval_2', 'recipes-eval', 'x'.repeat(32)]) {
			expect(isValidWorkspaceSlug(ok)).toBe(true);
		}
		for (const bad of ['', 'Research', 'has space', '../x', 'é', 'x'.repeat(33)]) {
			expect(isValidWorkspaceSlug(bad)).toBe(false);
		}
	});

	it('suggests a valid slug from a display name', () => {
		expect(suggestWorkspaceSlug('Research Notes')).toBe('research-notes');
		expect(suggestWorkspaceSlug('  Café  Déjà vu!! ')).toBe('cafe-deja-vu');
		expect(suggestWorkspaceSlug('!!!')).toBe('');
		const long = suggestWorkspaceSlug('a very long workspace name that keeps going on');
		expect(long.length).toBeLessThanOrEqual(32);
		expect(isValidWorkspaceSlug(long)).toBe(true);
		expect(long.endsWith('-')).toBe(false);
	});
});
