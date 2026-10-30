/**
 * The Run act's timeline: what the run actually did, event by event.
 *
 * **Steps are plan structure; timeline entries are what happened.** They are
 * different things at different resolutions and the panel must not conflate
 * them — mapping activity events onto `TaskPanelRun.steps` would make the Run
 * act's header read `step 143 of 217` about a run with no plan. So this is a
 * second list on the Run act, with its own noun in the header (`217 events`)
 * and its own rows in the body.
 *
 * **Why this is a slice of the Run act rather than a fourth act.** The retired
 * `DeepWorkPanel`'s feed carried eight kinds, and three of them are things the
 * acts already own: an open clarification is the Plan act's, a plan step is the
 * Run act's step list, the terminal output block is the Output act's. What is
 * left once those go home — LLM calls, reasoning, tool calls, shell commands,
 * browser observations, run lifecycle rows — is emitted **by an execution**,
 * every one of them. The old feed spanned the lifecycle because it had no acts
 * and had to carry everything; the residue does not span anything. See
 * `docs/archive/plans/2026-07-29-unified-task-panel-design.md` §4.
 *
 * Pure, no component, and **the only place a timeline is built** — both adapters
 * call `deriveTimeline` and neither has a projection of its own. That is not
 * tidiness: a second builder would be a second answer to which events belong to
 * a run, and the two would disagree the first time either was edited, with the
 * disagreement invisible because each surface renders only its own.
 *
 * It owns the whole `[]`-versus-`null` partition too, for the same reason. `[]`
 * is a run whose events were read and numbered zero; `null` is a run nothing
 * observed, and the act renders no timeline at all. Deciding that in the callers
 * would put the rule in two places and the panel would eventually claim
 * `0 events` about a run it never read.
 */

import type { FeedItem, FeedItemStatus } from '$lib/feed/types';
import { contextPreparationRow } from '$lib/magician/components/contextPreparation';
import { cachedPercentOf, totalUsage, type TokenUsage, type UsageTotals } from '$lib/llm/tokenUsage';
import type {
	ExecutionPanelDelegationGroup,
	ExecutionPanelState
} from '$lib/types/executionPanel';
import type { RecipeLifecycleEvent } from '$lib/magician/api-mining/recipeNarrative';
import { answeredModelLabel, requestedModelLabel } from '$lib/magician/components/llmModelLabel';

import { durationIfKnown } from './taskVerdict';

/**
 * What kind of thing happened. Seven, and each one renders differently — a
 * shell command shows its stdout in a `<pre>`, an observation shows whether a
 * screenshot was captured, an LLM call shows what it cost. A kind that rendered
 * identically to another would be a distinction the reader cannot use.
 *
 * `event` is the honest fallback for an event type this client does not model,
 * and it is not a bucket for the unclassifiable: it renders the backend's own
 * humanized title, which is a real sentence. `llm` and `reasoning` are separate
 * because only one of them has a token bill.
 */
export type TimelineEntryKind =
	| 'llm'
	| 'reasoning'
	| 'tool'
	| 'shell'
	| 'observation'
	| 'lifecycle'
	| 'event';

/**
 * How the thing that happened ended, or where it is.
 *
 * Its own union rather than a reuse of `RunStepStatus` for the reason that one
 * gives about `VerdictState`: an event can be `info` — a run lifecycle row is
 * neither done nor running — and no step can be, while `skipped` is meaningless
 * for an event that either happened or did not.
 */
export type TimelineEntryStatus = 'running' | 'done' | 'failed' | 'waiting' | 'info';

/**
 * What one LLM call cost. Every field is separately nullable because the
 * provider decides which it reports: a call with no cache participation carries
 * no `cache_read_tokens` at all, and `0` would be a claim that the cache was
 * consulted and missed.
 *
 * **The same shape the run-level aggregate sums, not a second one.** It is
 * `TokenUsage` from `$lib/llm/tokenUsage` under the name this module's readers
 * already import — one shape, so a per-row cost line and a whole-run total
 * cannot come to disagree about what a token count is. The alias is kept rather
 * than replaced at every call site because the row's field is `tokens` and
 * `TimelineTokens` is what the rows are about.
 */
export type TimelineTokens = TokenUsage;

export interface TimelineEntry {
	/** Stable across polls, so a keyed `{#each}` keeps a row's identity. */
	id: string;
	/** When it happened, ms epoch. The sort key, and the only ordering there is. */
	at: number;
	kind: TimelineEntryKind;
	/**
	 * `null` when the record carried no status this client models — the row then
	 * renders **no marker**, on the same rule `TaskPanelRunStep.status` follows:
	 * "not recorded" and "not started" are different facts and a neutral glyph
	 * claims the second.
	 */
	status: TimelineEntryStatus | null;
	title: string;
	/** The event's own prose, rendered as markdown. `null` when it carried none. */
	body: string | null;
	/** Monospace output — a shell command's stdout, or an id. `null` when there is none. */
	detail: string | null;
	/** How long the call took. `null` when nothing timed it. */
	latencyMs: number | null;
	/** The model that answered, verbatim. `null` for anything that is not an LLM call. */
	model: string | null;
	/** Backend-recorded price in US dollars. Never recomputed from a client price table. */
	costUsd: number | null;
	/** `null` when the event reported no usage at all — never a zeroed record. */
	tokens: TimelineTokens | null;
	/** Whether a browser observation captured a screenshot. False for every other kind. */
	screenshot: boolean;
	/** Structured Task Recipe lifecycle payload, retained for task-level cues. */
	recipe?: RecipeLifecycleEvent | null;
	/**
	 * The execution that produced this row, which for a delegated child is not
	 * the run on screen. This is the grouping key: it is what
	 * `ExecutionPanelDelegationGroup.execution_id` matches. `null` when the
	 * source record carried no id.
	 */
	executionId: string | null;
	/**
	 * The agent that produced this row — the delegate for a child's rows, not
	 * the parent that delegated. `null` when unattributed.
	 */
	agentId: string | null;
	/**
	 * The approved-plan step the run was on when this row was recorded
	 * (`metadata.plan_step_id`). Absent on a run without an approved plan and
	 * on rows that frame the work rather than being part of it.
	 */
	planStepId?: string | null;
	/**
	 * `true` when the run has an approved plan and this row was recorded
	 * outside every plan step (`metadata.off_plan`). See `planStepActivity.ts`.
	 */
	offPlan?: boolean;
}

/**
 * One position in the rendered feed: either a row of this run's own, or a whole
 * delegated child collapsed behind one header.
 */
export type TimelineSegment =
	| { kind: 'row'; entry: TimelineEntry }
	| {
			kind: 'delegation';
			group: ExecutionPanelDelegationGroup;
			entries: readonly TimelineEntry[];
	  };

/**
 * Fold each delegated child's rows into a single collapsible segment, left in
 * the parent's feed at the point the child's first event landed.
 *
 * **One segment per child, not per contiguous run of its rows.** A parent emits
 * its own lifecycle rows (`child.failed`, a resume) while a child's events are
 * still arriving, so contiguity would split one delegation into several blocks
 * that each claim to be the delegation. The child is one unit of work and reads
 * as one.
 *
 * A child with no `delegations` entry — a payload predating the field — stays a
 * plain row rather than vanishing: an ungrouped row is a formatting loss, a
 * dropped one is the bug this whole change exists to fix.
 */
