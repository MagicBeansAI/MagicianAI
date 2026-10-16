//! Crash-stable, bounded deliverables for fixed-roster pipeline terminals.
//!
//! A stage's inner stateless loop commits before the outer pipeline progress
//! CAS. The in-hand [`AgenticOutcome`] does not survive a crash in that gap, so
//! a terminal receipt carries a small descriptor for this protected sidecar.
//! The sidecar is written before the inner CAS, retained across uncertain CAS
//! results, consumed while adopting the terminal into the outer cursor, and
//! exact-deleted only after that cursor advances (or the inner CAS is proven
//! not to have committed).

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::magician_v2::{
    agents::AgentStorage,
    execution::{AgenticOutcome, ChildMediaRef},
};

use super::{
    execution_artifacts::FilesystemExecutionArtifactIndexStore,
    service::{ArtifactV2Error, ScopeRef, V3ReadApi},
    workspace::ArtifactV2Workspace,
};

const SCHEMA_VERSION: u8 = 1;
const SEAL_DOMAIN: &str = "magician.pipeline-terminal-settlement.v1";
const PROTECTED_DIRECTORY: &str = "pipeline_terminal_settlements";
const MAX_SIDECAR_BYTES: usize = 512 * 1024;
const MAX_JSON_DEPTH: usize = 16;
const MAX_JSON_NODES: usize = 8 * 1024;
const MAX_ID_BYTES: usize = 512;
const MAX_TERMINAL_KIND_BYTES: usize = 64;
const MAX_PRIMARY_TEXT_BYTES: usize = 64 * 1024;
const MAX_MEDIA_REFS: usize = 32;
const MAX_MEDIA_PATH_BYTES: usize = 2 * 1024;
const MAX_MEDIA_TYPE_BYTES: usize = 256;
const MAX_MEDIA_URL_BYTES: usize = 8 * 1024;
const MAX_MEDIA_CAPTION_BYTES: usize = 8 * 1024;
// Each sidecar has one adjacent advisory-lock file. Bound directory discovery
// by entries rather than matches so a scope containing only stale locks or
// unexpected files cannot turn cancellation/startup reconciliation into an
// unbounded walk.
const MAX_RECONCILIATION_DIRECTORY_ENTRIES: usize = 8 * 1024;
const PRIMARY_TEXT_TRUNCATION_MARKER: &str =
    "\n\n[pipeline terminal deliverable truncated at 64 KiB]\n";

/// Full immutable axis of one inner-terminal/outer-progress transaction.
///
/// Only a digest of this value appears in the filename. The complete axis is
/// repeated inside the HMAC-bound record, so copying a valid file to another
/// scope, task, execution, stage, attempt, sequence, or terminal cannot grant
/// authority there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PipelineTerminalSettlementBinding {
    pub(crate) principal: String,
    pub(crate) workspace: String,
    pub(crate) task_id: String,
    pub(crate) root_execution_id: String,
    pub(crate) exact_segment_id: String,
    pub(crate) terminal_kind: String,
    pub(crate) stage_index: u32,
    pub(crate) attempt: u32,
    pub(crate) terminal_seq: u64,
}

impl PipelineTerminalSettlementBinding {
    pub(crate) fn from_terminal_descriptor(
        descriptor: &crate::magician_v2::execution::agentic::run_loop::state::TerminalSettlementDescriptor,
        sidecar: &PipelineTerminalSettlementRef,
    ) -> Result<Self, ArtifactV2Error> {
        let binding = Self {
            principal: descriptor.principal.clone(),
            workspace: descriptor.workspace.clone(),
            task_id: descriptor.task_id.clone(),
            root_execution_id: descriptor.base_execution_id.clone(),
            exact_segment_id: descriptor.exact_segment_id.clone(),
            terminal_kind: descriptor.terminal_kind.clone(),
            stage_index: sidecar.stage_index,
            attempt: sidecar.attempt,
            terminal_seq: descriptor.terminal_seq,
        };
        validate_binding(&binding)?;
        validate_descriptor(&binding, sidecar)?;
        Ok(binding)
    }
}

/// Small reference embedded in the already scope-HMAC-sealed LoopState
/// terminal receipt. The content digest and encoded length make a descriptor
/// retry deterministic without retaining the deliverable in LoopState.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineTerminalSettlementRef {
    pub(crate) schema_version: u8,
    pub(crate) stage_index: u32,
    pub(crate) attempt: u32,
    pub(crate) terminal_seq: u64,
    pub(crate) content_sha256: String,
    pub(crate) encoded_bytes: u64,
}

/// The only payload retained by this transaction: bounded UTF-8 text and
/// already-materialized media references. Raw [`Artifact`](crate::magician_v2::execution::Artifact)
/// bytes are deliberately not representable here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PipelineTerminalDeliverable {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primary_text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    media: Vec<PipelineTerminalMediaRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineTerminalMediaRef {
    relative_path: String,
    media_type: String,
    serving_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    caption: Option<String>,
}

impl From<ChildMediaRef> for PipelineTerminalMediaRef {
    fn from(value: ChildMediaRef) -> Self {
        Self {
            relative_path: value.relative_path,
            media_type: value.media_type,
            serving_url: value.serving_url,
            caption: value.caption,
        }
    }
}

impl From<PipelineTerminalMediaRef> for ChildMediaRef {
    fn from(value: PipelineTerminalMediaRef) -> Self {
        Self {
            relative_path: value.relative_path,
            media_type: value.media_type,
            serving_url: value.serving_url,
            caption: value.caption,
        }
    }
}

impl PipelineTerminalDeliverable {
    pub(crate) fn from_parts(primary_text: Option<String>, media: Vec<ChildMediaRef>) -> Self {
        Self {
            primary_text,
            media: media.into_iter().map(Into::into).collect(),
        }
    }

    pub(crate) fn into_parts(self) -> (Option<String>, Vec<ChildMediaRef>) {
        (
            self.primary_text,
            self.media.into_iter().map(Into::into).collect(),
        )
    }

