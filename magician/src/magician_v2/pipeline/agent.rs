//! # Pipeline Agent
//!
//! Defines the [`PipelineAgent`] trait and the [`PipelineContext`] that is
//! threaded through every agent invocation. Each pipeline stage implements
//! `PipelineAgent`, reads from the shared [`ArtifactStore`], and writes its
//! output artifacts back.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::artifact::{ArtifactStore, ArtifactType};
use crate::magician_v2::agents::memory_tiers::MemoryTierDefinition;

// ---------------------------------------------------------------------------
// Agent ID constants (M-04)
// ---------------------------------------------------------------------------

/// Well-known system agent IDs used throughout the pipeline.
pub const AGENT_ID_SCHEDULER: &str = "system:scheduler";
pub const AGENT_ID_PLANNER: &str = "system:planner";
pub const AGENT_ID_ELICITOR: &str = "system:elicitor";
pub const AGENT_ID_QUERY_REWRITER: &str = "system:query-rewriter";
pub const AGENT_ID_ANSWER_INTERPRETER: &str = "system:answer-interpreter";
pub const AGENT_ID_SLOT_EXTRACTOR: &str = "system:slot-extractor";
pub const AGENT_ID_INTENT_CLASSIFIER: &str = "system:intent-classifier";
// AGENT_ID_AUTONOMOUS_EXECUTOR — REMOVED. Autonomous cycles bypass pipeline.
pub const AGENT_ID_PLAN_PATCHER: &str = "system:plan-patcher";
pub const MAX_REFINEMENT_ROUNDS: u32 = 2;
pub const MAX_REFINEMENT_USER_PAUSES: u32 = 2;

// ---------------------------------------------------------------------------
// Schedule types (C-09)
// ---------------------------------------------------------------------------

/// Which schedule variant drives this agent's trigger.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AgentScheduleKind {
    Cron {
        expression: String,
        timezone: Option<String>,
    },
    Interval {
        seconds: u64,
        jitter_seconds: Option<u64>,
    },
    Once {
        at: chrono::DateTime<chrono::Utc>,
    },
    OnEvent {
        event_pattern: String,
    },
}

/// What to do when fires were missed (process was down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MissedFirePolicy {
    #[default]
    Skip,
    RunOnce,
    Queue,
}

/// What to do when the previous goal run is still active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ConcurrentExecutionPolicy {
    #[default]
    Skip,
    Queue,
    CancelPrevious,
}

/// Injected by the scoped wake/scheduler dispatch path before dispatching
/// the system:scheduler step.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScheduleContext {
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub goal_id: String,
    /// Task ID when the schedule is owned by a Task (Phase 1 rearchitecture).
    /// `None` for legacy agent+goal keyed schedules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub schedule: AgentScheduleKind,
    pub last_fire: Option<chrono::DateTime<chrono::Utc>>,
    pub now: chrono::DateTime<chrono::Utc>,
    /// Number of fires missed since last successful execution (0 on normal wake).
    pub missed_fires: u32,
    pub missed_fire_policy: MissedFirePolicy,
    pub concurrent_execution_policy: ConcurrentExecutionPolicy,
}

// ---------------------------------------------------------------------------
// PipelineContext
// ---------------------------------------------------------------------------

