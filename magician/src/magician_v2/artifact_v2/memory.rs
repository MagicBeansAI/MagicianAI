use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(any(test, feature = "test-fixtures"))]
use tokio::fs;

use crate::magician_v2::agents::{
    memory::EpisodeOutcome,
    memory_tiers::{ActionSummary, MemoryDelta},
    AgentMemoryService,
};

use super::{
    models::{ExecutionOutcomeSnapshot, ExecutionRecord, OutputRef, TaskRecord},
    service::{ArtifactV2Error, ScopeRef},
    workspace::ArtifactV2Workspace,
};

const MAX_EPISODE_OUTPUT_EXCERPT_READ_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3EpisodeProvenance {
    pub task_manifest_relative_path: String,
    pub task_state_relative_path: String,
    pub task_refs_relative_path: String,
    pub execution_events_relative_path: String,
    pub execution_state_relative_path: String,
    pub execution_refs_relative_path: String,
    pub execution_output_relative_path: String,
    /// Legacy: the retired agent-audience task output. Always `None` for
    /// new episodes; kept so earlier episode files still load.
    pub task_agent_output_relative_path: Option<String>,
    pub task_user_output_relative_path: Option<String>,
}

// `V3MemoryTierRecord` lives in `magician-vector-index` so the
// memory-candidate / memory-index code there can use it without a reverse
// dependency on magician. Re-exported here to preserve existing import
// paths (`use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;`).
pub use magician_vector_index::V3MemoryTierRecord;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3EpisodeRecord {
    #[serde(default = "default_v3_memory_episode_schema_version")]
    pub schema_version: String,
    #[serde(default = "default_v3_memory_episode_record_type")]
    pub record_type: String,
    #[serde(default)]
    pub principal: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    pub agent_id: String,
    pub episode_id: String,
    pub goal_key: String,
    pub consolidation_key: String,
    pub trigger_type: String,
    pub trigger_seq: u64,
    pub trigger_timestamp: String,
    #[serde(default)]
    pub trigger_payload: Option<Value>,
    pub started_at: String,
    pub completed_at: String,
    pub outcome_kind: String,
    pub outcome_summary: String,
    #[serde(default)]
    pub outcome_remaining: Option<String>,
    #[serde(default)]
    pub pending_actions: Vec<String>,
    #[serde(default)]
    pub failure_count: Option<usize>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub execution_id: Option<String>,
    pub root_execution_id: Option<String>,
    pub parent_execution_id: Option<String>,
    #[serde(default)]
    pub relationship_type: Option<String>,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub task_description: Option<String>,
    /// What the run was asked to do: bounded, and free of the operating policy
    /// a server-assembled description carries around the user's words. Recall
    /// shows this instead of `task_description`, so an episode replays the
    /// intent and outcome of a run, never its run-specific instructions.
    /// `None` on records written before the field existed (recall derives it)
    /// and on producers with no task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_intent: Option<String>,
    #[serde(default)]
    pub execution_status: Option<String>,
    #[serde(default)]
    pub outcome_type: Option<String>,
    #[serde(default)]
    pub execution_output_id: Option<String>,
    /// Legacy: the retired agent-audience task output. Always `None` for
    /// new episodes; kept so earlier episode files still load.
    pub task_agent_output_id: Option<String>,
    pub task_user_output_id: Option<String>,
    #[serde(default)]
    pub source_output_ids: Vec<String>,
    #[serde(default)]
    pub actions_taken: Vec<ActionSummary>,
    #[serde(default)]
    pub observations: Vec<String>,
    #[serde(default)]
    pub memory_updates: Vec<MemoryDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_candidates: Vec<MemoryCandidate>,
    #[serde(default)]
    pub strategy_summary: Option<String>,
    #[serde(default)]
    pub context_at_start: Option<String>,
    #[serde(default)]
    pub artifact_output: Option<Value>,
    #[serde(default)]
    pub provenance: Option<V3EpisodeProvenance>,
    /// The invocation surface this episode was produced on.
    ///
    /// A **fact** about where the content entered the system, not a policy
    /// decision: trust is derived from it at read time, so a later policy change
    /// applies to already-written records instead of freezing today's answer
    /// into them. It is server-minted and never model-supplied, which is what
    /// makes it usable as an authority — unlike a declared `source_type`, which
    /// a model summarising untrusted text can set to anything.
    ///
    /// `None` on records written before this field existed and on producers with
    /// no surface (background jobs). Readers must treat `None` as "unknown",
    /// never as "owner".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_surface: Option<String>,
    /// The MEETING this episode was produced in, when it was produced in one.
    ///
    /// The second dimension of the same fact `origin_surface` records, and it
    /// exists because the first one is not enough: `origin_surface` says an
    /// episode came from a room, never WHICH room, so the only containment
    /// expressible from it was "no room reads any of it". With the occasion
    /// named, a rejoin of the same meeting can read what it produced itself
    /// while a later meeting reads none of it.
    ///
    /// Identical discipline to `origin_surface`: a **fact**, server-minted from
    /// the call's binding, never model-supplied, and never a policy decision —
    /// containment is derived from it at read time so a later policy change
    /// applies to records already on disk.
    ///
    /// `None` means "not produced in a meeting" for an owner surface and
    /// "unknown" for a room; readers on the room path must treat both as
    /// unreadable rather than as their own, which is what
    /// `magician_vector_index::ContextLabel::Unlabelled` already means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_meeting: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub candidate_type: String,
    pub target_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub value: Value,
    pub confidence: f64,
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone)]
pub struct V3EpisodeRecordingContext {
    pub scope: ScopeRef,
    pub task: TaskRecord,
    pub execution: ExecutionRecord,
    pub outcome: ExecutionOutcomeSnapshot,
    pub execution_output: OutputRef,
    pub task_user_output: Option<OutputRef>,
}

#[derive(Debug, Clone)]
pub struct RecordedEpisode {
    pub episode_id: String,
    pub relative_path: String,
    pub created: bool,
}

#[async_trait]
pub trait V3EpisodeRecorder: Send + Sync {
    async fn record_terminal_execution_episode(
        &self,
        ctx: &V3EpisodeRecordingContext,
    ) -> Result<RecordedEpisode, ArtifactV2Error>;

    async fn terminal_execution_episode_exists(
        &self,
        scope: &ScopeRef,
        agent_id: &str,
        execution_id: &str,
    ) -> Result<bool, ArtifactV2Error>;
}

#[derive(Debug, Clone)]
pub struct FilesystemV3EpisodeRecorder {
    workspace: ArtifactV2Workspace,
}

impl FilesystemV3EpisodeRecorder {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }
}