    /// Prefer the durable stage delta, falling back to bounded text still held
    /// by the terminal outcome. Binary outcome artifacts are never copied into
    /// the sidecar; they must first be materialized and appear as media refs.
    pub(crate) fn with_outcome_fallback(
        mut self,
        outcome: &AgenticOutcome,
    ) -> Result<Self, ArtifactV2Error> {
        if self
            .primary_text
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
        {
            self.primary_text = bounded_primary_text_from_outcome(outcome)?;
        }
        validate_deliverable(None, &self)?;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineTerminalSettlementRecord {
    schema_version: u8,
    binding: PipelineTerminalSettlementBinding,
    deliverable: PipelineTerminalDeliverable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineTerminalSettlementEnvelope {
    schema_version: u8,
    descriptor: PipelineTerminalSettlementRef,
    record: PipelineTerminalSettlementRecord,
    hmac_sha256: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PipelineTerminalSettlementStore {
    workspace: ArtifactV2Workspace,
}

impl PipelineTerminalSettlementStore {
    pub(crate) fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    /// Create the immutable sidecar or verify that an earlier retry created
    /// byte-identical authority. A conflicting existing generation is never
    /// overwritten.
    #[cfg(test)]
    pub(crate) async fn create_or_verify(
        &self,
        binding: &PipelineTerminalSettlementBinding,
        deliverable: PipelineTerminalDeliverable,
    ) -> Result<PipelineTerminalSettlementRef, ArtifactV2Error> {
        self.create_or_verify_inner(binding, deliverable, false)
            .await
    }

    /// Retry preparation after a process died between immutable sidecar
    /// creation and the LoopState receipt CAS. The caller may set
    /// `replace_uncommitted` only after proving, under the exact execution
    /// lifecycle exclusion, that LoopState is still at the preparation's
    /// source revision, below its terminal sequence, and carries no receipt.
    /// A prior file is replaced only after its own HMAC and complete binding
    /// verify; corrupt or peer authority always fails closed.
    pub(crate) async fn create_or_replace_uncommitted(
        &self,
        binding: &PipelineTerminalSettlementBinding,
        deliverable: PipelineTerminalDeliverable,
        replace_uncommitted: bool,
    ) -> Result<PipelineTerminalSettlementRef, ArtifactV2Error> {
        self.create_or_verify_inner(binding, deliverable, replace_uncommitted)
            .await
    }

    async fn create_or_verify_inner(
        &self,
        binding: &PipelineTerminalSettlementBinding,
        deliverable: PipelineTerminalDeliverable,
        replace_uncommitted: bool,
    ) -> Result<PipelineTerminalSettlementRef, ArtifactV2Error> {
        validate_binding(binding)?;
        validate_deliverable(Some(binding), &deliverable)?;
        let record = PipelineTerminalSettlementRecord {
            schema_version: SCHEMA_VERSION,
            binding: binding.clone(),
            deliverable,
        };
        let path = self.sidecar_path(binding)?;
        self.ensure_protected_directory(&path).await?;
        let descriptor = descriptor_for_record(&record)?;
        let envelope = self.seal(record, descriptor.clone())?;
        let encoded = serde_json::to_vec(&envelope)?;
        if encoded.len() > MAX_SIDECAR_BYTES {
            return Err(invalid("pipeline_terminal_sidecar_too_large"));
        }

        let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!("pipeline_terminal_sidecar_lock_failed:{error}"))
            })?;

        match self.read_verified_binding_unlocked(&path, binding).await {
            Ok(existing) => {
                if existing.record == envelope.record {
                    return Ok(descriptor);
                }
                if !replace_uncommitted {
                    return Err(invalid("pipeline_terminal_sidecar_conflict"));
                }
                magician_core::durable_io::write_bytes_durably_with_mode(
                    &path,
                    &encoded,
                    Some(0o600),
                )
                .await?;
                Ok(descriptor)
            },
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                magician_core::durable_io::write_bytes_durably_with_mode(
                    &path,
                    &encoded,
                    Some(0o600),
                )
                .await?;
                Ok(descriptor)
            },
            Err(error) => Err(error),
        }
    }

    async fn read_verified_binding_unlocked(
        &self,
        path: &Path,
        binding: &PipelineTerminalSettlementBinding,
    ) -> Result<PipelineTerminalSettlementEnvelope, ArtifactV2Error> {
        self.validate_protected_file(path).await?;
        let envelope = self
            .workspace
            .read_json_bounded_stream_path::<PipelineTerminalSettlementEnvelope, _>(
                path,
                MAX_SIDECAR_BYTES as u64,
                MAX_JSON_DEPTH,
                MAX_JSON_NODES,
            )
            .await?;
        let descriptor = envelope.descriptor.clone();
        self.verify_envelope(binding, &descriptor, &envelope)?;
        Ok(envelope)
    }