/// Shared context threaded through every pipeline agent invocation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PipelineContext {
    // === Core (always present) ===
    pub chain_id: String,
    pub cycle_id: String,
    pub workflow_id: String,
    pub query: String,
    pub iteration: u32,

    // === Planning-specific (all Option) ===
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_answer: Option<String>,

    // === Suspension / resume (populated by orchestrator / ResumeTriggerService) ===
    /// Timestamp set by the orchestrator at the top of `run()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_started_at: Option<DateTime<Utc>>,
    /// Set by the caller on resume with the user's answer text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_mode: Option<String>,
    /// LLM fallback budget tracker — incremented by routing agents.
    #[serde(default)]
    pub llm_routing_calls: u32,
    /// I-08: number of completed elicitation rounds in this pipeline run.
    /// Incremented by the orchestrator each time system:elicitor completes.
    /// The planner reads this instead of proxying from recommended_questions.len().
    #[serde(default)]
    pub elicitation_rounds: u32,

    // === Execution-adjacent planning state ===
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The question_id from the most recent PipelineSuspension — set on resume
    /// so AnswerInterpreterAgent can correlate the user's answer to the specific question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<String>,

    // === Agent routing (M-4, L-1, L-2) ===
    /// Strongly-typed agent kind from AgentDefinition.
    /// Used by TieredRouter to short-circuit worker/autonomous agents to a dedicated executor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<crate::magician_v2::agents::types::AgentKind>,
    /// Whether this pipeline run is an autonomous cycle (not user-initiated).
    #[serde(default)]
    pub is_autonomous_cycle: bool,
    /// Agent trust level from AgentDefinition (e.g. "standard", "elevated").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
    /// Memory tier definitions from AgentDefinition, enabling mid-execution tier access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tier_definitions: Vec<MemoryTierDefinition>,
    /// Default observation mode from AgentDefinition (e.g. "screenshot", "text_first").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_mode: Option<String>,
    /// LLM model override from AgentDefinition's llm_routing config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_model_override: Option<String>,

    /// Maximum delegation depth from the agent's coordination config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delegation_depth: Option<u8>,

    // === Scheduler-specific (C-09) ===
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_context: Option<ScheduleContext>,

    // === Task execution context (deferred execution) ===
    /// Principal scope for task-backed planning runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,

    /// Workspace scope for task-backed planning runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    /// Task ID — set when this pipeline run executes on behalf of a task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,

    /// Execution ID — the current execution record for audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,

    /// API port for the magician HTTP server. Used to construct API URLs in prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_port: Option<u16>,

    // === Refinement loop (Phase C) ===
    /// Step ID currently being refined. When set, refinement agents
    /// (slot-extractor, elicitor, query-rewriter) operate on this step's task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refinement_step_id: Option<String>,

    /// Original user query, saved when entering refinement mode so we can
    /// restore it after refinement completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_query: Option<String>,

    /// Number of refinement rounds completed in this pipeline run.
    #[serde(default)]
    pub refinement_rounds: u32,

    /// Number of user elicitation pauses during refinement.
    #[serde(default)]
    pub refinement_user_pauses: u32,
}

// ---------------------------------------------------------------------------
// PipelineAgentError
// ---------------------------------------------------------------------------

/// Errors that a [`PipelineAgent`] may produce during execution.
#[derive(Debug, Error)]
pub enum PipelineAgentError {
    #[error("missing required input artifact: {0}")]
    MissingInput(String),
    #[error("agent execution failed: {0}")]
    ExecutionFailed(String),
    #[error("serialization error: {0}")]
    SerializationError(String),
    #[error("underlying service error: {0}")]
    ServiceError(String),
}

// ---------------------------------------------------------------------------
// PipelineAgentResult
// ---------------------------------------------------------------------------

/// Return type for [`PipelineAgent::execute()`].
///
/// Replaces the previous `Vec<String>` (artifact IDs only) to carry both
/// completion state and scheduling intent back to the orchestrator.
#[derive(Debug, Clone)]
#[must_use]
pub enum PipelineAgentResult {
    /// Agent completed successfully. `artifact_ids` are the IDs written to the store.
    Completed { artifact_ids: Vec<String> },
    /// Agent requested a sleep (scheduler pattern). `wake_at` is when to resume.
    /// `artifact_ids` are any artifacts written before sleeping.
    Sleeping {
        wake_at: DateTime<Utc>,
        artifact_ids: Vec<String>,
    },
    /// Agent execution failed (non-retryable from the agent's perspective).
    /// The orchestrator applies the three-tier failure protocol.
    Failed {
        reason: String,
        artifact_ids: Vec<String>,
    },
    /// Agent is waiting for user input. The orchestrator should suspend the pipeline.
    WaitingForUser { artifact_ids: Vec<String> },
}

// ---------------------------------------------------------------------------
// PipelineAgent trait
// ---------------------------------------------------------------------------

/// A single stage in the agent pipeline.
///
/// Implementations read required artifacts from the [`ArtifactStore`], perform
/// their work, and write output artifacts back into the store.
#[async_trait]
pub trait PipelineAgent: Send + Sync {
    /// Unique identifier for this agent (e.g., `"system:slot-extractor"`).
    fn agent_id(&self) -> &str;

    /// Artifact types this agent requires as input.
    fn required_inputs(&self) -> Vec<ArtifactType>;

    /// Artifact types this agent produces as output.
    fn output_types(&self) -> Vec<ArtifactType>;

    /// Execute the agent, reading from and writing to the artifact store.
    /// Returns IDs of artifacts produced.
    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError>;

    /// Whether this agent can be skipped without error (default: false).
    fn skippable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::pipeline::artifact::{AgentArtifact, ArtifactStore, ArtifactType};
    use chrono::Utc;

    /// Mock agent that implements PipelineAgent to verify trait compilation.
    struct MockAgent;

