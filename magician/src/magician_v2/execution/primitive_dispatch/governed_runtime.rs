//! Product adapter from migrated universal-runtime packages to the Phase 6
//! authorization-before-auth coordinator.
//!
//! The compatibility `Primitive` catalog remains model-facing, but a retained
//! [`SkillRuntimePackage`] is the unambiguous execution-owner marker. This
//! module performs no routing or LLM work and never falls back to the legacy
//! CLI-template dispatcher after governed dispatch has been selected.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Instant,
};

use serde_json::Value;
use tracing::instrument;

use crate::magician_v2::analytics::runtime_activity_layer::KIND_PROCESS;
use tool_runtime_core::{
    action_overrides::{
        lower_typed_action_invocation, CompiledActionCatalog, EffectiveActionPolicy,
        LoweredTypedActionInvocation, TypedActionParameter, TypedActionRoute, WorkspacePathAccess,
    },
    credential_filesystem::CredentialScratchAuthority,
    credential_injection::{
        ChildEnvironmentBaseline, ChildEnvironmentVariable, CredentialCallId,
        CredentialInjectionPlan,
    },
    credential_lifecycle::{CredentialLifecycleOperation, CredentialLifecyclePlan},
    credential_lifecycle_coordinator::CredentialLifecyclePendingKind,
    credential_lifecycle_execution::CredentialLifecycleInvocation,
    credential_lifecycle_execution::{
        CredentialLifecycleExecutionBindingError, CredentialLifecycleInteractionAction,
        CredentialLifecycleInteractionBridge, CredentialLifecycleSensitiveOutput,
    },
    credential_lifecycle_observation::CredentialLifecycleTermination,
    credential_materialization::ChildEnvironmentValues,
    credential_preparation::{
        CredentialMaterialBindingName, CredentialMaterialKind, CredentialMaterialResolver,
        CredentialMaterialSink, CredentialPreparationBinding, CredentialPreparationError,
        CredentialPreparationPlan, MAX_PREPARED_CREDENTIAL_BINDINGS,
        MAX_TOTAL_PREPARED_CREDENTIAL_BYTES,
    },
    credential_profile_store::LocalCredentialProfileRegistry,
    credential_profiles::{
        CreateCredentialProfileReference, CredentialProfileBinding, CredentialProfileError,
        CredentialProfileKey, CredentialProfileRegistry, CredentialProfileRegistrySnapshot,
        CredentialProfileStatus, CredentialScope, SetCredentialProfileDisabled,
        UpdateCredentialProfileMetadata,
    },
    governed_batch_process::GovernedBatchCancellation,
    governed_execution::{
        GovernedExecutionPolicy, GovernedExecutionRequest, GovernedExecutionTerminal,
    },
    governed_execution_authority::GovernedWorkingDirectoryRoot,
    governed_execution_coordinator::{
        GovernedAuthorizationDecision, GovernedAuthorizationEvidence, GovernedAuthorizationRequest,
        GovernedCredentialFilesystemRequest, GovernedExecutionAuditError,
        GovernedExecutionAuditReceipt, GovernedExecutionAuditSink, GovernedExecutionAuthorizer,
        GovernedExecutionCallContext, GovernedExecutionInvocation,
    },
    manifest::{
        declared_login_prompt, ApprovalClass, AuthKind, AuthRequirement, AuthStorage,
        CliInteraction, LifecyclePrompt, LifecyclePromptKind, PolicyFloor, ProfileSelection,
        RuntimeProtocol, WorkingDirectoryMode,
    },
    manifest_parser::SkillRuntimePackage,
    manifest_validation::{
        validate_skill_runtime_contract, MAX_RUNTIME_STREAM_BYTES, MAX_RUNTIME_TIMEOUT_SECS,
    },
    profile_selection::{select_credential_profile, CredentialProfileSelectionRequest},
    scoped_paths::{ScopedPath, ScopedPathAuthority, ScopedPathComponent},
};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    capability_invoker::BrokeredWorkspacePath, exec_ctx::PrimitiveExecCtx,
    runner::PrimitiveToolResult,
};
use crate::magician_v2::{
    artifact_v2::capabilities::CapabilityScopePaths,
    execution::capability::GovernedRuntimeImplementation,
    secrets::{
        credential_lifecycle_executor::{
            CredentialLifecycleProcessCancellation, GovernedCredentialLifecycleExecutor,
        },
        credential_material_adapter::{CredentialGrantRoute, ScopedCredentialMaterialAdapter},
        operator_profile_adapter::OperatorProfileAdapter,
        SecretAuditEvent, SecretStore,
    },
};

const MAX_OPERATOR_CONFIG_BYTES: u64 = 1024 * 1024;
const GOVERNED_POLICY_REVISION: &str = "magician-governed-v1";
const MAX_RUNTIME_SECRET_BINDING_BYTES: usize =
    MAX_TOTAL_PREPARED_CREDENTIAL_BYTES / MAX_PREPARED_CREDENTIAL_BINDINGS;

/// Crate-private proof that the capability invocation crossed the existing
/// product trust/resource gates before any auth lifecycle or executable work.
/// Construction is intentionally limited to the deterministic invoker.
pub(crate) struct GovernedRouteAdmission(());

pub(crate) fn admit_governed_route() -> GovernedRouteAdmission {
    GovernedRouteAdmission(())
}

/// One span per governed execution — the whole credential-prepare, spawn,
/// run and audit sequence, including the MCP hand-off.
///
/// `skip_all` is a hard requirement rather than a convenience: `arguments` is
/// model-authored tool input and `exec_ctx` reaches a secret store. Only the
/// capability and action identifiers are named.
#[instrument(
    name = "governed_runtime_dispatch",
    skip_all,
    fields(
        activity_kind = KIND_PROCESS,
        capability = %capability_id,
        action = %action_id,
        principal = exec_ctx.principal.as_deref(),
        workspace = exec_ctx.workspace.as_deref(),
    )
)]
pub(super) async fn dispatch_governed_runtime(
    admission: GovernedRouteAdmission,
    runtime: Arc<GovernedRuntimeImplementation>,
    capability_id: String,
    action_id: String,
    arguments: Value,
    exec_ctx: PrimitiveExecCtx,
    scope_paths: CapabilityScopePaths,
    brokered_workspace_paths: BTreeMap<String, BrokeredWorkspacePath>,
) -> Result<PrimitiveToolResult, String> {
    if matches!(
        &runtime.package.contract.runtime,
        RuntimeProtocol::Mcp { .. }
    ) && !brokered_workspace_paths.is_empty()
    {
        return Err(
            "broker-owned workspace paths are valid only for governed CLI actions".to_owned(),
        );
    }
    if matches!(
        &runtime.package.contract.runtime,
        RuntimeProtocol::Mcp { .. }
    ) {
        return super::governed_mcp::dispatch_governed_mcp(
            admission,
            runtime,
            capability_id,
            action_id,
            arguments,
            exec_ctx,
        )
        .await;
    }
    let cancellation = GovernedBatchCancellation::new();
    let lifecycle_cancellation = CredentialLifecycleProcessCancellation::new();
    let watcher = exec_ctx.cancellation_token.clone().map(|token| {
        let cancellation = cancellation.clone();
        let lifecycle_cancellation = lifecycle_cancellation.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            cancellation.cancel();
            lifecycle_cancellation.cancel();
        })
    });
    let result = tokio::task::spawn_blocking(move || {
        dispatch_governed_runtime_blocking(
            runtime.as_ref(),
            admission,
            &capability_id,
            &action_id,
            arguments,
            &exec_ctx,
            &scope_paths,
            &brokered_workspace_paths,
            &cancellation,
            &lifecycle_cancellation,
        )
    })
    .await;
    if let Some(watcher) = watcher {
        watcher.abort();
    }
    result.map_err(|_| "governed runtime worker failed".to_owned())?
}

#[allow(clippy::too_many_arguments)]
/// The credential call id for one governed invocation.
///
/// Derived from the dispatch attempt whenever there is one, so a crash and
/// re-run of the SAME attempt reuses the id and the credential audit shows one
/// use of the credential rather than two. `Uuid::new_v4` cannot do that: it is
/// fresh on every call, so a replayed attempt reads as a second, unrelated use —
/// which is exactly the question an operator is trying to answer when they go
/// looking.
///
/// Derived rather than carried verbatim for two independent reasons.
/// `CredentialCallId` admits only `[A-Za-z0-9_-.+]` and an effect id contains
/// colons, so the raw value cannot be one. And this id reaches child process
/// environments, where an internal identifier does not belong. The digest is
/// one-way but deterministic, so an operator holding an effect id can compute
/// the call id and find the audit line — which is the join Phase 5 is about, and
/// it needs no new field on the deliberately narrow
/// `RuntimeCredentialAuditReceipt`.
///
/// Falls back to a random id when the dispatch is unattributable. That is not
/// stable across a replay, but it is distinct, which is the property the audit
/// needs to avoid collapsing two real invocations into one line.
/// Replace every value this run has delivered with `[REDACTED]`.
///
/// A governed program is a credential sink (its declared login prompts), and a
/// PTY with `ECHO` on writes the answer straight back. The run's own delivered
/// set is the only thing that knows those values — MagicRun's redactor is built
/// from the profile material it injected, which never includes a HITL answer.
fn scrub_with_delivered(exec_ctx: &PrimitiveExecCtx, text: String) -> String {
    let Some(delivered) = exec_ctx.delivered_secret_values.as_ref() else {
        return text;
    };
    let Ok(delivered) = delivered.lock() else {
        return text;
    };
    crate::magician_v2::secrets::known_value_replacements(&delivered)
        .iter()
        .fold(text, |acc, value| acc.replace(value, "[REDACTED]"))
}

fn governed_call_id(prefix: &str, exec_ctx: &PrimitiveExecCtx) -> String {
    match exec_ctx.effect_id.as_deref() {
        Some(effect_id) => format!(
            "{prefix}-{}",
            crate::magician_v2::analytics::llm_tool_lineage::LlmToolLineageIdentity::
                local_attempt_key(effect_id)
        ),
        None => format!("{prefix}-{}", Uuid::new_v4().simple()),
    }
}

fn bind_subprocess_storage_envelope(
    exec_ctx: &PrimitiveExecCtx,
    call_id: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    crate::magician_v2::subprocess_owners::install_skill_working_envelope_from_exec(
        &exec_ctx.storage_base_path,
        exec_ctx.principal.as_deref(),
        exec_ctx.workspace.as_deref(),
        call_id,
    )
    .map_err(|err| format!("subprocess storage envelope failed: {err}"))
}

fn dispatch_governed_runtime_blocking(
    runtime: &GovernedRuntimeImplementation,
    admission: GovernedRouteAdmission,
    capability_id: &str,
    action_id: &str,
    arguments: Value,
    exec_ctx: &PrimitiveExecCtx,
    scope_paths: &CapabilityScopePaths,
    brokered_workspace_paths: &BTreeMap<String, BrokeredWorkspacePath>,
    cancellation: &GovernedBatchCancellation,
    lifecycle_cancellation: &CredentialLifecycleProcessCancellation,
) -> Result<PrimitiveToolResult, String> {
    let started = Instant::now();
    let audit_store = exec_ctx
        .secret_store
        .as_deref()
        .ok_or_else(|| "governed runtime audit authority is unavailable".to_owned())?;
    let package = &runtime.package;
    let actions = runtime.cli_actions().map_err(str::to_owned)?;
    let action = actions
        .actions
        .get(action_id)
        .ok_or_else(|| "governed runtime action is unavailable".to_owned())?;
    let mut normalized = normalize_runtime_controls(package, arguments)?;
    let base_validated = validate_skill_runtime_contract(&package.contract)
        .map_err(|_| "governed runtime package validation failed".to_owned())?;

    let mut effective_contract = package.contract.clone();
    effective_contract.requires.bins =
        std::collections::BTreeSet::from([action.invocation.executable.clone()]);
    effective_contract.requires.entrypoint = Some(action.invocation.executable.clone());
    apply_effective_policy(
        &mut effective_contract.policy_floor,
        &action.effective_policy,
    );
    let validated = validate_skill_runtime_contract(&effective_contract)
        .map_err(|_| "governed runtime effective policy validation failed".to_owned())?;

    let principal = exec_ctx
        .principal
        .as_deref()
        .unwrap_or(crate::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_PRINCIPAL);
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .unwrap_or(crate::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_WORKSPACE);
    let scope = CredentialScope::new(principal, workspace)
        .map_err(|_| "governed runtime scope is invalid".to_owned())?;
    let authority = ScopedPathAuthority::open(exec_ctx.storage_base_path.join("scopes"))
        .map_err(|error| format!("governed runtime scope authority is unavailable: {error}"))?;
    let scope_root = authority
        .resolve_scope_root(&scope)
        .map_err(|_| "governed runtime scope root is unavailable".to_owned())?;
    bind_workspace_path_parameters(
        action,
        &mut normalized,
        &scope_root,
        brokered_workspace_paths,
    )?;
    let lowered = lower_typed_action_invocation(action, &normalized)
        .map_err(|error| format!("governed runtime input rejected: {error}"))?;

    if matches!(
        effective_contract.auth.kind,
        AuthKind::None | AuthKind::Secrets | AuthKind::NativePermission
    ) {
        return dispatch_profile_free_cli_runtime(
            runtime,
            admission,
            capability_id,
            action_id,
            lowered,
            validated,
            scope,
            scope_root,
            audit_store,
            exec_ctx,
            scope_paths,
            cancellation,
            started,
        );
    }
    if effective_contract.auth.kind == AuthKind::CliProfile
        && matches!(
            effective_contract.auth.profile_selection,
            ProfileSelection::Implicit
        )
        && matches!(effective_contract.auth.storage, AuthStorage::CliOwned)
    {
        return dispatch_implicit_cli_runtime(
            runtime,
            admission,
            capability_id,
            action_id,
            lowered,
            validated,
            scope,
            scope_root,
            audit_store,
            exec_ctx,
            scope_paths,
            cancellation,
            started,
        );
    }
    if effective_contract.auth.kind != AuthKind::CliProfile {
        return Err("governed runtime auth kind is not migrated yet".to_owned());
    }

    let auth_root = authority
        .resolve_auth_root(&scope)
        .map_err(|_| "governed runtime auth root is unavailable".to_owned())?;

    let config_source = operator_config_source(exec_ctx)?;
    let legacy = OperatorProfileAdapter::from_contract(&package.contract)
        .map_err(|_| "governed runtime profile adapter is invalid".to_owned())?
        .normalize(&config_source, scope.clone(), &authority)
        .map_err(|_| "governed runtime profile configuration is invalid".to_owned())?;
    let registry = LocalCredentialProfileRegistry::open(scope.clone(), auth_root, legacy)
        .map_err(|_| "governed runtime profile registry is unavailable".to_owned())?;
    let selection_request = CredentialProfileSelectionRequest::new(
        scope.clone(),
        package.contract.auth.provider.as_deref(),
        CredentialProfileBinding::Provider,
        &package.contract.auth.profile_selection,
        lowered.profile.as_deref(),
    )
    .map_err(|_| "governed runtime profile selection is invalid".to_owned())?;
    let initial_selection = select_credential_profile(&registry, &selection_request)
        .map_err(|_| "governed runtime profile selection failed".to_owned())?;
    let initial_status = initial_selection
        .selected_profile()
        .ok_or_else(|| "governed runtime requires a selected profile".to_owned())?;
    let profile_directory = profile_directory_for_contract(
        &package.contract.auth.storage,
        initial_status.key().alias.as_str(),
    )?;
    let profile_root = authority
        .resolve_profile_root(initial_status.key(), profile_directory)
        .map_err(|_| "governed runtime profile path is unavailable".to_owned())?;
    let executable_directories = governed_executable_directories(exec_ctx, runtime, scope_paths)?;
    let baseline = ChildEnvironmentBaseline::portable_cli();
    let status_baseline_values = baseline_values(
        &baseline,
        &executable_directories,
        false,
        &actions.execution.environment,
    )?;

    let status_plan = CredentialLifecyclePlan::for_profile(
        &registry,
        base_validated,
        initial_status.key(),
        CredentialLifecycleOperation::Status,
    )
    .map_err(|_| "governed runtime status plan is unavailable".to_owned())?;
    let status_invocation = CredentialLifecycleInvocation::bind(
        base_validated,
        status_plan.clone(),
        profile_root.clone(),
        &baseline,
        status_baseline_values,
    )
    .map_err(|_| "governed runtime status binding failed".to_owned())?;
    let (status_process, observed) = GovernedCredentialLifecycleExecutor::execute_status_for_plan(
        &status_plan,
        &status_invocation,
        lifecycle_cancellation,
    )
    .map_err(|_| "governed runtime profile status check failed".to_owned())?;
    record_lifecycle_audit(
        audit_store,
        capability_id,
        action_id,
        "governed_profile_status",
        serde_json::json!({
            "state": observed.state(),
            "identity_verification": observed.identity_verification(),
        }),
    )?;

    if action.invocation.route == TypedActionRoute::AuthStatus {
        let stdout = String::from_utf8_lossy(status_process.stdout()).into_owned();
        return Ok(PrimitiveToolResult {
            success: observed.is_execution_ready(),
            parsed_json: serde_json::from_slice(status_process.stdout()).ok(),
            stdout,
            stderr: String::from_utf8_lossy(status_process.stderr()).into_owned(),
            elapsed_ms: elapsed_ms(started),
            ..PrimitiveToolResult::default()
        });
    }
    if action.invocation.route == TypedActionRoute::AuthLogin {
        let login_plan = CredentialLifecyclePlan::for_profile(
            &registry,
            base_validated,
            initial_status.key(),
            CredentialLifecycleOperation::Login,
        )
        .map_err(|_| "governed runtime login plan is unavailable".to_owned())?;
        let login_invocation = CredentialLifecycleInvocation::bind(
            base_validated,
            login_plan,
            profile_root,
            &baseline,
            baseline_values(
                &baseline,
                &executable_directories,
                false,
                &runtime
                    .cli_actions()
                    .map_err(str::to_owned)?
                    .execution
                    .environment,
            )?,
        )
        .map_err(|_| "governed runtime login binding failed".to_owned())?;
        // P4 (secure-HITL CLI lane): the login hook's PTY prompts the skill
        // declared are answered from material the run already holds — a
        // password or a one-time code the user gave through HITL — and never
        // from the model; a prompt nothing can answer settles the login as a
        // typed `authentication_required` challenge the run resolves by asking
        // the user securely and trying again.
        let mut bridge = DeclaredPromptBridge::new(
            &effective_contract.auth.lifecycle.login_prompts,
            action.invocation.executable.as_str(),
            exec_ctx,
        );
        // A bridge is passed only when the contract DECLARED prompts: to the
        // executor a bridge means "this hook will be asked for material", and it
        // refuses a hook that wants prompts without a PTY (the default
        // interaction is batch, whose stdin is null — the hook would block on
        // its own prompt until the timeout and report a flat failure).
        let declares_prompts = !effective_contract.auth.lifecycle.login_prompts.is_empty();
        let bridge_ref: Option<&mut dyn CredentialLifecycleInteractionBridge> = if declares_prompts
        {
            Some(&mut bridge)
        } else {
            None
        };
        let execution = GovernedCredentialLifecycleExecutor::execute(
            &login_invocation,
            lifecycle_cancellation,
            bridge_ref,
        );
        if let Some(challenge) = bridge.challenge() {
            record_lifecycle_audit(
                audit_store,
                capability_id,
                action_id,
                "governed_profile_login",
                serde_json::json!({"success": false, "authentication_required": challenge.kind_name()}),
            )?;
            // The typed outcome is recorded on the run here, from the bridge's
            // own observation of the declared prompt — the result's text is
            // never parsed back into a challenge.
            if let (Some(pending), Some(observed)) = (
                exec_ctx.pending_challenge.as_ref(),
                challenge.as_authentication_challenge(),
            ) {
                if let Ok(mut pending) = pending.lock() {
                    *pending = Some(observed);
                }
            }
            return Ok(challenge.into_result(elapsed_ms(started)));
        }
        let result = match execution {
            Ok(result) => result,
            Err(_) => {
                // A one-time code is spent when it is handed to the PTY, so an
                // execution that fails AFTER that has burned it. Reporting a
                // flat "login failed" left the model to retry with a code the
                // store will refuse and the operator with no idea why; the
                // typed outcome says to ask for a fresh one. (A password is
                // still held for the run, so this only names a code.)
                let spent_a_code = bridge.answered_kinds().iter().any(|kind| *kind == "otp");
                if !spent_a_code {
                    return Err("governed runtime login failed".to_owned());
                }
                let challenge = LifecycleAuthenticationChallenge {
                    kind: LifecyclePromptKind::Otp,
                    program: action.invocation.executable.as_str().to_string(),
                    marker: String::new(),
                    reason: Some(
                        "the code was delivered to an attempt that then failed, so it is spent: ask for a fresh one"
                            .to_string(),
                    ),
                };
                record_lifecycle_audit(
                    audit_store,
                    capability_id,
                    action_id,
                    "governed_profile_login",
                    serde_json::json!({"success": false, "authentication_required": challenge.kind_name()}),
                )?;
                if let (Some(pending), Some(observed)) = (
                    exec_ctx.pending_challenge.as_ref(),
                    challenge.as_authentication_challenge(),
                ) {
                    if let Ok(mut pending) = pending.lock() {
                        *pending = Some(observed);
                    }
                }
                return Ok(challenge.into_result(elapsed_ms(started)));
            },
        };
        let success = matches!(
            result.termination(),
            CredentialLifecycleTermination::Exited { code: 0 }
        );
        record_lifecycle_audit(
            audit_store,
            capability_id,
            action_id,
            "governed_profile_login",
            serde_json::json!({"success": success, "answered_prompts": bridge.answered_kinds()}),
        )?;
        // What the program wrote is scrubbed with the run's delivered values
        // BEFORE it becomes a result. A login hook prompting on a PTY with
        // `ECHO` on writes the answer back through the master, and MagicRun's
        // own redactor only knows the profile material it injected — never the
        // HITL answer. Unscrubbed, that text became the tool result, the
        // dispatch error, the `magician.log` line and a durable
        // learning-evidence file.
        let scrub = |text: String| scrub_with_delivered(exec_ctx, text);
        return Ok(PrimitiveToolResult {
            success,
            parsed_json: serde_json::from_slice(result.stdout()).ok(),
            stdout: scrub(String::from_utf8_lossy(result.stdout()).into_owned()),
            stderr: scrub(String::from_utf8_lossy(result.stderr()).into_owned()),
            elapsed_ms: elapsed_ms(started),
            ..PrimitiveToolResult::default()
        });
    }
    if !observed.is_execution_ready() {
        return Err(
            "governed runtime profile is not authenticated as the expected identity".to_owned(),
        );
    }

    let ready_status =
        CredentialProfileStatus::new(initial_status.metadata().clone(), observed.state())
            .map_err(|_| "governed runtime verified profile status is invalid".to_owned())?;
    let verified_registry = VerifiedProfileRegistry {
        base: &registry,
        ready: ready_status.clone(),
    };
    let ready_selection = select_credential_profile(&verified_registry, &selection_request)
        .map_err(|_| "governed runtime verified profile selection failed".to_owned())?;
    let preparation = CredentialPreparationPlan::new(
        scope,
        effective_contract.auth.kind,
        &ready_selection,
        Vec::new(),
    )
    .map_err(|_| "governed runtime credential preparation failed".to_owned())?;
    let injection = CredentialInjectionPlan::compile(
        validated,
        &preparation,
        ChildEnvironmentBaseline::portable_cli(),
    )
    .map_err(|_| "governed runtime credential injection failed".to_owned())?;
    let values = baseline_values(
        injection.baseline(),
        &executable_directories,
        false,
        &runtime
            .cli_actions()
            .map_err(str::to_owned)?
            .execution
            .environment,
    )?;
    let execution_policy = GovernedExecutionPolicy::new(
        lowered.timeout_secs,
        lowered.timeout_secs.min(MAX_RUNTIME_TIMEOUT_SECS),
        MAX_RUNTIME_STREAM_BYTES,
        MAX_RUNTIME_STREAM_BYTES,
        MAX_RUNTIME_STREAM_BYTES,
    )
    .map_err(|_| "governed runtime execution policy is invalid".to_owned())?;
    let execution_contract =
        tool_runtime_core::governed_execution::GovernedExecutionContract::compile(
            validated,
            execution_policy,
        )
        .map_err(|_| "governed runtime execution contract failed".to_owned())?;
    let admitted_cwd = lowered
        .working_directory
        .as_ref()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let intent = execution_contract
        .admit(GovernedExecutionRequest::new(
            lowered.arguments,
            lowered.stdin.map(String::into_bytes),
            lowered.working_directory,
            Some(lowered.timeout_secs),
        ))
        .map_err(|_| "governed runtime invocation was rejected".to_owned())?;
    let envelope_id = governed_call_id("tool", exec_ctx);
    let _lease = bind_subprocess_storage_envelope(exec_ctx, &envelope_id)?;
    let cwd = admitted_cwd.unwrap_or(
        scope_root
            .revalidated_path()
            .map_err(|_| "governed runtime working directory changed".to_owned())?
            .to_path_buf(),
    );
    let working_directory =
        GovernedWorkingDirectoryRoot::open(WorkingDirectoryMode::Workspace, cwd)
            .map_err(|_| "governed runtime working directory is unavailable".to_owned())?;
    let scratch = CredentialScratchAuthority::open_or_create(&scope_root)
        .map_err(|_| "governed runtime scratch authority is unavailable".to_owned())?;
    let filesystem =
        GovernedCredentialFilesystemRequest::new(&scratch, Some((&profile_root, &ready_status)));
    let call_id = CredentialCallId::new(envelope_id)
        .map_err(|_| "governed runtime call identity is invalid".to_owned())?;
    let context = GovernedExecutionCallContext::new(call_id, capability_id, action_id)
        .map_err(|_| "governed runtime call context is invalid".to_owned())?;
    let invocation = GovernedExecutionInvocation::new(
        context,
        validated,
        intent,
        &preparation,
        &injection,
        values,
        Some(working_directory),
        None,
        Some(filesystem),
    )
    .map_err(|_| "governed runtime coordinator binding failed".to_owned())?;
    let mut authorizer = AdmittedAuthorizer {
        _admission: admission,
    };
    let mut resolver = EmptyResolver;
    let mut audit = ProductAuditSink { store: audit_store };
    let settlement = invocation
        .execute_batch(&mut authorizer, &mut resolver, &mut audit, cancellation)
        .map_err(|error| {
            format!(
                "governed runtime execution failed before settlement ({:?}, {:?})",
                error.code,
                error.dispatch()
            )
        })?;
    let result = settlement.result();
    Ok(governed_batch_result(
        exec_ctx,
        result.terminal().terminal(),
        result.exit_code(),
        result.stdout(),
        result.stderr(),
        elapsed_ms(started),
    ))
}