    /// Load one exact descriptor. Digest, length, HMAC, embedded axis, nested
    /// field bounds, and expected serving URLs are all revalidated before any
    /// payload is returned.
    pub(crate) async fn read_exact(
        &self,
        binding: &PipelineTerminalSettlementBinding,
        descriptor: &PipelineTerminalSettlementRef,
    ) -> Result<PipelineTerminalDeliverable, ArtifactV2Error> {
        validate_binding(binding)?;
        validate_descriptor(binding, descriptor)?;
        let path = self.sidecar_path(binding)?;
        self.ensure_protected_directory(&path).await?;
        let _guard = AgentStorage::acquire_file_lock_shared(&path)
            .await
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!("pipeline_terminal_sidecar_lock_failed:{error}"))
            })?;
        Ok(self
            .read_verified_unlocked(&path, binding, descriptor)
            .await?
            .record
            .deliverable)
    }

    /// Exact, idempotent abort/retirement. A mismatched descriptor cannot
    /// delete a peer generation, and an already-absent exact file is success.
    pub(crate) async fn abort_exact(
        &self,
        binding: &PipelineTerminalSettlementBinding,
        descriptor: &PipelineTerminalSettlementRef,
    ) -> Result<bool, ArtifactV2Error> {
        validate_binding(binding)?;
        validate_descriptor(binding, descriptor)?;
        let path = self.sidecar_path(binding)?;
        self.ensure_protected_directory(&path).await?;
        let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!("pipeline_terminal_sidecar_lock_failed:{error}"))
            })?;
        match self
            .read_verified_unlocked(&path, binding, descriptor)
            .await
        {
            Ok(_) => {},
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                // A prior unlink may have succeeded immediately before its
                // parent fsync failed or the process died. Re-syncing an
                // already-absent generation makes idempotent retirement close
                // that crash window instead of falsely reporting completion.
                super::io::sync_parent_dir(&path).await?;
                return Ok(false);
            },
            Err(error) => return Err(error),
        }
        self.workspace.remove_file_path(&path).await?;
        super::io::sync_parent_dir(&path).await?;
        Ok(true)
    }

    /// Retire pre-CAS sidecars abandoned by a crashed terminal preparer.
    ///
    /// The caller must hold the base execution's host-wide lifecycle
    /// exclusion. Discovery is deliberately two-pass: the complete bounded
    /// directory is first classified and every sidecar envelope is HMAC-
    /// verified, then exact target rows are re-read under their exclusive file
    /// locks before deletion. Thus a corrupt/oversized/incomplete scan deletes
    /// nothing, a peer execution's row is never touched, and a committed exact
    /// LoopState receipt remains authoritative even if its outer pipeline CAS
    /// has not happened yet.
    pub(crate) async fn reconcile_uncommitted_for_execution(
        &self,
        principal: &str,
        workspace: &str,
        root_execution_id: &str,
    ) -> Result<usize, ArtifactV2Error> {
        validate_scope_component("principal", principal)?;
        validate_scope_component("workspace", workspace)?;
        validate_literal_id("root_execution_id", root_execution_id)?;

        let directory = self
            .workspace
            .scope_root(principal, workspace)
            .join("restricted")
            .join(PROTECTED_DIRECTORY);
        let restricted_root = directory
            .parent()
            .ok_or_else(|| invalid("pipeline_terminal_sidecar_restricted_root_missing"))?;
        AgentStorage::new(restricted_root)
            .ensure_private_directory(&directory)
            .await
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!(
                    "pipeline_terminal_sidecar_directory_security_failed:{error}"
                ))
            })?;
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(ArtifactV2Error::Io(error)),
        };
        let mut visits = 0usize;
        let mut sidecar_paths = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            visits = visits.saturating_add(1);
            if visits > MAX_RECONCILIATION_DIRECTORY_ENTRIES {
                return Err(invalid(
                    "pipeline_terminal_reconciliation_directory_limit_exceeded",
                ));
            }
            let file_name = entry.file_name();
            let file_name = file_name
                .to_str()
                .ok_or_else(|| invalid("pipeline_terminal_reconciliation_filename_invalid"))?;
            let file_type = entry.file_type().await?;
            if (file_name.starts_with('.') && file_name.ends_with(".flock"))
                || is_durable_write_temp_file(file_name)
            {
                if !file_type.is_file() {
                    return Err(invalid("pipeline_terminal_reconciliation_entry_invalid"));
                }
                continue;
            }
            if !file_name.ends_with(".json") || !file_type.is_file() {
                return Err(invalid("pipeline_terminal_reconciliation_entry_invalid"));
            }
            sidecar_paths.push(entry.path());
        }
        sidecar_paths.sort();

        // Validate the whole bounded set before deleting any row. This makes a
        // corrupt sibling a fail-closed reconciliation error rather than a
        // partial garbage-collection pass whose coverage cannot be stated.
        let mut targets = Vec::new();
        for path in sidecar_paths {
            let _guard = AgentStorage::acquire_file_lock_shared(&path)
                .await
                .map_err(|error| {
                    ArtifactV2Error::Runtime(format!(
                        "pipeline_terminal_sidecar_lock_failed:{error}"
                    ))
                })?;
            let envelope = self.read_scanned_envelope_unlocked(&path).await?;
            if envelope.record.binding.principal == principal
                && envelope.record.binding.workspace == workspace
                && envelope.record.binding.root_execution_id == root_execution_id
            {
                targets.push((path, envelope.record.binding, envelope.descriptor));
            }
        }

        use crate::magician_v2::execution::agentic::run_loop::store::{
            fs::FsLoopStateStore, ExecutionKey, LoopStateStore,
        };
        let loop_store = FsLoopStateStore::new(self.workspace.base_root());
        // Complete every fallible LoopState/HMAC proof before deleting any
        // row. The caller's exact lifecycle exclusion prevents a terminal
        // preparer from replacing a generation during these passes; the
        // projector may only exact-retire one, which is handled as idempotent
        // absence below.
        let mut orphans = Vec::new();
        for (path, binding, descriptor) in targets {
            let key = ExecutionKey::new(
                &binding.principal,
                &binding.workspace,
                &binding.exact_segment_id,
            )
            .map_err(|error| ArtifactV2Error::InvalidRequest(error.to_string()))?;
            let committed = loop_store
                .load(&key)
                .await
                .map_err(|error| ArtifactV2Error::Runtime(error.to_string()))?;
            let exact_receipt_committed = match committed.as_ref().and_then(|committed| {
                committed
                    .state
                    .terminal_settlement_receipt
                    .as_ref()
                    .map(|receipt| (committed, receipt))
            }) {
                Some((committed, receipt)) => {
                    // A snapshot checksum is not authority for terminal
                    // settlement. Recompute the independent scope HMAC before
                    // allowing its descriptor to retain deliverable bytes.
                    verify_terminal_receipt_hmac(&self.workspace, receipt)?;
                    committed.state.journal_seq == binding.terminal_seq
                        && receipt.descriptor.principal.as_str() == binding.principal.as_str()
                        && receipt.descriptor.workspace.as_str() == binding.workspace.as_str()
                        && receipt.descriptor.task_id.as_str() == binding.task_id.as_str()
                        && receipt.descriptor.base_execution_id.as_str()
                            == binding.root_execution_id.as_str()
                        && receipt.descriptor.exact_segment_id.as_str()
                            == binding.exact_segment_id.as_str()
                        && receipt.descriptor.terminal_kind.as_str()
                            == binding.terminal_kind.as_str()
                        && receipt.descriptor.terminal_seq == binding.terminal_seq
                        && receipt.descriptor.pipeline_stage_settlement.as_ref()
                            == Some(&descriptor)
                },
                None => false,
            };
            if exact_receipt_committed {
                continue;
            }
            orphans.push((path, binding, descriptor));
        }

        let mut retired = 0usize;
        for (path, binding, descriptor) in orphans {
            let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
                .await
                .map_err(|error| {
                    ArtifactV2Error::Runtime(format!(
                        "pipeline_terminal_sidecar_lock_failed:{error}"
                    ))
                })?;
            let envelope = match self.read_scanned_envelope_unlocked(&path).await {
                Ok(envelope) => envelope,
                Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    // The terminal projector may already have retired the
                    // exact row before this exclusive lock was acquired.
                    continue;
                },
                Err(error) => return Err(error),
            };
            if envelope.record.binding != binding || envelope.descriptor != descriptor {
                return Err(invalid(
                    "pipeline_terminal_reconciliation_generation_changed",
                ));
            }
            self.workspace.remove_file_path(&path).await?;
            super::io::sync_parent_dir(&path).await?;
            retired = retired.saturating_add(1);
        }
        Ok(retired)
    }

    async fn read_scanned_envelope_unlocked(
        &self,
        path: &Path,
    ) -> Result<PipelineTerminalSettlementEnvelope, ArtifactV2Error> {
        self.validate_protected_file(path).await?;
        let envelope = self
            .workspace
            .read_json_bounded_stream_path::<PipelineTerminalSettlementEnvelope, _>(
                path,
                MAX_SIDECAR_BYTES as u64,
                MAX_JSON_DEPTH,
                MAX_JSON_NODES,
            )
            .await?;
        let binding = envelope.record.binding.clone();
        let descriptor = envelope.descriptor.clone();
        self.verify_envelope(&binding, &descriptor, &envelope)?;
        if self.sidecar_path(&binding)? != path {
            return Err(invalid("pipeline_terminal_sidecar_path_mismatch"));
        }
        Ok(envelope)
    }

    async fn read_verified_unlocked(
        &self,
        path: &Path,
        binding: &PipelineTerminalSettlementBinding,
        descriptor: &PipelineTerminalSettlementRef,
    ) -> Result<PipelineTerminalSettlementEnvelope, ArtifactV2Error> {
        self.validate_protected_file(path).await?;
        let envelope = self
            .workspace
            .read_json_bounded_stream_path::<PipelineTerminalSettlementEnvelope, _>(
                path,
                MAX_SIDECAR_BYTES as u64,
                MAX_JSON_DEPTH,
                MAX_JSON_NODES,
            )
            .await?;
        self.verify_envelope(binding, descriptor, &envelope)?;
        Ok(envelope)
    }

    async fn validate_protected_file(&self, path: &Path) -> Result<(), ArtifactV2Error> {
        let metadata = self
            .workspace
            .symlink_metadata_path(path)
            .await?
            .ok_or_else(|| {
                ArtifactV2Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "pipeline terminal sidecar is absent",
                ))
            })?;
        if !metadata.file_type().is_file() {
            return Err(invalid("pipeline_terminal_sidecar_not_regular_file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
                || metadata.nlink() != 1
            {
                return Err(invalid("pipeline_terminal_sidecar_file_security_mismatch"));
            }
        }
        Ok(())
    }

    fn seal(
        &self,
        record: PipelineTerminalSettlementRecord,
        descriptor: PipelineTerminalSettlementRef,
    ) -> Result<PipelineTerminalSettlementEnvelope, ArtifactV2Error> {
        let proof = serde_json::to_vec(&(SEAL_DOMAIN, SCHEMA_VERSION, &descriptor, &record))?;
        let scope = magicllm::LlmScope::new(&record.binding.principal, &record.binding.workspace);
        let hmac_sha256 =
            crate::magician_v2::analytics::llm_trace_content::scoped_content_fingerprint(
                &self.workspace,
                &scope,
                &proof,
            )
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!("pipeline_terminal_sidecar_seal_failed:{error}"))
            })?;
        Ok(PipelineTerminalSettlementEnvelope {
            schema_version: SCHEMA_VERSION,
            descriptor,
            record,
            hmac_sha256,
        })
    }

    fn verify_envelope(
        &self,
        binding: &PipelineTerminalSettlementBinding,
        descriptor: &PipelineTerminalSettlementRef,
        envelope: &PipelineTerminalSettlementEnvelope,
    ) -> Result<(), ArtifactV2Error> {
        if envelope.schema_version != SCHEMA_VERSION
            || envelope.record.schema_version != SCHEMA_VERSION
            || &envelope.record.binding != binding
            || &envelope.descriptor != descriptor
        {
            return Err(invalid("pipeline_terminal_sidecar_identity_mismatch"));
        }
        validate_binding(&envelope.record.binding)?;
        validate_descriptor(binding, &envelope.descriptor)?;
        validate_deliverable(Some(binding), &envelope.record.deliverable)?;
        let expected_descriptor = descriptor_for_record(&envelope.record)?;
        if expected_descriptor != envelope.descriptor {
            return Err(invalid("pipeline_terminal_sidecar_content_mismatch"));
        }
        let expected = self.seal(envelope.record.clone(), envelope.descriptor.clone())?;
        if !constant_time_eq(
            expected.hmac_sha256.as_bytes(),
            envelope.hmac_sha256.as_bytes(),
        ) {
            return Err(invalid("pipeline_terminal_sidecar_hmac_mismatch"));
        }
        Ok(())
    }

    fn sidecar_path(
        &self,
        binding: &PipelineTerminalSettlementBinding,
    ) -> Result<PathBuf, ArtifactV2Error> {
        let identity = serde_json::to_vec(&(SEAL_DOMAIN, SCHEMA_VERSION, binding))?;
        let filename = format!("{}.json", hex::encode(Sha256::digest(identity)));
        Ok(self
            .workspace
            .scope_root(&binding.principal, &binding.workspace)
            .join("restricted")
            .join(PROTECTED_DIRECTORY)
            .join(filename))
    }

    async fn ensure_protected_directory(&self, path: &Path) -> Result<(), ArtifactV2Error> {
        let directory = path
            .parent()
            .ok_or_else(|| invalid("pipeline_terminal_sidecar_protected_directory_missing"))?;
        let restricted_root = directory
            .parent()
            .ok_or_else(|| invalid("pipeline_terminal_sidecar_restricted_root_missing"))?;
        AgentStorage::new(restricted_root)
            .ensure_private_directory(directory)
            .await
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!(
                    "pipeline_terminal_sidecar_directory_security_failed:{error}"
                ))
            })
    }
}

