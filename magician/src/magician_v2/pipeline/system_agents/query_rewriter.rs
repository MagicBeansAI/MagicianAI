//! Pipeline agent wrapper for [`QuestionRewriter`].
//!
//! Reads `SlotGraph` and `InterpretedAnswer` artifacts, delegates to the
//! underlying `QuestionRewriter::rewrite_for_planner()`, and produces a
//! `ClarifiedTask` artifact.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    ask_loop::answer_interpreter::InterpretedAnswer,
    pipeline::{
        agent::{
            PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext,
            AGENT_ID_QUERY_REWRITER,
        },
        artifact::{AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION},
    },
    slot_graph::{types::SlotRecord, ProvisionalSlot, QuestionRewriter},
};

/// Pipeline agent that wraps [`QuestionRewriter`].
pub struct QueryRewriterAgent {
    rewriter: Arc<QuestionRewriter>,
    event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
}

impl QueryRewriterAgent {
    /// Create a new wrapper around an existing `QuestionRewriter`.
    pub fn new(rewriter: Arc<QuestionRewriter>) -> Self {
        Self {
            rewriter,
            event_broadcaster: None,
        }
    }

    pub fn with_event_broadcaster(
        mut self,
        broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    ) -> Self {
        self.event_broadcaster = broadcaster;
        self
    }
}

#[async_trait]
impl PipelineAgent for QueryRewriterAgent {
    fn agent_id(&self) -> &str {
        AGENT_ID_QUERY_REWRITER
    }

