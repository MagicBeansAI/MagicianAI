//! Pipeline agent wrapper for [`IntentAwareProcessor`].
//!
//! Classifies user intent (new task, slot answer, status query, cancellation,
//! etc.) and produces `QueryAnalysis` + `IntentClassification` artifacts.
//! First stage of the pipeline — cold-start routing directs here.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use crate::magician_v2::intent_handler::{IntentAwareProcessor, MessageHandlingResult};
use crate::magician_v2::pipeline::agent::{
    PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext,
    AGENT_ID_INTENT_CLASSIFIER,
};
use crate::magician_v2::pipeline::artifact::{
    AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION,
};
use crate::magician_v2::query_analysis::intent::QueryIntent;
use runtime_core::{ExecutionContext, ToolCatalog};

// ---------------------------------------------------------------------------
// IntentClassificationResult
// ---------------------------------------------------------------------------

/// Artifact payload written by the IntentClassifierAgent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentClassificationResult {
    pub intent: QueryIntent,
    /// `false` for StatusResponse, Cancelled, Resumed, NeedsClarification, AmbiguousAnswer.
    pub requires_pipeline: bool,
    /// Populated by `SlotAnswered` so TieredRouter Rules 1-3 can route to answer-interpreter.
    pub user_answer: Option<String>,
    pub is_new_task: bool,
    /// Duration of the intent classification LLM call (milliseconds).
    /// Used by `handle_pipeline_outcome` for `AnalysisMetadata.analysis_duration_ms`.
    #[serde(default)]
    pub classification_duration_ms: u64,
}

// ---------------------------------------------------------------------------
// IntentClassifierAgent
// ---------------------------------------------------------------------------

/// Wraps [`IntentAwareProcessor`] behind the [`PipelineAgent`] trait.
pub struct IntentClassifierAgent {
    intent_processor: Arc<IntentAwareProcessor>,
    tool_catalog: Arc<dyn ToolCatalog>,
    execution_context: ExecutionContext,
}

impl IntentClassifierAgent {
    pub fn new(
        intent_processor: Arc<IntentAwareProcessor>,
        tool_catalog: Arc<dyn ToolCatalog>,
        execution_context: ExecutionContext,
    ) -> Self {
        Self {
            intent_processor,
            tool_catalog,
            execution_context,
        }
    }
}

#[async_trait]
impl PipelineAgent for IntentClassifierAgent {
    fn agent_id(&self) -> &str {
        AGENT_ID_INTENT_CLASSIFIER
    }

    fn required_inputs(&self) -> Vec<ArtifactType> {
        vec![] // first stage — no inputs required
    }

    fn output_types(&self) -> Vec<ArtifactType> {
        vec![
            ArtifactType::IntentClassification,
            ArtifactType::QueryAnalysis,
        ]
    }

    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError> {
        let query = &context.query;
        let execution_id = context.execution_id.as_deref().unwrap_or(&context.chain_id);
        let correlation_id = context
            .correlation_id
            .as_deref()
            .unwrap_or(&context.cycle_id);

        // Load tools for query analysis context.
        let available_tools = self
            .tool_catalog
            .all_tools(&self.execution_context)
            .await
            .ok();

        let classification_start = std::time::Instant::now();
        let intent_result = self
            .intent_processor
            .handle_message(
                execution_id,
                query,
                correlation_id,
                &self.execution_context,
                available_tools.as_deref(),
            )
            .await
            .map_err(|e| {
                PipelineAgentError::ServiceError(format!("Intent handling failed: {}", e))
            })?;
        let classification_duration_ms = classification_start.elapsed().as_millis() as u64;

        // Extract the UnifiedQueryAnalysis from the result (all variants carry it).
        let query_analysis = match &intent_result {
            MessageHandlingResult::NewTask { analysis } => analysis.clone(),
            MessageHandlingResult::SlotAnswered { analysis, .. } => analysis.clone(),
            MessageHandlingResult::NeedsClarification { analysis, .. } => analysis.clone(),
            MessageHandlingResult::AmbiguousAnswer { analysis, .. } => analysis.clone(),
            MessageHandlingResult::StatusResponse { analysis, .. } => analysis.clone(),
            MessageHandlingResult::Cancelled { analysis } => analysis.clone(),
            MessageHandlingResult::Resumed { analysis } => analysis.clone(),
            MessageHandlingResult::ClarificationNeeded { analysis } => analysis.clone(),
        };

        let is_new_task = matches!(intent_result, MessageHandlingResult::NewTask { .. });

        // Determine requires_pipeline and user_answer from the intent result.
        let mut user_answer: Option<String> = None;
        let requires_pipeline = match &intent_result {
            MessageHandlingResult::NewTask { .. }
            | MessageHandlingResult::ClarificationNeeded { .. } => true,

            MessageHandlingResult::SlotAnswered { value, .. } => {
                user_answer = Some(
                    value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| value.to_string()),
                );
                true
            },

            MessageHandlingResult::StatusResponse { .. }
            | MessageHandlingResult::Cancelled { .. }
            | MessageHandlingResult::Resumed { .. }
            | MessageHandlingResult::NeedsClarification { .. }
            | MessageHandlingResult::AmbiguousAnswer { .. } => false,
        };

