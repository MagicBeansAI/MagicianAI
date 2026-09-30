/**
 * The seam: a `Task` from the store, mapped onto the panel's `TaskPanelModel`.
 *
 * Pure, and deliberately the **only** place the two vocabularies meet. The task
 * store and the panel's modules disagree about three things, and each
 * disagreement is settled here once rather than at every call site:
 *
 * 1. **How absence is spelled.** The store says `undefined` (`Task.planStatus?`,
 *    `Task.lastProgressAt?`); the pure modules say `null` throughout. The
 *    coercion is a `?? null` here, at the boundary — never a widening of the
 *    modules to `| null | undefined`, which would put two spellings for absence
 *    inside one module and make every downstream check ambiguous.
 * 2. **What a status is called.** `TaskStatus` has ten values and `deriveVerdict`
 *    models seven status words, so four of them collapse onto one — and the
 *    reason each collapsed travels with it, because the verdict's second line
 *    has to be true of the status it is about. See `VERDICT_STATUS`.
 * 3. **What an output file is.** The store carries artifact names; the outputs
 *    endpoint carries a relative path and a media type, which is what the
 *    panel's kind classification and its open/reveal affordances both need.
 *
 * It also takes one input that is neither store nor panel: the raw
 * `/execution-panel` payload for this task's current run. That is deliberate and
 * not a fourth vocabulary — two of the panel's slices are derived from it, and
 * both have to be about the execution the Run act names, which is a fact only
 * this function holds. Deriving them at the four call sites instead would be
 * four chances to read a different run's events under this run's heading.
 *
 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` and
 * `docs/components/unified-ui/unified-task-panel.md`.
 */

import type { HitlOpenTarget } from '$lib/hitl/types';
import type { PlanStep, Task, TaskStatus } from '$lib/stores/taskStore';
import type { ExecutionPanelState } from '$lib/types/executionPanel';

import type { OutputKind } from './actSummaries';
import { askTargetFrom, runAttentionFrom, type TaskAsk } from './taskAttention';
import { runsSliceOf, type RunRecord, type TaskPanelRuns } from './taskRuns';
import { deriveTimeline, runCostRows } from './taskTimeline';
import type { QueuedReason } from './taskVerdict';
import type {
	RunStepStatus,
	TaskPanelArtifact,
	TaskPanelFile,
	TaskPanelModel,
	TaskPanelOutput,
	TaskPanelPlan,
	TaskPanelQuestion,
	TaskPanelResponsibility,
	TaskPanelRun,
	TaskPanelRunStep
} from './UnifiedTaskPanel.svelte';

/**
 * One provenance row, restated structurally rather than imported from
 * `TaskActSection.svelte`. The panel's slices type theirs as `ProvenanceEntry`,
 * so these still have to satisfy it — a field added there fails to compile here
 * rather than passing through under-populated.
 */
export interface ProvenanceRow {
	label: string;
	value: string;
}

/**
 * An output file the panel renders.
 *
 * **One name for one thing**: this is `TaskPanelFile`, the panel's own file
 * shape, re-exported under the name both workspaces already import. It used to
 * be a wider local type over a narrower panel one, because the panel's contract
 * needed only a name and a kind — but a row that shows a thumbnail, a size and a
 * download needs the path, the mime and a URL, and two shapes for one file is
 * how a row and the handler that acts on it come to disagree about which file
 * they mean.
 */
export type PanelOutputFile = TaskPanelFile;

/**
 * One row of `GET /api/magician/v3/tasks/{id}/outputs`. Only the fields this
 * module reads are declared; the endpoint sends more, and a shape that restated
 * all of it would be a second copy of a contract nothing here checks.
 */
export interface TaskOutputRef {
	relative_path?: string | null;
	media_type?: string | null;
	size_bytes?: number | null;
}

/**
 * Where a file's bytes are served, given its path — or `null` when the caller
 * cannot say.
 *
 * A function rather than a base URL, because only the caller knows the scope the
 * request has to carry, and only the caller knows whether its task id is one the
 * outputs endpoint can answer for at all. A caller with no answer returns `null`
 * and every affordance that needs a URL is simply absent, which is the same
 * absent-not-greyed rule the acts follow.
 */
export type OutputUrlFor = (path: string) => string | null;

/**
 * The status word each task status becomes for `deriveVerdict`.
 *
 * A `Record<TaskStatus, …>`, so a new task status is a compile error here. That guard is the point: `deriveVerdict`'s final branch is
 * unconditional, so **an unmapped status silently reads `Finished`** — the
 * loudest possible lie about a task that has not started, and one nothing on
 * screen would contradict.
 *
 * Six of the ten map directly onto a status word the verdict models. The other four —
 * `pending`, `planning`, `ready`, `deferred` — are all "has not started", and
 * `queued` is the verdict's only state that says that without claiming
 * finality, progress or failure.
 *
 * `paused` and `archived` stay distinct all the way to the verdict. Neither is
 * queued: one can resume and the other is terminal. Collapsing either into
 * `queued` made the panel claim it was waiting for capacity, which was false.
 *
 * **The four that do collapse carry why they collapsed**, in the same row rather
 * than in a second record beside this one. Their verdict state is one word and
 * their second line is not: a task whose planner is working, and a monitor
 * asleep until its next fire, were both told they were `Waiting for a free
 * slot`, and neither was. Two maps keyed on the same union would be two answers
 * to one question about a status, and the first edit to either would make them
 * disagree without anything failing — so the word and the reason are one row and
 * are read by two accessors.
 */
const VERDICT_STATUS: Record<TaskStatus, { word: string; queuedFor: QueuedReason | null }> = {
	running: { word: 'running', queuedFor: null },
	failed: { word: 'failed', queuedFor: null },
	cancelled: { word: 'cancelled', queuedFor: null },
	completed: { word: 'finished', queuedFor: null },
	// Never planned and never started: there is nothing left to do but wait for
	// a runner, which is what the capacity sentence says.
	pending: { word: 'queued', queuedFor: 'capacity' },
	// The planner is running *now*. The run has not started, so the state is
	// still `queued`; what it is waiting for is its own plan, not a slot.
	planning: { word: 'queued', queuedFor: 'plan' },
	// Planned and approved. Same wait as `pending`, one stage later.
	ready: { word: 'queued', queuedFor: 'capacity' },
	// A precondition that was not met, or a monitor between fires (`sleeping`
	// normalises here). Both are waiting on a *time*, and a free runner now would
	// not start either of them.
	deferred: { word: 'queued', queuedFor: 'schedule' },
	paused: { word: 'paused', queuedFor: null },
	archived: { word: 'archived', queuedFor: null }
};

