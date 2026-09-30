import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import InternalTasksWorkspace from './InternalTasksWorkspace.svelte';

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$app/stores', () => ({
	page: {
		subscribe(run: (value: { url: URL }) => void) {
			run({ url: new URL('http://localhost/tasks?type=internal') });
			return () => {};
		}
	}
}));

const listInternalTasks = vi.hoisted(() => vi.fn());
const deleteInternalTask = vi.hoisted(() => vi.fn());
const fetchInternalTaskDetails = vi.hoisted(() => vi.fn());
const openInternalTaskOutputFile = vi.hoisted(() => vi.fn());
const revealInternalTaskOutputFile = vi.hoisted(() => vi.fn());
const retryInternalTaskSynthesis = vi.hoisted(() => vi.fn());
const openAuthenticatedTaskOutput = vi.hoisted(() => vi.fn());
vi.mock('$lib/internalTasks/api', () => ({
	listInternalTasks,
	cancelInternalExecution: vi.fn(),
	deleteInternalTask,
	fetchInternalTaskDetails,
	fetchInternalTaskExecutionPanel: vi.fn().mockResolvedValue(null),
	internalTaskOutputDownloadUrl: vi.fn(() => 'http://localhost/download'),
	openInternalTaskOutputFile,
	revealInternalTaskOutputFile,
	retryInternalTaskSynthesis
}));
vi.mock('$lib/magician/tasks/taskOutputs', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/magician/tasks/taskOutputs')>()),
	openAuthenticatedTaskOutput
}));

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
	vi.unstubAllGlobals();
});

