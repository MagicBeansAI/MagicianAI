//! Scoped inventory of a chat's registered uploads and task outputs.
use super::{session_matches_scope, ActiveSessionQuery, ChatApi};
use crate::{scope::resolve_required_scope_ref, task_api_v3::TaskApiV3};
use actix_web::{web, HttpRequest, HttpResponse};
use magician::magician_v2::{
    archived_filter::ArchivedFilter,
    artifact_v2::{
        execution_artifacts::FilesystemExecutionArtifactIndexStore, workspace::ArtifactV2Workspace,
        ArtifactV2Error, ScopeRef, V3ReadApi,
    },
    auth::middleware::AuthRuntime,
    chat::models::{ChatSessionAssetKey, ChatSessionFileOrigin, ChatSessionTaskOutput},
};
use serde_json::json;
use std::collections::BTreeSet;

pub async fn get_session_assets_handler(
    chat: web::Data<ChatApi>,
    tasks: web::Data<TaskApiV3>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
) -> HttpResponse {
    let scope = match resolve_required_scope_ref(req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let session = match chat.chat_service.get_session(&path.into_inner()).await {
        Ok(Some(session))
            if session_matches_scope(&session, &scope.principal(), &scope.workspace()) =>
        {
            session
        },
        Ok(Some(_)) => return HttpResponse::Forbidden().finish(),
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(_) => return HttpResponse::ServiceUnavailable().finish(),
    };
    let inventory = match chat.chat_service.session_file_inventory(&session).await {
        Ok(files) => files,
        Err(_) => {
            return HttpResponse::ServiceUnavailable()
                .json(json!({"error":"Chat file inventory is unavailable"}))
        },
    };
    let Some(layout) = chat.chat_service.workspace_ref() else {
        return HttpResponse::ServiceUnavailable().finish();
    };
    let outputs_dir =
        layout.chat_session_outputs_dir(&scope.principal(), &scope.workspace(), &session.id);
    let base = layout.canonicalize_path(&outputs_dir).await.ok();
    let mut files = Vec::new();
    let mut unavailable_files = 0usize;
    for mut file in inventory.files {
        let path = layout
            .canonicalize_path(outputs_dir.join(&file.stored_name))
            .await
            .ok();
        let metadata = match (base.as_ref(), path.as_ref()) {
            (Some(base), Some(path)) if path.starts_with(base) => layout
                .metadata_path(path)
                .await
                .ok()
                .flatten()
                .filter(|metadata| metadata.is_file()),
            _ => None,
        };
        if let Some(metadata) = metadata {
            file.size = metadata.len();
            files.push(file);
        } else {
            unavailable_files += 1;
        }
    }
    let service = tasks.service();
    let task_ids = match session_task_ids(&service, &scope, &session.id).await {
        Ok(ids) => ids,
        Err(response) => return response,
    };
    let mirrored: BTreeSet<_> = files
        .iter()
        .filter_map(|file| {
            Some((
                file.source_task_id.clone()?,
                file.source_task_output_id.clone()?,
            ))
        })
        .collect();
    let mut assets: Vec<_> = files.into_iter().map(|file| json!({
        "id":format!("session:{}",file.id), "name":file.original_name,
        "label":file.label, "path":file.stored_name, "size":file.size,
        "mime_type":file.mime_type, "source":if matches!(file.origin, ChatSessionFileOrigin::Attachment) {"upload"} else {"chat"},
        "session_id":session.id, "task_id":file.source_task_id,
        "output_id":file.source_task_output_id, "created_at":file.created_at, "execution_id":null,
    })).collect();
    let mut unavailable_tasks = Vec::new();
    for id in &task_ids {
        let outputs = match service.get_task_outputs_view(&scope, id).await {
            Ok(outputs) => outputs,
            Err(_) => {
                unavailable_tasks.push(id);
                continue;
            },
        };
        let represented_paths: BTreeSet<_> = outputs
            .outputs
            .iter()
            .map(|file| {
                file.output
                    .relative_path
                    .strip_prefix("outputs/")
                    .unwrap_or(&file.output.relative_path)
                    .to_owned()
            })
            .collect();
        for file in outputs.outputs {
            if inventory
                .removed_task_outputs
                .iter()
                .any(|removed| removed.task_id == *id && removed.output_id == file.output.output_id)
            {
                continue;
            }
            if mirrored.contains(&(id.clone(), file.output.output_id.clone())) {
                // Preserve the task's recorded provenance even when the chat serves a projected copy.
                if let Some(asset) = assets.iter_mut().find(|asset| {
                    asset["task_id"].as_str() == Some(id.as_str())
                        && asset["output_id"].as_str() == Some(file.output.output_id.as_str())
                }) {
                    asset["execution_id"] = json!(file.output.source_execution_id);
                    asset["created_at"] = json!(file.output.created_at);
                }
                continue;
            }
            if file.absolute_path.is_none() {
                continue;
            }
            let size = layout
                .metadata_path(file.absolute_path.as_ref().unwrap())
                .await
                .ok()
                .flatten()
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len());
            assets.push(json!({
                "id":format!("task:{id}:{}",file.output.output_id),
                "name":file.output.relative_path.rsplit('/').next().unwrap_or(&file.output.relative_path),
                "path":file.output.relative_path, "mime_type":file.output.media_type,
                "source":"task", "task_id":id, "deliverable":file.deliverable,
                "session_id":session.id, "execution_id":file.output.source_execution_id,
                "created_at":file.output.created_at, "output_id":file.output.output_id, "size":size,
            }));
        }
        match service.list_executions(&scope, id).await {
            Ok(executions) => {
                let execution_ids: Vec<_> = executions
                    .into_iter()
                    .map(|entry| entry.execution_id)
                    .collect();
                match registered_execution_assets(
                    layout,
                    &scope,
                    id,
                    &execution_ids,
                    &represented_paths,
                )
                .await
                {
                    Ok((registered, missing)) => {
                        unavailable_files += missing;
                        assets.extend(
                            registered
                                .into_iter()
                                .filter(|asset| {
                                    !inventory.removed_task_outputs.iter().any(|removed| {
                                        removed.task_id == *id
                                            && asset["output_id"].as_str()
                                                == Some(removed.output_id.as_str())
                                    })
                                })
                                .map(|mut asset| {
                                    asset["session_id"] = json!(session.id);
                                    asset
                                }),
                        );
                    },
                    Err(_) => unavailable_tasks.push(id),
                }
            },
            Err(_) => unavailable_tasks.push(id),
        }
    }
    let total_size = assets
        .iter()
        .filter_map(|asset| asset["size"].as_u64())
        .fold(0u64, u64::saturating_add);
    let unknown_size_files = assets
        .iter()
        .filter(|asset| asset["size"].as_u64().is_none())
        .count();
    HttpResponse::Ok().json(json!({"session_id":session.id,"chat_title":session.title,"thread_id":session.ui_thread_id,"files":assets,"task_ids":task_ids,"unavailable_tasks":unavailable_tasks,"unavailable_files":unavailable_files,"total_size":total_size,"unknown_size_files":unknown_size_files}))
}