export function groupTimelineByDelegation(
	entries: readonly TimelineEntry[],
	delegations: readonly ExecutionPanelDelegationGroup[] | null | undefined
): TimelineSegment[] {
	const groups = new Map<string, ExecutionPanelDelegationGroup>();
	for (const delegation of delegations ?? []) {
		if (delegation?.execution_id) groups.set(delegation.execution_id, delegation);
	}
	if (groups.size === 0) return entries.map((entry) => ({ kind: 'row', entry }));

	const segments: TimelineSegment[] = [];
	const collected = new Map<string, TimelineEntry[]>();

	for (const entry of entries) {
		const executionId = entry.executionId;
		const group = executionId === null ? undefined : groups.get(executionId);
		if (group === undefined || executionId === null) {
			segments.push({ kind: 'row', entry });
			continue;
		}
		let bucket = collected.get(executionId);
		if (bucket === undefined) {
			bucket = [];
			collected.set(executionId, bucket);
			// Reserve this child's place at its first event, then keep filling the
			// same bucket — later rows join the block above rather than opening a
			// second one further down.
			segments.push({ kind: 'delegation', group, entries: bucket });
		}
		bucket.push(entry);
	}

	return segments;
}

export interface DelegationTimeSpan {
	startClock: string | null;
	endClock: string | null;
	duration: string | null;
	summary: string | null;
}

/**
 * Derives the start time, completion time, duration, and human summary of a delegated child execution.
 */
export function delegationSpan(
	entries: readonly TimelineEntry[],
	group?: ExecutionPanelDelegationGroup | null
): DelegationTimeSpan {
	let startMs: number | null = null;
	let endMs: number | null = null;

	if (group?.started_at) {
		const parsed = Date.parse(group.started_at);
		if (Number.isFinite(parsed) && parsed > 0) startMs = parsed;
	}
	if (group?.completed_at) {
		const parsed = Date.parse(group.completed_at);
		if (Number.isFinite(parsed) && parsed > 0) endMs = parsed;
	}

	if (entries.length > 0) {
		const validEntries = entries.filter((e) => recorded(e.at));
		if (validEntries.length > 0) {
			if (startMs === null) startMs = validEntries[0].at;
			if (endMs === null && group?.status !== 'running') {
				endMs = validEntries[validEntries.length - 1].at;
			}
		}
	}

	const startClock = timelineClock(startMs);
	const endClock = timelineClock(endMs);
	const durationMs =
		startMs !== null && endMs !== null && endMs >= startMs ? endMs - startMs : null;
	const duration = durationIfKnown(durationMs);

	let summary: string | null = null;
	if (startClock && endClock && startClock !== endClock && duration) {
		summary = `${startClock} – ${endClock} (${duration})`;
	} else if (startClock && endClock && startClock !== endClock) {
		summary = `${startClock} – ${endClock}`;
	} else if (startClock) {
		summary = group?.status === 'running' ? `started ${startClock}` : startClock;
	}

	return { startClock, endClock, duration, summary };
}

/**
 * Maximum activity rows rendered at once. The complete list remains available
 * to aggregates and offset calculations; only DOM growth is bounded.
 */
export const TIMELINE_RENDER_LIMIT = 200;

export interface TimelineWindow {
	entries: readonly TimelineEntry[];
	hidden: number;
}

/**
 * The newest bounded window of a chronological timeline, plus the exact number
 * omitted. Returning the count is mandatory: silent truncation would present a
 * partial run as the complete one.
 */
export function timelineWindow(
	entries: readonly TimelineEntry[],
	limit: number = TIMELINE_RENDER_LIMIT
): TimelineWindow {
	const safeLimit = Number.isFinite(limit) ? Math.max(1, Math.floor(limit)) : TIMELINE_RENDER_LIMIT;
	if (entries.length <= safeLimit) return { entries, hidden: 0 };
	return {
		entries: entries.slice(entries.length - safeLimit),
		hidden: entries.length - safeLimit
	};
}

/** A finite number, or `null`. Anything else on the wire is not a measurement. */
function num(raw: unknown): number | null {
	return typeof raw === 'number' && Number.isFinite(raw) ? raw : null;
}

/** A non-empty trimmed string, or `null`. Absence and blank are one answer. */
function text(raw: unknown): string | null {
	if (typeof raw !== 'string') return null;
	const trimmed = raw.trim();
	return trimmed ? trimmed : null;
}

/**
 * The `FeedItem.status` vocabulary, as this list's. A `Record<FeedItemStatus, …>`
 * so a sixth feed status is a compile error here rather than a row that silently
 * loses its mark.
 */
const FEED_STATUS: Record<FeedItemStatus, TimelineEntryStatus> = {
	running: 'running',
	done: 'done',
	failed: 'failed',
	needs_action: 'waiting',
	info: 'info'
};

/**
 * What each event type is. Prefix-matched because the wire's event types are a
 * dotted namespace (`llm.succeeded`, `tool.failed`) with no union to switch on,
 * and the namespace is the classification the backend already made.
 */
function activityKind(eventType: string): TimelineEntryKind {
	if (eventType.startsWith('llm.')) return 'llm';
	if (eventType.startsWith('reasoning')) return 'reasoning';
	if (eventType.startsWith('tool.')) return 'tool';
	return 'event';
}

function recipeLifecycle(
	eventType: string,
	meta: Record<string, unknown>
): RecipeLifecycleEvent | null {
	if (eventType !== 'recipe.replay') return null;
	const kind = text(meta.kind);
	if (!kind?.startsWith('recipe.replay.')) return null;
	return {
		kind,
		recipe_id: text(meta.recipe_id) ?? undefined,
		template: text(meta.template) ?? undefined,
		duration_ms: num(meta.duration_ms) ?? undefined,
		step_id: text(meta.step_id) ?? undefined,
		class: text(meta.class) ?? undefined,
		replayed_steps: num(meta.replayed_steps) ?? undefined,
		origin: text(meta.origin) ?? undefined,
		to: text(meta.to) ?? undefined,
		version: num(meta.version) ?? undefined,
		decision: text(meta.decision) ?? undefined
	};
}

/**
 * Title an activity row in the **same vocabulary the chat activity card uses**,
 * so one step reads identically in the card and in the panel it opens:
 * `Thinking with <model>` for an LLM call (the capability when the event names
 * no model), `Calling <tool>` /
 * `<tool> returned` / `<tool> failed` for a tool.
 *
 * Falls back to the backend's own humanized title whenever the metadata that
 * would name the tool or the capability is absent — which is what an older
 * payload looks like, and a title reading `Calling undefined` would be worse
 * than the generic one.
 */
