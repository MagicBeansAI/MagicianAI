//! Pipeline agent wrapper for [`ElicitationService`].
//!
//! Calls [`ElicitationService::elicit_and_rewrite`] and stores the resulting
//! [`ElicitationResult`] as an [`ArtifactType::ElicitationResult`] artifact.
//! The `needs_clarification` field in the artifact drives router Rule 7
//! (Pause) vs Rule 8 (proceed to rewriter).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution;
use crate::magician_v2::pipeline::agent::{
    PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext, AGENT_ID_ELICITOR,
    AGENT_ID_SLOT_EXTRACTOR,
};
use crate::magician_v2::pipeline::artifact::{
    AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION,
};
use crate::magician_v2::query_analysis::unified_analyzer::UnifiedQueryAnalysis;
use crate::magician_v2::slot_graph::{
    ConversationContext, ElicitationService, ProvisionalSlot, SlotRecord,
};

/// Wraps [`ElicitationService`] behind the [`PipelineAgent`] trait.
///
/// Calls `elicit_and_rewrite(workflow_id, query, None, None, None)` and
/// stores the `ElicitationResult` as an artifact.  The router inspects
/// `content["needs_clarification"]` to decide whether to pause or continue.
pub struct ElicitorAgent {
    service: Arc<ElicitationService>,
}