/** Media types a reader opens and reads. Everything else is data or unknown. */
const DOCUMENT_MEDIA_TYPES = new Set([
	'application/pdf',
	'application/msword',
	'application/rtf',
	'application/vnd.openxmlformats-officedocument.wordprocessingml.document'
]);

/**
 * Extensions, for the rows that arrive without a media type. A weaker signal
 * than the mime, so it is consulted second and never overrides one.
 */
const DOCUMENT_EXTENSIONS = new Set(['md', 'markdown', 'txt', 'pdf', 'html', 'htm', 'doc', 'docx', 'rtf']);
const IMAGE_EXTENSIONS = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'avif']);

function extensionOf(path: string): string {
	const base = path.split('/').pop() ?? path;
	const dot = base.lastIndexOf('.');
	return dot <= 0 ? '' : base.slice(dot + 1).toLowerCase();
}

/**
 * What kind of thing this file is, for the summary's plural. The mime is
 * authoritative when present — design §4 notes that real output carries one —
 * and the extension is the fallback rather than the rule.
 *
 * There is no `chart` kind and no `data` kind, for the reasons `KIND_PLURAL`
 * gives: nothing here can tell a chart from a photograph, and `2 data files`
 * tells a reader no more than `2 other files`.
 */
export function outputKindOf(mediaType: string | null | undefined, path: string): OutputKind {
	const mime = (mediaType ?? '').trim().toLowerCase();
	if (mime) {
		if (mime.startsWith('image/')) return 'image';
		if (mime.startsWith('text/') || DOCUMENT_MEDIA_TYPES.has(mime)) return 'document';
		return 'other';
	}

	const extension = extensionOf(path);
	if (IMAGE_EXTENSIONS.has(extension)) return 'image';
	if (DOCUMENT_EXTENSIONS.has(extension)) return 'document';
	return 'other';
}

/**
 * The outputs endpoint's rows as panel files, in the order it sent them —
 * `outputSummary` names the first one, so the order is the caller's claim about
 * which output matters, not this module's.
 *
 * Rows with no path are dropped rather than rendered nameless: there is nothing
 * to display, nothing to open, and a blank row would assert a file exists while
 * being unable to say which.
 */
export function outputFilesFrom(
	refs: readonly TaskOutputRef[],
	urlFor: OutputUrlFor = () => null,
	scope?: NonNullable<TaskPanelFile['scope']>
): PanelOutputFile[] {
	const files: PanelOutputFile[] = [];
	for (const ref of refs) {
		const path = typeof ref.relative_path === 'string' ? ref.relative_path.trim() : '';
		if (!path) continue;
		const mediaType = typeof ref.media_type === 'string' ? ref.media_type.trim() : '';
		files.push({
			// The basename, because the design's line is `report.md and 2 images`
			// rather than a directory tree. The full path is kept on `path` and
			// rendered in the act's provenance, which is where L3 detail belongs.
			name: path.split('/').pop() || path,
			kind: outputKindOf(ref.media_type, path),
			path,
			// The mime as reported, never inferred from the extension. `outputKindOf`
			// above does fall back to the extension, and deliberately so — it is
			// choosing a plural for a summary. This is shown to the reader as a
			// fact about the file, and an extension-derived mime would be a guess
			// printed as one.
			mediaType: mediaType || null,
			sizeBytes:
				typeof ref.size_bytes === 'number' && Number.isFinite(ref.size_bytes)
					? ref.size_bytes
					: null,
			url: urlFor(path),
			...(scope ? { scope } : {})
		});
	}
	return files;
}

/**
 * The asks blocking this task, first one first. Empty strings are not asks.
 *
 * **They keep their ids**, which is what B5 turned from decoration into
 * load-bearing: the id a task-list row carries for a plan question is the same
 * id the backend uses as that clarification's `correlation_id`, so it is what
 * matches the store's ask to the answerable target the run payload publishes for
 * it. Matching on the question *text* would work today and stop working the
 * first time either side trimmed or reflowed a string, with nothing to notice.
 */
function questionsOf(task: Task): TaskPanelQuestion[] {
	const listed = task.pendingQuestions ?? (task.pendingQuestion ? [task.pendingQuestion] : []);
	return listed
		.map((entry) => ({ id: entry.id?.trim() ?? '', question: entry.question?.trim() ?? '' }))
		.filter((entry) => entry.question.length > 0);
}

/**
 * The answerable target the run payload published for one plan question, by id.
 *
 * `/execution-panel` builds a `hitl_request` for every pending clarification and
 * sets its `id` to the session question's id — the same id the task-list row
 * carries. So this is an identifier match against a contract, not a heuristic:
 * either the payload holds a target for *this* question or it holds none, and a
 * near-miss produces `null` rather than a control that answers a different ask.
 */
function clarificationTargetFor(
	runState: ExecutionPanelState | null,
	questionId: string
): HitlOpenTarget | null {
	if (!questionId) return null;
	for (const question of runState?.run?.pending_questions ?? []) {
		const target = askTargetFrom(question?.hitl_request);
		if (target && target.id === questionId) return target;
	}
	return null;
}

