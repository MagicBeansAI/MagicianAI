//! Live Thinking Map — durable, scope-owned persistent store (Phase 2a).
//!
//! Mirrors the scoped-storage stack (`learning/store.rs` +
//! [`ArtifactV2Workspace`]): per-map scoped directories, atomic snapshot +
//! manifest writes, an append-only authoritative event log, and graceful
//! missing-file handling. Nothing here is wired into the runtime — this is a
//! dormant library type only.
//!
//! ## On-disk layout
//! ```text
//! scopes/<principal>/<workspace>/thinking_maps/<map_id>/
//!   manifest.json         # atomic — lifecycle/title/source + latest_* + timestamps
//!   snapshot.json         # atomic — the full current ThinkingMap
//!   events.jsonl          # append-only — one MapEvent per applied envelope
//!   utterance_refs.jsonl  # append-only — RESERVED (Phase 4 populates)
//!   exports/              # RESERVED dir (Phase 9)
//! ```
//!
//! ## Crash-safety ordering
//! [`ThinkingMapStore::apply_and_persist`] appends + fsyncs the event to
//! `events.jsonl` FIRST, then materializes `snapshot.json`, then `manifest.json`.
//! The durable event log always leads the snapshot: a crash between the event
//! append and the snapshot write leaves an event whose `resulting_revision`
//! exceeds the (older) snapshot — recoverable by replay — never the reverse.
//!
//! ## Concurrency
//! A process-wide keyed async lock over normalized storage root + scope + map id
//! serializes the load→reduce→persist critical section even when handlers build
//! separate store values. Different maps hold different locks and proceed
//! concurrently.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::ArtifactV2Error;
use magician::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;

use super::errors::ThinkingMapError;
use super::models::{
    AssertionOrigin, EpistemicState, MapLifecycle, NodeId, NodeKind, ThinkingMap, ThinkingMapSource,
};
use super::operations::MapOperationEnvelope;
use super::reducer::{apply_envelope, semantic_hash, ApplyOutcome};

/// Current on-disk schema version for the store's manifest.
pub const THINKING_MAP_STORE_SCHEMA_VERSION: u32 = 1;

// ── Error taxonomy ───────────────────────────────────────────────────────────

/// Errors returned by [`ThinkingMapStore`].
#[derive(Debug, thiserror::Error)]
pub enum ThinkingMapStoreError {
    /// Underlying workspace/filesystem I/O or serialization failure.
    #[error("workspace error: {0}")]
    Io(#[from] ArtifactV2Error),

    /// The reducer rejected the envelope. NOT `#[from]` — persistence must map
    /// reducer rejections explicitly so a validation failure never writes.
    #[error("validation error: {0}")]
    Validation(ThinkingMapError),

    /// No map with the requested id exists in the scope.
    #[error("thinking map not found: {0}")]
    NotFound(String),

    /// `create_map` for a map id whose manifest already exists.
    #[error("thinking map already exists: {0}")]
    AlreadyExists(String),

    /// Permanent deletion is only valid for an existing soft-deleted map.
    #[error("thinking map must be soft-deleted before permanent deletion: {0}")]
    PurgeRequiresDeleted(String),

    /// A `principal`, `workspace`, or `map_id` failed the scope-id safety check.
    #[error("invalid id: {0}")]
    InvalidId(String),

    /// An on-disk artifact could not be deserialized (truncated/corrupt).
    #[error("corrupt store artifact: {0}")]
    Corrupt(String),
}

/// Result alias for store operations.
pub type StoreResult<T> = Result<T, ThinkingMapStoreError>;

// ── Persisted / returned types ───────────────────────────────────────────────

/// One applied envelope in the authoritative append-only event log. The log is
/// the crash-safe replay/audit trail; the snapshot is a derived materialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapEvent {
    pub sequence: u64,
    pub envelope: MapOperationEnvelope,
    pub resulting_revision: u64,
    pub semantic_hash: String,
    pub applied_at: String,
}

/// Per-map manifest: the small, atomically-written head record pointing at the
/// latest revision/sequence/hash. Enumerated by `list_maps`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapManifest {
    pub schema_version: u32,
    pub map_id: String,
    pub principal: String,
    pub workspace: String,
    pub title: String,
    pub source: ThinkingMapSource,
    pub lifecycle: MapLifecycle,
    pub latest_revision: u64,
    pub latest_sequence: u64,
    pub latest_semantic_hash: String,
    pub created_at: String,
    pub updated_at: String,
    /// Branch provenance (set by `restore_as_branch`): the source map this branch
    /// was forked from. Optional + serde-default so existing manifests without the
    /// field deserialize unchanged and non-branch manifests omit it entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branched_from_map_id: Option<String>,
    /// Branch provenance: the source sequence the branch was restored at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branched_from_sequence: Option<u64>,
}

/// Maximum number of nodes carried in a [`MapSummary::node_preview`]. Keeps the
/// list response cheap even for a large library — the preview only needs enough
/// nodes to draw the library card's mini-graph, not the whole board.
pub const NODE_PREVIEW_MAX_NODES: usize = 10;

