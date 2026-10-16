//! Project the agent's accumulated user-facing knowledge into the feed
//! as `FeedItemType::AgentLearning` cards.
//!
//! ## What this surfaces (vs what it doesn't)
//!
//! The legacy `sync_learning_insight_feed` projection (still alive in
//! `feed_api.rs`, now routed to `/feed` only) reflects the *machinery*
//! of learning — reflection completions, evaluation runs, memory-
//! promotion candidates waiting for review. Operators don't act on
//! "reflection produced 0 candidates" messages and shouldn't see them
//! in Today.
//!
//! This projector reflects the *content* of learning instead. Each
//! card is one substantive thing the agent has accumulated:
//!
//! | Source tier                          | Card shape                                  |
//! |--------------------------------------|----------------------------------------------|
//! | `users/research_findings.json`       | Finding · {topic}: {finding} (legacy fact/claim aliases accepted) |
//! | `users/contacts.json`                | Contact: {name} — {relationship_notes}      |
//! | `users/routines.json`                | Routine · {sublist}: {key_details / freq}   |
//! | `users/knowledge.json :: preferences`| Preference: {key} — {rationale}             |
//! | `users/knowledge.json :: skills`     | Skill: {key} — {rationale}                  |
//! | `users/knowledge.json :: workflows`  | Workflow: {key} — {rationale}               |
//!
//! ## Diff cursor + orphan reconcile (B2/B4 in v0.6.581)
//!
//! Each sync runs as a three-way diff per tier:
//!   * `new_hashes = current_source - previously_seen` → upsert as new feed
//!     cards
//!   * `removed_hashes = previously_seen - current_source` → delete the
//!     corresponding feed cards so entries that get pruned / edited in source
//!     disappear from Today Activity (no more orphan card accumulation)
//!   * `unchanged = intersection` → no-op (avoids `updated_at` bumps that would
//!     otherwise jump steady-state cards to the top of the feed every 15 min)
//!
//! After the sync, `seen_hashes_by_tier` mirrors the current source
//! exactly — no LRU eviction needed (it can't grow beyond the source
//! size). State file lives at
//! `<scope>/memory/feed_projection_state/agent_learnings.json` with a
//! sibling `.lock` for cross-process serialization
//! (`fs2::FileExt::lock_exclusive`).
//!
//! ## Hash stability
//!
//! Card ids are `agent_learning:<tier>:<blake3_hex>`. Same source
//! content → same hash → same id → upsert dedupes. Editing an entry
//! changes the hash → old card gets removed by the orphan reconcile
//! above; new card gets upserted with the new hash.
//!
//! ## Crash-recovery semantics (B12)
//!
//! The sync runs as fire-and-forget under `tokio::spawn` from
//! `FeedApi::sync_learning_feed_best_effort`. If the process is
//! killed mid-sync, the next sync's three-way diff converges
//! correctly without manual intervention. The cases:
//!
//! - **Crash between upsert and orphan-remove:** new cards are already in
//!   DuckDB; state file is unchanged. Next sync sees `previously_seen` without
//!   the new hashes, recomputes `new hashes = current_source - previously_seen`
//!   → upserts again (idempotent — same `id` → upsert dedupes, only
//!   `updated_at` bumps). `removed_hashes` is correctly computed from the old
//!   `previously_seen`, and orphan-removal retries.
//!
//! - **Crash between orphan-remove and save_state:** orphans are already gone
//!   from DuckDB; state file still has their hashes in `previously_seen`. Next
//!   sync computes `removed_hashes` against the (now stale) `previously_seen`,
//!   calls `remove_item` on already-removed ids; `remove_item` returns
//!   `Ok(false)` silently for ids not in the DB → no event emitted, no
//!   double-remove. Recoverable.
//!
//! - **Crash during save_state:** tmp file gets orphaned at
//!   `agent_learnings.json.tmp` but the atomic rename never fires — real state
//!   file unchanged. Next sync proceeds from the pre-crash state; the tmp file
//!   gets overwritten next save.
//!
//! - **Crash during file lock:** OS releases the lock on process exit (fs2
//!   advisory locks). Next sync acquires cleanly. No leaked locks across runs.
//!
//! All branches converge to the correct state within one full sync
//! cycle. No partial-state corruption is possible because the only
//! cross-resource invariant the projector maintains
//! (`feed_items` rows ↔ `seen_hashes`) is verified + repaired on
//! every sync via the diff.

