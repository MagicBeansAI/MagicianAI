/**
 * The second seam: an **execution** — a run, not a task — mapped onto the
 * panel's `TaskPanelModel`.
 *
 * `toTaskPanelModel` translates a store `Task`. Two surfaces open a panel on
 * something that is not one: a chat activity card inspecting a delegated run,
 * and `/crew/<id>` inspecting an agent cycle. Both used to synthesize a `Task`
 * to get past a panel that only accepted one — a record wearing another
 * record's shape, with `source: 'execution'` or an `agent-cycle:` id as the
 * tell. Neither is a task, and the fabrication was load-bearing: the chat one
 * stamped `Delegated run · <id>` as a title and `new Date()` as both timestamps,
 * so a two-hour-old run reported itself as created a moment ago.
 *
 * **This panel does not need the fabrication.** Which acts render is decided by
 * which slices are present, so a run with no plan simply has `plan: null` and
 * the Plan act is absent — not greyed, not empty. That is the capability model
 * doing the job it was designed for, and it is why an execution needs an
 * adapter rather than a panel.
 *
 * **One adapter, not two.** The chat gesture and the crew gesture read the same
 * `ExecutionPanelState` off the same endpoint family, and the only thing that
 * differs between them is whether a file list can be resolved — which is
 * already a parameter, exactly as it is on `toTaskPanelModel`. Two adapters
 * over one payload would be two answers to what a step status means, and they
 * would disagree the first time either was edited.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`, *The execution
 * adapter*, and `docs/archive/plans/2026-07-29-unified-task-panel-parity-and-migration.md`
 * Workstream C.
 */

import { timedFetch } from '$lib/shared/fetch';
import type {
	ExecutionPanelClarificationQuestion,
	ExecutionPanelState,
	ExecutionPanelStepStatus
} from '$lib/types/executionPanel';

import { askTargetFrom, runAttentionFrom, type TaskAsk } from './taskAttention';
import {
	delegatedStepsFrom,
	provenanceRows,
	queuedReasonOf,
	responsibilityFrom,
	verdictStatusOf,
	type PanelOutputFile
} from './taskPanelModel';
import { deriveTimeline, executionIdOf } from './taskTimeline';
import type {
	RunStepStatus,
	TaskPanelModel,
	TaskPanelOutput,
	TaskPanelRun,
	TaskPanelRunStep
} from './UnifiedTaskPanel.svelte';

const API_BASE = '/api/magician/v3';

/** A non-empty trimmed string, or `null`. Absence and blank are one answer. */
function text(raw: string | null | undefined): string | null {
	if (typeof raw !== 'string') return null;
	const trimmed = raw.trim();
	return trimmed ? trimmed : null;
}

/** A finite epoch-millis instant, or `null`. Zero is a sentinel, not a time. */
function instant(raw: number | null | undefined): number | null {
	return typeof raw === 'number' && Number.isFinite(raw) && raw > 0 ? raw : null;
}

/**
 * The step status this client recognises for one wire word, or `null`.
 *
 * The wire's `status` is a bare `string` — `ExecutionPanelStepStatus` declares
 * it so, and the backend has shipped at least three spellings of "done" — so
 * there is no union to write a `Record` over and no compile error to be had.
 * The guard that is available instead is the fallback: an unmapped word yields
 * `null`, and the panel renders **no marker** for it rather than a plausible
 * wrong one. `runStepsOf` in `taskPanelModel.ts` makes the same choice for the
 * same reason, from the other side of a typed union.
 *
 * **`waiting`, `blocked` and `paused` used to be deliberately absent**, because
 * none of the six step statuses meant "held up" and `pending` would have claimed
 * the step never started — false for one suspended mid-flight, with nothing on
 * screen to contradict it. That gap is closed rather than worked around:
 * `RunStepStatus` gained a `waiting` member, which a delegated child run needs
 * for exactly the same reason, and these three map onto it.
 */
const STEP_STATUS: Readonly<Record<string, RunStepStatus>> = {
	pending: 'pending',
	queued: 'pending',
	in_progress: 'in_progress',
	running: 'in_progress',
	ongoing: 'in_progress',
	completed: 'completed',
	complete: 'completed',
	done: 'completed',
	failed: 'failed',
	error: 'failed',
	skipped: 'skipped',
	cancelled: 'cancelled',
	canceled: 'cancelled',
	waiting: 'waiting',
	blocked: 'waiting',
	paused: 'waiting'
};

function stepStatusOf(raw: string | null | undefined): RunStepStatus | null {
	const word = text(raw)?.toLowerCase();
	return word ? (STEP_STATUS[word] ?? null) : null;
}

