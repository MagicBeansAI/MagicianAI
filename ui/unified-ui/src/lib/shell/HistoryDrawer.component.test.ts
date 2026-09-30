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
		expect(await screen.findByText('#screens')).toBeInTheDocument();

		await user.type(screen.getByRole('searchbox', { name: 'Search all history' }), 'capture');

		expect(await screen.findByLabelText('Searching all history')).toHaveTextContent('All sessions and threads');
		expect(screen.queryByText('#screens')).not.toBeInTheDocument();
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
		expect(requests.some((url) =>
			url.pathname.endsWith('/ui-threads')
			&& url.searchParams.get('history_lane') === 'automated'
		)).toBe(true);
	});
});
