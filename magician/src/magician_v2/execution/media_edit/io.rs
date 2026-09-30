use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use uuid::Uuid;

use crate::magician_v2::chat::models::{
    ChatSessionFileIndex, ChatSessionFileOrigin, ChatSessionFileRecord,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_handlers::staged_file_edit::resolve_path;
use crate::magician_v2::execution::error::ExecutionError;

/// Synthetic attachment session id, isolated from real chat sessions —
/// mirrors `capture_reference`'s `"vibedev-refs"`.
const ATTACHMENT_SESSION_ID: &str = "media-edit";

pub fn resolve_inputs(
    resources: &Arc<AgentResources>,
    args: &Value,
    raw_paths: &[String],
) -> Result<Vec<PathBuf>, ExecutionError> {
    if raw_paths.is_empty() {
        return Err(ExecutionError::Step(
            "media_edit requires at least one input path".into(),
        ));
    }
    raw_paths
        .iter()
        .map(|raw| {
            resolve_path(resources, args, raw)
                .map(|scoped| scoped.absolute_path)
                .map_err(|error| {
                    ExecutionError::Step(format!("could not resolve input `{raw}`: {error:#}"))
                })
        })
        .collect()
}

pub struct PreparedOutput {
    pub path: PathBuf,
    pub attachment_id: String,
    pub stored_name: String,
}

/// Owned, `Send + 'static` copy of what `register_output_attachment` needs —
/// `job::MediaJobRegistry::spawn`'s completion callback is `'static` and
/// cannot borrow a `PreparedOutput` that lives on the caller's stack.
#[derive(Clone)]
pub struct PreparedOutputHandle {
    pub path: PathBuf,
    pub attachment_id: String,
    pub stored_name: String,
}

impl From<&PreparedOutput> for PreparedOutputHandle {
    fn from(p: &PreparedOutput) -> Self {
        Self {
            path: p.path.clone(),
            attachment_id: p.attachment_id.clone(),
            stored_name: p.stored_name.clone(),
        }
    }
}

pub fn prepare_output(
    resources: &Arc<AgentResources>,
    principal: &str,
    workspace: &str,
    operation: &str,
    ext: &str,
) -> Result<PreparedOutput, ExecutionError> {
    let outputs_dir = resources.artifact_workspace.chat_session_outputs_dir(
        principal,
        workspace,
        ATTACHMENT_SESSION_ID,
    );
    std::fs::create_dir_all(&outputs_dir).map_err(|error| {
        ExecutionError::Step(format!("could not prepare media_edit outputs dir: {error}"))
    })?;

    let token = Uuid::new_v4().simple().to_string();
    let short = &token[..12];
    let stored_name = format!("{operation}_{short}.{ext}");
    let attachment_id = format!("att_{short}");
    let path = outputs_dir.join(&stored_name);

    Ok(PreparedOutput {
        path,
        attachment_id,
        stored_name,
    })
}

pub fn register_output_attachment(
    resources: &Arc<AgentResources>,
    principal: &str,
    workspace: &str,
    prepared: &PreparedOutputHandle,
    mime_type: &str,
    label: String,
) -> Result<(), ExecutionError> {
    let size = std::fs::metadata(&prepared.path)
        .map_err(|error| {
            ExecutionError::Step(format!(
                "media_edit output missing after ffmpeg reported success: {error}"
            ))
        })?
        .len();

    let record = ChatSessionFileRecord {
        id: prepared.attachment_id.clone(),
        stored_name: prepared.stored_name.clone(),
        original_name: prepared.stored_name.clone(),
        mime_type: mime_type.to_string(),
        size,
        label: Some(label),
        screen_capture: None,
        prompt_image: mime_type.starts_with("image/"),
        origin: ChatSessionFileOrigin::Attachment,
        source_task_output_id: None,
        source_task_id: None,
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    let index_path = resources.artifact_workspace.chat_session_file_index_path(
        principal,
        workspace,
        ATTACHMENT_SESSION_ID,
    );
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ExecutionError::Step(format!("could not create attachment index dir: {error}"))
        })?;
    }
    // A missing file (first attachment in this scope) defaults to an empty
    // index — expected. A file that EXISTS but fails to parse is different:
    // silently treating it as empty would overwrite it and discard every
    // attachment recorded before now, which is worse than just erroring.
    let mut index: ChatSessionFileIndex = match std::fs::read_to_string(&index_path) {
        Ok(content) => serde_json::from_str(&content).map_err(|error| {
            ExecutionError::Step(format!(
                "attachment index at `{}` is corrupt, refusing to overwrite it: {error}",
                index_path.display()
            ))
        })?,
        Err(_) => ChatSessionFileIndex::default(),
    };
    index.files.push(record);
    let serialized = serde_json::to_string_pretty(&index).map_err(|error| {
        ExecutionError::Step(format!("could not serialize attachment index: {error}"))
    })?;
    std::fs::write(&index_path, serialized).map_err(|error| {
        ExecutionError::Step(format!("could not write attachment index: {error}"))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{OnceLock, RwLock};

    use serde_json::json;

    use super::*;
    use crate::config::MagicianConfig;
    use crate::magician_v2::agents::definition_store::AgentDefinitionStore;
    use crate::magician_v2::agents::memory::AgentMemoryResolver;
    use crate::magician_v2::agents::storage::AgentStorage;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    fn test_agent_resources(root: &std::path::Path) -> Arc<AgentResources> {
        let workspace = ArtifactV2Workspace::new(root);
        let memory_resolver = Arc::new(AgentMemoryResolver::new(root));
        let agent_storage = AgentStorage::new(root);
        let agent_definition_store = Arc::new(AgentDefinitionStore::new(agent_storage));

        Arc::new(AgentResources {
            magician_config: Arc::new(RwLock::new(MagicianConfig::default())),
            memory_resolver,
            agent_definition_store,
            artifact_workspace: workspace,
            artifact_v2_service: None,
            event_broadcaster: None,
            operation_llm_router: None,
            secret_store_resolver: None,
            content_acquisition_resolver: Arc::new(RwLock::new(None)),
            file_sandbox: Default::default(),
            tool_index: Arc::new(OnceLock::new()),
            user_request_service: None,
            agent_runtime: None,
        })
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "media_edit_io_test_{label}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn resolve_inputs_rejects_an_empty_path_list() {
        let root = temp_root("empty_inputs");
        let resources = test_agent_resources(&root);
        let result = resolve_inputs(&resources, &json!({}), &[]);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepare_output_names_the_file_after_the_operation_and_creates_the_dir() {
        let root = temp_root("prepare_output");
        let resources = test_agent_resources(&root);
        let prepared = prepare_output(&resources, "p", "w", "trim", "mp4").unwrap();
        assert!(prepared.stored_name.starts_with("trim_"));
        assert!(prepared.stored_name.ends_with(".mp4"));
        assert!(prepared.path.parent().unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn write_dummy_output(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not really a video").unwrap();
    }

    #[test]
    fn register_output_attachment_creates_a_fresh_index() {
        let root = temp_root("fresh_index");
        let resources = test_agent_resources(&root);
        let prepared = prepare_output(&resources, "p", "w", "trim", "mp4").unwrap();
        write_dummy_output(&prepared.path);

        register_output_attachment(
            &resources,
            "p",
            "w",
            &PreparedOutputHandle::from(&prepared),
            "video/mp4",
            "media_edit: trim".to_string(),
        )
        .unwrap();

        let index_path = resources.artifact_workspace.chat_session_file_index_path(
            "p",
            "w",
            ATTACHMENT_SESSION_ID,
        );
        let index: ChatSessionFileIndex =
            serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
        assert_eq!(index.files.len(), 1);
        assert_eq!(index.files[0].id, prepared.attachment_id);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn register_output_attachment_appends_rather_than_overwrites() {
        let root = temp_root("appends");
        let resources = test_agent_resources(&root);

        let first = prepare_output(&resources, "p", "w", "trim", "mp4").unwrap();
        write_dummy_output(&first.path);
        register_output_attachment(
            &resources,
            "p",
            "w",
            &PreparedOutputHandle::from(&first),
            "video/mp4",
            "first".to_string(),
        )
        .unwrap();

        let second = prepare_output(&resources, "p", "w", "speed", "mp4").unwrap();
        write_dummy_output(&second.path);
        register_output_attachment(
            &resources,
            "p",
            "w",
            &PreparedOutputHandle::from(&second),
            "video/mp4",
            "second".to_string(),
        )
        .unwrap();

        let index_path = resources.artifact_workspace.chat_session_file_index_path(
            "p",
            "w",
            ATTACHMENT_SESSION_ID,
        );
        let index: ChatSessionFileIndex =
            serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
        assert_eq!(
            index.files.len(),
            2,
            "second registration must not discard the first"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression test for the corrupted-index bug: a parse failure on an
    /// EXISTING index must error rather than silently default to empty and
    /// overwrite whatever attachment history was already recorded.
    #[test]
    fn register_output_attachment_refuses_to_overwrite_a_corrupt_index() {
        let root = temp_root("corrupt_index");
        let resources = test_agent_resources(&root);

        let prepared = prepare_output(&resources, "p", "w", "trim", "mp4").unwrap();
        write_dummy_output(&prepared.path);

        let index_path = resources.artifact_workspace.chat_session_file_index_path(
            "p",
            "w",
            ATTACHMENT_SESSION_ID,
        );
        std::fs::create_dir_all(index_path.parent().unwrap()).unwrap();
        std::fs::write(&index_path, b"{ this is not valid json").unwrap();

        let result = register_output_attachment(
            &resources,
            "p",
            "w",
            &PreparedOutputHandle::from(&prepared),
            "video/mp4",
            "media_edit: trim".to_string(),
        );
        assert!(
            result.is_err(),
            "a corrupt index must be refused, not silently replaced"
        );

        // The corrupt file must survive untouched — proof nothing overwrote it.
        let after = std::fs::read_to_string(&index_path).unwrap();
        assert_eq!(after, "{ this is not valid json");
        let _ = std::fs::remove_dir_all(&root);
    }
}