impl V3EpisodeRecord {
    /// The containment label this one episode asserts.
    ///
    /// A room-produced episode is readable from that same room and nowhere
    /// else; an owner-surface episode names no occasion and so resolves to
    /// [`ContextLabel::Unlabelled`], which a bound retrieval refuses. Empty and
    /// whitespace-only ids are treated as absent rather than as a meeting
    /// nobody can name, so a blank cannot become a label a room might match.
    ///
    /// This is the single-episode rule that the consolidator's multi-episode
    /// agreement check is built from, so both paths spell "which occasion" the
    /// same way.
    pub fn origin_meeting_label(&self) -> crate::magician_v2::agents::ContextLabel {
        match self
            .origin_meeting
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            Some(meeting) => crate::magician_v2::agents::ContextLabel::Meeting(meeting.to_string()),
            None => crate::magician_v2::agents::ContextLabel::Unlabelled,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_memory_episode(
        scope: Option<(&str, &str)>,
        agent_id: impl Into<String>,
        episode_id: impl Into<String>,
        goal_key: impl Into<String>,
        trigger_type: impl Into<String>,
        trigger_seq: u64,
        trigger_timestamp: DateTime<Utc>,
        trigger_payload: Option<Value>,
        started_at: DateTime<Utc>,
        completed_at: DateTime<Utc>,
        outcome: &EpisodeOutcome,
        actions_taken: Vec<ActionSummary>,
        observations: Vec<String>,
        memory_updates: Vec<MemoryDelta>,
        strategy_summary: Option<String>,
        context_at_start: Option<String>,
        artifact_output: Option<Value>,
    ) -> Self {
        let goal_key = goal_key.into();
        let (
            outcome_kind,
            outcome_summary,
            outcome_remaining,
            pending_actions,
            failure_count,
            last_error,
        ) = v3_outcome_fields_from_legacy(outcome);

        Self {
            schema_version: V3_MEMORY_EPISODE_SCHEMA_ORIGIN_STAMPED.to_string(),
            record_type: "memory_episode".to_string(),
            principal: scope.map(|(principal, _)| principal.to_string()),
            workspace: scope.map(|(_, workspace)| workspace.to_string()),
            agent_id: agent_id.into(),
            episode_id: episode_id.into(),
            goal_key: goal_key.clone(),
            consolidation_key: goal_key,
            trigger_type: trigger_type.into(),
            trigger_seq,
            trigger_timestamp: trigger_timestamp.to_rfc3339(),
            trigger_payload,
            started_at: started_at.to_rfc3339(),
            completed_at: completed_at.to_rfc3339(),
            outcome_kind,
            outcome_summary,
            outcome_remaining,
            pending_actions,
            failure_count,
            last_error,
            task_id: None,
            execution_id: None,
            root_execution_id: None,
            parent_execution_id: None,
            relationship_type: None,
            ui_thread_id: None,
            task_title: None,
            task_description: None,
            task_intent: None,
            execution_status: Some(execution_status_from_outcome(outcome).to_string()),
            outcome_type: None,
            execution_output_id: None,
            task_agent_output_id: None,
            task_user_output_id: None,
            source_output_ids: Vec::new(),
            actions_taken,
            observations,
            memory_updates,
            memory_candidates: Vec::new(),
            strategy_summary,
            context_at_start,
            artifact_output,
            provenance: None,
            origin_surface: None,
            origin_meeting: None,
        }
    }

    pub fn goal_id(&self) -> &str {
        &self.goal_key
    }

    pub fn started_at_dt(&self) -> Result<DateTime<Utc>, ArtifactV2Error> {
        parse_rfc3339_utc(&self.started_at)
    }

    pub fn completed_at_dt(&self) -> Result<DateTime<Utc>, ArtifactV2Error> {
        parse_rfc3339_utc(&self.completed_at)
    }

    pub fn outcome_is_succeeded(&self) -> bool {
        matches!(
            self.outcome_kind.as_str(),
            "goal_achieved" | "partial_progress"
        )
    }

    pub fn outcome_is_failed(&self) -> bool {
        matches!(
            self.outcome_kind.as_str(),
            "failed" | "budget_exhausted" | "circuit_open"
        )
    }

    /// `true` iff the episode ended as PARTIAL success (some declared
    /// work completed, some gaps remain — typically routed through
    /// the partial-success safety net). Distinct from `is_failed`
    /// (no useful work) and from `is_succeeded` (full success). The
    /// "almost succeeded" class is exactly what the agent should
    /// learn from — these episodes feed failure_adaptation feedback
    /// loops alongside hard failures (tactical pattern T3).
    pub fn outcome_is_partial(&self) -> bool {
        self.outcome_kind == "partial_progress"
    }

    /// Composite predicate: episode is either a hard failure OR a
    /// partial success. Used by feedback loops that want to learn
    /// from "anything that didn't fully succeed" — the high-signal
    /// learning surface.
    pub fn outcome_is_failed_or_partial(&self) -> bool {
        self.outcome_is_failed() || self.outcome_is_partial()
    }

    pub fn outcome_is_paused(&self) -> bool {
        self.outcome_kind == "paused"
    }

    pub fn outcome_is_completed(&self) -> bool {
        self.outcome_is_succeeded() || self.outcome_is_failed()
    }

    pub fn outcome_summary_text(&self) -> &str {
        &self.outcome_summary
    }

    /// The outcome as recall shows it to a model: the summary when it is
    /// user-facing prose that does not replay the task brief, else assembled
    /// from structured fields (verdict, intent, the run's user-facing output),
    /// bounded — so a prompt never replays another run's operating
    /// instructions or machine text. Applied at read time to every record,
    /// whichever writer stored it; see
    /// [`magician_vector_index::episode_candidates::recalled_episode_outcome`].
    pub fn recalled_outcome_text(&self) -> String {
        magician_vector_index::episode_candidates::recalled_episode_outcome(
            &self.outcome_summary,
            &self.outcome_kind,
            self.task_title.as_deref(),
            self.task_description.as_deref(),
            self.task_intent.as_deref(),
            primary_output_excerpt(&self.memory_candidates),
        )
    }

    /// The strategy as recall shows it: `None` when the record has none, it
    /// repeats the outcome, or it is not recallable prose; else bounded like
    /// [`Self::recalled_outcome_text`].
    pub fn recalled_strategy_text(&self) -> Option<String> {
        magician_vector_index::episode_candidates::recalled_episode_strategy(
            self.strategy_summary.as_deref(),
            &self.outcome_summary,
            self.task_description.as_deref(),
            self.task_intent.as_deref(),
        )
    }

    /// What the run was asked to do, as recall shows it: the recorded
    /// `task_intent`, else one derived from the description — never the raw
    /// description, which can carry the run's operating policy.
    pub fn recalled_intent_text(&self) -> Option<String> {
        magician_vector_index::episode_candidates::recalled_episode_intent(
            self.task_intent.as_deref(),
            self.task_description.as_deref(),
        )
        .or_else(|| {
            magician_vector_index::episode_candidates::episode_task_intent(
                self.task_title.as_deref(),
                None,
            )
        })
    }

    /// Any text the record carries (a candidate summary, an evidence line) as
    /// recall shows it: the record's own outcome statement gets the full
    /// record-aware treatment of [`Self::recalled_outcome_text`]; any other
    /// text keeps only the user-facing prose it leads with
    /// ([`magician_vector_index::episode_candidates::user_facing_prose`]).
    pub fn recalled_text<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        if !text.trim().is_empty() && text.trim() == self.outcome_summary.trim() {
            return std::borrow::Cow::Owned(self.recalled_outcome_text());
        }
        magician_vector_index::episode_candidates::user_facing_prose(text)
    }

