//! Provider-neutral context budgeting and structured planning primitives.
//!
//! Physical preflight and logical planning are provider-neutral. Domain
//! adapters decide which structured boundaries are safe; providers remain
//! responsible only for one physical request at a time.

mod adapter;
mod budget;
mod error;
mod estimator;
mod planner;
mod types;

pub use adapter::ChunkDomainAdapter;
pub use budget::{
    calculate_effective_payload, preflight_request, validate_logical_context, ContextPreflight,
    ContextPreflightMode,
};
pub use error::{ChunkError, ChunkValidationError, ContextError};
pub use estimator::{ConservativeOllamaEstimator, TokenEstimate, TokenEstimator};
pub use planner::{plan_logical_request, validate_plan_completeness};
pub use types::{
    ChunkBudget, ChunkDescriptor, ChunkPlan, FinalValidationContract, LogicalItem,
    LogicalItemIdentity, LogicalLlmRequest, ReductionContext, ReductionPlan, ReductionRequest,
    ReductionStrategy, ValidatedChunkOutput,
};
