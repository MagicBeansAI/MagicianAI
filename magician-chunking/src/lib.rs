//! Logical-chunking domain adapters, extracted from the magician lib.
//!
//! `magicllm` owns the generic planning mechanics; the lib-side
//! `magician_v2::llm_chunking` owns the runner, registry and release
//! readiness. This crate owns the Magician domain adapters (memory and
//! hierarchical/consolidation) and the shadow evaluation harness.
//!
//! The built-in adapters are registered into the lib's global registry at
//! boot via [`register_builtin_chunk_adapters`] — the registry itself
//! initializes empty so the lib never depends on this crate.

pub mod hierarchical_adapters;
pub mod memory_adapters;
pub mod readiness;
pub mod shadow_eval;

pub use magician::magician_v2::llm_chunking::*;

/// Register every built-in domain adapter into the lib's global registry.
/// Idempotent: adapters already present are skipped. Call once at boot,
/// before router configs are loaded or validated.
pub fn register_builtin_chunk_adapters(
) -> Result<(), magician::magician_v2::llm_chunking::ChunkAdapterRegistryError> {
    use magician::magician_v2::llm_chunking::{
        global_chunk_adapter_registry, ChunkAdapterRegistryError,
    };
    let registry = global_chunk_adapter_registry();
    let mut guard = registry
        .write()
        .map_err(|_| ChunkAdapterRegistryError::RegistryPoisoned)?;
    let existing: Vec<String> = guard
        .inventory()
        .into_iter()
        .map(|e| e.adapter_id)
        .collect();
    if !existing.iter().any(|id| id == "memory_episode_quality_v1") {
        memory_adapters::register_builtin_memory_adapters(&mut guard)?;
    }
    if !existing.iter().any(|id| id == "evidence_distill_v1") {
        hierarchical_adapters::register_hierarchical_memory_adapters(&mut guard)?;
    }
    Ok(())
}