/// Transactional counterpart to `collect_child_delegation_deliverable`.
///
/// Missing output/index files that mean "no deliverable yet" remain empty,
/// while corrupt or unreadable execution/output/index state is returned to the
/// caller. Receipt preparation must retry such failures instead of sealing an
/// empty authoritative result.
pub(crate) async fn collect_pipeline_deliverable_strict<R: V3ReadApi + ?Sized>(
    read_api: &R,
    workspace: &ArtifactV2Workspace,
    scope: &ScopeRef,
    task_id: &str,
    execution_id: &str,
) -> Result<PipelineTerminalDeliverable, ArtifactV2Error> {
    ArtifactV2Workspace::validate_task_id(task_id)?;
    validate_literal_id("execution_id", execution_id)?;
    let outputs = read_api
        .get_execution_outputs(scope, task_id, execution_id)
        .await?;
    if outputs.task_id != task_id || outputs.execution_id != execution_id {
        return Err(invalid("pipeline_deliverable_execution_identity_mismatch"));
    }

    let primary = outputs
        .primary_execution_output_id
        .as_deref()
        .and_then(|id| {
            outputs
                .outputs
                .iter()
                .chain(outputs.child_outputs.iter())
                .find(|output| output.output_id == id)
        })
        .or_else(|| outputs.outputs.first());
    let primary_text = match primary {
        Some(output) => read_primary_output_strict(workspace, scope, task_id, output).await?,
        None => None,
    };

    let artifact_store = FilesystemExecutionArtifactIndexStore::new(workspace.clone());
    let records = artifact_store
        .list_artifacts(scope, task_id, execution_id)
        .await?;
    let outputs_dir = workspace.task_outputs_dir(scope.principal(), scope.workspace(), task_id);
    let canonical_outputs_dir = workspace.canonicalize_path(&outputs_dir).await.ok();
    let mut media_by_path = BTreeMap::<String, ChildMediaRef>::new();
    for record in records {
        let content_type = record
            .payload
            .get("content_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !is_media_type(content_type) {
            continue;
        }
        let task_relative_path = record
            .payload
            .get("task_relative_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("pipeline_media_task_path_missing"))?;
        let relative_path = task_relative_path
            .strip_prefix("outputs/")
            .ok_or_else(|| invalid("pipeline_media_task_path_outside_outputs"))?;
        validate_media_path(relative_path)?;
        if content_type.len() > MAX_MEDIA_TYPE_BYTES {
            return Err(invalid("pipeline_media_type_too_large"));
        }
        let canonical_outputs_dir = match canonical_outputs_dir.as_ref() {
            Some(path) => path,
            None => {
                return Err(invalid("pipeline_media_outputs_directory_missing"));
            },
        };
        let canonical_media = workspace
            .canonicalize_path(outputs_dir.join(relative_path))
            .await?;
        if !canonical_media.starts_with(canonical_outputs_dir)
            || canonical_media == *canonical_outputs_dir
        {
            return Err(invalid("pipeline_media_path_escape"));
        }
        let metadata = workspace
            .symlink_metadata_path(&canonical_media)
            .await?
            .ok_or_else(|| invalid("pipeline_media_file_missing"))?;
        if !metadata.file_type().is_file() {
            return Err(invalid("pipeline_media_path_not_file"));
        }
        let serving_url = serving_url(scope, task_id, relative_path);
        let candidate = ChildMediaRef {
            relative_path: relative_path.to_owned(),
            media_type: content_type.to_owned(),
            serving_url,
            caption: None,
        };
        if let Some(existing) = media_by_path.get(relative_path) {
            if existing != &candidate {
                return Err(invalid("pipeline_media_reference_conflict"));
            }
            continue;
        }
        if media_by_path.len() >= MAX_MEDIA_REFS {
            return Err(invalid("pipeline_media_reference_limit_exceeded"));
        }
        media_by_path.insert(relative_path.to_owned(), candidate);
    }

    let deliverable = PipelineTerminalDeliverable::from_parts(
        primary_text,
        media_by_path.into_values().collect(),
    );
    validate_deliverable(None, &deliverable)?;
    Ok(deliverable)
}

async fn read_primary_output_strict(
    workspace: &ArtifactV2Workspace,
    scope: &ScopeRef,
    task_id: &str,
    output: &super::models::OutputRef,
) -> Result<Option<String>, ArtifactV2Error> {
    if output.relative_path.is_empty() || output.relative_path.len() > MAX_MEDIA_PATH_BYTES {
        return Err(invalid("pipeline_primary_output_path_invalid"));
    }
    let task_dir = workspace.task_dir(scope.principal(), scope.workspace(), task_id);
    let canonical_task_dir = workspace.canonicalize_path(&task_dir).await?;
    let canonical_output = workspace
        .canonicalize_path(task_dir.join(&output.relative_path))
        .await?;
    if !canonical_output.starts_with(&canonical_task_dir) || canonical_output == canonical_task_dir
    {
        return Err(invalid("pipeline_primary_output_path_escape"));
    }
    let metadata = workspace
        .symlink_metadata_path(&canonical_output)
        .await?
        .ok_or_else(|| invalid("pipeline_primary_output_missing"))?;
    if !metadata.file_type().is_file() {
        return Err(invalid("pipeline_primary_output_not_file"));
    }

    let media_type = output
        .media_type
        .split(';')
        .next()
        .unwrap_or(&output.media_type)
        .trim()
        .to_ascii_lowercase();
    if !is_text_type(&media_type) {
        return Ok(Some(format!(
            "Output available at `{}` ({media_type}).",
            output.relative_path
        )));
    }
    let mut bytes = workspace
        .read_prefix_path(
            &canonical_output,
            (MAX_PRIMARY_TEXT_BYTES as u64).saturating_add(1),
        )
        .await?;
    let truncated = bytes.len() > MAX_PRIMARY_TEXT_BYTES;
    if truncated {
        bytes.truncate(MAX_PRIMARY_TEXT_BYTES);
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])
                .map_err(|_| invalid("pipeline_primary_output_not_utf8"))?
        },
        Err(_) => return Err(invalid("pipeline_primary_output_not_utf8")),
    };
    let trimmed = text.trim();
    if truncated {
        return Ok(Some(mark_text_truncated(trimmed)));
    }
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

