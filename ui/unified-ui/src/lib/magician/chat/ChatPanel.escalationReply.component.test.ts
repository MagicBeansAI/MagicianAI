// Mounts ChatPanel to prove an agent's pause is answered from the chat that
// started the run: the card's own options answer it in one click, and typed
// composer text answers it instead of starting a new turn. The chat store is
// stubbed (as in ChatPanel.chatNext.component.test.ts) with the few methods
// these paths call made real; the backend is the shared task/HITL fixture.
import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ModelChoice } from '$lib/llm/modelCatalog';
import type { ChatMessage } from '$lib/stores/chatStore';
import { chatProfileStore } from '$lib/stores/chatProfileStore';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { taskStore } from '$lib/stores/taskStore';
import { installFetchMock, jsonResponse, type MockFetchCall } from '../../../test/browser';
import {
	backendTask,
	createTaskAttentionBackend,
	type TaskAttentionBackend
} from '../../../test/taskAttentionBackend';
import ChatPanel from './ChatPanel.svelte';

const { chatState, chatCalls } = vi.hoisted(() => ({
	chatState: { current: null as unknown },
	chatCalls: {
		sendMessage: vi.fn(),
		sendMessageStreaming: vi.fn(),
		queueMessage: vi.fn(),
		markEscalationResolved: vi.fn()
	}
}));

// The composer dock's asks. Empty unless a test seeds one, so the card tests
// above see no dock; the thread selector stays real.
const { dockAsks } = vi.hoisted(() => ({ dockAsks: { current: null as unknown } }));
vi.mock('$lib/attention/pendingAsks', async (importActual) => {
	const actual = await importActual<typeof import('$lib/attention/pendingAsks')>();
	const { writable } = await import('svelte/store');
	const store = writable([] as import('$lib/attention/pendingAsks').PendingAsk[]);
	dockAsks.current = store;
	return { ...actual, pendingAsks: { subscribe: store.subscribe } };
});

vi.mock('$app/stores', async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});

