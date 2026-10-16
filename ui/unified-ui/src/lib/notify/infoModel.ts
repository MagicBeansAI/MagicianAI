/**
 * Notify-overlay INFORMATIONAL card model — pure, no I/O.
 *
 * Companion to `cardModel.ts`'s `hitlEventToCard` (actionable HITL cards).
 * Maps the canonical V3 lifecycle events that ride the same
 * `/api/magician/v3/events` NDJSON tail into the informational `NotifyCard`
 * variants (`kind: 'success' | 'error'`):
 *
 *   - errors / failures        → `kind: 'error'`,  auto-dismiss after 15s
 *   - successful completions   → `kind: 'success'`, auto-dismiss after 5s
 *
 * (Incoming chat messages are intentionally NOT surfaced here: the backend's
 * `ChatMessageReceived` fires for ALL messages — the user's own UI messages,
 * assistant replies, and system/status updates — never carrying a distinct
 * `inbound` direction, so it is pure noise as a notification. The valuable
 * "someone is reaching you" case is the actionable guest-request card —
 * `HitlRequested { source: 'user_request' }`, handled by `hitlEventToCard`.)
 *
 * Like `hitlEventToCard`, fields are read ONLY from the flat `data.*` layer of
 * the serialized `RuntimeTransportEvent` (`#[serde(tag = "event_type",
 * content = "data")]` — realtime_events.rs:59-60), never the deeper
 * `payload.*` / `data.event.payload.*` layers. Every variant we read here is a
 * top-level enum variant whose fields land flat under `data.*`:
 *   - `ExecutionFailed { execution_id, error, step_id, ... }`        (Execution, Error)
 *   - `ProcessingError { execution_id, error_message, error_type }`  (Pipeline, Error)
 *   - `AgenticStepFailed { execution_id, step_id, ... }`             (Agentic, Error)
 *   - `AgenticMaxIterationsReached { execution_id, ... }`            (Agentic, Warn)
 *   - `ExecutionCompleted { execution_id, success, steps_total }`    (Execution, Info)
 * (taxonomy: realtime_events.rs:2058/2106/2108/2135/2141/2152/2176).
 *
 * The `deepLink` is the in-app target the later view group hands to the Tauri
 * `open_app_at({ path })` command: the `execution_id` for lifecycle events.
 * The VIEW (a later group) is responsible for the auto-dismiss timer — this
 * model only stamps `dismissAfterMs`, keeping the mapping pure.
 *
 * Stable `id`s are deliberate: a chatty execution re-emitting the same
 * completion / failure (or a backfill replay) collapses to one card via the
 * `coalesceKey` in `coalesce.ts`, which keys info/success/error cards on
 * `${kind}:${deepLink ?? id}`. Picking `exec-fail-<id>` / `exec-done-<id>`
 * keeps both the id AND the coalesce key stable across re-emits.
 */

import { readString, type NotifyCard } from './cardModel';

/** Auto-dismiss windows (ms) the VIEW reads off `dismissAfterMs`. */
const COMPLETION_DISMISS_MS = 5000;
// Errors linger longer than successes (more to read / act on) but still vanish
// so transient failures never permanently bury the persistent approval queue.
const ERROR_DISMISS_MS = 15000;

/**
 * Map a canonical informational event to a `NotifyCard`, or `null` when the
 * event is not one of the informational types we surface (or is malformed,
 * or is a success/outbound case explicitly handled elsewhere).
 *
 * Pairs with `hitlEventToCard`: `notifyStream.applyEvent` tries each event
 * against BOTH mappers, so this one returns `null` for `HitlRequested` /
 * `HitlResolved`.
 */
export function infoEventToCard(event: {
	event_type: string;
	data: Record<string, unknown>;
}): NotifyCard | null {
	const data = event.data ?? {};

	switch (event.event_type) {
		// ── Errors / failures — transient, auto-dismiss 15s ─────────────
		case 'ExecutionFailed': {
			const executionId = readString(data, 'execution_id');
			if (!executionId) return null;
			const message = readString(data, 'error');
			return {
				id: `exec-fail-${executionId}`,
				kind: 'error',
				title: 'Execution failed',
				...(message !== undefined ? { message } : {}),
				deepLink: executionId,
				dismissAfterMs: ERROR_DISMISS_MS
			};
		}

		case 'ProcessingError': {
			const executionId = readString(data, 'execution_id');
			if (!executionId) return null;
			const message = readString(data, 'error_message');
			return {
				id: `exec-fail-${executionId}`,
				kind: 'error',
				title: 'Processing error',
				...(message !== undefined ? { message } : {}),
				deepLink: executionId,
				dismissAfterMs: ERROR_DISMISS_MS
			};
		}

		case 'AgenticStepFailed': {
			const executionId = readString(data, 'execution_id');
			if (!executionId) return null;
			return {
				id: `exec-fail-${executionId}`,
				kind: 'error',
				title: 'Step failed',
				deepLink: executionId,
				dismissAfterMs: ERROR_DISMISS_MS
			};
		}

		case 'AgenticMaxIterationsReached': {
			const executionId = readString(data, 'execution_id');
			if (!executionId) return null;
			return {
				id: `exec-fail-${executionId}`,
				kind: 'error',
				title: 'Max iterations reached',
				deepLink: executionId,
				dismissAfterMs: ERROR_DISMISS_MS
			};
		}

		// ── Completions — success only, auto-dismiss 5s ─────────────────
		case 'ExecutionCompleted': {
			// `ExecutionFailed` covers the failure path; a `success: false`
			// completion would otherwise show a misleading "Completed" card.
			if (data.success !== true) return null;
			const executionId = readString(data, 'execution_id');
			if (!executionId) return null;
			return {
				id: `exec-done-${executionId}`,
				kind: 'success',
				title: 'Completed',
				deepLink: executionId,
				dismissAfterMs: COMPLETION_DISMISS_MS
			};
		}

		default:
			return null;
	}
}
