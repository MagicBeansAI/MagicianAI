export type FeedItemType =
	| 'task'
	| 'approval'
	| 'agent_message'
	| 'data_delivery'
	| 'routine_result'
	| 'escalation'
	| 'learning_candidate'
	| 'learning_insight'
	// User-facing distilled knowledge the agent has accumulated —
	// research findings, contacts learned, routines observed, user
	// preferences detected, agent heuristics distilled. Surfaces on
	// `/today`. Distinct from `learning_insight` / `learning_candidate`
	// which are internal process telemetry shown on `/feed`.
	| 'agent_learning';

export type FeedItemStatus = 'running' | 'done' | 'needs_action' | 'failed' | 'info';

export interface FeedAction {
	id: string;
	label: string;
	action_type?: string | null;
	payload: unknown;
}

export interface FeedItem {
	id: string;
	principal: string;
	workspace: string;
	item_type: FeedItemType;
	task_id?: string | null;
	ui_thread_id?: string | null;
	agent_id?: string | null;
	title: string;
	summary?: string | null;
	status: FeedItemStatus;
	created_at: number;
	updated_at: number;
	actions: FeedAction[];
	metadata: unknown;
}

export interface FeedItemPatch {
	title?: string;
	summary?: string | null;
	status?: FeedItemStatus;
	task_id?: string | null;
	ui_thread_id?: string | null;
	agent_id?: string | null;
	updated_at?: number;
	metadata?: unknown;
}

export interface FeedAttentionCounts {
	requests: number;
	approvals: number;
	escalations: number;
	needs_action: number;
	failed: number;
	running: number;
}

export interface FeedCounts {
	total: number;
	running: number;
	needs_action: number;
	failed: number;
	done: number;
	info: number;
}

/** True per-lane totals (SQL COUNT, independent of the returned page) — used to
 *  decide whether the paginated inbox has more to load. */
export interface FeedAttentionTotals {
	requests: number;
	approvals: number;
	escalations: number;
	failed: number;
	running: number;
}

export interface FeedAttentionLanePageMeta {
	total: number;
	limit: number;
	cursor?: string | null;
	next_cursor?: string | null;
	has_more: boolean;
}

export interface FeedAttentionPages {
	requests: FeedAttentionLanePageMeta;
	approvals: FeedAttentionLanePageMeta;
	escalations: FeedAttentionLanePageMeta;
	failed: FeedAttentionLanePageMeta;
	running: FeedAttentionLanePageMeta;
}

export interface FeedAttentionResponse {
	counts: FeedAttentionCounts;
	totals?: FeedAttentionTotals;
	pages?: FeedAttentionPages;
	requests: FeedItem[];
	approvals: FeedItem[];
	escalations: FeedItem[];
	failed: FeedItem[];
	running: FeedItem[];
}
