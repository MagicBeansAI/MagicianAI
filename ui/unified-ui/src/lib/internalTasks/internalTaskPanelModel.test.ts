import { describe, expect, it } from 'vitest';

import { outputSummary } from '$lib/magician/tasks/actSummaries';
import { deriveVerdict } from '$lib/magician/tasks/taskVerdict';

import type { ExecutionPanelState } from '$lib/types/executionPanel';

import type {
	InternalExecutionDetails,
	InternalTaskDetails,
	InternalTaskListItem
} from './api';
import {
	internalTaskOutputFiles,
	toInternalTaskPanelModel,
	SYNTHESIS_FAILED_SUMMARY
} from './internalTaskPanelModel';

/**
 * The seam's fixture. Every value that could stand in for another is different,
 * so an adapter wiring the wrong field through has to fail:
 *
 * | | |
 * |---|---|
 * | run started | 600_000 |
 * | progress last moved | 700_000 |
 * | run completed | 780_000 |
 * | synthesis gave up | 820_000 |
 * | row last written | 900_000 |
 * | now | 1_000_000 |
 *
 * Two of the three outputs share a basename and differ only by directory, for
 * the same reason the panel's own suite does it: a handler resolving a path from
 * the display name opens the wrong file, and a fixture with three distinct names
 * never notices.
 */
const NOW = 1_000_000;
const STARTED_AT = new Date(600_000).toISOString();
const PROGRESS_AT = new Date(700_000).toISOString();
const COMPLETED_AT = new Date(780_000).toISOString();
const SYNTHESIS_FAILED_AT = new Date(820_000).toISOString();

const OUTPUT_REFS = [
	{ relative_path: 'alpha/report.md', media_type: 'text/markdown' },
	{ relative_path: 'beta/report.md', media_type: 'text/markdown' },
	{ relative_path: 'charts/revenue.png', media_type: 'image/png' }
];

function internalTask(overrides: Partial<InternalTaskListItem> = {}): InternalTaskListItem {
	return {
		id: 'internal-7',
		title: 'Reconcile the ledger',
		description: 'A runtime-spawned task',
		agent_id: 'reconciler',
		status: 'completed',
		created_at: new Date(500_000).toISOString(),
		updated_at: new Date(900_000).toISOString(),
		latest_root_execution_id: 'exec-root',
		last_progress_at: PROGRESS_AT,
		...overrides
	};
}

function execution(
	state: Record<string, unknown> = {}
): InternalExecutionDetails {
	return {
		state: {
			execution_id: 'exec-root',
			status: 'completed',
			started_at: STARTED_AT,
			completed_at: COMPLETED_AT,
			...state
		},
		refs: {},
		artifacts: []
	};
}

function details(overrides: Partial<InternalTaskDetails> = {}): InternalTaskDetails {
	return {
		task: { refs: { outputs: OUTPUT_REFS } },
		executions: [execution()],
		...overrides
	};
}

describe('toInternalTaskPanelModel — the acceptance test, as a value', () => {
	/**
	 * The whole reason this task exists. The panel renders no Plan act for an
	 * internal task, and it learns that from a `null` slice rather than from
	 * being told what kind of task it is holding — design §2's "if anyone ever
	 * needs `if (internal)` inside the panel, the unification has failed".
	 */
	it('never gives an internal task a Plan act', () => {
		for (const status of ['pending', 'planning', 'running', 'completed', 'failed']) {
			expect(toInternalTaskPanelModel(internalTask({ status }), details(), NOW).plan).toBeNull();
		}
	});

	it("carries the row's own id, so a swap is distinguishable from a poll", () => {
		expect(toInternalTaskPanelModel(internalTask({ id: 'internal-42' }), null, NOW).id).toBe(
			'internal-42'
		);
	});
});

/**
 * The events an internal run recorded were on disk the whole time — the panel
 * simply never asked. `/tasks/{id}/details` projects no event log, this adapter
 * hardcoded `timeline = null`, and the Run act said "nothing observed" about
 * successful runs that had 20-30 events sitting in `events.jsonl`.
 *
 * `/tasks/{id}/execution-panel` does project one, and it resolves internal task
 * ids because `workspace.task_dir` probes `internal_tasks/` before `tasks/`.
 * So the fix is a payload, not a projection: the timeline derives through the
 * same `deriveTimeline` the task adapter uses.
 */
