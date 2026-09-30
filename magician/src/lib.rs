// Magician V2 Service - Standalone Orchestrator
//
// This crate contains the extracted Magician V2 orchestrator service
// that runs independently and uses runtime-core/tool-runtime-core
// services for registry and tool discovery.

// datafusion/sqlparser AST types are deeply recursive; proving Send/Sync for
// large `tokio::spawn` futures that transitively touch a `LogicalPlan` overflows
// the default auto-trait recursion limit (128). Raise it crate-wide.
#![recursion_limit = "256"]
// Test fixtures are not dead code, but the LIBRARY build cannot see that.
//
// `magician-api` and `magician-comms` list `magician = { features =
// ["test-fixtures"] }` in their dev-dependencies. Cargo unifies features across
// a `--workspace` build, so the feature switches on for magician's own plain
// lib target too — and every item behind
// `#[cfg(any(test, feature = "test-fixtures"))]` then compiles into a target
// whose callers live in OTHER crates' test binaries. rustc cannot see across
// that boundary, so it reports each one as never used: 55 warnings became 2631.
//
// The suppression is deliberately narrow. It applies only when the fixtures
// feature is on AND this is not magician's own test build — precisely the
// configuration where "unused" carries no information, because everything
// test-only is unused by construction. `cargo check -p magician --lib`, the
// default build, is untouched and still reports real dead code.
#![cfg_attr(
    all(not(test), feature = "test-fixtures"),
    allow(dead_code, unused_imports)
)]

pub mod config; // Configuration types
pub mod error; // Error types
pub mod magician_core; // Shared initialization builder
pub mod magician_v2;
pub mod runtime_plan; // Pre-bootstrap effective runtime plan (PR8) // V2 orchestrator logic

// Re-export main types
pub use config::{MagicianConfig, MagicianFrontendMode, MagicianFrontendSettings};
pub use error::{MagicianError, Result};
pub use magician_core::{MagicianService, MagicianServiceBuilder};
pub use runtime_plan::{
    resolve_boot_runtime_plan, resolve_runtime_plan, EffectiveRuntimePlan, LeftoverDispatchScalars,
    RuntimePlanError, RuntimeScaleOverrides, RuntimeScaleSettings, ScaleProfile,
};

// Re-export trait implementations from runtime_core (HTTP adapters)
pub use runtime_core::services::{SemanticSearch, ToolCatalog, ToolMatching, ToolServices};

// Re-export core types for external use
pub use runtime_core::{
    context::{ExecutionContext, UserPreferences},
    discovery::ToolDiscovery,
    prompts::{Prompt, PromptCategory, PromptMetadata, PromptStore, PromptVariable},
    runtime::RuntimeConfig,
    storage::{PaginatedResult, PaginationInfo, PaginationParams, V2ConversationStore},
    tooling::{
        MultipleToolMatchResult, ParameterDefinition, ParameterMapping, ParameterSource,
        ParameterValue, SemanticMatch, SuccessMetrics, ToolInfo, ToolMatch, ToolMatchResult,
        ToolMetadata,
    },
};
