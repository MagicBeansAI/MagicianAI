/**
 * The seam for internal tasks: a `/v3/tasks/internal` row plus its `/details`
 * payload, mapped onto the panel's `TaskPanelModel`.
 *
 * Pure, and the sibling of `taskPanelModel.ts` — same job, different payload.
 * **Everything the two adapters agree about is imported rather than restated**:
 * the status vocabulary, the provenance-row rule, the output-file shape and the
 * Run act's cost totals all come from there, so `paused` cannot mean one thing on
 * `/tasks` and another on `/tasks?type=internal`. What is written here is only
 * what the internal payload spells differently.
 *
 * This module is where the design's acceptance test is paid for. Internal tasks
 * have no plan, so `plan` is **always `null`** and the panel renders no Plan act
 * — absent, never greyed — without the panel ever being told what kind of task
 * it is holding. The difference between the two surfaces lives here, in a
 * capability, and nowhere else.
 *
 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §2 and
 * `docs/components/unified-ui/unified-task-panel.md`.
 */

import {
	outputFilesFrom,
	outputKindOf,
	provenanceRows,
	queuedReasonOf,
	verdictStatusOf,
	type OutputUrlFor,
	type PanelOutputFile,
	type TaskOutputRef
} from '$lib/magician/tasks/taskPanelModel';
import {
	runsSliceOf,
	type RunRecord,
	type TaskPanelRuns
} from '$lib/magician/tasks/taskRuns';
import {
	deriveTimeline,
	runCostRows,
	type TimelineEntry
} from '$lib/magician/tasks/taskTimeline';
import type { ExecutionPanelState } from '$lib/types/executionPanel';
import type {
	TaskPanelArtifact,
	TaskPanelModel,
	TaskPanelOutput,
	TaskPanelRun
} from '$lib/magician/tasks/UnifiedTaskPanel.svelte';
import type { VerdictAttention } from '$lib/magician/tasks/taskVerdict';
import { normalizeV3TaskStatus, parseLastProgressAt } from '$lib/stores/taskStore';

import type {
	InternalExecutionDetails,
	InternalTaskDetails,
	InternalTaskListItem
} from './api';

/**
 * What the panel says when an internal task's output synthesis has given up.
 *
 * A sentence rather than the backend's `last_error`, because the reader's next
 * move is a retry and not a stack trace — and because the retry control is task
 * chrome the workspace owns, this line is the only thing that explains why it is
 * there. The raw error stays on the execution record, reachable from the
 * expanded row.
 */
export const SYNTHESIS_FAILED_SUMMARY =
	'Output synthesis failed — retry it to produce this task’s output';

/**
 * Epoch millis for an RFC3339 instant on the wire, or `null`.
 *
 * Delegates to `parseLastProgressAt` — named for the field it was written for,
 * but the rule is general and is the one every timestamp here needs: **only a
 * parseable RFC3339 string is accepted.** `parseTimestampToIso` is deliberately
 * never used, because it answers *now* for anything it cannot read, which would
 * turn "no progress recorded" into "just advanced" and make a wedged run
 * un-stallable. The `?? null` is this module's half of the store-says-undefined
 * / modules-say-null coercion, done once here rather than at four call sites.
 */
function instantOf(raw: unknown): number | null {
	return parseLastProgressAt(raw) ?? null;
}

/** A non-empty trimmed string, or `null`. Absence and blank are one answer. */
function text(raw: unknown): string | null {
	if (typeof raw !== 'string') return null;
	const trimmed = raw.trim();
	return trimmed ? trimmed : null;
}

/**
 * The task's own output refs, or `null` when the payload does not carry a list.
 *
 * `null` is "we did not learn what this task produced", which is a different
 * claim from `[]` and renders differently (design §6): the Output act is absent
 * rather than empty, because an empty act asserts `no output`. A malformed or
 * missing `refs.outputs` is therefore a failure to read, not a task that wrote
 * nothing — the same rule `fetchTaskOutputFiles` follows for a malformed body.
 */
