//! Execution-end hook that learns a Task Recipe from a browser-backed run.

use crate::magician_v2::api_mining::recipe_compiler::values::{
    collect_reported_values_for_task, ReportedValues,
};
use crate::magician_v2::api_mining::recipe_compiler::{
    compile_task_recipe_with_evidence, llm_fallback, RecipeCompileInput,
};
use crate::magician_v2::api_mining::recipe_observer::{RecipeEvent, RunObserver};
use crate::magician_v2::api_mining::recipe_store::RecipeStore;
use crate::magician_v2::api_mining::trace_storage::TraceStorage;
use crate::magician_v2::api_mining::types::NetworkTraceEvent;
use crate::magician_v2::artifact_v2::synthesis::ExecutionOutputSynthesisBundle;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use magician_core::prompts::PromptManager;
use std::path::PathBuf;
use std::sync::Arc;

const MAX_COMPILE_TRACE_FILES: usize = 32;
const MAX_COMPILE_TRACE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_COMPILE_TRACE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COMPILE_TRACES: usize = 10_000;
const MAX_TYPED_INPUT_FILES: usize = 32;
const MAX_TYPED_INPUT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TYPED_INPUT_VALUES: usize = 256;
const MAX_TYPED_INPUT_VALUE_BYTES: usize = 64 * 1024;

pub struct ReportedSources {
    pub outcome_summary: String,
    pub artifact_previews: Vec<serde_json::Value>,
    pub output_previews: Vec<String>,
}

/// Artifact types that record how the agent worked, not what it reported:
/// tool-call evidence, inline tool results, fetched files. Their payloads
/// (page dumps, snapshots, tool catalogs) are process, and feeding them to the
/// compiler as "reported values" makes answer coverage unsatisfiable.
const PROCESS_ARTIFACT_TYPES: &[&str] = &[
    "tool_call_evidence",
    "tool_inline_result",
    "tool_result",
    "tool_output_file",
    "downloaded_file",
];

fn is_reported_artifact(
    artifact: &crate::magician_v2::artifact_v2::synthesis::ArtifactEvidence,
) -> bool {
    !PROCESS_ARTIFACT_TYPES.contains(&artifact.artifact_type.as_str())
}

/// Persisted-record keys that describe the artifact rather than carry it.
const ARTIFACT_ENVELOPE_KEYS: &[&str] = &[
    "artifact_id",
    "artifact_kind",
    "artifact_type",
    "content_sha256",
    "content_type",
    "mime_type",
    "created_at",
    "produced_at",
    "display_name",
    "execution_absolute_path",
    "task_absolute_path",
    "relative_path",
    "execution_download_url",
    "task_download_url",
    "execution_id",
    "task_id",
    "source_execution_id",
    "source_artifact_id",
    "size_bytes",
    "read_policy",
    "schema",
];