function runStepsOf(steps: readonly ExecutionPanelStepStatus[]): TaskPanelRunStep[] {
	return steps.map((step) => ({
		// The step's own name, falling back to its position. A blank label would
		// render an empty row that still counts toward `step N of M`, which is
		// worse than a row that says only where it sits in the run.
		label: text(step.name) ?? `Step ${step.number}`,
		// Nothing on the wire times an individual step. `null` rather than a
		// difference between two instants that describe the run, not the step.
		durationMs: null,
		// No retry count reaches this payload either, so `0` — which `runSummary`
		// drops entirely rather than rendering `0 retries`.
		retries: 0,
		status: stepStatusOf(step.status),
		origin: 'step',
		// Declared on `ExecutionPanelStepStatus` and read here for the first time.
		// **These fill only as far as `step_statuses` does**, which for the V3
		// execution-panel adapter is not at all — it sends the collection empty, so
		// a payload with a capability to report has no rows to report it on and one
		// with rows has neither field blank. Either way there is no column here
		// that is always empty; the act is simply absent of rows.
		capability: text(step.capability),
		delegate: text(step.delegate_agent_id),
		// A step this run took itself is not waiting on another run. `false` for
		// the reason `retries` is `0` here: the row has an answer, and it is "no".
		blocking: false
	}));
}

/**
 * The step the run is on, 1-based, or `null`.
 *
 * Read off the same list the Run act body renders — never off
 * `overview.current_step`, which is a second answer to one question and the one
 * the reader cannot check against what is on screen. Design §4's rule for the
 * Run act's counts, applied to the other adapter's input.
 */
function liveStepOf(steps: readonly TaskPanelRunStep[]): number | null {
	const index = steps.findIndex((step) => step.status === 'in_progress');
	return index === -1 ? null : index + 1;
}

/**
 * The first ask this run can still be answered on, and the target that answers it.
 *
 * **Two sources, ranked the opposite way round from `toTaskPanelModel`**, and
 * for a reason that is about which one is richer rather than which one is
 * older. On a task-list row the only ask available is a plan-time clarification,
 * so it leads; here `needs_attention` is the canonical list the backend built
 * for this run, each row carrying a real `HitlSource` and the instant it was
 * raised, so it leads instead and `pending_questions` is the fallback for a
 * payload that did not project one.
 *
 * The fallback's `source` is `clarification` because that is what a pending
 * question is, and its `raisedAt` is `null` because the question record carries
 * no instant — design §5 omits a duration rather than approximating one. It
 * routes to the Plan act, which an execution does not have; `defaultOpenAct`
 * then falls back to the nearest act that exists, which is the whole reason
 * that fallback is directional.
 */
function attentionOf(state: ExecutionPanelState): TaskAsk | null {
	const raised = runAttentionFrom(state.run?.needs_attention ?? []);
	if (raised) return raised;

	const questions: readonly ExecutionPanelClarificationQuestion[] =
		state.run?.pending_questions ?? [];
	for (const question of questions) {
		const asked = text(question.question_text);
		// The row's own target, kept rather than discarded — the same fix
		// `runAttentionFrom` got one list over. A pending question publishes the
		// `hitl_request` that answers it, and reading the text while dropping the
		// target is what left this panel able to describe an ask and unable to
		// answer it.
		if (asked) {
			return {
				attention: { source: 'clarification', summary: asked, raisedAt: null },
				ask: askTargetFrom(question.hitl_request)
			};
		}
	}
	return null;
}

/**
 * When this run last did something, or `null`.
 *
 * `VerdictInput.lastProgressAt` must be a **real** progress instant, never an
 * `updated_at` — a record rewritten by a status poll would make a wedged run
 * un-stallable, which is the failure design §5 names. So this is the latest
 * instant across the two event logs the payload carries and nothing else:
 * `overview.updated_at` is exactly the value the rule forbids, and it is right
 * beside these in the same object.
 *
 * `activity_log` is the full humanized log and `recent_activity` is a capped
 * view of it; the maximum over both is taken rather than choosing between them,
 * because whichever is populated the answer is the same instant.
 *
 * This is a **gain** over the task adapter, which has no run-start or
 * run-progress instant at all and reports `lastProgressAt` only because the
 * task row happens to carry one.
 */
function lastProgressAtOf(state: ExecutionPanelState): number | null {
	const instants: number[] = [];
	const consider = (candidate: number | null): void => {
		if (candidate !== null) instants.push(candidate);
	};

	for (const item of state.run?.activity_log ?? []) consider(instant(item?.created_at));
	for (const item of state.run?.recent_activity ?? []) consider(instant(item?.created_at));
	for (const entry of state.debug?.timeline ?? []) consider(instant(entry?.timestamp));
	return instants.length > 0 ? Math.max(...instants) : null;
}

