//! Turn-engine resolution and the harness decide arm (plane Task 8).
//!
//! The seam sits inside `phases::decide::run`, not at `execute_agent_cycle`.
//! `magician` stays the default. Unknown engine names fail closed to the loop.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use once_cell::sync::Lazy;
use tokio_util::sync::CancellationToken;

use crate::config::{default_harness_turn_max_seconds, default_harness_turn_max_tool_calls};

use crate::magician_v2::execution::agentic::types::{
    account_execution_tokens, preflight_execution_token_budget, AgenticContext, Artifact,
    EnvironmentState, ExecutionHistory, LoopProtectiveState, UserInputType,
};
use crate::magician_v2::execution::agentic::{ActionExecutors, Decision};
use crate::magician_v2::execution::plane::engine::{
    HarnessError, HarnessSession, HarnessSessionRequest, HarnessStopReason, HarnessStreamSink,
    HarnessTurnInput, HarnessTurnSettled, HarnessUsage, PlaneEndpoint,
};
use crate::magician_v2::execution::plane::grant::{
    plane_grant_registry, PlaneGrant, PlanePendingApproval, PlaneTurnStopReason, RevokeGrantOnDrop,
};
// By crate path, not `super::`: `magician-bin`'s execution-harness contract
// compiles this file on its own, and the pin must be the library's type —
// the one `AgenticContext::run_engine_pin` holds.
use crate::magician_v2::execution::plane::engine_pin::RunEnginePin;

#[derive(Debug, Clone)]
pub struct HarnessEngineSnapshot {
    pub engine: String,
    /// The model the driving harness should run; `default` = the CLI's
    /// own choice. Rides the CLI's model flag when the engines spawn.
    pub harness_model: String,
    /// Magician profile name Pi uses for agentic turns, or Pi's own default.
    pub pi_profile: Option<String>,
    pub turn_max_tool_calls: u32,
    pub turn_max_seconds: u64,
    pub plane_endpoint: String,
}

impl Default for HarnessEngineSnapshot {
    fn default() -> Self {
        Self {
            engine: "magician".to_string(),
            harness_model: "default".to_string(),
            pi_profile: None,
            turn_max_tool_calls: default_harness_turn_max_tool_calls(),
            turn_max_seconds: default_harness_turn_max_seconds(),
            plane_endpoint: "http://127.0.0.1:8080/api/magician/v2/plane/mcp".to_string(),
        }
    }
}

static SNAPSHOT: Lazy<RwLock<HarnessEngineSnapshot>> =
    Lazy::new(|| RwLock::new(HarnessEngineSnapshot::default()));

/// Install / reload the process snapshot (`POST /settings/magician-config/reload`).
pub fn install_harness_engine_snapshot(snapshot: HarnessEngineSnapshot) {
    *SNAPSHOT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot;
}

