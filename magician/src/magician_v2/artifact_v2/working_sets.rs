//! Durable, scope-isolated evidence working sets.
//!
//! A working set snapshots already-authorized `ContentDocument` values into
//! content-addressed chunks. It deliberately owns no acquisition or authority
//! policy: web, document, and future research lanes provide validated content;
//! callers read it only through the existing scoped execution boundary.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex, OnceLock, Weak},
};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::content_sources::{
    ContentDocument, ContentPrivacy, ContentProvenance, SourceIdentity,
};

use super::{service::ArtifactV2Error, workspace::ArtifactV2Workspace};

pub const WORKING_SET_SCHEMA_VERSION: u32 = 1;
pub const MAX_WORKING_SET_SOURCES: usize = 128;
pub const MAX_WORKING_SET_TOTAL_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_WORKING_SET_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_WORKING_SET_CHUNK_BYTES: usize = 24 * 1024;
pub const MAX_WORKING_SET_CHUNKS: usize = 4_096;
pub const MAX_WORKING_SETS_PER_SCOPE: usize = 64;
pub const MAX_WORKING_SET_SCOPE_BYTES: u64 = 256 * 1024 * 1024;
pub const WORKING_SET_RETENTION_DAYS: i64 = 7;
const MAX_WORKING_SET_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_WORKING_SET_MANIFEST_DEPTH: usize = 16;
const MAX_WORKING_SET_MANIFEST_NODES: usize = 400_000;
const MAX_WORKING_SET_ID_CHARS: usize = 80;
const MAX_WORKING_SET_TITLE_CHARS: usize = 512;
const MAX_WORKING_SET_ACTOR_CHARS: usize = 256;
const MAX_WORKING_SET_QUERY_CHARS: usize = 512;
const MAX_WORKING_SET_SEARCH_RESULTS: usize = 20;
const MAX_WORKING_SET_EXCERPT_CHARS: usize = 480;
const WORKING_SET_SEARCH_BLOOM_BYTES: usize = 256;
const WORKING_SET_MIN_SEARCH_CHARS: usize = 3;
/// Staging persistence is bounded so a wedged filesystem cannot hold a create
/// request open indefinitely. The budget scales with the batch rather than being
/// flat: `persist_staging` performs one atomic write per chunk, and an accepted
/// request spans a single chunk up to `MAX_WORKING_SET_CHUNKS`, so one constant
/// deadline is either uselessly loose for a small request or trips a large one
/// that is making steady progress. The ceiling keeps the bound finite.
const WORKING_SET_PERSISTENCE_BASE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(1_500);
const WORKING_SET_PERSISTENCE_TIMEOUT_PER_CHUNK: std::time::Duration =
    std::time::Duration::from_millis(250);
const WORKING_SET_PERSISTENCE_TIMEOUT_CEILING: std::time::Duration =
    std::time::Duration::from_secs(120);
