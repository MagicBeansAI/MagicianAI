//! Deterministic invocation of installed CLI-template capabilities.
//!
//! Agent tool calls, user-defined feeds, recurring monitors, and other
//! application services use this same execution path. The caller decides
//! which capability/action to invoke; this module performs no LLM routing.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tool_runtime_core::action_overrides::WorkspacePathAccess;
use tracing::instrument;

use super::{
    cli_template::CliTemplateDispatcher,
    exec_ctx::PrimitiveExecCtx,
    governed_runtime::{admit_governed_route, dispatch_governed_runtime},
    runner::{PrimitiveDispatcher, PrimitiveToolResult},
};
use crate::magician_v2::{
    artifact_v2::{
        capabilities::{CapabilityScopePaths, CapabilityWorkspaceManager},
        workspace::{ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE},
    },
    execution::{
        actions::{ActionResult, ExecutableAction},
        capability::{CapabilityRegistry, ImplementationType},
        compiled_dispatch::{
            execute_maybe_gated, record_skill_invocation_result, CompiledDispatchContext,
            SkillInvocationRecord,
        },
        pack_provider::maybe_wrap_with_spend_gate,
        ExecutionError,
    },
    learning::{
        fingerprint_input_shape, redacted_input_shape, LearningSkillInvocationSource, LearningStore,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeterministicCapabilityInvocationSource {
    AgentTool,
    UserFeed,
    RecurringMonitor,
    ObservedSource,
    InteractiveRead,
    InternalSystem,
}

impl DeterministicCapabilityInvocationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentTool => "agent_tool",
            Self::UserFeed => "user_feed",
            Self::RecurringMonitor => "recurring_monitor",
            Self::ObservedSource => "observed_source",
            Self::InteractiveRead => "interactive_read",
            Self::InternalSystem => "internal_system",
        }
    }

    fn active_owner(self, fallback: &str) -> String {
        match self {
            // Preserve the pre-extraction owner exactly for agent calls so
            // spend reservations and observability identities do not drift.
            Self::AgentTool => format!("inner-loop:{fallback}"),
            _ => format!("{}:{fallback}", self.as_str()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeterministicCapabilityInvocation {
    pub capability_name: String,
    pub action: String,
    pub arguments: Value,
    pub source: DeterministicCapabilityInvocationSource,
    /// Optional scope overrides for application services. Agent calls leave
    /// these unset and retain the scope already present in `PrimitiveExecCtx`.
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub execution_id: Option<String>,
    /// The dispatch attempt this invocation belongs to, when it belongs to one.
    ///
    /// Carried on the INVOCATION rather than inherited from the invoker's base
    /// context, and the distinction matters. `invoke` clones a base
    /// `PrimitiveExecCtx` that a caller may hold for a long time — the
    /// content-source runtime caches one per scope — so an attempt id sitting in
    /// that base would be reused by every later invocation through the same
    /// handle. A stale key is worse than no key: it travels to a remote as an
    /// assertion that this request is a repeat of one already performed, and a
    /// remote honouring it answers with the earlier call's response instead of
    /// doing the work.
    pub effect_id: Option<String>,
    /// Exact non-model filesystem authorities supplied by a product-owned
    /// application service. Agent tool calls may never carry these bindings.
    pub(super) brokered_workspace_paths: BTreeMap<String, BrokeredWorkspacePath>,
}

#[derive(Debug, Clone)]
pub(super) struct BrokeredWorkspacePath {
    pub(super) path: PathBuf,
    pub(super) access: WorkspacePathAccess,
}

/// Resolve the context one invocation runs under, from the invoker's base.
///
/// Extracted from `invoke` so the rules below are assertable without standing up
/// a capability to dispatch to. There are two of them and they differ:
///
/// - Scope and execution id OVERRIDE. An invocation that says nothing keeps
///   whatever the base carries, because a direct application caller and an
///   agent caller legitimately share one invoker and differ only in scope.
/// - `effect_id` is ASSIGNED. The base may be a context held across many
///   attempts — the content-source runtime caches one per scope — so inheriting
///   an attempt id from it would key this dispatch to an unrelated earlier one.
///   A stale key is worse than none: it tells a remote this request is a repeat
///   of one already performed, and a remote honouring it answers with the
///   earlier response instead of doing the work.
fn resolved_exec_ctx(
    base: &PrimitiveExecCtx,
    invocation: &DeterministicCapabilityInvocation,
) -> Result<PrimitiveExecCtx, ExecutionError> {
    let mut exec_ctx = base.clone();
    match (
        invocation.principal.as_deref(),
        invocation.workspace.as_deref(),
    ) {
        (Some(principal), Some(workspace))
            if !principal.trim().is_empty() && !workspace.trim().is_empty() =>
        {
            exec_ctx.principal = Some(principal.to_string());
            exec_ctx.workspace = Some(workspace.to_string());
        },
        (None, None) => {},
        _ => {
            return Err(ExecutionError::Step(
                "deterministic capability scope override requires non-empty principal and workspace"
                    .to_string(),
            ));
        },
    }
    if let Some(execution_id) = invocation.execution_id.as_ref() {
        exec_ctx.execution_id = Some(execution_id.clone());
    }
    exec_ctx.effect_id = invocation.effect_id.clone();
    Ok(exec_ctx)
}

impl DeterministicCapabilityInvocation {
    pub fn new(
        capability_name: impl Into<String>,
        action: impl Into<String>,
        arguments: Value,
        source: DeterministicCapabilityInvocationSource,
    ) -> Self {
        Self {
            capability_name: capability_name.into(),
            action: action.into(),
            arguments,
            source,
            principal: None,
            workspace: None,
            execution_id: None,
            effect_id: None,
            brokered_workspace_paths: BTreeMap::new(),
        }
    }

    /// Attach the dispatch attempt this invocation belongs to.
    ///
    /// Only a caller that is itself inside one attempt should set this. An
    /// invoker held across attempts must leave it unset, which reads as "not
    /// attributable" and never as "safe to repeat".
    pub fn with_effect_id(mut self, effect_id: Option<String>) -> Self {
        self.effect_id = effect_id;
        self
    }

    pub fn with_scope(
        mut self,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        self.principal = Some(principal.into());
        self.workspace = Some(workspace.into());
        self
    }

    pub fn with_execution_id(mut self, execution_id: impl Into<String>) -> Self {
        self.execution_id = Some(execution_id.into());
        self
    }

    /// Bind one exact application-owned path to a typed workspace-path
    /// parameter. This is intentionally separate from `arguments`: the model
    /// cannot mint this authority by spelling an absolute path.
    pub fn with_brokered_workspace_path(
        mut self,
        parameter: impl Into<String>,
        path: impl Into<PathBuf>,
        access: WorkspacePathAccess,
    ) -> Self {
        self.brokered_workspace_paths.insert(
            parameter.into(),
            BrokeredWorkspacePath {
                path: path.into(),
                access,
            },
        );
        self
    }
}

#[derive(Debug, Clone)]
pub struct DeterministicCapabilityInvocationResult {
    pub output: ActionResult,
    pub duration_ms: u64,
}

impl DeterministicCapabilityInvocationResult {
    pub fn output_text(&self) -> Option<&str> {
        self.output.as_text()
    }

    pub fn parsed_json(&self) -> anyhow::Result<Value> {
        let text = self
            .output_text()
            .ok_or_else(|| anyhow::anyhow!("capability did not return textual output"))?;
        serde_json::from_str(text).map_err(Into::into)
    }
}

#[async_trait]
pub trait DeterministicCapabilityInvoker: Send + Sync {
    async fn invoke(
        &self,
        invocation: DeterministicCapabilityInvocation,
    ) -> Result<DeterministicCapabilityInvocationResult, ExecutionError>;
}

/// Runtime invoker bound to the installed capability registry and a base
/// execution context. Direct application consumers clone this handle and set
/// scope overrides per invocation; agent execution passes its populated
/// `PrimitiveExecCtx` as the base and leaves those overrides empty.
#[derive(Clone)]
pub struct ScopedDeterministicCapabilityInvoker {
    registry: Arc<CapabilityRegistry>,
    base_exec_ctx: PrimitiveExecCtx,
    scope_paths: Option<CapabilityScopePaths>,
}

impl ScopedDeterministicCapabilityInvoker {
    pub fn new(registry: Arc<CapabilityRegistry>, base_exec_ctx: PrimitiveExecCtx) -> Self {
        Self {
            registry,
            base_exec_ctx,
            scope_paths: None,
        }
    }

    /// Bind the canonical runtime paths for this invoker's exact scope.
    /// Application services should always use this instead of relying on the
    /// process working directory to reconstruct packaged skill paths.
    pub fn with_scope_paths(mut self, scope_paths: CapabilityScopePaths) -> Self {
        self.scope_paths = Some(scope_paths);
        self
    }
}

#[async_trait]
impl DeterministicCapabilityInvoker for ScopedDeterministicCapabilityInvoker {
    /// One span per capability invocation, whatever the caller — agent tool,
    /// feed, monitor, observed source — so the view can answer "what did this
    /// run actually call" without a span per internal dispatch stage.
    ///
    /// `invocation.arguments` is model-authored and may embed user content and
    /// secrets-by-reference, so `skip_all` is mandatory; only the capability
    /// name, action name and caller family are named. Scope falls back to the
    /// bound execution context when the invocation carries no override, and is
    /// omitted entirely when neither has one so the layer can inherit it from
    /// the enclosing execution span.
    #[instrument(
        name = "capability_invoke",
        skip_all,
        fields(
            activity_kind = crate::magician_v2::analytics::runtime_activity_layer::KIND_CAPABILITY,
            capability = %invocation.capability_name,
            action = %invocation.action,
            source = invocation.source.as_str(),
            principal = invocation
                .principal
                .as_deref()
                .or(self.base_exec_ctx.principal.as_deref()),
            workspace = invocation
                .workspace
                .as_deref()
                .or(self.base_exec_ctx.workspace.as_deref()),
        )
    )]
    async fn invoke(
        &self,
        invocation: DeterministicCapabilityInvocation,
    ) -> Result<DeterministicCapabilityInvocationResult, ExecutionError> {
        let exec_ctx = resolved_exec_ctx(&self.base_exec_ctx, &invocation)?;
        if let Some(scope_paths) = self.scope_paths.as_ref() {
            let principal = exec_ctx
                .principal
                .as_deref()
                .unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
            let workspace = exec_ctx
                .workspace
                .as_deref()
                .unwrap_or(DEFAULT_SCOPE_WORKSPACE);
            if scope_paths.principal != principal || scope_paths.workspace != workspace {
                return Err(ExecutionError::Step(format!(
                    "deterministic capability invocation scope {principal}/{workspace} does not match bound scope {}/{}",
                    scope_paths.principal, scope_paths.workspace
                )));
            }
        }
        invoke_cli_template_capability(
            &invocation,
            &exec_ctx,
            self.registry.clone(),
            self.scope_paths.as_ref(),
        )
        .await
    }
}

async fn invoke_cli_template_capability(
    invocation: &DeterministicCapabilityInvocation,
    exec_ctx: &PrimitiveExecCtx,
    registry: Arc<CapabilityRegistry>,
    bound_scope_paths: Option<&CapabilityScopePaths>,
) -> Result<DeterministicCapabilityInvocationResult, ExecutionError> {
    let pack_name = invocation.capability_name.trim();
    let action = invocation.action.trim();
    if pack_name.is_empty() || action.is_empty() {
        return Err(ExecutionError::Step(
            "deterministic capability invocation requires capability and action".to_string(),
        ));
    }
    if invocation.source == DeterministicCapabilityInvocationSource::AgentTool
        && !invocation.brokered_workspace_paths.is_empty()
    {
        return Err(ExecutionError::Step(
            "agent tool calls cannot carry broker-owned workspace paths".to_string(),
        ));
    }

    let pack = registry
        .get_pack_definition(pack_name)
        .ok_or_else(|| ExecutionError::Step(format!("unknown inner-loop pack `{pack_name}`")))?;
    let runtime_package = match &pack.implementation {
        ImplementationType::Primitive {
            provider_name: None,
            runtime_package,
            ..
        } => runtime_package.clone(),
        ImplementationType::Primitive {
            provider_name: Some(_),
            ..
        } => {
            return Err(ExecutionError::Step(format!(
                "capability `{pack_name}` is provider-backed, not a CLI-template skill"
            )));
        },
        _ => {
            return Err(ExecutionError::Step(format!(
                "capability `{pack_name}` is not a primitive skill"
            )));
        },
    };

    let principal = exec_ctx
        .principal
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_WORKSPACE);
    let scope_paths = bound_scope_paths.cloned().unwrap_or_else(|| {
        let scope_manager = CapabilityWorkspaceManager::new(
            ArtifactV2Workspace::new(&exec_ctx.storage_base_path),
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        scope_manager.scope_paths(principal, workspace)
    });
    enum DispatchOwner {
        Governed(Arc<crate::magician_v2::execution::capability::GovernedRuntimeImplementation>),
        Legacy(CliTemplateDispatcher),
    }
    let dispatch_owner = match runtime_package {
        Some(package) => DispatchOwner::Governed(package),
        None => DispatchOwner::Legacy(
            CliTemplateDispatcher::from_definition(&pack, Some(scope_paths.clone()))
                .map_err(|error| {
                    ExecutionError::Step(format!(
                        "Failed to build CliTemplateDispatcher for `{pack_name}`: {error}"
                    ))
                })?
                .with_progress_publisher(exec_ctx.progress_publisher.clone()),
        ),
    };
    let invocation_source = match &dispatch_owner {
        DispatchOwner::Governed(_) => LearningSkillInvocationSource::GovernedRuntime,
        DispatchOwner::Legacy(_) => LearningSkillInvocationSource::PrimitiveCliTemplate,
    };

    let resolved_params = arguments_to_param_map(&invocation.arguments);
    let representative = ExecutableAction::Pack {
        capability_name: pack.name.clone(),
        implementation: pack.implementation.clone(),
        resolved_params: resolved_params
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    crate::magician_v2::json_traversal::clone_json_iteratively(value),
                )
            })
            .collect(),
    };
    let lowered = maybe_wrap_with_spend_gate(representative, Some(&pack), &resolved_params);

    let session_id = exec_ctx
        .execution_id
        .clone()
        .or_else(|| exec_ctx.legacy_execution_id.clone());
    let owner_fallback = session_id.as_deref().unwrap_or(pack_name);
    let active_owner = invocation.source.active_owner(owner_fallback);
    let ctx = CompiledDispatchContext {
        principal,
        workspace: Some(workspace),
        agent_id: exec_ctx.agent_id.as_deref().unwrap_or(""),
        session_id: session_id.as_deref(),
        task_id: exec_ctx.task_id.as_deref(),
        execution_id: session_id.as_deref(),
        chat_session_id: exec_ctx.chat_session_id.as_deref(),
        invocation_context: exec_ctx.invocation_context.as_ref(),
        calling_profile_name: None,
        app_owner_execution_credential: None,
        active_owner,
        effect_id: None,
        invocation_source: invocation_source.clone(),
        learning_store: exec_ctx.artifact_workspace.clone().map(LearningStore::new),
    };

    let arguments_owned =
        crate::magician_v2::json_traversal::clone_json_iteratively(&invocation.arguments);
    let action_owned = action.to_string();
    let pack_name_owned = pack_name.to_string();
    let governed_exec_ctx = exec_ctx.clone();
    let governed_scope_paths = scope_paths.clone();
    let brokered_workspace_paths = invocation.brokered_workspace_paths.clone();
    let exec_inner = move |_action: &ExecutableAction| async move {
        let result = match dispatch_owner {
            DispatchOwner::Governed(package) => dispatch_governed_runtime(
                admit_governed_route(),
                package,
                pack_name_owned.clone(),
                action_owned.clone(),
                arguments_owned,
                governed_exec_ctx,
                governed_scope_paths,
                brokered_workspace_paths,
            )
            .await
            .map_err(ExecutionError::Step)?,
            DispatchOwner::Legacy(dispatcher) => dispatcher
                .dispatch(&action_owned, &arguments_owned)
                .await
                .map_err(|error| ExecutionError::Step(error.to_string()))?,
        };
        fold_primitive_result(&pack_name_owned, &action_owned, result)
    };

    let started = Instant::now();
    let outcome = execute_maybe_gated(
        lowered,
        exec_inner,
        exec_ctx.compiled_dispatch_authority.as_ref(),
        &ctx,
        pack_name,
    )
    .await;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    record_primitive_skill_invocation(
        exec_ctx,
        invocation_source.clone(),
        pack_name,
        Some(action),
        &invocation.arguments,
        started,
        serde_json::json!({
            "dispatch": invocation_source.as_str(),
            "invoker": "deterministic_capability_invoker",
            "deterministic_invocation_source": invocation.source.as_str(),
        }),
        &outcome,
    );
    tracing::debug!(
        capability = pack_name,
        action,
        invocation_source = invocation.source.as_str(),
        duration_ms,
        success = outcome.is_ok(),
        "deterministic capability invocation completed"
    );
    outcome.map(|output| DeterministicCapabilityInvocationResult {
        output,
        duration_ms,
    })
}