/// Execute a native CLI whose installed program owns its active login/session.
/// There is deliberately no synthetic profile registry or status command: the
/// contract's implicit identity and CLI-owned storage are the authority, while
/// the clean child environment exposes only `HOME` in addition to the portable
/// baseline so the CLI can find its own existing configuration.
#[allow(clippy::too_many_arguments)]
fn dispatch_implicit_cli_runtime(
    runtime: &GovernedRuntimeImplementation,
    admission: GovernedRouteAdmission,
    capability_id: &str,
    action_id: &str,
    mut lowered: LoweredTypedActionInvocation,
    validated: tool_runtime_core::manifest_validation::ValidatedSkillRuntimeContract<'_>,
    scope: CredentialScope,
    scope_root: ScopedPath,
    audit_store: &SecretStore,
    exec_ctx: &PrimitiveExecCtx,
    scope_paths: &CapabilityScopePaths,
    cancellation: &GovernedBatchCancellation,
    started: Instant,
) -> Result<PrimitiveToolResult, String> {
    if matches!(
        &validated.contract().runtime,
        RuntimeProtocol::Cli {
            interaction: CliInteraction::Pty,
            ..
        }
    ) {
        return Err(
            "coding-agent CLI packs have been retired; agents use run_coding_task and operators use the PTY workbench"
                .to_owned(),
        );
    }
    if lowered.profile.is_some() || action_id == "auth_login" || action_id == "auth_status" {
        return Err("governed implicit CLI runtime cannot select or manage a profile".to_owned());
    }
    let selection_request = CredentialProfileSelectionRequest::new(
        scope.clone(),
        validated.contract().auth.provider.as_deref(),
        CredentialProfileBinding::Provider,
        &validated.contract().auth.profile_selection,
        None,
    )
    .map_err(|_| "governed implicit CLI selection is invalid".to_owned())?;
    let selection = select_credential_profile(&NoProfileRegistry, &selection_request)
        .map_err(|_| "governed implicit CLI selection failed".to_owned())?;
    let bindings = validated
        .contract()
        .auth
        .secret_bindings
        .iter()
        .map(|binding| {
            CredentialPreparationBinding::optional(
                CredentialMaterialBindingName::new(binding.name.clone())?,
                CredentialMaterialKind::SecretBinding,
                MAX_RUNTIME_SECRET_BINDING_BYTES,
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "governed implicit CLI secret binding is invalid".to_owned())?;
    let preparation = CredentialPreparationPlan::new_with_minimum_present(
        scope,
        AuthKind::CliProfile,
        &selection,
        bindings,
        0,
    )
    .map_err(|_| "governed implicit CLI preparation failed".to_owned())?;
    let baseline = ChildEnvironmentBaseline::cli_owned_session();
    let injection = CredentialInjectionPlan::compile(validated, &preparation, baseline)
        .map_err(|_| "governed implicit CLI injection failed".to_owned())?;
    let executable_directories = governed_executable_directories(exec_ctx, runtime, scope_paths)?;
    let values = baseline_values(
        injection.baseline(),
        &executable_directories,
        false,
        &runtime
            .cli_actions()
            .map_err(str::to_owned)?
            .execution
            .environment,
    )?;
    let (working_directory, normalized_working_directory, _actual_working_directory) =
        implicit_cli_working_directory(validated, lowered.working_directory.take(), exec_ctx)?;
    lowered.working_directory = normalized_working_directory;
    if !lowered.runtime_controls.is_empty() {
        return Err("governed batch CLI has unconsumed runtime controls".to_owned());
    }

    let execution_policy = GovernedExecutionPolicy::new(
        lowered.timeout_secs,
        lowered.timeout_secs.min(MAX_RUNTIME_TIMEOUT_SECS),
        MAX_RUNTIME_STREAM_BYTES,
        MAX_RUNTIME_STREAM_BYTES,
        MAX_RUNTIME_STREAM_BYTES,
    )
    .map_err(|_| "governed implicit CLI policy is invalid".to_owned())?;
    let execution_contract =
        tool_runtime_core::governed_execution::GovernedExecutionContract::compile(
            validated,
            execution_policy,
        )
        .map_err(|_| "governed implicit CLI contract failed".to_owned())?;
    let request = GovernedExecutionRequest::new(
        lowered.arguments,
        lowered.stdin.map(String::into_bytes),
        lowered.working_directory,
        Some(lowered.timeout_secs),
    );
    let intent = execution_contract
        .admit(request)
        .map_err(|_| "governed implicit CLI invocation was rejected".to_owned())?;
    let scratch = CredentialScratchAuthority::open_or_create(&scope_root)
        .map_err(|_| "governed implicit CLI scratch authority is unavailable".to_owned())?;
    let filesystem = GovernedCredentialFilesystemRequest::new(&scratch, None);
    let envelope_id = governed_call_id("cli", exec_ctx);
    let _lease = bind_subprocess_storage_envelope(exec_ctx, &envelope_id)?;
    let call_id = CredentialCallId::new(envelope_id)
        .map_err(|_| "governed implicit CLI call identity is invalid".to_owned())?;
    let context = GovernedExecutionCallContext::new(call_id, capability_id, action_id)
        .map_err(|_| "governed implicit CLI call context is invalid".to_owned())?;
    let invocation = GovernedExecutionInvocation::new(
        context,
        validated,
        intent,
        &preparation,
        &injection,
        values,
        working_directory,
        None,
        Some(filesystem),
    )
    .map_err(|_| "governed implicit CLI coordinator binding failed".to_owned())?;

    let mut authorizer = AdmittedAuthorizer {
        _admission: admission,
    };
    let mut empty_resolver = EmptyResolver;
    let mut scoped_resolver;
    let resolver: &mut dyn CredentialMaterialResolver = if preparation.bindings().is_empty() {
        &mut empty_resolver
    } else {
        let secret_resolver = exec_ctx
            .secret_store_resolver
            .as_ref()
            .ok_or_else(|| "governed implicit CLI secret authority is unavailable".to_owned())?;
        let route = CredentialGrantRoute::new(capability_id, action_id, None)
            .map_err(|_| "governed implicit CLI credential route is invalid".to_owned())?;
        scoped_resolver =
            ScopedCredentialMaterialAdapter::for_secret_references_with_legacy_environment(
                Arc::clone(secret_resolver),
                validated,
                &preparation,
                route,
                runtime.legacy_secret_environment.as_deref(),
            )
            .map_err(|_| "governed implicit CLI credential adapter is invalid".to_owned())?;
        &mut scoped_resolver
    };
    let mut audit = ProductAuditSink { store: audit_store };
    let settlement = invocation
        .execute_batch(&mut authorizer, resolver, &mut audit, cancellation)
        .map_err(|error| {
            format!(
                "governed implicit CLI execution failed before settlement ({:?}, {:?})",
                error.code,
                error.dispatch()
            )
        })?;
    let result = settlement.result();
    Ok(governed_batch_result(
        exec_ctx,
        result.terminal().terminal(),
        result.exit_code(),
        result.stdout(),
        result.stderr(),
        elapsed_ms(started),
    ))
}

#[allow(clippy::too_many_arguments)]
fn dispatch_profile_free_cli_runtime(
    runtime: &GovernedRuntimeImplementation,
    admission: GovernedRouteAdmission,
    capability_id: &str,
    action_id: &str,
    lowered: LoweredTypedActionInvocation,
    validated: tool_runtime_core::manifest_validation::ValidatedSkillRuntimeContract<'_>,
    scope: CredentialScope,
    scope_root: ScopedPath,
    audit_store: &SecretStore,
    exec_ctx: &PrimitiveExecCtx,
    scope_paths: &CapabilityScopePaths,
    cancellation: &GovernedBatchCancellation,
    started: Instant,
) -> Result<PrimitiveToolResult, String> {
    if !matches!(
        validated.contract().auth.profile_selection,
        ProfileSelection::None
    ) || lowered.profile.is_some()
    {
        return Err("governed profile-free runtime cannot select a profile".to_owned());
    }

    let selection_request = CredentialProfileSelectionRequest::new(
        scope.clone(),
        validated.contract().auth.provider.as_deref(),
        CredentialProfileBinding::Provider,
        &validated.contract().auth.profile_selection,
        None,
    )
    .map_err(|_| "governed profile-free selection is invalid".to_owned())?;
    let selection = select_credential_profile(&NoProfileRegistry, &selection_request)
        .map_err(|_| "governed profile-free selection failed".to_owned())?;
    let auth_kind = validated.contract().auth.kind;
    let bindings = validated
        .contract()
        .auth
        .secret_bindings
        .iter()
        .map(|binding| {
            let name = CredentialMaterialBindingName::new(binding.name.clone())?;
            if matches!(
                validated.contract().auth.requirement,
                AuthRequirement::Optional | AuthRequirement::AtLeastOne
            ) {
                CredentialPreparationBinding::optional(
                    name,
                    CredentialMaterialKind::SecretBinding,
                    MAX_RUNTIME_SECRET_BINDING_BYTES,
                )
            } else {
                CredentialPreparationBinding::new(
                    name,
                    CredentialMaterialKind::SecretBinding,
                    MAX_RUNTIME_SECRET_BINDING_BYTES,
                )
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "governed profile-free binding is invalid".to_owned())?;
    let minimum_present = match validated.contract().auth.requirement {
        AuthRequirement::AtLeastOne => 1,
        AuthRequirement::Required | AuthRequirement::Conditional => bindings.len(),
        AuthRequirement::Optional | AuthRequirement::None => 0,
    };
    let preparation = CredentialPreparationPlan::new_with_minimum_present(
        scope,
        auth_kind,
        &selection,
        bindings,
        minimum_present,
    )
    .map_err(|_| "governed profile-free preparation failed".to_owned())?;
    let injection = CredentialInjectionPlan::compile(
        validated,
        &preparation,
        ChildEnvironmentBaseline::portable_cli(),
    )
    .map_err(|_| "governed profile-free injection failed".to_owned())?;

    let executable_directories = governed_executable_directories(exec_ctx, runtime, scope_paths)?;
    let values = baseline_values(
        injection.baseline(),
        &executable_directories,
        false,
        &runtime
            .cli_actions()
            .map_err(str::to_owned)?
            .execution
            .environment,
    )?;
    let execution_policy = GovernedExecutionPolicy::new(
        lowered.timeout_secs,
        lowered.timeout_secs.min(MAX_RUNTIME_TIMEOUT_SECS),
        MAX_RUNTIME_STREAM_BYTES,
        MAX_RUNTIME_STREAM_BYTES,
        MAX_RUNTIME_STREAM_BYTES,
    )
    .map_err(|_| "governed runtime execution policy is invalid".to_owned())?;
    let execution_contract =
        tool_runtime_core::governed_execution::GovernedExecutionContract::compile(
            validated,
            execution_policy,
        )
        .map_err(|_| "governed runtime execution contract failed".to_owned())?;
    let admitted_cwd = lowered
        .working_directory
        .as_ref()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let intent = execution_contract
        .admit(GovernedExecutionRequest::new(
            lowered.arguments,
            lowered.stdin.map(String::into_bytes),
            lowered.working_directory,
            Some(lowered.timeout_secs),
        ))
        .map_err(|_| "governed runtime invocation was rejected".to_owned())?;

    let envelope_id = governed_call_id("tool", exec_ctx);
    let _lease = bind_subprocess_storage_envelope(exec_ctx, &envelope_id)?;
    let working_directory = match &validated.contract().runtime {
        RuntimeProtocol::Cli {
            working_directory, ..
        } => match working_directory.mode {
            WorkingDirectoryMode::Denied => None,
            WorkingDirectoryMode::Workspace => {
                let cwd = admitted_cwd.unwrap_or(
                    scope_root
                        .revalidated_path()
                        .map_err(|_| "governed runtime working directory changed".to_owned())?
                        .to_path_buf(),
                );
                Some(
                    GovernedWorkingDirectoryRoot::open(WorkingDirectoryMode::Workspace, cwd)
                        .map_err(|_| {
                            "governed runtime working directory is unavailable".to_owned()
                        })?,
                )
            },
            WorkingDirectoryMode::OutputRoot => {
                return Err("governed output-root execution is not migrated yet".to_owned());
            },
        },
        RuntimeProtocol::Mcp { .. } => {
            return Err("governed static-secret CLI route is invalid".to_owned());
        },
    };
    let scratch = CredentialScratchAuthority::open_or_create(&scope_root)
        .map_err(|_| "governed runtime scratch authority is unavailable".to_owned())?;
    let filesystem = GovernedCredentialFilesystemRequest::new(&scratch, None);
    let call_id = CredentialCallId::new(envelope_id)
        .map_err(|_| "governed runtime call identity is invalid".to_owned())?;
    let context = GovernedExecutionCallContext::new(call_id, capability_id, action_id)
        .map_err(|_| "governed runtime call context is invalid".to_owned())?;
    let invocation = GovernedExecutionInvocation::new(
        context,
        validated,
        intent,
        &preparation,
        &injection,
        values,
        working_directory,
        None,
        Some(filesystem),
    )
    .map_err(|_| "governed runtime coordinator binding failed".to_owned())?;

    let mut empty_resolver = EmptyResolver;
    let mut scoped_resolver;
    let resolver: &mut dyn CredentialMaterialResolver = if preparation.bindings().is_empty() {
        &mut empty_resolver
    } else {
        let secret_resolver = exec_ctx
            .secret_store_resolver
            .as_ref()
            .ok_or_else(|| "governed runtime secret authority is unavailable".to_owned())?;
        let route = CredentialGrantRoute::new(capability_id, action_id, None)
            .map_err(|_| "governed runtime credential route is invalid".to_owned())?;
        scoped_resolver =
            ScopedCredentialMaterialAdapter::for_secret_references_with_legacy_environment(
                Arc::clone(secret_resolver),
                validated,
                &preparation,
                route,
                runtime.legacy_secret_environment.as_deref(),
            )
            .map_err(|_| "governed runtime credential adapter is invalid".to_owned())?;
        &mut scoped_resolver
    };
    let mut authorizer = AdmittedAuthorizer {
        _admission: admission,
    };
    let mut audit = ProductAuditSink { store: audit_store };
    let settlement = invocation
        .execute_batch(&mut authorizer, resolver, &mut audit, cancellation)
        .map_err(|error| {
            format!(
                "governed runtime execution failed before settlement ({:?}, {:?})",
                error.code,
                error.dispatch()
            )
        })?;
    let result = settlement.result();
    Ok(governed_batch_result(
        exec_ctx,
        result.terminal().terminal(),
        result.exit_code(),
        result.stdout(),
        result.stderr(),
        elapsed_ms(started),
    ))
}

fn governed_batch_result(
    exec_ctx: &PrimitiveExecCtx,
    terminal: GovernedExecutionTerminal,
    exit_code: Option<i32>,
    stdout_bytes: &[u8],
    stderr_bytes: &[u8],
    elapsed_ms: u64,
) -> PrimitiveToolResult {
    let success = terminal == GovernedExecutionTerminal::Success;
    // Scrubbed with the run's delivered values before it becomes a result: this
    // text also becomes the dispatch error, the log line and a durable
    // learning-evidence record, none of which get another chance.
    let stdout = scrub_with_delivered(exec_ctx, String::from_utf8_lossy(stdout_bytes).into_owned());
    let mut stderr =
        scrub_with_delivered(exec_ctx, String::from_utf8_lossy(stderr_bytes).into_owned());
    if !success && stdout.trim().is_empty() && stderr.trim().is_empty() {
        stderr = match exit_code {
            Some(code) => format!("governed runtime ended with {terminal:?} (exit code {code})"),
            None => format!("governed runtime ended with {terminal:?}"),
        };
    }
    PrimitiveToolResult {
        success,
        parsed_json: serde_json::from_slice(if success { stdout_bytes } else { stderr_bytes }).ok(),
        stdout,
        stderr,
        elapsed_ms,
        ..PrimitiveToolResult::default()
    }
}

fn normalize_runtime_controls(
    package: &SkillRuntimePackage,
    input: Value,
) -> Result<Value, String> {
    let Value::Object(mut normalized) = input else {
        return Err("governed runtime input must be an object".to_owned());
    };
    if let Some(parameter) = package.projected_profile_parameter() {
        if parameter.name != "profile" {
            if normalized.contains_key("profile") {
                return Err("governed runtime profile selector is ambiguous".to_owned());
            }
            if let Some(value) = normalized.remove(parameter.name) {
                normalized.insert("profile".to_owned(), value);
            }
        }
    }
    if let Some(parameter) = package.projected_working_directory_parameter() {
        if parameter.name != "working_dir" {
            if normalized.contains_key("working_dir") {
                return Err("governed runtime working-directory selector is ambiguous".to_owned());
            }
            if let Some(value) = normalized.remove(parameter.name) {
                normalized.insert("working_dir".to_owned(), value);
            }
        }
    }
    Ok(Value::Object(normalized))
}

fn bind_workspace_path_parameters(
    action: &tool_runtime_core::action_overrides::CompiledTypedAction,
    input: &mut Value,
    scope_root: &ScopedPath,
    brokered: &BTreeMap<String, BrokeredWorkspacePath>,
) -> Result<(), String> {
    let values = input
        .as_object_mut()
        .ok_or_else(|| "governed runtime input must be an object".to_owned())?;
    if brokered
        .keys()
        .any(|name| !action.parameters.contains_key(name))
    {
        return Err("broker-owned workspace path targets an unknown parameter".to_owned());
    }
    let scope_root = scope_root
        .revalidated_path()
        .map_err(|_| "governed workspace path root changed".to_owned())?;
    let scope_root = fs::canonicalize(scope_root)
        .map_err(|_| "governed workspace path root is unavailable".to_owned())?;
    let mut workspace_root = None;

    for (name, parameter) in &action.parameters {
        let TypedActionParameter::WorkspacePath { access, .. } = parameter else {
            if brokered.contains_key(name) {
                return Err("broker-owned path requires a workspace_path parameter".to_owned());
            }
            continue;
        };
        let Some(value) = values.get_mut(name) else {
            if brokered.contains_key(name) {
                return Err("broker-owned workspace path parameter is missing".to_owned());
            }
            continue;
        };
        let raw = value
            .as_str()
            .ok_or_else(|| "workspace path parameter must be a string".to_owned())?;
        let resolved = if let Some(binding) = brokered.get(name) {
            bind_brokered_workspace_path(raw, *access, binding)?
        } else {
            if workspace_root.is_none() {
                workspace_root = Some(resolve_workspace_file_root(&scope_root)?);
            }
            let root = workspace_root
                .as_deref()
                .ok_or_else(|| "governed workspace file root is unavailable".to_owned())?;
            bind_scoped_workspace_path(root, raw, *access)?
        };
        *value = Value::String(resolved.to_string_lossy().into_owned());
    }
    Ok(())
}

fn resolve_workspace_file_root(scope_root: &Path) -> Result<PathBuf, String> {
    let canonical_scope_root = fs::canonicalize(scope_root)
        .map_err(|_| "governed workspace scope root is unavailable".to_owned())?;
    if !fs::metadata(&canonical_scope_root).is_ok_and(|metadata| metadata.is_dir()) {
        return Err("governed workspace scope root must be a directory".to_owned());
    }
    let candidate = scope_root.join("workdirs");
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|_| "governed workspace file root is unavailable".to_owned())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("governed workspace file root must be a real directory".to_owned());
    }
    let canonical = fs::canonicalize(candidate)
        .map_err(|_| "governed workspace file root changed".to_owned())?;
    if !canonical.starts_with(&canonical_scope_root) {
        return Err("governed workspace file root escaped its scope".to_owned());
    }
    Ok(canonical)
}

fn bind_brokered_workspace_path(
    raw: &str,
    access: WorkspacePathAccess,
    binding: &BrokeredWorkspacePath,
) -> Result<PathBuf, String> {
    if access != binding.access || Path::new(raw) != binding.path {
        return Err("broker-owned workspace path does not match the admitted binding".to_owned());
    }
    match access {
        WorkspacePathAccess::ReadFile => {
            let metadata = fs::symlink_metadata(&binding.path)
                .map_err(|_| "broker-owned input file is unavailable".to_owned())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("broker-owned input must be a regular non-symlink file".to_owned());
            }
            fs::canonicalize(&binding.path)
                .map_err(|_| "broker-owned input file changed".to_owned())
        },
        WorkspacePathAccess::CreateFile | WorkspacePathAccess::CreateDirectory => {
            Err("broker-owned output paths are not supported".to_owned())
        },
    }
}

fn bind_scoped_workspace_path(
    root: &Path,
    raw: &str,
    access: WorkspacePathAccess,
) -> Result<PathBuf, String> {
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|_| "governed workspace file root is unavailable".to_owned())?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("governed workspace file root must be a real directory".to_owned());
    }
    let root =
        fs::canonicalize(root).map_err(|_| "governed workspace file root changed".to_owned())?;
    let relative = Path::new(raw);
    let normalized = relative.components().collect::<PathBuf>();
    if raw.contains('\\')
        || relative.is_absolute()
        || normalized.as_os_str() != relative.as_os_str()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("workspace path must be a normalized relative path".to_owned());
    }
    let candidate = root.join(relative);
    match access {
        WorkspacePathAccess::ReadFile => {
            reject_symlink_components(&root, relative, true)?;
            let canonical = fs::canonicalize(&candidate)
                .map_err(|_| "workspace input file is unavailable".to_owned())?;
            if !canonical.starts_with(&root)
                || !fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_file())
            {
                return Err(
                    "workspace input must be a regular file beneath the scope root".to_owned(),
                );
            }
            Ok(canonical)
        },
        WorkspacePathAccess::CreateFile | WorkspacePathAccess::CreateDirectory => {
            if fs::symlink_metadata(&candidate).is_ok() {
                return Err("workspace output path already exists".to_owned());
            }
            let parent = relative
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            reject_symlink_components(&root, parent, true)?;
            let parent = fs::canonicalize(root.join(parent))
                .map_err(|_| "workspace output parent is unavailable".to_owned())?;
            if !parent.starts_with(&root)
                || !fs::metadata(&parent).is_ok_and(|metadata| metadata.is_dir())
            {
                return Err(
                    "workspace output parent must be a directory beneath the scope root".to_owned(),
                );
            }
            Ok(candidate)
        },
    }
}

