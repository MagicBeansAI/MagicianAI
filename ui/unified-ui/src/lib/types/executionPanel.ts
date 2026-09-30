import type { FeedItem } from '$lib/feed/types';
import type { HitlOpenTarget } from '$lib/hitl/types';
import type { TaskPriority, TaskStatus, TaskTag } from '$lib/stores/taskStore';
import type { ExecutionSummaryRecord } from '$lib/types/executionResponsibility';

export type ExecutionPanelTab = 'plan' | 'run' | 'output' | 'debug';

export interface ExecutionPanelOverview {
	task_id: string;
	execution_id?: string | null;
	principal: string;
	workspace: string;
	ui_thread_id: string;
	title: string;
	description: string;
	status: TaskStatus;
	priority?: TaskPriority | null;
	assigned_agent_id: string;
	active_agent_id?: string | null;
	has_plan: boolean;
	progress?: number | null;
	current_step?: number | null;
	created_at: number;
	updated_at: number;
}

export interface ExecutionPanelRunState {
	summary?: string | null;
	responsibility?: ExecutionPanelResponsibilityState | null;
	pending_questions: ExecutionPanelClarificationQuestion[];
	needs_attention: ExecutionPanelAttentionItem[];
	recent_activity: FeedItem[];
	/** Full seq-ordered humanized event log (chronological). Powers the
	 *  tab-less deep-work feed. Optional for back-compat with payloads
	 *  that predate the field. */
	activity_log?: FeedItem[];
	/** One entry per delegated child contributing to `activity_log`, so each
	 *  delegation renders as its own collapsible group with a rollup header.
	 *  Optional: payloads that predate the field, and runs that delegated
	 *  nothing, both omit it. */
	delegations?: ExecutionPanelDelegationGroup[];
}

/**
 * A delegated child execution whose events appear in the parent's log.
 *
 * Group log entries by `execution_id` — matched against each entry's
 * `metadata.execution_id` — never by `agent_id`: the same agent can be
 * delegated to more than once in a run, and each of those is its own child
 * with its own status.
 */
export interface ExecutionPanelDelegationGroup {
	execution_id: string;
	agent_id: string;
	/** The child's own status, not the parent's. */
	status: string;
	/** How many log entries belong to this child. */
	entry_count: number;
	parent_execution_id?: string | null;
	started_at: string;
	completed_at?: string | null;
}

export interface ExecutionPanelOutputResult {
	summary?: string | null;
	outcome?: string | null;
	artifact_names: string[];
}

export interface ExecutionPanelRecentRun {
	execution_id: string;
	started_at: number;
	ended_at?: number | null;
	status: TaskStatus;
	completion_summary?: string | null;
	completion_outcome?: string | null;
	completion_artifact_names: string[];
	current_step?: number | null;
	progress?: number | null;
	error_message?: string | null;
}

export interface ExecutionPanelOutputRef {
	output_id: string;
	scope: string;
	audience: string;
	role: string;
	relative_path: string;
	media_type: string;
	created_at: string;
	source_execution_id?: string | null;
	source_plan_id?: string | null;
	source_output_ids?: string[];
}

/** Bounded metadata for one artifact persisted by the selected execution. */
export interface ExecutionPanelArtifactRef {
	artifact_id: string;
	artifact_type: string;
	content_type: string;
	produced_at: string;
	source_execution_id?: string | null;
	display_name?: string | null;
	/** Task-root-relative path when the artifact is file-backed. */
	relative_path?: string | null;
	size_bytes?: number | null;
}

export interface ExecutionPanelOutputState {
	result?: ExecutionPanelOutputResult | null;
	deliveries: FeedItem[];
	recent_runs: ExecutionPanelRecentRun[];
	/** Direct outputs written by the execution selected in `overview`. */
	selected_execution_outputs?: ExecutionPanelOutputRef[];
	/** Outputs collected from work delegated by that selected execution. */
	selected_child_outputs?: ExecutionPanelOutputRef[];
	/** Undefined on older/unavailable payloads; an empty array is known-empty. */
	selected_execution_artifacts?: ExecutionPanelArtifactRef[];
}

export interface ExecutionPanelLinkedInput {
	task_id: string;
	execution_id?: string | null;
	title: string;
	artifact_chain_id?: string | null;
	artifact_names: string[];
}

