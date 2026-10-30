//! Per-run budgets: which ceilings a run carries, and the owner's grants.
//!
//! Plan: `docs/archive/plans/2026-10-03-slash-commands-and-run-budgets.md`.
//!
//! A run is budgeted by who thinks it. A **native** run (`magician` engine)
//! keeps its agent's cumulative token meter (`max_tokens_per_cycle`) and the
//! native USD ceiling (`execution.budgets.native_max_cost_usd`, overridden by
//! `MAGICIAN_AGENTIC_MAX_COST_USD`). A **harness** run (any other launchable
//! engine) re-sends its whole context every decision, so a token meter sized
//! for the native loop runs out in a handful of steps; it carries no token
//! meter and the harness USD ceiling (`execution.budgets.harness_max_cost_usd`).
//!
//! Both are now only runaway valves. Money is budgeted across all autonomous
//! work by the owner-set spend envelope (`spend_envelope`, default $250 a
//! UTC day); while that envelope is enforced a native run installs **no**
//! token meter either, because the meter counts cached tokens and so tracks
//! context resend, not money. `AgentConstraints.max_tokens_per_cycle` stays in
//! the schema and `llm_tokens_used` keeps counting for display.
//!
//! The owner widens one run with **grants**, kept in `budget_grants.json`
//! beside the execution. The grant record is deliberately outside the
//! authority-hashed pause record: it is a durable append-only list the running
//! loop polls (one `stat` per check, re-read only when the file changes) and
//! every resume reads. Effective ceilings are always derived as
//! `base + Σ grants` (or none when any grant is `unlimited`), so nothing about
//! a grant is ever folded into the pause record and nothing compounds.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use magician_core::durable_io::write_bytes_durably_sync;
use serde::{Deserialize, Serialize};

use super::types::{AgenticContext, AgenticPauseState};
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};

/// The grant sidecar, in the execution directory beside `engine_pin.json`.
pub const BUDGET_GRANTS_FILE: &str = "budget_grants.json";
/// Content-free usage beside the grant ledger; display data, never authority.
pub const BUDGET_USAGE_FILE: &str = "budget_usage.json";

/// `source` of the grant a budget-pause Continue mints when the owner added
/// nothing: one fresh allowance of the original size.
pub const CONTINUE_GRANT_SOURCE: &str = "continue";
/// `source` of a grant the owner made through the API.
pub const OWNER_GRANT_SOURCE: &str = "owner";

/// One owner grant. Every `add_*` is additive; `unlimited` lifts the cost cap
/// and the token meter for the rest of the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetGrant {
    pub granted_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_iterations: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_duration_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unlimited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BudgetGrantsFile {
    #[serde(default)]
    grants: Vec<BudgetGrant>,
}

/// The sum of a run's grants.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BudgetGrantTotals {
    pub add_cost_usd: f64,
    pub add_tokens: u64,
    pub add_iterations: u64,
    pub add_duration_secs: u64,
    pub unlimited: bool,
}

impl BudgetGrantTotals {
    pub fn of(grants: &[BudgetGrant]) -> Self {
        grants.iter().fold(Self::default(), |mut totals, grant| {
            if let Some(cost) = grant.add_cost_usd.filter(|c| c.is_finite() && *c > 0.0) {
                totals.add_cost_usd += cost;
            }
            totals.add_tokens = totals
                .add_tokens
                .saturating_add(grant.add_tokens.unwrap_or(0));
            totals.add_iterations = totals
                .add_iterations
                .saturating_add(grant.add_iterations.unwrap_or(0));
            totals.add_duration_secs = totals
                .add_duration_secs
                .saturating_add(grant.add_duration_secs.unwrap_or(0));
            totals.unlimited |= grant.unlimited;
            totals
        })
    }
}

/// A grant as the API receives it, before it is stamped and recorded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetGrantRequest {
    #[serde(default)]
    pub add_cost_usd: Option<f64>,
    #[serde(default)]
    pub add_tokens: Option<u64>,
    #[serde(default)]
    pub add_iterations: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_duration_secs: Option<u64>,
    #[serde(default)]
    pub unlimited: Option<bool>,
}

impl BudgetGrantRequest {
    /// Refuse a grant that adds nothing or adds a non-positive / non-finite
    /// amount. `unlimited: false` alone adds nothing.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(cost) = self.add_cost_usd {
            if !(cost.is_finite() && cost > 0.0) {
                return Err(format!(
                    "add_cost_usd must be a positive finite dollar amount, got {cost}"
                ));
            }
        }
        if self.add_tokens == Some(0) {
            return Err("add_tokens must be positive".to_string());
        }
        if self.add_iterations == Some(0) {
            return Err("add_iterations must be positive".to_string());
        }
        if self.add_duration_secs.is_some_and(|secs| {
            secs == 0
                || secs.checked_mul(1000).is_none()
                || std::time::Instant::now()
                    .checked_add(std::time::Duration::from_secs(secs))
                    .is_none()
        }) {
            return Err(
                "add_duration_secs must be positive and fit the active-time clock".to_string(),
            );
        }
        let adds_something = self.add_cost_usd.is_some()
            || self.add_tokens.is_some()
            || self.add_iterations.is_some()
            || self.add_duration_secs.is_some()
            || self.unlimited == Some(true);
        if !adds_something {
            return Err(
                "name at least one of add_cost_usd, add_tokens, add_iterations, add_duration_secs, unlimited: true"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn into_grant(self, granted_at: DateTime<Utc>, source: &str) -> BudgetGrant {
        BudgetGrant {
            granted_at,
            add_cost_usd: self.add_cost_usd,
            add_tokens: self.add_tokens,
            add_iterations: self.add_iterations,
            add_duration_secs: self.add_duration_secs,
            unlimited: self.unlimited.unwrap_or(false),
            source: Some(source.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Sidecar IO
// ---------------------------------------------------------------------------

/// The grant sidecar of one execution.
pub fn budget_grants_path(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_id: &str,
    task_id: Option<&str>,
    execution_id: &str,
) -> PathBuf {
    workspace
        .runtime_execution_dir(principal, workspace_id, task_id, execution_id)
        .join(BUDGET_GRANTS_FILE)
}

/// The grant sidecar of the run `ctx` drives, when it has an execution id.
pub fn budget_grants_path_for_context(ctx: &AgenticContext) -> Option<PathBuf> {
    if ctx.controller_attempt_hash.is_some() {
        // Controller attempts retain the exact budget admitted by their owner.
        // Ordinary task grants cannot widen that frozen reservation.
        return None;
    }
    let execution_id = ctx
        .execution_id
        .as_deref()
        .or(ctx.legacy_execution_id.as_deref())
        .filter(|id| !id.trim().is_empty())?;
    let layout = ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(
        &ctx.storage_base_path,
    ));
    Some(budget_grants_path(
        &layout,
        ctx.principal.as_deref().unwrap_or(DEFAULT_SCOPE_PRINCIPAL),
        ctx.workspace.as_deref().unwrap_or(DEFAULT_SCOPE_WORKSPACE),
        ctx.task_id.as_deref(),
        execution_id,
    ))
}

#[derive(Clone)]
struct CachedGrants {
    modified: Option<SystemTime>,
    len: u64,
    grants: Arc<Vec<BudgetGrant>>,
}

const GRANT_CACHE_CAPACITY: usize = 1024;

fn grant_cache() -> &'static Mutex<HashMap<PathBuf, CachedGrants>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedGrants>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn grant_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn read_grants_uncached(path: &Path) -> Vec<BudgetGrant> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<BudgetGrantsFile>(&bytes) {
            Ok(file) => file.grants,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "[RUN-BUDGET] budget grant sidecar unreadable; the run keeps its base budget"
                );
                Vec::new()
            },
        },
        Err(_) => Vec::new(),
    }
}

/// The run's grants. Cheap enough for every loop check: one `stat`, and a
/// re-read only when the file's mtime or length moved.
pub fn load_budget_grants(path: &Path) -> Arc<Vec<BudgetGrant>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            if let Ok(mut cache) = grant_cache().lock() {
                cache.remove(path);
            }
            return Arc::new(Vec::new());
        },
    };
    let modified = metadata.modified().ok();
    let len = metadata.len();
    if let Ok(cache) = grant_cache().lock() {
        if let Some(cached) = cache.get(path) {
            if cached.modified == modified && cached.len == len {
                return Arc::clone(&cached.grants);
            }
        }
    }
    let grants = Arc::new(read_grants_uncached(path));
    if let Ok(mut cache) = grant_cache().lock() {
        if cache.len() >= GRANT_CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert(
            path.to_path_buf(),
            CachedGrants {
                modified,
                len,
                grants: Arc::clone(&grants),
            },
        );
    }
    grants
}

/// Append one grant through the shared durable writer (unique staging file,
/// file sync, atomic rename and parent-directory sync). Returns every grant
/// the run now holds.
pub fn append_budget_grant(path: &Path, grant: BudgetGrant) -> std::io::Result<Vec<BudgetGrant>> {
    let _guard = grant_write_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut grants = match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice::<BudgetGrantsFile>(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
                .grants
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let total_duration = grants
        .iter()
        .try_fold(grant.add_duration_secs.unwrap_or(0), |sum, old| {
            sum.checked_add(old.add_duration_secs.unwrap_or(0))
        });
    if total_duration.is_none_or(|secs| {
        secs.checked_mul(1000).is_none()
            || std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(secs))
                .is_none()
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "total duration grants exceed the active-time clock",
        ));
    }
    grants.push(grant);
    let body = serde_json::to_vec_pretty(&BudgetGrantsFile {
        grants: grants.clone(),
    })
    .map_err(std::io::Error::other)?;
    write_bytes_durably_sync(path, &body)?;
    if let Ok(mut cache) = grant_cache().lock() {
        cache.remove(path);
    }
    Ok(grants)
}