fn reject_symlink_components(
    root: &Path,
    relative: &Path,
    include_leaf: bool,
) -> Result<(), String> {
    if relative == Path::new(".") || relative.as_os_str().is_empty() {
        return Ok(());
    }
    let components = relative.components().collect::<Vec<_>>();
    let count = if include_leaf {
        components.len()
    } else {
        components.len().saturating_sub(1)
    };
    let mut current = root.to_path_buf();
    for component in components.into_iter().take(count) {
        let Component::Normal(component) = component else {
            return Err("workspace path contains an invalid component".to_owned());
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| "workspace path component is unavailable".to_owned())?;
        if metadata.file_type().is_symlink() {
            return Err("workspace path cannot traverse a symlink".to_owned());
        }
    }
    Ok(())
}

fn implicit_cli_working_directory(
    validated: tool_runtime_core::manifest_validation::ValidatedSkillRuntimeContract<'_>,
    requested: Option<String>,
    exec_ctx: &PrimitiveExecCtx,
) -> Result<
    (
        Option<GovernedWorkingDirectoryRoot>,
        Option<String>,
        Option<PathBuf>,
    ),
    String,
> {
    let mode = match &validated.contract().runtime {
        RuntimeProtocol::Cli {
            working_directory, ..
        } => working_directory.mode,
        RuntimeProtocol::Mcp { .. } => {
            return Err("governed implicit CLI runtime requires a CLI contract".to_owned());
        },
    };
    if mode == WorkingDirectoryMode::Denied {
        return if requested.is_none() {
            Ok((None, None, None))
        } else {
            Err("governed implicit CLI working directory is denied".to_owned())
        };
    }
    if mode != WorkingDirectoryMode::Workspace {
        return Err("governed implicit CLI output-root execution is unsupported".to_owned());
    }

    let current = fs::canonicalize(
        std::env::current_dir()
            .map_err(|_| "governed implicit CLI current workspace is unavailable".to_owned())?,
    )
    .map_err(|_| "governed implicit CLI current workspace is unavailable".to_owned())?;
    let Some(requested) = requested else {
        let root = GovernedWorkingDirectoryRoot::open(mode, &current)
            .map_err(|_| "governed implicit CLI workspace is unsafe".to_owned())?;
        return Ok((Some(root), None, Some(current)));
    };
    if requested == "." {
        let root = GovernedWorkingDirectoryRoot::open(mode, &current)
            .map_err(|_| "governed implicit CLI workspace is unsafe".to_owned())?;
        return Ok((Some(root), None, Some(current)));
    }
    let requested_path = Path::new(&requested);
    if !requested_path.is_absolute() {
        let actual = fs::canonicalize(current.join(&requested))
            .map_err(|_| "governed implicit CLI requested workspace is unavailable".to_owned())?;
        let root = GovernedWorkingDirectoryRoot::open(mode, &current)
            .map_err(|_| "governed implicit CLI workspace is unsafe".to_owned())?;
        return Ok((Some(root), Some(requested), Some(actual)));
    }

    let canonical = fs::canonicalize(requested_path)
        .map_err(|_| "governed implicit CLI requested workspace is unavailable".to_owned())?;
    if canonical != requested_path {
        return Err("governed implicit CLI requested workspace is not canonical".to_owned());
    }
    let mut authorized = canonical == current || canonical.starts_with(&current);
    if !authorized {
        let roots = exec_ctx
            .session_file_sandbox_roots
            .as_ref()
            .ok_or_else(|| {
                "governed implicit CLI requested workspace is not authorized".to_owned()
            })?
            .lock()
            .map_err(|_| "governed implicit CLI workspace authority is unavailable".to_owned())?;
        authorized = roots.iter().any(|root| {
            fs::canonicalize(root)
                .ok()
                .is_some_and(|root| canonical == root || canonical.starts_with(root))
        });
    }
    if !authorized {
        return Err("governed implicit CLI requested workspace is not authorized".to_owned());
    }
    let root = GovernedWorkingDirectoryRoot::open(mode, &canonical)
        .map_err(|_| "governed implicit CLI requested workspace is unsafe".to_owned())?;
    Ok((Some(root), None, Some(canonical)))
}

fn apply_effective_policy(base: &mut PolicyFloor, effective: &EffectiveActionPolicy) {
    if let Some(approval) = effective.required_approvals.iter().max().copied() {
        base.approval = base.approval.max(approval);
    }
    base.required_grants
        .extend(effective.required_grants.iter().cloned());
    base.resource_scopes
        .extend(effective.resource_scopes.iter().cloned());
    base.required_resource_authorities
        .extend(effective.required_resource_authorities.iter().cloned());
}

fn profile_directory_for_contract(
    storage: &AuthStorage,
    alias: &str,
) -> Result<ScopedPathComponent, String> {
    let AuthStorage::ScopedDirectory {
        namespace,
        partition_by_profile,
    } = storage
    else {
        return Err("governed runtime profile storage is not scoped".to_owned());
    };
    let directory = if *partition_by_profile {
        format!("{namespace}-{alias}")
    } else {
        namespace.clone()
    };
    ScopedPathComponent::new(directory)
        .map_err(|_| "governed runtime profile path is invalid".to_owned())
}

fn operator_config_source(exec_ctx: &PrimitiveExecCtx) -> Result<Zeroizing<String>, String> {
    if let Some(source) = &exec_ctx.governed_operator_config_source {
        return Ok(Zeroizing::new(source.to_string()));
    }
    let runtime = exec_ctx.storage_base_path.join("operator-config.yaml");
    let path = if runtime.is_file() {
        runtime
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("skillshub/operator-config.yaml")
    };
    let metadata = fs::metadata(&path)
        .map_err(|_| "governed runtime operator configuration is unavailable".to_owned())?;
    if metadata.len() > MAX_OPERATOR_CONFIG_BYTES {
        return Err("governed runtime operator configuration is too large".to_owned());
    }
    fs::read_to_string(path)
        .map(Zeroizing::new)
        .map_err(|_| "governed runtime operator configuration is unreadable".to_owned())
}

fn record_lifecycle_audit(
    store: &SecretStore,
    capability_id: &str,
    action_id: &str,
    event: &str,
    detail: Value,
) -> Result<(), String> {
    store
        .try_audit_event(
            SecretAuditEvent::new(event)
                .with_tool(capability_id)
                .with_action(action_id)
                .with_detail(detail.to_string()),
        )
        .map_err(|_| "governed runtime lifecycle audit failed".to_owned())
}

/// Every binary in this catalog that some route can bind as execution
/// authority, and therefore copy into the private single-file snapshot.
///
/// A v2 action may name any reviewed requirement as its own executable, and the
/// execute path then rebinds `requires.bins` to exactly that one name before it
/// binds execution authority. So the binaries that can reach the snapshot are
/// the package default plus every action's override -- never the whole of
/// `requires.bins`. The set is computed over all actions rather than the
/// invoked one because the credential-lifecycle and profile-free routes build
/// the same PATH with no single action in scope, and a requirement must not
/// change role depending on which route resolved it.
fn snapshot_target_executables(actions: &CompiledActionCatalog) -> BTreeSet<&str> {
    let mut targets = BTreeSet::from([actions.execution.executable.as_str()]);
    targets.extend(
        actions
            .actions
            .values()
            .map(|action| action.invocation.executable.as_str()),
    );
    targets
}

/// Why a requirement is being resolved to a directory.
///
/// The two roles have genuinely different requirements, and conflating them is
/// what made every `mmx`-backed package undispatchable. Only the binary an
/// action actually names is bound by [`ResolvedExecutable::resolve`] and copied
/// into the private single-file snapshot, so only that binary must survive
/// relocation. Every other `requires.bins` entry exists so the child's PATH can
/// reach it, and the child reaches it the ordinary way: it execs the entry in
/// place, from its real path, beside its real siblings. Nothing about it is
/// ever copied, so snapshot-safety is not a question that applies to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutableRole {
    /// An action can name this binary, so it can become the snapshot target.
    SnapshotTarget,
    /// Declared only so the child's PATH can reach it; never snapshotted.
    PathCompanion,
}

fn governed_executable_directory(
    exec_ctx: &PrimitiveExecCtx,
    runtime: &GovernedRuntimeImplementation,
    scope_paths: &CapabilityScopePaths,
    executable_name: &str,
    role: ExecutableRole,
) -> Result<PathBuf, String> {
    let configured_directory = exec_ctx
        .governed_executable_directory
        .clone()
        .or_else(|| runtime.executable_directory.clone());
    if let Some(directory) = configured_directory {
        if let Some(reviewed) = reviewed_executable_directory(&directory, executable_name, role) {
            return Ok(reviewed);
        }
    }

    // Shared dependency roots are runtime-owned installation authorities. Resolve a
    // plain standalone executable directly, or an npm package's adjacent `.bin_real`
    // artifact when the public `.bin` entry is a JavaScript launcher. The latter is a
    // package-layout rule rather than provider knowledge and keeps native CLI skills
    // drop-in without admitting an interpreter wrapper into the single-file snapshot.
    for directory in [
        &scope_paths.node_modules_bin,
        &scope_paths.venv_bin,
        &scope_paths.node_bin,
    ] {
        if let Some(directory) = reviewed_executable_directory(directory, executable_name, role) {
            return Ok(directory);
        }
    }
    if let Ok(executable) = which::which(executable_name) {
        if let Some(directory) = executable
            .parent()
            .and_then(|directory| reviewed_executable_directory(directory, executable_name, role))
        {
            return Ok(directory);
        }
    }
    Err("governed runtime executable directory is unavailable".to_owned())
}

fn governed_executable_directories(
    exec_ctx: &PrimitiveExecCtx,
    runtime: &GovernedRuntimeImplementation,
    scope_paths: &CapabilityScopePaths,
) -> Result<Vec<PathBuf>, String> {
    let mut executable_names = Vec::with_capacity(runtime.package.contract.requires.bins.len());
    let actions = runtime.cli_actions().map_err(str::to_owned)?;
    executable_names.push(actions.execution.executable.as_str());
    executable_names.extend(
        runtime
            .package
            .contract
            .requires
            .bins
            .iter()
            .map(String::as_str)
            .filter(|name| *name != actions.execution.executable.as_str()),
    );

    let snapshot_targets = snapshot_target_executables(actions);
    let mut directories = Vec::with_capacity(executable_names.len());
    for executable_name in executable_names {
        let role = if snapshot_targets.contains(executable_name) {
            ExecutableRole::SnapshotTarget
        } else {
            ExecutableRole::PathCompanion
        };
        let resolved =
            governed_executable_directory(exec_ctx, runtime, scope_paths, executable_name, role);
        let directory = match (resolved, role) {
            (Ok(directory), _) => directory,
            // A snapshot target is execution authority: the invocation names
            // this exact process, so an unresolved one has nothing to run and
            // must fail here rather than exec whatever a stale PATH offers.
            (Err(error), ExecutableRole::SnapshotTarget) => return Err(error),
            // A companion exists only so the child's PATH can reach it. When
            // this host does not carry it there is nothing to add to PATH and
            // nothing that authority depends on, so the correct answer is a
            // shorter PATH -- not a dead skill.
            //
            // Failing the whole build here is what made a *declaration* worse
            // than silence: `requires.bins` has no optional form, so naming the
            // companion of one engine (ocr's `tesseract`/`pdftoppm`) took every
            // other engine down with it on a host that has only the VLM CLIs,
            // and naming an interpreter would do the same to every skill that
            // merely prefers it. The child now reaches the point that actually
            // needs the companion and answers with its own account of what is
            // missing, which is the report an operator can act on.
            //
            // Nothing about *where* a companion may come from is relaxed:
            // `reviewed_executable_directory` still canonicalizes, still
            // demands a non-empty regular file, and still bounds the
            // `.bin_real` and native-artifact redirections. This governs only
            // what happens when no admissible directory exists at all.
            (Err(_), ExecutableRole::PathCompanion) => {
                tracing::debug!(
                    executable = executable_name,
                    entrypoint = actions.execution.executable.as_str(),
                    "governed PATH companion is not installed on this host; \
                     continuing without it"
                );
                continue;
            },
        };
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    Ok(directories)
}

fn reviewed_executable_directory(
    directory: &Path,
    executable_name: &str,
    role: ExecutableRole,
) -> Option<PathBuf> {
    let directory = fs::canonicalize(directory).ok()?;
    let executable = directory.join(executable_name);
    let metadata = fs::symlink_metadata(&executable).ok()?;
    if metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() > 0 {
        return Some(directory);
    }
    if !metadata.file_type().is_symlink() {
        return None;
    }

    let resolved = fs::canonicalize(&executable).ok()?;
    let package_root = directory.parent().and_then(|package_authority| {
        resolved.ancestors().skip(1).find(|ancestor| {
            ancestor.starts_with(package_authority) && ancestor.join("package.json").is_file()
        })
    });
    if let Some(package_root) = package_root {
        let standalone_root = package_root.join("node_modules/.bin_real");
        if fs::canonicalize(&standalone_root).ok().as_deref() == Some(standalone_root.as_path()) {
            let standalone = standalone_root.join(executable_name);
            if let Ok(standalone_metadata) = fs::symlink_metadata(&standalone) {
                if standalone_metadata.is_file()
                    && !standalone_metadata.file_type().is_symlink()
                    && standalone_metadata.len() > 0
                {
                    return standalone.parent().map(Path::to_path_buf);
                }
            }
        }
        if let Some(native_directory) =
            reviewed_native_package_executable_directory(package_root, executable_name, &resolved)
        {
            return Some(native_directory);
        }
    }
    let resolved_metadata = fs::symlink_metadata(&resolved).ok()?;
    if !resolved_metadata.is_file()
        || resolved_metadata.file_type().is_symlink()
        || resolved_metadata.len() == 0
    {
        return None;
    }
    // What actually launches is the canonical regular file behind this entry:
    // `ResolvedExecutable::resolve` follows the link, hashes the target, and
    // snapshots those exact bytes. So a rule that admits a file placed directly
    // in the directory but refuses the byte-identical file reached through a
    // link is not a boundary — it is an inconsistency, and it is why a vendored
    // venv console script (marimo) and the committed `bin/` wrappers that other
    // skills ship were unreachable while an equivalent regular file installed by
    // a host package manager resolved fine.
    //
    // The native test still governs the one shape it was written for: an entry
    // inside a package tree, where the launcher resolves modules adjacent to
    // itself and cannot survive relocation into the single-file snapshot. Inside
    // such a tree the two branches above are the only admissible answers, so a
    // launcher that reached this point stays refused.
    //
    // Outside a package tree a `#!` entry point carries its interpreter in its
    // own first line and is snapshot-safe: the kernel launches the interpreter
    // from that interpreter's real path, and the script body travels with the
    // copy. Nothing self-relative is implied by the shape.
    //
    // All of that reasoning is about surviving the copy, so it only governs a
    // binary that is actually copied. A companion requirement is never bound as
    // execution authority and never snapshotted -- the child execs it in place
    // through PATH, exactly as any other process on this machine would, so it
    // keeps its real path and its real siblings and the package-tree question
    // does not arise. Refusing it on snapshot grounds withheld nothing from an
    // attacker; it only made the package undispatchable, because a single
    // unresolvable requirement fails the whole PATH build. Everything that
    // constrains *where* the directory may come from -- canonicalization, the
    // regular-file and non-empty checks, and the install-authority containment
    // that bounds the `.bin_real` and native-artifact redirections -- is
    // enforced above and applies to both roles unchanged.
    match role {
        ExecutableRole::SnapshotTarget => {
            let snapshot_safe = native_executable_file(&resolved)
                || (package_root.is_none() && script_executable_file(&resolved));
            snapshot_safe.then_some(directory)
        },
        ExecutableRole::PathCompanion => Some(directory),
    }
}

/// Locate one standalone native executable shipped below an installed package
/// whose public PATH entry is an interpreter launcher. The scan is iterative,
/// does not follow links, is confined to the canonical package tree, and fails
/// closed on ambiguity or budget exhaustion. This covers npm's optional
/// platform-package layout without parsing or executing provider-owned JS.
fn reviewed_native_package_executable_directory(
    package_root: &Path,
    executable_name: &str,
    launcher: &Path,
) -> Option<PathBuf> {
    const MAX_PACKAGE_SCAN_ENTRIES: usize = 4_096;
    const MAX_PACKAGE_SCAN_DEPTH: usize = 10;

    let package_root = fs::canonicalize(package_root).ok()?;
    let launcher = fs::canonicalize(launcher).ok()?;
    let mut queue = VecDeque::from([(package_root.clone(), 0usize)]);
    let mut inspected = 0usize;
    let mut candidate = None;
    while let Some((directory, depth)) = queue.pop_front() {
        let entries = fs::read_dir(&directory).ok()?;
        for entry in entries {
            inspected = inspected.checked_add(1)?;
            if inspected > MAX_PACKAGE_SCAN_ENTRIES {
                return None;
            }
            let entry = entry.ok()?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).ok()?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if depth >= MAX_PACKAGE_SCAN_DEPTH {
                    return None;
                }
                queue.push_back((path, depth + 1));
                continue;
            }
            if !metadata.is_file()
                || metadata.len() == 0
                || path == launcher
                || path.file_name().and_then(|name| name.to_str()) != Some(executable_name)
                || !native_executable_file(&path)
            {
                continue;
            }
            let canonical = fs::canonicalize(path).ok()?;
            if !canonical.starts_with(&package_root) || candidate.replace(canonical).is_some() {
                return None;
            }
        }
    }
    candidate.and_then(|path| path.parent().map(Path::to_path_buf))
}

