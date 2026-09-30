use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;

use crate::magician_v2::{
    confidence::ConfidenceService,
    slot_graph::{elicitation::SlotGraphRepository, SlotRecord},
    state_tracker::{
        service::{StateTracker, TransitionContext},
        types::{
            BudgetState, ConfidenceScore, DeltaOperation, SlotDelta, StageContext, StateBundle,
            WorkflowState,
        },
    },
    storage::V2ConversationStore,
};

/// Slot graph repository backed by the V2 conversation store and state tracker.
///
/// Each slot record is persisted by appending a new planning snapshot so the
/// confidence history remains in sync with elicitation results.
pub struct ConversationSlotGraphRepository {
    store: Arc<dyn V2ConversationStore>,
    confidence_service: Arc<ConfidenceService>,
}

impl ConversationSlotGraphRepository {
    pub fn new(
        store: Arc<dyn V2ConversationStore>,
        confidence_service: Arc<ConfidenceService>,
    ) -> Self {
        Self {
            store,
            confidence_service,
        }
    }

    fn parse_workflow_id(&self, slot_id: &str) -> Result<String> {
        slot_id
            .split_once("::")
            .map(|(workflow_id, _)| workflow_id.to_string())
            .ok_or_else(|| anyhow!("slot id {slot_id} missing workflow prefix"))
    }
}

#[async_trait]
impl SlotGraphRepository for ConversationSlotGraphRepository {
    async fn create_slot(&self, slot: &SlotRecord) -> Result<()> {
        let workflow_id = self.parse_workflow_id(&slot.id)?;

        let tracker = StateTracker::with_confidence_service(
            self.store.clone(),
            self.confidence_service.clone(),
        );

        let mut resolved_slot = slot.clone();
        let slot_confidence = self
            .confidence_service
            .calculate_slot_confidence(&resolved_slot);
        resolved_slot.confidence = slot_confidence;

        let slot_delta = SlotDelta {
            slot_id: resolved_slot.id.clone(),
            operation: DeltaOperation::Create,
            previous_value: None,
            new_value: resolved_slot.value.clone(),
            stage: StageContext::PlanningBootstrap,
        };

        let resolved_slots = vec![resolved_slot.clone()];

        match tracker.latest_state(&workflow_id).await {
            Ok(Some(state)) => {
                tracker
                    .transition(
                        &workflow_id,
                        state.current_state,
                        state.current_state,
                        TransitionContext {
                            resolved_slots,
                            slot_deltas: vec![slot_delta],
                            stage_context: Some(StageContext::PlanningBootstrap),
                            ..TransitionContext::default()
                        },
                    )
                    .await
                    .with_context(|| {
                        format!("failed to append slot graph update for workflow {workflow_id}")
                    })?;
            },
            Ok(None) => {
                let summary = self
                    .confidence_service
                    .summarize_confidence(&resolved_slots);
                let timestamp = Utc::now();

                let mut confidence = ConfidenceScore::default();
                confidence.overall = summary.overall;
                confidence
                    .slot_records
                    .insert(resolved_slot.id.clone(), resolved_slot.clone());
                confidence
                    .per_slot
                    .insert(resolved_slot.id.clone(), resolved_slot.confidence);
                confidence
                    .history
                    .push((timestamp.timestamp_millis(), summary.overall));
                confidence.summary = Some(summary);

                let bundle = StateBundle {
                    state_id: uuid::Uuid::new_v4().to_string(),
                    workflow_id: workflow_id.clone(),
                    current_state: WorkflowState::Hypothesize,
                    llm_reasoning: None,
                    observations: Vec::new(),
                    slot_deltas: vec![slot_delta],
                    confidence,
                    budget: BudgetState::default(),
                    stage_context: StageContext::PlanningBootstrap,
                    completed_stages: Vec::new(),
                    failed_stage: None,
                    created_at: timestamp,
                };

                tracker.record_state(bundle).await.with_context(|| {
                    format!("failed to record initial slot graph state for {workflow_id}")
                })?;
            },
            Err(err) => {
                return Err(anyhow!(
                    "failed to load latest state for {}: {}",
                    workflow_id,
                    err
                ));
            },
        }

        Ok(())
    }
}