/// One node in a [`NodePreview`] — a stripped-down projection of a
/// [`ThinkingNode`] carrying only what the client's library-card mini-graph
/// needs (identity, tree link, kind, a title snippet, and whether it is a
/// model-suggested/provisional node). Intentionally omits detail markdown,
/// confidence, speaker, source refs, timestamps, etc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodePreviewNode {
    pub node_id: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<NodeId>,
    pub kind: NodeKind,
    /// True when the node is model-inferred or still provisional — mirrors the
    /// client projection's `suggested` rule so the preview matches the open map.
    pub suggested: bool,
    /// A short title snippet (the node label, truncated) — enough to distinguish
    /// nodes without shipping full labels for a preview.
    pub title: String,
}

/// A lightweight per-map node preview for the library card mini-graph: the first
/// [`NODE_PREVIEW_MAX_NODES`] live nodes plus the parent→child branch edges among
/// them. This is NOT the full graph — it is a bounded thumbnail so listing many
/// maps stays cheap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodePreview {
    pub nodes: Vec<NodePreviewNode>,
    /// Parent→child branch edges among the previewed nodes (both endpoints must
    /// be within `nodes`). Related/cross-link edges are intentionally omitted —
    /// the mini-graph only draws the branch skeleton.
    pub edges: Vec<NodePreviewEdge>,
}

/// One parent→child branch edge in a [`NodePreview`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodePreviewEdge {
    pub from: NodeId,
    pub to: NodeId,
}

/// Maximum characters retained for a [`NodePreviewNode::title`] snippet.
const NODE_PREVIEW_TITLE_MAX_CHARS: usize = 80;

impl NodePreview {
    /// Build a bounded node preview from a full map: take the first
    /// [`NODE_PREVIEW_MAX_NODES`] live (non-tombstoned) nodes, ordered by
    /// `created_at` then `node_id` (deterministic, matching the client
    /// projection's ordering), project each down to a [`NodePreviewNode`], and
    /// synthesize a branch edge for every previewed node whose parent is ALSO in
    /// the previewed set. Returns `None` when the map has no live nodes so the
    /// field is omitted entirely for empty maps.
    fn from_map(map: &ThinkingMap) -> Option<Self> {
        let mut live: Vec<&super::models::ThinkingNode> =
            map.nodes.values().filter(|n| !n.tombstoned).collect();
        if live.is_empty() {
            return None;
        }
        // Deterministic order: created_at asc, then node_id asc (tie-break).
        live.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        live.truncate(NODE_PREVIEW_MAX_NODES);

        let preview_ids: std::collections::HashSet<String> =
            live.iter().map(|n| n.node_id.clone()).collect();

        let mut nodes = Vec::with_capacity(live.len());
        let mut edges = Vec::new();
        for node in &live {
            // Only keep a parent link when the parent is itself in the preview.
            let parent_id = node
                .parent_id
                .as_ref()
                .filter(|pid| preview_ids.contains(pid.as_str()))
                .cloned();
            if let Some(parent) = &parent_id {
                edges.push(NodePreviewEdge {
                    from: parent.clone(),
                    to: node.node_id.clone(),
                });
            }
            let suggested = node.assertion_origin == AssertionOrigin::ModelInferred
                || node.epistemic_state == EpistemicState::Provisional;
            nodes.push(NodePreviewNode {
                node_id: node.node_id.clone(),
                parent_id,
                kind: node.kind,
                suggested,
                title: node
                    .label
                    .chars()
                    .take(NODE_PREVIEW_TITLE_MAX_CHARS)
                    .collect(),
            });
        }
        Some(Self { nodes, edges })
    }
}

/// Lightweight summary returned by `list_maps`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapSummary {
    pub map_id: String,
    pub title: String,
    pub lifecycle: MapLifecycle,
    pub latest_revision: u64,
    pub updated_at: String,
    /// OPTIONAL, backward-compatible bounded node preview for the client's
    /// library-card mini-graph (first [`NODE_PREVIEW_MAX_NODES`] live nodes +
    /// their branch edges). Omitted (`None`, not serialized) when the map has no
    /// live nodes, so older clients that ignore the field — and older servers
    /// that never emit it — both keep working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_preview: Option<NodePreview>,
}

// ── Store ────────────────────────────────────────────────────────────────────

/// Process-wide per-map locks. REST handlers construct short-lived store
/// values over the same Artifact V2 root; keeping locks on each store instance
/// allowed two handlers to read the same manifest head and append the same
/// sequence. Weak values prevent inactive map IDs from becoming a permanent
/// registry leak.
static THINKING_MAP_LOCKS: OnceLock<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>> = OnceLock::new();

/// Durable, scope-owned store for Live Thinking Maps.
#[derive(Clone)]
pub struct ThinkingMapStore {
    workspace: ArtifactV2Workspace,
    /// Canonical/lexically-normalized storage identity used by the process-wide
    /// lock registry. Computing it once avoids filesystem work on every map
    /// mutation and prevents relative/absolute aliases from taking two locks.
    lock_namespace: Arc<str>,
}

impl std::fmt::Debug for ThinkingMapStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThinkingMapStore")
            .field("base_root", &self.workspace.base_root())
            .field("lock_namespace", &self.lock_namespace)
            .finish()
    }
}