/**
 * How long the run took, or `null` while it is still going.
 *
 * Both instants describe the same execution, so their difference is measured
 * rather than approximated — the one duration on this panel that is. A live run
 * has no `ended_at`, and `now - started_at` would be computed once and then sit
 * frozen on screen until the next fetch, which is worse than the omission
 * design §5 asks for.
 */
function elapsedMsOf(state: ExecutionPanelState): number | null {
	const selected = state.debug?.selected_execution ?? null;
	const started = instant(selected?.started_at);
	const ended = instant(selected?.ended_at);
	if (started === null || ended === null || ended <= started) return null;
	return ended - started;
}

/**
 * The Run act. Present when this payload describes an execution at all, which
 * for these two surfaces it always does — the gesture that opened the panel was
 * "inspect this run". Absent rather than empty if it somehow does not, on the
 * same rule `runOf` follows: an empty Run act asserts the run started.
 */
function runOf(state: ExecutionPanelState, executionId: string | null): TaskPanelRun | null {
	if (executionId === null) return null;
	const steps = runStepsOf(state.debug?.selected_execution?.step_statuses ?? []);
	return {
		// The same projection the task adapter reads, over the same payload and by
		// the same function — a run that delegated work reads the same whether the
		// reader arrived from a task row or from a chat activity card. Appended
		// after the plan steps for the reason `runOf` in `taskPanelModel.ts` gives:
		// the live-step position is an index into this list.
		steps: [...steps, ...delegatedStepsFrom(state, executionId)],
		// The same projection over the same payload and the same execution-id guard
		// as the delegated rows above — see `responsibilityFrom`.
		responsibility: responsibilityFrom(state, executionId),
		// The same projection the task adapter reads, over the same payload — this
		// module has no timeline of its own and must not grow one. What differs is
		// only where the run's id came from: here it is read off the payload, so
		// `deriveTimeline`'s agreement check is tautological and the answer is
		// always an array. An execution that recorded no events is `[]`, which the
		// act says out loud rather than staying silent about.
		//
		// The id is passed rather than re-derived so this act's rows and this act's
		// provenance can never name different runs.
		timeline: deriveTimeline(state, executionId),
		// The grouping metadata for the delegated rows `deriveTimeline` now
		// includes. Absent for a run that delegated nothing, and for payloads
		// predating the field — both mean "no groups", which renders flat.
		delegations: state?.run?.delegations ?? [],
		provenance: provenanceRows([
			['Execution id', executionId],
			['Task id', text(state.overview?.task_id)],
			['Agent', text(state.overview?.active_agent_id) ?? text(state.overview?.assigned_agent_id)]
		])
	};
}

/**
 * The Output act, or `null` when no file list could be read.
 *
 * The same partition `outputOf` draws, and it has to be: `[]` is an act that
 * loaded and found nothing, `null` is one that could not load and must be
 * absent, because an empty act answers `no output` — a claim about the run made
 * out of a fact about the network.
 *
 * **A caller with no outputs endpoint to ask passes `null`**, and the act is
 * absent. That is the `/crew` case: an `agent-cycle:` id is not a task id and
 * `/v3/tasks/{id}/outputs` cannot answer for one. The cost is stated rather than
 * hidden — `output.result.summary` is dropped for that surface along with the
 * act that would have carried it. See the component doc.
 *
 * `summary` is the run's own written report and **only** that. The payload also
 * carries `outcome`, a one-line disposition; folding it in as a fallback would
 * put two kinds of text under one name and open a markdown block for a sentence
 * that is not a report. `toTaskPanelModel` reads `completionSummary` alone for
 * the same reason.
 */
function outputOf(
	state: ExecutionPanelState,
	files: readonly PanelOutputFile[] | null,
	executionId: string | null
): TaskPanelOutput | null {
	if (files === null) return null;
	return {
		files,
		summary: text(state.output?.result?.summary),
		provenance: provenanceRows([
			['Source execution id', executionId],
			...files.map((file): [string, string] => [file.name, file.path])
		])
	};
}

/**
 * The panel's whole model for one execution.
 *
 * `files` is the loaded output list, `[]` for a run that produced none, and
 * `null` when the caller could not read one **or has no endpoint to read from**.
 * The two absences are one value here because they render identically and both
 * are honest: neither claims the run produced nothing.
 */