/**
 * **B6 — a draft plan is an ask, and it never read as one.**
 *
 * `planStatus: 'draft'` normalises to task status `pending`, which
 * `VERDICT_STATUS` maps to `queued`, so a plan sitting there waiting for the
 * reader's approval reported `Queued · Waiting for a free slot`. That is the
 * same lie the mid-run ask fixed one field over, and it is why `approvePlan` and
 * `rejectPlan` had no reachable call site from this route: there was nothing on
 * the panel saying an approval was wanted.
 *
 * **The wire wins when it speaks.** Phase F surfaces plan approval as a
 * confirmation-shaped HITL in `/attention`, so when the run payload carries one
 * it arrives through `runAttentionFrom` and this never fires. This is the
 * fallback for a task whose row says `draft` and whose payload published no row
 * — which is the common case on `/tasks`, because a task awaiting plan approval
 * has no live execution for `/execution-panel` to describe.
 *
 * Everything here is a fact on the task record rather than a construction:
 * `correlation_id` **is** the plan id (the runtime's dispatcher looks the plan up
 * by it), `task_id` is the responder identity the dispatcher requires, and
 * `raisedAt` is the plan record's own update instant — for a draft, the update
 * that produced it, which is when the approval became due. A task with no
 * `latestPlanId` yields nothing: there is no id to answer against, and an ask
 * that cannot be answered is worse than a status that is merely vague.
 */
function planApprovalAsk(task: Task): TaskAsk | null {
	if (task.planStatus !== 'draft') return null;
	const planId = text(task.latestPlanId);
	if (!planId) return null;
	return {
		attention: {
			source: 'plan_approval',
			// No summary, so `deriveVerdict` fills the detail from `ATTENTION_COPY`
			// — `Approve the plan before it can run`. Writing a sentence here would
			// be a second copy of copy that already exists for this source.
			summary: null,
			raisedAt: task.planGeneratedAt ?? null
		},
		ask: {
			id: planId,
			source: 'plan_approval',
			input_type: 'confirmation',
			prompt: 'Approve this plan so it can run?',
			input_schema: { confirm_label: 'Approve', deny_label: 'Reject' },
			identifiers: { correlation_id: planId },
			scope: { workflow_id: task.id, task_id: task.id },
			at: task.planGeneratedAt
		}
	};
}

/**
 * The ask blocking this task, or `null`.
 *
 * **Two sources, and they answer different questions.** The store's
 * `pendingQuestions` are *plan-time* clarifications and nothing else — they are
 * the only ask a task-list row carries. A task blocked **mid-run** on an
 * approval, a diff review, an escalation or a bot sign-in appears on that row as
 * an ordinary `paused`, which accurately identifies suspension but cannot say
 * who must act. `runAttention` is the second source, fetched per task from the
 * execution-panel payload; see `taskAttention.ts` for why that endpoint and not
 * another.
 *
 * **Plan questions keep priority**, so nothing an existing task reads today
 * changes: the mid-run ask fills a gap rather than competing for a slot. The
 * pathological case — a stale plan question on a task now blocked mid-run —
 * opens the Plan act on the question it can still see, which is wrong about
 * where to go and right about what is being asked.
 *
 * For a plan question, **`source: 'clarification'` is chosen because it routes
 * to the act that holds the ask, not because the store told us the source.** The
 * store carries the questions and nothing about where they came from;
 * `VerdictAttention.source` is read for exactly two things, and only one of them
 * is reachable here. It picks the default-open act — and the Plan act is the
 * only place this model renders questions, so any run-time source would open an
 * act where nothing is being asked. The other use, the per-source fallback
 * sentence, never fires: `summary` is always the question itself, which outranks
 * it. `raisedAt` is `null` because no timestamp for a plan question exists on the
 * task record; design §5 is explicit that the duration is omitted rather than
 * approximated from the nearest available instant.
 *
 * A run ask has neither problem. Its source is a fact the backend supplied
 * rather than a routing choice, and it carries the instant it was raised — so it
 * is the first ask on this panel whose `Waiting on you · 12m` is measured rather
 * than omitted.
 *
 * `runAttentionFrom` is the same reader the execution adapter uses, over the
 * same field of the same payload, so the two surfaces cannot come to different
 * conclusions about which rows are asks.
 *
 * **Each branch carries the control for its own ask, or none.** The plan-question
 * branch looks its target up by the question's id and the run branch keeps the
 * one attached to the row it read, so the verdict and the control always name the
 * same ask — see `TaskAsk`. A branch that found no target still returns: the
 * verdict is about an ask that exists whether or not this client can answer it.
 */
function attentionOf(task: Task, runState: ExecutionPanelState | null): TaskAsk | null {
	const questions = questionsOf(task);
	if (questions.length > 0) {
		return {
			attention: { source: 'clarification', summary: questions[0].question, raisedAt: null },
			ask: clarificationTargetFor(runState, questions[0].id)
		};
	}
	return runAttentionFrom(runState?.run?.needs_attention ?? []) ?? planApprovalAsk(task);
}

/**
 * The step the run is on, 1-based, or `null`.
 *
 * Read off the step list the Run act body renders rather than off
 * `Task.currentStepIndex`, for the reason design §4 gives about the Run act's
 * counts: two sources for one fact can disagree about the same run, and the
 * body is the one the reader can check. `currentStepIndex` is also only written
 * for an execution this browser session is driving, so it is absent for exactly
 * the tasks a panel is most often opened on.
 */
function liveStepOf(task: Task): number | null {
	const index = (task.planSteps ?? []).findIndex((step) => step.status === 'in_progress');
	return index === -1 ? null : index + 1;
}

/**
 * The step statuses this client recognises. A `Record` over the store's own
 * union, so a seventh `PlanStep` status is a compile error here rather than a
 * row whose mark quietly disappears — and the values are the panel's union, so a
 * typo fails against it rather than rendering nothing.
 *
 * The two unions are spelled identically today and are still mapped rather than
 * cast: they belong to different modules with different reasons to change, and
 * the map is what makes a divergence a compile error instead of a silent gap.
 */
const STEP_STATUS: Record<PlanStep['status'], RunStepStatus> = {
	pending: 'pending',
	in_progress: 'in_progress',
	completed: 'completed',
	failed: 'failed',
	skipped: 'skipped',
	cancelled: 'cancelled'
};