describe('toInternalTaskPanelModel — the event log', () => {
	function panelState(executionId = 'exec-root'): ExecutionPanelState {
		return {
			overview: { execution_id: executionId },
			run: {
				activity_log: [
					{
						id: 'v3:log:exec-root:1',
						status: 'info',
						created_at: 610_000,
						agent_id: 'internal-agent',
						metadata: { event_type: 'llm.succeeded', execution_id: executionId }
					}
				]
			}
		} as unknown as ExecutionPanelState;
	}

	it('reads the run event log when panel state is supplied', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details(),
			NOW,
			undefined,
			null,
			panelState()
		);
		expect(model.run?.timeline).toHaveLength(1);
		expect(model.run?.timeline?.[0]?.id).toBe('v3:log:exec-root:1');
	});

	/**
	 * `null`, not `[]`. "Nothing looked" and "the run did nothing" are different
	 * facts, and only the second is a claim this adapter is entitled to make.
	 */
	it('reports nothing observed when no panel state was read', () => {
		expect(toInternalTaskPanelModel(internalTask(), details(), NOW).run?.timeline).toBeNull();
	});

	/**
	 * The run picker can outrun the panel request. Rendering the other run's
	 * events under this one's heading would be undetectable on screen, so
	 * `deriveTimeline` refuses — the act says "nothing observed" until the
	 * matching payload arrives.
	 */
	it('refuses a panel payload describing a different run', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details(),
			NOW,
			undefined,
			null,
			panelState('exec-someone-else')
		);
		expect(model.run?.timeline).toBeNull();
	});
});

describe('toInternalTaskPanelModel — the status vocabulary', () => {
	const cases: Array<[string, string]> = [
		['running', 'running'],
		['failed', 'failed'],
		['cancelled', 'cancelled'],
		['completed', 'finished']
	];

	it.each(cases)('reads a %s task as %s', (status, state) => {
		const model = toInternalTaskPanelModel(
			// No progress instant: `running` would otherwise be old enough to stall.
			internalTask({ status, last_progress_at: null }),
			details(),
			NOW
		);
		expect(deriveVerdict({ ...model, now: NOW }).state).toBe(state);
	});

	/**
	 * The one that matters. `deriveVerdict`'s last branch is unconditional, so a
	 * status that reaches it unmapped reads `Finished` — the loudest possible lie
	 * about a task that has not started. The wire status goes through the store's
	 * own normaliser first, whose answer for anything unrecognised is `pending`.
	 */
	it('never lets a status nobody models read as finished', () => {
		for (const status of ['pending', 'planning', 'paused', 'deferred', 'sleeping', 'wat']) {
			const model = toInternalTaskPanelModel(internalTask({ status }), details(), NOW);
			const verdict = deriveVerdict({ ...model, now: NOW });
			expect(verdict.state, status).not.toBe('finished');
			expect(verdict.headline, status).not.toMatch(/finished/i);
		}
	});
});

describe('toInternalTaskPanelModel — absence', () => {
	/**
	 * The coercion this module owes. The wire spells absence by omitting a key
	 * and the pure modules spell it `null`; a model carrying `undefined` would
	 * satisfy no `=== null` check downstream.
	 */
	it('spells every absent field `null`, never `undefined`', () => {
		const model = toInternalTaskPanelModel(
			{
				id: 'internal-bare',
				title: 'Bare',
				description: '',
				agent_id: '',
				status: 'pending',
				created_at: '',
				updated_at: ''
			},
			null,
			NOW
		);

		expect(model.attention).toBeNull();
		expect(model.error).toBeNull();
		expect(model.currentStep).toBeNull();
		expect(model.totalSteps).toBeNull();
		expect(model.currentStepLabel).toBeNull();
		expect(model.elapsedMs).toBeNull();
		expect(model.lastProgressAt).toBeNull();
		expect(model.plan).toBeNull();
		expect(model.run).toBeNull();
		expect(model.output).toBeNull();

		for (const [field, value] of Object.entries(model)) {
			expect(value, `${field} is undefined rather than null`).not.toBeUndefined();
		}
	});

	/**
	 * Both acts an internal task can have are read out of `/details`, so a panel
	 * opened a moment ago shows the verdict alone rather than acts describing a
	 * run it has not seen — design §6's absent-not-empty, applied to a load that
	 * has not finished rather than one that failed.
	 */
	it('declares no Run or Output act until the details have been read', () => {
		const model = toInternalTaskPanelModel(internalTask(), null, NOW);
		expect(model.run).toBeNull();
		expect(model.output).toBeNull();
	});
});