function activityTitle(item: FeedItem, meta: Record<string, unknown>, eventType: string): string {
	const preparation = contextPreparationRow(eventType, meta);
	if (preparation) return preparation.label;
	const tool = text(meta.detail) ?? text(meta.target) ?? text(meta.tool_name) ?? text(meta.action_type);
	const capability = text(meta.capability);
	const recipeKind = text(meta.kind);
	if (eventType === 'recipe.replay' && recipeKind?.startsWith('recipe.replay.')) {
		return recipeKind.replace('recipe.replay.', 'Recipe ').replaceAll('.', ' ').replaceAll('_', ' ');
	}
	switch (eventType) {
		case 'tool.succeeded':
			return tool ? `${tool} returned` : item.title;
		case 'tool.failed':
			return tool ? `${tool} failed` : item.title;
		case 'tool.started':
		case 'tool.requested':
			return tool ? `Calling ${tool}` : item.title;
		// The model by name, as the chat card titles it: the one asked for while
		// the call runs, the one that answered once it settles. The capability
		// ("decision") names an operation, not a model, so it is only the
		// fallback for an event that carries no model.
		case 'llm.requested':
		case 'llm.routed':
			return text(meta.model) || capability ? `Thinking with ${requestedModelLabel(meta)}` : item.title;
		case 'llm.succeeded':
		case 'llm.failed': {
			const model = answeredModelLabel(meta) ?? text(meta.model);
			const label = model ?? capability;
			return label ? `Thinking with ${label}` : item.title;
		}
		default:
			return item.title;
	}
}

/**
 * The usage this event reported, or `null`.
 *
 * `null` when **no** usage field was present, rather than a record of four
 * nulls: the row's meta line renders nothing at all for a call that reported no
 * bill, and a zeroed record would render `– → – tok`, which claims the call was
 * measured and cost nothing.
 */
function tokensOf(meta: Record<string, unknown>): TimelineTokens | null {
	const input = num(meta.input_tokens);
	const output = num(meta.output_tokens);
	const cacheRead = num(meta.cache_read_tokens);
	const cacheCreation = num(meta.cache_creation_tokens);
	if (input === null && output === null && cacheRead === null && cacheCreation === null) {
		return null;
	}
	return { input, output, cacheRead, cacheCreation };
}

/**
 * How a shell command ended.
 *
 * A missing exit code on a *complete* command is `done` rather than `failed`:
 * the backend omits the field for commands it did not wait on, and calling one
 * failed because nothing recorded its status is the fabrication this panel
 * exists to remove.
 */
function shellStatus(isComplete: boolean, exitCode: number | null | undefined): TimelineEntryStatus {
	if (!isComplete) return 'running';
	const code = num(exitCode);
	return code === null || code === 0 ? 'done' : 'failed';
}

/**
 * How a run ended, from its wire status. Deliberately **not** `verdictStatusOf`:
 * that answers what the *task* is, over `TaskStatus`, and this row is one
 * lifecycle event in a list. `deferred` and `paused` are `waiting` here because
 * the run stopped and something has to resume it.
 */
function runStatus(status: string): TimelineEntryStatus {
	switch (status) {
		case 'running':
		case 'planning':
			return 'running';
		case 'completed':
			return 'done';
		case 'failed':
			return 'failed';
		case 'paused':
		case 'deferred':
			return 'waiting';
		default:
			return 'info';
	}
}

/** The activity rows one LLM call emits, paired on `llm_call_id`. */
const LLM_CALL_EVENTS = new Set(['llm.requested', 'llm.routed', 'llm.succeeded', 'llm.failed']);
/** The activity rows one tool call emits, paired on `call_id`. */
const TOOL_CALL_EVENTS = new Set(['tool.started', 'tool.requested', 'tool.succeeded', 'tool.failed']);
/** The events that settle a paired call. */
const SETTLING_EVENTS = new Set(['llm.succeeded', 'llm.failed', 'tool.succeeded', 'tool.failed', 'context.retrieval.completed']);

/**
 * The id one call's rows share, or `null` when this row has none (a server
 * predating the ids, or an event that is not a call). An LLM call pairs on
 * `llm_call_id`; a tool call on its run and `call_id`, because a call id is
 * only unique within the run that issued it.
 */
function pairKeyOf(eventType: string, meta: Record<string, unknown>): string | null {
	const preparation = contextPreparationRow(eventType, meta);
	if (preparation) return preparation.key;
	if (LLM_CALL_EVENTS.has(eventType)) {
		const id = text(meta.llm_call_id);
		return id ? `llm-call:${id}` : null;
	}
	if (TOOL_CALL_EVENTS.has(eventType)) {
		const id = text(meta.call_id);
		return id ? `tool-call:${text(meta.execution_id) ?? ''}:${id}` : null;
	}
	return null;
}

/** `next` over `base`, except that a field `next` leaves absent keeps `base`'s. */
function mergeMeta(
	base: Record<string, unknown>,
	next: Record<string, unknown>
): Record<string, unknown> {
	const merged = { ...base };
	for (const [key, value] of Object.entries(next)) {
		if (value === null || value === undefined) continue;
		if (typeof value === 'string' && !value.trim()) continue;
		merged[key] = value;
	}
	return merged;
}

/**
 * Which run a payload describes, or `null`.
 *
 * **Here rather than in either adapter**, because three callers need it and two
 * of them are adapters that already import each other's neighbours — putting it
 * in one would make the other reach across a cycle to ask. The task adapter asks
 * whether a fetched payload is about the run its Run act names; the execution
 * adapter asks for that act's identity; the live subscription asks whether a
 * pushed state belongs to the drawer on screen. Two answers to "which run is
 * this?" would disagree the first time either was edited, and the failure is a
 * drawer showing one run's rows under another run's verdict — which every value
 * involved being individually valid means nothing downstream can catch.
 */
export function executionIdOf(state: ExecutionPanelState): string | null {
	return text(state.overview?.execution_id) ?? text(state.debug?.selected_execution?.execution_id);
}

/**
 * Every event this run recorded, oldest first — or `null` when nothing observed
 * them.
 *
 * **Three absences answer `null`, and none of them answers `[]`.** No payload at
 * all (the caller has no event source, or its request failed); no run for the
 * act to be about; and a payload that turns out to describe **another
 * execution**. That third one is the reason this guard lives here rather than in
 * the caller that happens to need it: the task adapter names its run from the
 * store row and reads its events from a separately fetched payload, so the two
 * can name different runs whenever the row moves on between fetches, and a list
 * of another attempt's calls under this attempt's heading is a fabrication that
 * every individual value would corroborate. The execution adapter passes the id
 * it read off the payload, so for it the check is tautological and the answer is
 * always the array it had before.
 *
 * `[]` is reserved for what it means: this run's events were read and there were
 * none.
 *
 * **The three things it does not carry**, because the acts already do: open
 * clarifications (the Plan act), plan steps (the Run act's own step list), and
 * the output block (the Output act). Rendering any of them here would put the
 * same fact on screen twice at two different resolutions, and the reader would
 * have no way to know they were one thing.
 *
 * **The row's instant is rendered, and this reverses an earlier decision.** This
 * comment used to say a per-row absolute timestamp belonged at L3 and that the
 * list's order carried the only temporal fact an L2 row needed. The owner's
 * review of the built panel overrules it: a feed whose rows say *when* nothing
 * happened is a feed you cannot line up against a log, a chat transcript or a
 * memory of when you asked. So `at` renders — as a wall clock **and** as an
 * offset from the feed's origin, both at the row's leading edge, on the reading
 * that L3 is *identifiers* rather than every precise number (§2, settled in
 * Task 6). The duration each row already carried is a different fact and keeps
 * its own place at the trailing edge: when → what → how long.
 *
 * `executionId` is **the run the act claims to be about**, resolved by the
 * caller from whatever it holds — a store row, or the payload itself. It is
 * never re-derived as a substitute for what the caller said, because a list that
 * silently switched runs would still be internally consistent and therefore
 * unfalsifiable on screen; `executionIdOf` is read only to *disagree* with it,
 * above. It then filters `output.recent_runs`, which is the task's whole run
 * history rather than this run's: unfiltered, a task retried three times renders
 * three indistinguishable `Run started` rows, and the only thing that could tell
 * them apart is an execution id — an L3 value, in an L2 row.
 *
 * The backend serialises absent collections as `null` even where the type says
 * array, so every iteration coalesces — one null field must not throw and take
 * the whole panel with it.
 */