fn bounded_primary_text_from_outcome(
    outcome: &AgenticOutcome,
) -> Result<Option<String>, ArtifactV2Error> {
    let artifacts = match outcome {
        AgenticOutcome::Success { artifacts, .. } => artifacts,
        AgenticOutcome::Failed { reason, .. } => {
            return Ok(bounded_nonempty_text(&format!(
                "Stage did not complete: {reason}"
            )));
        },
        _ => return Ok(None),
    };
    let mut text = String::new();
    for artifact in artifacts {
        if !is_text_type(&artifact.content_type.to_ascii_lowercase()) {
            continue;
        }
        let decoded = std::str::from_utf8(&artifact.data)
            .map_err(|_| invalid("pipeline_outcome_text_artifact_not_utf8"))?;
        let decoded = decoded.trim();
        if decoded.is_empty() {
            continue;
        }
        let separator = if text.is_empty() { "" } else { "\n\n" };
        if push_bounded(&mut text, separator) || push_bounded(&mut text, decoded) {
            break;
        }
    }
    Ok(bounded_nonempty_text(&text))
}

fn descriptor_for_record(
    record: &PipelineTerminalSettlementRecord,
) -> Result<PipelineTerminalSettlementRef, ArtifactV2Error> {
    let bytes = serde_json::to_vec(record)?;
    if bytes.len() > MAX_SIDECAR_BYTES {
        return Err(invalid("pipeline_terminal_record_too_large"));
    }
    Ok(PipelineTerminalSettlementRef {
        schema_version: SCHEMA_VERSION,
        stage_index: record.binding.stage_index,
        attempt: record.binding.attempt,
        terminal_seq: record.binding.terminal_seq,
        content_sha256: hex::encode(Sha256::digest(&bytes)),
        encoded_bytes: u64::try_from(bytes.len())
            .map_err(|_| invalid("pipeline_terminal_record_length_overflow"))?,
    })
}

fn validate_binding(binding: &PipelineTerminalSettlementBinding) -> Result<(), ArtifactV2Error> {
    validate_scope_component("principal", &binding.principal)?;
    validate_scope_component("workspace", &binding.workspace)?;
    for (label, value) in [
        ("root_execution_id", binding.root_execution_id.as_str()),
        ("exact_segment_id", binding.exact_segment_id.as_str()),
    ] {
        validate_literal_id(label, value)?;
    }
    if binding.task_id.len() > MAX_ID_BYTES {
        return Err(invalid("pipeline_terminal_task_id_invalid"));
    }
    ArtifactV2Workspace::validate_task_id(&binding.task_id)?;
    if binding.terminal_kind.is_empty()
        || binding.terminal_kind.len() > MAX_TERMINAL_KIND_BYTES
        || !binding
            .terminal_kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        || binding.attempt == 0
        || binding.terminal_seq == 0
    {
        return Err(invalid("pipeline_terminal_binding_invalid"));
    }
    Ok(())
}

fn validate_scope_component(label: &str, value: &str) -> Result<(), ArtifactV2Error> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(invalid(&format!("pipeline_terminal_{label}_invalid")));
    }
    Ok(())
}

fn validate_descriptor(
    binding: &PipelineTerminalSettlementBinding,
    descriptor: &PipelineTerminalSettlementRef,
) -> Result<(), ArtifactV2Error> {
    if descriptor.schema_version != SCHEMA_VERSION
        || descriptor.stage_index != binding.stage_index
        || descriptor.attempt != binding.attempt
        || descriptor.terminal_seq != binding.terminal_seq
        || descriptor.encoded_bytes == 0
        || descriptor.encoded_bytes > MAX_SIDECAR_BYTES as u64
        || descriptor.content_sha256.len() != 64
        || !descriptor
            .content_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid("pipeline_terminal_descriptor_invalid"));
    }
    Ok(())
}