describe('toInternalTaskPanelModel — progress', () => {
	it('reads the progress instant the row carries', () => {
		const model = toInternalTaskPanelModel(internalTask(), details(), NOW);
		expect(model.lastProgressAt).toBe(Date.parse(PROGRESS_AT));
	});

	/**
	 * The trap design §5 names, and the reason `parseTimestampToIso` is never
	 * used here: it answers *now* for anything it cannot read, which would turn
	 * "no progress recorded" into "just advanced" and make a wedged run
	 * permanently un-stallable. Asserted as `null` rather than "not now", because
	 * "not now" passes for any wrong number.
	 */
	it.each([undefined, null, '', 'not a timestamp', 0, -1])(
		'reads an unusable progress instant (%s) as nothing at all',
		(raw) => {
			const model = toInternalTaskPanelModel(
				internalTask({ last_progress_at: raw as unknown as string }),
				details(),
				NOW
			);
			expect(model.lastProgressAt).toBeNull();
		}
	);

	it('reports a wedged internal run as stalled, off the real instant', () => {
		const model = toInternalTaskPanelModel(
			internalTask({
				status: 'running',
				last_progress_at: new Date(NOW - 6 * 60_000).toISOString()
			}),
			details({ executions: [execution({ status: 'running', completed_at: null })] }),
			NOW
		);
		const verdict = deriveVerdict({ ...model, now: NOW });
		expect(verdict.state).toBe('stalled');
		expect(verdict.headline).toBe('Stalled · no progress for 6m');
	});
});

describe('toInternalTaskPanelModel — the Run act', () => {
	it('describes the execution the task names as its root, not the last one listed', () => {
		const model = toInternalTaskPanelModel(
			internalTask({ latest_root_execution_id: 'exec-root' }),
			details({
				executions: [execution(), execution({ execution_id: 'exec-child' })]
			}),
			NOW
		);
		expect(model.run?.provenance).toContainEqual({ label: 'Execution id', value: 'exec-root' });
	});

	it('has no run to describe when the id the task names is not among the executions', () => {
		const model = toInternalTaskPanelModel(
			internalTask({ latest_root_execution_id: 'exec-missing' }),
			details({ executions: [execution({ execution_id: 'exec-other' })] }),
			NOW
		);
		expect(model.run).toBeNull();
	});

	/**
	 * The task id is **not** here: the drawer's control row carries it, so a row
	 * behind this disclosure would be the same identifier twice in one panel.
	 *
	 * Nor is a cost total. `runCostRows` is called over this surface's timeline
	 * exactly as the task adapter calls it over its own — no `if (internal)`
	 * anywhere — and that timeline is `null`, so the sum is honestly absent rather
	 * than a row of zeros about a run whose events nothing here has read.
	 */
	it('carries the agent alongside the execution, and drops what is blank', () => {
		const model = toInternalTaskPanelModel(internalTask({ agent_id: '' }), details(), NOW);
		expect(model.run?.provenance).toEqual([{ label: 'Execution id', value: 'exec-root' }]);
	});

	it('names the agent when the row has one, and reports no cost it cannot observe', () => {
		const model = toInternalTaskPanelModel(internalTask(), details(), NOW);
		expect(model.run?.provenance).toEqual([
			{ label: 'Execution id', value: 'exec-root' },
			{ label: 'Agent', value: 'reconciler' }
		]);
	});

	/**
	 * Deliberately empty. An execution record carries `completed_step_ids` —
	 * identifiers rather than descriptions — and rendering one as a step label
	 * would put L3 content at L2. Design §4 takes the header's counts from the
	 * list the body renders, so with no rows there is no `14 steps` to
	 * contradict either.
	 */
	it('lists no steps, because none are recorded in a form a reader could read', () => {
		expect(toInternalTaskPanelModel(internalTask(), details(), NOW).run?.steps).toEqual([]);
	});

	it('measures a finished run from its own start and end', () => {
		const model = toInternalTaskPanelModel(internalTask(), details(), NOW);
		expect(model.elapsedMs).toBe(Date.parse(COMPLETED_AT) - Date.parse(STARTED_AT));
		expect(deriveVerdict({ ...model, now: NOW }).headline).toBe('Finished · 3m');
	});

	it('measures a live run against the clock it was given', () => {
		const model = toInternalTaskPanelModel(
			internalTask({ status: 'running', last_progress_at: null }),
			details({ executions: [execution({ status: 'running', completed_at: null })] }),
			NOW
		);
		expect(model.elapsedMs).toBe(NOW - Date.parse(STARTED_AT));
	});

	it('reports no duration for a run with no readable start', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details({ executions: [execution({ started_at: 'nonsense' })] }),
			NOW
		);
		expect(model.elapsedMs).toBeNull();
	});
});