export interface ExecutionPanelStepStatus {
	number: number;
	name: string;
	status: string;
	progress: string;
	step_id?: string | null;
	capability?: string | null;
	confidence?: number | null;
	delegate_agent_id?: string | null;
}

export interface ExecutionPanelExecutionContext {
	execution_id: string;
	status: TaskStatus;
	started_at?: number | null;
	ended_at?: number | null;
	error_message?: string | null;
	summary?: string | null;
	outcome?: string | null;
	artifact_names: string[];
	current_step?: number | null;
	progress?: number | null;
	plan_ref?: string | null;
	plan_id?: string | null;
	artifact_chain_id?: string | null;
	linked_inputs: ExecutionPanelLinkedInput[];
	step_statuses: ExecutionPanelStepStatus[];
}

export interface ExecutionPanelShellLine {
	text: string;
	stream: string;
	timestamp: number;
}

export interface ExecutionPanelShellEntry {
	step_id: string;
	step_index: number;
	command: string;
	lines: ExecutionPanelShellLine[];
	exit_code?: number | null;
	is_complete: boolean;
	started_at: number;
	execution_id: string;
}

export interface ExecutionPanelTimelineEntry {
	id: string;
	timestamp: number;
	severity: 'trace' | 'info' | 'warning' | 'error' | 'critical';
	title: string;
	message: string;
	execution_id?: string | null;
	agent_id?: string | null;
	step_id?: string | null;
}

export interface ExecutionPanelObservation {
	observation_id: string;
	captured_at: number;
	step_id?: string | null;
	page_stage?: string | null;
	url?: string | null;
	has_screenshot: boolean;
}

export interface ExecutionPanelClarificationOption {
	value: string;
	label: string;
	description?: string | null;
}

export interface ExecutionPanelClarificationSubmission {
	mode: string;
	input_type?: string | null;
	pause_state_id?: string | null;
	plan_id?: string | null;
	step_id?: string | null;
	agent_id?: string | null;
	goal_id?: string | null;
	cycle_id?: string | null;
}

export interface ExecutionPanelClarificationQuestion {
	id: string;
	question_text: string;
	status: string;
	context_snippets: string[];
	related_slots: string[];
	source_slot_id?: string | null;
	slot_confidence?: number | null;
	options: ExecutionPanelClarificationOption[];
	submission?: ExecutionPanelClarificationSubmission | null;
	hitl_request?: HitlOpenTarget | null;
}

export interface ExecutionPanelAttentionItem extends FeedItem {
	hitl_request?: HitlOpenTarget | null;
}

export interface ExecutionPanelResponsibilityChild {
	execution_id: string;
	title?: string | null;
	waiting_state: string;
	active_owner_agent_id: string;
	delegation_chain: string[];
	current_stage?: string | null;
	current_provider?: string | null;
	latest_summary?: ExecutionSummaryRecord | null;
	is_blocking: boolean;
}

export interface ExecutionPanelResponsibilityState {
	execution_id: string;
	parent_execution_id?: string | null;
	waiting_state: string;
	active_owner_agent_id: string;
	owner_stack: string[];
	owner_chain: string[];
	handover_active: boolean;
	waiting_on_children: boolean;
	active_child_count: number;
	historical_child_count: number;
	responsibility_summary: string;
	current_stage?: string | null;
	current_provider?: string | null;
	paused_from_state?: string | null;
	latest_summary?: ExecutionSummaryRecord | null;
	active_children: ExecutionPanelResponsibilityChild[];
}

export interface ExecutionPanelTaskplanDocument {
	execution_id: string;
	markdown: string;
}

export interface ExecutionPanelDebugState {
	selected_execution?: ExecutionPanelExecutionContext | null;
	taskplan?: ExecutionPanelTaskplanDocument | null;
	timeline: ExecutionPanelTimelineEntry[];
	observations: ExecutionPanelObservation[];
	shell_entries: ExecutionPanelShellEntry[];
	latest_error_message?: string | null;
	history_count: number;
	tags: TaskTag[];
}

export interface ExecutionPanelState {
	default_tab: ExecutionPanelTab;
	overview: ExecutionPanelOverview;
	run: ExecutionPanelRunState;
	output: ExecutionPanelOutputState;
	debug: ExecutionPanelDebugState;
}
