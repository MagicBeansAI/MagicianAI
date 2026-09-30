/**
 * What a task's current run is doing, over the wire — the ask blocking it, and
 * the log of what it has done.
 *
 * **This used to fetch the whole payload and keep one field.** The note below
 * called that "the cost, stated": `/execution-panel` also builds an event log,
 * observations, shell entries, a taskplan and a run tree, and only
 * `run.needs_attention` was read. The Run act's timeline needs exactly the part
 * that was being thrown away, so the four task surfaces got one for no extra
 * request — the response was already on the wire, already parsed, and already
 * discarded. What changed here is a return type; what did not change is the
 * number of requests.
 *
 * The two things the caller reads out of it are derived by `toTaskPanelModel`,
 * not here, because only the adapter knows which run its Run act names — see
 * `deriveTimeline` for why that matters.
 *
 * **Why this exists.** A task blocked mid-run — on an approval, a diff review,
 * an escalation, a bot sign-in, or any agentic pause — reaches the task list as
 * status `paused`, which now truthfully reads `Paused` but still cannot explain
 * that the task is waiting on the reader. `pending_questions` do not help: they
 * are *plan-time* clarifications and a mid-run block carries none. The row can
 * now name exactly one of those blocks — `awaiting_diff_approval`, added after
 * this file was written — which is one of the eight sources `KNOWN_SOURCES`
 * below models. The other seven still reach the list as an unexplained pause,
 * and none of the eight, the diff included, arrive with anything the reader can
 * answer from the row.
 *
 * **Why `/execution-panel` and not the alternatives.** Three sources were
 * available and only one of them is both complete and reachable from the client:
 *
 * - **`pendingHitlStore`** — free and live, but its entries key on
 *   `correlation_id` and `chat_turn_id` and carry **no task id**, so nothing can
 *   ask it about one task. The raw event it keeps sometimes carries a
 *   `scope.task_id`, which makes the answer depend on which adapter wrote the
 *   event: a per-task question answered "usually".
 * - **`/feed/attention`** — the store the attention centre reads, whose items do
 *   carry `task_id`. It is scope-wide and **paginated at 25 rows per lane**, so
 *   a task blocked below the fold reports no ask at all. A verdict that depends
 *   on a task's position in someone else's list is worse than a missing one,
 *   because it is right often enough to be trusted.
 * - **a field on the task-list row** — the shape that fixes the list's chips
 *   too. This used to read "`TaskListItemV3` has no such field today, so it is
 *   a backend change and out of reach here." **That is no longer true**:
 *   `TaskListItemV3.awaiting_diff_approval` exists, the list rows render it,
 *   and `Task.awaitingDiffApproval` carries it through the store. It does not
 *   replace this fetch and was never going to. It answers exactly one
 *   question — *is a staged diff waiting?* — for the one ask whose answer can
 *   be recomputed from a directory. The panel has to name **which** ask, quote
 *   its prompt, say when it was raised, and post a response against its id;
 *   none of that is on the row, and six of the eight `KNOWN_SOURCES` below
 *   have no row field at all. A boolean is a chip, not a control.
 *
 * `GET /v3/tasks/{id}/execution-panel` is what the retired panel used. Its
 * `run.needs_attention` is built server-side from the full attention list
 * filtered to this task, so it is complete for the task asked about, and each
 * item carries the canonical `hitl_request` — a real `HitlSource` and a real
 * instant, rather than the routing guesses the two existing attentions in this
 * directory have to make.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`.
 */

import { normalizeHitlInputType } from '$lib/hitl/adapters';
import type { HitlOpenTarget, HitlSource } from '$lib/hitl/types';
import { timedFetch } from '$lib/shared/fetch';
import type { ExecutionPanelAttentionItem, ExecutionPanelState } from '$lib/types/executionPanel';

import type { VerdictAttention } from './taskVerdict';

const API_BASE = '/api/magician/v3';

/**
 * The eight sources `VerdictAttention` models, as a set rather than a cast.
 * The payload is JSON and its `source` is a string; accepting it unchecked
 * would let an unknown one reach `ATTENTION_COPY`, whose lookup would answer
 * `undefined` and render the detail line empty. A source this client does not
 * model is not an ask it can describe, so the item is skipped and the verdict
 * falls back to the task's own status.
 */
const KNOWN_SOURCES = new Set<string>([
	'agentic',
	'user_request',
	'approval',
	'plan_approval',
	'clarification',
	'escalation',
	'diff_approval',
	'bot_auth'
]);

function isHitlSource(value: unknown): value is HitlSource {
	return typeof value === 'string' && KNOWN_SOURCES.has(value);
}

/** A non-empty trimmed string, or `null`. Absence and blank are one answer. */
function text(raw: unknown): string | null {
	if (typeof raw !== 'string') return null;
	const trimmed = raw.trim();
	return trimmed ? trimmed : null;
}