describe('Internal Tasks execution controls', () => {
	it('routes app runs to Apps without generic stop or delete controls', async () => {
		listInternalTasks.mockResolvedValue({
			tasks: [{
				id: `task_app_${'a'.repeat(64)}`, title: 'Town Square roster',
				agent_id: 'personal-assistant', status: 'running', lifecycle: 'persistent',
				created_at: '2026-09-08T00:00:00Z', updated_at: '2026-09-08T00:01:00Z',
				active_root_execution_id: 'exec-app', synthesis_failed_execution_id: 'exec-app-synthesis'
			}],
			pagination: { total: 1, limit: 50, offset: 0, has_more: false }
		});
		render(InternalTasksWorkspace);
		expect(await screen.findByRole('link', { name: 'Open Apps' })).toHaveAttribute('href', '/apps');
		expect(screen.queryByRole('button', { name: 'Delete' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Stop' })).not.toBeInTheDocument();
		expect(screen.queryByRole('button', { name: /retry.*synthesis/i })).not.toBeInTheDocument();
		expect(deleteInternalTask).not.toHaveBeenCalled();
	});

	it('renders capability-driven controls beside the existing task Stop action', async () => {
		listInternalTasks.mockResolvedValue({
			tasks: [
				{
					id: 'internal-1',
					title: 'Background research',
					description: 'Research task',
					agent_id: 'researcher',
					status: 'running',
					created_at: '2026-07-13T00:00:00Z',
					updated_at: '2026-07-13T00:01:00Z',
					active_root_execution_id: 'exec-internal'
				}
			],
			pagination: { total: 1, limit: 50, offset: 0, has_more: false }
		});
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(
					JSON.stringify({
						execution_id: 'exec-internal',
						waiting_state: 'executing',
						active: true,
						can_pause: true,
						can_resume: false,
						can_steer: true,
						can_cancel: true
					}),
					{ status: 200, headers: { 'Content-Type': 'application/json' } }
				)
			)
		);

		render(InternalTasksWorkspace);

		expect(await screen.findByRole('button', { name: 'Steer run' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Pause run' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Stop' })).toBeEnabled();
		expect(screen.queryByRole('button', { name: 'Stop run' })).not.toBeInTheDocument();
	});
});

/**
 * The panel-opening cases. They assert what only the composed thing can be
 * wrong about — which task's data reaches the panel, which acts it declares,
 * and which file a click acts on — since the adapter and the panel are each
 * already tested and neither proves the joint.
 */
const INTERNAL_TASK = {
	id: 'internal-7',
	title: 'Reconcile the ledger',
	description: 'A runtime-spawned task',
	agent_id: 'reconciler',
	status: 'completed',
	created_at: '2026-07-13T00:00:00Z',
	updated_at: '2026-07-13T00:05:00Z',
	latest_root_execution_id: 'exec-root'
};

/**
 * Two outputs sharing a basename and differing only by directory, because a
 * handler resolving a path from the display name opens the wrong file and a
 * fixture with distinct names never notices.
 */
const INTERNAL_DETAILS = {
	task: {
		refs: {
			outputs: [
				{ relative_path: 'alpha/report.md', media_type: 'text/markdown' },
				{ relative_path: 'beta/report.md', media_type: 'text/markdown' }
			]
		}
	},
	executions: [
		{
			state: {
				execution_id: 'exec-root',
				status: 'completed',
				started_at: '2026-07-13T00:00:00Z',
				completed_at: '2026-07-13T00:03:00Z'
			},
			refs: {},
			artifacts: []
		}
	]
};

function listOne(task: Record<string, unknown> = {}) {
	listInternalTasks.mockResolvedValue({
		tasks: [{ ...INTERNAL_TASK, ...task }],
		pagination: { total: 1, limit: 50, offset: 0, has_more: false }
	});
}

const titleButton = () => screen.getByRole('button', { name: /Reconcile the ledger/ });

/** Open the panel the way a reader does: click the task's title. */
async function openPanel(): Promise<HTMLElement> {
	render(InternalTasksWorkspace);
	const title = await screen.findByRole('button', { name: /Reconcile the ledger/ });
	// A real click focuses what it hits and `fireEvent` does not, which is
	// exactly what the drawer restores focus to when it closes.
	title.focus();
	await fireEvent.click(title);
	return await screen.findByRole('dialog', { name: 'Task panel' });
}

const actIds = (panel: HTMLElement) =>
	Array.from(panel.querySelectorAll<HTMLElement>('section[data-act]')).map(
		(section) => section.dataset.act
	);

describe('Internal Tasks open the unified task panel', () => {
	it('shows completion-based scheduling without promising a run while the current one is active', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue({ ...INTERNAL_DETAILS, recurring_schedule: {
			behavior_id: 'ambient_turn', interval_seconds: 300, next_due_at: null,
			latest_status: 'running', waiting_for_settlement: true
		} });
		const panel = await openPanel();
		expect(await within(panel).findByText(/Every 5 minutes after completion.*Waiting for this run to settle/)).toBeInTheDocument();
	});

	it('recurring execution history loads an older page only when requested', async () => {
		listOne();
		fetchInternalTaskDetails.mockImplementation((_task, _principal, _workspace, cursor) => Promise.resolve({
			...INTERNAL_DETAILS,
			next_execution_cursor: cursor ? undefined : 'older-page',
			execution_total: 2,
			executions: cursor ? [] : INTERNAL_DETAILS.executions
		}));
		await openPanel();
		const more = await screen.findByRole('button', { name: 'Load older executions' });
		expect(fetchInternalTaskDetails.mock.calls.every(call => call[3] === undefined)).toBe(true);
		await fireEvent.click(more);
		await waitFor(() => expect(fetchInternalTaskDetails).toHaveBeenCalledWith(
			'internal-7', expect.any(String), expect.any(String), 'older-page'));
		await waitFor(() => expect(screen.queryByRole('button', { name: 'Load older executions' })).not.toBeInTheDocument());
	});

	it('opens the shared side panel when a non-control part of the row is clicked', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue(INTERNAL_DETAILS);
		render(InternalTasksWorkspace);

		const agentCell = (await screen.findByText('reconciler')).closest('td');
		if (!agentCell) throw new Error('agent cell must render');
		await fireEvent.click(agentCell);

		expect(await screen.findByRole('dialog', { name: 'Task panel' })).toBeInTheDocument();
		expect(fetchInternalTaskDetails).toHaveBeenCalledWith(
			'internal-7',
			expect.any(String),
			expect.any(String)
		);
	});

	it('opens the panel focused on the Output act when Result button is clicked', async () => {
		listOne({ status: 'running', completion_summary: 'Partial summary' });
		fetchInternalTaskDetails.mockResolvedValue({
			...INTERNAL_DETAILS,
			executions: [
				{
					state: {
						execution_id: 'exec-root',
						status: 'running',
						started_at: '2026-07-13T00:00:00Z',
						completed_at: null
					},
					refs: {},
					artifacts: []
				}
			]
		});
		render(InternalTasksWorkspace);

		const resultBtn = await screen.findByRole('button', { name: 'Result' });
		expect(resultBtn).toBeInTheDocument();
		await fireEvent.click(resultBtn);

		const dialog = await screen.findByRole('dialog', { name: 'Task panel' });
		expect(dialog).toBeInTheDocument();

		await waitFor(() => {
			const outputAct = dialog.querySelector('section[data-act="output"]');
			expect(outputAct).toHaveClass('act--open');
		});
	});

	/**
	 * The proof this whole design owes: both task kinds render through one
	 * component, differing **only in declared capabilities**. An internal task
	 * has no plan, so the Plan act is absent — never present and greyed, which
	 * would be the type check sneaking back in through styling (design §4).
	 */
	it('renders the panel with no Plan act, because an internal task has no plan', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue(INTERNAL_DETAILS);

		const panel = await openPanel();
		await waitFor(() => expect(actIds(panel)).toEqual(['run', 'output']));

		expect(panel.querySelector('[data-verdict-state]')).toHaveAttribute(
			'data-verdict-state',
			'finished'
		);
		expect(fetchInternalTaskDetails).toHaveBeenCalledWith(
			'internal-7',
			expect.any(String),
			expect.any(String)
		);
	});

	/**
	 * The seam for the written report, on the surface that never had one: the
	 * row's `completion_summary` has always been on this wire — these rows are
	 * the same shape the `/tasks` list sends — and nothing on this route read it.
	 * The adapter's own suite proves the field reaches the model; this proves the
	 * model reaches the page as rendered markdown.
	 */
	it('renders the task\'s written report inside the Output act', async () => {
		listOne({ completion_summary: '## Ledger\n\n- Balanced to the cent' });
		fetchInternalTaskDetails.mockResolvedValue(INTERNAL_DETAILS);

		const panel = await openPanel();

		await waitFor(() =>
			expect(panel.querySelector('.output-summary h2')?.textContent).toBe('Ledger')
		);
		expect(panel.querySelector('.output-summary li')?.textContent).toBe('Balanced to the cent');
		// The source, not the render: markdown printed verbatim would contain this.
		expect(panel.textContent).not.toContain('## Ledger');
	});

	/**
	 * The other half of the same seam. The adapter cannot mint an address, so the
	 * workspace passes a minter. Authenticated output actions must retain each
	 * row's path even though the bearer is deliberately absent from the URL.
	 */
	it('gives each output row a bearer-safe address built from its own path', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue(INTERNAL_DETAILS);

		const panel = await openPanel();
		const rows = await waitFor(() => {
			const found = panel.querySelectorAll<HTMLElement>('.output-file');
			expect(found.length).toBe(2);
			return found;
		});

		// Two rows share a basename, so the two addresses have to differ by
		// directory — which is the case a minter keyed on the display name would
		// collapse.
		await fireEvent.click(within(rows[0]).getByRole('button', { name: 'In tab report.md' }));
		await fireEvent.click(within(rows[1]).getByRole('button', { name: 'In tab report.md' }));
		expect(openAuthenticatedTaskOutput.mock.calls[0]?.[0]).toContain(
			'/api/magician/v3/tasks/internal-7/outputs/alpha/report.md'
		);
		expect(openAuthenticatedTaskOutput.mock.calls[1]?.[0]).toContain(
			'/api/magician/v3/tasks/internal-7/outputs/beta/report.md'
		);
	});

	it('switches run-owned outputs and artifacts without relabelling task deliverables', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue({
			...INTERNAL_DETAILS,
			executions: [
				{
					state: {
						execution_id: 'exec-old',
						status: 'failed',
						started_at: '2026-07-12T23:00:00Z',
						completed_at: '2026-07-12T23:01:00Z'
					},
					refs: {
						output_refs: [
							{
								relative_path: 'executions/exec-old/outputs/attempt.md',
								media_type: 'text/markdown'
							}
						],
						child_output_refs: [
							{
								relative_path: 'executions/exec-child/outputs/evidence.json',
								media_type: 'application/json'
							}
						]
					},
					artifacts: [
						{
							artifact_id: 'artifact-observation',
							artifact_type: 'browser_observation',
							content_type: 'application/json',
							produced_at: '2026-07-12T23:00:30Z',
							payload: { url: 'https://example.test' }
						}
					]
				},
				{
					...INTERNAL_DETAILS.executions[0],
					refs: {
						output_refs: [
							{
								relative_path: 'executions/exec-root/outputs/final.md',
								media_type: 'text/markdown'
							}
						]
					}
				}
			]
		});

		const panel = await openPanel();
		await waitFor(() => expect(panel.textContent).toContain('final.md'));
		expect(panel.textContent).toContain('Task deliverables');
		expect(panel.textContent).not.toContain('attempt.md');

		const picker = panel.querySelector('.run-picker select') as HTMLSelectElement;
		await fireEvent.change(picker, { target: { value: 'exec-old' } });

		await waitFor(() => expect(panel.textContent).toContain('attempt.md'));
		expect(panel.textContent).toContain('evidence.json');
		expect(panel.textContent).toContain('artifact-observation');
		// Stable task deliverables remain visible while the run-owned groups change.
		expect(panel.textContent).toContain('report.md');
		expect(panel.querySelectorAll('[data-output-scope="task"]')).toHaveLength(2);
		expect(panel.querySelectorAll('[data-output-scope="execution"]')).toHaveLength(1);
		expect(panel.querySelectorAll('[data-output-scope="delegated"]')).toHaveLength(1);
	});

	/**
	 * The verdict's second line for a finished task is composed from the Output
	 * act, so this is also the assertion that the act reached the panel with its
	 * files rather than empty.
	 */
	it('reads the output back in the verdict rather than reporting a count', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue(INTERNAL_DETAILS);

		const panel = await openPanel();
		await waitFor(() =>
			// Both files' own names, because `outputSummary` names both when there
			// are exactly two — and they are the same word, which is the fixture's
			// point rather than a mistake.
			expect(panel.querySelector('.verdict__detail')?.textContent).toBe(
				'Wrote report.md and report.md'
			)
		);
	});

	/**
	 * The index is what makes the dispatch usable: both rows read `report.md`, so
	 * a handler resolving the path from the name would open the first one
	 * whichever was clicked.
	 */
	it('opens the file the clicked row names, resolved by position and not by name', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue(INTERNAL_DETAILS);

		const panel = await openPanel();
		// `button.` and not `.output-file__action` alone: the row also carries two
		// **links** in that class — open-in-tab and download, which the browser
		// performs and the workspace never hears about. Selecting the class alone
		// would count eight elements and index into the wrong one.
		const rows = await waitFor(() => {
			const found = panel.querySelectorAll<HTMLElement>('.output-file');
			expect(found.length).toBe(2);
			return found;
		});
		// **The row first, then the control by its name.** This counted
		// `button.output-file__action` across the whole act and indexed into the
		// flat list; it broke the moment a row grew a fifth control — the preview
		// disclosure — while the property under test was untouched. Both rows are
		// still called `report.md`, so a global `getByRole` cannot tell them
		// apart; narrowing to the row and then naming the control does, and
		// survives whatever else the row gains.
		const second = rows[1];
		const openSecond = second.querySelector<HTMLButtonElement>('button[aria-label^="Open "]')!;
		const revealSecond = second.querySelector<HTMLButtonElement>('button[aria-label^="Reveal "]')!;

		// The second row's Open, then its Reveal.
		await fireEvent.click(openSecond);
		await waitFor(() =>
			expect(openInternalTaskOutputFile).toHaveBeenCalledWith(
				'internal-7',
				'beta/report.md',
				expect.any(String),
				expect.any(String)
			)
		);

		await fireEvent.click(revealSecond);
		await waitFor(() =>
			expect(revealInternalTaskOutputFile).toHaveBeenCalledWith(
				'internal-7',
				'beta/report.md',
				expect.any(String),
				expect.any(String)
			)
		);
	});

	/**
	 * Where `retryInternalTaskSynthesis` landed, and the line that explains why
	 * the control is there. Without the ask, this task would read
	 * `Finished · Produced no output` — false twice over.
	 */
	it('offers Retry synthesis in the panel chrome, with the verdict saying why', async () => {
		listOne({ synthesis_failed_execution_id: 'exec-root' });
		fetchInternalTaskDetails.mockResolvedValue({
			...INTERNAL_DETAILS,
			task: { refs: { outputs: [] } },
			executions: [
				{
					...INTERNAL_DETAILS.executions[0],
					state: {
						...INTERNAL_DETAILS.executions[0].state,
						synthesis_failed: { failed_at: '2026-07-13T00:04:00Z' }
					}
				}
			]
		});

		const panel = await openPanel();
		expect(panel.querySelector('[data-verdict-state]')).toHaveAttribute(
			'data-verdict-state',
			'waiting'
		);
		expect(panel.querySelector('.verdict__detail')?.textContent).toMatch(
			/Output synthesis failed/
		);

		await fireEvent.click(screen.getByRole('button', { name: 'Retry synthesis' }));
		await waitFor(() =>
			expect(retryInternalTaskSynthesis).toHaveBeenCalledWith(
				'internal-7',
				'exec-root',
				expect.any(String),
				expect.any(String)
			)
		);
	});

	/**
	 * The joint the shell's own suite cannot make: that this surface opens the
	 * shared drawer at all, and therefore gets its focus handling. The drawer is
	 * `aria-modal`, so a caret left on the row behind it is walking a list
	 * assistive tech has just been told is not there.
	 */
	it('hands focus to the drawer on open and back to the row on close', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue(INTERNAL_DETAILS);

		const panel = await openPanel();
		expect(document.activeElement).toBe(panel);

		await fireEvent.click(screen.getByRole('button', { name: 'Close task panel' }));
		await waitFor(() => expect(document.activeElement).toBe(titleButton()));
	});

	/**
	 * Escape is the shell's now, not this workspace's. Asserted here rather than
	 * only in the shell's suite because the shell closing a drawer nobody mounted
	 * proves nothing — this is the surface actually losing its drawer to the key.
	 */
	it('closes the drawer on Escape', async () => {
		listOne();
		fetchInternalTaskDetails.mockResolvedValue(INTERNAL_DETAILS);

		await openPanel();
		await fireEvent.keyDown(window, { key: 'Escape' });

		await waitFor(() =>
			expect(screen.queryByRole('dialog', { name: 'Task panel' })).not.toBeInTheDocument()
		);
	});

	/**
	 * Design §6, case 3: the panel keeps showing what it has and says so, rather
	 * than presenting stale state as current or rendering a network failure as a
	 * failed task.
	 */
	it('says it is showing last known state when the details refresh fails', async () => {
		listOne();
		fetchInternalTaskDetails.mockRejectedValue(new Error('details unreachable'));

		const panel = await openPanel();
		await waitFor(() =>
			expect(panel.querySelector('.panel__stale')?.textContent).toMatch(/last known state/)
		);
		expect(panel.querySelector('[data-verdict-state]')).toHaveAttribute(
			'data-verdict-state',
			'finished'
		);
		// The acts come from the details, so a failed read leaves none rather than
		// rendering an empty Output act that asserts `no output`.
		expect(actIds(panel)).toEqual([]);
	});
});

