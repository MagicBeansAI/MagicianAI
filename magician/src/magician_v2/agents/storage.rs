//! File-system layout and I/O helpers for agent data.
//!
//! This module owns path derivation for the Phase 2 storage contract and provides
//! atomic JSON/YAML writes. No runtime behavior is implemented here.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::{fs, io::AsyncWriteExt, task};
use tracing::warn;

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};

use super::memory_tiers::TierScope;
use super::trust::TrustPolicyFile;

static TMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

const FILE_LOCK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const FILE_LOCK_RETRY_DELAY_MIN: Duration = Duration::from_millis(10);
const FILE_LOCK_RETRY_DELAY_MAX: Duration = Duration::from_millis(200);

const MAX_IDENTIFIER_BYTES: usize = 255;
const SANITIZED_SEGMENT_OVERFLOW_PREFIX: &str = "seg~";
const RESERVED_AGENT_IDS: &[&str] = &[
    "approvals",
    "proposals",
    "scheduler_state.json",
    ".definition_store.write.lock",
    ".scheduler_state.write.lock",
];

#[derive(Debug, Error)]
pub enum AgentStorageError {
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
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// RAII guard for a cross-process advisory file lock.
///
/// The lock is released when this guard is dropped (the underlying file
/// descriptor is closed, which releases the `fs2` advisory lock).
#[derive(Debug)]
pub struct FileLockGuard {
    _file: std::fs::File,
    _lock_path: PathBuf,
}

/// Verify that a locked descriptor is still the inode published at its stable
/// sentinel path. Opening with `O_NOFOLLOW` protects the initial open, but a
/// peer can rename that inode and install a replacement while this process is
/// waiting in `flock`; accepting the stale descriptor would split authority
/// between two independently locked files.
pub(crate) fn validate_locked_file_path_identity(
    file: &std::fs::File,
    path: &Path,
    authority: &str,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let locked = file.metadata()?;
        let published = std::fs::symlink_metadata(path)?;
        let euid = unsafe { libc::geteuid() };
        if !locked.is_file()
            || locked.uid() != euid
            || locked.mode() & 0o777 != 0o600
            || locked.nlink() != 1
            || published.file_type().is_symlink()
            || !published.is_file()
            || published.uid() != euid
            || published.mode() & 0o777 != 0o600
            || published.nlink() != 1
            || published.dev() != locked.dev()
            || published.ino() != locked.ino()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{authority} locked path identity validation failed"),
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path, authority);
    }
    Ok(())
}

/// Sanitizes arbitrary names (tier names, file fragments) into safe path segments.
pub fn sanitize_segment(raw: &str) -> String {
    if raw.is_empty() {
        return "unnamed".to_string();
    }

    // Encode unsafe bytes as "~xx" (hex) so the mapping is injective and does not
    // silently collapse distinct inputs onto the same filename.
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(byte as char),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(&mut out, "~{byte:02x}");
            },
        }
    }

    let sanitized = if out.is_empty() {
        "unnamed".to_string()
    } else if out.starts_with('.') {
        // Encode leading dots as ~2e so the result is never a dotfile (which
        // list_files_with_extension skips as temp-file artifacts) while
        // preserving the injective mapping (".foo" ≠ "foo").
        let dot_count = out.bytes().take_while(|&b| b == b'.').count();
        format!("{}{}", "~2e".repeat(dot_count), &out[dot_count..])
    } else {
        out
    };

    if sanitized.len() <= MAX_IDENTIFIER_BYTES {
        return sanitized;
    }

    // Keep path segments within filesystem limits even for high-expansion UTF-8
    // inputs (each unsafe byte expands to "~xx").
    let hash = blake3::hash(raw.as_bytes()).to_hex().to_string();
    let keep = MAX_IDENTIFIER_BYTES
        .saturating_sub(SANITIZED_SEGMENT_OVERFLOW_PREFIX.len() + 1 + hash.len());
    format!(
        "{SANITIZED_SEGMENT_OVERFLOW_PREFIX}{}_{}",
        &sanitized[..keep],
        hash
    )
}

/// Convert a human-readable name into a clean kebab-case agent slug.
///
/// - Lowercases, replaces non-alphanumeric runs with `-`, trims leading/trailing hyphens
/// - Truncates to 64 chars (well under the 255-byte limit)
/// - Returns `"unnamed"` for empty input
pub fn slugify_name(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive hyphens, trim leading/trailing
    let collapsed: String = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        return "unnamed".to_string();
    }
    // Truncate to keep room for a potential "-xxxx" suffix
    let max_base = 64;
    if collapsed.len() <= max_base {
        collapsed
    } else {
        // Cut at a hyphen boundary if possible
        collapsed[..max_base]
            .rfind('-')
            .map(|i| collapsed[..i].to_string())
            .unwrap_or_else(|| collapsed[..max_base].to_string())
    }
}

pub fn validate_identifier(value: &str) -> Result<(), AgentStorageError> {
    if value.trim().is_empty() || value != value.trim() {
        return Err(AgentStorageError::InvalidIdentifier(value.to_string()));
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(AgentStorageError::InvalidIdentifier(value.to_string()));
    }
    if value == "." || value.contains("..") || value.contains('/') || value.contains('\\') {
        return Err(AgentStorageError::InvalidIdentifier(value.to_string()));
    }
    Ok(())
}

pub fn validate_agent_identifier(value: &str) -> Result<(), AgentStorageError> {
    validate_identifier(value)?;
    if !value.is_ascii() {
        return Err(AgentStorageError::InvalidIdentifier(value.to_string()));
    }
    if RESERVED_AGENT_IDS
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(value))
    {
        return Err(AgentStorageError::InvalidIdentifier(value.to_string()));
    }
    Ok(())
}

fn is_scoped_agent_runtime_root(root: &Path) -> bool {
    let Some(agent_runtime) = root.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if agent_runtime != "agent_runtime" {
        return false;
    }
    let Some(scopes_dir) = root
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(|value| value.file_name())
        .and_then(|value| value.to_str())
    else {
        return false;
    };
    scopes_dir == "scopes"
}

