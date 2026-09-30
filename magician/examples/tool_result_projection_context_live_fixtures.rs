//! Runtime-backed fixture generator for the cost-bearing projection/context eval.
//!
//! The live evaluator must grade values produced by the same materialization,
//! projection, continuation-read, authority, and staged-deadline code used by
//! Magician. Keeping provider calls in Python is convenient for reporting, but
//! hand-authoring the projected side there would turn the eval into a prompt
//! microbenchmark rather than an activation gate.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use magician::{
    config::{load_magician_config_from_path, AgentSurfaceResultProjectionBudgetConfig},
    magician_v2::{
        agents::{FeatureMode, InvocationSurface},
        artifact_v2::{service::ScopeRef, workspace::ArtifactV2Workspace},
        context_retrieval::{
            retrieve_bound_staged_triple, ContextRetrievalPolicy, ContextRetrievalRequest,
            ContextStageDescriptor, ContextStageKind, ContextStageState, StagedValue,
        },
        tool_result_materialization::{
            model_lossless_read_success_payload, CanonicalRawResultStore, CanonicalResultError,
            RawResultOwner, RawResultReadContext, RawResultReadRequest, ResultAuthorityBinding,
            ResultRetentionClass, ScopedResultRef, SnapshotResultAuthority,
        },
        tool_result_projection::{
            ProjectionContractId, ProjectionContractRegistry, ToolOutcome, ToolOutcomeStatus,
        },
        tool_result_runtime::{
            materialize_and_project_tool_result, projection_budget_from_config,
            MaterializeAndProjectToolResultRequest,
        },
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::Instant;

#[derive(Debug, Parser)]
#[command(about = "Generate production-backed tool projection/context live-eval fixtures")]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    workspace_root: PathBuf,
    #[arg(long)]
    config: PathBuf,
}

#[derive(Debug, Deserialize)]
struct FixtureInput {
    cases: Vec<FixtureCase>,
}

#[derive(Debug, Deserialize)]
struct FixtureCase {
    name: String,
    surface: String,
    question: String,
    raw: Value,
    expected_outcome_status: ToolOutcomeStatus,
    #[serde(default)]
    contract_id: Option<String>,
    #[serde(default)]
    staged: Option<StagedFixture>,
    #[serde(default)]
    continuation_probe: bool,
    #[serde(default)]
    revocation_probe: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct StagedFixture {
    fast_memory: StageFixture,
    hybrid_memory: StageFixture,
    procedures: StageFixture,
}

#[derive(Debug, Clone, Deserialize)]
struct StageFixture {
    state: String,
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    delay_ms: u64,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    retryable: bool,
}

#[derive(Debug, Serialize)]
struct FixtureOutput {
    schema_version: &'static str,
    runtime_backed: bool,
    cases: Vec<GeneratedCase>,
}

#[derive(Debug, Serialize)]
struct GeneratedCase {
    name: String,
    surface: String,
    projected: Value,
    runtime: Value,
}

fn surface_projection_budget<'a>(
    surface: &str,
    config: &'a magician::config::AgentSurfaceRuntimeConfig,
) -> Result<&'a AgentSurfaceResultProjectionBudgetConfig> {
    match surface {
        "chat" => Ok(&config.result_projection.chat),
        "realtime_voice" => Ok(&config.result_projection.realtime_voice),
        "autonomous_task" => Ok(&config.result_projection.autonomous_task),
        other => anyhow::bail!("unsupported eval surface `{other}`"),
    }
}

fn surface_context_deadline_ms(
    surface: &str,
    config: &magician::config::AgentSurfaceRuntimeConfig,
) -> Result<u64> {
    match surface {
        "chat" => Ok(config.context_retrieval.chat.deadline_ms),
        "realtime_voice" => Ok(config.context_retrieval.realtime_voice.deadline_ms),
        "autonomous_task" => Ok(config.context_retrieval.autonomous_task.deadline_ms),
        other => anyhow::bail!("unsupported eval surface `{other}`"),
    }
}

async fn staged_value(spec: StageFixture) -> StagedValue<String> {
    if spec.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(spec.delay_ms)).await;
    }
    match spec.state.as_str() {
        "completed" => StagedValue::Completed(spec.value.unwrap_or(Value::Null).to_string()),
        "empty" => StagedValue::Empty,
        "pending" => std::future::pending::<StagedValue<String>>().await,
        "error" => StagedValue::error(
            spec.error_code
                .unwrap_or_else(|| "fixture_stage_error".to_string()),
            spec.retryable,
        ),
        other => StagedValue::error(format!("invalid_fixture_stage_{other}"), false),
    }
}

