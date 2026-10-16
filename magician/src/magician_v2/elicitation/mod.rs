//! Elicitation framework for Magician V2
//!
//! This module centralises slot/consent handling so the planner and executor
//! operate on the same state regardless of which strategy produced the plan.
//!
//! Also includes the progressive elicitation system for intelligent parameter resolution.

mod discovery;
mod inference;
mod manager;
mod models;
mod orchestrator_trait;
mod sensible_orchestrator;
mod types;

pub use manager::{ElicitationError, ElicitationManager, ElicitationManagerBuilder};
pub use models::{
    ObservationEvent, ObservationStatus, UnresolvedInput, UnresolvedInputUpdate, UpdateSource,
};

// Progressive elicitation types
pub use types::{
    classify_inputs,
    confidence_thresholds,
    // Phase 3: Question timing
    ClassifiedInputs,
    DiscoveryMethod,
    DiscoveryResult,
    ElicitationDecision,
    InferenceMethod,
    InferenceResult,
    InferenceSource,
    ObservationAsset,
    ParameterContext,
    PlanningContext,
    RefinementFeedback,
    ResolutionSource,
    ResolvedParameter,
    ToolMetadata,
    UserAnswer,
    WorkflowStage,
};

// Progressive elicitation traits
pub use orchestrator_trait::{
    AutonomousDiscoveryService, ParameterInferenceService, SensibleElicitationOrchestrator,
};

// Progressive elicitation implementations
pub use discovery::AutonomousDiscoveryServiceImpl;
pub use inference::ParameterInferenceServiceImpl;
pub use sensible_orchestrator::SensibleElicitationOrchestratorImpl;