#[derive(Debug, Clone)]
pub struct AgentStorage {
    /// Root directory for one agent-storage slice.
    ///
    /// On the live V3 path this is normally either a scoped
    /// `.../scopes/<principal>/<workspace>/agent_runtime` root or the system
    /// template root under `.../system/agent_templates`, not the bare
    /// `magician_data_v3` workspace root.
    root: PathBuf,
    memory_layout: AgentMemoryLayout,
    workspace_layout: Option<ArtifactV2Workspace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentMemoryLayout {
    Legacy,
    ScopedV3,
}

impl AgentStorage {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            memory_layout: if is_scoped_agent_runtime_root(&root) {
                AgentMemoryLayout::ScopedV3
            } else {
                AgentMemoryLayout::Legacy
            },
            root,
            workspace_layout: None,
        }
    }

    pub fn new_in_workspace(root: impl AsRef<Path>, workspace_layout: ArtifactV2Workspace) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            memory_layout: if is_scoped_agent_runtime_root(&root) {
                AgentMemoryLayout::ScopedV3
            } else {
                AgentMemoryLayout::Legacy
            },
            root,
            workspace_layout: Some(workspace_layout),
        }
    }

    pub fn with_scoped_memory_root(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            memory_layout: AgentMemoryLayout::ScopedV3,
            workspace_layout: None,
        }
    }

    pub fn with_scoped_memory_root_in_workspace(
        root: impl AsRef<Path>,
        workspace_layout: ArtifactV2Workspace,
    ) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            memory_layout: AgentMemoryLayout::ScopedV3,
            workspace_layout: Some(workspace_layout),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates and revalidates a destination-authority directory. On Unix the
    /// directory is required to be owned by the current uid, non-symlinked and
    /// mode 0700. This is intentionally narrower than general agent layout
    /// creation and is used by immutable app-memory receipt chains.
    pub(crate) async fn ensure_private_directory(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<(), AgentStorageError> {
        let root = self.root.clone();
        let path = path.as_ref().to_path_buf();
        task::spawn_blocking(move || ensure_private_directory_blocking(&root, &path))
            .await
            .map_err(|error| {
                std::io::Error::other(format!("private directory worker failed: {error}"))
            })?
    }

    /// Reads a private regular file without following its final symlink and
    /// without allocating past `max_bytes`.
    pub(crate) async fn read_private_bytes_bounded(
        &self,
        path: impl AsRef<Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, AgentStorageError> {
        let path = path.as_ref().to_path_buf();
        #[cfg(unix)]
        {
            let root = self.root.clone();
            return task::spawn_blocking(move || {
                read_private_file_nofollow_unix(&root, &path, max_bytes)
            })
            .await
            .map_err(|error| std::io::Error::other(format!("private read worker failed: {error}")))?
            .map_err(AgentStorageError::Io);
        }
        #[cfg(not(unix))]
        {
            self.ensure_existing_path_within_root(&path)?;
            if let Some(workspace) = self.scoped_workspace_layout() {
                let max_bytes = u64::try_from(max_bytes).map_err(|_| {
                    AgentStorageError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "private agent-storage byte ceiling exceeds u64",
                    ))
                })?;
                return workspace
                    .read_bounded_path(&path, max_bytes)
                    .await
                    .map_err(agent_storage_provider_error);
            }
            let root = self.root.clone();
            task::spawn_blocking(move || -> Result<Vec<u8>, AgentStorageError> {
                use std::io::Read as _;
                validate_private_path_components(&root, &path, false)?;
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                let mut file = options.open(&path)?;
                let metadata = file.metadata()?;
                if !metadata.is_file() || metadata.len() > max_bytes as u64 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "private agent-storage file exceeds its bound or is not regular",
                    )
                    .into());
                }
                let mut bytes = Vec::with_capacity(metadata.len() as usize);
                file.by_ref()
                    .take(max_bytes as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > max_bytes {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "private agent-storage file exceeds its byte ceiling",
                    )
                    .into());
                }
                Ok(bytes)
            })
            .await
            .map_err(|error| {
                AgentStorageError::Io(std::io::Error::other(format!(
                    "private read worker failed: {error}"
                )))
            })?
        }
    }

    pub(crate) async fn write_private_bytes_atomic(
        &self,
        path: impl AsRef<Path>,
        bytes: &[u8],
    ) -> Result<(), AgentStorageError> {
        let path = path.as_ref();
        let parent = path.parent().ok_or_else(|| {
            AgentStorageError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "private file has no parent",
            ))
        })?;
        self.ensure_private_directory(parent).await?;
        self.write_bytes_atomic(path, bytes).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
            let metadata = fs::symlink_metadata(path).await?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o777 != 0o600
                || metadata.nlink() != 1
            {
                return Err(AgentStorageError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "private agent-storage file failed post-write validation",
                )));
            }
        }
        Ok(())
    }

    /// Removes one already-bounded private owner file and fsyncs its parent.
    /// This is used only after a newer checkpoint is durable; missing files
    /// are an idempotent replay, while symlinked/non-private paths fail closed.
    pub(crate) async fn remove_private_file_and_sync_parent(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<(), AgentStorageError> {
        let root = self.root.clone();
        let path = path.as_ref().to_path_buf();
        task::spawn_blocking(move || {
            validate_private_path_components(&root, &path, true)?;
            match std::fs::remove_file(&path) {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(AgentStorageError::Io(error)),
            }
            let parent = path.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "private file has no parent",
                )
            })?;
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(AgentStorageError::Io)
        })
        .await
        .map_err(|error| {
            AgentStorageError::Io(std::io::Error::other(format!(
                "private remove worker failed: {error}"
            )))
        })?
    }

    pub fn scope_segments(&self) -> Option<(String, String)> {
        let scoped_leaf = self.root.file_name()?.to_str()?;
        if scoped_leaf != "agent_runtime" && scoped_leaf != "memory" {
            return None;
        }
        let workspace = self.root.parent()?.file_name()?.to_str()?.to_string();
        let principal = self
            .root
            .parent()?
            .parent()?
            .file_name()?
            .to_str()?
            .to_string();
        let scopes_dir = self
            .root
            .parent()?
            .parent()?
            .parent()?
            .file_name()?
            .to_str()?;
        if scopes_dir != "scopes" {
            return None;
        }
        Some((principal, workspace))
    }

    pub fn is_scoped_v3_memory(&self) -> bool {
        matches!(self.memory_layout, AgentMemoryLayout::ScopedV3)
    }

    pub fn agents_root(&self) -> PathBuf {
        self.root.join("agents")
    }

    pub fn system_root(&self) -> PathBuf {
        self.root.join("system")
    }

    pub fn workspace_layout(&self) -> Option<&ArtifactV2Workspace> {
        self.workspace_layout.as_ref()
    }

    fn scoped_workspace_layout(&self) -> Option<ArtifactV2Workspace> {
        if let Some(workspace_layout) = &self.workspace_layout {
            return Some(workspace_layout.clone());
        }
        let leaf = self.root.file_name()?.to_str()?;
        if leaf != "agent_runtime" && leaf != "memory" {
            return None;
        }
        let base_root = self.root.parent()?.parent()?.parent()?.parent()?;
        Some(ArtifactV2Workspace::new(base_root))
    }

    pub fn user_root(&self) -> PathBuf {
        match self.memory_layout {
            AgentMemoryLayout::Legacy => self.root.join("user"),
            AgentMemoryLayout::ScopedV3 => self.root.join("users"),
        }
    }

    pub fn memory_index_dir(&self) -> PathBuf {
        self.root.join("index")
    }

    pub fn memory_index_manifest_path(&self) -> PathBuf {
        self.memory_index_dir().join("manifest.json")
    }

    pub fn memory_index_documents_path(&self) -> PathBuf {
        self.memory_index_dir().join("documents.jsonl")
    }

    pub fn memory_lancedb_index_dir(&self) -> PathBuf {
        self.memory_index_dir().join("lancedb")
    }

    pub fn agent_dir(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        validate_agent_identifier(agent_id)?;
        let agents_root = self.agents_root();
        self.ensure_existing_path_within_root(&agents_root)?;
        let agent_dir = agents_root.join(agent_id);
        self.ensure_existing_path_within_root(&agent_dir)?;
        Ok(agent_dir)
    }

    pub fn agent_definition_path(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_dir(agent_id)?.join("definition.agent.yaml"))
    }

    pub fn agent_definition_versions_dir(
        &self,
        agent_id: &str,
    ) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_dir(agent_id)?.join("definitions"))
    }

    pub fn agent_definition_version_path(
        &self,
        agent_id: &str,
        version: u32,
    ) -> Result<PathBuf, AgentStorageError> {
        Ok(self
            .agent_definition_versions_dir(agent_id)?
            .join(format!("v{version}.agent.yaml")))
    }

    pub fn agent_memory_dir(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(match self.memory_layout {
            AgentMemoryLayout::Legacy => self.agent_dir(agent_id)?.join("memory"),
            AgentMemoryLayout::ScopedV3 => self.agent_dir(agent_id)?,
        })
    }

    pub fn agent_tiers_dir(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_memory_dir(agent_id)?.join("tiers"))
    }

    pub fn agent_episodes_dir(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_memory_dir(agent_id)?.join("episodes"))
    }

    pub fn agent_episode_index_path(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self
            .agent_episodes_dir(agent_id)?
            .join(".episode_index.json"))
    }

    pub fn agent_corrections_path(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_memory_dir(agent_id)?.join("corrections.jsonl"))
    }

    /// Work-evidence graph: the agent's structured evidence collection (a JSON
    /// array of `EvidenceRecord`, whole-file rewritten on each append).
    pub fn agent_evidence_path(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_memory_dir(agent_id)?.join("evidence.json"))
    }

    /// Work-evidence graph: the agent's canonical entity anchors (a JSON array
    /// of `EntityRecord`, whole-file rewritten on each resolve/correction).
    pub fn agent_entities_path(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_memory_dir(agent_id)?.join("entities.json"))
    }

    pub fn agent_corrections_lock_path(
        &self,
        agent_id: &str,
    ) -> Result<PathBuf, AgentStorageError> {
        Ok(self
            .agent_memory_dir(agent_id)?
            .join(".corrections.append.lock"))
    }

    pub fn agent_consolidations_dir(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_memory_dir(agent_id)?.join("consolidations"))
    }

    pub fn agent_state_dir(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_dir(agent_id)?.join("state"))
    }

    /// Per-agent user memory directory for `FullyIsolated` mode.
    ///
    /// Path: `{root}/agents/{agent_id}/memory/user_memory/`
    pub fn agent_user_memory_dir(&self, agent_id: &str) -> Result<PathBuf, AgentStorageError> {
        Ok(self.agent_memory_dir(agent_id)?.join("user_memory"))
    }

    pub fn agent_tier_path(
        &self,
        agent_id: &str,
        tier_name: &str,
        scope: &TierScope,
        goal_id: Option<&str>,
    ) -> Result<PathBuf, AgentStorageError> {
        self.agent_tier_path_with_user_root(agent_id, tier_name, scope, goal_id, None)
    }

    /// Like [`agent_tier_path`] but allows overriding the directory used for `User`-scoped
    /// tiers (e.g. when an agent uses `FullyIsolated` memory isolation).
    pub fn agent_tier_path_with_user_root(
        &self,
        agent_id: &str,
        tier_name: &str,
        scope: &TierScope,
        goal_id: Option<&str>,
        user_root_override: Option<&Path>,
    ) -> Result<PathBuf, AgentStorageError> {
        let tier = sanitize_segment(tier_name);
        let filename = match scope {
            TierScope::Agent => format!("{tier}.json"),
            TierScope::AgentGoal => {
                let Some(goal) = goal_id else {
                    return Err(AgentStorageError::MissingGoalId {
                        tier_name: tier_name.to_string(),
                    });
                };
                validate_identifier(goal)?;
                format!("{tier}_{}.json", sanitize_segment(goal))
            },
            TierScope::User => format!("{tier}.json"),
        };

        match scope {
            TierScope::User => {
                let base = user_root_override
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.user_root());
                Ok(base.join(filename))
            },
            _ => Ok(self.agent_tiers_dir(agent_id)?.join(filename)),
        }
    }

    pub fn user_profile_path(&self) -> PathBuf {
        self.user_root().join("profile.json")
    }

    pub fn user_knowledge_path(&self) -> PathBuf {
        self.user_root().join("knowledge.json")
    }

    /// Work-evidence graph: user-owned (passive/ambient) evidence + entity
    /// collections, shared across the user's agents (the ambient lane is not
    /// tied to one agent's task). Sibling of the agent-scoped `evidence.json`.
    pub fn user_work_evidence_path(&self) -> PathBuf {
        self.user_root().join("work_evidence.json")
    }

    pub fn user_work_entities_path(&self) -> PathBuf {
        self.user_root().join("work_entities.json")
    }

    pub async fn load_user_knowledge(&self) -> Result<serde_json::Value, AgentStorageError> {
        let path = self.user_knowledge_path();
        match self.read_json(&path).await {
            Ok(val) => Ok(val),
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(serde_json::Value::Object(serde_json::Map::new()))
            },
            Err(err) => Err(err),
        }
    }

    pub async fn load_native_tier_record<T: DeserializeOwned>(
        &self,
        agent_id: &str,
        tier_name: &str,
        scope: &TierScope,
        goal_id: Option<&str>,
    ) -> Result<Option<T>, AgentStorageError> {
        let path = self.agent_tier_path(agent_id, tier_name, scope, goal_id)?;
        match self.read_json::<T>(&path).await {
            Ok(val) => Ok(Some(val)),
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            },
            Err(err) => Err(err),
        }
    }

    pub fn scheduler_state_path(&self) -> PathBuf {
        self.agents_root().join("scheduler_state.json")
    }

    pub fn scheduler_write_lock_path(&self) -> PathBuf {
        self.agents_root().join(".scheduler_state.write.lock")
    }

    pub fn approvals_dir(&self) -> PathBuf {
        self.agents_root().join("approvals")
    }

    pub fn proposals_dir(&self) -> PathBuf {
        self.agents_root().join("proposals")
    }

    pub fn trust_policies_path(&self) -> PathBuf {
        self.system_root().join("trust_policies.yaml")
    }

    pub fn trust_policies_template_path(&self) -> PathBuf {
        self.system_root().join("trust_policies.template.yaml")
    }

    pub fn trust_policies_default_path(&self) -> PathBuf {
        self.system_root().join("trust_policies.default.yaml")
    }

    pub fn system_memory_consolidation_path(&self) -> PathBuf {
        self.system_root().join("memory_consolidation.yaml")
    }

    pub fn channels_path(&self) -> PathBuf {
        self.system_root().join("channels.yaml")
    }

    /// Derive the advisory lock file path for a given data file.
    ///
    /// Returns `{parent}/.{filename}.flock` so the lock sentinel lives beside
    /// the data file on the same filesystem (required for correct advisory
    /// locking semantics).
    pub fn file_lock_path(data_path: &Path) -> PathBuf {
        let filename = data_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        data_path.with_file_name(format!(".{filename}.flock"))
    }

    /// Synchronous counterpart to [`Self::acquire_file_lock_exclusive`] for
    /// durability boundaries that are themselves synchronous. It uses the
    /// same bounded wait and sentinel identity checks, so callers never fall
    /// back to an unbounded blocking `flock` or split authority after a lock
    /// path replacement.
    pub fn acquire_file_lock_exclusive_sync(
        data_path: &Path,
    ) -> Result<FileLockGuard, AgentStorageError> {
        let lock_path = Self::file_lock_path(data_path);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&lock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o777 != 0o600
                || metadata.nlink() != 1
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "agent-storage lock failed owner/mode/link validation",
                )
                .into());
            }
        }

        let started = Instant::now();
        let max_wait = FILE_LOCK_DEFAULT_TIMEOUT;
        let mut retry_delay = FILE_LOCK_RETRY_DELAY_MIN;
        loop {
            match file.try_lock() {
                Ok(()) => {
                    validate_locked_file_path_identity(
                        &file,
                        &lock_path,
                        "agent-storage exclusive lock",
                    )?;
                    return Ok(FileLockGuard {
                        _file: file,
                        _lock_path: lock_path,
                    });
                },
                Err(std::fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= max_wait {
                        return Err(AgentStorageError::FileLockTimeout {
                            lock_path: lock_path.display().to_string(),
                            wait_ms: u64::try_from(max_wait.as_millis()).unwrap_or(u64::MAX),
                        });
                    }
                    std::thread::sleep(retry_delay);
                    retry_delay = retry_delay.saturating_mul(2).min(FILE_LOCK_RETRY_DELAY_MAX);
                },
                Err(std::fs::TryLockError::Error(err)) => {
                    return Err(std::io::Error::other(format!(
                        "failed to acquire exclusive file lock on `{}`: {err}",
                        lock_path.display()
                    ))
                    .into());
                },
            }
        }
    }

    /// Try to acquire the same cross-process authority as
    /// [`Self::acquire_file_lock_exclusive_sync`] without waiting for a live
    /// owner. This is intended for bounded maintenance scans: `Ok(None)` means
    /// the sentinel does not exist or another process still owns the lease.
    /// This method never creates authority, while every acquired guard receives
    /// the same sentinel metadata and path-identity validation as a normal
    /// writer.
    pub fn try_acquire_file_lock_exclusive_sync(
        data_path: &Path,
    ) -> Result<Option<FileLockGuard>, AgentStorageError> {
        Self::try_acquire_file_lock_exclusive_sync_inner(data_path, false)
    }

    /// Try to create and acquire the same cross-process authority as
    /// [`Self::acquire_file_lock_exclusive_sync`] without waiting for a live
    /// owner. Unlike [`Self::try_acquire_file_lock_exclusive_sync`], this
    /// creates the stable sentinel when it is absent. Process-lifetime
    /// single-writer services use this at startup so a competing upgraded
    /// process fails closed immediately instead of stalling an async runtime
    /// thread for the ordinary bounded lock timeout.
    pub fn try_create_file_lock_exclusive_sync(
        data_path: &Path,
    ) -> Result<Option<FileLockGuard>, AgentStorageError> {
        Self::try_acquire_file_lock_exclusive_sync_inner(data_path, true)
    }

    fn try_acquire_file_lock_exclusive_sync_inner(
        data_path: &Path,
        create: bool,
    ) -> Result<Option<FileLockGuard>, AgentStorageError> {
        let lock_path = Self::file_lock_path(data_path);
        if create {
            if let Some(parent) = lock_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .truncate(false)
            .create(create);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(&lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o777 != 0o600
                || metadata.nlink() != 1
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "agent-storage lock failed owner/mode/link validation",
                )
                .into());
            }
        }

        match file.try_lock() {
            Ok(()) => {
                validate_locked_file_path_identity(
                    &file,
                    &lock_path,
                    "agent-storage nonblocking exclusive lock",
                )?;
                Ok(Some(FileLockGuard {
                    _file: file,
                    _lock_path: lock_path,
                }))
            },
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(err)) => Err(std::io::Error::other(format!(
                "failed to try exclusive file lock on `{}`: {err}",
                lock_path.display()
            ))
            .into()),
        }
    }

    /// Acquire a cross-process **exclusive** advisory lock on a data file.
    ///
    /// The returned [`FileLockGuard`] keeps the lock held until dropped.
    /// Callers should hold the guard for the entire read-modify-write window
    /// when they need to prevent concurrent access from other processes.
    ///
    /// Uses `try_lock_exclusive()` with exponential backoff instead of the
    /// blocking `lock_exclusive()` to avoid indefinite hangs when a peer
    /// process holds the lock.
    ///
    /// This complements (but does not replace) in-process async mutexes.
    pub async fn acquire_file_lock_exclusive(
        data_path: &Path,
    ) -> Result<FileLockGuard, AgentStorageError> {
        let lock_path = Self::file_lock_path(data_path);
        let lock_path_for_open = lock_path.clone();
        let mut file = task::spawn_blocking(move || {
            if let Some(parent) = lock_path_for_open.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.create(true).read(true).write(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let file = options.open(&lock_path_for_open)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                let metadata = file.metadata()?;
                if !metadata.is_file()
                    || metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.mode() & 0o777 != 0o600
                    || metadata.nlink() != 1
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "agent-storage lock failed owner/mode/link validation",
                    ));
                }
            }
            Ok::<std::fs::File, std::io::Error>(file)
        })
        .await
        .map_err(|join_err| {
            std::io::Error::other(format!("failed to join file-lock open task: {join_err}"))
        })??;

        let started = Instant::now();
        let max_wait = FILE_LOCK_DEFAULT_TIMEOUT;
        let mut retry_delay = FILE_LOCK_RETRY_DELAY_MIN;

        loop {
            let (next_file, lock_result) = task::spawn_blocking(move || {
                let lock_result = file.try_lock();
                (file, lock_result)
            })
            .await
            .map_err(|join_err| {
                std::io::Error::other(format!("failed to join file-lock attempt task: {join_err}"))
            })?;
            file = next_file;

            match lock_result {
                Ok(()) => {
                    validate_locked_file_path_identity(
                        &file,
                        &lock_path,
                        "agent-storage exclusive lock",
                    )?;
                    return Ok(FileLockGuard {
                        _file: file,
                        _lock_path: lock_path,
                    });
                },
                Err(std::fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= max_wait {
                        return Err(AgentStorageError::FileLockTimeout {
                            lock_path: lock_path.display().to_string(),
                            wait_ms: u64::try_from(max_wait.as_millis()).unwrap_or(u64::MAX),
                        });
                    }
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = retry_delay.saturating_mul(2).min(FILE_LOCK_RETRY_DELAY_MAX);
                },
                Err(std::fs::TryLockError::Error(err)) => {
                    return Err(std::io::Error::other(format!(
                        "failed to acquire exclusive file lock on `{}`: {err}",
                        lock_path.display()
                    ))
                    .into());
                },
            }
        }
    }

    /// Acquire a cross-process **shared** advisory lock on a data file.
    ///
    /// Multiple readers can hold shared locks concurrently; a shared lock
    /// blocks exclusive-lock acquisition (and vice versa).
    ///
    /// Uses `try_lock_shared()` with exponential backoff instead of the
    /// blocking `lock_shared()` to avoid indefinite hangs when a peer
    /// process holds an exclusive lock.
    pub async fn acquire_file_lock_shared(
        data_path: &Path,
    ) -> Result<FileLockGuard, AgentStorageError> {
        let lock_path = Self::file_lock_path(data_path);
        let lock_path_for_open = lock_path.clone();
        let mut file = task::spawn_blocking(move || {
            if let Some(parent) = lock_path_for_open.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.create(true).read(true).write(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let file = options.open(&lock_path_for_open)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                let metadata = file.metadata()?;
                if !metadata.is_file()
                    || metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.mode() & 0o777 != 0o600
                    || metadata.nlink() != 1
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "agent-storage shared lock failed owner/mode/link validation",
                    ));
                }
            }
            Ok::<std::fs::File, std::io::Error>(file)
        })
        .await
        .map_err(|join_err| {
            std::io::Error::other(format!("failed to join file-lock open task: {join_err}"))
        })??;

        let started = Instant::now();
        let max_wait = FILE_LOCK_DEFAULT_TIMEOUT;
        let mut retry_delay = FILE_LOCK_RETRY_DELAY_MIN;

        loop {
            let (next_file, lock_result) = task::spawn_blocking(move || {
                let lock_result = file.try_lock_shared();
                (file, lock_result)
            })
            .await
            .map_err(|join_err| {
                std::io::Error::other(format!("failed to join file-lock attempt task: {join_err}"))
            })?;
            file = next_file;

            match lock_result {
                Ok(()) => {
                    validate_locked_file_path_identity(
                        &file,
                        &lock_path,
                        "agent-storage shared lock",
                    )?;
                    return Ok(FileLockGuard {
                        _file: file,
                        _lock_path: lock_path,
                    });
                },
                Err(std::fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= max_wait {
                        return Err(AgentStorageError::FileLockTimeout {
                            lock_path: lock_path.display().to_string(),
                            wait_ms: u64::try_from(max_wait.as_millis()).unwrap_or(u64::MAX),
                        });
                    }
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = retry_delay.saturating_mul(2).min(FILE_LOCK_RETRY_DELAY_MAX);
                },
                Err(std::fs::TryLockError::Error(err)) => {
                    return Err(std::io::Error::other(format!(
                        "failed to acquire shared file lock on `{}`: {err}",
                        lock_path.display()
                    ))
                    .into());
                },
            }
        }
    }

    pub async fn ensure_base_layout(&self) -> Result<(), AgentStorageError> {
        let system_root_preexisting = fs::try_exists(self.system_root()).await?;
        let scoped_template_sources = if let Some(workspace) = self.scoped_workspace_layout() {
            let template_source_path = workspace.system_trust_policy_template_path();
            let default_source_path = workspace.system_trust_policy_default_path();
            // A read-only seed (container/deployment) ships these trust templates;
            // only write them when the templates root is writable (dev/local_file
            // where seed == store). The paths are still returned so the per-scope
            // materialization reads them from the seed.
            if !workspace.templates_are_read_only() {
                let template_root = workspace.system_trust_policy_template_root();
                fs::create_dir_all(&template_root).await?;
                if !fs::try_exists(&template_source_path).await? {
                    fs::write(
                        &template_source_path,
                        TrustPolicyFile::recommended_template_yaml().as_bytes(),
                    )
                    .await?;
                }
                if !fs::try_exists(&default_source_path).await? {
                    fs::write(
                        &default_source_path,
                        TrustPolicyFile::recommended_defaults_yaml().as_bytes(),
                    )
                    .await?;
                }
            }
            Some((template_source_path, default_source_path))
        } else {
            None
        };

        if matches!(self.memory_layout, AgentMemoryLayout::ScopedV3) {
            let dirs = [
                self.agents_root(),
                self.approvals_dir(),
                self.proposals_dir(),
                self.system_root(),
                self.user_root(),
            ];

            for dir in dirs {
                self.ensure_existing_path_within_root(&dir)?;
                fs::create_dir_all(&dir).await?;
                self.ensure_existing_path_within_root(&dir)?;
            }

            self.ensure_trust_policy_files(
                system_root_preexisting,
                scoped_template_sources.as_ref(),
            )
            .await?;
            return Ok(());
        }

        let dirs = [
            self.agents_root(),
            self.approvals_dir(),
            self.proposals_dir(),
            self.system_root(),
            self.user_root(),
        ];

        for dir in dirs {
            self.ensure_existing_path_within_root(&dir)?;
            fs::create_dir_all(&dir).await?;
            self.ensure_existing_path_within_root(&dir)?;
        }

        self.ensure_trust_policy_files(system_root_preexisting, scoped_template_sources.as_ref())
            .await?;

        Ok(())
    }

    async fn ensure_trust_policy_files(
        &self,
        system_root_preexisting: bool,
        scoped_template_sources: Option<&(PathBuf, PathBuf)>,
    ) -> Result<(), AgentStorageError> {
        let trust_policy_template_path = self.trust_policies_template_path();
        if !fs::try_exists(&trust_policy_template_path).await? {
            match scoped_template_sources {
                Some((template_source_path, _)) => {
                    let bytes = fs::read(template_source_path).await?;
                    self.write_bytes_atomic(&trust_policy_template_path, &bytes)
                        .await?;
                },
                None => {
                    self.write_bytes_atomic(
                        &trust_policy_template_path,
                        TrustPolicyFile::recommended_template_yaml().as_bytes(),
                    )
                    .await?;
                },
            }
        }

        let trust_policy_default_path = self.trust_policies_default_path();
        if !fs::try_exists(&trust_policy_default_path).await? {
            match scoped_template_sources {
                Some((_, default_source_path)) => {
                    let bytes = fs::read(default_source_path).await?;
                    self.write_bytes_atomic(&trust_policy_default_path, &bytes)
                        .await?;
                },
                None => {
                    self.write_bytes_atomic(
                        &trust_policy_default_path,
                        TrustPolicyFile::recommended_defaults_yaml().as_bytes(),
                    )
                    .await?;
                },
            }
        }

        let trust_policy_path = self.trust_policies_path();
        if !fs::try_exists(&trust_policy_path).await? && !system_root_preexisting {
            match scoped_template_sources {
                Some((_, default_source_path)) => {
                    let bytes = fs::read(default_source_path).await?;
                    self.write_bytes_atomic(&trust_policy_path, &bytes).await?;
                },
                None => {
                    self.write_bytes_atomic(
                        &trust_policy_path,
                        TrustPolicyFile::recommended_defaults_yaml().as_bytes(),
                    )
                    .await?;
                },
            }
        }

        Ok(())
    }

    pub async fn ensure_agent_layout(&self, agent_id: &str) -> Result<(), AgentStorageError> {
        let dirs = if matches!(self.memory_layout, AgentMemoryLayout::ScopedV3) {
            vec![
                self.agent_dir(agent_id)?,
                self.agent_tiers_dir(agent_id)?,
                self.agent_episodes_dir(agent_id)?,
                self.agent_state_dir(agent_id)?,
            ]
        } else {
            vec![
                self.agent_dir(agent_id)?,
                self.agent_definition_versions_dir(agent_id)?,
                self.agent_tiers_dir(agent_id)?,
                self.agent_episodes_dir(agent_id)?,
                self.agent_consolidations_dir(agent_id)?,
                self.agent_state_dir(agent_id)?,
            ]
        };

        for dir in dirs {
            self.ensure_existing_path_within_root(&dir)?;
            fs::create_dir_all(&dir).await?;
            self.ensure_existing_path_within_root(&dir)?;
        }

        Ok(())
    }

    pub async fn write_bytes_atomic(
        &self,
        path: impl AsRef<Path>,
        bytes: &[u8],
    ) -> Result<(), AgentStorageError> {
        let path = path.as_ref();
        if let Some(workspace) = self.scoped_workspace_layout() {
            self.ensure_existing_path_within_root(path)?;
            if crate::magician_v2::agent_owners::store_for_any_owner(&workspace, path).is_some() {
                crate::magician_v2::agent_owners::persist_agent_file(&workspace, path, bytes)
                    .await
                    .map_err(|err| AgentStorageError::Io(std::io::Error::other(err)))?;
                return Ok(());
            }
            workspace
                .write_atomic_path(path, bytes)
                .await
                .map_err(agent_storage_provider_error)?;
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
            self.ensure_existing_path_within_root(parent)?;
        }

        let (tmp_path, mut file) = {
            let pid = std::process::id();
            let mut attempts = 0_u8;
            loop {
                let counter = TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
                let hash = blake3::hash(path.as_os_str().as_encoded_bytes())
                    .to_hex()
                    .to_string();
                let candidate = path.with_file_name(format!(".tmp.{pid}.{counter}.{hash}"));
                match fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&candidate)
                    .await
                {
                    Ok(file) => break (candidate, file),
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && attempts < 8 => {
                        attempts += 1;
                    },
                    Err(err) => return Err(err.into()),
                }
            }
        };

        let write_result = async {
            file.write_all(bytes).await?;
            file.flush().await?;
            file.sync_all().await?;
            Ok::<(), std::io::Error>(())
        }
        .await;
        drop(file);

        if let Err(err) = write_result {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(err.into());
        }

        if let Err(err) = fs::rename(&tmp_path, path).await {
            if let Err(cleanup_err) = fs::remove_file(&tmp_path).await {
                warn!(
                    tmp_path = %tmp_path.display(),
                    error = %cleanup_err,
                    "failed to remove temp file after atomic rename failure"
                );
            }
            return Err(err.into());
        }

        // Fsync the parent directory so the rename is durable on crash.
        // Use spawn_blocking so filesystem sync does not block a Tokio worker thread.
        if let Some(parent) = path.parent() {
            let parent = parent.to_path_buf();
            if let Err(join_err) = task::spawn_blocking(move || {
                if let Ok(dir) = std::fs::File::open(parent) {
                    let _ = dir.sync_all();
                }
            })
            .await
            {
                warn!(
                    error = %join_err,
                    "failed to join parent-directory fsync task after atomic rename"
                );
            }
        }
        Ok(())
    }

    pub async fn create_dir_all(&self, path: impl AsRef<Path>) -> Result<(), AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            workspace
                .create_dir_all_path(path)
                .await
                .map_err(agent_storage_provider_error)?;
            return Ok(());
        }
        fs::create_dir_all(path).await?;
        Ok(())
    }

    pub async fn exists(&self, path: impl AsRef<Path>) -> Result<bool, AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            return workspace
                .exists_path(path)
                .await
                .map_err(agent_storage_provider_error);
        }
        Ok(fs::try_exists(path).await?)
    }

    pub fn exists_sync(&self, path: impl AsRef<Path>) -> Result<bool, AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            return workspace
                .exists_path_sync(path)
                .map_err(agent_storage_provider_error);
        }
        Ok(path.try_exists()?)
    }

    pub async fn remove_file(&self, path: impl AsRef<Path>) -> Result<(), AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            match workspace.remove_file_path(path).await {
                Ok(()) => return Ok(()),
                Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(());
                },
                Err(error) => return Err(agent_storage_provider_error(error)),
            }
        }
        match fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn remove_dir_all(&self, path: impl AsRef<Path>) -> Result<(), AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            match workspace.remove_dir_all_path(path).await {
                Ok(()) => return Ok(()),
                Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(());
                },
                Err(error) => return Err(agent_storage_provider_error(error)),
            }
        }
        match fs::remove_dir_all(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn read_bytes(&self, path: impl AsRef<Path>) -> Result<Vec<u8>, AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            return workspace
                .read_path(path)
                .await
                .map_err(agent_storage_provider_error);
        }
        Ok(fs::read(path).await?)
    }

    pub async fn append_bytes(
        &self,
        path: impl AsRef<Path>,
        bytes: &[u8],
    ) -> Result<(), AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            workspace
                .append_path(path, bytes)
                .await
                .map_err(agent_storage_provider_error)?;
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        Ok(())
    }

    pub async fn append_jsonl<T: Serialize>(
        &self,
        path: impl AsRef<Path>,
        value: &T,
    ) -> Result<(), AgentStorageError> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.append_bytes(path, &line).await
    }

    pub async fn write_json_atomic<T: Serialize>(
        &self,
        path: impl AsRef<Path>,
        value: &T,
    ) -> Result<(), AgentStorageError> {
        let content = serde_json::to_vec_pretty(value)?;
        self.write_bytes_atomic(path, &content).await
    }

    pub async fn write_yaml_atomic<T: Serialize>(
        &self,
        path: impl AsRef<Path>,
        value: &T,
    ) -> Result<(), AgentStorageError> {
        let content = serde_yaml::to_string(value)?;
        self.write_bytes_atomic(path, content.as_bytes()).await
    }

    pub async fn read_to_string(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<String, AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            return workspace
                .read_to_string_path(path)
                .await
                .map_err(agent_storage_provider_error);
        }
        fs::read_to_string(path).await.map_err(|err| {
            AgentStorageError::Io(std::io::Error::new(
                err.kind(),
                format!("failed to read `{}`: {err}", path.display()),
            ))
        })
    }

    pub fn read_to_string_sync(&self, path: impl AsRef<Path>) -> Result<String, AgentStorageError> {
        let path = path.as_ref();
        self.ensure_existing_path_within_root(path)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            return workspace
                .read_to_string_path_sync(path)
                .map_err(agent_storage_provider_error);
        }
        std::fs::read_to_string(path).map_err(|err| {
            AgentStorageError::Io(std::io::Error::new(
                err.kind(),
                format!("failed to read `{}`: {err}", path.display()),
            ))
        })
    }

    pub async fn read_json<T: DeserializeOwned>(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<T, AgentStorageError> {
        let content = self.read_to_string(path).await?;
        Ok(serde_json::from_str(&content)?)
    }

    pub async fn read_yaml<T: DeserializeOwned>(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<T, AgentStorageError> {
        let content = self.read_to_string(path).await?;
        Ok(serde_yaml::from_str(&content)?)
    }

    pub async fn list_files_with_extension(
        &self,
        dir: impl AsRef<Path>,
        extension: &str,
    ) -> Result<Vec<PathBuf>, AgentStorageError> {
        let dir = dir.as_ref();
        self.ensure_existing_path_within_root(dir)?;

        if let Some(workspace) = self.scoped_workspace_layout() {
            let entries = match workspace.read_dir_path(dir).await {
                Ok(entries) => entries,
                Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Vec::new());
                },
                Err(error) => return Err(agent_storage_provider_error(error)),
            };
            let mut files = entries
                .into_iter()
                .filter(|entry| entry.is_file)
                .filter(|entry| !entry.file_name.starts_with('.'))
                .map(|entry| workspace.base_root().join(entry.relative_path))
                .filter(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext == extension)
                })
                .collect::<Vec<_>>();
            files.sort();
            return Ok(files);
        }

        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        let mut entries = fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !entry.file_type().await?.is_file() {
                continue;
            }

            // Skip dotfiles — these include in-progress temp files from write_bytes_atomic
            // (`.{name}.tmp.{pid}.{counter}`) that may linger after interrupted writes.
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }

            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == extension)
            {
                files.push(path);
            }
        }

        files.sort();
        Ok(files)
    }

    pub async fn list_child_dirs(
        &self,
        dir: impl AsRef<Path>,
    ) -> Result<Vec<PathBuf>, AgentStorageError> {
        let dir = dir.as_ref();
        self.ensure_existing_path_within_root(dir)?;
        if let Some(workspace) = self.scoped_workspace_layout() {
            let entries = match workspace.read_dir_path(dir).await {
                Ok(entries) => entries,
                Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Vec::new());
                },
                Err(error) => return Err(agent_storage_provider_error(error)),
            };
            let mut dirs = entries
                .into_iter()
                .filter(|entry| entry.is_dir)
                .map(|entry| workspace.base_root().join(entry.relative_path))
                .collect::<Vec<_>>();
            dirs.sort();
            return Ok(dirs);
        }
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut dirs = Vec::new();
        let mut entries = fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                dirs.push(entry.path());
            }
        }
        dirs.sort();
        Ok(dirs)
    }

    fn ensure_existing_path_within_root(&self, path: &Path) -> Result<(), AgentStorageError> {
        if !self.root.exists() || !path.exists() {
            return Ok(());
        }

        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Err(AgentStorageError::PathOutsideRoot {
                path: path.display().to_string(),
                root: self.root.display().to_string(),
            });
        }

        let canonical_root = std::fs::canonicalize(&self.root)?;
        let canonical_path = std::fs::canonicalize(path)?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(AgentStorageError::PathOutsideRoot {
                path: path.display().to_string(),
                root: self.root.display().to_string(),
            });
        }

        Ok(())
    }
}

