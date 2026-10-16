//! Owner approval for writes outside the coding sandbox's allow-list.
//!
//! When a turn's engine is refused a write (see
//! `coding_engine::write_denials`), the owner gets one question per turn
//! listing the folders: *Allow for this project* or *Don't allow*. An allowed
//! answer is saved on the project (`sandbox_write_paths`) the next time one of
//! its turns starts, and those folders join the turn's write roots. Nothing is
//! retried automatically; the next turn simply can write there.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::projects::{
    mutate_project_store_at, vibedev_project_store_path, ProjectStoreEdit, VibeDevProjectRecord,
};
use crate::magician_v2::execution::coding_engine::write_denials::never_allow;
use crate::magician_v2::user_requests::{
    RequestOption, UserRequest, UserRequestRecord, UserRequestStatus,
};

/// `UserRequest::request_type` of the approval question.
pub const SANDBOX_WRITE_REQUEST_TYPE: &str = "vibedev.sandbox_write_approval";
/// The answer that saves the folders on the project.
pub const ALLOW_FOR_PROJECT: &str = "allow_project";
const DENY: &str = "deny";
/// Unanswered for a week, the question closes as not allowed.
const APPROVAL_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;
const MAX_PROJECT_WRITE_PATHS: usize = 32;

/// The question for one turn's refused folders.
pub fn approval_request(
    project: &VibeDevProjectRecord,
    dirs: &[PathBuf],
    principal: &str,
    workspace: &str,
    task_id: Option<String>,
    execution_id: Option<String>,
) -> UserRequest {
    let list = dirs
        .iter()
        .map(|dir| format!("- `{}`", dir.display()))
        .collect::<Vec<_>>()
        .join("\n");
    UserRequest {
        id: String::new(),
        request_type: SANDBOX_WRITE_REQUEST_TYPE.to_string(),
        question: format!(
            "**Coding sandbox: allow writes for {}?**\n\nThe coding engine tried to write \
             outside the folders a run may write, and the sandbox refused:\n\n{list}\n\n\
             Allow these folders for this project's future runs? The refused step is not \
             retried; ask the run again after allowing.",
            project.name
        ),
        options: vec![
            RequestOption {
                id: ALLOW_FOR_PROJECT.to_string(),
                label: "Allow for this project".to_string(),
                requires_input: false,
            },
            RequestOption {
                id: DENY.to_string(),
                label: "Don't allow".to_string(),
                requires_input: false,
            },
        ],
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        context: json!({
            "project_id": project.project_id,
            "paths": dirs.iter().map(|dir| dir.display().to_string()).collect::<Vec<_>>(),
        }),
        source: "vibedev".to_string(),
        execution_id,
        task_id,
        timeout_secs: APPROVAL_TIMEOUT_SECS,
        default_on_timeout: DENY.to_string(),
        created_at: 0,
        sensitive: None,
    }
}

/// One allowed answer: the question's id and the folders it listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedAnswer {
    pub request_id: String,
    pub paths: Vec<String>,
}

