//! Dispatch hook called by the outer executor when it encounters an
//! `ExecutableAction::Pack` whose pack uses `type: primitive`.
//!
//! Routes to:
//! - [`browser::execute_browser_action`] for the `browser` capability —
//!   constructs an [`AgentBrowserSession`] from the resolved CLI path.
//! - provider-backed inner-loop packs for tools such as DuckDB/iMessage that
//!   should keep using their Rust providers.
//! - [`capability_invoker::ScopedDeterministicCapabilityInvoker`] for
//!   YAML-driven CLI packs.
//!
//! All inner-loop dispatches go through this single entry point. New
//! inner-loop packs (gmail, csvkit, metabase_explore, duckdb, imessage) join
//! automatically once their YAML pack is registered.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::browser::trace_drain::drain_and_persist_cdp_proxy_traces;
use super::browser::{
    flat_browser_artifact_dir, get_or_create_flat_browser_session, resolve_browser_engine_plan,
    AgentBrowserSession, BrowserDispatcher, BrowserTransport, BrowserTransportCeiling,
    ConnectionMode, BUNDLED_BROWSER_ENGINE_NAME, DEFAULT_COMMAND_TIMEOUT_SECS,
    LIGHTPANDA_BROWSER_ENGINE_NAME,
};
use super::capability_invoker::{
    fold_primitive_result, record_primitive_skill_invocation, DeterministicCapabilityInvocation,
    DeterministicCapabilityInvocationSource, DeterministicCapabilityInvoker,
    ScopedDeterministicCapabilityInvoker,
};
use super::compiled_provider::dispatch_compiled_provider_primitive;
use super::exec_ctx::PrimitiveExecCtx;
use super::runner::{PrimitiveDispatcher, PrimitiveToolResult};
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::browser_engine_analytics::BrowserEngineAnalyticsContext;
use crate::magician_v2::execution::actions::ActionResult;
use crate::magician_v2::execution::capability::{CapabilityRegistry, ImplementationType};
use crate::magician_v2::execution::ExecutionError;
use crate::magician_v2::learning::LearningSkillInvocationSource;

/// Pack name reserved for the browser inner-loop dispatcher (Rust-coded).
pub const BROWSER_PACK_NAME: &str = "browser";

#[derive(Debug, Clone)]
enum BrowserApiTakeoverCandidate {
    Action {
        context: crate::magician_v2::api_mining::action_binding::ActionContext,
    },
}

#[derive(Debug, Clone)]
struct BrowserJsonGetPromotion {
    capability_id: String,
    origin: String,
    concrete_url: String,
    request_params: HashMap<String, String>,
    response_body: String,
}

#[derive(Debug, Clone)]
struct CorrelatedBrowserCapabilityPromotion {
    capability_id: String,
    origin: String,
    concrete_url: String,
    method: String,
    request_params: HashMap<String, String>,
    response_status: Option<u16>,
    response_body: Option<String>,
}

#[derive(Debug, Clone)]
enum BrowserWorkflowTakeover {
    Completed(ActionResult),
    BrowserFallback {
        result: crate::magician_v2::api_mining::workflow_replay::MixedReplayResult,
    },
}

fn provider_backed_primitive(
    capability_name: &str,
    registry: Option<Arc<CapabilityRegistry>>,
) -> Option<(String, Arc<CapabilityRegistry>)> {
    let registry = registry?;
    let provider_name = match registry
        .get_pack_definition(capability_name)?
        .implementation
    {
        ImplementationType::Primitive {
            provider_name: Some(provider_name),
            ..
        } => provider_name,
        _ => return None,
    };
    Some((provider_name, registry))
}

/// LLM-less single-primitive dispatch — the only flat-loop dispatch path for
/// `primitive`-type packs. Given a resolved primitive (`action`) and its
/// `arguments`, construct the pack's dispatcher and invoke `dispatch` ONCE,
/// returning the folded [`ActionResult`]. No nested LLM.
///
/// Routing:
/// - `provider_name: Some` → compiled-provider dispatch (duckdb / imessage / …)
/// - `provider_name: None` → CLI-template dispatch (skillshub subprocess packs)
/// - `browser` → LLM-less single-primitive dispatch via [`dispatch_browser_primitive`]
///   (per-execution agent-browser session cache + [`BrowserDispatcher`]).
///
/// Spend-gating: the compiled-provider path runs through
/// `dispatch_compiled_provider`; the CLI-template path runs through the shared
/// [`ScopedDeterministicCapabilityInvoker`], which preserves the same
/// reserve/commit/rollback gate. Packs without an `execution.spend:`
/// declaration continue to run unchanged.
pub async fn dispatch_primitive(
    pack_name: &str,
    action: &str,
    arguments: &Value,
    exec_ctx: &PrimitiveExecCtx,
    registry: Arc<CapabilityRegistry>,
    _cancellation_token: Option<CancellationToken>,
) -> Result<ActionResult, ExecutionError> {
    if pack_name == BROWSER_PACK_NAME {
        let started = Instant::now();
        let outcome = dispatch_browser_primitive(action, arguments, exec_ctx, registry).await;
        record_primitive_skill_invocation(
            exec_ctx,
            LearningSkillInvocationSource::BrowserTool,
            pack_name,
            Some(action),
            arguments,
            started,
            serde_json::json!({
                "dispatch": "primitive_browser",
            }),
            &outcome,
        );
        return outcome;
    }

    if let Some((provider_name, registry)) =
        provider_backed_primitive(pack_name, Some(registry.clone()))
    {
        // Compiled-provider primitive (duckdb / imessage / …) — spend-gating is
        // applied inside `dispatch_compiled_provider`.
        let result = dispatch_compiled_provider_primitive(
            pack_name,
            &provider_name,
            action,
            arguments,
            registry,
            exec_ctx,
        )
        .await?;
        return fold_primitive_result(pack_name, action, result);
    }

    // CLI-template subprocess pack (gmail / csvkit / agentmail-send / …) —
    // spend-gated via the shared `execute_maybe_gated` primitive.
    let invoker = ScopedDeterministicCapabilityInvoker::new(registry, exec_ctx.clone());
    invoker
        .invoke(
            DeterministicCapabilityInvocation::new(
                pack_name,
                action,
                arguments.clone(),
                DeterministicCapabilityInvocationSource::AgentTool,
            )
            // Stated on the invocation even though this invoker was built from
            // the very context that holds it. `invoke` no longer inherits the
            // id from its base, precisely so an invoker kept across attempts
            // cannot leak a stale one — which means the caller that IS inside
            // an attempt has to say so.
            .with_effect_id(exec_ctx.effect_id.clone()),
        )
        .await
        .map(|result| result.output)
}

/// LLM-less single browser primitive dispatch (Phase 6) — the flat-mode
/// equivalent of one step of the nested browser loop. Resolves the
/// agent-browser CLI + connection settings, resolves (or creates + caches) the
/// per-execution [`AgentBrowserSession`], then runs ONE `browser__<action>`
/// through [`BrowserDispatcher::dispatch`] — the same argv translator the
/// nested loop uses, minus the LLM.
///
/// The session is keyed by [`PrimitiveExecCtx::effective_browser_session_id`]
/// PLUS the owner agent's declared CEILING (`<id>--headed-headless`; no suffix
/// at all when the agent declared none), so `browser__open` then
/// `browser__click` hit the SAME tab. Teardown: the
/// `exec_ctx.browser_session_used` flag drives the outer-loop terminal handler
/// (`cleanup_browser_session_if_done` → `agent-browser close`), which closes
/// the derived thread id and every ceiling-suffixed variant of it still in the
/// cache, and evicts each one.
///
/// Session-level selectors (`connection_mode` / `cdp_url`) are read from the
/// call's top-level args (sibling to `args`) at session creation — so the FIRST
/// browser call (usually `open`) can select `headed`/`headless`/`cdp`.
/// Ordinary sessions retain the configured env/request precedence. Typed
/// retrieval handoffs are an exception: they require the exact transferred mode
/// and ignore env overrides.
///
/// A later call that names a DIFFERENT `connection_mode` still falls through to
/// the session the first call opened, and the selector is ignored — the
/// long-standing behaviour. The session key carries the agent's CEILING, not
/// the resolved transport, so it does not vary between calls of one execution.
///
/// Keying on the transport was tried and reverted: the transport is re-derived
/// every call and defaults to `cdp` when a call names none, which is the normal
/// shape (`open` names a mode, the `snapshot`/`click` after it do not). An
/// execution that chose `headless` would have keyed `<base>--cdp` on its next
/// call, missed its own session, and built a new one inside the owner's
/// signed-in Chrome.
///
/// Whatever that precedence resolves to is then held to the OWNER AGENT's
/// declared [`BrowserTransportCeiling`]
/// (`AgentDefinition::browser_transports`, empty = all three). A transport the
/// agent may not use refuses the call rather than being substituted — see
/// [`BrowserTransportCeiling::resolve`]. A typed retrieval handoff is held to
/// the same ceiling: its grant belongs to the scope, not to whichever agent is
/// holding the handoff id.
async fn dispatch_browser_primitive(
    action: &str,
    arguments: &Value,
    exec_ctx: &PrimitiveExecCtx,
    registry: Arc<CapabilityRegistry>,
) -> Result<ActionResult, ExecutionError> {
    let is_retrieval_handoff = arguments
        .get("retrieval_session_id")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    let secure_prompt_fill = action == "secure_prompt_fill";
    let workflow_takeover = if is_retrieval_handoff || secure_prompt_fill {
        None
    } else {
        attempt_browser_workflow_takeover(action, arguments, exec_ctx).await
    };
    if let Some(BrowserWorkflowTakeover::Completed(result)) = workflow_takeover.clone() {
        return Ok(result);
    }
    let workflow_fallback = match workflow_takeover {
        Some(BrowserWorkflowTakeover::BrowserFallback { result }) => Some(result),
        _ => None,
    };

    if workflow_fallback.is_none() && !is_retrieval_handoff && !secure_prompt_fill {
        if let Some(result) = attempt_browser_api_takeover(action, arguments, exec_ctx).await {
            return Ok(result);
        }
    }

    let cli_path = AgentBrowserSession::resolve_cli_path_for_scope(
        Some(&exec_ctx.storage_base_path),
        exec_ctx.principal.as_deref(),
        exec_ctx.workspace.as_deref(),
    )
    .map_err(|err| {
        ExecutionError::Step(format!(
            "agent-browser CLI not resolvable: {err}. Run `make setup-agent-browser` or set MAGICIAN_AGENT_BROWSER_CLI."
        ))
    })?;
    let retrieval_session_id = retrieval_handoff_session_id(arguments)?;
    // Parsed ONCE. It gates both arms below and names the session, and parsing
    // it twice would let an unrecognised transport refuse in one place while the
    // other had already keyed a session on it.
    let transport_ceiling = BrowserTransportCeiling::parse(&exec_ctx.browser_transports)
        .map_err(|error| ExecutionError::Step(format!("{error:#}")))?;
    let connection_mode = if let Some((_, expected_mode)) = retrieval_session_id.as_ref() {
        let mode = ConnectionMode::from_exact_request(arguments, &exec_ctx.browser_cdp_url)
            .map_err(|error| ExecutionError::Step(error.to_string()))?;
        validate_retrieval_handoff_mode(&mode, expected_mode, &exec_ctx.browser_cdp_url)?;
        // A typed handoff is a grant to the SCOPE, not to the agent holding it.
        // `handoff_session_allows` checks principal/workspace/action/domain/mode
        // and never asks which agent is calling, and a handoff id travels in the
        // delegation brief (the web-researcher persona is told to preserve
        // handoff IDs and action IDs verbatim). So without this an agent that
        // declares `browser_transports: [headless, headed]` continues an
        // `authenticated_interact` handoff and drives the owner's own signed-in
        // Chrome — the exact act the ceiling exists to refuse, reached at one
        // remove. The ceiling is a fact about the agent, so it outranks the
        // grant here just as it outranks the call and the operator override
        // below.
        transport_ceiling
            .resolve(mode)
            .map_err(|error| ExecutionError::Step(format!("{error:#}")))?
    } else {
        // The agent's declared ceiling is applied AFTER the call/env/default
        // precedence has produced one concrete transport, and it overrides all
        // three: `connection_mode` in the tool call is a request, not an
        // authorization, and `MAGICIAN_AGENT_BROWSER_MODE` is an operator
        // diagnostic that must not widen an agent. An unrecognised name in the
        // ceiling refuses the browse rather than being skipped.
        let requested = ConnectionMode::from_call_arguments(arguments, &exec_ctx.browser_cdp_url);
        transport_ceiling
            .resolve(requested)
            .map_err(|error| ExecutionError::Step(format!("{error:#}")))?
    };
    // §5A.2 — a confined execution never gets the owner's browser.
    //
    // `ConnectionMode::Cdp` attaches to the owner's real Chrome through the
    // magicutor proxy, which is the point of that mode: it "preserves the
    // user's signed-in profile". Under an engagement or a meeting that is the
    // leak in its most direct form — a research visit made for one
    // counterparty carries the owner's identity, cookies and logged-in
    // accounts, and any site it touches sees the owner rather than the
    // company. The session-id partition
    // (`PrimitiveExecCtx::effective_browser_session_id`) separates one
    // confined execution's browser from another's, but it cannot separate a
    // confined execution from a browser it did not launch.
    //
    // Refused rather than silently downgraded to headless: an act that quietly
    // ran somewhere other than where it was asked to run produces a result
    // nobody can account for, and the existing retrieval-mode note in
    // `browser/session.rs` already establishes that a transport must be chosen
    // deliberately, never inherited from an override.
    if matches!(connection_mode, ConnectionMode::Cdp { .. }) {
        // An unreadable carrier reads as confined, not as unconfined: `None`
        // here means the runtime could not tell which engagement this is, and
        // "we do not know" is never a reason to hand over the owner's Chrome.
        //
        // A `Program` carrier is confined too, and it needs saying because the
        // obvious repair gets it backwards. `engagement_ceiling_authority()`
        // truthfully answers `None` for a program — no roster ceiling exists —
        // and `contained_retrieval_scope(None)` is `Unbound`, whose
        // `is_bound()` is false. So routing a program through that accessor
        // would read as "not confined" and hand a program-scoped execution the
        // owner's signed-in Chrome, which is the exact inheritance the outward
        // boundary exists to prevent. The arms are therefore spelled out.
        let confined = match exec_ctx
            .work_authority
            .as_ref()
            .map(|carried| &carried.work)
        {
            Some(crate::magician_v2::work_context::WorkContextKind::Program(_)) => true,
            Some(crate::magician_v2::work_context::WorkContextKind::Engagement(_)) => {
                // Built here rather than through an accessor: this struct is
                // not `AgenticContext` and carries none. `try_from` cannot fail
                // on this arm — the match already proved the shape — but the
                // narrowing is chained INTO the scope read rather than answered
                // as a `None` carrier, because `contained_retrieval_scope(None)`
                // is `Unbound`. Answering `None` here would therefore read as
                // "not confined", so the impossible error would fail OPEN. Every
                // step that cannot answer collapses to `None` and
                // `unwrap_or(true)` reads that as confined.
                exec_ctx
                    .work_authority
                    .as_ref()
                    .and_then(|carried| {
                        crate::magician_v2::engagements::EngagementAuthorityRef::try_from(carried)
                            .ok()
                    })
                    .and_then(|ceiling| {
                        crate::magician_v2::engagement_retrieval::contained_retrieval_scope(Some(
                            &ceiling,
                        ))
                    })
                    .map(|scope| scope.is_bound())
                    .unwrap_or(true)
            },
            // No carrier at all: an ordinary execution, unconfined as before.
            None => false,
        };
        if confined {
            return Err(ExecutionError::Step(
                "NOT BROWSED — this execution is confined to one engagement and CDP mode \
                 attaches to the owner's own signed-in Chrome, so the visit would carry the \
                 owner's identity and cookies. Re-issue the browse in headed or headless \
                 mode, which launches a browser this engagement owns."
                    .into(),
            ));
        }
    }
    let principal = exec_ctx.principal.as_deref().unwrap_or("anonymous");
    let workspace = exec_ctx.workspace.as_deref().unwrap_or("default");
    if let Some((session_id, expected_mode)) = retrieval_session_id.as_ref() {
        let action_id = arguments.get("retrieval_action_id").and_then(Value::as_str);
        let target_url = browser_navigation_url_from_call(action, arguments);
        if !crate::magician_v2::content_sources::retrieval::global_retrieval_runtime_state()
            .handoff_session_allows(
                session_id,
                principal,
                workspace,
                expected_mode,
                action_id,
                target_url.as_deref(),
            )
        {
            return Err(ExecutionError::Step(
                "retrieval handoff is absent, stale, out of scope, or not approved for this \
                 mode/action/domain"
                    .into(),
            ));
        }
    }
    let requested_engine = requested_browser_engine(arguments, exec_ctx);
    // Mining learns from what the capture records, and Lightpanda's records no
    // request bodies at all — measured 2026-09-14 against a live search API:
    // every POST in its HAR and in `network request <id>` carries no
    // `postData`, while Chrome for Testing carries it in full. A recipe
    // compiled from that capture posts an empty body and the API rejects it,
    // so while capture is on the engine has to be one that captures.
    let api_mining_capture_enabled = api_mining_enabled_for_exec_ctx(exec_ctx);
    let requested_engine = engine_for_capture(requested_engine, api_mining_capture_enabled);
    if api_mining_capture_enabled && requested_engine == Some(BUNDLED_BROWSER_ENGINE_NAME) {
        tracing::info!(
            task_id = exec_ctx.task_id.as_deref().unwrap_or_default(),
            "[API_MINING] capture is on, so this run uses Chrome for Testing rather than an \
             engine whose capture omits request bodies"
        );
    }
    let engine_plan = resolve_browser_engine_plan(
        &exec_ctx.storage_base_path,
        principal,
        workspace,
        &connection_mode,
        requested_engine,
    )
    .map_err(|error| ExecutionError::Step(error.to_string()))?;
    // The CEILING is part of the session's identity — not the transport, and
    // that distinction is the whole correctness of this.
    //
    // `get_or_create_flat_browser_session` is keyed by this id alone and returns
    // a cached session WITHOUT re-running the builder, while a chat thread hands
    // every agent rooted on it the same id on purpose — "so every child
    // execution rooted on this thread (PA's direct calls, EA delegations,
    // web-researcher handovers) shares one Chrome window". So without a
    // namespace, a ceiling-restricted agent reuses the `cdp` window an
    // unrestricted one opened, having been refused it a line earlier.
    //
    // Keying on the resolved TRANSPORT closed that and opened something worse.
    // The transport is re-derived from the call arguments every call and
    // defaults to `cdp` when a call names none — the normal shape, since `open`
    // names a mode and the `snapshot`/`click`/`eval` after it do not. An
    // execution that chose `headless` would then have keyed `<base>--cdp` on its
    // next call, missed its own session, and built a new one inside the owner's
    // signed-in Chrome.
    //
    // The ceiling is a fact about the AGENT, identical on every call, so an
    // execution's calls all land on one session and two different ceilings can
    // never land on the same one. An unrestricted execution gets NO suffix and
    // therefore the byte-identical id it had before any of this — which is what
    // teardown, the socket-path budget and the artifact directories expect.
    //
    // A retrieval session keeps its own id untouched: that path selects the
    // authority first and carries the transport through its handoff validation.
    let session_id = retrieval_session_id
        .as_ref()
        .map(|(session_id, _)| session_id.clone())
        .unwrap_or_else(|| {
            let base = exec_ctx.effective_browser_session_id();
            match transport_ceiling.session_namespace() {
                Some(namespace) => format!("{base}--{namespace}"),
                None => base,
            }
        });
    let cleanup_cli_path = cli_path.clone();
    // A session this run did not derive from its own execution id — a chat
    // thread's shared `magician-chat-<thread>`, a retrieval handoff — is
    // released by this run's terminal cleanup too. The per-execution close
    // computes `magician-<execution>` and never reaches it, and nothing else
    // ever closes it: a finished chat task left the owner's Chrome attached,
    // its "is being debugged" bar showing, indefinitely.
    let own_session_prefix = format!(
        "magician-{}",
        super::browser::session::sanitize_session_id(&exec_ctx.thread_id())
    );
    if retrieval_session_id.is_some() || !session_id.starts_with(&own_session_prefix) {
        if let Some(session_ids) = exec_ctx.browser_additional_session_ids.as_ref() {
            session_ids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(session_id.clone());
        }
    }
    let publisher = exec_ctx.progress_publisher.clone();
    let session_id_for_build = session_id.clone();
    // Thread the opened page URL into the session so a forced reconnect re-opens
    // the REAL page rather than about:blank (the nested loop already does this).
    // Only the `open` action carries the URL; other first-actions leave it None,
    // preserving the legacy about:blank bootstrap. Defense-in-depth alongside the
    // disconnect-classifier fix: even a genuine transport reconnect must not blank
    // the live tab.
    let initial_url = if action == "open" {
        arguments
            .as_object()
            .and_then(|obj| super::browser::dispatch::open_target(obj).ok().flatten())
    } else {
        None
    };
    let handoff_capture_limit = is_retrieval_handoff
        .then_some(exec_ctx.browser_capture_limit_bytes)
        .flatten();
    let analytics_context = BrowserEngineAnalyticsContext::for_scope(
        &exec_ctx.storage_base_path,
        principal,
        workspace,
        exec_ctx.execution_id.clone(),
        exec_ctx.task_id.clone(),
    );
    let session = get_or_create_flat_browser_session(&session_id, move || {
        let mut session = AgentBrowserSession::new_with_session_id(
            session_id_for_build,
            connection_mode,
            cli_path,
        )?
        .with_progress_publisher(publisher)
        .with_engine_plan(engine_plan)
        .with_analytics_context(analytics_context)
        .with_api_mining_capture_enabled(api_mining_capture_enabled)
        .with_initial_url(initial_url);
        if let Some(limit) = handoff_capture_limit {
            session = session.with_capture_limit_bytes(limit);
        }
        Ok(session)
    })
    .map_err(|err| ExecutionError::Step(format!("failed to create AgentBrowserSession: {err}")))?;
    // The ceiling applied to the REQUESTED mode above cannot see session reuse.
    // Hold the session this call actually attached to against the same ceiling.
    // Retrieval handoffs are exempt: they own a private, mode-typed session id
    // already validated against the granted retrieval authority.
    if retrieval_session_id.is_none() {
        enforce_ceiling_on_attached_session(&exec_ctx.browser_transports, session.mode())?;
    }
    // Flag the execution as browser-using so the outer-loop terminal handler
    // fires `agent-browser close` — same contract as the nested browser loop.
    if let Some(flag) = exec_ctx.browser_session_used.as_ref() {
        flag.store(true, Ordering::SeqCst);
    }
    if secure_prompt_fill {
        // A dedicated material-bearing adapter. Never route credentials through
        // CLI argv, progress, generic browser output, replay recipes, or traces.
        return Ok(
            super::browser::secure_prompt_fill::dispatch(arguments, exec_ctx, &session).await,
        );
    }
    // Rail 2 for a Rail 1 anti-bot/network handoff. CDP may already be on the
    // authenticated page, so it can try immediately. A fresh headed/headless
    // session must execute `open` first; that post-open attempt is below.
    let recipe_continuation_after_open = workflow_fallback.is_none()
        && retrieval_session_id.is_none()
        && action == "open"
        && !matches!(session.mode(), ConnectionMode::Cdp { .. });
    if workflow_fallback.is_none()
        && retrieval_session_id.is_none()
        && matches!(session.mode(), ConnectionMode::Cdp { .. })
    {
        if let Some(result) = attempt_recipe_in_page_continuation(exec_ctx, &session).await {
            return Ok(result);
        }
    }
    let timeout_secs = registry
        .get_pack_definition(BROWSER_PACK_NAME)
        .and_then(|pack| pack.execution.and_then(|exec| exec.default_timeout_secs))
        .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS);
    // Transient staging dir so an explicit `browser__screenshot` writes a PNG the
    // dispatcher captures into `result.artifacts` — the on-demand vision path the
    // outer loop threads to the next decision (agent-browser is
    // text-snapshot-primary; screenshots are explicit, not per-iteration). The
    // PNG is read into `screenshot_storage` and deleted by the outer state
    // builder, and the whole dir is removed on session teardown, so it never
    // duplicates the canonical screenshot. Best-effort (a creation failure just
    // means no screenshot capture, not a hard failure).
    let artifact_root = {
        let dir = flat_browser_artifact_dir(&session_id);
        match std::fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(err) => {
                tracing::warn!(
                    target: "flat_loop",
                    error = %err,
                    dir = %dir.display(),
                    "failed to create flat browser staging dir; screenshots won't be captured"
                );
                None
            },
        }
    };
    let dispatcher = BrowserDispatcher::with_options(session.clone(), artifact_root, timeout_secs)
        .with_secret_context(
            exec_ctx.secret_store.clone(),
            exec_ctx.ephemeral_secret_scope_id.clone(),
        )
        .with_delivery_tracking(
            exec_ctx.delivered_secret_values.clone(),
            exec_ctx.browser_capture_withheld.clone(),
        )
        .with_yutori_translation(exec_ctx.yutori_browser_actions);

    let result = if let Some(result) = workflow_fallback {
        dispatch_workflow_browser_fallback(&result, exec_ctx, &session, &dispatcher).await
    } else {
        dispatch_browser_action_with_session(action, arguments, exec_ctx, &session, &dispatcher)
            .await
    }?;
    if recipe_continuation_after_open {
        if let Some(recipe_result) = attempt_recipe_in_page_continuation(exec_ctx, &session).await {
            return Ok(recipe_result);
        }
    }
    if let Some((session_id, "cdp")) = retrieval_session_id.as_ref() {
        validate_authenticated_handoff_location(
            action,
            arguments,
            exec_ctx,
            session_id,
            &session,
            &cleanup_cli_path,
        )
        .await?;
    }
    Ok(result)
}

