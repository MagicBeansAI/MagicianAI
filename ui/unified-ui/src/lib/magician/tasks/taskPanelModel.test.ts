import { describe, expect, it } from 'vitest';

import type { FeedItem } from '$lib/feed/types';
import type { PlanStep, Task, TaskStatus } from '$lib/stores/taskStore';
import type {
	ExecutionPanelAttentionItem,
	ExecutionPanelRecentRun,
	ExecutionPanelResponsibilityChild,
	ExecutionPanelResponsibilityState,
	ExecutionPanelState
} from '$lib/types/executionPanel';

import { deriveActs } from './taskCapabilities';
import { outputFilesFrom, outputKindOf, toTaskPanelModel } from './taskPanelModel';
import { deriveVerdict } from './taskVerdict';

/**
 * The run payload the third argument carries — `/execution-panel` for this task,
 * as `fetchTaskRunState` returns it.
 *
 * Written out here rather than reduced to the two fields the adapter reads,
 * because the adapter reads them **through the shape the backend sends**: an
 * `execution_id` that does not match the task row's is a real payload this
 * surface receives, and a fixture that omitted it could not tell a correct
 * adapter from one that ignored the disagreement.
 */
function runState(
	executionId: string,
	parts: {
		attention?: ExecutionPanelAttentionItem[];
		activity?: FeedItem[];
		responsibility?: ExecutionPanelResponsibilityState | null;
		recentRuns?: ExecutionPanelRecentRun[];
		directOutputs?: ExecutionPanelState['output']['selected_execution_outputs'];
		childOutputs?: ExecutionPanelState['output']['selected_child_outputs'];
		artifacts?: ExecutionPanelState['output']['selected_execution_artifacts'];
	} = {}
): ExecutionPanelState {
	return {
		default_tab: 'run',
		overview: {
			task_id: 'task-1',
			execution_id: executionId,
			principal: 'anonymous',
			workspace: 'default',
			ui_thread_id: 'general',
			title: 'Quarterly revenue',
			description: '',
			status: 'running',
			assigned_agent_id: 'personal-assistant',
			has_plan: false,
			created_at: 10_000,
			updated_at: 60_000
		},
		run: {
			pending_questions: [],
			needs_attention: parts.attention ?? [],
			recent_activity: [],
			activity_log: parts.activity ?? [],
			responsibility: parts.responsibility ?? null
		},
		output: {
			deliveries: [],
			recent_runs: parts.recentRuns ?? [],
			...(parts.directOutputs === undefined
				? {}
				: { selected_execution_outputs: parts.directOutputs }),
			...(parts.childOutputs === undefined
				? {}
				: { selected_child_outputs: parts.childOutputs }),
			...(parts.artifacts === undefined
				? {}
				: { selected_execution_artifacts: parts.artifacts })
		},
		debug: {
			selected_execution: null,
			timeline: [],
			observations: [],
			shell_entries: [],
			history_count: 0,
			tags: []
		}
	};
}

/**
 * One activity row. `item_type` is `'task'` because **`FeedItemType` has no
 * `execution` member** — vitest transpiles without type-checking, so a fixture
 * asserting a shape the backend cannot produce passes the suite and is caught
 * only by `svelte-check`.
 */
function activity(id: string, createdAt: number, metadata: Record<string, unknown>): FeedItem {
	return {
		id,
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'task',
		task_id: 'task-1',
		title: 'Something happened',
		summary: null,
		status: 'done',
		created_at: createdAt,
		updated_at: createdAt,
		actions: [],
		metadata
	};
}

/**
 * The seam's fixture. Every value that could stand in for another is different,
 * for the reason the panel's own suite gives: an adapter wiring the wrong field
 * through has to fail, and it cannot fail against a fixture where two fields
 * hold the same thing.
 *
 * | | |
 * |---|---|
 * | plan steps | 3 |
 * | the live step | 2 |
 * | progress instant | 40_000 |
 * | plan updated | 50_000 |
 * | created | 10_000 |
 */
const NOW = 1_000_000;

const STEPS: PlanStep[] = [
	{ id: 's1', description: 'Read the exports', status: 'completed', duration_ms: 3_000 },
	{ id: 's2', description: 'Summarise revenue', status: 'in_progress' },
	{ id: 's3', description: 'Render the chart', status: 'pending' }
];

function task(overrides: Partial<Task> = {}): Task {
	return {
		id: 'task-1',
		title: 'Quarterly revenue',
		description: 'Summarise the quarter',
		status: 'running',
		tags: [],
		source: 'task',
		createdAt: new Date(10_000).toISOString(),
		updatedAt: new Date(60_000).toISOString(),
		...overrides
	};
}

describe('toTaskPanelModel — identity', () => {
	/**
	 * The one field on the model that renders nothing. It decides whose
	 * reader-choice the open act belongs to, so an adapter that dropped it — or
	 * derived it from anything other than the task's own id — would carry one
	 * task's chosen act onto the next with nothing on screen to contradict it.
	 */
	it("carries the store's own task id, so a swap is distinguishable from a poll", () => {
		expect(toTaskPanelModel(task({ id: 'task-42' }), []).id).toBe('task-42');
	});
});

describe('toTaskPanelModel — the status vocabulary', () => {
	/**
	 * The map's whole reason to exist. `deriveVerdict`'s last branch is
	 * unconditional, so a status that does not reach one of the words it models
	 * reads `Finished` — and this asserts it through `deriveVerdict` rather than
	 * against the mapped string, because the string is not what misleads anyone.
	 */
	const notFinished: TaskStatus[] = ['pending', 'planning', 'ready', 'deferred', 'paused'];

	it.each(notFinished)('never lets a %s task read as finished', (status) => {
		const model = toTaskPanelModel(task({ status }), []);
		const verdict = deriveVerdict({ ...model, now: NOW });

		expect(verdict.state).not.toBe('finished');
		expect(verdict.headline).not.toMatch(/finished/i);
	});

	it('maps terminal, active, paused, and archived statuses onto honest verdict states', () => {
		const expected: Array<[TaskStatus, string]> = [
			['running', 'running'],
			['paused', 'paused'],
			['failed', 'failed'],
			['cancelled', 'cancelled'],
			['archived', 'archived'],
			['completed', 'finished']
		];
		for (const [status, state] of expected) {
			const model = toTaskPanelModel(task({ status, errorMessage: 'boom' }), []);
			expect(deriveVerdict({ ...model, now: NOW }).state).toBe(state);
		}
	});

	it('carries the failure text rather than a state alone', () => {
		const model = toTaskPanelModel(
			task({ status: 'failed', errorMessage: "Couldn't read revenue.csv" }),
			[]
		);
		expect(deriveVerdict({ ...model, now: NOW }).detail).toBe("Couldn't read revenue.csv");
	});
});

describe('toTaskPanelModel — absence', () => {
	/**
	 * The coercion this module exists for. The store spells absence `undefined`
	 * and the modules spell it `null`, and a model that passed `undefined`
	 * through would satisfy no `=== null` check downstream.
	 */
	it('spells every absent field `null`, never `undefined`', () => {
		const model = toTaskPanelModel(task({ status: 'ready' }), null);

		expect(model.error).toBeNull();
		expect(model.currentStep).toBeNull();
		expect(model.totalSteps).toBeNull();
		expect(model.currentStepLabel).toBeNull();
		expect(model.elapsedMs).toBeNull();
		expect(model.lastProgressAt).toBeNull();
		expect(model.attention).toBeNull();
		expect(model.plan).toBeNull();
		expect(model.run).toBeNull();
		expect(model.output).toBeNull();

		for (const [field, value] of Object.entries(model)) {
			expect(value, `${field} is undefined rather than null`).not.toBeUndefined();
		}
	});

	it('carries an absent plan status as `null` rather than dropping the act', () => {
		const model = toTaskPanelModel(task({ hasPlan: true }), []);
		expect(model.plan).not.toBeNull();
		expect(model.plan?.status).toBeNull();
	});
});

