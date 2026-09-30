/**
 * The execution adapter: a run, mapped onto the panel's model.
 *
 * What these pin is what only this seam can be wrong about — the claims it
 * would make out of a payload it misread. Every fixture value below is
 * deliberately distinct from every other, including the ones that would be
 * plausible substitutes for each other: the task id, the execution id, the two
 * step names and the four instants are all different, so a mapping that read
 * the wrong field cannot pass by accident. That collision is the first shape of
 * the recurring defect in this project and it has cost two suites already.
 */
import { describe, expect, it } from 'vitest';

import type { ExecutionPanelState } from '$lib/types/executionPanel';

import { executionPanelUrl, toExecutionPanelModel } from './executionPanelModel';
import type { PanelOutputFile } from './taskPanelModel';

const TASK_ID = 'task-corpus-reindex';
const EXECUTION_ID = 'exec-9f3c';
/** Four distinct instants, so a mapping cannot read one and pass as another. */
const STARTED_AT = 1_770_000_000_000;
const ACTIVITY_AT = 1_770_000_060_000;
const TIMELINE_AT = 1_770_000_120_000;
const ENDED_AT = 1_770_000_180_000;
/** Deliberately later than every real instant: nothing may read it. */
const UPDATED_AT = 1_770_000_999_000;

function state(overrides: {
	overview?: Partial<ExecutionPanelState['overview']>;
	run?: Partial<ExecutionPanelState['run']>;
	output?: Partial<ExecutionPanelState['output']>;
	debug?: Partial<ExecutionPanelState['debug']>;
	selectedExecution?: Partial<
		NonNullable<ExecutionPanelState['debug']['selected_execution']>
	> | null;
} = {}): ExecutionPanelState {
	const selected =
		overrides.selectedExecution === null
			? null
			: {
					execution_id: EXECUTION_ID,
					status: 'running' as const,
					started_at: STARTED_AT,
					ended_at: null,
					error_message: null,
					summary: null,
					outcome: null,
					artifact_names: [],
					current_step: null,
					progress: null,
					plan_ref: null,
					plan_id: null,
					artifact_chain_id: null,
					linked_inputs: [],
					step_statuses: [],
					...overrides.selectedExecution
				};
	return {
		default_tab: 'run',
		overview: {
			task_id: TASK_ID,
			execution_id: EXECUTION_ID,
			principal: 'anonymous',
			workspace: 'default',
			ui_thread_id: 'general',
			title: 'Reindex the corpus',
			description: '',
			status: 'running',
			assigned_agent_id: 'personal-assistant',
			active_agent_id: 'research-agent',
			has_plan: false,
			created_at: STARTED_AT,
			updated_at: UPDATED_AT,
			...overrides.overview
		},
		run: {
			pending_questions: [],
			needs_attention: [],
			recent_activity: [],
			...overrides.run
		},
		output: { deliveries: [], recent_runs: [], ...overrides.output },
		debug: {
			selected_execution: selected,
			timeline: [],
			observations: [],
			shell_entries: [],
			history_count: 0,
			tags: [],
			...overrides.debug
		}
	};
}

function file(name: string, path: string): PanelOutputFile {
	return { name, kind: 'document', path, mediaType: 'text/markdown', sizeBytes: 42, url: null };
}

