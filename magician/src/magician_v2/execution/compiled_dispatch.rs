//! Caller-agnostic fast-path dispatcher for `Compiled` capability packs.
//!
//! This module owns the in-process, low-overhead dispatch primitive that
//! short-circuits the heavyweight `dispatch_capability_pack` machinery
//! (sub-execution id, chat-fanout, progress subscriptions, disk markers,
//! heartbeat task) for capability packs whose `implementation.type` is
//! `Compiled` — i.e. pure Rust `CapabilityProvider`s that complete
//! in-process without spawning a subprocess.
//!
//! ## Why this lives outside `chat`
//!
//! The fast-path was first wired from `ChatService` and grew up under a
//! `try_dispatch_chat_compiled_pack` name (now renamed to
//! `try_dispatch_compiled_pack` on `ChatService`, kept as a thin wrapper).
//! Reviewing the body showed three
//! distinct concerns mashed together: (1) registry lookup + impl-type
//! check + execute, (2) optional spend-gating through the resource
//! authority, and (3) chat-shaped param injection / JSON enveloping.
//! Concerns 1 + 2 are caller-agnostic — the same primitive serves the
//! inner-loop `CompiledProviderDispatcher`, a future voice tool
//! dispatcher, a planning preflight, or any caller that needs to invoke
//! a Compiled pack with minimal ceremony. Concern 3 stays caller-shaped.
//!
//! The split also lets each caller pass its own `active_owner`
//! identifier into the gated path (chat uses `chat:{session_id}` to
//! avoid accidental token matches against real agent ids; another caller
//! would namespace its own pseudo-owner the same way).
//!
//! ## Boundary
//!
//! This module knows about:
//! - `CapabilityRegistry` + `CapabilityProvider` + `ImplementationType`
//! - `ActionResult` + `ExecutableAction` + `ExecutionError`
//! - The `resource_authority` primitives (ledger, token store, ceilings,
//!   freeze, spend resolver, gated action)
//!
//! This module does NOT know about:
//! - `ChatSession`, `ChatService`, chat-bridge tools, the chat event
//!   broadcaster
//! - The inner-loop's `PrimitiveToolResult` shape, the autonomous
//!   executor's outer-loop state
//! - Any caller-specific param-injection policy (chat's "search_memory
//!   needs agent_id" rule lives in the chat layer, not here)
//!
//! Callers compose around this primitive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::magician_v2::apps::boundary::AppOwnerExecutionCredential;
use crate::magician_v2::apps::models::{AppDigest, AppProjectionHandle};
use crate::magician_v2::apps::records::AppDataHandlingPolicy;
use crate::magician_v2::execution::actions::{ActionResult, ExecutableAction};
use crate::magician_v2::execution::capability::{
    CapabilityProvider, CapabilityRegistry, ImplementationType,
};
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::json_traversal::{
    clone_json_iteratively, exact_json_encoded_len, inspect_json_bounded,
    json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded, json_string_encoded_len,
    MAX_RETAINED_JSON_DEPTH,
};
use crate::magician_v2::learning::{
    classify_skill_invocation_failure, fingerprint_input_shape, redacted_input_shape,
    CreateLearningSkillInvocationEvidenceRequest, LearningEvidenceRef, LearningScope,
    LearningSkillInvocationSource, LearningSkillInvocationStatus, LearningStore,
};
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::resource_authority::scoped_authority::{
    is_safe_scope_id, ScopedAuthorityBundle, ScopedAuthorityResolver, SingleScopeResolver,
};
use crate::magician_v2::resource_authority::spend_session::{
    admit, MissingBudgetPolicy, SpendAdmission, SpendIntent, SpendOwnerPolicy, SpendSessionError,
};
use crate::magician_v2::strategy::plan::PlanStep;

tokio::task_local! {
    /// Per-execution cancellation token for the CURRENT compiled dispatch (B2).
    /// `dispatch_flat_action`'s PlainCompiled arm scopes this around the handler
    /// future; a compiled handler that owns a cancellable downstream reads it to
    /// opt into cooperative cancellation — e.g. `run_coding_task` threads it into
    /// `CodingEngineRequest.cancel_token` so a wall-clock deadline / Stop tears the
    /// Pi turn down GRACEFULLY instead of relying on the abrupt future-drop. `None`
    /// outside a scoped dispatch. The chain from the scope point to the handler is
    /// fully inline-awaited (no `tokio::spawn` between them), so the value
    /// propagates; a future spawn inserted there would silently drop it.
    pub static EXECUTION_CANCEL_TOKEN: Option<CancellationToken>;
}

tokio::task_local! {
    /// Verified owner request credential visible only while a governed tool is
    /// executing under the guard-preserving compiled dispatch owner. It is an
    /// in-process typed capability and never enters provider/model JSON.
    pub(crate) static COMPILED_APP_OWNER_EXECUTION_CREDENTIAL:
        Option<Arc<AppOwnerExecutionCredential>>;
}

pub(crate) fn current_compiled_app_owner_execution_credential(
) -> Option<Arc<AppOwnerExecutionCredential>> {
    COMPILED_APP_OWNER_EXECUTION_CREDENTIAL
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

tokio::task_local! {
    /// Server-resolved invocation provenance for the current compiled call.
    /// Authorization-aware handlers read this typed carrier instead of JSON
    /// parameters; absent scope is a denial, never an implicit direct turn.
    pub static COMPILED_INVOCATION_CONTEXT:
        Option<crate::magician_v2::agents::AgentInvocationContext>;
}

pub fn current_compiled_invocation_context(
) -> Option<crate::magician_v2::agents::AgentInvocationContext> {
    COMPILED_INVOCATION_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

tokio::task_local! {
    /// Exact physical LLM profile that produced the compiled tool call. The
    /// chat runtime resolves this server-side after adaptive-profile selection;
    /// model arguments cannot supply it. Security-sensitive handlers fail
    /// closed when a dispatch rail cannot provide the profile identity.
    static COMPILED_CALLING_PROFILE_NAME: Option<String>;
}

pub fn current_compiled_calling_profile_name() -> Option<String> {
    COMPILED_CALLING_PROFILE_NAME
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

/// Trusted policy metadata for a compiled app-data result. This type is
/// deliberately neither serializable nor cloneable: the handler publishes it
/// through a task-local sink and the dispatch owner consumes it exactly once.
pub struct CompiledAppResultGuard {
    handling_policy: AppDataHandlingPolicy,
    content_digest: AppDigest,
    projection_handle: Option<AppProjectionHandle>,
}

impl CompiledAppResultGuard {
    pub fn handling_policy(&self) -> &AppDataHandlingPolicy {
        &self.handling_policy
    }

    pub fn projection_handle(&self) -> Option<&AppProjectionHandle> {
        self.projection_handle.as_ref()
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn into_parts(
        self,
    ) -> (
        AppDataHandlingPolicy,
        AppDigest,
        Option<AppProjectionHandle>,
    ) {
        (
            self.handling_policy,
            self.content_digest,
            self.projection_handle,
        )
    }

    pub(crate) fn binds_value(&self, value: &Value) -> Result<bool, ExecutionError> {
        let digest = AppDigest::blake3_canonical_json(value)
            .map_err(|error| ExecutionError::Step(format!("app result digest: {error}")))?;
        Ok(digest == self.content_digest)
    }
}

tokio::task_local! {
    /// One-shot metadata return channel for the current compiled handler. A
    /// normal provider result cannot carry this authority because it crosses
    /// the model-visible JSON boundary.
    static COMPILED_APP_RESULT_SINK: Option<Arc<Mutex<Option<CompiledAppResultGuard>>>>;
}

pub fn publish_current_compiled_app_result_guard(
    handling_policy: AppDataHandlingPolicy,
    value: &Value,
    projection_handle: Option<AppProjectionHandle>,
) -> Result<(), ExecutionError> {
    let content_digest = AppDigest::blake3_canonical_json(value)
        .map_err(|error| ExecutionError::Step(format!("app result digest: {error}")))?;
    COMPILED_APP_RESULT_SINK
        .try_with(|sink| {
            let sink = sink.as_ref().ok_or_else(|| {
                ExecutionError::Step(
                    "governed app result was produced outside compiled dispatch".to_owned(),
                )
            })?;
            let mut slot = sink.lock().map_err(|_| {
                ExecutionError::Step("governed app result metadata sink is unavailable".to_owned())
            })?;
            if slot.is_some() {
                return Err(ExecutionError::Step(
                    "compiled handler published app result metadata more than once".to_owned(),
                ));
            }
            *slot = Some(CompiledAppResultGuard {
                handling_policy,
                content_digest,
                projection_handle,
            });
            Ok(())
        })
        .map_err(|_| {
            ExecutionError::Step(
                "governed app result was produced outside compiled dispatch".to_owned(),
            )
        })?
}

pub fn is_governed_app_compiled_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "app_discover"
            | "app_action_compose"
            | "app_action_invoke"
            | "app_data_query"
            | "app_data_search"
            | "app_data_compose"
            | "app_memory_propose"
    )
}

/// The owner which would have to preserve the non-serializable app-result
/// policy guard for a governed compiled tool. Registration is scope-wide, so
/// every surface constructor must apply this predicate before advertising a
/// registered owner-facing tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GovernedAppToolSurface {
    #[cfg(test)]
    GuardPreservingLocalChat,
    ExternalModel,
    AutonomousTask,
    DelegatedOrHandover,
    AppWorkflow,
}

pub(crate) fn compiled_tool_is_exposable_on_surface(
    tool_name: &str,
    surface: GovernedAppToolSurface,
) -> bool {
    if !is_governed_app_compiled_tool(tool_name) {
        return true;
    }
    #[cfg(test)]
    {
        matches!(surface, GovernedAppToolSurface::GuardPreservingLocalChat)
    }
    #[cfg(not(test))]
    {
        let _ = surface;
        false
    }
}

const MAX_RESULT_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESULT_JSON_NODES: usize = 200_000;
// Root object + kind/status/headers/body values. Object keys contribute bytes,
// while each header value contributes one retained JSON node.
const HTTP_RESULT_ENVELOPE_NODES: usize = 5;

tokio::task_local! {
    /// Exact product identity for a model call made from inside a compiled
    /// capability provider. Compiled providers receive only an action and an
    /// optional session id, so without this scoped hand-off a nested model
    /// request can retain neither the caller's tenant nor its task/chat
    /// lineage. The value is owned and cloneable so it cannot borrow the
    /// short-lived [`CompiledDispatchContext`].
    static COMPILED_LLM_CALL_CONTEXT: Option<CompiledLlmCallContext>;
}

/// Product identity inherited by a model-calling compiled provider.
#[derive(Debug, Clone)]
pub struct CompiledLlmCallContext {
    pub trace_context: magicllm::LlmTraceContext,
    pub agent_id: String,
}

/// Return the current compiled provider's model-call identity, if dispatch
/// supplied a valid principal/workspace pair. Providers that issue model
/// requests must fail closed when this is absent instead of silently writing
/// telemetry into a synthetic system/default tenant.
pub fn current_compiled_llm_call_context() -> Option<CompiledLlmCallContext> {
    COMPILED_LLM_CALL_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

tokio::task_local! {
    /// Operator-approved sandbox roots for the CURRENT compiled dispatch.
    /// `dispatch_flat_action`'s PlainCompiled arm scopes this (from
    /// `PrimitiveExecCtx.session_file_sandbox_roots`, threaded off
    /// `ActionExecutors`) around the handler future — the same inline-awaited
    /// chain `EXECUTION_CANCEL_TOKEN` relies on. File-touching compiled handlers
    /// (`read_file`) read `current_session_file_sandbox_roots()` and merge these
    /// into their effective `FileSandboxConfig.allowed_roots`, so a folder the
    /// operator approved via the sandbox-override HITL is readable on the retry
    /// (parity with the native `files` pack path). `None` outside a scoped
    /// dispatch — the handler then uses the boot `file_sandbox` unchanged.
    pub static SESSION_FILE_SANDBOX_ROOTS:
        Option<std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>>;
}

/// The operator-approved sandbox roots for the current compiled dispatch, if
/// this call is running inside a `SESSION_FILE_SANDBOX_ROOTS` scope. Compiled
/// file handlers use it to widen their sandbox on a post-approval retry.
pub fn current_session_file_sandbox_roots(
) -> Option<std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>> {
    SESSION_FILE_SANDBOX_ROOTS
        .try_with(|roots| roots.clone())
        .ok()
        .flatten()
}

/// Process-wide resource-authority handle. Internally an
/// `Arc<dyn ScopedAuthorityResolver>` that resolves
/// `(principal, workspace) → ScopedAuthorityBundle` lazily on first
/// access per scope.
///
/// Why a resolver instead of fixed Arcs: resource-authority state
/// (ledger journal, token store, ceilings, freeze) is persisted
/// per-scope under `<workspace>/scopes/{principal}/{workspace}/resource_authority/`.
/// Both the REST API (`/budget` UI) and the dispatch gate need to
/// operate on the same in-memory Arcs for a given scope so writes
/// from one path are visible to the other. The resolver caches the
/// bundle per `(principal, workspace)` to satisfy that single-source-
/// of-truth requirement.
///
/// Was previously a struct of fixed Arcs (named
/// `ChatResourceAuthorityContext` originally). That shape was
/// scope-agnostic and ended up empty + non-persistent in `bin/magician.rs`
/// while the REST API operated on a separate per-scope store —
/// the `/budget` UI and the gate never saw the same data. Lifting
/// the resolver behind this handle collapses that split.
#[derive(Clone)]
pub struct CompiledDispatchAuthority {
    inner: Arc<dyn ScopedAuthorityResolver>,
}

impl std::fmt::Debug for CompiledDispatchAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledDispatchAuthority")
            .field("enabled", &self.inner.is_enabled())
            .finish()
    }
}

