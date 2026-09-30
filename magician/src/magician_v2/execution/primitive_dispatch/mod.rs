//! LLM-less primitive-dispatch infrastructure reused by the flat loop.
//!
//! Many tools (browser, gmail/csvkit/metabase_explore, etc.) expose granular
//! primitives. The nested per-tool LLM loop that once drove them was retired in
//! the flatten migration (flat-loop Phase 7); the flat per-action loop now
//! dispatches each leaf directly via [`dispatch::dispatch_primitive`].
//! What remains here is the shared, router-free primitive machinery: the
//! [`runner::PrimitiveDispatcher`] trait + [`runner::PrimitiveToolResult`], the
//! per-tool dispatcher submodules ([`browser`], [`cli_template`], provider-backed
//! compiled dispatch), and the shared exec-context / artifact / cleanup helpers.
//!
//! See `docs/archive/plans/2026-04-28-agent-browser-compiled-tool-migration.md`
//! and `docs/plans/2026-05-29-flat-loop-phase7-retire-inner-loop.md`.

pub mod artifacts;
pub mod browser;
pub mod capability_invoker;
pub mod cleanup;
pub mod cli_template;
pub mod compiled_provider;
pub mod dispatch;
pub mod exec_ctx;
mod governed_mcp;
mod governed_runtime;
pub(crate) use governed_runtime::NoProfileRegistry;
pub mod outcome;
pub mod runner;

pub use exec_ctx::{
    primitive_objective_id, primitive_objective_text, PrimitiveExecCtx, SeedGoalBlock,
};

pub use artifacts::PrimitiveArtifact;
pub(crate) use capability_invoker::fold_primitive_result;
pub use capability_invoker::{
    DeterministicCapabilityInvocation, DeterministicCapabilityInvocationResult,
    DeterministicCapabilityInvocationSource, DeterministicCapabilityInvoker,
    ScopedDeterministicCapabilityInvoker,
};
pub(crate) use governed_mcp::dispatch_governed_mcp;
pub(crate) use governed_runtime::admit_governed_route;