function taskOutputRefs(details: InternalTaskDetails): TaskOutputRef[] | null {
	const refs = (details.task as { refs?: unknown } | undefined)?.refs;
	const outputs = (refs as { outputs?: unknown } | undefined)?.outputs;
	return Array.isArray(outputs) ? (outputs as TaskOutputRef[]) : null;
}

/**
 * This task's output files, in the order the record lists them, or `null` when
 * the details have not been read.
 *
 * Exported for callers that need the task-level output list itself. Panel
 * actions use the complete `TaskPanelFile` dispatched by the shared component;
 * that rendered list also contains selected-run and artifact files and must not
 * be indexed back into this narrower task-only list.
 */
export function internalTaskOutputFiles(
	details: InternalTaskDetails | null,
	urlFor: OutputUrlFor = () => null
): PanelOutputFile[] | null {
	if (details === null) return null;
	const refs = taskOutputRefs(details);
	return refs === null ? null : outputFilesFrom(refs, urlFor, 'task');
}

/**
 * The execution the Run act is about — the task's **root** run, never one of the
 * executions listed beneath it.
 *
 * Identified by the id the task record itself names. When it names none, a
 * single listed execution is unambiguously the run and is used; two or more with
 * no root named is a run this module cannot identify, and it answers `null`
 * rather than describing whichever one happened to be last. A Run act built on a
 * guess would carry the wrong execution id in its provenance, which is the one
 * value in that act a reader would take to another surface.
 */
function currentExecution(
	task: InternalTaskListItem,
	details: InternalTaskDetails | null
): InternalExecutionDetails | null {
	if (details === null) return null;
	const executions = Array.isArray(details.executions) ? details.executions : [];
	if (executions.length === 0) return null;

	const wanted = text(task.active_root_execution_id) ?? text(task.latest_root_execution_id);
	if (wanted === null) return executions.length === 1 ? executions[0] : null;
	return executions.find((entry) => text(entry.state?.execution_id) === wanted) ?? null;
}

/**
 * The execution the Run act describes: the one the reader picked, or the task's
 * current one.
 *
 * **Separate from `currentExecution` on purpose, and the separation is the whole
 * of design §1's verdict rule on this surface.** `elapsedMs` feeds the *verdict
 * line*, which has to keep describing the task — a task that finished in three
 * minutes must not read `Finished · 40s` because the reader is looking at an
 * attempt that died early. So the verdict keeps reading `currentExecution` and
 * only the Run act follows the picker.
 *
 * A picked id the payload does not list falls back to the current run rather than
 * to nothing: the reader asked for a run this `/details` response cannot describe,
 * and showing them the run the rest of the panel is about beats an empty act.
 * `runsSliceOf` reaches the same conclusion from the other side and renders no
 * control, so the two cannot disagree about which run is on screen.
 */
function selectedExecution(
	task: InternalTaskListItem,
	details: InternalTaskDetails | null,
	selectedRunId: string | null
): InternalExecutionDetails | null {
	const wanted = text(selectedRunId);
	if (wanted === null) return currentExecution(task, details);
	const executions = Array.isArray(details?.executions) ? (details?.executions ?? []) : [];
	return (
		executions.find((entry) => text(entry.state?.execution_id) === wanted)
		?? currentExecution(task, details)
	);
}

/**
 * The task's runs, as the picker lists them — or `null` when there is no choice.
 *
 * **The same three facts, off a different payload, through the same function.**
 * `/tasks` reads them out of `/execution-panel`'s `output.recent_runs` and this
 * reads them out of `/details`'s `executions`, and neither adapter contains a
 * branch about which surface it is: that is design §2's acceptance test for this
 * feature, and this function is where it is paid for. The status goes through the
 * store's normaliser and then the shared status map, so an option reading
 * `finished` means the same thing on both routes and the same thing as the
 * verdict above it.
 *
 * **Every execution in the payload is listed.** Unlike the panel payload, whose
 * `recent_runs` is pre-filtered to root nodes server-side, `/details` carries no
 * relationship field to filter on — so there is nothing here to filter *by*, and
 * inventing a rule ("the ones whose id the row mentions") would drop real runs
 * from a list whose whole job is to show they exist. Internal tasks are
 * single-agent and delegate nothing, so the two lists are the same list; if that
 * changes, the payload has to say so before this can.
 */