impl CompiledDispatchAuthority {
    /// Production path. Pass `Arc::new(DiskBackedScopedResolver::new(...))`
    /// from boot wiring; every dispatch + every REST handler resolves
    /// scopes through the same resolver.
    pub fn from_resolver(resolver: Arc<dyn ScopedAuthorityResolver>) -> Self {
        Self { inner: resolver }
    }

    /// Test / orphan path. Wraps a fixed `ScopedAuthorityBundle` in a
    /// `SingleScopeResolver` so callers without a workspace context
    /// still get a working authority.
    pub fn from_bundle(bundle: ScopedAuthorityBundle) -> Self {
        Self {
            inner: Arc::new(SingleScopeResolver::new(bundle)),
        }
    }

    /// Cheap process-wide enabled check — no scope resolution. The
    /// gate uses this to short-circuit `MaybeGatedAction::Gated → degraded`
    /// without touching the per-scope cache when authority is off.
    pub fn is_enabled(&self) -> bool {
        self.inner.is_enabled()
    }

    /// Resolve the per-`(principal, workspace)` bundle. First call
    /// for a given scope loads from disk + caches; subsequent calls
    /// return the cached Arcs.
    pub async fn resolve_scope(&self, principal: &str, workspace: &str) -> ScopedAuthorityBundle {
        self.inner.resolve_scope(principal, workspace).await
    }

    /// Underlying resolver Arc — needed by `ResourceAuthorityApi` so
    /// the REST API can resolve the same scope the gate sees.
    pub fn resolver(&self) -> Arc<dyn ScopedAuthorityResolver> {
        Arc::clone(&self.inner)
    }
}

/// Per-call caller-shaped context. Each caller fills these in from
/// whatever runtime state it owns (chat session, voice call, etc.).
///
/// `principal` + `workspace` together pick the on-disk resource-
/// authority scope the gate operates against. Both must be populated
/// when the dispatched pack carries a `spend:` declaration — without
/// `workspace` the resolver can't find the right scope, so the gate
/// rejects with a clear error.
///
/// `active_owner` is the synthetic identifier passed to
/// `spend_session::admit` as `SpendOwnerPolicy::Authorized`. Callers
/// choose a namespaced shape so no real agent's `issued_to` accidentally
/// matches a config-driven token through the active-owner branch. Chat
/// uses `chat:{session_id}`; another caller would pick its own prefix
/// (e.g. `voice:{call_id}`). See `spend_token_resolver` for the issuance
/// scheme that makes the chain branch the canonical match path.
#[derive(Clone)]
pub struct CompiledDispatchContext<'a> {
    pub principal: &'a str,
    pub workspace: Option<&'a str>,
    pub agent_id: &'a str,
    pub session_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub execution_id: Option<&'a str>,
    pub chat_session_id: Option<&'a str>,
    pub invocation_context: Option<&'a crate::magician_v2::agents::AgentInvocationContext>,
    pub calling_profile_name: Option<&'a str>,
    /// Authenticated request capability for owner-only governed app tools.
    /// Every non-HTTP/non-chat caller must set this explicitly to `None`.
    pub app_owner_execution_credential: Option<Arc<AppOwnerExecutionCredential>>,
    pub active_owner: String,
    /// Identity of the dispatch attempt, so a provider that reaches a remote
    /// can send a derived idempotency key. `None` means the dispatch is not
    /// attributable to a model tool call — never that it is safe to repeat.
    pub effect_id: Option<&'a str>,
    pub invocation_source: LearningSkillInvocationSource,
    pub learning_store: Option<LearningStore>,
}

/// High-level entry: try to dispatch `tool_name` through a `Compiled`
/// capability provider, looking up both the pack definition and the
/// provider from `registry` keyed by `tool_name`.
///
/// Use this when the LLM-supplied tool name equals the pack name
/// equals the provider name — the chat fast path case, where each
/// chat-exposed tool is registered as its own pack with its own
/// provider under the same key.
///
/// Returns:
/// - `None` — the registry has no `Compiled` pack matching `tool_name`.
///   Caller should fall through to whatever heavyweight dispatch path
///   it has (chat → `dispatch_capability_pack`, inner-loop → router).
/// - `Some(Ok(result))` — pack ran, returned an `ActionResult`. Caller
///   wraps it in whatever JSON envelope its surface expects (see
///   `compiled_pack_result_to_value` for the canonical envelope).
/// - `Some(Err(error))` — pack found but execution surfaced an error.
///
/// For the case where pack name and provider registry key differ (e.g.
/// inner-loop compiled-provider packs where one provider serves many
/// action names via `__action_name`), call
/// `dispatch_compiled_provider` directly with the pre-resolved provider.
pub async fn try_dispatch_compiled_pack(
    registry: &CapabilityRegistry,
    tool_name: &str,
    parameters: HashMap<String, Value>,
    timeout_override: Option<u64>,
    ctx: &CompiledDispatchContext<'_>,
    authority: Option<&CompiledDispatchAuthority>,
) -> Option<Result<ActionResult, ExecutionError>> {
    let pack = registry.get_pack_definition(tool_name)?;
    if !matches!(pack.implementation, ImplementationType::Compiled { .. }) {
        return None;
    }
    let provider = registry.get(tool_name)?;
    Some(
        // Heap-owned, not inlined: this entry point sits under the flat-loop
        // task-local scopes and above the whole provider chain, so nesting that
        // chain's state machine inline here put its bytes in every frame between.
        Box::pin(dispatch_compiled_provider(
            provider,
            tool_name,
            parameters,
            timeout_override,
            ctx,
            authority,
        ))
        .await,
    )
}

pub struct CompiledProviderDispatch {
    pub result: ActionResult,
    pub app_result_guard: Option<CompiledAppResultGuard>,
}

/// Guard-preserving variant for owners that carry non-serializable app policy
/// metadata through their complete result lifecycle. Ordinary callers refuse
/// governed app tools before provider execution.
pub async fn try_dispatch_compiled_pack_with_guard(
    registry: &CapabilityRegistry,
    tool_name: &str,
    parameters: HashMap<String, Value>,
    timeout_override: Option<u64>,
    ctx: &CompiledDispatchContext<'_>,
    authority: Option<&CompiledDispatchAuthority>,
) -> Option<Result<CompiledProviderDispatch, ExecutionError>> {
    let pack = registry.get_pack_definition(tool_name)?;
    if !matches!(pack.implementation, ImplementationType::Compiled { .. }) {
        return None;
    }
    let provider = registry.get(tool_name)?;
    Some(
        // Heap-owned for the same reason as the non-guard entry point above.
        Box::pin(dispatch_compiled_provider_with_guard(
            provider,
            tool_name,
            parameters,
            timeout_override,
            ctx,
            authority,
        ))
        .await,
    )
}

/// Lower-level entry: dispatch a known compiled provider directly,
/// bypassing the registry lookup. Caller is responsible for having
/// already resolved the provider and verified (where it matters) that
/// the corresponding pack's `implementation.type` is `Compiled`.
///
/// `tool_routing_key` populates the synthetic `PlanStep.tool` field,
/// which drives the provider's own action-routing logic in `lower()`.
/// For chat compiled-bridge providers this is the tool name (`search_memory`,
/// etc.); for inner-loop compiled-provider packs this is the provider
/// name (e.g. `duckdb`), with the specific action selected via the
/// `__action_name` entry in `parameters`.
///
/// `timeout_override` (when `Some`) overrides the provider's
/// `default_timeout_secs()`. Inner-loop callers use this to honour
/// per-action `timeout_secs` from `NativeActionSchemaDef`.
///
/// Returns the raw `Result<ActionResult, ExecutionError>` for ordinary
/// compiled results. A handler that publishes governed app-result metadata is
/// refused here: such a caller must use the guard-preserving entry point so
/// protected bytes cannot outlive or bypass their policy side channel.
pub async fn dispatch_compiled_provider(
    provider: Arc<dyn CapabilityProvider>,
    tool_routing_key: &str,
    parameters: HashMap<String, Value>,
    timeout_override: Option<u64>,
    ctx: &CompiledDispatchContext<'_>,
    authority: Option<&CompiledDispatchAuthority>,
) -> Result<ActionResult, ExecutionError> {
    if is_governed_app_compiled_tool(tool_routing_key) {
        return Err(ExecutionError::Step(
            "governed app tool requires a guard-preserving dispatch owner".to_owned(),
        ));
    }
    let dispatch = Box::pin(dispatch_compiled_provider_with_guard(
        provider,
        tool_routing_key,
        parameters,
        timeout_override,
        ctx,
        authority,
    ))
    .await?;
    if dispatch.app_result_guard.is_some() {
        return Err(ExecutionError::Step(
            "governed app result requires a guard-preserving dispatch owner".to_owned(),
        ));
    }
    Ok(dispatch.result)
}

