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

/// Await `$e` off the caller's poll frame.
///
/// A future built inline is a stack temporary of its full state size in the
/// awaiting function's poll frame, and a debug build gives every temporary
/// its own slot: a 22-arm match held all 22 futures at once (426 KiB), and a
/// 1,616-line resume function reserved 1 MiB of a 2 MiB worker (2026-10-12
/// crashes). Here the caller holds only a closure of borrows and the box
/// pointer: the `async` block — as large as the callee's future, since it
/// holds it — is built and boxed in [`box_off_frame`]'s own frame, which is
/// gone before the block is polled. Boxed in the caller instead
/// (`Box::pin(async { $e })`, or `.heap_boxed()`), it took a slot of its full
/// size there per site: 36 KiB, 31 KiB, … in one run loop (2026-11-03).
///
/// `$e` must be a plain awaited expression: `?` and `return` inside it apply
/// to the block, so put `?` outside — `off_frame!(self.load().await)?`.
#[macro_export]
macro_rules! off_frame {
    ($e:expr) => {
        $crate::box_off_frame(|| async { $e }).await
    };
}

/// [`off_frame!`]'s boxing step, kept out of line so the future it builds
/// never takes a slot in the awaiting function's frame.
#[doc(hidden)]
#[inline(never)]
pub fn box_off_frame<F, Fut>(build: F) -> std::pin::Pin<Box<Fut>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future,
{
    Box::pin(build())
}

/// The Magician core version (this crate's) — what people mean by "the
/// Magician backend". `magician-api` and `magician-bin` carry their own crate
/// versions; `GET /health` reports this one as `version` and the rest under
/// `versions`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
