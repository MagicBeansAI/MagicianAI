mod ffmpeg;
mod io;
mod job;
mod ops;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_handlers::staged_file_edit::scope_from_args;
use crate::magician_v2::execution::error::ExecutionError;

use job::{MediaJobRegistry, MediaJobStatus};

fn job_registry() -> &'static Arc<MediaJobRegistry> {
    static REGISTRY: OnceLock<Arc<MediaJobRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(MediaJobRegistry::new()))
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "mp4" | "mov" => "video/mp4",
        "webm" => "video/webm",
        "gif" => "image/gif",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

pub async fn handle_media_edit(
    resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let operation = match args.get("operation").and_then(Value::as_str) {
        Some(op) if !op.trim().is_empty() => op.trim(),
        _ => {
            return Ok(
                json!({"status": "error", "reason": "media_edit requires `operation` (string)"}),
            );
        },
    };

    let registry = ops::registry();
    let Some(op) = registry.get(operation) else {
        let known: Vec<&str> = {
            let mut names: Vec<&str> = registry.keys().copied().collect();
            names.sort_unstable();
            names
        };
        return Ok(json!({
            "status": "error",
            "reason": format!(
                "media_edit: unknown operation `{operation}`; known operations: {}",
                known.join(", ")
            ),
        }));
    };

    let raw_paths: Vec<String> = match args.get("input_paths").and_then(Value::as_array) {
        Some(arr) => arr
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        None => vec![],
    };
    let inputs = match io::resolve_inputs(&resources, &args, &raw_paths) {
        Ok(paths) => paths,
        Err(error) => return Ok(json!({"status": "error", "reason": error.to_string()})),
    };

    let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
    let (principal, workspace) = scope_from_args(&args);

    let probed_duration_secs = if op.needs_probe() {
        match inputs.first() {
            Some(first) => match ffmpeg::probe_duration_secs(first).await {
                Ok(d) => Some(d),
                Err(error) => {
                    return Ok(json!({
                        "status": "error",
                        "reason": format!("media_edit: could not probe input duration: {error}"),
                    }));
                },
            },
            None => None,
        }
    } else {
        None
    };

    let ext = op.output_ext(&params);
    let prepared = match io::prepare_output(&resources, &principal, &workspace, operation, ext) {
        Ok(p) => p,
        Err(error) => return Ok(json!({"status": "error", "reason": error.to_string()})),
    };

    let build_ctx = ops::OpContext {
        inputs: &inputs,
        params: &params,
        output_path: &prepared.path,
        probed_duration_secs,
    };
    let ffmpeg_args = match op.build_args(&build_ctx) {
        Ok(args) => args,
        Err(error) => return Ok(json!({"status": "error", "reason": error.to_string()})),
    };

    let mime_type = mime_for_ext(ext);
    let label = format!("media_edit: {operation}");
    let attachment_principal = principal.clone();
    let attachment_workspace = workspace.clone();
    let attachment_resources = Arc::clone(&resources);
    let prepared_for_callback = io::PreparedOutputHandle::from(&prepared);

    let job_id = match job_registry()
        .spawn(
            ffmpeg::ffmpeg_bin(),
            ffmpeg_args,
            probed_duration_secs,
            prepared.path.clone(),
            move || {
                io::register_output_attachment(
                    &attachment_resources,
                    &attachment_principal,
                    &attachment_workspace,
                    &prepared_for_callback,
                    mime_type,
                    label,
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
    {
        Ok(id) => id,
        Err(error) => return Ok(json!({"status": "error", "reason": error})),
    };

    Ok(json!({
        "status": "running",
        "job_id": job_id,
        "operation": operation,
    }))
}

pub async fn handle_media_edit_status(
    _resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let Some(job_id) = args
        .get("job_id")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
    else {
        return Ok(
            json!({"status": "error", "reason": "media_edit_status requires `job_id` (string)"}),
        );
    };

    match job_registry().status(job_id).await {
        Some(MediaJobStatus::Running { progress_pct }) => Ok(json!({
            "status": "running",
            "progress_pct": progress_pct,
        })),
        Some(MediaJobStatus::Completed { output_path }) => Ok(json!({
            "status": "completed",
            "output_path": output_path.display().to_string(),
        })),
        Some(MediaJobStatus::Failed { error }) => Ok(json!({
            "status": "failed",
            "error": error,
        })),
        None => Ok(json!({
            "status": "error",
            "reason": format!("no such job_id `{job_id}` (jobs do not survive a server restart)"),
        })),
    }
}