describe('toTaskPanelModel — the progress instant', () => {
	/**
	 * Task 7's field, end to end. A wedged run has to be reportable as stalled,
	 * and a missing instant must not read as one that just moved.
	 */
	it('reports a stall from the progress instant the store parsed', () => {
		const model = toTaskPanelModel(
			task({ status: 'running', lastProgressAt: NOW - 6 * 60_000 }),
			[]
		);

		expect(model.lastProgressAt).toBe(NOW - 6 * 60_000);
		expect(deriveVerdict({ ...model, now: NOW }).state).toBe('stalled');
	});

	it('reports no stall at all when no progress instant was recorded', () => {
		const model = toTaskPanelModel(task({ status: 'running', lastProgressAt: undefined }), []);

		// `null`, and specifically not `now`: substituting the clock would make a
		// wedged run look freshly advanced and un-stallable forever.
		expect(model.lastProgressAt).toBeNull();
		expect(deriveVerdict({ ...model, now: NOW }).state).toBe('running');
	});
});

describe('toTaskPanelModel — the acts a task has', () => {
	const actsOf = (model: ReturnType<typeof toTaskPanelModel>) =>
		deriveActs({
			hasPlanAct: model.plan !== null,
			hasRunAct: model.run !== null,
			hasOutputAct: model.output !== null
		});

	it('gives a planned, executing task with loaded outputs all three acts', () => {
		const model = toTaskPanelModel(
			task({ hasPlan: true, planSteps: STEPS, executionId: 'ex_1' }),
			[]
		);
		expect(actsOf(model)).toEqual(['plan', 'run', 'output']);
	});

	it('omits the Run act for a planned task that has never executed', () => {
		const model = toTaskPanelModel(task({ status: 'ready', hasPlan: true, planSteps: STEPS }), []);
		expect(actsOf(model)).toEqual(['plan', 'output']);
	});

	it('omits the Plan act for an execution with no plan of any kind', () => {
		const model = toTaskPanelModel(task({ executionId: 'ex_1' }), []);
		expect(actsOf(model)).toEqual(['run', 'output']);
	});

	it('gives a task an unasked-for Plan act when it has questions but no plan record', () => {
		// The questions render in the Plan act and nowhere else, so a task being
		// asked something must have one to be asked in.
		const model = toTaskPanelModel(
			task({ pendingQuestions: [{ id: 'q1', question: 'Which quarter?' }] }),
			[]
		);
		// The id travels with the question: it is what matches this ask to the
		// answerable target the run payload publishes for it, and what lets the
		// act body show the ask once rather than twice.
		expect(model.plan?.questions).toEqual([{ id: 'q1', question: 'Which quarter?' }]);
	});

	it('omits the Output act when the outputs are unknown, and keeps it when they are empty', () => {
		expect(toTaskPanelModel(task(), null).output).toBeNull();
		expect(toTaskPanelModel(task(), []).output).toEqual({
			files: [],
			summary: null,
			provenance: []
		});
	});
});

describe('toTaskPanelModel — the run', () => {
	it('reads the live step off the list the act body renders, 1-based', () => {
		const model = toTaskPanelModel(task({ planSteps: STEPS, executionId: 'ex_1' }), []);

		expect(model.currentStep).toBe(2);
		expect(model.totalSteps).toBe(3);
		expect(model.currentStepLabel).toBe('Summarise revenue');
		expect(deriveVerdict({ ...model, now: NOW }).headline).toBe('Running · step 2 of 3');
	});

	it('ignores `currentStepIndex`, which is 0-based and only written for a local run', () => {
		// A step index of 0 alongside an in-progress second step: an adapter
		// reading the field would report step 1 (or step 0), and the body would
		// disagree with the header about the same run.
		const model = toTaskPanelModel(
			task({ planSteps: STEPS, executionId: 'ex_1', currentStepIndex: 0 }),
			[]
		);
		expect(model.currentStep).toBe(2);
	});

	it('has no denominator when the run was never planned', () => {
		const model = toTaskPanelModel(task({ executionId: 'ex_1' }), []);

		expect(model.totalSteps).toBeNull();
		expect(model.run?.steps).toEqual([]);
	});

	it('carries each step with its recorded duration, and no duration where none was recorded', () => {
		const model = toTaskPanelModel(task({ planSteps: STEPS, executionId: 'ex_1' }), []);

		expect(model.run?.steps).toEqual([
			{
				label: 'Read the exports',
				durationMs: 3_000,
				retries: 0,
				status: 'completed',
				origin: 'step',
				capability: null,
				delegate: null,
				blocking: false
			},
			{
				label: 'Summarise revenue',
				durationMs: null,
				retries: 0,
				status: 'in_progress',
				origin: 'step',
				capability: null,
				delegate: null,
				blocking: false
			},
			{
				label: 'Render the chart',
				durationMs: null,
				retries: 0,
				status: 'pending',
				origin: 'step',
				capability: null,
				delegate: null,
				blocking: false
			}
		]);
	});

	/**
	 * The status was already on the store's `PlanStep` and already read by
	 * `liveStepOf` to find the running step — and then dropped, so the act body
	 * rendered a list in which a failed step and a completed one looked alike.
	 */
	it('carries every step status the store models', () => {
		const statuses: PlanStep['status'][] = [
			'pending',
			'in_progress',
			'completed',
			'failed',
			'skipped',
			'cancelled'
		];
		const model = toTaskPanelModel(
			task({
				executionId: 'ex_1',
				planSteps: statuses.map((status, i) => ({
					id: `s${i}`,
					description: `Step ${status}`,
					status
				}))
			}),
			[]
		);

		expect(model.run?.steps.map((step) => step.status)).toEqual(statuses);
	});

	/**
	 * Both fields were already in the store and already dropped here — the same
	 * shape of loss as the step status above. `extractPlanStepsFromPlanGraph`
	 * fills `tool_name` from `plan_graph.steps[].tool` and `providing_agent_id`
	 * from the agent the planner routed the step to, and the Run act rendered
	 * neither. "Searched memory" is less useful than knowing which agent searched.
	 */
	it('carries the capability and the agent the plan already recorded', () => {
		const model = toTaskPanelModel(
			task({
				executionId: 'ex_1',
				planSteps: [
					{
						id: 's1',
						description: 'Search the corpus',
						status: 'completed',
						tool_name: 'memory_search',
						providing_agent_id: 'research-agent'
					},
					// Half the answer: a capability with nobody named. Distinct from the
					// row below, which names an agent and no capability — a projection
					// reading one field for both cannot satisfy the two of them.
					{ id: 's2', description: 'Draft it', status: 'pending', tool_name: 'write_file' },
					{
						id: 's3',
						description: 'Review it',
						status: 'pending',
						providing_agent_id: 'editor-agent'
					},
					{ id: 's4', description: 'Ship it', status: 'pending' }
				]
			}),
			[]
		);

		expect(
			model.run?.steps.map((step) => [step.capability, step.delegate])
		).toEqual([
			['memory_search', 'research-agent'],
			['write_file', null],
			[null, 'editor-agent'],
			[null, null]
		]);
	});

	it('blanks a capability or an agent recorded as whitespace, rather than rendering a gap', () => {
		// Absence and blank are one answer everywhere else in this module, and a
		// row reading `via ` is worse than a row that says nothing.
		const model = toTaskPanelModel(
			task({
				executionId: 'ex_1',
				planSteps: [
					{
						id: 's1',
						description: 'Search',
						status: 'pending',
						tool_name: '   ',
						providing_agent_id: ''
					}
				]
			}),
			[]
		);

		expect(model.run?.steps[0].capability).toBeNull();
		expect(model.run?.steps[0].delegate).toBeNull();
	});

	it('renders no status for a step whose status this client does not model', () => {
		// `null`, not `pending`: a default would claim the step has not started,
		// which is a different fact from "the record did not say".
		const model = toTaskPanelModel(
			task({
				executionId: 'ex_1',
				planSteps: [
					{ id: 's1', description: 'Step', status: 'quantum' as PlanStep['status'] }
				]
			}),
			[]
		);

		expect(model.run?.steps[0].status).toBeNull();
	});

	it('prefers the active execution id in provenance, so a stale run id is not shown as the live one', () => {
		const model = toTaskPanelModel(
			task({ executionId: 'ex_old', activeExecutionId: 'ex_live' }),
			[]
		);
		expect(model.run?.provenance).toContainEqual({ label: 'Execution id', value: 'ex_live' });
		expect(model.run?.provenance).not.toContainEqual({ label: 'Execution id', value: 'ex_old' });
	});
});