const STALE_WORKING_SET_STAGING_AGE: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingSetScope {
    pub principal: String,
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSetManifest {
    pub schema_version: u32,
    pub working_set_id: String,
    pub title: String,
    pub scope: WorkingSetScope,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub total_source_bytes: u64,
    pub sources: Vec<WorkingSetSource>,
    pub chunks: Vec<WorkingSetChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSetSource {
    pub source_id: String,
    pub identity: SourceIdentity,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    pub privacy: ContentPrivacy,
    pub content_hash: String,
    pub fetched_at_ms: i64,
    pub provenance: ContentProvenance,
    pub source_bytes: u64,
    pub chunk_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSetChunk {
    pub source_id: String,
    pub chunk_index: u32,
    pub relative_path: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub content_hash: String,
    /// Hex-encoded, fixed-size Bloom filter over lowercase character trigrams.
    /// It only filters candidates; exact text matching remains authoritative.
    #[serde(default)]
    pub search_bloom: String,
}

#[derive(Debug, Clone)]
pub struct WorkingSetSourceInput {
    pub source_id: String,
    pub document: ContentDocument,
}

#[derive(Debug, Clone)]
pub struct CreateWorkingSetRequest {
    pub working_set_id: String,
    pub title: String,
    pub created_by: String,
    pub sources: Vec<WorkingSetSourceInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkingSetChunkRead {
    pub working_set_id: String,
    pub source_id: String,
    pub source_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    pub chunk_index: u32,
    pub start_byte: u64,
    pub end_byte: u64,
    pub content_hash: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkingSetSearchMatch {
    pub working_set_id: String,
    pub source_id: String,
    pub source_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    pub chunk_index: u32,
    pub content_hash: String,
    pub score: u32,
    pub excerpt: String,
}

/// One captured snapshot as the execution index remembers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingSetExecutionMember {
    pub working_set_id: String,
    pub title: String,
    pub source_count: u32,
    pub source_bytes: u64,
    pub captured_at: DateTime<Utc>,
}

/// The sentence that opened the working-set path for an execution, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingSetExecutionActivation {
    pub lane: String,
    pub reason: String,
    pub activated_at: DateTime<Utc>,
}

/// Boundary B's task-scoped view over the immutable per-read snapshots.
///
/// Every capture writes its own snapshot and that stays true: immutability,
/// the seven-day expiry and the size caps are the store's safety properties.
/// A task that reads ten pages therefore has ten snapshots, and nothing tied
/// them to the task. This index does — and because it accrues the same three
/// quantities the activation rule asks about, it is also the accumulator that
/// rule needs. Members the lifecycle has evicted are skipped at search time;
/// a task that opens more than the retained maximum evicts its own oldest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingSetExecutionIndex {
    pub schema_version: u32,
    pub execution_id: String,
    pub scope: WorkingSetScope,
    pub members: Vec<WorkingSetExecutionMember>,
    pub total_source_bytes: u64,
    /// Distinct by content hash: the same page read twice is one source.
    pub distinct_sources: u32,
    /// One per capture. The activation rule's "investigation depth".
    pub read_rounds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<WorkingSetExecutionActivation>,
    /// Content hashes seen so far, so `distinct_sources` survives a reload.
    #[serde(default)]
    pub source_hashes: Vec<String>,
    /// Reads whose model-facing projection was actually narrowed — pages
    /// larger than the ordinary window. Zero means the path is open but has
    /// changed nothing the model sees, and the prompt must not claim it has.
    #[serde(default)]
    pub narrowed_reads: u32,
    /// What the ordinary window could not show: for every distinct page, its
    /// bytes past the window, summed. The activation rule's measure of
    /// scale — the set holds this much the model has not seen.
    #[serde(default)]
    pub beyond_window_bytes: u64,
    /// The last routing decision, whichever way it went. An activation is
    /// also kept in `activation`; a refusal is only here, and its `gate_open`
    /// tells a closed gate from a task below the threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<WorkingSetExecutionDecision>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkingSetExecutionDecision {
    /// The lane the reading agent is in; `None` for an agent in no lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// Whether the lane's gate was open when the decision was taken.
    pub gate_open: bool,
    pub activated: bool,
    pub reason: String,
    pub decided_at: DateTime<Utc>,
}

impl WorkingSetExecutionIndex {
    pub fn is_activated(&self) -> bool {
        self.activation.is_some()
    }
}

/// An execution-scoped search: the matches, and how many members could not be
/// read because the lifecycle had already evicted them.
#[derive(Debug, Clone, Serialize)]
pub struct WorkingSetExecutionSearch {
    pub execution_id: String,
    pub members_searched: u32,
    pub members_evicted: u32,
    pub matches: Vec<WorkingSetSearchMatch>,
}

pub const WORKING_SET_EXECUTION_INDEX_SCHEMA_VERSION: u32 = 1;
const MAX_WORKING_SET_EXECUTION_MEMBERS: usize = 256;

#[derive(Debug, Clone)]
pub struct WorkingSetStore {
    workspace: ArtifactV2Workspace,
}

impl WorkingSetStore {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    pub async fn create(
        &self,
        principal: &str,
        workspace: &str,
        request: CreateWorkingSetRequest,
    ) -> Result<WorkingSetManifest, ArtifactV2Error> {
        validate_create_request(&request)?;
        let create_lock = working_set_create_lock(principal, workspace);
        let _create_guard = create_lock.lock().await;
        let root = self.working_sets_root(principal, workspace);
        let destination = root.join(&request.working_set_id);
        if self.workspace.exists_path(&destination).await? {
            return Err(invalid_request(format!(
                "working set `{}` already exists in this scope",
                request.working_set_id
            )));
        }

        self.workspace.create_dir_all_path(&root).await?;
        self.cleanup_stale_staging(&root).await?;
        let created_at = Utc::now();
        let staging = root.join(staging_directory_name(
            principal,
            workspace,
            &request.working_set_id,
            created_at,
        ));
        if self.workspace.exists_path(&staging).await? {
            return Err(invalid_request(
                "working set staging directory already exists; retry the request".to_string(),
            ));
        }

        let manifest = build_manifest(principal, workspace, &request, created_at)?;
        let write_result = match tokio::time::timeout(
            persistence_timeout(&request.sources),
            self.persist_staging(&staging, &manifest, &request.sources),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(invalid_request(
                "working set staging persistence exceeded its deadline".to_string(),
            )),
        };
        if let Err(error) = write_result {
            let _ = self.workspace.remove_dir_all_path(&staging).await;
            return Err(error);
        }
        let eviction_candidates = match self
            .eviction_candidates_for_create(principal, workspace, manifest.total_source_bytes)
            .await
        {
            Ok(candidates) => candidates,
            Err(error) => {
                let _ = self.workspace.remove_dir_all_path(&staging).await;
                return Err(error);
            },
        };
        if let Err(error) = self.rename_path(&staging, &destination).await {
            let _ = self.workspace.remove_dir_all_path(&staging).await;
            return Err(error);
        }
        for path in eviction_candidates {
            if let Err(error) = self.workspace.remove_dir_all_path(&path).await {
                tracing::warn!(
                    principal,
                    workspace,
                    path = %path.display(),
                    error = %error,
                    "working set was published but a planned retention eviction failed"
                );
            }
        }
        Ok(manifest)
    }

    pub async fn get(
        &self,
        principal: &str,
        workspace: &str,
        working_set_id: &str,
    ) -> Result<WorkingSetManifest, ArtifactV2Error> {
        validate_identifier(working_set_id, "working set id")?;
        let path = self.manifest_path(principal, workspace, working_set_id);
        if !self.workspace.exists_path(&path).await? {
            return Err(invalid_request(format!(
                "working set `{working_set_id}` was not found in this scope"
            )));
        }
        let manifest: WorkingSetManifest = self
            .workspace
            .read_json_bounded_stream_path(
                &path,
                MAX_WORKING_SET_MANIFEST_BYTES,
                MAX_WORKING_SET_MANIFEST_DEPTH,
                MAX_WORKING_SET_MANIFEST_NODES,
            )
            .await?;
        validate_manifest_scope(&manifest, principal, workspace)?;
        if manifest.created_at <= working_set_retention_cutoff() {
            let expired_root = self
                .working_sets_root(principal, workspace)
                .join(working_set_id);
            let _ = self.workspace.remove_dir_all_path(expired_root).await;
            return Err(invalid_request(
                "working set has expired after its seven-day retention window".to_string(),
            ));
        }
        Ok(manifest)
    }

    pub async fn read_chunk(
        &self,
        principal: &str,
        workspace: &str,
        working_set_id: &str,
        source_id: &str,
        chunk_index: u32,
    ) -> Result<WorkingSetChunkRead, ArtifactV2Error> {
        validate_identifier(source_id, "source id")?;
        let manifest = self.get(principal, workspace, working_set_id).await?;
        let source = manifest
            .sources
            .iter()
            .find(|source| source.source_id == source_id)
            .ok_or_else(|| {
                invalid_request(format!("source `{source_id}` is not in this working set"))
            })?;
        let chunk = manifest
            .chunks
            .iter()
            .find(|chunk| chunk.source_id == source_id && chunk.chunk_index == chunk_index)
            .ok_or_else(|| {
                invalid_request(format!(
                    "chunk {chunk_index} for source `{source_id}` is not in this working set"
                ))
            })?;
        let text = self
            .read_chunk_text(principal, workspace, working_set_id, chunk)
            .await?;
        Ok(WorkingSetChunkRead {
            working_set_id: manifest.working_set_id,
            source_id: source.source_id.clone(),
            source_title: source.title.clone(),
            canonical_url: source.canonical_url.clone(),
            chunk_index: chunk.chunk_index,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            content_hash: chunk.content_hash.clone(),
            text,
        })
    }

    pub async fn search(
        &self,
        principal: &str,
        workspace: &str,
        working_set_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<WorkingSetSearchMatch>, ArtifactV2Error> {
        let query = normalize_query(query)?;
        let limit = limit.clamp(1, MAX_WORKING_SET_SEARCH_RESULTS);
        let manifest = self.get(principal, workspace, working_set_id).await?;
        let query_positions = bloom_positions(&query);
        let mut matches = Vec::new();
        for chunk in &manifest.chunks {
            if !chunk.search_bloom.is_empty()
                && !bloom_might_contain(&chunk.search_bloom, &query_positions)
            {
                continue;
            }
            let text = self
                .read_chunk_text(principal, workspace, working_set_id, chunk)
                .await?;
            let lowered = text.to_lowercase();
            let score = lowered.match_indices(&query).count() as u32;
            if score == 0 {
                continue;
            }
            let source = manifest
                .sources
                .iter()
                .find(|source| source.source_id == chunk.source_id)
                .ok_or_else(|| {
                    invalid_request("working set manifest has an unknown chunk source".to_owned())
                })?;
            matches.push(WorkingSetSearchMatch {
                working_set_id: manifest.working_set_id.clone(),
                source_id: source.source_id.clone(),
                source_title: source.title.clone(),
                canonical_url: source.canonical_url.clone(),
                chunk_index: chunk.chunk_index,
                content_hash: chunk.content_hash.clone(),
                score,
                excerpt: excerpt_around_match(&text, &query),
            });
        }
        matches.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.source_id.cmp(&right.source_id))
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
        });
        matches.truncate(limit);
        Ok(matches)
    }

    /// Record a capture on its execution's index and return the index as it now
    /// stands. Locked read-modify-write under the scope's create lock, so two
    /// reads finishing together cannot lose each other's round.
    /// `window_bytes` is the ordinary model-facing window for one page; what
    /// each new page holds past it accrues as `beyond_window_bytes`.
    pub async fn record_execution_capture(
        &self,
        principal: &str,
        workspace: &str,
        execution_id: &str,
        manifest: &WorkingSetManifest,
        window_bytes: u64,
    ) -> Result<WorkingSetExecutionIndex, ArtifactV2Error> {
        validate_identifier(execution_id, "execution id")?;
        let lock = working_set_create_lock(principal, workspace);
        let _guard = lock.lock().await;
        let mut index = self
            .execution_index(principal, workspace, execution_id)
            .await?
            .unwrap_or_else(|| WorkingSetExecutionIndex {
                schema_version: WORKING_SET_EXECUTION_INDEX_SCHEMA_VERSION,
                execution_id: execution_id.to_string(),
                scope: WorkingSetScope {
                    principal: principal.to_string(),
                    workspace: workspace.to_string(),
                },
                members: Vec::new(),
                total_source_bytes: 0,
                distinct_sources: 0,
                read_rounds: 0,
                activation: None,
                source_hashes: Vec::new(),
                narrowed_reads: 0,
                beyond_window_bytes: 0,
                decision: None,
                updated_at: Utc::now(),
            });
        if index
            .members
            .iter()
            .any(|member| member.working_set_id == manifest.working_set_id)
        {
            return Ok(index);
        }
        index.members.push(WorkingSetExecutionMember {
            working_set_id: manifest.working_set_id.clone(),
            title: manifest.title.clone(),
            source_count: manifest.sources.len() as u32,
            source_bytes: manifest.total_source_bytes,
            captured_at: manifest.created_at,
        });
        // Bounded, oldest-first: the members past the store's own retention
        // are the ones a search will skip anyway.
        if index.members.len() > MAX_WORKING_SET_EXECUTION_MEMBERS {
            let excess = index.members.len() - MAX_WORKING_SET_EXECUTION_MEMBERS;
            index.members.drain(0..excess);
        }
        index.total_source_bytes = index
            .total_source_bytes
            .saturating_add(manifest.total_source_bytes);
        for source in &manifest.sources {
            if !index
                .source_hashes
                .iter()
                .any(|hash| hash == &source.content_hash)
            {
                index.source_hashes.push(source.content_hash.clone());
                // The same page read again is the same evidence, hidden or
                // not: only a page new to the task adds to what the window
                // could not show.
                index.beyond_window_bytes = index
                    .beyond_window_bytes
                    .saturating_add(source.source_bytes.saturating_sub(window_bytes));
            }
        }
        index.distinct_sources = index.source_hashes.len() as u32;
        index.read_rounds = index.read_rounds.saturating_add(1);
        index.updated_at = Utc::now();
        self.write_execution_index(&index).await?;
        Ok(index)
    }

    /// The index for an execution, or `None` when it has captured nothing.
    pub async fn execution_index(
        &self,
        principal: &str,
        workspace: &str,
        execution_id: &str,
    ) -> Result<Option<WorkingSetExecutionIndex>, ArtifactV2Error> {
        validate_identifier(execution_id, "execution id")?;
        let path = self.execution_index_path(principal, workspace, execution_id);
        if !self.workspace.exists_path(&path).await? {
            return Ok(None);
        }
        let index: WorkingSetExecutionIndex = self
            .workspace
            .read_json_bounded_stream_path(
                &path,
                MAX_WORKING_SET_MANIFEST_BYTES,
                MAX_WORKING_SET_MANIFEST_DEPTH,
                MAX_WORKING_SET_MANIFEST_NODES,
            )
            .await?;
        if index.scope.principal != principal || index.scope.workspace != workspace {
            return Err(invalid_request(
                "execution index scope does not match the requesting scope".to_string(),
            ));
        }
        Ok(Some(index))
    }

    /// Open the working-set path for an execution. Idempotent: the first
    /// sentence is the one that opened it and is kept.
    /// Keep the routing decision on the index whichever way it went, so a
    /// check can read a refusal and its kind. Does not touch `activation`:
    /// that is `activate_execution`'s, and it is sticky.
    pub async fn record_routing_decision(
        &self,
        principal: &str,
        workspace: &str,
        execution_id: &str,
        lane: Option<&str>,
        gate_open: bool,
        activated: bool,
        reason: &str,
    ) -> Result<WorkingSetExecutionIndex, ArtifactV2Error> {
        validate_identifier(execution_id, "execution id")?;
        let lock = working_set_create_lock(principal, workspace);
        let _guard = lock.lock().await;
        let mut index = self
            .execution_index(principal, workspace, execution_id)
            .await?
            .ok_or_else(|| {
                invalid_request(format!(
                    "execution `{execution_id}` has captured nothing to decide on"
                ))
            })?;
        index.decision = Some(WorkingSetExecutionDecision {
            lane: lane.map(str::to_string),
            gate_open,
            activated,
            reason: reason.to_string(),
            decided_at: Utc::now(),
        });
        index.updated_at = Utc::now();
        self.write_execution_index(&index).await?;
        Ok(index)
    }

    pub async fn activate_execution(
        &self,
        principal: &str,
        workspace: &str,
        execution_id: &str,
        lane: &str,
        reason: &str,
    ) -> Result<WorkingSetExecutionIndex, ArtifactV2Error> {
        let lock = working_set_create_lock(principal, workspace);
        let _guard = lock.lock().await;
        let mut index = self
            .execution_index(principal, workspace, execution_id)
            .await?
            .ok_or_else(|| {
                invalid_request("an execution with no captures cannot activate".to_string())
            })?;
        if index.activation.is_none() {
            index.activation = Some(WorkingSetExecutionActivation {
                lane: lane.to_string(),
                reason: reason.to_string(),
                activated_at: Utc::now(),
            });
            index.updated_at = Utc::now();
            self.write_execution_index(&index).await?;
        }
        Ok(index)
    }

    /// Count a read the router narrowed for the model. The prompt notice
    /// waits for the first one: a path that has changed nothing the model
    /// sees must not tell the model its pages are being withheld.
    pub async fn mark_read_narrowed(
        &self,
        principal: &str,
        workspace: &str,
        execution_id: &str,
    ) -> Result<WorkingSetExecutionIndex, ArtifactV2Error> {
        let lock = working_set_create_lock(principal, workspace);
        let _guard = lock.lock().await;
        let mut index = self
            .execution_index(principal, workspace, execution_id)
            .await?
            .ok_or_else(|| {
                invalid_request("an execution with no captures has no reads".to_string())
            })?;
        index.narrowed_reads = index.narrowed_reads.saturating_add(1);
        index.updated_at = Utc::now();
        self.write_execution_index(&index).await?;
        Ok(index)
    }

    /// Search every snapshot an execution captured, merged by score. A member
    /// the lifecycle has evicted is skipped and counted, never an error.
    pub async fn search_execution(
        &self,
        principal: &str,
        workspace: &str,
        execution_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<WorkingSetExecutionSearch, ArtifactV2Error> {
        // Validated once here: a malformed query is the caller's error and
        // must surface as one, not be counted as an evicted member per member.
        normalize_query(query)?;
        let limit = limit.clamp(1, MAX_WORKING_SET_SEARCH_RESULTS);
        let Some(index) = self
            .execution_index(principal, workspace, execution_id)
            .await?
        else {
            return Ok(WorkingSetExecutionSearch {
                execution_id: execution_id.to_string(),
                members_searched: 0,
                members_evicted: 0,
                matches: Vec::new(),
            });
        };
        let mut matches = Vec::new();
        let mut searched = 0u32;
        let mut evicted = 0u32;
        // Newest first, so a tie on score prefers what the task read last.
        for member in index.members.iter().rev() {
            match self
                .search(principal, workspace, &member.working_set_id, query, limit)
                .await
            {
                Ok(found) => {
                    searched += 1;
                    matches.extend(found);
                },
                Err(ArtifactV2Error::InvalidRequest(_)) => evicted += 1,
                Err(error) => return Err(error),
            }
        }
        matches.sort_by(|left, right| right.score.cmp(&left.score));
        matches.truncate(limit);
        Ok(WorkingSetExecutionSearch {
            execution_id: execution_id.to_string(),
            members_searched: searched,
            members_evicted: evicted,
            matches,
        })
    }

    /// Beside the snapshots, never among them: `create` sweeps every entry
    /// under the working-sets root as a snapshot directory and reads its
    /// manifest, so an index kept there would break the next capture.
    fn execution_index_root(&self, principal: &str, workspace: &str) -> std::path::PathBuf {
        self.workspace
            .scope_root(principal, workspace)
            .join("research")
            .join("working_set_executions")
    }

    fn execution_index_path(
        &self,
        principal: &str,
        workspace: &str,
        execution_id: &str,
    ) -> std::path::PathBuf {
        self.execution_index_root(principal, workspace)
            .join(format!("{execution_id}.json"))
    }

    async fn write_execution_index(
        &self,
        index: &WorkingSetExecutionIndex,
    ) -> Result<(), ArtifactV2Error> {
        let root = self.execution_index_root(&index.scope.principal, &index.scope.workspace);
        self.workspace.create_dir_all_path(&root).await?;
        let path = root.join(format!("{}.json", index.execution_id));
        self.workspace.write_json_atomic_path(&path, index).await
    }

    fn working_sets_root(&self, principal: &str, workspace: &str) -> std::path::PathBuf {
        self.workspace
            .scope_root(principal, workspace)
            .join("research")
            .join("working_sets")
    }

    fn manifest_path(
        &self,
        principal: &str,
        workspace: &str,
        working_set_id: &str,
    ) -> std::path::PathBuf {
        self.working_sets_root(principal, workspace)
            .join(working_set_id)
            .join("manifest.json")
    }

    async fn cleanup_stale_staging(&self, root: &Path) -> Result<(), ArtifactV2Error> {
        let stale_before = std::time::SystemTime::now()
            .checked_sub(STALE_WORKING_SET_STAGING_AGE)
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        for entry in self.workspace.read_dir_path_or_empty(root).await? {
            if !entry.is_dir
                || !entry.file_name.starts_with('.')
                || !entry.file_name.ends_with(".staging")
            {
                continue;
            }
            let path = root.join(&entry.file_name);
            let is_stale = self
                .workspace
                .symlink_metadata_path(&path)
                .await?
                .and_then(|metadata| metadata.modified().ok())
                .is_some_and(|modified| modified <= stale_before);
            if is_stale {
                self.workspace.remove_dir_all_path(path).await?;
            }
        }
        Ok(())
    }

    async fn eviction_candidates_for_create(
        &self,
        principal: &str,
        workspace: &str,
        incoming_bytes: u64,
    ) -> Result<Vec<std::path::PathBuf>, ArtifactV2Error> {
        if incoming_bytes > MAX_WORKING_SET_SCOPE_BYTES {
            return Err(invalid_request(format!(
                "working set exceeds the per-scope {MAX_WORKING_SET_SCOPE_BYTES}-byte retention cap"
            )));
        }

        let root = self.working_sets_root(principal, workspace);
        let cutoff = working_set_retention_cutoff();
        let mut retained = Vec::new();
        let mut evictions = Vec::new();
        for entry in self.workspace.read_dir_path_or_empty(&root).await? {
            if !entry.is_dir || validate_identifier(&entry.file_name, "working set id").is_err() {
                continue;
            }
            let manifest_path = root.join(&entry.file_name).join("manifest.json");
            let manifest: WorkingSetManifest = match self
                .workspace
                .read_json_bounded_stream_path(
                    &manifest_path,
                    MAX_WORKING_SET_MANIFEST_BYTES,
                    MAX_WORKING_SET_MANIFEST_DEPTH,
                    MAX_WORKING_SET_MANIFEST_NODES,
                )
                .await
            {
                Ok(manifest) => manifest,
                Err(error @ (ArtifactV2Error::Serde(_) | ArtifactV2Error::InvalidRequest(_))) => {
                    self.quarantine_invalid_working_set(&root, &entry.file_name)
                        .await?;
                    tracing::warn!(
                        principal,
                        workspace,
                        working_set_id = %entry.file_name,
                        error = %error,
                        "quarantined an unreadable working-set manifest"
                    );
                    continue;
                },
                Err(error) => return Err(error),
            };
            if let Err(error) = validate_manifest_scope(&manifest, principal, workspace) {
                self.quarantine_invalid_working_set(&root, &entry.file_name)
                    .await?;
                tracing::warn!(
                    principal,
                    workspace,
                    working_set_id = %entry.file_name,
                    error = %error,
                    "quarantined an invalid or legacy working set"
                );
                continue;
            }
            if manifest.created_at <= cutoff {
                evictions.push(root.join(&entry.file_name));
                continue;
            }
            retained.push((
                entry.file_name,
                manifest.created_at,
                manifest.total_source_bytes,
            ));
        }

        retained.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
        let mut retained_bytes = retained
            .iter()
            .fold(0_u64, |total, entry| total.saturating_add(entry.2));
        while retained.len() >= MAX_WORKING_SETS_PER_SCOPE
            || retained_bytes.saturating_add(incoming_bytes) > MAX_WORKING_SET_SCOPE_BYTES
        {
            let Some((working_set_id, _, source_bytes)) = retained.first().cloned() else {
                return Err(invalid_request(
                    "working-set retention could not make space for a new snapshot".to_string(),
                ));
            };
            retained.remove(0);
            evictions.push(root.join(working_set_id));
            retained_bytes = retained_bytes.saturating_sub(source_bytes);
        }
        Ok(evictions)
    }

    async fn rename_path(&self, from: &Path, to: &Path) -> Result<(), ArtifactV2Error> {
        let workspace = self.workspace.clone();
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        tokio::task::spawn_blocking(move || workspace.rename_path_sync(&from, &to))
            .await
            .map_err(|error| {
                ArtifactV2Error::Io(std::io::Error::other(format!(
                    "working-set rename task failed: {error}"
                )))
            })?
    }

    async fn quarantine_invalid_working_set(
        &self,
        root: &Path,
        working_set_id: &str,
    ) -> Result<(), ArtifactV2Error> {
        let quarantine_root = root
            .parent()
            .map(|research_root| research_root.join("working_sets_quarantine"))
            .ok_or_else(|| {
                invalid_request("working-set root has no research parent".to_string())
            })?;
        self.workspace.create_dir_all_path(&quarantine_root).await?;

        let source = root.join(working_set_id);
        let suffix = blake3::hash(format!("{working_set_id}:{}", Utc::now()).as_bytes())
            .to_hex()
            .to_string();
        let destination = quarantine_root.join(format!("{working_set_id}-{suffix}"));
        self.rename_path(&source, &destination).await
    }

    async fn persist_staging(
        &self,
        staging: &Path,
        manifest: &WorkingSetManifest,
        sources: &[WorkingSetSourceInput],
    ) -> Result<(), ArtifactV2Error> {
        self.workspace.create_dir_all_path(staging).await?;
        for source in sources {
            let source_root = staging.join("sources").join(&source.source_id);
            self.workspace.create_dir_all_path(&source_root).await?;
            for (chunk_index, (start, end)) in
                chunk_ranges(&source.document.text).iter().enumerate()
            {
                let path = source_root.join(format!("{chunk_index:05}.txt"));
                self.workspace
                    .write_atomic_path(&path, source.document.text[*start..*end].as_bytes())
                    .await?;
            }
        }
        let mut raw = serde_json::to_vec_pretty(manifest).map_err(ArtifactV2Error::Serde)?;
        raw.push(b'\n');
        self.workspace
            .write_atomic_path(staging.join("manifest.json"), &raw)
            .await
    }

    async fn read_chunk_text(
        &self,
        principal: &str,
        workspace: &str,
        working_set_id: &str,
        chunk: &WorkingSetChunk,
    ) -> Result<String, ArtifactV2Error> {
        let path = self
            .working_sets_root(principal, workspace)
            .join(working_set_id)
            .join(&chunk.relative_path);
        let metadata = self
            .workspace
            .symlink_metadata_path(&path)
            .await?
            .ok_or_else(|| invalid_request("working set chunk is missing".to_string()))?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_WORKING_SET_CHUNK_BYTES as u64 {
            return Err(invalid_request(
                "working set chunk is not a regular bounded file".to_string(),
            ));
        }
        let text = self
            .workspace
            .read_to_string_bounded_path(&path, MAX_WORKING_SET_CHUNK_BYTES as u64)
            .await?;
        let actual_len = text.len() as u64;
        if actual_len != chunk.end_byte.saturating_sub(chunk.start_byte)
            || blake3::hash(text.as_bytes()).to_hex().to_string() != chunk.content_hash
        {
            return Err(invalid_request(
                "working set chunk failed integrity verification".to_string(),
            ));
        }
        Ok(text)
    }
}

fn build_manifest(
    principal: &str,
    workspace: &str,
    request: &CreateWorkingSetRequest,
    created_at: DateTime<Utc>,
) -> Result<WorkingSetManifest, ArtifactV2Error> {
    let mut total_source_bytes = 0_u64;
    let mut chunks = Vec::new();
    let mut sources = Vec::with_capacity(request.sources.len());
    for input in &request.sources {
        let ranges = chunk_ranges(&input.document.text);
        total_source_bytes = total_source_bytes.saturating_add(input.document.text.len() as u64);
        sources.push(WorkingSetSource {
            source_id: input.source_id.clone(),
            identity: input.document.identity.clone(),
            title: input.document.title.clone(),
            canonical_url: input.document.canonical_url.clone(),
            media_type: input.document.media_type.clone(),
            privacy: input.document.privacy.clone(),
            content_hash: input.document.content_hash.clone(),
            fetched_at_ms: input.document.fetched_at_ms,
            provenance: input.document.provenance.clone(),
            source_bytes: input.document.text.len() as u64,
            chunk_count: ranges.len() as u32,
        });
        for (chunk_index, (start, end)) in ranges.into_iter().enumerate() {
            let text = &input.document.text[start..end];
            chunks.push(WorkingSetChunk {
                source_id: input.source_id.clone(),
                chunk_index: chunk_index as u32,
                relative_path: format!("sources/{}/{chunk_index:05}.txt", input.source_id),
                start_byte: start as u64,
                end_byte: end as u64,
                content_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
                search_bloom: search_bloom_hex(text),
            });
        }
    }
    Ok(WorkingSetManifest {
        schema_version: WORKING_SET_SCHEMA_VERSION,
        working_set_id: request.working_set_id.clone(),
        title: request.title.clone(),
        scope: WorkingSetScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        },
        created_at,
        created_by: request.created_by.clone(),
        total_source_bytes,
        sources,
        chunks,
    })
}

