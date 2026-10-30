// Mounts ChatPanel with a stubbed chatStore to prove the chat-next wiring:
// flag on → LiveTaskCard (expands in place on click), flag off → classic
// TaskStatusCard. The other ChatPanel tests are source-contract tests; this
// is the first that mounts the panel, so every collaborator it touches on
// mount is stubbed to an inert async no-op.
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { page } from '$app/stores';
import { desktopComposerDrafts } from './desktopComposerDrafts';
import { historyDrawerOpen } from '$lib/shell/shellState';

import type { ChatMessage } from '$lib/stores/chatStore';
import { taskStore } from '$lib/stores/taskStore';
import * as notifications from '$lib/shared/stores/notifications';
import { setChatNext } from './next/chatNextFlag';
import ChatPanel from './ChatPanel.svelte';

const { chatState } = vi.hoisted(() => ({ chatState: {
	current: null as unknown,
	nativeMode: false,
	before: null as null | ((event: { from: { url: URL }; to: { url: URL } }) => void),
	loadOlderMessages: vi.fn().mockResolvedValue(undefined),
	sendMessageStreaming: vi.fn().mockResolvedValue(null)
} }));

vi.mock('$app/stores', async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});

vi.mock('$app/navigation', () => ({
	beforeNavigate: (callback: NonNullable<typeof chatState.before>) => { chatState.before = callback; },
	goto: vi.fn().mockResolvedValue(undefined),
	replaceState: vi.fn()
}));

vi.mock('$lib/shell/hudWindow', () => ({ isHudWindow: () => chatState.nativeMode }));

vi.mock('$lib/stores/chatStore', async (importActual) => {
	const actual = await importActual<typeof import('$lib/stores/chatStore')>();
	const { writable } = await import('svelte/store');
	const state = writable({
		activeSessionId: 'session-1',
		sessions: [],
		messages: [] as ChatMessage[],
		isLoading: false,
		isLoadingOlder: false,
		hasMoreMessages: false,
		isSendingMessage: false,
		bubbleOpen: false,
		viewingArchivedId: null,
		error: null
	});
	chatState.current = state;
	// Every store method becomes an inert async no-op; only the state is real.
	const chatStore = new Proxy(
		{ subscribe: state.subscribe, listQueuedMessages: vi.fn().mockResolvedValue([]), sendMessageStreaming: chatState.sendMessageStreaming, loadOlderMessages: chatState.loadOlderMessages },
		{
			get: (target, key) =>
				key in target ? target[key as keyof typeof target] : vi.fn().mockResolvedValue(null)
		}
	);
	return { ...actual, chatStore };
});

type ChatStateStore = { update: (fn: (s: Record<string, unknown>) => Record<string, unknown>) => void };

function setMessages(messages: ChatMessage[]): void {
	(chatState.current as ChatStateStore).update((s) => ({ ...s, messages }));
}

// A user turn, the assistant's reply, then a background task update. With
// the reply first, both modes render the card standalone (classic folds an
// update that lands before the reply into the turn; see the in-flight
// describe block at the end).
function transcript(): ChatMessage[] {
	const t0 = Date.now() - 3_000;
	return [
		{
			id: 'user-1',
			session_id: 'session-1',
			direction: 'user',
			created_at: t0,
			content: { type: 'text', text: 'Review the release' }
		},
		{
			id: 'assistant-1',
			session_id: 'session-1',
			direction: 'assistant',
			created_at: t0 + 1_000,
			content: { type: 'text', text: 'Started a task for that.' }
		},
		{
			id: 'status-1',
			session_id: 'session-1',
			direction: 'system',
			created_at: t0 + 2_000,
			content: {
				type: 'task_status_update',
				task_id: 'task-1',
				display_label: 'Review release',
				status: 'running',
				execution_id: 'exec-1',
				summary: 'Checking the release gates.'
			}
		}
	] as unknown as ChatMessage[];
}

