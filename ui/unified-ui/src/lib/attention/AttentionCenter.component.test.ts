import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { v2Events } from '$lib/realtime/v2-websocket';
import { closeAll } from '$lib/shell/overlayCoordinator';
import { resolveAttentionPrompt } from '$lib/stores/attentionPromptStore';
import { attentionStore } from '$lib/stores/attentionStore';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import AttentionCenterHarness from '../../test/fixtures/AttentionCenterHarness.svelte';
import { installFetchMock, MockWebSocket } from '../../test/browser';
import {
	backendTask,
	clarificationAttentionItem,
	createTaskAttentionBackend,
	type TaskAttentionBackend
} from '../../test/taskAttentionBackend';
import { syncAttentionCenterUrl } from './centerState';
import type { FeedItem } from '$lib/feed/types';

const testKitRouter = vi.hoisted(() => {
	type PageSnapshot = {
		data: Record<string, unknown>;
		error: null;
		form: null;
		params: Record<string, string>;
		route: { id: string };
		state: Record<string, unknown>;
		status: number;
		url: URL;
	};
	let page: PageSnapshot = {
		data: {},
		error: null,
		form: null,
		params: {},
		route: { id: '/tasks' },
		state: {},
		status: 200,
		url: new URL('http://localhost/tasks')
	};
	const subscribers = new Set<(snapshot: PageSnapshot) => void>();
	function commit(
		url: string | URL,
		state: Record<string, unknown>,
		mode: 'push' | 'replace'
	): void {
		const nextUrl = new URL(String(url), 'http://localhost');
		window.history[mode === 'push' ? 'pushState' : 'replaceState'](state, '', nextUrl);
		page = { ...page, state, url: nextUrl };
		for (const subscriber of subscribers) subscriber(page);
	}
	return {
		page: {
			subscribe(subscriber: (snapshot: PageSnapshot) => void): () => void {
				subscribers.add(subscriber);
				subscriber(page);
				return () => subscribers.delete(subscriber);
			}
		},
		commit
	};
});

vi.mock('$app/stores', () => ({
	page: testKitRouter.page,
	navigating: { subscribe: (subscriber: (value: null) => void) => (subscriber(null), () => {}) },
	updated: {
		subscribe: (subscriber: (value: boolean) => void) => (subscriber(false), () => {}),
		check: vi.fn(async () => false)
	}
}));

vi.mock('$app/navigation', () => ({
	pushState: (url: string | URL, state: Record<string, unknown>) =>
		testKitRouter.commit(url, state, 'push'),
	replaceState: (url: string | URL, state: Record<string, unknown>) =>
		testKitRouter.commit(url, state, 'replace'),
	goto: vi.fn(
		async (
			url: string | URL,
			options: { replaceState?: boolean; state?: Record<string, unknown> } = {}
		) => {
			testKitRouter.commit(url, options.state ?? {}, options.replaceState ? 'replace' : 'push');
		}
	),
	beforeNavigate: vi.fn(),
	afterNavigate: vi.fn(),
	onNavigate: vi.fn(),
	invalidate: vi.fn(async () => {}),
	invalidateAll: vi.fn(async () => {}),
	preloadData: vi.fn(async () => ({ type: 'loaded', status: 200, data: {} })),
	preloadCode: vi.fn(async () => {})
}));

vi.mock('$lib/stores/pendingHitlStore', async () => {
	const actual = await vi.importActual<typeof import('$lib/stores/pendingHitlStore')>(
		'$lib/stores/pendingHitlStore'
	);
	return {
		...actual,
		ensurePendingHitlBridge: vi.fn()
	};
});

let backend: TaskAttentionBackend;

function resetAttentionLocation(): void {
	const url = new URL('/tasks', window.location.origin);
	testKitRouter.commit(url, {}, 'replace');
	syncAttentionCenterUrl(url);
}

function attentionItem(
	id: string,
	prompt = 'Which environment should I use?',
	updatedAt = Date.parse('2026-07-11T10:00:00.000Z')
): FeedItem {
	const task = backendTask(id, 'waiting_for_user', { title: `Task ${id}` });
	const item = clarificationAttentionItem(task);
	return {
		...item,
		title: prompt,
		summary: prompt,
		updated_at: updatedAt,
		metadata: {
			...(item.metadata as Record<string, unknown>),
			questions: [prompt]
		}
	};
}

async function openAttention(user: ReturnType<typeof userEvent.setup>): Promise<HTMLElement> {
	await user.click(screen.getByRole('button', { name: 'Open Attention' }));
	return await screen.findByRole('dialog', { name: 'Attention' });
}

async function rowForPrompt(dialog: HTMLElement, prompt: string): Promise<HTMLElement> {
	const promptElement = await within(dialog).findByText(prompt, {
		selector: '.attention-page__prompt'
	});
	const row = promptElement.closest('li');
	if (!row) throw new Error(`Attention row not found: ${prompt}`);
	return row;
}

