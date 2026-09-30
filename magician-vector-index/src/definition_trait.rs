//! Definition-store-shaped trait surface for memory-index code.
//!
//! `memory_index` needs to enumerate agent definitions to (a) discover
//! per-agent tier definitions for candidate extraction and (b) hash the
//! definition payload to detect index staleness. This trait surfaces just
//! those two reads.

use async_trait::async_trait;

use crate::memory_tiers::MemoryTierDefinition;

/// Minimal projection of `DefinitionRecord` that the moved memory-index
/// code reads. The magician-side `AgentDefinitionStore` impl materializes
/// the full `DefinitionRecord`, then projects each entry into this struct.
///
/// `definition_source_hash` is computed from the stable source bytes backing the
/// definition. Hashing serialized Rust structs is not stable enough here because
/// definition fields may contain unordered maps.
#[derive(Debug, Clone)]
pub struct MoveableDefinitionRecord {
    pub agent_id: String,
    pub memory_tiers: Vec<MemoryTierDefinition>,
    pub definition_source_hash: String,
}

/// Object-safe definition-store trait that supplies `MoveableDefinitionRecord`s.
///
/// Errors are surfaced as `anyhow::Error` because the memory-index callers
/// already wrap them with `anyhow::Context`. magician's
/// `AgentDefinitionStore` impl wraps its `DefinitionStoreError` accordingly.
#[async_trait]
pub trait DefinitionLookup: Send + Sync {
    async fn list_moveable_definitions(
        &self,
    ) -> Result<Vec<MoveableDefinitionRecord>, anyhow::Error>;
}
