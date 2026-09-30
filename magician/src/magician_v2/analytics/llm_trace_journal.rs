//! Durable, bounded capture pipeline for canonical LLM trace facts.
//!
//! The request path only validates and attempts a non-blocking enqueue. A
//! dedicated blocking worker appends typed envelopes to a rolling, fsynced
//! journal before invoking a materializer. The materialization watermark moves
//! only after the full uncommitted prefix succeeds, so restart replay is safe
//! and duplicate delivery never creates duplicate stable rows.

use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    time::{Duration, Instant},
};

use fs2::FileExt;
use magicllm::{LlmScope, LlmTraceContext};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;

mod index;

/// Latest indexed durable record, for honest analytics catch-up status.
pub fn read_indexed_journal_sequence(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
) -> Result<Option<u64>, LlmTraceJournalError> {
    index::indexed_sequence(workspace, scope)
}

/// Recovery is bounded independently of the historical journal size.
const REPLAY_BATCH_ROWS: usize = 256;
const REPLAY_BATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_OPEN_JOURNAL_SCOPES: usize = 8;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::{
    llm_scoped_path::{ensure_real_scoped_directory_chain, ensure_regular_file_or_missing},
    llm_trace_recorder::{
        LlmAttemptTerminalState, LlmCallTerminalState, LlmCaptureGap, LlmTraceBufferClass,
        LlmTraceJournalEnvelope, LlmTraceRecord, LlmTraceRecordError, LlmTraceRecordKey,
        LlmTraceRecordSink, TypedLlmTraceRecorder, LLM_TRACE_FACT_SCHEMA_VERSION,
        MAX_SERIALIZED_LLM_RESTRICTED_RECORD_BYTES,
    },
};
use crate::magician_v2::artifact_v2::{service::ArtifactV2Error, workspace::ArtifactV2Workspace};

pub const JOURNAL_WATERMARK_SCHEMA_VERSION: u16 = 1;
const CRITICAL_BUFFER_SATURATED: &str = "critical_buffer_saturated";
const CRITICAL_WORKER_UNAVAILABLE: &str = "critical_worker_unavailable";
const LINEAGE_BUFFER_SATURATED: &str = "lineage_buffer_saturated";
const LINEAGE_WORKER_UNAVAILABLE: &str = "lineage_worker_unavailable";
const TOOL_LINEAGE_BUFFER_SATURATED: &str = "tool_lineage_buffer_saturated";
const TOOL_LINEAGE_WORKER_UNAVAILABLE: &str = "tool_lineage_worker_unavailable";
const RESTRICTED_PAYLOAD_BUFFER_SATURATED: &str = "restricted_payload_buffer_saturated";
const RESTRICTED_PAYLOAD_WORKER_UNAVAILABLE: &str = "restricted_payload_worker_unavailable";
const WRITER_LOCK_FILE_NAME: &str = ".llm-trace-journal-writer.lock";
const RESTRICTED_WRITER_LOCK_FILE_NAME: &str = ".llm-restricted-journal-writer.lock";
pub const MAX_LLM_TRACE_JOURNAL_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES: usize =
    (MAX_SERIALIZED_LLM_RESTRICTED_RECORD_BYTES * 2) + 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmTraceJournalNamespace {
    CanonicalFacts,
    RestrictedContent,
}

#[derive(Debug, Clone)]
pub struct LlmTraceJournalConfig {
    pub namespace: LlmTraceJournalNamespace,
    pub critical_capacity: usize,
    pub lineage_capacity: usize,
    pub restricted_payload_capacity: usize,
    pub batch_row_threshold: usize,
    pub flush_interval: Duration,
    pub max_segment_bytes: u64,
    pub shutdown_timeout: Duration,
}

impl Default for LlmTraceJournalConfig {
    fn default() -> Self {
        Self {
            namespace: LlmTraceJournalNamespace::CanonicalFacts,
            critical_capacity: 8_192,
            lineage_capacity: 4_096,
            restricted_payload_capacity: 512,
            batch_row_threshold: 256,
            flush_interval: Duration::from_millis(500),
            max_segment_bytes: 32 * 1024 * 1024,
            shutdown_timeout: Duration::from_secs(10),
        }
    }
}

impl LlmTraceJournalConfig {
    fn validate(&self) -> Result<(), LlmTraceJournalError> {
        if self.critical_capacity == 0
            || self.lineage_capacity == 0
            || self.restricted_payload_capacity == 0
            || self.batch_row_threshold == 0
            || self.flush_interval.is_zero()
            || self.max_segment_bytes == 0
            || self.max_segment_bytes > MAX_LLM_TRACE_JOURNAL_SEGMENT_BYTES
            || self.shutdown_timeout.is_zero()
        {
            return Err(LlmTraceJournalError::InvalidConfiguration(
                "all capacities, thresholds and intervals must be positive and the segment size must be at most 64 MiB"
                    .to_string(),
            ));
        }
        if self.critical_capacity < self.restricted_payload_capacity {
            return Err(LlmTraceJournalError::InvalidConfiguration(
                "critical fact capacity must be at least the restricted payload capacity"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum LlmTraceJournalError {
    #[error("invalid LLM trace journal configuration: {0}")]
    InvalidConfiguration(String),
    #[error("LLM trace journal storage failed: {0}")]
    Storage(#[from] ArtifactV2Error),
    #[error("LLM trace journal JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("LLM trace journal record failed validation: {0}")]
    Record(#[from] LlmTraceRecordError),
    #[error("LLM trace journal sequence is corrupt: {0}")]
    Sequence(String),
    #[error("LLM trace journal idempotency conflict for {key}: {detail}")]
    IdempotencyConflict { key: String, detail: String },
    #[error("LLM trace materialization failed: {0}")]
    Materialization(String),
    #[error("LLM trace journal worker failed: {0}")]
    Worker(String),
    #[error("LLM trace journal index failed: {0}")]
    Index(#[from] rusqlite::Error),
}

/// Phase 2C supplies the authoritative Parquet materializer. Keeping this trait
/// at the journal boundary makes append-before-materialize ordering testable
/// without a provider or DuckDB process.
pub trait LlmTraceBatchMaterializer: Send + Sync + 'static {
    fn materialize(
        &self,
        scope: &LlmScope,
        records: &[LlmTraceJournalEnvelope],
    ) -> Result<(), LlmTraceJournalError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTraceJournalWatermark {
    pub schema_version: u16,
    pub committed_sequence: u64,
    pub committed_checksum: Option<String>,
    pub updated_at_ms: i64,
}

impl Default for LlmTraceJournalWatermark {
    fn default() -> Self {
        Self {
            schema_version: JOURNAL_WATERMARK_SCHEMA_VERSION,
            committed_sequence: 0,
            committed_checksum: None,
            updated_at_ms: 0,
        }
    }
}

#[derive(Debug)]
struct ScopeJournalState {
    index: Option<index::JournalIndex>,
    active_segment: Option<String>,
    active_segment_bytes: u64,
    next_sequence: u64,
    envelopes: Vec<LlmTraceJournalEnvelope>,
    keys: HashMap<LlmTraceRecordKey, String>,
    lifecycle: LifecycleAudit,
    watermark: LlmTraceJournalWatermark,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmTraceAppendReceipt {
    pub appended: usize,
    pub duplicates: usize,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
}

/// Synchronous store owned only by the dedicated blocking journal worker.
/// Public methods are also useful for restart/replay tools and deterministic
/// tests; callers must serialize access to one scope.
pub struct LlmTraceJournalStore {
    workspace: ArtifactV2Workspace,
    namespace: LlmTraceJournalNamespace,
    max_segment_bytes: u64,
    scopes: HashMap<LlmScope, ScopeJournalState>,
}

impl LlmTraceJournalStore {
    pub fn new(
        workspace: ArtifactV2Workspace,
        max_segment_bytes: u64,
    ) -> Result<Self, LlmTraceJournalError> {
        Self::new_with_namespace(
            workspace,
            max_segment_bytes,
            LlmTraceJournalNamespace::CanonicalFacts,
        )
    }

    pub fn new_with_namespace(
        workspace: ArtifactV2Workspace,
        max_segment_bytes: u64,
        namespace: LlmTraceJournalNamespace,
    ) -> Result<Self, LlmTraceJournalError> {
        if max_segment_bytes == 0 || max_segment_bytes > MAX_LLM_TRACE_JOURNAL_SEGMENT_BYTES {
            return Err(LlmTraceJournalError::InvalidConfiguration(
                "max_segment_bytes must be positive and at most 64 MiB".to_string(),
            ));
        }
        Ok(Self {
            workspace,
            namespace,
            max_segment_bytes,
            scopes: HashMap::new(),
        })
    }

    pub fn append_batch(
        &mut self,
        scope: &LlmScope,
        records: &[LlmTraceRecord],
    ) -> Result<LlmTraceAppendReceipt, LlmTraceJournalError> {
        if !scope.is_valid() {
            return Err(LlmTraceJournalError::Sequence(
                "journal scope must be valid".to_string(),
            ));
        }
        for record in records {
            record.validate()?;
            let restricted = matches!(
                record,
                LlmTraceRecord::CallIo(_)
                    | LlmTraceRecord::ContextBlock(_)
                    | LlmTraceRecord::ContentTombstone(_)
                    | LlmTraceRecord::ContentAccessAudit(_)
            );
            if restricted != matches!(self.namespace, LlmTraceJournalNamespace::RestrictedContent) {
                return Err(LlmTraceJournalError::Sequence(
                    "record kind does not belong to this journal namespace".to_string(),
                ));
            }
            if record.scope() != scope {
                return Err(LlmTraceJournalError::Sequence(format!(
                    "record {} belongs to {}/{}, not {}/{}",
                    record.key().idempotency_key(),
                    record.scope().principal,
                    record.scope().workspace,
                    scope.principal,
                    scope.workspace
                )));
            }
        }

        let mut state = self.take_or_load_scope(scope)?;
        if let Some(index) = state.index.as_mut() {
            let result = index.append(&self.workspace, self.max_segment_bytes, scope, records);
            if result.is_ok() {
                self.cache_scope(scope, state);
            }
            return result;
        }
        // Stable-revision conflicts are the primary durability violation and
        // must not be obscured by a secondary lifecycle difference inside the
        // conflicting payload. Exact duplicates may continue through the
        // lifecycle audit and are removed by append_with_state below.
        let mut batch_keys = HashMap::<LlmTraceRecordKey, String>::new();
        for record in records {
            let key = record.key();
            let checksum = record_checksum(record)?;
            if let Some(existing) = state.keys.get(&key).or_else(|| batch_keys.get(&key)) {
                if existing != &checksum {
                    return Err(LlmTraceJournalError::IdempotencyConflict {
                        key: key.idempotency_key(),
                        detail: "the same stable revision was delivered with a different payload"
                            .to_string(),
                    });
                }
            } else {
                batch_keys.insert(key, checksum);
            }
        }
        validate_lifecycle_consistency(&mut state.lifecycle, records)?;
        let result = self.append_with_state(scope, &mut state, records);
        if result.is_ok() {
            self.cache_scope(scope, state);
        }
        // On any failure, deliberately discard the cache. A retry rescans the
        // fsynced segments and safely observes any prefix written before the
        // failure instead of trusting stale in-memory offsets.
        result
    }

    /// Return the next bounded canonical recovery batch. Advance the commit
    /// watermark before asking for the next batch. Restricted payload recovery
    /// retains its separate prune-on-commit lifecycle.
    pub fn replay_uncommitted(
        &mut self,
        scope: &LlmScope,
    ) -> Result<Vec<LlmTraceJournalEnvelope>, LlmTraceJournalError> {
        let state = self.ensure_scope(scope)?;
        if let Some(index) = state.index.as_ref() {
            return index.replay_batch(state.watermark.committed_sequence);
        }
        Ok(state
            .envelopes
            .iter()
            .filter(|envelope| envelope.sequence > state.watermark.committed_sequence)
            .cloned()
            .collect())
    }

    pub fn watermark(
        &mut self,
        scope: &LlmScope,
    ) -> Result<LlmTraceJournalWatermark, LlmTraceJournalError> {
        Ok(self.ensure_scope(scope)?.watermark.clone())
    }

    pub fn commit_through(
        &mut self,
        scope: &LlmScope,
        sequence: u64,
    ) -> Result<LlmTraceJournalWatermark, LlmTraceJournalError> {
        self.validate_scope_storage(scope)?;
        let root = self.journal_root(scope);
        let watermark = {
            let state = self.ensure_scope(scope)?;
            if sequence < state.watermark.committed_sequence {
                return Err(LlmTraceJournalError::Sequence(format!(
                    "watermark cannot move backwards from {} to {sequence}",
                    state.watermark.committed_sequence
                )));
            }
            if sequence == state.watermark.committed_sequence {
                return Ok(state.watermark.clone());
            }
            let checksum = if let Some(index) = state.index.as_ref() {
                index.envelope(sequence)?.payload_checksum
            } else {
                state
                    .envelopes
                    .iter()
                    .find(|envelope| envelope.sequence == sequence)
                    .ok_or_else(|| {
                        LlmTraceJournalError::Sequence(format!(
                            "cannot commit unknown journal sequence {sequence}"
                        ))
                    })?
                    .payload_checksum
                    .clone()
            };
            LlmTraceJournalWatermark {
                schema_version: JOURNAL_WATERMARK_SCHEMA_VERSION,
                committed_sequence: sequence,
                committed_checksum: Some(checksum),
                updated_at_ms: now_ms(),
            }
        };
        let watermark_path = root.join("materialization-watermark.json");
        ensure_regular_file_or_missing(&watermark_path)
            .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))?;
        self.workspace
            .write_json_atomic_path_sync(watermark_path, &watermark)?;
        self.ensure_scope(scope)?.watermark = watermark.clone();
        if matches!(self.namespace, LlmTraceJournalNamespace::RestrictedContent) {
            self.prune_committed_restricted_prefix(scope)?;
        }
        Ok(watermark)
    }

    /// Append first, replay the entire uncommitted prefix, and only then advance
    /// the watermark. If materialization fails, every input remains replayable.
    pub fn append_materialize_commit(
        &mut self,
        scope: &LlmScope,
        records: &[LlmTraceRecord],
        materializer: &dyn LlmTraceBatchMaterializer,
    ) -> Result<LlmTraceAppendReceipt, LlmTraceJournalError> {
        let receipt = self.append_batch(scope, records)?;
        while self.materialize_next_batch(scope, materializer)? {}
        Ok(receipt)
    }

    /// Publish one bounded batch; callers choose whether to drain or yield to
    /// new journal writes. The checkpoint only advances after publication.
    fn materialize_next_batch(
        &mut self,
        scope: &LlmScope,
        materializer: &dyn LlmTraceBatchMaterializer,
    ) -> Result<bool, LlmTraceJournalError> {
        let uncommitted = self.replay_uncommitted(scope)?;
        if let Some(last) = uncommitted.last() {
            materializer.materialize(scope, &uncommitted)?;
            self.commit_through(scope, last.sequence)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn append_with_state(
        &self,
        scope: &LlmScope,
        state: &mut ScopeJournalState,
        records: &[LlmTraceRecord],
    ) -> Result<LlmTraceAppendReceipt, LlmTraceJournalError> {
        let mut duplicates = 0;
        let mut candidate_keys = HashMap::<LlmTraceRecordKey, String>::new();
        let mut candidates = Vec::<(String, Vec<u8>, LlmTraceJournalEnvelope)>::new();
        let mut active_segment = state.active_segment.clone();
        let mut active_bytes = state.active_segment_bytes;
        let mut next_sequence = state.next_sequence;

        for record in records {
            let checksum = record_checksum(record)?;
            let key = record.key();
            if let Some(existing) = state.keys.get(&key).or_else(|| candidate_keys.get(&key)) {
                if existing == &checksum {
                    duplicates += 1;
                    continue;
                }
                return Err(LlmTraceJournalError::IdempotencyConflict {
                    key: key.idempotency_key(),
                    detail: "the same stable revision was delivered with a different payload"
                        .to_string(),
                });
            }

            let envelope = LlmTraceJournalEnvelope::new(next_sequence, record.clone())?;
            let mut line = serde_json::to_vec(&envelope)?;
            line.push(b'\n');
            if line.len() > MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES {
                return Err(LlmTraceJournalError::Sequence(format!(
                    "serialized journal line exceeds {MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES}-byte limit"
                )));
            }
            let line_bytes = line.len() as u64;
            if active_segment.is_none()
                || (active_bytes > 0
                    && active_bytes.saturating_add(line_bytes) > self.max_segment_bytes)
            {
                active_segment = Some(segment_name(next_sequence));
                active_bytes = 0;
            }
            let segment = active_segment
                .clone()
                .expect("active journal segment is assigned above");
            active_bytes = active_bytes.saturating_add(line_bytes);
            next_sequence = next_sequence.checked_add(1).ok_or_else(|| {
                LlmTraceJournalError::Sequence("journal sequence overflow".to_string())
            })?;
            candidate_keys.insert(key, checksum);
            candidates.push((segment, line, envelope));
        }

        let first_sequence = candidates.first().map(|(_, _, row)| row.sequence);
        let last_sequence = candidates.last().map(|(_, _, row)| row.sequence);
        let root = self.journal_root(scope);
        let segments_root = root.join("segments");
        self.validate_scope_storage(scope)?;
        self.workspace.create_dir_all_path_sync(&segments_root)?;
        self.validate_scope_storage(scope)?;

        let mut offset = 0;
        while offset < candidates.len() {
            let segment = &candidates[offset].0;
            let mut body = Vec::new();
            while offset < candidates.len() && &candidates[offset].0 == segment {
                body.extend_from_slice(&candidates[offset].1);
                offset += 1;
            }
            ensure_regular_file_or_missing(&segments_root.join(segment))
                .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))?;
            self.workspace
                .append_path_sync(segments_root.join(segment), &body)?;
        }

        if !candidates.is_empty() {
            // `append_path_sync` fsyncs the file. Syncing the segment directory
            // as well makes a newly created segment name durable before the
            // state file can advertise it.
            sync_directory(&segments_root)?;
            state.active_segment = active_segment;
            state.active_segment_bytes = active_bytes;
            state.next_sequence = next_sequence;
            for (_, _, envelope) in candidates.iter().cloned() {
                state
                    .keys
                    .insert(envelope.key.clone(), envelope.payload_checksum.clone());
                state.envelopes.push(envelope);
            }
        }

        Ok(LlmTraceAppendReceipt {
            appended: candidates.len(),
            duplicates,
            first_sequence,
            last_sequence,
        })
    }

    fn take_or_load_scope(
        &mut self,
        scope: &LlmScope,
    ) -> Result<ScopeJournalState, LlmTraceJournalError> {
        match self.scopes.remove(scope) {
            Some(state) => Ok(state),
            None => self.load_scope(scope),
        }
    }

    fn ensure_scope(
        &mut self,
        scope: &LlmScope,
    ) -> Result<&mut ScopeJournalState, LlmTraceJournalError> {
        if !self.scopes.contains_key(scope) {
            let state = self.load_scope(scope)?;
            self.cache_scope(scope, state);
        }
        Ok(self
            .scopes
            .get_mut(scope)
            .expect("scope was inserted immediately above"))
    }

    fn load_scope(&self, scope: &LlmScope) -> Result<ScopeJournalState, LlmTraceJournalError> {
        self.validate_scope_storage(scope)?;
        let root = self.journal_root(scope);
        let segments_root = root.join("segments");
        let watermark_path = root.join("materialization-watermark.json");
        let watermark_exists = ensure_regular_file_or_missing(&watermark_path)
            .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))?;
        let watermark = if !watermark_exists {
            LlmTraceJournalWatermark::default()
        } else {
            self.workspace
                .read_json_path_sync::<LlmTraceJournalWatermark, _>(&watermark_path)?
        };
        validate_journal_watermark_shape(&watermark)?;
        if matches!(self.namespace, LlmTraceJournalNamespace::CanonicalFacts) {
            let index =
                index::JournalIndex::open(&self.workspace, &root, scope, self.max_segment_bytes)?;
            if watermark.committed_sequence > 0 {
                let envelope = index.envelope(watermark.committed_sequence)?;
                if watermark.committed_checksum.as_deref()
                    != Some(envelope.payload_checksum.as_str())
                {
                    return Err(LlmTraceJournalError::Sequence(
                        "watermark checksum does not match its journal envelope".into(),
                    ));
                }
            }
            return Ok(ScopeJournalState {
                index: Some(index),
                active_segment: None,
                active_segment_bytes: 0,
                next_sequence: 0,
                envelopes: Vec::new(),
                keys: HashMap::new(),
                lifecycle: LifecycleAudit::default(),
                watermark,
            });
        }
        let mut segment_names = match self.workspace.read_dir_path_sync(&segments_root) {
            Ok(entries) => {
                let mut names = Vec::new();
                for entry in entries {
                    let is_segment = entry.file_name.starts_with("segment-")
                        && entry.file_name.ends_with(".jsonl");
                    if !is_segment {
                        continue;
                    }
                    if !entry.is_file {
                        return Err(LlmTraceJournalError::Sequence(format!(
                            "journal segment must be a regular file, not a symlink or directory: {}",
                            entry.file_name
                        )));
                    }
                    if segment_first_sequence(&entry.file_name).is_none() {
                        return Err(LlmTraceJournalError::Sequence(format!(
                            "journal segment has an invalid sequence-bearing filename: {}",
                            entry.file_name
                        )));
                    }
                    names.push(entry.file_name);
                }
                names
            },
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Vec::new()
            },
            Err(error) => return Err(error.into()),
        };
        segment_names.sort();

        let mut envelopes = Vec::new();
        let mut keys = HashMap::new();
        let mut expected_sequence = None;
        let mut active_segment_bytes = 0_u64;
        for (index, segment_name) in segment_names.iter().enumerate() {
            let path = segments_root.join(segment_name);
            ensure_regular_file_or_missing(&path)
                .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))?;
            let segment_limit = self
                .max_segment_bytes
                .max(MAX_SERIALIZED_LLM_TRACE_JOURNAL_LINE_BYTES as u64);
            let bytes = read_bounded_journal_segment(&self.workspace, &path, segment_limit)?;
            let complete_len = complete_jsonl_prefix_len(&bytes);
            if complete_len != bytes.len() {
                if index + 1 != segment_names.len() {
                    return Err(LlmTraceJournalError::Sequence(format!(
                        "non-final segment {segment_name} has a partial trailing record"
                    )));
                }
                self.workspace
                    .write_atomic_path_sync(&path, &bytes[..complete_len])?;
            }
            if index + 1 == segment_names.len() {
                active_segment_bytes = complete_len as u64;
            }
            for line in bytes[..complete_len]
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                let envelope = LlmTraceJournalEnvelope::from_json_line_verified(line)?;
                let expected = match expected_sequence {
                    Some(expected) => expected,
                    None if matches!(self.namespace, LlmTraceJournalNamespace::CanonicalFacts) => 1,
                    None => {
                        let upper = watermark.committed_sequence.saturating_add(1);
                        if envelope.sequence == 0 || envelope.sequence > upper {
                            return Err(LlmTraceJournalError::Sequence(format!(
                                "restricted journal resumes at sequence {}, beyond committed prefix {}",
                                envelope.sequence, watermark.committed_sequence
                            )));
                        }
                        envelope.sequence
                    },
                };
                if envelope.sequence != expected {
                    return Err(LlmTraceJournalError::Sequence(format!(
                        "expected sequence {expected}, found {} in {segment_name}",
                        envelope.sequence
                    )));
                }
                expected_sequence = Some(expected.checked_add(1).ok_or_else(|| {
                    LlmTraceJournalError::Sequence("journal sequence overflow".to_string())
                })?);
                match keys.get(&envelope.key) {
                    Some(checksum) if checksum != &envelope.payload_checksum => {
                        return Err(LlmTraceJournalError::IdempotencyConflict {
                            key: envelope.key.idempotency_key(),
                            detail: "journal contains conflicting payload checksums".to_string(),
                        });
                    },
                    Some(_) => {},
                    None => {
                        keys.insert(envelope.key.clone(), envelope.payload_checksum.clone());
                    },
                }
                envelopes.push(envelope);
            }
        }

        validate_watermark(
            &watermark,
            &envelopes,
            matches!(self.namespace, LlmTraceJournalNamespace::RestrictedContent),
        )?;

        let mut lifecycle = LifecycleAudit::default();
        let persisted_records = envelopes
            .iter()
            .map(|envelope| envelope.record.clone())
            .collect::<Vec<_>>();
        validate_lifecycle_consistency(&mut lifecycle, &persisted_records)?;

        Ok(ScopeJournalState {
            index: None,
            active_segment: segment_names.last().cloned(),
            active_segment_bytes,
            next_sequence: expected_sequence
                .unwrap_or_else(|| watermark.committed_sequence.saturating_add(1).max(1)),
            envelopes,
            keys,
            lifecycle,
            watermark,
        })
    }

    fn journal_root(&self, scope: &LlmScope) -> std::path::PathBuf {
        match self.namespace {
            LlmTraceJournalNamespace::CanonicalFacts => self
                .workspace
                .analytics_llm_trace_journal_root(&scope.principal, &scope.workspace),
            LlmTraceJournalNamespace::RestrictedContent => self
                .workspace
                .analytics_llm_restricted_journal_root(&scope.principal, &scope.workspace),
        }
    }

    fn cache_scope(&mut self, scope: &LlmScope, state: ScopeJournalState) {
        if self.scopes.len() >= MAX_OPEN_JOURNAL_SCOPES && !self.scopes.contains_key(scope) {
            if let Some(oldest) = self.scopes.keys().next().cloned() {
                self.scopes.remove(&oldest);
            }
        }
        self.scopes.insert(scope.clone(), state);
    }

    /// Restricted content needs append-before-materialize crash safety, not an
    /// indefinite second copy after materialization. Once the watermark is
    /// durable, remove the fully committed replay prefix. A crash during file
    /// removal is safe: recovery permits only gaps wholly below the committed
    /// watermark and still requires a contiguous retained suffix.
    fn prune_committed_restricted_prefix(
        &mut self,
        scope: &LlmScope,
    ) -> Result<(), LlmTraceJournalError> {
        let root = self.journal_root(scope);
        let segments_root = root.join("segments");
        let has_uncommitted_suffix = {
            let state = self.ensure_scope(scope)?;
            state
                .envelopes
                .iter()
                .any(|envelope| envelope.sequence > state.watermark.committed_sequence)
        };
        if has_uncommitted_suffix {
            return Err(LlmTraceJournalError::Sequence(
                "cannot prune a restricted journal with an uncommitted suffix".to_string(),
            ));
        }
        match self.workspace.read_dir_path_sync(&segments_root) {
            Ok(entries) => {
                for entry in entries {
                    if entry.file_name.starts_with("segment-")
                        && entry.file_name.ends_with(".jsonl")
                    {
                        if !entry.is_file {
                            return Err(LlmTraceJournalError::Sequence(
                                "restricted journal segment is not a regular file".to_string(),
                            ));
                        }
                        self.workspace
                            .remove_file_path_sync(segments_root.join(entry.file_name))?;
                    }
                }
                sync_directory(&segments_root)?;
            },
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
        let state = self.ensure_scope(scope)?;
        state.active_segment = None;
        state.active_segment_bytes = 0;
        state.envelopes.clear();
        state.keys.clear();
        state.lifecycle = LifecycleAudit::default();
        Ok(())
    }

    fn validate_scope_storage(&self, scope: &LlmScope) -> Result<(), LlmTraceJournalError> {
        if !scope.is_valid() {
            return Err(LlmTraceJournalError::Sequence(
                "journal scope must be valid".to_string(),
            ));
        }
        ensure_real_scoped_directory_chain(self.workspace.base_root(), &self.journal_root(scope))
            .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))
    }
}