use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    path::Path,
};

use anyhow::{Context, Result};
use blake3::Hasher;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::{
    artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error},
    feed::{FeedItem, FeedItemPatch, FeedItemStatus, FeedItemType, FeedStore},
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

/// Cap on a single entry's serialized `raw_entry` payload bytes after
/// truncation. Over-cap entries get replaced with a structured stub
/// `{"_truncated": true, "size_bytes": N}` so `metadata_json`
/// columns in DuckDB stay bounded. Identifying fields are preserved
/// in the card's top-level `title` + `summary` (set by the extract
/// step), so truncating raw_entry doesn't lose anything
/// frontend-renderable — only the diagnostic payload.
const MAX_RAW_ENTRY_BYTES: usize = 2 * 1024;

/// State sidecar relative path (under `<scope>/memory/`). Keeping it
/// under the memory dir alongside the other projection state
/// (`eval_status/regression_status.json`, etc.) so cleanup tools that
/// purge memory data also clean this.
const STATE_RELATIVE_PATH: &str = "feed_projection_state/agent_learnings.json";
const STATE_LOCK_RELATIVE_PATH: &str = "feed_projection_state/agent_learnings.lock";

/// Per-tier seen-hash map persisted across magician restarts.
///
/// Schema-version bumps are reserved for breaking changes to the
/// state layout. Today's shape (v2): `{tier_name → set of hex
/// blake3 hashes mirroring the current source}`.
///
/// **v1 → v2 migration**: pre-v0.6.581 the value was a `VecDeque<String>`
/// (LRU ordering); we drop ordering and just treat the same array
/// as an unordered set. Existing `schema_version: 1` files
/// deserialize losslessly because both arrays + sets project from
/// the same JSON list.
#[derive(Debug, Serialize, Deserialize, Default)]
struct AgentLearningsState {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    seen_hashes_by_tier: HashMap<String, HashSet<String>>,
    #[serde(default)]
    last_synced_at_ms: i64,
}

fn default_schema_version() -> u32 {
    2
}

#[derive(Debug, Default, Clone)]
pub struct AgentLearningsSyncSummary {
    pub projected_count: usize,
    /// Cards newly upserted into the feed this cycle (entries whose
    /// hash wasn't in the prior seen set).
    pub new_card_count: usize,
    /// Cards removed from the feed this cycle (entries whose hash
    /// was in the prior seen set but no longer in the current
    /// source — orphan reconcile, B2 in v0.6.581).
    pub removed_card_count: usize,
    pub already_seen_count: usize,
    pub per_tier_projected: HashMap<String, usize>,
}

pub struct AgentLearningsProjector<'a> {
    workspace_layout: &'a ArtifactV2Workspace,
    feed_store: &'a FeedStore,
    /// Optional realtime broadcaster. When present, the projector
    /// emits `FeedItemCreated` / `FeedItemUpdated` / `FeedItemRemoved`
    /// events alongside its DuckDB writes so connected frontends
    /// patch the visible feed without waiting for the next HTTP
    /// poll. `None` in tests where the broadcaster isn't wired.
    broadcaster: Option<&'a std::sync::Arc<RuntimeTransportBroadcaster>>,
}

impl<'a> AgentLearningsProjector<'a> {
    pub fn new(workspace_layout: &'a ArtifactV2Workspace, feed_store: &'a FeedStore) -> Self {
        Self {
            workspace_layout,
            feed_store,
            broadcaster: None,
        }
    }

