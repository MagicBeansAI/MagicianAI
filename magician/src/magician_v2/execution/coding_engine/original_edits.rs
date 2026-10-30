//! Pre-write, revision-pinned edits for original-folder coding runs.
//! The engine can read the real checkout, but its OS fence forbids writes;
//! only this broker applies the exact transaction reviewed by the owner.
use super::super::file_edit::{
    snapshot::{apply_transaction_verified_decision, SnapshotStore, MAX_FILE_READ_BYTES},
    transaction::{
        FileEditStatus, FileEditTransaction, ProposedEdit, TransactionId, TransactionScope,
        TransactionStore,
    },
};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, LazyLock},
};
use tokio::sync::Mutex;

pub struct EditSession {
    pub principal: String,
    pub workspace: String,
    pub root_task_id: String,
    pub task_id: String,
    pub execution_id: String,
    pub scope_root: PathBuf,
    pub repo: PathBuf,
    pub agent_id: Option<String>,
}

static SESSIONS: LazyLock<std::sync::Mutex<HashMap<String, Arc<Mutex<EditSession>>>>> =
    LazyLock::new(Default::default);

pub struct EditLease(String);

impl Drop for EditLease {
    fn drop(&mut self) {
        SESSIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

pub fn register(token: &str, session: EditSession) -> EditLease {
    SESSIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(token.into(), Arc::new(Mutex::new(session)));
    EditLease(token.into())
}

pub fn revoke(token: &str) {
    SESSIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(token);
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditRequest {
    pub rationale: String,
    pub files: Vec<EditFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditFile {
    pub path: PathBuf,
    /// Full expected old text; null means create. Never silently overwrite
    /// edits made by the owner since the engine last read the file.
    pub before: Option<String>,
    /// Full new text; null means delete.
    pub after: Option<String>,
}

fn target(repo: &Path, path: &Path) -> Result<PathBuf> {
    if repo.canonicalize()? != repo {
        bail!("the original folder identity changed; restart the run");
    }
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("edit paths must be relative, without traversal");
    }
    if path
        .components()
        .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
    {
        bail!("repository metadata cannot be changed through the edit broker");
    }
    let mut full = repo.to_path_buf();
    for part in path.components() {
        full.push(part);
        match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("edit paths cannot traverse symlinks")
            },
            Ok(meta) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if meta.is_file() && meta.nlink() > 1 {
                        bail!("hard-linked files require a separate edit outside this broker");
                    }
                }
                #[cfg(not(unix))]
                let _ = meta;
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e.into()),
        }
    }
    Ok(full)
}

