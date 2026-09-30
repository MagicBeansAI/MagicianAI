pub use crate::magician_v2::execution::agentic::types::CompletionKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::magician_v2::monitors::{monitor_run::MonitorCursorV1, monitor_spec::MonitorSpecV1};
use crate::magician_v2::{
    gaui::MuijDocument,
    orchestrator::v2_orchestrator::{RecommendedQuestion, SlotGraphSnapshot},
    query_analysis::UnifiedQueryAnalysis,
    storage::{models::ClarificationHistoryEntry, StrategyAttempt},
    strategy::plan::PlanGraph,
};

pub fn default_ui_thread_id() -> String {
    "general".to_string()
}

pub fn default_task_created_by() -> String {
    "user".to_string()
}

pub fn default_task_approved() -> bool {
    true
}

pub fn default_task_output_mode() -> TaskOutputMode {
    TaskOutputMode::Accumulate
}

/// serde skip-helper for `TaskManifest.monitor_revision`: `0` means "not a
/// monitor / no spec ever set", so the key is omitted from the wire and old
/// manifests stay byte-identical.
fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// serde skip-helper for `TaskListItemV3.awaiting_diff_approval`: `false` is
/// what every task that is not holding a staged diff reports, which is almost
/// all of them, so the key is omitted and those rows stay byte-identical on the
/// wire.
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutputMode {
    #[default]
    Accumulate,
    Overwrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskTagRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

/// Canonical VibeDev **Build** signal (RCA fix #5): a run that MUST engage the
/// coding pipeline (delegate to an engineer that calls `run_coding_task` and
/// stages a diff), as opposed to a Discuss/plan run (the `plan` tag, where the
/// plan IS the artifact and no code is expected).
///
/// Derived purely from the manifest signals the cockpit already stamps
/// server-side (`ui_thread_id` / tags in `submit.ts`), so there is no new
/// manifest field and no new tag. The result is carried as the in-process
/// `coding_coordinator_run` bool through overrides -> `AgenticContext` ->
/// `CatalogBuildContext`, letting the executor and flat-loop catalog arm the
/// coding guardrails (RCA fixes #1-#4) without re-loading the task.
///
/// The `plan` tag (mirrors `PLAN_TASK_TAG` in
/// `execution/compiled_handlers/run_coding_task.rs`) is EXCLUDED: a Discuss/plan
/// run is intentionally read-only and emits no diff, so it must not be gated on
/// coding engagement, must not arm the coding sandbox, and must not be
/// tool-stripped.
pub fn is_vibedev_coding_build_run(ui_thread_id: &str, tags: &[TaskTagRecord]) -> bool {
    is_vibedev_cockpit_run(ui_thread_id, tags)
        && !tags.iter().any(|tag| tag.name.eq_ignore_ascii_case("plan"))
}

/// True for ANY VibeDev cockpit run — Build OR Discuss/plan. Broader than
/// [`is_vibedev_coding_build_run`] (it does NOT exclude plan runs) because task
/// VISIBILITY (Internal-by-default, promoted to user-visible only on an explicit
/// "save as task") applies to every cockpit run, not just builds. Use this for the
/// visibility/lifecycle gate; use `is_vibedev_coding_build_run` for coding-engagement
/// gates (sandbox arming, tool-stripping, the coding-lead agent override).
pub fn is_vibedev_cockpit_run(ui_thread_id: &str, tags: &[TaskTagRecord]) -> bool {
    ui_thread_id.eq_ignore_ascii_case("vibedev")
        || tags
            .iter()
            .any(|tag| tag.name.eq_ignore_ascii_case("vibedev"))
}

/// Phase 3 — Task lifetime discriminator. Two buckets: user-facing
/// tasks vs internal tasks. Drives the on-disk storage root (`tasks/`
/// vs `internal_tasks/`) and which list surface shows the task.
///
/// `Persistent` is the safe default for tasks the user is meant to
/// see and track: explicit `create_task` calls, plan-mode mints, and
/// background-spawned work the user owns (scheduler, autonomous agent,
/// API, learning-insight follow-ups). Stored under `tasks/`. Survives
/// across chat sessions and chat deletion; visible in `/tasks`.
///
/// `Internal` is set on every task that chat or the runtime spawns as
/// part of its own work rather than as a durable user-facing task:
/// chat capability dispatch, default chat delegations, handovers,
/// sub-goals, and debug-page execution runs (SOTA fixtures, skill
/// probes, agentic experiments). Stored under `internal_tasks/`;
/// visible in `/internal-tasks`, never in `/tasks`.
///
/// Auto-cleanup is keyed on `chat_session_id`, NOT on this variant:
/// when a chat session is cleared/deleted,
/// `cleanup_ephemeral_tasks_for_session` removes the `Internal` tasks
/// carrying that session id, so transient chat side-effects don't
/// linger. `Internal` tasks with no `chat_session_id` (e.g. debug-page
/// runs) are never auto-swept — they live until manually removed.
///
/// The `#[serde(alias …)]` entries keep pre-existing on-disk manifests
/// deserializing cleanly as `Internal`: `ephemeral_owned_by_chat` from
/// the old chat lifecycle and `internal_debug` from the old debug
/// lifecycle. No data migration or file moves are needed — both old
/// variants already wrote to `internal_tasks/`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskLifecycle {
    #[default]
    Persistent,
    #[serde(alias = "ephemeral_owned_by_chat", alias = "internal_debug")]
    Internal,
}