function runsOf(
	details: InternalTaskDetails | null,
	selectedRunId: string | null
): TaskPanelRuns | null {
	const executions = Array.isArray(details?.executions) ? (details?.executions ?? []) : [];
	const rows: RunRecord[] = executions.map((entry) => ({
		executionId: text(entry.state?.execution_id),
		startedAt: instantOf(entry.state?.started_at),
		status: verdictStatusOf(normalizeV3TaskStatus(text(entry.state?.status) ?? ''))
	}));
	return runsSliceOf(rows, selectedRunId);
}

/**
 * How long the run has been going, or took.
 *
 * Real data here, unlike the normal-task adapter, which omits this field
 * entirely because no run-start instant reaches its store: an execution record
 * carries `started_at`, so a finished internal run reads `Finished · 3m 12s`.
 * A live one is measured against `now`, which is why this module takes a clock
 * at all — without it the Run act's only segment would be missing and
 * `runSummary` would read `not started` about a run that plainly has.
 *
 * `completed_at` is the field the execution record actually writes;
 * `ended_at` is read as a fallback because the workspace's own types have long
 * declared it, and reading both costs nothing while assuming one is a guess.
 */
function runElapsed(execution: InternalExecutionDetails | null, now: number): number | null {
	const started = instantOf(execution?.state?.started_at);
	if (started === null) return null;
	const ended = instantOf(execution?.state?.completed_at) ?? instantOf(execution?.state?.ended_at);
	return (ended ?? now) - started;
}

/**
 * The Run act, which exists when the task has an execution to describe.
 *
 * **Its step list is deliberately empty.** An execution record carries
 * `completed_step_ids` — identifiers, not descriptions — and rendering an id as
 * a step label would put L3 content at L2, which is the exact defect design §1
 * blames for the panel this replaces. Design §4 also requires the header's
 * counts to come from the list the body renders, so with no rows there is no
 * `14 steps` either: the summary carries the duration alone rather than a count
 * the body could contradict.
 */
function runOf(
	task: InternalTaskListItem,
	execution: InternalExecutionDetails | null,
	panel: ExecutionPanelState | null
): TaskPanelRun | null {
	if (execution === null) return null;
	// **`null` still means "nothing looked", and it is still not `[]`.** The
	// steps below are empty because the record carries identifiers this act
	// refuses to render as labels — an observation about what is there. The
	// timeline is `null` only when no panel state was read, or when the state
	// read describes a different run than the one on screen; `deriveTimeline`
	// makes that second call itself and returns `null` rather than another
	// run's rows.
	//
	// It is no longer hardcoded. `/tasks/{id}/details` still projects no event
	// log, but `/tasks/{id}/execution-panel` does, and it resolves internal task
	// ids — so this surface reads the same log every other run does, through the
	// same derivation, with no `if (internal)` anywhere.
	const timeline: TimelineEntry[] | null = deriveTimeline(
		panel,
		text(execution.state?.execution_id)
	);
	return {
		steps: [],
		timeline,
		// **`null` because this surface cannot delegate, not because nothing was
		// read.** An internal task is one execution of one agent; `/details`
		// projects no responsibility snapshot and there is no execution tree behind
		// it to project one from. A block absent for that reason is the same answer
		// a run that delegated nothing gets, and it is the right one.
		responsibility: null,
		provenance: provenanceRows([
			['Execution id', text(execution.state?.execution_id)],
			// **`Task id` is gone from here too**, and for the same reason it left the
			// other adapter: the drawer's control row carries it now, so the row was
			// one identifier stated twice in one panel.
			['Agent', text(task.agent_id)],
			// **The acceptance test, as a call site — and it has now been paid.** The
			// same function the task adapter uses, over this surface's own timeline,
			// with no `if (internal)` anywhere. This surface now reads an event log
			// (`/tasks/{id}/execution-panel`, which resolves internal task ids), and
			// its Run act reports what the run cost with no further work here: the
			// derivation was already shared, so wiring the log was the whole change.
			// The sum is still honestly absent when no panel state was read.
			...runCostRows(timeline)
		])
	};
}

