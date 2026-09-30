//! Flat-loop tool catalog (Phase 1 of the outer/inner loop flattening).
//!
//! Two tiers, mirroring Claude Code:
//! - **Hot:** ~8–10 tools with full JSON schemas surfaced eagerly.
//! - **Deferred:** bare names only; the LLM calls `tool_search` to fetch a
//!   schema on demand, after which it sticks for the rest of the run.
//!
//! Phase 1 builds the catalog + the server-side [`tool_index::ToolIndex`] and
//! wires `tool_search` to it. The live autonomous loop still consumes
//! `agentic::native_catalog::build_execution_native_catalog` until Phase 3
//! flips it behind a flat execution mode.
//!
//! Plan: `docs/plans/2026-05-29-flat-loop-phase1-implementation.md`.
//! Classification: `docs/plans/2026-05-28-hot-deferred-tool-classification.md`.

pub mod catalog;
pub mod dispatch;
pub mod surface_plan;
pub mod tool_index;
pub mod working_set;

pub use catalog::{
    build_eager_flat_loop_tools, build_flat_loop_tools, conditional_hot_tools,
    promote_surface_initial_hot_tools, DeferredEntry, FlatToolCatalog, ALWAYS_HOT_TOOL_NAMES,
};
pub use dispatch::{classify_flat_route, dispatch_flat_action, parse_flat_tool_name, FlatRoute};
pub use surface_plan::{
    autonomous_authority_revision, expand_direct_grants_to_leaves, provider_schema_bytes,
    static_prompt_context_revision, EffectiveSurfacePlan, StaticPromptKey, SurfacePlanCache,
    SurfacePlanCacheStatus, SurfacePlanKey, SurfacePlanParityReport,
};
pub use tool_index::{
    build_surface_tool_index, build_tool_index, promote_pack_to_leaves, ToolIndex, ToolIndexEntry,
};
pub use working_set::{
    project_family_selection, selected_tool_names_from_query, FamilyLoadError,
    FamilyLoadProjection, PreparedFamilyLoad, PreparedFamilyLoadCommitError, SurfaceWorkingSetKey,
    SurfaceWorkingSetSnapshot, SurfaceWorkingSetStore, SurfaceWorkingSetStoreStatus,
    ToolWorkingSet, WorkingSetLimits,
};