fn ensure_private_directory_blocking(root: &Path, path: &Path) -> Result<(), AgentStorageError> {
    if !path.starts_with(root) {
        return Err(AgentStorageError::PathOutsideRoot {
            path: path.display().to_string(),
            root: root.display().to_string(),
        });
    }
    std::fs::create_dir_all(root)?;
    let root_metadata = std::fs::symlink_metadata(root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(AgentStorageError::PathOutsideRoot {
            path: root.display().to_string(),
            root: root.display().to_string(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if root_metadata.uid() != unsafe { libc::geteuid() } {
            return Err(AgentStorageError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "agent-storage root has the wrong owner",
            )));
        }
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        let root_rechecked = std::fs::symlink_metadata(root)?;
        if root_rechecked.mode() & 0o777 != 0o700
            || root_rechecked.uid() != unsafe { libc::geteuid() }
        {
            return Err(AgentStorageError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "private agent-storage root failed post-mode validation",
            )));
        }
    }
    let canonical_root = std::fs::canonicalize(root)?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| AgentStorageError::PathOutsideRoot {
            path: path.display().to_string(),
            root: root.display().to_string(),
        })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(segment) = component else {
            return Err(AgentStorageError::PathOutsideRoot {
                path: path.display().to_string(),
                root: root.display().to_string(),
            });
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(AgentStorageError::PathOutsideRoot {
                    path: current.display().to_string(),
                    root: root.display().to_string(),
                });
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&current) {
                    Ok(()) => {},
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
                    Err(error) => return Err(error.into()),
                }
            },
            Err(error) => return Err(error.into()),
        }
        let metadata = std::fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || !std::fs::canonicalize(&current)?.starts_with(&canonical_root)
        {
            return Err(AgentStorageError::PathOutsideRoot {
                path: current.display().to_string(),
                root: root.display().to_string(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(AgentStorageError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "private agent-storage directory has the wrong owner",
                )));
            }
            std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o700))?;
            let rechecked = std::fs::symlink_metadata(&current)?;
            if rechecked.file_type().is_symlink()
                || rechecked.mode() & 0o777 != 0o700
                || rechecked.uid() != unsafe { libc::geteuid() }
            {
                return Err(AgentStorageError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "private agent-storage directory failed post-mode validation",
                )));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_private_file_nofollow_unix(
    root: &Path,
    path: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, std::io::Error> {
    use std::{
        ffi::CString,
        io::Read as _,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        },
    };

    fn c_string(value: &std::ffi::OsStr) -> Result<CString, std::io::Error> {
        CString::new(value.as_bytes()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "private path contains an embedded NUL",
            )
        })
    }

    fn fstat(fd: i32) -> Result<libc::stat, std::io::Error> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { stat.assume_init() })
    }

    fn require_private_directory(fd: i32) -> Result<(), std::io::Error> {
        let stat = fstat(fd)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
            || stat.st_uid != unsafe { libc::geteuid() }
            || stat.st_mode & 0o777 != 0o700
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "private directory descriptor failed owner/mode/type validation",
            ));
        }
        Ok(())
    }

    if !path.starts_with(root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private path escapes its storage root",
        ));
    }
    let relative = path.strip_prefix(root).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private path escapes its storage root",
        )
    })?;
    let components = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(value) => Ok(value.to_owned()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "private path contains a non-normal component",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (leaf, parents) = components.split_last().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private read requires a file below the storage root",
        )
    })?;
    let root_name = c_string(root.as_os_str())?;
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    require_private_directory(directory.as_raw_fd())?;
    for component in parents {
        let component = c_string(component.as_os_str())?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let next = unsafe { OwnedFd::from_raw_fd(fd) };
        require_private_directory(next.as_raw_fd())?;
        directory = next;
    }
    let leaf = c_string(leaf.as_os_str())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let file_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut stat = fstat(file_fd.as_raw_fd())?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_uid != unsafe { libc::geteuid() }
        || stat.st_nlink != 1
        || stat.st_size < 0
        || u64::try_from(stat.st_size).unwrap_or(u64::MAX) > max_bytes as u64
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private file descriptor failed owner/type/link/size validation",
        ));
    }
    // Every parent directory above was proven euid-owned and 0700, so a
    // euid-owned regular file with a single link inside that chain cannot have
    // been placed there by anyone else. Slack mode bits on it are our own
    // hygiene failure rather than a tampering signal: `write_private_bytes_atomic`
    // renames the staged file into place and only then chmods it, so a crash in
    // that window leaves a readable-by-group/other file behind. Refusing to read
    // it is a trap with no exit, because the rewrite that would repair the mode
    // is gated behind this very read. Tighten the descriptor we already hold —
    // no path is re-resolved, so there is nothing to swap underneath us — and a
    // mode we cannot tighten still fails closed.
    if stat.st_mode & 0o777 != 0o600 {
        if unsafe { libc::fchmod(file_fd.as_raw_fd(), 0o600) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        stat = fstat(file_fd.as_raw_fd())?;
        if stat.st_mode & 0o777 != 0o600 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "private file descriptor could not be tightened to owner-only",
            ));
        }
        tracing::warn!(
            target: "magician::agents::storage",
            path = %path.display(),
            "repaired a private agent-storage file that was left readable beyond its owner"
        );
    }
    let mut file = std::fs::File::from(file_fd);
    let mut bytes = Vec::with_capacity(usize::try_from(stat.st_size).unwrap_or(max_bytes));
    file.by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private file exceeds its byte ceiling",
        ));
    }
    Ok(bytes)
}

