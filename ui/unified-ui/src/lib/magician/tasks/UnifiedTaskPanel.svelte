<script lang="ts" context="module">
	import type { HitlInputType, HitlOpenTarget } from '$lib/hitl/types';
	import type { TaskPlanStatus } from '$lib/stores/taskStore';

	import type { OutputFile } from './actSummaries';
	import type { ProvenanceEntry } from './TaskActSection.svelte';
	import { VERDICT_MARKER } from './TaskVerdictLine.svelte';
	import type { TaskPanelRuns } from './taskRuns';
	import {
		delegationSpan,
		groupTimelineByDelegation,
		timelineWindow,
		type TimelineEntry,
		type TimelineEntryKind,
		type TimelineEntryStatus
	} from './taskTimeline';
	import type { ExecutionPanelDelegationGroup } from '$lib/types/executionPanel';
	import type { VerdictInput, VerdictState } from './taskVerdict';

	/*
	 * The panel's whole contract, below. Tasks with planning and tasks without
	 * both arrive as one of these; **which acts render is decided by which slices
	 * are present**, never by what kind of task this is. Design §2 makes that the
	 * acceptance test for the design, and the shape is where it is enforced —
	 * there is no field here naming a kind, so no branch downstream can ask.
	 */

	/**
	 * One ask blocking the plan.
	 *
	 * **It carries an id**, which is what lets the act body render the ask block
	 * for the question it is answering and list the *other* questions underneath,
	 * with nothing said twice. Matching on the question's text would work today
	 * and stop working the first moment either side trimmed a string — the ask
	 * would appear once with a control and once again as a bare line, and every
	 * value involved would be right.
	 */
	export interface TaskPanelQuestion {
		id: string;
		question: string;
	}

	export interface TaskPanelPlan {
		/** `null` when the act exists but carries no status the client recognises. */
		status: TaskPlanStatus | null;
		approvedAt: number | null;
		/** The plan's steps, in order, as the act body lists them. */
		steps: readonly string[];
		/** The asks blocking the plan, in the order the act body lists them. */
		questions: readonly TaskPanelQuestion[];
		provenance: ProvenanceEntry[];
	}

	/**
	 * How a single step of the run ended, or where it is.
	 *
	 * Its own union rather than a reuse of `VerdictState`: the two answer
	 * different questions over different state spaces. A step can be `skipped`,
	 * which no task can be; a task can be `waiting`, `stalled` or `queued`, none
	 * of which a step records. Four of the six do correspond, and those four
	 * borrow the verdict's **marks** (see `STEP_MARKER`) so one vocabulary of
	 * glyphs reads down the whole panel — but the *types* stay apart, because
	 * merging them would let a step be given a state no step can be in.
	 */
	export type RunStepStatus =
		| 'pending'
		| 'in_progress'
		| 'waiting'
		| 'completed'
		| 'failed'
		| 'skipped'
		| 'cancelled';

	/**
	 * Whether a row is one of the run's **own** steps or work the run **handed to
	 * another agent**.
	 *
	 * Two values and not a depth number, because two is all the wire can support
	 * and a `depth: number` would invite a tree nothing here can build: no payload
	 * this panel reads says which *step* delegated a child, so a delegated row is
	 * a child of the run and of nothing narrower. Naming that fact is what stops a
	 * later reader indenting delegated rows under whichever step happens to
	 * precede them — a parentage every value on screen would corroborate and
	 * nothing could falsify.
	 */
	export type RunStepOrigin = 'step' | 'delegated';

	export interface TaskPanelRunStep {
		label: string;
		/** `null` when nothing timed this step; the row then shows no duration. */
		durationMs: number | null;
		/** Retries **of this step**, which is why the Run act has no retry rows. */
		retries: number;
		/**
		 * How this step ended, or `null` when the record carries no status the
		 * client recognises — in which case the row renders **no marker at all**
		 * rather than a neutral one, because "not recorded" and "not started yet"
		 * are different facts and `⋯` claims the second.
		 */
		status: RunStepStatus | null;
		/**
		 * Whose row this is: the run's own step, or work it delegated. Drives the
		 * indent, and is what every count of "steps" filters on — the act header
		 * says `4 steps` about the plan, and delegated work is not plan structure.
		 */
		origin: RunStepOrigin;
		/**
		 * What this step used — the capability or tool the plan named for it — or
		 * `null` when the record names none. L2: it is a fact about the row the
		 * reader is looking at, in the row's own words, not an identifier.
		 */
		capability: string | null;
		/**
		 * The agent that ran this step, or `null`. On a plan step it is the
		 * provider the plan routed the step to; on a delegated row it is the agent
		 * that owns the child run. `"Searched memory"` is less useful than knowing
		 * which agent searched.
		 */
		delegate: string | null;
		/**
		 * Whether the run is **still held by this row**. Only a delegated row can be
		 * `true`: a plan step is work the run did itself, so there is no other run it
		 * could be waiting on, and every plan-step projection sets `false`.
		 *
		 * This is the one fact the delegated rows could not state. The wire's
		 * `active_children` is *every* child a run ever delegated, and its status
		 * mark says how that child ended — neither answers "which of these is the
		 * parent still waiting for", which is the question a reader looking at a
		 * stalled multi-agent run is actually asking.
		 */
		blocking: boolean;
	}

	/**
	 * Who holds this run, and how much of it is in other agents' hands.
	 *
	 * Present only on a run that **delegated something** — see `responsibilityFrom`,
	 * which answers `null` otherwise rather than handing over an empty block. The
	 * fields are the run's, not any row's: which agent owns it now, how ownership
	 * reached them, what it is waiting on, and how many of its children are still
	 * holding it.
	 */
	export interface TaskPanelResponsibility {
		/** The agent holding the run now, or `null` when the payload named none. */
		owner: string | null;
		/**
		 * Every agent ownership passed through, root first, ending at `owner`.
		 * Empty when the payload carried no chain — the owner line then renders the
		 * owner alone rather than a one-link arrow.
		 */
		ownerChain: readonly string[];
		/**
		 * What the run is waiting on, in the reader's words, or `null` for a state
		 * this client does not model — in which case **no line renders**, because a
		 * fallback would either invent a state or leak the wire's enum.
		 */
		state: string | null;
		/** How many delegated children are still holding the run. */
		blocking: number;
		/** How many it delegated in total, finished ones included. */
		total: number;
	}

	/**
	 * How many of these rows are the run's own steps.
	 *
	 * **One function rather than a `.filter().length` at each site**, because
	 * three things count this list — the act header's `4 steps`, the live-step
	 * position, and the planned total — and a count that silently included
	 * delegated rows would read `step 2 of 7` about a four-step plan with three
	 * delegations. Every one of those numbers would be individually plausible,
	 * which is exactly the class of error nothing on screen can contradict.
	 */
	export function planStepCount(steps: readonly TaskPanelRunStep[]): number {
		return steps.reduce((total, step) => total + (step.origin === 'step' ? 1 : 0), 0);
	}

	export interface TaskPanelRun {
		steps: readonly TaskPanelRunStep[];
		/**
		 * **What the run did, event by event** — a second list in this act's body,
		 * at a finer resolution than the steps above it.
		 *
		 * The two are different things, not two views of one: a step is plan
		 * structure and an entry is something that happened, which is why they are
		 * separate fields with separate nouns in the header rather than one list
		 * the header would have to describe as `step 143 of 217`.
		 *
		 * **`null` is "nothing observed this run's events"**, and the timeline is
		 * then absent — no heading, no empty line, nothing. `[]` is "we read the
		 * run's events and there were none", which says so. The same partition
		 * `output` draws, for the same reason (design §6): an empty list rendered
		 * for an unread one would assert the run did nothing.
		 */
		timeline: readonly TimelineEntry[] | null;
		/**
		 * **Who holds this run**, when it handed work to other agents — and `null`
		 * when it handed out none, which is the common case and renders nothing at
		 * all.
		 *
		 * Not folded into `steps`: every field on it is a property of the *run*
		 * rather than of any row, and a column repeating one run-level fact down a
		 * step list is the shape §4 rejects. What belongs on a row — which child is
		 * still blocking — is on the row, as `TaskPanelRunStep.blocking`.
		 */
		responsibility: TaskPanelResponsibility | null;
		/**
		 * **The delegated children whose rows are in `timeline`**, so each folds
		 * into one collapsible block instead of an unattributed interleave.
		 *
		 * Grouping metadata rather than content: the rows themselves are already
		 * in `timeline`, each carrying the `executionId` that matches an entry
		 * here.
		 *
		 * **Optional, and absence is a real answer, not a gap.** A run that
		 * delegated nothing has no groups, and so does a caller built before this
		 * field existed; both render the flat feed that every run rendered
		 * before. There is no third state to distinguish, so this is one of the
		 * few fields on this contract that needs no explicit `null`.
		 */
		delegations?: readonly ExecutionPanelDelegationGroup[];
		provenance: ProvenanceEntry[];
	}

	/**
	 * One output file, plus everything a reader needs to *get at* it.
	 *
	 * `OutputFile` (the summary modules' shape) carries a display name and a kind,
	 * which is all a one-line summary needs. A row needs more: two outputs in
	 * different directories can share a basename, so `path` is what locates one;
	 * and a thumbnail, a download and an open-in-tab are all the same URL, which
	 * only a caller holding a scope can mint.
	 *
	 * **`url` is `null` when the caller could not mint one**, and every affordance
	 * that needs it is then absent rather than broken — the same absent-not-greyed
	 * rule the acts follow.
	 */
	export interface TaskPanelFile extends OutputFile {
		/** Task-route path: a task output, or `executions/<id>/…` beneath the task root. */
		path: string;
		/** The mime the endpoint reported, or `null`. Never guessed from the name. */
		mediaType: string | null;
		/** Size in bytes, or `null` when the record carries none. */
		sizeBytes: number | null;
		/** Where this file's bytes are served, or `null` when the caller could not mint it. */
		url: string | null;
		/** Which durable layer owns this row. Omitted means a task deliverable. */
		scope?: 'task' | 'execution' | 'delegated' | 'artifact';
		/** Present when this file row is also a persisted execution artifact. */
		artifactId?: string | null;
		artifactType?: string | null;
		producedAt?: string | null;
	}

	export interface TaskPanelArtifact {
		id: string;
		name: string;
		artifactType: string | null;
		contentType: string | null;
		producedAt: string | null;
		sourceExecutionId: string | null;
	}

	export interface TaskPanelOutput {
		/** Task deliverables followed by selected-run direct/delegated files. */
		files: readonly TaskPanelFile[];
		/** Structured persisted artifacts with no file path and therefore no file actions. */
		artifacts?: readonly TaskPanelArtifact[];
		/** False when the task-output request failed; never reinterpret that as no output. */
		taskFilesKnown?: boolean;
		/** True while stable task deliverables are still being synthesized. */
		taskFilesPending?: boolean;
		/** The run-level groups below belong only to this execution. */
		selectedExecutionId?: string | null;
		/** False when the selected execution's persisted-artifact index failed to load. */
		runArtifactsKnown?: boolean;
		/**
		 * **The task's own written report**, as markdown, or `null` when it wrote
		 * none. For a task whose deliverable *is* prose this is the deliverable,
		 * and it renders as L2 — inside the open act's body — rather than as part
		 * of the L1 line.
		 *
		 * Deliberately **not** folded into `outputSummary`. That line summarises
		 * *files*, and a summary is not one: letting its presence change what the
		 * header claims would make two tasks with identical file lists read
		 * differently at L1, and `Produced no output` stays true of a task that
		 * wrote prose and no artifacts.
		 */
		summary: string | null;
		provenance: ProvenanceEntry[];
	}

	/**
	 * Everything the panel renders.
	 *
	 * It extends `VerdictInput` minus `now` — the panel owns the clock as its own
	 * prop — so the verdict's inputs cannot drift from the panel's contract. A
	 * field added to `VerdictInput` becomes a compile error here rather than an
	 * input the panel silently stops forwarding.
	 *
	 * **An act is absent when its slice is `null`.** `output: null` is an Output
	 * act that failed to load and must not render; `output: { files: [] }` is one
	 * that loaded and found nothing. Those are different claims, so they are
	 * different values (design §6) — an empty card asserts `no output`, which may
	 * be false.
	 */
	export interface TaskPanelModel extends Omit<VerdictInput, 'now'> {
		/**
		 * **Which task this is** — the only field the panel reads that says nothing
		 * about what to render.
		 *
		 * It exists because the reader's chosen act is deliberately sticky across
		 * polls, and a poll and a task swap arrive the same way: a new object on the
		 * `task` prop. Without an identity the panel cannot tell them apart, so it
		 * either discards the choice several times a minute or carries it onto the
		 * next task. Required rather than optional: a caller that omitted it would
		 * get the second of those, silently, and nothing on screen would look wrong.
		 */
		id: string;
		/**
		 * **The ask, as something that can be answered** — `attention` says what is
		 * being asked, and this is the thing that answers it.
		 *
		 * Both come off one row in the adapters, never derived apart, because a
		 * verdict reading `Waiting on you — which quarter?` over a control that
		 * approves a diff is a panel where every value on screen is individually
		 * correct. See `TaskAsk` in `taskAttention.ts`.
		 *
		 * `null` covers two cases and neither needs telling apart here: nothing is
		 * asking, or the ask published no target this client can use. Both render
		 * no control, and the verdict is unaffected either way — an ask nobody here
		 * can answer is still an ask, and saying so is the point of the line above.
		 */
		ask: HitlOpenTarget | null;
		plan: TaskPanelPlan | null;
		run: TaskPanelRun | null;
		output: TaskPanelOutput | null;
		/**
		 * **Which of the task's runs the Run act describes**, and the others the
		 * reader can switch to — or `null` when there is no choice to offer.
		 *
		 * Not an act, and deliberately not shaped like one: it renders a control
		 * rather than a section, and it changes what one act below it says rather
		 * than adding a fourth. `deriveActs` never sees it.
		 *
		 * **`null` is the absent-not-disabled rule, in the type.** A task with one
		 * run — almost all of them — renders no control at all, exactly as a task
		 * with no plan renders no Plan act. See `TaskPanelRuns`.
		 *
		 * It scopes **the Run act and nothing else**, and the panel says so in the
		 * control's own label. The verdict line above is the *task's* and does not
		 * move when the selection does: a task that eventually finished must not
		 * read `Failed` because the reader is looking at an earlier attempt. What
		 * makes that legible rather than contradictory is the option label, which
		 * carries the selected run's own outcome — `Finished` over
		 * `Run details for #1 · 2 Jul 09:12 · failed` is two true statements about
		 * two different things, each naming which.
		 */
		runs: TaskPanelRuns | null;
	}

	/**
	 * Where each ask is answered: in the act, or in the focused prompt.
	 *
	 * **Every one of the eleven renders**, here or there — `HitlPromptFields` is
	 * one component and the panel embeds the same one the Attention centre does,
	 * so this map is a decision about *where a decision should be made* rather
	 * than about which renderers exist. A `Record<HitlInputType, string | null>`,
	 * so a twelfth input type is a build failure rather than an ask with no
	 * affordance under it: `null` renders in place, and a string is the label of
	 * the control that opens the prompt — the control says where it goes, which is
	 * the whole of its job.
	 *
	 * Four hand off, and each for a reason about the decision rather than the
	 * rendering:
	 *
	 * - **`password`.** A task panel is a drawer over a list, opened to read a
	 *   status and left open while it polls. A secret must be typed into something
	 *   the reader opened *in order to* type it, and which goes away when they are
	 *   done — not into a persistent, focusable field sitting over a task list for
	 *   as long as the drawer is up. The modal is that surface; it masks the field
	 *   and is torn down on resolve.
	 * - **`tool_authorization` and `sandbox_override`.** These grant a capability —
	 *   an unlisted tool, or a command that broke sandbox policy — rather than let
	 *   a step continue. The stakes are in the ask and render in place, so the
	 *   reader learns what is being asked without leaving; the *grant* happens in a
	 *   surface that has nothing else in it, where refusal is the primary control
	 *   and takes focus. A drawer over a task list is where a reader skims. Design
	 *   §4's L2 rule wants the question inline with its answer affordance, and it
	 *   is honoured: the affordance is here and says exactly what it opens.
	 * - **`diff_approval`.** The answer requires reading N unified diffs.
	 *   `DiffStrip` takes an `xl` dialog for that, and no act body may become
	 *   something you scroll past — which is not a preference, it is the rule the
	 *   panel's whole composition is built on.
	 */
	const ASK_HANDOFF: Record<HitlInputType, string | null> = {
		text: null,
		guidance: null,
		confirmation: null,
		choice: null,
		multi_choice: null,
		external_action: null,
		file_path: null,
		password: 'Enter it securely →',
		otp: 'Enter it securely →',
		tool_authorization: 'Review and decide →',
		sandbox_override: 'Review and decide →',
		diff_approval: 'Review the changes →',
		form: null
	};

	/**
	 * The Output summary while the task can still produce something: not
	 * "nothing was produced" but "nothing yet". Design §2's mock shows it beside
	 * a live run, and it is not a placeholder for a value we have — it is the
	 * honest answer to a question that has no answer yet.
	 */
	const NOTHING_YET = '—';

	/**
	 * The glyph each step status renders, and the reason the Run act's list is
	 * readable without colour — the same property `VERDICT_MARKER` buys the
	 * verdict line, one level down.
	 *
	 * **Four of the six borrow the verdict's own marks**, by reference rather
	 * than by copying the character: a completed step and a finished task are the
	 * same claim about different scopes, so they are the same tick, and an edit to
	 * one moves both. The two that do not borrow are the two the verdict has no
	 * word for. `pending` takes the queued ellipsis because a step that has not
	 * started is waiting for its turn, which is what that mark already says; and
	 * `skipped` gets a mark of its own, because none of the seven verdict states
	 * means "deliberately not run" — `cancelled` is the closest and is wrong,
	 * since the run continued past a skipped step.
	 *
	 * A `Record<RunStepStatus, string>`, so a seventh step status fails to compile
	 * here rather than rendering an empty gutter. Every value must stay distinct,
	 * which the component test asserts as a set.
	 */
	const STEP_MARKER: Record<RunStepStatus, string> = {
		pending: VERDICT_MARKER.queued,
		in_progress: VERDICT_MARKER.running,
		waiting: VERDICT_MARKER.waiting,
		completed: VERDICT_MARKER.finished,
		failed: VERDICT_MARKER.failed,
		cancelled: VERDICT_MARKER.cancelled,
		// U+2013, not a hyphen: it reads as a struck-through row at glyph size and
		// is the one mark here with no verdict to borrow from.
		skipped: '–'
	};

	/**
	 * The tone band each step status renders in, as a `data-` attribute the CSS
	 * selects on — the same split `TaskVerdictLine` makes, and for the same
	 * reason: the mapping stays in TypeScript where the union checks it, rather
	 * than becoming a second enumeration of the statuses inside a stylesheet.
	 *
	 * Only three statuses take a colour. A failed step is the one row in a long
	 * list a reader is scanning for; the live step is where "watching" lives; and
	 * a `waiting` row is a piece of work that has *stopped* and needs something
	 * to resume it, which is the third thing a reader scans a run for. Everything
	 * else is history and reads in the body colour, because a list where every
	 * row is coloured has no signal in it.
	 */
	const STEP_TONE: Record<RunStepStatus, string> = {
		pending: 'idle',
		in_progress: 'running',
		waiting: 'waiting',
		completed: 'idle',
		failed: 'failed',
		cancelled: 'idle',
		skipped: 'idle'
	};

	/**
	 * Has the task stopped, so that a claim about what it produced is safe to
	 * make? Read off the **lifecycle** — the verdict recomputed with the ask
	 * taken back off — never off the verdict itself. A finished task with an
	 * unanswered question reads `waiting` (design §3), and `—` would be as wrong
	 * there as `no output` is on a live run: both assert something the panel does
	 * not know.
	 *
	 * Two lines key off this, and both are claims that must not be made early:
	 * the Output act's `no output`, and the Run act swapping its live position
	 * for a step count.
	 *
	 * `waiting` cannot be reached here — the lifecycle is derived with no ask,
	 * and a verdict is `waiting` only when there is one. `false` is the value
	 * that claims least, so the unreachable row cannot become a premature claim
	 * if that ever changes. A `Record<VerdictState, boolean>`, so a new state
	 * has to be classified rather than defaulting to "still going".
	 */
	/**
	 * The glyph each timeline status renders, on the same terms `STEP_MARKER` is
	 * built: four borrow the verdict's own marks by reference, so one vocabulary
	 * reads down the whole panel and an edit to a mark moves every level at once.
	 *
	 * `info` is the one that has nothing to borrow. A run-lifecycle row and a
	 * browser observation are neither outcomes nor waits — they happened, and none
	 * of the seven verdict states means that. The middle dot is the quietest mark
	 * that still holds the gutter's column, which is what a row with no verdict to
	 * report should look like.
	 *
	 * A `Record<TimelineEntryStatus, string>`, so a sixth status fails to compile
	 * here rather than rendering an empty gutter.
	 */
	const TIMELINE_MARKER: Record<TimelineEntryStatus, string> = {
		running: VERDICT_MARKER.running,
		done: VERDICT_MARKER.finished,
		failed: VERDICT_MARKER.failed,
		waiting: VERDICT_MARKER.waiting,
		info: '·'
	};

	/**
	 * The tone band each timeline status renders in, as a `data-` attribute the
	 * CSS selects on. The same two-colour split the step rows make and for the
	 * same reason: a failed row is the one a reader scanning a 200-row feed is
	 * looking for, the running row is where "watching" lives, and a list in which
	 * every row is coloured has no signal left in it.
	 */
	const TIMELINE_TONE: Record<TimelineEntryStatus, string> = {
		running: 'running',
		done: 'idle',
		failed: 'failed',
		waiting: 'idle',
		info: 'idle'
	};

	/**
	 * What each kind is called in the row's eyebrow. A `Record<TimelineEntryKind,
	 * string>`, so an eighth kind is a compile error rather than a row whose
	 * eyebrow is blank.
	 *
	 * These are the operator's words rather than the wire's namespace: an `llm.*`
	 * event reads `thinking`, because what the reader is watching is the agent
	 * think. None of them is `step`, and that is the load-bearing one — the list
	 * directly above this spends that word on plan structure, and reusing it here
	 * is the conflation this whole slice exists to keep out of the panel.
	 */
	const TIMELINE_KIND_LABEL: Record<TimelineEntryKind, string> = {
		llm: 'thinking',
		reasoning: 'reasoning',
		tool: 'tool',
		shell: 'shell',
		observation: 'observation',
		lifecycle: 'run',
		event: 'event'
	};

	const SETTLED: Record<VerdictState, boolean> = {
		waiting: false,
		failed: true,
		stalled: false,
		running: false,
		paused: false,
		cancelled: true,
		queued: false,
		archived: true,
		finished: true
	};