async fn attempt_recipe_in_page_continuation(
    exec_ctx: &PrimitiveExecCtx,
    session: &Arc<AgentBrowserSession>,
) -> Option<ActionResult> {
    use crate::magician_v2::api_mining::origin_policy::OriginPolicyStore;
    use crate::magician_v2::api_mining::recipe_runner::{RecipeRunner, StepTransport};
    use crate::magician_v2::api_mining::recipe_store::RecipeStore;
    use crate::magician_v2::api_mining::replay_grants::ReplayGrantStore;
    use crate::magician_v2::artifact_v2::recipe_replay_hook::{
        answer_summary, has_partial_replay_handoff, recipe_context_block, take_recipe_continuation,
    };
    use crate::magician_v2::execution::primitive_dispatch::browser::in_page_fetch::InPageFetchTransport;

    let execution_id = exec_ctx.execution_id.as_deref()?;
    let continuation = take_recipe_continuation(execution_id)?;
    let base = scoped_api_mining_base_path(exec_ctx);
    if !api_mining_enabled_for_exec_ctx(exec_ctx) {
        return None;
    }
    let store = RecipeStore::new(base.clone());
    let replay_lock = store.replay_lock(&continuation.recipe_id).ok()?;
    let _replay_guard = replay_lock.lock().await;
    if !api_mining_switch_effective_for_base(&base) {
        return None;
    }
    let principal = exec_ctx
        .principal
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_WORKSPACE);
    let mut recipe = match store.load_for_scope(&continuation.recipe_id, principal, workspace) {
        Ok(Some(recipe))
            if recipe.is_read_only() && recipe.current_version == continuation.version =>
        {
            recipe
        },
        Ok(Some(_)) => {
            tracing::warn!(
                recipe_id = %continuation.recipe_id,
                "refused in-page recipe continuation because it contains a write"
            );
            return None;
        },
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                recipe_id = %continuation.recipe_id,
                %error,
                "could not reload recipe for in-page continuation"
            );
            return None;
        },
    };
    let metrics = crate::magician_v2::api_mining::recipe_metrics_for_scope(principal, workspace);
    let grants = ReplayGrantStore::open(&base);
    let policy = OriginPolicyStore::open(&base);
    let session_lookup = |origin: &str, url: &str| {
        exec_ctx
            .secret_store
            .as_ref()
            .and_then(|store| store.get_session(origin, url).map(|(session, _)| session))
    };
    let feedback_sink = exec_ctx.artifact_workspace.as_ref().and_then(|layout| {
        crate::magician_v2::api_mining::recipe_feedback::RecipeFeedbackSink::for_scope(
            layout, principal, workspace,
        )
        .map_err(|error| {
            tracing::warn!(%error, "recipe feedback unavailable for in-page continuation");
            error
        })
        .ok()
    });
    let step_feedback = |origin: &str,
                         capability_id: &str,
                         url_template: &str,
                         success: bool,
                         auth_stale: bool,
                         status: u16,
                         response_body: &str| {
        if api_mining_switch_effective_for_base(&base) {
            let Some(sink) = &feedback_sink else {
                return;
            };
            sink.record(
                origin,
                capability_id,
                url_template,
                success,
                auth_stale,
                status,
                response_body,
            );
        }
    };
    let runtime_config = crate::magician_v2::api_mining::switch::runtime_api_mining_config()?;
    if !runtime_config.recipes.enabled {
        return None;
    }
    if !runtime_config
        .recipes
        .transport_ladder
        .iter()
        .any(|transport| transport.eq_ignore_ascii_case("in_page_fetch"))
    {
        return None;
    }
    // This continuation exists only because the no-browser reqwest rail
    // already failed. Retry directly inside the authenticated page context;
    // putting reqwest first here would repeat a known-failing network call.
    let transports: Vec<Box<dyn StepTransport>> =
        vec![Box::new(InPageFetchTransport::new(Arc::clone(session)))];
    let can_continue = || api_mining_switch_effective_for_base(&base);
    let runner = RecipeRunner {
        can_continue: Some(&can_continue),
        transports,
        grants: &grants,
        origin_policy: &policy,
        session_lookup: &session_lookup,
        auth_healer: None,
        max_auth_heals: 0,
        step_feedback: Some(&step_feedback),
        observer: None,
    };
    metrics.record_replay_started();
    let replay_started_at = std::time::Instant::now();
    let result = runner.run(&mut recipe, &continuation.inputs).await;
    if !api_mining_switch_effective_for_base(&base) {
        return None;
    }
    let task_id = exec_ctx
        .task_id
        .clone()
        .unwrap_or_else(|| format!("recipe_{}", recipe.id));
    match crate::magician_v2::api_mining::recipe_runs::persist_capability_sequence(
        &base,
        &recipe,
        &result,
        &task_id,
        execution_id,
    ) {
        Ok(Some(_)) => {
            let sequence_metrics =
                crate::magician_v2::api_mining::sequence_metrics_for_scope(principal, workspace);
            sequence_metrics.record_started();
            sequence_metrics.record_finalized();
        },
        Ok(None) => {},
        Err(error) => tracing::warn!(
            recipe_id = %recipe.id,
            %error,
            "in-page replay sequence persistence failed"
        ),
    }
    let run_record = crate::magician_v2::api_mining::recipe_runs::RecipeRunRecord::from_result(
        &recipe,
        &result,
        task_id,
        execution_id.to_owned(),
        "api_then_browser",
        replay_started_at.elapsed().as_millis() as u64,
        Vec::new(),
    );
    if let Err(error) = crate::magician_v2::api_mining::recipe_runs::RecipeRunLedger::new(&base)
        .append(&run_record)
        .await
    {
        tracing::warn!(recipe_id = %recipe.id, %error, "in-page replay ledger append failed");
    }
    match store.save(&recipe) {
        Ok(()) => {
            if let Some(layout) = exec_ctx.artifact_workspace.as_ref() {
                let pack_store =
                    crate::magician_v2::execution::CapabilityPackStore::with_workspace_layout(
                        layout, principal, workspace,
                    );
                if let Err(error) =
                    crate::magician_v2::api_mining::recipe_packs::publish_recipe_pack(
                        &pack_store,
                        &layout.scope_skills_root(principal, workspace),
                        &recipe,
                    )
                {
                    tracing::warn!(
                        recipe_id = %recipe.id,
                        %error,
                        "in-page continuation could not refresh the recipe planner pack"
                    );
                }
            }
        },
        Err(error) => {
            tracing::warn!(
                recipe_id = %recipe.id,
                %error,
                "failed to save recipe after in-page continuation"
            );
        },
    }
    if result.success {
        metrics.record_replay_succeeded();
        let summary = answer_summary(&result.answer);
        set_api_replay_meta(
            exec_ctx,
            crate::magician_v2::execution::agentic::ApiReplayMeta {
                attempted: true,
                succeeded: true,
                time_ms: Some(result.steps.iter().map(|step| step.duration_ms).sum()),
                fallback_reason: None,
                replay_url: result.steps.last().map(|step| step.url.clone()),
                replay_status: result.steps.last().and_then(|step| step.status),
                replay_body: Some(summary.clone()),
            },
        );
        Some(ActionResult::Text {
            content: format!(
                "Learned API recipe {} completed in the browser context; do not repeat its calls.\n{}",
                recipe.id, summary
            ),
        })
    } else {
        metrics.record_replay_failed(
            result
                .failure
                .as_ref()
                .map(|failure| failure.class)
                .or_else(|| result.fallback.as_ref().map(|fallback| fallback.class)),
        );
        metrics.record_fallback_handoff();
        if has_partial_replay_handoff(&result) {
            result.fallback.as_ref().map(|fallback| {
                set_api_replay_meta(
                    exec_ctx,
                    crate::magician_v2::execution::agentic::ApiReplayMeta {
                        attempted: true,
                        succeeded: false,
                        time_ms: Some(result.steps.iter().map(|step| step.duration_ms).sum()),
                        fallback_reason: Some(format!("{:?}", fallback.class)),
                        replay_url: result.steps.last().map(|step| step.url.clone()),
                        replay_status: result.steps.last().and_then(|step| step.status),
                        replay_body: None,
                    },
                );
                ActionResult::Text {
                    content: recipe_context_block(&recipe.id, fallback),
                }
            })
        } else {
            None
        }
    }
}

/// The engine a run should actually use. Mining learns only from what the
/// capture records, and Lightpanda's records no request bodies at all
/// (measured 2026-09-14 against a live search API: every POST in its HAR and
/// in `network request <id>` carries no `postData`, while Chrome for Testing
/// carries it in full). A recipe compiled from such a capture posts an empty
/// body and the API rejects it, so while capture is on the engine has to be
/// one that captures. With capture off, the caller's choice stands.
fn engine_for_capture(requested: Option<&str>, capture_enabled: bool) -> Option<&str> {
    if capture_enabled && requested == Some(LIGHTPANDA_BROWSER_ENGINE_NAME) {
        return Some(BUNDLED_BROWSER_ENGINE_NAME);
    }
    requested
}

fn requested_browser_engine<'a>(
    arguments: &'a Value,
    exec_ctx: &'a PrimitiveExecCtx,
) -> Option<&'a str> {
    arguments
        .get("engine")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(exec_ctx.browser_engine.as_deref())
}

async fn validate_authenticated_handoff_location(
    action: &str,
    arguments: &Value,
    exec_ctx: &PrimitiveExecCtx,
    session_id: &str,
    session: &Arc<AgentBrowserSession>,
    cli_path: &std::path::Path,
) -> Result<(), ExecutionError> {
    if action == "close" {
        crate::magician_v2::content_sources::retrieval::global_retrieval_runtime_state()
            .revoke_handoff_session(session_id);
        return Ok(());
    }
    let current = session
        .run_command_with_options(&["get", "url"], DEFAULT_COMMAND_TIMEOUT_SECS, &[])
        .await
        .map_err(|error| {
            ExecutionError::Step(format!("verifying retrieval handoff URL: {error}"))
        })?;
    let current_url = current.stdout.trim();
    let principal = exec_ctx.principal.as_deref().unwrap_or("anonymous");
    let workspace = exec_ctx.workspace.as_deref().unwrap_or("default");
    let action_id = arguments.get("retrieval_action_id").and_then(Value::as_str);
    if current.success
        && crate::magician_v2::content_sources::retrieval::global_retrieval_runtime_state()
            .handoff_session_allows(
                session_id,
                principal,
                workspace,
                "cdp",
                action_id,
                Some(current_url),
            )
    {
        return Ok(());
    }
    let _ = crate::magician_v2::execution::primitive_dispatch::browser::close_session_by_id_with_options(
        session_id,
        cli_path,
        true,
    )
    .await;
    crate::magician_v2::content_sources::retrieval::global_retrieval_runtime_state()
        .revoke_handoff_session(session_id);
    Err(ExecutionError::Step(
        "authenticated retrieval handoff left its approved domain or its approval expired; the \
         session was released"
            .into(),
    ))
}

fn retrieval_handoff_session_id(
    arguments: &Value,
) -> Result<Option<(String, &'static str)>, ExecutionError> {
    let Some(session_id) = arguments
        .get("retrieval_session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if session_id.len() > 128
        || !session_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ExecutionError::Step(
            "retrieval_session_id is malformed".into(),
        ));
    }
    let expected_mode = if session_id.starts_with("retrieval-headless-rh_") {
        "headless"
    } else if session_id.starts_with("retrieval-headed-rh_") {
        "headed"
    } else if session_id.starts_with("retrieval-cdp-rh_") {
        "cdp"
    } else {
        return Err(ExecutionError::Step(
            "retrieval_session_id is not a typed retrieval handoff".into(),
        ));
    };
    Ok(Some((session_id.to_string(), expected_mode)))
}

fn validate_retrieval_handoff_mode(
    mode: &ConnectionMode,
    expected_mode: &str,
    configured_cdp_url: &str,
) -> Result<(), ExecutionError> {
    let actual_mode = match mode {
        ConnectionMode::Cdp { .. } => "cdp",
        ConnectionMode::Headed => "headed",
        ConnectionMode::Headless => "headless",
    };
    if actual_mode != expected_mode {
        return Err(ExecutionError::Step(format!(
            "retrieval handoff requires `{expected_mode}` mode, got `{actual_mode}`"
        )));
    }
    if matches!(mode, ConnectionMode::Cdp { url } if url != configured_cdp_url) {
        return Err(ExecutionError::Step(
            "authenticated retrieval handoffs must use the configured Magicutor CDP proxy".into(),
        ));
    }
    Ok(())
}