        info!(
            "[PIPELINE:intent-classifier] intent={:?}, requires_pipeline={}, is_new_task={}, \
             complexity={:.2}",
            query_analysis.intent, requires_pipeline, is_new_task, query_analysis.complexity.score
        );

        // On NewTask, clear stale prior-conversation artifacts that could mislead
        // downstream routing.  Intentionally preserves PlanGraph (may be pre-seeded
        // by the goal fast-path) and StepExecution artifacts.
        if is_new_task {
            for purge_type in &[
                ArtifactType::IntentClassification,
                ArtifactType::QueryAnalysis,
                ArtifactType::SlotGraph,
                ArtifactType::ElicitationResult,
                ArtifactType::InterpretedAnswer,
                ArtifactType::ClarifiedTask,
            ] {
                store.remove_all_of_type(purge_type);
            }
        }

        let mut artifact_ids = Vec::with_capacity(2);

        // Remove stale IntentClassification entries on continuation turns
        // (no-op after targeted purge on NewTask).
        if !is_new_task {
            store.remove_all_of_type(&ArtifactType::IntentClassification);
        }

        // Write IntentClassification artifact.
        let classification = IntentClassificationResult {
            intent: query_analysis.intent.clone(),
            requires_pipeline,
            user_answer,
            is_new_task,
            classification_duration_ms,
        };
        let ic_content = serde_json::to_value(&classification)
            .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?;
        let ic_id = Uuid::new_v4().to_string();
        store.put(AgentArtifact {
            artifact_id: ic_id.clone(),
            artifact_type: ArtifactType::IntentClassification,
            producer_agent_id: self.agent_id().to_string(),
            producer_cycle_id: context.cycle_id.clone(),
            content: ic_content,
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });
        artifact_ids.push(ic_id);

        // Write QueryAnalysis artifact.
        // Remove stale entries so we don't accumulate per-turn (no-op after targeted purge on NewTask).
        if !is_new_task {
            store.remove_all_of_type(&ArtifactType::QueryAnalysis);
        }
        let qa_content = serde_json::to_value(&query_analysis)
            .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?;
        let qa_id = Uuid::new_v4().to_string();
        store.put(AgentArtifact {
            artifact_id: qa_id.clone(),
            artifact_type: ArtifactType::QueryAnalysis,
            producer_agent_id: self.agent_id().to_string(),
            producer_cycle_id: context.cycle_id.clone(),
            content: qa_content,
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });
        artifact_ids.push(qa_id);

        Ok(PipelineAgentResult::Completed { artifact_ids })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn intent_classifier_agent_compiles_as_trait_object() {
        fn _assert_pipeline_agent<T: PipelineAgent>() {}
        _assert_pipeline_agent::<IntentClassifierAgent>();
    }

    #[test]
    fn intent_classification_result_serde_roundtrip() {
        let result = IntentClassificationResult {
            intent: QueryIntent::NewTask,
            requires_pipeline: true,
            user_answer: None,
            is_new_task: true,
            classification_duration_ms: 42,
        };
        let json = serde_json::to_value(&result).expect("serialize");
        let back: IntentClassificationResult = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.intent, QueryIntent::NewTask);
        assert!(back.requires_pipeline);
        assert!(back.is_new_task);
        assert!(back.user_answer.is_none());
        assert_eq!(back.classification_duration_ms, 42);
    }

    #[test]
    fn intent_classification_result_non_task() {
        let result = IntentClassificationResult {
            intent: QueryIntent::StatusQuery,
            requires_pipeline: false,
            user_answer: None,
            is_new_task: false,
            classification_duration_ms: 0,
        };
        let json = serde_json::to_value(&result).expect("serialize");
        let back: IntentClassificationResult = serde_json::from_value(json).expect("deserialize");
        assert!(!back.requires_pipeline);
    }
}
