//! Storage-shaped trait surface for memory-index / memory-candidate code.
//!
//! The concrete `AgentStorage` implementation lives in magician. This trait
//! lists only the methods the moved modules actually call. To stay
//! object-safe the trait avoids generic methods — generic deserialization is
//! pushed to the caller via `load_native_tier_value` / `read_json_value`,
//! which return `serde_json::Value` for the caller to deserialize.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use thiserror::Error;

use crate::memory_tiers::TierScope;

/// Error type surfaced from `MemoryStorage` operations.
///
/// We model it as a thin wrapper so the magician-side implementation can map
/// its concrete `AgentStorageError` onto this trait without leaking magician
/// types into the moved code. The actual contents are stringified — callers
/// of memory_index / memory_candidates already wrap these errors in
/// `anyhow::Context` / `AgentMemoryError::Storage`, so structural fidelity
/// is not required.
#[derive(Debug, Error)]
pub enum MemoryStorageError {
    #[error("invalid identifier `{0}`")]
    InvalidIdentifier(String),
    #[error("path `{path}` escapes storage root `{root}`")]
    PathOutsideRoot { path: String, root: String },
    #[error("missing goal_id for agent_goal tier `{tier_name}`")]
    MissingGoalId { tier_name: String },
    #[error("timed out acquiring file lock `{lock_path}` after {wait_ms}ms")]
    FileLockTimeout { lock_path: String, wait_ms: u64 },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("storage error: {0}")]
    Other(String),
}

/// Path / I/O facade the moved index code uses against magician's
/// `AgentStorage`. Object-safe — all generic methods (e.g.
/// `load_native_tier_record::<T>`) are replaced with `serde_json::Value`
/// returning equivalents.
#[async_trait]
pub trait MemoryStorage: Send + Sync {
    /// Root directory anchoring this storage slice.
    fn root(&self) -> &Path;

    /// `(principal, workspace)` derived from the path layout, when the
    /// storage root sits inside a scoped V3 workspace.
    fn scope_segments(&self) -> Option<(String, String)>;

    /// Per-agent tiers directory (sanitized tier JSON files live here).
    fn agent_tiers_dir(&self, agent_id: &str) -> Result<PathBuf, MemoryStorageError>;

    /// Per-agent episodes directory (one JSON file per episode).
    ///
    /// Mirrors [`Self::agent_tiers_dir`]: the crate takes the path from the
    /// facade and reads the directory itself, the same way tier goal-id
    /// discovery does.
    fn agent_episodes_dir(&self, agent_id: &str) -> Result<PathBuf, MemoryStorageError>;

    /// Path for a specific tier JSON file.
    fn agent_tier_path(
        &self,
        agent_id: &str,
        tier_name: &str,
        scope: &TierScope,
        goal_id: Option<&str>,
    ) -> Result<PathBuf, MemoryStorageError>;

    /// LanceDB hybrid index directory under `<root>/index/lancedb`.
    fn memory_lancedb_index_dir(&self) -> PathBuf;

    /// JSONL documents path under `<root>/index/documents.jsonl`.
    fn memory_index_documents_path(&self) -> PathBuf;

    /// Manifest path under `<root>/index/manifest.json`.
    fn memory_index_manifest_path(&self) -> PathBuf;

    /// Knowledge file path under `<user_root>/knowledge.json`.
    fn user_knowledge_path(&self) -> PathBuf;

    /// User root (scoped V3 uses `<root>/users`; legacy uses `<root>/user`).
    fn user_root(&self) -> PathBuf;

    /// Load user knowledge as a JSON value (returns empty object when the
    /// file is absent, matching magician's `load_user_knowledge` behaviour).
    async fn load_user_knowledge(&self) -> Result<serde_json::Value, MemoryStorageError>;

    /// Load a tier record as `serde_json::Value`. `Ok(None)` when the file
    /// is missing. Replaces the generic `load_native_tier_record::<T>` so
    /// the trait stays object-safe.
    async fn load_native_tier_value(
        &self,
        agent_id: &str,
        tier_name: &str,
        scope: &TierScope,
        goal_id: Option<&str>,
    ) -> Result<Option<serde_json::Value>, MemoryStorageError>;

    /// Read an arbitrary JSON file under the root as `serde_json::Value`.
    async fn read_json_value(&self, path: &Path) -> Result<serde_json::Value, MemoryStorageError>;

    /// Atomic write — used to persist the LanceDB documents JSONL.
    async fn write_bytes_atomic(&self, path: &Path, bytes: &[u8])
        -> Result<(), MemoryStorageError>;

    /// Atomic write of `serde_json::Value` to a JSON file (replaces the
    /// generic `write_json_atomic::<T>` for the manifest write).
    async fn write_json_value_atomic(
        &self,
        path: &Path,
        value: &serde_json::Value,
    ) -> Result<(), MemoryStorageError>;
}

/// Tier path filename sanitization helper used inside `memory_index` for
/// goal-id discovery. Mirrors magician's `sanitize_segment` (encodes unsafe
/// bytes as `~xx` hex pairs, encodes leading dots, caps overlong inputs
/// with a `~` hash prefix). The moved memory_index code only ever uses it
/// on tier names to compute the `<tier>_` filename prefix when scanning
/// for `<tier>_<goal>.json` files, so we re-implement that prefix here.
pub fn sanitize_segment(raw: &str) -> String {
    use std::fmt::Write as _;
    const MAX_IDENTIFIER_BYTES: usize = 255;
    const OVERFLOW_PREFIX: &str = "seg~";

    if raw.is_empty() {
        return "unnamed".to_string();
    }

    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(byte as char),
            _ => {
                let _ = write!(&mut out, "~{byte:02x}");
            },
        }
    }

    let sanitized = if out.is_empty() {
        "unnamed".to_string()
    } else if out.starts_with('.') {
        let dot_count = out.bytes().take_while(|&b| b == b'.').count();
        format!("{}{}", "~2e".repeat(dot_count), &out[dot_count..])
    } else {
        out
    };

    if sanitized.len() <= MAX_IDENTIFIER_BYTES {
        return sanitized;
    }

    let hash = blake3::hash(raw.as_bytes()).to_hex().to_string();
    let keep = MAX_IDENTIFIER_BYTES.saturating_sub(OVERFLOW_PREFIX.len() + 1 + hash.len());
    format!("{OVERFLOW_PREFIX}{}_{}", &sanitized[..keep], hash)
}