pub fn harness_engine_snapshot() -> HarnessEngineSnapshot {
    SNAPSHOT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The Settings choice as a pin: the engine, harness model, and Pi profile
/// the process snapshot names right now.
pub fn settings_pin(snapshot: &HarnessEngineSnapshot) -> RunEnginePin {
    RunEnginePin {
        engine: snapshot.engine.clone(),
        harness_model: snapshot.harness_model.clone(),
        pi_profile: snapshot.pi_profile.clone(),
    }
}

/// The pin a run's harness turn drives with. A context pinned at launch uses
/// its pin. A context with none (a run launched before pins existed) reads
/// the snapshot for its engine, as before.
fn effective_run_pin(ctx: &AgenticContext, snapshot: &HarnessEngineSnapshot) -> RunEnginePin {
    let engine = run_engine_for(ctx);
    match ctx.run_engine_pin.as_ref() {
        Some(pin) if pin.engine == engine => pin.clone(),
        _ => RunEnginePin::for_engine(&engine, &settings_pin(snapshot)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEngine {
    MagicianDecision,
    Harness(&'static str),
}

/// Resolve who thinks this iteration. Unknown names fail closed to the loop.
pub fn resolve_turn_engine(named: Option<&str>) -> TurnEngine {
    match named.map(str::trim).unwrap_or("") {
        "" | "magician" => TurnEngine::MagicianDecision,
        "pi" => TurnEngine::Harness("pi"),
        "claude_code" => TurnEngine::Harness("claude_code"),
        "codex" => TurnEngine::Harness("codex"),
        "codex_app_server" => TurnEngine::Harness("codex_app_server"),
        "grok" => TurnEngine::Harness("grok"),
        "agy" => TurnEngine::Harness("agy"),
        _ => TurnEngine::MagicianDecision,
    }
}

/// The engine a run thinks with, from its launch pin alone: the pin when it
/// names one (plane Task 6b — a plane-started run inherits the grant's
/// harness, and `"magician"` on the grant is a deliberate pin), the process
/// snapshot only as the fallback. [`run_engine_for`] reads the pin off the
/// live context; a terminal path reads it off the run's durable attenuation.
pub fn run_engine_for_pin(pinned: Option<&str>) -> String {
    pinned
        .filter(|named| !named.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| harness_engine_snapshot().engine)
}

/// The engine a run thinks with. One choice for the decide seam and for the
/// run's background operations, so the two cannot disagree.
pub fn run_engine_for(ctx: &AgenticContext) -> String {
    run_engine_for_pin(ctx.harness_engine.as_deref())
}

/// The parent engine of a run's background operations, from its launch pin
/// alone: the run's engine, unless that is the native loop, which names no
/// parent.
pub fn run_parent_engine_for_pin(pinned: Option<&str>) -> Option<String> {
    crate::magician_v2::query_analysis::parent_engine::normalize_parent_engine(Some(
        &run_engine_for_pin(pinned),
    ))
}

/// The parent engine of a run's background operations, read off the live
/// context: the launch pin first (a conversation grant pins the chat mouth
/// there, a run its engine); else a parent the context's routing overrides
/// already carry — the tier a terminal grant uses, naming the MCP client's
/// family with no engine pinned; every ingress strips a wire-supplied one
/// and `merge` never carries one, so a carried parent is the runtime's; else
/// the process snapshot. The decide seam keeps thinking with
/// [`run_engine_for`] — a carried parent routes, it does not decide.
pub fn run_parent_engine(ctx: &AgenticContext) -> Option<String> {
    let pinned = ctx
        .harness_engine
        .as_deref()
        .filter(|named| !named.trim().is_empty());
    let carried = ctx
        .llm_routing_overrides
        .as_ref()
        .and_then(|overrides| overrides.parent_engine.as_deref());
    match (pinned, carried) {
        (Some(pinned), _) => run_parent_engine_for_pin(Some(pinned)),
        (None, Some(carried)) => {
            crate::magician_v2::query_analysis::parent_engine::normalize_parent_engine(Some(
                carried,
            ))
        },
        (None, None) => run_parent_engine_for_pin(None),
    }
}

/// Process-local continuation state for one execution's harness turns.
///
/// `native_session_id` survives every settle so consecutive turns warm-resume
/// one harness conversation. The budget fields persist only across an
/// approval pause — a normally settled turn resets them — which is plane
/// Task 8's "HITL as interrupt, not you-lost-the-brain" rule: after the human
/// answers, the harness gets the remainder of its bound, not a fresh one.
#[derive(Default, Clone, Debug)]
struct HarnessTurnContinuation {
    native_session_id: Option<String>,
    /// [`RunEnginePin::session_fingerprint`] of the turn that produced
    /// `native_session_id`. A later turn under another fingerprint starts
    /// cold instead of handing one engine's session id to another.
    session_fingerprint: Option<String>,
    paused_grant_token: Option<String>,
    turn_tool_calls_spent: u32,
    turn_wall_clock_spent_ms: u64,
    /// The result of an action Magician executed while paused (execute-on-
    /// resume). Delivered into the next turn's input exactly once; the
    /// harness's own session transcript carries it from there.
    prior_result_note: Option<String>,
    /// The gated action captured by the plane when this turn ended for
    /// approval. Consumed by the pause-publication path, which stamps it into
    /// the durable pause so resume executes exactly this action.
    pending_approval: Option<PlanePendingApproval>,
}

static TURN_CONTINUATIONS: Lazy<RwLock<HashMap<String, HarnessTurnContinuation>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

fn turn_continuation(execution_id: &str) -> HarnessTurnContinuation {
    TURN_CONTINUATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(execution_id.to_string())
        .or_default()
        .clone()
}

fn store_turn_continuation(execution_id: &str, continuation: HarnessTurnContinuation) {
    TURN_CONTINUATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(execution_id.to_string(), continuation);
}

/// The native id and any undelivered result note belong to a resumable run.
/// Once the agentic cycle has a terminal outcome, keeping that process-local
/// state serves no continuation and retains potentially sensitive text.
pub(crate) fn forget_terminal_harness_continuation(execution_id: &str) {
    TURN_CONTINUATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(execution_id);
}

/// A stopped turn may still have a governed tool dispatch in flight. Keep its
/// grant until that dispatch releases the grant-wide lock, then revoke it.
/// This also cleans up a paused run that the operator never resumes.
fn revoke_paused_grant_after_dispatch(
    token: String,
    grant: PlaneGrant,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _dispatch_guard = grant.lock_dispatch().await;
        plane_grant_registry().revoke(&token).await;
    })
}

/// The durable half of a harness pause, stamped into the pause record when a
/// harness turn's approval gate pauses the run (plane Task 8).
///
/// `action_*` is the gated action resume must execute itself — depending on a
/// foreign harness to re-issue a byte-identical call is the replay mechanism,
/// and it does not survive a process that owes us nothing. The session and
/// budget fields carry the warm continuation so a resumed turn inherits the
/// same native conversation and only the remainder of its bounds.
#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HarnessPauseContinuation {
    pub action_json: Option<String>,
    pub action_summary: Option<String>,
    pub action_type: Option<String>,
    pub native_session_id: Option<String>,
    /// Fingerprint of the pin that produced `native_session_id`. A pause
    /// written before fingerprints existed has none and resumes cold.
    #[serde(default)]
    pub session_fingerprint: Option<String>,
    pub turn_tool_calls_spent: u32,
    pub turn_wall_clock_spent_ms: u64,
}

/// Read (without consuming) the gated action of an approval-paused harness
/// turn, so the apply phase can surface it as an ordinary confirmation pause.
pub(crate) fn peek_harness_pending_approval(
    execution_id: &str,
) -> Option<crate::magician_v2::execution::plane::grant::PlanePendingApproval> {
    TURN_CONTINUATIONS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(execution_id)
        .and_then(|continuation| continuation.pending_approval.clone())
}

/// Read (without consuming) the full continuation of an approval-paused
/// harness turn, in the durable form the pause record carries.
///
/// Deliberately non-destructive: terminal-receipt publication can be retried
/// after a partial failure, and a consuming read on the first attempt would
/// strip the continuation from every retry. The live entry is superseded when
/// resume restores the durable form, and a capture cannot outlive its turn
/// (the decide seam stores only the current turn's gate capture).
pub(crate) fn peek_harness_pause_continuation(
    execution_id: &str,
) -> Option<HarnessPauseContinuation> {
    TURN_CONTINUATIONS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(execution_id)
        .map(|continuation| {
            let pending = &continuation.pending_approval;
            HarnessPauseContinuation {
                action_json: pending.as_ref().map(|pending| pending.action_json.clone()),
                action_summary: pending
                    .as_ref()
                    .map(|pending| pending.action_summary.clone()),
                action_type: pending.as_ref().map(|pending| pending.action_type.clone()),
                native_session_id: continuation.native_session_id.clone(),
                session_fingerprint: continuation.session_fingerprint.clone(),
                turn_tool_calls_spent: continuation.turn_tool_calls_spent,
                turn_wall_clock_spent_ms: continuation.turn_wall_clock_spent_ms,
            }
        })
}

/// Rehydrate the live continuation of an approval-paused harness turn from
/// its durable pause record, so the resumed turn warm-resumes the same native
/// session with the remainder of its bounds. `prior_result_note` is the
/// result of the action Magician executed on resume; the next decide delivers
/// it into the turn input once.
pub(crate) async fn restore_harness_continuation_from_pause(
    execution_id: &str,
    pause: &HarnessPauseContinuation,
    prior_result_note: Option<String>,
) {
    let previous = TURN_CONTINUATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            execution_id.to_string(),
            HarnessTurnContinuation {
                native_session_id: pause.native_session_id.clone(),
                session_fingerprint: pause.session_fingerprint.clone(),
                paused_grant_token: None,
                turn_tool_calls_spent: pause.turn_tool_calls_spent,
                turn_wall_clock_spent_ms: pause.turn_wall_clock_spent_ms,
                prior_result_note,
                pending_approval: None,
            },
        );
    // Resume replaces the live continuation before the next decide. Revoke
    // the paused turn's grant here; otherwise its token is lost and stays in
    // the registry for the rest of this process's lifetime. Wait for a
    // dispatch that was still in flight at pause publication to finish.
    if let Some(token) = previous.and_then(|entry| entry.paused_grant_token) {
        if let Some(grant) = plane_grant_registry().resolve_run_scoped(&token).await {
            let _dispatch_guard = grant.lock_dispatch().await;
            plane_grant_registry().revoke(&token).await;
        }
    }
}

/// The saved native session a turn may resume: only one this same engine,
/// model, and profile made. A session saved under another fingerprint — or
/// by a pause written before fingerprints existed — starts the turn cold.
fn resumable_session_id(
    continuation: &HarnessTurnContinuation,
    session_fingerprint: &str,
    supports_resume: bool,
) -> Option<String> {
    (supports_resume && continuation.session_fingerprint.as_deref() == Some(session_fingerprint))
        .then(|| continuation.native_session_id.clone())
        .flatten()
}

/// One key for every harness-continuation lookup, so the decide seam, the
/// apply-phase peek, and pause publication cannot drift onto different
/// identity rules (and silently never meet on legacy-id executions).
pub(crate) fn harness_continuation_key(ctx: &AgenticContext) -> String {
    ctx.execution_id
        .clone()
        .or_else(|| ctx.legacy_execution_id.clone())
        .unwrap_or_else(|| "harness-anonymous".to_string())
}

/// The wall-clock remainder of a turn whose bound survives an approval pause.
/// `Some(ZERO)` means no Magician-side ceiling; `None` means the bound was
/// fully consumed by the pause and the turn cannot start at all.
fn remaining_turn_timeout(bound_secs: u64, spent_ms: u64) -> Option<Duration> {
    if bound_secs == 0 {
        return Some(Duration::ZERO);
    }
    let remaining_ms = bound_secs.saturating_mul(1000).saturating_sub(spent_ms);
    if remaining_ms == 0 {
        return None;
    }
    // A positive sub-second remainder still gets one full second: rounding it
    // down to zero would read as "no ceiling" inside the engine.
    Some(Duration::from_millis(remaining_ms.max(1000)))
}

/// Static-catalog harnesses take their only tool snapshot at startup. Load
/// the run's permitted indexed tools before that snapshot, and keep search
/// from unloading names the harness still believes it can call.
pub(crate) fn preload_run_tools(grant: &mut PlaneGrant, relists: bool) {
    if relists {
        return;
    }
    let names: std::collections::HashSet<_> = grant
        .allowed_tools
        .iter()
        .filter(|name| grant.permits(name) && grant.tool_index.get(name).is_some())
        .cloned()
        .collect();
    grant.preloaded_deferred = !names.is_empty();
    grant.replace_loaded_tools(names);
}

/// Run a harness turn in place of Magician decide, when configured.
///
/// Returns `Ok(None)` so the built-in decision path proceeds.
pub async fn maybe_harness_decide(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    observed_state: &EnvironmentState,
    pending_operator_steer: &[String],
    cancellation_token: Option<&CancellationToken>,
    iteration: usize,
) -> Result<Option<Decision>> {
    // Protected app runs must not think through a foreign harness: their
    // prompts, tool results, and model output are disclosure-guarded
    // continuation content, and a spawned CLI owes us no such boundary.
    // Fail closed to Magician's own decision path.
    if ctx.app_disclosure_guard.is_some() {
        return Ok(None);
    }
    if !matches!(
        resolve_turn_engine(Some(&run_engine_for(ctx))),
        TurnEngine::Harness(_)
    ) {
        return Ok(None);
    }

    let snapshot = harness_engine_snapshot();
    preflight_execution_token_budget()?;
    let execution_id = harness_continuation_key(ctx);
    let continuation = turn_continuation(&execution_id);
    // A grant retained by a paused turn is dead authority now: this decide is
    // a new turn on a fresh grant. It was kept only so an in-flight dispatch
    // could finish without its cancellation token being tripped mid-action.
    if let Some(paused) = &continuation.paused_grant_token {
        plane_grant_registry().revoke(paused).await;
    }

    // A bound fully consumed by the pause cannot start another child. Report
    // the exhausted bound explicitly; an empty Yield is a misleading
    // no-progress terminal, not a request to continue the loop.
    let turn_timeout = match remaining_turn_timeout(
        snapshot.turn_max_seconds,
        continuation.turn_wall_clock_spent_ms,
    ) {
        Some(remaining) => remaining,
        None => {
            store_turn_continuation(
                &execution_id,
                HarnessTurnContinuation {
                    native_session_id: continuation.native_session_id,
                    session_fingerprint: continuation.session_fingerprint,
                    ..HarnessTurnContinuation::default()
                },
            );
            return Ok(Some(Decision::Failed {
                reason: "harness turn wall-clock bound was consumed before approval resume".into(),
            }));
        },
    };

    let run_engine = run_engine_for(ctx);
    let pin = effective_run_pin(ctx, &snapshot);
    let session_fingerprint = pin.session_fingerprint();
    let pi_profile = if run_engine == "pi" {
        if let Some(name) = pin.pi_profile.as_deref() {
            let router = executors
                .operation_llm_router
                .as_ref()
                .and_then(|router| router.router_config_snapshot())
                .or_else(|| {
                    crate::magician_v2::query_analysis::operation_llm_router::global_operation_router()
                        .and_then(|router| router.router_config_snapshot())
                })
                .ok_or_else(|| anyhow::anyhow!("Pi run profile requires a configured LLM router"))?;
            Some(
                crate::magician_v2::query_analysis::multi_llm_service::resolve_pi_profile_config(
                    &router, name,
                )?,
            )
        } else {
            None
        }
    } else {
        None
    };
    let engine = crate::magician_v2::execution::plane::engines::harness_engine_for(&run_engine)
        .expect("the roster gate above guarantees the factory can build this engine");
    let mut grant = PlaneGrant::for_run(
        ctx.clone(),
        Arc::new(executors.clone()),
        execution_id.clone(),
    )
    .with_turn_tool_budget(
        snapshot.turn_max_tool_calls,
        continuation.turn_tool_calls_spent,
    );
    if let Some(cancel) = cancellation_token {
        // Revoking a turn must cancel its in-flight actions without ever
        // cancelling the parent run. Parent cancellation still reaches both.
        grant.cancellation_token = Some(cancel.child_token());
    }
    preload_run_tools(&mut grant, engine.capabilities().tools_list_changed);
    let token = plane_grant_registry().mint(grant).await;
    let mut grant_lease = RevokeGrantOnDrop::new(token.clone());

    let mut text = harness_execution_input(ctx, history).await;
    // A parent resumed after its delegated children settled is shown the same
    // block the native decision prompt renders — their deliverables, not a
    // path each — or it cannot state what they found.
    text.push_str(
        &crate::magician_v2::execution::agentic::build_delegation_results_section(
            executors.screenshot_storage.as_ref(),
            ctx.execution_id.as_deref(),
        )
        .await,
    );
    if let Some(note) = &continuation.prior_result_note {
        text.push_str(
            "\n\nWhile paused, Magician executed the action a human approved. Its result:\n",
        );
        text.push_str(note);
    }
    if !pending_operator_steer.is_empty() {
        text.push_str("\n\nOperator steer:\n");
        for line in pending_operator_steer {
            text.push_str("- ");
            text.push_str(line);
            text.push('\n');
        }
    }

    let supports_resume = engine.capabilities().supports_resume;
    let resume_session_id =
        resumable_session_id(&continuation, &session_fingerprint, supports_resume);
    let req = HarnessSessionRequest {
        planning_only: false,
        endpoint: PlaneEndpoint {
            url: snapshot.plane_endpoint.clone(),
        },
        grant: token.clone(),
        system_prompt: crate::magician_v2::secrets::sanitize_text_for_provider(&ctx.goal),
        model: Some(pin.harness_model.clone()).filter(|m| m != "default"),
        pi_profile,
        pi_images: Vec::new(),
        cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        env_allowlist: Vec::new(),
        cancel: cancellation_token.cloned(),
        resume_session_id: resume_session_id.clone(),
        turn_timeout,
        turn_idle_timeout: Some(std::time::Duration::from_secs(
            crate::config::default_harness_turn_idle_seconds(),
        )),
        // The loop seam keeps the pre-parity shape: a private temp home per
        // session. A persistent home is the chat mouth's (`chat_turn`).
        native_home: None,
    };
    let turn_started = std::time::Instant::now();
    let turn_result: std::result::Result<
        (HarnessTurnSettled, Box<dyn HarnessSession>),
        HarnessError,
    > = async {
        let mut session = engine.start(&req).await?;
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: crate::magician_v2::secrets::sanitize_text_for_provider(&text),
                    // Already included in text above, once for every engine.
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await?;
        Ok((settled, session))
    }
    .await;
    if !cancellation_token.is_some_and(CancellationToken::is_cancelled) {
        let health = match &turn_result {
            Ok((settled, session)) => session.service_health(settled),
            Err(error) => {
                crate::magician_v2::realtime_events::ServiceFailure::from_error(&error.to_string())
                    .map(Err)
            },
        };
        if let Some(health) = health {
            crate::magician_v2::decision_host::report_health(
                ctx.principal.as_deref().unwrap_or("anonymous"),
                ctx.workspace.as_deref().unwrap_or("default"),
                &req.health_service(&pin.engine),
                health,
            );
        }
    }
    let (mut settled, mut session) = match turn_result {
        Ok(pair) => pair,
        Err(harness_error) => {
            // The seam owns the grant even if an engine fails before it can
            // construct a session (and therefore has no Drop to release it).
            plane_grant_registry().revoke(&token).await;
            grant_lease.disarm();
            // Preserve the conversation identity, the undelivered result
            // note, and the pre-turn budget counters — a retry must not
            // inherit spend that never bought a turn, nor lose the warm
            // session to a failed spawn. The paused grant is already revoked
            // above, and the engine released its own grant on the error path.
            store_turn_continuation(
                &execution_id,
                HarnessTurnContinuation {
                    native_session_id: continuation.native_session_id,
                    session_fingerprint: continuation.session_fingerprint,
                    turn_tool_calls_spent: continuation.turn_tool_calls_spent,
                    turn_wall_clock_spent_ms: continuation.turn_wall_clock_spent_ms,
                    prior_result_note: continuation.prior_result_note,
                    ..HarnessTurnContinuation::default()
                },
            );
            return Err(anyhow::anyhow!("harness turn failed: {harness_error}"));
        },
    };
    // A parent Stop wins even if the grant had just latched an approval or
    // delegation stop while the child was tearing down. A cancelled run must
    // not publish a new confirmation pause or retain that grant.
    if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
        settled.stop_reason = HarnessStopReason::Cancelled;
        settled.assistant_text.clear();
    }
    tracing::info!(
        execution_id = %execution_id,
        engine = %run_engine_for(ctx),
        iteration,
        stop_reason = ?settled.stop_reason,
        input_tokens = settled.usage.as_ref().map(|usage| usage.input_tokens),
        cached_input_tokens = settled.usage.as_ref().map(|usage| usage.cached_input_tokens),
        output_tokens = settled.usage.as_ref().map(|usage| usage.output_tokens),
        total_tokens = settled.usage.as_ref().map(HarnessUsage::total_tokens),
        "agentic harness turn settled"
    );
    if let (Some(sink), Some(scope)) = (
        executors.canonical_event_sink.as_ref(),
        executors
            .canonical_event_scope
            .as_ref()
            .filter(|scope| scope.execution_id == execution_id),
    ) {
        // Generic AgentEvent envelopes are transport-only. Persist this
        // execution fact on the canonical rail so post-run evaluation and
        // diagnostics can prove which harness actually drove the turn.
        sink.emit(
            scope.clone(),
            crate::magician_v2::artifact_v2::ArtifactV2EventType::ExecutionProgress,
            serde_json::json!({
                "kind": "harness_turn_settled",
                "task_id": ctx.task_id,
                "execution_id": execution_id,
                "engine": run_engine_for(ctx),
                "iteration": iteration,
                "stop_reason": format!("{:?}", settled.stop_reason),
                "input_tokens": settled.usage.as_ref().map(|usage| usage.input_tokens),
                "cached_input_tokens": settled.usage.as_ref().filter(|u| u.cache_read_reported).map(|usage| usage.cached_input_tokens),
                "cache_creation_tokens": settled.usage.as_ref().and_then(|u| u.cache_creation_tokens),
                "cost_usd": settled.usage.as_ref().and_then(|u| u.cost_usd),
                "usage_availability": settled.usage.as_ref().map(|u| u.availability()).unwrap_or_default(),
                "output_tokens": settled.usage.as_ref().map(|usage| usage.output_tokens),
                "total_tokens": settled.usage.as_ref().map(HarnessUsage::total_tokens),
            }),
        );
    }
    let _ = loop_protective
        .loop_detector
        .check_after_turn(observed_state);
    // The grant, not the engine, is the authority on a delegation stop: an
    // engine that never observes the turn-stop signal (the one-shot CLIs)
    // settles on its own and would report the delegation as an ordinary
    // answer, targets and all discarded.
    let delegation_stop = settled.stop_reason != HarnessStopReason::Cancelled
        && plane_grant_registry()
            .resolve_run_scoped(&token)
            .await
            .is_some_and(|grant| grant.turn_stop_reason() == Some(PlaneTurnStopReason::Delegate));
    let paused = matches!(settled.stop_reason, HarnessStopReason::NeedsApproval) || delegation_stop;
    let elapsed_ms = turn_started.elapsed().as_millis() as u64;
    let native_session_id = if supports_resume {
        // A refusal may mean the seeded native id could not be loaded.
        // Keep it only when this engine confirmed a live session this turn.
        if settled.stop_reason == HarnessStopReason::Refused {
            settled.native_session_id.clone()
        } else {
            settled.native_session_id.clone().or(resume_session_id)
        }
    } else {
        None
    };
    let native_session_fingerprint = native_session_id
        .as_ref()
        .map(|_| session_fingerprint.clone());

    let mut pending_delegation = None;
    let decision = if paused {
        // The engine stopped the child and retained the grant. Carry the
        // session identity, the gated action the plane captured, and the
        // unspent remainder of both bounds across the pause so the resumed
        // turn continues this conversation with only what is left of its
        // budget — and so resume executes exactly the approved action.
        let paused_grant = plane_grant_registry().resolve_run_scoped(&token).await;
        let (spent_calls, pending_approval) = paused_grant
            .as_ref()
            .map(|grant| {
                if delegation_stop {
                    pending_delegation = grant.take_pending_delegation();
                }
                (grant.turn_tool_calls_spent(), grant.take_pending_approval())
            })
            .unwrap_or((continuation.turn_tool_calls_spent, None));
        store_turn_continuation(
            &execution_id,
            HarnessTurnContinuation {
                native_session_id,
                session_fingerprint: native_session_fingerprint,
                paused_grant_token: Some(token.clone()),
                turn_tool_calls_spent: spent_calls,
                turn_wall_clock_spent_ms: continuation
                    .turn_wall_clock_spent_ms
                    .saturating_add(elapsed_ms),
                prior_result_note: None,
                // Only THIS turn's gate capture. Preserving an older capture
                // here would let a later, differently-shaped NeedUserInput
                // hijack it into a confirmation pause and execute a stale
                // action under an approval the human gave to another question.
                pending_approval,
            },
        );
        if let Some(grant) = paused_grant {
            let _ = revoke_paused_grant_after_dispatch(token.clone(), grant);
        }
        grant_lease.disarm();
        if delegation_stop {
            delegation_decision_from_harness(&settled, pending_delegation)
        } else {
            decision_from_harness(&settled)
        }
    } else {
        session.shutdown().await;
        plane_grant_registry().revoke(&token).await;
        grant_lease.disarm();
        // The logical turn is over: the next turn warm-resumes the same
        // native conversation but with a fresh budget.
        store_turn_continuation(
            &execution_id,
            HarnessTurnContinuation {
                native_session_id,
                session_fingerprint: native_session_fingerprint,
                ..HarnessTurnContinuation::default()
            },
        );
        decision_from_harness(&settled)
    };
    settle_harness_usage(
        paused || settled.stop_reason == HarnessStopReason::Cancelled,
        settled.usage.as_ref(),
    )?;
    Ok(Some(decision))
}