/**
 * The offset pager's half of the shared removal policy
 * (`$lib/shared/components/pageAfterRemoval`). What only the composed thing can
 * be wrong about here is the offset the delete leaves behind: the helper is
 * already tested against a fake corpus, and the table is what turns a stale
 * offset into an empty page under a pager that still counts it.
 */
describe('Internal Tasks paging after a delete', () => {
	/** A server backed by a live corpus, so a delete really does shorten it. */
	function serveCorpus(ids: string[]): { corpus: string[] } {
		const corpus = [...ids];
		listInternalTasks.mockImplementation(
			async ({ limit, offset }: { limit: number; offset: number }) => ({
				tasks: corpus.slice(offset, offset + limit).map((id, index) => ({
					...INTERNAL_TASK,
					id,
					title: `Task ${id}`,
					latest_root_execution_id: `exec-${index}`
				})),
				pagination: {
					total: corpus.length,
					limit,
					offset,
					has_more: offset + limit < corpus.length
				}
			})
		);
		deleteInternalTask.mockImplementation(async (taskId: string) => {
			const at = corpus.indexOf(taskId);
			if (at >= 0) corpus.splice(at, 1);
		});
		return { corpus };
	}

	function lastListedOffset(): number {
		const call = listInternalTasks.mock.calls.at(-1);
		return (call?.[0] as { offset: number }).offset;
	}

	it('steps back when the deleted row was the only one on the last page', async () => {
		// 51 rows at the default page size of 50: page 2 holds exactly one.
		serveCorpus(Array.from({ length: 51 }, (_, i) => `internal-${i}`));
		vi.stubGlobal('confirm', vi.fn(() => true));

		render(InternalTasksWorkspace);
		await screen.findByRole('button', { name: /Task internal-0/ });

		await fireEvent.click(screen.getAllByRole('button', { name: 'Next page' })[0]);
		await waitFor(() => expect(lastListedOffset()).toBe(50));
		await screen.findByRole('button', { name: /Task internal-50/ });

		await fireEvent.click(screen.getByRole('button', { name: 'Delete' }));

		// The re-read of offset 50 comes back empty, so the reader lands on the
		// page that still has rows instead of on a blank table under "Page 2 of 2".
		await waitFor(() => expect(lastListedOffset()).toBe(0));
		await screen.findByRole('button', { name: /Task internal-0/ });
		for (const previous of screen.getAllByRole('button', { name: 'Previous page' })) {
			expect(previous).toBeDisabled();
		}
	});

	it('stays put when the page it emptied was not the last one', async () => {
		// 101 rows: deleting one off page 2 still leaves 49 there, and the row
		// that moved forward from page 3 is only reachable by re-reading offset 50.
		serveCorpus(Array.from({ length: 101 }, (_, i) => `internal-${i}`));
		vi.stubGlobal('confirm', vi.fn(() => true));

		render(InternalTasksWorkspace);
		await screen.findByRole('button', { name: /Task internal-0/ });

		await fireEvent.click(screen.getAllByRole('button', { name: 'Next page' })[0]);
		await waitFor(() => expect(lastListedOffset()).toBe(50));
		// Page 2 is internal-50…internal-99; internal-100 is still on page 3.
		await screen.findByRole('button', { name: /Task internal-50$/ });
		expect(screen.queryByRole('button', { name: /Task internal-100/ })).not.toBeInTheDocument();

		const before = listInternalTasks.mock.calls.length;
		await fireEvent.click(screen.getAllByRole('button', { name: 'Delete' })[0]);

		await waitFor(() => expect(listInternalTasks.mock.calls.length).toBeGreaterThan(before));
		expect(lastListedOffset()).toBe(50);
		// The row pulled forward off page 3 is on screen: a local filter alone
		// would have left a 49-row page with a gap where it should be.
		expect(await screen.findByRole('button', { name: /Task internal-100/ })).toBeInTheDocument();
	});
});

