import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { v2Events } from '$lib/realtime/v2-websocket';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { taskStore } from '$lib/stores/taskStore';
import TasksWorkspace from './TasksWorkspace.svelte';
import {
	installFetchMock,
	jsonResponse,
	MockWebSocket,
	type MockFetchRoute
} from '../../../test/browser';
import {
	activityItem,
	backendTask,
	createTaskAttentionBackend,
	emitTaskUpdated,
	type TaskAttentionBackend,
	type TaskBackendRecord
} from '../../../test/taskAttentionBackend';

let backend: TaskAttentionBackend;

/**
 * The list card for a title, resolved from the *card* and not from the document.
 *
 * A task's title is the accessible name of two buttons whenever its panel is
 * open: the card's title button in the list, and the panel header's
 * clamp-expand button — same role, same name, one in the list and one in the
 * `Task panel` dialog. So `getByRole` cannot be asked for "the button called
 * `Ship launch brief`" and be understood; it throws on the ambiguity. The card
 * is the match that sits inside an `<article>`, which is the list's element and
 * not the drawer's.
 *
 * This is why the helper takes all the matches and picks: the alternative —
 * scoping to a container found by class — asserts against a stylesheet name,
 * and the alternative to *that* is dropping the accessible-name query for a
 * `data-testid`, which stops testing the thing a reader actually uses to find
 * the row.
 */
function cardFor(title: string): HTMLElement {
	const card = screen
		.getAllByRole('button', { name: title })
		.map((button) => button.closest('article'))
		.find((element): element is HTMLElement => element !== null);
	if (!card) throw new Error(`Task card not found: ${title}`);
	return card;
}

function taskAction(title: string, name: string): HTMLButtonElement {
	const actions = within(cardFor(title)).getAllByRole('button', { name });
	return (
		actions.find((button) => button.classList.contains('presto-task-primary-action')) ?? actions[0]
	) as HTMLButtonElement;
}

async function waitForCardStatus(title: string, status: string): Promise<void> {
	await waitFor(
		() => expect(within(cardFor(title)).getByText(status, { selector: '.native-task-chip--status' })).toBeInTheDocument(),
		{ timeout: 2_500 }
	);
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
	taskStore.reset();
	v2Events.disconnect();
	v2Events.clear();
	MockWebSocket.instances = [];
});

