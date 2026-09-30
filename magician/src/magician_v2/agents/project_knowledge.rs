//! The shared project-knowledge recall lane — the substrate `contribute_to_project` publishes recall
//! facts into and the citizen `magician_code_knowledge` read recalls from.
//!
//! Defined at this (low) `agents` level so all THREE consumers reference one source of truth without
//! an `agents → execution` import cycle:
//!   - `execution::compiled_handlers::contribute_to_project` (the WRITE),
//!   - `api::vibedev_api::citizen_code_knowledge_handler` (the READ), and
//!   - `agents::definition_store::list_moveable_definitions` (the memory-index REBUILD — it injects a
//!     synthetic record for this lane so its facts are embedded for semantic recall, exactly like a
//!     roster agent's lane).
//!
//! The lane is a FIXED synthetic agent id (NOT a real roster agent): the write materializes the
//! on-disk layout on demand (`save_native_tier` → `ensure_agent_layout`), and both the read and the
//! index rebuild supply the tier def directly, so no `AgentDefinition` is needed anywhere.

use std::collections::BTreeMap;

use super::memory_tiers::{
    MemoryTierDefinition, RenderConfig, RetentionMode, TierFieldSchema, TierScope,
};

/// Synthetic agent id owning the shared project-knowledge `code_knowledge` lane.
pub const PROJECT_KNOWLEDGE_AGENT: &str = "vibedev-project-knowledge";

/// Cap on the project-knowledge `facts` array (oldest dropped first, by `updated_at`).
pub const MAX_RECALL_FACTS: usize = 200;

/// Synthetic source hash for the lane's memory-index record. Bump when the tier shape below changes
/// so the index correctly invalidates + re-embeds.
pub const PROJECT_KNOWLEDGE_SOURCE_HASH: &str = "synthetic:project-knowledge:v1";

/// The agent-scope `code_knowledge` tier the project-knowledge lane uses — shared by the write, the
/// read, and the index rebuild. The name MUST contain `code_knowledge` so `infer_semantic_memory_type`
/// routes its items to `SemanticMemoryType::CodeKnowledge`; the single `facts` Collection field is what
/// the read flattens into per-fact candidates and what the index embeds.
pub fn code_knowledge_tier_def() -> MemoryTierDefinition {
    let mut schema: BTreeMap<String, TierFieldSchema> = BTreeMap::new();
    schema.insert(
        "facts".to_string(),
        TierFieldSchema::Collection {
            max_items: Some(MAX_RECALL_FACTS),
            item_schema: None,
        },
    );
    MemoryTierDefinition {
        name: "code_knowledge".to_string(),
        scope: TierScope::Agent,
        description: "Shared per-project knowledge contributed via contribute_to_project (PRDs, \
                      specs, design notes), recalled by the coding loop's magician_code_knowledge."
            .to_string(),
        schema,
        render: RenderConfig {
            format: "json".to_string(),
            template: String::new(),
        },
        retention: RetentionMode::Forever,
    }
}
