//! File-based chat session storage implementation.
//! Each session stores metadata in a small JSON file and display messages in
//! bounded JSONL segments in the scoped V3 UI workspace.
//!
//! Follows the same scoped file-per-record pattern used by the V3 artifact stores:
//! - Metadata: `magician_data_v3/scopes/<principal>/<workspace>/ui/chat_sessions/{session_id}/session.json`
//! - Messages: `magician_data_v3/scopes/<principal>/<workspace>/ui/chat_sessions/{session_id}/messages/{segment}.jsonl`
//! - `DashMap<String, Arc<Mutex<()>>>` per-session write locks
//! - In-memory index for (principal, workspace, ui_thread_id) -> active session lookups

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    hash::{Hash, Hasher},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock, RwLock as StdRwLock,
    },
};

#[cfg(any(test, feature = "test-fixtures"))]
use std::sync::Mutex as StdMutex;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::models::{
    ChatChannel, ChatLlmTranscriptEntry, ChatMessage, ChatMessageContent, ChatSession,
    ChatSessionDocument, ChatSessionFileIndex, ChatSessionFileOrigin, ChatSessionStatus,
    ContentBlockRecord, ContentFileSource,
};
use crate::magician_v2::agents::storage::{AgentStorage, FileLockGuard};
use crate::magician_v2::analytics;
use crate::magician_v2::analytics::event_sink::AnalyticsEvent;
use crate::magician_v2::artifact_v2::{service::ArtifactV2Error, workspace::ArtifactV2Workspace};
use crate::magician_v2::history::{infer_legacy_session_history_lane, HistoryLane};

mod voice_requests;
use super::voice_requests::{InternalVoiceSession, VoiceAdmission, VoiceCoordinatorState, VoiceMutation, VoiceRequest};

#[cfg(not(any(test, feature = "test-fixtures")))]
const CHAT_MESSAGE_SEGMENT_SIZE: usize = 500;
#[cfg(any(test, feature = "test-fixtures"))]
const CHAT_MESSAGE_SEGMENT_SIZE: usize = 3;
const CHAT_MESSAGE_SEGMENT_WIDTH: usize = 6;
const CHAT_MESSAGE_SEGMENT_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub const CHAT_SESSION_FILE_INDEX_MAX_BYTES: usize = 4 * 1024 * 1024;
pub const CHAT_SESSION_FILE_INDEX_MAX_ENTRIES: usize = 10_000;
const CHAT_SESSION_DOCUMENT_FORMAT_VERSION: u32 = 2;
const CHAT_TRANSCRIPT_FORMAT_VERSION: u32 = 1;
const CHAT_TRANSCRIPT_SEGMENT_WIDTH: usize = 12;
const CHAT_TRANSCRIPT_MAX_ENTRIES_PER_APPEND: usize = 512;
const CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES: usize = 4 * 1024 * 1024;
const CHAT_TRANSCRIPT_MAX_MANIFEST_BYTES: usize = 256 * 1024;
const CHAT_TRANSCRIPT_MAX_OPEN_TOOL_CALLS: usize = 1_024;
const CHAT_TRANSCRIPT_MAX_TOOL_CALL_ID_BYTES: usize = 512;
const CHAT_TRANSCRIPT_MAX_TAIL_ENTRIES: usize = 20_000;
const CHAT_TRANSCRIPT_RECENT_SEGMENT_INDEX: usize = 256;
const CHAT_TRANSCRIPT_MAX_JSON_NODES_PER_APPEND: usize = 250_000;
const CHAT_TRANSCRIPT_MAX_ENCODED_JSON_NODES: usize =
    CHAT_TRANSCRIPT_MAX_JSON_NODES_PER_APPEND + 4_096;
pub const CHAT_STORED_JSON_MAX_NODES: usize = 1_000_000;
#[cfg(not(any(test, feature = "test-fixtures")))]
const CHAT_TRANSCRIPT_COMPACTION_SEGMENT_THRESHOLD: u64 = 256;
#[cfg(any(test, feature = "test-fixtures"))]
const CHAT_TRANSCRIPT_COMPACTION_SEGMENT_THRESHOLD: u64 = 8;
const CHAT_TRANSCRIPT_COMPACTION_QUIESCENCE_MS: i64 = 15 * 60 * 1_000;
const CHAT_SESSION_METADATA_FAST_PATH_BYTES: u64 = 256 * 1024;
const CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const CHAT_MESSAGE_MIGRATION_RECORD_MAX_BYTES: usize = 8 * 1024 * 1024;
const CHAT_ROLLBACK_INTENT_FORMAT_VERSION: u32 = 1;
const CHAT_ROLLBACK_INTENT_MAX_BYTES: usize = 256 * 1024;
const CHAT_ROLLBACK_MAX_MESSAGE_IDS: usize = 512;
const CHAT_ROLLBACK_MAX_MESSAGE_ID_BYTES: usize = 1_024;
const CHAT_CLEAR_INTENT_FORMAT_VERSION: u32 = 1;
const CHAT_CLEAR_INTENT_MAX_BYTES: usize = 16 * 1024;
const CHAT_SESSION_DELETION_MARKER_FORMAT_VERSION: u32 = 1;
const CHAT_SESSION_DELETION_MARKER_MAX_BYTES: u64 = 16 * 1024;
const CHAT_SESSION_GENERATION_MARKER_FORMAT_VERSION: u32 = 1;
const CHAT_SESSION_GENERATION_MARKER_MAX_BYTES: u64 = 16 * 1024;
const CHAT_OUTPUT_CLEANUP_INTENT_FORMAT_VERSION: u32 = 1;
const CHAT_OUTPUT_CLEANUP_INTENT_MAX_BYTES: usize = 4 * 1024 * 1024;
const CHAT_OUTPUT_CLEANUP_INTENT_MAX_ITEMS: usize = CHAT_SESSION_FILE_INDEX_MAX_ENTRIES;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ChatTranscriptRecentSegment {
    sequence: u64,
    group_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ChatTranscriptManifest {
    format_version: u32,
    generation: String,
    last_sequence: u64,
    effective_entry_count: u64,
    active_group_id: String,
    #[serde(default)]
    open_tool_call_ids: BTreeSet<String>,
    #[serde(default)]
    legacy_migration_complete: bool,
    #[serde(default)]
    recent_segments: Vec<ChatTranscriptRecentSegment>,
    #[serde(default)]
    compacted_through_sequence: u64,
}

impl ChatTranscriptManifest {
    fn empty() -> Self {
        Self {
            format_version: CHAT_TRANSCRIPT_FORMAT_VERSION,
            generation: Uuid::new_v4().simple().to_string(),
            last_sequence: 0,
            effective_entry_count: 0,
            active_group_id: Uuid::new_v4().simple().to_string(),
            open_tool_call_ids: BTreeSet::new(),
            legacy_migration_complete: true,
            recent_segments: Vec::new(),
            compacted_through_sequence: 0,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ChatTranscriptSegment {
    format_version: u32,
    generation: String,
    sequence: u64,
    group_id: String,
    mutation: ChatTranscriptMutation,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatTranscriptMutation {
    Append {
        entries: Vec<ChatLlmTranscriptEntry>,
    },
    TruncateTail {
        count: u64,
    },
}

#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatRollbackFailpoint {
    AfterIntent,
    AfterDisplayDeletion,
    AfterTranscriptCommit,
}

#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatClearFailpoint {
    AfterIntent,
    BeforeOutputCleanup,
    AfterDisplayDeletion,
    AfterTranscriptReset,
}

#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatOutputCleanupFailpoint {
    AfterIntent,
    AfterIndexPublish,
    BeforeIntentRemoval,
}

#[cfg(any(test, feature = "test-fixtures"))]
static CHAT_ROLLBACK_FAILPOINT: OnceLock<StdMutex<Option<(String, ChatRollbackFailpoint)>>> =
    OnceLock::new();

#[cfg(any(test, feature = "test-fixtures"))]
static CHAT_ROLLBACK_TEST_SERIAL: OnceLock<StdMutex<()>> = OnceLock::new();

#[cfg(any(test, feature = "test-fixtures"))]
static CHAT_CLEAR_FAILPOINT: OnceLock<StdMutex<Option<(String, ChatClearFailpoint)>>> =
    OnceLock::new();

#[cfg(any(test, feature = "test-fixtures"))]
static CHAT_CLEAR_TEST_SERIAL: OnceLock<StdMutex<()>> = OnceLock::new();

#[cfg(any(test, feature = "test-fixtures"))]
static CHAT_OUTPUT_CLEANUP_FAILPOINT: OnceLock<
    StdMutex<Option<(String, ChatOutputCleanupFailpoint)>>,
> = OnceLock::new();

#[cfg(any(test, feature = "test-fixtures"))]
static CHAT_OUTPUT_CLEANUP_TEST_SERIAL: OnceLock<StdMutex<()>> = OnceLock::new();

/// Durable cross-file transaction intent for removing one rejected user turn
/// from both display history and the canonical provider transcript.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ChatRollbackIntent {
    format_version: u32,
    session_id: String,
    message_ids: Vec<String>,
    transcript_generation: String,
    base_last_sequence: u64,
    base_effective_entry_count: u64,
    updated_at: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ChatClearIntent {
    format_version: u32,
    session_id: String,
    updated_at: i64,
}

/// Persistent lifecycle fence written before a chat-session directory is
/// removed. `session_generation` is the storage generation: a stale writer
/// must match both session id and generation. A marker fences only that exact
/// generation, so an explicit restore may reuse the id with a new generation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChatSessionDeletionMarker {
    format_version: u32,
    session_id: String,
    session_generation: String,
    deleted_at: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ChatSessionGenerationMarker {
    format_version: u32,
    session_id: String,
    session_generation: String,
    published_at: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ChatOutputCleanupItem {
    record_id: String,
    stored_name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ChatOutputCleanupIntent {
    format_version: u32,
    session_id: String,
    session_generation: String,
    items: Vec<ChatOutputCleanupItem>,
    updated_at: i64,
}

/// Holds both the process-local mutex and the stable external lifecycle fence.
/// The lifecycle guard is declared first so drop releases it before waking a
/// process-local waiter, the reverse of the local-then-lifecycle acquire order.
struct ChatSessionWriteGuard {
    _lifecycle: FileLockGuard,
    _local: OwnedMutexGuard<()>,
}

fn take_bounded_batch<I>(entries: &mut I, limit: usize) -> Vec<I::Item>
where
    I: Iterator,
{
    entries.take(limit).collect()
}

#[derive(Debug, serde::Deserialize)]
struct ChatSessionMetadataProjection {
    #[serde(default = "legacy_chat_session_document_version")]
    format_version: u32,
    session: ChatSession,
    #[serde(
        rename = "messages",
        default,
        deserialize_with = "deserialize_chat_inline_collection_nonempty"
    )]
    has_inline_messages: bool,
    #[serde(
        rename = "llm_history",
        default,
        deserialize_with = "deserialize_chat_inline_collection_nonempty"
    )]
    has_inline_llm_history: bool,
}

#[derive(Debug, serde::Deserialize)]
struct ChatSessionFormatProjection {
    #[serde(default = "legacy_chat_session_document_version")]
    format_version: u32,
}

fn legacy_chat_session_document_version() -> u32 {
    1
}

fn deserialize_chat_inline_collection_nonempty<'de, D>(
    deserializer: D,
) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct NonemptyCollectionVisitor;

    impl<'de> serde::de::Visitor<'de> for NonemptyCollectionVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a chat inline-history array")
        }

        fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<bool, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut nonempty = false;
            while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                nonempty = true;
            }
            Ok(nonempty)
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<bool, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            while map
                .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                .is_some()
            {}
            Ok(true)
        }

        fn visit_bool<E>(self, _value: bool) -> std::result::Result<bool, E> {
            Ok(true)
        }

        fn visit_i64<E>(self, _value: i64) -> std::result::Result<bool, E> {
            Ok(true)
        }

        fn visit_u64<E>(self, _value: u64) -> std::result::Result<bool, E> {
            Ok(true)
        }

        fn visit_f64<E>(self, _value: f64) -> std::result::Result<bool, E> {
            Ok(true)
        }

        fn visit_str<E>(self, _value: &str) -> std::result::Result<bool, E> {
            Ok(true)
        }

        fn visit_string<E>(self, _value: String) -> std::result::Result<bool, E> {
            Ok(true)
        }

        fn visit_none<E>(self) -> std::result::Result<bool, E> {
            Ok(true)
        }

        fn visit_unit<E>(self) -> std::result::Result<bool, E> {
            Ok(true)
        }
    }

    deserializer.deserialize_any(NonemptyCollectionVisitor)
}

fn validate_session_metadata_projection(projection: &ChatSessionMetadataProjection) -> Result<()> {
    match projection.format_version {
        1 => Ok(()),
        CHAT_SESSION_DOCUMENT_FORMAT_VERSION
            if !projection.has_inline_messages && !projection.has_inline_llm_history =>
        {
            Ok(())
        },
        CHAT_SESSION_DOCUMENT_FORMAT_VERSION => Err(anyhow!(
            "chat session metadata v2 must not contain inline transcript fields"
        )),
        version => Err(anyhow!(
            "unsupported chat session document format version {version}"
        )),
    }
}

struct ChatJsonDepthGuardReader<R> {
    inner: R,
    containers: Vec<u8>,
    expects_value: bool,
    nodes: usize,
    max_nodes: usize,
    in_string: bool,
    escaped: bool,
    bytes: u64,
    max_bytes: u64,
}

impl<R> ChatJsonDepthGuardReader<R> {
    fn new(inner: R, max_bytes: u64, max_nodes: usize) -> Self {
        Self {
            inner,
            containers: Vec::with_capacity(
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
            ),
            expects_value: true,
            nodes: 0,
            max_nodes,
            in_string: false,
            escaped: false,
            bytes: 0,
            max_bytes,
        }
    }
}

impl<R: std::io::Read> std::io::Read for ChatJsonDepthGuardReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes = self.bytes.saturating_add(read as u64);
        if self.bytes > self.max_bytes {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "chat JSON exceeds its configured byte limit",
            ));
        }
        for byte in &buf[..read] {
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if *byte == b'\\' {
                    self.escaped = true;
                } else if *byte == b'"' {
                    self.in_string = false;
                }
                continue;
            }
            match *byte {
                b' ' | b'\n' | b'\r' | b'\t' => {},
                b'"' => {
                    if self.expects_value {
                        if self.nodes >= self.max_nodes {
                            return Err(std::io::Error::new(
                                ErrorKind::InvalidData,
                                "chat JSON exceeds its configured node limit",
                            ));
                        }
                        self.nodes = self.nodes.saturating_add(1);
                    }
                    self.expects_value = false;
                    self.in_string = true;
                },
                b'{' | b'[' => {
                    if self.expects_value {
                        if self.nodes >= self.max_nodes {
                            return Err(std::io::Error::new(
                                ErrorKind::InvalidData,
                                "chat JSON exceeds its configured node limit",
                            ));
                        }
                        self.nodes = self.nodes.saturating_add(1);
                    }
                    if self.containers.len()
                        >= crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH
                    {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            "chat JSON exceeds its configured nesting limit",
                        ));
                    }
                    self.containers.push(*byte);
                    self.expects_value = *byte == b'[';
                },
                b'}' => {
                    if self.containers.pop() != Some(b'{') {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            "chat JSON contains an unmatched closing delimiter",
                        ));
                    }
                    self.expects_value = false;
                },
                b']' => {
                    if self.containers.pop() != Some(b'[') {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            "chat JSON contains an unmatched closing delimiter",
                        ));
                    }
                    self.expects_value = false;
                },
                b':' => self.expects_value = true,
                b',' => self.expects_value = self.containers.last().copied() == Some(b'['),
                _ => {
                    if self.expects_value {
                        if self.nodes >= self.max_nodes {
                            return Err(std::io::Error::new(
                                ErrorKind::InvalidData,
                                "chat JSON exceeds its configured node limit",
                            ));
                        }
                        self.nodes = self.nodes.saturating_add(1);
                        self.expects_value = false;
                    }
                },
            }
        }
        if read == 0 && (!self.containers.is_empty() || self.in_string || self.escaped) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "chat JSON ended before its structure was complete",
            ));
        }
        Ok(read)
    }
}

struct CappedVecWriter {
    bytes: Vec<u8>,
    max_bytes: usize,
    label: &'static str,
}

impl CappedVecWriter {
    fn new(max_bytes: usize, label: &'static str) -> Self {
        Self {
            bytes: Vec::with_capacity(max_bytes.min(8 * 1024)),
            max_bytes,
            label,
        }
    }
}

impl std::io::Write for CappedVecWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(buf.len()) > self.max_bytes {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                format!("{} exceeds its configured byte limit", self.label),
            ));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn serialize_json_bounded<T: serde::Serialize>(
    value: &T,
    max_bytes: usize,
    label: &'static str,
) -> Result<Vec<u8>> {
    let mut writer = CappedVecWriter::new(max_bytes, label);
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}

async fn load_chat_session_deletion_marker(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    session_id: &str,
) -> Result<Option<ChatSessionDeletionMarker>> {
    let path = workspace_layout.chat_session_deletion_marker_path(principal, workspace, session_id);
    let marker = match workspace_layout
        .read_json_bounded_stream_path::<ChatSessionDeletionMarker, _>(
            &path,
            CHAT_SESSION_DELETION_MARKER_MAX_BYTES,
            crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
            64,
        )
        .await
    {
        Ok(marker) => marker,
        Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
            return Ok(None);
        },
        Err(error) => return Err(error.into()),
    };
    if marker.format_version != CHAT_SESSION_DELETION_MARKER_FORMAT_VERSION
        || marker.session_id != session_id
        || marker.session_generation.is_empty()
        || marker.session_generation.len() > 1_024
    {
        return Err(anyhow!("invalid chat-session deletion marker"));
    }
    Ok(Some(marker))
}

fn chat_session_generation(session: &ChatSession) -> String {
    format!("v1:{}:{}", session.id, session.created_at)
}

async fn load_chat_session_generation_marker(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    session_id: &str,
) -> Result<Option<ChatSessionGenerationMarker>> {
    let path =
        workspace_layout.chat_session_generation_marker_path(principal, workspace, session_id);
    let marker = match workspace_layout
        .read_json_bounded_stream_path::<ChatSessionGenerationMarker, _>(
            &path,
            CHAT_SESSION_GENERATION_MARKER_MAX_BYTES,
            crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
            64,
        )
        .await
    {
        Ok(marker) => marker,
        Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
            return Ok(None);
        },
        Err(error) => return Err(error.into()),
    };
    if marker.format_version != CHAT_SESSION_GENERATION_MARKER_FORMAT_VERSION
        || marker.session_id != session_id
        || marker.session_generation.is_empty()
        || marker.session_generation.len() > 1_024
    {
        return Err(anyhow!("invalid chat-session generation marker"));
    }
    Ok(Some(marker))
}

async fn publish_chat_session_generation_marker(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    session_id: &str,
    generation: &str,
) -> Result<()> {
    if generation.is_empty() || generation.len() > 1_024 {
        return Err(anyhow!("invalid chat-session generation"));
    }
    let marker = ChatSessionGenerationMarker {
        format_version: CHAT_SESSION_GENERATION_MARKER_FORMAT_VERSION,
        session_id: session_id.to_string(),
        session_generation: generation.to_string(),
        published_at: Utc::now().timestamp_millis(),
    };
    let bytes = serialize_json_bounded(
        &marker,
        CHAT_SESSION_GENERATION_MARKER_MAX_BYTES as usize,
        "chat-session generation marker",
    )?;
    let path =
        workspace_layout.chat_session_generation_marker_path(principal, workspace, session_id);
    if let Err(error) = workspace_layout.write_atomic_path(&path, &bytes).await {
        match load_chat_session_generation_marker(
            workspace_layout,
            principal,
            workspace,
            session_id,
        )
        .await
        {
            Ok(Some(published)) if published == marker => {
                warn!(
                    session_id,
                    error = %error,
                    "[CHAT-STORE] generation-marker publish reported an error after commit"
                );
            },
            Ok(_) => return Err(error.into()),
            Err(verification_error) => {
                return Err(anyhow!(
                    "chat-session generation publication is uncertain: {error}; readback failed: {verification_error}"
                ));
            },
        }
    }
    Ok(())
}

async fn publish_chat_session_deletion_marker(
    workspace_layout: &ArtifactV2Workspace,
    session: &ChatSession,
) -> Result<ChatSessionDeletionMarker> {
    let session_generation = chat_session_generation(session);
    if session_generation.len() > 1_024 {
        return Err(anyhow!("invalid chat-session generation"));
    }
    let marker = ChatSessionDeletionMarker {
        format_version: CHAT_SESSION_DELETION_MARKER_FORMAT_VERSION,
        session_id: session.id.clone(),
        session_generation,
        deleted_at: Utc::now().timestamp_millis(),
    };
    let bytes = serialize_json_bounded(
        &marker,
        CHAT_SESSION_DELETION_MARKER_MAX_BYTES as usize,
        "chat-session deletion marker",
    )?;
    let path = workspace_layout.chat_session_deletion_marker_path(
        &session.principal,
        &session.workspace,
        &session.id,
    );
    if let Err(error) = workspace_layout.write_atomic_path(&path, &bytes).await {
        match load_chat_session_deletion_marker(
            workspace_layout,
            &session.principal,
            &session.workspace,
            &session.id,
        )
        .await
        {
            Ok(Some(published)) if published == marker => {
                warn!(
                    session_id = %session.id,
                    error = %error,
                    "[CHAT-STORE] deletion-marker publish reported an error after exact commit"
                );
            },
            Ok(_) => return Err(error.into()),
            Err(verification_error) => {
                return Err(anyhow!(
                    "chat-session deletion fence is uncertain: {error}; readback failed: {verification_error}"
                ));
            },
        }
    }
    Ok(marker)
}

async fn load_chat_session_authority_generation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    session_id: &str,
) -> Result<String> {
    let current_path = workspace_layout.chat_session_path(principal, workspace, session_id);
    let path = if workspace_layout
        .metadata_path(&current_path)
        .await?
        .is_some()
    {
        current_path
    } else {
        workspace_layout.legacy_chat_session_path(principal, workspace, session_id)
    };
    let projection = workspace_layout
        .read_json_bounded_stream_path::<ChatSessionMetadataProjection, _>(
            &path,
            CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES,
            crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
            CHAT_STORED_JSON_MAX_NODES,
        )
        .await?;
    validate_session_metadata_projection(&projection)?;
    if projection.session.id != session_id
        || projection.session.principal != principal
        || projection.session.workspace != workspace
    {
        return Err(anyhow!("chat-session scope or identity changed"));
    }
    Ok(chat_session_generation(&projection.session))
}

async fn rollback_chat_session_deletion_marker(
    workspace_layout: &ArtifactV2Workspace,
    session: &ChatSession,
    expected: &ChatSessionDeletionMarker,
) -> Result<()> {
    match load_chat_session_deletion_marker(
        workspace_layout,
        &session.principal,
        &session.workspace,
        &session.id,
    )
    .await?
    {
        Some(current) if &current == expected => {},
        Some(_) => {
            return Err(anyhow!(
                "refusing to roll back a different chat-session deletion fence"
            ));
        },
        None => return Ok(()),
    }
    let path = workspace_layout.chat_session_deletion_marker_path(
        &session.principal,
        &session.workspace,
        &session.id,
    );
    match workspace_layout.remove_file_path(&path).await {
        Ok(()) => Ok(()),
        Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn acquire_chat_session_lifecycle_guard_for_scope(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    session_id: &str,
    expected_generation: Option<&str>,
    allow_generation_replacement: bool,
    allow_matching_deletion_marker: bool,
) -> Result<FileLockGuard> {
    let lock_path =
        workspace_layout.chat_session_lifecycle_lock_path(principal, workspace, session_id);
    let guard = AgentStorage::acquire_file_lock_exclusive(&lock_path)
        .await
        .map_err(|error| anyhow!("lock chat session lifecycle: {error}"))?;
    let current_generation =
        load_chat_session_generation_marker(workspace_layout, principal, workspace, session_id)
            .await?;
    if let Some(expected_generation) = expected_generation {
        match current_generation.as_ref() {
            Some(current) if current.session_generation != expected_generation => {
                if !allow_generation_replacement {
                    return Err(anyhow!(
                        "Chat session generation changed for {session_id}; refusing stale writer"
                    ));
                }
                let session_path =
                    workspace_layout.chat_session_path(principal, workspace, session_id);
                let legacy_path =
                    workspace_layout.legacy_chat_session_path(principal, workspace, session_id);
                if workspace_layout
                    .metadata_path(&session_path)
                    .await?
                    .is_some()
                    || workspace_layout
                        .metadata_path(&legacy_path)
                        .await?
                        .is_some()
                {
                    return Err(anyhow!(
                        "Chat session generation changed for {session_id}; refusing stale writer"
                    ));
                }
                // A deliberate recreation may reuse an id only after the old
                // session tree is absent. Publish its distinct generation
                // while retaining the lifecycle lock; old waiters then see a
                // mismatch after the recreated document becomes visible.
                publish_chat_session_generation_marker(
                    workspace_layout,
                    principal,
                    workspace,
                    session_id,
                    expected_generation,
                )
                .await?;
            },
            Some(_) => {},
            None => {
                if allow_generation_replacement {
                    let session_path =
                        workspace_layout.chat_session_path(principal, workspace, session_id);
                    let legacy_path =
                        workspace_layout.legacy_chat_session_path(principal, workspace, session_id);
                    let authority_exists = workspace_layout
                        .metadata_path(&session_path)
                        .await?
                        .is_some()
                        || workspace_layout
                            .metadata_path(&legacy_path)
                            .await?
                            .is_some();
                    if authority_exists {
                        let authority_generation = load_chat_session_authority_generation(
                            workspace_layout,
                            principal,
                            workspace,
                            session_id,
                        )
                        .await?;
                        if authority_generation != expected_generation {
                            return Err(anyhow!(
                                "Chat session authority already exists for a different generation: {session_id}"
                            ));
                        }
                    }
                }
                publish_chat_session_generation_marker(
                    workspace_layout,
                    principal,
                    workspace,
                    session_id,
                    expected_generation,
                )
                .await?;
            },
        }
    }
    if let Some(marker) =
        load_chat_session_deletion_marker(workspace_layout, principal, workspace, session_id)
            .await?
    {
        let active_generation = expected_generation
            .map(str::to_string)
            .or_else(|| current_generation.map(|current| current.session_generation));
        if !allow_matching_deletion_marker
            && active_generation
                .as_deref()
                .is_none_or(|generation| generation == marker.session_generation)
        {
            return Err(anyhow!(
                "Chat session not found: {session_id} (durable deletion fence; generation={})",
                marker.session_generation
            ));
        }
    }
    Ok(guard)
}

/// Acquire the stable session lifecycle fence for an already-resolved session.
/// Chat-service output writers call this before the file-index lock and retain
/// it through byte + metadata publication, matching FileChatStore's lock order.
pub async fn acquire_chat_session_lifecycle_guard(
    workspace_layout: &ArtifactV2Workspace,
    session: &ChatSession,
) -> Result<FileLockGuard> {
    let generation = chat_session_generation(session);
    let guard = acquire_chat_session_lifecycle_guard_for_scope(
        workspace_layout,
        &session.principal,
        &session.workspace,
        &session.id,
        Some(&generation),
        false,
        false,
    )
    .await?;
    let authority_generation = load_chat_session_authority_generation(
        workspace_layout,
        &session.principal,
        &session.workspace,
        &session.id,
    )
    .await?;
    if authority_generation != generation {
        return Err(anyhow!(
            "Chat session generation disagrees with persisted authority for {}",
            session.id
        ));
    }
    Ok(guard)
}

/// Acquire lifecycle authority when a storage writer has only the exact scope
/// and session id. New sessions already have a generation marker; legacy
/// sessions fall back to a bounded metadata projection, then publish that
/// derived generation while holding the stable lock.
pub async fn acquire_chat_session_lifecycle_guard_for_existing_scope(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    session_id: &str,
) -> Result<FileLockGuard> {
    let expected_generation = match load_chat_session_generation_marker(
        workspace_layout,
        principal,
        workspace,
        session_id,
    )
    .await?
    {
        Some(marker) => marker.session_generation,
        None => {
            load_chat_session_authority_generation(
                workspace_layout,
                principal,
                workspace,
                session_id,
            )
            .await?
        },
    };
    let guard = acquire_chat_session_lifecycle_guard_for_scope(
        workspace_layout,
        principal,
        workspace,
        session_id,
        Some(&expected_generation),
        false,
        false,
    )
    .await?;
    let authority_generation =
        load_chat_session_authority_generation(workspace_layout, principal, workspace, session_id)
            .await?;
    if authority_generation != expected_generation {
        return Err(anyhow!(
            "Chat session generation marker disagrees with persisted authority for {session_id}"
        ));
    }
    Ok(guard)
}

pub fn chat_session_lifecycle_error_is_not_found(error: &anyhow::Error) -> bool {
    if matches!(
        error.downcast_ref::<ArtifactV2Error>(),
        Some(ArtifactV2Error::Io(io_error)) if io_error.kind() == ErrorKind::NotFound
    ) {
        return true;
    }
    let message = error.to_string();
    message.contains("Chat session not found")
        || message.contains("Chat session generation changed")
        || message.contains("Chat session generation disagrees")
        || message.contains("generation marker disagrees with persisted authority")
        || message.contains("chat-session scope or identity changed")
}

fn is_safe_chat_output_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 || name.contains('\0') {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

struct BoundedDigestWriter {
    digest: Sha256,
    bytes: usize,
    max_bytes: usize,
}

impl BoundedDigestWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            digest: Sha256::new(),
            bytes: 0,
            max_bytes,
        }
    }
}

impl std::io::Write for BoundedDigestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.bytes.saturating_add(buf.len()) > self.max_bytes {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "chat migration record exceeds its configured byte limit",
            ));
        }
        self.bytes += buf.len();
        self.digest.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_json_digest<T: serde::Serialize>(value: &T) -> Result<[u8; 32]> {
    let mut writer = BoundedDigestWriter::new(CHAT_MESSAGE_MIGRATION_RECORD_MAX_BYTES);
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.digest.finalize().into())
}

fn deserialize_guarded_chat_bytes<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    max_bytes: u64,
) -> Result<T> {
    deserialize_guarded_chat_bytes_with_nodes(bytes, max_bytes, CHAT_STORED_JSON_MAX_NODES)
}

fn deserialize_guarded_chat_bytes_with_nodes<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    max_bytes: u64,
    max_nodes: usize,
) -> Result<T> {
    if !crate::magician_v2::json_traversal::json_bytes_nodes_are_bounded(bytes, max_nodes) {
        return Err(anyhow!("chat JSON exceeds its configured node limit"));
    }
    let mut admission =
        ChatJsonDepthGuardReader::new(std::io::Cursor::new(bytes), max_bytes, max_nodes);
    std::io::copy(&mut admission, &mut std::io::sink())?;
    let guarded = ChatJsonDepthGuardReader::new(std::io::Cursor::new(bytes), max_bytes, max_nodes);
    Ok(serde_json::from_reader(guarded)?)
}