fn validate_watermark(
    watermark: &LlmTraceJournalWatermark,
    envelopes: &[LlmTraceJournalEnvelope],
    committed_prefix_may_be_pruned: bool,
) -> Result<(), LlmTraceJournalError> {
    validate_journal_watermark_shape(watermark)?;
    if watermark.committed_sequence == 0 {
        return Ok(());
    }
    let envelope = envelopes
        .iter()
        .find(|envelope| envelope.sequence == watermark.committed_sequence);
    let Some(envelope) = envelope else {
        if committed_prefix_may_be_pruned
            && envelopes
                .iter()
                .all(|row| row.sequence > watermark.committed_sequence)
        {
            return Ok(());
        }
        return Err(LlmTraceJournalError::Sequence(format!(
            "watermark references absent sequence {}",
            watermark.committed_sequence
        )));
    };
    if watermark.committed_checksum.as_deref() != Some(envelope.payload_checksum.as_str()) {
        return Err(LlmTraceJournalError::Sequence(
            "watermark checksum does not match its journal envelope".to_string(),
        ));
    }
    Ok(())
}

/// Validate the self-contained portion of a durable journal watermark.
///
/// Readers use this before advertising the committed journal prefix. The
/// journal recovery path additionally verifies that the checksum resolves to
/// the referenced envelope via [`validate_watermark`].
pub fn validate_journal_watermark_shape(
    watermark: &LlmTraceJournalWatermark,
) -> Result<(), LlmTraceJournalError> {
    if watermark.schema_version != JOURNAL_WATERMARK_SCHEMA_VERSION {
        return Err(LlmTraceJournalError::Sequence(format!(
            "unsupported watermark schema version {}",
            watermark.schema_version
        )));
    }
    if watermark.updated_at_ms < 0 {
        return Err(LlmTraceJournalError::Sequence(
            "watermark updated_at_ms must be non-negative".to_string(),
        ));
    }
    if watermark.committed_sequence == 0 {
        if watermark.committed_checksum.is_some() || watermark.updated_at_ms != 0 {
            return Err(LlmTraceJournalError::Sequence(
                "zero watermark cannot have a checksum or update timestamp".to_string(),
            ));
        }
        return Ok(());
    }
    if !watermark
        .committed_checksum
        .as_deref()
        .is_some_and(is_lower_hex_64)
        || watermark.updated_at_ms == 0
    {
        return Err(LlmTraceJournalError::Sequence(
            "committed watermark requires a positive timestamp and 64-character lowercase hexadecimal checksum"
                .to_string(),
        ));
    }
    Ok(())
}

/// Read the scoped committed watermark and prove that its sequence/checksum
/// resolves to an immutable journal envelope. This is intentionally lighter
/// than restart recovery: governed reads normally inspect only the segment
/// whose first sequence can contain the committed row, while still failing
/// closed for symlinks, malformed envelopes, absent sequences and checksum
/// drift.
pub fn read_verified_journal_watermark(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
) -> Result<Option<LlmTraceJournalWatermark>, LlmTraceJournalError> {
    if !scope.is_valid() {
        return Err(LlmTraceJournalError::Sequence(
            "journal scope must be valid".to_string(),
        ));
    }
    let root = workspace.analytics_llm_trace_journal_root(&scope.principal, &scope.workspace);
    ensure_real_scoped_directory_chain(workspace.base_root(), &root)
        .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))?;
    let watermark_path = root.join("materialization-watermark.json");
    if !ensure_regular_file_or_missing(&watermark_path)
        .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))?
    {
        return Ok(None);
    }
    let watermark = workspace
        .read_json_path_sync::<LlmTraceJournalWatermark, _>(&watermark_path)
        .map_err(LlmTraceJournalError::from)?;
    validate_journal_watermark_shape(&watermark)?;
    if watermark.committed_sequence == 0 {
        return Ok(Some(watermark));
    }

    let segments_root = root.join("segments");
    ensure_real_scoped_directory_chain(workspace.base_root(), &segments_root)
        .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))?;
    let entries = match workspace.read_dir_path_sync(&segments_root) {
        Ok(entries) => entries,
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Vec::new()
        },
        Err(error) => return Err(error.into()),
    };
    let mut candidates = Vec::new();
    for entry in entries {
        if !entry.file_name.starts_with("segment-") || !entry.file_name.ends_with(".jsonl") {
            continue;
        }
        if !entry.is_file {
            return Err(LlmTraceJournalError::Sequence(format!(
                "journal segment must be a regular file, not a symlink or directory: {}",
                entry.file_name
            )));
        }
        let first_sequence = segment_first_sequence(&entry.file_name).ok_or_else(|| {
            LlmTraceJournalError::Sequence(format!(
                "journal segment has an invalid sequence-bearing filename: {}",
                entry.file_name
            ))
        })?;
        if first_sequence <= watermark.committed_sequence {
            candidates.push((first_sequence, entry));
        }
    }
    candidates.sort_by_key(|(first_sequence, _)| std::cmp::Reverse(*first_sequence));

    for (_, entry) in candidates {
        let path = segments_root.join(&entry.file_name);
        ensure_regular_file_or_missing(&path)
            .map_err(|error| LlmTraceJournalError::Sequence(error.to_string()))?;
        let bytes =
            read_bounded_journal_segment(workspace, &path, MAX_LLM_TRACE_JOURNAL_SEGMENT_BYTES)?;
        let complete_len = complete_jsonl_prefix_len(&bytes);
        for line in bytes[..complete_len]
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let envelope = LlmTraceJournalEnvelope::from_json_line_verified(line)?;
            if envelope.sequence == watermark.committed_sequence {
                if watermark.committed_checksum.as_deref()
                    != Some(envelope.payload_checksum.as_str())
                {
                    return Err(LlmTraceJournalError::Sequence(
                        "watermark checksum does not match its journal envelope".to_string(),
                    ));
                }
                return Ok(Some(watermark));
            }
        }
    }

    Err(LlmTraceJournalError::Sequence(format!(
        "watermark references absent sequence {}",
        watermark.committed_sequence
    )))
}

