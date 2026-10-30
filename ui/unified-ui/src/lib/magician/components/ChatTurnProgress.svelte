<script lang="ts">
	import { contextPreparationRow } from './contextPreparation';
	import { llmStepUsageParts } from './llmStepUsage';
	import { recordTaskUsageEvent } from '$lib/magician/chat/next/taskUsage';
	/**
	 * In-flight assistant-turn progress bubble.
	 *
	 * Mounted while a chat send is in flight (`isSendingMessage` truthy)
	 * in place of the old plain "typing dots" placeholder. Subscribes to
	 * the app's event hub (one socket for the whole app; history is one
	 * `/api/magician/v3/events?…&backfill_only=true` request), filters to
	 * chat-relevant event types, and renders each as a one-line progress entry inside
	 * the assistant bubble.
	 *
	 * Coalescing rules — one row per logical operation, not per event:
	 *   • `reasoning.*` events keyed by `trace_id` — `.start` opens the
	 *     row, `.content` deltas append into the same row's detail,
	 *     `.end` freezes it.
	 *   • `tool.call.*` and `tool.*` events keyed by `call_id` — label
	 *     transitions `Calling X` → `X returned (Nms)` / `X failed`. A
	 *     delegated run's typed `AgenticActionStarted` / `AgenticActionExecuted`
	 *     map onto the same rows, keyed by their shared `call_id`.
	 *   • `llm.*` lifecycle keyed by `trace_id` (or `iteration` if
	 *     present) — one row per LLM call brackets request → response.
	 *     A delegated run's typed `LLMRequestSent` / `LLMRouted` /
	 *     `LLMResponseReceived` share `correlation.llm_call_id` and fill ONE
	 *     row: running "Thinking…" when sent, "Thinking with <model>" once
	 *     routed, settled in place. A chat-side `llm.*` event naming the same
	 *     `llm_call_id` lands on that row too.
	 *   • A running row shows how long it has been running, off the shared
	 *     1 s clock (`liveClock`), until it settles.
	 *   • `step.*` keyed by `step_id`; everything else (`artifact.*`,
	 *     `output.*`, `agentic.iteration_started`) opens a fresh row.
	 *
	 * HITL pause indicator — when an `input.requested` / `HitlRequested`
	 * / `waiting_for_confirmation` / `execution.waiting_for_user` /
	 * `clarification.queued` event fires, the bubble header flips to
	 * "Waiting on you" with an accent border so the operator notices
	 * they need to act.
	 *
	 * Closes the subscription on destroy / when the parent unmounts.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { get } from 'svelte/store';
	import {
		captureCompletedTurnActivity,
		type ChatTurnActivityRow
	} from '$lib/stores/chatTurnActivityStore';
	import { eventDedupeKey } from '$lib/stores/chatTurnEventsStore';
	import { timedFetch } from '$lib/shared/fetch';
	import { parseNdjsonFrames, subscribeWithBackfill } from '$lib/realtime/eventHub';
	import TurnStage from '$lib/magician/chat/next/TurnStage.svelte';
	import { liveClock, runningElapsed } from '$lib/magician/chat/next/liveClock';
	import {
		answeredModelLabel,
		llmCallIdOf,
		requestedModelLabel,
		routedModelLabel,
		THINKING_LABEL
	} from './llmModelLabel';
	import {
		agenticActionAsToolEvent,
		describeToolCall,
		toolRowName,
		toStageRows,
		type StageRow
	} from '$lib/magician/chat/next/stageRows';

	export let executionId: string | null = null;
	export let taskId: string | null = null;
	/**
	 * Chat session id this turn belongs to. Optional — when set, the rows
	 * accumulated during the turn are captured into
	 * `chatTurnActivityStore` on unmount so the final assistant message
	 * can render them as a collapsible "What happened" section.
	 */
	export let sessionId: string | null = null;
	/**
	 * How many progress rows render at once. Default `5` — small but
	 * deep enough to show the LLM-call → reasoning → tool-call lifecycle
	 * without each step instantly overwriting the previous one. The list
	 * rolls: older rows slide off the top as newer rows append at the
	 * bottom (latest-last), via `slice(-visibleRows)` in
	 * `computeVisibleRows`. Lower to `1` for a Claude-Code-style
	 * "current action only" view; raise for a long activity tail. The
	 * full accumulated set still captures into `chatTurnActivityStore`
	 * on unmount so the assistant message's `▸ What happened` dropdown
	 * has the complete trace.
	 */
	export let visibleRows = 5;
	/**
	 * Presentation only. `'next'` renders the chat-next `TurnStage`
	 * (working chip + settled dots) over ALL rows instead of the classic
	 * rolling list; the row model, subscription and settling are shared.
	 */
	export let variant: 'classic' | 'next' = 'classic';
	/**
	 * Presentation only. Drops the chat-bubble chrome ("Assistant" header,
	 * bubble surface, "Working…" head) for hosts that already frame the
	 * progress, e.g. the chat-next task progress panel. A HITL pause still
	 * shows, as a compact "Waiting on you" line.
	 */
	export let bare = false;

	type RowKind = 'llm' | 'reasoning' | 'tool' | 'step' | 'artifact' | 'pause';
	type RowStatus = 'running' | 'done' | 'failed' | 'waiting';
	type RowTone = 'info' | 'tool' | 'error' | 'reasoning' | 'pause';

	interface ProgressRow {
		key: string;
		kind: RowKind;
		label: string;
		detail: string | null;
		status: RowStatus;
		tone: RowTone;
		startedAt: number;
		durationMs: number | null;
	}

	/** Sentinel for "this row is in the order list" — we maintain the
	 *  ordering separately so updates-in-place don't shuffle the visual
	 *  position. */
	const rowsByKey = new Map<string, ProgressRow>();
	const orderedKeys: string[] = [];
	const terminalTutorRunIds = new Set<string>();
	/** Presentation-only side map for the `next` variant: the tool name and
	 *  a short target (host / file basename) of each `tool:<call_id>` row,
	 *  read from the tool event payload. Never feeds row status. */
	const toolMetaByKey = new Map<string, { tool: string; target?: string }>();
	/** llm_call_id → the row key that call lives under, so the chat-side
	 *  `llm.*` copy and the typed copies of one call share a row. */
	const llmKeyByCallId = new Map<string, string>();
	let version = 0;
	let pauseActive = false;
	let liveUnsub: (() => void) | null = null;
	/** Dedup so a backfill/resync replay doesn't re-apply events we already
	 *  have (same key the events store uses). Lets the subscription safely
	 *  replay the per-turn history on mount and on every hub resync. */
	const seenEventKeys = new Set<string>();
	/** Set once a turn-level terminal event is applied — marks the timeline
	 *  settled. */
	let turnTerminal = false;

	$: scope = get(scopeIdentityStore);
	$: rows = bumpVersion(version, computeVisibleRows());
	$: stageRows = variant === 'next' ? bumpVersion(version, computeStageRows()) : [];

	function bumpVersion<T>(_: number, value: T): T {
		return value;
	}

	// The classic list's running rows tick off the shared 1 s clock, held only
	// while one is running (the next variant's TurnStage holds its own).
	let now = Date.now();
	const clock = liveClock((tick) => (now = tick));
	$: clock.sync(variant !== 'next' && rows.some((row) => row.status === 'running'));

	function computeVisibleRows(): ProgressRow[] {
		const all = orderedKeys
			.map((key) => rowsByKey.get(key))
			.filter((row): row is ProgressRow => row !== undefined);
		return all.slice(-visibleRows);
	}

	function computeStageRows(): StageRow[] {
		const all = orderedKeys
			.map((key) => rowsByKey.get(key))
			.filter((row): row is ProgressRow => row !== undefined);
		return toStageRows(all, toolMetaByKey, (row) =>
			row.kind === 'artifact' && row.detail ? row.detail : undefined
		);
	}

	function upsertRow(row: ProgressRow): void {
		if (!rowsByKey.has(row.key)) {
			orderedKeys.push(row.key);
		}
		rowsByKey.set(row.key, row);
		version += 1;
	}

	function patchRow(key: string, patch: Partial<ProgressRow>): void {
		const existing = rowsByKey.get(key);
		if (!existing) return;
		rowsByKey.set(key, { ...existing, ...patch });
		version += 1;
	}

	function dropRow(key: string): void {
		if (!rowsByKey.delete(key)) return;
		const index = orderedKeys.indexOf(key);
		if (index !== -1) orderedKeys.splice(index, 1);
		version += 1;
	}

	/**
	 * The row key for a chat-side `llm.*` event: the trace/iteration key, or,
	 * when the event names an `llm_call_id`, the row that call already has. A
	 * typed copy that opened its own row before an id-less `requested`'s
	 * settle named the id is folded into the trace row.
	 */
	function chatLlmKey(
		eventType: string,
		payload: Record<string, unknown> | undefined,
		traceKey: string
	): string {
		const callId = llmCallIdOf(payload);
		if (!callId) return traceKey;
		const aliased = llmKeyByCallId.get(callId);
		let key: string;
		if (eventType === 'llm.requested') {
			key = aliased ?? `llm:call:${callId}`;
		} else if (aliased && aliased !== traceKey && rowsByKey.get(traceKey)?.status === 'running') {
			dropRow(aliased);
			key = traceKey;
		} else {
			key = aliased ?? traceKey;
		}
		// Only a key that holds (or is about to hold) this call's row: a settle
		// that patches nothing must not send the typed copy onto a shared key.
		if (eventType === 'llm.requested' || rowsByKey.has(key)) llmKeyByCallId.set(callId, key);
		return key;
	}

	/** Settle a tool row. An agentic step's finish can land without its start
	 *  (a legacy run emits no `AgenticActionStarted`, or the start is in an
	 *  older page), so it opens the row itself; a chat tool's finish only
	 *  patches the row its start opened. */
	function settleToolRow(
		key: string,
		ts: number,
		upsertIfMissing: boolean,
		patch: Pick<ProgressRow, 'label' | 'status' | 'tone' | 'detail'> & Partial<ProgressRow>
	): void {
		if (rowsByKey.has(key) || !upsertIfMissing) {
			patchRow(key, patch);
			return;
		}
		upsertRow({ key, kind: 'tool', startedAt: ts, durationMs: null, ...patch });
	}

	/** Key prefix of the "Writing the final answer…" row (`synth:<task_id>`):
	 *  the work is done but the answer is still being written, so it outlives
	 *  the execution-level terminal events and settles on the task's own. */
	const SYNTHESIS_ROW_PREFIX = 'synth:';

	/** Settle every still-'running' row to a terminal status. Called when the
	 *  turn/task terminates: the backend emits NO `agentic.iteration_completed`,
	 *  and a step/tool completion event can be dropped or key-mismatched, so rows
	 *  get stranded as 'running' (spinner) after the turn is over. */
	function closeAllRunningRows(
		status: 'done' | 'failed',
		options: { keepSynthesis?: boolean } = {}
	): void {
		let changed = false;
		for (const [key, row] of rowsByKey.entries()) {
			if (options.keepSynthesis && key.startsWith(SYNTHESIS_ROW_PREFIX)) continue;
			if (row.status === 'running') {
				rowsByKey.set(key, { ...row, status });
				changed = true;
			}
		}
		if (changed) version += 1;
	}

	/** Settle one task's "Writing the final answer…" row, if it is running. */
	function settleSynthesisRow(rowTaskId: string, status: 'done' | 'failed'): void {
		const key = `${SYNTHESIS_ROW_PREFIX}${rowTaskId}`;
		const row = rowsByKey.get(key);
		if (row?.status === 'running') {
			rowsByKey.set(key, { ...row, status });
			version += 1;
		}
	}

	async function notifyTutorOverlayStatusFromProgress(
		status: 'working' | 'idle',
		payload?: Record<string, unknown>
	): Promise<void> {
		if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return;
		const payloadSessionId =
			typeof payload?.chat_session_id === 'string'
				? payload.chat_session_id
				: typeof payload?.session_id === 'string'
					? payload.session_id
					: sessionId;
		if (!payloadSessionId) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('show_tutor_overlay_status', {
				status,
				sessionId: payloadSessionId
			});
		} catch (error) {
			console.warn('[ChatTurnProgress] tutor overlay status update failed:', error);
		}
	}

	function handleEvent(parsed: Record<string, unknown>): void {
		recordTaskUsageEvent(parsed, taskId ?? undefined);
		const outerType = String(parsed.event_type ?? '');
		if (!outerType || outerType.startsWith('__events_')) return;

		// Idempotency: skip an event we've already applied. A page refresh
		// (fresh mount) and a mid-turn reconnect both re-replay the full
		// per-turn projection; dedup lets us rebuild/settle rows from the
		// backfill without double-counting or duplicating distinct rows.
		const dedupeKey = eventDedupeKey(parsed);
		if (dedupeKey) {
			if (seenEventKeys.has(dedupeKey)) return;
			seenEventKeys.add(dedupeKey);
		}

		// Unwrap AgentEvent envelope so the inner event_type is what we
		// classify on. Mirrors backend `unwrap_event_type` and frontend
		// dedup logic in `EventStreamCard`.
		let eventType = outerType;
		let payload: Record<string, unknown> | undefined;
		let envelopeAgentId: string | undefined;
		const data = parsed.data as Record<string, unknown> | undefined;
		if (outerType === 'AgentEvent' && data) {
			const inner = data.event as Record<string, unknown> | undefined;
			if (inner && typeof inner.event_type === 'string') {
				eventType = inner.event_type;
				payload = inner.payload as Record<string, unknown> | undefined;
				envelopeAgentId =
					typeof inner.agent_id === 'string' ? inner.agent_id : undefined;
			}
		} else if (data) {
			payload = data;
			envelopeAgentId =
				typeof data.agent_id === 'string' ? (data.agent_id as string) : undefined;
		}

		// A delegated/agentic run's tool steps arrive as TYPED transport events
		// (`AgenticActionStarted` when an action begins, `AgenticActionExecuted`
		// when it settles), not `tool.call.*`. Map them onto the tool branch so
		// the live task card shows each step the moment it starts; the shared
		// `call_id` lets the finish settle the same row.
		const agentic = payload ? agenticActionAsToolEvent(outerType, payload) : null;
		if (agentic) {
			eventType = agentic.eventType;
			payload = agentic.payload;
		}

		// A task-scoped bubble (the live task card's activity) shows only its own
		// task's work. `task_id`/`execution_id` bound the REST backfill alone;
		// the hub offers every event in the scope, so without this the card
		// streamed other tasks' steps.
		if (taskId && !eventBelongsToTask(parsed, payload, taskId, executionId)) return;

		// Debug aid: console-log every event the bubble sees, so an
		// operator inspecting DevTools can tell whether the live
		// subscription is delivering events at all. Filter the console
		// noise via DevTools' built-in filter on "[ChatTurnProgress]".
		// Safe to leave on — chat turns emit dozens, not thousands.
		console.debug(
			'[ChatTurnProgress] event:',
			eventType,
			'outer:',
			outerType,
			'agent:',
			envelopeAgentId ?? null,
			'payload-tool:',
			(payload as Record<string, unknown> | undefined)?.tool_name ?? null,
		);

		// Build an `[<agent>]` prefix to attach to row labels when the
		// event came from a delegated/handed-over sub-agent. The chat
		// fanout overlay routes those sub-agent events into this
		// subscription, but without an agent prefix every row says e.g.
		// just "Calling search" — the operator can't tell which agent
		// is doing it.
		//
		// Fanout-aware: when the backend's chat-fanout emits a re-stamped
		// copy (see realtime_events.rs:2870-3026), it sets the envelope's
		// `agent_id` to the chat agent (e.g. `personal-assistant`) so the
		// chat-scoped subscription picks the event up, and preserves the
		// true emitter under `payload.origin_agent_id`. Prefer
		// `origin_agent_id` for the prefix so the primary emit AND the
		// fanout copy of the same logical event both render with the
		// real delegate's name — without this, the user sees every step
		// twice: once as `[personal-assistant]` and once as
		// `[simple-data-analyst]`.
		//
		// Skip prefixing when:
		//   - no agent_id on either source
		//   - `__system__` (system-scoped events; not informative)
		//   - looks like a synthetic task-id agent_id (internal execution
		//     contexts emit events with `ctx.task_id` as agent_id —
		//     starts with `task_` and is a long hash, not an agent name)
		const originAgentId =
			typeof payload?.origin_agent_id === 'string'
				? (payload.origin_agent_id as string)
				: undefined;
		const effectiveAgentId = originAgentId ?? envelopeAgentId;
		const skipPrefix =
			!effectiveAgentId
			|| effectiveAgentId === '__system__'
			|| effectiveAgentId.startsWith('task_')
			|| effectiveAgentId.startsWith('exec_')
			|| effectiveAgentId.startsWith('cycle_');
		const agentPrefix = skipPrefix ? '' : `[${effectiveAgentId}] `;

		const ts = extractTimestamp(parsed);
		const preparation = contextPreparationRow(eventType, payload);
		if (preparation) {
			if (!taskId && (!sessionId || payload?.chat_session_id !== sessionId)) return;
			const existing = rowsByKey.get(preparation.key);
			if (preparation.status === 'running' && existing) return;
			upsertRow({ ...preparation, kind: 'step', tone: 'info',
				label: `${agentPrefix}${preparation.label}`, startedAt: existing?.startedAt ?? ts });
			return;
		}

		// ─── Learned Task Recipe lifecycle ─────────────────────────────
		// One canonical event type carries transition-specific payload.kind.
		// Coalesce the sequence into a single, low-noise task cue.
		if (eventType === 'recipe.replay') {
			const kind = String(payload?.kind ?? '');
			// Every lifecycle event is scoped to the task execution, while only
			// some payloads carry recipe_id. Prefer executionId so start, heal,
			// fallback, and completion always coalesce into the same row.
			const key = `recipe:${String(executionId ?? payload?.recipe_id ?? 'active')}`;
			if (kind === 'recipe.replay.started') {
				upsertRow({
					key,
					kind: 'tool',
					label: `${agentPrefix}Using a learned API recipe`,
					detail: `v${String(payload?.version ?? '?')} · no browser`,
					status: 'running',
					tone: 'tool',
					startedAt: ts,
					durationMs: null
				});
			} else if (kind === 'recipe.replay.auth.healed') {
				patchRow(key, { detail: 'Session refreshed automatically' });
			} else if (kind === 'recipe.replay.transport.downgraded') {
				patchRow(key, { detail: `Continued with ${String(payload?.to ?? 'in-page fetch')}` });
			} else if (kind === 'recipe.replay.completed') {
				patchRow(key, {
					label: `${agentPrefix}Answered from the learned API recipe`,
					detail: 'No browser was opened',
					status: 'done',
					durationMs: Number(payload?.duration_ms ?? 0)
				});
			} else if (kind === 'recipe.replay.fallback.handoff') {
				patchRow(key, {
					label: `${agentPrefix}API recipe handed off to the browser`,
					detail: `${String(payload?.replayed_steps ?? 0)} API steps completed · ${String(payload?.class ?? 'fallback')}`,
					status: 'done',
					tone: 'tool'
				});
			} else if (kind === 'recipe.replay.recompiled') {
				upsertRow({
					key: `${key}:recompiled:${String(payload?.version ?? '?')}`,
					kind: 'tool',
					label: `${agentPrefix}Browser taught the API recipe what changed`,
					detail: `Recipe v${String(payload?.version ?? '?')} will run next time`,
					status: 'done',
					tone: 'tool',
					startedAt: ts,
					durationMs: null
				});
			}
			return;
		}

		// ─── Terminal turn/execution: settle any still-running rows ──────
		// Fixes the spinner that never stops after a turn is over. The backend
		// emits NO `agentic.iteration_completed`, and step/tool completion events
		// can be dropped or key-mismatched, so rows get stranded as 'running'.
		// `task.status_changed` is turn/task-level (unambiguously "all done").
		// Scope the faster execution.* signal to THIS card's top-level
		// `executionId` so a sub-agent's completion doesn't prematurely close the
		// parent's still-running rows.
		// A delegated or chat-spawned task reports the same status on
		// `chat.delegate.status_changed`; it carries the same fields.
		const delegateTaskId = String(payload?.task_id ?? '');
		// A delegate settling inside a wider turn (several delegates, no
		// card task) settles only its own answer row, never its siblings'.
		const otherDelegate =
			eventType.startsWith('chat.delegate.') && (!taskId || delegateTaskId !== taskId);
		// The final answer's current step (writing, checking against the
		// evidence, revising): the same row, relabelled, its clock kept. A
		// card not bound to this task only relabels a row it already shows.
		if (eventType === 'task.answer_progress') {
			const answerTaskId = String(payload?.task_id ?? taskId ?? 'active');
			const synthKey = `${SYNTHESIS_ROW_PREFIX}${answerTaskId}`;
			const existing = rowsByKey.get(synthKey);
			if (!existing && (!taskId || answerTaskId !== taskId)) return;
			if (existing && existing.status !== 'running') return;
			const label = typeof payload?.label === 'string' ? payload.label : 'Writing the final answer…';
			upsertRow({
				key: synthKey,
				kind: 'llm',
				label: `${agentPrefix}${label}`,
				detail: null,
				status: 'running',
				tone: 'info',
				startedAt: existing?.startedAt ?? ts,
				durationMs: null
			});
			return;
		}
		if (eventType === 'task.status_changed' || eventType === 'chat.delegate.status_changed') {
			const st = String(payload?.status ?? '');
			// The work has finished but the final answer is still being written
			// (`synthesis_pending`, typically 10 s to 2 min). The execution.*
			// events below close every other row at once, so this one row is
			// what shows the turn is still busy. The task's own terminal
			// status closes it (a synthesis failure arrives there too).
			// A `completed` carrying the flag is still synthesizing (the card's
			// own leaf reads it the same way); only failed/cancelled win over it.
			if (payload?.synthesis_pending === true && st !== 'failed' && st !== 'cancelled') {
				const synthKey = `${SYNTHESIS_ROW_PREFIX}${String(payload?.task_id ?? taskId ?? 'active')}`;
				// A repeated flagged event keeps the clock the first one started.
				if (rowsByKey.get(synthKey)?.status !== 'running') {
					upsertRow({
						key: synthKey,
						kind: 'llm',
						label: `${agentPrefix}Writing the final answer…`,
						detail: null,
						status: 'running',
						tone: 'info',
						startedAt: ts,
						durationMs: null
					});
				}
				return;
			}
			if (otherDelegate) {
				if (st === 'completed') settleSynthesisRow(delegateTaskId, 'done');
				else if (st === 'failed' || st === 'cancelled') settleSynthesisRow(delegateTaskId, 'failed');
				return;
			}
			if (st === 'completed') {
				closeAllRunningRows('done');
				turnTerminal = true;
			} else if (st === 'failed' || st === 'cancelled') {
				closeAllRunningRows('failed');
				turnTerminal = true;
			}
			return;
		}
		// A delegated task's answer is published (or its synthesis failed):
		// nothing is left running, the "Writing the final answer…" row
		// included.
		if (eventType === 'chat.delegate.output_ready' || eventType === 'chat.delegate.output_failed') {
			const settled = eventType === 'chat.delegate.output_ready' ? 'done' : 'failed';
			if (otherDelegate) {
				settleSynthesisRow(delegateTaskId, settled);
			} else {
				closeAllRunningRows(settled);
				turnTerminal = true;
			}
			return;
		}
		if (
			eventType === 'execution.completed' ||
			eventType === 'execution.failed' ||
			eventType === 'execution.cancelled' ||
			eventType === 'agentic.execution_completed'
		) {
			const execId =
				(payload?.execution_id as string | undefined) ??
				(payload?.executionId as string | undefined);
			// A sub-agent's execution.completed carries its own (non-matching)
			// execution_id, so it won't close this card's rows; the primary emit
			// (no execution_id) or a match for this card's executionId will.
			if (!execId || execId === executionId) {
				const to =
					eventType === 'execution.failed' || eventType === 'execution.cancelled'
						? 'failed'
						: 'done';
				// A finished execution leaves the final answer being written: that
				// row waits for the task's own terminal status. A failed or
				// cancelled one has no answer coming, so it closes with the rest.
				closeAllRunningRows(to, { keepSynthesis: to === 'done' });
				turnTerminal = true;
			}
			return;
		}

		// ─── Personal Tutor overlay lifecycle ────────────────────────
		if (eventType.startsWith('tutor.')) {
			const runId = (payload?.run_id as string | undefined) ?? 'active';
			if (eventType === 'tutor.run.completed' || eventType === 'tutor.run.failed') {
				terminalTutorRunIds.add(runId);
				void notifyTutorOverlayStatusFromProgress('idle', payload);
			} else if (eventType === 'tutor.run.started') {
				terminalTutorRunIds.delete(runId);
				void notifyTutorOverlayStatusFromProgress('working', payload);
			} else if (!terminalTutorRunIds.has(runId)) {
				void notifyTutorOverlayStatusFromProgress('working', payload);
			} else {
				console.debug('[ChatTurnProgress] ignored stale tutor overlay working event after terminal run', {
					runId,
					eventType
				});
			}
			const stepCount =
				typeof payload?.step_count === 'number'
					? payload.step_count
					: `${payload?.step_kind ?? eventType}`;
			const stepLabel =
				(payload?.step_label as string | undefined) ||
				(payload?.target as string | undefined) ||
				(payload?.goal as string | undefined) ||
				'Personal Tutor';
			const target = payload?.target as string | undefined;
			const note = payload?.note as string | undefined;
			const detail = target || note ? trimTo(target || note || '', 180) : null;
			const runKey = `tutor:${runId}`;
			const stepKey = `tutor:${runId}:${stepCount}`;
			const closeRunningTutorStepRows = (status: 'done' | 'failed') => {
				const prefix = `${runKey}:`;
				for (const [key, row] of rowsByKey.entries()) {
					if (key.startsWith(prefix) && row.status === 'running') {
						patchRow(key, { status });
					}
				}
			};
			if (eventType === 'tutor.run.started') {
				upsertRow({
					key: runKey,
					kind: 'step',
					label: `${agentPrefix}Tutor started`,
					detail: trimTo((payload?.goal as string | undefined) ?? '', 180) || null,
					status: 'running',
					tone: 'info',
					startedAt: ts,
					durationMs: null
				});
				return;
			}
			if (eventType === 'tutor.run.completed') {
				closeRunningTutorStepRows('done');
				const patch = {
					label: `${agentPrefix}Tutor completed`,
					status: 'done' as const,
					tone: 'info' as const
				};
				if (rowsByKey.has(runKey)) {
					patchRow(runKey, patch);
				} else {
					upsertRow({
						key: runKey,
						kind: 'step',
						detail: null,
						startedAt: ts,
						durationMs: null,
						...patch
					});
				}
				return;
			}
			if (eventType === 'tutor.run.failed') {
				closeRunningTutorStepRows('failed');
				const patch = {
					label: `${agentPrefix}Tutor failed`,
					status: 'failed' as const,
					tone: 'error' as const,
					detail: note ? trimTo(note, 180) : null
				};
				if (rowsByKey.has(runKey)) {
					patchRow(runKey, patch);
				} else {
					upsertRow({
						key: runKey,
						kind: 'step',
						startedAt: ts,
						durationMs: null,
						...patch
					});
				}
				return;
			}
			const stepPatch = (() => {
				switch (eventType) {
					case 'tutor.step.observed':
						return { label: `${agentPrefix}Observed screen`, status: 'done' as const, tone: 'info' as const };
					case 'tutor.step.target_resolved':
						return { label: `${agentPrefix}Resolved target`, status: 'done' as const, tone: 'info' as const };
					case 'tutor.step.drawing':
						return { label: `${agentPrefix}Drawing marker`, status: 'done' as const, tone: 'tool' as const };
					case 'tutor.step.action_delegated':
						return { label: `${agentPrefix}Action delegated`, status: 'running' as const, tone: 'tool' as const };
					case 'tutor.step.verifying':
						return { label: `${agentPrefix}Verifying action`, status: 'running' as const, tone: 'info' as const };
					case 'tutor.step.verified':
						return { label: `${agentPrefix}Verified action`, status: 'done' as const, tone: 'info' as const };
					case 'tutor.step.failed':
						return { label: `${agentPrefix}Tutor step failed`, status: 'failed' as const, tone: 'error' as const };
					case 'tutor.step.recovering':
						return { label: `${agentPrefix}Recovering tutor flow`, status: 'running' as const, tone: 'pause' as const };
					case 'tutor.step.clearing':
						return { label: `${agentPrefix}Clearing tutor marks`, status: 'done' as const, tone: 'tool' as const };
					default:
						return null;
				}
			})();
			if (!stepPatch) return;
			if (rowsByKey.has(stepKey)) {
				patchRow(stepKey, { ...stepPatch, detail: detail ?? stepLabel });
			} else {
				upsertRow({
					key: stepKey,
					kind: 'step',
					detail: detail ?? stepLabel,
					startedAt: ts,
					durationMs: null,
					...stepPatch
				});
			}
			return;
		}

		// ─── Coding-engine lifecycle (Pi) ────────────────────────────
		if (eventType.startsWith('coding.')) {
			const shadowId =
				(payload?.shadow_workspace_id as string | undefined) ?? 'coding';
			const profile = payload?.coding_profile as Record<string, unknown> | undefined;
			const profileLabel =
				(typeof profile?.label === 'string' && profile.label) ||
				(typeof profile?.id === 'string' && profile.id) ||
				'Coding agent';
			const mainKey = `coding:${shadowId}`;
			if (eventType === 'coding.started') {
				upsertRow({
					key: mainKey,
					kind: 'step',
					label: `${agentPrefix}Coding with ${profileLabel}`,
					detail:
						(typeof profile?.model === 'string' && profile.model) ||
						(payload?.engine as string | undefined) ||
						null,
					status: 'running',
					tone: 'info',
					startedAt: ts,
					durationMs: null
				});
				return;
			}
			if (eventType === 'coding.message') {
				const delta = (payload?.delta as string | undefined) ?? '';
				if (!delta) return;
				const existing = rowsByKey.get(mainKey);
				const merged = `${existing?.detail ?? ''} ${delta}`.trim().replace(/\s+/g, ' ');
				if (!existing) {
					upsertRow({
						key: mainKey,
						kind: 'step',
						label: `${agentPrefix}Coding with ${profileLabel}`,
						detail: trimTo(merged, 180),
						status: 'running',
						tone: 'info',
						startedAt: ts,
						durationMs: null
					});
				} else {
					patchRow(mainKey, {
						label: `${agentPrefix}Coding with ${profileLabel}`,
						detail: trimTo(merged, 180),
						status: 'running'
					});
				}
				return;
			}
			if (eventType === 'coding.tool.started' || eventType === 'coding.tool.finished') {
				const tool =
					(payload?.tool_name as string | undefined) ||
					(payload?.tool as string | undefined) ||
					'coding agent tool';
				const callId =
					(payload?.tool_call_id as string | undefined) ??
					`${payload?.sequence ?? ts}`;
				const key = `coding-tool:${shadowId}:${callId}`;
				if (eventType === 'coding.tool.started') {
					upsertRow({
						key,
						kind: 'tool',
						label: `${agentPrefix}Coding agent tool: ${tool}`,
						detail: null,
						status: 'running',
						tone: 'tool',
						startedAt: ts,
						durationMs: null
					});
				} else {
					if (!rowsByKey.has(key)) {
						upsertRow({
							key,
							kind: 'tool',
							label: `${agentPrefix}Coding agent tool finished: ${tool}`,
							detail: null,
							status: 'done',
							tone: 'tool',
							startedAt: ts,
							durationMs: null
						});
					} else {
						patchRow(key, {
							label: `${agentPrefix}Coding agent tool finished: ${tool}`,
							status: 'done',
							tone: 'tool'
						});
					}
				}
				return;
			}
			if (eventType === 'coding.approval_requested') {
				const fileCount = payload?.file_count as number | undefined;
				const rowPatch = {
					label: `${agentPrefix}Coding proposal ready`,
					detail:
						typeof fileCount === 'number'
							? `${fileCount} file${fileCount === 1 ? '' : 's'}`
							: null,
					status: 'waiting' as const,
					tone: 'pause' as const
				};
				if (rowsByKey.has(mainKey)) {
					patchRow(mainKey, rowPatch);
				} else {
					upsertRow({
						key: mainKey,
						kind: 'step',
						startedAt: ts,
						durationMs: null,
						...rowPatch
					});
				}
				return;
			}
			if (eventType === 'coding.completed') {
				const pendingApproval = payload?.pending_approval === true;
				const noChange = payload?.no_change === true;
				const assistantText = payload?.assistant_text as string | undefined;
				const rowPatch = {
					label: pendingApproval
						? `${agentPrefix}Coding proposal ready`
						: noChange
							? `${agentPrefix}Coding finished with no changes`
							: `${agentPrefix}Coding finished`,
					detail: assistantText ? trimTo(assistantText, 180) : null,
					status: pendingApproval ? ('waiting' as const) : ('done' as const),
					tone: pendingApproval ? ('pause' as const) : ('info' as const)
				};
				if (rowsByKey.has(mainKey)) {
					patchRow(mainKey, rowPatch);
				} else {
					upsertRow({
						key: mainKey,
						kind: 'step',
						startedAt: ts,
						durationMs: null,
						...rowPatch
					});
				}
				return;
			}
			if (eventType === 'coding.failed') {
				const error = (payload?.error as string | undefined) ?? 'coding failed';
				if (!rowsByKey.has(mainKey)) {
					upsertRow({
						key: mainKey,
						kind: 'step',
						label: `${agentPrefix}Coding failed`,
						detail: trimTo(error, 180),
						status: 'failed',
						tone: 'error',
						startedAt: ts,
						durationMs: null
					});
				} else {
					patchRow(mainKey, {
						label: `${agentPrefix}Coding failed`,
						detail: trimTo(error, 180),
						status: 'failed',
						tone: 'error'
					});
				}
				return;
			}
			return;
		}

		// ─── Resumed from a pause: nothing of this execution is waiting ──
		// The run went back to work, so every pause row it raised was
		// answered, even one whose answer carried a different id.
		if (eventType === 'ExecutionStatusChanged' || eventType === 'execution.status_changed') {
			// Live frames carry the fields under `data`; replayed canonical rows
			// under `payload`, with the execution id on the envelope.
			const status = payload ?? (parsed.payload as Record<string, unknown> | undefined);
			const normal = (value: unknown) =>
				String(value ?? '').replace(/[_\s]/g, '').toLowerCase();
			const statusExecution = status?.execution_id ?? parsed.execution_id;
			const resumed =
				['waitinguser', 'waitingforuser'].includes(normal(status?.previous_status)) &&
				// An answered ask resumes into planning (the durable ask loop) or
				// straight into execution.
				['planning', 'planningcomplete', 'runnable', 'executing', 'running'].includes(
					normal(status?.new_status)
				);
			if (resumed && (!executionId || statusExecution === executionId)) {
				settlePauseRows(null);
			}
			if (resumed) return;
		}

		// ─── HITL resolution: settle the pause row it answers ─────────
		// Without this a pause row stayed `waiting` after the answer (a plan
		// approved from the chat notification kept the turn reading "Waiting on
		// you" with nothing left to answer anywhere).
		if (isPauseResolvedEvent(outerType, eventType)) {
			const correlationId =
				(payload?.correlation_id as string | undefined) ??
				(payload?.pause_state_id as string | undefined) ??
				(payload?.request_id as string | undefined) ??
				null;
			settlePauseRows(correlationId);
			return;
		}

		// ─── HITL pause family ────────────────────────────────────────
		if (isPauseEvent(outerType, eventType)) {
			pauseActive = true;
			version += 1;
			const correlationId =
				(payload?.correlation_id as string | undefined) ??
				(payload?.pause_state_id as string | undefined) ??
				(payload?.request_id as string | undefined) ??
				'pause';
			const prompt =
				(payload?.prompt as string | undefined) ??
				(payload?.question as string | undefined) ??
				null;
			upsertRow({
				key: `pause:${correlationId}`,
				kind: 'pause',
				label: 'Waiting on your input',
				detail: prompt ? trimTo(prompt, 140) : null,
				status: 'waiting',
				tone: 'pause',
				startedAt: ts,
				durationMs: null
			});
			return;
		}

		// ─── Typed LLM call (delegated / agentic run) ─────────────────
		// `LLMRequestSent` opens a running row, `LLMRouted` names the model,
		// `LLMResponseReceived` settles the same row — paired by
		// `correlation.llm_call_id`. Without the id there is nothing to pair
		// and the bubble shows nothing, as it did before. An unscoped bubble
		// keeps to its own execution: the hub offers every LLM call in the
		// scope, background workers' included.
		if (
			outerType === 'LLMRequestSent' ||
			outerType === 'LLMRouted' ||
			outerType === 'LLMResponseReceived'
		) {
			const callId = llmCallIdOf(payload);
			if (!callId) return;
			if (!taskId && !(executionId && payload?.execution_id === executionId)) return;
			const key = llmKeyByCallId.get(callId) ?? `llm:call:${callId}`;
			llmKeyByCallId.set(callId, key);
			const existing = rowsByKey.get(key);
			const thinking = `${agentPrefix}${THINKING_LABEL}`;
			if (outerType === 'LLMRequestSent') {
				// Upsert-if-missing: a late request never reopens a settled row.
				if (existing) return;
				upsertRow({
					key,
					kind: 'llm',
					label: thinking,
					detail: null,
					status: 'running',
					tone: 'info',
					startedAt: ts,
					durationMs: null
				});
				return;
			}
			if (outerType === 'LLMRouted') {
				const model = routedModelLabel(payload);
				if (!model) return;
				if (!existing) {
					upsertRow({
						key,
						kind: 'llm',
						label: `${agentPrefix}Thinking with ${model}`,
						detail: null,
						status: 'running',
						tone: 'info',
						startedAt: ts,
						durationMs: null
					});
				} else if (existing.status === 'running') {
					patchRow(key, { label: `${agentPrefix}Thinking with ${model}` });
				}
				return;
			}
			// LLMResponseReceived. A row the chat-side settle already closed
			// keeps what it wrote (TTFT, usage).
			if (existing && existing.status !== 'running') return;
			const ok = payload?.success !== false;
			const ms = payload?.latency_ms as number | undefined;
			const answered = answeredModelLabel(payload);
			const label = answered
				? `${agentPrefix}Thinking with ${answered}`
				: existing && existing.label !== thinking
					? existing.label
					: `${agentPrefix}Thinking with ${requestedModelLabel(payload)}`;
			const error =
				(payload?.error as string | undefined) ||
				(payload?.decision_summary as string | undefined) ||
				'error';
			const settled: Pick<ProgressRow, 'label' | 'status' | 'tone' | 'detail' | 'durationMs'> = {
				label,
				status: ok ? 'done' : 'failed',
				tone: ok ? 'info' : 'error',
				detail: [
					ok ? formatLatencyPair(null, typeof ms === 'number' ? ms : null) : trimTo(error, 160),
					...llmStepUsageParts(payload)
				].filter(Boolean).join(' · ') || null,
				durationMs: typeof ms === 'number' ? ms : null
			};
			if (existing) {
				patchRow(key, settled);
			} else {
				upsertRow({ key, kind: 'llm', startedAt: ts, ...settled });
			}
			return;
		}

		// ─── LLM lifecycle (coalesced by trace_id or iteration) ───────
		if (eventType.startsWith('llm.')) {
			const traceKey =
				(payload?.trace_id as string | undefined) ??
				(payload?.iteration as number | undefined)?.toString() ??
				'llm';
			const key = chatLlmKey(eventType, payload, `llm:${traceKey}`);
			if (eventType === 'llm.requested') {
				const model =
					(payload?.model as string | undefined) ||
					(payload?.capability as string | undefined) ||
					'model';
				// A row a typed copy already settled stays settled.
				const existing = rowsByKey.get(key);
				if (existing && existing.status !== 'running') return;
				upsertRow({
					key,
					kind: 'llm',
					label: `${agentPrefix}Thinking with ${model}`,
					detail: null,
					status: 'running',
					tone: 'info',
					startedAt: ts,
					durationMs: null
				});
				return;
			}
			if (eventType === 'llm.first_token') {
				// Time-to-first-token chip — the row stays in
				// `running` because the LLM call is still in flight
				// (more tokens streaming). Detail format is
				// `TTFT (total)`; we have TTFT but not total yet, so
				// total renders as `…` until `llm.succeeded` lands and
				// rewrites the detail.
				const ttftMs = payload?.duration_ms as number | undefined;
				if (typeof ttftMs === 'number') {
					patchRow(key, {
						detail: formatLatencyPair(ttftMs, null)
					});
				}
				return;
			}
			if (eventType === 'llm.succeeded') {
				// Task-run calls report `latency_ms`; chat calls `duration_ms`.
				const ms = (payload?.duration_ms ?? payload?.latency_ms) as number | undefined;
				const ttftMs = payload?.ttft_ms as number | undefined | null;
				const latency = formatLatencyPair(
					typeof ttftMs === 'number' ? ttftMs : null,
					typeof ms === 'number' ? ms : null
				);
				const usageText = llmStepUsageParts(payload);
				patchRow(key, {
					label: `${agentPrefix}Response ready`,
					status: 'done',
					tone: 'info',
					detail: [latency, ...usageText].filter(Boolean).join(' · ') || null,
					durationMs: ms ?? null
				});
				return;
			}
			if (eventType === 'llm.failed') {
				const err = (payload?.error as string | undefined) ?? 'unknown error';
				const ms = payload?.duration_ms as number | undefined;
				patchRow(key, {
					label: `${agentPrefix}LLM call failed`,
					status: 'failed',
					tone: 'error',
					detail: [err, ...llmStepUsageParts(payload)].filter(Boolean).join(' · '),
					durationMs: ms ?? null
				});
				return;
			}
			return;
		}

		// ─── Reasoning (coalesced by trace_id) ────────────────────────
		if (eventType.startsWith('reasoning.')) {
			const traceId =
				(payload?.trace_id as string | undefined) ?? 'reasoning';
			const key = `reasoning:${traceId}`;
			if (eventType === 'reasoning.start') {
				upsertRow({
					key,
					kind: 'reasoning',
					label: 'Reasoning',
					detail: null,
					status: 'running',
					tone: 'reasoning',
					startedAt: ts,
					durationMs: null
				});
				return;
			}
			if (eventType === 'reasoning.content') {
				const delta =
					(payload?.delta as string | undefined) ??
					(payload?.content as string | undefined) ??
					'';
				const existing = rowsByKey.get(key);
				const merged = existing?.detail
					? `${existing.detail} ${delta}`.trim()
					: delta;
				if (!existing) {
					upsertRow({
						key,
						kind: 'reasoning',
						label: 'Reasoning',
						detail: trimTo(merged, 200),
						status: 'running',
						tone: 'reasoning',
						startedAt: ts,
						durationMs: null
					});
				} else {
					patchRow(key, { detail: trimTo(merged, 200) });
				}
				return;
			}
			if (eventType === 'reasoning.end') {
				const ms = payload?.duration_ms as number | undefined;
				patchRow(key, {
					status: 'done',
					durationMs: ms ?? null
				});
				return;
			}
			return;
		}

		// ─── Tool lifecycle (coalesced by call_id) ────────────────────
		if (eventType.startsWith('tool.call.') || eventType.startsWith('tool.')) {
			// Prefer `origin_call_id` (set by the chat-fanout re-stamp
			// when a sub-agent's tool call is forwarded into this chat's
			// scope — see realtime_events.rs:3021). The fanout namespaces
			// `payload.call_id` to `delegate-<exec>/<orig>` so it never
			// collides with the chat agent's own tool-calls, but using
			// that namespaced id as the row key here would split the
			// same logical tool call across two rows (primary emit keyed
			// by raw `orig`, fanout copy keyed by `delegate-<exec>/<orig>`).
			// Falling back to `origin_call_id` first collapses them.
			const callId =
				(payload?.origin_call_id as string | undefined) ??
				(payload?.call_id as string | undefined) ??
				(payload?.tool_call_id as string | undefined) ??
				`anon-${ts}`;
			const tool =
				(payload?.tool_name as string | undefined) ||
				(payload?.tool as string | undefined) ||
				(payload?.name as string | undefined) ||
				'tool';
			const key = `tool:${callId}`;
			// Row name: a server `detail` or an MCP `<pack> · <tool>` when the
			// event carries one; a finish without args keeps the start's name.
			const call = describeToolCall(tool, payload?.args, payload?.detail);
			const callName =
				call.source !== 'plain' ? call.name : (toolRowName(rowsByKey.get(key)?.label) ?? tool);
			const prevMeta = toolMetaByKey.get(key);
			const metaTarget = call.target ?? prevMeta?.target;
			toolMetaByKey.set(key, {
				tool: tool === 'tool' && prevMeta ? prevMeta.tool : tool,
				...(metaTarget ? { target: metaTarget } : {})
			});
			if (eventType.endsWith('.started')) {
				// tactical pattern T4: special-case delegate_to_agent to surface
				// parallel fan-out. The args carry the full
				// delegation_targets array (per realtime tool.call.started
				// emission); when N >= 2, the agent is decomposing the
				// task into N parallel sub-agents. Rewrite the label so
				// the user sees decomposition explicitly INSTEAD OF a
				// generic "Calling delegate_to_agent". Per-child task
				// cards still render below as usual.
				// A start replayed after its finish must not reopen the settled row.
				const existing = rowsByKey.get(key);
				if (existing && existing.status !== 'running') return;
				let label = `${agentPrefix}Calling ${call.name}`;
				// An MCP call's argument summary (e.g. the SQL) reads under the row.
				let detail: string | null = call.source === 'mcp' ? (call.target ?? null) : null;
				if (tool === 'delegate_to_agent') {
					const args =
						(payload?.args as Record<string, unknown> | undefined) ?? {};
					const targets = Array.isArray(args.delegation_targets)
						? (args.delegation_targets as unknown[])
						: [];
					const targetIds = targets
						.map((t) => {
							if (t && typeof t === 'object') {
								const obj = t as Record<string, unknown>;
								return typeof obj.target_agent_id === 'string'
									? obj.target_agent_id
									: null;
							}
							return null;
						})
						.filter((id): id is string => !!id);
					if (targetIds.length >= 2) {
						label = `${agentPrefix}Decomposing into ${targetIds.length} parallel agents`;
						detail = targetIds.join(', ');
					} else if (targetIds.length === 1) {
						label = `${agentPrefix}Delegating to ${targetIds[0]}`;
					}
				}
				upsertRow({
					key,
					kind: 'tool',
					label,
					detail,
					status: 'running',
					tone: 'tool',
					startedAt: ts,
					durationMs: null
				});
				return;
			}
			if (eventType.endsWith('.args')) {
				// Streaming JSON args — we don't render mid-args in the
				// progress bubble (cards want complete data), but if no
				// `.started` row exists yet, the args row is the earliest
				// signal a tool call is in flight.
				if (!rowsByKey.has(key)) {
					upsertRow({
						key,
						kind: 'tool',
						label: `${agentPrefix}Calling ${callName}`,
						detail: null,
						status: 'running',
						tone: 'tool',
						startedAt: ts,
						durationMs: null
					});
				}
				return;
			}
			if (
				eventType.endsWith('.succeeded') ||
				eventType.endsWith('.finished')
			) {
				const ms = payload?.duration_ms as number | undefined;
				settleToolRow(key, ts, !!agentic, {
					label: `${agentPrefix}${callName} returned`,
					status: 'done',
					tone: 'tool',
					detail: ms ? `${Math.round(ms)}ms` : null,
					durationMs: ms ?? null
				});
				return;
			}
			if (eventType.endsWith('.failed')) {
				const err = (payload?.error as string | undefined) ?? 'tool failed';
				settleToolRow(key, ts, !!agentic, {
					label: `${agentPrefix}${callName} failed`,
					status: 'failed',
					tone: 'error',
					detail: trimTo(err, 200)
				});
				return;
			}
			return;
		}

		// ─── Step lifecycle (coalesced by step_id) ────────────────────
		if (eventType.startsWith('step.')) {
			const stepId =
				(payload?.step_id as string | undefined) ??
				(payload?.label as string | undefined) ??
				`step-${ts}`;
			const key = `step:${stepId}`;
			if (eventType === 'step.started') {
				upsertRow({
					key,
					kind: 'step',
					label: `Step: ${stepId}`,
					detail: null,
					status: 'running',
					tone: 'info',
					startedAt: ts,
					durationMs: null
				});
				return;
			}
			if (eventType === 'step.completed') {
				patchRow(key, { status: 'done', label: `Step complete: ${stepId}` });
				return;
			}
			if (eventType === 'step.failed') {
				const err = (payload?.error as string | undefined) ?? '';
				patchRow(key, {
					status: 'failed',
					label: `Step failed: ${stepId}`,
					tone: 'error',
					detail: err
				});
				return;
			}
			return;
		}

		// ─── Agentic iteration markers ────────────────────────────────
		if (eventType === 'agentic.iteration_started') {
			const iter = payload?.iteration as number | undefined;
			upsertRow({
				key: `iter:${iter ?? ts}`,
				kind: 'step',
				label: iter ? `Iteration ${iter}` : 'Iteration',
				detail: null,
				status: 'running',
				tone: 'info',
				startedAt: ts,
				durationMs: null
			});
			return;
		}


		// ─── Artifact / output (each is distinct, no coalescing) ──────
		if (eventType === 'artifact.created' || eventType === 'output.created') {
			const name =
				(payload?.name as string | undefined) ||
				(payload?.title as string | undefined) ||
				'file';
			upsertRow({
				key: `artifact:${name}:${ts}`,
				kind: 'artifact',
				label: 'Produced file',
				detail: name,
				status: 'done',
				tone: 'tool',
				startedAt: ts,
				durationMs: null
			});
			return;
		}

		// Anything else drops silently. Still surfaces in `/events` and
		// the EventsConsole; the chat bubble is the curated view.
	}

	/**
	 * Whether an event is this task's: any task id it carries (the run's own, a
	 * delegated child's root/parent, an LLM call's correlation) is this task, or
	 * any execution id it carries is this run. An event naming none of them is
	 * not attributable, and a task-scoped stream does not show it.
	 */
	function eventBelongsToTask(
		parsed: Record<string, unknown>,
		payload: Record<string, unknown> | undefined,
		task: string,
		execution: string | null
	): boolean {
		const sources = [payload, parsed.data as Record<string, unknown> | undefined, parsed];
		const read = (key: string): string[] =>
			sources
				.map((source) => source?.[key])
				.filter((value): value is string => typeof value === 'string' && value.length > 0);
		const correlation = (payload?.correlation ?? (parsed.data as Record<string, unknown> | undefined)?.correlation) as
			| Record<string, unknown>
			| undefined;
		const taskIds = [
			...read('task_id'),
			...read('root_task_id'),
			...read('parent_task_id'),
			...(typeof correlation?.task_id === 'string' ? [correlation.task_id] : [])
		];
		if (taskIds.includes(task)) return true;
		if (execution) {
			const executionIds = [
				...read('execution_id'),
				...read('root_execution_id'),
				...read('parent_execution_id')
			];
			if (executionIds.includes(execution)) return true;
		}
		return false;
	}

	function isPauseResolvedEvent(outerType: string, eventType: string): boolean {
		if (outerType === 'HitlResolved') return true;
		return (
			eventType === 'hitl.resolved' ||
			eventType === 'input.received' ||
			eventType === 'input.resolved'
		);
	}

	/**
	 * Settle the pause rows an answer resolves: the row keyed on its correlation
	 * id, plus the id-less `pause:pause` row a payload without ids files under.
	 * An answer that names no id settles every waiting pause row. The turn stops
	 * reading "Waiting on you" once no pause row is left waiting.
	 */
	function settlePauseRows(correlationId: string | null): void {
		for (const [key, row] of rowsByKey.entries()) {
			if (row.kind !== 'pause' || row.status !== 'waiting') continue;
			// An answer may name the ask with an attempt suffix (`<id>~<attempt>`)
			// while another record of the same ask used the bare id.
			const id = key.slice('pause:'.length);
			const matches =
				correlationId === null ||
				id === correlationId ||
				id === 'pause' ||
				correlationId.startsWith(`${id}~`) ||
				id.startsWith(`${correlationId}~`);
			if (!matches) continue;
			rowsByKey.set(key, { ...row, label: 'Answered', status: 'done', tone: 'info' });
		}
		pauseActive = [...rowsByKey.values()].some(
			(row) => row.kind === 'pause' && row.status === 'waiting'
		);
		version += 1;
	}

	function isPauseEvent(outerType: string, eventType: string): boolean {
		// Legacy CamelCase outer-type matcher kept for back-compat with
		// any pre-canonical events still in flight.
		if (outerType === 'HitlRequested') return true;
		return (
			eventType === 'waiting_for_confirmation' ||
			eventType === 'execution.waiting_for_user' ||
			eventType === 'input.requested' ||
			// Canonical post-H7.1 LLM-emitted HITL request. Without
			// this, `hitl.requested { source: "agentic" }` events (fired
			// when a worker agent asks the user for input or external
			// action mid-execution) wouldn't render the chat's inline
			// "Waiting on your input" pause row — the user would only
			// see the request in the top-bar AttentionBar, and on
			// chat-delegated ephemeral tasks it wouldn't even reach
			// there until the backend's `list_attention_items` fix.
			eventType === 'hitl.requested' ||
			eventType === 'clarification.queued'
		);
	}

	function trimTo(value: string, max: number): string {
		const trimmed = value.replace(/\s+/g, ' ').trim();
		if (trimmed.length <= max) return trimmed;
		return `${trimmed.slice(0, max - 1)}…`;
	}

	// Render a latency value with the unit that reads most naturally
	// at the magnitude — sub-second stays in ms (LLM first-byte is
	// often 400–900 ms; "0.7s" loses precision), second-or-more flips
	// to a one-decimal `s` so a 19-second chip doesn't sit on the bar
	// as "19421ms" and force the operator to count digits.
	function formatLatency(ms: number): string {
		if (!Number.isFinite(ms) || ms < 0) return '—';
		if (ms < 1000) return `${Math.round(ms)}ms`;
		return `${(ms / 1000).toFixed(1)}s`;
	}

	// Compact `TTFT (total)` pair for LLM-call chips. The first number
	// is time-to-first-token (model started speaking); the parenthesized
	// number is the full call duration (when known). One side may be
	// null mid-flight — TTFT lands at `llm.first_token` while total is
	// still in flight, so total renders as `…`. After `llm.succeeded`,
	// both values are known and the chip reads e.g. `1.2s (4.5s)`. When
	// TTFT is absent (tool-call-only response, all-reasoning turn, non-
	// streaming provider), only the total survives so the chip falls
	// back to a single-value render — no orphaned parens.
	function formatLatencyPair(ttftMs: number | null, totalMs: number | null): string | null {
		const hasTtft = typeof ttftMs === 'number' && Number.isFinite(ttftMs);
		const hasTotal = typeof totalMs === 'number' && Number.isFinite(totalMs);
		if (!hasTtft && !hasTotal) return null;
		if (hasTtft && !hasTotal) return `${formatLatency(ttftMs!)} (…)`;
		if (!hasTtft && hasTotal) return formatLatency(totalMs!);
		return `${formatLatency(ttftMs!)} (${formatLatency(totalMs!)})`;
	}

	function extractTimestamp(parsed: Record<string, unknown>): number {
		const candidates: unknown[] = [
			(parsed as { timestamp_ms?: unknown }).timestamp_ms,
			(parsed as { timestamp?: unknown }).timestamp,
			(parsed.data as { timestamp_ms?: unknown } | undefined)?.timestamp_ms,
			(parsed.data as { timestamp?: unknown } | undefined)?.timestamp
		];
		for (const candidate of candidates) {
			if (typeof candidate === 'number' && Number.isFinite(candidate)) return candidate;
			if (typeof candidate === 'string') {
				const parsedTs = Date.parse(candidate);
				if (Number.isFinite(parsedTs)) return parsedTs;
			}
		}
		return Date.now();
	}

	/**
	 * History for this bubble: ONE `backfill_only` request. A page refresh
	 * (fresh mount) or a hub resync RESTORES and SETTLES the rows — the dedup
	 * in handleEvent skips events we already applied. Only with a unique
	 * `execution_id` (which can't leak a prior run's events); without one the
	 * bubble is live-only, as the old `since=now` stream was (the `/events`
	 * API otherwise backfills a 24h window, which would pull prior-turn events
	 * like a `Calling whatsapp` from an earlier run into this bubble).
	 */
	async function backfill(signal: AbortSignal): Promise<void> {
		if (!executionId) return;
		if (!scope.principal || !scope.workspace) return;
		const params = new URLSearchParams();
		params.set('execution_id', executionId);
		if (taskId) params.set('task_id', taskId);
		params.set('backfill_only', 'true');
		const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, { signal });
		if (signal.aborted) return;
		if (!response.ok) {
			console.warn('[ChatTurnProgress] /events backfill not OK, status:', response.status);
			return;
		}
		const body = await response.text();
		if (signal.aborted) return;
		for (const frame of parseNdjsonFrames(body)) handleEvent(frame);
	}

	function closeSubscription(): void {
		if (liveUnsub) {
			console.debug('[ChatTurnProgress] closing hub subscription');
			liveUnsub();
			liveUnsub = null;
		}
	}

	// Live events come from the app's event hub — the bubble never holds a
	// stream of its own. Every event in the scope is offered, as the old live
	// tail did; `handleEvent` keeps a task-scoped bubble to its own task.
	onMount(() => {
		liveUnsub = subscribeWithBackfill({
			filter: () => true,
			backfill: ({ signal }) => backfill(signal),
			onEvent: (event) => handleEvent(event.raw)
		});
	});

	onDestroy(() => {
		closeSubscription();
		clock.stop();
		// Carry the accumulated rows across the unmount boundary so the
		// final assistant message can render them as a collapsible
		// "What happened" section. No-op when sessionId isn't set
		// (component used outside a chat surface) or no rows were
		// produced (short-circuit reply with no progress events).
		if (sessionId) {
			const allRows: ChatTurnActivityRow[] = orderedKeys
				.map((key) => rowsByKey.get(key))
				.filter((row): row is ChatTurnActivityRow | undefined => row !== undefined)
				.filter((row): row is ChatTurnActivityRow => row !== undefined);
			captureCompletedTurnActivity(sessionId, allRows);
		}
	});