async fn read_optional_bounded_chat_path(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    max_bytes: u64,
    label: &'static str,
) -> Result<Option<Vec<u8>>> {
    match workspace
        .read_prefix_path(path, max_bytes.saturating_add(1))
        .await
    {
        Ok(bytes) if bytes.len() as u64 <= max_bytes => Ok(Some(bytes)),
        Ok(_) => Err(anyhow!("{label} exceeds its configured byte limit")),
        Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_guarded_chat_json_file<T: serde::de::DeserializeOwned>(
    path: &Path,
    max_bytes: u64,
) -> Result<T> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > max_bytes {
        return Err(anyhow!("chat JSON exceeds its configured byte limit"));
    }
    let file = std::fs::File::open(path)?;
    let mut admission = ChatJsonDepthGuardReader::new(
        std::io::BufReader::new(file),
        max_bytes,
        CHAT_STORED_JSON_MAX_NODES,
    );
    std::io::copy(&mut admission, &mut std::io::sink())?;
    let file = std::fs::File::open(path)?;
    let guarded = ChatJsonDepthGuardReader::new(
        std::io::BufReader::new(file),
        max_bytes,
        CHAT_STORED_JSON_MAX_NODES,
    );
    Ok(serde_json::from_reader(guarded)?)
}

fn validate_session_document_shape(doc: &ChatSessionDocument) -> Result<()> {
    match doc.format_version {
        1 => Ok(()),
        CHAT_SESSION_DOCUMENT_FORMAT_VERSION => {
            if !doc.messages.is_empty() || !doc.llm_history.is_empty() {
                return Err(anyhow!(
                    "chat session metadata v2 must not contain inline messages or LLM history"
                ));
            }
            Ok(())
        },
        version => Err(anyhow!(
            "unsupported chat session document format version {version}"
        )),
    }
}

/// Threads that keep a single rolling active session: creating a new session archives the
/// prior matching-agent session. These are internal feature surfaces (screen capture,
/// ambient tabs, VibeDev) that maintain exactly one live session per agent/thread. User chat threads (#general and
/// user-created) are intentionally NOT here — their sessions persist until the user
/// archives them manually, so an explicit "New Chat" never auto-archives (Option B).
/// Upper bound on how much of an archived room's transcript is copied into
/// its rejoin.
///
/// A bound rather than "all of it" because the copy is a second physical
/// record: an unbounded replay of a long meeting would double it on disk for
/// the sake of turns nobody is about to refer to. A room needs the recent
/// conversation; anything older is what the meeting's rolling summary is for.
pub const MEETING_CARRY_FORWARD_MESSAGES: usize = 200;

/// Where a meeting bot's rejoin landed, and what it took with it.
///
/// Returned instead of a bare session so the caller can SAY what happened —
/// a room that starts blank because its predecessor was unreachable and a
/// room that starts blank because nothing was ever said in it are the same
/// session and completely different events.
#[derive(Debug, Clone)]
pub struct MeetingRoomSession {
    pub session: ChatSession,
    /// The thread actually used. Differs from the requested one when a live
    /// occurrence of the same meeting was continued across a date boundary.
    pub thread: String,
    /// Set when continuity moved the rejoin off the thread it asked for.
    pub continued_from_thread: Option<String>,
    /// The archived session whose transcript was carried forward, if any.
    pub carried_from_session: Option<String>,
    /// How many messages were carried. A count, never a rate.
    pub carried_message_count: usize,
}

pub fn thread_rotates_sessions(ui_thread_id: &str) -> bool {
    matches!(ui_thread_id, "screens" | "tabs" | "vibedev")
}

// ---------------------------------------------------------------------------
// ChatStore trait
// ---------------------------------------------------------------------------

/// Newest-first window the default [`ChatStore::get_messages_before_exact`]
/// resolves an exact cursor inside. Stores that override the method never read
/// this many messages; the default exists for alternate/test stores, whose
/// sessions are small.
const EXACT_CURSOR_DEFAULT_SCAN_WINDOW: usize = 10_000;

/// Storage trait for chat session persistence.
#[async_trait]
pub trait ChatStore: Send + Sync {
    async fn record_concurrent_voice_task(&self, _session: &ChatSession, _message: &ChatMessage) -> Result<()> { Ok(()) }

    async fn upsert_voice_result_projection(&self, _parent: &ChatSession, _request: &VoiceRequest, _message: ChatMessage) -> Result<bool> {
        Err(anyhow!("concurrent voice storage is not supported"))
    }

    async fn admit_voice_request(&self, _parent_session_id: &str, _admission: VoiceAdmission) -> Result<(VoiceRequest, bool)> {
        Err(anyhow!("concurrent voice storage is not supported"))
    }

    async fn voice_state(&self, _principal: &str, _workspace: &str) -> Result<VoiceCoordinatorState> {
        Err(anyhow!("concurrent voice storage is not supported"))
    }

    async fn mutate_voice_state(&self, _principal: &str, _workspace: &str, _mutation: VoiceMutation) -> Result<VoiceCoordinatorState> {
        Err(anyhow!("concurrent voice storage is not supported"))
    }
    /// Get an active session for the exact principal/workspace/thread/agent, or
    /// create one if no exact agent match exists. `origin` is used only when
    /// creating a new session; ignored when the exact match is reused.
    async fn get_or_create_active_session(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        origin: &ChatChannel,
        agent_id: &str,
    ) -> Result<ChatSession> {
        self.get_or_create_active_session_with_history_lane(
            principal,
            workspace,
            ui_thread_id,
            origin,
            agent_id,
            HistoryLane::Personal,
        )
        .await
    }

    async fn get_or_create_active_session_with_history_lane(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        origin: &ChatChannel,
        agent_id: &str,
        history_lane: HistoryLane,
    ) -> Result<ChatSession>;

    /// Create a fresh session. Ordinary/brainstorming threads preserve active
    /// siblings. Rolling feature threads archive prior sessions for this exact
    /// agent while preserving sessions owned by other agents.
    async fn new_session(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        origin: &ChatChannel,
        agent_id: &str,
    ) -> Result<ChatSession> {
        self.new_session_with_history_lane(
            principal,
            workspace,
            ui_thread_id,
            origin,
            agent_id,
            HistoryLane::Personal,
        )
        .await
    }

    async fn new_session_with_history_lane(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        origin: &ChatChannel,
        agent_id: &str,
        history_lane: HistoryLane,
    ) -> Result<ChatSession>;

    /// Get a session by ID.
    async fn get_session(&self, session_id: &str) -> Result<Option<ChatSession>>;

    /// List all sessions for a principal/workspace, ordered by `updated_at` desc.
    async fn list_sessions(&self, principal: &str, workspace: &str) -> Result<Vec<ChatSession>>;

    /// One `(ui_thread_id, effective history lane)` per session in the scope,
    /// in no particular order.
    ///
    /// The ui-threads reconciler wants only these two fields, and asking for
    /// them via `list_sessions` costs a session lock plus a document read per
    /// session. File-backed stores override this with their in-memory index,
    /// which already carries everything the lane derivation needs. The default
    /// keeps other stores correct without forcing them to build an index.
    async fn list_session_thread_lanes(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, HistoryLane)>> {
        Ok(self
            .list_sessions(principal, workspace)
            .await?
            .into_iter()
            .map(|session| {
                let lane = session.effective_history_lane();
                (session.ui_thread_id, lane)
            })
            .collect())
    }

    async fn list_sessions_page(
        &self,
        principal: &str,
        workspace: &str,
        query: ChatSessionPageQuery,
    ) -> Result<ChatSessionPage> {
        let mut sessions = self.list_sessions(principal, workspace).await?;
        sessions.retain(|session| query.matches(session));
        let total = sessions.len();
        let sessions = sessions
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .collect();
        Ok(ChatSessionPage {
            sessions,
            total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    /// Return lightweight search hits without loading every session document.
    /// File-backed stores override this with their in-memory index; the
    /// fallback preserves compatibility for alternate/test stores.
    async fn search_session_candidates(
        &self,
        principal: &str,
        workspace: &str,
        search: &str,
    ) -> Result<Vec<ChatSessionSearchCandidate>> {
        let query = ChatSessionPageQuery {
            ui_thread_id: None,
            history_lane: None,
            search: search.to_string(),
            limit: usize::MAX,
            offset: 0,
        };
        let mut candidates = self
            .list_sessions(principal, workspace)
            .await?
            .into_iter()
            .filter(|session| query.matches(session))
            .map(|session| ChatSessionSearchCandidate {
                id: session.id,
                updated_at: session.updated_at,
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(candidates)
    }

    /// List sessions whose `ui_thread_id` starts with the given prefix
    /// (e.g. `meeting-`), ordered by `updated_at` desc. Index-backed: only
    /// the matching sessions' documents are loaded.
    async fn list_sessions_for_thread_prefix(
        &self,
        principal: &str,
        workspace: &str,
        thread_prefix: &str,
    ) -> Result<Vec<ChatSession>>;

    /// Lightweight thread listing: one summary per session whose thread starts
    /// with the prefix, newest first over a total `(updated_at, session_id)`
    /// order.
    ///
    /// File-backed stores answer this from their in-memory index and open no
    /// session document. The default loads documents so alternate stores stay
    /// correct; any caller that polls should prefer a store that overrides it.
    async fn list_thread_summaries_for_prefix(
        &self,
        principal: &str,
        workspace: &str,
        thread_prefix: &str,
    ) -> Result<Vec<ChatThreadSessionSummary>> {
        let mut summaries = self
            .list_sessions_for_thread_prefix(principal, workspace, thread_prefix)
            .await?
            .into_iter()
            .map(|session| ChatThreadSessionSummary {
                ui_thread_id: session.ui_thread_id,
                session_id: session.id,
                status: session.status,
                title: session.title,
                agent_id: session.agent_id,
                created_at: session.created_at,
                updated_at: session.updated_at,
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.session_id.cmp(&left.session_id))
        });
        Ok(summaries)
    }

    /// List active sessions currently bound to the given agent persona.
    async fn list_active_sessions_for_agent(&self, agent_id: &str) -> Result<Vec<ChatSession>>;

    /// Update the title of a session.
    async fn update_session_title(&self, session_id: &str, title: &str) -> Result<()>;

    /// Update the status of a session (e.g., "active" → "archived").
    async fn update_session_status(&self, session_id: &str, status: &str) -> Result<()>;

    /// Update the thread assignment of a session (for switching threads).
    async fn update_session_thread(&self, session_id: &str, ui_thread_id: &str) -> Result<()>;

    /// Permanently delete a session and all its messages.
    async fn delete_session(&self, session_id: &str) -> Result<()>;

    /// Append a message to a session.
    async fn append_message(&self, session_id: &str, msg: ChatMessage) -> Result<()>;

    /// Delete a single display message from a session.
    async fn delete_message(&self, session_id: &str, message_id: &str) -> Result<bool>;

    /// Delete every display message in the session, leaving the session itself
    /// intact (title, thread assignment, etc. are preserved). Returns the
    /// number of messages cleared.
    async fn clear_messages(&self, session_id: &str) -> Result<usize>;

    /// Atomically discard a server-attested screen capture only when no
    /// persisted message references it. The implementation owns the session
    /// lifecycle fence so the reference check and durable cleanup cannot race.
    async fn discard_unreferenced_screen_capture_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<bool>;

    /// Get messages for a session, most recent `limit` messages.
    async fn get_messages(&self, session_id: &str, limit: usize) -> Result<Vec<ChatMessage>>;

    /// Read conversational messages, excluding cross-session display copies
    /// before applying the limit so a busy notification inbox cannot evict the
    /// intended conversation. Exact cursors fail closed if history changes.
    async fn get_context_messages(&self, session_id: &str, limit: usize) -> Result<Vec<ChatMessage>> {
        let mut newest_first = Vec::with_capacity(limit.min(128));
        let mut cursor: Option<String> = None;
        while newest_first.len() < limit {
            let (page, more) = self
                .get_messages_before_exact(session_id, limit.clamp(32, 128), cursor.as_deref())
                .await?
                .ok_or_else(|| anyhow!("chat context cursor disappeared"))?;
            let next = page.first().map(|message| message.id.clone());
            for message in page
                .into_iter()
                .rev()
                .filter(|message| !message.is_context_projection())
            {
                newest_first.push(message);
                if newest_first.len() == limit {
                    break;
                }
            }
            if !more {
                break;
            }
            if next.is_none() || next == cursor {
                return Err(anyhow!("chat context cursor did not advance"));
            }
            cursor = next;
        }
        newest_first.reverse();
        Ok(newest_first)
    }

    /// Get paginated messages. Returns up to `limit` messages BEFORE the message
    /// with ID `before_id`. If `before_id` is None, returns the most recent messages.
    /// Returns (messages, has_more) where has_more indicates older messages exist.
    ///
    /// An unknown `before_id` is FORGIVEN: the newest page is returned instead.
    /// That is right for the interactive readers this serves and wrong for a
    /// programmatic pager — use [`ChatStore::get_messages_before_exact`] there.
    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        before_id: Option<&str>,
    ) -> Result<(Vec<ChatMessage>, bool)>;

    /// [`ChatStore::get_messages_paginated`] that fails closed on an unknown
    /// cursor: `Ok(None)` means `before_id` names no message in this session.
    ///
    /// A pager that silently receives the newest page after asking to advance
    /// past an unknown cursor loops back to page one forever, so every
    /// non-interactive consumer — the `meetings_data` transcript read — must
    /// take this variant. The default implementation resolves the cursor inside
    /// a bounded newest-first window; file-backed stores override it with an
    /// exact segment walk that reads only the page it returns.
    async fn get_messages_before_exact(
        &self,
        session_id: &str,
        limit: usize,
        before_id: Option<&str>,
    ) -> Result<Option<(Vec<ChatMessage>, bool)>> {
        let Some(before_id) = before_id else {
            return Ok(Some(
                self.get_messages_paginated(session_id, limit, None).await?,
            ));
        };
        let window = self
            .get_messages(session_id, EXACT_CURSOR_DEFAULT_SCAN_WINDOW)
            .await?;
        let truncated_window = window.len() >= EXACT_CURSOR_DEFAULT_SCAN_WINDOW;
        let Some(position) = window.iter().position(|message| message.id == before_id) else {
            // `Ok(None)` means "no such message". A cursor that merely fell off
            // the back of this bounded window is a DIFFERENT condition, and
            // reporting it as absent would tell a pager its own valid cursor
            // had vanished mid-thread. Stores that need deep history override
            // this method with an exact walk.
            if truncated_window {
                return Err(anyhow!(
                    "chat cursor `{before_id}` is older than the {EXACT_CURSOR_DEFAULT_SCAN_WINDOW}-message \
                     window this store resolves exact cursors in"
                ));
            }
            return Ok(None);
        };
        let older = &window[..position];
        let start = older.len().saturating_sub(limit);
        let page = older[start..].to_vec();
        // Never claim more history exists while returning no cursor to reach it
        // — an empty page ends the walk for this store. But a cursor resolved
        // near the FRONT of a truncated window has plenty older behind it, and
        // reporting `false` there would stop a pager mid-thread.
        let has_older = !page.is_empty() && (start > 0 || truncated_window);
        Ok(Some((page, has_older)))
    }

    /// Append canonical LLM transcript entries to a session.
    async fn append_llm_history_entries(
        &self,
        session_id: &str,
        entries: Vec<ChatLlmTranscriptEntry>,
    ) -> Result<()>;

    /// Remove a just-persisted user turn when later chat-side side effects fail.
    async fn rollback_user_turn(&self, session_id: &str, message_ids: &[String]) -> Result<()>;

    /// Load canonical LLM transcript history for a session.
    ///
    /// Older sessions may not yet have structured history persisted. In that
    /// case the store returns a best-effort synthesized transcript.
    async fn get_llm_history(&self, session_id: &str) -> Result<Vec<ChatLlmTranscriptEntry>>;

    /// Load only the required transcript tail when a consumer does not need a
    /// full stateless-provider replay. Alternate stores retain compatibility
    /// through the bounded-after-load default; the file store overrides this.
    async fn get_llm_history_tail(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<ChatLlmTranscriptEntry>> {
        let history = self.get_llm_history(session_id).await?;
        let start = history.len().saturating_sub(limit);
        Ok(history.into_iter().skip(start).collect())
    }
}

#[derive(Debug, Clone)]
pub struct ChatSessionPageQuery {
    pub ui_thread_id: Option<String>,
    pub history_lane: Option<HistoryLane>,
    pub search: String,
    pub limit: usize,
    pub offset: usize,
}

impl ChatSessionPageQuery {
    fn matches(&self, session: &ChatSession) -> bool {
        if self
            .ui_thread_id
            .as_deref()
            .is_some_and(|thread_id| session.ui_thread_id != thread_id)
        {
            return false;
        }
        if self
            .history_lane
            .is_some_and(|lane| session.effective_history_lane() != lane)
        {
            return false;
        }
        let search = self.search.trim().to_ascii_lowercase();
        search.is_empty()
            || session.id.to_ascii_lowercase().contains(&search)
            || session.ui_thread_id.to_ascii_lowercase().contains(&search)
            || session.agent_id.to_ascii_lowercase().contains(&search)
            || session
                .title
                .as_deref()
                .is_some_and(|title| title.to_ascii_lowercase().contains(&search))
    }
}

#[derive(Debug, Clone)]
pub struct ChatSessionPage {
    pub sessions: Vec<ChatSession>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

/// One session's index-resident metadata for a thread listing. Deliberately not
/// a `ChatSession`: it carries only what an index can answer, so a caller
/// cannot mistake it for a loaded document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatThreadSessionSummary {
    pub ui_thread_id: String,
    pub session_id: String,
    pub status: ChatSessionStatus,
    pub title: Option<String>,
    pub agent_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSessionSearchCandidate {
    pub id: String,
    pub updated_at: i64,
}

#[async_trait]
pub trait ChatSessionArchiveObserver: Send + Sync {
    /// Physical deletion invokes this while the deleted generation's external
    /// lifecycle fence is still held. Implementations must not synchronously
    /// recreate the same session id; identity-keyed cleanup must finish before
    /// a replacement generation can be activated.
    async fn on_session_archived(&self, session_id: &str) -> Result<()>;

    async fn on_session_activated(&self, _session: &ChatSession) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// In-memory scope index
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Eq)]
struct SessionScope {
    principal: String,
    workspace: String,
    ui_thread_id: String,
}

impl SessionScope {
    fn new(principal: &str, workspace: &str, ui_thread_id: &str) -> Self {
        Self {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            ui_thread_id: ui_thread_id.to_string(),
        }
    }
}

impl PartialEq for SessionScope {
    fn eq(&self, other: &Self) -> bool {
        self.principal == other.principal
            && self.workspace == other.workspace
            && self.ui_thread_id == other.ui_thread_id
    }
}

impl Hash for SessionScope {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.principal.hash(state);
        self.workspace.hash(state);
        self.ui_thread_id.hash(state);
    }
}

/// Tracks which sessions belong to which principal/workspace/thread scope, and which is active.
struct PrincipalIndex {
    /// (principal, workspace, ui_thread_id) -> list of session_ids
    sessions: DashMap<SessionScope, Vec<SessionIndexEntry>>,
}

#[derive(Debug, Clone)]
struct SessionIndexEntry {
    session_id: String,
    status: ChatSessionStatus,
    created_at: i64,
    updated_at: i64,
    title: Option<String>,
    agent_id: String,
    history_lane: HistoryLane,
    is_default_session: bool,
    is_concurrent: bool,
}

impl PrincipalIndex {
    fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    fn clear(&self) {
        self.sessions.clear();
    }

    fn default_session_id(&self, principal: &str, workspace: &str) -> Option<String> {
        self.sessions
            .get(&SessionScope::new(principal, workspace, "general"))
            .and_then(|entries| {
                entries
                    .iter()
                    .filter(|entry| !entry.is_concurrent)
                    .min_by(|left, right| {
                        left.created_at
                            .cmp(&right.created_at)
                            .then_with(|| right.is_default_session.cmp(&left.is_default_session))
                            .then_with(|| left.session_id.cmp(&right.session_id))
                    })
                    .map(|entry| entry.session_id.clone())
            })
    }

    fn is_default_session(&self, session: &ChatSession) -> bool {
        session.ui_thread_id == "general"
            && self
                .default_session_id(&session.principal, &session.workspace)
                .is_some_and(|session_id| session_id == session.id)
    }

    fn default_session_ids(&self) -> Vec<String> {
        self.sessions
            .iter()
            .filter(|entry| entry.key().ui_thread_id == "general")
            .filter_map(|entry| {
                let sessions = entry.value();
                sessions
                    .iter()
                    .filter(|entry| !entry.is_concurrent)
                    .min_by(|left, right| {
                        left.created_at
                            .cmp(&right.created_at)
                            .then_with(|| right.is_default_session.cmp(&left.is_default_session))
                            .then_with(|| left.session_id.cmp(&right.session_id))
                    })
                    .map(|session| session.session_id.clone())
            })
            .collect()
    }

    fn normalize_default_runtime_state(&self) {
        let default_ids = self
            .default_session_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        for mut scope in self.sessions.iter_mut() {
            for entry in scope.value_mut() {
                let is_default = default_ids.contains(&entry.session_id);
                entry.is_default_session = is_default;
                if is_default {
                    entry.status = ChatSessionStatus::Active;
                    entry.history_lane = HistoryLane::Personal;
                }
            }
        }
    }

    fn is_default_session_id(&self, session_id: &str) -> bool {
        self.default_session_ids()
            .iter()
            .any(|candidate| candidate == session_id)
    }

    fn insert(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        entry: SessionIndexEntry,
    ) {
        let mut entries = self
            .sessions
            .entry(SessionScope::new(principal, workspace, ui_thread_id))
            .or_default();
        if let Some(existing) = entries
            .iter_mut()
            .find(|e| e.session_id == entry.session_id)
        {
            *existing = entry;
        } else {
            entries.push(entry);
        }
    }

    fn active_session_ids(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
    ) -> Vec<String> {
        self.sessions
            .get(&SessionScope::new(principal, workspace, ui_thread_id))
            .map(|entries| {
                let mut active = entries
                    .iter()
                    .filter(|e| e.status == ChatSessionStatus::Active && !e.is_concurrent)
                    .cloned()
                    .collect::<Vec<_>>();
                active.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                active
                    .into_iter()
                    .map(|entry| entry.session_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    /// Every meeting thread in this scope, with the last time anything
    /// happened in it.
    ///
    /// Read from the index rather than from a second store on purpose: "is
    /// this gathering still alive" and "when did this session last change"
    /// must be one fact. A parallel liveness log would drift, and the
    /// direction it drifts — a stale log reading fresher than the sessions —
    /// is the one that merges two gatherings.
    fn meeting_thread_candidates(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<crate::magician_v2::media_seam::MeetingThreadCandidate> {
        use crate::magician_v2::media_seam::{is_meeting_thread, MeetingThreadCandidate};

        self.sessions
            .iter()
            .filter(|entry| {
                entry.key().principal == principal
                    && entry.key().workspace == workspace
                    && is_meeting_thread(&entry.key().ui_thread_id)
            })
            .filter_map(|entry| {
                let latest = entry.value().iter().map(|row| row.updated_at).max()?;
                Some(MeetingThreadCandidate {
                    thread: entry.key().ui_thread_id.clone(),
                    last_activity: DateTime::<Utc>::from_timestamp_millis(latest)?,
                })
            })
            .collect()
    }

    /// Archived sessions this agent left behind on a thread, newest first.
    ///
    /// Only this agent's: a room's predecessor is the room's own prior
    /// session, and inheriting some other agent's archived history would mix
    /// provenance exactly the way `get_or_create_active_session`'s exact-agent
    /// match exists to prevent.
    fn archived_sessions_for_agent(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        agent_id: &str,
    ) -> Vec<SessionIndexEntry> {
        self.sessions
            .get(&SessionScope::new(principal, workspace, ui_thread_id))
            .map(|entries| {
                let mut archived = entries
                    .iter()
                    .filter(|entry| {
                        entry.status == ChatSessionStatus::Archived && entry.agent_id == agent_id
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                archived.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                archived
            })
            .unwrap_or_default()
    }

    fn update_timestamp(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        session_id: &str,
        updated_at: i64,
    ) {
        if let Some(mut entries) =
            self.sessions
                .get_mut(&SessionScope::new(principal, workspace, ui_thread_id))
        {
            if let Some(entry) = entries.iter_mut().find(|e| e.session_id == session_id) {
                entry.updated_at = updated_at;
            }
        }
    }

    fn update_title(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        session_id: &str,
        title: Option<String>,
        updated_at: i64,
    ) {
        if let Some(mut entries) =
            self.sessions
                .get_mut(&SessionScope::new(principal, workspace, ui_thread_id))
        {
            if let Some(entry) = entries.iter_mut().find(|e| e.session_id == session_id) {
                entry.title = title;
                entry.updated_at = updated_at;
            }
        }
    }

    fn update_status(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        session_id: &str,
        status: ChatSessionStatus,
        updated_at: i64,
    ) {
        if let Some(mut entries) =
            self.sessions
                .get_mut(&SessionScope::new(principal, workspace, ui_thread_id))
        {
            if let Some(entry) = entries.iter_mut().find(|e| e.session_id == session_id) {
                entry.status = status;
                entry.updated_at = updated_at;
            }
        }
    }

    fn remove_session(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        session_id: &str,
    ) {
        if let Some(mut entries) =
            self.sessions
                .get_mut(&SessionScope::new(principal, workspace, ui_thread_id))
        {
            entries.retain(|e| e.session_id != session_id);
        }
    }

    fn remove_session_id(&self, session_id: &str) {
        for mut entries in self.sessions.iter_mut() {
            entries.retain(|entry| entry.session_id != session_id);
        }
    }

    fn list_sorted(&self, principal: &str, workspace: &str) -> Vec<String> {
        self.list_sorted_with_thread_prefix(principal, workspace, None)
    }

    fn quiescent_session_ids_sorted(&self, updated_before: i64) -> Vec<String> {
        let mut newest_by_id = HashMap::<String, i64>::new();
        for scope in self.sessions.iter() {
            for entry in scope.value() {
                newest_by_id
                    .entry(entry.session_id.clone())
                    .and_modify(|updated_at| *updated_at = (*updated_at).max(entry.updated_at))
                    .or_insert(entry.updated_at);
            }
        }
        let mut ids = newest_by_id
            .into_iter()
            .filter_map(|(session_id, updated_at)| {
                (updated_at <= updated_before).then_some(session_id)
            })
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    fn list_page(
        &self,
        principal: &str,
        workspace: &str,
        query: &ChatSessionPageQuery,
    ) -> (Vec<String>, usize) {
        let search = query.search.trim().to_ascii_lowercase();
        let mut entries = self
            .sessions
            .iter()
            .filter(|entry| {
                let scope = entry.key();
                scope.principal == principal
                    && scope.workspace == workspace
                    && query
                        .ui_thread_id
                        .as_deref()
                        .is_none_or(|thread_id| scope.ui_thread_id == thread_id)
            })
            .flat_map(|entry| {
                let thread_id = entry.key().ui_thread_id.clone();
                entry
                    .value()
                    .clone()
                    .into_iter()
                    .map(move |record| (thread_id.clone(), record))
            })
            .filter(|(thread_id, entry)| {
                query
                    .history_lane
                    .is_none_or(|lane| entry.history_lane == lane)
                    && (search.is_empty()
                        || entry.session_id.to_ascii_lowercase().contains(&search)
                        || thread_id.to_ascii_lowercase().contains(&search)
                        || entry.agent_id.to_ascii_lowercase().contains(&search)
                        || entry
                            .title
                            .as_deref()
                            .is_some_and(|title| title.to_ascii_lowercase().contains(&search)))
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .1
                .updated_at
                .cmp(&left.1.updated_at)
                .then_with(|| left.1.session_id.cmp(&right.1.session_id))
        });
        entries.dedup_by(|left, right| left.1.session_id == right.1.session_id);
        let total = entries.len();
        let ids = entries
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .map(|(_, entry)| entry.session_id)
            .collect();
        (ids, total)
    }

    fn search_candidates(
        &self,
        principal: &str,
        workspace: &str,
        search: &str,
    ) -> Vec<ChatSessionSearchCandidate> {
        let search = search.trim().to_ascii_lowercase();
        let mut deduped = HashMap::<String, ChatSessionSearchCandidate>::new();
        for entry in self.sessions.iter().filter(|entry| {
            let scope = entry.key();
            scope.principal == principal && scope.workspace == workspace
        }) {
            let thread_id = &entry.key().ui_thread_id;
            for record in entry.value().iter().filter(|record| {
                record.session_id.to_ascii_lowercase().contains(&search)
                    || thread_id.to_ascii_lowercase().contains(&search)
                    || record.agent_id.to_ascii_lowercase().contains(&search)
                    || record
                        .title
                        .as_deref()
                        .is_some_and(|title| title.to_ascii_lowercase().contains(&search))
            }) {
                let candidate = ChatSessionSearchCandidate {
                    id: record.session_id.clone(),
                    updated_at: record.updated_at,
                };
                deduped
                    .entry(candidate.id.clone())
                    .and_modify(|existing| {
                        if candidate.updated_at > existing.updated_at {
                            *existing = candidate.clone();
                        }
                    })
                    .or_insert(candidate);
            }
        }
        let mut candidates = deduped.into_values().collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        candidates
    }

    /// Index-only summaries for one thread prefix: the fields a thread listing
    /// needs, without opening a single session document.
    ///
    /// `list_sorted_with_thread_prefix` returns ids, and its callers then load
    /// every named document. A surface that polls a dated thread family every
    /// few seconds would re-read and re-parse the scope's entire meeting
    /// history on each poll, growing monotonically with the calendar.
    fn thread_summaries_with_prefix(
        &self,
        principal: &str,
        workspace: &str,
        thread_prefix: &str,
    ) -> Vec<ChatThreadSessionSummary> {
        let mut summaries = self
            .sessions
            .iter()
            .filter(|entry| {
                let scope = entry.key();
                scope.principal == principal
                    && scope.workspace == workspace
                    && scope.ui_thread_id.starts_with(thread_prefix)
            })
            .flat_map(|entry| {
                let ui_thread_id = entry.key().ui_thread_id.clone();
                entry
                    .value()
                    .clone()
                    .into_iter()
                    .map(move |row| ChatThreadSessionSummary {
                        ui_thread_id: ui_thread_id.clone(),
                        session_id: row.session_id,
                        status: row.status,
                        title: row.title,
                        agent_id: row.agent_id,
                        created_at: row.created_at,
                        updated_at: row.updated_at,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        // Deduplicate a session that appears under more than one index entry,
        // keeping the freshest, then impose a total order.
        summaries.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        summaries.dedup_by(|left, right| left.session_id == right.session_id);
        summaries.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.session_id.cmp(&left.session_id))
        });
        summaries
    }

    /// Like `list_sorted`, but restricted to scopes whose `ui_thread_id`
    /// starts with the given prefix. Filters on index keys only — no session
    /// documents are touched here.
    fn list_sorted_with_thread_prefix(
        &self,
        principal: &str,
        workspace: &str,
        thread_prefix: Option<&str>,
    ) -> Vec<String> {
        let mut deduped: HashMap<String, SessionIndexEntry> = self
            .sessions
            .iter()
            .filter(|entry| {
                let scope = entry.key();
                scope.principal == principal
                    && scope.workspace == workspace
                    && thread_prefix.map_or(true, |p| scope.ui_thread_id.starts_with(p))
            })
            .flat_map(|entry| entry.value().clone().into_iter())
            .fold(HashMap::new(), |mut acc, entry| {
                match acc.get_mut(&entry.session_id) {
                    Some(existing) if entry.updated_at > existing.updated_at => {
                        *existing = entry;
                    },
                    Some(_) => {},
                    None => {
                        acc.insert(entry.session_id.clone(), entry);
                    },
                }
                acc
            });
        let mut combined = deduped.drain().map(|(_, entry)| entry).collect::<Vec<_>>();
        // `updated_at` alone is not a total order and the input arrives from a
        // DashMap drain, so ties would resolve differently between two calls.
        // The session id breaks them.
        combined.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.session_id.cmp(&a.session_id))
        });
        combined.into_iter().map(|e| e.session_id).collect()
    }

    /// One `(ui_thread_id, effective history lane)` per session in the scope,
    /// answered entirely from the index.
    ///
    /// Every input `ChatSession::effective_history_lane` needs is already here:
    /// `ui_thread_id` is part of the scope key, and `title` / `history_lane`
    /// live on the entry, so the legacy-lane inference reproduces exactly what
    /// loading the document would have produced. Hydration normalizes each
    /// entry's lane before inserting it and `normalize_default_runtime_state`
    /// applies the same default-session forcing `decorate_session` applies on
    /// load, so index and document agree.
    ///
    /// Exists so `UiThreadService::sync_scope` can learn which threads sessions
    /// reference without taking a session lock and reading a document per
    /// session for two fields.
    fn session_thread_lanes(&self, principal: &str, workspace: &str) -> Vec<(String, HistoryLane)> {
        let mut lanes = Vec::new();
        for scope in self.sessions.iter() {
            let key = scope.key();
            if key.principal != principal || key.workspace != workspace {
                continue;
            }
            for entry in scope.value() {
                let lane = match entry.history_lane {
                    HistoryLane::Legacy => {
                        infer_legacy_session_history_lane(&key.ui_thread_id, entry.title.as_deref())
                    },
                    lane => lane,
                };
                lanes.push((key.ui_thread_id.clone(), lane));
            }
        }
        lanes
    }
}

// ---------------------------------------------------------------------------
// Process-wide reader slot
// ---------------------------------------------------------------------------

/// The one chat store the running server installed at boot, published so
/// workspace-coupled READ owners that are constructed without the server's
/// wiring can reach the same index instead of building a second one.
///
/// Constructing a second `FileChatStore` over the same layout is correct but
/// expensive: each instance rebuilds the principal index from disk and then
/// carries its own copy. Compiled read providers are rebuilt per scope, so the
/// second-store path would re-scan the chat root on every registry build.
///
/// Write access is deliberately NOT offered through this slot. It is a read
/// convenience for host binders; every mutation keeps going through the owner
/// that already holds the store.
static GLOBAL_CHAT_STORE: OnceLock<Arc<dyn ChatStore>> = OnceLock::new();

/// Publish the process's chat store. Returns `false` when a store was already
/// published — the first publication wins, so a late or duplicate boot path can
/// never swap the store out from under a live reader.
pub fn publish_global_chat_store(store: Arc<dyn ChatStore>) -> bool {
    GLOBAL_CHAT_STORE.set(store).is_ok()
}

/// The published chat store, or `None` in a process that never published one
/// (tests, control-plane boots). Readers must fail closed on `None` rather than
/// substituting a store of their own.
pub fn global_chat_store() -> Option<Arc<dyn ChatStore>> {
    GLOBAL_CHAT_STORE.get().cloned()
}

// ---------------------------------------------------------------------------
// FileChatStore
// ---------------------------------------------------------------------------

/// File-based implementation of `ChatStore`.
pub struct FileChatStore {
    workspace_layout: ArtifactV2Workspace,
    session_locks: DashMap<String, Arc<Mutex<()>>>,
    /// Per-scope locks to prevent concurrent session creation races.
    scope_locks: DashMap<SessionScope, Arc<Mutex<()>>>,
    index: PrincipalIndex,
    session_locations: DashMap<String, SessionLocation>,
    archive_observer: StdRwLock<Option<Arc<dyn ChatSessionArchiveObserver>>>,
    /// Maintenance never queues twice behind a live compaction of the same
    /// session. The session mutation lock still provides the authoritative
    /// generation switch; this set is only bounded backpressure/deduplication.
    transcript_compactions_inflight: DashMap<String, ()>,
    transcript_compaction_cursor: AtomicUsize,
}

struct TranscriptCompactionLease<'a> {
    inflight: &'a DashMap<String, ()>,
    session_id: String,
}

impl Drop for TranscriptCompactionLease<'_> {
    fn drop(&mut self) {
        self.inflight.remove(&self.session_id);
    }
}

#[derive(Clone)]
struct SessionLocation {
    principal: String,
    workspace: String,
}

impl FileChatStore {
    /// Create a new `FileChatStore` at the given base path.
    pub fn new<P: AsRef<Path>>(base_path: P) -> Self {
        Self::with_workspace_layout(ArtifactV2Workspace::new(base_path.as_ref()))
    }

    /// Create a new `FileChatStore` over an already-resolved workspace layout.
    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            session_locks: DashMap::new(),
            scope_locks: DashMap::new(),
            index: PrincipalIndex::new(),
            session_locations: DashMap::new(),
            archive_observer: StdRwLock::new(None),
            transcript_compactions_inflight: DashMap::new(),
            transcript_compaction_cursor: AtomicUsize::new(0),
        }
    }

    /// Create a new `FileChatStore` and build the index from disk.
    pub async fn with_index<P: AsRef<Path>>(base_path: P) -> Result<Self> {
        let store = Self::new(base_path);
        store.initialize().await?;
        Ok(store)
    }

    /// Create a new `FileChatStore` over an already-resolved workspace layout
    /// and build the index from the provider-backed workspace.
    pub async fn with_workspace_layout_index(
        workspace_layout: ArtifactV2Workspace,
    ) -> Result<Self> {
        let store = Self::with_workspace_layout(workspace_layout);
        store.initialize().await?;
        Ok(store)
    }

    /// Initialize the in-memory index by scanning session files on disk.
    pub async fn initialize(&self) -> Result<()> {
        self.workspace_layout.ensure_root().await?;
        self.index.clear();
        self.session_locations.clear();
        let mut count = 0usize;
        let scopes = self.workspace_layout.list_scope_segments().await?;
        if scopes.is_empty() {
            debug!("[CHAT-STORE] No scoped chat session directories found, starting fresh");
            return Ok(());
        }

        for (principal, workspace) in scopes {
            let sessions_dir = self
                .workspace_layout
                .chat_sessions_dir(&principal, &workspace);
            if self
                .workspace_layout
                .metadata_path(&sessions_dir)
                .await?
                .is_none()
            {
                continue;
            }
            let entries = self.workspace_layout.read_dir_path(&sessions_dir).await?;
            for entry in entries {
                if entry.is_dir && entry.file_name == ".lifecycle" {
                    continue;
                }
                let path = sessions_dir.join(&entry.file_name);
                let document_path = if entry.is_dir {
                    path.join("session.json")
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    path.clone()
                } else {
                    continue;
                };

                match self
                    .read_session_metadata_projection(document_path.clone())
                    .await
                {
                    Ok(mut projection) => {
                        projection.session.normalize_history_lane();
                        self.session_locations.insert(
                            projection.session.id.clone(),
                            SessionLocation {
                                principal: projection.session.principal.clone(),
                                workspace: projection.session.workspace.clone(),
                            },
                        );
                        if matches!(projection.session.internal_voice, Some(InternalVoiceSession::Coordinator { .. })) {
                            count += 1;
                            continue;
                        }
                        self.index.insert(
                            &projection.session.principal,
                            &projection.session.workspace,
                            &projection.session.ui_thread_id,
                            SessionIndexEntry {
                                session_id: projection.session.id.clone(),
                                status: projection.session.status.clone(),
                                created_at: projection.session.created_at,
                                updated_at: projection.session.updated_at,
                                title: projection.session.title.clone(),
                                agent_id: projection.session.agent_id.clone(),
                                history_lane: projection.session.history_lane,
                                is_default_session: projection.session.is_default_session,
                                is_concurrent: matches!(projection.session.internal_voice, Some(InternalVoiceSession::Branch { .. })),
                            },
                        );
                        count += 1;
                    },
                    Err(e) => {
                        warn!(
                            "[CHAT-STORE] Failed to read session metadata {:?}: {}",
                            document_path, e
                        );
                    },
                }
            }
        }

        self.normalize_default_sessions().await?;

        info!(
            "[CHAT-STORE] Initialized index with {} chat sessions",
            count
        );
        Ok(())
    }

    async fn read_session_metadata_projection(
        &self,
        path: PathBuf,
    ) -> Result<ChatSessionMetadataProjection> {
        let metadata = self
            .workspace_layout
            .metadata_path(&path)
            .await?
            .ok_or_else(|| anyhow!("chat session metadata file disappeared"))?;
        if metadata.len() > CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES {
            return Err(anyhow!(
                "chat session document exceeds the legacy migration byte limit"
            ));
        }
        if metadata.len() <= CHAT_SESSION_METADATA_FAST_PATH_BYTES {
            let bytes = self
                .workspace_layout
                .read_prefix_path(&path, metadata.len().saturating_add(1))
                .await?;
            let projection: ChatSessionMetadataProjection =
                deserialize_guarded_chat_bytes(&bytes, CHAT_SESSION_METADATA_FAST_PATH_BYTES)?;
            validate_session_metadata_projection(&projection)?;
            return Ok(projection);
        }

        // Legacy session documents may embed multi-megabyte message/history
        // arrays. Parse only the `session` projection from a reader on the
        // blocking pool so startup neither allocates the monolith nor performs
        // blocking disk I/O on an async runtime worker.
        tokio::task::spawn_blocking(move || -> Result<ChatSessionMetadataProjection> {
            let projection: ChatSessionMetadataProjection =
                read_guarded_chat_json_file(&path, CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES)?;
            validate_session_metadata_projection(&projection)?;
            if projection.format_version != 1 {
                return Err(anyhow!(
                    "oversized or unsupported metadata-only chat session document"
                ));
            }
            Ok(projection)
        })
        .await
        .map_err(|error| anyhow!("session metadata projection worker failed: {error}"))?
    }

    pub fn set_archive_observer(&self, observer: Arc<dyn ChatSessionArchiveObserver>) {
        *self
            .archive_observer
            .write()
            .expect("chat archive observer lock poisoned") = Some(observer);
    }

    fn archive_observer(&self) -> Option<Arc<dyn ChatSessionArchiveObserver>> {
        self.archive_observer
            .read()
            .expect("chat archive observer lock poisoned")
            .clone()
    }

    /// Resolve the session a meeting bot should rejoin into, and make sure it
    /// is not empty when the room's own transcript still exists somewhere.
    ///
    /// `get_or_create_active_session` is correct for every other caller and
    /// wrong for exactly this one, in two ways that only bite a room:
    ///
    /// 1. **The calendar splits a meeting.** The thread id carries the date, so
    ///    a bot that drops at 23:58 asks for a different thread at 00:02 and
    ///    lands in an empty room. Handled by
    ///    [`decide_meeting_continuity`](crate::magician_v2::media_seam::MeetingThreadCandidate),
    ///    which continues the live occurrence of the SAME meeting and never
    ///    reaches a different one.
    /// 2. **Archival strands the transcript.** The transcript endpoint refuses
    ///    archived sessions on purpose — that refusal is what makes the sink
    ///    re-resolve to the thread's current active session — so a session
    ///    archived between drop and rejoin leaves everything that was said in
    ///    it, and the rejoin opens a blank one beside it. Handled by carrying
    ///    the predecessor's transcript FORWARD into the new session.
    ///
    /// Carrying forward rather than un-archiving is deliberate: archiving is a
    /// terminal state and reviving one would silently undo whoever ended the
    /// session, and would also re-open a session the transcript endpoint has
    /// already told the sink to stop writing to. Copying leaves the archived
    /// record exactly as it was.
    ///
    /// Bounded by [`MEETING_CARRY_FORWARD_MESSAGES`] — a room needs the recent
    /// conversation, not an unbounded replay of a nine-hour meeting into a
    /// second copy on disk.
    ///
    /// Idempotent: the carry-forward fires only into a session that holds no
    /// messages, so a second resolve — a second drop, a retry — finds history
    /// and copies nothing. Nothing here mutates the predecessor.
    pub async fn resolve_meeting_room_session(
        &self,
        principal: &str,
        workspace: &str,
        requested_thread: &str,
        origin: &ChatChannel,
        agent_id: &str,
        now: DateTime<Utc>,
    ) -> Result<MeetingRoomSession> {
        use crate::magician_v2::media_seam::{decide_meeting_continuity, REJOIN_GRACE};

        let candidates = self.index.meeting_thread_candidates(principal, workspace);
        let continuity =
            decide_meeting_continuity(requested_thread, &candidates, now, REJOIN_GRACE);
        let thread = continuity.thread(requested_thread).to_string();
        let continued_from_thread = (thread != requested_thread).then(|| thread.clone());

        let session = self
            .get_or_create_active_session(principal, workspace, &thread, origin, agent_id)
            .await?;

        // Empty is the discriminator, not "was just created": it makes the
        // copy idempotent and it also covers the case where a previous resolve
        // created the session and died before copying anything into it.
        let existing = self
            .get_messages(&session.id, 1)
            .await
            .context("reading the rejoined room's history")?;
        if !existing.is_empty() {
            return Ok(MeetingRoomSession {
                session,
                thread,
                continued_from_thread,
                carried_from_session: None,
                carried_message_count: 0,
            });
        }

        let Some(predecessor) = self
            .index
            .archived_sessions_for_agent(principal, workspace, &thread, agent_id)
            .into_iter()
            .next()
        else {
            return Ok(MeetingRoomSession {
                session,
                thread,
                continued_from_thread,
                carried_from_session: None,
                carried_message_count: 0,
            });
        };

        let carried = self
            .get_messages(&predecessor.session_id, MEETING_CARRY_FORWARD_MESSAGES)
            .await
            .context("reading the archived predecessor's transcript")?;
        let mut carried_message_count = 0usize;
        for message in carried {
            let mut copy = message.clone();
            copy.id = Uuid::new_v4().to_string();
            copy.session_id = session.id.clone();
            self.append_message(&session.id, copy)
                .await
                .context("carrying an archived room's transcript into its rejoin")?;
            carried_message_count += 1;
        }
        info!(
            "[CHAT-STORE] carried {} transcript message(s) from archived session {} into rejoined \
             meeting session {}",
            carried_message_count, predecessor.session_id, session.id
        );

        Ok(MeetingRoomSession {
            session,
            thread,
            continued_from_thread,
            carried_from_session: Some(predecessor.session_id),
            carried_message_count,
        })
    }

    fn decorate_session(&self, mut session: ChatSession) -> ChatSession {
        session.normalize_history_lane();
        session.is_default_session = session.internal_voice.is_none() && self.index.is_default_session(&session);
        if session.is_default_session {
            session.status = ChatSessionStatus::Active;
            session.history_lane = HistoryLane::Personal;
        }
        session
    }

    async fn normalize_default_sessions(&self) -> Result<()> {
        // Startup normalization is index-only. Loading and rewriting a legacy
        // default session here used to materialize its entire inline transcript.
        // `load_document` applies the same projection lazily and the next real
        // mutation persists it without changing observable session semantics.
        self.index.normalize_default_runtime_state();
        Ok(())
    }

    async fn persist_session_status(
        &self,
        session_id: &str,
        new_status: ChatSessionStatus,
    ) -> Result<ChatSession> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        if doc.session.internal_voice.is_some() { return Err(anyhow!("internal_voice_session_lifecycle_managed")); }
        if new_status == ChatSessionStatus::Archived && doc.session.is_default_session {
            return Err(anyhow!("The default #general session cannot be archived"));
        }
        self.ensure_segmented_document_for_write(&mut doc).await?;
        doc.session.status = new_status.clone();
        doc.session.updated_at = Utc::now().timestamp_millis();
        self.save_document(&doc).await?;

        self.index.update_status(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            new_status,
            doc.session.updated_at,
        );

        Ok(doc.session)
    }

    async fn archive_session_status(&self, session_id: &str) -> Result<()> {
        self.persist_session_status(session_id, ChatSessionStatus::Archived)
            .await?;
        if let Some(observer) = self.archive_observer() {
            if let Err(error) = observer.on_session_archived(session_id).await {
                warn!(
                    error = %error,
                    session_id = %session_id,
                    "[CHAT-STORE] Failed to clean up archived session"
                );
            }
        }
        Ok(())
    }

    async fn activate_session_status(&self, session_id: &str) -> Result<()> {
        let session = self
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow!("Chat session not found: {}", session_id))?;
        let scope_lock = self.get_scope_lock(
            &session.principal,
            &session.workspace,
            &session.ui_thread_id,
        );
        let _scope_guard = scope_lock.lock().await;

        let activated_session = self
            .persist_session_status(session_id, ChatSessionStatus::Active)
            .await?;
        if thread_rotates_sessions(&activated_session.ui_thread_id) {
            let active_session_ids = self
                .index
                .active_session_ids(
                    &activated_session.principal,
                    &activated_session.workspace,
                    &activated_session.ui_thread_id,
                )
                .into_iter()
                .filter(|candidate| candidate != session_id)
                .collect::<Vec<_>>();
            let mut duplicate_session_ids = Vec::new();
            for candidate in active_session_ids {
                let matches_agent = {
                    let _guard = self.lock_session(&candidate).await?;
                    self.load_document(&candidate)
                        .await
                        .is_ok_and(|doc| doc.session.agent_id == activated_session.agent_id)
                };
                if matches_agent {
                    duplicate_session_ids.push(candidate);
                }
            }
            for duplicate_session_id in duplicate_session_ids {
                self.archive_session_status(&duplicate_session_id).await?;
            }
        }
        if let Some(observer) = self.archive_observer() {
            if let Err(error) = observer.on_session_activated(&activated_session).await {
                warn!(
                    error = %error,
                    session_id = %session_id,
                    "[CHAT-STORE] Failed to resubscribe activated session"
                );
            }
        }
        Ok(())
    }

    fn message_segments_dir(&self, doc: &ChatSessionDocument) -> PathBuf {
        self.workspace_layout
            .chat_session_dir(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            )
            .join("messages")
    }

    fn message_segment_path(&self, doc: &ChatSessionDocument, segment_index: u64) -> PathBuf {
        self.message_segments_dir(doc).join(format!(
            "{:0width$}.jsonl",
            segment_index,
            width = CHAT_MESSAGE_SEGMENT_WIDTH
        ))
    }

    fn parse_message_segment_index(path: &Path) -> Option<u64> {
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            return None;
        }
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u64>().ok())
    }

    fn transcript_dir(&self, doc: &ChatSessionDocument) -> PathBuf {
        self.workspace_layout
            .chat_session_dir(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            )
            .join("llm_history")
    }

    fn transcript_manifest_path(&self, doc: &ChatSessionDocument) -> PathBuf {
        self.transcript_dir(doc).join("manifest.json")
    }

    fn transcript_segments_dir(&self, doc: &ChatSessionDocument) -> PathBuf {
        self.transcript_dir(doc).join("segments")
    }

    fn transcript_segment_path(
        &self,
        doc: &ChatSessionDocument,
        generation: &str,
        sequence: u64,
    ) -> PathBuf {
        self.transcript_segments_dir(doc).join(format!(
            "{}-{:0width$}.json",
            generation,
            sequence,
            width = CHAT_TRANSCRIPT_SEGMENT_WIDTH
        ))
    }

    fn rollback_intent_path(&self, doc: &ChatSessionDocument) -> PathBuf {
        self.workspace_layout
            .chat_session_dir(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            )
            .join("rollback-intent.json")
    }

    fn clear_intent_path(&self, doc: &ChatSessionDocument) -> PathBuf {
        self.workspace_layout
            .chat_session_dir(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            )
            .join("clear-intent.json")
    }

    /// Remove every `<scope>/ui/chat_turn_events/<chat_turn_id>.jsonl`
    /// file referenced by messages in `doc`. Called from `delete_session`
    /// and `clear_messages` — without this, the per-turn activity-card
    /// event logs persist forever even after the user clears the chat
    /// or deletes the session, leaking content the user thought they'd
    /// removed and growing the chat_turn_events dir unboundedly.
    ///
    /// Best-effort: a missing file is normal (turn fired no events the
    /// sink kept). Other errors log a warning and continue; the caller's
    /// primary cleanup (session dir or message segments) is not blocked.
    async fn cleanup_chat_turn_event_files(&self, doc: &ChatSessionDocument) -> Result<()> {
        let mut chat_turn_ids: HashSet<String> = HashSet::new();
        for msg in &doc.messages {
            if let Some(id) = msg.chat_turn_id.as_ref() {
                if !id.is_empty() {
                    chat_turn_ids.insert(id.clone());
                }
            }
        }
        for (_, path) in self.list_message_segments(doc).await? {
            for msg in self.load_message_segment(&path).await? {
                if let Some(id) = msg.chat_turn_id {
                    if !id.is_empty() {
                        chat_turn_ids.insert(id);
                    }
                }
            }
        }

        for chat_turn_id in chat_turn_ids {
            let path = self.workspace_layout.chat_turn_events_path(
                &doc.session.principal,
                &doc.session.workspace,
                &chat_turn_id,
            );
            match self.workspace_layout.remove_file_path(&path).await {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => {
                    warn!(
                        error = %error,
                        path = %path.display(),
                        chat_turn_id = %chat_turn_id,
                        "[CHAT-STORE] failed to remove chat-turn events file"
                    );
                },
            }
        }

        // v0.6.654 — also sweep synthetic `chat-task-<task_id>-<session_id>.jsonl`
        // activity-card files. These come from `ChatTurnEventSink` writes
        // for chat-spawned task immersion (see `subscribe_chat_to_task` in
        // chat/service.rs). The synthetic chat_turn_id is computed at
        // runtime from `task_id + session_id` and is never persisted onto
        // any `ChatMessage.chat_turn_id` field, so the message-walking
        // loop above misses them. Without this sweep, every chat-spawned
        // pack leaks one jsonl file into `<scope>/ui/chat_turn_events/`
        // for the lifetime of the data dir.
        let session_suffix = format!("-{}.jsonl", doc.session.id);
        let prefix = "chat-task-";
        let events_dir = self
            .workspace_layout
            .chat_turn_events_dir(&doc.session.principal, &doc.session.workspace);
        match self.workspace_layout.read_dir_path(&events_dir).await {
            Ok(entries) => {
                for entry in entries {
                    let name_str = entry.file_name.as_str();
                    if !name_str.starts_with(prefix) {
                        continue;
                    }
                    // (a) Match `chat-task-<task_id>-<session_id>.jsonl` for
                    //     the current session (new format, v0.6.654+).
                    // (b) Match `chat-task-<task_id>.jsonl` pre-v0.6.654
                    //     orphans (NO session suffix on disk). Detection:
                    //     task_ids are minted as `task_<32-char-simple-uuid>`
                    //     (see `artifact_v2/service.rs:1114`) — no hyphens.
                    //     So old-format filename body is `task_<32hex>`:
                    //     starts with `task_` AND contains no hyphens.
                    //     New-format body is `task_<32hex>-<session-uuid>`:
                    //     starts with `task_` but DOES contain hyphens.
                    //     Crucially the simpler `matches('-').count() == 4`
                    //     check would also match OTHER sessions' new-format
                    //     files (their `<task_id>-<session-uuid>` body
                    //     contains exactly 4 hyphens), causing cross-session
                    //     data loss; this hyphen-presence check is precise.
                    let should_remove = name_str.ends_with(&session_suffix) || {
                        // Strip prefix + `.jsonl` suffix to inspect body.
                        let body_opt = name_str
                            .strip_prefix(prefix)
                            .and_then(|rest| rest.strip_suffix(".jsonl"));
                        match body_opt {
                            Some(body) => body.starts_with("task_") && !body.contains('-'),
                            None => false,
                        }
                    };
                    if should_remove {
                        let path = events_dir.join(&entry.file_name);
                        if let Err(error) = self.workspace_layout.remove_file_path(&path).await {
                            if !matches!(
                                error,
                                ArtifactV2Error::Io(
                                    ref io_error
                                ) if io_error.kind() == ErrorKind::NotFound
                            ) {
                                warn!(
                                    error = %error,
                                    path = %path.display(),
                                    "[CHAT-STORE] failed to remove orphan chat-task activity-card file"
                                );
                            }
                        }
                    }
                }
            },
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => {
                warn!(
                    error = %error,
                    path = %events_dir.display(),
                    "[CHAT-STORE] failed to scan chat_turn_events dir for orphan chat-task files"
                );
            },
        }
        Ok(())
    }

    /// Load the per-session file index. Returns an empty index when no
    /// file exists yet (a session that never received an upload or tool
    /// output is normal).
    async fn load_file_index_for_doc(
        &self,
        doc: &ChatSessionDocument,
    ) -> Result<ChatSessionFileIndex> {
        let path = self.workspace_layout.chat_session_file_index_path(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );
        let index = match self
            .workspace_layout
            .read_json_bounded_stream_path::<ChatSessionFileIndex, _>(
                &path,
                CHAT_SESSION_FILE_INDEX_MAX_BYTES as u64,
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                CHAT_STORED_JSON_MAX_NODES,
            )
            .await
        {
            Ok(index) => index,
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Ok(ChatSessionFileIndex::default());
            },
            Err(error) => return Err(anyhow!("parse chat session file index: {error}")),
        };
        if index.files.len() > CHAT_SESSION_FILE_INDEX_MAX_ENTRIES {
            return Err(anyhow!("chat session file index exceeds its entry limit"));
        }
        Ok(index)
    }

    async fn save_file_index_for_doc(
        &self,
        doc: &ChatSessionDocument,
        index: &ChatSessionFileIndex,
    ) -> Result<()> {
        let path = self.workspace_layout.chat_session_file_index_path(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );
        if index.files.len() > CHAT_SESSION_FILE_INDEX_MAX_ENTRIES {
            return Err(anyhow!("chat session file index exceeds its entry limit"));
        }
        let bytes = serialize_json_bounded(
            index,
            CHAT_SESSION_FILE_INDEX_MAX_BYTES,
            "chat session file index",
        )?;
        self.workspace_layout
            .write_atomic_path(&path, &bytes)
            .await?;
        Ok(())
    }

    fn output_cleanup_intent_path(&self, doc: &ChatSessionDocument) -> PathBuf {
        self.workspace_layout
            .chat_session_output_cleanup_intent_path(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            )
    }

    async fn load_output_cleanup_intent(
        &self,
        doc: &ChatSessionDocument,
    ) -> Result<Option<ChatOutputCleanupIntent>> {
        let path = self.output_cleanup_intent_path(doc);
        let intent = match self
            .workspace_layout
            .read_json_bounded_stream_path::<ChatOutputCleanupIntent, _>(
                &path,
                CHAT_OUTPUT_CLEANUP_INTENT_MAX_BYTES as u64,
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                CHAT_STORED_JSON_MAX_NODES,
            )
            .await
        {
            Ok(intent) => intent,
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Ok(None);
            },
            Err(error) => return Err(error.into()),
        };
        if intent.format_version != CHAT_OUTPUT_CLEANUP_INTENT_FORMAT_VERSION
            || intent.session_id != doc.session.id
            || intent.session_generation != chat_session_generation(&doc.session)
            || intent.items.is_empty()
            || intent.items.len() > CHAT_OUTPUT_CLEANUP_INTENT_MAX_ITEMS
            || intent.items.iter().any(|item| {
                item.record_id.is_empty()
                    || item.record_id.len() > 1_024
                    || !is_safe_chat_output_name(&item.stored_name)
            })
        {
            return Err(anyhow!("invalid chat output-cleanup intent"));
        }
        Ok(Some(intent))
    }

    async fn save_output_cleanup_intent(
        &self,
        doc: &ChatSessionDocument,
        intent: &ChatOutputCleanupIntent,
    ) -> Result<()> {
        if intent.format_version != CHAT_OUTPUT_CLEANUP_INTENT_FORMAT_VERSION
            || intent.session_id != doc.session.id
            || intent.session_generation != chat_session_generation(&doc.session)
            || intent.items.is_empty()
            || intent.items.len() > CHAT_OUTPUT_CLEANUP_INTENT_MAX_ITEMS
            || intent.items.iter().any(|item| {
                item.record_id.is_empty()
                    || item.record_id.len() > 1_024
                    || !is_safe_chat_output_name(&item.stored_name)
            })
        {
            return Err(anyhow!("invalid chat output-cleanup intent items"));
        }
        let bytes = serialize_json_bounded(
            intent,
            CHAT_OUTPUT_CLEANUP_INTENT_MAX_BYTES,
            "chat output-cleanup intent",
        )?;
        self.workspace_layout
            .write_atomic_path(self.output_cleanup_intent_path(doc), &bytes)
            .await?;
        Ok(())
    }

    async fn remove_output_cleanup_intent(&self, doc: &ChatSessionDocument) -> Result<()> {
        match self
            .workspace_layout
            .remove_file_path(&self.output_cleanup_intent_path(doc))
            .await
        {
            Ok(()) => Ok(()),
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Durable metadata-first reclamation. The intent is published before the
    /// file-index rows disappear and is cleared only after every physical file
    /// is absent. Recovery is idempotent across crashes and partial unlinks.
    /// Caller holds the stable session lifecycle guard; this method takes the
    /// file-index lock second, preserving the global lock order.
    async fn cleanup_file_records_with_intent_locked(
        &self,
        doc: &ChatSessionDocument,
        records: &[super::models::ChatSessionFileRecord],
    ) -> Result<()> {
        if records.len() > CHAT_OUTPUT_CLEANUP_INTENT_MAX_ITEMS {
            return Err(anyhow!("chat output-cleanup batch exceeds its item limit"));
        }
        let index_path = self.workspace_layout.chat_session_file_index_path(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );
        let _index_guard = AgentStorage::acquire_file_lock_exclusive(&index_path)
            .await
            .map_err(|error| anyhow!("lock chat session file index: {error}"))?;
        let mut index = self.load_file_index_for_doc(doc).await?;
        let mut intent = self
            .load_output_cleanup_intent(doc)
            .await?
            .unwrap_or_else(|| ChatOutputCleanupIntent {
                format_version: CHAT_OUTPUT_CLEANUP_INTENT_FORMAT_VERSION,
                session_id: doc.session.id.clone(),
                session_generation: chat_session_generation(&doc.session),
                items: Vec::new(),
                updated_at: Utc::now().timestamp_millis(),
            });
        let mut intent_keys = intent
            .items
            .iter()
            .map(|item| (item.record_id.clone(), item.stored_name.clone()))
            .collect::<HashSet<_>>();
        for record in records {
            if intent_keys.insert((record.id.clone(), record.stored_name.clone())) {
                if intent.items.len() == CHAT_OUTPUT_CLEANUP_INTENT_MAX_ITEMS {
                    return Err(anyhow!("chat output-cleanup intent exceeds its item limit"));
                }
                intent.items.push(ChatOutputCleanupItem {
                    record_id: record.id.clone(),
                    stored_name: record.stored_name.clone(),
                });
            }
        }
        drop(intent_keys);
        if intent.items.is_empty() {
            return Ok(());
        }
        if intent.items.len() > CHAT_OUTPUT_CLEANUP_INTENT_MAX_ITEMS {
            return Err(anyhow!("chat output-cleanup intent exceeds its item limit"));
        }
        intent.updated_at = Utc::now().timestamp_millis();
        if !records.is_empty() {
            if let Err(error) = self.save_output_cleanup_intent(doc, &intent).await {
                match self.load_output_cleanup_intent(doc).await {
                    Ok(Some(published)) if published == intent => {
                        warn!(
                            session_id = %doc.session.id,
                            error = %error,
                            "[CHAT-STORE] output-cleanup intent publish reported an error after commit"
                        );
                    },
                    Ok(_) => return Err(error),
                    Err(verification_error) => {
                        return Err(anyhow!(
                            "output-cleanup intent publication is uncertain: {error}; readback failed: {verification_error}"
                        ));
                    },
                }
            }
        }

        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_output_cleanup_failure_if_configured(
            &doc.session.id,
            ChatOutputCleanupFailpoint::AfterIntent,
        )?;

        let target_keys = intent
            .items
            .iter()
            .map(|item| (item.record_id.as_str(), item.stored_name.as_str()))
            .collect::<HashSet<_>>();
        let before = index.files.len();
        index.files.retain(|record| {
            !target_keys.contains(&(record.id.as_str(), record.stored_name.as_str()))
        });
        if index.files.len() != before {
            if let Err(error) = self.save_file_index_for_doc(doc, &index).await {
                match self.load_file_index_for_doc(doc).await {
                    Ok(published) if published == index => {
                        warn!(
                            session_id = %doc.session.id,
                            error = %error,
                            "[CHAT-STORE] output-cleanup index publish reported an error after commit"
                        );
                        index = published;
                    },
                    Ok(_) => return Err(error),
                    Err(verification_error) => {
                        return Err(anyhow!(
                            "output-cleanup index publication is uncertain: {error}; readback failed: {verification_error}"
                        ));
                    },
                }
            }
        }
        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_output_cleanup_failure_if_configured(
            &doc.session.id,
            ChatOutputCleanupFailpoint::AfterIndexPublish,
        )?;

        let outputs_dir = self.workspace_layout.chat_session_outputs_dir(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );
        let live_names = index
            .files
            .iter()
            .map(|record| record.stored_name.as_str())
            .collect::<HashSet<_>>();
        let mut first_unlink_error = None;
        for item in &intent.items {
            if live_names.contains(item.stored_name.as_str()) {
                // The target row is gone, but another authoritative row now
                // owns the same physical name. Retaining the bytes completes
                // this cleanup item; keeping the intent pending would block
                // every later session operation without making unlink safe.
                continue;
            }
            let path = outputs_dir.join(&item.stored_name);
            match self.workspace_layout.remove_file_path(&path).await {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => {
                    warn!(
                        session_id = %doc.session.id,
                        path = %path.display(),
                        error = %error,
                        "[CHAT-STORE] durable output cleanup will retry physical unlink"
                    );
                    if first_unlink_error.is_none() {
                        first_unlink_error = Some(error.to_string());
                    }
                },
            }
        }
        if let Some(error) = first_unlink_error {
            return Err(anyhow!("chat output cleanup remains pending: {error}"));
        }

        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_output_cleanup_failure_if_configured(
            &doc.session.id,
            ChatOutputCleanupFailpoint::BeforeIntentRemoval,
        )?;
        self.remove_output_cleanup_intent(doc).await
    }

    /// Compute the set of `stored_name`s under `<session>/outputs/`
    /// that this message references — via inline content blocks
    /// (`ContentBlockRecord::File { source: SessionOutput, .. }`),
    /// via attachment lookup (`Attachment.filename` → match the current
    /// stored name or the legacy original name for Attachment records),
    /// or via tool-call-id lookup (`tool_call_id` on
    /// `ToolCallExecuted` / `RichToolResult` / `PackProgress` → match
    /// `file_index.files[].origin.ToolOutput.tool_call_id`).
    ///
    /// Used by `cleanup_outputs_for_deleted_message` to decide which
    /// physical files in `outputs/` belong to the deleted message.
    fn referenced_stored_names(msg: &ChatMessage, index: &ChatSessionFileIndex) -> HashSet<String> {
        let mut names: HashSet<String> = HashSet::new();
        let mut tool_call_ids: HashSet<&str> = HashSet::new();
        let mut attachment_originals: HashSet<&str> = HashSet::new();

        fn add_blocks(blocks: &[ContentBlockRecord], names: &mut HashSet<String>) {
            for block in blocks {
                if let ContentBlockRecord::File {
                    source: ContentFileSource::SessionOutput,
                    relative_path,
                    ..
                } = block
                {
                    if !relative_path.is_empty() {
                        names.insert(relative_path.clone());
                    }
                }
            }
        }

        match &msg.content {
            ChatMessageContent::RichToolResult {
                content_blocks,
                tool_call_id,
                ..
            } => {
                add_blocks(content_blocks, &mut names);
                if let Some(id) = tool_call_id.as_deref() {
                    tool_call_ids.insert(id);
                }
            },
            ChatMessageContent::ToolCallExecuted { tool_call_id, .. } => {
                if let Some(id) = tool_call_id.as_deref() {
                    tool_call_ids.insert(id);
                }
            },
            ChatMessageContent::TaskStatusUpdate { output_files, .. } => {
                add_blocks(output_files, &mut names);
            },
            ChatMessageContent::Attachment { filename, .. } => {
                if !filename.is_empty() {
                    attachment_originals.insert(filename);
                }
            },
            _ => {},
        }

        for record in &index.files {
            match &record.origin {
                ChatSessionFileOrigin::ToolOutput {
                    tool_call_id: Some(id),
                    ..
                } => {
                    if tool_call_ids.contains(id.as_str()) {
                        names.insert(record.stored_name.clone());
                    }
                },
                ChatSessionFileOrigin::Attachment => {
                    if attachment_originals.contains(record.stored_name.as_str())
                        || attachment_originals.contains(record.original_name.as_str())
                    {
                        names.insert(record.stored_name.clone());
                    }
                },
                _ => {},
            }
        }

        names
    }

    /// Remove every file under `<session>/outputs/` referenced ONLY by
    /// `deleted_message` — i.e. orphans after the delete. Walk all
    /// remaining messages (inline + segments on disk) to build the
    /// still-referenced set; only files referenced by the deleted
    /// message AND not by any survivor are removed. Updates
    /// `file_index.json` to drop the matching records.
    ///
    /// Conservative on purpose: if the user deleted a user-attachment
    /// that's also embedded in a later assistant turn's tool result,
    /// the file stays so the assistant message keeps rendering.
    ///
    /// Best-effort: missing files / index-write errors log but don't
    /// abort. Call AFTER `delete_segmented_messages` so the on-disk
    /// segments reflect the post-delete state when we walk them.
    async fn cleanup_outputs_for_deleted_message(
        &self,
        doc: &ChatSessionDocument,
        deleted_message: &ChatMessage,
    ) -> Result<()> {
        // `delete_message` holds the stable session lifecycle guard, so no
        // cross-process message or output writer can invalidate this reference
        // walk before the durable cleanup helper takes the file-index lock.
        let index = self.load_file_index_for_doc(doc).await?;
        if index.files.is_empty() {
            return Ok(());
        }

        let deleted_refs = Self::referenced_stored_names(deleted_message, &index);
        if deleted_refs.is_empty() {
            return Ok(());
        }

        let mut still_referenced: HashSet<String> = HashSet::new();
        for message in &doc.messages {
            if message.id != deleted_message.id {
                still_referenced.extend(Self::referenced_stored_names(message, &index));
            }
        }
        for (_, path) in self.list_message_segments(doc).await? {
            for message in self.load_message_segment(&path).await? {
                if message.id != deleted_message.id {
                    still_referenced.extend(Self::referenced_stored_names(&message, &index));
                }
            }
        }

        let to_remove: HashSet<String> = deleted_refs
            .difference(&still_referenced)
            .cloned()
            .collect();
        if to_remove.is_empty() {
            return Ok(());
        }

        let records = index
            .files
            .iter()
            .filter(|record| to_remove.contains(&record.stored_name))
            .cloned()
            .collect::<Vec<_>>();
        self.cleanup_file_records_with_intent_locked(doc, &records)
            .await
    }

    /// Remove every regular file under `<session>/outputs/` and drop
    /// the per-session file index. Called from `clear_messages` —
    /// when the user empties the chat, every tool-output image,
    /// uploaded attachment, and capability-pack artifact tied to the
    /// cleared turns should go too. (`delete_session` doesn't call
    /// this; it removes the whole session dir, which already takes
    /// `outputs/` and `file_index.json` with it.)
    ///
    /// Missing directories/files are idempotent success. Other cleanup errors
    /// fail the current clear attempt so its durable intent remains available
    /// for retry; visible history is not removed while output cleanup is only
    /// partially authorized.
    async fn cleanup_all_outputs_for_session(&self, doc: &ChatSessionDocument) -> Result<()> {
        let index_path = self.workspace_layout.chat_session_file_index_path(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );
        let _index_guard = AgentStorage::acquire_file_lock_exclusive(&index_path)
            .await
            .map_err(|error| anyhow!("lock chat session file index: {error}"))?;
        let outputs_dir = self.workspace_layout.chat_session_outputs_dir(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );
        let mut first_removal_error = None;
        match self.workspace_layout.read_dir_path(&outputs_dir).await {
            Ok(entries) => {
                for entry in entries {
                    if !entry.is_file {
                        continue;
                    }
                    let path = outputs_dir.join(&entry.file_name);
                    if let Err(error) = self.workspace_layout.remove_file_path(&path).await {
                        if !matches!(
                            error,
                            ArtifactV2Error::Io(
                                ref io_error
                            ) if io_error.kind() == ErrorKind::NotFound
                        ) {
                            warn!(
                                error = %error,
                                path = %path.display(),
                                "[CHAT-STORE] failed to remove session output file"
                            );
                            if first_removal_error.is_none() {
                                first_removal_error = Some(error.to_string());
                            }
                        }
                    }
                }
            },
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                // The outputs directory and file index have independent
                // lifetimes. A missing directory must not leave a stale index
                // behind after whole-chat clear.
            },
            Err(error) => return Err(error.into()),
        }

        if let Some(error) = first_removal_error {
            return Err(anyhow!(
                "session output cleanup remains incomplete: {error}"
            ));
        }

        match self.workspace_layout.remove_file_path(&index_path).await {
            Ok(()) => {},
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
        self.remove_output_cleanup_intent(doc).await?;
        Ok(())
    }

    async fn list_message_segments(
        &self,
        doc: &ChatSessionDocument,
    ) -> Result<Vec<(u64, PathBuf)>> {
        let dir = self.message_segments_dir(doc);
        let mut segments = Vec::new();
        let entries = match self.workspace_layout.read_dir_path(&dir).await {
            Ok(entries) => entries,
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Ok(segments);
            },
            Err(error) => return Err(error.into()),
        };

        for entry in entries {
            let path = dir.join(&entry.file_name);
            if let Some(index) = Self::parse_message_segment_index(&path) {
                segments.push((index, path));
            }
        }
        segments.sort_by_key(|(index, _)| *index);
        Ok(segments)
    }

    async fn load_message_segment(&self, path: &Path) -> Result<Vec<ChatMessage>> {
        let Some(content) = read_optional_bounded_chat_path(
            &self.workspace_layout,
            path,
            CHAT_MESSAGE_SEGMENT_MAX_BYTES,
            "chat message segment",
        )
        .await?
        else {
            return Ok(Vec::new());
        };
        let content = String::from_utf8(content)
            .map_err(|error| anyhow!("chat message segment is not UTF-8: {error}"))?;
        let mut messages = Vec::new();
        for (line_index, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > CHAT_MESSAGE_MIGRATION_RECORD_MAX_BYTES {
                return Err(anyhow!(
                    "chat message segment record exceeds its configured byte limit"
                ));
            }
            let mut message = deserialize_guarded_chat_bytes::<ChatMessage>(
                line.as_bytes(),
                CHAT_MESSAGE_MIGRATION_RECORD_MAX_BYTES as u64,
            )
            .map_err(|error| {
                anyhow!(
                    "Failed to parse chat message segment {:?} line {}: {}",
                    path,
                    line_index + 1,
                    error
                )
            })?;
            message.ensure_presentation();
            messages.push(message);
        }
        Ok(messages)
    }

    async fn save_message_segment(&self, path: &Path, messages: &[ChatMessage]) -> Result<()> {
        if messages.is_empty() {
            match self.workspace_layout.remove_file_path(path).await {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => return Err(error.into()),
            }
            return Ok(());
        }

        let mut writer = CappedVecWriter::new(
            CHAT_MESSAGE_SEGMENT_MAX_BYTES as usize,
            "chat message segment",
        );
        for message in messages {
            serde_json::to_writer(&mut writer, message)?;
            std::io::Write::write_all(&mut writer, b"\n")?;
        }
        self.workspace_layout
            .write_atomic_path(path, &writer.bytes)
            .await?;
        Ok(())
    }

    async fn append_segmented_message(
        &self,
        doc: &ChatSessionDocument,
        mut msg: ChatMessage,
    ) -> Result<()> {
        msg.ensure_presentation();
        self.workspace_layout
            .ensure_chat_session_workspace(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            )
            .await?;
        let segments_dir = self.message_segments_dir(doc);
        self.workspace_layout
            .create_dir_all_path(&segments_dir)
            .await?;

        let mut record = serialize_json_bounded(
            &msg,
            CHAT_MESSAGE_MIGRATION_RECORD_MAX_BYTES,
            "chat message record",
        )?;
        if record.len().saturating_add(1) > CHAT_MESSAGE_SEGMENT_MAX_BYTES as usize {
            return Err(anyhow!("chat message record exceeds segment byte limit"));
        }
        record.push(b'\n');

        let segments = self.list_message_segments(doc).await?;
        let (segment_index, segment_path, mut segment_messages) = match segments.last() {
            Some((index, path)) => {
                let messages = self.load_message_segment(path).await?;
                if messages.iter().any(|existing| existing.id == msg.id) {
                    return Ok(());
                }
                let current_bytes = self
                    .workspace_layout
                    .metadata_path(path)
                    .await?
                    .map_or(0, |metadata| metadata.len());
                if messages.len() >= CHAT_MESSAGE_SEGMENT_SIZE
                    || current_bytes.saturating_add(record.len() as u64)
                        > CHAT_MESSAGE_SEGMENT_MAX_BYTES
                {
                    let next_index = index + 1;
                    (
                        next_index,
                        self.message_segment_path(doc, next_index),
                        Vec::new(),
                    )
                } else {
                    (*index, path.clone(), messages)
                }
            },
            None => (0, self.message_segment_path(doc, 0), Vec::new()),
        };

        let message_id = msg.id.clone();
        segment_messages.push(msg);
        self.save_message_segment(&segment_path, &segment_messages)
            .await?;

        debug!(
            session_id = %doc.session.id,
            segment_index = segment_index,
            message_id = %message_id,
            "[CHAT-STORE] Appended chat message segment record"
        );
        Ok(())
    }

    async fn load_all_segmented_messages(
        &self,
        doc: &ChatSessionDocument,
    ) -> Result<Vec<ChatMessage>> {
        let mut all_messages = Vec::new();
        for (_, path) in self.list_message_segments(doc).await? {
            all_messages.extend(self.load_message_segment(&path).await?);
        }
        Ok(all_messages)
    }

    async fn synthesize_segmented_llm_history_tail(
        &self,
        doc: &ChatSessionDocument,
        limit: usize,
    ) -> Result<Vec<ChatLlmTranscriptEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let segments = self.list_message_segments(doc).await?;
        let mut newest_first = Vec::with_capacity(limit);
        'segments: for (_, path) in segments.iter().rev() {
            let messages = self.load_message_segment(path).await?;
            for message in messages.into_iter().rev() {
                let mut synthesized = Self::synthesize_llm_history(std::slice::from_ref(&message));
                if let Some(entry) = synthesized.pop() {
                    newest_first.push(entry);
                    if newest_first.len() >= limit {
                        break 'segments;
                    }
                }
            }
        }
        newest_first.reverse();
        Ok(newest_first)
    }

    async fn delete_segmented_messages(
        &self,
        doc: &ChatSessionDocument,
        message_ids: &HashSet<String>,
    ) -> Result<bool> {
        let mut removed_any = false;
        for (_, path) in self.list_message_segments(doc).await? {
            let mut messages = self.load_message_segment(&path).await?;
            let original_len = messages.len();
            messages.retain(|message| !message_ids.contains(&message.id));
            if messages.len() != original_len {
                self.save_message_segment(&path, &messages).await?;
                removed_any = true;
            }
        }
        Ok(removed_any)
    }

    async fn segmented_messages_contain_any(
        &self,
        doc: &ChatSessionDocument,
        message_ids: &HashSet<String>,
    ) -> Result<bool> {
        for (_, path) in self.list_message_segments(doc).await? {
            if self
                .load_message_segment(&path)
                .await?
                .iter()
                .any(|message| message_ids.contains(&message.id))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// One backwards walk from `before_id` (or from the newest message when it
    /// is `None`). Returns `None` — and reads no page — when `before_id` was
    /// supplied and names no message in this session.
    ///
    /// Callers that want the historical forgiving behavior compose this with a
    /// second cursor-free call; callers that page programmatically must NOT,
    /// because silently re-serving the newest page turns "advance past an
    /// unknown cursor" into an endless loop back to page one.
    async fn segmented_messages_before_exact(
        &self,
        doc: &ChatSessionDocument,
        limit: usize,
        before_id: Option<&str>,
    ) -> Result<Option<(Vec<ChatMessage>, bool)>> {
        // A cursor-free zero-limit read is an empty page with no "more" claim,
        // matching the trait default; a zero-limit read WITH a cursor is an
        // existence probe and still walks, so an unknown cursor is `None`.
        if limit == 0 && before_id.is_none() {
            return Ok(Some((Vec::new(), false)));
        }
        let segments = self.list_message_segments(doc).await?;
        let mut collected_newest_first = Vec::with_capacity(limit);
        let mut collect = before_id.is_none();

        for (_, path) in segments.iter().rev() {
            let messages = self.load_message_segment(path).await?;
            for message in messages.into_iter().rev() {
                if !collect {
                    if before_id == Some(message.id.as_str()) {
                        collect = true;
                    }
                    continue;
                }

                if collected_newest_first.len() >= limit {
                    collected_newest_first.reverse();
                    // A zero-limit call is a cursor existence probe: the cursor
                    // resolved, and everything past it is still "more".
                    return Ok(Some((collected_newest_first, true)));
                }
                collected_newest_first.push(message);
            }
        }

        if !collect {
            return Ok(None);
        }
        collected_newest_first.reverse();
        Ok(Some((collected_newest_first, false)))
    }

    async fn get_segmented_messages_paginated(
        &self,
        doc: &ChatSessionDocument,
        limit: usize,
        before_id: Option<&str>,
    ) -> Result<(Vec<ChatMessage>, bool)> {
        // Preserved verbatim from before the exact walk was split out: a
        // zero-limit interactive read is an empty page with no "more" claim.
        if limit == 0 {
            return Ok((Vec::new(), false));
        }
        if let Some(page) = self
            .segmented_messages_before_exact(doc, limit, before_id)
            .await?
        {
            return Ok(page);
        }
        // Historical forgiving behavior for the interactive readers: an unknown
        // cursor falls back to the newest page rather than erroring.
        Ok(self
            .segmented_messages_before_exact(doc, limit, None)
            .await?
            .unwrap_or_else(|| (Vec::new(), false)))
    }

    async fn load_transcript_manifest(
        &self,
        doc: &ChatSessionDocument,
    ) -> Result<Option<ChatTranscriptManifest>> {
        let path = self.transcript_manifest_path(doc);
        let manifest: ChatTranscriptManifest = match self
            .workspace_layout
            .read_json_bounded_stream_path(
                &path,
                CHAT_TRANSCRIPT_MAX_MANIFEST_BYTES as u64,
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                CHAT_STORED_JSON_MAX_NODES,
            )
            .await
        {
            Ok(manifest) => manifest,
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Ok(None);
            },
            Err(error) => return Err(error.into()),
        };
        if manifest.format_version != CHAT_TRANSCRIPT_FORMAT_VERSION
            || manifest.generation.trim().is_empty()
            || manifest.active_group_id.trim().is_empty()
            || manifest.open_tool_call_ids.len() > CHAT_TRANSCRIPT_MAX_OPEN_TOOL_CALLS
            || manifest.recent_segments.len() > CHAT_TRANSCRIPT_RECENT_SEGMENT_INDEX
            || manifest
                .open_tool_call_ids
                .iter()
                .any(|id| id.len() > CHAT_TRANSCRIPT_MAX_TOOL_CALL_ID_BYTES)
        {
            return Err(anyhow!("invalid canonical chat transcript manifest"));
        }
        Ok(Some(manifest))
    }

    async fn save_transcript_manifest(
        &self,
        doc: &ChatSessionDocument,
        manifest: &ChatTranscriptManifest,
    ) -> Result<()> {
        self.workspace_layout
            .create_dir_all_path(&self.transcript_dir(doc))
            .await?;
        let bytes = serialize_json_bounded(
            manifest,
            CHAT_TRANSCRIPT_MAX_MANIFEST_BYTES,
            "canonical chat transcript manifest",
        )?;
        self.workspace_layout
            .write_atomic_path(self.transcript_manifest_path(doc), &bytes)
            .await?;
        Ok(())
    }

    async fn load_rollback_intent(
        &self,
        doc: &ChatSessionDocument,
    ) -> Result<Option<ChatRollbackIntent>> {
        let path = self.rollback_intent_path(doc);
        let intent: ChatRollbackIntent = match self
            .workspace_layout
            .read_json_bounded_stream_path(
                &path,
                CHAT_ROLLBACK_INTENT_MAX_BYTES as u64,
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                CHAT_STORED_JSON_MAX_NODES,
            )
            .await
        {
            Ok(intent) => intent,
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Ok(None);
            },
            Err(error) => return Err(error.into()),
        };
        if intent.format_version != CHAT_ROLLBACK_INTENT_FORMAT_VERSION
            || intent.session_id != doc.session.id
            || intent.message_ids.is_empty()
            || intent.message_ids.len() > CHAT_ROLLBACK_MAX_MESSAGE_IDS
            || intent
                .message_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > CHAT_ROLLBACK_MAX_MESSAGE_ID_BYTES)
            || intent.transcript_generation.is_empty()
            || intent.base_effective_entry_count == 0
        {
            return Err(anyhow!("invalid chat rollback intent"));
        }
        Ok(Some(intent))
    }

    async fn save_rollback_intent(
        &self,
        doc: &ChatSessionDocument,
        intent: &ChatRollbackIntent,
    ) -> Result<()> {
        let bytes = serialize_json_bounded(
            intent,
            CHAT_ROLLBACK_INTENT_MAX_BYTES,
            "chat rollback intent",
        )?;
        self.workspace_layout
            .write_atomic_path(self.rollback_intent_path(doc), &bytes)
            .await?;
        Ok(())
    }

    async fn remove_rollback_intent(&self, doc: &ChatSessionDocument) -> Result<()> {
        match self
            .workspace_layout
            .remove_file_path(&self.rollback_intent_path(doc))
            .await
        {
            Ok(()) => Ok(()),
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn load_clear_intent(
        &self,
        doc: &ChatSessionDocument,
    ) -> Result<Option<ChatClearIntent>> {
        let path = self.clear_intent_path(doc);
        let intent: ChatClearIntent = match self
            .workspace_layout
            .read_json_bounded_stream_path(
                &path,
                CHAT_CLEAR_INTENT_MAX_BYTES as u64,
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                CHAT_STORED_JSON_MAX_NODES,
            )
            .await
        {
            Ok(intent) => intent,
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Ok(None);
            },
            Err(error) => return Err(error.into()),
        };
        if intent.format_version != CHAT_CLEAR_INTENT_FORMAT_VERSION
            || intent.session_id != doc.session.id
        {
            return Err(anyhow!("invalid chat clear intent"));
        }
        Ok(Some(intent))
    }

    async fn save_clear_intent(
        &self,
        doc: &ChatSessionDocument,
        intent: &ChatClearIntent,
    ) -> Result<()> {
        let bytes =
            serialize_json_bounded(intent, CHAT_CLEAR_INTENT_MAX_BYTES, "chat clear intent")?;
        self.workspace_layout
            .write_atomic_path(self.clear_intent_path(doc), &bytes)
            .await?;
        Ok(())
    }

    async fn remove_clear_intent(&self, doc: &ChatSessionDocument) -> Result<()> {
        match self
            .workspace_layout
            .remove_file_path(&self.clear_intent_path(doc))
            .await
        {
            Ok(()) => Ok(()),
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn inject_rollback_failure_if_configured(
        session_id: &str,
        phase: ChatRollbackFailpoint,
    ) -> Result<()> {
        let failpoint = CHAT_ROLLBACK_FAILPOINT.get_or_init(|| StdMutex::new(None));
        let mut configured = failpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if configured
            .as_ref()
            .is_some_and(|(configured_session, configured_phase)| {
                configured_session == session_id && *configured_phase == phase
            })
        {
            configured.take();
            return Err(anyhow!("injected chat rollback crash at {phase:?}"));
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn inject_clear_failure_if_configured(
        session_id: &str,
        phase: ChatClearFailpoint,
    ) -> Result<()> {
        let failpoint = CHAT_CLEAR_FAILPOINT.get_or_init(|| StdMutex::new(None));
        let mut configured = failpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if configured
            .as_ref()
            .is_some_and(|(configured_session, configured_phase)| {
                configured_session == session_id && *configured_phase == phase
            })
        {
            configured.take();
            return Err(anyhow!("injected chat clear crash at {phase:?}"));
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn inject_output_cleanup_failure_if_configured(
        session_id: &str,
        phase: ChatOutputCleanupFailpoint,
    ) -> Result<()> {
        let failpoint = CHAT_OUTPUT_CLEANUP_FAILPOINT.get_or_init(|| StdMutex::new(None));
        let mut configured = failpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if configured
            .as_ref()
            .is_some_and(|(configured_session, configured_phase)| {
                configured_session == session_id && *configured_phase == phase
            })
        {
            configured.take();
            return Err(anyhow!("injected chat output-cleanup crash at {phase:?}"));
        }
        Ok(())
    }

    async fn transcript_has_committed_rollback(
        &self,
        doc: &ChatSessionDocument,
        intent: &ChatRollbackIntent,
        manifest: &ChatTranscriptManifest,
    ) -> Result<bool> {
        if manifest.generation != intent.transcript_generation
            || manifest.last_sequence != intent.base_last_sequence.saturating_add(1)
            || manifest.effective_entry_count != intent.base_effective_entry_count.saturating_sub(1)
        {
            return Ok(false);
        }
        let path = self.transcript_segment_path(doc, &manifest.generation, manifest.last_sequence);
        let segment: ChatTranscriptSegment = self
            .workspace_layout
            .read_json_bounded_stream_path(
                &path,
                CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES as u64,
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                CHAT_TRANSCRIPT_MAX_ENCODED_JSON_NODES,
            )
            .await?;
        Ok(segment.format_version == CHAT_TRANSCRIPT_FORMAT_VERSION
            && segment.generation == manifest.generation
            && segment.sequence == manifest.last_sequence
            && matches!(
                segment.mutation,
                ChatTranscriptMutation::TruncateTail { count: 1 }
            ))
    }

    /// Complete an already-published rollback transaction. The caller holds
    /// the session lock. Every step is idempotent, and the intent is removed
    /// only after display deletion, transcript truncation, and metadata commit.
    async fn complete_rollback_intent_locked(
        &self,
        doc: &mut ChatSessionDocument,
        intent: &ChatRollbackIntent,
    ) -> Result<()> {
        let message_ids = intent.message_ids.iter().cloned().collect::<HashSet<_>>();
        self.delete_segmented_messages(doc, &message_ids).await?;

        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_rollback_failure_if_configured(
            &doc.session.id,
            ChatRollbackFailpoint::AfterDisplayDeletion,
        )?;

        let manifest = self
            .load_transcript_manifest(doc)
            .await?
            .ok_or_else(|| anyhow!("chat rollback transcript manifest is missing"))?;
        let at_base = manifest.generation == intent.transcript_generation
            && manifest.last_sequence == intent.base_last_sequence
            && manifest.effective_entry_count == intent.base_effective_entry_count;
        if at_base {
            self.append_transcript_mutation(
                doc,
                manifest,
                ChatTranscriptMutation::TruncateTail { count: 1 },
                true,
            )
            .await?;
        } else if !self
            .transcript_has_committed_rollback(doc, intent, &manifest)
            .await?
        {
            return Err(anyhow!(
                "chat rollback transcript authority diverged from its durable intent"
            ));
        }

        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_rollback_failure_if_configured(
            &doc.session.id,
            ChatRollbackFailpoint::AfterTranscriptCommit,
        )?;

        doc.session.updated_at = intent.updated_at;
        doc.messages.clear();
        doc.llm_history.clear();
        self.save_document(doc).await?;
        self.remove_rollback_intent(doc).await
    }

    async fn recover_rollback_intent_locked(&self, session_id: &str) -> Result<()> {
        let intent_path = if let Some(location) = self.session_locations.get(session_id) {
            self.workspace_layout
                .chat_session_dir(&location.principal, &location.workspace, session_id)
                .join("rollback-intent.json")
        } else {
            let session_path = match self.resolve_session_path(session_id).await {
                Ok(path) => path,
                Err(error) if error.to_string().contains("Chat session not found") => {
                    return Ok(());
                },
                Err(error) => return Err(error),
            };
            let Some(session_dir) = session_path.parent() else {
                return Ok(());
            };
            session_dir.join("rollback-intent.json")
        };
        match self.workspace_layout.metadata_path(&intent_path).await {
            Ok(Some(_)) => {},
            Ok(None) => return Ok(()),
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        // Only parse the session document when an intent is actually pending.
        // Corrupt unrelated sessions retain the existing get/list behavior,
        // while a pending rollback fails closed until it can be recovered.
        let mut doc = self.load_document(session_id).await?;
        let Some(intent) = self.load_rollback_intent(&doc).await? else {
            return Ok(());
        };
        self.ensure_segmented_document_for_write(&mut doc).await?;
        self.complete_rollback_intent_locked(&mut doc, &intent)
            .await?;
        self.index.update_timestamp(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            doc.session.updated_at,
        );
        Ok(())
    }

    /// Complete a durable whole-chat clear while the caller holds the session
    /// lock. Auxiliary cleanup is attempted before either visible messages or
    /// the provider transcript is removed, and every authority mutation is
    /// idempotent for restart recovery.
    async fn complete_clear_intent_locked(
        &self,
        doc: &mut ChatSessionDocument,
        intent: &ChatClearIntent,
    ) -> Result<usize> {
        if let Err(error) = self.cleanup_chat_turn_event_files(doc).await {
            warn!(
                error = %error,
                session_id = %doc.session.id,
                "[CHAT-STORE] failed to clean up chat-turn event files"
            );
        }
        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_clear_failure_if_configured(
            &doc.session.id,
            ChatClearFailpoint::BeforeOutputCleanup,
        )?;
        // Unlike activity diagnostics, session outputs can contain user
        // attachments and authoritative task/tool files. A lock timeout or
        // deletion failure must retain the durable clear intent and leave
        // visible/provider history untouched so recovery can retry safely.
        self.cleanup_all_outputs_for_session(doc).await?;

        let had_llm_history =
            !doc.llm_history.is_empty() || self.load_transcript_manifest(doc).await?.is_some();
        let mut total_cleared = 0usize;
        for (_, path) in self.list_message_segments(doc).await? {
            let messages = self.load_message_segment(&path).await?;
            if messages.is_empty() {
                continue;
            }
            total_cleared = total_cleared.saturating_add(messages.len());
            self.save_message_segment(&path, &Vec::new()).await?;
        }

        // Preserve the established idempotent clear contract. Auxiliary
        // orphan cleanup above still runs, but an already-empty display and
        // provider history must not acquire a new timestamp merely because the
        // crash-recovery intent protocol is now present.
        if total_cleared == 0 && doc.messages.is_empty() && !had_llm_history {
            self.remove_clear_intent(doc).await?;
            return Ok(0);
        }

        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_clear_failure_if_configured(
            &doc.session.id,
            ChatClearFailpoint::AfterDisplayDeletion,
        )?;

        doc.llm_history.clear();
        self.reset_segmented_transcript(doc).await?;

        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_clear_failure_if_configured(
            &doc.session.id,
            ChatClearFailpoint::AfterTranscriptReset,
        )?;

        doc.session.updated_at = intent.updated_at;
        doc.messages.clear();
        doc.format_version = CHAT_SESSION_DOCUMENT_FORMAT_VERSION;
        self.save_document(doc).await?;
        self.remove_clear_intent(doc).await?;
        Ok(total_cleared)
    }

    async fn recover_clear_intent_locked(&self, session_id: &str) -> Result<()> {
        let intent_path = if let Some(location) = self.session_locations.get(session_id) {
            self.workspace_layout
                .chat_session_dir(&location.principal, &location.workspace, session_id)
                .join("clear-intent.json")
        } else {
            let session_path = match self.resolve_session_path(session_id).await {
                Ok(path) => path,
                Err(error) if error.to_string().contains("Chat session not found") => {
                    return Ok(());
                },
                Err(error) => return Err(error),
            };
            let Some(session_dir) = session_path.parent() else {
                return Ok(());
            };
            session_dir.join("clear-intent.json")
        };
        match self.workspace_layout.metadata_path(&intent_path).await {
            Ok(Some(_)) => {},
            Ok(None) => return Ok(()),
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        let mut doc = self.load_document(session_id).await?;
        let Some(intent) = self.load_clear_intent(&doc).await? else {
            return Ok(());
        };
        self.ensure_segmented_document_for_write(&mut doc).await?;
        self.complete_clear_intent_locked(&mut doc, &intent).await?;
        self.index.update_timestamp(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            doc.session.updated_at,
        );
        Ok(())
    }

    async fn recover_output_cleanup_intent_locked(&self, session_id: &str) -> Result<()> {
        let intent_path = if let Some(location) = self.session_locations.get(session_id) {
            self.workspace_layout
                .chat_session_output_cleanup_intent_path(
                    &location.principal,
                    &location.workspace,
                    session_id,
                )
        } else {
            let session_path = match self.resolve_session_path(session_id).await {
                Ok(path) => path,
                Err(error) if error.to_string().contains("Chat session not found") => {
                    return Ok(());
                },
                Err(error) => return Err(error),
            };
            let Some(session_dir) = session_path.parent() else {
                return Ok(());
            };
            session_dir.join("output_cleanup_intent.json")
        };
        match self.workspace_layout.metadata_path(&intent_path).await {
            Ok(Some(_)) => {},
            Ok(None) => return Ok(()),
            Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        let doc = self.load_document(session_id).await?;
        self.cleanup_file_records_with_intent_locked(&doc, &[])
            .await
    }

    fn apply_transcript_entries_to_open_calls(
        open: &mut BTreeSet<String>,
        entries: &[ChatLlmTranscriptEntry],
    ) -> Result<()> {
        for entry in entries {
            match entry {
                ChatLlmTranscriptEntry::AssistantTurn { tool_calls, .. } => {
                    for call in tool_calls {
                        if call.id.len() > CHAT_TRANSCRIPT_MAX_TOOL_CALL_ID_BYTES {
                            return Err(anyhow!(
                                "canonical transcript tool-call id exceeds byte limit"
                            ));
                        }
                        open.insert(call.id.clone());
                    }
                },
                ChatLlmTranscriptEntry::ToolResult { tool_call_id, .. }
                | ChatLlmTranscriptEntry::ToolResultRich { tool_call_id, .. }
                | ChatLlmTranscriptEntry::ToolResultProjected { tool_call_id, .. } => {
                    if tool_call_id.len() > CHAT_TRANSCRIPT_MAX_TOOL_CALL_ID_BYTES {
                        return Err(anyhow!(
                            "canonical transcript tool-result id exceeds byte limit"
                        ));
                    }
                    open.remove(tool_call_id);
                },
                ChatLlmTranscriptEntry::UserText { .. }
                | ChatLlmTranscriptEntry::UserTurn { .. } => {},
            }
            if open.len() > CHAT_TRANSCRIPT_MAX_OPEN_TOOL_CALLS {
                return Err(anyhow!(
                    "canonical chat transcript has too many unmatched tool calls"
                ));
            }
        }
        Ok(())
    }

    fn transcript_entries_have_bounded_json(entries: &[ChatLlmTranscriptEntry]) -> bool {
        let mut remaining_nodes = CHAT_TRANSCRIPT_MAX_JSON_NODES_PER_APPEND;
        let mut value_is_bounded = |value: &serde_json::Value| {
            let Some(metrics) =
                crate::magician_v2::json_traversal::inspect_json_bounded(value, remaining_nodes)
            else {
                return false;
            };
            if metrics.max_depth > crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH {
                return false;
            }
            remaining_nodes = remaining_nodes.saturating_sub(metrics.nodes);
            true
        };
        for entry in entries {
            match entry {
                ChatLlmTranscriptEntry::AssistantTurn {
                    tool_calls,
                    provider_state,
                    ..
                } => {
                    for call in tool_calls {
                        if !value_is_bounded(&call.arguments) {
                            return false;
                        }
                    }
                    let provider_values = match provider_state {
                        Some(super::models::AssistantProviderState::Gemini { parts }) => {
                            Some(parts.as_slice())
                        },
                        Some(super::models::AssistantProviderState::AnthropicMessages {
                            content,
                        }) => Some(content.as_slice()),
                        Some(super::models::AssistantProviderState::OpenaiResponses { .. })
                        | None => None,
                    };
                    if let Some(values) = provider_values {
                        for value in values {
                            if !value_is_bounded(value) {
                                return false;
                            }
                        }
                    }
                },
                ChatLlmTranscriptEntry::ToolResultProjected { projection, .. } => {
                    if !value_is_bounded(&projection.model.value) {
                        return false;
                    }
                },
                ChatLlmTranscriptEntry::UserText { .. }
                | ChatLlmTranscriptEntry::UserTurn { .. }
                | ChatLlmTranscriptEntry::ToolResult { .. }
                | ChatLlmTranscriptEntry::ToolResultRich { .. } => {},
            }
        }
        true
    }

    fn discard_transcript_json_iteratively(entries: &mut [ChatLlmTranscriptEntry]) {
        let discard = |value: &mut serde_json::Value| {
            crate::magician_v2::json_traversal::discard_json_iteratively(std::mem::take(value));
        };
        for entry in entries {
            match entry {
                ChatLlmTranscriptEntry::AssistantTurn {
                    tool_calls,
                    provider_state,
                    ..
                } => {
                    for call in tool_calls {
                        discard(&mut call.arguments);
                    }
                    match provider_state {
                        Some(super::models::AssistantProviderState::Gemini { parts }) => {
                            for value in parts {
                                discard(value);
                            }
                        },
                        Some(super::models::AssistantProviderState::AnthropicMessages {
                            content,
                        }) => {
                            for value in content {
                                discard(value);
                            }
                        },
                        Some(super::models::AssistantProviderState::OpenaiResponses { .. })
                        | None => {},
                    }
                },
                ChatLlmTranscriptEntry::ToolResultProjected { projection, .. } => {
                    discard(&mut projection.model.value);
                },
                ChatLlmTranscriptEntry::UserText { .. }
                | ChatLlmTranscriptEntry::UserTurn { .. }
                | ChatLlmTranscriptEntry::ToolResult { .. }
                | ChatLlmTranscriptEntry::ToolResultRich { .. } => {},
            }
        }
    }

    async fn append_transcript_mutation(
        &self,
        doc: &ChatSessionDocument,
        mut manifest: ChatTranscriptManifest,
        mutation: ChatTranscriptMutation,
        publish_manifest: bool,
    ) -> Result<ChatTranscriptManifest> {
        let entry_delta = match &mutation {
            ChatTranscriptMutation::Append { entries } => {
                if entries.is_empty() || entries.len() > CHAT_TRANSCRIPT_MAX_ENTRIES_PER_APPEND {
                    return Err(anyhow!("canonical transcript append count exceeds limit"));
                }
                if manifest.open_tool_call_ids.is_empty() {
                    manifest.active_group_id = Uuid::new_v4().simple().to_string();
                }
                Self::apply_transcript_entries_to_open_calls(
                    &mut manifest.open_tool_call_ids,
                    entries,
                )?;
                entries.len() as i64
            },
            ChatTranscriptMutation::TruncateTail { count } => {
                if *count == 0 || *count > manifest.effective_entry_count {
                    return Err(anyhow!("invalid canonical transcript tail truncation"));
                }
                // Rollback is permitted only for a just-persisted user entry;
                // callers inspect the authoritative tail before recording it.
                -(*count as i64)
            },
        };

        let sequence = manifest.last_sequence.saturating_add(1);
        let segment = ChatTranscriptSegment {
            format_version: CHAT_TRANSCRIPT_FORMAT_VERSION,
            generation: manifest.generation.clone(),
            sequence,
            group_id: manifest.active_group_id.clone(),
            mutation,
        };
        let segment_bytes = serialize_json_bounded(
            &segment,
            CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES,
            "canonical transcript segment",
        )?;
        self.workspace_layout
            .create_dir_all_path(&self.transcript_segments_dir(doc))
            .await?;
        self.workspace_layout
            .write_atomic_path(
                self.transcript_segment_path(doc, &manifest.generation, sequence),
                &segment_bytes,
            )
            .await?;

        manifest.last_sequence = sequence;
        manifest.recent_segments.push(ChatTranscriptRecentSegment {
            sequence,
            group_id: segment.group_id,
        });
        if manifest.recent_segments.len() > CHAT_TRANSCRIPT_RECENT_SEGMENT_INDEX {
            let drain = manifest.recent_segments.len() - CHAT_TRANSCRIPT_RECENT_SEGMENT_INDEX;
            manifest.recent_segments.drain(0..drain);
        }
        manifest.effective_entry_count = if entry_delta >= 0 {
            manifest
                .effective_entry_count
                .saturating_add(entry_delta as u64)
        } else {
            manifest
                .effective_entry_count
                .saturating_sub((-entry_delta) as u64)
        };
        // Publishing the manifest last makes it the commit point. Complete
        // segment files from a failed append are inert orphans.
        if publish_manifest {
            self.save_transcript_manifest(doc, &manifest).await?;
        }
        Ok(manifest)
    }

    async fn load_segmented_transcript(
        &self,
        doc: &ChatSessionDocument,
        manifest: &ChatTranscriptManifest,
    ) -> Result<Vec<ChatLlmTranscriptEntry>> {
        let capacity = usize::try_from(manifest.effective_entry_count)
            .unwrap_or(usize::MAX)
            .min(100_000);
        let mut history = Vec::with_capacity(capacity);
        for sequence in 1..=manifest.last_sequence {
            let path = self.transcript_segment_path(doc, &manifest.generation, sequence);
            let mut segment: ChatTranscriptSegment = self
                .workspace_layout
                .read_json_bounded_stream_path(
                    &path,
                    CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES as u64,
                    crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                    CHAT_TRANSCRIPT_MAX_ENCODED_JSON_NODES,
                )
                .await?;
            if segment.format_version != CHAT_TRANSCRIPT_FORMAT_VERSION
                || segment.generation != manifest.generation
                || segment.sequence != sequence
            {
                return Err(anyhow!("canonical transcript segment identity mismatch"));
            }
            if let ChatTranscriptMutation::Append { entries } = &mut segment.mutation {
                if !Self::transcript_entries_have_bounded_json(entries) {
                    Self::discard_transcript_json_iteratively(entries);
                    return Err(anyhow!(
                        "canonical transcript segment exceeds the retained JSON contract"
                    ));
                }
            }
            match segment.mutation {
                ChatTranscriptMutation::Append { entries } => history.extend(entries),
                ChatTranscriptMutation::TruncateTail { count } => {
                    let count = usize::try_from(count).unwrap_or(usize::MAX);
                    if count > history.len() {
                        return Err(anyhow!("canonical transcript truncation exceeds history"));
                    }
                    history.truncate(history.len() - count);
                },
            }
        }
        if history.len() as u64 != manifest.effective_entry_count {
            return Err(anyhow!("canonical transcript manifest count mismatch"));
        }
        Ok(history)
    }

    async fn load_segmented_transcript_tail(
        &self,
        doc: &ChatSessionDocument,
        manifest: &ChatTranscriptManifest,
        limit: usize,
    ) -> Result<Vec<ChatLlmTranscriptEntry>> {
        if limit == 0 || manifest.effective_entry_count == 0 {
            return Ok(Vec::new());
        }
        let mut newest_first = Vec::with_capacity(limit.min(1_024));
        let mut skip_deleted = 0_u64;
        let mut boundary_group: Option<String> = None;

        for sequence in (1..=manifest.last_sequence).rev() {
            let path = self.transcript_segment_path(doc, &manifest.generation, sequence);
            let mut segment: ChatTranscriptSegment = self
                .workspace_layout
                .read_json_bounded_stream_path(
                    &path,
                    CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES as u64,
                    crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                    CHAT_TRANSCRIPT_MAX_ENCODED_JSON_NODES,
                )
                .await?;
            if segment.format_version != CHAT_TRANSCRIPT_FORMAT_VERSION
                || segment.generation != manifest.generation
                || segment.sequence != sequence
            {
                return Err(anyhow!("canonical transcript segment identity mismatch"));
            }
            if boundary_group
                .as_deref()
                .is_some_and(|group| group != segment.group_id.as_str())
                && skip_deleted == 0
            {
                break;
            }
            if let ChatTranscriptMutation::Append { entries } = &mut segment.mutation {
                if !Self::transcript_entries_have_bounded_json(entries) {
                    Self::discard_transcript_json_iteratively(entries);
                    return Err(anyhow!(
                        "canonical transcript segment exceeds the retained JSON contract"
                    ));
                }
            }
            match segment.mutation {
                ChatTranscriptMutation::TruncateTail { count } => {
                    skip_deleted = skip_deleted.saturating_add(count);
                },
                ChatTranscriptMutation::Append { entries } => {
                    for entry in entries.into_iter().rev() {
                        if skip_deleted > 0 {
                            skip_deleted -= 1;
                            continue;
                        }
                        newest_first.push(entry);
                        if newest_first.len() > CHAT_TRANSCRIPT_MAX_TAIL_ENTRIES {
                            return Err(anyhow!(
                                "canonical transcript continuation group exceeds tail limit"
                            ));
                        }
                    }
                    if newest_first.len() >= limit && boundary_group.is_none() {
                        boundary_group = Some(segment.group_id);
                    }
                },
            }
        }
        if skip_deleted > 0 {
            return Err(anyhow!(
                "canonical transcript truncation exceeds retained history"
            ));
        }
        newest_first.reverse();
        Ok(newest_first)
    }

    async fn migrate_legacy_messages_if_needed(&self, doc: &mut ChatSessionDocument) -> Result<()> {
        if doc.messages.is_empty() {
            return Ok(());
        }

        // Segment names are deterministic and each segment is replaced
        // atomically. If a crash happens part-way through migration, the next
        // attempt verifies the already-written prefix and resumes at the first
        // missing record rather than duplicating display history.
        let mut existing_count = 0usize;
        let segments = self.list_message_segments(doc).await?;
        for (position, (index, path)) in segments.iter().enumerate() {
            if *index != position as u64 {
                return Err(anyhow!(
                    "segmented chat message migration contains a sequence gap"
                ));
            }
            let persisted_messages = self.load_message_segment(path).await?;
            if position + 1 < segments.len()
                && persisted_messages.len() != CHAT_MESSAGE_SEGMENT_SIZE
            {
                return Err(anyhow!(
                    "segmented chat message migration contains a partial interior segment"
                ));
            }
            for persisted in persisted_messages {
                let legacy = doc.messages.get(existing_count).ok_or_else(|| {
                    anyhow!("segmented chat message migration exceeds the legacy source")
                })?;
                if serialized_json_digest(&persisted)? != serialized_json_digest(legacy)? {
                    return Err(anyhow!(
                        "segmented chat message migration disagrees with the legacy source"
                    ));
                }
                existing_count = existing_count.saturating_add(1);
            }
        }
        if existing_count < doc.messages.len() && existing_count % CHAT_MESSAGE_SEGMENT_SIZE != 0 {
            return Err(anyhow!(
                "segmented chat message migration contains a partial non-terminal segment"
            ));
        }

        self.workspace_layout
            .create_dir_all_path(&self.message_segments_dir(doc))
            .await?;
        let legacy = std::mem::take(&mut doc.messages);
        let mut remaining = legacy.into_iter().skip(existing_count);
        let mut segment_index = (existing_count / CHAT_MESSAGE_SEGMENT_SIZE) as u64;
        loop {
            let messages = take_bounded_batch(&mut remaining, CHAT_MESSAGE_SEGMENT_SIZE);
            if messages.is_empty() {
                break;
            }
            self.save_message_segment(&self.message_segment_path(doc, segment_index), &messages)
                .await?;
            segment_index = segment_index.saturating_add(1);
        }
        Ok(())
    }

    async fn migrate_legacy_transcript_if_needed(
        &self,
        doc: &mut ChatSessionDocument,
    ) -> Result<ChatTranscriptManifest> {
        self.migrate_legacy_messages_if_needed(doc).await?;
        if let Some(mut manifest) = self.load_transcript_manifest(doc).await? {
            if manifest.legacy_migration_complete {
                if doc.format_version != CHAT_SESSION_DOCUMENT_FORMAT_VERSION
                    || !doc.llm_history.is_empty()
                {
                    doc.llm_history.clear();
                    doc.format_version = CHAT_SESSION_DOCUMENT_FORMAT_VERSION;
                    self.save_document(doc).await?;
                }
                return Ok(manifest);
            }

            let legacy = std::mem::take(&mut doc.llm_history);
            let migrated = usize::try_from(manifest.effective_entry_count)
                .map_err(|_| anyhow!("legacy transcript migration count is not addressable"))?;
            if migrated > legacy.len() {
                return Err(anyhow!(
                    "legacy transcript migration manifest exceeds source history"
                ));
            }
            let mut remaining = legacy.into_iter().skip(migrated);
            loop {
                let entries =
                    take_bounded_batch(&mut remaining, CHAT_TRANSCRIPT_MAX_ENTRIES_PER_APPEND);
                if entries.is_empty() {
                    break;
                }
                manifest = self
                    .append_transcript_mutation(
                        doc,
                        manifest,
                        ChatTranscriptMutation::Append { entries },
                        true,
                    )
                    .await?;
            }
            manifest.legacy_migration_complete = true;
            self.save_transcript_manifest(doc, &manifest).await?;
            doc.format_version = CHAT_SESSION_DOCUMENT_FORMAT_VERSION;
            self.save_document(doc).await?;
            return Ok(manifest);
        }

        let legacy = std::mem::take(&mut doc.llm_history);
        let mut manifest = ChatTranscriptManifest::empty();
        manifest.legacy_migration_complete = legacy.is_empty();
        self.save_transcript_manifest(doc, &manifest).await?;
        let mut remaining = legacy.into_iter();
        loop {
            let entries =
                take_bounded_batch(&mut remaining, CHAT_TRANSCRIPT_MAX_ENTRIES_PER_APPEND);
            if entries.is_empty() {
                break;
            }
            manifest = self
                .append_transcript_mutation(
                    doc,
                    manifest,
                    ChatTranscriptMutation::Append { entries },
                    true,
                )
                .await?;
        }
        if !manifest.legacy_migration_complete {
            manifest.legacy_migration_complete = true;
            self.save_transcript_manifest(doc, &manifest).await?;
        }
        doc.format_version = CHAT_SESSION_DOCUMENT_FORMAT_VERSION;
        self.save_document(doc).await?;
        Ok(manifest)
    }

    async fn ensure_segmented_document_for_write(
        &self,
        doc: &mut ChatSessionDocument,
    ) -> Result<()> {
        if doc.format_version == CHAT_SESSION_DOCUMENT_FORMAT_VERSION {
            validate_session_document_shape(doc)?;
            return Ok(());
        }
        self.migrate_legacy_transcript_if_needed(doc).await?;
        validate_session_document_shape(doc)
    }

    async fn reset_segmented_transcript(&self, doc: &ChatSessionDocument) -> Result<()> {
        // A generation switch is an O(1) logical clear. Older immutable
        // segments are no longer addressable and can be reclaimed
        // by background retention without delaying the user's clear response.
        self.save_transcript_manifest(doc, &ChatTranscriptManifest::empty())
            .await
    }

    async fn reclaim_stale_transcript_generations(
        &self,
        doc: &ChatSessionDocument,
        retained_generation: &str,
    ) {
        for (dir, retain_current_generation) in [
            (self.transcript_segments_dir(doc), true),
            // Detached provider checkpoints are no longer written or read.
            // Maintenance removes all remnants of that transitional format.
            (self.transcript_dir(doc).join("checkpoints"), false),
        ] {
            let entries = match self.workspace_layout.read_dir_path(&dir).await {
                Ok(entries) => entries,
                Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                    continue;
                },
                Err(error) => {
                    warn!(
                        session_id = %doc.session.id,
                        %error,
                        "[CHAT-STORAGE] transcript retention scan failed"
                    );
                    continue;
                },
            };
            for entry in entries {
                let retain =
                    retain_current_generation && entry.file_name.starts_with(retained_generation);
                if !retain {
                    if let Err(error) = self
                        .workspace_layout
                        .remove_file_path(dir.join(&entry.file_name))
                        .await
                    {
                        warn!(
                            session_id = %doc.session.id,
                            %error,
                            "[CHAT-STORAGE] transcript retention kept a stale generation file"
                        );
                    }
                }
            }
        }
    }

    /// Maintenance-only compaction. It is never called from append, provider
    /// dispatch, or visible-answer finalization. A complete candidate
    /// generation is written first and becomes authoritative with one final
    /// manifest replacement, so a crash cannot publish a partial compaction.
    pub async fn compact_llm_history(&self, session_id: &str) -> Result<bool> {
        self.compact_llm_history_admitted(session_id, false).await
    }

    async fn compact_llm_history_admitted(
        &self,
        session_id: &str,
        require_quiescent: bool,
    ) -> Result<bool> {
        if self
            .transcript_compactions_inflight
            .insert(session_id.to_string(), ())
            .is_some()
        {
            return Ok(false);
        }
        let _lease = TranscriptCompactionLease {
            inflight: &self.transcript_compactions_inflight,
            session_id: session_id.to_string(),
        };
        Box::pin(self.compact_llm_history_exclusive(session_id, require_quiescent)).await
    }

    async fn compact_llm_history_exclusive(
        &self,
        session_id: &str,
        require_quiescent: bool,
    ) -> Result<bool> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        if require_quiescent
            && doc.session.updated_at
                > Utc::now()
                    .timestamp_millis()
                    .saturating_sub(CHAT_TRANSCRIPT_COMPACTION_QUIESCENCE_MS)
        {
            // Revalidate under the same mutation lock used by append. A chat
            // can become active after the maintenance index snapshot; in that
            // race auxiliary compaction must yield without rewriting history.
            return Ok(false);
        }
        let current = self.migrate_legacy_transcript_if_needed(&mut doc).await?;
        if current
            .last_sequence
            .saturating_sub(current.compacted_through_sequence)
            < CHAT_TRANSCRIPT_COMPACTION_SEGMENT_THRESHOLD
        {
            self.reclaim_stale_transcript_generations(&doc, &current.generation)
                .await;
            return Ok(false);
        }
        let history = self.load_segmented_transcript(&doc, &current).await?;
        let mut candidate = ChatTranscriptManifest::empty();
        let mut remaining = history.into_iter();
        loop {
            let entries =
                take_bounded_batch(&mut remaining, CHAT_TRANSCRIPT_MAX_ENTRIES_PER_APPEND);
            if entries.is_empty() {
                break;
            }
            candidate = self
                .append_transcript_mutation(
                    &doc,
                    candidate,
                    ChatTranscriptMutation::Append { entries },
                    false,
                )
                .await?;
        }
        candidate.compacted_through_sequence = candidate.last_sequence;
        let retained_generation = candidate.generation.clone();
        self.save_transcript_manifest(&doc, &candidate).await?;

        // Reclamation happens only after the generation switch. Failures are
        // harmless retained bytes and are retried by later maintenance.
        self.reclaim_stale_transcript_generations(&doc, &retained_generation)
            .await;
        Ok(true)
    }

    /// One fair, bounded slice for the process-owned storage-maintenance
    /// runtime. It never runs on append/finalization and never builds an
    /// unbounded work queue: at most `limit` session futures are awaited
    /// sequentially, starting from a rotating cursor.
    pub async fn compact_llm_history_maintenance_batch(
        &self,
        limit: usize,
    ) -> Result<(usize, usize)> {
        let updated_before = Utc::now()
            .timestamp_millis()
            .saturating_sub(CHAT_TRANSCRIPT_COMPACTION_QUIESCENCE_MS);
        let session_ids = self.index.quiescent_session_ids_sorted(updated_before);
        if session_ids.is_empty() || limit == 0 {
            return Ok((0, 0));
        }
        let batch_len = limit.min(session_ids.len());
        let start = self
            .transcript_compaction_cursor
            .fetch_add(batch_len, Ordering::AcqRel)
            % session_ids.len();
        let mut inspected = 0usize;
        let mut compacted = 0usize;
        for offset in 0..batch_len {
            let session_id = &session_ids[(start + offset) % session_ids.len()];
            match Box::pin(self.compact_llm_history_admitted(session_id, true)).await {
                Ok(changed) => {
                    inspected = inspected.saturating_add(1);
                    compacted = compacted.saturating_add(usize::from(changed));
                },
                Err(error) => {
                    inspected = inspected.saturating_add(1);
                    warn!(
                        session_id,
                        %error,
                        "[CHAT-STORAGE] transcript maintenance kept the prior generation"
                    );
                },
            }
            tokio::task::yield_now().await;
        }
        Ok((inspected, compacted))
    }

    fn acquire_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        self.session_locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn lock_session(&self, session_id: &str) -> Result<ChatSessionWriteGuard> {
        let local = self.acquire_lock(session_id).lock_owned().await;
        let mut location = self
            .session_locations
            .get(session_id)
            .map(|entry| entry.value().clone());
        if location.is_none() {
            // Resolve a session that another process created after this
            // instance built its index. If it is still absent, fail before
            // releasing the lookup boundary: returning an unfenced guard would
            // let a concurrent same-id creation appear between lookup and the
            // caller's first write.
            self.resolve_session_path(session_id).await?;
            location = self
                .session_locations
                .get(session_id)
                .map(|entry| entry.value().clone());
        }
        let location = location.ok_or_else(|| anyhow!("Chat session not found: {session_id}"))?;
        // Capture generation before waiting on the external lock. A
        // cross-process delete followed by deliberate same-id restore must not
        // let this already-started operation silently attach itself to the
        // replacement generation.
        let expected_generation = match load_chat_session_generation_marker(
            &self.workspace_layout,
            &location.principal,
            &location.workspace,
            session_id,
        )
        .await?
        {
            Some(marker) => marker.session_generation,
            None => {
                load_chat_session_authority_generation(
                    &self.workspace_layout,
                    &location.principal,
                    &location.workspace,
                    session_id,
                )
                .await?
            },
        };
        let lifecycle = acquire_chat_session_lifecycle_guard_for_scope(
            &self.workspace_layout,
            &location.principal,
            &location.workspace,
            session_id,
            Some(&expected_generation),
            false,
            false,
        )
        .await?;
        let authority_generation = load_chat_session_authority_generation(
            &self.workspace_layout,
            &location.principal,
            &location.workspace,
            session_id,
        )
        .await?;
        if authority_generation != expected_generation {
            return Err(anyhow!(
                "Chat session generation marker disagrees with persisted authority for {session_id}"
            ));
        }
        self.recover_clear_intent_locked(session_id).await?;
        self.recover_rollback_intent_locked(session_id).await?;
        self.recover_output_cleanup_intent_locked(session_id)
            .await?;
        Ok(ChatSessionWriteGuard {
            _local: local,
            _lifecycle: lifecycle,
        })
    }

    /// Get or create a per-scope mutex to serialize session creation.
    fn get_scope_lock(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
    ) -> Arc<Mutex<()>> {
        self.scope_locks
            .entry(SessionScope::new(principal, workspace, ui_thread_id))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn load_document(&self, session_id: &str) -> Result<ChatSessionDocument> {
        let path = self.resolve_session_path(session_id).await?;
        let metadata = self
            .workspace_layout
            .metadata_path(&path)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Chat session not found: {} (path={:?}, error={})",
                    session_id,
                    path,
                    e
                )
            })?
            .ok_or_else(|| anyhow!("Chat session not found: {session_id} (path={path:?})"))?;
        if metadata.len() > CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES {
            return Err(anyhow!(
                "chat session document exceeds the legacy migration byte limit"
            ));
        }
        let mut doc = if metadata.len() <= CHAT_SESSION_METADATA_FAST_PATH_BYTES {
            let bytes = self
                .workspace_layout
                .read_prefix_path(&path, metadata.len().saturating_add(1))
                .await?;
            let doc: ChatSessionDocument =
                deserialize_guarded_chat_bytes(&bytes, CHAT_SESSION_METADATA_FAST_PATH_BYTES)?;
            validate_session_document_shape(&doc)?;
            doc
        } else {
            tokio::task::spawn_blocking(move || -> Result<ChatSessionDocument> {
                let projection: ChatSessionFormatProjection =
                    read_guarded_chat_json_file(&path, CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES)?;
                if projection.format_version != 1 {
                    return Err(anyhow!(
                        "oversized or unsupported metadata-only chat session document"
                    ));
                }
                let doc: ChatSessionDocument =
                    read_guarded_chat_json_file(&path, CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES)?;
                validate_session_document_shape(&doc)?;
                Ok(doc)
            })
            .await
            .map_err(|error| anyhow!("chat session document worker failed: {error}"))??
        };
        doc.session = self.decorate_session(doc.session);
        Ok(doc)
    }

    async fn save_document(&self, doc: &ChatSessionDocument) -> Result<()> {
        if doc.format_version != CHAT_SESSION_DOCUMENT_FORMAT_VERSION
            || !doc.messages.is_empty()
            || !doc.llm_history.is_empty()
        {
            return Err(anyhow!(
                "chat session metadata writes require v2 with segmented messages and LLM history"
            ));
        }
        let path = self.workspace_layout.chat_session_path(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );

        self.workspace_layout
            .ensure_chat_session_workspace(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            )
            .await?;

        let bytes = serialize_json_bounded(
            doc,
            CHAT_SESSION_METADATA_FAST_PATH_BYTES as usize,
            "chat session metadata",
        )?;
        if crate::magician_v2::chat_owners::store_for_any_owner(&self.workspace_layout, &path)
            .is_some()
        {
            crate::magician_v2::chat_owners::persist_chat_file(
                &self.workspace_layout,
                &path,
                &bytes,
            )
            .await?;
        } else {
            self.workspace_layout
                .write_atomic_path(&path, &bytes)
                .await?;
        }
        let legacy_path = self.workspace_layout.legacy_chat_session_path(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );
        if legacy_path != path {
            match self.workspace_layout.remove_file_path(&legacy_path).await {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(err)) if err.kind() == ErrorKind::NotFound => {},
                Err(err) => {
                    warn!(
                        "[CHAT-STORE] Failed to remove legacy chat session file {:?}: {}",
                        legacy_path, err
                    );
                },
            }
        }
        self.session_locations.insert(
            doc.session.id.clone(),
            SessionLocation {
                principal: doc.session.principal.clone(),
                workspace: doc.session.workspace.clone(),
            },
        );
        Ok(())
    }

    async fn save_new_document_with_lifecycle(&self, doc: &ChatSessionDocument) -> Result<()> {
        let generation = chat_session_generation(&doc.session);
        let _lifecycle = acquire_chat_session_lifecycle_guard_for_scope(
            &self.workspace_layout,
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
            Some(&generation),
            true,
            false,
        )
        .await?;
        let expected = serialize_json_bounded(
            doc,
            CHAT_SESSION_METADATA_FAST_PATH_BYTES as usize,
            "new chat session metadata",
        )?;
        if let Err(error) = self.save_document(doc).await {
            let path = self.workspace_layout.chat_session_path(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            );
            match self
                .workspace_layout
                .read_prefix_path(&path, expected.len().saturating_add(1) as u64)
                .await
            {
                Ok(published) if published == expected => {
                    // The session document rename committed before its parent
                    // directory durability acknowledgement failed.
                    self.session_locations.insert(
                        doc.session.id.clone(),
                        SessionLocation {
                            principal: doc.session.principal.clone(),
                            workspace: doc.session.workspace.clone(),
                        },
                    );
                    warn!(
                        session_id = %doc.session.id,
                        error = %error,
                        "[CHAT-STORE] new-session publish reported an error after exact commit"
                    );
                    return Ok(());
                },
                Err(ArtifactV2Error::Io(read_error))
                    if read_error.kind() == ErrorKind::NotFound =>
                {
                    let generation = chat_session_generation(&doc.session);
                    if load_chat_session_generation_marker(
                        &self.workspace_layout,
                        &doc.session.principal,
                        &doc.session.workspace,
                        &doc.session.id,
                    )
                    .await?
                    .is_some_and(|marker| marker.session_generation == generation)
                    {
                        let generation_path =
                            self.workspace_layout.chat_session_generation_marker_path(
                                &doc.session.principal,
                                &doc.session.workspace,
                                &doc.session.id,
                            );
                        self.workspace_layout
                            .remove_file_path(&generation_path)
                            .await?;
                    }
                    return Err(error);
                },
                Ok(_) => {
                    return Err(anyhow!(
                        "new chat-session publication failed and exact readback found different bytes: {error}"
                    ));
                },
                Err(verification_error) => {
                    return Err(anyhow!(
                        "new chat-session publication is uncertain: {error}; readback failed: {verification_error}"
                    ));
                },
            }
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    async fn save_legacy_document_fixture(&self, doc: &ChatSessionDocument) -> Result<()> {
        if doc.format_version != 1 {
            return Err(anyhow!("legacy chat fixture must use format version 1"));
        }
        let path = self.workspace_layout.chat_session_path(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.id,
        );
        self.workspace_layout
            .ensure_chat_session_workspace(
                &doc.session.principal,
                &doc.session.workspace,
                &doc.session.id,
            )
            .await?;
        let bytes = serialize_json_bounded(
            doc,
            CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES as usize,
            "legacy chat session fixture",
        )?;
        self.workspace_layout
            .write_atomic_path(path, &bytes)
            .await?;
        Ok(())
    }

    async fn resolve_session_path(&self, session_id: &str) -> Result<PathBuf> {
        if let Some(path) = self.session_locations.get(session_id).map(|location| {
            self.workspace_layout.chat_session_path(
                &location.principal,
                &location.workspace,
                session_id,
            )
        }) {
            if self.workspace_layout.metadata_path(&path).await?.is_some() {
                return Ok(path);
            }
        }

        if let Some(path) = self.session_locations.get(session_id).map(|location| {
            self.workspace_layout.legacy_chat_session_path(
                &location.principal,
                &location.workspace,
                session_id,
            )
        }) {
            if self.workspace_layout.metadata_path(&path).await?.is_some() {
                return Ok(path);
            }
        }

        let scopes = self.workspace_layout.list_scope_segments().await?;
        for (principal, workspace) in scopes {
            let path = self
                .workspace_layout
                .chat_session_path(&principal, &workspace, session_id);
            if self.workspace_layout.metadata_path(&path).await?.is_some() {
                self.session_locations.insert(
                    session_id.to_string(),
                    SessionLocation {
                        principal,
                        workspace,
                    },
                );
                return Ok(path);
            }

            let legacy_path = self
                .workspace_layout
                .legacy_chat_session_path(&principal, &workspace, session_id);
            if self
                .workspace_layout
                .metadata_path(&legacy_path)
                .await?
                .is_some()
            {
                self.session_locations.insert(
                    session_id.to_string(),
                    SessionLocation {
                        principal,
                        workspace,
                    },
                );
                return Ok(legacy_path);
            }
        }

        Err(anyhow::anyhow!("Chat session not found: {}", session_id))
    }

    async fn resolve_deleted_session_location(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionLocation>> {
        if let Some(location) = self
            .session_locations
            .get(session_id)
            .map(|entry| entry.value().clone())
        {
            return Ok(Some(location));
        }
        let mut found = None;
        for (principal, workspace) in self.workspace_layout.list_scope_segments().await? {
            if load_chat_session_deletion_marker(
                &self.workspace_layout,
                &principal,
                &workspace,
                session_id,
            )
            .await?
            .is_none()
            {
                continue;
            }
            if found.is_some() {
                return Err(anyhow!(
                    "chat-session deletion marker is ambiguous across scopes"
                ));
            }
            found = Some(SessionLocation {
                principal,
                workspace,
            });
        }
        Ok(found)
    }

    fn create_session_inner(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        origin: &ChatChannel,
        agent_id: &str,
        history_lane: HistoryLane,
    ) -> ChatSession {
        let now = Utc::now().timestamp_millis();
        ChatSession {
            internal_voice: None,
            id: Uuid::new_v4().to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            agent_id: agent_id.to_string(),
            ui_thread_id: ui_thread_id.to_string(),
            title: None,
            origin_channel: origin.clone(),
            status: ChatSessionStatus::Active,
            history_lane,
            is_default_session: false,
            created_at: now,
            updated_at: now,
        }
    }

    fn synthesize_llm_history(messages: &[ChatMessage]) -> Vec<ChatLlmTranscriptEntry> {
        let mut history = Vec::new();
        for message in messages {
            if message.is_context_projection() {
                continue;
            }
            match (&message.direction, &message.content) {
                (
                    super::models::ChatMessageDirection::User,
                    ChatMessageContent::Text { text, .. },
                ) => {
                    history.push(ChatLlmTranscriptEntry::UserText { text: text.clone() });
                },
                (
                    super::models::ChatMessageDirection::Assistant,
                    ChatMessageContent::Text { text, .. },
                ) => {
                    history.push(ChatLlmTranscriptEntry::AssistantTurn {
                        text: Some(text.clone()),
                        tool_calls: Vec::new(),
                        provider_state: None,
                    });
                },
                _ => {},
            }
        }
        history
    }
}

#[async_trait]
impl ChatStore for FileChatStore {
    async fn upsert_voice_result_projection(&self, parent: &ChatSession, request: &VoiceRequest, message: ChatMessage) -> Result<bool> {
        self.upsert_voice_projection_inner(parent, request, message).await
    }

    async fn record_concurrent_voice_task(&self, session: &ChatSession, message: &ChatMessage) -> Result<()> {
        self.record_voice_task_update(session, message).await
    }

    async fn admit_voice_request(&self, parent_session_id: &str, admission: VoiceAdmission) -> Result<(VoiceRequest, bool)> {
        self.admit_voice_request_inner(parent_session_id, admission).await
    }

    async fn voice_state(&self, principal: &str, workspace: &str) -> Result<VoiceCoordinatorState> {
        self.load_voice_state(principal, workspace).await
    }

    async fn mutate_voice_state(&self, principal: &str, workspace: &str, mutation: VoiceMutation) -> Result<VoiceCoordinatorState> {
        self.mutate_voice_state_inner(principal, workspace, mutation).await
    }
    async fn get_or_create_active_session_with_history_lane(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        origin: &ChatChannel,
        agent_id: &str,
        history_lane: HistoryLane,
    ) -> Result<ChatSession> {
        // Hold per-scope lock for the entire check-and-create to prevent
        // two concurrent requests from both creating new sessions.
        let scope_lock = self.get_scope_lock(principal, workspace, ui_thread_id);
        let _scope_guard = scope_lock.lock().await;

        let active_session_ids = self
            .index
            .active_session_ids(principal, workspace, ui_thread_id);

        // Threads may contain several simultaneously active agent sessions.
        // Resolve by the exact requested agent; returning the newest unrelated
        // session lets a caller render one agent's history with another
        // agent's tool authority (notably realtime voice vs Loom).
        let mut matching_active_sessions = Vec::new();
        for session_id in &active_session_ids {
            let _guard = self.lock_session(session_id).await?;
            if let Ok(doc) = self.load_document(session_id).await {
                if doc.session.status == ChatSessionStatus::Active
                    && doc.session.agent_id == agent_id
                    && doc.session.effective_history_lane() == history_lane
                {
                    matching_active_sessions.push(doc.session);
                }
            }
        }

        // Rolling feature threads keep one active session per agent, not one
        // session across every agent sharing the thread.
        if thread_rotates_sessions(ui_thread_id) {
            for duplicate in matching_active_sessions.iter().skip(1) {
                let _ = self.update_session_status(&duplicate.id, "archived").await;
            }
        }

        if let Some(session) = matching_active_sessions.into_iter().next() {
            return Ok(session);
        }

        // No active session found — create one
        let is_default_session = ui_thread_id == "general"
            && self
                .index
                .default_session_id(principal, workspace)
                .is_none();
        let history_lane = if is_default_session {
            HistoryLane::Personal
        } else {
            history_lane
        };
        let mut session = self.create_session_inner(
            principal,
            workspace,
            ui_thread_id,
            origin,
            agent_id,
            history_lane,
        );
        session.is_default_session = is_default_session;
        let doc = ChatSessionDocument {
            format_version: CHAT_SESSION_DOCUMENT_FORMAT_VERSION,
            session: session.clone(),
            messages: Vec::new(),
            llm_history: Vec::new(),
        };

        self.save_new_document_with_lifecycle(&doc).await?;
        self.index.insert(
            principal,
            workspace,
            ui_thread_id,
            SessionIndexEntry {
                session_id: session.id.clone(),
                status: ChatSessionStatus::Active,
                created_at: session.created_at,
                updated_at: session.updated_at,
                title: session.title.clone(),
                agent_id: session.agent_id.clone(),
                history_lane: session.history_lane,
                is_default_session: session.is_default_session,
                is_concurrent: false,
            },
        );

        debug!(
            "[CHAT-STORE] Created new active session {} for principal {}",
            session.id, principal
        );
        analytics::emit(AnalyticsEvent::chat_session(
            &session.id,
            principal,
            workspace,
            agent_id,
            "active",
        ));
        Ok(session)
    }

    async fn new_session_with_history_lane(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: &str,
        origin: &ChatChannel,
        agent_id: &str,
        history_lane: HistoryLane,
    ) -> Result<ChatSession> {
        let scope_lock = self.get_scope_lock(principal, workspace, ui_thread_id);
        let _scope_guard = scope_lock.lock().await;

        // Feature/rotation threads keep one rolling session per agent. A
        // different agent sharing the same product thread is a separate
        // authority boundary and must not be archived as collateral damage.
        // User chat threads persist all sessions until archived manually.
        if thread_rotates_sessions(ui_thread_id) {
            let active_session_ids =
                self.index
                    .active_session_ids(principal, workspace, ui_thread_id);
            let mut matching_session_ids = Vec::new();
            for session_id in active_session_ids {
                let matches_agent_and_lane = {
                    let _guard = self.lock_session(&session_id).await?;
                    self.load_document(&session_id).await.is_ok_and(|doc| {
                        doc.session.agent_id == agent_id
                            && doc.session.effective_history_lane() == history_lane
                    })
                };
                if matches_agent_and_lane {
                    matching_session_ids.push(session_id);
                }
            }
            for duplicate_session_id in matching_session_ids {
                self.archive_session_status(&duplicate_session_id).await?;
            }
        }

        let is_default_session = ui_thread_id == "general"
            && self
                .index
                .default_session_id(principal, workspace)
                .is_none();
        let history_lane = if is_default_session {
            HistoryLane::Personal
        } else {
            history_lane
        };
        let mut session = self.create_session_inner(
            principal,
            workspace,
            ui_thread_id,
            origin,
            agent_id,
            history_lane,
        );
        session.is_default_session = is_default_session;
        let doc = ChatSessionDocument {
            format_version: CHAT_SESSION_DOCUMENT_FORMAT_VERSION,
            session: session.clone(),
            messages: Vec::new(),
            llm_history: Vec::new(),
        };

        self.save_new_document_with_lifecycle(&doc).await?;
        self.index.insert(
            principal,
            workspace,
            ui_thread_id,
            SessionIndexEntry {
                session_id: session.id.clone(),
                status: ChatSessionStatus::Active,
                created_at: session.created_at,
                updated_at: session.updated_at,
                title: session.title.clone(),
                agent_id: session.agent_id.clone(),
                history_lane: session.history_lane,
                is_default_session: session.is_default_session,
                is_concurrent: false,
            },
        );

        debug!(
            "[CHAT-STORE] Created fresh session {} for principal {} workspace {}",
            session.id, principal, workspace
        );
        analytics::emit(AnalyticsEvent::chat_session(
            &session.id,
            principal,
            workspace,
            agent_id,
            "active",
        ));
        Ok(session)
    }

    async fn get_session(&self, session_id: &str) -> Result<Option<ChatSession>> {
        let _guard = match self.lock_session(session_id).await {
            Ok(guard) => guard,
            Err(error) if chat_session_lifecycle_error_is_not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        match self.load_document(session_id).await {
            Ok(doc) => Ok(Some(doc.session)),
            Err(_) => Ok(None),
        }
    }

    async fn list_sessions(&self, principal: &str, workspace: &str) -> Result<Vec<ChatSession>> {
        let session_ids = self.index.list_sorted(principal, workspace);
        let mut sessions = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            let _guard = self.lock_session(&session_id).await?;
            if let Ok(doc) = self.load_document(&session_id).await {
                sessions.push(doc.session);
            }
        }
        Ok(sessions)
    }

    /// Index-only: no session lock and no document read.
    ///
    /// One deliberate widening against the `list_sessions` default: that path
    /// cannot report a session whose document is unreadable, this one reports
    /// the index entry regardless. Removal keeps the index in step with the
    /// files, so the two disagree only while an entry is stale — and reporting
    /// a thread the user has sessions in is the safer side of that race.
    ///
    /// "Cannot report", not "silently drops": `list_sessions` looks tolerant,
    /// but its `lock_session(..)?` validates the document before the
    /// `if let Ok(doc)` is reached, so a single unreadable session errors the
    /// whole scope's listing rather than costing that one row. Pinned by
    /// `thread_lanes_come_from_the_index_without_reading_documents`, which is
    /// why that test asserts the index path directly instead of contrasting the
    /// two.
    async fn list_session_thread_lanes(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, HistoryLane)>> {
        Ok(self.index.session_thread_lanes(principal, workspace))
    }

    async fn list_sessions_page(
        &self,
        principal: &str,
        workspace: &str,
        query: ChatSessionPageQuery,
    ) -> Result<ChatSessionPage> {
        let (session_ids, total) = self.index.list_page(principal, workspace, &query);
        let mut sessions = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            let _guard = self.lock_session(&session_id).await?;
            if let Ok(doc) = self.load_document(&session_id).await {
                sessions.push(doc.session);
            }
        }
        Ok(ChatSessionPage {
            sessions,
            total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    async fn search_session_candidates(
        &self,
        principal: &str,
        workspace: &str,
        search: &str,
    ) -> Result<Vec<ChatSessionSearchCandidate>> {
        Ok(self.index.search_candidates(principal, workspace, search))
    }

    async fn list_sessions_for_thread_prefix(
        &self,
        principal: &str,
        workspace: &str,
        thread_prefix: &str,
    ) -> Result<Vec<ChatSession>> {
        let session_ids =
            self.index
                .list_sorted_with_thread_prefix(principal, workspace, Some(thread_prefix));
        let mut sessions = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            let _guard = self.lock_session(&session_id).await?;
            if let Ok(doc) = self.load_document(&session_id).await {
                sessions.push(doc.session);
            }
        }
        Ok(sessions)
    }

    async fn list_active_sessions_for_agent(&self, agent_id: &str) -> Result<Vec<ChatSession>> {
        let session_ids = self
            .index
            .sessions
            .iter()
            .flat_map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|session| session.status == ChatSessionStatus::Active)
                    .map(|session| session.session_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let mut sessions = Vec::new();
        for session_id in session_ids {
            let _guard = self.lock_session(&session_id).await?;
            if let Ok(doc) = self.load_document(&session_id).await {
                if doc.session.status == ChatSessionStatus::Active
                    && doc.session.agent_id == agent_id
                {
                    sessions.push(doc.session);
                }
            }
        }
        Ok(sessions)
    }

    async fn update_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;
        doc.session.title = Some(title.to_string());
        doc.session.updated_at = Utc::now().timestamp_millis();
        self.save_document(&doc).await?;

        self.index.update_title(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            doc.session.title.clone(),
            doc.session.updated_at,
        );
        Ok(())
    }

    async fn update_session_status(&self, session_id: &str, status: &str) -> Result<()> {
        match status {
            "archived" => self.archive_session_status(session_id).await,
            _ => self.activate_session_status(session_id).await,
        }
    }

    async fn update_session_thread(&self, session_id: &str, ui_thread_id: &str) -> Result<()> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        if doc.session.internal_voice.is_some() { return Err(anyhow!("internal_voice_session_lifecycle_managed")); }
        if doc.session.is_default_session && ui_thread_id != "general" {
            return Err(anyhow!(
                "The default #general session cannot be moved to another thread"
            ));
        }

        self.ensure_segmented_document_for_write(&mut doc).await?;
        let old_thread = doc.session.ui_thread_id.clone();
        doc.session.ui_thread_id = ui_thread_id.to_string();
        doc.session.updated_at = Utc::now().timestamp_millis();
        self.save_document(&doc).await?;

        // Move in the index: remove from old thread scope, insert into new
        self.index.remove_session(
            &doc.session.principal,
            &doc.session.workspace,
            &old_thread,
            session_id,
        );
        self.index.insert(
            &doc.session.principal,
            &doc.session.workspace,
            ui_thread_id,
            SessionIndexEntry {
                session_id: session_id.to_string(),
                status: doc.session.status.clone(),
                created_at: doc.session.created_at,
                updated_at: doc.session.updated_at,
                title: doc.session.title.clone(),
                agent_id: doc.session.agent_id.clone(),
                history_lane: doc.session.effective_history_lane(),
                is_default_session: doc.session.is_default_session,
                is_concurrent: doc.session.internal_voice.is_some(),
            },
        );
        Ok(())
    }

    async fn delete_session(&self, session_id: &str) -> Result<()> {
        if self.index.is_default_session_id(session_id) {
            return Err(anyhow!("The default #general session cannot be deleted"));
        }
        let local_guard = self.acquire_lock(session_id).lock_owned().await;
        let archive_observer = self.archive_observer();
        let initial_doc = match self.load_document(session_id).await {
            Ok(doc) => doc,
            Err(load_error) => {
                // Missing deletion remains idempotent. An unreadable scoped
                // session is different. A previously published exact
                // tombstone authorizes completion of a partial delete; without
                // that proof we fail closed instead of guessing a generation.
                if let Some(location) = self.resolve_deleted_session_location(session_id).await? {
                    let session_path = self.workspace_layout.chat_session_path(
                        &location.principal,
                        &location.workspace,
                        session_id,
                    );
                    let legacy_path = self.workspace_layout.legacy_chat_session_path(
                        &location.principal,
                        &location.workspace,
                        session_id,
                    );
                    let deletion_marker = load_chat_session_deletion_marker(
                        &self.workspace_layout,
                        &location.principal,
                        &location.workspace,
                        session_id,
                    )
                    .await?;
                    let generation_marker = load_chat_session_generation_marker(
                        &self.workspace_layout,
                        &location.principal,
                        &location.workspace,
                        session_id,
                    )
                    .await?;
                    if let (Some(deletion_marker), Some(generation_marker)) =
                        (deletion_marker, generation_marker)
                    {
                        if deletion_marker.session_generation
                            == generation_marker.session_generation
                        {
                            let lifecycle_guard = acquire_chat_session_lifecycle_guard_for_scope(
                                &self.workspace_layout,
                                &location.principal,
                                &location.workspace,
                                session_id,
                                Some(&generation_marker.session_generation),
                                false,
                                true,
                            )
                            .await?;
                            let session_dir = self.workspace_layout.chat_session_dir(
                                &location.principal,
                                &location.workspace,
                                session_id,
                            );
                            match self
                                .workspace_layout
                                .remove_dir_all_path(&session_dir)
                                .await
                            {
                                Ok(()) => {},
                                Err(ArtifactV2Error::Io(error))
                                    if error.kind() == ErrorKind::NotFound => {},
                                Err(error) => return Err(error.into()),
                            }
                            match self.workspace_layout.remove_file_path(&legacy_path).await {
                                Ok(()) => {},
                                Err(ArtifactV2Error::Io(error))
                                    if error.kind() == ErrorKind::NotFound => {},
                                Err(error) => return Err(error.into()),
                            }
                            self.index.remove_session_id(session_id);
                            self.session_locations.remove(session_id);
                            drop(local_guard);
                            if let Some(observer) = archive_observer {
                                if let Err(error) = observer.on_session_archived(session_id).await {
                                    warn!(
                                        error = %error,
                                        session_id = %session_id,
                                        "[CHAT-STORE] failed post-recovery cleanup for partially deleted session"
                                    );
                                }
                            }
                            // Keep the generation fence until external lifecycle
                            // cleanup is complete. Otherwise a same-id restore can
                            // activate new subscriptions which this old-generation
                            // callback would immediately remove.
                            drop(lifecycle_guard);
                            self.session_locks
                                .remove_if(session_id, |_, lock| Arc::strong_count(lock) == 1);
                            return Ok(());
                        }
                    }
                    if self
                        .workspace_layout
                        .metadata_path(&session_path)
                        .await?
                        .is_some()
                        || self
                            .workspace_layout
                            .metadata_path(&legacy_path)
                            .await?
                            .is_some()
                    {
                        return Err(anyhow!(
                            "refusing to delete unreadable chat session {session_id} without a verified generation: {load_error}"
                        ));
                    }
                }
                self.session_locations.remove(session_id);
                drop(local_guard);
                self.session_locks
                    .remove_if(session_id, |_, lock| Arc::strong_count(lock) == 1);
                return Ok(());
            },
        };
        if initial_doc.session.internal_voice.is_some() { return Err(anyhow!("internal_voice_session_lifecycle_managed")); }
        if initial_doc.session.is_default_session {
            return Err(anyhow!("The default #general session cannot be deleted"));
        }
        let generation = chat_session_generation(&initial_doc.session);
        let lifecycle_guard = acquire_chat_session_lifecycle_guard_for_scope(
            &self.workspace_layout,
            &initial_doc.session.principal,
            &initial_doc.session.workspace,
            session_id,
            Some(&generation),
            false,
            true,
        )
        .await?;
        // The pre-lock document only locates the stable lock. Reload under the
        // fence so a deliberate same-id recreation cannot be deleted by an
        // older request that was waiting on the prior generation. A matching
        // marker plus a vanished tree is an idempotent concurrent delete.
        let doc = match self.load_document(session_id).await {
            Ok(doc) => doc,
            Err(error) => {
                let already_fenced = load_chat_session_deletion_marker(
                    &self.workspace_layout,
                    &initial_doc.session.principal,
                    &initial_doc.session.workspace,
                    session_id,
                )
                .await?
                .is_some_and(|marker| marker.session_generation == generation);
                if already_fenced {
                    initial_doc
                } else {
                    return Err(error);
                }
            },
        };
        if chat_session_generation(&doc.session) != generation {
            return Err(anyhow!(
                "Chat session generation changed while deletion was waiting"
            ));
        }
        if doc.session.is_default_session {
            return Err(anyhow!("The default #general session cannot be deleted"));
        }
        if let Err(error) = self.cleanup_chat_turn_event_files(&doc).await {
            warn!(
                error = %error,
                session_id = %session_id,
                "[CHAT-STORE] failed to clean up chat-turn event files"
            );
        }

        let rollback_authority = match self.resolve_session_path(session_id).await {
            Ok(authority_path) => match self.workspace_layout.metadata_path(&authority_path).await?
            {
                Some(metadata) if metadata.len() <= CHAT_SESSION_METADATA_FAST_PATH_BYTES => {
                    let bytes = self
                        .workspace_layout
                        .read_prefix_path(&authority_path, metadata.len().saturating_add(1))
                        .await?;
                    Some((authority_path, bytes))
                },
                _ => None,
            },
            Err(error) if error.to_string().contains("Chat session not found") => None,
            Err(error) => return Err(error),
        };

        let deletion_marker =
            publish_chat_session_deletion_marker(&self.workspace_layout, &doc.session).await?;
        let session_dir = self.workspace_layout.chat_session_dir(
            &doc.session.principal,
            &doc.session.workspace,
            session_id,
        );
        let legacy_path = self.workspace_layout.legacy_chat_session_path(
            &doc.session.principal,
            &doc.session.workspace,
            session_id,
        );
        let removal_result = async {
            match self
                .workspace_layout
                .remove_dir_all_path(&session_dir)
                .await
            {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
            match self.workspace_layout.remove_file_path(&legacy_path).await {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(error)) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
            Ok::<(), ArtifactV2Error>(())
        }
        .await;
        if let Err(error) = removal_result {
            let exact_authority_survives = match rollback_authority.as_ref() {
                Some((authority_path, expected)) => self
                    .workspace_layout
                    .read_prefix_path(
                        authority_path,
                        u64::try_from(expected.len().saturating_add(1)).unwrap_or(u64::MAX),
                    )
                    .await
                    .is_ok_and(|published| published.as_slice() == expected.as_slice()),
                None => false,
            };
            if exact_authority_survives {
                // Roll back only when byte-for-byte readback proves the same
                // session authority still exists. Directory existence alone
                // may be a partially removed tree with no session document.
                rollback_chat_session_deletion_marker(
                    &self.workspace_layout,
                    &doc.session,
                    &deletion_marker,
                )
                .await?;
                return Err(error.into());
            }
            let session_survives = self
                .workspace_layout
                .metadata_path(&session_dir)
                .await?
                .is_some()
                || self
                    .workspace_layout
                    .metadata_path(&legacy_path)
                    .await?
                    .is_some();
            if session_survives {
                warn!(
                    session_id = %session_id,
                    error = %error,
                    "[CHAT-STORE] partial session removal retained its deletion fence for retry"
                );
                return Err(error.into());
            }
            warn!(
                session_id = %session_id,
                error = %error,
                "[CHAT-STORE] session removal reported an error after the tree disappeared"
            );
        }

        self.index.remove_session(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
        );

        // Phase 3.5 — chat-pack-exec dirs are no longer created;
        // every chat-spawned pack runs inside a real V3 task. The archive
        // observer owns those external task records after this directory is
        // durably fenced and absent.

        self.session_locations.remove(session_id);
        drop(local_guard);
        if let Some(observer) = archive_observer {
            if let Err(error) = observer.on_session_archived(session_id).await {
                warn!(
                    error = %error,
                    session_id = %session_id,
                    "[CHAT-STORE] Failed to clean up deleted session"
                );
            }
        }
        // The observer removes identity-keyed external lifecycle state. Keep
        // the old generation fenced until that cleanup finishes so it cannot
        // erase state belonging to a deliberate same-id recreation.
        drop(lifecycle_guard);
        self.session_locks.remove_if(session_id, |_, lock| {
            // Do not split synchronization if another request already cloned
            // the Arc and is waiting to acquire this session lock.
            Arc::strong_count(lock) == 1
        });
        Ok(())
    }

    async fn append_message(&self, session_id: &str, msg: ChatMessage) -> Result<()> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;
        // Voice admission/result commits precede their live bus hints. A slow
        // sink or restart reconciliation may replay the same canonical message
        // after a segment rolls over; dedupe beyond the current tail segment.
        if msg.chat_turn_id.as_deref().is_some_and(|id| id.starts_with("voice-request-"))
            && self.segmented_messages_contain_any(&doc, &HashSet::from([msg.id.clone()])).await? {
            return Ok(());
        }
        let now = Utc::now().timestamp_millis();
        doc.session.updated_at = now;
        doc.messages.clear();

        // Emit analytics before moving msg into the vec
        let direction = format!("{:?}", msg.direction).to_lowercase();
        let content_text = match &msg.content {
            ChatMessageContent::Text { text, .. } => text.clone(),
            other => format!("{:?}", other),
        };
        analytics::emit(AnalyticsEvent::chat_message(
            session_id,
            &doc.session.principal,
            &doc.session.workspace,
            &direction,
            &content_text,
        ));

        let voice_task_message = (doc.session.internal_voice.is_some() && matches!(msg.content, ChatMessageContent::TaskStatusUpdate { .. })).then(|| msg.clone());
        self.append_segmented_message(&doc, msg).await?;
        self.save_document(&doc).await?;

        // Update index timestamp
        self.index.update_timestamp(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            now,
        );
        drop(_guard);
        if let Some(message) = voice_task_message {
            self.record_voice_task_update(&doc.session, &message).await?;
        }
        Ok(())
    }

    async fn delete_message(&self, session_id: &str, message_id: &str) -> Result<bool> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;

        // Capture the message body before it disappears so we can drop
        // any session outputs (attachments / tool-output images / pack
        // artifacts) it was the sole owner of. Walk inline first, then
        // segments; first match wins.
        let mut deleted_message: Option<ChatMessage> =
            doc.messages.iter().find(|m| m.id == message_id).cloned();
        if deleted_message.is_none() {
            'outer: for (_, path) in self.list_message_segments(&doc).await? {
                for message in self.load_message_segment(&path).await? {
                    if message.id == message_id {
                        deleted_message = Some(message);
                        break 'outer;
                    }
                }
            }
        }

        let removed = self
            .delete_segmented_messages(
                &doc,
                &HashSet::from_iter(std::iter::once(message_id.to_string())),
            )
            .await?;

        if !removed {
            return Ok(false);
        }

        // Outputs cleanup runs AFTER `delete_segmented_messages` so the
        // "still-referenced" walk sees the post-delete state on disk.
        if let Some(ref deleted) = deleted_message {
            if let Err(error) = self
                .cleanup_outputs_for_deleted_message(&doc, deleted)
                .await
            {
                warn!(
                    error = %error,
                    session_id = %session_id,
                    message_id = %message_id,
                    "[CHAT-STORE] failed to clean up outputs for deleted message"
                );
            }
        }

        let now = Utc::now().timestamp_millis();
        doc.session.updated_at = now;
        doc.messages.clear();
        self.save_document(&doc).await?;

        self.index.update_timestamp(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            now,
        );
        Ok(true)
    }

    async fn clear_messages(&self, session_id: &str) -> Result<usize> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;

        let intent = ChatClearIntent {
            format_version: CHAT_CLEAR_INTENT_FORMAT_VERSION,
            session_id: session_id.to_string(),
            updated_at: Utc::now().timestamp_millis(),
        };
        // Publishing the intent is the transaction start. Recovery completes
        // auxiliary cleanup, display deletion, transcript reset, and metadata
        // commit in that order under this same session lock.
        self.save_clear_intent(&doc, &intent).await?;

        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_clear_failure_if_configured(session_id, ChatClearFailpoint::AfterIntent)?;

        let total_cleared = self.complete_clear_intent_locked(&mut doc, &intent).await?;

        self.index.update_timestamp(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            intent.updated_at,
        );
        Ok(total_cleared)
    }

    async fn discard_unreferenced_screen_capture_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<bool> {
        let _guard = self.lock_session(session_id).await?;
        let doc = self.load_document(session_id).await?;
        let index = self.load_file_index_for_doc(&doc).await?;
        let Some(record) = index
            .files
            .iter()
            .find(|record| record.id == attachment_id)
            .cloned()
        else {
            return Ok(true);
        };
        if !matches!(record.origin, ChatSessionFileOrigin::Attachment)
            || !record
                .screen_capture
                .as_ref()
                .is_some_and(|context| context.server_registered)
        {
            return Ok(false);
        }
        for message in &doc.messages {
            if Self::referenced_stored_names(message, &index).contains(&record.stored_name) {
                return Ok(false);
            }
        }
        for (_, path) in self.list_message_segments(&doc).await? {
            for message in self.load_message_segment(&path).await? {
                if Self::referenced_stored_names(&message, &index).contains(&record.stored_name) {
                    return Ok(false);
                }
            }
        }
        self.cleanup_file_records_with_intent_locked(&doc, &[record])
            .await?;
        Ok(true)
    }

    async fn get_messages(&self, session_id: &str, limit: usize) -> Result<Vec<ChatMessage>> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;
        let (messages, _) = self
            .get_segmented_messages_paginated(&doc, limit, None)
            .await?;
        Ok(messages)
    }

    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        before_id: Option<&str>,
    ) -> Result<(Vec<ChatMessage>, bool)> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;
        self.get_segmented_messages_paginated(&doc, limit, before_id)
            .await
    }

    async fn list_thread_summaries_for_prefix(
        &self,
        principal: &str,
        workspace: &str,
        thread_prefix: &str,
    ) -> Result<Vec<ChatThreadSessionSummary>> {
        Ok(self
            .index
            .thread_summaries_with_prefix(principal, workspace, thread_prefix))
    }

    async fn get_messages_before_exact(
        &self,
        session_id: &str,
        limit: usize,
        before_id: Option<&str>,
    ) -> Result<Option<(Vec<ChatMessage>, bool)>> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;
        self.segmented_messages_before_exact(&doc, limit, before_id)
            .await
    }

    async fn append_llm_history_entries(
        &self,
        session_id: &str,
        mut entries: Vec<ChatLlmTranscriptEntry>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        if !Self::transcript_entries_have_bounded_json(&entries) {
            Self::discard_transcript_json_iteratively(&mut entries);
            return Err(anyhow!(
                "canonical chat transcript exceeds the retained-value contract for JSON depth or node count"
            ));
        }

        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;
        let now = Utc::now().timestamp_millis();
        doc.session.updated_at = now;
        let manifest = self.migrate_legacy_transcript_if_needed(&mut doc).await?;
        // Session metadata is auxiliary. Persist its timestamp before the
        // transcript manifest commit so a metadata write failure cannot make a
        // successful transcript append look failed and get retried/duplicated.
        self.save_document(&doc).await?;
        self.append_transcript_mutation(
            &doc,
            manifest,
            ChatTranscriptMutation::Append { entries },
            true,
        )
        .await?;

        self.index.update_timestamp(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            now,
        );
        Ok(())
    }

    async fn rollback_user_turn(&self, session_id: &str, message_ids: &[String]) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        if message_ids.len() > CHAT_ROLLBACK_MAX_MESSAGE_IDS
            || message_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > CHAT_ROLLBACK_MAX_MESSAGE_ID_BYTES)
        {
            return Err(anyhow!("chat rollback message-id set exceeds its limit"));
        }

        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        self.ensure_segmented_document_for_write(&mut doc).await?;
        let now = Utc::now().timestamp_millis();
        let message_id_set = message_ids.iter().cloned().collect::<HashSet<_>>();
        if !self
            .segmented_messages_contain_any(&doc, &message_id_set)
            .await?
        {
            return Ok(());
        }

        let manifest = self.migrate_legacy_transcript_if_needed(&mut doc).await?;
        let history = self
            .load_segmented_transcript_tail(&doc, &manifest, 1)
            .await?;
        let has_user_tail = match history.last() {
            Some(ChatLlmTranscriptEntry::UserTurn { .. })
            | Some(ChatLlmTranscriptEntry::UserText { .. }) => true,
            Some(other) => {
                warn!(
                    session_id = %session_id,
                    transcript_entry = ?other,
                    "[CHAT-STORAGE] rollback_user_turn found non-user transcript tail"
                );
                false
            },
            None => false,
        };
        if !has_user_tail {
            return Err(anyhow!(
                "chat rollback refused to delete display history without its canonical user transcript tail"
            ));
        }

        let intent = ChatRollbackIntent {
            format_version: CHAT_ROLLBACK_INTENT_FORMAT_VERSION,
            session_id: session_id.to_string(),
            message_ids: message_ids.to_vec(),
            transcript_generation: manifest.generation,
            base_last_sequence: manifest.last_sequence,
            base_effective_entry_count: manifest.effective_entry_count,
            updated_at: now,
        };
        self.save_rollback_intent(&doc, &intent).await?;

        #[cfg(any(test, feature = "test-fixtures"))]
        Self::inject_rollback_failure_if_configured(
            session_id,
            ChatRollbackFailpoint::AfterIntent,
        )?;

        self.complete_rollback_intent_locked(&mut doc, &intent)
            .await?;
        self.index.update_timestamp(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            now,
        );
        Ok(())
    }

    async fn get_llm_history(&self, session_id: &str) -> Result<Vec<ChatLlmTranscriptEntry>> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        let manifest = self.migrate_legacy_transcript_if_needed(&mut doc).await?;
        let history = self.load_segmented_transcript(&doc, &manifest).await?;
        if history.is_empty() {
            let messages = if doc.messages.is_empty() {
                self.load_all_segmented_messages(&doc).await?
            } else {
                doc.messages
            };
            Ok(Self::synthesize_llm_history(&messages))
        } else {
            Ok(history)
        }
    }

    async fn get_llm_history_tail(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<ChatLlmTranscriptEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        let manifest = self.migrate_legacy_transcript_if_needed(&mut doc).await?;
        let history = self
            .load_segmented_transcript_tail(&doc, &manifest, limit)
            .await?;
        if history.is_empty() {
            if doc.messages.is_empty() {
                self.synthesize_segmented_llm_history_tail(&doc, limit)
                    .await
            } else {
                // Legacy inline messages are already materialized by the
                // bounded legacy loader; preserve their historical synthesis
                // semantics until lazy migration clears the inline array.
                let synthesized = Self::synthesize_llm_history(&doc.messages);
                let synthesized_start = synthesized.len().saturating_sub(limit);
                Ok(synthesized.into_iter().skip(synthesized_start).collect())
            }
        } else {
            Ok(history)
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::chat::models::{
        AssistantProviderState, ChatLlmTranscriptEntry, ChatMessageContent, ChatMessageDirection,
    };
    use tokio::fs;
    use tokio::sync::{Mutex as TokioMutex, Notify};

    #[derive(Default)]
    struct RecordingArchiveObserver {
        archived_session_ids: TokioMutex<Vec<String>>,
        activated_session_ids: TokioMutex<Vec<String>>,
    }

    #[async_trait]
    impl ChatSessionArchiveObserver for RecordingArchiveObserver {
        async fn on_session_archived(&self, session_id: &str) -> Result<()> {
            self.archived_session_ids
                .lock()
                .await
                .push(session_id.to_string());
            Ok(())
        }

        async fn on_session_activated(&self, session: &ChatSession) -> Result<()> {
            self.activated_session_ids
                .lock()
                .await
                .push(session.id.clone());
            Ok(())
        }
    }

    struct BlockingArchiveObserver {
        entered: Notify,
        release: Notify,
    }

    #[async_trait]
    impl ChatSessionArchiveObserver for BlockingArchiveObserver {
        async fn on_session_archived(&self, _session_id: &str) -> Result<()> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(())
        }

        async fn on_session_activated(&self, _session: &ChatSession) -> Result<()> {
            Ok(())
        }
    }

    fn text_message(
        session_id: &str,
        id: impl Into<String>,
        text: impl Into<String>,
    ) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            session_id: session_id.to_string(),
            direction: ChatMessageDirection::User,
            content: ChatMessageContent::Text {
                text: text.into(),
                plan_reply: None,
            },
            created_at: 1_000,
            chat_turn_id: None,
            voice_origin: None,
            context_origin: None,
            speech_segments: None,
            source_surface: None,
            presence_session_id: None,
            presentation: None,
        }
    }

    fn test_output_record(
        id: impl Into<String>,
        stored_name: impl Into<String>,
    ) -> crate::magician_v2::chat::models::ChatSessionFileRecord {
        let stored_name = stored_name.into();
        crate::magician_v2::chat::models::ChatSessionFileRecord {
            id: id.into(),
            stored_name: stored_name.clone(),
            original_name: stored_name,
            mime_type: "application/octet-stream".to_string(),
            size: 7,
            label: None,
            screen_capture: None,
            prompt_image: false,
            origin: ChatSessionFileOrigin::ToolOutput {
                tool_name: "test-output".to_string(),
                tool_call_id: Some("call-test-output".to_string()),
            },
            source_task_output_id: None,
            source_task_id: None,
            created_at: 1_000,
        }
    }

    #[test]
    fn lifecycle_paths_are_stable_and_outside_the_deletable_session_tree() {
        let workspace = ArtifactV2Workspace::new("/tmp/chat-lifecycle-layout");
        let session_dir = workspace.chat_session_dir("user", "workspace", "session/unsafe");
        let lifecycle_lock =
            workspace.chat_session_lifecycle_lock_path("user", "workspace", "session/unsafe");
        let deletion_marker =
            workspace.chat_session_deletion_marker_path("user", "workspace", "session/unsafe");
        let generation_marker =
            workspace.chat_session_generation_marker_path("user", "workspace", "session/unsafe");

        assert!(!lifecycle_lock.starts_with(&session_dir));
        assert!(!deletion_marker.starts_with(&session_dir));
        assert!(!generation_marker.starts_with(&session_dir));
        assert_eq!(
            lifecycle_lock,
            workspace.chat_session_lifecycle_lock_path("user", "workspace", "session_unsafe"),
            "aliases of one physical session tree must share one lock inode"
        );
    }

    #[test]
    fn cleanup_output_names_are_single_bounded_file_components() {
        assert!(is_safe_chat_output_name("tool_output_123.json"));
        assert!(!is_safe_chat_output_name(""));
        assert!(!is_safe_chat_output_name("../outside"));
        assert!(!is_safe_chat_output_name("nested/output.json"));
        assert!(!is_safe_chat_output_name("/absolute.json"));
        assert!(!is_safe_chat_output_name("nul\0byte.json"));
        assert!(!is_safe_chat_output_name(&"x".repeat(256)));
    }

    #[test]
    fn lifecycle_error_mapping_does_not_hide_storage_failures_as_missing_owners() {
        assert!(chat_session_lifecycle_error_is_not_found(&anyhow!(
            "Chat session not found: gone"
        )));
        assert!(chat_session_lifecycle_error_is_not_found(&anyhow!(
            "Chat session generation changed for stale; refusing stale writer"
        )));
        assert!(!chat_session_lifecycle_error_is_not_found(&anyhow!(
            "lifecycle backend unavailable"
        )));
    }

    #[tokio::test]
    async fn canonical_path_aliases_share_lock_but_fail_exact_marker_identity_closed() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let now = Utc::now().timestamp_millis();
        let session = ChatSession {
            internal_voice: None,
            id: "session/alias".to_string(),
            principal: "user-1".to_string(),
            workspace: "workspace-a".to_string(),
            agent_id: "agent".to_string(),
            ui_thread_id: "alias-test".to_string(),
            title: None,
            origin_channel: ChatChannel::web(),
            status: ChatSessionStatus::Active,
            history_lane: HistoryLane::Personal,
            is_default_session: false,
            created_at: now,
            updated_at: now,
        };
        store
            .save_new_document_with_lifecycle(&ChatSessionDocument {
                format_version: CHAT_SESSION_DOCUMENT_FORMAT_VERSION,
                session: session.clone(),
                messages: Vec::new(),
                llm_history: Vec::new(),
            })
            .await
            .expect("canonical path owner");

        let mut alias = session;
        alias.id = "session_alias".to_string();
        assert_eq!(
            store.workspace_layout.chat_session_lifecycle_lock_path(
                &alias.principal,
                &alias.workspace,
                &alias.id,
            ),
            store.workspace_layout.chat_session_lifecycle_lock_path(
                &alias.principal,
                &alias.workspace,
                "session/alias",
            )
        );
        assert!(
            acquire_chat_session_lifecycle_guard(&store.workspace_layout, &alias)
                .await
                .is_err(),
            "the exact marker identity rejects a path alias"
        );
    }

    #[tokio::test]
    async fn session_mutation_rejects_a_generation_marker_that_disagrees_with_authority() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "generation-authority",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let marker = ChatSessionGenerationMarker {
            format_version: CHAT_SESSION_GENERATION_MARKER_FORMAT_VERSION,
            session_id: session.id.clone(),
            session_generation: format!("v1:{}:forged", session.id),
            published_at: Utc::now().timestamp_millis(),
        };
        store
            .workspace_layout
            .write_json_atomic_path(
                store.workspace_layout.chat_session_generation_marker_path(
                    &session.principal,
                    &session.workspace,
                    &session.id,
                ),
                &marker,
            )
            .await
            .expect("replace generation marker fixture");

        assert!(store
            .append_message(
                &session.id,
                text_message(&session.id, "late", "must not land")
            )
            .await
            .is_err());
        assert!(
            acquire_chat_session_lifecycle_guard_for_existing_scope(
                &store.workspace_layout,
                &session.principal,
                &session.workspace,
                &session.id,
            )
            .await
            .is_err(),
            "an output-only writer must also prove the persisted generation"
        );
        assert!(store
            .load_document(&session.id)
            .await
            .expect("persisted authority")
            .messages
            .is_empty());
    }

    #[tokio::test]
    async fn output_writer_rejects_a_generation_marker_without_session_authority() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "missing-generation-authority",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let session_dir = store.workspace_layout.chat_session_dir(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        store
            .workspace_layout
            .remove_dir_all_path(&session_dir)
            .await
            .expect("simulate missing authority without API tombstone");

        assert!(
            acquire_chat_session_lifecycle_guard(&store.workspace_layout, &session)
                .await
                .is_err(),
            "the generation marker alone must not authorize output publication"
        );
        assert!(store
            .workspace_layout
            .metadata_path(&session_dir)
            .await
            .expect("session directory metadata")
            .is_none());
    }

    #[tokio::test]
    async fn same_id_creation_cannot_replace_an_unmarked_legacy_generation() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let now = Utc::now().timestamp_millis();
        let old_session = ChatSession {
            internal_voice: None,
            id: "legacy-generation-owner".to_string(),
            principal: "user-1".to_string(),
            workspace: "workspace-a".to_string(),
            agent_id: "agent".to_string(),
            ui_thread_id: "legacy-generation".to_string(),
            title: None,
            origin_channel: ChatChannel::web(),
            status: ChatSessionStatus::Active,
            history_lane: HistoryLane::Personal,
            is_default_session: false,
            created_at: now,
            updated_at: now,
        };
        store
            .save_legacy_document_fixture(&ChatSessionDocument {
                format_version: 1,
                session: old_session.clone(),
                messages: Vec::new(),
                llm_history: Vec::new(),
            })
            .await
            .expect("unmarked legacy authority");

        let mut replacement = old_session;
        replacement.created_at = replacement.created_at.saturating_add(1);
        replacement.updated_at = replacement.created_at;
        assert!(store
            .save_new_document_with_lifecycle(&ChatSessionDocument {
                format_version: CHAT_SESSION_DOCUMENT_FORMAT_VERSION,
                session: replacement,
                messages: Vec::new(),
                llm_history: Vec::new(),
            })
            .await
            .is_err());
        assert_eq!(
            store
                .load_document("legacy-generation-owner")
                .await
                .expect("legacy authority retained")
                .session
                .created_at,
            now
        );
    }

    #[tokio::test]
    async fn deletion_fences_old_generation_but_allows_deliberate_same_id_recreation() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let old_session = store
            .new_session(
                "user-1",
                "workspace-a",
                "lifecycle-generation",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("old session");
        let old_doc = store.load_document(&old_session.id).await.expect("old doc");
        store
            .delete_session(&old_session.id)
            .await
            .expect("delete old generation");

        assert!(
            acquire_chat_session_lifecycle_guard(&store.workspace_layout, &old_session)
                .await
                .is_err(),
            "the deletion marker must reject late writers from the deleted generation"
        );
        assert!(store
            .workspace_layout
            .metadata_path(&store.workspace_layout.chat_session_dir(
                &old_session.principal,
                &old_session.workspace,
                &old_session.id,
            ))
            .await
            .expect("deleted path metadata")
            .is_none());

        let mut recreated_doc = old_doc;
        recreated_doc.session.created_at = recreated_doc.session.created_at.saturating_add(1);
        recreated_doc.session.updated_at = recreated_doc.session.created_at;
        store
            .save_new_document_with_lifecycle(&recreated_doc)
            .await
            .expect("publish a distinct restored generation");

        acquire_chat_session_lifecycle_guard(&store.workspace_layout, &recreated_doc.session)
            .await
            .expect("new generation is not blocked by the old tombstone");
        assert!(
            acquire_chat_session_lifecycle_guard(&store.workspace_layout, &old_session)
                .await
                .is_err(),
            "an old-generation writer must not enter the recreated session"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deletion_keeps_same_id_recreation_fenced_through_external_cleanup() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileChatStore::new(temp_dir.path()));
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "lifecycle-observer-order",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("old session");
        let old_doc = store
            .load_document(&session.id)
            .await
            .expect("old document");
        let observer = Arc::new(BlockingArchiveObserver {
            entered: Notify::new(),
            release: Notify::new(),
        });
        store.set_archive_observer(observer.clone());

        let deleting_store = store.clone();
        let deleting_session_id = session.id.clone();
        let deletion =
            tokio::spawn(async move { deleting_store.delete_session(&deleting_session_id).await });
        observer.entered.notified().await;

        let mut recreated_doc = old_doc;
        recreated_doc.session.created_at = recreated_doc.session.created_at.saturating_add(1);
        recreated_doc.session.updated_at = recreated_doc.session.created_at;
        let recreating_store = store.clone();
        let mut recreation = tokio::spawn(async move {
            recreating_store
                .save_new_document_with_lifecycle(&recreated_doc)
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut recreation)
                .await
                .is_err(),
            "same-id recreation must remain parked until old-generation cleanup returns"
        );

        observer.release.notify_one();
        deletion
            .await
            .expect("deletion task")
            .expect("old-generation deletion");
        recreation
            .await
            .expect("recreation task")
            .expect("new generation publication");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writer_waiting_across_deletion_cannot_resurrect_the_session_tree() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "lifecycle-race",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let deletion_guard =
            acquire_chat_session_lifecycle_guard(&store.workspace_layout, &session)
                .await
                .expect("deletion lifecycle guard");
        let waiting_workspace = store.workspace_layout.clone();
        let waiting_session = session.clone();
        let waiting_writer = tokio::spawn(async move {
            acquire_chat_session_lifecycle_guard(&waiting_workspace, &waiting_session).await
        });
        tokio::task::yield_now().await;

        publish_chat_session_deletion_marker(&store.workspace_layout, &session)
            .await
            .expect("deletion marker");
        let session_dir = store.workspace_layout.chat_session_dir(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        store
            .workspace_layout
            .remove_dir_all_path(&session_dir)
            .await
            .expect("remove session tree");
        drop(deletion_guard);

        assert!(waiting_writer.await.expect("waiting writer task").is_err());
        assert!(store
            .workspace_layout
            .metadata_path(&session_dir)
            .await
            .expect("session path metadata")
            .is_none());
    }

    #[tokio::test]
    async fn deletion_fails_closed_when_session_generation_cannot_be_verified() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "unreadable-delete",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let session_dir = store.workspace_layout.chat_session_dir(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        let session_path = store.workspace_layout.chat_session_path(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        store
            .workspace_layout
            .write_path(&session_path, b"not valid session JSON")
            .await
            .expect("corrupt fixture");

        let error = store
            .delete_session(&session.id)
            .await
            .expect_err("unreadable generation must not be partially deleted");
        assert!(error.to_string().contains("verified generation"));
        assert!(store
            .workspace_layout
            .metadata_path(&session_dir)
            .await
            .expect("retained session tree")
            .is_some());
    }

    #[tokio::test]
    async fn matching_tombstone_completes_a_partial_delete_after_authority_disappears() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "partial-delete-recovery",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let lifecycle = acquire_chat_session_lifecycle_guard(&store.workspace_layout, &session)
            .await
            .expect("lifecycle guard");
        publish_chat_session_deletion_marker(&store.workspace_layout, &session)
            .await
            .expect("deletion marker");
        let session_path = store.workspace_layout.chat_session_path(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        store
            .workspace_layout
            .remove_file_path(&session_path)
            .await
            .expect("simulate authority removal before directory cleanup");
        drop(lifecycle);

        store
            .delete_session(&session.id)
            .await
            .expect("matching tombstone completes partial delete");
        let session_dir = store.workspace_layout.chat_session_dir(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        assert!(store
            .workspace_layout
            .metadata_path(&session_dir)
            .await
            .expect("deleted tree metadata")
            .is_none());
        assert!(
            acquire_chat_session_lifecycle_guard(&store.workspace_layout, &session)
                .await
                .is_err(),
            "completed partial deletion keeps the old generation fenced"
        );
    }

    #[test]
    fn delete_rolls_back_only_after_exact_authority_readback() {
        let source = include_str!("storage.rs");
        let deletion = source
            .split_once("    async fn delete_session(&self, session_id: &str) -> Result<()> {")
            .expect("delete session implementation")
            .1
            .split_once("    async fn append_message(")
            .expect("delete session implementation end")
            .0;
        let exact_readback = deletion
            .find("exact_authority_survives")
            .expect("exact authority readback");
        let rollback = deletion
            .find("rollback_chat_session_deletion_marker")
            .expect("deletion-marker rollback");
        let partial_fence = deletion
            .find("partial session removal retained its deletion fence")
            .expect("partial deletion fence retention");
        assert!(exact_readback < rollback && rollback < partial_fence);
        assert!(!deletion.contains("if session_survives {\n                rollback"));

        let creation = source
            .split_once("    async fn save_new_document_with_lifecycle(")
            .expect("new-session transaction")
            .1
            // The attribute, not the exact `#[cfg(test)]`: the crate split
            // rewrote it to `#[cfg(any(test, feature = "test-fixtures"))]`, and
            // the old literal then matched nothing.
            .split_once("    async fn save_legacy_document_fixture")
            .expect("new-session transaction end")
            .0;
        let create_readback = creation
            .find("read_prefix_path")
            .expect("new-session exact readback");
        let generation_rollback = creation
            .find("remove_file_path(&generation_path)")
            .expect("new-session generation rollback");
        assert!(create_readback < generation_rollback);
        assert!(creation.contains("published == expected"));
    }

    #[tokio::test]
    async fn active_sessions_are_scoped_by_workspace() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let web_session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("workspace-a session");
        let telegram_session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-b",
                "general",
                &ChatChannel::new("telegram", "42"),
                "agent",
            )
            .await
            .expect("workspace-b session");
        let repeated_web_session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("repeat workspace-a session");

        assert_eq!(repeated_web_session.id, web_session.id);
        assert_ne!(telegram_session.id, web_session.id);

        let workspace_a_sessions = store
            .list_sessions("user-1", "workspace-a")
            .await
            .expect("list workspace-a sessions");
        let workspace_b_sessions = store
            .list_sessions("user-1", "workspace-b")
            .await
            .expect("list workspace-b sessions");

        assert_eq!(workspace_a_sessions.len(), 1);
        assert_eq!(workspace_a_sessions[0].id, web_session.id);
        assert_eq!(workspace_b_sessions.len(), 1);
        assert_eq!(workspace_b_sessions[0].id, telegram_session.id);
    }

    #[tokio::test]
    async fn new_session_keeps_existing_active_across_workspaces() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let original_a = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("workspace-a session");
        let original_b = store
            .get_or_create_active_session(
                "user-1",
                "workspace-b",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("workspace-b session");

        let fresh_a = store
            .new_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("fresh workspace-a session");

        assert_ne!(fresh_a.id, original_a.id);

        let workspace_a_sessions = store
            .list_sessions("user-1", "workspace-a")
            .await
            .expect("list workspace-a sessions");
        let workspace_b_sessions = store
            .list_sessions("user-1", "workspace-b")
            .await
            .expect("list workspace-b sessions");

        // Option B: "general" is a user chat thread — new_session does NOT archive the
        // prior session, so original_a stays active alongside the fresh session.
        assert!(workspace_a_sessions
            .iter()
            .any(|session| session.id == original_a.id
                && session.status == ChatSessionStatus::Active));
        assert!(
            workspace_a_sessions
                .iter()
                .any(|session| session.id == fresh_a.id
                    && session.status == ChatSessionStatus::Active)
        );

        // workspace-b untouched
        assert_eq!(workspace_b_sessions.len(), 1);
        assert_eq!(workspace_b_sessions[0].id, original_b.id);
        assert_eq!(workspace_b_sessions[0].status, ChatSessionStatus::Active);
    }

    #[tokio::test]
    async fn active_sessions_are_scoped_by_thread_within_workspace() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let general = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("general session");
        let travel = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "travel",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("travel session");
        let repeated_general = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("repeat general session");

        assert_eq!(repeated_general.id, general.id);
        assert_ne!(travel.id, general.id);

        let workspace_sessions = store
            .list_sessions("user-1", "workspace-a")
            .await
            .expect("list workspace sessions");

        assert_eq!(workspace_sessions.len(), 2);
        assert!(workspace_sessions.iter().any(|session| {
            session.id == general.id
                && session.ui_thread_id == "general"
                && session.status == ChatSessionStatus::Active
        }));
        assert!(workspace_sessions.iter().any(|session| {
            session.id == travel.id
                && session.ui_thread_id == "travel"
                && session.status == ChatSessionStatus::Active
        }));
    }

    #[tokio::test]
    async fn update_session_status_archives_and_updates_index() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let session = store
            .get_or_create_active_session(
                "user-1",
                "ws-1",
                "project-x",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();

        assert_eq!(session.status, ChatSessionStatus::Active);
        assert!(store
            .index
            .active_session_ids("user-1", "ws-1", "project-x")
            .into_iter()
            .next()
            .is_some());

        // Archive
        store
            .update_session_status(&session.id, "archived")
            .await
            .unwrap();
        let reloaded = store.get_session(&session.id).await.unwrap().unwrap();
        assert_eq!(reloaded.status, ChatSessionStatus::Archived);
        // Index must reflect archived — find_active should return None
        assert!(store
            .index
            .active_session_ids("user-1", "ws-1", "project-x")
            .into_iter()
            .next()
            .is_none());

        // Restore
        store
            .update_session_status(&session.id, "active")
            .await
            .unwrap();
        let restored = store.get_session(&session.id).await.unwrap().unwrap();
        assert_eq!(restored.status, ChatSessionStatus::Active);
    }

    #[tokio::test]
    async fn update_session_thread_moves_between_scopes() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let session = store
            .get_or_create_active_session(
                "user-1",
                "ws-1",
                "project-source",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();

        assert_eq!(session.ui_thread_id, "project-source");

        store
            .update_session_thread(&session.id, "project-x")
            .await
            .unwrap();
        let moved = store.get_session(&session.id).await.unwrap().unwrap();
        assert_eq!(moved.ui_thread_id, "project-x");
    }

    #[tokio::test]
    async fn delete_session_removes_file_and_index() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let session = store
            .get_or_create_active_session(
                "user-1",
                "ws-1",
                "project-x",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();

        assert!(store.get_session(&session.id).await.unwrap().is_some());
        store.delete_session(&session.id).await.unwrap();
        assert!(store.get_session(&session.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn new_session_keeps_existing_active_thread_session() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let first = store
            .get_or_create_active_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();

        let second = store
            .new_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();

        assert_ne!(first.id, second.id);

        // Option B: an explicit "New Chat" no longer archives the prior session — both
        // stay active until the user archives one manually.
        let first_reloaded = store.get_session(&first.id).await.unwrap().unwrap();
        let second_reloaded = store.get_session(&second.id).await.unwrap().unwrap();
        assert_eq!(first_reloaded.status, ChatSessionStatus::Active);
        assert_eq!(second_reloaded.status, ChatSessionStatus::Active);
    }

    #[tokio::test]
    async fn brainstorming_thread_keeps_each_idea_session_active_until_user_action() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let first = store
            .new_session(
                "user-1",
                "ws-1",
                "brainstorming",
                &ChatChannel::new("contextual-assist", "app:ios:thinking-map"),
                "brainstorm-facilitator",
            )
            .await
            .unwrap();
        let second = store
            .new_session(
                "user-1",
                "ws-1",
                "brainstorming",
                &ChatChannel::new("contextual-assist", "app:ios:thinking-map"),
                "brainstorm-facilitator",
            )
            .await
            .unwrap();

        assert_ne!(first.id, second.id);
        assert!(!thread_rotates_sessions("brainstorming"));
        assert_eq!(
            store.get_session(&first.id).await.unwrap().unwrap().status,
            ChatSessionStatus::Active
        );
        assert_eq!(
            store.get_session(&second.id).await.unwrap().unwrap().status,
            ChatSessionStatus::Active
        );
    }

    #[tokio::test]
    async fn new_session_rotates_only_matching_agent_in_feature_thread() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        // Feature/rotation threads (e.g. "screens") keep a single rolling
        // session per exact agent, unlike user chat threads.
        let first = store
            .get_or_create_active_session(
                "user-1",
                "ws-1",
                "screens",
                &ChatChannel::web(),
                "agent-a",
            )
            .await
            .unwrap();
        let sibling = store
            .new_session("user-1", "ws-1", "screens", &ChatChannel::web(), "agent-b")
            .await
            .unwrap();
        let replacement = store
            .new_session("user-1", "ws-1", "screens", &ChatChannel::web(), "agent-a")
            .await
            .unwrap();

        assert_ne!(first.id, replacement.id);
        let first_reloaded = store.get_session(&first.id).await.unwrap().unwrap();
        let sibling_reloaded = store.get_session(&sibling.id).await.unwrap().unwrap();
        let replacement_reloaded = store.get_session(&replacement.id).await.unwrap().unwrap();
        assert_eq!(first_reloaded.status, ChatSessionStatus::Archived);
        assert_eq!(sibling_reloaded.status, ChatSessionStatus::Active);
        assert_eq!(replacement_reloaded.status, ChatSessionStatus::Active);
    }

    #[tokio::test]
    async fn restoring_feature_session_preserves_other_agent_sibling() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let first = store
            .new_session("user-1", "ws-1", "screens", &ChatChannel::web(), "agent-a")
            .await
            .unwrap();
        let sibling = store
            .new_session("user-1", "ws-1", "screens", &ChatChannel::web(), "agent-b")
            .await
            .unwrap();
        store
            .update_session_status(&first.id, "archived")
            .await
            .unwrap();
        store
            .update_session_status(&first.id, "active")
            .await
            .unwrap();

        assert_eq!(
            store.get_session(&first.id).await.unwrap().unwrap().status,
            ChatSessionStatus::Active
        );
        assert_eq!(
            store
                .get_session(&sibling.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            ChatSessionStatus::Active
        );
    }

    /// A room must never inherit the owner's chat session.
    ///
    /// Resume and compaction read whatever session the call is bound to, so if
    /// a room could attach to the owner's session on a shared thread id it
    /// would compact the owner's history into a room — the exact leak the
    /// outward boundary exists to stop, arriving through history rather than
    /// through a tool.
    ///
    /// The protection is that resolution requires an EXACT agent match, which
    /// is easy to mistake for a caching detail and "simplify" away. This test
    /// is here so that fails loudly instead of silently mixing provenance.
    /// A room rejoining the same meeting picks up where it left off.
    ///
    /// This is the whole recall story for a rejoin, and it rests on three
    /// separate things holding at once — which is why it is pinned end to end
    /// rather than by unit-testing each: the meeting thread id is derived
    /// deterministically from url + title + date, so a rejoin names the SAME
    /// thread; `get_or_create_active_session` reuses a session on an exact
    /// agent match, so the same thread hands back the SAME session; and
    /// transcript lines are persisted as display messages rather than merely
    /// broadcast, so that session still holds what was said.
    ///
    /// Break any one and a rejoining bot silently starts blank — with every
    /// other test still green, because each piece is individually fine.
    #[tokio::test]
    async fn a_room_rejoining_the_same_meeting_still_holds_its_own_prior_transcript() {
        use crate::magician_v2::media_seam::meeting_session::derive_meeting_thread_id;

        let temp = tempfile::tempdir().expect("temp dir");
        let store = FileChatStore::new(temp.path());

        let thread = derive_meeting_thread_id(
            "https://meet.google.com/abc-defg-hij",
            Some("Weekly sync"),
            "2026-08-18",
        );

        // First join: the transcript streams into the room's session.
        let first = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                &thread,
                &ChatChannel::web(),
                "envoy",
            )
            .await
            .expect("first join session");
        store
            .append_message(
                &first.id,
                text_message(&first.id, "line-1", "We agreed to ship on Friday."),
            )
            .await
            .expect("transcript line persists");

        // The bot drops and rejoins the SAME meeting.
        let rejoined = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                &derive_meeting_thread_id(
                    "https://meet.google.com/abc-defg-hij",
                    Some("Weekly sync"),
                    "2026-08-18",
                ),
                &ChatChannel::web(),
                "envoy",
            )
            .await
            .expect("rejoin session");

        assert_eq!(
            rejoined.id, first.id,
            "a rejoin opened a NEW session, so the room would start blank"
        );

        let history = store
            .get_messages(&rejoined.id, 10)
            .await
            .expect("history readable after rejoin");
        assert!(
            history.iter().any(|message| message
                .content
                .text_content()
                .is_some_and(|text| text.contains("ship on Friday"))),
            "the rejoining room could not see what was said before it dropped"
        );
    }

    /// A meeting that runs past midnight, or a bot that drops at 23:58 and
    /// rejoins at 00:02, must land in the occurrence that holds its
    /// transcript.
    ///
    /// The failure this pins is the one the dated thread id causes: the
    /// calendar rolls over, the derived thread changes, `get_or_create_active_
    /// session` honestly creates a session for the new thread, and the room
    /// starts blank with every existing test still green.
    ///
    /// Driven through the store rather than through the pure decision, because
    /// the decision is only worth anything if the candidates it reads are the
    /// real ones: the liveness it keys on is the session index's `updated_at`,
    /// and a helper that gathered the wrong threads would agree with itself.
    #[tokio::test]
    async fn a_room_rejoining_across_midnight_keeps_the_transcript_it_already_has() {
        use crate::magician_v2::media_seam::meeting_session::derive_meeting_thread_id;

        let temp = tempfile::tempdir().expect("temp dir");
        let store = FileChatStore::new(temp.path());

        let yesterday = derive_meeting_thread_id(
            "https://meet.google.com/abc-defg-hij",
            Some("All hands"),
            "2026-08-18",
        );
        let today = derive_meeting_thread_id(
            "https://meet.google.com/abc-defg-hij",
            Some("All hands"),
            "2026-08-19",
        );
        assert_ne!(
            yesterday, today,
            "the calendar must still produce two thread ids — this test is \
             about surviving that, not about removing it"
        );

        let opened = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                &yesterday,
                &ChatChannel::web(),
                "envoy",
            )
            .await
            .expect("first join session");
        store
            .append_message(
                &opened.id,
                text_message(&opened.id, "line-1", "We agreed to ship on Friday."),
            )
            .await
            .expect("transcript line persists");

        // 00:02, four minutes after the last thing anyone said.
        let rejoined = store
            .resolve_meeting_room_session(
                "anonymous",
                "default",
                &today,
                &ChatChannel::web(),
                "envoy",
                Utc::now() + chrono::Duration::minutes(4),
            )
            .await
            .expect("rejoin resolves");

        assert_eq!(
            rejoined.session.id, opened.id,
            "the rejoin opened a NEW room across the date boundary, so the bot \
             started blank holding a live meeting's transcript one thread away"
        );
        assert_eq!(rejoined.thread, yesterday);
        assert_eq!(rejoined.continued_from_thread, Some(yesterday.clone()));

        let history = store
            .get_messages(&rejoined.session.id, 10)
            .await
            .expect("history readable after rejoin");
        assert!(
            history.iter().any(|message| message
                .content
                .text_content()
                .is_some_and(|text| text.contains("ship on Friday"))),
            "the room that crossed midnight could not see what was said before it"
        );
    }

    /// The containment half of the same mechanism, and the reason the grace is
    /// short: the next occurrence of a recurring meeting is a different
    /// gathering, and merging them would put one day's participants in front
    /// of another day's transcript.
    #[tokio::test]
    async fn tomorrows_occurrence_does_not_inherit_todays_room() {
        use crate::magician_v2::media_seam::meeting_session::derive_meeting_thread_id;

        let temp = tempfile::tempdir().expect("temp dir");
        let store = FileChatStore::new(temp.path());

        let today = derive_meeting_thread_id("", Some("Standup"), "2026-08-18");
        let tomorrow = derive_meeting_thread_id("", Some("Standup"), "2026-08-19");

        let opened = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                &today,
                &ChatChannel::web(),
                "envoy",
            )
            .await
            .expect("today session");
        store
            .append_message(
                &opened.id,
                text_message(&opened.id, "line-1", "The acquisition price is 4 crore."),
            )
            .await
            .expect("transcript line persists");

        // A day later, well past the rejoin grace.
        let next = store
            .resolve_meeting_room_session(
                "anonymous",
                "default",
                &tomorrow,
                &ChatChannel::web(),
                "envoy",
                Utc::now() + chrono::Duration::hours(23),
            )
            .await
            .expect("next occurrence resolves");

        assert_ne!(
            next.session.id, opened.id,
            "a recurring meeting's next occurrence inherited the previous one's room"
        );
        assert_eq!(next.thread, tomorrow);
        assert_eq!(next.continued_from_thread, None);
        let history = store
            .get_messages(&next.session.id, 10)
            .await
            .expect("history readable");
        assert!(
            history.is_empty(),
            "the next occurrence started with the previous gathering's transcript"
        );
    }

    /// A session archived between drop and rejoin must not strand the room's
    /// own transcript.
    ///
    /// The transcript endpoint refuses archived sessions deliberately — that
    /// is what makes the sink re-resolve to the thread's CURRENT active
    /// session — so the archived session keeps everything that was said and
    /// the rejoin opens a blank one beside it. Silently blank is the failure
    /// being avoided; the fix carries the transcript forward rather than
    /// reviving the archived session, which is terminal.
    #[tokio::test]
    async fn a_rejoin_after_archival_carries_the_stranded_transcript_forward() {
        use crate::magician_v2::media_seam::meeting_session::derive_meeting_thread_id;

        let temp = tempfile::tempdir().expect("temp dir");
        let store = FileChatStore::new(temp.path());
        let thread = derive_meeting_thread_id("", Some("Board review"), "2026-08-18");

        let first = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                &thread,
                &ChatChannel::web(),
                "envoy",
            )
            .await
            .expect("first join session");
        store
            .append_message(
                &first.id,
                text_message(&first.id, "line-1", "We agreed to ship on Friday."),
            )
            .await
            .expect("transcript line persists");
        store
            .update_session_status(&first.id, "archived")
            .await
            .expect("session archived between drop and rejoin");

        let rejoined = store
            .resolve_meeting_room_session(
                "anonymous",
                "default",
                &thread,
                &ChatChannel::web(),
                "envoy",
                Utc::now(),
            )
            .await
            .expect("rejoin resolves");

        assert_ne!(
            rejoined.session.id, first.id,
            "the archived session was revived; archival is terminal and reviving \
             it re-opens a session the transcript sink was told to stop writing to"
        );
        assert_eq!(
            rejoined.carried_from_session,
            Some(first.id.clone()),
            "the rejoin did not name the predecessor it read, so a blank room \
             would be indistinguishable from an empty meeting"
        );
        assert_eq!(rejoined.carried_message_count, 1);

        let history = store
            .get_messages(&rejoined.session.id, 10)
            .await
            .expect("history readable after rejoin");
        assert!(
            history.iter().any(|message| message
                .content
                .text_content()
                .is_some_and(|text| text.contains("ship on Friday"))),
            "the rejoin stranded the transcript in the archived session"
        );

        // The archived record is left exactly as it was.
        assert_eq!(
            store
                .get_session(&first.id)
                .await
                .expect("archived session load")
                .expect("archived session present")
                .status,
            ChatSessionStatus::Archived
        );

        // Idempotent: resolving again finds history and copies nothing more.
        let again = store
            .resolve_meeting_room_session(
                "anonymous",
                "default",
                &thread,
                &ChatChannel::web(),
                "envoy",
                Utc::now(),
            )
            .await
            .expect("second rejoin resolves");
        assert_eq!(again.session.id, rejoined.session.id);
        assert_eq!(
            again.carried_message_count, 0,
            "a second rejoin duplicated the carried transcript"
        );
        assert_eq!(
            store
                .get_messages(&again.session.id, 50)
                .await
                .expect("history readable")
                .len(),
            1
        );
    }

    /// The same binding is what keeps a rejoin from reaching a DIFFERENT
    /// meeting: a different meeting derives a different thread, so its session
    /// and transcript are unreachable.
    #[tokio::test]
    async fn a_rejoin_reaches_only_the_meeting_it_rejoined() {
        use crate::magician_v2::media_seam::meeting_session::derive_meeting_thread_id;

        let temp = tempfile::tempdir().expect("temp dir");
        let store = FileChatStore::new(temp.path());

        let mine = derive_meeting_thread_id(
            "https://meet.google.com/aaa-aaaa-aaa",
            Some("My sync"),
            "2026-08-18",
        );
        let other = derive_meeting_thread_id(
            "https://meet.google.com/bbb-bbbb-bbb",
            Some("Someone else's sync"),
            "2026-08-18",
        );
        assert_ne!(mine, other, "two meetings must not share a binding");

        let other_session = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                &other,
                &ChatChannel::web(),
                "envoy",
            )
            .await
            .expect("other meeting session");
        store
            .append_message(
                &other_session.id,
                text_message(
                    &other_session.id,
                    "secret",
                    "The acquisition price is 4 crore.",
                ),
            )
            .await
            .expect("other transcript line");

        let my_session = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                &mine,
                &ChatChannel::web(),
                "envoy",
            )
            .await
            .expect("my session");

        assert_ne!(my_session.id, other_session.id);
        let history = store
            .get_messages(&my_session.id, 10)
            .await
            .expect("history");
        assert!(
            !history.iter().any(|message| message
                .content
                .text_content()
                .is_some_and(|text| text.contains("acquisition price"))),
            "one meeting's room read another meeting's transcript"
        );
    }

    #[tokio::test]
    async fn a_room_agent_never_inherits_the_owner_session_on_the_same_thread() {
        let temp = tempfile::tempdir().expect("temp dir");
        let store = FileChatStore::new(temp.path());

        // The owner's voice session on a thread.
        let owner = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                "shared-thread",
                &ChatChannel::web(),
                "personal-assistant",
            )
            .await
            .expect("owner session");

        // The ambassador asking for the SAME thread must not receive it.
        let room = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                "shared-thread",
                &ChatChannel::web(),
                "envoy",
            )
            .await
            .expect("room session");

        assert_ne!(
            room.id, owner.id,
            "a room inherited the owner's chat session, so resume/compaction \
             would replay owner history into a room"
        );
        assert_eq!(room.agent_id, "envoy");

        // And the reverse: the owner must not be handed the room's session.
        let owner_again = store
            .get_or_create_active_session(
                "anonymous",
                "default",
                "shared-thread",
                &ChatChannel::web(),
                "personal-assistant",
            )
            .await
            .expect("owner session again");
        assert_eq!(owner_again.id, owner.id, "the owner lost their own session");
        assert_ne!(owner_again.id, room.id);
    }

    #[tokio::test]
    async fn get_or_create_active_session_matches_agent_and_preserves_siblings() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let first = store
            .new_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent-a")
            .await
            .unwrap();
        let second = store
            .new_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent-b")
            .await
            .unwrap();

        let resolved = store
            .get_or_create_active_session(
                "user-1",
                "ws-1",
                "general",
                &ChatChannel::web(),
                "agent-c",
            )
            .await
            .unwrap();

        assert_ne!(resolved.id, first.id);
        assert_ne!(resolved.id, second.id);
        assert_eq!(resolved.agent_id, "agent-c");

        let first_reloaded = store.get_session(&first.id).await.unwrap().unwrap();
        let second_reloaded = store.get_session(&second.id).await.unwrap().unwrap();
        assert_eq!(first_reloaded.status, ChatSessionStatus::Active);
        assert_eq!(second_reloaded.status, ChatSessionStatus::Active);
    }

    #[tokio::test]
    async fn resolving_user_thread_creates_exact_agent_without_implicit_archive() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let observer = Arc::new(RecordingArchiveObserver::default());
        store.set_archive_observer(observer.clone());

        let first = store
            .new_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent-a")
            .await
            .unwrap();
        let second = store
            .new_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent-b")
            .await
            .unwrap();

        let resolved = store
            .get_or_create_active_session(
                "user-1",
                "ws-1",
                "general",
                &ChatChannel::web(),
                "agent-c",
            )
            .await
            .unwrap();

        assert_ne!(resolved.id, first.id);
        assert_ne!(resolved.id, second.id);
        assert_eq!(resolved.agent_id, "agent-c");
        assert!(observer.archived_session_ids.lock().await.is_empty());
        assert_eq!(
            store.get_session(&first.id).await.unwrap().unwrap().status,
            ChatSessionStatus::Active
        );
    }

    #[tokio::test]
    async fn restore_user_session_preserves_other_active_session_and_notifies_observer() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let observer = Arc::new(RecordingArchiveObserver::default());
        store.set_archive_observer(observer.clone());

        let current = store
            .get_or_create_active_session(
                "user-1",
                "ws-1",
                "general",
                &ChatChannel::web(),
                "agent-a",
            )
            .await
            .unwrap();
        let archived = store
            .new_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent-b")
            .await
            .unwrap();
        store
            .update_session_status(&archived.id, "archived")
            .await
            .unwrap();
        observer.archived_session_ids.lock().await.clear();
        observer.activated_session_ids.lock().await.clear();

        store
            .update_session_status(&archived.id, "active")
            .await
            .unwrap();

        let current_reloaded = store.get_session(&current.id).await.unwrap().unwrap();
        let archived_reloaded = store.get_session(&archived.id).await.unwrap().unwrap();
        assert_eq!(current_reloaded.status, ChatSessionStatus::Active);
        assert_eq!(archived_reloaded.status, ChatSessionStatus::Active);
        assert!(observer.archived_session_ids.lock().await.is_empty());
        assert_eq!(
            observer.activated_session_ids.lock().await.as_slice(),
            &[archived.id]
        );
    }

    #[tokio::test]
    async fn delete_session_notifies_archive_observer() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let observer = Arc::new(RecordingArchiveObserver::default());
        store.set_archive_observer(observer.clone());

        let session = store
            .get_or_create_active_session(
                "user-1",
                "ws-1",
                "project-x",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();

        store.delete_session(&session.id).await.unwrap();

        assert_eq!(
            observer.archived_session_ids.lock().await.as_slice(),
            &[session.id]
        );
    }

    #[tokio::test]
    async fn delete_message_removes_only_that_message_from_segments() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let session = store
            .get_or_create_active_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();

        for id in ["msg-1", "msg-2", "msg-3"] {
            store
                .append_message(
                    &session.id,
                    ChatMessage {
                        id: id.to_string(),
                        session_id: session.id.clone(),
                        direction: ChatMessageDirection::User,
                        content: ChatMessageContent::Text {
                            text: id.to_string(),
                            plan_reply: None,
                        },
                        created_at: 1000,
                        chat_turn_id: None,
                        voice_origin: None,
                        context_origin: None,
                        speech_segments: None,
                        source_surface: None,
                        presence_session_id: None,
                        presentation: None,
                    },
                )
                .await
                .unwrap();
        }

        let doc = store.load_document(&session.id).await.unwrap();
        assert!(doc.messages.is_empty());
        assert_eq!(store.list_message_segments(&doc).await.unwrap().len(), 1);

        assert!(store.delete_message(&session.id, "msg-2").await.unwrap());
        assert!(!store.delete_message(&session.id, "missing").await.unwrap());

        let messages = store.get_messages(&session.id, 20).await.unwrap();
        let message_ids = messages
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(message_ids, vec!["msg-1", "msg-3"]);
    }

    #[tokio::test]
    async fn paginated_messages_cross_segment_boundaries() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let session = store
            .get_or_create_active_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();

        for i in 0..7 {
            store
                .append_message(
                    &session.id,
                    ChatMessage {
                        id: format!("msg-{}", i),
                        session_id: session.id.clone(),
                        direction: ChatMessageDirection::User,
                        content: ChatMessageContent::Text {
                            text: format!("message {}", i),
                            plan_reply: None,
                        },
                        created_at: 1000 + i as i64,
                        chat_turn_id: None,
                        voice_origin: None,
                        context_origin: None,
                        speech_segments: None,
                        source_surface: None,
                        presence_session_id: None,
                        presentation: None,
                    },
                )
                .await
                .unwrap();
        }

        let doc = store.load_document(&session.id).await.unwrap();
        assert!(doc.messages.is_empty());
        assert_eq!(store.list_message_segments(&doc).await.unwrap().len(), 3);

        let (newest, has_more) = store
            .get_messages_paginated(&session.id, 4, None)
            .await
            .unwrap();
        let newest_ids = newest
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(newest_ids, vec!["msg-3", "msg-4", "msg-5", "msg-6"]);
        assert!(has_more);

        let (older, older_has_more) = store
            .get_messages_paginated(&session.id, 2, Some("msg-3"))
            .await
            .unwrap();
        let older_ids = older
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(older_ids, vec!["msg-1", "msg-2"]);
        assert!(older_has_more);

        assert!(store.delete_message(&session.id, "msg-5").await.unwrap());
        let (after_delete, _) = store
            .get_messages_paginated(&session.id, 4, None)
            .await
            .unwrap();
        let after_delete_ids = after_delete
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(after_delete_ids, vec!["msg-2", "msg-3", "msg-4", "msg-6"]);
    }

    #[tokio::test]
    async fn get_messages_paginated_returns_newest_and_has_more() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());

        let session = store
            .get_or_create_active_session("user-1", "ws-1", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();

        // Add 5 messages
        for i in 0..5 {
            store
                .append_message(
                    &session.id,
                    ChatMessage {
                        id: format!("msg-{}", i),
                        session_id: session.id.clone(),
                        direction: ChatMessageDirection::User,
                        content: ChatMessageContent::Text {
                            text: format!("message {}", i),
                            plan_reply: None,
                        },
                        created_at: 1000 + i as i64,
                        chat_turn_id: None,
                        voice_origin: None,
                        context_origin: None,
                        speech_segments: None,
                        source_surface: None,
                        presence_session_id: None,
                        presentation: None,
                    },
                )
                .await
                .unwrap();
        }

        // Get newest 3 (no cursor)
        let (msgs, has_more) = store
            .get_messages_paginated(&session.id, 3, None)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].id, "msg-2");
        assert_eq!(msgs[2].id, "msg-4");
        assert!(has_more);

        // Get 3 before msg-2 (the oldest from previous page)
        let (older, has_more_2) = store
            .get_messages_paginated(&session.id, 3, Some("msg-2"))
            .await
            .unwrap();
        assert_eq!(older.len(), 2); // only msg-0 and msg-1
        assert_eq!(older[0].id, "msg-0");
        assert_eq!(older[1].id, "msg-1");
        assert!(!has_more_2);

        // Get all (large limit, no cursor)
        let (all, has_more_3) = store
            .get_messages_paginated(&session.id, 100, None)
            .await
            .unwrap();
        assert_eq!(all.len(), 5);
        assert!(!has_more_3);

        // Nonexistent cursor falls back to end
        let (fallback, _) = store
            .get_messages_paginated(&session.id, 3, Some("nonexistent"))
            .await
            .unwrap();
        assert_eq!(fallback.len(), 3);
        assert_eq!(fallback[2].id, "msg-4");
    }

    #[tokio::test]
    async fn get_llm_history_synthesizes_segmented_message_history_when_transcript_missing() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");

        store
            .append_message(
                &session.id,
                ChatMessage {
                    id: Uuid::new_v4().to_string(),
                    session_id: session.id.clone(),
                    direction: ChatMessageDirection::User,
                    content: ChatMessageContent::Text {
                        text: "legacy user".to_string(),
                        plan_reply: None,
                    },
                    created_at: Utc::now().timestamp_millis(),
                    chat_turn_id: None,
                    voice_origin: None,
                    context_origin: None,
                    speech_segments: None,
                    source_surface: None,
                    presence_session_id: None,
                    presentation: None,
                },
            )
            .await
            .expect("append user");
        store
            .append_message(
                &session.id,
                ChatMessage {
                    id: Uuid::new_v4().to_string(),
                    session_id: session.id.clone(),
                    direction: ChatMessageDirection::Assistant,
                    content: ChatMessageContent::Text {
                        text: "legacy assistant".to_string(),
                        plan_reply: None,
                    },
                    created_at: Utc::now().timestamp_millis(),
                    chat_turn_id: None,
                    voice_origin: None,
                    context_origin: None,
                    speech_segments: None,
                    source_surface: None,
                    presence_session_id: None,
                    presentation: None,
                },
            )
            .await
            .expect("append assistant");

        let synthesized = store
            .get_llm_history(&session.id)
            .await
            .expect("synthesized history");
        assert!(matches!(
            &synthesized[0],
            ChatLlmTranscriptEntry::UserText { text } if text == "legacy user"
        ));
        assert!(matches!(
            &synthesized[1],
            ChatLlmTranscriptEntry::AssistantTurn { text, tool_calls, .. }
                if text.as_deref() == Some("legacy assistant") && tool_calls.is_empty()
        ));

        store
            .append_llm_history_entries(
                &session.id,
                vec![ChatLlmTranscriptEntry::AssistantTurn {
                    text: Some("materialized assistant".to_string()),
                    tool_calls: Vec::new(),
                    provider_state: Some(AssistantProviderState::OpenaiResponses {
                        response_id: "clean-checkpoint".to_string(),
                        tool_protocol_repair_checkpoint: true,
                    }),
                }],
            )
            .await
            .expect("append llm history");

        let materialized = store
            .get_llm_history(&session.id)
            .await
            .expect("materialized history");
        assert_eq!(materialized.len(), 1);
        assert!(matches!(
            &materialized[0],
            ChatLlmTranscriptEntry::AssistantTurn {
                text,
                tool_calls,
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id,
                    tool_protocol_repair_checkpoint: true,
                }),
            } if text.as_deref() == Some("materialized assistant")
                && tool_calls.is_empty()
                && response_id == "clean-checkpoint"
        ));
    }

    #[tokio::test]
    async fn rollback_intent_recovers_after_each_cross_file_commit_boundary() {
        let _serial = CHAT_ROLLBACK_TEST_SERIAL
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for phase in [
            ChatRollbackFailpoint::AfterIntent,
            ChatRollbackFailpoint::AfterDisplayDeletion,
            ChatRollbackFailpoint::AfterTranscriptCommit,
        ] {
            let temp_dir = tempfile::tempdir().expect("tempdir");
            let store = FileChatStore::new(temp_dir.path());
            let session = store
                .new_session(
                    "user-1",
                    "workspace-a",
                    "rollback-crash",
                    &ChatChannel::web(),
                    "agent",
                )
                .await
                .expect("session");
            let message_id = format!("rollback-message-{phase:?}");
            store
                .append_message(
                    &session.id,
                    text_message(&session.id, &message_id, "rejected user turn"),
                )
                .await
                .expect("display message");
            store
                .append_llm_history_entries(
                    &session.id,
                    vec![
                        ChatLlmTranscriptEntry::AssistantTurn {
                            text: Some("retained assistant context".to_string()),
                            tool_calls: Vec::new(),
                            provider_state: None,
                        },
                        ChatLlmTranscriptEntry::UserText {
                            text: "rejected user turn".to_string(),
                        },
                    ],
                )
                .await
                .expect("canonical transcript");
            *CHAT_ROLLBACK_FAILPOINT
                .get_or_init(|| StdMutex::new(None))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((session.id.clone(), phase));

            store
                .rollback_user_turn(&session.id, std::slice::from_ref(&message_id))
                .await
                .expect_err("failpoint simulates process loss");
            drop(store);

            let restarted = FileChatStore::with_index(temp_dir.path())
                .await
                .expect("restart");
            assert!(restarted
                .get_messages(&session.id, 10)
                .await
                .expect("display recovery")
                .is_empty());
            let history = restarted
                .get_llm_history(&session.id)
                .await
                .expect("transcript recovery");
            assert_eq!(history.len(), 1, "rollback must truncate exactly once");
            assert!(matches!(
                &history[0],
                ChatLlmTranscriptEntry::AssistantTurn { text: Some(text), .. }
                    if text == "retained assistant context"
            ));
            let doc = restarted.load_document(&session.id).await.unwrap();
            assert!(restarted
                .load_rollback_intent(&doc)
                .await
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn clear_intent_recovers_after_each_cross_file_commit_boundary() {
        let _serial = CHAT_CLEAR_TEST_SERIAL
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for phase in [
            ChatClearFailpoint::AfterIntent,
            ChatClearFailpoint::BeforeOutputCleanup,
            ChatClearFailpoint::AfterDisplayDeletion,
            ChatClearFailpoint::AfterTranscriptReset,
        ] {
            let temp_dir = tempfile::tempdir().expect("tempdir");
            let store = FileChatStore::new(temp_dir.path());
            let session = store
                .new_session(
                    "user-1",
                    "workspace-a",
                    "clear-crash",
                    &ChatChannel::web(),
                    "agent",
                )
                .await
                .expect("session");
            store
                .append_message(
                    &session.id,
                    text_message(&session.id, "clear-message", "clear me"),
                )
                .await
                .expect("display message");
            store
                .append_llm_history_entries(
                    &session.id,
                    vec![ChatLlmTranscriptEntry::UserText {
                        text: "clear me".to_string(),
                    }],
                )
                .await
                .expect("canonical transcript");
            *CHAT_CLEAR_FAILPOINT
                .get_or_init(|| StdMutex::new(None))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((session.id.clone(), phase));

            store
                .clear_messages(&session.id)
                .await
                .expect_err("failpoint simulates process loss");
            drop(store);

            let restarted = FileChatStore::with_index(temp_dir.path())
                .await
                .expect("restart");
            assert!(restarted
                .get_messages(&session.id, 10)
                .await
                .expect("display recovery")
                .is_empty());
            assert!(restarted
                .get_llm_history(&session.id)
                .await
                .expect("transcript recovery")
                .is_empty());
            let doc = restarted.load_document(&session.id).await.unwrap();
            assert!(restarted.load_clear_intent(&doc).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn output_cleanup_intent_recovers_after_every_commit_boundary() {
        let _serial = CHAT_OUTPUT_CLEANUP_TEST_SERIAL
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for phase in [
            ChatOutputCleanupFailpoint::AfterIntent,
            ChatOutputCleanupFailpoint::AfterIndexPublish,
            ChatOutputCleanupFailpoint::BeforeIntentRemoval,
        ] {
            let temp_dir = tempfile::tempdir().expect("tempdir");
            let store = FileChatStore::new(temp_dir.path());
            let session = store
                .new_session(
                    "user-1",
                    "workspace-a",
                    "output-cleanup-crash",
                    &ChatChannel::web(),
                    "agent",
                )
                .await
                .expect("session");
            let doc = store.load_document(&session.id).await.expect("document");
            let record =
                test_output_record(format!("record-{phase:?}"), format!("output-{phase:?}.bin"));
            let output_path = store
                .workspace_layout
                .chat_session_outputs_dir(&session.principal, &session.workspace, &session.id)
                .join(&record.stored_name);
            store
                .workspace_layout
                .write_path(&output_path, b"payload")
                .await
                .expect("output bytes");
            store
                .save_file_index_for_doc(
                    &doc,
                    &ChatSessionFileIndex {
                        files: vec![record.clone()],
                    },
                )
                .await
                .expect("output index");
            *CHAT_OUTPUT_CLEANUP_FAILPOINT
                .get_or_init(|| StdMutex::new(None))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((session.id.clone(), phase));

            let lifecycle = acquire_chat_session_lifecycle_guard(&store.workspace_layout, &session)
                .await
                .expect("lifecycle guard");
            store
                .cleanup_file_records_with_intent_locked(&doc, std::slice::from_ref(&record))
                .await
                .expect_err("failpoint simulates process loss");
            assert!(store
                .load_output_cleanup_intent(&doc)
                .await
                .expect("pending intent")
                .is_some());
            drop(lifecycle);
            drop(store);

            let restarted = FileChatStore::with_index(temp_dir.path())
                .await
                .expect("restart");
            restarted
                .get_messages(&session.id, 1)
                .await
                .expect("trigger idempotent cleanup recovery");
            let recovered_doc = restarted
                .load_document(&session.id)
                .await
                .expect("recovered document");
            assert!(restarted
                .load_file_index_for_doc(&recovered_doc)
                .await
                .expect("recovered index")
                .files
                .iter()
                .all(|candidate| candidate.id != record.id));
            assert!(restarted
                .workspace_layout
                .metadata_path(&output_path)
                .await
                .expect("output metadata")
                .is_none());
            assert!(restarted
                .load_output_cleanup_intent(&recovered_doc)
                .await
                .expect("completed intent")
                .is_none());
        }
    }

    #[tokio::test]
    async fn physical_unlink_failure_keeps_bounded_intent_until_retry_succeeds() {
        let _serial = CHAT_OUTPUT_CLEANUP_TEST_SERIAL
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "output-cleanup-unlink",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let doc = store.load_document(&session.id).await.expect("document");
        let record = test_output_record("unlink-record", "unlink-target.bin");
        let output_path = store
            .workspace_layout
            .chat_session_outputs_dir(&session.principal, &session.workspace, &session.id)
            .join(&record.stored_name);
        fs::create_dir_all(&output_path)
            .await
            .expect("directory at unlink target");
        let child_path = output_path.join("child");
        fs::write(&child_path, b"keep directory non-empty")
            .await
            .expect("directory child");
        store
            .save_file_index_for_doc(
                &doc,
                &ChatSessionFileIndex {
                    files: vec![record.clone()],
                },
            )
            .await
            .expect("output index");

        let lifecycle = acquire_chat_session_lifecycle_guard(&store.workspace_layout, &session)
            .await
            .expect("lifecycle guard");
        store
            .cleanup_file_records_with_intent_locked(&doc, std::slice::from_ref(&record))
            .await
            .expect_err("a non-file unlink target must remain pending");
        assert!(store
            .load_file_index_for_doc(&doc)
            .await
            .expect("metadata-first index")
            .files
            .is_empty());
        assert!(store
            .load_output_cleanup_intent(&doc)
            .await
            .expect("durable pending intent")
            .is_some());
        drop(lifecycle);
        drop(store);

        fs::remove_file(&child_path).await.expect("remove child");
        fs::remove_dir(&output_path)
            .await
            .expect("make retry idempotently absent");
        let restarted = FileChatStore::with_index(temp_dir.path())
            .await
            .expect("restart");
        restarted
            .get_messages(&session.id, 1)
            .await
            .expect("retry pending cleanup");
        let recovered_doc = restarted.load_document(&session.id).await.unwrap();
        assert!(restarted
            .load_output_cleanup_intent(&recovered_doc)
            .await
            .expect("completed intent")
            .is_none());
    }

    #[tokio::test]
    async fn output_cleanup_completes_without_unlinking_a_reused_live_name() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "output-cleanup-live-name",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let doc = store.load_document(&session.id).await.expect("document");
        let target = test_output_record("old-row", "shared-output.bin");
        let live = test_output_record("live-row", "shared-output.bin");
        let output_path = store
            .workspace_layout
            .chat_session_outputs_dir(&session.principal, &session.workspace, &session.id)
            .join(&target.stored_name);
        store
            .workspace_layout
            .write_path(&output_path, b"live bytes")
            .await
            .expect("shared output");
        store
            .save_file_index_for_doc(
                &doc,
                &ChatSessionFileIndex {
                    files: vec![target.clone(), live.clone()],
                },
            )
            .await
            .expect("shared-name index");

        let lifecycle = acquire_chat_session_lifecycle_guard(&store.workspace_layout, &session)
            .await
            .expect("lifecycle guard");
        store
            .cleanup_file_records_with_intent_locked(&doc, &[target])
            .await
            .expect("metadata-only shared-name cleanup");
        drop(lifecycle);

        assert_eq!(
            store
                .load_file_index_for_doc(&doc)
                .await
                .expect("live index")
                .files,
            vec![live]
        );
        assert!(store
            .workspace_layout
            .metadata_path(&output_path)
            .await
            .expect("shared bytes metadata")
            .is_some());
        assert!(store
            .load_output_cleanup_intent(&doc)
            .await
            .expect("completed cleanup intent")
            .is_none());
    }

    #[tokio::test]
    async fn output_cleanup_intent_rejects_unsafe_paths_and_unbounded_item_sets() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "output-cleanup-admission",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let doc = store.load_document(&session.id).await.expect("document");
        let mut intent = ChatOutputCleanupIntent {
            format_version: CHAT_OUTPUT_CLEANUP_INTENT_FORMAT_VERSION,
            session_id: session.id.clone(),
            session_generation: chat_session_generation(&session),
            items: vec![ChatOutputCleanupItem {
                record_id: "unsafe-record".to_string(),
                stored_name: "../outside-session".to_string(),
            }],
            updated_at: Utc::now().timestamp_millis(),
        };
        assert!(store
            .save_output_cleanup_intent(&doc, &intent)
            .await
            .is_err());

        intent.items = (0..=CHAT_OUTPUT_CLEANUP_INTENT_MAX_ITEMS)
            .map(|index| ChatOutputCleanupItem {
                record_id: format!("record-{index}"),
                stored_name: format!("output-{index}.bin"),
            })
            .collect();
        assert!(store
            .save_output_cleanup_intent(&doc, &intent)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn clearing_an_already_empty_session_preserves_its_timestamp() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "empty-clear",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");

        assert_eq!(store.clear_messages(&session.id).await.expect("clear"), 0);
        let restored = store
            .get_session(&session.id)
            .await
            .expect("session lookup")
            .expect("session remains");
        assert_eq!(restored.updated_at, session.updated_at);
        let doc = store.load_document(&session.id).await.expect("session doc");
        assert!(store.load_clear_intent(&doc).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn transcript_control_documents_reject_oversize_before_typed_decode() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "bounded-control-documents",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let doc = store.load_document(&session.id).await.expect("session doc");

        store
            .workspace_layout
            .write_atomic_path(
                store.clear_intent_path(&doc),
                &vec![b' '; CHAT_CLEAR_INTENT_MAX_BYTES + 1],
            )
            .await
            .expect("oversized clear intent fixture");
        assert!(store.load_clear_intent(&doc).await.is_err());

        store
            .workspace_layout
            .write_atomic_path(
                store.rollback_intent_path(&doc),
                &vec![b' '; CHAT_ROLLBACK_INTENT_MAX_BYTES + 1],
            )
            .await
            .expect("oversized rollback intent fixture");
        assert!(store.load_rollback_intent(&doc).await.is_err());

        store
            .workspace_layout
            .write_atomic_path(
                store.transcript_manifest_path(&doc),
                &vec![b' '; CHAT_TRANSCRIPT_MAX_MANIFEST_BYTES + 1],
            )
            .await
            .expect("oversized manifest fixture");
        assert!(store.load_transcript_manifest(&doc).await.is_err());
    }

    #[tokio::test]
    async fn bounded_segment_read_admits_exact_limit_and_rejects_an_oversized_replacement() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let path = temp_dir.path().join("bounded-segment-read.jsonl");
        workspace
            .write_atomic_path(&path, &[b'x'; 32])
            .await
            .expect("exact-limit fixture");
        assert_eq!(
            read_optional_bounded_chat_path(&workspace, &path, 32, "test segment")
                .await
                .expect("exact limit admitted")
                .map(|bytes| bytes.len()),
            Some(32)
        );

        workspace
            .write_atomic_path(&path, &[b'y'; 33])
            .await
            .expect("oversized atomic replacement");
        assert!(
            read_optional_bounded_chat_path(&workspace, &path, 32, "test segment")
                .await
                .is_err()
        );
    }

    #[test]
    fn transcript_control_and_segment_loaders_use_the_race_checked_stream_reader() {
        let source = include_str!("storage.rs");
        for (start, end) in [
            (
                "    async fn load_transcript_manifest(",
                "    async fn save_transcript_manifest(",
            ),
            (
                "    async fn load_rollback_intent(",
                "    async fn save_rollback_intent(",
            ),
            (
                "    async fn load_clear_intent(",
                "    async fn save_clear_intent(",
            ),
            (
                "    async fn load_file_index_for_doc(",
                "    async fn save_file_index_for_doc(",
            ),
            (
                "    async fn transcript_has_committed_rollback(",
                "    /// Complete an already-published rollback transaction.",
            ),
            (
                "    async fn load_segmented_transcript(",
                "    async fn load_segmented_transcript_tail(",
            ),
            (
                "    async fn load_segmented_transcript_tail(",
                "    async fn migrate_legacy_messages_if_needed(",
            ),
        ] {
            let body = source
                .split_once(start)
                .expect("bounded chat loader start")
                .1
                .split_once(end)
                .expect("bounded chat loader end")
                .0;
            assert!(body.contains("read_json_bounded_stream_path"));
            assert!(!body.contains(".read_to_string_path("));
            assert!(!body.contains(".metadata_path("));
        }

        let workspace_source = include_str!("../artifact_v2/workspace.rs");
        let reader = workspace_source
            .split_once("    pub async fn read_json_bounded_stream_path")
            .expect("bounded workspace reader")
            .1
            .split_once("    pub fn write_atomic_path_sync")
            .expect("bounded workspace reader end")
            .0;
        assert!(reader.contains("admitted_hash"));
        assert!(reader.contains("changed during bounded decode"));

        for (start, end) in [(
            "    async fn load_message_segment(",
            "    async fn save_message_segment(",
        )] {
            let body = source
                .split_once(start)
                .expect("bounded segmented payload loader")
                .1
                .split_once(end)
                .expect("bounded segmented payload loader end")
                .0;
            assert!(body.contains("read_optional_bounded_chat_path("));
            assert!(!body.contains(".read_to_string_path("));
            assert!(!body.contains(".metadata_path("));
        }
    }

    #[test]
    fn output_cleanup_keeps_index_and_file_mutations_in_one_locked_transaction() {
        let source = include_str!("storage.rs");
        let deleted_message_cleanup = source
            .split_once("    async fn cleanup_outputs_for_deleted_message(")
            .expect("deleted-message output cleanup")
            .1
            .split_once("    /// Remove every regular file under")
            .expect("deleted-message output cleanup end")
            .0;
        assert!(deleted_message_cleanup.contains("load_file_index_for_doc"));
        assert!(deleted_message_cleanup.contains("cleanup_file_records_with_intent_locked"));

        let durable_cleanup = source
            .split_once("    async fn cleanup_file_records_with_intent_locked(")
            .expect("durable output cleanup")
            .1
            .split_once("    /// Compute the set of `stored_name`s")
            .expect("durable output cleanup end")
            .0;
        let delete_lock = durable_cleanup
            .find("acquire_file_lock_exclusive")
            .expect("durable cleanup index lock");
        let intent_publish = durable_cleanup
            .find("save_output_cleanup_intent")
            .expect("durable cleanup intent publish");
        let index_publish = durable_cleanup
            .find("save_file_index_for_doc")
            .expect("durable cleanup index publish");
        let delete_bytes = durable_cleanup
            .rfind("remove_file_path(&path)")
            .expect("durable cleanup physical unlink");
        assert!(delete_lock < intent_publish);
        assert!(intent_publish < index_publish);
        assert!(index_publish < delete_bytes);
        assert!(durable_cleanup.contains("chat output cleanup remains pending"));

        let whole_session_cleanup = source
            .split_once("    async fn cleanup_all_outputs_for_session(")
            .expect("whole-session output cleanup")
            .1
            .split_once("    async fn list_message_segments(")
            .expect("whole-session output cleanup end")
            .0;
        let clear_lock = whole_session_cleanup
            .find("acquire_file_lock_exclusive")
            .expect("whole-session cleanup lock");
        let clear_scan = whole_session_cleanup
            .find("read_dir_path")
            .expect("whole-session output scan");
        let clear_index = whole_session_cleanup
            .rfind("remove_file_path(&index_path)")
            .expect("whole-session index cleanup");
        assert!(clear_lock < clear_scan && clear_scan < clear_index);
        assert!(whole_session_cleanup.contains("first_removal_error"));
    }

    #[tokio::test]
    async fn file_index_reader_rejects_oversized_and_deep_unknown_payloads() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        let doc = store
            .load_document(&session.id)
            .await
            .expect("session document");
        let path = store.workspace_layout.chat_session_file_index_path(
            &session.principal,
            &session.workspace,
            &session.id,
        );

        store
            .workspace_layout
            .write_atomic_path(&path, &vec![b'x'; CHAT_SESSION_FILE_INDEX_MAX_BYTES + 1])
            .await
            .expect("oversized file index");
        assert!(store.load_file_index_for_doc(&doc).await.is_err());

        let nested = format!(
            "{{\"files\":[],\"ignored\":{}null{}}}",
            "[".repeat(crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH + 1),
            "]".repeat(crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH + 1)
        );
        store
            .workspace_layout
            .write_atomic_path(&path, nested.as_bytes())
            .await
            .expect("deep file index");
        assert!(
            store.load_file_index_for_doc(&doc).await.is_err(),
            "unknown fields must not bypass encoded-depth admission"
        );
    }

    #[tokio::test]
    async fn synthesized_history_tail_does_not_open_old_display_message_segments() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();
        for index in 0..7 {
            store
                .append_message(
                    &session.id,
                    text_message(&session.id, format!("msg-{index}"), format!("text-{index}")),
                )
                .await
                .unwrap();
        }
        let doc = store.load_document(&session.id).await.unwrap();
        let segments = store.list_message_segments(&doc).await.unwrap();
        assert_eq!(segments.len(), 3);
        fs::write(&segments[0].1, b"corrupt old display segment")
            .await
            .unwrap();

        let tail = store.get_llm_history_tail(&session.id, 1).await.unwrap();
        assert_eq!(
            tail,
            vec![ChatLlmTranscriptEntry::UserText {
                text: "text-6".to_string(),
            }],
            "a small synthesized tail must reverse-scan only the newest display segment"
        );
        assert!(
            store.get_llm_history(&session.id).await.is_err(),
            "the full replay still observes corruption in the old segment"
        );
    }

    #[test]
    fn bounded_transcript_batches_move_non_clone_entries_and_drop_exactly_once() {
        struct DropProbe {
            value: String,
            drops: Arc<std::sync::atomic::AtomicUsize>,
        }

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.drops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let original = "owned transcript payload".repeat(64);
        let original_ptr = original.as_ptr();
        let mut entries = vec![DropProbe {
            value: original,
            drops: Arc::clone(&drops),
        }]
        .into_iter();
        let batch = take_bounded_batch(&mut entries, CHAT_TRANSCRIPT_MAX_ENTRIES_PER_APPEND);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].value.as_ptr(), original_ptr);
        assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(batch);
        assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn chat_encoded_node_admission_is_exact_for_wide_arrays_and_strings_on_small_stack() {
        std::thread::Builder::new()
            .name("chat-node-admission".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let wide_string =
                    serde_json::to_vec(&serde_json::json!(["x".repeat(1024 * 1024)])).unwrap();
                let admitted: serde_json::Value = deserialize_guarded_chat_bytes_with_nodes(
                    &wide_string,
                    CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES as u64,
                    2,
                )
                .expect("array plus one wide string is exactly two nodes");
                assert!(admitted.as_array().is_some_and(|values| values.len() == 1));
                assert!(
                    deserialize_guarded_chat_bytes_with_nodes::<serde_json::Value>(
                        &wide_string,
                        CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES as u64,
                        1,
                    )
                    .unwrap_err()
                    .to_string()
                    .contains("node limit")
                );
                drop(admitted);

                let wide_array = format!("[{}]", vec!["0"; 20_000].join(","));
                let exact: serde_json::Value = deserialize_guarded_chat_bytes_with_nodes(
                    wide_array.as_bytes(),
                    CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES as u64,
                    20_001,
                )
                .expect("container and every scalar fit the exact node boundary");
                assert_eq!(exact.as_array().map(Vec::len), Some(20_000));
                drop(exact);
                assert!(
                    deserialize_guarded_chat_bytes_with_nodes::<serde_json::Value>(
                        wide_array.as_bytes(),
                        CHAT_TRANSCRIPT_MAX_SEGMENT_BYTES as u64,
                        20_000,
                    )
                    .unwrap_err()
                    .to_string()
                    .contains("node limit")
                );
            })
            .unwrap()
            .join()
            .expect("raw chat node admission fits a small worker stack");
    }

    #[tokio::test]
    async fn canonical_transcript_appends_are_linear_and_session_document_stays_metadata_only() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");

        for index in 0..1_000 {
            store
                .append_llm_history_entries(
                    &session.id,
                    vec![ChatLlmTranscriptEntry::UserText {
                        text: format!("entry-{index}"),
                    }],
                )
                .await
                .expect("bounded transcript append");
        }

        let doc = store
            .load_document(&session.id)
            .await
            .expect("metadata doc");
        let session_path = store.workspace_layout.chat_session_path(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        let session_json = fs::read_to_string(&session_path)
            .await
            .expect("session json");
        assert_eq!(doc.format_version, CHAT_SESSION_DOCUMENT_FORMAT_VERSION);
        assert!(doc.llm_history.is_empty());
        assert!(!session_json.contains("llm_history"));
        assert!(session_json.len() < 16 * 1024);

        let manifest = store.load_transcript_manifest(&doc).await.unwrap().unwrap();
        assert_eq!(manifest.last_sequence, 1_000);
        assert_eq!(manifest.effective_entry_count, 1_000);
        let mut persisted_segment_bytes = 0_u64;
        let mut segments = fs::read_dir(store.transcript_segments_dir(&doc))
            .await
            .expect("segments dir");
        while let Some(entry) = segments.next_entry().await.expect("segment entry") {
            persisted_segment_bytes += entry.metadata().await.unwrap().len();
        }
        assert!(
            persisted_segment_bytes < 1_000 * 1_024,
            "per-append storage should remain linear and bounded"
        );
        assert!(store.compact_llm_history(&session.id).await.unwrap());
        assert!(
            !store.compact_llm_history(&session.id).await.unwrap(),
            "an unchanged compacted revision must not compact twice"
        );
        assert_eq!(
            store.get_llm_history(&session.id).await.unwrap().len(),
            1_000
        );
    }

    #[tokio::test]
    async fn background_compaction_is_deduplicated_and_preserves_exact_provider_replay() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "maintenance",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        for index in 0..CHAT_TRANSCRIPT_COMPACTION_SEGMENT_THRESHOLD {
            store
                .append_llm_history_entries(
                    &session.id,
                    vec![ChatLlmTranscriptEntry::AssistantTurn {
                        text: Some(format!("answer-{index}")),
                        tool_calls: Vec::new(),
                        provider_state: Some(AssistantProviderState::OpenaiResponses {
                            response_id: format!("response-{index}"),
                            tool_protocol_repair_checkpoint: index % 2 == 0,
                        }),
                    }],
                )
                .await
                .expect("append transcript revision");
        }
        let before = store.get_llm_history(&session.id).await.expect("before");

        store
            .transcript_compactions_inflight
            .insert(session.id.clone(), ());
        assert!(
            !store.compact_llm_history(&session.id).await.unwrap(),
            "a duplicate maintenance request must not queue on the session lock"
        );
        assert!(store
            .transcript_compactions_inflight
            .contains_key(&session.id));
        store.transcript_compactions_inflight.remove(&session.id);
        assert_eq!(
            store
                .compact_llm_history_maintenance_batch(1)
                .await
                .unwrap(),
            (0, 0),
            "maintenance must not contend with a recently updated interactive session"
        );

        let quiescent_at = Utc::now()
            .timestamp_millis()
            .saturating_sub(CHAT_TRANSCRIPT_COMPACTION_QUIESCENCE_MS + 1);
        store.index.update_timestamp(
            &session.principal,
            &session.workspace,
            &session.ui_thread_id,
            &session.id,
            quiescent_at,
        );
        let mut quiescent_doc = store.load_document(&session.id).await.unwrap();
        quiescent_doc.session.updated_at = quiescent_at;
        store.save_document(&quiescent_doc).await.unwrap();

        let (inspected, compacted) = store
            .compact_llm_history_maintenance_batch(1)
            .await
            .expect("maintenance batch");
        assert_eq!((inspected, compacted), (1, 1));
        assert_eq!(
            store.get_llm_history(&session.id).await.expect("after"),
            before,
            "generation replacement must preserve ordering and provider replay state exactly"
        );
        assert_eq!(
            store
                .compact_llm_history_maintenance_batch(1)
                .await
                .unwrap(),
            (1, 0),
            "an unchanged transcript revision must compact exactly once"
        );
    }

    #[tokio::test]
    async fn maintenance_revalidates_quiescence_under_the_session_lock() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "maintenance-race",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");
        for index in 0..CHAT_TRANSCRIPT_COMPACTION_SEGMENT_THRESHOLD {
            store
                .append_llm_history_entries(
                    &session.id,
                    vec![ChatLlmTranscriptEntry::UserText {
                        text: format!("turn-{index}"),
                    }],
                )
                .await
                .unwrap();
        }

        // Simulate an old maintenance index snapshot while the authoritative
        // document remains recent. The under-lock recheck must win.
        store.index.update_timestamp(
            &session.principal,
            &session.workspace,
            &session.ui_thread_id,
            &session.id,
            Utc::now()
                .timestamp_millis()
                .saturating_sub(CHAT_TRANSCRIPT_COMPACTION_QUIESCENCE_MS + 1),
        );
        assert_eq!(
            store
                .compact_llm_history_maintenance_batch(1)
                .await
                .unwrap(),
            (1, 0),
        );
    }

    #[tokio::test]
    async fn startup_indexes_metadata_without_decoding_corrupt_transcript_segments() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();
        store
            .append_llm_history_entries(
                &session.id,
                vec![ChatLlmTranscriptEntry::UserText {
                    text: "persisted".to_string(),
                }],
            )
            .await
            .unwrap();
        let doc = store.load_document(&session.id).await.unwrap();
        let manifest = store.load_transcript_manifest(&doc).await.unwrap().unwrap();
        fs::write(
            store.transcript_segment_path(&doc, &manifest.generation, 1),
            "corrupt transcript body",
        )
        .await
        .unwrap();

        let restarted = FileChatStore::with_index(temp_dir.path())
            .await
            .expect("metadata-only startup must ignore transcript bodies");
        assert!(restarted.get_session(&session.id).await.unwrap().is_some());
        assert!(restarted.get_llm_history(&session.id).await.is_err());
    }

    #[tokio::test]
    async fn startup_streams_large_legacy_metadata_without_hydrating_inline_history() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();
        let session_path = store.workspace_layout.chat_session_path(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        let deliberately_non_hydratable_legacy = serde_json::json!({
            "format_version": 1,
            "session": session,
            // A full ChatSessionDocument rejects this shape. The startup
            // projection must skip it without allocating the 2 MiB value.
            "llm_history": "x".repeat(2 * 1024 * 1024),
        });
        store
            .workspace_layout
            .write_atomic_path(
                &session_path,
                &serde_json::to_vec(&deliberately_non_hydratable_legacy).unwrap(),
            )
            .await
            .unwrap();
        drop(store);

        let restarted = FileChatStore::with_index(temp_dir.path())
            .await
            .expect("startup should parse only the streamed metadata projection");
        assert!(restarted.session_locations.contains_key(&session.id));
        assert!(restarted.load_document(&session.id).await.is_err());
    }

    #[tokio::test]
    async fn provider_continuations_and_tool_pairs_survive_segment_boundaries_exactly() {
        use crate::magician_v2::chat::models::StoredToolCall;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();
        let openai = ChatLlmTranscriptEntry::AssistantTurn {
            text: None,
            tool_calls: vec![StoredToolCall {
                id: "call-1".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"q": "bounded"}),
            }],
            provider_state: Some(AssistantProviderState::OpenaiResponses {
                response_id: "resp-openai".to_string(),
                tool_protocol_repair_checkpoint: false,
            }),
        };
        let result = ChatLlmTranscriptEntry::ToolResult {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("search".to_string()),
            content: "result".to_string(),
        };
        let gemini = ChatLlmTranscriptEntry::AssistantTurn {
            text: Some("gemini".to_string()),
            tool_calls: Vec::new(),
            provider_state: Some(AssistantProviderState::Gemini {
                parts: vec![serde_json::json!({"text": "gemini-native"})],
            }),
        };
        let anthropic = ChatLlmTranscriptEntry::AssistantTurn {
            text: Some("anthropic".to_string()),
            tool_calls: Vec::new(),
            provider_state: Some(AssistantProviderState::AnthropicMessages {
                content: vec![serde_json::json!({"type": "text", "text": "native"})],
            }),
        };
        for entry in [&openai, &result, &gemini, &anthropic] {
            store
                .append_llm_history_entries(&session.id, vec![entry.clone()])
                .await
                .unwrap();
        }

        let restored = store.get_llm_history(&session.id).await.unwrap();
        assert_eq!(restored, vec![openai, result, gemini, anthropic]);
        let doc = store.load_document(&session.id).await.unwrap();
        let manifest = store.load_transcript_manifest(&doc).await.unwrap().unwrap();
        let first: ChatTranscriptSegment = serde_json::from_str(
            &fs::read_to_string(store.transcript_segment_path(&doc, &manifest.generation, 1))
                .await
                .unwrap(),
        )
        .unwrap();
        let second: ChatTranscriptSegment = serde_json::from_str(
            &fs::read_to_string(store.transcript_segment_path(&doc, &manifest.generation, 2))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            first.group_id, second.group_id,
            "a provider tool call and its result must retain one logical segment group"
        );
        assert!(manifest.open_tool_call_ids.is_empty());

        let latest_openai = ChatLlmTranscriptEntry::AssistantTurn {
            text: Some("latest openai".to_string()),
            tool_calls: Vec::new(),
            provider_state: Some(AssistantProviderState::OpenaiResponses {
                response_id: "resp-latest".to_string(),
                tool_protocol_repair_checkpoint: true,
            }),
        };
        store
            .append_llm_history_entries(&session.id, vec![latest_openai.clone()])
            .await
            .unwrap();
        for index in 0..5 {
            store
                .append_llm_history_entries(
                    &session.id,
                    vec![ChatLlmTranscriptEntry::UserText {
                        text: format!("later-{index}"),
                    }],
                )
                .await
                .unwrap();
        }

        let bounded_tail = store.get_llm_history_tail(&session.id, 2).await.unwrap();
        assert_eq!(
            bounded_tail,
            vec![
                ChatLlmTranscriptEntry::UserText {
                    text: "later-3".to_string(),
                },
                ChatLlmTranscriptEntry::UserText {
                    text: "later-4".to_string(),
                },
            ],
            "a bounded read must not detach and fabricate an older provider anchor"
        );
        let exact = store.get_llm_history(&session.id).await.unwrap();
        assert_eq!(exact[4], latest_openai);
        assert!(matches!(
            &exact[4],
            ChatLlmTranscriptEntry::AssistantTurn {
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id,
                    ..
                }),
                ..
            } if response_id == "resp-latest"
        ));
        assert!(
            store
                .workspace_layout
                .metadata_path(store.transcript_dir(&doc).join("checkpoints"))
                .await
                .unwrap()
                .is_none(),
            "provider state remains only in its exact semantic transcript position"
        );
    }

    #[tokio::test]
    async fn transcript_commit_ignores_partial_tail_and_bounded_tail_skips_corrupt_old_segment() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();
        for value in ["old", "middle", "new"] {
            store
                .append_llm_history_entries(
                    &session.id,
                    vec![ChatLlmTranscriptEntry::UserText {
                        text: value.to_string(),
                    }],
                )
                .await
                .unwrap();
        }
        let doc = store.load_document(&session.id).await.unwrap();
        let manifest = store.load_transcript_manifest(&doc).await.unwrap().unwrap();
        fs::write(
            store.transcript_segment_path(&doc, &manifest.generation, manifest.last_sequence + 1),
            "partial uncommitted segment",
        )
        .await
        .unwrap();
        assert_eq!(store.get_llm_history(&session.id).await.unwrap().len(), 3);

        fs::write(
            store.transcript_segment_path(&doc, &manifest.generation, 1),
            "corrupt old committed segment",
        )
        .await
        .unwrap();
        assert!(store.get_llm_history(&session.id).await.is_err());
        assert_eq!(
            store.get_llm_history_tail(&session.id, 1).await.unwrap(),
            vec![ChatLlmTranscriptEntry::UserText {
                text: "new".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn monolithic_llm_history_migrates_lazily_and_idempotently() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();
        let mut legacy = store.load_document(&session.id).await.unwrap();
        legacy.format_version = 1;
        legacy.llm_history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "legacy".to_string(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: Some("restored".to_string()),
                tool_calls: Vec::new(),
                provider_state: Some(AssistantProviderState::OpenaiResponses {
                    response_id: "resp-legacy".to_string(),
                    tool_protocol_repair_checkpoint: false,
                }),
            },
        ];
        store.save_legacy_document_fixture(&legacy).await.unwrap();

        let restarted = FileChatStore::with_index(temp_dir.path()).await.unwrap();
        let first = restarted.get_llm_history(&session.id).await.unwrap();
        let migrated_doc = restarted.load_document(&session.id).await.unwrap();
        let first_manifest = restarted
            .load_transcript_manifest(&migrated_doc)
            .await
            .unwrap()
            .unwrap();
        let second = restarted.get_llm_history(&session.id).await.unwrap();
        let second_manifest = restarted
            .load_transcript_manifest(&restarted.load_document(&session.id).await.unwrap())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(first_manifest.last_sequence, second_manifest.last_sequence);
        assert!(first_manifest.legacy_migration_complete);
        assert_eq!(
            migrated_doc.format_version,
            CHAT_SESSION_DOCUMENT_FORMAT_VERSION
        );
        assert!(migrated_doc.llm_history.is_empty());
    }

    #[tokio::test]
    async fn metadata_and_display_mutations_segment_a_large_legacy_document_before_rewrite() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "project-history",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();
        let mut legacy = store.load_document(&session.id).await.unwrap();
        legacy.format_version = 1;
        legacy.messages = (0..12)
            .map(|index| {
                text_message(
                    &session.id,
                    format!("legacy-message-{index}"),
                    format!("legacy display {index}: {}", "m".repeat(32 * 1024)),
                )
            })
            .collect();
        legacy.llm_history = (0..12)
            .map(|index| ChatLlmTranscriptEntry::UserText {
                text: format!("legacy prompt {index}: {}", "h".repeat(32 * 1024)),
            })
            .collect();
        store.save_legacy_document_fixture(&legacy).await.unwrap();
        let session_path = store.workspace_layout.chat_session_path(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        assert!(fs::metadata(&session_path).await.unwrap().len() > 256 * 1024);

        // A metadata-only title update is the first mutation after restart. It
        // must migrate both inline ledgers before writing the v2 document.
        let restarted = FileChatStore::with_index(temp_dir.path()).await.unwrap();
        restarted
            .update_session_title(&session.id, "Migrated title")
            .await
            .unwrap();
        restarted
            .append_message(
                &session.id,
                text_message(&session.id, "new-display-message", "after migration"),
            )
            .await
            .unwrap();
        restarted
            .update_session_status(&session.id, "archived")
            .await
            .unwrap();

        let doc = restarted.load_document(&session.id).await.unwrap();
        let metadata = fs::read_to_string(&session_path).await.unwrap();
        assert_eq!(doc.format_version, CHAT_SESSION_DOCUMENT_FORMAT_VERSION);
        assert!(doc.messages.is_empty());
        assert!(doc.llm_history.is_empty());
        assert_eq!(doc.session.title.as_deref(), Some("Migrated title"));
        assert_eq!(doc.session.status, ChatSessionStatus::Archived);
        assert!(metadata.len() < 16 * 1024);
        assert!(!metadata.contains("\"messages\""));
        assert!(!metadata.contains("\"llm_history\""));

        let messages = restarted.get_messages(&session.id, 100).await.unwrap();
        assert_eq!(messages.len(), 13);
        assert_eq!(messages.last().unwrap().id, "new-display-message");
        let history = restarted.get_llm_history(&session.id).await.unwrap();
        assert_eq!(history.len(), 12);
    }

    #[test]
    fn deep_oversized_and_inline_v2_session_documents_fail_closed_on_a_small_stack() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let root = temp_dir.path().to_path_buf();
        std::thread::Builder::new()
            .name("chat-document-admission-small-stack".to_string())
            .stack_size(512 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let store = FileChatStore::new(&root);

                    let deep_transcript = store
                        .new_session(
                            "user-1",
                            "workspace-a",
                            "deep-transcript",
                            &ChatChannel::web(),
                            "agent",
                        )
                        .await
                        .unwrap();
                    let mut deep_arguments = serde_json::Value::Null;
                    for _ in 0..10_000 {
                        deep_arguments = serde_json::Value::Array(vec![deep_arguments]);
                    }
                    assert!(store
                        .append_llm_history_entries(
                            &deep_transcript.id,
                            vec![ChatLlmTranscriptEntry::AssistantTurn {
                                text: None,
                                tool_calls: vec![
                                    crate::magician_v2::chat::models::StoredToolCall {
                                        id: "deep-call".to_string(),
                                        name: "deep-tool".to_string(),
                                        arguments: deep_arguments,
                                    },
                                ],
                                provider_state: None,
                            }],
                        )
                        .await
                        .is_err());

                    let deep = store
                        .new_session(
                            "user-1",
                            "workspace-a",
                            "deep-json",
                            &ChatChannel::web(),
                            "agent",
                        )
                        .await
                        .unwrap();
                    let deep_path = store.workspace_layout.chat_session_path(
                        &deep.principal,
                        &deep.workspace,
                        &deep.id,
                    );
                    let mut deep_json = fs::read_to_string(&deep_path).await.unwrap();
                    assert_eq!(deep_json.pop(), Some('}'));
                    deep_json.push_str(",\"untrusted\":");
                    deep_json.extend(std::iter::repeat('[').take(10_000));
                    deep_json.push('0');
                    deep_json.extend(std::iter::repeat(']').take(10_000));
                    deep_json.push('}');
                    fs::write(&deep_path, deep_json).await.unwrap();
                    assert!(store.load_document(&deep.id).await.is_err());

                    let oversized = store
                        .new_session(
                            "user-1",
                            "workspace-a",
                            "oversized-json",
                            &ChatChannel::web(),
                            "agent",
                        )
                        .await
                        .unwrap();
                    let oversized_path = store.workspace_layout.chat_session_path(
                        &oversized.principal,
                        &oversized.workspace,
                        &oversized.id,
                    );
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&oversized_path)
                        .unwrap()
                        .set_len(CHAT_SESSION_LEGACY_DOCUMENT_MAX_BYTES + 1)
                        .unwrap();
                    assert!(store.load_document(&oversized.id).await.is_err());

                    let inline = store
                        .new_session(
                            "user-1",
                            "workspace-a",
                            "inline-v2",
                            &ChatChannel::web(),
                            "agent",
                        )
                        .await
                        .unwrap();
                    let inline_path = store.workspace_layout.chat_session_path(
                        &inline.principal,
                        &inline.workspace,
                        &inline.id,
                    );
                    let invalid = ChatSessionDocument {
                        format_version: CHAT_SESSION_DOCUMENT_FORMAT_VERSION,
                        session: inline.clone(),
                        messages: vec![text_message(&inline.id, "forged-inline", "must reject")],
                        llm_history: Vec::new(),
                    };
                    fs::write(&inline_path, serde_json::to_vec(&invalid).unwrap())
                        .await
                        .unwrap();
                    assert!(store.load_document(&inline.id).await.is_err());

                    let reindexed = FileChatStore::with_index(&root).await.unwrap();
                    assert!(reindexed.get_session(&deep.id).await.is_err());
                    assert!(reindexed.get_session(&oversized.id).await.is_err());
                    assert!(reindexed.get_session(&inline.id).await.is_err());
                });
            })
            .unwrap()
            .join()
            .expect("chat JSON admission must fit a 512 KiB stack");
    }

    #[tokio::test]
    async fn wide_provider_json_is_rejected_before_transcript_serde() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .new_session(
                "user-1",
                "workspace-a",
                "wide-transcript",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .unwrap();
        let wide_arguments = serde_json::Value::Array(vec![
            serde_json::Value::Null;
            CHAT_TRANSCRIPT_MAX_JSON_NODES_PER_APPEND
        ]);
        let error = store
            .append_llm_history_entries(
                &session.id,
                vec![ChatLlmTranscriptEntry::AssistantTurn {
                    text: None,
                    tool_calls: vec![crate::magician_v2::chat::models::StoredToolCall {
                        id: "wide-call".to_string(),
                        name: "wide-tool".to_string(),
                        arguments: wide_arguments,
                    }],
                    provider_state: None,
                }],
            )
            .await
            .expect_err("wide JSON must fail aggregate-node admission");
        assert!(error.to_string().contains("retained-value contract"));
        assert!(store.get_llm_history(&session.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn initialize_deduplicates_legacy_and_scoped_session_files() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let session = store
            .get_or_create_active_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "agent",
            )
            .await
            .expect("session");

        let scoped_path =
            store
                .workspace_layout
                .chat_session_path("user-1", "workspace-a", &session.id);
        let legacy_path =
            store
                .workspace_layout
                .legacy_chat_session_path("user-1", "workspace-a", &session.id);
        let scoped_content = fs::read(&scoped_path).await.expect("scoped content");
        fs::write(&legacy_path, &scoped_content)
            .await
            .expect("legacy duplicate");

        let reloaded_store = FileChatStore::with_index(temp_dir.path())
            .await
            .expect("reloaded store");
        let sessions = reloaded_store
            .list_sessions("user-1", "workspace-a")
            .await
            .expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.id);

        reloaded_store
            .update_session_title(&session.id, "Updated title")
            .await
            .expect("update title");
        assert!(
            fs::metadata(&legacy_path).await.is_err(),
            "legacy duplicate should be removed once the session is saved"
        );
    }

    #[tokio::test]
    async fn session_history_page_filters_searches_and_pages_from_the_index() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let personal = store
            .new_session_with_history_lane(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "personal-assistant",
                HistoryLane::Personal,
            )
            .await
            .expect("personal session");
        store
            .update_session_title(&personal.id, "Quarterly planning")
            .await
            .expect("personal title");
        for thread_id in ["tabs", "screens"] {
            store
                .new_session_with_history_lane(
                    "user-1",
                    "workspace-a",
                    thread_id,
                    &ChatChannel::web(),
                    "personal-assistant",
                    HistoryLane::Automated,
                )
                .await
                .expect("automated session");
        }

        let personal_page = store
            .list_sessions_page(
                "user-1",
                "workspace-a",
                ChatSessionPageQuery {
                    ui_thread_id: None,
                    history_lane: Some(HistoryLane::Personal),
                    search: "planning".to_string(),
                    limit: 10,
                    offset: 0,
                },
            )
            .await
            .expect("personal page");
        assert_eq!(personal_page.total, 1);
        assert_eq!(personal_page.sessions[0].id, personal.id);

        let automated_page = store
            .list_sessions_page(
                "user-1",
                "workspace-a",
                ChatSessionPageQuery {
                    ui_thread_id: None,
                    history_lane: Some(HistoryLane::Automated),
                    search: String::new(),
                    limit: 1,
                    offset: 1,
                },
            )
            .await
            .expect("automated page");
        assert_eq!(automated_page.total, 2);
        assert_eq!(automated_page.sessions.len(), 1);
        assert_eq!(automated_page.limit, 1);
        assert_eq!(automated_page.offset, 1);
    }

    #[tokio::test]
    async fn lane_aware_reuse_does_not_cross_personal_and_automated_history() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let personal = store
            .get_or_create_active_session_with_history_lane(
                "user-1",
                "workspace-a",
                "shared-thread",
                &ChatChannel::web(),
                "personal-assistant",
                HistoryLane::Personal,
            )
            .await
            .expect("personal session");
        let automated = store
            .get_or_create_active_session_with_history_lane(
                "user-1",
                "workspace-a",
                "shared-thread",
                &ChatChannel::web(),
                "personal-assistant",
                HistoryLane::Automated,
            )
            .await
            .expect("automated session");

        assert_ne!(personal.id, automated.id);
        assert_eq!(personal.history_lane, HistoryLane::Personal);
        assert_eq!(automated.history_lane, HistoryLane::Automated);
    }

    #[tokio::test]
    async fn original_general_session_is_personal_and_cannot_be_archived_deleted_or_moved() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let original = store
            .new_session_with_history_lane(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "personal-assistant",
                HistoryLane::Automated,
            )
            .await
            .expect("original general session");
        let later = store
            .new_session_with_history_lane(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "personal-assistant",
                HistoryLane::Automated,
            )
            .await
            .expect("later general session");

        assert_eq!(original.history_lane, HistoryLane::Personal);
        assert!(original.is_default_session);
        assert_eq!(later.history_lane, HistoryLane::Automated);
        assert!(!later.is_default_session);

        assert!(store
            .update_session_status(&original.id, "archived")
            .await
            .is_err());
        assert!(store.delete_session(&original.id).await.is_err());
        assert!(store
            .update_session_thread(&original.id, "another-thread")
            .await
            .is_err());

        let unchanged = store
            .get_session(&original.id)
            .await
            .expect("load original")
            .expect("original exists");
        assert_eq!(unchanged.status, ChatSessionStatus::Active);
        assert_eq!(unchanged.ui_thread_id, "general");

        store
            .update_session_status(&later.id, "archived")
            .await
            .expect("later general session remains archivable");
        store
            .delete_session(&later.id)
            .await
            .expect("later general session remains deletable");
    }

    #[tokio::test]
    async fn index_migrates_and_restores_a_legacy_default_general_session() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(temp_dir.path());
        let original = store
            .new_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "personal-assistant",
            )
            .await
            .expect("original general session");
        let later = store
            .new_session(
                "user-1",
                "workspace-a",
                "general",
                &ChatChannel::web(),
                "personal-assistant",
            )
            .await
            .expect("later general session");

        let mut original_doc = store
            .load_document(&original.id)
            .await
            .expect("load original document");
        original_doc.session.created_at = 100;
        original_doc.session.status = ChatSessionStatus::Archived;
        original_doc.session.history_lane = HistoryLane::Automated;
        original_doc.session.is_default_session = false;
        store
            .save_document(&original_doc)
            .await
            .expect("write legacy original document");

        let mut later_doc = store
            .load_document(&later.id)
            .await
            .expect("load later document");
        later_doc.session.created_at = 200;
        // A stale marker on a later record must not supersede creation order.
        later_doc.session.is_default_session = true;
        store
            .save_document(&later_doc)
            .await
            .expect("write legacy later document");
        // Legacy stores did not publish lifecycle generation markers. Remove
        // the markers created by the current fixture setup so startup exercises
        // the actual marker-free migration contract instead of manufacturing a
        // stale-marker tamper case by changing `created_at` in place.
        for session in [&original_doc.session, &later_doc.session] {
            let marker_path = store.workspace_layout.chat_session_generation_marker_path(
                &session.principal,
                &session.workspace,
                &session.id,
            );
            store
                .workspace_layout
                .remove_file_path(&marker_path)
                .await
                .expect("remove current generation marker from legacy fixture");
        }
        drop(store);

        let reloaded = FileChatStore::with_index(temp_dir.path())
            .await
            .expect("reload indexed store");
        let migrated = reloaded
            .get_session(&original.id)
            .await
            .expect("load migrated original")
            .expect("migrated original exists");
        let unchanged_later = reloaded
            .get_session(&later.id)
            .await
            .expect("load later session")
            .expect("later session exists");

        assert_eq!(migrated.status, ChatSessionStatus::Active);
        assert_eq!(migrated.history_lane, HistoryLane::Personal);
        assert!(migrated.is_default_session);
        assert!(!unchanged_later.is_default_session);
    }

    #[test]
    fn session_search_candidates_are_deduplicated_and_recency_ordered() {
        let index = PrincipalIndex::new();
        let entry = |session_id: &str, title: &str, updated_at: i64| SessionIndexEntry {
            session_id: session_id.to_string(),
            status: ChatSessionStatus::Active,
            created_at: 1,
            updated_at,
            title: Some(title.to_string()),
            agent_id: "personal-assistant".to_string(),
            history_lane: HistoryLane::Personal,
            is_default_session: false,
            is_concurrent: false,
        };
        index.insert(
            "user-1",
            "workspace-a",
            "general",
            entry("older", "Quarterly plan", 10),
        );
        index.insert(
            "user-1",
            "workspace-a",
            "screens",
            entry("newer", "Quarterly capture", 30),
        );
        index.insert(
            "user-1",
            "workspace-a",
            "moved",
            entry("newer", "Quarterly capture", 20),
        );
        index.insert(
            "another-user",
            "workspace-a",
            "general",
            entry("foreign", "Quarterly plan", 40),
        );

        let candidates = index.search_candidates("user-1", "workspace-a", "quarterly");

        assert_eq!(
            candidates,
            vec![
                ChatSessionSearchCandidate {
                    id: "newer".to_string(),
                    updated_at: 30,
                },
                ChatSessionSearchCandidate {
                    id: "older".to_string(),
                    updated_at: 10,
                },
            ]
        );
    }

    #[test]
    fn index_carries_the_thread_and_lane_sync_scope_needs() {
        let index = PrincipalIndex::new();
        let entry =
            |session_id: &str, title: Option<&str>, history_lane: HistoryLane| SessionIndexEntry {
                session_id: session_id.to_string(),
                status: ChatSessionStatus::Active,
                created_at: 1,
                updated_at: 1,
                title: title.map(str::to_string),
                agent_id: "personal-assistant".to_string(),
                history_lane,
                is_default_session: false,
                is_concurrent: false,
            };
        index.insert(
            "user-1",
            "workspace-a",
            "general",
            entry("personal", None, HistoryLane::Personal),
        );
        index.insert(
            "user-1",
            "workspace-a",
            "screens",
            entry("automated", None, HistoryLane::Automated),
        );
        // A Legacy entry must resolve through the same `(ui_thread_id, title)`
        // inference `ChatSession::effective_history_lane` uses, so index-derived
        // and document-derived lanes cannot drift.
        index.insert(
            "user-1",
            "workspace-a",
            "travel",
            entry("legacy", Some("Wtf"), HistoryLane::Legacy),
        );
        index.insert(
            "other-user",
            "workspace-a",
            "general",
            entry("foreign", None, HistoryLane::Personal),
        );

        let mut lanes = index.session_thread_lanes("user-1", "workspace-a");
        // `HistoryLane` is not `Ord`; the thread ids are unique here, so order by those.
        lanes.sort_by(|left, right| left.0.cmp(&right.0));

        let mut expected = vec![
            ("general".to_string(), HistoryLane::Personal),
            ("screens".to_string(), HistoryLane::Automated),
            (
                "travel".to_string(),
                infer_legacy_session_history_lane("travel", Some("Wtf")),
            ),
        ];
        expected.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(lanes, expected, "other principals must not leak in");
    }

    #[tokio::test]
    async fn thread_lanes_come_from_the_index_without_reading_documents() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::with_index(temp_dir.path())
            .await
            .expect("indexed store");

        let session = store
            .new_session_with_history_lane(
                "user-1",
                "workspace-a",
                "screens",
                &ChatChannel::web(),
                "personal-assistant",
                HistoryLane::Automated,
            )
            .await
            .expect("create session");

        let before = store
            .list_session_thread_lanes("user-1", "workspace-a")
            .await
            .expect("thread lanes");
        assert!(before.contains(&("screens".to_string(), HistoryLane::Automated)));

        // Damage the document the old path loads per session. The lane read must
        // be completely unaffected, which is only possible if it never opened
        // the file.
        //
        // Deliberately **not** contrasted against `list_sessions` here. That
        // path looks tolerant — `if let Ok(doc) = self.load_document(..)` — but
        // the `lock_session(..)?` on the line above validates the document and
        // propagates, so the tolerant branch is unreachable for real damage:
        // one unreadable session errors the whole scope's listing, whether the
        // file is corrupt or missing. Asserting a skip here would have been
        // asserting something that does not happen.
        let document_path =
            store
                .workspace_layout
                .chat_session_path("user-1", "workspace-a", &session.id);
        std::fs::write(&document_path, b"{ not a chat session document")
            .expect("corrupt session document");

        let after = store
            .list_session_thread_lanes("user-1", "workspace-a")
            .await
            .expect("thread lanes after document removal");
        assert_eq!(
            before, after,
            "the lane read must be served entirely from the in-memory index"
        );
    }
}
