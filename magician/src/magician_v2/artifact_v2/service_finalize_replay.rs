//! Replays one finished execution's finalization — the execution output
//! (`v3_execution_output_synthesis`), then the task-user projection with its
//! claim check, repair and caveat — through the same finalizers the service
//! runs, on a COPY of the execution's task directory.
//!
//! The source runtime is only read; everything is written under the output
//! directory. The caller supplies the synthesis dependencies, so the final
//! answer eval (`magician-bin/examples/final_answer_live_eval.rs`) runs it
//! with the live model router.

use super::super::synthesis::render_synthesis_bundle_section;
use super::*;
use std::path::{Path, PathBuf};

pub struct FinalizeReplayRequest<'a> {
    /// Runtime root the execution ran under (read only).
    pub runtime_root: &'a Path,
    pub principal: &'a str,
    pub workspace: &'a str,
    pub task_id: &'a str,
    pub execution_id: &'a str,
    /// Fresh directory: the task copy goes under `runtime/`, the rendered
    /// bundles beside it.
    pub output_dir: &'a Path,
    pub dependencies: SynthesisDependencies,
    /// Text replacements applied to the agent's own answer (its terminal
    /// files and outcome summary), never to tool results: a negative case
    /// plants an error the evidence does not support.
    pub alter_answer: &'a [(String, String)],
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalizeReplayResult {
    pub execution_output: String,
    pub execution_media_type: String,
    pub user_output: String,
    pub user_media_type: String,
    /// `answer_claim_check.json` beside the published answer, when written.
    pub claim_check: Option<Value>,
}

fn io_error(context: &str, error: std::io::Error) -> ArtifactV2Error {
    ArtifactV2Error::Runtime(format!("finalize replay: {context}: {error}"))
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), ArtifactV2Error> {
    std::fs::create_dir_all(to).map_err(|error| io_error("create copy", error))?;
    for entry in std::fs::read_dir(from).map_err(|error| io_error("read source", error))? {
        let entry = entry.map_err(|error| io_error("read source entry", error))?;
        let target = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| io_error("source file type", error))?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).map_err(|error| io_error("copy file", error))?;
        }
    }
    Ok(())
}

/// The runtime's own terminal record: outcome label and summary, exactly as
/// the service received them.
fn terminal_outcome(journal: &Path) -> Result<(String, String), ArtifactV2Error> {
    let text =
        std::fs::read_to_string(journal).map_err(|error| io_error("runtime journal", error))?;
    let payload = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|line| line.get("body").cloned())
        .find(|body| {
            body.get("event_type").and_then(Value::as_str) == Some("AgenticExecutionCompleted")
        })
        .and_then(|body| body.get("payload").cloned())
        .ok_or_else(|| {
            ArtifactV2Error::Runtime(format!(
                "finalize replay: no AgenticExecutionCompleted in {}",
                journal.display()
            ))
        })?;
    Ok((
        payload["outcome"]
            .as_str()
            .unwrap_or("completed")
            .to_string(),
        payload["summary"].as_str().unwrap_or_default().to_string(),
    ))
}

fn alter_text(text: &str, alter_answer: &[(String, String)]) -> String {
    alter_answer
        .iter()
        .fold(text.to_string(), |text, (from, to)| {
            text.replace(from.as_str(), to)
        })
}