    /// SlotGraph is the hard requirement. InterpretedAnswer is consumed when
    /// present (resume path) but intentionally omitted from required_inputs —
    /// on first-pass (rule_slots_resolved) no answer has been interpreted yet.
    fn required_inputs(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::SlotGraph]
    }

    fn output_types(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::ClarifiedTask]
    }

    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError> {
        let query = &context.query;

        // Planning latency P3: on a fresh turn the elicitor already rewrote
        // this run's slot graph (Conservative + Aggressive passes) and found
        // nothing to ask. Re-running the same rewrite over the same inputs only
        // repeats two LLM calls, so publish the elicitor's ClarifiedTask.
        if let Some(clarified_task) = reusable_elicitation_rewrite(store, context) {
            info!(
                "[PIPELINE:query-rewriter] Reusing this run's elicitation rewrite \
                 (confidence={:.2}, task='{}'); skipping a second question_rewriting pass",
                clarified_task.confidence, clarified_task.clarified_task
            );
            let artifact_id = Uuid::new_v4().to_string();
            store.put(AgentArtifact {
                artifact_id: artifact_id.clone(),
                artifact_type: ArtifactType::ClarifiedTask,
                producer_agent_id: self.agent_id().to_string(),
                producer_cycle_id: context.cycle_id.clone(),
                content: serde_json::to_value(&clarified_task)
                    .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: Utc::now(),
                render_hints: None,
            });
            return Ok(PipelineAgentResult::Completed {
                artifact_ids: vec![artifact_id],
            });
        }

        // ---- Deserialize slot records from the SlotGraph artifact ----
        // SlotExtractorAgent may persist normalized Vec<SlotRecord> when
        // memory auto-resolution upgrades the extracted slots. Older snapshots
        // and some seeded tests still use Vec<ProvisionalSlot>. Try that first,
        // then fall back to Vec<SlotRecord>; default to empty.
        let slot_records: Vec<SlotRecord> = store
            .latest_of_type(&ArtifactType::SlotGraph)
            .and_then(|a| {
                // Primary: SlotExtractorAgent writes Vec<ProvisionalSlot>
                if let Ok(provisional) = a.deserialize_content::<Vec<ProvisionalSlot>>() {
                    let records: Vec<SlotRecord> = provisional
                        .into_iter()
                        .map(|p| SlotRecord::from_provisional("pipeline", p))
                        .collect();
                    return Some(records);
                }
                // Fallback: ElicitorAgent or other producers write Vec<SlotRecord>
                a.deserialize_content::<Vec<SlotRecord>>()
                    .map_err(|e| {
                        warn!(
                            "[PIPELINE:query-rewriter] SlotGraph deserialization failed as both ProvisionalSlot and SlotRecord: {}",
                            e
                        );
                        e
                    })
                    .ok()
            })
            .unwrap_or_default();

        // B-03 / C-01: merge slots from InterpretedAnswer (resume path) into slot_records.
        // Upsert by slot_type: if a slot with the same slot_type already exists, replace it
        // (so the interpreted/answered value wins); otherwise append it. Using slot_type as
        // the stable key avoids the duplicate-ID problem where UUID-based ids differ between
        // the SlotGraph path and the InterpretedAnswer path for what is semantically the same
        // slot, which previously caused the answered slot to still appear as unresolved.
        let mut slot_records = slot_records;
        if let Some(interpreted) = store
            .latest_of_type(&ArtifactType::InterpretedAnswer)
            .and_then(|a| a.deserialize_content::<InterpretedAnswer>().ok())
        {
            for interpreted_slot in interpreted.slots {
                if let Some(pos) = slot_records
                    .iter()
                    .position(|s| s.slot_type == interpreted_slot.slot_type)
                {
                    slot_records[pos] = interpreted_slot;
                } else {
                    slot_records.push(interpreted_slot);
                }
            }
        }

        // IDs of slots that still lack a value (unresolved).
        let unresolved_ids: Vec<String> = slot_records
            .iter()
            .filter(|r| r.value.is_null())
            .map(|r| r.id.clone())
            .collect();

        // I-04: use prior clarified text as the second arg, falling back to the raw
        // query only when no ClarifiedTask artifact exists in the store.
        let prior_clarified: String = store
            .latest_of_type(&ArtifactType::ClarifiedTask)
            .and_then(|a| {
                a.content
                    .get("clarified_task")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| query.to_string());

        debug!(
            "[PIPELINE:query-rewriter] Rewriting query='{}', prior_clarified='{}', slots={}, unresolved={}",
            query,
            prior_clarified,
            slot_records.len(),
            unresolved_ids.len(),
        );

        let telemetry = match (
            self.event_broadcaster.as_ref(),
            context.principal.as_deref(),
            context.workspace.as_deref(),
        ) {
            (Some(broadcaster), Some(principal), Some(workspace)) => {
                Some(OperationLlmTelemetryContext::new(
                    Arc::clone(broadcaster),
                    principal,
                    workspace,
                    "question_rewriting",
                ))
            },
            _ => None,
        };
        let clarified_task = self
            .rewriter
            .rewrite_for_planner_with_telemetry(
                query,
                &prior_clarified,
                &slot_records,
                &unresolved_ids,
                telemetry.as_ref(),
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
            "[PIPELINE:query-rewriter] Produced ClarifiedTask confidence={:.2}, task='{}'",
            clarified_task.confidence, clarified_task.clarified_task
        );

        // ---- Store the artifact ----
        let artifact_id = Uuid::new_v4().to_string();
        let artifact = AgentArtifact {
            artifact_id: artifact_id.clone(),
            artifact_type: ArtifactType::ClarifiedTask,
            producer_agent_id: self.agent_id().to_string(),
            producer_cycle_id: context.cycle_id.clone(),
            content: serde_json::to_value(&clarified_task)
                .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
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

/// The elicitor's `ClarifiedTask` when it is exactly what this stage would
/// compute again.
///
/// Holds only when, within the current run (`context.run_started_at`):
/// - the latest `ElicitationResult` resolved (`needs_clarification == false`)
///   and was produced after the latest `SlotGraph` — the elicitor rewrote the
///   same slot graph this stage would read;
/// - no `InterpretedAnswer` exists — this stage would merge answered slots
///   the elicitor never saw;
/// - no `ClarifiedTask` exists — this stage would rewrite from the prior
///   clarified text rather than the raw message the elicitor used.
///
/// That is the first pass of a new task (intent classification purges all
/// three on `NewTask`). Resume and replan turns keep rewriting here.
pub fn reusable_elicitation_rewrite(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<crate::magician_v2::slot_graph::ClarifiedTask> {
    let run_started_at = context.run_started_at?;
    if store.latest_of_type(&ArtifactType::ClarifiedTask).is_some()
        || store
            .latest_of_type(&ArtifactType::InterpretedAnswer)
            .is_some()
    {
        return None;
    }
    let elicitation = store.latest_of_type(&ArtifactType::ElicitationResult)?;
    let slot_graph = store.latest_of_type(&ArtifactType::SlotGraph)?;
    if elicitation.produced_at < run_started_at || elicitation.produced_at < slot_graph.produced_at
    {
        return None;
    }
    if elicitation
        .content
        .get("needs_clarification")
        .and_then(|v| v.as_bool())
        != Some(false)
    {
        return None;
    }
    let clarified_task: crate::magician_v2::slot_graph::ClarifiedTask =
        serde_json::from_value(elicitation.content.get("clarified_task")?.clone()).ok()?;
    if clarified_task.clarified_task.trim().is_empty() || !clarified_task.open_questions.is_empty()
    {
        return None;
    }
    Some(clarified_task)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn query_rewriter_agent_compiles_as_pipeline_agent() {
        fn _assert_pipeline_agent<T: PipelineAgent>() {}
        _assert_pipeline_agent::<QueryRewriterAgent>();
    }

    use crate::magician_v2::slot_graph::{
        ClarifiedTask, RewriteModel, RewriteModelRequest, RewriteModelResponse, RewriterConfig,
    };
    use chrono::{DateTime, Duration};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingModel(AtomicUsize);

    #[async_trait::async_trait]
    impl RewriteModel for CountingModel {
        async fn generate(
            &self,
            request: RewriteModelRequest,
        ) -> anyhow::Result<RewriteModelResponse> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(RewriteModelResponse {
                completion: format!(
                    r#"{{"clarified_task": "rewritten by {}", "constraints": [], "objectives": [], "resources": [], "open_questions": [], "confidence": 0.9}}"#,
                    request.strategy
                ),
                telemetry: None,
            })
        }
    }

    fn put(
        store: &mut ArtifactStore,
        ty: ArtifactType,
        content: serde_json::Value,
        at: DateTime<Utc>,
    ) {
        store.put(AgentArtifact {
            artifact_id: Uuid::new_v4().to_string(),
            artifact_type: ty,
            producer_agent_id: "test".to_string(),
            producer_cycle_id: "c1".to_string(),
            content,
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: at,
            render_hints: None,
        });
    }

    fn elicited_task() -> ClarifiedTask {
        ClarifiedTask {
            clarified_task: "Summarize the weather in Pune".to_string(),
            confidence: 0.93,
            slot_graph_id: "graph-1".to_string(),
            original_message: "weather in pune".to_string(),
            ..ClarifiedTask::default()
        }
    }

    /// A fresh-turn store: SlotGraph, then a resolved ElicitationResult that
    /// already carries the rewrite, both inside the current run.
    fn fresh_turn() -> (ArtifactStore, PipelineContext) {
        let run_started_at = Utc::now() - Duration::seconds(30);
        let mut store = ArtifactStore::new("chain");
        put(
            &mut store,
            ArtifactType::SlotGraph,
            serde_json::json!([]),
            run_started_at + Duration::seconds(5),
        );
        put(
            &mut store,
            ArtifactType::ElicitationResult,
            serde_json::json!({
                "needs_clarification": false,
                "clarified_task": elicited_task(),
            }),
            run_started_at + Duration::seconds(10),
        );
        let context = PipelineContext {
            workflow_id: "wf".to_string(),
            query: "weather in pune".to_string(),
            run_started_at: Some(run_started_at),
            ..PipelineContext::default()
        };
        (store, context)
    }

    #[tokio::test]
    async fn fresh_turn_reuses_elicitation_rewrite_without_llm_calls() {
        let (mut store, context) = fresh_turn();
        let model = Arc::new(CountingModel(AtomicUsize::new(0)));
        let storage = crate::magician_v2::prompts::JsonPromptStorage::new(
            crate::magician_v2::prompts::json_storage::JsonStorageConfig::default(),
        )
        .expect("prompt storage");
        let rewriter = Arc::new(QuestionRewriter::new(
            Arc::clone(&model) as Arc<dyn RewriteModel>,
            Arc::new(crate::magician_v2::prompts::PromptManager::new(Arc::new(
                storage,
            ))),
            RewriterConfig::default(),
        ));
        let agent = QueryRewriterAgent::new(rewriter);

        let _result = agent.execute(&mut store, &context).await.expect("execute");

        assert_eq!(
            model.0.load(Ordering::SeqCst),
            0,
            "question_rewriting must not run twice in one planning turn"
        );
        let published: ClarifiedTask = store
            .latest_of_type(&ArtifactType::ClarifiedTask)
            .expect("ClarifiedTask published")
            .deserialize_content()
            .expect("ClarifiedTask content");
        assert_eq!(published, elicited_task());
    }

    #[test]
    fn reuse_requires_a_resolved_elicitation_from_this_run() {
        let (store, context) = fresh_turn();
        assert!(reusable_elicitation_rewrite(&store, &context).is_some());

        // No run boundary known → never reuse.
        let mut no_run = context.clone();
        no_run.run_started_at = None;
        assert!(reusable_elicitation_rewrite(&store, &no_run).is_none());

        // Elicitation from an earlier run.
        let mut later_run = context.clone();
        later_run.run_started_at = Some(Utc::now() + Duration::seconds(60));
        assert!(reusable_elicitation_rewrite(&store, &later_run).is_none());

        // Clarification still needed.
        let (mut pending, ctx) = fresh_turn();
        put(
            &mut pending,
            ArtifactType::ElicitationResult,
            serde_json::json!({ "needs_clarification": true, "clarified_task": elicited_task() }),
            Utc::now(),
        );
        assert!(reusable_elicitation_rewrite(&pending, &ctx).is_none());
    }

    #[test]
    fn reuse_is_skipped_when_this_stage_would_see_different_inputs() {
        // A slot graph newer than the elicitation (e.g. full replan).
        let (mut newer_graph, ctx) = fresh_turn();
        put(
            &mut newer_graph,
            ArtifactType::SlotGraph,
            serde_json::json!([]),
            Utc::now(),
        );
        assert!(reusable_elicitation_rewrite(&newer_graph, &ctx).is_none());

        // An interpreted answer (resume path merges answered slots).
        let (mut answered, ctx) = fresh_turn();
        put(
            &mut answered,
            ArtifactType::InterpretedAnswer,
            serde_json::json!({}),
            Utc::now(),
        );
        assert!(reusable_elicitation_rewrite(&answered, &ctx).is_none());

        // A prior ClarifiedTask (this stage rewrites from the prior text).
        let (mut prior, ctx) = fresh_turn();
        put(
            &mut prior,
            ArtifactType::ClarifiedTask,
            serde_json::to_value(elicited_task()).unwrap(),
            Utc::now(),
        );
        assert!(reusable_elicitation_rewrite(&prior, &ctx).is_none());
    }
}
