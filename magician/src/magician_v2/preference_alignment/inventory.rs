//! The structurally eligible intention inventory for one owner/workspace.
//!
//! Combines every source adapter, merges their coverage (the weakest status
//! wins), and records the generations the inventory was read at. Eligibility
//! is structural only — owner scope, lifecycle, validity window. Nothing here
//! looks at the pending act: no topic, similarity or top-k filtering, because
//! an inventory cut by relevance could make a conflicting intention look
//! absent, and `nothing_to_compare` must only ever mean "there is nothing".
//!
//! The native knowledge and its intention generation are one consistent read:
//! the generation record is matched against the exact `knowledge.json` bytes
//! that are then projected (see [`super::generation`]). A generation that
//! cannot be read or repaired makes the inventory `unavailable`.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::magician_v2::agents::storage::AgentStorage;
use crate::magician_v2::agents::{AgentMemoryResolver, AgentMemoryService};
use crate::magician_v2::taste_capture::TasteCaptureService;
use crate::magician_v2::taste_profile::TasteProfileLoader;

use super::generation::{
    read_intention_generation_with_knowledge, reconcile_intention_generation, GenerationRead,
    IntentionGenerationError,
};
use super::intention::{Coverage, CoverageStatus, IntentionInventory, StandingGeneration};
use super::sources::{app_memory, native, taste_profile, SourceCollection};

/// Coverage name for the per-owner intention generation.
pub const INTENTION_GENERATION_SOURCE: &str = "intention_generation";

/// What to collect, and the bounds for collecting it.
#[derive(Clone, Copy)]
pub struct InventoryRequest<'a> {
    pub principal: &'a str,
    pub workspace: &'a str,
    /// The process taste profile loader; `None` means no profile can be in
    /// effect (tests, tools).
    pub taste_loader: Option<&'a TasteProfileLoader>,
    /// The capture service whose approvals prove a profile line's
    /// provenance; `None` leaves every line `unknown_provenance`.
    pub taste_capture: Option<&'a TasteCaptureService>,
    /// Bound on the taste profile read. An admission path passes its own;
    /// see `taste_profile::DEFAULT_TASTE_PROFILE_ADMISSION_TIMEOUT`.
    pub taste_timeout: Duration,
    /// Validity windows are evaluated at this instant.
    pub now: DateTime<Utc>,
}

/// Collect the inventory for `(principal, workspace)`.
pub async fn collect_inventory(
    resolver: &AgentMemoryResolver,
    request: InventoryRequest<'_>,
) -> IntentionInventory {
    match resolver.resolve_for_scope(request.principal, request.workspace) {
        Ok(memory) => collect_inventory_for_memory(&memory, request).await,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "preference alignment: owner memory could not be resolved"
            );
            assemble_inventory(
                vec![
                    SourceCollection::unavailable(native::NATIVE_SOURCE),
                    SourceCollection::unavailable(INTENTION_GENERATION_SOURCE),
                ],
                StandingGeneration::default(),
            )
        },
    }
}

/// Collect the inventory from an already-resolved owner memory service.
pub async fn collect_inventory_for_memory(
    memory: &AgentMemoryService,
    request: InventoryRequest<'_>,
) -> IntentionInventory {
    let native_future = native_with_generation(memory, request.now);
    let taste_future = taste_profile::load_taste_profile_intentions(
        request.taste_loader,
        request.taste_capture,
        request.principal,
        request.workspace,
        request.taste_timeout,
    );
    let ((native_parts, intention_generation), taste) = tokio::join!(native_future, taste_future);
    let (app, app_generation) = app_memory::app_memory_intentions();

    let mut parts = native_parts;
    parts.push(taste.collection);
    parts.push(app);
    assemble_inventory(
        parts,
        StandingGeneration {
            intention_generation,
            profile_version: taste.profile_version,
            app_generation,
        },
    )
}

/// The native intentions and the generation they were read at. On a
/// generation failure the intentions are still read (a shadow judge can use
/// them) but the generation source is reported unavailable, and the returned
/// generation is 0 and carries no meaning.
async fn native_with_generation(
    memory: &AgentMemoryService,
    now: DateTime<Utc>,
) -> (Vec<SourceCollection>, u64) {
    match consistent_generation_read(memory.storage()).await {
        Ok(read) => (
            vec![native::project_native_knowledge_bytes(
                read.knowledge_bytes.as_deref(),
                now,
            )],
            read.generation,
        ),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "preference alignment: intention generation unavailable"
            );
            (
                vec![
                    native::load_native_intentions(memory, now).await,
                    SourceCollection::unavailable(INTENTION_GENERATION_SOURCE),
                ],
                0,
            )
        },
    }
}

/// Read the generation with its knowledge; repair a stale or corrupt record
/// once (a lost bump must not leave alignment unavailable until the next
/// unrelated knowledge write).
async fn consistent_generation_read(
    storage: &AgentStorage,
) -> Result<GenerationRead, IntentionGenerationError> {
    match read_intention_generation_with_knowledge(storage).await {
        Ok(read) => Ok(read),
        Err(IntentionGenerationError::Stale { .. } | IntentionGenerationError::Corrupt(_)) => {
            reconcile_intention_generation(storage).await
        },
        Err(error) => Err(error),
    }
}