describe('toInternalTaskPanelModel — the Output act', () => {
	it('carries the files in the order the record lists them, each with its path', () => {
		const model = toInternalTaskPanelModel(internalTask(), details(), NOW);
		expect(model.output?.files.map((file) => file.name)).toEqual([
			'report.md',
			'report.md',
			'revenue.png'
		]);
		expect(internalTaskOutputFiles(details())?.map((file) => file.path)).toEqual([
			'alpha/report.md',
			'beta/report.md',
			'charts/revenue.png'
		]);
		expect(outputSummary(model.output!.files)).toBe('report.md and 2 other files');
	});

	it('puts every file path in provenance, where L3 detail belongs', () => {
		const model = toInternalTaskPanelModel(internalTask(), details(), NOW);
		expect(model.output?.provenance).toEqual([
			{ label: 'Source execution id', value: 'exec-root' },
			{ label: 'report.md', value: 'alpha/report.md' },
			{ label: 'report.md', value: 'beta/report.md' },
			{ label: 'revenue.png', value: 'charts/revenue.png' }
		]);
	});

	/**
	 * `[]` and "no list at all" are different claims and render differently: an
	 * act that loaded and found nothing summarises `no output`, and one that was
	 * never read must be absent, because an empty act asserts a fact about the
	 * task made out of a fact about the payload (design §6).
	 */
	it('has an empty Output act for a run that produced nothing', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details({ task: { refs: { outputs: [] } } }),
			NOW
		);
		expect(model.output?.files).toEqual([]);
	});

	it('has no Output act when the record carries no output list to read', () => {
		const model = toInternalTaskPanelModel(internalTask(), details({ task: {} }), NOW);
		expect(model.output).toBeNull();
	});

	/**
	 * The claim this surface could make and must not: synthesis is still writing
	 * the output, so `no output` would be a finality claim about a file that is
	 * on its way. Absent instead — and the finished verdict's second line, which
	 * the panel composes from this act, stays empty rather than reading
	 * `Produced no output`.
	 */
	it('stays silent while synthesis is still writing and nothing has landed', () => {
		const model = toInternalTaskPanelModel(
			internalTask({ synthesis_pending: true }),
			details({ task: { refs: { outputs: [] } } }),
			NOW
		);
		expect(model.output).toBeNull();
	});

	it('still shows files that already exist while synthesis is running', () => {
		const model = toInternalTaskPanelModel(
			internalTask({ synthesis_pending: true }),
			details(),
			NOW
		);
		expect(model.output?.files).toHaveLength(3);
	});

	it('shows run-owned evidence without claiming pending task synthesis produced nothing', () => {
		const model = toInternalTaskPanelModel(
			internalTask({ synthesis_pending: true }),
			details({
				task: { refs: { outputs: [] } },
				executions: [
					{
						...execution(),
						refs: {
							output_refs: [
								{
									relative_path: 'executions/exec-root/outputs/evidence.md',
									media_type: 'text/markdown'
								}
							]
						}
					}
				]
			}),
			NOW
		);

		expect(model.output?.taskFilesPending).toBe(true);
		expect(model.output?.files).toEqual([
			expect.objectContaining({ scope: 'execution', name: 'evidence.md' })
		]);
	});
});

