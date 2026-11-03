import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import ChatHistoryRail from './ChatHistoryRail.svelte';
import { chatHistoryReopenOnSessions, historyPickedThread } from './shellState';
import { confirmationStore, resolveConfirmation } from '$lib/stores/confirmationStore';
import { threadLabels } from '$lib/threads/threadLinks';

const { gotoMock } = vi.hoisted(() => ({
	gotoMock: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$app/navigation', () => ({ goto: gotoMock }));

afterEach(() => {
	cleanup();
	localStorage.clear();
	vi.clearAllMocks();
	vi.unstubAllGlobals();
	historyPickedThread.set(null);
	chatHistoryReopenOnSessions.set(null);
});

const PICKABLE_THREADS = [
	{ id: 'plans', name: 'Plans', archived: false, updated_at: 1 },
	{ id: 'general', name: 'general', archived: false, updated_at: 1 }
];
const NAV = { keepFocus: true, noScroll: true };

function sessionIn(thread: string | null) {
	return {
		id: `in-${thread ?? 'all'}`, principal: 'anonymous', workspace: 'default', agent_id: 'personal-assistant',
		ui_thread_id: thread ?? 'general', title: thread ? `Session in ${thread}` : 'From every thread',
		origin_channel: { channel_type: 'web' }, status: 'active', is_default_session: false,
		created_at: 1, updated_at: 2
	};
}

/**
 * Threads, a thread search hit, sessions that name the thread they were asked
 * for, and the enrolment, `chat/new` and thread DELETE a New Session or a
 * delete needs. Returns the `/chat/sessions` requests; `created` collects
 * `chat/new` requests.
 */
function stubThreadPicking(created: URL[] = []): URL[] {
	const requested: URL[] = [];
	const json = (body: unknown) => new Response(JSON.stringify(body), {
		status: 200, headers: { 'Content-Type': 'application/json' }
	});
	vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
		const url = new URL(String(request), 'http://localhost');
		const method = (init?.method ?? 'GET').toUpperCase();
		if (url.pathname.endsWith('/chat/activity')) return json({ working_session_ids: [] });
		if (url.pathname.endsWith('/chat/enroll/status')) return json({ enrolled: true, principal: 'anonymous' });
		if (url.pathname.endsWith('/chat/new')) {
			created.push(url);
			return json({ session: { ...sessionIn(url.searchParams.get('ui_thread_id')), id: 'brand-new' } });
		}
		if (method === 'DELETE') return json({});
		if (url.pathname.endsWith('/ui-threads')) {
			return json({ threads: PICKABLE_THREADS, total: 2, limit: 15, offset: 0 });
		}
		if (url.pathname.endsWith('/history/search')) {
			return json({
				items: [{ kind: 'thread', history_lane: 'personal', thread: PICKABLE_THREADS[0] }],
				total: 1, limit: 15, offset: 0
			});
		}
		if (!url.pathname.endsWith('/chat/sessions')) return new Response('not found', { status: 404 });
		requested.push(url);
		return json({ sessions: [sessionIn(url.searchParams.get('ui_thread_id'))], total: 1, limit: 15, offset: 0 });
	}));
	return requested;
}