/**
 * The step statuses a **delegated child run** reports, as this client's.
 *
 * The wire word is `WaitingState`, serialised as its Rust variant name — so the
 * keys are `PascalCase` and not the snake_case the rest of this payload uses.
 * That is not a typo to tidy: the enum carries no `rename_all`, its sibling
 * enums do, and `ExecutionResponsibilityPanel.svelte` already switches on the
 * same PascalCase spellings. A fixture written in snake_case would be a fixture
 * asserting a shape the backend cannot produce, and every test over it would
 * pass while the feature was dead.
 *
 * Not a `Record` over a union, because the field is typed `string` on the wire:
 * there is no union to be exhaustive against and no compile error to be had. The
 * guard available instead is the fallback — an unmapped word yields `null` and
 * the row renders **no** marker, which claims least.
 *
 * Three rows are judgements rather than translations:
 *
 * - **`WaitingChildren` is `in_progress`**, not `waiting`. The child is mid-run
 *   and waiting on *its own* children; nothing is being asked of the reader, and
 *   `waiting` on this panel means work that has stopped until something acts.
 * - **`WaitingUser` and `Paused` are `waiting`**, which is the status this union
 *   gained for them. `pending` would claim the work never started — false for a
 *   run suspended mid-flight, and the reader would have no way to tell.
 * - **`Runnable` and `PlanningComplete` are `pending`**: both mean "ready, not
 *   yet begun", which is exactly what `pending` says.
 */
const CHILD_STATUS: Readonly<Record<string, RunStepStatus>> = {
	Planning: 'pending',
	PlanningComplete: 'pending',
	Runnable: 'pending',
	Executing: 'in_progress',
	WaitingChildren: 'in_progress',
	WaitingUser: 'waiting',
	Paused: 'waiting',
	Completed: 'completed',
	Failed: 'failed',
	Cancelled: 'cancelled'
};

/** A non-empty trimmed string, or `null`. Absence and blank are one answer. */
function text(raw: string | null | undefined): string | null {
	if (typeof raw !== 'string') return null;
	const trimmed = raw.trim();
	return trimmed ? trimmed : null;
}

/**
 * The plan's steps as Run act rows.
 *
 * `statusesDescribeThisRun` is what the run picker made necessary. **A step's
 * label and its status come from different places in time**: the plan record
 * carries one set of steps and one set of marks, and the marks are the *latest*
 * execution's. So when the reader is reading an earlier run, the labels are still
 * that run's plan structure — a retry re-executes the same plan — and the marks
 * are somebody else's. Rendering them anyway would put the winning attempt's
 * green ticks over the failed attempt's timeline, which is the failure this whole
 * panel is written against: every value on screen individually correct and the
 * reading false.
 *
 * So the marks are dropped rather than the rows. `status: null` already means
 * exactly this — "not recorded", which renders **no** marker, as distinct from
 * `pending`'s "not started yet" — so the honest rendering was already in the
 * contract and needed no new state. `duration_ms` goes with it for the same
 * reason: it times whichever run last touched the step.
 *
 * **A per-run answer does exist on the wire and is empty.** The panel payload's
 * `debug.selected_execution.step_statuses` is per-execution and carries a name, a
 * status, a capability and a delegate — but the V3 adapter sends the collection
 * empty (`step_statuses: Vec::new()`), so there is nothing to read. When it fills,
 * these rows should come from it and this parameter goes away.
 */
function runStepsOf(task: Task, statusesDescribeThisRun: boolean): TaskPanelRunStep[] {
	return (task.planSteps ?? []).map((step) => ({
		label: step.description,
		durationMs: statusesDescribeThisRun ? (step.duration_ms ?? null) : null,
		// The store's `PlanStep` records no retry count, so this is `0` rather
		// than a guess. `runSummary` drops a zero segment entirely, so the header
		// says nothing about retries rather than claiming there were none.
		retries: 0,
		// `?? null` rather than a default: a record carrying a status this client
		// does not know renders **no** mark, where `pending` would claim the step
		// has not started. The row still lists the step, which is the fact we do
		// have.
		status: statusesDescribeThisRun ? (STEP_STATUS[step.status] ?? null) : null,
		origin: 'step',
		// **Both of these already arrived and were being thrown away.** The store
		// fills `PlanStep` from `plan_graph.steps[]`, where `tool` is the
		// capability the planner chose and `providing_agent_id` is the agent it
		// routed the step to; `extractPlanStepsFromPlanGraph` has carried them
		// since long before this panel existed. A row that says *which agent*
		// searched is the difference between a log and an account of the work.
		capability: text(step.tool_name),
		delegate: text(step.providing_agent_id),
		// A plan step is work this run did itself, so there is no other run for it
		// to be blocking on. `false` rather than a nullable field, for the same
		// reason `retries` is `0` here: the row has an answer, and it is "no".
		blocking: false
	}));
}

/**
 * What a delegated child's own state is called, in the reader's words.
 *
 * The wire word is `WaitingState`, serialised as its Rust variant name — the
 * same PascalCase vocabulary `CHILD_STATUS` keys on, and for the same reason the
 * two are separate maps: one answers "what mark does this row get", the other
 * answers "what does the run say it is waiting on", and a single map serving
 * both would force one of the two questions to be answered by the other's
 * vocabulary.
 *
 * An unmapped word yields `null` and **no line renders**, which claims least. A
 * fallback that echoed the enum would put `WaitingChildren` on screen, which is
 * the leak §3 forbids one field over.
 */
const RESPONSIBILITY_STATE: Readonly<Record<string, string>> = {
	Planning: 'Working out its plan',
	PlanningComplete: 'Planned, not started',
	Runnable: 'Ready to start',
	Executing: 'Running',
	WaitingChildren: 'Waiting on the work it delegated',
	WaitingUser: 'Waiting on a person',
	Paused: 'Paused',
	Completed: 'Finished',
	Failed: 'Failed',
	Cancelled: 'Cancelled'
};