/// Phase 3.1 — Synchronization mode for task dispatchers. Drives
/// whether the spawning chat tool-call blocks on the task's terminal
/// status (`Await`) or returns the `task_id` immediately
/// (`Deferred`).
///
/// `Deferred` is the safe default — matches today's `create_task`
/// semantics: the LLM tool returns immediately with the task_id,
/// the task runs independently, status updates flow back as
/// `TaskStatusUpdate` chat cards. Suitable for long-running work
/// the user doesn't need an immediate answer for.
///
/// `Await` is the chat-pack-replacement mode (Phase 3 target): the
/// LLM tool blocks until the task's terminal status, then returns
/// the task's outputs as the tool result. The LLM's reasoning chain
/// continues with the task result in-context, exactly as it would
/// for an inline tool call — but the work runs inside a real V3
/// task with all the chat-native immersion (activity card,
/// incremental outputs, etc.) the Phase 1/2 wins provide.
///
/// `Await`-mode tasks are typically `lifecycle: Internal` because
/// they're synchronous chat side-effects that shouldn't outlive the
/// spawning conversation; the two fields are independent, though, so
/// an exotic case (e.g. `await` for a chat build the user explicitly
/// wants to keep) can still mark `Persistent`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskSyncMode {
    #[default]
    Deferred,
    Await,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskManifest {
    pub task_id: String,
    pub principal: String,
    pub workspace: String,
    pub title: String,
    pub description: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default = "default_ui_thread_id")]
    pub ui_thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default)]
    pub tags: Vec<TaskTagRecord>,
    #[serde(default = "default_task_created_by")]
    pub created_by: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default = "default_task_approved")]
    pub approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Value>,
    #[serde(default = "default_task_output_mode")]
    pub output_mode: TaskOutputMode,
    /// Phase 3 — Chat session that spawned this task (if any).
    /// `Some(session_id)` for chat-spawned tasks; `None` for tasks
    /// created by background agents, scheduler, or API.
    ///
    /// This is the SOLE discriminator for per-session task cleanup in
    /// `cleanup_ephemeral_tasks_for_session`: when the spawning chat is
    /// cleared or deleted, every `Internal` task carrying this session
    /// id is removed. `Internal` tasks with `None` (e.g. debug-page
    /// runs) and `Persistent` tasks are never auto-swept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,
    /// Phase 3 — Task lifetime classifier. Default is `Persistent`
    /// (existing tasks deserialize cleanly). See `TaskLifecycle`.
    #[serde(default)]
    pub lifecycle: TaskLifecycle,
    /// Phase 3.1 — Sync mode classifier. Default is `Deferred`
    /// (existing tasks deserialize cleanly with today's semantics).
    /// `Await` is used by chat-pack-replacement dispatchers that
    /// block on terminal status. See `TaskSyncMode`.
    #[serde(default)]
    pub sync_mode: TaskSyncMode,
    /// Recurring Monitors Phase 1 — optional typed monitor contract
    /// (plan §6.1). `Some(_)` makes this task a Monitor; `None` for every
    /// ordinary task, and legacy manifests deserialize with `None`. This
    /// field is the SOLE source of truth — monitor identity is never
    /// inferred from titles or tags (the reserved `system:monitor` tag is
    /// a read-time projection only, never persisted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_spec: Option<MonitorSpecV1>,
    /// Recurring Monitors Phase 1 — SERVER-OWNED spec revision counter
    /// (plan §6.1 rule 5), used by run and notification fingerprints.
    /// `0` = not a monitor (key omitted on the wire); `update_task` sets it
    /// to 1 on the first `monitor_spec` write and increments it on every
    /// spec edit. Clients never write this value.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub monitor_revision: u32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub task_id: String,
    pub status: String,
    /// Full or partial delivery once `status` is `completed`, from the root
    /// execution that closed the task. This is the field a list, a badge, or
    /// an eval reads to tell a partial from a full answer without parsing
    /// prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_kind: Option<CompletionKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_items: Vec<String>,
    pub active_root_execution_id: Option<String>,
    pub latest_root_execution_id: Option<String>,
    pub last_completed_root_execution_id: Option<String>,
    pub default_task_agent_output_id: Option<String>,
    pub primary_user_output_id: Option<String>,
    /// Number of times this task's schedule has fired. Incremented by the
    /// scheduler dispatch path before each enqueue and used to enforce
    /// `TaskSchedule.max_runs`. `#[serde(default)]` so existing task JSON
    /// files (pre-Phase-2) deserialize cleanly with count 0.
    #[serde(default)]
    pub schedule_fire_count: u32,
    /// Per-execution synthesis-in-flight registry. An entry is
    /// pushed by `reduce_execution_terminal_status_only` when that
    /// execution's Step 1 lands, and removed by
    /// `reduce_execution_terminal` (success), `reduce_execution_synthesis_failed`
    /// (permanent failure), or `reduce_execution_terminal_without_outputs`
    /// (degraded settle).
    ///
    /// `task.synthesis_pending()` returns `true` when this vec is
    /// non-empty — i.e. "at least one of this task's executions is
    /// still synthesizing." Downstream scheduling (`depends_on`
    /// resolution) gates on emptiness.
    ///
    /// Using a Vec instead of a bool prevents the multi-execution
    /// lifecycle race: if Run 1's synthesis is still in flight when
    /// Run 2 starts, both ids sit in the vec. Run 1 completing only
    /// removes its own id; Run 2's pending state is preserved.
    /// `#[serde(default)]` so historical records deserialize as
    /// empty — the startup reconciler picks up any
    /// `execution.synthesis_pending=true` records by walking
    /// executions directly, so denormalised task state isn't load-
    /// bearing for recovery.
    #[serde(default)]
    pub synthesis_pending_executions: Vec<String>,
    /// `Some(_)` when at least one execution under this task has
    /// `ExecutionState.synthesis_failed`. Last-failure-wins (older
    /// failures get overwritten when a newer execution also fails).
    /// Surfaced on UI as the retry HITL trigger. Cleared once the
    /// failed synthesis is re-run successfully via the
    /// `retry-synthesis` endpoint.
    /// `#[serde(default)]` for historical records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_failed_execution_id: Option<String>,
    /// Recurring Monitors Phase 2 — bounded hot state for the change ledger
    /// (plan §6.3 rule 1): the last accepted execution id (idempotency key),
    /// the previous accepted change fingerprint, whether the last accepted
    /// scan was complete, and the capped recent stable-key entries the §7
    /// comparison needs. `None` on every non-monitor task and on monitors
    /// that have not accepted a run yet; `#[serde(default)]` +
    /// skip-if-none keep pre-Phase-2 task state files byte-identical.
    /// Written only by `accept_monitor_run` under the task write guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_cursor: Option<MonitorCursorV1>,
    /// RFC3339 instant at which the run last ACTUALLY ADVANCED — a step
    /// started or a step finished — and nothing else.
    ///
    /// Deliberately NOT `updated_at`. `updated_at` moves on every write to
    /// this record: a mirrored runtime heartbeat, a status re-assert, a
    /// metric flush, a metadata edit. Stall detection reading `updated_at`
    /// therefore lets a wedged run refresh its own liveness and never report
    /// stalled — an indicator that reports writes while appearing to report
    /// life. Only `reduce_execution_initialized` (the run's first step
    /// starting), `activate_execution` (a re-activated run's step starting),
    /// `reduce_step_event` (a step finishing) and `reduce_action_settled` (a
    /// tool call settling with a result or a failure — the flat loop's unit
    /// of advancement, since a direct run has no taskplan steps) may write
    /// this field; every other write path must leave it untouched.
    ///
    /// `None` means "no step transition has been recorded", not "no
    /// progress" — consumers render nothing rather than guessing, and never
    /// substitute a sentinel. `#[serde(default)]` + skip-if-none keep
    /// pre-existing task state files byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<String>,
    pub updated_at: String,
}