fn validate_private_path_components(
    root: &Path,
    path: &Path,
    leaf_may_be_missing: bool,
) -> Result<(), AgentStorageError> {
    if !path.starts_with(root) || !root.exists() {
        return Err(AgentStorageError::PathOutsideRoot {
            path: path.display().to_string(),
            root: root.display().to_string(),
        });
    }
    let canonical_root = std::fs::canonicalize(root)?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| AgentStorageError::PathOutsideRoot {
            path: path.display().to_string(),
            root: root.display().to_string(),
        })?;
    let mut current = root.to_path_buf();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        let std::path::Component::Normal(segment) = component else {
            return Err(AgentStorageError::PathOutsideRoot {
                path: path.display().to_string(),
                root: root.display().to_string(),
            });
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !std::fs::canonicalize(&current)?.starts_with(&canonical_root)
                {
                    return Err(AgentStorageError::PathOutsideRoot {
                        path: current.display().to_string(),
                        root: root.display().to_string(),
                    });
                }
            },
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && leaf_may_be_missing
                    && index + 1 == component_count => {},
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn agent_storage_provider_error(error: ArtifactV2Error) -> AgentStorageError {
    match error {
        ArtifactV2Error::Io(error) => AgentStorageError::Io(error),
        other => AgentStorageError::Io(std::io::Error::other(other)),
    }
}

