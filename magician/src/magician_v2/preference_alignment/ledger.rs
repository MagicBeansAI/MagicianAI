//! The exception ledger: one durable, consume-once record per owner answer
//! that lets a held act through (plan §10.3, P5.3).
//!
//! An owner's `allow_once` or `proceed_without_check` becomes a
//! purpose-prefixed receipt in the resumed run's replay list (see
//! [`super::receipts`]). That list is in-memory and is copied wherever the run's
//! context is cloned — an in-context delegate starts from a clone of its
//! parent — so the list alone cannot promise "once". The ledger can: the
//! answer is recorded here when the owner gives it, and the gate admits the
//! act only if it can mark that record consumed. A second consumer, a sibling
//! run, a different act or a late copy finds nothing to consume.
//!
//! # Identity and retries
//!
//! A record is keyed by the ask it answers. Recording the same answer to the
//! same ask again (a lost response, a client retry) returns the original
//! record; a different answer to an ask that already has one is a conflict
//! and records nothing.
//!
//! # Locking
//!
//! Every read-modify-write runs under an exclusive flock on the ledger file,
//! so two processes or two delegates cannot both consume one record.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::receipts::AlignmentReceiptPurpose;
use crate::magician_v2::agents::storage::{AgentStorage, AgentStorageError};
use crate::magician_v2::agents::AgentMemoryResolver;

pub const ALIGNMENT_LEDGER_FILE: &str = "preference_alignment_ledger.json";
pub const ALIGNMENT_LEDGER_SCHEMA_VERSION: &str = "alignment_ledger.v1";
/// How long an answer waits for the resumed run to reach the act. The resume
/// re-proposes the act within seconds; an hour covers a slow restart without
/// turning a one-time answer into a standing one.
pub const LEDGER_GRANT_TTL_MS: i64 = 60 * 60 * 1000;
/// Records kept per owner. Consumed and expired records go first.
pub const MAX_LEDGER_ENTRIES: usize = 512;
/// Ask ids of pruned records, kept so a replayed answer can never be issued
/// (and run) a second time. Longer than any hold's answer window.
pub const MAX_RETIRED_ASKS: usize = 8192;
pub const RETIRED_ASK_TTL_MS: i64 = 31 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// The ask this answer resolves.
    pub ask_id: String,
    pub purpose: AlignmentReceiptPurpose,
    /// `alignment_binding.v1` of the one act the answer covers.
    pub binding_id: String,
    /// The execution the ask was raised in. Another run cannot consume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at_ms: Option<i64>,
    /// For a waiver (`proceed_without_check`): the owner's saved-preference
    /// generation when they answered. A preference saved after it voids the
    /// waiver (plan §11.1); the act is checked again. An exception answers
    /// for this exact act and carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intention_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LedgerFile {
    schema_version: String,
    #[serde(default)]
    entries: Vec<LedgerEntry>,
    /// `(ask_id, retired_at_ms)` of records pruned from `entries`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired: Vec<(String, i64)>,
}