/// Applies `alter_answer` to every `terminal_decision:` file the copied
/// artifact index names, and records the new digest and size so the file
/// still reads as the run's own.
fn alter_terminal_files(
    index: &Path,
    alter_answer: &[(String, String)],
) -> Result<(), ArtifactV2Error> {
    use sha2::{Digest, Sha256};
    let text =
        std::fs::read_to_string(index).map_err(|error| io_error("read artifact index", error))?;
    let mut records: Value = serde_json::from_str(&text).map_err(|error| {
        ArtifactV2Error::Runtime(format!("finalize replay: artifact index: {error}"))
    })?;
    let items = match &mut records {
        Value::Array(items) => Some(items),
        Value::Object(map) => map.values_mut().find_map(|value| value.as_array_mut()),
        _ => None,
    };
    let mut altered = 0usize;
    for record in items.into_iter().flatten() {
        let is_terminal = record["artifact_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("terminal_decision:"));
        let Some(path) = record["payload"]["execution_absolute_path"]
            .as_str()
            .map(PathBuf::from)
        else {
            continue;
        };
        if !is_terminal {
            continue;
        }
        let Ok(before) = std::fs::read_to_string(&path) else {
            continue;
        };
        let after = alter_text(&before, alter_answer);
        if after == before {
            continue;
        }
        std::fs::write(&path, &after).map_err(|error| io_error("write altered answer", error))?;
        record["payload"]["content_sha256"] =
            Value::String(hex::encode(Sha256::digest(after.as_bytes())));
        record["payload"]["size_bytes"] = Value::from(after.len());
        altered += 1;
    }
    if altered == 0 {
        return Err(ArtifactV2Error::Runtime(
            "finalize replay: alter_answer matched none of the agent's terminal files".to_string(),
        ));
    }
    std::fs::write(
        index,
        serde_json::to_vec_pretty(&records).unwrap_or_default(),
    )
    .map_err(|error| io_error("rewrite artifact index", error))
}

pub async fn replay_finalization(
    request: FinalizeReplayRequest<'_>,
) -> Result<FinalizeReplayResult, ArtifactV2Error> {
    let FinalizeReplayRequest {
        runtime_root,
        principal,
        workspace: workspace_name,
        task_id,
        execution_id,
        output_dir,
        dependencies,
        alter_answer,
    } = request;
    if output_dir.exists() {
        return Err(ArtifactV2Error::Runtime(format!(
            "finalize replay: {} must be a fresh directory",
            output_dir.display()
        )));
    }
    let source = ArtifactV2Workspace::new(runtime_root);
    let source_task = source.task_dir(principal, workspace_name, task_id);
    let replay_root = output_dir.join("runtime");
    let workspace = ArtifactV2Workspace::new(&replay_root);
    let task_dir = workspace.internal_task_dir(principal, workspace_name, task_id);
    copy_tree(&source_task, &task_dir)?;
    let execution_dir = task_dir.join("executions").join(execution_id);
    // The run's recorded effects resolve the result references its answer
    // cites.
    let source_effects = source
        .runtime_root(principal, workspace_name)
        .join("executions")
        .join(execution_id)
        .join("effects");
    if source_effects.is_dir() {
        copy_tree(
            &source_effects,
            &workspace
                .runtime_root(principal, workspace_name)
                .join("executions")
                .join(execution_id)
                .join("effects"),
        )?;
    }

    // Recorded absolute paths point at the source task; repoint them at the
    // copy (both spellings, since the runtime root may be a symlink).
    let index = execution_dir.join("artifacts/persisted_artifacts.json");
    if let Ok(mut text) = std::fs::read_to_string(&index) {
        let canonical = std::fs::canonicalize(&source_task).unwrap_or(source_task.clone());
        for spelling in [&source_task, &canonical] {
            text = text.replace(&*spelling.to_string_lossy(), &*task_dir.to_string_lossy());
        }
        std::fs::write(&index, text).map_err(|error| io_error("rewrite artifact index", error))?;
    }

    if !alter_answer.is_empty() {
        alter_terminal_files(&index, alter_answer)?;
    }

    // Back to the state finalization starts from: this execution's own
    // outputs and the task's answer sidecars do not exist yet.
    let _ = std::fs::remove_file(execution_dir.join(format!("outputs/out_exec_{execution_id}.md")));
    if let Ok(entries) = std::fs::read_dir(task_dir.join("outputs")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if (name.starts_with("out_task_user_") && name.contains(execution_id))
                || matches!(
                    name.as_str(),
                    ANSWER_CLAIM_CHECK_SIDECAR_FILE
                        | "task_summary.json"
                        | VOICE_SPEECH_SIDECAR_FILE
                )
            {
                std::fs::remove_file(entry.path())
                    .map_err(|error| io_error("remove published output", error))?;
            }
        }
    }

    let builder = Arc::new(FilesystemArtifactV2BundleBuilder::new(workspace.clone()));
    let scope = ScopeRef::system_internal_unauthenticated(principal, workspace_name);
    let task = builder.load_task_record(&scope, task_id).await?;
    let execution = builder
        .load_execution_record(&scope, task_id, execution_id)
        .await?;
    let plan_ref = execution.refs.plan_refs.first().cloned().ok_or_else(|| {
        ArtifactV2Error::Runtime("finalize replay: execution has no plan ref".to_string())
    })?;
    let journal: PathBuf = source
        .runtime_root(principal, workspace_name)
        .join("executions")
        .join(execution_id)
        .join("journal.jsonl");
    let (outcome_type, mut outcome_summary) = terminal_outcome(&journal)?;
    outcome_summary = alter_text(&outcome_summary, alter_answer);
    let execution_status = execution.state.status.clone();

    let context = ExecutionContext {
        principal: principal.to_string(),
        workspace: workspace_name.to_string(),
        task_id: task_id.to_string(),
        execution_id: execution_id.to_string(),
    };
    let registry = Arc::new(OutputWriterRegistry::with_default_text_writers());
    let formats: Arc<dyn AllowedOutputFormatsPolicy> = Arc::new(DefaultAllowedOutputFormatsPolicy);
    let execution_ctx = ExecutionFinalizeContext {
        execution: context.clone(),
        task_dir: task_dir.clone(),
        execution_dir,
        manifest: task.manifest.clone(),
        plan_ref: plan_ref.clone(),
        execution_status: execution_status.clone(),
        outcome_type: outcome_type.clone(),
        outcome_summary: outcome_summary.clone(),
        source_output_ids: Vec::new(),
        llm_routing_overrides: None,
        verified_terminal_projection: None,
        now: now_rfc3339(),
    };
    if let Ok(bundle) = builder.build_execution_output_bundle(&execution_ctx).await {
        let section = render_synthesis_bundle_section(&bundle)?;
        std::fs::write(output_dir.join("execution_bundle.txt"), section)
            .map_err(|error| io_error("write execution bundle", error))?;
    }
    let execution_output = FilesystemExecutionFinalizer::new(
        workspace.clone(),
        builder.clone(),
        dependencies.clone(),
        Arc::clone(&registry),
        Arc::clone(&formats),
        Arc::new(PromptManagerExecutionOutputSynthesizer::new(
            dependencies.clone(),
        )),
    )
    .finalize(&execution_ctx)
    .await?;

    let task_ctx = TaskFinalizeContext {
        execution: context,
        task_dir: task_dir.clone(),
        manifest: task.manifest.clone(),
        task_output_mode: execution.state.task_output_mode.clone(),
        task_projection_execution_id: execution_id.to_string(),
        execution_output: execution_output.clone(),
        plan_ref,
        execution_status,
        outcome_type,
        outcome_summary,
        source_output_ids: Vec::new(),
        verified_terminal_projection: None,
        llm_routing_overrides: None,
        delivered_files: Vec::new(),
        now: now_rfc3339(),
    };
    if let Ok(bundle) = builder.build_task_user_output_bundle(&task_ctx).await {
        let section = render_synthesis_bundle_section(&bundle)?;
        std::fs::write(output_dir.join("task_user_bundle.txt"), section)
            .map_err(|error| io_error("write task-user bundle", error))?;
    }
    let user_output = FilesystemTaskUserFinalizer::new(
        workspace,
        builder,
        dependencies.clone(),
        registry,
        formats,
        Arc::new(PromptManagerTaskUserOutputSynthesizer::new(dependencies)),
    )
    .finalize_projection(&task_ctx, false)
    .await?;

    let read =
        |relative: &str| std::fs::read_to_string(task_dir.join(relative)).unwrap_or_default();
    Ok(FinalizeReplayResult {
        execution_output: read(&execution_output.relative_path),
        execution_media_type: execution_output.media_type,
        user_output: read(&user_output.relative_path),
        user_media_type: user_output.media_type,
        claim_check: std::fs::read_to_string(
            task_dir
                .join("outputs")
                .join(ANSWER_CLAIM_CHECK_SIDECAR_FILE),
        )
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok()),
    })
}
