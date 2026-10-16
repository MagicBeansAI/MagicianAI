//! Bot-auth → canonical HITL transition broker.
//!
//! Bot adapters (gmail/whatsapp/telegram/kapso) write a
//! `BotNeedsAuthSidecar` JSON file when they can't authenticate. The bot
//! auth probe (`auth_snapshot`) reads that sidecar back as
//! `BotAuthStatus::NeedsAuth` or `AccountMismatch`. Pre-consolidation
//! the frontend synthesized FeedItems from that state and dropped them
//! into the AttentionBar's `escalations` bucket — outside the canonical
//! HITL pipeline.
//!
//! This broker bridges the gap: every call to the bot-auth list endpoint
//! records each snapshot through [`AuthHitlBroker::record_snapshot`],
//! which compares the bot's status to the last-known cache and emits:
//!
//! - `RuntimeTransportEvent::HitlRequested { source: "bot_auth",
//!   correlation_id: "bot_auth:<principal>:<workspace>:<bot>" }` when a
//!   bot transitions *into* a needs-attention state (NeedsAuth /
//!   AccountMismatch).
//! - `RuntimeTransportEvent::HitlResolved { source: "bot_auth",
//!   outcome: "responded" }` when a bot transitions *away from*
//!   needs-attention (typically Ok or Unsupported).
//!
//! The events flow through the canonical pipeline → V3 attention
//! summary → `feed_api.rs::list_v3_attention_items` → AttentionBar's
//! Requests bucket. Click → `AttentionPromptModal` opens with the
//! `external_action` input shape (operator triggers the auth flow,
//! then clicks "I've signed in" to re-probe; see
//! `web_api.rs::respond_hitl_handler` :: "bot_auth" arm).
//!
//! ## Why a broker (not per-emit at the bot subsystem level)?
//!
//! Plumbing `RuntimeTransportBroadcaster` into `BotManager` and every
//! `ManagedBot` would touch dozens of constructors + tests. The bot
//! auth list endpoint is polled every 15s from every connected
//! frontend, so transitions surface within 15s either way — colocating
//! the comparator with the polling endpoint is simpler and equally
//! correct.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, warn};

