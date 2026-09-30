//! # Pipeline Module
//!
//! Foundation for the agent pipeline — typed artifacts and an in-memory
//! artifact store that pipeline stages use to pass data between agents.

pub mod agent;
pub mod artifact;
pub mod definition;
pub mod orchestrator;
pub mod router;
pub mod schedule_utils;
pub mod system_agents;

pub use agent::{PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext};
pub use artifact::{AgentArtifact, ArtifactStore, ArtifactType};
pub use definition::{
    LoopConfig, PipelineDefinition, PipelineOverrides, RetryConfig, StageDefinition, StageOverride,
};
pub use orchestrator::{
    PipelineOrchestratorError, PipelineStatus, PipelineSuspension, PlanningOrchestrator,
    PlanningOutcome,
};
pub use router::{RouterAgent, RouterLLM, RoutingDecision, RoutingRule, TieredRouter};
pub use schedule_utils::next_fire;
pub use system_agents::PlannerBackend;