/**
 * The work this run handed to other agents, as rows for the Run act's step
 * list — or an empty list when it delegated none, or when nothing can be read.
 *
 * **Exported and shared, because both adapters need it over the same payload.**
 * A second projection would be a second answer to what counts as a delegated
 * child of this run, and the two would disagree the first time either was
 * edited, invisibly — each surface renders only its own.
 *
 * **These are children of the *run*, and of nothing narrower.** The wire says
 * which agent owns each child and what it was asked to do; it does **not** say
 * which step delegated it. `DelegationReadinessEntry` carries a
 * `parent_step_id` server-side and `ExecutionPanelResponsibilityChild` drops it,
 * so no payload this panel can read attributes a child to a step. Nesting these
 * rows under whichever step happens to precede them would be a parentage every
 * value on screen corroborates and nothing can falsify — so `origin` says
 * `delegated` and the indent means "under this run", which is the relation the
 * payload actually states.
 *
 * **The guard is the record's own claim about which run it describes.** The
 * responsibility snapshot names its execution; if that is not the execution the
 * Run act names, this returns nothing rather than another attempt's children
 * under this attempt's heading. That is the same failure `deriveTimeline` guards
 * against one field over, and for the same reason: a task row that moved on
 * between fetches produces it, and every individual value would corroborate the
 * result.
 *
 * A child whose sub-goal the payload does not carry keeps its row rather than
 * being dropped. Unlike an output file with no path there **is** something to
 * say about it — which agent owns it, and what state it is in — and dropping it
 * would make the run look like it delegated less work than it did.
 */
export function delegatedStepsFrom(
	runState: ExecutionPanelState | null,
	executionId: string | null
): TaskPanelRunStep[] {
	if (runState === null || executionId === null) return [];
	const responsibility = runState.run?.responsibility ?? null;
	if (responsibility === null) return [];
	if (text(responsibility.execution_id) !== executionId) return [];

	const rows: TaskPanelRunStep[] = [];
	for (const child of responsibility.active_children ?? []) {
		if (!child) continue;
		rows.push({
			label: text(child.title) ?? 'Delegated work',
			// Nothing on this payload times a child run: the snapshot carries the
			// child's *state*, not its instants. Design §5 omits rather than
			// approximates, so no duration is claimed.
			durationMs: null,
			retries: 0,
			status: CHILD_STATUS[child.waiting_state] ?? null,
			origin: 'delegated',
			// A child run is not a capability. The payload names no tool for it, and
			// the sub-goal is already the label.
			capability: null,
			delegate: text(child.active_owner_agent_id),
			// **The one fact the row could not previously state**: whether the parent
			// is still waiting on this child. `active_children` is every child the run
			// ever delegated — the field name is the wire's, not a filter — and
			// `is_blocking` is the backend's own answer to which of them the parent is
			// still held by. Compared to `true` rather than coerced: the field is
			// declared `boolean` and arrives from JSON, and a missing one must read as
			// "not blocking" rather than as truthy.
			blocking: child.is_blocking === true
		});
	}
	return rows;
}

/**
 * Who holds this run, and what it handed to other agents — or `null` when it
 * handed out nothing.
 *
 * **This is the panel's whole account of a delegated multi-agent run**, and
 * outside `/debug` there is no other. The Run act's delegated rows say *what*
 * was handed out and to whom; nothing said who owns the run now, how ownership
 * reached them, or what the run is waiting on. Those are properties of the run
 * rather than of any one row, so they are one block rather than a column
 * repeated down the list.
 *
 * **`null` when the run delegated nothing**, not an empty block. A run with no
 * children has an owner and a waiting state, and both are already said better by
 * the verdict line and the step list — a block restating them would be an act
 * body you scroll past, which §4 forbids.
 *
 * **Three fields on this payload are deliberately not read**, on D2's rule that
 * a field the wire cannot fill must not be rendered:
 *
 * - `handover_active` is hard-coded `false` by the only producer
 *   (`execution_panel/v3_adapter.rs`), so a handover notice could never fire.
 * - `current_stage`, `current_provider`, `paused_from_state` and `latest_summary`
 *   are all hard-coded `None` there, on the snapshot and on every child.
 * - `owner_stack` is assigned the *same expression* as `owner_chain`, so reading
 *   both would be one fact twice.
 *
 * `delegation_chain` is real but is a chain of **execution ids**, not agents —
 * `build_execution_chain` pushes `node.execution_id` where `build_owner_chain`
 * pushes `node.agent_id`. Opaque ids are L3 and this block is L2, so the chain's
 * only readable content, its depth, is already carried by the child rows.
 */
export function responsibilityFrom(
	runState: ExecutionPanelState | null,
	executionId: string | null
): TaskPanelResponsibility | null {
	if (runState === null || executionId === null) return null;
	const responsibility = runState.run?.responsibility ?? null;
	if (responsibility === null) return null;
	// The same guard `delegatedStepsFrom` carries, for the same reason: a task row
	// that moved on between fetches produces a snapshot about another attempt, and
	// every value in it would corroborate the wrong heading.
	if (text(responsibility.execution_id) !== executionId) return null;

	const children = responsibility.active_children ?? [];
	// **The count of children is the list's length, never `historical_child_count`.**
	// The two agree on the wire today because both come off `child_execution_ids`,
	// and if they ever stop agreeing this block must say what the rows below it
	// show rather than what a second field claims.
	const total = children.length;
	if (total === 0) return null;

	const owner = text(responsibility.active_owner_agent_id);
	const chain = (responsibility.owner_chain ?? [])
		.map((entry) => text(entry))
		.filter((entry): entry is string => entry !== null);

	return {
		// `owner_chain` is root-first and ends at the current owner, so its last
		// entry and `active_owner_agent_id` are the same agent. The chain wins when
		// there is one: it is the field that can also say who handed the work over.
		owner: chain.length > 0 ? chain[chain.length - 1] : owner,
		ownerChain: chain,
		state: RESPONSIBILITY_STATE[responsibility.waiting_state] ?? null,
		// What the run is still held by, counted off the children rather than read
		// off `active_child_count` — for the reason `total` is counted here too.
		blocking: children.reduce((count, child) => count + (child?.is_blocking === true ? 1 : 0), 0),
		total
	};
}

/**
 * Provenance rows, with every entry that has nothing to say dropped.
 *
 * Exported because the internal-task adapter builds the same act provenance
 * from a different payload, and the rule it enforces is one nobody should have
 * to remember twice: a row whose value is absent or empty renders a label with
 * a blank `<dd>`, which claims an identifier exists while being unable to say
 * what it is.
 */
export function provenanceRows(
	entries: Array<[string, string | null | undefined]>
): ProvenanceRow[] {
	return entries
		.filter((entry): entry is [string, string] => typeof entry[1] === 'string' && entry[1].length > 0)
		.map(([label, value]) => ({ label, value }));
}