    pub fn with_broadcaster(
        mut self,
        broadcaster: &'a std::sync::Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Mirror of `FeedApi::emit_feed_delta` for projector-driven
    /// writes. When the broadcaster is wired:
    /// - `previous = None` → `FeedItemCreated`
    /// - `previous = Some(prev)` → `FeedItemUpdated` with a diff-only patch
    ///   (skip emit if patch is empty so duplicate syncs don't generate
    ///   empty-payload events).
    fn emit_feed_delta(&self, previous: Option<FeedItem>, item: FeedItem) {
        let Some(broadcaster) = self.broadcaster else {
            return;
        };
        match previous {
            None => {
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemCreated {
                    item,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            },
            Some(prev) => {
                let patch = FeedItemPatch::between(&prev, &item);
                if patch.is_empty() {
                    return;
                }
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemUpdated {
                    principal: item.principal.clone(),
                    workspace: item.workspace.clone(),
                    id: item.id.clone(),
                    task_id: item.task_id.clone(),
                    ui_thread_id: item.ui_thread_id.clone(),
                    execution_id: None,
                    patch,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            },
        }
    }

    fn emit_feed_item_removed(&self, principal: &str, workspace: &str, id: &str) {
        let Some(broadcaster) = self.broadcaster else {
            return;
        };
        broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            id: id.to_string(),
            // Agent learning cards aren't task / thread / execution
            // scoped (they're per-scope knowledge digest items), so
            // these fields stay None — same shape as the legacy
            // `cleanup_stale_candidate_insights` removals.
            task_id: None,
            ui_thread_id: None,
            execution_id: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// **B10 note (intentional non-fix):** This method runs blocking
    /// IO (fs2 file lock acquire, tier JSON reads, state save) in an
    /// async context. We don't wrap in `tokio::task::spawn_blocking`
    /// because (a) the lock must be held across the async upsert /
    /// remove calls in the middle — splitting into spawn_blocking
    /// phases would either drop the lock (defeats B3) or require
    /// `block_on` inside spawn_blocking (deadlock risk), and (b) the
    /// projector runs inside `tokio::spawn` from
    /// `sync_learning_feed_best_effort` so the worker stall is
    /// background-only, never on a user-facing HTTP path. Total
    /// blocking time is bounded to ~50ms uncontended (file lock +
    /// 6 small JSON reads + one atomic state write). Acceptable for
    /// a 15-min-throttled background task.
    pub async fn sync(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<AgentLearningsSyncSummary> {
        let memory_root = self.workspace_layout.memory_root(principal, workspace);
        let users_dir = memory_root.join("users");
        let state_path = memory_root.join(STATE_RELATIVE_PATH);
        let lock_path = memory_root.join(STATE_LOCK_RELATIVE_PATH);

        // B3: cross-process exclusive lock around the
        // load → mutate → save cycle. Without this, two concurrent
        // syncs (possible during the throttle window's startup race)
        // could lose hashes via last-write-wins → re-projection on
        // next sync → cards' `updated_at` bumping unexpectedly.
        let _lock_guard = acquire_state_lock(&lock_path)?;
        let mut state = load_state(&self.workspace_layout, &state_path);
        let mut summary = AgentLearningsSyncSummary::default();

        // Cache parsed tier files within a single sync. `knowledge.json`
        // backs three projections (preferences/skills/workflows); without
        // this cache the file would be opened + parsed three times per
        // sync. Errors are also memoized so a bad file logs once, not
        // once-per-subfield.
        let mut tier_file_cache: HashMap<&'static str, Result<(Option<i64>, Value), String>> =
            HashMap::new();

        for projection in TIER_PROJECTIONS {
            let cached = tier_file_cache
                .entry(projection.relative_path)
                .or_insert_with(|| {
                    let tier_path = users_dir.join(projection.relative_path);
                    read_tier_file(&self.workspace_layout, &tier_path)
                        .map_err(|err| format!("{err:#}"))
                });
            let (tier_last_updated_ms, parsed_value) = match cached {
                Ok(pair) => (pair.0, &pair.1),
                Err(error) => {
                    warn!(
                        target: "feed::agent_learnings_projection",
                        principal,
                        workspace,
                        tier = projection.tier_name,
                        relative_path = projection.relative_path,
                        error = %error,
                        "skipping tier — read/parse failed"
                    );
                    continue;
                },
            };
            let entries = (projection.extract)(parsed_value);

            let previously_seen: HashSet<String> = state
                .seen_hashes_by_tier
                .remove(projection.tier_name)
                .unwrap_or_default();

            // Compute current source hashes once; reuse for both the
            // new-card upsert pass and the orphan-removal pass so we
            // never look at stale data mid-sync.
            let mut current_hashes: HashSet<String> = HashSet::new();
            let mut hash_to_entry: Vec<(String, RawEntry)> = Vec::new();
            for entry in entries {
                let hash = hash_entry(&entry.raw);
                if current_hashes.insert(hash.clone()) {
                    hash_to_entry.push((hash, entry));
                }
            }

            // Upsert NEW entries (in current source but not in
            // prior seen set). Unchanged entries (in both) are
            // skipped to avoid `updated_at` churn.
            let mut per_tier_projected = 0usize;
            for (hash, entry) in &hash_to_entry {
                if previously_seen.contains(hash) {
                    summary.already_seen_count += 1;
                    continue;
                }
                let item = entry.to_feed_item(
                    principal,
                    workspace,
                    projection.tier_name,
                    projection.label,
                    hash,
                    tier_last_updated_ms,
                );
                let item_for_emit = item.clone();
                let previous = self.feed_store.upsert_item(item).await.with_context(|| {
                    format!(
                        "upserting agent_learning card for tier {}",
                        projection.tier_name
                    )
                })?;
                // B8: emit realtime delta so connected Today frontends
                // see the new card without an HTTP poll. Mirrors how
                // sync_learning_insight_feed wires up
                // `FeedApi::emit_feed_delta`. No-op when broadcaster
                // isn't wired (tests).
                self.emit_feed_delta(previous, item_for_emit);
                per_tier_projected += 1;
                summary.projected_count += 1;
                summary.new_card_count += 1;
            }

            // B2/B4 orphan reconcile: anything in previously_seen but
            // NOT in current_hashes is a stale card. The source either
            // pruned it (memory consolidation removed an old finding)
            // or edited it (hash changed → new card exists with new
            // hash → old card is orphan).
            let removed_hashes: Vec<&String> = previously_seen
                .iter()
                .filter(|h| !current_hashes.contains(*h))
                .collect();
            for hash in removed_hashes {
                let stale_id = card_id(projection.tier_name, hash);
                match self
                    .feed_store
                    .remove_item(principal, workspace, &stale_id)
                    .await
                {
                    Ok(true) => {
                        // B8: broadcast removal so connected Today
                        // frontends drop the card without a refresh.
                        self.emit_feed_item_removed(principal, workspace, &stale_id);
                        summary.removed_card_count += 1;
                    },
                    Ok(false) => {
                        // Card already gone from the feed DB; state
                        // file just hadn't caught up. Silent — happens
                        // naturally after a feed DB reset. No event
                        // emit either (nothing to remove from a UI
                        // that doesn't show it).
                    },
                    Err(error) => {
                        warn!(
                            target: "feed::agent_learnings_projection",
                            principal,
                            workspace,
                            tier = projection.tier_name,
                            id = %stale_id,
                            error = %error,
                            "failed to remove orphan agent_learning card; will retry next sync"
                        );
                    },
                }
            }

            // Replace seen set with current source: no LRU needed, the
            // set's size is bounded by the source size itself.
            state
                .seen_hashes_by_tier
                .insert(projection.tier_name.to_string(), current_hashes);
            summary
                .per_tier_projected
                .insert(projection.tier_name.to_string(), per_tier_projected);
        }

        // B11: drop seen-hash entries for tier names no longer in
        // TIER_PROJECTIONS. Prevents unbounded state-file growth if a
        // tier is ever renamed (the old name's entries would
        // otherwise persist forever — never iterated, never removed).
        // Doesn't affect correctness, only state-file size.
        let known_tier_names: HashSet<&'static str> =
            TIER_PROJECTIONS.iter().map(|p| p.tier_name).collect();
        state
            .seen_hashes_by_tier
            .retain(|tier_name, _| known_tier_names.contains(tier_name.as_str()));

        state.last_synced_at_ms = chrono::Utc::now().timestamp_millis();
        state.schema_version = default_schema_version();
        if let Err(error) = save_state(&self.workspace_layout, &state_path, &state) {
            // Persistence failure is non-fatal — next sync will see
            // the same entries unseen and reproject them. Upsert
            // dedupes by id (hash-derived), so the only cost is a
            // wasted DB pass (and a transient `updated_at` bump for
            // existing cards). The orphan-reconcile is unaffected
            // because it computes from previously_seen → current,
            // and previously_seen will be empty next time → no false
            // deletes.
            warn!(
                target: "feed::agent_learnings_projection",
                principal,
                workspace,
                path = %state_path.display(),
                error = %error,
                "failed to persist agent_learnings projection state — next sync will reproject"
            );
        }
        Ok(summary)
    }
}

fn card_id(tier_name: &str, hash: &str) -> String {
    format!("agent_learning:{tier_name}:{hash}")
}

// ─── Tier projection registry ─────────────────────────────────────────

struct TierProjection {
    /// Internal stable name for the state sidecar (don't rename
    /// without a state-schema migration; old hashes would silently
    /// re-fire as new cards and the renamed-tier orphan reconcile
    /// would delete every card under the old name).
    tier_name: &'static str,
    /// Path component under `<scope>/memory/users/`.
    relative_path: &'static str,
    /// Human-readable label shown as the card's eyebrow / category.
    label: &'static str,
    /// Reads the tier file + flattens it into per-entry projections.
    extract: fn(&Value) -> Vec<RawEntry>,
}

const TIER_PROJECTIONS: &[TierProjection] = &[
    TierProjection {
        tier_name: "research_findings",
        relative_path: "research_findings.json",
        label: "Finding",
        extract: extract_research_findings,
    },
    TierProjection {
        tier_name: "contacts",
        relative_path: "contacts.json",
        label: "Contact",
        extract: extract_contacts,
    },
    TierProjection {
        tier_name: "routines",
        relative_path: "routines.json",
        label: "Routine",
        extract: extract_routines,
    },
    TierProjection {
        tier_name: "knowledge_preferences",
        relative_path: "knowledge.json",
        label: "Preference",
        extract: |v| extract_knowledge_subfield(v, "preferences"),
    },
    TierProjection {
        tier_name: "knowledge_skills",
        relative_path: "knowledge.json",
        label: "Skill",
        extract: |v| extract_knowledge_subfield(v, "skills"),
    },
    TierProjection {
        tier_name: "knowledge_workflows",
        relative_path: "knowledge.json",
        label: "Workflow",
        extract: |v| extract_knowledge_subfield(v, "workflows"),
    },
];

struct RawEntry {
    title: String,
    summary: Option<String>,
    raw: Value,
}

impl RawEntry {
    fn to_feed_item(
        &self,
        principal: &str,
        workspace: &str,
        tier_name: &str,
        label: &str,
        hash: &str,
        tier_last_updated_ms: Option<i64>,
    ) -> FeedItem {
        // B6: prefer the tier's `last_updated` timestamp over "now"
        // so first-projection of historical entries lands them with
        // approximately the right age. Upsert preserves `created_at`
        // on subsequent syncs, so even when we fall back to now_ms
        // the value is sticky.
        let now_ms = chrono::Utc::now().timestamp_millis();
        let ts_ms = tier_last_updated_ms.unwrap_or(now_ms);
        let id = card_id(tier_name, hash);
        // B5: bound the diagnostic payload. The card's user-visible
        // title + summary already carry the meaningful content; raw_entry
        // is for debug / future-feature retrieval and shouldn't bloat
        // `feed_items.metadata_json`.
        let raw_payload = cap_raw_entry(&self.raw);
        FeedItem {
            id,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            item_type: FeedItemType::AgentLearning,
            task_id: None,
            ui_thread_id: None,
            agent_id: None,
            title: format!("{label}: {}", truncate(&self.title, 140)),
            summary: self.summary.as_deref().map(|s| truncate(s, 600)),
            status: FeedItemStatus::Info,
            created_at: ts_ms,
            updated_at: ts_ms,
            actions: Vec::new(),
            metadata: json!({
                "card_kind": "agent_learning",
                "tier": tier_name,
                "label": label,
                "raw_entry": raw_payload,
            }),
        }
    }
}

// ─── Tier extractors ──────────────────────────────────────────────────
//
// Each `extract_*` takes the parsed tier `Value` (so `read_tier` can
// read `last_updated` once at the top level then hand the same Value
// down) and returns `Vec<RawEntry>` — flattening per-sublist where
// the tier nests entries under multiple groups (routines).

fn extract_research_findings(value: &Value) -> Vec<RawEntry> {
    value
        .get("fields")
        .and_then(|f| f.get("findings"))
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let topic = string_field(&entry, "topic").unwrap_or_else(|| "Untitled".to_string());
            let claim = [
                "finding",
                "specific_fact_or_claim",
                "fact_or_claim",
                "specific_fact",
                "fact",
                "claim",
            ]
            .into_iter()
            .find_map(|field| string_field(&entry, field))?;
            Some(RawEntry {
                title: topic,
                summary: Some(claim),
                raw: entry,
            })
        })
        .collect()
}

fn extract_contacts(value: &Value) -> Vec<RawEntry> {
    value
        .get("fields")
        .and_then(|f| f.get("entries"))
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let name = string_field(&entry, "name")?;
            let summary = string_field(&entry, "relationship_notes")
                .or_else(|| string_field(&entry, "preferred_communication_channel"));
            Some(RawEntry {
                title: name,
                summary,
                raw: entry,
            })
        })
        .collect()
}

fn extract_routines(value: &Value) -> Vec<RawEntry> {
    let Some(obj) = value.get("fields").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (sublist_name, sublist_value) in obj {
        let Some(arr) = sublist_value.as_array() else {
            continue;
        };
        for entry in arr {
            let frequency = string_field(entry, "frequency").unwrap_or_default();
            let details: String = entry
                .get("key_details")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .unwrap_or_default();
            let title = if details.is_empty() {
                sublist_name.to_string()
            } else {
                format!("{sublist_name} · {}", first_n_words(&details, 8))
            };
            let summary = if frequency.is_empty() && details.is_empty() {
                None
            } else if frequency.is_empty() {
                Some(details)
            } else if details.is_empty() {
                Some(format!("Frequency: {frequency}"))
            } else {
                Some(format!("{frequency} — {details}"))
            };
            let mut raw = entry.clone();
            if let Some(obj) = raw.as_object_mut() {
                obj.insert("_sublist".to_string(), Value::String(sublist_name.clone()));
            }
            out.push(RawEntry {
                title,
                summary,
                raw,
            });
        }
    }
    out
}

fn extract_knowledge_subfield(value: &Value, subfield: &str) -> Vec<RawEntry> {
    value
        .get(subfield)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            let key = string_field(&entry, "key").unwrap_or_else(|| "untitled".to_string());
            let rationale = string_field(&entry, "rationale");
            RawEntry {
                title: humanize_key(&key),
                summary: rationale,
                raw: entry,
            }
        })
        .collect()
}