describe('TasksWorkspace state flows', () => {
	it('moves one task through pending, planning, ready, running, paused, needs-input, resumed, and completed views', async () => {
		const user = userEvent.setup();
		const task = backendTask('launch', 'pending', { title: 'Ship launch brief' });
		backend = createTaskAttentionBackend([task]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await waitForCardStatus(task.title, 'Ready');
		expect(taskAction(task.title, 'PrePlan')).toBeEnabled();

		backend.setTaskStatus(task.id, 'planning', {
			active_root_execution_id: `execution-${task.id}`
		});
		emitTaskUpdated(task.id);
		await waitForCardStatus(task.title, 'Planning');
		expect(taskAction(task.title, 'Stop')).toBeEnabled();
		expect(await within(cardFor(task.title)).findByRole('button', { name: 'Steer run' })).toBeEnabled();
		expect(within(cardFor(task.title)).getByRole('button', { name: 'Pause run' })).toBeEnabled();

		backend.setTaskStatus(task.id, 'ready');
		emitTaskUpdated(task.id);
		await waitForCardStatus(task.title, 'Ready');

		await user.click(taskAction(task.title, 'Run Now'));
		await waitForCardStatus(task.title, 'Running');
		expect(taskAction(task.title, 'Stop')).toBeEnabled();

		const closePanel = await screen.findByRole('button', { name: 'Close task panel' });
		await user.click(closePanel);

		backend.setTaskStatus(task.id, 'paused');
		emitTaskUpdated(task.id);
		await waitForCardStatus(task.title, 'Paused');
		expect(taskAction(task.title, 'View Execution')).toBeEnabled();
		expect(await within(cardFor(task.title)).findByRole('button', { name: 'Resume run' })).toBeEnabled();
		expect(within(cardFor(task.title)).queryByRole('button', { name: 'Steer run' })).not.toBeInTheDocument();

		backend.setTaskStatus(task.id, 'waiting_for_user');
		emitTaskUpdated(task.id);
		await waitForCardStatus(task.title, 'Needs Input');
		expect(taskAction(task.title, 'View Question')).toBeEnabled();

		backend.setTaskStatus(task.id, 'running');
		emitTaskUpdated(task.id);
		await waitForCardStatus(task.title, 'Running');

		backend.setTaskStatus(task.id, 'completed', {
			last_completed_root_execution_id: `execution-${task.id}`,
			completion_summary: 'Launch brief delivered.'
		});
		emitTaskUpdated(task.id);
		await waitFor(() => {
			expect(screen.queryByRole('button', { name: task.title })).not.toBeInTheDocument();
		});

		await user.click(screen.getByRole('button', { name: /^Completed/ }));
		await screen.findByRole('button', { name: task.title });
		await waitFor(() => {
			const checkbox = within(cardFor(task.title)).getByRole('checkbox', {
				name: `Mark ${task.title} complete`
			});
			expect(checkbox).toBeChecked();
		});
		expect(within(cardFor(task.title)).queryByText('Completed', { selector: '.native-task-chip--status' })).not.toBeInTheDocument();
		expect(screen.getByText('Launch brief delivered.')).toBeInTheDocument();
	});

	it('stops active work and resets cancelled and failed executions to a runnable state', async () => {
		const user = userEvent.setup();
		const task = backendTask('recovery', 'running', { title: 'Recover deployment' });
		backend = createTaskAttentionBackend([task]);
		const { calls } = installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(taskAction(task.title, 'Stop'));

		await waitForCardStatus(task.title, 'Ready');
		expect(backend.cancelledExecutions).toEqual([`execution-${task.id}`]);
		const cancelIndex = calls.findIndex((call) => call.url.endsWith(`/executions/execution-${task.id}/cancel`));
		const statusIndex = calls.findIndex((call) => call.url.endsWith(`/tasks/${task.id}/status`));
		expect(statusIndex).toBeGreaterThan(cancelIndex);
		expect(taskAction(task.title, 'Run Now')).toBeEnabled();

		backend.setTaskStatus(task.id, 'cancelled', {
			active_root_execution_id: null,
			latest_root_execution_id: `execution-${task.id}`,
			completion_outcome: 'Stopped by operator'
		});
		emitTaskUpdated(task.id);
		await waitForCardStatus(task.title, 'Cancelled');
		expect(screen.getByText('Stopped by operator')).toBeInTheDocument();
		await user.click(taskAction(task.title, 'Reset to Ready'));
		await waitForCardStatus(task.title, 'Ready');

		backend.setTaskStatus(task.id, 'failed', {
			active_root_execution_id: null,
			latest_root_execution_id: `execution-${task.id}`,
			completion_outcome: 'Provider unavailable'
		});
		emitTaskUpdated(task.id);
		await waitForCardStatus(task.title, 'Failed');
		expect(screen.getByText('Provider unavailable')).toBeInTheDocument();

		await user.click(taskAction(task.title, 'Reset to Ready'));
		await waitForCardStatus(task.title, 'Ready');
		expect(taskAction(task.title, 'Run Now')).toBeEnabled();
	});

	it('returns a successful recurring execution to ready instead of filing it as completed', async () => {
		const user = userEvent.setup();
		const task = backendTask('recurring', 'ready', {
			title: 'Daily account digest',
			schedule: {
				kind: { Cron: { expression: '0 9 * * *', timezone: 'Asia/Kolkata' } },
				timezone: 'Asia/Kolkata'
			}
		});
		backend = createTaskAttentionBackend([task]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(taskAction(task.title, 'Run Now'));
		await waitForCardStatus(task.title, 'Running');

		taskStore.completeExecution(true);
		await waitForCardStatus(task.title, 'Ready');
		expect(screen.queryByText('Completed', { selector: '.native-task-chip--status' })).not.toBeInTheDocument();
	});

	it('publishes a completed task through the scoped Notes projection endpoint', async () => {
		const user = userEvent.setup();
		const task = backendTask('publish-note', 'completed', {
			title: 'Archive launch result',
			last_completed_root_execution_id: 'execution-publish-note'
		});
		backend = createTaskAttentionBackend([task]);
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: `/notes/publish/task/${task.id}`,
				handle: () => jsonResponse({
					projection_id: `task:${task.id}`,
					task_id: task.id,
					provider: 'silverbullet',
					used_fallback: false,
					note_path: `Tasks/2026-08-01/${task.id}.md`,
					assets: []
				})
			},
			...backend.routes()
		]);

		render(TasksWorkspace, { navigationMode: 'local' });
		await user.click(await screen.findByRole('button', { name: /^Completed/ }));
		await screen.findByRole('button', { name: task.title });
		await user.click(taskAction(task.title, 'Publish to Notes'));

		await waitFor(() => {
			const request = calls.find((call) => call.url.endsWith(`/notes/publish/task/${task.id}`));
			expect(request).toBeDefined();
			const body = JSON.parse(String(request?.init?.body));
			expect(body).not.toHaveProperty('principal');
			expect(body).not.toHaveProperty('workspace');
		});
	});

	it('removes a task only after the destructive confirmation is accepted', async () => {
		const user = userEvent.setup();
		const task = backendTask('delete', 'ready', { title: 'Obsolete investigation' });
		backend = createTaskAttentionBackend([task]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(within(cardFor(task.title)).getByTitle('More actions'));
		await user.click(screen.getByRole('menuitem', { name: 'Delete…' }));

		expect(screen.getByRole('dialog', { name: 'Delete task' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: task.title })).toBeInTheDocument();
		await user.click(screen.getByRole('button', { name: 'Delete task' }));

		await waitFor(() => {
			expect(screen.queryByRole('button', { name: task.title })).not.toBeInTheDocument();
		});
		expect(backend.tasks).toEqual([]);
	});
});

