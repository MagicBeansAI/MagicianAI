export interface ExecutionSummaryRecord {
	execution_id: string;
	plan_id: string;
	step_id: string;
	outcome: string;
	iterations_used: number;
	artifacts: string[];
	duration_ms: number;
	summary: string;
	timestamp: number;
	loop_detection_type?: string | null;
	loop_repeated_action?: string | null;
	loop_recommendation?: string | null;
	loop_cycle_pattern?: string[] | null;
	loop_similarity?: number | null;
	budget_dimension?: string | null;
	budget_details?: string | null;
	cannot_proceed_reason?: string | null;
}

export interface ExecutionResponsibilityChild {
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

export interface ExecutionResponsibilitySnapshot {
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
	active_children: ExecutionResponsibilityChild[];
}
