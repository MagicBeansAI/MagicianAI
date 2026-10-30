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
    /// `execution.harness_engine: chat` — agentic flows think with whatever
    /// the chat engine and model are. Resolved on every read from the live
    /// chat snapshot, so a later chat-engine change needs no re-install.
    pub follows_chat: bool,
}

/// The `execution.harness_engine` value meaning "same as the chat engine".
pub const FOLLOW_CHAT_ENGINE: &str = "chat";

/// True when a configured agentic-flow engine follows the chat engine.
pub fn follows_chat_engine(configured: &str) -> bool {
    configured.trim() == FOLLOW_CHAT_ENGINE
}

/// Overlay the chat engine and model on a snapshot that follows chat. The
/// chat's `magician` mouth maps to the `magician` loop; unknown chat names
/// coerce to `magician` as they do for the mouth.
fn resolve_follow_chat(
    mut snapshot: HarnessEngineSnapshot,
    chat: &super::chat_turn::ChatHarnessSnapshot,
) -> HarnessEngineSnapshot {
    if snapshot.follows_chat {
        snapshot.engine = super::chat_turn::coerce_chat_mouth_engine(&chat.engine).to_string();
        snapshot.harness_model = chat.harness_model.clone();
    }
    snapshot
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
            follows_chat: false,
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
    let snapshot = SNAPSHOT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if !snapshot.follows_chat {
        return snapshot;
    }
    resolve_follow_chat(snapshot, &super::chat_turn::chat_harness_snapshot())
}

/// Whether the installed agentic-flow setting follows the chat engine (the
/// Settings choice, before [`harness_engine_snapshot`] resolves it).
pub fn harness_engine_follows_chat() -> bool {
    SNAPSHOT
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .follows_chat
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
    native_home: Option<std::path::PathBuf>,
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
    if let Some(continuation) = TURN_CONTINUATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(execution_id)
    {
        crate::magician_v2::execution::plane::chat_turn::remove_native_home(
            continuation.native_home,
        );
    }
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
pub use crate::magician_v2::execution::plane::engine::HarnessPauseContinuation;

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
                native_home: continuation.native_home.clone(),
                prior_result_note: continuation.prior_result_note.clone(),
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
                native_home: pause.native_home.clone().filter(|home| {
                    crate::magician_v2::execution::plane::chat_turn::native_home_is_owned(home)
                }),
                session_fingerprint: pause.session_fingerprint.clone(),
                paused_grant_token: None,
                turn_tool_calls_spent: pause.turn_tool_calls_spent,
                turn_wall_clock_spent_ms: pause.turn_wall_clock_spent_ms,
                prior_result_note: match (pause.prior_result_note.clone(), prior_result_note) {
                    (Some(prior), Some(result)) => {
                        Some(format!("{prior}\n\nHost continuation context:\n{result}"))
                    },
                    (prior, result) => result.or(prior),
                },
                pending_approval: None,
            },
        );
    if let Some(old_home) = previous
        .as_ref()
        .and_then(|entry| entry.native_home.clone())
    {
        if Some(&old_home) != pause.native_home.as_ref() {
            crate::magician_v2::execution::plane::chat_turn::remove_native_home(Some(old_home));
        }
    }
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

pub(crate) async fn restore_harness_checkpoint(
    ctx: &AgenticContext,
    checkpoint: &HarnessPauseContinuation,
) {
    restore_harness_continuation_from_pause(&harness_continuation_key(ctx), checkpoint, None).await;
}

/// A settled (or failed) harness turn's service health and, when it failed
/// for its CLI's sign-in, account, limits or reachability, that failure in
/// the CLI's own words (`harness_auth::failure`).
fn classify_harness_turn(
    engine: &str,
    result: &std::result::Result<(HarnessTurnSettled, Box<dyn HarnessSession>), HarnessError>,
) -> (
    Option<std::result::Result<(), crate::magician_v2::realtime_events::ServiceFailure>>,
    Option<crate::magician_v2::harness_auth::failure::HarnessFailure>,
) {
    use crate::magician_v2::harness_auth::failure::HarnessFailure;
    match result {
        Ok((settled, session)) => {
            let failure = (settled.stop_reason == HarnessStopReason::Refused)
                .then(|| {
                    HarnessFailure::classify(
                        engine,
                        &[
                            settled.assistant_text.as_str(),
                            session.failure_text().as_str(),
                        ],
                    )
                })
                .flatten();
            (session.service_health(settled), failure)
        },
        Err(error) => {
            let text = error.to_string();
            (
                crate::magician_v2::realtime_events::ServiceFailure::from_error(&text).map(Err),
                HarnessFailure::classify(engine, &[text.as_str()]),
            )
        },
    }
}

/// Report a run turn's service health; a classified failure's notice
/// carries the CLI's reason and remedy.
fn report_harness_health(
    ctx: &AgenticContext,
    service: &str,
    health: std::result::Result<(), crate::magician_v2::realtime_events::ServiceFailure>,
    failure: Option<&crate::magician_v2::harness_auth::failure::HarnessFailure>,
) {
    let detail = failure.map(
        |failure| crate::magician_v2::realtime_events::ServiceDetail {
            title: crate::magician_v2::harness_auth::failure::engine_label(&failure.engine)
                .to_string(),
            reason: failure.reason.clone(),
            remedy: failure.remedy.clone(),
        },
    );
    crate::magician_v2::decision_host::report_health_with_detail(
        ctx.principal.as_deref().unwrap_or("anonymous"),
        ctx.workspace.as_deref().unwrap_or("default"),
        service,
        health,
        detail.as_ref(),
    );
}

