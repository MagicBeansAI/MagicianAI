//! The durable per-owner intention generation (P0 contracts §6.3).
//!
//! A captured alignment assessment is only valid against the owner knowledge
//! it read. This module keeps one small typed record beside
//! `memory/users/knowledge.json` — `intention_generation.json` — and moves it
//! forward on every knowledge write and on every `forget_memory` deletion. An
//! admission point compares the generation it captured with the current one
//! for equality, so any change in between forces reassessment.
//!
//! # Why the record carries a fingerprint
//!
//! The bump runs after `knowledge.json` is written, and a failed bump must not
//! fail the knowledge write: the owner's memory matters more than an
//! alignment cache. So a bump can be lost. The record therefore also stores a
//! BLAKE3 fingerprint of the exact `knowledge.json` bytes it was recorded
//! against, and the reader compares it with the bytes on disk. A mismatch
//! means knowledge changed without a bump, and the reader reports
//! [`IntentionGenerationError::Stale`] instead of a number that no longer
//! describes the store. It never answers 0 for a record it cannot trust.
//!
//! BLAKE3 because the crate already uses it for every memory digest
//! (`memory_lifecycle::digest`, the taste profile `version`), and because a
//! fingerprint over bytes catches writers that bypass the bump as well as
//! lost bumps.
//!
//! # Why the generation is a clock, not a count
//!
//! The next generation is `max(previous + 1, now in microseconds)`. Equality
//! is the only comparison anyone makes, so the value only has to be
//! *never reused*. A plain counter is reused when its record is lost —
//! deleted, corrupted, or restored from an older backup — because it restarts
//! from 1 and can climb back to a value an old assessment captured. Seeding
//! from the wall clock makes a lost record harmless: the next value is past
//! anything handed out before, and `previous + 1` keeps it strictly increasing
//! if the clock steps backwards.
//!
//! # Locking
//!
//! Writers run under the existing exclusive `knowledge.json` flock, so the
//! knowledge write and its bump are one critical section. Readers take the
//! shared flock so the record and the bytes they fingerprint are one
//! consistent pair. Never call a reader while holding the exclusive flock on
//! the same path: the flock is not re-entrant.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::storage::{AgentStorage, AgentStorageError};

pub const INTENTION_GENERATION_FILE: &str = "intention_generation.json";
pub const INTENTION_GENERATION_SCHEMA_VERSION: &str = "intention_generation.v1";

/// Fingerprint recorded when `knowledge.json` does not exist.
pub const ABSENT_KNOWLEDGE_FINGERPRINT: &str = "absent";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentionGenerationRecord {
    pub schema_version: String,
    pub generation: u64,
    /// `blake3:<hex>` of the `knowledge.json` bytes this generation describes,
    /// or [`ABSENT_KNOWLEDGE_FINGERPRINT`].
    pub knowledge_fingerprint: String,
    pub bumped_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum IntentionGenerationError {
    #[error("intention generation record could not be read: {0}")]
    Unreadable(String),
    #[error("intention generation record is corrupt: {0}")]
    Corrupt(String),
    #[error("user knowledge could not be read for its fingerprint: {0}")]
    KnowledgeUnreadable(String),
    #[error("intention generation {generation} is stale: user knowledge changed without a bump")]
    Stale { generation: u64 },
    #[error("intention generation could not be written: {0}")]
    Write(String),
    #[error("user knowledge lock failed: {0}")]
    Lock(String),
}

/// One consistent read: the generation and the exact knowledge bytes it
/// describes. `knowledge_bytes` is `None` when `knowledge.json` does not
/// exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRead {
    pub generation: u64,
    pub knowledge_bytes: Option<Vec<u8>>,
}

pub fn intention_generation_path(storage: &AgentStorage) -> PathBuf {
    storage.user_root().join(INTENTION_GENERATION_FILE)
}