    pub fn outcome_error_summary(&self) -> String {
        match self.outcome_kind.as_str() {
            "failed" | "circuit_open" => self
                .last_error
                .clone()
                .unwrap_or_else(|| self.outcome_summary.clone()),
            "budget_exhausted" => "Budget exhausted".to_string(),
            _ => String::new(),
        }
    }

    pub fn outcome_pending_actions(&self) -> &[String] {
        &self.pending_actions
    }

    pub fn from_terminal_context(ctx: &V3EpisodeRecordingContext) -> Self {
        let completed_at = ctx
            .execution
            .state
            .completed_at
            .clone()
            .unwrap_or_else(|| ctx.execution.state.updated_at.clone());
        let trigger_seq = parse_rfc3339_utc(&completed_at)
            .map(|dt| dt.timestamp_millis().try_into().unwrap_or_default())
            .unwrap_or_default();
        let source_output_ids = ctx
            .task_user_output
            .as_ref()
            .map(|output| output.source_output_ids.clone())
            .unwrap_or_else(|| ctx.execution_output.source_output_ids.clone());
        let semantic_goal_key = ctx
            .task
            .manifest
            .goal_id
            .clone()
            .unwrap_or_else(|| ctx.task.manifest.task_id.clone());
        let task_intent = terminal_episode_task_intent(ctx);
        let outcome_statement =
            terminal_episode_outcome_statement(ctx, task_intent.as_deref(), None);

        Self {
            schema_version: V3_MEMORY_EPISODE_SCHEMA_ORIGIN_STAMPED.to_string(),
            record_type: "execution_terminal".to_string(),
            principal: Some(ctx.scope.principal().to_string()),
            workspace: Some(ctx.scope.workspace().to_string()),
            agent_id: ctx.execution.state.agent_id.clone(),
            episode_id: ctx.execution.state.execution_id.clone(),
            goal_key: semantic_goal_key.clone(),
            consolidation_key: semantic_goal_key,
            trigger_type: "v3_execution_terminal".to_string(),
            trigger_seq,
            trigger_timestamp: ctx.execution.state.started_at.clone(),
            trigger_payload: Some(json!({
                "task_id": ctx.task.manifest.task_id,
                "goal_id": ctx.task.manifest.goal_id,
                "execution_id": ctx.execution.state.execution_id,
                "root_execution_id": ctx.execution.state.root_execution_id,
                "parent_execution_id": ctx.execution.state.parent_execution_id,
                "relationship_type": ctx.execution.state.relationship_type,
                "scope": {
                    "principal": ctx.scope.principal(),
                    "workspace": ctx.scope.workspace(),
                },
            })),
            started_at: ctx.execution.state.started_at.clone(),
            completed_at: completed_at.clone(),
            outcome_kind: outcome_kind_from_legacy(&legacy_episode_outcome(&ctx.outcome)),
            outcome_summary: outcome_statement.clone(),
            outcome_remaining: None,
            pending_actions: pending_actions_from_outcome(&legacy_episode_outcome(&ctx.outcome)),
            failure_count: failure_count_from_outcome(&legacy_episode_outcome(&ctx.outcome)),
            last_error: last_error_from_outcome(&legacy_episode_outcome(&ctx.outcome)),
            task_id: Some(ctx.task.manifest.task_id.clone()),
            execution_id: Some(ctx.execution.state.execution_id.clone()),
            root_execution_id: ctx.execution.state.root_execution_id.clone(),
            parent_execution_id: ctx.execution.state.parent_execution_id.clone(),
            relationship_type: Some(ctx.execution.state.relationship_type.clone()),
            ui_thread_id: Some(ctx.task.manifest.ui_thread_id.clone()),
            task_title: Some(ctx.task.manifest.title.clone()),
            task_description: Some(ctx.task.manifest.description.clone()),
            task_intent,
            execution_status: Some(ctx.outcome.execution_status.clone()),
            outcome_type: Some(ctx.outcome.outcome_type.clone()),
            execution_output_id: Some(ctx.execution_output.output_id.clone()),
            task_agent_output_id: None,
            task_user_output_id: ctx.task_user_output.as_ref().map(|o| o.output_id.clone()),
            source_output_ids,
            actions_taken: Vec::new(),
            observations: build_episode_observations(ctx),
            memory_updates: Vec::new(),
            memory_candidates: Vec::new(),
            strategy_summary: Some(outcome_statement),
            context_at_start: Some(format!(
                "principal={}; workspace={}; ui_thread_id={}",
                ctx.scope.principal(),
                ctx.scope.workspace(),
                ctx.task.manifest.ui_thread_id
            )),
            artifact_output: Some(json!({
                "task_id": ctx.task.manifest.task_id,
                "execution_id": ctx.execution.state.execution_id,
                "root_execution_id": ctx.execution.state.root_execution_id,
                "parent_execution_id": ctx.execution.state.parent_execution_id,
                "relationship_type": ctx.execution.state.relationship_type,
                "execution_output": {
                    "output_id": ctx.execution_output.output_id,
                    "relative_path": ctx.execution_output.relative_path,
                    "media_type": ctx.execution_output.media_type,
                },
                "task_user_output": ctx.task_user_output.as_ref().map(|output| {
                    json!({
                        "output_id": output.output_id,
                        "relative_path": output.relative_path,
                        "media_type": output.media_type,
                    })
                }),
            })),
            // A task execution IS an owner surface: the work was authorized by
            // the owner, not asserted by a participant in a room. Stated
            // explicitly rather than left unset, because unset now means "a
            // producer forgot to stamp" and fails closed.
            origin_surface: Some(
                crate::magician_v2::agents::InvocationSurface::Task
                    .as_str()
                    .to_string(),
            ),
            // A task execution happens in no meeting. `None` here is the
            // positive fact "not an occasion", not an omission: an owner
            // surface never reads under a meeting confinement, so nothing
            // downstream needs an occasion from it.
            origin_meeting: None,
            provenance: Some(V3EpisodeProvenance {
                task_manifest_relative_path: String::new(),
                task_state_relative_path: String::new(),
                task_refs_relative_path: String::new(),
                execution_events_relative_path: String::new(),
                execution_state_relative_path: String::new(),
                execution_refs_relative_path: String::new(),
                execution_output_relative_path: ctx.execution_output.relative_path.clone(),
                task_agent_output_relative_path: None,
                task_user_output_relative_path: ctx
                    .task_user_output
                    .as_ref()
                    .map(|o| o.relative_path.clone()),
            }),
        }
    }
}