pub(super) fn grant_totals_for_context(ctx: &AgenticContext) -> BudgetGrantTotals {
    budget_grants_path_for_context(ctx)
        .map(|path| BudgetGrantTotals::of(&load_budget_grants(&path)))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Ceilings
// ---------------------------------------------------------------------------

/// A run is a harness run when its engine (the launch pin, else Settings)
/// resolves to a launchable harness rather than the native loop.
pub fn is_harness_run(ctx: &AgenticContext) -> bool {
    use crate::magician_v2::execution::plane::{resolve_turn_engine, run_engine_for, TurnEngine};
    matches!(
        resolve_turn_engine(Some(&run_engine_for(ctx))),
        TurnEngine::Harness(_)
    )
}

/// Whether an engine name drives a harness run.
pub fn is_harness_engine(engine: &str) -> bool {
    use crate::magician_v2::execution::plane::{resolve_turn_engine, TurnEngine};
    matches!(resolve_turn_engine(Some(engine)), TurnEngine::Harness(_))
}

/// The configured per-run USD ceiling before grants. `None` is unlimited.
pub fn base_cost_cap(harness: bool) -> Option<f64> {
    if harness {
        crate::config::harness_max_cost_usd()
    } else {
        crate::config::agentic_max_cost_usd()
    }
}

/// `base + Σ add_cost_usd`, none when any grant is unlimited, then lowered by
/// a plane-delegated ceiling (a grant never widens a delegated child past the
/// authority it was launched with).
pub fn effective_cost_cap_from(
    base: Option<f64>,
    totals: &BudgetGrantTotals,
    plane_ceiling: Option<f64>,
) -> Option<f64> {
    let widened = if totals.unlimited {
        None
    } else {
        base.map(|base| base + totals.add_cost_usd)
    };
    match (widened, plane_ceiling) {
        (Some(cap), Some(ceiling)) => Some(cap.min(ceiling)),
        (cap, ceiling) => cap.or(ceiling),
    }
}

/// `base + Σ add_tokens` for a native run; none for a harness run, a run with
/// no meter, an unlimited grant, or while the spend envelope is enforced (the
/// meter tracks context resend, not money).
pub fn effective_token_limit_from(
    harness: bool,
    base: Option<u64>,
    totals: &BudgetGrantTotals,
) -> Option<u64> {
    effective_token_limit_with(
        crate::magician_v2::spend_envelope::supersedes_token_meter(),
        harness,
        base,
        totals,
    )
}

/// [`effective_token_limit_from`] with the envelope's state passed in.
pub fn effective_token_limit_with(
    envelope_governs: bool,
    harness: bool,
    base: Option<u64>,
    totals: &BudgetGrantTotals,
) -> Option<u64> {
    if envelope_governs || harness || totals.unlimited {
        return None;
    }
    base.map(|base| base.saturating_add(totals.add_tokens))
}

/// The cost ceiling every check in the loop compares the run's cumulative
/// spend against.
pub fn effective_run_cost_cap(ctx: &AgenticContext) -> Option<f64> {
    effective_cost_cap_from(
        base_cost_cap(is_harness_run(ctx)),
        &grant_totals_for_context(ctx),
        ctx.max_cost_usd,
    )
}

/// The token meter limit the run launches (or resumes) under; `None` installs
/// no meter.
pub fn effective_run_token_limit(ctx: &AgenticContext) -> Option<u64> {
    effective_token_limit_from(
        is_harness_run(ctx),
        ctx.max_tokens_per_cycle,
        &grant_totals_for_context(ctx),
    )
}

fn context_execution_id(ctx: &AgenticContext) -> Option<&str> {
    ctx.execution_id
        .as_deref()
        .or(ctx.legacy_execution_id.as_deref())
}

fn iteration_grants_in_pause(pause: &AgenticPauseState, grants: &[BudgetGrant]) -> u64 {
    pause
        .loop_protective_state
        .as_ref()
        .and_then(|state| state.iteration_grants_applied)
        .unwrap_or_else(|| {
            grants
                .iter()
                .filter(|grant| grant.granted_at <= pause.paused_at)
                .map(|grant| grant.add_iterations.unwrap_or(0))
                .fold(0, u64::saturating_add)
        })
}

pub fn iteration_extension_size(pause: &AgenticPauseState) -> usize {
    pause
        .loop_protective_state
        .as_ref()
        .and_then(|state| state.iteration_budget_base)
        .unwrap_or_else(|| {
            pause
                .continuation_frames
                .last()
                .map(|frame| frame.remaining_iterations)
                .unwrap_or(pause.max_iterations)
                .max(pause.max_iterations)
        })
        .max(1)
}

pub fn iteration_extension_prompt(pause: &AgenticPauseState) -> String {
    // Parked wakes ran no work, so they are not reported as allowance used.
    let used = pause
        .iteration
        .saturating_sub(parked_iteration_refunds_in_pause(pause));
    format!("Iteration allowance reached ({} used). Approve another allowance of up to {} iterations and continue this run from its saved progress?",
        pause.iteration_offset.saturating_add(used), iteration_extension_size(pause))
}

/// Whether an iteration that did no work may be handed back
/// (`LoopProtectiveState::parked_iteration_refunds`): a spend-envelope park,
/// or a decision call that failed with a provider error and is retried. A
/// sealed controller attempt keeps the frozen limit its recipe admitted, and
/// an app contract's limit is not the owner's to widen, so neither is
/// refunded.
pub(super) fn refunds_parked_iterations(ctx: &AgenticContext) -> bool {
    ctx.controller_attempt_hash.is_none() && ctx.app_disclosure_guard.is_none()
}

/// The iteration bound both drivers hold a segment to: its allowance plus the
/// iterations spend-envelope parks and provider-error retries handed back.
/// `ctx.max_iterations` itself is never raised for a refund.
pub(super) fn iteration_ceiling(
    ctx: &AgenticContext,
    protective: &super::types::LoopProtectiveState,
) -> usize {
    ctx.max_iterations
        .saturating_add(protective.parked_iteration_refunds)
}

/// Refunds a pause's segment was granted and had not yet folded into a limit.
pub(super) fn parked_iteration_refunds_in_pause(pause: &AgenticPauseState) -> usize {
    pause
        .loop_protective_state
        .as_ref()
        .map_or(0, |state| state.parked_iteration_refunds)
}

/// Whether `pause` is the owner question raised when the model provider kept
/// failing (`BudgetDimension::ProviderFailure`). Its Cancel ends the run as
/// cancelled; its Retry resumes with the provider retry budgets reset.
pub(super) fn is_provider_failure_pause(pause: &AgenticPauseState) -> bool {
    pause
        .loop_protective_state
        .as_ref()
        .is_some_and(|state| state.provider_failure_pause)
}

/// Whether a run whose provider retries ran out pauses for its owner instead
/// of failing. The same runs that own a renewable wall clock: an ordinary
/// task-backed root run. A delegated child, a sealed controller attempt and an
/// app contract fail as before, so the parent or controller decides.
pub(super) fn pauses_on_provider_failure(ctx: &AgenticContext) -> bool {
    is_owner_renewable_task_run(ctx)
}

/// Called only after the owner confirms an iteration-limit pause. A retry of
/// that answer sees the recorded grant, and cannot mint a second allowance.
pub(super) fn record_iteration_continuation_grant(
    ctx: &AgenticContext,
    pause: &AgenticPauseState,
) -> Result<(), String> {
    if ctx.controller_attempt_hash.is_some()
        || pause.controller_attempt_hash.is_some()
        || pause.iteration < pause.max_iterations
    {
        return Ok(());
    }
    if ctx.app_disclosure_guard.is_some() || pause.app_disclosure_checkpoint.is_some() {
        return Err("This iteration limit belongs to an app contract; an ordinary run grant cannot extend it".to_string());
    }
    let path = budget_grants_path_for_context(ctx)
        .ok_or_else(|| "An iteration extension requires a durable execution owner".to_string())?;
    let grants = load_budget_grants(&path);
    if BudgetGrantTotals::of(&grants).add_iterations > iteration_grants_in_pause(pause, &grants) {
        return Ok(());
    }
    append_budget_grant(
        &path,
        BudgetGrant {
            granted_at: Utc::now(),
            add_cost_usd: None,
            add_tokens: None,
            add_iterations: Some(
                u64::try_from(iteration_extension_size(pause)).unwrap_or(u64::MAX),
            ),
            add_duration_secs: None,
            unlimited: false,
            source: Some(CONTINUE_GRANT_SOURCE.into()),
        },
    )
    .map(|_| ())
    .map_err(|error| format!("Could not record the approved iteration extension: {error}"))
}

/// Whether this is an ordinary task-backed run whose wall clock belongs to its
/// owner. A delegated active-work ceiling, sealed controller attempt or app
/// contract is separate authority and keeps its hard deadline.
pub(super) fn is_owner_renewable_task_run(ctx: &AgenticContext) -> bool {
    // A delegated child inherits its parent's task id and may carry no work
    // budget of its own (its target declared neither a timeout nor a depth), so
    // the budget alone does not mark it. A child sits below depth 0 or under a
    // root execution other than itself.
    let delegated_child = ctx.depth > 0
        || matches!(
            (ctx.root_execution_id.as_deref(), context_execution_id(ctx)),
            (Some(root), Some(own)) if root != own
        );
    !delegated_child
        && ctx.work_budget_secs.is_none()
        && ctx.controller_attempt_hash.is_none()
        && ctx.app_disclosure_guard.is_none()
        && ctx
            .task_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty())
        && context_execution_id(ctx).is_some_and(|id| !id.trim().is_empty())
}