export function deriveTimeline(
	state: ExecutionPanelState | null,
	executionId: string | null
): TimelineEntry[] | null {
	if (state === null || executionId === null) return null;
	if (executionIdOf(state) !== executionId) return null;

	const entries: TimelineEntry[] = [];

	// The full seq-ordered log is the primary source; `recent_activity` is a
	// capped view of the same events, read only when the full log is absent so
	// the two cannot double-render.
	const activityLog = state.run?.activity_log ?? [];
	const activity = activityLog.length > 0 ? activityLog : (state.run?.recent_activity ?? []);
	// One call, one row, as in the chat card. A call's rows share an id —
	// `llm_call_id` for a model call (`llm.requested` → `llm.routed` →
	// `llm.succeeded` / `llm.failed`), `call_id` for a tool (`tool.started` →
	// `tool.succeeded` / `tool.failed`) — and fold into ONE entry under a
	// stable id: running from the first row (with the model as soon as the
	// route names it), and the settled row replaces it in place, keeping the
	// start as the entry's instant. A late start never reopens a settled call.
	const pairIndex = new Map<string, number>();
	const pairMeta = new Map<number, Record<string, unknown>>();
	// The fallback for a server that sends no `llm_call_id` on the request:
	// `llm.requested` then names only the capability and nothing links it to
	// its settled event. The two arrive in order per run, so a requested row
	// the next settled LLM event of the same run answers is that call's start,
	// and only the settled row (model, time, bill) is kept. A requested row
	// nothing settled stays: it is a call in flight or lost.
	const openRequest = new Map<string, number>();
	const answeredRequests = new Set<number>();
	for (const item of activity) {
		if (!item) continue;
		const meta = (item.metadata ?? {}) as Record<string, unknown>;
		const eventType = text(meta.event_type) ?? '';
		const preparation = contextPreparationRow(eventType, meta);
		// The server retires an unpaired start once the execution moves on.
		// Only a measured completion overrides that authoritative feed status.
		const preparationStatus = eventType === 'context.retrieval.completed'
			? preparation?.status : undefined;
		const kind = activityKind(eventType);
		const recipe = recipeLifecycle(eventType, meta);
		const callRun = text(meta.execution_id) ?? '';
		const pairKey = pairKeyOf(eventType, meta);
		const paired = pairKey === null ? undefined : pairIndex.get(pairKey);
		if (paired !== undefined) {
			const previous = entries[paired];
			const callMeta = mergeMeta(pairMeta.get(paired) ?? {}, meta);
			pairMeta.set(paired, callMeta);
			if (SETTLING_EVENTS.has(eventType)) {
				entries[paired] = {
					...previous,
					status: preparationStatus ?? FEED_STATUS[item.status] ?? previous.status,
					title: activityTitle(item, callMeta, eventType),
					body: text(item.summary) ?? previous.body,
					latencyMs: preparation?.durationMs ?? num(meta.latency_ms) ?? previous.latencyMs,
					model: text(meta.model) ?? previous.model,
					costUsd:
						kind === 'llm' ? (num(meta.cost_usd) ?? num(meta.cost) ?? previous.costUsd) : null,
					tokens: tokensOf(meta) ?? previous.tokens
				};
			} else if (previous.status === 'running') {
				// A route (or a repeated start) on a call still in flight: the
				// model it names becomes the row's, and the row stays running.
				entries[paired] = {
					...previous,
					title: activityTitle(item, callMeta, eventType),
					model: text(meta.model) ?? previous.model
				};
			}
			continue;
		}
		if (eventType === 'llm.requested' && pairKey === null) {
			openRequest.set(callRun, entries.length);
		} else if (eventType === 'llm.succeeded' || eventType === 'llm.failed') {
			const requested = openRequest.get(callRun);
			if (requested !== undefined) answeredRequests.add(requested);
			openRequest.delete(callRun);
		}
		if (pairKey !== null) {
			pairIndex.set(pairKey, entries.length);
			pairMeta.set(entries.length, meta);
		}
		// A tool's own start instant, when the row carries one, is when it began.
		const startedAt = eventType === 'tool.started' ? num(meta.started_at_ms) : null;
		entries.push({
			id: pairKey ?? item.id,
			at: startedAt ?? num(item.created_at) ?? 0,
			kind,
			status: preparationStatus ?? FEED_STATUS[item.status] ?? null,
			title: activityTitle(item, meta, eventType),
			body: text(item.summary),
			detail: null,
			latencyMs: preparation?.durationMs ?? num(meta.latency_ms),
			model: text(meta.model),
			// `cost_usd` is the unit-bearing contract. `cost` keeps older execution
			// snapshots useful while their persisted activity rows age out.
			costUsd: kind === 'llm' ? (num(meta.cost_usd) ?? num(meta.cost)) : null,
			tokens: tokensOf(meta),
			screenshot: false,
			// The child's own identity, so a delegated run can group under its own
			// header instead of reading as the parent's work.
			executionId: text(meta.execution_id),
			agentId: text(item.agent_id),
			planStepId: text(meta.plan_step_id),
			offPlan: meta.off_plan === true,
			...(recipe ? { recipe } : {})
		});
	}

	if (answeredRequests.size > 0) {
		const kept = entries.filter((_, index) => !answeredRequests.has(index));
		entries.length = 0;
		entries.push(...kept);
	}

	// The progress/reasoning timeline, and only when there is no activity log —
	// the two describe the same run from two projections.
	if (activityLog.length === 0) {
		for (const entry of state.debug?.timeline ?? []) {
			if (!entry) continue;
			entries.push({
				id: `timeline:${entry.id}`,
				at: num(entry.timestamp) ?? 0,
				kind: 'reasoning',
				status: entry.severity === 'error' || entry.severity === 'critical' ? 'failed' : 'info',
				title: entry.title,
				body: text(entry.message),
				detail: null,
				latencyMs: null,
				model: null,
				costUsd: null,
				tokens: null,
				screenshot: false,
				executionId: text(entry.execution_id),
				agentId: text(entry.agent_id),
				planStepId: text(entry.plan_step_id),
				offPlan: entry.off_plan === true
			});
		}
	}

	for (const shell of state.debug?.shell_entries ?? []) {
		if (!shell) continue;
		entries.push({
			// `started_at` rather than the loop position: two commands can share a
			// step index, and a positional id would move under the reader every time
			// the list re-sorts.
			id: `shell:${shell.execution_id}:${shell.step_index}:${shell.started_at}`,
			at: num(shell.started_at) ?? 0,
			kind: 'shell',
			status: shellStatus(shell.is_complete, shell.exit_code),
			title: shell.command,
			body: null,
			detail: (shell.lines ?? []).map((line) => line.text).join('\n') || null,
			latencyMs: null,
			model: null,
			costUsd: null,
			tokens: null,
			screenshot: false,
			executionId: text(shell.execution_id),
			agentId: null
		});
	}

	for (const observation of state.debug?.observations ?? []) {
		if (!observation) continue;
		entries.push({
			id: `observation:${observation.observation_id}`,
			at: num(observation.captured_at) ?? 0,
			kind: 'observation',
			// An observation is a capture, not an outcome: it neither succeeded nor
			// failed, so it takes the neutral mark rather than a tick it did not earn.
			status: 'info',
			title: text(observation.url) ?? text(observation.page_stage) ?? 'Observation',
			body: null,
			detail: null,
			latencyMs: null,
			model: null,
			costUsd: null,
			tokens: null,
			screenshot: observation.has_screenshot === true,
			// Observations carry no execution id of their own; they belong to the
			// run on screen.
			executionId,
			agentId: null
		});
	}

	for (const run of state.output?.recent_runs ?? []) {
		if (!run) continue;
		// Only the run this act describes. This collection is the **task's** whole
		// run history rather than this run's, so it is the one thing here that has
		// to be filtered; the id is non-null by the guard at the top, and repeating
		// that check here would be a second guard against a condition already
		// settled, which is how a real one comes to be deleted as redundant.
		if (run.execution_id !== executionId) continue;
		entries.push({
			id: `run:${run.execution_id}:start`,
			at: num(run.started_at) ?? 0,
			kind: 'lifecycle',
			status: 'info',
			title: 'Run started',
			body: null,
			// No id here. The execution id is L3 and the Run act's provenance
			// already carries it; a `<pre>` block holding one is an L3 value sitting
			// in an L2 row, which is the defect design §1 blames for the panel this
			// replaces.
			detail: null,
			latencyMs: null,
			model: null,
			costUsd: null,
			tokens: null,
			screenshot: false,
			executionId: run.execution_id,
			agentId: null
		});
		const endedAt = num(run.ended_at);
		if (endedAt !== null) {
			entries.push({
				id: `run:${run.execution_id}:end`,
				at: endedAt,
				kind: 'lifecycle',
				status: runStatus(run.status),
				title: `Run ${run.status}`,
				body:
					text(run.completion_summary) ?? text(run.error_message) ?? text(run.completion_outcome),
				detail: null,
				latencyMs: null,
				model: null,
				costUsd: null,
				tokens: null,
				screenshot: false,
				executionId: run.execution_id,
				agentId: null
			});
		}
	}

	entries.sort((a, b) => a.at - b.at);
	return entries;
}

