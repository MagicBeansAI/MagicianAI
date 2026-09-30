import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { installBrowserTestPolyfills, installFetchMock, jsonResponse } from '../../../test/browser';
import SkillsPage from './+page.svelte';

const mockPageUrl = vi.hoisted(() => ({ value: new URL('http://localhost/skills') }));

vi.mock('$app/stores', async () => {
	const { readable } = await import('svelte/store');
	return {
		page: readable<{ url: URL } | undefined>(undefined, (set) => {
			// Live getter: tests retarget mockPageUrl before render to prove
			// the `?tab=` deep-link path without real navigation.
			set({ get url() { return mockPageUrl.value; } });
		})
	};
});

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

const CATALOG = {
	skills: [
		{
			name: 'tool-x',
			description: 'a tool',
			kind: 'tool',
			origin: 'skillshub',
			requires_bins: [],
			requires_env: ['TOOL_X_KEY'],
			installed_scopes: []
		},
		{
			name: 'proc-x',
			description: 'a procedure',
			kind: 'procedure',
			origin: 'skillshub',
			requires_bins: [],
			requires_env: [],
			installed_scopes: ['anonymous/default']
		},
		{
			name: 'persona-x',
			description: 'a personality',
			kind: 'personality',
			origin: 'skillshub',
			requires_bins: [],
			requires_env: [],
			installed_scopes: ['alice/other']
		},
		{
			name: 'built-in-x',
			description: 'a compiled pack',
			kind: 'compiled',
			origin: 'built-in',
			requires_bins: [],
			requires_env: [],
			installed_scopes: []
		}
	]
};

beforeEach(() => {
	vi.clearAllMocks();
	scopeIdentityStore.reset();
	installBrowserTestPolyfills();
	sessionStorage.setItem('magician:vault:setup-token', 'tok-1');
});

afterEach(() => {
	cleanup();
});

describe('Skills catalog page', () => {
	it('renders every kind with per-kind tabs and counts', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/skills/catalog',
				handle: () => jsonResponse(CATALOG)
			}
		]);
		render(SkillsPage);
		await waitFor(() => expect(calls.length).toBeGreaterThan(0));
		await screen.findByText('tool-x');
		expect(screen.getByText('proc-x')).toBeTruthy();
		expect(screen.getByText('persona-x')).toBeTruthy();
		expect(screen.getByText('built-in-x')).toBeTruthy();
		for (const tab of ['All 4', 'Tools 1', 'Procedures 1', 'Personalities 1', 'Built-ins 1']) {
			expect(screen.getByRole('tab', { name: new RegExp(tab) })).toBeTruthy();
		}
	});

	it('gates row actions on install state: Install for absent, Remove for installed here', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/skills/catalog',
				handle: () => jsonResponse(CATALOG)
			}
		]);
		render(SkillsPage);
		await screen.findByText('tool-x');
		// Not installed anywhere and authored in skillshub → installable.
		const installBtn = screen
			.getAllByRole('button')
			.find((b) => b.textContent === 'Install' && b.closest('li')?.textContent?.includes('tool-x'));
		expect(installBtn).toBeTruthy();
		// Installed in the active scope → Remove offered.
		const removeBtn = screen
			.getAllByRole('button')
			.find((b) => b.textContent === 'Remove' && b.closest('li')?.textContent?.includes('proc-x'));
		expect(removeBtn).toBeTruthy();
		// Compiled packs are read-only.
		const compiledRow = screen.getByText('built-in-x').closest('li');
		expect(compiledRow?.textContent).not.toContain('Install');
		expect(compiledRow?.textContent).not.toContain('Remove');
	});

	it('installs into the active scope via skillshub source with the setup token', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/skills/catalog',
				handle: () => jsonResponse(CATALOG)
			},
			{
				method: 'POST',
				match: '/api/magician/v2/skills/install',
				handle: () => jsonResponse({ installed: ['tool-x'], kind: 'tool', targets: [] })
			}
		]);
		render(SkillsPage);
		await screen.findByText('tool-x');
		const installBtn = screen
			.getAllByRole('button')
			.find((b) => b.textContent === 'Install' && b.closest('li')?.textContent?.includes('tool-x'));
		await userEvent.click(installBtn!);
		await waitFor(() =>
			expect(calls.some((c) => c.method === 'POST' && c.url.includes('/skills/install'))).toBe(true)
		);
		const post = calls.find((c) => c.method === 'POST' && c.url.includes('/skills/install'));
		expect(JSON.parse(String(post?.init?.body))).toEqual({
			source: 'skillshub:tool-x',
			target: { workspaces: ['anonymous/default'] }
		});
		expect((post?.init?.headers as Record<string, string>)['X-Magician-Setup-Token']).toBe(
			'tok-1'
		);
	});

	it('deep-links filter the catalog to one kind via ?tab=', async () => {
		mockPageUrl.value = new URL('http://localhost/skills?tab=personality');
		installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/skills/catalog',
				handle: () => jsonResponse(CATALOG)
			}
		]);
		render(SkillsPage);
		await screen.findByText('persona-x');
		await waitFor(() => expect(screen.queryByText('tool-x')).toBeNull());
		expect(screen.queryByText('proc-x')).toBeNull();
		expect(screen.getByRole('tab', { name: /Personalities/ }).getAttribute('aria-selected')).toBe('true');
	});
});