describe('toTaskPanelModel — the ask', () => {
	const asked = task({
		status: 'paused',
		pendingQuestions: [
			{ id: 'q1', question: 'Which quarter?' },
			{ id: 'q2', question: 'Include forecasts?' }
		]
	});

	it('ranks a blocked task as waiting on you, above whatever its status says', () => {
		const model = toTaskPanelModel(asked, []);
		const verdict = deriveVerdict({ ...model, now: NOW });

		expect(verdict.state).toBe('waiting');
		// The question itself, never the generic per-source sentence.
		expect(verdict.detail).toBe('Which quarter?');
	});

	it('omits the blocked duration rather than approximating one from another timestamp', () => {
		const model = toTaskPanelModel(asked, []);

		expect(model.attention?.raisedAt).toBeNull();
		expect(deriveVerdict({ ...model, now: NOW }).headline).toBe('Waiting on you');
	});

	it('opens the ask in the act that holds it', async () => {
		const { defaultOpenAct } = await import('./taskCapabilities');
		const model = toTaskPanelModel({ ...asked, hasPlan: true, executionId: 'ex_1' }, []);

		expect(defaultOpenAct('waiting', ['plan', 'run', 'output'], model.attention!.source)).toBe(
			'plan'
		);
	});

	it('is not blocked by a question that is only whitespace', () => {
		const model = toTaskPanelModel(task({ pendingQuestions: [{ id: 'q1', question: '  ' }] }), []);
		expect(model.attention).toBeNull();
	});

	/**
	 * **The bug, not the gap.** A task blocked mid-run — on an approval, a diff
	 * review, an escalation, a bot sign-in — carries no pending *plan* question,
	 * so it reached this adapter as an ordinary `paused`, which the status map
	 * reads as `queued`. The panel then said `Queued · Waiting for a free slot`
	 * about a task that was waiting on the reader: the loudest possible wrong
	 * answer to the one question the verdict line exists to answer.
	 */
	/**
	 * **B5 — answering a planning question where you are told about it.**
	 *
	 * A plan question keeps the slot it has always had; what is new is that it
	 * arrives with the thing that answers it. The match is by **id**: the id the
	 * task-list row carries for a clarification is the id `/execution-panel`
	 * stamps on that clarification's `hitl_request`, so this is an identifier
	 * check against a contract rather than a text comparison that would work
	 * until someone trimmed a string.
	 *
	 * Every fixture id here is distinct, and the near-miss case exists because a
	 * lookup that fell back to "the first clarification on the payload" would pass
	 * every other test in this file.
	 */
	describe('the plan question, with its answer', () => {
		function questionRow(id: string, prompt: string) {
			return {
				id,
				question_text: prompt,
				status: 'waiting_on_user',
				context_snippets: [],
				related_slots: [],
				options: [],
				hitl_request: {
					id,
					source: 'clarification' as const,
					input_type: 'text' as const,
					prompt,
					identifiers: { correlation_id: id },
					scope: {
						principal: 'anonymous',
						workspace: 'default',
						workflow_id: 'task-1',
						task_id: 'task-1',
						execution_id: 'ex_1'
					}
				}
			};
		}

		function withQuestions(...rows: ReturnType<typeof questionRow>[]): ExecutionPanelState {
			const state = runState('ex_1');
			state.run.pending_questions = rows;
			return state;
		}

		const askedTask = task({
			status: 'paused',
			pendingQuestions: [{ id: 'sq_7c11', question: 'Which quarter?' }]
		});

		it('finds the target the run payload published for this question', () => {
			const model = toTaskPanelModel(
				askedTask,
				[],
				withQuestions(
					questionRow('sq_0000', 'Which region?'),
					questionRow('sq_7c11', 'Which quarter?')
				)
			);

			expect(model.attention?.summary).toBe('Which quarter?');
			expect(model.ask?.id).toBe('sq_7c11');
			expect(model.ask?.input_type).toBe('text');
		});

		it('offers no control when the payload published none for this question', () => {
			// The task row says it is blocked and the run payload has not caught up.
			// The verdict is unchanged — the ask is real either way — and there is
			// simply nothing to answer it with.
			const model = toTaskPanelModel(askedTask, [], withQuestions(questionRow('sq_0000', 'Which region?')));

			expect(model.attention?.summary).toBe('Which quarter?');
			expect(model.ask).toBeNull();
		});

		it('never answers a different question than the one it is describing', () => {
			// One row, wrong id. A lookup that took "the first clarification"
			// would post an answer to `sq_0000` under the words of `sq_7c11`.
			const model = toTaskPanelModel(
				task({ status: 'paused', pendingQuestions: [{ id: 'sq_7c11', question: 'Which quarter?' }] }),
				[],
				withQuestions(questionRow('sq_0000', 'Which quarter?'))
			);

			expect(model.ask).toBeNull();
		});

		it('has nothing to answer with when no run payload was read at all', () => {
			const model = toTaskPanelModel(askedTask, [], null);
			expect(model.attention?.summary).toBe('Which quarter?');
			expect(model.ask).toBeNull();
		});
	});

	/**
	 * **B6 — a draft plan is an ask, and read as `Queued` until now.**
	 *
	 * `planStatus: 'draft'` normalises to task status `pending`, which the status
	 * map turns into `queued`, so a plan waiting for the reader's approval said
	 * `Queued · Waiting for a free slot`. That is why `approvePlan` and
	 * `rejectPlan` had no call site reachable from this route: nothing on the
	 * panel said an approval was wanted.
	 */
	describe('the plan awaiting approval', () => {
		const draft = task({ status: 'pending', planStatus: 'draft', latestPlanId: 'plan_31f', planGeneratedAt: NOW - 9 * 60_000 });

		it('reads Waiting on you, and carries the approval as the ask', () => {
			const model = toTaskPanelModel(draft, []);
			const verdict = deriveVerdict({ ...model, now: NOW });

			expect(verdict.state).toBe('waiting');
			expect(verdict.headline).toBe('Waiting on you · 9m');
			expect(verdict.detail).toBe('Approve the plan before it can run');
			expect(verdict.headline).not.toMatch(/queued/i);
		});

		it('answers against the plan id, which is what the dispatcher looks the plan up by', () => {
			const model = toTaskPanelModel(draft, []);

			expect(model.ask?.source).toBe('plan_approval');
			expect(model.ask?.input_type).toBe('confirmation');
			expect(model.ask?.id).toBe('plan_31f');
			expect(model.ask?.identifiers?.correlation_id).toBe('plan_31f');
			// The responder identity the dispatcher requires alongside it.
			expect(model.ask?.scope?.task_id).toBe(draft.id);
		});

		it('opens the Plan act, because approving a plan acts on what that act renders', async () => {
			const { defaultOpenAct } = await import('./taskCapabilities');
			const model = toTaskPanelModel({ ...draft, hasPlan: true }, []);

			expect(defaultOpenAct('waiting', ['plan', 'run', 'output'], model.attention!.source)).toBe(
				'plan'
			);
		});

		it('says nothing at all when there is no plan id to answer against', () => {
			// An ask that cannot be answered is worse than a status that is vague:
			// the panel would say `Waiting on you` under a control that posts to
			// nothing. So the task keeps the verdict its own status earns.
			const model = toTaskPanelModel(task({ status: 'pending', planStatus: 'draft' }), []);

			expect(model.attention).toBeNull();
			expect(model.ask).toBeNull();
			expect(deriveVerdict({ ...model, now: NOW }).state).toBe('queued');
		});

		it('is only for a draft — an approved plan is not asking anything', () => {
			const model = toTaskPanelModel(
				task({ status: 'pending', planStatus: 'approved', latestPlanId: 'plan_31f' }),
				[]
			);
			expect(model.attention).toBeNull();
		});

		it('yields to the ask the wire published, which is the same approval with more on it', () => {
			// Phase F surfaces plan approval as a HITL in `/attention`. When that
			// row reaches the payload it wins: it carries the instant it was raised
			// and the identity the backend chose, and this fallback exists only for
			// the common case where a task awaiting approval has no live execution
			// for `/execution-panel` to describe.
			const published = runState('ex_1', {
				attention: [
					{
						id: 'feed-9',
						principal: 'anonymous',
						workspace: 'default',
						item_type: 'approval',
						task_id: 'task-1',
						title: 'Approval requested',
						summary: 'Plan approval',
						status: 'needs_action',
						created_at: NOW - 3 * 60_000,
						updated_at: NOW - 3 * 60_000,
						actions: [],
						metadata: {},
						hitl_request: {
							id: 'plan_wire_88',
							source: 'plan_approval' as const,
							input_type: 'confirmation' as const,
							prompt: 'Approve the revenue plan?',
							identifiers: { correlation_id: 'plan_wire_88' },
							scope: { principal: 'anonymous', workspace: 'default', task_id: 'task-1' },
							at: NOW - 3 * 60_000
						}
					}
				]
			});
			const model = toTaskPanelModel({ ...draft, executionId: 'ex_1' }, [], published);

			expect(model.ask?.id).toBe('plan_wire_88');
			expect(model.attention?.summary).toBe('Approve the revenue plan?');
		});
	});

	describe('the mid-run ask', () => {
		const blocked = task({ status: 'paused', executionId: 'ex_1' });
		/**
		 * The payload a blocked task's run reports. The two instants differ on
		 * purpose — the feed row's and the request's — because the reading order
		 * between them is the thing under test, and a reader of the wrong half gets
		 * a plausible sentence and a duration wrong by minutes.
		 */
		const diffReview = runState('ex_1', {
			attention: [
				{
					id: 'feed-1',
					principal: 'anonymous',
					workspace: 'default',
					item_type: 'approval',
					task_id: 'task-1',
					title: 'Approval requested',
					summary: 'The lane label, not the ask',
					status: 'needs_action',
					created_at: NOW - 30 * 60_000,
					updated_at: NOW - 30 * 60_000,
					actions: [],
					metadata: {},
					hitl_request: {
						id: 'ccp-9f2c',
						source: 'diff_approval',
						input_type: 'diff_approval',
						prompt: 'Apply 3 edits to revenue.py?',
						at: NOW - 12 * 60_000
					}
				}
			]
		});

		it('reads Paused when nothing says who must act', () => {
			// A deliberate pause must remain distinct from queueing. Once an ask is
			// observed below, attention still outranks this lifecycle state.
			const verdict = deriveVerdict({ ...toTaskPanelModel(blocked, []), now: NOW });
			expect(verdict.state).toBe('paused');
			expect(verdict.detail).toBe('Ready to resume when you are');
		});

		it('reads Waiting on you once the ask reaches it, never Queued', () => {
			const model = toTaskPanelModel(blocked, [], diffReview);
			const verdict = deriveVerdict({ ...model, now: NOW });

			expect(verdict.state).toBe('waiting');
			expect(verdict.headline).toBe('Waiting on you · 12m');
			expect(verdict.detail).toBe('Apply 3 edits to revenue.py?');
			expect(verdict.headline).not.toMatch(/queued/i);
		});

		it('routes to the act the ask lives in, which is not the one a plan question opens', async () => {
			const { defaultOpenAct } = await import('./taskCapabilities');
			const model = toTaskPanelModel(
				{ ...blocked, hasPlan: true, executionId: 'ex_1' },
				[],
				diffReview
			);

			// The source is a fact the backend supplied rather than this module's
			// routing choice, and it is the reason a mid-run review opens Run while
			// a plan clarification opens Plan.
			expect(model.attention?.source).toBe('diff_approval');
			expect(defaultOpenAct('waiting', ['plan', 'run', 'output'], model.attention!.source)).toBe(
				'run'
			);
		});

		it('leaves a plan question in charge, so nothing that reads correctly today changes', () => {
			// Both present. The plan question is the ask this surface can *show* —
			// the Plan act renders it — so it keeps the slot, and the fix is purely
			// additive to the tasks that had no ask at all.
			const model = toTaskPanelModel(asked, [], diffReview);

			expect(model.attention?.source).toBe('clarification');
			expect(model.attention?.summary).toBe('Which quarter?');
		});

		/**
		 * **B5 — the control, off the same row as the verdict.**
		 *
		 * The target was always on this payload and was narrowed away. Nothing is
		 * fetched to recover it, and the identity check is the point: a verdict
		 * describing one ask over a control answering another is a panel where every
		 * value on screen is individually correct.
		 */
		it('keeps the answerable target for the ask it just described', () => {
			const model = toTaskPanelModel(blocked, [], diffReview);

			expect(model.ask?.id).toBe('ccp-9f2c');
			expect(model.ask?.input_type).toBe('diff_approval');
			expect(model.ask?.source).toBe(model.attention?.source);
			expect(model.ask?.prompt).toBe(model.attention?.summary);
		});

		it('describes an ask whose target it cannot use, and offers no control for it', () => {
			const unrenderable = runState('ex_1', {
				attention: [
					{
						...diffReview.run.needs_attention[0],
						// A twelfth variant. The declared type says this cannot happen and
						// vitest transpiles without type-checking, which is exactly the
						// gap `askTargetFrom` exists to close.
						hitl_request: {
							...diffReview.run.needs_attention[0].hitl_request!,
							input_type: 'holographic_gesture' as never
						}
					}
				]
			});
			const model = toTaskPanelModel(blocked, [], unrenderable);

			expect(model.attention?.source).toBe('diff_approval');
			expect(model.ask).toBeNull();
		});

		it('degrades to the task\'s own verdict when the run state could not be read', () => {
			// `null` is both "nothing is asking" and "the request failed", and it
			// has to be: an absent ask renders nothing, so neither spelling can
			// make the panel assert something it did not learn.
			const model = toTaskPanelModel(task({ status: 'running' }), [], null);
			expect(model.attention).toBeNull();
			expect(deriveVerdict({ ...model, now: NOW }).state).toBe('running');
		});
	});
});

