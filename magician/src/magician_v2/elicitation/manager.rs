use std::{collections::HashMap, sync::Arc, time::Duration};

use serde_json::{json, Map, Value};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::magician_v2::{
    realtime_events::RuntimeTransportBroadcaster,
    storage::{V2ConversationStore, V2Slot, V2SlotStatus},
};

use super::{
    models::{ObservationEvent, ObservationStatus, UnresolvedInput, UnresolvedInputUpdate},
    orchestrator_trait::{
        AutonomousDiscoveryService, ParameterInferenceService, SensibleElicitationOrchestrator,
    },
    types::{
        DiscoveryResult, ElicitationDecision, InferenceResult, ParameterContext, RefinementFeedback,
    },
};

const META_KEY: &str = "x-magician";
const META_SLOT_ID: &str = "slot_id";
const META_PROMPT: &str = "prompt";
const META_SOURCE: &str = "source";
const META_LINKED_STEPS: &str = "linked_steps";
const META_AUTO_FILL_CONFIDENCE: &str = "auto_fill_confidence";

#[derive(Debug, Error)]
pub enum ElicitationError {
    #[error("storage error: {0}")]
    Storage(#[from] crate::magician_v2::storage::V2StorageError),

    #[error("slot '{0}' not registered")]
    UnknownSlot(String),

    #[error("invalid metadata for slot '{0}': {1}")]
    InvalidMetadata(String, String),

    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("invalid format: {0}")]
    InvalidFormat(String),

    #[error("LLM service error: {0}")]
    LlmServiceError(String),
}

/// Builder responsible for creating an `ElicitationManager`.
pub struct ElicitationManagerBuilder {
    store: Arc<dyn V2ConversationStore>,
    execution_id: String,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
}

impl ElicitationManagerBuilder {
    pub fn new(store: Arc<dyn V2ConversationStore>, execution_id: impl Into<String>) -> Self {
        Self {
            store,
            execution_id: execution_id.into(),
            event_broadcaster: None,
        }
    }

    pub fn with_event_broadcaster(
        mut self,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    ) -> Self {
        self.event_broadcaster = broadcaster;
        self
    }

    pub async fn build(self) -> Result<ElicitationManager, ElicitationError> {
        ElicitationManager::initialise(self.store, self.execution_id, self.event_broadcaster).await
    }
}

/// Centralised slot/consent manager bridging planner and executor.
pub struct ElicitationManager {
    store: Arc<dyn V2ConversationStore>,
    execution_id: String,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    slot_mapping: Arc<RwLock<HashMap<String, String>>>, // plan_slot_id -> store_slot_id

    // Progressive elicitation services (optional for backward compatibility)
    sensible_orchestrator: Option<Arc<dyn SensibleElicitationOrchestrator>>,
    parameter_inference: Option<Arc<dyn ParameterInferenceService>>,
    autonomous_discovery: Option<Arc<dyn AutonomousDiscoveryService>>,
}

impl ElicitationManager {
    async fn initialise(
        store: Arc<dyn V2ConversationStore>,
        execution_id: String,
        event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    ) -> Result<Self, ElicitationError> {
        let manager = Self {
            store: store.clone(),
            execution_id,
            event_broadcaster,
            slot_mapping: Arc::new(RwLock::new(HashMap::new())),
            // Progressive elicitation services default to None
            sensible_orchestrator: None,
            parameter_inference: None,
            autonomous_discovery: None,
        };

        manager.refresh_mapping().await?;
        Ok(manager)
    }

    async fn refresh_mapping(&self) -> Result<(), ElicitationError> {
        let slots = self.store.get_slots(&self.execution_id).await?;
        let mut map = self.slot_mapping.write().await;
        map.clear();
        for slot in slots {
            if let Some(plan_id) = Self::extract_plan_slot_id(&slot.schema_json) {
                map.insert(plan_id, slot.id);
            }
        }
        Ok(())
    }