fn arguments_to_param_map(arguments: &Value) -> HashMap<String, Value> {
    let mut params = HashMap::new();
    if let Some(obj) = arguments.as_object() {
        if let Some(nested) = obj.get("parameters").and_then(Value::as_object) {
            for (key, value) in nested {
                params.insert(
                    key.clone(),
                    crate::magician_v2::json_traversal::clone_json_iteratively(value),
                );
            }
        }
        for (key, value) in obj {
            if key != "parameters" {
                params.insert(
                    key.clone(),
                    crate::magician_v2::json_traversal::clone_json_iteratively(value),
                );
            }
        }
    }
    params
}

pub(super) fn record_primitive_skill_invocation<E: std::fmt::Display>(
    exec_ctx: &PrimitiveExecCtx,
    source: LearningSkillInvocationSource,
    pack_name: &str,
    action: Option<&str>,
    arguments: &Value,
    started: Instant,
    payload: Value,
    outcome: &std::result::Result<ActionResult, E>,
) {
    let Some(workspace_layout) = exec_ctx.artifact_workspace.clone() else {
        return;
    };
    let principal = exec_ctx
        .principal
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_WORKSPACE);
    let session_id = exec_ctx
        .execution_id
        .as_deref()
        .or(exec_ctx.legacy_execution_id.as_deref());
    let input_shape = redacted_input_shape(&arguments_to_param_map(arguments));
    let input_fingerprint = fingerprint_input_shape(&input_shape);
    let active_owner = format!(
        "primitive:{}",
        session_id.unwrap_or_else(|| action.unwrap_or(pack_name))
    );
    let ctx = CompiledDispatchContext {
        principal,
        workspace: Some(workspace),
        agent_id: exec_ctx.agent_id.as_deref().unwrap_or(""),
        session_id,
        task_id: exec_ctx.task_id.as_deref(),
        execution_id: session_id,
        chat_session_id: exec_ctx.chat_session_id.as_deref(),
        invocation_context: exec_ctx.invocation_context.as_ref(),
        calling_profile_name: None,
        app_owner_execution_credential: None,
        active_owner,
        effect_id: None,
        invocation_source: source,
        learning_store: Some(LearningStore::new(workspace_layout)),
    };
    record_skill_invocation_result(
        &ctx,
        SkillInvocationRecord {
            skill_name: pack_name.to_string(),
            tool_action_name: action.map(str::to_string),
            task_id: exec_ctx.task_id.clone(),
            execution_id: session_id.map(str::to_string),
            input_fingerprint,
            input_shape,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            retry_count: 0,
            evidence_refs: Vec::new(),
            payload,
        },
        outcome,
    );
}