describe('toInternalTaskPanelModel — the synthesis ask', () => {
	const failed = internalTask({ synthesis_failed_execution_id: 'exec-root' });
	const failedDetails = details({
		executions: [execution({ synthesis_failed: { failed_at: SYNTHESIS_FAILED_AT } })]
	});

	it('leaves a healthy task unblocked', () => {
		expect(toInternalTaskPanelModel(internalTask(), details(), NOW).attention).toBeNull();
	});

	/**
	 * Without this, a synthesis-failed task reads `Finished · Produced no output`
	 * — false twice over: the run produced something, and nothing more is coming
	 * until a person acts. The backend already models the failure as a HITL, so
	 * ranking it `Waiting on you` is a translation rather than an interpretation.
	 */
	it('ranks a failed synthesis above the finished lifecycle', () => {
		const model = toInternalTaskPanelModel(
			failed,
			details({ ...failedDetails, task: { refs: { outputs: [] } } }),
			NOW
		);
		const verdict = deriveVerdict({ ...model, now: NOW });
		expect(verdict.state).toBe('waiting');
		expect(verdict.detail).toBe(SYNTHESIS_FAILED_SUMMARY);
	});

	it('says how long it has been sitting there, from the failure the record recorded', () => {
		const model = toInternalTaskPanelModel(failed, failedDetails, NOW);
		expect(model.attention?.raisedAt).toBe(Date.parse(SYNTHESIS_FAILED_AT));
		expect(deriveVerdict({ ...model, now: NOW }).headline).toBe('Waiting on you · 3m');
	});

	it('omits the duration rather than inventing one before the details are read', () => {
		const model = toInternalTaskPanelModel(failed, null, NOW);
		expect(model.attention?.raisedAt).toBeNull();
		expect(deriveVerdict({ ...model, now: NOW }).headline).toBe('Waiting on you');
	});

	/**
	 * The source is a routing choice — it picks the act the reader lands in — and
	 * never a word anyone reads. `summary` always outranks the per-source copy,
	 * so the enum cannot reach the screen.
	 */
	it('never leaks the routing source to the reader', () => {
		const model = toInternalTaskPanelModel(failed, failedDetails, NOW);
		const verdict = deriveVerdict({ ...model, now: NOW });
		expect(verdict.headline + verdict.detail).not.toMatch(/escalation/i);
	});
});

describe('internalTaskOutputFiles', () => {
	it('answers `null` for details that were never read', () => {
		expect(internalTaskOutputFiles(null)).toBeNull();
	});

	it('answers `null` for a payload whose output list is not a list', () => {
		expect(
			internalTaskOutputFiles(details({ task: { refs: { outputs: 'nope' } } }))
		).toBeNull();
	});

	it('drops a row with no path rather than rendering it nameless', () => {
		const files = internalTaskOutputFiles(
			details({ task: { refs: { outputs: [{ media_type: 'text/plain' }, OUTPUT_REFS[2]] } } })
		);
		expect(files?.map((file) => file.name)).toEqual(['revenue.png']);
	});

	it('mints each row an address from the caller, keyed on the row\'s own path', () => {
		// Two of the three share a basename, which is exactly the case a minter
		// keyed on the display name would collapse into one address.
		const files = internalTaskOutputFiles(details(), (path) => `/outputs/${path}`);

		expect(files?.map((file) => file.url)).toEqual([
			'/outputs/alpha/report.md',
			'/outputs/beta/report.md',
			'/outputs/charts/revenue.png'
		]);
	});

	it('leaves the address absent when the caller passes no minter', () => {
		expect(internalTaskOutputFiles(details())?.every((file) => file.url === null)).toBe(true);
	});
});

/**
 * The same restoration as the normal route's, off the field the internal row has
 * always carried: these rows are the same `TaskListItemV3` the `/tasks` list
 * sends, so `completion_summary` was on the wire here all along and nothing on
 * this surface read it.
 */
