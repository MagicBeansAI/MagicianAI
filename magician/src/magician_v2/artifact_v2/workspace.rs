use std::{
    fmt::Debug,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::magician_v2::json_traversal::{
    discard_json_iteratively, json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded,
    EncodedJsonStreamAdmission, MAX_RETAINED_JSON_DEPTH,
};

use super::{
    io::{
        publish_staged_file_durably_sync, sync_parent_dir, sync_parent_dir_with_admission,
        write_bytes_atomic, write_bytes_atomic_sync, write_canonical_json_value_atomic_stream_sync,
        write_json_atomic_stream_sync, write_json_pretty_atomic_stream_sync,
    },
    service::ArtifactV2Error,
};

pub const DEFAULT_SCOPE_PRINCIPAL: &str = "anonymous";
pub const DEFAULT_SCOPE_WORKSPACE: &str = "default";

/// What a scope is for, and therefore what it is allowed to grow on disk.
///
/// Scopes are created implicitly — any write runs `create_dir_all` on its
/// parent — so a subsystem that merely enumerates scopes materializes itself in
/// all of them. That is how a reserved event bucket came to hold an app store,
/// a mail database and a UI feed for a reader that cannot exist. Eager
/// initialization is fine; initializing a subsystem the scope has no use for is
/// not, and the cost is not only bytes: each per-scope DuckDB spawns a
/// scheduler pool sized to the core count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeProfile {
    /// A real tenant. Every subsystem may materialize here.
    User,
    /// A reserved sink that exists to catch records which belong to no tenant.
    /// It has no owner, no inbox, no UI and no apps, and must stay that way.
    ReservedSink,
}

/// Reserved sinks, named by the constants that define them rather than by
/// spelling: `system`/`system` catches events carrying no scope, and
/// `_quarantine`/`_quarantine` catches scope strings that failed validation.
/// The default user scope is deliberately NOT here — it is a real tenant.
pub fn scope_profile(principal: &str, workspace: &str) -> ScopeProfile {
    use crate::magician_v2::transport_log::{
        QUARANTINE_PRINCIPAL, QUARANTINE_WORKSPACE, SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE,
    };
    let reserved = (principal == SYSTEM_PRINCIPAL && workspace == SYSTEM_WORKSPACE)
        || (principal == QUARANTINE_PRINCIPAL && workspace == QUARANTINE_WORKSPACE);
    if reserved {
        ScopeProfile::ReservedSink
    } else {
        ScopeProfile::User
    }
}

/// Whether a scope may materialize the subsystems that only a tenant uses:
/// apps, mail, UI feed and threads, social, api-mining. Event and analytics
/// state is not gated — that is precisely what a reserved sink is for.
pub fn scope_hosts_user_subsystems(principal: &str, workspace: &str) -> bool {
    scope_profile(principal, workspace) == ScopeProfile::User
}
pub const MAGICIAN_STORAGE_PATH_ENV: &str = "MAGICIAN_STORAGE_PATH";
pub(crate) const WORKSPACE_DESTINATION_CAS_CONFLICT_DETAIL: &str =
    "workspace destination changed before verified publication";
/// Canonical runtime-data-root override. Set by the container/deployment to point
/// the entire runtime store (scopes, runtime system state, settings) at a host
/// path, independent of the read-only seed in `magician_data_v3`. Takes precedence
/// over the legacy `MAGICIAN_STORAGE_PATH`.
pub const MAGICIAN_ROOT_DIR_ENV: &str = "MAGICIAN_ROOT_DIR";
pub const LOCAL_FILE_WORKSPACE_PROVIDER_ID: &str = "local_file";
pub const SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID: &str = "silverbullet_space";
pub const DEFAULT_SILVERBULLET_RUNTIME_ROOT: &str = ".magician/runtime";
const MAX_WORKSPACE_JSON_VALIDATION_NODES: usize = 1_000_000;
// Canonical payload admission allows one million nodes. Durable event records
// wrap that payload with identity/sequence fields and bounded lineage arrays,
// so readers reserve explicit envelope headroom while still rejecting a
// corrupt legacy record before typed Serde allocation.
const MAX_JSONL_RECORD_NODES: usize = 1_050_000;

/// Revalidate that an opened ordinary workspace file is still the exact inode
/// published at its path. Unlike the lock-sentinel validator, ordinary data
/// files may legitimately be owner-readable/group-readable (for example
/// `0644`); they must still be owned by this process user, single-linked, and
/// never group/world writable.
pub(crate) fn validate_workspace_file_path_identity(
    file: &std::fs::File,
    path: &Path,
    authority: &str,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let opened = file.metadata()?;
        let published = std::fs::symlink_metadata(path)?;
        let euid = unsafe { libc::geteuid() };
        if !opened.is_file()
            || opened.uid() != euid
            || opened.mode() & 0o022 != 0
            || opened.nlink() != 1
            || published.file_type().is_symlink()
            || !published.is_file()
            || published.uid() != euid
            || published.mode() & 0o022 != 0
            || published.nlink() != 1
            || published.dev() != opened.dev()
            || published.ino() != opened.ino()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{authority} path identity validation failed"),
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path, authority);
    }
    Ok(())
}

/// Validate the exact inode/path relationship and strict mode of an already
/// private authoritative workspace file without mutating it.
pub(crate) fn validate_private_workspace_file_path_identity(
    file: &std::fs::File,
    path: &Path,
    authority: &str,
) -> std::io::Result<()> {
    validate_workspace_file_path_identity(file, path, authority)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let opened = file.metadata()?;
        let published = std::fs::symlink_metadata(path)?;
        if opened.mode() & 0o777 != 0o600 || published.mode() & 0o777 != 0o600 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{authority} private mode validation failed"),
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path, authority);
    }
    Ok(())
}

pub(crate) fn ensure_private_workspace_file_path_identity(
    file: &std::fs::File,
    path: &Path,
    authority: &str,
) -> std::io::Result<()> {
    // Tighten only after the general validator has rejected wrong ownership,
    // links, symlinks, and group/world-writable files. Descriptor chmod avoids
    // a pathname race; the second exact validation proves the published inode.
    validate_workspace_file_path_identity(file, path, authority)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        if file.metadata()?.mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            // Permission repair is part of private-file publication authority,
            // not a best-effort cosmetic change.
            file.sync_all()?;
        }
    }
    validate_private_workspace_file_path_identity(file, path, authority)
}

struct HashingJsonReader<R> {
    inner: R,
    hasher: blake3::Hasher,
    bytes: u64,
    expected_size: u64,
    admission: EncodedJsonStreamAdmission,
}

impl<R: std::io::Read> std::io::Read for HashingJsonReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.bytes >= self.expected_size {
            // Probe one byte beyond the admitted snapshot. Returning an error
            // keeps a concurrently grown suffix out of typed Serde entirely.
            let mut probe = [0_u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "workspace JSON source grew during verified decode",
                )),
            };
        }
        let remaining =
            usize::try_from(self.expected_size.saturating_sub(self.bytes)).unwrap_or(usize::MAX);
        let requested = buffer.len().min(remaining);
        let read = self.inner.read(&mut buffer[..requested])?;
        if !self.admission.feed(&buffer[..read]) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "workspace JSON source changed outside structural admission during decode",
            ));
        }
        self.hasher.update(&buffer[..read]);
        self.bytes = self.bytes.saturating_add(read as u64);
        Ok(read)
    }
}

fn buffered_hashing_json_reader<R: std::io::Read>(
    inner: R,
    expected_size: u64,
    max_depth: usize,
    max_nodes: usize,
) -> std::io::BufReader<HashingJsonReader<R>> {
    std::io::BufReader::with_capacity(
        64 * 1024,
        HashingJsonReader {
            inner,
            hasher: blake3::Hasher::new(),
            bytes: 0,
            expected_size,
            admission: EncodedJsonStreamAdmission::new(max_depth, max_nodes),
        },
    )
}

fn workspace_json_document_is_admitted(data: &[u8]) -> bool {
    json_bytes_nesting_is_bounded(data, MAX_RETAINED_JSON_DEPTH)
        && json_bytes_nodes_are_bounded(data, MAX_WORKSPACE_JSON_VALIDATION_NODES)
}

fn jsonl_record_is_admitted(data: &[u8]) -> bool {
    json_bytes_nesting_is_bounded(data, MAX_RETAINED_JSON_DEPTH)
        && json_bytes_nodes_are_bounded(data, MAX_JSONL_RECORD_NODES)
}

fn validate_workspace_json_reader<R: std::io::Read + std::io::Seek>(
    reader: R,
    max_depth: usize,
    max_nodes: usize,
) -> Result<(), ArtifactV2Error> {
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, reader);
    let mut admission = EncodedJsonStreamAdmission::new(max_depth, max_nodes);
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut reader, &mut chunk)?;
        if read == 0 {
            break;
        }
        if !admission.feed(&chunk[..read]) {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "workspace JSON exceeds the admitted {max_depth}-level/{max_nodes}-node structural ceiling"
            )));
        }
    }
    if !admission.finish() {
        return Err(ArtifactV2Error::InvalidRequest(
            "workspace JSON is structurally incomplete or exceeds its admission ceiling"
                .to_string(),
        ));
    }

    std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(0))?;
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    serde::de::IgnoredAny::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(())
}

/// Process-wide default seed root (the repo `magician_data_v3`), set ONCE at
/// startup from the boot path. The many bare `ArtifactV2Workspace::new(...)`
/// subsystems (Path B) don't attach an explicit `with_seed_root`; without this
/// they fell back to the runtime store root for bootstrap templates and
/// materialized them into `<runtime_root>/system/`. With it set, every workspace
/// resolves templates against the read-only repo seed instead — the same seam
/// `runtime_root_env` documents for keeping Path A and Path B aligned.
static DEFAULT_SEED_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static INSTALLED_RUNTIME_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Publish the resolved workspace root once at composition-root startup.
/// First writer wins. Magician-bin is the production caller so satellite
/// crates share that root instead of resolving `$HOME` / `MAGICIAN_ROOT_DIR`
/// a second time.
pub fn set_installed_runtime_root(root: PathBuf) {
    let _ = INSTALLED_RUNTIME_ROOT.set(pin_existing_runtime_root(root));
}

/// Process-wide runtime root installed by magician-bin, if any.
pub fn installed_runtime_root() -> Option<PathBuf> {
    INSTALLED_RUNTIME_ROOT.get().cloned()
}

/// Set the process-wide default seed root once at startup (first writer wins).
/// The boot path should only call this with a seed that actually contains
/// templates (`<seed>/system`), so a misconfigured path never silently redirects
/// template reads away from a working store.
pub fn set_default_seed_root(seed_root: PathBuf) {
    let _ = DEFAULT_SEED_ROOT.set(seed_root);
}

/// The process-wide default seed root, if configured at startup; `None` before
/// init (e.g. unit tests / pre-boot bare subsystems), where the legacy
/// store-root fallback applies.
pub fn default_seed_root() -> Option<PathBuf> {
    DEFAULT_SEED_ROOT.get().cloned()
}

/// The env-configured runtime data root, if any: `MAGICIAN_ROOT_DIR` (preferred)
/// or the legacy `MAGICIAN_STORAGE_PATH`. Returns `None` when neither is set, so
/// callers fall back to a context-appropriate default (the configured
/// `storage_path` on the boot path; `magician_data_v3` for bare subsystems).
/// This is the single seam that keeps the provider-resolved store (Path A) and
/// the bare-workspace subsystems (Path B) rooted at the same place.
pub fn runtime_root_env() -> Option<PathBuf> {
    [MAGICIAN_ROOT_DIR_ENV, MAGICIAN_STORAGE_PATH_ENV]
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

/// Resolve a runtime-root config file (`.env`, `operator-config.yaml`, …).
/// Prefers `<MAGICIAN_ROOT_DIR>/<file_name>` when it exists so a mounted
/// container root is self-contained; otherwise falls back to `legacy` (the
/// historical CWD-relative path) so dev checkouts keep loading their in-tree
/// copy.
pub fn runtime_config_path(file_name: &str, legacy: &str) -> PathBuf {
    // Effective runtime root = env override, else `$HOME/MagicianNotes` (temp
    // under the cargo test harness). Falls back to the legacy in-tree path when
    // the runtime root has no copy yet (dev checkouts).
    let candidate = default_storage_base_path().join(file_name);
    if candidate.exists() {
        return candidate;
    }
    PathBuf::from(legacy)
}

/// Local-profile runtime root. After magician-bin installs the resolved
/// workspace this returns that root so libraries cannot fork a second
/// `$HOME` / env backend. Before install, env then the fallback apply
/// (tests and pre-boot). New library env reads fail
/// `scripts/check_typed_storage_boundaries.py`.
pub fn default_storage_base_path() -> PathBuf {
    if let Some(root) = installed_runtime_root() {
        return root;
    }
    pin_existing_runtime_root(runtime_root_env().unwrap_or_else(default_storage_base_path_fallback))
}

/// Resolve an already-existing runtime root to its physical directory once at
/// the trusted configuration boundary. Security-sensitive stores can then
/// reject symlinks below that captured root without repeatedly following an
/// operator-provided root alias on every read or write.
///
/// A fresh root is intentionally returned unchanged: its owner still creates
/// and validates it through the normal workspace/store admission path.
fn pin_existing_runtime_root(configured_root: PathBuf) -> PathBuf {
    std::fs::canonicalize(&configured_root).unwrap_or(configured_root)
}

fn default_storage_base_path_fallback() -> PathBuf {
    if running_under_cargo_test_harness() {
        return std::env::temp_dir()
            .join(format!("magician-cargo-test-{}", std::process::id()))
            .join("magician_data_v3");
    }

    default_magician_notes_root()
}

/// Default runtime root for a real (non-test) run with no `MAGICIAN_ROOT_DIR` /
/// `MAGICIAN_STORAGE_PATH`: `$HOME/MagicianNotes` (the macOS location the
/// migration seeds, so the service "just works" without an env var). Falls back
/// to a CWD-relative `MagicianNotes` only if `HOME` is unset.
fn default_magician_notes_root() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join("MagicianNotes"),
        _ => PathBuf::from("MagicianNotes"),
    }
}

/// Whether this process is a cargo test binary (unit or integration). Crate
/// code that would otherwise touch the operator's runtime root from a test —
/// a first-use sweep, a scratch tree — decides on this, not on `cfg!(test)`
/// alone, which is false in the library an integration test links.
pub(crate) fn running_under_cargo_test_harness() -> bool {
    if cfg!(test) {
        return true;
    }

    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .map(|name| name == "deps")
        })
        .unwrap_or(false)
}

/// Which folder a given task lives in on disk. User-visible tasks
/// (created from the `/tasks` UI, or via `create_task` with no
/// override) live in `tasks/<id>/`. Internal tasks (chat-inline
/// delegate transients, etc.) live in `internal_tasks/<id>/`. The
/// `/tasks` listing surfaces only the former; the `/internal-tasks`
/// surface lists the latter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskLocation {
    UserVisible,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceFileEntry {
    pub relative_path: PathBuf,
    pub file_name: String,
    pub is_dir: bool,
    pub is_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDirectoryPage {
    pub entries: Vec<WorkspaceFileEntry>,
    /// Lexicographic exclusive cursor for the next page in this pass.
    pub next_after: Option<String>,
    /// True when this pass reached the end of the bounded directory snapshot.
    pub complete: bool,
    /// The directory exceeded its admitted physical scan ceiling. The bounded
    /// prefix page remains usable; entries beyond the ceiling are explicit
    /// capacity debt and are not silently treated as discovered authority.
    pub overflow: bool,
}

/// Result of a bounded JSONL tail read.
///
/// `bytes_read` is exposed for operational telemetry and regression coverage:
/// callers asking for a small recent window must not accidentally regress to
/// reading the complete lifetime log. A non-newline-terminated trailing record
/// is treated as an unacknowledged crash fragment and is not returned.
#[derive(Debug)]
pub struct JsonlTailRead<T> {
    pub records: Vec<T>,
    pub bytes_read: u64,
    pub file_len: u64,
}

/// One bounded, record-boundary-aligned page from the authoritative committed
/// prefix of a JSONL log. `next_offset` is an append-stable byte cursor: later
/// writes cannot move already committed records, unlike a moving tail window.
#[derive(Debug)]
pub struct CommittedJsonlForwardPage<T> {
    pub records: Vec<T>,
    pub next_offset: u64,
    pub committed_len: u64,
}

impl<T> CommittedJsonlForwardPage<T> {
    pub fn is_complete(&self) -> bool {
        self.next_offset == self.committed_len
    }
}

const JSONL_TAIL_WINDOW_ERROR_PREFIX: &str = "JSONL tail requires more than the bounded ";
const MAX_JSONL_COMMIT_AUTHORITY_BYTES: u64 = 32;

/// Whether a bounded tail read must prove it saw `limit` complete records.
///
/// `Required` is the canonical recovery contract: a window that cannot
/// contain the requested history is an error, never a silently short answer.
/// `BestEffort` serves recent-activity views that want a hard I/O bound more
/// than they want an exact count. Those callers still never get a silent
/// claim of completeness — `file_len > bytes_read` says the answer is a
/// bounded view of a longer history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailCompleteness {
    Required,
    BestEffort,
}

fn decode_jsonl_tail<T: DeserializeOwned>(
    mut bytes: Vec<u8>,
    file_len: u64,
    starts_at_record_boundary: bool,
    limit: usize,
    max_bytes: u64,
    completeness: TailCompleteness,
) -> Result<JsonlTailRead<T>, ArtifactV2Error> {
    let bytes_read = bytes.len() as u64;
    if bytes.is_empty() {
        return Ok(JsonlTailRead {
            records: Vec::new(),
            bytes_read,
            file_len,
        });
    }

    if bytes.last().copied() != Some(b'\n') {
        match bytes.iter().rposition(|byte| *byte == b'\n') {
            Some(last_newline) => bytes.truncate(last_newline.saturating_add(1)),
            None => bytes.clear(),
        }
    }

    let window_starts_mid_file = file_len > bytes_read;
    let start = if window_starts_mid_file && !starts_at_record_boundary {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index.saturating_add(1))
            .unwrap_or(bytes.len())
    } else {
        0
    };
    let complete = &bytes[start..];
    let mut lines = Vec::with_capacity(limit.min(1_024));
    for line in complete
        .rsplit(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
    {
        lines.push(line);
        if lines.len() == limit {
            break;
        }
    }
    if window_starts_mid_file && lines.len() < limit && completeness == TailCompleteness::Required {
        return Err(ArtifactV2Error::InvalidRequest(format!(
            "{JSONL_TAIL_WINDOW_ERROR_PREFIX}{max_bytes}-byte window for {limit} records"
        )));
    }
    lines.reverse();
    let records = lines
        .into_iter()
        .map(|line| {
            if !jsonl_record_is_admitted(line) {
                return Err(ArtifactV2Error::InvalidRequest(format!(
                    "JSONL record exceeds the admitted {MAX_RETAINED_JSON_DEPTH}-level/{MAX_JSONL_RECORD_NODES}-node ceiling or is structurally malformed"
                )));
            }
            serde_json::from_slice(line).map_err(ArtifactV2Error::from)
        })
        .collect::<Result<Vec<T>, ArtifactV2Error>>()?;
    Ok(JsonlTailRead {
        records,
        bytes_read,
        file_len,
    })
}

/// Local compatibility adapter for the workspace's file-backed tree.
///
/// This is not a remote-storage boundary. Typed capabilities in
/// `magician-storage` (`ObjectStore`, `DatasetStore`, and the rest) are.
/// Production implementations are `local_file` and `silverbullet_space` (a
/// shipped local profile variant). Track A does not remove the Settings
/// Runtime provider control. Copying a scope directory is not a supported
/// migration or remote-acceptance path.
#[async_trait]
pub trait WorkspaceFileProvider: Send + Sync + Debug {
    fn id(&self) -> &'static str;
    fn root(&self) -> &Path;
    fn visible_root(&self) -> &Path {
        self.root()
    }
    fn resolve_path(&self, relative_path: &Path) -> PathBuf;

    async fn create_dir_all(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
        fs::create_dir_all(self.resolve_path(relative_path)).await?;
        Ok(())
    }

    fn create_dir_all_sync(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
        std::fs::create_dir_all(self.resolve_path(relative_path))?;
        Ok(())
    }

    async fn read(&self, relative_path: &Path) -> Result<Vec<u8>, ArtifactV2Error> {
        Ok(fs::read(self.resolve_path(relative_path)).await?)
    }