/**
 * Runtime-spawned coding work lands on this surface and nowhere else on
 * `/tasks`, so it is the list most likely to be holding a staged diff and the
 * least likely to be watched. The row's `awaiting_diff_approval` is the only
 * thing here that says a run stopped for a human rather than for the machine —
 * `paused` alone reads the same either way, and the synthesis pills next to it
 * mean the opposite ("still busy, wait").
 */
describe('Internal Tasks flag a task waiting on a diff approval', () => {
	/** The `<tr>` a title belongs to, so one row cannot answer for another. */
	function rowFor(title: string): HTMLElement {
		const row = screen.getByRole('button', { name: title }).closest('tr');
		if (!row) throw new Error(`Task row not found: ${title}`);
		return row as HTMLElement;
	}

	it('marks the flagged row and leaves the rest quiet', async () => {
		listInternalTasks.mockResolvedValue({
			tasks: [
				{
					...INTERNAL_TASK,
					id: 'staged',
					title: 'Refactor the ingest path',
					status: 'paused',
					awaiting_diff_approval: true
				},
				// No `awaiting_diff_approval` key: the server omits it while false,
				// so this is the shape of every ordinary row and the shape a client
				// reading absence as "unknown" would get wrong.
				{
					...INTERNAL_TASK,
					id: 'quiet',
					title: 'Summarise the inbox',
					status: 'paused'
				},
				// Terminal. The server answers `false` for these even with a
				// `Pending` proposal still on disk, because that proposal is
				// orphaned rather than actionable. The client renders the row's
				// answer and does not re-derive the rule — so what this pins is the
				// wire contract holding, not a guard in this component.
				{
					...INTERNAL_TASK,
					id: 'finished',
					title: 'Migrate the index',
					status: 'failed'
				}
			],
			pagination: { total: 3, limit: 50, offset: 0, has_more: false }
		});

		render(InternalTasksWorkspace);
		await screen.findByRole('button', { name: 'Refactor the ingest path' });

		expect(within(rowFor('Refactor the ingest path')).getByText('review changes')).toBeInTheDocument();
		expect(within(rowFor('Summarise the inbox')).queryByText('review changes')).toBeNull();
		expect(within(rowFor('Migrate the index')).queryByText('review changes')).toBeNull();
	});

	it('renders recurring indicator for recurring internal tasks', async () => {
		listInternalTasks.mockResolvedValue({
			tasks: [
				{
					...INTERNAL_TASK,
					id: 'task-recurring-1',
					title: 'Recurring hourly sync',
					tags: [{ id: 'app_recurring', name: 'app_recurring' }]
				},
				{
					...INTERNAL_TASK,
					id: 'task_app_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',
					title: 'App launch run',
					tags: [{ id: 'app', name: 'app' }]
				}
			],
			pagination: { total: 2, limit: 50, offset: 0, has_more: false }
		});

		render(InternalTasksWorkspace);
		await screen.findByRole('button', { name: /Recurring hourly sync/ });

		const recurringRow = rowFor('Recurring hourly sync');
		expect(within(recurringRow).getByLabelText('Recurring task')).toBeInTheDocument();
		expect(within(recurringRow).getByText('↻')).toBeInTheDocument();

		const appRow = rowFor('App launch run');
		expect(within(appRow).queryByLabelText('Recurring task')).toBeNull();
		const taskIdElem = appRow.querySelector('.task-id');
		expect(taskIdElem).toBeInTheDocument();
		expect(taskIdElem?.textContent).toBe('task_app_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef');
	});
});

