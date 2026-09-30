import { get } from 'svelte/store';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';

/**
 * Quests = the scope's v3 TASKS, read game-side (light fetcher in the house
 * style of the other bounded Town Square readers - no heavyweight store). The Quest Log
 * lists them; the Quest Sheet shows one. Best-effort: null on failure.
 */

export interface Quest {
	id: string;
	title: string;
	description: string;
	/** Normalized lowercase status (running/planning/paused/pending/completed/failed/...). */
	status: string;
	/** Game-facing lifecycle derived from the complete task contract. */
	state: QuestState;
	agentId: string | null;
	/** Active root execution (drives ExecutionControls), else latest known. */
	executionId: string | null;
	/** True when the active root execution is live (controls make sense). */
	active: boolean;
	priority: string | null;
	dueDate: string | null;
	dependsOn: string[];
	isBlocked: boolean;
	currentStep: string | null;
	currentSubstep: string | null;
	pendingQuestions: QuestQuestion[];
	hasPlan: boolean;
	planId: string | null;
	planStatus: string | null;
	synthesisPending: boolean;
	synthesisFailedExecutionId: string | null;
	completionSummary: string | null;
	completionOutcome: string | null;
	artifacts: string[];
	createdAt: number | null;
	updatedAt: number | null;
}

export type QuestState =
	| 'planning'
	| 'ready'
	| 'active'
	| 'awaiting_orders'
	| 'blocked'
	| 'delivering'
	| 'succeeded'
	| 'failed'
	| 'cancelled';

export interface QuestQuestion {
	id: string | null;
	prompt: string;
}

export function questPhase(q: Quest): 'active' | 'waiting' | 'done' {
	if (q.state === 'active' || q.state === 'planning' || q.state === 'delivering') {
		return 'active';
	}
	if (q.state === 'ready' || q.state === 'blocked' || q.state === 'awaiting_orders') {
		return 'waiting';
	}
	return 'done';
}

function asRecord(v: unknown): Record<string, unknown> {
	return v && typeof v === 'object' ? (v as Record<string, unknown>) : {};
}

function str(v: unknown): string | null {
	return typeof v === 'string' && v.trim() ? v : null;
}

function strings(v: unknown): string[] {
	return Array.isArray(v) ? v.map(str).filter((item): item is string => item != null) : [];
}

function bool(v: unknown): boolean {
	return v === true;
}

function question(v: unknown): QuestQuestion | null {
	const rec = asRecord(v);
	const prompt = str(rec.prompt) ?? str(rec.question) ?? str(rec.text);
	if (!prompt) return null;
	return { id: str(rec.id) ?? str(rec.question_id), prompt };
}

function toMs(v: unknown): number | null {
	if (typeof v === 'number' && Number.isFinite(v)) return v > 1e12 ? v : v * 1000;
	if (typeof v === 'string') {
		const parsed = Date.parse(v);
		return Number.isNaN(parsed) ? null : parsed;
	}
	return null;
}

function questState(status: string, state: Record<string, unknown>): QuestState {
	if (status === 'cancelled' || status === 'canceled') return 'cancelled';
	if (status === 'failed' || status === 'error') return 'failed';
	if (bool(state.is_blocked) || status === 'blocked') return 'blocked';
	const pendingQuestions = Array.isArray(state.pending_questions)
		? state.pending_questions
		: state.pending_question
			? [state.pending_question]
			: [];
	if (
		pendingQuestions.length > 0 ||
		status === 'paused' ||
		status === 'waiting_for_user' ||
		status === 'waiting'
	) {
		return 'awaiting_orders';
	}
	if (bool(state.synthesis_pending) || status === 'delivering' || status === 'synthesizing') {
		return 'delivering';
	}
	if (status === 'completed' || status === 'complete' || status === 'done' || status === 'succeeded') {
		return 'succeeded';
	}
	if (status === 'running' || status === 'in_progress' || status === 'active') return 'active';
	if (status === 'planning' || status === 'draft') return 'planning';
	return 'ready';
}

/** Tolerant of both flat rows and {manifest, state} shapes. */
export function taskToQuest(raw: unknown): Quest | null {
	const rec = asRecord(raw);
	const manifest = { ...rec, ...asRecord(rec.manifest) };
	const state = { ...rec, ...asRecord(rec.state) };
	const id = str(manifest.id) ?? str(rec.id);
	if (!id) return null;
	const activeExec = str(state.active_root_execution_id);
	const latestExec =
		activeExec ??
		str(state.latest_root_execution_id) ??
		str(state.last_completed_root_execution_id);
	const status = (str(state.status) ?? 'unknown').toLowerCase();
	const pendingQuestions = (Array.isArray(state.pending_questions)
		? state.pending_questions
		: state.pending_question
			? [state.pending_question]
			: []
	)
		.map(question)
		.filter((item): item is QuestQuestion => item != null);
	return {
		id,
		title: str(manifest.title) ?? id,
		description: str(manifest.description) ?? '',
		status,
		state: questState(status, state),
		agentId: str(manifest.agent_id),
		executionId: latestExec,
		active: activeExec != null,
		priority: str(manifest.priority),
		dueDate: str(manifest.due_date),
		dependsOn: strings(manifest.depends_on),
		isBlocked: bool(state.is_blocked),
		currentStep: str(state.current_step_title),
		currentSubstep: str(state.current_substep_title),
		pendingQuestions,
		hasPlan: bool(state.has_plan),
		planId: str(state.approved_plan_id) ?? str(state.latest_plan_id),
		planStatus: str(state.plan_status),
		synthesisPending: bool(state.synthesis_pending),
		synthesisFailedExecutionId: str(state.synthesis_failed_execution_id),
		completionSummary: str(state.completion_summary),
		completionOutcome: str(state.completion_outcome),
		artifacts: strings(state.completion_artifact_names),
		createdAt: toMs(manifest.created_at),
		updatedAt: toMs(state.updated_at) ?? toMs(manifest.updated_at)
	};
}

export async function fetchQuests(limit = 40): Promise<Quest[] | null> {
	const scope = get(scopeIdentityStore);
	if (!scope.isResolved) return null;
	try {
		const params = new URLSearchParams({
			limit: String(limit)
		});
		const res = await timedFetch(`/api/magician/v3/tasks?${params.toString()}`);
		if (!res.ok) return null;
		const body = (await res.json()) as { tasks?: unknown[] };
		const quests = (body.tasks ?? [])
			.map(taskToQuest)
			.filter((q): q is Quest => q !== null);
		// newest activity first
		quests.sort((a, b) => (b.updatedAt ?? b.createdAt ?? 0) - (a.updatedAt ?? a.createdAt ?? 0));
		return quests;
	} catch {
		return null;
	}
}