/// Merge source collections into one inventory. Coverage takes the weakest
/// status; an id that two sources both claim is kept once and the duplicate
/// is reported as omitted, so the judge never sees an ambiguous reference.
pub fn assemble_inventory(
    parts: Vec<SourceCollection>,
    generation: StandingGeneration,
) -> IntentionInventory {
    let mut coverage = Coverage::complete();
    let mut intentions = Vec::new();
    let mut seen = BTreeSet::new();
    let mut duplicates = Vec::new();
    for part in parts {
        coverage.merge(part.coverage);
        for intention in part.intentions {
            if seen.insert(intention.id.clone()) {
                intentions.push(intention);
            } else {
                duplicates.push(intention.id);
            }
        }
    }
    if !duplicates.is_empty() {
        coverage.merge(Coverage {
            status: CoverageStatus::Partial,
            omitted_ids: duplicates,
            unavailable_sources: Vec::new(),
        });
    }
    IntentionInventory {
        intentions,
        coverage,
        generation,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::magician_v2::preference_alignment::generation::{
        intention_generation_path, read_intention_generation,
    };
    use crate::magician_v2::preference_alignment::intention::AssertionStatus;

    fn request(now: DateTime<Utc>) -> InventoryRequest<'static> {
        InventoryRequest {
            principal: "owner",
            workspace: "default",
            taste_loader: None,
            taste_capture: None,
            taste_timeout: Duration::from_millis(200),
            now,
        }
    }

    #[test]
    fn verified_empty_only_when_every_source_is_complete() {
        let empty = assemble_inventory(
            vec![
                SourceCollection::empty_complete(),
                SourceCollection::empty_complete(),
            ],
            StandingGeneration::default(),
        );
        assert!(empty.is_verified_empty());

        let taste_down = assemble_inventory(
            vec![
                SourceCollection::empty_complete(),
                SourceCollection::unavailable(taste_profile::TASTE_PROFILE_SOURCE),
            ],
            StandingGeneration::default(),
        );
        assert!(taste_down.intentions.is_empty());
        assert!(!taste_down.is_verified_empty());
        assert_eq!(taste_down.coverage.status, CoverageStatus::Unavailable);
        assert_eq!(
            taste_down.coverage.unavailable_sources,
            vec![taste_profile::TASTE_PROFILE_SOURCE.to_string()]
        );

        let partial = assemble_inventory(
            vec![SourceCollection {
                intentions: Vec::new(),
                coverage: Coverage {
                    status: CoverageStatus::Partial,
                    omitted_ids: vec!["odd".to_string()],
                    unavailable_sources: Vec::new(),
                },
            }],
            StandingGeneration::default(),
        );
        assert!(!partial.is_verified_empty());
    }

    #[tokio::test]
    async fn an_owner_with_no_memory_and_no_profile_is_verified_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        let inventory = collect_inventory_for_memory(&memory, request(Utc::now())).await;
        assert!(inventory.is_verified_empty());
        assert_eq!(inventory.generation, StandingGeneration::default());
    }

    #[tokio::test]
    async fn the_inventory_carries_the_generation_its_knowledge_was_read_at() {
        let tmp = tempfile::tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        memory
            .save_user_knowledge(&json!({"preferences": [{
                "memory_record_id": "check-first",
                "key": "meat_orders",
                "value": "Always check with me before ordering meat for me",
                "source_type": "explicit_user_statement",
                "memory_lifecycle": "pending_review",
                "memory_evidence": [{
                    "id": "turn-1",
                    "source_role": "user",
                    "source_id": "chat-turn-1:user",
                    "quote": "Always check with me before ordering meat for me"
                }]
            }]}))
            .await
            .unwrap();
        let generation = read_intention_generation(memory.storage()).await.unwrap();

        let inventory = collect_inventory_for_memory(&memory, request(Utc::now())).await;
        assert_eq!(inventory.coverage.status, CoverageStatus::Complete);
        assert_eq!(inventory.generation.intention_generation, generation);
        assert_eq!(inventory.intentions.len(), 1);
        assert_eq!(
            inventory.intentions[0].assertion_status,
            AssertionStatus::DirectOwnerStatement
        );
        assert!(!inventory.is_verified_empty());
    }

    #[tokio::test]
    async fn a_stale_generation_is_repaired_before_it_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        memory
            .save_user_knowledge(&json!({"preferences": []}))
            .await
            .unwrap();
        let before = read_intention_generation(memory.storage()).await.unwrap();
        let storage = memory.storage();
        storage
            .write_json_atomic(
                storage.user_knowledge_path(),
                &json!({"preferences": [{"memory_record_id": "new", "key": "k", "value": "v"}]}),
            )
            .await
            .unwrap();

        let inventory = collect_inventory_for_memory(&memory, request(Utc::now())).await;
        assert_eq!(inventory.coverage.status, CoverageStatus::Complete);
        assert!(inventory.generation.intention_generation > before);
        assert_eq!(inventory.intentions.len(), 1);
    }

    #[tokio::test]
    async fn an_unreadable_generation_makes_the_inventory_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        // A directory where the record should be: reads fail with an error
        // that is not "not found", and nothing may report generation 0.
        std::fs::create_dir_all(intention_generation_path(memory.storage())).unwrap();
        memory
            .save_user_knowledge(&json!({"preferences": [{"key": "k", "value": "v"}]}))
            .await
            .unwrap();

        let inventory = collect_inventory_for_memory(&memory, request(Utc::now())).await;
        assert_eq!(inventory.coverage.status, CoverageStatus::Unavailable);
        assert_eq!(
            inventory.coverage.unavailable_sources,
            vec![INTENTION_GENERATION_SOURCE.to_string()]
        );
        assert!(!inventory.is_verified_empty());
        // The intentions are still read for a shadow judge.
        assert_eq!(inventory.intentions.len(), 1);
    }
}