fn validate_deliverable(
    binding: Option<&PipelineTerminalSettlementBinding>,
    deliverable: &PipelineTerminalDeliverable,
) -> Result<(), ArtifactV2Error> {
    if deliverable
        .primary_text
        .as_ref()
        .is_some_and(|text| text.len() > MAX_PRIMARY_TEXT_BYTES)
        || deliverable.media.len() > MAX_MEDIA_REFS
    {
        return Err(invalid("pipeline_terminal_deliverable_limit_exceeded"));
    }
    let mut prior_path: Option<&str> = None;
    for media in &deliverable.media {
        validate_media_path(&media.relative_path)?;
        if media.media_type.len() > MAX_MEDIA_TYPE_BYTES || !is_media_type(&media.media_type) {
            return Err(invalid("pipeline_terminal_media_type_invalid"));
        }
        if media.serving_url.is_empty() || media.serving_url.len() > MAX_MEDIA_URL_BYTES {
            return Err(invalid("pipeline_terminal_media_url_invalid"));
        }
        if media
            .caption
            .as_ref()
            .is_some_and(|caption| caption.len() > MAX_MEDIA_CAPTION_BYTES)
        {
            return Err(invalid("pipeline_terminal_media_caption_too_large"));
        }
        if prior_path.is_some_and(|prior| prior >= media.relative_path.as_str()) {
            return Err(invalid("pipeline_terminal_media_paths_not_unique_sorted"));
        }
        if let Some(binding) = binding {
            let scope =
                ScopeRef::system_internal_unauthenticated(&binding.principal, &binding.workspace);
            if media.serving_url != serving_url(&scope, &binding.task_id, &media.relative_path) {
                return Err(invalid("pipeline_terminal_media_url_mismatch"));
            }
        }
        prior_path = Some(&media.relative_path);
    }
    Ok(())
}

fn validate_literal_id(label: &str, value: &str) -> Result<(), ArtifactV2Error> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.trim() != value
        || Path::new(value).is_absolute()
        || value
            .chars()
            .any(|ch| ch == '/' || ch == '\\' || ch == ':' || ch.is_control())
    {
        return Err(invalid(&format!("pipeline_terminal_{label}_invalid")));
    }
    let mut components = Path::new(value).components();
    if !matches!(components.next(), Some(Component::Normal(segment)) if segment == value)
        || components.next().is_some()
    {
        return Err(invalid(&format!("pipeline_terminal_{label}_invalid")));
    }
    Ok(())
}

fn is_durable_write_temp_file(file_name: &str) -> bool {
    file_name
        .strip_prefix(".artifact-write-")
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|nonce| {
            nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn validate_media_path(path: &str) -> Result<(), ArtifactV2Error> {
    if path.is_empty()
        || path.len() > MAX_MEDIA_PATH_BYTES
        || Path::new(path).is_absolute()
        || path
            .chars()
            .any(|ch| ch == '\\' || ch == ':' || ch.is_control())
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid("pipeline_terminal_media_path_invalid"));
    }
    Ok(())
}

fn serving_url(_scope: &ScopeRef, task_id: &str, relative_path: &str) -> String {
    format!(
        "/api/magician/v3/tasks/{}/outputs/{}",
        task_id, relative_path,
    )
}

fn is_media_type(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.starts_with("image/") || value.starts_with("video/") || value.starts_with("audio/")
}

fn is_text_type(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.starts_with("text/")
        || value.contains("json")
        || value.contains("markdown")
        || value.contains("html")
        || value.contains("xml")
}

fn bounded_nonempty_text(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut bounded = String::new();
    push_bounded(&mut bounded, value);
    Some(bounded)
}

fn push_bounded(target: &mut String, value: &str) -> bool {
    let remaining = MAX_PRIMARY_TEXT_BYTES.saturating_sub(target.len());
    if value.len() <= remaining {
        target.push_str(value);
        return false;
    }
    let payload_limit = MAX_PRIMARY_TEXT_BYTES.saturating_sub(PRIMARY_TEXT_TRUNCATION_MARKER.len());
    truncate_utf8(target, payload_limit);
    let remaining = payload_limit.saturating_sub(target.len());
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= remaining)
        .last()
        .unwrap_or(0);
    target.push_str(&value[..boundary]);
    target.push_str(PRIMARY_TEXT_TRUNCATION_MARKER);
    true
}