#[async_trait]
impl V3EpisodeRecorder for FilesystemV3EpisodeRecorder {
    async fn record_terminal_execution_episode(
        &self,
        ctx: &V3EpisodeRecordingContext,
    ) -> Result<RecordedEpisode, ArtifactV2Error> {
        let agent_id = ctx.execution.state.agent_id.clone();
        self.workspace
            .ensure_memory_agent_layout(&ctx.scope.principal(), &ctx.scope.workspace(), &agent_id)
            .await?;

        let episode_id = ctx.execution.state.execution_id.clone();
        let mut record = V3EpisodeRecord::from_terminal_context(ctx);
        record.provenance = Some(V3EpisodeProvenance {
            task_manifest_relative_path: relative_to_scope_root(
                &self
                    .workspace
                    .scope_root(&ctx.scope.principal(), &ctx.scope.workspace()),
                &self.workspace.task_manifest_path(
                    &ctx.scope.principal(),
                    &ctx.scope.workspace(),
                    &ctx.task.manifest.task_id,
                ),
            ),
            task_state_relative_path: relative_to_scope_root(
                &self
                    .workspace
                    .scope_root(&ctx.scope.principal(), &ctx.scope.workspace()),
                &self.workspace.task_state_path(
                    &ctx.scope.principal(),
                    &ctx.scope.workspace(),
                    &ctx.task.manifest.task_id,
                ),
            ),
            task_refs_relative_path: relative_to_scope_root(
                &self
                    .workspace
                    .scope_root(&ctx.scope.principal(), &ctx.scope.workspace()),
                &self.workspace.task_refs_path(
                    &ctx.scope.principal(),
                    &ctx.scope.workspace(),
                    &ctx.task.manifest.task_id,
                ),
            ),
            execution_events_relative_path: relative_to_scope_root(
                &self
                    .workspace
                    .scope_root(&ctx.scope.principal(), &ctx.scope.workspace()),
                &self.workspace.execution_events_path(
                    &ctx.scope.principal(),
                    &ctx.scope.workspace(),
                    &ctx.task.manifest.task_id,
                    &ctx.execution.state.execution_id,
                ),
            ),
            execution_state_relative_path: relative_to_scope_root(
                &self
                    .workspace
                    .scope_root(&ctx.scope.principal(), &ctx.scope.workspace()),
                &self.workspace.execution_state_path(
                    &ctx.scope.principal(),
                    &ctx.scope.workspace(),
                    &ctx.task.manifest.task_id,
                    &ctx.execution.state.execution_id,
                ),
            ),
            execution_refs_relative_path: relative_to_scope_root(
                &self
                    .workspace
                    .scope_root(&ctx.scope.principal(), &ctx.scope.workspace()),
                &self.workspace.execution_refs_path(
                    &ctx.scope.principal(),
                    &ctx.scope.workspace(),
                    &ctx.task.manifest.task_id,
                    &ctx.execution.state.execution_id,
                ),
            ),
            execution_output_relative_path: ctx.execution_output.relative_path.clone(),
            task_agent_output_relative_path: None,
            task_user_output_relative_path: ctx
                .task_user_output
                .as_ref()
                .map(|o| o.relative_path.clone()),
        });
        record.memory_candidates =
            build_terminal_memory_candidates(&self.workspace, ctx, &record.outcome_summary).await;
        // An outcome that is not user-facing prose is assembled from structured
        // fields; the run's own final output is its result.
        if let Some(result) = primary_output_excerpt(&record.memory_candidates) {
            let statement = terminal_episode_outcome_statement(
                ctx,
                record.task_intent.as_deref(),
                Some(result),
            );
            record.strategy_summary = Some(statement.clone());
            record.outcome_summary = statement;
        }

        let memory_service = AgentMemoryService::with_scoped_memory_scope(
            self.workspace
                .memory_root(&ctx.scope.principal(), &ctx.scope.workspace()),
            ctx.scope.principal(),
            ctx.scope.workspace(),
        );
        let episodes_dir = memory_service
            .storage()
            .agent_episodes_dir(&agent_id)
            .map_err(|err| {
                ArtifactV2Error::Runtime(format!("v3_memory_episode_path_failed: {err}"))
            })?;
        let episode_path =
            episodes_dir.join(memory_service.native_episode_file_name_for_record(&record));
        let created = !tokio::fs::try_exists(&episode_path).await?;
        memory_service
            .append_native_episode(&agent_id, &record)
            .await
            .map_err(|err| {
                ArtifactV2Error::Runtime(format!("v3_memory_episode_write_failed: {err}"))
            })?;

        Ok(RecordedEpisode {
            episode_id,
            relative_path: relative_to_scope_root(
                &self
                    .workspace
                    .scope_root(&ctx.scope.principal(), &ctx.scope.workspace()),
                &episode_path,
            ),
            created,
        })
    }

    async fn terminal_execution_episode_exists(
        &self,
        scope: &ScopeRef,
        agent_id: &str,
        execution_id: &str,
    ) -> Result<bool, ArtifactV2Error> {
        let memory_service = AgentMemoryService::with_scoped_memory_scope(
            self.workspace
                .memory_root(&scope.principal(), &scope.workspace()),
            scope.principal(),
            scope.workspace(),
        );
        let episodes_dir = memory_service
            .storage()
            .agent_episodes_dir(agent_id)
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!("v3_memory_episode_path_failed: {error}"))
            })?;
        let episode_path =
            episodes_dir.join(memory_service.native_episode_file_name_for_id(execution_id));
        Ok(tokio::fs::try_exists(episode_path).await?)
    }
}