// ── magician-vector-index trait impl ───────────────────────────────────────
//
// The memory-index and memory-candidate code now lives in
// `magician-vector-index`. This adapter lets that code consume our
// concrete `AgentStorage` through a trait object without duplicating any
// path/IO logic.

impl From<AgentStorageError> for magician_vector_index::storage_trait::MemoryStorageError {
    fn from(value: AgentStorageError) -> Self {
        use magician_vector_index::storage_trait::MemoryStorageError as E;
        match value {
            AgentStorageError::InvalidIdentifier(id) => E::InvalidIdentifier(id),
            AgentStorageError::PathOutsideRoot { path, root } => E::PathOutsideRoot { path, root },
            AgentStorageError::MissingGoalId { tier_name } => E::MissingGoalId { tier_name },
            AgentStorageError::Io(io) => E::Io(io),
            AgentStorageError::Json(json) => E::Json(json),
            AgentStorageError::Yaml(err) => E::Other(err.to_string()),
            AgentStorageError::FileLockTimeout { lock_path, wait_ms } => {
                E::FileLockTimeout { lock_path, wait_ms }
            },
        }
    }
}

#[async_trait::async_trait]
impl magician_vector_index::storage_trait::MemoryStorage for AgentStorage {
    fn root(&self) -> &Path {
        AgentStorage::root(self)
    }