impl ElicitorAgent {
    /// Create a new wrapper around the given elicitation service.
    pub fn new(service: Arc<ElicitationService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl PipelineAgent for ElicitorAgent {
    fn agent_id(&self) -> &str {
        AGENT_ID_ELICITOR
    }

    fn required_inputs(&self) -> Vec<ArtifactType> {
        // SlotGraph listed for routing dependency; not consumed in execute().
        vec![ArtifactType::SlotGraph]
    }

    fn output_types(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::ElicitationResult]
    }

    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError> {
        debug!(
            "[PIPELINE:elicitor] elicit_and_rewrite workflow_id='{}', query='{}'",
            context.workflow_id, context.query
        );

        // Issue 5: read QueryAnalysis artifact from store and pass it through.
        let query_analysis_opt: Option<UnifiedQueryAnalysis> = store
            .latest_of_type(&ArtifactType::QueryAnalysis)
            .and_then(|a| {
                a.deserialize_content()
                    .map_err(|e| {
                        warn!(
                            "[PIPELINE:elicitor] QueryAnalysis deserialize failed: {}",
                            e
                        );
                        e
                    })
                    .ok()
            });

        // B-02: forward existing slots from SlotGraph artifact as conversation context.
        // M-01: try Vec<ProvisionalSlot> first (written by slot-extractor), then fall back to
        // Vec<SlotRecord> directly (written by query-rewriter after normalization).
        let conversation_context = extract_conversation_context(store);

        // Planning latency P3: the slot-extractor stage of THIS run already
        // extracted (and memory-resolved) the slots for this message. Hand
        // them over as the extraction result instead of asking the service to
        // extract again with those slots as "existing" context.
        let extracted_slots = slots_extracted_this_run(store, context);

        let result = self
            .service
            .elicit_and_rewrite_with_scope(
                &context.workflow_id,
                &context.query,
                query_analysis_opt.as_ref(),
                conversation_context,
                extracted_slots,
                None, // stage_context_override
                context.principal.as_deref(),
                context.workspace.as_deref(),
                OperationLlmCallAttribution {
                    execution_id: context.execution_id.clone(),
                    task_id: context.task_id.clone(),
                    agent_id: context.agent_id.clone(),
                    ..OperationLlmCallAttribution::default()
                },
            )
            .await
            .map_err(|e| PipelineAgentError::ServiceError(e.to_string()))?;

        info!(
            "[PIPELINE:elicitor] needs_clarification={}, slots={}, questions={}",
            result.needs_clarification,
            result.slot_graph.len(),
            result.recommended_questions.len(),
        );

        let mut content = serde_json::to_value(&result)
            .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?;

        // Issue 1: project question_text and slot_ids for router inspection.
        let question_text = result
            .recommended_questions
            .first()
            .map(|q| q.question_text.clone())
            .unwrap_or_default();
        let slot_ids: Vec<serde_json::Value> = build_slot_ids(&result.recommended_questions);
        if let Some(obj) = content.as_object_mut() {
            obj.insert(
                "question_text".to_string(),
                serde_json::Value::String(question_text),
            );
            obj.insert("slot_ids".to_string(), serde_json::Value::Array(slot_ids));
        }

        let artifact_id = Uuid::new_v4().to_string();
        let artifact = AgentArtifact {
            artifact_id: artifact_id.clone(),
            artifact_type: ArtifactType::ElicitationResult,
            producer_agent_id: self.agent_id().to_string(),
            producer_cycle_id: context.cycle_id.clone(),
            content,
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        };

        store.put(artifact);
        Ok(PipelineAgentResult::Completed {
            artifact_ids: vec![artifact_id],
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract [`ConversationContext`] from the `SlotGraph` artifact in `store`.
///
/// Tries to deserialize the artifact content as `Vec<ProvisionalSlot>` first
/// (older slot-extractor snapshots), converting each to a `SlotRecord`. Falls
/// back to interpreting the content as `Vec<SlotRecord>` directly (the
/// normalized slot-extractor/query-rewriter path). Returns `None` when no
/// `SlotGraph` artifact is present or neither format parses.
pub fn extract_conversation_context(store: &ArtifactStore) -> Option<ConversationContext> {
    store
        .latest_of_type(&ArtifactType::SlotGraph)
        .and_then(|a| {
            // Primary: try ProvisionalSlot (written by slot-extractor)
            if let Ok(provisionals) = a.deserialize_content::<Vec<ProvisionalSlot>>() {
                let existing_slots: Vec<SlotRecord> = provisionals
                    .into_iter()
                    .map(|p| SlotRecord::from_provisional("pipeline", p))
                    .collect();
                return Some(ConversationContext {
                    existing_slots,
                    previous_messages: vec![],
                    screenshots: vec![],
                });
            }
            // Fallback: try SlotRecord directly (written by query-rewriter)
            if let Ok(records) = a.deserialize_content::<Vec<SlotRecord>>() {
                return Some(ConversationContext {
                    existing_slots: records,
                    previous_messages: vec![],
                    screenshots: vec![],
                });
            }
            None
        })
}

/// Slots the slot-extractor stage produced during the current pipeline run.
///
/// Returns `Some` only when the latest `SlotGraph` artifact was written by
/// `system:slot-extractor` at or after `context.run_started_at`, i.e. for
/// this turn's message. A graph carried over from an earlier run (resume,
/// light update after an interpreted answer) or written by another producer
/// returns `None`, and the elicitation service extracts as before.
pub fn slots_extracted_this_run(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<Vec<SlotRecord>> {
    let run_started_at = context.run_started_at?;
    let artifact = store.latest_of_type(&ArtifactType::SlotGraph)?;
    if artifact.producer_agent_id != AGENT_ID_SLOT_EXTRACTOR
        || artifact.produced_at < run_started_at
    {
        return None;
    }
    extract_conversation_context(store).map(|ctx| ctx.existing_slots)
}

/// Build the `slot_ids` JSON array for the ElicitationResult artifact.
///
/// For each question, use `source_slot_id` when present.  When it is `None`
/// (generic clarifying questions not tied to a specific slot), fall back to a
/// positional key `"question-{i}"` so the list is never empty when questions
/// exist.  An empty `slot_ids` array would cause Rule 7 in the router to
/// return `Error` instead of `Pause`, killing the pipeline before the user is
/// asked a clarifying question.
fn build_slot_ids(
    questions: &[crate::magician_v2::ask_loop::clarifier::ClarifierQuestion],
) -> Vec<serde_json::Value> {
    questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let id = q
                .source_slot_id
                .clone()
                .unwrap_or_else(|| format!("question-{}", i));
            serde_json::Value::String(id)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::ask_loop::clarifier::ClarifierQuestion;

    #[test]
    fn elicitor_agent_compiles_as_trait_object() {
        fn _assert_pipeline_agent<T: PipelineAgent>() {}
        _assert_pipeline_agent::<ElicitorAgent>();
    }

    /// C-07 regression: when all questions have `source_slot_id = None`,
    /// `build_slot_ids` must return a non-empty list so Rule 7 can produce
    /// `Pause` instead of `Error`.
    #[test]
    fn slot_ids_non_empty_when_source_slot_id_is_none() {
        // Create two questions with no source_slot_id.
        let q1 = ClarifierQuestion {
            source_slot_id: None,
            question_text: "What environment are you targeting?".to_string(),
            ..ClarifierQuestion::default()
        };
        let q2 = ClarifierQuestion {
            source_slot_id: None,
            question_text: "Any deadline constraints?".to_string(),
            ..ClarifierQuestion::default()
        };

        let slot_ids = build_slot_ids(&[q1, q2]);

        assert_eq!(
            slot_ids.len(),
            2,
            "slot_ids must have one entry per question"
        );
        assert_eq!(
            slot_ids[0],
            serde_json::Value::String("question-0".to_string())
        );
        assert_eq!(
            slot_ids[1],
            serde_json::Value::String("question-1".to_string())
        );
    }

    /// When `source_slot_id` is present it must be used as-is.
    #[test]
    fn slot_ids_uses_source_slot_id_when_present() {
        let q = ClarifierQuestion {
            source_slot_id: Some("slot-abc-123".to_string()),
            question_text: "Which region?".to_string(),
            ..ClarifierQuestion::default()
        };

        let slot_ids = build_slot_ids(&[q]);

        assert_eq!(slot_ids.len(), 1);
        assert_eq!(
            slot_ids[0],
            serde_json::Value::String("slot-abc-123".to_string())
        );
    }

    /// Mixed: some questions have a slot id, others do not.
    #[test]
    fn slot_ids_mixed_some_and_none() {
        let q0 = ClarifierQuestion {
            source_slot_id: Some("slot-xyz".to_string()),
            ..ClarifierQuestion::default()
        };
        let q1 = ClarifierQuestion {
            source_slot_id: None,
            ..ClarifierQuestion::default()
        };

        let slot_ids = build_slot_ids(&[q0, q1]);

        assert_eq!(slot_ids.len(), 2);
        assert_eq!(
            slot_ids[0],
            serde_json::Value::String("slot-xyz".to_string())
        );
        assert_eq!(
            slot_ids[1],
            serde_json::Value::String("question-1".to_string())
        );
    }

    /// Empty question list must produce an empty slot_ids list.
    #[test]
    fn slot_ids_empty_when_no_questions() {
        let slot_ids = build_slot_ids(&[]);
        assert!(slot_ids.is_empty());
    }

    // -----------------------------------------------------------------------
    // I-21: extract_conversation_context() tests
    // -----------------------------------------------------------------------

    fn make_store_with_slot_graph(content: serde_json::Value) -> ArtifactStore {
        let mut store = ArtifactStore::new("test-chain".to_string());
        store.put(AgentArtifact {
            artifact_id: "sg-1".to_string(),
            artifact_type: ArtifactType::SlotGraph,
            producer_agent_id: "test".to_string(),
            producer_cycle_id: "c0".to_string(),
            content,
            schema_version: crate::magician_v2::pipeline::artifact::ARTIFACT_SCHEMA_VERSION,
            produced_at: chrono::Utc::now(),
            render_hints: None,
        });
        store
    }

    /// I-21: extract_conversation_context returns slots when content is Vec<ProvisionalSlot>.
    #[test]
    fn extract_conversation_context_from_provisional_slots() {
        let provisionals = serde_json::json!([
            {
                "slot_type": "entity",
                "value": {"raw": "production"},
                "confidence": 0.9,
                "rationale": "user specified"
            }
        ]);
        let store = make_store_with_slot_graph(provisionals);
        let ctx = extract_conversation_context(&store)
            .expect("should produce a ConversationContext from ProvisionalSlot content");
        assert_eq!(ctx.existing_slots.len(), 1);
        // The slot id is generated by from_provisional; just assert it's non-empty.
        assert!(!ctx.existing_slots[0].id.is_empty());
    }

    /// I-21: extract_conversation_context falls back to Vec<SlotRecord> when ProvisionalSlot
    /// parse fails (e.g. content written by query-rewriter after normalization).
    #[test]
    fn extract_conversation_context_from_slot_records() {
        let now = chrono::Utc::now().timestamp_millis();
        let records = serde_json::json!([
            {
                "id": "slot-001",
                "slot_type": "entity",
                "value": {"raw": "staging"},
                "confidence": 0.85,
                "provenance": [{"source": "user_reply", "timestamp": now}],
                "evidence_links": [],
                "created_at": now,
                "updated_at": now
            }
        ]);
        let store = make_store_with_slot_graph(records);
        let ctx = extract_conversation_context(&store)
            .expect("should produce a ConversationContext from SlotRecord content");
        assert_eq!(ctx.existing_slots.len(), 1);
        assert_eq!(ctx.existing_slots[0].id, "slot-001");
    }

    // -----------------------------------------------------------------------
    // I-21: artifact content projection test (question_text + slot_ids)
    // -----------------------------------------------------------------------

    /// I-21: After serializing an ElicitationResult to JSON, the projection logic must
    /// insert `question_text` (from the first question) and `slot_ids` into the content object.
    #[test]
    fn artifact_content_gains_question_text_and_slot_ids() {
        use crate::magician_v2::ask_loop::clarifier::ClarifierQuestion;
        use crate::magician_v2::confidence::ConfidenceSummary;
        use crate::magician_v2::slot_graph::ClarifiedTask;
        use crate::magician_v2::slot_graph::ElicitationResult;

        let q = ClarifierQuestion {
            source_slot_id: Some("slot-xyz".to_string()),
            question_text: "Which environment?".to_string(),
            ..ClarifierQuestion::default()
        };
        let result = ElicitationResult {
            slot_graph: vec![],
            clarified_task: ClarifiedTask::default(),
            confidence_summary: ConfidenceSummary {
                overall: 0.5,
                min_critical_slot: 0.5,
                unresolved_slots: vec![],
            },
            needs_clarification: true,
            recommended_questions: vec![q],
            enrichment_summary: None,
            slot_trigger_mappings: vec![],
            confidence_boost_results: Default::default(),
            llm_calls_used: 0,
        };

        // Replicate the projection logic from execute() lines 121–134.
        let mut content = serde_json::to_value(&result).expect("serialize ElicitationResult");
        let question_text = result
            .recommended_questions
            .first()
            .map(|q| q.question_text.clone())
            .unwrap_or_default();
        let slot_ids = build_slot_ids(&result.recommended_questions);
        if let Some(obj) = content.as_object_mut() {
            obj.insert(
                "question_text".to_string(),
                serde_json::Value::String(question_text.clone()),
            );
            obj.insert("slot_ids".to_string(), serde_json::Value::Array(slot_ids));
        }

        assert_eq!(
            content.get("question_text").and_then(|v| v.as_str()),
            Some("Which environment?"),
            "question_text should be projected into content"
        );
        let slot_ids_arr = content
            .get("slot_ids")
            .and_then(|v| v.as_array())
            .expect("slot_ids array should be present");
        assert_eq!(slot_ids_arr.len(), 1);
        assert_eq!(
            slot_ids_arr[0],
            serde_json::Value::String("slot-xyz".to_string())
        );
    }

    /// Planning latency P3: the elicitor reuses the slot-extractor's output
    /// for the same run instead of extracting a second time.
    #[test]
    fn slots_extracted_this_run_reuses_only_this_runs_slot_extractor_graph() {
        use chrono::Duration;

        let run_started_at = Utc::now() - Duration::seconds(30);
        let slot = crate::magician_v2::slot_graph::SlotRecord {
            id: "wf::slot-1".to_string(),
            slot_type: crate::magician_v2::slot_graph::SlotType::Entity,
            value: serde_json::json!({"name": "Hacker News"}),
            confidence: 0.99,
            provenance: vec![],
            evidence_links: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let graph = |producer: &str, at| AgentArtifact {
            artifact_id: Uuid::new_v4().to_string(),
            artifact_type: ArtifactType::SlotGraph,
            producer_agent_id: producer.to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::to_value(vec![slot.clone()]).unwrap(),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: at,
            render_hints: None,
        };
        let context = PipelineContext {
            workflow_id: "wf".to_string(),
            run_started_at: Some(run_started_at),
            ..PipelineContext::default()
        };

        let mut fresh = ArtifactStore::new("chain");
        fresh.put(graph(
            AGENT_ID_SLOT_EXTRACTOR,
            run_started_at + Duration::seconds(5),
        ));
        let reused = slots_extracted_this_run(&fresh, &context).expect("reused");
        assert_eq!(reused.len(), 1);
        assert_eq!(reused[0].id, "wf::slot-1", "slot ids are kept as extracted");

        let mut stale = ArtifactStore::new("chain");
        stale.put(graph(
            AGENT_ID_SLOT_EXTRACTOR,
            run_started_at - Duration::seconds(5),
        ));
        assert!(slots_extracted_this_run(&stale, &context).is_none());

        let mut other = ArtifactStore::new("chain");
        other.put(graph(
            "system:query-rewriter",
            run_started_at + Duration::seconds(5),
        ));
        assert!(slots_extracted_this_run(&other, &context).is_none());

        let mut no_run = context.clone();
        no_run.run_started_at = None;
        assert!(slots_extracted_this_run(&fresh, &no_run).is_none());
    }
}