fn relative_to_scope_root(scope_root: &Path, path: &Path) -> String {
    path.strip_prefix(scope_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Schema version for records written BEFORE `origin_surface` existed.
///
/// Kept as the serde default deliberately: a record on disk that omits
/// `schema_version` predates the field, and must be read as legacy rather than
/// as a current record that failed to stamp its origin.
fn default_v3_memory_episode_schema_version() -> String {
    "v3_memory_episode/v1".to_string()
}

/// Schema version every episode written from now on carries.
///
/// It is a promise: a record at this version has had its `origin_surface`
/// stamped by its producer. That promise is what lets trust derivation fail
/// CLOSED — an episode claiming this version with no origin is a producer that
/// forgot, and is treated as untrusted rather than inheriting full trust.
pub const V3_MEMORY_EPISODE_SCHEMA_ORIGIN_STAMPED: &str = "v3_memory_episode/v2";

fn default_v3_memory_episode_record_type() -> String {
    "execution_terminal".to_string()
}

fn outcome_kind_from_legacy(outcome: &EpisodeOutcome) -> String {
    match outcome {
        EpisodeOutcome::GoalAchieved { .. } => "goal_achieved".to_string(),
        EpisodeOutcome::PartialProgress { .. } => "partial_progress".to_string(),
        EpisodeOutcome::Failed { .. } => "failed".to_string(),
        EpisodeOutcome::UserIntervened { .. } => "user_intervened".to_string(),
        EpisodeOutcome::BudgetExhausted => "budget_exhausted".to_string(),
        EpisodeOutcome::Paused { .. } => "paused".to_string(),
        EpisodeOutcome::CircuitOpen { .. } => "circuit_open".to_string(),
    }
}

fn execution_status_from_outcome(outcome: &EpisodeOutcome) -> &'static str {
    match outcome {
        EpisodeOutcome::GoalAchieved { .. } => "completed",
        EpisodeOutcome::PartialProgress { .. } => "completed",
        EpisodeOutcome::Failed { .. } => "failed",
        EpisodeOutcome::UserIntervened { .. } => "cancelled",
        EpisodeOutcome::BudgetExhausted => "budget_exhausted",
        EpisodeOutcome::Paused { .. } => "paused",
        EpisodeOutcome::CircuitOpen { .. } => "failed",
    }
}

fn v3_outcome_fields_from_legacy(
    outcome: &EpisodeOutcome,
) -> (
    String,
    String,
    Option<String>,
    Vec<String>,
    Option<usize>,
    Option<String>,
) {
    match outcome {
        EpisodeOutcome::GoalAchieved { summary } => (
            "goal_achieved".to_string(),
            summary.clone(),
            None,
            Vec::new(),
            None,
            None,
        ),
        EpisodeOutcome::PartialProgress { summary, remaining } => (
            "partial_progress".to_string(),
            summary.clone(),
            Some(remaining.clone()),
            Vec::new(),
            None,
            None,
        ),
        EpisodeOutcome::Failed { error } => (
            "failed".to_string(),
            error.clone(),
            None,
            Vec::new(),
            None,
            Some(error.clone()),
        ),
        EpisodeOutcome::UserIntervened { reason } => (
            "user_intervened".to_string(),
            reason.clone(),
            None,
            Vec::new(),
            None,
            None,
        ),
        EpisodeOutcome::BudgetExhausted => (
            "budget_exhausted".to_string(),
            "Budget exhausted".to_string(),
            None,
            Vec::new(),
            None,
            None,
        ),
        EpisodeOutcome::Paused { pending_actions } => (
            "paused".to_string(),
            pending_actions
                .first()
                .cloned()
                .unwrap_or_else(|| "Paused".to_string()),
            None,
            pending_actions.clone(),
            None,
            None,
        ),
        EpisodeOutcome::CircuitOpen {
            failure_count,
            last_error,
        } => (
            "circuit_open".to_string(),
            "Circuit open".to_string(),
            None,
            Vec::new(),
            Some(*failure_count),
            Some(last_error.clone()),
        ),
    }
}

fn pending_actions_from_outcome(outcome: &EpisodeOutcome) -> Vec<String> {
    match outcome {
        EpisodeOutcome::Paused { pending_actions } => pending_actions.clone(),
        _ => Vec::new(),
    }
}

fn failure_count_from_outcome(outcome: &EpisodeOutcome) -> Option<usize> {
    match outcome {
        EpisodeOutcome::CircuitOpen { failure_count, .. } => Some(*failure_count),
        _ => None,
    }
}

fn last_error_from_outcome(outcome: &EpisodeOutcome) -> Option<String> {
    match outcome {
        EpisodeOutcome::Failed { error } => Some(error.clone()),
        EpisodeOutcome::CircuitOpen { last_error, .. } => Some(last_error.clone()),
        _ => None,
    }
}

fn legacy_episode_outcome(outcome: &ExecutionOutcomeSnapshot) -> EpisodeOutcome {
    match outcome.execution_status.as_str() {
        "completed" => {
            // Distinguish full success from partial-success (tactical pattern T3).
            // The agentic loop's partial-success safety net (executor.rs:
            // PARTIAL-SUCCESS-SAFETY-NET) maps cannot_proceed + completed
            // work → `outcome_type: "goal_achieved_partial"`. Without this
            // discriminator the memory pipeline classifies it as plain
            // GoalAchieved, which means failure_adaptation feedback loops
            // (filter `outcome.is_failed`) never see the "almost succeeded"
            // episodes — exactly the cases we most want the agent to learn
            // from. Mapping to PartialProgress surfaces it as a learnable
            // class without flipping it to "failed" (because real work DID
            // get done).
            if outcome.completion_kind
                == Some(crate::magician_v2::execution::agentic::types::CompletionKind::Partial)
                || outcome.outcome_type == "goal_achieved_partial"
            {
                EpisodeOutcome::PartialProgress {
                    summary: outcome.outcome_summary.clone(),
                    remaining: "remaining gap captured in partial_findings.md artifact".to_string(),
                }
            } else {
                EpisodeOutcome::GoalAchieved {
                    summary: outcome.outcome_summary.clone(),
                }
            }
        },
        "cancelled" => EpisodeOutcome::UserIntervened {
            reason: outcome.outcome_summary.clone(),
        },
        "waiting_for_user" | "waiting_for_confirmation" | "paused" => EpisodeOutcome::Paused {
            pending_actions: vec![outcome.outcome_summary.clone()],
        },
        "budget_exhausted" => EpisodeOutcome::BudgetExhausted,
        _ => EpisodeOutcome::Failed {
            error: outcome.outcome_summary.clone(),
        },
    }
}

/// The bounded, policy-free intent of the task a terminal episode records.
fn terminal_episode_task_intent(ctx: &V3EpisodeRecordingContext) -> Option<String> {
    magician_vector_index::episode_candidates::episode_task_intent(
        Some(ctx.task.manifest.title.as_str()),
        Some(ctx.task.manifest.description.as_str()),
    )
}

/// The terminal outcome as an episode keeps it, decided at record time by the
/// same structural rule recall applies: the runtime's summary when it is
/// user-facing prose that does not replay the task brief, else verdict +
/// `task_intent` + the run's own output — so the record never stores a goal
/// echo, a handoff payload or the run's operating policy.
fn terminal_episode_outcome_statement(
    ctx: &V3EpisodeRecordingContext,
    task_intent: Option<&str>,
    result: Option<&str>,
) -> String {
    magician_vector_index::episode_candidates::episode_outcome_statement(
        &ctx.outcome.outcome_summary,
        &outcome_kind_from_legacy(&legacy_episode_outcome(&ctx.outcome)),
        Some(ctx.task.manifest.description.as_str()),
        task_intent,
        result,
    )
}

/// The run's final output text, preferring the user-audience output.
fn primary_output_excerpt(candidates: &[MemoryCandidate]) -> Option<&str> {
    ["task_user_output", "execution_output"]
        .into_iter()
        .find_map(|source| {
            candidates
                .iter()
                .find(|candidate| {
                    candidate.candidate_type == "final_output_excerpt" && candidate.source == source
                })
                .and_then(|candidate| candidate.value.get("content_excerpt"))
                .and_then(Value::as_str)
        })
}

fn build_episode_observations(ctx: &V3EpisodeRecordingContext) -> Vec<String> {
    let mut observations = vec![
        format!("task_id: {}", ctx.task.manifest.task_id),
        format!("execution_id: {}", ctx.execution.state.execution_id),
        format!("agent_id: {}", ctx.execution.state.agent_id),
        format!(
            "relationship_type: {}",
            ctx.execution.state.relationship_type
        ),
        format!("task_title: {}", ctx.task.manifest.title),
        format!("outcome_type: {}", ctx.outcome.outcome_type),
    ];
    // The description is not repeated here: it can carry server-injected
    // operating policy, and the record's bounded `task_intent` stands for it.
    if let Some(root_execution_id) = &ctx.execution.state.root_execution_id {
        observations.push(format!("root_execution_id: {}", root_execution_id));
    }
    if let Some(parent_execution_id) = &ctx.execution.state.parent_execution_id {
        observations.push(format!("parent_execution_id: {}", parent_execution_id));
    }
    observations.push(format!(
        "execution_output: {}",
        ctx.execution_output.relative_path
    ));
    if let Some(task_user_output) = &ctx.task_user_output {
        observations.push(format!(
            "task_user_output: {}",
            task_user_output.relative_path
        ));
    }
    observations
}

async fn build_terminal_memory_candidates(
    workspace: &ArtifactV2Workspace,
    ctx: &V3EpisodeRecordingContext,
    outcome_statement: &str,
) -> Vec<MemoryCandidate> {
    let mut candidates = Vec::new();
    let terminal_value = json!({
        "outcome_type": ctx.outcome.outcome_type.clone(),
        "execution_status": ctx.outcome.execution_status.clone(),
        "task_status": ctx.outcome.task_status.clone(),
        "summary": outcome_statement,
        "iterations_used": ctx.outcome.iterations_used,
        "task_title": ctx.task.manifest.title.clone(),
        "task_intent": terminal_episode_task_intent(ctx),
    });
    candidates.push(MemoryCandidate {
        candidate_type: "terminal_outcome".to_string(),
        target_hint: "agent.memory_quality_source".to_string(),
        key: Some(ctx.execution.state.execution_id.clone()),
        value: terminal_value,
        confidence: if ctx.outcome.is_terminal { 0.85 } else { 0.45 },
        source: "terminal_execution".to_string(),
        evidence: vec![outcome_statement.to_string()],
        rationale:
            "Terminal outcome is source material for durable memory extraction; it is not durable by itself."
                .to_string(),
    });

    for (label, output) in [
        ("execution_output", Some(&ctx.execution_output)),
        ("task_user_output", ctx.task_user_output.as_ref()),
    ] {
        let Some(output) = output else {
            continue;
        };
        let mut value = json!({
            "output_id": output.output_id.clone(),
            "relative_path": output.relative_path.clone(),
            "media_type": output.media_type.clone(),
            "role": output.role.clone(),
            "audience": output.audience.clone(),
        });
        let content = read_output_text_excerpt(workspace, ctx, output).await;
        if let Some(content) = content {
            if let Some(map) = value.as_object_mut() {
                map.insert(
                    "content_excerpt".to_string(),
                    Value::String(content.clone()),
                );
            }
            candidates.push(MemoryCandidate {
                candidate_type: "final_output_excerpt".to_string(),
                target_hint: "agent_or_user_memory_candidate_source".to_string(),
                key: Some(format!("{}:{}", label, output.output_id)),
                value,
                confidence: if label == "task_user_output" {
                    0.9
                } else {
                    0.75
                },
                source: label.to_string(),
                evidence: vec![format!("{label}:{}", output.output_id)],
                rationale:
                    "Final output text is high-signal source material for extracting durable facts."
                        .to_string(),
            });
        } else {
            candidates.push(MemoryCandidate {
                candidate_type: "final_output_reference".to_string(),
                target_hint: "agent.memory_quality_source".to_string(),
                key: Some(format!("{}:{}", label, output.output_id)),
                value,
                confidence: 0.35,
                source: label.to_string(),
                evidence: Vec::new(),
                rationale:
                    "Non-text or unavailable output is kept as provenance, not durable memory."
                        .to_string(),
            });
        }
    }

    if ctx.outcome.execution_status != "completed" {
        candidates.push(MemoryCandidate {
            candidate_type: "failure_or_pause_signal".to_string(),
            target_hint: "agent.environment_knowledge_or_strategy".to_string(),
            key: Some(ctx.execution.state.execution_id.clone()),
            value: json!({
                "execution_status": ctx.outcome.execution_status.clone(),
                "outcome_type": ctx.outcome.outcome_type.clone(),
                "summary": outcome_statement,
            }),
            confidence: 0.7,
            source: "terminal_execution".to_string(),
            evidence: vec![outcome_statement.to_string()],
            rationale: "Failures and pauses may contain reusable retry-avoidance knowledge."
                .to_string(),
        });
    }

    candidates
}

async fn read_output_text_excerpt(
    workspace: &ArtifactV2Workspace,
    ctx: &V3EpisodeRecordingContext,
    output: &OutputRef,
) -> Option<String> {
    let media_type = base_media_type(&output.media_type);
    if !matches!(
        media_type,
        "text/markdown" | "text/plain" | "application/json" | "application/xml" | "text/xml"
    ) {
        return None;
    }
    let path = workspace
        .task_dir(
            &ctx.scope.principal(),
            &ctx.scope.workspace(),
            &ctx.task.manifest.task_id,
        )
        .join(&output.relative_path);
    let raw = read_bounded_episode_output_text(workspace, &path).await?;
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    Some(truncate_chars(text, 4_000))
}

async fn read_bounded_episode_output_text(
    workspace: &ArtifactV2Workspace,
    path: &Path,
) -> Option<String> {
    let mut bytes = workspace
        .read_prefix_path(path, MAX_EPISODE_OUTPUT_EXCERPT_READ_BYTES + 1)
        .await
        .ok()?;
    let truncated = bytes.len() as u64 > MAX_EPISODE_OUTPUT_EXCERPT_READ_BYTES;
    if truncated {
        bytes.truncate(MAX_EPISODE_OUTPUT_EXCERPT_READ_BYTES as usize);
    }
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid_up_to = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid_up_to);
            String::from_utf8(bytes).ok()
        },
        Err(_) => None,
    }
}