fn checkpoint_decision(execution_id: &str, note: String) -> Decision {
    let continuation = peek_harness_pause_continuation(execution_id).unwrap_or_default();
    if continuation.native_session_id.is_none() {
        return Decision::NeedUserInput {
            question: "The harness reached an internal slice without a resumable native session. Continue after reviewing the completed work?".into(),
            // Continuing restarts paid harness work: keep it out of
            // one-click and typed answers, like a max-iterations prompt.
            input_type: UserInputType::Confirmation {
                confirm_label: Some("Continue".into()),
                deny_label: Some("Stop".into()),
                destructive: true,
            },
            hint: Some("Completed actions and usage remain recorded. Automatic replay is disabled when the harness cannot resume its session.".into()),
            options: None,
        };
    }
    Decision::HarnessCheckpoint {
        note,
        continuation: Box::new(continuation),
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
    maybe_harness_decide_with_engine(
        ctx,
        executors,
        history,
        loop_protective,
        observed_state,
        pending_operator_steer,
        cancellation_token,
        iteration,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn maybe_harness_decide_with_engine(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    observed_state: &EnvironmentState,
    pending_operator_steer: &[String],
    cancellation_token: Option<&CancellationToken>,
    iteration: usize,
    engine_override: Option<Arc<dyn crate::magician_v2::execution::plane::engine::HarnessEngine>>,
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
    // A harness turn is paid for outside the router, so the owner's spend
    // envelope cannot refuse its model calls one by one: it refuses the turn
    // up front instead. The typed refusal parks the run (see
    // `spend_envelope::park_wait_for_error`) rather than failing it. A run
    // launched from a chat is interactive: counted, never refused.
    crate::magician_v2::spend_envelope_producers::preflight(
        crate::magician_v2::spend_envelope_producers::agentic_run_class(ctx),
    )?;
    let execution_id = harness_continuation_key(ctx);
    let continuation = turn_continuation(&execution_id);
    // A grant retained by a paused turn is dead authority now: this decide is
    // a new turn on a fresh grant. It was kept only so an in-flight dispatch
    // could finish without its cancellation token being tripped mid-action.
    if let Some(paused) = &continuation.paused_grant_token {
        plane_grant_registry().revoke(paused).await;
    }

    // Approval resumes keep their slice remainder. Once consumed, rotate the
    // internal allowance without changing the enclosing run budget.
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
                    native_home: continuation.native_home,
                    prior_result_note: continuation.prior_result_note,
                    session_fingerprint: continuation.session_fingerprint,
                    ..HarnessTurnContinuation::default()
                },
            );
            return Ok(Some(checkpoint_decision(
                &execution_id,
                "Internal harness time slice consumed; continuing with the same run allowance."
                    .into(),
            )));
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
    let engine = engine_override
        .or_else(|| crate::magician_v2::execution::plane::engines::harness_engine_for(&run_engine))
        .expect("the roster gate above guarantees the factory can build this engine");
    let mut grant = PlaneGrant::for_run(
        ctx.clone(),
        Arc::new(executors.clone()),
        execution_id.clone(),
    )
    .with_turn_time_budget(turn_timeout)
    .with_turn_tool_budget(
        snapshot.turn_max_tool_calls,
        continuation.turn_tool_calls_spent,
    );
    if let Some(cancel) = cancellation_token {
        // Revoking a turn must cancel its in-flight actions without ever
        // cancelling the parent run. Parent cancellation still reaches both.
        grant.cancellation_token = Some(cancel.child_token());
    }
    // Reuse the bounded Plane result ledger so a parallel client's final
    // completed result cannot disappear between tool completion and child stop.
    grant.turn_ledger = Some(Arc::new(std::sync::Mutex::new(Default::default())));
    preload_run_tools(&mut grant, engine.capabilities().tools_list_changed);
    // Retain the grant's shared stop/capture state even when an adapter has
    // already removed its registry entry during teardown.
    let turn_grant = grant.clone();
    // A plan a turn submits is keyed by its grant session; a stale one from
    // a turn that never settled must not start this turn's plan.
    let plan_session = grant.session_id.clone();
    let _ = crate::magician_v2::execution::plane::submitted_plans::take(&plan_session);
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
        text.push_str("\n\nPrevious approved-action or internal-checkpoint context:\n");
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
    let native_home = if supports_resume {
        match continuation.native_home.clone().filter(|home| {
            continuation.session_fingerprint.as_deref() == Some(session_fingerprint.as_str())
                && crate::magician_v2::execution::plane::chat_turn::native_home_is_owned(home)
        }) {
            Some(home) => Some(home),
            None => {
                crate::magician_v2::execution::plane::chat_turn::remove_native_home(
                    continuation.native_home.clone(),
                );
                Some(
                    crate::magician_v2::execution::plane::chat_turn::mint_native_home()
                        .ok_or_else(|| {
                            anyhow::anyhow!("Cannot create a private harness continuation home")
                        })?,
                )
            },
        }
    } else {
        None
    };
    let mut home_lease =
        crate::magician_v2::execution::plane::chat_turn::RemoveNewNativeHomeOnDrop::new(
            native_home
                .clone()
                .filter(|home| Some(home) != continuation.native_home.as_ref()),
        );
    let resume_session_id =
        resumable_session_id(&continuation, &session_fingerprint, supports_resume)
            .filter(|_| native_home.is_some() && native_home == continuation.native_home);
    // A newly minted home has no transcript for an old native ID. Legacy
    // pauses and missing homes start cold instead of resuming an absent file.
    // Super god mode: the owner approves (once / this session / always)
    // before this harness starts with no restrictions.
    let unrestricted =
        crate::magician_v2::god_mode::approve_unrestricted_launch(ctx, &run_engine, None).await;
    // Started unrestricted, the harness also stops when super god mode ends.
    let (harness_cancel, _super_watch) = if unrestricted {
        let (token, watch) =
            crate::magician_v2::god_mode::unrestricted_cancel(cancellation_token, &run_engine);
        (Some(token), Some(watch))
    } else {
        (cancellation_token.cloned(), None)
    };
    let req = HarnessSessionRequest {
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
        cancel: harness_cancel,
        resume_session_id: resume_session_id.clone(),
        // Time slicing is checked at the next governed dispatch, after the
        // preceding tool result reached its native session. Never interrupt
        // an in-flight effect just to rotate an internal allowance. The
        // enclosing run's deadline and cancellation still stop the child.
        turn_timeout: Duration::ZERO,
        turn_idle_timeout: Some(std::time::Duration::from_secs(
            crate::config::default_harness_turn_idle_seconds(),
        )),
        // Reuse the existing managed native home across Work slices/pauses.
        native_home: native_home.clone(),
        unrestricted,
    };
    // `login_first` with the CLI not signed in (or its session expired):
    // the owner decides before the launch whether Magician's key may stand
    // in. Never automatic (`harness_auth::fallback`).
    let ask_scope = crate::magician_v2::harness_auth::fallback::AskScope {
        principal: ctx
            .principal
            .clone()
            .unwrap_or_else(|| "anonymous".to_string()),
        workspace: ctx
            .workspace
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        execution_id: ctx.execution_id.clone(),
        task_id: ctx.task_id.clone(),
    };
    let key_asked = crate::magician_v2::harness_auth::fallback::preflight(&ask_scope, &pin.engine)
        .await
        .is_some();
    let turn_input = HarnessTurnInput {
        text: crate::magician_v2::secrets::sanitize_text_for_provider(&text),
        // Already included in text above, once for every engine.
        operator_steer: Vec::new(),
    };
    // The grant is good only on this turn's own loopback channel, which its
    // box alone may reach (`plane::grant_channel`).
    let mut req = req;
    let plane_channel = crate::magician_v2::execution::plane::grant_channel::open(
        &token,
        native_home.iter().cloned().collect(),
    )
    .await;
    let plane_session = plane_channel.as_ref().map(|(session, _)| session.clone());
    if let Some(session) = &plane_session {
        req.endpoint.url = crate::magician_v2::execution::plane::grant_channel::endpoint_on(
            &req.endpoint.url,
            session.port,
        );
    }
    // The harness process's box keeps out the goal workspaces this run's
    // active owner may not reach (`goal_workspace_access`). A key-fallback
    // rerun below runs in the same box, on the same channel.
    let acting_agent = crate::magician_v2::execution::goal_workspace_access::acting_agent_for(ctx);
    let read_delegators =
        crate::magician_v2::execution::goal_workspace_access::read_delegators_for(ctx);
    let turn_started = std::time::Instant::now();
    let mut turn_result: std::result::Result<
        (HarnessTurnSettled, Box<dyn HarnessSession>),
        HarnessError,
    > = crate::magician_v2::execution::native_executors::with_read_delegators(
        read_delegators.clone(),
        crate::magician_v2::execution::native_executors::with_acting_agent(
            acting_agent.clone(),
            crate::magician_v2::execution::plane::grant_channel::with_plane_session(
                plane_session.clone(),
                Box::pin(async {
                    let mut session = engine.start(&req).await?;
                    let settled = session
                        .turn(&turn_input, &HarnessStreamSink::drain())
                        .await?;
                    Ok((settled, session))
                }),
            ),
        ),
    )
    .await;
    let mut failure = None;
    if !cancellation_token.is_some_and(CancellationToken::is_cancelled) {
        use crate::magician_v2::harness_auth::fallback;
        let (health, classified) = classify_harness_turn(&pin.engine, &turn_result);
        crate::magician_v2::harness_auth::failure::observe(
            &pin.engine,
            health,
            classified.as_ref(),
        );
        let offer = classified.as_ref().filter(|failure| {
            !key_asked
                && failure.is_account_failure()
                && fallback::offers_key(&pin.engine, failure.class)
        });
        if let Some(health) = health {
            // The decision card already says why; one card, not two.
            if offer.is_none() {
                report_harness_health(
                    ctx,
                    &req.health_service(&pin.engine),
                    health,
                    classified.as_ref(),
                );
            }
        }
        let mut retried = false;
        if let Some(offer) = offer {
            let decision =
                fallback::ask_key_fallback(&ask_scope, &pin.engine, offer.class, &offer.reason)
                    .await;
            // A failed launch has no session left to rerun on; the approval
            // stands for the run's next turn.
            if decision.approved()
                && !cancellation_token.is_some_and(CancellationToken::is_cancelled)
                && turn_result.is_ok()
            {
                turn_result = match turn_result {
                    Ok((_, mut session)) => {
                        let rerun = crate::magician_v2::execution::native_executors::with_read_delegators(
                            read_delegators.clone(),
                            crate::magician_v2::execution::native_executors::with_acting_agent(
                                acting_agent.clone(),
                                crate::magician_v2::execution::plane::grant_channel::with_plane_session(
                                    plane_session.clone(),
                                    Box::pin(session.turn(&turn_input, &HarnessStreamSink::drain())),
                                ),
                            ),
                        )
                        .await;
                        match rerun {
                            Ok(settled) => Ok((settled, session)),
                            Err(error) => Err(error),
                        }
                    },
                    Err(error) => Err(error),
                };
                fallback::finish_once(&pin.engine);
                retried = true;
            }
        }
        if key_asked && !retried {
            // A "once" approval given before the launch covers this turn only.
            fallback::finish_once(&pin.engine);
        }
        failure = if retried {
            let (health, classified) = classify_harness_turn(&pin.engine, &turn_result);
            crate::magician_v2::harness_auth::failure::observe(
                &pin.engine,
                health,
                classified.as_ref(),
            );
            if let Some(health) = health {
                report_harness_health(
                    ctx,
                    &req.health_service(&pin.engine),
                    health,
                    classified.as_ref(),
                );
            }
            classified
        } else {
            classified
        };
    }
    // The loopback channel outlives the key-fallback rerun above.
    drop(plane_channel);
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
                    native_home: native_home.clone(),
                    session_fingerprint: continuation.session_fingerprint,
                    turn_tool_calls_spent: continuation.turn_tool_calls_spent,
                    turn_wall_clock_spent_ms: continuation.turn_wall_clock_spent_ms,
                    prior_result_note: continuation.prior_result_note,
                    ..HarnessTurnContinuation::default()
                },
            );
            return Err(match &failure {
                Some(failure) => anyhow::anyhow!("harness turn failed: {}", failure.user_message()),
                None => anyhow::anyhow!("harness turn failed: {harness_error}"),
            });
        },
    };
    // A refused turn says why, in the CLI's words, with a remedy.
    if let Some(failure) = &failure {
        if settled.stop_reason == HarnessStopReason::Refused {
            settled.assistant_text = failure.annotate(&settled.assistant_text);
        }
    }
    // A parent Stop wins even if the grant had just latched an approval or
    // delegation stop while the child was tearing down. A cancelled run must
    // not publish a new confirmation pause or retain that grant.
    if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
        settled.stop_reason = HarnessStopReason::Cancelled;
        settled.assistant_text.clear();
    }
    if settled.stop_reason != HarnessStopReason::Cancelled {
        if let Some(stop) = turn_grant.turn_stop_reason() {
            settled.stop_reason = match stop {
                PlaneTurnStopReason::NeedsApproval => HarnessStopReason::NeedsApproval,
                PlaneTurnStopReason::Delegate => HarnessStopReason::Delegate,
                PlaneTurnStopReason::TurnBudgetSpent => HarnessStopReason::TurnBudgetSpent,
                PlaneTurnStopReason::RunControl => HarnessStopReason::RunControl,
            };
        }
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
    let captured_control = (settled.stop_reason != HarnessStopReason::Cancelled)
        .then(|| turn_grant.take_run_control())
        .flatten();
    let input_stop = matches!(&captured_control, Some(Decision::NeedUserInput { .. }));
    let delegation_stop = settled.stop_reason == HarnessStopReason::Delegate;
    let paused = matches!(settled.stop_reason, HarnessStopReason::NeedsApproval)
        || delegation_stop
        || input_stop;
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
        let paused_grant = Some(turn_grant.clone());
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
                native_home: native_home.clone(),
                session_fingerprint: native_session_fingerprint,
                paused_grant_token: Some(token.clone()),
                turn_tool_calls_spent: spent_calls,
                turn_wall_clock_spent_ms: continuation
                    .turn_wall_clock_spent_ms
                    .saturating_add(elapsed_ms),
                prior_result_note: Some(harness_checkpoint_note(&settled, &turn_grant)),
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
            captured_control
                .map(run_control_decision)
                .unwrap_or_else(|| decision_from_harness(&settled))
        }
    } else {
        session.shutdown().await;
        plane_grant_registry().revoke(&token).await;
        grant_lease.disarm();
        // The logical turn is over: the next turn warm-resumes the same
        // native conversation but with a fresh budget.
        //
        // A turn that settled on a final answer without `yield` leaves the
        // run with no completion record, which would end it partial however
        // complete the work was. Once per run, a resumable session is asked
        // to submit that record instead (every engine settles here).
        let completion_nudge = completion_record_nudge_due(
            settled.stop_reason,
            !settled.assistant_text.trim().is_empty(),
            captured_control.is_some(),
            native_session_id.is_some(),
        ) && claim_completion_record_nudge(&execution_id);
        store_turn_continuation(
            &execution_id,
            HarnessTurnContinuation {
                native_session_id,
                native_home: native_home.clone(),
                session_fingerprint: native_session_fingerprint,
                prior_result_note: if completion_nudge {
                    Some(COMPLETION_RECORD_NUDGE.to_string())
                } else {
                    (settled.stop_reason == HarnessStopReason::TurnBudgetSpent)
                        .then(|| harness_checkpoint_note(&settled, &turn_grant))
                },
                ..HarnessTurnContinuation::default()
            },
        );
        if settled.stop_reason == HarnessStopReason::TurnBudgetSpent {
            checkpoint_decision(
                &execution_id,
                "Internal harness slice ended; continuing under the existing run budget.".into(),
            )
        } else if completion_nudge {
            tracing::info!(
                execution_id = %execution_id,
                engine = %run_engine_for(ctx),
                "[HARNESS] turn settled on a final answer without yield; asking the harness \
                 to submit its completion record"
            );
            checkpoint_decision(&execution_id, COMPLETION_RECORD_NUDGE.to_string())
        } else {
            captured_control
                .map(run_control_decision)
                .unwrap_or_else(|| decision_from_harness(&settled))
        }
    };
    home_lease.disarm();
    charge_harness_turn_usage(loop_protective, settled.usage.as_ref());
    charge_harness_turn_envelope(
        ctx,
        &run_engine,
        req.pi_profile.as_ref(),
        settled.usage.as_ref(),
    );
    settle_harness_usage(
        paused || settled.stop_reason == HarnessStopReason::Cancelled,
        settled.usage.as_ref(),
    )?;
    // The harness handed over a plan: the loop runs it as a sticky plan
    // (the decide phase resolves its first step now).
    if let Some(plan) = crate::magician_v2::execution::plane::submitted_plans::take(&plan_session) {
        if !paused && settled.stop_reason != HarnessStopReason::Cancelled {
            tracing::info!(
                steps = plan.steps.len(),
                "[DECISION-RAIL] the harness submitted a plan; the loop runs it"
            );
            let mut tools: Vec<String> = plan
                .steps
                .iter()
                .map(|step| step.call.tool.clone())
                .collect();
            tools.sort();
            tools.dedup();
            crate::magician_v2::execution::agentic::load_tools_for_submitted_plan(ctx, &tools);
            loop_protective.decision_rail_plan = Some(plan);
            loop_protective.decision_rail_plan_fresh = true;
        }
    }
    Ok(Some(decision))
}

