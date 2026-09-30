//! API Mining Layer
//!
//! Automatically learns and reverse-engineers REST/GraphQL APIs by observing
//! network traffic during browser automation sessions. Once learned, these APIs
//! can be replayed directly for 75x cost reduction.
//!
//! ## Architecture
//!
//! ```text
//! Browser Automation → CDP Trace Collector → TraceManager → TraceStorage (JSONL)
//!     → ApiMiner (clustering + parameterization) → CapabilityStore (per-origin JSON)
//!     → CapabilityRegistry (global index) → ApiRunner (replay with verification)
//! ```
//!
//! ## Phases
//!
//! - Phase 1: Trace capture pipeline (complete)
//! - Phase 2: Mining + capability generation (complete)
//! - Phase 3: Action-to-network correlation (complete)
//! - Phase 4: Replay engine (complete)
//! - Phase 5: Router integration + decision layer (complete)
//! - Phase 6: Deployment hardening — storage cleanup and perf logging (complete)

pub mod action_binding;
pub mod approval;
pub mod auth_capture;
pub mod auth_refresh;
pub mod auto_replay;
pub mod body_template;
pub mod capability;
pub mod capability_store;
pub mod correlator;
pub mod maintenance;
pub mod metrics;
pub mod miner;
pub mod noise_filter;
pub mod openapi_generator;
pub mod origin_policy;
pub mod passive_validation;
pub mod path_safe;
pub mod projection;
pub mod projection_pipeline;
pub mod recipe;
pub mod recipe_compiler;
pub mod recipe_feedback;
pub mod recipe_matcher;
pub mod recipe_observer;
pub mod recipe_packs;
pub mod recipe_runner;
pub mod recipe_runs;
pub mod recipe_store;
pub mod recipe_verification;
pub mod registry;
pub mod relevance;
pub mod replay;
pub mod replay_grants;
pub mod router;
pub mod sequence;
pub mod sequence_recorder;
pub mod sequence_store;
pub mod skill_emitter;
pub mod switch;
pub mod trace_buffer;
pub mod trace_manager;
pub mod trace_storage;
pub mod types;
pub mod workflow;
pub mod workflow_compiler;
pub mod workflow_replay;
pub mod workflow_store;

#[cfg(any(test, feature = "test-fixtures"))]
mod fixture_harness;

pub use metrics::{
    passive_validation_metrics_for_scope, passive_validation_snapshot_for_scope,
    projection_metrics_for_scope, projection_snapshot_for_scope, recipe_metrics_for_scope,
    recipe_snapshot_for_scope, replay_metrics_for_scope, replay_snapshot_for_scope,
    router_metrics_for_scope, router_snapshot_for_scope, sequence_metrics_for_scope,
    sequence_snapshot_for_scope, workflow_metrics_for_scope, workflow_snapshot_for_scope,
    PassiveValidationMetrics, PassiveValidationMetricsSnapshot, ProjectionEvent, ProjectionMetrics,
    ProjectionMetricsSnapshot, RecipeMetrics, RecipeMetricsSnapshot, ReplayMetrics,
    ReplayMetricsSnapshot, RouterMetrics, RouterMetricsSnapshot, RouterOutcome, SequenceMetrics,
    SequenceMetricsSnapshot, WorkflowMetrics, WorkflowMetricsSnapshot,
};