    fn extract_plan_slot_id(schema: &Value) -> Option<String> {
        schema
            .as_object()
            .and_then(|obj| obj.get(META_KEY))
            .and_then(|meta| meta.get(META_SLOT_ID))
            .and_then(Value::as_str)
            .map(|s| s.to_string())
    }

    fn embed_metadata(
        registration: &UnresolvedInput,
        mut schema: Value,
    ) -> Result<Value, ElicitationError> {
        if !schema.is_object() {
            schema = json!({ "type": "string" });
        }

        let obj = schema.as_object_mut().ok_or_else(|| {
            ElicitationError::InvalidMetadata(
                registration.id.clone(),
                "Schema is not a valid JSON object".to_string(),
            )
        })?;

        let meta_entry = obj
            .entry(META_KEY.to_string())
            .or_insert_with(|| Value::Object(Map::<String, Value>::new()));

        let meta = meta_entry.as_object_mut().ok_or_else(|| {
            ElicitationError::InvalidMetadata(
                registration.id.clone(),
                "Metadata entry is not a valid JSON object".to_string(),
            )
        })?;

        meta.insert(
            META_SLOT_ID.to_string(),
            Value::String(registration.id.clone()),
        );
        meta.insert(
            META_PROMPT.to_string(),
            Value::String(registration.prompt.clone()),
        );

        // Convert InputSource to string for storage
        let source_str = match registration.source {
            crate::magician_v2::strategy::plan::InputSource::Planner => "planner",
            crate::magician_v2::strategy::plan::InputSource::Validator => "validator",
            crate::magician_v2::strategy::plan::InputSource::QueryAnalysis => "query_analysis",
            crate::magician_v2::strategy::plan::InputSource::Inference => "inference",
            crate::magician_v2::strategy::plan::InputSource::Discovery => "discovery",
            crate::magician_v2::strategy::plan::InputSource::UserReply => "user",
            crate::magician_v2::strategy::plan::InputSource::AutoFill => "auto_fill",
        };
        meta.insert(
            META_SOURCE.to_string(),
            Value::String(source_str.to_string()),
        );
        meta.insert(
            META_LINKED_STEPS.to_string(),
            Value::Array(
                registration
                    .linked_steps
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        );

        if let Some(confidence) = registration.auto_fill_confidence {
            if let Some(value) = serde_json::Number::from_f64(confidence as f64) {
                meta.insert(META_AUTO_FILL_CONFIDENCE.to_string(), Value::Number(value));
            }
        }

        Ok(schema)
    }

    async fn resolve_store_slot_id(&self, plan_slot_id: &str) -> Result<String, ElicitationError> {
        if let Some(id) = self.slot_mapping.read().await.get(plan_slot_id).cloned() {
            return Ok(id);
        }

        self.refresh_mapping().await?;
        if let Some(id) = self.slot_mapping.read().await.get(plan_slot_id).cloned() {
            Ok(id)
        } else {
            Err(ElicitationError::UnknownSlot(plan_slot_id.to_string()))
        }
    }

    /// Register or update slots for the current thread.
    pub async fn register_slots(
        &self,
        registrations: Vec<UnresolvedInput>,
    ) -> Result<Vec<String>, ElicitationError> {
        let mut created_ids = Vec::new();

        for reg in registrations {
            let store_id = {
                let map = self.slot_mapping.read().await;
                map.get(&reg.id).cloned()
            };

            if let Some(store_id) = store_id {
                debug!(
                    "[MAGICIAN-V2-ELICITATION] Slot '{}' already registered -> {}",
                    reg.id, store_id
                );

                if let Some(value) = reg.auto_fill.clone() {
                    self.store
                        .update_slot_answer(&self.execution_id, &store_id, value)
                        .await?;
                    self.store
                        .update_slot_status(&self.execution_id, &store_id, V2SlotStatus::Answered)
                        .await?;
                }
                created_ids.push(store_id);
                continue;
            }

            // Use json_schema if available, otherwise default to empty object
            let schema_value = reg.json_schema.clone().unwrap_or_else(|| json!({}));
            let schema = Self::embed_metadata(&reg, schema_value)?;
            let slot = self
                .store
                .create_slot(
                    &self.execution_id,
                    reg.display_name.clone(),
                    schema,
                    reg.required,
                    None,
                )
                .await?;

            {
                let mut map = self.slot_mapping.write().await;
                map.insert(reg.id.clone(), slot.id.clone());
            }

            if let Some(value) = reg.auto_fill.clone() {
                self.store
                    .update_slot_answer(&self.execution_id, &slot.id, value)
                    .await?;
                self.store
                    .update_slot_status(&self.execution_id, &slot.id, V2SlotStatus::Answered)
                    .await?;
            }

            created_ids.push(slot.id);
        }

        Ok(created_ids)
    }

    /// Manually update an unresolved input (e.g., user provided an answer).
    pub async fn update_slot(&self, update: UnresolvedInputUpdate) -> Result<(), ElicitationError> {
        let store_id = self.resolve_store_slot_id(&update.plan_slot_id).await?;

        if let Some(answer) = update.answer {
            self.store
                .update_slot_answer(&self.execution_id, &store_id, answer)
                .await?;
        }

        if let Some(status) = update.status {
            self.store
                .update_slot_status(&self.execution_id, &store_id, status)
                .await?;
        }

        if let Some(_broadcaster) = &self.event_broadcaster {
            info!(
                "[MAGICIAN-V2-ELICITATION] Slot '{}' updated by {:?}",
                update.plan_slot_id, update.source
            );
        }

        Ok(())
    }

    /// Return all pending slots for this thread.
    pub async fn pending_slots(&self) -> Result<Vec<V2Slot>, ElicitationError> {
        Ok(self.store.get_pending_slots(&self.execution_id).await?)
    }

    /// Wait until the given slot transitions to Answered or Skipped.
    pub async fn await_slot(
        &self,
        plan_slot_id: &str,
        poll_interval: Duration,
    ) -> Result<V2Slot, ElicitationError> {
        let store_id = self.resolve_store_slot_id(plan_slot_id).await?;
        loop {
            let slot = self.store.get_slot(&self.execution_id, &store_id).await?;
            match slot.status {
                V2SlotStatus::Pending => tokio::time::sleep(poll_interval).await,
                V2SlotStatus::Answered | V2SlotStatus::Skipped => return Ok(slot),
            }
        }
    }

    /// Record post-execution observation checkpoints.
    pub async fn record_observation(
        &self,
        event: ObservationEvent,
    ) -> Result<(), ElicitationError> {
        match event.status {
            ObservationStatus::Satisfied => info!(
                "[MAGICIAN-V2-ELICITATION] Observation satisfied for step {}",
                event.step_id
            ),
            ObservationStatus::RetryRecommended => warn!(
                "[MAGICIAN-V2-ELICITATION] Observation retry suggested for step {}: {:?}",
                event.step_id, event.notes
            ),
            ObservationStatus::Escalate => warn!(
                "[MAGICIAN-V2-ELICITATION] Observation escalation required for step {}: {:?}",
                event.step_id, event.notes
            ),
        }
        Ok(())
    }

    pub fn thread_id(&self) -> &str {
        &self.execution_id
    }

    /// Builder method to set progressive elicitation services
    pub fn with_progressive_elicitation(
        mut self,
        sensible_orchestrator: Arc<dyn SensibleElicitationOrchestrator>,
        parameter_inference: Arc<dyn ParameterInferenceService>,
        autonomous_discovery: Arc<dyn AutonomousDiscoveryService>,
    ) -> Self {
        self.sensible_orchestrator = Some(sensible_orchestrator);
        self.parameter_inference = Some(parameter_inference);
        self.autonomous_discovery = Some(autonomous_discovery);
        self
    }

    // ========== Progressive Elicitation Methods ==========

    /// Process an unresolved input through the 5-tier progressive elicitation system
    ///
    /// Delegates to the sensible orchestrator if available, otherwise returns an error.
    pub async fn process_unresolved_input(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<ElicitationDecision, ElicitationError> {
        let orchestrator = self.sensible_orchestrator.as_ref().ok_or_else(|| {
            ElicitationError::NotImplemented(
                "Progressive elicitation not configured - sensible orchestrator not available"
                    .to_string(),
            )
        })?;

        orchestrator.process_unresolved_input(input, context).await
    }

    /// Process multiple inputs with batching
    ///
    /// Delegates to the sensible orchestrator if available, otherwise returns an error.
    pub async fn process_batch(
        &self,
        inputs: Vec<UnresolvedInput>,
        context: &ParameterContext,
    ) -> Result<Vec<ElicitationDecision>, ElicitationError> {
        let orchestrator = self.sensible_orchestrator.as_ref().ok_or_else(|| {
            ElicitationError::NotImplemented(
                "Progressive elicitation not configured - sensible orchestrator not available"
                    .to_string(),
            )
        })?;

        orchestrator.process_batch(inputs, context).await
    }

    /// Apply refinement feedback to a previous decision
    ///
    /// Delegates to the sensible orchestrator if available, otherwise returns an error.
    pub async fn apply_refinement(
        &self,
        input_id: &str,
        feedback: RefinementFeedback,
        context: &ParameterContext,
    ) -> Result<ElicitationDecision, ElicitationError> {
        let orchestrator = self.sensible_orchestrator.as_ref().ok_or_else(|| {
            ElicitationError::NotImplemented(
                "Progressive elicitation not configured - sensible orchestrator not available"
                    .to_string(),
            )
        })?;

        orchestrator
            .apply_refinement(input_id, feedback, context)
            .await
    }

    /// Attempt to infer a parameter value from context
    ///
    /// Delegates to the parameter inference service if available, otherwise returns an error.
    pub async fn try_inference(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<InferenceResult, ElicitationError> {
        let inference = self
            .parameter_inference
            .as_ref()
            .ok_or_else(|| ElicitationError::NotImplemented(
                "Progressive elicitation not configured - parameter inference service not available".to_string()
            ))?;

        inference.infer(input, context).await
    }

    /// Check if inference is likely to succeed for a parameter
    ///
    /// Delegates to the parameter inference service if available, otherwise returns false.
    pub async fn can_infer(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<bool, ElicitationError> {
        let inference = self
            .parameter_inference
            .as_ref()
            .ok_or_else(|| ElicitationError::NotImplemented(
                "Progressive elicitation not configured - parameter inference service not available".to_string()
            ))?;

        inference.can_infer(input, context).await
    }

    /// Attempt to discover a parameter value autonomously
    ///
    /// Delegates to the autonomous discovery service if available, otherwise returns an error.
    pub async fn try_discovery(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<DiscoveryResult, ElicitationError> {
        let discovery = self
            .autonomous_discovery
            .as_ref()
            .ok_or_else(|| ElicitationError::NotImplemented(
                "Progressive elicitation not configured - autonomous discovery service not available".to_string()
            ))?;

        discovery.discover(input, context).await
    }

    /// Check if autonomous discovery is safe for a parameter
    ///
    /// Delegates to the autonomous discovery service if available, otherwise returns false.
    pub async fn is_safe_to_discover(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<bool, ElicitationError> {
        let discovery = self
            .autonomous_discovery
            .as_ref()
            .ok_or_else(|| ElicitationError::NotImplemented(
                "Progressive elicitation not configured - autonomous discovery service not available".to_string()
            ))?;

        discovery.is_safe_to_discover(input, context).await
    }

    /// Get the parameter inference service (if configured)
    ///
    /// Returns the Arc-wrapped inference service for use by strategies that need
    /// direct access to parameter inference capabilities.
    pub fn get_inference_service(&self) -> Option<Arc<dyn ParameterInferenceService>> {
        self.parameter_inference.clone()
    }

    /// Get the autonomous discovery service (if configured)
    ///
    /// Returns the Arc-wrapped discovery service for use by strategies that need
    /// direct access to autonomous discovery capabilities.
    pub fn get_discovery_service(&self) -> Option<Arc<dyn AutonomousDiscoveryService>> {
        self.autonomous_discovery.clone()
    }
}