/// Account a settled turn's usage against the run's token budget.
///
/// A turn Magician itself stopped mid-flight — for an approval, delegation,
/// or parent cancellation — can settle with no usage because Magician killed
/// the child, not because the CLI hid its bill. The missing-usage rule exists
/// for an ordinary settled turn and fails closed by exhausting the run's
/// whole budget; applied to an interrupted turn it ended resumed parents with
/// "20000000/20000000 reached" before it could decide, while the engines that
/// never observe the stop signal settled with real usage and resumed fine.
/// An interrupted turn that reports usage is still charged.
fn settle_harness_usage(interrupted: bool, usage: Option<&HarnessUsage>) -> Result<()> {
    match (interrupted, usage) {
        (true, None) => Ok(()),
        (true, Some(_)) => {
            let _ = account_harness_usage(usage);
            Ok(())
        },
        (false, _) => account_harness_usage(usage),
    }
}

/// Foreign turns need the same run identity and continuation evidence as the
/// built-in decision path. In particular a retried terminal must see the
/// evidence gate's rejection, rather than repeat the original goal blindly.
/// The preamble text, when the store has no entry and no manager is installed.
/// `magician-core`'s `rendered_prompt_or` warns and returns this, so a missing
/// file degrades the turn to known-good framing instead of an empty prompt.
const HARNESS_PREAMBLE_FALLBACK: &str =
    "You are executing a Magician task. Use the governed Magician plane tools \
     for actions and tool_search to discover additional tools. Do not use native \
     shell tools or search for runtime credentials. Perform the requested work, \
     then return the final answer with the observed result. Report any unfinished \
     work or blocker honestly.\n\n{identity_line}";