describe('toExecutionPanelModel', () => {
	it('names the run, never the task that owns it', () => {
		// Two runs of one task are two things to inspect. An id naming the task
		// would carry the reader's open act from one onto the other while the
		// panel had no way to tell they had changed.
		expect(toExecutionPanelModel(state(), null).id).toBe(`execution:${EXECUTION_ID}`);
		expect(
			toExecutionPanelModel(
				state({ overview: { execution_id: null }, selectedExecution: null }),
				null
			).id
		).toBe(`task:${TASK_ID}`);
	});

	it('has no Plan act, whatever the payload says about planning', () => {
		const model = toExecutionPanelModel(
			state({
				overview: { has_plan: true },
				selectedExecution: {
					step_statuses: [{ number: 1, name: 'Fetch the corpus', status: 'completed', progress: '' }]
				}
			}),
			null
		);
		// Step statuses are the run's account of executing a plan, not the plan.
		// The act is absent, which is the capability model rather than a gap.
		expect(model.plan).toBeNull();
		expect(model.run?.steps).toHaveLength(1);
	});

	it('reads the live step off the list the body renders', () => {
		const model = toExecutionPanelModel(
			state({
				overview: { current_step: 1 },
				selectedExecution: {
					step_statuses: [
						{ number: 1, name: 'Fetch the corpus', status: 'completed', progress: '' },
						{ number: 2, name: 'Rebuild the index', status: 'in_progress', progress: '' },
						{ number: 3, name: 'Publish the manifest', status: 'pending', progress: '' }
					]
				}
			}),
			null
		);
		// `overview.current_step` says 1 and the list says 2. The list wins,
		// because it is the one the reader can check against what is on screen.
		expect(model.currentStep).toBe(2);
		expect(model.currentStepLabel).toBe('Rebuild the index');
		expect(model.totalSteps).toBe(3);
	});

	it('renders no marker for a step status it does not model', () => {
		const model = toExecutionPanelModel(
			state({
				selectedExecution: {
					step_statuses: [
						{ number: 1, name: 'Something new', status: 'quiesced', progress: '' },
						{ number: 2, name: 'Went sideways', status: 'ERROR', progress: '' }
					]
				}
			}),
			null
		);
		// A word this client has never seen yields `null` and the row renders no
		// mark — never `pending`, which would claim the step had not started.
		// Case is not part of the vocabulary, which is why `ERROR` still lands.
		expect(model.run?.steps.map((step) => step.status)).toEqual([null, 'failed']);
	});

	/**
	 * The three words that used to yield `null` because the six step statuses had
	 * nothing that meant "held up". `RunStepStatus` gained a seventh for delegated
	 * child runs, and these three are the same claim — so they map onto it rather
	 * than staying unmodelled, and a step suspended mid-flight stops reading as a
	 * step with no recorded status.
	 */
	it('models the three words that mean held up, rather than dropping their mark', () => {
		const model = toExecutionPanelModel(
			state({
				selectedExecution: {
					step_statuses: [
						{ number: 1, name: 'Held up', status: 'blocked', progress: '' },
						{ number: 2, name: 'Asked a question', status: 'waiting', progress: '' },
						{ number: 3, name: 'Suspended', status: 'paused', progress: '' }
					]
				}
			}),
			null
		);
		expect(model.run?.steps.map((step) => step.status)).toEqual([
			'waiting',
			'waiting',
			'waiting'
		]);
		// And they are not the live step: `waiting` is work that has stopped.
		expect(model.currentStep).toBeNull();
	});

	it('carries the capability and the delegate agent each step status names', () => {
		const model = toExecutionPanelModel(
			state({
				selectedExecution: {
					step_statuses: [
						{
							number: 1,
							name: 'Fetch the corpus',
							status: 'completed',
							progress: '',
							capability: 'corpus_fetch',
							delegate_agent_id: 'research-agent'
						},
						// Blank rather than absent, which is what the wire sends for a step
						// nothing was recorded against. Both must read as absent, or the
						// row renders `via ` with nothing after it.
						{
							number: 2,
							name: 'Rebuild the index',
							status: 'in_progress',
							progress: '',
							capability: '  ',
							delegate_agent_id: null
						}
					]
				}
			}),
			null
		);

		expect(model.run?.steps.map((step) => [step.capability, step.delegate])).toEqual([
			['corpus_fetch', 'research-agent'],
			[null, null]
		]);
	});

	/**
	 * The delegated children come off `run.responsibility`, which this payload
	 * carries and this adapter used to ignore. The projection is
	 * `delegatedStepsFrom` in `taskPanelModel.ts` — **the same function the task
	 * adapter calls**, so a run that delegated work reads identically whether the
	 * reader arrived from a task row or from a chat activity card. A second
	 * projection here would be a second answer to what counts as a child of this
	 * run, and each surface renders only its own, so the two could disagree
	 * forever without anything noticing.
	 */
	it('lists delegated children beside the steps, without counting them as steps', () => {
		const model = toExecutionPanelModel(
			state({
				selectedExecution: {
					step_statuses: [
						{ number: 1, name: 'Fetch the corpus', status: 'completed', progress: '' },
						{ number: 2, name: 'Rebuild the index', status: 'in_progress', progress: '' }
					]
				},
				run: {
					pending_questions: [],
					needs_attention: [],
					recent_activity: [],
					responsibility: {
						execution_id: EXECUTION_ID,
						waiting_state: 'WaitingChildren',
						active_owner_agent_id: 'personal-assistant',
						owner_stack: [EXECUTION_ID],
						owner_chain: [EXECUTION_ID],
						handover_active: false,
						waiting_on_children: true,
						active_child_count: 1,
						historical_child_count: 1,
						responsibility_summary: 'Waiting on delegated work',
						active_children: [
							{
								execution_id: 'exec-child-11',
								title: 'Re-embed the changed documents',
								waiting_state: 'WaitingUser',
								active_owner_agent_id: 'indexer-agent',
								delegation_chain: [EXECUTION_ID, 'exec-child-11'],
								is_blocking: true
							}
						]
					}
				}
			}),
			null
		);

		expect(model.run?.steps.map((step) => [step.origin, step.label])).toEqual([
			['step', 'Fetch the corpus'],
			['step', 'Rebuild the index'],
			['delegated', 'Re-embed the changed documents']
		]);
		expect(model.run?.steps[2].delegate).toBe('indexer-agent');
		expect(model.run?.steps[2].status).toBe('waiting');
		// Two steps, not three: the plan's denominator is the plan's.
		expect(model.totalSteps).toBe(2);
		expect(model.currentStep).toBe(2);
		expect(model.currentStepLabel).toBe('Rebuild the index');
	});

	it('takes the last progress instant from the event logs, never from updated_at', () => {
		const model = toExecutionPanelModel(
			state({
				run: {
					activity_log: [
						{
							id: 'activity-1',
							principal: 'anonymous',
							workspace: 'default',
							// `FeedItemType` has no `execution` member. The adapter reads
							// only `created_at` from these rows, so the wrong value was
							// invisible until the type check ran.
							item_type: 'task',
							title: 'Thinking',
							status: 'running',
							created_at: ACTIVITY_AT,
							updated_at: ACTIVITY_AT,
							actions: [],
							metadata: {}
						}
					]
				},
				debug: {
					timeline: [
						{
							id: 'timeline-1',
							timestamp: TIMELINE_AT,
							severity: 'info',
							title: 'Step started',
							message: ''
						}
					]
				}
			}),
			null
		);
		// The maximum across both logs. `updated_at` is later than either and is
		// exactly the value design §5 forbids: a status poll rewrites it, which
		// would make a wedged run un-stallable.
		expect(model.lastProgressAt).toBe(TIMELINE_AT);
		expect(model.lastProgressAt).not.toBe(UPDATED_AT);
	});

	it('measures the run only once it has ended', () => {
		expect(toExecutionPanelModel(state(), null).elapsedMs).toBeNull();
		expect(
			toExecutionPanelModel(state({ selectedExecution: { ended_at: ENDED_AT } }), null).elapsedMs
		).toBe(ENDED_AT - STARTED_AT);
	});

	it('ranks the canonical ask above a pending question, and falls back to it', () => {
		const raised = toExecutionPanelModel(
			state({
				run: {
					needs_attention: [
						{
							id: 'attention-1',
							principal: 'anonymous',
							workspace: 'default',
							item_type: 'escalation',
							title: 'Approval requested',
							summary: 'A feed lane label',
							status: 'needs_action',
							created_at: ACTIVITY_AT,
							updated_at: ACTIVITY_AT,
							actions: [],
							metadata: {},
							// A real `HitlOpenTarget`: `id` and `input_type` are required,
							// and a fixture missing them proves less than it appears to —
							// `taskAttention` keys its whole filter on this object.
							hitl_request: {
								id: 'hitl-1',
								source: 'diff_approval',
								input_type: 'confirmation',
								prompt: 'Apply the index migration?',
								at: TIMELINE_AT
							}
						}
					],
					pending_questions: [
						{
							id: 'question-1',
							question_text: 'Which corpus revision?',
							status: 'open',
							context_snippets: [],
							related_slots: [],
							options: []
						}
					]
				}
			}),
			null
		);
		// The canonical row carries a real source and a real instant; the question
		// carries neither, so it loses even though both are present.
		expect(raised.attention).toEqual({
			source: 'diff_approval',
			summary: 'Apply the index migration?',
			raisedAt: TIMELINE_AT
		});

		const fallback = toExecutionPanelModel(
			state({
				run: {
					pending_questions: [
						{
							id: 'question-1',
							question_text: 'Which corpus revision?',
							status: 'open',
							context_snippets: [],
							related_slots: [],
							options: []
						}
					]
				}
			}),
			null
		);
		// Without it the question is the only ask there is, and a run blocked on
		// one must not read `Queued` — the failure the whole attention path exists
		// to fix.
		expect(fallback.attention).toEqual({
			source: 'clarification',
			summary: 'Which corpus revision?',
			raisedAt: null
		});
	});

	it('keeps the Output act absent when no file list was read', () => {
		// The `/crew` case: `/v3/tasks/{id}/outputs` cannot answer for an
		// `agent-cycle:` id, so nothing has read what the run produced. An empty
		// act would answer `no output`, which is a claim this caller cannot make —
		// and it stays absent even though the payload carries a written summary.
		const model = toExecutionPanelModel(
			state({ output: { result: { summary: 'Reindexed 4,102 documents.', artifact_names: [] } } }),
			null
		);
		expect(model.output).toBeNull();
	});

	it('carries the written report into an Output act that did load', () => {
		const model = toExecutionPanelModel(
			state({
				output: {
					result: {
						summary: '# Reindex\n\nReindexed 4,102 documents.',
						outcome: 'succeeded',
						artifact_names: ['corpus/index']
					}
				}
			}),
			[file('report.md', 'reports/report.md')]
		);
		expect(model.output?.summary).toBe('# Reindex\n\nReindexed 4,102 documents.');
		expect(model.output?.files.map((entry) => entry.name)).toEqual(['report.md']);
		// `outcome` is a one-line disposition, not a report. Folding it in as a
		// fallback would open a markdown block for a sentence that is not one.
		expect(model.output?.summary).not.toContain('succeeded');
	});

	it('renders an empty Output act for a run that produced nothing', () => {
		const model = toExecutionPanelModel(state(), []);
		expect(model.output).not.toBeNull();
		expect(model.output?.files).toEqual([]);
		expect(model.output?.summary).toBeNull();
	});

	/**
	 * The timeline is why this adapter exists at all now: it is the only one
	 * reaching the panel whose payload carries an event log. `[]` and `null` are
	 * different claims here exactly as they are for `output` — a run that
	 * recorded nothing says so, and a model nothing observed the events of stays
	 * silent — and only this adapter can ever produce the first.
	 */
	it('carries the run\'s events, and says so even when there were none', () => {
		const withEvents = toExecutionPanelModel(
			state({
				run: {
					activity_log: [
						{
							id: 'log-1',
							principal: 'anonymous',
							workspace: 'default',
							item_type: 'task',
							title: 'LLM call succeeded',
							summary: null,
							status: 'done',
							created_at: ACTIVITY_AT,
							updated_at: ACTIVITY_AT,
							actions: [],
							metadata: { event_type: 'llm.succeeded', capability: 'research' }
						}
					]
				}
			}),
			null
		);
		expect(withEvents.run?.timeline?.map((entry) => entry.title)).toEqual([
			'Thinking with research'
		]);

		// An execution whose payload was read and held no events. Never `null`:
		// the state was fetched, so its events *were* observed.
		expect(toExecutionPanelModel(state(), null).run?.timeline).toEqual([]);
	});

	it('reports the run error from the payload, not from the run summary', () => {
		const model = toExecutionPanelModel(
			state({
				overview: { status: 'failed' },
				run: { summary: 'Reindexed 4,102 documents.' },
				debug: { latest_error_message: 'Provider unavailable' }
			}),
			null
		);
		expect(model.status).toBe('failed');
		expect(model.error).toBe('Provider unavailable');
	});
});

describe('executionPanelUrl', () => {
	it('routes a real task id to the task-scoped panel with the execution pinned', () => {
		expect(
			executionPanelUrl({ taskId: TASK_ID, executionId: EXECUTION_ID }, 'anonymous', 'default')
		).toBe(
			`/api/magician/v3/tasks/${TASK_ID}/execution-panel?execution_id=${EXECUTION_ID}`
		);
	});

	it('routes a synthetic agent-cycle id to the execution-scoped panel', () => {
		// A cycle id is not a task id and the task route cannot answer for one.
		expect(
			executionPanelUrl(
				{ taskId: 'agent-cycle:research-agent:cycle-7', executionId: EXECUTION_ID },
				'anonymous',
				'default'
			)
		).toBe(
			`/api/magician/v3/executions/${EXECUTION_ID}/execution-panel`
		);
	});
});