/// Hold the browser session this call ACTUALLY attached to against the owner
/// agent's declared [`BrowserTransportCeiling`].
///
/// [`BrowserTransportCeiling::resolve`] at the top of
/// [`dispatch_browser_primitive`] gates the transport the call *requested*. It
/// cannot gate the transport the call *gets*: [`get_or_create_flat_browser_session`]
/// is keyed by session id alone and returns a cached session without ever
/// running the builder, and a chat thread deliberately hands every agent rooted
/// on it the same `browser_session_id_override` so the live browser state
/// "survives the agent boundary". So an agent whose ceiling omits `cdp` could
/// request `headless`, pass the request-time check, and be handed the owner's
/// signed-in Chrome that an unrestricted agent opened on that thread a moment
/// earlier — arriving at the owner's identity without ever asking for it, which
/// is the whole leak the ceiling exists to stop.
///
/// Refused rather than re-attached or silently re-keyed: a live session's
/// transport is fixed at construction, and continuing in one this agent may not
/// use is exactly the unaccountable act [`BrowserTransportCeiling::resolve`]
/// refuses for the same reason.
fn enforce_ceiling_on_attached_session(
    browser_transports: &[String],
    attached: &ConnectionMode,
) -> Result<(), ExecutionError> {
    let ceiling = BrowserTransportCeiling::parse(browser_transports)
        .map_err(|error| ExecutionError::Step(format!("{error:#}")))?;
    let transport = BrowserTransport::of_connection_mode(attached);
    if ceiling.permits(transport) {
        return Ok(());
    }
    Err(ExecutionError::Step(format!(
        "NOT BROWSED — the browser session for this thread is already attached over `{}`, and \
         this agent may only use [{}].{} A live session cannot be re-attached in another \
         transport; close the browser session before browsing from this agent.",
        transport.label(),
        ceiling.label(),
        if transport.is_identity_bearing() {
            " `cdp` attaches to the owner's own signed-in Chrome, so continuing in it would carry \
             the owner's identity, cookies and logged-in accounts."
        } else {
            ""
        },
    )))
}

async fn dispatch_browser_action_with_session(
    action: &str,
    arguments: &Value,
    exec_ctx: &PrimitiveExecCtx,
    session: &Arc<AgentBrowserSession>,
    dispatcher: &BrowserDispatcher,
) -> Result<ActionResult, ExecutionError> {
    // Close-the-loop effect probe (Change 1), OFF by default
    // (`crate::config::strict_browser_interaction_gate_enabled`). When the strict
    // gate is enabled, capture a cheap digest of the page's mutable state BEFORE
    // and AFTER a *mutating* act so the harness can tell whether the act changed
    // the page (→ `BROWSER_NO_EFFECT_MARKER`). Off by default: the digest is a
    // heuristic that can false-negative on class/style mutations, and we no
    // longer dictate method over effect — so we skip the extra before/after
    // evals entirely unless explicitly benchmarking primitives.
    let probe_session = if crate::config::strict_browser_interaction_gate_enabled()
        && browser_action_mutates(action, arguments)
    {
        Some(Arc::clone(session))
    } else {
        None
    };
    let pre_effect_digest = match &probe_session {
        Some(s) => browser_effect_digest(s).await,
        None => None,
    };
    let browser_started_ms = chrono::Utc::now().timestamp_millis();
    let navigation_url = browser_navigation_url_from_call(action, arguments);
    let page_url_before = navigation_url
        .clone()
        .or_else(|| current_page_url_for_api_mining(exec_ctx));
    let api_mining_enabled = api_mining_enabled_for_exec_ctx(exec_ctx);
    let action_events = if api_mining_enabled {
        browser_action_events_for_call(
            action,
            arguments,
            page_url_before.as_deref(),
            browser_started_ms,
        )
    } else {
        Vec::new()
    };
    let dispatch_result = dispatcher.dispatch(action, arguments).await;
    let drain = if api_mining_enabled {
        drain_and_persist_cdp_proxy_traces(session, exec_ctx).await
    } else {
        Default::default()
    };
    if drain.written > 0 {
        tracing::debug!(
            target: "magician::api_mining",
            action,
            drained = drain.written,
            "drained browser network traces after primitive dispatch"
        );
    }
    let mut result = dispatch_result.map_err(|err| ExecutionError::Step(err.to_string()))?;
    let action_events_for_live_correlation = action_events.clone();
    if api_mining_enabled {
        push_api_mining_action_events(exec_ctx, action_events);
    }
    if result.success {
        if let Some(url) = navigation_url.as_ref() {
            set_current_page_url_for_api_mining(exec_ctx, url);
        }
    }
    let page_url_for_promotion = if result.success {
        navigation_url
            .clone()
            .or_else(|| current_page_url_for_api_mining(exec_ctx))
    } else {
        None
    };
    let browser_json_promotion = if api_mining_enabled && result.success {
        promote_browser_json_get_capability(
            exec_ctx,
            action,
            page_url_for_promotion.as_deref(),
            &result,
        )
    } else {
        None
    };
    let browser_action_promotion =
        if result.success && browser_json_promotion.is_none() && !drain.traces.is_empty() {
            promote_correlated_browser_action_capability(
                exec_ctx,
                &action_events_for_live_correlation,
                &drain.traces,
            )
        } else {
            None
        };
    if let Some(promotion) = browser_json_promotion.as_ref() {
        record_browser_json_capability_sequence_step(
            exec_ctx,
            action,
            arguments,
            promotion,
            result.elapsed_ms,
        );
    } else if let Some(promotion) = browser_action_promotion.as_ref() {
        record_correlated_browser_capability_sequence_step(
            exec_ctx,
            action,
            arguments,
            promotion,
            result.elapsed_ms,
        );
    } else if api_mining_enabled {
        record_browser_sequence_step(
            exec_ctx,
            action,
            arguments,
            page_url_before.as_deref(),
            result.elapsed_ms,
        );
    }
    let post_effect_digest = match &probe_session {
        Some(s) => browser_effect_digest(s).await,
        None => None,
    };
    // A non-zero agent-browser exit does NOT mean "no usable output". `batch
    // --json` exits non-zero whenever ANY subcommand fails, yet still prints the
    // FULL results array — including every successful subcommand's eval/get
    // evidence — to stdout (and nothing to stderr in --json mode); a lone
    // `eval`/`get` can likewise fail-yet-return JSON. The old code discarded that
    // structured stdout on `!success` and handed the model a bare
    // "browser__<action> failed", BLINDING it to its own page-owned evidence
    // (e.g. a connector drag whose eval proved `targetConnected:false`) — the
    // exact regression that tipped a recoverable run into give-up/gaming.
    //
    // So whenever the command produced well-formed structured output, surface it
    // as a normal observation; the per-subcommand `success:false` flags inside
    // convey the failures and the model can re-ground and retry. Only a command
    // with NO usable structured output (a genuine CLI/transport failure) becomes
    // a hard step error — and even then we prefer stdout over a generic message.
    // A `dialog accept/dismiss` that finds no dialog open is a benign no-op:
    // agent-browser auto-accepts alert/beforeunload by default, so the dialog is
    // usually gone before an explicit handle runs. Treat "No dialog is showing"
    // as a soft success so it neither `--bail`-aborts the batch nor surfaces as a
    // hard step error. (Pairs with the disconnect-classifier fix, which stops the
    // same error from re-opening the page on about:blank.)
    if !result.success
        && result
            .stderr
            .to_lowercase()
            .contains("no dialog is showing")
    {
        tracing::debug!(
            "browser dialog handle found no dialog open (already auto-accepted); treating as no-op success"
        );
        result.success = true;
        if !result.stdout.is_empty() {
            result.stdout.push('\n');
        }
        result.stdout.push_str(
            "[harness: no JS dialog was open — alert/beforeunload are auto-accepted, so the explicit dialog handle was a harmless no-op]",
        );
    }
    if !result.success && result.parsed_json.is_none() {
        let msg = if !result.stderr.trim().is_empty() {
            result.stderr.clone()
        } else if !result.stdout.trim().is_empty() {
            result.stdout.clone()
        } else {
            format!("`browser__{action}` failed")
        };
        return Err(ExecutionError::Step(msg));
    }
    // Surface the no-effect signal. Only when we have a confident before/after
    // reading AND the page did not change: the act ran but moved nothing. We
    // append it to the output so (a) the model sees it next turn and (b) the
    // executor's verification gate maps it to `ActionOutcomeCategory::NoEffect`
    // (see `BROWSER_NO_EFFECT_MARKER`). "changed" / "unknown" stay silent so the
    // existing success→PartialProgress fallback is preserved.
    if let (Some(pre), Some(post)) = (pre_effect_digest, post_effect_digest) {
        if pre == post {
            result.stdout.push('\n');
            result.stdout.push_str(BROWSER_NO_EFFECT_MARKER);
            result.stdout.push_str(
                " — this action ran but did NOT change the page. Re-read your target's actual \
                 state and switch grounding (semantic primitive, eval geometry, scroll-into-view, \
                 native `drag`) before treating this step as done.",
            );
        }
    }
    if action == "read" {
        let fallback_url = current_page_url_for_api_mining(exec_ctx);
        if let Some(document_result) =
            browser_read_document_result(&result, arguments, fallback_url)
        {
            return Ok(ActionResult::Text {
                content: document_result.to_string(),
            });
        }
    }
    Ok(browser_primitive_action_result(result))
}

/// Materialize a successful browser `read` as a content document carrying the
/// same evidence envelope `content_read` emits.
///
/// The persona sanctions a typed browser handoff as a research path, and the
/// terminal grounding judge admits evidence by the shape of that envelope — so
/// a page read here must carry it, or every fact reached through the
/// sanctioned fallback is structurally unverifiable. Eligibility is the SAME
/// per-document decision `content_read` runs
/// (`project_scalar_evidence_classification`): a genuinely opened, complete
/// page is claim-eligible; a degraded shell — login wall, error page,
/// JavaScript-required page, classified by the retrieval quality codes — is
/// emitted as `discovery_only`, still visible to the judge as coverage but
/// never admitted as a source.
///
/// `document.text` holds the full page for the raw result; `excerpt` is the
/// bounded copy that survives model projection. Before this the read was a
/// bare text scalar, and a long page was dropped whole as an oversized scalar:
/// the model itself received `{kind: "text"}` with no text.
///
/// `None` keeps the legacy text result: a failed read, a read that already
/// produced structured JSON, an empty page, or a document that fails the
/// content-source validators.
fn browser_read_document_result(
    result: &PrimitiveToolResult,
    arguments: &Value,
    fallback_url: Option<String>,
) -> Option<Value> {
    use crate::magician_v2::content_sources::{
        canonicalize_http_url, retrieval::noncontent_shell_codes, ContentDocument, ContentPrivacy,
        ContentProvenance, SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION,
    };
    use crate::magician_v2::execution::compiled_handlers::content_read::{
        opened_page_excerpt, project_scalar_evidence_classification,
    };

    if !result.success || result.parsed_json.is_some() {
        return None;
    }
    let text = result.stdout.trim();
    if text.is_empty() {
        return None;
    }
    let requested_url = browser_arg_strings(arguments)
        .into_iter()
        .find(|argument| browser_url_is_http(argument))
        .or(fallback_url);
    let canonical_url = requested_url
        .as_deref()
        .and_then(|url| canonicalize_http_url(url).ok());
    let content_hash = blake3::hash(text.as_bytes()).to_hex().to_string();
    let quality_findings = noncontent_shell_codes(text);
    let status = if quality_findings.is_empty() {
        "complete"
    } else {
        "degraded"
    };
    let title = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(200).collect::<String>())
        .or_else(|| canonical_url.clone())
        .unwrap_or_else(|| "Browser page".to_string());
    let source_label = canonical_url
        .as_deref()
        .and_then(|url| url.split("//").nth(1))
        .and_then(|rest| rest.split('/').next())
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "browser".to_string());
    let item_id = canonical_url
        .clone()
        .unwrap_or_else(|| format!("active-tab:{content_hash}"));
    let document = ContentDocument {
        schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
        identity: SourceIdentity::new("browser-read", item_id).ok()?,
        title,
        text: text.to_string(),
        canonical_url: canonical_url.clone(),
        media_type: Some("text/plain; source=browser-read".to_string()),
        fetched_at_ms: chrono::Utc::now().timestamp_millis(),
        privacy: ContentPrivacy::Public,
        content_hash,
        provenance: ContentProvenance {
            source_label,
            source_url: canonical_url,
            retrieved_by: "browser-read".to_string(),
        },
        metadata: BTreeMap::new(),
    };
    document.validate().ok()?;
    let (excerpt, excerpt_complete) = opened_page_excerpt(text);
    let mut value = json!({
        "kind": "document",
        "status": status,
        "document": document,
        "excerpt": excerpt,
        "excerpt_complete": excerpt_complete,
        "quality_findings": quality_findings,
    });
    project_scalar_evidence_classification(&mut value);
    Some(value)
}

/// Distinctive token appended to a mutating browser act's output when the
/// harness's before/after page digest shows the act produced NO change. Shared
/// with the executor's verification gate, which detects it and records the
/// iteration as `ActionOutcomeCategory::NoEffect`.
pub const BROWSER_NO_EFFECT_MARKER: &str = "[harness page-effect: NONE]";

