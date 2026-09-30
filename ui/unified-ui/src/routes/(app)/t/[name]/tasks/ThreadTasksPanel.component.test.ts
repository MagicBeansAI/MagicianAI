/**
 * The thread workspace's Tasks tab, opening the unified task panel.
 *
 * **This exists because both sides were already tested and the joint was not.**
 * `taskPanelModel.test.ts` proves the store→panel mapping, and
 * `UnifiedTaskPanel.component.test.ts` proves the rendering; a route that
 * mounted the drawer and never fed it — or fed it the wrong task — would pass
 * both suites. Workstream B lost a whole workspace's attention call to exactly
 * that gap, so these assert what only the composed route can be wrong about:
 * that opening a task here reaches the panel through `toTaskPanelModel`, and
 * that the task on screen is the one the reader picked.
 */
import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { v2Events } from '$lib/realtime/v2-websocket';
import { PANEL_POLL_FAST_MS } from '$lib/magician/tasks/taskPanelPoll';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { taskStore } from '$lib/stores/taskStore';
import ThreadTasksPanelHarness from '../../../../../test/fixtures/ThreadTasksPanelHarness.svelte';
import {
	installFetchMock,
	jsonResponse,
	MockWebSocket,
	type MockFetchRoute
} from '../../../../../test/browser';
import {
	activityItem,
	backendTask,
	createTaskAttentionBackend,
	taskPanelState
} from '../../../../../test/taskAttentionBackend';

/**
 * The route reads `$page.url` and calls `goto` to round-trip `?selected=`, and
 * neither exists outside a running SvelteKit app. A one-URL stand-in is enough:
 * nothing here asserts on navigation, it only has to not be `undefined`.
 */
const testKitRouter = vi.hoisted(() => {
	let url = new URL('http://localhost/t/general/tasks');
	const subscribers = new Set<(snapshot: { url: URL }) => void>();
	function commit(next: string | URL): void {
		url = new URL(String(next), 'http://localhost');
		for (const subscriber of subscribers) subscriber({ url });
	}
	return {
		commit,
		page: {
			subscribe(subscriber: (snapshot: { url: URL }) => void): () => void {
				subscribers.add(subscriber);
				subscriber({ url });
				return () => subscribers.delete(subscriber);
			}
		}
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
	goto: vi.fn(async (url: string | URL) => testKitRouter.commit(url)),
	pushState: vi.fn(),
	replaceState: vi.fn(),
	beforeNavigate: vi.fn(),
	afterNavigate: vi.fn(),
	onNavigate: vi.fn(),
	invalidate: vi.fn(async () => {}),
	invalidateAll: vi.fn(async () => {}),
	preloadData: vi.fn(async () => ({ type: 'loaded', status: 200, data: {} })),
	preloadCode: vi.fn(async () => {})
}));

const THREAD = 'general';

const panel = () => screen.getByRole('dialog', { name: 'Task panel' });

/** An act section's header button, scoped to the panel so a card cannot answer for it. */
const act = (name: 'Plan' | 'Run' | 'Output') =>
	within(panel()).getByRole('button', { name: new RegExp(`^${name}`) });

/**
 * Open an act's body, whatever state it is already in.
 *
 * A bare click would **close** the act the panel opened for the reader — with no
 * choice made the panel follows the task's state, so which act starts open is a
 * fact about the fixture's status rather than about this test, and a test that
 * assumed it would silently assert against a collapsed body.
 */
async function openAct(
	user: ReturnType<typeof userEvent.setup>,
	name: 'Plan' | 'Run' | 'Output'
): Promise<void> {
	const header = act(name);
	if (header.getAttribute('aria-expanded') !== 'true') await user.click(header);
}

/**
 * The thread record the route asks for after any action it takes. Only the
 * shape `threadStore` reads is answered; the endpoint sends more, and restating
 * all of it would be a second copy of a contract nothing here checks.
 */
function threadRoute(): MockFetchRoute {
	return {
		match: (call) => new URL(call.url, 'http://localhost').pathname.includes('/threads'),
		handle: () => jsonResponse({ thread: { id: THREAD, name: THREAD }, threads: [] })
	};
}

function outputsRoute(taskId: string, refs: unknown[]): MockFetchRoute {
	return {
		method: 'GET',
		match: (call) =>
			new URL(call.url, 'http://localhost').pathname ===
			`/api/magician/v3/tasks/${taskId}/outputs`,
		handle: () => jsonResponse({ outputs: { outputs: refs } })
	};
}

beforeEach(() => {
	scopeIdentityStore.reset();
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
	vi.useRealTimers();
	taskStore.reset();
	v2Events.disconnect();
	v2Events.clear();
	MockWebSocket.instances = [];
});