impl Default for LedgerFile {
    fn default() -> Self {
        Self {
            schema_version: ALIGNMENT_LEDGER_SCHEMA_VERSION.to_string(),
            entries: Vec::new(),
            retired: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueOutcome {
    Issued,
    /// The same answer to the same ask was already recorded.
    AlreadyIssued,
    /// The ask already has a different answer. Nothing was recorded.
    Conflict,
    /// The ask was answered and its record retired. A replayed answer is
    /// never issued again.
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeOutcome {
    Consumed,
    /// The answer was used already.
    AlreadyConsumed,
    /// The answer's window closed before the act came back.
    Expired,
    /// No answer covers this act in this run.
    NotIssued,
    /// The owner's saved preferences changed after a waiver was given. The
    /// waiver stays unused and the act is checked again.
    Stale,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("ledger scope unresolved: {0}")]
    Scope(String),
    #[error("ledger lock failed: {0}")]
    Lock(String),
    #[error("ledger unreadable: {0}")]
    Read(String),
    #[error("ledger corrupt: {0}")]
    Corrupt(String),
    #[error("ledger write failed: {0}")]
    Write(String),
}

pub fn alignment_ledger_path(storage: &AgentStorage) -> PathBuf {
    storage.user_root().join(ALIGNMENT_LEDGER_FILE)
}

fn storage_for(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<AgentStorage, LedgerError> {
    resolver
        .resolve_for_scope(principal, workspace)
        .map(|memory| memory.storage().clone())
        .map_err(|error| LedgerError::Scope(error.to_string()))
}

async fn read_ledger(storage: &AgentStorage, path: &PathBuf) -> Result<LedgerFile, LedgerError> {
    match storage.read_bytes(path).await {
        Ok(bytes) => {
            let ledger: LedgerFile = serde_json::from_slice(&bytes)
                .map_err(|error| LedgerError::Corrupt(error.to_string()))?;
            if ledger.schema_version != ALIGNMENT_LEDGER_SCHEMA_VERSION {
                return Err(LedgerError::Corrupt(format!(
                    "unsupported schema version `{}`",
                    ledger.schema_version
                )));
            }
            Ok(ledger)
        },
        Err(AgentStorageError::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => {
            Ok(LedgerFile::default())
        },
        Err(error) => Err(LedgerError::Read(error.to_string())),
    }
}

/// Keep the newest records, dropping consumed and expired ones first.
fn prune(ledger: &mut LedgerFile, now_ms: i64) {
    if ledger.entries.len() <= MAX_LEDGER_ENTRIES {
        return;
    }
    let live = |entry: &LedgerEntry| entry.consumed_at_ms.is_none() && entry.expires_at_ms > now_ms;
    let mut dead: Vec<usize> = (0..ledger.entries.len())
        .filter(|index| !live(&ledger.entries[*index]))
        .collect();
    let excess = ledger.entries.len() - MAX_LEDGER_ENTRIES;
    dead.truncate(excess);
    let mut index = 0;
    let mut retired = Vec::new();
    ledger.entries.retain(|entry| {
        let keep = !dead.contains(&index);
        index += 1;
        if !keep {
            retired.push(entry.ask_id.clone());
        }
        keep
    });
    // Still over: drop the oldest regardless. An answer lost this way only
    // costs the owner a second ask.
    if ledger.entries.len() > MAX_LEDGER_ENTRIES {
        let drop = ledger.entries.len() - MAX_LEDGER_ENTRIES;
        retired.extend(ledger.entries.drain(..drop).map(|entry| entry.ask_id));
    }
    ledger
        .retired
        .extend(retired.into_iter().map(|ask_id| (ask_id, now_ms)));
    ledger
        .retired
        .retain(|(_, at)| now_ms.saturating_sub(*at) < RETIRED_ASK_TTL_MS);
    if ledger.retired.len() > MAX_RETIRED_ASKS {
        let drop = ledger.retired.len() - MAX_RETIRED_ASKS;
        ledger.retired.drain(..drop);
    }
}

/// Apply one change to the owner's ledger under its exclusive flock.
async fn with_ledger<T>(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    change: impl FnOnce(&mut LedgerFile) -> (T, bool),
) -> Result<T, LedgerError> {
    let storage = storage_for(resolver, principal, workspace)?;
    let path = alignment_ledger_path(&storage);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| LedgerError::Write(error.to_string()))?;
    }
    let _flock = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| LedgerError::Lock(error.to_string()))?;
    let mut ledger = read_ledger(&storage, &path).await?;
    let (result, changed) = change(&mut ledger);
    if changed {
        storage
            .write_json_atomic(&path, &ledger)
            .await
            .map_err(|error| LedgerError::Write(error.to_string()))?;
    }
    Ok(result)
}

/// Record the owner's answer. Idempotent on the ask.
pub async fn issue(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    entry: LedgerEntry,
) -> Result<IssueOutcome, LedgerError> {
    with_ledger(resolver, principal, workspace, |ledger| {
        if let Some(existing) = ledger
            .entries
            .iter()
            .find(|existing| existing.ask_id == entry.ask_id)
        {
            let same = existing.purpose == entry.purpose
                && existing.binding_id == entry.binding_id
                && existing.execution_id == entry.execution_id;
            return (
                if same {
                    IssueOutcome::AlreadyIssued
                } else {
                    IssueOutcome::Conflict
                },
                false,
            );
        }
        if ledger.retired.iter().any(|(ask_id, _)| *ask_id == entry.ask_id) {
            return (IssueOutcome::Retired, false);
        }
        let now_ms = entry.issued_at_ms;
        ledger.entries.push(entry);
        prune(ledger, now_ms);
        (IssueOutcome::Issued, true)
    })
    .await
}

/// Mark the answer covering this act consumed. Only [`ConsumeOutcome::Consumed`]
/// admits the act.
pub async fn consume(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    execution_id: Option<&str>,
    binding_id: &str,
    purpose: AlignmentReceiptPurpose,
    now_ms: i64,
) -> Result<ConsumeOutcome, LedgerError> {
    // Read before the ledger's own lock, so the knowledge lock is never
    // taken under it. `None` when unreadable: a fenced waiver then stays
    // unused (fail closed).
    // Only a waiver records a generation; an exception never reads one.
    let current_generation = if purpose == AlignmentReceiptPurpose::CheckWaiver {
        match storage_for(resolver, principal, workspace) {
            Ok(storage) => super::generation::current_intention_generation(&storage).await.ok(),
            Err(_) => None,
        }
    } else {
        None
    };
    with_ledger(resolver, principal, workspace, |ledger| {
        let matches = |entry: &LedgerEntry| {
            entry.purpose == purpose
                && entry.binding_id == binding_id
                && entry.execution_id.as_deref() == execution_id
        };
        if let Some(entry) = ledger
            .entries
            .iter_mut()
            .find(|entry| matches(entry) && entry.consumed_at_ms.is_none() && entry.expires_at_ms > now_ms)
        {
            if let Some(answered_at) = entry.intention_generation {
                if current_generation != Some(answered_at) {
                    return (ConsumeOutcome::Stale, false);
                }
            }
            entry.consumed_at_ms = Some(now_ms);
            return (ConsumeOutcome::Consumed, true);
        }
        let outcome = if ledger
            .entries
            .iter()
            .any(|entry| matches(entry) && entry.consumed_at_ms.is_none())
        {
            ConsumeOutcome::Expired
        } else if ledger.entries.iter().any(matches) {
            ConsumeOutcome::AlreadyConsumed
        } else {
            ConsumeOutcome::NotIssued
        };
        (outcome, false)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ask_id: &str, binding: &str, execution: &str, now_ms: i64) -> LedgerEntry {
        LedgerEntry {
            ask_id: ask_id.to_string(),
            purpose: AlignmentReceiptPurpose::Exception,
            binding_id: binding.to_string(),
            execution_id: Some(execution.to_string()),
            issued_at_ms: now_ms,
            expires_at_ms: now_ms + LEDGER_GRANT_TTL_MS,
            consumed_at_ms: None,
            intention_generation: None,
        }
    }

    #[tokio::test]
    async fn an_answer_is_consumed_once_and_only_by_its_own_run_and_act() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let (p, w) = ("owner", "default");
        let now = 1_000_000;
        assert_eq!(
            issue(&resolver, p, w, entry("ask-1", "b1", "exec-1", now)).await.unwrap(),
            IssueOutcome::Issued
        );
        let take = |execution: &'static str, binding: &'static str| {
            let resolver = &resolver;
            async move {
                consume(
                    resolver,
                    p,
                    w,
                    Some(execution),
                    binding,
                    AlignmentReceiptPurpose::Exception,
                    now + 1,
                )
                .await
                .unwrap()
            }
        };
        // A sibling run and a different act find nothing.
        assert_eq!(take("exec-2", "b1").await, ConsumeOutcome::NotIssued);
        assert_eq!(take("exec-1", "b2").await, ConsumeOutcome::NotIssued);
        // The waiver purpose does not satisfy an exception, or the reverse.
        assert_eq!(
            consume(&resolver, p, w, Some("exec-1"), "b1", AlignmentReceiptPurpose::CheckWaiver, now + 1)
                .await
                .unwrap(),
            ConsumeOutcome::NotIssued
        );
        assert_eq!(take("exec-1", "b1").await, ConsumeOutcome::Consumed);
        // A cloned context holding the same receipt cannot use it again.
        assert_eq!(take("exec-1", "b1").await, ConsumeOutcome::AlreadyConsumed);
    }

    #[tokio::test]
    async fn a_retried_answer_returns_the_original_and_a_different_one_conflicts() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let now = 5;
        let first = entry("ask-1", "b1", "exec-1", now);
        assert_eq!(issue(&resolver, "o", "w", first.clone()).await.unwrap(), IssueOutcome::Issued);
        assert_eq!(
            issue(&resolver, "o", "w", first.clone()).await.unwrap(),
            IssueOutcome::AlreadyIssued
        );
        let mut other = first;
        other.purpose = AlignmentReceiptPurpose::CheckWaiver;
        assert_eq!(issue(&resolver, "o", "w", other).await.unwrap(), IssueOutcome::Conflict);
    }

    #[tokio::test]
    async fn an_answer_past_its_window_does_not_admit() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let now = 10;
        issue(&resolver, "o", "w", entry("ask-1", "b1", "exec-1", now)).await.unwrap();
        assert_eq!(
            consume(
                &resolver,
                "o",
                "w",
                Some("exec-1"),
                "b1",
                AlignmentReceiptPurpose::Exception,
                now + LEDGER_GRANT_TTL_MS + 1
            )
            .await
            .unwrap(),
            ConsumeOutcome::Expired
        );
    }