// Native transcript persistence and HTTP result delivery are separate events.
// Carry recent host receipts too, bounded by the existing Plane ledger budget,
// so automatic continuation can see effects even if a CLI exits before reading
// its final concurrent result. Older receipts remain in the native transcript
// and canonical execution records; omitted output is explicitly identified.
fn harness_checkpoint_note(settled: &HarnessTurnSettled, grant: &PlaneGrant) -> String {
    use crate::magician_v2::execution::plane::grant::PLANE_TURN_LEDGER_MAX_BYTES;
    let calls = grant
        .turn_ledger
        .as_ref()
        .map(|ledger| ledger.lock().unwrap_or_else(|p| p.into_inner()).drain())
        .unwrap_or_default();
    let total = calls.len();
    let mut receipts = Vec::new();
    let mut remaining = PLANE_TURN_LEDGER_MAX_BYTES;
    for call in calls.into_iter().rev() {
        let record = serde_json::json!({
            "invocation_ref": call.call_id, "tool": call.tool_name,
            "arguments": call.arguments, "result": call.content, "is_error": call.is_error,
        })
        .to_string();
        let bytes = record.len().saturating_add(1);
        if bytes > remaining {
            break;
        }
        remaining -= bytes;
        receipts.push(record);
    }
    receipts.reverse();
    format!("Internal harness slice consumed; continue remaining work in the next internal slice under the same enclosing run allowance. Do not repeat completed actions or use terminal yield merely to report slice progress. Previous slice response: {}\nCompleted governed calls (host receipts; {} older receipts omitted from this note):\n{}",
        settled.assistant_text.chars().take(4000).collect::<String>(),
        total.saturating_sub(receipts.len()), receipts.join("\n"))
}

