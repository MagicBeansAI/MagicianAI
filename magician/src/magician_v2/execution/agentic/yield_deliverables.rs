//! `yield.deliverables`: the files a run declares as its answer.
//!
//! A run that writes its deliverable to a file (a report, a dataset, a
//! document — any file type) declares it on its terminal `yield` instead of
//! pasting the full text into `artifacts`. Each declared path is resolved
//! (absolute, relative to the task workdir, or `outputs/<name>` for an
//! `artifacts` item of the same yield), validated to be a regular file that
//! stays inside this task's roots after canonicalization (so neither `..`
//! nor a symlink can reach out), size-capped, and copied into the execution
//! outputs and the task `outputs/delivered/<execution>/` directory. It is
//! then indexed as a `tool_output_file` record marked `declared_deliverable`,
//! which the terminal collector
//! (`ArtifactV2Service::delivered_file_outputs_for_terminal`) promotes to a
//! `user_attachment` output without requiring the summary to name it. The
//! first declared file is the primary deliverable.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

use super::executor::{task_filesystem_root, ActionExecutors};
use super::types::{AgenticContext, Artifact};
use super::yield_decision::YieldDeliverable;
use crate::magician_v2::artifact_v2::models::{task_output_served_url, MAX_DELIVERED_FILE_BYTES};
use crate::magician_v2::artifact_v2::{
    FilesystemExecutionArtifactIndexStore, PersistedExecutionArtifactRecord, ScopeRef,
};

/// At most this many declared files are recorded per yield (the same ceiling
/// the terminal collector attaches).
pub(crate) const MAX_DECLARED_DELIVERABLES: usize = 8;

/// The roots a declared deliverable may live in and the bases a relative
/// path resolves against, for one execution.
#[derive(Debug, Clone)]
pub(crate) struct DeliverableRoots {
    /// Relative paths resolve against these, in order: the task workdir,
    /// then the execution dir (so `outputs/<name>` is the execution
    /// outputs), then the task dir.
    pub relative_bases: Vec<PathBuf>,
    /// A resolved file must canonicalize inside one of these.
    pub allowed_roots: Vec<PathBuf>,
}

impl DeliverableRoots {
    fn for_context(ctx: &AgenticContext, executors: &ActionExecutors) -> Option<Self> {
        let principal = ctx.principal.as_deref()?;
        let workspace = ctx.workspace.as_deref()?;
        let task_id = ctx.task_id.as_deref()?;
        let execution_id = ctx.execution_id.as_deref()?;
        let task_dir = task_filesystem_root(ctx, executors)?;
        let workdir = task_dir.join("workdir");
        let execution_outputs = executors.artifact_v2_workspace.execution_outputs_dir(
            principal,
            workspace,
            task_id,
            execution_id,
        );
        let task_outputs = executors
            .artifact_v2_workspace
            .task_outputs_dir(principal, workspace, task_id);
        let mut relative_bases = vec![workdir.clone()];
        if let Some(execution_dir) = execution_outputs.parent() {
            relative_bases.push(execution_dir.to_path_buf());
        }
        relative_bases.push(task_dir);
        Some(Self {
            relative_bases,
            allowed_roots: vec![workdir, execution_outputs, task_outputs],
        })
    }
}