beforeEach(() => {
	historyDrawerOpen.set(false);
	chatState.nativeMode = false;
	desktopComposerDrafts.clear();
	get(page).url.pathname = '/chat';
	get(page).url.search = '';
	chatState.sendMessageStreaming.mockClear();
	chatState.loadOlderMessages.mockReset().mockResolvedValue(undefined);
	// jsdom has no scrolling; ChatPanel scrolls the transcript on load.
	Element.prototype.scrollTo = vi.fn();
	// Svelte cancels incoming message animations when switching transcripts.
	if (!Element.prototype.getAnimations) {
		Object.defineProperty(Element.prototype, 'getAnimations', { configurable: true, value: () => [] });
	}
	vi.stubGlobal(
		'fetch',
		vi.fn(async () =>
			new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json' } })
		)
	);
	setMessages([]);
	(chatState.current as ChatStateStore).update((s) => ({ ...s, activeSessionId: 'session-1', sessions: [], viewingArchivedId: null, isLoading: false, isLoadingOlder: false, hasMoreMessages: false, focusedMessageId: null }));
});

afterEach(() => {
	cleanup();
	setChatNext(false);
	localStorage.clear();
	sessionStorage.clear();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('ChatPanel chat-next wiring', () => {
	it('shows the thread bar on default chat with Files beside Settings', async () => {
		get(page).url.search = '?session=session-1';
		render(ChatPanel);
		const bar = within(screen.getByRole('navigation', { name: 'Thread workspace' }));
		expect(bar.getByText('#general')).toBeInTheDocument();
		expect(bar.getByRole('link', { name: 'Chat' })).toHaveAttribute('href', '/chat?session=session-1');
		expect(bar.getByRole('link', { name: /^Tasks/ })).toHaveAttribute('href', '/work?thread=general');
		const settings = bar.getByRole('link', { name: 'Settings' });
		expect(settings).toHaveAttribute('href', '/t/general/settings?session=session-1');
		expect(settings.nextElementSibling).toBe(bar.getByRole('link', { name: 'Files' }));
		expect(bar.getByRole('link', { name: 'Files' })).toHaveAttribute('href', '/chat/files?session=session-1&thread=general');
		expect(bar.getByRole('link', { name: 'Models' })).toHaveAttribute('href', '/chat/models?session=session-1&thread=general');
		expect(bar.queryByLabelText('Model for next message')).not.toBeInTheDocument();
		expect(bar.getByLabelText('Total chat usage')).toBeInTheDocument();
		expect(bar.queryByRole('button', { name: 'Files' })).not.toBeInTheDocument();
	});

	it('uses the viewed archived session for files and cumulative usage', async () => {
		get(page).url.pathname = '/t/research/chat';
		get(page).url.search = '?session=archived-session';
		(chatState.current as ChatStateStore).update((s) => ({ ...s,
			viewingArchivedId: 'archived-session',
			sessions: [{ id: 'archived-session', title: 'Earlier research', ui_thread_id: 'research' }]
		}));
		render(ChatPanel, { props: { threadId: 'research' } });
		const bar = within(screen.getByRole('navigation', { name: 'Thread workspace' }));
		expect(bar.getByText('#research')).toBeInTheDocument();
		expect(bar.getByRole('link', { name: 'Settings' })).toHaveAttribute('href', '/t/research/settings?session=archived-session');
        expect(bar.getByRole('link', { name: 'Files' })).toHaveAttribute('href', '/t/research/files?session=archived-session');
        expect(bar.getByRole('link', { name: 'Models' })).toHaveAttribute('href', '/t/research/models?session=archived-session');
        await waitFor(() => expect(vi.mocked(fetch).mock.calls.some(([url, init]) => String(url).endsWith('/analytics/llm_calls/query') && String(init?.body).includes("chat_session_id = 'archived-session'"))).toBe(true));
	});

	it('moves the verification reason into a message-header popup and preserves the answer figures', async () => {
		render(ChatPanel);
		const messages = transcript();
		messages[1].content = { type: 'text', text: "> ⚠️ **Not fully verified.** Magician's evidence check could not confirm part of this answer: figure 93217, 45330 not found in any tool result\n\nContact: +91 93217 45330." };
		setMessages(messages);
		await waitFor(() => expect(screen.getByText('Contact: +91 93217 45330.')).toBeInTheDocument());
		expect(screen.queryByText(/figure 93217/)).not.toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Not fully verified — view details' }));
		expect(screen.getByRole('dialog', { name: 'Answer verification' })).toHaveTextContent('figure 93217, 45330 not found in any tool result');
	});

	it('restores switch-carried text as an editable draft without sending it', async () => {
		get(page).url.search = '?draft=Fix%20layout%20%26%20spacing&q=do-not-send';
		render(ChatPanel);
		await waitFor(() => expect(screen.getByRole('textbox')).toHaveTextContent('Fix layout & spacing'));
		expect(chatState.sendMessageStreaming).not.toHaveBeenCalled();
	});

	it('waits for the chat’s model catalog before auto-sending a ?q= link', async () => {
		const model = { id: 'custom/model', label: 'Custom model', model: 'physical-model', provider: 'custom', connection: 'custom',
			limits: null, capabilities: { vision: true, tools: true, streaming: true, computer_use: false, structured_output: false, web_search: false },
			thinking: { mode: 'effort', levels: ['low'], default: 'low' }, choices: { auto: 'opaque-auto', low: 'opaque-low' } };
		vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => new Response(JSON.stringify(
			String(input).includes('/chat/models') ? { models: [model], default_selection: { model_id: model.id, thinking: 'auto' } } : {}
		), { status: 200, headers: { 'Content-Type': 'application/json' } })));
		get(page).url.search = '?q=Plan%20my%20week';
		render(ChatPanel);
		await waitFor(() => expect(chatState.sendMessageStreaming).toHaveBeenCalledTimes(1));
		const [sessionId, text, , , options] = chatState.sendMessageStreaming.mock.calls[0];
		expect([sessionId, text]).toEqual(['session-1', 'Plan my week']);
		expect(options).toMatchObject({ harnessEngine: 'magician',
			modelRequest: { harness_engine: 'magician', model_selection: { model_id: 'custom/model', thinking: 'auto' } } });
	});

	it('keeps a ?q= link’s words and says why when the model choice cannot be used', async () => {
		const showError = vi.spyOn(notifications, 'showError');
		// The default stub answers the catalog with no models and no default.
		get(page).url.search = '?q=Plan%20my%20week';
		render(ChatPanel);
		await waitFor(() => expect(showError).toHaveBeenCalledWith('Message not sent', expect.any(String)));
		expect(chatState.sendMessageStreaming).not.toHaveBeenCalled();
		await waitFor(() => expect(screen.getByRole('textbox')).toHaveTextContent('Plan my week'));
	});

	it('recovers typed text after reload, isolates chats and respects clearing', async () => {
		const first = render(ChatPanel);
		await waitFor(() => expect(screen.getByRole('textbox')).toBeInTheDocument());
		const editor = screen.getByRole('textbox');
		editor.textContent = 'My unfinished message';
		await fireEvent.input(editor);
		await waitFor(() => expect(Object.keys(sessionStorage).some(key => key.startsWith('magician:composer-draft:'))).toBe(true));
		first.unmount();
		render(ChatPanel);
		await waitFor(() => expect(screen.getByRole('textbox')).toHaveTextContent('My unfinished message'));
		(chatState.current as ChatStateStore).update(s => ({ ...s, activeSessionId: 'session-2' }));
		await waitFor(() => expect(screen.getByRole('textbox')).toBeEmptyDOMElement());
		(chatState.current as ChatStateStore).update(s => ({ ...s, activeSessionId: 'session-1' }));
		await waitFor(() => expect(screen.getByRole('textbox')).toHaveTextContent('My unfinished message'));
		screen.getByRole('textbox').textContent = '';
		await fireEvent.input(screen.getByRole('textbox'));
		await waitFor(() => expect(Object.keys(sessionStorage).filter(key => key.startsWith('magician:composer-draft:'))).toHaveLength(0));
		expect(chatState.sendMessageStreaming).not.toHaveBeenCalled();
	});

	it('hands off an unsent desktop draft and attachments between the app and HUD', async () => {
		chatState.nativeMode = true;
		get(page).url.search = '?draft=Unsent%20desktop%20draft';
		const first = render(ChatPanel, { props: { seedStagedAttachments: [{ session_id: 'session-1', attachment_id: 'attachment-1', filename: 'handoff-note.txt', mime_type: 'text/plain', size: 15 }] } });
		await waitFor(() => expect(screen.getByRole('textbox')).toHaveTextContent('Unsent desktop draft'));
		chatState.before!({ from: { url: new URL('http://localhost/chat') }, to: { url: new URL('http://localhost/hud') } });
		expect([...desktopComposerDrafts.values()][0].composer.attachments).toHaveLength(1);
		first.unmount();
		get(page).url.search = '';
		render(ChatPanel, { props: { hud: true } });
		await waitFor(() => expect(screen.getByRole('textbox')).toHaveTextContent('Unsent desktop draft'));
		expect(screen.getByText('handoff-note.txt')).toBeInTheDocument();
		expect(desktopComposerDrafts.size).toBe(0);
		expect(chatState.sendMessageStreaming).not.toHaveBeenCalled();
	});

	it('keeps page switching out of the compact HUD while preserving its composer', () => {
		render(ChatPanel, { props: { hud: true } });
		expect(screen.queryByRole('radiogroup', { name: 'Talk or Code mode' })).not.toBeInTheDocument();
		expect(screen.getByRole('group', { name: 'Message composer' })).toBeInTheDocument();
	});

	it('opens the HUD-owned session picker instead of the page-navigating chat rail', async () => {
		const { container } = render(ChatPanel, { props: { hud: true } });
		await fireEvent.click(screen.getByRole('button', { name: 'Open history' }));
		expect(get(historyDrawerOpen)).toBe(true);
		expect(container.querySelector('.chat-history-rail')).toBeNull();
	});

	it('keeps Talk and Code available on the full chat page', () => {
		render(ChatPanel);
		expect(screen.getByRole('radiogroup', { name: 'Talk or Code mode' })).toBeInTheDocument();
	});

	it('renders the live task card for a running task when chatNext is on', async () => {
		setChatNext(true);
		const { container } = render(ChatPanel);
		setMessages(transcript());

		await waitFor(() => expect(container.querySelector('.cn-live-task')).not.toBeNull());
		expect(container.querySelector('.chat-status-alert')).toBeNull();
		expect(container.querySelector('.chat-page.chat-next')).not.toBeNull();
	});

	it('opens the transcript with a day divider pill', async () => {
		setChatNext(true);
		const { container } = render(ChatPanel);
		setMessages(transcript());

		await waitFor(() => expect(container.querySelector('.chat-day-divider')).not.toBeNull());
		// One transcript, one day: exactly one divider, on its first message.
		expect(container.querySelectorAll('.chat-day-divider')).toHaveLength(1);
		expect(container.querySelector('.chat-day-float')).not.toBeNull();
	});

	it('keeps the classic TaskStatusCard when chatNext is off', async () => {
		setChatNext(false);
		const { container } = render(ChatPanel);
		setMessages(transcript());

		await waitFor(() => expect(container.querySelector('.chat-status-alert')).not.toBeNull());
		expect(container.querySelector('.cn-live-task')).toBeNull();
		expect(container.querySelector('.chat-next')).toBeNull();
	});

	it('mounts an idle presence orb by the composer when chatNext is on', async () => {
		setChatNext(true);
		const { container } = render(ChatPanel);
		setMessages(transcript());

		await waitFor(() =>
			expect(container.querySelector('[data-testid="presence-orb"]')).not.toBeNull()
		);
		expect(container.querySelector('[data-testid="presence-orb"]')).toHaveAttribute(
			'data-state',
			'idle'
		);
	});

	it('renders no presence orb when chatNext is off', async () => {
		setChatNext(false);
		const { container } = render(ChatPanel);
		setMessages(transcript());

		await waitFor(() => expect(container.querySelector('.chat-status-alert')).not.toBeNull());
		expect(container.querySelector('[data-testid="presence-orb"]')).toBeNull();
	});

	it('renders the next empty state for an empty session when chatNext is on', async () => {
		setChatNext(true);
		const { container } = render(ChatPanel);

		await waitFor(() =>
			expect(container.querySelector('[data-testid="chat-empty-next"]')).not.toBeNull()
		);
		expect(container.querySelector('.chat-empty-state')).toBeNull();
	});

	it('keeps the classic empty state for an empty session when chatNext is off', async () => {
		setChatNext(false);
		const { container } = render(ChatPanel);

		await waitFor(() => expect(container.querySelector('.chat-empty-state')).not.toBeNull());
		expect(container.querySelector('[data-testid="chat-empty-next"]')).toBeNull();
	});

	it('keeps Cancel in the title row and run controls in the inline expansion', async () => {
		setChatNext(true);
		const { container } = render(ChatPanel);
		setMessages(transcript());

		await waitFor(() => expect(container.querySelector('.cn-live-task')).not.toBeNull());
		// The active task's lifecycle controls make expansion useful even before
		// its first activity event. They stay in the card, without a side panel.
		expect(screen.getByRole('button', { name: /Review release.*show details/ })).toHaveAttribute('aria-expanded', 'false');

		const cancel = screen.getByRole('button', { name: 'Cancel' });
		expect(container.querySelector('.cn-live-task__top')).toContainElement(cancel);
		await fireEvent.click(cancel);
		expect(screen.getByRole('dialog', { name: 'Cancel this task?' })).toBeInTheDocument();
		expect(document.querySelector('[data-morph-target]')).toBeNull();
	});

	it('Inspect opens the run in the task drawer', async () => {
		setChatNext(true);
		const { container } = render(ChatPanel);
		setMessages(transcript());

		await waitFor(() => expect(container.querySelector('.cn-live-task')).not.toBeNull());
		await fireEvent.click(screen.getByRole('button', { name: 'Inspect →' }));

		// The shared drawer (TaskPanelDrawer) carries data-morph-target.
		await waitFor(() => expect(document.querySelector('[data-morph-target]')).not.toBeNull());
	});
});