/// Add a settled turn's CLI-reported USD cost to the run's cumulative spend,
/// and its reported tokens to the run's harness token tally.
/// A harness run is budgeted by cost, not tokens (`run_budget`): the cost is
/// the spend the harness cost ceiling compares against at the next iteration
/// top. A CLI that reports no cost charges nothing — the cost is never
/// inferred. The tokens are display only: a harness run installs no token
/// meter, so the budget usage reads them from this tally.
fn charge_harness_turn_usage(protective: &mut LoopProtectiveState, usage: Option<&HarnessUsage>) {
    let Some(usage) = usage else {
        return;
    };
    if let Some(cost) = usage
        .cost_usd
        .filter(|cost| cost.is_finite() && *cost > 0.0)
    {
        protective.cumulative_run_cost_usd += cost;
    }
    protective.cumulative_harness_tokens = protective
        .cumulative_harness_tokens
        .saturating_add(usage.total_tokens());
}

/// Count a settled turn toward the owner's spend envelope: this spend never
/// passes through the router. Unlike the run's own cost tally above, a turn
/// whose CLI reported tokens but no dollar figure is priced here (the
/// envelope must not read a subscription-less CLI as free); one that reported
/// neither charges nothing.
fn charge_harness_turn_envelope(
    ctx: &AgenticContext,
    engine: &str,
    pi_profile: Option<&magicllm::config::LlmConfig>,
    usage: Option<&HarnessUsage>,
) {
    let at_ms = chrono::Utc::now().timestamp_millis();
    let Some((micro_usd, estimated)) = harness_turn_spend(engine, pi_profile, usage, at_ms) else {
        note_unmetered_harness_engine(engine);
        return;
    };
    crate::magician_v2::spend_envelope_producers::charge_micro(
        crate::magician_v2::spend_envelope_producers::agentic_run_class(ctx),
        ctx.principal.as_deref().unwrap_or("anonymous"),
        ctx.workspace.as_deref().unwrap_or("default"),
        micro_usd,
        estimated,
    );
}

