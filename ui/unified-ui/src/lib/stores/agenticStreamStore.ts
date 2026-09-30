// Agentic stream store — captures the three first-class realtime taxonomies
// added in unified-ui v0.0.241 (modeled on AG-UI / assistant-ui /
// CopilotKit):
//   • `plan.snapshot`              — full structured plan tree at approval
//   • `plan.step.finished`         — a step terminated (status + duration)
//   • `tool.call.started`          — tool dispatch begun
//   • `tool.call.finished`         — tool dispatch returned
//   • `reasoning.start/content/end`— LLM extended-thinking segment
//
// The store is execution-scoped: each task execution gets its own slot for
// plan/tool/reasoning state. Components subscribe to the slot they need.
//
// This is the *event capture* layer. Rendering is up to consumer
// components — the existing ExecutionPanel can pivot to read from
// `planSnapshots` instead of parsing markdown, and a new
// "thinking…" / tool-call rail can subscribe directly.

import { derived, writable, type Readable } from 'svelte/store';
import type { AgentEventEnvelope } from '$lib/realtime/v2-websocket';

export const PLAN_SNAPSHOT_EVENT_TYPE = 'plan.snapshot';
export const PLAN_STEP_STARTED_EVENT_TYPE = 'plan.step.started';
export const PLAN_STEP_FINISHED_EVENT_TYPE = 'plan.step.finished';
export const TOOL_CALL_STARTED_EVENT_TYPE = 'tool.call.started';
export const TOOL_CALL_ARGS_EVENT_TYPE = 'tool.call.args';
export const TOOL_CALL_FINISHED_EVENT_TYPE = 'tool.call.finished';
export const REASONING_START_EVENT_TYPE = 'reasoning.start';
export const REASONING_CONTENT_EVENT_TYPE = 'reasoning.content';
export const REASONING_END_EVENT_TYPE = 'reasoning.end';

export const AGENTIC_STREAM_EVENT_TYPES = new Set<string>([
	PLAN_SNAPSHOT_EVENT_TYPE,
	PLAN_STEP_STARTED_EVENT_TYPE,
	PLAN_STEP_FINISHED_EVENT_TYPE,
	TOOL_CALL_STARTED_EVENT_TYPE,
	TOOL_CALL_ARGS_EVENT_TYPE,
	TOOL_CALL_FINISHED_EVENT_TYPE,
	REASONING_START_EVENT_TYPE,
	REASONING_CONTENT_EVENT_TYPE,
	REASONING_END_EVENT_TYPE
]);

// ---------------------------------------------------------------------------
// Plan snapshot — keyed by execution_id (from payload). One snapshot per plan.
// ---------------------------------------------------------------------------

export interface PlanStep {
	step_id: string;
	step_index: number;
	name: string;
	tool: string | null;
	depends_on: string[];
	depth: number;
	role: string | null;
	success_criteria: string | null;
	providing_agent_id: string | null;
	// Live-updated by plan.step.{started,finished}
	status?: 'pending' | 'running' | 'completed' | 'failed';
	started_at?: number;
	finished_at?: number;
	duration_ms?: number;
}

export interface PlanSnapshot {
	execution_id: string;
	plan_id: string;
	goal: string;
	steps: PlanStep[];
	updated_at: number;
}

const planMap = writable<Map<string, PlanSnapshot>>(new Map());

export const planSnapshots: Readable<Map<string, PlanSnapshot>> = {
	subscribe: planMap.subscribe
};

export function planSnapshotForExecution(
	executionId: string | null | undefined
): Readable<PlanSnapshot | null> {
	return derived(planMap, ($map) => (executionId ? $map.get(executionId) ?? null : null));
}

// ---------------------------------------------------------------------------
// Tool calls — list per execution_id, capped to MAX_TOOL_CALLS most recent.
// ---------------------------------------------------------------------------

const MAX_TOOL_CALLS_PER_EXECUTION = 200;

export interface ToolCallRecord {
	call_id: string;
	tool_name: string;
	execution_id: string | null;
	step_id: string | null;
	args: unknown;
	args_streaming?: string; // accumulated tool.call.args delta
	started_at: number;
	finished_at?: number;
	duration_ms?: number;
	success?: boolean;
	exit_code?: number | null;
	error?: string | null;
	content_preview?: string;
	status: 'running' | 'success' | 'error';
}

const toolCallsMap = writable<Map<string, ToolCallRecord[]>>(new Map());

export const toolCalls: Readable<Map<string, ToolCallRecord[]>> = {
	subscribe: toolCallsMap.subscribe
};

export function toolCallsForExecution(
	executionId: string | null | undefined
): Readable<ToolCallRecord[]> {
	return derived(toolCallsMap, ($map) => (executionId ? $map.get(executionId) ?? [] : []));
}

// ---------------------------------------------------------------------------
// Reasoning streams — keyed by trace_id. Caller appends content as it streams.
// ---------------------------------------------------------------------------