/// Whether a browser act represents a COMPLETE page-state mutation worth a
/// before/after effect probe.
///
/// - Complete standalone mutators (`click`/`drag`/`fill`/`type`/`press`/
///   `check`/`uncheck`/`select`/`tap`/`upload`) → yes.
/// - Raw `mouse` is deliberately EXCLUDED as a standalone act: it is a gesture
///   *fragment* (`down`/`move`/`up`), so probing around a single fragment is
///   meaningless (and inserting an eval mid-gesture is risky). A complete
///   gesture is normally composed inside a `batch`.
/// - `batch` → yes IFF it contains at least one mutating sub-command (incl. the
///   raw `mouse` steps that compose a drag). The model does most of its real
///   interactions via `batch`, so without this the probe never engages. A
///   read-only batch (snapshot/get/eval/wait) is NOT probed — otherwise it
///   would be falsely flagged no-effect.
/// - Reads (`snapshot`/`get`/`eval`/`screenshot`/`is`/`find`) and navigation/
///   viewport ops carry their own result or legitimately don't change state.
fn browser_action_mutates(action: &str, arguments: &Value) -> bool {
    fn verb_mutates(verb: &str) -> bool {
        matches!(
            verb,
            "click"
                | "drag"
                | "fill"
                | "type"
                | "press"
                | "check"
                | "uncheck"
                | "select"
                | "tap"
                | "upload"
                | "mouse"
        )
    }
    match action {
        // Raw `mouse` excluded here (fragment); covered inside `batch` below.
        "click" | "drag" | "fill" | "type" | "press" | "check" | "uncheck" | "select" | "tap"
        | "upload" => true,
        "batch" => arguments
            .get("commands")
            .and_then(Value::as_array)
            .map(|cmds| {
                cmds.iter().any(|c| {
                    c.as_array()
                        .and_then(|a| a.first())
                        .and_then(Value::as_str)
                        .map(verb_mutates)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// Page-state digest used to detect whether a mutating act produced a
/// GOAL-RELEVANT effect. It hashes only page-*produced* outcome state —
/// `data-status`/`data-state`/`aria-*` attributes, state-bearing classes
/// (`connected`/`success`/`visible`/`checked`/…), form-control values, and the
/// text of result/status/summary regions — and deliberately EXCLUDES raw
/// `innerText`. This is the key v2 refinement: a near-miss interaction (e.g. a
/// drag that lands on a container instead of the precise drop target) still
/// perturbs the board and grows append-only event-trace logs, which a raw-text
/// digest would read as "changed". Keying on success-state instead means such a
/// near-miss reads as NO change → the act is flagged no-effect → the model gets
/// "that didn't connect" and re-grounds (e.g. targets the inner element).
///
/// Returns `None` (treated as "unknown" → no no-effect claim) on any probe
/// failure AND when the page exposes no status-bearing state at all (the JS
/// returns `null`), so pages without outcome state never produce false
/// no-effect flags.
async fn browser_effect_digest(session: &Arc<AgentBrowserSession>) -> Option<i64> {
    const DIGEST_JS: &str = "(()=>{let p=[];let \
         f=0;document.querySelectorAll('[data-status],[data-state],[aria-valuenow],[aria-checked],\
         [aria-selected],[aria-pressed]').forEach(e=>{f++;['data-status','data-state','\
         aria-valuenow','aria-checked','aria-selected','aria-pressed'].forEach(a=>{let \
         v=e.getAttribute(a);if(v!=null)p.push(a+'='+v);});});const \
         SC=['connected','success','complete','completed','active','done','visible','checked','\
         passed','failed','error','selected','valid','invalid','expanded'];document.\
         querySelectorAll('*').forEach(e=>{if(e.classList&&e.classList.length){SC.\
         forEach(c=>{if(e.classList.contains(c)){f++;p.push((e.id||e.tagName)+'.'+c);}});}});\
         document.querySelectorAll('input,textarea,select').forEach(e=>{f++;p.push('v:'+(e.\
         value||'')+(e.checked?'C':''));});document.querySelectorAll('[class*=result],[class*\
         =status],[class*=summary],[id*=result],[id*=status],[id*=summary]').forEach(e=>{f++;p.\
         push('t:'+(e.textContent||'').trim().slice(0,160));});if(f===0)return null;let \
         s=p.join('|');let h=0;for(let i=0;i<s.length;i++){h=(h*31+s.charCodeAt(i))|0;}return \
         h;})()";
    let result = session.run_command(&["eval", DIGEST_JS]).await.ok()?;
    if !result.success {
        return None;
    }
    if let Some(value) = result.parsed_json.as_ref() {
        if let Some(n) = value.as_i64() {
            return Some(n);
        }
        if let Some(n) = value.get("result").and_then(serde_json::Value::as_i64) {
            return Some(n);
        }
    }
    result.stdout.trim().parse::<i64>().ok()
}

/// Fold a browser primitive's result into an [`ActionResult`]. When the
/// primitive produced a screenshot artifact (an explicit `browser__screenshot`),
/// carry its on-disk path via [`ActionResult::Browser`] so the outer loop can
/// persist it and thread it to the next decision as an image (Phase 6 on-demand
/// vision). Text primitives (e.g. `browser__snapshot`'s accessibility tree) flow
/// through as [`ActionResult::Text`].
fn browser_primitive_action_result(result: PrimitiveToolResult) -> ActionResult {
    let screenshot_path = result
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "screenshot")
        .and_then(|artifact| artifact.effective_path())
        .map(|path| path.display().to_string());
    match screenshot_path {
        Some(path) => ActionResult::Browser {
            data: serde_json::json!({
                "output": result.stdout,
                "screenshot_path": path,
            }),
        },
        None => ActionResult::Text {
            content: result.stdout,
        },
    }
}

async fn dispatch_workflow_browser_fallback(
    result: &crate::magician_v2::api_mining::workflow_replay::MixedReplayResult,
    exec_ctx: &PrimitiveExecCtx,
    session: &Arc<AgentBrowserSession>,
    dispatcher: &BrowserDispatcher,
) -> Result<ActionResult, ExecutionError> {
    let Some(fallback) = result.fallback.as_ref() else {
        return Ok(workflow_replay_action_result(result));
    };
    let Some(browser) = fallback.browser.as_ref() else {
        return Ok(workflow_replay_action_result(result));
    };

    tracing::info!(
        target: "magician::api_mining",
        workflow_id = %result.workflow_id,
        origin = %result.origin_key,
        step_id = %fallback.step_id,
        action = %browser.action,
        reason = %fallback.reason,
        "api_mining.workflow_replay.browser_fallback_dispatch"
    );

    dispatch_browser_action_with_session(
        &browser.action,
        &browser.arguments,
        exec_ctx,
        session,
        dispatcher,
    )
    .await
}

async fn attempt_browser_workflow_takeover(
    action: &str,
    arguments: &Value,
    exec_ctx: &PrimitiveExecCtx,
) -> Option<BrowserWorkflowTakeover> {
    let (decision, _metrics) = route_browser_api_candidate(action, arguments, exec_ctx)?;
    let (trigger_capability_id, trigger_origin, trigger_url) = match decision {
        crate::magician_v2::api_mining::router::RouteDecision::Replay {
            capability_id,
            origin,
            request,
            ..
        } => (capability_id, origin, request.url),
        crate::magician_v2::api_mining::router::RouteDecision::ReplayRequiresHitl { .. } => {
            return None;
        },
        _ => return None,
    };

    let base_path = scoped_api_mining_base_path(exec_ctx);
    let store =
        crate::magician_v2::api_mining::workflow_store::WorkflowStore::new(base_path.clone());
    let mut workflows = match store.list(&trigger_origin) {
        Ok(workflows) => workflows
            .into_iter()
            .filter(|workflow| {
                workflow_is_safe_live_takeover_candidate(workflow, &trigger_capability_id)
            })
            .collect::<Vec<_>>(),
        Err(err) => {
            tracing::warn!(
                target: "magician::api_mining",
                origin = %trigger_origin,
                error = %err,
                "api_mining.workflow_replay.workflow_list_failed"
            );
            return None;
        },
    };
    if workflows.is_empty() {
        return None;
    }
    sort_workflow_takeover_candidates(&mut workflows);

    let principal = exec_ctx
        .principal
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_WORKSPACE);
    for mut workflow in workflows {
        if let Some(reason) = live_workflow_policy_block(&base_path, &workflow) {
            tracing::debug!(
                target: "magician::api_mining",
                workflow_id = %workflow.id,
                origin = %workflow.origin_key,
                reason = %reason,
                "api_mining.workflow_replay.skipped_by_policy"
            );
            continue;
        }

        let runner =
            match crate::magician_v2::api_mining::replay::ApiRunner::with_base_path(&base_path) {
                Ok(runner) => Arc::new(tokio::sync::Mutex::new(runner)),
                Err(err) => {
                    tracing::warn!(
                        target: "magician::api_mining",
                        workflow_id = %workflow.id,
                        error = %err,
                        "api_mining.workflow_replay.runner_init_failed"
                    );
                    continue;
                },
            };
        let executor = Arc::new(
            crate::magician_v2::api_mining::workflow_replay::step_executor::StepExecutor::new(
                runner,
            )
            .with_effect_id(exec_ctx.effect_id.clone()),
        );
        let replay_metrics =
            crate::magician_v2::api_mining::replay_metrics_for_scope(principal, workspace);
        let engine =
            crate::magician_v2::api_mining::workflow_replay::WorkflowReplayEngine::new(executor)
                .with_metrics(replay_metrics);
        let inputs = crate::magician_v2::api_mining::workflow_replay::ReplayInputs {
            timeout_ms: Some(30_000),
            ..Default::default()
        };
        let session_ctx = session_context_for_replay(exec_ctx, &trigger_url);
        let result = engine
            .replay_until_browser_fallback(&mut workflow, &inputs, HashMap::new(), &session_ctx)
            .await;

        if let Err(err) = store.save(&workflow) {
            tracing::warn!(
                target: "magician::api_mining",
                workflow_id = %workflow.id,
                error = %err,
                "api_mining.workflow_replay.save_after_live_replay_failed"
            );
        }

        record_workflow_api_sequence_steps(exec_ctx, &result);
        set_workflow_api_replay_meta(exec_ctx, &result);

        if result.success {
            tracing::info!(
                target: "magician::api_mining",
                workflow_id = %result.workflow_id,
                origin = %result.origin_key,
                steps = result.steps.len(),
                "api_mining.workflow_replay.live_completed"
            );
            return Some(BrowserWorkflowTakeover::Completed(
                workflow_replay_action_result(&result),
            ));
        }

        if result.fallback_required {
            return Some(BrowserWorkflowTakeover::BrowserFallback { result });
        }

        tracing::warn!(
            target: "magician::api_mining",
            workflow_id = %result.workflow_id,
            origin = %result.origin_key,
            failure = ?result.failure,
            "api_mining.workflow_replay.live_failed_without_fallback"
        );
        return Some(BrowserWorkflowTakeover::Completed(
            workflow_replay_action_result(&result),
        ));
    }

    None
}

fn workflow_is_safe_live_takeover_candidate(
    workflow: &crate::magician_v2::api_mining::workflow::WorkflowGraph,
    trigger_capability_id: &str,
) -> bool {
    if workflow.steps.len() < 2 {
        return false;
    }
    if !workflow
        .steps
        .iter()
        .all(|step| step.browser_fallback.is_some())
    {
        return false;
    }
    workflow
        .steps
        .first()
        .and_then(|step| step.capability_id.as_deref())
        == Some(trigger_capability_id)
}

fn sort_workflow_takeover_candidates(
    workflows: &mut [crate::magician_v2::api_mining::workflow::WorkflowGraph],
) {
    workflows.sort_by(|left, right| {
        workflow_maturity_rank(right.confidence.workflow_level)
            .cmp(&workflow_maturity_rank(left.confidence.workflow_level))
            .then_with(|| {
                right
                    .replay_stats
                    .successful_replays
                    .cmp(&left.replay_stats.successful_replays)
            })
            .then_with(|| right.steps.len().cmp(&left.steps.len()))
            .then_with(|| right.last_compiled_at_ms.cmp(&left.last_compiled_at_ms))
    });
}

fn workflow_maturity_rank(
    maturity: crate::magician_v2::api_mining::workflow::WorkflowMaturity,
) -> u8 {
    match maturity {
        crate::magician_v2::api_mining::workflow::WorkflowMaturity::Draft => 0,
        crate::magician_v2::api_mining::workflow::WorkflowMaturity::Candidate => 1,
        crate::magician_v2::api_mining::workflow::WorkflowMaturity::Validated => 2,
        crate::magician_v2::api_mining::workflow::WorkflowMaturity::Trusted => 3,
    }
}

fn live_workflow_policy_block(
    base_path: &std::path::Path,
    workflow: &crate::magician_v2::api_mining::workflow::WorkflowGraph,
) -> Option<String> {
    let registry =
        crate::magician_v2::api_mining::registry::CapabilityRegistry::with_base_path(base_path)
            .ok();
    let store = crate::magician_v2::api_mining::capability_store::CapabilityStore::with_base_path(
        base_path,
    );
    let origin_policy =
        crate::magician_v2::api_mining::origin_policy::OriginPolicyStore::open(base_path);

    for step in &workflow.steps {
        let Some(capability_id) = step.capability_id.as_deref() else {
            continue;
        };
        let capability = registry
            .as_ref()
            .and_then(|registry| {
                registry
                    .get_capability(&workflow.origin_key, capability_id)
                    .ok()
            })
            .or_else(|| {
                store.load_all_origins().ok().and_then(|origins| {
                    origins
                        .into_values()
                        .flatten()
                        .find(|capability| capability.id == capability_id)
                })
            });
        let Some(capability) = capability else {
            continue;
        };
        let side_effects = capability.effective_side_effects();
        match origin_policy.check_live_replay(
            &capability.origin,
            &side_effects,
            &capability.confidence,
        ) {
            crate::magician_v2::api_mining::origin_policy::OriginReplayCheck::Allowed => {},
            crate::magician_v2::api_mining::origin_policy::OriginReplayCheck::RequiresHitl {
                reason,
            } => {
                return Some(format!(
                    "workflow {} step {} capability {} requires HITL: {}",
                    workflow.id, step.id, capability_id, reason
                ));
            },
            crate::magician_v2::api_mining::origin_policy::OriginReplayCheck::Denied { reason } => {
                return Some(format!(
                    "workflow {} step {} capability {} denied: {}",
                    workflow.id, step.id, capability_id, reason
                ));
            },
        }
    }

    None
}

fn record_workflow_api_sequence_steps(
    exec_ctx: &PrimitiveExecCtx,
    result: &crate::magician_v2::api_mining::workflow_replay::MixedReplayResult,
) {
    for step in &result.steps {
        if step.skipped {
            continue;
        }
        let (Some(capability_id), Some(method), Some(url), Some(status)) = (
            step.capability_id.as_deref(),
            step.replay_method.as_deref(),
            step.replay_url.as_deref(),
            step.http_status,
        ) else {
            continue;
        };
        let replay_request = crate::magician_v2::api_mining::replay::ReplayRequest {
            method: method.to_string(),
            url: url.to_string(),
            headers: HashMap::new(),
            timeout_ms: 0,
            body: step.request_body.clone(),
        };
        let replay_result = crate::magician_v2::api_mining::types::ReplayResult {
            success: (200..300).contains(&status),
            capability_id: capability_id.to_string(),
            replay_method: "workflow_reqwest".to_string(),
            status,
            response_headers: None,
            response_body: step.response_body_preview.clone(),
            timing_ms: step.duration_ms,
            verification: None,
            error: None,
            fallback_reason: None,
            auth_failure: false,
        };
        record_api_sequence_step(
            exec_ctx,
            &result.origin_key,
            capability_id,
            &replay_request,
            step.request_params.clone(),
            &replay_result,
        );
    }
}

fn set_workflow_api_replay_meta(
    exec_ctx: &PrimitiveExecCtx,
    result: &crate::magician_v2::api_mining::workflow_replay::MixedReplayResult,
) {
    let elapsed_ms = (result.finished_at_ms - result.started_at_ms)
        .max(0)
        .try_into()
        .ok();
    let last_http_step = result
        .steps
        .iter()
        .rev()
        .find(|step| step.replay_url.is_some() || step.http_status.is_some());
    let fallback_reason = if result.success {
        None
    } else if let Some(fallback) = result.fallback.as_ref() {
        Some(format!(
            "workflow {} fell back to browser at step {}: {}",
            result.workflow_id, fallback.step_id, fallback.reason
        ))
    } else {
        result.failure.as_ref().map(ToString::to_string)
    };
    set_api_replay_meta(
        exec_ctx,
        crate::magician_v2::execution::agentic::ApiReplayMeta {
            attempted: true,
            succeeded: result.success,
            time_ms: elapsed_ms,
            fallback_reason,
            replay_url: last_http_step.and_then(|step| step.replay_url.clone()),
            replay_status: last_http_step.and_then(|step| step.http_status),
            replay_body: last_http_step.and_then(|step| step.response_body_preview.clone()),
        },
    );
}

fn workflow_replay_action_result(
    result: &crate::magician_v2::api_mining::workflow_replay::MixedReplayResult,
) -> ActionResult {
    ActionResult::Browser {
        data: json!({
            "success": result.success,
            "api_replayed": result.success,
            "workflow_replayed": true,
            "workflow_id": result.workflow_id,
            "origin": result.origin_key,
            "steps": result.steps,
            "fallback_required": result.fallback_required,
            "fallback": result.fallback,
            "failure": result.failure,
            "started_at_ms": result.started_at_ms,
            "finished_at_ms": result.finished_at_ms,
        }),
    }
}

fn browser_arg_strings(arguments: &Value) -> Vec<String> {
    arguments
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn browser_batch_commands(arguments: &Value) -> Vec<Vec<String>> {
    arguments
        .get("commands")
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(Value::as_array)
                .map(|argv| {
                    argv.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .filter(|argv| !argv.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn browser_command_can_bind_to_api(command: &str) -> bool {
    matches!(
        command,
        "click" | "dblclick" | "fill" | "type" | "press" | "select" | "check" | "uncheck" | "tap"
    )
}

fn browser_command_action_type(command: &str) -> Option<&'static str> {
    match command {
        "click" | "dblclick" | "tap" => Some("Click"),
        "fill" | "type" => Some("Type"),
        "press" => Some("Press"),
        "select" => Some("Select"),
        "check" | "uncheck" => Some("Toggle"),
        _ => None,
    }
}

fn page_context_from_url(page_url: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(page_url) = page_url else {
        return (None, None);
    };
    let origin = crate::magician_v2::api_mining::router::extract_origin(page_url);
    let origin = (!origin.is_empty()).then_some(origin);
    let path_template = url::Url::parse(page_url)
        .ok()
        .map(|url| path_template_for_url(&url));
    (origin, path_template)
}

fn path_template_for_url(url: &url::Url) -> String {
    let segments = url
        .path_segments()
        .map(|segments| {
            segments
                .map(|segment| {
                    if looks_like_runtime_path_segment(segment) {
                        "{id}".to_string()
                    } else {
                        segment.to_string()
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut path = if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    };
    if path.is_empty() {
        path.push('/');
    }
    path
}

fn looks_like_runtime_path_segment(segment: &str) -> bool {
    if segment.is_empty() {
        return false;
    }
    let all_digits = segment.chars().all(|ch| ch.is_ascii_digit());
    let uuidish = segment.len() >= 16
        && segment
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || ch == '-')
        && segment.contains('-');
    all_digits || uuidish
}

fn browser_command_action_context(
    command: &str,
    args: &[String],
    page_url: Option<&str>,
) -> Option<crate::magician_v2::api_mining::action_binding::ActionContext> {
    if command == "eval" {
        return args
            .first()
            .and_then(|script| browser_eval_action_context(script, page_url));
    }

    if !browser_command_can_bind_to_api(command) {
        return None;
    }
    let action_type = browser_command_action_type(command)?.to_string();
    let target = args.first().cloned().unwrap_or_default();
    let value = match command {
        "fill" => args.get(1).cloned(),
        "type" if args.len() >= 2 => args.get(1).cloned(),
        "type" => args.first().cloned(),
        "press" => args.first().cloned(),
        "select" => args.get(1).cloned(),
        _ => None,
    };
    let signature = match command {
        "click" | "tap" => format!("selector={target}|button=Left|count=1|iframe="),
        "dblclick" => format!("selector={target}|button=Left|count=2|iframe="),
        "fill" | "type" => {
            let selector = if args.len() >= 2 {
                target.as_str()
            } else {
                "<focused>"
            };
            format!("selector={selector}|submit=false|iframe=")
        },
        "press" => format!("key={target}|iframe="),
        "select" => format!(
            "selector={target}|option={}|iframe=",
            value.clone().unwrap_or_default()
        ),
        "check" => format!("selector={target}|checked=true|iframe="),
        "uncheck" => format!("selector={target}|checked=false|iframe="),
        _ => return None,
    };
    let semantic_signature = match command {
        "press" => Some(format!("press:{target}")),
        _ if !target.is_empty() => Some(format!("{command}:{target}")),
        _ => None,
    };
    let (page_origin, page_path_template) = page_context_from_url(page_url);
    let mut param_values = HashMap::new();
    if !target.is_empty() {
        param_values.insert("target".to_string(), target.clone());
    }
    if let Some(value) = value.clone().filter(|value| !value.is_empty()) {
        param_values.insert("value".to_string(), value.clone());
        param_values.insert("text".to_string(), value);
    }
    let user_values = value
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect();
    Some(
        crate::magician_v2::api_mining::action_binding::ActionContext {
            action_type,
            action_signature: signature,
            semantic_signature,
            page_origin,
            page_path_template,
            param_values,
            user_values,
        },
    )
}

fn browser_eval_action_context(
    script: &str,
    page_url: Option<&str>,
) -> Option<crate::magician_v2::api_mining::action_binding::ActionContext> {
    if browser_eval_navigation_url(script).is_some() {
        return None;
    }
    if !script_contains_dom_click(script) {
        return None;
    }

    let label = browser_eval_click_label(script);
    let digest = browser_eval_action_digest(script);
    let action_signature = label
        .as_ref()
        .map(|label| format!("eval_click|label={label}|iframe="))
        .unwrap_or_else(|| format!("eval_click|digest={digest}|iframe="));
    let semantic_signature = label
        .as_ref()
        .map(|label| format!("click:text:{}", normalize_eval_label(label)))
        .or_else(|| Some(format!("eval_click:{digest}")));
    let (page_origin, page_path_template) = page_context_from_url(page_url);
    let mut param_values = HashMap::from([("eval_digest".to_string(), digest)]);
    if let Some(label) = label {
        param_values.insert("target".to_string(), label.clone());
        param_values.insert("text".to_string(), label);
    }

    Some(
        crate::magician_v2::api_mining::action_binding::ActionContext {
            action_type: "Click".to_string(),
            action_signature,
            semantic_signature,
            page_origin,
            page_path_template,
            param_values,
            user_values: Vec::new(),
        },
    )
}

fn script_contains_dom_click(script: &str) -> bool {
    let lowered = script.to_ascii_lowercase();
    lowered.contains(".click(")
        || lowered.contains(".click.call(")
        || lowered.contains("dispatchevent(new mouseevent")
}

fn browser_eval_click_label(script: &str) -> Option<String> {
    let literals = js_string_literals(script);
    literals
        .into_iter()
        .filter_map(|literal| {
            let trimmed = literal.trim();
            if trimmed.len() < 2 || trimmed.len() > 80 {
                return None;
            }
            let lowered = trimmed.to_ascii_lowercase();
            if matches!(
                lowered.as_str(),
                "a" | "button"
                    | "div"
                    | "span"
                    | "input"
                    | "select"
                    | "textarea"
                    | "label"
                    | "form"
                    | "role"
                    | "aria-label"
            ) {
                return None;
            }
            if lowered.contains("not found")
                || lowered.contains("error")
                || lowered.contains("reason")
                || lowered.contains("failed")
            {
                return None;
            }
            if trimmed.contains('{') || trimmed.contains('}') || trimmed.contains(';') {
                return None;
            }

            let mut score = 0i32;
            if script.contains(&format!("=== '{}'", trimmed))
                || script.contains(&format!("=== \"{}\"", trimmed))
                || script.contains(&format!("== '{}'", trimmed))
                || script.contains(&format!("== \"{}\"", trimmed))
            {
                score += 6;
            }
            if script.contains(&format!(".includes('{}')", trimmed))
                || script.contains(&format!(".includes(\"{}\")", trimmed))
            {
                score += 4;
            }
            if trimmed.chars().any(char::is_whitespace) || trimmed.contains('-') {
                score += 3;
            }
            if trimmed.len() >= 4 {
                score += 1;
            }

            (score > 0).then(|| (score, trimmed.to_string()))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then(left.1.len().cmp(&right.1.len())))
        .map(|(_, label)| label)
}

fn js_string_literals(script: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut chars = script.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if !matches!(ch, '\'' | '"' | '`') {
            continue;
        }
        let quote = ch;
        let mut literal = String::new();
        let mut escaped = false;
        for (_, next) in chars.by_ref() {
            if escaped {
                literal.push(next);
                escaped = false;
                continue;
            }
            if next == '\\' {
                escaped = true;
                continue;
            }
            if next == quote {
                break;
            }
            literal.push(next);
        }
        if !literal.trim().is_empty() {
            literals.push(literal);
        }
    }
    literals
}

fn normalize_eval_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn browser_eval_action_digest(script: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(script.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

fn page_load_action_context(
    url: &str,
) -> Option<crate::magician_v2::api_mining::action_binding::ActionContext> {
    let parsed = url::Url::parse(url).ok()?;
    let origin = crate::magician_v2::api_mining::router::extract_origin(url);
    if origin.is_empty() {
        return None;
    }
    let path_template = path_template_for_url(&parsed);
    let mut query_pairs = parsed
        .query_pairs()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    query_pairs.sort_by(|(left, _), (right, _)| left.cmp(right));
    let query_keys = query_pairs
        .iter()
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let action_signature = format!("{origin}{path_template}|query_keys={query_keys}");
    let semantic_signature = Some(format!("page_load:{origin}{path_template}?{query_keys}"));

    let mut param_values = HashMap::new();
    let mut user_values = Vec::new();
    for (key, value) in query_pairs {
        if value.is_empty() {
            continue;
        }
        let param_key = format!("page_query_{}", sanitize_action_param_key(&key));
        param_values.insert(param_key, value.clone());
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "q" | "query" | "search" | "term" | "keyword" | "text"
        ) {
            param_values
                .entry("query".to_string())
                .or_insert_with(|| value.clone());
            param_values
                .entry("text".to_string())
                .or_insert_with(|| value.clone());
        }
        user_values.push(value);
    }

    Some(
        crate::magician_v2::api_mining::action_binding::ActionContext {
            action_type: "PageLoad".to_string(),
            action_signature,
            semantic_signature,
            page_origin: Some(origin),
            page_path_template: Some(path_template),
            param_values,
            user_values,
        },
    )
}

fn sanitize_action_param_key(raw: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "value".to_string()
    } else {
        trimmed.to_string()
    }
}

fn browser_takeover_candidate(
    action: &str,
    arguments: &Value,
    page_url: Option<&str>,
) -> Option<BrowserApiTakeoverCandidate> {
    if action == "open" {
        return None;
    }

    if action == "batch" {
        let commands = browser_batch_commands(arguments);
        if commands
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("open"))
        {
            return None;
        }
        let mut candidates = commands
            .into_iter()
            .filter_map(|argv| {
                let command = argv.first()?.as_str();
                let args = argv[1..].to_vec();
                browser_command_action_context(command, &args, page_url)
                    .map(|context| BrowserApiTakeoverCandidate::Action { context })
            })
            .collect::<Vec<_>>();
        return (candidates.len() == 1).then(|| candidates.remove(0));
    }

    let args = browser_arg_strings(arguments);
    browser_command_action_context(action, &args, page_url)
        .map(|context| BrowserApiTakeoverCandidate::Action { context })
}

fn browser_navigation_url_from_call(action: &str, arguments: &Value) -> Option<String> {
    if action == "open" {
        return browser_arg_strings(arguments)
            .into_iter()
            .next()
            .filter(|url| browser_url_is_http(url));
    }
    if action == "tab" {
        return browser_tab_navigation_url(&browser_arg_strings(arguments));
    }
    if action == "eval" {
        return browser_arg_strings(arguments)
            .into_iter()
            .find_map(|script| browser_eval_navigation_url(&script));
    }
    if action != "batch" {
        return None;
    }
    let mut urls = browser_batch_commands(arguments)
        .into_iter()
        .filter_map(|argv| {
            let command = argv.first()?;
            let args = argv[1..].to_vec();
            browser_navigation_url_from_argv(command, &args)
        })
        .collect::<Vec<_>>();
    (urls.len() == 1).then(|| urls.remove(0))
}

fn browser_navigation_url_from_argv(command: &str, args: &[String]) -> Option<String> {
    match command {
        "open" => args.first().filter(|url| browser_url_is_http(url)).cloned(),
        "tab" => browser_tab_navigation_url(args),
        "eval" => args
            .iter()
            .find_map(|script| browser_eval_navigation_url(script)),
        _ => None,
    }
}

fn browser_tab_navigation_url(args: &[String]) -> Option<String> {
    let subcommand = args.first().map(String::as_str)?;
    if !matches!(subcommand, "new" | "open") {
        return None;
    }
    args.get(1).filter(|url| browser_url_is_http(url)).cloned()
}

fn browser_eval_navigation_url(script: &str) -> Option<String> {
    let lower = script.to_ascii_lowercase();
    let looks_like_navigation = [
        "location.href",
        "window.location",
        "document.location",
        "location.assign",
        "location.replace",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if !looks_like_navigation {
        return None;
    }
    extract_first_http_url_literal(script)
}

fn extract_first_http_url_literal(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut index = 0usize;
    while index < input.len() {
        let rest = &input[index..];
        let Some(relative) = rest.find("http://").or_else(|| rest.find("https://")) else {
            break;
        };
        let start = index + relative;
        let mut end = start;
        while end < input.len() {
            let ch = bytes[end] as char;
            if ch.is_ascii_whitespace()
                || matches!(
                    ch,
                    '\'' | '"' | '`' | ')' | '}' | ']' | '<' | '>' | ';' | ','
                )
            {
                break;
            }
            end += 1;
        }
        let candidate = input[start..end].to_string();
        if browser_url_is_http(&candidate) {
            return Some(candidate);
        }
        index = end.saturating_add(1);
    }
    None
}

fn browser_url_is_http(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .map(|parsed| matches!(parsed.scheme(), "http" | "https"))
        .unwrap_or(false)
}

fn browser_action_events_for_call(
    action: &str,
    arguments: &Value,
    page_url: Option<&str>,
    timestamp_ms: i64,
) -> Vec<crate::magician_v2::api_mining::correlator::ActionEvent> {
    let contexts = if action == "batch" {
        browser_batch_commands(arguments)
            .into_iter()
            .filter_map(|argv| {
                let command = argv.first()?.clone();
                let args = argv[1..].to_vec();
                if command == "open" {
                    return args.first().and_then(|url| page_load_action_context(url));
                }
                browser_command_action_context(&command, &args, page_url)
            })
            .collect::<Vec<_>>()
    } else if action == "open" {
        browser_arg_strings(arguments)
            .into_iter()
            .next()
            .or_else(|| page_url.map(str::to_string))
            .and_then(|url| page_load_action_context(&url))
            .into_iter()
            .collect()
    } else {
        let args = browser_arg_strings(arguments);
        browser_command_action_context(action, &args, page_url)
            .into_iter()
            .collect()
    };

    contexts
        .into_iter()
        .enumerate()
        .map(
            |(index, context)| crate::magician_v2::api_mining::correlator::ActionEvent {
                action_id: format!("browser_{timestamp_ms}_{index}_{}", context.action_type),
                action_type: context.action_type,
                timestamp_ms,
                frame_id: None,
                user_values: context.user_values,
                page_url: page_url.map(str::to_string),
                action_signature: Some(context.action_signature),
                semantic_signature: context.semantic_signature,
                element_metadata: HashMap::new(),
                action_params: context.param_values,
                page_origin: context.page_origin,
                page_path_template: context.page_path_template,
            },
        )
        .collect()
}

fn current_page_url_for_api_mining(exec_ctx: &PrimitiveExecCtx) -> Option<String> {
    exec_ctx
        .api_mining_last_page_url
        .as_ref()
        .and_then(|slot| slot.lock().ok().and_then(|guard| guard.clone()))
}

fn set_current_page_url_for_api_mining(exec_ctx: &PrimitiveExecCtx, url: impl Into<String>) {
    let url = url.into();
    if url.trim().is_empty() {
        return;
    }
    if let Some(slot) = exec_ctx.api_mining_last_page_url.as_ref() {
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(url);
        }
    }
}

fn session_context_for_replay(
    exec_ctx: &PrimitiveExecCtx,
    replay_url: &str,
) -> crate::magician_v2::api_mining::types::SessionContext {
    exec_ctx
        .secret_store
        .as_ref()
        .and_then(|store| {
            let origin = crate::magician_v2::api_mining::router::extract_origin(replay_url);
            store.get_session(&origin, replay_url).map(|(ctx, _)| ctx)
        })
        .unwrap_or_default()
}

fn set_api_replay_meta(
    exec_ctx: &PrimitiveExecCtx,
    meta: crate::magician_v2::execution::agentic::ApiReplayMeta,
) {
    if let Some(slot) = exec_ctx.api_replay_last_outcome.as_ref() {
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(meta);
        }
    }
}

fn scoped_api_mining_base_path(exec_ctx: &PrimitiveExecCtx) -> std::path::PathBuf {
    exec_ctx
        .api_mining_base_path
        .as_ref()
        .cloned()
        .unwrap_or_else(|| {
            ArtifactV2Workspace::new(&exec_ctx.storage_base_path).api_mining_root(
                exec_ctx.principal.as_deref().unwrap_or("anonymous"),
                exec_ctx.workspace.as_deref().unwrap_or("default"),
            )
        })
}

fn api_mining_enabled_for_exec_ctx(exec_ctx: &PrimitiveExecCtx) -> bool {
    let router_enabled = exec_ctx.api_router.as_ref().is_some_and(|router| {
        router
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .config()
            .any_enabled()
    });
    if !router_enabled {
        return false;
    }
    api_mining_switch_effective_for_base(&scoped_api_mining_base_path(exec_ctx))
}

fn api_mining_switch_effective_for_base(api_mining_base_path: &std::path::Path) -> bool {
    let process_enabled = crate::magician_v2::api_mining::switch::runtime_api_mining_config()
        .map(|config| config.enabled)
        .unwrap_or(false);
    crate::magician_v2::api_mining::switch::ApiMiningSwitch::effective_from_disk(
        process_enabled,
        api_mining_base_path,
    )
}

fn api_mining_projection_records_dir(
    exec_ctx: &PrimitiveExecCtx,
    api_mining_base_path: &std::path::Path,
) -> std::path::PathBuf {
    let principal = exec_ctx
        .principal
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_WORKSPACE);
    let kit_db = crate::magician_v2::database_owners::database_file_path(
        &ArtifactV2Workspace::new(&exec_ctx.storage_base_path),
        principal,
        workspace,
        crate::magician_v2::database_owners::DatabaseOwner::ApiMining,
    );
    if let Some(kit_dir) = kit_db.parent() {
        if exec_ctx.api_mining_base_path.is_none()
            || api_mining_base_path.join("projections") == kit_dir
        {
            return kit_dir.to_path_buf();
        }
    }
    api_mining_base_path.join("projections")
}

fn route_browser_api_candidate(
    action: &str,
    arguments: &Value,
    exec_ctx: &PrimitiveExecCtx,
) -> Option<(
    crate::magician_v2::api_mining::router::RouteDecision,
    Arc<crate::magician_v2::api_mining::RouterMetrics>,
)> {
    let router = exec_ctx.api_router.as_ref()?.clone();
    let page_url = current_page_url_for_api_mining(exec_ctx);
    let candidate = browser_takeover_candidate(action, arguments, page_url.as_deref())?;

    let router_guard = router.lock().ok()?;
    let decision = match &candidate {
        BrowserApiTakeoverCandidate::Action { context } => {
            let replay_url_hint = page_url.as_deref().unwrap_or("");
            let session = session_context_for_replay(exec_ctx, replay_url_hint);
            router_guard.route_action_context(context, &session)
        },
    };
    Some((decision, router_guard.metrics()))
}

fn ensure_sequence_recorder<'a>(
    exec_ctx: &'a PrimitiveExecCtx,
    origin: &str,
) -> Option<
    std::sync::MutexGuard<
        'a,
        Option<crate::magician_v2::api_mining::sequence_recorder::SequenceRecorder>,
    >,
> {
    exec_ctx.api_router.as_ref()?;
    let slot = exec_ctx.api_mining_sequence_recorder.as_ref()?;
    let mut guard = slot.lock().ok()?;
    if guard.is_none() {
        let task_id = exec_ctx.task_id.as_deref().unwrap_or("unknown_task");
        let execution_id = exec_ctx
            .execution_id
            .as_deref()
            .or(exec_ctx.legacy_execution_id.as_deref())
            .unwrap_or("unknown_execution");
        let origin_key =
            crate::magician_v2::api_mining::capability_store::CapabilityStore::origin_to_key(
                origin,
            );
        *guard = Some(
            crate::magician_v2::api_mining::sequence_recorder::SequenceRecorder::start(
                task_id,
                execution_id,
                &origin_key,
                chrono::Utc::now().timestamp_millis(),
            ),
        );
        let principal = exec_ctx.principal.as_deref().unwrap_or("anonymous");
        let workspace = exec_ctx.workspace.as_deref().unwrap_or("default");
        crate::magician_v2::api_mining::sequence_metrics_for_scope(principal, workspace)
            .record_started();
    }
    Some(guard)
}

fn record_api_sequence_step(
    exec_ctx: &PrimitiveExecCtx,
    origin: &str,
    capability_id: &str,
    request: &crate::magician_v2::api_mining::replay::ReplayRequest,
    request_params: HashMap<String, String>,
    replay: &crate::magician_v2::api_mining::types::ReplayResult,
) {
    let Some(mut guard) = ensure_sequence_recorder(exec_ctx, origin) else {
        return;
    };
    if let Some(recorder) = guard.as_mut() {
        recorder.record_api_step(
            Some(capability_id.to_string()),
            origin,
            &request.url,
            &request.method,
            request_params,
            request.body.as_deref(),
            Some(replay.status),
            replay.response_body.as_deref(),
            None,
            chrono::Utc::now().timestamp_millis(),
            replay.timing_ms,
        );
    }
}

fn ingest_successful_api_replay_projection(
    exec_ctx: &PrimitiveExecCtx,
    api_mining_base_path: &std::path::Path,
    origin: &str,
    capability_id: &str,
    url_template: &str,
    replay: &crate::magician_v2::api_mining::types::ReplayResult,
) {
    if !replay.success {
        return;
    }

    let Some(body) = replay.response_body.as_deref() else {
        tracing::debug!(
            target: "magician::api_mining",
            origin = %origin,
            capability_id = %capability_id,
            "api_mining.projection_ingest.skipped_no_body"
        );
        return;
    };

    let response = match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                error = %error,
                "api_mining.projection_ingest.skipped_non_json"
            );
            return;
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
    let records_dir = api_mining_projection_records_dir(exec_ctx, api_mining_base_path);

    let pipeline = match crate::magician_v2::api_mining::projection_pipeline::pipeline_for_scope(
        principal,
        workspace,
        &records_dir,
    ) {
        Ok(pipeline) => pipeline,
        Err(error) => {
            tracing::warn!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                error = %error,
                "api_mining.projection_ingest.pipeline_open_failed"
            );
            return;
        },
    };

    let outcome = pipeline.ingest_response(capability_id, url_template, &response);
    match outcome {
        crate::magician_v2::api_mining::projection_pipeline::IngestOutcome::NotProjectable => {
            tracing::debug!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                url_template = %url_template,
                "api_mining.projection_ingest.not_projectable"
            );
        },
        crate::magician_v2::api_mining::projection_pipeline::IngestOutcome::PendingCreated {
            projection_id,
        } => {
            tracing::info!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                projection_id = %projection_id,
                "api_mining.projection_ingest.pending_created"
            );
        },
        crate::magician_v2::api_mining::projection_pipeline::IngestOutcome::PendingExisting {
            projection_id,
        } => {
            tracing::debug!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                projection_id = %projection_id,
                "api_mining.projection_ingest.pending_existing"
            );
        },
        crate::magician_v2::api_mining::projection_pipeline::IngestOutcome::Ingested {
            projection_id,
            row_count,
            schema_added_columns,
        } => {
            tracing::info!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                projection_id = %projection_id,
                row_count,
                schema_added_columns,
                "api_mining.projection_ingest.ingested"
            );
        },
        crate::magician_v2::api_mining::projection_pipeline::IngestOutcome::MigrationRejected {
            projection_id,
            reason,
        } => {
            tracing::warn!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                projection_id = %projection_id,
                reason = %reason,
                "api_mining.projection_ingest.migration_rejected"
            );
        },
        crate::magician_v2::api_mining::projection_pipeline::IngestOutcome::Failed { reason } => {
            tracing::warn!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                reason = %reason,
                "api_mining.projection_ingest.failed"
            );
        },
    }
}