beforeEach(() => {
	resolveAttentionPrompt(null);
	closeAll();
	resetAttentionLocation();
	scopeIdentityStore.reset();
	attentionStore.resetPage();
	v2Events.disconnect();
	v2Events.clear();
	MockWebSocket.instances = [];
});

afterEach(() => {
	cleanup();
	resolveAttentionPrompt(null);
	closeAll();
	attentionStore.stop();
	v2Events.disconnect();
	v2Events.clear();
	MockWebSocket.instances = [];
	resetAttentionLocation();
});

describe('AttentionCenter mounted flows', () => {
	it('opens the paginated list, focuses it, and restores focus when closed', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('focus')];
		installFetchMock(backend.routes());
		render(AttentionCenterHarness);

		const trigger = screen.getByRole('button', { name: 'Open Attention' });
		trigger.focus();
		const dialog = await openAttention(user);

		await waitFor(() => expect(dialog).toHaveFocus());
		expect(await rowForPrompt(dialog, 'Which environment should I use?')).toBeInTheDocument();
		await user.click(within(dialog).getByRole('button', { name: 'Close Attention' }));

		await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument());
		await waitFor(() => expect(trigger).toHaveFocus());
	});

	it('opens an exact item directly without flashing the list and cancels it with Escape', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('direct')];
		installFetchMock(backend.routes());
		render(AttentionCenterHarness, { exactItemId: 'pause-direct' });

		await user.click(screen.getByRole('button', { name: 'Open exact item' }));
		const prompt = await screen.findByRole('dialog', { name: 'Provide input' });

		expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument();
		expect(within(prompt).getByText('Which environment should I use?')).toBeInTheDocument();
		await user.keyboard('{Escape}');

		await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument());
		expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument();
	});

	it('resolves a direct item absent from the capped feed page through the exact endpoint', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('outside-page', 'Only the exact lookup can see me')];
		backend.attentionListItems = [];
		const { calls } = installFetchMock(backend.routes());
		render(AttentionCenterHarness, { exactItemId: 'pause-outside-page' });

		await user.click(screen.getByRole('button', { name: 'Open exact item' }));
		await waitFor(() =>
			expect(
				calls.some(
					(call) =>
						new URL(call.url, window.location.origin).pathname ===
						'/api/magician/v2/feed/attention/pause-outside-page'
				)
			).toBe(true)
		);
		const prompt = await screen.findByRole('dialog', { name: 'Provide input' });

		expect(within(prompt).getByText('Only the exact lookup can see me')).toBeInTheDocument();
		expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument();
		expect(
			calls.some(
				(call) =>
					new URL(call.url, window.location.origin).pathname ===
					'/api/magician/v2/feed/attention/pause-outside-page'
			)
		).toBe(true);
		expect(
			calls.some((call) => new URL(call.url, window.location.origin).search.includes('_cursor='))
		).toBe(false);
		await user.keyboard('{Escape}');
		await waitFor(() =>
			expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument()
		);
	});

	it('closes a detached exact prompt when another surface resolves it', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('remote-resolution', 'Resolve me elsewhere')];
		backend.attentionListItems = [];
		installFetchMock(backend.routes());
		render(AttentionCenterHarness, { exactItemId: 'pause-remote-resolution' });

		await user.click(screen.getByRole('button', { name: 'Open exact item' }));
		expect(await screen.findByRole('dialog', { name: 'Provide input' })).toBeInTheDocument();
		const socket = MockWebSocket.instances.at(-1);
		if (!socket) throw new Error('Expected Attention realtime socket');
		socket.receive(
			JSON.stringify({
				event_type: 'HitlResolved',
				data: {
					correlation_id: 'pause-remote-resolution',
					principal: 'anonymous',
					workspace: 'default'
				}
			})
		);

		await waitFor(() =>
			expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument()
		);
		await waitFor(() =>
			expect(new URL(window.location.href).searchParams.get('attention_item')).toBeNull()
		);
		expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument();
	});

	it('opens a taskless bot-auth notification through the exact endpoint', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		const correlationId = 'bot_auth:anonymous:default:gmail';
		backend.attentionItems = [
			{
				id: `runtime:hitl:${correlationId}`,
				principal: 'anonymous',
				workspace: 'default',
				item_type: 'escalation',
				title: 'Connect Gmail',
				summary: 'Sign in to continue',
				status: 'needs_action',
				created_at: 1,
				updated_at: 1,
				actions: [],
				metadata: {
					attention_kind: 'hitl.requested',
					source: 'bot_auth',
					input_type: 'choice',
					correlation_id: correlationId,
					request_id: correlationId,
					input_schema: {
						type: 'choice',
						options: [{ id: 'open_auth_flow', label: 'Connect' }]
					}
				}
			}
		];
		backend.attentionListItems = [];
		installFetchMock(backend.routes());
		render(AttentionCenterHarness, { exactItemId: correlationId });

		await user.click(screen.getByRole('button', { name: 'Open exact item' }));
		const prompt = await screen.findByRole('dialog', { name: 'Choose an option' });
		expect(within(prompt).getByText('Sign in to continue')).toBeInTheDocument();
		expect(within(prompt).getByRole('radio', { name: 'Connect' })).toBeInTheDocument();
		expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument();

		await user.keyboard('{Escape}');
		await waitFor(() =>
			expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument()
		);
	});

	it('releases a direct item selection when the exact endpoint returns 404', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionListItems = [];
		installFetchMock(backend.routes());
		render(AttentionCenterHarness, { exactItemId: 'missing-item' });

		await user.click(screen.getByRole('button', { name: 'Open exact item' }));

		await waitFor(() =>
			expect(new URL(window.location.href).searchParams.get('attention_item')).toBeNull()
		);
		expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument();
		expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument();
	});

	it('uses native_attention shell mode to suppress browser-only Attention navigation', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('native-list')];
		installFetchMock(backend.routes());
		const url = new URL('/attention?native_attention=1&attention=1', window.location.origin);
		testKitRouter.commit(url, {}, 'replace');

		render(AttentionCenterHarness);

		const dialog = await screen.findByRole('dialog', { name: 'Attention' });
		expect(within(dialog).queryByRole('link', { name: /full Attention/i })).not.toBeInTheDocument();
		await user.click(within(dialog).getByRole('button', { name: 'Close Attention' }));
		await waitFor(() =>
			expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument()
		);
	});

	it('resolves attention_item from a native Attention route directly into the exact HITL prompt', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('native-route')];
		backend.attentionListItems = [];
		installFetchMock(backend.routes());
		const url = new URL(
			'/attention?native_attention=1&attention_item=pause-native-route',
			window.location.origin
		);
		testKitRouter.commit(url, {}, 'replace');

		render(AttentionCenterHarness);

		const prompt = await screen.findByRole('dialog', { name: 'Provide input' });
		expect(screen.queryByRole('dialog', { name: 'Attention' })).not.toBeInTheDocument();
		expect(within(prompt).getByText('Which environment should I use?')).toBeInTheDocument();
		expect(window.location.pathname).toBe('/attention');
		expect(new URL(window.location.href).searchParams.get('attention_item')).toBe(
			'pause-native-route'
		);

		await user.keyboard('{Escape}');
		await waitFor(() =>
			expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument()
		);
	});

	// Secure HITL P5: the link a critical-request alert carries opens the exact
	// item behind login; once the request has resolved it says so instead of
	// showing a form or an empty page.
	it('opens a critical-alert link (attention=1&attention_item=<correlation>) to the exact prompt', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('alert-link')];
		installFetchMock(backend.routes());
		testKitRouter.commit(
			new URL('/attention?attention=1&attention_item=pause-alert-link', window.location.origin),
			{},
			'replace'
		);
		render(AttentionCenterHarness);
		const prompt = await screen.findByRole('dialog', { name: 'Provide input' });
		expect(within(prompt).getByText('Which environment should I use?')).toBeInTheDocument();
		await user.keyboard('{Escape}');
		await waitFor(() =>
			expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument()
		);
	});

	it('shows "no longer available" for a critical-alert link whose request already resolved', async () => {
		backend = createTaskAttentionBackend();
		backend.attentionItems = [];
		backend.attentionListItems = [];
		installFetchMock(backend.routes());
		testKitRouter.commit(
			new URL('/attention?attention=1&attention_item=pause-alert-link', window.location.origin),
			{},
			'replace'
		);
		render(AttentionCenterHarness);
		const dialog = await screen.findByRole('dialog', { name: 'Attention' });
		expect(await within(dialog).findByText('Item no longer available')).toBeInTheDocument();
		expect(within(dialog).getByText(/may have been resolved/)).toBeInTheDocument();
		expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument();
	});

	it('submits the selected row through the canonical HITL endpoint and removes it', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('resolve')];
		backend.onHitlResponse = () => {
			backend.attentionItems = [];
		};
		installFetchMock(backend.routes());
		render(AttentionCenterHarness);

		await openAttention(user);
		const row = await rowForPrompt(
			screen.getByRole('dialog', { name: 'Attention' }),
			'Which environment should I use?'
		);
		await user.click(within(row).getByRole('button'));

		const response = await screen.findByRole('textbox', { name: 'Response' });
		await user.type(response, 'staging');
		await user.click(screen.getByRole('button', { name: 'Submit' }));

		await waitFor(() => expect(backend.hitlBodies).toHaveLength(1));
		expect(backend.hitlBodies[0]).toEqual({
			source: 'clarification',
			input_type: 'text',
			value: { type: 'text', value: 'staging' },
			channel: 'web',
			task_id: 'resolve',
			execution_id: 'execution-resolve'
		});
		await screen.findByText('Inbox zero.');
		expect(screen.queryByRole('dialog', { name: 'Provide input' })).not.toBeInTheDocument();
	});

	it('keeps a validation reask in the same flow and pre-fills the previous answer', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		backend.attentionItems = [attentionItem('reask')];
		backend.hitlReplies = [
			{
				body: {
					resumed: false,
					status: 'reask_required',
					question: 'Specify the exact deployment account.',
					hint: 'Use the account name, not only the environment.',
					previous_answer: 'staging'
				}
			},
			{ body: { resumed: true } }
		];
		backend.onHitlResponse = () => {
			if (backend.hitlBodies.length === 2) backend.attentionItems = [];
		};
		installFetchMock(backend.routes());
		render(AttentionCenterHarness);

		await openAttention(user);
		const row = await rowForPrompt(
			screen.getByRole('dialog', { name: 'Attention' }),
			'Which environment should I use?'
		);
		await user.click(within(row).getByRole('button'));
		let response = await screen.findByRole('textbox', { name: 'Response' });
		await user.type(response, 'staging');
		await user.click(screen.getByRole('button', { name: 'Submit' }));

		expect(await screen.findByText('Specify the exact deployment account.')).toBeInTheDocument();
		response = screen.getByRole('textbox', { name: 'Response' });
		expect(response).toHaveValue('staging');
		await user.clear(response);
		await user.type(response, 'production account');
		await user.click(screen.getByRole('button', { name: 'Submit' }));

		await waitFor(() => expect(backend.hitlBodies).toHaveLength(2));
		expect(backend.hitlBodies[1]).toMatchObject({
			value: { type: 'text', value: 'production account' }
		});
		await screen.findByText('Inbox zero.');
	});

	it('pages a proven seven-item chronology forward and backward in six-row windows', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		const baseTime = Date.parse('2026-07-11T10:00:00.000Z');
		backend.attentionItems = Array.from({ length: 7 }, (_, index) =>
			attentionItem(`page-${index + 1}`, `Decision ${index + 1}`, baseTime - index * 1_000)
		);
		installFetchMock(backend.routes());
		render(AttentionCenterHarness);

		const dialog = await openAttention(user);
		expect(await rowForPrompt(dialog, 'Decision 1')).toBeInTheDocument();
		expect(within(dialog).getByText('Decision 6', { selector: '.attention-page__prompt' })).toBeInTheDocument();
		expect(within(dialog).queryByText('Decision 7', { selector: '.attention-page__prompt' })).not.toBeInTheDocument();
		expect(within(dialog).getByRole('button', { name: 'All 7' })).toHaveAttribute('aria-current', 'page');

		await user.click(within(dialog).getByRole('button', { name: 'Next page' }));
		expect(await rowForPrompt(dialog, 'Decision 7')).toBeInTheDocument();
		expect(within(dialog).queryByText('Decision 1', { selector: '.attention-page__prompt' })).not.toBeInTheDocument();
		expect(within(dialog).getByText('Page 2 of 2')).toBeInTheDocument();
		expect(within(dialog).getByText('7-7 of 7')).toBeInTheDocument();

		await user.click(within(dialog).getByRole('button', { name: 'Previous page' }));
		expect(await rowForPrompt(dialog, 'Decision 1')).toBeInTheDocument();
		expect(within(dialog).queryByText('Decision 7', { selector: '.attention-page__prompt' })).not.toBeInTheDocument();
	});

	it('jumps straight to the last and first page of a proven chronology', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend();
		const baseTime = Date.parse('2026-07-11T10:00:00.000Z');
		backend.attentionItems = Array.from({ length: 7 }, (_, index) =>
			attentionItem(`page-${index + 1}`, `Decision ${index + 1}`, baseTime - index * 1_000)
		);
		installFetchMock(backend.routes());
		render(AttentionCenterHarness);

		const dialog = await openAttention(user);
		expect(await rowForPrompt(dialog, 'Decision 1')).toBeInTheDocument();

		await user.click(within(dialog).getByRole('button', { name: 'Last page' }));
		expect(await rowForPrompt(dialog, 'Decision 7')).toBeInTheDocument();
		expect(within(dialog).getByText('Page 2 of 2')).toBeInTheDocument();

		await user.click(within(dialog).getByRole('button', { name: 'First page' }));
		expect(await rowForPrompt(dialog, 'Decision 1')).toBeInTheDocument();
		expect(within(dialog).getByText('Page 1 of 2')).toBeInTheDocument();
	});
});