/// Resolve one declared deliverable path to the canonical file it names, or
/// say why it is refused. `same_yield_artifacts` are `(name, materialized
/// path)` pairs of this yield's inline artifacts, so `outputs/<name>` and
/// `<name>` find a file the same yield wrote.
pub(crate) fn resolve_declared_deliverable_path(
    raw: &str,
    roots: &DeliverableRoots,
    same_yield_artifacts: &[(String, PathBuf)],
) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('\0') {
        return Err("empty deliverable path".to_string());
    }
    let declared = if raw
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
    {
        url::Url::parse(raw)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or_else(|| format!("unreadable file URL `{raw}`"))?
    } else {
        PathBuf::from(raw)
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    if declared.is_absolute() {
        candidates.push(declared.clone());
    } else {
        let bare = raw.strip_prefix("outputs/").unwrap_or(raw);
        candidates.extend(
            same_yield_artifacts
                .iter()
                .filter(|(name, _)| name == raw || name == bare)
                .map(|(_, path)| path.clone()),
        );
        candidates.extend(roots.relative_bases.iter().map(|base| base.join(&declared)));
    }
    let Some((candidate, metadata)) = candidates.into_iter().find_map(|candidate| {
        std::fs::symlink_metadata(&candidate)
            .ok()
            .map(|metadata| (candidate, metadata))
    }) else {
        return Err(format!("`{raw}` does not exist in this task's workdir or outputs"));
    };
    if metadata.file_type().is_symlink() {
        return Err(format!("`{raw}` is a symlink, not a file"));
    }
    if !metadata.file_type().is_file() {
        return Err(format!("`{raw}` is not a regular file"));
    }
    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|error| format!("`{raw}` could not be resolved: {error}"))?;
    let inside = roots
        .allowed_roots
        .iter()
        .filter(|root| {
            std::fs::symlink_metadata(root)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        })
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .any(|root| canonical.starts_with(&root));
    if !inside {
        return Err(format!(
            "`{raw}` resolves outside this task's workdir and outputs"
        ));
    }
    let len = metadata.len();
    if len == 0 {
        return Err(format!("`{raw}` is empty"));
    }
    if len > MAX_DELIVERED_FILE_BYTES {
        return Err(format!(
            "`{raw}` is {len} bytes; delivered files are capped at {MAX_DELIVERED_FILE_BYTES}"
        ));
    }
    Ok(canonical)
}

fn same_yield_artifact_paths(artifacts: &[Artifact]) -> Vec<(String, PathBuf)> {
    artifacts
        .iter()
        .filter_map(|artifact| {
            artifact
                .materialized_path
                .as_deref()
                .map(|path| (artifact.name.clone(), PathBuf::from(path)))
        })
        .collect()
}

/// Evidence stand-ins for the success gates: one small JSON artifact per
/// declared deliverable that resolves now, or that names an inline artifact
/// of the same yield (written just after the gates pass). A declared file is
/// durable output, so a yield that delivers by path is not held to pasting
/// the text into `artifacts`.
pub(crate) fn declared_deliverable_evidence(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    deliverables: &[YieldDeliverable],
    inline_artifacts: &[Artifact],
) -> Vec<Artifact> {
    if deliverables.is_empty() {
        return Vec::new();
    }
    let Some(roots) = DeliverableRoots::for_context(ctx, executors) else {
        return Vec::new();
    };
    let same_yield = same_yield_artifact_paths(inline_artifacts);
    deliverables
        .iter()
        .take(MAX_DECLARED_DELIVERABLES)
        .filter_map(|deliverable| {
            let raw = deliverable.path.trim();
            let bare = raw.strip_prefix("outputs/").unwrap_or(raw);
            let names_inline_artifact = inline_artifacts
                .iter()
                .any(|artifact| !artifact.data.is_empty() && (artifact.name == raw || artifact.name == bare));
            let resolved = resolve_declared_deliverable_path(raw, &roots, &same_yield).ok();
            if resolved.is_none() && !names_inline_artifact {
                return None;
            }
            let name = resolved
                .as_deref()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| bare.to_string());
            let body = json!({
                "artifact_kind": "declared_deliverable",
                "path": raw,
                "title": deliverable.title,
            });
            Some(Artifact {
                name,
                content_type: "application/json".to_string(),
                data: serde_json::to_vec(&body).unwrap_or_default(),
                artifact_type: Some("declared_deliverable".to_string()),
                render_hints: None,
                materialized_path: resolved.map(|path| path.to_string_lossy().into_owned()),
            })
        })
        .collect()
}

/// The file name a delivered copy is published under: the last path
/// segment, control characters replaced, leading dots dropped. Mirrors the
/// terminal collector's naming so both copies land on one path.
fn delivered_copy_name(display_name: &str) -> Option<String> {
    let last_segment = display_name.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = last_segment
        .chars()
        .map(|ch| if ch.is_control() { '_' } else { ch })
        .collect();
    let name = name.trim().trim_start_matches('.').trim_start();
    (!name.is_empty() && name.len() <= 255).then(|| name.to_string())
}