    fn scope_segments(&self) -> Option<(String, String)> {
        AgentStorage::scope_segments(self)
    }

    fn agent_tiers_dir(
        &self,
        agent_id: &str,
    ) -> Result<PathBuf, magician_vector_index::storage_trait::MemoryStorageError> {
        AgentStorage::agent_tiers_dir(self, agent_id).map_err(Into::into)
    }

    fn agent_episodes_dir(
        &self,
        agent_id: &str,
    ) -> Result<PathBuf, magician_vector_index::storage_trait::MemoryStorageError> {
        AgentStorage::agent_episodes_dir(self, agent_id).map_err(Into::into)
    }

    fn agent_tier_path(
        &self,
        agent_id: &str,
        tier_name: &str,
        scope: &TierScope,
        goal_id: Option<&str>,
    ) -> Result<PathBuf, magician_vector_index::storage_trait::MemoryStorageError> {
        AgentStorage::agent_tier_path(self, agent_id, tier_name, scope, goal_id).map_err(Into::into)
    }

    fn memory_lancedb_index_dir(&self) -> PathBuf {
        AgentStorage::memory_lancedb_index_dir(self)
    }

    fn memory_index_documents_path(&self) -> PathBuf {
        AgentStorage::memory_index_documents_path(self)
    }

    fn memory_index_manifest_path(&self) -> PathBuf {
        AgentStorage::memory_index_manifest_path(self)
    }

