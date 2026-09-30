//! The one server-owned way to mint a task-backed direct root run — shared by
//! the HTTP create path, the eval runner's production executor, and the
//! artifact service. Extracted from `api::web_api` (api-crate extraction
//! prerequisite); the api file re-imports.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::magician_v2::artifact_v2::{ArtifactV2Service, CreateTaskInput, ScopeRef};
use crate::magician_v2::orchestrator::MagicianV2Orchestrator;
use crate::magician_v2::work_context::WorkContextKind;

pub fn direct_root_task_title(title: Option<&str>, description: &str) -> String {
    let title = title.unwrap_or_default().trim();
    if !title.is_empty() {
        return title.to_string();
    }

    let trimmed = description.trim();
    if trimmed.is_empty() {
        return "Direct execution".to_string();
    }

    let mut value = trimmed.chars().take(60).collect::<String>();
    if trimmed.chars().count() > 60 {
        value.push_str("...");
    }
    value
}

pub fn scope_ref(principal: &str, workspace: &str) -> ScopeRef {
    ScopeRef::system_internal_unauthenticated(&principal.to_string(), &workspace.to_string())
}

pub fn legacy_task_created_by_to_v3(
    created_by: &crate::magician_v2::storage::TaskCreatedBy,
) -> String {
    match created_by {
        crate::magician_v2::storage::TaskCreatedBy::User => "user".to_string(),
        crate::magician_v2::storage::TaskCreatedBy::Agent { .. } => "agent".to_string(),
        crate::magician_v2::storage::TaskCreatedBy::Delegation { .. } => "delegation".to_string(),
    }
}

pub async fn create_v3_task_execution_shell(
    artifact_v2_service: &ArtifactV2Service,
    orchestrator: &Arc<MagicianV2Orchestrator>,
    input: CreateTaskInput,
) -> Result<(String, String, String), String> {
    create_v3_task_execution_shell_under_work(artifact_v2_service, orchestrator, input, None).await
}

/// Boxed variant for callers that are already deep in an async chain.
///
/// An `async fn` stores its callee's state machine inline, so on a long
/// dispatch chain the frames multiply. Minting a task shell is a
/// once-per-dispatch step, so the allocation is free next to what it saves.
pub fn create_v3_task_execution_shell_boxed<'a>(
    artifact_v2_service: &'a ArtifactV2Service,
    orchestrator: &'a Arc<MagicianV2Orchestrator>,
    input: CreateTaskInput,
) -> Pin<Box<dyn Future<Output = Result<(String, String, String), String>> + Send + 'a>> {
    Box::pin(create_v3_task_execution_shell(
        artifact_v2_service,
        orchestrator,
        input,
    ))
}

/// The same shell, for a run that belongs to a named piece of **work**.
///
/// `work` is the generic [`WorkContextKind`] — an engagement, a program, or
/// whatever kind is added next — and it is the *name* of the work, never a
/// grant. The orchestrator validates it against the live roster and reads the
/// authority revision from there, so naming a work context is not the same as
/// being granted it (§4.2c row 5, applied to a root rather than a child).
///
/// A work context that cannot be validated **fails the whole creation**. That
/// is deliberate and it is the fail-closed half: a caller that asked for a
/// confined run and silently received an unconfined one has no way to notice.
pub async fn create_v3_task_execution_shell_under_work(
    artifact_v2_service: &ArtifactV2Service,
    orchestrator: &Arc<MagicianV2Orchestrator>,
    input: CreateTaskInput,
    work: Option<&WorkContextKind>,
) -> Result<(String, String, String), String> {
    let scope = scope_ref(&input.principal, &input.workspace);
    // Refuse before Artifact creates a shell: accepting the task and failing
    // only when its detached runtime starts would expose a durable no-op. The
    // immutable scope cutover is the only absence proof while old stateless
    // binaries may still omit reverse bindings.
    orchestrator
        .require_stateless_scope_activated(&scope.principal(), &scope.workspace())
        .await?;
    let active_owner_agent_id = input.agent_id.clone();
    let title = input.title.clone();
    let (task, execution) = artifact_v2_service
        .create_task_with_execution_shell(input)
        .await
        .map_err(|error| format!("failed to create V3 task shell: {}", error))?;
    let runtime_execution = orchestrator
        .create_root_execution_under_work(
            &scope.principal(),
            &scope.workspace(),
            Some(title),
            &active_owner_agent_id,
            &execution.state.execution_id,
            Some(task.manifest.task_id.clone()),
            Some(execution.state.execution_id.clone()),
            work,
        )
        .await
        .map_err(|error| format!("failed to create V3 runtime execution shell: {}", error))?;
    Ok((
        task.manifest.task_id,
        execution.state.execution_id,
        runtime_execution.id,
    ))
}

