//! Persisted operator decisions for noisy API-mining origins.
//!
//! This is intentionally small in scope: it records whether an origin has been
//! explicitly allowed or blocked from future capture/mining so the Forge UI can
//! review high-frequency origins without editing config files.

use super::capability::{ConfidenceLevel, SideEffects};
use super::capability_store::CapabilityStore;
use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use tracing::warn;

const ORIGIN_POLICY_VERSION: &str = "1.0.0";
const ORIGIN_POLICY_FILENAME: &str = "origin_policies.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OriginPolicyDecision {
    Allowed,
    Blocked,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OriginReplayMode {
    ObserveOnly,
    ValidateOnly,
    ReplayReads,
    ReplayWritesWithHitl,
    ReplayTrustedWrites,
}

impl Default for OriginReplayMode {
    fn default() -> Self {
        Self::ValidateOnly
    }
}

impl OriginReplayMode {
    pub fn allows_inline_auto_replay(self) -> bool {
        matches!(
            self,
            Self::ReplayReads | Self::ReplayWritesWithHitl | Self::ReplayTrustedWrites
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginReplayCheck {
    Allowed,
    RequiresHitl { reason: String },
    Denied { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredOriginPolicyEntry {
    decision: OriginPolicyDecision,
    updated_at: i64,
    /// Whether mined capabilities from this origin may be auto-replayed
    /// during mining (PL/CE deferred-work item — cold-start fix).
    ///
    /// Defaults to `false` so legacy entries — which were written before
    /// this field existed — remain conservative after upgrade. Operators
    /// flip it to `true` per origin via
    /// `OriginPolicyStore::set_allow_replay` once they have audited the
    /// origin's safety posture.
    #[serde(default)]
    allow_replay: bool,
    /// Explicit replay policy for live browser-to-API takeover.
    ///
    /// Old files do not have this field; `effective_replay_mode` maps
    /// `allow_replay=true` to read replay only and `allow_replay=false` to
    /// validate-only so an upgrade never silently enables writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replay_mode: Option<OriginReplayMode>,
}

impl StoredOriginPolicyEntry {
    fn effective_replay_mode(&self) -> OriginReplayMode {
        self.replay_mode.unwrap_or_else(|| {
            if self.allow_replay {
                OriginReplayMode::ReplayReads
            } else {
                OriginReplayMode::ValidateOnly
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OriginPolicyFile {
    version: String,
    origins: HashMap<String, StoredOriginPolicyEntry>,
}

impl Default for OriginPolicyFile {
    fn default() -> Self {
        Self {
            version: ORIGIN_POLICY_VERSION.to_string(),
            origins: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OriginPolicyEntry {
    pub origin_key: String,
    pub origin_url: String,
    pub decision: OriginPolicyDecision,
    pub updated_at: i64,
    /// Operator-set: whether auto-replay during mining may fire against
    /// this origin. Mirrors `StoredOriginPolicyEntry::allow_replay` for
    /// the HTTP/Forge surface.
    #[serde(default)]
    pub allow_replay: bool,
    #[serde(default)]
    pub replay_mode: OriginReplayMode,
}

/// One shared in-memory policy map per policy file, for the life of the process.
///
/// `open` is called once per request. Before this existed each call built its own
/// `RwLock` over its own copy, so the lock guarded nothing that another request
/// could see: two concurrent writers each read the file, each mutated their
/// private map, and each wrote the whole map back. The later write silently
/// reverted the earlier one — and for this store the earlier one is an operator
/// deciding to **block an origin**, with `Allowed` as the default for anything
/// missing. A lost update here is a security decision undone with no error.
///
/// Sharing the state rather than the store keeps `open` a cheap handle, so no
/// caller changes; the `RwLock` it already had simply becomes meaningful. The
/// read-modify-write in every setter is already inside that lock, so nothing
/// else has to move.
///
/// One process per data root is the supported deployment (decided 2026-08-12),
/// which is what makes an in-process lock sufficient here rather than a file
/// lock. Entries are keyed by policy path and never evicted: a handful of scopes
/// in production, one per temp directory in tests.
static ORIGIN_POLICY_STATES: OnceLock<Mutex<HashMap<PathBuf, Arc<RwLock<OriginPolicyFile>>>>> =
    OnceLock::new();

/// Shared store for persisted origin decisions under `{api_mining_base}/origin_policies.json`.
pub struct OriginPolicyStore {
    path: PathBuf,
    workspace_layout: ArtifactV2Workspace,
    state: Arc<RwLock<OriginPolicyFile>>,
}

impl OriginPolicyStore {
    pub fn open<P: AsRef<Path>>(base_path: P) -> Self {
        let base_path = base_path.as_ref().to_path_buf();
        let path = base_path.join(ORIGIN_POLICY_FILENAME);
        let workspace_layout = ArtifactV2Workspace::with_local_file_provider(&base_path);

        let mut registry = ORIGIN_POLICY_STATES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (state, first_open) = match registry.get(&path) {
            Some(existing) => (Arc::clone(existing), false),
            None => {
                let created = Arc::new(RwLock::new(OriginPolicyFile::default()));
                registry.insert(path.clone(), Arc::clone(&created));
                (created, true)
            },
        };
        drop(registry);

        let store = Self {
            path,
            workspace_layout,
            state,
        };
        // Only the first handle reads the file. Every later one already holds
        // the authoritative map — each setter persists under the write lock, so
        // memory is never behind disk, and re-reading would be a window in which
        // a concurrent write could be read back over.
        if first_open {
            if let Err(err) = store.load_from_disk() {
                warn!(
                    "[API_MINING] Failed to load origin policy store {:?}: {}",
                    store.path, err
                );
            }
        }
        store
    }

    /// Drop the shared in-memory map for this policy file so the next `open`
    /// reads from disk again. Destructive scope purge must call this after
    /// deleting the policy file; otherwise re-enabling the scope can resurrect
    /// decisions from the process cache. Persistence tests also use it to force
    /// a real disk round trip.
    pub fn forget_shared_state<P: AsRef<Path>>(base_path: P) {
        let path = base_path.as_ref().join(ORIGIN_POLICY_FILENAME);
        if let Some(registry) = ORIGIN_POLICY_STATES.get() {
            registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&path);
        }
    }

    pub fn decision_for_origin(&self, origin_url: &str) -> Option<OriginPolicyDecision> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state.origins.get(origin_url).map(|entry| entry.decision)
    }

    pub fn is_blocked(&self, origin_url: &str) -> bool {
        matches!(
            self.decision_for_origin(origin_url),
            Some(OriginPolicyDecision::Blocked)
        )
    }

    pub fn set_decision(
        &self,
        origin_url: &str,
        decision: OriginPolicyDecision,
    ) -> Result<OriginPolicyEntry, String> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let updated_at = chrono::Utc::now().timestamp();
        let existing_allow_replay = state
            .origins
            .get(origin_url)
            .map(|entry| entry.allow_replay)
            .unwrap_or(false);
        let existing_replay_mode = state.origins.get(origin_url).and_then(|entry| {
            entry
                .replay_mode
                .or_else(|| Some(entry.effective_replay_mode()))
        });
        state.origins.insert(
            origin_url.to_string(),
            StoredOriginPolicyEntry {
                decision,
                updated_at,
                allow_replay: existing_allow_replay,
                replay_mode: existing_replay_mode,
            },
        );
        self.persist_locked(&state)?;
        Ok(OriginPolicyEntry {
            origin_key: CapabilityStore::origin_to_key(origin_url),
            origin_url: origin_url.to_string(),
            decision,
            updated_at,
            allow_replay: existing_allow_replay,
            replay_mode: existing_replay_mode.unwrap_or_default(),
        })
    }

    /// Operator-set: enable or disable inline auto-replay during mining
    /// for `origin_url`. New origins default to `false`; this fn is the
    /// only path to flip them.
    ///
    /// Creates a default `Allowed` decision if no entry exists yet, so
    /// operators don't have to call `set_decision` first. The resulting
    /// entry is returned for the Forge UI to display.
    pub fn set_allow_replay(
        &self,
        origin_url: &str,
        allow_replay: bool,
    ) -> Result<OriginPolicyEntry, String> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let updated_at = chrono::Utc::now().timestamp();
        let decision = state
            .origins
            .get(origin_url)
            .map(|entry| entry.decision)
            .unwrap_or(OriginPolicyDecision::Allowed);
        state.origins.insert(
            origin_url.to_string(),
            StoredOriginPolicyEntry {
                decision,
                updated_at,
                allow_replay,
                replay_mode: Some(if allow_replay {
                    OriginReplayMode::ReplayReads
                } else {
                    OriginReplayMode::ValidateOnly
                }),
            },
        );
        self.persist_locked(&state)?;
        Ok(OriginPolicyEntry {
            origin_key: CapabilityStore::origin_to_key(origin_url),
            origin_url: origin_url.to_string(),
            decision,
            updated_at,
            allow_replay,
            replay_mode: if allow_replay {
                OriginReplayMode::ReplayReads
            } else {
                OriginReplayMode::ValidateOnly
            },
        })
    }

    pub fn set_replay_mode(
        &self,
        origin_url: &str,
        replay_mode: OriginReplayMode,
    ) -> Result<OriginPolicyEntry, String> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let updated_at = chrono::Utc::now().timestamp();
        let decision = state
            .origins
            .get(origin_url)
            .map(|entry| entry.decision)
            .unwrap_or(OriginPolicyDecision::Allowed);
        let allow_replay = replay_mode.allows_inline_auto_replay();
        state.origins.insert(
            origin_url.to_string(),
            StoredOriginPolicyEntry {
                decision,
                updated_at,
                allow_replay,
                replay_mode: Some(replay_mode),
            },
        );
        self.persist_locked(&state)?;
        Ok(OriginPolicyEntry {
            origin_key: CapabilityStore::origin_to_key(origin_url),
            origin_url: origin_url.to_string(),
            decision,
            updated_at,
            allow_replay,
            replay_mode,
        })
    }

    /// Whether inline auto-replay may fire against `origin_url`. False
    /// for unknown origins so a new mining surface cannot trigger
    /// replays until an operator has explicitly opted in.
    pub fn allow_replay_for_origin(&self, origin_url: &str) -> bool {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state
            .origins
            .get(origin_url)
            .map(|entry| entry.allow_replay)
            .unwrap_or(false)
    }

    pub fn allows_passive_validation_for_origin(&self, origin_url: &str) -> bool {
        if self.is_blocked(origin_url) {
            return false;
        }
        !matches!(
            self.replay_mode_for_origin(origin_url),
            OriginReplayMode::ObserveOnly
        )
    }

    /// Live browser-to-API replay defaults to read-only takeover for origins
    /// without a stored policy, preserving existing read replay behavior while
    /// still requiring an explicit policy before write replay can occur.
    pub fn live_replay_mode_for_origin(&self, origin_url: &str) -> OriginReplayMode {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state
            .origins
            .get(origin_url)
            .map(StoredOriginPolicyEntry::effective_replay_mode)
            .unwrap_or(OriginReplayMode::ReplayReads)
    }

    pub fn replay_mode_for_origin(&self, origin_url: &str) -> OriginReplayMode {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state
            .origins
            .get(origin_url)
            .map(StoredOriginPolicyEntry::effective_replay_mode)
            .unwrap_or_default()
    }

    pub fn check_live_replay(
        &self,
        origin_url: &str,
        side_effects: &SideEffects,
        confidence: &ConfidenceLevel,
    ) -> OriginReplayCheck {
        if self.is_blocked(origin_url) {
            return OriginReplayCheck::Denied {
                reason: "origin_policy_blocked".to_string(),
            };
        }
        if *side_effects == SideEffects::Unknown {
            return OriginReplayCheck::Denied {
                reason: "unknown_side_effects".to_string(),
            };
        }

        match self.live_replay_mode_for_origin(origin_url) {
            OriginReplayMode::ObserveOnly => OriginReplayCheck::Denied {
                reason: "origin_replay_mode_observe_only".to_string(),
            },
            OriginReplayMode::ValidateOnly => OriginReplayCheck::Denied {
                reason: "origin_replay_mode_validate_only".to_string(),
            },
            OriginReplayMode::ReplayReads => {
                if *side_effects == SideEffects::ReadOnly {
                    OriginReplayCheck::Allowed
                } else {
                    OriginReplayCheck::Denied {
                        reason: "origin_replay_mode_replay_reads_blocks_write".to_string(),
                    }
                }
            },
            OriginReplayMode::ReplayWritesWithHitl => {
                if *side_effects == SideEffects::ReadOnly {
                    OriginReplayCheck::Allowed
                } else {
                    OriginReplayCheck::RequiresHitl {
                        reason: "write_replay_requires_hitl_approval".to_string(),
                    }
                }
            },
            OriginReplayMode::ReplayTrustedWrites => {
                if *side_effects == SideEffects::ReadOnly
                    || (*side_effects == SideEffects::Write
                        && *confidence == ConfidenceLevel::Trusted)
                {
                    OriginReplayCheck::Allowed
                } else {
                    OriginReplayCheck::RequiresHitl {
                        reason: "write_replay_not_trusted_requires_hitl".to_string(),
                    }
                }
            },
        }
    }

    pub fn list_entries(&self) -> Vec<OriginPolicyEntry> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        let mut entries: Vec<OriginPolicyEntry> = state
            .origins
            .iter()
            .map(|(origin_url, entry)| OriginPolicyEntry {
                origin_key: CapabilityStore::origin_to_key(origin_url),
                origin_url: origin_url.clone(),
                decision: entry.decision,
                updated_at: entry.updated_at,
                allow_replay: entry.allow_replay,
                replay_mode: entry.effective_replay_mode(),
            })
            .collect();
        entries.sort_by(|left, right| left.origin_url.cmp(&right.origin_url));
        entries
    }

    fn load_from_disk(&self) -> Result<(), String> {
        let content = match self.workspace_layout.read_to_string_path_sync(&self.path) {
            Ok(content) => content,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            },
            Err(error) => return Err(format!("Failed to read origin policy file: {}", error)),
        };
        let parsed: OriginPolicyFile = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse origin policy file: {}", e))?;
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        *state = parsed;
        Ok(())
    }

    fn persist_locked(&self, state: &OriginPolicyFile) -> Result<(), String> {
        let content = serde_json::to_string_pretty(state)
            .map_err(|e| format!("Failed to serialize origin policy file: {}", e))?;

        self.workspace_layout
            .write_atomic_path_sync(&self.path, content.as_bytes())
            .map_err(|e| format!("Failed to write origin policy file: {}", e))?;

        Ok(())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn origin_policy_persists_decisions() {
        let temp = TempDir::new().unwrap();
        let store = OriginPolicyStore::open(temp.path());

        let entry = store
            .set_decision("https://example.com", OriginPolicyDecision::Blocked)
            .unwrap();
        assert_eq!(entry.origin_key, "https___example_com");
        assert!(store.is_blocked("https://example.com"));

        // A real reload: without this the shared map answers and the disk
        // round-trip this test is named for never happens.
        OriginPolicyStore::forget_shared_state(temp.path());
        let reloaded = OriginPolicyStore::open(temp.path());
        assert_eq!(
            reloaded.decision_for_origin("https://example.com"),
            Some(OriginPolicyDecision::Blocked)
        );
    }

    #[test]
    fn allow_replay_defaults_to_false_and_persists_when_set() {
        let temp = TempDir::new().unwrap();
        let store = OriginPolicyStore::open(temp.path());

        assert!(!store.allow_replay_for_origin("https://example.com"));

        let entry = store.set_allow_replay("https://example.com", true).unwrap();
        assert!(entry.allow_replay);
        assert_eq!(entry.replay_mode, OriginReplayMode::ReplayReads);
        assert!(store.allow_replay_for_origin("https://example.com"));

        // A real reload: without this the shared map answers and the disk
        // round-trip this test is named for never happens.
        OriginPolicyStore::forget_shared_state(temp.path());
        let reloaded = OriginPolicyStore::open(temp.path());
        assert!(reloaded.allow_replay_for_origin("https://example.com"));
        assert_eq!(
            reloaded.replay_mode_for_origin("https://example.com"),
            OriginReplayMode::ReplayReads
        );
    }

    /// The defect this store had: `open` per request meant a per-request lock
    /// over a per-request copy, so two writers each read, each mutated their own
    /// map, and each wrote the whole map back. The later write reverted the
    /// earlier one — and the earlier one here is an operator blocking an origin,
    /// with `Allowed` the default for anything missing.
    #[test]
    fn a_block_survives_a_concurrent_write_from_another_handle() {
        let temp = TempDir::new().unwrap();
        OriginPolicyStore::forget_shared_state(temp.path());

        // Two handles, opened independently exactly as two requests would.
        let blocking = OriginPolicyStore::open(temp.path());
        let other = OriginPolicyStore::open(temp.path());

        blocking
            .set_decision("https://blocked.example", OriginPolicyDecision::Blocked)
            .unwrap();
        // A write through the other handle, to a different origin. Before the
        // shared map this rewrote the whole file from a copy taken before the
        // block existed, and the block was gone.
        other
            .set_decision("https://other.example", OriginPolicyDecision::Allowed)
            .unwrap();

        assert!(
            other.is_blocked("https://blocked.example"),
            "the second handle must see the first handle's block"
        );

        // And it reached disk, not just the shared map.
        OriginPolicyStore::forget_shared_state(temp.path());
        let reloaded = OriginPolicyStore::open(temp.path());
        assert!(
            reloaded.is_blocked("https://blocked.example"),
            "the block must survive the other handle's rewrite on disk too"
        );
        assert_eq!(
            reloaded.decision_for_origin("https://other.example"),
            Some(OriginPolicyDecision::Allowed),
            "and the other write must not have been lost either"
        );
    }

    #[test]
    fn concurrent_writers_do_not_lose_decisions() {
        use std::sync::Arc as StdArc;
        let temp = TempDir::new().unwrap();
        OriginPolicyStore::forget_shared_state(temp.path());
        let base = StdArc::new(temp.path().to_path_buf());

        let handles: Vec<_> = (0..8)
            .map(|index| {
                let base = StdArc::clone(&base);
                std::thread::spawn(move || {
                    let store = OriginPolicyStore::open(base.as_path());
                    store
                        .set_decision(
                            &format!("https://origin-{index}.example"),
                            OriginPolicyDecision::Blocked,
                        )
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("writer thread");
        }

        OriginPolicyStore::forget_shared_state(temp.path());
        let reloaded = OriginPolicyStore::open(temp.path());
        for index in 0..8 {
            assert!(
                reloaded.is_blocked(&format!("https://origin-{index}.example")),
                "decision {index} was lost"
            );
        }
    }

    #[test]
    fn set_decision_preserves_existing_allow_replay() {
        let temp = TempDir::new().unwrap();
        let store = OriginPolicyStore::open(temp.path());

        store.set_allow_replay("https://example.com", true).unwrap();
        let entry = store
            .set_decision("https://example.com", OriginPolicyDecision::Blocked)
            .unwrap();
        assert!(entry.allow_replay);
        assert_eq!(entry.replay_mode, OriginReplayMode::ReplayReads);
    }

    #[test]
    fn replay_mode_persists_and_drives_auto_replay_bool() {
        let temp = TempDir::new().unwrap();
        let store = OriginPolicyStore::open(temp.path());

        let entry = store
            .set_replay_mode(
                "https://example.com",
                OriginReplayMode::ReplayWritesWithHitl,
            )
            .unwrap();
        assert!(entry.allow_replay);
        assert_eq!(entry.replay_mode, OriginReplayMode::ReplayWritesWithHitl);
        assert_eq!(
            store.replay_mode_for_origin("https://example.com"),
            OriginReplayMode::ReplayWritesWithHitl
        );

        // A real reload: without this the shared map answers and the disk
        // round-trip this test is named for never happens.
        OriginPolicyStore::forget_shared_state(temp.path());
        let reloaded = OriginPolicyStore::open(temp.path());
        assert_eq!(
            reloaded.replay_mode_for_origin("https://example.com"),
            OriginReplayMode::ReplayWritesWithHitl
        );
    }

    #[test]
    fn live_replay_defaults_to_reads_but_stored_default_is_validate_only() {
        let temp = TempDir::new().unwrap();
        let store = OriginPolicyStore::open(temp.path());

        assert_eq!(
            store.replay_mode_for_origin("https://example.com"),
            OriginReplayMode::ValidateOnly
        );
        assert_eq!(
            store.live_replay_mode_for_origin("https://example.com"),
            OriginReplayMode::ReplayReads
        );
    }

    #[test]
    fn write_replay_requires_hitl_unless_origin_allows_trusted_write() {
        let temp = TempDir::new().unwrap();
        let store = OriginPolicyStore::open(temp.path());
        store
            .set_replay_mode(
                "https://example.com",
                OriginReplayMode::ReplayWritesWithHitl,
            )
            .unwrap();

        let hitl = store.check_live_replay(
            "https://example.com",
            &SideEffects::Write,
            &ConfidenceLevel::Validated,
        );
        assert!(matches!(hitl, OriginReplayCheck::RequiresHitl { .. }));

        store
            .set_replay_mode("https://example.com", OriginReplayMode::ReplayTrustedWrites)
            .unwrap();
        let allowed = store.check_live_replay(
            "https://example.com",
            &SideEffects::Write,
            &ConfidenceLevel::Trusted,
        );
        assert_eq!(allowed, OriginReplayCheck::Allowed);
    }
}