/**
 * The status word this task status becomes for `deriveVerdict`.
 *
 * Exported because there is **one** status vocabulary in this directory and two
 * adapters that need it — this one and `internalTaskPanelModel.ts`. A second
 * copy of `VERDICT_STATUS` would be a second answer to what `paused` means, and
 * the two would disagree the first time a status is added to either.
 *
 * The parameter is a `TaskStatus` rather than a `string`, so a raw wire status
 * can only reach it through `normalizeV3TaskStatus` — which is where the policy
 * for an unrecognised status already lives, and answers `pending`. That matters:
 * a status passed through raw would fall to `deriveVerdict`'s unconditional last
 * branch and read `Finished`, while `pending` maps to `queued` and reads
 * `Queued`. Both are wrong about an unknown status; only one of them claims the
 * task is done.
 */
export function verdictStatusOf(status: TaskStatus): string {
	return VERDICT_STATUS[status].word;
}

/**
 * Why a task with this status has not started, or `null` when it is not one of
 * the statuses `verdictStatusOf` answers `queued` for.
 *
 * The second accessor onto the one record above, exported for the same reason
 * that one is: three adapters build a `TaskPanelModel` and all three must give
 * the same answer about what a `deferred` task is waiting for.
 */
export function queuedReasonOf(status: TaskStatus): QueuedReason | null {
	return VERDICT_STATUS[status].queuedFor;
}

function planOf(task: Task, questions: TaskPanelQuestion[]): TaskPanelPlan | null {
	const steps = task.planSteps ?? [];
	const hasPlanAct =
		Boolean(task.hasPlan) || task.planStatus !== undefined || steps.length > 0 || questions.length > 0;
	if (!hasPlanAct) return null;

	return {
		status: task.planStatus ?? null,
		// The plan record's last-update instant. `planSummary` reads it only when
		// the status is `approved`, and the update that set that status is the
		// approval, so on that one row the two coincide. Absent for a plan the
		// server never timestamped, which renders as `approved` with no age
		// rather than an invented one.
		approvedAt: task.planGeneratedAt ?? null,
		steps: steps.map((step) => step.description),
		questions,
		provenance: provenanceRows([
			['Plan id', task.latestPlanId],
			['Approved plan id', task.approvedPlanId]
		])
	};
}

/**
 * The run the task record currently points at — the panel's default, and the
 * only one it could show before this.
 *
 * `activeExecutionId` first because a live run is the one a reader opening the
 * panel is asking about; `executionId` is the settled task's last one. Both are
 * store fields, so this is a restatement of the row rather than a choice.
 */
function currentRunIdOf(task: Task): string | null {
	return text(task.activeExecutionId) ?? text(task.executionId);
}

/**
 * The task's runs, as the picker lists them — or `null` when there is nothing to
 * choose between.
 *
 * **The options come off a field that was already on the wire and already being
 * discarded.** `/execution-panel` builds `output.recent_runs` from the execution
 * tree's **root** nodes, newest first, each carrying the run's id, when it
 * started, and how it ended — which is exactly the three facts an option needs.
 * So the control costs no request: the response the panel already fetches for
 * the Run act's timeline carries the list too.
 *
 * **Root runs only, and that filter is the backend's.** The tree also holds every
 * delegated child, and a picker listing those would offer to re-point the Run act
 * at a sub-run of the attempt it is already showing — a choice between a run and
 * a part of itself. `build_recent_runs` filters to `relationship_type == "root"`
 * server-side, so this reads the already-filtered list rather than re-deriving
 * the rule client-side, where it would be a second answer to what a run is.
 *
 * **`status` is mapped into the panel's own vocabulary** by the same
 * `verdictStatusOf` the headline uses, so an option reading `finished` and a
 * verdict reading `Finished` are one word about one outcome. `recent_runs`
 * declares its status as a `TaskStatus`, which is the type that map takes, so
 * nothing is coerced on the way.
 *
 * `null` whenever the payload has not arrived: with no list there is nothing to
 * choose from, so the control is absent rather than a dropdown holding the one
 * run we can name. That is also what makes the feature degrade cleanly — a task
 * whose run state failed to load reads exactly as it did before this existed.
 */
function runsOf(
	runState: ExecutionPanelState | null,
	selectedRunId: string | null
): TaskPanelRuns | null {
	const rows: RunRecord[] = (runState?.output?.recent_runs ?? []).map((run) => ({
		executionId: run?.execution_id,
		startedAt: run?.started_at,
		status: run?.status ? verdictStatusOf(run.status) : null
	}));
	return runsSliceOf(rows, selectedRunId);
}

/**
 * The Run act exists when the task has an execution — not when it has steps.
 * A planned task that has never run has a plan to read and no run to watch, and
 * an empty Run act would assert it started.
 *
 * `executionId` is **the run the reader chose**, which is the task's current one
 * until they choose otherwise. Everything below is derived against it, and the
 * two derivations that read the payload each carry their own guard: `deriveTimeline`
 * returns `null` unless the payload describes *this* execution, and
 * `delegatedStepsFrom` returns nothing unless the responsibility snapshot names
 * it. That is what makes the switch safe without a loading flag — during the
 * round trip for a newly chosen run the payload still describes the old one, so
 * those two are absent rather than wrong, and they fill when the reply lands.
 */