</script>

<script lang="ts">
	/**
	 * The task panel: a verdict line, then the acts the task has, in fixed order,
	 * with one of them open.
	 *
	 * It composes the four pure modules and the two small components rather than
	 * deciding anything they decide. What it owns is what only a composer can
	 * know: which act the reader should land in, whether a claim about output has
	 * been earned yet, how the finished verdict's sentence is built, and the
	 * difference between a task that failed and a panel that could not load.
	 *
	 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md`, and
	 * `docs/components/unified-ui/unified-task-panel.md`.
	 */
	import { afterUpdate, createEventDispatcher, onDestroy } from 'svelte';

	import HitlPromptFields from '$lib/hitl/HitlPromptFields.svelte';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import { narrate, recipeCue } from '$lib/magician/api-mining/recipeNarrative';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Select from '$lib/magician/components/native/Select.svelte';
	import {
		PROMPT_HAS_OWN_ACTIONS,
		type AttentionPromptResult
	} from '$lib/stores/attentionPromptStore';

	import { askPromptFrom, type TaskAskState } from './taskAsk';
	import { outputSummary, planSummary, runSummary } from './actSummaries';
	import {
		authenticatedTaskOutputImage,
		downloadAuthenticatedTaskOutput,
		openAuthenticatedTaskOutput
	} from './taskOutputs';
	import TaskActSection from './TaskActSection.svelte';
	import TaskVerdictLine from './TaskVerdictLine.svelte';
	import {
		bytesIfKnown,
		delimiterOf,
		parseDelimited,
		prettyJson,
		previewKindOf,
		type TaskFilePreview
	} from './taskFilePreview';
	import {
		ACT_TITLES,
		defaultOpenAct,
		deriveActs,
		type ActId
	} from './taskCapabilities';
	import {
		followsBottom,
		latencyScale,
		latencyShare,
		timelineClock,
		timelineCost,
		timelineIsolated,
		timelineOffset,
		timelineOrigin
	} from './taskTimeline';
	import { deriveVerdict, durationIfKnown, type Verdict, type VerdictAttention } from './taskVerdict';

	/** `null` while nothing has loaded. The panel then renders nothing rather than a shape. */
	export let task: TaskPanelModel | null = null;
	/** Why the last load or refresh failed. See the two cases it produces below. */
	export let loadError: string | null = null;
	/** When the state on screen was last known good. Drives the staleness line's age. */
	export let lastLoadedAt: number | null = null;
	/**
	 * The clock, as a prop rather than a capture, so the two headlines that tick
	 * against it advance when the caller polls. No default: a component that
	 * quietly read `Date.now()` would make every test that forgot it flaky.
	 */
	export let now: number;
	/**
	 * Whether each output row carries open and reveal controls.
	 *
	 * Off by default, and that default is the honest one: the panel cannot open a
	 * file. Opening one is a request against the task's outputs directory, which
	 * only a caller wired to a scope and a store can make — design §4's reason
	 * these affordances belong to the workspaces. A caller that has them turns
	 * this on and listens; one that has not gets rows that promise nothing.
	 */
	export let outputActions = false;
	/**
	 * Whether the caller answers `previewFile` — that is, whether a row can offer
	 * to show what is *in* the file.
	 *
	 * Off by default, and honest for the same reason `outputActions` is: reading a
	 * file's bytes is a request, and this component does not make requests. It
	 * renders what it was handed and dispatches what it cannot do. A caller wired
	 * to a scope turns this on and answers with `filePreview` below; one that has
	 * not gets rows that offer nothing rather than a disclosure that never fills.
	 *
	 * **Image rows do not need it.** A `<img src>` is loaded by the browser, not
	 * by the panel, so an image preview costs no caller and appears wherever the
	 * URL is known — the same split the row's controls already draw between what
	 * the browser performs and what only a wired caller can.
	 */
	export let filePreviews = false;
	/**
	 * The contents of the row the reader expanded, as the caller read them.
	 *
	 * `null` until the caller answers. Its `index` is checked against the open row
	 * before anything is drawn, so a slow reply for one file cannot render under
	 * another — the same guard the surfaces put on a task's outputs, one level
	 * down.
	 *
	 * It carries **no kind**: how to draw a file is `previewKindOf` over the row
	 * the panel is already holding, and a second copy arriving with the bytes
	 * would be a second answer to one question.
	 */
	export let filePreview: TaskFilePreview | null = null;
	/**
	 * Whether the caller answers `answer` — that is, whether the ask blocking this
	 * task carries a control at all.
	 *
	 * Off by default, and honest for the reason `outputActions` and `filePreviews`
	 * are: posting a response is a request against a scope, and this component
	 * makes no requests. A caller that has one turns this on and listens; one that
	 * has not gets the ask described by the verdict line and no affordance
	 * promising something nothing could do.
	 */
	export let answerAsk = false;
	/**
	 * Where the caller has got to with the answer it is posting.
	 *
	 * `null` when nothing is in flight. Its `id` is checked against the ask on
	 * screen before anything is drawn — the same guard `filePreview` carries, and
	 * for the same reason: this surface polls, and the ask can change while a post
	 * is in the air.
	 *
	 * **There is no `answered` state and there must not be one.** The ask goes
	 * away when the server stops listing it, never because the panel decided the
	 * post worked. An ask that looks answered and was not is the worst possible
	 * instance of the thing this panel exists to prevent.
	 */
	export let askState: TaskAskState | null = null;
	/**
	 * Act to focus/open on task selection or explicit caller navigation.
	 */
	export let preferredAct: ActId | null = null;

	/**
	 * `index` travels with the file because **the panel's `OutputFile` is not
	 * enough to act on.** It carries a display name, and two outputs in different
	 * directories can share one; the caller built this list and is the only thing
	 * that knows where each row came from. The index is into `output.files`,
	 * unchanged and in order.
	 *
	 * `previewFile` carries the whole `TaskPanelFile` rather than the narrow
	 * `OutputFile` the other two use, because for this one it **is** enough: the
	 * address, the mime and the size are all on the row, and they are exactly what
	 * a caller needs to fetch it. The index still travels, and still for the
	 * reason it does everywhere else — it is what the reply is matched against.
	 */
	const dispatch = createEventDispatcher<{
		retry: void;
		openFile: { file: TaskPanelFile; index: number };
		revealFile: { file: TaskPanelFile; index: number };
		previewFile: { file: TaskPanelFile; index: number };
		/**
		 * The reader answered the ask, or asked for the surface that can take the
		 * answer. `result` is what the shared renderer produced; `null` means *open
		 * the focused prompt* — the four asks this panel hands off (see
		 * `ASK_HANDOFF`) have no answer to carry, only a request to be taken
		 * somewhere that can collect one.
		 *
		 * The **target** travels rather than an index, because unlike an output row
		 * it is enough on its own: the id, the source, the identifiers and the scope
		 * are all on it, and they are exactly what a caller needs to post.
		 */
		answer: { ask: HitlOpenTarget; result: AttentionPromptResult | null; cancel?: boolean };
		/**
		 * The reader chose a different run to read.
		 *
		 * **The panel does not act on it**, and that is the same split every other
		 * event here draws: switching runs means re-reading `/execution-panel` for
		 * another execution id, which is a request against a scope, and this
		 * component makes no requests. It renders the control, reports the choice,
		 * and waits to be handed a model describing the run it asked for — so a
		 * caller that does not listen never gets a control at all, because it never
		 * fills `runs`.
		 *
		 * The execution id travels rather than an ordinal or an index: it is what
		 * the endpoint takes, and an index into a list that polls is a reference
		 * that can mean a different run by the time it is read.
		 */
		selectRun: { executionId: string };
	}>();

	/**
	 * The act the reader opened, and whether they have opened anything at all.
	 * Two variables rather than one three-valued one: `null` here means "the
	 * reader closed the column", which is a different fact from "the reader has
	 * not chosen", and one variable spelling absence two ways is the confusion
	 * this feature avoids everywhere else.
	 */
	let chosenAct: ActId | null = null;
	let readerChose = false;
	/**
	 * The task the choice above was made about, so a *different* task can be told
	 * from another poll of the same one. `null` until a task arrives; it is never
	 * cleared, so a panel that loses its task and gets the same one back keeps the
	 * reader where they were.
	 */
	let chosenFor: string | null = null;

	/**
	 * Which output row has its contents open, or `null` for none.
	 *
	 * **One number, so one preview.** The Output act lists files and a preview is
	 * a disclosure *within* a row; six files that could each be expanded at once
	 * is six documents stacked in one act, which is the card wall this panel
	 * replaced with extra steps. Opening one closes whatever was open, exactly as
	 * opening an act closes the act above it.
	 *
	 * It is reset by the same statement that resets the chosen act, off the same
	 * task identity, because it is the same fact: a different task is a different
	 * reader. A second mechanism for it would be one more thing that can break
	 * silently.
	 */
	let openPreview: number | null = null;
	/** The exact row path paired with `openPreview`; indexes can move on a poll. */
	let openPreviewPath: string | null = null;
	/** The selected execution whose file indexes `openPreview` currently names. */
	let previewRunFor: string | null = null;

	/**
	 * One construction site for `VerdictInput`, so a field added to it is a single
	 * compile error rather than a silently unforwarded input. `attention` is a
	 * parameter because the panel asks this twice: once as it really is, and once
	 * with the ask removed to recover the task's own lifecycle.
	 */
	function verdictOf(
		model: TaskPanelModel,
		at: number,
		attention: VerdictAttention | null
	): Verdict {
		return deriveVerdict({
			status: model.status,
			// Forwarded rather than recomputed: the reason a task has not started is
			// the adapter's answer about *this* task, and a second derivation here
			// would need the task status the model no longer carries.
			queuedFor: model.queuedFor,
			attention,
			error: model.error,
			currentStep: model.currentStep,
			totalSteps: model.totalSteps,
			currentStepLabel: model.currentStepLabel,
			elapsedMs: model.elapsedMs,
			lastProgressAt: model.lastProgressAt,
			now: at
		});
	}

	/**
	 * `no output` is a verdict, and only a task that has stopped has earned it.
	 * `outputSummary` is pure and sees a file list, so it cannot make this
	 * distinction; the panel is the only layer that knows whether anything more
	 * is coming (design §4).
	 */
	function outputLine(files: readonly OutputFile[], settled: boolean): string {
		return files.length === 0 && !settled ? NOTHING_YET : outputSummary(files);
	}

	function outputScope(file: TaskPanelFile): NonNullable<TaskPanelFile['scope']> {
		return file.scope ?? 'task';
	}

	function taskOutputFiles(output: TaskPanelOutput): readonly TaskPanelFile[] {
		return output.files.filter((file) => outputScope(file) === 'task');
	}

	function runOutputFiles(output: TaskPanelOutput): readonly TaskPanelFile[] {
		return output.files.filter((file) => outputScope(file) !== 'task');
	}

	function outputScopeTitle(scope: NonNullable<TaskPanelFile['scope']>): string {
		switch (scope) {
			case 'task': return 'Task deliverables';
			case 'execution': return 'Direct outputs';
			case 'delegated': return 'Delegated outputs';
			case 'artifact': return 'Persisted artifacts';
		}
	}

	function outputScopeDescription(scope: NonNullable<TaskPanelFile['scope']>): string {
		switch (scope) {
			case 'task':
				return 'Stable task-level deliverables. These can be promoted or replaced across runs.';
			case 'execution':
				return 'Files written directly by the selected execution.';
			case 'delegated':
				return 'Files returned by work delegated from the selected execution.';
			case 'artifact':
				return 'File-backed evidence persisted during the selected execution.';
		}
	}

	function beginsOutputScope(files: readonly TaskPanelFile[], index: number): boolean {
		return index === 0 || outputScope(files[index - 1]) !== outputScope(files[index]);
	}

	function beginsRunOutputScope(files: readonly TaskPanelFile[], index: number): boolean {
		let prevIndex = index - 1;
		while (prevIndex >= 0 && outputScope(files[prevIndex]) === 'task') {
			prevIndex--;
		}
		if (prevIndex < 0) return true;
		return outputScope(files[prevIndex]) !== outputScope(files[index]);
	}

	function selectedRunBadge(executionId: string, choices: TaskPanelRuns | null): string {
		return choices?.options.find((option) => option.executionId === executionId)?.label ?? 'RUN';
	}

	/**
	 * Open this row's contents, or close them if they are already open — a
	 * disclosure whose click does nothing is broken, which is the same invariant
	 * the act headers hold.
	 *
	 * **The request is made here and nowhere else**, which is what "fetch on
	 * expand, never on render" means concretely: listing ten files costs no
	 * requests at all, and the reader pays one per file they actually open. An
	 * image costs none even then — the browser loads the `<img>` the open row
	 * renders, so there is nothing for a caller to fetch and nothing dispatched.
	 */
	function togglePreview(file: TaskPanelFile, index: number): void {
		if (openPreview === index) {
			openPreview = null;
			openPreviewPath = null;
			return;
		}
		openPreview = index;
		openPreviewPath = file.path;
		if (previewKindOf(file.mediaType, file.path) !== 'image') {
			dispatch('previewFile', { file, index });
		}
	}

	/**
	 * Whether this row's contents can be shown at all, and by whom.
	 *
	 * Three things have to be true and they fail for different reasons: there has
	 * to be an address, there has to be a way to draw the bytes, and — for
	 * everything but an image — a caller willing to fetch them. A row failing any
	 * of them offers **no** preview control rather than a disabled one, which is
	 * the absent-not-greyed rule the acts follow, applied two levels down.
	 */
	/**
	 * Which row's stdout has just been copied, so the control can say so.
	 *
	 * **One id, so one confirmation.** The message belongs to the block the reader
	 * acted on, and a boolean would confirm on every block at once. Cleared on a
	 * timer rather than left up: it is feedback for an action, not a fact about the
	 * row, and a `Copied` that never goes away eventually describes nothing.
	 */
	let copiedDetail: string | null = null;
	let copiedTimer: ReturnType<typeof setTimeout> | null = null;

	/**
	 * Put a row's stdout on the clipboard.
	 *
	 * **The one action in the timeline that needs no caller**, exactly as the output
	 * row's `In tab` and `Download` need none: the browser performs it. That is why
	 * this is a real control rather than a dispatch nothing would answer — see the
	 * note on `screenshot captured` below for the affordance that *would* need one
	 * and has no address to offer.
	 *
	 * A failure is silent by design: `navigator.clipboard` is absent outside a
	 * secure context and rejects when the document is not focused, and neither is
	 * something a reader who clicked `Copy` on a log can act on. What must not happen
	 * is `Copied` appearing when nothing was.
	 */
	async function copyDetail(id: string, detail: string): Promise<void> {
		try {
			await navigator.clipboard?.writeText(detail);
		} catch {
			return;
		}
		copiedDetail = id;
		if (copiedTimer !== null) clearTimeout(copiedTimer);
		copiedTimer = setTimeout(() => {
			copiedDetail = null;
			copiedTimer = null;
		}, 2000);
	}

	onDestroy(() => {
		if (copiedTimer !== null) clearTimeout(copiedTimer);
	});

	function canPreview(file: TaskPanelFile): boolean {
		if (file.url === null) return false;
		const kind = previewKindOf(file.mediaType, file.path);
		if (kind === null) return false;
		return kind === 'image' || filePreviews;
	}

	/**
	 * The contents to draw under this row, or `null` while there are none to
	 * draw.
	 *
	 * The index check is the whole point: the caller answers asynchronously, and
	 * the reader can have collapsed one row and opened another in between. Without
	 * it a reply for `report.md` renders under `revenue.csv`, and every value
	 * involved is individually valid, so nothing downstream would catch it.
	 */
	function previewFor(index: number): TaskFilePreview | null {
		return filePreview !== null && filePreview.index === index ? filePreview : null;
	}

	/**
	 * The row's second line of fact: how big it is and what it is. Empty when the
	 * record carried neither, so a row with nothing to add renders no element
	 * rather than a stray separator — the same rule `runSummary` applies to its
	 * own segments.
	 */
	function fileMeta(file: TaskPanelFile): string {
		return [file.artifactType, bytesIfKnown(file.sizeBytes), file.mediaType, file.producedAt]
			.filter((segment): segment is string => !!segment)
			.join(' · ');
	}

	/**
	 * Design §3 fills a finished task's second line from the output summary —
	 * `Wrote report.md and 2 images` — and `deriveVerdict` leaves it empty for
	 * whatever composes the verb with the summary. Two shapes the prefix does not
	 * survive:
	 *
	 * - **Nothing produced.** `Wrote no output` is not a sentence, and a finished
	 *   task that produced nothing is ordinary. It gets a sentence of its own.
	 * - **No Output act.** One that failed to load tells us nothing about what the
	 *   task wrote, so the line stays empty. Reporting an absence we never
	 *   observed is design §6's fabricated verdict in miniature.
	 */
	function finishedDetail(output: TaskPanelOutput | null): string {
		if (
			output === null
			|| output.taskFilesKnown === false
			|| output.taskFilesPending === true
		) {
			return '';
		}
		const files = taskOutputFiles(output);
		return files.length === 0
			? 'Produced no output'
			: `Wrote ${outputSummary(files)}`;
	}

	/**
	 * Both act maps are **total** — a `Record<ActId, …>` for every input,
	 * including no task at all. Two reasons, and the second is the load-bearing
	 * one: an act joining `ORDER` fails to compile here rather than rendering a
	 * header with nothing on it, and the template reads these inside an `{#each}`
	 * where a nullable value would have to be re-narrowed per iteration. With no
	 * task there are no acts, so the empty rows are never rendered.
	 */
	function summariesFor(
		model: TaskPanelModel | null,
		at: number,
		settled: boolean
	): Record<ActId, string> {
		if (model === null) return { plan: '', run: '', output: '' };
		const { plan, run, output } = model;
		return {
			plan:
				plan === null
					? ''
					: planSummary({
							status: plan.status,
							approvedAt: plan.approvedAt,
							// **Every question, including the one the ask block renders.** The
							// header counts what is blocking the plan; the body decides where
							// each one is shown. Counting only the ones left in the list would
							// read `1 question waiting` about two.
							questions: plan.questions.map((question) => question.question),
							now: at
						}),
			run:
				run === null
					? ''
					: runSummary({
							// Every count comes from a list the act body renders, so the
							// header and the body cannot disagree about the same run.
							//
							// **The run's own steps, not the delegated rows beside them.**
							// The noun in this line is `steps`, which means plan structure;
							// counting delegated work into it would report `7 steps` about a
							// four-step plan and put the header's number out of step with
							// `step 4 of 7` on the row below — two numbers about one run,
							// both plausible, neither checkable.
							steps: planStepCount(run.steps),
							retries: run.steps.reduce((total, step) => total + step.retries, 0),
							// A run nobody read the events of and one that recorded none read
							// the same **here** and only here: a count of zero drops out of
							// this line like every other empty segment. The difference
							// between them is a claim about what was observed, and the act
							// body is where it is made — one renders nothing at all, the
							// other says the run recorded nothing.
							events: run.timeline?.length ?? 0,
							elapsedMs: model.elapsedMs,
							// The position replaces the count only while the run can still
							// move; a stopped task showing `step 3 of 7` would report a
							// moment that has passed as the present one.
							currentStep: settled ? null : model.currentStep,
							totalSteps: model.totalSteps
						}),
			output:
				output === null
					? ''
					: output.taskFilesKnown === false
						? (output.selectedExecutionId ? 'run output available' : '')
						: outputLine(taskOutputFiles(output), settled && output.taskFilesPending !== true)
		};
	}

	/**
	 * Who ran a step and what it used, as one line — or `''` when the record says
	 * neither, in which case the row renders no element rather than a stray
	 * separator. The rule `timelineMeta` and `fileMeta` already follow.
	 *
	 * **The agent is prefixed `via`, and the capability is not prefixed at all.**
	 * With both present, `web_search · via research-agent` is unambiguous. With
	 * only one, a bare token would be — `research-agent` and `web_search` are the
	 * same shape of word, and a reader seeing one has no way to tell which fact
	 * they are being told. Four characters buy the distinction on every row that
	 * has only half the answer, which is most of them.
	 */
	function stepWho(step: TaskPanelRunStep): string {
		const segments: string[] = [];
		if (step.capability) segments.push(step.capability);
		if (step.delegate) segments.push(`via ${step.delegate}`);
		return segments.join(' · ');
	}

	/**
	 * How ownership of the run reached the agent that holds it — `a → b → c`, or
	 * the owner alone when the payload carried no chain longer than one link.
	 *
	 * `''` when neither is known, which renders no row. The chain's last entry
	 * *is* the owner, so the two are never printed together.
	 */
	function responsibilityOwner(who: TaskPanelResponsibility): string {
		if (who.ownerChain.length > 1) return who.ownerChain.join(' → ');
		return who.owner ?? '';
	}

	/**
	 * How much of the run is in other agents' hands.
	 *
	 * Two sentences rather than a ratio, because `0 of 3` and `3 of 3` are read at
	 * a glance as the same shape of fact and they are opposite answers to "is
	 * anything still out there". Singular and plural are spelled out for the same
	 * reason `retries` is: `1 delegated runs` reads as a bug in the panel, which
	 * costs the reader trust in the number beside it.
	 */
	function responsibilityDelegated(who: TaskPanelResponsibility): string {
		const runs = `${who.total} delegated ${who.total === 1 ? 'run' : 'runs'}`;
		return who.blocking === 0
			? `${runs} · none still running`
			: `${runs} · ${who.blocking} still running`;
	}

	function provenanceFor(model: TaskPanelModel | null): Record<ActId, ProvenanceEntry[]> {
		return {
			plan: model?.plan?.provenance ?? [],
			run: model?.run?.provenance ?? [],
			output: model?.output?.provenance ?? []
		};
	}

	/**
	 * The reader reports a click; the panel decides. Clicking the open act closes
	 * it — a disclosure button whose click does nothing is broken — so the
	 * invariant here is **never two open**, not "always exactly one".
	 */
	function onToggle(event: CustomEvent<ActId>) {
		readerChose = true;
		chosenAct = openAct === event.detail ? null : event.detail;
	}

	/**
	 * The feed's own scroll box, and whether it is still following.
	 *
	 * **Its own scroll region rather than the drawer's**, which is what makes
	 * stick-to-bottom possible at all here: the drawer body scrolls three acts and
	 * a verdict, so following the newest event in it would drag the Output act off
	 * screen every time a tool returned. A 200-row feed inside an accordion also
	 * has to be bounded or the acts under it become unreachable.
	 *
	 * Starts stuck, because a reader who just opened the act wants the present,
	 * and stops the moment they scroll away from the end — the one gesture that
	 * says they are reading history rather than watching.
	 */
	let timelineEl: HTMLElement | null = null;
	let followTimeline = true;

	function onTimelineScroll(): void {
		if (timelineEl === null) return;
		followTimeline = followsBottom(timelineEl);
	}

	/**
	 * Follow new entries after every render that could have added one.
	 *
	 * `afterUpdate` rather than a reactive statement: the entries have to be in
	 * the DOM before `scrollHeight` means anything, and a `$:` block runs before
	 * the patch. Reading the *live* element each time rather than a captured
	 * height, so a row that grew — a shell command's stdout arriving after its
	 * title — is followed too.
	 */
	afterUpdate(() => {
		if (timelineEl !== null && followTimeline) {
			timelineEl.scrollTop = timelineEl.scrollHeight;
		}
	});

	/**
	 * The ask's own state, and why it lives here rather than in `HitlPromptFields`.
	 *
	 * `askPromptId` counts asks, not renders. `AttentionPromptRequest.id` is what
	 * the shared renderer resets its half-typed answer on, and on this surface the
	 * moment that should happen is when the *ask* changes — not several times a
	 * minute as the panel re-renders under a poll, and not never, which would
	 * carry one ask's draft onto the next. **One statement, keyed on the ask's own
	 * identity**, exactly as the chosen act is keyed on the task's; a second reset
	 * beside it would be one more thing that can silently stop working.
	 */
	let askPromptId = 0;
	let askPromptFor: string | null = null;
	let askFields: HitlPromptFields | null = null;
	let askCanSubmit = false;

	$: ask = task?.ask ?? null;
	$: if ((ask?.id ?? null) !== askPromptFor) {
		askPromptFor = ask?.id ?? null;
		askPromptId += 1;
	}
	$: askPrompt = ask === null ? null : askPromptFrom(ask, askPromptId);
	// `null` renders in place; a string is the label of the control that opens the
	// focused prompt. See `ASK_HANDOFF` for why each of the four hands off.
	// A secret hands off whatever its widget type: the backend's spec can
	// classify a `text` ask as a password or a code, and that ask belongs in
	// the surface opened in order to type it, not in a drawer that stays up.
	$: askHandoff =
		ask === null
			? null
			: (ASK_HANDOFF[ask.input_type] ??
				(ask.input_schema?.sensitive ? ASK_HANDOFF.password : null));
	// The state belongs to *this* ask or to none. A reply for the ask the reader
	// was looking at a moment ago must not render under the one they are looking
	// at now — the guard `previewFor` makes over an index, made here over an id.
	$: askProgress = askState !== null && ask !== null && askState.id === ask.id ? askState : null;
	$: askBusy = askProgress?.status === 'sending';
	$: askFailure = askProgress?.status === 'failed' ? askProgress.message : null;

	function onAskAnswer(event: CustomEvent<AttentionPromptResult>): void {
		if (ask === null || askBusy) return;
		dispatch('answer', { ask, result: event.detail });
	}

	/**
	 * The fields gave up on a secret ask (an expired code, a fresh one wanted).
	 * Reported as an answer of `null`, which `answerTaskAsk` turns into the
	 * explicit cancel a secret ask needs rather than a re-opened modal.
	 */
	function onAskDismiss(): void {
		if (ask === null || askBusy) return;
		dispatch('answer', { ask, result: null, cancel: true });
	}

	/**
	 * The reader picked a run. Reported, never acted on — see `selectRun`.
	 *
	 * **Re-picking the run already shown dispatches nothing.** `Select` fires
	 * `change` on any commit, and a caller that re-fetched on every one would spend
	 * a request to arrive back where it was — and would blank the Run act's
	 * timeline for a round trip while doing it, which reads as the panel losing the
	 * run the reader just confirmed they wanted.
	 *
	 * An id the slice does not list is dropped rather than forwarded. `runs` is the
	 * only thing that filled the options, so this is unreachable through the
	 * control; it is the guard against a `change` arriving between a poll replacing
	 * the model and the next render, when the value on the DOM node belongs to the
	 * previous list.
	 */
	function onSelectRun(executionId: string): void {
		if (runs === null || executionId === runs.selectedId) return;
		if (!runs.options.some((option) => option.executionId === executionId)) return;
		dispatch('selectRun', { executionId });
	}

	$: plan = task?.plan ?? null;
	$: run = task?.run ?? null;
	$: output = task?.output ?? null;
	// A run switch can put a different file at the same numeric index. Collapse
	// the old preview at that ownership boundary so its delayed bytes cannot land
	// under the newly selected execution's row.
	$: if ((output?.selectedExecutionId ?? null) !== previewRunFor) {
		previewRunFor = output?.selectedExecutionId ?? null;
		openPreview = null;
		openPreviewPath = null;
	}
	// The same execution can append a direct output ahead of an already-open
	// delegated/artifact row. Keep the index only while it still names the exact
	// same path; otherwise a delayed preview would be correctly indexed and
	// attached to the wrong file.
	$: if (openPreview !== null && output?.files[openPreview]?.path !== openPreviewPath) {
		openPreview = null;
		openPreviewPath = null;
	}
	/**
	 * The run picker's slice, and the `<Select>` options built off it.
	 *
	 * `runs` is `null` for a task with one execution and the control is then
	 * absent — never disabled (see `TaskPanelRuns`). The option list is mapped
	 * rather than passed through because `Select` wants `{ value, label }` and the
	 * slice carries the facts those were built from; the label is **not** rebuilt
	 * here, so the string the reader picks is the one the pure module composed and
	 * the one its own test pins.
	 */
	$: runs = task?.runs ?? null;
	$: runOptions = (runs?.options ?? []).map((option) => ({
		value: option.executionId,
		label: option.label
	}));
	// Lifted out for the same reason the act maps are total: the Run act body
	// reads this inside an `{#each}`, and a value narrowed outside one is not
	// narrowed inside it.
	$: currentStep = task?.currentStep ?? null;
	// Lifted for the same reason: the act body reads it inside an `{#each}`, and a
	// value narrowed outside one is not narrowed inside it.
	$: timeline = run?.timeline ?? null;
	$: recipeEvents = (timeline ?? []).flatMap((entry) => (entry.recipe ? [entry.recipe] : []));
	$: recipeNarrative = narrate(recipeEvents);
	$: taskRecipeCue = recipeCue(recipeEvents);
	// Lifted and coerced for the same two reasons `timeline` above it is: the act
	// body reads it inside an `{#each}`, and absence has one spelling here — a
	// caller that omitted the field must read as "this run delegated nothing",
	// not as an object with no fields.
	$: responsibility = run?.responsibility ?? null;
	$: visibleTimeline = timeline === null ? null : timelineWindow(timeline);
	// Grouped from the *windowed* rows, not the whole feed: a block must contain
	// exactly the rows rendered under it, or its "N steps" would count rows the
	// window trimmed and the reader cannot see.
	$: timelineSegments = groupTimelineByDelegation(
		visibleTimeline?.entries ?? [],
		run?.delegations ?? []
	);
	let timelineMode: 'grouped' | 'chronological' = 'grouped';
	/**
	 * The yardstick the latency bars are drawn against — the longest call in this
	 * feed, or `null` when fewer than two rows were timed and there is therefore
	 * no comparison to draw.
	 *
	 * Computed **once per feed** rather than inside the `{#each}`: a per-row
	 * derivation would be O(n²) over a two-hundred-row log, and worse, each row
	 * would be free to answer "longest" differently.
	 */
	$: timelineScale = timeline === null ? null : latencyScale(timeline);
	/**
	 * The instant every row's offset is measured from. Computed **once per feed**
	 * for the reason `timelineScale` is: a per-row derivation would be O(n²) over a
	 * two-hundred-row log, and every row would be free to answer "when did this run
	 * begin" differently — which is the class of disagreement nothing on screen can
	 * catch, because each row's number would be individually plausible.
	 */
	$: timelineFrom = timeline === null ? null : timelineOrigin(timeline);
	/**
	 * What an observed-but-empty feed says. Two sentences rather than one, on the
	 * same rule the Output act's `—` follows: a run that has stopped recorded
	 * nothing and that is final, while a run still going has recorded nothing
	 * **yet**, and one sentence covering both would make the second a finality
	 * claim it has not earned.
	 */
	$: timelineEmptyLine = settled
		? 'No activity was recorded for this run'
		: 'No activity recorded yet';

	$: verdict = task === null ? null : verdictOf(task, now, task.attention);

	// The task's own state with the ask taken off. Only computed while blocked,
	// because with no ask the verdict already is the lifecycle.
	$: lifecycle =
		task === null || verdict === null
			? null
			: task.attention === null
				? verdict.state
				: verdictOf(task, now, null).state;
	$: settled = lifecycle !== null && SETTLED[lifecycle];

	// `deriveVerdict` leaves the finished row's detail for the panel to compose;
	// every other state already carries its own sentence and keeps it.
	//
	// **The only place the panel overwrites a detail**, and therefore the only
	// place a failed task's `Couldn't read revenue.csv — file not found` could be
	// replaced by what it wrote. The equality names one member of the union on
	// purpose; the component test pins every other state's detail verbatim, so
	// widening this fails rather than quietly eating the sentence that matters
	// most (design §6, case 1).
	$: shownVerdict =
		verdict === null
			? null
			: verdict.state === 'finished'
				? { ...verdict, detail: finishedDetail(output) }
				: verdict;

	// What the task *has*. The panel never asks what it is.
	$: acts =
		task === null
			? []
			: deriveActs({
					hasPlanAct: plan !== null,
					hasRunAct: run !== null,
					hasOutputAct: output !== null
				});

	$: summaries = summariesFor(task, now, settled);
	$: actProvenance = provenanceFor(task);

	$: autoAct =
		verdict === null ? null : defaultOpenAct(verdict.state, acts, task?.attention?.source ?? null);

	/**
	 * Which act holds the ask — the same expression that decides where the panel
	 * opens, evaluated once and read by both.
	 *
	 * It has to be `autoAct` rather than `openAct`: the reader may have opened a
	 * different act, and the ask does not move to follow them. Deriving it a
	 * second way would let the act the panel opens and the act the ask is in come
	 * apart, which is a panel that lands the reader somewhere the control is not.
	 */
	$: askAct = ask === null ? null : autoAct;

	/**
	 * The plan questions the list renders: every one except the one the ask block
	 * is already showing, matched on the id both carry.
	 *
	 * Not filtered on the *text*: the two strings are the same field of the same
	 * record today, and the first trim on either side would put the ask on screen
	 * twice — once with a control and once as a bare line — with every value
	 * involved still correct.
	 */
	$: planQuestions = (plan?.questions ?? []).filter(
		(question) => !(answerAsk && ask !== null && question.id === ask.id)
	);

	/**
	 * **A different task is a different reader.** The choice belongs to the task it
	 * was made about, and this is the one thing that ends it — not a reload, not an
	 * act disappearing, not the caller remounting the component. Callers must not
	 * add a second mechanism: two of them means neither is pinned, and the one
	 * that stops working is the one that was doing the job.
	 *
	 * Only `task.id` is read, so a poll delivering a new object for the same task
	 * leaves the choice alone — which is the whole point of it being sticky.
	 */
	let lastHandledPreferredAct: ActId | null = null;

	$: if (task !== null && task.id !== chosenFor) {
		chosenFor = task.id;
		readerChose = preferredAct !== null;
		chosenAct = preferredAct;
		lastHandledPreferredAct = preferredAct;
		// The open row belongs to the task it was opened on, for exactly the
		// reason the chosen act does — and it is reset *here*, in the one
		// statement, rather than by a second one keyed on the same id.
		openPreview = null;
		openPreviewPath = null;
	}

	$: if (preferredAct !== lastHandledPreferredAct) {
		lastHandledPreferredAct = preferredAct;
		if (preferredAct !== null) {
			chosenAct = preferredAct;
			readerChose = true;
		}
	}

	/**
	 * The reader's choice outlives a poll — recomputing from state on every tick
	 * would throw it away invisibly, several times a minute. It is dropped only
	 * when the act they chose is no longer there to open, in which case the panel
	 * falls back to what the state asks for rather than leaving the column shut.
	 */
	$: openAct = !readerChose
		? autoAct
		: chosenAct === null
			? null
			: acts.includes(chosenAct)
				? chosenAct
				: autoAct;

	// Stale is "a refresh failed while we still have something on screen", not a
	// threshold someone picked: with polling healthy, state from two minutes ago
	// is current, and with it broken, state from two seconds ago is not.
	$: stale = task !== null && loadError !== null;
	$: staleAge = durationIfKnown(lastLoadedAt === null ? null : now - lastLoadedAt);
	$: staleLine =
		staleAge === null
			? 'Showing last known state — reconnecting'
			: `Showing last known state from ${staleAge} ago — reconnecting`;