/** A finite epoch-millis number, or `null`. Zero is a sentinel, not an instant. */
function instant(raw: unknown): number | null {
	return typeof raw === 'number' && Number.isFinite(raw) && raw !== 0 ? raw : null;
}

/**
 * An ask, and the thing that answers it.
 *
 * **One value with two fields rather than two values**, and that is the whole
 * point of the shape: the verdict says *what* is being asked and the control
 * posts an answer to *some* ask, and if those are derived separately they can
 * come from different rows — a panel reading `Waiting on you — which quarter?`
 * over a button that approves a diff. Every value on screen would be individually
 * correct. So both come off one row, in one function, or neither does.
 *
 * `ask` is `null` when the row that produced the attention published no target
 * this client can use. The verdict still reads `Waiting on you`, because the ask
 * is real; there is simply no control, which is the honest rendering of "we can
 * tell you about it and cannot answer it from here".
 */
export interface TaskAsk {
	attention: VerdictAttention;
	ask: HitlOpenTarget | null;
}

/**
 * A wire `HitlOpenTarget` this client can actually render and answer, or `null`.
 *
 * **The declared type is not a check.** `ExecutionPanelAttentionItem.hitl_request`
 * is typed `HitlOpenTarget`, whose `source` and `input_type` are TypeScript
 * unions over fields that arrive as bare JSON strings — the backend builds them
 * from `UserInputType::type_name()`, and a payload carrying a twelfth spelling
 * type-checks all the way to a `Record` lookup that answers `undefined`. So both
 * are narrowed here, through the same normalisers the rest of the HITL layer
 * uses rather than a second copy of either list.
 *
 * An id and a prompt are required for the same reason they are required at the
 * far end: the id is what the response is posted against, and a control under a
 * blank prompt asks nothing.
 */
export function askTargetFrom(raw: HitlOpenTarget | null | undefined): HitlOpenTarget | null {
	if (!raw) return null;
	const inputType = normalizeHitlInputType(raw.input_type);
	const id = text(raw.id);
	const prompt = text(raw.prompt);
	if (!isHitlSource(raw.source) || !inputType || !id || !prompt) return null;
	return { ...raw, id, source: raw.source, input_type: inputType, prompt };
}

/**
 * The first attention item that is actually an ask, as a `TaskAsk`.
 *
 * **The filter is `hitl_request`, and that is the whole rule.** The attention
 * list a task carries is wider than "a human must answer": it also holds
 * informational rows, notably terminal execution failures. Ranking one of those
 * as `waiting` would paint a **failed** task `Waiting on you` and hide the error
 * message — a louder lie than the `Queued` this exists to fix. The backend
 * embeds a canonical `hitl_request` on exactly the rows that can be responded
 * to, so its presence is the signal, and the fallback of reading
 * `metadata.attention_kind` and enumerating the informational ones is a second
 * copy of a taxonomy that already reached us as data.
 *
 * Exported for its own test: the shapes it has to reject are the point, and a
 * test that could only reach it through `fetch` would be testing the mock.
 */
export function runAttentionFrom(items: readonly ExecutionPanelAttentionItem[]): TaskAsk | null {
	for (const item of items) {
		const request = item?.hitl_request;
		if (!request || !isHitlSource(request.source)) continue;
		return {
			attention: {
				source: request.source,
				// The prompt is the ask in the words the backend chose; the feed item's
				// title is a lane label (`Approval requested`) and says less. Falling
				// back to it rather than to `ATTENTION_COPY` would substitute a generic
				// sentence for a generic sentence, so `null` is left for
				// `deriveVerdict` to fill from the source it already models.
				summary: text(request.prompt) ?? text(item.summary),
				// The request's own instant first: `created_at` is when the *feed row*
				// was written, which is the same moment often enough to be tempting and
				// not always. Both are omitted rather than approximated when neither is
				// readable, and the headline then reads `Waiting on you` with no
				// duration (design §5).
				raisedAt: instant(request.at) ?? instant(item.created_at)
			},
			// **The rest of the row, which used to be dropped here.** This function
			// read `hitl_request` and kept three of its fields, discarding the id,
			// the input type, the schema, the identifiers and the scope — that is,
			// everything needed to answer the ask it had just described. Nothing new
			// is fetched: the payload was already on the wire, already parsed, and
			// already thrown away, exactly as the Run act's timeline was.
			ask: askTargetFrom(request)
		};
	}
	return null;
}