</script>

<div
	class="chat-turn-progress"
	class:chat={!bare}
	class:chat-start={!bare}
	class:chat-turn-progress--bare={bare}
	class:chat-turn-progress--paused={pauseActive}
>
	{#if !bare}
		<div class="chat-header">Assistant</div>
	{/if}
	<div
		class="chat-turn-progress__bubble"
		class:chat-bubble={!bare}
		class:chat-bubble-neutral={!bare}
	>
		{#if !bare}
			<div class="chat-turn-progress__head">
				<span class="chat-typing-dots"><span></span><span></span><span></span></span>
				<span class="chat-turn-progress__title">
					{pauseActive ? 'Waiting on you' : 'Working…'}
				</span>
			</div>
		{:else if pauseActive}
			<div class="chat-turn-progress__head chat-turn-progress__pause" role="status">
				Waiting on you
			</div>
		{/if}
		{#if variant === 'next'}
			<TurnStage rows={stageRows} />
		{:else if rows.length > 0}
			<ul class="chat-turn-progress__list">
				{#each rows as row (row.key)}
					<li
						class="chat-turn-progress__row chat-turn-progress__row--{row.tone}"
						class:chat-turn-progress__row--running={row.status === 'running'}
						class:chat-turn-progress__row--done={row.status === 'done'}
						class:chat-turn-progress__row--failed={row.status === 'failed'}
						class:chat-turn-progress__row--waiting={row.status === 'waiting'}
					>
						<span class="chat-turn-progress__status" aria-hidden="true">
							{#if row.status === 'running'}
								◐
							{:else if row.status === 'done'}
								✓
							{:else if row.status === 'failed'}
								✕
							{:else if row.status === 'waiting'}
								⏸
							{/if}
						</span>
						<span class="chat-turn-progress__label">{row.label}</span>
						{#if row.status === 'running' && runningElapsed(row.startedAt, now)}
							<span class="chat-turn-progress__elapsed">{runningElapsed(row.startedAt, now)}</span>
						{/if}
						{#if row.detail}
							<span class="chat-turn-progress__detail">{row.detail}</span>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</div>

<style>
	.chat-turn-progress__bubble {
		max-width: min(560px, 100%);
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		transition: border-color 200ms ease, background 200ms ease;
	}

	.chat-turn-progress--bare .chat-turn-progress__bubble {
		max-width: none;
	}

	.chat-turn-progress--paused:not(.chat-turn-progress--bare) .chat-turn-progress__bubble {
		border: 1px solid color-mix(in srgb, var(--accent-primary) 60%, transparent);
		background: color-mix(
			in srgb,
			var(--accent-primary) 6%,
			var(--bg-card, var(--bg-base))
		);
	}

	.chat-turn-progress__head {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.72rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.16em;
		color: var(--text-muted);
	}

	.chat-turn-progress--paused .chat-turn-progress__head {
		color: var(--accent-primary);
	}

	.chat-turn-progress__title {
		opacity: 0.85;
	}

	.chat-turn-progress__list {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.18rem;
		border-top: 1px dashed
			color-mix(in srgb, var(--border-soft, currentColor) 30%, transparent);
		padding-top: 0.4rem;
	}

	.chat-turn-progress__row {
		display: grid;
		grid-template-columns: 1rem max-content 1fr;
		align-items: baseline;
		gap: 0.5rem;
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.74rem;
		line-height: 1.35;
		min-width: 0;
	}

	.chat-turn-progress__status {
		font-size: 0.78rem;
		opacity: 0.7;
		text-align: center;
	}

	.chat-turn-progress__row--running .chat-turn-progress__status {
		color: var(--accent-primary);
		animation: chat-turn-progress-spin 1.2s linear infinite;
	}

	.chat-turn-progress__row--done .chat-turn-progress__status {
		color: var(--color-success, #4ea64e);
	}

	.chat-turn-progress__row--failed .chat-turn-progress__status,
	.chat-turn-progress__row--waiting .chat-turn-progress__status {
		opacity: 1;
	}

	.chat-turn-progress__row--failed .chat-turn-progress__status {
		color: var(--color-error, #b04545);
	}

	.chat-turn-progress__row--waiting .chat-turn-progress__status {
		color: var(--accent-primary);
	}

	.chat-turn-progress__label {
		font-weight: 600;
		color: var(--text-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* A running row's live elapsed: tabular digits so the tick holds still. */
	.chat-turn-progress__elapsed {
		flex: none;
		color: var(--text-muted);
		font-variant-numeric: tabular-nums;
	}

	.chat-turn-progress__detail {
		color: var(--text-secondary);
		overflow-wrap: anywhere;
		word-break: break-word;
		min-width: 0;
	}

	.chat-turn-progress__row--tool .chat-turn-progress__label {
		color: var(--accent-primary);
	}

	.chat-turn-progress__row--reasoning .chat-turn-progress__label {
		color: var(--text-muted);
		font-style: italic;
	}

	.chat-turn-progress__row--error .chat-turn-progress__label {
		color: var(--color-error, #b04545);
	}

	.chat-turn-progress__row--pause .chat-turn-progress__label {
		color: var(--accent-primary);
	}

	@keyframes chat-turn-progress-spin {
		from {
			transform: rotate(0deg);
		}
		to {
			transform: rotate(360deg);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.chat-turn-progress__row--running .chat-turn-progress__status {
			animation: none;
		}
	}
</style>