/**
 * **The Run act's second list, on a task-backed surface.**
 *
 * The events were always in the payload this adapter's third argument now
 * carries — the request that fetches the mid-run ask reads the whole
 * `/execution-panel` response and used to keep one field of it. What these pin
 * is the part that is genuinely this module's: which run the events are read
 * under, and the difference between a run that recorded none and one nobody
 * read.
 */
/**
 * Delegated subtasks — the shape of the work, which flat rows lose entirely.
 *
 * The only nesting any payload this panel reads actually states: a run's
 * **children**, each with the sub-goal it was given and the agent that owns it.
 * What the wire does **not** state is which *step* delegated a child —
 * `ExecutionPanelResponsibilityChild` carries no `parent_step_id`, though the
 * server-side delegation record does — so these rows are children of the run and
 * of nothing narrower, and that is what `origin` says.
 *
 * **Every fixture value below is distinct**: two children, two agents, two
 * sub-goals, and a waiting state per case that no other case uses. A projection
 * reading the wrong child, or the wrong field of the right one, cannot coincide
 * with the expected answer.
 */
describe('toTaskPanelModel — delegated subtasks', () => {
	const child = (
		overrides: Partial<ExecutionPanelResponsibilityChild> = {}
	): ExecutionPanelResponsibilityChild => ({
		execution_id: 'ex_child_a',
		title: 'Pull the regional exports',
		waiting_state: 'Executing',
		active_owner_agent_id: 'research-agent',
		delegation_chain: ['ex_1', 'ex_child_a'],
		is_blocking: true,
		...overrides
	});

	const responsibility = (
		executionId: string,
		children: ExecutionPanelResponsibilityChild[]
	): ExecutionPanelResponsibilityState => ({
		execution_id: executionId,
		waiting_state: 'WaitingChildren',
		active_owner_agent_id: 'personal-assistant',
		// **Agent ids, not execution ids.** `build_owner_chain` in
		// `execution_panel/v3_adapter.rs` pushes `node.agent_id`; it is
		// `build_execution_chain`, which fills the *child's* `delegation_chain`,
		// that pushes `node.execution_id`. The fixture said execution ids in both
		// places, and nothing noticed because nothing read `owner_chain` — a
		// fixture asserting a shape the backend cannot produce, harmless right up
		// until the field was rendered.
		//
		// `owner_stack` is assigned the *same expression* as `owner_chain` by that
		// adapter, so it is spelled identically here rather than differently.
		owner_stack: ['personal-assistant'],
		owner_chain: ['personal-assistant'],
		handover_active: false,
		waiting_on_children: true,
		active_child_count: children.length,
		historical_child_count: children.length,
		responsibility_summary: 'Waiting on delegated work',
		active_children: children
	});

	const delegating = (children: ExecutionPanelResponsibilityChild[], executionId = 'ex_1') =>
		toTaskPanelModel(
			task({ status: 'running', executionId: 'ex_1', planSteps: STEPS }),
			[],
			runState('ex_1', { responsibility: responsibility(executionId, children) })
		);

	it('reads a delegated run as a subtask of the run, under its own agent', () => {
		const model = delegating([
			child(),
			child({
				execution_id: 'ex_child_b',
				title: 'Draft the regional summary',
				waiting_state: 'Completed',
				active_owner_agent_id: 'writer-agent',
				is_blocking: false
			})
		]);

		// Three plan steps, then the two delegated rows — never interleaved, and
		// never counted as plan structure.
		expect(model.run?.steps.map((step) => step.origin)).toEqual([
			'step',
			'step',
			'step',
			'delegated',
			'delegated'
		]);
		expect(model.run?.steps.slice(3)).toEqual([
			{
				label: 'Pull the regional exports',
				durationMs: null,
				retries: 0,
				status: 'in_progress',
				origin: 'delegated',
				capability: null,
				delegate: 'research-agent',
				blocking: true
			},
			{
				label: 'Draft the regional summary',
				durationMs: null,
				retries: 0,
				status: 'completed',
				origin: 'delegated',
				capability: null,
				delegate: 'writer-agent',
				blocking: false
			}
		]);
	});

	/**
	 * **`is_blocking` is not the child's status, and the two disagree on real
	 * payloads.** `active_children` is every child the run ever delegated — the
	 * wire's field name is not a filter — and the backend answers `is_blocking`
	 * off `active_child_execution_ids`, which is the subset the parent is still
	 * held by. A child can be `Executing` and not blocking (the parent moved on),
	 * and the pair below is chosen so a projection reading the status instead
	 * fails: both children are `Executing` and only one of them blocks.
	 */
	it('says which delegated child the run is still held by, not merely which is running', () => {
		const model = delegating([
			child({ execution_id: 'ex_child_a', waiting_state: 'Executing', is_blocking: true }),
			child({
				execution_id: 'ex_child_b',
				title: 'Draft the regional summary',
				waiting_state: 'Executing',
				active_owner_agent_id: 'writer-agent',
				is_blocking: false
			})
		]);

		expect(model.run?.steps.slice(3).map((step) => [step.status, step.blocking])).toEqual([
			['in_progress', true],
			['in_progress', false]
		]);
	});

	it('never marks one of the run’s own steps as blocking it', () => {
		// A plan step is work the run did itself; there is no other run for it to
		// be waiting on. Pinned because the field is a `boolean` on every row and a
		// projection that set it from the delegated branch alone would leave the
		// plan rows `undefined`, which renders identically to `false` today.
		const model = delegating([child()]);

		expect(model.run?.steps.slice(0, 3).map((step) => step.blocking)).toEqual([
			false,
			false,
			false
		]);
	});

	/**
	 * The block that answers "who is doing this and what is it waiting on" — the
	 * only account of a delegated multi-agent run outside `/debug`.
	 *
	 * Every value below is distinct from every other, on this file's own rule: two
	 * agents, a chain that is neither of them repeated, a total that is not the
	 * blocking count, and a waiting state that appears nowhere else in the fixture.
	 */
	it('says who holds a delegated run, how ownership reached them, and what it waits on', () => {
		const model = delegating([
			child({ execution_id: 'ex_child_a', is_blocking: true }),
			child({ execution_id: 'ex_child_b', is_blocking: false }),
			child({ execution_id: 'ex_child_c', is_blocking: false })
		]);

		expect(model.run?.responsibility).toEqual({
			owner: 'personal-assistant',
			ownerChain: ['personal-assistant'],
			state: 'Waiting on the work it delegated',
			blocking: 1,
			total: 3
		});
	});

	it('reads the owner off the end of the chain, which is where a handover shows', () => {
		// `owner_chain` is root-first and ends at the current owner, so the two
		// fields agree on a run nobody handed over. They stop agreeing the moment
		// one does, and the chain is the field that can say so — asserted with a
		// chain whose last entry is deliberately **not** `active_owner_agent_id`,
		// which is the only arrangement that can tell the two reads apart.
		const model = delegating([child()], 'ex_1');
		const snapshot = model.run?.responsibility;
		expect(snapshot?.ownerChain).toEqual(['personal-assistant']);
		expect(snapshot?.owner).toBe('personal-assistant');

		const handed = toTaskPanelModel(
			task({ status: 'running', executionId: 'ex_1', planSteps: STEPS }),
			[],
			runState('ex_1', {
				responsibility: {
					...responsibility('ex_1', [child()]),
					owner_chain: ['personal-assistant', 'research-agent'],
					active_owner_agent_id: 'personal-assistant'
				}
			})
		);
		expect(handed.run?.responsibility?.ownerChain).toEqual([
			'personal-assistant',
			'research-agent'
		]);
		expect(handed.run?.responsibility?.owner).toBe('research-agent');
	});

	it('has no responsibility block for a run that delegated nothing', () => {
		// Absent, never an empty tree: a run with no children has an owner and a
		// state, and the verdict line and the step list already say both better.
		const model = delegating([]);
		expect(model.run?.responsibility).toBeNull();

		// And absent for a payload that carries no snapshot at all, which is every
		// task whose run state has not been fetched.
		const unread = toTaskPanelModel(
			task({ status: 'running', executionId: 'ex_1', planSteps: STEPS }),
			[]
		);
		expect(unread.run?.responsibility).toBeNull();
	});

	it('refuses a responsibility snapshot that names another run', () => {
		// The same guard the delegated rows carry, and it has to be the same
		// answer: a task row that moved on between fetches produces a snapshot
		// about the previous attempt, and every value in it would corroborate the
		// wrong heading.
		const model = delegating([child()], 'ex_other');
		expect(model.run?.responsibility).toBeNull();
		expect(model.run?.steps.every((step) => step.origin === 'step')).toBe(true);
	});

	it('renders no waiting line for a state this client does not model', () => {
		// `null`, not the enum. A fallback that echoed the wire would put
		// `SomethingNew` in front of a reader, which is the leak the verdict line
		// forbids one field over.
		const model = toTaskPanelModel(
			task({ status: 'running', executionId: 'ex_1', planSteps: STEPS }),
			[],
			runState('ex_1', {
				responsibility: {
					...responsibility('ex_1', [child()]),
					waiting_state: 'SomethingNew'
				}
			})
		);

		expect(model.run?.responsibility?.state).toBeNull();
		expect(JSON.stringify(model.run?.responsibility)).not.toContain('SomethingNew');
	});

	it('leaves the plan counts to the plan, however much work the run delegated', () => {
		// The failure this prevents: `step 2 of 5` about a three-step plan, with
		// both numbers individually plausible and nothing on screen to check them
		// against. `totalSteps` and the live position are read off the plan, and
		// two delegated rows must not move either.
		const model = delegating([child(), child({ execution_id: 'ex_child_b' })]);

		expect(model.totalSteps).toBe(3);
		expect(model.currentStep).toBe(2);
		expect(model.currentStepLabel).toBe('Summarise revenue');
	});

	it('keeps a child whose sub-goal the payload did not name', () => {
		// Unlike an output row with no path, there **is** something to say about
		// it — which agent owns it and what state it is in — and dropping it would
		// make the run look like it delegated less work than it did.
		const model = delegating([child({ title: null }), child({ title: '   ' })]);

		expect(model.run?.steps.slice(3).map((step) => [step.label, step.delegate])).toEqual([
			['Delegated work', 'research-agent'],
			['Delegated work', 'research-agent']
		]);
	});

	/**
	 * The wire word is `WaitingState`, serialised as its Rust **variant name** —
	 * the enum carries no `rename_all` while its siblings do. So these keys are
	 * PascalCase and not the snake_case the rest of the payload uses. A fixture
	 * written in snake_case would be a fixture asserting a shape the backend
	 * cannot produce, and would have passed while the mapping was dead.
	 */
	it('maps the child states a delegated run really reports', () => {
		const states = [
			'Planning',
			'PlanningComplete',
			'Runnable',
			'Executing',
			'WaitingChildren',
			'WaitingUser',
			'Paused',
			'Completed',
			'Failed',
			'Cancelled',
			// Not a state this client knows. `null`, so the row renders no mark
			// rather than a plausible wrong one.
			'Quiesced'
		];
		const model = delegating(
			states.map((waiting_state, index) =>
				child({ execution_id: `ex_child_${index}`, waiting_state })
			)
		);

		expect(model.run?.steps.slice(3).map((step) => step.status)).toEqual([
			'pending',
			'pending',
			'pending',
			'in_progress',
			// Waiting on its **own** children is still mid-run: nothing is being
			// asked of the reader, and `waiting` on this panel means stopped.
			'in_progress',
			// Blocked on the reader, and suspended, are the two that are.
			'waiting',
			'waiting',
			'completed',
			'failed',
			'cancelled',
			null
		]);
	});

	it('refuses one run the children of another', () => {
		// The task row names the run; the payload is fetched separately and can
		// describe a different one whenever the row moved on between fetches. Every
		// value in the wrong payload is individually valid, so nothing downstream
		// could catch it — the same disagreement `deriveTimeline` guards one field
		// over, and the same answer: nothing rather than the other run's rows.
		const model = delegating([child()], 'ex_2');

		expect(model.run?.steps.map((step) => step.origin)).toEqual(['step', 'step', 'step']);
	});

	it('lists only the plan when no run payload reached the adapter', () => {
		const model = toTaskPanelModel(
			task({ status: 'running', executionId: 'ex_1', planSteps: STEPS }),
			[]
		);

		expect(model.run?.steps.map((step) => step.origin)).toEqual(['step', 'step', 'step']);
	});
});