// ─── Helpers ──────────────────────────────────────────────────────────

/// Read a tier JSON file once; return `(tier_last_updated_ms, value)`.
/// Missing file → `(None, Value::Null)` (extractors treat as empty).
/// Malformed → Err so the caller can log + skip the tier without
/// aborting the whole sync.
///
/// Per-projection extraction happens in the caller so the parsed
/// `Value` can be reused across projections that share a file (e.g.,
/// `knowledge.json` backs preferences/skills/workflows).
fn read_tier_file(
    workspace_layout: &ArtifactV2Workspace,
    path: &Path,
) -> Result<(Option<i64>, Value)> {
    let bytes = match workspace_layout.read_path_sync(path) {
        Ok(b) => b,
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, Value::Null));
        },
        Err(error) => {
            return Err(
                anyhow::Error::from(error).context(format!("reading tier at {}", path.display()))
            );
        },
    };
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing tier JSON at {}", path.display()))?;
    let ts = string_field(&value, "last_updated").and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
    });
    Ok((ts, value))
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn hash_entry(value: &Value) -> String {
    let canonical = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = Hasher::new();
    hasher.update(&canonical);
    let digest = hasher.finalize();
    digest.to_hex().to_string()
}

/// Cap a `raw_entry` JSON payload at [`MAX_RAW_ENTRY_BYTES`]. When
/// the serialized size exceeds the cap, replace the value with a
/// structured stub so downstream consumers can detect truncation
/// (instead of receiving a silently-truncated JSON string).
fn cap_raw_entry(value: &Value) -> Value {
    let serialized = serde_json::to_vec(value).unwrap_or_default();
    if serialized.len() <= MAX_RAW_ENTRY_BYTES {
        return value.clone();
    }
    json!({
        "_truncated": true,
        "size_bytes": serialized.len(),
        "note": format!(
            "raw_entry exceeded {} bytes; original content available in source tier JSON",
            MAX_RAW_ENTRY_BYTES
        ),
    })
}