fn stage_label(state: ContextStageState) -> &'static str {
    match state {
        ContextStageState::Completed => "completed",
        ContextStageState::Empty => "empty",
        ContextStageState::TimedOut => "timed_out",
        ContextStageState::Error => "error",
        ContextStageState::Cancelled => "cancelled",
    }
}

async fn generate_staged_case(
    case: &FixtureCase,
    staged: StagedFixture,
    deadline_ms: u64,
) -> Result<GeneratedCase> {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    let surface = match case.surface.as_str() {
        "chat" => InvocationSurface::Chat,
        "realtime_voice" => InvocationSurface::RealtimeVoice,
        "autonomous_task" => InvocationSurface::Task,
        other => anyhow::bail!("unsupported eval surface `{other}`"),
    };
    let request = ContextRetrievalRequest::new(
        "projection-live-eval",
        "synthetic",
        "personal-assistant",
        surface,
        FeatureMode::None,
        format!("binding-{}", case.name),
        format!("turn-{}", case.name),
        1,
        case.question.clone(),
        "eval-authority-v1",
        BTreeMap::from([
            ("fast_memory".to_string(), "fixture-v1".to_string()),
            ("hybrid_memory".to_string(), "fixture-v1".to_string()),
            ("reusable_procedures".to_string(), "fixture-v1".to_string()),
        ]),
    );
    let fast_stage = ContextStageDescriptor::new(ContextStageKind::FastMemory, 0, "fast_memory");
    let hybrid_stage =
        ContextStageDescriptor::new(ContextStageKind::HybridMemory, 0, "hybrid_memory");
    let procedure_stage = ContextStageDescriptor::new(
        ContextStageKind::ReusableProcedures,
        0,
        "reusable_procedures",
    );
    let outcome = retrieve_bound_staged_triple(
        request,
        ContextRetrievalPolicy::until(deadline),
        tokio_util::sync::CancellationToken::new(),
        fast_stage,
        staged_value(staged.fast_memory),
        hybrid_stage,
        staged_value(staged.hybrid_memory),
        procedure_stage,
        staged_value(staged.procedures),
    )
    .await
    .context("running bound staged-context fixture")?;
    let decode = |value: Option<String>| {
        value.map(|value| serde_json::from_str(&value).unwrap_or(Value::String(value)))
    };
    let projected = json!({
        "status": if outcome.cancelled { "cancelled" } else if outcome.deadline_reached { "partial" } else { "ok" },
        "stages": {
            "fast_memory": stage_label(outcome.first_status),
            "hybrid_memory": stage_label(outcome.second_status),
            "procedures": stage_label(outcome.third_status),
        },
        "fast_memory": decode(outcome.first),
        "memory": decode(outcome.second),
        "procedure": decode(outcome.third),
    });
    Ok(GeneratedCase {
        name: case.name.clone(),
        surface: case.surface.clone(),
        projected,
        runtime: json!({
            "kind": "staged_context",
            "deadline_ms": deadline_ms,
            "deadline_reached": outcome.deadline_reached,
            "cancelled": outcome.cancelled,
            "elapsed_ms": outcome.elapsed_ms,
            "turn_generation": outcome.canonical.turn.turn_generation,
            "relevance_query_digest": outcome.canonical.turn.relevance_query_digest,
            "reuse_key_fingerprint": outcome.canonical.reuse_key.fingerprint(),
            "accepted_contribution_count": outcome.canonical.contributions.len(),
            "deduplicated_evidence_count": outcome.canonical.deduplicated_evidence_count,
            "omitted_by_budget_count": outcome.canonical.omitted_by_budget_count,
            "canonical_stage_statuses": outcome.canonical.stage_statuses,
            "stage_elapsed_ms": {
                "fast_memory": outcome.first_elapsed_ms,
                "hybrid_memory": outcome.second_elapsed_ms,
                "procedures": outcome.third_elapsed_ms,
            }
        }),
    })
}

