//! Restricted, audited reads and lifecycle management for sanitized LLM I/O.
//!
//! This module is deliberately separate from the ordinary fact reader. It
//! never registers its datasets in the fact catalog and requires a short-lived
//! server-issued grant bound to one scope, actor, call and reason.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{NaiveDate, Utc};
use duckdb::{params, Connection};
use magicllm::LlmScope;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    duckdb_safety::{analytics_duckdb_guard, configure_analytics_connection_checked},
    llm_scoped_path::ensure_real_scoped_directory_chain,
    llm_trace_content::{resanitize_restricted_payload, LlmContentCaptureSettingsHandle},
    llm_trace_journal::LlmTraceJournalError,
    llm_trace_recorder::{
        LlmContentAccessAudit, LlmContentTombstone, LlmTraceRecorder,
        LLM_RESTRICTED_CONTENT_SCHEMA_VERSION,
    },
};
use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace, secrets::SecretStoreResolver,
};

const DEFAULT_GRANT_TTL_SECS: u64 = 60;
const MAX_GRANT_TTL_SECS: u64 = 300;
const MAX_RESTRICTED_PARQUET_FILES: usize = 10_000;
const MAX_RESTRICTED_RESPONSE_BYTES: usize = 1_048_576;
const MAX_RESTRICTED_REVISIONS_PER_CALL: usize = 256;

pub fn discover_restricted_recovery_scopes(
    workspace: &ArtifactV2Workspace,
) -> Result<Vec<LlmScope>, LlmTraceJournalError> {
    let mut recovery = Vec::new();
    for (principal, workspace_name) in workspace.list_scope_segments_sync()? {
        let scope = LlmScope::new(principal, workspace_name);
        if !scope.is_valid() {
            return Err(LlmTraceJournalError::Sequence(format!(
                "invalid on-disk scope component during restricted LLM journal recovery: {}/{}",
                scope.principal, scope.workspace
            )));
        }
        if workspace
            .analytics_llm_restricted_journal_root(&scope.principal, &scope.workspace)
            .exists()
        {
            recovery.push(scope);
        }
    }
    Ok(recovery)
}

