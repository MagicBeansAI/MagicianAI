//! What admission carries from the check to the dispatch: the admission
//! ticket, the freshness fence, the consume-once registry the backstop reads,
//! and the durable, content-free alignment log (plan §9.2 steps 6–7, P0
//! contracts §2 and §6.4).
//!
//! # The fence
//!
//! A check judges a captured snapshot of the owner's intentions, and the judge
//! call takes seconds. An owner who says "actually, check with me first" in
//! that window must not see the act go out on the old answer. So an admitted
//! act carries the [`StandingGeneration`] it was judged at, and the last
//! host-controlled boundary before the fire re-reads the current generation
//! and compares for equality ([`verify_fence`]). Any difference, or a read
//! that fails, means the assessment is stale: the act does not fire, and the
//! next turn checks it again. No model call runs inside the fence.
//!
//! # The registry
//!
//! `execute_action_inner` is the one function every dispatch path shares. The
//! native loop has already judged its acts by the time they reach it; plane
//! calls and harness execute-on-resume have not. The loop therefore records
//! each admitted act here, keyed by owner, execution and the act's binding,
//! and the backstop consumes the entry once. An act with no entry is judged at
//! the backstop. An argument that changed after admission changes the binding,
//! so a widened act is judged again rather than riding the earlier admission.
//!
//! # The log
//!
//! One JSON line per check and per admission, in the owner's own memory
//! directory. It records classes, dispositions, generations, latencies and
//! binding hashes — never intentions, arguments or explanation text — so a
//! shadow period can be compared offline without keeping owner content twice.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::generation::{read_intention_generation, IntentionGenerationError};
use super::intention::StandingGeneration;
use super::sources::taste_profile::{
    read_taste_profile_checked, DEFAULT_TASTE_PROFILE_ADMISSION_TIMEOUT,
};
use crate::magician_v2::agents::storage::AgentStorage;
use crate::magician_v2::agents::AgentMemoryResolver;
use crate::magician_v2::taste_profile::TasteProfileLoader;

pub const ADMISSION_RECORD_SCHEMA_VERSION: &str = "alignment_admission.v1";
pub const ALIGNMENT_LOG_FILE: &str = "preference_alignment_log.jsonl";
/// The log is rotated once it passes this size: the current file moves to
/// `.1` (replacing an older `.1`) and a new file starts.
pub const ALIGNMENT_LOG_ROTATE_BYTES: u64 = 4 * 1024 * 1024;
/// How long an admission waits to be consumed by the dispatch it was made
/// for. Longer than any single dispatch's queueing; an unconsumed entry only
/// costs the act a second check.
pub const ADMISSION_TTL: Duration = Duration::from_secs(2 * 60);
/// Upper bound on outstanding admissions process-wide. The oldest are dropped
/// first; a dropped admission only costs a second check.
pub const MAX_OUTSTANDING_ADMISSIONS: usize = 4_096;

/// Why an act was admitted (P0 contracts §6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOrigin {
    /// The judge decided the act may proceed (with or without a nudge).
    ModelAction,
    /// A complete, verified-empty inventory: nothing to compare.
    NothingToCompare,
    /// The owner answered an alignment hold with `allow_once`.
    HostException,
    /// The owner chose to proceed although the check could not run.
    OwnerWaiver,
    /// Shadow mode: recorded, never enforced.
    Shadow,
    /// A worker re-entered `Apply` against a batch committed earlier. The act
    /// was admitted when that batch was gated and may already have fired, so
    /// it is not judged again.
    RecoveredCommittedBatch,
}

/// The content-free record of one admitted act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRecord {
    pub schema_version: String,
    pub binding_id: String,
    pub decision_origin: DecisionOrigin,
    /// The generation the act was judged at. `None` when the owner answered
    /// for this exact act: their answer is about the act, not a snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<StandingGeneration>,
    pub mode: String,
    pub policy_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub admitted_at_ms: i64,
}

/// What the gate hands the dispatch for one admitted, checked act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionTicket {
    pub principal: String,
    pub workspace: String,
    pub record: AdmissionRecord,
    /// Whether the fence governs this act. False in shadow mode, where the
    /// answer never changes what happens.
    pub enforced: bool,
}

impl AdmissionTicket {
    /// The generation the fence must find unchanged, when one applies.
    pub fn fenced_generation(&self) -> Option<&StandingGeneration> {
        if self.enforced {
            self.record.generation.as_ref()
        } else {
            None
        }
    }
}