function runOf(
	task: Task,
	runState: ExecutionPanelState | null,
	executionId: string | null
): TaskPanelRun | null {
	if (!executionId) return null;
	// **One derivation, read twice.** The act's rows and the act's cost totals have
	// to be the same events: deriving the timeline again for the provenance would
	// be a second read of a payload the poll can replace between them, and a total
	// that does not add up to the rows under it is unfalsifiable on screen.
	const timeline = deriveTimeline(runState, executionId);
	// Whether the plan record's step marks are about the run this act is naming.
	// They are the *latest* run's, so they are only this run's when this run is the
	// latest — see `runStepsOf`.
	const stepsAreThisRun = executionId === currentRunIdOf(task);
	return {
		// **Delegated rows go last, after every plan step**, and that ordering is
		// load-bearing rather than cosmetic: the panel marks the live step by its
		// position in this list, and `liveStepOf` counts positions in the plan. Any
		// interleaving would put a delegated row at a plan step's index. The
		// `origin` field is what the panel actually checks — the ordering is the
		// second thing that has to be true, not the thing being relied on.
		steps: [
			...runStepsOf(task, stepsAreThisRun),
			...delegatedStepsFrom(runState, executionId)
		],
		// Read off the same payload and guarded by the same execution id as the
		// delegated rows above, and by the same function both adapters call: a run
		// that handed work out reads the same whether the reader arrived from a task
		// row or from a chat activity card.
		responsibility: responsibilityFrom(runState, executionId),
		// **The store `Task` still carries no events** — the list endpoint that
		// fills it projects none — so this comes entirely from the separately
		// fetched run state, and is `null` for every caller that passes none.
		//
		// The id is the task row's, not the payload's, because it is the id this
		// act's provenance names two lines below and the two must be one run.
		// `deriveTimeline` returns `null` rather than the other run's rows when the
		// payload disagrees, which is the case a row that moved on between fetches
		// produces.
		timeline,
		// Same source as the rows above: `deriveTimeline` folds in the delegated
		// children's events, and this is what lets each fold into its own block.
		delegations: runState?.run?.delegations ?? [],
		provenance: provenanceRows([
			['Execution id', executionId],
			// **`Task id` used to sit here and is gone.** The drawer's control row
			// carries the task id now — elided, click-to-copy, present rather than
			// reachable — so a row here was the same identifier twice in one panel,
			// and the second copy was the one behind a disclosure.
			//
			// What replaces it is the fact this act could not answer at all: what the
			// run cost and how long it really took. Every timeline row below states
			// its own bill, so a seventy-event run stated its total nowhere.
			// `runCostRows` sums the rows the body renders, so the totals and the
			// rows can only disagree if the arithmetic is wrong — never about *which
			// run* they describe.
			...runCostRows(timeline)
		])
	};
}

/**
 * The directory the outputs live in, stated once — or the per-file paths when they
 * do not share one.
 *
 * Exported for its own test: "one row when they agree, N rows when they don't" is
 * the whole behaviour, and it is a claim about a list rather than about a component.
 */
export function outputLocationRows(
	files: readonly PanelOutputFile[]
): Array<[string, string | null | undefined]> {
	if (files.length === 0) return [];
	const dirOf = (path: string) => {
		const cut = path.lastIndexOf('/');
		return cut < 0 ? '' : path.slice(0, cut);
	};
	const dirs = new Set(files.map((file) => dirOf(file.path)));
	if (dirs.size > 1) {
		// Genuinely different locations, so the path is what tells them apart.
		return files.map((file): [string, string] => [file.name, file.path]);
	}
	const [only] = [...dirs];
	// A flat listing has no directory to name, and `Directory = ''` would be a row
	// asserting a location it does not have.
	return only === '' ? [] : [['Directory', only]];
}