/// Deletes only chat-owned bytes and references, never the originating task file.
pub async fn delete_session_asset_handler(
    chat: web::Data<ChatApi>,
    tasks: web::Data<TaskApiV3>,
    auth: Option<web::Data<AuthRuntime>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ActiveSessionQuery>,
) -> HttpResponse {
    if let Err(response) = crate::programs_api::require_owner_session_or_phone(
        &req,
        auth.as_ref().map(|auth| auth.get_ref()),
    )
    .await
    {
        return response;
    }
    let scope = match resolve_required_scope_ref(req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (session_id, asset_id) = path.into_inner();
    let session = match chat.chat_service.get_session(&session_id).await {
        Ok(Some(session))
            if session_matches_scope(&session, &scope.principal(), &scope.workspace()) =>
        {
            session
        },
        Ok(_) => return HttpResponse::NotFound().finish(),
        Err(_) => return HttpResponse::ServiceUnavailable().finish(),
    };
    let Some(asset) = parse_asset_key(&asset_id) else {
        return HttpResponse::BadRequest().finish();
    };
    if let ChatSessionAssetKey::TaskOutput(output) = &asset {
        let service = tasks.service();
        let ids = match session_task_ids(&service, &scope, &session_id).await {
            Ok(ids) => ids,
            Err(response) => return response,
        };
        if !ids.contains(&output.task_id) {
            return HttpResponse::NotFound().finish();
        }
        let found = match service.get_task_outputs_view(&scope, &output.task_id).await {
            Ok(view) => view
                .outputs
                .iter()
                .any(|file| file.output.output_id == output.output_id),
            Err(_) => return HttpResponse::ServiceUnavailable().finish(),
        };
        if !found {
            let Some(layout) = chat.chat_service.workspace_ref() else {
                return HttpResponse::ServiceUnavailable().finish();
            };
            let executions = match service.list_executions(&scope, &output.task_id).await {
                Ok(executions) => executions
                    .into_iter()
                    .map(|entry| entry.execution_id)
                    .collect::<Vec<_>>(),
                Err(_) => return HttpResponse::ServiceUnavailable().finish(),
            };
            match registered_execution_assets(
                layout,
                &scope,
                &output.task_id,
                &executions,
                &BTreeSet::new(),
            )
            .await
            {
                Ok((files, _))
                    if files.iter().any(|file| {
                        file["output_id"].as_str() == Some(output.output_id.as_str())
                    }) => {},
                Ok(_) => return HttpResponse::NotFound().finish(),
                Err(_) => return HttpResponse::ServiceUnavailable().finish(),
            }
        }
    }
    match chat.chat_service.delete_session_asset(&session, &asset).await {
        Ok(()) => HttpResponse::Ok().json(json!({"deleted":true})),
        Err(error) if error.to_string() == "chat_files_busy" => HttpResponse::Conflict()
            .json(json!({"message":"Wait for this chat's current or queued messages to finish before deleting files."})),
        Err(error) => {
            tracing::warn!(%session_id, %error, "chat file deletion failed");
            HttpResponse::ServiceUnavailable().json(json!({"message":"The file could not be deleted. Refresh and try again."}))
        },
    }
}

/// Use only registered producer files and verified paths under that producer's
/// outputs. Mutable workdirs and unrelated/copy-source metadata are not authority.
async fn registered_execution_assets(
    layout: &ArtifactV2Workspace,
    scope: &ScopeRef,
    task_id: &str,
    execution_ids: &[String],
    represented_paths: &BTreeSet<String>,
) -> Result<(Vec<serde_json::Value>, usize), ArtifactV2Error> {
    use std::path::{Component, Path};
    let store = FilesystemExecutionArtifactIndexStore::new(layout.clone());
    let task_root = layout
        .canonicalize_path(layout.task_dir(scope.principal(), scope.workspace(), task_id))
        .await?;
    let mut files = Vec::new();
    let mut missing = 0;
    for execution_id in execution_ids {
        if Path::new(execution_id).components().count() != 1
            || Path::new(execution_id)
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
        {
            continue;
        }
        let root = layout.execution_outputs_dir(
            scope.principal(),
            scope.workspace(),
            task_id,
            execution_id,
        );
        let Some(root) = layout
            .canonicalize_path(root)
            .await
            .ok()
            .filter(|root| root.starts_with(&task_root))
        else {
            continue;
        };
        for record in store.list_artifacts(scope, task_id, execution_id).await? {
            let payload = &record.payload;
            if record.artifact_type != "tool_output_file"
                || payload["artifact_kind"].as_str() != Some("tool_output_file")
                || payload["task_id"].as_str() != Some(task_id)
                || payload["execution_id"].as_str() != Some(execution_id.as_str())
            {
                continue;
            }
            let projected = payload["task_relative_path"]
                .as_str()
                .unwrap_or_default()
                .strip_prefix("outputs/")
                .unwrap_or_default();
            if !projected.is_empty() && represented_paths.contains(projected) {
                continue;
            }
            let Some(relative) = payload["execution_relative_path"]
                .as_str()
                .and_then(|path| path.strip_prefix("outputs/"))
            else {
                continue;
            };
            let relative_path = Path::new(relative);
            if relative.is_empty()
                || relative_path
                    .components()
                    .any(|p| !matches!(p, Component::Normal(_)))
            {
                continue;
            }
            let Some(path) = layout
                .canonicalize_path(root.join(relative_path))
                .await
                .ok()
                .filter(|path| path.starts_with(&root))
            else {
                missing += 1;
                continue;
            };
            let Some(metadata) = layout
                .metadata_path(&path)
                .await?
                .filter(|metadata| metadata.is_file())
            else {
                missing += 1;
                continue;
            };
            let output_id = format!("artifact:{execution_id}:{}", record.artifact_id);
            files.push(json!({
                "id":format!("task:{task_id}:{output_id}"), "output_id":output_id,
                "name":payload["display_name"].as_str().filter(|name| !name.is_empty()).unwrap_or(relative),
                "path":format!("executions/{execution_id}/outputs/{relative}"),
                "mime_type":payload["content_type"].as_str(), "size":metadata.len(),
                "source":"task", "task_id":task_id, "execution_id":execution_id,
                "created_at":record.produced_at, "deliverable":false,
            }));
        }
    }
    Ok((files, missing))
}

fn parse_asset_key(id: &str) -> Option<ChatSessionAssetKey> {
    if id.len() > 1024 {
        return None;
    }
    if let Some(file) = id.strip_prefix("session:").filter(|id| !id.is_empty()) {
        return Some(ChatSessionAssetKey::File(file.to_owned()));
    }
    let (task, output) = id.strip_prefix("task:")?.split_once(':')?;
    if task.is_empty() || output.is_empty() {
        return None;
    }
    Some(ChatSessionAssetKey::TaskOutput(ChatSessionTaskOutput {
        task_id: task.into(),
        output_id: output.into(),
    }))
}

async fn session_task_ids(
    service: &magician::magician_v2::artifact_v2::service::ArtifactV2Service,
    scope: &magician::magician_v2::artifact_v2::service::ScopeRef,
    session_id: &str,
) -> Result<BTreeSet<String>, HttpResponse> {
    let (regular, internal) = tokio::join!(
        service.list_tasks_archive_filtered(&scope, ArchivedFilter::Include),
        service.list_internal_tasks_archive_filtered(&scope, ArchivedFilter::Include),
    );
    let (Ok(regular), Ok(internal)) = (regular, internal) else {
        return Err(HttpResponse::ServiceUnavailable()
            .json(json!({"error":"Task file inventory is unavailable"})));
    };
    // Chat lineage is carried through delegation/subtasks, independently of
    // subscription or which chat happened to be visible when a child started.
    let related = regular
        .into_iter()
        .chain(internal)
        .map(|task| {
            (
                task.id,
                task.chat_session_id,
                task.tags
                    .iter()
                    .filter_map(|tag| {
                        tag.id
                            .strip_prefix("agentic-source-task:")
                            .map(str::to_owned)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    Ok(related_task_ids(session_id, &related))
}

/// Follow canonical task-creation provenance as well as inherited chat lineage.
/// A visited set handles cycles; dependencies/reference links are not ancestry.
fn related_task_ids(
    session: &str,
    tasks: &[(String, Option<String>, Vec<String>)],
) -> BTreeSet<String> {
    let mut ids: BTreeSet<_> = tasks
        .iter()
        .filter(|(_, chat, _)| chat.as_deref() == Some(session))
        .map(|(id, _, _)| id.clone())
        .collect();
    loop {
        let children: Vec<_> = tasks
            .iter()
            .filter(|(id, _, parents)| {
                !ids.contains(id) && parents.iter().any(|parent| ids.contains(parent))
            })
            .map(|(id, _, _)| id.clone())
            .collect();
        if children.is_empty() {
            return ids;
        }
        ids.extend(children);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn chat_files_inventory_lists_only_registered_producer_outputs() {
        use magician::magician_v2::artifact_v2::models::PersistedExecutionArtifactRecord;
        let temp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(temp.path());
        let scope = ScopeRef::system_internal_unauthenticated("owner", "workspace");
        let root = layout.execution_outputs_dir("owner", "workspace", "task-files", "exec-files");
        layout.create_dir_all_path(&root).await.unwrap();
        std::fs::write(root.join("report.md"), b"done").unwrap();
        std::fs::write(root.join("not-registered.md"), b"private scratch").unwrap();
        let store = FilesystemExecutionArtifactIndexStore::new(layout.clone());
        let base = json!({"artifact_kind":"tool_output_file", "task_id":"task-files", "execution_id":"exec-files", "execution_relative_path":"outputs/report.md", "task_relative_path":"outputs/projected-report.md", "display_name":"report.md", "content_type":"text/markdown", "size_bytes":99999});
        for (id, relative, task) in [
            ("good", "outputs/report.md", "task-files"),
            ("traversal", "../report.md", "task-files"),
            ("wrong-task", "outputs/report.md", "unrelated-task"),
        ] {
            let mut payload = base.clone();
            payload["execution_relative_path"] = json!(relative);
            payload["task_id"] = json!(task);
            store
                .upsert_artifact(
                    &scope,
                    "task-files",
                    "exec-files",
                    PersistedExecutionArtifactRecord {
                        artifact_id: id.to_owned(),
                        artifact_type: "tool_output_file".to_owned(),
                        content_type: "application/json".to_owned(),
                        payload,
                        produced_at: "2026-01-01T00:00:00Z".to_owned(),
                        source_execution_id: None,
                        source_artifact_id: None,
                    },
                )
                .await
                .unwrap();
        }
        let ids = ["exec-files".to_owned()];
        let (files, missing) =
            registered_execution_assets(&layout, &scope, "task-files", &ids, &BTreeSet::new())
                .await
                .unwrap();
        assert_eq!(missing, 0);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["size"], 4);
        assert_eq!(files[0]["path"], "executions/exec-files/outputs/report.md");
        assert!(
            matches!(parse_asset_key(files[0]["id"].as_str().unwrap()),Some(ChatSessionAssetKey::TaskOutput(key)) if key.output_id == "artifact:exec-files:good")
        );
        let (deduplicated, _) = registered_execution_assets(
            &layout,
            &scope,
            "task-files",
            &ids,
            &BTreeSet::from(["projected-report.md".to_owned()]),
        )
        .await
        .unwrap();
        assert!(deduplicated.is_empty());
        #[cfg(unix)]
        {
            std::fs::remove_file(root.join("report.md")).unwrap();
            let outside = temp.path().join("outside.md");
            std::fs::write(&outside, b"outside").unwrap();
            std::os::unix::fs::symlink(outside, root.join("report.md")).unwrap();
            let (files, missing) =
                registered_execution_assets(&layout, &scope, "task-files", &ids, &BTreeSet::new())
                    .await
                    .unwrap();
            assert!(files.is_empty());
            assert_eq!(missing, 1);
        }
    }

    #[test]
    fn includes_indirect_legacy_children_without_unrelated_tasks_or_cycles() {
        let tasks = vec![
            ("root".into(), Some("chat".into()), vec![]),
            ("child".into(), None, vec!["root".into()]),
            ("grandchild".into(), None, vec!["child".into()]),
            ("other".into(), Some("other-chat".into()), vec![]),
            ("cycle-a".into(), None, vec!["cycle-b".into()]),
            ("cycle-b".into(), None, vec!["cycle-a".into()]),
        ];
        assert_eq!(
            related_task_ids("chat", &tasks),
            BTreeSet::from(["root".into(), "child".into(), "grandchild".into()])
        );
    }
}