/// The fence's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceVerdict {
    /// Nothing the assessment depended on has changed.
    Fresh,
    /// Something changed: the assessment no longer describes the owner.
    Stale { reason: String },
    /// The current generation could not be read. Treated like stale: the act
    /// does not fire on an assessment nobody can confirm is current.
    Unverifiable { reason: String },
}

impl FenceVerdict {
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Fresh => None,
            Self::Stale { reason } | Self::Unverifiable { reason } => Some(reason),
        }
    }
}

/// Compare the ticket's captured generation with the current one.
///
/// Reads only: the intention generation under the knowledge file's shared
/// lock, and the taste profile's content version through the checked reader.
/// No model call, no write.
pub async fn verify_fence(
    resolver: &AgentMemoryResolver,
    taste_loader: Option<&TasteProfileLoader>,
    ticket: &AdmissionTicket,
) -> FenceVerdict {
    let Some(captured) = ticket.fenced_generation() else {
        return FenceVerdict::Fresh;
    };
    let memory = match resolver.resolve_for_scope(&ticket.principal, &ticket.workspace) {
        Ok(memory) => memory,
        Err(error) => {
            return FenceVerdict::Unverifiable {
                reason: format!("owner memory could not be resolved: {error}"),
            }
        },
    };
    match read_intention_generation(memory.storage()).await {
        Ok(current) if current == captured.intention_generation => {},
        Ok(_) => {
            return FenceVerdict::Stale {
                reason: "saved preferences changed after the check".to_string(),
            }
        },
        // Knowledge changed without its bump: changed all the same.
        Err(IntentionGenerationError::Stale { .. }) => {
            return FenceVerdict::Stale {
                reason: "saved preferences changed after the check".to_string(),
            }
        },
        Err(error) => {
            return FenceVerdict::Unverifiable {
                reason: format!("the preference generation could not be read: {error}"),
            }
        },
    }
    let current_profile = match taste_loader {
        None => None,
        Some(loader) => {
            match tokio::time::timeout(
                DEFAULT_TASTE_PROFILE_ADMISSION_TIMEOUT,
                read_taste_profile_checked(loader, &ticket.principal, &ticket.workspace),
            )
            .await
            {
                Ok(Ok(read)) => read.map(|read| read.version),
                Ok(Err(error)) => {
                    return FenceVerdict::Unverifiable {
                        reason: format!("the taste profile could not be read: {error}"),
                    }
                },
                Err(_) => {
                    return FenceVerdict::Unverifiable {
                        reason: "the taste profile read timed out".to_string(),
                    }
                },
            }
        },
    };
    // Without a loader the profile was not read at check time either, so
    // there is nothing to compare.
    if taste_loader.is_some() && current_profile != captured.profile_version {
        return FenceVerdict::Stale {
            reason: "the taste profile changed after the check".to_string(),
        };
    }
    // The owner's intervention policy is part of what the check decided
    // under: a tighter policy saved while the judge ran must stop the act.
    match super::policy::read_policy(resolver, &ticket.principal, &ticket.workspace).await {
        Ok(policy) if policy.effective().version == ticket.record.policy_version => {},
        Ok(_) => {
            return FenceVerdict::Stale {
                reason: "your check policy changed after the check".to_string(),
            }
        },
        Err(error) => {
            return FenceVerdict::Unverifiable {
                reason: format!("the check policy could not be read: {error}"),
            }
        },
    }
    FenceVerdict::Fresh
}

// ── The consume-once registry ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AdmissionKey {
    principal: String,
    workspace: String,
    execution_id: String,
    binding_id: String,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<AdmissionKey, Vec<Instant>>,
    outstanding: usize,
}

impl Registry {
    fn prune(&mut self, now: Instant) {
        let mut outstanding = 0;
        self.entries.retain(|_, stamps| {
            stamps.retain(|stamp| now.duration_since(*stamp) < ADMISSION_TTL);
            outstanding += stamps.len();
            !stamps.is_empty()
        });
        self.outstanding = outstanding;
    }

    fn drop_oldest(&mut self) {
        let oldest = self
            .entries
            .iter()
            .filter_map(|(key, stamps)| stamps.first().map(|stamp| (key.clone(), *stamp)))
            .min_by_key(|(_, stamp)| *stamp);
        if let Some((key, _)) = oldest {
            if let Some(stamps) = self.entries.get_mut(&key) {
                stamps.remove(0);
                self.outstanding = self.outstanding.saturating_sub(1);
                if stamps.is_empty() {
                    self.entries.remove(&key);
                }
            }
        }
    }
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut slot = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(slot.get_or_insert_with(Registry::default))
}

