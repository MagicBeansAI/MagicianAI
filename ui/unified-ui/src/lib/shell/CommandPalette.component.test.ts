import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { goto } from '$app/navigation';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { taskStore } from '$lib/stores/taskStore';
import { backendTask, createTaskAttentionBackend } from '../../test/taskAttentionBackend';
import { installBrowserTestPolyfills, installFetchMock, jsonResponse } from '../../test/browser';
import CommandPalette from './CommandPalette.svelte';

vi.mock('$app/stores', async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/tasks') }) };
});

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

beforeEach(() => {
	vi.clearAllMocks();
	scopeIdentityStore.reset();
	taskStore.reset();
	installBrowserTestPolyfills();
});

afterEach(() => {
	cleanup();
	taskStore.reset();
});

describe('CommandPalette execution controls', () => {
	it('puts the literal Settings destination ahead of unrelated fuzzy matches', async () => {
		const backend = createTaskAttentionBackend([]);
		installFetchMock([
			{ method: 'GET', match: '/api/magician/v2/agents', handle: () => jsonResponse({ agents: [], system_agents: [] }) },
			{ method: 'GET', match: '/api/magician/v2/approvals', handle: () => jsonResponse({ approvals: [] }) },
			...backend.routes()
		]);
		const user = userEvent.setup();

		render(CommandPalette, { open: true });
		await user.type(screen.getByRole('combobox'), 'Settings');

		const visibleItems = screen
			.getAllByRole('option')
			.filter((item) => item.getAttribute('aria-hidden') !== 'true');
		expect(visibleItems).toHaveLength(1);
		expect(visibleItems[0]).toHaveTextContent('Settings');
	});

	it('defers palette hydration until opened while sharing shell App navigation', async () => {
		const backend = createTaskAttentionBackend([]);
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/api/magician/v2/agents', handle: () => jsonResponse({ agents: [], system_agents: [] }) },
			{ method: 'GET', match: '/api/magician/v2/approvals', handle: () => jsonResponse({ approvals: [] }) },
			...backend.routes()
		]);
		const component = render(CommandPalette, { open: false });
		await new Promise((resolve) => setTimeout(resolve, 0));
		// The shared navigation store also serves TopBar and owns its one
		// directory poll. No palette-only hydration may run while closed.
		expect(calls.filter((call) => new URL(call.url, 'http://localhost').pathname !== '/api/magician/v2/apps/directory')).toEqual([]);
		await component.rerender({ open: true });
		await waitFor(() => expect(calls.some((call) => call.url.includes('/api/magician/v2/agents'))).toBe(true));
	});

	it('handles an agent refresh timeout while keeping palette commands usable', async () => {
		const backend = createTaskAttentionBackend([]);
		installFetchMock([
			{ method: 'GET', match: '/api/magician/v2/agents', handle: () => jsonResponse({ error: 'Request timed out' }, { status: 408 }) },
			{ method: 'GET', match: '/api/magician/v2/approvals', handle: () => jsonResponse({ approvals: [] }) },
			...backend.routes()
		]);
		render(CommandPalette, { open: true });
		expect(await screen.findByRole('combobox')).toBeEnabled();
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(screen.getByRole('dialog', { name: 'Command palette' })).toBeVisible();
	});

	it('discovers and opens an enabled app from server metadata', async () => {
		const backend = createTaskAttentionBackend([]);
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/apps/directory',
				handle: () => jsonResponse({
					entries: [{
						installation_id: 'install_trip',
						name: 'Trip planner',
						description: 'Private itinerary',
						icon: { kind: 'monogram', value: 'T' },
						package_version: '1.0.0',
						package_revision_ref: 'package:trip',
						installation_generation: 1,
						status: 'enabled',
						default_route: '/apps/install_trip',
						views: [{ view_id: 'trips', label: 'Trips', route: '/apps/install_trip', pinned: false }],
						actions: [],
						permissions: {
							granted_tools: 0,
							granted_context_reads: 0,
							granted_personal_data_projections: 0,
							background_execution: false,
							network_access: false
						},
						storage: { record_count: 0, revision_count: 0, payload_bytes: 0, attachment_bytes: 0 },
						record_count: 0,
						payload_bytes: 0
					}],
					has_more: false
				})
			},
			{
				method: 'POST',
				match: '/directory-activity',
				handle: () => jsonResponse({
					installation_id: 'install_trip',
					updated_at: '2026-08-17T00:00:00Z'
				})
			},
			{
				method: 'POST',
				match: '/api/magician/v2/chat/enroll',
				handle: () => jsonResponse({ enrolled: false, code: 'test-pending' })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/approvals',
				handle: () => jsonResponse({ approvals: [] })
			},
			{
				method: 'GET',
				match: '/api/magician/v2/agents',
				handle: () => jsonResponse({ agents: [], system_agents: [] })
			},
			...backend.routes()
		]);
		const user = userEvent.setup();

		render(CommandPalette, { open: true });
		await user.type(screen.getByRole('combobox'), 'Trip planner');
		await waitFor(() => {
			expect(calls.some((call) => call.method === 'GET' && call.url.includes('search=Trip+planner'))).toBe(true);
		});
		const appMatches = await screen.findAllByText('Trip planner');
		expect(appMatches.length).toBeGreaterThan(0);
		await user.click(appMatches[0]);

		await waitFor(() => expect(goto).toHaveBeenCalledWith('/apps/install_trip'));
	});

	it('routes Browser direct execution to Internal tasks with user provenance', async () => {
		const backend = createTaskAttentionBackend([]);
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: '/api/magician/v2/executions',
				handle: () => jsonResponse({ execution_id: 'browser-run' }, { status: 202 })
			},
			...backend.routes()
		]);
		const user = userEvent.setup();

		render(CommandPalette, { open: true });
		await user.type(screen.getByRole('combobox'), 'Browser');
		await user.click(await screen.findByText('Browser…'));
		await user.type(screen.getByRole('combobox'), 'Summarize https://example.com');
		await user.click(await screen.findByText('Dispatch: "Summarize https://example.com"'));

		await waitFor(() => {
			const call = calls.find(
				(candidate) =>
					candidate.method === 'POST' &&
					candidate.url.endsWith('/api/magician/v2/executions')
			);
			expect(call).toBeDefined();
			const body = JSON.parse(String(call?.init?.body));
			expect(body).toMatchObject({
				initial_message: 'Summarize https://example.com',
				skip_planning: true,
				internal: true
			});
			expect(body.debug).toBeUndefined();
		});
	});

	it('routes command-palette SOTA fixtures as debug Internal executions', async () => {
		const backend = createTaskAttentionBackend([]);
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: '/api/magician/v2/executions',
				handle: () => jsonResponse({ execution_id: 'sota-run' }, { status: 202 })
			},
			...backend.routes()
		]);
		const user = userEvent.setup();

		render(CommandPalette, { open: true });
		await user.type(screen.getByRole('combobox'), 'SOTA');
		await user.click(await screen.findByText('SOTA…'));
		await user.click(await screen.findByText('01 · Cross Origin Iframe'));

		await waitFor(() => {
			const call = calls.find(
				(candidate) =>
					candidate.method === 'POST' &&
					candidate.url.endsWith('/api/magician/v2/executions')
			);
			expect(call).toBeDefined();
			const body = JSON.parse(String(call?.init?.body));
			expect(body).toMatchObject({
				skip_planning: true,
				debug: true
			});
			expect(body.internal).toBeUndefined();
		});
	});

	it('opens the internal task view through its canonical Tasks URL', async () => {
		const backend = createTaskAttentionBackend([]);
		installFetchMock(backend.routes());
		const user = userEvent.setup();

		render(CommandPalette, { open: true });
		await user.type(screen.getByRole('combobox'), 'Internal Tasks');
		await user.click(await screen.findByText('Internal Tasks'));

		await waitFor(() => {
			expect(goto).toHaveBeenCalledWith('/tasks?type=internal');
		});
	});

	it.each(['paused', 'planning'] as const)(
		'offers authoritative cancellation while the task is %s',
		async (status) => {
			const task = backendTask(`palette-cancel-${status}`, status, {
				title: `Cancel ${status} run`,
				active_root_execution_id: `execution-${status}`,
				latest_root_execution_id: `execution-${status}`
			});
			const backend = createTaskAttentionBackend([task]);
			installFetchMock(backend.routes());
			const user = userEvent.setup();
			await taskStore.loadTasks();

			render(CommandPalette, { open: true });
			await user.click((await screen.findAllByText(task.title))[0]);

			expect(await screen.findByText('Cancel execution')).toBeInTheDocument();
		}
	);

	it('loads task capabilities and submits Steer through a text-entry subpage', async () => {
		const task = backendTask('palette-run', 'running', { title: 'Investigate queue stall' });
		const backend = createTaskAttentionBackend([task]);
		const { calls } = installFetchMock(backend.routes());
		const user = userEvent.setup();
		await taskStore.loadTasks();

		render(CommandPalette, { open: true });
		await user.click((await screen.findAllByText(task.title))[0]);
		await waitFor(() => {
			expect(calls.some((call) => call.url.endsWith('/control-state'))).toBe(true);
		});
		await user.click(await screen.findByText('Steer execution'));

		const input = screen.getByPlaceholderText('Guide the next decision turn — press Enter to send');
		await user.type(input, 'Finish the current query, then inspect the queue owner.');
		await user.click(await screen.findByText(/Send steer:/));

		await waitFor(() => {
			expect(
				calls.some(
					(call) =>
						call.method === 'POST' && call.url.endsWith('/executions/execution-palette-run/steer')
				)
			).toBe(true);
		});
	});

	it('does not apply a delayed capability response to a replacement execution', async () => {
		const task = backendTask('palette-replace', 'running', {
			title: 'Replace active run',
			active_root_execution_id: 'execution-old',
			latest_root_execution_id: 'execution-old'
		});
		const backend = createTaskAttentionBackend([task]);
		let resolveOld!: (response: Response) => void;
		const oldState = new Promise<Response>((resolve) => { resolveOld = resolve; });
		const pausedExecutions: string[] = [];
		const controlRoutes = [
			{
				method: 'GET',
				match: (call: { url: string }) => call.url.endsWith('/execution-old/control-state'),
				handle: () => oldState
			},
			{
				method: 'GET',
				match: (call: { url: string }) => call.url.endsWith('/execution-new/control-state'),
				handle: () => jsonResponse({
					execution_id: 'execution-new',
					waiting_state: 'executing',
					active: true,
					can_pause: true,
					can_resume: false,
					can_steer: false,
					can_cancel: true
				})
			},
			{
				method: 'POST',
				match: /\/executions\/[^/]+\/pause$/,
				handle: (call: { url: string }) => {
					pausedExecutions.push(call.url.includes('execution-new') ? 'execution-new' : 'execution-old');
					return jsonResponse({ paused: true });
				}
			}
		];
		const { calls } = installFetchMock([...controlRoutes, ...backend.routes()]);
		const user = userEvent.setup();
		await taskStore.loadTasks();

		render(CommandPalette, { open: true });
		await user.click((await screen.findAllByText(task.title))[0]);
		await waitFor(() => {
			expect(calls.some((call) => call.url.endsWith('/execution-old/control-state'))).toBe(true);
		});

		backend.setTaskStatus(task.id, 'running', {
			active_root_execution_id: 'execution-new',
			latest_root_execution_id: 'execution-new'
		});
		await taskStore.loadTasks();
		await waitFor(() => {
			expect(calls.some((call) => call.url.endsWith('/execution-new/control-state'))).toBe(true);
		});
		resolveOld(jsonResponse({
			execution_id: 'execution-old',
			waiting_state: 'executing',
			active: true,
			can_pause: true,
			can_resume: false,
			can_steer: true,
			can_cancel: true
		}));

		await user.click(await screen.findByText('Pause execution'));
		await waitFor(() => expect(pausedExecutions).toEqual(['execution-new']));
	});

	it('does not steer an execution replaced while the steer composer is open', async () => {
		const task = backendTask('palette-steer-replace', 'running', {
			title: 'Replace steer target',
			active_root_execution_id: 'execution-old',
			latest_root_execution_id: 'execution-old'
		});
		const backend = createTaskAttentionBackend([task]);
		const { calls } = installFetchMock(backend.routes());
		const user = userEvent.setup();
		await taskStore.loadTasks();

		render(CommandPalette, { open: true });
		await user.click((await screen.findAllByText(task.title))[0]);
		await user.click(await screen.findByText('Steer execution'));

		backend.setTaskStatus(task.id, 'running', {
			active_root_execution_id: 'execution-new',
			latest_root_execution_id: 'execution-new'
		});
		await taskStore.loadTasks();

		const input = screen.getByPlaceholderText('Guide the next decision turn — press Enter to send');
		await user.type(input, 'Continue with the replacement run.');
		await user.click(await screen.findByText(/Send steer:/));

		await waitFor(() => {
			expect(screen.getByText('Open task')).toBeInTheDocument();
		});
		expect(
			calls.some(
				(call) =>
					call.method === 'POST' &&
					call.url.endsWith('/executions/execution-old/steer')
			)
		).toBe(false);
	});
});