pub(crate) fn fold_primitive_result(
    pack_name: &str,
    action: &str,
    result: PrimitiveToolResult,
) -> Result<ActionResult, ExecutionError> {
    if !result.success {
        if let Some(failure) = parse_capability_failure(&result.stderr) {
            return Err(ExecutionError::CapabilityFailure {
                code: failure.error.code,
                message: failure.error.message,
            });
        }
        let message = if !result.stderr.trim().is_empty() {
            result.stderr
        } else if !result.stdout.trim().is_empty() {
            result.stdout
        } else {
            format!("`{pack_name}__{action}` failed")
        };
        return Err(ExecutionError::Step(message));
    }
    Ok(ActionResult::Text {
        content: result.stdout,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityFailureEnvelope {
    ok: bool,
    error: CapabilityFailureBody,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityFailureBody {
    code: String,
    message: String,
}

fn parse_capability_failure(stderr: &str) -> Option<CapabilityFailureEnvelope> {
    if stderr.len() > 16 * 1024 {
        return None;
    }
    let envelope = serde_json::from_str::<CapabilityFailureEnvelope>(stderr.trim()).ok()?;
    if envelope.ok
        || envelope.error.code.is_empty()
        || envelope.error.code.len() > 64
        || !envelope
            .error
            .code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || envelope.error.message.is_empty()
        || envelope.error.message.len() > 8 * 1024
        || envelope.error.message.chars().any(char::is_control)
    {
        return None;
    }
    Some(envelope)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn fixture_registry(script: &std::path::Path) -> Arc<CapabilityRegistry> {
        let registry = Arc::new(CapabilityRegistry::new());
        registry.set_pack_definition(
            "deterministic-fixture",
            crate::magician_v2::execution::capability::CapabilityPackDefinition {
                name: "deterministic-fixture".to_string(),
                description: None,
                version: Some("1.0.0".to_string()),
                guide: None,
                native_action_schemas: HashMap::new(),
                parameters: Vec::new(),
                implementation: ImplementationType::Primitive {
                    runtime_package: None,
                    provider_name: None,
                    intent_description: None,
                    command: Some(vec!["sh".to_string(), script.to_string_lossy().to_string()]),
                    cwd: None,
                    env: HashMap::from([("FIXTURE_QUERY".to_string(), "{query}".to_string())]),
                    suffix_args: Vec::new(),
                    timeout_secs: Some(5),
                    operation: None,
                    prompt: None,
                },
                execution: None,
                auth: None,
                reliability: None,
                result_projection: None,
            },
        );
        registry
    }

    #[test]
    fn agent_owner_identity_is_backward_compatible() {
        assert_eq!(
            DeterministicCapabilityInvocationSource::AgentTool.active_owner("exec-1"),
            "inner-loop:exec-1"
        );
        assert_eq!(
            DeterministicCapabilityInvocationSource::UserFeed.active_owner("feed-1"),
            "user_feed:feed-1"
        );
    }

    #[test]
    fn invocation_result_parses_structured_stdout() {
        let result = DeterministicCapabilityInvocationResult {
            output: ActionResult::Text {
                content: r#"{"results":[]}"#.to_string(),
            },
            duration_ms: 2,
        };
        assert_eq!(
            result.parsed_json().unwrap()["results"],
            serde_json::json!([])
        );
    }

    #[test]
    fn declared_failure_envelope_preserves_stable_code_and_message() {
        for (code, message) in [
            ("unsupported", "scanned PDF requires OCR"),
            ("resource_limit", "document exceeded a parser safety limit"),
            ("missing_part", "document package is incomplete"),
        ] {
            let error = fold_primitive_result(
                "document-to-markdown",
                "convert",
                PrimitiveToolResult {
                    success: false,
                    stderr: serde_json::json!({
                        "ok": false,
                        "error": {"code": code, "message": message}
                    })
                    .to_string(),
                    ..PrimitiveToolResult::default()
                },
            )
            .unwrap_err();

            assert!(matches!(
                error,
                ExecutionError::CapabilityFailure { code: actual_code, message: actual_message }
                    if actual_code == code && actual_message == message
            ));
        }
    }

    #[test]
    fn malformed_or_unbounded_failure_envelope_remains_an_opaque_step_error() {
        for stderr in [
            r#"{"ok":false,"error":{"code":"NOT-STABLE","message":"bad"}}"#.to_string(),
            format!(
                r#"{{"ok":false,"error":{{"code":"unsupported","message":"{}"}}}}"#,
                "x".repeat(9 * 1024)
            ),
        ] {
            let error = fold_primitive_result(
                "fixture",
                "run",
                PrimitiveToolResult {
                    success: false,
                    stderr,
                    ..PrimitiveToolResult::default()
                },
            )
            .unwrap_err();
            assert!(matches!(error, ExecutionError::Step(_)));
        }
    }

    #[tokio::test]
    async fn direct_and_agent_paths_share_the_same_cli_execution() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("fixture.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '{\"query\":\"%s\"}\\n' \"$FIXTURE_QUERY\"\n",
        )
        .unwrap();
        let registry = fixture_registry(&script);
        let mut exec_ctx = PrimitiveExecCtx::default_for_runtime();
        exec_ctx.storage_base_path = temp.path().join("storage");
        exec_ctx.principal = Some("principal".to_string());
        exec_ctx.workspace = Some("workspace".to_string());
        exec_ctx.execution_id = Some("execution".to_string());
        let arguments = serde_json::json!({"query":"same implementation"});

        let direct = ScopedDeterministicCapabilityInvoker::new(registry.clone(), exec_ctx.clone())
            .invoke(DeterministicCapabilityInvocation::new(
                "deterministic-fixture",
                "run",
                arguments.clone(),
                DeterministicCapabilityInvocationSource::UserFeed,
            ))
            .await
            .unwrap();
        let agent = super::super::dispatch::dispatch_primitive(
            "deterministic-fixture",
            "run",
            &arguments,
            &exec_ctx,
            registry,
            None,
        )
        .await
        .unwrap();

        assert_eq!(direct.output.as_text(), agent.as_text());
        assert_eq!(
            direct.parsed_json().unwrap()["query"],
            serde_json::json!("same implementation")
        );
    }

    // ========================================================================
    // Effect identity — docs/components/magician/effect-identity.md
    // ========================================================================

    fn invocation() -> DeterministicCapabilityInvocation {
        DeterministicCapabilityInvocation::new(
            "fixture",
            "run",
            serde_json::json!({}),
            DeterministicCapabilityInvocationSource::AgentTool,
        )
    }

    #[test]
    fn an_invoker_reused_across_attempts_cannot_leak_a_stale_key() {
        // REGRESSION GUARD, and the reason `effect_id` is assigned rather than
        // inherited. This invoker's base was built inside one attempt; the
        // content-source runtime caches invokers per scope, so the same handle
        // really does outlive the attempt that created it. If the id came
        // through the clone, every later dispatch would tell a remote it was a
        // repeat of that first attempt — and a remote honouring the key would
        // answer with the first call's response instead of doing the work.
        let mut base = PrimitiveExecCtx::default_for_runtime();
        base.effect_id = Some("llm_call_abc:tool:toolu_01".to_string());

        let resolved = resolved_exec_ctx(&base, &invocation()).expect("scope resolves");
        assert!(
            resolved.effect_id.is_none(),
            "an invocation that names no attempt must not inherit one, got {:?}",
            resolved.effect_id
        );
    }

    #[test]
    fn a_caller_inside_an_attempt_still_carries_it_through() {
        // The other half: the loop's own dispatch builds an invoker from the
        // live context and states the attempt on the invocation. Assignment
        // must not have cost us the case the whole change exists to serve.
        let base = PrimitiveExecCtx::default_for_runtime();
        let resolved = resolved_exec_ctx(
            &base,
            &invocation().with_effect_id(Some("llm_call_abc:tool:toolu_01".to_string())),
        )
        .expect("scope resolves");

        assert_eq!(
            resolved.effect_id.as_deref(),
            Some("llm_call_abc:tool:toolu_01")
        );
    }

    #[test]
    fn scope_and_execution_id_still_fall_back_to_the_base() {
        // Assignment applies to the attempt id ONLY. Scope and execution id
        // keep overriding, because a direct application caller and an agent
        // caller legitimately share one invoker and differ only in scope.
        let mut base = PrimitiveExecCtx::default_for_runtime();
        base.principal = Some("owner".to_string());
        base.workspace = Some("default".to_string());
        base.execution_id = Some("execution-from-base".to_string());

        let resolved = resolved_exec_ctx(&base, &invocation()).expect("scope resolves");
        assert_eq!(resolved.principal.as_deref(), Some("owner"));
        assert_eq!(resolved.workspace.as_deref(), Some("default"));
        assert_eq!(
            resolved.execution_id.as_deref(),
            Some("execution-from-base")
        );
    }

    #[tokio::test]
    async fn partial_scope_override_fails_before_capability_resolution() {
        let invoker = ScopedDeterministicCapabilityInvoker::new(
            Arc::new(CapabilityRegistry::new()),
            PrimitiveExecCtx::default_for_runtime(),
        );
        let mut invocation = DeterministicCapabilityInvocation::new(
            "missing",
            "run",
            serde_json::json!({}),
            DeterministicCapabilityInvocationSource::InternalSystem,
        );
        invocation.principal = Some("principal".to_string());

        let error = invoker.invoke(invocation).await.unwrap_err();
        assert!(error.to_string().contains("principal and workspace"));
    }

    #[tokio::test]
    async fn agent_invocations_cannot_mint_product_brokered_file_authority() {
        let invoker = ScopedDeterministicCapabilityInvoker::new(
            Arc::new(CapabilityRegistry::new()),
            PrimitiveExecCtx::default_for_runtime(),
        );
        let invocation = DeterministicCapabilityInvocation::new(
            "missing",
            "run",
            serde_json::json!({"input_file":"/tmp/untrusted"}),
            DeterministicCapabilityInvocationSource::AgentTool,
        )
        .with_brokered_workspace_path(
            "input_file",
            "/tmp/untrusted",
            WorkspacePathAccess::ReadFile,
        );

        let error = invoker.invoke(invocation).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("agent tool calls cannot carry broker-owned workspace paths"));
        assert!(!error.to_string().contains("unknown inner-loop pack"));
    }

    #[tokio::test]
    async fn canonical_scope_paths_reject_foreign_scope_before_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let manager = CapabilityWorkspaceManager::new(
            ArtifactV2Workspace::new(temp.path().join("runtime")),
            temp.path(),
        );
        let invoker = ScopedDeterministicCapabilityInvoker::new(
            Arc::new(CapabilityRegistry::new()),
            PrimitiveExecCtx::default_for_runtime().with_scope(
                Some("owner".to_string()),
                Some("default".to_string()),
                None,
            ),
        )
        .with_scope_paths(manager.scope_paths("owner", "default"));
        let invocation = DeterministicCapabilityInvocation::new(
            "missing",
            "run",
            serde_json::json!({}),
            DeterministicCapabilityInvocationSource::InternalSystem,
        )
        .with_scope("guest", "default");

        let error = invoker.invoke(invocation).await.unwrap_err();
        assert!(error.to_string().contains("does not match bound scope"));
        assert!(!error.to_string().contains("unknown inner-loop pack"));
    }
}