async fn generate_projected_case(
    case: &FixtureCase,
    workspace: &ArtifactV2Workspace,
    runtime_config: &magician::config::AgentSurfaceRuntimeConfig,
) -> Result<GeneratedCase> {
    let scope = ScopeRef::system_internal_unauthenticated("projection-live-eval", "synthetic");
    let task_id = format!("task-{}", case.name);
    let execution_id = format!("execution-{}", case.name);
    let (owner, retention_class, expires_at_ms) = match case.surface.as_str() {
        "chat" => {
            let session_id = format!("chat-{}", case.name);
            workspace
                .create_dir_all_path(workspace.chat_session_dir(
                    scope.principal(),
                    scope.workspace(),
                    &session_id,
                ))
                .await
                .context("creating synthetic chat owner anchor")?;
            (
                RawResultOwner::Chat { session_id },
                ResultRetentionClass::ChatLifecycle,
                None,
            )
        },
        "realtime_voice" => (
            RawResultOwner::EphemeralVoice {
                voice_session_id: format!("voice-{}", case.name),
            },
            ResultRetentionClass::EphemeralVoice,
            Some(Utc::now().timestamp_millis() + 60 * 60 * 1_000),
        ),
        "autonomous_task" => {
            workspace
                .create_dir_all_path(workspace.execution_dir(
                    scope.principal(),
                    scope.workspace(),
                    &task_id,
                    &execution_id,
                ))
                .await
                .context("creating synthetic task execution owner anchor")?;
            (
                RawResultOwner::Task {
                    task_id: task_id.clone(),
                    execution_id: Some(execution_id.clone()),
                },
                ResultRetentionClass::TaskLifecycle,
                None,
            )
        },
        other => anyhow::bail!("unsupported eval surface `{other}`"),
    };
    let owner_kind = match &owner {
        RawResultOwner::Chat { .. } => "chat",
        RawResultOwner::Task { .. } => "task",
        RawResultOwner::EphemeralVoice { .. } => "ephemeral_voice",
    };
    let tool_call_id = format!("call-{}", case.name);
    let authority_revision = "eval-authority-v1".to_string();
    let contract_id = case
        .contract_id
        .as_deref()
        .map(ProjectionContractId::new)
        .transpose()
        .context("validating fixture projection contract")?;
    let contract_registry = ProjectionContractRegistry::default();
    let contract = contract_id
        .as_ref()
        .map(|contract_id| {
            contract_registry
                .get(contract_id)
                .cloned()
                .with_context(|| format!("unknown fixture projection contract `{contract_id}`"))
        })
        .transpose()?;
    let budget =
        projection_budget_from_config(surface_projection_budget(&case.surface, runtime_config)?);
    let projection = materialize_and_project_tool_result(
        workspace.clone(),
        MaterializeAndProjectToolResultRequest {
            scope: scope.clone(),
            owner: owner.clone(),
            agent_id: "personal-assistant".to_string(),
            tool_name: "synthetic_eval_tool".to_string(),
            tool_call_id: tool_call_id.clone(),
            trust_tool: "synthetic_eval_tool".to_string(),
            trust_action: "read".to_string(),
            execution_id: (case.surface == "autonomous_task").then(|| execution_id.clone()),
            task_id: (case.surface == "autonomous_task").then(|| task_id.clone()),
            authority_revision: authority_revision.clone(),
            outcome: ToolOutcome::with_status(case.expected_outcome_status),
            safe_value: &case.raw,
            media_type: "application/json".to_string(),
            retention_class,
            expires_at_ms,
            projection_contract: contract.as_ref(),
            projection_budget: budget.clone(),
            spoken_hint: None,
        },
    )
    .await
    .context("materializing and projecting fixture")?;

    let binding = ResultAuthorityBinding {
        agent_id: "personal-assistant".to_string(),
        owner: owner.clone(),
        tool_name: "synthetic_eval_tool".to_string(),
        tool_call_id,
        trust_tool: Some("synthetic_eval_tool".to_string()),
        trust_action: Some("read".to_string()),
    };
    let content_ref = ScopedResultRef::parse(projection.raw.content_ref.result_ref.clone())?;
    let read_context = RawResultReadContext {
        scope: scope.clone(),
        owner: owner.clone(),
        agent_id: "personal-assistant".to_string(),
    };
    let current_authority = SnapshotResultAuthority::for_current_binding(
        scope.clone(),
        binding.clone(),
        authority_revision.clone(),
    )?;
    let store = CanonicalRawResultStore::new(workspace.clone(), Arc::new(current_authority));
    let verified_page = store
        .read(
            &read_context,
            &RawResultReadRequest::first_page(content_ref.clone(), 1),
        )
        .await
        .context("verifying canonical fixture read")?;
    let mut projected =
        magician::magician_v2::tool_result_projection::provider_safe_model_value(&projection);
    let mut continuation_pages_read = 0usize;
    let mut continuation_page_projected = false;
    let mut revocation_denied = false;

    if case.continuation_probe {
        let first = store
            .read(
                &read_context,
                &RawResultReadRequest {
                    content_ref: content_ref.clone(),
                    cursor: None,
                    field_paths: vec!["/rows".to_string()],
                    max_records: 26,
                    max_serialized_bytes: 256 * 1024,
                },
            )
            .await
            .context("reading first continuation fixture page")?;
        let cursor = first
            .next_cursor
            .context("continuation fixture did not produce a cursor")?;
        let second = store
            .read(
                &read_context,
                &RawResultReadRequest {
                    content_ref: content_ref.clone(),
                    cursor: Some(cursor),
                    field_paths: vec!["/rows".to_string()],
                    max_records: 26,
                    max_serialized_bytes: 256 * 1024,
                },
            )
            .await
            .context("reading second continuation fixture page")?;
        continuation_pages_read = 2;
        let continuation_value = model_lossless_read_success_payload(second);
        let continuation_projection = materialize_and_project_tool_result(
            workspace.clone(),
            MaterializeAndProjectToolResultRequest {
                scope: scope.clone(),
                owner: owner.clone(),
                agent_id: "personal-assistant".to_string(),
                tool_name: "read_result".to_string(),
                tool_call_id: format!("call-{}-continuation", case.name),
                trust_tool: "read_result".to_string(),
                trust_action: "read".to_string(),
                execution_id: (case.surface == "autonomous_task").then(|| execution_id.clone()),
                task_id: (case.surface == "autonomous_task").then(|| task_id.clone()),
                authority_revision: authority_revision.clone(),
                outcome: ToolOutcome::succeeded(),
                safe_value: &continuation_value,
                media_type: "application/json".to_string(),
                retention_class,
                expires_at_ms,
                projection_contract: None,
                projection_budget: budget,
                spoken_hint: None,
            },
        )
        .await
        .context("projecting continuation fixture through the production projector")?;
        projected = magician::magician_v2::tool_result_projection::provider_safe_model_value(
            &continuation_projection,
        );
        continuation_page_projected = true;
    }

    if case.revocation_probe {
        let revoked_authority =
            SnapshotResultAuthority::for_current_binding(scope, binding, "eval-authority-v2")?;
        let revoked_store =
            CanonicalRawResultStore::new(workspace.clone(), Arc::new(revoked_authority));
        let result = revoked_store
            .read(
                &read_context,
                &RawResultReadRequest::first_page(content_ref, 1),
            )
            .await;
        revocation_denied = matches!(result, Err(CanonicalResultError::Revoked));
        if !revocation_denied {
            anyhow::bail!("revocation fixture did not fail closed");
        }
        projected = json!({
            "status": "revoked",
            "reason": "authority revision changed",
            "content_disclosed": false,
        });
    }

    Ok(GeneratedCase {
        name: case.name.clone(),
        surface: case.surface.clone(),
        projected,
        runtime: json!({
            "kind": "materialized_projection",
            "owner_kind": owner_kind,
            "retention_class": retention_class,
            "projection_schema_version": projection.schema_version,
            "projection_strategy": &projection.model.strategy,
            "raw_bytes": projection.metrics.raw_bytes,
            "model_bytes": projection.metrics.model_bytes,
            "estimated_model_tokens": projection.metrics.estimated_model_tokens,
            "included_records": projection.metrics.included_records,
            "omitted_records": projection.metrics.omitted_records,
            "omitted_fields": projection.metrics.omitted_fields,
            "read_hash_verified": verified_page.content_hash == projection.raw.content_hash,
            "continuation_pages_read": continuation_pages_read,
            "continuation_page_projected": continuation_page_projected,
            "revocation_denied": revocation_denied,
        }),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let input: FixtureInput = serde_json::from_slice(
        &tokio::fs::read(&args.input)
            .await
            .with_context(|| format!("reading {}", args.input.display()))?,
    )
    .context("parsing fixture input")?;
    let config = load_magician_config_from_path(&args.config)
        .with_context(|| format!("loading {}", args.config.display()))?;
    let workspace = ArtifactV2Workspace::new(&args.workspace_root);
    let mut cases = Vec::with_capacity(input.cases.len());
    for case in input.cases {
        let generated = if let Some(staged) = case.staged.clone() {
            generate_staged_case(
                &case,
                staged,
                surface_context_deadline_ms(&case.surface, &config.agent_surface_runtime)?,
            )
            .await?
        } else {
            generate_projected_case(&case, &workspace, &config.agent_surface_runtime).await?
        };
        cases.push(generated);
    }
    let output = FixtureOutput {
        schema_version: "tool_result_projection_context_live_fixture.v1",
        runtime_backed: true,
        cases,
    };
    if let Some(parent) = args.output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&args.output, serde_json::to_vec_pretty(&output)?)
        .await
        .with_context(|| format!("writing {}", args.output.display()))?;
    Ok(())
}