    fn user_knowledge_path(&self) -> PathBuf {
        AgentStorage::user_knowledge_path(self)
    }

    fn user_root(&self) -> PathBuf {
        AgentStorage::user_root(self)
    }

    async fn load_user_knowledge(
        &self,
    ) -> Result<serde_json::Value, magician_vector_index::storage_trait::MemoryStorageError> {
        AgentStorage::load_user_knowledge(self)
            .await
            .map_err(Into::into)
    }

    async fn load_native_tier_value(
        &self,
        agent_id: &str,
        tier_name: &str,
        scope: &TierScope,
        goal_id: Option<&str>,
    ) -> Result<Option<serde_json::Value>, magician_vector_index::storage_trait::MemoryStorageError>
    {
        AgentStorage::load_native_tier_record::<serde_json::Value>(
            self, agent_id, tier_name, scope, goal_id,
        )
        .await
        .map_err(Into::into)
    }

    async fn read_json_value(
        &self,
        path: &Path,
    ) -> Result<serde_json::Value, magician_vector_index::storage_trait::MemoryStorageError> {
        AgentStorage::read_json::<serde_json::Value>(self, path)
            .await
            .map_err(Into::into)
    }

    async fn write_bytes_atomic(
        &self,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), magician_vector_index::storage_trait::MemoryStorageError> {
        AgentStorage::write_bytes_atomic(self, path, bytes)
            .await
            .map_err(Into::into)
    }

    async fn write_json_value_atomic(
        &self,
        path: &Path,
        value: &serde_json::Value,
    ) -> Result<(), magician_vector_index::storage_trait::MemoryStorageError> {
        AgentStorage::write_json_atomic(self, path, value)
            .await
            .map_err(Into::into)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    #[tokio::test]
    async fn ensure_layout_creates_expected_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());

        storage.ensure_base_layout().await.unwrap();
        storage.ensure_agent_layout("agent-1").await.unwrap();