pub fn knowledge_fingerprint(bytes: Option<&[u8]>) -> String {
    match bytes {
        Some(bytes) => format!("blake3:{}", blake3::hash(bytes).to_hex()),
        None => ABSENT_KNOWLEDGE_FINGERPRINT.to_string(),
    }
}

/// The value after `previous`: strictly greater than it, and at least the
/// current wall-clock microsecond so a lost record never leads to reuse.
pub fn next_generation(previous: Option<u64>, now_micros: u64) -> u64 {
    let floor = previous.map_or(1, |value| value.saturating_add(1));
    floor.max(now_micros)
}

fn now_micros(now: DateTime<Utc>) -> u64 {
    u64::try_from(now.timestamp_micros()).unwrap_or(0)
}

fn is_not_found(error: &AgentStorageError) -> bool {
    matches!(error, AgentStorageError::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
}

async fn read_knowledge_bytes(
    storage: &AgentStorage,
) -> Result<Option<Vec<u8>>, IntentionGenerationError> {
    match storage.read_bytes(storage.user_knowledge_path()).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(IntentionGenerationError::KnowledgeUnreadable(
            error.to_string(),
        )),
    }
}

/// The stored record, `None` when there is none. A record that exists but
/// cannot be parsed is `Corrupt`, never `None`.
async fn read_record(
    storage: &AgentStorage,
) -> Result<Option<IntentionGenerationRecord>, IntentionGenerationError> {
    let path = intention_generation_path(storage);
    let bytes = match storage.read_bytes(&path).await {
        Ok(bytes) => bytes,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(IntentionGenerationError::Unreadable(error.to_string())),
    };
    let record: IntentionGenerationRecord = serde_json::from_slice(&bytes)
        .map_err(|error| IntentionGenerationError::Corrupt(error.to_string()))?;
    if record.schema_version != INTENTION_GENERATION_SCHEMA_VERSION {
        return Err(IntentionGenerationError::Corrupt(format!(
            "unsupported schema version `{}`",
            record.schema_version
        )));
    }
    Ok(Some(record))
}

/// Match a record against the knowledge bytes it claims to describe.
fn check_record(
    record: Option<IntentionGenerationRecord>,
    knowledge_bytes: Option<Vec<u8>>,
) -> Result<GenerationRead, IntentionGenerationError> {
    match record {
        // No record and no knowledge: nothing has ever been written for this
        // owner, so generation zero is exact.
        None if knowledge_bytes.is_none() => Ok(GenerationRead {
            generation: 0,
            knowledge_bytes,
        }),
        // Knowledge exists but no bump was ever recorded (it predates this
        // record, or a writer bypassed `persist_user_knowledge`). Zero would
        // stay zero across later unrecorded edits, so it is stale: the
        // reconcile path writes a fingerprinted record and the reader trusts
        // that instead.
        None => Err(IntentionGenerationError::Stale { generation: 0 }),
        Some(record) => {
            if record.knowledge_fingerprint == knowledge_fingerprint(knowledge_bytes.as_deref()) {
                Ok(GenerationRead {
                    generation: record.generation,
                    knowledge_bytes,
                })
            } else {
                Err(IntentionGenerationError::Stale {
                    generation: record.generation,
                })
            }
        },
    }
}

/// Move the generation forward. The caller must already hold the exclusive
/// flock on `knowledge.json` and must have finished writing it, so the
/// fingerprint describes the bytes this bump publishes.
///
/// An unreadable or corrupt previous record does not block the bump: the
/// clock seed in [`next_generation`] keeps the new value unused, and
/// rewriting the record is how a corrupt one heals.
pub async fn bump_intention_generation_locked(
    storage: &AgentStorage,
) -> Result<u64, IntentionGenerationError> {
    let knowledge_bytes = read_knowledge_bytes(storage).await?;
    let previous = match read_record(storage).await {
        Ok(record) => record.map(|record| record.generation),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "intention generation: previous record unusable; reseeding from the clock"
            );
            None
        },
    };
    let now = Utc::now();
    let generation = next_generation(previous, now_micros(now));
    let record = IntentionGenerationRecord {
        schema_version: INTENTION_GENERATION_SCHEMA_VERSION.to_string(),
        generation,
        knowledge_fingerprint: knowledge_fingerprint(knowledge_bytes.as_deref()),
        bumped_at: now,
    };
    storage
        .write_json_atomic(intention_generation_path(storage), &record)
        .await
        .map_err(|error| IntentionGenerationError::Write(error.to_string()))?;
    Ok(generation)
}