describe('the thread Tasks tab opens the unified task panel', () => {
	it('answers "is this task okay?" for the task the reader opened', async () => {
		const user = userEvent.setup();
		const task = backendTask('thread-verdict', 'running', {
			title: 'Reindex the thread corpus',
			ui_thread_id: THREAD
		});
		const backend = createTaskAttentionBackend([task]);
		installFetchMock([threadRoute(), ...backend.routes()]);
		await taskStore.loadTasks();

		render(ThreadTasksPanelHarness, { threadName: THREAD });
		await user.click(await screen.findByRole('button', { name: task.title }));

		// The verdict, derived from the store task's status by the same adapter
		// `/tasks` uses. Sever the `toTaskPanelModel` call and there is no
		// dialog to find at all.
		const verdict = await within(panel()).findByRole('status');
		expect(verdict).toHaveAttribute('data-verdict-state', 'running');

		// The drawer's chrome is the shell's, not this route's: the retired
		// panel's tab bar is gone and the shared close control is present.
		expect(within(panel()).queryByRole('button', { name: 'Debug' })).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Close task panel' })).toBeInTheDocument();

		// The Output act arrives with its own request, so it is absent until
		// that answers — an empty act would claim `no output` about a live run.
		await waitFor(() => expect(act('Output')).toBeInTheDocument());
	});

	/**
	 * The wrong-task failure the id checks exist to prevent: a verdict, a title
	 * and an output list that belong to the task the reader just left. Nothing
	 * downstream can catch it, because every value involved is individually
	 * valid.
	 */
	it('replaces the whole panel when the reader opens a second task', async () => {
		const user = userEvent.setup();
		const first = backendTask('thread-alpha', 'running', {
			title: 'Alpha thread run',
			ui_thread_id: THREAD
		});
		const second = backendTask('thread-beta', 'failed', {
			title: 'Beta thread run',
			ui_thread_id: THREAD,
			completion_outcome: 'Provider unavailable'
		});
		const backend = createTaskAttentionBackend([first, second]);
		installFetchMock([
			threadRoute(),
			outputsRoute(first.id, [{ relative_path: 'alpha-report.md', media_type: 'text/markdown' }]),
			outputsRoute(second.id, []),
			...backend.routes()
		]);
		await taskStore.loadTasks();

		render(ThreadTasksPanelHarness, { threadName: THREAD });
		await user.click(await screen.findByRole('button', { name: first.title }));
		await waitFor(() =>
			expect(within(panel()).getByRole('status')).toHaveAttribute('data-verdict-state', 'running')
		);
		await waitFor(() => expect(within(panel()).getByText('alpha-report.md')).toBeInTheDocument());

		await user.click(screen.getByRole('button', { name: 'Close task panel' }));
		await user.click(await screen.findByRole('button', { name: second.title }));

		await waitFor(() =>
			expect(within(panel()).getByRole('status')).toHaveAttribute('data-verdict-state', 'failed')
		);
		// The first task's file must not survive the swap: the outputs are handed
		// to the adapter only once they are known to describe *this* task.
		expect(within(panel()).queryByText('alpha-report.md')).not.toBeInTheDocument();
	});

	/**
	 * **The Run act's timeline, on a task-backed surface.**
	 *
	 * This is the behavioural half of a proof that four surfaces share.
	 * `taskPanelSurfaces.test.ts` asserts `/today` computes its model and fetches
	 * its run state with expressions character-identical to this route's, and that
	 * transfer is only worth anything if this route's version is proven to *work*
	 * — so the thing being transferred has to be asserted somewhere, and this is
	 * the only one of the four a component test can mount.
	 *
	 * What it pins is the whole path in one gesture: the fetch happens, its
	 * payload survives to the adapter, `deriveTimeline` agrees that the payload
	 * describes the run the act names, and the rows reach the DOM. Sever any link
	 * and the act's summary loses its event count.
	 */
	it('renders what the run did, from the payload the ask already arrives in', async () => {
		const user = userEvent.setup();
		const task = backendTask('thread-timeline', 'running', {
			title: 'Rebuild the index',
			ui_thread_id: THREAD
		});
		const backend = createTaskAttentionBackend([task]);
		backend.activityByTask[task.id] = [
			activityItem(task.id, {
				id: 'log-1',
				created_at: 1_770_000_030_000,
				title: 'Ignored, because the metadata names the capability',
				metadata: { event_type: 'llm.succeeded', capability: 'research', model: 'claude-opus-4' }
			}),
			activityItem(task.id, {
				id: 'log-2',
				created_at: 1_770_000_060_000,
				status: 'running',
				metadata: { event_type: 'tool.started', target: 'read_file' }
			})
		];
		installFetchMock([threadRoute(), ...backend.routes()]);
		await taskStore.loadTasks();

		render(ThreadTasksPanelHarness, { threadName: THREAD });
		await user.click(await screen.findByRole('button', { name: task.title }));

		// The count is on the closed act's L1 line, so it is readable before the
		// act is opened — and it is the one assertion a fabricated `[]` timeline
		// could not satisfy, because an empty list contributes no `events` segment.
		await waitFor(() => expect(act('Run')).toHaveTextContent('2 events'));

		await openAct(user, 'Run');
		// The rows in the vocabulary the chat activity card uses, which is the
		// point of titling them from metadata rather than from the humanized title
		// the backend sent — the fixture's title is deliberately not either of
		// these, so a row that fell back to it fails here.
		expect(await within(panel()).findByText('Thinking with research')).toBeInTheDocument();
		expect(within(panel()).getByText('Calling read_file')).toBeInTheDocument();
	});

	/**
	 * **How the timeline stays current, and what that costs — measured rather
	 * than claimed.**
	 *
	 * The panel has no subscription of its own, so the list follows a live run
	 * only as often as the shared panel poll makes this request. Drive its real
	 * fast-cadence timer here: a general task-list refresh is deliberately not a
	 * second trigger, because the panel poll refreshes only the open task and
	 * coalesces an in-flight read.
	 *
	 * The second half is what makes that refetch usable rather than a strobe. A
	 * loader that blanked its state before every request would empty the Run act
	 * on every poll and refill it a round trip later, so the response is held
	 * open here and the rows are asserted to still be on screen *while the next
	 * request is in flight* — the exact window a reader would otherwise spend
	 * looking at an empty act.
	 */
	it('follows the run across polls without blanking what is already on screen', async () => {
		vi.useFakeTimers();
		const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
		const task = backendTask('thread-cadence', 'running', {
			title: 'Cadence run',
			ui_thread_id: THREAD
		});
		const activity = [
			activityItem(task.id, {
				id: 'log-1',
				created_at: 1_770_000_030_000,
				metadata: { event_type: 'llm.succeeded', capability: 'research' }
			})
		];

		let served = 0;
		let holdFrom = Number.POSITIVE_INFINITY;
		/** The held responses' resolvers. A list rather than a `let`, because
		 *  narrowing cannot see a value assigned inside a promise executor. */
		const held: Array<() => void> = [];
		const { calls } = installFetchMock([
			threadRoute(),
			{
				method: 'GET',
				match: (call) =>
					new URL(call.url, 'http://localhost').pathname ===
					`/api/magician/v3/tasks/${task.id}/execution-panel`,
				handle: async () => {
					served += 1;
					if (served >= holdFrom) await new Promise<void>((resolve) => held.push(resolve));
					return jsonResponse(taskPanelState(task, { activity }));
				}
			},
			...createTaskAttentionBackend([task]).routes()
		]);
		await taskStore.loadTasks();

		render(ThreadTasksPanelHarness, { threadName: THREAD });
		await user.click(await screen.findByRole('button', { name: task.title }));
		await waitFor(() => expect(act('Run')).toHaveTextContent('1 event'));

		const panelRequests = () =>
			calls.filter((call) => call.url.includes(`/tasks/${task.id}/execution-panel`)).length;
		const before = panelRequests();
		holdFrom = served + 1;

		await vi.advanceTimersByTimeAsync(PANEL_POLL_FAST_MS);
		expect(panelRequests()).toBeGreaterThan(before);
		// Still on screen with the refresh unanswered: the loader kept what it had
		// because the task is the same task.
		expect(act('Run')).toHaveTextContent('1 event');

		for (const resolve of held) resolve();
	});

	/**
	 * **The other half of the capability model, and the half a fabrication would
	 * pass.** A run whose events were *read* and numbered zero says so; a run
	 * whose payload never arrived renders no timeline at all. Both produce no
	 * `events` segment on the L1 line — `count` drops a zero — so the L1 line
	 * cannot tell them apart, and the body is where the distinction is either kept
	 * or lost. An unread run showing `No activity recorded yet` would be a claim
	 * about the run made out of a fact about the network.
	 */
	it('distinguishes a run that recorded nothing from one nobody could read', async () => {
		const user = userEvent.setup();
		const read = backendTask('thread-quiet', 'running', {
			title: 'Quiet run',
			ui_thread_id: THREAD
		});
		const unread = backendTask('thread-unread', 'running', {
			title: 'Unreadable run',
			ui_thread_id: THREAD
		});
		const backend = createTaskAttentionBackend([read, unread]);
		installFetchMock([
			threadRoute(),
			// Ahead of the backend's own handler, so this one task's panel state is
			// the only request that fails.
			{
				method: 'GET',
				match: (call) =>
					new URL(call.url, 'http://localhost').pathname ===
					`/api/magician/v3/tasks/${unread.id}/execution-panel`,
				handle: () => jsonResponse({ error: 'nope' }, { status: 500 })
			},
			...backend.routes()
		]);
		await taskStore.loadTasks();

		render(ThreadTasksPanelHarness, { threadName: THREAD });
		await user.click(await screen.findByRole('button', { name: read.title }));
		await waitFor(() => expect(act('Run')).toBeInTheDocument());
		await openAct(user, 'Run');
		expect(await within(panel()).findByText('No activity recorded yet')).toBeInTheDocument();
		expect(act('Run')).not.toHaveTextContent(/events/);

		await user.click(screen.getByRole('button', { name: 'Close task panel' }));
		await user.click(await screen.findByRole('button', { name: unread.title }));
		await waitFor(() => expect(act('Run')).toBeInTheDocument());
		await openAct(user, 'Run');
		await waitFor(() =>
			expect(within(panel()).queryByText('No activity recorded yet')).not.toBeInTheDocument()
		);
		expect(within(panel()).queryByRole('log', { name: 'Run activity' })).not.toBeInTheDocument();
	});
});
