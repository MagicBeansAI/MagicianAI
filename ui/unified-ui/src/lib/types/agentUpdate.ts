// Agent update event vocabulary — TypeScript mirror of
// `magician/src/magician_v2/agents/agent_update.rs`.
//
// Keep this file in sync with the Rust enum. When Rust adds a variant, add
// it here; when Rust renames a field, rename here. A hand-written mirror is
// acceptable for now; a `ts-rs` codegen path is on the roadmap.
//
// Wire shape: a single flat object with `kind` as discriminator. Envelope
// fields (id, ts, workspace_id, agent_id, thread_id?, cycle_id?) ride
// alongside the variant fields.

import type { AgentKind } from './agents';

// ---------------------------------------------------------------------------
// Id aliases (strings on the wire)
// ---------------------------------------------------------------------------

export type AgentId = string;
export type CycleId = string;
export type GoalId = string;
export type ApprovalId = string;
export type ArtifactId = string;
export type TaskId = string;
export type WorkspaceId = string;
export type ThreadId = string;

// ---------------------------------------------------------------------------
// Support types
// ---------------------------------------------------------------------------

export type CycleOutcome = 'succeeded' | 'failed' | 'paused' | 'partially_succeeded';

export type ApprovalDecision = 'approve' | 'reject';

export interface ArtifactRef {
	artifact_id: ArtifactId;
	kind: string;
	surface_url?: string;
	label?: string;
}

// ---------------------------------------------------------------------------
// AgentUpdateKind — discriminated union matching Rust `#[serde(tag = "kind")]`
// ---------------------------------------------------------------------------

export type AgentUpdateKind =
	// Agent lifecycle
	| { kind: 'agent_created'; name: string; agent_kind: AgentKind }
	| { kind: 'agent_updated'; changed_fields: string[] }
	| { kind: 'agent_deleted' }
	| { kind: 'agent_paused'; reason?: string }
	| { kind: 'agent_resumed' }
	// Cycle
	| { kind: 'cycle_started'; focus_area?: string; trigger: string }
	| { kind: 'cycle_completed'; outcome: CycleOutcome; duration_ms: number }
	| { kind: 'cycle_failed'; error: string; duration_ms: number }
	| { kind: 'cycle_paused'; reason: string }
	// Goal
	| { kind: 'goal_completed'; goal_id: GoalId; artifacts?: ArtifactRef[] }
	| { kind: 'goal_failed'; goal_id: GoalId; error: string }
	| { kind: 'goal_recovered'; goal_id: GoalId; recovery_strategy: string }
	// Approval
	| {
			kind: 'approval_requested';
			approval_id: ApprovalId;
			tool?: string;
			action?: string;
			params?: unknown;
			pending_action_count?: number;
	  }
	| {
			kind: 'approval_resolved';
			approval_id: ApprovalId;
			decision: ApprovalDecision;
			resolver: string;
	  }
	| { kind: 'approval_expired'; approval_id: ApprovalId }
	// Artifact
	| { kind: 'artifact_created'; artifact: ArtifactRef }
	| { kind: 'artifact_create_failed'; attempted_kind: string; error: string }
	// Circuit breaker
	| { kind: 'circuit_opened'; scope: string; reason: string; next_retry_at?: number }
	| { kind: 'circuit_recovered'; scope: string; recovered_at: number }
	// Task
	| { kind: 'task_created'; task_id: TaskId; title: string }
	| { kind: 'task_updated'; task_id: TaskId; changed_fields: string[] }
	| { kind: 'task_completed'; task_id: TaskId; artifacts?: ArtifactRef[] }
	| { kind: 'task_failed'; task_id: TaskId; error: string }
	// Memory / feedback (aggregated)
	| {
			kind: 'memory_report';
			summary: string;
			episodes_processed: number;
			corrections_applied: number;
	  }
	| { kind: 'feedback_generated'; corrections: number; success_patterns: number }
	// Tier consolidation
	| {
			kind: 'tier_consolidated';
			source: string;
			target: string;
			records_written: number;
			rule: string;
	  }
	// Delegation
	| {
			kind: 'delegation_issued';
			from_agent: AgentId;
			to_agent: AgentId;
			task_id: TaskId;
	  }
	| {
			kind: 'delegation_resolved';
			from_agent: AgentId;
			to_agent: AgentId;
			task_id: TaskId;
			outcome: string;
	  }
	// Health
	| { kind: 'feed_stalled'; silent_for_ms: number }
	// Publication
	| { kind: 'published_surface_changed'; surface_id: string; url: string };

/** String-valued `kind` tags, derived from the discriminated union. */
export type AgentUpdateKindTag = AgentUpdateKind['kind'];

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

export interface AgentUpdateEnvelope {
	id: string;
	ts: number;
	workspace_id: WorkspaceId;
	agent_id: AgentId;
	thread_id?: ThreadId;
	cycle_id?: CycleId;
}

/** Full wire shape: envelope fields + variant fields + `kind` discriminator. */
export type AgentUpdate = AgentUpdateEnvelope & AgentUpdateKind;
