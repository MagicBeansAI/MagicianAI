import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import ChatHistoryRail from './ChatHistoryRail.svelte';

const { gotoMock } = vi.hoisted(() => ({
	gotoMock: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$app/navigation', () => ({ goto: gotoMock }));

afterEach(() => {
	cleanup();
	localStorage.clear();
	vi.clearAllMocks();
	vi.unstubAllGlobals();
});

describe('ChatHistoryRail component', () => {
	it('renders expanded rail with History title and new button', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => {
			return new Response(JSON.stringify({ sessions: [], total: 0, limit: 15, offset: 0 }), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		}));

		render(ChatHistoryRail, { expanded: true });
		expect(screen.getByRole('heading', { name: 'History' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Start new chat' })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: 'Sessions' })).toBeInTheDocument();
		expect(screen.getByRole('tab', { name: 'Threads' })).toBeInTheDocument();
	});

	it('renders collapsed strip with expand affordance when expanded is false', () => {
		render(ChatHistoryRail, { expanded: false });
		expect(screen.getByRole('button', { name: 'Expand history' })).toBeInTheDocument();
		expect(screen.queryByRole('heading', { name: 'History' })).toBeNull();
	});

	it('renders sessions when loaded from API', async () => {
		const sampleSession = {
			id: 'sess-123',
			title: 'Fix authentication flow',
			status: 'active',
			ui_thread_id: 'general',
			updated_at: new Date().toISOString()
		};

		vi.stubGlobal('fetch', vi.fn(async () => {
			return new Response(JSON.stringify({ sessions: [sampleSession], total: 1, limit: 15, offset: 0 }), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		}));

		render(ChatHistoryRail, { expanded: true });
		expect(await screen.findByText('Fix authentication flow')).toBeInTheDocument();
		expect(screen.getByText('#general')).toBeInTheDocument();
	});

	it('opens contextual new menu with session and thread options', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => {
			return new Response(JSON.stringify({ sessions: [], total: 0, limit: 15, offset: 0 }), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		}));

		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });
		const newBtn = screen.getByRole('button', { name: 'Start new chat' });
		await user.click(newBtn);

		expect(screen.getByRole('menuitem', { name: 'New Session' })).toBeInTheDocument();
		expect(screen.getByRole('menuitem', { name: 'New Thread' })).toBeInTheDocument();
	});

	it('routes vibedev session click to /vibe?session=... and vibedev thread to /vibe', async () => {
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			if (url.pathname.endsWith('/chat/sessions')) {
				return new Response(JSON.stringify({
					sessions: [{
						id: 'vibe-sess-99',
						title: 'Landing page build',
						status: 'active',
						ui_thread_id: 'vibedev',
						updated_at: new Date().toISOString()
					}],
					total: 1, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			if (url.pathname.endsWith('/ui-threads')) {
				return new Response(JSON.stringify({
					threads: [{
						id: 'vibedev',
						name: 'VibeDev',
						archived: false,
						updated_at: new Date().toISOString()
					}],
					total: 1, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			return new Response('not found', { status: 404 });
		}));

		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });

		// Click vibedev session
		expect(await screen.findByText('Landing page build')).toBeInTheDocument();
		await user.click(screen.getByText('Landing page build'));
		expect(gotoMock).toHaveBeenCalledWith('/vibe?session=vibe-sess-99');

		// Click vibedev thread
		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		expect(await screen.findByText('VibeDev')).toBeInTheDocument();
		await user.click(screen.getByText('VibeDev'));
		expect(gotoMock).toHaveBeenCalledWith('/vibe');
	});

	it('routes mixed-case VibeDev session and thread to /vibe', async () => {
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			if (url.pathname.endsWith('/chat/sessions')) {
				return new Response(JSON.stringify({
					sessions: [{
						id: 'vibe-sess-mixed',
						title: 'E-commerce store',
						status: 'active',
						ui_thread_id: 'VibeDev',
						updated_at: new Date().toISOString()
					}],
					total: 1, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			if (url.pathname.endsWith('/ui-threads')) {
				return new Response(JSON.stringify({
					threads: [{
						id: 'VIBEDEV',
						name: 'VibeDev',
						archived: false,
						updated_at: new Date().toISOString()
					}],
					total: 1, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			return new Response('not found', { status: 404 });
		}));

		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });

		expect(await screen.findByText('E-commerce store')).toBeInTheDocument();
		await user.click(screen.getByText('E-commerce store'));
		expect(gotoMock).toHaveBeenCalledWith('/vibe?session=vibe-sess-mixed');

		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		expect(await screen.findByText('VibeDev')).toBeInTheDocument();
		await user.click(screen.getByText('VibeDev'));
		expect(gotoMock).toHaveBeenCalledWith('/vibe');
	});
});