fn mark_text_truncated(value: &str) -> String {
    let mut marked = value.to_owned();
    let payload_limit = MAX_PRIMARY_TEXT_BYTES.saturating_sub(PRIMARY_TEXT_TRUNCATION_MARKER.len());
    truncate_utf8(&mut marked, payload_limit);
    marked.push_str(PRIMARY_TEXT_TRUNCATION_MARKER);
    marked
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn verify_terminal_receipt_hmac(
    workspace: &ArtifactV2Workspace,
    receipt: &crate::magician_v2::execution::agentic::run_loop::state::TerminalSettlementReceipt,
) -> Result<(), ArtifactV2Error> {
    const RECEIPT_SCHEMA_VERSION: u32 = 1;
    const RECEIPT_SEAL_DOMAIN: &str = "magician.stateless_terminal_settlement.v1";
    if receipt.schema_version != RECEIPT_SCHEMA_VERSION
        || receipt.descriptor.schema_version != RECEIPT_SCHEMA_VERSION
    {
        return Err(invalid(
            "pipeline_terminal_reconciliation_receipt_schema_invalid",
        ));
    }
    let proof = serde_json::to_vec(&(
        RECEIPT_SEAL_DOMAIN,
        RECEIPT_SCHEMA_VERSION,
        &receipt.descriptor,
    ))?;
    let scope =
        magicllm::LlmScope::new(&receipt.descriptor.principal, &receipt.descriptor.workspace);
    let expected = crate::magician_v2::analytics::llm_trace_content::scoped_content_fingerprint(
        workspace, &scope, &proof,
    )
    .map_err(|error| {
        ArtifactV2Error::Runtime(format!(
            "pipeline_terminal_reconciliation_receipt_seal_failed:{error}"
        ))
    })?;
    if !constant_time_eq(expected.as_bytes(), receipt.hmac_sha256.as_bytes()) {
        return Err(invalid(
            "pipeline_terminal_reconciliation_receipt_hmac_mismatch",
        ));
    }
    Ok(())
}

fn invalid(code: &str) -> ArtifactV2Error {
    ArtifactV2Error::InvalidRequest(code.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> PipelineTerminalSettlementBinding {
        PipelineTerminalSettlementBinding {
            principal: "principal".to_owned(),
            workspace: "workspace".to_owned(),
            task_id: "task_1".to_owned(),
            root_execution_id: "exec_1".to_owned(),
            exact_segment_id: "exec_1-s2-a1".to_owned(),
            terminal_kind: "success".to_owned(),
            stage_index: 2,
            attempt: 1,
            terminal_seq: 17,
        }
    }

    fn deliverable(binding: &PipelineTerminalSettlementBinding) -> PipelineTerminalDeliverable {
        let scope =
            ScopeRef::system_internal_unauthenticated(&binding.principal, &binding.workspace);
        PipelineTerminalDeliverable {
            primary_text: Some("durable stage answer".to_owned()),
            media: vec![PipelineTerminalMediaRef::from(ChildMediaRef {
                relative_path: "images/result.png".to_owned(),
                media_type: "image/png".to_owned(),
                serving_url: serving_url(&scope, &binding.task_id, "images/result.png"),
                caption: Some("result".to_owned()),
            })],
        }
    }

    #[tokio::test]
    async fn create_read_retry_and_exact_abort_are_idempotent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace);
        let binding = binding();
        let deliverable = deliverable(&binding);

        let first = store
            .create_or_verify(&binding, deliverable.clone())
            .await
            .expect("first create");
        let retry = store
            .create_or_verify(&binding, deliverable.clone())
            .await
            .expect("idempotent retry");
        assert_eq!(first, retry);
        assert_eq!(
            store
                .read_exact(&binding, &first)
                .await
                .expect("read exact"),
            deliverable
        );
        assert!(store
            .abort_exact(&binding, &first)
            .await
            .expect("first abort"));
        assert!(!store
            .abort_exact(&binding, &first)
            .await
            .expect("idempotent abort"));
    }

    #[tokio::test]
    async fn conflicting_retry_does_not_replace_first_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace);
        let binding = binding();
        let first_payload = deliverable(&binding);
        let descriptor = store
            .create_or_verify(&binding, first_payload.clone())
            .await
            .expect("first create");
        let mut conflicting = first_payload.clone();
        conflicting.primary_text = Some("different".to_owned());

        let error = store
            .create_or_verify(&binding, conflicting)
            .await
            .expect_err("conflict must fail");
        assert!(error.to_string().contains("pipeline_terminal_sidecar"));
        assert_eq!(
            store
                .read_exact(&binding, &descriptor)
                .await
                .expect("original remains"),
            first_payload
        );
    }

    #[tokio::test]
    async fn proven_uncommitted_retry_replaces_only_verified_exact_orphan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace);
        let binding = binding();
        let first_payload = deliverable(&binding);
        let first = store
            .create_or_verify(&binding, first_payload)
            .await
            .expect("orphan preparation");
        let mut retry_payload = deliverable(&binding);
        retry_payload.primary_text = Some("filesystem delta after crashed preparation".to_owned());

        let replacement = store
            .create_or_replace_uncommitted(&binding, retry_payload.clone(), true)
            .await
            .expect("proven pre-CAS orphan can be replaced");
        assert_ne!(replacement, first);
        assert_eq!(
            store
                .read_exact(&binding, &replacement)
                .await
                .expect("replacement remains exact"),
            retry_payload
        );
        assert!(store.read_exact(&binding, &first).await.is_err());
    }

    #[tokio::test]
    async fn descriptor_mismatch_cannot_read_or_delete_exact_generation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace);
        let binding = binding();
        let descriptor = store
            .create_or_verify(&binding, deliverable(&binding))
            .await
            .expect("create");
        let mut mismatched = descriptor.clone();
        mismatched.terminal_seq += 1;

        assert!(store.read_exact(&binding, &mismatched).await.is_err());
        assert!(store.abort_exact(&binding, &mismatched).await.is_err());
        assert!(store.read_exact(&binding, &descriptor).await.is_ok());
    }

    #[tokio::test]
    async fn tampered_sidecar_fails_integrity_and_is_not_deleted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace.clone());
        let binding = binding();
        let descriptor = store
            .create_or_verify(&binding, deliverable(&binding))
            .await
            .expect("create");
        let path = store.sidecar_path(&binding).expect("path");
        let mut envelope: PipelineTerminalSettlementEnvelope = workspace
            .read_json_bounded_stream_path(
                &path,
                MAX_SIDECAR_BYTES as u64,
                MAX_JSON_DEPTH,
                MAX_JSON_NODES,
            )
            .await
            .expect("read envelope");
        envelope.record.deliverable.primary_text = Some("tampered".to_owned());
        workspace
            .write_json_compact_atomic_path(&path, &envelope)
            .await
            .expect("tamper fixture");

        assert!(store.read_exact(&binding, &descriptor).await.is_err());
        assert!(store.abort_exact(&binding, &descriptor).await.is_err());
        assert!(workspace
            .symlink_metadata_path(&path)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn reconciliation_retires_only_an_exact_pre_cas_orphan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace.clone());
        let binding = binding();
        let descriptor = store
            .create_or_verify(&binding, deliverable(&binding))
            .await
            .expect("pre-CAS sidecar");

        assert_eq!(
            store
                .reconcile_uncommitted_for_execution(
                    &binding.principal,
                    &binding.workspace,
                    &binding.root_execution_id,
                )
                .await
                .expect("reconcile orphan"),
            1
        );
        assert!(store.read_exact(&binding, &descriptor).await.is_err());
    }

    async fn commit_terminal_receipt(
        workspace: &ArtifactV2Workspace,
        binding: &PipelineTerminalSettlementBinding,
        descriptor: PipelineTerminalSettlementRef,
        corrupt_receipt_hmac: bool,
    ) {
        use crate::magician_v2::execution::agentic::run_loop::{
            journal::{JournalAppend, RecordedStep, TerminalKind},
            outcome::Phase,
            state::{
                LoopState, RunIdentity, TerminalOutcomeProjection, TerminalSettlementDescriptor,
                TerminalSettlementReceipt,
            },
            store::{fs::FsLoopStateStore, ExecutionKey, LoopStateStore, Revision},
        };

        let loop_store = FsLoopStateStore::new(workspace.base_root());
        let key = ExecutionKey::new(
            &binding.principal,
            &binding.workspace,
            &binding.exact_segment_id,
        )
        .expect("execution key");
        let terminal_seq = loop_store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Epilogue,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::Success,
                    },
                )],
            )
            .await
            .expect("terminal append");
        assert_eq!(terminal_seq, binding.terminal_seq);
        let receipt_descriptor = TerminalSettlementDescriptor {
            schema_version: 1,
            principal: binding.principal.clone(),
            workspace: binding.workspace.clone(),
            task_id: binding.task_id.clone(),
            base_execution_id: binding.root_execution_id.clone(),
            exact_segment_id: binding.exact_segment_id.clone(),
            terminal_kind: binding.terminal_kind.clone(),
            terminal_seq,
            outcome: TerminalOutcomeProjection {
                execution_status: "completed".to_owned(),
                task_status: "completed".to_owned(),
                outcome_type: "success".to_owned(),
                outcome_summary: "done".to_owned(),
                iterations_used: Some(1),
                is_terminal: true,
                completion_kind: None,
                open_items: Vec::new(),
            },
            waiting_for_user_diff_approval: false,
            pipeline_loop_state_segment: None,
            pipeline_stage_settlement: Some(descriptor),
            pause_key: None,
            pause_revision: None,
            pause_body_sha256: None,
            pause_resume_segment: None,
        };
        let proof = serde_json::to_vec(&(
            "magician.stateless_terminal_settlement.v1",
            1_u32,
            &receipt_descriptor,
        ))
        .expect("receipt proof");
        let scope = magicllm::LlmScope::new(&binding.principal, &binding.workspace);
        let mut hmac_sha256 =
            crate::magician_v2::analytics::llm_trace_content::scoped_content_fingerprint(
                workspace, &scope, &proof,
            )
            .expect("receipt seal");
        if corrupt_receipt_hmac {
            hmac_sha256 = "0".repeat(hmac_sha256.len());
        }
        let mut state = LoopState::new(RunIdentity {
            task_id: Some(binding.task_id.clone()),
            execution_id: Some(binding.root_execution_id.clone()),
            principal: Some(binding.principal.clone()),
            workspace: Some(binding.workspace.clone()),
            ..RunIdentity::default()
        });
        state.journal_seq = terminal_seq;
        state.terminal_settlement_receipt = Some(TerminalSettlementReceipt {
            schema_version: 1,
            hmac_sha256,
            descriptor: receipt_descriptor,
        });
        loop_store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("terminal state commit");
    }

    #[tokio::test]
    async fn reconciliation_preserves_a_sidecar_named_by_a_sealed_committed_receipt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace.clone());
        let mut binding = binding();
        binding.terminal_seq = 1;
        let descriptor = store
            .create_or_verify(&binding, deliverable(&binding))
            .await
            .expect("sidecar");
        commit_terminal_receipt(&workspace, &binding, descriptor.clone(), false).await;

        assert_eq!(
            store
                .reconcile_uncommitted_for_execution(
                    &binding.principal,
                    &binding.workspace,
                    &binding.root_execution_id,
                )
                .await
                .expect("reconcile committed receipt"),
            0
        );
        assert!(store.read_exact(&binding, &descriptor).await.is_ok());
    }

    #[tokio::test]
    async fn reconciliation_fails_closed_on_an_unsealed_committed_receipt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace.clone());
        let mut binding = binding();
        binding.terminal_seq = 1;
        let descriptor = store
            .create_or_verify(&binding, deliverable(&binding))
            .await
            .expect("sidecar");
        commit_terminal_receipt(&workspace, &binding, descriptor.clone(), true).await;

        let error = store
            .reconcile_uncommitted_for_execution(
                &binding.principal,
                &binding.workspace,
                &binding.root_execution_id,
            )
            .await
            .expect_err("unsealed receipt is not retention authority");
        assert!(error.to_string().contains("receipt_hmac_mismatch"));
        assert!(store.read_exact(&binding, &descriptor).await.is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_directory_rejects_a_symlinked_authority_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let escape = tempfile::tempdir().expect("escape tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = PipelineTerminalSettlementStore::new(workspace.clone());
        let binding = binding();
        let restricted = workspace
            .scope_root(&binding.principal, &binding.workspace)
            .join("restricted");
        workspace
            .create_dir_all_path(restricted.parent().expect("scope root"))
            .await
            .expect("scope root");
        symlink(escape.path(), &restricted).expect("symlink authority root");

        let reconcile_error = store
            .reconcile_uncommitted_for_execution(
                &binding.principal,
                &binding.workspace,
                &binding.root_execution_id,
            )
            .await
            .expect_err("reconciliation must not scan a redirected authority root");
        assert!(reconcile_error
            .to_string()
            .contains("pipeline_terminal_sidecar_directory_security_failed"));

        let error = store
            .create_or_verify(&binding, deliverable(&binding))
            .await
            .expect_err("protected sidecar must not follow a redirected root");
        assert!(error
            .to_string()
            .contains("pipeline_terminal_sidecar_directory_security_failed"));
        assert!(std::fs::read_dir(escape.path())
            .expect("escape remains readable")
            .next()
            .is_none());
    }

    #[test]
    fn nested_bounds_and_media_authority_are_enforced() {
        let binding = binding();
        let mut oversized = deliverable(&binding);
        oversized.primary_text = Some("x".repeat(MAX_PRIMARY_TEXT_BYTES + 1));
        assert!(validate_deliverable(Some(&binding), &oversized).is_err());

        let mut wrong_url = deliverable(&binding);
        wrong_url.media[0].serving_url = "/different".to_owned();
        assert!(validate_deliverable(Some(&binding), &wrong_url).is_err());

        let mut traversal = deliverable(&binding);
        traversal.media[0].relative_path = "../secret".to_owned();
        assert!(validate_deliverable(Some(&binding), &traversal).is_err());
    }

    #[test]
    fn reconciliation_recognizes_only_exact_durable_writer_temp_names() {
        assert!(is_durable_write_temp_file(
            ".artifact-write-0123456789abcdef0123456789abcdef.tmp"
        ));
        assert!(!is_durable_write_temp_file(".artifact-write-short.tmp"));
        assert!(!is_durable_write_temp_file(
            ".artifact-write-0123456789abcdef0123456789abcdeg.tmp"
        ));
        assert!(!is_durable_write_temp_file(
            "artifact-write-0123456789abcdef0123456789abcdef.tmp"
        ));
    }

    #[test]
    fn binary_outcome_artifacts_are_never_copied_into_sidecar_text() {
        use crate::magician_v2::execution::{Artifact, EnvironmentState};

        let outcome = AgenticOutcome::Success {
            completion: crate::magician_v2::execution::agentic::types::CompletionKind::Full,
            open: Vec::new(),
            final_state: EnvironmentState::Uninitialized,
            iterations_used: 1,
            artifacts: vec![Artifact {
                name: "image".to_owned(),
                content_type: "image/png".to_owned(),
                data: vec![0, 1, 2, 3],
                artifact_type: None,
                render_hints: None,
                materialized_path: None,
            }],
        };
        let resolved = PipelineTerminalDeliverable::default()
            .with_outcome_fallback(&outcome)
            .expect("bounded fallback");
        assert!(resolved.primary_text.is_none());
        assert!(resolved.media.is_empty());
    }

    #[test]
    fn oversized_text_is_explicitly_marked_not_silently_cut() {
        let text = "é".repeat(MAX_PRIMARY_TEXT_BYTES);
        let bounded = bounded_nonempty_text(&text).expect("bounded text");
        assert!(bounded.len() <= MAX_PRIMARY_TEXT_BYTES);
        assert!(bounded.ends_with(PRIMARY_TEXT_TRUNCATION_MARKER));
    }

    #[test]
    fn invalid_utf8_text_artifact_fails_closed() {
        use crate::magician_v2::execution::{Artifact, EnvironmentState};

        let outcome = AgenticOutcome::Success {
            completion: crate::magician_v2::execution::agentic::types::CompletionKind::Full,
            open: Vec::new(),
            final_state: EnvironmentState::Uninitialized,
            iterations_used: 1,
            artifacts: vec![Artifact {
                name: "invalid-text".to_owned(),
                content_type: "text/plain".to_owned(),
                data: vec![0xff, 0xfe],
                artifact_type: None,
                render_hints: None,
                materialized_path: None,
            }],
        };
        assert!(PipelineTerminalDeliverable::default()
            .with_outcome_fallback(&outcome)
            .is_err());
    }
}