export function toExecutionPanelModel(
	state: ExecutionPanelState,
	files: readonly PanelOutputFile[] | null
): TaskPanelModel {
	const executionId = executionIdOf(state);
	const steps = runStepsOf(state.debug?.selected_execution?.step_statuses ?? []);
	const liveStep = liveStepOf(steps);
	const asked = attentionOf(state);

	return {
		// **Which run this is**, and it must be the run rather than the task: two
		// executions of one task are two different things to inspect, and a model
		// id that named the task would carry the reader's open act from the first
		// onto the second while the panel had no way to tell they had changed.
		// Prefixed so a task id and an execution id can never read as the same
		// identity even if the two id spaces ever overlap.
		id: executionId ? `execution:${executionId}` : `task:${state.overview?.task_id ?? ''}`,
		status: verdictStatusOf(state.overview.status),
		queuedFor: queuedReasonOf(state.overview.status),
		attention: asked?.attention ?? null,
		// The control that answers it, off the same row as the verdict that
		// describes it — see `TaskAsk`.
		ask: asked?.ask ?? null,
		// The run's own error, from the two fields that carry one. `run.summary` is
		// **not** consulted: on a real payload it is a summary of the run, and only
		// the client-side fallback state ever put an error message there.
		error:
			text(state.debug?.latest_error_message)
			?? text(state.debug?.selected_execution?.error_message),
		currentStep: liveStep,
		// `0 of 0` is not a denominator: an empty step list means the run was never
		// planned into steps, and `stepPhrase` drops the `of N` rather than
		// inventing one.
		totalSteps: steps.length > 0 ? steps.length : null,
		currentStepLabel: liveStep === null ? null : (steps[liveStep - 1]?.label ?? null),
		elapsedMs: elapsedMsOf(state),
		lastProgressAt: lastProgressAtOf(state),
		// **An execution has no plan act.** The plan belongs to the task and is
		// served by a different endpoint; this payload carries step *statuses*,
		// which are the run's account of executing a plan rather than the plan
		// itself. The capability model states that absence as `null`, and the act
		// simply does not render — which is the whole reason these two surfaces do
		// not need a panel of their own.
		plan: null,
		run: runOf(state, executionId),
		output: outputOf(state, files, executionId),
		/**
		 * **`null`, and it is a fact about this surface rather than a gap in it.**
		 *
		 * The two callers here opened the panel *on a run* — a chat activity card
		 * inspecting one delegated execution, and a `/crew/<id>` cycle. There is one
		 * run by construction, so there is nothing to choose between, and a control
		 * offering to switch would be offering to switch to the same thing.
		 *
		 * The reader's route to the task's *other* runs from here is the task panel,
		 * which is where the picker lives. Reproducing it on a surface whose whole
		 * identity is one execution would make `Run details for #2 of 3` appear over
		 * a panel that cannot show `#1` — the payload's `recent_runs` lists the
		 * task's runs, but every other slice on screen belongs to this one.
		 */
		runs: null
	};
}

/** Which run to inspect: the task that owns it, and the execution itself. */
export interface ExecutionPanelTarget {
	/** A real task id, or the synthetic `agent-cycle:<agent>:<cycle>` a crew row carries. */
	taskId: string;
	executionId: string;
}

/**
 * Where this run's panel state is served.
 *
 * **Two routes, and the id decides.** A synthetic `agent-cycle:` id is not a
 * task id, so `/v3/tasks/{id}/execution-panel` cannot answer for it and the
 * execution-scoped route is the only one that can. A real task id takes the
 * task-scoped route with `execution_id` pinned, because that one is populated
 * for task-backed delegate runs and the execution-scoped one is not — the
 * failure reads `execution panel state not found`, which is why the chat card
 * requires a task id before it offers *Inspect run* at all.
 *
 * Exported for its own test: the branch is the whole content of this function
 * and a test that could only reach it through `fetch` would be testing the mock.
 */
export function executionPanelUrl(
	target: ExecutionPanelTarget,
	principal: string,
	workspace: string
): string {
	void principal;
	void workspace;
	const params = new URLSearchParams();
	if (target.taskId.startsWith('agent-cycle:')) {
		const query = params.toString();
		return `${API_BASE}/executions/${encodeURIComponent(target.executionId)}/execution-panel${query ? `?${query}` : ''}`;
	}
	params.set('execution_id', target.executionId);
	return `${API_BASE}/tasks/${encodeURIComponent(target.taskId)}/execution-panel?${params.toString()}`;
}

/**
 * This run's panel state, or `null` when it could not be read.
 *
 * `null` is the panel's `loadError` case at the call site, not an empty model:
 * a run whose state did not load has no verdict to report, and the drawer's
 * skeleton or its `Can't load this run` line is the honest answer. A malformed
 * body is a failure for the same reason — we did not learn anything about the
 * run.
 */
export async function fetchExecutionPanelState(
	target: ExecutionPanelTarget,
	principal: string,
	workspace: string
): Promise<ExecutionPanelState | null> {
	try {
		const response = await timedFetch(executionPanelUrl(target, principal, workspace), {
			headers: { Accept: 'application/json' }
		});
		if (!response.ok) return null;
		const payload = await response.json();
		// The one field every branch above reads. A body without it is not a panel
		// state, however well-formed the JSON was.
		if (!payload?.overview?.status) return null;
		return payload as ExecutionPanelState;
	} catch {
		return null;
	}
}
