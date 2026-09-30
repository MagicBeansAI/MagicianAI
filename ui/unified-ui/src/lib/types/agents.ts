// Unified Agentic Architecture contract types.
// Goals and triggers have been REMOVED from agents — schedules now live on tasks.

export type AgentStatus = 'idle' | 'running' | 'error' | 'paused' | 'disabled';

/** Agent kind determines capabilities and coordination behavior. */
export type AgentKind = 'Personal' | 'Worker';

/**
 * Resolve the effective tool list for an agent (mirrors backend `resolved_tools`).
 * When `tools` is empty → all available tools (minus excluded).
 * When `tools` has entries → only those (minus excluded).
 *
 * `availableTools` is the live pack list from the backend — no hardcoded set.
 */
export function resolveAgentTools(
	tools: string[],
	excludedTools: string[],
	availableTools: readonly string[],
): string[] {
	const base = tools.length === 0 ? availableTools : tools;
	return base.filter(t => !excludedTools.includes(t));
}

export interface AgentToolInfo {
	agent_id: string;
	name?: string;
	kind?: AgentKind;
	tools: string[];
	excluded_tools: string[];
	delegation_targets?: string[];
}

/** Created-by provenance for tasks. Backend uses 'autonomous' with serde alias 'agent'. */
export type TaskCreatedBy = 'user' | 'autonomous' | 'system' | 'delegation';

/** Focus area priority levels. */
export type FocusAreaPriority = 'high' | 'medium' | 'low';

/** A focus area for autonomous agent operation. */
export interface FocusArea {
    name: string;
    description: string;
    priority: FocusAreaPriority;
    schedule?: string;
    program?: string;
    scope?: string[];
}

/** Configuration for autonomous agent behavior. */
export interface AutonomousConfig {
    schedule: string;      // cron expression
    focus_areas: FocusArea[];
    max_tasks_per_cycle: number;
    max_steps_per_plan: number;
}

/** Optional harness capability configuration for a personal agent. */
export interface HarnessConfig {
    program_section?: string;
}

/** Memory isolation mode for personal agents. */
export type UserMemoryIsolation = 'shared' | 'fully_isolated';

export interface AgentSummary {
    agent_id: string;
    name: string;
    description: string;
    status: AgentStatus;
    kind: AgentKind;
    trust_level: string;
    disabled?: boolean;
    configured_disabled?: boolean;
    tools: string[];
    excluded_tools: string[];
    last_cycle_at: string | null;
    consecutive_failures: number;
    delegation_targets: string[];
    is_primary: boolean;
    autonomous_config?: AutonomousConfig;
    harness?: HarnessConfig;
    readable_agents: string[];
    user_memory_isolation: UserMemoryIsolation;
}

export interface CoordinationView {
    shared_tiers: string[];
    max_delegation_depth: number;
}

export interface ConstraintView {
    max_iterations: number;
    max_tokens_per_cycle: number;
    max_consecutive_failures: number;
    requires_approval: boolean;
    allow_self_modification: boolean;
}

export interface AgentDetail extends AgentSummary {
    memory_tiers: TierSummary[];
    coordination: CoordinationView;
    constraints: ConstraintView;
    recent_episodes: EpisodeSummary[];
}

export interface CycleView {
    cycle_id: string;
    agent_id: string;
    status: 'running' | 'completed' | 'failed';
    started_at: string;
    completed_at: string | null;
    steps_completed: number;
    outcome: string | null;
}

export interface TierSummary {
    name: string;
    scope: string;
    entry_count: number;
    last_updated_at: string | null;
    shared: boolean;
}

export interface EpisodeSummary {
    episode_id: string;
    outcome: 'success' | 'failure' | 'partial';
    started_at: string;
    duration_ms: number;
    actions_taken: number;
}

export interface ArtifactView {
    key: string;
    artifact_type: string;
    agent_id: string;
    produced_at: string;
    content_preview: string;
    size_bytes: number;
}

export interface ApprovalView {
    approval_id: string;
    agent_id: string;
    agent_name: string;
    action_summary: string;
    status: 'pending' | 'approved' | 'rejected' | 'expired';
    created_at: string;
    expires_at: string;
}

/** Schedule on a task (not an agent). */
export interface TaskSchedule {
    cron: string;
    timezone?: string;
    /** Execution history retention policy. */
    execution_history_retention?: ExecutionHistoryRetention;
    /** Maximum number of times this schedule should fire before retiring.
     *  Omit / undefined → runs indefinitely. Enforcement is owned by the
     *  scheduler tick loop (Phase-2 backend change); Phase-1 only persists
     *  the declared limit alongside the rest of the schedule. */
    max_runs?: number;
    /** User-controlled pause flag. When `true`, the scheduler skips this
     *  task at hydration time (it never re-registers with the cron
     *  service). Toggle back to `false` / undefined to resume. Independent
     *  of task `status` — "schedule paused" and "execution paused" are
     *  different concepts. */
    paused?: boolean;
}

/** Policy for trimming old execution-history records. */
export interface ExecutionHistoryRetention {
    /** Maximum number of records to keep (takes precedence over max_age_days). */
    max_records?: number;
    /** Maximum age in days — records older than this are pruned. */
    max_age_days?: number;
}