impl TaskState {
    /// Derived: `true` when at least one execution under this task
    /// is still in its synthesis pipeline. See the
    /// `synthesis_pending_executions` doc-comment for lifecycle.
    pub fn synthesis_pending(&self) -> bool {
        // A CANCELLED run abandons its synthesis — the execution is killed, so no
        // Step-2 finalization is coming and any entry left in
        // `synthesis_pending_executions` is stale (e.g. a restart killed a run
        // mid-synthesis and force-cancelled it before the registry drained).
        // Reporting `true` there freezes a "Synthesizing" pill + a Stop control
        // that can only no-op. NOTE: `failed`/`completed` are NOT gated — a Step-1
        // terminal run (incl. `failed`) legitimately stays synthesis-pending while
        // Step-2 async finalization attaches its outputs (see
        // `reduce_execution_terminal_status_only`); only `cancel` truly abandons it.
        if matches!(self.status.as_str(), "cancelled" | "canceled") {
            return false;
        }
        !self.synthesis_pending_executions.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskRefs {
    pub task_id: String,
    pub outputs: Vec<OutputRef>,
    pub default_task_agent_output_id: Option<String>,
    pub primary_user_output_id: Option<String>,
    pub updated_at: Option<String>,
}

/// The only `ExecutionState::relationship_type` value a task root carries.
pub const RELATIONSHIP_TYPE_ROOT: &str = "root";

/// The only `ExecutionState::relationship_type` value a delegated child
/// carries. `execution_record_from_runtime` is the sole writer, and it picks
/// between this and [`RELATIONSHIP_TYPE_ROOT`] on `parent_execution_id`.
///
/// These are constants rather than inline literals because the vocabulary did
/// drift: every lineage guard compared against a spelling no writer ever
/// produced, so each one silently rejected every real delegated child and
/// delegation failed at launch. Compare against these, never a literal.
pub const RELATIONSHIP_TYPE_DELEGATE: &str = "delegate";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionState {
    pub execution_id: String,
    pub task_id: String,
    pub root_execution_id: Option<String>,
    pub parent_execution_id: Option<String>,
    pub agent_id: String,
    /// Always [`RELATIONSHIP_TYPE_ROOT`] or [`RELATIONSHIP_TYPE_DELEGATE`].
    pub relationship_type: String,
    pub status: String,
    /// Full or partial delivery once `status` is `completed`. Propagated
    /// unchanged from the yield: a delegation root copies its child's, and an
    /// aggregate takes the weakest among its required parts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_kind: Option<CompletionKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_items: Vec<String>,
    pub plan_id: Option<String>,
    pub primary_execution_output_id: Option<String>,
    #[serde(default)]
    pub active_child_execution_ids: Vec<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub updated_at: String,
    /// Step IDs completed (from step.completed canonical events)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_step_ids: Vec<String>,
    /// Step IDs failed (from step.failed canonical events)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_step_ids: Vec<String>,
    /// Current active step ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step_id: Option<String>,
    #[serde(default = "default_task_output_mode")]
    pub task_output_mode: TaskOutputMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refinement: Option<String>,

    /// True between the atomic terminal-status disk write (Step 1) and
    /// the moment all three output-synthesis steps (1.1
    /// `v3_execution_output_synthesis`, 1.2 `v3_task_agent_output_synthesis`,
    /// 1.3 `v3_task_user_output_synthesis`) have landed their artifacts.
    /// Read-side consumers (downstream tasks, dashboards, chat-pack
    /// continuations, UI download buttons) gate on this — `true` means
    /// "execution is terminal but the synthesized output isn't ready
    /// yet, wait or show a synthesizing affordance".
    ///
    /// `#[serde(default)]` so historical execution records that
    /// predate this field deserialize as `false` (the synthesis pipeline
    /// either ran inline already or this execution never had outputs).
    #[serde(default)]
    pub synthesis_pending: bool,

    /// `Some(_)` when synthesis exhausted its retries and a HITL was
    /// emitted asking the operator whether to retry. Cleared on
    /// successful synthesis or on HITL-approved re-run. Downstream
    /// consumers that hit this should NOT silently consume a degraded
    /// artifact (raw outcome_summary) — they surface a HITL on
    /// themselves saying "upstream synthesis failed, retry upstream?".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_failed: Option<SynthesisFailure>,
}

/// Captured state of a permanently-failed synthesis attempt. Lives on
/// `ExecutionState.synthesis_failed`. Drives the retry-synthesis HITL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SynthesisFailure {
    /// Which of 1.1 / 1.2 / 1.3 exhausted retries. Identifies the
    /// stage so the HITL approval can re-spawn only the failed step.
    pub stage: SynthesisStage,
    /// Last error message captured from the LLM router or storage layer.
    pub last_error: String,
    /// Number of retries that ran before giving up.
    pub attempts: u32,
    /// RFC3339 timestamp of the final failure.
    pub failed_at: String,
}

/// Which output-synthesis step (1.1 / 1.2 / 1.3) failed. The retry HITL
/// re-runs ONLY this stage rather than the whole pipeline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SynthesisStage {
    /// 1.1 — `v3_execution_output_synthesis`
    ExecutionOutput,
    /// 1.2 — `v3_task_agent_output_synthesis`
    TaskAgentOutput,
    /// 1.3 — `v3_task_user_output_synthesis`
    TaskUserOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionRefs {
    pub execution_id: String,
    pub plan_refs: Vec<PlanRef>,
    pub output_refs: Vec<OutputRef>,
    #[serde(default)]
    pub child_output_refs: Vec<OutputRef>,
    #[serde(default)]
    pub delegations: Vec<DelegationRef>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRef {
    pub plan_id: String,
    pub relative_path: String,
    pub created_at: String,
}

// ─── PlanGraph per-task persistence (legacy "TaskPlan" naming) ─────────
//
// **What this cluster of types actually is:** per-task storage for a
// `PlanGraph` plus its build-time planning-pipeline artifacts
// (query_analysis, slot_graph_snapshot, strategy_attempts,
// clarification_history, pending_questions).
//
// **Why the name is misleading:** the prefix `TaskPlan*` (struct names,
// status enum, field paths, route segments, on-disk `plans/` dir) is a
// fossil from the era when "task plan" was a distinct concept — a
// runtime-mutable markdown document (`taskplan_live.md` /
// `projected_plan_summary_*.md`) that the inner LLM read and edited each
// cycle. That subsystem was retired by the 2026-04-29 inner-loop
// runtime-context-compaction cutover (see
// `docs/archive/plans/2026-04-29-inner-loop-runtime-context-compaction.md`
// — tests assert `taskplan_live.md` must no longer appear). What
// survived is *just the per-task persistence shape for a PlanGraph
// produced by Plan mode (`process_with_strategy`)*. The legacy name
// stuck because rewiring the URLs, disk path, frontend names, and Rust
// identifiers across ~600 sites is non-trivial — a deliberate
// deprioritised cleanup, not a bug.
//
// **Mental model when reading this code:**
//   - `TaskPlanRecord`         = wire/disk envelope for ONE versioned PlanGraph
//   - `TaskPlanIndexRecord`    = pointer to latest + approved PlanGraph for a task
//   - `TaskPlanVersionsRecord` = history of PlanGraphs for a task
//   - `TaskPlanStatus`         = lifecycle of the wrapping envelope, not of any
//                                 separate "task plan" concept
//
// **What is and isn't on the wire:** the inner `plan_graph: Option<PlanGraph>`
// IS the modern PlanGraph and IS returned by `GET /tasks/{id}/plan`. The
// legacy `taskplan_markdown` field that may still appear on disk from
// pre-2026-04-29 files has NO corresponding Rust field — serde silently
// drops it on read, and it is never re-serialised. Wire payloads are
// always clean PlanGraph data.
//
// **When TaskPlanRecord is created**: only via Plan mode entry points —
// `POST /tasks/{id}/plan` (UI Plan button), chat composer in Plan mode
// (`ChatMessageMode::Plan`), or `replan_task`. Do-mode delegations
// (`delegate_to_agent` / `handover_to_agent` / `spawn_sub_goal`) and
// direct executes (`doit_direct`) skip planning entirely → no
// TaskPlanRecord, no `plans/` dir on disk → `GET …/plan` returns 404
// `task_plan_not_found:<id>`, which is expected and the UI guards on
// `task.hasPlan` before fetching.
//
// **When TaskPlanRecord is consumed**: at run time `approved_plan_runtime_context`
// extracts `.plan_graph`, renders it via `render_plan_graph_runtime_context` to
// markdown, and prepends that text to the user-prompt goal. The runtime never
// iterates the PlanGraph as a graph — it's advisory context only.
//
// **Future cleanup (optional)**: rename `TaskPlan*` → `TaskPlanGraph*`
// in Rust + frontend, rename URL `/plan` → `/plan_graph`, rename disk
// path `tasks/{id}/plans/` → `tasks/{id}/plan_graphs/`. Estimated
// ~1 day with a wire-compatibility break. Deferred; the naming is
// misleading but the data flow is correct.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskPlanStatus {
    Planning,
    #[default]
    Draft,
    Eliciting,
    Approved,
    Rejected,
    Failed,
}

impl TaskPlanStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Approved | Self::Rejected | Self::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlanningRecoveryReceipt {
    pub schema_version: u8,
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub plan_id: String,
    pub agent_id: String,
    pub planning_execution_id: String,
    pub deterministic_turn_id: String,
    pub goal: String,
    pub hmac_sha256: String,
    /// Mutable cross-process lease state. The HMAC covers only the immutable
    /// request above; lease writes are serialized by the task write fence and
    /// compared by exact owner/revision before every renewal.
    #[serde(default)]
    pub claim_owner: Option<String>,
    #[serde(default)]
    pub claim_until_ms: Option<i64>,
    #[serde(default)]
    pub claim_revision: u64,
    /// Set immediately before model dispatch. Planning has no outward action
    /// effects, so an expired claim may re-drive the exact deterministic turn;
    /// durable processing metadata is adopted when the first dispatch landed,
    /// with provider duplicate cost remaining the documented crash boundary.
    #[serde(default)]
    pub dispatch_started: bool,
    #[serde(default)]
    pub settled: bool,
    /// Highest Runtime status revision whose complete planning projection was
    /// committed. Mutable recovery metadata (excluded from the immutable HMAC)
    /// prevents an older cross-store snapshot from overwriting a newer resume.
    #[serde(default)]
    pub projection_status_revision: u64,
    #[serde(default)]
    pub projection_source_epoch_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_source_sha256: Option<String>,
    /// Exact AskLoop continuation notifications accepted after elicitation.
    /// Each entry owns its own renewable claim because the initial planning
    /// dispatch may already be settled when a clarification resumes it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resume_recoveries: Vec<TaskPlanningResumeRecoveryReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlanningResumeRecoveryReceipt {
    pub schema_version: u8,
    pub recovery_id: String,
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub plan_id: String,
    pub planning_execution_id: String,
    pub preparation: crate::magician_v2::ask_loop::ResumePreparation,
    pub hmac_sha256: String,
    #[serde(default)]
    pub phase: TaskPlanningResumeRecoveryPhase,
    #[serde(default)]
    pub claim_owner: Option<String>,
    #[serde(default)]
    pub claim_until_ms: Option<i64>,
    #[serde(default)]
    pub claim_revision: u64,
    #[serde(default)]
    pub settled: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPlanningResumeRecoveryPhase {
    Prepared,
    #[default]
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlanRecord {
    pub plan_id: String,
    pub task_id: String,
    pub agent_id: String,
    pub status: TaskPlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning_execution_id: Option<String>,
    /// Integrity-bound restart authority for the exact plan-only Runtime and
    /// deterministic inbound turn. Legacy plan records omit this field and
    /// are projection-only during recovery; they are never replayed from a
    /// mutable task manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning_recovery: Option<TaskPlanningRecoveryReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_graph: Option<PlanGraph>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_analysis: Option<UnifiedQueryAnalysis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_graph_snapshot: Option<SlotGraphSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_attempts: Vec<StrategyAttempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clarification_history: Vec<ClarificationHistoryEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_questions: Vec<RecommendedQuestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlanIndexEntry {
    pub plan_id: String,
    pub status: TaskPlanStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskPlanIndexRecord {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_plan_id: Option<String>,
    #[serde(default)]
    pub plans: Vec<TaskPlanIndexEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

pub const MAX_TASK_PLAN_HISTORY_VERSIONS: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskPlanVersionSourceKind {
    PlanRecord,
    ManualSave,
    RestoreBackup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlanVersionRecord {
    pub epoch_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_graph: Option<PlanGraph>,
    pub source_kind: TaskPlanVersionSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_execution_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskPlanVersionsRecord {
    pub task_id: String,
    #[serde(default)]
    pub versions: Vec<TaskPlanVersionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl TaskPlanVersionsRecord {
    pub fn prune_versions(&mut self) {
        if self.versions.len() > MAX_TASK_PLAN_HISTORY_VERSIONS {
            self.versions.sort_by(|a, b| b.epoch_ms.cmp(&a.epoch_ms));
            self.versions.truncate(MAX_TASK_PLAN_HISTORY_VERSIONS);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputRef {
    pub output_id: String,
    pub scope: String,
    pub audience: String,
    pub role: String,
    pub relative_path: String,
    pub media_type: String,
    pub created_at: String,
    pub source_execution_id: Option<String>,
    pub source_plan_id: Option<String>,
    #[serde(default)]
    pub source_output_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DelegationRef {
    pub parent_step_id: String,
    pub sub_goal: String,
    pub child_execution_id: Option<String>,
    pub child_agent_id: Option<String>,
    pub status: String,
    pub outcome_type: Option<String>,
    pub output_id: Option<String>,
    pub budget_iterations: Option<usize>,
    pub depth: Option<usize>,
    pub iterations_used: Option<usize>,
    pub duration_ms: Option<u64>,
    pub requested_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionIndexEntry {
    pub execution_id: String,
    pub task_id: String,
    pub root_execution_id: Option<String>,
    pub parent_execution_id: Option<String>,
    pub agent_id: String,
    pub relationship_type: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_kind: Option<CompletionKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_items: Vec<String>,
    pub plan_id: Option<String>,
    pub primary_execution_output_id: Option<String>,
    #[serde(default)]
    pub active_child_execution_ids: Vec<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutputsRecord {
    pub task_id: String,
    pub default_task_agent_output_id: Option<String>,
    pub primary_user_output_id: Option<String>,
    pub outputs: Vec<OutputRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOutputsRecord {
    pub execution_id: String,
    pub task_id: String,
    pub primary_execution_output_id: Option<String>,
    pub outputs: Vec<OutputRef>,
    pub child_outputs: Vec<OutputRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersistedExecutionArtifactRecord {
    pub artifact_id: String,
    pub artifact_type: String,
    pub content_type: String,
    pub payload: Value,
    pub produced_at: String,
    #[serde(default)]
    pub source_execution_id: Option<String>,
    #[serde(default)]
    pub source_artifact_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PersistedExecutionArtifactsIndex {
    pub execution_id: String,
    #[serde(default)]
    pub artifacts: Vec<PersistedExecutionArtifactRecord>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedSurfacePlacement {
    pub placement_kind: String,
    #[serde(default)]
    pub placement_id: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedSurfaceRecord {
    pub surface_id: String,
    pub principal: String,
    pub workspace: String,
    pub surface_kind: String,
    pub status: String,
    #[serde(default)]
    pub logical_surface_id: Option<String>,
    pub route: String,
    pub document_key: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
    #[serde(default)]
    pub source_output_id: Option<String>,
    #[serde(default)]
    pub source_execution_id: Option<String>,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub materialized_render_kind: Option<String>,
    #[serde(default)]
    pub materialized_document_key: Option<String>,
    #[serde(default)]
    pub materialized_at: Option<String>,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    pub placement: PublishedSurfacePlacement,
    #[serde(default)]
    pub manifest_artifact_uid: Option<String>,
    #[serde(default)]
    pub manifest_name: Option<String>,
    #[serde(default)]
    pub input_artifact_ids: Vec<String>,
    pub published_at: String,
    #[serde(default)]
    pub unpublished_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedSurfaceIndexEntry {
    pub surface_id: String,
    pub surface_kind: String,
    pub status: String,
    #[serde(default)]
    pub logical_surface_id: Option<String>,
    pub route: String,
    pub document_key: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
    #[serde(default)]
    pub source_output_id: Option<String>,
    #[serde(default)]
    pub materialized_render_kind: Option<String>,
    #[serde(default)]
    pub materialized_document_key: Option<String>,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    pub placement: PublishedSurfacePlacement,
    pub published_at: String,
    #[serde(default)]
    pub unpublished_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedSurfaceIndexRecord {
    pub principal: String,
    pub workspace: String,
    #[serde(default)]
    pub surfaces: Vec<PublishedSurfaceIndexEntry>,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct PublishSurfaceInput {
    pub task_id: String,
    pub source_output_id: Option<String>,
    pub materialize_as: Option<String>,
    pub logical_surface_id: Option<String>,
    pub surface_kind: Option<String>,
    pub route: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub placement: Option<PublishedSurfacePlacement>,
}

/// Shared dashboard publish-input builder used by both the harness handler
/// (`execute_create_dashboard`) and the chat dispatcher
/// (`dispatch_create_dashboard`). Encodes the placement-kind → placement-id
/// rule, the canonical `logical_surface_id` shape, and the standard
/// `muij_surface`/`dashboard` materialize+kind defaults so the two paths can't
/// drift (BUG-10).
pub fn build_dashboard_publish_input(
    task_id: &str,
    workspace: &str,
    ui_thread_id: &str,
    placement_kind: &str,
    pinned: bool,
    route: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    source_output_id: Option<String>,
) -> Result<PublishSurfaceInput, String> {
    let placement_id = match placement_kind {
        "thread" => Some(ui_thread_id.to_string()),
        "workspace" => Some(workspace.to_string()),
        "task" => Some(task_id.to_string()),
        "global" => None,
        other => {
            return Err(format!(
                "unsupported placement_kind `{other}`. Use one of workspace|thread|task|global."
            ));
        },
    };

    let logical_surface_id = format!(
        "task:{}:{}:dashboard",
        task_id,
        route.as_deref().unwrap_or("/briefing")
    );

    Ok(PublishSurfaceInput {
        task_id: task_id.to_string(),
        source_output_id,
        materialize_as: Some("muij_surface".to_string()),
        logical_surface_id: Some(logical_surface_id),
        surface_kind: Some("dashboard".to_string()),
        route,
        title,
        summary,
        placement: Some(PublishedSurfacePlacement {
            placement_kind: placement_kind.to_string(),
            placement_id,
            pinned,
        }),
    })
}

#[derive(Debug, Clone, Default)]
pub struct RepublishSurfaceInput {
    pub source_output_id: Option<String>,
    pub materialize_as: Option<String>,
    pub logical_surface_id: Option<String>,
    pub surface_kind: Option<String>,
    pub route: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub placement: Option<PublishedSurfacePlacement>,
}

#[derive(Debug, Clone, Default)]
pub struct PublishedSurfaceProjectionFilter {
    pub route: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub ui_thread_id: Option<String>,
    pub placement_kind: Option<String>,
    pub status: Option<String>,
    pub pinned_only: bool,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedSurfaceProjectionRecord {
    pub surface: PublishedSurfaceRecord,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub task_status: Option<String>,
    #[serde(default)]
    pub source_agent_id: Option<String>,
    #[serde(default)]
    pub source_output_media_type: Option<String>,
    #[serde(default)]
    pub source_output_summary: Option<String>,
    pub render_origin: String,
    pub render_kind: String,
    pub presentation_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedSurfaceRenderRecord {
    pub surface: PublishedSurfaceRecord,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub task_status: Option<String>,
    #[serde(default)]
    pub source_agent_id: Option<String>,
    #[serde(default)]
    pub source_output_id: Option<String>,
    #[serde(default)]
    pub source_execution_id: Option<String>,
    #[serde(default)]
    pub source_output_relative_path: Option<String>,
    #[serde(default)]
    pub source_output_summary: Option<String>,
    #[serde(default)]
    pub media_type: Option<String>,
    pub render_origin: String,
    pub render_kind: String,
    pub presentation_state: String,
    #[serde(default)]
    pub durable_document_key: Option<String>,
    #[serde(default)]
    pub durable_manifest_name: Option<String>,
    #[serde(default)]
    pub muij_document: Option<MuijDocument>,
    #[serde(default)]
    pub text_content: Option<String>,
    #[serde(default)]
    pub json_content: Option<Value>,
    /// True when inline rendering contains only a bounded prefix. The complete
    /// authoritative output remains available through its download route.
    #[serde(default)]
    pub content_truncated: bool,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedSurfaceTopFeedRecord {
    pub principal: String,
    pub workspace: String,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub global: Vec<PublishedSurfaceProjectionRecord>,
    #[serde(default)]
    pub workspace_pinned: Vec<PublishedSurfaceProjectionRecord>,
    #[serde(default)]
    pub thread: Vec<PublishedSurfaceProjectionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionScheduleStep {
    pub step_id: String,
    pub title: String,
    pub order: usize,
    #[serde(default)]
    pub depends_on_step_ids: Vec<String>,
    pub capability: Option<String>,
    pub delegate_agent_id: Option<String>,
    pub taskplan_status: String,
    pub taskplan_progress: String,
    #[serde(default)]
    pub sub_step_labels: Vec<String>,
    /// Present only for the sealed app-recipe runner. This is canonical V3
    /// scheduler state, not a UI projection: every dispatch transition binds
    /// the exact lowered node, typed input and attempt before owner I/O.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<AppRecipeScheduleStepState>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeScheduleNodePhase {
    Pending,
    DispatchReserved,
    Started,
    Completed,
    Failed,
    Cancelled,
    Skipped,
    OutcomeUncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeScheduleStepState {
    pub node_execution_id: String,
    pub parent_node_execution_id: String,
    pub binding_digest: String,
    pub input_schema_ref: String,
    pub output_schema_ref: String,
    pub uncertainty: String,
    /// Immutable ceilings copied from the reviewed node/recipe contract.
    pub max_active_millis: u64,
    pub cancellation_acknowledgement_timeout_millis: u64,
    pub aggregate_deadline_at_ms: i64,
    pub phase: AppRecipeScheduleNodePhase,
    pub attempt: u16,
    #[serde(default)]
    pub claim_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_owner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_lease_expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_deadline_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_encoded_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_identity_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_requested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_ack_deadline_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionScheduleRecord {
    pub execution_id: String,
    pub task_id: String,
    pub plan_id: Option<String>,
    pub source_plan_relative_path: Option<String>,
    pub source_kind: String,
    pub updated_at: String,
    pub steps: Vec<ExecutionScheduleStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationReadinessEntry {
    pub parent_step_id: String,
    pub sub_goal: String,
    pub child_execution_id: Option<String>,
    pub child_agent_id: Option<String>,
    pub status: String,
    pub readiness: String,
    pub outcome_type: Option<String>,
    pub output_id: Option<String>,
    pub output: Option<OutputRef>,
    pub budget_iterations: Option<usize>,
    pub depth: Option<usize>,
    pub iterations_used: Option<usize>,
    pub duration_ms: Option<u64>,
    pub requested_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationReadinessRecord {
    pub execution_id: String,
    pub task_id: String,
    pub execution_status: String,
    pub waiting_for_children: bool,
    pub active_child_execution_ids: Vec<String>,
    pub ready_child_output_ids: Vec<String>,
    pub result_ready_count: usize,
    pub waiting_count: usize,
    pub blocked_count: usize,
    pub terminal_without_output_count: usize,
    pub delegations: Vec<DelegationReadinessEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleReadinessStep {
    pub step_id: String,
    pub title: String,
    pub order: usize,
    #[serde(default)]
    pub depends_on_step_ids: Vec<String>,
    #[serde(default)]
    pub blocked_by_step_ids: Vec<String>,
    pub capability: Option<String>,
    pub delegate_agent_id: Option<String>,
    pub taskplan_status: String,
    pub taskplan_progress: String,
    #[serde(default)]
    pub sub_step_labels: Vec<String>,
    pub readiness: String,
    pub child_execution_id: Option<String>,
    pub child_output_id: Option<String>,
    pub child_execution_status: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionScheduleReadinessRecord {
    pub execution_id: String,
    pub task_id: String,
    pub plan_id: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub ready_step_ids: Vec<String>,
    #[serde(default)]
    pub blocked_step_ids: Vec<String>,
    #[serde(default)]
    pub waiting_step_ids: Vec<String>,
    #[serde(default)]
    pub satisfied_step_ids: Vec<String>,
    #[serde(default)]
    pub running_step_ids: Vec<String>,
    #[serde(default)]
    pub failed_step_ids: Vec<String>,
    pub waiting_for_children: bool,
    pub steps: Vec<ScheduleReadinessStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTreeNode {
    pub execution_id: String,
    pub parent_execution_id: Option<String>,
    pub root_execution_id: Option<String>,
    pub agent_id: String,
    pub relationship_type: String,
    pub status: String,
    pub plan_id: Option<String>,
    pub primary_execution_output_id: Option<String>,
    pub active_child_execution_ids: Vec<String>,
    pub child_execution_ids: Vec<String>,
    pub ready_child_output_ids: Vec<String>,
    pub waiting_for_children: bool,
    pub delegation_summary: DelegationReadinessRecord,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTreeRecord {
    pub task_id: String,
    pub task_status: String,
    pub active_root_execution_id: Option<String>,
    pub latest_root_execution_id: Option<String>,
    pub last_completed_root_execution_id: Option<String>,
    pub root_execution_id: Option<String>,
    pub nodes: Vec<ExecutionTreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOutcomeSnapshot {
    pub execution_status: String,
    pub task_status: String,
    pub outcome_type: String,
    pub outcome_summary: String,
    pub iterations_used: Option<usize>,
    pub is_terminal: bool,
    /// Full or partial delivery of a `completed` terminal. `None` for
    /// non-terminal and failed snapshots, and for records written before the
    /// kind existed. Orthogonal to `outcome_type`, which stays a lifecycle
    /// label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_kind: Option<CompletionKind>,
    /// What a partial declared undone, verbatim from the yield.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_items: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizedOutputs {
    pub execution_output: OutputRef,
    pub task_agent_output: OutputRef,
    pub task_user_output: OutputRef,
    #[serde(default)]
    pub continuation_context_output: Option<OutputRef>,
    /// User-audience OutputRefs for media files (image/video/audio) the
    /// execution produced and the capture step promoted into the task
    /// `outputs/` dir. Surfacing these makes a headline media deliverable
    /// render inline in chat + the panel instead of being lost behind the
    /// text write-up. Empty for tasks with no produced media.
    #[serde(default)]
    pub media_outputs: Vec<OutputRef>,
    pub task_output_mode: TaskOutputMode,
}

pub const CONTINUATION_CONTEXT_OUTPUT_ROLE: &str = "continuation_context";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalEvent {
    pub event_id: String,
    pub seq: u64,
    pub timestamp: String,
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub execution_id: String,
    pub plan_id: Option<String>,
    pub step_id: Option<String>,
    pub event_type: String,
    #[serde(default)]
    pub ref_ids: EventRefIds,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventRefIds {
    #[serde(default)]
    pub output_ids: Vec<String>,
    #[serde(default)]
    pub plan_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub manifest: TaskManifest,
    pub state: TaskState,
    pub refs: TaskRefs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskListItemV3 {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub agent_id: String,
    #[serde(default = "default_ui_thread_id")]
    pub ui_thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default)]
    pub tags: Vec<TaskTagRecord>,
    #[serde(default = "default_task_created_by")]
    pub created_by: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default = "default_task_approved")]
    pub approved: bool,
    #[serde(default)]
    pub is_blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Value>,
    #[serde(default = "default_task_output_mode")]
    pub output_mode: TaskOutputMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_root_execution_id: Option<String>,
    pub latest_root_execution_id: Option<String>,
    pub last_completed_root_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_substep_title: Option<String>,
    pub completion_summary: Option<String>,
    pub completion_outcome: Option<String>,
    #[serde(default)]
    pub completion_artifact_names: Vec<String>,
    #[serde(default)]
    pub has_plan: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_status: Option<TaskPlanStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_question: Option<RecommendedQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_questions: Vec<RecommendedQuestion>,
    /// Phase 3 — Spawning chat session id (mirrored from
    /// `TaskManifest.chat_session_id`). `Some(_)` for chat-spawned
    /// tasks; `None` for scheduler/API/autonomous origins. Lets the
    /// `/tasks` UI hide chat_inline-origin tasks by default so the
    /// page isn't drowned by transient chat side-effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,
    /// Phase 3 — Task lifecycle classifier (mirrored from
    /// `TaskManifest.lifecycle`).
    #[serde(default)]
    pub lifecycle: TaskLifecycle,
    /// Phase 3.1 — Task sync mode (mirrored from
    /// `TaskManifest.sync_mode`).
    #[serde(default)]
    pub sync_mode: TaskSyncMode,
    /// Mirrors `TaskState.synthesis_pending`. `true` between the
    /// atomic Step 1 disk write and the moment the synthesis pipeline
    /// (1.1/1.2/1.3) lands its artifacts. UI list rows render a
    /// "synthesizing…" affordance when this is `true`.
    #[serde(default)]
    pub synthesis_pending: bool,
    /// Mirrors `TaskState.synthesis_failed_execution_id`. `Some(_)`
    /// when at least one execution under this task had its synthesis
    /// pipeline exhaust retries; UI renders the "retry synthesis"
    /// affordance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_failed_execution_id: Option<String>,
    /// Recurring Monitors Phase 7 — mirrors the server-owned
    /// `TaskManifest.monitor_revision` so task-list clients can tell
    /// "already a monitor" (`> 0`) from "eligible for explicit
    /// conversion" (`0`) WITHOUT a per-row manifest read. Same wire
    /// discipline as the manifest field: omitted while 0, so plain-task
    /// rows stay byte-identical.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub monitor_revision: u32,
    /// VibeDev handoff Phase 2 — this run is holding a staged
    /// `CodeChangeProposal` that is still `Pending`, so it is waiting on the
    /// user to approve or reject the diff.
    ///
    /// **Derived from the proposal store on every read, never from an event.**
    /// A HITL event can be dropped or arrive before a restart; the
    /// `code_change_proposals/` directory cannot, so a process that has just
    /// started reports exactly what the one that staged the diff reported. The
    /// same rule the attention feed already applies
    /// (`list_attention_items`): the store, not the event, decides.
    ///
    /// `false` for a terminal task even when a `Pending` proposal is still on
    /// disk — that proposal is orphaned, not actionable, which is the same
    /// call `agentic_attention_is_still_actionable` makes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub awaiting_diff_approval: bool,
    /// Mirrors `TaskState.last_progress_at` — when the run last actually
    /// advanced (a step started or finished), RFC3339.
    ///
    /// This is the field task surfaces use to tell a wedged run from a
    /// working one, so it must never be filled from `updated_at`: that moves
    /// on any write and would make every run look permanently alive. The
    /// wire carries a real timestamp or omits the key entirely — never a
    /// sentinel (`0`, `-1`, a magic "never" value), because clients compute a
    /// duration from it and a sentinel renders as garbage rather than
    /// degrading to "unknown".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub state: ExecutionState,
    pub refs: ExecutionRefs,
}