/**
 * A token count, scaled to the largest unit that leaves a number worth reading.
 * `null` for a count nothing reported — the meta line then omits that half of
 * the arrow rather than printing a zero the provider never sent.
 */
export function formatTokens(count: number | null): string | null {
	if (count === null || count < 0) return null;
	if (count < 1000) return String(Math.round(count));
	if (count < 1_000_000) return `${(count / 1000).toFixed(count < 10_000 ? 1 : 0)}k`;
	return `${(count / 1_000_000).toFixed(1)}M`;
}

/**
 * The prompt-cache hit rate for one row, as a percentage, or `null`.
 *
 * `cachedPercentOf`'s, over this row's two figures — not a second copy of it.
 * The rate is `cacheRead / input` because **this provider's `input_tokens`
 * already includes the cached portion**, and dividing by `input + cacheRead`
 * reports roughly half the real rate on a warm call; that fact now lives in one
 * place, and the run-level aggregate below reads it from the same function.
 */
export function cachedPercent(tokens: TimelineTokens | null): number | null {
	if (tokens === null) return null;
	return cachedPercentOf(tokens.cacheRead, tokens.input);
}

/**
 * The row's second line of fact: which model answered, what it cost, and how
 * much of the prompt was cached. Empty when the event reported none of it, so a
 * row with nothing to add renders no element rather than a stray separator —
 * the rule `runSummary` and `fileMeta` both already follow.
 *
 * **This is L2, and the level is not a matter of flavour.** L3 is act-scoped
 * `label → value` identifier rows behind one Details control (design §2, settled
 * in Task 6); there is no honest provenance row for *the seventeenth call's*
 * cache rate, and inventing one would repeat exactly the mistake that section
 * struck when it refused raw payloads a place on the ladder. What is genuinely
 * L3 here is unchanged: the Run act's execution id, which is the identifier that
 * gets a reader to `/events` where the payloads live.
 */
export function timelineMeta(entry: TimelineEntry): string {
	const model = entry.model ? [entry.model] : [];
	return [...model, ...timelineCost(entry)].join(' · ');
}

/**
 * The cost half of the line above, **without the model**, as its own segments.
 *
 * It exists because the two halves are different kinds of thing and the row now
 * renders them differently: a model name is an **identifier**, so it goes in
 * `<code>`, while the token counts are figures a reader compares down a column, so
 * they stay tabular proportional text. Joined into one string they had to share one
 * treatment, and the treatment they shared made a provider's model id look like
 * prose.
 *
 * `timelineMeta` is kept and is now written in terms of this, so the one string
 * form has no second implementation — a caller that wants the whole line as text
 * still gets exactly what it got before.
 *
 * Segments rather than a joined string: the separator is the row's to draw, since
 * it now sits between elements rather than inside one.
 */
export function timelineCost(entry: TimelineEntry): string[] {
	if (entry.tokens === null) return [];

	const segments: string[] = [];
	const input = formatTokens(entry.tokens.input);
	const output = formatTokens(entry.tokens.output);
	// Both halves or neither: `– → 380 tok` reads as a rendering fault, and the
	// arrow means nothing with one side missing.
	if (input !== null && output !== null) segments.push(`${input} → ${output} tok`);
	const cached = cachedPercent(entry.tokens);
	if (cached !== null) segments.push(`${cached}% cached`);

	return segments;
}

/**
 * One model this run used, and how many of its calls that model answered.
 */
export interface RunModelCalls {
	name: string;
	calls: number;
}