/// Bump for a writer that changed owner memory without holding the
/// `knowledge.json` flock (for example `forget_memory`). Takes the flock
/// itself, so it must not be called while the caller holds it.
pub async fn bump_intention_generation(
    storage: &AgentStorage,
) -> Result<u64, IntentionGenerationError> {
    let _flock = AgentStorage::acquire_file_lock_exclusive(&storage.user_knowledge_path())
        .await
        .map_err(|error| IntentionGenerationError::Lock(error.to_string()))?;
    bump_intention_generation_locked(storage).await
}

/// The current generation and the knowledge bytes it describes, read under
/// the shared flock so the two are one consistent pair.
///
/// Missing record: generation 0. Unreadable or corrupt record, unreadable
/// knowledge, or a fingerprint that does not match the knowledge on disk:
/// an error — never a silent 0.
pub async fn read_intention_generation_with_knowledge(
    storage: &AgentStorage,
) -> Result<GenerationRead, IntentionGenerationError> {
    let _flock = AgentStorage::acquire_file_lock_shared(&storage.user_knowledge_path())
        .await
        .map_err(|error| IntentionGenerationError::Lock(error.to_string()))?;
    let record = read_record(storage).await?;
    let knowledge_bytes = read_knowledge_bytes(storage).await?;
    check_record(record, knowledge_bytes)
}

/// The current generation. See [`read_intention_generation_with_knowledge`].
pub async fn read_intention_generation(
    storage: &AgentStorage,
) -> Result<u64, IntentionGenerationError> {
    read_intention_generation_with_knowledge(storage)
        .await
        .map(|read| read.generation)
}

/// Repair a stale or corrupt record under the exclusive flock, then return a
/// consistent read. A record that is already current is left alone.
///
/// A stale record means knowledge changed after the generation an earlier
/// reader saw, so moving to a fresh generation is exactly the invalidation
/// that was missed. Without this, one lost bump would leave alignment
/// unavailable until some unrelated knowledge write happened to bump again.
/// The current generation as a check reads it: repaired, under the
/// knowledge lock, when its record is stale or corrupt, so a damaged record
/// never keeps an answer from being recorded or compared.
pub async fn current_intention_generation(
    storage: &AgentStorage,
) -> Result<u64, IntentionGenerationError> {
    match read_intention_generation_with_knowledge(storage).await {
        Ok(read) => Ok(read.generation),
        Err(IntentionGenerationError::Stale { .. } | IntentionGenerationError::Corrupt(_)) => {
            reconcile_intention_generation(storage)
                .await
                .map(|read| read.generation)
        },
        Err(error) => Err(error),
    }
}