describe('toTaskPanelModel — the Run act timeline', () => {
	const running = task({ status: 'running', executionId: 'ex_1' });
	const call = activity('log-1', 40_000, {
		event_type: 'llm.succeeded',
		capability: 'research',
		model: 'claude-opus-4'
	});

	it('reads the run state\'s events into the act the task row already named', () => {
		const model = toTaskPanelModel(running, [], runState('ex_1', { activity: [call] }));

		expect(model.run?.timeline).toHaveLength(1);
		expect(model.run?.timeline?.[0].title).toBe('Thinking with research');
		// The act's provenance and the act's rows are about one run, which is the
		// invariant the id is threaded through the adapter to keep.
		expect(model.run?.provenance).toContainEqual({ label: 'Execution id', value: 'ex_1' });
	});

	it('has no timeline when no run state was fetched, rather than an empty one', () => {
		// `[]` would render `0 events` about a run that may have made two hundred
		// calls; the store `Task` on its own has observed nothing.
		expect(toTaskPanelModel(running, []).run?.timeline).toBeNull();
	});

	it('says the run recorded nothing only when the payload actually said so', () => {
		expect(toTaskPanelModel(running, [], runState('ex_1')).run?.timeline).toEqual([]);
	});

	/**
	 * The failure this adapter is uniquely exposed to. Its run's id comes off a
	 * store row and its events off a payload fetched separately, so a row that
	 * advanced to a retry between the two would put the previous attempt's calls
	 * under the new attempt's heading — every value correct, the whole list wrong,
	 * and nothing on screen able to contradict it.
	 */
	it('refuses a payload describing an execution the task row has moved on from', () => {
		const model = toTaskPanelModel(
			task({ status: 'running', activeExecutionId: 'ex_2', executionId: 'ex_1' }),
			[],
			runState('ex_1', { activity: [call] })
		);

		expect(model.run?.provenance).toContainEqual({ label: 'Execution id', value: 'ex_2' });
		expect(model.run?.timeline).toBeNull();
	});

	/**
	 * L3 gained the one question the act could not answer and lost a duplicate.
	 *
	 * **The totals are summed from the very entries the body renders**, off one
	 * derivation — so they cannot describe a different run than the rows beneath
	 * them, which is the failure a second `deriveTimeline` call in this function
	 * would have made possible and nothing on screen could contradict.
	 */
	it('sums what the run cost from the events it renders, in the act\'s own disclosure', () => {
		const billed = (id: string, at: number) =>
			activity(id, at, {
				event_type: 'llm.succeeded',
				capability: 'research',
				model: 'claude-opus-4',
				input_tokens: 12_000,
				output_tokens: 384,
				cache_read_tokens: 9_600,
				cost_usd: 0.00610875,
				latency_ms: 4_200
			});
		const model = toTaskPanelModel(
			running,
			[],
			runState('ex_1', { activity: [billed('log-1', 40_000), billed('log-2', 70_000)] })
		);

		expect(model.run?.provenance).toEqual([
			{ label: 'Execution id', value: 'ex_1' },
			{ label: 'Cost', value: '$0.0122175' },
			{ label: 'Tokens', value: '24k → 768 tok · 2 calls' },
			// 19,200 of 24,000 input tokens, off `input` alone.
			{ label: 'Prompt cache', value: '80% cached · 19k of 24k tok' },
			// 8.4s inside the model across a 30s feed.
			{ label: 'Model time', value: '8s of 30s observed · 28%' },
			{ label: 'Model', value: 'claude-opus-4' }
		]);
	});

	it('does not repeat the task id the drawer\'s control row already carries', () => {
		// And reports no cost for a run whose events were read and carried none —
		// one row, not a row of zeros.
		const model = toTaskPanelModel(running, [], runState('ex_1'));
		expect(model.run?.provenance.map((entry) => entry.label)).toEqual(['Execution id']);
	});

	it('has no Run act at all for a task with no execution, so no timeline either', () => {
		expect(toTaskPanelModel(task({ status: 'ready' }), [], runState('ex_1', { activity: [call] })).run).toBeNull();
	});
});