fn native_executable_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0_u8; 4];
    let Ok(read) = file.read(&mut magic) else {
        return false;
    };
    native_executable_magic(&magic[..read])
}

/// A `#!` entry point names its own interpreter, so relocating the file into the
/// private snapshot directory does not change what runs: the kernel launches the
/// interpreter from that interpreter's own real path and hands it the copied
/// script. Only the first two bytes are read; the interpreter line is never
/// parsed, compared, or resolved here.
fn script_executable_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0_u8; 2];
    let Ok(read) = file.read(&mut magic) else {
        return false;
    };
    magic[..read].starts_with(b"#!")
}

fn native_executable_magic(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some(b"\x7fELF")
            | Some(b"\xfe\xed\xfa\xce")
            | Some(b"\xfe\xed\xfa\xcf")
            | Some(b"\xce\xfa\xed\xfe")
            | Some(b"\xcf\xfa\xed\xfe")
            | Some(b"\xca\xfe\xba\xbe")
            | Some(b"\xbe\xba\xfe\xca")
    ) || bytes.starts_with(b"MZ")
}

/// Runtime-owned CA bundle candidates, ordered by trust rather than by
/// specificity. The first match wins, so a store any non-root actor can rewrite
/// must never precede one only root can replace; add new candidates to the
/// group that matches who owns the path, not to the end of the list. The value
/// is never read from the parent environment: a caller-supplied `SSL_CERT_FILE`
/// must not be able to redirect governed TLS trust.
const CA_BUNDLE_CANDIDATES: &[&str] = &[
    // OS-owned stores first: a non-root actor cannot replace these.
    "/etc/ssl/cert.pem",                  // macOS, Alpine, Arch
    "/etc/ssl/certs/ca-certificates.crt", // Debian/Ubuntu
    "/etc/pki/tls/certs/ca-bundle.crt",   // RHEL/Fedora/Amazon
    "/etc/ssl/ca-bundle.pem",             // SUSE
    // Package-manager prefixes last: writable without root on a typical host.
    "/opt/homebrew/etc/ca-certificates/cert.pem",
    "/usr/local/etc/openssl@3/cert.pem",
];

/// Pick the first candidate that is a readable regular file. Split from
/// [`resolve_ca_bundle`] so the ordering guarantee above is testable against
/// temporary paths instead of the host's real trust stores.
fn first_bundle(candidates: &[&'static str]) -> Option<&'static str> {
    candidates.iter().copied().find(|candidate| {
        let path = Path::new(candidate);
        // Following symlinks is DELIBERATE here. Every other filesystem check in
        // this file uses `fs::symlink_metadata` and rejects links, but Arch and
        // several other distributions ship `/etc/ssl/cert.pem` as a symlink into
        // the extracted trust store, so rejecting symlinks would break exactly
        // the hosts this targets. Do not "restore consistency" here.
        //
        // `is_file` does not prove readability, and binding an unreadable path is
        // strictly worse than binding nothing: OpenSSL then fails against the
        // named file instead of falling back to the interpreter default.
        path.is_file() && fs::File::open(path).is_ok()
    })
}

/// Resolve once per process. `baseline_values` runs on every governed dispatch,
/// so an uncached sweep would re-stat the candidates and an uncached warning
/// would repeat for each call.
fn resolve_ca_bundle() -> Option<&'static str> {
    static RESOLVED: OnceLock<Option<&'static str>> = OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let found = first_bundle(CA_BUNDLE_CANDIDATES);
        match found {
            // Logged so it is possible to tell which store a child actually got.
            Some(bundle) => tracing::debug!(
                bundle,
                "governed child TLS trust bound to a runtime-owned CA bundle"
            ),
            // Without this the operator sees only SSLCertVerificationError across
            // every HTTPS adapter, indistinguishable from a genuinely bad cert.
            None => tracing::warn!(
                candidates = CA_BUNDLE_CANDIDATES.len(),
                "no CA bundle resolved; governed adapters fall back to the interpreter default"
            ),
        }
        found
    })
}

fn baseline_values(
    baseline: &ChildEnvironmentBaseline,
    executable_directories: &[PathBuf],
    interactive_pty: bool,
    fixed_environment: &BTreeMap<String, String>,
) -> Result<ChildEnvironmentValues, String> {
    let mut values = ChildEnvironmentValues::new(baseline);
    let mut search_path = executable_directories.to_vec();
    search_path.push(PathBuf::from("/usr/bin"));
    search_path.push(PathBuf::from("/bin"));
    let path = std::env::join_paths(search_path)
        .map_err(|_| "governed runtime PATH construction failed".to_owned())?;
    values
        .provide(
            ChildEnvironmentVariable::Path,
            path.to_string_lossy().into_owned().into_bytes(),
        )
        .map_err(|_| "governed runtime PATH binding failed".to_owned())?;
    if baseline
        .variables()
        .contains(&ChildEnvironmentVariable::Home)
    {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| "governed CLI-owned HOME is unavailable".to_owned())?;
        values
            .provide(
                ChildEnvironmentVariable::Home,
                home.to_string_lossy().into_owned().into_bytes(),
            )
            .map_err(|_| "governed CLI-owned HOME binding failed".to_owned())?;
    }
    values
        .provide(ChildEnvironmentVariable::Lang, b"C.UTF-8".to_vec())
        .map_err(|_| "governed runtime locale binding failed".to_owned())?;
    values
        .provide(ChildEnvironmentVariable::LcAll, b"C.UTF-8".to_vec())
        .map_err(|_| "governed runtime locale binding failed".to_owned())?;
    values
        .provide(ChildEnvironmentVariable::LcCtype, b"C.UTF-8".to_vec())
        .map_err(|_| "governed runtime locale binding failed".to_owned())?;
    let terminal = if interactive_pty {
        b"xterm-256color".to_vec()
    } else {
        b"dumb".to_vec()
    };
    values
        .provide(ChildEnvironmentVariable::Term, terminal)
        .map_err(|_| "governed runtime terminal binding failed".to_owned())?;
    values
        .provide(ChildEnvironmentVariable::Tz, b"UTC".to_vec())
        .map_err(|_| "governed runtime timezone binding failed".to_owned())?;
    if baseline
        .variables()
        .contains(&ChildEnvironmentVariable::SslCertFile)
    {
        // An absent bundle degrades to the interpreter default rather than
        // refusing every governed CLI call on a host with no readable store.
        if let Some(bundle) = resolve_ca_bundle() {
            values
                .provide(
                    ChildEnvironmentVariable::SslCertFile,
                    bundle.as_bytes().to_vec(),
                )
                .map_err(|_| "governed runtime CA bundle binding failed".to_owned())?;
        }
    }
    for (name, value) in fixed_environment {
        values
            .provide_fixed(name.clone(), value.as_bytes().to_vec())
            .map_err(|_| "governed runtime fixed environment binding failed".to_owned())?;
    }
    Ok(values)
}

/// Sentinel registry for auth kinds whose contract explicitly selects no
/// profile. `select_credential_profile` must not call any method on this type;
/// returning a stable unavailable error keeps that invariant fail-closed if it
/// ever regresses. Shared with the app OS-jail owner's static-secret plan.
pub(crate) struct NoProfileRegistry;

impl CredentialProfileRegistry for NoProfileRegistry {
    fn snapshot(
        &self,
        _scope: &CredentialScope,
    ) -> Result<CredentialProfileRegistrySnapshot, CredentialProfileError> {
        Err(CredentialProfileError::registry_unavailable())
    }

    fn status(
        &self,
        _key: &CredentialProfileKey,
    ) -> Result<Option<CredentialProfileStatus>, CredentialProfileError> {
        Err(CredentialProfileError::registry_unavailable())
    }

    fn create_reference(
        &self,
        _request: CreateCredentialProfileReference,
    ) -> Result<CredentialProfileStatus, CredentialProfileError> {
        Err(CredentialProfileError::registry_unavailable())
    }

    fn update_metadata(
        &self,
        _request: UpdateCredentialProfileMetadata,
    ) -> Result<CredentialProfileStatus, CredentialProfileError> {
        Err(CredentialProfileError::registry_unavailable())
    }

    fn set_disabled(
        &self,
        _request: SetCredentialProfileDisabled,
    ) -> Result<CredentialProfileStatus, CredentialProfileError> {
        Err(CredentialProfileError::registry_unavailable())
    }
}

struct VerifiedProfileRegistry<'a> {
    base: &'a LocalCredentialProfileRegistry,
    ready: CredentialProfileStatus,
}

impl CredentialProfileRegistry for VerifiedProfileRegistry<'_> {
    fn snapshot(
        &self,
        scope: &CredentialScope,
    ) -> Result<CredentialProfileRegistrySnapshot, CredentialProfileError> {
        let base = self.base.snapshot(scope)?;
        let profiles = base
            .profiles()
            .iter()
            .map(|status| {
                if status.key() == self.ready.key() {
                    self.ready.clone()
                } else {
                    status.clone()
                }
            })
            .collect();
        CredentialProfileRegistrySnapshot::new(scope.clone(), profiles)
    }

    fn status(
        &self,
        key: &CredentialProfileKey,
    ) -> Result<Option<CredentialProfileStatus>, CredentialProfileError> {
        if key == self.ready.key() {
            Ok(Some(self.ready.clone()))
        } else {
            self.base.status(key)
        }
    }

    fn create_reference(
        &self,
        request: CreateCredentialProfileReference,
    ) -> Result<CredentialProfileStatus, CredentialProfileError> {
        self.base.create_reference(request)
    }

    fn update_metadata(
        &self,
        request: UpdateCredentialProfileMetadata,
    ) -> Result<CredentialProfileStatus, CredentialProfileError> {
        self.base.update_metadata(request)
    }

    fn set_disabled(
        &self,
        request: SetCredentialProfileDisabled,
    ) -> Result<CredentialProfileStatus, CredentialProfileError> {
        self.base.set_disabled(request)
    }
}

struct EmptyResolver;

impl CredentialMaterialResolver for EmptyResolver {
    fn resolve_once(
        &mut self,
        _plan: &CredentialPreparationPlan,
        _sink: &mut CredentialMaterialSink<'_>,
    ) -> Result<(), CredentialPreparationError> {
        Ok(())
    }
}

/// The prompt a login could not answer: the typed `authentication_required`
/// outcome of the CLI lane (plan §5.1). Value-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LifecycleAuthenticationChallenge {
    pub kind: LifecyclePromptKind,
    pub program: String,
    pub marker: String,
    /// Why the material the run held could not be delivered, when it held
    /// some (expired, spent, bound elsewhere); `None` when it held none.
    pub reason: Option<String>,
}

impl LifecycleAuthenticationChallenge {
    /// The run-level challenge the next secure ask binds to, for the kinds a
    /// secure ask can answer.
    pub(crate) fn as_authentication_challenge(
        &self,
    ) -> Option<crate::magician_v2::secrets::challenge::AuthenticationChallenge> {
        crate::magician_v2::secrets::challenge::AuthenticationChallenge::from_tool_result_json(
            &serde_json::json!({
                "status": "authentication_required",
                "kind": self.kind_name(),
                "program": self.program,
                "prompt": self.marker,
            }),
        )
    }

    pub(crate) fn kind_name(&self) -> &'static str {
        match self.kind {
            LifecyclePromptKind::Username => "username",
            LifecyclePromptKind::Password => "password",
            LifecyclePromptKind::Otp => "otp",
            LifecyclePromptKind::DeviceCode => "device_code",
            LifecyclePromptKind::Operator => "operator",
        }
    }

    /// What the model sees: the challenge, the program, and the one way to
    /// resolve it (ask the user with the matching secure input type, then run
    /// the login again — the answer is delivered to the prompt, never typed
    /// by the model).
    pub(crate) fn into_result(self, elapsed_ms: u64) -> PrimitiveToolResult {
        let resolution = match self.kind {
            LifecyclePromptKind::Password => "ask the user with need_user_input (input_type password), then run the login again",
            LifecyclePromptKind::Otp => "ask the user with need_user_input (input_type otp), then run the login again",
            LifecyclePromptKind::Username => "ask the user for the sign-in identifier with need_user_input, then run the login again",
            LifecyclePromptKind::DeviceCode | LifecyclePromptKind::Operator => "this login needs the user at the terminal; ask them to complete it and run the status check again",
        };
        let held = match self.reason.as_deref() {
            Some(reason) => {
                format!(" the material this run held could not be delivered ({reason});")
            },
            None => String::new(),
        };
        let message = format!(
            "authentication_required: `{}` asked for a {} (prompt `{}`);{} {}.",
            self.program,
            self.kind_name(),
            self.marker,
            held,
            resolution
        );
        PrimitiveToolResult {
            success: false,
            parsed_json: Some(serde_json::json!({
                "status": "authentication_required",
                "kind": self.kind_name(),
                "program": self.program,
                "prompt": self.marker,
                "reason": self.reason,
                "resolution": resolution,
            })),
            stdout: String::new(),
            stderr: message,
            elapsed_ms,
            ..PrimitiveToolResult::default()
        }
    }
}

/// Answers the login hook's declared prompts from the run's material (P4).
///
/// The bridge keeps the tail of the PTY output and asks
/// `declared_login_prompt` after every chunk; a match is answered at most
/// once per run with the material the run holds under the standard keys
/// (`password`, `otp`, `login_identifier` — what a HITL answer is vaulted
/// under), a code reserved for this program and consumed as it is written,
/// and every delivered value joins the run's scrub set. A prompt with no
/// material — or a kind nobody can answer in-process — cancels the login and
/// records the challenge for a typed `authentication_required` result. The
/// model never supplies a prompt answer.
struct DeclaredPromptBridge<'a> {
    prompts: &'a [LifecyclePrompt],
    program: String,
    store: Option<&'a crate::magician_v2::secrets::SecretStore>,
    scope: Option<&'a str>,
    delivered: Option<&'a Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
    tail: Vec<u8>,
    answered: Vec<LifecyclePromptKind>,
    challenge: Option<LifecycleAuthenticationChallenge>,
}

/// How much PTY output a prompt may trail: the cursor's line and a little
/// history, never the whole transcript.
const PROMPT_TAIL_BYTES: usize = 512;

impl<'a> DeclaredPromptBridge<'a> {
    fn new(prompts: &'a [LifecyclePrompt], program: &str, exec_ctx: &'a PrimitiveExecCtx) -> Self {
        Self {
            prompts,
            program: program.to_string(),
            store: exec_ctx.secret_store.as_deref(),
            scope: exec_ctx.ephemeral_secret_scope_id.as_deref(),
            delivered: exec_ctx.delivered_secret_values.as_ref(),
            tail: Vec::with_capacity(PROMPT_TAIL_BYTES),
            answered: Vec::new(),
            challenge: None,
        }
    }

    fn challenge(&self) -> Option<LifecycleAuthenticationChallenge> {
        self.challenge.clone()
    }

    fn answered_kinds(&self) -> Vec<&'static str> {
        self.answered
            .iter()
            .map(|kind| match kind {
                LifecyclePromptKind::Username => "username",
                LifecyclePromptKind::Password => "password",
                LifecyclePromptKind::Otp => "otp",
                LifecyclePromptKind::DeviceCode => "device_code",
                LifecyclePromptKind::Operator => "operator",
            })
            .collect()
    }

    /// The standard key a HITL answer of this kind is vaulted under.
    ///
    /// KNOWN SHAPE LIMIT (deep review round 3): these are the single-value keys
    /// — the ones a chat answer uses (`turn_secrets::seeded_key`) and the ones an
    /// agentic ask uses when its parameter is named this way. A form BUNDLE
    /// (plan §3.3) vaults per field as `<input id>.<field id>`, and the bridge
    /// cannot read those: a CLI prompt is answered one value at a time, so the
    /// ask a CLI challenge raises is single-value by construction, and a bundle
    /// answer settles the login as a typed `authentication_required` rather than
    /// being mis-read. Resolving a key by KIND instead of by name would need the
    /// run's `resolved_input_sensitivity` map on `PrimitiveExecCtx`; it is not
    /// there today, and nothing silently substitutes a different credential —
    /// material bound to another destination is refused above.
    fn key_for(kind: LifecyclePromptKind) -> Option<&'static str> {
        match kind {
            LifecyclePromptKind::Password => Some("password"),
            LifecyclePromptKind::Otp => Some("otp"),
            LifecyclePromptKind::Username => Some("login_identifier"),
            LifecyclePromptKind::DeviceCode | LifecyclePromptKind::Operator => None,
        }
    }

    /// The material for a prompt, taken the way its kind demands: a plain
    /// scoped read for a password or identifier; for a code, one reservation
    /// bound to this program, consumed here because the write that follows
    /// is the submission.
    fn take(&mut self, kind: LifecyclePromptKind) -> Result<Zeroizing<String>, Option<String>> {
        let key = Self::key_for(kind).ok_or(None)?;
        let (store, scope) = (self.store.ok_or(None)?, self.scope.ok_or(None)?);
        // Material a challenge bound goes only to that program (P4).
        let bound = crate::magician_v2::secrets::sinks::bound_destination(store, scope, key);
        if !crate::magician_v2::secrets::sinks::destination_admits(
            bound.as_deref(),
            Some(self.program.as_str()),
        ) {
            return Err(Some(format!(
                "it is bound to {} and this program is {}",
                bound.unwrap_or_default(),
                self.program
            )));
        }
        let value = if store.one_time_state(scope, key).is_some() {
            let reservation = store
                .reserve_one_time(
                    scope,
                    key,
                    crate::magician_v2::secrets::OneTimeClaim {
                        operation: "cli:auth_login".to_string(),
                        destination: Some(self.program.clone()),
                        challenge_id: None,
                    },
                )
                .map_err(|error| Some(error.to_string()))?;
            let reservation_id = reservation
                .receipt
                .reservation_id
                .clone()
                .ok_or_else(|| Some("the reservation is not held".to_string()))?;
            store
                .consume_one_time(&reservation_id)
                .map_err(|error| Some(error.to_string()))?;
            reservation.value
        } else {
            Zeroizing::new(store.get_ephemeral_scoped(scope, key).ok_or(None)?)
        };
        if let Some(delivered) = self.delivered {
            if let Ok(mut delivered) = delivered.lock() {
                let delivery = format!("{key}@{}", delivered.len());
                delivered.insert(delivery, value.to_string());
            }
        }
        Ok(value)
    }

    fn pending_kind(kind: LifecyclePromptKind) -> CredentialLifecyclePendingKind {
        match kind {
            LifecyclePromptKind::Otp => CredentialLifecyclePendingKind::Otp,
            LifecyclePromptKind::Password | LifecyclePromptKind::Username => {
                CredentialLifecyclePendingKind::Password
            },
            LifecyclePromptKind::DeviceCode => CredentialLifecyclePendingKind::DeviceCode,
            LifecyclePromptKind::Operator => CredentialLifecyclePendingKind::OperatorRequired,
        }
    }
}