/**
 * What the whole run cost, summed from the rows the Run act already renders.
 *
 * **The question this answers is one nobody could answer before.** Every
 * timeline row carries its own model, token bill and latency, and a seventy-event
 * run therefore states its cost seventy times and its total nowhere — "what did
 * this run cost, and how long did it really take" was answerable only by reading
 * and summing by hand. This is that sum, and it is the one place the Run act
 * gets it.
 *
 * **Derived on the client from the timeline, with no new request** (design §5).
 * The events were already fetched and already projected; a second endpoint for
 * their totals would be a second answer to what this run cost, and the two could
 * disagree about a run the reader is looking at.
 *
 * Every field is separately absent, because they are separately observed:
 *
 * - `costUsd` is `null` when no LLM event carried a backend-recorded price.
 *   The client never reconstructs it from model ids and token totals.
 * - `usage` is `null` when **no** event reported a bill. Not a zeroed record —
 *   see `totalUsage`, whose contract this is.
 * - `modelTimeMs` is `null` when nothing was timed. A run whose events carried
 *   no latency has no measured model time, which is a different fact from a run
 *   that spent no time in a model, and `0s` would claim the second.
 * - `observedMs` is `null` unless the feed spans two recorded instants. One
 *   event, or none with a time, gives nothing to be a span *of*, and a zero span
 *   is not a duration a reader can act on.
 * - `models` is empty when no event named the model that answered.
 * - `failures` counts what it can see and is `0` when it saw none. That zero is
 *   an observation rather than an absence — the feed was read — and the row
 *   built from it is still dropped, on the same rule `runSummary` follows for a
 *   step's retries: saying nothing about failures beats claiming there were
 *   none.
 */
export interface RunCost {
	/** Sum of backend-recorded per-call USD prices; null when no call carried one. */
	costUsd: number | null;
	usage: UsageTotals | null;
	/** Summed latency across every timed row. Concurrent calls each count in full. */
	modelTimeMs: number | null;
	/** First recorded instant in the feed to the last. */
	observedMs: number | null;
	/** Most-used model first; ties keep the order the feed introduced them in. */
	models: RunModelCalls[];
	/** Failed rows, excluding the run's own lifecycle outcome. */
	failures: number;
}

/**
 * A **failed** row that is a failed *attempt*, rather than the run's verdict.
 *
 * A `lifecycle` row reading `Run failed` is the outcome of everything above it,
 * not another thing that was tried; counting it would add one to every failed
 * run's tally and the number would be wrong by exactly one in the case a reader
 * is most likely to check.
 */
function isFailedAttempt(entry: TimelineEntry): boolean {
	return entry.status === 'failed' && entry.kind !== 'lifecycle';
}

/**
 * What this run cost, or `null` when there is no feed to sum.
 *
 * `null` for a `null` timeline — nothing observed this run's events, which is
 * the case the whole panel's absence rule is built around — and `null` for an
 * empty one, where the events were read and there were none to sum. Neither is a
 * run that cost nothing.
 */
export function runCost(entries: readonly TimelineEntry[] | null): RunCost | null {
	if (entries === null || entries.length === 0) return null;

	let modelTimeMs: number | null = null;
	let earliest: number | null = null;
	let latest: number | null = null;
	let failures = 0;
	let costUsd: number | null = null;
	const byModel = new Map<string, number>();

	for (const entry of entries) {
		const latency = entry.latencyMs;
		if (latency !== null && Number.isFinite(latency) && latency > 0) {
			modelTimeMs = (modelTimeMs ?? 0) + latency;
		}
		if (recorded(entry.at)) {
			if (earliest === null || entry.at < earliest) earliest = entry.at;
			if (latest === null || entry.at > latest) latest = entry.at;
		}
		if (isFailedAttempt(entry)) failures += 1;
		if (
			entry.kind === 'llm' &&
			entry.costUsd !== null &&
			Number.isFinite(entry.costUsd) &&
			entry.costUsd >= 0
		) {
			costUsd = (costUsd ?? 0) + entry.costUsd;
		}
		if (entry.model !== null) byModel.set(entry.model, (byModel.get(entry.model) ?? 0) + 1);
	}

	const span = earliest === null || latest === null ? 0 : latest - earliest;
	const models = [...byModel].map(([name, calls]) => ({ name, calls }));
	// Stable, so equal counts keep the order the feed introduced them in — the
	// only ordering fact the list actually contains.
	models.sort((a, b) => b.calls - a.calls);

	return {
		costUsd,
		usage: totalUsage(entries.map((entry) => entry.tokens)),
		modelTimeMs,
		observedMs: span > 0 ? span : null,
		models,
		failures
	};
}

/**
 * A backend-recorded USD amount: three decimal places below a dollar
 * (`$0.005`, `$0.040`, `$0.999`), two from a dollar up (`$1.00`, `$3.23`,
 * `$12.00`), `$0.000` for a zero and `<$0.001` for a positive amount that
 * rounds to zero thousandths. Rounding is half-up, with no thousands
 * separators; an amount that rounds up to a dollar at three places takes the
 * dollar form. Missing, negative or non-finite prices are not measurements
 * and render nothing. The same rule as magios `TaskTimelineFormatting.usd`
 * and magdroid `TaskUsageFormat.usd`.
 */
export function formatUsd(amount: number | null): string | null {
	if (amount === null || !Number.isFinite(amount) || amount < 0) return null;
	const mills = roundHalfUp(amount, 1000);
	if (mills === 0) return amount === 0 ? '$0.000' : '<$0.001';
	if (mills < 1000) return `$0.${String(mills).padStart(3, '0')}`;
	return `$${(roundHalfUp(amount, 100) / 100).toFixed(2)}`;
}

/**
 * `formatUsd` for a screen reader: the sub-tenth-of-a-cent mark reads as
 * words instead of a symbol run.
 */
export function spokenUsd(amount: number | null): string | null {
	const text = formatUsd(amount);
	return text === '<$0.001' ? 'less than a tenth of a cent' : text;
}

/**
 * `amount * scale` rounded half-up on its decimal value: the product is
 * cleaned to 12 significant digits first, so a binary artefact such as
 * `1.005 * 100 = 100.49999…` still rounds to 101.
 */
function roundHalfUp(amount: number, scale: number): number {
	return Math.floor(Number((amount * scale).toPrecision(12)) + 0.5);
}

/**
 * The Run act's cost rows, for its L3 disclosure — or none, when the run has
 * nothing measured to report.
 *
 * **A run-level total is an L3 row where a per-event one is not**, and that is
 * the line design §2 draws rather than a softening of it. L3 is act-scoped
 * `label → value` rows about *the act*; there is no honest row that names the
 * seventeenth call's cache rate, which is why the per-row figures stay at L2 in
 * `timelineCost`. `Tokens = 12k → 3.1k tok · 18 calls` is a fact about this
 * execution, in exactly the shape the disclosure already holds — one label, one
 * value, and it belongs beside the execution id a reader came down here to copy.
 *
 * **Every row is independently earned and independently absent.** A run whose
 * events carried no usage renders no token row and no cache row — not `0 tok`,
 * not `0% cached` — while still reporting the model time, if anything timed it.
 * Absence and zero are not one value, and a fabricated zero here would be worse
 * than a missing row: the reader would take it for a measurement.
 *
 * Pairs rather than `ProvenanceRow`s, so a caller composes them with its own
 * identifier rows and hands the whole list to `provenanceRows` once.
 */