pub async fn reconcile_intention_generation(
    storage: &AgentStorage,
) -> Result<GenerationRead, IntentionGenerationError> {
    let _flock = AgentStorage::acquire_file_lock_exclusive(&storage.user_knowledge_path())
        .await
        .map_err(|error| IntentionGenerationError::Lock(error.to_string()))?;
    let record = match read_record(storage).await {
        Ok(record) => record,
        Err(IntentionGenerationError::Corrupt(_)) => None,
        Err(error) => return Err(error),
    };
    let knowledge_bytes = read_knowledge_bytes(storage).await?;
    let current = record.as_ref().is_some_and(|record| {
        record.knowledge_fingerprint == knowledge_fingerprint(knowledge_bytes.as_deref())
    });
    if current {
        return check_record(record, knowledge_bytes);
    }
    let generation = bump_intention_generation_locked(storage).await?;
    Ok(GenerationRead {
        generation,
        knowledge_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::agents::AgentMemoryService;

    #[test]
    fn next_generation_is_strictly_increasing_and_clock_seeded() {
        assert_eq!(next_generation(None, 0), 1);
        assert_eq!(next_generation(None, 500), 500);
        assert_eq!(next_generation(Some(7), 0), 8);
        assert_eq!(next_generation(Some(7), 100), 100);
        // A clock that steps backwards still moves forward.
        assert_eq!(next_generation(Some(1_000), 10), 1_001);
    }

    #[tokio::test]
    async fn missing_record_reads_as_generation_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        assert_eq!(
            read_intention_generation(service.storage()).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn knowledge_without_a_record_is_stale_until_reconciled() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let storage = service.storage();
        // A writer that bypasses `persist_user_knowledge` leaves no record.
        storage
            .write_json_atomic(
                storage.user_knowledge_path(),
                &serde_json::json!({"preferences": [{"key": "a"}]}),
            )
            .await
            .unwrap();
        assert!(matches!(
            read_intention_generation(storage).await,
            Err(IntentionGenerationError::Stale { generation: 0 })
        ));
        let repaired = reconcile_intention_generation(storage).await.unwrap();
        assert!(repaired.generation > 0);
        assert_eq!(
            read_intention_generation(storage).await.unwrap(),
            repaired.generation
        );
    }

    #[tokio::test]
    async fn every_knowledge_write_bumps_monotonically() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({"preferences": [{"key": "a"}]}))
            .await
            .unwrap();
        let first = read_intention_generation(service.storage()).await.unwrap();
        assert!(first > 0);
        service
            .save_user_knowledge(&serde_json::json!({"preferences": [{"key": "b"}]}))
            .await
            .unwrap();
        let second = read_intention_generation(service.storage()).await.unwrap();
        assert!(second > first, "{second} must follow {first}");
        // Re-saving identical content is still a write and still bumps:
        // over-invalidation only costs a reassessment.
        service
            .save_user_knowledge(&serde_json::json!({"preferences": [{"key": "b"}]}))
            .await
            .unwrap();
        let third = read_intention_generation(service.storage()).await.unwrap();
        assert!(third > second);
    }

    #[tokio::test]
    async fn a_write_that_bypassed_the_bump_reads_as_stale_until_reconciled() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({"preferences": [{"key": "a"}]}))
            .await
            .unwrap();
        let before = read_intention_generation(service.storage()).await.unwrap();

        // A writer that skips `persist_user_knowledge` (or a lost bump).
        let storage = service.storage();
        storage
            .write_json_atomic(
                storage.user_knowledge_path(),
                &serde_json::json!({"preferences": [{"key": "changed"}]}),
            )
            .await
            .unwrap();
        match read_intention_generation(storage).await {
            Err(IntentionGenerationError::Stale { generation }) => assert_eq!(generation, before),
            other => panic!("expected a stale generation, got {other:?}"),
        }

        let repaired = reconcile_intention_generation(storage).await.unwrap();
        assert!(repaired.generation > before);
        assert_eq!(
            read_intention_generation(storage).await.unwrap(),
            repaired.generation
        );
    }

    #[tokio::test]
    async fn a_corrupt_record_is_an_error_never_zero_and_the_next_bump_heals_it() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({"preferences": []}))
            .await
            .unwrap();
        let before = read_intention_generation(service.storage()).await.unwrap();
        let storage = service.storage();
        storage
            .write_bytes_atomic(intention_generation_path(storage), b"{not json")
            .await
            .unwrap();
        assert!(matches!(
            read_intention_generation(storage).await,
            Err(IntentionGenerationError::Corrupt(_))
        ));

        service
            .save_user_knowledge(&serde_json::json!({"preferences": []}))
            .await
            .unwrap();
        let after = read_intention_generation(service.storage()).await.unwrap();
        assert!(
            after > before,
            "a reseeded generation never reuses an old value"
        );
    }
}