fn segment_first_sequence(name: &str) -> Option<u64> {
    let suffix = name.strip_prefix("segment-")?.strip_suffix(".jsonl")?;
    let sequence = suffix.split_once('-')?.0;
    (sequence.len() == 20 && sequence.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| sequence.parse().ok())
        .flatten()
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Default)]
struct LifecycleAudit {
    calls: HashMap<String, CallLifecycleAudit>,
    attempts: HashMap<String, AttemptLifecycleAudit>,
    parents: HashMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CallLifecycleAudit {
    context: Option<LlmTraceContext>,
    operation: Option<String>,
    dispatch_job_id: Option<String>,
    started_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    terminal_state: Option<LlmCallTerminalState>,
    provider_attempt_count: Option<u32>,
    observed_attempt_indexes: HashSet<u32>,
    attempt_ids: HashSet<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AttemptLifecycleAudit {
    context: Option<LlmTraceContext>,
    operation: Option<String>,
    dispatch_job_id: Option<String>,
    effective_profile: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    model_revision: Option<String>,
    provider_attempt_index: Option<u32>,
    started_at_ms: Option<i64>,
    first_token_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    terminal_state: Option<LlmAttemptTerminalState>,
}

/// Validate invariants that cannot be checked on an isolated revision. This
/// runs against the durable history plus the candidate batch before a byte is
/// appended, preventing internally valid revisions from forming a corrupt
/// lifecycle (identity drift, attempt over-count, or parent cycles).
fn validate_lifecycle_consistency(
    audit: &mut LifecycleAudit,
    incoming: &[LlmTraceRecord],
) -> Result<(), LlmTraceJournalError> {
    let mut touched_calls = HashSet::new();
    let mut touched_attempts = HashSet::new();
    for record in incoming {
        match record {
            LlmTraceRecord::CallStarted(value) => {
                let call_id = value.context.llm_call_id.clone();
                let call = audit
                    .calls
                    .entry(value.context.llm_call_id.clone())
                    .or_default();
                register_call_identity(call, &value.context, &value.operation, None)?;
                set_equal_optional(
                    &mut call.started_at_ms,
                    Some(value.occurred_at_ms),
                    "call start timestamp",
                )?;
                register_parent(audit, &value.context)?;
                touched_calls.insert(call_id);
            },
            LlmTraceRecord::CallCompleted(value) => {
                let call_id = value.context.llm_call_id.clone();
                let call = audit
                    .calls
                    .entry(value.context.llm_call_id.clone())
                    .or_default();
                register_call_identity(
                    call,
                    &value.context,
                    &value.operation,
                    value.dispatch_job_id.as_deref(),
                )?;
                set_equal_optional(
                    &mut call.completed_at_ms,
                    Some(value.occurred_at_ms),
                    "call completion timestamp",
                )?;
                set_equal_optional(
                    &mut call.started_at_ms,
                    value.timing.created_at_ms,
                    "logical call creation timestamp",
                )?;
                set_equal_optional(
                    &mut call.provider_attempt_count,
                    Some(value.provider_attempt_count),
                    "call provider attempt count",
                )?;
                set_equal_optional(
                    &mut call.terminal_state,
                    Some(value.terminal_state),
                    "call terminal state",
                )?;
                register_parent(audit, &value.context)?;
                touched_calls.insert(call_id);
            },
            LlmTraceRecord::ProviderAttempt(value) => {
                let call_id = value.context.llm_call_id.clone();
                let attempt_id = value.provider_attempt_id.clone();
                let call = audit
                    .calls
                    .entry(value.context.llm_call_id.clone())
                    .or_default();
                register_call_identity(
                    call,
                    &value.context,
                    &value.operation,
                    value.dispatch_job_id.as_deref(),
                )?;
                call.observed_attempt_indexes
                    .insert(value.provider_attempt_index);
                call.attempt_ids.insert(attempt_id.clone());
                register_parent(audit, &value.context)?;

                let attempt = audit
                    .attempts
                    .entry(value.provider_attempt_id.clone())
                    .or_default();
                register_equal(
                    &mut attempt.context,
                    value.context.clone(),
                    "provider attempt trace context",
                )?;
                register_equal(
                    &mut attempt.operation,
                    value.operation.clone(),
                    "provider attempt operation",
                )?;
                if let Some(dispatch_job_id) = value.dispatch_job_id.clone() {
                    register_equal(
                        &mut attempt.dispatch_job_id,
                        dispatch_job_id,
                        "provider attempt dispatch job",
                    )?;
                }
                set_equal_optional(
                    &mut attempt.effective_profile,
                    value.effective_profile.clone(),
                    "provider attempt effective profile",
                )?;
                register_equal(
                    &mut attempt.provider,
                    value.provider.clone(),
                    "provider attempt provider",
                )?;
                register_equal(
                    &mut attempt.model,
                    value.model.clone(),
                    "provider attempt model",
                )?;
                set_equal_optional(
                    &mut attempt.model_revision,
                    value.model_revision.clone(),
                    "provider attempt model revision",
                )?;
                register_equal(
                    &mut attempt.provider_attempt_index,
                    value.provider_attempt_index,
                    "provider attempt ordinal",
                )?;
                if let Some(started_at_ms) = value.timing.started_at_ms {
                    register_equal(
                        &mut attempt.started_at_ms,
                        started_at_ms,
                        "provider attempt start timestamp",
                    )?;
                }
                if let Some(first_token_at_ms) = value.timing.first_token_at_ms {
                    register_equal(
                        &mut attempt.first_token_at_ms,
                        first_token_at_ms,
                        "provider attempt first-token timestamp",
                    )?;
                }
                if let Some(completed_at_ms) = value.timing.completed_at_ms {
                    register_equal(
                        &mut attempt.completed_at_ms,
                        completed_at_ms,
                        "provider attempt completion timestamp",
                    )?;
                }
                set_equal_optional(
                    &mut attempt.terminal_state,
                    value.terminal_state,
                    "provider attempt terminal state",
                )?;
                touched_calls.insert(call_id);
                touched_attempts.insert(attempt_id);
            },
            LlmTraceRecord::CaptureGap(_)
            | LlmTraceRecord::ToolLineage(_)
            | LlmTraceRecord::CallIo(_)
            | LlmTraceRecord::ContextBlock(_)
            | LlmTraceRecord::ContentTombstone(_)
            | LlmTraceRecord::ContentAccessAudit(_) => {},
        }
    }

    for call_id in &touched_calls {
        let call = audit
            .calls
            .get(call_id)
            .expect("touched call was registered above");
        if let (Some(started), Some(completed)) = (call.started_at_ms, call.completed_at_ms) {
            if started > completed {
                return lifecycle_error(call_id, "call completes before it starts");
            }
        }
        if let Some(declared) = call.provider_attempt_count {
            if call
                .observed_attempt_indexes
                .iter()
                .any(|attempt| *attempt > declared)
            {
                return lifecycle_error(
                    call_id,
                    "observed provider attempt exceeds the completed call's attempt count",
                );
            }
        }
        for attempt_id in &call.attempt_ids {
            let Some(attempt) = audit.attempts.get(attempt_id) else {
                continue;
            };
            if let (Some(call_started), Some(attempt_started)) =
                (call.started_at_ms, attempt.started_at_ms)
            {
                if attempt_started < call_started {
                    return lifecycle_error(
                        attempt_id,
                        "provider attempt starts before its logical call",
                    );
                }
            }
            if let (Some(call_completed), Some(attempt_completed)) =
                (call.completed_at_ms, attempt.completed_at_ms)
            {
                if attempt_completed > call_completed {
                    return lifecycle_error(
                        attempt_id,
                        "provider attempt completes after its logical call",
                    );
                }
            }
            if attempt.provider_attempt_index == call.provider_attempt_count {
                if let (Some(call_state), Some(attempt_state)) =
                    (call.terminal_state, attempt.terminal_state)
                {
                    let call_succeeded = call_state == LlmCallTerminalState::Succeeded;
                    let attempt_succeeded = attempt_state == LlmAttemptTerminalState::Succeeded;
                    if call_succeeded != attempt_succeeded {
                        return lifecycle_error(
                            attempt_id,
                            "final provider attempt transport outcome disagrees with its logical call",
                        );
                    }
                }
            }
        }
        reject_parent_cycle_from(&audit.parents, call_id)?;
    }

    // A parent revision may arrive after its child. Rechecking only the calls
    // touched by the current batch misses an already-persisted child whose
    // formerly-unknown parent has just become resolvable. Validate every known
    // edge after applying the candidate batch so delivery order cannot weaken
    // the trace/scope invariant.
    for child_id in audit.parents.keys() {
        validate_parent_identity(audit, child_id)?;
    }

    for attempt_id in touched_attempts {
        let attempt = audit
            .attempts
            .get(&attempt_id)
            .expect("touched attempt was registered above");
        if let (Some(started), Some(completed)) = (attempt.started_at_ms, attempt.completed_at_ms) {
            if started > completed {
                return lifecycle_error(&attempt_id, "provider attempt completes before it starts");
            }
        }
        if let Some(context) = attempt.context.as_ref() {
            if let Some(call_completed_at) = audit
                .calls
                .get(&context.llm_call_id)
                .and_then(|call| call.completed_at_ms)
            {
                if attempt
                    .completed_at_ms
                    .is_some_and(|attempt_completed| attempt_completed > call_completed_at)
                {
                    return lifecycle_error(
                        &attempt_id,
                        "provider attempt completes after its logical call",
                    );
                }
            }
        }
    }
    Ok(())
}

fn register_parent(
    audit: &mut LifecycleAudit,
    context: &LlmTraceContext,
) -> Result<(), LlmTraceJournalError> {
    if let Some(parent_id) = context.parent_call_id.as_ref() {
        match audit.parents.get(&context.llm_call_id) {
            Some(existing) if existing != parent_id => {
                return lifecycle_error(
                    &context.llm_call_id,
                    "logical call parent changed across immutable revisions",
                );
            },
            Some(_) => {},
            None => {
                audit
                    .parents
                    .insert(context.llm_call_id.clone(), parent_id.clone());
            },
        }
    }
    Ok(())
}

fn register_call_identity(
    call: &mut CallLifecycleAudit,
    context: &LlmTraceContext,
    operation: &str,
    dispatch_job_id: Option<&str>,
) -> Result<(), LlmTraceJournalError> {
    register_equal(
        &mut call.context,
        context.clone(),
        "logical call trace context",
    )?;
    register_equal(
        &mut call.operation,
        operation.to_string(),
        "logical call operation",
    )?;
    if let Some(dispatch_job_id) = dispatch_job_id {
        register_equal(
            &mut call.dispatch_job_id,
            dispatch_job_id.to_string(),
            "logical call dispatch job",
        )?;
    }
    Ok(())
}

fn register_equal<T: PartialEq>(
    target: &mut Option<T>,
    value: T,
    label: &str,
) -> Result<(), LlmTraceJournalError> {
    match target {
        Some(existing) if existing != &value => Err(LlmTraceJournalError::Sequence(format!(
            "{label} changed across immutable lifecycle revisions"
        ))),
        Some(_) => Ok(()),
        None => {
            *target = Some(value);
            Ok(())
        },
    }
}

fn set_equal_optional<T: PartialEq>(
    target: &mut Option<T>,
    value: Option<T>,
    label: &str,
) -> Result<(), LlmTraceJournalError> {
    if let Some(value) = value {
        register_equal(target, value, label)?;
    }
    Ok(())
}

fn reject_parent_cycle_from(
    parents: &HashMap<String, String>,
    start: &str,
) -> Result<(), LlmTraceJournalError> {
    let mut seen = HashSet::new();
    let mut cursor = start;
    while let Some(parent) = parents.get(cursor) {
        if !seen.insert(cursor.to_string()) {
            return lifecycle_error(start, "logical call parent graph contains a cycle");
        }
        cursor = parent;
    }
    Ok(())
}

fn validate_parent_identity(
    audit: &LifecycleAudit,
    child_id: &str,
) -> Result<(), LlmTraceJournalError> {
    let Some(parent_id) = audit.parents.get(child_id) else {
        return Ok(());
    };
    let Some(child) = audit
        .calls
        .get(child_id)
        .and_then(|call| call.context.as_ref())
    else {
        return Ok(());
    };
    let Some(parent) = audit
        .calls
        .get(parent_id)
        .and_then(|call| call.context.as_ref())
    else {
        return Ok(());
    };
    if child.scope != parent.scope || child.trace_id != parent.trace_id {
        return lifecycle_error(
            child_id,
            "known parent must share the child call's trace and scope",
        );
    }
    Ok(())
}

fn lifecycle_error(id: &str, detail: &str) -> Result<(), LlmTraceJournalError> {
    Err(LlmTraceJournalError::Sequence(format!(
        "invalid LLM lifecycle {id}: {detail}"
    )))
}

fn complete_jsonl_prefix_len(bytes: &[u8]) -> usize {
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        return bytes.len();
    }
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn read_bounded_journal_segment(
    workspace: &ArtifactV2Workspace,
    path: &std::path::Path,
    max_bytes: u64,
) -> Result<Vec<u8>, LlmTraceJournalError> {
    let probe_limit = max_bytes.checked_add(1).ok_or_else(|| {
        LlmTraceJournalError::InvalidConfiguration(
            "journal segment read limit overflowed".to_string(),
        )
    })?;
    let bytes = workspace.read_prefix_path_sync(path, probe_limit)?;
    if bytes.len() as u64 > max_bytes {
        return Err(LlmTraceJournalError::Sequence(format!(
            "journal segment exceeds {max_bytes}-byte recovery limit"
        )));
    }
    Ok(bytes)
}

fn record_checksum(record: &LlmTraceRecord) -> Result<String, LlmTraceJournalError> {
    Ok(blake3::hash(&serde_json::to_vec(record)?)
        .to_hex()
        .to_string())
}

fn sync_directory(path: &std::path::Path) -> Result<(), LlmTraceJournalError> {
    let directory = std::fs::File::open(path).map_err(ArtifactV2Error::from)?;
    directory.sync_all().map_err(ArtifactV2Error::from)?;
    Ok(())
}

fn segment_name(first_sequence: u64) -> String {
    format!("segment-{first_sequence:020}-{}.jsonl", ulid::Ulid::new())
}

/// Hold one cross-process writer lease for the entire pipeline lifetime.
///
/// `spawn_blocking` cannot be force-cancelled once it has started. Retaining
/// this advisory lock in the worker means a bounded shutdown timeout may
/// return diagnostics without allowing a replacement process/pipeline to
/// overlap the still-finishing journal writer.
fn acquire_writer_lock(
    workspace: &ArtifactV2Workspace,
    namespace: LlmTraceJournalNamespace,
) -> Result<File, LlmTraceJournalError> {
    workspace.create_dir_all_path_sync(workspace.base_root())?;
    let lock_path = workspace.base_root().join(match namespace {
        LlmTraceJournalNamespace::CanonicalFacts => WRITER_LOCK_FILE_NAME,
        LlmTraceJournalNamespace::RestrictedContent => RESTRICTED_WRITER_LOCK_FILE_NAME,
    });
    ensure_regular_file_or_missing(&lock_path)
        .map_err(|error| LlmTraceJournalError::Worker(error.to_string()))?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    // Close the check/open race on Unix: a concurrently substituted symlink
    // must never redirect the process-wide writer lease outside the runtime
    // root. The post-open metadata check also rejects special objects.
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(&lock_path).map_err(|error| {
        LlmTraceJournalError::Worker(format!(
            "opening LLM trace writer lock {} failed: {error}",
            lock_path.display()
        ))
    })?;
    if !file
        .metadata()
        .map_err(|error| {
            LlmTraceJournalError::Worker(format!(
                "inspecting LLM trace writer lock {} failed: {error}",
                lock_path.display()
            ))
        })?
        .is_file()
    {
        return Err(LlmTraceJournalError::Worker(format!(
            "LLM trace writer lock must be a regular file: {}",
            lock_path.display()
        )));
    }
    file.try_lock_exclusive().map_err(|error| {
        let detail = if error.kind() == std::io::ErrorKind::WouldBlock {
            "another LLM trace journal writer is already active".to_string()
        } else {
            format!("acquiring the LLM trace journal writer lock failed: {error}")
        };
        LlmTraceJournalError::Worker(detail)
    })?;
    Ok(file)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CaptureGapKey {
    scope: LlmScope,
    llm_call_id: Option<String>,
    operation: String,
    reason: String,
}

#[derive(Debug, Clone)]
struct PendingCaptureGap {
    gap_id: String,
    missing_record_count: u64,
    first_observed_at_ms: i64,
    last_observed_at_ms: i64,
}

impl PendingCaptureGap {
    fn into_record(self, key: CaptureGapKey, emitted_at_ms: i64) -> LlmTraceRecord {
        LlmTraceRecord::CaptureGap(LlmCaptureGap {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            gap_id: self.gap_id,
            scope: key.scope,
            llm_call_id: key.llm_call_id,
            operation: key.operation,
            reason: key.reason,
            missing_record_count: self.missing_record_count,
            first_observed_at_ms: self.first_observed_at_ms,
            last_observed_at_ms: self.last_observed_at_ms,
            emitted_at_ms: emitted_at_ms.max(self.last_observed_at_ms),
        })
    }
}

#[derive(Debug, Default)]
struct LlmTraceBufferMetrics {
    accepted: AtomicU64,
    rejected_full: AtomicU64,
    rejected_after_shutdown: AtomicU64,
    critical_depth: AtomicU64,
    lineage_depth: AtomicU64,
    restricted_payload_depth: AtomicU64,
    worker_buffered_depth: AtomicU64,
    critical_high_water: AtomicU64,
    lineage_high_water: AtomicU64,
    restricted_payload_high_water: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LlmTraceBufferStats {
    pub accepted: u64,
    pub rejected_full: u64,
    pub rejected_after_shutdown: u64,
    pub critical_depth: u64,
    pub lineage_depth: u64,
    pub restricted_payload_depth: u64,
    pub worker_buffered_depth: u64,
    pub critical_high_water: u64,
    pub lineage_high_water: u64,
    pub restricted_payload_high_water: u64,
    pub pending_gap_groups: usize,
    pub pending_missing_records: u64,
}

struct LlmTraceBufferShared {
    accepting: AtomicBool,
    /// Serializes the final admission check with channel publication. Without
    /// this gate, shutdown can drain the channels and stop the worker between
    /// a producer's accepting check and its `try_send`, losing a record that
    /// was reported as accepted.
    admission: Mutex<()>,
    pending_gaps: Mutex<HashMap<CaptureGapKey, PendingCaptureGap>>,
    metrics: LlmTraceBufferMetrics,
}

impl Default for LlmTraceBufferShared {
    fn default() -> Self {
        Self {
            accepting: AtomicBool::new(true),
            admission: Mutex::new(()),
            pending_gaps: Mutex::new(HashMap::new()),
            metrics: LlmTraceBufferMetrics::default(),
        }
    }
}

pub struct BufferedLlmTraceSink {
    critical_tx: SyncSender<LlmTraceRecord>,
    lineage_tx: SyncSender<LlmTraceRecord>,
    restricted_payload_tx: SyncSender<LlmTraceRecord>,
    shared: Arc<LlmTraceBufferShared>,
}

impl BufferedLlmTraceSink {
    pub fn stats(&self) -> LlmTraceBufferStats {
        let pending = self.shared.pending_gaps.lock();
        LlmTraceBufferStats {
            accepted: self.shared.metrics.accepted.load(Ordering::Relaxed),
            rejected_full: self.shared.metrics.rejected_full.load(Ordering::Relaxed),
            rejected_after_shutdown: self
                .shared
                .metrics
                .rejected_after_shutdown
                .load(Ordering::Relaxed),
            critical_depth: self.shared.metrics.critical_depth.load(Ordering::Relaxed),
            lineage_depth: self.shared.metrics.lineage_depth.load(Ordering::Relaxed),
            restricted_payload_depth: self
                .shared
                .metrics
                .restricted_payload_depth
                .load(Ordering::Relaxed),
            worker_buffered_depth: self
                .shared
                .metrics
                .worker_buffered_depth
                .load(Ordering::Relaxed),
            critical_high_water: self
                .shared
                .metrics
                .critical_high_water
                .load(Ordering::Relaxed),
            lineage_high_water: self
                .shared
                .metrics
                .lineage_high_water
                .load(Ordering::Relaxed),
            restricted_payload_high_water: self
                .shared
                .metrics
                .restricted_payload_high_water
                .load(Ordering::Relaxed),
            pending_gap_groups: pending.len(),
            pending_missing_records: pending.values().map(|gap| gap.missing_record_count).sum(),
        }
    }

    fn try_emit_pending_gap(&self) {
        let pending = {
            let mut gaps = self.shared.pending_gaps.lock();
            let key = gaps.keys().next().cloned();
            key.and_then(|key| gaps.remove(&key).map(|gap| (key, gap)))
        };
        let Some((key, gap)) = pending else {
            return;
        };
        let record = gap.clone().into_record(key.clone(), now_ms());
        match self.critical_tx.try_send(record) {
            Ok(()) => {
                increment_depth(
                    &self.shared.metrics.critical_depth,
                    &self.shared.metrics.critical_high_water,
                );
                self.shared.metrics.accepted.fetch_add(1, Ordering::Relaxed);
            },
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                use std::collections::hash_map::Entry;

                let mut gaps = self.shared.pending_gaps.lock();
                match gaps.entry(key) {
                    Entry::Vacant(entry) => {
                        entry.insert(gap);
                    },
                    Entry::Occupied(mut entry) => merge_pending_gap(entry.get_mut(), gap),
                }
            },
        }
    }

    fn note_gap(&self, record: &LlmTraceRecord, reason: &str, preserve_call_owner: bool) {
        let key = CaptureGapKey {
            scope: record.scope().clone(),
            llm_call_id: preserve_call_owner
                .then(|| match record {
                    LlmTraceRecord::ToolLineage(record) => Some(record.context.llm_call_id.clone()),
                    _ => None,
                })
                .flatten(),
            operation: record.operation().to_string(),
            reason: reason.to_string(),
        };
        let observed_at_ms = record.observed_at_ms().max(0);
        let mut gaps = self.shared.pending_gaps.lock();
        let gap = gaps.entry(key).or_insert_with(|| PendingCaptureGap {
            gap_id: ulid::Ulid::new().to_string(),
            missing_record_count: 0,
            first_observed_at_ms: observed_at_ms,
            last_observed_at_ms: observed_at_ms,
        });
        gap.missing_record_count = gap.missing_record_count.saturating_add(1);
        gap.first_observed_at_ms = gap.first_observed_at_ms.min(observed_at_ms);
        gap.last_observed_at_ms = gap.last_observed_at_ms.max(observed_at_ms);
    }

    fn try_send(
        &self,
        class: LlmTraceBufferClass,
        record: LlmTraceRecord,
    ) -> Result<(), LlmTraceRecordError> {
        let (tx, depth, high_water) = match class {
            LlmTraceBufferClass::Critical => (
                &self.critical_tx,
                &self.shared.metrics.critical_depth,
                &self.shared.metrics.critical_high_water,
            ),
            LlmTraceBufferClass::Lineage => (
                &self.lineage_tx,
                &self.shared.metrics.lineage_depth,
                &self.shared.metrics.lineage_high_water,
            ),
            LlmTraceBufferClass::RestrictedPayload => (
                &self.restricted_payload_tx,
                &self.shared.metrics.restricted_payload_depth,
                &self.shared.metrics.restricted_payload_high_water,
            ),
        };
        match tx.try_send(record) {
            Ok(()) => {
                increment_depth(depth, high_water);
                self.shared.metrics.accepted.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            Err(TrySendError::Full(record)) => {
                self.shared
                    .metrics
                    .rejected_full
                    .fetch_add(1, Ordering::Relaxed);
                if class == LlmTraceBufferClass::Critical {
                    self.note_gap(&record, CRITICAL_BUFFER_SATURATED, false);
                } else if class == LlmTraceBufferClass::Lineage {
                    if matches!(&record, LlmTraceRecord::ToolLineage(_)) {
                        self.note_gap(&record, TOOL_LINEAGE_BUFFER_SATURATED, true);
                    } else {
                        self.note_gap(&record, LINEAGE_BUFFER_SATURATED, false);
                    }
                } else if class == LlmTraceBufferClass::RestrictedPayload {
                    self.note_gap(&record, RESTRICTED_PAYLOAD_BUFFER_SATURATED, false);
                }
                Err(LlmTraceRecordError::Sink(format!(
                    "{class:?} LLM trace buffer is full"
                )))
            },
            Err(TrySendError::Disconnected(record)) => {
                if class == LlmTraceBufferClass::Critical {
                    self.note_gap(&record, CRITICAL_WORKER_UNAVAILABLE, false);
                } else if class == LlmTraceBufferClass::Lineage {
                    if matches!(&record, LlmTraceRecord::ToolLineage(_)) {
                        self.note_gap(&record, TOOL_LINEAGE_WORKER_UNAVAILABLE, true);
                    } else {
                        self.note_gap(&record, LINEAGE_WORKER_UNAVAILABLE, false);
                    }
                } else if class == LlmTraceBufferClass::RestrictedPayload {
                    self.note_gap(&record, RESTRICTED_PAYLOAD_WORKER_UNAVAILABLE, false);
                }
                Err(LlmTraceRecordError::Sink(
                    "LLM trace journal worker is unavailable".to_string(),
                ))
            },
        }
    }
}

impl LlmTraceRecordSink for BufferedLlmTraceSink {
    fn try_record(&self, record: LlmTraceRecord) -> Result<(), LlmTraceRecordError> {
        let _admission = self.shared.admission.lock();
        if !self.shared.accepting.load(Ordering::Acquire) {
            self.shared
                .metrics
                .rejected_after_shutdown
                .fetch_add(1, Ordering::Relaxed);
            return Err(LlmTraceRecordError::Sink(
                "LLM trace pipeline is shutting down".to_string(),
            ));
        }
        self.try_emit_pending_gap();
        self.try_send(record.buffer_class(), record)
    }
}

fn merge_pending_gap(target: &mut PendingCaptureGap, incoming: PendingCaptureGap) {
    target.missing_record_count = target
        .missing_record_count
        .saturating_add(incoming.missing_record_count);
    target.first_observed_at_ms = target
        .first_observed_at_ms
        .min(incoming.first_observed_at_ms);
    target.last_observed_at_ms = target.last_observed_at_ms.max(incoming.last_observed_at_ms);
}

fn increment_depth(depth: &AtomicU64, high_water: &AtomicU64) {
    let current = depth.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    let mut observed = high_water.load(Ordering::Relaxed);
    while current > observed {
        match high_water.compare_exchange_weak(
            observed,
            current,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => observed = actual,
        }
    }
}

fn decrement_depth(depth: &AtomicU64) {
    let _ = depth.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_sub(1))
    });
}

enum WorkerControl {
    Flush(oneshot::Sender<Result<(), String>>),
    Shutdown(oneshot::Sender<LlmTraceShutdownReport>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LlmTraceShutdownReport {
    pub drained_records: u64,
    pub emitted_gap_records: u64,
    pub remaining_buffered_records: u64,
    pub remaining_missing_records: u64,
    pub rejected_after_shutdown: u64,
    pub flush_error: Option<String>,
    pub timed_out: bool,
}

pub struct LlmTraceDurablePipeline {
    recorder: Arc<TypedLlmTraceRecorder>,
    sink: Arc<BufferedLlmTraceSink>,
    control_tx: mpsc::Sender<WorkerControl>,
    worker: Option<tokio::task::JoinHandle<()>>,
    shutdown_timeout: Duration,
}

impl Drop for LlmTraceDurablePipeline {
    fn drop(&mut self) {
        // `shutdown()` is the preferred path because it returns a durable
        // drain report. Still, early-return and panic paths must not leave an
        // externally retained recorder accepting records after the owner and
        // its control channel disappear. Closing admission under the same
        // gate used by `try_record` gives the worker a stable final channel
        // prefix to drain when it observes control-channel disconnect.
        let _admission = self.sink.shared.admission.lock();
        self.sink.shared.accepting.store(false, Ordering::Release);
    }
}

impl LlmTraceDurablePipeline {
    pub fn start(
        workspace: ArtifactV2Workspace,
        config: LlmTraceJournalConfig,
        materializer: Arc<dyn LlmTraceBatchMaterializer>,
        recovery_scopes: Vec<LlmScope>,
    ) -> Result<Self, LlmTraceJournalError> {
        config.validate()?;
        // Acquire before creating the worker. The descriptor moves into the
        // blocking worker and therefore also protects a timed-out shutdown
        // tail from an overlapping replacement writer.
        let writer_lock = acquire_writer_lock(&workspace, config.namespace)?;
        let (critical_tx, critical_rx) = mpsc::sync_channel(config.critical_capacity);
        let (lineage_tx, lineage_rx) = mpsc::sync_channel(config.lineage_capacity);
        let (restricted_payload_tx, restricted_payload_rx) =
            mpsc::sync_channel(config.restricted_payload_capacity);
        let (control_tx, control_rx) = mpsc::channel();
        let shared = Arc::new(LlmTraceBufferShared::default());
        let sink = Arc::new(BufferedLlmTraceSink {
            critical_tx,
            lineage_tx,
            restricted_payload_tx,
            shared: shared.clone(),
        });
        let recorder = Arc::new(TypedLlmTraceRecorder::new(sink.clone()));
        let shutdown_timeout = config.shutdown_timeout;
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let worker = tokio::task::spawn_blocking(move || {
            journal_worker(
                workspace,
                config,
                materializer,
                recovery_scopes,
                shared,
                critical_rx,
                lineage_rx,
                restricted_payload_rx,
                control_rx,
                startup_tx,
                writer_lock,
            )
        });
        // A running `spawn_blocking` task cannot be cancelled by aborting its
        // JoinHandle. Returning on a synthetic startup timeout would therefore
        // leave an unowned journal writer alive while a caller could start a
        // second pipeline against the same scoped files. Startup is a strict
        // writer barrier: wait for validation to finish or for the worker to
        // terminate. Only the already-running pipeline's shutdown is bounded.
        match startup_rx.recv() {
            Ok(Ok(())) => {},
            Ok(Err(error)) => {
                sink.shared.accepting.store(false, Ordering::Release);
                return Err(LlmTraceJournalError::Worker(error));
            },
            Err(error) => {
                sink.shared.accepting.store(false, Ordering::Release);
                return Err(LlmTraceJournalError::Worker(format!(
                    "journal worker terminated before startup recovery completed: {error}"
                )));
            },
        }
        Ok(Self {
            recorder,
            sink,
            control_tx,
            worker: Some(worker),
            shutdown_timeout,
        })
    }

    pub fn recorder(&self) -> Arc<TypedLlmTraceRecorder> {
        self.recorder.clone()
    }

    pub fn sink(&self) -> Arc<BufferedLlmTraceSink> {
        self.sink.clone()
    }

    pub async fn flush(&self) -> Result<(), LlmTraceJournalError> {
        let (tx, rx) = oneshot::channel();
        {
            // Sending the barrier while holding the admission gate guarantees
            // every record accepted before this call is already in a worker
            // channel when the worker observes the control message.
            let _admission = self.sink.shared.admission.lock();
            self.control_tx
                .send(WorkerControl::Flush(tx))
                .map_err(|_| {
                    LlmTraceJournalError::Worker("worker control channel closed".to_string())
                })?;
        }
        rx.await
            .map_err(|_| LlmTraceJournalError::Worker("worker dropped flush receipt".to_string()))?
            .map_err(LlmTraceJournalError::Worker)
    }

    pub async fn shutdown(mut self) -> LlmTraceShutdownReport {
        let (tx, rx) = oneshot::channel();
        {
            // Close admission and publish the shutdown barrier atomically with
            // respect to producers. No accepted record can arrive after the
            // worker's final channel drain.
            let _admission = self.sink.shared.admission.lock();
            self.sink.shared.accepting.store(false, Ordering::Release);
            if self.control_tx.send(WorkerControl::Shutdown(tx)).is_err() {
                let stats = self.sink.stats();
                return shutdown_failure_report(
                    &stats,
                    "worker control channel closed before shutdown",
                    false,
                );
            }
        }
        match tokio::time::timeout(self.shutdown_timeout, rx).await {
            Ok(Ok(report)) => {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.await;
                }
                report
            },
            Ok(Err(_)) => shutdown_failure_report(
                &self.sink.stats(),
                "worker dropped shutdown receipt",
                false,
            ),
            Err(_) => {
                // spawn_blocking work cannot be force-cancelled once running;
                // abort detaches the join while the worker completes safely.
                if let Some(worker) = self.worker.take() {
                    worker.abort();
                }
                let stats = self.sink.stats();
                LlmTraceShutdownReport {
                    remaining_buffered_records: stats
                        .critical_depth
                        .saturating_add(stats.lineage_depth)
                        .saturating_add(stats.restricted_payload_depth)
                        .saturating_add(stats.worker_buffered_depth),
                    remaining_missing_records: stats.pending_missing_records,
                    rejected_after_shutdown: stats.rejected_after_shutdown,
                    flush_error: Some("bounded shutdown deadline elapsed".to_string()),
                    timed_out: true,
                    ..LlmTraceShutdownReport::default()
                }
            },
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn journal_worker(
    workspace: ArtifactV2Workspace,
    config: LlmTraceJournalConfig,
    materializer: Arc<dyn LlmTraceBatchMaterializer>,
    recovery_scopes: Vec<LlmScope>,
    shared: Arc<LlmTraceBufferShared>,
    critical_rx: Receiver<LlmTraceRecord>,
    lineage_rx: Receiver<LlmTraceRecord>,
    restricted_payload_rx: Receiver<LlmTraceRecord>,
    control_rx: Receiver<WorkerControl>,
    startup_tx: SyncSender<Result<(), String>>,
    _writer_lock: File,
) {
    let mut store = match LlmTraceJournalStore::new_with_namespace(
        workspace,
        config.max_segment_bytes,
        config.namespace,
    ) {
        Ok(store) => store,
        Err(error) => {
            tracing::error!(target: "analytics", error = %error, "LLM trace journal failed to start");
            let _ = startup_tx.send(Err(error.to_string()));
            return;
        },
    };
    // Writer readiness requires a consistent durable index. Historical
    // Parquet catch-up is independent and runs in bounded batches below.
    for scope in &recovery_scopes {
        if let Err(error) = store.watermark(scope) {
            tracing::error!(target: "analytics", error = %error, "LLM trace journal startup validation failed");
            let _ = startup_tx.send(Err(error.to_string()));
            return;
        }
    }
    if startup_tx.send(Ok(())).is_err() {
        return;
    }
    let mut buffered = HashMap::<LlmScope, Vec<LlmTraceRecord>>::new();
    let mut dirty_scopes = recovery_scopes.into_iter().collect::<HashSet<_>>();
    let mut drained_records = 0_u64;
    let mut last_flush = Instant::now();

    loop {
        match control_rx.try_recv() {
            Ok(WorkerControl::Flush(receipt)) => {
                drain_available(
                    &config,
                    &shared,
                    &critical_rx,
                    &lineage_rx,
                    &restricted_payload_rx,
                    &mut buffered,
                    &mut dirty_scopes,
                    &mut drained_records,
                );
                queue_pending_gap_records(&shared, &mut buffered, &mut dirty_scopes);
                let result = flush_all(
                    &mut store,
                    materializer.as_ref(),
                    &mut buffered,
                    &mut dirty_scopes,
                    &shared,
                    true,
                );
                last_flush = Instant::now();
                let _ = receipt.send(result.map_err(|error| error.to_string()));
                continue;
            },
            Ok(WorkerControl::Shutdown(receipt)) => {
                drain_available(
                    &config,
                    &shared,
                    &critical_rx,
                    &lineage_rx,
                    &restricted_payload_rx,
                    &mut buffered,
                    &mut dirty_scopes,
                    &mut drained_records,
                );
                let emitted_gap_records =
                    queue_pending_gap_records(&shared, &mut buffered, &mut dirty_scopes);
                let flush_result = flush_all(
                    &mut store,
                    materializer.as_ref(),
                    &mut buffered,
                    &mut dirty_scopes,
                    &shared,
                    true,
                );
                let stats = buffer_stats(&shared);
                let remaining_buffered_records = buffered
                    .values()
                    .map(|records| records.len() as u64)
                    .sum::<u64>()
                    .saturating_add(stats.critical_depth)
                    .saturating_add(stats.lineage_depth)
                    .saturating_add(stats.restricted_payload_depth);
                let _ = receipt.send(LlmTraceShutdownReport {
                    drained_records,
                    emitted_gap_records,
                    remaining_buffered_records,
                    remaining_missing_records: stats.pending_missing_records,
                    rejected_after_shutdown: stats.rejected_after_shutdown,
                    flush_error: flush_result.err().map(|error| error.to_string()),
                    timed_out: false,
                });
                break;
            },
            Err(TryRecvError::Disconnected) => {
                // Defensive teardown path for an owner dropped without the
                // explicit async shutdown handshake. `Drop` closes admission
                // before the control sender disappears, so this drain sees a
                // stable final prefix and cannot race a producer that still
                // holds a cloned recorder.
                drain_available(
                    &config,
                    &shared,
                    &critical_rx,
                    &lineage_rx,
                    &restricted_payload_rx,
                    &mut buffered,
                    &mut dirty_scopes,
                    &mut drained_records,
                );
                queue_pending_gap_records(&shared, &mut buffered, &mut dirty_scopes);
                if let Err(error) = flush_all(
                    &mut store,
                    materializer.as_ref(),
                    &mut buffered,
                    &mut dirty_scopes,
                    &shared,
                    true,
                ) {
                    tracing::error!(
                        target: "analytics",
                        error = %error,
                        "LLM trace journal owner disappeared and the defensive final flush failed; journal replay remains authoritative"
                    );
                }
                break;
            },
            Err(TryRecvError::Empty) => {},
        }

        if last_flush.elapsed() >= config.flush_interval {
            queue_pending_gap_records(&shared, &mut buffered, &mut dirty_scopes);
            if let Err(error) = flush_all(
                &mut store,
                materializer.as_ref(),
                &mut buffered,
                &mut dirty_scopes,
                &shared,
                false,
            ) {
                tracing::warn!(target: "analytics", error = %error, "LLM trace journal interval flush failed; retained for replay");
            }
            last_flush = Instant::now();
            continue;
        }

        let worker_limit = config
            .critical_capacity
            .saturating_add(config.lineage_capacity)
            .saturating_add(config.restricted_payload_capacity) as u64;
        if shared.metrics.worker_buffered_depth.load(Ordering::Relaxed) >= worker_limit {
            let _ = flush_all(
                &mut store,
                materializer.as_ref(),
                &mut buffered,
                &mut dirty_scopes,
                &shared,
                false,
            );
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        let next = critical_rx
            .try_recv()
            .map(|record| (record, LlmTraceBufferClass::Critical))
            .or_else(|_| {
                lineage_rx
                    .try_recv()
                    .map(|record| (record, LlmTraceBufferClass::Lineage))
            })
            .or_else(|_| {
                restricted_payload_rx
                    .try_recv()
                    .map(|record| (record, LlmTraceBufferClass::RestrictedPayload))
            });
        if let Ok((record, class)) = next {
            decrement_class_depth(&shared.metrics, class);
            drained_records = drained_records.saturating_add(1);
            let scope = record.scope().clone();
            let reached_threshold = {
                let scope_buffer = buffered.entry(scope.clone()).or_default();
                scope_buffer.push(record);
                scope_buffer.len() >= config.batch_row_threshold
            };
            shared
                .metrics
                .worker_buffered_depth
                .fetch_add(1, Ordering::Relaxed);
            dirty_scopes.insert(scope.clone());
            if reached_threshold {
                queue_pending_gap_records(&shared, &mut buffered, &mut dirty_scopes);
                if let Err(error) = flush_scope(
                    &mut store,
                    materializer.as_ref(),
                    &scope,
                    &mut buffered,
                    &mut dirty_scopes,
                    &shared,
                ) {
                    tracing::warn!(
                        target: "analytics",
                        principal = scope.principal,
                        workspace = scope.workspace,
                        error = %error,
                        "LLM trace journal batch flush failed; retained for replay"
                    );
                }
            }
            continue;
        }

        let remaining = config.flush_interval.saturating_sub(last_flush.elapsed());
        let wait = remaining.min(Duration::from_millis(10));
        if let Ok(record) = critical_rx.recv_timeout(wait) {
            decrement_class_depth(&shared.metrics, LlmTraceBufferClass::Critical);
            drained_records = drained_records.saturating_add(1);
            let scope = record.scope().clone();
            let reached_threshold = {
                let scope_buffer = buffered.entry(scope.clone()).or_default();
                scope_buffer.push(record);
                scope_buffer.len() >= config.batch_row_threshold
            };
            shared
                .metrics
                .worker_buffered_depth
                .fetch_add(1, Ordering::Relaxed);
            dirty_scopes.insert(scope.clone());
            if reached_threshold {
                queue_pending_gap_records(&shared, &mut buffered, &mut dirty_scopes);
                if let Err(error) = flush_scope(
                    &mut store,
                    materializer.as_ref(),
                    &scope,
                    &mut buffered,
                    &mut dirty_scopes,
                    &shared,
                ) {
                    tracing::warn!(
                        target: "analytics",
                        principal = scope.principal,
                        workspace = scope.workspace,
                        error = %error,
                        "LLM trace journal batch flush failed; retained for replay"
                    );
                }
            }
        }
    }
}

fn shutdown_failure_report(
    stats: &LlmTraceBufferStats,
    message: &str,
    timed_out: bool,
) -> LlmTraceShutdownReport {
    LlmTraceShutdownReport {
        remaining_buffered_records: stats
            .critical_depth
            .saturating_add(stats.lineage_depth)
            .saturating_add(stats.restricted_payload_depth)
            .saturating_add(stats.worker_buffered_depth),
        remaining_missing_records: stats.pending_missing_records,
        rejected_after_shutdown: stats.rejected_after_shutdown,
        flush_error: Some(message.to_string()),
        timed_out,
        ..LlmTraceShutdownReport::default()
    }
}

fn drain_available(
    config: &LlmTraceJournalConfig,
    shared: &LlmTraceBufferShared,
    critical_rx: &Receiver<LlmTraceRecord>,
    lineage_rx: &Receiver<LlmTraceRecord>,
    restricted_payload_rx: &Receiver<LlmTraceRecord>,
    buffered: &mut HashMap<LlmScope, Vec<LlmTraceRecord>>,
    dirty_scopes: &mut HashSet<LlmScope>,
    drained_records: &mut u64,
) {
    for (receiver, class, capacity) in [
        (
            critical_rx,
            LlmTraceBufferClass::Critical,
            config.critical_capacity,
        ),
        (
            lineage_rx,
            LlmTraceBufferClass::Lineage,
            config.lineage_capacity,
        ),
        (
            restricted_payload_rx,
            LlmTraceBufferClass::RestrictedPayload,
            config.restricted_payload_capacity,
        ),
    ] {
        // Every pre-barrier record fits in its lane's capacity. FIFO draining
        // that many covers the accepted prefix even if producers keep sending.
        // Use capacity rather than approximate depth metrics as the hard bound.
        for _ in 0..capacity {
            let Ok(record) = receiver.try_recv() else {
                break;
            };
            decrement_class_depth(&shared.metrics, class);
            *drained_records = (*drained_records).saturating_add(1);
            let scope = record.scope().clone();
            buffered.entry(scope.clone()).or_default().push(record);
            shared
                .metrics
                .worker_buffered_depth
                .fetch_add(1, Ordering::Relaxed);
            dirty_scopes.insert(scope);
        }
    }
}

fn flush_all(
    store: &mut LlmTraceJournalStore,
    materializer: &dyn LlmTraceBatchMaterializer,
    buffered: &mut HashMap<LlmScope, Vec<LlmTraceRecord>>,
    dirty_scopes: &mut HashSet<LlmScope>,
    shared: &LlmTraceBufferShared,
    drain: bool,
) -> Result<(), LlmTraceJournalError> {
    let scopes = dirty_scopes.iter().cloned().collect::<Vec<_>>();
    let mut first_error = None;
    for scope in scopes {
        loop {
            if let Err(error) =
                flush_scope(store, materializer, &scope, buffered, dirty_scopes, shared)
            {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                break;
            }
            if !drain || !dirty_scopes.contains(&scope) {
                break;
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn flush_scope(
    store: &mut LlmTraceJournalStore,
    materializer: &dyn LlmTraceBatchMaterializer,
    scope: &LlmScope,
    buffered: &mut HashMap<LlmScope, Vec<LlmTraceRecord>>,
    dirty_scopes: &mut HashSet<LlmScope>,
    shared: &LlmTraceBufferShared,
) -> Result<(), LlmTraceJournalError> {
    if let Some(records) = buffered.get(scope) {
        // Release accepted input as soon as it is journaled. A failed or slow
        // materializer leaves its backlog on disk, never in this Vec.
        store.append_batch(scope, records)?;
    }
    let removed = buffered
        .remove(scope)
        .map(|records| records.len() as u64)
        .unwrap_or(0);
    let _ = shared.metrics.worker_buffered_depth.fetch_update(
        Ordering::Relaxed,
        Ordering::Relaxed,
        |value| Some(value.saturating_sub(removed)),
    );
    if !store.materialize_next_batch(scope, materializer)? {
        dirty_scopes.remove(scope);
    }
    Ok(())
}

fn take_pending_gap_records(shared: &LlmTraceBufferShared) -> Vec<LlmTraceRecord> {
    let emitted_at_ms = now_ms();
    let mut pending = shared.pending_gaps.lock();
    pending
        .drain()
        .map(|(key, gap)| gap.into_record(key, emitted_at_ms))
        .collect()
}

fn queue_pending_gap_records(
    shared: &LlmTraceBufferShared,
    buffered: &mut HashMap<LlmScope, Vec<LlmTraceRecord>>,
    dirty_scopes: &mut HashSet<LlmScope>,
) -> u64 {
    let gaps = take_pending_gap_records(shared);
    let count = gaps.len() as u64;
    for gap in gaps {
        let scope = gap.scope().clone();
        buffered.entry(scope.clone()).or_default().push(gap);
        shared
            .metrics
            .worker_buffered_depth
            .fetch_add(1, Ordering::Relaxed);
        dirty_scopes.insert(scope);
    }
    count
}

fn decrement_class_depth(metrics: &LlmTraceBufferMetrics, class: LlmTraceBufferClass) {
    match class {
        LlmTraceBufferClass::Critical => decrement_depth(&metrics.critical_depth),
        LlmTraceBufferClass::Lineage => decrement_depth(&metrics.lineage_depth),
        LlmTraceBufferClass::RestrictedPayload => {
            decrement_depth(&metrics.restricted_payload_depth)
        },
    }
}

fn buffer_stats(shared: &LlmTraceBufferShared) -> LlmTraceBufferStats {
    let pending = shared.pending_gaps.lock();
    LlmTraceBufferStats {
        accepted: shared.metrics.accepted.load(Ordering::Relaxed),
        rejected_full: shared.metrics.rejected_full.load(Ordering::Relaxed),
        rejected_after_shutdown: shared
            .metrics
            .rejected_after_shutdown
            .load(Ordering::Relaxed),
        critical_depth: shared.metrics.critical_depth.load(Ordering::Relaxed),
        lineage_depth: shared.metrics.lineage_depth.load(Ordering::Relaxed),
        restricted_payload_depth: shared
            .metrics
            .restricted_payload_depth
            .load(Ordering::Relaxed),
        worker_buffered_depth: shared.metrics.worker_buffered_depth.load(Ordering::Relaxed),
        critical_high_water: shared.metrics.critical_high_water.load(Ordering::Relaxed),
        lineage_high_water: shared.metrics.lineage_high_water.load(Ordering::Relaxed),
        restricted_payload_high_water: shared
            .metrics
            .restricted_payload_high_water
            .load(Ordering::Relaxed),
        pending_gap_groups: pending.len(),
        pending_missing_records: pending.values().map(|gap| gap.missing_record_count).sum(),
    }
}

/// A temp directory whose path contains no symlink.
///
/// macOS `std::env::temp_dir()` is `/var/folders/…`, and `/var` is a symlink
/// to `/private/var`. The journal index opens SQLite with
/// `SQLITE_OPEN_NOFOLLOW`, which refuses that path (extended code 1550).
/// Canonicalizing the root keeps that hardening under test.
#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn canonical_tempdir() -> tempfile::TempDir {
    let root = std::fs::canonicalize(std::env::temp_dir()).expect("canonical temporary root");
    tempfile::tempdir_in(root).expect("temporary directory")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex as StdMutex,
    };

    use magicllm::{LlmTraceContext, LlmWorkloadClass};
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::analytics::llm_trace_recorder::{
        LlmCallCompleted, LlmCallIoPhase, LlmCallIoRecord, LlmCallStarted, LlmCaptureFact,
        LlmCaptureMode, LlmCaptureStatus, LlmImmediateValidationFact, LlmPricingFact,
        LlmProviderAttemptEvent, LlmProviderAttemptPhase, LlmRedactionReport, LlmTimingFact,
        LlmTokenUsageFact, LlmToolLineageRecord, LlmTraceRecorder,
        LLM_RESTRICTED_CONTENT_SCHEMA_VERSION, LLM_TRACE_FACT_SCHEMA_VERSION,
    };

    #[test]
    fn config_rejects_payload_capacity_larger_than_critical_facts() {
        let config = LlmTraceJournalConfig {
            critical_capacity: 1,
            restricted_payload_capacity: 2,
            ..LlmTraceJournalConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(LlmTraceJournalError::InvalidConfiguration(message))
                if message.contains("critical fact capacity")
        ));
    }

    #[test]
    fn config_and_store_reject_unbounded_segment_sizes() {
        let config = LlmTraceJournalConfig {
            max_segment_bytes: MAX_LLM_TRACE_JOURNAL_SEGMENT_BYTES + 1,
            ..LlmTraceJournalConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(LlmTraceJournalError::InvalidConfiguration(message))
                if message.contains("at most 64 MiB")
        ));

        let (_temp, workspace) = fixture();
        assert!(matches!(
            LlmTraceJournalStore::new(workspace, MAX_LLM_TRACE_JOURNAL_SEGMENT_BYTES + 1),
            Err(LlmTraceJournalError::InvalidConfiguration(message))
                if message.contains("at most 64 MiB")
        ));
    }

    #[test]
    fn bounded_segment_reader_rejects_before_loading_an_unbounded_file() {
        let (_temp, workspace) = fixture();
        let path = workspace.base_root().join("oversized-segment.jsonl");
        workspace
            .write_atomic_path_sync(&path, &[b'x'; 129])
            .expect("fixture");

        let error = read_bounded_journal_segment(&workspace, &path, 128)
            .expect_err("oversized segment must fail closed");
        assert!(matches!(
            error,
            LlmTraceJournalError::Sequence(message)
                if message.contains("exceeds 128-byte recovery limit")
        ));
    }

    #[derive(Default)]
    struct RecordingMaterializer {
        fail: AtomicBool,
        batches: StdMutex<Vec<Vec<u64>>>,
    }

    impl RecordingMaterializer {
        fn failing() -> Self {
            Self {
                fail: AtomicBool::new(true),
                batches: StdMutex::new(Vec::new()),
            }
        }

        fn set_fail(&self, fail: bool) {
            self.fail.store(fail, Ordering::SeqCst);
        }

        fn batches(&self) -> Vec<Vec<u64>> {
            self.batches.lock().expect("batches").clone()
        }
    }

    impl LlmTraceBatchMaterializer for RecordingMaterializer {
        fn materialize(
            &self,
            _scope: &LlmScope,
            records: &[LlmTraceJournalEnvelope],
        ) -> Result<(), LlmTraceJournalError> {
            self.batches
                .lock()
                .expect("batches")
                .push(records.iter().map(|record| record.sequence).collect());
            if self.fail.load(Ordering::SeqCst) {
                return Err(LlmTraceJournalError::Materialization(
                    "injected failure".to_string(),
                ));
            }
            Ok(())
        }
    }

    fn fixture() -> (TempDir, ArtifactV2Workspace) {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        (temp, workspace)
    }

    fn scope(name: &str) -> LlmScope {
        LlmScope::new(format!("principal-{name}"), "workspace")
    }

    fn started(scope: &LlmScope, operation: &str, at_ms: i64) -> LlmTraceRecord {
        let context = LlmTraceContext::new(scope.clone(), LlmWorkloadClass::ForegroundChat);
        LlmTraceRecord::CallStarted(LlmCallStarted::new(context, operation, at_ms))
    }

    fn proposed_tool(scope: &LlmScope, model_call_id: &str, at_ms: i64) -> LlmTraceRecord {
        let context = LlmTraceContext::new(scope.clone(), LlmWorkloadClass::ForegroundChat);
        LlmTraceRecord::ToolLineage(LlmToolLineageRecord {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            tool_execution_id: format!("{}:tool:{model_call_id}", context.llm_call_id),
            context,
            model_tool_call_id: model_call_id.to_string(),
            branch_id: format!("branch-{model_call_id}"),
            operation: "chat".to_string(),
            source_surface: "chat".to_string(),
            tool_name: "browser__open".to_string(),
            tool_family: Some("browser".to_string()),
            stage: crate::magician_v2::analytics::llm_trace_recorder::LlmToolLineageStage::Proposed,
            stage_index: 0,
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms,
            arguments_fingerprint: Some("a".repeat(64)),
            result_ref: None,
            canonical_event_ref: None,
            related_execution_ids: Vec::new(),
            consumed_by_call_id: None,
            name_known: None,
            arguments_parsed: None,
            schema_matched: None,
            policy_allowed: None,
            approval_required: None,
            approval_obtained: None,
            transport_ran: None,
            tool_reported_success: None,
            result_validation_success: None,
            outcome:
                crate::magician_v2::analytics::llm_trace_recorder::LlmToolLineageOutcome::Pending,
            failure_owner: None,
            failure_code: None,
            side_effect_state:
                crate::magician_v2::analytics::llm_trace_recorder::LlmToolSideEffectState::None,
            branch_state:
                crate::magician_v2::analytics::llm_trace_recorder::LlmToolBranchState::Active,
            on_successful_path: None,
            same_tool_arguments_count: 1,
            observation_action_cycle_count: 0,
            recovered_after_failure: false,
            linkage_gap: None,
        })
    }

    fn restricted_call_io(scope: &LlmScope, at_ms: i64) -> LlmTraceRecord {
        let context = LlmTraceContext::new(scope.clone(), LlmWorkloadClass::ForegroundChat);
        LlmTraceRecord::CallIo(LlmCallIoRecord {
            schema_version: LLM_RESTRICTED_CONTENT_SCHEMA_VERSION,
            context,
            operation: "chat".to_string(),
            phase: LlmCallIoPhase::LogicalRequest,
            provider_attempt_index: None,
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms,
            capture: LlmCaptureFact {
                mode: LlmCaptureMode::Sanitized,
                status: LlmCaptureStatus::Complete,
                training_eligible_at_capture: false,
                training_exclusion_reason: Some("training_not_enabled".to_string()),
            },
            retention_class: "sanitized_30d".to_string(),
            redaction: LlmRedactionReport {
                policy_version: "llm-content-redaction-v1".to_string(),
                ..LlmRedactionReport::default()
            },
            logical_request_fingerprint: Some("a".repeat(64)),
            effective_request_fingerprint: None,
            response_fingerprint: None,
            original_bytes: 64,
            sanitized_bytes: 32,
            payload: serde_json::json!({"messages": [{"content": "safe"}]}),
        })
    }

    #[test]
    fn journal_namespaces_reject_cross_lane_records() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("namespace");
        let restricted = restricted_call_io(&target_scope, 1_700_000_000_000);
        let mut canonical =
            LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("canonical");
        assert!(matches!(
            canonical.append_batch(&target_scope, std::slice::from_ref(&restricted)),
            Err(LlmTraceJournalError::Sequence(message))
                if message.contains("journal namespace")
        ));
        let mut content = LlmTraceJournalStore::new_with_namespace(
            workspace,
            1024 * 1024,
            LlmTraceJournalNamespace::RestrictedContent,
        )
        .expect("restricted");
        assert!(matches!(
            content.append_batch(
                &target_scope,
                &[started(&target_scope, "chat", 1_700_000_000_000)]
            ),
            Err(LlmTraceJournalError::Sequence(message))
                if message.contains("journal namespace")
        ));
    }

    #[test]
    fn restricted_journal_prunes_committed_payload_but_keeps_watermark() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("restricted-prune");
        let record = restricted_call_io(&target_scope, 1_700_000_000_000);
        let materializer = RecordingMaterializer::default();
        let mut store = LlmTraceJournalStore::new_with_namespace(
            workspace.clone(),
            1024 * 1024,
            LlmTraceJournalNamespace::RestrictedContent,
        )
        .expect("restricted store");
        store
            .append_materialize_commit(&target_scope, &[record], &materializer)
            .expect("append/materialize/commit");
        assert!(store
            .replay_uncommitted(&target_scope)
            .expect("replay")
            .is_empty());
        let segments = workspace
            .analytics_llm_restricted_journal_root(&target_scope.principal, &target_scope.workspace)
            .join("segments");
        assert_eq!(
            workspace
                .read_dir_path_sync(&segments)
                .expect("segments")
                .len(),
            0
        );
        let mut restarted = LlmTraceJournalStore::new_with_namespace(
            workspace,
            1024 * 1024,
            LlmTraceJournalNamespace::RestrictedContent,
        )
        .expect("restart");
        assert_eq!(
            restarted
                .watermark(&target_scope)
                .expect("watermark")
                .committed_sequence,
            1
        );
    }

    #[test]
    fn restricted_uncommitted_restart_replays_the_exact_sanitized_fixture() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("restricted-restart");
        let record = restricted_call_io(&target_scope, 1_700_000_000_000);
        let mut first = LlmTraceJournalStore::new_with_namespace(
            workspace.clone(),
            1024 * 1024,
            LlmTraceJournalNamespace::RestrictedContent,
        )
        .expect("restricted store");
        first
            .append_batch(&target_scope, std::slice::from_ref(&record))
            .expect("append sanitized fixture");
        drop(first);

        let mut restarted = LlmTraceJournalStore::new_with_namespace(
            workspace,
            1024 * 1024,
            LlmTraceJournalNamespace::RestrictedContent,
        )
        .expect("restart");
        let replay = restarted
            .replay_uncommitted(&target_scope)
            .expect("replay exact fixture");
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].record, record);
        assert_eq!(replay[0].sequence, 1);
        assert_eq!(
            replay[0].payload_checksum,
            record_checksum(&record).expect("fixture checksum")
        );
    }