#[derive(Debug, Error)]
pub enum LlmRestrictedContentError {
    #[error("restricted content scope or target is invalid")]
    InvalidTarget,
    #[error("restricted content reason must contain 1..=256 non-control characters")]
    InvalidReason,
    #[error("restricted content grant is missing, forged, expired, consumed, or out of scope")]
    GrantDenied,
    #[error("restricted content was deleted")]
    Deleted,
    #[error("restricted content was not found")]
    NotFound,
    #[error("restricted content cannot be re-redacted safely: {0}")]
    RedactionUnavailable(&'static str),
    #[error("restricted content response exceeds the bounded reveal budget")]
    ResponseTooLarge,
    #[error("restricted content access could not be durably audited")]
    AuditUnavailable,
    #[error("restricted content storage is unavailable: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct IssuedLlmContentGrant {
    pub token: String,
    pub expires_at_ms: i64,
    pub target_kind: String,
    pub target_id: String,
}

/// Typed grant accepted by in-process callers. Fields remain private so an LLM
/// argument map cannot manufacture this authority object.
#[derive(Clone)]
pub struct LlmContentReadGrant {
    token: String,
}

#[derive(Clone)]
struct GrantRecord {
    scope: LlmScope,
    actor_id: String,
    target_kind: String,
    target_id: String,
    reason: String,
    execution_id: Option<String>,
    expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestrictedLlmContentRevision {
    pub phase: String,
    pub provider_attempt_index: Option<u32>,
    pub observed_at_ms: i64,
    pub capture_status: String,
    pub redaction_version_at_capture: String,
    pub redaction_version_at_read: String,
    pub redaction_categories_at_read: Vec<String>,
    pub content_fingerprint: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestrictedLlmCallContent {
    pub scope: LlmScope,
    pub llm_call_id: String,
    pub revisions: Vec<RestrictedLlmContentRevision>,
}

#[derive(Clone)]
pub struct LlmRestrictedContentService {
    workspace: ArtifactV2Workspace,
    recorder: Arc<dyn LlmTraceRecorder>,
    secret_store_resolver: Arc<SecretStoreResolver>,
    settings: LlmContentCaptureSettingsHandle,
    grants: Arc<Mutex<HashMap<String, GrantRecord>>>,
    live_tombstones: Arc<Mutex<HashMap<(LlmScope, String, String), i64>>>,
}

impl LlmRestrictedContentService {
    pub fn new(
        workspace: ArtifactV2Workspace,
        recorder: Arc<dyn LlmTraceRecorder>,
        secret_store_resolver: Arc<SecretStoreResolver>,
        settings: LlmContentCaptureSettingsHandle,
    ) -> Self {
        Self {
            workspace,
            recorder,
            secret_store_resolver,
            settings,
            grants: Arc::new(Mutex::new(HashMap::new())),
            live_tombstones: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn issue_call_read_grant(
        &self,
        scope: LlmScope,
        actor_id: &str,
        llm_call_id: &str,
        reason: &str,
        execution_id: Option<String>,
        ttl_secs: Option<u64>,
    ) -> Result<IssuedLlmContentGrant, LlmRestrictedContentError> {
        validate_scope_target(&scope, "llm_call", llm_call_id)?;
        validate_identifier(actor_id)?;
        validate_reason(reason)?;
        if execution_id
            .as_deref()
            .is_some_and(|value| validate_identifier(value).is_err())
        {
            return Err(LlmRestrictedContentError::InvalidTarget);
        }
        let ttl_secs = ttl_secs
            .unwrap_or(DEFAULT_GRANT_TTL_SECS)
            .clamp(1, MAX_GRANT_TTL_SECS);
        let expires_at_ms = now_ms().saturating_add(
            i64::try_from(ttl_secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000),
        );
        let mut raw = [0_u8; 32];
        OsRng.fill_bytes(&mut raw);
        let token = hex::encode(raw);
        let key = grant_key(&token);
        self.grants
            .lock()
            .expect("LLM content grant lock poisoned")
            .insert(
                key,
                GrantRecord {
                    scope,
                    actor_id: actor_id.to_string(),
                    target_kind: "llm_call".to_string(),
                    target_id: llm_call_id.to_string(),
                    // Audit rows must remain useful without becoming a second
                    // arbitrary-text persistence surface.
                    reason: machine_reason(reason),
                    execution_id,
                    expires_at_ms,
                },
            );
        Ok(IssuedLlmContentGrant {
            token,
            expires_at_ms,
            target_kind: "llm_call".to_string(),
            target_id: llm_call_id.to_string(),
        })
    }

    pub fn read_call_content_with_token(
        &self,
        scope: &LlmScope,
        token: &str,
        llm_call_id: &str,
    ) -> Result<RestrictedLlmCallContent, LlmRestrictedContentError> {
        self.read_call_content(
            scope,
            &LlmContentReadGrant {
                token: token.to_string(),
            },
            llm_call_id,
        )
    }

    pub fn read_call_content(
        &self,
        scope: &LlmScope,
        grant: &LlmContentReadGrant,
        llm_call_id: &str,
    ) -> Result<RestrictedLlmCallContent, LlmRestrictedContentError> {
        validate_scope_target(scope, "llm_call", llm_call_id)?;
        let grant_record = self.consume_grant(scope, &grant.token, "llm_call", llm_call_id)?;

        let outcome = (|| {
            if self.is_tombstoned(scope, "llm_call", llm_call_id)? {
                return Err(LlmRestrictedContentError::Deleted);
            }
            let stored = read_call_io_rows(&self.workspace, scope, llm_call_id)?;
            if stored.is_empty() {
                return Err(LlmRestrictedContentError::NotFound);
            }
            let settings = self.settings.snapshot();
            let mut revisions = Vec::with_capacity(stored.len());
            for row in stored {
                let payload: Value = serde_json::from_str(&row.payload_json)
                    .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
                let (payload, report) = resanitize_restricted_payload(
                    &self.workspace,
                    self.secret_store_resolver.as_ref(),
                    scope,
                    &settings,
                    &payload,
                )
                .map_err(LlmRestrictedContentError::RedactionUnavailable)?;
                revisions.push(RestrictedLlmContentRevision {
                    phase: row.phase,
                    provider_attempt_index: row.provider_attempt_index,
                    observed_at_ms: row.observed_at_ms,
                    capture_status: row.capture_status,
                    redaction_version_at_capture: row.redaction_version,
                    redaction_version_at_read: report.policy_version,
                    redaction_categories_at_read: report.categories,
                    content_fingerprint: row.content_fingerprint,
                    payload,
                });
            }
            let response = RestrictedLlmCallContent {
                scope: scope.clone(),
                llm_call_id: llm_call_id.to_string(),
                revisions,
            };
            let bytes = serde_json::to_vec(&response)
                .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?
                .len();
            if bytes > MAX_RESTRICTED_RESPONSE_BYTES {
                return Err(LlmRestrictedContentError::ResponseTooLarge);
            }
            Ok((response, bytes))
        })();

        let (outcome_name, bytes) = match &outcome {
            Ok((_, bytes)) => ("granted", u64::try_from(*bytes).unwrap_or(u64::MAX)),
            Err(LlmRestrictedContentError::Deleted) => ("deleted", 0),
            Err(LlmRestrictedContentError::NotFound) => ("not_found", 0),
            Err(LlmRestrictedContentError::ResponseTooLarge) => ("response_oversize", 0),
            Err(LlmRestrictedContentError::RedactionUnavailable(_)) => ("redaction_unavailable", 0),
            Err(_) => ("storage_failure", 0),
        };
        self.audit(&grant_record, outcome_name, bytes)?;
        outcome.map(|(response, _)| response)
    }

    pub fn tombstone_call_content(
        &self,
        scope: LlmScope,
        llm_call_id: &str,
        reason: &str,
    ) -> Result<String, LlmRestrictedContentError> {
        validate_scope_target(&scope, "llm_call", llm_call_id)?;
        validate_reason(reason)?;
        let at_ms = now_ms();
        let tombstone_id = ulid::Ulid::new().to_string();
        let tombstone = LlmContentTombstone {
            schema_version: LLM_RESTRICTED_CONTENT_SCHEMA_VERSION,
            tombstone_id: tombstone_id.clone(),
            scope: scope.clone(),
            target_kind: "llm_call".to_string(),
            target_id: llm_call_id.to_string(),
            reason: machine_reason(reason),
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms,
        };
        // This marker is the restart-safe denial authority. The recorder copy
        // remains useful for governed analytics but is intentionally async.
        write_immediate_tombstone(&self.workspace, &tombstone)?;
        self.recorder
            .record_content_tombstone(tombstone)
            .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
        self.live_tombstones
            .lock()
            .expect("LLM content tombstone lock poisoned")
            .insert(
                (scope, "llm_call".to_string(), llm_call_id.to_string()),
                at_ms,
            );
        Ok(tombstone_id)
    }

    fn consume_grant(
        &self,
        scope: &LlmScope,
        token: &str,
        target_kind: &str,
        target_id: &str,
    ) -> Result<GrantRecord, LlmRestrictedContentError> {
        let key = grant_key(token.trim());
        let mut grants = self.grants.lock().expect("LLM content grant lock poisoned");
        grants.retain(|_, grant| grant.expires_at_ms >= now_ms());
        let Some(grant) = grants.remove(&key) else {
            self.audit_denied(scope, target_kind, target_id, "grant_denied")?;
            return Err(LlmRestrictedContentError::GrantDenied);
        };
        if grant.expires_at_ms < now_ms()
            || &grant.scope != scope
            || grant.target_kind != target_kind
            || grant.target_id != target_id
        {
            self.audit_denied(scope, target_kind, target_id, "grant_scope_mismatch")?;
            return Err(LlmRestrictedContentError::GrantDenied);
        }
        Ok(grant)
    }

    fn is_tombstoned(
        &self,
        scope: &LlmScope,
        target_kind: &str,
        target_id: &str,
    ) -> Result<bool, LlmRestrictedContentError> {
        if self
            .live_tombstones
            .lock()
            .expect("LLM content tombstone lock poisoned")
            .contains_key(&(
                scope.clone(),
                target_kind.to_string(),
                target_id.to_string(),
            ))
        {
            return Ok(true);
        }
        read_tombstone_exists(&self.workspace, scope, target_kind, target_id)
    }

    fn audit(
        &self,
        grant: &GrantRecord,
        outcome: &str,
        bytes_returned: u64,
    ) -> Result<(), LlmRestrictedContentError> {
        let at_ms = now_ms();
        self.persist_audit(LlmContentAccessAudit {
            schema_version: LLM_RESTRICTED_CONTENT_SCHEMA_VERSION,
            audit_id: ulid::Ulid::new().to_string(),
            scope: grant.scope.clone(),
            actor_id: grant.actor_id.clone(),
            execution_id: grant.execution_id.clone(),
            target_kind: grant.target_kind.clone(),
            target_id: grant.target_id.clone(),
            content_kind: "sanitized_llm_call_io".to_string(),
            reason: grant.reason.clone(),
            redaction_version: self.settings.snapshot().redaction.policy_version,
            bytes_returned,
            outcome: outcome.to_string(),
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms,
        })
    }

    fn audit_denied(
        &self,
        scope: &LlmScope,
        target_kind: &str,
        target_id: &str,
        reason: &str,
    ) -> Result<(), LlmRestrictedContentError> {
        if validate_scope_target(scope, target_kind, target_id).is_err() {
            return Err(LlmRestrictedContentError::InvalidTarget);
        }
        let at_ms = now_ms();
        self.persist_audit(LlmContentAccessAudit {
            schema_version: LLM_RESTRICTED_CONTENT_SCHEMA_VERSION,
            audit_id: ulid::Ulid::new().to_string(),
            scope: scope.clone(),
            actor_id: "untrusted_request".to_string(),
            execution_id: None,
            target_kind: target_kind.to_string(),
            target_id: target_id.to_string(),
            content_kind: "sanitized_llm_call_io".to_string(),
            reason: reason.to_string(),
            redaction_version: self.settings.snapshot().redaction.policy_version,
            bytes_returned: 0,
            outcome: "denied".to_string(),
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms,
        })
    }

    fn persist_audit(&self, audit: LlmContentAccessAudit) -> Result<(), LlmRestrictedContentError> {
        // The synchronous marker is the durability boundary for a reveal.
        // The journaled copy remains the governed Parquet analytics surface.
        write_immediate_access_audit(&self.workspace, &audit)?;
        self.recorder
            .record_content_access_audit(audit)
            .map(|_| ())
            .map_err(|_| LlmRestrictedContentError::AuditUnavailable)
    }
}

#[derive(Debug)]
struct StoredCallIoRow {
    phase: String,
    provider_attempt_index: Option<u32>,
    observed_at_ms: i64,
    capture_status: String,
    redaction_version: String,
    content_fingerprint: String,
    payload_json: String,
}

fn read_call_io_rows(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    llm_call_id: &str,
) -> Result<Vec<StoredCallIoRow>, LlmRestrictedContentError> {
    let root = workspace.analytics_llm_call_io_root(&scope.principal, &scope.workspace);
    let files = restricted_parquet_files(workspace, &root)?;
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let _guard = analytics_duckdb_guard();
    let conn = Connection::open_in_memory()
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    configure_analytics_connection_checked(&conn, "restricted_llm_content_read")
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    let source = parquet_list_sql(&files);
    let mut statement = conn
        .prepare(&format!(
            "SELECT content_phase, provider_attempt_index, observed_at_ms, capture_status, redaction_version, content_fingerprint, restricted_payload_json FROM read_parquet({source}, hive_partitioning = false, union_by_name = true) WHERE principal = ?1 AND workspace = ?2 AND llm_call_id = ?3 ORDER BY record_revision ASC LIMIT {}",
            MAX_RESTRICTED_REVISIONS_PER_CALL + 1
        ))
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    let rows = statement
        .query_map(
            params![&scope.principal, &scope.workspace, llm_call_id],
            |row| {
                Ok(StoredCallIoRow {
                    phase: row.get(0)?,
                    provider_attempt_index: row.get(1)?,
                    observed_at_ms: row.get(2)?,
                    capture_status: row.get(3)?,
                    redaction_version: row.get(4)?,
                    content_fingerprint: row.get(5)?,
                    payload_json: row.get(6)?,
                })
            },
        )
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    let rows = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    if rows.len() > MAX_RESTRICTED_REVISIONS_PER_CALL {
        return Err(LlmRestrictedContentError::ResponseTooLarge);
    }
    Ok(rows)
}

fn read_tombstone_exists(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    target_kind: &str,
    target_id: &str,
) -> Result<bool, LlmRestrictedContentError> {
    let root = workspace.analytics_llm_content_tombstones_root(&scope.principal, &scope.workspace);
    if read_immediate_tombstone_exists(workspace, scope, target_kind, target_id)? {
        return Ok(true);
    }
    let files = restricted_parquet_files(workspace, &root)?;
    if files.is_empty() {
        return Ok(false);
    }
    let _guard = analytics_duckdb_guard();
    let conn = Connection::open_in_memory()
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    configure_analytics_connection_checked(&conn, "restricted_llm_tombstone_read")
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    let source = parquet_list_sql(&files);
    conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM read_parquet({source}, hive_partitioning = false, union_by_name = true) WHERE principal = ?1 AND workspace = ?2 AND content_target_kind = ?3 AND content_target_id = ?4)"),
        params![&scope.principal, &scope.workspace, target_kind, target_id],
        |row| row.get(0),
    )
    .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))
}

fn tombstone_marker_name(target_kind: &str, target_id: &str) -> String {
    let fingerprint = blake3::hash(format!("{target_kind}\0{target_id}").as_bytes())
        .to_hex()
        .to_string();
    format!("revocation-{fingerprint}.json")
}

fn write_immediate_access_audit(
    workspace: &ArtifactV2Workspace,
    audit: &LlmContentAccessAudit,
) -> Result<(), LlmRestrictedContentError> {
    let date = chrono::DateTime::<Utc>::from_timestamp_millis(audit.occurred_at_ms)
        .ok_or(LlmRestrictedContentError::AuditUnavailable)?
        .format("%Y-%m-%d")
        .to_string();
    let root = workspace
        .analytics_llm_content_access_audit_root(&audit.scope.principal, &audit.scope.workspace);
    let partition = root.join(format!("dt={date}"));
    ensure_real_scoped_directory_chain(workspace.base_root(), &partition)
        .map_err(|_| LlmRestrictedContentError::AuditUnavailable)?;
    workspace
        .create_dir_all_path_sync(&partition)
        .map_err(|_| LlmRestrictedContentError::AuditUnavailable)?;
    ensure_real_scoped_directory_chain(workspace.base_root(), &partition)
        .map_err(|_| LlmRestrictedContentError::AuditUnavailable)?;
    workspace
        .write_json_atomic_path_sync(
            partition.join(format!("access-{}.json", audit.audit_id)),
            audit,
        )
        .map_err(|_| LlmRestrictedContentError::AuditUnavailable)
}

fn write_immediate_tombstone(
    workspace: &ArtifactV2Workspace,
    tombstone: &LlmContentTombstone,
) -> Result<(), LlmRestrictedContentError> {
    let date = chrono::DateTime::<Utc>::from_timestamp_millis(tombstone.occurred_at_ms)
        .ok_or(LlmRestrictedContentError::InvalidTarget)?
        .format("%Y-%m-%d")
        .to_string();
    let root = workspace.analytics_llm_content_tombstones_root(
        &tombstone.scope.principal,
        &tombstone.scope.workspace,
    );
    let partition = root.join(format!("dt={date}"));
    ensure_real_scoped_directory_chain(workspace.base_root(), &partition)
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    workspace
        .create_dir_all_path_sync(&partition)
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    ensure_real_scoped_directory_chain(workspace.base_root(), &partition)
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    workspace
        .write_json_atomic_path_sync(
            partition.join(tombstone_marker_name(
                &tombstone.target_kind,
                &tombstone.target_id,
            )),
            tombstone,
        )
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))
}