fn humanize_key(key: &str) -> String {
    key.replace('_', " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn first_n_words(s: &str, n: usize) -> String {
    let words: Vec<&str> = s.split_whitespace().take(n).collect();
    words.join(" ")
}

// ─── State persistence + cross-process lock ───────────────────────────

/// Cross-process exclusive lock around state load → mutate → save.
/// Returned guard releases on drop.
struct StateLockGuard {
    file: File,
}

impl Drop for StateLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_state_lock(lock_path: &Path) -> Result<StateLockGuard> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating state lock dir at {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("opening state lock at {}", lock_path.display()))?;
    // B9: time the lock acquisition. Uncontended this is microseconds
    // (the per-scope 15-min throttle ensures no in-process concurrent
    // syncs); a lock that blocks for >1s indicates either a wedged
    // peer process holding the lock or filesystem latency worth
    // surfacing. Don't fail — just warn so the operator can
    // investigate.
    let lock_start = std::time::Instant::now();
    file.lock_exclusive()
        .with_context(|| format!("acquiring exclusive lock on {}", lock_path.display()))?;
    let lock_elapsed = lock_start.elapsed();
    if lock_elapsed > std::time::Duration::from_secs(1) {
        warn!(
            target: "feed::agent_learnings_projection",
            lock_path = %lock_path.display(),
            elapsed_ms = lock_elapsed.as_millis() as u64,
            "state file lock took >1s to acquire — another sync may be in flight or filesystem under pressure"
        );
    }
    Ok(StateLockGuard { file })
}