fn validate_create_request(request: &CreateWorkingSetRequest) -> Result<(), ArtifactV2Error> {
    validate_identifier(&request.working_set_id, "working set id")?;
    validate_nonempty_bounded(
        &request.title,
        "working set title",
        MAX_WORKING_SET_TITLE_CHARS,
    )?;
    validate_nonempty_bounded(
        &request.created_by,
        "working set created_by",
        MAX_WORKING_SET_ACTOR_CHARS,
    )?;
    if request.sources.is_empty() {
        return Err(invalid_request(
            "working set requires at least one source".to_string(),
        ));
    }
    if request.sources.len() > MAX_WORKING_SET_SOURCES {
        return Err(invalid_request(format!(
            "working set exceeds the {MAX_WORKING_SET_SOURCES}-source limit"
        )));
    }
    let mut total_bytes = 0_usize;
    let mut total_chunks = 0_usize;
    let mut source_ids = HashSet::new();
    for source in &request.sources {
        validate_identifier(&source.source_id, "source id")?;
        if !source_ids.insert(source.source_id.as_str()) {
            return Err(invalid_request(format!(
                "working set contains duplicate source id `{}`",
                source.source_id
            )));
        }
        source
            .document
            .validate()
            .map_err(|error| invalid_request(format!("working set source is invalid: {error}")))?;
        if source.document.privacy != ContentPrivacy::Public {
            return Err(invalid_request(
                "working sets may persist only public content documents".to_string(),
            ));
        }
        if blake3::hash(source.document.text.as_bytes())
            .to_hex()
            .to_string()
            != source.document.content_hash
        {
            return Err(invalid_request(format!(
                "source `{}` content hash does not match its text",
                source.source_id
            )));
        }
        let source_bytes = source.document.text.len();
        if source_bytes > MAX_WORKING_SET_SOURCE_BYTES {
            return Err(invalid_request(format!(
                "source `{}` exceeds the {}-byte limit",
                source.source_id, MAX_WORKING_SET_SOURCE_BYTES
            )));
        }
        total_bytes = total_bytes.saturating_add(source_bytes);
        total_chunks = total_chunks.saturating_add(chunk_ranges(&source.document.text).len());
    }
    if total_bytes > MAX_WORKING_SET_TOTAL_BYTES {
        return Err(invalid_request(format!(
            "working set exceeds the {}-byte total limit",
            MAX_WORKING_SET_TOTAL_BYTES
        )));
    }
    if total_chunks > MAX_WORKING_SET_CHUNKS {
        return Err(invalid_request(format!(
            "working set exceeds the {MAX_WORKING_SET_CHUNKS}-chunk limit"
        )));
    }
    Ok(())
}