/// Allowed answers for `project_id` in resolved approval questions.
pub fn approved_answers(records: &[UserRequestRecord], project_id: &str) -> Vec<ApprovedAnswer> {
    records
        .iter()
        .filter(|record| record.request.request_type == SANDBOX_WRITE_REQUEST_TYPE)
        .filter(|record| record.status == UserRequestStatus::Resolved)
        .filter(|record| {
            record
                .response
                .as_ref()
                .is_some_and(|response| response.decision == ALLOW_FOR_PROJECT)
        })
        .filter(|record| {
            record
                .request
                .context
                .get("project_id")
                .and_then(Value::as_str)
                == Some(project_id)
        })
        .map(|record| ApprovedAnswer {
            request_id: record.request.id.clone(),
            paths: record
                .request
                .context
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(|path| path.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// Validate a list the owner sets (or that approvals add): absolute, and
/// nothing on the never-allow list.
pub fn validate_write_paths(paths: &[String], home: Option<&Path>) -> Result<(), String> {
    if paths.len() > MAX_PROJECT_WRITE_PATHS {
        return Err(format!("at most {MAX_PROJECT_WRITE_PATHS} writable paths"));
    }
    for path in paths {
        let candidate = Path::new(path.trim());
        if !candidate.is_absolute() {
            return Err(format!("`{path}` must be an absolute path"));
        }
        if never_allow(candidate, home) || holds_or_inside_magician(candidate) {
            return Err(format!(
                "`{path}` can never be made writable for coding runs"
            ));
        }
    }
    Ok(())
}

/// Whether `path` is inside, or holds, Magician's runtime root or source.
pub fn holds_or_inside_magician(path: &Path) -> bool {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    crate::magician_v2::execution::coding_engine::guarded_magician_paths()
        .iter()
        .any(|guarded| path.starts_with(guarded) || guarded.starts_with(&path))
}

/// Most applied question ids kept on a project; the oldest drop first.
const MAX_APPLIED_APPROVALS: usize = 256;

/// Record `answers` as applied without adding their folders: the owner set
/// the list by hand, so earlier answers must not add back what they removed.
pub fn mark_answers_applied(project: &mut VibeDevProjectRecord, answers: &[ApprovedAnswer]) {
    for answer in answers {
        if !project
            .applied_sandbox_approvals
            .contains(&answer.request_id)
        {
            project
                .applied_sandbox_approvals
                .push(answer.request_id.clone());
        }
    }
    let excess = project
        .applied_sandbox_approvals
        .len()
        .saturating_sub(MAX_APPLIED_APPROVALS);
    project.applied_sandbox_approvals.drain(..excess);
}

/// Apply allowed answers not yet applied to the project: their folders join
/// `sandbox_write_paths` once, and their ids are recorded so a folder the
/// owner removes later stays removed. Returns the project's list afterwards.
pub async fn persist_approved_paths(
    scope_root: &Path,
    project_id: &str,
    answers: &[ApprovedAnswer],
) -> Result<Vec<String>, String> {
    let home = dirs::home_dir();
    let mut result = Vec::new();
    let store_path = vibedev_project_store_path(scope_root);
    mutate_project_store_at(&store_path, |store| {
        let Some(project) = store
            .projects
            .iter_mut()
            .find(|project| project.project_id == project_id)
        else {
            return Ok(ProjectStoreEdit::Skip);
        };
        let mut changed = false;
        for answer in answers {
            if project
                .applied_sandbox_approvals
                .contains(&answer.request_id)
            {
                continue;
            }
            for path in &answer.paths {
                if validate_write_paths(&[path.clone()], home.as_deref()).is_ok()
                    && !project.sandbox_write_paths.contains(path)
                    && project.sandbox_write_paths.len() < MAX_PROJECT_WRITE_PATHS
                {
                    project.sandbox_write_paths.push(path.clone());
                }
            }
            changed = true;
        }
        mark_answers_applied(project, answers);
        result = project.sandbox_write_paths.clone();
        Ok(if changed {
            ProjectStoreEdit::Write
        } else {
            ProjectStoreEdit::Skip
        })
    })
    .await?;
    Ok(result)
}

/// The project's writable folders, first saving any allowed answer not yet
/// on it — so a coordinator turn and a coding turn see the same list as soon
/// as the owner answers.
pub async fn project_write_paths_with_approvals(
    scope_root: &Path,
    project: &VibeDevProjectRecord,
    history: &[UserRequestRecord],
) -> Vec<String> {
    let mut paths = project.sandbox_write_paths.clone();
    let pending: Vec<ApprovedAnswer> = approved_answers(history, &project.project_id)
        .into_iter()
        .filter(|answer| {
            !project
                .applied_sandbox_approvals
                .contains(&answer.request_id)
        })
        .collect();
    if !pending.is_empty() {
        match persist_approved_paths(scope_root, &project.project_id, &pending).await {
            Ok(all) => paths = all,
            Err(error) => tracing::warn!(%error, "could not save approved sandbox write paths"),
        }
    }
    paths
}

/// Marks the coordinator's block about the project's writable folders.
pub const SANDBOX_WRITE_POLICY_MARKER: &str = "Coding sandbox (owner-allowed folders):";

/// The coordinator's note on the folders the owner allowed, or `None` when
/// the project has none. Without it a follow-up after an approval only sees
/// the parent's "Operation not permitted" and investigates instead of
/// running the step again.
pub fn coordinator_policy(paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let list = paths
        .iter()
        .map(|path| format!("  - {path}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "{SANDBOX_WRITE_POLICY_MARKER}\n\
         - Besides the project, coding runs may also write these folders; the owner allowed them:\n\
         {list}\n\
         - If an earlier run's write to one of them was refused by the sandbox, that is resolved: \
         run the step again with run_coding_task rather than investigating the earlier failure."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::user_requests::UserResponse;

    fn record(project: &str, decision: &str, paths: &[&str]) -> UserRequestRecord {
        let id = format!("{project}-{decision}");
        serde_json::from_value(json!({
            "id": id,
            "request_type": SANDBOX_WRITE_REQUEST_TYPE,
            "question": "",
            "options": [],
            "principal": "p",
            "workspace": "w",
            "context": { "project_id": project, "paths": paths },
            "source": "vibedev",
            "timeout_secs": 1,
            "default_on_timeout": DENY,
            "created_at": 0,
            "status": "resolved",
            "response": serde_json::to_value(UserResponse {
                request_id: id.clone(),
                decision: decision.into(),
                input: None,
                channel: "web".into(),
                sensitive: vec![],
            })
            .unwrap(),
            "resolved_at": 1,
        }))
        .expect("record")
    }

    #[test]
    fn only_allowed_answers_for_the_project_count() {
        let records = vec![
            record("proj-a", ALLOW_FOR_PROJECT, &["/opt/tool", "/srv/data"]),
            record("proj-a", DENY, &["/var/other"]),
            record("proj-b", ALLOW_FOR_PROJECT, &["/elsewhere"]),
        ];
        assert_eq!(
            approved_answers(&records, "proj-a"),
            vec![ApprovedAnswer {
                request_id: format!("proj-a-{ALLOW_FOR_PROJECT}"),
                paths: vec!["/opt/tool".into(), "/srv/data".into()],
            }]
        );
    }

    #[tokio::test]
    async fn an_answer_is_applied_once_so_a_removed_folder_stays_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = vibedev_project_store_path(tmp.path());
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        let project: VibeDevProjectRecord = serde_json::from_value(json!({
            "project_id": "proj-a", "name": "A", "chat_thread_id": "t",
            "chat_session_id": "s", "created_at_ms": 0, "updated_at_ms": 0,
        }))
        .unwrap();
        std::fs::write(&store, json!({ "projects": [project] }).to_string()).unwrap();
        let history = vec![record("proj-a", ALLOW_FOR_PROJECT, &["/opt/tool"])];
        let read = || {
            super::super::projects::read_vibedev_projects(tmp.path())
                .into_iter()
                .next()
                .unwrap()
        };

        let paths = project_write_paths_with_approvals(tmp.path(), &read(), &history).await;
        assert_eq!(paths, vec!["/opt/tool"]);

        // The owner removes it; the same answer in history does not add it back.
        mutate_project_store_at(&store, |store| {
            store.projects[0].sandbox_write_paths.clear();
            Ok(ProjectStoreEdit::Write)
        })
        .await
        .unwrap();
        let paths = project_write_paths_with_approvals(tmp.path(), &read(), &history).await;
        assert!(paths.is_empty(), "{paths:?}");
    }

    #[test]
    fn coordinator_policy_lists_allowed_folders_and_says_to_rerun() {
        assert_eq!(coordinator_policy(&[]), None);
        let block = coordinator_policy(&["/opt/tool".into()]).unwrap();
        assert!(block.starts_with(SANDBOX_WRITE_POLICY_MARKER));
        assert!(block.contains("  - /opt/tool"));
        assert!(block.contains("run the step again with run_coding_task"));
    }

    #[test]
    fn write_paths_must_be_absolute_and_not_protected() {
        let home = Path::new("/Users/me");
        assert!(validate_write_paths(&["/opt/tool".into()], Some(home)).is_ok());
        assert!(validate_write_paths(&["relative".into()], Some(home)).is_err());
        assert!(validate_write_paths(&["/Users/me".into()], Some(home)).is_err());
        assert!(validate_write_paths(&["/Users/me/.ssh".into()], Some(home)).is_err());
    }
}