fn load_state(workspace_layout: &ArtifactV2Workspace, path: &Path) -> AgentLearningsState {
    match workspace_layout.read_path_sync(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                warn!(
                    target: "feed::agent_learnings_projection",
                    path = %path.display(),
                    error = %error,
                    "agent_learnings state file is malformed — resetting; existing cards stay in feed (dedupe by id)"
                );
                AgentLearningsState::default()
            },
        },
        Err(_) => AgentLearningsState::default(),
    }
}

fn save_state(
    workspace_layout: &ArtifactV2Workspace,
    path: &Path,
    state: &AgentLearningsState,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state).context("serializing agent_learnings state")?;
    workspace_layout
        .write_atomic_path_sync(path, &bytes)
        .with_context(|| format!("writing agent_learnings state at {}", path.display()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn humanize_key_capitalizes_words() {
        assert_eq!(
            humanize_key("ui_test_fallback_technique"),
            "Ui Test Fallback Technique"
        );
        assert_eq!(humanize_key("simple"), "Simple");
        assert_eq!(humanize_key(""), "");
    }

    #[test]
    fn truncate_caps_long_strings() {
        assert_eq!(truncate("short", 100), "short");
        let s = "a".repeat(200);
        let t = truncate(&s, 50);
        assert_eq!(t.chars().count(), 50);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn hash_entry_is_stable_for_same_value() {
        let v = json!({"topic": "foo", "claim": "bar"});
        assert_eq!(hash_entry(&v), hash_entry(&v));
    }

    #[test]
    fn read_tier_missing_returns_empty() {
        let temp_root = std::env::temp_dir();
        let workspace_layout = ArtifactV2Workspace::new(&temp_root);
        let tmp = temp_root.join("agent_learnings_test_missing");
        let _ = std::fs::remove_file(&tmp);
        let (ts, value) = read_tier_file(&workspace_layout, &tmp).unwrap();
        assert!(ts.is_none());
        assert!(extract_research_findings(&value).is_empty());
    }

    #[test]
    fn read_tier_extracts_findings_and_timestamp() {
        let temp_root = std::env::temp_dir();
        let workspace_layout = ArtifactV2Workspace::new(&temp_root);
        let tmp = temp_root.join("agent_learnings_test_findings.json");
        let body = json!({
            "last_updated": "2026-05-18T10:00:00Z",
            "fields": {
                "findings": [
                    {"topic": "T1", "specific_fact_or_claim": "Fact 1", "confidence_level": "high"},
                    {"topic": "T2", "finding": "Fact 2", "confidence": "medium"},
                    {"topic": "T3", "fact": "Fact 3"},
                    {"topic": "T4"} // missing claim — filtered out
                ]
            }
        });
        std::fs::write(&tmp, serde_json::to_vec(&body).unwrap()).unwrap();
        let (ts, value) = read_tier_file(&workspace_layout, &tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(ts.is_some());
        let entries = extract_research_findings(&value);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].title, "T1");
        assert_eq!(entries[0].summary.as_deref(), Some("Fact 1"));
        assert_eq!(entries[1].summary.as_deref(), Some("Fact 2"));
        assert_eq!(entries[2].summary.as_deref(), Some("Fact 3"));
    }

    #[test]
    fn knowledge_subfields_can_share_a_parsed_value() {
        // Mirrors the per-sync cache: parse knowledge.json once, run
        // the three subfield extractors against the same Value. Guards
        // against any future refactor reintroducing per-projection file
        // reads.
        let body = json!({
            "preferences": [{"key": "tone", "rationale": "concise"}],
            "skills": [{"key": "rust", "rationale": "primary stack"}],
            "workflows": [{"key": "tdd", "rationale": "happy-path first"}],
        });
        let prefs = extract_knowledge_subfield(&body, "preferences");
        let skills = extract_knowledge_subfield(&body, "skills");
        let workflows = extract_knowledge_subfield(&body, "workflows");
        assert_eq!(prefs.len(), 1);
        assert_eq!(skills.len(), 1);
        assert_eq!(workflows.len(), 1);
        assert_eq!(prefs[0].title, "Tone");
        assert_eq!(skills[0].title, "Rust");
        assert_eq!(workflows[0].title, "Tdd");
    }

    #[test]
    fn cap_raw_entry_keeps_small_values_intact() {
        let v = json!({"topic": "foo"});
        let capped = cap_raw_entry(&v);
        assert_eq!(capped, v);
    }

    #[test]
    fn cap_raw_entry_replaces_oversized_with_stub() {
        let big_str = "x".repeat(MAX_RAW_ENTRY_BYTES * 2);
        let v = json!({"rationale": big_str});
        let capped = cap_raw_entry(&v);
        assert_eq!(
            capped.get("_truncated").and_then(Value::as_bool),
            Some(true)
        );
        assert!(
            capped
                .get("size_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > MAX_RAW_ENTRY_BYTES as u64
        );
    }

    #[test]
    fn state_v1_schema_deserializes_into_v2_shape() {
        // Pre-v0.6.581 stored seen_hashes_by_tier as `VecDeque<String>`
        // (JSON array). v2 stores it as `HashSet<String>` (same JSON
        // array). Same bytes deserialize cleanly into either Rust type.
        let v1 = r#"{
            "schema_version": 1,
            "seen_hashes_by_tier": {
                "research_findings": ["abc", "def"]
            },
            "last_synced_at_ms": 12345
        }"#;
        let state: AgentLearningsState = serde_json::from_str(v1).unwrap();
        assert_eq!(state.schema_version, 1);
        let hashes = state.seen_hashes_by_tier.get("research_findings").unwrap();
        assert!(hashes.contains("abc"));
        assert!(hashes.contains("def"));
    }
}