describe('toInternalTaskPanelModel — the written report', () => {
	it('carries the row\'s completion summary into the Output act', () => {
		const model = toInternalTaskPanelModel(
			internalTask({ completion_summary: '## Ledger\n\nBalanced to the cent.' }),
			details(),
			NOW
		);

		expect(model.output?.summary).toBe('## Ledger\n\nBalanced to the cent.');
	});

	it('treats a blank summary as none', () => {
		expect(
			toInternalTaskPanelModel(internalTask({ completion_summary: '  ' }), details(), NOW).output
				?.summary
		).toBeNull();
		expect(toInternalTaskPanelModel(internalTask(), details(), NOW).output?.summary).toBeNull();
	});

	it('reads the task-level summary, not whichever execution the Run act describes', () => {
		// The execution record carries one of its own, and it can belong to a
		// *different* run than the outputs listed beside it — a live retry, say.
		// The two are given different words here so a reader of the wrong one
		// fails rather than coincidentally matching.
		const model = toInternalTaskPanelModel(
			internalTask({ completion_summary: 'The task-level account' }),
			details({ executions: [execution({ completion_summary: 'This run only' })] }),
			NOW
		);

		expect(model.output?.summary).toBe('The task-level account');
	});
});

/**
 * The run picker on the internal surface — **design §2's acceptance test for this
 * feature**, and the reason it is worth writing twice.
 *
 * `/tasks` reads its runs out of `/execution-panel`'s `output.recent_runs` and
 * this route reads them out of `/details`'s `executions`. Neither adapter
 * contains a branch about which surface it is on: both map their payload onto
 * three neutral facts and hand them to the same `runsSliceOf`. So these cases
 * are the same claims as `taskPanelModel.test.ts`'s, made over a different wire —
 * and if the two ever disagree, the shared function is where it shows.
 */
const OLDER_AT = new Date(200_000).toISOString();
const OLDEST_AT = new Date(100_000).toISOString();

/** Three executions of one internal task, newest last, two of them failures. */
const THREE_EXECUTIONS: InternalExecutionDetails[] = [
	execution({ execution_id: 'exec-1', status: 'failed', started_at: OLDEST_AT }),
	execution({ execution_id: 'exec-2', status: 'failed', started_at: OLDER_AT }),
	execution({ execution_id: 'exec-root', status: 'completed', started_at: STARTED_AT })
];