fn base_media_type(media_type: &str) -> &str {
    media_type.split(';').next().unwrap_or(media_type).trim()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text.chars().take(max_chars).collect::<String>();
    out.push_str("...");
    out
}

fn parse_rfc3339_utc(raw: &str) -> Result<DateTime<Utc>, ArtifactV2Error> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| {
            ArtifactV2Error::Runtime(format!("invalid_rfc3339_timestamp `{raw}`: {err}"))
        })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn episode_output_excerpt_read_is_bounded_without_size_metadata() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("large-output.txt");
        let mut body = vec![b'x'; MAX_EPISODE_OUTPUT_EXCERPT_READ_BYTES as usize];
        body.extend_from_slice("€ignored".as_bytes());
        fs::write(&path, body).await.expect("large output fixture");

        let text = read_bounded_episode_output_text(&workspace, &path)
            .await
            .expect("bounded UTF-8 prefix");
        assert_eq!(text.len() as u64, MAX_EPISODE_OUTPUT_EXCERPT_READ_BYTES);
        assert!(!text.contains("ignored"));
    }

    #[tokio::test]
    async fn episode_output_excerpt_drops_only_a_split_terminal_utf8_scalar() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("utf8-boundary.txt");
        let mut body = vec![b'x'; MAX_EPISODE_OUTPUT_EXCERPT_READ_BYTES as usize - 1];
        body.extend_from_slice("€tail".as_bytes());
        fs::write(&path, body)
            .await
            .expect("UTF-8 boundary fixture");

        let text = read_bounded_episode_output_text(&workspace, &path)
            .await
            .expect("valid prefix before split scalar");
        assert_eq!(text.len() as u64, MAX_EPISODE_OUTPUT_EXCERPT_READ_BYTES - 1);
        assert!(!text.ends_with('�'));
    }
}

