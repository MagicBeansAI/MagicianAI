//! Contract-first foundation for installable Magician apps.
//!
//! Phase 0 established the bounded transport and policy kernel. Phase 1 added
//! package admission, the scoped installation registry and inert control
//! adapters. Phase 2 owns schema, record, query, mutation, portability and
//! retention services plus authenticated owner/generic-agent adapters over the
//! same store. App execution and app UI remain gated.

pub mod agent_capability;
pub mod android_device;
pub mod android_owner;
pub mod android_owner_bootstrap;
pub mod app_discovery;
pub mod app_tool_bind;
pub mod approval_boundary;
pub mod artifact_selection;
pub mod authoring;
pub mod authoring_catalog;
pub mod authority;
pub mod background_behaviors;
pub mod behavior_recipe;
pub(crate) mod bound_http;
pub(crate) mod bound_path;
pub mod boundary;
pub mod browser_capability;
pub mod candidate_publication;
pub mod capability_catalog;
pub mod capability_publication;
pub mod component_contract;
pub mod composition;
pub mod composition_service;
pub mod contextual_round;
pub mod contextual_round_declaration;
pub mod contextual_round_program;
pub mod contribution;
pub(crate) mod contribution_frequency;
pub(crate) mod contribution_terminal;
mod control_encoding;
mod effect_deadline;
pub mod effect_kernel;
pub mod entity_adapter;
mod entity_index;
pub mod entity_mutation;
pub mod entity_store;
pub mod event_behaviors;
pub mod experience_capability;
pub(crate) mod governed_mcp;
mod indexed_snapshot;
pub mod interactive;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod leftover_evals;
pub mod lifecycle;
pub mod linked_text_rows;
pub mod llm_operations;
pub mod store_transaction;
// The behavior LLM lane is an activation-blocked internal scaffold. Keep its
// dispatcher and request pieces outside the public crate API until one sealed
// aggregate binds behavior/package/grant/model-context/execution identity.
pub(crate) mod llm_dispatch;
pub mod macos_host;
pub(crate) mod macos_pairing;
pub mod macos_pairing_service;
pub mod manifest;
pub mod memory;
pub mod memory_access;
pub mod memory_access_store;
pub mod memory_bridge;
pub mod memory_contribution_outbox;
pub mod memory_contribution_projection;
pub mod memory_proposal;
pub mod memory_store;
mod model_output_schema;
pub mod models;
pub mod observability;
pub mod os_jail;
pub mod os_jail_egress;
pub mod os_jail_in_place;
pub mod owner_notifications;
pub mod package_lock;
pub mod package_staging;
pub mod package_transfer;
pub mod personal_agent_retrieval_projection;
pub mod policy;
pub mod portability;
pub mod portable_archive_transfer;
pub mod primitive_catalog;
pub mod procedure_publication;
pub mod processing_boundary;
mod query_contract;
pub mod query_semantics;
pub mod recipe_ir;
pub(crate) mod recipe_lifecycle;
pub(crate) mod recipe_lowering;
pub mod reconciliation;
pub mod records;
pub mod registry;
#[cfg(test)]
pub mod release_qualification;
pub mod resource_authority;
pub mod resource_contract;
pub mod retrieval_contribution_outbox;
mod runtime_contract;
mod scheduled_input;
pub mod schema_compiler;
pub mod secret_access;
pub mod skill_dependencies;
pub mod slot_assignments;
pub mod surface_compiler;
pub mod system_boot_admission;
pub mod tool_catalog;
pub mod tool_disclosure;
pub mod tool_eligibility;
pub mod town_square_migration;
pub mod value_mapping;
pub mod vibedev_artifact_handoff;
pub mod widget_runtime;
pub mod workflows;