fn read(repo: &Path, path: &Path) -> Result<Option<String>> {
    let path = target(repo, path)?;
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > MAX_FILE_READ_BYTES => bail!("file is too large for diff review"),
        Ok(_) => Ok(Some(std::fs::read_to_string(path)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn proposed(repo: &Path, files: &[EditFile]) -> Result<Vec<ProposedEdit>> {
    if files.is_empty() || files.len() > 100 {
        bail!("supply between 1 and 100 file edits");
    }
    let mut seen = std::collections::HashSet::new();
    files
        .iter()
        .map(|file| {
            if !seen.insert(file.path.clone()) {
                bail!("duplicate edit path");
            }
            if file
                .after
                .as_ref()
                .is_some_and(|text| text.len() as u64 > MAX_FILE_READ_BYTES)
            {
                bail!("new file text is too large for diff review");
            }
            if read(repo, &file.path)? != file.before {
                bail!(
                    "{} changed; read it again and re-stage",
                    file.path.display()
                );
            }
            match (&file.before, &file.after) {
                (None, Some(content)) => Ok(ProposedEdit::Create {
                    path: file.path.clone(),
                    content: content.clone(),
                }),
                (Some(_), Some(new_content)) => Ok(ProposedEdit::Modify {
                    path: file.path.clone(),
                    new_content: new_content.clone(),
                }),
                (Some(_), None) => Ok(ProposedEdit::Delete {
                    path: file.path.clone(),
                }),
                (None, None) => bail!("an edit must create, modify or delete a file"),
            }
        })
        .collect()
}

pub async fn edit(token: &str, request: EditRequest) -> Result<Value> {
    let session = SESSIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(token)
        .cloned()
        .ok_or_else(|| anyhow!("this run has no writable original-folder session"))?;
    let session_guard = session.lock().await;
    let s = &*session_guard;
    let edits = proposed(&s.repo, &request.files)?;
    let store = TransactionStore::new(&s.scope_root);
    let mut transaction = store.stage(
        TransactionScope {
            principal: s.principal.clone(),
            workspace: s.workspace.clone(),
        },
        &request.rationale,
        edits,
        &mut |path| read(&s.repo, path)?.ok_or_else(|| anyhow!("file disappeared")),
    )?;
    transaction.task_id = Some(s.task_id.clone());
    transaction.execution_id = Some(s.execution_id.clone());
    transaction.staged_by_agent = s.agent_id.clone();
    store.persist(&transaction)?;
    let index = history_path(&s.scope_root, &s.root_task_id);
    std::fs::create_dir_all(index.parent().unwrap())?;
    let mut ids: Vec<String> = match std::fs::read(&index) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    ids.push(transaction.id.as_str().into());
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(
        &index,
        &serde_json::to_vec(&ids)?,
    )?;
    let revision = transaction.content_hash();
    let paths: Vec<_> = request.files.iter().map(|f| s.repo.join(&f.path)).collect();
    let mut files = serde_json::to_value(transaction.diff_approval_files())?;
    for (file, path) in files.as_array_mut().unwrap().iter_mut().zip(&paths) {
        file["path"] = json!(path);
    }
    if serde_json::to_vec(&files)?.len() > 256 * 1024 {
        let _ = store.reject(&transaction.id);
        bail!("this diff is too large for one approval card; split it into smaller edits");
    }
    let result = async {
        crate::magician_v2::god_mode::authorize_repository_edit(
            &s.principal,
            &s.workspace,
            &s.root_task_id,
            &s.task_id,
            &s.execution_id,
            &s.repo,
            &paths,
            &request.rationale,
            &files,
        )
        .await
        .map_err(anyhow::Error::msg)?;
        let active_sessions = SESSIONS.lock().unwrap_or_else(|p| p.into_inner());
        if !active_sessions
            .get(token)
            .is_some_and(|active| Arc::ptr_eq(active, &session))
        {
            bail!("the run ended before this edit was approved");
        }
        // Recheck both path identities and full preimages after the approval wait.
        proposed(&s.repo, &request.files)?;
        let decision = apply_transaction_verified_decision(
            &store,
            &SnapshotStore::new(&s.scope_root),
            &transaction.id,
            &s.repo,
            revision,
        )?;
        if !decision.transitioned() {
            bail!("this edit was already resolved");
        }
        Ok(json!({"ok":true, "transaction_id":transaction.id, "files":files}))
    }
    .await;
    if result.is_err() {
        let _ = store.reject(&transaction.id);
    }
    result
}

pub const HELPER: &str = include_str!("../../../../assets/original_repo_edit.py");

pub fn apply_env(request: &super::CodingEngineRequest, command: &mut tokio::process::Command) {
    if request.original_repo.is_some() {
        for key in ["MAGICIAN_CITIZEN_URL", "MAGICIAN_CITIZEN_TOKEN"] {
            if let Some(value) = request.env.get(key) {
                command.env(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::{
        god_mode,
        user_requests::service::{UserRequestService, UserResponse},
    };

    fn edit_request(before: &str, after: &str) -> EditRequest {
        EditRequest {
            rationale: "Test the exact edit".into(),
            files: vec![EditFile {
                path: "note.txt".into(),
                before: Some(before.into()),
                after: Some(after.into()),
            }],
        }
    }

    async fn owner(
        service: &UserRequestService,
        path: &Path,
        decision: &str,
        replace: Option<&str>,
    ) {
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(request) = service.list_pending().await.into_iter().next() {
                    break request;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("diff approval");
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "before\n",
            "real file changed before approval"
        );
        assert_eq!(request.context["vibedev_edit"], true);
        assert!(request.context["files"][0]["unified_diff"]
            .as_str()
            .unwrap()
            .contains("+approved"));
        for choice in [
            "allow_once",
            "session:file",
            "always:file",
            "session:repository",
            "deny",
        ] {
            assert!(request.options.iter().any(|option| option.id == choice));
        }
        if let Some(text) = replace {
            std::fs::write(path, text).unwrap();
        }
        assert!(
            service
                .respond(UserResponse {
                    request_id: request.id,
                    decision: decision.into(),
                    input: None,
                    channel: "web".into(),
                    sensitive: Vec::new()
                })
                .await
        );
    }

    #[tokio::test]
    async fn exact_diff_approval_deny_scoped_grants_and_stale_preimages_without_god_mode() {
        let _god = god_mode::test_scope(false);
        let _roots =
            crate::magician_v2::execution::goal_workspace_access::install_test_roots(Vec::new());
        for (decision, replace) in [
            ("allow_once", None),
            ("deny", None),
            ("session:file", None),
            ("always:file", None),
            ("allow_once", Some("owner changed it\n")),
        ] {
            let scope = tempfile::tempdir().unwrap();
            let repo = tempfile::tempdir().unwrap();
            let repo = repo.path().canonicalize().unwrap();
            let file = repo.join("note.txt");
            std::fs::write(&file, "before\n").unwrap();
            let token = uuid::Uuid::new_v4().to_string();
            let lease = register(
                &token,
                EditSession {
                    principal: "owner".into(),
                    workspace: "default".into(),
                    root_task_id: "root".into(),
                    task_id: "task".into(),
                    execution_id: "execution".into(),
                    scope_root: scope.path().into(),
                    repo: repo.clone(),
                    agent_id: None,
                },
            );
            let service = Arc::new(UserRequestService::new(Arc::new(
                crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(64),
            )));
            god_mode::with_test_approver(service.clone(), async {
                let (result, ()) = tokio::join!(
                    edit(&token, edit_request("before\n", "approved\n")),
                    owner(&service, &file, decision, replace)
                );
                let applied = decision != "deny" && replace.is_none();
                assert_eq!(result.is_ok(), applied, "{result:?}");
                assert_eq!(
                    std::fs::read_to_string(&file).unwrap(),
                    replace.unwrap_or(if applied { "approved\n" } else { "before\n" })
                );
                assert_eq!(
                    applied_history(scope.path(), "root").len(),
                    if applied { 1 } else { 0 }
                );
                if decision.starts_with("session:") || decision.starts_with("always:") {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        edit(&token, edit_request("approved\n", "second\n")),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert!(service.list_pending().await.is_empty());
                    assert_eq!(std::fs::read_to_string(&file).unwrap(), "second\n");
                }
            })
            .await;
            drop(lease);
            assert!(edit(&token, edit_request("approved\n", "expired\n"))
                .await
                .is_err());
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn super_god_native_process_cannot_write_original_folder() {
        if !super::super::os_sandbox_available() {
            return;
        }
        let _roots =
            crate::magician_v2::execution::goal_workspace_access::install_test_roots(Vec::new());
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let repo_path = repo.path().canonicalize().unwrap();
        let file = repo_path.join("note.txt");
        std::fs::write(&file, "before\n").unwrap();
        let mut request = super::super::CodingEngineRequest::new(
            "test",
            &repo_path,
            &repo_path,
            outside.path(),
            TransactionScope {
                principal: "owner".into(),
                workspace: "default".into(),
            },
        );
        request.original_repo = Some(repo_path);
        let output = super::super::with_coding_context(
            true,
            super::super::with_harness_fence(super::super::HarnessFence::Unrestricted, async {
                let args = [
                    "-c".into(),
                    "printf forbidden > \"$1\"; printf allowed > \"$2\"".into(),
                    "test".into(),
                    file.as_os_str().into(),
                    outside.path().join("allowed.txt").into_os_string(),
                ];
                let mut command = super::super::harness_command(
                    super::super::CodingEngineKind::Pi,
                    Path::new("/bin/sh"),
                    &args,
                    &request,
                    false,
                );
                command.command.output().await.unwrap()
            }),
        )
        .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_to_string(file).unwrap(), "before\n");
        assert_eq!(
            std::fs::read_to_string(outside.path().join("allowed.txt")).unwrap(),
            "allowed"
        );
    }
    #[test]
    fn refuses_stale_preimages_traversal_metadata_and_symlinks() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        assert!(target(&root, Path::new("valid.txt")).is_ok());
        std::fs::write(repo.path().join("a.txt"), "owner edit").unwrap();
        assert!(proposed(
            &root,
            &[EditFile {
                path: "a.txt".into(),
                before: Some("old".into()),
                after: Some("new".into())
            }]
        )
        .is_err());
        assert!(target(&root, Path::new("../a")).is_err());
        assert!(target(&root, Path::new(".git/config")).is_err());
        assert!(target(&root, Path::new(".GIT/config")).is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), repo.path().join("link")).unwrap();
            assert!(target(&root, Path::new("link/file")).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
            "owner edit"
        );
    }
}

fn history_path(scope_root: &Path, root_task_id: &str) -> PathBuf {
    // Root identity comes from the admitted run, never a client-supplied file path.
    scope_root
        .join("coding_engine/original_edits")
        .join(format!("{}.json", blake3::hash(root_task_id.as_bytes())))
}

pub fn applied_history(scope_root: &Path, root_task_id: &str) -> Vec<FileEditTransaction> {
    let path = history_path(scope_root, root_task_id);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Vec::new();
    };
    if meta.len() > 1024 * 1024 {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(ids) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        return Vec::new();
    };
    let store = TransactionStore::new(scope_root);
    ids.iter()
        .filter_map(|id| TransactionId::parse(id).ok())
        .filter_map(|id| store.load(&id).ok())
        .filter(|txn| txn.status == FileEditStatus::Applied)
        .collect()
}