/// The structured payload worth mining for reported values. A file-backed
/// artifact's payload is its envelope (id, digest, paths) while its content
/// is in `content_preview`, so only an inline artifact's payload counts — and
/// even there the envelope keys are not answers.
fn reported_payload(
    artifact: &crate::magician_v2::artifact_v2::synthesis::ArtifactEvidence,
) -> Option<serde_json::Value> {
    if artifact.content_preview.is_some() {
        return None;
    }
    match &artifact.payload_preview {
        serde_json::Value::Object(map) => {
            let data: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter(|(key, _)| !ARTIFACT_ENVELOPE_KEYS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            (!data.is_empty()).then_some(serde_json::Value::Object(data))
        },
        serde_json::Value::Null => None,
        other => Some(other.clone()),
    }
}

impl ReportedSources {
    pub fn from_bundle(bundle: &ExecutionOutputSynthesisBundle) -> Self {
        let reported = || {
            bundle
                .selected_artifacts
                .iter()
                .filter(|artifact| is_reported_artifact(artifact))
        };
        Self {
            outcome_summary: bundle.outcome.outcome_summary.clone(),
            artifact_previews: reported().filter_map(reported_payload).collect(),
            output_previews: reported()
                .filter_map(|artifact| artifact.content_preview.clone())
                .chain(
                    bundle
                        .child_outputs
                        .iter()
                        .chain(&bundle.source_outputs)
                        .filter_map(|output| output.content_preview.clone()),
                )
                .collect(),
        }
    }
}

pub fn reported_values_from(sources: &ReportedSources) -> ReportedValues {
    reported_values_for_task(sources, "")
}

/// Reported values with the task text steering extraction: a field the task
/// names is read off the summary even when it is neither quoted nor numeric.
pub fn reported_values_for_task(sources: &ReportedSources, task_text: &str) -> ReportedValues {
    let output_previews: Vec<_> = sources.output_previews.iter().map(String::as_str).collect();
    collect_reported_values_for_task(
        &sources.outcome_summary,
        &sources.artifact_previews,
        &output_previews,
        task_text,
    )
}

pub struct RecipeCompileJob {
    pub switch: crate::magician_v2::api_mining::switch::ApiMiningSwitch,
    pub workspace_layout: crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    pub mining_base: PathBuf,
    pub task_id: String,
    pub execution_id: String,
    pub monitor_revision: Option<u32>,
    pub agent_id: String,
    pub principal: String,
    pub workspace: String,
    pub task_title: String,
    pub task_description: String,
    pub sources: ReportedSources,
    pub router: Option<Arc<OperationLlmRouter>>,
    pub prompt_manager: Arc<PromptManager>,
    pub observer: Option<Arc<dyn RunObserver>>,
    pub capability_resolver: Option<Arc<crate::magician_v2::execution::ScopedCapabilityResolver>>,
}

/// Learn and persist a recipe without affecting execution finalization.
pub async fn compile_and_store(job: RecipeCompileJob) -> Option<String> {
    if !job.switch.effective(&job.principal, &job.workspace) {
        return None;
    }
    let storage = TraceStorage::with_base_path(&job.mining_base);
    let files = match files_for_run(&storage, &job.task_id, &job.execution_id) {
        Ok(files) if !files.is_empty() => files,
        Ok(_) => {
            tracing::info!(
                "[API_MINING] recipe compile: no traces for task {}",
                job.task_id
            );
            return None;
        },
        Err(error) => {
            tracing::warn!(
                "[API_MINING] recipe compile: failed to list traces for {}: {error}",
                job.task_id
            );
            return None;
        },
    };
    if files.len() > MAX_COMPILE_TRACE_FILES {
        tracing::warn!(
            task_id = %job.task_id,
            trace_files = files.len(),
            "[API_MINING] recipe compile skipped because the execution trace set exceeded its file limit"
        );
        return None;
    }
    let traces = match complete_traces_for_run(&storage, &files).await {
        Ok(traces) => traces,
        Err(error) => {
            tracing::warn!(
                task_id = %job.task_id,
                %error,
                "[API_MINING] recipe compile skipped because execution evidence is incomplete"
            );
            return None;
        },
    };
    let sequences =
        crate::magician_v2::api_mining::sequence_store::SequenceStore::new(job.mining_base.clone())
            .list_for_task(&job.task_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|sequence| sequence.execution_id == job.execution_id)
            .collect();
    let reported = reported_values_for_task(
        &job.sources,
        &format!("{} {}", job.task_title, job.task_description),
    );
    let input = RecipeCompileInput {
        task_id: job.task_id.clone(),
        execution_id: job.execution_id.clone(),
        monitor_revision: job.monitor_revision,
        agent_id: job.agent_id.clone(),
        principal: job.principal.clone(),
        workspace: job.workspace.clone(),
        task_title: job.task_title.clone(),
        task_text: job.task_description,
        reported,
        traces,
        typed_inputs: typed_inputs_for_run(&job.mining_base, &job.task_id, &job.execution_id),
        trace_files: files
            .iter()
            .map(|file| file.display().to_string())
            .collect(),
        sequences,
    };
    let (mut recipe, compile_evidence) = match compile_task_recipe_with_evidence(&input) {
        Ok(compiled) => compiled,
        Err(error) => {
            tracing::info!(
                "[API_MINING] recipe compile skipped for task {}: {error}",
                job.task_id
            );
            return None;
        },
    };
    if let Some(router) = job.router {
        if let Err(error) = llm_fallback::refine_with_llm(
            router,
            job.prompt_manager,
            &mut recipe,
            &job.task_title,
            &input,
            &compile_evidence,
        )
        .await
        {
            tracing::warn!(
                "[API_MINING] recipe refinement failed for task {}: {error}; keeping deterministic recipe",
                job.task_id
            );
        }
    }
    let validation_inputs = crate::magician_v2::api_mining::recipe_runner::RecipeRunInputs {
        inputs: recipe
            .shape
            .inputs
            .iter()
            .map(|input| (input.name.clone(), input.example_value.clone()))
            .collect(),
        timeout_ms: None,
        approved_write_steps: Default::default(),
    };
    if let Err((class, detail)) =
        crate::magician_v2::api_mining::recipe_runner::validate_recipe_preflight(
            &recipe,
            &validation_inputs,
        )
    {
        tracing::warn!(
            task_id = %job.task_id,
            ?class,
            %detail,
            "[API_MINING] refusing to publish a recipe that failed replay preflight"
        );
        return None;
    }
    // Refinement can await an LLM. Re-check the live switch before the first
    // durable recipe write so a workspace disabled during that await stays off.
    if !job.switch.effective(&job.principal, &job.workspace) {
        return None;
    }
    let store = RecipeStore::new(job.mining_base.clone());
    let _scope_guard = store.scope_mutation_lock().lock_owned().await;
    // A purge disables the scope before waiting on this lock. This check is
    // therefore the publication commit gate, not merely a fast-path check.
    if !job.switch.effective(&job.principal, &job.workspace) {
        return None;
    }
    // A fallback browser run teaches the existing task shape a new version;
    // it must not create a competing recipe that leaves deterministic lookup
    // order deciding which one runs next.
    let mut recompiled = false;
    let in_same_scope = |candidate: &crate::magician_v2::api_mining::recipe::TaskRecipe| {
        candidate.agent_id == recipe.agent_id
            && candidate.scope_principal == recipe.scope_principal
            && candidate.scope_workspace == recipe.scope_workspace
    };
    let existing_for_task = match store.find_by_task(&job.task_id) {
        Ok(recipe) => recipe.filter(&in_same_scope),
        Err(error) => {
            tracing::warn!(
                task_id = %job.task_id,
                %error,
                "[API_MINING] recipe compile could not read the task index"
            );
            return None;
        },
    };
    let existing = if existing_for_task.is_some() {
        existing_for_task
    } else {
        match store.find_by_shape(&recipe.shape.fingerprint) {
            Ok(candidates) => candidates.into_iter().find(&in_same_scope),
            Err(error) => {
                tracing::warn!(
                    task_id = %job.task_id,
                    %error,
                    "[API_MINING] recipe compile could not read the shape index"
                );
                return None;
            },
        }
    };
    let replay_recipe_id = existing
        .as_ref()
        .map(|candidate| candidate.id.as_str())
        .unwrap_or(recipe.id.as_str());
    let replay_lock = match store.replay_lock(replay_recipe_id) {
        Ok(lock) => lock,
        Err(error) => {
            tracing::warn!(
                recipe_id = %replay_recipe_id,
                %error,
                "[API_MINING] recipe compile could not acquire its publication lock"
            );
            return None;
        },
    };
    // Purge and compilation acquire scope → recipe in the same order. Replay
    // acquires only the recipe lock. Reload after waiting so a completed
    // replay's counters and transport hints become the base of the new
    // version instead of being overwritten by the earlier index snapshot.
    let _replay_guard = replay_lock.lock_owned().await;
    let mut previous_recipe = None;
    if let Some(existing) = existing {
        recompiled = true;
        let latest = match store.load(&existing.id) {
            Ok(Some(latest)) => latest,
            Ok(None) => {
                tracing::warn!(
                    recipe_id = %existing.id,
                    "[API_MINING] recipe disappeared before recompilation publication"
                );
                return None;
            },
            Err(error) => {
                tracing::warn!(
                    recipe_id = %existing.id,
                    %error,
                    "[API_MINING] recipe could not be reloaded for recompilation"
                );
                return None;
            },
        };
        previous_recipe = Some(latest.clone());
        recipe = merge_version(latest, recipe);
    }
    if first_blocked_origin(&job.mining_base, &recipe).is_some() {
        return None;
    }
    if let Err(error) = store.save_and_bind(&recipe, &job.task_id) {
        tracing::warn!(
            "[API_MINING] recipe save failed for task {}: {error}",
            job.task_id
        );
        return None;
    }
    // `block-and-purge` may have committed while the atomic recipe write was
    // in progress. Roll back the just-published recipe before any ledger,
    // relevance, or planner-pack side effect can externalize it. This compiler
    // owns only the current recipe's replay lock, so rollback must stay exact;
    // the waiting purge owns the broad origin deletion transaction.
    if let Some(origin) = first_blocked_origin(&job.mining_base, &recipe) {
        let previous_to_restore = previous_recipe
            .as_ref()
            .filter(|previous| first_blocked_origin(&job.mining_base, previous).is_none());
        if let Err(error) =
            rollback_recipe_publication(&store, &recipe, previous_to_restore, &job.task_id)
        {
            tracing::warn!(
                recipe_id = %recipe.id,
                %origin,
                %error,
                "[API_MINING] blocked-origin recipe rollback failed"
            );
        }
        return None;
    }
    if !job.switch.effective(&job.principal, &job.workspace) {
        if let Err(error) =
            rollback_recipe_publication(&store, &recipe, previous_recipe.as_ref(), &job.task_id)
        {
            tracing::warn!(
                recipe_id = %recipe.id,
                %error,
                "[API_MINING] failed to roll back a recipe published while the feature switch turned off"
            );
        }
        return None;
    }
    let learned_run =
        crate::magician_v2::api_mining::recipe_runs::RecipeRunRecord::learned_from_browser(
            &recipe,
            job.task_id.clone(),
            job.execution_id.clone(),
            recompiled,
        );
    if let Err(error) =
        crate::magician_v2::api_mining::recipe_runs::RecipeRunLedger::new(&job.mining_base)
            .append(&learned_run)
            .await
    {
        tracing::warn!(
            recipe_id = %recipe.id,
            %error,
            "[API_MINING] recipe learned but browser run ledger append failed"
        );
    }
    if !job.switch.effective(&job.principal, &job.workspace) {
        return None;
    }
    if recompiled {
        if let Some(observer) = &job.observer {
            observer.observe(RecipeEvent::Recompiled {
                recipe_id: &recipe.id,
                version: recipe.current_version,
            });
        }
    }
    if let Err(error) = crate::magician_v2::api_mining::relevance::link_recipe_capabilities(
        &job.mining_base,
        &recipe,
    ) {
        tracing::warn!(
            recipe_id = %recipe.id,
            %error,
            "[API_MINING] recipe saved but capability relevance links could not be updated"
        );
    }
    if !job.switch.effective(&job.principal, &job.workspace) {
        return None;
    }
    let pack_store = crate::magician_v2::execution::CapabilityPackStore::with_workspace_layout(
        &job.workspace_layout,
        &recipe.scope_principal,
        &recipe.scope_workspace,
    );
    match crate::magician_v2::api_mining::recipe_packs::publish_recipe_pack(
        &pack_store,
        &job.workspace_layout
            .scope_skills_root(&recipe.scope_principal, &recipe.scope_workspace),
        &recipe,
    ) {
        Ok(true) => {
            if let Some(resolver) = &job.capability_resolver {
                resolver.invalidate_scope(&recipe.scope_principal, &recipe.scope_workspace);
            }
        },
        Ok(false) => {},
        Err(error) => tracing::warn!(
            recipe_id = %recipe.id,
            %error,
            "[API_MINING] recipe saved but planner pack upsert failed"
        ),
    }
    tracing::info!(
        "[API_MINING] recipe {} compiled for task {} ({} steps, {} inputs)",
        recipe.id,
        job.task_id,
        recipe.current().map_or(0, |version| version.steps.len()),
        recipe.shape.inputs.len()
    );
    Some(recipe.id)
}

fn rollback_recipe_publication(
    store: &RecipeStore,
    published: &crate::magician_v2::api_mining::recipe::TaskRecipe,
    previous: Option<&crate::magician_v2::api_mining::recipe::TaskRecipe>,
    task_id: &str,
) -> std::io::Result<()> {
    if let Some(previous) = previous {
        store.save_and_bind(previous, task_id)
    } else {
        store.remove_recipe(&published.id).map(|_| ())
    }
}

fn first_blocked_origin(
    mining_base: &std::path::Path,
    recipe: &crate::magician_v2::api_mining::recipe::TaskRecipe,
) -> Option<String> {
    let policies =
        crate::magician_v2::api_mining::origin_policy::OriginPolicyStore::open(mining_base);
    recipe
        .versions
        .iter()
        .flat_map(|version| version.origins.iter())
        .find(|origin| policies.is_blocked(origin))
        .cloned()
}

fn merge_version(
    mut existing: crate::magician_v2::api_mining::recipe::TaskRecipe,
    mut learned: crate::magician_v2::api_mining::recipe::TaskRecipe,
) -> crate::magician_v2::api_mining::recipe::TaskRecipe {
    let next = existing
        .versions
        .iter()
        .map(|version| version.version)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    if let Some(mut version) = learned.versions.pop() {
        version.version = next;
        existing.versions.push(version);
        existing.current_version = next;
        existing.shape = learned.shape;
    }
    existing
}

async fn complete_traces_for_run(
    storage: &TraceStorage,
    files: &[PathBuf],
) -> Result<Vec<NetworkTraceEvent>, String> {
    if files.len() > MAX_COMPILE_TRACE_FILES {
        return Err("execution trace set exceeds the file limit".into());
    }
    let mut traces = Vec::new();
    let mut trace_bytes = 0u64;
    for file in files {
        let (mut file_traces, bytes) = storage
            .read_complete_traces(
                file,
                MAX_COMPILE_TRACE_FILE_BYTES.min(MAX_COMPILE_TRACE_BYTES - trace_bytes),
                MAX_COMPILE_TRACES - traces.len(),
            )
            .await?;
        trace_bytes += bytes;
        traces.append(&mut file_traces);
    }
    Ok(traces)
}

fn files_for_run(
    storage: &TraceStorage,
    _task_id: &str,
    execution_id: &str,
) -> Result<Vec<std::path::PathBuf>, String> {
    // A task can have many historical runs. Absence of current evidence does
    // not prove legacy task-scoped files belong to this execution.
    storage.list_trace_files(execution_id)
}

fn typed_inputs_for_run(base: &std::path::Path, task_id: &str, execution_id: &str) -> Vec<String> {
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    let layout = ArtifactV2Workspace::with_local_file_provider(base);
    let read_entries = |record_id: &str| {
        let directory = crate::magician_v2::api_mining::types::safe_join(base, record_id).ok()?;
        layout.read_dir_path_sync(&directory).ok()
    };
    let entries = read_entries(execution_id)
        .filter(|entries| {
            entries.iter().any(|entry| {
                entry.is_file
                    && entry.file_name.starts_with("actions_")
                    && entry.file_name.ends_with(".jsonl")
            })
        })
        .unwrap_or_default();
    let action_entries: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            entry.is_file
                && entry.file_name.starts_with("actions_")
                && entry.file_name.ends_with(".jsonl")
        })
        .collect();
    if action_entries.len() > MAX_TYPED_INPUT_FILES {
        tracing::warn!(
            task_id,
            execution_id,
            action_files = action_entries.len(),
            "[API_MINING] typed-input collection skipped because its file limit was exceeded"
        );
        return Vec::new();
    }
    let mut values = Vec::new();
    for entry in action_entries {
        let path = base.join(&entry.relative_path);
        if std::fs::metadata(&path)
            .ok()
            .is_some_and(|metadata| metadata.len() > MAX_TYPED_INPUT_FILE_BYTES)
        {
            tracing::warn!(
                task_id,
                execution_id,
                action_file = %path.display(),
                "[API_MINING] oversized action file omitted from typed-input collection"
            );
            continue;
        }
        let Ok(text) = layout.read_to_string_path_sync(&path) else {
            continue;
        };
        for line in text.lines() {
            if let Ok(event) = serde_json::from_str::<
                crate::magician_v2::api_mining::correlator::ActionEvent,
            >(line)
            {
                for value in event.user_values {
                    if value.len() <= MAX_TYPED_INPUT_VALUE_BYTES {
                        values.push(value);
                    }
                    if values.len() >= MAX_TYPED_INPUT_VALUES {
                        break;
                    }
                }
                if values.len() >= MAX_TYPED_INPUT_VALUES {
                    break;
                }
            }
        }
        if values.len() >= MAX_TYPED_INPUT_VALUES {
            break;
        }
    }
    values.sort();
    values.dedup();
    values
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::recipe::{
        CompiledFrom, RecipeAuth, RecipeMaturity, RecipeShape, RecipeVersion, TaskRecipe,
    };
    use crate::magician_v2::api_mining::workflow::ReplayStats;

    fn rollback_fixture(id: &str, version: u32) -> TaskRecipe {
        TaskRecipe {
            id: id.into(),
            scope_principal: "owner".into(),
            scope_workspace: "default".into(),
            agent_id: "assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "fixture".into(),
                fingerprint: format!("shape_{version}"),
                inputs: Vec::new(),
            },
            current_version: version,
            versions: vec![RecipeVersion {
                version,
                origins: Vec::new(),
                steps: Vec::new(),
                data_flows: Vec::new(),
                answer_spec: Vec::new(),
                auth: RecipeAuth::default(),
                maturity: RecipeMaturity::Draft,
                replay_stats: ReplayStats::default(),
                compiled_from: CompiledFrom {
                    task_id: "task".into(),
                    execution_id: "execution".into(),
                    task_text_fingerprint: None,
                    monitor_revision: None,
                    sequence_ids: Vec::new(),
                    trace_files: Vec::new(),
                },
                compiled_at_ms: i64::from(version),
                last_replayed_at_ms: None,
            }],
        }
    }

    #[test]
    fn reported_values_come_from_summary_artifacts_and_outputs() {
        let sources = ReportedSources {
            outcome_summary: "Coke Zero 300ml: ₹40 at Zepto".into(),
            artifact_previews: vec![serde_json::json!({
                "product": "Coke Zero 300ml",
                "price": 40
            })],
            output_previews: vec!["Price: ₹40".into()],
        };
        let values = reported_values_from(&sources);
        assert!(values.values.iter().any(|value| value.normalized == "40"));
        assert!(values
            .values
            .iter()
            .any(|value| value.field.as_deref() == Some("product")));
    }

    #[test]
    fn process_evidence_is_not_a_reported_value() {
        use crate::magician_v2::artifact_v2::synthesis::ArtifactEvidence;
        let artifact = |artifact_type: &str, payload: serde_json::Value| ArtifactEvidence {
            artifact_id: format!("{artifact_type}:1"),
            artifact_type: artifact_type.into(),
            content_type: "application/json".into(),
            source_execution_id: None,
            source_artifact_id: None,
            payload_preview: payload,
            content_preview: None,
            display_name: None,
            tool_name: None,
            task_absolute_path: None,
            execution_absolute_path: None,
            execution_download_url: None,
            task_download_url: None,
        };
        assert!(!is_reported_artifact(&artifact(
            "tool_call_evidence",
            serde_json::json!({"action_tool": "browser__eval", "duration_ms": 1234})
        )));
        assert!(!is_reported_artifact(&artifact(
            "tool_inline_result",
            serde_json::json!({"bodyText": "Rust 2031 roadmap 3119 points"})
        )));
        assert!(is_reported_artifact(&artifact(
            "task_deliverable",
            serde_json::json!({"points": 3119})
        )));
        assert!(is_reported_artifact(&artifact(
            "json",
            serde_json::json!({"price": 40})
        )));

        // A file-backed deliverable's payload is its envelope; the content
        // preview carries the report. An inline data artifact keeps its data
        // minus the envelope keys.
        let mut deliverable = artifact(
            "task_deliverable",
            serde_json::json!({
                "artifact_id": "terminal_decision:0:5d9c9a2cb2a7f202",
                "content_sha256": "cceaa2f9",
                "size_bytes": 85
            }),
        );
        deliverable.content_preview = Some("verified the first result has 3119 points.".into());
        assert_eq!(reported_payload(&deliverable), None);
        let inline = artifact(
            "json",
            serde_json::json!({"artifact_id": "tool_inline_result:1", "price": 40, "product": "Coke Zero"}),
        );
        assert_eq!(
            reported_payload(&inline),
            Some(serde_json::json!({"price": 40, "product": "Coke Zero"}))
        );
    }

    #[test]
    fn run_files_prefer_execution_scope_without_blending_task_history() {
        let temp = tempfile::tempdir().unwrap();
        let task_dir = temp.path().join("task_1");
        let execution_dir = temp.path().join("exec_2");
        std::fs::create_dir_all(&task_dir).unwrap();
        std::fs::create_dir_all(&execution_dir).unwrap();
        std::fs::write(task_dir.join("trace_old.jsonl"), b"{}\n").unwrap();
        std::fs::write(execution_dir.join("trace_current.jsonl"), b"{}\n").unwrap();
        let storage = TraceStorage::with_base_path(temp.path());

        let files = files_for_run(&storage, "task_1", "exec_2").unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].file_name().and_then(|value| value.to_str()),
            Some("trace_current.jsonl")
        );
        assert!(files_for_run(&storage, "task_1", "exec_without_capture")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn typed_inputs_prefer_the_current_execution() {
        let temp = tempfile::tempdir().unwrap();
        let task_dir = temp.path().join("task_1");
        let execution_dir = temp.path().join("exec_2");
        std::fs::create_dir_all(&task_dir).unwrap();
        std::fs::create_dir_all(&execution_dir).unwrap();
        std::fs::write(
            task_dir.join("actions_old.jsonl"),
            br#"{"action_id":"a1","action_type":"type","timestamp_ms":1,"user_values":["old-value"]}
"#,
        )
        .unwrap();
        std::fs::write(
            execution_dir.join("actions_current.jsonl"),
            br#"{"action_id":"a2","action_type":"type","timestamp_ms":2,"user_values":["current-value"]}
"#,
        )
        .unwrap();

        let values = typed_inputs_for_run(temp.path(), "task_1", "exec_2");

        assert_eq!(values, vec!["current-value"]);
        assert!(typed_inputs_for_run(temp.path(), "task_1", "exec_without_actions").is_empty());
    }

    #[tokio::test]
    async fn compile_evidence_never_keeps_only_the_read_after_losing_a_trace_file() {
        let temp = tempfile::tempdir().unwrap();
        let storage = TraceStorage::with_base_path(temp.path());
        let trace: NetworkTraceEvent = serde_json::from_value(serde_json::json!({
            "request_id": "answer-read", "method": "GET", "url": "https://example.test/items",
            "request_headers": {}, "response_headers": {}, "response_body": "{\"label\":\"new\"}",
            "status": 200, "timing": {"request_time": 0.0, "total_duration": 1.0},
            "initiator": {"initiator_type": "script"}, "timestamp": 1,
            "request_size": 0, "response_size": 15
        }))
        .unwrap();
        let good = storage.write_traces("execution", &[trace]).unwrap();
        assert_eq!(
            complete_traces_for_run(&storage, &[good.clone()])
                .await
                .unwrap()
                .len(),
            1
        );
        let corrupt = temp.path().join("trace_lost_write.jsonl");
        std::fs::write(&corrupt, b"{incomplete mutation}\n").unwrap();
        for lost in [corrupt, temp.path().join("missing.jsonl")] {
            assert!(complete_traces_for_run(&storage, &[good.clone(), lost])
                .await
                .is_err());
        }
    }

    #[test]
    fn switch_race_rollback_removes_a_new_recipe_and_its_binding() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecipeStore::new(temp.path().to_path_buf());
        let published = rollback_fixture("rcp_new", 1);
        let unrelated = rollback_fixture("rcp_unrelated", 7);
        store.save_and_bind(&unrelated, "other_task").unwrap();
        store.save_and_bind(&published, "task").unwrap();

        rollback_recipe_publication(&store, &published, None, "task").unwrap();

        assert!(store.load("rcp_new").unwrap().is_none());
        assert!(store.find_by_task("task").unwrap().is_none());
        assert!(store.find_by_shape("shape_1").unwrap().is_empty());
        assert_eq!(
            store
                .find_by_task("other_task")
                .unwrap()
                .unwrap()
                .current_version,
            7
        );
    }

    #[test]
    fn switch_race_rollback_restores_the_previous_recipe_version() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecipeStore::new(temp.path().to_path_buf());
        let previous = rollback_fixture("rcp_existing", 1);
        let published = rollback_fixture("rcp_existing", 2);
        store.save_and_bind(&previous, "task").unwrap();
        store.save_and_bind(&published, "task").unwrap();

        rollback_recipe_publication(&store, &published, Some(&previous), "task").unwrap();

        let restored = store.load("rcp_existing").unwrap().unwrap();
        assert_eq!(restored.current_version, 1);
        assert_eq!(
            store.find_by_task("task").unwrap().unwrap().current_version,
            1
        );
        assert!(store.find_by_shape("shape_2").unwrap().is_empty());
        assert_eq!(store.find_by_shape("shape_1").unwrap().len(), 1);
    }
}