fn validate_manifest_scope(
    manifest: &WorkingSetManifest,
    principal: &str,
    workspace: &str,
) -> Result<(), ArtifactV2Error> {
    if manifest.schema_version != WORKING_SET_SCHEMA_VERSION
        || manifest.scope.principal != principal
        || manifest.scope.workspace != workspace
    {
        return Err(invalid_request(
            "working set manifest is incompatible with this scope".to_string(),
        ));
    }
    validate_identifier(&manifest.working_set_id, "working set id")?;
    validate_nonempty_bounded(
        &manifest.title,
        "working set title",
        MAX_WORKING_SET_TITLE_CHARS,
    )?;
    validate_nonempty_bounded(
        &manifest.created_by,
        "working set created_by",
        MAX_WORKING_SET_ACTOR_CHARS,
    )?;
    if manifest.sources.is_empty() || manifest.sources.len() > MAX_WORKING_SET_SOURCES {
        return Err(invalid_request(
            "working set manifest has an invalid source count".to_string(),
        ));
    }
    if manifest.chunks.len() > MAX_WORKING_SET_CHUNKS {
        return Err(invalid_request(
            "working set manifest exceeds its chunk limit".to_string(),
        ));
    }
    let mut source_ids = HashSet::new();
    let mut source_bytes = 0_u64;
    let mut expected_chunk_counts = HashMap::new();
    for source in &manifest.sources {
        validate_identifier(&source.source_id, "working set source id")?;
        if !source_ids.insert(source.source_id.as_str())
            || source.privacy != ContentPrivacy::Public
            || source.source_bytes == 0
            || source.source_bytes > MAX_WORKING_SET_SOURCE_BYTES as u64
            || source.chunk_count == 0
            || source.content_hash.is_empty()
        {
            return Err(invalid_request(
                "working set manifest contains an invalid source".to_string(),
            ));
        }
        source_bytes = source_bytes.saturating_add(source.source_bytes);
        expected_chunk_counts.insert(source.source_id.as_str(), source.chunk_count);
    }
    if source_bytes != manifest.total_source_bytes
        || source_bytes > MAX_WORKING_SET_TOTAL_BYTES as u64
    {
        return Err(invalid_request(
            "working set manifest has invalid total source bytes".to_string(),
        ));
    }
    let mut observed_chunk_counts = HashMap::new();
    let mut chunk_keys = HashSet::new();
    for chunk in &manifest.chunks {
        validate_identifier(&chunk.source_id, "working set chunk source id")?;
        let Some(expected_count) = expected_chunk_counts.get(chunk.source_id.as_str()) else {
            return Err(invalid_request(
                "working set manifest contains a chunk for an unknown source".to_string(),
            ));
        };
        if !chunk_keys.insert((chunk.source_id.as_str(), chunk.chunk_index))
            || chunk.chunk_index >= *expected_count
            || chunk.relative_path
                != format!("sources/{}/{:05}.txt", chunk.source_id, chunk.chunk_index)
            || chunk.end_byte <= chunk.start_byte
            || chunk.end_byte.saturating_sub(chunk.start_byte) > MAX_WORKING_SET_CHUNK_BYTES as u64
            || chunk.content_hash.is_empty()
            || (!chunk.search_bloom.is_empty() && !valid_search_bloom(&chunk.search_bloom))
        {
            return Err(invalid_request(
                "working set manifest contains an invalid chunk reference".to_string(),
            ));
        }
        *observed_chunk_counts
            .entry(chunk.source_id.as_str())
            .or_insert(0_u32) += 1;
    }
    if expected_chunk_counts != observed_chunk_counts {
        return Err(invalid_request(
            "working set manifest chunk counts do not match its sources".to_string(),
        ));
    }
    for source in &manifest.sources {
        let mut source_chunks = manifest
            .chunks
            .iter()
            .filter(|chunk| chunk.source_id == source.source_id)
            .collect::<Vec<_>>();
        source_chunks.sort_by_key(|chunk| chunk.chunk_index);
        let mut expected_start = 0_u64;
        for (expected_index, chunk) in source_chunks.into_iter().enumerate() {
            if chunk.chunk_index != expected_index as u32 || chunk.start_byte != expected_start {
                return Err(invalid_request(
                    "working set manifest has non-contiguous source chunk ranges".to_string(),
                ));
            }
            expected_start = chunk.end_byte;
        }
        if expected_start != source.source_bytes {
            return Err(invalid_request(
                "working set manifest chunk ranges do not cover their source".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<(), ArtifactV2Error> {
    let bytes = value.as_bytes();
    let valid_first = matches!(bytes.first(), Some(b'a'..=b'z' | b'0'..=b'9'));
    let valid_rest = bytes
        .iter()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'));
    if value.is_empty()
        || value.chars().count() > MAX_WORKING_SET_ID_CHARS
        || !valid_first
        || !valid_rest
    {
        return Err(invalid_request(format!(
            "{label} must use lowercase ASCII letters, digits, `_`, or `-` and be at most {MAX_WORKING_SET_ID_CHARS} characters"
        )));
    }
    Ok(())
}

fn validate_nonempty_bounded(
    value: &str,
    label: &str,
    maximum_chars: usize,
) -> Result<(), ArtifactV2Error> {
    if value.trim().is_empty() || value.chars().count() > maximum_chars {
        return Err(invalid_request(format!(
            "{label} must be non-empty and at most {maximum_chars} characters"
        )));
    }
    Ok(())
}

/// Derives the staging write budget from the batch the request will persist.
///
/// The chunk count is estimated arithmetically instead of by calling
/// `chunk_ranges`, which would repeat the full scan the write path is about to
/// perform. The estimate is a lower bound, because the real split also breaks on
/// character boundaries; the per-chunk allowance is sized generously enough to
/// absorb that difference. Callers reach this only after `build_manifest` has
/// enforced the source, byte, and chunk ceilings.
fn persistence_timeout(sources: &[WorkingSetSourceInput]) -> std::time::Duration {
    let chunks = sources.iter().fold(0u32, |total, source| {
        let bytes = source.document.text.len();
        let estimate = bytes.div_ceil(MAX_WORKING_SET_CHUNK_BYTES).max(1);
        total.saturating_add(u32::try_from(estimate).unwrap_or(u32::MAX))
    });
    WORKING_SET_PERSISTENCE_BASE_TIMEOUT
        .saturating_add(WORKING_SET_PERSISTENCE_TIMEOUT_PER_CHUNK.saturating_mul(chunks))
        .min(WORKING_SET_PERSISTENCE_TIMEOUT_CEILING)
}

fn chunk_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + MAX_WORKING_SET_CHUNK_BYTES).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        debug_assert!(
            end > start,
            "working-set chunk limit exceeds UTF-8 code point width"
        );
        ranges.push((start, end));
        start = end;
    }
    ranges
}

fn normalize_query(query: &str) -> Result<String, ArtifactV2Error> {
    let query = query.trim();
    if query.chars().count() < WORKING_SET_MIN_SEARCH_CHARS
        || query.chars().count() > MAX_WORKING_SET_QUERY_CHARS
    {
        return Err(invalid_request(format!(
            "working set search query must contain at least {WORKING_SET_MIN_SEARCH_CHARS} characters and at most {MAX_WORKING_SET_QUERY_CHARS} characters"
        )));
    }
    Ok(query.to_lowercase())
}

fn search_bloom_hex(text: &str) -> String {
    let mut bloom = [0_u8; WORKING_SET_SEARCH_BLOOM_BYTES];
    for position in bloom_positions(&text.to_lowercase()) {
        let byte_index = (position as usize) / 8;
        bloom[byte_index] |= 1 << ((position as usize) % 8);
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(WORKING_SET_SEARCH_BLOOM_BYTES * 2);
    for byte in bloom {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn bloom_positions(text: &str) -> Vec<u16> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut positions = HashSet::new();
    for window in characters.windows(3) {
        let mut trigram = String::new();
        for character in window {
            trigram.push(*character);
        }
        let hash = blake3::hash(trigram.as_bytes());
        let bytes = hash.as_bytes();
        positions.insert(
            u16::from_le_bytes([bytes[0], bytes[1]]) % (WORKING_SET_SEARCH_BLOOM_BYTES as u16 * 8),
        );
        positions.insert(
            u16::from_le_bytes([bytes[2], bytes[3]]) % (WORKING_SET_SEARCH_BLOOM_BYTES as u16 * 8),
        );
    }
    positions.into_iter().collect()
}

fn bloom_might_contain(encoded: &str, positions: &[u16]) -> bool {
    positions.iter().all(|position| {
        let byte_index = *position as usize / 8;
        let high = hex_nibble(encoded.as_bytes()[byte_index * 2]);
        let low = hex_nibble(encoded.as_bytes()[byte_index * 2 + 1]);
        let Some(byte) = high.zip(low).map(|(high, low)| (high << 4) | low) else {
            return false;
        };
        byte & (1 << (*position as usize % 8)) != 0
    })
}

fn valid_search_bloom(encoded: &str) -> bool {
    encoded.len() == WORKING_SET_SEARCH_BLOOM_BYTES * 2
        && encoded
            .as_bytes()
            .iter()
            .all(|byte| hex_nibble(*byte).is_some())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn excerpt_around_match(text: &str, query: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut original_offsets = Vec::with_capacity(text.chars().count());
    for (offset, character) in text.char_indices() {
        for lowered in character.to_lowercase() {
            folded.push(lowered);
            original_offsets.push(offset);
        }
    }
    let folded_byte_index = folded.find(query).unwrap_or(0);
    let folded_char_index = folded[..folded_byte_index].chars().count();
    let match_start = original_offsets
        .get(folded_char_index)
        .copied()
        .unwrap_or(0);
    let prefix = &text[..match_start];
    let start = prefix
        .char_indices()
        .rev()
        .nth(MAX_WORKING_SET_EXCERPT_CHARS / 2)
        .map(|(index, _)| index)
        .unwrap_or(0);
    text[start..]
        .chars()
        .take(MAX_WORKING_SET_EXCERPT_CHARS)
        .collect()
}

fn staging_directory_name(
    principal: &str,
    workspace: &str,
    working_set_id: &str,
    created_at: DateTime<Utc>,
) -> String {
    let seed = format!(
        "{principal}\0{workspace}\0{working_set_id}\0{}",
        created_at.timestamp_nanos_opt().unwrap_or_default()
    );
    format!(
        ".{working_set_id}-{}.staging",
        blake3::hash(seed.as_bytes()).to_hex()
    )
}

fn working_set_retention_cutoff() -> DateTime<Utc> {
    Utc::now() - Duration::days(WORKING_SET_RETENTION_DAYS)
}

fn invalid_request(message: String) -> ArtifactV2Error {
    ArtifactV2Error::InvalidRequest(message)
}

fn working_set_create_lock(principal: &str, workspace: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>> = OnceLock::new();

    let key = format!("{principal}\0{workspace}");
    let locks = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.upgrade().is_some());
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }

    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn test_document(text: String) -> ContentDocument {
        ContentDocument {
            schema_version: crate::magician_v2::content_sources::CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new("test", "source-1").expect("identity"),
            title: "Test source".to_string(),
            text,
            canonical_url: Some("https://example.com/source".to_string()),
            media_type: Some("text/plain".to_string()),
            fetched_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: String::new(),
            provenance: ContentProvenance {
                source_label: "Example".to_string(),
                source_url: Some("https://example.com/source".to_string()),
                retrieved_by: "test".to_string(),
            },
            metadata: BTreeMap::new(),
        }
    }

    fn valid_document(text: String) -> ContentDocument {
        let mut document = test_document(text);
        document.content_hash = blake3::hash(document.text.as_bytes()).to_hex().to_string();
        document
    }

    #[test]
    fn persistence_budget_scales_with_the_batch_and_holds_a_ceiling() {
        let one_chunk = vec![WorkingSetSourceInput {
            source_id: "short".to_string(),
            document: test_document("a short note".to_string()),
        }];
        assert_eq!(
            persistence_timeout(&one_chunk),
            WORKING_SET_PERSISTENCE_BASE_TIMEOUT
                .saturating_add(WORKING_SET_PERSISTENCE_TIMEOUT_PER_CHUNK)
        );

        // A batch whose staging writes cannot finish inside the base budget alone.
        // The flat deadline rejected these outright however fast they progressed.
        let many_chunks = vec![WorkingSetSourceInput {
            source_id: "long".to_string(),
            document: test_document("x".repeat(MAX_WORKING_SET_CHUNK_BYTES * 60)),
        }];
        let scaled = persistence_timeout(&many_chunks);
        assert_eq!(
            scaled,
            WORKING_SET_PERSISTENCE_BASE_TIMEOUT
                .saturating_add(WORKING_SET_PERSISTENCE_TIMEOUT_PER_CHUNK.saturating_mul(60))
        );
        assert!(scaled > WORKING_SET_PERSISTENCE_BASE_TIMEOUT);

        // Past the ceiling the budget clamps, so the deadline stays finite no
        // matter how much text an accepted request carries.
        let saturating: Vec<_> = (0..2)
            .map(|index| WorkingSetSourceInput {
                source_id: format!("bulk-{index}"),
                document: test_document("y".repeat(MAX_WORKING_SET_CHUNK_BYTES * 250)),
            })
            .collect();
        assert_eq!(
            persistence_timeout(&saturating),
            WORKING_SET_PERSISTENCE_TIMEOUT_CEILING
        );
    }

    #[tokio::test]
    async fn working_set_round_trips_chunked_evidence_and_searches_with_citations() {
        let root = std::env::temp_dir().join(format!(
            "magician-working-set-test-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let workspace = ArtifactV2Workspace::new(&root);
        let store = WorkingSetStore::new(workspace.clone());
        let document = valid_document(format!(
            "{}\nneedle evidence appears here\n{}",
            "prefix ".repeat(4_000),
            "suffix ".repeat(4_000)
        ));
        let manifest = store
            .create(
                "owner",
                "workspace",
                CreateWorkingSetRequest {
                    working_set_id: "investor-research".to_string(),
                    title: "Investor research".to_string(),
                    created_by: "web-researcher".to_string(),
                    sources: vec![WorkingSetSourceInput {
                        source_id: "firm-site".to_string(),
                        document,
                    }],
                },
            )
            .await
            .expect("create working set");
        assert!(manifest.chunks.len() > 1);

        let results = store
            .search(
                "owner",
                "workspace",
                "investor-research",
                "needle evidence",
                10,
            )
            .await
            .expect("search working set");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_id, "firm-site");
        assert_eq!(
            results[0].canonical_url.as_deref(),
            Some("https://example.com/source")
        );

        let chunk = store
            .read_chunk(
                "owner",
                "workspace",
                "investor-research",
                "firm-site",
                results[0].chunk_index,
            )
            .await
            .expect("read cited chunk");
        assert!(chunk.text.contains("needle evidence"));

        let mut malformed_manifest = manifest.clone();
        malformed_manifest.chunks[0].start_byte = 1;
        assert!(validate_manifest_scope(&malformed_manifest, "owner", "workspace").is_err());

        let chunk_path = workspace
            .scope_root("owner", "workspace")
            .join("research")
            .join("working_sets")
            .join("investor-research")
            .join("sources")
            .join("firm-site")
            .join(format!("{:05}.txt", results[0].chunk_index));
        std::fs::write(&chunk_path, vec![b'x'; MAX_WORKING_SET_CHUNK_BYTES + 1])
            .expect("replace chunk with oversized content");
        assert!(store
            .read_chunk(
                "owner",
                "workspace",
                "investor-research",
                "firm-site",
                results[0].chunk_index,
            )
            .await
            .is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn working_set_creation_locks_are_reused_per_scope() {
        let first = working_set_create_lock("owner", "workspace");
        let same_scope = working_set_create_lock("owner", "workspace");
        let other_scope = working_set_create_lock("owner", "other-workspace");

        assert!(Arc::ptr_eq(&first, &same_scope));
        assert!(!Arc::ptr_eq(&first, &other_scope));
    }

    #[tokio::test]
    async fn legacy_private_manifest_is_quarantined_without_blocking_new_capture() {
        let root = std::env::temp_dir().join(format!(
            "magician-working-set-test-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let workspace = ArtifactV2Workspace::new(&root);
        let store = WorkingSetStore::new(workspace.clone());
        store
            .create(
                "owner",
                "workspace",
                CreateWorkingSetRequest {
                    working_set_id: "legacy-private".to_string(),
                    title: "Legacy private".to_string(),
                    created_by: "test".to_string(),
                    sources: vec![WorkingSetSourceInput {
                        source_id: "source-1".to_string(),
                        document: valid_document("legacy evidence".to_string()),
                    }],
                },
            )
            .await
            .expect("create legacy working set");

        let working_sets_root = workspace
            .scope_root("owner", "workspace")
            .join("research")
            .join("working_sets");
        let manifest_path = working_sets_root
            .join("legacy-private")
            .join("manifest.json");
        let mut legacy_manifest: WorkingSetManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
                .expect("decode manifest");
        legacy_manifest.sources[0].privacy = ContentPrivacy::Private;
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&legacy_manifest).expect("encode private manifest"),
        )
        .expect("write legacy private manifest");

        store
            .create(
                "owner",
                "workspace",
                CreateWorkingSetRequest {
                    working_set_id: "fresh-public".to_string(),
                    title: "Fresh public".to_string(),
                    created_by: "test".to_string(),
                    sources: vec![WorkingSetSourceInput {
                        source_id: "source-1".to_string(),
                        document: valid_document("fresh evidence".to_string()),
                    }],
                },
            )
            .await
            .expect("new capture survives legacy manifest");

        assert!(!working_sets_root.join("legacy-private").exists());
        let quarantine_root = working_sets_root
            .parent()
            .expect("research root")
            .join("working_sets_quarantine");
        assert_eq!(
            std::fs::read_dir(&quarantine_root)
                .expect("read quarantine")
                .count(),
            1
        );
        assert!(working_sets_root.join("fresh-public").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn working_set_rejects_unsafe_source_ids_before_writing() {
        let root = std::env::temp_dir().join(format!(
            "magician-working-set-test-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let store = WorkingSetStore::new(ArtifactV2Workspace::new(&root));
        let result = store
            .create(
                "owner",
                "workspace",
                CreateWorkingSetRequest {
                    working_set_id: "safe-id".to_string(),
                    title: "Safe working set".to_string(),
                    created_by: "test".to_string(),
                    sources: vec![WorkingSetSourceInput {
                        source_id: "../unsafe".to_string(),
                        document: valid_document("content".to_string()),
                    }],
                },
            )
            .await;
        assert!(result.is_err());
        assert!(!root.exists());
    }

    #[test]
    fn unicode_casefolded_excerpt_uses_the_original_text_boundary() {
        let text = "before \u{0130}STANBUL evidence after";
        let excerpt = excerpt_around_match(text, "i\u{0307}stanbul");
        assert!(excerpt.contains("\u{0130}STANBUL evidence"));
    }

    fn source_input(source_id: &str, text: &str) -> WorkingSetSourceInput {
        let mut document = valid_document(text.to_string());
        document.identity = SourceIdentity::new("test", source_id).expect("identity");
        document.title = format!("Source {source_id}");
        WorkingSetSourceInput {
            source_id: source_id.to_string(),
            document,
        }
    }

    async fn captured(
        store: &WorkingSetStore,
        execution_id: &str,
        working_set_id: &str,
        sources: Vec<WorkingSetSourceInput>,
    ) -> WorkingSetExecutionIndex {
        let manifest = store
            .create(
                "owner",
                "default",
                CreateWorkingSetRequest {
                    working_set_id: working_set_id.to_string(),
                    title: format!("Read {working_set_id}"),
                    created_by: "web-researcher".to_string(),
                    sources,
                },
            )
            .await
            .expect("create");
        store
            .record_execution_capture(
                "owner",
                "default",
                execution_id,
                &manifest,
                TEST_WINDOW_BYTES,
            )
            .await
            .expect("record")
    }

    /// The ordinary window the tests measure "beyond" against — small, so a
    /// short body can be over it.
    const TEST_WINDOW_BYTES: u64 = 8;

    /// The index is the accumulator the activation rule reads: bytes,
    /// distinct sources and rounds accrue across an execution's captures, and
    /// the same page read twice is one source.
    #[tokio::test]
    async fn an_execution_index_accrues_what_the_activation_rule_asks_about() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = WorkingSetStore::new(ArtifactV2Workspace::new(dir.path()));

        assert!(store
            .execution_index("owner", "default", "exec-1")
            .await
            .expect("index")
            .is_none());

        let first = captured(
            &store,
            "exec-1",
            "ws-first",
            vec![
                source_input("a", "alpha body"),
                source_input("b", "beta body"),
            ],
        )
        .await;
        assert_eq!(first.read_rounds, 1);
        assert_eq!(first.distinct_sources, 2);
        assert_eq!(first.members.len(), 1);
        assert!(!first.is_activated());

        // The second read re-fetches `a` and adds `c`.
        let second = captured(
            &store,
            "exec-1",
            "ws-second",
            vec![
                source_input("a", "alpha body"),
                source_input("c", "gamma body"),
            ],
        )
        .await;
        assert_eq!(second.read_rounds, 2);
        assert_eq!(
            second.distinct_sources, 3,
            "the same content hash counts once"
        );
        assert_eq!(second.members.len(), 2);
        assert_eq!(
            second.total_source_bytes,
            first.total_source_bytes + ("alpha body".len() + "gamma body".len()) as u64
        );

        // What the window could not show: per source, bytes past the window,
        // summed — the measure the activation rule reads. "alpha body" is 10
        // bytes over an 8-byte window, so 2 beyond; "beta body" 1. The same
        // page read again is the same evidence, hidden or not, so only
        // `gamma body` adds on the second round.
        assert_eq!(first.beyond_window_bytes, 2 + 1);
        assert_eq!(second.beyond_window_bytes, first.beyond_window_bytes + 2);

        // Recording the same snapshot twice is a no-op, not a second round.
        let manifest = store
            .get("owner", "default", "ws-second")
            .await
            .expect("get");
        let again = store
            .record_execution_capture("owner", "default", "exec-1", &manifest, TEST_WINDOW_BYTES)
            .await
            .expect("record again");
        assert_eq!(again.read_rounds, 2);
        assert_eq!(again.beyond_window_bytes, second.beyond_window_bytes);
    }

    /// The decision the runtime took is on the index whichever way it went,
    /// so an operator's check can read a refusal — a closed gate, a task
    /// below the threshold — and not only an activation.
    #[tokio::test]
    async fn the_index_keeps_the_last_routing_decision_either_way() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = WorkingSetStore::new(ArtifactV2Workspace::new(dir.path()));
        captured(
            &store,
            "exec-d",
            "ws-d1",
            vec![source_input("a", "alpha body")],
        )
        .await;

        let stayed = store
            .record_routing_decision(
                "owner",
                "default",
                "exec-d",
                Some("web-research"),
                true,
                false,
                "below the threshold",
            )
            .await
            .expect("record stay");
        let decision = stayed.decision.as_ref().expect("a decision is kept");
        assert_eq!(decision.lane.as_deref(), Some("web-research"));
        assert!(decision.gate_open);
        assert!(!decision.activated);
        assert_eq!(decision.reason, "below the threshold");
        assert!(!stayed.is_activated());

        let shut = store
            .record_routing_decision(
                "owner",
                "default",
                "exec-d",
                Some("web-research"),
                false,
                false,
                "the gate is closed",
            )
            .await
            .expect("record shut");
        assert!(!shut.decision.as_ref().expect("decision").gate_open);

        let reloaded = store
            .execution_index("owner", "default", "exec-d")
            .await
            .expect("index")
            .expect("present");
        assert_eq!(
            reloaded.decision.as_ref().map(|d| d.reason.as_str()),
            Some("the gate is closed")
        );
    }

    /// Activation is a decision the execution keeps: the first sentence
    /// opened the path and a later call cannot rewrite why.
    #[tokio::test]
    async fn activation_is_sticky_and_keeps_the_sentence_that_opened_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = WorkingSetStore::new(ArtifactV2Workspace::new(dir.path()));
        captured(&store, "exec-2", "ws-1", vec![source_input("a", "alpha")]).await;

        let activated = store
            .activate_execution("owner", "default", "exec-2", "web-research", "first reason")
            .await
            .expect("activate");
        assert!(activated.is_activated());
        let again = store
            .activate_execution(
                "owner",
                "default",
                "exec-2",
                "web-research",
                "second reason",
            )
            .await
            .expect("activate again");
        assert_eq!(
            again.activation.as_ref().map(|a| a.reason.as_str()),
            Some("first reason")
        );

        let error = store
            .activate_execution("owner", "default", "exec-never", "web-research", "r")
            .await
            .expect_err("no captures, nothing to activate");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    /// The model never juggles snapshot ids: a fact in the third snapshot is
    /// found through the execution and still names where it lives.
    #[tokio::test]
    async fn an_execution_search_finds_a_fact_in_a_later_snapshot_and_attributes_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = WorkingSetStore::new(ArtifactV2Workspace::new(dir.path()));
        captured(
            &store,
            "exec-3",
            "ws-a",
            vec![source_input("a", "nothing here")],
        )
        .await;
        captured(
            &store,
            "exec-3",
            "ws-b",
            vec![source_input("b", "still nothing")],
        )
        .await;
        captured(
            &store,
            "exec-3",
            "ws-c",
            vec![source_input(
                "c",
                "the vendor quoted 14 microcents per call",
            )],
        )
        .await;

        let found = store
            .search_execution("owner", "default", "exec-3", "14 microcents", 5)
            .await
            .expect("search");
        assert_eq!(found.members_searched, 3);
        assert_eq!(found.members_evicted, 0);
        assert_eq!(found.matches.len(), 1);
        assert_eq!(found.matches[0].working_set_id, "ws-c");
        assert_eq!(found.matches[0].source_id, "c");
    }

    /// A member the lifecycle removed is skipped and counted. A task past the
    /// retained maximum evicts its own oldest, and that must read as "one
    /// snapshot gone", never as a failed search.
    #[tokio::test]
    async fn an_evicted_member_is_skipped_and_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(dir.path());
        let store = WorkingSetStore::new(workspace.clone());
        captured(
            &store,
            "exec-4",
            "ws-old",
            vec![source_input("a", "old fact here")],
        )
        .await;
        captured(
            &store,
            "exec-4",
            "ws-new",
            vec![source_input("b", "new fact here")],
        )
        .await;

        let old_root = workspace
            .scope_root("owner", "default")
            .join("research")
            .join("working_sets")
            .join("ws-old");
        std::fs::remove_dir_all(&old_root).expect("evict the old snapshot");

        let found = store
            .search_execution("owner", "default", "exec-4", "fact here", 5)
            .await
            .expect("search survives eviction");
        assert_eq!(found.members_searched, 1);
        assert_eq!(found.members_evicted, 1);
        assert_eq!(found.matches.len(), 1);
        assert_eq!(found.matches[0].working_set_id, "ws-new");

        let bad = store
            .search_execution("owner", "default", "exec-4", "   ", 5)
            .await
            .expect_err("a malformed query is the caller's error");
        assert!(matches!(bad, ArtifactV2Error::InvalidRequest(_)));
    }

    /// An execution that captured nothing searches as empty, not as an error.
    #[tokio::test]
    async fn an_unknown_execution_searches_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = WorkingSetStore::new(ArtifactV2Workspace::new(dir.path()));
        let found = store
            .search_execution("owner", "default", "exec-none", "anything", 5)
            .await
            .expect("search");
        assert_eq!(found.members_searched, 0);
        assert!(found.matches.is_empty());
    }
}