    fn tombstoned(context: LlmTraceContext, operation: &str, at_ms: i64) -> LlmTraceRecord {
        LlmTraceRecord::CallCompleted(LlmCallCompleted {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context,
            dispatch_job_id: Some("job".to_string()),
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms,
            operation: operation.to_string(),
            terminal_state: LlmCallTerminalState::Tombstoned,
            provider_attempt_count: 0,
            provider_response_id: None,
            response_kind: None,
            error_class: Some("cancelled".to_string()),
            error_code: None,
            finish_reason: None,
            refusal: None,
            truncated: None,
            timing: LlmTimingFact {
                created_at_ms: Some(at_ms - 1),
                completed_at_ms: Some(at_ms),
                latency_ms: Some(1),
                ..LlmTimingFact::default()
            },
            usage: LlmTokenUsageFact::default(),
            pricing: LlmPricingFact::default(),
            validation: LlmImmediateValidationFact::default(),
            capture: Default::default(),
        })
    }

    #[test]
    fn journal_restart_replays_uncommitted_records_in_sequence_order() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("restart");
        let records = vec![
            started(&target_scope, "chat", 10),
            started(&target_scope, "chat", 20),
        ];
        let mut first = LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("store");
        let receipt = first
            .append_batch(&target_scope, &records)
            .expect("journal append");
        assert_eq!(receipt.appended, 2);
        drop(first);

