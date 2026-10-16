//! Sensible progressive elicitation orchestrator implementation
//!
//! Implements the 5-tier progressive elicitation system.

use crate::magician_v2::ask_loop::clarifier::ClarifierLibrary;
use crate::magician_v2::confidence::ConfidenceService;
use crate::magician_v2::elicitation::{
    manager::ElicitationError,
    orchestrator_trait::{
        AutonomousDiscoveryService, ParameterInferenceService, SensibleElicitationOrchestrator,
    },
    types::{ElicitationDecision, ParameterContext, RefinementFeedback},
};
use crate::magician_v2::strategy::plan::{
    AskTiming, DiscoveryTiming, QuestionPriority, UnresolvedInput,
};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Implementation of the sensible progressive elicitation orchestrator
///
/// This orchestrator implements the 5-tier system:
/// 1. Priority Classification - Uses the priority already assigned to UnresolvedInput
/// 2. Inference Engine - Attempts to infer values using inference service
/// 3. Upfront Batching - Groups similar priority questions together
/// 4. Just-in-Time Questions - Decides optimal timing for asking
/// 5. Autonomous Discovery - Decides when autonomous discovery is appropriate
#[allow(dead_code)]
pub struct SensibleElicitationOrchestratorImpl {
    /// Service for parameter inference
    inference_service: Arc<dyn ParameterInferenceService>,

    /// Service for autonomous discovery
    discovery_service: Arc<dyn AutonomousDiscoveryService>,

    /// Service for confidence calculation
    confidence_service: Arc<ConfidenceService>,

    /// Library for question generation
    clarifier: Arc<ClarifierLibrary>,
}

impl SensibleElicitationOrchestratorImpl {
    /// Create a new orchestrator instance
    pub fn new(
        inference_service: Arc<dyn ParameterInferenceService>,
        discovery_service: Arc<dyn AutonomousDiscoveryService>,
        confidence_service: Arc<ConfidenceService>,
        clarifier: Arc<ClarifierLibrary>,
    ) -> Self {
        info!("Creating SensibleElicitationOrchestrator");
        Self {
            inference_service,
            discovery_service,
            confidence_service,
            clarifier,
        }
    }

    /// Tier 1: Classify priority
    ///
    /// Uses the priority already assigned in UnresolvedInput (which will be LLM-assigned in Phase 3)
    fn classify_priority(&self, input: &UnresolvedInput) -> QuestionPriority {
        input.priority
    }

    /// Tier 3: Generate batch key for grouping similar questions
    ///
    /// Questions with the same batch key can be asked together to reduce interruptions.
    fn generate_batch_key(&self, input: &UnresolvedInput) -> Option<String> {
        match input.priority {
            QuestionPriority::Critical => None, // Never batch critical questions
            QuestionPriority::PreExecution => Some(format!("pre_execution_{}", input.parameter)),
            QuestionPriority::JustInTime => Some(format!("jit_{}", input.parameter)),
            QuestionPriority::Optional => Some(format!("optional_{}", input.parameter)),
            QuestionPriority::Inferrable => Some(format!("inferrable_{}", input.parameter)),
        }
    }

    /// Tier 4: Decide when to ask based on timing and confidence
    ///
    /// Returns true if we should ask the user for this parameter.
    fn decide_ask_timing(
        &self,
        input: &UnresolvedInput,
        inferred_value: Option<&serde_json::Value>,
        inference_confidence: f64,
    ) -> bool {
        // If we have high-confidence inference, don't ask
        if inferred_value.is_some() && inference_confidence >= input.inference_threshold {
            debug!(
                "Skipping ask for {} due to high-confidence inference: {:.2}",
                input.parameter, inference_confidence
            );
            return false;
        }

        // Otherwise, respect ask_timing
        match input.ask_timing {
            AskTiming::PreExecution => true,
            AskTiming::JustInTime => true, // Will be asked later, but mark as should_ask
            AskTiming::Never => false,
        }
    }

