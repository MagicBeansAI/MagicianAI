//! Service traits for progressive elicitation orchestration
//!
//! Defines the interfaces for the sensible progressive elicitation system.

use crate::magician_v2::elicitation::manager::ElicitationError;
use crate::magician_v2::elicitation::types::{
    DiscoveryResult, ElicitationDecision, InferenceResult, ParameterContext, RefinementFeedback,
};
use crate::magician_v2::strategy::plan::UnresolvedInput;
use async_trait::async_trait;

/// Service for orchestrating sensible progressive elicitation
///
/// This is the main orchestrator that implements the 5-tier progressive elicitation system:
/// 1. Priority Classification - Classify parameter criticality
/// 2. Inference Engine - Attempt to infer values from context
/// 3. Upfront Batching - Group similar questions together
/// 4. Just-in-Time Questions - Ask questions at optimal timing
/// 5. Autonomous Discovery - Discover values independently when safe
#[async_trait]
pub trait SensibleElicitationOrchestrator: Send + Sync {
    /// Process an unresolved input through the 5-tier system
    ///
    /// # Arguments
    /// * `input` - The unresolved input to process
    /// * `context` - The parameter context (thread, user message, tool context, etc.)
    ///
    /// # Returns
    /// An `ElicitationDecision` indicating whether to ask, discover, or use inferred value
    async fn process_unresolved_input(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<ElicitationDecision, ElicitationError>;

    /// Process multiple inputs with batching
    ///
    /// # Arguments
    /// * `inputs` - List of unresolved inputs to process
    /// * `context` - The parameter context
    ///
    /// # Returns
    /// List of `ElicitationDecision` for each input
    async fn process_batch(
        &self,
        inputs: Vec<UnresolvedInput>,
        context: &ParameterContext,
    ) -> Result<Vec<ElicitationDecision>, ElicitationError>;

    /// Apply refinement feedback to a previous decision
    ///
    /// # Arguments
    /// * `input_id` - ID of the input to refine
    /// * `feedback` - User feedback on the inferred value
    /// * `context` - The parameter context
    ///
    /// # Returns
    /// Updated `ElicitationDecision` after applying refinement
    async fn apply_refinement(
        &self,
        input_id: &str,
        feedback: RefinementFeedback,
        context: &ParameterContext,
    ) -> Result<ElicitationDecision, ElicitationError>;
}

/// Service for inferring parameter values from context
///
/// Uses various methods (LLM, rules, history, defaults) to infer parameter values
/// without asking the user.
#[async_trait]
pub trait ParameterInferenceService: Send + Sync {
    /// Infer a parameter value from context
    ///
    /// # Arguments
    /// * `input` - The unresolved input to infer
    /// * `context` - The parameter context
    ///
    /// # Returns
    /// An `InferenceResult` containing the inferred value and confidence
    async fn infer(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<InferenceResult, ElicitationError>;

    /// Check if inference is likely to succeed
    ///
    /// # Arguments
    /// * `input` - The unresolved input to check
    /// * `context` - The parameter context
    ///
    /// # Returns
    /// `true` if inference is likely to succeed, `false` otherwise
    async fn can_infer(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<bool, ElicitationError>;
}

/// Service for autonomous parameter discovery
///
/// Performs autonomous actions (web search, filesystem search, API queries, etc.)
/// to discover parameter values when safe to do so.
#[async_trait]
pub trait AutonomousDiscoveryService: Send + Sync {
    /// Discover a parameter value autonomously
    ///
    /// # Arguments
    /// * `input` - The unresolved input to discover
    /// * `context` - The parameter context
    ///
    /// # Returns
    /// A `DiscoveryResult` containing the discovered value and confidence
    async fn discover(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<DiscoveryResult, ElicitationError>;

    /// Check if discovery is safe to attempt
    ///
    /// # Arguments
    /// * `input` - The unresolved input to check
    /// * `context` - The parameter context
    ///
    /// # Returns
    /// `true` if discovery is safe to attempt, `false` otherwise
    async fn is_safe_to_discover(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<bool, ElicitationError>;
}