fn read_immediate_tombstone_exists(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    target_kind: &str,
    target_id: &str,
) -> Result<bool, LlmRestrictedContentError> {
    let root = workspace.analytics_llm_content_tombstones_root(&scope.principal, &scope.workspace);
    ensure_real_scoped_directory_chain(workspace.base_root(), &root)
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    if !root.exists() {
        return Ok(false);
    }
    let marker_name = tombstone_marker_name(target_kind, target_id);
    for partition in safe_child_directories(&root)? {
        if !partition
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with("dt="))
        {
            continue;
        }
        let marker = partition.join(&marker_name);
        match fs::symlink_metadata(&marker) {
            Ok(metadata) if metadata.file_type().is_file() => {
                let stored: LlmContentTombstone = workspace
                    .read_json_path_sync(&marker)
                    .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
                if stored.scope != *scope
                    || stored.target_kind != target_kind
                    || stored.target_id != target_id
                {
                    return Err(LlmRestrictedContentError::Storage(
                        "restricted content revocation marker identity mismatch".to_string(),
                    ));
                }
                return Ok(true);
            },
            Ok(_) => {
                return Err(LlmRestrictedContentError::Storage(
                    "restricted content revocation marker is not a regular file".to_string(),
                ));
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(LlmRestrictedContentError::Storage(error.to_string())),
        }
    }
    Ok(false)
}