impl ThinkingMapStore {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        let lock_namespace = normalized_lock_namespace(workspace.base_root());
        Self {
            workspace,
            lock_namespace: lock_namespace.into(),
        }
    }

    // ── Path helpers (all validate ids first; fail closed) ───────────────────

    fn validate_scope(&self, principal: &str, workspace: &str) -> StoreResult<()> {
        if !is_safe_scope_id(principal) {
            return Err(ThinkingMapStoreError::InvalidId(format!(
                "principal={principal}"
            )));
        }
        if !is_safe_scope_id(workspace) {
            return Err(ThinkingMapStoreError::InvalidId(format!(
                "workspace={workspace}"
            )));
        }
        Ok(())
    }

    pub(super) fn validate_ids(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
    ) -> StoreResult<()> {
        self.validate_scope(principal, workspace)?;
        if !is_safe_scope_id(map_id) {
            return Err(ThinkingMapStoreError::InvalidId(format!("map_id={map_id}")));
        }
        Ok(())
    }

    /// `scopes/<principal>/<workspace>/thinking_maps/` — the enumeration root.
    fn maps_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace
            .scope_root(principal, workspace)
            .join("thinking_maps")
    }

    /// `scopes/<principal>/<workspace>/thinking_maps/<map_id>/`.
    fn map_dir(&self, principal: &str, workspace: &str, map_id: &str) -> PathBuf {
        self.maps_root(principal, workspace).join(map_id)
    }

    pub(super) fn manifest_path(&self, principal: &str, workspace: &str, map_id: &str) -> PathBuf {
        self.map_dir(principal, workspace, map_id)
            .join("manifest.json")
    }

    pub(super) fn snapshot_path(&self, principal: &str, workspace: &str, map_id: &str) -> PathBuf {
        self.map_dir(principal, workspace, map_id)
            .join("snapshot.json")
    }

    pub(super) fn events_path(&self, principal: &str, workspace: &str, map_id: &str) -> PathBuf {
        self.map_dir(principal, workspace, map_id)
            .join("events.jsonl")
    }

    /// Return (creating if absent) the per-map async lock.
    pub(super) fn map_lock(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
    ) -> Arc<AsyncMutex<()>> {
        let key = format!("{}/{principal}/{workspace}/{map_id}", self.lock_namespace);
        let registry = THINKING_MAP_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lock) = guard.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        // Opportunistically prune dead entries while a new map is being
        // registered. This keeps the global registry proportional to active
        // maps without a background maintenance task.
        guard.retain(|_, lock| lock.strong_count() > 0);
        let lock = Arc::new(AsyncMutex::new(()));
        guard.insert(key, Arc::downgrade(&lock));
        lock
    }

    // ── Low-level read helpers ───────────────────────────────────────────────

    pub(super) async fn read_manifest(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
    ) -> StoreResult<Option<MapManifest>> {
        let path = self.manifest_path(principal, workspace, map_id);
        match self.workspace.read_to_string_path(&path).await {
            Ok(body) => {
                let manifest: MapManifest = serde_json::from_str(&body).map_err(|err| {
                    ThinkingMapStoreError::Corrupt(format!("{}: {err}", path.display()))
                })?;
                Ok(Some(manifest))
            },
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ThinkingMapStoreError::Io(err)),
        }
    }

    pub(super) async fn read_snapshot(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
    ) -> StoreResult<Option<ThinkingMap>> {
        let path = self.snapshot_path(principal, workspace, map_id);
        match self.workspace.read_to_string_path(&path).await {
            Ok(body) => {
                let map: ThinkingMap = serde_json::from_str(&body).map_err(|err| {
                    ThinkingMapStoreError::Corrupt(format!("{}: {err}", path.display()))
                })?;
                Ok(Some(map))
            },
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ThinkingMapStoreError::Io(err)),
        }
    }

    pub(super) fn manifest_from_map(
        map: &ThinkingMap,
        latest_sequence: u64,
        latest_semantic_hash: String,
    ) -> MapManifest {
        MapManifest {
            schema_version: THINKING_MAP_STORE_SCHEMA_VERSION,
            map_id: map.map_id.clone(),
            principal: map.principal.clone(),
            workspace: map.workspace.clone(),
            title: map.title.clone(),
            source: map.source.clone(),
            lifecycle: map.lifecycle,
            latest_revision: map.revision,
            latest_sequence,
            latest_semantic_hash,
            created_at: map.created_at.clone(),
            updated_at: map.updated_at.clone(),
            branched_from_map_id: None,
            branched_from_sequence: None,
        }
    }

    /// Accessor for the underlying workspace so sibling modules (e.g. `replay`)
    /// can perform raw reads/writes without widening the field itself.
    pub(super) fn workspace(&self) -> &ArtifactV2Workspace {
        &self.workspace
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Create a new map on disk. Fails with `AlreadyExists` if a manifest is
    /// already present. Writes an empty `events.jsonl`, the snapshot, then the
    /// manifest (latest_revision = map.revision, latest_sequence = 0).
    pub async fn create_map(&self, map: &ThinkingMap) -> StoreResult<()> {
        self.validate_ids(&map.principal, &map.workspace, &map.map_id)?;
        let lock = self.map_lock(&map.principal, &map.workspace, &map.map_id);
        let _guard = lock.lock().await;

        if self
            .read_manifest(&map.principal, &map.workspace, &map.map_id)
            .await?
            .is_some()
        {
            return Err(ThinkingMapStoreError::AlreadyExists(map.map_id.clone()));
        }

        let map_dir = self.map_dir(&map.principal, &map.workspace, &map.map_id);
        self.workspace.create_dir_all_path(&map_dir).await?;

        // Touch/create an empty events.jsonl (append of zero bytes creates it).
        let events = self.events_path(&map.principal, &map.workspace, &map.map_id);
        self.workspace.append_path(&events, b"").await?;

        // snapshot then manifest.
        let snapshot = self.snapshot_path(&map.principal, &map.workspace, &map.map_id);
        self.workspace
            .write_json_atomic_path(&snapshot, map)
            .await?;

        let manifest = Self::manifest_from_map(map, 0, semantic_hash(map));
        let manifest_path = self.manifest_path(&map.principal, &map.workspace, &map.map_id);
        self.workspace
            .write_json_atomic_path(&manifest_path, &manifest)
            .await?;
        Ok(())
    }

    /// Load the current materialized map. Missing snapshot ⇒ `Ok(None)`.
    pub async fn load_map(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
    ) -> StoreResult<Option<ThinkingMap>> {
        self.validate_ids(principal, workspace, map_id)?;
        self.read_snapshot(principal, workspace, map_id).await
    }

    /// The core apply path: load current map, run the reducer, and — only on
    /// `Applied` — durably persist (event log FIRST, then snapshot, then
    /// manifest). Reducer rejections map to `Validation(..)` with no disk write;
    /// idempotent replays write nothing.
    pub async fn apply_and_persist(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
        envelope: &MapOperationEnvelope,
        applied_at: &str,
    ) -> StoreResult<ApplyOutcome> {
        self.validate_ids(principal, workspace, map_id)?;
        let lock = self.map_lock(principal, workspace, map_id);
        let _guard = lock.lock().await;

        // Manifest is the head record — carries latest_sequence for the log.
        let manifest = self
            .read_manifest(principal, workspace, map_id)
            .await?
            .ok_or_else(|| ThinkingMapStoreError::NotFound(map_id.to_string()))?;
        let map = self
            .read_snapshot(principal, workspace, map_id)
            .await?
            .ok_or_else(|| ThinkingMapStoreError::NotFound(map_id.to_string()))?;

        let outcome = apply_envelope(&map, envelope, applied_at)
            .map_err(ThinkingMapStoreError::Validation)?;

        match &outcome {
            ApplyOutcome::Applied {
                map: new_map,
                resulting_revision,
                semantic_hash,
            } => {
                let event = MapEvent {
                    sequence: manifest.latest_sequence + 1,
                    envelope: envelope.clone(),
                    resulting_revision: *resulting_revision,
                    semantic_hash: semantic_hash.clone(),
                    applied_at: applied_at.to_string(),
                };

                // (b) Append + fsync the event FIRST — the durable log leads the
                // snapshot. `append_jsonl_path` serializes + appends and fsyncs
                // the file before returning (its `serde_json::Error` maps into
                // `ArtifactV2Error::Serde`).
                let events = self.events_path(principal, workspace, map_id);
                self.workspace.append_jsonl_path(&events, &event).await?;

                // (c) Then the snapshot.
                let snapshot = self.snapshot_path(principal, workspace, map_id);
                self.workspace
                    .write_json_atomic_path(&snapshot, new_map)
                    .await?;

                // (d) Then the manifest head record.
                let new_manifest =
                    Self::manifest_from_map(new_map, event.sequence, semantic_hash.clone());
                let manifest_path = self.manifest_path(principal, workspace, map_id);
                self.workspace
                    .write_json_atomic_path(&manifest_path, &new_manifest)
                    .await?;

                Ok(outcome)
            },
            ApplyOutcome::IdempotentReplay { .. } => Ok(outcome),
        }
    }

    /// Enumerate every visible (non-deleted) map in the scope, most recently
    /// updated first (`updated_at` desc, `map_id` asc tie-break — RFC3339
    /// strings from one clock compare correctly as strings). A deleted map is
    /// a durable tombstone: it remains directly loadable for recovery/audit,
    /// but is not part of normal discovery. Unreadable/corrupt manifests are
    /// skipped (quarantined) rather than failing the whole list.
    pub async fn list_maps(
        &self,
        principal: &str,
        workspace: &str,
    ) -> StoreResult<Vec<MapSummary>> {
        self.list_map_summaries(principal, workspace, None).await
    }

    /// Enumerate maps in one exact lifecycle. This is the server-side source
    /// for lifecycle-specific library tabs, including the Deleted tombstone
    /// view; ordering and corruption handling match [`Self::list_maps`].
    pub async fn list_maps_by_lifecycle(
        &self,
        principal: &str,
        workspace: &str,
        lifecycle: MapLifecycle,
    ) -> StoreResult<Vec<MapSummary>> {
        self.list_map_summaries(principal, workspace, Some(lifecycle))
            .await
    }

    async fn list_map_summaries(
        &self,
        principal: &str,
        workspace: &str,
        lifecycle: Option<MapLifecycle>,
    ) -> StoreResult<Vec<MapSummary>> {
        self.validate_scope(principal, workspace)?;
        let root = self.maps_root(principal, workspace);
        let entries = self.workspace.read_dir_path_or_empty(&root).await?;

        let mut summaries = Vec::new();
        for entry in entries {
            if !entry.is_dir {
                continue;
            }
            let map_id = entry.file_name;
            // Defense-in-depth: a stray directory with an unsafe name is skipped.
            if !is_safe_scope_id(&map_id) {
                continue;
            }
            match self.read_manifest(principal, workspace, &map_id).await {
                Ok(Some(manifest)) => {
                    let matches = lifecycle
                        .map(|expected| manifest.lifecycle == expected)
                        .unwrap_or(manifest.lifecycle != MapLifecycle::Deleted);
                    if !matches {
                        continue;
                    }
                    // Best-effort bounded node preview for the library card
                    // mini-graph. Read the snapshot and project a capped preview;
                    // a missing/corrupt snapshot just yields `None` (the summary
                    // metadata still lists) rather than failing the whole list.
                    let node_preview = match self.read_snapshot(principal, workspace, &map_id).await
                    {
                        Ok(Some(map)) => NodePreview::from_map(&map),
                        Ok(None) | Err(ThinkingMapStoreError::Corrupt(_)) => None,
                        Err(err) => return Err(err),
                    };
                    summaries.push(MapSummary {
                        map_id: manifest.map_id,
                        title: manifest.title,
                        lifecycle: manifest.lifecycle,
                        latest_revision: manifest.latest_revision,
                        updated_at: manifest.updated_at,
                        node_preview,
                    });
                },
                // Missing manifest (partially-created dir) or corrupt → skip.
                Ok(None) | Err(ThinkingMapStoreError::Corrupt(_)) => continue,
                Err(err) => return Err(err),
            }
        }
        summaries.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.map_id.cmp(&b.map_id))
        });
        Ok(summaries)
    }

    /// Every map id that has a directory in the scope, ascending by id.
    ///
    /// Deliberately NOT `list_maps`: that is the library/discovery view and it
    /// hides soft-deleted tombstones and skips maps whose manifest will not
    /// parse. Crash recovery cares about the durability of the event log, which
    /// is orthogonal to whether a map is still visible in the library, so this
    /// returns every directory that could hold one. A stray directory with an
    /// unsafe name is skipped (defense-in-depth, same as `list_maps`), and a
    /// missing enumeration root is an empty list, not an error.
    pub(super) async fn list_map_ids(
        &self,
        principal: &str,
        workspace: &str,
    ) -> StoreResult<Vec<String>> {
        self.validate_scope(principal, workspace)?;
        let root = self.maps_root(principal, workspace);
        let entries = self.workspace.read_dir_path_or_empty(&root).await?;
        let mut ids: Vec<String> = entries
            .into_iter()
            .filter(|entry| entry.is_dir && is_safe_scope_id(&entry.file_name))
            .map(|entry| entry.file_name)
            .collect();
        ids.sort();
        Ok(ids)
    }

    /// Permanently remove one map directory, including its snapshot, manifest,
    /// append-only event log, and exports. The lifecycle precondition is checked
    /// while holding the map lock so an active map can never be purged by this
    /// method. A deleted map id may be reused after successful removal.
    pub async fn permanently_delete_map(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
    ) -> StoreResult<()> {
        self.validate_ids(principal, workspace, map_id)?;
        let lock = self.map_lock(principal, workspace, map_id);
        let _guard = lock.lock().await;

        let manifest = self
            .read_manifest(principal, workspace, map_id)
            .await?
            .ok_or_else(|| ThinkingMapStoreError::NotFound(map_id.to_string()))?;
        if manifest.lifecycle != MapLifecycle::Deleted {
            return Err(ThinkingMapStoreError::PurgeRequiresDeleted(
                map_id.to_string(),
            ));
        }

        self.workspace
            .remove_dir_all_path(self.map_dir(principal, workspace, map_id))
            .await?;
        Ok(())
    }

    /// Read the append-only event log and return events with
    /// `sequence > after_sequence` in ascending sequence order. Missing file ⇒
    /// empty vec.
    pub async fn events_after(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
        after_sequence: u64,
    ) -> StoreResult<Vec<MapEvent>> {
        self.validate_ids(principal, workspace, map_id)?;
        let path = self.events_path(principal, workspace, map_id);
        let body = match self.workspace.read_to_string_path(&path).await {
            Ok(body) => body,
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(err) => return Err(ThinkingMapStoreError::Io(err)),
        };

        let mut events = Vec::new();
        for (idx, raw) in body.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let event: MapEvent = serde_json::from_str(line).map_err(|err| {
                ThinkingMapStoreError::Corrupt(format!(
                    "{} line {}: {err}",
                    path.display(),
                    idx + 1
                ))
            })?;
            if event.sequence > after_sequence {
                events.push(event);
            }
        }
        events.sort_by_key(|event| event.sequence);
        Ok(events)
    }
}