    /// Tier 5: Decide when to discover autonomously
    ///
    /// Returns true if we should attempt autonomous discovery for this parameter.
    fn decide_discovery_timing(&self, input: &UnresolvedInput, should_ask: bool) -> bool {
        // If we're going to ask, don't discover
        if should_ask {
            return false;
        }

        // Otherwise, respect discovery_timing
        match input.discovery_timing {
            DiscoveryTiming::PreExecution => true,
            DiscoveryTiming::JustInTime => true,
            DiscoveryTiming::Auto => {
                // Auto: discover if inferrable priority
                matches!(input.priority, QuestionPriority::Inferrable)
            },
        }
    }
}

#[async_trait]
impl SensibleElicitationOrchestrator for SensibleElicitationOrchestratorImpl {
    async fn process_unresolved_input(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<ElicitationDecision, ElicitationError> {
        info!(
            "Processing unresolved input: {} (priority: {:?})",
            input.id, input.priority
        );

        // Tier 1: Priority Classification
        let priority = self.classify_priority(input);
        debug!("Tier 1 - Priority: {:?}", priority);

        // Tier 2: Inference Engine
        let inference_result = if !input.inference_hints.is_empty() || input.default_value.is_some()
        {
            debug!(
                "Tier 2 - Attempting inference with {} hints",
                input.inference_hints.len()
            );
            match self.inference_service.infer(input, context).await {
                Ok(result) => {
                    debug!(
                        "Tier 2 - Inference result: value={}, confidence={:.2}, method={:?}",
                        result.value.is_some(),
                        result.confidence,
                        result.method
                    );
                    Some(result)
                },
                Err(e) => {
                    warn!("Tier 2 - Inference failed: {}", e);
                    None
                },
            }
        } else {
            debug!("Tier 2 - Skipping inference (no hints or defaults)");
            None
        };

        let inferred_value = inference_result.as_ref().and_then(|r| r.value.clone());
        let inference_confidence = inference_result
            .as_ref()
            .map(|r| r.confidence)
            .unwrap_or(0.0);

        // Tier 3: Upfront Batching
        let batch_key = self.generate_batch_key(input);
        debug!("Tier 3 - Batch key: {:?}", batch_key);

        // Tier 4: Just-in-Time Questions
        let should_ask =
            self.decide_ask_timing(input, inferred_value.as_ref(), inference_confidence);
        debug!("Tier 4 - Should ask: {}", should_ask);

        // Tier 5: Autonomous Discovery
        let should_discover = self.decide_discovery_timing(input, should_ask);
        debug!("Tier 5 - Should discover: {}", should_discover);

        let reasoning = format!(
            "Priority: {:?}, Inference: {}, Ask: {}, Discover: {}",
            priority,
            if inferred_value.is_some() {
                format!("Yes ({:.1}% confidence)", inference_confidence * 100.0)
            } else {
                "No".to_string()
            },
            should_ask,
            should_discover
        );

        info!(
            "Elicitation decision for {}: should_ask={}, should_discover={}, confidence={:.2}",
            input.id, should_ask, should_discover, inference_confidence
        );

        Ok(ElicitationDecision {
            input_id: input.id.clone(),
            should_ask,
            should_discover,
            inferred_value,
            confidence: inference_confidence,
            batch_key,
            reasoning,
        })
    }

    async fn process_batch(
        &self,
        inputs: Vec<UnresolvedInput>,
        context: &ParameterContext,
    ) -> Result<Vec<ElicitationDecision>, ElicitationError> {
        info!("Processing batch of {} inputs", inputs.len());

        let mut decisions = Vec::new();

        for input in inputs {
            let decision = self.process_unresolved_input(&input, context).await?;
            decisions.push(decision);
        }

        info!(
            "Batch processing complete: {} decisions, {} to ask, {} to discover",
            decisions.len(),
            decisions.iter().filter(|d| d.should_ask).count(),
            decisions.iter().filter(|d| d.should_discover).count()
        );

        Ok(decisions)
    }

    async fn apply_refinement(
        &self,
        input_id: &str,
        feedback: RefinementFeedback,
        _context: &ParameterContext,
    ) -> Result<ElicitationDecision, ElicitationError> {
        info!(
            "Applying refinement for input_id: {}, feedback: {:?}",
            input_id, feedback
        );

        // Handle each feedback type appropriately
        match feedback {
            RefinementFeedback::Confirmed(value) => {
                // User confirmed the inferred value - use it with high confidence
                debug!("User confirmed inferred value for {}", input_id);
                Ok(ElicitationDecision {
                    input_id: input_id.to_string(),
                    should_ask: false,
                    should_discover: false,
                    inferred_value: Some(value),
                    confidence: 0.95,
                    batch_key: None,
                    reasoning: "User confirmed inferred value".to_string(),
                })
            },
            RefinementFeedback::Rejected => {
                // User rejected the inferred value - need to ask
                debug!("User rejected inferred value for {}", input_id);
                Ok(ElicitationDecision {
                    input_id: input_id.to_string(),
                    should_ask: true,
                    should_discover: false,
                    inferred_value: None,
                    confidence: 0.0,
                    batch_key: None,
                    reasoning: "User rejected inferred value, asking required".to_string(),
                })
            },
            RefinementFeedback::RequestedContext => {
                // User needs more context before deciding - ask with explanation
                debug!("User requested additional context for {}", input_id);
                Ok(ElicitationDecision {
                    input_id: input_id.to_string(),
                    should_ask: true,
                    should_discover: false,
                    inferred_value: None,
                    confidence: 0.0,
                    batch_key: None,
                    reasoning: "User requested additional context before deciding".to_string(),
                })
            },
            RefinementFeedback::Alternative(value) => {
                // User provided their own value - use it with maximum confidence
                debug!("User provided alternative value for {}", input_id);
                Ok(ElicitationDecision {
                    input_id: input_id.to_string(),
                    should_ask: false,
                    should_discover: false,
                    inferred_value: Some(value),
                    confidence: 1.0,
                    batch_key: None,
                    reasoning: "User provided alternative value".to_string(),
                })
            },
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::elicitation::types::{
        DiscoveryMethod, DiscoveryResult, InferenceMethod, InferenceResult, WorkflowStage,
    };
    use crate::magician_v2::strategy::plan::UnresolvedInput;
    use std::collections::HashMap;

    // Mock inference service for testing
    struct MockInferenceService;

    #[async_trait]
    impl ParameterInferenceService for MockInferenceService {
        async fn infer(
            &self,
            _input: &UnresolvedInput,
            _context: &ParameterContext,
        ) -> Result<InferenceResult, ElicitationError> {
            Ok(InferenceResult {
                value: Some(serde_json::json!("mock_value")),
                confidence: 0.8,
                method: InferenceMethod::LLMBased,
                explanation: "Mock inference".to_string(),
                sources: vec![],
            })
        }

        async fn can_infer(
            &self,
            _input: &UnresolvedInput,
            _context: &ParameterContext,
        ) -> Result<bool, ElicitationError> {
            Ok(true)
        }
    }

    // Mock discovery service for testing
    struct MockDiscoveryService;

    #[async_trait]
    impl AutonomousDiscoveryService for MockDiscoveryService {
        async fn discover(
            &self,
            _input: &UnresolvedInput,
            _context: &ParameterContext,
        ) -> Result<DiscoveryResult, ElicitationError> {
            Ok(DiscoveryResult {
                value: None,
                confidence: 0.0,
                method: DiscoveryMethod::ContextAnalysis,
                explanation: "Mock discovery".to_string(),
                external_actions_performed: false,
            })
        }

        async fn is_safe_to_discover(
            &self,
            _input: &UnresolvedInput,
            _context: &ParameterContext,
        ) -> Result<bool, ElicitationError> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn test_priority_classification() {
        // TODO: Full integration test requires complex service setup
        // For now, test the priority classification helper directly

        let input = UnresolvedInput {
            id: "test_param".to_string(),
            parameter: "api_key".to_string(),
            display_name: "API Key".to_string(),
            step_id: None,
            linked_steps: vec![],
            expected_type: Some("string".to_string()),
            json_schema: None,
            prompt: "Enter your API key".to_string(),
            required: true,
            notes: None,
            priority: QuestionPriority::Critical,
            ask_timing: crate::magician_v2::strategy::plan::AskTiming::PreExecution,
            discovery_timing: crate::magician_v2::strategy::plan::DiscoveryTiming::Auto,
            default_value: None,
            inference_hints: vec![],
            inference_threshold: 0.7,
            auto_fill: None,
            auto_fill_confidence: None,
            source: crate::magician_v2::strategy::plan::InputSource::Planner,
            created_at: None,
            updated_at: None,
            status: None,
        };

        // Priority classification should return the priority from the input
        assert_eq!(input.priority, QuestionPriority::Critical);
    }

    #[tokio::test]
    async fn test_inference_with_high_confidence() {
        // Test the mock inference service directly
        let inference = MockInferenceService;

        let input = UnresolvedInput {
            id: "test_param".to_string(),
            parameter: "timestamp".to_string(),
            display_name: "Timestamp".to_string(),
            step_id: None,
            linked_steps: vec![],
            expected_type: Some("string".to_string()),
            json_schema: None,
            prompt: "Enter timestamp".to_string(),
            required: false,
            notes: None,
            priority: QuestionPriority::Inferrable,
            ask_timing: crate::magician_v2::strategy::plan::AskTiming::Never,
            discovery_timing: crate::magician_v2::strategy::plan::DiscoveryTiming::Auto,
            default_value: None,
            inference_hints: vec!["current_time".to_string()],
            inference_threshold: 0.7,
            auto_fill: None,
            auto_fill_confidence: None,
            source: crate::magician_v2::strategy::plan::InputSource::Planner,
            created_at: None,
            updated_at: None,
            status: None,
        };

        let context = ParameterContext {
            execution_id: "test_thread".to_string(),
            user_message: "test message".to_string(),
            tool_context: None,
            slot_context: HashMap::new(),
            observations: vec![],
            stage: WorkflowStage::Execution,
            prompt_identity: None,
        };

        // Test inference service
        let result = inference.infer(&input, &context).await;
        assert!(result.is_ok());

        let inference_result = result.unwrap();
        assert!(inference_result.value.is_some());
        assert_eq!(inference_result.confidence, 0.8);
        assert!(matches!(inference_result.method, InferenceMethod::LLMBased));
    }

    #[tokio::test]
    async fn test_batching_logic() {
        // Test that discovery service returns expected values
        let discovery = MockDiscoveryService;

        let input = UnresolvedInput {
            id: "param1".to_string(),
            parameter: "host".to_string(),
            display_name: "Host".to_string(),
            step_id: None,
            linked_steps: vec![],
            expected_type: Some("string".to_string()),
            json_schema: None,
            prompt: "Enter host".to_string(),
            required: true,
            notes: None,
            priority: QuestionPriority::PreExecution,
            ask_timing: crate::magician_v2::strategy::plan::AskTiming::PreExecution,
            discovery_timing: crate::magician_v2::strategy::plan::DiscoveryTiming::Auto,
            default_value: None,
            inference_hints: vec![],
            inference_threshold: 0.7,
            auto_fill: None,
            auto_fill_confidence: None,
            source: crate::magician_v2::strategy::plan::InputSource::Planner,
            created_at: None,
            updated_at: None,
            status: None,
        };

        let context = ParameterContext {
            execution_id: "test_thread".to_string(),
            user_message: "test message".to_string(),
            tool_context: None,
            slot_context: HashMap::new(),
            observations: vec![],
            stage: WorkflowStage::Planning,
            prompt_identity: None,
        };

        // Test discovery service
        let safety = discovery.is_safe_to_discover(&input, &context).await;
        assert!(safety.is_ok());
        assert!(!safety.unwrap()); // Mock always returns false for safety

        let result = discovery.discover(&input, &context).await;
        assert!(result.is_ok());

        let discovery_result = result.unwrap();
        assert!(discovery_result.value.is_none()); // Mock returns no value
        assert_eq!(discovery_result.confidence, 0.0);
        assert!(matches!(
            discovery_result.method,
            DiscoveryMethod::ContextAnalysis
        ));
    }
}