// Regression: right after the chat page loads the task store is still empty
// ("Tasks 0"). A live card with no run id (a planning-phase status update
// carries none) falls back to opening the task, and that open used to await a
// full `/v3/tasks` list load before rendering anything — so the first Inspect
// click showed nothing for seconds. The drawer must open at once on its
// loading skeleton and fill in when the task arrives.
describe('ChatPanel live card Inspect before tasks load', () => {
	function planningTranscript(): ChatMessage[] {
		return transcript().map((message) =>
			message.id === 'status-1'
				? ({
						...message,
						content: {
							type: 'task_status_update',
							task_id: 'task-1',
							display_label: 'Review release',
							status: 'planning',
							summary: 'Planning the release review.'
						}
					} as unknown as ChatMessage)
				: message
		);
	}

	function json(body: unknown): Response {
		return new Response(JSON.stringify(body), {
			status: 200,
			headers: { 'Content-Type': 'application/json' }
		});
	}

	it('opens the drawer on its skeleton at once, then shows the task', async () => {
		let releaseTask: () => void = () => {};
		const taskArrived = new Promise<void>((resolve) => (releaseTask = resolve));
		const taskRecord = {
			task: {
				manifest: { task_id: 'task-1', title: 'Review release' },
				state: { task_id: 'task-1', status: 'planning' }
			}
		};
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				// The task list and the by-id read both wait until the test says so:
				// the store is empty and loading at click time.
				if (url.includes('/api/magician/v3/tasks/task-1')) {
					await taskArrived;
					return json(taskRecord);
				}
				if (url.includes('/api/magician/v3/tasks')) {
					await taskArrived;
					return json({ tasks: [] });
				}
				return json({});
			})
		);
		setChatNext(true);
		const { container } = render(ChatPanel);
		setMessages(planningTranscript());

		await waitFor(() => expect(container.querySelector('.cn-live-task')).not.toBeNull());
		await fireEvent.click(screen.getByRole('button', { name: 'Inspect →' }));

		// Before the task arrives: the drawer is already up, on its skeleton.
		await waitFor(() => expect(document.querySelector('[data-morph-target]')).not.toBeNull());
		expect(screen.getByRole('status', { name: 'Loading task' })).toBeInTheDocument();

		releaseTask();
		await waitFor(() =>
			expect(screen.queryByRole('status', { name: 'Loading task' })).toBeNull()
		);
		expect(document.querySelector('[data-morph-target]')).not.toBeNull();
	});

	it('closes the skeleton and says so when the task cannot be found', async () => {
		const showError = vi.spyOn(notifications, 'showError');
		vi.stubGlobal(
			'fetch',
			vi.fn(async (input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/api/magician/v3/tasks/task-1')) {
					return new Response('{}', { status: 404 });
				}
				if (url.includes('/api/magician/v3/tasks')) return json({ tasks: [] });
				return json({});
			})
		);
		setChatNext(true);
		const { container } = render(ChatPanel);
		setMessages(planningTranscript());

		await waitFor(() => expect(container.querySelector('.cn-live-task')).not.toBeNull());
		await fireEvent.click(screen.getByRole('button', { name: 'Inspect →' }));

		await waitFor(() => expect(showError).toHaveBeenCalledWith('Task not found.'));
		expect(document.querySelector('[data-morph-target]')).toBeNull();
	});
});