/// What a settled turn cost in micro-USD, and whether Magician estimated it:
/// the CLI's own cost when it reported one, else its tokens at the pricing
/// table's rate for the turn's model (a Pi profile names its provider and
/// model; other CLIs are attributed by model name, then engine). `None` when
/// the turn reported neither cost nor tokens. Shared with the harness chat
/// turn (`chat_turn`), which charges it as foreground chat.
pub(crate) fn harness_turn_spend(
    engine: &str,
    pi_profile: Option<&magicllm::config::LlmConfig>,
    usage: Option<&HarnessUsage>,
    at_ms: i64,
) -> Option<(u64, bool)> {
    use crate::magician_v2::spend_envelope_producers::{
        engine_provider, price_tokens, token_usage,
    };
    let usage = usage?;
    if let Some(cost) = usage
        .cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
    {
        return magicllm::pricing::usd_to_microusd_ceil(cost).map(|micro| (micro, false));
    }
    let (provider, model) = match pi_profile {
        Some(profile) => (profile.provider.clone(), Some(profile.model.as_str())),
        None => (
            engine_provider(engine, usage.model.as_deref()),
            usage.model.as_deref(),
        ),
    };
    let tokens = token_usage(
        usage.input_tokens,
        usage.output_tokens,
        usage
            .cache_read_reported
            .then_some(usage.cached_input_tokens),
        usage.cache_creation_tokens,
    );
    price_tokens(&provider, model, &tokens, at_ms)
}