fn key(
    principal: &str,
    workspace: &str,
    execution_id: Option<&str>,
    binding_id: &str,
) -> AdmissionKey {
    AdmissionKey {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        execution_id: execution_id.unwrap_or_default().to_string(),
        binding_id: binding_id.to_string(),
    }
}

/// Record that this exact act was admitted for this owner and execution.
pub fn register_admission(
    principal: &str,
    workspace: &str,
    execution_id: Option<&str>,
    binding_id: &str,
) {
    let now = Instant::now();
    with_registry(|registry| {
        registry.prune(now);
        while registry.outstanding >= MAX_OUTSTANDING_ADMISSIONS {
            registry.drop_oldest();
        }
        registry
            .entries
            .entry(key(principal, workspace, execution_id, binding_id))
            .or_default()
            .push(now);
        registry.outstanding += 1;
    });
}

/// Whether an admission is outstanding for this exact act, without taking it.
pub fn has_admission(
    principal: &str,
    workspace: &str,
    execution_id: Option<&str>,
    binding_id: &str,
) -> bool {
    let now = Instant::now();
    with_registry(|registry| {
        registry.prune(now);
        registry
            .entries
            .contains_key(&key(principal, workspace, execution_id, binding_id))
    })
}

/// Consume one admission for this exact act. False when none is outstanding.
pub fn take_admission(
    principal: &str,
    workspace: &str,
    execution_id: Option<&str>,
    binding_id: &str,
) -> bool {
    let now = Instant::now();
    with_registry(|registry| {
        registry.prune(now);
        let key = key(principal, workspace, execution_id, binding_id);
        let Some(stamps) = registry.entries.get_mut(&key) else {
            return false;
        };
        stamps.remove(0);
        registry.outstanding = registry.outstanding.saturating_sub(1);
        if stamps.is_empty() {
            registry.entries.remove(&key);
        }
        true
    })
}

// ── The durable log ─────────────────────────────────────────────────────────

/// Where the owner's alignment log lives: beside their knowledge file.
pub fn alignment_log_path(storage: &AgentStorage) -> PathBuf {
    storage.user_root().join(ALIGNMENT_LOG_FILE)
}

/// Append one content-free line to the owner's alignment log.
///
/// Best effort by design: the log is evidence for offline comparison, not an
/// authority. A failed append is logged and never changes what happens to the
/// act — the fence and the receipts are what govern admission.
pub async fn append_alignment_log(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    entry: &Value,
) {
    let memory = match resolver.resolve_for_scope(principal, workspace) {
        Ok(memory) => memory,
        Err(error) => {
            tracing::warn!(error = %error, "preference alignment: log scope unresolved");
            return;
        },
    };
    let path = alignment_log_path(memory.storage());
    if let Err(error) = append_line(&path, entry).await {
        tracing::warn!(
            error = %error,
            "preference alignment: could not append to the alignment log"
        );
    }
}