// A task the assistant starts mid-turn: its first status update lands after
// the user turn and before the assistant reply. Classic mode folds that
// update into the turn (the live turn bubble stands in for it); chat-next
// shows the live card at once, the way a planning task's card shows.
describe('ChatPanel chat-started task while the turn is in flight', () => {
	function status(id: string, at: number, extra: Record<string, unknown> = {}): ChatMessage {
		return {
			id,
			session_id: 'session-1',
			direction: 'system',
			created_at: at,
			content: {
				type: 'task_status_update',
				task_id: 'task-7',
				display_label: 'Compile the digest',
				status: 'running',
				execution_id: 'exec-7',
				summary: 'Reading the inbox.',
				...extra
			}
		} as unknown as ChatMessage;
	}

	function inFlight(): ChatMessage[] {
		const t0 = Date.now() - 4_000;
		return [
			{
				id: 'user-7',
				session_id: 'session-1',
				direction: 'user',
				created_at: t0,
				content: { type: 'text', text: 'Make me a digest' }
			} as unknown as ChatMessage,
			status('status-7a', t0 + 1_000)
		];
	}

	it('renders the live card before the assistant reply, and keeps one card as it updates and the reply lands', async () => {
		setChatNext(true);
		const { container } = render(ChatPanel);
		const turn = inFlight();
		setMessages(turn);

		await waitFor(() => expect(container.querySelector('.cn-live-task')).not.toBeNull());
		const card = container.querySelector('.cn-live-task') as HTMLElement;
		expect(card.querySelector('[data-testid="live-task-dot"]')).toHaveAttribute('data-status', 'running');
		expect(card).toHaveTextContent('Compile the digest');
		expect(card).toHaveTextContent('Running');
		expect(card.querySelector('[data-testid="live-task-elapsed"]')).not.toBeNull();
		expect(card).toHaveTextContent('Reading the inbox.');

		// A later update for the same task: still one card, now on the new line.
		const later = status('status-7b', turn[1].created_at + 1_000, { summary: 'Summarising 12 threads.' });
		setMessages([...turn, later]);
		await waitFor(() =>
			expect(container.querySelector('.cn-live-task')).toHaveTextContent('Summarising 12 threads.')
		);
		expect(container.querySelectorAll('.cn-live-task')).toHaveLength(1);

		// The assistant reply lands: the card stays.
		setMessages([
			...turn,
			later,
			{
				id: 'assistant-7',
				session_id: 'session-1',
				direction: 'assistant',
				created_at: later.created_at + 500,
				content: { type: 'text', text: 'Started the digest.' }
			} as unknown as ChatMessage
		]);
		await waitFor(() => expect(container).toHaveTextContent('Started the digest.'));
		expect(container.querySelectorAll('.cn-live-task')).toHaveLength(1);
		expect(container.querySelector('.cn-live-task')).toHaveTextContent('Summarising 12 threads.');
	});

	it('keeps classic mode folding the in-flight update into the turn', async () => {
		setChatNext(false);
		const { container } = render(ChatPanel);
		setMessages(inFlight());

		await waitFor(() => expect(container).toHaveTextContent('Make me a digest'));
		expect(container.querySelector('.chat-status-alert')).toBeNull();
		expect(container.querySelector('.cn-live-task')).toBeNull();
	});
});