/**
 * The seam between the task store and the panel's pure modules.
 *
 * Both sides are already tested — `taskPanelModel.test.ts` proves the mapping
 * and `UnifiedTaskPanel.component.test.ts` proves the rendering — and neither
 * proves the joint. These assert what only the composed thing can be wrong
 * about: which task's data reaches the panel, whose reader-choice it carries,
 * and which file a click acts on.
 */
describe('TasksWorkspace opens the unified task panel', () => {
	const panel = () => screen.getByRole('dialog', { name: 'Task panel' });

	/** An act section's header button, scoped to the panel so a card cannot answer for it. */
	const act = (name: 'Plan' | 'Run' | 'Output') =>
		within(panel()).getByRole('button', { name: new RegExp(`^${name}`) });

	function outputsRoute(taskId: string, refs: unknown[]): MockFetchRoute {
		return {
			method: 'GET',
			match: (call) =>
				new URL(call.url, 'http://localhost').pathname ===
				`/api/magician/v3/tasks/${taskId}/outputs`,
			handle: () => jsonResponse({ outputs: { outputs: refs } })
		};
	}

	/**
	 * **There used to be an `attentionRoute` here, and it was a lie.** It answered
	 * `/execution-panel` with `{ run: { needs_attention: [...] } }` — no
	 * `overview`, no `output`, no `debug` — on the stated grounds that the suite
	 * read only that branch. That held exactly as long as the client read only
	 * that branch: the moment the same response also had to carry the run's event
	 * log, the fixture was answering a shape the endpoint cannot produce, and no
	 * amount of coverage could have said so because vitest transpiles without
	 * type-checking.
	 *
	 * The rows now go through `backend.attentionByTask`, which fills the one
	 * `panelState` every test in the repo shares. A fixture with one definition
	 * cannot drift from the endpoint in only some suites.
	 */
	it('answers "is this task okay?" for the selected task instead of showing its tabs', async () => {
		const user = userEvent.setup();
		const task = backendTask('verdict', 'running', { title: 'Reindex the corpus' });
		backend = createTaskAttentionBackend([task]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(screen.getByRole('button', { name: task.title }));

		// The verdict, not a tab bar: one sentence about the task's state, and
		// the acts as rows beneath it.
		const verdict = await within(panel()).findByRole('status');
		expect(verdict).toHaveAttribute('data-verdict-state', 'running');
		expect(within(panel()).queryByRole('button', { name: 'Debug' })).not.toBeInTheDocument();

		// The Output act arrives with its own request, so it is absent until that
		// answers — an empty act would claim `no output` about a live run.
		await waitFor(() => expect(act('Output')).toBeInTheDocument());
		expect(act('Run')).toHaveAttribute('aria-expanded', 'true');
	});

	/**
	 * The panel keeps the reader's chosen act across polls on purpose, and must
	 * not keep it across *tasks*. It tells the two apart from `TaskPanelModel.id`,
	 * which the adapter fills from the store — this asserts that end to end, over
	 * a real selection rather than a prop swap.
	 *
	 * **The chosen act is Plan, not Output, and that is the whole point.** Both
	 * tasks have a plan, so Plan is still present after the swap and the panel's
	 * "the act they chose is gone" fallback cannot fire. Choosing Output instead
	 * would prove nothing: the outputs are refetched per task, so the Output act
	 * is briefly absent and that fallback would open Run whether or not identity
	 * did anything.
	 */
	it('does not open the next task on the act the reader chose for the last one', async () => {
		const user = userEvent.setup();
		const first = backendTask('alpha', 'running', { title: 'Alpha run', has_plan: true });
		const second = backendTask('beta', 'running', { title: 'Beta run', has_plan: true });
		backend = createTaskAttentionBackend([first, second]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: first.title });
		await user.click(screen.getByRole('button', { name: first.title }));
		await waitFor(() => expect(act('Plan')).toBeInTheDocument());

		// A running task opens on Run; the reader moves to Plan, which is a choice
		// the state would never have made.
		await user.click(act('Plan'));
		expect(act('Plan')).toHaveAttribute('aria-expanded', 'true');
		expect(act('Run')).toHaveAttribute('aria-expanded', 'false');

		await user.click(screen.getByRole('button', { name: second.title }));
		await waitFor(() =>
			expect(within(panel()).getByRole('heading', { level: 2 })).toHaveTextContent(second.title)
		);

		// Beta is in the same state as alpha, so anything carried over would look
		// like a correct default. It is Run that is correct here.
		await waitFor(() => expect(act('Run')).toHaveAttribute('aria-expanded', 'true'));
		expect(act('Plan')).toHaveAttribute('aria-expanded', 'false');
	});

	it('never shows one task\'s output files under another task', async () => {
		const user = userEvent.setup();
		const first = backendTask('written', 'failed', { title: 'Wrote something' });
		const second = backendTask('silent', 'failed', { title: 'Wrote nothing' });
		backend = createTaskAttentionBackend([first, second]);
		installFetchMock([
			outputsRoute(first.id, [{ relative_path: 'q3/notes.md', media_type: 'text/markdown' }]),
			outputsRoute(second.id, []),
			...backend.routes()
		]);

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: first.title });
		await user.click(screen.getByRole('button', { name: first.title }));
		await waitFor(() => expect(act('Output')).toHaveTextContent('notes.md'));

		await user.click(screen.getByRole('button', { name: second.title }));

		// `no output` is a real claim about the second task, and `notes.md` would
		// be a false one — the two tasks are otherwise identical, so a panel
		// carrying the first one's files over looks entirely plausible.
		await waitFor(() => expect(act('Output')).toHaveTextContent('no output'));
		expect(within(panel()).queryByText(/notes\.md/)).not.toBeInTheDocument();
	});

	it('opens the panel focused on the Output act when Result is clicked', async () => {
		const user = userEvent.setup();
		const task = backendTask('done-task', 'completed', {
			title: 'Generate quarterly numbers',
			completion_summary: 'Report generated.'
		});
		backend = createTaskAttentionBackend([task]);
		installFetchMock([
			outputsRoute(task.id, [{ relative_path: 'q3/report.csv', media_type: 'text/csv' }]),
			...backend.routes()
		]);

		render(TasksWorkspace, { navigationMode: 'local' });
		await user.click(screen.getByRole('button', { name: /^Completed/ }));
		await screen.findByRole('button', { name: task.title });

		const viewResultButton = taskAction(task.title, 'Result');
		expect(viewResultButton).toBeInTheDocument();
		await user.click(viewResultButton);

		await waitFor(() => expect(act('Output')).toBeInTheDocument());
		expect(act('Output')).toHaveAttribute('aria-expanded', 'true');
	});

	/**
	 * The joint that the display name cannot carry. Two outputs share a
	 * basename, so the rows are indistinguishable by what the panel renders —
	 * anything resolving the path from the name sends the wrong file, and a
	 * fixture with two distinct names would not notice.
	 */
	it('opens the file the clicked row names, not the first one that matches it', async () => {
		const user = userEvent.setup();
		const task = backendTask('outputs', 'failed', { title: 'Nightly summary' });
		backend = createTaskAttentionBackend([task]);
		const { calls } = installFetchMock([
			outputsRoute(task.id, [
				{ relative_path: 'alpha/report.md', media_type: 'text/markdown' },
				{ relative_path: 'beta/report.md', media_type: 'text/markdown' }
			]),
			...backend.routes()
		]);

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(screen.getByRole('button', { name: task.title }));
		await waitFor(() => expect(act('Output')).toBeInTheDocument());
		await user.click(act('Output'));

		const opens = within(panel()).getAllByRole('button', { name: 'Open report.md' });
		expect(opens).toHaveLength(2);
		await user.click(opens[1]);

		await waitFor(() => {
			const request = calls.find((call) => call.url.includes('/outputs/open-file'));
			expect(request).toBeDefined();
			expect(JSON.parse(String(request?.init?.body ?? '{}'))).toEqual({
				relative_path: 'beta/report.md'
			});
		});

		await user.click(within(panel()).getAllByRole('button', { name: 'Reveal report.md' })[0]);
		await waitFor(() => {
			const request = calls.find((call) => call.url.includes('/outputs/open-folder'));
			expect(JSON.parse(String(request?.init?.body ?? '{}'))).toEqual({
				relative_path: 'alpha/report.md'
			});
		});
	});

	it('leaves the Output act absent when its request fails, rather than claiming no output', async () => {
		const user = userEvent.setup();
		const task = backendTask('unreadable', 'failed', { title: 'Broken outputs' });
		backend = createTaskAttentionBackend([task]);
		installFetchMock([
			{
				method: 'GET',
				match: (call) =>
					new URL(call.url, 'http://localhost').pathname ===
					`/api/magician/v3/tasks/${task.id}/outputs`,
				handle: () => jsonResponse({ error: 'nope' }, { status: 500 })
			},
			...backend.routes()
		]);

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(screen.getByRole('button', { name: task.title }));
		await within(panel()).findByRole('status');

		// The Run act proves the panel rendered at all, so the absent Output act
		// below is a decision rather than a panel that never appeared.
		expect(act('Run')).toBeInTheDocument();
		expect(within(panel()).queryByRole('button', { name: /^Output/ })).not.toBeInTheDocument();
		expect(within(panel()).queryByText(/no output/i)).not.toBeInTheDocument();
	});

	/**
	 * **Watching, which is one of the two modes this panel was built for.**
	 *
	 * The event log and the verdict come from two different requests, and until
	 * this poll existed neither was made on a clock: an open panel refreshed only
	 * when the task store happened to replace the record, which is the list's own
	 * backstop and stops entirely once nothing in the list is live. So a panel left
	 * open on a running task showed a frozen feed under `Running`, forever.
	 *
	 * This asserts the whole of it with **nothing touched after the panel opens** —
	 * new events reach the feed, and the verdict above them moves when the task's
	 * own state does. Both halves matter: live events under a headline that cannot
	 * change do not read as stale, they read as broken.
	 *
	 * It spends real seconds, which is why it is the only test here that does. The
	 * cadence is the subject; faking the clock would assert that a timer was set
	 * rather than that the panel followed the run.
	 */
	it('follows a live run on its own clock, and moves the verdict with it', async () => {
		const user = userEvent.setup();
		const task = backendTask('watched', 'running', { title: 'Reindex the corpus' });
		backend = createTaskAttentionBackend([task]);
		backend.activityByTask[task.id] = [
			activityItem(task.id, {
				id: 'log-1',
				created_at: 1_770_000_030_000,
				metadata: { event_type: 'llm.succeeded', capability: 'research' }
			})
		];
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(screen.getByRole('button', { name: task.title }));
		await waitFor(() => expect(act('Run')).toHaveTextContent('1 event'));

		// The run does two more things. Nothing here clicks, refreshes, or emits a
		// realtime event — the only thing that can bring these to the panel is the
		// panel's own poll.
		backend.activityByTask[task.id].push(
			activityItem(task.id, {
				id: 'log-2',
				created_at: 1_770_000_060_000,
				status: 'running',
				metadata: { event_type: 'tool.started', target: 'read_file' }
			}),
			activityItem(task.id, {
				id: 'log-3',
				created_at: 1_770_000_090_000,
				metadata: { event_type: 'llm.succeeded', capability: 'research' }
			})
		);

		await waitFor(() => expect(act('Run')).toHaveTextContent('3 events'), { timeout: 12_000 });

		// And the verdict, which is read off the task record rather than off the
		// payload above: the same tick refreshes the row, so the headline cannot sit
		// at `Running` under a feed that has moved on.
		expect(within(panel()).getByRole('status')).toHaveAttribute('data-verdict-state', 'running');
		backend.setTaskStatus(task.id, 'waiting_for_user');

		await waitFor(
			() =>
				expect(within(panel()).getByRole('status')).toHaveAttribute(
					'data-verdict-state',
					'waiting'
				),
			{ timeout: 12_000 }
		);
	}, 40_000);

	/**
	 * **A settled task is not polled, and that must not be confused with not being
	 * read.** The panel's run payload has exactly one source on this route, so a
	 * cadence of `off` that also skipped the first read would leave a finished
	 * task's Run act with no timeline at all — and the reader would have no way to
	 * tell that from a run that recorded nothing.
	 */
	it('reads a settled task\'s run once, even though it will not poll it', async () => {
		const user = userEvent.setup();
		// `failed` rather than `completed` for a reason that is the list's and not
		// the panel's: the `all` lane filters completed tasks out, so the card the
		// panel opens from would not be there to click.
		const task = backendTask('settled', 'failed', { title: 'Settled run' });
		backend = createTaskAttentionBackend([task]);
		backend.activityByTask[task.id] = [
			activityItem(task.id, {
				id: 'log-1',
				created_at: 1_770_000_030_000,
				metadata: { event_type: 'llm.succeeded', capability: 'research' }
			}),
			activityItem(task.id, {
				id: 'log-2',
				created_at: 1_770_000_060_000,
				metadata: { event_type: 'tool.started', target: 'read_file' }
			})
		];
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(screen.getByRole('button', { name: task.title }));

		await waitFor(() => expect(act('Run')).toHaveTextContent('2 events'));
		expect(within(panel()).getByRole('status')).toHaveAttribute('data-verdict-state', 'failed');
	});

	/**
	 * Design §6, case 3: never render stale state as current. A refresh that does
	 * not land is the one failure a poll adds to this panel, and the honest answer
	 * is neither a red verdict — that would be a claim about the task made out of a
	 * fact about the network — nor silence, which is what `sharedPoll` does on its
	 * own by keeping its last good value.
	 */
	it('says the state is stale when its refresh fails, rather than freezing quietly', async () => {
		const user = userEvent.setup();
		const task = backendTask('unreachable', 'running', { title: 'Rebuild the index' });
		backend = createTaskAttentionBackend([task]);
		installFetchMock([
			// Ahead of the backend's own handler, so the run behind this task is the
			// only thing that cannot be read.
			{
				method: 'GET',
				match: (call) =>
					new URL(call.url, 'http://localhost').pathname ===
					`/api/magician/v3/tasks/${task.id}/execution-panel`,
				handle: () => jsonResponse({ error: 'gone' }, { status: 503 })
			},
			...backend.routes()
		]);

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: task.title });
		await user.click(screen.getByRole('button', { name: task.title }));

		const stale = await within(panel()).findByText(/Showing last known state/);
		expect(stale).toBeInTheDocument();
		// The task's own verdict is untouched by the network's failure — the panel
		// reports what it knows and labels how current it is.
		expect(
			within(panel()).getByText(/Running/, { selector: '.verdict__headline' })
		).toBeInTheDocument();
		expect(within(panel()).queryByText("Can't load this task")).not.toBeInTheDocument();
	});

	/**
	 * **The joint, not either side of it.** The adapter's own suite proves a
	 * `VerdictAttention` handed to it produces `Waiting on you`, and
	 * `runAttentionFrom`'s suite proves the payload produces a `VerdictAttention`
	 * — and both passed while nothing fetched the payload at all, which is
	 * exactly the shape of defect this plan keeps producing. These two tests run
	 * the whole path: a wire status, a real request, a rendered verdict.
	 */
	describe('a task blocked mid-run', () => {
		/**
		 * `waiting_for_user` with the plan question taken back off. That pairing is
		 * the edge: the store normalises the status to `paused`, which is truthful
		 * for a deliberate pause but cannot by itself say a human is being waited on.
		 */
		const blocked = (id: string, title: string) =>
			backendTask(id, 'waiting_for_user', { title, pending_questions: [] });

		it('reads Paused while nothing can tell the panel an ask exists', async () => {
			const user = userEvent.setup();
			const task = blocked('unasked', 'Reconcile the ledger');
			backend = createTaskAttentionBackend([task]);
			installFetchMock([outputsRoute(task.id, []), ...backend.routes()]);

			render(TasksWorkspace, { navigationMode: 'local' });
			await screen.findByRole('button', { name: task.title });
			await user.click(screen.getByRole('button', { name: task.title }));

			await waitFor(() =>
				expect(within(panel()).getByRole('status')).toHaveTextContent('Paused')
			);
		});

		it('reads Waiting on you once the ask is fetched, and says what is being asked', async () => {
			const user = userEvent.setup();
			const task = blocked('blocked', 'Apply the migration');
			backend = createTaskAttentionBackend([task]);
			backend.attentionByTask[task.id] = [
				{
					id: 'feed-1',
					principal: 'anonymous',
					workspace: 'default',
					task_id: task.id,
					item_type: 'approval',
					title: 'Approval requested',
					status: 'needs_action',
					created_at: Date.now() - 60_000,
					updated_at: Date.now() - 60_000,
					actions: [],
					metadata: {},
					hitl_request: {
						id: 'ccp-1',
						source: 'diff_approval',
						input_type: 'diff_approval',
						prompt: 'Apply 3 edits to schema.sql?'
					}
				}
			];
			installFetchMock([outputsRoute(task.id, []), ...backend.routes()]);

			render(TasksWorkspace, { navigationMode: 'local' });
			await screen.findByRole('button', { name: task.title });
			await user.click(screen.getByRole('button', { name: task.title }));

			await waitFor(() => {
				const verdict = within(panel()).getByRole('status');
				expect(verdict).toHaveTextContent('Waiting on you');
				// The ask itself, so the reader knows what is wanted without
				// leaving the panel — and not the lane label the feed row carries.
				expect(verdict).toHaveTextContent('Apply 3 edits to schema.sql?');
				expect(verdict).not.toHaveTextContent('Queued');
			});

			// The ask is raised by a run, so the panel lands the reader in Run and
			// not in the act a plan-time clarification would have opened.
			expect(act('Run')).toHaveAttribute('aria-expanded', 'true');
		});

		/**
		 * **The control that answers it, on the surface that told the reader about
		 * it.** B5 built the whole path — the ask on the model, the prompt fields,
		 * the bounded re-ask loop, `answerTaskAsk`, and this workspace's
		 * `handlePanelAnswer` with its request-id guard — and then never passed
		 * `answerAsk` to the drawer. So the panel's own suite covered the block
		 * through a harness that sets the prop, `handlePanelAnswer` had no caller,
		 * and on the real surface the reader was told what was wanted and given
		 * nowhere to say it. Both halves were tested; the joint was not.
		 *
		 * This asserts the joint: the block reaches the DOM through the surface's
		 * own mount, with a control in it, keyed to the ask the verdict describes.
		 */
		it('offers the control that answers the ask, and not only the sentence describing it', async () => {
			const user = userEvent.setup();
			const task = blocked('answerable', 'Apply the migration');
			backend = createTaskAttentionBackend([task]);
			backend.attentionByTask[task.id] = [
				{
					id: 'feed-1',
					principal: 'anonymous',
					workspace: 'default',
					task_id: task.id,
					item_type: 'approval',
					title: 'Approval requested',
					status: 'needs_action',
					created_at: Date.now() - 60_000,
					updated_at: Date.now() - 60_000,
					actions: [],
					metadata: {},
					hitl_request: {
						id: 'ccp-1',
						source: 'diff_approval',
						input_type: 'diff_approval',
						prompt: 'Apply 3 edits to schema.sql?'
					}
				}
			];
			installFetchMock([outputsRoute(task.id, []), ...backend.routes()]);

			render(TasksWorkspace, { navigationMode: 'local' });
			await screen.findByRole('button', { name: task.title });
			await user.click(screen.getByRole('button', { name: task.title }));

			const ask = await waitFor(() => {
				const found = panel().querySelector<HTMLElement>('.ask');
				if (found === null) throw new Error('no ask block on the panel');
				return found;
			});

			// The ask the verdict named, not some other row's.
			expect(ask.dataset.askSource).toBe('diff_approval');
			expect(ask).toHaveTextContent('Apply 3 edits to schema.sql?');
			// And something to press. A block with the prompt and no control is the
			// state this surface was already in.
			expect(ask.querySelector('button')).not.toBeNull();
		});
	});
});

