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
});