    /// Read at most max_bytes from a workspace file, rejecting a file that
    /// grows beyond that limit while it is being read.
    async fn read_bounded(
        &self,
        relative_path: &Path,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        use tokio::io::AsyncReadExt;

        let file = fs::File::open(self.resolve_path(relative_path)).await?;
        let mut limited = file.take(max_bytes.saturating_add(1));
        let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024) as usize);
        limited.read_to_end(&mut bytes).await?;
        if bytes.len() as u64 > max_bytes {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "workspace file exceeds its {max_bytes}-byte read limit"
            )));
        }
        Ok(bytes)
    }

    fn read_sync(&self, relative_path: &Path) -> Result<Vec<u8>, ArtifactV2Error> {
        Ok(std::fs::read(self.resolve_path(relative_path))?)
    }

    fn read_prefix_sync(
        &self,
        relative_path: &Path,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        use std::io::Read;

        let file = std::fs::File::open(self.resolve_path(relative_path))?;
        let mut bytes = Vec::new();
        file.take(max_bytes).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    async fn read_prefix(
        &self,
        relative_path: &Path,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        use tokio::io::AsyncReadExt;

        let file = fs::File::open(self.resolve_path(relative_path)).await?;
        let mut limited = file.take(max_bytes);
        let mut bytes = Vec::new();
        limited.read_to_end(&mut bytes).await?;
        Ok(bytes)
    }

    /// Read at most `max_bytes` beginning at an exact byte offset.
    ///
    /// This is the bounded random-access primitive used by derived indexes.
    /// Callers still validate the independently committed logical length; the
    /// provider merely returns physical bytes and never promotes them to
    /// authority on its own.
    async fn read_range(
        &self,
        relative_path: &Path,
        offset: u64,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let mut file = fs::File::open(self.resolve_path(relative_path)).await?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut limited = file.take(max_bytes);
        let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024) as usize);
        limited.read_to_end(&mut bytes).await?;
        Ok(bytes)
    }

    async fn read_to_string(&self, relative_path: &Path) -> Result<String, ArtifactV2Error> {
        Ok(fs::read_to_string(self.resolve_path(relative_path)).await?)
    }

    async fn read_tail(
        &self,
        relative_path: &Path,
        max_bytes: u64,
    ) -> Result<(Vec<u8>, u64, bool), ArtifactV2Error> {
        let path = self.resolve_path(relative_path);
        let mut file = match fs::File::open(path).await {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), 0, true));
            },
            Err(err) => return Err(ArtifactV2Error::Io(err)),
        };
        let file_len = file.metadata().await?.len();
        let read_len = file_len.min(max_bytes);
        let start = file_len.saturating_sub(read_len);
        let starts_at_record_boundary = if start == 0 {
            true
        } else {
            file.seek(std::io::SeekFrom::Start(start - 1)).await?;
            let mut preceding = [0_u8; 1];
            file.read_exact(&mut preceding).await?;
            preceding[0] == b'\n'
        };
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let mut bytes = Vec::with_capacity(read_len.min(usize::MAX as u64) as usize);
        file.take(read_len).read_to_end(&mut bytes).await?;
        Ok((bytes, file_len, starts_at_record_boundary))
    }

    /// Read a tail ending at an explicitly committed logical length.
    ///
    /// A durable append may have placed bytes in the physical file before its
    /// commit authority was published. Canonical readers must not let those
    /// bytes become authoritative merely because they happen to end in a
    /// newline. The caller supplies the independently durable committed
    /// length; physical bytes beyond it remain invisible until recovery either
    /// commits or truncates them.
    async fn read_tail_through(
        &self,
        relative_path: &Path,
        committed_len: u64,
        max_bytes: u64,
    ) -> Result<(Vec<u8>, u64, bool), ArtifactV2Error> {
        let path = self.resolve_path(relative_path);
        let mut file = match fs::File::open(path).await {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && committed_len == 0 => {
                return Ok((Vec::new(), 0, true));
            },
            Err(err) => return Err(ArtifactV2Error::Io(err)),
        };
        let physical_len = file.metadata().await?.len();
        if physical_len < committed_len {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "JSONL commit authority ({committed_len} bytes) exceeds physical file length ({physical_len} bytes)"
            )));
        }
        let read_len = committed_len.min(max_bytes);
        let start = committed_len.saturating_sub(read_len);
        let starts_at_record_boundary = if start == 0 {
            true
        } else {
            file.seek(std::io::SeekFrom::Start(start - 1)).await?;
            let mut preceding = [0_u8; 1];
            file.read_exact(&mut preceding).await?;
            preceding[0] == b'\n'
        };
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let mut bytes = Vec::with_capacity(read_len.min(usize::MAX as u64) as usize);
        file.take(read_len).read_to_end(&mut bytes).await?;
        Ok((bytes, committed_len, starts_at_record_boundary))
    }

    /// Roll a file back to a caller-proven commit boundary and make the
    /// truncation durable before returning.
    async fn truncate_to_len(
        &self,
        relative_path: &Path,
        committed_len: u64,
    ) -> Result<(), ArtifactV2Error> {
        let path = self.resolve_path(relative_path);
        let file = fs::OpenOptions::new().write(true).open(path).await?;
        file.set_len(committed_len).await?;
        file.sync_all().await?;
        Ok(())
    }

    async fn sync_file_and_parent(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
        let path = self.resolve_path(relative_path);
        fs::File::open(&path).await?.sync_all().await?;
        sync_parent_dir(&path).await
    }

    /// Remove an unacknowledged JSONL suffix that does not end in a newline.
    ///
    /// Callers must hold the same cross-process lock used for appends. A
    /// newline is an acknowledgement boundary for the workspace JSONL
    /// contract, so bytes after the final newline can only be a cancelled or
    /// crashed append. The bounded reverse scan fails closed if the fragment
    /// is larger than the caller's admitted record ceiling.
    async fn truncate_unterminated_jsonl_tail(
        &self,
        relative_path: &Path,
        max_fragment_bytes: u64,
    ) -> Result<bool, ArtifactV2Error> {
        if max_fragment_bytes == 0 {
            return Err(ArtifactV2Error::InvalidRequest(
                "JSONL repair byte budget must be greater than zero".to_string(),
            ));
        }
        let path = self.resolve_path(relative_path);
        let mut file = match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .await
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(ArtifactV2Error::Io(err)),
        };
        let file_len = file.metadata().await?.len();
        if file_len == 0 {
            return Ok(false);
        }

        file.seek(std::io::SeekFrom::End(-1)).await?;
        let mut final_byte = [0_u8; 1];
        file.read_exact(&mut final_byte).await?;
        if final_byte[0] == b'\n' {
            return Ok(false);
        }

        let read_len = file_len.min(max_fragment_bytes);
        let start = file_len.saturating_sub(read_len);
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let mut suffix = Vec::with_capacity(read_len.min(usize::MAX as u64) as usize);
        (&mut file).take(read_len).read_to_end(&mut suffix).await?;
        let truncate_to = match suffix.iter().rposition(|byte| *byte == b'\n') {
            Some(last_newline) => start.saturating_add(last_newline as u64).saturating_add(1),
            None if start == 0 => 0,
            None => {
                return Err(ArtifactV2Error::InvalidRequest(format!(
                    "unterminated JSONL suffix exceeds the bounded {max_fragment_bytes}-byte repair ceiling"
                )));
            },
        };
        file.set_len(truncate_to).await?;
        file.sync_all().await?;
        Ok(true)
    }

    fn read_to_string_sync(&self, relative_path: &Path) -> Result<String, ArtifactV2Error> {
        Ok(std::fs::read_to_string(self.resolve_path(relative_path))?)
    }

    async fn write(&self, relative_path: &Path, bytes: &[u8]) -> Result<(), ArtifactV2Error> {
        let path = self.resolve_path(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(path, bytes).await?;
        Ok(())
    }

    fn write_sync(&self, relative_path: &Path, bytes: &[u8]) -> Result<(), ArtifactV2Error> {
        let path = self.resolve_path(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    async fn write_atomic(
        &self,
        relative_path: &Path,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        write_bytes_atomic(&self.resolve_path(relative_path), bytes).await
    }

    /// Stream a local source into this provider and publish it atomically.
    ///
    /// Artifact-producing tools commonly return paths to files much larger
    /// than their JSON envelopes. Keeping the copy at this boundary avoids a
    /// payload-sized `Vec<u8>` in every caller and prevents a failed overwrite
    /// from exposing a partial destination.
    async fn copy_external_atomic(
        &self,
        source: &Path,
        relative_path: &Path,
    ) -> Result<u64, ArtifactV2Error> {
        let destination = self.resolve_path(relative_path);
        let Some(parent) = destination.parent() else {
            return Err(ArtifactV2Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace copy destination has no parent",
            )));
        };
        fs::create_dir_all(parent).await?;
        let temporary = parent.join(format!(".artifact-copy-{}.tmp", Uuid::new_v4().simple()));

        let copied = async {
            let mut input = fs::File::open(source).await?;
            let mut output = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .await?;
            let copied = tokio::io::copy(&mut input, &mut output).await?;
            output.flush().await?;
            output.sync_all().await?;
            drop(output);
            let permit = magician_core::blocking_admission::acquire_blocking_admission()
                .await
                .map_err(|err| {
                    ArtifactV2Error::Runtime(format!("workspace copy parent-sync admission: {err}"))
                })?;
            fs::rename(&temporary, &destination).await?;
            sync_parent_dir_with_admission(&destination, permit).await?;
            Ok::<u64, ArtifactV2Error>(copied)
        }
        .await;

        match copied {
            Ok(copied) => Ok(copied),
            Err(error) => {
                let _ = fs::remove_file(&temporary).await;
                Err(error)
            },
        }
    }

    /// Stream-copy one file and publish it only when the bytes still match the
    /// accepted digest. This closes the validation-to-copy race for terminal
    /// deliverables without materializing the file in a `String` or `Vec`.
    async fn copy_external_verified_atomic(
        &self,
        source: &Path,
        relative_path: &Path,
        expected_sha256: &str,
    ) -> Result<u64, ArtifactV2Error> {
        if expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "workspace verified copy requires a 64-character SHA-256 digest".to_string(),
            ));
        }
        let destination = self.resolve_path(relative_path);
        let Some(parent) = destination.parent() else {
            return Err(ArtifactV2Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace verified-copy destination has no parent",
            )));
        };
        fs::create_dir_all(parent).await?;
        let temporary = parent.join(format!(
            ".artifact-verified-copy-{}.tmp",
            Uuid::new_v4().simple()
        ));

        let copied = async {
            let mut input = fs::File::open(source).await?;
            let mut output = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .await?;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0_u8; 64 * 1024];
            let mut copied = 0_u64;
            loop {
                let read = input.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
                output.write_all(&buffer[..read]).await?;
                copied = copied.saturating_add(read as u64);
            }
            output.flush().await?;
            output.sync_all().await?;
            let observed_sha256 = format!("{:x}", hasher.finalize());
            if !observed_sha256.eq_ignore_ascii_case(expected_sha256) {
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace verified-copy source changed after terminal acceptance".to_string(),
                ));
            }
            drop(output);
            let permit = magician_core::blocking_admission::acquire_blocking_admission()
                .await
                .map_err(|err| {
                    ArtifactV2Error::Runtime(format!(
                        "workspace verified-copy parent-sync admission: {err}"
                    ))
                })?;
            fs::rename(&temporary, &destination).await?;
            sync_parent_dir_with_admission(&destination, permit).await?;
            Ok::<u64, ArtifactV2Error>(copied)
        }
        .await;

        match copied {
            Ok(copied) => Ok(copied),
            Err(error) => {
                let _ = fs::remove_file(&temporary).await;
                Err(error)
            },
        }
    }

    /// Synchronous counterpart for blocking-worker compaction paths. The
    /// provider streams and re-hashes the caller-owned staging file into its
    /// own unique atomic staging sibling, then durably publishes only an exact
    /// digest match. Aggregate file size does not become a heap allocation.
    fn copy_external_verified_atomic_sync(
        &self,
        source: &Path,
        relative_path: &Path,
        expected_sha256: &str,
        expected_destination: Option<(u64, &str)>,
    ) -> Result<u64, ArtifactV2Error> {
        self.copy_external_verified_atomic_sync_with_privacy(
            source,
            relative_path,
            expected_sha256,
            expected_destination,
            false,
        )
    }

    /// Private counterpart for authoritative journals containing user request
    /// bodies. The provider-owned staging inode is the inode ultimately
    /// published at the destination, so it—not only the caller's source—is
    /// created and verified with mode 0600.
    fn copy_external_private_verified_atomic_sync(
        &self,
        source: &Path,
        relative_path: &Path,
        expected_sha256: &str,
        expected_destination: Option<(u64, &str)>,
    ) -> Result<u64, ArtifactV2Error> {
        self.copy_external_verified_atomic_sync_with_privacy(
            source,
            relative_path,
            expected_sha256,
            expected_destination,
            true,
        )
    }

    fn copy_external_verified_atomic_sync_with_privacy(
        &self,
        source: &Path,
        relative_path: &Path,
        expected_sha256: &str,
        expected_destination: Option<(u64, &str)>,
        private: bool,
    ) -> Result<u64, ArtifactV2Error> {
        use std::io::{Read as _, Write as _};

        if expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "workspace verified copy requires a 64-character SHA-256 digest".to_string(),
            ));
        }
        let destination = self.resolve_path(relative_path);
        let Some(parent) = destination.parent() else {
            return Err(ArtifactV2Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace verified-copy destination has no parent",
            )));
        };
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".artifact-verified-copy-{}.tmp",
            Uuid::new_v4().simple()
        ));
        let mut temporary_owned = false;
        let copied = (|| {
            let mut input = std::fs::File::open(source)?;
            let mut output_options = std::fs::OpenOptions::new();
            output_options.create_new(true).write(true);
            #[cfg(unix)]
            if private {
                use std::os::unix::fs::OpenOptionsExt as _;
                output_options.mode(0o600);
            }
            let mut output = output_options.open(&temporary)?;
            temporary_owned = true;
            if private {
                ensure_private_workspace_file_path_identity(
                    &output,
                    &temporary,
                    "private workspace verified-copy staging",
                )?;
            }
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            let mut copied = 0_u64;
            loop {
                let read = input.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
                output.write_all(&buffer[..read])?;
                copied = copied.saturating_add(read as u64);
            }
            output.flush()?;
            output.sync_all()?;
            let observed_sha256 = format!("{:x}", hasher.finalize());
            if !observed_sha256.eq_ignore_ascii_case(expected_sha256) {
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace verified-copy source changed after compaction".to_string(),
                ));
            }
            drop(output);
            if let Some((expected_len, expected_destination_sha256)) = expected_destination {
                if expected_destination_sha256.len() != 64
                    || !expected_destination_sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "workspace destination CAS requires a 64-character SHA-256 digest"
                            .to_string(),
                    ));
                }
                let mut current_options = std::fs::OpenOptions::new();
                current_options.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    current_options.custom_flags(libc::O_NOFOLLOW);
                }
                let mut current = current_options.open(&destination)?;
                if private {
                    ensure_private_workspace_file_path_identity(
                        &current,
                        &destination,
                        "private workspace verified destination CAS",
                    )?;
                }
                if current.metadata()?.len() != expected_len {
                    return Err(ArtifactV2Error::InvalidRequest(
                        WORKSPACE_DESTINATION_CAS_CONFLICT_DETAIL.to_string(),
                    ));
                }
                let mut current_hasher = Sha256::new();
                let mut current_len = 0_u64;
                loop {
                    let read = current.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    current_hasher.update(&buffer[..read]);
                    current_len = current_len.saturating_add(read as u64);
                }
                if private {
                    ensure_private_workspace_file_path_identity(
                        &current,
                        &destination,
                        "private workspace verified destination CAS",
                    )?;
                } else {
                    validate_workspace_file_path_identity(
                        &current,
                        &destination,
                        "workspace verified destination CAS",
                    )?;
                }
                if current_len != expected_len
                    || current.metadata()?.len() != expected_len
                    || !format!("{:x}", current_hasher.finalize())
                        .eq_ignore_ascii_case(expected_destination_sha256)
                {
                    return Err(ArtifactV2Error::InvalidRequest(
                        WORKSPACE_DESTINATION_CAS_CONFLICT_DETAIL.to_string(),
                    ));
                }
            }
            publish_staged_file_durably_sync(&temporary, &destination)?;
            if private {
                let published = std::fs::File::open(&destination)?;
                ensure_private_workspace_file_path_identity(
                    &published,
                    &destination,
                    "private workspace verified publication",
                )?;
            }
            Ok::<u64, ArtifactV2Error>(copied)
        })();
        match copied {
            Ok(copied) => Ok(copied),
            Err(error) => {
                if temporary_owned {
                    let _ = std::fs::remove_file(&temporary);
                }
                Err(error)
            },
        }
    }

    fn write_atomic_sync(&self, relative_path: &Path, bytes: &[u8]) -> Result<(), ArtifactV2Error> {
        write_bytes_atomic_sync(&self.resolve_path(relative_path), bytes)
    }

    async fn append(&self, relative_path: &Path, bytes: &[u8]) -> Result<(), ArtifactV2Error> {
        let path = self.resolve_path(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        use tokio::io::AsyncWriteExt;
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

    fn append_sync(&self, relative_path: &Path, bytes: &[u8]) -> Result<(), ArtifactV2Error> {
        use std::io::Write;

        let path = self.resolve_path(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }

    /// Specialized durable append for private authoritative journals. Unlike
    /// the compatibility append above, this refuses a symlink destination,
    /// revalidates the opened inode against its published path before and
    /// after the write, and syncs the parent when it creates the journal.
    fn append_private_durable_sync(
        &self,
        relative_path: &Path,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        use std::io::Write;

        let path = self.resolve_path(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let created = match std::fs::symlink_metadata(&path) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(error.into()),
        };
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&path)?;
        ensure_private_workspace_file_path_identity(
            &file,
            &path,
            "private workspace journal append",
        )?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        ensure_private_workspace_file_path_identity(
            &file,
            &path,
            "private workspace journal append",
        )?;
        if created {
            magician_core::durable_io::sync_parent_dir_blocking(&path)?;
        }
        Ok(())
    }

    async fn remove_file(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
        fs::remove_file(self.resolve_path(relative_path)).await?;
        Ok(())
    }

    fn remove_file_sync(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
        std::fs::remove_file(self.resolve_path(relative_path))?;
        Ok(())
    }

    async fn remove_dir_all(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
        fs::remove_dir_all(self.resolve_path(relative_path)).await?;
        Ok(())
    }

    fn remove_dir_all_sync(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
        std::fs::remove_dir_all(self.resolve_path(relative_path))?;
        Ok(())
    }

    fn rename_sync(&self, from: &Path, to: &Path) -> Result<(), ArtifactV2Error> {
        let resolved_to = self.resolve_path(to);
        if let Some(parent) = resolved_to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(self.resolve_path(from), resolved_to)?;
        Ok(())
    }

    async fn metadata(
        &self,
        relative_path: &Path,
    ) -> Result<Option<std::fs::Metadata>, ArtifactV2Error> {
        match fs::metadata(self.resolve_path(relative_path)).await {
            Ok(metadata) => Ok(Some(metadata)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ArtifactV2Error::Io(err)),
        }
    }

    async fn symlink_metadata(
        &self,
        relative_path: &Path,
    ) -> Result<Option<std::fs::Metadata>, ArtifactV2Error> {
        match fs::symlink_metadata(self.resolve_path(relative_path)).await {
            Ok(metadata) => Ok(Some(metadata)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ArtifactV2Error::Io(err)),
        }
    }

    fn metadata_sync(
        &self,
        relative_path: &Path,
    ) -> Result<Option<std::fs::Metadata>, ArtifactV2Error> {
        match std::fs::metadata(self.resolve_path(relative_path)) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ArtifactV2Error::Io(err)),
        }
    }

    async fn canonicalize(&self, relative_path: &Path) -> Result<PathBuf, ArtifactV2Error> {
        Ok(fs::canonicalize(self.resolve_path(relative_path)).await?)
    }

    async fn read_dir(
        &self,
        relative_path: &Path,
    ) -> Result<Vec<WorkspaceFileEntry>, ArtifactV2Error> {
        let mut entries = fs::read_dir(self.resolve_path(relative_path)).await?;
        let mut files = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            files.push(WorkspaceFileEntry {
                relative_path: relative_path.join(&file_name),
                file_name,
                is_dir: file_type.is_dir(),
                is_file: file_type.is_file(),
            });
        }
        files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        Ok(files)
    }

    /// Retain only the next `page_size` lexicographic entries after `after`.
    /// Memory stays proportional to the requested page and physical directory
    /// work stops after at most `scan_ceiling + 1` entries. A directory beyond
    /// that ceiling is reported as debt without suppressing its valid prefix.
    async fn read_dir_page_bounded(
        &self,
        relative_path: &Path,
        after: Option<&str>,
        page_size: usize,
        scan_ceiling: usize,
    ) -> Result<WorkspaceDirectoryPage, ArtifactV2Error> {
        if page_size == 0 || scan_ceiling == 0 || page_size > scan_ceiling {
            return Err(ArtifactV2Error::InvalidRequest(
                "workspace directory page requires 0 < page_size <= scan_ceiling".to_string(),
            ));
        }
        let mut entries = fs::read_dir(self.resolve_path(relative_path)).await?;
        let mut page = Vec::with_capacity(page_size.min(256));
        let mut examined = 0usize;
        let mut has_more = false;
        let mut overflow = false;
        while let Some(entry) = entries.next_entry().await? {
            if examined >= scan_ceiling {
                overflow = true;
                has_more = true;
                break;
            }
            examined = examined.saturating_add(1);
            let file_type = entry.file_type().await?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if after.is_some_and(|cursor| file_name.as_str() <= cursor) {
                continue;
            }
            let candidate = WorkspaceFileEntry {
                relative_path: relative_path.join(&file_name),
                file_name,
                is_dir: file_type.is_dir(),
                is_file: file_type.is_file(),
            };
            let insertion = page
                .binary_search_by(|current: &WorkspaceFileEntry| {
                    current.file_name.cmp(&candidate.file_name)
                })
                .unwrap_or_else(|index| index);
            if insertion < page_size {
                page.insert(insertion, candidate);
                if page.len() > page_size {
                    page.pop();
                    has_more = true;
                }
            } else {
                has_more = true;
            }
        }
        let next_after = has_more
            .then(|| page.last().map(|entry| entry.file_name.clone()))
            .flatten();
        Ok(WorkspaceDirectoryPage {
            entries: page,
            next_after,
            complete: !has_more,
            overflow,
        })
    }

    fn read_dir_sync(
        &self,
        relative_path: &Path,
    ) -> Result<Vec<WorkspaceFileEntry>, ArtifactV2Error> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(self.resolve_path(relative_path))? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            files.push(WorkspaceFileEntry {
                relative_path: relative_path.join(&file_name),
                file_name,
                is_dir: file_type.is_dir(),
                is_file: file_type.is_file(),
            });
        }
        files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        Ok(files)
    }
}

#[derive(Debug, Clone)]
pub struct LocalFileWorkspaceProvider {
    root: PathBuf,
}

impl LocalFileWorkspaceProvider {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl WorkspaceFileProvider for LocalFileWorkspaceProvider {
    fn id(&self) -> &'static str {
        LOCAL_FILE_WORKSPACE_PROVIDER_ID
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn resolve_path(&self, relative_path: &Path) -> PathBuf {
        self.root.join(relative_path)
    }
}

#[derive(Debug, Clone)]
pub struct SilverBulletSpaceWorkspaceProvider {
    space_root: PathBuf,
}

impl SilverBulletSpaceWorkspaceProvider {
    pub fn new<P: AsRef<Path>>(space_root: P) -> Result<Self, ArtifactV2Error> {
        Self::with_runtime_root(space_root, DEFAULT_SILVERBULLET_RUNTIME_ROOT)
    }

    pub fn with_runtime_root<P: AsRef<Path>, Q: AsRef<Path>>(
        space_root: P,
        runtime_root: Q,
    ) -> Result<Self, ArtifactV2Error> {
        // The configured `runtime_root` is still validated (settings back-compat
        // + existing callers/tests), but it no longer re-roots the store. The SB
        // provider now roots directly at `space_root`, behaving identically to
        // `LocalFileWorkspaceProvider` — the `.magician/runtime` redirection is
        // gone. SB search/notes parity comes from SilverBullet serving this same
        // root, not from a hidden sub-tree.
        normalize_silverbullet_runtime_root(runtime_root.as_ref())?;
        Ok(Self {
            space_root: space_root.as_ref().to_path_buf(),
        })
    }
}

impl WorkspaceFileProvider for SilverBulletSpaceWorkspaceProvider {
    fn id(&self) -> &'static str {
        SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID
    }

    fn root(&self) -> &Path {
        &self.space_root
    }

    // `visible_root` intentionally uses the trait default (== `root()`); the SB
    // provider no longer distinguishes a separate visible space from the store.

    fn resolve_path(&self, relative_path: &Path) -> PathBuf {
        self.space_root.join(relative_path)
    }
}

fn normalize_silverbullet_runtime_root(runtime_root: &Path) -> Result<PathBuf, ArtifactV2Error> {
    let runtime_root = if runtime_root.as_os_str().is_empty() {
        Path::new(DEFAULT_SILVERBULLET_RUNTIME_ROOT)
    } else {
        runtime_root
    };
    if runtime_root.is_absolute() {
        return Err(ArtifactV2Error::InvalidRequest(format!(
            "SilverBullet runtime root '{}' must be relative to the Space",
            runtime_root.display()
        )));
    }

    let mut normalized = PathBuf::new();
    for component in runtime_root.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ArtifactV2Error::InvalidRequest(format!(
                    "SilverBullet runtime root '{}' must stay inside the Space",
                    runtime_root.display()
                )));
            },
        }
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(DEFAULT_SILVERBULLET_RUNTIME_ROOT);
    }
    Ok(normalized)
}

fn canonicalize_with_missing_tail(path: &Path) -> Option<PathBuf> {
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        if let Ok(mut canonical) = std::fs::canonicalize(cursor) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return Some(canonical);
        }
        missing.push(cursor.file_name()?.to_os_string());
        cursor = cursor.parent()?;
    }
}

fn cached_provider_root_candidates(provider_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![provider_root.to_path_buf()];
    if let Some(canonical_root) = canonicalize_with_missing_tail(provider_root) {
        if canonical_root != provider_root && !roots.contains(&canonical_root) {
            roots.push(canonical_root);
        }
    }
    roots
}

/// What [`ArtifactV2Workspace::read_jsonl_path_tolerant`] recovered, and what it
/// could not.
///
/// The two damage counts are separate because they are not the same event. A
/// torn tail is expected after an unclean shutdown and costs nothing; a corrupt
/// record is committed data that is now gone. Collapsing them into one number
/// would make the ordinary case and the reportable one indistinguishable, which
/// is the failure mode a tolerant reader is most likely to introduce.
#[derive(Debug)]
pub struct TolerantJsonlRead<T> {
    /// Every record that parsed, in file order.
    pub records: Vec<T>,
    /// Committed lines that would not parse, or that exceeded the retained-JSON
    /// ceiling. Real loss — the caller is expected to surface this.
    pub corrupt: usize,
    /// Bytes after the last newline. Never acknowledged by a durable append, so
    /// dropping them is not a fault.
    pub torn_tail_bytes: usize,
}

/// Hand-written rather than derived: `#[derive(Default)]` on a generic struct
/// bounds the impl on `T: Default`, which an empty `Vec<T>` does not need and
/// which every caller would then have to satisfy for no reason.
impl<T> Default for TolerantJsonlRead<T> {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            corrupt: 0,
            torn_tail_bytes: 0,
        }
    }
}