/// A CLI that reports neither cost nor tokens is invisible to the envelope.
/// Said once per engine, at debug: it is a property of the CLI, not the turn.
fn note_unmetered_harness_engine(engine: &str) {
    static NOTED: Lazy<std::sync::Mutex<std::collections::HashSet<String>>> =
        Lazy::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let first = NOTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(engine.to_string());
    if first {
        tracing::debug!(
            engine = %engine,
            "[SPEND-ENVELOPE] harness turn reported no cost and no tokens; not counted"
        );
    }
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
     then call yield with the final answer in summary and explicit completed, open \
     and blocker lists. Use need_user_input when an owner answer is required; \
     it pauses this Work run. Plain final text cannot declare full completion.\n\n\
     Host continuation protocol: the time/call allowance of one native turn \
     is an internal execution slice. When it is consumed, the controller automatically \
     resumes the same task and native session with a fresh slice while the enclosing \
     run still has allowance. Continue the requested work across these slices. Do not \
     reserve your last call for yield or use partial yield merely to report slice \
     progress. Yield ends Work; use it only when the work is done or a genuine blocker \
     prevents further progress. Use need_user_input when an actual owner answer is \
     required. Enclosing budget exhaustion is controlled by the host and requires \
     owner renewal.\n\n{identity_line}";

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
    } else {
        line.push_str(&harness_delegation_roster(&ctx.delegation_targets));
    }
    line
}

/// Most tools listed per target; the rest are a `get_agent_details` away.
const ROSTER_TOOLS_PER_TARGET: usize = 16;

/// The agents a coordinator can hand work to and the tools each owns — what
/// the native decision prompt shows as its specialist targets. Without it a
/// harness lead told to get a `run_coding_task` call made went searching for
/// a tool only its engineers own (the plane keeps it off a lead's catalog)
/// and yielded "not available" instead of delegating.
fn harness_delegation_roster(
    targets: &[crate::magician_v2::execution::agentic::delegation_dispatch::DelegationTarget],
) -> String {
    use crate::magician_v2::agents::InvocationSurface;
    let mut roster = String::from(
        "\n\nOther agents own tools you do not hold. To get one of their tools used, hand \
         them the work with `delegate_to_agent` (or `handover_to_agent` where listed); \
         never search for or call their tools yourself. Your targets:",
    );
    for target in targets {
        let routes = target
            .allowed_invocation_surfaces
            .iter()
            .filter_map(|surface| match surface {
                InvocationSurface::Delegation => Some("delegate_to_agent"),
                InvocationSurface::Handover => Some("handover_to_agent"),
                _ => None,
            })
            .collect::<Vec<_>>();
        roster.push_str(&format!(
            "\n- `{}` ({})",
            target.agent_id,
            target.name.trim()
        ));
        if !routes.is_empty() {
            roster.push_str(&format!(", via {}", routes.join(" or ")));
        }
        if !target.tools.is_empty() {
            let shown = target.tools.iter().take(ROSTER_TOOLS_PER_TARGET);
            roster.push_str(&format!(
                ": {}",
                shown.map(String::as_str).collect::<Vec<_>>().join(", ")
            ));
            if target.tools.len() > ROSTER_TOOLS_PER_TARGET {
                roster.push_str(", …");
            }
        }
    }
    roster
}

/// Foreign turns need the same run identity and continuation evidence as the
/// built-in decision path. In particular a retried terminal must see the
/// evidence gate's rejection, rather than repeat the original goal blindly.
///
/// The preamble is store-managed (`harness_execution_preamble` v1.0.2 in
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
        "1.0.2",
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

fn run_control_decision(mut decision: Decision) -> Decision {
    if let Decision::Yield { payload } = &mut decision {
        if !payload.summary.trim().is_empty()
            && !payload
                .artifacts
                .iter()
                .any(|artifact| artifact.artifact_type.as_deref() == Some("task_deliverable"))
        {
            payload
                .artifacts
                .push(Artifact::task_deliverable(payload.summary.clone()));
        }
    }
    decision
}

/// What a harness that settled without `yield` is told on its one follow-up
/// turn. Delivered as the next turn's checkpoint context.
const COMPLETION_RECORD_NUDGE: &str = "Your previous turn ended with a final answer but did \
     not call `yield`, so Magician has no completion record for this task and would report \
     it as only partly done. If the work is done, call `yield` now with that final answer \
     in summary and explicit completed, open and blocker lists; do not redo finished work. \
     If work remains, continue it and call `yield` when it is done.";

/// Runs already asked once to submit their completion record. In memory: a
/// restart may ask once more, never more than that per process.
static COMPLETION_RECORD_NUDGED: Lazy<RwLock<std::collections::HashSet<String>>> =
    Lazy::new(|| RwLock::new(std::collections::HashSet::new()));

/// Bound on [`COMPLETION_RECORD_NUDGED`]; past it the set starts over.
const MAX_COMPLETION_RECORD_NUDGED_RUNS: usize = 4_096;

/// Whether a settled turn should be asked for its completion record instead
/// of ending the run partial: it settled normally on a final answer, the
/// harness submitted no run control (`yield`, `need_user_input`, …), and its
/// native session can be resumed so the follow-up keeps the transcript.
fn completion_record_nudge_due(
    stop_reason: HarnessStopReason,
    has_final_text: bool,
    has_run_control: bool,
    resumable: bool,
) -> bool {
    stop_reason == HarnessStopReason::Settled && has_final_text && !has_run_control && resumable
}