/**
 * The list pager.
 *
 * Paging here is CLIENT-side over the whole loaded pool, on purpose — see the
 * long note on `TASKS_PAGE_SIZE` in `TasksWorkspace.svelte` for why the server
 * cannot page this list yet. What that makes worth asserting is the pair that a
 * pager can independently get wrong: which slice of cards is on screen, and
 * whether the numbers it prints describe that slice. A pager whose arrows work
 * while its summary counts the wrong pool is the failure mode, and it is
 * invisible until someone reads both at once.
 */
describe('TasksWorkspace pages the task list', () => {
	/** 30 ready tasks, newest first, so the store's updated_at sort is fixed. */
	function pagedBacklog(count: number): TaskBackendRecord[] {
		const base = Date.parse('2026-07-11T10:00:00.000Z');
		return Array.from({ length: count }, (_, index) =>
			backendTask(`paged-${String(index + 1).padStart(2, '0')}`, 'ready', {
				title: `Paged task ${String(index + 1).padStart(2, '0')}`,
				updated_at: new Date(base - index * 60_000).toISOString()
			})
		);
	}

	/**
	 * There are two pagers — above the list and below it — and they render the same
	 * derived numbers, so reading either proves the summary. This reads the top one
	 * and asserts the pair agree, because two copies of one control disagreeing is
	 * the failure a single-pager assertion could never see.
	 *
	 * They carry *distinct* landmark names on purpose: two navigation regions called
	 * the same thing are unhelpful to anyone listing them, and an ambiguous query
	 * here was how this surfaced.
	 */
	const PAGER_TOP = 'Task pages, above the list';
	const PAGER_BOTTOM = 'Task pages, below the list';

	/** One pager, scoped. Its controls share their names with the other pager's. */
	function pager(name: string) {
		return within(screen.getByRole('navigation', { name }));
	}

	function pagerSummary(): string {
		const read = (name: string) =>
			screen.getByRole('navigation', { name }).textContent!.replace(/\s+/g, ' ').trim();
		const top = read(PAGER_TOP);
		expect(read(PAGER_BOTTOM), 'the two pagers disagree').toBe(top);
		return top;
	}

	it('shows a first page of 25 whose summary counts the filtered pool, not the page', async () => {
		const user = userEvent.setup();
		backend = createTaskAttentionBackend(pagedBacklog(30));
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: 'Paged task 01' });

		// 30 tasks, 25 to a page: the boundary card is on page one and the one
		// after it is not, which is what makes this a page and not a filter.
		expect(screen.getByRole('button', { name: 'Paged task 25' })).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Paged task 26' })).not.toBeInTheDocument();
		expect(pagerSummary()).toContain('Page 1 of 2');
		expect(pagerSummary()).toContain('1-25 of 30');

		await user.click(pager(PAGER_TOP).getByRole('button', { name: 'Next page' }));

		// The list actually changed hands — page one's cards are gone, not merely
		// joined by page two's.
		await screen.findByRole('button', { name: 'Paged task 26' });
		expect(screen.queryByRole('button', { name: 'Paged task 25' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Paged task 01' })).not.toBeInTheDocument();
		expect(pagerSummary()).toContain('Page 2 of 2');
		expect(pagerSummary()).toContain('26-30 of 30');
	});

	it('counts the lane it is paging, and returns to page one when the lane changes', async () => {
		const user = userEvent.setup();
		// 30 ready + 4 completed. `All` is the non-completed lane, so the pager
		// must describe 30 while the toolbar still knows about all 34.
		backend = createTaskAttentionBackend([
			...pagedBacklog(30),
			...Array.from({ length: 4 }, (_, index) =>
				backendTask(`done-${index}`, 'completed', {
					title: `Finished task ${index}`,
					last_completed_root_execution_id: `execution-done-${index}`
				})
			)
		]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: 'Paged task 01' });
		expect(pagerSummary()).toContain('1-25 of 30');

		await user.click(pager(PAGER_TOP).getByRole('button', { name: 'Next page' }));
		await screen.findByRole('button', { name: 'Paged task 26' });

		// Completed is 4 — under one page — so switching lanes must retire the
		// pager entirely rather than strand the view on a page that cannot exist.
		await user.click(screen.getByRole('button', { name: /^Completed/ }));
		await screen.findByRole('button', { name: 'Finished task 0' });
		// Both retire together — a surface with one pager and not the other reads as a
		// rendering fault.
		expect(screen.queryByRole('navigation', { name: PAGER_TOP })).not.toBeInTheDocument();
		expect(screen.queryByRole('navigation', { name: PAGER_BOTTOM })).not.toBeInTheDocument();

		// Back to a lane that pages: page one, not the page two we left on.
		await user.click(screen.getByRole('button', { name: /^All/ }));
		await screen.findByRole('button', { name: 'Paged task 01' });
		expect(pagerSummary()).toContain('Page 1 of 2');
		expect(pagerSummary()).toContain('1-25 of 30');
	});
});