fn record_browser_sequence_step(
    exec_ctx: &PrimitiveExecCtx,
    action: &str,
    arguments: &Value,
    page_url: Option<&str>,
    duration_ms: u64,
) {
    let Some(page_url) = page_url.filter(|url| !url.trim().is_empty()) else {
        return;
    };
    let origin = crate::magician_v2::api_mining::router::extract_origin(page_url);
    if origin.is_empty() {
        return;
    }
    let Some(mut guard) = ensure_sequence_recorder(exec_ctx, &origin) else {
        return;
    };
    if let Some(recorder) = guard.as_mut() {
        let desc = format!("browser__{action} {}", compact_browser_args(arguments));
        recorder.record_browser_step(
            None,
            &origin,
            Some(page_url),
            None,
            None,
            None,
            &desc,
            Some(action),
            Some(arguments),
            chrono::Utc::now().timestamp_millis(),
            duration_ms,
        );
    }
}

fn record_browser_json_capability_sequence_step(
    exec_ctx: &PrimitiveExecCtx,
    action: &str,
    arguments: &Value,
    promotion: &BrowserJsonGetPromotion,
    duration_ms: u64,
) {
    let Some(mut guard) = ensure_sequence_recorder(exec_ctx, &promotion.origin) else {
        return;
    };
    if let Some(recorder) = guard.as_mut() {
        let desc = format!("browser__{action} {}", compact_browser_args(arguments));
        recorder.record_browser_capability_step(
            promotion.capability_id.clone(),
            &promotion.origin,
            &promotion.concrete_url,
            "GET",
            promotion.request_params.clone(),
            Some(200),
            Some(&promotion.response_body),
            &desc,
            Some(action),
            Some(arguments),
            chrono::Utc::now().timestamp_millis(),
            duration_ms,
        );
    }
}

fn record_correlated_browser_capability_sequence_step(
    exec_ctx: &PrimitiveExecCtx,
    action: &str,
    arguments: &Value,
    promotion: &CorrelatedBrowserCapabilityPromotion,
    duration_ms: u64,
) {
    let Some(mut guard) = ensure_sequence_recorder(exec_ctx, &promotion.origin) else {
        return;
    };
    if let Some(recorder) = guard.as_mut() {
        let desc = format!("browser__{action} {}", compact_browser_args(arguments));
        recorder.record_browser_capability_step(
            promotion.capability_id.clone(),
            &promotion.origin,
            &promotion.concrete_url,
            &promotion.method,
            promotion.request_params.clone(),
            promotion.response_status,
            promotion.response_body.as_deref(),
            &desc,
            Some(action),
            Some(arguments),
            chrono::Utc::now().timestamp_millis(),
            duration_ms,
        );
    }
}