export function runCostRows(entries: readonly TimelineEntry[] | null): Array<[string, string]> {
	const cost = runCost(entries);
	if (cost === null) return [];

	const rows: Array<[string, string]> = [];
	const usage = cost.usage;
	const priced = formatUsd(cost.costUsd);
	if (priced !== null) rows.push(['Cost', priced]);

	if (usage !== null) {
		const input = formatTokens(usage.input);
		const output = formatTokens(usage.output);
		// Both halves or neither, exactly as a single row's cost line decides it:
		// `– → 3.1k tok` reads as a rendering fault, and the arrow means nothing
		// with one side missing.
		if (input !== null && output !== null) {
			rows.push([
				'Tokens',
				`${input} → ${output} tok · ${usage.calls} ${usage.calls === 1 ? 'call' : 'calls'}`
			]);
		}
		if (usage.cachedPercent !== null) {
			const served = formatTokens(usage.cacheRead);
			const prompt = formatTokens(usage.input);
			// The rate is the answer; the two counts behind it are what makes it
			// checkable. They are dropped rather than guessed if either is absent —
			// which the rate itself being non-null already rules out, and the guard
			// costs nothing next to a row that could read `of null`.
			rows.push([
				'Prompt cache',
				served === null || prompt === null
					? `${usage.cachedPercent}% cached`
					: `${usage.cachedPercent}% cached · ${served} of ${prompt} tok`
			]);
		}
	}

	const modelTime = durationIfKnown(cost.modelTimeMs);
	if (modelTime !== null) {
		const observed = durationIfKnown(cost.observedMs);
		rows.push([
			'Model time',
			observed === null
				? modelTime
				// **The share can exceed 100%, and when it does it is telling the
				// truth.** Latencies are per call and a run can have several in
				// flight at once, so summed model time is not bounded by the span it
				// happened in. Clamping it would hide the one thing the comparison is
				// for: a run that spent 3x its wall clock inside models ran them in
				// parallel, and a reader who sees `112%` learns that, where `100%`
				// would tell them the run was saturated and nothing more.
				: `${modelTime} of ${observed} observed · ${Math.round(
						((cost.modelTimeMs as number) / (cost.observedMs as number)) * 100
					)}%`
		]);
	}

	// Dropped at zero — see `RunCost.failures`.
	if (cost.failures > 0) rows.push(['Failed calls', String(cost.failures)]);

	// The label carries the count claim, so a reader knows whether one model
	// answered this run or several without counting the names. One model needs no
	// call count: it answered all of them, and the token row already says how many
	// that was.
	if (cost.models.length === 1) rows.push(['Model', cost.models[0].name]);
	else if (cost.models.length > 1) {
		rows.push(['Models', cost.models.map((model) => `${model.name} (${model.calls})`).join(' · ')]);
	}

	return rows;
}

/**
 * Two digits, always — `9` is not a clock field.
 */
function pad(value: number): string {
	return value < 10 ? `0${value}` : String(value);
}

/**
 * An instant nothing recorded, distinguished from one recorded at the epoch.
 *
 * `deriveTimeline` coalesces every missing timestamp to `0` so a null field
 * cannot throw and take the whole panel down. That leaves `0` meaning **"this
 * event carried no time"**, and it must never render: `01:00:00` against a 1970
 * epoch is precisely the invented number this panel exists to remove, and it
 * would sort and read as a real one.
 *
 * Anything non-finite or negative is the same answer for the same reason —
 * `durationIfKnown` above it draws the line in the same place.
 */
function recorded(at: number | null): boolean {
	return at !== null && Number.isFinite(at) && at > 0;
}

/**
 * The row's wall clock, `HH:MM:SS` in the reader's own timezone — or `null` when
 * the event carried no instant.
 *
 * **Seconds are not optional.** A feed's rows are often within one minute of
 * each other, and `14:05` repeated eleven times answers nothing the order did
 * not already. Milliseconds are the other direction and belong to the latency
 * column, which measures rather than locates.
 *
 * Local rather than UTC, and hand-built rather than `toLocaleTimeString`: a
 * locale can render a 12-hour clock with a meridiem, which is two more glyphs
 * of varying width in a column whose whole job is to be scannable straight
 * down. The date is deliberately absent — a run spanning midnight is the one
 * case this loses, and the offset beside it is what disambiguates it.
 */
export function timelineClock(at: number | null): string | null {
	if (!recorded(at)) return null;
	const when = new Date(at as number);
	return `${pad(when.getHours())}:${pad(when.getMinutes())}:${pad(when.getSeconds())}`;
}

/**
 * The instant every row's offset is measured from, or `null` when the feed
 * contains no recorded instant at all.
 *
 * **The earliest recorded instant in the feed**, which is the run's own start
 * whenever the payload carried one: `deriveTimeline` emits that as a `lifecycle`
 * row at `started_at`, and a run starts before anything it does. Where the
 * payload carried no run-start row, this is the first thing anyone observed
 * about the run — the honest available origin, and the one every offset in the
 * feed is then internally consistent with.
 *
 * **It is derived rather than passed in, and that is the conservative choice.**
 * The alternative was a task-start field on the model, filled by both adapters;
 * every source either of them could read is *also* in this list, so the field
 * would have added a second answer to "when did this run begin" without adding
 * a fact. Rows with no recorded instant are skipped rather than counted as
 * epoch, so one untimed event cannot drag the origin to 1970 and make every
 * offset on screen a fifty-six-year number.
 */
export function timelineOrigin(entries: readonly TimelineEntry[]): number | null {
	let earliest: number | null = null;
	for (const entry of entries) {
		if (!recorded(entry.at)) continue;
		if (earliest === null || entry.at < earliest) earliest = entry.at;
	}
	return earliest;
}

/**
 * How far into the run this row happened — `+1m 12s` — or `null` when there is
 * no honest answer.
 *
 * Three absences answer `null`, and none of them answers `+0s`:
 *
 * - **The feed has no origin.** Nothing in it carried an instant, so there is
 *   nothing to be relative to. An offset against a guessed start is worse than
 *   no offset, which is the rule the whole panel is built on.
 * - **This row carried no instant.** Same rule, one row down.
 * - **The row precedes the origin.** Unreachable while the origin is the feed's
 *   own minimum, and stated anyway: the two are separate functions, and a
 *   negative offset rendered as `+` would be a wrong number wearing a correct
 *   shape.
 *
 * `+0s` *does* render for the origin row itself, and it is a measurement rather
 * than a placeholder — that row is where the run began.
 *
 * The `+` is the whole grammar of the column: it says *since*, so the number
 * beside the wall clock cannot be read as a second clock. The duration itself is
 * `durationIfKnown`'s, not a format of its own, so a row's offset, a step's
 * duration and the verdict's age all scale their units the same way.
 */
export function timelineOffset(at: number | null, origin: number | null): string | null {
	if (!recorded(origin) || !recorded(at)) return null;
	const elapsed = (at as number) - (origin as number);
	if (elapsed < 0) return null;
	const rendered = durationIfKnown(elapsed);
	return rendered === null ? null : `+${rendered}`;
}

/**
 * Does this row need a container of its own?
 *
 * **Isolation is content-driven, and the two failure modes are symmetric.** A
 * row carrying prose or a stdout block is several lines tall, and without a
 * bound the reader cannot tell where it ends and its neighbour begins — the
 * defect the owner reported. Boxing every row of a two-hundred-row feed is the
 * same mistake from the other side: a list in which everything is framed is the
 * card grid design §1 blames for nothing being more important than anything
 * else, rebuilt one level down.
 *
 * So: a body or a detail earns the box, and a single-line row stays flat.
 * Deliberately **not** keyed on the cost line — `model · 1.2k → 380 tok` is one
 * short line that reads as part of its title, and framing every LLM call in the
 * feed would frame most of it.
 *
 * A predicate here rather than a truthiness test in the template, because it is
 * the one part of the row's shape a test can hold: jsdom computes no layout, so
 * the border it drives is unobservable in a render, while this is not.
 */