        let mut restarted =
            LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("restarted store");
        let replay = restarted
            .replay_uncommitted(&target_scope)
            .expect("restart replay");
        assert_eq!(
            replay
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            restarted
                .watermark(&target_scope)
                .expect("watermark")
                .committed_sequence,
            0
        );
    }

    #[test]
    fn duplicate_delivery_is_idempotent_and_conflicting_revision_fails_closed() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("duplicate");
        let record = started(&target_scope, "chat", 10);
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        store
            .append_batch(&target_scope, std::slice::from_ref(&record))
            .expect("initial append");
        let duplicate = store
            .append_batch(&target_scope, std::slice::from_ref(&record))
            .expect("idempotent duplicate");
        assert_eq!(duplicate.appended, 0);
        assert_eq!(duplicate.duplicates, 1);

        let mut conflicting = record;
        if let LlmTraceRecord::CallStarted(start) = &mut conflicting {
            start.operation = "different-operation".to_string();
        }
        assert!(matches!(
            store.append_batch(&target_scope, &[conflicting]),
            Err(LlmTraceJournalError::IdempotencyConflict { .. })
        ));
        assert_eq!(
            store
                .replay_uncommitted(&target_scope)
                .expect("replay")
                .len(),
            1
        );
    }

    #[test]
    fn lifecycle_context_drift_is_rejected_before_append() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("context-drift");
        let context = LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::ForegroundChat);
        let start = LlmTraceRecord::CallStarted(LlmCallStarted::new(context.clone(), "chat", 10));
        let mut drifted = context;
        drifted.trace_id = "different-trace".to_string();
        let completion = tombstoned(drifted, "chat", 20);
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        assert!(matches!(
            store.append_batch(&target_scope, &[start, completion]),
            Err(LlmTraceJournalError::Sequence(message)) if message.contains("trace context")
        ));
        assert!(store
            .replay_uncommitted(&target_scope)
            .expect("no corrupt prefix")
            .is_empty());
    }

    #[test]
    fn missing_or_late_lifecycle_revisions_do_not_poison_scope_flushes() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("partial-lifecycle");
        let context = LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::ForegroundChat);
        let completion = tombstoned(context.clone(), "chat", 20);
        let unrelated = started(&target_scope, "memory", 30);
        let late_start = LlmTraceRecord::CallStarted(LlmCallStarted::new(context, "chat", 19));
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");

        store
            .append_batch(&target_scope, &[completion])
            .expect("a completion remains durable when its start revision was lost");
        store
            .append_batch(&target_scope, &[unrelated])
            .expect("an incomplete lifecycle must not poison later scope writes");
        store
            .append_batch(&target_scope, &[late_start])
            .expect("a delayed immutable start revision can repair the lifecycle");

        let replay = store
            .replay_uncommitted(&target_scope)
            .expect("replay partial lifecycle");
        assert_eq!(replay.len(), 3);
        assert_eq!(replay[0].key.revision, 2);
        assert_eq!(replay[2].key.revision, 1);
    }

    #[test]
    fn lifecycle_parent_cycle_is_rejected_before_append() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("parent-cycle");
        let mut first =
            LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::AutonomousTask);
        let mut second =
            LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::AutonomousTask);
        first.parent_call_id = Some(second.llm_call_id.clone());
        first.parent_relation = Some(magicllm::LlmParentRelation::Supports);
        second.parent_call_id = Some(first.llm_call_id.clone());
        second.parent_relation = Some(magicllm::LlmParentRelation::Supports);
        let records = vec![
            LlmTraceRecord::CallStarted(LlmCallStarted::new(first, "agentic", 10)),
            LlmTraceRecord::CallStarted(LlmCallStarted::new(second, "agentic", 11)),
        ];
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        assert!(matches!(
            store.append_batch(&target_scope, &records),
            Err(LlmTraceJournalError::Sequence(message)) if message.contains("parent graph")
        ));
    }

    #[test]
    fn late_parent_with_a_different_trace_is_rejected_before_append() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("late-parent-trace-drift");
        let parent = LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::AutonomousTask);
        let child = parent.child(
            magicllm::LlmParentRelation::Supports,
            magicllm::LlmCallRole::Supporting,
        );
        let child_record = LlmTraceRecord::CallStarted(LlmCallStarted::new(child, "agentic", 10));
        let mut drifted_parent = parent;
        drifted_parent.trace_id = "different-trace".to_string();
        let parent_record =
            LlmTraceRecord::CallStarted(LlmCallStarted::new(drifted_parent, "agentic", 9));
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");

        store
            .append_batch(&target_scope, &[child_record])
            .expect("an unresolved parent edge may arrive later");
        assert!(matches!(
            store.append_batch(&target_scope, &[parent_record]),
            Err(LlmTraceJournalError::Sequence(message))
                if message.contains("known parent must share")
        ));
        assert_eq!(
            store
                .replay_uncommitted(&target_scope)
                .expect("rejected parent was not appended")
                .len(),
            1
        );
    }

    #[test]
    fn late_parent_with_the_same_trace_repairs_the_unresolved_edge() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("late-parent-valid");
        let parent = LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::AutonomousTask);
        let child = parent.child(
            magicllm::LlmParentRelation::Supports,
            magicllm::LlmCallRole::Supporting,
        );
        let child_record = LlmTraceRecord::CallStarted(LlmCallStarted::new(child, "agentic", 10));
        let parent_record = LlmTraceRecord::CallStarted(LlmCallStarted::new(parent, "agentic", 9));
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");

        store
            .append_batch(&target_scope, &[child_record])
            .expect("unresolved child edge");
        store
            .append_batch(&target_scope, &[parent_record])
            .expect("matching late parent");

        assert_eq!(
            store
                .replay_uncommitted(&target_scope)
                .expect("both revisions remain replayable")
                .len(),
            2
        );
    }

    #[test]
    fn lifecycle_first_token_timestamp_drift_is_rejected_before_append() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("first-token-drift");
        let context = LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::ForegroundChat);
        let started =
            LlmProviderAttemptEvent::started(context, "chat", 1, "openai", "gpt-test", 10);
        let mut first_token = started.clone();
        first_token.phase = LlmProviderAttemptPhase::FirstToken;
        first_token.occurred_at_ms = 15;
        first_token.observed_at_ms = 15;
        first_token.timing.first_token_at_ms = Some(15);
        first_token.timing.ttft_ms = Some(5);
        let mut completed = first_token.clone();
        completed.phase = LlmProviderAttemptPhase::Completed;
        completed.occurred_at_ms = 20;
        completed.observed_at_ms = 20;
        completed.timing.first_token_at_ms = Some(16);
        completed.timing.ttft_ms = Some(6);
        completed.timing.generation_after_ttft_ms = Some(4);
        completed.timing.completed_at_ms = Some(20);
        completed.timing.latency_ms = Some(10);
        completed.terminal_state = Some(LlmAttemptTerminalState::Succeeded);

        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        let error = store
            .append_batch(
                &target_scope,
                &[
                    LlmTraceRecord::ProviderAttempt(started),
                    LlmTraceRecord::ProviderAttempt(first_token),
                    LlmTraceRecord::ProviderAttempt(completed),
                ],
            )
            .expect_err("first-token drift must fail before append");
        assert!(matches!(
            error,
            LlmTraceJournalError::Sequence(message) if message.contains("first-token timestamp")
        ));
    }

    #[test]
    fn lifecycle_effective_route_drift_is_rejected_before_append() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("attempt-route-drift");
        let context = LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::ForegroundChat);
        let mut started =
            LlmProviderAttemptEvent::started(context, "chat", 1, "openai", "gpt-test", 10);
        started.effective_profile = Some("profile-a".to_string());
        started.model_revision = Some("revision-a".to_string());
        let mut completed = started.clone();
        completed.phase = LlmProviderAttemptPhase::Completed;
        completed.occurred_at_ms = 20;
        completed.observed_at_ms = 20;
        completed.timing.completed_at_ms = Some(20);
        completed.timing.latency_ms = Some(10);
        completed.terminal_state = Some(LlmAttemptTerminalState::Succeeded);
        completed.effective_profile = Some("profile-b".to_string());

        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        let error = store
            .append_batch(
                &target_scope,
                &[
                    LlmTraceRecord::ProviderAttempt(started),
                    LlmTraceRecord::ProviderAttempt(completed),
                ],
            )
            .expect_err("an immutable physical attempt cannot change route");
        assert!(matches!(
            error,
            LlmTraceJournalError::Sequence(message)
                if message.contains("effective profile")
        ));
        assert!(store
            .replay_uncommitted(&target_scope)
            .expect("rejected batch leaves no durable prefix")
            .is_empty());
    }

    #[test]
    fn final_attempt_transport_outcome_must_match_the_logical_call() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("terminal-outcome-drift");
        let context = LlmTraceContext::new(target_scope.clone(), LlmWorkloadClass::ForegroundChat);
        let mut attempt =
            LlmProviderAttemptEvent::started(context.clone(), "chat", 1, "openai", "gpt-test", 11);
        attempt.phase = LlmProviderAttemptPhase::Completed;
        attempt.occurred_at_ms = 19;
        attempt.observed_at_ms = 19;
        attempt.timing.completed_at_ms = Some(19);
        attempt.timing.latency_ms = Some(8);
        attempt.terminal_state = Some(LlmAttemptTerminalState::Failed);
        attempt.error_class = Some("provider_error".to_string());
        let completed = LlmCallCompleted {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context,
            dispatch_job_id: None,
            occurred_at_ms: 20,
            observed_at_ms: 20,
            operation: "chat".to_string(),
            terminal_state: LlmCallTerminalState::Succeeded,
            provider_attempt_count: 1,
            provider_response_id: None,
            response_kind: Some("text".to_string()),
            error_class: None,
            error_code: None,
            finish_reason: None,
            refusal: None,
            truncated: None,
            timing: LlmTimingFact {
                created_at_ms: Some(10),
                completed_at_ms: Some(20),
                latency_ms: Some(10),
                ..LlmTimingFact::default()
            },
            usage: LlmTokenUsageFact::default(),
            pricing: LlmPricingFact::default(),
            validation: LlmImmediateValidationFact {
                response_present: true,
                ..LlmImmediateValidationFact::default()
            },
            capture: Default::default(),
        };

        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        let error = store
            .append_batch(
                &target_scope,
                &[
                    LlmTraceRecord::ProviderAttempt(attempt),
                    LlmTraceRecord::CallCompleted(completed),
                ],
            )
            .expect_err("successful call cannot own a failed terminal attempt");
        assert!(matches!(
            error,
            LlmTraceJournalError::Sequence(message)
                if message.contains("transport outcome")
        ));
        assert!(store
            .replay_uncommitted(&target_scope)
            .expect("rejected batch leaves no durable prefix")
            .is_empty());
    }

    #[test]
    fn materialization_failure_does_not_advance_watermark_and_retry_commits() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("materializer");
        let record = started(&target_scope, "chat", 10);
        let materializer = RecordingMaterializer::failing();
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("store");
        assert!(matches!(
            store.append_materialize_commit(
                &target_scope,
                std::slice::from_ref(&record),
                &materializer
            ),
            Err(LlmTraceJournalError::Materialization(_))
        ));
        assert_eq!(
            store
                .watermark(&target_scope)
                .expect("watermark")
                .committed_sequence,
            0
        );
        drop(store);

        materializer.set_fail(false);
        let mut restarted =
            LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("restart store");
        let receipt = restarted
            .append_materialize_commit(&target_scope, &[], &materializer)
            .expect("replay materialization");
        assert_eq!(receipt.appended, 0);
        assert_eq!(
            restarted
                .watermark(&target_scope)
                .expect("watermark")
                .committed_sequence,
            1
        );
        assert_eq!(materializer.batches(), vec![vec![1], vec![1]]);
    }

    #[test]
    fn rotation_preserves_order_and_repairs_only_a_partial_final_line() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("rotation");
        let records = (1..=3)
            .map(|index| started(&target_scope, "chat", index * 10))
            .collect::<Vec<_>>();
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 1).expect("store");
        store
            .append_batch(&target_scope, &records)
            .expect("rotated append");
        let segments_root = workspace
            .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace)
            .join("segments");
        let mut segments = workspace
            .read_dir_path_sync(&segments_root)
            .expect("segments");
        segments.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        assert_eq!(segments.len(), 3);
        let last = segments_root.join(&segments.last().expect("last segment").file_name);
        workspace
            .append_path_sync(&last, br#"{"partial":"#)
            .expect("inject interrupted tail");
        drop(store);

        let mut restarted = LlmTraceJournalStore::new(workspace.clone(), 1).expect("restart");
        let replay = restarted
            .replay_uncommitted(&target_scope)
            .expect("repair and replay");
        assert_eq!(
            replay
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(workspace
            .read_path_sync(last)
            .expect("repaired segment")
            .ends_with(b"\n"));
    }

    #[test]
    fn recovery_rejects_malformed_sequence_bearing_segment_names() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("bad-segment-name");
        let segments_root = workspace
            .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace)
            .join("segments");
        workspace
            .create_dir_all_path_sync(&segments_root)
            .expect("segments root");
        workspace
            .write_atomic_path_sync(segments_root.join("segment-invalid.jsonl"), b"")
            .expect("invalid segment fixture");

        let mut restarted =
            LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("restart store");
        let error = restarted
            .replay_uncommitted(&target_scope)
            .expect_err("invalid segment name must fail recovery closed");
        assert!(error
            .to_string()
            .contains("invalid sequence-bearing filename"));
    }

    #[test]
    fn writer_lease_rejects_a_second_pipeline_owner() {
        let (_temp, workspace) = fixture();
        let first = acquire_writer_lock(&workspace, LlmTraceJournalNamespace::CanonicalFacts)
            .expect("first writer lease");
        let error = acquire_writer_lock(&workspace, LlmTraceJournalNamespace::CanonicalFacts)
            .expect_err("a concurrent writer lease must be rejected");
        assert!(error.to_string().contains("already active"));
        drop(first);
        acquire_writer_lock(&workspace, LlmTraceJournalNamespace::CanonicalFacts)
            .expect("lease is released with its descriptor");
    }

    #[test]
    fn canonical_and_restricted_writer_leases_are_independent() {
        let (_temp, workspace) = fixture();
        let canonical = acquire_writer_lock(&workspace, LlmTraceJournalNamespace::CanonicalFacts)
            .expect("canonical writer lease");
        let restricted =
            acquire_writer_lock(&workspace, LlmTraceJournalNamespace::RestrictedContent)
                .expect("restricted writer lease");
        assert!(workspace.base_root().join(WRITER_LOCK_FILE_NAME).is_file());
        assert!(workspace
            .base_root()
            .join(RESTRICTED_WRITER_LOCK_FILE_NAME)
            .is_file());
        drop((canonical, restricted));
    }

    #[cfg(unix)]
    #[test]
    fn writer_lease_symlink_is_rejected_instead_of_followed() {
        use std::os::unix::fs::symlink;

        let (_temp, workspace) = fixture();
        let external = tempfile::NamedTempFile::new().expect("external lock target");
        symlink(
            external.path(),
            workspace.base_root().join(WRITER_LOCK_FILE_NAME),
        )
        .expect("writer lock symlink");

        let error = acquire_writer_lock(&workspace, LlmTraceJournalNamespace::CanonicalFacts)
            .expect_err("writer lock symlink must fail closed");
        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn watermark_is_checksum_bound_monotonic_and_scope_isolated() {
        let (_temp, workspace) = fixture();
        let alpha = scope("alpha");
        let beta = scope("beta");
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        store
            .append_batch(&alpha, &[started(&alpha, "chat", 10)])
            .expect("alpha append");
        store
            .append_batch(&beta, &[started(&beta, "memory", 10)])
            .expect("beta append");
        let committed = store.commit_through(&alpha, 1).expect("commit alpha");
        assert!(committed.committed_checksum.is_some());
        assert_eq!(
            store
                .watermark(&beta)
                .expect("beta watermark")
                .committed_sequence,
            0
        );
        assert!(store.commit_through(&alpha, 2).is_err());
        assert_eq!(
            store
                .commit_through(&alpha, 1)
                .expect("idempotent commit")
                .committed_sequence,
            1
        );
    }

    #[test]
    fn malformed_persisted_watermark_fails_recovery_closed() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("malformed-watermark");
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("store");
        store
            .append_batch(&target_scope, &[started(&target_scope, "chat", 10)])
            .expect("append");
        store.commit_through(&target_scope, 1).expect("commit");
        drop(store);
        let watermark_path = workspace
            .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace)
            .join("materialization-watermark.json");
        workspace
            .write_json_atomic_path_sync(
                watermark_path,
                &serde_json::json!({
                    "schema_version": JOURNAL_WATERMARK_SCHEMA_VERSION,
                    "committed_sequence": 1,
                    "committed_checksum": "not-a-checksum",
                    "updated_at_ms": 1
                }),
            )
            .expect("corrupt watermark");

        let mut restarted =
            LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("restart store");
        let error = restarted
            .replay_uncommitted(&target_scope)
            .expect_err("recovery must reject malformed watermark");
        assert!(error
            .to_string()
            .contains("64-character lowercase hexadecimal"));
    }

    #[test]
    fn record_from_another_scope_is_rejected_before_append() {
        let (_temp, workspace) = fixture();
        let alpha = scope("alpha-mismatch");
        let beta = scope("beta-mismatch");
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        assert!(matches!(
            store.append_batch(&alpha, &[started(&beta, "chat", 10)]),
            Err(LlmTraceJournalError::Sequence(_))
        ));
        assert!(store
            .replay_uncommitted(&alpha)
            .expect("empty alpha")
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn journal_scope_symlink_cannot_redirect_durable_writes() {
        use std::os::unix::fs::symlink;

        let (_temp, workspace) = fixture();
        let external = tempfile::tempdir().expect("external tempdir");
        let target_scope = scope("redirected");
        let principal_root = workspace
            .base_root()
            .join("scopes")
            .join(&target_scope.principal);
        std::fs::create_dir_all(&principal_root).expect("principal root");
        symlink(
            external.path(),
            principal_root.join(&target_scope.workspace),
        )
        .expect("workspace symlink");
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("store");
        let error = store
            .append_batch(&target_scope, &[started(&target_scope, "chat", 10)])
            .expect_err("journal must reject a redirected tenant path");
        assert!(error.to_string().contains("real directory"));
        assert!(std::fs::read_dir(external.path())
            .expect("external directory")
            .next()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn journal_watermark_symlink_is_rejected_instead_of_followed() {
        use std::os::unix::fs::symlink;

        let (_temp, workspace) = fixture();
        let external = tempfile::NamedTempFile::new().expect("external watermark");
        let target_scope = scope("watermark-link");
        let root = workspace
            .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace);
        std::fs::create_dir_all(root.join("segments")).expect("journal root");
        symlink(external.path(), root.join("materialization-watermark.json"))
            .expect("watermark symlink");
        let mut restarted =
            LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("restart store");
        let error = restarted
            .replay_uncommitted(&target_scope)
            .expect_err("journal recovery must reject a watermark symlink");
        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn saturated_critical_buffer_accumulates_exact_gap_and_emits_on_recovery() {
        let (critical_tx, critical_rx) = mpsc::sync_channel(1);
        let (lineage_tx, _lineage_rx) = mpsc::sync_channel(1);
        let (restricted_payload_tx, _payload_rx) = mpsc::sync_channel(1);
        let shared = Arc::new(LlmTraceBufferShared::default());
        let sink = BufferedLlmTraceSink {
            critical_tx,
            lineage_tx,
            restricted_payload_tx,
            shared: shared.clone(),
        };
        let target_scope = scope("gap");
        sink.try_record(started(&target_scope, "chat", 10))
            .expect("first record fills channel");
        assert!(sink.try_record(started(&target_scope, "chat", 20)).is_err());
        assert!(sink.try_record(started(&target_scope, "chat", 30)).is_err());
        assert_eq!(sink.stats().pending_missing_records, 2);

        critical_rx.recv().expect("free one slot");
        decrement_depth(&shared.metrics.critical_depth);
        assert!(sink.try_record(started(&target_scope, "chat", 40)).is_err());
        let emitted = critical_rx.recv().expect("recovery gap");
        match emitted {
            LlmTraceRecord::CaptureGap(gap) => {
                assert_eq!(gap.missing_record_count, 2);
                assert_eq!(gap.first_observed_at_ms, 20);
                assert_eq!(gap.last_observed_at_ms, 30);
            },
            other => panic!("expected capture gap, got {other:?}"),
        }
        // The current record could not use the one slot consumed by the gap,
        // so it begins a new exact gap window instead of disappearing.
        assert_eq!(sink.stats().pending_missing_records, 1);
    }

    #[test]
    fn saturated_lineage_buffer_emits_call_owned_capture_gap() {
        let (critical_tx, critical_rx) = mpsc::sync_channel(1);
        let (lineage_tx, lineage_rx) = mpsc::sync_channel(1);
        let (restricted_payload_tx, _payload_rx) = mpsc::sync_channel(1);
        let shared = Arc::new(LlmTraceBufferShared::default());
        let sink = BufferedLlmTraceSink {
            critical_tx,
            lineage_tx,
            restricted_payload_tx,
            shared: shared.clone(),
        };
        let target_scope = scope("lineage-gap");
        sink.try_record(proposed_tool(&target_scope, "call_1", 10))
            .expect("first lineage fills channel");
        let dropped = proposed_tool(&target_scope, "call_2", 20);
        let dropped_call_id = match &dropped {
            LlmTraceRecord::ToolLineage(record) => record.context.llm_call_id.clone(),
            _ => unreachable!(),
        };
        assert!(sink.try_record(dropped).is_err());
        assert_eq!(sink.stats().pending_missing_records, 1);

        lineage_rx.recv().expect("free lineage slot");
        decrement_depth(&shared.metrics.lineage_depth);
        // Every admission first publishes any pending gap to the critical
        // lane, independently of which data lane recovered.
        sink.try_record(proposed_tool(&target_scope, "call_3", 30))
            .expect("lineage recovery record");
        let emitted = critical_rx.recv().expect("lineage capture gap");
        match emitted {
            LlmTraceRecord::CaptureGap(gap) => {
                assert_eq!(gap.reason, TOOL_LINEAGE_BUFFER_SATURATED);
                assert_eq!(gap.llm_call_id.as_deref(), Some(dropped_call_id.as_str()));
                assert_eq!(gap.missing_record_count, 1);
            },
            other => panic!("expected lineage capture gap, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bounded_shutdown_drains_and_materializes_queued_records() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("shutdown");
        let materializer = Arc::new(RecordingMaterializer::default());
        let config = LlmTraceJournalConfig {
            namespace: LlmTraceJournalNamespace::CanonicalFacts,
            critical_capacity: 8,
            lineage_capacity: 2,
            restricted_payload_capacity: 2,
            batch_row_threshold: 32,
            flush_interval: Duration::from_secs(60),
            max_segment_bytes: 1024 * 1024,
            shutdown_timeout: Duration::from_secs(2),
        };
        let pipeline =
            LlmTraceDurablePipeline::start(workspace.clone(), config, materializer.clone(), vec![])
                .expect("pipeline");
        pipeline
            .recorder()
            .begin_call(match started(&target_scope, "chat", 10) {
                LlmTraceRecord::CallStarted(start) => start,
                _ => unreachable!(),
            })
            .expect("enqueue");
        let report = pipeline.shutdown().await;
        assert!(!report.timed_out);
        assert!(report.flush_error.is_none());
        assert_eq!(report.remaining_buffered_records, 0);

        let mut restarted =
            LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("restart store");
        assert_eq!(
            restarted
                .watermark(&target_scope)
                .expect("watermark")
                .committed_sequence,
            1
        );
        assert_eq!(materializer.batches(), vec![vec![1]]);
    }

    #[tokio::test]
    async fn dropped_owner_closes_admission_and_defensively_flushes_accepted_records() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("owner-drop");
        let materializer = Arc::new(RecordingMaterializer::default());
        let config = LlmTraceJournalConfig {
            namespace: LlmTraceJournalNamespace::CanonicalFacts,
            critical_capacity: 8,
            lineage_capacity: 2,
            restricted_payload_capacity: 2,
            batch_row_threshold: 32,
            flush_interval: Duration::from_secs(60),
            max_segment_bytes: 1024 * 1024,
            shutdown_timeout: Duration::from_secs(2),
        };
        let pipeline =
            LlmTraceDurablePipeline::start(workspace, config, materializer.clone(), vec![])
                .expect("pipeline");
        let recorder = pipeline.recorder();
        recorder
            .begin_call(match started(&target_scope, "chat", 10) {
                LlmTraceRecord::CallStarted(start) => start,
                _ => unreachable!(),
            })
            .expect("accepted before owner drop");

        drop(pipeline);

        assert!(recorder
            .begin_call(match started(&target_scope, "chat", 20) {
                LlmTraceRecord::CallStarted(start) => start,
                _ => unreachable!(),
            })
            .is_err());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if materializer.batches() == vec![vec![1]] {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("defensive flush after owner drop");
    }

    #[tokio::test]
    async fn recovery_scope_is_materialized_and_committed_by_flush_barrier() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("startup-recovery");
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("store");
        store
            .append_batch(&target_scope, &[started(&target_scope, "chat", 10)])
            .expect("uncommitted append");
        drop(store);

        let materializer = Arc::new(RecordingMaterializer::default());
        let pipeline = LlmTraceDurablePipeline::start(
            workspace.clone(),
            LlmTraceJournalConfig::default(),
            materializer.clone(),
            vec![target_scope.clone()],
        )
        .expect("pipeline");
        pipeline.flush().await.expect("catch-up barrier");
        let mut recovered =
            LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("recovered store");
        assert_eq!(
            recovered
                .watermark(&target_scope)
                .expect("startup watermark")
                .committed_sequence,
            1
        );
        assert_eq!(materializer.batches(), vec![vec![1]]);
        let report = pipeline.shutdown().await;
        assert!(report.flush_error.is_none());

        let mut restarted =
            LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("restart store");
        assert_eq!(
            restarted
                .watermark(&target_scope)
                .expect("watermark")
                .committed_sequence,
            1
        );
        assert_eq!(materializer.batches(), vec![vec![1]]);
    }

    #[tokio::test]
    async fn materialization_failure_keeps_backlog_on_disk_without_blocking_writer_readiness() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("startup-recovery-failure");
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("store");
        store
            .append_batch(&target_scope, &[started(&target_scope, "chat", 10)])
            .expect("append");
        drop(store);
        let materializer = Arc::new(RecordingMaterializer::failing());
        let pipeline = LlmTraceDurablePipeline::start(
            workspace.clone(),
            LlmTraceJournalConfig::default(),
            materializer.clone(),
            vec![target_scope.clone()],
        )
        .expect("writer ready despite analytics backlog");
        assert!(pipeline
            .flush()
            .await
            .unwrap_err()
            .to_string()
            .contains("injected failure"));
        // New recording remains available and failed publication retains no
        // second in-memory copy of already durable accepted records.
        pipeline
            .sink()
            .try_record(started(&target_scope, "chat", 20))
            .expect("accept new record");
        assert!(pipeline.flush().await.is_err());
        assert_eq!(pipeline.sink().stats().worker_buffered_depth, 0);
        materializer.set_fail(false);
        pipeline.flush().await.expect("retry catch-up");
        let report = pipeline.shutdown().await;
        assert!(report.flush_error.is_none());
        let mut restarted = LlmTraceJournalStore::new(workspace, 1024 * 1024).expect("restart");
        assert_eq!(
            restarted
                .watermark(&target_scope)
                .unwrap()
                .committed_sequence,
            2
        );
    }

    #[test]
    fn legacy_history_migrates_in_batches_and_warm_restart_decodes_no_history() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("index-migration");
        let root = workspace
            .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace);
        let mut bytes = Vec::new();
        for sequence in 1..=777 {
            let envelope = LlmTraceJournalEnvelope::new(
                sequence,
                started(&target_scope, "chat", sequence as i64),
            )
            .unwrap();
            bytes.extend(serde_json::to_vec(&envelope).unwrap());
            bytes.push(b'\n');
        }
        workspace
            .write_atomic_path_sync(root.join("segments").join(segment_name(1)), &bytes)
            .unwrap();
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 32 * 1024 * 1024).unwrap();
        let mut total = 0;
        loop {
            let batch = store.replay_uncommitted(&target_scope).unwrap();
            if batch.is_empty() {
                break;
            }
            assert!(batch.len() <= REPLAY_BATCH_ROWS);
            total += batch.len();
            store
                .commit_through(&target_scope, batch.last().unwrap().sequence)
                .unwrap();
        }
        assert_eq!(total, 777);
        let state = store.ensure_scope(&target_scope).unwrap();
        assert_eq!(state.index.as_ref().unwrap().recovered_records, 777);
        assert!(
            state.envelopes.is_empty() && state.keys.is_empty() && state.lifecycle.calls.is_empty()
        );
        drop(store);
        let mut restarted = LlmTraceJournalStore::new(workspace, 32 * 1024 * 1024).unwrap();
        assert!(restarted
            .replay_uncommitted(&target_scope)
            .unwrap()
            .is_empty());
        assert_eq!(
            restarted
                .ensure_scope(&target_scope)
                .unwrap()
                .index
                .as_ref()
                .unwrap()
                .recovered_records,
            0
        );
    }

    #[test]
    fn index_transaction_loss_recovers_only_suffix_across_segment_rotation() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("index-crash");
        let records = (1..=5)
            .map(|n| started(&target_scope, "chat", n))
            .collect::<Vec<_>>();
        let size = serde_json::to_vec(&LlmTraceJournalEnvelope::new(1, records[0].clone()).unwrap())
            .unwrap()
            .len() as u64;
        let limit = size * 2 + 64;
        let root = workspace
            .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace);
        let mut store = LlmTraceJournalStore::new(workspace.clone(), limit).unwrap();
        store.append_batch(&target_scope, &records[..1]).unwrap();
        drop(store);
        let checkpoint = std::fs::read(root.join("recovery-index.sqlite3")).unwrap();
        let mut store = LlmTraceJournalStore::new(workspace.clone(), limit).unwrap();
        store.append_batch(&target_scope, &records[1..]).unwrap();
        drop(store);
        // Simulate fsynced journal appends followed by a lost index transaction.
        std::fs::write(root.join("recovery-index.sqlite3"), checkpoint).unwrap();
        let mut recovered = LlmTraceJournalStore::new(workspace, limit).unwrap();
        assert_eq!(
            recovered.replay_uncommitted(&target_scope).unwrap().len(),
            5
        );
        assert_eq!(
            recovered
                .ensure_scope(&target_scope)
                .unwrap()
                .index
                .as_ref()
                .unwrap()
                .recovered_records,
            4
        );
        let duplicate = recovered.append_batch(&target_scope, &records).unwrap();
        assert_eq!(duplicate.appended, 0);
        assert_eq!(duplicate.duplicates, 5);
    }

    #[test]
    fn indexed_final_partial_write_is_trimmed_and_checkpoint_remains_restartable() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("index-partial");
        let root = workspace
            .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace);
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).unwrap();
        store
            .append_batch(&target_scope, &[started(&target_scope, "chat", 1)])
            .unwrap();
        drop(store);
        let path = std::fs::read_dir(root.join("segments"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let original = std::fs::metadata(&path).unwrap().len();
        workspace.append_path_sync(&path, b"{partial").unwrap();
        for _ in 0..2 {
            let mut recovered = LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).unwrap();
            assert_eq!(
                recovered.replay_uncommitted(&target_scope).unwrap().len(),
                1
            );
            assert_eq!(std::fs::metadata(&path).unwrap().len(), original);
        }
    }

    #[test]
    fn changed_indexed_history_fails_closed() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("index-corruption");
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 1).unwrap();
        store
            .append_batch(
                &target_scope,
                &[
                    started(&target_scope, "chat", 1),
                    started(&target_scope, "chat", 2),
                ],
            )
            .unwrap();
        drop(store);
        let root = workspace
            .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace);
        let mut paths = std::fs::read_dir(root.join("segments"))
            .unwrap()
            .map(|v| v.unwrap().path())
            .collect::<Vec<_>>();
        paths.sort();
        let mut bytes = std::fs::read(&paths[0]).unwrap();
        bytes[0] = b'!';
        std::fs::write(&paths[0], bytes).unwrap();
        let mut recovered = LlmTraceJournalStore::new(workspace, 1).unwrap();
        assert!(recovered
            .replay_uncommitted(&target_scope)
            .unwrap_err()
            .to_string()
            .contains("segment changed"));
    }

    #[cfg(unix)]
    #[test]
    fn journal_index_and_sqlite_sidecar_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let (_temp, workspace) = fixture();
            let target_scope = scope("index-symlink");
            let root = workspace
                .analytics_llm_trace_journal_root(&target_scope.principal, &target_scope.workspace);
            std::fs::create_dir_all(&root).unwrap();
            let external = tempfile::NamedTempFile::new().unwrap();
            symlink(
                external.path(),
                root.join(format!("recovery-index.sqlite3{suffix}")),
            )
            .unwrap();
            let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).unwrap();
            assert!(store
                .watermark(&target_scope)
                .unwrap_err()
                .to_string()
                .contains("regular file"));
        }
    }

    #[test]
    fn scope_eviction_preserves_persistent_deduplication() {
        let (_temp, workspace) = fixture();
        let mut store = LlmTraceJournalStore::new(workspace, 1024 * 1024).unwrap();
        let original_scope = scope("first-index");
        let original = started(&original_scope, "chat", 1);
        store
            .append_batch(&original_scope, std::slice::from_ref(&original))
            .unwrap();
        for n in 0..MAX_OPEN_JOURNAL_SCOPES * 2 {
            let next = scope(&format!("index-{n}"));
            store
                .append_batch(&next, &[started(&next, "chat", 1)])
                .unwrap();
            assert!(store.scopes.len() <= MAX_OPEN_JOURNAL_SCOPES);
        }
        let receipt = store.append_batch(&original_scope, &[original]).unwrap();
        assert_eq!(receipt.duplicates, 1);
        assert_eq!(receipt.appended, 0);
    }

    #[test]
    fn indexed_sequence_exposes_unpublished_records_without_loading_history() {
        let (_temp, workspace) = fixture();
        let target_scope = scope("index-freshness");
        assert_eq!(
            read_indexed_journal_sequence(&workspace, &target_scope).unwrap(),
            None
        );
        let mut store = LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).unwrap();
        store
            .append_batch(
                &target_scope,
                &[
                    started(&target_scope, "chat", 1),
                    started(&target_scope, "chat", 2),
                ],
            )
            .unwrap();
        store.commit_through(&target_scope, 1).unwrap();
        assert_eq!(
            read_indexed_journal_sequence(&workspace, &target_scope).unwrap(),
            Some(2)
        );
        assert_eq!(
            read_verified_journal_watermark(&workspace, &target_scope)
                .unwrap()
                .unwrap()
                .committed_sequence,
            1
        );
    }
}
