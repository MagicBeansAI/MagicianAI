// Recurring Monitors — Phase 0 contract mirror (hand-written TypeScript over
// the CANONICAL JSON fixtures in magician/tests/fixtures/monitors/; plan:
// docs/plans/2026-07-21-recurring-monitors-productization-design-implementation.md
// §6–§8). No runtime code — the vitest sibling (monitorContract.test.ts)
// parses the shared fixtures against these shapes so backend/web/iOS break
// together on drift. Phase 1 backs these with real endpoints.

export type MonitorMatchMode = 'strict' | 'balanced' | 'broad';
export type MonitorNotificationPolicy = 'material_changes' | 'every_run' | 'never';
export type MonitorRunStatus = 'baseline' | 'changed' | 'unchanged' | 'degraded' | 'failed';
export type MonitorFindingClassification = 'new' | 'updated' | 'unchanged' | 'possibly_removed';

export interface MonitorSpecV1 {
	schema_version: 1;
	objective: string;
	query_seeds: string[];
	sources: {
		urls: string[];
		domains: string[];
		authenticated_sources: string[];
	};
	include_rules: string[];
	exclude_rules: string[];
	match_mode: MonitorMatchMode;
	notification_policy: MonitorNotificationPolicy;
	notify_initial_baseline: boolean;
}

export interface MonitorEvidenceV1 {
	kind: string;
	value: string;
	url?: string;
}

export interface MonitorFindingV1 {
	stable_key: string;
	title: string;
	canonical_url?: string | null;
	source: string;
	observed_at: string;
	published_at?: string | null;
	summary: string;
	why_it_matters: string;
	entities: string[];
	evidence: MonitorEvidenceV1[];
	content_fingerprint: string;
	classification: MonitorFindingClassification;
}

export interface MonitorSourceOutcomeV1 {
	source: string;
	status: 'ok' | 'auth_failed' | 'timeout' | 'rate_limited' | 'error';
	complete: boolean;
	items_scanned: number;
	note?: string;
}

export interface MonitorRunResultV1 {
	monitor_task_id: string;
	execution_id: string;
	monitor_revision: number;
	started_at: string;
	completed_at: string;
	status: MonitorRunStatus;
	complete_scan: boolean;
	source_outcomes: MonitorSourceOutcomeV1[];
	counts: {
		scanned: number;
		new: number;
		updated: number;
		unchanged: number;
		possibly_removed: number;
	};
	findings: MonitorFindingV1[];
	run_fingerprint: string;
	/**
	 * Present ONLY when status is `changed`. Finalized `baseline` results
	 * never carry it (a baseline has no previous state to have changed
	 * from); an opted-in baseline notification dedupes on the execution id.
	 */
	change_fingerprint?: string;
	/** Present when a source needs the user (login/permission). */
	access_problem?: {
		source: string;
		kind: string;
		message: string;
		since: string;
	};
}

/** One row of `GET /monitors?limit=&cursor=`. */
export interface MonitorListItemV1 {
	task_id: string;
	title: string;
	objective: string;
	state: 'active' | 'paused';
	cadence_summary: string;
	monitor_revision: number;
	last_run_at?: string;
	/**
	 * The type admits the full run-status vocabulary for forward
	 * compatibility, but the Phase 1 backend derives this from `TaskState`
	 * alone and only ever emits `never_ran` | `unchanged` | `failed`
	 * (`monitors_api::monitor_list_item`). `baseline`/`changed`/`degraded`
	 * appear here only once list rows read the Phase 2 run ledger.
	 */
	last_run_status: MonitorRunStatus | 'never_ran';
	/**
	 * Reserved; NOT emitted yet. Next-fire time is in-memory scheduler
	 * state in Phase 1 — the fixture models the Phase 2+ shape, the live
	 * endpoint never sends this key.
	 */
	next_run_at?: string;
	/**
	 * `failing` is reserved-not-emitted: the Phase 1 backend only emits
	 * `ok` | `needs_attention` (task status `failed` → `needs_attention`).
	 * Kept in the union for fixture compatibility and the intended Phase 2+
	 * escalation ladder.
	 */
	health: 'ok' | 'needs_attention' | 'failing';
}

/**
 * Cursor envelope (plan §8: `limit=&cursor=`, not offset) — plus the two
 * counting keys the list index added.
 */
export interface MonitorListPageV1 {
	items: MonitorListItemV1[];
	next_cursor: string | null;
	limit: number;
	/**
	 * Rows in the corpus this page is a window onto: after the `state` filter,
	 * never reduced by paging.
	 *
	 * **Optional because absence is meaningful.** A server that predates the
	 * counting envelope sends no `total`, and that must read as "no page count
	 * is available" — cursor paging only — never as "zero rows".
	 */
	total?: number;
	/**
	 * The position the CURSOR resolved to, not a number the caller sent (there
	 * is no `offset` request parameter). Absent from the same older servers as
	 * `total`; then the page index carries the position.
	 */
	offset?: number;
}