/// Claim this run's one completion-record nudge: `true` the first time.
fn claim_completion_record_nudge(execution_id: &str) -> bool {
    let mut nudged = COMPLETION_RECORD_NUDGED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if nudged.len() >= MAX_COMPLETION_RECORD_NUDGED_RUNS && !nudged.contains(execution_id) {
        nudged.clear();
    }
    nudged.insert(execution_id.to_string())
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
            // Approving runs a harness action, so it is never a plain
            // question: `destructive` keeps it out of one-click and typed
            // answers. `apply` normally swaps this for a typed action
            // confirmation; this is the fallback when no action was captured.
            input_type: UserInputType::Confirmation {
                confirm_label: Some("Approve".to_string()),
                deny_label: Some("Deny".to_string()),
                destructive: true,
            },
            hint: Some(
                "Magician will execute the approved action on resume; the harness is paused, not cancelled."
                    .to_string(),
            ),
            options: None,
        },
        HarnessStopReason::Settled if !settled.assistant_text.trim().is_empty() => {
            // Plain text conveys no completion disposition. Preserve it as
            // a partial deliverable; only the governed yield can assert full.
            use crate::magician_v2::execution::agentic::yield_decision::YieldDecision;
            Decision::Yield { payload: YieldDecision {
                summary: settled.assistant_text.clone(),
                open: vec!["The harness did not submit a governed completion record.".into()],
                artifacts: vec![Artifact::task_deliverable(settled.assistant_text.clone())],
                ..Default::default()
            }}
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
        HarnessStopReason::TurnBudgetSpent => Decision::HarnessCheckpoint {
            note: "Internal harness slice exhausted; work remains.".into(),
            continuation: Box::default(),
        },
        HarnessStopReason::Settled | HarnessStopReason::Refused
        | HarnessStopReason::Cancelled
        | HarnessStopReason::Delegate | HarnessStopReason::RunControl => Decision::Failed {
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

    #[test]
    fn agentic_flows_that_follow_chat_take_the_chat_engine_and_model() {
        let chat = |engine: &str, model: &str| {
            crate::magician_v2::execution::plane::chat_turn::ChatHarnessSnapshot {
                engine: engine.to_string(),
                harness_model: model.to_string(),
                ..Default::default()
            }
        };
        let following = HarnessEngineSnapshot {
            engine: FOLLOW_CHAT_ENGINE.to_string(),
            follows_chat: true,
            ..Default::default()
        };
        let resolved = resolve_follow_chat(following.clone(), &chat("claude_code", "opus"));
        assert_eq!(resolved.engine, "claude_code");
        assert_eq!(resolved.harness_model, "opus");
        // The chat's own mouth is the loop; unknown names fail closed to it.
        assert_eq!(
            resolve_follow_chat(following.clone(), &chat("magician", "default")).engine,
            "magician"
        );
        assert_eq!(
            resolve_follow_chat(following, &chat("not-an-engine", "x")).engine,
            "magician"
        );
        // A pinned engine is left alone.
        let pinned = HarnessEngineSnapshot {
            engine: "pi".to_string(),
            ..Default::default()
        };
        assert_eq!(
            resolve_follow_chat(pinned, &chat("codex", "gpt")).engine,
            "pi"
        );
        assert!(follows_chat_engine(" chat "));
        assert!(!follows_chat_engine("magician"));
    }

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

    /// A harness run's cost ceiling compares against the CLI-reported spend;
    /// a turn without a reported cost charges nothing.
    #[test]
    fn a_harness_turn_charges_its_reported_cost_to_the_run() {
        let mut protective = LoopProtectiveState::default();
        let usage = |cost: Option<f64>| HarnessUsage {
            input_tokens: 127_000,
            cached_input_tokens: 120_000,
            output_tokens: 900,
            cache_read_reported: true,
            cache_creation_tokens: None,
            cost_usd: cost,
            model: None,
        };
        charge_harness_turn_usage(&mut protective, Some(&usage(Some(1.25))));
        charge_harness_turn_usage(&mut protective, Some(&usage(Some(0.75))));
        charge_harness_turn_usage(&mut protective, Some(&usage(None)));
        charge_harness_turn_usage(&mut protective, Some(&usage(Some(f64::NAN))));
        charge_harness_turn_usage(&mut protective, None);
        assert_eq!(protective.cumulative_run_cost_usd, 2.0);
        // Every reported turn counts its tokens, priced or not; a turn with no
        // usage counts none.
        assert_eq!(protective.cumulative_harness_tokens, 4 * 127_900);
    }

    /// The spend envelope takes the CLI's reported cost as is, prices a turn
    /// that reported only tokens, and counts nothing for a turn that reported
    /// neither.
    #[test]
    fn the_envelope_prices_a_turn_that_reported_tokens_but_no_cost() {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let usage = |cost: Option<f64>, input: u64, model: Option<&str>| HarnessUsage {
            input_tokens: input,
            cached_input_tokens: 0,
            output_tokens: input / 10,
            cache_read_reported: false,
            cache_creation_tokens: None,
            cost_usd: cost,
            model: model.map(str::to_string),
        };
        assert_eq!(
            harness_turn_spend("codex", None, Some(&usage(Some(0.5), 1_000, None)), at_ms),
            Some((500_000, false)),
            "a reported cost is charged exactly"
        );
        let (micro, estimated) = harness_turn_spend(
            "codex",
            None,
            Some(&usage(None, 1_000_000, Some("gpt-5"))),
            at_ms,
        )
        .expect("a token-only turn is priced");
        assert!(micro > 0);
        assert!(!estimated, "a listed model is priced from its own row");
        let (micro, estimated) =
            harness_turn_spend("codex", None, Some(&usage(None, 1_000_000, None)), at_ms)
                .expect("a turn with no model name is still priced");
        assert!(micro > 0);
        assert!(
            estimated,
            "no model to look up: the provider's highest rate"
        );
        assert_eq!(
            harness_turn_spend("codex", None, Some(&usage(None, 0, None)), at_ms),
            None,
            "no cost and no tokens: nothing to count"
        );
        assert_eq!(harness_turn_spend("codex", None, None, at_ms), None);
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

    /// A harness approval runs an action when approved, so its fallback
    /// confirmation is never answerable by one click or a typed "yes".
    #[test]
    fn a_harness_approval_fallback_is_marked_destructive() {
        match decision_from_harness(&settled(HarnessStopReason::NeedsApproval, "")) {
            Decision::NeedUserInput {
                input_type: UserInputType::Confirmation { destructive, .. },
                ..
            } => assert!(destructive),
            other => panic!("expected a confirmation, got {other:?}"),
        }
    }

    /// A provider's final text does not declare whether the goal is complete.
    #[test]
    fn long_run_untyped_harness_text_preserves_a_partial_deliverable() {
        use crate::magician_v2::execution::agentic::yield_decision::{
            dispose_yield, YieldDisposition,
        };
        let decision =
            decision_from_harness(&settled(HarnessStopReason::Settled, "4 lines; cedar"));
        match decision {
            Decision::Yield { payload } => {
                assert!(matches!(
                    dispose_yield(&payload),
                    YieldDisposition::PartialSuccess { .. }
                ));
                assert_eq!(payload.summary, "4 lines; cedar");
                assert_eq!(payload.artifacts[0].data, b"4 lines; cedar");
                assert_eq!(
                    payload.artifacts[0].artifact_type.as_deref(),
                    Some("task_deliverable")
                );
                assert!(!payload.open.is_empty());
            },
            other => panic!("untyped reply cannot declare full completion: {other:?}"),
        }
    }

    #[test]
    fn long_run_typed_harness_yield_keeps_its_disposition_and_exact_answer() {
        use crate::magician_v2::execution::agentic::yield_decision::{
            dispose_yield, YieldDecision, YieldDisposition,
        };
        for open in [vec![], vec!["remaining work".to_string()]] {
            let expected_full = open.is_empty();
            let decision = run_control_decision(Decision::Yield {
                payload: YieldDecision {
                    summary: "The actual result: cedar.".into(),
                    completed: vec!["local artifact".into()],
                    open,
                    ..Default::default()
                },
            });
            let Decision::Yield { payload } = decision else {
                panic!("missing typed yield");
            };
            assert_eq!(
                matches!(dispose_yield(&payload), YieldDisposition::Completed { .. }),
                expected_full
            );
            assert_eq!(payload.artifacts[0].data, b"The actual result: cedar.");
        }
    }

    #[test]
    fn long_run_missing_session_requests_review_and_legacy_pause_shape_is_preserved() {
        let id = uuid::Uuid::new_v4().to_string();
        store_turn_continuation(&id, HarnessTurnContinuation::default());
        assert!(matches!(
            checkpoint_decision(&id, "slice ended".into()),
            Decision::NeedUserInput { .. }
        ));
        let old = serde_json::json!({
            "action_json": null, "action_summary": null, "action_type": null,
            "native_session_id": "legacy", "session_fingerprint": null,
            "turn_tool_calls_spent": 2, "turn_wall_clock_spent_ms": 100,
        });
        let pause: HarnessPauseContinuation = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(pause).unwrap(), old);
        let protective = serde_json::to_value(LoopProtectiveState::default()).unwrap();
        assert!(protective.get("duration_grants_applied_secs").is_none());
        forget_terminal_harness_continuation(&id);
    }

    #[test]
    fn unsuccessful_harness_stops_never_claim_completion() {
        for stop in [HarnessStopReason::Refused, HarnessStopReason::Cancelled] {
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

    /// Live 2026-10-20: a Pi run finished its work, answered in plain text and
    /// never called `yield`, so the run ended "Goal partially achieved". A
    /// settled final answer without run control on a resumable session is
    /// asked once for its completion record; nothing else is, and never twice.
    #[test]
    fn a_settled_answer_without_yield_is_asked_once_for_its_completion_record() {
        let settled = HarnessStopReason::Settled;
        assert!(completion_record_nudge_due(settled, true, false, true));
        // Already yielded / asked the owner, no answer, not resumable, or
        // not a normal settle: no nudge.
        assert!(!completion_record_nudge_due(settled, true, true, true));
        assert!(!completion_record_nudge_due(settled, false, false, true));
        assert!(!completion_record_nudge_due(settled, true, false, false));
        for stop in [
            HarnessStopReason::Refused,
            HarnessStopReason::Cancelled,
            HarnessStopReason::Stalled,
            HarnessStopReason::TurnBudgetSpent,
        ] {
            assert!(
                !completion_record_nudge_due(stop, true, false, true),
                "{stop:?}"
            );
        }
        let run = format!("exec-nudge-{}", uuid::Uuid::new_v4());
        assert!(claim_completion_record_nudge(&run));
        assert!(!claim_completion_record_nudge(&run), "one nudge per run");
        assert!(COMPLETION_RECORD_NUDGE.contains("`yield`"));
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
        assert!(
            coordinator.contains("- `android-operator` (Pilot)"),
            "{coordinator}"
        );
    }

    /// A harness lead learns which target owns the tool its goal names, and
    /// that it reaches it by delegating.
    #[test]
    fn a_coordinator_identity_line_lists_what_its_targets_own() {
        use crate::magician_v2::agents::InvocationSurface;
        let mut ctx = AgenticContext::default();
        ctx.agent_id = Some("cto".into());
        ctx.delegation_targets = vec![
            crate::magician_v2::execution::agentic::delegation_dispatch::DelegationTarget {
                agent_id: "junior-software-engineer".to_string(),
                name: "Junior".to_string(),
                aliases: Vec::new(),
                description: "codes".to_string(),
                tools: vec![
                    "run_coding_task".to_string(),
                    "delegation_files".to_string(),
                ],
                allowed_invocation_surfaces: vec![InvocationSurface::Delegation],
            },
        ];
        let line = harness_identity_line(&ctx);
        assert!(
            line.contains(
                "- `junior-software-engineer` (Junior), via delegate_to_agent: run_coding_task, delegation_files"
            ),
            "{line}"
        );
        assert!(
            line.contains("hand them the work with `delegate_to_agent`"),
            "{line}"
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
        assert_eq!(snapshot.turn_max_tool_calls, 12000);
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
                native_home: None,
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
                native_home: None,
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
                    alignment: None,
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
            ..Default::default()
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