impl CredentialLifecycleInteractionBridge for DeclaredPromptBridge<'_> {
    fn on_output(
        &mut self,
        output: CredentialLifecycleSensitiveOutput<'_>,
    ) -> Result<CredentialLifecycleInteractionAction, CredentialLifecycleExecutionBindingError>
    {
        if self.prompts.is_empty() || self.challenge.is_some() {
            return Ok(CredentialLifecycleInteractionAction::Continue);
        }
        self.tail.extend_from_slice(output.bytes());
        if self.tail.len() > PROMPT_TAIL_BYTES {
            let excess = self.tail.len() - PROMPT_TAIL_BYTES;
            self.tail.drain(..excess);
        }
        let Some(prompt) = declared_login_prompt(self.prompts, &self.tail) else {
            return Ok(CredentialLifecycleInteractionAction::Continue);
        };
        if self.answered.contains(&prompt.kind) {
            // The program asked again: the material was wrong or spent. Never
            // answer a second time; the run asks the user afresh.
            self.challenge = Some(LifecycleAuthenticationChallenge {
                kind: prompt.kind,
                program: self.program.clone(),
                marker: prompt.marker.clone(),
                reason: Some(
                    "the program asked again after the material was delivered".to_string(),
                ),
            });
            return Ok(CredentialLifecycleInteractionAction::Cancel);
        }
        let kind = prompt.kind;
        let marker = prompt.marker.clone();
        let value = match self.take(kind) {
            Ok(value) => value,
            Err(reason) => {
                self.challenge = Some(LifecycleAuthenticationChallenge {
                    kind,
                    program: self.program.clone(),
                    marker,
                    reason,
                });
                return Ok(CredentialLifecycleInteractionAction::Cancel);
            },
        };
        // One allocation, sized for the newline: `to_vec()` then `push` grows
        // and frees the first buffer with the credential still in it, and only
        // the second one is wrapped in `Zeroizing`.
        let mut bytes = Vec::with_capacity(value.len() + 1);
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(b'\n');
        let input = tool_runtime_core::credential_lifecycle_execution::CredentialLifecycleSensitiveInput::new(bytes)?;
        self.answered.push(kind);
        self.tail.clear();
        Ok(CredentialLifecycleInteractionAction::ProvideInput {
            pending: Self::pending_kind(kind),
            input,
        })
    }
}

struct AdmittedAuthorizer {
    _admission: GovernedRouteAdmission,
}

impl GovernedExecutionAuthorizer for AdmittedAuthorizer {
    fn authorize(
        &mut self,
        request: &GovernedAuthorizationRequest<'_>,
    ) -> GovernedAuthorizationDecision {
        let policy = request.policy_floor();
        let receipt = (policy.approval != ApprovalClass::Ordinary)
            .then(|| format!("dispatch-{}", request.context().call_id().as_str()));
        match GovernedAuthorizationEvidence::new(
            request.request_digest(),
            GOVERNED_POLICY_REVISION,
            policy.approval,
            receipt,
            policy.required_grants.clone(),
            policy.resource_scopes.clone(),
            policy.required_resource_authorities.clone(),
        ) {
            Ok(evidence) => GovernedAuthorizationDecision::Approved(evidence),
            Err(_) => GovernedAuthorizationDecision::Unavailable,
        }
    }
}

struct ProductAuditSink<'a> {
    store: &'a SecretStore,
}