async fn dispatch_compiled_provider_with_guard(
    provider: Arc<dyn CapabilityProvider>,
    tool_routing_key: &str,
    parameters: HashMap<String, Value>,
    timeout_override: Option<u64>,
    ctx: &CompiledDispatchContext<'_>,
    authority: Option<&CompiledDispatchAuthority>,
) -> Result<CompiledProviderDispatch, ExecutionError> {
    let started = Instant::now();
    let input_shape = redacted_input_shape(&parameters);
    let input_fingerprint = fingerprint_input_shape(&input_shape);
    let tool_action_name = parameters
        .get("__action_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let task_id = ctx
        .task_id
        .map(str::to_string)
        .or_else(|| parameter_string(&parameters, "__task_id"));
    let execution_id = ctx
        .execution_id
        .map(str::to_string)
        .or_else(|| parameter_string(&parameters, "__execution_id"));
    let timeout_secs = timeout_override.unwrap_or_else(|| provider.default_timeout_secs());
    let session_id_owned = ctx.session_id.map(|s| s.to_string());

    // Unified path: always go through `dispatch_with_gating` even
    // when authority is `None` or `enabled: false`. The internal
    // `execute_maybe_gated` routing extracts `inner_action` and
    // executes it without ledger bookkeeping in the degraded cases,
    // so callers without RA wiring still get correct behaviour while
    // gated calls (with full RA context) charge the spend ledger.
    // This consolidation lets `CapabilityProvider::execute_direct`
    // drop its own degraded-mode `Gated` arm — every production
    // caller now routes through this unified path.
    let compiled_llm_context = compiled_llm_call_context(ctx);
    let app_owner_execution_credential = is_governed_app_compiled_tool(tool_routing_key)
        .then(|| ctx.app_owner_execution_credential.clone())
        .flatten();
    let app_result_sink = Arc::new(Mutex::new(None));
    let outcome = COMPILED_APP_RESULT_SINK
        .scope(
            Some(Arc::clone(&app_result_sink)),
            COMPILED_APP_OWNER_EXECUTION_CREDENTIAL.scope(
                app_owner_execution_credential,
                COMPILED_INVOCATION_CONTEXT.scope(
                    ctx.invocation_context.cloned(),
                    COMPILED_CALLING_PROFILE_NAME.scope(
                        ctx.calling_profile_name.map(str::to_owned),
                        // Five `TaskLocalFuture`s stack up here. Heap-owning the
                        // innermost future keeps each of their poll frames holding a
                        // pointer rather than the gated-dispatch state machine and
                        // everything it awaits. The scopes are unaffected: the future
                        // is still polled inside all five, so every task-local is set
                        // for the provider's whole run.
                        COMPILED_LLM_CALL_CONTEXT.scope(
                            compiled_llm_context,
                            Box::pin(async {
                                dispatch_with_gating(
                                    provider,
                                    parameters,
                                    tool_routing_key,
                                    timeout_secs,
                                    ctx,
                                    authority,
                                    session_id_owned,
                                )
                                .await
                            }),
                        ),
                    ),
                ),
            ),
        )
        .await;

    // Generic learning evidence is not a policy-aware hidden consumer. Even
    // its redacted shapes and result sizes can disclose app schema/query
    // information, so governed tools omit this path entirely until a labeled
    // learning adapter exists.
    if !is_governed_app_compiled_tool(tool_routing_key) {
        record_skill_invocation_result(
            ctx,
            SkillInvocationRecord {
                skill_name: tool_routing_key.to_string(),
                tool_action_name,
                task_id,
                execution_id,
                input_fingerprint,
                input_shape,
                duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                retry_count: 0,
                evidence_refs: Vec::new(),
                payload: json!({
                    "dispatch": "compiled_dispatch",
                    "tool_routing_key": tool_routing_key,
                }),
            },
            &outcome,
        );
    }

    let result = outcome?;
    let app_result_guard = app_result_sink
        .lock()
        .map_err(|_| {
            ExecutionError::Step("governed app result metadata sink is unavailable".to_owned())
        })?
        .take();
    if is_governed_app_compiled_tool(tool_routing_key) && app_result_guard.is_none() {
        return Err(ExecutionError::Step(
            "governed app provider returned without result metadata".to_owned(),
        ));
    }
    if !is_governed_app_compiled_tool(tool_routing_key) && app_result_guard.is_some() {
        return Err(ExecutionError::Step(
            "ordinary compiled provider returned governed app metadata".to_owned(),
        ));
    }
    if let Some(guard) = app_result_guard.as_ref() {
        let value = compiled_pack_result_to_value(&result).ok_or_else(|| {
            ExecutionError::Step("governed app result exceeded compiled result bounds".to_owned())
        })?;
        if !guard.binds_value(&value)? {
            return Err(ExecutionError::Step(
                "governed app result metadata did not bind the returned bytes".to_owned(),
            ));
        }
    }
    Ok(CompiledProviderDispatch {
        result,
        app_result_guard,
    })
}

/// Prepared one-shot dispatcher for a disclosure-attested deterministic local
/// app tool. Lowering is completed synchronously before resource reservation.
/// Pack `execution.spend` is retained as [`SpendGate`] and settled by the app
/// workflow through `spend_session::admit` immediately before provider I/O.
/// The owned action may contain protected bytes, so this type intentionally
/// has no `Debug`, `Clone` or serialization seam.
pub struct PreparedAttestedLocalCompiledDispatch {
    provider: Arc<dyn CapabilityProvider>,
    action: ExecutableAction,
    session_id: Option<String>,
    timeout_secs: u64,
    capability_name: String,
    canonical_parameters: HashMap<String, Value>,
    workdir: Option<PathBuf>,
    exact_pack_digest: crate::magician_v2::apps::models::AppDigest,
    implementation_identity: &'static str,
    bound_http: Option<crate::magician_v2::apps::bound_http::PreparedAppBoundHttp>,
    bound_path: Option<crate::magician_v2::apps::bound_path::PreparedAppBoundPath>,
    spend_gate: Option<crate::magician_v2::resource_authority::spend_gate::SpendGate>,
    /// The dispatch attempt this operation belongs to.
    ///
    /// Held on the prepared value rather than passed to `execute`, because
    /// `execute` takes `self` and is called after the attestation fences, where
    /// no caller context is left to consult.
    effect_id: Option<String>,
    reconciliation_projection:
        Option<crate::magician_v2::apps::reconciliation::AppReconciliationSourceProjection>,
}

impl PreparedAttestedLocalCompiledDispatch {
    pub(crate) fn with_reconciliation_projection(
        mut self,
        projection: crate::magician_v2::apps::reconciliation::AppReconciliationSourceProjection,
    ) -> Result<Self, ExecutionError> {
        let operation = self
            .canonical_parameters
            .get("operation")
            .or_else(|| self.canonical_parameters.get("__action_name"))
            .or_else(|| self.canonical_parameters.get("action"))
            .or_else(|| self.canonical_parameters.get("method"))
            .and_then(Value::as_str);
        let plan = crate::magician_v2::apps::app_tool_bind::plan_app_tool_call(
            &self.capability_name,
            operation,
            crate::magician_v2::apps::app_tool_bind::AppToolContainProfile::InProcessCompiled,
        );
        if plan.io_kind != crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundHostRead
            || !projection.matches_target(&self.capability_name, operation)
        {
            return Err(ExecutionError::Configuration(
                "reconciliation projection target mismatch".to_owned(),
            ));
        }
        self.reconciliation_projection = Some(projection);
        Ok(self)
    }

    /// Attest the exact lowered parameter set that will enter the provider.
    /// This prevents pack defaults/normalization from changing the operation
    /// after disclosure and resource identity were calculated.
    pub fn attest_app_tool_target(
        &self,
        tool_ref: crate::magician_v2::apps::models::AppReference,
    ) -> Option<crate::magician_v2::apps::tool_disclosure::AttestedAppToolTarget> {
        let operation = self
            .canonical_parameters
            .get("operation")
            .or_else(|| self.canonical_parameters.get("__action_name"))
            .or_else(|| self.canonical_parameters.get("action"))
            .or_else(|| self.canonical_parameters.get("method"))
            .and_then(|value| value.as_str());
        let plan = crate::magician_v2::apps::app_tool_bind::plan_app_tool_call(
            &self.capability_name,
            operation,
            crate::magician_v2::apps::app_tool_bind::AppToolContainProfile::InProcessCompiled,
        );
        if !plan.runnable
            || crate::magician_v2::apps::app_tool_bind::
                compiled_app_provider_implementation_identity(&self.capability_name)
                != Some(self.implementation_identity)
            || !self
                .provider
                .prove_app_tool_args(&self.canonical_parameters)
        {
            return None;
        }
        // The receipt must attest the act, not the description of it. This
        // re-plan reads the parameter map again, so it inherits whatever the
        // alias chain above picked; re-deriving the class from the lowered
        // action and refusing to mint on disagreement keeps a one-shot receipt
        // from ever vouching for a call whose real effect differs from its
        // classification. `prepare_attested_local_compiled_dispatch` refuses the
        // same mismatch earlier — this is the second, independent check at the
        // point authority is actually minted.
        if crate::magician_v2::apps::app_tool_bind::classify_lowered_action(&self.action)
            .is_some_and(|effective| effective != plan.io_kind)
        {
            return None;
        }
        if plan.io_kind == crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundHttp {
            let http = self.bound_http.as_ref()?;
            let ExecutableAction::Http(action) = &self.action else {
                return None;
            };
            return http
                .matches_action(action)
                .then(|| http.attest(tool_ref, chrono::Utc::now()))
                .flatten();
        }
        if matches!(
            plan.io_kind,
            crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundFile
                | crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundWrite
                | crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundTable
        ) {
            let owner = self.bound_path.as_ref()?;
            return owner
                .matches_action(&self.action)
                .then(|| owner.attest(tool_ref))
                .flatten();
        }
        plan.mint_with_evidence(
            tool_ref,
            Some(
                crate::magician_v2::apps::app_tool_bind::AppToolBindEvidence {
                    parameters: &self.canonical_parameters,
                    workdir: self.workdir.as_deref(),
                    now: chrono::Utc::now(),
                },
            ),
        )
    }

    pub fn exact_parameters(&self) -> Option<&HashMap<String, Value>> {
        Some(&self.canonical_parameters)
    }

    pub(crate) fn capability_name(&self) -> &str {
        &self.capability_name
    }

    pub(crate) fn take_spend_gate(
        &mut self,
    ) -> Option<crate::magician_v2::resource_authority::spend_gate::SpendGate> {
        self.spend_gate.take()
    }

    pub(crate) fn reviewed_transport_result_byte_ceiling(&self) -> Option<u64> {
        let operation = self
            .canonical_parameters
            .get("operation")
            .or_else(|| self.canonical_parameters.get("__action_name"))
            .or_else(|| self.canonical_parameters.get("action"))
            .or_else(|| self.canonical_parameters.get("method"))
            .and_then(Value::as_str);
        let plan = crate::magician_v2::apps::app_tool_bind::plan_app_tool_call(
            &self.capability_name,
            operation,
            crate::magician_v2::apps::app_tool_bind::AppToolContainProfile::InProcessCompiled,
        );
        crate::magician_v2::apps::app_tool_bind::reviewed_app_transport_result_ceiling(&plan)
    }

    pub(crate) fn reviewed_transport_input_byte_ceiling(&self) -> Option<u64> {
        let operation = self
            .canonical_parameters
            .get("operation")
            .or_else(|| self.canonical_parameters.get("__action_name"))
            .or_else(|| self.canonical_parameters.get("action"))
            .or_else(|| self.canonical_parameters.get("method"))
            .and_then(Value::as_str);
        let plan = crate::magician_v2::apps::app_tool_bind::plan_app_tool_call(
            &self.capability_name,
            operation,
            crate::magician_v2::apps::app_tool_bind::AppToolContainProfile::InProcessCompiled,
        );
        crate::magician_v2::apps::app_tool_bind::reviewed_app_transport_input_ceiling(&plan)
    }

    /// Call only after the app disclosure/current-authority and resource
    /// expiry fences have completed. No queue, resolver or learning await
    /// occurs before `CapabilityProvider::execute` is entered.
    pub async fn execute(mut self) -> Result<ActionResult, ExecutionError> {
        let projection = self.reconciliation_projection.take();
        let raw_ceiling = self.reviewed_transport_result_byte_ceiling();
        let result = Box::pin(self.execute_raw()).await?;
        match projection {
            Some(projection) => {
                project_reconciliation_source_result(result, projection, raw_ceiling)
            },
            None => Ok(result),
        }
    }

    async fn execute_raw(self) -> Result<ActionResult, ExecutionError> {
        // Read before the branches below partially move `self`.
        let effect_id = self.effect_id.clone();
        if let Some(bound_http) = self.bound_http {
            let ExecutableAction::Http(action) = &self.action else {
                return Err(ExecutionError::Configuration(
                    "protected app bound-HTTP owner retained a non-HTTP action".to_owned(),
                ));
            };
            // No idempotency key, and not an oversight: this owner admits
            // GET only (`validate_read_only_action`, enforced at both prepare
            // and execute). A lost response to a GET costs nothing to re-issue,
            // so a key would buy the caller nothing — and this path pins DNS to
            // reproduce an exact recorded request, where an extra header is a
            // change to the request's identity.
            return bound_http.execute(action).await;
        }
        if let Some(bound_path) = self.bound_path {
            if bound_path.is_table() {
                let table_action = bound_path.bound_table_action().map_err(|error| {
                    ExecutionError::Configuration(format!(
                        "protected app bound-table target failed its final descriptor fence: {error}"
                    ))
                })?;
                // Keep the move-only owner (and therefore the exact source fd)
                // alive across the provider await. DuckDB receives only a
                // `/dev/fd` or `/proc/self/fd` capability path built by the
                // owner; the model's ambient path/SQL never reaches it.
                let result = self
                    .provider
                    .execute(
                        &ExecutableAction::DuckDb(table_action),
                        self.session_id,
                        self.timeout_secs,
                    )
                    .await?;
                let encoded = serde_json::to_vec(&result).map_err(|error| {
                    ExecutionError::Step(format!(
                        "protected app bound-table result encoding failed: {error}"
                    ))
                })?;
                if u64::try_from(encoded.len()).ok().is_none_or(|size| {
                    size > crate::magician_v2::apps::bound_path::APP_BOUND_TABLE_RESULT_CEILING
                }) {
                    return Err(ExecutionError::Step(
                        "protected app bound-table result exceeded its reviewed byte ceiling"
                            .to_owned(),
                    ));
                }
                drop(bound_path);
                return Ok(result);
            }
            return tokio::task::spawn_blocking(move || bound_path.execute_file())
                .await
                .map_err(|error| {
                    ExecutionError::Step(format!(
                        "protected app bound-file worker failed: {error}"
                    ))
                })?
                .map_err(|error| {
                    ExecutionError::Step(format!(
                        "protected app bound-file target failed its descriptor fence or I/O: {error}"
                    ))
                });
        }
        if matches!(
            self.action,
            ExecutableAction::File(_) | ExecutableAction::DuckDb(_)
        ) {
            return Err(ExecutionError::Configuration(
                "protected app path/table action has no retained capability-directory owner"
                    .to_owned(),
            ));
        }
        if let Some(workdir) = self.workdir.clone() {
            let mut roots = std::collections::HashSet::new();
            roots.insert(workdir.to_string_lossy().into_owned());
            return SESSION_FILE_SANDBOX_ROOTS
                .scope(
                    Some(std::sync::Arc::new(std::sync::Mutex::new(roots))),
                    self.provider.execute_with_effect(
                        &self.action,
                        self.session_id,
                        self.timeout_secs,
                        effect_id.as_deref(),
                    ),
                )
                .await;
        }
        // `execute_with_effect`, not `execute`. This is the protected-app path:
        // the operations that reach it are the ones that send a message or
        // place an order, and it sits under `dispatch_pack_with_reliability`,
        // which re-attempts on failure. Dropping the id here would leave the
        // highest-consequence effect class as the one place a retried dispatch
        // carries nothing the far side can deduplicate on.
        self.provider
            .execute_with_effect(
                &self.action,
                self.session_id,
                self.timeout_secs,
                effect_id.as_deref(),
            )
            .await
    }
}

impl crate::magician_v2::apps::effect_kernel::AppEffectPhysicalOwner
    for PreparedAttestedLocalCompiledDispatch
{
    fn attest_effect_target(
        &self,
        tool_ref: &crate::magician_v2::apps::models::AppReference,
        primitive: &crate::magician_v2::apps::package_lock::AppLockedPrimitiveBinding,
        action: &crate::magician_v2::apps::package_lock::AppLockedPrimitiveActionBinding,
    ) -> Result<
        crate::magician_v2::apps::effect_kernel::AppEffectPhysicalTarget,
        crate::magician_v2::apps::effect_kernel::AppEffectKernelError,
    > {
        let reviewed_plan = crate::magician_v2::apps::app_tool_bind::plan_app_tool_call(
            &self.capability_name,
            Some(action.name()),
            crate::magician_v2::apps::app_tool_bind::AppToolContainProfile::InProcessCompiled,
        );
        let expected_provider_identity =
            crate::magician_v2::apps::app_tool_bind::compiled_app_provider_implementation_identity(
                &self.capability_name,
            );
        let implementation_plan_digest =
            crate::magician_v2::apps::app_tool_bind::compiled_implementation_plan_digest(
                primitive.source_content_digest(),
                &reviewed_plan,
            );
        if !crate::magician_v2::apps::app_tool_bind::app_effect_owner_supported(&reviewed_plan)
            || Some(self.implementation_identity) != expected_provider_identity
            || primitive.source_content_digest() != &self.exact_pack_digest
            || !primitive.actions().iter().any(|locked| locked == action)
            || implementation_plan_digest.as_ref() != action.implementation_plan_digest()
        {
            return Err(
                crate::magician_v2::apps::effect_kernel::AppEffectKernelError::IdentityMismatch,
            );
        }
        let target = self.attest_app_tool_target(tool_ref.clone()).ok_or(
            crate::magician_v2::apps::effect_kernel::AppEffectKernelError::IdentityMismatch,
        )?;
        Ok(
            crate::magician_v2::apps::effect_kernel::AppEffectPhysicalTarget::from_owner(
                primitive.primitive_ref().clone(),
                primitive.source_content_digest().clone(),
                target,
            ),
        )
    }
}

/// Lower one exact trusted-local operation before resource reservation. Pack
/// `execution.spend` is copied onto the prepared owner and settled by the app
/// workflow through `spend_session::admit` immediately before provider I/O.
pub(crate) async fn prepare_attested_local_compiled_dispatch(
    witness: crate::magician_v2::execution::capability::AppBuiltinProviderWitness,
    tool_routing_key: &str,
    parameters: HashMap<String, Value>,
    timeout_secs: u64,
    session_id: Option<String>,
    workdir: Option<&Path>,
    effect_id: Option<String>,
) -> Result<PreparedAttestedLocalCompiledDispatch, ExecutionError> {
    let (provider, witnessed_tool_name, exact_pack_digest, implementation_identity) =
        witness.into_parts();
    if witnessed_tool_name != tool_routing_key {
        return Err(ExecutionError::Configuration(
            "protected app provider witness names a different tool".to_owned(),
        ));
    }
    // An app call names its operation ONCE. Two disagreeing aliases let the
    // name the classifier reads differ from the one the provider acts on, which
    // is how `{operation: "get", method: "DELETE"}` classified as a read and
    // performed a delete. Refused rather than normalised — picking a winner
    // would run a call the app did not write.
    if let Some((first, second)) =
        crate::magician_v2::apps::app_tool_bind::divergent_operation_alias(&parameters)
    {
        return Err(ExecutionError::Configuration(format!(
            "protected app tool `{tool_routing_key}` names its operation twice and \
             inconsistently (`{first}` vs `{second}`); supply exactly one"
        )));
    }
    let operation = parameters
        .get("operation")
        .or_else(|| parameters.get("__action_name"))
        .or_else(|| parameters.get("action"))
        .or_else(|| parameters.get("method"))
        .and_then(Value::as_str);
    let plan = crate::magician_v2::apps::app_tool_bind::plan_app_tool_call(
        tool_routing_key,
        operation,
        crate::magician_v2::apps::app_tool_bind::AppToolContainProfile::InProcessCompiled,
    );
    let expected_provider_identity =
        crate::magician_v2::apps::app_tool_bind::compiled_app_provider_implementation_identity(
            tool_routing_key,
        );
    if !plan.runnable
        || !crate::magician_v2::apps::app_tool_bind::app_effect_owner_supported(&plan)
        || Some(implementation_identity) != expected_provider_identity
    {
        return Err(ExecutionError::Configuration(format!(
            "protected app tool `{tool_routing_key}` is not runnable"
        )));
    }
    let parameters = crate::magician_v2::apps::app_tool_bind::bind_parameters_for_call(
        &plan,
        &parameters,
        workdir,
    )
    .ok_or_else(|| {
        ExecutionError::Configuration(format!(
            "protected app tool `{tool_routing_key}` could not be bound to a closed call: a path \
             escaped the installation workdir, a required operation could not be resolved, or the \
             call supplied a free-form fragment (such as duckdb `select` / `where_clause`) that \
             cannot be contained"
        ))
    })?;
    if !provider.prove_app_tool_args(&parameters) {
        return Err(ExecutionError::Configuration(format!(
            "protected app tool `{tool_routing_key}` failed argument proof"
        )));
    }
    let step = PlanStep {
        id: format!("app-compiled-{tool_routing_key}"),
        task: format!("protected app compiled pack `{tool_routing_key}` dispatch"),
        tool: Some(tool_routing_key.to_owned()),
        parameters: parameters.clone(),
        timeout_override_secs: Some(timeout_secs),
        ..PlanStep::default()
    };
    let (action, spend_gate) = match provider.lower(&step).map_err(|error| match error {
        ExecutionError::Configuration(_) => error,
        other => ExecutionError::Step(format!("protected app compiled-pack lower failed: {other}")),
    })? {
        MaybeGatedAction::Bare(action) => (action, None),
        MaybeGatedAction::Gated(gated) => {
            let gate = gated.gate.clone();
            (gated.into_inner(), Some(gate))
        },
    };
    if !app_compiled_action_matches_tool(&action, tool_routing_key) {
        return Err(ExecutionError::Configuration(
            "protected app compiled dispatch lowered to a different action identity".to_owned(),
        ));
    }
    // The identity check above only proves the lowered action is the right KIND
    // (Http/File/...), never that it does what it was classified as doing. The
    // class was decided from the parameter map; this decides it again from the
    // lowered action — the canonical effective act — and refuses if they
    // disagree. Without it a call classified BoundHttp (a read, runnable) could
    // lower to an HTTP DELETE, or one classified BoundFile could lower to
    // FileAction::Delete.
    //
    // `None` means the lowered form cannot settle the class (duckdb's opaque
    // SQL, Pack's params); those classes are contained by closing their
    // parameter surface instead, so there is nothing to compare here.
    if let Some(effective_kind) =
        crate::magician_v2::apps::app_tool_bind::classify_lowered_action(&action)
    {
        if effective_kind != plan.io_kind {
            return Err(ExecutionError::Configuration(format!(
                "protected app tool `{tool_routing_key}` was classified as {:?} but lowers to \
                 {effective_kind:?}; the call must declare the operation it performs",
                plan.io_kind
            )));
        }
    }
    if let ExecutableAction::Http(http) = &action {
        // Redirects remain explicit in the lowered action. The physical owner
        // follows them itself so each hop is same-origin, re-resolved,
        // revalidated, and pinned before connect. A header can re-open exactly
        // the hole the method classification just closed: many servers honour
        // `X-HTTP-Method-Override` on a GET, so a call classified BoundHttp (a
        // read) would mutate at the destination.
        // `Host` re-routes the request away from the host the destination grant
        // was checked against, and `Transfer-Encoding`/`Content-Length` invite
        // request smuggling past that same check.
        //
        // A denylist rather than an allowlist: apps legitimately need arbitrary
        // auth and content headers, and an allowlist would break them. These
        // few are the ones that change WHICH request is made rather than what it
        // carries. Refused, not stripped — a silently altered call is one the
        // app did not write.
        const REFUSED_HTTP_HEADERS: &[&str] = &[
            "x-http-method-override",
            "x-http-method",
            "x-method-override",
            "host",
            "transfer-encoding",
            "content-length",
        ];
        if let Some(header) = http
            .headers
            .keys()
            .find(|name| REFUSED_HTTP_HEADERS.contains(&name.trim().to_ascii_lowercase().as_str()))
        {
            return Err(ExecutionError::Configuration(format!(
                "protected app tool `{tool_routing_key}` set header `{header}`, which can change \
                 which request is performed or where it lands; it cannot be reconciled with the \
                 approved destination and method"
            )));
        }
    }
    let canonical_parameters = match &action {
        ExecutableAction::Pack {
            resolved_params, ..
        } => resolved_params.clone(),
        _ => parameters,
    };
    let bound_http = match &action {
        ExecutableAction::Http(http)
            if plan.io_kind
                == crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundHttp =>
        {
            Some(
                crate::magician_v2::apps::bound_http::PreparedAppBoundHttp::prepare(
                    http,
                    chrono::Utc::now(),
                )
                .await
                .map_err(|error| {
                    ExecutionError::Configuration(format!(
                        "protected app bound-HTTP target could not be pinned: {error}"
                    ))
                })?,
            )
        },
        _ => None,
    };
    let bound_path = if matches!(
        plan.io_kind,
        crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundFile
            | crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundWrite
            | crate::magician_v2::apps::app_tool_bind::AppToolIoKind::BoundTable
    ) {
        let workdir = workdir.ok_or_else(|| {
            ExecutionError::Configuration(
                "protected app path/table action has no reviewed capability directory".to_owned(),
            )
        })?;
        Some(
            crate::magician_v2::apps::bound_path::PreparedAppBoundPath::prepare(
                &action,
                &canonical_parameters,
                workdir,
            )
            .map_err(|error| {
                ExecutionError::Configuration(format!(
                    "protected app path/table action could not retain an exact descriptor owner: {error}"
                ))
            })?,
        )
    } else {
        None
    };
    Ok(PreparedAttestedLocalCompiledDispatch {
        provider,
        action,
        session_id,
        timeout_secs,
        capability_name: tool_routing_key.to_owned(),
        canonical_parameters,
        workdir: workdir.map(Path::to_path_buf),
        exact_pack_digest,
        implementation_identity,
        bound_http,
        bound_path,
        spend_gate,
        effect_id,
        reconciliation_projection: None,
    })
}

fn project_reconciliation_source_result(
    result: ActionResult,
    projection: crate::magician_v2::apps::reconciliation::AppReconciliationSourceProjection,
    raw_ceiling: Option<u64>,
) -> Result<ActionResult, ExecutionError> {
    let Some(ceiling) = raw_ceiling.and_then(|value| usize::try_from(value).ok()) else {
        crate::magician_v2::apps::workflows::discard_action_result_iteratively(result);
        return Err(ExecutionError::Configuration(
            "missing reviewed source result ceiling".to_owned(),
        ));
    };
    // Check the actual provider envelope before narrowing. A projection cannot
    // hide excess transport, change the physical invocation, or truncate rows.
    if let Err(error) =
        crate::magician_v2::apps::workflows::bounded_action_result_bytes(&result, ceiling)
    {
        crate::magician_v2::apps::workflows::discard_action_result_iteratively(result);
        return Err(ExecutionError::Step(error.to_string()));
    }
    let value = compiled_pack_result_to_value(&result)
        .ok_or_else(|| ExecutionError::Step("invalid reconciliation source result".to_owned()))?;
    let projected = projection.project(value);
    let content = serde_json::to_string(&projected)
        .map_err(|error| ExecutionError::Step(error.to_string()))?;
    Ok(ActionResult::text(content))
}

fn app_compiled_action_matches_tool(action: &ExecutableAction, tool_routing_key: &str) -> bool {
    match action {
        ExecutableAction::Pack {
            capability_name, ..
        } => capability_name == tool_routing_key,
        ExecutableAction::File(_) => {
            matches!(
                tool_routing_key,
                "files" | "read_file" | "write_file" | "edit_file"
            )
        },
        ExecutableAction::Http(_) => {
            tool_routing_key == "http" || tool_routing_key.starts_with("http_")
        },
        ExecutableAction::DuckDb(_) => tool_routing_key == "duckdb",
        _ => false,
    }
}

fn compiled_llm_call_context(ctx: &CompiledDispatchContext<'_>) -> Option<CompiledLlmCallContext> {
    let workspace = ctx.workspace?;
    let scope = magicllm::LlmScope::new(ctx.principal, workspace);
    if !scope.is_valid() {
        return None;
    }
    let workload_class = if ctx.chat_session_id.is_some() {
        magicllm::LlmWorkloadClass::ForegroundChat
    } else if ctx.task_id.is_some() {
        magicllm::LlmWorkloadClass::AutonomousTask
    } else {
        magicllm::LlmWorkloadClass::InteractiveTask
    };
    let mut trace_context = magicllm::LlmTraceContext::new(scope, workload_class);
    trace_context.task_id = ctx.task_id.map(str::to_string);
    trace_context.execution_id = ctx.execution_id.map(str::to_string);
    trace_context.root_execution_id = ctx.execution_id.map(str::to_string);
    trace_context.chat_session_id = ctx.chat_session_id.map(str::to_string);
    if let Some(execution_id) = ctx
        .execution_id
        .map(str::trim)
        .filter(|execution_id| !execution_id.is_empty())
    {
        trace_context.trace_id = execution_id.to_string();
    }
    Some(CompiledLlmCallContext {
        trace_context,
        agent_id: ctx.agent_id.to_string(),
    })
}

/// Gated dispatch implementation — builds a synthetic `PlanStep`, runs
/// `provider.lower()`, and routes the `MaybeGatedAction` through
/// `execute_maybe_gated`.
///
/// Caller-supplied `active_owner` so non-agent callers don't collide
/// with real agent ids. Spend is settled through `spend_session::admit`.
async fn dispatch_with_gating(
    provider: Arc<dyn CapabilityProvider>,
    parameters: HashMap<String, Value>,
    tool_routing_key: &str,
    timeout_secs: u64,
    ctx: &CompiledDispatchContext<'_>,
    authority: Option<&CompiledDispatchAuthority>,
    session_id: Option<String>,
) -> Result<ActionResult, ExecutionError> {
    // `timeout_override_secs` on the synthetic step is set so any
    // provider whose `lower()` reads it (e.g. providers that route
    // through `execution::lowering` helpers, which propagate
    // `step.timeout_override_secs` into the lowered action's internal
    // `timeout_secs`) honours the caller-supplied per-call timeout.
    // The same value is also passed to `provider.execute(...)` below
    // so providers that read the parameter directly also see it —
    // setting both matches the pre-refactor inner-loop behaviour.
    let step = PlanStep {
        id: format!("compiled-{tool_routing_key}"),
        task: format!("compiled pack `{tool_routing_key}` dispatch"),
        tool: Some(tool_routing_key.to_string()),
        parameters,
        timeout_override_secs: Some(timeout_secs),
        ..PlanStep::default()
    };
    // Preserve phase-specific diagnostics by labelling `lower()`
    // failures explicitly at the source. `Configuration` errors pass
    // through untouched so the operator-facing remediation message
    // (e.g. budget misconfiguration produced inside `lower()`) doesn't
    // get buried in a phase prefix. The execute phase keeps its
    // underlying error shape — the JSON envelope wrapper at
    // `compiled_pack_error_value` adds the dispatch-phase prefix unless
    // the message is already phase-labelled here.
    let lowered = provider.lower(&step).map_err(|err| match err {
        ExecutionError::Configuration(_) => err,
        other => ExecutionError::Step(format!("compiled-pack lower failed: {other}")),
    })?;

    // Move (not clone) the Arc and Option<String> into the closure
    // captures. The function-level `provider` and `session_id` aren't
    // referenced after this point, so the previous `Arc::clone(&provider)`
    // and `session_id.clone()` were redundant refcount/allocation
    // churn — one extra Arc bump and one extra String alloc per call.
    let provider_for_closure = provider;
    let session_id_for_closure = session_id;
    // Owned before the `move` closure: `ctx` borrows the caller's frame and the
    // closure outlives it.
    let effect_id_for_closure = ctx.effect_id.map(str::to_string);
    let exec_inner = move |action: &ExecutableAction| {
        let provider = Arc::clone(&provider_for_closure);
        let session_id = session_id_for_closure.clone();
        let effect_id = effect_id_for_closure.clone();
        let action_clone = action.try_clone_for_retention();
        async move {
            let action_clone = action_clone.map_err(|error| {
                ExecutionError::Step(format!(
                    "compiled-pack action rejected before provider dispatch: {error}"
                ))
            })?;
            provider
                .execute_with_effect(
                    &action_clone,
                    session_id,
                    timeout_secs,
                    effect_id.as_deref(),
                )
                .await
        }
    };

    Box::pin(execute_maybe_gated(
        lowered,
        exec_inner,
        authority,
        ctx,
        tool_routing_key,
    ))
    .await
}

/// Pure routing primitive over a `MaybeGatedAction`. Callers that
/// produce a `MaybeGatedAction` from a non-`CapabilityProvider` source
/// (e.g. the skill cli-template path, which produces the gate via
/// `PackCapabilityProvider::lower()` but dispatches through the
/// CLI-template primitive dispatcher instead of `provider.execute()`)
/// reuse this primitive to avoid re-implementing the
/// reserve/commit/rollback flow.
///
/// Semantics:
/// - `MaybeGatedAction::Bare(action)` → calls `exec_inner(&action)`.
/// - `MaybeGatedAction::Gated(gated)` with `authority = None` OR
///   `authority.is_enabled() == false` → calls
///   `exec_inner(gated.inner_action())` without charging the ledger
///   (the degraded-mode fallback — same shape as
///   `CapabilityProvider::execute_direct`'s legacy behaviour). The
///   per-scope cache stays cold in this branch.
/// - `MaybeGatedAction::Gated(gated)` with enabled authority →
///   resolves the per-`(principal, workspace)` `ScopedAuthorityBundle`,
///   asks `spend_session::admit` to reserve against the bundle, then
///   runs `exec_inner` and commits or rolls back.
/// - `MaybeGatedAction::Gated(gated)` with enabled authority but a
///   missing `ctx.workspace` returns
///   `ExecutionError::Configuration("Gated dispatch ... is missing
///   workspace context ...")` rather than silently writing into a
///   wrong scope's ledger.
///
/// When the resolver returns an empty token list (no `budgets:` row
/// matches the call's (principal, agent_id, tool) tuple for the
/// gate's commodity), **fails open**: runs the inner action uncounted
/// (same posture as the disabled path) rather than rejecting, so the
/// master switch enforces only the commodities that actually have a
/// budget row and does not block every other gated tool. A `debug!`
/// line records the uncounted run (trade-off: a mistyped budget row
/// under-enforces silently instead of failing loud).
pub async fn execute_maybe_gated<F, Fut>(
    lowered: MaybeGatedAction,
    exec_inner: F,
    authority: Option<&CompiledDispatchAuthority>,
    ctx: &CompiledDispatchContext<'_>,
    tool_routing_key: &str,
) -> Result<ActionResult, ExecutionError>
where
    F: FnOnce(&ExecutableAction) -> Fut,
    Fut: std::future::Future<Output = Result<ActionResult, ExecutionError>>,
{
    // Every arm below heap-owns its `exec_inner` future. This match is the
    // widest frame on the compiled-dispatch path: with the inner futures nested
    // inline, an unoptimised build reserves room for each arm's provider-execution
    // state machine in one frame, even though exactly one arm ever runs.
    match lowered {
        MaybeGatedAction::Bare(action) => Box::pin(exec_inner(&action)).await,
        MaybeGatedAction::Gated(gated) => {
            let Some(authority) = authority else {
                // No authority wired into this caller. Degraded: run
                // inner action without ledger bookkeeping. Same
                // posture `CapabilityProvider::execute_direct` had
                // before Phase A of the layer-2 rollout; preserved
                // here so callers that legitimately have no RA
                // context (orphan tests, ad-hoc dispatchers) still
                // function.
                return Box::pin(exec_inner(gated.inner_action())).await;
            };
            // Process-wide disabled — short-circuit before resolving a
            // scope, so the per-scope cache stays cold until the
            // operator flips `resource_authority.enabled: true`.
            if !authority.is_enabled() {
                return Box::pin(exec_inner(gated.inner_action())).await;
            }
            // Workspace + principal pick the per-scope ledger / token
            // store and become path segments under
            // `<root>/scopes/{principal}/{workspace}/...`. Validate
            // both via `is_safe_scope_id` so a caller can't:
            // 1. Pass empty (`Some("")` / `""`) → would resolve to a
            //    junk scope like `<root>/scopes//workspace/...` whose
            //    state could never align with any real scope.
            // 2. Pass `..` / `/` / `\` / control chars → would let
            //    the joined path escape the scope dir.
            // Reject up front rather than create orphan state or
            // walk outside the scope tree.
            if !is_safe_scope_id(ctx.principal) {
                return Err(ExecutionError::Configuration(format!(
                    "Gated dispatch of `{tool_routing_key}` has unsafe principal id (empty, \
                     too long, or contains path separators / control characters). \
                     Spend tracking is per-(principal, workspace); the caller must \
                     populate `CompiledDispatchContext.principal` with a non-empty id \
                     ≤255 chars, free of `/`, `\\`, NUL, and control characters."
                )));
            }
            let workspace = match ctx.workspace {
                Some(w) if is_safe_scope_id(w) => w,
                Some(_) => {
                    return Err(ExecutionError::Configuration(format!(
                        "Gated dispatch of `{tool_routing_key}` has unsafe workspace id (empty, \
                         too long, or contains path separators / control characters). \
                         Spend tracking is per-(principal, workspace); the caller must \
                         populate `CompiledDispatchContext.workspace` with a non-empty id \
                         ≤255 chars, free of `/`, `\\`, NUL, and control characters."
                    )));
                },
                None => {
                    return Err(ExecutionError::Configuration(format!(
                        "Gated dispatch of `{tool_routing_key}` is missing workspace context. \
                         Spend tracking is per-(principal, workspace); the caller must \
                         populate `CompiledDispatchContext.workspace`."
                    )));
                },
            };
            let admission = admit(
                authority.resolver().as_ref(),
                SpendIntent {
                    principal: ctx.principal.to_string(),
                    workspace: workspace.to_string(),
                    agent_id: ctx.agent_id.to_string(),
                    tool_name: tool_routing_key.to_string(),
                    commodity: gated.gate.commodity.clone(),
                    amount: gated.gate.reserve_amount(),
                    missing_budget: MissingBudgetPolicy::Uncounted,
                    owner: SpendOwnerPolicy::Authorized {
                        active_owner: ctx.active_owner.clone(),
                    },
                },
            )
            .await
            .map_err(dispatch_spend_error)?;
            match admission {
                SpendAdmission::Uncounted => Box::pin(exec_inner(gated.inner_action())).await,
                SpendAdmission::Reserved(hold) => {
                    let result = Box::pin(exec_inner(gated.inner_action())).await;
                    if result.is_ok() {
                        if let Err(error) = hold.commit(None).await {
                            warn!(
                                tool = %tool_routing_key,
                                error = %error,
                                "spend gate commit failed — reservation will be stale"
                            );
                        }
                    } else if let Err(error) = hold.rollback().await {
                        warn!(
                            tool = %tool_routing_key,
                            error = %error,
                            "spend gate rollback failed — reservation will be stale"
                        );
                    }
                    result
                },
            }
        },
    }
}

fn dispatch_spend_error(error: SpendSessionError) -> ExecutionError {
    ExecutionError::Step(format!("budget gate reservation failed: {error}"))
}

/// Canonical JSON envelope for a compiled-pack `ActionResult`. Text
/// results are JSON-parsed first — most compiled providers serialise
/// their own result envelope into `ActionResult::Text { content }`, so
/// surfacing it as a nested JSON string would force the LLM to
/// re-parse. Falls back to the structural representation for the other
/// `ActionResult` variants (`Binary`, `Http`, `Bool`, `List`,
/// `Browser`, `Success`).
pub fn compiled_pack_result_to_value(result: &ActionResult) -> Option<Value> {
    if let ActionResult::Text { content } = result {
        if content.len() > MAX_RESULT_JSON_BYTES {
            return None;
        }
        if json_bytes_nesting_is_bounded(content.as_bytes(), MAX_RETAINED_JSON_DEPTH) {
            if !json_bytes_nodes_are_bounded(content.as_bytes(), MAX_RESULT_JSON_NODES) {
                // The lexical node scanner deliberately is not a JSON grammar
                // parser. Distinguish a valid over-budget document (reject)
                // from malformed text (retain the long-standing structural
                // Text fallback) without allocating a `Value` tree.
                if serde_json::from_str::<serde::de::IgnoredAny>(content).is_ok() {
                    return None;
                }
                return action_result_to_value(result);
            }
            if let Ok(value) = serde_json::from_str::<Value>(content) {
                let admitted = inspect_json_bounded(&value, MAX_RESULT_JSON_NODES)
                    .is_some_and(|metrics| metrics.max_depth <= MAX_RETAINED_JSON_DEPTH);
                if admitted {
                    return Some(value);
                }
                crate::magician_v2::json_traversal::discard_json_iteratively(value);
                return None;
            }
        }
    }
    action_result_to_value(result)
}

/// Structural JSON of an `ActionResult` (no Text-parsing shortcut).
/// Used directly by callers that emit raw tool outputs into surfaces
/// outside the compiled-pack envelope (e.g. chat runtime-tool turns).
pub fn action_result_to_value(result: &ActionResult) -> Option<Value> {
    Some(match result {
        ActionResult::Success => json!({"kind": "success"}),
        ActionResult::Text { content } => {
            if json_string_encoded_len(content).ok()?.saturating_add(32) > MAX_RESULT_JSON_BYTES {
                return None;
            }
            json!({"kind": "text", "content": content})
        },
        ActionResult::Binary { data, mime_type } => {
            let encoded_mime_bytes = mime_type
                .as_deref()
                .map(json_string_encoded_len)
                .transpose()
                .ok()?
                .unwrap_or(4); // JSON `null`
            if encoded_mime_bytes.saturating_add(64) > MAX_RESULT_JSON_BYTES {
                return None;
            }
            json!({
                "kind": "binary",
                "mime_type": mime_type,
                "size_b64": data.len(),
            })
        },
        ActionResult::Http {
            status,
            headers,
            body,
        } => {
            if headers.len() > MAX_RESULT_JSON_NODES.saturating_sub(HTTP_RESULT_ENVELOPE_NODES) {
                return None;
            }
            let estimated_bytes = headers.iter().try_fold(
                json_string_encoded_len(body).ok()?.saturating_add(96),
                |total, (key, value)| {
                    Some(
                        total
                            .saturating_add(json_string_encoded_len(key).ok()?)
                            .saturating_add(json_string_encoded_len(value).ok()?)
                            .saturating_add(2),
                    )
                },
            )?;
            if estimated_bytes > MAX_RESULT_JSON_BYTES {
                return None;
            }
            json!({
                "kind": "http",
                "status": status,
                "headers": headers,
                "body": body,
            })
        },
        ActionResult::Bool { value } => json!({"kind": "bool", "value": value}),
        ActionResult::List { items } => {
            if items.len() > MAX_RESULT_JSON_NODES
                || items.iter().try_fold(32usize, |total, item| {
                    Some(
                        total
                            .saturating_add(json_string_encoded_len(item).ok()?)
                            .saturating_add(1),
                    )
                })? > MAX_RESULT_JSON_BYTES
            {
                return None;
            }
            json!({"kind": "list", "items": items})
        },
        ActionResult::Browser { data, .. } => {
            let metrics = inspect_json_bounded(data, MAX_RESULT_JSON_NODES)?;
            if metrics.max_depth > MAX_RETAINED_JSON_DEPTH
                || exact_json_encoded_len(data) > MAX_RESULT_JSON_BYTES
            {
                return None;
            }
            clone_json_iteratively(data)
        },
    })
}

/// Canonical error envelope for tool result reporting. Callers that
/// want a consistent `{"status": "error", "tool": ..., "reason": ...}`
/// JSON shape for failed compiled-pack dispatch share this helper
/// rather than duplicating the format in every wrapper. Caller is
/// responsible for any contextualized `warn!` logging (chat adds
/// `session_id`, inner-loop adds `execution_id`, etc.) — keeping the
/// helper log-free avoids double-logging.
///
/// `ExecutionError::Configuration` is surfaced *without* the generic
/// `"compiled-pack dispatch failed:"` prefix because configuration
/// errors (e.g. the "Gated dispatch … is missing workspace context"
/// / unsafe-scope-id errors) are already self-explanatory; wrapping
/// them buries the specific remediation guidance the operator/LLM
/// needs. (Note: a missing `budgets:` row no longer errors — the gate
/// fails open and runs uncounted; see `execute_maybe_gated`.) Phase-labelled `Step` errors
/// (those whose message already begins with `compiled-pack`, e.g.
/// `"compiled-pack lower failed: …"` emitted at the source in
/// `dispatch_with_gating`) are surfaced verbatim to preserve the
/// phase distinction. Other error variants get the generic
/// `"compiled-pack dispatch failed: …"` prefix so the consumer can
/// distinguish dispatch-layer failures from upstream callers' errors.
pub fn compiled_pack_error_value(tool_name: &str, error: &ExecutionError) -> Value {
    let reason = match error {
        ExecutionError::Configuration(msg) => msg.clone(),
        ExecutionError::Step(msg) if msg.starts_with("compiled-pack ") => msg.clone(),
        other => format!("compiled-pack dispatch failed: {other}"),
    };
    json!({
        "status": "error",
        "tool": tool_name,
        "reason": reason,
    })
}

#[derive(Debug, Clone)]
pub struct SkillInvocationRecord {
    pub skill_name: String,
    pub tool_action_name: Option<String>,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub input_fingerprint: String,
    pub input_shape: Value,
    pub duration_ms: u64,
    pub retry_count: u32,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub payload: Value,
}

pub fn record_skill_invocation_result<E: std::fmt::Display>(
    ctx: &CompiledDispatchContext<'_>,
    record: SkillInvocationRecord,
    outcome: &Result<ActionResult, E>,
) {
    let Some(store) = ctx.learning_store.as_ref() else {
        return;
    };
    let principal = ctx.principal.trim();
    let Some(workspace) = ctx
        .workspace
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    if principal.is_empty() {
        return;
    }

    let (status, failure_class, error_summary, result_summary, result_success) = match outcome {
        Ok(result) if result.is_success() => (
            LearningSkillInvocationStatus::Succeeded,
            None,
            None,
            Some(action_result_evidence_summary(result)),
            Some(true),
        ),
        Ok(result) => {
            let summary = action_result_evidence_summary(result);
            (
                LearningSkillInvocationStatus::Failed,
                Some(classify_skill_invocation_failure(&summary)),
                None,
                Some(summary),
                Some(false),
            )
        },
        Err(error) => {
            let summary = truncate_for_evidence(&error.to_string(), 1000);
            let status = if matches!(
                classify_skill_invocation_failure(&summary),
                crate::magician_v2::learning::LearningSkillInvocationFailureClass::Cancelled
            ) {
                LearningSkillInvocationStatus::Cancelled
            } else {
                LearningSkillInvocationStatus::Failed
            };
            (
                status,
                Some(classify_skill_invocation_failure(&summary)),
                Some(summary),
                None,
                None,
            )
        },
    };
    let mut payload = record.payload;
    if let Some(object) = payload.as_object_mut() {
        object.insert("result_success".to_string(), json!(result_success));
    } else {
        payload = json!({
            "payload": payload,
            "result_success": result_success,
        });
    }

    let request = CreateLearningSkillInvocationEvidenceRequest {
        source: ctx.invocation_source.clone(),
        skill_name: record.skill_name.clone(),
        tool_action_name: record.tool_action_name,
        agent_id: (!ctx.agent_id.trim().is_empty()).then(|| ctx.agent_id.to_string()),
        task_id: record.task_id,
        execution_id: record.execution_id,
        chat_session_id: ctx.chat_session_id.map(str::to_string),
        input_fingerprint: record.input_fingerprint,
        input_shape: record.input_shape,
        status,
        failure_class,
        error_summary,
        result_summary,
        duration_ms: record.duration_ms,
        retry_count: record.retry_count,
        evidence_refs: record.evidence_refs,
        payload,
    };

    if let Err(error) = store.record_skill_invocation_evidence(
        LearningScope::new(principal.to_string(), workspace.to_string()),
        request,
    ) {
        warn!(
            tool = %record.skill_name,
            principal = %principal,
            workspace = %workspace,
            error = %error,
            "failed to record skill invocation learning evidence"
        );
    }
}

fn parameter_string(parameters: &HashMap<String, Value>, key: &str) -> Option<String> {
    parameters
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn action_result_evidence_summary(result: &ActionResult) -> String {
    match result {
        ActionResult::Success => "success".to_string(),
        ActionResult::Text { content } => {
            format!("text result ({} chars)", content.chars().count())
        },
        ActionResult::Binary { data, mime_type } => format!(
            "binary result ({} base64 chars, mime={})",
            data.len(),
            mime_type.as_deref().unwrap_or("unknown")
        ),
        ActionResult::Http { status, body, .. } => {
            format!(
                "http result (status={status}, body={} chars)",
                body.chars().count()
            )
        },
        ActionResult::Bool { value } => format!("bool result ({value})"),
        ActionResult::List { items } => format!("list result ({} items)", items.len()),
        ActionResult::Browser { data } => {
            let success = data.get("success").and_then(Value::as_bool);
            let field_count = data.as_object().map(|object| object.len()).unwrap_or(0);
            format!("browser result (success={success:?}, fields={field_count})")
        },
    }
}

fn truncate_for_evidence(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let end = value
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(value.len());
    format!("{}...", &value[..end])
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    fn learning_source_projection(
    ) -> crate::magician_v2::apps::reconciliation::AppReconciliationSourceProjection {
        let package: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../magician_data_v3/system/learning/app/recipes/sync-queue.json"
        )))
        .unwrap();
        let declaration: crate::magician_v2::apps::reconciliation::AppReconciliation =
            serde_json::from_value(
                package["recipe"]["nodes"]["root"]["node"]["declaration"].clone(),
            )
            .unwrap();
        declaration.source_projection(None).unwrap()
    }

    #[test]
    fn app_source_projection_retains_all_25_learning_rows_before_effect_sealing() {
        let source = json!({
            "candidates": (0..25).map(|n| json!({
                "id": format!("candidate-{n}"), "candidate_type":"test", "state":"proposed",
                "title":format!("Candidate {n}"), "summary":"Summary", "risk_level":"low",
                "source_agent_id":null, "review_required":true,
                "created_at":"2026-09-10T00:00:00Z", "updated_at":"2026-09-10T00:00:00Z",
                "evidence":{"large_unused_document":"x".repeat(4096)}
            })).collect::<Vec<_>>(),
            "next_cursor":"next-page", "scan_truncated":true
        });
        let raw = ActionResult::text(serde_json::to_string_pretty(&source).unwrap());
        let ceiling =
            crate::magician_v2::apps::app_tool_bind::APP_BOUND_LEARNING_READ_RESULT_CEILING;
        assert!(
            crate::magician_v2::apps::workflows::canonical_effect_result_bytes(
                &raw,
                ceiling as usize
            )
            .is_err()
        );
        let projected =
            project_reconciliation_source_result(raw, learning_source_projection(), Some(ceiling))
                .unwrap();
        let bytes = crate::magician_v2::apps::workflows::canonical_effect_result_bytes(
            &projected,
            ceiling as usize,
        )
        .unwrap();
        assert!(bytes.len() < 64 * 1024);
        let value = compiled_pack_result_to_value(&projected).unwrap();
        assert_eq!(value["candidates"].as_array().unwrap().len(), 25);
        assert_eq!(value["next_cursor"], "next-page");
        assert_eq!(value["scan_truncated"], true);
        for (index, row) in value["candidates"].as_array().unwrap().iter().enumerate() {
            assert_eq!(row["id"], format!("candidate-{index}"));
            assert_eq!(row["title"], format!("Candidate {index}"));
            assert_eq!(row["source_agent_id"], Value::Null);
            assert_eq!(row.as_object().unwrap().len(), 10);
            assert!(row.get("evidence").is_none());
        }
        // The normal effect owner seals exactly the projected bytes it will
        // retain. Narrowing does not create a second or mismatched receipt.
        let receipt =
            crate::magician_v2::apps::effect_kernel::AppEffectSettlementReceipt::committed_result(
                AppDigest::blake3(b"compiled-source-binding"),
                &bytes,
            )
            .unwrap();
        assert_eq!(
            receipt.result().unwrap().result_digest,
            AppDigest::blake3(&bytes)
        );
        assert_eq!(receipt.result().unwrap().result_bytes, bytes.len() as u64);
    }

    #[test]
    fn app_source_projection_cannot_hide_transport_overflow_or_change_a_tool_target() {
        let raw = ActionResult::text(
            serde_json::to_string(&json!({"candidates":[], "unused":"x".repeat(4096)})).unwrap(),
        );
        assert!(project_reconciliation_source_result(
            raw,
            learning_source_projection(),
            Some(1024)
        )
        .is_err());
        assert!(project_reconciliation_source_result(
            ActionResult::success(),
            learning_source_projection(),
            None
        )
        .is_err());
        let projection = learning_source_projection();
        assert!(projection.matches_target("internal_data", Some("list_learning_candidates")));
        assert!(!projection.matches_target("internal_data", Some("read_learning_candidate")));
        assert!(!projection.matches_target("time_math", Some("now")));
    }

    #[test]
    fn app_source_projection_rejects_deep_results_without_recursive_drop() {
        for ceiling in [None, Some(512 * 1024)] {
            let mut value = Value::Null;
            for _ in 0..20_000 {
                value = Value::Array(vec![value]);
            }
            assert!(project_reconciliation_source_result(
                ActionResult::Browser { data: value },
                learning_source_projection(),
                ceiling,
            )
            .is_err());
        }
    }

    use super::*;

    #[test]
    fn governed_tools_are_exposable_only_to_guard_preserving_local_chat() {
        for tool in [
            "app_discover",
            "app_data_query",
            "app_data_search",
            "app_data_compose",
            "app_action_compose",
            "app_memory_propose",
            "app_action_invoke",
        ] {
            assert!(compiled_tool_is_exposable_on_surface(
                tool,
                GovernedAppToolSurface::GuardPreservingLocalChat,
            ));
            assert!(!compiled_tool_is_exposable_on_surface(
                tool,
                GovernedAppToolSurface::ExternalModel,
            ));
            assert!(!compiled_tool_is_exposable_on_surface(
                tool,
                GovernedAppToolSurface::AutonomousTask,
            ));
            assert!(!compiled_tool_is_exposable_on_surface(
                tool,
                GovernedAppToolSurface::DelegatedOrHandover,
            ));
            assert!(!compiled_tool_is_exposable_on_surface(
                tool,
                GovernedAppToolSurface::AppWorkflow,
            ));
        }
        assert!(compiled_tool_is_exposable_on_surface(
            "browser",
            GovernedAppToolSurface::AppWorkflow,
        ));
    }

    #[derive(Debug)]
    struct NormalizingAppProvider;

    #[async_trait::async_trait]
    impl CapabilityProvider for NormalizingAppProvider {
        fn tool_name(&self) -> &str {
            "catchup_merge"
        }

        fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
            let mut resolved_params = step.parameters.clone();
            resolved_params
                .entry("server_default".to_owned())
                .or_insert_with(|| Value::String("frozen-before-admission".to_owned()));
            Ok(MaybeGatedAction::Bare(ExecutableAction::Pack {
                capability_name: self.tool_name().to_owned(),
                implementation: ImplementationType::Composite { steps: Vec::new() },
                resolved_params,
            }))
        }

        async fn execute(
            &self,
            _action: &ExecutableAction,
            _session_id: Option<String>,
            _timeout_secs: u64,
        ) -> Result<ActionResult, ExecutionError> {
            Ok(ActionResult::success())
        }
    }

    fn context<'a>(workspace: Option<&'a str>) -> CompiledDispatchContext<'a> {
        CompiledDispatchContext {
            principal: "principal-a",
            workspace,
            agent_id: "agent-a",
            session_id: Some("session-a"),
            task_id: Some("task-a"),
            execution_id: Some("execution-a"),
            chat_session_id: Some("chat-a"),
            invocation_context: None,
            calling_profile_name: None,
            app_owner_execution_credential: None,
            active_owner: "test:owner".to_string(),
            effect_id: None,
            invocation_source: LearningSkillInvocationSource::CompiledPack,
            learning_store: None,
        }
    }

    #[test]
    fn compiled_model_context_preserves_scope_and_product_lineage() {
        let inherited = compiled_llm_call_context(&context(Some("workspace-a")))
            .expect("valid compiled model context");
        assert_eq!(inherited.agent_id, "agent-a");
        assert_eq!(inherited.trace_context.scope.principal, "principal-a");
        assert_eq!(inherited.trace_context.scope.workspace, "workspace-a");
        assert_eq!(inherited.trace_context.task_id.as_deref(), Some("task-a"));
        assert_eq!(
            inherited.trace_context.execution_id.as_deref(),
            Some("execution-a")
        );
        assert_eq!(
            inherited.trace_context.root_execution_id.as_deref(),
            Some("execution-a")
        );
        assert_eq!(inherited.trace_context.trace_id, "execution-a");
        assert_eq!(
            inherited.trace_context.chat_session_id.as_deref(),
            Some("chat-a")
        );
        assert_eq!(
            inherited.trace_context.workload_class,
            magicllm::LlmWorkloadClass::ForegroundChat
        );
        assert!(inherited.trace_context.is_valid());
    }

    #[tokio::test]
    async fn prepared_app_dispatch_freezes_lowered_parameters_before_admission() {
        let registry = crate::magician_v2::execution::capability::CapabilityRegistry::new();
        registry.register_builtin_time_math_provider(
            Arc::new(
                crate::magician_v2::execution::compiled_providers::TimeMathCapabilityProvider::new(
                ),
            ),
            crate::magician_v2::execution::embedded_compiled_pack_yaml("time_math")
                .unwrap()
                .as_bytes(),
        );
        let prepared = prepare_attested_local_compiled_dispatch(
            registry.app_builtin_provider_witness("time_math").unwrap(),
            "time_math",
            HashMap::from([
                (
                    "operation".to_owned(),
                    Value::String("date_range".to_owned()),
                ),
                (
                    "start_date".to_owned(),
                    Value::String("2026-08-01".to_owned()),
                ),
                (
                    "end_date".to_owned(),
                    Value::String("2026-08-02".to_owned()),
                ),
            ]),
            30,
            Some("session-a".to_owned()),
            None,
            None,
        )
        .await
        .expect("trusted local app action lowers once");

        let parameters = prepared
            .exact_parameters()
            .expect("prepared action remains the exact pack action");
        assert_eq!(
            parameters.get("operation"),
            Some(&Value::String("date_range".to_owned()))
        );
    }

    #[tokio::test]
    async fn governed_dispatch_rejects_success_without_bound_result_metadata() {
        let error = match dispatch_compiled_provider_with_guard(
            Arc::new(NormalizingAppProvider),
            "app_action_compose",
            HashMap::new(),
            Some(30),
            &context(Some("workspace-a")),
            None,
        )
        .await
        {
            Ok(_) => panic!("governed success without a policy guard must fail closed"),
            Err(error) => error,
        };

        assert!(error
            .to_string()
            .contains("returned without result metadata"));
    }

    #[test]
    fn compiled_model_context_rejects_missing_or_unsafe_scope() {
        assert!(compiled_llm_call_context(&context(None)).is_none());
        let mut unsafe_context = context(Some("../escape"));
        unsafe_context.principal = "principal-a";
        assert!(compiled_llm_call_context(&unsafe_context).is_none());
    }

    #[tokio::test]
    async fn compiled_model_task_local_is_bounded_to_dispatch_scope() {
        assert!(current_compiled_llm_call_context().is_none());
        let inherited = compiled_llm_call_context(&context(Some("workspace-a")));
        COMPILED_LLM_CALL_CONTEXT
            .scope(inherited, async {
                assert_eq!(
                    current_compiled_llm_call_context()
                        .expect("scoped context")
                        .trace_context
                        .task_id
                        .as_deref(),
                    Some("task-a")
                );
            })
            .await;
        assert!(current_compiled_llm_call_context().is_none());
    }

    #[test]
    fn compiled_result_parses_bounded_json_and_preserves_malformed_text() {
        let parsed = compiled_pack_result_to_value(&ActionResult::Text {
            content: r#"{"status":"ok","items":[1,2]}"#.to_string(),
        })
        .expect("bounded JSON result");
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["items"], serde_json::json!([1, 2]));

        let malformed = ActionResult::Text {
            content: "{not-json".to_string(),
        };
        assert_eq!(
            compiled_pack_result_to_value(&malformed),
            Some(serde_json::json!({"kind": "text", "content": "{not-json"}))
        );
    }

    #[test]
    fn compiled_result_rejects_deep_browser_json_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut data = Value::Null;
                for _ in 0..10_000 {
                    data = Value::Array(vec![data]);
                }
                let result = ActionResult::Browser { data };
                assert!(action_result_to_value(&result).is_none());
                let ActionResult::Browser { data } = result else {
                    unreachable!();
                };
                crate::magician_v2::json_traversal::discard_json_iteratively(data);
            })
            .expect("small-stack result worker")
            .join()
            .expect("deep result must fail boundedly");
    }

    #[test]
    fn compiled_result_clones_wide_browser_json_without_semantic_drift() {
        let data = Value::Array(
            (0..20_000)
                .map(|index| serde_json::json!({"index": index, "ok": true}))
                .collect(),
        );
        let result = ActionResult::Browser { data };
        let cloned = action_result_to_value(&result).expect("bounded wide result");
        let ActionResult::Browser { data } = result else {
            unreachable!();
        };
        assert_eq!(cloned, data);
    }

    #[test]
    fn compiled_result_rejects_browser_json_over_the_node_budget() {
        let result = ActionResult::Browser {
            data: Value::Array((0..200_000).map(|_| Value::Null).collect()),
        };
        assert!(action_result_to_value(&result).is_none());
    }

    #[test]
    fn compiled_result_rejects_wide_text_json_before_projection() {
        let content = format!(
            "[{}]",
            std::iter::repeat("null")
                .take(MAX_RESULT_JSON_NODES)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(compiled_pack_result_to_value(&ActionResult::Text { content }).is_none());
    }

    #[test]
    fn compiled_result_admits_exact_text_node_budget() {
        let content = format!(
            "[{}]",
            std::iter::repeat("null")
                .take(MAX_RESULT_JSON_NODES - 1)
                .collect::<Vec<_>>()
                .join(",")
        );
        let admitted = compiled_pack_result_to_value(&ActionResult::Text { content })
            .expect("root plus children exactly at node ceiling");
        assert_eq!(
            admitted.as_array().map(Vec::len),
            Some(MAX_RESULT_JSON_NODES - 1)
        );
    }

    #[test]
    fn compiled_result_node_scanner_ignores_json_tokens_inside_strings() {
        let token_text = "[{null},{null}]".repeat(10_000);
        let content = serde_json::to_string(&serde_json::json!({"payload": token_text}))
            .expect("string payload");
        let admitted = compiled_pack_result_to_value(&ActionResult::Text { content })
            .expect("tokens inside a string are not nodes");
        assert!(admitted["payload"].as_str().is_some());
    }

    #[test]
    fn compiled_result_preserves_malformed_over_node_budget_as_text() {
        let content = format!(
            "[{}",
            std::iter::repeat("null")
                .take(MAX_RESULT_JSON_NODES)
                .collect::<Vec<_>>()
                .join(",")
        );
        let projected = compiled_pack_result_to_value(&ActionResult::Text {
            content: content.clone(),
        })
        .expect("malformed provider text keeps its structural fallback");
        assert_eq!(projected["kind"], "text");
        assert_eq!(projected["content"], content);
    }

    #[test]
    fn structural_result_budget_counts_json_escape_expansion() {
        let content = "\\".repeat(MAX_RESULT_JSON_BYTES / 2);
        assert!(action_result_to_value(&ActionResult::Text { content }).is_none());
    }

    #[test]
    fn binary_result_bounds_encoded_mime_before_materializing_envelope() {
        let result = ActionResult::Binary {
            data: "AA==".to_string(),
            mime_type: Some("\\".repeat(MAX_RESULT_JSON_BYTES / 2)),
        };
        assert!(action_result_to_value(&result).is_none());

        let admitted = action_result_to_value(&ActionResult::Binary {
            data: "AA==".to_string(),
            mime_type: Some("application/octet-stream".to_string()),
        })
        .expect("ordinary binary metadata remains admitted");
        assert_eq!(admitted["size_b64"], 4);
    }

    #[test]
    fn http_result_rejects_header_nodes_before_json_materialization() {
        let headers = (0..=(MAX_RESULT_JSON_NODES - HTTP_RESULT_ENVELOPE_NODES))
            .map(|index| (format!("x-{index}"), String::new()))
            .collect::<HashMap<_, _>>();
        let result = ActionResult::Http {
            status: 200,
            headers,
            body: String::new(),
        };

        assert!(action_result_to_value(&result).is_none());
    }
}