describe('toTaskPanelModel — the written report', () => {
	it('carries the completion summary the store already holds', () => {
		const model = toTaskPanelModel(
			task({ status: 'completed', completionSummary: '## Revenue\n\nNorth rose 12%.' }),
			[]
		);

		expect(model.output?.summary).toBe('## Revenue\n\nNorth rose 12%.');
	});

	it('treats a blank summary as none, so no empty block opens above the files', () => {
		expect(toTaskPanelModel(task({ completionSummary: '   ' }), []).output?.summary).toBeNull();
		expect(toTaskPanelModel(task({}), []).output?.summary).toBeNull();
	});

	/**
	 * The residue, recorded rather than hidden: the report rides on the Output
	 * act, and that act is absent when the *files* request failed. A panel built
	 * on half an observation would have to claim something about files nobody
	 * read, so absent is the answer that claims least — and the panel's Retry
	 * re-reads both.
	 */
	it('is absent with the act when the outputs could not be read', () => {
		const model = toTaskPanelModel(task({ completionSummary: 'A report' }), null);
		expect(model.output).toBeNull();
	});
});

describe('output files', () => {
	it('names a file by its basename and keeps the path it can be opened with', () => {
		expect(
			outputFilesFrom([{ relative_path: 'reports/q3/summary.md', media_type: 'text/markdown' }])
		).toEqual([
			{
				name: 'summary.md',
				kind: 'document',
				path: 'reports/q3/summary.md',
				mediaType: 'text/markdown',
				sizeBytes: null,
				// No minter passed, so no URL — and every affordance that needs one is
				// then absent rather than pointing at an address nothing serves.
				url: null
			}
		]);
	});

	/**
	 * The three fields a row needs and the summary line never did. The component
	 * doc called an image thumbnail "blocked on a per-file URL the model doesn't
	 * carry" — true of the shape, and never true of the endpoint, which has always
	 * sent a path and a mime and whose GET route serves the bytes.
	 */
	it('carries the mime, the size and an address the caller minted', () => {
		const [file] = outputFilesFrom(
			[{ relative_path: 'charts/revenue.png', media_type: 'image/png', size_bytes: 20_480 }],
			(path) => `/api/magician/v3/tasks/t1/outputs/${path}`
		);

		expect(file.mediaType).toBe('image/png');
		expect(file.sizeBytes).toBe(20_480);
		expect(file.url).toBe('/api/magician/v3/tasks/t1/outputs/charts/revenue.png');
	});

	it('reports the mime as sent and never one inferred from the name', () => {
		// `outputKindOf` above *does* fall back to the extension, and rightly: it
		// is choosing a plural for a summary line. This is shown to the reader as
		// a fact about the file, so a guess printed as one is the thing to avoid.
		const [file] = outputFilesFrom([{ relative_path: 'notes/summary.md' }]);

		expect(file.kind).toBe('document');
		expect(file.mediaType).toBeNull();
	});

	it('has no address when no caller minted one, rather than a half-built path', () => {
		const [file] = outputFilesFrom([{ relative_path: 'q3/report.md' }]);
		expect(file.url).toBeNull();
	});

	it('drops a size that is not a finite number', () => {
		const [file] = outputFilesFrom([
			{ relative_path: 'q3/report.md', size_bytes: Number.NaN }
		]);
		expect(file.sizeBytes).toBeNull();
	});

	it('keeps two files that share a basename apart by their paths', () => {
		// The reason the panel reports an index rather than a name: these two
		// rows are indistinguishable by the only field the panel renders.
		const files = outputFilesFrom([
			{ relative_path: 'alpha/report.md', media_type: 'text/markdown' },
			{ relative_path: 'beta/report.md', media_type: 'text/markdown' }
		]);

		expect(files.map((file) => file.name)).toEqual(['report.md', 'report.md']);
		expect(files.map((file) => file.path)).toEqual(['alpha/report.md', 'beta/report.md']);
	});

	it('drops a row with no path rather than rendering a file it cannot name or open', () => {
		expect(outputFilesFrom([{ media_type: 'text/markdown' }, { relative_path: '  ' }])).toEqual([]);
	});

	it('classifies from the media type first and the extension only when there is none', () => {
		expect(outputKindOf('image/png', 'chart.bin')).toBe('image');
		expect(outputKindOf('application/pdf', 'notes')).toBe('document');
		expect(outputKindOf('application/json', 'data.md')).toBe('other');
		expect(outputKindOf(null, 'revenue.PNG')).toBe('image');
		expect(outputKindOf(undefined, 'report.md')).toBe('document');
		expect(outputKindOf('', 'archive.tar.gz')).toBe('other');
	});

	it('names the shared directory once instead of repeating each name as its own path', () => {
		const model = toTaskPanelModel(
			task({ executionId: 'ex_1' }),
			outputFilesFrom([{ relative_path: 'reports/summary.md', media_type: 'text/markdown' }])
		);

		// This asserted `{ label: 'summary.md', value: 'reports/summary.md' }` — a row
		// whose label is its own value plus a directory. Measured on a real task, that
		// shape produced three rows repeating names already listed in the act body.
		expect(model.output?.provenance).toEqual([
			{ label: 'Source execution id', value: 'ex_1' },
			{ label: 'Directory', value: 'reports' }
		]);
	});

	it('falls back to per-file paths when the files do not share a directory', () => {
		const model = toTaskPanelModel(
			task({ executionId: 'ex_1' }),
			outputFilesFrom([
				{ relative_path: 'reports/summary.md', media_type: 'text/markdown' },
				{ relative_path: 'charts/revenue.png', media_type: 'image/png' }
			])
		);

		// Here the path is the only thing telling the two apart, so it earns its row.
		expect(model.output?.provenance).toEqual([
			{ label: 'Source execution id', value: 'ex_1' },
			{ label: 'summary.md', value: 'reports/summary.md' },
			{ label: 'revenue.png', value: 'charts/revenue.png' }
		]);
	});

	it('names no directory for a flat listing, rather than asserting an empty one', () => {
		const model = toTaskPanelModel(
			task({ executionId: 'ex_1' }),
			outputFilesFrom([{ relative_path: 'summary.md', media_type: 'text/markdown' }])
		);

		expect(model.output?.provenance).toEqual([{ label: 'Source execution id', value: 'ex_1' }]);
	});
});

