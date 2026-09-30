import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../../test/browser';
import AppOptionsPanel from './AppOptionsPanel.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('AppOptionsPanel catalog isolation', () => {
	it('keeps degraded tools visible when the agent catalog fails', async () => {
		installFetchMock([
			{
				match: '/apps/authoring/tools',
				handle: () =>
					jsonResponse({
						status: 'degraded',
						count: 1,
						items: [
							{
								name: 'browser__snapshot',
								kind: 'interactive',
								description: 'Observe the reviewed browser.',
								yaml_declaration: '- name: browser__snapshot',
								app_eligible: true,
								lock_review_required: true,
								dispatchable: true
							}
						]
					}),
			},
			{
				match: '/apps/authoring/agents',
				handle: () => jsonResponse({ message: 'The agent catalog is unavailable.' }, { status: 503 })
			},
			{
				match: '/apps/authoring/personalities',
				handle: () => jsonResponse({ status: 'ok', count: 0, items: [] })
			},
			{
				match: '/apps/authoring/procedures',
				handle: () => jsonResponse({ status: 'ok', count: 0, items: [] })
			}
		]);

		render(AppOptionsPanel, { open: true });

		expect(await screen.findByText('browser__snapshot')).toBeInTheDocument();
		expect(screen.getByText('interactive')).toBeInTheDocument();
		expect(screen.getByText(/Some tool sources could not be loaded/)).toBeInTheDocument();

		await fireEvent.click(screen.getByRole('button', { name: 'Agents' }));
		expect(await screen.findByText('The agent catalog is unavailable.')).toBeInTheDocument();

		await fireEvent.click(screen.getByRole('button', { name: 'Tools' }));
		expect(screen.getByText('browser__snapshot')).toBeInTheDocument();
	});

	it('shows runnable tools first and collapses blocked tools under their blocker', async () => {
		const jail = 'skillshub/CLI tools need OS-jail contain before they can run in apps';
		const skill = (name: string) => ({
			name,
			kind: 'skill',
			description: `${name} skill`,
			yaml_declaration: `- name: ${name}`,
			app_eligible: true,
			lock_review_required: true,
			dispatchable: false,
			dispatch_note: jail
		});
		installFetchMock([
			{
				match: '/apps/authoring/tools',
				handle: () =>
					jsonResponse({
						status: 'ok',
						count: 3,
						items: [
							{
								name: 'time_math',
								kind: 'compiled',
								description: 'Dates.',
								yaml_declaration: '- name: time_math',
								app_eligible: true,
								lock_review_required: false,
								dispatchable: true
							},
							skill('youtube-search'),
							skill('weather')
						]
					})
			},
			{
				match: '/apps/authoring/agents',
				handle: () => jsonResponse({ status: 'ok', count: 0, items: [] })
			},
			{
				match: '/apps/authoring/personalities',
				handle: () => jsonResponse({ status: 'ok', count: 0, items: [] })
			},
			{
				match: '/apps/authoring/procedures',
				handle: () => jsonResponse({ status: 'ok', count: 0, items: [] })
			}
		]);

		render(AppOptionsPanel, { open: true });

		expect(await screen.findByText('Ready in apps (1)')).toBeInTheDocument();
		expect(screen.getByText(/Not yet runnable in apps \(2\)/)).toBeInTheDocument();
		// The blocker is stated once for its group, not once per tool.
		expect(screen.getAllByText(jail)).toHaveLength(1);
		expect(screen.getByText('youtube-search').closest('details')).not.toBeNull();
		expect(screen.getByText('time_math').closest('details')).toBeNull();
	});
});