describe('the run picker, on the other wire', () => {
	it('offers nothing at one execution — absent, not disabled', () => {
		expect(toInternalTaskPanelModel(internalTask(), details(), NOW).runs).toBeNull();
	});

	it('offers nothing before /details has answered', () => {
		expect(toInternalTaskPanelModel(internalTask(), null, NOW).runs).toBeNull();
	});

	it('offers every execution, newest first, in the panel’s own status vocabulary', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details({ executions: THREE_EXECUTIONS }),
			NOW
		);

		expect(model.runs?.options.map((option) => option.executionId)).toEqual([
			'exec-root',
			'exec-2',
			'exec-1'
		]);
		// `completed` on the wire, `finished` in the option — the same word the
		// verdict line above the control uses for the same outcome.
		expect(model.runs?.options.map((option) => option.status)).toEqual([
			'finished',
			'failed',
			'failed'
		]);
		expect(model.runs?.options.map((option) => option.ordinal)).toEqual([3, 2, 1]);
	});

	it('never renders a bare execution id as a label', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details({ executions: THREE_EXECUTIONS }),
			NOW
		);

		for (const option of model.runs?.options ?? []) {
			expect(option.label).not.toContain(option.executionId);
			expect(option.label.startsWith(`#${option.ordinal}`)).toBe(true);
		}
	});

	it('selects the row’s current run by default, so nothing changes for a caller that passes none', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details({ executions: THREE_EXECUTIONS }),
			NOW
		);

		expect(model.runs?.selectedId).toBe('exec-root');
		expect(model.run?.provenance).toContainEqual({ label: 'Execution id', value: 'exec-root' });
	});

	it('points the Run act at the chosen execution', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details({ executions: THREE_EXECUTIONS }),
			NOW,
			undefined,
			'exec-1'
		);

		expect(model.runs?.selectedId).toBe('exec-1');
		expect(model.run?.provenance).toContainEqual({ label: 'Execution id', value: 'exec-1' });
	});

	/**
	 * **The correctness requirement, on this surface.** `elapsedMs` feeds the
	 * verdict line, so it has to keep reading the *current* execution: a task that
	 * finished in three minutes must not read `Finished · 40s` because the reader is
	 * looking at an attempt that died early.
	 */
	it('leaves the verdict describing the task when an earlier execution is selected', () => {
		const row = internalTask({ status: 'completed' });
		const payload = details({ executions: THREE_EXECUTIONS });

		const current = toInternalTaskPanelModel(row, payload, NOW);
		const earlier = toInternalTaskPanelModel(row, payload, NOW, undefined, 'exec-1');

		expect(earlier.status).toBe('finished');
		expect(deriveVerdict({ ...earlier, now: NOW }).state).toBe('finished');
		// The duration is the task's run, not the selected attempt's — the two
		// executions have different `started_at`, so a bled-through value would show.
		expect(earlier.elapsedMs).toBe(current.elapsedMs);
		// And the control says which run the acts are about, so the pair is legible.
		expect(earlier.runs?.options.find((option) => option.executionId === 'exec-1')?.status).toBe(
			'failed'
		);
	});

	it('keeps task deliverables stable and scopes direct, delegated, and persisted output to the selected run', () => {
		const row = internalTask();
		const withRunOutput = (
			base: InternalExecutionDetails,
			path: string,
			includeEvidence = false
		): InternalExecutionDetails => ({
			...base,
			refs: {
				output_refs: [{ relative_path: path, media_type: 'text/markdown' }],
				child_output_refs: includeEvidence
					? [{ relative_path: 'executions/exec-child/outputs/evidence.json', media_type: 'application/json' }]
					: []
			},
			artifacts: includeEvidence
				? [
						{
							artifact_id: 'artifact-file',
							artifact_type: 'screen_capture',
							content_type: 'image/png',
							produced_at: new Date(210_000).toISOString(),
							payload: {
								execution_relative_path: 'outputs/capture.png',
								display_name: 'capture.png',
								size_bytes: 64
							}
						},
						{
							artifact_id: 'artifact-structured',
							artifact_type: 'browser_observation',
							content_type: 'application/json',
							produced_at: new Date(220_000).toISOString(),
							payload: { url: 'https://example.test' }
						}
					]
				: []
		});
		const payload = details({
			executions: [
				withRunOutput(THREE_EXECUTIONS[0], 'executions/exec-1/outputs/attempt.md', true),
				THREE_EXECUTIONS[1],
				withRunOutput(THREE_EXECUTIONS[2], 'executions/exec-root/outputs/final.md')
			]
		});
		const urlFor = (path: string) => `/files/${path}`;
		const current = toInternalTaskPanelModel(row, payload, NOW, urlFor);
		const earlier = toInternalTaskPanelModel(row, payload, NOW, urlFor, 'exec-1');

		expect(current.output?.files.slice(0, 3).map((file) => file.scope)).toEqual([
			'task',
			'task',
			'task'
		]);
		expect(current.output?.files.at(-1)).toEqual(
			expect.objectContaining({ scope: 'execution', name: 'final.md' })
		);
		expect(earlier.output?.files.slice(3).map((file) => [file.scope, file.name])).toEqual([
			['execution', 'attempt.md'],
			['delegated', 'evidence.json'],
			['artifact', 'capture.png']
		]);
		expect(earlier.output?.files.at(-1)?.url).toBe(
			'/files/executions/exec-1/outputs/capture.png'
		);
		expect(earlier.output?.artifacts).toEqual([
			expect.objectContaining({ id: 'artifact-structured', sourceExecutionId: 'exec-1' })
		]);
		expect(earlier.output?.selectedExecutionId).toBe('exec-1');
	});

	/**
	 * The reader asked for a run this payload cannot describe. The Run act falls
	 * back to the current one rather than emptying, and the control reports the run
	 * it is actually showing — never the argument it was handed.
	 */
	it('falls back to the current execution for a selection the payload does not list', () => {
		const model = toInternalTaskPanelModel(
			internalTask(),
			details({ executions: THREE_EXECUTIONS }),
			NOW,
			undefined,
			'exec-from-another-task'
		);

		expect(model.runs?.selectedId).toBe('exec-root');
		expect(model.run?.provenance).toContainEqual({ label: 'Execution id', value: 'exec-root' });
	});
});
