//! The crew CRUD surface seam (plan workstream 3.5,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! The Crew page's surface logic — the listing/projection/response shaping
//! the `/api/magician/v2/agents` CRUD family serves and the health
//! summarization that feeds `/agents/health` — registered as one
//! seam-registered module tree with a one-way dependency on the core agent
//! substrate. Not an app package: no stated packaging benefit, and packaging
//! would imply a Crew-UI rewrite.
//!
//! What moved here in 3.5, behavior-identical (decision extraction only —
//! every moved body is verbatim):
//! - [`projection`] — the crew record/list response shapes
//!   (`AgentDefinitionRecordResponse`, `AgentDefinitionListResponse`), the
//!   `SYSTEM_AGENT_DIRECTORY` block with the surface's system-agent filter
//!   policy, listing pagination policy, and the live-task runtime-status
//!   selection the hydrated projection is built from.
//! - [`health`] — the pure health summarization that maps a runtime status plus
//!   scoped tasks onto the `AgentHealthState` bands the read model scores
//!   (`crew_health_state`, `crew_health_task_needs_attention`).
//!
//! Compat shims: `web_api` re-exports every moved public name
//! (`pub use crate::crew_surface::...`), so `lib.rs`'s existing
//! `pub use web_api::{AgentDefinitionListResponse, ...}` and every in-crate
//! call site — including the pre-existing `web_api` test suites — compile
//! unedited. The wire shapes did not change.
//!
//! What did not move (Layer 1 stays Layer 1):
//! - The crew RUNTIME — `magician_v2::agents` (definitions store, approval,
//!   scheduler, wake-up queue, storage) — stays core; this seam only reads it
//!   through the same services the handlers already held.
//! - The durable health read model stays `crate::crew_health_api`
//!   (`CrewHealthService`: cache, LLM analytics rollups, daily history);
//!   re-exported below as [`read_model`] so the surface has one discoverable
//!   home while the module keeps its own file and tests.
//! - HTTP glue stays in `web_api`: routing, scope resolution, ETag/304
//!   handling, store error mapping, and the async hydration that fans into the
//!   runtime services (`hydrate_agent_runtime_state` +
//!   `build_crew_health_inputs` call THIS module's decisions; they never
//!   re-derive surface policy).

pub mod health;
pub mod projection;

pub use health::{crew_health_state, crew_health_task_needs_attention};
pub use projection::{
    apply_agent_list_pagination, is_system_agent_id, live_task_runtime_status,
    most_recent_live_task_for_agent, validate_agent_list_pagination, AgentDefinitionListResponse,
    AgentDefinitionRecordResponse, ListAgentDefinitionsQuery, SystemAgentDescriptor,
    AGENT_LIST_LIMIT_MAX, SYSTEM_AGENT_DIRECTORY,
};

pub use crate::crew_health_api as read_model;
