//! The workspace ownership registry — `scopes/<principal>/workspaces.json`.
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md` §2.5.
//! Directories remain the **isolation** mechanism (the storage-isolation
//! tests prove that half); this registry is the **authorization** source of
//! truth — what `ScopeRef::from_session` checks.
//!
//! March-doc decisions carried in (per §2.5): the default workspace is
//! minted with the identity, is labelled "Personal", and cannot be deleted
//! or renamed. Creation is explicit only — no API creates a workspace as a
//! side effect of naming one. Deletion refuses workspaces with live state
//! on disk (an over-cautious but honest proxy: any non-empty workspace
//! directory blocks deletion until its contents are cleared by hand).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::identity::{validate_principal_name, Identity};
use super::{AuthError, AuthStoreError};

pub const DEFAULT_WORKSPACE_ID: &str = "default";
const DEFAULT_WORKSPACE_DISPLAY_NAME: &str = "Personal";
const REGISTRY_FILE: &str = "workspaces.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    /// Slug — a directory name, validated by the same pattern as principals.
    pub id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    /// `true` only for `default`.
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspacesFile {
    pub schema_version: u32,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    /// Workspaces deleted with `purge`: the registry row is already gone, and
    /// the directory is removed by [`drain_pending_purges`] at the next server
    /// start, before any store discovers scopes.
    ///
    /// Not removed on the spot because the running service cannot let go of a
    /// hydrated scope: a dozen per-scope caches (analytics and feed DuckDB,
    /// social SQLite, the events log, the agent schedulers) hold handles with
    /// no eviction path, background workers do not pass the HTTP ownership
    /// gate, and the LLM trace journal recreates a deleted directory from its
    /// in-memory sequence — which the next boot then refuses to load. At boot
    /// nothing is hydrated yet, so removal there cannot race any of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_purge: Vec<String>,
    /// Tombstones: ids deleted through [`delete`] or
    /// [`delete_and_schedule_purge`]. This — not mere absence from
    /// `workspaces` — is the positive evidence the destructive retirement
    /// paths (boot row/file/bearer/device cleanup) require: a row can go
    /// missing for other reasons (a pre-registry workspace never adopted, a
    /// damaged file), and those must never cost a tenant its data. Ids queued
    /// in `pending_purge` count as tombstoned too. [`create`] clears a
    /// slug's tombstone once its purge has drained.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
}

/// Exclusive advisory lock on `workspaces.json.lock` beside the registry,
/// held across every read-modify-write so concurrent create/update/delete/
/// drain cannot lose each other's rows. Released on drop. The registry itself
/// is replaced atomically (staged temp + rename), so lock-free readers never
/// see a torn file.
fn lock_registry(path: &Path) -> Result<std::fs::File, AuthStoreError> {
    let lock_path = path.with_file_name(format!("{REGISTRY_FILE}.lock"));
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    file.lock()?;
    Ok(file)
}

fn tombstone(file: &mut WorkspacesFile, workspace_id: &str) {
    if !file.deleted.iter().any(|id| id == workspace_id) {
        file.deleted.push(workspace_id.to_string());
    }
}

/// `scopes/<principal-dir>/workspaces.json` for the identity's scope root.
pub fn registry_path(scopes_root: &Path, identity: &Identity) -> PathBuf {
    registry_path_for_principal_dir(scopes_root, &identity.scope_root)
}

/// `scopes/<principal-dir>/workspaces.json` for a principal directory name —
/// the same file [`registry_path`] names, for callers that start from a
/// directory walk rather than from an [`Identity`].
pub fn registry_path_for_principal_dir(scopes_root: &Path, principal_dir: &str) -> PathBuf {
    scopes_root.join(principal_dir).join(REGISTRY_FILE)
}

fn read_registry(path: &Path) -> Result<WorkspacesFile, AuthStoreError> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(WorkspacesFile::default()),
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(WorkspacesFile::default()),
        Err(error) => Err(error.into()),
    }
}

fn write_registry(path: &Path, file: &WorkspacesFile) -> Result<(), AuthStoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(file)?;
    crate::magician_v2::artifact_v2::io::write_bytes_durably_with_mode_sync(
        path,
        &bytes,
        Some(0o600),
    )?;
    Ok(())
}

/// The registry for one identity, with the default workspace ensured. The
/// default is minted on first touch — including for the adopted
/// `anonymous` root, whose `default` workspace already exists as data and
/// only needs its registry row.
pub fn ensure_default(
    scopes_root: &Path,
    identity: &Identity,
) -> Result<Vec<Workspace>, AuthStoreError> {
    let path = registry_path(scopes_root, identity);
    let has_default = |file: &WorkspacesFile| {
        file.workspaces
            .iter()
            .any(|workspace| workspace.id == DEFAULT_WORKSPACE_ID)
    };
    // Lock-free fast path: this runs on every ownership check.
    let file = read_registry(&path)?;
    if has_default(&file) {
        return Ok(file.workspaces);
    }
    let _lock = lock_registry(&path)?;
    let mut file = read_registry(&path)?;
    if !has_default(&file) {
        file.workspaces.push(Workspace {
            id: DEFAULT_WORKSPACE_ID.to_string(),
            display_name: DEFAULT_WORKSPACE_DISPLAY_NAME.to_string(),
            description: None,
            created_at: Utc::now(),
            is_default: true,
        });
        write_registry(&path, &file)?;
    }
    Ok(file.workspaces)
}

pub fn list(scopes_root: &Path, identity: &Identity) -> Result<Vec<Workspace>, AuthStoreError> {
    ensure_default(scopes_root, identity)
}

/// Does the identity own this workspace id? (The set `from_session`
/// authorizes against.)
pub fn owns(
    scopes_root: &Path,
    identity: &Identity,
    workspace_id: &str,
) -> Result<bool, AuthStoreError> {
    Ok(list(scopes_root, identity)?
        .iter()
        .any(|workspace| workspace.id == workspace_id))
}

/// Explicit-only creation (workspace design §2.5). Slugs are validated by
/// the same pattern as principals — workspaces are directory names too.
pub fn create(
    scopes_root: &Path,
    identity: &Identity,
    slug: &str,
    display_name: &str,
    description: Option<String>,
) -> Result<Workspace, AuthError> {
    validate_principal_name(slug).map_err(|_| AuthError::InvalidPrincipalName(slug.to_string()))?;
    let path = registry_path(scopes_root, identity);
    let _lock = lock_registry(&path)?;
    let mut file = read_registry(&path)?;
    if file.workspaces.iter().any(|workspace| workspace.id == slug) {
        return Err(AuthError::WorkspaceExists(slug.to_string()));
    }
    // The old directory is still on disk until the next start drains it: a new
    // workspace would silently inherit the deleted one's data, or be deleted
    // itself by the drain. Refuse until the purge has run.
    if file.pending_purge.iter().any(|pending| pending == slug) {
        return Err(AuthError::WorkspacePendingPurge(slug.to_string()));
    }
    let workspace = Workspace {
        id: slug.to_string(),
        display_name: display_name.trim().to_string(),
        description,
        created_at: Utc::now(),
        is_default: false,
    };
    // Its purge has drained (checked above), so the slug starts clean: the
    // tombstone that let boot retire the old workspace's rows is lifted.
    file.deleted.retain(|id| id != slug);
    file.workspaces.push(workspace.clone());
    write_registry(&path, &file).map_err(AuthError::from)?;
    Ok(workspace)
}

/// `PATCH` surface: display name and description only. The slug and the
/// default flag are immutable — the March rule that `default` cannot be
/// renamed.
pub fn update(
    scopes_root: &Path,
    identity: &Identity,
    workspace_id: &str,
    display_name: Option<String>,
    description: Option<Option<String>>,
) -> Result<Workspace, AuthError> {
    let path = registry_path(scopes_root, identity);
    let _lock = lock_registry(&path)?;
    let mut file = read_registry(&path)?;
    let workspace = file
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| AuthError::UnownedWorkspace {
            principal: identity.scope_root.clone(),
            workspace: workspace_id.to_string(),
        })?;
    if let Some(display_name) = display_name {
        workspace.display_name = display_name.trim().to_string();
    }
    if let Some(description) = description {
        workspace.description = description;
    }
    let updated = workspace.clone();
    write_registry(&path, &file).map_err(AuthError::from)?;
    Ok(updated)
}

/// Does any actual file live under `dir` (bounded depth so a symlink loop
/// cannot hang deletion)? Empty leftover directories are not live state —
/// only files are.
fn workspace_has_files(dir: &Path, depth: u8) -> bool {
    if depth > 4 {
        return true; // unreasonably deep: fail closed, refuse deletion
    }
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().any(|entry| {
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            let is_file = entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false);
            is_file || (is_dir && workspace_has_files(&entry.path(), depth + 1))
        }),
        Err(_) => false,
    }
}

/// Guarded deletion: the default workspace refuses outright, and any
/// workspace whose directory still contains files refuses until cleared
/// (empty directories are clearable residue). The registry row is the only
/// thing deleted — never data.
pub fn delete(
    scopes_root: &Path,
    identity: &Identity,
    workspace_id: &str,
) -> Result<(), AuthError> {
    let path = registry_path(scopes_root, identity);
    let _lock = lock_registry(&path)?;
    let mut file = read_registry(&path)?;
    let Some(workspace) = file.workspaces.iter().find(|w| w.id == workspace_id) else {
        return Err(AuthError::UnownedWorkspace {
            principal: identity.scope_root.clone(),
            workspace: workspace_id.to_string(),
        });
    };
    if workspace.is_default {
        return Err(AuthError::DefaultWorkspaceProtected);
    }
    let workspace_dir = scopes_root.join(&identity.scope_root).join(workspace_id);
    if workspace_has_files(&workspace_dir, 0) {
        return Err(AuthError::WorkspaceHasLiveState(workspace_id.to_string()));
    }
    file.workspaces.retain(|w| w.id != workspace_id);
    tombstone(&mut file, workspace_id);
    write_registry(&path, &file).map_err(AuthError::from)?;
    Ok(())
}

/// Delete a workspace *with* its data. Unlike [`delete`], a workspace that
/// still holds files is accepted: the registry row goes now — so the ownership
/// gate refuses every request for the scope from this moment — and the id is
/// queued for [`drain_pending_purges`], which removes the directory at the
/// next server start. The default workspace stays protected.
pub fn delete_and_schedule_purge(
    scopes_root: &Path,
    identity: &Identity,
    workspace_id: &str,
) -> Result<(), AuthError> {
    let path = registry_path(scopes_root, identity);
    let _lock = lock_registry(&path)?;
    let mut file = read_registry(&path)?;
    let Some(workspace) = file.workspaces.iter().find(|w| w.id == workspace_id) else {
        return Err(AuthError::UnownedWorkspace {
            principal: identity.scope_root.clone(),
            workspace: workspace_id.to_string(),
        });
    };
    if workspace.is_default {
        return Err(AuthError::DefaultWorkspaceProtected);
    }
    file.workspaces.retain(|w| w.id != workspace_id);
    tombstone(&mut file, workspace_id);
    if !file
        .pending_purge
        .iter()
        .any(|pending| pending == workspace_id)
    {
        file.pending_purge.push(workspace_id.to_string());
    }
    write_registry(&path, &file).map_err(AuthError::from)?;
    Ok(())
}

/// What happened to one queued purge at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurgeOutcome {
    /// The directory was removed (or was already absent).
    Removed {
        principal: String,
        workspace: String,
    },
    /// Dropped from the queue without touching disk: the id is not a valid
    /// workspace name, names the default, or is registered again.
    Skipped {
        principal: String,
        workspace: String,
        reason: &'static str,
    },
    /// Removal failed; the id stays queued for the next start.
    Failed {
        principal: String,
        workspace: String,
        error: String,
    },
}

/// Remove the directories of workspaces deleted with purge. Call only at
/// server start, after the runtime lease is held and BEFORE any store
/// discovers scopes: every boot sweep enumerates directories, so a directory
/// removed here cannot be resurrected, and nothing holds a handle into it yet.
/// Never call it from a CLI path — a CLI command can run beside a live server.
pub fn drain_pending_purges(scopes_root: &Path) -> Vec<PurgeOutcome> {
    let mut outcomes = Vec::new();
    let Ok(principals) = std::fs::read_dir(scopes_root) else {
        return outcomes;
    };
    for principal_entry in principals.flatten() {
        if !principal_entry
            .file_type()
            .map(|kind| kind.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        let principal = principal_entry.file_name().to_string_lossy().into_owned();
        let path = principal_entry.path().join(REGISTRY_FILE);
        if !path.exists() {
            continue;
        }
        let Ok(_lock) = lock_registry(&path) else {
            tracing::warn!(principal = %principal, "could not lock the workspace registry; purges stay queued");
            continue;
        };
        let Ok(mut file) = read_registry(&path) else {
            continue;
        };
        if file.pending_purge.is_empty() {
            continue;
        }
        let mut still_pending = Vec::new();
        for workspace in std::mem::take(&mut file.pending_purge) {
            // A hand-edited or corrupted queue must never become a path
            // outside this principal's scope directory.
            let skip = if validate_principal_name(&workspace).is_err() {
                Some("not a valid workspace name")
            } else if workspace == DEFAULT_WORKSPACE_ID {
                Some("the default workspace is never purged")
            } else if file.workspaces.iter().any(|w| w.id == workspace) {
                Some("registered again since the purge was queued")
            } else {
                None
            };
            if let Some(reason) = skip {
                outcomes.push(PurgeOutcome::Skipped {
                    principal: principal.clone(),
                    workspace,
                    reason,
                });
                continue;
            }
            let dir = principal_entry.path().join(&workspace);
            // The queue entry is the deletion evidence; it must outlive the
            // drain so store retirement (after the stores open) still sees it.
            tombstone(&mut file, &workspace);
            // `remove_dir_all` does not follow symlinks, so a linked bots or
            // skills directory inside the scope loses only the link.
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => outcomes.push(PurgeOutcome::Removed {
                    principal: principal.clone(),
                    workspace,
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    outcomes.push(PurgeOutcome::Removed {
                        principal: principal.clone(),
                        workspace,
                    })
                },
                Err(error) => {
                    outcomes.push(PurgeOutcome::Failed {
                        principal: principal.clone(),
                        workspace: workspace.clone(),
                        error: error.to_string(),
                    });
                    still_pending.push(workspace);
                },
            }
        }
        file.pending_purge = still_pending;
        if let Err(error) = write_registry(&path, &file) {
            tracing::warn!(principal = %principal, error = %error, "could not rewrite the workspace registry after draining purges");
        }
    }
    outcomes
}

/// The workspaces of one principal directory that are **live**: every
/// registered workspace, plus the default workspace (always — on a fresh
/// install it exists before anything has written its registry row), minus any
/// id queued in `pending_purge`. The default is never purged, so it stays live
/// even if a hand-edited queue names it.
///
/// A directory on disk is not evidence of a workspace. Deleted workspaces'
/// directories are recreated by writers that do not consult this registry, and
/// a boot sweep that walks directories admits, opens and indexes every one of
/// them for an owner who can no longer reach it. This is the single predicate
/// for "does this scope still belong to someone"; enumerate with it rather
/// than with a directory scan.
///
/// Read-only: unlike [`list`], a missing registry is not minted here. Ids that
/// are not valid workspace names are ignored, so a corrupted row can never
/// become a path. An unreadable registry is an error the caller must decide
/// how to fail on (see [`enumerate_live_scopes`], which keeps the directories).
pub fn live_workspaces(
    scopes_root: &Path,
    principal_dir: &str,
) -> Result<std::collections::BTreeSet<String>, AuthStoreError> {
    let path = registry_path_for_principal_dir(scopes_root, principal_dir);
    let present = path.exists();
    let file = read_registry(&path)?;
    let mut live = live_workspaces_in(&file);
    // `default` is invented only for a real principal: one with a registry,
    // or whose `default` directory already exists. A stray directory under
    // `scopes/` (or a reserved bucket such as `_quarantine`) is not given a
    // tenant workspace it never had.
    if !present && !default_dir_exists(scopes_root, principal_dir) {
        live.remove(DEFAULT_WORKSPACE_ID);
    }
    Ok(live)
}

fn default_dir_exists(scopes_root: &Path, principal_dir: &str) -> bool {
    scopes_root
        .join(principal_dir)
        .join(DEFAULT_WORKSPACE_ID)
        .is_dir()
}

/// Reserved principals own exactly one scope each — their sink pair — and
/// never a tenant workspace.
fn is_reserved_principal(principal_dir: &str) -> bool {
    use crate::magician_v2::transport_log::{QUARANTINE_PRINCIPAL, SYSTEM_PRINCIPAL};
    principal_dir == SYSTEM_PRINCIPAL || principal_dir == QUARANTINE_PRINCIPAL
}

fn live_workspaces_in(file: &WorkspacesFile) -> std::collections::BTreeSet<String> {
    let mut live = file
        .workspaces
        .iter()
        .map(|workspace| workspace.id.clone())
        .filter(|id| validate_principal_name(id).is_ok())
        .collect::<std::collections::BTreeSet<_>>();
    for pending in &file.pending_purge {
        live.remove(pending);
    }
    live.insert(DEFAULT_WORKSPACE_ID.to_string());
    live
}

/// Where one `(principal directory, workspace)` scope stands. The single
/// source behind [`is_live_workspace`], [`is_retired_workspace`] and
/// [`enumerate_live_scopes`], so they cannot disagree about a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeStanding {
    /// A reserved sink pair (`system`/`system`, `_quarantine`/`_quarantine`).
    Sink,
    /// Vouched for: registered and not queued for purge, or the default
    /// workspace of a real principal (one with a registry, or whose
    /// `default` directory exists).
    Live,
    /// The principal's registry exists, is readable, and TOMBSTONES this
    /// workspace: deleted through the registry (`deleted`) or queued in
    /// `pending_purge`. The only standing destructive retirement acts on.
    Deleted,
    /// The principal's registry exists and is readable but neither lists nor
    /// tombstones this workspace. Absence is not proof of deletion (a
    /// pre-registry workspace, a lost update), so this is only ever grounds to
    /// SKIP work or refuse to CREATE a missing directory — never to delete.
    Unlisted,
    /// The principal has no registry file, and this is not its (existing)
    /// default. Not live — nothing vouches for it — but not retired either:
    /// nothing was ever deleted through a registry that does not exist, so
    /// the gates that refuse deleted workspaces leave it alone (test
    /// fixtures and pre-registry data live here).
    Unregistered,
    /// The registry exists but cannot be read: fails open (counts as live).
    Unknown,
    /// Not a scope at all: an invalid principal or workspace name,
    /// `system/<other>`, or a reserved principal off its sink pair.
    NotAScope,
}

/// Classify one scope — see [`ScopeStanding`]. One `stat` (plus a parse only
/// when the registry changed; see [`cached_registry_liveness`]).
pub fn scope_standing(scopes_root: &Path, principal_dir: &str, workspace: &str) -> ScopeStanding {
    use crate::magician_v2::artifact_v2::workspace::{scope_profile, ScopeProfile};
    match scope_profile(principal_dir, workspace) {
        ScopeProfile::Invalid => return ScopeStanding::NotAScope,
        ScopeProfile::ReservedSink => return ScopeStanding::Sink,
        ScopeProfile::User => {},
    }
    if is_reserved_principal(principal_dir)
        || validate_principal_name(principal_dir).is_err()
        || validate_principal_name(workspace).is_err()
    {
        return ScopeStanding::NotAScope;
    }
    match cached_registry_liveness(&registry_path_for_principal_dir(scopes_root, principal_dir)) {
        RegistryLiveness::Live { live, tombstoned } => {
            if live.contains(workspace) {
                ScopeStanding::Live
            } else if tombstoned.contains(workspace) {
                ScopeStanding::Deleted
            } else {
                ScopeStanding::Unlisted
            }
        },
        RegistryLiveness::Unreadable => ScopeStanding::Unknown,
        RegistryLiveness::Missing => {
            if workspace == DEFAULT_WORKSPACE_ID && default_dir_exists(scopes_root, principal_dir) {
                ScopeStanding::Live
            } else {
                ScopeStanding::Unregistered
            }
        },
    }
}

/// Whether one `(principal directory, workspace)` scope is live — see
/// [`ScopeStanding`]: sinks, vouched workspaces, and (failing open) scopes
/// whose registry cannot be read. A principal without a registry has only its
/// existing `default` live; reserved principals off their sink pair, invalid
/// names and `system/<other>` are never live.
pub fn is_live_workspace(scopes_root: &Path, principal_dir: &str, workspace: &str) -> bool {
    match scope_standing(scopes_root, principal_dir, workspace) {
        ScopeStanding::Sink | ScopeStanding::Live => true,
        ScopeStanding::Unknown => {
            tracing::warn!(
                principal = principal_dir,
                workspace,
                "workspace registry is unreadable; treating the scope as live"
            );
            true
        },
        ScopeStanding::Deleted
        | ScopeStanding::Unlisted
        | ScopeStanding::Unregistered
        | ScopeStanding::NotAScope => false,
    }
}

/// What one principal's registry says, as cached by [`is_retired_workspace`].
#[derive(Debug, Clone)]
enum RegistryLiveness {
    /// No registry file: nothing was ever deleted through it.
    Missing,
    /// The file exists but cannot be read or parsed — or is implausible (see
    /// [`registry_is_implausible`]) and is therefore not trusted.
    Unreadable,
    Live {
        live: std::sync::Arc<std::collections::BTreeSet<String>>,
        /// `deleted` ∪ `pending_purge`, minus anything registered again.
        tombstoned: std::sync::Arc<std::collections::BTreeSet<String>>,
    },
}

/// A registry that parses but cannot be the real one: zero bytes, or no
/// `default` row while the principal has other workspace directories
/// (`ensure_default` always writes that row first). Trusting it would make
/// every listed-nowhere workspace look unvouched, so it fails open instead.
fn registry_is_implausible(path: &Path, bytes_len: u64, file: &WorkspacesFile) -> bool {
    if bytes_len == 0 {
        return true;
    }
    if file
        .workspaces
        .iter()
        .any(|workspace| workspace.id == DEFAULT_WORKSPACE_ID)
    {
        return false;
    }
    let Some(principal_path) = path.parent() else {
        return false;
    };
    std::fs::read_dir(principal_path)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry.path().is_dir() && entry.file_name().to_str() != Some(DEFAULT_WORKSPACE_ID)
            })
        })
        .unwrap_or(false)
}

/// `(mtime, len, inode)` — the registry is replaced by atomic rename, so a
/// rewrite changes the inode even when mtime granularity would not show it.
type RegistryStamp = (Option<std::time::SystemTime>, u64, u64);

fn registry_stamp(metadata: &std::fs::Metadata) -> RegistryStamp {
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt as _;
        metadata.ino()
    };
    #[cfg(not(unix))]
    let inode = 0;
    (metadata.modified().ok(), metadata.len(), inode)
}

/// Registry reads keyed by path and invalidated by [`RegistryStamp`]: the
/// retirement predicate runs inside directory sweeps and per-scope creators,
/// so each call costs one `stat`, and a parse only when the file changed.
fn cached_registry_liveness(path: &Path) -> RegistryLiveness {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (RegistryStamp, RegistryLiveness)>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Ok(mut guard) = cache.lock() {
                guard.remove(path);
            }
            return RegistryLiveness::Missing;
        },
        Err(_) => return RegistryLiveness::Unreadable,
    };
    let stamp = registry_stamp(&metadata);
    if let Ok(guard) = cache.lock() {
        if let Some((cached_stamp, liveness)) = guard.get(path) {
            if *cached_stamp == stamp {
                return liveness.clone();
            }
        }
    }
    let liveness = match read_registry(path) {
        Ok(file) if registry_is_implausible(path, metadata.len(), &file) => {
            tracing::warn!(
                path = %path.display(),
                "workspace registry is empty or has no default row beside other workspaces; failing open"
            );
            RegistryLiveness::Unreadable
        },
        Ok(file) => {
            let live = live_workspaces_in(&file);
            let tombstoned = file
                .deleted
                .iter()
                .chain(file.pending_purge.iter())
                .filter(|id| !live.contains(*id) || file.pending_purge.contains(*id))
                .cloned()
                .collect();
            RegistryLiveness::Live {
                live: std::sync::Arc::new(live),
                tombstoned: std::sync::Arc::new(tombstoned),
            }
        },
        Err(_) => RegistryLiveness::Unreadable,
    };
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_path_buf(), (stamp, liveness.clone()));
    }
    liveness
}

/// Whether background work must leave a scope alone: its principal's registry
/// exists, is readable and plausible, and does not vouch for it —
/// [`ScopeStanding::Deleted`] or [`ScopeStanding::Unlisted`].
///
/// NON-destructive gate only: directory sweeps skip the scope, lazy creators
/// refuse to create its missing directory, database-sourced worker lists drop
/// it. Never use it to delete anything — absence from the registry is not
/// proof of deletion; use [`is_deleted_workspace`] for that. A principal
/// without a registry ([`ScopeStanding::Unregistered`]) is not retired —
/// refusing its directories would only break fixtures and pre-registry data —
/// and an unreadable or implausible registry fails open. The default
/// workspace of a real principal and the reserved sinks are never retired.
/// Cheap: one `stat` per call.
pub fn is_retired_workspace(scopes_root: &Path, principal_dir: &str, workspace: &str) -> bool {
    matches!(
        scope_standing(scopes_root, principal_dir, workspace),
        ScopeStanding::Deleted | ScopeStanding::Unlisted
    )
}

/// Whether a scope was positively DELETED: its registry tombstones it
/// (`deleted`, or queued in `pending_purge`) — [`ScopeStanding::Deleted`].
/// The only predicate a destructive path (row, file, bearer, device, push
/// retirement; dropping persisted wakes) may act on.
pub fn is_deleted_workspace(scopes_root: &Path, principal_dir: &str, workspace: &str) -> bool {
    scope_standing(scopes_root, principal_dir, workspace) == ScopeStanding::Deleted
}

/// Log, once per process per scope and site, that work for a deleted workspace
/// was skipped. Every gate calls this so a sweep that meets the same retired
/// scope every tick does not flood the log.
pub fn note_retired_scope_skipped(principal: &str, workspace: &str, site: &'static str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static LOGGED: OnceLock<Mutex<HashSet<(String, String, &'static str)>>> = OnceLock::new();
    let logged = LOGGED.get_or_init(|| Mutex::new(HashSet::new()));
    let first = logged
        .lock()
        .map(|mut guard| guard.insert((principal.to_string(), workspace.to_string(), site)))
        .unwrap_or(false);
    if first {
        tracing::info!(
            principal,
            workspace,
            site,
            "skipping work for a deleted workspace"
        );
    }
}

/// Structural guard for shared per-scope creators: refuse to create a MISSING
/// `scopes/<principal>/<workspace>` directory for a retired workspace.
///
/// A deleted workspace's directory came back sixteen times because lazy
/// creators (`ensure_base_layout`, the mail store, the embeddings sink) were
/// fed scope ids from global state — the wake queue, the learning database —
/// and ran `create_dir_all` without asking whether the scope still belongs to
/// anyone. An existing directory is left alone (the delete→restart window is
/// gated by the sweeps, and a write into an existing directory does not
/// resurrect anything). Registry `create` and `ensure_default` do not pass
/// through here, so a re-created slug still materializes.
///
/// `scope_dir` must be `<...>/scopes/<principal>/<workspace>`; any other shape
/// passes. Never panics; logs once per scope.
pub fn guard_scope_dir_creation(scope_dir: &Path, site: &'static str) -> std::io::Result<()> {
    if scope_dir.exists() {
        return Ok(());
    }
    let Some((scopes_root, principal, workspace)) = split_scope_dir(scope_dir) else {
        return Ok(());
    };
    if is_retired_workspace(scopes_root, &principal, &workspace) {
        note_retired_scope_skipped(&principal, &workspace, site);
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "workspace {principal}/{workspace} was deleted; refusing to recreate its directory"
            ),
        ));
    }
    Ok(())
}

/// `<scopes_root>/<principal>/<workspace>` → its three parts, only when the
/// root is literally named `scopes`.
fn split_scope_dir(scope_dir: &Path) -> Option<(&Path, String, String)> {
    let workspace = scope_dir.file_name()?.to_str()?.to_string();
    let principal_path = scope_dir.parent()?;
    let principal = principal_path.file_name()?.to_str()?.to_string();
    let scopes_root = principal_path.parent()?;
    if scopes_root.file_name()?.to_str()? != "scopes" {
        return None;
    }
    Some((scopes_root, principal, workspace))
}

/// The DELETED (tombstoned) scopes among `scopes`, sorted and deduplicated —
/// see [`is_deleted_workspace`]. Used by the boot-time store retirement to find
/// rows a deleted workspace left in databases outside its directory.
pub fn deleted_scopes_among<I>(scopes_root: &Path, scopes: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut retired = scopes
        .into_iter()
        .filter(|(principal, workspace)| is_deleted_workspace(scopes_root, principal, workspace))
        .collect::<Vec<_>>();
    retired.sort();
    retired.dedup();
    retired
}

/// Every live scope under `scopes_root`, and the scope directories that are not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveScopeEnumeration {
    /// Live `(principal directory, workspace)` pairs, sorted. Includes each
    /// principal's default workspace and registered workspaces whose directory
    /// does not exist yet, and reserved sinks present on disk.
    pub live: Vec<(String, String)>,
    /// Scope directories on disk that no registry vouches for — residue of
    /// deleted workspaces. Never deleted here; callers skip and report them.
    pub residue: Vec<(String, String)>,
    /// Principal directories whose registry could not be read, with the error.
    /// Their directories are all counted live (fail open, see
    /// [`is_live_workspace`]).
    pub unreadable_registries: Vec<(String, String)>,
}

/// Enumerate live scopes per principal directory under `scopes_root`: the
/// registry's registered workspaces + the principal's default workspace, minus
/// `pending_purge` — every scope [`scope_standing`] calls live. Reserved sinks
/// on disk are live; a reserved principal contributes nothing else, and
/// `system/<other>` debris or a directory with an invalid name is neither live
/// nor residue (no enumeration ever treats it as a scope). A principal with no
/// registry gets `default` only when that directory already exists, so a stray
/// directory under `scopes/` is never handed a tenant workspace.
///
/// Principals are discovered by directory, so a principal with no directory at
/// all (the very first boot) contributes nothing; a caller that must always
/// cover the deployment's default scope adds it itself. Blocking: a directory
/// walk plus one small file read per principal.
pub fn enumerate_live_scopes(scopes_root: &Path) -> LiveScopeEnumeration {
    fn subdirs(path: &Path) -> Vec<String> {
        let mut dirs = std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.path().is_dir())
                    .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        dirs.sort();
        dirs
    }
    let mut enumeration = LiveScopeEnumeration::default();
    if !scopes_root.is_dir() {
        return enumeration;
    }
    for principal in subdirs(scopes_root) {
        let on_disk = subdirs(&scopes_root.join(&principal));
        if is_reserved_principal(&principal) {
            for workspace in on_disk {
                if scope_standing(scopes_root, &principal, &workspace) == ScopeStanding::Sink {
                    enumeration.live.push((principal.clone(), workspace));
                }
            }
            continue;
        }
        if validate_principal_name(&principal).is_err() {
            continue;
        }
        let registered = match live_workspaces(scopes_root, &principal) {
            Ok(live) => live,
            Err(error) => {
                enumeration
                    .unreadable_registries
                    .push((principal.clone(), error.to_string()));
                std::collections::BTreeSet::new()
            },
        };
        let mut live = std::collections::BTreeSet::new();
        for workspace in on_disk {
            match scope_standing(scopes_root, &principal, &workspace) {
                ScopeStanding::Sink | ScopeStanding::Live | ScopeStanding::Unknown => {
                    live.insert(workspace);
                },
                ScopeStanding::Deleted | ScopeStanding::Unlisted | ScopeStanding::Unregistered => {
                    enumeration.residue.push((principal.clone(), workspace));
                },
                ScopeStanding::NotAScope => {},
            }
        }
        // Registered workspaces whose directory does not exist yet.
        for workspace in registered {
            if scope_standing(scopes_root, &principal, &workspace) == ScopeStanding::Live {
                live.insert(workspace);
            }
        }
        enumeration.live.extend(
            live.into_iter()
                .map(|workspace| (principal.clone(), workspace)),
        );
    }
    enumeration
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::auth::credentials::CredentialKind;
    use crate::magician_v2::auth::AuthStore;

    fn identity_named(name: &str, scope_root: &str) -> Identity {
        Identity {
            name: name.to_string(),
            display_name: name.to_string(),
            scope_root: scope_root.to_string(),
            created_at: Utc::now(),
        }
    }

    fn setup() -> (tempfile::TempDir, Identity) {
        let dir = tempfile::tempdir().expect("tempdir");
        (dir, identity_named("owner", "anonymous"))
    }

    #[test]
    fn purge_accepts_a_workspace_with_data_and_removes_it_only_at_the_drain() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        create(&scopes, &identity, "eval-1", "Eval", None).expect("create");
        let data = scopes
            .join(&identity.scope_root)
            .join("eval-1")
            .join("notes");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("memory.md"), b"remembered").unwrap();

        // The guarded delete still refuses a workspace holding files.
        assert!(matches!(
            delete(&scopes, &identity, "eval-1"),
            Err(AuthError::WorkspaceHasLiveState(_))
        ));

        delete_and_schedule_purge(&scopes, &identity, "eval-1").expect("purge");
        // The row is gone at once, so the ownership gate refuses the scope ...
        assert!(!owns(&scopes, &identity, "eval-1").unwrap());
        // ... but the data stays until the next start drains it.
        assert!(
            data.join("memory.md").exists(),
            "a live purge would race the service"
        );

        let outcomes = drain_pending_purges(&scopes);
        assert_eq!(
            outcomes,
            vec![PurgeOutcome::Removed {
                principal: identity.scope_root.clone(),
                workspace: "eval-1".into()
            }]
        );
        assert!(!scopes.join(&identity.scope_root).join("eval-1").exists());
        assert!(scopes
            .join(&identity.scope_root)
            .join(REGISTRY_FILE)
            .exists());
        // The queue is empty afterwards; a second drain does nothing.
        assert!(drain_pending_purges(&scopes).is_empty());
    }

    /// A purge queued without a tombstone (hand-queued residue, or a queue
    /// written before tombstones existed) keeps its deletion evidence after the
    /// drain empties the queue, so store retirement still finds it.
    #[test]
    fn a_drained_purge_leaves_its_tombstone() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        let path = registry_path(&scopes, &identity);
        let mut file = read_registry(&path).unwrap();
        file.pending_purge.push("old-eval".into());
        write_registry(&path, &file).unwrap();
        std::fs::create_dir_all(scopes.join(&identity.scope_root).join("old-eval")).unwrap();

        drain_pending_purges(&scopes);
        let file = read_registry(&path).unwrap();
        assert!(file.pending_purge.is_empty());
        assert_eq!(file.deleted, vec!["old-eval".to_string()]);
        assert!(is_deleted_workspace(
            &scopes,
            &identity.scope_root,
            "old-eval"
        ));
    }

    #[test]
    fn purge_never_touches_the_default_workspace() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        assert!(matches!(
            delete_and_schedule_purge(&scopes, &identity, DEFAULT_WORKSPACE_ID),
            Err(AuthError::DefaultWorkspaceProtected)
        ));
        // Even a hand-edited queue naming it is skipped, not obeyed.
        let path = registry_path(&scopes, &identity);
        let mut file = read_registry(&path).unwrap();
        file.pending_purge.push(DEFAULT_WORKSPACE_ID.into());
        write_registry(&path, &file).unwrap();
        let default_dir = scopes.join(&identity.scope_root).join(DEFAULT_WORKSPACE_ID);
        std::fs::create_dir_all(&default_dir).unwrap();
        std::fs::write(default_dir.join("keep"), b"x").unwrap();
        let outcomes = drain_pending_purges(&scopes);
        assert!(matches!(
            outcomes.as_slice(),
            [PurgeOutcome::Skipped { .. }]
        ));
        assert!(default_dir.join("keep").exists());
    }

    #[test]
    fn a_hand_edited_queue_cannot_escape_the_scope_directory() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious"), b"x").unwrap();
        let path = registry_path(&scopes, &identity);
        let mut file = read_registry(&path).unwrap();
        file.pending_purge.push("../../outside".into());
        write_registry(&path, &file).unwrap();
        let outcomes = drain_pending_purges(&scopes);
        assert!(matches!(
            outcomes.as_slice(),
            [PurgeOutcome::Skipped { .. }]
        ));
        assert!(outside.join("precious").exists());
    }

    #[test]
    fn a_slug_waiting_to_be_purged_cannot_be_recreated_until_the_drain() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        create(&scopes, &identity, "eval-2", "Eval", None).expect("create");
        let old = scopes.join(&identity.scope_root).join("eval-2");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("old-memory"), b"x").unwrap();
        delete_and_schedule_purge(&scopes, &identity, "eval-2").expect("purge");

        // Recreating now would inherit the deleted workspace's data.
        assert!(matches!(
            create(&scopes, &identity, "eval-2", "Eval again", None),
            Err(AuthError::WorkspacePendingPurge(_))
        ));
        drain_pending_purges(&scopes);
        create(&scopes, &identity, "eval-2", "Eval again", None).expect("fresh after the drain");
        assert!(!old.join("old-memory").exists());
    }

    #[test]
    fn ensure_default_mints_personal_once() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        let first = ensure_default(&scopes, &identity).expect("ensure");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, "default");
        assert!(first[0].is_default);
        assert_eq!(first[0].display_name, "Personal");
        let again = ensure_default(&scopes, &identity).expect("ensure again");
        assert_eq!(again.len(), 1, "default is minted exactly once");
    }

    #[test]
    fn create_validates_slugs_and_refuses_duplicates() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).unwrap();
        assert!(matches!(
            create(&scopes, &identity, "../etc", "x", None),
            Err(AuthError::InvalidPrincipalName(_))
        ));
        assert!(matches!(
            create(&scopes, &identity, "default", "x", None),
            Err(AuthError::WorkspaceExists(_))
        ));
        let second = create(&scopes, &identity, "company", "Company", None).expect("create");
        assert!(!second.is_default);
        assert!(owns(&scopes, &identity, "company").unwrap());
    }

    #[test]
    fn default_cannot_be_deleted_or_renamed_but_can_be_described() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).unwrap();
        assert!(matches!(
            delete(&scopes, &identity, "default"),
            Err(AuthError::DefaultWorkspaceProtected)
        ));
        let updated = update(
            &scopes,
            &identity,
            "default",
            Some("Mine".into()),
            Some(None),
        )
        .expect("update");
        assert_eq!(updated.display_name, "Mine");
        assert_eq!(updated.id, "default", "slug is immutable");
    }

    #[test]
    fn deletion_refuses_live_state_and_removes_empty() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).unwrap();
        create(&scopes, &identity, "scratch", "Scratch", None).unwrap();
        // Live content blocks deletion.
        let ws_dir = scopes.join("anonymous").join("scratch");
        std::fs::create_dir_all(ws_dir.join("tasks")).unwrap();
        std::fs::write(ws_dir.join("tasks").join("x.json"), b"{}").unwrap();
        assert!(matches!(
            delete(&scopes, &identity, "scratch"),
            Err(AuthError::WorkspaceHasLiveState(_))
        ));
        // Cleared by hand → deletable.
        std::fs::remove_file(ws_dir.join("tasks").join("x.json")).unwrap();
        delete(&scopes, &identity, "scratch").expect("delete");
        assert!(!owns(&scopes, &identity, "scratch").unwrap());
    }

    #[test]
    fn from_session_property_denies_unowned_workspace() {
        let (dir, identity) = setup();
        let store = AuthStore::open(dir.path()).unwrap();
        let _ = store;
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).unwrap();
        create(&scopes, &identity, "company", "Company", None).unwrap();
        let scope =
            crate::magician_v2::auth::ScopeRef::from_session(&scopes, &identity, Some("company"))
                .expect("owned");
        assert_eq!(scope.principal(), "anonymous");
        assert_eq!(scope.workspace(), "company");
        assert!(matches!(
            crate::magician_v2::auth::ScopeRef::from_session(&scopes, &identity, Some("eve-ws")),
            Err(AuthError::UnownedWorkspace { .. })
        ));
        // Absent selector lands on default.
        let default_scope =
            crate::magician_v2::auth::ScopeRef::from_session(&scopes, &identity, None).unwrap();
        assert_eq!(default_scope.workspace(), "default");
    }

    /// A fresh install has scope directories before any registry row exists:
    /// the default workspace is live anyway, and nothing else is.
    #[test]
    fn the_default_workspace_is_live_without_a_registry() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = dir.path().join("scopes");
        std::fs::create_dir_all(scopes.join("anonymous").join("default")).unwrap();
        std::fs::create_dir_all(scopes.join("anonymous").join("stray")).unwrap();

        let live = live_workspaces(&scopes, "anonymous").expect("absent registry reads empty");
        assert_eq!(live.into_iter().collect::<Vec<_>>(), vec!["default"]);
        assert!(is_live_workspace(&scopes, "anonymous", "default"));
        assert!(!is_live_workspace(&scopes, "anonymous", "stray"));
        assert!(
            !registry_path_for_principal_dir(&scopes, "anonymous").exists(),
            "the predicate is read-only and must not mint a registry"
        );

        let enumeration = enumerate_live_scopes(&scopes);
        assert_eq!(
            enumeration.live,
            vec![("anonymous".to_string(), "default".to_string())]
        );
        assert_eq!(
            enumeration.residue,
            vec![("anonymous".to_string(), "stray".to_string())]
        );
    }

    /// Enumeration is registry-driven: a registered workspace is live even
    /// before its directory exists, a deleted one's leftover directory is
    /// residue, and an id queued for purge is never live.
    #[test]
    fn enumeration_follows_the_registry_and_excludes_pending_purges() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        create(&scopes, &identity, "work", "Work", None).expect("create work");
        create(&scopes, &identity, "fresh", "Fresh", None).expect("create fresh");
        create(&scopes, &identity, "eval-1", "Eval", None).expect("create eval");
        for workspace in ["default", "work", "eval-1", "deleted-long-ago"] {
            std::fs::create_dir_all(scopes.join("anonymous").join(workspace)).unwrap();
        }
        delete_and_schedule_purge(&scopes, &identity, "eval-1").expect("purge");
        // A second principal with its own registry is enumerated independently.
        let alice = identity_named("alice", "alice");
        ensure_default(&scopes, &alice).expect("alice default");
        std::fs::create_dir_all(scopes.join("alice").join("default")).unwrap();
        std::fs::create_dir_all(scopes.join("alice").join("work")).unwrap();
        // Reserved sinks are live; `system/<other>` is never a scope.
        std::fs::create_dir_all(scopes.join("system").join("system")).unwrap();
        std::fs::create_dir_all(scopes.join("system").join("default")).unwrap();

        let enumeration = enumerate_live_scopes(&scopes);
        let pair = |p: &str, w: &str| (p.to_string(), w.to_string());
        assert_eq!(
            enumeration.live,
            vec![
                pair("alice", "default"),
                pair("anonymous", "default"),
                pair("anonymous", "fresh"),
                pair("anonymous", "work"),
                pair("system", "system"),
            ]
        );
        assert_eq!(
            enumeration.residue,
            vec![
                pair("alice", "work"),
                pair("anonymous", "deleted-long-ago"),
                pair("anonymous", "eval-1"),
            ]
        );
        assert!(enumeration.unreadable_registries.is_empty());
        assert!(!is_live_workspace(&scopes, "anonymous", "eval-1"));
        assert!(is_live_workspace(&scopes, "anonymous", "fresh"));
        assert!(is_live_workspace(&scopes, "system", "system"));
        assert!(!is_live_workspace(&scopes, "system", "default"));
    }

    /// An unreadable registry fails open: its directories stay live rather
    /// than a parse error hiding a tenant's data.
    #[test]
    fn an_unreadable_registry_keeps_every_directory_live() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = dir.path().join("scopes");
        std::fs::create_dir_all(scopes.join("anonymous").join("work")).unwrap();
        std::fs::write(
            registry_path_for_principal_dir(&scopes, "anonymous"),
            b"{ not json",
        )
        .unwrap();

        let enumeration = enumerate_live_scopes(&scopes);
        assert!(enumeration
            .live
            .contains(&("anonymous".to_string(), "work".to_string())));
        assert!(enumeration.residue.is_empty());
        assert_eq!(enumeration.unreadable_registries.len(), 1);
        assert!(is_live_workspace(&scopes, "anonymous", "work"));
    }

    /// A reserved bucket or a stray directory under `scopes/` is never handed
    /// a `default` tenant workspace: only the exact sink pair is live for a
    /// reserved principal, and a principal with no registry and no `default`
    /// directory contributes nothing live.
    #[test]
    fn reserved_and_stray_principals_are_not_given_a_default_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = dir.path().join("scopes");
        std::fs::create_dir_all(scopes.join("_quarantine").join("_quarantine")).unwrap();
        std::fs::create_dir_all(scopes.join("_quarantine").join("default")).unwrap();
        std::fs::create_dir_all(scopes.join("system").join("system")).unwrap();
        std::fs::create_dir_all(scopes.join("tmp").join("junk")).unwrap();
        std::fs::create_dir_all(scopes.join("Bad Name").join("default")).unwrap();

        let enumeration = enumerate_live_scopes(&scopes);
        let pair = |p: &str, w: &str| (p.to_string(), w.to_string());
        assert_eq!(
            enumeration.live,
            vec![pair("_quarantine", "_quarantine"), pair("system", "system")]
        );
        assert_eq!(enumeration.residue, vec![pair("tmp", "junk")]);
        assert!(!scopes.join("tmp").join("default").exists());
        assert!(live_workspaces(&scopes, "tmp").unwrap().is_empty());

        assert!(!is_live_workspace(&scopes, "_quarantine", "default"));
        assert!(!is_live_workspace(&scopes, "system", "default"));
        assert!(!is_live_workspace(&scopes, "tmp", "default"));
        assert!(!is_live_workspace(&scopes, "tmp", "junk"));
        assert!(is_live_workspace(&scopes, "_quarantine", "_quarantine"));
        assert!(is_live_workspace(&scopes, "system", "system"));
        assert_eq!(
            scope_standing(&scopes, "_quarantine", "default"),
            ScopeStanding::NotAScope
        );
        assert_eq!(
            scope_standing(&scopes, "tmp", "junk"),
            ScopeStanding::Unregistered
        );
        // Neither stray nor reserved scopes count as deleted workspaces.
        for (principal, workspace) in [
            ("_quarantine", "default"),
            ("_quarantine", "_quarantine"),
            ("system", "system"),
            ("tmp", "junk"),
            ("tmp", "default"),
        ] {
            assert!(!is_retired_workspace(&scopes, principal, workspace));
        }
    }

    /// The liveness and retirement gates are one classification: every
    /// standing maps to exactly one answer for each.
    #[test]
    fn liveness_and_retirement_derive_from_one_standing() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        create(&scopes, &identity, "work", "Work", None).expect("create");
        create(&scopes, &identity, "gone", "Gone", None).expect("create");
        delete_and_schedule_purge(&scopes, &identity, "gone").expect("purge");
        // A second principal whose registry is unreadable.
        std::fs::create_dir_all(scopes.join("broken").join("work")).unwrap();
        std::fs::write(
            registry_path_for_principal_dir(&scopes, "broken"),
            b"{ not json",
        )
        .unwrap();

        let cases = [
            // (principal, workspace, standing, live, retired, deleted)
            (
                "anonymous",
                "default",
                ScopeStanding::Live,
                true,
                false,
                false,
            ),
            ("anonymous", "work", ScopeStanding::Live, true, false, false),
            (
                "anonymous",
                "gone",
                ScopeStanding::Deleted,
                false,
                true,
                true,
            ),
            (
                "anonymous",
                "never",
                ScopeStanding::Unlisted,
                false,
                true,
                false,
            ),
            ("broken", "work", ScopeStanding::Unknown, true, false, false),
            ("system", "system", ScopeStanding::Sink, true, false, false),
            (
                "system",
                "default",
                ScopeStanding::NotAScope,
                false,
                false,
                false,
            ),
            (
                "nobody",
                "work",
                ScopeStanding::Unregistered,
                false,
                false,
                false,
            ),
        ];
        for (principal, workspace, standing, live, retired, deleted) in cases {
            assert_eq!(
                scope_standing(&scopes, principal, workspace),
                standing,
                "{principal}/{workspace}"
            );
            assert_eq!(is_live_workspace(&scopes, principal, workspace), live);
            assert_eq!(is_retired_workspace(&scopes, principal, workspace), retired);
            assert_eq!(is_deleted_workspace(&scopes, principal, workspace), deleted);
        }
        assert_eq!(
            deleted_scopes_among(
                &scopes,
                [
                    ("anonymous".to_string(), "gone".to_string()),
                    ("anonymous".to_string(), "work".to_string()),
                    ("anonymous".to_string(), "never".to_string()),
                    ("anonymous".to_string(), "gone".to_string()),
                ]
            ),
            vec![("anonymous".to_string(), "gone".to_string())]
        );
    }

    /// Delete and purge write a tombstone; the drain keeps it (so boot can
    /// still retire rows); re-creating the slug clears it.
    #[test]
    fn tombstones_are_written_by_delete_and_purge_and_cleared_by_create() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        create(&scopes, &identity, "plain", "Plain", None).expect("create");
        create(&scopes, &identity, "purged", "Purged", None).expect("create");
        delete(&scopes, &identity, "plain").expect("delete");
        delete_and_schedule_purge(&scopes, &identity, "purged").expect("purge");
        let file = read_registry(&registry_path(&scopes, &identity)).unwrap();
        assert_eq!(
            file.deleted,
            vec!["plain".to_string(), "purged".to_string()]
        );
        assert!(is_deleted_workspace(&scopes, "anonymous", "plain"));
        assert!(is_deleted_workspace(&scopes, "anonymous", "purged"));

        drain_pending_purges(&scopes);
        assert!(is_deleted_workspace(&scopes, "anonymous", "purged"));

        create(&scopes, &identity, "purged", "Again", None).expect("re-create");
        create(&scopes, &identity, "plain", "Again", None).expect("re-create");
        let file = read_registry(&registry_path(&scopes, &identity)).unwrap();
        assert!(file.deleted.is_empty());
        assert!(is_live_workspace(&scopes, "anonymous", "purged"));
        assert!(!is_deleted_workspace(&scopes, "anonymous", "plain"));
    }

    /// An empty registry, or one with no `default` row beside other workspace
    /// directories, is implausible: it fails open (live, never retired).
    #[test]
    fn an_implausible_registry_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = dir.path().join("scopes");
        std::fs::create_dir_all(scopes.join("anonymous").join("work")).unwrap();
        let path = registry_path_for_principal_dir(&scopes, "anonymous");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            scope_standing(&scopes, "anonymous", "work"),
            ScopeStanding::Unknown
        );
        assert!(!is_retired_workspace(&scopes, "anonymous", "work"));

        std::fs::write(
            &path,
            br#"{"schema_version":1,"workspaces":[],"deleted":["work"]}"#,
        )
        .unwrap();
        assert_eq!(
            scope_standing(&scopes, "anonymous", "work"),
            ScopeStanding::Unknown
        );
        assert!(!is_deleted_workspace(&scopes, "anonymous", "work"));
    }

    /// Registry writes are serialized: concurrent creates and deletes on one
    /// principal never lose each other's rows.
    #[test]
    fn concurrent_registry_mutations_keep_every_effect() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        for n in 0..8 {
            create(&scopes, &identity, &format!("old-{n}"), "Old", None).expect("create");
        }
        std::thread::scope(|threads| {
            for n in 0..8 {
                let scopes = scopes.clone();
                let identity = identity.clone();
                threads.spawn(move || {
                    create(&scopes, &identity, &format!("new-{n}"), "New", None)
                        .expect("concurrent create");
                    delete(&scopes, &identity, &format!("old-{n}")).expect("concurrent delete");
                });
            }
        });
        let file = read_registry(&registry_path(&scopes, &identity)).unwrap();
        let ids = file
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for n in 0..8 {
            assert!(ids.contains(&format!("new-{n}")), "lost create new-{n}");
            assert!(!ids.contains(&format!("old-{n}")), "lost delete old-{n}");
            assert!(file.deleted.contains(&format!("old-{n}")));
        }
    }

    /// The structural creator guard refuses to re-create a deleted
    /// workspace's missing directory, leaves an existing one alone, and never
    /// blocks a live or unregistered scope.
    #[test]
    fn the_creation_guard_refuses_only_missing_retired_scope_dirs() {
        let (dir, identity) = setup();
        let scopes = dir.path().join("scopes");
        ensure_default(&scopes, &identity).expect("ensure");
        create(&scopes, &identity, "gone", "Gone", None).expect("create");
        delete_and_schedule_purge(&scopes, &identity, "gone").expect("purge");
        drain_pending_purges(&scopes);

        let gone = scopes.join("anonymous").join("gone");
        assert!(guard_scope_dir_creation(&gone, "test").is_err());
        assert!(
            guard_scope_dir_creation(&scopes.join("anonymous").join("default"), "test").is_ok()
        );
        assert!(guard_scope_dir_creation(&scopes.join("fixture").join("ws"), "test").is_ok());
        // Not a scope path at all: passes.
        assert!(guard_scope_dir_creation(&dir.path().join("elsewhere").join("x"), "test").is_ok());
        // Re-creating the slug through the registry makes it live again.
        create(&scopes, &identity, "gone", "Gone again", None).expect("re-create");
        assert!(guard_scope_dir_creation(&gone, "test").is_ok());
    }

    #[test]
    fn create_identity_mints_the_default_registry_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::open(dir.path()).unwrap();
        let identity = store
            .create_identity(
                "owner",
                "Alex",
                CredentialKind::Password { hash: "x".into() },
            )
            .unwrap();
        let scopes = dir.path().join("scopes");
        let workspaces = list(&scopes, &identity).unwrap();
        assert!(workspaces.iter().any(|w| w.id == "default" && w.is_default));
    }
}