/**
 * The run picker, from the adapter's side: which runs it offers, which one is
 * selected, and what the selection does and does not move.
 *
 * The options come off `output.recent_runs`, a field this adapter previously
 * ignored — so these are also the first assertions that the response's own list
 * of the task's runs reaches the panel at all.
 */
function recentRun(
	executionId: string,
	startedAt: number,
	status: TaskStatus
): ExecutionPanelRecentRun {
	return {
		execution_id: executionId,
		started_at: startedAt,
		ended_at: null,
		status,
		completion_summary: null,
		completion_outcome: null,
		completion_artifact_names: [],
		current_step: null,
		progress: null,
		error_message: null
	};
}

/** Three runs of one task, oldest first: two failures then the run that worked. */
const THREE_RUNS: ExecutionPanelRecentRun[] = [
	recentRun('ex_1', 100_000, 'failed'),
	recentRun('ex_2', 200_000, 'failed'),
	recentRun('ex_3', 300_000, 'completed')
];

describe('the run picker', () => {
	it('offers nothing when the payload lists one run — absent, not disabled', () => {
		const model = toTaskPanelModel(
			task({ executionId: 'ex_1' }),
			[],
			runState('ex_1', { recentRuns: [recentRun('ex_1', 100_000, 'completed')] })
		);

		expect(model.runs).toBeNull();
	});

	it('offers nothing when there is no run payload at all', () => {
		expect(toTaskPanelModel(task({ executionId: 'ex_1' }), [], null).runs).toBeNull();
	});

	it('offers every run the payload lists, newest first, with readable labels', () => {
		const model = toTaskPanelModel(
			task({ executionId: 'ex_3' }),
			[],
			runState('ex_3', { recentRuns: THREE_RUNS })
		);

		expect(model.runs?.options.map((option) => option.executionId)).toEqual([
			'ex_3',
			'ex_2',
			'ex_1'
		]);
		// The status words are the **verdict's** vocabulary, not the wire's: the row
		// said `completed` and the option says `finished`, which is the word the
		// headline above it uses for the same outcome.
		expect(model.runs?.options.map((option) => option.status)).toEqual([
			'finished',
			'failed',
			'failed'
		]);
		for (const option of model.runs?.options ?? []) {
			expect(option.label).not.toContain(option.executionId);
			expect(option.label.startsWith(`#${option.ordinal}`)).toBe(true);
		}
	});

	it('selects the task’s current run by default, so nothing changes for a caller that passes none', () => {
		const model = toTaskPanelModel(
			task({ activeExecutionId: 'ex_3', executionId: 'ex_1' }),
			[],
			runState('ex_3', { recentRuns: THREE_RUNS })
		);

		expect(model.runs?.selectedId).toBe('ex_3');
	});

	it('selects the run the caller asked for', () => {
		const model = toTaskPanelModel(
			task({ activeExecutionId: 'ex_3' }),
			[],
			runState('ex_2', { recentRuns: THREE_RUNS }),
			'ex_2'
		);

		expect(model.runs?.selectedId).toBe('ex_2');
	});

	/**
	 * **The correctness requirement.** A task that eventually finished must not read
	 * `Failed` because the reader is looking at an earlier attempt: the verdict is
	 * about the task and the acts are about the run.
	 */
	it('leaves the verdict describing the task when an earlier run is selected', () => {
		const finished = task({ status: 'completed', activeExecutionId: 'ex_3' });
		const current = toTaskPanelModel(finished, [], runState('ex_3', { recentRuns: THREE_RUNS }));
		const earlier = toTaskPanelModel(
			finished,
			[],
			runState('ex_1', { recentRuns: THREE_RUNS }),
			'ex_1'
		);

		expect(current.status).toBe('finished');
		expect(earlier.status).toBe('finished');
		expect(deriveVerdict({ ...earlier, now: NOW }).state).toBe('finished');
		// And the control says which run the acts are about, so the pair is legible.
		expect(earlier.runs?.options.find((option) => option.executionId === 'ex_1')?.status).toBe(
			'failed'
		);
	});

	it('names the selected run in the Run act’s provenance', () => {
		const model = toTaskPanelModel(
			task({ activeExecutionId: 'ex_3' }),
			[],
			runState('ex_1', { recentRuns: THREE_RUNS }),
			'ex_1'
		);

		expect(model.run?.provenance[0]).toEqual({ label: 'Execution id', value: 'ex_1' });
	});

	/**
	 * The plan record carries one set of step marks and they are the **latest**
	 * run's. Rendering them under an earlier run's timeline would put the winning
	 * attempt's green ticks over the failed attempt's events.
	 */
	it('drops the plan’s step marks when the selected run is not the one they describe', () => {
		const row = task({ planSteps: STEPS, activeExecutionId: 'ex_3' });

		const current = toTaskPanelModel(row, [], runState('ex_3', { recentRuns: THREE_RUNS }));
		expect(current.run?.steps.map((step) => step.status)).toEqual([
			'completed',
			'in_progress',
			'pending'
		]);
		expect(current.run?.steps[0]?.durationMs).toBe(3_000);

		const earlier = toTaskPanelModel(row, [], runState('ex_1', { recentRuns: THREE_RUNS }), 'ex_1');
		// The rows stay — that plan is what this run executed — but nothing claims how
		// this run fared on each step, because nothing recorded it.
		expect(earlier.run?.steps.map((step) => step.label)).toEqual(
			STEPS.map((step) => step.description)
		);
		expect(earlier.run?.steps.map((step) => step.status)).toEqual([null, null, null]);
		expect(earlier.run?.steps.every((step) => step.durationMs === null)).toBe(true);
	});

	/**
	 * The in-flight window: the reader has picked `ex_1` and the payload still
	 * describes `ex_3`. Absent beats another run's events under this run's heading.
	 */
	it('renders no timeline while the payload still describes the previous run', () => {
		const model = toTaskPanelModel(
			task({ planSteps: STEPS, activeExecutionId: 'ex_3' }),
			[],
			runState('ex_3', { recentRuns: THREE_RUNS }),
			'ex_1'
		);

		expect(model.run?.timeline).toBeNull();
		// The control still names the reader's choice, so the click is not refused.
		expect(model.runs?.selectedId).toBe('ex_1');
	});

	it('falls back to the current run for a selection the payload does not list', () => {
		const model = toTaskPanelModel(
			task({ activeExecutionId: 'ex_3' }),
			[],
			runState('ex_3', { recentRuns: THREE_RUNS }),
			'ex_from_another_task'
		);

		// The control cannot honestly say which run is shown, so there is none — and
		// the Run act keeps naming the run the reader asked for, which is the one the
		// caller's own request pinned.
		expect(model.runs).toBeNull();
	});

	it('keeps task deliverables stable while the selected run changes its own outputs and artifacts', () => {
		const row = task({ activeExecutionId: 'ex_3', completionSummary: 'The quarter grew 4%' });
		const files = outputFilesFrom([{ relative_path: 'reports/summary.md', media_type: 'text/markdown' }]);
		const outputRef = (path: string, source: string) => ({
			output_id: `out-${path}`,
			scope: 'execution',
			audience: 'agent',
			role: 'primary_execution',
			relative_path: path,
			media_type: 'text/markdown',
			created_at: new Date(100_000).toISOString(),
			source_execution_id: source,
			source_plan_id: null,
			source_output_ids: []
		});
		const currentState = runState('ex_3', {
			recentRuns: THREE_RUNS,
			directOutputs: [outputRef('executions/ex_3/outputs/current.md', 'ex_3')],
			childOutputs: [],
			artifacts: []
		});
		const earlierState = runState('ex_1', {
			recentRuns: THREE_RUNS,
			directOutputs: [outputRef('executions/ex_1/outputs/attempt.md', 'ex_1')],
			childOutputs: [outputRef('executions/ex_child/outputs/evidence.md', 'ex_child')],
			artifacts: [
				{
					artifact_id: 'artifact-file',
					artifact_type: 'browser_capture',
					content_type: 'image/png',
					produced_at: new Date(110_000).toISOString(),
					source_execution_id: 'ex_1',
					display_name: 'evidence.png',
					relative_path: 'executions/ex_1/outputs/evidence.png',
					size_bytes: 42
				},
				{
					artifact_id: 'artifact-structured',
					artifact_type: 'browser_observation',
					content_type: 'application/json',
					produced_at: new Date(120_000).toISOString(),
					source_execution_id: 'ex_1'
				}
			]
		});
		const urlFor = (path: string) => `/task-output/${path}`;

		const current = toTaskPanelModel(row, files, currentState, null, urlFor);
		const earlier = toTaskPanelModel(row, files, earlierState, 'ex_1', urlFor);

		expect(current.output?.files.map((file) => [file.scope, file.name])).toEqual([
			['task', 'summary.md'],
			['execution', 'current.md']
		]);
		expect(earlier.output?.files.map((file) => [file.scope, file.name])).toEqual([
			['task', 'summary.md'],
			['execution', 'attempt.md'],
			['delegated', 'evidence.md'],
			['artifact', 'evidence.png']
		]);
		expect(earlier.output?.artifacts).toEqual([
			expect.objectContaining({ id: 'artifact-structured', sourceExecutionId: 'ex_1' })
		]);
		expect(earlier.output?.selectedExecutionId).toBe('ex_1');
		expect(earlier.output?.runArtifactsKnown).toBe(true);
		expect(earlier.output?.files[3].url).toBe(
			'/task-output/executions/ex_1/outputs/evidence.png'
		);
		// Task-level provenance still names the run that promoted the stable
		// deliverables, not the historical run selected for inspection.
		expect(earlier.output?.provenance[0]).toEqual({
			label: 'Source execution id',
			value: 'ex_3'
		});
	});
});