fn promote_correlated_browser_action_capability(
    exec_ctx: &PrimitiveExecCtx,
    action_events: &[crate::magician_v2::api_mining::correlator::ActionEvent],
    traces: &[crate::magician_v2::api_mining::types::NetworkTraceEvent],
) -> Option<CorrelatedBrowserCapabilityPromotion> {
    if action_events.is_empty() || traces.is_empty() {
        return None;
    }

    let mut correlator_config =
        crate::magician_v2::api_mining::correlator::CorrelatorConfig::default();
    correlator_config.max_per_action = 0;
    let correlations =
        crate::magician_v2::api_mining::correlator::Correlator::new(correlator_config)
            .correlate_all(action_events, traces);
    if correlations.is_empty() {
        return None;
    }

    let mut grouped: HashMap<
        &str,
        Vec<&crate::magician_v2::api_mining::correlator::CorrelatedRequest>,
    > = HashMap::new();
    for correlation in &correlations {
        grouped
            .entry(correlation.action_id.as_str())
            .or_default()
            .push(correlation);
    }

    let base_path = scoped_api_mining_base_path(exec_ctx);
    let mut registry =
        match crate::magician_v2::api_mining::registry::CapabilityRegistry::with_base_path(
            &base_path,
        ) {
            Ok(registry) => registry,
            Err(err) => {
                tracing::debug!(
                    target: "magician::api_mining",
                    error = %err,
                    "api_mining.live_action_binding.registry_open_failed"
                );
                return None;
            },
        };

    for action in action_events {
        let candidates = grouped
            .get(action.action_id.as_str())
            .cloned()
            .unwrap_or_default();
        if candidates.is_empty() {
            continue;
        }
        let ranked =
            crate::magician_v2::api_mining::action_binding::select_ranked_action_binding_candidates(
                action,
                &candidates,
            );
        for correlation in ranked {
            let Some(promotion) =
                persist_correlated_action_binding(exec_ctx, &mut registry, action, correlation)
            else {
                continue;
            };
            tracing::info!(
                target: "magician::api_mining",
                capability_id = %promotion.capability_id,
                origin = %promotion.origin,
                method = %promotion.method,
                url = %crate::magician_v2::api_mining::router::truncate_url(&promotion.concrete_url),
                "api_mining.live_action_binding.persisted"
            );
            return Some(promotion);
        }
    }

    None
}

fn persist_correlated_action_binding(
    _exec_ctx: &PrimitiveExecCtx,
    registry: &mut crate::magician_v2::api_mining::registry::CapabilityRegistry,
    action: &crate::magician_v2::api_mining::correlator::ActionEvent,
    correlation: &crate::magician_v2::api_mining::correlator::CorrelatedRequest,
) -> Option<CorrelatedBrowserCapabilityPromotion> {
    use crate::magician_v2::api_mining::action_binding::{
        action_context_from_event, capability_requires_runtime_params, infer_action_binding,
    };
    use crate::magician_v2::api_mining::miner::{
        compute_body_fingerprint_standalone, detect_graphql_request_info_standalone,
    };

    let context = action_context_from_event(action)?;
    let method = correlation.trace.method.to_uppercase();
    let content_type = correlation
        .trace
        .request_headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str());
    let graphql_info = detect_graphql_request_info_standalone(
        correlation.trace.request_body.as_deref(),
        content_type,
    );
    let body_fingerprint = if !matches!(method.as_str(), "GET" | "HEAD") {
        compute_body_fingerprint_standalone(
            correlation.trace.request_body.as_deref(),
            content_type,
            graphql_info.is_some(),
        )
    } else {
        None
    };

    let summary = registry.find_by_url_with_request_context(
        &method,
        &correlation.trace.url,
        body_fingerprint.as_deref(),
        graphql_info
            .as_ref()
            .map(|info| info.operation_name.as_str()),
        graphql_info
            .as_ref()
            .and_then(|info| match info.operation_kind {
                crate::magician_v2::api_mining::capability::GraphqlOperationKind::Unknown => None,
                kind => Some(kind),
            }),
    )?;
    let origin = url::Url::parse(&correlation.trace.url)
        .ok()?
        .origin()
        .unicode_serialization();
    let mut capability = registry.get_capability(&origin, &summary.id).ok()?;
    let binding = infer_action_binding(action, &capability, &correlation.trace)?;

    if binding.param_bindings.is_empty()
        && binding.default_params.is_empty()
        && (!context.param_values.is_empty() || capability_requires_runtime_params(&capability))
    {
        tracing::debug!(
            target: "magician::api_mining",
            capability_id = %capability.id,
            capability_name = %capability.name,
            action_type = %action.action_type,
            "api_mining.live_action_binding.skipped_unmapped_runtime_params"
        );
        return None;
    }

    let request_params = sequence_request_params_from_binding(&binding, &context);
    capability.record_action_binding(binding);
    registry.register(&capability).ok()?;

    Some(CorrelatedBrowserCapabilityPromotion {
        capability_id: summary.id,
        origin,
        concrete_url: correlation.trace.url.clone(),
        method,
        request_params,
        response_status: Some(correlation.trace.status),
        response_body: correlation.trace.response_body.clone(),
    })
}

fn sequence_request_params_from_binding(
    binding: &crate::magician_v2::api_mining::action_binding::ActionBinding,
    context: &crate::magician_v2::api_mining::action_binding::ActionContext,
) -> HashMap<String, String> {
    let mut params = binding
        .default_params
        .iter()
        .filter(|(_, value)| !value.starts_with("__magician_runtime:"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<HashMap<_, _>>();

    for mapping in &binding.param_bindings {
        if let Some(value) = context.param_values.get(&mapping.action_param) {
            params.insert(mapping.capability_param.clone(), value.clone());
        }
    }

    params
}

fn compact_browser_args(arguments: &Value) -> String {
    if let Some(args) = arguments.get("args").and_then(Value::as_array) {
        return args
            .iter()
            .filter_map(Value::as_str)
            .take(4)
            .collect::<Vec<_>>()
            .join(" ");
    }
    if arguments.get("commands").is_some() {
        return "<batch>".to_string();
    }
    String::new()
}

fn promote_browser_json_get_capability(
    exec_ctx: &PrimitiveExecCtx,
    action: &str,
    page_url: Option<&str>,
    result: &PrimitiveToolResult,
) -> Option<BrowserJsonGetPromotion> {
    if !matches!(action, "open" | "get") {
        return None;
    }

    let page_url = page_url?;
    let parsed_url = url::Url::parse(page_url).ok()?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return None;
    }
    if action == "get" && !browser_json_get_url_looks_api_like(&parsed_url) {
        return None;
    }
    if browser_json_get_url_is_denied(&parsed_url) {
        return None;
    }

    let (response_body, parsed_json) = extract_browser_json_document_body(result)?;
    let origin = crate::magician_v2::api_mining::router::extract_origin(page_url);
    if origin.is_empty() {
        return None;
    }

    let url_template = browser_json_get_url_template(&parsed_url)?;
    let mut capability = crate::magician_v2::api_mining::capability::ApiCapability::new(
        browser_json_get_capability_name(&parsed_url),
        origin.clone(),
        "GET".to_string(),
        url_template,
    );
    capability.response_schema = Some(json_schema_sketch(&parsed_json));

    let router = exec_ctx.api_router.as_ref()?.clone();
    let mut router_guard = router.lock().ok()?;
    let candidate_min_samples = router_guard.config().takeover_min_samples("GET");
    capability.try_promote_with_threshold(candidate_min_samples);

    let registry = router_guard.registry_mut()?;
    let existing_summary = registry
        .find_by_url("GET", page_url)
        .filter(|summary| summary.url_template == capability.url_template);
    let observed_capability = if let Some(existing_summary) = existing_summary {
        match registry.get_capability(&origin, &existing_summary.id) {
            Ok(mut existing) => {
                existing.add_sample(browser_json_get_trace_id(exec_ctx, page_url));
                existing.response_schema = Some(json_schema_sketch(&parsed_json));
                existing.refresh_side_effects();
                existing.try_promote_with_threshold(candidate_min_samples);
                existing
            },
            Err(err) => {
                tracing::warn!(
                    target: "magician::api_mining",
                    url = %page_url,
                    capability_id = %existing_summary.id,
                    error = %err,
                    "failed to load existing browser JSON GET capability; registering fresh observation"
                );
                capability
            },
        }
    } else {
        capability
    };

    if let Err(err) = registry.register(&observed_capability) {
        tracing::warn!(
            target: "magician::api_mining",
            url = %page_url,
            error = %err,
            "failed to register browser JSON GET capability"
        );
        return None;
    }

    let summary = registry
        .find_by_url("GET", page_url)
        .unwrap_or_else(|| observed_capability.to_summary());
    let request_params = crate::magician_v2::api_mining::router::extract_template_params(
        &summary.url_template,
        page_url,
    );

    Some(BrowserJsonGetPromotion {
        capability_id: summary.id,
        origin,
        concrete_url: page_url.to_string(),
        request_params,
        response_body,
    })
}

fn browser_json_get_trace_id(exec_ctx: &PrimitiveExecCtx, page_url: &str) -> String {
    let task_id = exec_ctx.task_id.as_deref().unwrap_or("unknown_task");
    let execution_id = exec_ctx
        .execution_id
        .as_deref()
        .or(exec_ctx.legacy_execution_id.as_deref())
        .unwrap_or("unknown_execution");
    format!(
        "browser_json_get:{task_id}:{execution_id}:{}:{}",
        chrono::Utc::now().timestamp_millis(),
        page_url
    )
}

fn extract_browser_json_document_body(
    result: &PrimitiveToolResult,
) -> Option<(String, serde_json::Value)> {
    if let Some(value) = result.parsed_json.as_ref() {
        if matches!(
            value,
            serde_json::Value::Object(_) | serde_json::Value::Array(_)
        ) {
            return Some((result.stdout.trim().to_string(), value.clone()));
        }
        if let Some(text) = value.as_str() {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.trim()) {
                if matches!(
                    parsed,
                    serde_json::Value::Object(_) | serde_json::Value::Array(_)
                ) {
                    return Some((text.trim().to_string(), parsed));
                }
            }
        }
    }

    let trimmed = result.stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    if matches!(
        parsed,
        serde_json::Value::Object(_) | serde_json::Value::Array(_)
    ) {
        Some((trimmed.to_string(), parsed))
    } else {
        None
    }
}

fn browser_json_get_url_looks_api_like(url: &url::Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    path.starts_with("/api")
        || path.contains("/api/")
        || path.ends_with(".json")
        || path.contains("/graphql")
        || path.contains("/query")
        || path.contains("/items/")
}

fn browser_json_get_url_is_denied(url: &url::Url) -> bool {
    let haystack = format!(
        "{}?{}",
        url.path().to_ascii_lowercase(),
        url.query().unwrap_or("").to_ascii_lowercase()
    );
    crate::magician_v2::api_mining::auto_replay::DEFAULT_URL_DENYLIST
        .iter()
        .any(|marker| haystack.contains(marker))
}

fn browser_json_get_url_template(url: &url::Url) -> Option<String> {
    let origin = crate::magician_v2::api_mining::router::extract_origin(url.as_str());
    if origin.is_empty() {
        return None;
    }

    let mut used_param_names = Vec::new();
    let mut previous_literal_segment: Option<String> = None;
    let segments = url
        .path_segments()
        .map(|segments| {
            segments
                .map(|segment| {
                    if let Some(base_name) = browser_json_dynamic_path_param_name(
                        segment,
                        previous_literal_segment.as_deref(),
                    ) {
                        let name = unique_template_param_name(&base_name, &mut used_param_names);
                        format!("{{{name}}}")
                    } else {
                        previous_literal_segment = Some(segment.to_string());
                        segment.to_string()
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let path = if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    };

    let mut query_pairs = url
        .query_pairs()
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();
    query_pairs.sort();
    query_pairs.dedup();
    if query_pairs.is_empty() {
        return Some(format!("{origin}{path}"));
    }

    let query_template = query_pairs
        .into_iter()
        .map(|key| {
            let base_name = sanitize_template_param_name(&format!("query_{key}"));
            let name = unique_template_param_name(&base_name, &mut used_param_names);
            format!("{key}={{{name}}}")
        })
        .collect::<Vec<_>>()
        .join("&");
    Some(format!("{origin}{path}?{query_template}"))
}

fn browser_json_dynamic_path_param_name(
    segment: &str,
    previous_literal_segment: Option<&str>,
) -> Option<String> {
    if segment.is_empty() {
        return None;
    }
    let all_digits = segment.chars().all(|ch| ch.is_ascii_digit());
    if all_digits {
        let prefix = previous_literal_segment
            .map(singularize_last_path_segment)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "id".to_string());
        return Some(sanitize_template_param_name(&format!("{prefix}_id")));
    }
    if segment.len() == 36
        && segment
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || ch == '-')
        && segment.contains('-')
    {
        let prefix = previous_literal_segment
            .map(singularize_last_path_segment)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "resource".to_string());
        return Some(sanitize_template_param_name(&format!("{prefix}_uuid")));
    }
    if segment.len() >= 32 && segment.chars().all(|ch| ch.is_ascii_hexdigit()) {
        let prefix = previous_literal_segment
            .map(singularize_last_path_segment)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "resource".to_string());
        return Some(sanitize_template_param_name(&format!("{prefix}_hash")));
    }
    if segment.len() >= 16
        && segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        && segment.chars().any(|ch| ch.is_ascii_digit())
        && segment.chars().any(|ch| ch.is_ascii_alphabetic())
    {
        let prefix = previous_literal_segment
            .map(singularize_last_path_segment)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "resource".to_string());
        return Some(sanitize_template_param_name(&format!("{prefix}_id")));
    }
    None
}

fn singularize_last_path_segment(segment: &str) -> String {
    let sanitized = sanitize_template_param_name(segment);
    sanitized
        .strip_suffix("ies")
        .map(|stem| format!("{stem}y"))
        .or_else(|| sanitized.strip_suffix('s').map(str::to_string))
        .unwrap_or(sanitized)
}

fn sanitize_template_param_name(raw: &str) -> String {
    let mut out = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        return "param".to_string();
    }
    if out
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
    {
        format!("p_{out}")
    } else {
        out.to_string()
    }
}

fn unique_template_param_name(base: &str, used: &mut Vec<String>) -> String {
    let base = if base.is_empty() { "param" } else { base };
    if !used.iter().any(|name| name == base) {
        used.push(base.to_string());
        return base.to_string();
    }
    let mut index = 2usize;
    loop {
        let candidate = format!("{base}_{index}");
        if !used.iter().any(|name| name == &candidate) {
            used.push(candidate.clone());
            return candidate;
        }
        index += 1;
    }
}

fn browser_json_get_capability_name(url: &url::Url) -> String {
    let label = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .map(sanitize_template_param_name)
        .unwrap_or_else(|| "json_document".to_string());
    format!("browser_json_get_{label}")
}

fn json_schema_sketch(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut properties = serde_json::Map::new();
            for (key, value) in map.iter().take(64) {
                properties.insert(key.clone(), json_schema_sketch(value));
            }
            json!({
                "type": "object",
                "properties": properties,
            })
        },
        serde_json::Value::Array(items) => json!({
            "type": "array",
            "items": items.first().map(json_schema_sketch).unwrap_or_else(|| json!({})),
        }),
        serde_json::Value::String(_) => json!({"type": "string"}),
        serde_json::Value::Number(_) => json!({"type": "number"}),
        serde_json::Value::Bool(_) => json!({"type": "boolean"}),
        serde_json::Value::Null => json!({"type": "null"}),
    }
}

fn push_api_mining_action_events(
    exec_ctx: &PrimitiveExecCtx,
    events: Vec<crate::magician_v2::api_mining::correlator::ActionEvent>,
) {
    if events.is_empty() || exec_ctx.api_router.is_none() {
        return;
    }
    if let Some(slot) = exec_ctx.api_mining_action_events.as_ref() {
        if let Ok(mut guard) = slot.lock() {
            guard.extend(events);
        }
    }
}

fn replay_action_result(
    capability_id: &str,
    origin: &str,
    request: &crate::magician_v2::api_mining::replay::ReplayRequest,
    replay: &crate::magician_v2::api_mining::types::ReplayResult,
) -> ActionResult {
    ActionResult::Browser {
        data: json!({
            "success": true,
            "api_replayed": true,
            "capability_id": capability_id,
            "origin": origin,
            "replay_method": replay.replay_method,
            "replay_url": request.url,
            "replay_status": replay.status,
            "timing_ms": replay.timing_ms,
            "output": replay.response_body.clone().unwrap_or_else(|| {
                format!("API replay succeeded with status {}", replay.status)
            }),
        }),
    }
}