impl<T> TolerantJsonlRead<T> {
    /// True when committed records were lost. Not true for a torn tail alone —
    /// callers log on this, so folding the tail in here would make every
    /// unclean shutdown look like data loss.
    pub fn lost_committed_records(&self) -> bool {
        self.corrupt > 0
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactV2Workspace {
    file_provider: Arc<dyn WorkspaceFileProvider>,
    provider_resolved_root: PathBuf,
    provider_root_candidates: Arc<[PathBuf]>,
    /// Optional read-only seed root (the repo `magician_data_v3`) supplying the
    /// bootstrap TEMPLATES (agent_templates / db_templates / trust_policy_templates)
    /// independent of the runtime store root. `None` ⇒ templates resolve against the
    /// store's own `system/` (dev/local_file, where seed and runtime coincide).
    seed_root: Option<PathBuf>,
}

impl ArtifactV2Workspace {
    pub fn new<P: AsRef<Path>>(base_root: P) -> Self {
        Self::with_local_file_provider(base_root)
    }

    pub fn with_local_file_provider<P: AsRef<Path>>(base_root: P) -> Self {
        Self::with_file_provider(Arc::new(LocalFileWorkspaceProvider::new(base_root)))
    }

    pub fn with_silverbullet_space_provider<P: AsRef<Path>>(
        space_root: P,
    ) -> Result<Self, ArtifactV2Error> {
        Self::with_silverbullet_space_provider_and_runtime_root(
            space_root,
            DEFAULT_SILVERBULLET_RUNTIME_ROOT,
        )
    }

    pub fn with_silverbullet_space_provider_and_runtime_root<P: AsRef<Path>, Q: AsRef<Path>>(
        space_root: P,
        runtime_root: Q,
    ) -> Result<Self, ArtifactV2Error> {
        Ok(Self::with_file_provider(Arc::new(
            SilverBulletSpaceWorkspaceProvider::with_runtime_root(space_root, runtime_root)?,
        )))
    }

    pub fn with_file_provider(file_provider: Arc<dyn WorkspaceFileProvider>) -> Self {
        let provider_resolved_root = file_provider.resolve_path(Path::new(""));
        let provider_root_candidates =
            cached_provider_root_candidates(&provider_resolved_root).into();
        Self {
            file_provider,
            provider_resolved_root,
            provider_root_candidates,
            seed_root: None,
        }
    }

    /// Attach a read-only seed root (the repo `magician_data_v3`) supplying the
    /// bootstrap templates. When set and distinct from the store root, template
    /// reads (agent_templates / db_templates / trust_policy_templates) resolve
    /// here while all runtime state stays under the store root.
    pub fn with_seed_root<P: AsRef<Path>>(mut self, seed_root: P) -> Self {
        self.seed_root = Some(seed_root.as_ref().to_path_buf());
        self
    }

    pub fn file_provider_id(&self) -> &'static str {
        self.file_provider.id()
    }

    pub fn base_root(&self) -> &Path {
        self.file_provider.root()
    }

    pub fn visible_root(&self) -> &Path {
        self.file_provider.visible_root()
    }

    fn provider_path<P: AsRef<Path>>(&self, relative_path: P) -> PathBuf {
        self.file_provider.resolve_path(relative_path.as_ref())
    }

    fn provider_resolved_root(&self) -> &Path {
        &self.provider_resolved_root
    }

    fn provider_relative_path_for_resolved_path(
        &self,
        resolved_path: &Path,
    ) -> Result<PathBuf, ArtifactV2Error> {
        let provider_root = self.provider_resolved_root();
        let mut relative_path = None;
        for root in self.provider_root_candidates.iter() {
            if let Ok(stripped) = resolved_path.strip_prefix(&root) {
                relative_path = Some(stripped.to_path_buf());
                break;
            }
        }
        let Some(relative_path) = relative_path else {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "path '{}' is outside workspace file provider root '{}'",
                resolved_path.display(),
                provider_root.display()
            )));
        };
        if relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "path '{}' resolves outside workspace file provider root '{}'",
                resolved_path.display(),
                provider_root.display()
            )));
        }
        Ok(relative_path)
    }

    pub async fn create_dir_all_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<(), ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.create_dir_all(&relative_path).await
    }

    pub fn create_dir_all_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<(), ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.create_dir_all_sync(&relative_path)
    }

    pub async fn read_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        let path = resolved_path.as_ref();
        if let Some(bytes) = crate::magician_v2::typed_io::read(self, path).await? {
            return Ok(bytes);
        }
        self.read_path_raw(path).await
    }

    pub async fn read_path_raw<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.read(&relative_path).await
    }

    pub async fn read_bounded_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider
            .read_bounded(&relative_path, max_bytes)
            .await
    }

    pub fn read_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        let path = resolved_path.as_ref();
        if crate::magician_v2::typed_io::is_classified(self, path) {
            return std::fs::read(path).map_err(ArtifactV2Error::from);
        }
        self.read_path_sync_raw(path)
    }

    fn read_path_sync_raw<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.read_sync(&relative_path)
    }

    pub fn read_prefix_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider
            .read_prefix_sync(&relative_path, max_bytes)
    }

    pub async fn read_prefix_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider
            .read_prefix(&relative_path, max_bytes)
            .await
    }

    pub async fn read_range_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        offset: u64,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider
            .read_range(&relative_path, offset, max_bytes)
            .await
    }

    pub async fn read_to_string_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<String, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.read_to_string(&relative_path).await
    }

    pub async fn read_to_string_bounded_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        max_bytes: u64,
    ) -> Result<String, ArtifactV2Error> {
        let bytes = self.read_bounded_path(resolved_path, max_bytes).await?;
        String::from_utf8(bytes).map_err(|error| {
            ArtifactV2Error::InvalidRequest(format!(
                "workspace bounded text file is not valid UTF-8: {error}"
            ))
        })
    }

    pub fn read_to_string_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<String, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.read_to_string_sync(&relative_path)
    }

    pub async fn write_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        let path = resolved_path.as_ref();
        if crate::magician_v2::typed_io::persist(self, path, bytes).await? {
            return Ok(());
        }
        self.write_path_raw(path, bytes).await
    }

    pub async fn write_path_raw<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.write(&relative_path, bytes).await
    }

    pub fn write_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        let path = resolved_path.as_ref();
        if crate::magician_v2::typed_io::persist_sync(self, path, bytes)? {
            return Ok(());
        }
        let relative_path = self.provider_relative_path_for_resolved_path(path)?;
        self.file_provider.write_sync(&relative_path, bytes)
    }

    pub async fn write_atomic_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        let path = resolved_path.as_ref();
        if crate::magician_v2::typed_io::persist(self, path, bytes).await? {
            return Ok(());
        }
        let relative_path = self.provider_relative_path_for_resolved_path(path)?;
        self.file_provider.write_atomic(&relative_path, bytes).await
    }

    /// Serialize an owned JSON document into an atomic staging file without a
    /// payload-sized intermediate byte vector. The document is moved into the
    /// blocking worker so neither serialization nor its eventual drop runs on
    /// an async runtime worker. The writer aborts and removes its staging file
    /// as soon as the encoded byte ceiling is crossed.
    pub async fn write_json_value_atomic_stream_path<T, P>(
        &self,
        resolved_path: P,
        value: T,
        max_bytes: usize,
    ) -> Result<(), ArtifactV2Error>
    where
        T: Serialize + Send + 'static,
        P: AsRef<Path>,
    {
        self.write_json_value_atomic_stream_path_with_cleanup(
            resolved_path,
            value,
            max_bytes,
            |_| {},
        )
        .await
    }

    /// Streaming typed-JSON writer with an explicit blocking-worker cleanup
    /// hook. Typed envelopes containing admitted `Value` fields use this to
    /// drain those fields iteratively after either success or failure, before
    /// the envelope's ordinary destructor runs.
    pub async fn write_json_value_atomic_stream_path_with_cleanup<T, P, F>(
        &self,
        resolved_path: P,
        mut value: T,
        max_bytes: usize,
        cleanup: F,
    ) -> Result<(), ArtifactV2Error>
    where
        T: Serialize + Send + 'static,
        P: AsRef<Path>,
        F: FnOnce(&mut T) + Send + 'static,
    {
        if let Err(error) = crate::magician_v2::typed_io::reject_if_lease_lost() {
            cleanup(&mut value);
            return Err(error);
        }
        let mut cleanup = Some(cleanup);
        let relative_path =
            match self.provider_relative_path_for_resolved_path(resolved_path.as_ref()) {
                Ok(relative_path) => relative_path,
                Err(error) => {
                    cleanup.take().expect("cleanup is present")(&mut value);
                    return Err(error);
                },
            };
        let destination = self.file_provider.resolve_path(&relative_path);
        magician_core::blocking_admission::spawn_blocking_admitted(move || {
            let written = write_json_atomic_stream_sync(&destination, &value, max_bytes);
            cleanup.take().expect("cleanup is present")(&mut value);
            written
        })
        .await
        .map_err(|error| {
            ArtifactV2Error::Runtime(format!(
                "workspace streaming JSON writer failed to join: {error}"
            ))
        })??;
        self.republish_classified_file(resolved_path.as_ref()).await
    }

    async fn republish_classified_file(&self, path: &Path) -> Result<(), ArtifactV2Error> {
        if !crate::magician_v2::typed_io::is_classified(self, path) {
            return Ok(());
        }
        let bytes = std::fs::read(path)?;
        crate::magician_v2::typed_io::persist(self, path, &bytes)
            .await
            .map(|_| ())
    }

    /// Pretty-wire counterpart to
    /// [`Self::write_json_value_atomic_stream_path_with_cleanup`]. The value is
    /// owned by a blocking worker, so large typed documents neither allocate a
    /// second complete encoded buffer nor run their cleanup on a runtime
    /// worker.
    pub async fn write_json_pretty_atomic_stream_path_with_cleanup<T, P, F>(
        &self,
        resolved_path: P,
        mut value: T,
        max_bytes: usize,
        cleanup: F,
    ) -> Result<(), ArtifactV2Error>
    where
        T: Serialize + Send + 'static,
        P: AsRef<Path>,
        F: FnOnce(&mut T) + Send + 'static,
    {
        if let Err(error) = crate::magician_v2::typed_io::reject_if_lease_lost() {
            cleanup(&mut value);
            return Err(error);
        }
        let mut cleanup = Some(cleanup);
        let relative_path =
            match self.provider_relative_path_for_resolved_path(resolved_path.as_ref()) {
                Ok(relative_path) => relative_path,
                Err(error) => {
                    cleanup.take().expect("cleanup is present")(&mut value);
                    return Err(error);
                },
            };
        let destination = self.file_provider.resolve_path(&relative_path);
        magician_core::blocking_admission::spawn_blocking_admitted(move || {
            let written = write_json_pretty_atomic_stream_sync(&destination, &value, max_bytes);
            cleanup.take().expect("cleanup is present")(&mut value);
            written
        })
        .await
        .map_err(|error| {
            ArtifactV2Error::Runtime(format!(
                "workspace pretty streaming JSON writer failed to join: {error}"
            ))
        })??;
        self.republish_classified_file(resolved_path.as_ref()).await
    }

    /// Stack-safe canonical `Value` specialization of the streaming writer.
    pub async fn write_canonical_json_value_atomic_stream_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        mut value: serde_json::Value,
    ) -> Result<(), ArtifactV2Error> {
        if let Err(error) = crate::magician_v2::typed_io::reject_if_lease_lost() {
            discard_json_iteratively(std::mem::replace(&mut value, serde_json::Value::Null));
            return Err(error);
        }
        let relative_path = match self
            .provider_relative_path_for_resolved_path(resolved_path.as_ref())
        {
            Ok(relative_path) => relative_path,
            Err(error) => {
                discard_json_iteratively(std::mem::replace(&mut value, serde_json::Value::Null));
                return Err(error);
            },
        };
        let destination = self.file_provider.resolve_path(&relative_path);
        magician_core::blocking_admission::spawn_blocking_admitted(move || {
            let mut value = value;
            let written = write_canonical_json_value_atomic_stream_sync(&destination, &value);
            discard_json_iteratively(std::mem::replace(&mut value, serde_json::Value::Null));
            written
        })
        .await
        .map_err(|error| {
            ArtifactV2Error::Runtime(format!(
                "workspace canonical JSON writer failed to join: {error}"
            ))
        })??;
        self.republish_classified_file(resolved_path.as_ref()).await
    }

    pub async fn copy_external_atomic_path<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        source: P,
        resolved_destination: Q,
    ) -> Result<u64, ArtifactV2Error> {
        crate::magician_v2::typed_io::reject_if_lease_lost()?;
        if crate::magician_v2::typed_io::is_classified(self, resolved_destination.as_ref()) {
            let bytes = tokio::fs::read(source.as_ref()).await?;
            crate::magician_v2::typed_io::persist(self, resolved_destination.as_ref(), &bytes)
                .await?;
            return Ok(bytes.len() as u64);
        }
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_destination.as_ref())?;
        self.file_provider
            .copy_external_atomic(source.as_ref(), &relative_path)
            .await
    }

    /// Copy an already-authorized file from this workspace without retaining
    /// its bytes in the caller. Canonicalizing and re-validating the source
    /// prevents symlinks or lexical traversal from turning the internal
    /// file-backed output variant into an arbitrary host-file copier.
    pub async fn copy_workspace_file_verified_atomic_path<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        source: P,
        resolved_destination: Q,
        expected_sha256: &str,
    ) -> Result<u64, ArtifactV2Error> {
        let canonical_source = self.canonicalize_path(source.as_ref()).await?;
        self.provider_relative_path_for_resolved_path(&canonical_source)?;
        let metadata = self
            .symlink_metadata_path(&canonical_source)
            .await?
            .ok_or_else(|| {
                ArtifactV2Error::InvalidRequest(
                    "workspace verified-copy source does not exist".to_string(),
                )
            })?;
        if !metadata.file_type().is_file() {
            return Err(ArtifactV2Error::InvalidRequest(
                "workspace verified-copy source must be a regular file".to_string(),
            ));
        }
        crate::magician_v2::typed_io::reject_if_lease_lost()?;
        if crate::magician_v2::typed_io::is_classified(self, resolved_destination.as_ref()) {
            let bytes = tokio::fs::read(&canonical_source).await?;
            crate::magician_v2::typed_io::persist(self, resolved_destination.as_ref(), &bytes)
                .await?;
            return Ok(bytes.len() as u64);
        }
        let relative_destination =
            self.provider_relative_path_for_resolved_path(resolved_destination.as_ref())?;
        self.file_provider
            .copy_external_verified_atomic(
                &canonical_source,
                &relative_destination,
                expected_sha256,
            )
            .await
    }

    /// Verified streaming publication with compare-and-swap authority over the
    /// current destination. Callers hold their destination's cross-process
    /// publication lock; the provider performs a bounded second-pass
    /// length/digest/identity check immediately before the durable rename.
    pub(crate) fn copy_workspace_file_verified_atomic_path_sync_if_destination_matches<
        P: AsRef<Path>,
        Q: AsRef<Path>,
    >(
        &self,
        source: P,
        resolved_destination: Q,
        expected_source_sha256: &str,
        expected_destination_len: u64,
        expected_destination_sha256: &str,
    ) -> Result<u64, ArtifactV2Error> {
        self.copy_workspace_file_verified_atomic_path_sync_if_destination_matches_with_privacy(
            source,
            resolved_destination,
            expected_source_sha256,
            expected_destination_len,
            expected_destination_sha256,
            false,
        )
    }

    /// Private variant for authoritative journals. Unlike the general copy,
    /// the provider-owned inode published at the destination is guaranteed to
    /// be mode 0600, and an older readable destination is durably tightened
    /// during the same CAS-protected publication window.
    pub(crate) fn copy_private_workspace_file_verified_atomic_path_sync_if_destination_matches<
        P: AsRef<Path>,
        Q: AsRef<Path>,
    >(
        &self,
        source: P,
        resolved_destination: Q,
        expected_source_sha256: &str,
        expected_destination_len: u64,
        expected_destination_sha256: &str,
    ) -> Result<u64, ArtifactV2Error> {
        self.copy_workspace_file_verified_atomic_path_sync_if_destination_matches_with_privacy(
            source,
            resolved_destination,
            expected_source_sha256,
            expected_destination_len,
            expected_destination_sha256,
            true,
        )
    }

    fn copy_workspace_file_verified_atomic_path_sync_if_destination_matches_with_privacy<
        P: AsRef<Path>,
        Q: AsRef<Path>,
    >(
        &self,
        source: P,
        resolved_destination: Q,
        expected_source_sha256: &str,
        expected_destination_len: u64,
        expected_destination_sha256: &str,
        private: bool,
    ) -> Result<u64, ArtifactV2Error> {
        let canonical_source = std::fs::canonicalize(source.as_ref())?;
        self.provider_relative_path_for_resolved_path(&canonical_source)?;
        if !std::fs::symlink_metadata(&canonical_source)?
            .file_type()
            .is_file()
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "workspace verified-copy source must be a regular file".to_string(),
            ));
        }
        crate::magician_v2::typed_io::reject_if_lease_lost()?;
        if crate::magician_v2::typed_io::is_classified(self, resolved_destination.as_ref())
            && !crate::magician_v2::typed_io::specialized_jsonl_commit_authority(
                resolved_destination.as_ref(),
            )
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "streaming verified copy cannot bypass a classified destination owner".to_string(),
            ));
        }
        let relative_destination =
            self.provider_relative_path_for_resolved_path(resolved_destination.as_ref())?;
        let expected_destination = Some((expected_destination_len, expected_destination_sha256));
        if private {
            self.file_provider
                .copy_external_private_verified_atomic_sync(
                    &canonical_source,
                    &relative_destination,
                    expected_source_sha256,
                    expected_destination,
                )
        } else {
            self.file_provider.copy_external_verified_atomic_sync(
                &canonical_source,
                &relative_destination,
                expected_source_sha256,
                expected_destination,
            )
        }
    }

    /// Validate a workspace-owned JSON document without materializing its
    /// complete body. File-backed terminal projections must retain the same
    /// media contract as `OutputBody::Json`; digest authority alone proves
    /// byte identity, not that those bytes are syntactically valid JSON.
    pub async fn validate_workspace_json_file_path<P: AsRef<Path>>(
        &self,
        source: P,
    ) -> Result<(), ArtifactV2Error> {
        let canonical_source = self.canonicalize_path(source.as_ref()).await?;
        self.provider_relative_path_for_resolved_path(&canonical_source)?;
        let metadata = self
            .symlink_metadata_path(&canonical_source)
            .await?
            .ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("workspace JSON source does not exist".to_string())
            })?;
        if !metadata.file_type().is_file() {
            return Err(ArtifactV2Error::InvalidRequest(
                "workspace JSON source must be a regular file".to_string(),
            ));
        }

        magician_core::blocking_admission::spawn_blocking_admitted(move || {
            let file = std::fs::File::open(canonical_source)?;
            validate_workspace_json_reader(
                file,
                MAX_RETAINED_JSON_DEPTH,
                MAX_WORKSPACE_JSON_VALIDATION_NODES,
            )
        })
        .await
        .map_err(|error| {
            ArtifactV2Error::Runtime(format!(
                "workspace JSON validation worker failed to join: {error}"
            ))
        })?
    }

    /// Verify and decode a bounded JSON value without retaining its encoded
    /// body beside the parsed tree. The admission/hash pass and Serde pass use
    /// the same open file handle, closing both heap amplification and a
    /// validation-to-reopen race.
    pub async fn read_verified_json_value_path<P: AsRef<Path>>(
        &self,
        source: P,
        expected_size: u64,
        expected_blake3: &str,
        max_bytes: u64,
        max_depth: usize,
        max_nodes: usize,
    ) -> Result<serde_json::Value, ArtifactV2Error> {
        let canonical_source = self.canonicalize_path(source.as_ref()).await?;
        self.provider_relative_path_for_resolved_path(&canonical_source)?;
        let expected_blake3 = expected_blake3.to_string();
        magician_core::blocking_admission::spawn_blocking_admitted(move || {
            use std::io::{Read, Seek};

            if expected_size > max_bytes {
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace JSON source exceeds its byte limit".to_string(),
                ));
            }
            let mut file = std::fs::File::open(canonical_source)?;
            if file.metadata()?.len() != expected_size {
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace JSON source size does not match its descriptor".to_string(),
                ));
            }
            let mut admission = EncodedJsonStreamAdmission::new(max_depth, max_nodes);
            let mut hasher = blake3::Hasher::new();
            let mut buffer = vec![0_u8; 64 * 1024];
            let mut observed_size = 0u64;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                observed_size = observed_size.saturating_add(read as u64);
                if observed_size > expected_size
                    || observed_size > max_bytes
                    || !admission.feed(&buffer[..read])
                {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "workspace JSON source failed encoded admission".to_string(),
                    ));
                }
                hasher.update(&buffer[..read]);
            }
            if observed_size != expected_size || !admission.finish() {
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace JSON source failed encoded admission".to_string(),
                ));
            }
            let observed_blake3 = format!("blake3:{}", hasher.finalize().to_hex());
            if observed_blake3 != expected_blake3 {
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace JSON source hash does not match its descriptor".to_string(),
                ));
            }

            file.seek(std::io::SeekFrom::Start(0))?;
            // `serde_json::from_reader` may request very small slices. Keep the
            // integrity/admission reader behind a buffer so the second pass
            // hashes and scans chunks instead of turning a large index into
            // byte-at-a-time file I/O.
            let mut buffered_reader =
                buffered_hashing_json_reader(file, expected_size, max_depth, max_nodes);
            let mut deserializer = serde_json::Deserializer::from_reader(&mut buffered_reader);
            let mut value = serde_json::Value::deserialize(&mut deserializer)?;
            if let Err(error) = deserializer.end() {
                drop(deserializer);
                discard_json_iteratively(std::mem::replace(&mut value, serde_json::Value::Null));
                return Err(ArtifactV2Error::Serde(error));
            }
            drop(deserializer);
            let verified_reader = buffered_reader.into_inner();
            let parsed_blake3 = format!("blake3:{}", verified_reader.hasher.finalize().to_hex());
            if verified_reader.bytes != expected_size
                || !verified_reader.admission.finish()
                || parsed_blake3 != expected_blake3
            {
                discard_json_iteratively(std::mem::replace(&mut value, serde_json::Value::Null));
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace JSON source changed during verified decode".to_string(),
                ));
            }
            Ok(value)
        })
        .await
        .map_err(|error| {
            ArtifactV2Error::Runtime(format!(
                "workspace verified JSON reader failed to join: {error}"
            ))
        })?
    }

    /// Stream-admit and deserialize a typed workspace JSON document without a
    /// complete encoded `Vec`. This is intended for bounded indexes whose
    /// authoritative file has no independent content digest.
    pub async fn read_json_bounded_stream_path<T, P>(
        &self,
        source: P,
        max_bytes: u64,
        max_depth: usize,
        max_nodes: usize,
    ) -> Result<T, ArtifactV2Error>
    where
        T: DeserializeOwned + Send + 'static,
        P: AsRef<Path>,
    {
        self.read_json_bounded_stream_path_with_cleanup_on_error(
            source,
            max_bytes,
            max_depth,
            max_nodes,
            |_| {},
        )
        .await
    }

    /// Typed streaming reader with a type-aware cleanup hook for failures that
    /// occur after Serde has constructed the value (trailing data or a changed
    /// second-pass byte stream). Containers with `Value` fields can therefore
    /// drain them iteratively before returning the error.
    pub async fn read_json_bounded_stream_path_with_cleanup_on_error<T, P, F>(
        &self,
        source: P,
        max_bytes: u64,
        max_depth: usize,
        max_nodes: usize,
        cleanup: F,
    ) -> Result<T, ArtifactV2Error>
    where
        T: DeserializeOwned + Send + 'static,
        P: AsRef<Path>,
        F: Fn(&mut T) + Send + 'static,
    {
        let canonical_source = self.canonicalize_path(source.as_ref()).await?;
        self.provider_relative_path_for_resolved_path(&canonical_source)?;
        magician_core::blocking_admission::spawn_blocking_admitted(move || {
            use std::io::{Read, Seek};

            let mut file = std::fs::File::open(canonical_source)?;
            let expected_size = file.metadata()?.len();
            if expected_size > max_bytes {
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace JSON document exceeds its byte limit".to_string(),
                ));
            }
            let mut admission = EncodedJsonStreamAdmission::new(max_depth, max_nodes);
            let mut admitted_hasher = blake3::Hasher::new();
            let mut buffer = vec![0_u8; 64 * 1024];
            let mut observed_size = 0u64;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                observed_size = observed_size.saturating_add(read as u64);
                if observed_size > max_bytes || !admission.feed(&buffer[..read]) {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "workspace JSON document failed encoded admission".to_string(),
                    ));
                }
                admitted_hasher.update(&buffer[..read]);
            }
            if observed_size != expected_size || !admission.finish() {
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace JSON document failed encoded admission".to_string(),
                ));
            }
            let admitted_hash = admitted_hasher.finalize();
            file.seek(std::io::SeekFrom::Start(0))?;
            let mut buffered_reader =
                buffered_hashing_json_reader(file, expected_size, max_depth, max_nodes);
            let mut deserializer = serde_json::Deserializer::from_reader(&mut buffered_reader);
            let mut value = T::deserialize(&mut deserializer)?;
            if let Err(error) = deserializer.end() {
                drop(deserializer);
                cleanup(&mut value);
                return Err(ArtifactV2Error::Serde(error));
            }
            drop(deserializer);
            let verified_reader = buffered_reader.into_inner();
            if verified_reader.bytes != expected_size
                || !verified_reader.admission.finish()
                || verified_reader.hasher.finalize() != admitted_hash
            {
                cleanup(&mut value);
                return Err(ArtifactV2Error::InvalidRequest(
                    "workspace JSON document changed during bounded decode".to_string(),
                ));
            }
            Ok(value)
        })
        .await
        .map_err(|error| {
            ArtifactV2Error::Runtime(format!(
                "workspace bounded JSON reader failed to join: {error}"
            ))
        })?
    }

    pub fn write_atomic_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        if crate::magician_v2::typed_io::persist_sync(self, resolved_path.as_ref(), bytes)? {
            return Ok(());
        }
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.write_atomic_sync(&relative_path, bytes)
    }

    pub async fn append_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        crate::magician_v2::typed_io::reject_if_lease_lost()?;
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.append(&relative_path, bytes).await
    }

    pub async fn truncate_unterminated_jsonl_tail_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        max_fragment_bytes: u64,
    ) -> Result<bool, ArtifactV2Error> {
        crate::magician_v2::typed_io::reject_if_lease_lost()?;
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider
            .truncate_unterminated_jsonl_tail(&relative_path, max_fragment_bytes)
            .await
    }

    pub async fn truncate_path_to_len<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        committed_len: u64,
    ) -> Result<(), ArtifactV2Error> {
        crate::magician_v2::typed_io::reject_if_lease_lost()?;
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider
            .truncate_to_len(&relative_path, committed_len)
            .await
    }

    pub async fn sync_path_and_parent<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<(), ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider
            .sync_file_and_parent(&relative_path)
            .await
    }

    pub fn append_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        crate::magician_v2::typed_io::reject_if_lease_lost()?;
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.append_sync(&relative_path, bytes)
    }

    pub(crate) fn append_private_durable_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        bytes: &[u8],
    ) -> Result<(), ArtifactV2Error> {
        crate::magician_v2::typed_io::reject_if_lease_lost()?;
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider
            .append_private_durable_sync(&relative_path, bytes)
    }

    pub async fn read_json_path<T: DeserializeOwned, P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<T, ArtifactV2Error> {
        let body = self.read_to_string_path(resolved_path).await?;
        if !workspace_json_document_is_admitted(body.as_bytes()) {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "workspace JSON exceeds the admitted {MAX_RETAINED_JSON_DEPTH}-level/{MAX_WORKSPACE_JSON_VALIDATION_NODES}-node ceiling or is structurally malformed"
            )));
        }
        Ok(serde_json::from_str(&body)?)
    }

    pub fn read_json_path_sync<T: DeserializeOwned, P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<T, ArtifactV2Error> {
        let body = self.read_to_string_path_sync(resolved_path)?;
        if !workspace_json_document_is_admitted(body.as_bytes()) {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "workspace JSON exceeds the admitted {MAX_RETAINED_JSON_DEPTH}-level/{MAX_WORKSPACE_JSON_VALIDATION_NODES}-node ceiling or is structurally malformed"
            )));
        }
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn read_jsonl_path<T: DeserializeOwned, P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Vec<T>, ArtifactV2Error> {
        match self.read_to_string_path(resolved_path).await {
            Ok(body) => body
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    // This API intentionally reads complete history, but each
                    // individual record still crosses the same retained-JSON
                    // depth boundary as bounded tail reads. Check the encoded
                    // bytes before Serde builds its recursive representation;
                    // otherwise one corrupt/deep historical record can
                    // overflow an ordinary execution-worker stack before the
                    // parser has a chance to return an error.
                    if !jsonl_record_is_admitted(line.as_bytes()) {
                        return Err(ArtifactV2Error::InvalidRequest(format!(
                            "JSONL record exceeds the admitted {MAX_RETAINED_JSON_DEPTH}-level/{MAX_JSONL_RECORD_NODES}-node ceiling or is structurally malformed"
                        )));
                    }
                    serde_json::from_str(line).map_err(ArtifactV2Error::from)
                })
                .collect(),
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            },
            Err(err) => Err(err),
        }
    }

    /// Read a JSONL file, keeping the records that parse instead of failing the
    /// whole file when one does not.
    ///
    /// [`read_jsonl_path`](Self::read_jsonl_path) collects into a `Result`, so
    /// the first bad line loses every good one — one damaged record costs the
    /// listing, the index, or the history it belonged to, permanently, until
    /// someone deletes the file by hand. For a *derived* or *append-only*
    /// record set that is the wrong trade.
    ///
    /// **It is not the wrong trade everywhere, which is why this is opt-in.**
    /// Skipping a line understates whatever the file counts. For a spend ledger
    /// that means handing out budget already consumed, so those readers keep the
    /// strict form deliberately.
    ///
    /// Two kinds of damage, reported separately because they mean different
    /// things:
    ///
    /// - **A torn tail** — a final line with no terminating newline. A durable
    ///   append writes the newline and syncs before acknowledging, so those
    ///   bytes were never committed. Dropping them loses nothing and is not a
    ///   fault.
    /// - **A corrupt record** — a line inside the terminated prefix that will
    ///   not parse, or one over the retained-JSON ceiling. Those bytes *were*
    ///   committed, so this is real loss and the caller is expected to say so.
    ///   Returning the count rather than logging here is deliberate: this layer
    ///   does not know whether losing a row is a shrug or an incident.
    pub async fn read_jsonl_path_tolerant<T: DeserializeOwned, P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<TolerantJsonlRead<T>, ArtifactV2Error> {
        let body = match self.read_to_string_path(resolved_path).await {
            Ok(body) => body,
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TolerantJsonlRead::default())
            },
            Err(err) => return Err(err),
        };

        // A trailing fragment is only a fragment if the file does not end on a
        // record boundary. An empty file ends on one vacuously.
        let (committed, torn_tail_bytes) = match body.rfind('\n') {
            Some(last_newline) if last_newline + 1 < body.len() => {
                (&body[..=last_newline], body.len() - last_newline - 1)
            },
            Some(_) => (body.as_str(), 0),
            None if body.trim().is_empty() => (body.as_str(), 0),
            None => ("", body.len()),
        };

        let mut records = Vec::new();
        let mut corrupt = 0_usize;
        for line in committed.lines().filter(|line| !line.trim().is_empty()) {
            // Checked before Serde builds its recursive representation, for the
            // same reason the strict reader checks it: one deep record can
            // overflow a worker stack before the parser can return an error.
            if !jsonl_record_is_admitted(line.as_bytes()) {
                corrupt += 1;
                continue;
            }
            match serde_json::from_str(line) {
                Ok(record) => records.push(record),
                Err(_) => corrupt += 1,
            }
        }

        Ok(TolerantJsonlRead {
            records,
            corrupt,
            torn_tail_bytes,
        })
    }

    pub async fn read_committed_jsonl_path<T: DeserializeOwned, P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        resolved_path: P,
        commit_authority_path: Q,
    ) -> Result<Vec<T>, ArtifactV2Error> {
        let committed_len = self
            .committed_jsonl_len_path(&resolved_path, commit_authority_path)
            .await?;
        if committed_len == 0 {
            return Ok(Vec::new());
        }
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        let bytes = self
            .file_provider
            .read_prefix(&relative_path, committed_len)
            .await?;
        if bytes.len() as u64 != committed_len {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "JSONL commit authority ({committed_len} bytes) exceeds readable physical prefix ({} bytes)",
                bytes.len()
            )));
        }
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
            .map(|line| {
                if !jsonl_record_is_admitted(line) {
                    return Err(ArtifactV2Error::InvalidRequest(format!(
                        "JSONL record exceeds the admitted {MAX_RETAINED_JSON_DEPTH}-level/{MAX_JSONL_RECORD_NODES}-node ceiling or is structurally malformed"
                    )));
                }
                serde_json::from_slice(line).map_err(ArtifactV2Error::from)
            })
            .collect()
    }

    /// Read forward from an exact committed JSONL record boundary without
    /// materializing the lifetime log. The returned byte cursor remains valid
    /// as new records append. A page is bounded independently by record and
    /// byte ceilings; a single record that cannot fit fails closed rather than
    /// being skipped or causing a no-progress retry loop.
    pub async fn read_committed_jsonl_forward_page_path<
        T: DeserializeOwned,
        P: AsRef<Path>,
        Q: AsRef<Path>,
    >(
        &self,
        resolved_path: P,
        commit_authority_path: Q,
        offset: u64,
        max_records: usize,
        max_bytes: u64,
    ) -> Result<CommittedJsonlForwardPage<T>, ArtifactV2Error> {
        if max_records == 0 || max_bytes == 0 {
            return Err(ArtifactV2Error::InvalidRequest(
                "committed JSONL forward page requires positive record and byte ceilings"
                    .to_string(),
            ));
        }
        let committed_len = self
            .committed_jsonl_len_path(&resolved_path, commit_authority_path)
            .await?;
        if offset > committed_len {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "committed JSONL forward cursor ({offset}) exceeds authority ({committed_len})"
            )));
        }
        if offset == committed_len {
            return Ok(CommittedJsonlForwardPage {
                records: Vec::new(),
                next_offset: offset,
                committed_len,
            });
        }
        if offset > 0 {
            let preceding = self
                .read_range_path(&resolved_path, offset.saturating_sub(1), 1)
                .await?;
            if preceding.as_slice() != b"\n" {
                return Err(ArtifactV2Error::InvalidRequest(format!(
                    "committed JSONL forward cursor ({offset}) is not a record boundary"
                )));
            }
        }

        let remaining = committed_len.saturating_sub(offset);
        let read_len = remaining.min(max_bytes.saturating_add(1));
        let bytes = self
            .read_range_path(&resolved_path, offset, read_len)
            .await?;
        if bytes.len() as u64 != read_len {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "committed JSONL forward page expected {read_len} bytes but read {}",
                bytes.len()
            )));
        }

        let mut records = Vec::with_capacity(max_records.min(256));
        let mut consumed = 0usize;
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if records.len() >= max_records
                || line.last() != Some(&b'\n')
                || consumed.saturating_add(line.len()) as u64 > max_bytes
            {
                break;
            }
            consumed = consumed.saturating_add(line.len());
            let record = &line[..line.len().saturating_sub(1)];
            if record.iter().all(u8::is_ascii_whitespace) {
                return Err(ArtifactV2Error::InvalidRequest(format!(
                    "committed JSONL forward page contains a blank record at offset {}",
                    offset.saturating_add(consumed.saturating_sub(line.len()) as u64)
                )));
            }
            if !jsonl_record_is_admitted(record) {
                return Err(ArtifactV2Error::InvalidRequest(format!(
                    "JSONL record exceeds the admitted {MAX_RETAINED_JSON_DEPTH}-level/{MAX_JSONL_RECORD_NODES}-node ceiling or is structurally malformed"
                )));
            }
            records.push(serde_json::from_slice(record)?);
        }
        if consumed == 0 {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "committed JSONL record at offset {offset} exceeds the {max_bytes}-byte forward-page ceiling or is unterminated"
            )));
        }
        let next_offset = offset.saturating_add(consumed as u64);
        Ok(CommittedJsonlForwardPage {
            records,
            next_offset,
            committed_len,
        })
    }

    /// Read at most `max_bytes` from the end of a JSONL file and decode its
    /// last `limit` complete records in canonical file order.
    ///
    /// The first line is discarded only when the byte window begins
    /// mid-record; the provider also inspects the preceding byte so an exact
    /// record-boundary seek does not discard valid history.
    /// Likewise, an unterminated final line is ignored because durable JSONL
    /// appends acknowledge only after writing the newline and syncing the file.
    /// If the bounded window cannot contain the requested number of complete
    /// records, the method fails closed instead of silently returning an
    /// incomplete recent-history view.
    pub async fn read_jsonl_tail_path<T: DeserializeOwned, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        limit: usize,
        max_bytes: u64,
    ) -> Result<JsonlTailRead<T>, ArtifactV2Error> {
        self.read_jsonl_tail_with_completeness(
            resolved_path,
            limit,
            max_bytes,
            TailCompleteness::Required,
        )
        .await
    }

    async fn read_jsonl_tail_with_completeness<T: DeserializeOwned, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        limit: usize,
        max_bytes: u64,
        completeness: TailCompleteness,
    ) -> Result<JsonlTailRead<T>, ArtifactV2Error> {
        if limit == 0 {
            return Ok(JsonlTailRead {
                records: Vec::new(),
                bytes_read: 0,
                file_len: 0,
            });
        }
        if max_bytes == 0 {
            return Err(ArtifactV2Error::InvalidRequest(
                "JSONL tail byte budget must be greater than zero".to_string(),
            ));
        }
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        let (bytes, file_len, starts_at_record_boundary) = self
            .file_provider
            .read_tail(&relative_path, max_bytes)
            .await?;
        decode_jsonl_tail(
            bytes,
            file_len,
            starts_at_record_boundary,
            limit,
            max_bytes,
            completeness,
        )
    }

    /// Bounded recent-history tail that degrades instead of failing closed.
    ///
    /// Escalates the read window exactly like `read_jsonl_tail_adaptive_path`,
    /// so a shard whose records are unusually fat still yields `limit`
    /// records. What differs is the end of the escalation: when even
    /// `hard_max_bytes` cannot prove `limit` complete records are present —
    /// the case where a single record is larger than the whole ceiling — this
    /// returns the records that ceiling-sized window did hold rather than an
    /// error. A recent-activity reader must not lose its entire view because
    /// one record in the shard is enormous.
    ///
    /// The result still never claims to be complete. `file_len > bytes_read`
    /// means the caller is looking at a bounded view of a longer history, and
    /// a caller that needs the exact `limit` must use
    /// `read_jsonl_tail_adaptive_path`, which fails closed instead.
    ///
    /// Total bytes read is bounded by the geometric escalation:
    /// `initial·(1+2+…) + hard ≤ 3·hard_max_bytes`.
    pub async fn read_jsonl_tail_recent_path<T: DeserializeOwned, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        limit: usize,
        initial_max_bytes: u64,
        hard_max_bytes: u64,
    ) -> Result<JsonlTailRead<T>, ArtifactV2Error> {
        if initial_max_bytes == 0 || hard_max_bytes < initial_max_bytes {
            return Err(ArtifactV2Error::InvalidRequest(
                "recent JSONL tail requires 0 < initial_max_bytes <= hard_max_bytes".to_string(),
            ));
        }
        let resolved_path = resolved_path.as_ref();
        match self
            .read_jsonl_tail_adaptive_path::<T, _>(
                resolved_path,
                limit,
                initial_max_bytes,
                hard_max_bytes,
            )
            .await
        {
            Ok(tail) => Ok(tail),
            Err(ArtifactV2Error::InvalidRequest(message))
                if message.starts_with(JSONL_TAIL_WINDOW_ERROR_PREFIX) =>
            {
                self.read_jsonl_tail_with_completeness(
                    resolved_path,
                    limit,
                    hard_max_bytes,
                    TailCompleteness::BestEffort,
                )
                .await
            },
            Err(error) => Err(error),
        }
    }

    pub async fn committed_jsonl_len_path<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        resolved_path: P,
        commit_authority_path: Q,
    ) -> Result<u64, ArtifactV2Error> {
        let commit_authority_path = commit_authority_path.as_ref();
        let authority_metadata = self.symlink_metadata_path(commit_authority_path).await?;
        let committed_len = match authority_metadata {
            None => {
                // Backward compatibility for logs created before commit
                // authority existed. Writers publish the legacy length before
                // their first new append, so a missing marker never coexists
                // with bytes from the new uncertain-append protocol.
                self.symlink_metadata_path(resolved_path.as_ref())
                    .await?
                    .map_or(0, |metadata| metadata.len())
            },
            Some(metadata) => {
                if metadata.len() > MAX_JSONL_COMMIT_AUTHORITY_BYTES {
                    return Err(ArtifactV2Error::InvalidRequest(format!(
                        "JSONL commit authority exceeds the admitted {MAX_JSONL_COMMIT_AUTHORITY_BYTES}-byte ceiling"
                    )));
                }
                let bytes = self
                    .read_prefix_path(commit_authority_path, MAX_JSONL_COMMIT_AUTHORITY_BYTES + 1)
                    .await?;
                if bytes.len() as u64 > MAX_JSONL_COMMIT_AUTHORITY_BYTES {
                    return Err(ArtifactV2Error::InvalidRequest(format!(
                        "JSONL commit authority exceeds the admitted {MAX_JSONL_COMMIT_AUTHORITY_BYTES}-byte ceiling"
                    )));
                }
                let value = std::str::from_utf8(&bytes).map_err(|error| {
                    ArtifactV2Error::InvalidRequest(format!(
                        "invalid UTF-8 JSONL commit authority: {error}"
                    ))
                })?;
                value.trim().parse::<u64>().map_err(|error| {
                    ArtifactV2Error::InvalidRequest(format!(
                        "invalid JSONL commit authority: {error}"
                    ))
                })?
            },
        };
        let physical_len = self
            .symlink_metadata_path(resolved_path.as_ref())
            .await?
            .map_or(0, |metadata| metadata.len());
        if physical_len < committed_len {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "JSONL commit authority ({committed_len} bytes) exceeds physical file length ({physical_len} bytes)"
            )));
        }
        Ok(committed_len)
    }

    /// Decode only the prefix authorized by a canonical JSONL commit marker.
    /// Physical newline-terminated bytes beyond that boundary are an
    /// uncertain append and are intentionally invisible to readers.
    pub async fn read_committed_jsonl_tail_path<
        T: DeserializeOwned,
        P: AsRef<Path>,
        Q: AsRef<Path>,
    >(
        &self,
        resolved_path: P,
        commit_authority_path: Q,
        limit: usize,
        max_bytes: u64,
    ) -> Result<JsonlTailRead<T>, ArtifactV2Error> {
        if limit == 0 {
            return Ok(JsonlTailRead {
                records: Vec::new(),
                bytes_read: 0,
                file_len: 0,
            });
        }
        if max_bytes == 0 {
            return Err(ArtifactV2Error::InvalidRequest(
                "JSONL tail byte budget must be greater than zero".to_string(),
            ));
        }
        let committed_len = self
            .committed_jsonl_len_path(&resolved_path, commit_authority_path)
            .await?;
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        let (bytes, logical_len, starts_at_record_boundary) = self
            .file_provider
            .read_tail_through(&relative_path, committed_len, max_bytes)
            .await?;
        decode_jsonl_tail(
            bytes,
            logical_len,
            starts_at_record_boundary,
            limit,
            max_bytes,
            TailCompleteness::Required,
        )
    }

    /// Retry a bounded tail with geometrically larger windows when the first
    /// window starts inside a complete record or cannot prove the requested
    /// history is present. `hard_max_bytes` is the total read ceiling, not a
    /// per-record promise. Callers recovering after a possible crash fragment
    /// must budget `max_fragment_bytes + wanted_history_bytes`; the method
    /// fails closed when `limit` complete records cannot fit in that ceiling.
    pub async fn read_jsonl_tail_adaptive_path<T: DeserializeOwned, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        limit: usize,
        initial_max_bytes: u64,
        hard_max_bytes: u64,
    ) -> Result<JsonlTailRead<T>, ArtifactV2Error> {
        if initial_max_bytes == 0 || hard_max_bytes < initial_max_bytes {
            return Err(ArtifactV2Error::InvalidRequest(
                "adaptive JSONL tail requires 0 < initial_max_bytes <= hard_max_bytes".to_string(),
            ));
        }
        let resolved_path = resolved_path.as_ref();
        let mut max_bytes = initial_max_bytes;
        loop {
            match self
                .read_jsonl_tail_path::<T, _>(resolved_path, limit, max_bytes)
                .await
            {
                Ok(tail) => return Ok(tail),
                Err(ArtifactV2Error::InvalidRequest(message))
                    if message.starts_with(JSONL_TAIL_WINDOW_ERROR_PREFIX)
                        && max_bytes < hard_max_bytes =>
                {
                    max_bytes = max_bytes.saturating_mul(2).min(hard_max_bytes);
                },
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn read_committed_jsonl_tail_adaptive_path<
        T: DeserializeOwned,
        P: AsRef<Path>,
        Q: AsRef<Path>,
    >(
        &self,
        resolved_path: P,
        commit_authority_path: Q,
        limit: usize,
        initial_max_bytes: u64,
        hard_max_bytes: u64,
    ) -> Result<JsonlTailRead<T>, ArtifactV2Error> {
        if initial_max_bytes == 0 || hard_max_bytes < initial_max_bytes {
            return Err(ArtifactV2Error::InvalidRequest(
                "adaptive JSONL tail requires 0 < initial_max_bytes <= hard_max_bytes".to_string(),
            ));
        }
        let resolved_path = resolved_path.as_ref();
        let commit_authority_path = commit_authority_path.as_ref();
        let mut max_bytes = initial_max_bytes;
        loop {
            match self
                .read_committed_jsonl_tail_path::<T, _, _>(
                    resolved_path,
                    commit_authority_path,
                    limit,
                    max_bytes,
                )
                .await
            {
                Ok(tail) => return Ok(tail),
                Err(ArtifactV2Error::InvalidRequest(message))
                    if message.starts_with(JSONL_TAIL_WINDOW_ERROR_PREFIX)
                        && max_bytes < hard_max_bytes =>
                {
                    max_bytes = max_bytes.saturating_mul(2).min(hard_max_bytes);
                },
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn write_json_atomic_path<T: Serialize, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        value: &T,
    ) -> Result<(), ArtifactV2Error> {
        let bytes = serde_json::to_vec_pretty(value)?;
        self.write_atomic_path(resolved_path, &bytes).await
    }

    /// Atomically write JSON without indentation.
    ///
    /// For rebuildable machine-read indexes that are never opened by a human
    /// and are rewritten on a timer: the indentation is bytes serialized,
    /// written and fsynced on every flush that nothing ever reads. Parsing is
    /// format-agnostic, so a store can move between this and
    /// `write_json_atomic_path` without a migration.
    pub async fn write_json_compact_atomic_path<T: Serialize, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        value: &T,
    ) -> Result<(), ArtifactV2Error> {
        let bytes = serde_json::to_vec(value)?;
        self.write_atomic_path(resolved_path, &bytes).await
    }

    pub fn write_json_atomic_path_sync<T: Serialize, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        value: &T,
    ) -> Result<(), ArtifactV2Error> {
        let bytes = serde_json::to_vec_pretty(value)?;
        self.write_atomic_path_sync(resolved_path, &bytes)
    }

    pub async fn write_string_atomic_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        value: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.write_atomic_path(resolved_path, value.as_bytes())
            .await
    }

    pub async fn append_jsonl_path<T: Serialize, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        value: &T,
    ) -> Result<(), ArtifactV2Error> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.append_path(resolved_path, &line).await
    }

    pub async fn write_jsonl_records_atomic_path<T: Serialize, P: AsRef<Path>>(
        &self,
        resolved_path: P,
        records: &[T],
    ) -> Result<(), ArtifactV2Error> {
        let mut body = Vec::new();
        for record in records {
            body.extend(serde_json::to_vec(record)?);
            body.push(b'\n');
        }
        self.write_atomic_path(resolved_path, &body).await
    }

    /// Replay a persisted write journal, and report **which paths it landed**.
    ///
    /// The paths, not a "did I replay anything" bool, because a replay finishes
    /// a write somebody else started: the only sanctioned caller
    /// (`TaskWriteReconciler::recover_task_writes`) has to decide from the
    /// write set whether the record it just completed is one the list index
    /// and Today's cache project off, which is the same decision the commit
    /// path makes and which a bool cannot answer.
    ///
    /// Empty when there was no journal — the overwhelmingly common case, and
    /// the one that must cost nothing.
    ///
    /// **Self-healing on a partial replay.** The journal is removed only after
    /// every write lands, so an error part-way through leaves it on disk and
    /// the next recovery replays it in full. That is why returning `Err` here
    /// without the paths is safe: the write is not lost, it is merely not
    /// finished yet.
    ///
    /// **Module-visible on purpose** — see
    /// [`Self::commit_multi_write_journal_path`].
    pub(in crate::magician_v2::artifact_v2) async fn recover_multi_write_journal_path<
        P: AsRef<Path>,
    >(
        &self,
        journal_path: P,
    ) -> Result<Vec<PathBuf>, ArtifactV2Error> {
        let journal_path = journal_path.as_ref();
        let metadata = match self.metadata_path(journal_path).await? {
            Some(metadata) => metadata,
            None => {
                return Ok(Vec::new());
            },
        };
        if metadata.len() > MAX_MULTI_WRITE_JOURNAL_BYTES {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "multi-write journal exceeds {MAX_MULTI_WRITE_JOURNAL_BYTES} bytes"
            )));
        }
        let journal_bytes = self
            .read_prefix_path(
                journal_path,
                MAX_MULTI_WRITE_JOURNAL_BYTES.saturating_add(1),
            )
            .await?;
        if journal_bytes.len() as u64 != metadata.len() {
            return Err(ArtifactV2Error::InvalidRequest(
                "multi-write journal changed while it was being read".to_string(),
            ));
        }
        if !json_bytes_nesting_is_bounded(&journal_bytes, MAX_RETAINED_JSON_DEPTH) {
            return Err(ArtifactV2Error::InvalidRequest(
                "multi-write journal has invalid or excessive JSON nesting".to_string(),
            ));
        }
        let journal = serde_json::from_slice::<WorkspacePersistedWriteJournal>(&journal_bytes)?;

        let (replayed, staging_dir) = match journal {
            WorkspacePersistedWriteJournal::V1(journal) => {
                if journal.version != 1 || journal.writes.len() > MAX_MULTI_WRITE_OPS {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "invalid legacy multi-write journal".to_string(),
                    ));
                }
                let mut replayed = Vec::with_capacity(journal.writes.len());
                let mut total_bytes = 0u64;
                // Validate the complete legacy set before replaying any target.
                // The prior one-pass compatibility path could land an early
                // write and then discover an invalid later destination.
                for write in &journal.writes {
                    self.provider_relative_path_for_resolved_path(&write.path)?;
                    total_bytes = total_bytes.saturating_add(write.bytes.len() as u64);
                    if total_bytes > MAX_MULTI_WRITE_TOTAL_BYTES {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "legacy multi-write journal exceeds byte limit".to_string(),
                        ));
                    }
                }
                for write in &journal.writes {
                    self.write_atomic_path(&write.path, &write.bytes).await?;
                    replayed.push(write.path.clone());
                }
                (replayed, None)
            },
            WorkspacePersistedWriteJournal::V2(journal) => {
                if journal.version != 2 || journal.writes.len() > MAX_MULTI_WRITE_OPS {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "invalid multi-write journal".to_string(),
                    ));
                }
                let expected_staging_dir = multi_write_staging_dir(journal_path);
                let expected_staging_relative =
                    self.provider_relative_path_for_resolved_path(&expected_staging_dir)?;
                let actual_staging_relative =
                    self.provider_relative_path_for_resolved_path(&journal.staging_dir)?;
                if actual_staging_relative != expected_staging_relative {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "multi-write journal staging directory mismatch".to_string(),
                    ));
                }
                let mut replayed = Vec::with_capacity(journal.writes.len());
                let mut total_bytes = 0u64;
                // Validate the complete staged set before changing any target.
                // This is a two-pass disk read by design: retaining all payloads
                // would recreate the heap amplification this format removes.
                for (index, write) in journal.writes.iter().enumerate() {
                    self.provider_relative_path_for_resolved_path(&write.path)?;
                    let expected_staged_path =
                        expected_staging_dir.join(format!("{index:04}.payload"));
                    let expected_staged_relative =
                        self.provider_relative_path_for_resolved_path(&expected_staged_path)?;
                    let actual_staged_relative =
                        self.provider_relative_path_for_resolved_path(&write.staged_path)?;
                    if actual_staged_relative != expected_staged_relative {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "multi-write journal staged payload path mismatch".to_string(),
                        ));
                    }
                    total_bytes = total_bytes.saturating_add(write.size_bytes);
                    if write.size_bytes > MAX_MULTI_WRITE_TOTAL_BYTES
                        || total_bytes > MAX_MULTI_WRITE_TOTAL_BYTES
                    {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "multi-write journal exceeds byte limit".to_string(),
                        ));
                    }
                    let metadata =
                        self.metadata_path(&write.staged_path)
                            .await?
                            .ok_or_else(|| {
                                ArtifactV2Error::InvalidRequest(
                                    "multi-write journal staged payload is missing".to_string(),
                                )
                            })?;
                    if metadata.len() != write.size_bytes {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "multi-write journal staged payload size mismatch".to_string(),
                        ));
                    }
                    let bytes = self
                        .read_prefix_path(&write.staged_path, write.size_bytes.saturating_add(1))
                        .await?;
                    if bytes.len() as u64 != write.size_bytes
                        || multi_write_content_hash(&bytes) != write.content_hash
                    {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "multi-write journal staged payload hash mismatch".to_string(),
                        ));
                    }
                }
                for write in &journal.writes {
                    let bytes = self
                        .read_prefix_path(&write.staged_path, write.size_bytes.saturating_add(1))
                        .await?;
                    if bytes.len() as u64 != write.size_bytes
                        || multi_write_content_hash(&bytes) != write.content_hash
                    {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "multi-write journal staged payload changed during replay".to_string(),
                        ));
                    }
                    self.write_atomic_path(&write.path, &bytes).await?;
                    replayed.push(write.path.clone());
                }
                (replayed, Some(journal.staging_dir))
            },
        };

        match self.remove_file_path(journal_path).await {
            Ok(()) => {},
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {},
            Err(err) => return Err(err),
        }
        if let Some(staging_dir) = staging_dir {
            match self.remove_dir_all_path(staging_dir).await {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => tracing::warn!(
                    error = %error,
                    "committed multi-write journal left an orphan staging directory"
                ),
            }
        }
        Ok(replayed)
    }

    /// Persist a write set, then apply it, then drop the journal.
    ///
    /// **Visible only inside `artifact_v2`, and that narrowing is the
    /// invariant, not tidiness.** This lands a task record without reindexing
    /// or invalidating anything; its one sanctioned caller is
    /// [`TaskWriteReconciler::commit_task_writes`], which does both. Left
    /// `pub`, it was reachable from every holder of
    /// `ArtifactV2Service::workspace()` — `api/task_api_v3.rs`,
    /// `api/programs_api.rs` and `api/events_api.rs` among them — and
    /// `only_the_reconciler_reaches_a_task_write_journal` reads only the flat
    /// `artifact_v2/` directory, so a writer in `api/` or `storage/` could have
    /// landed a stale-index bug with the guard still green. The compiler now
    /// enforces the shape that test approximates; the test stays, because
    /// visibility says nothing about the callers *inside* this module and that
    /// is where the next one would come from.
    ///
    /// [`TaskWriteReconciler::commit_task_writes`]:
    ///     crate::magician_v2::artifact_v2::TaskWriteReconciler::commit_task_writes
    pub(in crate::magician_v2::artifact_v2) async fn commit_multi_write_journal_path<
        P: AsRef<Path>,
    >(
        &self,
        journal_path: P,
        writes: &[(PathBuf, Vec<u8>)],
    ) -> Result<(), ArtifactV2Error> {
        let journal_path = journal_path.as_ref();
        if writes.is_empty() {
            return Ok(());
        }
        if writes.len() > MAX_MULTI_WRITE_OPS {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "multi-write set exceeds {MAX_MULTI_WRITE_OPS} operations"
            )));
        }
        let total_bytes = writes.iter().try_fold(0u64, |total, (_, bytes)| {
            total.checked_add(bytes.len() as u64).ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("multi-write set byte count overflow".to_string())
            })
        })?;
        if total_bytes > MAX_MULTI_WRITE_TOTAL_BYTES {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "multi-write set exceeds {MAX_MULTI_WRITE_TOTAL_BYTES} bytes"
            )));
        }
        // Validate the complete destination set before creating the staging
        // directory or durable journal. Otherwise one invalid path discovered
        // during application would leave a journal that every future recovery
        // correctly rejects but can never complete.
        for (path, _) in writes {
            self.provider_relative_path_for_resolved_path(path)?;
        }
        if self.exists_path(journal_path).await? {
            return Err(ArtifactV2Error::InvalidRequest(
                "pending multi-write journal requires recovery before a new commit".to_string(),
            ));
        }

        let staging_dir = multi_write_staging_dir(journal_path);
        match self.remove_dir_all_path(&staging_dir).await {
            Ok(()) => {},
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
        self.create_dir_all_path(&staging_dir).await?;
        let mut journal_writes = Vec::with_capacity(writes.len());
        for (index, (path, bytes)) in writes.iter().enumerate() {
            let staged_path = staging_dir.join(format!("{index:04}.payload"));
            self.write_atomic_path(&staged_path, bytes).await?;
            journal_writes.push(WorkspacePersistedWriteOpV2 {
                path: path.clone(),
                staged_path,
                size_bytes: bytes.len() as u64,
                content_hash: multi_write_content_hash(bytes),
            });
        }
        let journal = WorkspacePersistedWriteJournalV2 {
            version: 2,
            staging_dir: staging_dir.clone(),
            writes: journal_writes,
        };
        self.write_json_atomic_path(journal_path, &journal).await?;

        for (path, bytes) in writes {
            self.write_atomic_path(path, bytes).await?;
        }

        match self.remove_file_path(journal_path).await {
            Ok(()) => {},
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {},
            Err(err) => return Err(err),
        }
        match self.remove_dir_all_path(staging_dir).await {
            Ok(()) => {},
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => tracing::warn!(
                error = %error,
                "committed multi-write set left an orphan staging directory"
            ),
        }
        Ok(())
    }

    pub async fn remove_file_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<(), ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.remove_file(&relative_path).await
    }

    pub fn remove_file_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<(), ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.remove_file_sync(&relative_path)
    }

    pub async fn remove_dir_all_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<(), ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.remove_dir_all(&relative_path).await
    }

    pub fn remove_dir_all_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<(), ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.remove_dir_all_sync(&relative_path)
    }

    pub fn rename_path_sync<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        from: P,
        to: Q,
    ) -> Result<(), ArtifactV2Error> {
        let from_relative = self.provider_relative_path_for_resolved_path(from.as_ref())?;
        let to_relative = self.provider_relative_path_for_resolved_path(to.as_ref())?;
        self.file_provider.rename_sync(&from_relative, &to_relative)
    }

    pub async fn metadata_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Option<std::fs::Metadata>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.metadata(&relative_path).await
    }

    pub async fn exists_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<bool, ArtifactV2Error> {
        Ok(self.metadata_path(resolved_path).await?.is_some())
    }

    pub fn exists_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<bool, ArtifactV2Error> {
        Ok(self.metadata_path_sync(resolved_path)?.is_some())
    }

    pub async fn symlink_metadata_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Option<std::fs::Metadata>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.symlink_metadata(&relative_path).await
    }

    pub fn metadata_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Option<std::fs::Metadata>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.metadata_sync(&relative_path)
    }

    fn path_exists_sync<P: AsRef<Path>>(&self, resolved_path: P) -> bool {
        self.metadata_path_sync(resolved_path)
            .ok()
            .flatten()
            .is_some()
    }

    pub async fn canonicalize_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<PathBuf, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.canonicalize(&relative_path).await
    }

    pub async fn read_dir_path<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Vec<WorkspaceFileEntry>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.read_dir(&relative_path).await
    }

    pub async fn read_dir_path_or_empty<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Vec<WorkspaceFileEntry>, ArtifactV2Error> {
        match self.read_dir_path(resolved_path).await {
            Ok(entries) => Ok(entries),
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            },
            Err(err) => Err(err),
        }
    }

    /// Return one lexicographically stable page from a directory whose retained
    /// memory and physical scan work are bounded. The provider retains only
    /// `page_size` entries and reports work beyond `scan_ceiling` as explicit
    /// capacity debt while returning the admitted prefix page. Additions at or
    /// before `after` are picked up after the caller completes and resets its
    /// pass.
    pub async fn read_dir_page_path_or_empty<P: AsRef<Path>>(
        &self,
        resolved_path: P,
        after: Option<&str>,
        page_size: usize,
        scan_ceiling: usize,
    ) -> Result<WorkspaceDirectoryPage, ArtifactV2Error> {
        if page_size == 0 || scan_ceiling == 0 || page_size > scan_ceiling {
            return Err(ArtifactV2Error::InvalidRequest(
                "workspace directory page requires 0 < page_size <= scan_ceiling".to_string(),
            ));
        }
        if after.is_some_and(|cursor| {
            cursor.len() > 1024
                || cursor
                    .chars()
                    .any(|character| matches!(character, '/' | '\\' | '\0'))
        }) {
            return Err(ArtifactV2Error::InvalidRequest(
                "workspace directory page cursor is invalid".to_string(),
            ));
        }
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        match self
            .file_provider
            .read_dir_page_bounded(&relative_path, after, page_size, scan_ceiling)
            .await
        {
            Ok(page) => Ok(page),
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(WorkspaceDirectoryPage {
                    entries: Vec::new(),
                    next_after: None,
                    complete: true,
                    overflow: false,
                })
            },
            Err(error) => Err(error),
        }
    }

    pub fn read_dir_path_sync<P: AsRef<Path>>(
        &self,
        resolved_path: P,
    ) -> Result<Vec<WorkspaceFileEntry>, ArtifactV2Error> {
        let relative_path =
            self.provider_relative_path_for_resolved_path(resolved_path.as_ref())?;
        self.file_provider.read_dir_sync(&relative_path)
    }

    /// The scoped runtime root for a given base.
    ///
    /// Post seed/runtime split, scoped runtime state lives DIRECTLY under the
    /// runtime root (`MAGICIAN_ROOT_DIR`, e.g. `$HOME/MagicianNotes/scopes/...`) —
    /// NOT under a nested `magician_data_v3/` subdir. `magician_data_v3` is now
    /// only the repo SEED root (templates). This returns the base unchanged so
    /// every storage component (tasks, secrets, api_mining, definitions, …) shares
    /// one flat runtime root.
    ///
    /// Previously this appended `magician_data_v3` to any base not already named
    /// that, which — after the split moved the runtime root to `$HOME/MagicianNotes`
    /// while leaving the definition store flat — split runtime state between
    /// `<root>/scopes` (definitions) and `<root>/magician_data_v3/scopes` (tasks,
    /// secrets, api_mining). Returning the base directly closes that split.
    pub fn resolve_scoped_root(base_root: &Path) -> PathBuf {
        base_root.to_path_buf()
    }

    pub async fn ensure_root(&self) -> Result<(), ArtifactV2Error> {
        self.file_provider
            .create_dir_all(Path::new("scopes"))
            .await?;
        self.file_provider
            .create_dir_all(Path::new("system"))
            .await?;
        Ok(())
    }

    pub fn ensure_root_sync(&self) -> Result<(), ArtifactV2Error> {
        self.file_provider
            .create_dir_all_sync(Path::new("scopes"))?;
        self.file_provider
            .create_dir_all_sync(Path::new("system"))?;
        Ok(())
    }

    pub async fn list_scope_segments(&self) -> Result<Vec<(String, String)>, ArtifactV2Error> {
        let scopes_root = self.provider_path("scopes");
        let principals = match self.read_dir_path(&scopes_root).await {
            Ok(entries) => entries,
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(err) => return Err(err),
        };

        let mut scopes = Vec::new();
        for principal_entry in principals {
            if !principal_entry.is_dir {
                continue;
            }
            let principal = principal_entry.file_name;

            let workspaces = self
                .file_provider
                .read_dir(&principal_entry.relative_path)
                .await?;
            for workspace_entry in workspaces {
                if !workspace_entry.is_dir {
                    continue;
                }
                let workspace = workspace_entry.file_name;
                scopes.push((principal.clone(), workspace));
            }
        }

        scopes.sort();
        Ok(scopes)
    }

    pub fn list_scope_segments_sync(&self) -> Result<Vec<(String, String)>, ArtifactV2Error> {
        let scopes_root = self.provider_path("scopes");
        let principals = match self.read_dir_path_sync(&scopes_root) {
            Ok(entries) => entries,
            Err(ArtifactV2Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(err) => return Err(err),
        };

        let mut scopes = Vec::new();
        for principal_entry in principals {
            if !principal_entry.is_dir {
                continue;
            }
            let principal = principal_entry.file_name;

            for workspace_entry in self
                .file_provider
                .read_dir_sync(&principal_entry.relative_path)?
            {
                if !workspace_entry.is_dir {
                    continue;
                }
                let workspace = workspace_entry.file_name;
                scopes.push((principal.clone(), workspace));
            }
        }

        scopes.sort();
        Ok(scopes)
    }

    pub fn scope_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.provider_path("scopes")
            .join(safe_segment(principal))
            .join(safe_segment(workspace))
    }

    /// Root of all per-(principal, workspace) scope directories. Used by
    /// retention sweeps and tools that enumerate every scope on disk.
    pub fn scopes_root(&self) -> PathBuf {
        self.provider_path("scopes")
    }

    /// The DIRECTORY NAMES a scope is stored under — the same normalisation
    /// `scope_root` applies before touching the disk.
    ///
    /// The list index learns a scope by walking `scopes/`, so its
    /// `principal`/`workspace` columns hold these names, not whatever a
    /// caller's `ScopeRef` happens to carry. A reader that queried the index
    /// with an un-normalised scope would match no rows and serve an empty
    /// list — a confident, silent wrong answer rather than an error — so
    /// every index read and write goes through here first.
    pub fn scope_dir_segments(principal: &str, workspace: &str) -> (String, String) {
        (safe_segment(principal), safe_segment(workspace))
    }

    /// Per-scope path for scope-owned flat-file state (user-request history/pending,
    /// bot-auth HITL cache) — at the ROOT of the scope folder,
    /// `scopes/<principal>/<workspace>/<file_name>`, keeping that scope-specific
    /// runtime state with its scope instead of at the store root.
    pub fn scope_requests_path(
        &self,
        principal: &str,
        workspace: &str,
        file_name: &str,
    ) -> PathBuf {
        self.scope_root(principal, workspace).join(file_name)
    }

    /// Enumerate every `(principal, workspace)` scope present on disk under
    /// `scopes/`. Empty when the scopes root does not exist yet.
    pub fn list_scopes(&self) -> Vec<(String, String)> {
        let scopes_root = self.scopes_root();
        let mut scopes = Vec::new();
        let Ok(principals) = self.read_dir_path_sync(&scopes_root) else {
            return scopes;
        };
        for principal in principals.into_iter().filter(|entry| entry.is_dir) {
            let principal_dir = scopes_root.join(&principal.file_name);
            let Ok(workspaces) = self.read_dir_path_sync(&principal_dir) else {
                continue;
            };
            for workspace in workspaces.into_iter().filter(|entry| entry.is_dir) {
                scopes.push((principal.file_name.clone(), workspace.file_name.clone()));
            }
        }
        scopes
    }

    /// The async counterpart of [`Self::list_tenant_scopes`], for sweeps that
    /// enumerate through the provider.
    pub async fn list_tenant_scope_segments(
        &self,
    ) -> Result<Vec<(String, String)>, ArtifactV2Error> {
        let mut scopes = self.list_scope_segments().await?;
        scopes.retain(|(principal, workspace)| scope_hosts_user_subsystems(principal, workspace));
        Ok(scopes)
    }

    /// The `list_scope_segments_sync` counterpart of
    /// [`Self::list_tenant_scopes`], for sweeps that need the fallible form.
    pub fn list_tenant_scope_segments_sync(
        &self,
    ) -> Result<Vec<(String, String)>, ArtifactV2Error> {
        let mut scopes = self.list_scope_segments_sync()?;
        scopes.retain(|(principal, workspace)| scope_hosts_user_subsystems(principal, workspace));
        Ok(scopes)
    }

    /// Every scope that hosts a tenant's subsystems — `list_scopes()` without the
    /// reserved sinks.
    ///
    /// This is the enumeration an eager boot sweep wants. A sweep that lists
    /// every directory and initialises itself in each one materialises its
    /// subsystem in buckets that exist only to catch records belonging to no
    /// tenant, which is how a sink came to hold an inbox database, a UI feed
    /// and a dispatch-intent catalogue for a reader that cannot exist. Sweeps
    /// whose state genuinely belongs in a sink — the transport-log event
    /// stream and the analytics derived from it — keep using
    /// [`Self::list_scopes`] deliberately.
    pub fn list_tenant_scopes(&self) -> Vec<(String, String)> {
        let mut scopes = self.list_scopes();
        scopes.retain(|(principal, workspace)| scope_hosts_user_subsystems(principal, workspace));
        scopes
    }

    /// Scopes that currently have a `requests/<file_name>` shard on disk. Used on
    /// persist to also rewrite (empty) scopes that lost all their records, so a
    /// removed entry never resurrects from a stale shard.
    pub fn scopes_with_request_shard(&self, file_name: &str) -> Vec<(String, String)> {
        self.list_scopes()
            .into_iter()
            .filter(|(principal, workspace)| {
                let shard = self.scope_requests_path(principal, workspace, file_name);
                self.metadata_path_sync(&shard).ok().flatten().is_some()
            })
            .collect()
    }

    pub fn system_root(&self) -> PathBuf {
        self.provider_path("system")
    }

    /// The `system/` root that holds bootstrap TEMPLATES. Resolves to the attached
    /// seed root (the repo `magician_data_v3/system`) when one is set, else the
    /// store's own `system/` (dev/local_file). Runtime system state (secrets, chat,
    /// wake queue) always uses `system_root()`.
    /// The seed root this workspace resolves bootstrap templates against: an
    /// explicit `with_seed_root`, else the process-wide default seed root (set at
    /// startup), else `None` (dev/local_file where the seed and the store coincide,
    /// or pre-init bare subsystems — the legacy store-root fallback applies).
    fn effective_seed_root(&self) -> Option<PathBuf> {
        self.seed_root.clone().or_else(default_seed_root)
    }

    pub fn system_seed_root(&self) -> PathBuf {
        match self.effective_seed_root() {
            Some(seed) => seed.join("system"),
            None => self.system_root(),
        }
    }

    /// True when bootstrap templates are served from a read-only seed root distinct
    /// from the runtime store root (the container/deployment case). Seeders must NOT
    /// write into the seed when this is true. False in dev/local_file where the seed
    /// and the store coincide (templates remain writable as before).
    pub fn templates_are_read_only(&self) -> bool {
        match self.effective_seed_root() {
            Some(seed) => seed.as_path() != self.base_root(),
            None => false,
        }
    }

    pub fn default_scope_root(&self) -> PathBuf {
        self.scope_root(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
    }

    /// Dormant app-platform storage root for one authenticated scope.
    ///
    /// These accessors describe the canonical topology only. They deliberately
    /// do not create directories or open the future app store, so introducing
    /// the Phase-0 contract cannot change startup or production behavior.
    pub fn apps_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("apps")
    }

    pub fn app_store_db_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.apps_root(principal, workspace)
            .join("app_store.sqlite3")
    }

    pub fn app_packages_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.apps_root(principal, workspace).join("packages")
    }

    pub fn app_attachments_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.apps_root(principal, workspace).join("attachments")
    }

    pub fn app_exports_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.apps_root(principal, workspace).join("exports")
    }

    pub fn app_captures_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.apps_root(principal, workspace).join("captures")
    }

    pub fn app_evaluations_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.apps_root(principal, workspace).join("evaluations")
    }

    pub fn system_db_templates_root(&self) -> PathBuf {
        self.system_seed_root().join("db_templates")
    }

    pub fn system_agent_template_root(&self) -> PathBuf {
        self.system_seed_root().join("agent_templates")
    }

    pub fn system_trust_policy_template_root(&self) -> PathBuf {
        self.system_seed_root().join("trust_policy_templates")
    }

    /// AgentSkills v1 per-scope skills root (populated by
    /// `make -C skillshub install-scope SCOPE=<id>`). The system-shared
    /// skills tier has been retired — extras declared in
    /// `tool-runtime-config.yaml :: registry.paths` fill the same role
    /// for per-deployment overlays.
    pub fn scope_skills_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("skills")
    }

    pub fn programs_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("programs")
    }

    pub fn program_specs_root(&self, principal: &str, workspace: &str) -> PathBuf {
        // Program SPECS are SCOPED — a focus area belongs to a specific
        // (principal, workspace), not shared across every scope (the rare
        // all-scope focus area is the exception, not the norm). They live as
        // `.md` files directly under the scope's `programs/` dir, alongside the
        // runtime *state* at `programs_root()/state`. (They can't be a sibling
        // `Programs/` folder: a case-insensitive macOS FS collides that with the
        // lowercase `programs/`.) Still SilverBullet-visible — `scopes/` is not a
        // hidden (`.`-prefixed) folder, so the space indexes these `.md` files
        // just as it did the old flat `Programs/`.
        self.programs_root(principal, workspace)
    }

    pub fn program_runtime_states_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.programs_root(principal, workspace).join("state")
    }

    pub fn program_runtime_state_path(
        &self,
        principal: &str,
        workspace: &str,
        state_file_name: &str,
    ) -> PathBuf {
        self.program_runtime_states_dir(principal, workspace)
            .join(safe_segment(state_file_name))
    }

    pub fn programs_anomalies_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.programs_root(principal, workspace).join("anomalies")
    }

    pub fn programs_anomalies_path(
        &self,
        principal: &str,
        workspace: &str,
        file_name: &str,
    ) -> PathBuf {
        self.programs_anomalies_dir(principal, workspace)
            .join(safe_segment(file_name))
    }

    pub fn programs_backlog_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.programs_root(principal, workspace).join("backlog")
    }

    pub fn programs_backlog_path(
        &self,
        principal: &str,
        workspace: &str,
        file_name: &str,
    ) -> PathBuf {
        self.programs_backlog_dir(principal, workspace)
            .join(safe_segment(file_name))
    }

    /// Boundary D's supplemental guidance, one file per program document.
    /// Keyed by the program it supplements because guidance written for one
    /// program must never be read into another — the profile itself carries
    /// `program_relative_path` and the reader checks it, but keying the path
    /// too means a mismatch cannot even be loaded by accident.
    ///
    /// This is the one place the path is spelled. The cycle-prompt reader
    /// (`magician-api`) and the store that applies revisions both call it,
    /// because a writer and a reader that each spell the path themselves
    /// will one day spell it differently, and the boundary is then a file
    /// that is written and a different file that is read.
    pub fn programs_supplemental_profiles_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.programs_root(principal, workspace)
            .join("supplemental")
    }

    pub fn programs_supplemental_profile_path(
        &self,
        principal: &str,
        workspace: &str,
        program_relative_path: &str,
    ) -> PathBuf {
        self.programs_supplemental_profiles_dir(principal, workspace)
            .join(format!(
                "{}.json",
                program_relative_path.replace(['/', '\\'], "_")
            ))
    }

    /// Root of the uniform eval run history for a scope
    /// (`evals/runs/<lane_id>/<run_id>.json`). The eval *reports* themselves are
    /// not here — those stay wherever each lane already writes them, under the
    /// repo's coverage dir; only the uniform per-run record this scope owns
    /// lives in the scope.
    pub fn evals_runs_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("evals")
            .join("runs")
    }

    /// One lane's run history. A directory per lane, because the lane is the
    /// unit the page reads and trends by.
    pub fn evals_run_lane_dir(&self, principal: &str, workspace: &str, lane_id: &str) -> PathBuf {
        self.evals_runs_dir(principal, workspace)
            .join(safe_segment(lane_id))
    }

    /// One eval run record. Both the lane id and the file name go through
    /// `safe_segment`: lane ids come from Makefile target names, and while the
    /// eval registry already flags a target that is not path-shaped, a flagged
    /// lane is still a lane that can be run — so it must not be able to write
    /// outside its scope.
    pub fn evals_run_path(
        &self,
        principal: &str,
        workspace: &str,
        lane_id: &str,
        file_name: &str,
    ) -> PathBuf {
        self.evals_run_lane_dir(principal, workspace, lane_id)
            .join(safe_segment(file_name))
    }

    // bot_configs.yaml + bots/ source-of-truth moved to skillshub/bots/.
    // The corresponding `system_capability_template_bot_configs_path` and
    // `system_capability_template_bots_root` methods are gone — call sites
    // construct paths via `CapabilityWorkspaceManager::skillshub_bots_root()`
    // (which has access to `repo_root`).

    pub fn system_trust_policy_template_path(&self) -> PathBuf {
        self.system_trust_policy_template_root()
            .join("trust_policies.template.yaml")
    }

    pub fn system_trust_policy_default_path(&self) -> PathBuf {
        self.system_trust_policy_template_root()
            .join("trust_policies.default.yaml")
    }

    pub fn feed_db_template_dir(&self) -> PathBuf {
        self.system_db_templates_root().join("feed")
    }

    pub fn feed_db_template_schema_path(&self) -> PathBuf {
        self.feed_db_template_dir().join("schema.sql")
    }

    pub fn ui_threads_db_template_dir(&self) -> PathBuf {
        self.system_db_templates_root().join("ui_threads")
    }

    pub fn ui_threads_db_template_schema_path(&self) -> PathBuf {
        self.ui_threads_db_template_dir().join("schema.sql")
    }

    pub fn analytics_db_template_dir(&self) -> PathBuf {
        self.system_db_templates_root().join("analytics")
    }

    pub fn analytics_db_template_schema_path(&self) -> PathBuf {
        self.analytics_db_template_dir().join("schema.sql")
    }

    pub fn mail_assist_db_template_dir(&self) -> PathBuf {
        self.system_db_templates_root().join("mail_assist")
    }

    pub fn channel_assist_db_template_dir(&self) -> PathBuf {
        self.mail_assist_db_template_dir()
    }

    pub fn mail_assist_db_template_schema_path(&self) -> PathBuf {
        self.mail_assist_db_template_dir().join("schema.sql")
    }

    pub fn channel_assist_db_template_schema_path(&self) -> PathBuf {
        self.mail_assist_db_template_schema_path()
    }

    pub fn scoped_agent_runtime_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("agent_runtime")
    }

    pub fn resource_authority_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("resource_authority")
    }

    /// Scoped Mail Assist data dir (legacy physical location for Channel Assist).
    pub fn mail_assist_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("mail_assist")
    }

    /// Scoped Channel Assist data dir.
    ///
    /// This intentionally points at the legacy `mail_assist` directory so
    /// existing stores and backups remain stable during the compatibility window.
    pub fn channel_assist_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.mail_assist_dir(principal, workspace)
    }

    pub fn mail_assist_db_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.mail_assist_dir(principal, workspace)
            .join("mail_assist.duckdb")
    }

    pub fn channel_assist_db_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.mail_assist_db_path(principal, workspace)
    }

    pub fn mail_assist_lock_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.mail_assist_dir(principal, workspace)
            .join("mail_assist.lock")
    }

    pub fn channel_assist_lock_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.mail_assist_lock_path(principal, workspace)
    }

    pub fn capability_evolution_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("capability_evolution")
    }

    pub fn capability_evolution_backlog_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("backlog")
    }

    pub fn capability_evolution_proposals_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("proposals")
    }

    pub fn capability_evolution_validations_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("validations")
    }

    pub fn capability_evolution_implementations_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("implementations")
    }

    pub fn capability_evolution_applications_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("applications")
    }

    pub fn capability_evolution_rollback_recommendations_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("rollback_recommendations")
    }

    pub fn capability_evolution_post_promotion_monitors_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("post_promotion_monitors")
    }

    pub fn capability_evolution_steward_runs_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("steward_runs")
    }

    pub fn capability_evolution_candidate_validations_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.capability_evolution_validations_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn capability_evolution_candidate_implementations_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.capability_evolution_implementations_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn capability_evolution_candidate_applications_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.capability_evolution_applications_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn capability_evolution_candidate_rollback_recommendations_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.capability_evolution_rollback_recommendations_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn capability_evolution_backlog_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.capability_evolution_backlog_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(candidate_id)))
    }

    pub fn capability_evolution_proposal_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.capability_evolution_proposals_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(candidate_id)))
    }

    pub fn capability_evolution_validation_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        validation_id: &str,
    ) -> PathBuf {
        self.capability_evolution_candidate_validations_dir(principal, workspace, candidate_id)
            .join(format!("{}.json", safe_segment(validation_id)))
    }

    pub fn capability_evolution_implementation_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        implementation_id: &str,
    ) -> PathBuf {
        self.capability_evolution_candidate_implementations_dir(principal, workspace, candidate_id)
            .join(format!("{}.json", safe_segment(implementation_id)))
    }

    pub fn capability_evolution_application_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        application_id: &str,
    ) -> PathBuf {
        self.capability_evolution_candidate_applications_dir(principal, workspace, candidate_id)
            .join(format!("{}.json", safe_segment(application_id)))
    }

    pub fn capability_evolution_rollback_recommendation_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        recommendation_id: &str,
    ) -> PathBuf {
        self.capability_evolution_candidate_rollback_recommendations_dir(
            principal,
            workspace,
            candidate_id,
        )
        .join(format!("{}.json", safe_segment(recommendation_id)))
    }

    pub fn capability_evolution_post_promotion_monitor_path(
        &self,
        principal: &str,
        workspace: &str,
        promotion_id: &str,
    ) -> PathBuf {
        self.capability_evolution_post_promotion_monitors_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(promotion_id)))
    }

    pub fn capability_evolution_steward_run_path(
        &self,
        principal: &str,
        workspace: &str,
        run_id: &str,
    ) -> PathBuf {
        self.capability_evolution_steward_runs_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(run_id)))
    }

    /// Legacy-compat read path for the learning subsystem's capability/skill
    /// evolution promotion audit. This `capability_evolution/promotion_audit.jsonl`
    /// file is SHARED on disk with the api-mining capability-pack store
    /// (`execution::capability_pack`). That sharing is safe: the learning
    /// promotion reader scans this file alongside
    /// `skill_evolution_promotion_audit_path` and SKIPS any foreign row that does
    /// not deserialize as a learning promotion record (the `foreign_count` skip
    /// path), so api-mining rows never contaminate learning reads. New learning
    /// promotion records are written to `skill_evolution_promotion_audit_path`;
    /// this stays a read-only legacy source so historical learning rows already
    /// on disk remain visible without any migration.
    pub fn capability_evolution_promotion_audit_path(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.capability_evolution_root(principal, workspace)
            .join("promotion_audit.jsonl")
    }

    pub fn skill_evolution_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("skill_evolution")
    }

    pub fn skill_evolution_backlog_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("backlog")
    }

    pub fn skill_evolution_proposals_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("proposals")
    }

    pub fn skill_evolution_validations_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("validations")
    }

    pub fn skill_evolution_implementations_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("implementations")
    }

    pub fn skill_evolution_applications_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("applications")
    }

    pub fn skill_evolution_rollback_recommendations_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("rollback_recommendations")
    }

    pub fn skill_evolution_post_promotion_monitors_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("post_promotion_monitors")
    }

    pub fn skill_evolution_steward_runs_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("steward_runs")
    }

    pub fn skill_evolution_candidate_validations_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.skill_evolution_validations_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn skill_evolution_candidate_implementations_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.skill_evolution_implementations_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn skill_evolution_candidate_applications_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.skill_evolution_applications_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn skill_evolution_candidate_rollback_recommendations_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.skill_evolution_rollback_recommendations_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn skill_evolution_backlog_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.skill_evolution_backlog_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(candidate_id)))
    }

    pub fn skill_evolution_proposal_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.skill_evolution_proposals_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(candidate_id)))
    }

    pub fn skill_evolution_validation_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        validation_id: &str,
    ) -> PathBuf {
        self.skill_evolution_candidate_validations_dir(principal, workspace, candidate_id)
            .join(format!("{}.json", safe_segment(validation_id)))
    }

    pub fn skill_evolution_implementation_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        implementation_id: &str,
    ) -> PathBuf {
        self.skill_evolution_candidate_implementations_dir(principal, workspace, candidate_id)
            .join(format!("{}.json", safe_segment(implementation_id)))
    }

    pub fn skill_evolution_application_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        application_id: &str,
    ) -> PathBuf {
        self.skill_evolution_candidate_applications_dir(principal, workspace, candidate_id)
            .join(format!("{}.json", safe_segment(application_id)))
    }

    pub fn skill_evolution_rollback_recommendation_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        recommendation_id: &str,
    ) -> PathBuf {
        self.skill_evolution_candidate_rollback_recommendations_dir(
            principal,
            workspace,
            candidate_id,
        )
        .join(format!("{}.json", safe_segment(recommendation_id)))
    }

    pub fn skill_evolution_post_promotion_monitor_path(
        &self,
        principal: &str,
        workspace: &str,
        promotion_id: &str,
    ) -> PathBuf {
        self.skill_evolution_post_promotion_monitors_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(promotion_id)))
    }

    pub fn skill_evolution_steward_run_path(
        &self,
        principal: &str,
        workspace: &str,
        run_id: &str,
    ) -> PathBuf {
        self.skill_evolution_steward_runs_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(run_id)))
    }

    pub fn skill_evolution_promotion_audit_path(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.skill_evolution_root(principal, workspace)
            .join("promotion_audit.jsonl")
    }

    pub fn system_secrets_root(&self) -> PathBuf {
        // Operator setup token lives at the runtime-root top level
        // (`<root>/secrets/`), NOT under system/ (which is read-only,
        // seed-derived templates).
        self.provider_path("secrets")
    }

    pub fn system_chat_root(&self) -> PathBuf {
        self.system_root().join("chat")
    }

    pub fn chat_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("chat")
    }

    pub fn chat_enrollments_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.chat_root(principal, workspace)
            .join("enrollments.json")
    }

    pub fn tasks_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("tasks")
    }

    /// Parallel folder to `tasks/` for internal (non-user-visible)
    /// tasks. Chat-inline delegate transients, system tasks, anything
    /// the user didn't explicitly opt into tracking lands here.
    /// User-facing `/tasks` listings + lookups read from `tasks/`
    /// only; the `/internal-tasks` debug surface reads from here.
    pub fn internal_tasks_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("internal_tasks")
    }

    /// Small authoritative discovery lane for exact planning receipts. One
    /// stable file per task is journaled before a new TaskPlan, so normal boot
    /// recovery never depends on the rebuildable list cache being complete.
    pub fn task_planning_recovery_catalog_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("task_planning_recovery")
    }

    pub fn task_planning_recovery_catalog_entry_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.task_planning_recovery_catalog_dir(principal, workspace)
            .join(format!("{task_id}.json"))
    }

    pub fn task_planning_recovery_catalog_bootstrap_path(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.task_planning_recovery_catalog_dir(principal, workspace)
            .join("bootstrap-v1.json")
    }

    /// Restricted, compact discovery lane for AskLoop continuations that are
    /// not embedded in an Artifact TaskPlan. Each HMAC-named entry is the
    /// complete restart authority for one exact paused Runtime generation.
    pub fn runtime_resume_recovery_catalog_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("restricted")
            .join("runtime_resume_recovery")
    }

    pub fn runtime_resume_recovery_catalog_entry_path(
        &self,
        principal: &str,
        workspace: &str,
        recovery_id: &str,
    ) -> PathBuf {
        self.runtime_resume_recovery_catalog_dir(principal, workspace)
            .join(format!("{recovery_id}.json"))
    }

    pub fn runtime_resume_recovery_catalog_lock_path(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.runtime_resume_recovery_catalog_dir(principal, workspace)
            .join(".catalog.lock")
    }

    pub fn runtime_resume_recovery_catalog_cursor_path(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.runtime_resume_recovery_catalog_dir(principal, workspace)
            .join(".scan-cursor.json")
    }

    /// Parallel-folder analog of `task_dir` rooted under
    /// `internal_tasks/`. Same on-disk layout — every subpath helper
    /// (`internal_task_manifest_path`, `internal_task_state_path`, …)
    /// derives from this.
    pub fn internal_task_dir(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.internal_tasks_root(principal, workspace).join(task_id)
    }

    pub fn internal_task_state_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.internal_task_dir(principal, workspace, task_id)
            .join("state")
    }

    pub fn internal_task_outputs_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.internal_task_dir(principal, workspace, task_id)
            .join("outputs")
    }

    pub fn internal_task_manifest_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.internal_task_dir(principal, workspace, task_id)
            .join("manifest.json")
    }

    pub fn internal_task_state_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.internal_task_state_dir(principal, workspace, task_id)
            .join("task_state.json")
    }

    pub fn internal_task_refs_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.internal_task_dir(principal, workspace, task_id)
            .join("task_refs.json")
    }

    /// Resolve the actual on-disk directory for `task_id`, checking
    /// the canonical `internal_tasks/` folder first, then the
    /// user-visible `tasks/` fallback. Returns `None` when the task
    /// doesn't exist in either folder.
    ///
    /// Synchronous (uses the workspace file provider's metadata probe) so it
    /// composes with the non-async path helpers above without coloring every
    /// caller `async`. The cost is one stat call per miss; the happy path
    /// (user-visible) hits on the first probe.
    pub fn resolve_task_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Option<PathBuf> {
        // Use the explicit raw paths here. `task_dir` itself probes,
        // which would conflate the two checks below and lose the
        // "which folder did we actually find it in?" signal that
        // callers like `resolve_task_location` need.
        let internal = self.internal_task_dir(principal, workspace, task_id);
        if self.path_exists_sync(&internal) {
            return Some(internal);
        }
        let user_visible = self.user_visible_task_dir(principal, workspace, task_id);
        if self.path_exists_sync(&user_visible) {
            return Some(user_visible);
        }
        None
    }

    /// Like `resolve_task_dir` but returns an enum tag identifying
    /// which folder the task came from. Callers that want to apply
    /// per-folder behaviour (e.g. derive sibling paths) use this.
    pub fn resolve_task_location(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Option<TaskLocation> {
        if self.path_exists_sync(self.internal_task_dir(principal, workspace, task_id)) {
            return Some(TaskLocation::Internal);
        }
        if self.path_exists_sync(self.user_visible_task_dir(principal, workspace, task_id)) {
            return Some(TaskLocation::UserVisible);
        }
        None
    }

    pub fn scoped_executions_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("executions")
    }

    pub fn scoped_execution_dir(
        &self,
        principal: &str,
        workspace: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.scoped_executions_root(principal, workspace)
            .join(safe_segment(execution_id))
    }

    pub fn memory_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("memory")
    }

    pub fn learning_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("learning")
    }

    pub fn learning_events_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_root(principal, workspace).join("events")
    }

    pub fn learning_skill_invocations_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_root(principal, workspace)
            .join("skill_invocations")
    }

    pub fn learning_skill_invocation_day_dir(
        &self,
        principal: &str,
        workspace: &str,
        date: &str,
    ) -> PathBuf {
        self.learning_skill_invocations_dir(principal, workspace)
            .join(format!("dt={}", safe_segment(date)))
    }

    pub fn learning_skill_invocation_path(
        &self,
        principal: &str,
        workspace: &str,
        date: &str,
        invocation_id: &str,
    ) -> PathBuf {
        self.learning_skill_invocation_day_dir(principal, workspace, date)
            .join(format!("{}.json", safe_segment(invocation_id)))
    }

    pub fn learning_skill_invocation_failure_clusters_dir(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.learning_root(principal, workspace)
            .join("skill_invocation_failure_clusters")
    }

    pub fn learning_skill_invocation_failure_cluster_path(
        &self,
        principal: &str,
        workspace: &str,
        cluster_id: &str,
    ) -> PathBuf {
        self.learning_skill_invocation_failure_clusters_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(cluster_id)))
    }

    pub fn learning_skill_invocation_failure_cluster_lock_path(
        &self,
        principal: &str,
        workspace: &str,
        cluster_id: &str,
    ) -> PathBuf {
        self.learning_skill_invocation_failure_clusters_dir(principal, workspace)
            .join(format!(".{}.lock", safe_segment(cluster_id)))
    }

    pub fn learning_candidates_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_root(principal, workspace).join("candidates")
    }

    pub fn learning_candidate_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.learning_candidates_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(candidate_id)))
    }

    pub fn learning_decisions_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_root(principal, workspace).join("decisions")
    }

    pub fn learning_candidate_decisions_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.learning_decisions_dir(principal, workspace)
            .join(format!("{}.jsonl", safe_segment(candidate_id)))
    }

    pub fn learning_evaluations_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_root(principal, workspace).join("evaluations")
    }

    pub fn learning_evaluation_backlog_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_evaluations_dir(principal, workspace)
            .join("backlog")
    }

    pub fn learning_evaluation_backlog_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.learning_evaluation_backlog_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(candidate_id)))
    }

    pub fn learning_evaluation_runs_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_evaluations_dir(principal, workspace)
            .join("runs")
    }

    pub fn learning_evaluation_candidate_runs_dir(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> PathBuf {
        self.learning_evaluation_runs_dir(principal, workspace)
            .join(safe_segment(candidate_id))
    }

    pub fn learning_evaluation_run_path(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        run_id: &str,
    ) -> PathBuf {
        self.learning_evaluation_candidate_runs_dir(principal, workspace, candidate_id)
            .join(format!("{}.json", safe_segment(run_id)))
    }

    pub fn learning_growth_evaluation_runs_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_evaluations_dir(principal, workspace)
            .join("growth_runs")
    }

    pub fn learning_growth_evaluation_run_path(
        &self,
        principal: &str,
        workspace: &str,
        run_id: &str,
    ) -> PathBuf {
        self.learning_growth_evaluation_runs_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(run_id)))
    }

    pub fn learning_procedures_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_root(principal, workspace).join("procedures")
    }

    pub fn learning_procedure_status_dir(
        &self,
        principal: &str,
        workspace: &str,
        status: &str,
    ) -> PathBuf {
        self.learning_procedures_dir(principal, workspace)
            .join(safe_segment(status))
    }

    pub fn learning_procedure_path(
        &self,
        principal: &str,
        workspace: &str,
        status: &str,
        procedure_id: &str,
    ) -> PathBuf {
        self.learning_procedure_status_dir(principal, workspace, status)
            .join(format!("{}.yaml", safe_segment(procedure_id)))
    }

    pub fn learning_procedure_decisions_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_procedures_dir(principal, workspace)
            .join("decisions")
    }

    pub fn learning_procedure_decisions_path(
        &self,
        principal: &str,
        workspace: &str,
        procedure_id: &str,
    ) -> PathBuf {
        self.learning_procedure_decisions_dir(principal, workspace)
            .join(format!("{}.jsonl", safe_segment(procedure_id)))
    }

    pub fn learning_procedure_index_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_procedures_dir(principal, workspace)
            .join("index")
    }

    pub fn learning_procedure_lancedb_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_procedure_index_dir(principal, workspace)
            .join("lancedb")
    }

    pub fn learning_procedure_index_manifest_path(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.learning_procedure_index_dir(principal, workspace)
            .join("manifest.json")
    }

    pub fn learning_procedure_index_dirty_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_procedure_index_dir(principal, workspace)
            .join("dirty.json")
    }

    pub fn learning_audits_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.learning_root(principal, workspace).join("audits")
    }

    pub fn runtime_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("runtime")
    }

    pub fn durable_artifacts_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("durable_artifacts")
    }

    pub fn analytics_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("analytics")
    }

    pub fn analytics_db_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("analytics.duckdb")
    }

    pub fn analytics_schema_catalog_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("schema_catalog.json")
    }

    /// Root directory for per-(principal, workspace) Parquet partitions of
    /// `LLMResponseReceived` rows. Layout: `<scope>/analytics/llm_calls/dt=YYYY-MM-DD/<batch>.parquet`.
    /// Queried via DuckDB's `read_parquet(...)` from `analytics_api`.
    pub fn analytics_llm_calls_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace).join("llm_calls")
    }

    /// Root directory for per-(principal, workspace) Parquet partitions of
    /// local embedding-call rows (one flat record per embed batch). Kept
    /// separate from `llm_calls` so the hot per-call query path is never
    /// bloated by high-frequency embedding telemetry.
    /// Layout: `<scope>/analytics/llm_embeddings/dt=YYYY-MM-DD/embed_<ulid>.parquet`.
    pub fn analytics_llm_embeddings_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_embeddings")
    }

    /// Durable append-before-materialize journal for canonical LLM trace facts.
    /// The journal is deliberately separate from Parquet/read models so a
    /// materializer failure cannot erase an already accepted fact.
    pub fn analytics_llm_trace_journal_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_trace_journal")
    }

    /// Dedicated append-before-materialize journal for sanitized content.
    /// Keeping this separate from canonical fact replay gives content its own
    /// deletion and retention lifecycle.
    pub fn analytics_llm_restricted_journal_root(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_restricted_journal")
    }

    /// Canonical provider-attempt lifecycle facts materialized from the durable
    /// LLM trace journal. Raw revisions remain immutable; stable latest-attempt
    /// views are installed by the governed-read phase.
    pub fn analytics_llm_provider_attempts_root(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_provider_attempts")
    }

    /// Content-free lifecycle and causal edges between a model-emitted tool
    /// call, its authoritative runtime execution, later result consumers, and
    /// optional rollback/branch materialization.
    pub fn analytics_llm_tool_calls_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_tool_calls")
    }

    /// Exact capture-loss facts emitted by the bounded trace recorder.
    pub fn analytics_llm_capture_gaps_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_capture_gaps")
    }

    /// Sanitized request/response payload revisions. This root is restricted:
    /// ordinary fact SQL, generic files, and analyst DuckDB projections must
    /// never register or expose it.
    pub fn analytics_llm_call_io_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_call_io")
    }

    /// Content-free provenance descriptors for the blocks selected into an
    /// LLM request. Payload references resolve only through the restricted
    /// reader and never through this metadata dataset itself.
    pub fn analytics_llm_context_blocks_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_context_blocks")
    }

    pub fn analytics_llm_content_tombstones_root(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_content_tombstones")
    }

    pub fn analytics_llm_content_access_audit_root(
        &self,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_content_access_audit")
    }

    /// Content-free catalog for the canonical LLM fact registry and the
    /// governed source selection currently visible in this exact scope.
    pub fn analytics_llm_fact_catalog_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("llm_fact_catalog.json")
    }

    /// Durable daily snapshots for the scoped crew-health read model.
    pub fn analytics_agent_health_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("agent_health")
    }

    pub fn analytics_agent_health_history_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_agent_health_root(principal, workspace)
            .join("history.json")
    }

    /// Root directory for per-(principal, workspace) Parquet partitions of
    /// memory retrieval/consolidation audit rows. Layout:
    /// `<scope>/analytics/memory_events/dt=YYYY-MM-DD/<batch>.parquet`.
    pub fn analytics_memory_events_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("memory_events")
    }

    /// Root directory for the activity spine — one Parquet row per completed
    /// span, from `analytics::activity_rows_sink`. Layout:
    /// `<scope>/analytics/activity_rows/dt=YYYY-MM-DD/hour=HH/batch_<ulid>.parquet`.
    ///
    /// The hour level is what the other analytics datasets do not have, and it
    /// is deliberate: this dataset writes an order of magnitude more rows than
    /// any of them, so a closed hour is the smallest unit worth compacting on
    /// its own rather than waiting for the day to end.
    pub fn analytics_activity_rows_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("activity_rows")
    }

    /// Root directory for the activity spine's rolled-up tier — one row per
    /// `(dt, hour, kind, workload_class, agent_id, outcome)`, written when the
    /// detail rows behind it expire. Layout:
    /// `<scope>/analytics/activity_rollups/dt=YYYY-MM-DD/rollup_<ulid>.parquet`.
    ///
    /// A sibling root rather than a subdirectory of `activity_rows` so the two
    /// tiers have separate retention, separate readers and separate
    /// storage-governance rows. Nested under the detail root, the 13-month tier
    /// would be swept by the 7-day rule that empties the tier above it.
    pub fn analytics_activity_rollups_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.analytics_root(principal, workspace)
            .join("activity_rollups")
    }

    pub fn api_mining_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("api_mining")
    }

    pub fn secrets_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("secrets")
    }

    pub fn pause_states_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.runtime_root(principal, workspace).join("pause_states")
    }

    pub fn ui_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("ui")
    }

    pub fn progress_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("progress_channels")
    }

    /// `<scope>/` — the scope's runtime tree root. Pre-refactor this was
    /// `<scope>/capabilities/`; bots/auth/workdirs are now siblings of
    /// skills/ directly at scope root, so the `capabilities/` umbrella
    /// is gone. Method name kept for callsite stability — the path it
    /// returns is just the scope root now.
    pub fn capabilities_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
    }

    pub fn capability_bots_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("bots")
    }

    pub fn capability_auth_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("auth")
    }

    pub fn capability_workdirs_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace).join("workdirs")
    }

    pub fn capability_home_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.capability_workdirs_root(principal, workspace)
            .join("home")
    }

    /// `<scope>/bots/bot_configs.yaml` — colocated with the bot
    /// bundles since it's a bot-launcher concern (defines which
    /// bot daemons to spawn, env files, restart policy, etc.).
    pub fn capability_bot_configs_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("bots")
            .join("bot_configs.yaml")
    }

    /// Per-workspace personality templates dir. Mirrors the system-level
    /// catalog at `system_capability_template_root().join("personality")`
    /// but is private to the scope. Resolution order at lookup time is
    /// workspace-first, system-fallback: the chat-runtime
    /// `switch_personality` tool checks this dir before falling back to
    /// the shared system catalog, so a workspace can shadow a baseline
    /// preset (or define its own) without affecting any other workspace.
    pub fn capability_personality_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.capabilities_root(principal, workspace)
            .join("personality")
    }

    pub fn progress_events_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.progress_root(principal, workspace).join("events")
    }

    pub fn progress_subscriptions_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.progress_root(principal, workspace)
            .join("subscriptions.json")
    }

    pub fn progress_lineage_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.progress_root(principal, workspace)
            .join("lineage_index.json")
    }

    pub fn ui_indexes_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_root(principal, workspace).join("indexes")
    }

    pub fn ui_feed_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_root(principal, workspace).join("feed")
    }

    pub fn ui_feed_db_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_feed_dir(principal, workspace).join("feed.duckdb")
    }

    pub fn ui_feed_lock_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_feed_dir(principal, workspace).join("feed.lock")
    }

    pub fn ui_threads_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_root(principal, workspace).join("threads")
    }

    pub fn ui_threads_db_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_threads_dir(principal, workspace)
            .join("ui_threads.duckdb")
    }

    pub fn ui_threads_lock_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_threads_dir(principal, workspace)
            .join("ui_threads.lock")
    }

    pub fn published_surfaces_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_root(principal, workspace)
            .join("published_surfaces")
    }

    pub fn chat_sessions_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_root(principal, workspace).join("chat_sessions")
    }

    /// Per-chat-turn event log directory:
    ///   `<scope>/ui/chat_turn_events/`
    /// Each chat turn gets a `<chat_turn_id>.jsonl` file inside,
    /// holding the activity events that fired during that turn
    /// (LLM, tool, reasoning, delegate status — everything the
    /// activity card renders). This is the SOLE store the activity
    /// card reads from: live SSE + REST refresh both consume it,
    /// so the two views agree by construction. The raw per-scope
    /// `events.jsonl` log keeps existing for debug/observability
    /// surfaces but is no longer the chat activity card's source.
    pub fn chat_turn_events_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_root(principal, workspace).join("chat_turn_events")
    }

    pub fn chat_turn_events_path(
        &self,
        principal: &str,
        workspace: &str,
        chat_turn_id: &str,
    ) -> PathBuf {
        self.chat_turn_events_dir(principal, workspace)
            .join(format!("{}.jsonl", safe_segment(chat_turn_id)))
    }

    pub fn legacy_chat_session_path(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> PathBuf {
        self.chat_sessions_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(session_id)))
    }

    pub fn chat_session_dir(&self, principal: &str, workspace: &str, session_id: &str) -> PathBuf {
        self.chat_sessions_dir(principal, workspace)
            .join(safe_segment(session_id))
    }

    pub fn chat_session_path(&self, principal: &str, workspace: &str, session_id: &str) -> PathBuf {
        self.chat_session_dir(principal, workspace, session_id)
            .join("session.json")
    }

    pub fn chat_session_outputs_dir(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> PathBuf {
        self.chat_session_dir(principal, workspace, session_id)
            .join("outputs")
    }

    pub fn chat_session_file_index_path(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> PathBuf {
        self.chat_session_dir(principal, workspace, session_id)
            .join("file_index.json")
    }

    /// Stable cross-process lifecycle lock target for a chat session. It lives
    /// outside the session directory so deleting that directory cannot split
    /// the lock inode from a writer that was already waiting on it.
    pub fn chat_session_lifecycle_lock_path(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> PathBuf {
        self.chat_sessions_dir(principal, workspace)
            .join(".lifecycle")
            .join("locks")
            .join(format!("{}.lock", safe_segment(session_id)))
    }

    /// Durable deletion fence for stale chat-session writers. This marker is
    /// intentionally outside the deletable session tree and remains after a
    /// successful delete.
    pub fn chat_session_deletion_marker_path(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> PathBuf {
        self.chat_sessions_dir(principal, workspace)
            .join(".lifecycle")
            .join("deleted")
            .join(format!("{}.deleted.json", safe_segment(session_id)))
    }

    /// Current storage generation for a chat-session id. A deliberate restore
    /// that reuses an id publishes a new generation under the lifecycle lock;
    /// stale writers from the prior generation then fail before touching the
    /// recreated session directory.
    pub fn chat_session_generation_marker_path(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> PathBuf {
        self.chat_sessions_dir(principal, workspace)
            .join(".lifecycle")
            .join("generations")
            .join(format!("{}.generation.json", safe_segment(session_id)))
    }

    /// Durable per-session journal for metadata-first output reclamation.
    /// It survives individual file-index replacements and is removed only
    /// after every recorded physical unlink has completed idempotently.
    pub fn chat_session_output_cleanup_intent_path(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> PathBuf {
        self.chat_session_dir(principal, workspace, session_id)
            .join("output_cleanup_intent.json")
    }

    pub fn published_surface_path(
        &self,
        principal: &str,
        workspace: &str,
        surface_id: &str,
    ) -> PathBuf {
        self.published_surfaces_dir(principal, workspace)
            .join(format!("{}.json", safe_segment(surface_id)))
    }

    pub fn published_surfaces_index_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.ui_indexes_dir(principal, workspace)
            .join("published_surfaces.json")
    }

    pub fn memory_agents_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.memory_root(principal, workspace).join("agents")
    }

    pub fn memory_agent_dir(&self, principal: &str, workspace: &str, agent_id: &str) -> PathBuf {
        self.memory_agents_root(principal, workspace).join(agent_id)
    }

    pub fn memory_agent_episodes_dir(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
    ) -> PathBuf {
        self.memory_agent_dir(principal, workspace, agent_id)
            .join("episodes")
    }

    pub fn memory_agent_tiers_dir(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
    ) -> PathBuf {
        self.memory_agent_dir(principal, workspace, agent_id)
            .join("tiers")
    }

    pub fn memory_agent_consolidations_dir(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
    ) -> PathBuf {
        self.memory_agent_dir(principal, workspace, agent_id)
            .join("consolidations")
    }

    pub fn memory_agent_corrections_path(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
    ) -> PathBuf {
        self.memory_agent_dir(principal, workspace, agent_id)
            .join("corrections.jsonl")
    }

    pub fn memory_agent_episode_path(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
        episode_id: &str,
    ) -> PathBuf {
        self.memory_agent_episodes_dir(principal, workspace, agent_id)
            .join(format!("{episode_id}.json"))
    }

    /// Resolve a task's directory. Probes `internal_tasks/<id>/` first
    /// (cheap stat) and falls back to `tasks/<id>/` when the task is
    /// either user-visible or doesn't exist yet. All derived helpers
    /// (`task_state_dir`, `task_outputs_dir`, `task_manifest_path`,
    /// etc.) compose on top of this so a single probe routes reads
    /// AND writes for both folders.
    ///
    /// For NEW internal tasks, callers must materialize
    /// `internal_tasks/<id>/` first via
    /// `ensure_task_workspace_for_lifecycle` so the probe finds it.
    /// Otherwise the file lands in `tasks/<id>/` and the task ends up
    /// user-visible — the very bug this routing exists to prevent.
    pub fn task_dir(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        let internal = self.internal_task_dir(principal, workspace, task_id);
        if self.path_exists_sync(&internal) {
            return internal;
        }
        self.tasks_root(principal, workspace).join(task_id)
    }

    /// Task ids are persisted as one literal directory segment. Reject rather
    /// than sanitize so two caller-provided ids can never alias the same task
    /// directory and hostile ids cannot escape their exact scope root.
    pub fn validate_task_id(task_id: &str) -> Result<(), ArtifactV2Error> {
        if task_id.is_empty()
            || task_id.trim() != task_id
            || task_id == "."
            || task_id == ".."
            || Path::new(task_id).is_absolute()
            || task_id
                .chars()
                .any(|ch| ch == '/' || ch == '\\' || ch == ':' || ch.is_control())
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "invalid_task_id".to_string(),
            ));
        }

        let mut components = Path::new(task_id).components();
        if !matches!(components.next(), Some(Component::Normal(segment)) if segment == task_id)
            || components.next().is_some()
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "invalid_task_id".to_string(),
            ));
        }
        Ok(())
    }

    /// Always returns the `tasks/<id>/` path regardless of which
    /// folder the task lives in. Used by deletion code that probes
    /// each folder explicitly.
    pub fn user_visible_task_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.tasks_root(principal, workspace).join(task_id)
    }

    pub fn task_state_dir(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_dir(principal, workspace, task_id).join("state")
    }

    pub fn task_outputs_dir(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_dir(principal, workspace, task_id).join("outputs")
    }

    pub fn task_plans_dir(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_dir(principal, workspace, task_id).join("plans")
    }

    pub fn task_indexes_dir(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_dir(principal, workspace, task_id).join("indexes")
    }

    pub fn task_manifest_path(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_dir(principal, workspace, task_id)
            .join("manifest.json")
    }

    pub fn task_lock_path(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join(".task_lifecycle")
            .join("locks")
            .join(format!("{task_id}.lock"))
    }

    /// Stable cross-process admission lock used before deterministic task
    /// compare-or-create and root-execution start. It is deliberately distinct
    /// from `task_lock_path`: both operations call reducers which acquire the
    /// ordinary task-record lock, so reusing that file would self-deadlock on a
    /// nested flock descriptor. Lock order is admission, then task record, then
    /// any canonical event lock.
    pub fn task_start_admission_lock_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.scope_root(principal, workspace)
            .join(".task_lifecycle")
            .join("start_admission_locks")
            .join(format!("{task_id}.lock"))
    }

    /// Cross-process serialization point shared by canonical event appends
    /// and the durable deletion fence. It is separate from the task record
    /// lock but uses the same stable lock order: task writers take the task
    /// lock first and the event lock second.
    pub fn task_event_lock_path(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join(".task_lifecycle")
            .join("event_locks")
            .join(format!("{task_id}.lock"))
    }

    /// Durable fence written before physical deletion. It intentionally lives
    /// outside the task directory: post-answer summary/reflection writers that
    /// wake after deletion can acquire the lifecycle lock, observe this fence,
    /// and fail closed instead of recreating `tasks/<id>`.
    pub fn task_deletion_marker_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.scope_root(principal, workspace)
            .join(".task_lifecycle")
            .join("deleted")
            .join(format!("{task_id}.deleted"))
    }

    pub fn task_multi_write_journal_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.task_dir(principal, workspace, task_id)
            .join(".artifact_v2.multi_write_journal.json")
    }

    pub fn task_refs_path(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_dir(principal, workspace, task_id)
            .join("task_refs.json")
    }

    pub fn task_state_path(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_state_dir(principal, workspace, task_id)
            .join("task_state.json")
    }

    /// Recurring Monitors Phase 3 — the per-task durable update ledger
    /// (`MonitorUpdateDetailV1` JSONL, newest last). Lives in the task's
    /// `state/` dir beside `task_state.json`; appended idempotently under
    /// the task write guard by `project_monitor_run_outcome`.
    pub fn monitor_updates_log_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.task_state_dir(principal, workspace, task_id)
            .join("monitor_updates.jsonl")
    }

    /// Recurring Monitors Phase 6 — the per-task durable feedback ledger
    /// (`MonitorUpdateFeedbackV1` JSONL, newest last). Lives beside
    /// `monitor_updates.jsonl`; appended idempotently under the task write
    /// guard by `record_monitor_update_feedback` with the same bounded
    /// retention (`MONITOR_LEDGER_RETENTION_CAP`).
    pub fn monitor_feedback_log_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.task_state_dir(principal, workspace, task_id)
            .join("monitor_feedback.jsonl")
    }

    pub fn task_plan_index_path(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_plans_dir(principal, workspace, task_id)
            .join("plans_index.json")
    }

    pub fn task_plan_record_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        plan_id: &str,
    ) -> PathBuf {
        self.task_plans_dir(principal, workspace, task_id)
            .join(format!("{plan_id}.json"))
    }

    pub fn task_plan_versions_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.task_plans_dir(principal, workspace, task_id)
            .join("plan_versions.json")
    }

    pub fn task_executions_index_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.task_indexes_dir(principal, workspace, task_id)
            .join("executions.jsonl")
    }

    pub fn task_progress_projection_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.task_indexes_dir(principal, workspace, task_id)
            .join("progress_projection.json")
    }

    pub fn task_attention_projection_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> PathBuf {
        self.task_indexes_dir(principal, workspace, task_id)
            .join("attention_projection.json")
    }

    pub fn executions_root(&self, principal: &str, workspace: &str, task_id: &str) -> PathBuf {
        self.task_dir(principal, workspace, task_id)
            .join("executions")
    }

    pub fn execution_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.executions_root(principal, workspace, task_id)
            .join(execution_id)
    }

    pub fn runtime_execution_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> PathBuf {
        match task_id {
            Some(task_id) => self.execution_dir(principal, workspace, task_id, execution_id),
            None => self.scoped_execution_dir(principal, workspace, execution_id),
        }
    }

    pub fn execution_outputs_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_dir(principal, workspace, task_id, execution_id)
            .join("outputs")
    }

    pub fn execution_artifacts_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_dir(principal, workspace, task_id, execution_id)
            .join("artifacts")
    }

    pub fn execution_artifacts_index_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_artifacts_dir(principal, workspace, task_id, execution_id)
            .join("persisted_artifacts.json")
    }

    pub fn execution_taskplan_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_dir(principal, workspace, task_id, execution_id)
            .join("taskplan")
    }

    pub fn execution_plans_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_taskplan_dir(principal, workspace, task_id, execution_id)
    }

    pub fn execution_pipeline_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> PathBuf {
        self.runtime_execution_dir(principal, workspace, task_id, execution_id)
            .join("pipeline")
    }

    pub fn execution_pipeline_store_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_pipeline_dir(principal, workspace, task_id, execution_id)
            .join("store.json")
    }

    pub fn runtime_execution_artifacts_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> PathBuf {
        self.runtime_execution_dir(principal, workspace, task_id, execution_id)
            .join("artifacts")
    }

    pub fn runtime_execution_downloads_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> PathBuf {
        self.runtime_execution_artifacts_dir(principal, workspace, task_id, execution_id)
            .join("downloads")
    }

    pub fn runtime_execution_session_downloads_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> PathBuf {
        self.runtime_execution_downloads_dir(principal, workspace, task_id, execution_id)
            .join("session")
    }

    pub fn runtime_execution_recordings_dir(
        &self,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> PathBuf {
        self.runtime_execution_dir(principal, workspace, task_id, execution_id)
            .join("recordings")
    }

    pub fn execution_state_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_dir(principal, workspace, task_id, execution_id)
            .join("state.json")
    }

    pub fn execution_refs_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_dir(principal, workspace, task_id, execution_id)
            .join("refs.json")
    }

    pub fn execution_schedule_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_dir(principal, workspace, task_id, execution_id)
            .join("schedule.json")
    }

    pub fn execution_events_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_dir(principal, workspace, task_id, execution_id)
            .join("events.jsonl")
    }

    /// Durable byte-length authority for `events.jsonl`.
    ///
    /// The append file may temporarily contain a newline-terminated record
    /// whose write/flush/fsync outcome was uncertain. Canonical readers expose
    /// only bytes through this independently atomically-published boundary.
    pub fn execution_events_commit_path(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        self.execution_dir(principal, workspace, task_id, execution_id)
            .join("events.jsonl.commit")
    }

    pub async fn ensure_task_workspace(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Result<(), ArtifactV2Error> {
        // Legacy entry point — defaults to user-visible (`tasks/<id>/`).
        // Use `ensure_task_workspace_for_lifecycle` when the caller has
        // the manifest in hand and may want the internal folder.
        self.ensure_task_workspace_for_lifecycle(
            principal,
            workspace,
            task_id,
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent,
        )
        .await
    }

    /// Lifecycle-aware version of `ensure_task_workspace`. Routes
    /// `Internal` tasks to `internal_tasks/<id>/`; `Persistent` lands
    /// in `tasks/<id>/`. The task dir is created FIRST so subsequent
    /// `task_dir()` probes resolve correctly when derived path helpers
    /// run.
    pub async fn ensure_task_workspace_for_lifecycle(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle,
    ) -> Result<(), ArtifactV2Error> {
        Self::validate_task_id(task_id)?;
        self.ensure_root().await?;
        let task_root = match lifecycle {
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal => {
                self.internal_tasks_root(principal, workspace)
            },
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent => {
                self.tasks_root(principal, workspace)
            },
        };
        self.create_dir_all_path(&task_root).await?;
        let task_dir = task_root.join(task_id);
        self.create_dir_all_path(&task_dir).await?;
        // Subdirs derived from task_dir's probe — now that the task_dir
        // exists, the probe in `task_dir()` finds it and the derived
        // helpers all route to the same place.
        self.create_dir_all_path(self.task_state_dir(principal, workspace, task_id))
            .await?;
        self.create_dir_all_path(self.task_outputs_dir(principal, workspace, task_id))
            .await?;
        // `plans/` intentionally NOT pre-created: the active runtime no longer
        // uses LLM-mutated taskplans, so most tasks never persist a plan
        // record. Leaving it uncreated keeps every fresh task's directory
        // tree clean. If a plan record IS persisted later, write_bytes_atomic
        // calls fs::create_dir_all on the parent before writing, so the
        // directory materializes lazily on first write.
        self.create_dir_all_path(self.task_indexes_dir(principal, workspace, task_id))
            .await?;
        self.create_dir_all_path(self.executions_root(principal, workspace, task_id))
            .await?;
        self.create_dir_all_path(self.published_surfaces_dir(principal, workspace))
            .await?;
        self.create_dir_all_path(self.ui_indexes_dir(principal, workspace))
            .await?;
        Ok(())
    }

    pub async fn ensure_chat_session_workspace(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.ensure_root().await?;
        self.create_dir_all_path(self.chat_sessions_dir(principal, workspace))
            .await?;
        self.create_dir_all_path(self.chat_session_dir(principal, workspace, session_id))
            .await?;
        self.create_dir_all_path(self.chat_session_outputs_dir(principal, workspace, session_id))
            .await?;
        Ok(())
    }

    pub async fn ensure_memory_agent_layout(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.ensure_root().await?;
        self.create_dir_all_path(self.memory_agent_episodes_dir(principal, workspace, agent_id))
            .await?;
        self.create_dir_all_path(self.memory_agent_tiers_dir(principal, workspace, agent_id))
            .await?;
        self.create_dir_all_path(
            self.memory_agent_consolidations_dir(principal, workspace, agent_id),
        )
        .await?;
        Ok(())
    }

    pub async fn ensure_pause_state_layout(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.ensure_root().await?;
        self.create_dir_all_path(self.pause_states_dir(principal, workspace))
            .await?;
        Ok(())
    }

    pub async fn ensure_execution_workspace(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.ensure_root().await?;
        if self
            .resolve_task_dir(principal, workspace, task_id)
            .is_none()
        {
            return Err(ArtifactV2Error::TaskNotFound(task_id.to_string()));
        }
        self.create_dir_all_path(self.execution_outputs_dir(
            principal,
            workspace,
            task_id,
            execution_id,
        ))
        .await?;
        self.create_dir_all_path(self.execution_artifacts_dir(
            principal,
            workspace,
            task_id,
            execution_id,
        ))
        .await?;
        self.create_dir_all_path(self.execution_taskplan_dir(
            principal,
            workspace,
            task_id,
            execution_id,
        ))
        .await?;
        Ok(())
    }

    pub async fn ensure_runtime_execution_workspace(
        &self,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> Result<(), ArtifactV2Error> {
        match task_id {
            Some(task_id) => {
                self.ensure_execution_workspace(principal, workspace, task_id, execution_id)
                    .await?;
            },
            None => {
                self.ensure_root().await?;
                self.create_dir_all_path(self.scoped_executions_root(principal, workspace))
                    .await?;
                self.create_dir_all_path(self.scoped_execution_dir(
                    principal,
                    workspace,
                    execution_id,
                ))
                .await?;
            },
        }
        self.create_dir_all_path(self.execution_pipeline_dir(
            principal,
            workspace,
            task_id,
            execution_id,
        ))
        .await?;
        self.create_dir_all_path(self.runtime_execution_downloads_dir(
            principal,
            workspace,
            task_id,
            execution_id,
        ))
        .await?;
        Ok(())
    }
}

const MAX_MULTI_WRITE_OPS: usize = 64;
const MAX_MULTI_WRITE_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
// This mainly bounds version-one compatibility recovery, whose JSON embedded
// payload bytes as integer arrays. Version-two journals are metadata-only and
// remain orders of magnitude smaller than this ceiling.
const MAX_MULTI_WRITE_JOURNAL_BYTES: u64 = 32 * 1024 * 1024;

/// Version-one journals embedded every destination payload in the JSON
/// document. They remain readable for crash recovery across upgrades, but new
/// commits use the bounded version-two staging format below.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WorkspacePersistedWriteOpV1 {
    path: PathBuf,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WorkspacePersistedWriteJournalV1 {
    version: u32,
    writes: Vec<WorkspacePersistedWriteOpV1>,
}

/// Version-two journals contain only bounded metadata. Each payload is staged
/// as its own file so recovery never deserializes or clones the complete write
/// set at once.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WorkspacePersistedWriteOpV2 {
    path: PathBuf,
    staged_path: PathBuf,
    size_bytes: u64,
    content_hash: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WorkspacePersistedWriteJournalV2 {
    version: u32,
    staging_dir: PathBuf,
    writes: Vec<WorkspacePersistedWriteOpV2>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
enum WorkspacePersistedWriteJournal {
    V2(WorkspacePersistedWriteJournalV2),
    V1(WorkspacePersistedWriteJournalV1),
}

fn multi_write_staging_dir(journal_path: &Path) -> PathBuf {
    let file_name = journal_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "multi-write-journal".to_string());
    journal_path.with_file_name(format!(".{file_name}.payloads"))
}

fn multi_write_content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn safe_segment(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "default".to_string();
    }

    let sanitized: String = trimmed
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .collect();

    if sanitized == "." || sanitized == ".." {
        return "default".to_string();
    }

    sanitized.replace("..", "__")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        io::{Cursor, Read},
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use crate::magician_v2::artifact_v2::{models::TaskLifecycle, service::ArtifactV2Error};

    use super::{
        buffered_hashing_json_reader, jsonl_record_is_admitted, multi_write_content_hash,
        multi_write_staging_dir, safe_segment, scope_hosts_user_subsystems, scope_profile,
        validate_workspace_json_reader, workspace_json_document_is_admitted, ArtifactV2Workspace,
        LocalFileWorkspaceProvider, ScopeProfile, SilverBulletSpaceWorkspaceProvider, TaskLocation,
        WorkspaceFileEntry, WorkspaceFileProvider, WorkspacePersistedWriteJournalV1,
        WorkspacePersistedWriteJournalV2, WorkspacePersistedWriteOpV1, WorkspacePersistedWriteOpV2,
        DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE, JSONL_TAIL_WINDOW_ERROR_PREFIX,
        MAX_JSONL_COMMIT_AUTHORITY_BYTES, MAX_JSONL_RECORD_NODES, MAX_RETAINED_JSON_DEPTH,
        MAX_WORKSPACE_JSON_VALIDATION_NODES,
    };

    #[cfg(unix)]
    #[test]
    fn existing_runtime_root_alias_is_pinned_to_its_physical_directory() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().expect("runtime-root parent");
        let physical_root = parent.path().join("physical-root");
        std::fs::create_dir(&physical_root).expect("physical runtime root");
        let alias = parent.path().join("runtime-root-alias");
        symlink(&physical_root, &alias).expect("runtime-root alias");

        let pinned = super::pin_existing_runtime_root(alias);
        assert_eq!(
            pinned,
            std::fs::canonicalize(&physical_root).expect("canonical physical runtime root")
        );
        assert!(
            !std::fs::symlink_metadata(&pinned)
                .expect("pinned runtime root metadata")
                .file_type()
                .is_symlink(),
            "the trusted runtime-root resolver must capture a physical directory"
        );
    }

    #[test]
    fn missing_runtime_root_remains_available_for_normal_creation() {
        let parent = tempfile::tempdir().expect("runtime-root parent");
        let missing = parent.path().join("not-created-yet");
        assert_eq!(super::pin_existing_runtime_root(missing.clone()), missing);
    }

    fn flat_json_array_record(nodes: usize, newline: bool) -> Vec<u8> {
        assert!(nodes > 0, "array root consumes one node");
        let mut body = Vec::with_capacity(nodes.saturating_mul(5).saturating_add(2));
        body.push(b'[');
        for index in 1..nodes {
            if index > 1 {
                body.push(b',');
            }
            body.extend_from_slice(b"null");
        }
        body.push(b']');
        if newline {
            body.push(b'\n');
        }
        body
    }

    struct CountingReader {
        inner: Cursor<Vec<u8>>,
        reads: Arc<AtomicUsize>,
    }

    impl Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.inner.read(buffer)
        }
    }

    #[test]
    fn verified_decode_buffers_large_sources_before_hashing_and_admission() {
        let encoded =
            serde_json::to_vec(&"x".repeat(1024 * 1024)).expect("large JSON string fixture");
        let reads = Arc::new(AtomicUsize::new(0));
        let source = CountingReader {
            inner: Cursor::new(encoded.clone()),
            reads: Arc::clone(&reads),
        };
        let mut reader = buffered_hashing_json_reader(
            source,
            encoded.len() as u64,
            MAX_RETAINED_JSON_DEPTH,
            MAX_WORKSPACE_JSON_VALIDATION_NODES,
        );
        let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
        let restored = <String as serde::Deserialize>::deserialize(&mut deserializer)
            .expect("buffered JSON decode");
        deserializer.end().expect("complete JSON document");
        drop(deserializer);
        let admitted = reader.into_inner();

        assert_eq!(restored.len(), 1024 * 1024);
        assert!(admitted.admission.finish());
        assert_eq!(admitted.bytes, encoded.len() as u64);
        assert!(
            reads.load(Ordering::Relaxed) < 64,
            "one-megabyte decode must use chunked source reads"
        );
    }

    #[derive(Debug)]
    struct PanicOnDeserialize;

    impl<'de> serde::Deserialize<'de> for PanicOnDeserialize {
        fn deserialize<D: serde::Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
            panic!("typed Serde must not run after raw admission rejection")
        }
    }

    static BOUNDED_JSON_REPLACEMENT_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
    static BOUNDED_JSON_DEEP_REPLACEMENT: Mutex<Option<(PathBuf, Vec<u8>)>> = Mutex::new(None);

    #[derive(Debug)]
    struct ReplaceSourceOnDeserialize;

    impl<'de> serde::Deserialize<'de> for ReplaceSourceOnDeserialize {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let replacement_path = BOUNDED_JSON_REPLACEMENT_PATH
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("replacement path configured");
            std::fs::write(&replacement_path, br#"{"value":"replacement"}"#)
                .expect("replace bounded JSON source between passes");
            let _: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
            Ok(Self)
        }
    }

    #[derive(Debug)]
    struct ReplaceSourceWithDeepJsonOnDeserialize;

    impl<'de> serde::Deserialize<'de> for ReplaceSourceWithDeepJsonOnDeserialize {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let (replacement_path, replacement) = BOUNDED_JSON_DEEP_REPLACEMENT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("deep replacement configured");
            std::fs::write(&replacement_path, replacement)
                .expect("replace source with same-size deep JSON between passes");
            let _: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
            Ok(Self)
        }
    }

    #[test]
    fn jsonl_record_node_admission_is_exact_and_ignores_string_tokens() {
        let exact = flat_json_array_record(MAX_JSONL_RECORD_NODES, false);
        assert!(jsonl_record_is_admitted(&exact));

        let one_over = flat_json_array_record(MAX_JSONL_RECORD_NODES + 1, false);
        assert!(!jsonl_record_is_admitted(&one_over));

        let string_tokens = format!(r#"{{"payload":"{}"}}"#, "[null]".repeat(100_000));
        assert!(jsonl_record_is_admitted(string_tokens.as_bytes()));
    }

    #[test]
    fn workspace_json_document_admission_is_exact_and_rejects_deep_or_wide() {
        let exact = flat_json_array_record(MAX_WORKSPACE_JSON_VALIDATION_NODES, false);
        assert!(workspace_json_document_is_admitted(&exact));

        let one_over = flat_json_array_record(MAX_WORKSPACE_JSON_VALIDATION_NODES + 1, false);
        assert!(!workspace_json_document_is_admitted(&one_over));

        let depth = MAX_RETAINED_JSON_DEPTH + 1;
        let mut deep = vec![b'['; depth];
        deep.extend_from_slice(b"null");
        deep.extend(std::iter::repeat(b']').take(depth));
        assert!(!workspace_json_document_is_admitted(&deep));
    }

    #[test]
    fn workspace_json_stream_admission_is_exact_before_syntax_validation() {
        let exact = br#"{"rows":[null,false]}"#;
        validate_workspace_json_reader(std::io::Cursor::new(exact), 2, 4)
            .expect("root, array, and two scalar nodes fit exactly");
        let over = validate_workspace_json_reader(std::io::Cursor::new(exact), 2, 3)
            .expect_err("one additional node must fail streaming admission");
        assert!(matches!(over, ArtifactV2Error::InvalidRequest(_)));

        let malformed = br#"{"rows":}"#;
        let error = validate_workspace_json_reader(std::io::Cursor::new(malformed), 2, 4)
            .expect_err("admitted structure still uses Serde as syntax authority");
        assert!(!matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[test]
    fn workspace_json_stream_admission_ignores_wide_string_structure_and_rejects_depth() {
        let wide_string = format!(r#"{{"value":"{}"}}"#, "[null]".repeat(100_000));
        validate_workspace_json_reader(std::io::Cursor::new(wide_string), 1, 2)
            .expect("one wide string is one node");

        let deep = br#"[[[null]]]"#;
        let error = validate_workspace_json_reader(std::io::Cursor::new(deep), 2, 4)
            .expect_err("depth over the stream ceiling fails before Serde");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn verified_json_value_roundtrip_streams_canonical_bytes_and_enforces_raw_nodes() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let path = temp.path().join("verified-value.json");
        let value = serde_json::json!({
            "rows": [null, false],
            "unicode": "नमस्ते-🧭-مرحبا-".repeat(8_000),
        });
        let canonical = crate::magician_v2::json_traversal::canonical_json_bytes(&value)
            .expect("canonical fixture");
        let digest = format!("blake3:{}", blake3::hash(&canonical).to_hex());
        workspace
            .write_canonical_json_value_atomic_stream_path(&path, value.clone())
            .await
            .expect("streamed write");

        let restored = workspace
            .read_verified_json_value_path(
                &path,
                canonical.len() as u64,
                &digest,
                canonical.len() as u64,
                MAX_RETAINED_JSON_DEPTH,
                5,
            )
            .await
            .expect("verified streamed read");
        assert_eq!(restored, value);

        let error = workspace
            .read_verified_json_value_path(
                &path,
                canonical.len() as u64,
                &digest,
                canonical.len() as u64,
                MAX_RETAINED_JSON_DEPTH,
                4,
            )
            .await
            .expect_err("one fewer node must reject before Serde");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn bounded_typed_json_reader_rejects_before_deserialize_and_writer_cleans_staging() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let rejected_path = temp.path().join("raw-rejected.json");
        workspace
            .write_atomic_path(&rejected_path, br#"[null]"#)
            .await
            .expect("raw fixture");

        let error = workspace
            .read_json_bounded_stream_path::<PanicOnDeserialize, _>(
                &rejected_path,
                64,
                MAX_RETAINED_JSON_DEPTH,
                1,
            )
            .await
            .expect_err("array child exceeds the one-node ceiling");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));

        let oversized_path = temp.path().join("bounded-write.json");
        let error = workspace
            .write_json_value_atomic_stream_path(
                &oversized_path,
                serde_json::json!({"body": "x".repeat(1_024)}),
                32,
            )
            .await
            .expect_err("streaming writer must stop at its byte ceiling");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
        assert!(!oversized_path.exists());
        let leaked_staging = std::fs::read_dir(temp.path())
            .expect("workspace listing")
            .flatten()
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("artifact-json-write")
            });
        assert!(!leaked_staging);
    }

    #[tokio::test]
    async fn bounded_typed_json_reader_rejects_a_source_changed_between_admission_and_decode() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let path = temp.path().join("replace-between-passes.json");
        workspace
            .write_atomic_path(&path, br#"{"value":"original"}"#)
            .await
            .expect("original fixture");
        *BOUNDED_JSON_REPLACEMENT_PATH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path.clone());

        let error = workspace
            .read_json_bounded_stream_path::<ReplaceSourceOnDeserialize, _>(
                &path,
                1_024,
                MAX_RETAINED_JSON_DEPTH,
                8,
            )
            .await
            .expect_err("a changed source must not pass the admitted hash authority");
        assert!(matches!(
            error,
            ArtifactV2Error::InvalidRequest(_) | ArtifactV2Error::Serde(_)
        ));
    }

    #[tokio::test]
    async fn bounded_typed_json_reader_reapplies_depth_admission_during_decode() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let path = temp.path().join("deep-replacement-between-passes.json");
        let replacement = format!(
            "{}null{}",
            "[".repeat(MAX_RETAINED_JSON_DEPTH + 1),
            "]".repeat(MAX_RETAINED_JSON_DEPTH + 1),
        )
        .into_bytes();
        let original = serde_json::to_vec(&"x".repeat(replacement.len().saturating_sub(2)))
            .expect("same-size shallow JSON");
        assert_eq!(original.len(), replacement.len());
        workspace
            .write_atomic_path(&path, &original)
            .await
            .expect("original shallow fixture");
        *BOUNDED_JSON_DEEP_REPLACEMENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((path.clone(), replacement));

        let error = workspace
            .read_json_bounded_stream_path::<ReplaceSourceWithDeepJsonOnDeserialize, _>(
                &path,
                original.len() as u64,
                MAX_RETAINED_JSON_DEPTH,
                MAX_WORKSPACE_JSON_VALIDATION_NODES,
            )
            .await
            .expect_err("second-pass structural admission must reject the replacement");
        assert!(matches!(error, ArtifactV2Error::Serde(_)));
    }

    #[test]
    fn analytics_llm_embeddings_root_is_under_scope_analytics() {
        let ws = ArtifactV2Workspace::new(Path::new("/tmp/mb-embeddings-root-test"));
        let root = ws.analytics_llm_embeddings_root("owner", "default");
        assert!(
            root.ends_with("analytics/llm_embeddings"),
            "embeddings root should live under analytics/llm_embeddings, got {}",
            root.display()
        );
        // It must be a sibling of llm_calls (its own dataset, not nested inside).
        assert_eq!(
            root.parent(),
            ws.analytics_llm_calls_root("owner", "default").parent(),
            "llm_embeddings and llm_calls should share the analytics parent"
        );
    }

    #[test]
    fn safe_segment_normalizes_empty_and_dot_segments() {
        assert_eq!(safe_segment(""), "default");
        assert_eq!(safe_segment("   "), "default");
        assert_eq!(safe_segment("."), "default");
        assert_eq!(safe_segment(".."), "default");
    }

    #[test]
    fn safe_segment_blocks_embedded_parent_traversal() {
        assert_eq!(safe_segment("../secrets"), "___secrets");
        assert_eq!(safe_segment("safe..name"), "safe__name");
        assert_eq!(safe_segment("a/../b"), "a____b");
    }

    #[test]
    fn dormant_app_paths_are_scope_bound_canonical_and_side_effect_free() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let ws = ArtifactV2Workspace::new(temporary.path());
        let apps = temporary.path().join("scopes/user_1/work_space/apps");

        assert_eq!(ws.apps_root("user:1", "work/space"), apps);
        assert_eq!(
            ws.app_store_db_path("user:1", "work/space"),
            apps.join("app_store.sqlite3")
        );
        assert_eq!(
            ws.app_packages_root("user:1", "work/space"),
            apps.join("packages")
        );
        assert_eq!(
            ws.app_attachments_root("user:1", "work/space"),
            apps.join("attachments")
        );
        assert_eq!(
            ws.app_exports_root("user:1", "work/space"),
            apps.join("exports")
        );
        assert_eq!(
            ws.app_captures_root("user:1", "work/space"),
            apps.join("captures")
        );
        assert_eq!(
            ws.app_evaluations_root("user:1", "work/space"),
            apps.join("evaluations")
        );
        assert_ne!(
            ws.apps_root("user:1", "other"),
            ws.apps_root("user:1", "work/space")
        );
        assert!(
            !apps.exists(),
            "path discovery must not materialize dormant app storage"
        );
    }

    #[test]
    fn scope_dir_segments_are_the_names_the_scope_root_actually_uses() {
        // The list index stores the directory names a rebuild read off disk.
        // If this drifts from `scope_root`, an index query built from a raw
        // ScopeRef matches nothing and the reader is served an empty list
        // that looks exactly like a scope with no tasks.
        let ws = ArtifactV2Workspace::new(std::path::Path::new("/tmp/scope-segments"));
        for (principal, workspace) in [
            ("owner", "default"),
            ("user:1", "work/space"),
            ("  ", "."),
            ("../secrets", "safe..name"),
        ] {
            let (indexed_principal, indexed_workspace) =
                ArtifactV2Workspace::scope_dir_segments(principal, workspace);
            let root = ws.scope_root(principal, workspace);
            let mut components = root.components().rev();
            let on_disk_workspace = components.next().expect("workspace segment");
            let on_disk_principal = components.next().expect("principal segment");
            assert_eq!(
                on_disk_workspace.as_os_str(),
                indexed_workspace.as_str(),
                "workspace segment for {principal}/{workspace}"
            );
            assert_eq!(
                on_disk_principal.as_os_str(),
                indexed_principal.as_str(),
                "principal segment for {principal}/{workspace}"
            );
        }
        // And it really does normalise — a pass-through would make the
        // assertions above true for the wrong reason.
        assert_eq!(
            ArtifactV2Workspace::scope_dir_segments("user:1", "work/space"),
            ("user_1".to_string(), "work_space".to_string())
        );
    }

    #[test]
    fn task_id_validation_requires_one_canonical_segment() {
        for invalid in [
            "",
            " ",
            ".",
            "..",
            "../task",
            "task/child",
            "task\\child",
            "/absolute",
            "C:\\absolute",
            " task",
            "task ",
            "task\nchild",
        ] {
            assert!(
                ArtifactV2Workspace::validate_task_id(invalid).is_err(),
                "accepted hostile task id {invalid:?}"
            );
        }
        assert!(ArtifactV2Workspace::validate_task_id("task_abc-123").is_ok());
    }

    #[tokio::test]
    async fn hostile_task_id_cannot_create_outside_exact_scope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let error = workspace
            .ensure_task_workspace("anonymous", "default", "../../escaped")
            .await
            .expect_err("path escape must be rejected");
        assert!(
            matches!(error, ArtifactV2Error::InvalidRequest(ref code) if code == "invalid_task_id")
        );
        assert!(!temp.path().join("escaped").exists());
    }

    #[test]
    fn programs_anomalies_path_is_under_scope_programs_anomalies() {
        let ws = ArtifactV2Workspace::new(std::path::Path::new("/tmp/x"));
        let p = ws.programs_anomalies_path("anonymous", "default", "abc123.json");
        assert!(p.ends_with("programs/anomalies/abc123.json"), "got {p:?}");
    }

    #[test]
    fn programs_backlog_path_is_under_scope_programs_backlog() {
        let ws = ArtifactV2Workspace::new(std::path::Path::new("/tmp/x"));
        let p = ws.programs_backlog_path("anonymous", "default", "bk_abc123.json");
        assert!(p.ends_with("programs/backlog/bk_abc123.json"), "got {p:?}");
    }

    #[test]
    fn evals_run_path_is_under_scope_evals_runs_lane() {
        let ws = ArtifactV2Workspace::new(std::path::Path::new("/tmp/x"));
        let p = ws.evals_run_path("anonymous", "default", "eval-monitor-golden", "evr_1.json");
        assert!(
            p.ends_with("evals/runs/eval-monitor-golden/evr_1.json"),
            "got {p:?}"
        );
    }

    /// Lane ids come from Makefile target names, so a lane id must never be able
    /// to steer a run record out of its own scope.
    #[test]
    fn evals_run_path_cannot_escape_the_scope_via_the_lane_id() {
        let ws = ArtifactV2Workspace::new(std::path::Path::new("/tmp/x"));
        let p = ws.evals_run_path("anonymous", "default", "../../escaped", "evr_1.json");
        let rendered = p.to_string_lossy();
        assert!(!rendered.contains(".."), "got {p:?}");
        assert!(
            rendered.contains("scopes/anonymous/default/evals/runs/"),
            "got {p:?}"
        );
    }

    #[test]
    fn resolve_scoped_root_is_the_flat_runtime_root() {
        // Post seed/runtime split, the scoped root IS the runtime root: no
        // `magician_data_v3` nesting, and any legacy nested child is NOT
        // preferred over the flat top-level scopes/system layout.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("scopes")).expect("scopes");
        std::fs::create_dir_all(tmp.path().join("system")).expect("system");
        std::fs::create_dir_all(tmp.path().join("magician_data_v3").join("scopes"))
            .expect("legacy nested child");

        assert_eq!(
            ArtifactV2Workspace::resolve_scoped_root(tmp.path()),
            tmp.path()
        );
    }

    #[test]
    fn local_file_provider_preserves_existing_workspace_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let principal = "anonymous";
        let workspace_name = "default";
        let task_id = "task_layout";
        let session_id = "session_layout";
        let execution_id = "exec_layout";

        assert_eq!(workspace.file_provider_id(), "local_file");
        assert_eq!(workspace.base_root(), tmp.path());
        assert_eq!(
            workspace.tasks_root(principal, workspace_name),
            tmp.path().join("scopes/anonymous/default/tasks")
        );
        assert_eq!(
            workspace.task_outputs_dir(principal, workspace_name, task_id),
            tmp.path()
                .join("scopes/anonymous/default/tasks/task_layout/outputs")
        );
        assert_eq!(
            workspace.internal_task_outputs_dir(principal, workspace_name, task_id),
            tmp.path()
                .join("scopes/anonymous/default/internal_tasks/task_layout/outputs")
        );
        assert_eq!(
            workspace.chat_session_outputs_dir(principal, workspace_name, session_id),
            tmp.path()
                .join("scopes/anonymous/default/ui/chat_sessions/session_layout/outputs")
        );
        assert_eq!(
            workspace.scoped_execution_dir(principal, workspace_name, execution_id),
            tmp.path()
                .join("scopes/anonymous/default/executions/exec_layout")
        );
    }

    #[tokio::test]
    async fn external_artifact_copy_streams_and_atomically_replaces_destination() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root = tmp.path().join("workspace");
        let workspace = ArtifactV2Workspace::new(&workspace_root);
        let source = tmp.path().join("large-source.bin");
        let bytes = vec![0xA5; 5 * 1024 * 1024];
        tokio::fs::write(&source, &bytes)
            .await
            .expect("source write");
        let destination = workspace_root.join("outputs/copied.bin");
        workspace
            .write_path(&destination, b"previous")
            .await
            .expect("seed destination");

        let copied = workspace
            .copy_external_atomic_path(&source, &destination)
            .await
            .expect("streaming copy");

        assert_eq!(copied, bytes.len() as u64);
        assert_eq!(
            tokio::fs::read(&destination)
                .await
                .expect("copied destination"),
            bytes
        );
        let temporary_files = std::fs::read_dir(destination.parent().expect("destination parent"))
            .expect("destination directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(".artifact-copy-") && name.ends_with(".tmp")
            })
            .count();
        assert_eq!(temporary_files, 0);
    }

    #[test]
    fn workspace_paths_resolve_through_injected_file_provider() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let provider_root = tmp.path().join("provider-root");
        let workspace = ArtifactV2Workspace::with_file_provider(Arc::new(
            LocalFileWorkspaceProvider::new(&provider_root),
        ));

        assert_eq!(workspace.file_provider_id(), "local_file");
        assert_eq!(workspace.base_root(), provider_root.as_path());
        assert_eq!(
            workspace.task_outputs_dir("anonymous", "default", "task_x"),
            provider_root.join("scopes/anonymous/default/tasks/task_x/outputs")
        );
        assert_eq!(
            workspace.chat_session_outputs_dir("anonymous", "default", "session_x"),
            provider_root.join("scopes/anonymous/default/ui/chat_sessions/session_x/outputs")
        );
    }

    #[test]
    fn silverbullet_space_provider_roots_directly_at_space_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let space_root = tmp.path().join("MagicianNotes");
        // The SB provider no longer re-roots into `.magician/runtime`; the store
        // lives directly at `space_root` (identical to local_file). Alias
        // `runtime_root` to `space_root` so the path assertions below read naturally.
        let runtime_root = space_root.clone();
        let workspace = ArtifactV2Workspace::with_silverbullet_space_provider(&space_root)
            .expect("silverbullet provider");

        assert_eq!(workspace.file_provider_id(), "silverbullet_space");
        assert_eq!(workspace.base_root(), runtime_root.as_path());
        assert_eq!(workspace.visible_root(), space_root.as_path());
        assert_eq!(
            workspace.tasks_root("anonymous", "default"),
            runtime_root.join("scopes/anonymous/default/tasks")
        );
        // Program specs are now SCOPED (same root as their runtime state),
        // not the old flat visible-root `Programs/`.
        assert_eq!(
            workspace.program_specs_root("anonymous", "default"),
            runtime_root.join("scopes/anonymous/default/programs")
        );
        assert_eq!(
            workspace.program_runtime_states_dir("anonymous", "default"),
            runtime_root.join("scopes/anonymous/default/programs/state")
        );
        assert_eq!(
            workspace.chat_session_outputs_dir("anonymous", "default", "session_x"),
            runtime_root.join("scopes/anonymous/default/ui/chat_sessions/session_x/outputs")
        );
        assert_eq!(workspace.system_root(), runtime_root.join("system"));
    }

    #[test]
    fn silverbullet_space_provider_validates_but_ignores_custom_runtime_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let space_root = tmp.path().join("MagicianNotes");
        // A valid `runtime_root` is still accepted (validated) for settings
        // back-compat, but it no longer relocates the store — the provider roots
        // directly at `space_root`.
        let workspace = ArtifactV2Workspace::with_silverbullet_space_provider_and_runtime_root(
            &space_root,
            "Runtime/state",
        )
        .expect("silverbullet provider");

        assert_eq!(workspace.base_root(), space_root.as_path());
        assert_eq!(
            workspace.scoped_execution_dir("p", "w", "exec_1"),
            space_root.join("scopes/p/w/executions/exec_1")
        );
    }

    #[test]
    fn silverbullet_space_provider_rejects_runtime_root_escapes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let space_root = tmp.path().join("MagicianNotes");

        assert!(
            SilverBulletSpaceWorkspaceProvider::with_runtime_root(&space_root, "/tmp").is_err()
        );
        assert!(
            SilverBulletSpaceWorkspaceProvider::with_runtime_root(&space_root, "../runtime")
                .is_err()
        );
    }

    #[derive(Debug)]
    struct PrefixingTestProvider {
        root: PathBuf,
    }

    impl WorkspaceFileProvider for PrefixingTestProvider {
        fn id(&self) -> &'static str {
            "prefixing_test"
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn resolve_path(&self, relative_path: &Path) -> PathBuf {
            self.root.join("routed").join(relative_path)
        }
    }

    #[derive(Debug, Default)]
    struct RecordingProviderCalls {
        create_dir_all: Mutex<Vec<PathBuf>>,
        create_dir_all_sync: Mutex<Vec<PathBuf>>,
        metadata_sync: Mutex<Vec<PathBuf>>,
        read_dir: Mutex<Vec<PathBuf>>,
        read_dir_sync: Mutex<Vec<PathBuf>>,
    }

    impl RecordingProviderCalls {
        fn created_dirs(&self) -> Vec<PathBuf> {
            self.create_dir_all.lock().expect("created dirs").clone()
        }

        fn sync_created_dirs(&self) -> Vec<PathBuf> {
            self.create_dir_all_sync
                .lock()
                .expect("sync created dirs")
                .clone()
        }

        fn metadata_sync_paths(&self) -> Vec<PathBuf> {
            self.metadata_sync
                .lock()
                .expect("metadata sync paths")
                .clone()
        }

        fn read_dir_paths(&self) -> Vec<PathBuf> {
            self.read_dir.lock().expect("read dir paths").clone()
        }

        fn read_dir_sync_paths(&self) -> Vec<PathBuf> {
            self.read_dir_sync
                .lock()
                .expect("read dir sync paths")
                .clone()
        }
    }

    #[derive(Debug)]
    struct RecordingTestProvider {
        root: PathBuf,
        calls: Arc<RecordingProviderCalls>,
    }

    impl RecordingTestProvider {
        fn new(root: PathBuf) -> (Self, Arc<RecordingProviderCalls>) {
            let calls = Arc::new(RecordingProviderCalls::default());
            (
                Self {
                    root,
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    #[async_trait::async_trait]
    impl WorkspaceFileProvider for RecordingTestProvider {
        fn id(&self) -> &'static str {
            "recording_test"
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn resolve_path(&self, relative_path: &Path) -> PathBuf {
            self.root.join("routed").join(relative_path)
        }

        async fn create_dir_all(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
            self.calls
                .create_dir_all
                .lock()
                .expect("create_dir_all calls")
                .push(relative_path.to_path_buf());
            tokio::fs::create_dir_all(self.resolve_path(relative_path)).await?;
            Ok(())
        }

        fn create_dir_all_sync(&self, relative_path: &Path) -> Result<(), ArtifactV2Error> {
            self.calls
                .create_dir_all_sync
                .lock()
                .expect("create_dir_all_sync calls")
                .push(relative_path.to_path_buf());
            std::fs::create_dir_all(self.resolve_path(relative_path))?;
            Ok(())
        }

        fn metadata_sync(
            &self,
            relative_path: &Path,
        ) -> Result<Option<std::fs::Metadata>, ArtifactV2Error> {
            self.calls
                .metadata_sync
                .lock()
                .expect("metadata_sync calls")
                .push(relative_path.to_path_buf());
            match std::fs::metadata(self.resolve_path(relative_path)) {
                Ok(metadata) => Ok(Some(metadata)),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(ArtifactV2Error::Io(err)),
            }
        }

        async fn read_dir(
            &self,
            relative_path: &Path,
        ) -> Result<Vec<WorkspaceFileEntry>, ArtifactV2Error> {
            self.calls
                .read_dir
                .lock()
                .expect("read_dir calls")
                .push(relative_path.to_path_buf());
            let mut entries = tokio::fs::read_dir(self.resolve_path(relative_path)).await?;
            let mut files = Vec::new();
            while let Some(entry) = entries.next_entry().await? {
                let file_type = entry.file_type().await?;
                let file_name = entry.file_name().to_string_lossy().into_owned();
                files.push(WorkspaceFileEntry {
                    relative_path: relative_path.join(&file_name),
                    file_name,
                    is_dir: file_type.is_dir(),
                    is_file: file_type.is_file(),
                });
            }
            files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
            Ok(files)
        }

        fn read_dir_sync(
            &self,
            relative_path: &Path,
        ) -> Result<Vec<WorkspaceFileEntry>, ArtifactV2Error> {
            self.calls
                .read_dir_sync
                .lock()
                .expect("read_dir_sync calls")
                .push(relative_path.to_path_buf());
            let mut files = Vec::new();
            for entry in std::fs::read_dir(self.resolve_path(relative_path))? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                let file_name = entry.file_name().to_string_lossy().into_owned();
                files.push(WorkspaceFileEntry {
                    relative_path: relative_path.join(&file_name),
                    file_name,
                    is_dir: file_type.is_dir(),
                    is_file: file_type.is_file(),
                });
            }
            files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
            Ok(files)
        }
    }

    #[test]
    fn workspace_helpers_do_not_bypass_provider_resolution() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::with_file_provider(Arc::new(PrefixingTestProvider {
            root: tmp.path().to_path_buf(),
        }));

        assert_eq!(workspace.file_provider_id(), "prefixing_test");
        assert_eq!(workspace.scopes_root(), tmp.path().join("routed/scopes"));
        assert_eq!(workspace.system_root(), tmp.path().join("routed/system"));
        assert_eq!(
            workspace.task_outputs_dir("anonymous", "default", "task_x"),
            tmp.path()
                .join("routed/scopes/anonymous/default/tasks/task_x/outputs")
        );
        assert_eq!(
            workspace.chat_session_outputs_dir("anonymous", "default", "session_x"),
            tmp.path()
                .join("routed/scopes/anonymous/default/ui/chat_sessions/session_x/outputs")
        );
    }

    #[tokio::test]
    async fn workspace_materialization_calls_provider_file_ops() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (provider, calls) = RecordingTestProvider::new(tmp.path().to_path_buf());
        let workspace = ArtifactV2Workspace::with_file_provider(Arc::new(provider));
        let principal = "anonymous";
        let workspace_name = "default";

        workspace
            .ensure_task_workspace_for_lifecycle(
                principal,
                workspace_name,
                "task_internal",
                TaskLifecycle::Internal,
            )
            .await
            .expect("internal task workspace");
        workspace
            .ensure_chat_session_workspace(principal, workspace_name, "session_1")
            .await
            .expect("chat session workspace");
        workspace
            .ensure_runtime_execution_workspace(principal, workspace_name, None, "exec_scoped")
            .await
            .expect("scoped execution workspace");
        workspace
            .ensure_root_sync()
            .expect("sync root creation through provider");
        assert_eq!(
            workspace
                .list_scope_segments()
                .await
                .expect("async scope listing through provider"),
            vec![(principal.to_string(), workspace_name.to_string())]
        );
        assert_eq!(
            workspace
                .list_scope_segments_sync()
                .expect("sync scope listing through provider"),
            vec![(principal.to_string(), workspace_name.to_string())]
        );

        let created_dirs = calls.created_dirs();
        let sync_created_dirs = calls.sync_created_dirs();
        let read_dir_paths = calls.read_dir_paths();
        let read_dir_sync_paths = calls.read_dir_sync_paths();
        assert!(created_dirs.contains(&PathBuf::from("scopes")));
        assert!(created_dirs.contains(&PathBuf::from("system")));
        assert!(sync_created_dirs.contains(&PathBuf::from("scopes")));
        assert!(sync_created_dirs.contains(&PathBuf::from("system")));
        assert!(read_dir_paths.contains(&PathBuf::from("scopes")));
        assert!(read_dir_paths.contains(&PathBuf::from("scopes/anonymous")));
        assert!(read_dir_sync_paths.contains(&PathBuf::from("scopes")));
        assert!(read_dir_sync_paths.contains(&PathBuf::from("scopes/anonymous")));
        assert!(created_dirs.contains(&PathBuf::from("scopes/anonymous/default/internal_tasks")));
        assert!(created_dirs.contains(&PathBuf::from(
            "scopes/anonymous/default/internal_tasks/task_internal"
        )));
        assert!(created_dirs.contains(&PathBuf::from(
            "scopes/anonymous/default/ui/chat_sessions/session_1/outputs"
        )));
        assert!(created_dirs.contains(&PathBuf::from(
            "scopes/anonymous/default/executions/exec_scoped"
        )));
        assert!(
            !tmp.path().join("scopes").exists(),
            "provider operations must not write to the raw provider root"
        );
        assert!(tmp.path().join("routed/scopes").exists());
    }

    #[tokio::test]
    async fn workspace_io_wrappers_validate_and_delegate_to_provider() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::with_local_file_provider(tmp.path());
        let file_path = workspace
            .default_scope_root()
            .join("provider-ops")
            .join("note.txt");

        workspace
            .write_atomic_path(&file_path, b"hello")
            .await
            .expect("atomic write through provider");
        assert_eq!(
            workspace
                .read_to_string_path(&file_path)
                .await
                .expect("read string through provider"),
            "hello"
        );
        assert_eq!(
            workspace
                .read_path(&file_path)
                .await
                .expect("read bytes through provider"),
            b"hello"
        );
        assert_eq!(
            workspace
                .read_prefix_path(&file_path, 2)
                .await
                .expect("read prefix through provider"),
            b"he"
        );
        let sync_path = workspace
            .default_scope_root()
            .join("provider-ops")
            .join("sync.txt");
        workspace
            .write_path_sync(&sync_path, b"sync")
            .expect("sync write through provider");
        workspace
            .append_path_sync(&sync_path, b"-append")
            .expect("sync append through provider");
        assert_eq!(
            workspace
                .read_to_string_path_sync(&sync_path)
                .expect("sync read string through provider"),
            "sync-append"
        );
        assert_eq!(
            workspace
                .read_path_sync(&sync_path)
                .expect("sync read bytes through provider"),
            b"sync-append"
        );
        workspace
            .write_json_atomic_path_sync(
                workspace
                    .default_scope_root()
                    .join("provider-ops")
                    .join("sync.json"),
                &serde_json::json!({"ok": true}),
            )
            .expect("sync JSON write through provider");
        let sync_json: serde_json::Value = workspace
            .read_json_path_sync(
                workspace
                    .default_scope_root()
                    .join("provider-ops")
                    .join("sync.json"),
            )
            .expect("sync JSON read through provider");
        assert_eq!(sync_json["ok"], serde_json::Value::Bool(true));
        assert!(workspace
            .metadata_path(&file_path)
            .await
            .expect("metadata through provider")
            .is_some());
        let canonical_file_path = workspace
            .canonicalize_path(&file_path)
            .await
            .expect("canonicalize through provider");
        assert_eq!(
            canonical_file_path,
            file_path
                .canonicalize()
                .expect("local file canonicalize baseline")
        );
        assert_eq!(
            workspace
                .read_path(&canonical_file_path)
                .await
                .expect("read canonical path through provider"),
            b"hello"
        );
        assert!(workspace
            .metadata_path(&canonical_file_path)
            .await
            .expect("metadata canonical path through provider")
            .is_some());

        let entries = workspace
            .read_dir_path(file_path.parent().expect("file parent"))
            .await
            .expect("read dir through provider");
        assert!(entries.iter().any(|entry| entry.file_name == "note.txt"));

        workspace
            .remove_file_path(&file_path)
            .await
            .expect("remove file through provider");
        assert!(!file_path.exists());
        workspace
            .remove_file_path_sync(&sync_path)
            .expect("sync remove file through provider");
        assert!(!sync_path.exists());

        let provider_ops_dir = workspace.default_scope_root().join("provider-ops");
        workspace
            .remove_dir_all_path(&provider_ops_dir)
            .await
            .expect("remove dir through provider");
        assert!(!provider_ops_dir.exists());

        let outside = tmp
            .path()
            .parent()
            .unwrap_or(tmp.path())
            .join("outside.txt");
        let err = workspace
            .write_path(&outside, b"outside")
            .await
            .expect_err("outside path should be rejected before provider write");
        assert!(matches!(err, ArtifactV2Error::InvalidRequest(_)));
        let err = workspace
            .read_prefix_path(&outside, 4)
            .await
            .expect_err("outside prefix read should be rejected before provider read");
        assert!(matches!(err, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn canonical_provider_path_round_trips_when_root_did_not_exist_at_construction() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("not-created-yet").join("magician_data_v3");
        let workspace = ArtifactV2Workspace::with_local_file_provider(&root);
        let file_path = workspace
            .default_scope_root()
            .join("canonical-roundtrip.txt");

        workspace
            .write_atomic_path(&file_path, b"roundtrip")
            .await
            .expect("create provider root and file after workspace construction");
        let canonical_file = std::fs::canonicalize(&file_path).expect("canonical file path");
        assert_eq!(
            workspace
                .read_path(&canonical_file)
                .await
                .expect("canonical child stays inside the precomputed provider authority"),
            b"roundtrip"
        );
    }

    #[test]
    fn task_resolution_uses_provider_metadata_probe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (provider, calls) = RecordingTestProvider::new(tmp.path().to_path_buf());
        let workspace = ArtifactV2Workspace::with_file_provider(Arc::new(provider));
        let principal = "anonymous";
        let workspace_name = "default";
        let task_id = "task_internal";
        let internal = workspace.internal_task_dir(principal, workspace_name, task_id);

        std::fs::create_dir_all(&internal).expect("internal task dir");

        assert_eq!(
            workspace.resolve_task_location(principal, workspace_name, task_id),
            Some(TaskLocation::Internal)
        );
        assert_eq!(
            workspace.resolve_task_dir(principal, workspace_name, task_id),
            Some(internal.clone())
        );
        assert_eq!(
            workspace.task_dir(principal, workspace_name, task_id),
            internal
        );

        let metadata_paths = calls.metadata_sync_paths();
        assert!(metadata_paths.iter().any(|path| {
            path == &PathBuf::from("scopes/anonymous/default/internal_tasks/task_internal")
        }));
    }

    #[tokio::test]
    async fn local_file_provider_materializes_runtime_storage_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let principal = "anonymous";
        let workspace_name = "default";

        workspace
            .ensure_task_workspace_for_lifecycle(
                principal,
                workspace_name,
                "task_visible",
                TaskLifecycle::Persistent,
            )
            .await
            .expect("persistent task workspace");
        workspace
            .ensure_task_workspace_for_lifecycle(
                principal,
                workspace_name,
                "task_internal",
                TaskLifecycle::Internal,
            )
            .await
            .expect("internal task workspace");
        workspace
            .ensure_chat_session_workspace(principal, workspace_name, "session_1")
            .await
            .expect("chat session workspace");
        workspace
            .ensure_runtime_execution_workspace(principal, workspace_name, None, "exec_scoped")
            .await
            .expect("scoped execution workspace");

        tokio::fs::write(
            workspace
                .task_outputs_dir(principal, workspace_name, "task_visible")
                .join("out.md"),
            b"visible",
        )
        .await
        .expect("write visible output");
        tokio::fs::write(
            workspace
                .internal_task_outputs_dir(principal, workspace_name, "task_internal")
                .join("out.md"),
            b"internal",
        )
        .await
        .expect("write internal output");
        tokio::fs::write(
            workspace
                .chat_session_outputs_dir(principal, workspace_name, "session_1")
                .join("attachment.txt"),
            b"chat",
        )
        .await
        .expect("write chat output");

        assert!(workspace
            .task_outputs_dir(principal, workspace_name, "task_visible")
            .join("out.md")
            .exists());
        assert!(workspace
            .internal_task_outputs_dir(principal, workspace_name, "task_internal")
            .join("out.md")
            .exists());
        assert!(workspace
            .chat_session_outputs_dir(principal, workspace_name, "session_1")
            .join("attachment.txt")
            .exists());
        assert!(workspace
            .scoped_execution_dir(principal, workspace_name, "exec_scoped")
            .exists());
    }

    #[tokio::test]
    async fn silverbullet_space_provider_materializes_runtime_storage_at_space_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let space_root = tmp.path().join("MagicianNotes");
        // The provider roots directly at `space_root` now (no `.magician/runtime`
        // redirection), so the store materializes there. Alias `runtime_root`.
        let runtime_root = space_root.clone();
        let workspace = ArtifactV2Workspace::with_silverbullet_space_provider(&space_root)
            .expect("silverbullet provider");

        workspace.ensure_root().await.expect("runtime root");
        assert!(runtime_root.join("scopes").is_dir());
        assert!(runtime_root.join("system").is_dir());

        let output_path = workspace
            .task_outputs_dir("anonymous", "default", "task_visible")
            .join("out.md");
        workspace
            .write_path(&output_path, b"visible")
            .await
            .expect("write through silverbullet provider");

        assert_eq!(
            tokio::fs::read_to_string(
                runtime_root.join("scopes/anonymous/default/tasks/task_visible/outputs/out.md")
            )
            .await
            .expect("stored output"),
            "visible"
        );
    }

    #[tokio::test]
    async fn ensure_execution_workspace_preserves_internal_task_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let principal = "anonymous";
        let workspace_name = "default";
        let task_id = "task_internal";
        let execution_id = "exec_internal";

        workspace
            .ensure_task_workspace_for_lifecycle(
                principal,
                workspace_name,
                task_id,
                TaskLifecycle::Internal,
            )
            .await
            .expect("internal task workspace");
        workspace
            .ensure_execution_workspace(principal, workspace_name, task_id, execution_id)
            .await
            .expect("execution workspace");

        assert!(
            workspace
                .internal_task_dir(principal, workspace_name, task_id)
                .join("executions")
                .join(execution_id)
                .exists(),
            "execution should be under internal_tasks/<id>"
        );
        assert!(
            !workspace
                .user_visible_task_dir(principal, workspace_name, task_id)
                .exists(),
            "internal execution setup must not create an empty tasks/<id> sibling"
        );
    }

    #[tokio::test]
    async fn ensure_execution_workspace_requires_existing_task_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let principal = "anonymous";
        let workspace_name = "default";
        let task_id = "task_missing";

        let err = workspace
            .ensure_execution_workspace(principal, workspace_name, task_id, "exec_missing")
            .await
            .expect_err("missing task root should fail");

        assert!(matches!(err, ArtifactV2Error::TaskNotFound(ref id) if id == task_id));
        assert!(
            !workspace
                .user_visible_task_dir(principal, workspace_name, task_id)
                .exists(),
            "missing task execution setup must not create tasks/<id>"
        );
        assert!(
            !workspace
                .internal_task_dir(principal, workspace_name, task_id)
                .exists(),
            "missing task execution setup must not create internal_tasks/<id>"
        );
    }

    #[tokio::test]
    async fn bounded_jsonl_tail_ignores_large_corrupt_prefix_and_returns_exact_recent_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = vec![b'x'; 2 * 1024 * 1024];
        body.push(b'\n');
        for seq in 1_u64..=40 {
            body.extend(serde_json::to_vec(&serde_json::json!({"seq": seq})).expect("json"));
            body.push(b'\n');
        }
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_path::<serde_json::Value, _>(&path, 24, 64 * 1024)
            .await
            .expect("bounded tail");

        assert!(tail.bytes_read <= 64 * 1024);
        assert!(tail.file_len > tail.bytes_read);
        assert_eq!(tail.records.len(), 24);
        assert_eq!(
            tail.records.first().and_then(|row| row["seq"].as_u64()),
            Some(17)
        );
        assert_eq!(
            tail.records.last().and_then(|row| row["seq"].as_u64()),
            Some(40)
        );
    }

    #[tokio::test]
    async fn complete_jsonl_history_rejects_one_over_node_legacy_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("legacy-events.jsonl");
        let body = flat_json_array_record(MAX_JSONL_RECORD_NODES + 1, true);
        tokio::fs::write(&path, body).await.expect("legacy fixture");

        let error = workspace
            .read_jsonl_path::<serde::de::IgnoredAny, _>(&path)
            .await
            .expect_err("one-over legacy record must fail before typed Serde allocation");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn bounded_jsonl_tail_rejects_one_over_node_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let body = flat_json_array_record(MAX_JSONL_RECORD_NODES + 1, true);
        let max_bytes = body.len() as u64;
        tokio::fs::write(&path, body).await.expect("tail fixture");

        let error = workspace
            .read_jsonl_tail_path::<serde::de::IgnoredAny, _>(&path, 1, max_bytes)
            .await
            .expect_err("one-over tail record must fail before typed Serde allocation");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn committed_jsonl_reader_admits_exact_nodes_and_rejects_one_over() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let commit_path = tmp.path().join("events.commit");

        let exact = flat_json_array_record(MAX_JSONL_RECORD_NODES, true);
        tokio::fs::write(&path, &exact)
            .await
            .expect("exact fixture");
        tokio::fs::write(&commit_path, exact.len().to_string())
            .await
            .expect("exact commit authority");
        let admitted = workspace
            .read_committed_jsonl_path::<serde::de::IgnoredAny, _, _>(&path, &commit_path)
            .await
            .expect("exact node boundary remains readable");
        assert_eq!(admitted.len(), 1);

        let one_over = flat_json_array_record(MAX_JSONL_RECORD_NODES + 1, true);
        tokio::fs::write(&path, &one_over)
            .await
            .expect("one-over fixture");
        tokio::fs::write(&commit_path, one_over.len().to_string())
            .await
            .expect("one-over commit authority");
        let error = workspace
            .read_committed_jsonl_path::<serde::de::IgnoredAny, _, _>(&path, &commit_path)
            .await
            .expect_err("committed one-over record fails admission");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn committed_jsonl_forward_pages_keep_an_append_stable_cursor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let commit_path = tmp.path().join("events.commit");
        let mut body = Vec::new();
        for seq in 0_u64..5 {
            body.extend(serde_json::to_vec(&serde_json::json!({"seq": seq})).expect("json"));
            body.push(b'\n');
        }
        tokio::fs::write(&path, &body).await.expect("event fixture");
        tokio::fs::write(&commit_path, body.len().to_string())
            .await
            .expect("commit authority");

        let first = workspace
            .read_committed_jsonl_forward_page_path::<serde_json::Value, _, _>(
                &path,
                &commit_path,
                0,
                2,
                4096,
            )
            .await
            .expect("first page");
        assert_eq!(first.records.len(), 2);
        assert_eq!(first.records[0]["seq"], 0);
        assert_eq!(first.records[1]["seq"], 1);
        assert!(!first.is_complete());

        for seq in 5_u64..7 {
            body.extend(serde_json::to_vec(&serde_json::json!({"seq": seq})).expect("json"));
            body.push(b'\n');
        }
        tokio::fs::write(&path, &body)
            .await
            .expect("appended fixture");
        tokio::fs::write(&commit_path, body.len().to_string())
            .await
            .expect("advanced commit authority");
        let second = workspace
            .read_committed_jsonl_forward_page_path::<serde_json::Value, _, _>(
                &path,
                &commit_path,
                first.next_offset,
                2,
                4096,
            )
            .await
            .expect("second page after append");
        assert_eq!(second.records.len(), 2);
        assert_eq!(second.records[0]["seq"], 2);
        assert_eq!(second.records[1]["seq"], 3);
        assert!(second.next_offset > first.next_offset);

        let boundary_error = workspace
            .read_committed_jsonl_forward_page_path::<serde_json::Value, _, _>(
                &path,
                &commit_path,
                1,
                2,
                4096,
            )
            .await
            .expect_err("mid-record cursor must fail closed");
        assert!(matches!(boundary_error, ArtifactV2Error::InvalidRequest(_)));

        let record_too_wide = workspace
            .read_committed_jsonl_forward_page_path::<serde_json::Value, _, _>(
                &path,
                &commit_path,
                0,
                2,
                4,
            )
            .await
            .expect_err("one oversized record cannot become a no-progress page");
        assert!(matches!(
            record_too_wide,
            ArtifactV2Error::InvalidRequest(_)
        ));

        tokio::fs::write(&path, b"\n")
            .await
            .expect("blank committed record fixture");
        tokio::fs::write(&commit_path, "1")
            .await
            .expect("blank commit authority");
        let blank_error = workspace
            .read_committed_jsonl_forward_page_path::<serde_json::Value, _, _>(
                &path,
                &commit_path,
                0,
                1,
                4096,
            )
            .await
            .expect_err("blank committed records cannot bypass the record budget");
        assert!(matches!(blank_error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn bounded_directory_pages_retain_only_a_page_and_advance_stably() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let directory = tmp.path().join("receipts");
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("receipt directory");
        for name in ["a.json", "b.json", "c.json", "d.json"] {
            tokio::fs::write(directory.join(name), b"{}")
                .await
                .expect("receipt fixture");
        }

        let first = workspace
            .read_dir_page_path_or_empty(&directory, None, 2, 4)
            .await
            .expect("first bounded page");
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.file_name.as_str())
                .collect::<Vec<_>>(),
            vec!["a.json", "b.json"]
        );
        assert_eq!(first.next_after.as_deref(), Some("b.json"));
        assert!(!first.complete);
        assert!(!first.overflow);

        // Mutation before the cursor cannot rewind the active pass. The
        // caller's completed-pass reset discovers it deterministically.
        tokio::fs::remove_file(directory.join("a.json"))
            .await
            .expect("remove prior receipt");
        tokio::fs::write(directory.join("aa.json"), b"{}")
            .await
            .expect("insert before cursor");

        let second = workspace
            .read_dir_page_path_or_empty(&directory, first.next_after.as_deref(), 2, 4)
            .await
            .expect("second bounded page");
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.file_name.as_str())
                .collect::<Vec<_>>(),
            vec!["c.json", "d.json"]
        );
        assert!(second.next_after.is_none());
        assert!(second.complete);
        assert!(!second.overflow);

        let reset = workspace
            .read_dir_page_path_or_empty(&directory, None, 2, 4)
            .await
            .expect("completed pass resets from the beginning");
        assert_eq!(
            reset
                .entries
                .iter()
                .map(|entry| entry.file_name.as_str())
                .collect::<Vec<_>>(),
            vec!["aa.json", "b.json"]
        );
    }

    #[tokio::test]
    async fn bounded_directory_pages_fail_closed_on_overflow_and_bad_cursor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let directory = tmp.path().join("receipts");
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("receipt directory");
        for name in ["a", "b", "c", "d", "e"] {
            tokio::fs::write(directory.join(name), b"{}")
                .await
                .expect("receipt fixture");
        }

        let overflow = workspace
            .read_dir_page_path_or_empty(&directory, None, 2, 4)
            .await
            .expect("one-over directory still returns a progress page");
        assert!(overflow.overflow);
        assert_eq!(overflow.entries.len(), 2);
        assert!(!overflow.complete);
        let admitted_tail = workspace
            .read_dir_page_path_or_empty(&directory, overflow.next_after.as_deref(), 2, 4)
            .await
            .expect("overflow pass advances through its admitted prefix");
        assert!(admitted_tail.overflow);
        assert_eq!(admitted_tail.entries.len(), 2);
        let terminal_debt = workspace
            .read_dir_page_path_or_empty(&directory, admitted_tail.next_after.as_deref(), 2, 4)
            .await
            .expect("overflow pass terminates without rescanning beyond its ceiling");
        assert!(terminal_debt.overflow);
        assert!(terminal_debt.entries.is_empty());
        assert!(terminal_debt.next_after.is_none());
        assert!(!terminal_debt.complete);
        let bad_cursor = workspace
            .read_dir_page_path_or_empty(&directory, Some("../escape"), 2, 8)
            .await
            .expect_err("path-like cursor must fail before scanning");
        assert!(matches!(bad_cursor, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn committed_jsonl_length_rejects_oversized_authority_before_reading_its_body() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let commit_path = tmp.path().join("events.commit");
        tokio::fs::write(&path, b"{\"seq\":1}\n")
            .await
            .expect("event fixture");
        tokio::fs::write(
            &commit_path,
            vec![b'9'; (MAX_JSONL_COMMIT_AUTHORITY_BYTES + 1) as usize],
        )
        .await
        .expect("oversized authority fixture");

        let error = workspace
            .committed_jsonl_len_path(&path, &commit_path)
            .await
            .expect_err("oversized authority must fail closed");
        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn bounded_jsonl_tail_hides_unacknowledged_partial_record_after_crash() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        tokio::fs::write(&path, b"{\"seq\":1}\n{\"seq\":2}\n{\"seq\":3")
            .await
            .expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_path::<serde_json::Value, _>(&path, 2, 4096)
            .await
            .expect("bounded tail");

        assert_eq!(tail.records.len(), 2);
        assert_eq!(tail.records[0]["seq"], 1);
        assert_eq!(tail.records[1]["seq"], 2);
    }

    #[tokio::test]
    async fn bounded_jsonl_tail_keeps_first_record_at_exact_window_boundary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let prefix = b"{\"seq\":0}\n";
        let wanted = b"{\"seq\":1}\n{\"seq\":2}\n";
        let mut body = prefix.to_vec();
        body.extend_from_slice(wanted);
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_path::<serde_json::Value, _>(&path, 2, wanted.len() as u64)
            .await
            .expect("exact-boundary tail");

        assert_eq!(tail.records.len(), 2);
        assert_eq!(tail.records[0]["seq"], 1);
        assert_eq!(tail.records[1]["seq"], 2);
    }

    #[tokio::test]
    async fn bounded_jsonl_tail_retains_only_requested_lines_from_a_wide_window() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = Vec::new();
        for seq in 0_u64..200_000 {
            body.extend(serde_json::to_vec(&serde_json::json!({"seq": seq})).expect("json"));
            body.push(b'\n');
        }
        let body_len = body.len() as u64;
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_path::<serde_json::Value, _>(&path, 3, body_len)
            .await
            .expect("wide bounded tail");

        assert_eq!(tail.records.len(), 3);
        assert_eq!(tail.records[0]["seq"], 199_997);
        assert_eq!(tail.records[2]["seq"], 199_999);
    }

    #[test]
    fn complete_jsonl_history_rejects_deep_records_on_a_small_stack_before_serde() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let depth = MAX_RETAINED_JSON_DEPTH + 32;
        let mut body = b"{\"seq\":1}\n".to_vec();
        body.extend(std::iter::repeat(b'[').take(depth));
        body.extend_from_slice(b"null");
        body.extend(std::iter::repeat(b']').take(depth));
        body.extend_from_slice(b"\n{\"seq\":3}\n");
        std::fs::write(&path, body).expect("fixture write");

        let worker = std::thread::Builder::new()
            .name("jsonl-history-small-stack".to_string())
            .stack_size(256 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime")
                    .block_on(workspace.read_jsonl_path::<serde_json::Value, _>(&path))
            })
            .expect("small-stack worker");
        let error = worker
            .join()
            .expect("small-stack worker must not overflow")
            .expect_err("deep JSONL history record must fail admission");

        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[test]
    fn bounded_jsonl_tail_rejects_deep_records_on_a_small_stack_before_serde() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let depth = MAX_RETAINED_JSON_DEPTH + 32;
        let mut record = Vec::with_capacity(depth.saturating_mul(2).saturating_add(6));
        record.extend(std::iter::repeat(b'[').take(depth));
        record.extend_from_slice(b"null");
        record.extend(std::iter::repeat(b']').take(depth));
        record.push(b'\n');
        std::fs::write(&path, record).expect("fixture write");

        let worker = std::thread::Builder::new()
            .name("jsonl-tail-small-stack".to_string())
            .stack_size(256 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime")
                    .block_on(workspace.read_jsonl_tail_path::<serde_json::Value, _>(
                        &path,
                        1,
                        64 * 1024,
                    ))
            })
            .expect("small-stack worker");
        let error = worker
            .join()
            .expect("small-stack worker must not overflow")
            .expect_err("deep JSONL record must fail admission");

        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn adaptive_jsonl_tail_recovers_latest_record_larger_than_initial_window() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = b"{\"seq\":1}\n".to_vec();
        body.extend(
            serde_json::to_vec(&serde_json::json!({
                "seq": 2,
                "payload": "x".repeat(10 * 1024 * 1024),
            }))
            .expect("large record"),
        );
        body.push(b'\n');
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_adaptive_path::<serde_json::Value, _>(
                &path,
                1,
                64 * 1024,
                16 * 1024 * 1024,
            )
            .await
            .expect("adaptive oversized tail");

        assert_eq!(tail.records.len(), 1);
        assert_eq!(tail.records[0]["seq"], 2);
        assert!(tail.bytes_read > 64 * 1024);
        assert!(tail.bytes_read <= 16 * 1024 * 1024);
    }

    #[tokio::test]
    async fn adaptive_jsonl_tail_discards_oversized_unterminated_crash_fragment() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = b"{\"seq\":1}\n{\"seq\":2}\n{\"seq\":3,\"payload\":\"".to_vec();
        body.extend(std::iter::repeat(b'x').take(10 * 1024 * 1024));
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_adaptive_path::<serde_json::Value, _>(
                &path,
                2,
                64 * 1024,
                16 * 1024 * 1024,
            )
            .await
            .expect("adaptive corrupt tail");

        assert_eq!(tail.records.len(), 2);
        assert_eq!(tail.records[0]["seq"], 1);
        assert_eq!(tail.records[1]["seq"], 2);
        assert!(tail.bytes_read > 64 * 1024);
        assert!(tail.bytes_read <= 16 * 1024 * 1024);
    }

    #[tokio::test]
    async fn adaptive_jsonl_tail_skips_a_near_cap_crash_fragment_before_history() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = b"{\"seq\":1}\n{\"seq\":2}\n{\"seq\":3,\"payload\":\"".to_vec();
        body.extend(std::iter::repeat(b'x').take((64 * 1024 * 1024) - 32));
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_adaptive_path::<serde_json::Value, _>(
                &path,
                2,
                8 * 1024 * 1024,
                72 * 1024 * 1024,
            )
            .await
            .expect("near-cap crash-fragment recovery");

        assert_eq!(tail.records.len(), 2);
        assert_eq!(tail.records[0]["seq"], 1);
        assert_eq!(tail.records[1]["seq"], 2);
        assert!(tail.bytes_read > 64 * 1024 * 1024);
        assert!(tail.bytes_read <= 72 * 1024 * 1024);
    }

    #[tokio::test]
    async fn adaptive_jsonl_tail_budgets_both_crash_fragment_and_large_predecessor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = serde_json::to_vec(&serde_json::json!({
            "seq": 7,
            "payload": "p".repeat(4 * 1024 * 1024),
        }))
        .expect("large predecessor");
        body.push(b'\n');
        body.extend_from_slice(b"{\"seq\":8,\"payload\":\"");
        body.extend(std::iter::repeat(b'x').take(4 * 1024 * 1024));
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_adaptive_path::<serde_json::Value, _>(
                &path,
                1,
                1024 * 1024,
                9 * 1024 * 1024,
            )
            .await
            .expect("fragment plus predecessor recovery");

        assert_eq!(tail.records.len(), 1);
        assert_eq!(tail.records[0]["seq"], 7);
        assert!(tail.bytes_read > 8 * 1024 * 1024);
        assert!(tail.bytes_read <= 9 * 1024 * 1024);
    }

    #[tokio::test]
    async fn adaptive_jsonl_tail_fails_closed_when_requested_history_exceeds_ceiling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = Vec::new();
        for seq in 1_u64..=3 {
            body.extend(
                serde_json::to_vec(&serde_json::json!({
                    "seq": seq,
                    "payload": "x".repeat(2 * 1024 * 1024),
                }))
                .expect("large record"),
            );
            body.push(b'\n');
        }
        tokio::fs::write(&path, body).await.expect("fixture write");

        let error = workspace
            .read_jsonl_tail_adaptive_path::<serde_json::Value, _>(
                &path,
                3,
                1024 * 1024,
                4 * 1024 * 1024,
            )
            .await
            .expect_err("three complete records exceed the total tail ceiling");

        assert!(matches!(
            error,
            ArtifactV2Error::InvalidRequest(message)
                if message.starts_with(JSONL_TAIL_WINDOW_ERROR_PREFIX)
        ));
    }

    #[tokio::test]
    async fn recent_jsonl_tail_stops_at_the_window_instead_of_reading_the_whole_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = Vec::new();
        for seq in 1_u64..=8_000 {
            body.extend(
                serde_json::to_vec(&serde_json::json!({"seq": seq, "pad": "x".repeat(256)}))
                    .expect("json"),
            );
            body.push(b'\n');
        }
        let file_len = body.len() as u64;
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_recent_path::<serde_json::Value, _>(
                &path,
                60,
                64 * 1024,
                4 * 1024 * 1024,
            )
            .await
            .expect("recent tail");

        assert_eq!(tail.records.len(), 60);
        assert_eq!(
            tail.records.last().and_then(|row| row["seq"].as_u64()),
            Some(8_000)
        );
        assert_eq!(
            tail.records.first().and_then(|row| row["seq"].as_u64()),
            Some(7_941)
        );
        assert!(tail.bytes_read <= 64 * 1024);
        assert!(
            file_len > 16 * tail.bytes_read,
            "fixture must dwarf the window"
        );
        assert_eq!(tail.file_len, file_len);
    }

    #[tokio::test]
    async fn recent_jsonl_tail_degrades_where_the_adaptive_tail_fails_closed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = Vec::new();
        for seq in 1_u64..=3 {
            body.extend(
                serde_json::to_vec(&serde_json::json!({
                    "seq": seq,
                    "payload": "x".repeat(2 * 1024 * 1024),
                }))
                .expect("large record"),
            );
            body.push(b'\n');
        }
        tokio::fs::write(&path, body).await.expect("fixture write");

        // Same fixture and same ceiling as
        // `adaptive_jsonl_tail_fails_closed_when_requested_history_exceeds_ceiling`,
        // which errors. A recent-activity reader gets a short answer instead.
        let tail = workspace
            .read_jsonl_tail_recent_path::<serde_json::Value, _>(
                &path,
                3,
                1024 * 1024,
                4 * 1024 * 1024,
            )
            .await
            .expect("recent tail degrades rather than failing closed");

        assert!(!tail.records.is_empty() && tail.records.len() < 3);
        assert_eq!(
            tail.records.last().and_then(|row| row["seq"].as_u64()),
            Some(3)
        );
        assert!(tail.bytes_read <= 4 * 1024 * 1024);
        assert!(
            tail.file_len > tail.bytes_read,
            "a short answer must still say more history exists"
        );
    }

    #[tokio::test]
    async fn recent_jsonl_tail_returns_a_short_file_whole() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("events.jsonl");
        let mut body = Vec::new();
        for seq in 1_u64..=4 {
            body.extend(serde_json::to_vec(&serde_json::json!({"seq": seq})).expect("json"));
            body.push(b'\n');
        }
        tokio::fs::write(&path, body).await.expect("fixture write");

        let tail = workspace
            .read_jsonl_tail_recent_path::<serde_json::Value, _>(&path, 60, 64 * 1024, 1024 * 1024)
            .await
            .expect("recent tail");

        assert_eq!(tail.records.len(), 4);
        assert_eq!(
            tail.file_len, tail.bytes_read,
            "the window covered the file"
        );
    }

    #[tokio::test]
    async fn compact_json_write_round_trips_through_the_pretty_reader() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("index.json");
        let value = serde_json::json!({"a": {"b": [1, 2, 3]}});

        workspace
            .write_json_compact_atomic_path(&path, &value)
            .await
            .expect("compact write");

        let raw = tokio::fs::read(&path).await.expect("read back");
        assert!(
            !raw.contains(&b'\n'),
            "compact JSON must not carry indentation"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&raw).expect("parse"),
            value
        );
    }

    #[tokio::test]
    async fn version_two_multi_write_recovery_keeps_large_payload_out_of_journal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let journal_path = tmp.path().join("task.write_journal.json");
        let staging_dir = multi_write_staging_dir(&journal_path);
        let staged_path = staging_dir.join("0000.payload");
        let target_path = tmp.path().join("state").join("task_state.json");
        let payload = vec![b'x'; 2 * 1024 * 1024];
        workspace
            .write_atomic_path(&staged_path, &payload)
            .await
            .expect("staged payload");
        let journal = WorkspacePersistedWriteJournalV2 {
            version: 2,
            staging_dir: staging_dir.clone(),
            writes: vec![WorkspacePersistedWriteOpV2 {
                path: target_path.clone(),
                staged_path,
                size_bytes: payload.len() as u64,
                content_hash: multi_write_content_hash(&payload),
            }],
        };
        let journal_bytes = serde_json::to_vec(&journal).expect("journal serializes");
        assert!(
            journal_bytes.len() < 2048,
            "metadata journal must not scale with the staged payload"
        );
        workspace
            .write_atomic_path(&journal_path, &journal_bytes)
            .await
            .expect("journal persists");

        let recovered = workspace
            .recover_multi_write_journal_path(&journal_path)
            .await
            .expect("journal recovers");

        assert_eq!(recovered, vec![target_path.clone()]);
        assert_eq!(
            tokio::fs::metadata(&target_path)
                .await
                .expect("target metadata")
                .len(),
            payload.len() as u64
        );
        assert!(!journal_path.exists());
        assert!(!staging_dir.exists());
    }

    #[tokio::test]
    async fn version_two_multi_write_recovery_fails_closed_before_corrupt_write_lands() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let journal_path = tmp.path().join("task.write_journal.json");
        let staging_dir = multi_write_staging_dir(&journal_path);
        let first_staged = staging_dir.join("0000.payload");
        let corrupt_staged = staging_dir.join("0001.payload");
        let first_target = tmp.path().join("first.json");
        let corrupt_target = tmp.path().join("corrupt.json");
        workspace
            .write_atomic_path(&first_staged, b"first-new")
            .await
            .expect("first staged payload");
        workspace
            .write_atomic_path(&corrupt_staged, b"tampered")
            .await
            .expect("corrupt staged payload");
        workspace
            .write_atomic_path(&first_target, b"first-old")
            .await
            .expect("first target seed");
        let journal = WorkspacePersistedWriteJournalV2 {
            version: 2,
            staging_dir: staging_dir.clone(),
            writes: vec![
                WorkspacePersistedWriteOpV2 {
                    path: first_target.clone(),
                    staged_path: first_staged,
                    size_bytes: 9,
                    content_hash: multi_write_content_hash(b"first-new"),
                },
                WorkspacePersistedWriteOpV2 {
                    path: corrupt_target.clone(),
                    staged_path: corrupt_staged,
                    size_bytes: 8,
                    content_hash: multi_write_content_hash(b"expected"),
                },
            ],
        };
        workspace
            .write_json_atomic_path(&journal_path, &journal)
            .await
            .expect("journal persists");

        let error = workspace
            .recover_multi_write_journal_path(&journal_path)
            .await
            .expect_err("corruption must fail closed");

        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
        assert_eq!(
            tokio::fs::read(&first_target)
                .await
                .expect("first target remains"),
            b"first-old"
        );
        assert!(!corrupt_target.exists());
        assert!(journal_path.exists(), "journal must remain retryable");
        assert!(
            staging_dir.exists(),
            "staged evidence must remain inspectable"
        );
    }

    #[tokio::test]
    async fn multi_write_rejects_every_destination_before_creating_recovery_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let journal_path = tmp.path().join("task.write_journal.json");
        let staging_dir = multi_write_staging_dir(&journal_path);
        let outside = tmp
            .path()
            .parent()
            .expect("temp parent")
            .join("outside.json");

        workspace
            .commit_multi_write_journal_path(
                &journal_path,
                &[(outside, b"must-not-stage".to_vec())],
            )
            .await
            .expect_err("out-of-root destination is rejected");

        assert!(!journal_path.exists());
        assert!(!staging_dir.exists());
    }

    #[tokio::test]
    async fn legacy_multi_write_validates_every_destination_before_replay() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let journal_path = tmp.path().join("task.write_journal.json");
        let first_target = tmp.path().join("first.json");
        let outside = tmp
            .path()
            .parent()
            .expect("temp parent")
            .join("outside.json");
        workspace
            .write_atomic_path(&first_target, b"old")
            .await
            .expect("seed target");
        let journal = WorkspacePersistedWriteJournalV1 {
            version: 1,
            writes: vec![
                WorkspacePersistedWriteOpV1 {
                    path: first_target.clone(),
                    bytes: b"new".to_vec(),
                },
                WorkspacePersistedWriteOpV1 {
                    path: outside,
                    bytes: b"must-not-land".to_vec(),
                },
            ],
        };
        workspace
            .write_json_atomic_path(&journal_path, &journal)
            .await
            .expect("legacy journal persists");

        workspace
            .recover_multi_write_journal_path(&journal_path)
            .await
            .expect_err("out-of-root legacy destination is rejected");

        assert_eq!(
            tokio::fs::read(first_target)
                .await
                .expect("first target remains"),
            b"old"
        );
        assert!(journal_path.exists(), "failed recovery remains inspectable");
    }

    #[tokio::test]
    async fn multi_write_recovery_rejects_deep_json_before_deserialization() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let journal_path = tmp.path().join("task.write_journal.json");
        let mut body = vec![b'['; 10_000];
        body.extend(std::iter::repeat_n(b']', 10_000));
        workspace
            .write_atomic_path(&journal_path, &body)
            .await
            .expect("adversarial journal persists");

        let error = workspace
            .recover_multi_write_journal_path(&journal_path)
            .await
            .expect_err("deep journal is rejected by raw admission");

        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
        assert!(
            journal_path.exists(),
            "rejected journal remains inspectable"
        );
    }

    #[test]
    fn task_resolution_prefers_internal_when_duplicate_roots_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let principal = "anonymous";
        let workspace_name = "default";
        let task_id = "task_duplicate";
        let user_visible = workspace.user_visible_task_dir(principal, workspace_name, task_id);
        let internal = workspace.internal_task_dir(principal, workspace_name, task_id);

        std::fs::create_dir_all(&user_visible).expect("user-visible task dir");
        std::fs::create_dir_all(&internal).expect("internal task dir");

        assert_eq!(
            workspace.resolve_task_dir(principal, workspace_name, task_id),
            Some(internal.clone())
        );
        assert_eq!(
            workspace.resolve_task_location(principal, workspace_name, task_id),
            Some(TaskLocation::Internal)
        );
        assert_eq!(
            workspace.task_dir(principal, workspace_name, task_id),
            internal
        );
    }

    /// The three enumerations must agree. A sweep picks whichever shape its
    /// call site needs — sync, fallible-sync, or async — and a sink that leaks
    /// through any one of them is materialised just as thoroughly as through
    /// the others.
    #[tokio::test]
    async fn every_tenant_enumeration_drops_the_reserved_sinks() {
        use crate::magician_v2::transport_log::{
            QUARANTINE_PRINCIPAL, QUARANTINE_WORKSPACE, SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE,
        };

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        for (principal, workspace_name) in [
            (DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
            ("alice", "default"),
            (SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE),
            (QUARANTINE_PRINCIPAL, QUARANTINE_WORKSPACE),
        ] {
            std::fs::create_dir_all(workspace.scope_root(principal, workspace_name))
                .expect("scope directory");
        }

        let expected = vec![
            ("alice".to_string(), "default".to_string()),
            (
                DEFAULT_SCOPE_PRINCIPAL.to_string(),
                DEFAULT_SCOPE_WORKSPACE.to_string(),
            ),
        ];
        let sorted = |mut scopes: Vec<(String, String)>| {
            scopes.sort();
            scopes
        };

        assert_eq!(sorted(workspace.list_scopes()).len(), 4, "all four exist");
        assert_eq!(sorted(workspace.list_tenant_scopes()), expected);
        assert_eq!(
            sorted(
                workspace
                    .list_tenant_scope_segments_sync()
                    .expect("sync segments")
            ),
            expected
        );
        assert_eq!(
            sorted(
                workspace
                    .list_tenant_scope_segments()
                    .await
                    .expect("async segments")
            ),
            expected
        );
    }

    /// The reserved sinks are the two buckets that catch records belonging to
    /// no tenant. They have no owner, no surface and no inbox, so the
    /// subsystems only a tenant uses must never materialize in them.
    #[test]
    fn reserved_sinks_do_not_host_user_subsystems() {
        use crate::magician_v2::transport_log::{
            QUARANTINE_PRINCIPAL, QUARANTINE_WORKSPACE, SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE,
        };

        assert_eq!(
            scope_profile(SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE),
            ScopeProfile::ReservedSink
        );
        assert_eq!(
            scope_profile(QUARANTINE_PRINCIPAL, QUARANTINE_WORKSPACE),
            ScopeProfile::ReservedSink
        );
        assert!(!scope_hosts_user_subsystems(
            SYSTEM_PRINCIPAL,
            SYSTEM_WORKSPACE
        ));
        assert!(!scope_hosts_user_subsystems(
            QUARANTINE_PRINCIPAL,
            QUARANTINE_WORKSPACE
        ));
    }

    /// The default scope is a real tenant and the one a single-user deployment
    /// actually uses. Gating it would ship a deployment with no apps, so the
    /// predicate must not widen from "reserved sink" to "anything system-ish".
    #[test]
    fn real_tenants_including_the_default_scope_host_user_subsystems() {
        for (principal, workspace_name) in [
            (DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
            ("alice", "default"),
            ("system", "notes"),
            ("anonymous", "system"),
        ] {
            assert_eq!(
                scope_profile(principal, workspace_name),
                ScopeProfile::User,
                "{principal}/{workspace_name} is a tenant"
            );
            assert!(scope_hosts_user_subsystems(principal, workspace_name));
        }
    }
}