        assert!(storage.agents_root().exists());
        assert!(storage.approvals_dir().exists());
        assert!(storage.proposals_dir().exists());
        assert!(storage.agent_tiers_dir("agent-1").unwrap().exists());
        assert!(storage.agent_episodes_dir("agent-1").unwrap().exists());
        assert!(storage
            .agent_consolidations_dir("agent-1")
            .unwrap()
            .exists());
        assert!(storage.trust_policies_template_path().exists());
        assert!(storage.trust_policies_default_path().exists());
        assert!(storage.trust_policies_path().exists());
        assert!(!storage.system_root().join("meta-agent.agent.yaml").exists());
    }

    #[tokio::test]
    async fn scoped_agent_runtime_hardens_trust_policies_from_global_templates() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path().join("magician_data_v3"));
        let storage =
            AgentStorage::new(workspace.scoped_agent_runtime_root("principal-a", "workspace-a"));

        storage.ensure_base_layout().await.unwrap();

        assert!(workspace.system_trust_policy_template_path().exists());
        assert!(workspace.system_trust_policy_default_path().exists());
        assert!(storage.trust_policies_template_path().exists());
        assert!(storage.trust_policies_default_path().exists());
        assert!(storage.trust_policies_path().exists());

        let global_template =
            tokio::fs::read_to_string(workspace.system_trust_policy_template_path())
                .await
                .unwrap();
        let scoped_template = tokio::fs::read_to_string(storage.trust_policies_template_path())
            .await
            .unwrap();
        assert_eq!(scoped_template, global_template);

        let global_default =
            tokio::fs::read_to_string(workspace.system_trust_policy_default_path())
                .await
                .unwrap();
        let scoped_default = tokio::fs::read_to_string(storage.trust_policies_default_path())
            .await
            .unwrap();
        let scoped_live = tokio::fs::read_to_string(storage.trust_policies_path())
            .await
            .unwrap();
        assert_eq!(scoped_default, global_default);
        assert_eq!(scoped_live, global_default);
        assert!(storage.agents_root().exists());
        assert!(storage.user_root().exists());
        assert!(storage.system_root().exists());
        assert!(!storage.system_root().join("meta-agent.agent.yaml").exists());
        assert!(!storage.system_root().join("workflows").exists());
        assert!(!storage
            .system_root()
            .join("workflows")
            .join("runs")
            .exists());
    }

    #[tokio::test]
    async fn scoped_agent_runtime_does_not_create_runtime_consolidations_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path().join("magician_data_v3"));
        let storage =
            AgentStorage::new(workspace.scoped_agent_runtime_root("principal-a", "workspace-a"));

        storage.ensure_base_layout().await.unwrap();
        storage.ensure_agent_layout("agent-1").await.unwrap();

        assert!(storage.agent_tiers_dir("agent-1").unwrap().exists());
        assert!(storage.agent_episodes_dir("agent-1").unwrap().exists());
        assert!(storage.agent_state_dir("agent-1").unwrap().exists());
        assert!(!storage
            .agent_consolidations_dir("agent-1")
            .unwrap()
            .exists());
    }

    #[tokio::test]
    async fn write_and_read_json_roundtrip() {
        #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
        struct Item {
            value: String,
        }

        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());

        let path = storage.root().join("test.json");
        storage
            .write_json_atomic(
                &path,
                &Item {
                    value: "ok".to_string(),
                },
            )
            .await
            .unwrap();

        let loaded: Item = storage.read_json(path).await.unwrap();
        assert_eq!(
            loaded,
            Item {
                value: "ok".to_string()
            }
        );
    }

    #[tokio::test]
    async fn list_files_with_extension_skips_dotfiles_and_returns_sorted_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        let dir = storage.root().join("episodes");
        fs::create_dir_all(&dir).await.unwrap();
        fs::create_dir_all(dir.join("nested")).await.unwrap();

        fs::write(dir.join("b.json"), "{}").await.unwrap();
        fs::write(dir.join("a.json"), "{}").await.unwrap();
        fs::write(dir.join("c.yaml"), "{}").await.unwrap();
        fs::write(dir.join(".hidden.json"), "{}").await.unwrap();
        fs::write(dir.join("nested").join("z.json"), "{}")
            .await
            .unwrap();

        let listed = storage
            .list_files_with_extension(&dir, "json")
            .await
            .unwrap();
        let names: Vec<String> = listed
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap()
                    .to_string()
            })
            .collect();

        assert_eq!(names, vec!["a.json".to_string(), "b.json".to_string()]);
    }

    #[tokio::test]
    async fn read_json_utf8_error_includes_path_context() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        let path = storage.root().join("bad.json");
        fs::write(&path, vec![0xff, 0xfe, 0xfd]).await.unwrap();

        let err = storage
            .read_json::<serde_json::Value>(&path)
            .await
            .unwrap_err();
        match err {
            AgentStorageError::Io(io_err) => {
                let msg = io_err.to_string();
                assert!(msg.contains(path.to_string_lossy().as_ref()));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_yaml_utf8_error_includes_path_context() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        let path = storage.root().join("bad.yaml");
        fs::write(&path, vec![0xff, 0xfe, 0xfd]).await.unwrap();

        let err = storage
            .read_yaml::<serde_yaml::Value>(&path)
            .await
            .unwrap_err();
        match err {
            AgentStorageError::Io(io_err) => {
                let msg = io_err.to_string();
                assert!(msg.contains(path.to_string_lossy().as_ref()));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_agent_layout_rejects_symlinked_agent_dir_outside_root() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let escape = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        storage.ensure_base_layout().await.unwrap();

        let agent_dir = storage.agents_root().join("agent-1");
        symlink(escape.path(), &agent_dir).unwrap();

        let err = storage.ensure_agent_layout("agent-1").await.unwrap_err();
        match err {
            AgentStorageError::PathOutsideRoot { path, root } => {
                assert!(path.contains("agent-1"));
                assert!(root.contains(tmp.path().to_string_lossy().as_ref()));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_bytes_atomic_rejects_symlinked_parent_outside_root() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let escape = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        storage.ensure_base_layout().await.unwrap();

        let linked_parent = storage.root().join("linked-parent");
        symlink(escape.path(), &linked_parent).unwrap();

        let err = storage
            .write_bytes_atomic(linked_parent.join("escape.txt"), b"nope")
            .await
            .unwrap_err();
        match err {
            AgentStorageError::PathOutsideRoot { path, root } => {
                assert!(path.contains("linked-parent"));
                assert!(root.contains(tmp.path().to_string_lossy().as_ref()));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shared_file_lock_rejects_a_symlinked_sentinel() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("settlement.json");
        let lock_path = AgentStorage::file_lock_path(&data_path);
        let outside = tmp.path().join("outside-lock-target");
        std::fs::write(&outside, b"do not open through the sentinel").unwrap();
        symlink(&outside, &lock_path).unwrap();

        let error = AgentStorage::acquire_file_lock_shared(&data_path)
            .await
            .expect_err("shared lock must not follow a sentinel symlink");
        assert!(matches!(error, AgentStorageError::Io(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shared_file_lock_publishes_a_private_single_link_sentinel() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("settlement.json");
        let guard = AgentStorage::acquire_file_lock_shared(&data_path)
            .await
            .expect("shared lock");
        let metadata = std::fs::symlink_metadata(AgentStorage::file_lock_path(&data_path)).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        drop(guard);
    }

    #[cfg(unix)]
    #[test]
    fn nonblocking_create_lock_excludes_a_second_writer_and_releases() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("process-lifetime-writer");
        let first = AgentStorage::try_create_file_lock_exclusive_sync(&data_path)
            .expect("first acquisition")
            .expect("first writer owns the sentinel");
        assert!(
            AgentStorage::try_create_file_lock_exclusive_sync(&data_path)
                .expect("contended acquisition")
                .is_none()
        );
        let metadata = std::fs::symlink_metadata(AgentStorage::file_lock_path(&data_path)).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);

        drop(first);
        assert!(
            AgentStorage::try_create_file_lock_exclusive_sync(&data_path)
                .expect("successor acquisition")
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn locked_file_path_identity_rejects_a_renamed_replacement() {
        use fs2::FileExt as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("authority.flock");
        let displaced_path = tmp.path().join("authority.displaced");
        let locked = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .unwrap();
        locked
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        locked.lock_exclusive().unwrap();
        validate_locked_file_path_identity(&locked, &lock_path, "test authority")
            .expect("original published inode is exact");

        // Test-only adversarial inode substitution, not store publication.
        std::fs::rename(&lock_path, &displaced_path).unwrap();
        let replacement = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .unwrap();
        replacement
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();

        validate_locked_file_path_identity(&locked, &lock_path, "test authority")
            .expect_err("a replacement inode must not share stale lock authority");
    }

    #[test]
    fn invalid_identifier_is_rejected() {
        assert!(validate_identifier("../bad").is_err());
        assert!(validate_identifier("a/b").is_err());
        assert!(validate_identifier(".").is_err());
        assert!(validate_identifier(" agent ").is_err());
        assert!(validate_identifier(&"a".repeat(256)).is_err());
        assert!(validate_agent_identifier("approvals").is_err());
        assert!(validate_agent_identifier("proposals").is_err());
        assert!(validate_agent_identifier(".definition_store.write.lock").is_err());
        assert!(validate_agent_identifier(".scheduler_state.write.lock").is_err());
        assert!(validate_agent_identifier("agënt").is_err());
        assert!(validate_identifier("ok-id").is_ok());
    }

    #[test]
    fn sanitize_segment_is_injective_for_common_collisions() {
        assert_ne!(sanitize_segment("a/b"), sanitize_segment("a_b"));
        assert_ne!(sanitize_segment("goal"), sanitize_segment("Goal"));
        assert_ne!(
            sanitize_segment("name with space"),
            sanitize_segment("name_with_space")
        );
    }

    #[test]
    fn sanitize_segment_encodes_unsafe_bytes() {
        assert_eq!(sanitize_segment("a/b"), "a~2fb");
        assert_eq!(sanitize_segment("Goal"), "~47oal");
        assert_eq!(sanitize_segment(""), "unnamed");
    }

    #[test]
    fn sanitize_segment_encodes_leading_dots() {
        // Leading dots are encoded as ~2e so the result is never a dotfile
        assert_eq!(sanitize_segment(".hidden"), "~2ehidden");
        assert_eq!(sanitize_segment("..foo"), "~2e~2efoo");
        assert_eq!(sanitize_segment("..."), "~2e~2e~2e");

        // Injectivity: ".foo" and "foo" must produce distinct results
        assert_ne!(sanitize_segment(".foo"), sanitize_segment("foo"));
        assert_ne!(sanitize_segment("..x"), sanitize_segment(".x"));

        // Non-leading dots are still passed through
        assert_eq!(sanitize_segment("a.b"), "a.b");
    }

    #[test]
    fn sanitize_segment_caps_output_length_with_hash_suffix() {
        let raw = "🚀".repeat(120);
        let sanitized = sanitize_segment(&raw);
        assert!(sanitized.len() <= MAX_IDENTIFIER_BYTES);
        assert!(sanitized.starts_with(SANITIZED_SEGMENT_OVERFLOW_PREFIX));
        assert!(sanitized.contains('_'));
    }

    #[test]
    fn sanitize_segment_overflow_remains_stable_and_distinct() {
        let a = "🚀".repeat(120);
        let b = format!("{a}x");
        assert_eq!(sanitize_segment(&a), sanitize_segment(&a));
        assert_ne!(sanitize_segment(&a), sanitize_segment(&b));
    }

    #[test]
    fn agent_episode_index_path_returns_dotfile() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());

        let path = storage.agent_episode_index_path("agent-1").unwrap();
        let file_name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(file_name, ".episode_index.json");
        assert!(
            file_name.starts_with('.'),
            "index file should be a dotfile to be invisible to list_files_with_extension"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn private_bounded_io_rejects_symlink_and_oversize() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        let private = storage.root().join("app-contributions").join("memory-v1");
        storage.ensure_private_directory(&private).await.unwrap();
        let value = private.join("value.json");
        storage
            .write_private_bytes_atomic(&value, b"{}")
            .await
            .unwrap();
        assert_eq!(
            storage.read_private_bytes_bounded(&value, 2).await.unwrap(),
            b"{}"
        );
        assert!(storage.read_private_bytes_bounded(&value, 1).await.is_err());

        let link = private.join("link.json");
        symlink(&value, &link).unwrap();
        assert!(storage.read_private_bytes_bounded(&link, 16).await.is_err());

        let parent_link = storage.root().join("memory-link");
        symlink(&private, &parent_link).unwrap();
        assert!(storage
            .read_private_bytes_bounded(parent_link.join("value.json"), 16)
            .await
            .is_err());

        let hard_link = private.join("hard-link.json");
        std::fs::hard_link(&value, &hard_link).unwrap();
        assert!(storage
            .read_private_bytes_bounded(&hard_link, 16)
            .await
            .is_err());
    }

    /// A crash between the atomic rename and the chmod leaves an owner-written
    /// file at the umask's mode inside a directory chain that is still 0700.
    /// The read must repair it rather than refuse forever: the only writer that
    /// could fix the mode is gated behind this read.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_private_file_left_group_readable_is_tightened_and_still_read() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        let private = storage.root().join("app-contributions").join("memory-v1");
        storage.ensure_private_directory(&private).await.unwrap();
        let value = private.join("projection.json");
        storage
            .write_private_bytes_atomic(&value, b"{}")
            .await
            .unwrap();
        std::fs::set_permissions(&value, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(
            storage.read_private_bytes_bounded(&value, 2).await.unwrap(),
            b"{}"
        );
        assert_eq!(
            std::fs::metadata(&value).unwrap().permissions().mode() & 0o777,
            0o600,
            "the read should have tightened the file it just accepted"
        );
    }
}