/// An ordinary task's wall-clock allowance is renewable by its owner (see
/// [`is_owner_renewable_task_run`]).
///
/// Use the segment's captured deadline, including a recovered remainder;
/// rereading configuration here would misreport a run admitted under an older
/// limit. The caller has already observed expiry under its control fence.
pub(super) fn owner_duration_budget_at_deadline(
    ctx: &AgenticContext,
    protective: &mut super::types::LoopProtectiveState,
    execution_start: std::time::Instant,
    deadline: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<super::types::BudgetDimension> {
    if !is_owner_renewable_task_run(ctx) {
        return None;
    }
    let limit = deadline?.saturating_duration_since(execution_start);
    protective.remaining_max_duration_ms = Some(0);
    protective.deadline_at = None;
    Some(super::types::BudgetDimension::Time {
        used_seconds: now.saturating_duration_since(execution_start).as_secs(),
        limit_seconds: limit.as_secs(),
    })
}

/// The run's limits at the top of an iteration. Polls the grant sidecar, moves
/// the token meter this run owns to `base + Σ add_tokens` (lifting it on an
/// unlimited grant), and publishes the live snapshot the budget API reads.
/// Returns the token snapshot (`used`, `limit`) and the cost ceiling.
///
/// `parked_iteration_refunds` is the segment's
/// `LoopProtectiveState::parked_iteration_refunds`: the published iteration
/// limit is the ceiling the loop actually holds the run to
/// ([`iteration_ceiling`]), as [`live_budget_from_pause`] reports it.
pub fn refresh_running_budget(
    ctx: &AgenticContext,
    cost_used_usd: f64,
    iteration: usize,
    parked_iteration_refunds: usize,
) -> (Option<(u64, u64)>, Option<f64>) {
    refresh_running_budget_with_harness_tokens(
        ctx,
        cost_used_usd,
        0,
        iteration,
        parked_iteration_refunds,
    )
}

/// [`refresh_running_budget`] that also reports the tokens the run's harness
/// turns returned (`LoopProtectiveState::cumulative_harness_tokens`).
///
/// A harness run installs no token meter (it is budgeted by cost), so its
/// usage otherwise showed `tokens_used: 0` however much the turn read. With no
/// meter the published count is the largest of the run's own counter, this
/// tally and what was already published for the execution, so a refresh that
/// does not carry the tally (the iteration-top checks) cannot erase it. When a
/// meter applies it stays authoritative: harness turns charge it directly.
pub fn refresh_running_budget_with_harness_tokens(
    ctx: &AgenticContext,
    cost_used_usd: f64,
    harness_tokens: u64,
    iteration: usize,
    parked_iteration_refunds: usize,
) -> (Option<(u64, u64)>, Option<f64>) {
    let harness = is_harness_run(ctx);
    let totals = grant_totals_for_context(ctx);
    let token_limit = effective_token_limit_from(harness, ctx.max_tokens_per_cycle, &totals);
    if let Some(owner) = context_execution_id(ctx) {
        super::types::set_owned_execution_token_limit(owner, token_limit);
    }
    let cost_cap = effective_cost_cap_from(base_cost_cap(harness), &totals, ctx.max_cost_usd);
    let tokens = super::types::execution_token_budget_snapshot();
    if let Some(execution_id) = context_execution_id(ctx) {
        let snapshot = LiveRunBudget {
            harness,
            base_tokens: ctx.max_tokens_per_cycle,
            plane_cost_ceiling: ctx.max_cost_usd,
            cost_used_usd,
            tokens_used: match tokens {
                Some((used, _)) => used,
                None => ctx.llm_tokens_used.max(harness_tokens).max(
                    live_run_budget(execution_id)
                        .map(|published| published.tokens_used)
                        .unwrap_or(0),
                ),
            },
            iterations_used: ctx
                .iteration_offset
                .saturating_add(iteration.saturating_sub(1)),
            // Iterations refunded to a park or a provider-error retry count
            // as used (the loop ran them) and raise the limit by the same
            // amount, so used never reads past the limit.
            iterations_limit: ctx
                .iteration_offset
                .saturating_add(ctx.max_iterations)
                .saturating_add(parked_iteration_refunds),
        };
        persist_run_budget_snapshot(ctx, &snapshot);
        publish_live_run_budget(execution_id, snapshot);
    }
    // A meter the run does not own (an inline segment) still reports its own.
    (tokens, cost_cap)
}

/// Apply the run's grants on resume, before the resumed segment's first check.
///
/// - **Tokens.** A native run whose cumulative usage already meets its
///   effective limit (`base + Σ add_tokens`) would re-pause on its first
///   iteration. Continue must grant one fresh allowance of the original size,
///   so a `continue` grant of `add_tokens = base` is recorded in the sidecar —
///   durable, so a retried resume does not mint a second one, and visible to
///   the budget API. The pause record is never touched.
/// - **Cost.** A spent cap gets one fresh base-sized recorded allowance;
///   cumulative spend is retained. A delegated parent ceiling is never widened.
/// - **Duration.** An exhausted active-time remainder gets a recorded fresh
///   allowance. Explicit time grants apply once at the next resume.
/// - **Iterations.** Unused owner grants raise the resumed ceiling once,
///   including grants recorded before the pause.
pub fn apply_budget_grants_on_resume(
    ctx: &mut AgenticContext,
    pause_state: &AgenticPauseState,
    owner_budget_continue: bool,
) {
    if ctx.controller_attempt_hash.is_some() || pause_state.controller_attempt_hash.is_some() {
        // A controller-owned pause continues the original attempt and spend;
        // resuming it does not mint a fresh ordinary-task allowance.
        return;
    }
    let path = budget_grants_path_for_context(ctx);
    let grants = path
        .as_deref()
        .map(load_budget_grants)
        .unwrap_or_else(|| Arc::new(Vec::new()));
    let harness = is_harness_run(ctx);
    let totals = BudgetGrantTotals::of(&grants);

    let applied = ctx
        .resume_loop_protective
        .as_ref()
        .and_then(|state| state.iteration_grants_applied)
        .unwrap_or_else(|| iteration_grants_in_pause(pause_state, &grants));
    let additional = totals.add_iterations.saturating_sub(applied);
    if additional > 0 {
        ctx.max_iterations = ctx
            .max_iterations
            .saturating_add(usize::try_from(additional).unwrap_or(usize::MAX));
    }
    let protective = ctx
        .resume_loop_protective
        .get_or_insert_with(Default::default);
    protective.iteration_grants_applied = Some(totals.add_iterations);
    protective
        .iteration_budget_base
        .get_or_insert(iteration_extension_size(pause_state));

    if let (Some(base), Some(limit)) = (
        ctx.max_tokens_per_cycle,
        effective_token_limit_from(harness, ctx.max_tokens_per_cycle, &totals),
    ) {
        if owner_budget_continue && ctx.llm_tokens_used >= limit {
            let fresh = base.max(1);
            let recorded = path.as_deref().map(|path| {
                append_budget_grant(
                    path,
                    BudgetGrant {
                        granted_at: Utc::now(),
                        add_cost_usd: None,
                        add_tokens: Some(fresh),
                        add_iterations: None,
                        add_duration_secs: None,
                        unlimited: false,
                        source: Some(CONTINUE_GRANT_SOURCE.to_string()),
                    },
                )
            });
            match recorded {
                Some(Ok(_)) => tracing::info!(
                    execution_id = ?context_execution_id(ctx),
                    used = ctx.llm_tokens_used,
                    limit,
                    fresh,
                    "[RUN-BUDGET] token budget exhausted at resume; Continue grants one fresh allowance"
                ),
                other => {
                    // No sidecar to record into: give the resumed segment the
                    // allowance in memory so Continue is never a dead button.
                    if let Some(Err(error)) = other {
                        tracing::warn!(
                            execution_id = ?context_execution_id(ctx),
                            %error,
                            "[RUN-BUDGET] continue grant not recorded; granting in memory"
                        );
                    }
                    ctx.max_tokens_per_cycle = Some(limit.saturating_add(fresh));
                },
            }
        }
    }

    let totals = path
        .as_deref()
        .map(|path| BudgetGrantTotals::of(&load_budget_grants(path)))
        .unwrap_or(totals);
    if let Some(cap) = effective_cost_cap_from(base_cost_cap(harness), &totals, ctx.max_cost_usd) {
        if let Some(protective) = ctx.resume_loop_protective.as_ref() {
            let spent = protective.cumulative_run_cost_usd;
            if owner_budget_continue
                && spent >= cap
                && spent.is_finite()
                && ctx.max_cost_usd.is_none()
            {
                // Preserve actual spend, including a CLI-reported overage. The
                // new allowance is one original-sized round beyond that spend.
                let fresh = base_cost_cap(harness).unwrap_or(cap).max(f64::EPSILON);
                if let Some(path) = path.as_deref() {
                    if let Err(error) = append_budget_grant(
                        path,
                        BudgetGrant {
                            granted_at: Utc::now(),
                            add_cost_usd: Some(spent - cap + fresh),
                            add_tokens: None,
                            add_iterations: None,
                            add_duration_secs: None,
                            unlimited: false,
                            source: Some(CONTINUE_GRANT_SOURCE.into()),
                        },
                    ) {
                        tracing::warn!(%error, "[RUN-BUDGET] cost allowance could not be recorded; prior spend retained");
                    }
                }
            }
        }
    }
    // Duration grants take effect at the next resume. Applied totals live in
    // the existing pause state, so grants made before the pause also count,
    // and reconstructing/retrying a resume cannot compound one grant.
    if let Some(protective) = ctx.resume_loop_protective.as_mut() {
        let Some(remaining) = protective.remaining_max_duration_ms else {
            return;
        };
        let mut totals = path
            .as_deref()
            .map(|p| BudgetGrantTotals::of(&load_budget_grants(p)))
            .unwrap_or(totals);
        if owner_budget_continue
            && remaining == 0
            && totals.add_duration_secs <= protective.duration_grants_applied_secs
        {
            if let Some(path) = path.as_deref() {
                let fresh = crate::config::agentic_max_duration()
                    .unwrap_or_else(|| {
                        std::time::Duration::from_secs(
                            crate::config::DEFAULT_AGENTIC_MAX_DURATION_SECS,
                        )
                    })
                    .as_secs()
                    .max(1);
                if let Err(error) = append_budget_grant(
                    path,
                    BudgetGrant {
                        granted_at: Utc::now(),
                        add_cost_usd: None,
                        add_tokens: None,
                        add_iterations: None,
                        add_duration_secs: Some(fresh),
                        unlimited: false,
                        source: Some(CONTINUE_GRANT_SOURCE.into()),
                    },
                ) {
                    tracing::warn!(%error, "[RUN-BUDGET] time allowance could not be recorded; prior remainder retained");
                }
                totals = BudgetGrantTotals::of(&load_budget_grants(path));
            }
        }
        let extra = totals
            .add_duration_secs
            .saturating_sub(protective.duration_grants_applied_secs)
            .saturating_mul(1000);
        protective.remaining_max_duration_ms = Some(remaining.saturating_add(extra));
        protective.duration_grants_applied_secs = totals.add_duration_secs;
        protective.deadline_at = None;
    }
}

// ---------------------------------------------------------------------------
// Live snapshot + effective budget (API)
// ---------------------------------------------------------------------------

/// What the running loop last published about one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveRunBudget {
    pub harness: bool,
    pub base_tokens: Option<u64>,
    pub plane_cost_ceiling: Option<f64>,
    pub cost_used_usd: f64,
    pub tokens_used: u64,
    pub iterations_used: usize,
    pub iterations_limit: usize,
}