export interface ReasoningStream {
	trace_id: string;
	execution_id: string | null;
	model: string | null;
	budget_tokens: number | null;
	content: string; // accumulated deltas
	total_tokens?: number | null;
	duration_ms?: number | null;
	started_at: number;
	ended_at?: number;
	status: 'streaming' | 'ended';
}

const reasoningMap = writable<Map<string, ReasoningStream>>(new Map());

export const reasoningStreams: Readable<Map<string, ReasoningStream>> = {
	subscribe: reasoningMap.subscribe
};

export function reasoningStreamByTraceId(
	traceId: string | null | undefined
): Readable<ReasoningStream | null> {
	return derived(reasoningMap, ($map) => (traceId ? $map.get(traceId) ?? null : null));
}

// ---------------------------------------------------------------------------
// Envelope router — call from v2-websocket.ts for every inbound envelope.
// ---------------------------------------------------------------------------

export function handleAgenticStreamEvent(envelope: AgentEventEnvelope): void {
	const t = envelope.event_type;
	if (!AGENTIC_STREAM_EVENT_TYPES.has(t)) return;
	const payload = (envelope.payload ?? {}) as Record<string, unknown>;

	if (t === PLAN_SNAPSHOT_EVENT_TYPE) {
		ingestPlanSnapshot(payload);
		return;
	}
	if (t === PLAN_STEP_STARTED_EVENT_TYPE) {
		updateStepStatus(payload, 'running');
		return;
	}
	if (t === PLAN_STEP_FINISHED_EVENT_TYPE) {
		const status = (payload.status as string) === 'failed' ? 'failed' : 'completed';
		updateStepStatus(payload, status);
		return;
	}
	if (t === TOOL_CALL_STARTED_EVENT_TYPE) {
		ingestToolCallStarted(payload);
		return;
	}
	if (t === TOOL_CALL_ARGS_EVENT_TYPE) {
		appendToolCallArgsDelta(payload);
		return;
	}
	if (t === TOOL_CALL_FINISHED_EVENT_TYPE) {
		ingestToolCallFinished(payload);
		return;
	}
	if (t === REASONING_START_EVENT_TYPE) {
		ingestReasoningStart(payload);
		return;
	}
	if (t === REASONING_CONTENT_EVENT_TYPE) {
		appendReasoningContent(payload);
		return;
	}
	if (t === REASONING_END_EVENT_TYPE) {
		ingestReasoningEnd(payload);
	}
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

function ingestPlanSnapshot(payload: Record<string, unknown>): void {
	const executionId = String(payload.execution_id ?? '');
	if (!executionId) return;
	const stepsRaw = Array.isArray(payload.steps) ? (payload.steps as unknown[]) : [];
	const steps: PlanStep[] = stepsRaw.map((entry, idx) => {
		const e = (entry ?? {}) as Record<string, unknown>;
		return {
			step_id: String(e.step_id ?? `step_${idx}`),
			step_index: typeof e.step_index === 'number' ? e.step_index : idx,
			name: String(e.name ?? ''),
			tool: typeof e.tool === 'string' ? e.tool : null,
			depends_on: Array.isArray(e.depends_on) ? (e.depends_on as string[]) : [],
			depth: typeof e.depth === 'number' ? e.depth : 0,
			role: typeof e.role === 'string' ? e.role : null,
			success_criteria: typeof e.success_criteria === 'string' ? e.success_criteria : null,
			providing_agent_id:
				typeof e.providing_agent_id === 'string' ? e.providing_agent_id : null,
			status: 'pending'
		};
	});
	planMap.update((map) => {
		const next = new Map(map);
		next.set(executionId, {
			execution_id: executionId,
			plan_id: String(payload.plan_id ?? ''),
			goal: String(payload.goal ?? ''),
			steps,
			updated_at: Date.now()
		});
		return next;
	});
}

function updateStepStatus(
	payload: Record<string, unknown>,
	status: 'running' | 'completed' | 'failed'
): void {
	const executionId = String(payload.execution_id ?? '');
	const stepId = String(payload.step_id ?? '');
	if (!executionId || !stepId) return;
	planMap.update((map) => {
		const snapshot = map.get(executionId);
		if (!snapshot) return map;
		const steps = snapshot.steps.map((s) => {
			if (s.step_id !== stepId) return s;
			const next: PlanStep = { ...s, status };
			if (status === 'running' && typeof payload.started_at === 'number') {
				next.started_at = payload.started_at as number;
			}
			if ((status === 'completed' || status === 'failed') && typeof payload.finished_at === 'number') {
				next.finished_at = payload.finished_at as number;
			}
			if (typeof payload.duration_ms === 'number') {
				next.duration_ms = payload.duration_ms as number;
			}
			return next;
		});
		const updated = { ...snapshot, steps, updated_at: Date.now() };
		const out = new Map(map);
		out.set(executionId, updated);
		return out;
	});
}

function ingestToolCallStarted(payload: Record<string, unknown>): void {
	const callId = String(payload.call_id ?? '');
	if (!callId) return;
	const executionId = (payload.execution_id as string | null) ?? null;
	const key = executionId ?? '__no_execution__';
	const record: ToolCallRecord = {
		call_id: callId,
		tool_name: String(payload.tool_name ?? 'unknown'),
		execution_id: executionId,
		step_id: (payload.step_id as string | null) ?? null,
		args: payload.args ?? null,
		started_at: typeof payload.started_at === 'number' ? (payload.started_at as number) : Date.now(),
		status: 'running'
	};
	toolCallsMap.update((map) => {
		const list = map.get(key) ?? [];
		const next = [record, ...list].slice(0, MAX_TOOL_CALLS_PER_EXECUTION);
		const out = new Map(map);
		out.set(key, next);
		return out;
	});
}

function appendToolCallArgsDelta(payload: Record<string, unknown>): void {
	const callId = String(payload.call_id ?? '');
	const delta = String(payload.delta ?? '');
	if (!callId || !delta) return;
	toolCallsMap.update((map) => {
		const out = new Map(map);
		for (const [key, list] of out) {
			const idx = list.findIndex((r) => r.call_id === callId);
			if (idx === -1) continue;
			const updated = { ...list[idx], args_streaming: (list[idx].args_streaming ?? '') + delta };
			const nextList = [...list];
			nextList[idx] = updated;
			out.set(key, nextList);
			break;
		}
		return out;
	});
}

function ingestToolCallFinished(payload: Record<string, unknown>): void {
	const callId = String(payload.call_id ?? '');
	if (!callId) return;
	const success = Boolean(payload.success);
	toolCallsMap.update((map) => {
		const out = new Map(map);
		for (const [key, list] of out) {
			const idx = list.findIndex((r) => r.call_id === callId);
			if (idx === -1) continue;
			const prior = list[idx];
			const updated: ToolCallRecord = {
				...prior,
				finished_at:
					typeof payload.finished_at === 'number' ? (payload.finished_at as number) : Date.now(),
				duration_ms: typeof payload.duration_ms === 'number' ? (payload.duration_ms as number) : undefined,
				success,
				exit_code:
					typeof payload.exit_code === 'number' ? (payload.exit_code as number) : null,
				error: typeof payload.error === 'string' ? (payload.error as string) : null,
				content_preview:
					typeof payload.content_preview === 'string'
						? (payload.content_preview as string)
						: undefined,
				status: success ? 'success' : 'error'
			};
			const nextList = [...list];
			nextList[idx] = updated;
			out.set(key, nextList);
			break;
		}
		return out;
	});
}

function ingestReasoningStart(payload: Record<string, unknown>): void {
	const traceId = String(payload.trace_id ?? '');
	if (!traceId) return;
	const stream: ReasoningStream = {
		trace_id: traceId,
		execution_id: (payload.execution_id as string | null) ?? null,
		model: typeof payload.model === 'string' ? payload.model : null,
		budget_tokens: typeof payload.budget_tokens === 'number' ? (payload.budget_tokens as number) : null,
		content: '',
		started_at: typeof payload.started_at === 'number' ? (payload.started_at as number) : Date.now(),
		status: 'streaming'
	};
	reasoningMap.update((map) => {
		const out = new Map(map);
		out.set(traceId, stream);
		return out;
	});
}

function appendReasoningContent(payload: Record<string, unknown>): void {
	const traceId = String(payload.trace_id ?? '');
	const delta = String(payload.delta ?? '');
	if (!traceId) return;
	reasoningMap.update((map) => {
		const prior = map.get(traceId);
		const out = new Map(map);
		if (prior) {
			out.set(traceId, { ...prior, content: prior.content + delta });
		} else {
			// `content` event arrived before `start` — synthesize a stub so the
			// UI doesn't lose data on out-of-order delivery.
			out.set(traceId, {
				trace_id: traceId,
				execution_id: null,
				model: null,
				budget_tokens: null,
				content: delta,
				started_at: Date.now(),
				status: 'streaming'
			});
		}
		return out;
	});
}

function ingestReasoningEnd(payload: Record<string, unknown>): void {
	const traceId = String(payload.trace_id ?? '');
	if (!traceId) return;
	reasoningMap.update((map) => {
		const prior = map.get(traceId);
		if (!prior) return map;
		const out = new Map(map);
		out.set(traceId, {
			...prior,
			status: 'ended',
			ended_at: typeof payload.ended_at === 'number' ? (payload.ended_at as number) : Date.now(),
			total_tokens: typeof payload.total_tokens === 'number' ? (payload.total_tokens as number) : null,
			duration_ms: typeof payload.duration_ms === 'number' ? (payload.duration_ms as number) : null
		});
		return out;
	});
}

// Test-only: clear all stream state.
export function __resetAgenticStreamStoreForTests(): void {
	planMap.set(new Map());
	toolCallsMap.set(new Map());
	reasoningMap.set(new Map());
}
