import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../../test/browser';
import AppOptionsPanel from './AppOptionsPanel.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('AppOptionsPanel catalog isolation', () => {
	it('fetches only visited tabs, retains selections, and reopens without another scan', async () => {
		const { calls } = installFetchMock([
			{
				match: '/apps/authoring/tools',
				handle: () => jsonResponse({
					status: 'ok', count: 1, items: [{
						name: 'time_math', kind: 'compiled', description: 'Dates.',
						yaml_declaration: '- name: time_math', app_eligible: true, dispatchable: true
					}]
				})
			},
			{
				match: '/apps/authoring/agents',
				handle: () => jsonResponse({ status: 'ok', count: 0, items: [] })
			}
		]);
		const component = render(AppOptionsPanel, { open: false });
		expect(calls).toHaveLength(0);

		await component.rerender({ open: true });
		expect(await screen.findByText('time_math')).toBeInTheDocument();
		expect(calls.map(call => call.url)).toEqual(['/api/magician/v2/apps/authoring/tools']);
		await fireEvent.click(screen.getByRole('checkbox', { name: /time_math/ }));
		expect(screen.getByText('1 selected')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Agents' }));
		await waitFor(() => expect(screen.queryByText('Loading catalog…')).not.toBeInTheDocument());
		expect(calls).toHaveLength(2);

		await component.rerender({ open: false });
		await component.rerender({ open: true });
		await fireEvent.click(screen.getByRole('button', { name: 'Tools' }));
		expect(screen.getByRole('checkbox', { name: /time_math/ })).toBeChecked();
		expect(calls).toHaveLength(2);
	});

	it('shows a ready tab while another request is pending without fetching that pending tab twice', async () => {
		let finishTools!: (response: Response) => void;
		const tools = new Promise<Response>(resolve => { finishTools = resolve; });
		const { calls } = installFetchMock([
			{ match: '/apps/authoring/tools', handle: () => tools },
			{
				match: '/apps/authoring/personalities',
				handle: () => jsonResponse({ status: 'ok', count: 1, items: [{
					name: 'precise', description: 'Be precise.', yaml_declaration: 'personality: precise'
				}] })
			}
		]);
		render(AppOptionsPanel, { open: true });
		expect(await screen.findByText('Loading catalog…')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Personalities' }));
		expect(await screen.findByText('precise')).toBeInTheDocument();
		expect(screen.queryByText('Loading catalog…')).not.toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Tools' }));
		expect(screen.getByText('Loading catalog…')).toBeInTheDocument();
		expect(calls).toHaveLength(2);
		finishTools(jsonResponse({ status: 'ok', count: 0, items: [] }));
		await waitFor(() => expect(screen.queryByText('Loading catalog…')).not.toBeInTheDocument());
		await fireEvent.click(screen.getByRole('button', { name: 'Personalities' }));
		expect(screen.getByText('precise')).toBeInTheDocument();
		expect(calls).toHaveLength(2);
	});

	it('keeps degraded tools visible when the agent catalog fails', async () => {
		let agentsAvailable = false;
		const { calls } = installFetchMock([
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
				handle: () => agentsAvailable
					? jsonResponse({ status: 'ok', count: 1, items: [{
						name: 'Muse', description: 'Create media.', yaml_declaration: 'agent: Muse'
					}] })
					: jsonResponse({ message: 'The agent catalog is unavailable.' }, { status: 503 })
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
		agentsAvailable = true;
		await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
		expect(await screen.findByText('Muse')).toBeInTheDocument();
		expect(screen.queryByText('The agent catalog is unavailable.')).not.toBeInTheDocument();
		expect(calls.map(call => call.url)).toEqual([
			'/api/magician/v2/apps/authoring/tools',
			'/api/magician/v2/apps/authoring/agents',
			'/api/magician/v2/apps/authoring/agents'
		]);

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