#[derive(Serialize, Deserialize)]
struct BudgetUsageRecord {
    execution_id: String,
    snapshot: LiveRunBudget,
}

/// Recover last committed usage for the owner display after cold restart.
/// This record never grants budget or reconstructs execution authority.
pub fn persisted_run_budget(path: &Path, execution_id: &str) -> Option<LiveRunBudget> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(32 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    let record: BudgetUsageRecord = serde_json::from_slice(&bytes).ok()?;
    (record.execution_id == execution_id
        && record.snapshot.cost_used_usd.is_finite()
        && record.snapshot.cost_used_usd >= 0.0)
        .then_some(record.snapshot)
}

pub(super) fn persist_run_budget_snapshot(ctx: &AgenticContext, snapshot: &LiveRunBudget) {
    if ctx.app_disclosure_guard.is_some() {
        return;
    }
    let Some(path) =
        budget_grants_path_for_context(ctx).map(|path| path.with_file_name(BUDGET_USAGE_FILE))
    else {
        return;
    };
    // Never resurrect a runtime scope that cancellation has reclaimed.
    if !path.parent().is_some_and(Path::is_dir) {
        return;
    }
    let Some(execution_id) = context_execution_id(ctx) else {
        return;
    };
    let _guard = grant_write_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut snapshot = snapshot.clone();
    if let Some(prior) = persisted_run_budget(&path, execution_id) {
        snapshot.cost_used_usd = snapshot.cost_used_usd.max(prior.cost_used_usd);
        snapshot.tokens_used = snapshot.tokens_used.max(prior.tokens_used);
        snapshot.iterations_used = snapshot.iterations_used.max(prior.iterations_used);
        if snapshot == prior {
            return;
        }
    }
    let result = serde_json::to_vec(&BudgetUsageRecord {
        execution_id: execution_id.into(),
        snapshot,
    })
    .map_err(std::io::Error::other)
    .and_then(|bytes| write_bytes_durably_sync(&path, &bytes));
    if let Err(error) = result {
        tracing::warn!(%execution_id, %error, "[RUN-BUDGET] usage display checkpoint could not be committed");
    }
}

/// The pause carries authoritative usage newer than the last iteration's
/// display snapshot. Commit those exact counters for cold reads on every
/// resumable pause path, without granting resources or widening authority.
pub(super) fn publish_paused_run_budget(
    ctx: &AgenticContext,
    outcome: &super::types::AgenticOutcome,
) {
    let Some(pause) = outcome.pause_state() else {
        return;
    };
    let Some(execution_id) = context_execution_id(ctx) else {
        return;
    };
    if ctx.controller_attempt_hash.is_some()
        || pause.controller_attempt_hash.is_some()
        || ctx.app_disclosure_guard.is_some()
    {
        return;
    }
    let snapshot = live_budget_from_pause(pause, is_harness_run(ctx));
    persist_run_budget_snapshot(ctx, &snapshot);
    publish_live_run_budget(execution_id, snapshot);
}

const LIVE_REGISTRY_CAPACITY: usize = 512;

fn live_registry() -> &'static Mutex<HashMap<String, LiveRunBudget>> {
    static LIVE: OnceLock<Mutex<HashMap<String, LiveRunBudget>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn publish_live_run_budget(execution_id: &str, budget: LiveRunBudget) {
    if let Ok(mut live) = live_registry().lock() {
        if live.len() >= LIVE_REGISTRY_CAPACITY && !live.contains_key(execution_id) {
            live.clear();
        }
        live.insert(execution_id.to_string(), budget);
    }
}

pub fn live_run_budget(execution_id: &str) -> Option<LiveRunBudget> {
    live_registry()
        .lock()
        .ok()
        .and_then(|live| live.get(execution_id).cloned())
}

/// A run's budget as the API reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EffectiveRunBudget {
    pub execution_id: String,
    /// A dollar amount, or `"unlimited"`.
    pub cost_cap_usd: serde_json::Value,
    pub cost_used_usd: f64,
    /// `null` when no token meter applies (harness runs, unlimited grants).
    pub tokens_limit: Option<u64>,
    pub tokens_used: u64,
    pub iterations_limit: usize,
    pub iterations_used: usize,
    /// Total recorded additional active-time allowances, including prior rounds.
    pub duration_granted_secs: u64,
    /// Whether this request resumed a run paused on its budget.
    pub resumed: bool,
    /// Additive: whether the run is budgeted as a harness run.
    pub harness: bool,
}

impl EffectiveRunBudget {
    pub fn compute(
        execution_id: &str,
        snapshot: &LiveRunBudget,
        grants: &[BudgetGrant],
        resumed: bool,
    ) -> Self {
        let totals = BudgetGrantTotals::of(grants);
        let cost_cap = effective_cost_cap_from(
            base_cost_cap(snapshot.harness),
            &totals,
            snapshot.plane_cost_ceiling,
        );
        Self {
            execution_id: execution_id.to_string(),
            cost_cap_usd: match cost_cap {
                Some(cap) => serde_json::json!(cap),
                None => serde_json::Value::String("unlimited".to_string()),
            },
            cost_used_usd: snapshot.cost_used_usd,
            tokens_limit: effective_token_limit_from(
                snapshot.harness,
                snapshot.base_tokens,
                &totals,
            ),
            tokens_used: snapshot.tokens_used,
            iterations_limit: snapshot.iterations_limit,
            iterations_used: snapshot.iterations_used,
            duration_granted_secs: totals.add_duration_secs,
            resumed,
            harness: snapshot.harness,
        }
    }
}