fn normalized_lock_namespace(root: &Path) -> String {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(root)
    };
    let mut lexical = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {},
            Component::ParentDir => {
                lexical.pop();
            },
            other => lexical.push(other.as_os_str()),
        }
    }
    std::fs::canonicalize(&lexical)
        .unwrap_or(lexical)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking_map::models::{
        AssertionOrigin, EpistemicState, NodeKind, ThinkingMapSource, ThinkingNode,
    };
    use crate::thinking_map::operations::{MapOperation, MapOperationEnvelope, OperationActor};
    use tempfile::TempDir;

    const TS: &str = "2026-07-19T00:00:00Z";
    const TS2: &str = "2026-07-19T01:00:00Z";

    fn store() -> (TempDir, ThinkingMapStore) {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        (tmp, ThinkingMapStore::new(workspace))
    }

    fn sample_map(map_id: &str) -> ThinkingMap {
        ThinkingMap::new(
            map_id.to_string(),
            "anonymous",
            "default",
            "Test map",
            ThinkingMapSource::Solo,
            TS,
        )
    }

    fn owner() -> OperationActor {
        OperationActor::Owner {
            principal: "anonymous".to_string(),
        }
    }

    fn node(id: &str) -> ThinkingNode {
        ThinkingNode {
            node_id: id.to_string(),
            kind: NodeKind::Idea,
            label: format!("label-{id}"),
            detail_markdown: None,
            epistemic_state: EpistemicState::Provisional,
            assertion_origin: AssertionOrigin::OwnerSpoken,
            confidence: 0.5,
            speaker: None,
            source_refs: vec![],
            parent_id: None,
            position: None,
            position_locked: false,
            promoted_refs: vec![],
            tombstoned: false,
            created_at: TS.to_string(),
            updated_at: TS.to_string(),
        }
    }

    /// Envelope adding `node_id` against `base_revision`.
    fn add_node_env(
        map_id: &str,
        base_revision: u64,
        idem: &str,
        node_id: &str,
    ) -> MapOperationEnvelope {
        MapOperationEnvelope::new(
            format!("env-{idem}"),
            map_id.to_string(),
            base_revision,
            owner(),
            idem,
            vec![MapOperation::AddNode {
                node: node(node_id),
            }],
            TS,
        )
    }

    #[tokio::test]
    async fn create_and_load_round_trip() {
        let (_tmp, store) = store();
        let map = sample_map("m1");
        store.create_map(&map).await.expect("create");
        let loaded = store
            .load_map("anonymous", "default", "m1")
            .await
            .expect("load")
            .expect("present");
        assert_eq!(loaded, map);
    }

    #[tokio::test]
    async fn apply_persists() {
        let (_tmp, store) = store();
        let map = sample_map("m1");
        store.create_map(&map).await.expect("create");

        let env = add_node_env("m1", 0, "i1", "n1");
        let outcome = store
            .apply_and_persist("anonymous", "default", "m1", &env, TS2)
            .await
            .expect("apply");
        assert!(matches!(outcome, ApplyOutcome::Applied { .. }));

        let reloaded = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.revision, 1);
        assert!(reloaded.nodes.contains_key("n1"));

        let manifest = store
            .read_manifest("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(manifest.latest_sequence, 1);
        assert_eq!(manifest.latest_revision, 1);
    }

    #[tokio::test]
    async fn independent_store_instances_share_one_map_append_boundary() {
        let tmp = TempDir::new().expect("tempdir");
        let first = ThinkingMapStore::new(ArtifactV2Workspace::new(tmp.path()));
        let second = ThinkingMapStore::new(ArtifactV2Workspace::new(tmp.path().join(".")));
        first.create_map(&sample_map("m1")).await.expect("create");

        let left = add_node_env("m1", 0, "left", "left-node");
        let right = add_node_env("m1", 0, "right", "right-node");
        let (left_result, right_result) = tokio::join!(
            first.apply_and_persist("anonymous", "default", "m1", &left, TS2),
            second.apply_and_persist("anonymous", "default", "m1", &right, TS2),
        );

        assert_ne!(left_result.is_ok(), right_result.is_ok());
        let events = first
            .events_after("anonymous", "default", "m1", 0)
            .await
            .expect("read authoritative log");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 1);
        assert_eq!(
            first
                .read_manifest("anonymous", "default", "m1")
                .await
                .expect("manifest read")
                .expect("manifest")
                .latest_sequence,
            1
        );
    }

    #[test]
    fn equivalent_storage_root_spellings_share_one_lock_namespace() {
        let tmp = TempDir::new().expect("tempdir");
        let direct = normalized_lock_namespace(tmp.path());
        let aliased = normalized_lock_namespace(&tmp.path().join("not-created").join(".."));
        assert_eq!(direct, aliased);
    }

    #[tokio::test]
    async fn event_log_records_applied_envelope() {
        let (_tmp, store) = store();
        let map = sample_map("m1");
        store.create_map(&map).await.expect("create");

        let env = add_node_env("m1", 0, "i1", "n1");
        let outcome = store
            .apply_and_persist("anonymous", "default", "m1", &env, TS2)
            .await
            .expect("apply");
        let (expected_rev, expected_hash) = match outcome {
            ApplyOutcome::Applied {
                resulting_revision,
                semantic_hash,
                ..
            } => (resulting_revision, semantic_hash),
            other => panic!("expected Applied, got {other:?}"),
        };

        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[0].resulting_revision, expected_rev);
        assert_eq!(events[0].semantic_hash, expected_hash);
        assert_eq!(events[0].applied_at, TS2);

        // Nothing after sequence 1.
        let none = store
            .events_after("anonymous", "default", "m1", 1)
            .await
            .expect("events");
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn idempotent_replay_writes_nothing() {
        let (_tmp, store) = store();
        let map = sample_map("m1");
        store.create_map(&map).await.expect("create");

        let env = add_node_env("m1", 0, "i1", "n1");
        store
            .apply_and_persist("anonymous", "default", "m1", &env, TS2)
            .await
            .expect("first apply");

        // Re-apply the SAME envelope (base_revision still 0, same idem/id).
        let outcome = store
            .apply_and_persist("anonymous", "default", "m1", &env, TS2)
            .await
            .expect("replay");
        assert!(matches!(outcome, ApplyOutcome::IdempotentReplay { .. }));

        // Event log still length 1; snapshot revision unchanged.
        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        let reloaded = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.revision, 1);
    }

    #[tokio::test]
    async fn validation_error_does_not_write() {
        let (_tmp, store) = store();
        let map = sample_map("m1");
        store.create_map(&map).await.expect("create");

        // set_epistemic_state on an unknown node → reducer UnknownNode error.
        let env = MapOperationEnvelope::new(
            "env-bad".to_string(),
            "m1".to_string(),
            0,
            owner(),
            "bad",
            vec![MapOperation::SetEpistemicState {
                node_id: "ghost".to_string(),
                state: EpistemicState::Asserted,
            }],
            TS,
        );
        let err = store
            .apply_and_persist("anonymous", "default", "m1", &env, TS2)
            .await
            .expect_err("should reject");
        assert!(matches!(err, ThinkingMapStoreError::Validation(_)));

        // Event log unchanged (empty), snapshot unchanged (revision 0).
        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        assert!(events.is_empty());
        let reloaded = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.revision, 0);
        assert!(reloaded.nodes.is_empty());
    }

    #[tokio::test]
    async fn apply_to_nonexistent_map_is_not_found() {
        let (_tmp, store) = store();
        let env = add_node_env("ghost", 0, "i1", "n1");
        let err = store
            .apply_and_persist("anonymous", "default", "ghost", &env, TS2)
            .await
            .expect_err("should be NotFound");
        assert!(matches!(err, ThinkingMapStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn create_existing_map_is_already_exists() {
        let (_tmp, store) = store();
        let map = sample_map("m1");
        store.create_map(&map).await.expect("first create");
        let err = store.create_map(&map).await.expect_err("second create");
        assert!(matches!(err, ThinkingMapStoreError::AlreadyExists(_)));
    }

    #[tokio::test]
    async fn list_maps_returns_visible_maps_sorted_and_keeps_tombstones_loadable() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("bravo")).await.unwrap();
        store.create_map(&sample_map("alpha")).await.unwrap();
        let mut deleted = sample_map("deleted");
        deleted.lifecycle = MapLifecycle::Deleted;
        store.create_map(&deleted).await.unwrap();
        // Bump bravo's revision at TS2 — recency sort must put it FIRST even
        // though "alpha" < "bravo" lexically (map_id is only the tie-break).
        let env = add_node_env("bravo", 0, "i1", "n1");
        store
            .apply_and_persist("anonymous", "default", "bravo", &env, TS2)
            .await
            .unwrap();

        let summaries = store.list_maps("anonymous", "default").await.unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].map_id, "bravo");
        assert_eq!(summaries[1].map_id, "alpha");
        assert_eq!(summaries[0].latest_revision, 1);
        assert_eq!(summaries[1].latest_revision, 0);

        // bravo gained one node → it carries a preview; alpha (empty) omits it.
        let bravo_preview = summaries[0]
            .node_preview
            .as_ref()
            .expect("bravo has a node preview");
        assert_eq!(bravo_preview.nodes.len(), 1);
        assert_eq!(bravo_preview.nodes[0].node_id, "n1");
        assert!(bravo_preview.edges.is_empty()); // root node → no branch edge
        assert!(summaries[1].node_preview.is_none()); // empty map → no preview

        let tombstone = store
            .load_map("anonymous", "default", "deleted")
            .await
            .unwrap()
            .expect("deleted map remains directly loadable");
        assert_eq!(tombstone.lifecycle, MapLifecycle::Deleted);

        let deleted_summaries = store
            .list_maps_by_lifecycle("anonymous", "default", MapLifecycle::Deleted)
            .await
            .unwrap();
        assert_eq!(deleted_summaries.len(), 1);
        assert_eq!(deleted_summaries[0].map_id, "deleted");
    }

    #[tokio::test]
    async fn permanent_delete_requires_tombstone_removes_all_artifacts_and_allows_id_reuse() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();

        let error = store
            .permanently_delete_map("anonymous", "default", "m1")
            .await
            .expect_err("active maps must not be permanently deleted");
        assert!(matches!(
            error,
            ThinkingMapStoreError::PurgeRequiresDeleted(ref id) if id == "m1"
        ));

        let delete_envelope = MapOperationEnvelope::new(
            "env-delete".to_string(),
            "m1".to_string(),
            0,
            owner(),
            "delete-m1",
            vec![MapOperation::SetLifecycle {
                lifecycle: MapLifecycle::Deleted,
            }],
            TS2,
        );
        store
            .apply_and_persist("anonymous", "default", "m1", &delete_envelope, TS2)
            .await
            .unwrap();

        store
            .permanently_delete_map("anonymous", "default", "m1")
            .await
            .unwrap();
        assert!(store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .list_maps_by_lifecycle("anonymous", "default", MapLifecycle::Deleted)
            .await
            .unwrap()
            .is_empty());

        store
            .create_map(&sample_map("m1"))
            .await
            .expect("purged id can be reused");
    }

    #[tokio::test]
    async fn list_maps_node_preview_is_bounded_and_has_branch_edges() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();

        // Add a root node, then a child of it, then MORE than the cap of roots so
        // truncation is exercised. Each apply advances the base revision.
        let root = MapOperationEnvelope::new(
            "env-root".to_string(),
            "m1".to_string(),
            0,
            owner(),
            "i-root",
            vec![MapOperation::AddNode { node: node("root") }],
            TS,
        );
        store
            .apply_and_persist("anonymous", "default", "m1", &root, TS2)
            .await
            .unwrap();

        // A child of root (parent_id set) → yields a branch edge in the preview.
        let mut child_node = node("child");
        child_node.parent_id = Some("root".to_string());
        let child = MapOperationEnvelope::new(
            "env-child".to_string(),
            "m1".to_string(),
            1,
            owner(),
            "i-child",
            vec![MapOperation::AddNode { node: child_node }],
            TS,
        );
        store
            .apply_and_persist("anonymous", "default", "m1", &child, TS2)
            .await
            .unwrap();

        // Push node count past the cap so the preview truncates. All nodes share
        // `created_at = TS`, so ordering falls to the `node_id` tie-break — the
        // "z-extra-*" ids sort AFTER "root"/"child", so root+child survive the cut.
        for i in 0..(NODE_PREVIEW_MAX_NODES + 5) {
            let env = MapOperationEnvelope::new(
                format!("env-extra-{i}"),
                "m1".to_string(),
                2 + i as u64,
                owner(),
                &format!("i-extra-{i}"),
                vec![MapOperation::AddNode {
                    node: node(&format!("z-extra-{i:02}")),
                }],
                TS,
            );
            store
                .apply_and_persist("anonymous", "default", "m1", &env, TS2)
                .await
                .unwrap();
        }

        let summaries = store.list_maps("anonymous", "default").await.unwrap();
        let preview = summaries[0].node_preview.as_ref().expect("preview present");
        // Capped at NODE_PREVIEW_MAX_NODES regardless of the real node count.
        assert_eq!(preview.nodes.len(), NODE_PREVIEW_MAX_NODES);
        // root+child sort first (node_id tie-break), so their branch edge survives.
        assert!(preview
            .edges
            .iter()
            .any(|e| e.from == "root" && e.to == "child"));
        // Every previewed node's parent (when present) is itself previewed.
        let ids: std::collections::HashSet<&str> =
            preview.nodes.iter().map(|n| n.node_id.as_str()).collect();
        for n in &preview.nodes {
            if let Some(parent) = &n.parent_id {
                assert!(ids.contains(parent.as_str()));
            }
        }
    }

    #[tokio::test]
    async fn events_after_paging() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();

        // Three sequential applies (revision advances each time).
        for (rev, (idem, node_id)) in [("i1", "n1"), ("i2", "n2"), ("i3", "n3")]
            .into_iter()
            .enumerate()
        {
            let env = add_node_env("m1", rev as u64, idem, node_id);
            store
                .apply_and_persist("anonymous", "default", "m1", &env, TS2)
                .await
                .unwrap();
        }

        let after_one = store
            .events_after("anonymous", "default", "m1", 1)
            .await
            .unwrap();
        assert_eq!(
            after_one.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let (_tmp, store) = store();
        // load_map with a hostile map_id.
        let err = store
            .load_map("anonymous", "default", "../evil")
            .await
            .expect_err("traversal load");
        assert!(matches!(err, ThinkingMapStoreError::InvalidId(_)));

        // apply_and_persist with a hostile map_id.
        let env = add_node_env("../evil", 0, "i1", "n1");
        let err = store
            .apply_and_persist("anonymous", "default", "../evil", &env, TS2)
            .await
            .expect_err("traversal apply");
        assert!(matches!(err, ThinkingMapStoreError::InvalidId(_)));
    }

    #[tokio::test]
    async fn concurrent_different_maps_both_succeed() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        store.create_map(&sample_map("m2")).await.unwrap();

        let env1 = add_node_env("m1", 0, "i1", "n1");
        let env2 = add_node_env("m2", 0, "i2", "n2");

        let (r1, r2) = tokio::join!(
            store.apply_and_persist("anonymous", "default", "m1", &env1, TS2),
            store.apply_and_persist("anonymous", "default", "m2", &env2, TS2),
        );
        assert!(matches!(r1.unwrap(), ApplyOutcome::Applied { .. }));
        assert!(matches!(r2.unwrap(), ApplyOutcome::Applied { .. }));

        let m1 = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        let m2 = store
            .load_map("anonymous", "default", "m2")
            .await
            .unwrap()
            .unwrap();
        assert!(m1.nodes.contains_key("n1"));
        assert!(m2.nodes.contains_key("n2"));
    }

    #[tokio::test]
    async fn crash_safety_ordering_event_matches_snapshot() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let env = add_node_env("m1", 0, "i1", "n1");
        store
            .apply_and_persist("anonymous", "default", "m1", &env, TS2)
            .await
            .unwrap();

        // The last event's resulting_revision equals the snapshot's revision:
        // event log and snapshot agree after a successful apply.
        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        let last = events.last().expect("at least one event");
        let snapshot = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(last.resulting_revision, snapshot.revision);
    }
}