/// Which agent this turn *is*, and whether it has anywhere to delegate. A
/// harness that does not know it is the worker goes looking for one: grok asked
/// which agent owns the desktop capability, was answered with the agent it
/// already was, and delegated to itself three times. The plane no longer
/// advertises delegation to an agent with no targets; saying so here keeps the
/// model from hunting for a route that does not exist.
fn harness_identity_line(ctx: &AgenticContext) -> String {
    let Some(agent_id) = &ctx.agent_id else {
        return String::new();
    };
    let mut line = format!("You are the agent `{agent_id}`.");
    if ctx.delegation_targets.is_empty() {
        line.push_str(
            " You are the worker for this task: there is no other agent to hand it to, \
             and the tools you already hold are the tools for this job. Do the work \
             yourself.",
        );
    }
    line
}

/// Foreign turns need the same run identity and continuation evidence as the
/// built-in decision path. In particular a retried terminal must see the
/// evidence gate's rejection, rather than repeat the original goal blindly.
///
/// The preamble is store-managed (`harness_execution_preamble` v1.0.0 in
/// `data/magician_v2/prompts/`); the literal above is the fallback. Only that
/// framing is authored copy — the goal, task id, prior knowledge and recent
/// history are run context and stay assembled here, where a missing placeholder
/// cannot become a silent prompt regression.
async fn harness_execution_input(ctx: &AgenticContext, history: &ExecutionHistory) -> String {
    let variables = std::collections::HashMap::from([(
        "identity_line".to_string(),
        harness_identity_line(ctx),
    )]);
    let identity = harness_identity_line(ctx);
    let preamble = magician_core::prompts::rendered_prompt_or(
        "harness_execution_preamble",
        "1.0.0",
        variables,
        HARNESS_PREAMBLE_FALLBACK,
    )
    .await;
    // `rendered_prompt_or` renders the store's template, but hands back the
    // fallback verbatim — placeholders and all. Substituting here covers both
    // paths: a no-op once the store has rendered, and the only substitution
    // when a missing file or an unset manager fell through to the literal.
    let preamble = preamble.replace("{identity_line}", &identity);
    let mut text = format!("{}\n\nGoal:\n{}", preamble.trim_end(), ctx.goal);
    if let Some(task_id) = &ctx.task_id {
        text.push_str(&format!("\n\nCurrent task id: {task_id}"));
    }
    if let Some(knowledge) = &ctx.prior_environment_knowledge {
        text.push_str("\n\nPrior execution context:\n");
        text.push_str(knowledge);
    }
    let recent = history.format_for_llm(8);
    if !recent.trim().is_empty() {
        text.push_str("\n\nRecent execution history (including any rejected completion):\n");
        text.push_str(&recent);
    }
    text
}