async fn attempt_browser_api_takeover(
    action: &str,
    arguments: &Value,
    exec_ctx: &PrimitiveExecCtx,
) -> Option<ActionResult> {
    let (decision, metrics) = route_browser_api_candidate(action, arguments, exec_ctx)?;
    let (request, request_params, capability_id, origin) = match decision {
        crate::magician_v2::api_mining::router::RouteDecision::Replay {
            request,
            request_params,
            capability_id,
            origin,
            ..
        } => {
            metrics.record(crate::magician_v2::api_mining::metrics::RouterOutcome::Replayed);
            (request, request_params, capability_id, origin)
        },
        crate::magician_v2::api_mining::router::RouteDecision::ReplayRequiresHitl {
            request,
            request_params,
            capability_id,
            origin,
            confidence,
            side_effects,
            reason,
        } => {
            if !request_api_replay_approval(
                exec_ctx,
                &origin,
                &capability_id,
                &request,
                &confidence,
                &side_effects,
                &reason,
            )
            .await
            {
                metrics.record(
                    crate::magician_v2::api_mining::metrics::RouterOutcome::PassThroughNotReplayable,
                );
                set_api_replay_meta(
                    exec_ctx,
                    crate::magician_v2::execution::agentic::ApiReplayMeta {
                        attempted: true,
                        succeeded: false,
                        time_ms: None,
                        fallback_reason: Some(
                            "write API replay was not approved; direct API and browser mutation were stopped safely"
                                .to_string(),
                        ),
                        replay_url: Some(request.url.clone()),
                        replay_status: None,
                        replay_body: None,
                    },
                );
                return Some(api_replay_approval_denied_result(
                    &origin,
                    &capability_id,
                    &request,
                    &reason,
                ));
            }
            metrics.record(crate::magician_v2::api_mining::metrics::RouterOutcome::Replayed);
            (request, request_params, capability_id, origin)
        },
        other => {
            metrics.record(other.outcome());
            return None;
        },
    };

    let session = session_context_for_replay(exec_ctx, &request.url);
    let base_path = scoped_api_mining_base_path(exec_ctx);
    let mut runner =
        match crate::magician_v2::api_mining::replay::ApiRunner::with_base_path(&base_path) {
            Ok(runner) => runner,
            Err(err) => {
                set_api_replay_meta(
                    exec_ctx,
                    crate::magician_v2::execution::agentic::ApiReplayMeta {
                        attempted: true,
                        succeeded: false,
                        time_ms: None,
                        fallback_reason: Some(format!("failed to initialize API runner: {err}")),
                        replay_url: Some(request.url.clone()),
                        replay_status: None,
                        replay_body: None,
                    },
                );
                return None;
            },
        };

    let replay = match runner
        .replay_with_reqwest(
            &origin,
            &capability_id,
            &request_params,
            &session,
            Some(&request.url),
            // A 1:1 substitution rather than a fan-out — one browser gesture
            // becomes one HTTP request — so the dispatch key applies directly.
            exec_ctx.effect_id.as_deref(),
        )
        .await
    {
        Ok(result) => result,
        Err(err) => {
            set_api_replay_meta(
                exec_ctx,
                crate::magician_v2::execution::agentic::ApiReplayMeta {
                    attempted: true,
                    succeeded: false,
                    time_ms: None,
                    fallback_reason: Some(err),
                    replay_url: Some(request.url.clone()),
                    replay_status: None,
                    replay_body: None,
                },
            );
            return None;
        },
    };

    let meta = crate::magician_v2::execution::agentic::ApiReplayMeta {
        attempted: true,
        succeeded: replay.success,
        time_ms: Some(replay.timing_ms),
        fallback_reason: if replay.success {
            None
        } else {
            replay
                .error
                .clone()
                .or_else(|| replay.fallback_reason.clone())
                .or_else(|| Some("API replay did not verify; falling back to browser".to_string()))
        },
        replay_url: Some(request.url.clone()),
        replay_status: Some(replay.status),
        replay_body: replay.response_body.clone(),
    };
    set_api_replay_meta(exec_ctx, meta);

    if !replay.success {
        return None;
    }

    let url_template = runner
        .registry()
        .get_capability(&origin, &capability_id)
        .map(|capability| capability.url_template)
        .unwrap_or_else(|error| {
            tracing::warn!(
                target: "magician::api_mining",
                origin = %origin,
                capability_id = %capability_id,
                error = %error,
                "api_mining.projection_ingest.capability_load_failed"
            );
            request.url.clone()
        });
    ingest_successful_api_replay_projection(
        exec_ctx,
        &base_path,
        &origin,
        &capability_id,
        &url_template,
        &replay,
    );

    record_api_sequence_step(
        exec_ctx,
        &origin,
        &capability_id,
        &request,
        request_params,
        &replay,
    );
    Some(replay_action_result(
        &capability_id,
        &origin,
        &request,
        &replay,
    ))
}

fn api_replay_approval_denied_result(
    origin: &str,
    capability_id: &str,
    request: &crate::magician_v2::api_mining::replay::ReplayRequest,
    reason: &str,
) -> ActionResult {
    ActionResult::Browser {
        data: json!({
            "success": false,
            "api_replay_approval_required": true,
            "api_replayed": false,
            "origin": origin,
            "capability_id": capability_id,
            "method": request.method.clone(),
            "url": redact_url_for_approval(&request.url),
            "reason": reason,
            "error": "Direct write API replay was not approved, so the backend stopped before executing the equivalent browser mutation.",
        }),
    }
}

async fn request_api_replay_approval(
    exec_ctx: &PrimitiveExecCtx,
    origin: &str,
    capability_id: &str,
    request: &crate::magician_v2::api_mining::replay::ReplayRequest,
    confidence: &crate::magician_v2::api_mining::capability::ConfidenceLevel,
    side_effects: &crate::magician_v2::api_mining::capability::SideEffects,
    reason: &str,
) -> bool {
    let Some(service) = exec_ctx.user_request_service.as_ref() else {
        tracing::warn!(
            target: "magician::api_mining",
            origin = %origin,
            capability_id = %capability_id,
            "api_replay.write_hitl.no_user_request_service"
        );
        return false;
    };

    use crate::magician_v2::api_mining::approval::{
        apply_decision, build_api_replay_approval_request, replay_request_shape_fingerprint,
        ApprovalSubject,
    };
    use crate::magician_v2::api_mining::replay_grants::{
        is_denylisted_url_template, GrantKey, ReplayGrantStore,
    };

    let grants = ReplayGrantStore::open(scoped_api_mining_base_path(exec_ctx));
    let principal = exec_ctx
        .principal
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_WORKSPACE);
    let recipe_metrics =
        crate::magician_v2::api_mining::recipe_metrics_for_scope(principal, workspace);
    let grant_key = GrantKey {
        recipe_id: None,
        step_id: None,
        capability_id: Some(capability_id.to_owned()),
        request_shape_fingerprint: replay_request_shape_fingerprint(
            &request.method,
            &request.url,
            request.body.as_deref(),
        ),
    };
    let durable_grant_allowed =
        crate::magician_v2::api_mining::recipe::request_body_shape_is_grantable(
            request.body.as_deref(),
        );
    if durable_grant_allowed
        && !is_denylisted_url_template(&request.url)
        && grants.lookup(&grant_key).is_some()
    {
        recipe_metrics.record_grant_used();
        return true;
    }

    let preview = redacted_replay_request_preview(request);
    let subject = ApprovalSubject {
        grant_key,
        origin: origin.to_owned(),
        method: request.method.clone(),
        url_template: request.url.clone(),
        side_effects: side_effects.clone(),
        request_preview: json!({
            "request": preview,
            "capability_id": capability_id,
            "confidence": confidence,
        }),
        durable_grant_allowed,
        policy_reason: reason.to_owned(),
        owner_agent_id: exec_ctx.agent_id.clone(),
    };
    let request = build_api_replay_approval_request(
        &subject,
        exec_ctx
            .principal
            .as_deref()
            .unwrap_or(DEFAULT_SCOPE_PRINCIPAL),
        exec_ctx
            .workspace
            .as_deref()
            .unwrap_or(DEFAULT_SCOPE_WORKSPACE),
        exec_ctx.execution_id.clone(),
        exec_ctx.task_id.clone(),
    );
    let response = service.ask(request).await;
    let created =
        response.decision == crate::magician_v2::api_mining::approval::OPTION_APPROVE_ALWAYS_STEP;
    let approved = apply_decision(&grants, &subject, &response.decision, &response.request_id)
        .unwrap_or_else(|error| {
            tracing::warn!(
                target: "magician::api_mining",
                capability_id,
                %error,
                "api_replay.write_hitl.decision_failed"
            );
            false
        });
    if approved && created {
        recipe_metrics.record_grant_created();
    } else if !approved {
        recipe_metrics.record_approval_denied();
    }
    approved
}

fn redacted_replay_request_preview(
    request: &crate::magician_v2::api_mining::replay::ReplayRequest,
) -> Value {
    let mut header_names: Vec<_> = request
        .headers
        .keys()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    header_names.sort_unstable();
    header_names.dedup();

    json!({
        "method": request.method.clone(),
        "url": redact_url_for_approval(&request.url),
        "header_names": header_names,
        "timeout_ms": request.timeout_ms,
        "body_shape": crate::magician_v2::api_mining::recipe::request_body_shape(
            request.body.as_deref(),
        ),
    })
}

