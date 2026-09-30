<script lang="ts">
	/**
	 * In-flight assistant-turn progress bubble.
	 *
	 * Mounted while a chat send is in flight (`isSendingMessage` truthy)
	 * in place of the old plain "typing dots" placeholder. Opens an
	 * NDJSON subscription against `/api/magician/v3/events` scoped to
	 * the current (principal, workspace), filters to chat-relevant
	 * event types, and renders each as a one-line progress entry inside
	 * the assistant bubble.
	 *
	 * Coalescing rules — one row per logical operation, not per event:
	 *   • `reasoning.*` events keyed by `trace_id` — `.start` opens the
	 *     row, `.content` deltas append into the same row's detail,
	 *     `.end` freezes it.
	 *   • `tool.call.*` and `tool.*` events keyed by `call_id` — label
	 *     transitions `Calling X` → `X returned (Nms)` / `X failed`.
	 *   • `llm.*` lifecycle keyed by `trace_id` (or `iteration` if
	 *     present) — one row per LLM call brackets request → response.
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
	import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';

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
	let version = 0;
	let pauseActive = false;
	let connection: AbortController | null = null;
	/** Dedup so a backfill/reconnect replay doesn't re-apply events we already
	 *  have (same key the events store uses). Lets the subscription safely
	 *  replay the full per-turn projection on mount and on every reconnect. */
	const seenEventKeys = new Set<string>();
	/** Set once a turn-level terminal event is applied — stops the reconnect
	 *  loop (no more events are coming) and marks the timeline settled. */
	let turnTerminal = false;
	/** True while the reconnect loop is alive; cleared on unmount to stop it. */
	let subscriptionActive = false;

	$: scope = get(scopeIdentityStore);
	$: rows = bumpVersion(version, computeVisibleRows());

	function bumpVersion<T>(_: number, value: T): T {
		return value;
	}

	function computeVisibleRows(): ProgressRow[] {
		const all = orderedKeys
			.map((key) => rowsByKey.get(key))
			.filter((row): row is ProgressRow => row !== undefined);
		return all.slice(-visibleRows);
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

	/** Settle every still-'running' row to a terminal status. Called when the
	 *  turn/task terminates: the backend emits NO `agentic.iteration_completed`,
	 *  and a step/tool completion event can be dropped or key-mismatched, so rows
	 *  get stranded as 'running' (spinner) after the turn is over. */
	function closeAllRunningRows(status: 'done' | 'failed'): void {
		let changed = false;
		for (const [key, row] of rowsByKey.entries()) {
			if (row.status === 'running') {
				rowsByKey.set(key, { ...row, status });
				changed = true;
			}
		}
		if (changed) version += 1;
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
		if (eventType === 'task.status_changed') {
			const st = String(payload?.status ?? '');
			if (st === 'completed') {
				closeAllRunningRows('done');
				turnTerminal = true;
			} else if (st === 'failed' || st === 'cancelled') {
				closeAllRunningRows('failed');
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
				closeAllRunningRows(to);
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

		// ─── LLM lifecycle (coalesced by trace_id or iteration) ───────
		if (eventType.startsWith('llm.')) {
			const traceKey =
				(payload?.trace_id as string | undefined) ??
				(payload?.iteration as number | undefined)?.toString() ??
				'llm';
			const key = `llm:${traceKey}`;
			if (eventType === 'llm.requested') {
				const model =
					(payload?.model as string | undefined) ||
					(payload?.capability as string | undefined) ||
					'model';
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
				const ms = payload?.duration_ms as number | undefined;
				const ttftMs = payload?.ttft_ms as number | undefined | null;
				patchRow(key, {
					label: `${agentPrefix}Response ready`,
					status: 'done',
					tone: 'info',
					detail: formatLatencyPair(
						typeof ttftMs === 'number' ? ttftMs : null,
						typeof ms === 'number' ? ms : null
					),
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
					detail: err,
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
			if (eventType.endsWith('.started')) {
				// tactical pattern T4: special-case delegate_to_agent to surface
				// parallel fan-out. The args carry the full
				// delegation_targets array (per realtime tool.call.started
				// emission); when N >= 2, the agent is decomposing the
				// task into N parallel sub-agents. Rewrite the label so
				// the user sees decomposition explicitly INSTEAD OF a
				// generic "Calling delegate_to_agent". Per-child task
				// cards still render below as usual.
				let label = `${agentPrefix}Calling ${tool}`;
				let detail: string | null = null;
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
						label: `${agentPrefix}Calling ${tool}`,
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
				patchRow(key, {
					label: `${agentPrefix}${tool} returned`,
					status: 'done',
					tone: 'tool',
					detail: ms ? `${Math.round(ms)}ms` : null,
					durationMs: ms ?? null
				});
				return;
			}
			if (eventType.endsWith('.failed')) {
				const err = (payload?.error as string | undefined) ?? 'tool failed';
				patchRow(key, {
					label: `${agentPrefix}${tool} failed`,
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

	async function openSubscription(): Promise<void> {
		if (!scope.principal || !scope.workspace) return;
		connection = new AbortController();
		console.debug(
			'[ChatTurnProgress] opening /events subscription',
			'session:',
			sessionId,
			'principal:',
			scope.principal,
			'workspace:',
			scope.workspace,
		);
		const params = new URLSearchParams();
		if (executionId) params.set('execution_id', executionId);
		if (taskId) params.set('task_id', taskId);
		// Backfill the turn's timeline so a page refresh (fresh mount) or a
		// mid-turn reconnect RESTORES and SETTLES the rows — the dedup in
		// handleEvent skips events we already applied. Scope the backfill to a
		// unique `execution_id` (which can't leak a prior run's events); without
		// one we can't bound the backfill safely, so fall back to "from now"
		// (the `/events` API otherwise backfills a 24h window, which would pull
		// prior-turn events like a `Calling whatsapp` from an earlier run into
		// this bubble).
		if (!executionId) {
			params.set('since', String(Date.now()));
		}
		try {
			const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, {
				signal: connection.signal,
				// SSE stream that may stay open for a full LLM-bound turn
				// (reasoning models routinely run 30s+). The default 30s
				// `timedFetch` timeout aborted the stream mid-turn — first
				// event arrived (e.g. `llm.requested`), then the connection
				// died and every subsequent `reasoning.*` / `tool.call.*`
				// row was lost. Use the long-running-endpoint timeout so
				// the SSE stays open for the whole turn.
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			});
			if (!response.ok || !response.body) {
				console.warn(
					'[ChatTurnProgress] /events response not OK',
					'status:',
					response.status,
					'has-body:',
					!!response.body,
				);
				return;
			}
			console.debug('[ChatTurnProgress] /events stream open, reading…');
			const reader = response.body.getReader();
			const decoder = new TextDecoder('utf-8');
			let buffer = '';
			let totalLinesSeen = 0;
			while (true) {
				const { done, value } = await reader.read();
				if (done) {
					console.debug(
						'[ChatTurnProgress] /events stream closed by server, total lines:',
						totalLinesSeen,
					);
					return;
				}
				buffer += decoder.decode(value, { stream: true });
				let newlineIndex: number;
				while ((newlineIndex = buffer.indexOf('\n')) !== -1) {
					const line = buffer.slice(0, newlineIndex);
					buffer = buffer.slice(newlineIndex + 1);
					if (!line.trim()) continue;
					totalLinesSeen += 1;
					try {
						const parsed = JSON.parse(line) as Record<string, unknown>;
						handleEvent(parsed);
					} catch (err) {
						console.warn('[ChatTurnProgress] unparseable line:', line.slice(0, 120), err);
					}
				}
			}
		} catch (err) {
			console.debug(
				'[ChatTurnProgress] /events stream aborted (likely unmount or timeout):',
				err instanceof Error ? err.name + ': ' + err.message : err,
			);
		}
	}

	function closeSubscription(): void {
		subscriptionActive = false;
		if (connection) {
			console.debug('[ChatTurnProgress] closing /events subscription');
			connection.abort();
			connection = null;
		}
	}

	// Keep a live subscription for the whole turn. `openSubscription` returns
	// when the stream ends (server close / timeout / mid-turn drop). If the turn
	// hasn't terminated and we weren't unmounted, reconnect with backoff — the
	// re-open backfills the projection (dedup skips what we already have), so a
	// terminal that fired during the gap is recovered rather than leaving a
	// spinner stuck forever.
	async function runSubscriptionLoop(): Promise<void> {
		if (subscriptionActive) return;
		subscriptionActive = true;
		let backoff = 800;
		while (subscriptionActive && !turnTerminal) {
			await openSubscription();
			if (!subscriptionActive || turnTerminal) break;
			await new Promise((resolve) => setTimeout(resolve, backoff));
			backoff = Math.min(backoff * 2, 15_000);
		}
		subscriptionActive = false;
	}

	onMount(() => {
		void runSubscriptionLoop();
	});

	onDestroy(() => {
		closeSubscription();
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
	class="chat chat-start chat-turn-progress"
	class:chat-turn-progress--paused={pauseActive}
>
	<div class="chat-header">Assistant</div>
	<div class="chat-bubble chat-bubble-neutral chat-turn-progress__bubble">
		<div class="chat-turn-progress__head">
			<span class="chat-typing-dots"><span></span><span></span><span></span></span>
			<span class="chat-turn-progress__title">
				{pauseActive ? 'Waiting on you' : 'Working…'}
			</span>
		</div>
		{#if rows.length > 0}
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

	.chat-turn-progress--paused .chat-turn-progress__bubble {
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