/// `pub` so the eval runner's production executor
/// (`magician_v2::evals::executor`) creates its executions through the exact
/// same path `create_execution` uses, rather than growing a second, subtly
/// different way to mint a task-backed direct run.
pub async fn create_task_backed_direct_root_run(
    artifact_v2_service: &ArtifactV2Service,
    orchestrator: &Arc<MagicianV2Orchestrator>,
    principal: &str,
    workspace: &str,
    ui_thread_id: &str,
    title: Option<&str>,
    description: &str,
    active_owner_agent_id: &str,
    created_by: crate::magician_v2::storage::TaskCreatedBy,
    debug_run: bool,
    internal_run: bool,
) -> Result<(String, String, String), String> {
    create_task_backed_direct_root_run_under_work(
        artifact_v2_service,
        orchestrator,
        principal,
        workspace,
        ui_thread_id,
        title,
        description,
        active_owner_agent_id,
        created_by,
        debug_run,
        internal_run,
        None,
    )
    .await
}

/// The same direct root run, bound to the **work that asked for it**.
///
/// This is the one production path by which a root execution comes into
/// existence carrying authority. Everything downstream already worked: the
/// durable `ExecutionRun` carries the generic
/// [`crate::magician_v2::work_context::WorkAuthorityRef`], the orchestrator
/// narrows it to the engagement reference the dispatch boundary enforces, and
/// from there the outward capability ceiling, the envelope scope, retrieval
/// containment and the maturity sweep all light up. They were inert only
/// because nothing ever wrote the carrier at the root.
#[allow(clippy::too_many_arguments)]
pub async fn create_task_backed_direct_root_run_under_work(
    artifact_v2_service: &ArtifactV2Service,
    orchestrator: &Arc<MagicianV2Orchestrator>,
    principal: &str,
    workspace: &str,
    ui_thread_id: &str,
    title: Option<&str>,
    description: &str,
    active_owner_agent_id: &str,
    created_by: crate::magician_v2::storage::TaskCreatedBy,
    debug_run: bool,
    internal_run: bool,
    work: Option<&WorkContextKind>,
) -> Result<(String, String, String), String> {
    // Debug-page runs (SOTA tests, skill action probes, agentic
    // experiments) carry `debug_run: true`. They get the `Internal`
    // lifecycle, routing into the `internal_tasks/` storage root, so
    // they:
    //   * stay out of `/tasks` (which only reads `tasks/`)
    //   * appear in `/internal-tasks` (which reads `internal_tasks/`)
    //   * are NEVER auto-swept on chat-session cleanup — they carry
    //     `chat_session_id: None` (the sole cleanup discriminator), so
    //     the operator removes them manually via `/internal-tasks`.
    // The `created_by: __system__` marker is kept as a belt-and-braces
    // filter for any legacy code path that lists from `tasks/` and
    // expects to filter system entries, and lets the UI flag
    // debug-origin rows; the storage-root split (via `Internal`) is the
    // primary mechanism.
    let resolved_created_by = if debug_run {
        "__system__".to_string()
    } else {
        legacy_task_created_by_to_v3(&created_by)
    };
    // Audience and provenance are independent: debug changes the creator marker,
    // while either debug or an explicit internal request routes execution
    // machinery away from the user's durable `/tasks` commitments.
    let resolved_lifecycle = if debug_run || internal_run {
        crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal
    } else {
        crate::magician_v2::artifact_v2::models::TaskLifecycle::default()
    };
    let input = CreateTaskInput {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        ui_thread_id: ui_thread_id.to_string(),
        title: direct_root_task_title(title, description),
        description: description.to_string(),
        agent_id: active_owner_agent_id.to_string(),
        goal_id: None,
        priority: None,
        due_date: None,
        tags: Vec::new(),
        created_by: resolved_created_by,
        depends_on: Vec::new(),
        approved: true,
        schedule: None,
        output_mode: crate::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
        chat_session_id: None,
        lifecycle: resolved_lifecycle,
        sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
    };
    let (task_id, execution_id, runtime_execution_id) =
        create_v3_task_execution_shell_under_work(artifact_v2_service, orchestrator, input, work)
            .await?;
    artifact_v2_service
        .activate_execution(&scope_ref(principal, workspace), &task_id, &execution_id)
        .await
        .map_err(|error| format!("failed to activate V3 root execution: {}", error))?;
    Ok((task_id, execution_id, runtime_execution_id))
}