fn restricted_parquet_files(
    workspace: &ArtifactV2Workspace,
    root: &Path,
) -> Result<Vec<PathBuf>, LlmRestrictedContentError> {
    ensure_real_scoped_directory_chain(workspace.base_root(), root)
        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?
        {
            let entry =
                entry.map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
            if metadata.file_type().is_symlink() {
                return Err(LlmRestrictedContentError::Storage(
                    "restricted content root contains a symlink".to_string(),
                ));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("parquet")
            {
                files.push(entry.path());
                if files.len() > MAX_RESTRICTED_PARQUET_FILES {
                    return Err(LlmRestrictedContentError::Storage(
                        "restricted content file budget exceeded".to_string(),
                    ));
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

fn parquet_list_sql(files: &[PathBuf]) -> String {
    let values = files
        .iter()
        .map(|path| format!("'{}'", path.to_string_lossy().replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

/// Independent UTC-partition retention for sanitized payloads, content-free
/// context descriptors, tombstones and audit rows.
pub struct LlmRestrictedContentRetention;

impl LlmRestrictedContentRetention {
    pub fn spawn(
        workspace: ArtifactV2Workspace,
        settings: LlmContentCaptureSettingsHandle,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
            loop {
                interval.tick().await;
                let policy = settings.snapshot().retention;
                for (root_name, days) in [
                    ("llm_call_io", policy.sanitized_io_days),
                    ("llm_context_blocks", policy.context_metadata_days),
                    (
                        "llm_content_tombstones",
                        policy.facts_days.max(policy.sanitized_io_days),
                    ),
                    ("llm_content_access_audit", policy.facts_days),
                ] {
                    match sweep_dataset_partitions(&workspace, root_name, days) {
                        Ok(0) => {},
                        // A sweep that removed something is worth a line. Only
                        // the failure was logged before, so the one outcome
                        // nobody could afford to miss — a mass deletion from a
                        // misconfigured horizon — was the silent one.
                        Ok(removed) => tracing::info!(
                            target: "analytics::llm_restricted_content",
                            dataset = root_name,
                            retention_days = days,
                            partitions_removed = removed,
                            "restricted LLM content retention sweep removed partitions"
                        ),
                        Err(error) => tracing::warn!(
                            target: "analytics::llm_restricted_content",
                            dataset = root_name,
                            error = %error,
                            "restricted LLM content retention sweep failed"
                        ),
                    }
                }
            }
        })
    }
}

fn sweep_dataset_partitions(
    workspace: &ArtifactV2Workspace,
    dataset: &str,
    retention_days: u32,
) -> Result<usize, LlmRestrictedContentError> {
    // Zero means "not configured", and the safe reading of that for a
    // destructive sweeper is *skip*, not *delete everything*. With a zero
    // horizon the cutoff is today and `date < cutoff` matches every partition
    // ever written, for every principal and workspace.
    //
    // Config validation rejects a zero here, so this is the second layer: it
    // holds for a policy built in code, and for whatever a future config path
    // forgets to validate. A retention sweeper is the wrong place to discover
    // that an invariant was only enforced somewhere else.
    if retention_days == 0 {
        return Ok(0);
    }
    let scopes_root = workspace.scopes_root();
    if !scopes_root.exists() {
        return Ok(0);
    }
    let cutoff = Utc::now().date_naive() - chrono::Duration::days(i64::from(retention_days));
    let mut removed = 0_usize;
    for principal in safe_child_directories(&scopes_root)? {
        for scoped_workspace in safe_child_directories(&principal)? {
            let root = scoped_workspace.join("analytics").join(dataset);
            if !root.exists() {
                continue;
            }
            ensure_real_scoped_directory_chain(workspace.base_root(), &root)
                .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
            for partition in safe_child_directories(&root)? {
                let Some(name) = partition.file_name().and_then(|value| value.to_str()) else {
                    continue;
                };
                let Some(date) = name.strip_prefix("dt=") else {
                    continue;
                };
                let Ok(date) = NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
                    continue;
                };
                if date < cutoff {
                    fs::remove_dir_all(&partition)
                        .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
                    removed = removed.saturating_add(1);
                }
            }
        }
    }
    Ok(removed)
}

fn safe_child_directories(root: &Path) -> Result<Vec<PathBuf>, LlmRestrictedContentError> {
    let mut directories = Vec::new();
    for entry in
        fs::read_dir(root).map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?
    {
        let entry = entry.map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| LlmRestrictedContentError::Storage(error.to_string()))?;
        if metadata.file_type().is_symlink() {
            return Err(LlmRestrictedContentError::Storage(
                "restricted content directory contains a symlink".to_string(),
            ));
        }
        if metadata.is_dir() {
            directories.push(entry.path());
        }
    }
    Ok(directories)
}

fn validate_scope_target(
    scope: &LlmScope,
    target_kind: &str,
    target_id: &str,
) -> Result<(), LlmRestrictedContentError> {
    if !scope.is_valid() || target_kind != "llm_call" {
        return Err(LlmRestrictedContentError::InvalidTarget);
    }
    validate_identifier(target_id)
}

fn validate_identifier(value: &str) -> Result<(), LlmRestrictedContentError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(LlmRestrictedContentError::InvalidTarget);
    }
    Ok(())
}

fn validate_reason(reason: &str) -> Result<(), LlmRestrictedContentError> {
    let reason = reason.trim();
    if reason.is_empty() || reason.len() > 256 || reason.chars().any(char::is_control) {
        return Err(LlmRestrictedContentError::InvalidReason);
    }
    Ok(())
}

fn machine_reason(reason: &str) -> String {
    let normalized = reason
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() {
        "user_requested".to_string()
    } else {
        normalized.chars().take(64).collect()
    }
}

fn grant_key(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{
        analytics::{
            llm_trace_content::LlmContentCaptureSettingsHandle,
            llm_trace_recorder::{
                LlmTraceRecord, LlmTraceRecordError, LlmTraceRecordSink, TypedLlmTraceRecorder,
            },
        },
        secrets::{InMemoryKeyProvider, SecretRuntimeCapabilities},
    };

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<LlmTraceRecord>>);

    impl LlmTraceRecordSink for RecordingSink {
        fn try_record(&self, record: LlmTraceRecord) -> Result<(), LlmTraceRecordError> {
            self.0.lock().expect("recording sink").push(record);
            Ok(())
        }
    }

    fn service() -> (
        tempfile::TempDir,
        LlmRestrictedContentService,
        Arc<RecordingSink>,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let sink = Arc::new(RecordingSink::default());
        let recorder: Arc<dyn LlmTraceRecorder> =
            Arc::new(TypedLlmTraceRecorder::new(sink.clone()));
        let secrets = Arc::new(SecretStoreResolver::new_with_capabilities(
            Box::new(InMemoryKeyProvider::new()),
            temp.path().to_path_buf(),
            SecretRuntimeCapabilities::fully_available("test"),
        ));
        let settings = LlmContentCaptureSettingsHandle::from_settings(
            crate::config::LlmTraceSettings::default(),
        );
        (
            temp,
            LlmRestrictedContentService::new(workspace, recorder, secrets, settings),
            sink,
        )
    }

    fn write_call_io_fixture(workspace: &ArtifactV2Workspace, call_id: &str, payload: &Value) {
        let partition = workspace
            .analytics_llm_call_io_root("owner", "default")
            .join("dt=2026-07-23");
        fs::create_dir_all(&partition).expect("partition");
        let path = partition.join("fixture.parquet");
        let payload = payload.to_string().replace('\'', "''");
        let path = path.to_string_lossy().replace('\'', "''");
        let call_id = call_id.replace('\'', "''");
        let connection = Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch(&format!(
                "COPY (SELECT 'logical_request'::VARCHAR AS content_phase, NULL::INTEGER AS provider_attempt_index, 1700000000000::BIGINT AS observed_at_ms, 'redacted'::VARCHAR AS capture_status, 'llm-content-redaction-v0'::VARCHAR AS redaction_version, '{}'::VARCHAR AS content_fingerprint, '{}'::VARCHAR AS restricted_payload_json, 'owner'::VARCHAR AS principal, 'default'::VARCHAR AS workspace, '{}'::VARCHAR AS llm_call_id, 1::INTEGER AS record_revision) TO '{}' (FORMAT PARQUET)",
                "a".repeat(64), payload, call_id, path
            ))
            .expect("write fixture parquet");
    }

    #[test]
    fn grant_tokens_are_hashed_and_target_validation_is_strict() {
        assert_ne!(grant_key("token"), "token");
        assert!(
            validate_scope_target(&LlmScope::new("owner", "default"), "llm_call", "call-1").is_ok()
        );
        assert!(
            validate_scope_target(&LlmScope::new("owner", "default"), "llm_call", "../escape")
                .is_err()
        );
    }

    #[test]
    fn deletion_reason_is_machine_bounded() {
        assert_eq!(
            machine_reason("User requested deletion"),
            "user_requested_deletion"
        );
        assert!(machine_reason(&"x".repeat(500)).len() <= 64);
    }

    #[test]
    fn restricted_retention_removes_only_expired_utc_partitions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let root = workspace.analytics_llm_call_io_root("owner", "default");
        let expired = root.join("dt=2000-01-01");
        let current = root.join(format!("dt={}", Utc::now().date_naive()));
        fs::create_dir_all(&expired).expect("expired");
        fs::create_dir_all(&current).expect("current");
        assert_eq!(
            sweep_dataset_partitions(&workspace, "llm_call_io", 30).expect("sweep"),
            1
        );
        assert!(!expired.exists());
        assert!(current.exists());
    }

    #[test]
    fn a_zero_horizon_sweeps_nothing_rather_than_everything() {
        // With `retention_days == 0` the cutoff is today and `date < cutoff`
        // matches every partition ever written. The datasets on this sweep
        // include `llm_content_tombstones` — the proof a user asked for content
        // to be deleted — and `llm_content_access_audit`, neither of which has
        // another copy. Zero means "not configured"; for a destructive sweeper
        // the safe reading of that is skip.
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let root = workspace.analytics_llm_call_io_root("owner", "default");
        let ancient = root.join("dt=2000-01-01");
        let yesterday = root.join(format!(
            "dt={}",
            Utc::now().date_naive() - chrono::Duration::days(1)
        ));
        fs::create_dir_all(&ancient).expect("ancient");
        fs::create_dir_all(&yesterday).expect("yesterday");

        assert_eq!(
            sweep_dataset_partitions(&workspace, "llm_call_io", 0).expect("sweep"),
            0
        );
        assert!(ancient.exists(), "a zero horizon must not delete anything");
        assert!(yesterday.exists());
    }

    #[test]
    fn cross_scope_and_consumed_grants_fail_closed_and_are_audited() {
        let (_temp, service, sink) = service();
        let owner = LlmScope::new("owner", "default");
        let other = LlmScope::new("other", "default");
        let grant = service
            .issue_call_read_grant(
                owner.clone(),
                "setup_admin",
                "call-1",
                "debugging",
                None,
                Some(60),
            )
            .expect("grant");
        assert!(matches!(
            service.read_call_content_with_token(&other, &grant.token, "call-1"),
            Err(LlmRestrictedContentError::GrantDenied)
        ));
        assert!(matches!(
            service.read_call_content_with_token(&owner, &grant.token, "call-1"),
            Err(LlmRestrictedContentError::GrantDenied)
        ));
        let records = sink.0.lock().expect("records");
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record, LlmTraceRecord::ContentAccessAudit(_)))
                .count(),
            2
        );
    }

    #[test]
    fn missing_and_forged_grants_are_denied_and_audited_without_storage_reads() {
        let (_temp, service, sink) = service();
        let scope = LlmScope::new("owner", "default");
        for token in ["", "forged-token"] {
            assert!(matches!(
                service.read_call_content_with_token(&scope, token, "call-unauthorized"),
                Err(LlmRestrictedContentError::GrantDenied)
            ));
        }
        let records = sink.0.lock().expect("records");
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record, LlmTraceRecord::ContentAccessAudit(audit) if audit.outcome == "denied" && audit.bytes_returned == 0))
                .count(),
            2
        );
    }

    #[test]
    fn tombstone_denies_an_already_granted_read_before_any_payload_lookup() {
        let (_temp, service, sink) = service();
        let scope = LlmScope::new("owner", "default");
        let grant = service
            .issue_call_read_grant(
                scope.clone(),
                "setup_admin",
                "call-2",
                "incident_review",
                None,
                Some(60),
            )
            .expect("grant");
        service
            .tombstone_call_content(scope.clone(), "call-2", "User requested deletion")
            .expect("tombstone");
        service
            .live_tombstones
            .lock()
            .expect("live tombstones")
            .clear();
        assert!(service
            .is_tombstoned(&scope, "llm_call", "call-2")
            .expect("durable revocation marker"));
        assert!(matches!(
            service.read_call_content_with_token(&scope, &grant.token, "call-2"),
            Err(LlmRestrictedContentError::Deleted)
        ));
        let records = sink.0.lock().expect("records");
        assert!(records
            .iter()
            .any(|record| matches!(record, LlmTraceRecord::ContentTombstone(_))));
        assert!(records.iter().any(|record| {
            matches!(record, LlmTraceRecord::ContentAccessAudit(audit) if audit.outcome == "deleted")
        }));
    }

    #[test]
    fn restricted_read_reapplies_current_redaction_and_audits_returned_bytes() {
        let (_temp, service, sink) = service();
        write_call_io_fixture(
            &service.workspace,
            "call-3",
            &serde_json::json!({"text": "api_key=must-never-return"}),
        );
        let scope = LlmScope::new("owner", "default");
        let grant = service
            .issue_call_read_grant(
                scope.clone(),
                "setup_admin",
                "call-3",
                "redaction_regression",
                None,
                Some(60),
            )
            .expect("grant");
        let response = service
            .read_call_content_with_token(&scope, &grant.token, "call-3")
            .expect("restricted read");
        let encoded = serde_json::to_string(&response).expect("response JSON");
        assert!(!encoded.contains("must-never-return"));
        assert!(encoded.contains("[REDACTED]"));
        assert!(sink.0.lock().expect("records").iter().any(|record| {
            matches!(record, LlmTraceRecord::ContentAccessAudit(audit) if audit.outcome == "granted" && audit.bytes_returned > 0)
        }));
        let audit_root = service
            .workspace
            .analytics_llm_content_access_audit_root("owner", "default");
        let durable_audits = fs::read_dir(audit_root)
            .expect("audit root")
            .flat_map(|partition| {
                fs::read_dir(partition.expect("partition").path()).expect("partition rows")
            })
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("access-") && name.ends_with(".json"))
            })
            .count();
        assert_eq!(durable_audits, 1);
    }
}