use magician::magician_v2::{
    artifact_v2::{io::write_bytes_durably_sync, workspace::ArtifactV2Workspace, ArtifactV2Error},
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

use super::{BotAuthSnapshot, BotAuthStatus};

/// Per-process cache of the last-known bot-auth status, keyed by
/// `(principal, workspace, bot_name)`. Used by [`AuthHitlBroker`] to
/// detect transitions across successive snapshot observations.
type LastKnownMap = HashMap<(String, String, String), BotAuthStatus>;

/// Bot-auth → canonical HITL transition broker. Holds a shared
/// broadcaster handle plus a per-process status cache.
///
/// Thread-safe: the inner map is behind an `RwLock`; the broadcaster is
/// already `Send + Sync` via `Arc`. Cloning the broker is cheap (Arc
/// bumps); the cache is shared across clones so transition detection
/// stays coherent regardless of which clone records first.
///
/// Optionally persists the cache to disk via `with_cache_persist_path`
/// so transitions are correctly classified across magician restarts.
/// Without persistence, every restart drops the cache and depends on
/// the defensive first-observation-healthy-bot fallback in
/// `classify_transition` to clear stale HitlRequested rows.
#[derive(Clone)]
pub struct AuthHitlBroker {
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    last_known: Arc<RwLock<LastKnownMap>>,
    cache_persist_path: Option<Arc<PathBuf>>,
    cache_workspace_layout: Option<Arc<ArtifactV2Workspace>>,
}

/// Serializable wire form for one cache entry. The in-memory map is
/// keyed by a tuple, but JSON doesn't allow tuple keys, so we
/// serialize as an array of these structs.
#[derive(Serialize, Deserialize)]
struct CacheEntryDisk {
    principal: String,
    workspace: String,
    bot: String,
    status: BotAuthStatus,
}

impl AuthHitlBroker {
    pub fn new(broadcaster: Arc<RuntimeTransportBroadcaster>) -> Self {
        Self {
            broadcaster,
            last_known: Arc::new(RwLock::new(HashMap::new())),
            cache_persist_path: None,
            cache_workspace_layout: None,
        }
    }

    /// Load the persisted cache from `path` (if it exists) and route
    /// future cache updates back to the same file. Idempotent: multiple
    /// calls overwrite the previously-set path; the in-memory cache is
    /// repopulated on each call to reflect the latest disk state.
    ///
    /// Cache writes happen synchronously on the calling thread after
    /// each `record_snapshot` that mutates the map. The file is tiny
    /// (one entry per bot per scope; typical magnitude single digits),
    /// so the IO cost per write is negligible. Write uses temp-file +
    /// atomic rename so a crash mid-write can't leave a partial file.
    pub fn with_cache_persist_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let restored = load_cache(&path, None);
        self.restore_cache_from_disk(&path, restored);
        self.cache_persist_path = Some(Arc::new(path));
        self
    }

    pub fn with_cache_persist_path_in_workspace(
        mut self,
        path: impl Into<PathBuf>,
        workspace_layout: ArtifactV2Workspace,
    ) -> Self {
        let path = path.into();
        let workspace_layout = Arc::new(workspace_layout);
        let restored = load_cache(&path, Some(workspace_layout.as_ref()));
        self.restore_cache_from_disk(&path, restored);
        self.cache_persist_path = Some(Arc::new(path));
        self.cache_workspace_layout = Some(workspace_layout);
        self
    }

    fn restore_cache_from_disk(&self, path: &Path, restored: LastKnownMap) {
        if !restored.is_empty() {
            let restored_count = restored.len();
            if let Ok(mut guard) = self.last_known.write() {
                *guard = restored;
            }
            debug!(
                count = restored_count,
                path = %path.display(),
                "[AUTH-HITL-BROKER] Restored last-known cache from disk"
            );
        }
    }

    /// Stable correlation id for a bot's auth HITL. Reused across
    /// re-emissions so dedup at the frontend (`pendingHitlStore` keys
    /// by `correlation_id`) collapses repeated polls into one row.
    pub fn correlation_id(principal: &str, workspace: &str, bot: &str) -> String {
        format!("bot_auth:{principal}:{workspace}:{bot}")
    }

    /// Record an observation; emit a canonical HITL event iff the
    /// status transitioned.
    ///
    /// Transition matrix:
    ///   - prev = None | Ok | Unsupported | Error  AND
    ///     curr = NeedsAuth | AccountMismatch
    ///     → emit `HitlRequested`
    ///   - prev = NeedsAuth | AccountMismatch  AND
    ///     curr = Ok | Unsupported
    ///     → emit `HitlResolved { outcome: "responded" }`
    ///   - prev = NeedsAuth | AccountMismatch  AND
    ///     curr = Error
    ///     → no emit (probe is in error state; preserve the prior
    ///     needs-auth row so the operator still sees the row instead
    ///     of having it silently disappear into a transient probe
    ///     failure). The next successful snapshot will transition
    ///     correctly.
    ///   - everything else → no emit
    pub fn record_snapshot(&self, principal: &str, workspace: &str, snapshot: &BotAuthSnapshot) {
        let key = (
            principal.to_string(),
            workspace.to_string(),
            snapshot.name.clone(),
        );
        let curr = snapshot.status.clone();
        let prev = {
            let guard = self.last_known.read().unwrap_or_else(|e| e.into_inner());
            guard.get(&key).cloned()
        };

        let transition = classify_transition(prev.as_ref(), &curr);

        // Update cache eagerly even when no emit fires — keeps the
        // transition matrix monotonic (a no-emit observation still
        // moves the baseline forward so the *next* observation sees
        // the right `prev`).
        let cache_changed = match &prev {
            Some(existing) => existing != &curr,
            None => true,
        };
        if let Ok(mut guard) = self.last_known.write() {
            guard.insert(key.clone(), curr.clone());
            // Persist only when the cache state actually changed — the
            // 15s poll cadence per bot would otherwise churn the file
            // every tick.
            if cache_changed {
                if let Some(path) = self.cache_persist_path.as_deref() {
                    persist_cache(path, self.cache_workspace_layout.as_deref(), &guard);
                }
            }
        }

        match transition {
            Transition::Enter => self.emit_requested(principal, workspace, snapshot),
            Transition::Exit => self.emit_resolved(principal, workspace, snapshot),
            Transition::None => {},
        }
    }

    fn emit_requested(&self, principal: &str, workspace: &str, snapshot: &BotAuthSnapshot) {
        let correlation_id = Self::correlation_id(principal, workspace, &snapshot.name);
        let prompt = build_prompt(snapshot);
        let hint = snapshot.detail.clone();
        let input_schema = build_input_schema(principal, workspace, snapshot);
        self.broadcaster.emit(RuntimeTransportEvent::HitlRequested {
            correlation_id,
            source: "bot_auth".to_string(),
            // `choice` (not `external_action`) so the response value
            // carries the operator's `selected_id` (`open_auth_flow` /
            // `recheck` / `dismiss`) end-to-end — the
            // `external_action_completed` value drops it on the
            // frontend adapter, blinding the backend dispatcher.
            input_type: "choice".to_string(),
            prompt,
            hint,
            input_schema: Some(input_schema),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    fn emit_resolved(&self, principal: &str, workspace: &str, snapshot: &BotAuthSnapshot) {
        let correlation_id = Self::correlation_id(principal, workspace, &snapshot.name);
        self.broadcaster.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id,
            source: "bot_auth".to_string(),
            outcome: "responded".to_string(),
            decision: Some("authenticated".to_string()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Emit a `HitlResolved { outcome: "dismissed" }` for an operator
    /// who clicked "dismiss" on a bot_auth row without completing auth.
    /// Called from the `respond_hitl_handler :: "bot_auth"` arm. Does
    /// NOT update the per-process status cache: the sidecar is still
    /// present, so on the next `list_bots_auth` poll the broker
    /// observes the same (NeedsAuth → NeedsAuth) steady-state and
    /// silently keeps it that way. The row will only re-surface if the
    /// underlying bot status changes (e.g., a transient probe error
    /// followed by needs_auth) so dismiss feels durable to the
    /// operator until something actually changes.
    pub fn emit_dismissed(&self, principal: &str, workspace: &str, bot_name: &str) {
        let correlation_id = Self::correlation_id(principal, workspace, bot_name);
        self.broadcaster.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id,
            source: "bot_auth".to_string(),
            outcome: "dismissed".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Test-only: forcibly clear the per-process cache so successive
    /// tests don't see leftover transitions from an earlier case.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn reset_for_test(&self) {
        if let Ok(mut guard) = self.last_known.write() {
            guard.clear();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transition {
    /// Entering a needs-attention state from anything else.
    Enter,
    /// Leaving a needs-attention state into Ok / Unsupported. Error
    /// states do NOT trigger Exit because they're transient probe
    /// failures, not operator-confirmed auth completions.
    Exit,
    None,
}

fn needs_attention(status: &BotAuthStatus) -> bool {
    matches!(
        status,
        BotAuthStatus::NeedsAuth | BotAuthStatus::AccountMismatch
    )
}

fn classify_transition(prev: Option<&BotAuthStatus>, curr: &BotAuthStatus) -> Transition {
    let prev_needs = prev.map(needs_attention).unwrap_or(false);
    let curr_needs = needs_attention(curr);
    if !prev_needs && curr_needs {
        return Transition::Enter;
    }
    if prev_needs && !curr_needs {
        // Treat probe errors as "keep the row visible" — don't exit.
        if matches!(curr, BotAuthStatus::Error) {
            return Transition::None;
        }
        return Transition::Exit;
    }
    // Defensive: first observation per process where the bot is
    // already healthy. Two scenarios collapse here —
    //   (a) magician just started and the bot was never in NeedsAuth,
    //   (b) magician restarted and the operator completed sign-in
    //       *during downtime* (or even just before this poll), so
    //       there's still a stale `HitlRequested` in events.jsonl
    //       that the attention surface keeps surfacing forever
    //       because no `HitlResolved` was ever emitted to pair with
    //       it (the in-memory cache that would have tracked the
    //       NeedsAuth → Ok transition died with the prior process).
    // Emitting `HitlResolved` defensively handles (b) cleanly. The
    // (a) case generates a single per-restart `HitlResolved` per
    // healthy bot — the attention surface dedups by `correlation_id`,
    // so a HitlResolved with no matching HitlRequested is silently
    // ignored. Worth the tiny extra event-log noise to never leave a
    // bot-auth row stuck across a restart.
    if prev.is_none() && matches!(curr, BotAuthStatus::Ok | BotAuthStatus::Unsupported) {
        return Transition::Exit;
    }
    Transition::None
}

// Per-scope cache sharding: bot-auth HITL state lives under each scope at
// `scopes/<principal>/<workspace>/requests/<file>` instead of one file at the
// store root. Load merges every shard plus the legacy single-file location (the
// map key (principal, workspace, bot) is the natural dedup); persist groups by
// scope, rewrites every scope that has/had a shard (so a removed entry can't
// resurrect), and migrates the legacy file away.

fn cache_shard_file_name(legacy_path: &Path) -> String {
    legacy_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bot_auth_hitl_cache.json")
        .to_string()
}

fn cache_shard_paths(
    legacy_path: &Path,
    workspace_layout: Option<&ArtifactV2Workspace>,
) -> Vec<PathBuf> {
    let mut shards = Vec::new();
    if let Some(layout) = workspace_layout {
        let file_name = cache_shard_file_name(legacy_path);
        for (principal, workspace) in layout.list_scopes() {
            shards.push(layout.scope_requests_path(&principal, &workspace, &file_name));
        }
    }
    shards.push(legacy_path.to_path_buf());
    shards
}

fn read_cache_entries(
    path: &Path,
    workspace_layout: Option<&ArtifactV2Workspace>,
) -> Vec<CacheEntryDisk> {
    let bytes = match read_cache_bytes(path, workspace_layout) {
        Ok(b) => b,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            warn!(path = %path.display(), error = %error, "[AUTH-HITL-BROKER] Failed to read cache shard");
            return Vec::new();
        },
    };
    match serde_json::from_slice(&bytes) {
        Ok(entries) => entries,
        Err(error) => {
            warn!(path = %path.display(), error = %error, "[AUTH-HITL-BROKER] Failed to parse cache shard; dropping it");
            Vec::new()
        },
    }
}

fn load_cache(path: &Path, workspace_layout: Option<&ArtifactV2Workspace>) -> LastKnownMap {
    let mut map: LastKnownMap = HashMap::new();
    for shard in cache_shard_paths(path, workspace_layout) {
        for entry in read_cache_entries(&shard, workspace_layout) {
            map.insert((entry.principal, entry.workspace, entry.bot), entry.status);
        }
    }
    map
}

fn persist_cache(path: &Path, workspace_layout: Option<&ArtifactV2Workspace>, map: &LastKnownMap) {
    let Some(layout) = workspace_layout else {
        // No provider (tests): legacy single-file write.
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                warn!(path = %path.display(), error = %error, "[AUTH-HITL-BROKER] Failed to create cache directory");
                return;
            }
        }
        let entries: Vec<CacheEntryDisk> = map
            .iter()
            .map(|((principal, workspace, bot), status)| CacheEntryDisk {
                principal: principal.clone(),
                workspace: workspace.clone(),
                bot: bot.clone(),
                status: status.clone(),
            })
            .collect();
        write_cache_entries_legacy(path, &entries);
        return;
    };
    let file_name = cache_shard_file_name(path);
    let mut by_scope: HashMap<(String, String), Vec<CacheEntryDisk>> = HashMap::new();
    for ((principal, workspace, bot), status) in map.iter() {
        by_scope
            .entry((principal.clone(), workspace.clone()))
            .or_default()
            .push(CacheEntryDisk {
                principal: principal.clone(),
                workspace: workspace.clone(),
                bot: bot.clone(),
                status: status.clone(),
            });
    }
    let mut scopes: std::collections::HashSet<(String, String)> =
        by_scope.keys().cloned().collect();
    scopes.extend(layout.scopes_with_request_shard(&file_name));
    for (principal, workspace) in scopes {
        let shard = layout.scope_requests_path(&principal, &workspace, &file_name);
        let entries = by_scope.remove(&(principal, workspace)).unwrap_or_default();
        if let Err(error) = layout
            .write_json_atomic_path_sync(&shard, &entries)
            .map_err(artifact_v2_error_to_io)
        {
            warn!(path = %shard.display(), error = %error, "[AUTH-HITL-BROKER] Failed to persist cache shard");
        }
    }
    // Migrate away the legacy single-file location once shards are written.
    let _ = layout.remove_file_path_sync(path);
}

/// Single-file cache write for the no-workspace-layout path.
///
/// Publishes through the shared durable writer: the previous form staged under
/// a fixed `<file>.tmp` shared by every writer of this path and fsynced
/// nothing. The helper also removes its own staging file on failure, so the
/// explicit cleanup is gone.
fn write_cache_entries_legacy(path: &Path, entries: &[CacheEntryDisk]) {
    let bytes = match serde_json::to_vec_pretty(entries) {
        Ok(b) => b,
        Err(error) => {
            warn!(path = %path.display(), error = %error, "[AUTH-HITL-BROKER] Failed to serialize cache");
            return;
        },
    };
    if let Err(error) = write_bytes_durably_sync(path, &bytes) {
        warn!(path = %path.display(), error = %error, "[AUTH-HITL-BROKER] Failed to persist cache");
    }
}

fn read_cache_bytes(
    path: &Path,
    workspace_layout: Option<&ArtifactV2Workspace>,
) -> std::io::Result<Vec<u8>> {
    if let Some(layout) = workspace_layout {
        return layout.read_path_sync(path).map_err(artifact_v2_error_to_io);
    }
    fs::read(path)
}

fn artifact_v2_error_to_io(error: ArtifactV2Error) -> std::io::Error {
    match error {
        ArtifactV2Error::Io(error) => error,
        other => std::io::Error::other(other),
    }
}

fn build_prompt(snapshot: &BotAuthSnapshot) -> String {
    let provider_part = snapshot
        .profile_label
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| format!(" · {s}"))
        .unwrap_or_default();
    match snapshot.status {
        BotAuthStatus::NeedsAuth => {
            format!("Sign in required: {}{provider_part}", snapshot.name)
        },
        BotAuthStatus::AccountMismatch => {
            let mismatch = match (
                snapshot.expected_account.as_deref(),
                snapshot.current_account.as_deref(),
            ) {
                (Some(expected), Some(current)) => {
                    format!(" (expected {expected}, signed in as {current})")
                },
                (Some(expected), None) => format!(" (expected {expected})"),
                _ => String::new(),
            };
            format!(
                "Account mismatch for {}{provider_part}{mismatch}",
                snapshot.name
            )
        },
        _ => {
            // Should not be called for non-needs states, but keep a
            // sane fallback so a future regression doesn't panic.
            warn!(
                bot = %snapshot.name,
                "build_prompt called for non-needs status — emitting generic prompt"
            );
            format!("Bot auth attention needed: {}", snapshot.name)
        },
    }
}

fn build_input_schema(
    principal: &str,
    workspace: &str,
    snapshot: &BotAuthSnapshot,
) -> serde_json::Value {
    // `external_action` semantics: the operator triggers the auth flow
    // out-of-band (browser OAuth, QR scan, etc.) and clicks one of the
    // surfaced options to tell magician what happened. The dispatcher
    // arm in `web_api.rs::respond_hitl_handler` :: "bot_auth" reads
    // `selected_id` and acts accordingly.
    let options = serde_json::json!([
        {
            "id": "open_auth_flow",
            "label": "Open auth flow",
            "description": "Trigger the bot's start_auth endpoint (opens browser / QR / OAuth as the provider requires)",
            "requires_input": false,
        },
        {
            "id": "recheck",
            "label": "I've signed in — re-check",
            "description": "Re-probe the bot's auth status. Resolves the row if probe passes; keeps it open with the new error otherwise.",
            "requires_input": false,
        },
        {
            "id": "dismiss",
            "label": "Dismiss",
            "description": "Hide this row without re-probing (it will re-surface on the next poll if the bot still needs auth).",
            "requires_input": false,
        },
    ]);
    json!({
        "type": "choice",
        "bot_name": snapshot.name,
        "bot_provider": snapshot.provider,
        "bot_profile_label": snapshot.profile_label,
        "bot_expected_account": snapshot.expected_account,
        "bot_current_account": snapshot.current_account,
        "bot_status": serde_yaml::to_value(&snapshot.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", snapshot.status).to_lowercase()),
        "principal": principal,
        "workspace": workspace,
        "options": options,
        // Surface `instructions` so the modal's external_action default
        // multiline path renders something sensible if a future client
        // sees the request before the options-aware code path lands.
        "instructions": "Click \"Open auth flow\" to start the sign-in. Once you've completed it, click \"I've signed in\" so the backend rechecks the bot connection.",
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::bots::BotAuthFlowState;

    fn snapshot(name: &str, status: BotAuthStatus) -> BotAuthSnapshot {
        BotAuthSnapshot {
            name: name.to_string(),
            supported: true,
            provider: Some("google_workspace".to_string()),
            status,
            flow_state: BotAuthFlowState::Idle,
            profile_label: Some("primary".to_string()),
            expected_account: Some("a@b.com".to_string()),
            current_account: None,
            detail: None,
        }
    }

    #[test]
    fn classify_transition_into_needs_auth_emits_enter() {
        assert_eq!(
            classify_transition(None, &BotAuthStatus::NeedsAuth),
            Transition::Enter
        );
        assert_eq!(
            classify_transition(Some(&BotAuthStatus::Ok), &BotAuthStatus::AccountMismatch),
            Transition::Enter
        );
    }

    #[test]
    fn classify_transition_away_from_needs_auth_emits_exit() {
        assert_eq!(
            classify_transition(Some(&BotAuthStatus::NeedsAuth), &BotAuthStatus::Ok),
            Transition::Exit
        );
        assert_eq!(
            classify_transition(
                Some(&BotAuthStatus::AccountMismatch),
                &BotAuthStatus::Unsupported
            ),
            Transition::Exit
        );
    }

    #[test]
    fn classify_transition_probe_error_does_not_resolve() {
        // Error is treated as transient — keep the row visible.
        assert_eq!(
            classify_transition(Some(&BotAuthStatus::NeedsAuth), &BotAuthStatus::Error),
            Transition::None
        );
    }

    #[test]
    fn classify_transition_steady_state_does_not_emit() {
        assert_eq!(
            classify_transition(Some(&BotAuthStatus::NeedsAuth), &BotAuthStatus::NeedsAuth),
            Transition::None
        );
        assert_eq!(
            classify_transition(Some(&BotAuthStatus::Ok), &BotAuthStatus::Ok),
            Transition::None
        );
    }

    #[test]
    fn classify_transition_first_observation_healthy_emits_exit() {
        // Defensive: process restarted between a NeedsAuth → Ok
        // transition. The in-memory cache lost the prev=NeedsAuth
        // signal, but events.jsonl still has a stale HitlRequested
        // that needs clearing. Emit Exit so the row clears.
        assert_eq!(
            classify_transition(None, &BotAuthStatus::Ok),
            Transition::Exit
        );
        assert_eq!(
            classify_transition(None, &BotAuthStatus::Unsupported),
            Transition::Exit
        );
    }

    #[test]
    fn classify_transition_first_observation_error_does_not_exit() {
        // Error is transient — don't emit Exit even on first
        // observation. Next successful probe will produce the right
        // transition.
        assert_eq!(
            classify_transition(None, &BotAuthStatus::Error),
            Transition::None
        );
    }

    #[test]
    fn correlation_id_is_stable_per_bot() {
        let id1 = AuthHitlBroker::correlation_id("alice", "default", "gmail");
        let id2 = AuthHitlBroker::correlation_id("alice", "default", "gmail");
        assert_eq!(id1, id2);
        assert_eq!(id1, "bot_auth:alice:default:gmail");
    }

    #[tokio::test]
    async fn record_snapshot_emits_on_transition() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(32));
        let mut rx = broadcaster.subscribe();
        let broker = AuthHitlBroker::new(Arc::clone(&broadcaster));

        // First needs_auth observation should emit Requested.
        broker.record_snapshot(
            "alice",
            "default",
            &snapshot("gmail", BotAuthStatus::NeedsAuth),
        );
        let event = rx.recv().await.expect("recv first event");
        assert!(matches!(event, RuntimeTransportEvent::HitlRequested { .. }));

        // Same needs_auth — no emit.
        broker.record_snapshot(
            "alice",
            "default",
            &snapshot("gmail", BotAuthStatus::NeedsAuth),
        );
        let try_recv = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
        assert!(try_recv.is_err(), "steady state should not emit");

        // Transition to Ok — emit Resolved.
        broker.record_snapshot("alice", "default", &snapshot("gmail", BotAuthStatus::Ok));
        let event = rx.recv().await.expect("recv resolved event");
        assert!(matches!(event, RuntimeTransportEvent::HitlResolved { .. }));
    }

    /// The legacy single-file cache write must leave a parseable file and no
    /// staging file. Only the no-workspace-layout path reaches
    /// `write_cache_entries_legacy`; production goes through the layout's
    /// atomic writer instead.
    #[tokio::test]
    async fn legacy_cache_write_leaves_no_staging_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache_path = dir.path().join("bot_auth_cache.json");

        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(32));
        let broker = AuthHitlBroker::new(broadcaster).with_cache_persist_path(cache_path.clone());

        broker.record_snapshot(
            "alice",
            "default",
            &snapshot("gmail", BotAuthStatus::NeedsAuth),
        );
        broker.record_snapshot(
            "alice",
            "default",
            &snapshot("telegram", BotAuthStatus::NeedsAuth),
        );

        let staging: Vec<String> = fs::read_dir(dir.path())
            .expect("cache directory listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            staging.is_empty(),
            "durable writes must leave no staging file, found {staging:?}"
        );

        let published = fs::read(&cache_path).expect("cache file should be readable");
        let parsed: Vec<CacheEntryDisk> =
            serde_json::from_slice(&published).expect("published cache should parse");
        assert_eq!(parsed.len(), 2);
    }
}
