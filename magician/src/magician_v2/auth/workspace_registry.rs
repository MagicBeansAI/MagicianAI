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
}

/// `scopes/<principal-dir>/workspaces.json` for the identity's scope root.
pub fn registry_path(scopes_root: &Path, identity: &Identity) -> PathBuf {
    scopes_root.join(&identity.scope_root).join(REGISTRY_FILE)
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
    let mut file = read_registry(&path)?;
    if !file
        .workspaces
        .iter()
        .any(|workspace| workspace.id == DEFAULT_WORKSPACE_ID)
    {
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