    #[test]
    fn pruning_keeps_live_answers_first() {
        let mut ledger = LedgerFile::default();
        for index in 0..(MAX_LEDGER_ENTRIES + 3) {
            let mut e = entry(&format!("ask-{index}"), "b", "e", 100);
            if index < 3 {
                e.consumed_at_ms = Some(101);
            }
            ledger.entries.push(e);
        }
        prune(&mut ledger, 100);
        assert_eq!(ledger.entries.len(), MAX_LEDGER_ENTRIES);
        assert!(ledger.entries.iter().all(|e| e.consumed_at_ms.is_none()));
        // A pruned answer is remembered, so replaying it issues nothing.
        assert_eq!(ledger.retired.len(), 3);
        assert!(ledger.retired.iter().any(|(ask_id, _)| ask_id == "ask-0"));
    }

    #[tokio::test]
    async fn a_replayed_answer_whose_record_was_pruned_is_never_issued_again() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let (p, w) = ("owner", "default");
        let now = 1_000_000;
        // The record of an answered ask was pruned; only its tombstone is left.
        let storage = storage_for(&resolver, p, w).unwrap();
        let path = alignment_ledger_path(&storage);
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        let mut ledger = LedgerFile::default();
        ledger.retired.push(("ask-0".to_string(), now));
        storage.write_json_atomic(&path, &ledger).await.unwrap();
        assert_eq!(
            issue(&resolver, p, w, entry("ask-0", "b0", "e", now)).await.unwrap(),
            IssueOutcome::Retired
        );
    }
}
