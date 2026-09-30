use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use serde_json::Value;
use tokio::fs;

use crate::magician_v2::artifact_v2::{memory::V3EpisodeRecord, workspace::ArtifactV2Workspace};

#[derive(Debug, Clone, Serialize)]
pub struct TraceBlob {
    pub relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    pub encoding: String,
    pub truncated: bool,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedTrace {
    pub episode: V3EpisodeRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_manifest: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_refs: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_refs: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_events: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_output: Option<TraceBlob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_agent_output: Option<TraceBlob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_user_output: Option<TraceBlob>,
}

#[derive(Debug, Clone)]
pub struct HarnessTraceReader {
    workspace: ArtifactV2Workspace,
}

impl HarnessTraceReader {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    pub async fn load_trace(
        &self,
        episode: V3EpisodeRecord,
        output_max_chars: usize,
    ) -> Result<LoadedTrace, String> {
        let principal = episode.principal.as_deref().ok_or_else(|| {
            format!(
                "episode `{}` is missing principal scope",
                episode.episode_id
            )
        })?;
        let workspace = episode.workspace.as_deref().ok_or_else(|| {
            format!(
                "episode `{}` is missing workspace scope",
                episode.episode_id
            )
        })?;
        let provenance = episode
            .provenance
            .clone()
            .ok_or_else(|| format!("episode `{}` has no trace provenance", episode.episode_id))?;
        let scope_root = self.workspace.scope_root(principal, workspace);

        let execution_output_relative_path = provenance.execution_output_relative_path.clone();
        let task_agent_output_relative_path = provenance.task_agent_output_relative_path.clone();
        let task_user_output_relative_path = provenance.task_user_output_relative_path.clone();

        Ok(LoadedTrace {
            task_manifest: read_json_value(&scope_root, &provenance.task_manifest_relative_path)
                .await?,
            task_state: read_json_value(&scope_root, &provenance.task_state_relative_path).await?,
            task_refs: read_json_value(&scope_root, &provenance.task_refs_relative_path).await?,
            execution_state: read_json_value(
                &scope_root,
                &provenance.execution_state_relative_path,
            )
            .await?,
            execution_refs: read_json_value(&scope_root, &provenance.execution_refs_relative_path)
                .await?,
            execution_events: read_jsonl_values(
                &scope_root,
                &provenance.execution_events_relative_path,
            )
            .await?,
            execution_output: read_blob(
                &scope_root,
                &execution_output_relative_path,
                None,
                output_max_chars,
            )
            .await?,
            task_agent_output: read_blob_opt(
                &scope_root,
                task_agent_output_relative_path.as_deref(),
                None,
                output_max_chars,
            )
            .await?,
            task_user_output: read_blob_opt(
                &scope_root,
                task_user_output_relative_path.as_deref(),
                None,
                output_max_chars,
            )
            .await?,
            episode,
        })
    }
}

async fn read_json_value(
    scope_root: &std::path::Path,
    relative_path: &str,
) -> Result<Option<Value>, String> {
    if relative_path.trim().is_empty() {
        return Ok(None);
    }
    let path = scope_root.join(relative_path);
    if !fs::try_exists(&path)
        .await
        .map_err(|error| format!("failed to check trace path `{}`: {error}", path.display()))?
    {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .await
        .map_err(|error| format!("failed to read trace json `{}`: {error}", path.display()))?;
    serde_json::from_str::<Value>(&raw)
        .map(Some)
        .map_err(|error| format!("failed to parse trace json `{}`: {error}", path.display()))
}

async fn read_jsonl_values(
    scope_root: &std::path::Path,
    relative_path: &str,
) -> Result<Vec<Value>, String> {
    if relative_path.trim().is_empty() {
        return Ok(Vec::new());
    }
    let path = scope_root.join(relative_path);
    if !fs::try_exists(&path)
        .await
        .map_err(|error| format!("failed to check trace jsonl `{}`: {error}", path.display()))?
    {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)
        .await
        .map_err(|error| format!("failed to read trace jsonl `{}`: {error}", path.display()))?;
    let mut values = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(trimmed).map_err(|error| {
            format!(
                "failed to parse trace jsonl `{}` line {}: {error}",
                path.display(),
                index + 1
            )
        })?;
        values.push(value);
    }
    Ok(values)
}

async fn read_blob_opt(
    scope_root: &std::path::Path,
    relative_path: Option<&str>,
    media_type: Option<String>,
    output_max_chars: usize,
) -> Result<Option<TraceBlob>, String> {
    match relative_path {
        Some(path) if !path.trim().is_empty() => {
            read_blob(scope_root, path, media_type, output_max_chars).await
        },
        _ => Ok(None),
    }
}

async fn read_blob(
    scope_root: &std::path::Path,
    relative_path: &str,
    media_type: Option<String>,
    output_max_chars: usize,
) -> Result<Option<TraceBlob>, String> {
    let path = scope_root.join(relative_path);
    if !fs::try_exists(&path)
        .await
        .map_err(|error| format!("failed to check output blob `{}`: {error}", path.display()))?
    {
        return Ok(None);
    }

    let bytes = fs::read(&path)
        .await
        .map_err(|error| format!("failed to read output blob `{}`: {error}", path.display()))?;

    let (encoding, mut content) = match String::from_utf8(bytes.clone()) {
        Ok(text) => ("utf-8".to_string(), text),
        Err(_) => ("base64".to_string(), STANDARD.encode(bytes)),
    };

    let mut truncated = false;
    if output_max_chars > 0 && content.chars().count() > output_max_chars {
        content = content.chars().take(output_max_chars).collect();
        truncated = true;
    }

    Ok(Some(TraceBlob {
        relative_path: relative_path.to_string(),
        media_type,
        encoding,
        truncated,
        content,
    }))
}
