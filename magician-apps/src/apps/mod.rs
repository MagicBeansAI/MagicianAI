pub mod app_directory;
pub mod app_publications;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod benchmark_fixtures;
pub mod custom_surface_review;
pub mod entity_changes;
pub mod entity_outbox;
pub mod entity_portability;
pub mod entity_retention;
pub mod fixtures;
pub mod installation_purge;
pub mod installation_review;
pub mod migration;
pub mod registry_lifecycle;
pub mod retention;
pub mod sandbox;
pub mod surface_assets;
pub mod surface_host;
pub mod surface_hydration;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod surface_qualification;
pub mod surface_runtime;
pub mod surface_scripted_host;
pub mod surface_worker;
pub mod threat_model;
pub mod tool_dispatch;
pub mod update;

// The split crate owns the route-facing and projection-oriented app services,
// while the authority, schema, registry, and execution kernels remain in the
// `magician` crate. Re-export those kernel modules here so extracted owners can
// keep their original sibling-module vocabulary without recreating a second
// contract namespace.
pub use magician::magician_v2::apps::{
    agent_capability, android_device, app_tool_bind, approval_boundary, authoring_catalog,
    authority, boundary, browser_capability, composition, contribution, entity_adapter,
    entity_mutation, entity_store, experience_capability, interactive, lifecycle, macos_host,
    manifest, memory_access, memory_access_store, memory_contribution_outbox, memory_store, models,
    os_jail, package_lock, package_staging, portability, portable_archive_transfer,
    primitive_catalog, query_semantics, records, registry, retrieval_contribution_outbox,
    schema_compiler, secret_access, surface_compiler, tool_catalog, value_mapping,
};