    #[async_trait]
    impl PipelineAgent for MockAgent {
        fn agent_id(&self) -> &str {
            "mock:test"
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![ArtifactType::QueryAnalysis]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            vec![ArtifactType::SlotGraph]
        }
        async fn execute(
            &self,
            store: &mut ArtifactStore,
            context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            let artifact = AgentArtifact {
                artifact_id: "test-artifact".to_string(),
                artifact_type: ArtifactType::SlotGraph,
                producer_agent_id: self.agent_id().to_string(),
                producer_cycle_id: context.cycle_id.clone(),
                content: serde_json::json!({"test": true}),
                schema_version: 1,
                produced_at: Utc::now(),
                render_hints: None,
            };
            let id = artifact.artifact_id.clone();
            store.put(artifact);
            Ok(PipelineAgentResult::Completed {
                artifact_ids: vec![id],
            })
        }
    }

    #[tokio::test]
    async fn mock_agent_implements_pipeline_agent() {
        let agent: Box<dyn PipelineAgent> = Box::new(MockAgent);
        assert_eq!(agent.agent_id(), "mock:test");
        assert_eq!(agent.required_inputs(), vec![ArtifactType::QueryAnalysis]);
        assert_eq!(agent.output_types(), vec![ArtifactType::SlotGraph]);
        assert!(!agent.skippable());

        let mut store = ArtifactStore::new("test-chain".to_string());
        let context = PipelineContext {
            chain_id: "test-chain".to_string(),
            cycle_id: "cycle-1".to_string(),
            workflow_id: "wf-1".to_string(),
            query: "test query".to_string(),
            iteration: 0,
            agent_id: None,
            correlation_id: None,
            user_answer: None,
            run_started_at: None,
            resume_mode: None,
            llm_routing_calls: 0,
            elicitation_rounds: 0,
            session_id: None,
            question_id: None,
            agent_kind: None,
            is_autonomous_cycle: false,
            trust_level: None,
            tier_definitions: Vec::new(),
            observation_mode: None,
            llm_model_override: None,
            max_delegation_depth: None,
            schedule_context: None,
            ..Default::default()
        };

        let result = agent.execute(&mut store, &context).await.unwrap();
        let ids = match result {
            PipelineAgentResult::Completed { artifact_ids } => artifact_ids,
            _ => panic!("expected Completed"),
        };
        assert_eq!(ids, vec!["test-artifact"]);
        assert!(store.get("test-artifact").is_some());
    }

    #[test]
    fn pipeline_context_serde_roundtrip() {
        let ctx = PipelineContext {
            chain_id: "chain-1".to_string(),
            cycle_id: "cycle-1".to_string(),
            workflow_id: "wf-1".to_string(),
            query: "book a meeting".to_string(),
            iteration: 2,
            agent_id: Some("agent-xyz".to_string()),
            correlation_id: None,
            user_answer: Some("yes".to_string()),
            run_started_at: None,
            resume_mode: None,
            llm_routing_calls: 0,
            elicitation_rounds: 0,
            session_id: None,
            question_id: None,
            agent_kind: None,
            is_autonomous_cycle: false,
            trust_level: None,
            tier_definitions: Vec::new(),
            observation_mode: None,
            llm_model_override: None,
            max_delegation_depth: None,
            schedule_context: None,
            ..Default::default()
        };

        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: PipelineContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.chain_id, "chain-1");
        assert_eq!(deserialized.iteration, 2);
        assert_eq!(deserialized.user_answer, Some("yes".to_string()));
        assert!(deserialized.correlation_id.is_none());
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod schedule_context_tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn pipeline_context_carries_schedule_context() {
        let ctx = PipelineContext {
            chain_id: "c1".into(),
            cycle_id: "cy1".into(),
            workflow_id: "w1".into(),
            query: "scan".into(),
            iteration: 0,
            schedule_context: Some(ScheduleContext {
                principal: "anonymous".into(),
                workspace: "default".into(),
                agent_id: "career-copilot".into(),
                goal_id: "scan_opportunities".into(),
                task_id: None,
                schedule: AgentScheduleKind::Interval {
                    seconds: 3600,
                    jitter_seconds: None,
                },
                last_fire: None,
                now: Utc::now(),
                missed_fires: 0,
                missed_fire_policy: MissedFirePolicy::Skip,
                concurrent_execution_policy: ConcurrentExecutionPolicy::Skip,
            }),
            ..PipelineContext::default()
        };
        let sc = ctx.schedule_context.unwrap();
        assert_eq!(sc.principal, "anonymous");
        assert_eq!(sc.workspace, "default");
        assert_eq!(sc.agent_id, "career-copilot");
        assert_eq!(sc.missed_fires, 0);
    }
}