/// `name` unless `used` (lowercased) already holds it; then `stem (2).ext`,
/// `stem (3).ext`, … — the first free one.
fn unique_copy_name(name: &str, used: &[String]) -> String {
    if !used.contains(&name.to_lowercase()) {
        return name.to_string();
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){extension}"))
        .find(|candidate| !used.contains(&candidate.to_lowercase()))
        .unwrap_or_else(|| name.to_string())
}

fn sanitized_extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| {
            value
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .take(16)
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
        .map(|value| format!(".{value}"))
        .unwrap_or_default()
}

async fn file_sha256(path: &Path) -> std::io::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Copy and index every declared deliverable of a terminal yield (see the
/// module docs). Runs after the yield's inline artifacts are materialized,
/// so `outputs/<name>` finds them. A nested same-execution segment (a
/// sub-goal frame) records nothing: its yield returns to its parent frame,
/// not to the owner. Never fails the terminal: a refused or unreadable file
/// is logged and left out. Returns how many were recorded.
///
/// One copy is made, into the execution outputs; the terminal collector
/// publishes it to `outputs/delivered/<execution>/<name>` (the
/// `task_relative_path` recorded here), and two files of one yield that
/// share a name are published as `name.ext` and `name (2).ext`.
pub(crate) async fn record_declared_yield_deliverables(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    deliverables: &[YieldDeliverable],
    terminal_artifacts: &[Artifact],
    iteration: usize,
) -> usize {
    if deliverables.is_empty() {
        return 0;
    }
    if ctx.is_nested_same_execution_segment() {
        info!(
            declared = deliverables.len(),
            "[YIELD-DELIVERABLES] nested sub-goal yield; its files are not delivered to the owner"
        );
        return 0;
    }
    let (Some(principal), Some(workspace_id), Some(task_id), Some(execution_id)) = (
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
        ctx.task_id.as_deref(),
        ctx.execution_id.as_deref(),
    ) else {
        return 0;
    };
    let Some(roots) = DeliverableRoots::for_context(ctx, executors) else {
        return 0;
    };
    let scope = ScopeRef::system_internal_unauthenticated(principal, workspace_id);
    let workspace = &executors.artifact_v2_workspace;
    let execution_outputs_dir =
        workspace.execution_outputs_dir(principal, workspace_id, task_id, execution_id);
    if workspace
        .create_dir_all_path(&execution_outputs_dir)
        .await
        .is_err()
    {
        warn!(%task_id, %execution_id, "[YIELD-DELIVERABLES] output directories unavailable");
        return 0;
    }
    let same_yield = same_yield_artifact_paths(terminal_artifacts);
    let store = FilesystemExecutionArtifactIndexStore::new(workspace.clone());
    // Groups this yield's records so the collector can order "newest yield
    // first, then rank" without relying on a loop-local counter.
    let yield_id = Uuid::new_v4().simple().to_string();
    let mut used_names: Vec<String> = Vec::new();
    let mut recorded = 0usize;
    for (rank, deliverable) in deliverables
        .iter()
        .take(MAX_DECLARED_DELIVERABLES)
        .enumerate()
    {
        let source = match resolve_declared_deliverable_path(&deliverable.path, &roots, &same_yield)
        {
            Ok(source) => source,
            Err(reason) => {
                warn!(
                    %task_id,
                    %execution_id,
                    path = %deliverable.path,
                    %reason,
                    "[YIELD-DELIVERABLES] declared deliverable refused"
                );
                continue;
            },
        };
        let Some(display_name) = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        let Some(base_name) = delivered_copy_name(&display_name) else {
            continue;
        };
        let copy_name = unique_copy_name(&base_name, &used_names);
        // The published name is the record's name, so the collector keeps
        // both same-named files and their ranks.
        let display_name = copy_name.clone();
        let artifact_uuid = Uuid::new_v4();
        let artifact_id = format!("tool_output_file:{artifact_uuid}");
        let execution_file_name = format!(
            "deliverable_{iteration}_{rank}_{}{}",
            artifact_uuid.simple(),
            sanitized_extension(&source)
        );
        let execution_path = execution_outputs_dir.join(&execution_file_name);
        let Ok(source_len) = std::fs::metadata(&source).map(|metadata| metadata.len()) else {
            continue;
        };
        match workspace
            .copy_external_atomic_path(&source, &execution_path)
            .await
        {
            Ok(bytes) if bytes == source_len => {},
            _ => {
                let _ = workspace.remove_file_path(&execution_path).await;
                warn!(%task_id, %execution_id, path = %deliverable.path, "[YIELD-DELIVERABLES] copy failed");
                continue;
            },
        }
        let Ok(content_sha256) = file_sha256(&execution_path).await else {
            let _ = workspace.remove_file_path(&execution_path).await;
            continue;
        };
        let task_relative_path = format!("outputs/delivered/{execution_id}/{copy_name}");
        let task_path = workspace
            .task_outputs_dir(principal, workspace_id, task_id)
            .join("delivered")
            .join(execution_id)
            .join(&copy_name);
        // Typed from the source: the execution copy's extension is
        // sanitized, the source keeps the name the run gave it.
        let content_type =
            crate::magician_v2::artifact_v2::file_types::output_file_content_type(&source, None)
                .await;
        let produced_at = Utc::now().to_rfc3339();
        let payload: Value = json!({
            "artifact_kind": "tool_output_file",
            "artifact_id": artifact_id,
            "tool_name": "yield",
            "display_name": display_name,
            "execution_absolute_path": execution_path.to_string_lossy(),
            "execution_relative_path": format!("outputs/{execution_file_name}"),
            "task_absolute_path": task_path.to_string_lossy(),
            "task_relative_path": task_relative_path,
            "execution_download_url": format!(
                "/api/magician/v3/executions/{execution_id}/artifacts/outputs/{execution_file_name}"
            ),
            "task_download_url": task_output_served_url(task_id, &task_relative_path),
            "content_type": content_type,
            "size_bytes": source_len,
            "content_sha256": content_sha256,
            "execution_id": execution_id,
            "task_id": task_id,
            "plan_id": ctx.plan_id,
            "step_id": ctx.step_id,
            "cycle_id": ctx.cycle_id,
            "agent_id": ctx.agent_id,
            "iteration": iteration,
            "produced_at": produced_at,
            "declared_deliverable": true,
            "deliverable_rank": rank,
            "deliverable_title": deliverable.title,
            "deliverable_yield_id": yield_id,
            "source_absolute_path": source.to_string_lossy(),
        });
        if let Err(error) = store
            .upsert_artifact(
                &scope,
                task_id,
                execution_id,
                PersistedExecutionArtifactRecord {
                    artifact_id,
                    artifact_type: "tool_output_file".to_string(),
                    content_type: "application/json".to_string(),
                    payload,
                    produced_at,
                    source_execution_id: None,
                    source_artifact_id: None,
                },
            )
            .await
        {
            warn!(%task_id, %execution_id, %error, "[YIELD-DELIVERABLES] could not index the deliverable");
            let _ = workspace.remove_file_path(&execution_path).await;
            continue;
        }
        used_names.push(copy_name.to_lowercase());
        recorded += 1;
    }
    if recorded > 0 {
        info!(%task_id, %execution_id, recorded, "[YIELD-DELIVERABLES] declared deliverables recorded");
    }
    recorded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots_in(base: &Path) -> (DeliverableRoots, PathBuf, PathBuf) {
        let task_dir = base.join("internal_tasks").join("task-1");
        let workdir = task_dir.join("workdir");
        let execution_outputs = task_dir.join("executions").join("exec-1").join("outputs");
        let task_outputs = task_dir.join("outputs");
        for dir in [&workdir, &execution_outputs, &task_outputs] {
            std::fs::create_dir_all(dir).unwrap();
        }
        (
            DeliverableRoots {
                relative_bases: vec![
                    workdir.clone(),
                    execution_outputs.parent().unwrap().to_path_buf(),
                    task_dir.clone(),
                ],
                allowed_roots: vec![workdir.clone(), execution_outputs.clone(), task_outputs],
            },
            workdir,
            execution_outputs,
        )
    }

    #[test]
    fn relative_and_absolute_workdir_files_resolve() {
        let temp = tempfile::tempdir().unwrap();
        let (roots, workdir, _) = roots_in(temp.path());
        std::fs::create_dir_all(workdir.join("reports")).unwrap();
        std::fs::write(workdir.join("reports/q3.md"), "# Q3").unwrap();
        let canonical = std::fs::canonicalize(workdir.join("reports/q3.md")).unwrap();
        assert_eq!(
            resolve_declared_deliverable_path("reports/q3.md", &roots, &[]).unwrap(),
            canonical
        );
        let absolute = workdir.join("reports/q3.md");
        assert_eq!(
            resolve_declared_deliverable_path(absolute.to_str().unwrap(), &roots, &[]).unwrap(),
            canonical
        );
    }

    #[test]
    fn outputs_name_finds_a_same_yield_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let (roots, _, execution_outputs) = roots_in(temp.path());
        let written = execution_outputs.join("brief.md");
        std::fs::write(&written, "brief").unwrap();
        let same_yield = vec![("brief.md".to_string(), written.clone())];
        let expected = std::fs::canonicalize(&written).unwrap();
        assert_eq!(
            resolve_declared_deliverable_path("outputs/brief.md", &roots, &same_yield).unwrap(),
            expected
        );
        assert_eq!(
            resolve_declared_deliverable_path("brief.md", &roots, &same_yield).unwrap(),
            expected
        );
        // Without the artifact list `outputs/` still means the execution outputs.
        assert_eq!(
            resolve_declared_deliverable_path("outputs/brief.md", &roots, &[]).unwrap(),
            expected
        );
    }

    #[test]
    fn escapes_symlinks_and_empty_files_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let (roots, workdir, _) = roots_in(temp.path());
        let outside = temp.path().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();

        assert!(resolve_declared_deliverable_path("../../../secret.txt", &roots, &[]).is_err());
        assert!(
            resolve_declared_deliverable_path(outside.to_str().unwrap(), &roots, &[]).is_err()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, workdir.join("link.txt")).unwrap();
            let refused = resolve_declared_deliverable_path("link.txt", &roots, &[]).unwrap_err();
            assert!(refused.contains("symlink"), "{refused}");
            // A symlinked directory cannot carry a file out either.
            std::fs::create_dir_all(temp.path().join("elsewhere")).unwrap();
            std::fs::write(temp.path().join("elsewhere/data.csv"), "a,b").unwrap();
            std::os::unix::fs::symlink(temp.path().join("elsewhere"), workdir.join("dir")).unwrap();
            let refused =
                resolve_declared_deliverable_path("dir/data.csv", &roots, &[]).unwrap_err();
            assert!(refused.contains("outside"), "{refused}");
        }
        std::fs::write(workdir.join("empty.md"), "").unwrap();
        assert!(resolve_declared_deliverable_path("empty.md", &roots, &[])
            .unwrap_err()
            .contains("empty"));
        std::fs::create_dir_all(workdir.join("folder")).unwrap();
        assert!(resolve_declared_deliverable_path("folder", &roots, &[]).is_err());
        assert!(resolve_declared_deliverable_path("missing.md", &roots, &[]).is_err());
        assert!(resolve_declared_deliverable_path("  ", &roots, &[]).is_err());
    }

    #[test]
    fn same_named_files_of_one_yield_get_distinct_copy_names() {
        let used = vec!["report.md".to_string(), "report (2).md".to_string()];
        assert_eq!(unique_copy_name("data.csv", &used), "data.csv");
        assert_eq!(unique_copy_name("Report.md", &used), "Report (3).md");
        assert_eq!(unique_copy_name("README", &["readme".to_string()]), "README (2)");
    }

    #[test]
    fn delivered_copy_names_are_plain_file_names() {
        assert_eq!(delivered_copy_name("reports/q3.md").as_deref(), Some("q3.md"));
        assert_eq!(delivered_copy_name(".hidden").as_deref(), Some("hidden"));
        assert_eq!(delivered_copy_name("..."), None);
    }
}
