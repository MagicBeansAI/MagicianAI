import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

import { chatStore } from '$lib/stores/chatStore';
import {
	historyDrawerInitialTab,
	historyDrawerOpen,
	historyDrawerThreadFilter,
	openHistoryDrawer
} from './shellState';
import HistoryDrawer from './HistoryDrawer.svelte';

const { gotoMock } = vi.hoisted(() => ({
	gotoMock: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$app/navigation', () => ({ goto: gotoMock }));

afterEach(() => {
	cleanup();
	localStorage.clear();
	vi.clearAllMocks();
	vi.unstubAllGlobals();
	historyDrawerOpen.set(false);
	historyDrawerThreadFilter.set(null);
	historyDrawerInitialTab.set('sessions');
});

describe('HistoryDrawer session navigation', () => {
	it.each(['general', 'vibedev'])('lets the HUD own %s history without replacing its page', async (thread) => {
		const session = {
			id: 'hud-selected', principal: 'anonymous', workspace: 'default', agent_id: 'personal-assistant',
			ui_thread_id: thread, title: 'Selected from HUD', origin_channel: { channel_type: 'web' },
			status: 'active', is_default_session: false, created_at: 1, updated_at: 2
		};
		vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ sessions: [session], total: 1, limit: 15, offset: 0 }), {
			status: 200, headers: { 'Content-Type': 'application/json' }
		})));
		const onSelectSession = vi.fn();
		const onSelectCodingSession = vi.fn();
		render(HistoryDrawer, { open: true, onSelectSession, onSelectCodingSession });
		await fireEvent.click(await screen.findByText('Selected from HUD'));
		expect(thread === 'vibedev' ? onSelectCodingSession : onSelectSession).toHaveBeenCalledWith(expect.objectContaining({ id: 'hud-selected', ui_thread_id: thread }));
		expect(gotoMock).not.toHaveBeenCalled();
	});

	it('clears a stale thread scope when a global history opener is used', () => {
		historyDrawerThreadFilter.set('screens');
		historyDrawerInitialTab.set('threads');

		openHistoryDrawer();

		expect(get(historyDrawerOpen)).toBe(true);
		expect(get(historyDrawerThreadFilter)).toBeNull();
		expect(get(historyDrawerInitialTab)).toBe('sessions');
	});

	it('replaces a thread-scoped label with the canonical all-history scope while searching', async () => {
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			if (url.pathname.endsWith('/history/search')) {
				return new Response(JSON.stringify({ items: [], total: 0, limit: 15, offset: 0 }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			return new Response(JSON.stringify({ sessions: [], total: 0, limit: 15, offset: 0 }), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			});
		}));
		const user = userEvent.setup();
		render(HistoryDrawer, { open: true, threadFilter: 'screens' });
		expect(await screen.findByText('#screens', { selector: '.scope-label-name' })).toBeInTheDocument();

		await user.type(screen.getByRole('searchbox', { name: 'Search all history' }), 'capture');

		expect(await screen.findByLabelText('Searching all history')).toHaveTextContent('All sessions and threads');
		expect(screen.queryByText('#screens', { selector: '.scope-label-name' })).not.toBeInTheDocument();
	});

	it('clears the selected thread so sessions from other threads appear', async () => {
		historyDrawerThreadFilter.set('screens');
		const requested: URL[] = [];
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			requested.push(url);
			const thread = url.searchParams.get('ui_thread_id');
			const sessions = thread
				? [{
					id: 'in-thread', principal: 'anonymous', workspace: 'default', agent_id: 'personal-assistant',
					ui_thread_id: 'screens', title: 'Only this thread', origin_channel: { channel_type: 'web' },
					status: 'active', is_default_session: false, created_at: 1, updated_at: 2
				}]
				: [{
					id: 'other-thread', principal: 'anonymous', workspace: 'default', agent_id: 'personal-assistant',
					ui_thread_id: 'plans', title: 'From another thread', origin_channel: { channel_type: 'web' },
					status: 'active', is_default_session: false, created_at: 1, updated_at: 3
				}];
			return new Response(JSON.stringify({ sessions, total: 1, limit: 15, offset: 0 }), {
				status: 200, headers: { 'Content-Type': 'application/json' }
			});
		}));
		const user = userEvent.setup();
		render(HistoryDrawer, { open: true, threadFilter: 'screens' });
		expect(await screen.findByText('Only this thread')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.get('ui_thread_id')).toBe('screens');

		await user.click(screen.getByRole('button', { name: 'Show sessions from all threads' }));

		expect(await screen.findByText('From another thread')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Show sessions from all threads' })).not.toBeInTheDocument();
		expect(screen.getByRole('group', { name: 'History source' })).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.has('ui_thread_id')).toBe(false);
		expect(requested.at(-1)!.searchParams.get('history_lane')).toBe('personal');
		expect(get(historyDrawerThreadFilter)).toBeNull();
	});

	it('pages archived sessions and threads from the server behind the Archived switch', async () => {
		const requested: URL[] = [];
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			requested.push(url);
			const archivedOnly = url.searchParams.get('archived') === 'only';
			if (url.pathname.endsWith('/ui-threads')) {
				return new Response(JSON.stringify({
					threads: archivedOnly ? [{ id: 'old-plans', name: 'Old plans', archived: true }] : [],
					total: archivedOnly ? 1 : 0, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			return new Response(JSON.stringify({
				sessions: archivedOnly ? [{
					id: 'shelved', principal: 'anonymous', workspace: 'default', agent_id: 'personal-assistant',
					ui_thread_id: 'general', title: 'Shelved conversation', origin_channel: { channel_type: 'web' },
					status: 'archived', is_default_session: false, created_at: 1, updated_at: 2
				}] : [],
				total: archivedOnly ? 1 : 0, limit: 15, offset: 0
			}), { status: 200, headers: { 'Content-Type': 'application/json' } });
		}));
		const user = userEvent.setup();
		render(HistoryDrawer, { open: true });
		await waitFor(() => expect(requested.some((url) => url.searchParams.get('archived') === 'exclude')).toBe(true));
		expect(screen.getByRole('button', { name: 'Start new chat' })).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Start new chat' }));
		expect(screen.getByRole('menuitem', { name: 'New Session' })).toBeInTheDocument();
		expect(screen.getByRole('menuitem', { name: 'New Thread' })).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Archived' }));
		expect(await screen.findByText('Shelved conversation')).toBeInTheDocument();
		const sessionsRequest = requested.at(-1)!;
		expect(sessionsRequest.pathname).toMatch(/\/chat\/sessions$/);
		expect(sessionsRequest.searchParams.get('archived')).toBe('only');

		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		expect(await screen.findByText('Old plans')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.get('archived')).toBe('only');
	});

	it('carries the exact clicked session ID across routes instead of loading the default active session', async () => {
		window.history.replaceState({}, '', '/tasks');
		localStorage.setItem('chat_principal', 'anonymous');
		localStorage.setItem('chat_channel_address', 'history-drawer-test');

		const fetchMock = vi.fn(async (request: string | URL | Request) => {
			const url = String(request);
			if (url.includes('/chat/enroll/status?')) {
				return new Response(JSON.stringify({ enrolled: true, principal: 'anonymous' }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			if (url.includes('/chat/sessions?')) {
				return new Response(JSON.stringify({ sessions: [{
					id: 'clicked-active',
					principal: 'anonymous',
					workspace: 'default',
					agent_id: 'personal-assistant',
					ui_thread_id: 'general',
					title: 'Clicked conversation',
					origin_channel: { channel_type: 'web' },
					status: 'active',
					is_default_session: true,
					created_at: 1,
					updated_at: 2
				}] }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			return new Response('not found', { status: 404 });
		});
		vi.stubGlobal('fetch', fetchMock);
		await chatStore.loadSessions(null);

		const user = userEvent.setup();
		render(HistoryDrawer, { open: true });
		expect(await screen.findByText('Clicked conversation')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Archive session' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Delete session' })).not.toBeInTheDocument();
		await user.click(await screen.findByText('Clicked conversation'));

		await waitFor(() => {
			expect(gotoMock).toHaveBeenCalledWith(
				'/t/general/chat?session=clicked-active',
				{ keepFocus: true, noScroll: true }
			);
		});
		expect(
			fetchMock.mock.calls.some(([request]) => String(request).includes('/chat/sessions/clicked-active?'))
		).toBe(false);
	});

	it('filters and pages sessions through the server contract', async () => {
		localStorage.setItem('chat_principal', 'anonymous');
		const requests: URL[] = [];
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			requests.push(url);
			if (url.pathname.endsWith('/history/search')) {
				return new Response(JSON.stringify({
					items: [
						{
							kind: 'session',
							history_lane: 'automated',
							session: {
								id: 'screen-match', principal: 'anonymous', workspace: 'default',
								agent_id: 'personal-assistant', ui_thread_id: 'screens',
								title: 'Screen match', origin_channel: { channel_type: 'web' },
								status: 'active', history_lane: 'automated', created_at: 1, updated_at: 4
							}
						},
						{
							kind: 'thread',
							history_lane: 'personal',
							thread: {
								principal: 'anonymous', workspace: 'default', id: 'screen-notes',
								name: 'Screen notes', archived: false, sort_order: 1,
								created_at: 1, updated_at: 3, history_lane: 'personal'
							}
						}
					],
					total: 2, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			if (!url.pathname.endsWith('/chat/sessions')) return new Response('not found', { status: 404 });
			const lane = url.searchParams.get('history_lane');
			const offset = Number(url.searchParams.get('offset') ?? 0);
			const query = url.searchParams.get('q');
			const title = query
				? 'Screen match'
				: lane === 'automated'
					? 'Automated run'
					: offset > 0
						? 'Personal page two'
						: 'Personal page one';
			return new Response(JSON.stringify({
				sessions: [{
					id: title.toLowerCase().replaceAll(' ', '-'),
					principal: 'anonymous',
					workspace: 'default',
					agent_id: 'personal-assistant',
					ui_thread_id: lane === 'automated' ? 'screens' : 'general',
					title,
					origin_channel: { channel_type: 'web' },
					status: 'active',
					history_lane: lane ?? 'personal',
					created_at: 1,
					updated_at: 2
				}],
				total: lane === 'personal' && !query ? 16 : 1,
				limit: 15,
				offset
			}), { status: 200, headers: { 'Content-Type': 'application/json' } });
		}));

		const user = userEvent.setup();
		render(HistoryDrawer, { open: true });
		expect(await screen.findByText('Personal page one')).toBeInTheDocument();
		await user.click(screen.getByRole('button', { name: 'Next page' }));
		expect(await screen.findByText('Personal page two')).toBeInTheDocument();
		await user.click(screen.getByRole('button', { name: 'Automated' }));
		expect(await screen.findByText('Automated run')).toBeInTheDocument();
		await user.type(screen.getByRole('searchbox', { name: 'Search all history' }), 'screen');
		expect(await screen.findByText('Screen match')).toBeInTheDocument();
		expect(await screen.findByText('Screen notes')).toBeInTheDocument();
		expect(screen.getByText('Session')).toBeInTheDocument();
		expect(screen.getByText('Thread')).toBeInTheDocument();
		expect(screen.getByText('Personal')).toBeInTheDocument();
		expect(screen.getByText('Automated')).toBeInTheDocument();
		expect(screen.getByLabelText('Searching all history')).toHaveTextContent('All sessions and threads');
		expect(screen.getByLabelText('Searching all history')).toHaveTextContent('Personal + Automated');
		expect(screen.queryByRole('tab', { name: 'Sessions' })).not.toBeInTheDocument();

		const searchBox = screen.getByRole('searchbox', { name: 'Search all history' });
		await fireEvent.input(searchBox, { target: { value: 'x'.repeat(125) } });
		await waitFor(() => {
			const boundedRequests = requests.filter((url) =>
				url.pathname.endsWith('/history/search') && url.searchParams.get('q')?.startsWith('x')
			);
			const boundedRequest = boundedRequests[boundedRequests.length - 1];
			expect(boundedRequest?.searchParams.get('q')).toHaveLength(120);
		});
		expect(searchBox).toHaveValue('x'.repeat(120));

		expect(requests.some((url) =>
			url.searchParams.get('history_lane') === 'personal'
			&& url.searchParams.get('limit') === '15'
			&& url.searchParams.get('offset') === '15'
		)).toBe(true);
		expect(requests.some((url) =>
			url.pathname.endsWith('/history/search')
			&& url.searchParams.get('history_lane') === null
			&& url.searchParams.get('q') === 'screen'
		)).toBe(true);
	});

	it('uses the same lane and search controls for threads', async () => {
		const requests: URL[] = [];
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			requests.push(url);
			if (url.pathname.endsWith('/chat/sessions')) {
				return new Response(JSON.stringify({ sessions: [], total: 0, limit: 15, offset: 0 }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			}
			if (url.pathname.endsWith('/ui-threads')) {
				const automated = url.searchParams.get('history_lane') === 'automated';
				return new Response(JSON.stringify({
					threads: [{
						principal: 'anonymous', workspace: 'default',
						id: automated ? 'tabs' : 'general',
						name: automated ? 'Observed tabs' : 'general',
						archived: false, sort_order: 0, created_at: 1, updated_at: 2,
						history_lane: automated ? 'automated' : 'personal'
					}],
					total: 1, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			return new Response('not found', { status: 404 });
		}));

		const user = userEvent.setup();
		render(HistoryDrawer, { open: true });
		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		expect(await screen.findByText('general')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Archive thread' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Delete thread' })).not.toBeInTheDocument();
		await user.click(screen.getByRole('button', { name: 'Automated' }));
		expect(await screen.findByText('Observed tabs')).toBeInTheDocument();
		expect(screen.getByText('Observed tabs')).toHaveAttribute('title', 'Observed tabs');
		expect(screen.getByRole('button', { name: 'Archive thread' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Delete thread' })).toBeInTheDocument();
		expect(requests.some((url) =>
			url.pathname.endsWith('/ui-threads')
			&& url.searchParams.get('history_lane') === 'automated'
		)).toBe(true);
	});

	it('routes vibedev session click to /vibe?session=...', async () => {
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			if (url.pathname.endsWith('/chat/sessions')) {
				return new Response(JSON.stringify({
					sessions: [{
						id: 'vibe-sess-1',
						principal: 'anonymous',
						workspace: 'default',
						agent_id: 'coding-agent',
						ui_thread_id: 'vibedev',
						title: 'Landing page build',
						origin_channel: { channel_type: 'web' },
						status: 'active',
						created_at: 1,
						updated_at: 2
					}],
					total: 1, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			return new Response('not found', { status: 404 });
		}));

		const user = userEvent.setup();
		render(HistoryDrawer, { open: true });

		expect(await screen.findByText('Landing page build')).toBeInTheDocument();
		await user.click(screen.getByText('Landing page build'));
		expect(gotoMock).toHaveBeenCalledWith('/vibe?session=vibe-sess-1');
	});

	it('routes vibedev thread click to /vibe', async () => {
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			if (url.pathname.endsWith('/chat/sessions')) {
				return new Response(JSON.stringify({ sessions: [], total: 0, limit: 15, offset: 0 }), {
					status: 200, headers: { 'Content-Type': 'application/json' }
				});
			}
			if (url.pathname.endsWith('/ui-threads')) {
				return new Response(JSON.stringify({
					threads: [{
						principal: 'anonymous',
						workspace: 'default',
						id: 'vibedev',
						name: 'VibeDev',
						archived: false,
						sort_order: 0,
						created_at: 1,
						updated_at: 2
					}],
					total: 1, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			return new Response('not found', { status: 404 });
		}));

		const user = userEvent.setup();
		render(HistoryDrawer, { open: true });

		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		expect(await screen.findByText('VibeDev')).toBeInTheDocument();
		await user.click(screen.getByText('VibeDev'));
		expect(gotoMock).toHaveBeenCalledWith('/vibe');
	});
});