function scopedQuery(
	_principal: string,
	_workspace: string,
	executionId: string | null = null
): string {
	const params = new URLSearchParams();
	// **The one parameter that changes which run the response describes.** Omitted
	// rather than sent empty when there is no choice to express: the handler's
	// `execution_id` is an `Option<String>` and its `None` branch resolves the
	// task's active-then-latest-then-last-completed root run, which is the same
	// answer this panel wants by default and one no client should restate.
	if (executionId !== null && executionId.trim()) {
		params.set('execution_id', executionId.trim());
	}
	const query = params.toString();
	return query ? `?${query}` : '';
}

/**
 * What this task's current run is doing, or `null` when we could not find out.
 *
 * **`null` is "we did not learn anything about this run", and it stays a single
 * value** rather than splitting into an unread ask and an unread log. Both of
 * the things the adapter reads out of this degrade to absence: an unfetched ask
 * renders nothing at all and the verdict falls back to exactly what the task's
 * own status earns, and an unfetched log renders no timeline rather than
 * `0 events`. Neither can assert anything, so there is nothing a second spelling
 * of absence would distinguish. That is the opposite of the outputs request
 * beside it, whose `[]` *does* claim something — `no output` — and therefore has
 * to stay distinguishable from `null`.
 *
 * A body without `overview.status` is a failure for the reason
 * `fetchExecutionPanelState` gives: it is not a panel state, however well-formed
 * the JSON was, and every field read out of it would be read out of something
 * else.
 *
 * `executionId` is **which** of the task's runs to describe, or `null` for the
 * one the task record points at.
 *
 * **The route already took it and nothing sent it.** `TaskExecutionPanelQuery`
 * has carried an optional `execution_id` since the panel this one replaced, and
 * `get_task_panel_state` threads it straight into the run's own event log,
 * taskplan, responsibility snapshot and output result while leaving
 * `overview.status` read off the **task** record. That split is the whole reason
 * the run picker needed no backend work and no second endpoint: the task-scoped
 * route with an execution pinned answers about one run *without* forgetting which
 * task it belongs to, which is exactly what a panel showing a task-level verdict
 * over an execution-scoped act has to have.
 *
 * The execution-scoped route — `/v3/executions/{id}/execution-panel` — is
 * deliberately **not** what this uses, and `executionPanelUrl` explains why one
 * file over: it is not populated for task-backed delegate runs, and the failure
 * is a 404 rather than a thinner payload.
 */
export async function fetchTaskRunState(
	taskId: string,
	principal: string,
	workspace: string,
	executionId: string | null = null
): Promise<ExecutionPanelState | null> {
	const result = await readTaskRunState(taskId, principal, workspace, executionId);
	return result.ok ? result.state : null;
}

/**
 * The same read, with the one distinction a **poller** cannot do without: did the
 * endpoint answer, or did the request fail?
 *
 * `fetchTaskRunState` above collapses both onto `null`, and for a one-shot load
 * that is right — every slice derived from the payload renders absent either way,
 * so nothing on screen could use the difference. A poll can: design §6 forbids
 * rendering stale state as current, and the staleness line is what a *failed
 * refresh* earns. Collapsed, a task that has simply never run would claim to be
 * reconnecting forever, and a backend that had gone away would claim nothing at
 * all — each one wearing the other's answer.
 *
 * So the split is drawn where the wire draws it:
 *
 * | what came back | answer |
 * |---|---|
 * | a panel state | `{ ok: true, state }` |
 * | `404` | `{ ok: true, state: null }` — the endpoint answered: this task has no run to describe |
 * | any other non-OK, or a thrown request | `{ ok: false, reason }` |
 * | `200` with a body that is not a panel state | `{ ok: true, state: null }` |
 *
 * The last row is the arguable one and it is deliberate: a well-formed body
 * without `overview.status` is not a panel state, but the request *did* complete,
 * and a backoff aimed at a backend that is answering promptly would be aimed at
 * the wrong thing. It renders as absence, exactly as it did before this split
 * existed.
 */
export type TaskRunStateResult =
	| { ok: true; state: ExecutionPanelState | null }
	| { ok: false; reason: string };

export async function readTaskRunState(
	taskId: string,
	principal: string,
	workspace: string,
	executionId: string | null = null
): Promise<TaskRunStateResult> {
	try {
		const response = await timedFetch(
			`${API_BASE}/tasks/${encodeURIComponent(taskId)}/execution-panel${scopedQuery(principal, workspace, executionId)}`,
			{ headers: { Accept: 'application/json' } }
		);
		if (response.status === 404) return { ok: true, state: null };
		if (!response.ok) return { ok: false, reason: `Couldn't read this run (HTTP ${response.status})` };
		const payload = await response.json();
		if (!payload?.overview?.status) return { ok: true, state: null };
		return { ok: true, state: payload as ExecutionPanelState };
	} catch (error) {
		return {
			ok: false,
			reason: error instanceof Error && error.message.trim()
				? error.message.trim()
				: "Couldn't reach the server"
		};
	}
}
