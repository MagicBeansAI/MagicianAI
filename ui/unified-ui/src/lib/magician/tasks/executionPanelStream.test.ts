/**
 * The live subscription's one decision: is this pushed state about the run on
 * screen?
 *
 * Everything else in `executionPanelStream.ts` is the store's own plumbing — a
 * subscribe, a sequence gate, an unsubscribe — and a test that reached it
 * through a mocked socket would be testing the mock. What cannot be left to
 * review is the identity test, because getting it wrong renders **another run's
 * complete and plausible state** under this drawer, with every value valid and
 * nothing downstream able to notice.
 *
 * The fixtures are two executions of **one task**, which is the shape that
 * makes the wrong rule pass: a matcher keyed on the task id accepts both, and a
 * fixture with two different tasks would let it through.
 */
import { describe, expect, it } from 'vitest';

import type { V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import type { ExecutionPanelState } from '$lib/types/executionPanel';

import { panelDeltaState, type PanelScope } from './executionPanelStream';

const TASK_ID = 'task-corpus-reindex';
/** Two runs of one task. This is the whole point of the fixture. */
const WATCHED_EXECUTION = 'exec-9f3c';
const OTHER_EXECUTION = 'exec-4a71';

const SCOPE: PanelScope = { principal: 'anonymous', workspace: 'default' };
const OTHER_SCOPE: PanelScope = { principal: 'someone-else', workspace: 'default' };

const TARGET = { taskId: TASK_ID, executionId: WATCHED_EXECUTION };

/**
 * The `/crew/<id>` shape, and it is a different shape.
 *
 * A cycle's target carries the page's **synthetic** `agent-cycle:<agent>:<cycle>`
 * as its task id — a string the backend has never seen and cannot produce. What
 * the projector pushes is a state naming the *real* task it resolved the cycle's
 * execution to. So on this surface the target's task id and the payload's task id
 * never match, and the subscription works anyway because the identity test does
 * not consult either one.
 *
 * Two executions of one agent again, for the same reason the two above are two
 * executions of one task: the next cycle is what a drawer left open drifts onto.
 */
const CYCLE_HOST_TASK = 'task-presto-autonomous-118';
const CYCLE_EXECUTION = 'exec-c1d2';
const NEXT_CYCLE_EXECUTION = 'exec-e5f6';
const CYCLE_TARGET = { taskId: 'agent-cycle:presto:cycle-118', executionId: CYCLE_EXECUTION };

function state(
	executionId: string | null,
	status: 'running' | 'completed' = 'running',
	taskId: string = TASK_ID
): ExecutionPanelState {
	return {
		default_tab: 'run',
		overview: {
			task_id: taskId,
			execution_id: executionId,
			principal: SCOPE.principal,
			workspace: SCOPE.workspace,
			ui_thread_id: 'general',
			title: 'Reindex the corpus',
			description: '',
			status,
			assigned_agent_id: 'personal-assistant',
			has_plan: false,
			created_at: 1_770_000_000_000,
			updated_at: 1_770_000_060_000
		},
		run: { pending_questions: [], needs_attention: [], recent_activity: [] },
		output: { deliveries: [], recent_runs: [] },
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

function delta(overrides: {
	state: ExecutionPanelState;
	principal?: string;
	workspace?: string;
	taskId?: string | undefined;
	executionId?: string | undefined;
}): Extract<V2WebSocketEvent, { event_type: 'ExecutionPanelDelta' }> {
	return {
		event_type: 'ExecutionPanelDelta',
		data: {
			principal: overrides.principal ?? SCOPE.principal,
			workspace: overrides.workspace ?? SCOPE.workspace,
			task_id: 'taskId' in overrides ? overrides.taskId : TASK_ID,
			execution_id: 'executionId' in overrides ? overrides.executionId : WATCHED_EXECUTION,
			state: overrides.state
		}
	} as Extract<V2WebSocketEvent, { event_type: 'ExecutionPanelDelta' }>;
}

describe('panelDeltaState', () => {
	it('accepts a push about the run on screen', () => {
		const pushed = state(WATCHED_EXECUTION, 'completed');
		// Identity, not just truthiness: the *same object* has to come back, or a
		// matcher returning some other state would pass this.
		expect(panelDeltaState(delta({ state: pushed }), TARGET, SCOPE)).toBe(pushed);
	});

	/**
	 * The defect this function exists to prevent. Both runs belong to the task
	 * the reader opened, so a matcher with a task-id arm — which is what the
	 * retired data layer had, correctly, for a panel pinned to a *task* — drops
	 * the newer run's state into a drawer showing the older one.
	 */
	it('refuses another execution of the same task', () => {
		expect(
			panelDeltaState(
				delta({ state: state(OTHER_EXECUTION), executionId: OTHER_EXECUTION }),
				TARGET,
				SCOPE
			)
		).toBeNull();
	});

	/**
	 * The same refusal reached from the other side. Here the *envelope* names the
	 * watched run and the *state* describes another — which is the shape a
	 * matcher reading `event.data.execution_id` would accept, and it is the state
	 * that gets rendered.
	 */
	it('refuses a state describing another run, whatever the envelope claims', () => {
		expect(
			panelDeltaState(
				delta({ state: state(OTHER_EXECUTION), executionId: WATCHED_EXECUTION }),
				TARGET,
				SCOPE
			)
		).toBeNull();
	});

	it('refuses a state that names no run at all', () => {
		// Nothing shows this is about the watched execution, so nothing may claim
		// it is. The drawer keeps the state it fetched.
		expect(panelDeltaState(delta({ state: state(null) }), TARGET, SCOPE)).toBeNull();
	});

	it('refuses another principal\'s run, even with the same execution id', () => {
		// A delta is a broadcast, and an id from another scope can collide.
		expect(
			panelDeltaState(delta({ state: state(WATCHED_EXECUTION) }), TARGET, OTHER_SCOPE)
		).toBeNull();
		expect(
			panelDeltaState(
				delta({ state: state(WATCHED_EXECUTION), workspace: 'other-workspace' }),
				TARGET,
				SCOPE
			)
		).toBeNull();
	});

	it('refuses a payload that is not a panel state', () => {
		// The same guard `fetchExecutionPanelState` applies to a response body: a
		// well-formed envelope carrying nothing readable teaches us nothing about
		// the run, and rendering it would blank the drawer mid-watch.
		const malformed = { overview: {} } as unknown as ExecutionPanelState;
		expect(panelDeltaState(delta({ state: malformed }), TARGET, SCOPE)).toBeNull();
	});

	/**
	 * **The `/crew/<id>` case: the stream does reach an agent cycle.**
	 *
	 * Nothing in this payload matches the target's task id and nothing is meant
	 * to — `agent-cycle:presto:cycle-118` is a string this client made up to pick
	 * an endpoint, and the projector answers with the real task it resolved the
	 * execution to. The one thing that does match is the execution, which is the
	 * agent's real `current_execution_id`, and that is the whole of the test.
	 *
	 * A matcher that *required* a task-id match would reject every delta a cycle
	 * ever produces, and the drawer would sit on its opening snapshot looking
	 * exactly as live as one that was working.
	 */
	it('accepts a cycle push whose task id could not possibly match the target', () => {
		const pushed = state(CYCLE_EXECUTION, 'running', CYCLE_HOST_TASK);
		expect(
			panelDeltaState(
				delta({ state: pushed, taskId: CYCLE_HOST_TASK, executionId: CYCLE_EXECUTION }),
				CYCLE_TARGET,
				SCOPE
			)
		).toBe(pushed);
	});

	/**
	 * The failure the subscription introduces to `/crew/<id>`, which a drawer that
	 * only ever fetched once could not have. An agent runs cycle after cycle under
	 * the same host task; the moment the next one starts, its state is pushed to
	 * every open socket. The drawer was opened on cycle 118 and must keep showing
	 * it — a silent switch to 119 would be a complete, plausible, wrong run.
	 */
	it('refuses the agent\'s next cycle under the drawer showing this one', () => {
		expect(
			panelDeltaState(
				delta({
					state: state(NEXT_CYCLE_EXECUTION, 'running', CYCLE_HOST_TASK),
					taskId: CYCLE_HOST_TASK,
					executionId: NEXT_CYCLE_EXECUTION
				}),
				CYCLE_TARGET,
				SCOPE
			)
		).toBeNull();
	});

	it('reads the run off the selected execution when the overview omits it', () => {
		// The same two-field fallback `toExecutionPanelModel` uses for the model's
		// id — through the adapter's own function, so the two cannot disagree about
		// which run a payload describes.
		const fromDebug = state(null);
		fromDebug.debug.selected_execution = {
			execution_id: WATCHED_EXECUTION,
			status: 'running',
			artifact_names: [],
			linked_inputs: [],
			step_statuses: []
		};
		expect(panelDeltaState(delta({ state: fromDebug }), TARGET, SCOPE)).toBe(fromDebug);
	});
});