// Use measured row heights: jsdom does not lay out the conversation itself.
function measureTranscript(el: HTMLElement): void {
	Object.defineProperty(el, 'scrollHeight', { configurable: true, get: () => el.querySelectorAll('.chat-message-row').length * 200 });
	Object.defineProperty(el, 'clientHeight', { configurable: true, value: 400 });
}

function historyRows(sessionId: string, count = 20): ChatMessage[] {
	return Array.from({ length: count }, (_, i) => ({
		id: `${sessionId}-${i}`, session_id: sessionId, direction: 'assistant',
		created_at: Date.now() - (count - i) * 1000, content: { type: 'text', text: `Reply ${sessionId} ${i}` }
	}));
}

describe('ChatPanel history viewport', () => {
	it('follows late card layout at the tail but leaves a reader who scrolls up in place', async () => {
		let notifyResize = () => {};
		vi.stubGlobal('ResizeObserver', class {
			constructor(private callback: () => void) {}
			observe(node: Element) {
				if (node.classList.contains('chat-message-list')) notifyResize = this.callback;
			}
			unobserve() {}
			disconnect() {}
		});
		const { container } = render(ChatPanel);
		const el = container.querySelector<HTMLElement>('.chat-messages-area')!;
		let extraHeight = 0;
		Object.defineProperty(el, 'scrollHeight', { configurable: true, get: () => el.querySelectorAll('.chat-message-row').length * 200 + extraHeight });
		Object.defineProperty(el, 'clientHeight', { configurable: true, value: 400 });
		setMessages(historyRows('session-1'));
		await waitFor(() => expect(el.scrollTop).toBe(4000));
		extraHeight = 500;
		notifyResize();
		expect(el.scrollTop).toBe(4500);
		el.scrollTop = 1000;
		await fireEvent.scroll(el);
		extraHeight = 1000;
		notifyResize();
		expect(el.scrollTop).toBe(1000);
	});

	it('lands instantly at the tail again when another conversation loads', async () => {
		const { container } = render(ChatPanel);
		const el = container.querySelector<HTMLElement>('.chat-messages-area')!;
		measureTranscript(el);
		setMessages(historyRows('session-1'));
		await waitFor(() => expect(el.scrollTop).toBe(4000));
		(el.scrollTo as ReturnType<typeof vi.fn>).mockClear();
		(chatState.current as ChatStateStore).update(s => ({ ...s, isLoading: true }));
		await waitFor(() => expect(screen.getByText('Loading conversation…')).toBeInTheDocument());
		(chatState.current as ChatStateStore).update(s => ({ ...s, activeSessionId: 'session-2', messages: historyRows('session-2', 30), isLoading: false }));
		await waitFor(() => expect(el.scrollTop).toBe(6000));
		expect(el.scrollTo).not.toHaveBeenCalledWith(expect.objectContaining({ behavior: 'smooth' }));
		expect(chatState.loadOlderMessages).not.toHaveBeenCalled();
	});

	it('loads a single older page at the top and preserves the reading position', async () => {
		const { container } = render(ChatPanel);
		const el = container.querySelector<HTMLElement>('.chat-messages-area')!;
		measureTranscript(el);
		setMessages(historyRows('session-1'));
		(chatState.current as ChatStateStore).update(s => ({ ...s, hasMoreMessages: true }));
		await waitFor(() => expect(el.scrollTop).toBe(4000));
		chatState.loadOlderMessages.mockImplementation(async () => {
			(chatState.current as ChatStateStore).update(s => ({ ...s,
				messages: [...historyRows('older', 50), ...(s.messages as ChatMessage[])], hasMoreMessages: true
			}));
		});
		el.scrollTop = 0;
		await fireEvent.scroll(el);
		await waitFor(() => expect(el.scrollTop).toBe(10000));
		expect(chatState.loadOlderMessages).toHaveBeenCalledExactlyOnceWith('session-1');
		expect(screen.getByRole('button', { name: 'Load earlier messages' })).toBeInTheDocument();
	});
});
