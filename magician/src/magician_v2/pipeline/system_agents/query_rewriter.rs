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

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn query_rewriter_agent_compiles_as_pipeline_agent() {
        fn _assert_pipeline_agent<T: PipelineAgent>() {}
        _assert_pipeline_agent::<QueryRewriterAgent>();
    }
}
