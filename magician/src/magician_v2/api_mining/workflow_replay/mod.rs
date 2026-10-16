//! Workflow-replay Phase 3: execute a compiled WorkflowGraph via direct HTTP.
//!
//! Walks the graph step by step. Each step resolves its parameters from one
//! of four sources (Literal / DataFlow / UserInput / SessionAuth), invokes
//! the existing single-capability replay path via `ApiRunner`, stores the
//! response for downstream JSONPath data-flow lookups, and continues. On
//! step failure or `browser_only` step, the API-only path returns a typed
//! `ReplayError`. The mixed path stops at a typed browser fallback request so
//! normal execution can continue on the visual rail with the captured browser
//! primitive.

pub mod engine;
pub mod jsonpath;
pub mod params;
pub mod skip;
pub mod step_executor;
pub mod types;

pub use engine::WorkflowReplayEngine;
pub use types::{
    BrowserFallbackRequest, MixedReplayResult, ReplayError, ReplayInputs, ReplayResult,
    StepReplayOutcome,
};