#[cfg(test)]
mod origin_meeting_label_tests {
    use super::*;
    use crate::magician_v2::agents::ContextLabel;

    fn episode(origin_meeting: Option<&str>) -> V3EpisodeRecord {
        let now = Utc::now();
        let mut record = V3EpisodeRecord::new_memory_episode(
            None,
            "envoy".to_string(),
            "ep-1".to_string(),
            "goal-1".to_string(),
            "manual".to_string(),
            1,
            now,
            None,
            now,
            now,
            &EpisodeOutcome::PartialProgress {
                summary: "did some of it".to_string(),
                remaining: "the rest".to_string(),
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
        );
        record.origin_meeting = origin_meeting.map(str::to_string);
        record
    }

    #[test]
    fn a_room_episode_is_labelled_to_that_meeting() {
        assert_eq!(
            episode(Some("meeting-weekly-sync")).origin_meeting_label(),
            ContextLabel::Meeting("meeting-weekly-sync".to_string())
        );
    }

    #[test]
    fn an_owner_episode_names_no_occasion() {
        assert_eq!(
            episode(None).origin_meeting_label(),
            ContextLabel::Unlabelled
        );
    }

    #[test]
    fn a_blank_meeting_id_is_absent_not_a_label() {
        // A blank must not become a meeting a room could match on.
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                episode(Some(blank)).origin_meeting_label(),
                ContextLabel::Unlabelled,
                "blank id {blank:?} must not label the episode"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_does_not_change_the_occasion() {
        assert_eq!(
            episode(Some("  meeting-a  ")).origin_meeting_label(),
            ContextLabel::Meeting("meeting-a".to_string())
        );
    }
}

#[cfg(test)]
mod terminal_episode_text_tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::{
        ExecutionRefs, ExecutionState, TaskLifecycle, TaskManifest, TaskOutputMode, TaskRefs,
        TaskState, TaskSyncMode,
    };

    /// A server-assembled description: the user's words in a fence, then
    /// operating policy that belongs to this run alone.
    const DESCRIPTION: &str = "VibeDev coding request:\n\
        Original VibeDev user prompt:\n\
        <<<VIBEDEV_USER_PROMPT\n\
        Append a line \"concurrency test\" to README.md. Nothing else.\n\
        VIBEDEV_USER_PROMPT\n\
        \n\
        Execution policy:\n\
        - Stage code changes as CodeChangeProposal diff_approval items.\n\
        - VibeDev manual review is enabled; wait for approval before applying proposals.\n";
    const INTENT: &str = "Append a line \"concurrency test\" to README.md. Nothing else.";

    fn output(output_id: &str) -> OutputRef {
        OutputRef {
            output_id: output_id.to_string(),
            scope: "task".to_string(),
            audience: "user".to_string(),
            role: "primary_task_user".to_string(),
            relative_path: format!("outputs/{output_id}.md"),
            media_type: "text/markdown".to_string(),
            created_at: "2026-10-19T21:01:36Z".to_string(),
            source_execution_id: Some("exec-1".to_string()),
            source_plan_id: None,
            source_output_ids: Vec::new(),
        }
    }

    fn context(outcome_summary: &str) -> V3EpisodeRecordingContext {
        let at = "2026-10-19T21:01:36Z".to_string();
        V3EpisodeRecordingContext {
            scope: ScopeRef::system_internal_unauthenticated("anonymous", "default"),
            task: TaskRecord {
                manifest: TaskManifest {
                    task_id: "task-1".to_string(),
                    principal: "anonymous".to_string(),
                    workspace: "default".to_string(),
                    title:
                        "VibeDev · Append a line \"concurrency test\" to README.md. Nothing els…"
                            .to_string(),
                    description: DESCRIPTION.to_string(),
                    agent_id: "cto".to_string(),
                    goal_id: None,
                    ui_thread_id: "vibedev".to_string(),
                    priority: None,
                    due_date: None,
                    tags: Vec::new(),
                    created_by: "user".to_string(),
                    depends_on: Vec::new(),
                    approved: true,
                    schedule: None,
                    output_mode: TaskOutputMode::Accumulate,
                    chat_session_id: None,
                    lifecycle: TaskLifecycle::default(),
                    sync_mode: TaskSyncMode::default(),
                    monitor_spec: None,
                    monitor_revision: 0,
                    relentless: None,
                    created_at: at.clone(),
                    updated_at: at.clone(),
                },
                state: TaskState {
                    task_id: "task-1".to_string(),
                    status: "completed".to_string(),
                    completion_kind: None,
                    open_items: Vec::new(),
                    active_root_execution_id: None,
                    latest_root_execution_id: None,
                    last_completed_root_execution_id: None,
                    default_task_agent_output_id: None,
                    primary_user_output_id: None,
                    schedule_fire_count: 0,
                    synthesis_pending_executions: Vec::new(),
                    synthesis_failed_execution_id: None,
                    monitor_cursor: None,
                    last_progress_at: None,
                    relentless_policy_runs: Default::default(),
                    updated_at: at.clone(),
                },
                refs: TaskRefs {
                    task_id: "task-1".to_string(),
                    outputs: Vec::new(),
                    default_task_agent_output_id: None,
                    primary_user_output_id: None,
                    primary_deliverable_output_id: None,
                    latest_delivered_output_ids: Vec::new(),
                    updated_at: Some(at.clone()),
                },
            },
            execution: ExecutionRecord {
                state: ExecutionState {
                    execution_id: "exec-1".to_string(),
                    task_id: "task-1".to_string(),
                    root_execution_id: Some("exec-1".to_string()),
                    parent_execution_id: None,
                    agent_id: "cto".to_string(),
                    relationship_type: "root".to_string(),
                    status: "completed".to_string(),
                    completion_kind: None,
                    open_items: Vec::new(),
                    plan_id: None,
                    primary_execution_output_id: None,
                    active_child_execution_ids: Vec::new(),
                    started_at: at.clone(),
                    completed_at: Some(at.clone()),
                    updated_at: at,
                    completed_step_ids: Vec::new(),
                    failed_step_ids: Vec::new(),
                    current_step_id: None,
                    task_output_mode: TaskOutputMode::Accumulate,
                    refinement: None,
                    synthesis_pending: false,
                    synthesis_failed: None,
                    pack_dispatch_mode: None,
                },
                refs: ExecutionRefs {
                    execution_id: "exec-1".to_string(),
                    ..Default::default()
                },
            },
            outcome: ExecutionOutcomeSnapshot {
                execution_status: "completed".to_string(),
                task_status: "completed".to_string(),
                outcome_type: "goal_achieved".to_string(),
                outcome_summary: outcome_summary.to_string(),
                iterations_used: Some(1),
                is_terminal: true,
                completion_kind: None,
                open_items: Vec::new(),
            },
            execution_output: output("out_exec_exec-1"),
            task_user_output: Some(output("out_task_user_task-1")),
        }
    }

    fn assert_no_operating_policy(text: &str) {
        for policy in [
            "Stage code changes as CodeChangeProposal",
            "VibeDev manual review is enabled",
            "Execution policy",
            "Trusted project (git branch mode)",
        ] {
            assert!(!text.contains(policy), "{policy:?} recorded: {text}");
        }
    }

    /// The live 2026-10-19 shape: the runtime's `Goal achieved: <goal>`
    /// statement echoed a goal that was the description plus a block the run
    /// appended. The episode records the intent and the outcome, never that
    /// run's operating policy — in its outcome, strategy or observations.
    #[test]
    fn a_terminal_episode_records_intent_and_outcome_without_operating_policy() {
        let ctx = context(&format!(
            "Goal achieved: {DESCRIPTION}\nTrusted project (git branch mode):\n\
             - There is NO CodeChangeProposal for this project.\n\n\
             [Execution resumed]. All tracked task-state micro-goals are resolved."
        ));
        let record = V3EpisodeRecord::from_terminal_context(&ctx);

        assert_eq!(record.task_intent.as_deref(), Some(INTENT));
        assert_eq!(record.outcome_summary, format!("Goal achieved: {INTENT}"));
        assert_eq!(
            record.strategy_summary.as_deref(),
            Some(record.outcome_summary.as_str())
        );
        assert_eq!(record.outcome_kind, "goal_achieved");
        for observation in &record.observations {
            assert_no_operating_policy(observation);
        }
        // The description stays on the record as the task's provenance;
        // recall never shows it.
        assert_eq!(record.task_description.as_deref(), Some(DESCRIPTION));
        assert_no_operating_policy(&record.recalled_outcome_text());

        // The run's own final output stands in for the answer the echoed
        // goal swallowed.
        let candidates = vec![MemoryCandidate {
            candidate_type: "final_output_excerpt".to_string(),
            target_hint: "agent_or_user_memory_candidate_source".to_string(),
            key: None,
            value: json!({"content_excerpt": "Appended `concurrency test` to README.md."}),
            confidence: 0.9,
            source: "task_user_output".to_string(),
            evidence: Vec::new(),
            rationale: String::new(),
        }];
        let result = primary_output_excerpt(&candidates);
        assert_eq!(result, Some("Appended `concurrency test` to README.md."));
        assert_eq!(
            terminal_episode_outcome_statement(&ctx, record.task_intent.as_deref(), result),
            format!("Goal achieved: {INTENT}\nAppended `concurrency test` to README.md.")
        );
    }

    /// A summary that does not echo the description — a deliverable answer, an
    /// error — is recorded as the runtime wrote it.
    #[test]
    fn a_summary_that_does_not_echo_the_goal_is_recorded_unchanged() {
        let record = V3EpisodeRecord::from_terminal_context(&context("Line count: 4."));
        assert_eq!(record.outcome_summary, "Line count: 4.");
        assert_eq!(record.task_intent.as_deref(), Some(INTENT));
    }

    /// A record written before `task_intent` existed is cleaned when it is
    /// recalled, without rewriting the file.
    #[test]
    fn a_legacy_record_recalls_a_bounded_outcome() {
        let mut record = V3EpisodeRecord::from_terminal_context(&context("ignored"));
        record.task_intent = None;
        record.outcome_summary = format!("Goal achieved: {DESCRIPTION}");
        let recalled = record.recalled_outcome_text();
        assert_eq!(recalled, format!("Goal achieved: {INTENT}"));
        assert_no_operating_policy(&recalled);
    }

    /// A partial-success statement (`Goal partially achieved: <goal>\n\n##
    /// PARTIAL SUCCESS …`) and an internal continuation handoff are not
    /// user-facing prose. The writer stores verdict + intent + the run's own
    /// output, and recall applies the same structural rule to a record stored
    /// before the writer knew it — even one that already carries `task_intent`.
    #[test]
    fn non_prose_outcomes_record_and_recall_intent_and_final_output() {
        let partial = format!(
            "Goal partially achieved: {DESCRIPTION}\nTrusted project (git branch mode):\n\
             - There is NO CodeChangeProposal for this project.\n\n\
             [Execution resumed at iteration 1/12000].\n\n\
             ## PARTIAL SUCCESS — completed work and remaining gaps\n\n\
             Proposal rejected; README.md unchanged."
        );
        let handoff = "Delegated children finished. Use this compact continuation handoff \
            instead of re-running the same stage.\n{\"children\":[{\"context\":\
            \"[Delegation] Implement ONLY the lane.\"}]}";
        let output = MemoryCandidate {
            candidate_type: "final_output_excerpt".to_string(),
            target_hint: "agent_or_user_memory_candidate_source".to_string(),
            key: None,
            value: json!({"content_excerpt": "Proposal rejected; README.md unchanged."}),
            confidence: 0.9,
            source: "task_user_output".to_string(),
            evidence: Vec::new(),
            rationale: String::new(),
        };
        for summary in [partial.as_str(), handoff] {
            let mut ctx = context(summary);
            ctx.outcome.outcome_type = "goal_achieved_partial".to_string();
            let record = V3EpisodeRecord::from_terminal_context(&ctx);
            let expected = format!("Goal partially achieved: {INTENT}");
            assert_eq!(record.outcome_kind, "partial_progress");
            assert_eq!(record.outcome_summary, expected);
            assert_eq!(record.recalled_outcome_text(), expected, "recall is stable");
            assert_eq!(record.recalled_strategy_text(), None);
            let candidates = vec![output.clone()];
            assert_eq!(
                terminal_episode_outcome_statement(
                    &ctx,
                    record.task_intent.as_deref(),
                    primary_output_excerpt(&candidates),
                ),
                format!("{expected}\nProposal rejected; README.md unchanged.")
            );

            let mut interim = record.clone();
            interim.outcome_summary = summary.to_string();
            interim.strategy_summary = Some(summary.to_string());
            interim.memory_candidates = candidates;
            let recalled = interim.recalled_outcome_text();
            assert_eq!(
                recalled,
                format!("{expected}\nProposal rejected; README.md unchanged.")
            );
            assert_eq!(interim.recalled_strategy_text(), None);
            assert_no_operating_policy(&recalled);
            assert!(!recalled.contains("children"), "{recalled}");
        }
    }
}
