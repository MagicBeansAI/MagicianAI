/**
 * The one line each act header carries when collapsed — level L1 of the
 * disclosure ladder. Pure, no component.
 *
 * The rule all three enforce: **a summary carries content, not a count.**
 * `Questions: 2` tells the reader a click is required; `2 questions waiting —
 * "Which quarter?"` sometimes means they do not need the click at all. Where no
 * content exists the count stays, as a deliberate fallback rather than a
 * default.
 *
 * See `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §2 and §4.
 */

import type { TaskPlanStatus } from '$lib/stores/taskStore';
import { formatRelativeTime } from '$lib/shared/formatRelativeTime';
import { durationIfKnown, stepPhrase } from './taskVerdict';

export interface PlanSummaryInput {
	/** `null` when the task has a plan act but no status the client recognises. */
	status: TaskPlanStatus | null;
	approvedAt: number | null;
	/** The asks blocking the plan, in the order the act body lists them. */
	questions: readonly string[];
	now: number;
}

export interface RunSummaryInput {
	/** Steps recorded so far. A list length (design §5), so never negative. */
	steps: number;
	retries: number;
	elapsedMs: number | null;
	/** Non-null while the run is live, which swaps the step count for the position. */
	currentStep: number | null;
	totalSteps: number | null;
	/**
	 * Timeline entries recorded. A list length like `steps`, and dropped at zero
	 * like every other segment here.
	 *
	 * **A count, and deliberately so.** The rule above prefers content, and this
	 * is the sanctioned fallback rather than a lapse from it: there is no
	 * one-line content for two hundred events, and `217 events` is the only thing
	 * a header can honestly say about a feed. It carries its own noun for the
	 * reason the whole slice exists — steps are plan structure, events are what
	 * happened, and a line reading `step 143 of 217` would conflate them.
	 *
	 * **Not `number | null`, and that was a real mistake caught by mutation.**
	 * `TaskPanelRun.timeline` *is* nullable — a run nobody read the events of is
	 * a different claim from one that recorded none — but that difference is
	 * observable in the act **body**, where one renders nothing and the other
	 * says so. Here both render no segment, because a count of zero drops out.
	 * A nullable parameter would have been a second mechanism for a distinction
	 * this line cannot express, and a test asserting it would have been asserting
	 * a guarantee that does not hold.
	 *
	 * Required rather than optional: a caller that forgot it would silently drop
	 * the one segment telling the reader there is a feed inside the act.
	 */
	events: number;
}

export type OutputKind = 'document' | 'image' | 'other';

export interface OutputFile {
	name: string;
	kind: OutputKind;
}

/**
 * What each plan status means, in the user's words. A `Record` over the union,
 * so a seventh backend status fails to compile here rather than rendering an
 * empty line. `planning` and `eliciting` are rewritten because neither word
 * tells a reader anything; the other four are ordinary English and keep theirs.
 */
const PLAN_STATUS_COPY: Record<TaskPlanStatus, string> = {
	planning: 'being planned',
	eliciting: 'gathering what it needs before planning',
	draft: 'draft, not yet approved',
	approved: 'approved',
	rejected: 'rejected, so it will not run',
	failed: 'planning failed'
};

/**
 * How each kind reads when counted. `other` is both the unclassifiable kind and
 * the answer when the counted files disagree, so a mixed remainder needs no
 * branch of its own.
 *
 * There is no `chart` kind: nothing producing these files can tell a chart from
 * a photograph, so `2 charts` is a claim the data cannot support while
 * `2 images` is true of both. A kind earns its place by changing what the line
 * claims, which is also why `data` is absent — `2 data files` tells the reader
 * no more than `2 other files` does. A `chart` kind can be added once a producer
 * can distinguish one, and this map makes that a compile error until its plural
 * is written.
 */
const KIND_PLURAL: Record<OutputKind, string> = {
	document: 'documents',
	image: 'images',
	other: 'other files'
};

/**
 * When the plan was approved, phrased to follow the word `approved`. `null`
 * when there is no readable timestamp — the line then says `approved` alone
 * rather than guessing, which is the answer `durationIfKnown` gives too.
 *
 * `formatRelativeTime` stays the only relative formatter, but two of the shapes
 * it returns do not compose with ` ago`: `now`, and the absolute date it falls
 * back to after a week. The test distinguishes them by shape rather than by
 * re-deriving that week threshold here, which would be the second copy this is
 * avoiding. Relative tiers are digits followed by a unit in every locale; the
 * date fallback is not, in any.
 */
function approvalTime(approvedAt: number | null, now: number): string | null {
	const relative = formatRelativeTime(approvedAt, now);
	if (relative === '') return null;
	if (relative === 'now') return 'just now';
	return /^\d+[mhd]$/.test(relative) ? `${relative} ago` : `on ${relative}`;
}

/** `null` rather than `0 steps`, so a segment with nothing to report drops out. */
function count(n: number, singular: string, plural: string): string | null {
	if (n <= 0) return null;
	return `${n} ${n === 1 ? singular : plural}`;
}

export function planSummary(input: PlanSummaryInput): string {
	// An unanswered question outranks the approval: it is the only thing in this
	// act asking the reader to do something, and the verdict line above has
	// already said `Waiting on you`, so leading with `approved` would contradict
	// it. The approval is still true underneath and readable once the act opens.
	if (input.questions.length > 0) {
		const noun = input.questions.length === 1 ? 'question' : 'questions';
		const waiting = `${input.questions.length} ${noun} waiting`;
		const asked = input.questions[0]?.trim();
		return asked ? `${waiting} — "${asked}"` : waiting;
	}

	if (input.status === 'approved') {
		const when = approvalTime(input.approvedAt, input.now);
		if (when) return `approved ${when}`;
	}

	// Empty is "render no summary", never a placeholder: with no status we know
	// the act exists and nothing else, and any line here would be a claim.
	return input.status === null ? '' : PLAN_STATUS_COPY[input.status];
}

export function runSummary(input: RunSummaryInput): string {
	// Live, the position replaces the step count — a running total is a number
	// the reader cannot use — but retries and elapsed still apply. A live line
	// carrying only `step 4 of 7` would repeat the verdict headline above it.
	const progress =
		input.currentStep === null
			? count(input.steps, 'step', 'steps')
			: stepPhrase(input.currentStep, input.totalSteps);

	const line = [
		progress,
		count(input.retries, 'retry', 'retries'),
		// After the steps and their retries, which are about the plan, and before
		// the duration, which is about the whole run.
		count(input.events, 'event', 'events'),
		durationIfKnown(input.elapsedMs)
	]
		.filter((segment): segment is string => !!segment)
		.join(' · ');

	return line || 'not started';
}

export function outputSummary(files: readonly OutputFile[]): string {
	if (files.length === 0) return 'no output';
	if (files.length === 1) return files[0].name;
	if (files.length === 2) return `${files[0].name} and ${files[1].name}`;

	// The named file is the first, so the header names whatever the act body
	// lists first; the ordering is the caller's decision, not this module's.
	const rest = files.slice(1);
	const kinds = new Set(rest.map((file) => file.kind));
	// Never singular: three or more files leave at least two in the remainder.
	const plural = kinds.size === 1 ? KIND_PLURAL[rest[0].kind] : KIND_PLURAL.other;
	return `${files[0].name} and ${rest.length} ${plural}`;
}