export function timelineIsolated(entry: TimelineEntry): boolean {
	return Boolean(entry.body) || Boolean(entry.detail);
}

/**
 * How close to the bottom the feed still counts as "following", in pixels.
 * A hair over one row's height: a reader who has scrolled up by a whole row has
 * chosen to look at something, and a reader sitting at the end after a sub-pixel
 * layout shift has not.
 */
export const TIMELINE_STICK_THRESHOLD_PX = 48;

/**
 * Should the feed keep following new entries?
 *
 * Pure, over the three scroll metrics, because that is the only part of
 * stick-to-bottom a test can reach: jsdom computes no layout, so `scrollHeight`
 * and `clientHeight` are both zero in every component render and the decision
 * this makes would be unobservable inside one.
 */
export function followsBottom(metrics: {
	scrollTop: number;
	scrollHeight: number;
	clientHeight: number;
}): boolean {
	const distance = metrics.scrollHeight - metrics.scrollTop - metrics.clientHeight;
	return !Number.isFinite(distance) || distance < TIMELINE_STICK_THRESHOLD_PX;
}

// ── the Run card's glance: one dot per step ─────────────────────────────────

/**
 * The four states a dot can be drawn in, plus `unknown` for a row whose status
 * was never recorded — the same honesty `TimelineEntry.status: null` keeps: a
 * dot coloured `done` would claim an outcome the wire never stated.
 */
export type TimelineDotStatus = 'done' | 'failed' | 'running' | 'waiting' | 'unknown';

export interface TimelineDot {
	/** Stable across polls: the row's id, or the delegated child's execution id. */
	key: string;
	status: TimelineDotStatus;
	/** The tooltip: what this step was. */
	label: string;
}

export interface TimelineDotsView {
	/** The newest `limit` dots, oldest first. */
	dots: TimelineDot[];
	/** How many older dots the cap left out — rendered as `+N` ahead of the row. */
	hidden: number;
	/** The accessible sentence for the whole row: `12 events, 1 failed`. */
	summary: string;
}

/** How many dots the collapsed Run card draws before it says `+N`. */
export const TIMELINE_DOT_LIMIT = 24;

/**
 * An `info` row (a run-lifecycle row, a browser observation) happened, and a
 * dot can say no more than that — so it reads `done`. A row with no recorded
 * status is `unknown`, never a guessed outcome.
 */
const DOT_FOR_ENTRY: Record<TimelineEntryStatus, TimelineDotStatus> = {
	done: 'done',
	info: 'done',
	failed: 'failed',
	running: 'running',
	waiting: 'waiting'
};

/**
 * A delegated child's own status word. The wire's field is a free string, so
 * unrecognised words fall through to what the child's rows say.
 */
function delegationDot(status: string, entries: readonly TimelineEntry[]): TimelineDotStatus {
	switch (status.trim().toLowerCase()) {
		case 'running':
		case 'in_progress':
			return 'running';
		case 'failed':
		case 'cancelled':
		case 'canceled':
		case 'error':
			return 'failed';
		case 'completed':
		case 'done':
		case 'finished':
		case 'succeeded':
			return 'done';
		case 'waiting':
		case 'paused':
		case 'needs_action':
			return 'waiting';
	}
	const statuses = entries.map((entry) => (entry.status === null ? 'unknown' : DOT_FOR_ENTRY[entry.status]));
	for (const candidate of ['failed', 'running', 'waiting', 'done'] as const) {
		if (statuses.includes(candidate)) return candidate;
	}
	return 'unknown';
}

/**
 * **A step is one position in the grouped feed** — exactly what the timeline's
 * `Grouped` mode renders as one item: a row of the run's own, or one delegated
 * child folded into its envelope. No new backend semantics: the dots are the
 * grouped feed seen from far away.
 */
export function timelineDots(segments: readonly TimelineSegment[]): TimelineDot[] {
	return segments.map((segment) => {
		if (segment.kind === 'row') {
			const { entry } = segment;
			return {
				key: entry.id,
				status: entry.status === null ? 'unknown' : DOT_FOR_ENTRY[entry.status],
				label: entry.title
			};
		}
		return {
			key: `delegation:${segment.group.execution_id}`,
			status: delegationDot(segment.group.status ?? '', segment.entries),
			label: `Delegated to ${segment.group.agent_id}`
		};
	});
}

const plural = (count: number, one: string, many: string): string =>
	`${count} ${count === 1 ? one : many}`;

/**
 * The capped dot row and the one sentence that stands for it.
 *
 * **The newest dots are kept**, and the `+N` stands for the oldest: the dot a
 * reader of a running task wants is the last one, the one still breathing.
 * The sentence counts **every** dot, not the drawn ones, so a failure the cap
 * pushed off the row is still heard.
 *
 * The noun is `events` (and `delegated runs`), never `steps`: the Run card's
 * header spends `steps` on plan structure, and the two counts would disagree.
 */
export function timelineDotsView(
	segments: readonly TimelineSegment[],
	limit: number = TIMELINE_DOT_LIMIT
): TimelineDotsView {
	const all = timelineDots(segments);
	const safeLimit = Number.isFinite(limit) ? Math.max(1, Math.floor(limit)) : TIMELINE_DOT_LIMIT;
	const hidden = Math.max(0, all.length - safeLimit);

	const rows = segments.filter((segment) => segment.kind === 'row').length;
	const delegated = segments.length - rows;
	const parts: string[] = [];
	const counted: string[] = [];
	if (rows > 0) counted.push(plural(rows, 'event', 'events'));
	if (delegated > 0) counted.push(plural(delegated, 'delegated run', 'delegated runs'));
	if (counted.length > 0) parts.push(counted.join(' and '));
	for (const status of ['failed', 'running', 'waiting'] as const) {
		const count = all.filter((dot) => dot.status === status).length;
		if (count > 0) parts.push(`${count} ${status}`);
	}

	return { dots: all.slice(hidden), hidden, summary: parts.join(', ') };
}

// ── the expanded feed's tail ────────────────────────────────────────────────

/** How many items the expanded Run card shows before `Show all`. */
export const TIMELINE_TAIL = 8;

/**
 * The newest `tail` items of a feed unless the reader asked for all of them,
 * plus how many were left out. A long run opens on its present — the last few
 * things it did — and the history is one control away.
 */
export function timelineTail<T>(
	items: readonly T[],
	showAll: boolean,
	tail: number = TIMELINE_TAIL
): { items: readonly T[]; hidden: number } {
	const safeTail = Number.isFinite(tail) ? Math.max(1, Math.floor(tail)) : TIMELINE_TAIL;
	if (showAll || items.length <= safeTail) return { items, hidden: 0 };
	return { items: items.slice(items.length - safeTail), hidden: items.length - safeTail };
}
