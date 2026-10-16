// Mounts ChatPanel with a stubbed chatStore to prove the chat-next wiring:
// flag on → LiveTaskCard (expands in place on click), flag off → classic
// TaskStatusCard. The other ChatPanel tests are source-contract tests; this
// is the first that mounts the panel, so every collaborator it touches on
// mount is stubbed to an inert async no-op.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ChatMessage } from '$lib/stores/chatStore';
import { taskStore } from '$lib/stores/taskStore';
import * as notifications from '$lib/shared/stores/notifications';
import { setChatNext } from './next/chatNextFlag';
import ChatPanel from './ChatPanel.svelte';

const { chatState } = vi.hoisted(() => ({ chatState: { current: null as unknown } }));

vi.mock('$app/stores', async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});

vi.mock('$app/navigation', () => ({
	goto: vi.fn().mockResolvedValue(undefined),
	replaceState: vi.fn()
}));

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
		{ subscribe: state.subscribe, listQueuedMessages: vi.fn().mockResolvedValue([]) },
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
	// jsdom has no scrolling; ChatPanel scrolls the transcript on load.
	Element.prototype.scrollTo = vi.fn();
	vi.stubGlobal(
		'fetch',
		vi.fn(async () =>
			new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json' } })
		)
	);
	setMessages([]);
});

afterEach(() => {
	cleanup();
	setChatNext(false);
	localStorage.clear();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('ChatPanel chat-next wiring', () => {
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

	it('offers Cancel in the title row and no empty expansion; no side panel opens', async () => {
		setChatNext(true);
		const { container } = render(ChatPanel);
		setMessages(transcript());

		await waitFor(() => expect(container.querySelector('.cn-live-task')).not.toBeNull());
		// The live activity has produced nothing yet: nothing to expand.
		expect(screen.queryByRole('button', { name: /Review release/ })).toBeNull();
		expect(container.querySelector('.cn-live-task__chevron')).toBeNull();

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