function safeTaskRelativePath(raw: string | null | undefined): string | null {
	const path = raw?.trim().replace(/^\.\//, '') ?? '';
	if (!path || path.startsWith('/') || path.includes('\\')) return null;
	if (path.split('/').some((component) => component === '..')) return null;
	return path;
}

function artifactName(path: string | null, displayName: string | null, artifactId: string): string {
	return displayName ?? (path ? path.split('/').pop() || path : artifactId);
}

function selectedRunOutputs(
	runState: ExecutionPanelState | null,
	executionId: string | null,
	urlFor: OutputUrlFor
): {
	files: PanelOutputFile[];
	artifacts: TaskPanelArtifact[];
	artifactsKnown: boolean;
} | null {
	if (!executionId || runState?.overview.execution_id !== executionId) return null;
	const direct = runState.output.selected_execution_outputs;
	const delegated = runState.output.selected_child_outputs;
	// These arrays are deliberately always serialized by the updated backend.
	// Their absence identifies an older payload whose run-level output ownership
	// cannot be inferred safely.
	if (!Array.isArray(direct) || !Array.isArray(delegated)) return null;

	const files: PanelOutputFile[] = [
		...outputFilesFrom(direct, urlFor, 'execution'),
		...outputFilesFrom(delegated, urlFor, 'delegated')
	];
	const rawArtifacts = runState.output.selected_execution_artifacts;
	const artifactsKnown = Array.isArray(rawArtifacts);
	const artifacts: TaskPanelArtifact[] = [];
	for (const artifact of rawArtifacts ?? []) {
		const path = safeTaskRelativePath(artifact.relative_path);
		const displayName = text(artifact.display_name);
		const name = artifactName(path, displayName, artifact.artifact_id);
		if (path) {
			files.push({
				name,
				kind: outputKindOf(artifact.content_type, path),
				path,
				mediaType: text(artifact.content_type),
				sizeBytes:
					typeof artifact.size_bytes === 'number' && Number.isFinite(artifact.size_bytes)
						? artifact.size_bytes
						: null,
				url: urlFor(path),
				scope: 'artifact',
				artifactId: artifact.artifact_id,
				artifactType: text(artifact.artifact_type),
				producedAt: text(artifact.produced_at)
			});
		} else {
			artifacts.push({
				id: artifact.artifact_id,
				name,
				artifactType: text(artifact.artifact_type),
				contentType: text(artifact.content_type),
				producedAt: text(artifact.produced_at),
				sourceExecutionId: text(artifact.source_execution_id)
			});
		}
	}
	return { files, artifacts, artifactsKnown };
}

function outputOf(
	task: Task,
	taskFiles: readonly PanelOutputFile[] | null,
	runState: ExecutionPanelState | null,
	executionId: string | null,
	urlFor: OutputUrlFor
): TaskPanelOutput | null {
	// `null` is design §6's partial load: the outputs request failed or has not
	// answered, so the act is absent. An empty act would answer `no output`,
	// which is a claim about the task made out of a fact about the network.
	//
	// **The written report is hostage to that**, and knowingly: a task whose
	// summary is its deliverable renders nothing when the *file* request fails.
	// The alternative is an Output act built on half an observation, whose L1
	// line would have to claim something about files nobody read. Absent is the
	// answer that claims least, and the panel's Retry re-reads both.
	const selectedRun = selectedRunOutputs(runState, executionId, urlFor);
	if (
		taskFiles === null
		&& (selectedRun === null
			|| (selectedRun.files.length === 0 && selectedRun.artifacts.length === 0))
	) {
		return null;
	}
	const taskScopedFiles = (taskFiles ?? []).map((file) => ({ ...file, scope: 'task' as const }));
	return {
		files: [...taskScopedFiles, ...(selectedRun?.files ?? [])],
		...(taskFiles === null ? { taskFilesKnown: false } : {}),
		...(selectedRun
			? {
					artifacts: selectedRun.artifacts,
					selectedExecutionId: executionId,
					runArtifactsKnown: selectedRun.artifactsKnown
				}
			: {}),
		// The run's own account of what it did, straight off the task record.
		// Blank is absence: a summary of `""` is not a report, and rendering it
		// would open an empty markdown block above the file list.
		summary: task.completionSummary?.trim() || null,
		provenance: provenanceRows([
			['Source execution id', task.activeExecutionId ?? task.executionId],
			// **One directory row, not one row per file.** This was
			// `files.map(f => [f.name, f.path])`, and measured on a real task it
			// produced rows whose label *was* their own value:
			//
			//     out_task_continuation_…json  =  outputs/out_task_continuation_…json
			//
			// The pair added exactly the string `outputs/`, and the names were already
			// listed in the act body above — so the rows were redundant against the
			// body and against themselves, three times over. They were also the rows
			// that rendered at 0px wide and 110 lines tall.
			//
			// The directory is the only thing they carried, so it is stated once. When
			// the files do *not* share one — `path` is documented as whatever the
			// outputs endpoint reports, so a nested layout is possible — the per-file
			// rows come back, because then the path genuinely distinguishes them.
			...outputLocationRows(taskScopedFiles)
		])
	};
}

/**
 * The panel's whole model for one task.
 *
 * `outputs` is the loaded output list, or `null` when the outputs request has
 * not answered or failed. The two are different claims and the panel renders
 * them differently, so they are different values here rather than one empty
 * array standing for both.
 *
 * `runState` is this task's current run as `/execution-panel` reports it, from
 * the caller that fetched it, or `null` when the caller has no such request or
 * it did not answer. **Two things are read out of it and both are read here**:
 * the ask blocking the run, and the log of what the run did. Handing the payload
 * over rather than two pre-derived values is what keeps the timeline's run and
 * the Run act's run the same run — only this function knows which execution the
 * act names, because only it holds the task row.
 *
 * Its absence is a single `null`, unlike `outputs`: everything derived from it
 * renders nothing when it is missing, so no empty shape could assert anything,
 * and the panel degrades to exactly the verdict the task's own status earns.
 *
 * `selectedRunId` is **the run the reader picked**, or `null` for the task's
 * current one — which is what the panel showed before the picker existed, so a
 * caller that passes nothing gets exactly the previous behaviour and a task with
 * one execution can never notice this parameter.
 *
 * It scopes the **Run act** and the run-owned half of the Output act. The task
 * output request remains task-scoped, so its report, promoted files, summary,
 * verdict, and provenance never follow a historical selection. The panel
 * payload now carries separate direct, delegated, and persisted-artifact rows
 * for `overview.execution_id`; only those groups follow the picker. This is a
 * data boundary, not an attribution heuristic.
 */
export function toTaskPanelModel(
	task: Task,
	outputs: readonly PanelOutputFile[] | null,
	runState: ExecutionPanelState | null = null,
	selectedRunId: string | null = null,
	outputUrlFor: OutputUrlFor = () => null
): TaskPanelModel {
	const questions = questionsOf(task);
	const steps = task.planSteps ?? [];
	const liveStep = liveStepOf(task);
	// **Resolved once, and every slice below reads the same answer.** A second
	// `?? current` at another call site is how the picker and the act it scopes
	// would come to name different runs.
	const runId = text(selectedRunId) ?? currentRunIdOf(task);
	// One call, so the verdict's ask and the control that answers it can never
	// come from two different rows — see `TaskAsk`.
	const asked = attentionOf(task, runState);

	return {
		// The panel's identity, and the reason the model carries one at all: it is
		// how the panel tells a task swap from another poll of the same task, and
		// therefore whose reader-choice the open act belongs to. Straight from the
		// store's id — anything derived would be a second name for the same task.
		id: task.id,
		status: verdictStatusOf(task.status),
		queuedFor: queuedReasonOf(task.status),
		attention: asked?.attention ?? null,
		ask: asked?.ask ?? null,
		error: task.errorMessage ?? null,
		currentStep: liveStep,
		// `0 of 0` is not a denominator. An empty list means the total is unknown,
		// which `stepPhrase` renders by dropping the `of N` entirely.
		totalSteps: steps.length > 0 ? steps.length : null,
		currentStepLabel:
			(liveStep === null ? null : (steps[liveStep - 1]?.description ?? null))
			?? task.currentStepTitle
			?? null,
		// **Not wired, deliberately.** No run-start instant reaches this store:
		// `executionHistory` is never populated on the V3 path, and `createdAt` is
		// when the task was written, not when the run began. Design §5's rule is
		// to omit a duration rather than approximate it, so every headline that
		// carries one drops it rather than reporting the wrong number.
		elapsedMs: null,
		// The Task 7 field, and the whole reason stall detection can work. Already
		// parsed by `parseLastProgressAt` (never `parseTimestampToIso`, which
		// answers *now* for anything unreadable and would make a wedged run
		// un-stallable); all that is left here is the absence coercion.
		lastProgressAt: task.lastProgressAt ?? null,
		plan: planOf(task, questions),
		run: runOf(task, runState, runId),
		output: outputOf(task, outputs, runState, runId, outputUrlFor),
		// **The picker's own selection is `runId`, the same value the Run act was
		// built against.** Passing `selectedRunId` here instead would let the control
		// read `#1` while the act described the current run, on any task whose row
		// moved on between the reader's click and this render.
		runs: runsOf(runState, runId)
	};
}