export interface MonitorUpdateDetailV1 {
	update_id: string;
	monitor_task_id: string;
	monitor_revision: number;
	execution_id: string;
	occurred_at: string;
	/**
	 * The producing run's status. The canonical fixture pins `changed`; the
	 * Phase 3 ledger also records baselines and (under `every_run`) quiet
	 * receipts, so the full run-status vocabulary is legal here.
	 */
	status: MonitorRunStatus;
	/** Absent on updates without a change fingerprint (quiet receipts, empty baselines). */
	change_fingerprint?: string;
	headline: string;
	summary: string;
	findings: MonitorFindingV1[];
	notification: {
		policy: MonitorNotificationPolicy;
		emitted: boolean;
		channel: string;
		/** (scope, monitor_task_id, monitor_revision, change_fingerprint, channel) — §7.4 */
		dedupe_key: string;
	};
}

// ── Phase 4 client shapes (backed by the Phase 1-3 endpoints) ───────────────

/** `GET /monitors/{task_id}` — monitor detail projection. */
export interface MonitorDetailV1 {
	task_id: string;
	title: string;
	spec: MonitorSpecV1;
	monitor_revision: number;
	schedule?: TaskScheduleWire | null;
	state: {
		status: string;
		schedule_fire_count: number;
	};
	created_at: string;
	updated_at: string;
	/** Includes the read-time-projected reserved `system:monitor` tag. */
	tags: string[];
}

/** The `Task.schedule` wire object (the existing TaskSchedule shape). */
export interface TaskScheduleWire {
	kind: TaskScheduleKindWire;
	timezone?: string | null;
	max_runs?: number | null;
	paused?: boolean | null;
	[key: string]: unknown;
}

/**
 * Schedule kinds as serialized by the backend enum (serde EXTERNAL tagging —
 * one variant key wrapping the variant fields). `Once`/`OnEvent` ride the
 * index-signature arm; the monitor surfaces only author Cron/Interval.
 */
export type TaskScheduleKindWire =
	| { Cron: { expression: string; timezone?: string | null } }
	| { Interval: { seconds: number; jitter_seconds?: number | null } }
	| Record<string, unknown>;

/** Simple page envelope of the runs/updates read seams (`next_cursor` is null today). */
export interface MonitorItemsPageV1<T> {
	items: T[];
	next_cursor: string | null;
	limit: number;
}

/** `POST /monitors` body. */
export interface CreateMonitorRequestV1 {
	title?: string;
	spec: MonitorSpecV1;
	schedule?: TaskScheduleWire;
}

/** `PATCH /monitors/{task_id}` body — provided fields replace. */
export interface UpdateMonitorRequestV1 {
	title?: string;
	spec?: MonitorSpecV1;
	schedule?: TaskScheduleWire;
}

/** `POST /monitors` / `PATCH /monitors/{task_id}` response. */
export interface MonitorMutationResponseV1 {
	task_id: string;
	monitor_revision: number;
}

/**
 * `POST /monitors/{task_id}/convert` body — the FIXED Phase 7 conversion
 * contract. The task keeps its id, schedule, history, executions, and
 * outputs; conversion only attaches the validated spec (server-owned
 * revision starts at 1) and optionally retitles. There is deliberately NO
 * `schedule` key — the existing schedule is never touched.
 */
export interface ConvertMonitorRequestV1 {
	spec: MonitorSpecV1;
	title?: string;
}

/**
 * Convert response: `200 {task_id, monitor_revision: 1, converted: true}`.
 * Errors: 404 `task_not_found` · 409 `monitor_already_exists` (incl.
 * archived former monitors) · 409 `task_not_eligible_for_monitor`
 * (Internal-lifecycle / archived tasks) · 400 the standard `monitor_*`
 * admission reasons.
 */
export interface ConvertMonitorResponseV1 {
	task_id: string;
	monitor_revision: number;
	converted: boolean;
}

// ── Phase 6 feedback (plan §10) ─────────────────────────────────────────────

export type MonitorFeedbackVerdict = 'useful' | 'not_relevant';

/** `POST /monitors/{task_id}/updates/{update_id}/feedback` body. */
export interface MonitorFeedbackRequestV1 {
	verdict: MonitorFeedbackVerdict;
	/** Optional, ≤500 chars. */
	note?: string;
}

/**
 * Feedback POST response. `recorded: false` is the idempotent replay of the
 * SAME verdict (nothing new stored); posting the OPPOSITE verdict replaces
 * the stored one and records `true`. Errors: 404
 * `monitor_not_found`/`update_not_found`, 400
 * `monitor_feedback_verdict_invalid`.
 */
export interface MonitorFeedbackResponseV1 {
	task_id: string;
	update_id: string;
	verdict: MonitorFeedbackVerdict;
	recorded: boolean;
	/** Stable feedback record id (`mf_…`). */
	feedback_id: string;
}

/** One row of `GET /monitors/{task_id}/feedback?limit=`. */
export interface MonitorFeedbackRecordV1 {
	feedback_id: string;
	update_id: string;
	verdict: MonitorFeedbackVerdict;
	note?: string;
	recorded_at: string;
}