/**
 * The one thing a task list row can say about a mid-run block without a
 * per-row request: a staged diff is waiting on the reader.
 *
 * `TaskListItemV3.awaiting_diff_approval` is derived server-side from the
 * proposal store on every read, so it survives the restart that loses the HITL
 * event, and it is already `false` for a terminal task whose `Pending`
 * proposal is merely orphaned. Both of those decisions are the server's; what
 * the client can independently get wrong is whether the flag reaches a row at
 * all, and whether the reader can see it without hovering.
 */
describe('TasksWorkspace flags a task waiting on a diff approval', () => {
	/** The awaiting-diff chip inside one card, or `null` when the row is quiet. */
	function diffChip(title: string): HTMLElement | null {
		return within(cardFor(title)).queryByText('Review changes');
	}

	it('shows the flag only on the row the server flagged', async () => {
		const staged = backendTask('staged', 'paused', {
			title: 'Refactor the ingest path',
			awaiting_diff_approval: true
		});
		// No `awaiting_diff_approval` key at all — the wire shape of every task
		// that is not holding a staged diff, since the server skips the field
		// while it is false. A client reading absence as anything but `false`
		// paints "someone must look at this" across a list that is fine.
		const quiet = backendTask('quiet', 'paused', { title: 'Summarise the inbox' });
		// Terminal, and therefore never flagged: the server answers `false` here
		// even when a `Pending` proposal is still on disk. This asserts the row
		// is quiet, NOT that the client suppresses it — the client does not
		// re-derive terminality, because that would be a second copy of a rule
		// that already reached us as data (the same call `runAttentionFrom`
		// makes about `hitl_request`). A terminal row arriving with `true` would
		// render the chip; the server is what stops that happening.
		const finished = backendTask('finished', 'failed', { title: 'Migrate the index' });
		backend = createTaskAttentionBackend([staged, quiet, finished]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: staged.title });

		expect(diffChip(staged.title)).toBeInTheDocument();
		expect(diffChip(quiet.title)).toBeNull();
		expect(diffChip(finished.title)).toBeNull();
	});

	/**
	 * **The placement is the feature.** The card's chip row —
	 * `.presto-task-reveal-row` — is `max-height: 0; opacity: 0` until the card
	 * is hovered or focused, which is right for a due date and wrong for a claim
	 * on the reader's attention: a cue you only see after looking is not a cue.
	 *
	 * Asserting on a class name rather than on what the reader sees is the
	 * compromise, and it is deliberate — jsdom computes no layout, so
	 * `toBeVisible()` cannot see a collapsed ancestor, and the hover collapse is
	 * keyed on exactly this class. The class IS the mechanism here, not a
	 * stylistic detail standing in for one.
	 */
	it('puts the flag in the always-visible row, not the hover-revealed chip row', async () => {
		const staged = backendTask('staged', 'paused', {
			title: 'Refactor the ingest path',
			awaiting_diff_approval: true
		});
		backend = createTaskAttentionBackend([staged]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: staged.title });

		const chip = diffChip(staged.title);
		expect(chip).not.toBeNull();
		expect(chip!.closest('.presto-task-reveal-row')).toBeNull();
		// The status chip is the control: it lives in the collapsed row, which is
		// what makes this assertion mean something rather than pass by accident.
		const status = within(cardFor(staged.title)).getByText('Paused', {
			selector: '.native-task-chip--status'
		});
		expect(status.closest('.presto-task-reveal-row')).not.toBeNull();
	});

	it('displays recurring badge and ensures single Result button on task cards with results', async () => {
		const recurringTask = backendTask('recurring-1', 'completed', {
			title: 'Daily backup job',
			schedule: { cron: '0 0 * * *' },
			completion_summary: 'Backup succeeded.'
		});
		backend = createTaskAttentionBackend([recurringTask]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		const user = userEvent.setup();
		await user.click(screen.getByRole('button', { name: /^Completed/ }));
		await screen.findByRole('button', { name: recurringTask.title });

		const card = cardFor(recurringTask.title);
		expect(within(card).getByText('↻ Recurring')).toBeInTheDocument();

		// Should have exactly ONE "Result" button on the card
		const resultButtons = within(card).getAllByRole('button', { name: 'Result' });
		expect(resultButtons).toHaveLength(1);
	});

	it('does not display Result button when a task has no results', async () => {
		const emptyTask = backendTask('empty-task', 'completed', {
			title: 'Check pantry supplies'
		});
		backend = createTaskAttentionBackend([emptyTask]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		const user = userEvent.setup();
		await user.click(screen.getByRole('button', { name: /^Completed/ }));
		await screen.findByRole('button', { name: emptyTask.title });

		const card = cardFor(emptyTask.title);
		expect(within(card).queryByRole('button', { name: 'Result' })).not.toBeInTheDocument();
	});

	it('does not duplicate Reset to Ready between top right and bottom panel', async () => {
		const failedTask = backendTask('failed-task', 'failed', {
			title: 'Deploy to staging',
			execution_id: 'exec-failed-1'
		});
		backend = createTaskAttentionBackend([failedTask]);
		installFetchMock(backend.routes());

		render(TasksWorkspace, { navigationMode: 'local' });
		await screen.findByRole('button', { name: failedTask.title });

		const card = cardFor(failedTask.title);
		// Top right primary action has Reset to Ready
		const resetButtons = within(card).getAllByRole('button', { name: 'Reset to Ready' });
		expect(resetButtons).toHaveLength(1);
		expect(resetButtons[0]).toHaveClass('presto-task-primary-action');
	});
});