vi.mock('$app/navigation', () => ({
	beforeNavigate: vi.fn(),
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
	// The store's own resolution: the matching card turns resolved.
	chatCalls.markEscalationResolved.mockImplementation(
		(_executionId: string, _requestId?: string, pauseStateId?: string) =>
			state.update((s) => ({
				...s,
				messages: s.messages.map((message) =>
					message.content.type === 'escalation' && message.content.pause_state_id === pauseStateId
						? { ...message, content: { ...message.content, resolved: true } }
						: message
				)
			}))
	);
	const target = {
		subscribe: state.subscribe,
		listQueuedMessages: vi.fn().mockResolvedValue([]),
		sendMessage: chatCalls.sendMessage,
		sendMessageStreaming: chatCalls.sendMessageStreaming,
		queueMessage: chatCalls.queueMessage,
		markEscalationResolved: chatCalls.markEscalationResolved
	};
	// Every other store method is an inert async no-op; only the state is real.
	const chatStore = new Proxy(target, {
		get: (object, key) =>
			key in object ? object[key as keyof typeof object] : vi.fn().mockResolvedValue(null)
	});
	return { ...actual, chatStore };
});

type ChatStateStore = { update: (fn: (s: Record<string, unknown>) => Record<string, unknown>) => void };

function setDockAsks(asks: import('$lib/attention/pendingAsks').PendingAsk[]): void {
	(dockAsks.current as { set: (value: unknown) => void }).set(asks);
}

/** The same pause as the dock holds it: chat leaves `hitl.requested` to the dock. */
function dockChoiceAsk(): import('$lib/attention/pendingAsks').PendingAsk {
	return {
		target: {
			id: 'pause-1',
			source: 'agentic',
			input_type: 'choice',
			prompt: 'Approve the Magican goal tree and write 19 notes?',
			input_schema: {
				type: 'choice',
				options: [
					{ id: 'approve_notes', label: 'Approve notes' },
					{ id: 'change', label: 'Change…' }
				]
			},
			identifiers: { pause_state_id: 'pause-1', correlation_id: 'pause-1' },
			scope: { execution_id: EXECUTION_ID, task_id: 't1', thread_id: 'general' }
		},
		taskId: 't1',
		threadId: 'general',
		// Raised by this chat's run: the shared `general` thread alone never
		// makes an ask local.
		chatSessionId: 'session-1',
		at: Date.now()
	} as import('$lib/attention/pendingAsks').PendingAsk;
}

function setMessages(messages: ChatMessage[]): void {
	(chatState.current as ChatStateStore).update((s) => ({ ...s, messages }));
}

// The waiting task's execution; the card's actionability reads the task.
const EXECUTION_ID = 'execution-t1';

/** A delegated run's `need_user_input(input_type="choice")` card, as the listener persists it. */
function choiceCard(overrides: Record<string, unknown> = {}): ChatMessage {
	return {
		id: 'esc-1',
		session_id: 'session-1',
		direction: 'system',
		created_at: Date.now() - 1_000,
		content: {
			type: 'escalation',
			question: 'The notes are drafted. What should I do?',
			escalation_type: 'user_input',
			input_type: 'choice',
			execution_id: EXECUTION_ID,
			pause_state_id: 'pause-1',
			options: [{ id: 'respond', label: 'Respond', requires_input: true }],
			input_schema: {
				type: 'choice',
				options: [
					{ id: 'approve_notes', label: 'Approve notes' },
					{ id: 'approve_all', label: 'Approve everything' },
					{ id: 'change', label: 'Change…' }
				],
				allow_other: false
			},
			...overrides
		}
	} as unknown as ChatMessage;
}

const composerModel: ModelChoice = {
	id: 'custom/model', label: 'Custom model', model: 'physical-model', provider: 'custom', connection: 'custom',
	limits: null,
	capabilities: { vision: true, tools: true, streaming: true, computer_use: false, structured_output: false, web_search: false },
	thinking: { mode: 'effort', levels: ['low'], default: 'low' },
	choices: { auto: 'opaque-auto', low: 'opaque-low' }
};

let backend: TaskAttentionBackend;
let calls: MockFetchCall[];

function respondCalls(): MockFetchCall[] {
	return calls.filter((call) => call.method === 'POST' && /\/hitl\/[^/]+\/respond$/.test(call.url));
}

function expectChoiceBody(selectedId: string, otherValue?: string): void {
	expect(respondCalls()).toHaveLength(1);
	expect(respondCalls()[0].url).toContain('/api/magician/v2/hitl/pause-1/respond');
	expect(backend.hitlBodies).toEqual([
		{
			source: 'escalation',
			input_type: 'choice',
			value: {
				type: 'choice',
				selected_id: selectedId,
				...(otherValue === undefined ? {} : { other_value: otherValue })
			},
			channel: 'web',
			execution_id: EXECUTION_ID
		}
	]);
}

function normalSends(): number {
	return (
		chatCalls.sendMessage.mock.calls.length +
		chatCalls.sendMessageStreaming.mock.calls.length +
		chatCalls.queueMessage.mock.calls.length
	);
}

function composer(): HTMLElement {
	return within(screen.getByRole('group', { name: 'Message composer' })).getByRole('textbox');
}

async function mountWith(messages: ChatMessage[]): Promise<void> {
	render(ChatPanel);
	setMessages(messages);
	// The composer refuses a typed send until Magician's model catalog has loaded.
	await waitFor(() => expect(get(chatProfileStore)).toMatchObject({ isLoading: false, selectionError: null }));
}

beforeEach(async () => {
	Element.prototype.scrollTo = vi.fn();
	scopeIdentityStore.reset();
	taskStore.reset();
	backend = createTaskAttentionBackend([backendTask('t1', 'waiting_for_user')]);
	({ calls } = installFetchMock([
		{
			match: '/plane/engines',
			handle: () => jsonResponse({
				engines: [
					{ name: 'magician', installed: true },
					{ name: 'claude_code', installed: true, models: ['default', 'opus'] },
					{ name: 'codex', installed: true, models: ['default'] }
				],
				current: 'magician',
				chat_current: 'magician'
			})
		},
		{
			match: '/chat/models',
			handle: () => jsonResponse({
				models: [composerModel],
				default_selection: { model_id: composerModel.id, thinking: 'auto' }
			})
		},
		...backend.routes()
	]));
	// The card is actionable only while its task is known and not finished.
	await taskStore.loadTasks();
	chatCalls.sendMessage.mockClear().mockResolvedValue(undefined);
	chatCalls.sendMessageStreaming.mockClear().mockResolvedValue(undefined);
	chatCalls.queueMessage.mockClear().mockResolvedValue(null);
	chatCalls.markEscalationResolved.mockClear();
	(chatState.current as ChatStateStore).update((s) => ({ ...s, isSendingMessage: false }));
	setMessages([]);
	setDockAsks([]);
});

afterEach(() => {
	cleanup();
	taskStore.reset();
	localStorage.clear();
	vi.unstubAllGlobals();
});

describe('ChatPanel one-click answers', () => {
	it('posts the clicked option to the pause and resolves the card', async () => {
		const user = userEvent.setup();
		await mountWith([choiceCard()]);

		await user.click(await screen.findByRole('button', { name: 'Approve notes' }));

		await waitFor(() => expect(backend.hitlBodies).toHaveLength(1));
		expectChoiceBody('approve_notes');
		await waitFor(() =>
			expect(chatCalls.markEscalationResolved).toHaveBeenCalledWith(EXECUTION_ID, undefined, 'pause-1')
		);
		await waitFor(() =>
			expect(screen.queryByRole('button', { name: 'Approve notes' })).not.toBeInTheDocument()
		);
		expect(normalSends()).toBe(0);
	});

	it('shows the failure on the card and keeps its options', async () => {
		const user = userEvent.setup();
		backend.hitlReplies.push({ status: 500, body: { error: 'resume failed' } });
		const { container } = render(ChatPanel);
		setMessages([choiceCard()]);

		await user.click(await screen.findByRole('button', { name: 'Approve notes' }));

		await waitFor(() =>
			expect(container.querySelector('.chat-escalation-error')).toHaveTextContent('resume failed')
		);
		expect(chatCalls.markEscalationResolved).not.toHaveBeenCalled();
		expect(screen.getByRole('button', { name: 'Approve notes' })).toBeEnabled();
	});
});

describe('ChatPanel typed answers', () => {
	it('sends composer text to the pending pause, not as a new turn', async () => {
		const user = userEvent.setup();
		await mountWith([choiceCard()]);

		expect(await screen.findByTestId('escalation-reply-banner')).toHaveTextContent(
			'Your reply will answer:'
		);
		await user.type(composer(), 'approve everything');
		await user.keyboard('{Enter}');

		await waitFor(() => expect(backend.hitlBodies).toHaveLength(1));
		// Matched by label, case-insensitively.
		expectChoiceBody('approve_all');
		await waitFor(() =>
			expect(chatCalls.markEscalationResolved).toHaveBeenCalledWith(
				EXECUTION_ID,
				undefined,
				'pause-1',
				'Answered: approve everything'
			)
		);
		expect(normalSends()).toBe(0);
		await waitFor(() => expect(screen.queryByTestId('escalation-reply-banner')).not.toBeInTheDocument());
	});

	it('sends words that name no option as the change option, with the words', async () => {
		const user = userEvent.setup();
		await mountWith([choiceCard()]);

		await screen.findByTestId('escalation-reply-banner');
		await user.type(composer(), 'Rename the second heading');
		await user.keyboard('{Enter}');

		await waitFor(() => expect(backend.hitlBodies).toHaveLength(1));
		expectChoiceBody('change', 'Rename the second heading');
		expect(normalSends()).toBe(0);
	});

	it('keeps words that name no answer in the composer and says what the ask takes', async () => {
		const user = userEvent.setup();
		await mountWith([
			choiceCard({
				input_schema: {
					type: 'choice',
					options: [
						{ id: 'plan_a', label: 'Plan A' },
						{ id: 'plan_b', label: 'Plan B' }
					]
				}
			})
		]);

		await screen.findByTestId('escalation-reply-banner');
		await user.type(composer(), 'something else entirely');
		await user.keyboard('{Enter}');

		await waitFor(() =>
			expect(screen.getByTestId('escalation-reply-banner')).toHaveTextContent(
				'Reply with: Plan A · Plan B, or ✕ to send as a message'
			)
		);
		expect(composer()).toHaveTextContent('something else entirely');
		expect(respondCalls()).toHaveLength(0);
		expect(normalSends()).toBe(0);
	});

	it('gives the words back when the pause was already gone', async () => {
		const user = userEvent.setup();
		// Current backends answer a drained pause with 410 `pause_state_gone`.
		backend.hitlReplies.push({ status: 410, body: { reason: 'pause_state_gone' } });
		await mountWith([choiceCard()]);

		await screen.findByTestId('escalation-reply-banner');
		await user.type(composer(), 'approve everything');
		await user.keyboard('{Enter}');

		await waitFor(() => expect(backend.hitlBodies).toHaveLength(1));
		await waitFor(() =>
			expect(chatCalls.markEscalationResolved).toHaveBeenCalledWith(
				EXECUTION_ID,
				undefined,
				'pause-1',
				'Already answered or closed'
			)
		);
		await waitFor(() => expect(composer()).toHaveTextContent('approve everything'));
		expect(normalSends()).toBe(0);
	});

	it('sends a normal message after the hint is cancelled', async () => {
		const user = userEvent.setup();
		await mountWith([choiceCard()]);

		await user.click(
			await screen.findByRole('button', { name: 'Send as a normal message instead' })
		);
		await waitFor(() => expect(screen.queryByTestId('escalation-reply-banner')).not.toBeInTheDocument());
		await user.type(composer(), 'approve everything');
		await user.keyboard('{Enter}');

		await waitFor(() => expect(normalSends()).toBe(1));
		expect(respondCalls()).toHaveLength(0);
		expect(chatCalls.markEscalationResolved).not.toHaveBeenCalled();
	});

	it('never targets a pause the card shows as resolved', async () => {
		const user = userEvent.setup();
		await mountWith([choiceCard({ resolved: true })]);

		await screen.findByRole('group', { name: 'Message composer' });
		expect(screen.queryByTestId('escalation-reply-banner')).not.toBeInTheDocument();
		await user.type(composer(), 'approve everything');
		await user.keyboard('{Enter}');

		await waitFor(() => expect(normalSends()).toBe(1));
		expect(respondCalls()).toHaveLength(0);
	});
});

describe('ChatPanel typed answers to the composer dock', () => {
	it('shows a taskless question whose only conversation stamp is the session id', async () => {
		const user = userEvent.setup();
		await mountWith([]);
		const ask = dockChoiceAsk();
		ask.threadId = null;
		ask.taskId = null;
		ask.chatSessionId = null;
		ask.target = {
			...ask.target,
			source: 'user_request',
			identifiers: { request_id: 'pause-1', correlation_id: 'pause-1' },
			scope: { principal: 'anonymous', workspace: 'default' },
			input_schema: {
				...ask.target.input_schema,
				request_type: 'need_user_input',
				context: { chat_session_id: 'session-1', chat_turn_id: 'turn-1' }
			}
		};
		setDockAsks([ask]);
		const dock = await screen.findByTestId('composer-ask-dock');
		expect(dock).toHaveTextContent(ask.target.prompt);
		const choice = within(dock).getByRole('radio', { name: 'Approve notes' });
		expect(choice).toBeEnabled();
		await user.click(choice);
		expect(within(dock).getByRole('button', { name: 'Answer' })).toBeEnabled();
	});

	it('answers the dock ask, with no card in the transcript, instead of starting a turn', async () => {
		const user = userEvent.setup();
		await mountWith([]);
		setDockAsks([dockChoiceAsk()]);

		expect(await screen.findByTestId('escalation-reply-banner')).toBeInTheDocument();
		await user.type(composer(), 'approve notes');
		await user.keyboard('{Enter}');

		await waitFor(() => expect(respondCalls()).toHaveLength(1));
		expect(respondCalls()[0].url).toContain('/api/magician/v2/hitl/pause-1/respond');
		expect(normalSends()).toBe(0);
	});

	it('sends a normal message when the dock has no ask', async () => {
		const user = userEvent.setup();
		await mountWith([]);

		await screen.findByRole('group', { name: 'Message composer' });
		expect(screen.queryByTestId('escalation-reply-banner')).not.toBeInTheDocument();
		await user.type(composer(), 'approve notes');
		await user.keyboard('{Enter}');

		await waitFor(() => expect(normalSends()).toBe(1));
		expect(respondCalls()).toHaveLength(0);
	});
});

describe('ChatPanel composer approvals during a live turn', () => {
	it.each(['file', 'exact'].flatMap(scope => [
		['allow_once', 'Allow once', scope],
		[`session:${scope}`, 'This session', scope],
		[`always:${scope}`, 'Always', scope],
		['deny', 'Deny', scope]
	]))('can submit %s while the chat is waiting for approval (%s, %s)', async (decision, label, scope) => {
		const user = userEvent.setup();
		await mountWith([]);
		const ask = dockChoiceAsk();
		ask.target = {
			...ask.target,
			id: 'god-file-edit',
			source: 'user_request',
			prompt: scope === 'exact' ? 'God mode: run this exact shell invocation?' : 'God mode: apply these file edits?',
			identifiers: { request_id: 'god-file-edit', correlation_id: 'god-file-edit' },
			scope: { principal: 'anonymous', workspace: 'default' },
			input_schema: {
				request_type: 'sandbox_override',
				context: { god_mode: true, chat_session_id: 'session-1', session: 'chat:session-1',
					command: scope === 'exact' ? "python3 -c 'print(1)'" : '/notes/plan.md' },
				options: [
					{ id: 'allow_once', label: 'Allow once' },
					{ id: `session:${scope}`, label: `This session: this ${scope}` },
					{ id: `always:${scope}`, label: `Always: this ${scope}` },
					{ id: 'deny', label: 'Deny' }
				]
			}
		};
		setDockAsks([ask]);
		(chatState.current as ChatStateStore).update((s) => ({ ...s, isSendingMessage: true }));

		const dock = within(await screen.findByTestId('composer-ask-dock'));
		for (const name of ['Allow once', 'This session', 'Always', 'Deny']) {
			expect(dock.getByRole('button', { name })).toBeEnabled();
		}
		await user.click(dock.getByRole('button', { name: label }));
		await waitFor(() => expect(backend.hitlBodies).toHaveLength(1));
		expect(respondCalls()[0].url).toContain('/api/magician/v2/hitl/god-file-edit/respond');
		expect(backend.hitlBodies[0]).toMatchObject({
			source: 'user_request',
			input_type: 'choice',
			value: { type: 'choice', selected_id: decision }
		});
		expect(normalSends()).toBe(0);
	});
});


describe('ChatPanel per-chat engine picks', () => {
	// The mocked chat state is shared by every test in this file.
	afterEach(() => (chatState.current as ChatStateStore).update((s) => ({ ...s, activeSessionId: 'session-1' })));

	function pin(sessionId: string, engine: string, model: string): void {
		const identity = get(scopeIdentityStore);
		localStorage.setItem(
			`magician.chat.harnessPreference.${JSON.stringify([identity.principal, identity.workspace, sessionId])}`,
			JSON.stringify({ engine, model })
		);
	}

	function lastSentOptions(): Record<string, unknown> | undefined {
		const sends = [
			...chatCalls.sendMessage.mock.calls,
			...chatCalls.sendMessageStreaming.mock.calls,
			...chatCalls.queueMessage.mock.calls
		];
		return sends.at(-1)?.[4] as Record<string, unknown> | undefined;
	}

	it('sends the open chat’s own engine on mount and after switching chats', async () => {
		pin('session-1', 'claude_code', 'opus');
		pin('session-2', 'codex', 'default');
		const user = userEvent.setup();
		await mountWith([]);

		await user.type(composer(), 'first chat');
		await user.keyboard('{Enter}');
		await waitFor(() => expect(lastSentOptions()).toMatchObject({ harnessEngine: 'claude_code', harnessModel: 'opus' }));

		(chatState.current as ChatStateStore).update((s) => ({ ...s, activeSessionId: 'session-2' }));
		await user.type(composer(), 'second chat');
		await user.keyboard('{Enter}');
		await waitFor(() => expect(lastSentOptions()).toMatchObject({
			harnessEngine: 'codex', harnessModel: 'default',
			modelRequest: { harness_engine: 'codex', harness_model: 'default' }
		}));

		(chatState.current as ChatStateStore).update((s) => ({ ...s, activeSessionId: 'session-1' }));
		await user.type(composer(), 'back to first');
		await user.keyboard('{Enter}');
		await waitFor(() => expect(lastSentOptions()).toMatchObject({ harnessEngine: 'claude_code', harnessModel: 'opus' }));
	});
});
