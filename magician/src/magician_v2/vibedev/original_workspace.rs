//! Original-folder runs have durable location records, but no managed checkout.
//! Keeping these separate from trusted clones prevents clone retention, rewind,
//! branch publication and deletion code from ever operating on an owner's files.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalWorkspace {
    pub root_task_id: String,
    pub project_id: String,
    pub repo: PathBuf,
}

fn record_path(scope_root: &Path, root_task_id: &str) -> Result<PathBuf, String> {
    if root_task_id.is_empty()
        || !root_task_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("invalid original-workspace run id".into());
    }
    Ok(scope_root
        .join("coding_engine/original_runs")
        .join(format!("{root_task_id}.json")))
}

pub fn load(scope_root: &Path, root_task_id: &str) -> Result<Option<OriginalWorkspace>, String> {
    let path = record_path(scope_root, root_task_id)?;
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn ensure(
    scope_root: &Path,
    root_task_id: &str,
    project_id: &str,
    repo: &Path,
) -> Result<OriginalWorkspace, String> {
    if let Some(record) = load(scope_root, root_task_id)? {
        if record.project_id != project_id
            || !record.repo.is_dir()
            || record.repo.canonicalize().ok().as_deref() != Some(record.repo.as_path())
        {
            return Err("the original folder recorded for this run is unavailable".into());
        }
        return Ok(record);
    }
    let repo = repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", repo.display()))?;
    if !repo.is_dir() {
        return Err("the original project folder must be a directory".into());
    }
    let record = OriginalWorkspace {
        root_task_id: root_task_id.into(),
        project_id: project_id.into(),
        repo,
    };
    let path = record_path(scope_root, root_task_id)?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(
        &path,
        &serde_json::to_vec(&record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_is_pinned_without_copying_or_changing_owner_files() {
        let scope = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("dirty.txt"), "uncommitted\n").unwrap();
        let run = ensure(scope.path(), "run-1", "p", repo.path()).unwrap();
        let other = tempfile::tempdir().unwrap();
        assert_eq!(
            ensure(scope.path(), "run-1", "p", other.path()).unwrap(),
            run
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("dirty.txt")).unwrap(),
            "uncommitted\n"
        );
        assert_eq!(std::fs::read_dir(repo.path()).unwrap().count(), 1);
        assert!(load(scope.path(), "../escape").is_err());
    }
}

/// Follow typed run links, so a check on a follow-up still uses its root's folder.
pub fn for_task(
    scope_root: &Path,
    project: &super::projects::VibeDevProjectRecord,
    task_id: &str,
) -> Result<Option<OriginalWorkspace>, String> {
    let mut id = task_id;
    let mut visited = std::collections::HashSet::new();
    while visited.insert(id.to_string()) {
        if let Some(run) = load(scope_root, id)? {
            if run.project_id != project.project_id {
                return Err("original run belongs to another project".into());
            }
            return Ok(Some(run));
        }
        match project
            .run_links
            .get(id)
            .and_then(|link| link.parent_task_id.as_deref())
        {
            Some(parent) => id = parent,
            None => return Ok(None),
        }
    }
    Err("cyclic VibeDev run links".into())
}

/// Resolve a compiled check from authenticated task provenance, rather than
/// trusting a model's repository argument or the project's current default.
pub async fn for_execution_task(
    service: &std::sync::Arc<crate::magician_v2::artifact_v2::service::ArtifactV2Service>,
    scope: &crate::magician_v2::artifact_v2::service::ScopeRef,
    scope_root: &Path,
    task_id: &str,
) -> Result<Option<OriginalWorkspace>, String> {
    use crate::magician_v2::artifact_v2::service::V3ReadApi;
    let task = service
        .get_task(scope, task_id)
        .await
        .map_err(|e| e.to_string())?;
    let Some(project_id) = super::projects::parse_vibedev_project_id(&task.manifest.description)
    else {
        return Ok(None);
    };
    let Some(project) = super::projects::read_vibedev_projects(scope_root)
        .into_iter()
        .find(|p| p.project_id == project_id && !p.archived)
    else {
        return Ok(None);
    };
    let ancestors = crate::magician_v2::execution::compiled_handlers::run_coding_task::vibedev_chain_ancestor_ids(service, scope, &task.manifest.description).await;
    for_task(
        scope_root,
        &project,
        ancestors.last().map(String::as_str).unwrap_or(task_id),
    )
}