fn redact_url_for_approval(url: &str) -> String {
    crate::magician_v2::api_mining::approval::approval_url_shape(url)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    #[test]
    fn capture_substitutes_an_engine_that_records_request_bodies() {
        use super::{
            engine_for_capture, BUNDLED_BROWSER_ENGINE_NAME, LIGHTPANDA_BROWSER_ENGINE_NAME,
        };
        // Capture on: an engine that records no request bodies is replaced.
        assert_eq!(
            engine_for_capture(Some(LIGHTPANDA_BROWSER_ENGINE_NAME), true),
            Some(BUNDLED_BROWSER_ENGINE_NAME)
        );
        // Capture off: the caller keeps the engine it asked for.
        assert_eq!(
            engine_for_capture(Some(LIGHTPANDA_BROWSER_ENGINE_NAME), false),
            Some(LIGHTPANDA_BROWSER_ENGINE_NAME)
        );
        // Any other choice is left alone, including "no preference".
        assert_eq!(
            engine_for_capture(Some("cloak-browser"), true),
            Some("cloak-browser")
        );
        assert_eq!(engine_for_capture(None, true), None);
    }

    use super::*;

    fn read_result(stdout: &str) -> PrimitiveToolResult {
        PrimitiveToolResult {
            success: true,
            stdout: stdout.to_string(),
            ..PrimitiveToolResult::default()
        }
    }

    #[test]
    fn browser_read_materializes_a_claim_eligible_document_for_an_opened_page() {
        let arguments = json!({"args": ["https://example.org/releases/latest"]});
        let value = browser_read_document_result(
            &read_result(
                "Announcing Rust 1.98.1\n\nThe Rust team is happy to announce a new version.",
            ),
            &arguments,
            None,
        )
        .expect("document result");

        assert_eq!(value["kind"], json!("document"));
        assert_eq!(value["status"], json!("complete"));
        assert_eq!(value["fetch_status"], json!("complete"));
        assert_eq!(value["evidence_role"], json!("opened_page"));
        assert_eq!(value["claim_eligible"], json!(true));
        assert_eq!(
            value["document"]["canonical_url"],
            json!("https://example.org/releases/latest")
        );
        assert_eq!(value["document"]["title"], json!("Announcing Rust 1.98.1"));
        assert_eq!(
            value["document"]["identity"]["adapter_id"],
            json!("browser-read")
        );
        assert_eq!(
            value["document"]["provenance"]["retrieved_by"],
            json!("browser-read")
        );
        assert_eq!(
            value["document"]["provenance"]["source_label"],
            json!("example.org")
        );
        assert_eq!(value["excerpt_complete"], json!(true));
        assert!(value["excerpt"]
            .as_str()
            .is_some_and(|excerpt| excerpt.contains("1.98.1")));
    }

    #[test]
    fn browser_read_reports_a_degraded_shell_as_discovery_only() {
        let value = browser_read_document_result(
            &read_result("Sign in to continue to your account"),
            &json!({"args": []}),
            Some("https://example.org/private".to_string()),
        )
        .expect("document result");

        assert_eq!(value["status"], json!("degraded"));
        assert_eq!(value["quality_findings"], json!(["login_wall"]));
        assert_eq!(value["claim_eligible"], json!(false));
        assert_eq!(value["evidence_role"], json!("discovery_only"));
        assert_eq!(value["fetch_status"], json!("partial"));
        assert_eq!(
            value["document"]["canonical_url"],
            json!("https://example.org/private")
        );
    }

    #[test]
    fn browser_read_keeps_the_legacy_text_result_when_there_is_no_page() {
        let arguments = json!({"args": []});
        assert!(browser_read_document_result(&read_result("   "), &arguments, None).is_none());
        let mut failed = read_result("boom");
        failed.success = false;
        assert!(browser_read_document_result(&failed, &arguments, None).is_none());
        let mut structured = read_result("{\"ok\":true}");
        structured.parsed_json = Some(json!({"ok": true}));
        assert!(browser_read_document_result(&structured, &arguments, None).is_none());
        // No URL at all still yields a document keyed by content.
        let value = browser_read_document_result(&read_result("Plain page text"), &arguments, None)
            .expect("active-tab document");
        assert!(value["document"]["identity"]["item_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("active-tab:")));
        assert!(value["document"].get("canonical_url").is_none());
    }

    fn ctx() -> PrimitiveExecCtx {
        PrimitiveExecCtx::default_for_runtime()
    }

    fn successful_replay(
        body: serde_json::Value,
    ) -> crate::magician_v2::api_mining::types::ReplayResult {
        crate::magician_v2::api_mining::types::ReplayResult {
            success: true,
            capability_id: "cap-items".to_string(),
            replay_method: "rust_reqwest".to_string(),
            status: 200,
            response_headers: Some(HashMap::new()),
            response_body: Some(body.to_string()),
            timing_ms: 7,
            verification: None,
            error: None,
            fallback_reason: None,
            auth_failure: false,
        }
    }

    fn workflow_for_selection(
        id: &str,
        first_capability_id: Option<&str>,
        maturity: crate::magician_v2::api_mining::workflow::WorkflowMaturity,
        successful_replays: u32,
        fallback_all_steps: bool,
    ) -> crate::magician_v2::api_mining::workflow::WorkflowGraph {
        use crate::magician_v2::api_mining::workflow::{
            AuthRequirements, BrowserFallbackStep, ReplayStats, WorkflowConfidence, WorkflowGraph,
            WorkflowStep,
        };

        let fallback = fallback_all_steps.then(|| BrowserFallbackStep {
            action: "click".to_string(),
            arguments: serde_json::json!({"args":["#submit"]}),
            description: Some("browser__click #submit".to_string()),
        });
        WorkflowGraph {
            id: id.to_string(),
            origin_key: "https://example.com".to_string(),
            name: id.to_string(),
            steps: vec![
                WorkflowStep {
                    id: "step_0".to_string(),
                    step_index: 0,
                    capability_id: first_capability_id.map(str::to_string),
                    param_sources: HashMap::new(),
                    skip_if: None,
                    browser_only: false,
                    browser_fallback: fallback.clone(),
                },
                WorkflowStep {
                    id: "step_1".to_string(),
                    step_index: 1,
                    capability_id: Some("cap_second".to_string()),
                    param_sources: HashMap::new(),
                    skip_if: None,
                    browser_only: false,
                    browser_fallback: fallback,
                },
            ],
            data_flows: vec![],
            auth_requirements: AuthRequirements::default(),
            confidence: WorkflowConfidence {
                workflow_level: maturity,
                step_confidences: HashMap::new(),
            },
            compiled_from_sequence_ids: vec!["seq_1".to_string()],
            last_compiled_at_ms: 100,
            last_replayed_at_ms: None,
            replay_stats: ReplayStats {
                successful_replays,
                failed_replays: 0,
            },
        }
    }

    #[test]
    fn workflow_takeover_candidate_requires_first_capability_match_and_fallbacks() {
        use crate::magician_v2::api_mining::workflow::WorkflowMaturity;

        let matched = workflow_for_selection(
            "wf_matched",
            Some("cap_first"),
            WorkflowMaturity::Candidate,
            0,
            true,
        );
        assert!(workflow_is_safe_live_takeover_candidate(
            &matched,
            "cap_first"
        ));

        let wrong_first = workflow_for_selection(
            "wf_wrong",
            Some("cap_other"),
            WorkflowMaturity::Candidate,
            0,
            true,
        );
        assert!(!workflow_is_safe_live_takeover_candidate(
            &wrong_first,
            "cap_first"
        ));

        let missing_fallback = workflow_for_selection(
            "wf_no_fallback",
            Some("cap_first"),
            WorkflowMaturity::Candidate,
            0,
            false,
        );
        assert!(!workflow_is_safe_live_takeover_candidate(
            &missing_fallback,
            "cap_first"
        ));
    }

    #[test]
    fn workflow_takeover_candidates_sort_by_maturity_success_and_size() {
        use crate::magician_v2::api_mining::workflow::WorkflowMaturity;

        let mut workflows = vec![
            workflow_for_selection(
                "draft",
                Some("cap_first"),
                WorkflowMaturity::Draft,
                50,
                true,
            ),
            workflow_for_selection(
                "trusted",
                Some("cap_first"),
                WorkflowMaturity::Trusted,
                1,
                true,
            ),
            workflow_for_selection(
                "validated",
                Some("cap_first"),
                WorkflowMaturity::Validated,
                9,
                true,
            ),
        ];
        sort_workflow_takeover_candidates(&mut workflows);

        assert_eq!(workflows[0].id, "trusted");
        assert_eq!(workflows[1].id, "validated");
        assert_eq!(workflows[2].id, "draft");
    }

    #[test]
    fn api_replay_approval_preview_redacts_sensitive_material() {
        let request = crate::magician_v2::api_mining::replay::ReplayRequest {
            method: "POST".to_string(),
            url: "https://api.example.com/cart?api_key=secret&page=1".to_string(),
            headers: HashMap::from([
                (
                    "authorization".to_string(),
                    "Bearer secret-token".to_string(),
                ),
                ("cookie".to_string(), "SID=secret-cookie".to_string()),
                ("accept".to_string(), "application/json".to_string()),
            ]),
            timeout_ms: 10_000,
            body: Some(
                r#"{"item":"coffee","password":"secret","nested":{"csrfToken":"abc"}}"#.to_string(),
            ),
        };

        let preview = redacted_replay_request_preview(&request);
        let rendered = preview.to_string();

        assert!(rendered.contains("api_key=[REDACTED]"));
        assert!(rendered.contains("page=[REDACTED]"));
        assert!(rendered.contains("authorization"));
        assert!(rendered.contains("cookie"));
        assert!(rendered.contains("accept"));
        assert!(rendered.contains("key:$/item"));
        assert!(rendered.contains("key:$/password"));
        assert!(rendered.contains("key:$/nested/csrfToken"));
        assert!(!rendered.contains("page=1"));
        assert!(!rendered.contains("coffee"));
        assert!(!rendered.contains("application/json"));
        assert!(!rendered.contains("secret-token"));
        assert!(!rendered.contains("secret-cookie"));
        assert!(!rendered.contains("\"secret\""));
        assert!(!rendered.contains("\"abc\""));
    }

    #[test]
    fn api_replay_approval_denial_is_failed_browser_result() {
        let request = crate::magician_v2::api_mining::replay::ReplayRequest {
            method: "POST".to_string(),
            url: "https://approval-user:approval-password@api.example.com/order/customer-123?token=secret&item=private-item#private-fragment".to_string(),
            headers: HashMap::new(),
            timeout_ms: 10_000,
            body: None,
        };

        let result = api_replay_approval_denied_result(
            "https://api.example.com",
            "cap-order",
            &request,
            "write_replay_requires_hitl_approval",
        );

        assert!(!result.is_success());
        match result {
            ActionResult::Browser { data } => {
                assert_eq!(
                    data.get("api_replayed").and_then(Value::as_bool),
                    Some(false)
                );
                assert_eq!(
                    data.get("api_replay_approval_required")
                        .and_then(Value::as_bool),
                    Some(true)
                );
                assert_eq!(
                    data.get("url").and_then(Value::as_str),
                    Some("https://api.example.com/<2 path segments>?item=[REDACTED]&token=[REDACTED]")
                );
                let rendered = data.to_string();
                for private_value in [
                    "approval-user",
                    "approval-password",
                    "/order",
                    "customer-123",
                    "secret",
                    "private-item",
                    "private-fragment",
                ] {
                    assert!(!rendered.contains(private_value), "leaked {private_value}");
                }
            },
            other => panic!("expected browser-shaped failure, got {other:?}"),
        }
    }

    // The full dispatch path is integration-tested elsewhere (it requires
    // a live OperationLlmRouter and PromptManager). Primitive dispatch
    // takes no parameters from the outer LLM; there is no per-call
    // argument validation surface to unit-test here.

    #[test]
    fn ctx_default_resolves_storage_path() {
        // Smoke test: the runtime default ctx is constructable and has a
        // non-empty storage path. `dispatch_primitive` depends on
        // this for resolving the CLI binary.
        let c = ctx();
        assert!(!c.storage_base_path.as_os_str().is_empty());
    }

    /// Pins the wiring, not just the resolver: the ceiling the flat dispatch
    /// applies is the one carried on `PrimitiveExecCtx`, so a ceiling that
    /// failed to be threaded from the agent definition would show up here as a
    /// call that resolves to the owner's Chrome.
    ///
    /// Built from `ConnectionMode` values directly rather than through
    /// `from_call_arguments`, which reads process-global env; a sibling test in
    /// `browser::session` mutates that variable and cargo runs both in one
    /// process.
    #[test]
    fn the_exec_ctx_ceiling_is_what_gates_an_ordinary_browser_call() {
        // Default ctx = no declared ceiling = today's behaviour, cdp and all.
        let unrestricted = ctx();
        assert!(unrestricted.browser_transports.is_empty());
        let resolved = BrowserTransportCeiling::parse(&unrestricted.browser_transports)
            .unwrap()
            .resolve(ConnectionMode::default())
            .unwrap();
        assert!(matches!(resolved, ConnectionMode::Cdp { .. }));

        // web-researcher's ceiling: cdp refused, headless allowed.
        let confined =
            ctx().with_browser_transports(vec!["headless".to_string(), "headed".to_string()]);
        let ceiling = BrowserTransportCeiling::parse(&confined.browser_transports).unwrap();
        assert!(ceiling.resolve(ConnectionMode::default()).is_err());
        assert_eq!(
            ceiling.resolve(ConnectionMode::Headless).unwrap(),
            ConnectionMode::Headless
        );

        // A typo in the declared ceiling refuses the browse outright rather
        // than being dropped on the floor.
        let typo = ctx().with_browser_transports(vec!["headles".to_string()]);
        assert!(BrowserTransportCeiling::parse(&typo.browser_transports).is_err());
    }

    /// Pins the session-reuse hole the request-time ceiling cannot see.
    ///
    /// A chat thread deliberately shares one `browser_session_id_override`
    /// across every agent rooted on it ("PA's direct calls, EA delegations,
    /// web-researcher handovers"), and the flat session cache is keyed by
    /// session id alone. Without this check a `[headless, headed]` agent asks
    /// for `headless`, passes the request-time ceiling, and is handed the CDP
    /// session an unrestricted agent opened on that thread — browsing as the
    /// owner without ever requesting it.
    #[test]
    fn a_reused_session_is_held_to_the_ceiling_the_request_already_passed() {
        let restricted = vec!["headless".to_string(), "headed".to_string()];
        let owners_chrome = ConnectionMode::Cdp {
            url: "ws://127.0.0.1:3003/devtools/browser/magician-chat-t1".to_string(),
        };

        let error = enforce_ceiling_on_attached_session(&restricted, &owners_chrome)
            .expect_err("a cached cdp session must not be handed to a headless-only agent");
        let message = error.to_string();
        assert!(message.contains("NOT BROWSED"), "{message}");
        assert!(message.contains("already attached"), "{message}");

        // A permitted transport passes, and an agent that declared nothing is
        // unchanged — the ceiling stays opt-in on this seam too.
        assert!(
            enforce_ceiling_on_attached_session(&restricted, &ConnectionMode::Headless).is_ok()
        );
        let unrestricted: Vec<String> = Vec::new();
        assert!(enforce_ceiling_on_attached_session(&unrestricted, &owners_chrome).is_ok());
    }

    #[test]
    fn typed_retrieval_handoff_requires_exact_mode_and_magicutor_proxy() {
        let configured_cdp_url =
            crate::magician_v2::execution::primitive_dispatch::browser::DEFAULT_MAGICUTOR_PROXY_URL;
        let args = json!({
            "retrieval_session_id": "retrieval-cdp-rh_abc",
            "connection_mode": "cdp",
        });
        let (_, expected) = retrieval_handoff_session_id(&args).unwrap().unwrap();
        let mode = ConnectionMode::from_exact_request(&args, configured_cdp_url).unwrap();
        validate_retrieval_handoff_mode(&mode, expected, configured_cdp_url).unwrap();

        let wrong_mode = json!({
            "retrieval_session_id": "retrieval-cdp-rh_abc",
            "connection_mode": "headless",
        });
        let mode = ConnectionMode::from_exact_request(&wrong_mode, configured_cdp_url).unwrap();
        assert!(validate_retrieval_handoff_mode(&mode, "cdp", configured_cdp_url).is_err());

        let wrong_proxy = json!({
            "retrieval_session_id": "retrieval-cdp-rh_abc",
            "connection_mode": "cdp",
            "cdp_url": "ws://127.0.0.1:9999/devtools/browser/unsafe",
        });
        let mode = ConnectionMode::from_exact_request(&wrong_proxy, configured_cdp_url).unwrap();
        assert!(validate_retrieval_handoff_mode(&mode, "cdp", configured_cdp_url).is_err());
    }

    #[test]
    fn retrieval_handoff_rejects_untyped_or_malformed_session_ids() {
        assert!(retrieval_handoff_session_id(&json!({
            "retrieval_session_id": "magician-ordinary"
        }))
        .is_err());
        assert!(retrieval_handoff_session_id(&json!({
            "retrieval_session_id": "retrieval-headless-rh_bad/escape"
        }))
        .is_err());
    }

    #[test]
    fn browser_engine_override_precedes_config_and_config_precedes_bundled_default() {
        let mut configured = ctx();
        configured.browser_engine = Some("configured-browser".into());
        assert_eq!(
            requested_browser_engine(&json!({}), &configured),
            Some("configured-browser")
        );
        assert_eq!(
            requested_browser_engine(&json!({"engine": "custom-browser"}), &configured),
            Some("custom-browser")
        );
        assert_eq!(requested_browser_engine(&json!({}), &ctx()), None);
    }

    fn tool_result(
        stdout: &str,
        artifacts: Vec<super::super::artifacts::PrimitiveArtifact>,
    ) -> PrimitiveToolResult {
        PrimitiveToolResult {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
            parsed_json: None,
            artifacts,
            elapsed_ms: 0,
        }
    }

    #[test]
    fn browser_primitive_carries_screenshot_via_browser_result() {
        // Phase 6 on-demand vision: an explicit browser__screenshot yields a
        // screenshot artifact, which the fold carries as ActionResult::Browser
        // with the on-disk path for the outer loop to thread as an image.
        let shot = super::super::artifacts::PrimitiveArtifact::from_path(
            "screenshot",
            "screenshot",
            "call-1",
            std::path::PathBuf::from("/tmp/shot-xyz.png"),
        );
        match browser_primitive_action_result(tool_result("captured", vec![shot])) {
            ActionResult::Browser { data } => {
                assert_eq!(
                    data.get("screenshot_path").and_then(|v| v.as_str()),
                    Some("/tmp/shot-xyz.png")
                );
                assert_eq!(
                    data.get("output").and_then(|v| v.as_str()),
                    Some("captured")
                );
            },
            other => panic!("expected ActionResult::Browser, got {other:?}"),
        }
    }

    #[test]
    fn browser_primitive_without_screenshot_is_text() {
        // A text primitive (e.g. browser__snapshot's accessibility tree) carries
        // no screenshot artifact, so it flows through as ActionResult::Text.
        match browser_primitive_action_result(tool_result("- button \"Submit\"", vec![])) {
            ActionResult::Text { content } => assert!(content.contains("Submit")),
            other => panic!("expected ActionResult::Text, got {other:?}"),
        }
        // A non-screenshot artifact (e.g. a download) also stays Text.
        let dl = super::super::artifacts::PrimitiveArtifact::from_path(
            "download",
            "download",
            "call-2",
            std::path::PathBuf::from("/tmp/file.pdf"),
        );
        assert!(matches!(
            browser_primitive_action_result(tool_result("done", vec![dl])),
            ActionResult::Text { .. }
        ));
    }

    #[test]
    fn successful_api_replay_ingests_projection_rows_after_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let principal = format!("projection-test-{}", uuid::Uuid::new_v4());
        let workspace = "default".to_string();
        let mut ctx = PrimitiveExecCtx::default_for_runtime();
        ctx.principal = Some(principal.clone());
        ctx.workspace = Some(workspace.clone());

        let base_path = tmp.path().join("api_mining");
        let origin = "https://x.com";
        let url_template = "https://x.com/api/items";
        let replay = successful_replay(json!({
            "items": [
                { "id": "a", "name": "Alpha" },
                { "id": "b", "name": "Beta" }
            ]
        }));

        ingest_successful_api_replay_projection(
            &ctx,
            &base_path,
            origin,
            "cap-items",
            url_template,
            &replay,
        );

        let pipeline = crate::magician_v2::api_mining::projection_pipeline::pipeline_for_scope(
            &principal,
            &workspace,
            &base_path.join("projections"),
        )
        .unwrap();
        let projection_id = match pipeline.query_known_resource(origin, "items", None, &[]) {
            Err(
                crate::magician_v2::api_mining::projection_pipeline::QueryKnownResourceError::PendingApproval {
                    projection_id,
                },
            ) => projection_id,
            other => panic!("expected pending projection, got {other:?}"),
        };
        pipeline.approve_projection(&projection_id).unwrap();

        ingest_successful_api_replay_projection(
            &ctx,
            &base_path,
            origin,
            "cap-items",
            url_template,
            &replay,
        );

        let (rows, served_from) = pipeline
            .query_known_resource(origin, "items", None, &[])
            .unwrap();
        assert_eq!(
            served_from,
            crate::magician_v2::api_mining::projection_pipeline::ServedFrom::Projection
        );
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn browser_takeover_candidate_skips_multi_mutation_batch() {
        let candidate = browser_takeover_candidate(
            "batch",
            &serde_json::json!({
                "commands": [
                    ["fill", "@e1", "query"],
                    ["click", "@e2"]
                ]
            }),
            Some("https://shop.example.com/search"),
        );
        assert!(
            candidate.is_none(),
            "multi-mutation batches must fall back to browser execution"
        );
    }

    #[test]
    fn browser_takeover_candidate_skips_browser_navigation() {
        let open_candidate = browser_takeover_candidate(
            "open",
            &serde_json::json!({"args": ["https://acme.keka.com"]}),
            None,
        );
        assert!(
            open_candidate.is_none(),
            "browser open must navigate the tab instead of replaying a mined API"
        );

        let batch_candidate = browser_takeover_candidate(
            "batch",
            &serde_json::json!({
                "commands": [
                    ["open", "https://acme.keka.com"],
                    ["click", "@e54"]
                ]
            }),
            Some("https://acme.keka.com"),
        );
        assert!(
            batch_candidate.is_none(),
            "batches that include navigation must stay on the browser rail"
        );
    }

    #[test]
    fn browser_action_events_capture_stable_signature() {
        let events = browser_action_events_for_call(
            "fill",
            &serde_json::json!({"args": ["input[name=q]", "coke zero"]}),
            Some("https://shop.example.com/products/42"),
            1_780_000_000_000,
        );
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.action_type, "Type");
        assert_eq!(
            event.action_signature.as_deref(),
            Some("selector=input[name=q]|submit=false|iframe=")
        );
        assert_eq!(event.page_path_template.as_deref(), Some("/products/{id}"));
        assert_eq!(
            event.action_params.get("text").map(String::as_str),
            Some("coke zero")
        );
    }

    #[test]
    fn browser_open_emits_page_load_action_event() {
        let events = browser_action_events_for_call(
            "open",
            &serde_json::json!({"args": ["https://hn.algolia.com/?q=rust&tags=story"]}),
            Some("https://hn.algolia.com/?q=rust&tags=story"),
            1_780_000_000_000,
        );

        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.action_type, "PageLoad");
        assert_eq!(event.page_origin.as_deref(), Some("https://hn.algolia.com"));
        assert_eq!(event.page_path_template.as_deref(), Some("/"));
        assert_eq!(
            event.action_params.get("query").map(String::as_str),
            Some("rust")
        );
        assert_eq!(
            event
                .action_params
                .get("page_query_tags")
                .map(String::as_str),
            Some("story")
        );
        assert_eq!(
            event.action_signature.as_deref(),
            Some("https://hn.algolia.com/|query_keys=q,tags")
        );
    }

    #[test]
    fn browser_eval_click_emits_bindable_click_event() {
        let script = r#"(() => {
  const btn = [...document.querySelectorAll('button')]
    .find(b => (b.innerText || '').trim() === 'Web Clock-In');
  if (!btn) return { clicked: false, reason: 'Web Clock-In button not found' };
  btn.click();
  return { clicked: true, text: (btn.innerText || '').trim() };
})()"#;
        let events = browser_action_events_for_call(
            "eval",
            &serde_json::json!({"args": [script]}),
            Some("https://app.example.com/dashboard"),
            1_780_000_000_000,
        );

        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.action_type, "Click");
        assert_eq!(
            event.action_signature.as_deref(),
            Some("eval_click|label=Web Clock-In|iframe=")
        );
        assert_eq!(
            event.semantic_signature.as_deref(),
            Some("click:text:web clock-in")
        );
        assert_eq!(
            event.action_params.get("text").map(String::as_str),
            Some("Web Clock-In")
        );
        assert!(
            event.user_values.is_empty(),
            "button labels are not request user values and must not require value-match correlation"
        );
    }

    #[test]
    fn live_browser_action_correlation_persists_eval_click_binding() {
        use crate::magician_v2::api_mining::capability::{ApiCapability, ConfidenceLevel};
        use crate::magician_v2::api_mining::registry::CapabilityRegistry;
        use crate::magician_v2::api_mining::types::{
            NetworkTraceEvent, RequestInitiator, RequestTiming,
        };

        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("api_mining");
        let origin = "https://app.example.com";
        let url = "https://app.example.com/api/webclockin";
        let mut capability = ApiCapability::new(
            "web_clock_in".to_string(),
            origin.to_string(),
            "POST".to_string(),
            url.to_string(),
        );
        capability.confidence = ConfidenceLevel::Candidate;
        capability.sample_count = 2;
        capability.body_fingerprint = Some("json_keys:timestamp".to_string());
        capability.body_template =
            Some(r#"{"timestamp":{{string:body_payload_timestamp}}}"#.to_string());

        let mut registry = CapabilityRegistry::with_base_path(&base_path).unwrap();
        registry.register(&capability).unwrap();

        let script = r#"(() => {
  const btn = [...document.querySelectorAll('button')]
    .find(b => (b.innerText || '').trim() === 'Web Clock-In');
  btn.click();
})()"#;
        let action_events = browser_action_events_for_call(
            "eval",
            &serde_json::json!({"args": [script]}),
            Some("https://app.example.com/dashboard"),
            1_000,
        );
        let trace = NetworkTraceEvent {
            request_id: "req-1".to_string(),
            method: "POST".to_string(),
            url: url.to_string(),
            resource_type: Some("XHR".to_string()),
            frame_id: None,
            tab_id: None,
            thread_id: None,
            request_headers: HashMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]),
            request_body: Some(r#"{"timestamp":"2026-06-16T19:56:34.298Z"}"#.to_string()),
            response_headers: HashMap::new(),
            response_body: Some(r#"{"ok":true}"#.to_string()),
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            status: 200,
            timing: RequestTiming {
                request_time: 1.1,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 25.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".to_string(),
                stack: None,
                url: Some("https://app.example.com/dashboard".to_string()),
            },
            timestamp: 1_100,
            request_size: 48,
            response_size: 11,
            capture_source: None,
        };
        let mut ctx = PrimitiveExecCtx::default_for_runtime();
        ctx.api_mining_base_path = Some(base_path.clone());

        let promotion =
            promote_correlated_browser_action_capability(&ctx, &action_events, &[trace])
                .expect("live action binding promotion");

        assert_eq!(promotion.capability_id, capability.id);
        assert_eq!(promotion.method, "POST");
        assert!(
            !promotion
                .request_params
                .contains_key("body_payload_timestamp"),
            "sequence params must not freeze runtime timestamp defaults"
        );

        let registry = CapabilityRegistry::with_base_path(&base_path).unwrap();
        let stored = registry.get_capability(origin, &capability.id).unwrap();
        assert_eq!(stored.action_bindings.len(), 1);
        let binding = &stored.action_bindings[0];
        assert_eq!(
            binding.action_signature,
            "eval_click|label=Web Clock-In|iframe="
        );
        assert_eq!(
            binding
                .default_params
                .get("body_payload_timestamp")
                .map(String::as_str),
            Some("__magician_runtime:now_iso_utc")
        );
    }

    #[test]
    fn browser_navigation_url_tracks_tab_new() {
        let url = browser_navigation_url_from_call(
            "tab",
            &serde_json::json!({"args": ["new", "https://hn.algolia.com/api/v1/search?query=rust&tags=story"]}),
        );

        assert_eq!(
            url.as_deref(),
            Some("https://hn.algolia.com/api/v1/search?query=rust&tags=story")
        );
    }

    #[test]
    fn browser_navigation_url_tracks_eval_location_href() {
        let url = browser_navigation_url_from_call(
            "eval",
            &serde_json::json!({
                "args": ["(() => { location.href = 'https://hn.algolia.com/api/v1/items/22238335'; return 'navigating'; })()"]
            }),
        );

        assert_eq!(
            url.as_deref(),
            Some("https://hn.algolia.com/api/v1/items/22238335")
        );
    }

    #[test]
    fn browser_navigation_url_ignores_non_navigation_eval() {
        let url = browser_navigation_url_from_call(
            "eval",
            &serde_json::json!({
                "args": ["(() => 'https://hn.algolia.com/api/v1/items/22238335')()"]
            }),
        );

        assert!(url.is_none());
    }

    #[test]
    fn browser_json_get_url_template_parameterizes_detail_id() {
        let url = url::Url::parse("https://hn.algolia.com/api/v1/items/22238335").unwrap();
        let template = browser_json_get_url_template(&url).unwrap();
        assert_eq!(template, "https://hn.algolia.com/api/v1/items/{item_id}");
        let params = crate::magician_v2::api_mining::router::extract_template_params(
            &template,
            url.as_str(),
        );
        assert_eq!(params.get("item_id").map(String::as_str), Some("22238335"));
    }

    #[test]
    fn browser_json_get_url_template_parameterizes_query_values() {
        let url =
            url::Url::parse("https://api.example.com/search?q=rust&page=2&tags=story").unwrap();
        let template = browser_json_get_url_template(&url).unwrap();
        assert_eq!(
            template,
            "https://api.example.com/search?page={query_page}&q={query_q}&tags={query_tags}"
        );
        let params = crate::magician_v2::api_mining::router::extract_template_params(
            &template,
            url.as_str(),
        );
        assert_eq!(params.get("query_q").map(String::as_str), Some("rust"));
        assert_eq!(params.get("query_page").map(String::as_str), Some("2"));
        assert_eq!(params.get("query_tags").map(String::as_str), Some("story"));
    }

    #[test]
    fn browser_json_document_body_accepts_raw_json_object() {
        let result = PrimitiveToolResult {
            success: true,
            stdout: r#"{"id":22238335,"title":"Why Discord is switching"}"#.to_string(),
            stderr: String::new(),
            parsed_json: None,
            artifacts: Vec::new(),
            elapsed_ms: 1,
        };
        let (body, parsed) = extract_browser_json_document_body(&result).unwrap();
        assert!(body.contains("22238335"));
        assert_eq!(
            parsed.get("id").and_then(serde_json::Value::as_i64),
            Some(22238335)
        );
    }
}