/// A paused run's budget, read from its pause record.
pub fn live_budget_from_pause(pause: &AgenticPauseState, harness: bool) -> LiveRunBudget {
    LiveRunBudget {
        harness,
        base_tokens: pause.max_tokens_per_cycle,
        plane_cost_ceiling: pause.max_cost_usd,
        cost_used_usd: pause
            .loop_protective_state
            .as_ref()
            .map(|state| state.cumulative_run_cost_usd)
            .unwrap_or(0.0),
        // A harness run has no meter, so its tokens live in the protective
        // tally; a metered run already counted harness turns in the meter.
        tokens_used: pause.llm_tokens_used.max(
            pause
                .loop_protective_state
                .as_ref()
                .map(|state| state.cumulative_harness_tokens)
                .unwrap_or(0),
        ),
        iterations_used: pause.iteration_offset.saturating_add(pause.iteration),
        // `pause.iteration` counts parked wakes too; their refunds raise the
        // limit by the same amount, so used never reads past the limit.
        iterations_limit: pause
            .iteration_offset
            .saturating_add(pause.max_iterations)
            .saturating_add(parked_iteration_refunds_in_pause(pause)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        configure_execution_budgets, execution_budget_settings, execution_budget_test_lock,
        BudgetCapUsd, ExecutionBudgetSettings,
    };
    use crate::magician_v2::execution::agentic::types::{
        execution_token_budget_snapshot, with_owned_execution_token_meter, LoopProtectiveState,
    };
    use crate::magician_v2::execution::plane::RunEnginePin;

    struct BudgetDefaults {
        previous: ExecutionBudgetSettings,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl BudgetDefaults {
        fn set(native: BudgetCapUsd, harness: BudgetCapUsd) -> Self {
            let guard = execution_budget_test_lock();
            let previous = execution_budget_settings();
            configure_execution_budgets(&ExecutionBudgetSettings {
                native_max_cost_usd: native,
                harness_max_cost_usd: harness,
                ..ExecutionBudgetSettings::default()
            });
            Self {
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for BudgetDefaults {
        fn drop(&mut self) {
            configure_execution_budgets(&self.previous);
        }
    }

    fn pinned_ctx(storage: &Path, engine: &str, execution_id: &str) -> AgenticContext {
        let mut ctx = AgenticContext::new("goal", "done")
            .with_max_tokens_per_cycle(1_000_000)
            .with_storage_base_path(storage.to_path_buf())
            .with_run_engine_pin(Some(RunEnginePin {
                engine: engine.to_string(),
                harness_model: "default".to_string(),
                pi_profile: None,
            }));
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("default".to_string());
        ctx.task_id = Some("task_budget".to_string());
        ctx.execution_id = Some(execution_id.to_string());
        ctx
    }

    #[test]
    fn the_token_meter_stands_down_while_the_spend_envelope_governs() {
        let none = BudgetGrantTotals::default();
        let grant = BudgetGrantTotals {
            add_tokens: 500,
            ..BudgetGrantTotals::default()
        };
        // Envelope not enforced: the meter is exactly what it was.
        assert_eq!(
            effective_token_limit_with(false, false, Some(1_000), &none),
            Some(1_000)
        );
        assert_eq!(
            effective_token_limit_with(false, false, Some(1_000), &grant),
            Some(1_500)
        );
        // Envelope enforced: no meter, whatever the agent declared or was
        // granted. (Cached tokens make the meter a context-resend counter.)
        assert_eq!(
            effective_token_limit_with(true, false, Some(1_000), &none),
            None
        );
        assert_eq!(
            effective_token_limit_with(true, false, Some(1_000), &grant),
            None
        );
        // The unchanged exits stay unchanged.
        assert_eq!(
            effective_token_limit_with(false, true, Some(1_000), &none),
            None
        );
        assert_eq!(effective_token_limit_with(false, false, None, &none), None);
        // And the public form follows the process envelope, which no unit test
        // installs: the meter keeps its old behaviour there.
        assert_eq!(
            effective_token_limit_from(false, Some(1_000), &none),
            Some(1_000)
        );
    }

    #[test]
    fn long_run_owner_deadline_checkpoints_exhaustion_and_cold_grants_apply_once() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "duration_boundary");
        let start = Instant::now();
        let deadline = start + Duration::from_secs(86400);
        let mut protective = LoopProtectiveState {
            remaining_max_duration_ms: Some(5000),
            deadline_at: Some(Utc::now()),
            cumulative_run_cost_usd: 7.125,
            ..Default::default()
        };
        let dimension = owner_duration_budget_at_deadline(
            &ctx,
            &mut protective,
            start,
            Some(deadline),
            deadline + Duration::from_secs(1),
        )
        .unwrap();
        assert!(matches!(
            dimension,
            super::super::types::BudgetDimension::Time {
                used_seconds: 86401,
                limit_seconds: 86400,
            }
        ));
        assert_eq!(protective.remaining_max_duration_ms, Some(0));
        assert!(protective.deadline_at.is_none());
        assert_eq!(protective.cumulative_run_cost_usd, 7.125);
        // The same production checkpoint is serialized before simulated cold
        // recovery. The grant is recorded while no resident loop is present.
        let bytes = serde_json::to_vec(&protective).unwrap();
        ctx.resume_loop_protective = Some(serde_json::from_slice(&bytes).unwrap());
        let path = budget_grants_path_for_context(&ctx).unwrap();
        append_budget_grant(
            &path,
            owner_grant(BudgetGrantRequest {
                add_duration_secs: Some(600),
                ..Default::default()
            }),
        )
        .unwrap();
        let pause = token_pause(&ctx, 0);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        let recovered = ctx.resume_loop_protective.as_ref().unwrap();
        assert_eq!(recovered.remaining_max_duration_ms, Some(600_000));
        assert_eq!(recovered.duration_grants_applied_secs, 600);
        assert_eq!(recovered.cumulative_run_cost_usd, 7.125);
        assert_eq!(load_budget_grants(&path).len(), 1);
    }

    #[test]
    fn long_run_owner_deadline_never_widens_sealed_or_delegated_authority() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let ctx = pinned_ctx(dir.path(), "codex", "hard_boundary");
        let start = Instant::now();
        let deadline = start + Duration::from_secs(120);
        let mut delegated = ctx.clone();
        delegated.work_budget_secs = Some(120);
        let mut sealed = ctx.clone();
        sealed.controller_attempt_hash = Some("sealed-contract".into());
        let mut orphan = ctx.clone();
        orphan.task_id = None;
        let mut no_identity = ctx.clone();
        no_identity.execution_id = None;
        let mut deeper = ctx.clone();
        deeper.depth = 1;
        let mut under_a_root = ctx.clone();
        under_a_root.root_execution_id = Some("exec-of-the-parent-root".into());
        let mut pipeline = ctx.clone();
        pipeline.pipeline_loop_state_segment =
            Some(super::super::types::PipelineLoopStateSegment {
                stage_index: 0,
                attempt: 1,
            });
        for bounded in [delegated, deeper, under_a_root, sealed, orphan, no_identity] {
            let mut protective = LoopProtectiveState::default();
            assert!(owner_duration_budget_at_deadline(
                &bounded,
                &mut protective,
                start,
                Some(deadline),
                deadline,
            )
            .is_none());
            assert!(protective.remaining_max_duration_ms.is_none());
        }
        let mut protective = LoopProtectiveState::default();
        assert!(
            owner_duration_budget_at_deadline(&ctx, &mut protective, start, None, deadline,)
                .is_none()
        );
        assert!(protective.remaining_max_duration_ms.is_none());
        // A pipeline cursor namespaces a stage; it is not itself a frozen
        // resource contract. Ordinary owner time can still be renewed there.
        assert!(owner_duration_budget_at_deadline(
            &pipeline,
            &mut protective,
            start,
            Some(deadline),
            deadline,
        )
        .is_some());
    }

    /// Only an ordinary task-backed root run is left without the default wall
    /// clock; a delegated child, a sealed attempt and an app run keep theirs.
    #[test]
    fn only_an_ordinary_task_backed_root_run_loses_the_default_wall_clock() {
        let dir = tempfile::tempdir().unwrap();
        let mut root = pinned_ctx(dir.path(), "codex", "no_wall_clock");
        assert!(is_owner_renewable_task_run(&root));
        // A rerun's root is itself, not a parent.
        root.root_execution_id = context_execution_id(&root).map(str::to_owned);
        assert!(is_owner_renewable_task_run(&root));

        let mut delegated = root.clone();
        delegated.work_budget_secs = Some(120);
        let mut deeper = root.clone();
        deeper.depth = 1;
        let mut under_a_root = root.clone();
        under_a_root.root_execution_id = Some("exec-of-the-parent-root".into());
        let mut sealed = root.clone();
        sealed.controller_attempt_hash = Some("sealed-contract".into());
        let mut orphan = root.clone();
        orphan.task_id = None;
        let mut no_identity = root.clone();
        no_identity.execution_id = None;
        for bounded in [delegated, deeper, under_a_root, sealed, orphan, no_identity] {
            assert!(!is_owner_renewable_task_run(&bounded));
        }
    }

    #[test]
    fn long_run_usage_display_survives_cold_restart_without_resetting_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "cold_usage");
        ctx.iteration_offset = 8;
        ctx.llm_tokens_used = 42;
        let grants_path = budget_grants_path_for_context(&ctx).unwrap();
        std::fs::create_dir_all(grants_path.parent().unwrap()).unwrap();
        refresh_running_budget(&ctx, 7.125, 3, 0);
        let before = live_run_budget("cold_usage").unwrap();
        assert_eq!(before.iterations_used, 10);
        append_budget_grant(
            &grants_path,
            owner_grant(BudgetGrantRequest {
                add_duration_secs: Some(600),
                ..Default::default()
            }),
        )
        .unwrap();
        live_registry().lock().unwrap().remove("cold_usage");
        let usage_path = grants_path.with_file_name(BUDGET_USAGE_FILE);
        let cold = persisted_run_budget(&usage_path, "cold_usage").unwrap();
        assert_eq!(cold, before);
        let display = EffectiveRunBudget::compute(
            "cold_usage",
            &cold,
            &load_budget_grants(&grants_path),
            false,
        );
        assert_eq!(display.cost_used_usd, 7.125);
        assert_eq!(display.tokens_used, 42);
        assert_eq!(display.duration_granted_secs, 600);
        assert_eq!(display.iterations_used, 10);
        // A stale lower usage publication cannot erase a previous round.
        let mut stale = cold.clone();
        stale.cost_used_usd = 0.0;
        stale.tokens_used = 0;
        stale.iterations_used = 0;
        persist_run_budget_snapshot(&ctx, &stale);
        assert_eq!(persisted_run_budget(&usage_path, "cold_usage"), Some(cold));
        assert!(persisted_run_budget(&usage_path, "different_execution").is_none());
    }

    /// Iterations handed back to a park or a provider-error retry raise the
    /// published limit to the bound the loop actually holds the run to
    /// (`iteration_ceiling`), as the paused display does
    /// (`live_budget_from_pause`), so used never reads past the limit.
    #[test]
    fn the_live_iteration_limit_includes_refunded_iterations() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "refunded_limit");
        ctx.max_iterations = 20;
        let protective = LoopProtectiveState {
            parked_iteration_refunds: 6,
            ..Default::default()
        };
        refresh_running_budget(&ctx, 0.0, 25, protective.parked_iteration_refunds);
        let live = live_run_budget("refunded_limit").unwrap();
        assert_eq!(live.iterations_used, 24);
        assert_eq!(live.iterations_limit, 26);
        assert_eq!(
            live.iterations_limit,
            ctx.iteration_offset + iteration_ceiling(&ctx, &protective)
        );
    }

    /// The provider-failure owner question is raised only by the runs that own
    /// a renewable wall clock; the rest fail so their parent or controller
    /// decides. The pause is recognised by its persisted marker.
    #[test]
    fn only_an_owner_backed_root_run_pauses_on_provider_failure() {
        let dir = tempfile::tempdir().unwrap();
        let root = pinned_ctx(dir.path(), "codex", "provider_failure_root");
        assert!(pauses_on_provider_failure(&root));
        let mut deeper = root.clone();
        deeper.depth = 1;
        let mut under_a_root = root.clone();
        under_a_root.root_execution_id = Some("exec-of-the-parent-root".into());
        let mut sealed = root.clone();
        sealed.controller_attempt_hash = Some("sealed-contract".into());
        let mut orphan = root.clone();
        orphan.task_id = None;
        for bounded in [deeper, under_a_root, sealed, orphan] {
            assert!(!pauses_on_provider_failure(&bounded));
        }

        let mut pause = token_pause(&root, 0);
        assert!(!is_provider_failure_pause(&pause));
        pause.loop_protective_state = Some(LoopProtectiveState {
            provider_failure_pause: true,
            ..Default::default()
        });
        assert!(is_provider_failure_pause(&pause));
    }

    /// A harness run has no token meter: the harness turn's reported tokens
    /// and cost reach the run's usage, and later checks that carry no tally
    /// (the iteration-top refreshes) never erase them.
    #[test]
    fn harness_run_usage_records_the_turns_reported_tokens_and_cost() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = pinned_ctx(dir.path(), "pi", "harness_usage");
        let grants_path = budget_grants_path_for_context(&ctx).unwrap();
        std::fs::create_dir_all(grants_path.parent().unwrap()).unwrap();
        assert!(execution_token_budget_snapshot().is_none());

        refresh_running_budget_with_harness_tokens(&ctx, 0.25, 16_143, 1, 0);
        let live = live_run_budget("harness_usage").unwrap();
        assert!(live.harness);
        assert_eq!(live.tokens_used, 16_143);
        assert_eq!(live.cost_used_usd, 0.25);

        refresh_running_budget(&ctx, 0.25, 2, 0);
        assert_eq!(
            live_run_budget("harness_usage").unwrap().tokens_used,
            16_143
        );
        let usage_path = grants_path.with_file_name(BUDGET_USAGE_FILE);
        let cold = persisted_run_budget(&usage_path, "harness_usage").unwrap();
        assert_eq!(cold.tokens_used, 16_143);
        assert_eq!(cold.cost_used_usd, 0.25);

        let mut pause = token_pause(&ctx, 0);
        pause.loop_protective_state = Some(LoopProtectiveState {
            cumulative_run_cost_usd: 0.25,
            cumulative_harness_tokens: 16_143,
            ..Default::default()
        });
        let paused = live_budget_from_pause(&pause, true);
        assert_eq!(paused.tokens_used, 16_143);
        assert_eq!(paused.cost_used_usd, 0.25);
        live_registry().lock().unwrap().remove("harness_usage");
    }

    #[test]
    fn long_run_usage_checkpoint_never_recreates_a_reclaimed_execution_scope() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = pinned_ctx(dir.path(), "codex", "missing_scope");
        let path = budget_grants_path_for_context(&ctx)
            .unwrap()
            .with_file_name(BUDGET_USAGE_FILE);
        refresh_running_budget(&ctx, 1.0, 1, 0);
        assert!(!path.parent().unwrap().exists());
        assert!(persisted_run_budget(&path, "missing_scope").is_none());
        live_registry().lock().unwrap().remove("missing_scope");
    }

    #[test]
    fn long_run_budget_pause_commits_the_last_charge_before_cold_display_reads() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = pinned_ctx(dir.path(), "codex", "paused_last_charge");
        let path = budget_grants_path_for_context(&ctx).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        refresh_running_budget(&ctx, 7.0, 4, 0);
        let mut pause = token_pause(&ctx, 0);
        pause.iteration = 4;
        pause.loop_protective_state = Some(LoopProtectiveState {
            cumulative_run_cost_usd: 7.125,
            remaining_max_duration_ms: Some(0),
            ..Default::default()
        });
        let outcome = super::super::types::AgenticOutcome::BudgetExhausted {
            dimension: super::super::types::BudgetDimension::Time {
                used_seconds: 120,
                limit_seconds: 120,
            },
            last_state: pause.environment_state.clone(),
            iterations_completed: 3,
            actions_completed: 3,
            pause_state: Some(pause),
        };
        assert!(outcome.pause_state().is_some());
        assert!(outcome
            .budget_dimension()
            .unwrap()
            .continue_prompt()
            .starts_with("Time budget (120s/120s)"));
        use crate::magician_v2::execution::agentic::run_loop::driver_worker::TerminalSteerPolicy;
        assert_eq!(
            TerminalSteerPolicy::for_runtime_closure(&outcome),
            TerminalSteerPolicy::BudgetPause
        );
        let unresumable = super::super::types::AgenticOutcome::BudgetExhausted {
            dimension: super::super::types::BudgetDimension::Time {
                used_seconds: 120,
                limit_seconds: 120,
            },
            last_state: super::super::types::EnvironmentState::Uninitialized,
            iterations_completed: 3,
            actions_completed: 3,
            pause_state: None,
        };
        assert_eq!(
            TerminalSteerPolicy::for_runtime_closure(&unresumable),
            TerminalSteerPolicy::RuntimeClosure
        );
        publish_paused_run_budget(&ctx, &outcome);
        live_registry().lock().unwrap().remove("paused_last_charge");
        let cold = persisted_run_budget(
            &path.with_file_name(BUDGET_USAGE_FILE),
            "paused_last_charge",
        )
        .unwrap();
        assert_eq!(cold.cost_used_usd, 7.125);
        assert_eq!(cold.iterations_used, 4);
    }

    #[test]
    fn long_run_answering_an_unrelated_ask_never_grants_exhausted_budgets() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        if !env_cost_unset() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "magician", "ordinary_ask");
        ctx.resume_loop_protective = Some(LoopProtectiveState {
            remaining_max_duration_ms: Some(0),
            cumulative_run_cost_usd: 15.5,
            ..Default::default()
        });
        ctx.llm_tokens_used = 1_000_001;
        let pause = token_pause(&ctx, 1_000_001);
        apply_budget_grants_on_resume(&mut ctx, &pause, false);
        let path = budget_grants_path_for_context(&ctx).unwrap();
        assert!(load_budget_grants(&path).is_empty());
        let state = ctx.resume_loop_protective.as_ref().unwrap();
        assert_eq!(state.cumulative_run_cost_usd, 15.5);
        assert_eq!(state.remaining_max_duration_ms, Some(0));
        // An explicit owner grant is distinct authority; it still applies
        // once, even when the outstanding ask is not a budget question.
        append_budget_grant(
            &path,
            owner_grant(BudgetGrantRequest {
                add_duration_secs: Some(1),
                ..Default::default()
            }),
        )
        .unwrap();
        apply_budget_grants_on_resume(&mut ctx, &pause, false);
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .remaining_max_duration_ms,
            Some(1000)
        );
        assert_eq!(load_budget_grants(&path).len(), 1);
    }

    #[test]
    fn long_run_time_rounds_are_recorded_and_resume_retries_do_not_compound_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "time_rounds");
        ctx.resume_loop_protective = Some(LoopProtectiveState {
            remaining_max_duration_ms: Some(0),
            cumulative_run_cost_usd: 7.0,
            ..Default::default()
        });
        let pause = token_pause(&ctx, 0);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        let path = budget_grants_path_for_context(&ctx).unwrap();
        let first = load_budget_grants(&path);
        assert_eq!(first.len(), 1);
        let fresh = first[0].add_duration_secs.unwrap();
        assert!(fresh > 0);
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .remaining_max_duration_ms,
            Some(fresh * 1000)
        );
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .cumulative_run_cost_usd,
            7.0
        );
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(load_budget_grants(&path).len(), 1);
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .remaining_max_duration_ms,
            Some(fresh * 1000)
        );
        // A new exhausted round records one new allowance; previous usage and
        // previous grants are retained instead of resetting the whole budget.
        ctx.resume_loop_protective
            .as_mut()
            .unwrap()
            .remaining_max_duration_ms = Some(0);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(load_budget_grants(&path).len(), 2);
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .duration_grants_applied_secs,
            fresh * 2
        );
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .remaining_max_duration_ms,
            Some(fresh * 1000)
        );
    }

    #[test]
    fn long_run_time_grants_given_before_pause_apply_once_without_widening_cost() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "time_pregrant");
        ctx.resume_loop_protective = Some(LoopProtectiveState {
            remaining_max_duration_ms: Some(3000),
            cumulative_run_cost_usd: 4.0,
            ..Default::default()
        });
        let path = budget_grants_path_for_context(&ctx).unwrap();
        append_budget_grant(
            &path,
            owner_grant(BudgetGrantRequest {
                add_duration_secs: Some(86400),
                ..Default::default()
            }),
        )
        .unwrap();
        let pause = token_pause(&ctx, 0);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .remaining_max_duration_ms,
            Some(86403000)
        );
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .cumulative_run_cost_usd,
            4.0
        );
        assert_eq!(load_budget_grants(&path).len(), 1);
        assert_eq!(load_budget_grants(&path)[0].add_cost_usd, None);
        assert!(BudgetGrantRequest {
            add_duration_secs: Some(0),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(BudgetGrantRequest {
            add_duration_secs: Some(u64::MAX),
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    fn owner_grant(request: BudgetGrantRequest) -> BudgetGrant {
        request.into_grant(Utc::now(), OWNER_GRANT_SOURCE)
    }

    fn env_cost_unset() -> bool {
        std::env::var_os("MAGICIAN_AGENTIC_MAX_COST_USD").is_none()
    }

    #[test]
    fn appending_grants_preserves_history_and_refreshes_cached_readers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("execution").join(BUDGET_GRANTS_FILE);
        let first = owner_grant(BudgetGrantRequest {
            add_cost_usd: Some(25.0),
            ..Default::default()
        });
        let second = owner_grant(BudgetGrantRequest {
            add_tokens: Some(1_000_000),
            ..Default::default()
        });

        assert_eq!(
            append_budget_grant(&path, first.clone()).unwrap(),
            vec![first.clone()]
        );
        assert_eq!(load_budget_grants(&path).as_ref(), &vec![first.clone()]);

        let expected = vec![first, second.clone()];
        assert_eq!(append_budget_grant(&path, second).unwrap(), expected);
        assert_eq!(load_budget_grants(&path).as_ref(), &expected);
        let persisted: BudgetGrantsFile =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.grants, expected);
    }

    #[test]
    fn a_harness_run_has_no_token_meter_and_the_100_dollar_cap() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let dir = tempfile::tempdir().unwrap();
        let harness = pinned_ctx(dir.path(), "claude_code", "exec_harness");
        assert!(is_harness_run(&harness));
        assert_eq!(effective_run_token_limit(&harness), None);
        assert_eq!(effective_run_cost_cap(&harness), Some(100.0));

        let native = pinned_ctx(dir.path(), "magician", "exec_native");
        assert!(!is_harness_run(&native));
        assert_eq!(effective_run_token_limit(&native), Some(1_000_000));
        if env_cost_unset() {
            assert_eq!(effective_run_cost_cap(&native), Some(15.0));
        }
    }

    #[test]
    fn unlimited_removes_the_cap_by_default_and_by_grant() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Unlimited);
        let dir = tempfile::tempdir().unwrap();
        let harness = pinned_ctx(dir.path(), "codex", "exec_unlimited_default");
        assert_eq!(effective_run_cost_cap(&harness), None);

        let native = pinned_ctx(dir.path(), "magician", "exec_unlimited_grant");
        let path = budget_grants_path_for_context(&native).unwrap();
        append_budget_grant(
            &path,
            owner_grant(BudgetGrantRequest {
                unlimited: Some(true),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(effective_run_cost_cap(&native), None);
        assert_eq!(effective_run_token_limit(&native), None);
    }

    #[test]
    fn a_plane_ceiling_still_narrows_a_granted_run() {
        let totals = BudgetGrantTotals {
            add_cost_usd: 50.0,
            ..Default::default()
        };
        assert_eq!(
            effective_cost_cap_from(Some(15.0), &totals, Some(20.0)),
            Some(20.0)
        );
        assert_eq!(
            effective_cost_cap_from(Some(15.0), &totals, None),
            Some(65.0)
        );
        let unlimited = BudgetGrantTotals {
            unlimited: true,
            ..Default::default()
        };
        assert_eq!(
            effective_cost_cap_from(Some(15.0), &unlimited, Some(2.0)),
            Some(2.0)
        );
        assert_eq!(effective_cost_cap_from(None, &totals, None), None);
    }

    #[tokio::test]
    async fn a_grant_raises_a_running_runs_ceilings_at_its_next_iteration() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let dir = tempfile::tempdir().unwrap();
        let ctx = pinned_ctx(dir.path(), "magician", "exec_running");
        let path = budget_grants_path_for_context(&ctx).unwrap();
        with_owned_execution_token_meter(
            Some("exec_running".to_string()),
            1_000_000,
            1_000_000,
            async {
                // Iteration N: exhausted.
                let (tokens, _) = refresh_running_budget(&ctx, 1.0, 5, 0);
                assert_eq!(tokens, Some((1_000_000, 1_000_000)));

                // The owner grants while the run is going.
                append_budget_grant(
                    &path,
                    owner_grant(BudgetGrantRequest {
                        add_tokens: Some(1_000_000),
                        add_cost_usd: Some(25.0),
                        ..Default::default()
                    }),
                )
                .unwrap();

                // Iteration N+1 sees it.
                let (tokens, cost) = refresh_running_budget(&ctx, 1.0, 6, 0);
                assert_eq!(tokens, Some((1_000_000, 2_000_000)));
                assert_eq!(
                    execution_token_budget_snapshot(),
                    Some((1_000_000, 2_000_000))
                );
                if env_cost_unset() {
                    assert_eq!(cost, Some(40.0));
                }
                let live = live_run_budget("exec_running").expect("published");
                assert_eq!(live.iterations_used, 5);
                assert_eq!(live.tokens_used, 1_000_000);

                // An unlimited grant lifts the meter.
                append_budget_grant(
                    &path,
                    owner_grant(BudgetGrantRequest {
                        unlimited: Some(true),
                        ..Default::default()
                    }),
                )
                .unwrap();
                let (tokens, cost) = refresh_running_budget(&ctx, 1.0, 7, 0);
                assert_eq!(tokens, Some((1_000_000, u64::MAX)));
                assert_eq!(cost, None);
            },
        )
        .await;
    }

    #[tokio::test]
    async fn an_inline_segment_cannot_move_a_meter_it_does_not_own() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let dir = tempfile::tempdir().unwrap();
        let mut child = pinned_ctx(dir.path(), "magician", "exec_child");
        child.max_tokens_per_cycle = Some(10);
        with_owned_execution_token_meter(Some("exec_root".to_string()), 0, 500, async {
            let (tokens, _) = refresh_running_budget(&child, 0.0, 1, 0);
            assert_eq!(tokens, Some((0, 500)));
        })
        .await;
    }

    fn token_pause(ctx: &AgenticContext, used: u64) -> AgenticPauseState {
        use crate::magician_v2::execution::agentic::types::{EnvironmentState, ShellState};
        let mut pause = AgenticPauseState::new(
            11,
            "goal",
            "done",
            EnvironmentState::Shell(ShellState::default()),
            "summary",
            4000,
            5,
        );
        pause.paused_at = Utc::now();
        pause.max_tokens_per_cycle = ctx.max_tokens_per_cycle;
        pause.llm_tokens_used = used;
        pause
    }

    fn iteration_pause(ctx: &AgenticContext) -> AgenticPauseState {
        let mut pause = token_pause(ctx, 42);
        pause.iteration = 12_000;
        pause.max_iterations = 12_000;
        pause.loop_protective_state = Some(LoopProtectiveState {
            iteration_budget_base: Some(12_000),
            iteration_grants_applied: Some(0),
            cumulative_run_cost_usd: 7.125,
            remaining_max_duration_ms: Some(5000),
            ..Default::default()
        });
        pause
    }

    #[test]
    fn iteration_approval_is_durable_and_retried_resumes_do_not_compound_it() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "iteration_continue");
        let pause = iteration_pause(&ctx);
        ctx.max_iterations = 0;
        ctx.llm_tokens_used = 42;
        ctx.resume_loop_protective = pause.loop_protective_state.clone();
        for _ in 0..2 {
            record_iteration_continuation_grant(&ctx, &pause).unwrap();
            apply_budget_grants_on_resume(&mut ctx, &pause, false);
            assert_eq!(ctx.max_iterations, 12_000);
            assert_eq!(ctx.llm_tokens_used, 42);
            let state = ctx.resume_loop_protective.as_ref().unwrap();
            assert_eq!(state.cumulative_run_cost_usd, 7.125);
            assert_eq!(state.remaining_max_duration_ms, Some(5000));
        }
        let path = budget_grants_path_for_context(&ctx).unwrap();
        let grants = load_budget_grants(&path);
        assert_eq!(grants.len(), 1);
        let totals = BudgetGrantTotals::of(&grants);
        assert_eq!(totals.add_iterations, 12_000);
        assert_eq!(
            (
                totals.add_cost_usd,
                totals.add_tokens,
                totals.add_duration_secs
            ),
            (0.0, 0, 0)
        );
        let mut cold = ctx.clone();
        cold.max_iterations = 0;
        cold.resume_loop_protective = pause.loop_protective_state.clone();
        record_iteration_continuation_grant(&cold, &pause).unwrap();
        apply_budget_grants_on_resume(&mut cold, &pause, false);
        assert_eq!(cold.max_iterations, 12_000);
        assert_eq!(load_budget_grants(&path).len(), 1);

        let mut next_pause = pause.clone();
        next_pause.iteration_offset = 12_000;
        next_pause.loop_protective_state = cold.resume_loop_protective.clone();
        cold.max_iterations = 0;
        record_iteration_continuation_grant(&cold, &next_pause).unwrap();
        apply_budget_grants_on_resume(&mut cold, &next_pause, false);
        assert_eq!(cold.max_iterations, 12_000);
        assert_eq!(load_budget_grants(&path).len(), 2);
        assert_eq!(grant_totals_for_context(&cold).add_iterations, 24_000);
        assert_eq!(
            live_budget_from_pause(&next_pause, true).iterations_used,
            24_000
        );
        assert!(iteration_extension_prompt(&next_pause).contains("24000 used"));
    }

    #[test]
    fn unrelated_answers_cannot_grant_exhausted_iterations() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "iteration_no_approval");
        let pause = iteration_pause(&ctx);
        ctx.max_iterations = 0;
        ctx.resume_loop_protective = pause.loop_protective_state.clone();
        apply_budget_grants_on_resume(&mut ctx, &pause, false);
        assert_eq!(ctx.max_iterations, 0);
        assert_eq!(grant_totals_for_context(&ctx).add_iterations, 0);
    }

    #[test]
    fn preapproved_iterations_are_consumed_once_even_if_granted_before_the_pause() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "iteration_preapproved");
        let path = budget_grants_path_for_context(&ctx).unwrap();
        append_budget_grant(
            &path,
            owner_grant(BudgetGrantRequest {
                add_iterations: Some(50),
                ..Default::default()
            }),
        )
        .unwrap();
        let pause = iteration_pause(&ctx);
        ctx.max_iterations = 0;
        ctx.resume_loop_protective = pause.loop_protective_state.clone();
        for _ in 0..2 {
            record_iteration_continuation_grant(&ctx, &pause).unwrap();
            apply_budget_grants_on_resume(&mut ctx, &pause, false);
            assert_eq!(ctx.max_iterations, 50);
        }
        assert_eq!(load_budget_grants(&path).len(), 1);
    }

    #[test]
    fn iteration_grant_record_failure_cannot_silently_extend_a_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "iteration_corrupt");
        let pause = iteration_pause(&ctx);
        ctx.max_iterations = 0;
        let path = budget_grants_path_for_context(&ctx).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ damaged").unwrap();
        assert!(record_iteration_continuation_grant(&ctx, &pause).is_err());
        assert_eq!(ctx.max_iterations, 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"{ damaged");
    }

    #[test]
    fn ordinary_iteration_continue_cannot_widen_controller_owned_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "codex", "iteration_sealed");
        let mut pause = iteration_pause(&ctx);
        ctx.controller_attempt_hash = Some("a".repeat(64));
        pause.controller_attempt_hash = ctx.controller_attempt_hash.clone();
        ctx.max_iterations = 0;
        record_iteration_continuation_grant(&ctx, &pause).unwrap();
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(ctx.max_iterations, 0);
        assert!(budget_grants_path_for_context(&ctx).is_none());
    }

    #[test]
    fn a_damaged_grant_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(BUDGET_GRANTS_FILE);
        let damaged = b"{ incomplete grant record";
        std::fs::write(&path, damaged).unwrap();
        let result = append_budget_grant(
            &path,
            owner_grant(BudgetGrantRequest {
                add_cost_usd: Some(25.0),
                ..Default::default()
            }),
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), damaged);
    }

    #[test]
    fn relentless_controller_resume_keeps_frozen_budget_and_spend() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "magician", "exec_controller");
        let grants_path = budget_grants_path_for_context(&ctx).unwrap();
        append_budget_grant(
            &grants_path,
            owner_grant(BudgetGrantRequest {
                add_cost_usd: Some(50.0),
                add_tokens: Some(2_000_000),
                add_iterations: Some(100),
                add_duration_secs: Some(86400),
                unlimited: Some(true),
            }),
        )
        .unwrap();
        ctx.controller_attempt_hash = Some("frozen-attempt".to_string());
        ctx.max_cost_usd = Some(2.0);
        ctx.llm_tokens_used = 1_000_000;
        let iterations = ctx.max_iterations;
        let mut protective = LoopProtectiveState::default();
        protective.cumulative_run_cost_usd = 15.5;
        protective.remaining_max_duration_ms = Some(0);
        ctx.resume_loop_protective = Some(protective);
        let mut pause = token_pause(&ctx, ctx.llm_tokens_used);
        pause.controller_attempt_hash = ctx.controller_attempt_hash.clone();

        assert!(budget_grants_path_for_context(&ctx).is_none());
        assert_eq!(effective_run_cost_cap(&ctx), Some(2.0));
        assert_eq!(effective_run_token_limit(&ctx), Some(1_000_000));
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(ctx.max_iterations, iterations);
        assert_eq!(ctx.max_tokens_per_cycle, Some(1_000_000));
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .cumulative_run_cost_usd,
            15.5
        );
        assert_eq!(load_budget_grants(&grants_path).len(), 1);
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .remaining_max_duration_ms,
            Some(0)
        );

        // A sealed controller pause also blocks grants before the context's
        // controller identity is restored by the continuation bridge.
        ctx.controller_attempt_hash = None;
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(ctx.max_iterations, iterations);
        assert_eq!(ctx.max_tokens_per_cycle, Some(1_000_000));
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .cumulative_run_cost_usd,
            15.5
        );
        assert_eq!(load_budget_grants(&grants_path).len(), 1);
    }

    #[test]
    fn continue_after_a_token_pause_gets_one_fresh_allowance_and_does_not_repause() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "magician", "exec_continue");
        ctx.llm_tokens_used = 1_085_936;
        let pause = token_pause(&ctx, 1_085_936);

        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        // The pause record's base is untouched; the allowance is a grant.
        assert_eq!(ctx.max_tokens_per_cycle, Some(1_000_000));
        let limit = effective_run_token_limit(&ctx).expect("native meter");
        assert_eq!(limit, 2_000_000);
        assert!(
            ctx.llm_tokens_used < limit,
            "the first resumed iteration must not re-pause"
        );
        let grants = load_budget_grants(&budget_grants_path_for_context(&ctx).unwrap());
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].source.as_deref(), Some(CONTINUE_GRANT_SOURCE));

        // A retried resume of the same pause does not mint a second allowance.
        let mut retried = pinned_ctx(dir.path(), "magician", "exec_continue");
        retried.llm_tokens_used = 1_085_936;
        apply_budget_grants_on_resume(&mut retried, &pause, true);
        assert_eq!(
            load_budget_grants(&budget_grants_path_for_context(&retried).unwrap()).len(),
            1
        );
    }

    #[test]
    fn continue_after_an_owner_grant_mints_no_extra_allowance() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "magician", "exec_owner_grant");
        ctx.llm_tokens_used = 1_000_500;
        let pause = token_pause(&ctx, 1_000_500);
        let path = budget_grants_path_for_context(&ctx).unwrap();
        append_budget_grant(
            &path,
            owner_grant(BudgetGrantRequest {
                add_tokens: Some(1_000_000),
                add_iterations: Some(50),
                ..Default::default()
            }),
        )
        .unwrap();
        let before = ctx.max_iterations;
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(load_budget_grants(&path).len(), 1);
        assert_eq!(effective_run_token_limit(&ctx), Some(2_000_000));
        assert_eq!(ctx.max_iterations, before + 50);
    }

    #[test]
    fn a_harness_run_resumed_from_an_old_token_pause_has_no_meter_to_exhaust() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "claude_code", "exec_old_harness_pause");
        ctx.llm_tokens_used = 1_085_936;
        let pause = token_pause(&ctx, 1_085_936);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(effective_run_token_limit(&ctx), None);
        assert!(budget_grants_path_for_context(&ctx)
            .map(|path| load_budget_grants(&path).is_empty())
            .unwrap());
    }

    #[test]
    fn a_spent_cost_budget_extends_on_resume_without_erasing_spend() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        if !env_cost_unset() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = pinned_ctx(dir.path(), "magician", "exec_cost");
        let mut protective = LoopProtectiveState::default();
        protective.cumulative_run_cost_usd = 15.5;
        ctx.resume_loop_protective = Some(protective.clone());
        let pause = token_pause(&ctx, 0);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(
            ctx.resume_loop_protective
                .as_ref()
                .unwrap()
                .cumulative_run_cost_usd,
            15.5
        );

        let path = budget_grants_path_for_context(&ctx).unwrap();
        let grants = load_budget_grants(&path);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].add_cost_usd, Some(15.5));
        assert_eq!(grant_totals_for_context(&ctx).add_cost_usd, 15.5);
        apply_budget_grants_on_resume(&mut ctx, &pause, true);
        assert_eq!(load_budget_grants(&path).len(), 1);

        // With +$25 granted the run is under its cap: spend is kept.
        let mut granted = pinned_ctx(dir.path(), "magician", "exec_cost_granted");
        granted.resume_loop_protective = Some(protective);
        append_budget_grant(
            &budget_grants_path_for_context(&granted).unwrap(),
            owner_grant(BudgetGrantRequest {
                add_cost_usd: Some(25.0),
                ..Default::default()
            }),
        )
        .unwrap();
        apply_budget_grants_on_resume(&mut granted, &pause, true);
        assert_eq!(
            granted
                .resume_loop_protective
                .as_ref()
                .unwrap()
                .cumulative_run_cost_usd,
            15.5
        );
    }

    #[test]
    fn grant_requests_refuse_nothing_negative_and_nan() {
        assert!(BudgetGrantRequest::default().validate().is_err());
        assert!(BudgetGrantRequest {
            unlimited: Some(false),
            ..Default::default()
        }
        .validate()
        .is_err());
        for bad in [-1.0, 0.0, f64::NAN, f64::INFINITY] {
            assert!(BudgetGrantRequest {
                add_cost_usd: Some(bad),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        assert!(BudgetGrantRequest {
            add_tokens: Some(0),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(BudgetGrantRequest {
            add_cost_usd: Some(25.0),
            ..Default::default()
        }
        .validate()
        .is_ok());
        assert!(serde_json::from_value::<BudgetGrantRequest>(
            serde_json::json!({"add_tokens": -5})
        )
        .is_err());
        assert!(
            serde_json::from_value::<BudgetGrantRequest>(serde_json::json!({"bogus": 1})).is_err()
        );
    }

    #[test]
    fn the_effective_budget_reports_unlimited_and_null_meters() {
        let _defaults = BudgetDefaults::set(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0));
        let live = LiveRunBudget {
            harness: true,
            base_tokens: Some(1_000_000),
            plane_cost_ceiling: None,
            cost_used_usd: 12.5,
            tokens_used: 3_000_000,
            iterations_used: 11,
            iterations_limit: 4000,
        };
        let budget = EffectiveRunBudget::compute("exec_1", &live, &[], false);
        assert_eq!(budget.cost_cap_usd, serde_json::json!(100.0));
        assert_eq!(budget.tokens_limit, None);
        let unlimited = owner_grant(BudgetGrantRequest {
            unlimited: Some(true),
            ..Default::default()
        });
        let budget = EffectiveRunBudget::compute("exec_1", &live, &[unlimited], true);
        assert_eq!(budget.cost_cap_usd, serde_json::json!("unlimited"));
        let wire = serde_json::to_value(&budget).unwrap();
        for key in [
            "execution_id",
            "cost_cap_usd",
            "cost_used_usd",
            "tokens_limit",
            "tokens_used",
            "iterations_limit",
            "iterations_used",
            "resumed",
        ] {
            assert!(wire.get(key).is_some(), "missing {key}: {wire}");
        }
    }
}