describe('ChatHistoryRail component', () => {
	it('marks background work and clears the badge when the server reports completion', async () => {
		let working = true;
		vi.stubGlobal('fetch', vi.fn(async (input: string | URL | Request) => {
			const activity = String(input).includes('/chat/activity?');
			return new Response(JSON.stringify(activity
				? { working_session_ids: working ? ['background', 'not-on-this-page'] : [] }
				: { sessions: [{ id: 'background', title: 'Background research', status: 'active', ui_thread_id: 'general', updated_at: Date.now() }], total: 1, limit: 15, offset: 0 }),
				{ status: 200, headers: { 'Content-Type': 'application/json' } });
		}));
		render(ChatHistoryRail, { expanded: true });
		expect(await screen.findByLabelText('Session is working')).toBeInTheDocument();
		expect(screen.getAllByText('Working')).toHaveLength(1);
		working = false;
		await waitFor(() => expect(screen.queryByLabelText('Session is working')).toBeNull(), { timeout: 4500 });
		expect(screen.getByText('Background research')).toBeInTheDocument();
	}, 6500);
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

	it('clears the selected thread so sessions from other threads appear', async () => {
		const requested: URL[] = [];
		vi.stubGlobal('fetch', vi.fn(async (request: string | URL | Request) => {
			const url = new URL(String(request), 'http://localhost');
			if (url.pathname.endsWith('/chat/sessions')) requested.push(url);
			const thread = url.searchParams.get('ui_thread_id');
			const sessions = thread
				? [{ id: 'in-thread', title: 'Only this thread', status: 'active', ui_thread_id: 'screens', updated_at: 2 }]
				: [{ id: 'other-thread', title: 'From another thread', status: 'active', ui_thread_id: 'plans', updated_at: 3 }];
			return new Response(JSON.stringify({ sessions, total: 1, limit: 15, offset: 0 }), {
				status: 200, headers: { 'Content-Type': 'application/json' }
			});
		}));
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true, threadFilter: 'screens' });
		expect(await screen.findByText('Only this thread')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.get('ui_thread_id')).toBe('screens');

		await user.click(screen.getByRole('button', { name: 'Show sessions from all threads' }));

		expect(await screen.findByText('From another thread')).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Show sessions from all threads' })).not.toBeInTheDocument();
		expect(screen.getByRole('group', { name: 'History source' })).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.has('ui_thread_id')).toBe(false);
		expect(requested.at(-1)!.searchParams.get('history_lane')).toBe('personal');
	});

	it('shows archive and delete on a thread, keeps a long name on one line, and says Archiving while it saves', async () => {
		const longName = 'Quarterly planning thread for the whole product family and every follow-up';
		let releasePatch: (response: Response) => void = () => {};
		const patch = new Promise<Response>((resolve) => {
			releasePatch = resolve;
		});
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
			const url = String(input instanceof Request ? input.url : input);
			const method = (init?.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase();
			if (method === 'PATCH') return patch;
			if (url.includes('/ui-threads')) {
				return new Response(JSON.stringify({
					threads: [
						{ id: 'general', name: 'general', archived: false, updated_at: 1 },
						{ id: 'plans', name: longName, archived: false, updated_at: 2 }
					],
					total: 2, limit: 15, offset: 0
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			if (url.includes('/chat/activity')) {
				return new Response(JSON.stringify({ working_session_ids: [] }), {
					status: 200, headers: { 'Content-Type': 'application/json' }
				});
			}
			return new Response(JSON.stringify({ sessions: [], total: 0, limit: 15, offset: 0 }), {
				status: 200, headers: { 'Content-Type': 'application/json' }
			});
		}));

		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });
		await user.click(screen.getByRole('tab', { name: 'Threads' }));

		expect(await screen.findByText('general')).toBeInTheDocument();
		const generalRow = screen.getByText('general').closest('.thread-item');
		expect(generalRow?.querySelector('[aria-label="Archive thread"]')).toBeNull();
		expect(generalRow?.querySelector('[aria-label="Delete thread"]')).toBeNull();

		const title = screen.getByText(longName);
		expect(title).toHaveAttribute('title', longName);
		expect(title).toHaveClass('title');
		expect(screen.getByRole('button', { name: 'Archive thread' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Delete thread' })).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Archive thread' }));
		expect(await screen.findByRole('status')).toHaveTextContent('Archiving…');
		expect(screen.getByRole('button', { name: 'Archive thread' })).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Delete thread' })).toBeDisabled();

		releasePatch(new Response(JSON.stringify({
			thread: {
				principal: 'anonymous', workspace: 'default', id: 'plans', name: longName,
				archived: true, sort_order: 0, created_at: 1, updated_at: 3
			}
		}), { status: 200, headers: { 'Content-Type': 'application/json' } }));
		await waitFor(() => expect(screen.queryByText('Archiving…')).not.toBeInTheDocument());
	});

	it('a thread tap shows that thread\'s sessions', async () => {
		const requested = stubThreadPicking();
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });
		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByText('Plans'));

		expect(gotoMock).toHaveBeenCalledWith('/t/plans', NAV);
		expect(screen.getByRole('tab', { name: 'Sessions' })).toHaveAttribute('aria-selected', 'true');
		expect(await screen.findByText('Session in plans')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.get('ui_thread_id')).toBe('plans');
		expect(requested.at(-1)!.searchParams.has('history_lane')).toBe(false);
		// threadStore is shared across tests, so ask it for the label it shows.
		expect(document.querySelector('.scope-label-name')).toHaveTextContent(`#${get(threadLabels)('plans')}`);
		expect(screen.getByRole('button', { name: 'Show sessions from all threads' })).toBeInTheDocument();
		expect(screen.queryByRole('group', { name: 'History source' })).not.toBeInTheDocument();
		expect(get(historyPickedThread)).toBe('plans');
		// The route kept this rail mounted, so it withdraws its own reopen request.
		await waitFor(() => expect(get(chatHistoryReopenOnSessions)).toBeNull());
	});

	it('leaves the reopen request for the rail that replaces it', async () => {
		stubThreadPicking();
		const user = userEvent.setup();
		const { unmount } = render(ChatHistoryRail, { expanded: true });
		gotoMock.mockImplementationOnce(() => new Promise((resolve) => setTimeout(() => {
			unmount();
			resolve(undefined);
		}, 0)));
		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByText('Plans'));

		await waitFor(() => expect(gotoMock).toHaveBeenCalledWith('/t/plans', NAV));
		await new Promise((resolve) => setTimeout(resolve, 20));
		expect(get(chatHistoryReopenOnSessions)).toEqual(expect.any(Number));
	});

	it('tapping #general filters to #general', async () => {
		const requested = stubThreadPicking();
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });
		expect(await screen.findByText('From every thread')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.has('ui_thread_id')).toBe(false);

		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByText('general'));

		expect(gotoMock).toHaveBeenCalledWith('/t/general', NAV);
		expect(await screen.findByText('Session in general')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.get('ui_thread_id')).toBe('general');
		expect(screen.getByText('#general', { selector: '.scope-label-name' })).toBeInTheDocument();
	});

	it('× clears a picked thread and a later tap filters again', async () => {
		const requested = stubThreadPicking();
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });
		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByText('Plans'));
		expect(await screen.findByText('Session in plans')).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Show sessions from all threads' }));

		expect(await screen.findByText('From every thread')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.has('ui_thread_id')).toBe(false);
		expect(requested.at(-1)!.searchParams.get('history_lane')).toBe('personal');
		expect(screen.getByRole('group', { name: 'History source' })).toBeInTheDocument();
		expect(get(historyPickedThread)).toBeNull();

		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByText('Plans'));
		expect(await screen.findByText('Session in plans')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.get('ui_thread_id')).toBe('plans');
	});

	it('a thread tap keeps the phone panel open', async () => {
		vi.stubGlobal('innerWidth', 800);
		stubThreadPicking();
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true, mobileOpen: true });
		expect(document.querySelector('.chat-history-backdrop')).not.toBeNull();
		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByText('Plans'));

		expect(gotoMock).toHaveBeenCalledWith('/t/plans', NAV);
		expect(await screen.findByText('Session in plans')).toBeInTheDocument();
		expect(document.querySelector('.chat-history-backdrop')).not.toBeNull();
		expect(screen.getByRole('tab', { name: 'Sessions' })).toHaveAttribute('aria-selected', 'true');
	});

	it('a rail that mounts after the tap\'s navigation reopens on Sessions', async () => {
		vi.stubGlobal('innerWidth', 800);
		stubThreadPicking();
		historyPickedThread.set('plans');
		// goto resolved a few seconds ago; the thread layout only now rendered the chat.
		chatHistoryReopenOnSessions.set(Date.now() - 3_000);
		render(ChatHistoryRail, { expanded: true, mobileOpen: false, initialTab: 'threads' });

		await waitFor(() => expect(document.querySelector('.chat-history-backdrop')).not.toBeNull());
		expect(screen.getByRole('tab', { name: 'Sessions' })).toHaveAttribute('aria-selected', 'true');
		expect(get(chatHistoryReopenOnSessions)).toBeNull();
		expect(await screen.findByText('Session in plans')).toBeInTheDocument();
	});

	it('ignores a reopen request older than ten seconds', async () => {
		vi.stubGlobal('innerWidth', 800);
		stubThreadPicking();
		chatHistoryReopenOnSessions.set(Date.now() - 11_000);
		render(ChatHistoryRail, { expanded: true, mobileOpen: false, initialTab: 'threads' });

		await waitFor(() => expect(get(chatHistoryReopenOnSessions)).toBeNull());
		expect(screen.getByRole('tab', { name: 'Threads' })).toHaveAttribute('aria-selected', 'true');
		expect(document.querySelector('.chat-history-backdrop')).toBeNull();
	});

	it('on a desktop a remounted rail switches to Sessions without the phone panel', async () => {
		vi.stubGlobal('innerWidth', 1600);
		stubThreadPicking();
		chatHistoryReopenOnSessions.set(Date.now());
		render(ChatHistoryRail, { expanded: true, mobileOpen: false, initialTab: 'threads' });

		await waitFor(() => expect(get(chatHistoryReopenOnSessions)).toBeNull());
		expect(screen.getByRole('tab', { name: 'Sessions' })).toHaveAttribute('aria-selected', 'true');
		expect(document.querySelector('.chat-history-backdrop')).toBeNull();
	});

	it('a thread hit in search results behaves like a Threads-tab tap', async () => {
		const requested = stubThreadPicking();
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });
		const searchBox = screen.getByRole('searchbox', { name: 'Search history' });
		await user.type(searchBox, 'plan');
		await user.click(await screen.findByText('Plans'));

		expect(gotoMock).toHaveBeenCalledWith('/t/plans', NAV);
		expect(searchBox).toHaveValue('');
		expect(screen.queryByLabelText('Searching all history')).not.toBeInTheDocument();
		expect(screen.getByRole('tab', { name: 'Sessions' })).toHaveAttribute('aria-selected', 'true');
		expect(await screen.findByText('Session in plans')).toBeInTheDocument();
		expect(requested.at(-1)!.searchParams.get('ui_thread_id')).toBe('plans');
		expect(requested.at(-1)!.searchParams.has('q')).toBe(false);
	});

	it('New Session in a filtered list creates in the picked thread and opens it', async () => {
		localStorage.setItem('chat_principal', 'anonymous');
		const created: URL[] = [];
		stubThreadPicking(created);
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true, threadFilter: 'screens' });
		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByText('Plans'));
		expect(await screen.findByText('Session in plans')).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Start new chat' }));
		await user.click(screen.getByRole('menuitem', { name: 'New Session' }));

		await waitFor(() => expect(gotoMock).toHaveBeenCalledTimes(2));
		expect(created).toHaveLength(1);
		expect(created[0].searchParams.get('ui_thread_id')).toBe('plans');
		expect(gotoMock).toHaveBeenLastCalledWith('/t/plans', NAV);
	});

	it('New Session with nothing picked creates in the route\'s thread and stays put', async () => {
		localStorage.setItem('chat_principal', 'anonymous');
		const created: URL[] = [];
		stubThreadPicking(created);
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true, threadFilter: 'screens' });
		expect(await screen.findByText('Session in screens')).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Start new chat' }));
		await user.click(screen.getByRole('menuitem', { name: 'New Session' }));

		await waitFor(() => expect(created).toHaveLength(1));
		expect(created[0].searchParams.get('ui_thread_id')).toBe('screens');
		expect(gotoMock).not.toHaveBeenCalled();
	});

	it('deleting the picked thread clears the pick', async () => {
		localStorage.setItem('chat_principal', 'anonymous');
		stubThreadPicking();
		const user = userEvent.setup();
		render(ChatHistoryRail, { expanded: true });
		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByText('Plans'));
		expect(get(historyPickedThread)).toBe('plans');

		await user.click(screen.getByRole('tab', { name: 'Threads' }));
		await user.click(await screen.findByRole('button', { name: 'Delete thread' }));
		await waitFor(() => expect(get(confirmationStore).request).not.toBeNull());
		resolveConfirmation(true);

		await waitFor(() => expect(get(historyPickedThread)).toBeNull());
	});
});