fn account_harness_usage(usage: Option<&HarnessUsage>) -> Result<()> {
    let Some(usage) = usage else {
        tracing::debug!("harness turn settled without usage; unmetered (the CLI bills itself)");
        return Ok(());
    };
    account_execution_tokens(usage.input_tokens.saturating_add(usage.output_tokens))?;
    Ok(())
}

/// A delegation stop hands the loop the targets the harness captured. A stop
/// with nothing captured is a failure the loop can see, never a silent
/// completion built from whatever text the harness left behind.
fn delegation_decision_from_harness(
    settled: &crate::magician_v2::execution::plane::engine::HarnessTurnSettled,
    pending: Option<Vec<crate::magician_v2::execution::actions::DelegationTargetRequest>>,
) -> Decision {
    match pending {
        Some(targets) if !targets.is_empty() => Decision::DelegateToAgent { targets },
        _ => Decision::Failed {
            reason: format!(
                "harness turn ended to delegate but captured no targets ({:?}): {}",
                settled.stop_reason,
                if settled.assistant_text.trim().is_empty() {
                    "no final answer"
                } else {
                    &settled.assistant_text
                },
            ),
        },
    }
}

fn decision_from_harness(
    settled: &crate::magician_v2::execution::plane::engine::HarnessTurnSettled,
) -> Decision {
    match settled.stop_reason {
        HarnessStopReason::NeedsApproval => Decision::NeedUserInput {
            question: if settled.assistant_text.is_empty() {
                "Approval required for a plane action.".to_string()
            } else {
                settled.assistant_text.clone()
            },
            input_type: UserInputType::Confirmation {
                confirm_label: Some("Approve".to_string()),
                deny_label: Some("Deny".to_string()),
                destructive: false,
            },
            hint: Some(
                "Magician will execute the approved action on resume; the harness is paused, not cancelled."
                    .to_string(),
            ),
            options: None,
        },
        // The answer is the run's deliverable, typed as one, so the task
        // publishes it byte-preserving as a magician yield's answer is
        // published — not re-synthesised and judged, both riding the harness.
        HarnessStopReason::Settled if !settled.assistant_text.trim().is_empty() => Decision::Completed {
            evidence: Some(settled.assistant_text.clone()),
            artifacts: vec![Artifact::task_deliverable(settled.assistant_text.clone())],
        },
        HarnessStopReason::Stalled => Decision::Failed {
            reason: format!(
                "harness turn made no progress within its idle bound and was ended{}",
                if settled.assistant_text.trim().is_empty() {
                    String::new()
                } else {
                    format!("; last text: {}", settled.assistant_text.trim())
                }
            ),
        },
        HarnessStopReason::Settled | HarnessStopReason::Refused
        | HarnessStopReason::Cancelled | HarnessStopReason::TurnBudgetSpent
        | HarnessStopReason::Delegate => Decision::Failed {
            reason: format!(
                "harness turn ended ({:?}): {}",
                settled.stop_reason,
                if settled.assistant_text.trim().is_empty() { "no final answer" } else { &settled.assistant_text },
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(stop_reason: HarnessStopReason, text: &str) -> HarnessTurnSettled {
        HarnessTurnSettled {
            assistant_text: text.into(),
            stop_reason,
            usage: None,
            native_session_id: None,
        }
    }

    /// Neither a turn Magician stopped for a delegation or an approval, nor one
    /// whose CLI simply did not report a bill, exhausts the run's budget. The
    /// first was always exempt so a resumed parent could decide; the second
    /// became exempt when a harness turn stopped being metered at all — the
    /// subscription CLI bills itself, and condemning the cap on one silent turn
    /// ended runs that were doing real work.
    #[tokio::test]
    async fn a_turn_magician_stopped_does_not_exhaust_the_budget_for_missing_usage() {
        crate::magician_v2::execution::agentic::types::with_execution_token_meter(
            0,
            1_000,
            async {
                settle_harness_usage(true, None)
                    .expect("a paused turn without usage is not charged");
                assert_eq!(
                    crate::magician_v2::execution::agentic::types::execution_token_budget_snapshot(
                    ),
                    Some((0, 1_000))
                );
                settle_harness_usage(false, None)
                    .expect("a settled turn without usage is unmetered, not condemned");
                assert_eq!(
                    crate::magician_v2::execution::agentic::types::execution_token_budget_snapshot(
                    ),
                    Some((0, 1_000)),
                    "an unreported bill charges nothing"
                );
            },
        )
        .await;
    }

    /// A delegation stop hands the loop the targets the harness captured; a
    /// stop with nothing captured is a failure, never a silent completion.
    #[test]
    fn a_delegation_stop_becomes_the_loops_delegation_decision() {
        let targets = vec![
            crate::magician_v2::execution::actions::DelegationTargetRequest {
                target_agent_id: "researcher".to_string(),
                context: "count the lines in the fixture".to_string(),
                input_artifact_ids: Vec::new(),
                input_data: None,
                depth: None,
                timeout_secs: None,
                spend_token_ids: Vec::new(),
                required_capability: None,
                expected_artifacts: Vec::new(),
            },
        ];
        match delegation_decision_from_harness(
            &settled(HarnessStopReason::Delegate, ""),
            Some(targets),
        ) {
            Decision::DelegateToAgent { targets } => {
                assert_eq!(targets.len(), 1);
                assert_eq!(targets[0].target_agent_id, "researcher");
            },
            other => panic!("a delegation stop must become DelegateToAgent, got {other:?}"),
        }
        assert!(matches!(
            delegation_decision_from_harness(&settled(HarnessStopReason::Delegate, ""), None),
            Decision::Failed { .. }
        ));
    }

    /// The settled answer is the run's deliverable, typed as one: a magician
    /// yield's answer is materialised as `task_deliverable` and published
    /// byte-preserving, while an untyped text artifact is not a verified
    /// terminal projection — the task-user output is then re-synthesised and
    /// judged for grounding, both riding the harness, and every switched
    /// engine paid a repair round after every completed run.
    #[test]
    fn a_successful_harness_answer_is_a_deliverable_not_an_empty_yield() {
        let decision =
            decision_from_harness(&settled(HarnessStopReason::Settled, "4 lines; cedar"));
        match decision {
            Decision::Completed {
                evidence,
                artifacts,
            } => {
                assert_eq!(evidence.as_deref(), Some("4 lines; cedar"));
                assert_eq!(artifacts.len(), 1);
                assert_eq!(artifacts[0].data, b"4 lines; cedar");
                assert_eq!(
                    artifacts[0].artifact_type.as_deref(),
                    Some("task_deliverable")
                );
                assert_eq!(artifacts[0].name, "task_deliverable.md");
                assert_eq!(artifacts[0].content_type, "text/markdown");
            },
            other => {
                panic!("a completed reply must reach normal evidence/output handling: {other:?}")
            },
        }
    }

    #[test]
    fn unsuccessful_harness_stops_never_claim_completion() {
        for stop in [
            HarnessStopReason::Refused,
            HarnessStopReason::Cancelled,
            HarnessStopReason::TurnBudgetSpent,
        ] {
            assert!(matches!(
                decision_from_harness(&settled(stop, "partial work")),
                Decision::Failed { .. }
            ));
        }
        assert!(matches!(
            decision_from_harness(&settled(HarnessStopReason::Settled, " \n")),
            Decision::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn harness_tokens_share_the_execution_budget_and_missing_usage_is_unmetered() {
        use crate::magician_v2::execution::agentic::types::{
            execution_token_budget_snapshot, with_execution_token_meter,
        };
        with_execution_token_meter(10, 100, async {
            account_harness_usage(Some(&HarnessUsage {
                input_tokens: 60,
                cached_input_tokens: 0,
                output_tokens: 30,
                ..Default::default()
            }))
            .unwrap();
            assert_eq!(execution_token_budget_snapshot(), Some((100, 100)));
            assert!(preflight_execution_token_budget().is_err());
            assert!(account_harness_usage(Some(&HarnessUsage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 0,
                ..Default::default()
            }))
            .is_err());
            assert_eq!(execution_token_budget_snapshot(), Some((101, 100)));
        })
        .await;
        // A harness turn that cannot report usage is unmetered: the CLI bills
        // itself, and condemning the whole budget on one such turn ended runs
        // that were doing real work.
        with_execution_token_meter(0, 100, async {
            account_harness_usage(None).expect("missing harness usage is unmetered");
            assert_eq!(execution_token_budget_snapshot(), Some((0, 100)));
            assert!(preflight_execution_token_budget().is_ok());
        })
        .await;
        // Unmetered fixture runs do not acquire a new implicit budget.
        account_harness_usage(None).unwrap();
    }

    /// With no prompt manager installed the store cannot answer, so the turn
    /// falls back to the compiled preamble — the run context it assembles must
    /// be unchanged either way.
    #[tokio::test]
    async fn execution_input_carries_task_identity_and_prior_work() {
        let mut ctx = AgenticContext::default();
        ctx.goal = "Finish the calculation".into();
        ctx.task_id = Some("task-fixture".into());
        ctx.prior_environment_knowledge = Some("The source contains four lines".into());
        let text = harness_execution_input(&ctx, &ExecutionHistory::default()).await;
        assert!(text.contains("Finish the calculation"));
        assert!(text.contains("task-fixture"));
        assert!(text.contains("The source contains four lines"));
        assert!(text.contains("tool_search"));
        // No agent id: the identity placeholder resolves to nothing rather than
        // leaving `{identity_line}` in the prompt.
        assert!(!text.contains("{identity_line}"), "{text}");
    }

    /// The identity line names the agent, and tells a worker it is the worker.
    #[test]
    fn the_identity_line_says_when_there_is_nobody_to_delegate_to() {
        let mut ctx = AgenticContext::default();
        assert_eq!(harness_identity_line(&ctx), "");

        ctx.agent_id = Some("mac-operator".into());
        let worker = harness_identity_line(&ctx);
        assert!(
            worker.contains("You are the agent `mac-operator`."),
            "{worker}"
        );
        assert!(
            worker.contains("You are the worker for this task"),
            "{worker}"
        );

        ctx.delegation_targets = vec![
            crate::magician_v2::execution::agentic::delegation_dispatch::DelegationTarget {
                agent_id: "android-operator".to_string(),
                name: "Pilot".to_string(),
                aliases: Vec::new(),
                description: "phone".to_string(),
                tools: Vec::new(),
                allowed_invocation_surfaces: Vec::new(),
            },
        ];
        let coordinator = harness_identity_line(&ctx);
        assert!(
            coordinator.contains("You are the agent `mac-operator`."),
            "{coordinator}"
        );
        assert!(
            !coordinator.contains("worker for this task"),
            "{coordinator}"
        );
    }

    #[test]
    fn the_loop_stays_the_default() {
        assert_eq!(resolve_turn_engine(None), TurnEngine::MagicianDecision);
        assert_eq!(
            resolve_turn_engine(Some("magician")),
            TurnEngine::MagicianDecision
        );
    }

    #[test]
    fn an_unknown_engine_name_fails_closed_to_the_loop() {
        assert_eq!(
            resolve_turn_engine(Some("clade_code")),
            TurnEngine::MagicianDecision
        );
    }

    #[test]
    fn claude_code_selects_the_harness() {
        assert_eq!(
            resolve_turn_engine(Some("claude_code")),
            TurnEngine::Harness("claude_code")
        );
    }

    /// The seam selects engines through the factory, never a hard-coded
    /// construction — a sixth harness must cost a file, not a seam change.
    #[test]
    fn the_seam_selects_engines_through_the_factory_not_a_hardcoded_one() {
        let source = include_str!("turn_engine.rs");
        let impl_src = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !impl_src.contains("ClaudeCodeEngine::default()"),
            "the seam must not construct a specific engine"
        );
        assert!(
            impl_src.contains("harness_engine_for("),
            "selection flows through the roster factory"
        );
        // The third assertion this test once carried — "impl must not
        // contain `Harness(\"claude_code\")`" — contradicted the design it
        // guarded: `resolve_turn_engine` IS the roster table, and naming
        // engines there is its job. It had been failing unobserved since
        // the seam landed (these suites were never executed until
        // 2026-08-31). Name parity between this table and the factory is
        // enforced where it belongs, by the drift test in
        // `plane::engines` covering both sides.
    }

    #[test]
    fn the_seam_is_inside_the_iteration_not_at_the_cycle() {
        let orchestrator = include_str!("../../orchestrator/v2_orchestrator.rs");
        assert!(
            !orchestrator.contains("TurnEngine::"),
            "the engine choice must not branch at the cycle level"
        );
    }

    #[test]
    fn snapshot_defaults_match_magician_run_scale() {
        let snapshot = HarnessEngineSnapshot::default();
        assert_eq!(snapshot.turn_max_tool_calls, 4000);
        assert_eq!(
            snapshot.turn_max_seconds,
            crate::config::DEFAULT_AGENTIC_MAX_DURATION_SECS
        );
    }

    #[test]
    fn continuation_keys_agree_between_seam_and_apply() {
        let mut ctx = AgenticContext::default();
        ctx.execution_id = Some("exec-key".to_string());
        assert_eq!(harness_continuation_key(&ctx), "exec-key");
        // A legacy-id execution must key identically in the seam and the
        // apply-phase peek, or the capture is written under one key and read
        // under another.
        ctx.execution_id = None;
        ctx.legacy_execution_id = Some("legacy-key".to_string());
        assert_eq!(harness_continuation_key(&ctx), "legacy-key");
        ctx.legacy_execution_id = None;
        assert_eq!(harness_continuation_key(&ctx), "harness-anonymous");
    }

    fn test_pin(engine: &str, harness_model: &str) -> RunEnginePin {
        RunEnginePin {
            engine: engine.to_string(),
            harness_model: harness_model.to_string(),
            pi_profile: None,
        }
    }

    /// An engine the snapshot does not name, whatever this process's
    /// Settings say, so a named-engine case is never the snapshot's own.
    fn engine_other_than_snapshot() -> &'static str {
        if harness_engine_snapshot().engine == "grok" {
            "claude_code"
        } else {
            "grok"
        }
    }

    #[test]
    fn a_turn_drives_with_the_launch_pin_not_the_snapshot() {
        let pinned = test_pin("codex", "gpt-pinned");
        let mut ctx = AgenticContext::default().with_run_engine_pin(Some(pinned.clone()));
        let snapshot = harness_engine_snapshot();
        assert_eq!(effective_run_pin(&ctx, &snapshot), pinned);
        // A pin that disagrees with the engine (a pre-pin pause restored
        // over it) re-derives for the engine the run names.
        let other = engine_other_than_snapshot();
        ctx.harness_engine = Some(other.to_string());
        let rederived = effective_run_pin(&ctx, &snapshot);
        assert_eq!(rederived.engine, other);
        assert_eq!(rederived.harness_model, "default");
    }

    #[test]
    fn a_native_session_resumes_only_under_the_fingerprint_that_made_it() {
        let made_by = test_pin("codex", "gpt-a");
        let continuation = HarnessTurnContinuation {
            native_session_id: Some("native-codex".to_string()),
            session_fingerprint: Some(made_by.session_fingerprint()),
            ..HarnessTurnContinuation::default()
        };
        assert_eq!(
            resumable_session_id(&continuation, &made_by.session_fingerprint(), true).as_deref(),
            Some("native-codex")
        );
        for switched in [test_pin("claude_code", "gpt-a"), test_pin("codex", "gpt-b")] {
            assert_eq!(
                resumable_session_id(&continuation, &switched.session_fingerprint(), true),
                None,
                "{switched:?} must start cold, not load another pin's session"
            );
        }
        let mut profiled = made_by.clone();
        profiled.pi_profile = Some("fast".to_string());
        assert_eq!(
            resumable_session_id(&continuation, &profiled.session_fingerprint(), true),
            None
        );
        assert_eq!(
            resumable_session_id(&continuation, &made_by.session_fingerprint(), false),
            None,
            "an engine without resume never gets an id"
        );
        let legacy = HarnessTurnContinuation {
            native_session_id: Some("native-legacy".to_string()),
            ..HarnessTurnContinuation::default()
        };
        assert_eq!(
            resumable_session_id(&legacy, &made_by.session_fingerprint(), true),
            None,
            "a session of unknown origin starts cold"
        );
    }

    #[test]
    fn a_launch_pinned_engine_beats_the_process_snapshot() {
        let mut ctx = AgenticContext::default();
        // Snapshot default is `magician`; a launch-pinned harness wins.
        ctx.harness_engine = Some("claude_code".to_string());
        assert_eq!(run_engine_for(&ctx), "claude_code");
        // The forced pin survives the same way, and an empty pin is not one.
        ctx.harness_engine = Some("magician".to_string());
        assert_eq!(run_engine_for(&ctx), "magician");
        ctx.harness_engine = Some("   ".to_string());
        assert_eq!(
            run_engine_for(&ctx),
            harness_engine_snapshot().engine,
            "a blank pin must fall back to the snapshot, not disable the seam"
        );
        ctx.harness_engine = None;
        assert_eq!(
            run_engine_for(&ctx),
            harness_engine_snapshot().engine,
            "no pin means the process snapshot governs, as before Task 6b"
        );
    }

    /// The run's background operations follow the same engine the decide
    /// seam thinks with — unless that engine is the native loop, which is no
    /// parent at all.
    #[test]
    fn a_run_names_its_engine_as_parent_unless_native() {
        let mut ctx = AgenticContext::default();
        ctx.harness_engine = Some("codex".to_string());
        assert_eq!(run_parent_engine(&ctx).as_deref(), Some("codex"));
        ctx.harness_engine = Some("magician".to_string());
        assert_eq!(run_parent_engine(&ctx), None);
        // Unpinned, the snapshot governs; its default is the native loop.
        ctx.harness_engine = None;
        assert_eq!(
            run_parent_engine(&ctx),
            crate::magician_v2::query_analysis::parent_engine::normalize_parent_engine(Some(
                &harness_engine_snapshot().engine
            )),
        );
        // A durable pin makes the same choice the live context does.
        assert_eq!(
            run_parent_engine_for_pin(Some("codex")).as_deref(),
            Some("codex")
        );
        assert_eq!(run_parent_engine_for_pin(Some("magician")), None);
        assert_eq!(run_parent_engine_for_pin(None), run_parent_engine(&ctx));

        // A terminal grant pins no engine and names its parent on the routing
        // overrides instead; the pin still wins when both are present, and
        // the carried parent never changes what the seam thinks with.
        use crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides;
        ctx.llm_routing_overrides =
            Some(OperationRoutingOverrides::default().with_parent_engine(Some("grok".to_string())));
        assert_eq!(run_parent_engine(&ctx).as_deref(), Some("grok"));
        assert_eq!(run_engine_for(&ctx), harness_engine_snapshot().engine);
        ctx.harness_engine = Some("codex".to_string());
        assert_eq!(run_parent_engine(&ctx).as_deref(), Some("codex"));
        ctx.harness_engine = Some("magician".to_string());
        assert_eq!(
            run_parent_engine(&ctx),
            None,
            "a deliberate native pin outranks a carried parent"
        );
    }

    #[test]
    fn a_fully_consumed_wall_bound_cannot_start_a_turn() {
        assert_eq!(remaining_turn_timeout(300, 300_000), None);
        assert_eq!(
            remaining_turn_timeout(300, 600_000),
            None,
            "over-spend stays spent"
        );
    }

    #[test]
    fn a_partial_remainder_rounds_up_to_one_second() {
        // Rounding a sub-second remainder down to zero would read as "no
        // ceiling" inside the engine — the opposite of the intended bound.
        assert_eq!(
            remaining_turn_timeout(300, 299_750),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            remaining_turn_timeout(300, 100_000),
            Some(Duration::from_secs(200))
        );
    }

    #[test]
    fn a_zero_bound_means_no_ceiling_not_an_instant_stop() {
        assert_eq!(remaining_turn_timeout(0, u64::MAX), Some(Duration::ZERO));
    }

    #[test]
    fn a_settled_turn_resets_its_budget_but_keeps_the_conversation() {
        store_turn_continuation(
            "exec-settle",
            HarnessTurnContinuation {
                native_session_id: Some("native-1".to_string()),
                session_fingerprint: None,
                paused_grant_token: Some("plt_paused".to_string()),
                turn_tool_calls_spent: 12,
                turn_wall_clock_spent_ms: 5_000,
                prior_result_note: None,
                pending_approval: None,
            },
        );
        // The non-approval settle path stores exactly this shape.
        store_turn_continuation(
            "exec-settle",
            HarnessTurnContinuation {
                native_session_id: Some("native-1".to_string()),
                ..HarnessTurnContinuation::default()
            },
        );
        let continuation = turn_continuation("exec-settle");
        assert_eq!(continuation.native_session_id.as_deref(), Some("native-1"));
        assert_eq!(continuation.paused_grant_token, None);
        assert_eq!(continuation.turn_tool_calls_spent, 0);
        assert_eq!(continuation.turn_wall_clock_spent_ms, 0);
    }

    #[test]
    fn a_paused_turn_carries_its_unspent_bound_forward() {
        store_turn_continuation(
            "exec-pause",
            HarnessTurnContinuation {
                native_session_id: Some("native-2".to_string()),
                session_fingerprint: None,
                paused_grant_token: Some("plt_kept".to_string()),
                turn_tool_calls_spent: 7,
                turn_wall_clock_spent_ms: 90_000,
                prior_result_note: None,
                pending_approval: None,
            },
        );
        let continuation = turn_continuation("exec-pause");
        assert_eq!(continuation.turn_tool_calls_spent, 7);
        assert_eq!(continuation.turn_wall_clock_spent_ms, 90_000);
        assert_eq!(continuation.paused_grant_token.as_deref(), Some("plt_kept"));
    }

    /// Peek feeds the apply phase and pause publication without consuming;
    /// publication can be retried, so a consuming read would strip the
    /// continuation from every retry after the first.
    #[test]
    fn peek_never_consumes_the_pending_approval() {
        store_turn_continuation(
            "exec-peek",
            HarnessTurnContinuation {
                pending_approval: Some(PlanePendingApproval {
                    capture_id: "pltcap_test".to_string(),
                    action_json: "{\"tool\":\"read_file\"}".to_string(),
                    action_summary: "read_file".to_string(),
                    action_type: "file".to_string(),
                }),
                ..HarnessTurnContinuation::default()
            },
        );
        let first = peek_harness_pending_approval("exec-peek").expect("peek 1");
        let second = peek_harness_pending_approval("exec-peek").expect("peek 2");
        assert_eq!(first, second, "peek must be repeatable");
        let durable = peek_harness_pause_continuation("exec-peek").expect("durable peek 1");
        assert_eq!(
            durable.action_json.as_deref(),
            Some("{\"tool\":\"read_file\"}")
        );
        assert_eq!(durable.action_summary.as_deref(), Some("read_file"));
        // Still there for a retried publication.
        let retried = peek_harness_pause_continuation("exec-peek").expect("durable peek 2");
        assert_eq!(retried.action_json, durable.action_json);
        assert!(
            peek_harness_pending_approval("exec-peek").is_some(),
            "publication peeks must not consume the capture"
        );
    }

    /// The durable form round-trips into the live map with the result note
    /// the resumed turn's input opens with.
    #[tokio::test]
    async fn restore_from_pause_rehydrates_the_live_continuation_with_the_result() {
        let execution_id = format!("exec-restore-{}", uuid::Uuid::new_v4().simple());
        let grant_token = plane_grant_registry()
            .mint(PlaneGrant::for_test(&execution_id))
            .await;
        store_turn_continuation(
            &execution_id,
            HarnessTurnContinuation {
                paused_grant_token: Some(grant_token.clone()),
                ..HarnessTurnContinuation::default()
            },
        );
        assert!(plane_grant_registry()
            .resolve_run_scoped(&grant_token)
            .await
            .is_some());
        let durable = HarnessPauseContinuation {
            action_json: Some("{\"tool\":\"shell\"}".to_string()),
            action_summary: Some("shell".to_string()),
            action_type: Some("shell".to_string()),
            native_session_id: Some("native-3".to_string()),
            session_fingerprint: Some("codex\u{1f}default\u{1f}".to_string()),
            turn_tool_calls_spent: 4,
            turn_wall_clock_spent_ms: 15_000,
        };
        restore_harness_continuation_from_pause(
            &execution_id,
            &durable,
            Some("ok — done".to_string()),
        )
        .await;
        assert!(plane_grant_registry()
            .resolve_run_scoped(&grant_token)
            .await
            .is_none());
        let continuation = turn_continuation(&execution_id);
        assert_eq!(continuation.native_session_id.as_deref(), Some("native-3"));
        assert_eq!(continuation.turn_tool_calls_spent, 4);
        assert_eq!(continuation.turn_wall_clock_spent_ms, 15_000);
        assert_eq!(continuation.prior_result_note.as_deref(), Some("ok — done"));
        assert_eq!(continuation.paused_grant_token, None);
        assert_eq!(continuation.pending_approval, None);
    }

    #[tokio::test]
    async fn paused_grant_waits_for_dispatch_then_revokes_without_resume() {
        let execution_id = format!("exec-paused-{}", uuid::Uuid::new_v4().simple());
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test(&execution_id))
            .await;
        let grant = plane_grant_registry()
            .resolve_run_scoped(&token)
            .await
            .expect("minted grant");
        grant.set_turn_stop(PlaneTurnStopReason::NeedsApproval);
        let dispatch_guard = grant.lock_dispatch().await;
        let cleanup = revoke_paused_grant_after_dispatch(token.clone(), grant);
        tokio::task::yield_now().await;
        assert!(plane_grant_registry()
            .resolve_run_scoped(&token)
            .await
            .is_some());
        drop(dispatch_guard);
        tokio::time::timeout(Duration::from_secs(2), cleanup)
            .await
            .expect("cleanup finished after dispatch")
            .expect("cleanup task");
        assert!(plane_grant_registry()
            .resolve_run_scoped(&token)
            .await
            .is_none());
    }
}