</script>

{#if task === null && loadError !== null}
	<!--
		Design §6, case 2. **Not a verdict.** A panel that could not load renders no
		verdict state and no acts, because a red `Failed` here would be a claim
		about the task invented out of a claim about the network. The dash-clause in
		the design's copy — "Can't load this task — retry" — is an affordance rather
		than a sentence, so it is a real control; the words alone would promise
		something nothing could do.
	-->
	<div class="panel panel--unloadable" role="status">
		<p class="panel__load-error">Can't load this task</p>
		<p class="panel__load-error-detail">{loadError}</p>
		<Button
			label="Retry"
			variant="outline"
			size="sm"
			className="panel__retry"
			on:click={() => dispatch('retry')}
		/>
	</div>
{:else if shownVerdict !== null}
	<div class="panel">
		{#if stale}
			<!--
				Design §6, case 3. Its own region rather than folded into the verdict:
				the verdict text has not changed — that is the whole point — so nothing
				would announce that what is on screen stopped being current.
			-->
			<p class="panel__stale" role="status">{staleLine}</p>
		{/if}

		<TaskVerdictLine verdict={shownVerdict} cue={taskRecipeCue} />

		<!--
			Order comes from `deriveActs`, which reads it off the one lifecycle order
			there is. The bodies below switch on the act with no `{:else}`, so a
			fourth act renders an empty body rather than another act's — and both
			`Record<ActId, …>` maps above fail to compile until it is given a summary
			and a provenance list.
		-->
		{#each acts as id (id)}
			{#if id === 'run' && runs !== null}
				<!--
					**The run picker, and it sits here rather than in the header.**

					Three placements were available and two of them mislead. The drawer's
					header is already five rows, so a sixth costs every task the vertical
					space for something almost none of them can use — and a control up
					there, above the verdict, reads as scoping the *panel*, when it scopes
					one act. Inside the Run act's body it would vanish whenever the act is
					collapsed, so a reader scanning the closed headers could not tell that
					`Run · 4 steps` is describing the second attempt rather than the last.

					So it sits **immediately above the act it changes**, inside the same
					`{#each}` that orders the acts, which is what makes its scope
					unambiguous: the label names the act, and the act is the next thing on
					screen. It renders only when there is a choice — `runs` is `null` at one
					execution — and only in front of the Run act, so a task whose Run act
					failed to load gets no orphaned control.

					**It is deliberately outside the verdict's reach.** `Run details for`
					says what moves; the verdict line above it is the task's and stays put.
					The option carries the selected run's own outcome, so a `Finished`
					verdict over `#1 · 2 Jul 09:12 · failed` reads as two facts about two
					things rather than a contradiction.
				-->
				<div class="run-picker">
					<span class="run-picker__label">Run details for</span>
					<!--
						`ariaLabel` repeats the visible words rather than replacing them, for
						the reason the drawer's thread mover gives: a control whose accessible
						name shares no words with the text beside it cannot be addressed by
						voice. It says more than the label because "Run details for" alone
						does not say *out of what*.
					-->
					<div class="run-picker__control">
						<Select
							value={runs.selectedId}
							interactive={true}
							ariaLabel="Run details for which execution"
							options={runOptions}
							on:change={(event) => onSelectRun(event.detail.value)}
						/>
					</div>
					<!--
						The total, which no option can carry: `#2` says where the reader is
						and this says how far the list goes. Singular is unreachable — the
						slice is `null` below two — but the plural is written from the count
						rather than hardcoded, because a rule that reads off the data cannot
						fall out of step with it.
					-->
					<span class="run-picker__count"
						>of {runs.options.length} run{runs.options.length === 1 ? '' : 's'}</span
					>
				</div>
			{/if}
			<TaskActSection
				{id}
				title={ACT_TITLES[id]}
				summary={summaries[id]}
				open={openAct === id}
				provenance={actProvenance[id]}
				on:toggle={onToggle}
			>
				<!--
					**The ask, and the thing that answers it, in the act the reader was
					sent to** — design §4's L2 rule, that being blocked and unblocking
					yourself happen in one place.

					`askAct` is `defaultOpenAct` over the ask's own source, which is the
					same expression that decides where the panel opens, so the ask is by
					construction in the act the reader lands in. One render site: for a
					plan clarification the matching `<li>` below is suppressed rather than
					this block moving into the list, because a mid-run ask has no list to
					move into and two sites is two things to keep in step.

					Only when the caller answers. Without a listener the verdict line has
					already said what is being asked, and a second copy of it under a
					control that could not post would be a promise with nothing behind it.
				-->
				{#if answerAsk && ask !== null && askPrompt !== null && id === askAct}
					<div class="ask" data-ask-source={ask.source} data-ask-input={ask.input_type}>
						<p class="ask__prompt">{ask.prompt}</p>
						{#if askHandoff === null}
							<HitlPromptFields
								bind:this={askFields}
								bind:canSubmit={askCanSubmit}
								request={askPrompt}
								busy={askBusy}
								on:answer={onAskAnswer}
								on:dismiss={onAskDismiss}
							/>
							{#if !PROMPT_HAS_OWN_ACTIONS[askPrompt.kind]}
								<!--
									The submit for the shapes that are fields rather than
									decisions. The modal puts this in a footer; an act body has no
									footer, so it sits under the field — and it gates on the same
									`canSubmit` the modal's does, because there is one gate.
								-->
								<div class="ask__actions">
									<Button
										label={askBusy ? 'Sending…' : 'Answer'}
										variant="secondary"
										size="sm"
										className="ask__submit"
										disabled={!askCanSubmit || askBusy}
										on:click={() => askFields?.submit()}
									/>
								</div>
							{/if}
						{:else}
							<div class="ask__actions">
								<Button
									label={askBusy ? 'Sending…' : askHandoff}
									size="sm"
									className="ask__submit"
									disabled={askBusy}
									on:click={() => dispatch('answer', { ask, result: null })}
								/>
							</div>
						{/if}
						{#if askFailure !== null}
							<!--
								**The ask stays open and says why.** `role="alert"` because the
								reader has just acted and nothing else on screen changed: the
								verdict still reads `Waiting on you`, which is now the truth
								twice over. An ask that looked answered and was not is the
								failure this whole feature is against.
							-->
							<p class="ask__error" role="alert">{askFailure}</p>
						{/if}
					</div>
				{/if}

				{#if id === 'plan' && plan !== null}
					{#if planQuestions.length > 0}
						<!--
							The plan's asks. The one the block above is answering is absent
							from this list — matched by **id**, so a question and its control
							cannot both appear and cannot both disappear. The header's count
							still names every question, because what is blocking the plan does
							not change with where each one is drawn.
						-->
						<ul class="plan-asks">
							{#each planQuestions as question (question.id)}
								<li class="plan-ask">{question.question}</li>
							{/each}
						</ul>
					{/if}
					{#if plan.steps.length > 0}
						<ol class="plan-steps">
							{#each plan.steps as step}
								<li class="plan-step">{step}</li>
							{/each}
						</ol>
					{/if}
				{:else if id === 'run' && run !== null}
					<ol class="run-steps">
						{#each run.steps as step, index}
							{@const stepDuration = durationIfKnown(step.durationMs)}
							{@const who = stepWho(step)}
							<li
								class="run-step"
								class:run-step--current={!settled &&
									step.origin === 'step' &&
									currentStep === index + 1}
								data-step-status={step.status ?? 'unknown'}
								data-step-origin={step.origin}
								data-step-tone={step.status === null ? 'idle' : STEP_TONE[step.status]}
							>
								<!--
									The gutter is present on every row and empty on the rows whose
									status was never recorded, so the labels stay on one left edge
									instead of shuffling left when a status is missing. `aria-hidden`
									because the mark is a restatement of `data-step-status` for the
									eye; a screen reader that read `✓` per row would hear a glyph
									name, not a status.
								-->
								<span class="run-step__marker" aria-hidden="true"
									>{step.status === null ? '' : STEP_MARKER[step.status]}</span
								>
								<span class="run-step__label">{step.label}</span>
								{#if step.retries > 0}
									<!--
										Nested under the step rather than repeated as rows, so
										"5 retries" reads as marks on two steps instead of five
										lines the reader has to notice are the same one.
									-->
									<span class="run-step__retries"
										>{step.retries} {step.retries === 1 ? 'retry' : 'retries'}</span
									>
								{/if}
								{#if stepDuration !== null}
									<span class="run-step__duration">{stepDuration}</span>
								{/if}
								{#if step.blocking}
									<!--
										On the row rather than in the block below, because it is the one
										responsibility fact that is about *this child* and not about the
										run. A reader asking why a delegated run has not finished is
										asking which child it is still held by, and this is that
										answer — beside the child, not in a second list of the same
										children two elements down.
									-->
									<span class="run-step__blocking">Still holding the run</span>
								{/if}
								{#if who !== ''}
									<!--
										Who did this, on its own line under the label: the capability
										the plan named and the agent it routed the step to. L2 and not
										L3 — an agent name and a capability are the row's own words
										about work the reader is already looking at, exactly as a
										timeline row's model and token bill are. What stays behind
										Details is unchanged: the execution id and the task id, which
										are identifiers rather than descriptions.
									-->
									<span class="run-step__who">{who}</span>
								{/if}
							</li>
						{/each}
					</ol>

					<!--
						Who holds this run, when it handed work to other agents. Absent
						entirely — no heading, no empty tree — for a run that delegated
						nothing, which is every run on most tasks.

						**Here rather than behind Details**, and here rather than above the
						steps. Not L3: an agent name is a description, not an identifier, and
						the ladder puts opaque ids behind Details and the row's own words in
						the body — the same call `run-step__who` makes one element up. And
						below the steps rather than above them because the delegated rows are
						the tail of that list: this block is the account of the rows the
						reader has just read, not a preamble to the ones they have not.
					-->
					{#if responsibility !== null}
						{@const who = responsibility}
						{@const ownerLine = responsibilityOwner(who)}
						<dl class="run-responsibility">
							{#if ownerLine !== ''}
								<div class="run-responsibility__row">
									<dt>Owner</dt>
									<dd>{ownerLine}</dd>
								</div>
							{/if}
							{#if who.state !== null}
								<div class="run-responsibility__row">
									<dt>Waiting on</dt>
									<dd>{who.state}</dd>
								</div>
							{/if}
							<div class="run-responsibility__row">
								<dt>Delegated</dt>
								<dd>{responsibilityDelegated(who)}</dd>
							</div>
						</dl>
					{/if}

					{#if recipeNarrative.length > 0}
						<section class="recipe-narrative" aria-label="Learned API recovery summary">
							{#each recipeNarrative as line (line)}<p>{line}</p>{/each}
						</section>
					{/if}

					<!--
						The timeline: the same act at a finer resolution. Absent entirely when
						nothing observed the run's events — no heading, no placeholder — and a
						sentence of its own when something did observe and found none.
					-->
					{#if timeline !== null}
						{#if timeline.length === 0}
							<p class="run-timeline__empty">{timelineEmptyLine}</p>
						{:else}
							<div class="run-timeline__header">
								{#if visibleTimeline && visibleTimeline.hidden > 0}
									<p class="run-timeline__window" role="note">
										Showing the latest {visibleTimeline.entries.length} of {timeline.length} events
									</p>
								{:else}
									<span class="run-timeline__title">Run activity</span>
								{/if}
								<div class="run-timeline__modes" role="group" aria-label="Timeline grouping mode">
									<Button
										label="Grouped"
										variant={timelineMode === 'grouped' ? 'secondary' : 'outline'}
										size="sm"
										className="run-timeline__mode-btn {timelineMode === 'grouped' ? 'is-active' : ''}"
										on:click={() => (timelineMode = 'grouped')}
									/>
									<Button
										label="Chronological"
										variant={timelineMode === 'chronological' ? 'secondary' : 'outline'}
										size="sm"
										className="run-timeline__mode-btn {timelineMode === 'chronological' ? 'is-active' : ''}"
										on:click={() => (timelineMode = 'chronological')}
									/>
								</div>
							</div>
							<!--
								`role="log"` rather than `role="status"`: this is an append-only
								stream of independent entries, and `aria-live="polite"` on the
								container announces the new one instead of re-reading the feed.
								The verdict above already owns the panel's one `status` region.
							-->
							{#snippet timelineRow(entry: TimelineEntry)}
									{@const cost = timelineCost(entry)}
									{@const latency = durationIfKnown(entry.latencyMs)}
									{@const clock = timelineClock(entry.at)}
									{@const offset = timelineOffset(entry.at, timelineFrom)}
									<li
										class="timeline-row"
										data-timeline-kind={entry.kind}
										data-timeline-status={entry.status ?? 'unknown'}
										data-timeline-tone={entry.status === null
											? 'idle'
											: TIMELINE_TONE[entry.status]}
										data-timeline-isolated={timelineIsolated(entry)}
									>
										<!--
											**When, at the leading edge, in both forms the owner asked
											for.** The wall clock lines this row up against a log, a
											chat transcript or a memory of when the ask went in; the
											offset says how far into the run it happened, which is the
											only one of the two that survives being read a day later.
											They sit together because they answer one question — the
											latency at the trailing edge answers a different one, and
											a reader running down the feed gets one column of *when*,
											then what, then how long.

											A `<time>` element, so the instant is machine-readable
											rather than eight characters that look like one; the
											offset is not one, because there is no `datetime` grammar
											for "1m 12s after something else". Either half is absent
											rather than approximated on a row the wire gave no
											instant for — see `timelineClock` and `timelineOffset`.

											**The column is present on every row and empty on the
											rows with nothing to put in it**, exactly as the marker
											gutter beside it is: an absent cluster would shunt one
											row's title left of every other row's, which turns a
											missing fact into a broken grid.
										-->
										<span class="timeline-row__when">
											{#if clock !== null}
												<time class="timeline-row__clock" datetime={new Date(entry.at).toISOString()}
													>{clock}</time
												>
											{/if}
											{#if offset !== null}
												<span class="timeline-row__offset">{offset}</span>
											{/if}
										</span>
										<!--
											The fifth reader of the disclosure column, so an event's mark
											lands on the same axis as its step's, its act's and the
											verdict's. `aria-hidden` for the reason every other marker
											here is: it restates `data-timeline-status` for the eye, and
											a reader hearing `·` learns a glyph name rather than a state.
										-->
										<span class="timeline-row__marker" aria-hidden="true"
											>{entry.status === null ? '' : TIMELINE_MARKER[entry.status]}</span
										>
										<span class="timeline-row__kind">{TIMELINE_KIND_LABEL[entry.kind]}</span>
										<span class="timeline-row__title"
											>{entry.title}{#if timelineMode === 'chronological' && entry.agentId}<span
													class="timeline-row__agent-tag">via {entry.agentId}</span
												>{/if}</span
										>
										{#if latency !== null}
											<!--
												At the end of the row, where a step's duration already sits.
												It is the same fact about a smaller thing, so it takes the
												same place rather than joining the cost line below.

												**The bar is the number's own underline, not a row of its
												own.** A feed is already the longest thing in this panel and
												a second line per entry would double it — the constraint
												this act is held to is that no body becomes something the
												reader scrolls past to reach the next act. Drawn behind a
												right-aligned, tabular number in a fixed-width track, so the
												bars share one baseline and one origin and the eye can run
												down them; absent entirely on a row nothing timed, and on
												every row of a feed with only one measurement in it.
											-->
											{@const share = latencyShare(entry.latencyMs, timelineScale)}
											<span class="timeline-row__latency">
												{#if share !== null}
													<span
														class="timeline-row__bar"
														aria-hidden="true"
														style="width: {share}%"
													></span>
												{/if}
												<span class="timeline-row__latency-value">{latency}</span>
											</span>
										{/if}
										{#if entry.body}
											<div class="timeline-row__body">
												<ChatMarkdown content={entry.body} sessionId={null} />
											</div>
										{/if}
										{#if entry.model || cost.length > 0}
											<!--
												Which model, what it cost, how much was cached. L2 detail
												about a row the reader is already looking at, exactly as an
												output row's size and mime are — see `timelineMeta` for why
												this is not L3.

												**The model is set in `<code>` and the figures are not**, which
												is the distinction the owner asked for and one the data
												supports: `entry.model` is its own field carrying a provider's
												verbatim identifier, while the token counts are numbers a
												reader compares down a column. Joined into one string — which
												is what `timelineMeta` still does for any caller that wants
												the plain text — they had to share one treatment, and the
												treatment they shared made `claude-opus-4-20250514` read as
												prose.
											-->
											<span class="timeline-row__meta">
												{#if entry.model}
													<code class="timeline-row__id">{entry.model}</code>
												{/if}
												{#if cost.length > 0}
													<span class="timeline-row__figures">{cost.join(' · ')}</span>
												{/if}
											</span>
										{/if}
										{#if entry.detail}
											<!--
												**A code container, not a monospace paragraph.**

												`<figure>` + `<pre><code>` is what a block of machine output is:
												`<pre>` keeps the whitespace, `<code>` says the content is code
												rather than merely fixed-width, and the figure is what lets the
												block carry a control without putting a `<button>` inside a
												`<pre>` whose whitespace is significant.

												**Only `shell` entries reach here**, and that is a fact about
												`deriveTimeline` rather than a guess: every other kind sets
												`detail: null`, so this block is a command's stdout and nothing
												else. There is deliberately no per-kind map deciding what the
												block *is* — six of its seven rows would be unreachable, which
												is an exhaustiveness guard that guards nothing. The row's own
												eyebrow already says `SHELL`.

												`Copy` needs no caller, exactly as `In tab` and `Download` need
												none: the browser performs it. Compare `screenshot captured`
												below, which is the affordance that would need one.
											-->
											<figure class="timeline-row__code">
												<pre class="timeline-row__detail"><code>{entry.detail}</code></pre>
												<figcaption class="timeline-row__code-actions">
													<Button
														label={copiedDetail === entry.id ? 'Copied' : 'Copy'}
														variant="outline"
														size="sm"
														className="timeline-row__copy"
														ariaLabel={`Copy output of ${entry.title}`}
														on:click={() => void copyDetail(entry.id, entry.detail ?? '')}
													/>
												</figcaption>
											</figure>
										{/if}
										{#if entry.screenshot}
											<!--
												**A claim with nothing behind it, and it stays a claim.**
												`TimelineEntry.screenshot` is a boolean: the observation it comes
												from has an `observation_id`, but `deriveTimeline` does not carry
												it and no endpoint here mints a URL for it — so there is no
												address to view, download or preview. An affordance would be a
												control that could not work, which is the one thing this panel
												refuses everywhere else. What is needed is the id on the entry
												and a caller that can fetch it; both are recorded as owed.
											-->
											<span class="timeline-row__capture">screenshot captured</span>
										{/if}
									</li>
							{/snippet}

							<ol
								class="run-timeline"
								role="log"
								aria-label="Run activity"
								bind:this={timelineEl}
								on:scroll={onTimelineScroll}
							>
								{#if timelineMode === 'grouped'}
									{#each timelineSegments as segment (segment.kind === 'row' ? segment.entry.id : `delegation:${segment.group.execution_id}`)}
										{#if segment.kind === 'row'}
											{@render timelineRow(segment.entry)}
										{:else}
											{@const span = delegationSpan(segment.entries, segment.group)}
											<li class="timeline-delegation" data-delegation-status={segment.group.status}>
												<details open>
													<summary class="timeline-delegation__summary">
														<span class="timeline-delegation__agent">Delegated to {segment.group.agent_id}</span>
														<span class="timeline-delegation__meta">
															{segment.entries.length} {segment.entries.length === 1 ? 'step' : 'steps'} · {segment.group.status}{#if span.summary} · {span.summary}{/if}
														</span>
													</summary>
													<ol class="run-timeline run-timeline--delegated">
														{#each segment.entries as entry (entry.id)}
															{@render timelineRow(entry)}
														{/each}
													</ol>
													<div class="timeline-delegation__footer" data-delegation-status={segment.group.status}>
														{#if segment.group.status === 'running'}
															<span class="timeline-delegation__footer-icon" aria-hidden="true">⟳</span>
															<span class="timeline-delegation__footer-text">Active delegation in progress...</span>
														{:else if segment.group.status === 'failed' || segment.group.status === 'cancelled'}
															<span class="timeline-delegation__footer-icon" aria-hidden="true">✕</span>
															<span class="timeline-delegation__footer-text">
																Delegation {segment.group.status}{#if span.endClock} at {span.endClock}{/if}{#if span.duration} ({span.duration}){/if}
															</span>
														{:else}
															<span class="timeline-delegation__footer-icon" aria-hidden="true">↳</span>
															<span class="timeline-delegation__footer-text">
																Handed back results to main agent{#if span.endClock} · {span.endClock}{/if}{#if span.duration} ({span.duration}){/if}
															</span>
														{/if}
													</div>
												</details>
											</li>
										{/if}
									{/each}
								{:else}
									{#each visibleTimeline?.entries ?? [] as entry (entry.id)}
										{@render timelineRow(entry)}
									{/each}
								{/if}
							</ol>
						{/if}
					{/if}
				{:else if id === 'output' && output !== null}
					<div class="output-scope-heading output-scope-heading--task">
						<div>
							<strong>Task deliverables</strong>
							<span>Stable outputs promoted for the task across its runs.</span>
						</div>
						<span class="output-scope-heading__badge">TASK</span>
					</div>
					{#if output.summary}
						<!--
							**The deliverable, for every task whose deliverable is prose.**
							L2 and first, above the files, because for those tasks the file
							list is the appendix and this is the thing. Rendered through
							`ChatMarkdown` rather than as text: headings, lists, tables and
							code are what a written report is made of, and a task id or an
							output path mentioned in it becomes a working link.

							`sessionId={null}`, exactly as the retired panel's Output card
							passed it — there is no chat session here, and with one the
							component would wire filesystem paths to a session-scoped
							open-file endpoint that could not answer for them.
						-->
						<div class="output-summary">
							<ChatMarkdown content={output.summary} sessionId={null} />
						</div>
					{/if}
					{#if output.taskFilesKnown === false}
						<p class="output-scope-empty">Task-level deliverables could not be loaded.</p>
					{:else if output.taskFilesPending === true && taskOutputFiles(output).length === 0}
						<p class="output-scope-empty">Task-level deliverables are still being synthesized.</p>
					{:else if taskOutputFiles(output).length === 0 && !output.summary}
						<p class="output-scope-empty">No task-level deliverables were recorded.</p>
					{/if}
					{#snippet outputFileRow(file: TaskPanelFile, index: number)}
						{@const scope = outputScope(file)}
						{@const previewKind = previewKindOf(file.mediaType, file.path)}
						<li class="output-file" data-output-scope={scope}>
								{#if file.kind === 'image' && file.url}
									<!--
										A thumbnail, because for an image the name is the one thing
										that says least about it. The browser fetches it; the panel
										still does not, which is the line the component holds — it
										renders what it was handed and asks for nothing. The link
										wraps the image so the same gesture works with or without
										the actions row below.
									-->
									<button
										type="button"
										class="output-file__thumb"
										on:click={() => void openAuthenticatedTaskOutput(file.url!)}
									>
										<img use:authenticatedTaskOutputImage={file.url} alt={file.name} loading="lazy" />
									</button>
								{/if}
								<span class="output-file__name">{file.name}</span>
								{#if fileMeta(file)}
									<!--
										Size and mime, which the old panel's file cards carried and
										the row list dropped. L2 detail about a row the reader is
										already looking at, so it sits inline rather than behind
										`Details`.
									-->
									<span class="output-file__meta">{fileMeta(file)}</span>
								{/if}
								{#if file.artifactId}
									<code class="output-file__artifact-id" title={file.artifactId}>{file.artifactId}</code>
								{/if}
								{#if file.url || outputActions || canPreview(file)}
									<!--
										One actions row per file, in the order the reader reaches for
										them: read it here, keep a copy, then act on it where it
										lives. Each control's accessible name carries the file —
										several rows of identically-named buttons is the L1 card
										grid's defect in miniature, heard rather than seen — and each
										visible label is a prefix of its accessible name, so voice
										control still reaches it by what is on screen.
									-->
									<span class="output-file__actions">
										{#if canPreview(file)}
											<!--
												**First, because reading it here is the point.** The
												three controls after it all take the reader somewhere
												else; this one is the only one that answers the
												question without leaving the panel.

												A disclosure, not a link: `aria-expanded` is what
												carries "there is more of this row" to a screen reader,
												and the visible word stays `Preview` in both states so
												it remains a prefix of the accessible name — a control
												that renamed itself to `Hide` would break the
												voice-control path the other three keep.
											-->
											<Button
												label="Preview"
												variant="outline"
												size="sm"
												className="output-file__action"
												ariaExpanded={openPreview === index}
												ariaControls={`output-preview-${index}`}
												ariaLabel={`Preview ${file.name}`}
												on:click={() => togglePreview(file, index)}
											/>
										{/if}
										{#if file.url}
											<!-- Authenticated fetches become short-lived blob URLs. -->
											<Button
												label="In tab"
												variant="outline"
												size="sm"
												className="output-file__action"
												ariaLabel={`In tab ${file.name}`}
												on:click={() => void openAuthenticatedTaskOutput(file.url!)}
											/>
											<Button
												label="Download"
												variant="outline"
												size="sm"
												className="output-file__action"
												ariaLabel={`Download ${file.name}`}
												on:click={() => void downloadAuthenticatedTaskOutput(file.url!, file.name)}
											/>
										{/if}
										{#if outputActions}
											<!--
												The design's `report.md   open · reveal` row: the two
												**OS** actions, which the panel cannot perform. They are
												dispatches rather than links for that reason, and they
												render only for a caller that said it can act.
											-->
											<Button
												label="Open"
												variant="outline"
												size="sm"
												className="output-file__action"
												ariaLabel={`Open ${file.name}`}
												on:click={() => dispatch('openFile', { file, index })}
											/>
											<Button
												label="Reveal"
												variant="outline"
												size="sm"
												className="output-file__action"
												ariaLabel={`Reveal ${file.name}`}
												on:click={() => dispatch('revealFile', { file, index })}
											/>
										{/if}
									</span>
								{/if}
								{#if openPreview === index && previewKind !== null}
									{@const preview = previewFor(index)}
									<!--
										**A caller that has not answered is loading**, which is one
										statement rather than two: `null` and `loading` are the same
										fact about the same row, and spelling them as separate
										branches would be two mechanisms saying "nothing yet" — one
										of which would eventually stop being reached and nothing
										would notice.
									-->
									{@const previewStatus = preview?.status ?? 'loading'}
									{@const previewText = preview?.text ?? ''}
									<div
										class="output-preview"
										id={`output-preview-${index}`}
										data-preview-kind={previewKind}
										data-preview-status={previewStatus}
									>
										{#if previewKind === 'image'}
											<!--
												**No request, and therefore no caller.** The browser
												loads this, exactly as it loads the row's thumbnail —
												which is why an image row offers a preview even to a
												caller that answers nothing. Sized to the drawer rather
												than to the file, and wrapped so the same gesture
												reaches the full-size image.
											-->
											<button
												type="button"
												class="output-preview__figure"
												aria-label={`Open ${file.name} at full size`}
												on:click={() => void openAuthenticatedTaskOutput(file.url!)}
											>
												<img use:authenticatedTaskOutputImage={file.url!} alt={file.name} loading="lazy" />
											</button>
										{:else if previewStatus === 'loading'}
											<p class="output-preview__note" role="status">Reading {file.name}…</p>
										{:else if previewStatus === 'failed'}
											<!--
												**It says so, and Open still works.** A failed read that
												rendered an empty block would be indistinguishable from
												a file that is genuinely empty — the panel asserting
												something about the task out of something that happened
												to the network, which is design §6's whole subject.
											-->
											<p class="output-preview__note" role="status">
												Couldn't read {file.name}{preview?.detail ? ` — ${preview.detail}` : ''}
											</p>
										{:else if previewStatus === 'too-large'}
											<!--
												The ceiling, stated when it bites, with the two controls
												that still work sitting on the row above it. Never a
												silent truncation: half a file read as if it were the
												whole one is the worst answer available here.
											-->
											<p class="output-preview__note">{preview?.detail}</p>
										{:else if previewText.trim() === ''}
											<p class="output-preview__note">This file is empty</p>
										{:else if previewKind === 'markdown'}
											<!--
												**Rendered, not raw** — through the same `ChatMarkdown`
												the report above uses, so a written output reads as the
												document it is rather than as its source. `sessionId`
												is `null` for the reason the report gives: there is no
												chat session here.
											-->
											<div class="output-preview__markdown">
												<ChatMarkdown content={previewText} sessionId={null} />
											</div>
										{:else if previewKind === 'csv'}
											{@const table = parseDelimited(
												previewText,
												delimiterOf(file.mediaType, file.path)
											)}
											<!--
												A table, not comma soup. The first row is drawn as the
												header, which is the convention every producer of these
												files follows and the only assumption this makes about
												them — a file without one loses nothing but the bold.
											-->
											<div class="output-preview__scroll">
												<table class="output-preview__table">
													<thead>
														<tr>
															{#each table.rows[0] ?? [] as cell}
																<th>{cell}</th>
															{/each}
														</tr>
													</thead>
													<tbody>
														{#each table.rows.slice(1) as row}
															<tr>
																{#each row as cell}
																	<td>{cell}</td>
																{/each}
															</tr>
														{/each}
													</tbody>
												</table>
											</div>
											{#if table.truncatedRows > 0}
												<p class="output-preview__note">
													{table.truncatedRows} more rows — open the file to read them all
												</p>
											{/if}
										{:else}
											<!--
												JSON indented; a source file and a log as they are.
												Monospace with the whitespace kept, which is the one
												thing a log cannot be read without.
											-->
											{@const body =
												previewKind === 'json' ? (prettyJson(previewText) ?? previewText) : previewText}
											<pre class="output-preview__text">{body}</pre>
										{/if}
									</div>
								{/if}
							</li>
					{/snippet}

					{#if taskOutputFiles(output).length > 0}
						<ul class="output-files output-files--task">
							{#each output.files as file, index}
								{#if outputScope(file) === 'task'}
									{@render outputFileRow(file, index)}
								{/if}
							{/each}
						</ul>
					{/if}

					{@const intermediatesCount = runOutputFiles(output).length + (output.artifacts?.length ?? 0)}
					{#if intermediatesCount > 0 || output.selectedExecutionId}
						<details class="output-intermediates">
							<summary class="output-intermediates__summary">
								<div class="output-intermediates__title-group">
									<span class="output-intermediates__title">Intermediate artifacts & evidence</span>
									<span class="output-intermediates__count">({intermediatesCount} {intermediatesCount === 1 ? 'item' : 'items'})</span>
								</div>
								<span class="output-intermediates__hint">Outputs and evidence belonging only to execution runs</span>
							</summary>
							<div class="output-intermediates__content">
								<ul class="output-files output-files--run">
									{#if output.selectedExecutionId}
										<li class="output-scope-heading output-scope-heading--run">
											<div>
												<strong>Selected run</strong>
												<span>Outputs and evidence belonging only to this execution.</span>
											</div>
											<span
												class="output-scope-heading__badge"
												title={output.selectedExecutionId}
											>
												{selectedRunBadge(output.selectedExecutionId, runs)}
											</span>
										</li>
									{/if}
									{#each output.files as file, index}
										{@const scope = outputScope(file)}
										{#if scope !== 'task'}
											{#if beginsRunOutputScope(output.files, index)}
												<li class="output-group-heading">
													<strong>{outputScopeTitle(scope)}</strong>
													<span>{outputScopeDescription(scope)}</span>
												</li>
											{/if}
											{@render outputFileRow(file, index)}
										{/if}
									{/each}
									{#if (output.artifacts?.length ?? 0) > 0}
										{#if !output.files.some((file) => outputScope(file) === 'artifact')}
											<li class="output-group-heading">
												<strong>Persisted artifacts</strong>
												<span>Structured execution evidence with no file to open.</span>
											</li>
										{/if}
										{#each output.artifacts ?? [] as artifact (artifact.id)}
											<li class="output-file output-artifact" data-output-scope="artifact">
												<span class="output-file__name">{artifact.name}</span>
												<span class="output-file__meta">
													{[artifact.artifactType, artifact.contentType, artifact.producedAt]
														.filter(Boolean)
														.join(' · ')}
												</span>
												<code class="output-file__artifact-id" title={artifact.id}>{artifact.id}</code>
											</li>
										{/each}
									{:else if output.selectedExecutionId && runOutputFiles(output).length === 0 && output.runArtifactsKnown !== false}
										<li class="output-scope-empty">No run-level outputs or persisted artifacts were recorded.</li>
									{/if}
									{#if output.selectedExecutionId && output.runArtifactsKnown === false}
										<li class="output-scope-empty">Persisted artifacts for this run could not be loaded.</li>
									{/if}
								</ul>
							</div>
						</details>
					{/if}
				{/if}
			</TaskActSection>
		{/each}
	</div>
{/if}

<style>
	.panel {
		/**
		 * The disclosure column: the width of the gutter the verdict's state
		 * glyph and every act's `▸` sit in.
		 *
		 * **One home, here, because this is the only element both readers are
		 * inside.** The number was written out three times across two files —
		 * twice as a marker width and once inside the act body's indent — and the
		 * verdict's marker still ended up 16px right of every act's, because the
		 * thing that broke the shared left edge was not the width at all but a
		 * horizontal padding one of the two carried. Three copies of a number
		 * that has to agree is what that failure looks like from the inside: each
		 * copy was correct, nothing pointed at anything else, and no edit to one
		 * of them could have been wrong.
		 *
		 * Deliberately without a fallback in either reader. A `var()` with a
		 * fallback is the same number written three times again, one of them
		 * hidden; without one, this element is the only place the column has a
		 * width, and `taskPanelPresentation.test.ts` can say so by counting.
		 */
		--task-panel-disclosure: 0.75rem;

		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	/* Above the verdict, quieter than it: the state below is still the story, and
	   this says how much to trust it.

	   **A pill, not a band**, which is why this one takes the inset as a margin
	   rather than as padding: it is a notice about the panel rather than a section
	   of it, and a full-width wash here would compete with the verdict's own band
	   directly below. The two treatments are the distinction — see the note where
	   `--task-panel-bleed` is declared. */
	.panel__stale {
		margin: 0 var(--task-panel-bleed, 0px) var(--space-sm);
		padding: var(--space-xs) var(--space-sm);
		border-radius: var(--radius-sm);
		background: var(--status-paused-soft, transparent);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
	}

	/* The run picker: chrome for the act below it, ranked below the act's own
	   header so it reads as a setting on the section rather than as a section.

	   **The horizontal inset is `--task-panel-bleed`**, the same one the verdict,
	   the act headers and the stale pill take, so the label starts on the panel's
	   one left edge. It is padding rather than margin because there is no wash to
	   inset — see the note where the token is declared.

	   `flex-wrap` with a shrinkable basis on the control, so a long option in a
	   narrow drawer reflows instead of pushing the count off the right edge. The
	   `min-width: 0` is what makes that reachable: without it the flex item floors
	   at the widest option's intrinsic width and the wrap rule can never fire,
	   which is the defect the presentation pass measured once already. */
	.run-picker {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-xs) var(--space-sm);
		padding: 0 var(--task-panel-bleed, 0px) var(--space-sm);
		min-width: 0;
	}

	.run-picker__control {
		flex: 1 1 10rem;
		min-width: 0;
	}

	/* Both words are annotations on the control between them, so they take the
	   same quiet step of the scale the stale pill and the provenance rows do — and
	   the same colour token, which is the only one the panel's quiet text uses in
	   any theme. **They are ranked by weight rather than by a second colour**: the
	   label says what the control does and the count is a fact about the list, and
	   reaching for a third text token to say so would be a contrast the theme
	   sweep has not measured. */
	.run-picker__label {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--text-secondary);
	}

	.run-picker__count {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
	}

	/* The one branch that is a block rather than a column of sections: no section
	   means nothing to run edge to edge, so it simply insets its own content. The
	   horizontal inset is the shared one so the failure lines up with where the
	   verdict would have been. */
	.panel--unloadable {
		padding: var(--space-md) var(--task-panel-bleed, 0px);
	}

	/* This branch's L0. It answers the same question in the same place as the
	   verdict headline — it is only a different answer — so it takes the same
	   step of the scale, and the presentation test holds the two together rather
	   than letting the unloadable panel quietly flatten while the loaded one
	   stays ranked. */
	.panel__load-error {
		margin: 0;
		font-family: var(--font-display);
		font-size: 1.125rem;
		font-weight: 700;
		color: var(--text-primary);
	}

	.panel__load-error-detail {
		margin: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	/* Only its place in the column. Everything that made this look and behave like
	   a control is `native/Button.svelte`'s, which is what it now is — including
	   the hover it shipped without. `:global` because `className` puts this class
	   on an element inside the component, which carries the component's scope. */
	.panel--unloadable :global(.panel__retry) {
		margin-top: var(--space-sm);
	}

	.plan-asks,
	.plan-steps,
	.run-steps,
	.output-files {
		margin: 0;
		padding: 0;
		list-style: none;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
	}

	.plan-ask {
		color: var(--text-primary);
	}

	/* ── the ask, and the thing that answers it ───────────────────────────
	   Sits at the top of its act with a rule under it, so the reader's eye
	   separates "the thing you have to do" from the record below it without a
	   card, a wash or a border box — the three gestures this panel replaced.
	   No colour of its own: the verdict line above already carries the
	   attention band, and a second one in the body would be the same alarm
	   raised twice. */
	.ask {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		padding-bottom: var(--space-md);
		margin-bottom: var(--space-md);
		border-bottom: 1px solid var(--border-soft);
	}

	/* The ask in the backend's own words. One step up from the body text
	   around it, because within this act it is the sentence everything else is
	   subordinate to — and it is the only place the full prompt appears: the
	   verdict's detail line is the same string and clips. */
	.ask__prompt {
		margin: 0;
		font-family: var(--font-primary);
		font-size: 0.875rem;
		font-weight: 500;
		line-height: 1.45;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.ask__actions {
		display: flex;
		justify-content: flex-end;
	}

	/* **The one control in this panel that changes something rather than revealing
	   it, so it is the one that takes `native/Button.svelte`'s `primary` variant**
	   — every other control here is `outline`. The nineteen declarations this
	   replaces were an approximation of that variant built out of an accent-mixed
	   border, and they carried none of the component's per-theme treatments.

	   No rule of its own: `.ask__actions` places it. */

	/* The one place in this panel where prose takes a status colour, and it
	   earns it: nothing else on screen changed when the post failed — the
	   verdict still reads `Waiting on you` — so this line is the only signal
	   that the answer did not land. Pulled toward `--text-primary` for the
	   contrast floor, the same mix the verdict's marker uses. */
	.ask__error {
		margin: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		line-height: 1.45;
		color: color-mix(in srgb, var(--status-failed) 60%, var(--text-primary));
		overflow-wrap: anywhere;
	}

	.plan-steps {
		margin-top: var(--space-sm);
		counter-reset: plan-step;
	}

	.plan-step,
	.output-file {
		color: var(--text-secondary);
	}

	/* The task's own report, above the file list. It borrows the act body's type
	   rather than setting its own, so a heading inside the markdown steps against
	   this panel's scale instead of the chat bubble's — `ChatMarkdown` already
	   inherits font and colour, which is why it can be dropped into a row-shaped
	   column without bringing a bubble with it. */
	.output-summary {
		margin-bottom: var(--space-sm);
		color: var(--text-primary);
		line-height: 1.5;
	}

	.output-scope-heading {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--space-sm);
		padding: var(--space-sm) 0;
		border-bottom: 1px solid var(--border-soft);
		color: var(--text-primary);
	}

	.output-scope-heading--task {
		margin-bottom: var(--space-sm);
	}

	.output-scope-heading--run {
		margin-top: var(--space-sm);
		padding-top: var(--space-md);
		border-top: 1px solid var(--border-medium, var(--border-soft));
	}

	.output-scope-heading > div,
	.output-group-heading {
		display: flex;
		flex-direction: column;
		gap: 0.125rem;
		min-width: 0;
	}

	.output-scope-heading strong,
	.output-group-heading strong {
		font-size: 0.8125rem;
		font-weight: 700;
		color: var(--text-primary);
	}

	.output-scope-heading span,
	.output-group-heading span,
	.output-scope-empty {
		font-size: 0.75rem;
		line-height: 1.4;
		color: var(--text-secondary);
	}

	.output-scope-heading__badge {
		flex: none;
		padding: 0.15rem 0.4rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-xs, 0.25rem);
		background: var(--bg-soft);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.output-group-heading {
		padding: var(--space-xs) var(--space-xs) 0;
	}

	.output-scope-empty {
		margin: 0 0 var(--space-sm);
	}

	li.output-scope-empty {
		padding: var(--space-sm);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm);
	}

	/* One output per row, each row bounded and separated from the next.

	   **Unlike the timeline, every row here gets its container**, and the
	   difference is what the two lists are. A feed is two hundred homogeneous
	   events read as a stream, where a frame per row is noise; an output list is
	   two or three *deliverables*, each with a name, a size, a mime, up to five
	   controls and possibly an opened preview — a row here is a block of five
	   things, and three of those blocks flat against each other is what made
	   multiple outputs read as one undifferentiated list. */
	.output-files {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}

	/* Collapsible container grouping intermediate execution artifacts and evidence */
	.output-intermediates {
		margin-top: var(--space-md);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: var(--bg-soft);
	}

	.output-intermediates__summary {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--space-xs);
		cursor: pointer;
		padding: var(--space-sm) var(--space-md);
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-secondary);
		background: var(--bg-soft);
		user-select: none;
	}

	.output-intermediates__summary:hover {
		background: color-mix(in srgb, var(--bg-soft) 80%, var(--bg-card, var(--bg-elevated)));
	}

	.output-intermediates[open] .output-intermediates__summary {
		border-bottom: 1px solid var(--border-soft);
	}

	.output-intermediates__title-group {
		display: flex;
		align-items: baseline;
		gap: var(--space-xs);
		min-width: 0;
	}

	.output-intermediates__title {
		font-weight: 600;
		color: var(--text-primary);
	}

	.output-intermediates__count {
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}

	.output-intermediates__hint {
		min-width: 0;
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.output-intermediates__content {
		padding: var(--space-sm);
	}

	/* The same row shape as a run step: the name reads first, the controls sit
	   at the end of the line where the design's mock puts them. It wraps because
	   four controls, a size and a mime do not fit beside a long filename in a
	   560px drawer, and a row that overflows is worse than one that takes two
	   lines.

	   The border is `--border-soft` and the fill is the surface's own — a bounded
	   row rather than a raised card. `--bg-soft` here would put the row's own
	   preview block, which is `--bg-soft`, on a wash of the same colour. */
	.output-file {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-sm);
		padding: var(--space-sm);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
	}

	.output-file__name {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	/* A thumbnail, sized to the row rather than to the image: the list is still
	   a list, and a grid of large previews is the card wall this panel replaced.
	   `align-self: center` because the row's baseline belongs to its text. */
	.output-file__thumb {
		flex: none;
		align-self: center;
		display: inline-flex;
		padding: 0;
		border: 0;
		background: transparent;
		cursor: pointer;
		border-radius: var(--radius-sm);
		overflow: hidden;
	}

	.output-file__thumb img {
		display: block;
		width: 2.5rem;
		height: 2.5rem;
		object-fit: cover;
		background: var(--bg-soft);
	}

	/* L2 detail about the row it sits on, one step quieter than the name. Reads
	   `12 KB · text/markdown`, or one of the two when only one was recorded.

	   **Shrinkable rather than `flex: none`, which is what it shipped as.** A mime
	   is not a short string — `application/vnd.openxmlformats-officedocument…` is
	   65 characters — and a non-shrinking item at that width overflows the row it
	   is meant to annotate. Shrink plus the floor override plus the wrap rule is
	   the same three-part fix `.act__provenance dd` needed. */
	.output-file__meta {
		min-width: 0;
		overflow-wrap: anywhere;
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}

	.output-file__artifact-id {
		min-width: 0;
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	/* Wrapping, because five controls, a size and a mime do not fit beside a long
	   filename in a 560px drawer — and a wrapped set of controls inside a bounded
	   row still reads as belonging to it, which is what the row's border buys.

	   **`flex: none` here defeated that wrap.** A non-shrinking flex item sizes to
	   `max-content`, which for a wrapping container is every child on one line — so
	   the block sized to all five controls side by side and overflowed the row
	   instead of folding inside it. Growing with the floor override is what lets
	   the inner wrap fire; `justify-content: flex-end` keeps the controls at the
	   trailing edge that `margin-left: auto` used to buy. */
	.output-file__actions {
		flex: 1 1 auto;
		min-width: 0;
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: var(--space-xs);
	}

	/* **All five are now `native/Button.svelte`'s outline variant at `sm`, and two
	   of the five are still `<a>` elements.**

	   The previous round replaced twenty-three declarations approximating that
	   variant, declaration by declaration, with a comment on each saying which one
	   it was copying. The component is the thing those comments describe, so the
	   panel now renders the component — which is also what gets these controls the
	   per-theme treatments they never had (`retro-16bit` squares every button in
	   the app and reached none of these).

	   Preview, Open and Reveal are `<button>`; In tab and Download are `<a>`,
	   because opening a tab and saving a file are navigations the browser performs
	   and a `<button>` loses middle-click, `Copy link address` and the download
	   attribute. Both come out of the same component — see its `href` prop, which
	   exists so that a navigation does not have to be restyled by hand to sit in a
	   row of controls. No CSS is left here at all.

	   The one thing lost against the hand-rolled version: these were `0.75rem`
	   deliberately, one step below the row's name. `sm` is also `0.75rem`, so
	   nothing moved. */

	/* ── an output, read in place ────────────────────────────────────────
	   The row's own disclosure. `flex-basis: 100%` drops it onto its own line
	   under the row that opened it without a wrapper element per file, exactly
	   as the timeline row's body does one act up. Bounded and scrolling in its
	   own right for the same reason the feed is: a preview that pushed the next
	   file below the fold would make the list unreadable to open one item of
	   it.

	   `box-sizing: border-box` because of the `padding` below: on a `flex-basis:
	   100%` box the padding would otherwise be added *outside* a box already as
	   wide as its container, so an opened preview overflowed the panel by
	   `--space-sm` on each side. Same mechanism as `.run-step__who` above. */
	.output-preview {
		box-sizing: border-box;
		flex: 0 0 100%;
		margin-top: var(--space-xs);
		min-width: 0;
		max-height: 20rem;
		overflow: auto;
		overscroll-behavior: contain;
		padding: var(--space-sm);
		border-radius: var(--radius-sm);
		background: var(--bg-soft);
		color: var(--text-primary);
	}

	/* What the preview says when it is not the file: reading it, refusing it,
	   or reporting that it could not. One class for all three — they are the
	   same kind of line, and the row's controls are what differ. */
	.output-preview__note {
		margin: 0;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	/* Sized to the drawer, not to the file: an output image is usually a chart,
	   and a chart is unreadable at the row thumbnail's 40px and does not need
	   more than the column's width. */
	.output-preview__figure {
		display: block;
		padding: 0;
		border: 0;
		background: transparent;
		cursor: pointer;
	}

	.output-preview__figure img {
		display: block;
		max-width: 100%;
		max-height: 18rem;
		object-fit: contain;
		border-radius: var(--radius-sm);
	}

	/* The markdown borrows the act body's type rather than setting its own, on
	   the same rule the report above it follows — a heading inside a previewed
	   document steps against this panel's scale, not the chat bubble's. */
	.output-preview__markdown {
		font-size: 0.8125rem;
		line-height: 1.5;
	}

	/* Source, JSON and logs. Scrolls sideways rather than wrapping mid-token,
	   so a wide log does not force the drawer itself to scroll horizontally —
	   the same treatment a shell row's stdout gets in the Run act. */
	.output-preview__text {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		line-height: 1.45;
		color: var(--text-primary);
		white-space: pre;
		overflow-x: auto;
	}

	/* The table's own horizontal scroll, so a forty-column CSV scrolls inside
	   the preview instead of widening the drawer. */
	.output-preview__scroll {
		overflow-x: auto;
	}

	.output-preview__table {
		border-collapse: collapse;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		/* Numbers in a table are read down a column, and a proportional zero is
		   what stops that working. */
		font-variant-numeric: tabular-nums;
	}

	.output-preview__table th,
	.output-preview__table td {
		padding: 0.125rem var(--space-sm) 0.125rem 0;
		text-align: left;
		vertical-align: top;
		white-space: nowrap;
		color: var(--text-primary);
	}

	/* The header row, distinguished by weight and a rule rather than by a fill:
	   a background band would need its own contrast pass in each of the themes
	   in `app.css`, and weight survives greyscale for free. */
	.output-preview__table th {
		font-weight: 600;
		border-bottom: 1px solid var(--border-soft);
		color: var(--text-secondary);
	}

	/* Wrapping, so the `via <agent>` line can take a row of its own under the
	   label without a wrapper element per step — the same `flex-basis: 100%`
	   trick the timeline rows use for their cost line. */
	.run-step {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0 var(--space-sm);
		color: var(--text-secondary);
	}

	/* Work this run handed to another agent, indented one level. The indent says
	   "under this run" and nothing narrower: no payload here names which *step*
	   delegated a child, so these rows sit under the run they belong to and the
	   rule down their left edge is what separates them from the step above rather
	   than joining them to it. */
	.run-step[data-step-origin='delegated'] {
		margin-left: calc(var(--task-panel-disclosure) + var(--space-sm));
		padding-left: var(--space-sm);
		border-left: 1px solid var(--border-soft);
	}

	/* The step's own gutter, and the fourth reader of the disclosure column:
	   every row's mark lands on the same axis as the act markers above it and
	   the verdict's above those, so the panel reads as one column of state
	   rather than three lists that happen to be stacked. */
	.run-step__marker {
		flex: none;
		width: var(--task-panel-disclosure);
		font-size: 0.75rem;
		text-align: center;
		color: var(--text-secondary);
	}

	/* Two tones, from the same tokens the verdict's bands use, pulled toward the
	   text colour for the reason `.verdict__marker` gives: a solid status colour
	   at glyph size falls under the 3:1 floor in roughly half this app's themes.
	   `idle` takes no rule at all — it is the inherited body colour above, and a
	   list in which every row is coloured has no signal left in it. */
	.run-step[data-step-tone='failed'] .run-step__marker {
		color: color-mix(in srgb, var(--status-failed) 60%, var(--text-primary));
	}

	.run-step[data-step-tone='running'] .run-step__marker {
		color: color-mix(in srgb, var(--status-running) 60%, var(--text-primary));
	}

	/* Work that has stopped until something acts on it. The attention colour at
	   the same ratio `.run-step__retries` uses, and for the same reason: neat, it
	   is a marker in an accent that clears 3:1 in only a handful of the themes in
	   `app.css`. */
	.run-step[data-step-tone='waiting'] .run-step__marker {
		color: color-mix(in srgb, var(--status-attention) 60%, var(--text-primary));
	}

	/* Where "watching" lives: the live step is the hero and the completed ones
	   recede, so a glance finds the present without reading the list. */
	.run-step--current {
		color: var(--text-primary);
		font-weight: 600;
	}

	.run-step__label {
		min-width: 0;
		overflow-wrap: anywhere;
	}

	/* The attention colour, pulled toward the text colour rather than used neat —
	   `Badge`'s own move for the row whose status colour is too light to read.
	   Neat, `--status-attention` is body text in an accent, which clears 4.5:1
	   in only a handful of the themes in `app.css`; at this ratio the count still
	   reads as flagged and clears the floor in all of them. Losing the colour
	   entirely was the other option and it costs the signal: this sits inline
	   with a step label that is already `--text-secondary`. */
	.run-step__retries {
		flex: none;
		color: color-mix(in srgb, var(--status-attention) 50%, var(--text-primary));
	}

	.run-step__duration {
		flex: none;
		margin-left: auto;
		font-variant-numeric: tabular-nums;
		color: var(--text-secondary);
	}

	/* The capability and the agent, one step quieter than the label and hanging
	   off the same content edge the marker gutter sets — so the labels keep one
	   left edge and their attribution keeps another.

	   **`padding-left`, never `margin-left`** — the same fix the timeline's four
	   sub-lines needed, and this line is why it is now a sweep rather than four
	   selectors. `flex-basis: 100%` already sizes the box to the container's whole
	   content width, so a left margin adds to that total and pushes the box out by
	   exactly the indent; padding under `border-box` indents inside the same 100%.
	   The two spellings read identically in a stylesheet and differ by the width of
	   the overflow. */
	.run-step__who {
		box-sizing: border-box;
		flex: 0 0 100%;
		padding-left: calc(var(--task-panel-disclosure) + var(--space-sm));
		min-width: 0;
		font-size: 0.75rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	/* A delegated row already carries the gutter's worth of indent on the row
	   itself, so its attribution line hangs off the row's own content edge rather
	   than indenting a second time. */
	.run-step[data-step-origin='delegated'] .run-step__who {
		margin-left: 0;
	}

	/* The same treatment as `.run-step__retries`, and for the same reason: it is
	   an inline flag on a row whose label is already secondary, so the accent is
	   pulled toward the text colour rather than used neat — `--status-attention`
	   clears 4.5:1 in only a handful of the themes in `app.css`.

	   `flex: none` so it keeps its words on one line and the duration keeps its
	   `margin-left: auto` push to the right edge. */
	.run-step__blocking {
		flex: none;
		color: color-mix(in srgb, var(--status-attention) 50%, var(--text-primary));
	}

	/* ── who holds the run ───────────────────────────────────────────────
	   Three rows at most, below the step list and above the timeline, separated
	   from both by a rule rather than by a heading — a heading inside an act body
	   would be a level the ladder does not have, which is the same call
	   `.run-timeline` makes.

	   **Grid with `minmax(0, 1fr)`, not flex**, on the evidence in
	   `TaskActSection`'s `.act__provenance-row`: a two-column flex row containing
	   a long unbroken token collapsed to a 0px-wide, 110-line column in a real
	   browser. An agent id is exactly that shape of token. The track floors
	   itself, so the value column cannot be squeezed by a long sibling. */
	.run-responsibility {
		margin: var(--space-md) 0 0;
		padding: var(--space-sm) 0 0;
		border-top: 1px solid var(--border-soft);
		font-size: 0.8125rem;
	}

	.run-responsibility__row {
		display: grid;
		grid-template-columns: 7rem minmax(0, 1fr);
		gap: var(--space-sm);
	}

	.run-responsibility dt {
		min-width: 7rem;
		color: var(--text-secondary);
	}

	/* Not `--font-mono`: this is not provenance. An agent id here is the row's
	   own words about who did the work, at the same weight the step list's
	   attribution line gives it. */
	.run-responsibility dd {
		margin: 0;
		min-width: 0;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.recipe-narrative {
		margin: var(--space-sm) 0 var(--space-md);
		padding: var(--space-sm) var(--space-md);
		border-inline-start: 3px solid var(--status-running);
		background: var(--surface-raised);
	}

	.recipe-narrative p + p {
		margin-top: var(--space-xs);
	}

	.recipe-narrative p {
		margin: 0;
		font-size: 0.8125rem;
		line-height: 1.45;
		color: var(--text-secondary);
	}

	/* ── the timeline ────────────────────────────────────────────────────
	   The Run act's second list. It reads as a continuation of the steps above
	   it — same left edge, same gutter, same marks — separated by a rule rather
	   than by a heading, because a heading inside an act body would be a level
	   the ladder does not have. */
	.run-timeline__header {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-xs);
		margin: var(--space-md) 0 0;
		padding: 0 0 var(--space-2xs);
	}

	.run-timeline__title {
		font-family: var(--font-primary);
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--text-secondary);
		letter-spacing: 0.01em;
	}

	.run-timeline__modes {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
	}

	:global(.run-timeline__mode-btn) {
		font-size: 0.6875rem !important;
		padding: 0.15rem 0.45rem !important;
		min-height: auto !important;
		height: auto !important;
		line-height: 1.2 !important;
	}

	:global(.run-timeline__mode-btn.is-active) {
		font-weight: 600;
	}

	.run-timeline {
		/**
		 * The width of the leading `when` column — the wall clock and the offset
		 * beside it.
		 *
		 * **One home, on the element both readers are inside**, for the reason
		 * `--task-panel-disclosure` has one: the column's width is declared by the
		 * cluster and read again by the indent every sub-line hangs off, and those
		 * two numbers have to agree.
		 *
		 * Sized for one of the two, not both side by side, because they are now stacked.
		 * `00:00:00` at this size and tabular figures is the wider of the two;
		 * `+1h 20m` is narrower and shares its right edge.
		 */
		--timeline-when: 3.5rem;

		margin: var(--space-2xs) 0 0;
		padding: var(--space-sm) 0 0;
		border-top: 1px solid var(--border-soft);
		list-style: none;
		font-family: var(--font-primary);
		font-size: 0.8125rem;

		max-height: 22rem;
		overflow-x: hidden;
		overflow-y: auto;
		overscroll-behavior: contain;
	}

	/* A delegated child, folded into a distinct branch envelope inside the parent's feed.
	   Open by default: the events were invisible before this existed, and a
	   reader opening the panel to see what the delegate did should not have to
	   find a disclosure first. */
	.timeline-delegation {
		list-style: none;
		margin: var(--space-xs) 0;
		border: 1px solid var(--border-soft);
		border-left: 3px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-card, var(--bg-elevated));
		overflow: hidden;
	}

	.timeline-delegation[data-delegation-status='running'] {
		border-left-color: var(--status-running, #3b82f6);
	}

	.timeline-delegation[data-delegation-status='failed'],
	.timeline-delegation[data-delegation-status='cancelled'] {
		border-left-color: var(--status-failed, #ef4444);
	}

	.timeline-delegation[data-delegation-status='completed'],
	.timeline-delegation[data-delegation-status='done'] {
		border-left-color: var(--status-done, #10b981);
	}

	.timeline-delegation__summary {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-xs);
		cursor: pointer;
		padding: var(--space-xs) var(--space-sm);
		font-family: var(--font-primary);
		font-size: var(--text-xs);
		color: var(--text-secondary);
		background: var(--bg-soft);
		border-bottom: 1px solid var(--border-soft);
		user-select: none;
	}

	.timeline-delegation__summary:hover {
		background: color-mix(in srgb, var(--bg-soft) 80%, var(--bg-card, var(--bg-elevated)));
	}

	.timeline-delegation:not([open]) .timeline-delegation__summary {
		border-bottom: none;
	}

	.timeline-delegation__agent {
		font-weight: 600;
		color: var(--text-primary);
	}

	/* The rollup — step count and the child's own outcome. Never the parent's:
	   a delegation that failed inside a run still going is exactly the state a
	   reader is looking for. */
	.timeline-delegation__meta {
		font-variant-numeric: tabular-nums;
	}

	.timeline-delegation[data-delegation-status='failed'] .timeline-delegation__meta,
	.timeline-delegation[data-delegation-status='cancelled'] .timeline-delegation__meta {
		color: var(--status-error, var(--text-secondary));
	}

	/* The nested feed scrolls with the parent rather than in its own box */
	.run-timeline--delegated {
		max-height: none;
		overflow: visible;
		margin: 0;
		padding: var(--space-2xs) var(--space-sm);
		border-left: none;
		border-top: none;
		background: transparent;
	}

	/* Return / handback bar at the bottom of the branch envelope */
	.timeline-delegation__footer {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		padding: var(--space-2xs) var(--space-sm);
		font-size: 0.6875rem;
		font-family: var(--font-primary);
		color: var(--text-secondary);
		background: color-mix(in srgb, var(--bg-soft) 50%, var(--bg-card, var(--bg-elevated)));
		border-top: 1px dashed var(--border-soft);
		font-variant-numeric: tabular-nums;
	}

	.timeline-delegation__footer-icon {
		flex: none;
		font-size: 0.75rem;
		color: var(--text-secondary);
	}

	.timeline-delegation__footer-text {
		flex: 1 1 auto;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.timeline-delegation[data-delegation-status='failed'] .timeline-delegation__footer,
	.timeline-delegation[data-delegation-status='cancelled'] .timeline-delegation__footer {
		color: var(--status-failed, #ef4444);
	}

	.timeline-row__agent-tag {
		display: inline-block;
		margin-left: 0.35rem;
		padding: 0.05rem 0.3rem;
		font-size: 0.6875rem;
		font-weight: 500;
		color: var(--text-secondary);
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 4px);
		vertical-align: middle;
	}

	.run-timeline__empty {
		margin: var(--space-md) 0 0;
		padding: var(--space-sm) 0 0;
		border-top: 1px solid var(--border-soft);
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-secondary);
	}

	.run-timeline__window {
		margin: 0;
		padding: var(--space-xs) var(--space-sm);
		color: var(--text-secondary);
		font-size: var(--text-xs);
		font-weight: 600;
		letter-spacing: 0.01em;
	}

	/* A row, wrapping: the title, the cost line, a shell block and a capture
	   marker are four things of very different widths, and a 560px drawer fits
	   at most two of them on one line.

	   `gap: 0 var(--space-xs)` rather than `--space-sm`: the row now carries five
	   things on its first line — when, mark, kind, title, how long — and the
	   half-rem gutter it had between four of them spent 2rem of a ~30rem column
	   on air. The columns are what separate them now. */
	.timeline-row {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0 var(--space-xs);
		padding: 0.125rem 0;
		color: var(--text-secondary);
	}

	/* ── isolation, driven by what the row carries ────────────────────────
	   A row with prose or a stdout block is several lines tall, and flat against
	   its neighbours there is nothing saying where it ends. So it gets a bound —
	   a hairline above and below and a hair of space outside them — while a
	   single-line row gets none. See `timelineIsolated` for why the predicate is
	   content-driven in both directions: a feed of two hundred framed rows is
	   design §1's card grid rebuilt one level down.

	   **Bounded on the axis the rows run into each other, and not on the other
	   one.** No inset and no wash: the whole panel is one column of marks on one
	   axis, and a box that indented its own content would take this row's marker,
	   kind and title off the column every other row shares. The recessed `<pre>`
	   inside it is where the contrast comes from. */
	.timeline-row[data-timeline-isolated='true'] {
		margin: var(--space-xs) 0;
		padding: var(--space-xs) 0;
		border-top: 1px solid var(--border-soft);
		border-bottom: 1px solid var(--border-soft);
	}

	/* ── when it happened ─────────────────────────────────────────────────
	   The row's leading column, and the reason the feed can be read as a log: a
	   wall clock and an offset from the run's start, both tabular, both in the
	   quietest text this panel uses, on one axis down the whole feed. Fixed width
	   so a row whose instant the wire never recorded holds the column empty
	   rather than shunting its title left of its neighbours'.

	   **One column of two rows, not two columns.** Side by side these two spent
	   6.75rem of a ~508px content column, and the title — the one thing a reader
	   scans a feed for — had about thirty characters before it wrapped. Stacked, the
	   column is 3.5rem: the clock's own width, since the offset is narrower. The
	   cost is one line of height on a row that is already more than one line tall.

	   `text-align: right` on the container rather than on each half, so the two share
	   one right edge by construction — a reader running down the offsets to find
	   where the run slowed needs that edge, and it cannot drift if only one element
	   declares it. `line-height` tightened so the pair occupies about the height of
	   the title beside it instead of pushing every row taller. */
	.timeline-row__when {
		flex: none;
		display: inline-flex;
		flex-direction: column;
		width: var(--timeline-when);
		font-size: 0.6875rem;
		line-height: 1.3;
		font-variant-numeric: tabular-nums;
		text-align: right;
		color: var(--text-secondary);
	}

	/* Eight characters, always, so the colons line up down the feed. */
	.timeline-row__clock {
		flex: none;
	}

	/* `+4s` and `+1h 20m` differ by four characters, and a ragged edge is what
	   stops a reader running down the offsets to find where the run slowed — so
	   the two lines of the cluster share one edge (see `.timeline-row__when`).

	   `min-width: 0` is what makes the `overflow: hidden` beside it reachable at
	   all: this is a flex item, and a flex item's default `min-width: auto` is a
	   content-based floor, so an offset wider than the column would size the
	   column rather than be clipped by it. The same omission as
	   `.act__provenance dd`. */
	.timeline-row__offset {
		flex: 0 1 auto;
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		/* No `text-align` of its own: the column above declares it once so the two
		   halves cannot end up on different edges. */
	}

	.timeline-row__marker {
		flex: none;
		width: var(--task-panel-disclosure);
		font-size: 0.75rem;
		text-align: center;
		color: var(--text-secondary);
	}

	/* Two tones only, from the same tokens and at the same ratio the step rows
	   use — a solid status colour at glyph size falls under 3:1 in roughly half
	   this app's themes. `idle` takes no rule: it is the inherited body colour,
	   and a feed in which every row is coloured has no signal left in it. */
	.timeline-row[data-timeline-tone='failed'] .timeline-row__marker {
		color: color-mix(in srgb, var(--status-failed) 60%, var(--text-primary));
	}

	.timeline-row[data-timeline-tone='running'] .timeline-row__marker {
		color: color-mix(in srgb, var(--status-running) 60%, var(--text-primary));
	}

	/* The eyebrow: which kind of thing this was. Quiet and fixed-width so the
	   titles line up down the feed instead of stepping in and out with the
	   length of the word beside them.

	   **A micro-label rather than a word competing with the title.** It shipped at
	   the same colour and nearly the same size as the title beside it, which is
	   what made a row read as two equal halves — and the kind is the half the
	   reader is *not* scanning for: `Calling agent_browser` already says it is a
	   tool. Down a step in size and up in tracking, which is a treatment rather
	   than a colour, so it stays legible where a third grey would not. Narrowed
	   to 4rem, which the five short labels clear and `observation` ellipses in —
	   the feed's horizontal budget went to the two things that answer a question,
	   the title and the time. */
	.timeline-row__kind {
		flex: none;
		width: 4rem;
		/* **Without this the `4rem` above is a suggestion and the ellipsis below
		   never fires.** `min-width: auto` on a flex item is a content-based floor
		   that outranks a declared width, and `observation` at this size and
		   tracking is wider than 4rem — so the column this comment claims the label
		   ellipses into was in fact being widened by it, pushing every title on that
		   row right. The same omission as `.act__provenance dd`. */
		min-width: 0;
		font-size: 0.625rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--text-secondary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* **What the reader is scanning for, and the one thing in the row that carries
	   weight.** Everything else on the row is a fact *about* it — when, what kind,
	   how long, what it cost — and every one of them was rendering at this
	   element's weight, which is why the feed read as a wall. 500 rather than 600:
	   two hundred rows of semibold is a wall of its own, and one step is all a
	   step needs to be. */
	.timeline-row__title {
		min-width: 0;
		flex: 1 1 auto;
		font-weight: 500;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	/* A fixed-width track, so every row's bar starts at the same x and the eye can
	   run down them. Right-aligned and tabular for the number that sits on it —
	   these are figures a reader compares down a column, and a ragged left edge
	   would make the bars incomparable even though the numbers were not. */
	.timeline-row__latency {
		position: relative;
		flex: none;
		width: 4rem;
		/* The same step down the leading time column takes, and for the same
		   reason: this is a measurement about the row rather than the row itself,
		   and the two number columns either side of the title should read as one
		   tier. */
		font-size: 0.6875rem;
		text-align: right;
		font-variant-numeric: tabular-nums;
		color: var(--text-secondary);
	}

	/* Where the time went. A 2px rule under the number rather than a bar beside
	   it: the row is already four elements wide in a 560px drawer, and a feed of
	   two hundred rows cannot afford a line each. Quiet enough to read as an
	   annotation on the number rather than as a chart competing with the titles —
	   the answer is in the *relative* lengths, which survive at any opacity. */
	.timeline-row__bar {
		position: absolute;
		left: 0;
		bottom: -0.1rem;
		height: 2px;
		border-radius: 1px;
		background: color-mix(in srgb, var(--text-secondary) 45%, transparent);
	}

	/* Above the bar, so a full-width bar reads as an underline rather than a
	   strike. */
	.timeline-row__latency-value {
		position: relative;
	}

	/* Everything under the title line hangs off the row's own content edge — the
	   leading time column plus the marker gutter — so the feed keeps one left edge
	   for its content and another for its marks. `flex-basis: 100%` puts each on
	   its own line without a wrapper element per row.

	   The indent is written as the two columns it is the sum of, rather than as a
	   number that happens to equal them: the time column's width is declared once,
	   above, and changing it must move these with it.

	   **`padding-left`, never `margin-left`.** These were margins, and it put 85
	   elements past the panel's right edge — measured in a real browser, the worst
	   overflowing by 60px. `flex-basis: 100%` already sizes the box to the whole
	   container, so a left *margin* adds to that total and pushes the box out by
	   exactly the indent; padding under `border-box` indents the content inside
	   the same 100%. The two read identically in a stylesheet and differ by the
	   width of the overflow. */
	.timeline-row__body,
	.timeline-row__meta,
	.timeline-row__code,
	.timeline-row__capture {
		box-sizing: border-box;
		flex: 0 0 100%;
		padding-left: calc(var(--timeline-when) + var(--task-panel-disclosure) + var(--space-xs) * 2);
		min-width: 0;
	}

	.timeline-row__body {
		margin-top: 0.125rem;
		color: var(--text-secondary);
		line-height: 1.5;
	}

	/* The cost line: model, tokens, cache rate. **The smallest tier in the row**,
	   and deliberately below the body prose above it: this is L2 detail about a
	   row already in view, so it should be there when looked for and silent when
	   not. Tabular, because these are numbers a reader compares down a column.

	   It keeps `--text-secondary` rather than dropping to `--text-muted`, and that
	   is a constraint rather than a preference — `--text-muted` is under 3:1 on
	   `--bg-elevated` in this app's light themes, so the tier below secondary is
	   spelled in size and spacing, which every theme renders identically. */
	.timeline-row__meta {
		margin-top: 0.125rem;
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		/* The separator the joined string used to carry. `·` between two elements
		   would be a third element carrying no information, so the gap does it. */
		gap: 0 var(--space-xs);
		font-size: 0.6875rem;
		letter-spacing: 0.01em;
		color: var(--text-secondary);
	}

	/* **An identifier, set as one.** A provider's model id is a token to be matched
	   against a config file, not a word to be read — so it takes the mono face and a
	   hairline container, which is the house treatment for a value the reader might
	   copy (see `.act__provenance dd`, the same face for the same reason). It does
	   *not* take the recessed wash a stdout block takes: this is one token inline in
	   a line of figures, and a filled chip at 11px on a feed of two hundred rows
	   would out-shout the titles above it.

	   `min-width: 0` and the wrap rule together, because a model id is arbitrary
	   length and this is a flex item — the pair the whole A1 sweep was about. */
	.timeline-row__id {
		min-width: 0;
		padding: 0 0.1875rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		font-family: var(--font-mono);
		/* Mono faces run large at a shared nominal size; this keeps the id on the
		   same visual tier as the figures beside it rather than a step above them. */
		font-size: 0.9375em;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	/* Numbers a reader compares down a column, which is the whole reason they are
	   split from the identifier beside them: tabular figures on the figures only. */
	.timeline-row__figures {
		min-width: 0;
		font-variant-numeric: tabular-nums;
		overflow-wrap: anywhere;
	}

	/* ── a row's machine output, as a code container ──────────────────────
	   `<figure>` wrapping a `<pre><code>` and a caption row. The figure exists so
	   the block can carry a control: a `<button>` inside a `<pre>` sits in
	   whitespace that is significant, and the newline before it would be part of the
	   copied text.

	   No frame of its own — the `<pre>` inside carries the wash and the radius, and a
	   second border around it would be a box in a box. */
	.timeline-row__code {
		/* Only the top: the indent is the shared one above, and restating it here
		   would be the second copy of a number the timeline's whole layout hangs off. */
		margin-top: var(--space-xs);
		margin-bottom: 0;
	}

	/* The control row under the block. Right-aligned so it lands under the block's
	   trailing edge rather than beside its first line of output. */
	.timeline-row__code-actions {
		display: flex;
		justify-content: flex-end;
		margin-top: var(--space-xs);
	}

	/* Shell stdout, which is the one thing in this panel that must keep its own
	   whitespace — and the one block in it that is a *recessed container* rather
	   than a line: its own wash, its own radius, its own inset.

	   **It wraps, and it never scrolls sideways.** `white-space: pre-wrap` keeps
	   the newlines that make a log readable while letting a long line fold, and
	   `overflow-wrap: anywhere` folds a single unbroken token too — a 400-character
	   URL in a `<pre>` is what put a horizontal scrollbar across the whole drawer.
	   `overflow-x: hidden` is then not a second mechanism but the thing that stops
	   `overflow-y: auto` from *implying* `overflow-x: auto`: a box with one axis
	   `visible` and the other not resolves the visible one to `auto`, which is a
	   scrollbar on an axis with nothing to scroll. */
	.timeline-row__detail {
		margin: 0;
		padding: var(--space-xs) var(--space-sm);
		border-radius: var(--radius-sm);
		background: var(--bg-soft);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		line-height: 1.45;
		color: var(--text-primary);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		overflow-x: hidden;
		max-height: 12rem;
		overflow-y: auto;
	}

	/* The `<code>` inside carries the semantics and none of the type: browsers apply
	   a font-size reduction to a nested element whose family is monospace, so without
	   this the block would render a step smaller than the `<pre>` declares. */
	.timeline-row__detail code {
		font: inherit;
		color: inherit;
	}

	.timeline-row__capture {
		margin-top: 0.125rem;
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}
</style>