/// Append one JSON line to `path`, rotating it to `.jsonl.1` past
/// [`ALIGNMENT_LOG_ROTATE_BYTES`]. Shared by the owner's alignment journals.
pub(crate) async fn append_line(path: &std::path::Path, entry: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Serialize rotation and append; otherwise an append can land in the old
    // file after a concurrent rotation, or two writers can split a JSON line.
    let flock = AgentStorage::acquire_file_lock_exclusive(path)
        .await
        .map_err(std::io::Error::other)?;
    let mut line = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let path = path.to_path_buf();
    let permit = magician_core::blocking_admission::acquire_blocking_admission()
        .await
        .map_err(std::io::Error::other)?;
    magician_core::blocking_admission::spawn_blocking_with_admission(permit, move || {
        use magician_core::durable_io::{
            publish_staged_file_durably_sync, sync_parent_dir_blocking,
        };
        use std::io::Write;
        // Keep the lock in the blocking task even if its async caller is cancelled.
        let _flock = flock;
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() >= ALIGNMENT_LOG_ROTATE_BYTES => {
                publish_staged_file_durably_sync(&path, &path.with_extension("jsonl.1"))?;
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(&line)?;
        file.sync_all()?;
        sync_parent_dir_blocking(&path)
    })
    .await
    .map_err(std::io::Error::other)?
}

/// The log line for an admission.
pub fn admission_log_entry(record: &AdmissionRecord, fence: Option<&FenceVerdict>) -> Value {
    serde_json::json!({
        "kind": "admission",
        "record": record,
        "fence": match fence {
            None => "not_fenced",
            Some(FenceVerdict::Fresh) => "fresh",
            Some(FenceVerdict::Stale { .. }) => "stale",
            Some(FenceVerdict::Unverifiable { .. }) => "unverifiable",
        },
        "at_ms": chrono::Utc::now().timestamp_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::generation::bump_intention_generation;

    fn ticket(generation: u64, enforced: bool) -> AdmissionTicket {
        AdmissionTicket {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            record: AdmissionRecord {
                schema_version: ADMISSION_RECORD_SCHEMA_VERSION.to_string(),
                binding_id: "alignment_binding.v1:abc".to_string(),
                decision_origin: DecisionOrigin::ModelAction,
                generation: Some(StandingGeneration {
                    intention_generation: generation,
                    profile_version: None,
                    app_generation: None,
                }),
                mode: "enforce".to_string(),
                policy_version: super::super::service::DEFAULT_POLICY_VERSION.to_string(),
                execution_id: Some("exec".to_string()),
                admitted_at_ms: 0,
            },
            enforced,
        }
    }

    #[test]
    fn an_admission_is_consumed_once_and_only_for_its_exact_act() {
        let binding = "alignment_binding.v1:registry-test";
        register_admission("p-reg", "w", Some("e1"), binding);
        assert!(!take_admission("p-reg", "w", Some("e2"), binding));
        assert!(!take_admission(
            "p-reg",
            "w",
            Some("e1"),
            "alignment_binding.v1:other"
        ));
        assert!(take_admission("p-reg", "w", Some("e1"), binding));
        assert!(!take_admission("p-reg", "w", Some("e1"), binding));
    }

    #[tokio::test]
    async fn the_fence_passes_an_unchanged_generation_and_stops_a_changed_one() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let memory = resolver.resolve_for_scope("owner", "default").unwrap();
        let generation = bump_intention_generation(memory.storage()).await.unwrap();

        assert_eq!(
            verify_fence(&resolver, None, &ticket(generation, true)).await,
            FenceVerdict::Fresh
        );

        bump_intention_generation(memory.storage()).await.unwrap();
        let verdict = verify_fence(&resolver, None, &ticket(generation, true)).await;
        assert!(matches!(verdict, FenceVerdict::Stale { .. }), "{verdict:?}");
    }

    #[tokio::test]
    async fn shadow_and_owner_answers_are_not_fenced() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        // A generation that cannot match anything on disk.
        assert!(verify_fence(&resolver, None, &ticket(u64::MAX, false))
            .await
            .is_fresh());
        let mut answered = ticket(u64::MAX, true);
        answered.record.generation = None;
        answered.record.decision_origin = DecisionOrigin::HostException;
        assert!(verify_fence(&resolver, None, &answered).await.is_fresh());
    }

    #[tokio::test]
    async fn the_log_appends_content_free_lines_and_rotates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("users").join(ALIGNMENT_LOG_FILE);
        let record = ticket(1, true).record;
        append_line(
            &path,
            &admission_log_entry(&record, Some(&FenceVerdict::Fresh)),
        )
        .await
        .unwrap();
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(text.lines().count(), 1);
        let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(line["kind"], "admission");
        assert_eq!(line["fence"], "fresh");
        assert_eq!(line["record"]["binding_id"], "alignment_binding.v1:abc");

        // Past the rotation size, the next append moves the file aside.
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(ALIGNMENT_LOG_ROTATE_BYTES).unwrap();
        append_line(&path, &admission_log_entry(&record, None))
            .await
            .unwrap();
        assert!(path.with_extension("jsonl.1").exists());
        let fresh = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(fresh.lines().count(), 1);
    }

    #[tokio::test]
    async fn concurrent_appends_preserve_every_complete_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(ALIGNMENT_LOG_FILE);
        let mut writers = tokio::task::JoinSet::new();
        for id in 0..32 {
            let path = path.clone();
            writers.spawn(async move { append_line(&path, &serde_json::json!({"id": id})).await });
        }
        while let Some(result) = writers.join_next().await {
            result.unwrap().unwrap();
        }
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(text.ends_with('\n'));
        let ids: std::collections::BTreeSet<_> = text
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["id"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(ids, (0..32).collect());
        assert_eq!(text.lines().count(), 32);
    }

    #[tokio::test]
    async fn failed_rotation_keeps_the_current_log_and_reports_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(ALIGNMENT_LOG_FILE);
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(ALIGNMENT_LOG_ROTATE_BYTES).unwrap();
        tokio::fs::create_dir(path.with_extension("jsonl.1"))
            .await
            .unwrap();
        assert!(append_line(&path, &serde_json::json!({"id": 1}))
            .await
            .is_err());
        assert_eq!(
            tokio::fs::metadata(&path).await.unwrap().len(),
            ALIGNMENT_LOG_ROTATE_BYTES
        );
    }
}
