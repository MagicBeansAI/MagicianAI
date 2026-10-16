import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { syncAttentionCenterUrl } from '$lib/attention/centerState';
import { v2Events } from '$lib/realtime/v2-websocket';
import { closeAll } from '$lib/shell/overlayCoordinator';
import { resolveAttentionPrompt } from '$lib/stores/attentionPromptStore';
import { attentionStore } from '$lib/stores/attentionStore';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { taskStore } from '$lib/stores/taskStore';
import TaskAttentionFlowHarness from '../../../test/fixtures/TaskAttentionFlowHarness.svelte';
import { installFetchMock, MockWebSocket } from '../../../test/browser';
import {
	backendTask,
	clarificationAttentionItem,
	createTaskAttentionBackend,
	emitTaskUpdated,
	type TaskAttentionBackend
} from '../../../test/taskAttentionBackend';

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

function resetLocation(): void {
	const url = new URL('/tasks', window.location.origin);
	testKitRouter.commit(url, {}, 'replace');
	syncAttentionCenterUrl(url);
}

function cardFor(title: string): HTMLElement {
	const card = screen.getByRole('button', { name: title }).closest('article');
	if (!card) throw new Error(`Task card not found: ${title}`);
	return card;
}

function taskAction(title: string, name: string): HTMLButtonElement {
	const actions = within(cardFor(title)).getAllByRole('button', { name });
	return (
		actions.find((button) => button.classList.contains('presto-task-primary-action')) ?? actions[0]
	) as HTMLButtonElement;
}

beforeEach(() => {
	resolveAttentionPrompt(null);
	closeAll();
	resetLocation();
	scopeIdentityStore.reset();
	attentionStore.resetPage();
	taskStore.reset();
	v2Events.disconnect();
	v2Events.clear();
	MockWebSocket.instances = [];
	vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
		callback(0);
		return 1;
	});
});

afterEach(() => {
	cleanup();
	resolveAttentionPrompt(null);
	closeAll();
	attentionStore.stop();
	taskStore.reset();
	v2Events.disconnect();
	v2Events.clear();
	MockWebSocket.instances = [];
	resetLocation();
});

describe('task and Attention integration', () => {
	it('moves a waiting task back to running after its canonical Attention response', async () => {
		const user = userEvent.setup();
		const task = backendTask('handoff', 'waiting_for_user', {
			title: 'Prepare customer handoff'
		});
		backend = createTaskAttentionBackend([task]);
		backend.attentionItems = [clarificationAttentionItem(task)];
		backend.onHitlResponse = () => {
			backend.setTaskStatus(task.id, 'running');
			backend.attentionItems = [];
		};
		installFetchMock(backend.routes());
		render(TaskAttentionFlowHarness);

		await screen.findByRole('button', { name: task.title });
		expect(
			within(cardFor(task.title)).getByText('Needs Input', {
				selector: '.native-task-chip--status'
			})
		).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Open Attention' }));
		const attentionDialog = await screen.findByRole('dialog', { name: 'Attention' });
		const row = (
			await within(attentionDialog).findByText('Which environment should I use?', {
				selector: '.attention-page__prompt'
			})
		).closest('li');
		await user.click(within(row as HTMLElement).getByRole('button'));
		await user.type(await screen.findByRole('textbox', { name: 'Response' }), 'production');
		await user.click(screen.getByRole('button', { name: 'Submit' }));

		await waitFor(() => expect(backend.hitlBodies).toHaveLength(1));
		expect(backend.hitlBodies[0]).toMatchObject({
			source: 'clarification',
			input_type: 'text',
			value: { type: 'text', value: 'production' },
			task_id: task.id,
			execution_id: `execution-${task.id}`
		});

		emitTaskUpdated(task.id);
		await waitFor(
			() => {
				expect(
					within(cardFor(task.title)).getByText('Running', {
						selector: '.native-task-chip--status'
					})
				).toBeInTheDocument();
			},
			{ timeout: 2_500 }
		);
		expect(taskAction(task.title, 'Stop')).toBeEnabled();
		expect(await screen.findByText('Inbox zero.')).toBeInTheDocument();
		expect(within(attentionDialog).getByRole('button', { name: 'All 0' })).toHaveAttribute(
			'aria-current',
			'page'
		);
	});
});