impl GovernedExecutionAuditSink for ProductAuditSink<'_> {
    fn record(
        &mut self,
        receipt: &GovernedExecutionAuditReceipt,
    ) -> Result<(), GovernedExecutionAuditError> {
        let detail = serde_json::to_string(receipt)
            .map_err(|_| GovernedExecutionAuditError::unavailable())?;
        self.store
            .try_audit_event(
                SecretAuditEvent::new("governed_execution")
                    .with_tool(receipt.context.capability_id())
                    .with_action(receipt.context.action_id())
                    .with_detail(detail),
            )
            .map_err(|_| GovernedExecutionAuditError::unavailable())
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        collections::HashMap,
        os::unix::fs::{symlink, PermissionsExt},
        sync::Arc,
        time::Duration,
    };

    use tempfile::TempDir;
    use tool_runtime_core::manifest_parser::parse_skill_runtime_package;

    use super::*;

    // ---- P4 Task 4.5: the CLI lane answers declared prompts from the run's
    // material and reports a typed challenge otherwise ------------------------

    fn prompt_bridge_fixture() -> (TempDir, Arc<SecretStore>, PrimitiveExecCtx) {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(SecretStore::new_empty(
            Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
            dir.path().join("secrets"),
        ));
        let scope = "execution:cli-1";
        store
            .register_ephemeral_bounded(
                scope,
                "password",
                "pw-cli-canary".to_string(),
                chrono::Utc::now().timestamp_millis() + 60_000,
            )
            .unwrap();
        store
            .register_one_time(
                scope,
                "otp",
                "042917".to_string(),
                chrono::Utc::now().timestamp_millis() + 60_000,
                crate::magician_v2::secrets::OneTimeBinding::default(),
            )
            .unwrap();
        let ctx = PrimitiveExecCtx::default_for_runtime()
            .with_secret_context(Some(store.clone()), Some(scope.to_string()))
            .with_delivery_tracking(Arc::default(), Arc::default(), Arc::default());
        (dir, store, ctx)
    }

    fn declared() -> Vec<LifecyclePrompt> {
        vec![
            LifecyclePrompt {
                kind: LifecyclePromptKind::Password,
                marker: "Password:".into(),
            },
            LifecyclePrompt {
                kind: LifecyclePromptKind::Otp,
                marker: "Code:".into(),
            },
        ]
    }

    fn feed(
        bridge: &mut DeclaredPromptBridge<'_>,
        text: &str,
    ) -> CredentialLifecycleInteractionAction {
        bridge
            .on_output(CredentialLifecycleSensitiveOutput::new(
                tool_runtime_core::credential_lifecycle_execution::CredentialLifecycleOutputChannel::Pty,
                text.as_bytes(),
            ))
            .unwrap()
    }

    #[test]
    fn declared_prompts_are_answered_from_the_runs_material_once_each() {
        let (_dir, store, ctx) = prompt_bridge_fixture();
        let prompts = declared();
        let mut bridge = DeclaredPromptBridge::new(&prompts, "/usr/bin/vendor-cli", &ctx);
        assert!(matches!(
            feed(&mut bridge, "Signing in to vendor\n"),
            CredentialLifecycleInteractionAction::Continue
        ));
        let CredentialLifecycleInteractionAction::ProvideInput { pending, input } =
            feed(&mut bridge, "Password: ")
        else {
            panic!("a declared password prompt is answered");
        };
        assert_eq!(pending, CredentialLifecyclePendingKind::Password);
        assert_eq!(
            input.with_bytes(|b| b.to_vec()),
            b"pw-cli-canary\n".to_vec()
        );
        let CredentialLifecycleInteractionAction::ProvideInput { pending, input } =
            feed(&mut bridge, "\x1b[32mCode:\x1b[0m ")
        else {
            panic!("a declared code prompt is answered");
        };
        assert_eq!(pending, CredentialLifecyclePendingKind::Otp);
        assert_eq!(input.with_bytes(|b| b.to_vec()), b"042917\n".to_vec());
        assert_eq!(
            store
                .one_time_state("execution:cli-1", "otp")
                .map(|s| s.state),
            Some(crate::magician_v2::secrets::OneTimeState::Consumed),
            "the write to the PTY is the submission"
        );
        let delivered = ctx
            .delivered_secret_values
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        assert!(delivered.values().any(|value| value == "pw-cli-canary"));
        assert!(delivered.values().any(|value| value == "042917"));
        drop(delivered);
        assert_eq!(bridge.answered_kinds(), vec!["password", "otp"]);
        // Asked again: the material was wrong or spent — never answered twice.
        assert!(matches!(
            feed(&mut bridge, "Code: "),
            CredentialLifecycleInteractionAction::Cancel
        ));
        let challenge = bridge.challenge().expect("a repeat prompt is a challenge");
        assert_eq!(challenge.kind, LifecyclePromptKind::Otp);
        assert_eq!(challenge.program, "/usr/bin/vendor-cli");
    }

    #[test]
    fn a_prompt_without_material_settles_as_a_typed_challenge() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(SecretStore::new_empty(
            Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
            dir.path().join("secrets"),
        ));
        let ctx = PrimitiveExecCtx::default_for_runtime()
            .with_secret_context(Some(store), Some("execution:cli-2".to_string()));
        let prompts = declared();
        let mut bridge = DeclaredPromptBridge::new(&prompts, "/usr/bin/vendor-cli", &ctx);
        assert!(matches!(
            feed(&mut bridge, "Password: "),
            CredentialLifecycleInteractionAction::Cancel
        ));
        let challenge = bridge.challenge().unwrap();
        assert!(challenge.reason.is_none(), "nothing was held");
        assert!(
            challenge.as_authentication_challenge().is_some(),
            "the run binds the next password ask to the program"
        );
        let result = challenge.into_result(7);
        assert!(!result.success);
        let json = result.parsed_json.unwrap();
        assert_eq!(json["status"], "authentication_required");
        assert_eq!(json["kind"], "password");
        assert_eq!(json["program"], "/usr/bin/vendor-cli");
        assert!(json["resolution"]
            .as_str()
            .unwrap()
            .contains("input_type password"));
        assert!(result.stderr.starts_with("authentication_required:"));
        // Arbitrary terminal text is not a prompt; nothing declared, nothing happens.
        let none: Vec<LifecyclePrompt> = Vec::new();
        let mut quiet = DeclaredPromptBridge::new(&none, "/usr/bin/vendor-cli", &ctx);
        assert!(matches!(
            feed(&mut quiet, "Password: "),
            CredentialLifecycleInteractionAction::Continue
        ));
        assert!(quiet.challenge().is_none());
    }

    const PACKS: [(&str, &str); 6] = [
        (
            "calendar",
            include_str!("../../../../../skillshub/calendar/SKILL.md"),
        ),
        (
            "gmail",
            include_str!("../../../../../skillshub/gmail/SKILL.md"),
        ),
        (
            "sheets",
            include_str!("../../../../../skillshub/sheets/SKILL.md"),
        ),
        (
            "presto-calendar",
            include_str!("../../../../../skillshub/presto-calendar/SKILL.md"),
        ),
        (
            "presto-gmail",
            include_str!("../../../../../skillshub/presto-gmail/SKILL.md"),
        ),
        (
            "presto-sheets",
            include_str!("../../../../../skillshub/presto-sheets/SKILL.md"),
        ),
    ];

    #[test]
    fn empty_governed_failure_preserves_terminal_diagnostic() {
        let result = governed_batch_result(
            &PrimitiveExecCtx::default_for_runtime(),
            GovernedExecutionTerminal::LaunchRejected,
            None,
            &[],
            &[],
            7,
        );
        assert!(!result.success);
        assert!(result.stdout.is_empty());
        assert_eq!(result.stderr, "governed runtime ended with LaunchRejected");
        assert_eq!(result.elapsed_ms, 7);
    }

    #[test]
    fn workspace_path_authority_rejects_escape_symlinks_and_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("documents")).unwrap();
        fs::write(root.path().join("documents/input.docx"), b"document").unwrap();
        fs::write(outside.path().join("secret"), b"secret").unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();

        let input = bind_scoped_workspace_path(
            root.path(),
            "documents/input.docx",
            WorkspacePathAccess::ReadFile,
        )
        .unwrap();
        assert_eq!(
            input,
            fs::canonicalize(root.path().join("documents/input.docx")).unwrap()
        );
        assert!(bind_scoped_workspace_path(
            root.path(),
            "../secret",
            WorkspacePathAccess::ReadFile
        )
        .is_err());
        assert!(bind_scoped_workspace_path(
            root.path(),
            "documents//input.docx",
            WorkspacePathAccess::ReadFile,
        )
        .is_err());
        assert!(bind_scoped_workspace_path(
            root.path(),
            outside.path().join("secret").to_str().unwrap(),
            WorkspacePathAccess::ReadFile
        )
        .is_err());
        assert!(bind_scoped_workspace_path(
            root.path(),
            "linked/secret",
            WorkspacePathAccess::ReadFile
        )
        .is_err());
        assert!(bind_scoped_workspace_path(
            root.path(),
            "documents/input.docx",
            WorkspacePathAccess::CreateFile
        )
        .is_err());
        assert!(bind_scoped_workspace_path(
            root.path(),
            "documents/new.md",
            WorkspacePathAccess::CreateFile
        )
        .is_ok());
    }

    #[test]
    fn workspace_path_authority_is_rooted_in_workdirs_not_scope_state() {
        let scope = tempfile::tempdir().unwrap();
        fs::create_dir(scope.path().join("workdirs")).unwrap();
        fs::create_dir(scope.path().join("auth")).unwrap();
        fs::write(scope.path().join("auth/provider-token"), b"secret").unwrap();

        let root = resolve_workspace_file_root(scope.path()).unwrap();
        assert_eq!(
            root,
            fs::canonicalize(scope.path().join("workdirs")).unwrap()
        );
        assert!(bind_scoped_workspace_path(
            &root,
            "../auth/provider-token",
            WorkspacePathAccess::ReadFile,
        )
        .is_err());
    }

    #[test]
    fn brokered_workspace_path_is_exact_read_only_authority() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("downloaded-document");
        fs::write(&input, b"document").unwrap();
        let binding = BrokeredWorkspacePath {
            path: input.clone(),
            access: WorkspacePathAccess::ReadFile,
        };

        assert_eq!(
            bind_brokered_workspace_path(
                input.to_str().unwrap(),
                WorkspacePathAccess::ReadFile,
                &binding,
            )
            .unwrap(),
            fs::canonicalize(&input).unwrap()
        );
        assert!(bind_brokered_workspace_path(
            root.path().join("other").to_str().unwrap(),
            WorkspacePathAccess::ReadFile,
            &binding,
        )
        .is_err());
        assert!(bind_brokered_workspace_path(
            input.to_str().unwrap(),
            WorkspacePathAccess::CreateFile,
            &binding,
        )
        .is_err());
    }
    const TAVILY_PACK: (&str, &str) = (
        "news-search-via-tavily",
        include_str!("../../../../../skillshub/news-search-via-tavily/SKILL.md"),
    );
    const EXA_PACK: (&str, &str) = (
        "semantic-websearch-via-exa",
        include_str!("../../../../../skillshub/semantic-websearch-via-exa/SKILL.md"),
    );
    const OPENAI_WEBSEARCH_PACK: (&str, &str) = (
        "websearch-via-openai",
        include_str!("../../../../../skillshub/websearch-via-openai/SKILL.md"),
    );
    const CLAUDE_WEBSEARCH_PACK: (&str, &str) = (
        "websearch-via-claude",
        include_str!("../../../../../skillshub/websearch-via-claude/SKILL.md"),
    );
    const KLIPY_GIF_SEARCH_PACK: (&str, &str) = (
        "gif-search-via-klipy",
        include_str!("../../../../../skillshub/gif-search-via-klipy/SKILL.md"),
    );
    const IMGFLIP_MEME_PACK: (&str, &str) = (
        "meme-generation-via-imgflip",
        include_str!("../../../../../skillshub/meme-generation-via-imgflip/SKILL.md"),
    );
    const OPENAI_DEEP_RESEARCH_PACK: (&str, &str) = (
        "deep-research-with-openai",
        include_str!("../../../../../skillshub/deep-research-with-openai/SKILL.md"),
    );
    const CLAUDE_DEEP_RESEARCH_PACK: (&str, &str) = (
        "deep-research-with-claude",
        include_str!("../../../../../skillshub/deep-research-with-claude/SKILL.md"),
    );
    const GITHUB_SEARCH_PACK: (&str, &str) = (
        "github-search",
        include_str!("../../../../../skillshub/github-search/SKILL.md"),
    );
    const IMAGE_GENERATION_PACK: (&str, &str) = (
        "image-generation",
        include_str!("../../../../../skillshub/image-generation/SKILL.md"),
    );
    const VEO_GENERATION_PACK: (&str, &str) = (
        "video-generation-via-veo",
        include_str!("../../../../../skillshub/video-generation-via-veo/SKILL.md"),
    );
    const METABASE_PACK: (&str, &str) = (
        "metabase",
        include_str!("../../../../../skillshub/metabase/SKILL.md"),
    );
    const TELEGRAM_PACK: (&str, &str) = (
        "telegram",
        include_str!("../../../../../skillshub/telegram/SKILL.md"),
    );
    const CLI_OWNED_BATCH_PACK: (&str, &str) = (
        "cli-owned-batch",
        r#"---
name: "cli-owned-batch"
description: "Fixture CLI-owned batch pack for workspace authority tests."
metadata:
  magician:
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [cli-owned-batch]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        working_directory: {mode: workspace}
        limits:
          timeout_secs: 3600
          stdout_bytes: 67108864
          stderr_bytes: 8388608
      auth:
        kind: cli_profile
        requirement: required
        provider: fixture-provider
        profile_selection: {mode: implicit}
        storage: {kind: cli_owned}
      policy_floor:
        approval: conditional_external_side_effect
        resource_scopes: [workspace]
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        run:
          description: Run one fixture task in the governed workspace.
          fixed_args: [run]
          suffix_args: [--format, json]
          parameters:
            prompt:
              type: string
              description: Exact fixture prompt.
              required: true
              min_length: 1
              max_length: 4096
          mappings:
            - {type: positional, parameter: prompt}
          timeout_secs: 3600
---
"#,
    );
    const MINIMAX_VISION_PACK: (&str, &str) = (
        "analyze-image-via-minimax",
        include_str!("../../../../../skillshub/analyze-image-via-minimax/SKILL.md"),
    );
    const CSVKIT_PACK: (&str, &str) = (
        "csvkit",
        include_str!("../../../../../skillshub/csvkit/SKILL.md"),
    );
    const HIGGSFIELD_PACK: (&str, &str) = (
        "higgsfield",
        include_str!("../../../../../skillshub/higgsfield/SKILL.md"),
    );

    #[test]
    fn reviewed_executable_resolution_is_package_generic() {
        let root = tempfile::tempdir().expect("temp root");
        let package = root.path().join("node_modules/@vendor/tool");
        let public_bin = root.path().join("node_modules/.bin");
        let standalone_bin = package.join("node_modules/.bin_real");
        fs::create_dir_all(&public_bin).expect("public bin");
        fs::create_dir_all(&standalone_bin).expect("standalone bin");
        fs::create_dir_all(package.join("dist")).expect("package dist");
        fs::write(
            package.join("package.json"),
            b"{\"name\":\"@vendor/tool\"}\n",
        )
        .expect("package metadata");
        fs::write(package.join("dist/launch.js"), b"require('./runtime')\n").expect("launcher");
        symlink(
            "../@vendor/tool/dist/launch.js",
            public_bin.join("fixture-cli"),
        )
        .expect("package link");
        let standalone = standalone_bin.join("fixture-cli");
        fs::write(&standalone, b"standalone executable").expect("standalone executable");
        fs::set_permissions(&standalone, fs::Permissions::from_mode(0o700))
            .expect("standalone permissions");

        assert_eq!(
            reviewed_executable_directory(
                &public_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            Some(fs::canonicalize(standalone_bin).expect("canonical standalone bin"))
        );
    }

    #[test]
    fn reviewed_executable_resolution_rejects_nonstandalone_package_launchers() {
        let root = tempfile::tempdir().expect("temp root");
        let package = root.path().join("node_modules/@vendor/tool");
        let public_bin = root.path().join("node_modules/.bin");
        fs::create_dir_all(&package).expect("package");
        fs::create_dir_all(&public_bin).expect("public bin");
        fs::write(package.join("launch.js"), b"require('./runtime')\n").expect("launcher");
        symlink("../@vendor/tool/launch.js", public_bin.join("fixture-cli")).expect("package link");

        assert_eq!(
            reviewed_executable_directory(
                &public_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            None
        );
    }

    #[test]
    fn reviewed_executable_resolution_finds_one_bounded_native_platform_binary() {
        let root = tempfile::tempdir().expect("temp root");
        let package = root.path().join("node_modules/@vendor/tool");
        let public_bin = root.path().join("node_modules/.bin");
        let native_bin = package.join("node_modules/@vendor/tool-darwin/vendor/arm64/bin");
        fs::create_dir_all(&public_bin).expect("public bin");
        fs::create_dir_all(&native_bin).expect("native bin");
        fs::write(
            package.join("package.json"),
            b"{\"name\":\"@vendor/tool\"}\n",
        )
        .expect("package metadata");
        fs::write(package.join("launch.js"), b"#!/usr/bin/env node\n").expect("launcher");
        symlink("../@vendor/tool/launch.js", public_bin.join("fixture-cli")).expect("package link");
        fs::write(
            native_bin.join("fixture-cli"),
            b"\xcf\xfa\xed\xfe standalone fixture",
        )
        .expect("native executable");

        assert_eq!(
            reviewed_executable_directory(
                &public_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            Some(fs::canonicalize(native_bin).expect("canonical native bin"))
        );
    }

    #[test]
    fn reviewed_executable_resolution_rejects_ambiguous_native_platform_binaries() {
        let root = tempfile::tempdir().expect("temp root");
        let package = root.path().join("node_modules/@vendor/tool");
        let public_bin = root.path().join("node_modules/.bin");
        fs::create_dir_all(&public_bin).expect("public bin");
        fs::create_dir_all(&package).expect("package");
        fs::write(
            package.join("package.json"),
            b"{\"name\":\"@vendor/tool\"}\n",
        )
        .expect("package metadata");
        fs::write(package.join("launch.js"), b"#!/usr/bin/env node\n").expect("launcher");
        symlink("../@vendor/tool/launch.js", public_bin.join("fixture-cli")).expect("package link");
        for platform in ["darwin", "linux"] {
            let native_bin = package.join(format!("vendor/{platform}/bin"));
            fs::create_dir_all(&native_bin).expect("native bin");
            fs::write(
                native_bin.join("fixture-cli"),
                b"\xcf\xfa\xed\xfe standalone fixture",
            )
            .expect("native executable");
        }

        assert_eq!(
            reviewed_executable_directory(
                &public_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            None
        );
    }

    /// The scoped-install shape: `<scope>/skills/<name>/bin/<name>` is a symlink
    /// onto a package-source entry point that is a `#!` script rather than a
    /// native binary — a vendored venv console script, or one of the committed
    /// `bin/` wrappers. Placing that identical file directly in the directory has
    /// always resolved; reaching it through the link must resolve too, because
    /// `ResolvedExecutable::resolve` canonicalizes the link and snapshots the
    /// same bytes either way.
    #[test]
    fn reviewed_executable_resolution_admits_a_linked_script_entry_point() {
        let root = tempfile::tempdir().expect("temp root");
        let source = root.path().join("package-source");
        let scoped_bin = root.path().join("scope/skills/fixture/bin");
        fs::create_dir_all(&source).expect("package source");
        fs::create_dir_all(&scoped_bin).expect("scoped bin");
        let entry_point = source.join("fixture-cli");
        fs::write(&entry_point, b"#!/bin/sh\nexec /usr/bin/true \"$@\"\n").expect("entry point");
        fs::set_permissions(&entry_point, fs::Permissions::from_mode(0o755))
            .expect("entry point permissions");
        symlink(&entry_point, scoped_bin.join("fixture-cli")).expect("scoped link");

        assert_eq!(
            reviewed_executable_directory(
                &scoped_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            Some(fs::canonicalize(&scoped_bin).expect("canonical scoped bin"))
        );
    }

    /// The relaxation above is scoped to entry points that live outside a package
    /// tree. Inside one, a launcher resolves its modules next to itself and cannot
    /// survive relocation into the single-file snapshot, so the shebang alone must
    /// not admit it — the `.bin_real` and native-artifact rules remain the only
    /// admissible answers there.
    #[test]
    fn reviewed_executable_resolution_still_refuses_a_linked_package_script_launcher() {
        let root = tempfile::tempdir().expect("temp root");
        let package = root.path().join("node_modules/@vendor/tool");
        let public_bin = root.path().join("node_modules/.bin");
        fs::create_dir_all(&package).expect("package");
        fs::create_dir_all(&public_bin).expect("public bin");
        fs::write(
            package.join("package.json"),
            b"{\"name\":\"@vendor/tool\"}\n",
        )
        .expect("package metadata");
        fs::write(
            package.join("launch.js"),
            b"#!/usr/bin/env node\nrequire('./runtime')\n",
        )
        .expect("launcher");
        symlink("../@vendor/tool/launch.js", public_bin.join("fixture-cli")).expect("package link");

        assert_eq!(
            reviewed_executable_directory(
                &public_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            None
        );
    }

    fn compiled_catalog(pack: (&str, &str)) -> CompiledActionCatalog {
        let package = parse_skill_runtime_package(pack.1)
            .expect("runtime package parse")
            .expect("runtime package");
        let validated =
            validate_skill_runtime_contract(&package.contract).expect("validated contract");
        tool_runtime_core::action_overrides::compile_typed_action_overrides(
            pack.0,
            validated,
            package.actions.as_ref().expect("actions"),
        )
        .expect("compiled actions")
    }

    /// The two roles read off the shipped manifests. A MiniMax package declares
    /// `mmx` so its wrapper can find the vendored CLI on PATH, but names the
    /// wrapper as its entry point and never overrides an action onto `mmx`, so
    /// `mmx` is a companion and the snapshot-safety gate must not reach it.
    #[test]
    fn shipped_manifests_classify_only_action_executables_as_snapshot_targets() {
        let vision = compiled_catalog(MINIMAX_VISION_PACK);
        let targets = snapshot_target_executables(&vision);
        assert!(
            targets.contains("minimax-vision"),
            "the declared entry point is a snapshot target"
        );
        assert!(
            !targets.contains("mmx"),
            "no MiniMax action names mmx, so it is only a PATH companion"
        );
        // The requirement is real: it must still be resolved into the PATH.
        let package = parse_skill_runtime_package(MINIMAX_VISION_PACK.1)
            .expect("runtime package parse")
            .expect("runtime package");
        assert!(
            package.contract.requires.bins.contains("mmx"),
            "mmx stays a declared requirement"
        );
    }

    /// The contrasting shape: csvkit routes each action onto its own binary, so
    /// every one of those binaries can reach the snapshot and every one keeps
    /// the full snapshot-safety gate. The relaxation must not touch them.
    #[test]
    fn per_action_executable_overrides_remain_snapshot_targets() {
        let csvkit = compiled_catalog(CSVKIT_PACK);
        let targets = snapshot_target_executables(&csvkit);
        for executable in ["in2csv", "csvstat", "csvsql", "csvgrep", "csvjson"] {
            assert!(
                targets.contains(executable),
                "{executable} is named by a csvkit action and must stay a snapshot target"
            );
        }
    }

    /// The `mmx` shape: a JavaScript launcher inside a package tree, reached
    /// through the public `.bin` link. It genuinely cannot survive the snapshot
    /// -- relocating it breaks the module resolution it does next to itself --
    /// so as a snapshot target it must stay refused. But it is only ever a
    /// companion: the manifests that require it name a different binary as
    /// their entry point and never override an action onto it, so the child
    /// execs it in place through PATH and the copy never happens. Refusing the
    /// directory on snapshot grounds is what made every `mmx`-backed package
    /// fail to dispatch at all.
    #[test]
    fn path_companion_admits_the_package_launcher_a_snapshot_target_refuses() {
        let root = tempfile::tempdir().expect("temp root");
        let package = root.path().join("node_modules/vendor-cli");
        let public_bin = root.path().join("node_modules/.bin");
        fs::create_dir_all(package.join("dist")).expect("package dist");
        fs::create_dir_all(&public_bin).expect("public bin");
        fs::write(package.join("package.json"), b"{\"name\":\"vendor-cli\"}\n")
            .expect("package metadata");
        fs::write(
            package.join("dist/cli.mjs"),
            b"#!/usr/bin/env node\nimport 'undici'\n",
        )
        .expect("launcher");
        symlink("../vendor-cli/dist/cli.mjs", public_bin.join("fixture-cli"))
            .expect("package link");

        assert_eq!(
            reviewed_executable_directory(
                &public_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            None,
            "a package launcher cannot survive relocation into the snapshot"
        );
        assert_eq!(
            reviewed_executable_directory(
                &public_bin,
                "fixture-cli",
                ExecutableRole::PathCompanion
            ),
            Some(fs::canonicalize(&public_bin).expect("canonical public bin")),
            "a companion is execed in place and never snapshotted"
        );
    }

    /// The companion role relaxes only the snapshot-survival question. A
    /// requirement that resolves to nothing at all still fails closed, so a
    /// dangling link cannot put a directory on the child's PATH.
    #[test]
    fn path_companion_still_fails_closed_on_an_unresolvable_requirement() {
        let root = tempfile::tempdir().expect("temp root");
        let public_bin = root.path().join("node_modules/.bin");
        fs::create_dir_all(&public_bin).expect("public bin");
        symlink(
            root.path().join("node_modules/vendor-cli/dist/absent.mjs"),
            public_bin.join("fixture-cli"),
        )
        .expect("dangling link");

        for role in [
            ExecutableRole::SnapshotTarget,
            ExecutableRole::PathCompanion,
        ] {
            assert_eq!(
                reviewed_executable_directory(&public_bin, "fixture-cli", role),
                None,
                "{role:?} must not admit a directory for an unresolvable requirement"
            );
        }
    }

    /// Containment is a property of where the directory may come from, not of
    /// what will be done with the file, so the companion role does not loosen
    /// it: a `.bin` entry that escapes the install authority is still refused
    /// the escaped package's own artifact directory.
    #[test]
    fn path_companion_does_not_relax_install_authority_containment() {
        let root = tempfile::tempdir().expect("temp root");
        let public_bin = root.path().join("node_modules/.bin");
        let escaped_package = root.path().join("escaped/tool");
        fs::create_dir_all(&public_bin).expect("public bin");
        fs::create_dir_all(escaped_package.join("node_modules/.bin_real"))
            .expect("escaped standalone bin");
        fs::write(
            escaped_package.join("package.json"),
            b"{\"name\":\"escaped\"}\n",
        )
        .expect("escaped package metadata");
        fs::write(
            escaped_package.join("launch.js"),
            b"#!/usr/bin/env node\nrequire('./runtime')\n",
        )
        .expect("escaped launcher");
        fs::write(
            escaped_package.join("node_modules/.bin_real/fixture-cli"),
            b"standalone executable",
        )
        .expect("escaped standalone executable");
        symlink(
            "../../escaped/tool/launch.js",
            public_bin.join("fixture-cli"),
        )
        .expect("escaped package link");

        // The escaped package's own `.bin_real` is never the answer for either
        // role; a companion resolves to the reviewed install authority it was
        // found in, which is the directory already on the child's PATH.
        let escaped_standalone = fs::canonicalize(escaped_package.join("node_modules/.bin_real"))
            .expect("canonical escaped standalone bin");
        for role in [
            ExecutableRole::SnapshotTarget,
            ExecutableRole::PathCompanion,
        ] {
            assert_ne!(
                reviewed_executable_directory(&public_bin, "fixture-cli", role),
                Some(escaped_standalone.clone()),
                "{role:?} must not follow a link out of the install authority"
            );
        }
    }

    /// One PATH build over a package that names a companion this host does not
    /// have, and over a package whose entry point is the missing one.
    ///
    /// The two roles must answer differently, and the difference is the whole
    /// point of declaring a companion at all. Before this, one unresolvable
    /// name failed the entire PATH build, so declaring the companion of an
    /// optional engine (`ocr`'s `tesseract`) took every other engine of that
    /// skill down with it on a host that has only the VLM CLIs — a declaration
    /// that left the package strictly worse off than saying nothing.
    ///
    /// The absent name is spelled so that no host can accidentally satisfy it:
    /// this resolution ends with a `which::which` probe of the real PATH, and a
    /// plausible name would make the assertion pass or fail on what the
    /// developer's machine happens to have installed.
    fn companion_probe_package(entrypoint: &str) -> String {
        format!(
            r#"---
name: companion-probe
description: Companion probe.
metadata:
  magician:
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [probe-cli, magician-canary-absent-companion]
        entrypoint: {entrypoint}
      runtime:
        protocol: cli
        command_prefix: []
        limits:
          timeout_secs: 30
      auth:
        kind: none
        requirement: none
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        run:
          description: Run the probe.
---
"#
        )
    }

    fn companion_probe_directories(
        entrypoint: &str,
        executable_directory: PathBuf,
        empty_roots: &Path,
    ) -> Result<Vec<PathBuf>, String> {
        let source = companion_probe_package(entrypoint);
        let package = parse_skill_runtime_package(&source)
            .expect("probe package parses")
            .expect("probe package");
        let validated = validate_skill_runtime_contract(&package.contract).expect("probe contract");
        let actions = tool_runtime_core::action_overrides::compile_typed_action_overrides(
            "companion-probe",
            validated,
            package.actions.as_ref().expect("probe actions"),
        )
        .expect("probe actions compile");
        let runtime = GovernedRuntimeImplementation {
            package,
            actions: Some(actions),
            executable_directory: Some(executable_directory),
            legacy_secret_environment: None,
        };
        let scope_root = empty_roots.join("scope");
        let scope_paths = CapabilityScopePaths {
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            capabilities_root: scope_root.clone(),
            bots_root: scope_root.join("bots"),
            auth_root: scope_root.join("auth"),
            workdirs_root: scope_root.join("workdirs"),
            home_root: scope_root,
            // Deliberately absent directories: the probe must not borrow a
            // real dependency root and resolve the companion by accident.
            node_modules_bin: empty_roots.join("node_modules/.bin"),
            node_bin: empty_roots.join("node/bin"),
            venv_bin: empty_roots.join("venv/bin"),
        };
        governed_executable_directories(
            &PrimitiveExecCtx::default_for_runtime(),
            &runtime,
            &scope_paths,
        )
    }

    #[test]
    fn an_absent_path_companion_shortens_the_path_instead_of_killing_the_skill() {
        let root = tempfile::tempdir().expect("temp root");
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).expect("probe bin");
        fs::write(bin.join("probe-cli"), b"#!/bin/sh\nexit 0\n").expect("probe entry point");

        let directories = companion_probe_directories("probe-cli", bin.clone(), root.path())
            .expect("an absent companion must not fail the PATH build");
        assert_eq!(
            directories,
            vec![fs::canonicalize(&bin).expect("canonical probe bin")],
            "the PATH carries the entry point's directory and nothing invented for the \
             absent companion"
        );
    }

    #[test]
    fn an_absent_entry_point_still_fails_the_path_build() {
        let root = tempfile::tempdir().expect("temp root");
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).expect("probe bin");
        fs::write(bin.join("probe-cli"), b"#!/bin/sh\nexit 0\n").expect("probe entry point");

        // Same package, same host, only the role of the missing name changes:
        // it is now execution authority, so there is nothing to launch.
        assert!(
            companion_probe_directories("magician-canary-absent-companion", bin, root.path())
                .is_err(),
            "an unresolvable snapshot target must fail closed"
        );
    }

    /// A linked payload that is neither a native image nor a `#!` entry point
    /// names no interpreter and has no loadable format, so it stays refused.
    #[test]
    fn reviewed_executable_resolution_refuses_a_linked_payload_with_no_entry_point() {
        let root = tempfile::tempdir().expect("temp root");
        let source = root.path().join("package-source");
        let scoped_bin = root.path().join("scope/skills/fixture/bin");
        fs::create_dir_all(&source).expect("package source");
        fs::create_dir_all(&scoped_bin).expect("scoped bin");
        let payload = source.join("fixture-cli");
        fs::write(&payload, b"plain payload, no interpreter\n").expect("payload");
        symlink(&payload, scoped_bin.join("fixture-cli")).expect("scoped link");

        assert_eq!(
            reviewed_executable_directory(
                &scoped_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            None
        );
    }

    /// The `/usr/bin`-backed skills resolve the host tool directly.
    ///
    /// These three used to ship a `#!/bin/sh` wrapper that re-execed the system
    /// binary at its absolute path, because the governed runtime always
    /// launched a private copy and macOS SIGKILLs a copy of a platform binary.
    /// `ResolvedExecutable::snapshot` now launches a sealed-system-volume
    /// binary from its own path, so the wrappers were retired -- keeping both a
    /// general fix and a special case for the same defect is how the special
    /// case comes to look load-bearing.
    ///
    /// What must hold now is that the plain system path still resolves: the
    /// entry is a regular file, so `reviewed_executable_directory` admits its
    /// directory on the first check, with no link or package rule involved.
    #[cfg(target_os = "macos")]
    #[test]
    fn system_tool_skills_resolve_the_host_binary_without_a_wrapper() {
        let skillshub = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("magician/ has a parent")
            .join("skillshub");
        for skill in ["awk", "jq", "sed"] {
            assert!(
                !skillshub.join(skill).join("bin").exists(),
                "{skill} must not reintroduce a wrapper now that the runtime \
                 executes sealed system binaries in place"
            );

            let system_bin = Path::new("/usr/bin");
            if !system_bin.join(skill).is_file() {
                continue;
            }
            assert_eq!(
                reviewed_executable_directory(system_bin, skill, ExecutableRole::SnapshotTarget),
                Some(system_bin.to_path_buf()),
                "{skill} must resolve directly from the system directory"
            );
        }
    }

    #[test]
    fn reviewed_native_package_discovery_fails_closed_at_the_depth_budget() {
        let root = tempfile::tempdir().expect("temp root");
        let package = root.path().join("node_modules/@vendor/tool");
        let launcher = package.join("launch.js");
        let native_bin = package.join("vendor/darwin/bin");
        fs::create_dir_all(&native_bin).expect("native bin");
        fs::write(&launcher, b"#!/usr/bin/env node\n").expect("launcher");
        fs::write(
            native_bin.join("fixture-cli"),
            b"\xcf\xfa\xed\xfe standalone fixture",
        )
        .expect("native executable");
        let mut deep = package.join("unrelated");
        for depth in 0..=10 {
            deep = deep.join(format!("d{depth}"));
            fs::create_dir_all(&deep).expect("deep package directory");
        }

        assert_eq!(
            reviewed_native_package_executable_directory(&package, "fixture-cli", &launcher,),
            None,
            "a shallow candidate cannot hide an uninspected deeper candidate"
        );
    }

    #[test]
    fn reviewed_executable_resolution_rejects_package_links_outside_the_install_authority() {
        let root = tempfile::tempdir().expect("temp root");
        let public_bin = root.path().join("node_modules/.bin");
        let escaped_package = root.path().join("escaped/tool");
        fs::create_dir_all(&public_bin).expect("public bin");
        fs::create_dir_all(escaped_package.join("node_modules/.bin_real"))
            .expect("escaped standalone bin");
        fs::write(
            escaped_package.join("package.json"),
            b"{\"name\":\"escaped\"}\n",
        )
        .expect("escaped package metadata");
        fs::write(escaped_package.join("launch.js"), b"require('./runtime')\n")
            .expect("escaped launcher");
        fs::write(
            escaped_package.join("node_modules/.bin_real/fixture-cli"),
            b"standalone executable",
        )
        .expect("escaped standalone executable");
        symlink(
            "../../escaped/tool/launch.js",
            public_bin.join("fixture-cli"),
        )
        .expect("escaped package link");

        assert_eq!(
            reviewed_executable_directory(
                &public_bin,
                "fixture-cli",
                ExecutableRole::SnapshotTarget
            ),
            None
        );
    }

    struct Fixture {
        _root: TempDir,
        storage: PathBuf,
        bin: PathBuf,
        scope_paths: CapabilityScopePaths,
        secret_store: Arc<SecretStore>,
        secret_resolver: Arc<crate::magician_v2::secrets::SecretStoreResolver>,
        audit_path: PathBuf,
        legacy_env: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temp root");
            // The shared Cargo TMPDIR may live below an external-volume alias on
            // macOS. Production correctly rejects aliased scope roots, so the
            // fixture must use the canonical spelling rather than weakening the
            // authority check.
            let canonical_root = fs::canonicalize(root.path()).expect("canonical temp root");
            let storage = canonical_root.join("runtime");
            let scope_root = storage.join("scopes/owner/default");
            let auth_root = scope_root.join("auth");
            let bin = canonical_root.join("bin");
            let audit_root = canonical_root.join("secrets");
            let audit_path = audit_root.join("secret_audit.jsonl");
            let secret_store = Arc::new(SecretStore::new_empty(
                Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                audit_root,
            ));
            let secret_resolver = Arc::new(
                crate::magician_v2::secrets::SecretStoreResolver::new_with_capabilities(
                    Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                    canonical_root.join("credential-vault"),
                    crate::magician_v2::secrets::SecretRuntimeCapabilities::fully_available(
                        "in_memory",
                    ),
                ),
            );
            fs::create_dir_all(&auth_root).unwrap();
            fs::create_dir_all(&bin).unwrap();
            for alias in ["work", "personal", "presto"] {
                fs::create_dir_all(auth_root.join(format!("gws-{alias}/cloudsdk"))).unwrap();
            }
            for path in [
                storage.join("scopes"),
                storage.join("scopes/owner"),
                scope_root.clone(),
            ] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            fs::set_permissions(&auth_root, fs::Permissions::from_mode(0o700)).unwrap();
            for alias in ["work", "personal", "presto"] {
                fs::set_permissions(
                    auth_root.join(format!("gws-{alias}")),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
                fs::set_permissions(
                    auth_root.join(format!("gws-{alias}/cloudsdk")),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
            let executable = bin.join("gws");
            fs::write(
                &executable,
                r#"#!/bin/sh
case "$GOOGLE_WORKSPACE_CLI_CONFIG_DIR" in
  *gws-personal) user="personal@example.com" ;;
  *gws-presto) user="reach.magican@gmail.com" ;;
  *) user="work@example.com" ;;
esac
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  printf '{"user":"%s"}\n' "$user"
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "login" ]; then
  printf '{"login":true,"user":"%s"}\n' "$user"
  exit 0
fi
if [ -f "$GOOGLE_WORKSPACE_CLI_CONFIG_DIR/slow" ]; then
  sleep 10
fi
touch "$GOOGLE_WORKSPACE_CLI_CONFIG_DIR/action-ran"
if [ "$CLOUDSDK_CONFIG" = "$GOOGLE_WORKSPACE_CLI_CONFIG_DIR/cloudsdk" ]; then cloud_ok=true; else cloud_ok=false; fi
printf '{"ok":true,"cloud_ok":%s,"first":"%s"}\n' "$cloud_ok" "$1"
"#,
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            let coding_workspace = canonical_root.join("coding-workspace");
            fs::create_dir(&coding_workspace).unwrap();
            fs::set_permissions(&coding_workspace, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(coding_workspace.join(".cli-owned-workspace"), b"fixture\n").unwrap();
            let cli_owned = bin.join("cli-owned-batch");
            fs::write(
                &cli_owned,
                r#"#!/bin/sh
if [ ! -f .cli-owned-workspace ]; then printf 'wrong cwd\n' >&2; exit 8; fi
if [ -z "$HOME" ]; then printf 'HOME missing\n' >&2; exit 9; fi
if [ "$#" -ne 4 ] || [ "$1" != "run" ] || [ "$2" != "make drop-in" ] || [ "$3" != "--format" ] || [ "$4" != "json" ]; then
  printf 'argv mismatch\n' >&2
  exit 7
fi
touch .cli-owned-ran
printf '{"ok":true,"session":"cli-owned"}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&cli_owned, fs::Permissions::from_mode(0o700)).unwrap();
            let retired_pty = bin.join("retired-pty");
            fs::write(
                &retired_pty,
                r#"#!/bin/sh
touch .retired-pty-ran
printf '{"ok":true}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&retired_pty, fs::Permissions::from_mode(0o700)).unwrap();
            let higgsfield = bin.join("higgsfield");
            fs::write(
                &higgsfield,
                r#"#!/bin/sh
script_dir=${0%/*}
printf '%s\n' "$*" >> "$script_dir/.higgsfield-invocations"
if [ "$*" = "model list --json" ]; then
  printf '{"models":[]}\n'
  exit 0
fi
if [ "$*" = "workspace status --json" ]; then
  printf '{"workspace":"fixture"}\n'
  exit 0
fi
printf 'unexpected argv\n' >&2
exit 7
"#,
            )
            .unwrap();
            fs::set_permissions(&higgsfield, fs::Permissions::from_mode(0o700)).unwrap();
            let tavily_executable = bin.join("tavily-search");
            fs::write(
                &tavily_executable,
                r#"#!/bin/sh
if [ "$TAVILY_API_KEY" != "canary-tavily-secret" ]; then
  printf 'credential missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"auto_parameters":false,"exact_match":false,"include_answer":"false","include_raw_content":"false","max_results":3,"query":"latest-news","search_depth":"advanced","topic":"general"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"query":"latest-news","results":[],"cost_microunits":2000000}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&tavily_executable, fs::Permissions::from_mode(0o700)).unwrap();
            let telegram_executable = bin.join("telegram-bot-adapter");
            fs::write(
                &telegram_executable,
                r#"#!/bin/sh
if [ "$TELEGRAM_TOKEN" != "123456:canary-telegram-secret" ]; then
  printf 'credential missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"data":"{\"chat_id\":7,\"text\":\"hello\"}","method":"sendMessage"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"ok":true,"result":{"message_id":11}}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&telegram_executable, fs::Permissions::from_mode(0o700)).unwrap();
            let exa_executable = bin.join("exa-search");
            fs::write(
                &exa_executable,
                r#"#!/bin/sh
if [ "$EXA_API_KEY" != "canary-exa-secret" ]; then
  printf 'credential missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"contents":false,"highlights":true,"num_results":2,"query":"semantic-launch","type":"deep"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"query":"semantic-launch","search_type":"deep","results":[]}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&exa_executable, fs::Permissions::from_mode(0o700)).unwrap();
            let openai_executable = bin.join("openai-websearch");
            fs::write(
                &openai_executable,
                r#"#!/bin/sh
if [ "$OPENAI_API_KEY" != "canary-openai-secret" ]; then
  printf 'credential missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"allowed_domains":"example.com,openai.com","model":"future-model","query":"cited-answer"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"answer":"bounded","sources":[],"model":"future-model","usage":{"total_tokens":12}}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&openai_executable, fs::Permissions::from_mode(0o700)).unwrap();
            let claude_executable = bin.join("claude-websearch");
            fs::write(
                &claude_executable,
                r#"#!/bin/sh
if [ "$ANTHROPIC_API_KEY" != "canary-anthropic-secret" ]; then
  printf 'credential missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"allowed_domains":"example.com/blog,research.example","max_searches":4,"model":"future-claude","query":"contextual-answer"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"answer":"bounded","sources":[],"model":"future-claude","usage":{"web_search_requests":2}}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&claude_executable, fs::Permissions::from_mode(0o700)).unwrap();
            let klipy_executable = bin.join("klipy-gif-search");
            fs::write(
                &klipy_executable,
                r#"#!/bin/sh
if [ "$KLIPY_API_KEY" != "canary-klipy-secret" ]; then
  printf 'credential missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"content_filter":"pg-13","limit":"7","query":"celebrate"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"query":"celebrate","count":1,"gifs":[{"url":"https://cdn.example/g.gif"}]}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&klipy_executable, fs::Permissions::from_mode(0o700)).unwrap();
            let imgflip_executable = bin.join("imgflip-meme");
            fs::write(
                &imgflip_executable,
                r#"#!/bin/sh
if [ "$IMGFLIP_USERNAME" != "canary-imgflip-user" ]; then
  printf 'username missing\n' >&2
  exit 9
fi
if [ "$IMGFLIP_PASSWORD" != "canary-imgflip-password" ]; then
  printf 'password missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"action":"caption","template_id":"181913649","template_name":"Drake","text0":"old-way","text1":"new-way"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"action":"caption","template_id":"181913649","url":"https://i.imgflip.com/result.jpg"}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&imgflip_executable, fs::Permissions::from_mode(0o700)).unwrap();
            let openai_deep_research = bin.join("openai-deep-research");
            fs::write(
                &openai_deep_research,
                r#"#!/bin/sh
if [ "$OPENAI_API_KEY" != "canary-openai-deep-secret" ]; then
  printf 'credential missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"max_poll_attempts":2,"max_tool_calls":9,"model":"research-model","poll_interval_secs":1,"query":"investigate"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"answer":"deep-openai","sources":[],"poll_attempts":2}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&openai_deep_research, fs::Permissions::from_mode(0o700)).unwrap();
            let claude_deep_research = bin.join("claude-deep-research");
            fs::write(
                &claude_deep_research,
                r#"#!/bin/sh
if [ "$ANTHROPIC_API_KEY" != "canary-claude-deep-secret" ]; then
  printf 'credential missing\n' >&2
  exit 9
fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"allowed_domains":"example.com","max_fetches":2,"max_searches":3,"model":"claude-research","query":"investigate-claude","thinking_budget":2048}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"answer":"deep-claude","sources":[],"thinking_preview":"bounded"}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&claude_deep_research, fs::Permissions::from_mode(0o700)).unwrap();
            let github_search = bin.join("github-search");
            fs::write(
                &github_search,
                r#"#!/bin/sh
if [ -n "$GITHUB_TOKEN" ]; then printf 'optional credential unexpectedly injected\n' >&2; exit 9; fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"days":7,"limit":5,"mode":"repos","query":"drop-in-runtime"}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"query":"drop-in-runtime","source":"github","items":[],"count":0}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&github_search, fs::Permissions::from_mode(0o700)).unwrap();
            let image_generation = bin.join("nanobanana2");
            fs::write(
                &image_generation,
                r#"#!/bin/sh
if [ "$NANOBANANA2_API_KEY" != "canary-image-secret" ]; then printf 'credential missing\n' >&2; exit 9; fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"aspect_ratio":"16:9","model":"","output_path":"outputs/p.png","prompt":"draw-runtime","quality_tier":"balanced","resolution":"2K","thinking":"minimal","use_search":false}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"success":true,"output_path":"outputs/p.png","model":"fixture"}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&image_generation, fs::Permissions::from_mode(0o700)).unwrap();
            let veo_generation = bin.join("veo31");
            fs::write(
                &veo_generation,
                r#"#!/bin/sh
if [ -n "$VEO31_API_KEY" ]; then printf 'missing optional key was injected\n' >&2; exit 9; fi
if [ "$GEMINI_API_KEY" != "canary-gemini-secret" ]; then printf 'alternative credential missing\n' >&2; exit 9; fi
if [ "$#" -ne 0 ]; then printf 'unexpected argv\n' >&2; exit 8; fi
input=$(cat)
expected='{"aspect_ratio":"9:16","duration_seconds":4,"enhance_prompt":false,"generate_audio":true,"model":"","negative_prompt":"","number_of_videos":1,"prompt":"animate-runtime","quality_tier":"fast","resolution":"720p","seed":42}'
if [ "$input" != "$expected" ]; then printf 'canonical input mismatch\n' >&2; exit 8; fi
printf '{"success":true,"output_path":"outputs/v.mp4","model":"fixture"}\n'
"#,
            )
            .unwrap();
            fs::set_permissions(&veo_generation, fs::Permissions::from_mode(0o700)).unwrap();
            let metabase = bin.join("metabase-pp-cli");
            fs::write(
                &metabase,
                r#"#!/bin/sh
if [ "$METABASE_BASE_URL" != "https://metabase.example" ]; then printf 'base url missing\n' >&2; exit 9; fi
if [ "$METABASE_API_KEY" != "canary-metabase-secret" ]; then printf 'api key missing\n' >&2; exit 9; fi
case "$*" in
  "--json --no-input --no-color --yes table list --term active --can-query false --data-source 12 --select id,name")
    printf '{"data":[{"id":12,"name":"active"}]}\n'
    ;;
  "--json --no-input --no-color --yes dataset parameter-search needle --stdin")
    input=$(cat)
    if [ "$input" != '{"field":1}' ]; then printf 'stdin mismatch\n' >&2; exit 8; fi
    printf '{"data":["value"]}\n'
    ;;
  *)
    printf 'argv mismatch: %s\n' "$*" >&2
    exit 8
    ;;
esac
"#,
            )
            .unwrap();
            fs::set_permissions(&metabase, fs::Permissions::from_mode(0o700)).unwrap();
            let legacy_root = canonical_root.join("legacy-secret-env");
            fs::create_dir_all(&legacy_root).unwrap();
            fs::set_permissions(&legacy_root, fs::Permissions::from_mode(0o700)).unwrap();
            let legacy_env = legacy_root.join("tavily.env");
            fs::write(&legacy_env, b"TAVILY_API_KEY=canary-tavily-secret\n").unwrap();
            fs::set_permissions(&legacy_env, fs::Permissions::from_mode(0o600)).unwrap();
            let scope_paths = CapabilityScopePaths {
                principal: "owner".to_owned(),
                workspace: "default".to_owned(),
                capabilities_root: scope_root.clone(),
                bots_root: scope_root.join("bots"),
                auth_root,
                workdirs_root: scope_root.join("workdirs"),
                home_root: scope_root,
                node_modules_bin: canonical_root.join("node_modules/.bin"),
                node_bin: canonical_root.join("node/bin"),
                venv_bin: canonical_root.join("venv/bin"),
            };
            Self {
                _root: root,
                storage,
                bin,
                scope_paths,
                secret_store,
                secret_resolver,
                audit_path,
                legacy_env,
            }
        }

        fn config(&self, expected_work: &str) -> Arc<str> {
            Arc::from(format!(
                "tool_runtime_profiles:\n  - provider: google-workspace\n    storage_namespace: gws\n    alias: work\n    expected_identity: {expected_work}\n  - provider: google-workspace\n    storage_namespace: gws\n    alias: personal\n    expected_identity: personal@example.com\n  - provider: google-workspace\n    storage_namespace: gws\n    alias: presto\n    expected_identity: reach.magican@gmail.com\n"
            ))
        }

        fn ctx(&self, expected_work: &str) -> PrimitiveExecCtx {
            let mut ctx = PrimitiveExecCtx::default_for_runtime()
                .with_scope(Some("owner".to_owned()), Some("default".to_owned()), None)
                .with_secret_context(Some(self.secret_store.clone()), None)
                .with_secret_store_resolver(Some(self.secret_resolver.clone()))
                .with_governed_runtime_overrides(self.config(expected_work), self.bin.clone());
            ctx.storage_base_path = self.storage.clone();
            ctx
        }

        fn provision_tavily_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "TAVILY_API_KEY",
                "canary-tavily-secret",
                "news-search-via-tavily:run",
            );
        }

        fn provision_exa_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "EXA_API_KEY",
                "canary-exa-secret",
                "semantic-websearch-via-exa:run",
            );
        }

        fn provision_openai_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "OPENAI_API_KEY",
                "canary-openai-secret",
                "websearch-via-openai:run",
            );
        }

        fn provision_anthropic_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "ANTHROPIC_API_KEY",
                "canary-anthropic-secret",
                "websearch-via-claude:run",
            );
        }

        fn provision_klipy_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "KLIPY_API_KEY",
                "canary-klipy-secret",
                "gif-search-via-klipy:run",
            );
        }

        fn provision_imgflip_secrets(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "IMGFLIP_USERNAME",
                "canary-imgflip-user",
                "meme-generation-via-imgflip:run",
            );
            self.provision_static_secret(
                principal,
                "IMGFLIP_PASSWORD",
                "canary-imgflip-password",
                "meme-generation-via-imgflip:run",
            );
        }

        fn provision_openai_deep_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "OPENAI_API_KEY",
                "canary-openai-deep-secret",
                "deep-research-with-openai:run",
            );
        }

        fn provision_claude_deep_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "ANTHROPIC_API_KEY",
                "canary-claude-deep-secret",
                "deep-research-with-claude:run",
            );
        }

        fn provision_image_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "NANOBANANA2_API_KEY",
                "canary-image-secret",
                "image-generation:run",
            );
        }

        fn provision_gemini_video_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "GEMINI_API_KEY",
                "canary-gemini-secret",
                "video-generation-via-veo:run",
            );
        }

        fn provision_metabase_secrets(&self, principal: &str, action: &str) {
            let allowed_tool = format!("metabase:{action}");
            self.provision_static_secret(
                principal,
                "METABASE_BASE_URL",
                "https://metabase.example",
                &allowed_tool,
            );
            self.provision_static_secret(
                principal,
                "METABASE_API_KEY",
                "canary-metabase-secret",
                &allowed_tool,
            );
        }

        fn provision_telegram_secret(&self, principal: &str) {
            self.provision_static_secret(
                principal,
                "TELEGRAM_TOKEN",
                "123456:canary-telegram-secret",
                "telegram:run",
            );
        }

        fn provision_static_secret(
            &self,
            principal: &str,
            secret_id: &str,
            value: &str,
            allowed_tool: &str,
        ) {
            self.secret_resolver
                .resolve_for_scope(principal, "default")
                .expect("scoped secret store")
                .store_provisioned(
                    secret_id,
                    "Static provider key",
                    HashMap::from([("value".to_owned(), value.to_owned())]),
                    crate::magician_v2::secrets::InjectionTarget::Header {
                        name: "Authorization".to_owned(),
                        prefix: Some("Bearer ".to_owned()),
                    },
                    crate::magician_v2::secrets::SecretPolicy {
                        allowed_tools: vec![allowed_tool.to_owned()],
                        ..crate::magician_v2::secrets::SecretPolicy::default()
                    },
                )
                .expect("provision static secret");
        }

        async fn run(
            &self,
            name: &str,
            source: &str,
            arguments: Value,
            expected_work: &str,
            ctx: Option<PrimitiveExecCtx>,
        ) -> Result<PrimitiveToolResult, String> {
            self.run_action(name, source, "help", arguments, expected_work, ctx)
                .await
        }

        async fn run_action(
            &self,
            name: &str,
            source: &str,
            action: &str,
            arguments: Value,
            expected_work: &str,
            ctx: Option<PrimitiveExecCtx>,
        ) -> Result<PrimitiveToolResult, String> {
            self.run_action_with_legacy_environment(
                name,
                source,
                action,
                arguments,
                expected_work,
                ctx,
                None,
            )
            .await
        }

        #[allow(clippy::too_many_arguments)]
        async fn run_action_with_legacy_environment(
            &self,
            name: &str,
            source: &str,
            action: &str,
            arguments: Value,
            expected_work: &str,
            ctx: Option<PrimitiveExecCtx>,
            legacy_secret_environment: Option<PathBuf>,
        ) -> Result<PrimitiveToolResult, String> {
            let package = parse_skill_runtime_package(source)
                .expect("runtime package parse")
                .expect("runtime package");
            let validated = validate_skill_runtime_contract(&package.contract).expect("contract");
            let actions = tool_runtime_core::action_overrides::compile_typed_action_overrides(
                name,
                validated,
                package.actions.as_ref().expect("actions"),
            )
            .expect("compiled actions");
            dispatch_governed_runtime(
                admit_governed_route(),
                Arc::new(GovernedRuntimeImplementation {
                    package,
                    actions: Some(actions),
                    executable_directory: None,
                    legacy_secret_environment,
                }),
                name.to_owned(),
                action.to_owned(),
                arguments,
                ctx.unwrap_or_else(|| self.ctx(expected_work)),
                self.scope_paths.clone(),
                BTreeMap::new(),
            )
            .await
        }
    }

    #[tokio::test]
    async fn all_six_google_workspace_packs_execute_through_the_governed_runtime() {
        let fixture = Fixture::new();
        for (name, source) in PACKS {
            let fixed = name.starts_with("presto-");
            let arguments = if fixed {
                serde_json::json!({})
            } else {
                serde_json::json!({"account": "work"})
            };
            let result = fixture
                .run(name, source, arguments, "work@example.com", None)
                .await
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(result.success, "{name}");
            assert_eq!(result.parsed_json.as_ref().unwrap()["cloud_ok"], true);
            let alias = if fixed { "presto" } else { "work" };
            assert!(fixture
                .scope_paths
                .auth_root
                .join(format!("gws-{alias}/action-ran"))
                .is_file());
        }
        let audit = fs::read_to_string(&fixture.audit_path).expect("durable audit journal");
        assert_eq!(
            audit
                .matches("\"event\":\"governed_profile_status\"")
                .count(),
            6
        );
        assert_eq!(audit.matches("\"event\":\"governed_execution\"").count(), 6);
        for (name, _) in PACKS {
            assert!(audit.contains(&format!("\"tool\":\"{name}\"")), "{name}");
        }
    }

    #[tokio::test]
    async fn selectable_profile_isolation_binds_only_the_requested_auth_directory() {
        let fixture = Fixture::new();
        let result = fixture
            .run(
                "calendar",
                PACKS[0].1,
                serde_json::json!({"account": "personal"}),
                "work@example.com",
                None,
            )
            .await
            .expect("personal invocation");
        let output = result.parsed_json.expect("json output");
        assert_eq!(output["cloud_ok"], true);
        assert!(fixture
            .scope_paths
            .auth_root
            .join("gws-personal/action-ran")
            .is_file());
        assert!(!fixture
            .scope_paths
            .auth_root
            .join("gws-work/action-ran")
            .exists());
    }

    #[tokio::test]
    async fn auth_status_and_login_use_declared_lifecycle_hooks_not_action_argv() {
        let fixture = Fixture::new();
        let status = fixture
            .run_action(
                "calendar",
                PACKS[0].1,
                "auth_status",
                serde_json::json!({"account": "work"}),
                "work@example.com",
                None,
            )
            .await
            .expect("status");
        assert!(status.success);
        assert_eq!(status.parsed_json.unwrap()["user"], "work@example.com");

        let login = fixture
            .run_action(
                "calendar",
                PACKS[0].1,
                "auth_login",
                serde_json::json!({"account": "work"}),
                "work@example.com",
                None,
            )
            .await
            .expect("login");
        assert!(login.success);
        assert_eq!(login.parsed_json.unwrap()["login"], true);
        assert!(!fixture
            .scope_paths
            .auth_root
            .join("gws-work/action-ran")
            .exists());
    }

    #[tokio::test]
    async fn expected_identity_mismatch_blocks_before_the_action_process() {
        let fixture = Fixture::new();
        let error = fixture
            .run(
                "calendar",
                PACKS[0].1,
                serde_json::json!({"account": "work"}),
                "wrong@example.com",
                None,
            )
            .await
            .expect_err("identity mismatch");
        assert!(error.contains("expected identity"));
        assert!(!fixture
            .scope_paths
            .auth_root
            .join("gws-work/action-ran")
            .exists());
    }

    #[tokio::test]
    async fn missing_audit_authority_blocks_before_status_or_action_processes() {
        let fixture = Fixture::new();
        let mut ctx = fixture.ctx("work@example.com");
        ctx.secret_store = None;

        let error = fixture
            .run(
                "calendar",
                PACKS[0].1,
                serde_json::json!({"account": "work"}),
                "work@example.com",
                Some(ctx),
            )
            .await
            .expect_err("missing audit authority");

        assert_eq!(error, "governed runtime audit authority is unavailable");
        assert!(!fixture
            .scope_paths
            .auth_root
            .join("gws-work/action-ran")
            .exists());
    }

    #[tokio::test]
    async fn timeout_and_cancellation_are_bounded_and_do_not_fall_back() {
        let fixture = Fixture::new();
        fs::write(fixture.scope_paths.auth_root.join("gws-work/slow"), b"slow").unwrap();
        let started = Instant::now();
        let timed = fixture
            .run(
                "calendar",
                PACKS[0].1,
                serde_json::json!({"account": "work", "timeout_secs": 1}),
                "work@example.com",
                None,
            )
            .await;
        assert!(timed.is_err() || !timed.unwrap().success);
        assert!(started.elapsed() < Duration::from_secs(6));

        let token = tokio_util::sync::CancellationToken::new();
        let ctx = fixture.ctx("work@example.com");
        let mut cancelled_ctx = ctx;
        cancelled_ctx.cancellation_token = Some(token.clone());
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token.cancel();
        });
        let started = Instant::now();
        let cancelled = fixture
            .run(
                "calendar",
                PACKS[0].1,
                serde_json::json!({"account": "work", "timeout_secs": 5}),
                "work@example.com",
                Some(cancelled_ctx),
            )
            .await;
        cancel_task.await.unwrap();
        assert!(cancelled.is_err() || !cancelled.unwrap().success);
        assert!(started.elapsed() < Duration::from_secs(6));
    }

    #[tokio::test]
    async fn static_secret_pack_resolves_exact_scope_and_delivers_canonical_input_after_admission()
    {
        let fixture = Fixture::new();
        fixture.provision_tavily_secret("owner");

        let result = fixture
            .run_action(
                TAVILY_PACK.0,
                TAVILY_PACK.1,
                "run",
                serde_json::json!({
                    "query": "latest-news",
                    "search_depth": "advanced",
                    "max_results": 3
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("static-secret execution");

        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["cost_microunits"], 2_000_000);
        assert!(!result.stdout.contains("canary-tavily-secret"));
        let audit = fs::read_to_string(&fixture.audit_path).expect("durable audit journal");
        assert!(audit.contains("\"tool\":\"news-search-via-tavily\""));
        assert!(!audit.contains("canary-tavily-secret"));
    }

    #[tokio::test]
    async fn telegram_bot_keeps_token_out_of_argv_and_delivers_canonical_input() {
        let fixture = Fixture::new();
        fixture.provision_telegram_secret("owner");

        let result = fixture
            .run_action(
                TELEGRAM_PACK.0,
                TELEGRAM_PACK.1,
                "run",
                serde_json::json!({
                    "method": "sendMessage",
                    "data": "{\"chat_id\":7,\"text\":\"hello\"}"
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("telegram execution");

        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["result"]["message_id"], 11);
        assert!(!result.stdout.contains("canary-telegram-secret"));
        let audit = fs::read_to_string(&fixture.audit_path).expect("durable audit journal");
        assert!(audit.contains("\"tool\":\"telegram\""));
        assert!(!audit.contains("canary-telegram-secret"));
    }

    #[tokio::test]
    async fn exa_static_secret_pack_receives_canonical_boolean_and_search_input() {
        let fixture = Fixture::new();
        fixture.provision_exa_secret("owner");

        let result = fixture
            .run_action(
                EXA_PACK.0,
                EXA_PACK.1,
                "run",
                serde_json::json!({
                    "query": "semantic-launch",
                    "type": "deep",
                    "num_results": 2,
                    "contents": false,
                    "highlights": true
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("Exa static-secret execution");

        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["search_type"], "deep");
        assert!(!result.stdout.contains("canary-exa-secret"));
    }

    #[tokio::test]
    async fn openai_websearch_static_secret_pack_receives_canonical_model_and_domain_input() {
        let fixture = Fixture::new();
        fixture.provision_openai_secret("owner");

        let result = fixture
            .run_action(
                OPENAI_WEBSEARCH_PACK.0,
                OPENAI_WEBSEARCH_PACK.1,
                "run",
                serde_json::json!({
                    "query": "cited-answer",
                    "model": "future-model",
                    "allowed_domains": "example.com,openai.com"
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("OpenAI websearch static-secret execution");

        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["usage"]["total_tokens"], 12);
        assert!(!result.stdout.contains("canary-openai-secret"));
    }

    #[tokio::test]
    async fn claude_websearch_static_secret_pack_receives_canonical_search_and_domain_input() {
        let fixture = Fixture::new();
        fixture.provision_anthropic_secret("owner");

        let result = fixture
            .run_action(
                CLAUDE_WEBSEARCH_PACK.0,
                CLAUDE_WEBSEARCH_PACK.1,
                "run",
                serde_json::json!({
                    "query": "contextual-answer",
                    "model": "future-claude",
                    "max_searches": 4,
                    "allowed_domains": "example.com/blog,research.example"
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("Claude websearch static-secret execution");

        assert!(result.success);
        assert_eq!(
            result.parsed_json.unwrap()["usage"]["web_search_requests"],
            2
        );
        assert!(!result.stdout.contains("canary-anthropic-secret"));
    }

    #[tokio::test]
    async fn klipy_gif_search_static_secret_pack_receives_canonical_filter_and_limit_input() {
        let fixture = Fixture::new();
        fixture.provision_klipy_secret("owner");

        let result = fixture
            .run_action(
                KLIPY_GIF_SEARCH_PACK.0,
                KLIPY_GIF_SEARCH_PACK.1,
                "run",
                serde_json::json!({
                    "query": "celebrate",
                    "limit": "7",
                    "content_filter": "pg-13"
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("KLIPY GIF-search static-secret execution");

        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["count"], 1);
        assert!(!result.stdout.contains("canary-klipy-secret"));
    }

    #[tokio::test]
    async fn imgflip_meme_multi_secret_pack_injects_both_values_and_receives_canonical_input() {
        let fixture = Fixture::new();
        fixture.provision_imgflip_secrets("owner");

        let result = fixture
            .run_action(
                IMGFLIP_MEME_PACK.0,
                IMGFLIP_MEME_PACK.1,
                "run",
                serde_json::json!({
                    "action": "caption",
                    "template_id": "181913649",
                    "template_name": "Drake",
                    "text0": "old-way",
                    "text1": "new-way"
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("Imgflip meme multi-secret execution");

        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["action"], "caption");
        assert!(!result.stdout.contains("canary-imgflip-user"));
        assert!(!result.stdout.contains("canary-imgflip-password"));
    }

    #[tokio::test]
    async fn deep_research_packs_receive_only_canonical_input_and_scoped_credentials() {
        let openai = Fixture::new();
        openai.provision_openai_deep_secret("owner");
        let result = openai
            .run_action(
                OPENAI_DEEP_RESEARCH_PACK.0,
                OPENAI_DEEP_RESEARCH_PACK.1,
                "run",
                serde_json::json!({
                    "query": "investigate",
                    "model": "research-model",
                    "poll_interval_secs": 1,
                    "max_poll_attempts": 2,
                    "max_tool_calls": 9
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("OpenAI deep-research execution");
        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["poll_attempts"], 2);
        assert!(!result.stdout.contains("canary-openai-deep-secret"));

        let claude = Fixture::new();
        claude.provision_claude_deep_secret("owner");
        let result = claude
            .run_action(
                CLAUDE_DEEP_RESEARCH_PACK.0,
                CLAUDE_DEEP_RESEARCH_PACK.1,
                "run",
                serde_json::json!({
                    "query": "investigate-claude",
                    "model": "claude-research",
                    "max_searches": 3,
                    "max_fetches": 2,
                    "thinking_budget": 2048,
                    "allowed_domains": "example.com"
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("Claude deep-research execution");
        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["answer"], "deep-claude");
        assert!(!result.stdout.contains("canary-claude-deep-secret"));
    }

    #[tokio::test]
    async fn optional_and_alternative_secret_contracts_omit_missing_environment_values() {
        let github = Fixture::new();
        let result = github
            .run_action(
                GITHUB_SEARCH_PACK.0,
                GITHUB_SEARCH_PACK.1,
                "run",
                serde_json::json!({
                    "query": "drop-in-runtime",
                    "days": 7,
                    "limit": 5,
                    "mode": "repos"
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("anonymous GitHub search execution");
        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["count"], 0);

        let veo = Fixture::new();
        veo.provision_gemini_video_secret("owner");
        let result = veo
            .run_action(
                VEO_GENERATION_PACK.0,
                VEO_GENERATION_PACK.1,
                "run",
                serde_json::json!({
                    "prompt": "animate-runtime",
                    "duration_seconds": 4,
                    "aspect_ratio": "9:16",
                    "resolution": "720p",
                    "quality_tier": "fast",
                    "number_of_videos": 1,
                    "negative_prompt": "",
                    "seed": 42,
                    "enhance_prompt": false,
                    "generate_audio": true,
                    "model": ""
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("single-alternative Veo credential execution");
        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["model"], "fixture");
        assert!(!result.stdout.contains("canary-gemini-secret"));
    }

    #[tokio::test]
    async fn image_pack_executes_in_governed_workspace_with_canonical_input() {
        let fixture = Fixture::new();
        fixture.provision_image_secret("owner");
        let result = fixture
            .run_action(
                IMAGE_GENERATION_PACK.0,
                IMAGE_GENERATION_PACK.1,
                "run",
                serde_json::json!({
                    "prompt": "draw-runtime",
                    "output_path": "outputs/p.png",
                    "aspect_ratio": "16:9",
                    "resolution": "2K",
                    "quality_tier": "balanced",
                    "thinking": "minimal",
                    "use_search": false,
                    "model": ""
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("image-generation execution");
        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["output_path"], "outputs/p.png");
        assert!(!result.stdout.contains("canary-image-secret"));
    }

    #[tokio::test]
    async fn metabase_direct_cli_preserves_boolean_argv_and_action_owned_stdin_ordering() {
        let fixture = Fixture::new();
        fixture.provision_metabase_secrets("owner", "table_list");
        let result = fixture
            .run_action(
                METABASE_PACK.0,
                METABASE_PACK.1,
                "table_list",
                serde_json::json!({
                    "term": "active",
                    "can_query": false,
                    "data_source": 12,
                    "extra_args": ["--select", "id,name"]
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("Metabase typed argv execution");
        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["data"][0]["id"], 12);

        fixture.provision_metabase_secrets("owner", "dataset_parameter_search");
        let result = fixture
            .run_action(
                METABASE_PACK.0,
                METABASE_PACK.1,
                "dataset_parameter_search",
                serde_json::json!({
                    "args": ["needle"],
                    "stdin": "{\"field\":1}"
                }),
                "unused@example.com",
                None,
            )
            .await
            .expect("Metabase action-owned stdin execution");
        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["data"][0], "value");
        assert!(!result.stdout.contains("canary-metabase-secret"));
    }

    #[tokio::test]
    async fn implicit_cli_owned_session_preserves_home_native_argv_and_approved_workspace() {
        let fixture = Fixture::new();
        let workspace = fs::canonicalize(fixture._root.path().join("coding-workspace")).unwrap();
        let mut ctx = fixture.ctx("unused@example.com");
        ctx.session_file_sandbox_roots = Some(Arc::new(std::sync::Mutex::new(
            std::collections::HashSet::from([workspace.display().to_string()]),
        )));

        let result = fixture
            .run_action(
                CLI_OWNED_BATCH_PACK.0,
                CLI_OWNED_BATCH_PACK.1,
                "run",
                serde_json::json!({
                    "prompt": "make drop-in",
                    "working_dir": workspace,
                    "timeout_secs": 30
                }),
                "unused@example.com",
                Some(ctx),
            )
            .await
            .expect("implicit CLI-owned execution");
        assert!(result.success);
        assert_eq!(result.parsed_json.unwrap()["session"], "cli-owned");
        assert!(workspace.join(".cli-owned-ran").is_file());
        let audit = fs::read_to_string(&fixture.audit_path).expect("durable audit journal");
        assert!(audit.contains("\"tool\":\"cli-owned-batch\""));
        if let Ok(home) = std::env::var("HOME") {
            assert!(home.is_empty() || !audit.contains(&home));
        }
    }

    #[tokio::test]
    async fn implicit_cli_argument_rules_replace_the_higgsfield_policy_wrapper() {
        let fixture = Fixture::new();

        for args in [
            serde_json::json!(["model", "list", "--json"]),
            serde_json::json!(["workspace", "status", "--json"]),
        ] {
            let result = fixture
                .run_action(
                    HIGGSFIELD_PACK.0,
                    HIGGSFIELD_PACK.1,
                    "run",
                    serde_json::json!({"args": args, "timeout_secs": 30}),
                    "unused@example.com",
                    None,
                )
                .await
                .expect("allowed direct Higgsfield execution");
            assert!(result.success, "{}", result.stderr);
        }

        let invocation_log = fixture.bin.join(".higgsfield-invocations");
        let before = fs::read_to_string(&invocation_log).unwrap_or_default();
        for args in [
            serde_json::json!(["auth", "login"]),
            serde_json::json!(["workspace", "set", "other"]),
            serde_json::json!(["workspace"]),
        ] {
            let error = fixture
                .run_action(
                    HIGGSFIELD_PACK.0,
                    HIGGSFIELD_PACK.1,
                    "run",
                    serde_json::json!({"args": args}),
                    "unused@example.com",
                    None,
                )
                .await
                .expect_err("manifest-owned argument policy must fail before launch");
            assert!(
                error.starts_with("governed runtime input rejected:"),
                "{error}"
            );
            assert!(!error.contains("auth"));
            assert!(!error.contains("workspace"));
        }
        assert_eq!(
            fs::read_to_string(&invocation_log).unwrap_or_default(),
            before,
            "denied invocations must never reach the installed CLI"
        );
    }

    #[tokio::test]
    async fn implicit_cli_owned_session_rejects_unapproved_absolute_workspace_before_launch() {
        let fixture = Fixture::new();
        let workspace = fs::canonicalize(fixture._root.path().join("coding-workspace")).unwrap();
        let error = fixture
            .run_action(
                CLI_OWNED_BATCH_PACK.0,
                CLI_OWNED_BATCH_PACK.1,
                "run",
                serde_json::json!({"prompt": "make drop-in", "working_dir": workspace}),
                "unused@example.com",
                None,
            )
            .await
            .expect_err("unapproved absolute workspace");
        assert_eq!(
            error,
            "governed implicit CLI requested workspace is not authorized"
        );
        assert!(!workspace.join(".cli-owned-ran").exists());
    }

    #[tokio::test]
    async fn pack_level_pty_coding_agents_are_rejected() {
        let fixture = Fixture::new();
        let error = fixture
            .run_action(
                "retired-pty",
                r#"---
name: "retired-pty"
description: "Fixture pack-level PTY skill."
metadata:
  magician:
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [retired-pty]
      runtime:
        protocol: cli
        command_prefix: []
        interaction: pty
        working_directory: {mode: workspace}
        limits:
          timeout_secs: 30
          stdout_bytes: 1024
          stderr_bytes: 1024
      auth:
        kind: cli_profile
        requirement: required
        provider: fixture-provider
        profile_selection: {mode: implicit}
        storage: {kind: cli_owned}
      policy_floor:
        approval: conditional_external_side_effect
        resource_scopes: [workspace]
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        run:
          description: Run.
          parameters:
            prompt:
              type: string
              description: Exact fixture prompt.
              required: true
          mappings:
            - {type: runtime_control, parameter: prompt}
---
"#,
                "run",
                serde_json::json!({"prompt": "should not spawn"}),
                "unused@example.com",
                None,
            )
            .await
            .expect_err("retired coding-agent PTY packs must fail closed");
        assert!(
            error.contains("coding-agent CLI packs have been retired"),
            "{error}"
        );
        let spawn_marker = fixture
            .bin
            .parent()
            .expect("fixture root")
            .join("coding-workspace")
            .join(".retired-pty-ran");
        assert!(
            !spawn_marker.exists() && !fixture.bin.join(".retired-pty-ran").exists(),
            "pack-level PTY coding agents must fail before spawn"
        );
    }

    #[tokio::test]
    async fn static_secret_pack_fails_closed_for_missing_or_wrong_scope_material() {
        let fixture = Fixture::new();
        fixture.provision_tavily_secret("anonymous");

        let error = fixture
            .run_action(
                TAVILY_PACK.0,
                TAVILY_PACK.1,
                "run",
                serde_json::json!({"query": "latest-news", "search_depth": "advanced", "max_results": 3}),
                "unused@example.com",
                None,
            )
            .await
            .expect_err("wrong-scope secret must not resolve");

        assert!(error.contains("before settlement"));
        assert!(!error.contains("TAVILY_API_KEY"));
        assert!(!error.contains("canary-tavily-secret"));
    }

    #[tokio::test]
    async fn static_secret_pack_can_use_private_legacy_env_without_exposing_it() {
        let fixture = Fixture::new();

        let result = fixture
            .run_action_with_legacy_environment(
                TAVILY_PACK.0,
                TAVILY_PACK.1,
                "run",
                serde_json::json!({"query": "latest-news", "search_depth": "advanced", "max_results": 3}),
                "unused@example.com",
                None,
                Some(fixture.legacy_env.clone()),
            )
            .await
            .expect("legacy environment bridge");

        assert!(result.success);
        assert!(!result.stdout.contains("canary-tavily-secret"));
        let audit = fs::read_to_string(&fixture.audit_path).expect("durable audit journal");
        assert!(!audit.contains("canary-tavily-secret"));
    }

    #[tokio::test]
    async fn canonical_static_secret_takes_precedence_over_legacy_environment() {
        let fixture = Fixture::new();
        fixture.provision_tavily_secret("owner");
        fs::write(&fixture.legacy_env, b"TAVILY_API_KEY=wrong-legacy-value\n").unwrap();
        fs::set_permissions(&fixture.legacy_env, fs::Permissions::from_mode(0o600)).unwrap();

        let result = fixture
            .run_action_with_legacy_environment(
                TAVILY_PACK.0,
                TAVILY_PACK.1,
                "run",
                serde_json::json!({"query": "latest-news", "search_depth": "advanced", "max_results": 3}),
                "unused@example.com",
                None,
                Some(fixture.legacy_env.clone()),
            )
            .await
            .expect("canonical vault value must win");

        assert!(result.success);
        assert!(!result.stdout.contains("wrong-legacy-value"));
    }

    #[tokio::test]
    async fn legacy_static_secret_environment_must_be_private_and_regular() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.legacy_env, fs::Permissions::from_mode(0o644)).unwrap();

        let error = fixture
            .run_action_with_legacy_environment(
                TAVILY_PACK.0,
                TAVILY_PACK.1,
                "run",
                serde_json::json!({"query": "latest-news", "search_depth": "advanced", "max_results": 3}),
                "unused@example.com",
                None,
                Some(fixture.legacy_env.clone()),
            )
            .await
            .expect_err("world-readable legacy secrets must fail closed");

        assert_eq!(error, "governed runtime credential adapter is invalid");
    }

    #[tokio::test]
    async fn legacy_static_secret_environment_is_bounded_before_parsing() {
        let fixture = Fixture::new();
        fs::write(&fixture.legacy_env, vec![b'x'; (1024 * 1024) + 1]).unwrap();
        fs::set_permissions(&fixture.legacy_env, fs::Permissions::from_mode(0o600)).unwrap();

        let error = fixture
            .run_action_with_legacy_environment(
                TAVILY_PACK.0,
                TAVILY_PACK.1,
                "run",
                serde_json::json!({"query": "latest-news", "search_depth": "advanced", "max_results": 3}),
                "unused@example.com",
                None,
                Some(fixture.legacy_env.clone()),
            )
            .await
            .expect_err("oversized legacy secrets must fail before parsing");

        assert_eq!(error, "governed runtime credential adapter is invalid");
    }

    /// `first_bundle` selects by `&'static str`, so a temporary path has to
    /// outlive the borrow checker's view of the test. Leaking a few bytes per
    /// case is cheaper than making the production signature owned.
    fn static_path(path: PathBuf) -> &'static str {
        Box::leak(path.to_string_lossy().into_owned().into_boxed_str())
    }

    #[test]
    fn ca_bundle_resolution_is_decided_by_order_not_by_existence_alone() {
        let root = tempfile::tempdir().expect("temp root");
        let preferred = root.path().join("preferred.pem");
        let fallback = root.path().join("fallback.pem");
        fs::write(&preferred, b"preferred").expect("preferred bundle");
        fs::write(&fallback, b"fallback").expect("fallback bundle");
        let preferred = static_path(preferred);
        let fallback = static_path(fallback);

        // Both exist, so only position can decide -- and reversing the list
        // reverses the answer. This is what a silent reorder would break.
        assert_eq!(first_bundle(&[preferred, fallback]), Some(preferred));
        assert_eq!(first_bundle(&[fallback, preferred]), Some(fallback));

        // A missing leading candidate is skipped, not treated as terminal.
        let missing = static_path(root.path().join("absent.pem"));
        assert_eq!(first_bundle(&[missing, fallback]), Some(fallback));
        assert_eq!(first_bundle(&[missing]), None);
        assert_eq!(first_bundle(&[]), None);
    }

    #[test]
    fn ca_bundle_resolution_rejects_directories_and_unreadable_files() {
        let root = tempfile::tempdir().expect("temp root");
        let directory = root.path().join("cert.pem");
        fs::create_dir(&directory).expect("directory candidate");
        let unreadable = root.path().join("unreadable.pem");
        fs::write(&unreadable, b"unreadable").expect("unreadable bundle");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000))
            .expect("drop read permission");
        let readable = root.path().join("readable.pem");
        fs::write(&readable, b"readable").expect("readable bundle");
        let directory = static_path(directory);
        let unreadable = static_path(unreadable);
        let readable = static_path(readable);

        // A directory named like a bundle is not a bundle.
        assert_eq!(first_bundle(&[directory, readable]), Some(readable));
        assert_eq!(first_bundle(&[directory]), None);

        // A root test runner can read a 0o000 file, which would make the
        // readability assertions vacuous rather than wrong. Only assert them
        // once the precondition actually holds.
        if fs::File::open(unreadable).is_err() {
            assert_eq!(first_bundle(&[unreadable, readable]), Some(readable));
            assert_eq!(first_bundle(&[unreadable]), None);
        }
    }

    #[test]
    fn os_owned_ca_bundles_precede_package_manager_prefixes() {
        let first_writable_prefix = CA_BUNDLE_CANDIDATES
            .iter()
            .position(|candidate| {
                candidate.starts_with("/opt/homebrew/") || candidate.starts_with("/usr/local/")
            })
            .expect("package-manager candidates are present");
        let last_os_owned = CA_BUNDLE_CANDIDATES
            .iter()
            .rposition(|candidate| candidate.starts_with("/etc/"))
            .expect("OS-owned candidates are present");

        // First match wins, so a prefix a non-root actor can rewrite must never
        // be reachable before an OS-owned store.
        assert!(
            last_os_owned < first_writable_prefix,
            "a writable package-manager prefix must not shadow an OS-owned trust store"
        );
        // The distro-canonical Linux store must be reachable before any prefix;
        // its absence was the original ordering defect.
        assert!(CA_BUNDLE_CANDIDATES[..first_writable_prefix]
            .contains(&"/etc/ssl/certs/ca-certificates.crt"));
    }
}