/**
 * The Output act, absent in two different situations that mean the same thing:
 * **we have not observed what this task produced.**
 *
 * The first is the ordinary one — the details have not been read, so `files` is
 * `null` (design §6).
 *
 * The second is this surface's own: output synthesis is still running and has
 * written nothing yet. An empty act would summarise `no output`, and a finished
 * task's verdict would read `Produced no output` — a finality claim about output
 * that is still being written. Files that already exist are shown, because those
 * are observations rather than claims; it is only the empty case that has to
 * stay silent.
 */
function recordOf(raw: unknown): Record<string, unknown> | null {
	return raw !== null && typeof raw === 'object' && !Array.isArray(raw)
		? (raw as Record<string, unknown>)
		: null;
}

function numberOf(raw: unknown): number | null {
	return typeof raw === 'number' && Number.isFinite(raw) ? raw : null;
}

function safeTaskRelativePath(raw: unknown): string | null {
	const path = text(raw)?.replace(/^\.\//, '') ?? '';
	if (!path || path.startsWith('/') || path.includes('\\')) return null;
	if (path.split('/').some((component) => component === '..')) return null;
	return path;
}

function executionArtifactPath(
	payload: Record<string, unknown>,
	executionId: string
): string | null {
	const taskPath = safeTaskRelativePath(payload.task_relative_path);
	if (taskPath) return taskPath;
	const legacyPath = safeTaskRelativePath(payload.relative_path);
	if (legacyPath?.startsWith('outputs/') || legacyPath?.startsWith('executions/')) {
		return legacyPath;
	}
	const executionPath = safeTaskRelativePath(payload.execution_relative_path);
	return executionPath ? `executions/${executionId}/${executionPath}` : null;
}

function selectedExecutionOutput(
	execution: InternalExecutionDetails | null,
	urlFor: OutputUrlFor
): {
	files: PanelOutputFile[];
	artifacts: TaskPanelArtifact[];
	artifactsKnown: boolean;
} | null {
	if (execution === null) return null;
	const executionId = text(execution.state?.execution_id);
	if (!executionId) return null;
	const directRefs = Array.isArray(execution.refs?.output_refs)
		? execution.refs.output_refs
		: Array.isArray(execution.refs?.outputs)
			? execution.refs.outputs
			: [];
	const delegatedRefs = Array.isArray(execution.refs?.child_output_refs)
		? execution.refs.child_output_refs
		: [];
	const files: PanelOutputFile[] = [
		...outputFilesFrom(directRefs, urlFor, 'execution'),
		...outputFilesFrom(delegatedRefs, urlFor, 'delegated')
	];
	const artifacts: TaskPanelArtifact[] = [];

	for (const rawArtifact of execution.artifacts ?? []) {
		const artifact = recordOf(rawArtifact);
		if (!artifact) continue;
		const payload = recordOf(artifact.payload) ?? {};
		const artifactId = text(artifact.artifact_id) ?? 'Persisted artifact';
		const path = executionArtifactPath(payload, executionId);
		const displayName = text(payload.display_name ?? payload.file_name);
		const name = displayName ?? (path ? path.split('/').pop() || path : artifactId);
		const contentType = text(artifact.content_type ?? payload.content_type ?? payload.media_type);
		const artifactType = text(artifact.artifact_type);
		const producedAt = text(artifact.produced_at);
		const sourceExecutionId = text(artifact.source_execution_id) ?? executionId;
		if (path) {
			files.push({
				name,
				kind: outputKindOf(contentType, path),
				path,
				mediaType: contentType,
				sizeBytes: numberOf(payload.size_bytes),
				url: urlFor(path),
				scope: 'artifact',
				artifactId,
				artifactType,
				producedAt
			});
		} else {
			artifacts.push({
				id: artifactId,
				name,
				artifactType,
				contentType,
				producedAt,
				sourceExecutionId
			});
		}
	}

	return { files, artifacts, artifactsKnown: Array.isArray(execution.artifacts) };
}

function outputOf(
	task: InternalTaskListItem,
	taskFiles: readonly PanelOutputFile[] | null,
	execution: InternalExecutionDetails | null,
	urlFor: OutputUrlFor
): TaskPanelOutput | null {
	const runOutput = selectedExecutionOutput(execution, urlFor);
	const runHasContent =
		runOutput !== null && (runOutput.files.length > 0 || runOutput.artifacts.length > 0);
	if (taskFiles === null && !runHasContent) return null;
	if ((taskFiles?.length ?? 0) === 0 && !runHasContent && task.synthesis_pending === true) {
		return null;
	}
	const taskScopedFiles = (taskFiles ?? []).map((file) => ({ ...file, scope: 'task' as const }));
	return {
		files: [...taskScopedFiles, ...(runOutput?.files ?? [])],
		artifacts: runOutput?.artifacts ?? [],
		taskFilesKnown: taskFiles !== null,
		taskFilesPending: task.synthesis_pending === true,
		selectedExecutionId: text(execution?.state?.execution_id),
		runArtifactsKnown: runOutput?.artifactsKnown ?? false,
		// The run's own written account, from the row rather than from an
		// execution record: the field is the task-level one, and the Run act may
		// be describing a *live* execution whose summary belongs to a different
		// run than the outputs listed beside it here.
		summary: text(task.completion_summary),
		provenance: provenanceRows([
			[
				'Source execution id',
				text(task.latest_root_execution_id) ?? text(task.active_root_execution_id)
			],
			// L3 is where the paths live; the rows above carry only the name a
			// reader can read.
			...taskScopedFiles.map((file): [string, string] => [file.name, file.path])
		])
	};
}

/**
 * The ask blocking this task, or `null`.
 *
 * There is exactly one, and it is not a plan question: **output synthesis
 * exhausted its retries.** The backend already models that as a HITL — it emits
 * one asking the operator whether to retry — so ranking it as `Waiting on you`
 * is a translation rather than an interpretation. Without it, a synthesis-failed
 * task reads `Finished · Produced no output`, which is false twice over: the run
 * produced something, and nothing is coming until a person acts.
 *
 * **`source: 'escalation'` is a routing choice, not a fact the wire supplied.**
 * `VerdictAttention.source` is read for two things and only one is reachable
 * here: it picks the default-open act, and Run is where the failing execution's
 * id is. The other use — the generic per-source sentence — never fires, because
 * `summary` always outranks it, so the enum never reaches a reader.
 *
 * `raisedAt` is the failed execution's own `failed_at`, so the line can say how
 * long this has been sitting there; absent until the details are read, and
 * omitted rather than approximated when the record carries none (design §5).
 */
function synthesisAttention(
	task: InternalTaskListItem,
	details: InternalTaskDetails | null
): VerdictAttention | null {
	const failedExecutionId = text(task.synthesis_failed_execution_id);
	if (failedExecutionId === null) return null;

	const execution = (details?.executions ?? []).find(
		(entry) => text(entry.state?.execution_id) === failedExecutionId
	);
	const failure = execution?.state?.synthesis_failed as { failed_at?: unknown } | undefined;

	return {
		source: 'escalation',
		summary: SYNTHESIS_FAILED_SUMMARY,
		raisedAt: instantOf(failure?.failed_at)
	};
}

/**
 * The panel's whole model for one internal task.
 *
 * `details` is `null` until `/details` answers. **Both the Run and the Output
 * act come from it**, so a panel opened a moment ago shows the verdict alone
 * rather than acts describing a run it has not read yet — the same
 * absent-not-empty rule §6 applies to a partial load, applied to a load that has
 * not finished.
 *
 * `now` is here for the live run's elapsed time and nothing else; every other
 * duration on screen is derived by the panel from its own clock.
 *
 * `urlFor` mints the address a file's bytes are served from — the thumbnail, the
 * download and the open-in-tab are all that one URL. It is a parameter because
 * the scope it has to carry lives in a store this module must stay clear of; a
 * caller that cannot mint one passes nothing, and every affordance needing a URL
 * is then absent rather than broken.
 */
export function toInternalTaskPanelModel(
	task: InternalTaskListItem,
	details: InternalTaskDetails | null,
	now: number,
	urlFor: OutputUrlFor = () => null,
	selectedRunId: string | null = null,
	panel: ExecutionPanelState | null = null
): TaskPanelModel {
	// **Two executions, and which one each field reads is the point.** The verdict's
	// duration is about the task and reads the current run; the Run act is about the
	// run the reader chose. `null` for `selectedRunId` makes them the same
	// execution, which is exactly the behaviour this surface had before the picker.
	const execution = currentExecution(task, details);
	const shownExecution = selectedExecution(task, details, selectedRunId);
	const files = internalTaskOutputFiles(details, urlFor);

	return {
		// The panel's identity, and the single mechanism that resets the reader's
		// chosen act when the panel is pointed at another task. Straight from the
		// row's own id — anything derived would be a second name for one task.
		id: task.id,
		// One vocabulary, two adapters: the wire status is normalised by the
		// store's own normaliser and then mapped by the store adapter's map.
		status: verdictStatusOf(normalizeV3TaskStatus(task.status)),
		queuedFor: queuedReasonOf(normalizeV3TaskStatus(task.status)),
		attention: synthesisAttention(task, details),
		/**
		 * **`null`, and there is nothing to put here.**
		 *
		 * This surface's one attention is a synthesis failure, which is not a HITL
		 * pause: it has no correlation id, no responder in the canonical dispatcher,
		 * and no answer a person types. What unblocks it is the drawer header's
		 * *Retry synthesis*, which acts on the task rather than answering an ask —
		 * see the note beside it in `InternalTasksWorkspace`.
		 *
		 * So this route needs no responder wiring at all, and giving it one would be
		 * a re-ask loop that no payload can reach: code no test could execute, which
		 * is a worse outcome than the absence.
		 */
		ask: null,
		// **Not wired, and there is nothing to wire.** Neither the internal task
		// row nor an execution record carries an error message; the only failure
		// text on this surface is the synthesis failure above. `deriveVerdict`
		// answers `No error message was recorded`, which is exactly true.
		error: null,
		// No step-level progress reaches this payload — see `runOf`. Omitted
		// rather than approximated, so the verdict reads `Running` rather than
		// `Running · step 4 of 7` with numbers nothing produced.
		currentStep: null,
		totalSteps: null,
		currentStepLabel: null,
		elapsedMs: runElapsed(execution, now),
		// Task 7's field, carried on the internal row exactly as it is on a normal
		// one, so a wedged internal run reports `Stalled · no progress for 6m`
		// here for the same reason and off the same instant.
		lastProgressAt: instantOf(task.last_progress_at),
		// **The acceptance test, as a value.** Internal tasks have no plan — the
		// backend hard-codes `has_plan: false` on every row this surface lists —
		// so the Plan act is absent rather than empty or disabled, and the panel
		// never learns why.
		plan: null,
		run: runOf(task, shownExecution, panel),
		output: outputOf(task, files, shownExecution, urlFor),
		// **The selection is read off the execution the Run act is actually
		// describing**, never off the argument. `selectedExecution` falls back to the
		// current run for an id this payload cannot describe, and reading the
		// argument here would leave the control naming a run the act below it is not
		// about — the one failure mode a picker has that a static panel does not.
		runs: runsOf(details, text(shownExecution?.state?.execution_id))
	};
}
