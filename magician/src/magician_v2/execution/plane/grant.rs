//! Per-run grants for the Magician plane.
//!
//! A harness never names its own authority. It presents an opaque `plt_` token
//! and the server resolves it to the run's real execution context. Nothing
//! about the run crosses the wire — the harness sends a tool name and JSON
//! arguments, and everything else is looked up here.
//!
//! Mirrors `coding_engine::citizen`, one altitude up: mint at run start, revoke
//! the moment it settles, so a stale token can never act.
//!
//! Durable `plt_` credentials (plane plan Task 10) already live in
//! [`crate::magician_v2::auth::store::AuthStore`]. This registry is the
//! *live* map of run-scoped grants: same prefix, not persisted, dies with the
//! process. Task 6's endpoint resolves the live map first, then the durable
//! store.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex as StdMutex, OnceLock,
    },
};

use magicllm::types::LLMToolSpec;
use once_cell::sync::Lazy;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::mouth_bridge::ChatMouthBridge;
use crate::magician_v2::agents::{
    ActionPattern, AgentConstraints, AgentInvocationContext, ApprovalRule, FeatureMode,
    InvocationSourceKind, InvocationSurface,
};
use crate::magician_v2::auth::sessions::{
    mint_token_value, GRANT_TOKEN_PREFIX, NEVER_ON_THE_PLANE,
};
use crate::magician_v2::execution::agentic::{ActionExecutors, AgenticContext};
use crate::magician_v2::execution::flat_loop::ToolIndex;

/// Process-global live grant map. Minted when a plane-backed run starts,
/// revoked the moment it settles.
static REGISTRY: Lazy<Arc<PlaneGrantRegistry>> =
    Lazy::new(|| Arc::new(PlaneGrantRegistry::default()));

/// The process-global plane grant registry.
pub fn plane_grant_registry() -> Arc<PlaneGrantRegistry> {
    REGISTRY.clone()
}

static RUNTIME_FORGET: OnceLock<Arc<dyn Fn(&str) + Send + Sync>> = OnceLock::new();

/// HTTP-layer hook so revoke also drops SSE/replay maps (those live in
/// `magician-api`). A missing hook is a no-op — crate tests do not serve MCP.
pub fn install_plane_runtime_forget(forget: Arc<dyn Fn(&str) + Send + Sync>) {
    let _ = RUNTIME_FORGET.set(forget);
}

fn forget_runtime(token: &str) {
    if let Some(forget) = RUNTIME_FORGET.get() {
        forget(token.trim());
    }
}

/// Why a plane `tools/call` stopped mid-turn.
///
/// Lives here rather than on `HarnessStopReason` so `grant` does not
/// import `engine` (that module already revokes through this one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneTurnStopReason {
    /// T1: end the harness turn so Magician's loop can pause and ask.
    NeedsApproval,
    /// T1: the turn's tool-call bound is spent. Ends the turn the same way —
    /// the harness survives a refusal — but no human is asked; control simply
    /// returns to the loop, which wakes with a fresh budget next iteration.
    TurnBudgetSpent,
    /// T1: the harness asked to delegate. The turn ends, the loop spawns the
    /// children it captured on the grant and parks; the next turn resumes
    /// this conversation with their deliverables.
    Delegate,
}

/// What the plane does when a pause-shaped gate fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanePauseDisposition {
    /// Live harness turn (T1). Control returns to the loop.
    EndTurn,
    /// Terminal declared MCP elicitation (Task 12a). Until that task
    /// lands this still fails closed as a refusal at the call site.
    Elicit,
    /// No turn, no elicitation — refuse.
    Refuse,
}

/// Which hot tier this grant projects.
///
/// Magician-spawned bare harnesses have no native tools (`--tools ""`); a
/// terminal already has better file/shell than ours. The two profiles are
/// opposite, and a test asserts they differ — a spawned grant given the
/// terminal list still *works*, just slowly, and CI would not notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaneCatalogProfile {
    /// Magician spawned the process and stripped its native tools.
    SpawnedBare,
    /// An operator terminal Magician does not control.
    #[default]
    Terminal,
}

/// The gated action that ended a live harness turn, captured at the
/// pause-shaped gate so the loop can execute exactly it on resume (plane
/// Task 8, execute-on-resume) instead of relying on a foreign harness to
/// re-issue a byte-identical call.
///
/// `action_json` uses the loop's `stable_confirmation_action_json`
/// serialization: the durable pause, the resume path, and any replay
/// comparison all see one canonical form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanePendingApproval {
    /// Unique id for THIS capture. The approval must bind to the exact
    /// capture it describes: the grant's slot is single and shared, so a
    /// later gated call supersedes an earlier one, and an answer arriving
    /// for a superseded capture must refuse rather than execute the newer
    /// action under the older approval (Task 12a review).
    pub capture_id: String,
    pub action_json: String,
    pub action_summary: String,
    pub action_type: String,
}

/// May this grant delegate work to Magician runs, or only act directly?
///
/// `None` is T3 / Direct tools — no run. `Some(_)` is Variant 2's start: the
/// caller may `run_task`, and the run's **engine** comes from
/// `harness_engine` here — the terminal's harness by default, with
/// `"magician"` as a deliberate forced pin, never a silent fallback. A name
/// Magician cannot launch makes `run_task` refuse.
///
/// Plane plan Task 6b. Until the delegated-launch entry lands (attenuation
/// travelling onto the new run's catalog), a grant with run authority still
/// gets a fail-closed refusal from `run_task` rather than an unattenuated
/// start.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaneRunAuthority {
    /// Agents this grant may run as. Empty means "the surface's defaults",
    /// which `InvocationSurface::Plane` makes fail-closed.
    pub allowed_agents: Vec<String>,
    /// Who thinks during a run this grant starts.
    pub harness_engine: String,
    /// Ceilings applied to every run this grant starts, on top of the task's.
    pub max_usd: Option<f64>,
    pub max_wall_clock: Option<std::time::Duration>,
    pub max_concurrent_runs: usize,
}

/// Ceiling on the result text one turn-ledger record carries. The transcript
/// replays these to a model on a later cold turn, so a single large result
/// must not crowd out the rest of the conversation.
pub const PLANE_TURN_RESULT_MAX_BYTES: usize = 8 * 1024;
/// Marks a record's content as a head when the bound cut it, so a replay
/// never mistakes the head for the whole.
pub const PLANE_TURN_RESULT_CUT_MARK: &str = " \u{2026}[cut]";

/// The head of `text` within `max` bytes, cut on a character boundary.
pub fn head_within(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
/// Ceiling on the argument and result bytes one turn's ledger carries in
/// all. The turn's call budget is Magician-run-scale, and the whole ledger
/// is what the chat turn persists, so a bound per record alone would let a
/// long turn outgrow the transcript store's per-append segment; past this
/// a call is still counted, its payload omitted.
pub const PLANE_TURN_LEDGER_MAX_BYTES: usize = 512 * 1024;
/// What a record carries in place of its result once the turn's ledger
/// budget is spent.
pub const PLANE_TURN_LEDGER_BUDGET_SPENT: &str = "[omitted: turn ledger budget spent]";

/// One plane call of a chat harness turn, as the transcript will carry it.
///
/// A swapped mouth's hands act through the plane, so without this record
/// the chat transcript would hold only the reply text: a later cold turn
/// (engine switch, restart, native-mouth takeover) could not see what the
/// tools returned. Recorded by the governed tail per dispatched call and
/// drained by the chat turn when it settles.
#[derive(Debug, Clone)]
pub struct PlaneTurnToolCall {
    /// The call's `action_invocation_ref`, the same id its chat tool events
    /// carry.
    pub call_id: String,
    pub tool_name: String,
    /// Bounded the way the durable event row bounds them.
    pub arguments: Value,
    /// Result text bounded to `PLANE_TURN_RESULT_MAX_BYTES` (a cut head
    /// ends in `PLANE_TURN_RESULT_CUT_MARK`); an error as the status object
    /// the chat's tool events carry, so a replay can tell the two apart.
    pub content: String,
    pub is_error: bool,
}

impl PlaneTurnToolCall {
    /// What this record costs against the turn's ledger budget: its
    /// arguments as compact JSON plus its content.
    fn payload_bytes(&self) -> usize {
        let arguments = serde_json::to_vec(&self.arguments)
            .map(|compact| compact.len())
            .unwrap_or(usize::MAX);
        arguments.saturating_add(self.content.len())
    }

    /// The same call with its payload omitted: the id and name stay so the
    /// call count stays honest and every call still balances a result.
    fn budget_spent(self) -> Self {
        Self {
            call_id: self.call_id,
            tool_name: self.tool_name,
            arguments: Value::Null,
            content: PLANE_TURN_LEDGER_BUDGET_SPENT.to_owned(),
            is_error: false,
        }
    }
}

/// The calls one chat harness turn dispatched, in dispatch order, with the
/// payload bytes they have spent of `PLANE_TURN_LEDGER_MAX_BYTES`.
#[derive(Debug, Default)]
pub struct PlaneTurnLedger {
    calls: Vec<PlaneTurnToolCall>,
    bytes: usize,
}

impl PlaneTurnLedger {
    /// Record one dispatched call. The budget latches shut on the first
    /// call that does not fit: it and every call after it are recorded
    /// with their payload omitted, never dropped, so the transcript still
    /// names everything that ran.
    pub fn record(&mut self, call: PlaneTurnToolCall) {
        if self.bytes >= PLANE_TURN_LEDGER_MAX_BYTES {
            self.calls.push(call.budget_spent());
            return;
        }
        let spent = self.bytes.saturating_add(call.payload_bytes());
        if spent > PLANE_TURN_LEDGER_MAX_BYTES {
            self.bytes = PLANE_TURN_LEDGER_MAX_BYTES;
            self.calls.push(call.budget_spent());
            return;
        }
        self.bytes = spent;
        self.calls.push(call);
    }

    /// Take the calls in dispatch order, leaving a fresh ledger.
    pub fn drain(&mut self) -> Vec<PlaneTurnToolCall> {
        std::mem::take(self).calls
    }
}

/// Take what a turn ledger holds, in dispatch order, leaving it empty. A
/// grant without a ledger yields nothing. The chat turn drains through the
/// `Arc` it kept before the mint, since the grant itself has gone into the
/// registry by the time the turn settles.
pub(crate) fn drain_turn_ledger(
    ledger: &Option<Arc<StdMutex<PlaneTurnLedger>>>,
) -> Vec<PlaneTurnToolCall> {
    ledger
        .as_ref()
        .map(|ledger| {
            ledger
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .drain()
        })
        .unwrap_or_default()
}

/// Unique `action_invocation_ref` per `tools/call`.
///
/// `execute_action_inner` opens by clearing any app-labeled result for this
/// ref, so a reused value would wipe another in-flight call's result. Task 4
/// threads this into dispatch; Task 2 owns the mint so the name exists.
pub fn mint_invocation_ref() -> String {
    format!("pltinv_{}", Uuid::new_v4().simple())
}

/// Revoke a process-local turn grant if its owning future is dropped before
/// the normal settle path. These grants have no expiry, so an aborted turn
/// must not leave a bearer callable for the lifetime of the service.
#[doc(hidden)]
pub struct RevokeGrantOnDrop(Option<String>);

impl RevokeGrantOnDrop {
    #[doc(hidden)]
    pub fn new(token: String) -> Self {
        Self(Some(token))
    }

    /// The owner explicitly revoked the grant or transferred it to pause
    /// cleanup after any in-flight dispatch.
    #[doc(hidden)]
    pub fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for RevokeGrantOnDrop {
    fn drop(&mut self) {
        let Some(token) = self.0.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                plane_grant_registry().revoke(&token).await;
            });
        }
    }
}

/// Live authority a plane `tools/call` resolves to.
///
/// `executors` is the live handle `execute_action` needs. A T1 mint from
/// a running execution attaches the run's `Arc`. Direct tools (T3) without
/// a session context (Task 11) leave this `None`; the MCP wire advertises no
/// runnable tools and a guessed `tools/call` returns an honest unwired error.
#[derive(Clone)]
pub struct PlaneGrant {
    /// The run's context. Run and terminal mint stamp
    /// [`InvocationSurface::Plane`]. Chat conversation mint keeps the
    /// conversation's lane surface.
    pub ctx: AgenticContext,
    /// Live executors for governed dispatch. Shared across clones of the
    /// same grant (the registry clones out before `await`).
    pub executors: Option<Arc<ActionExecutors>>,
    pub session_id: String,
    pub cancellation_token: Option<CancellationToken>,
    /// Least privilege **on top of** [`NEVER_ON_THE_PLANE`]. Empty means "the
    /// catalog minus the denied families", never "everything".
    ///
    /// This is attenuation *within* a surface, not the surface itself. Which
    /// agents are reachable at all is decided by `allowed_direct_surfaces`.
    pub allowed_tools: Vec<String>,
    /// Which hot list `tools/list` projects. Spawned-bare vs terminal.
    pub catalog_profile: PlaneCatalogProfile,
    /// Canonical schemas and deferred universe for `tool_search`. Durable
    /// terminal projections receive the runtime index at the authenticated door.
    pub tool_index: Arc<ToolIndex>,
    /// Pack-confirmation rules this grant evaluates itself. The loop's
    /// `checked_approval` is keyed to a `Decision` a harness never produces.
    pub constraints: AgentConstraints,
    /// True while a Magician-spawned harness turn is in flight (T1).
    pub live_harness_turn: bool,
    /// Terminal declared MCP elicitation (Task 12a).
    pub elicitation_enabled: bool,
    /// Shared so a `tools/call` on a cloned grant is visible to the loop.
    pub turn_stop: Arc<StdMutex<Option<PlaneTurnStopReason>>>,
    /// One governed dispatch at a time per grant. Approval state, loaded-tool
    /// state, and action scratch are grant-scoped; allowing two MCP calls to
    /// race would let both observe the same one-shot authority and then spend
    /// it independently.
    pub(crate) dispatch_lock: Arc<Mutex<()>>,
    /// Registry removal is not enough: a request may have cloned the grant and
    /// be queued on `dispatch_lock`. Every clone observes this bit.
    revoked: Arc<AtomicBool>,
    /// Wakes the harness turn immediately when a plane call encounters a
    /// pause-shaped gate. This is deliberately separate from run cancellation.
    turn_stop_signal: CancellationToken,
    /// Turn-scope tool-call bound (plane Task 8). `0` means no ceiling. The
    /// seam sets it at mint and carries the spent count across an approval
    /// pause, so a resumed turn inherits only the remainder of its bound.
    turn_tool_call_limit: u32,
    turn_tool_calls_spent: Arc<AtomicU32>,
    /// The action whose approval gate ended the live turn, if any. Taken by
    /// the decide seam when the turn settles `NeedsApproval`.
    pending_approval: Arc<StdMutex<Option<PlanePendingApproval>>>,
    /// The delegation a harness turn asked for, waiting for the loop that
    /// settles the turn. Single and superseding, like the approval slot.
    pending_delegation:
        Arc<StdMutex<Option<Vec<crate::magician_v2::execution::actions::DelegationTargetRequest>>>>,
    /// Delegated-run authority (plane Task 6b). `None` keeps this grant to
    /// Direct tools: `run_task` refuses.
    pub run_authority: Option<PlaneRunAuthority>,
    /// Set when the mouth cannot re-list tools mid-turn and the allowlisted
    /// Deferred hands were loaded at mint. A later `select:` then merges
    /// instead of replacing, so the door never drops a tool the mouth can
    /// still see.
    pub preloaded_deferred: bool,
    /// The turn's dispatched calls, for the chat transcript. Conversation
    /// grants only; run and terminal grants have their own records. Shared
    /// across clones so a `tools/call` on a resolved clone lands in the
    /// ledger the chat turn drains.
    pub turn_ledger: Option<Arc<StdMutex<PlaneTurnLedger>>>,
    /// The native mouth's own tools this grant advertises and dispatches
    /// through `mouth_bridge` instead of the plane's executors, by name,
    /// with the schema the native mouth advertises. Conversation grants
    /// only; empty everywhere else. A name here is hot and never reaches
    /// lowering, the pause gate, or `execute_action`.
    pub bridged_tools: BTreeMap<String, LLMToolSpec>,
    /// The chat service's dispatcher, packaged for this turn. `None` makes
    /// every bridged name an honest error at dispatch.
    pub mouth_bridge: Option<ChatMouthBridge>,
}

impl PlaneGrant {
    /// The surface this grant acts on. Run and terminal mints stamp
    /// [`InvocationSurface::Plane`]. Conversation mints keep the chat/lane
    /// surface already on `ctx`.
    pub fn surface(&self) -> InvocationSurface {
        self.ctx
            .invocation_context_override
            .as_ref()
            .map(|invocation| invocation.surface)
            .unwrap_or(InvocationSurface::Plane)
    }

    pub fn permits(&self, tool: &str) -> bool {
        if NEVER_ON_THE_PLANE.contains(&tool) {
            return false;
        }
        if self
            .ctx
            .denied_capability_names
            .iter()
            .chain(self.ctx.plane_denied_capability_names.iter())
            .any(|denied| crate::magician_v2::agents::tool_name_matches_block_entry(tool, denied))
        {
            return false;
        }
        if self
            .ctx
            .plane_allowed_capability_names
            .as_ref()
            .is_some_and(|allowed| !allowed.iter().any(|name| name == tool))
        {
            return false;
        }
        self.allowed_tools.is_empty() || self.allowed_tools.iter().any(|name| name == tool)
    }

    /// Serialize the complete authorize-and-dispatch transaction for this
    /// grant, not merely the executor call at its tail.
    #[doc(hidden)]
    pub async fn lock_dispatch(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.dispatch_lock).lock_owned().await
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    fn mark_revoked(&self) {
        self.revoked.store(true, Ordering::Release);
        if let Some(token) = &self.cancellation_token {
            token.cancel();
        }
    }

    pub fn pause_disposition(&self) -> PlanePauseDisposition {
        // Chat-scoped grants keep the conversation's lane surface. Gated
        // actions refuse (Task 4) — chat HITL is the next user message,
        // not Magician pending_inputs / EndTurn.
        if self.surface() != InvocationSurface::Plane {
            return PlanePauseDisposition::Refuse;
        }
        if self.live_harness_turn {
            PlanePauseDisposition::EndTurn
        } else if self.elicitation_enabled {
            PlanePauseDisposition::Elicit
        } else {
            PlanePauseDisposition::Refuse
        }
    }

    pub fn turn_stop_reason(&self) -> Option<PlaneTurnStopReason> {
        self.turn_stop
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_turn_stop(&self, reason: PlaneTurnStopReason) {
        *self
            .turn_stop
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason);
        self.turn_stop_signal.cancel();
    }

    pub fn turn_stop_signal(&self) -> CancellationToken {
        self.turn_stop_signal.clone()
    }

    /// Set this grant's turn tool-call bound, optionally starting the spent
    /// count above zero when a resumed turn inherits only the remainder of a
    /// bound that was interrupted by an approval pause.
    pub fn with_turn_tool_budget(mut self, limit: u32, spent: u32) -> Self {
        self.turn_tool_call_limit = limit;
        self.turn_tool_calls_spent.store(spent, Ordering::Relaxed);
        self
    }

    /// Attach the scope's tool index so grant-scoped `tool_search` can find
    /// and load Deferred names from the allowlist. Sets both the grant field
    /// and the context so the two never disagree.
    pub fn with_tool_index(mut self, index: Arc<ToolIndex>) -> Self {
        self.ctx.tool_index = Some(Arc::clone(&index));
        self.tool_index = index;
        self
    }

    /// Attach the native mouth's tools this grant bridges and the bridge
    /// that runs them. An attenuated allowlist admits the bridged names too,
    /// so `permits` and the catalog agree on them; an empty allowlist stays
    /// empty (it already admits every name outside the denied families).
    pub fn with_bridged_tools(
        mut self,
        specs: BTreeMap<String, LLMToolSpec>,
        bridge: ChatMouthBridge,
    ) -> Self {
        if !self.allowed_tools.is_empty() {
            for name in specs.keys() {
                if !self.allowed_tools.iter().any(|allowed| allowed == name) {
                    self.allowed_tools.push(name.clone());
                }
            }
        }
        self.bridged_tools = specs;
        self.mouth_bridge = Some(bridge);
        self
    }

    pub fn turn_tool_call_limit(&self) -> u32 {
        self.turn_tool_call_limit
    }

    pub fn turn_tool_calls_spent(&self) -> u32 {
        self.turn_tool_calls_spent.load(Ordering::Relaxed)
    }

    pub fn turn_tool_call_budget_exhausted(&self) -> bool {
        self.turn_tool_call_limit != 0 && self.turn_tool_calls_spent() >= self.turn_tool_call_limit
    }

    /// Count one governed dispatch against the turn bound. Returns the spent
    /// count including this call.
    pub(crate) fn count_turn_tool_call(&self) -> u32 {
        self.turn_tool_calls_spent.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn set_pending_approval(&self, approval: PlanePendingApproval) {
        *self
            .pending_approval
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(approval);
    }

    pub fn set_pending_delegation(
        &self,
        targets: Vec<crate::magician_v2::execution::actions::DelegationTargetRequest>,
    ) {
        *self
            .pending_delegation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(targets);
    }

    pub fn take_pending_delegation(
        &self,
    ) -> Option<Vec<crate::magician_v2::execution::actions::DelegationTargetRequest>> {
        self.pending_delegation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub fn take_pending_approval(&self) -> Option<PlanePendingApproval> {
        self.pending_approval
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Compare before consuming: a stale answer cannot discard a newer turn's
    /// capture. Terminal MCP calls own their captures independently of this slot.
    pub fn take_matching_pending_approval(&self, expected: &str) -> Option<PlanePendingApproval> {
        let mut pending = self
            .pending_approval
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if pending.as_ref().is_some_and(|p| p.capture_id == expected) {
            pending.take()
        } else {
            None
        }
    }

    pub fn with_live_harness_turn(mut self) -> Self {
        self.live_harness_turn = true;
        self
    }

    pub fn with_elicitation(mut self) -> Self {
        self.elicitation_enabled = true;
        self
    }

    pub fn with_run_authority(mut self, authority: PlaneRunAuthority) -> Self {
        self.run_authority = Some(authority);
        self
    }

    pub fn with_approval_rule_for(mut self, tool: &str) -> Self {
        let rule_tool = crate::magician_v2::agents::approval::pack_tool_for_approval(tool);
        self.constraints.requires_approval.push(ApprovalRule {
            tool: rule_tool,
            action: ActionPattern::Single("*".to_string()),
            when: None,
            ttl_secs: None,
        });
        self
    }

    /// Tools `tool_search` has loaded onto this grant. Shared across clones of
    /// the same `AgenticContext` (the `Arc` inside scratch), so a mint, a
    /// resolve, and a `tools/call` all see one set.
    pub fn loaded_tool_names(&self) -> HashSet<String> {
        self.ctx
            .scratch
            .loaded_tools
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Fixture grant for plane tests in this crate and in `magician-api`.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn for_test(execution_id: &str) -> Self {
        let mut ctx = AgenticContext::default();
        ctx.execution_id = Some(execution_id.to_string());
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("default".to_string());
        ctx.agent_id = Some("personal-assistant".to_string());
        Self {
            ctx,
            executors: None,
            session_id: format!("sess-{execution_id}"),
            cancellation_token: Some(CancellationToken::new()),
            allowed_tools: Vec::new(),
            catalog_profile: PlaneCatalogProfile::Terminal,
            tool_index: Arc::new(ToolIndex::from_entries(Vec::new())),
            constraints: AgentConstraints::default(),
            live_harness_turn: false,
            elicitation_enabled: false,
            turn_stop: Arc::new(StdMutex::new(None)),
            dispatch_lock: Arc::new(Mutex::new(())),
            revoked: Arc::new(AtomicBool::new(false)),
            turn_stop_signal: CancellationToken::new(),
            turn_tool_call_limit: 0,
            turn_tool_calls_spent: Arc::new(AtomicU32::new(0)),
            pending_approval: Arc::new(StdMutex::new(None)),
            pending_delegation: Arc::new(StdMutex::new(None)),
            run_authority: None,
            preloaded_deferred: false,
            turn_ledger: None,
            bridged_tools: BTreeMap::new(),
            mouth_bridge: None,
        }
    }

    /// Rehydrate the live, process-local projection of a durable terminal
    /// grant. The caller must first validate the token, identity, expiry, and
    /// engraved workspace against `AuthStore`; this constructor only creates
    /// the governed execution context cached by the plane registry.
    pub fn for_terminal(
        mut ctx: AgenticContext,
        session_id: String,
        allowed_tools: Vec<String>,
        tool_index: Arc<ToolIndex>,
    ) -> Self {
        ctx.invocation_context_override = None;
        ctx.tool_index = Some(Arc::clone(&tool_index));
        let mut grant = Self {
            ctx,
            executors: None,
            session_id,
            cancellation_token: Some(CancellationToken::new()),
            allowed_tools,
            catalog_profile: PlaneCatalogProfile::Terminal,
            tool_index,
            constraints: AgentConstraints::default(),
            live_harness_turn: false,
            elicitation_enabled: false,
            turn_stop: Arc::new(StdMutex::new(None)),
            dispatch_lock: Arc::new(Mutex::new(())),
            revoked: Arc::new(AtomicBool::new(false)),
            turn_stop_signal: CancellationToken::new(),
            turn_tool_call_limit: 0,
            turn_tool_calls_spent: Arc::new(AtomicU32::new(0)),
            pending_approval: Arc::new(StdMutex::new(None)),
            pending_delegation: Arc::new(StdMutex::new(None)),
            run_authority: None,
            preloaded_deferred: false,
            turn_ledger: None,
            bridged_tools: BTreeMap::new(),
            mouth_bridge: None,
        };
        grant.stamp_plane_surface();
        grant
    }

    /// Conversation-scoped grant for a chat harness mouth. Keeps the
    /// invocation surface already on `ctx` (Chat / Tutor / …). Does not
    /// stamp Plane — that would drop lane admission.
    ///
    /// `cancel` is the chat-turn token. The grant holds a **child** of it:
    /// cancelling the turn still stops in-flight dispatch, but revoking the
    /// grant must not cancel the conversation (revoke fires at every settled
    /// turn).
    pub fn for_conversation(
        mut ctx: AgenticContext,
        session_id: String,
        cancel: CancellationToken,
    ) -> Self {
        if ctx.invocation_context_override.is_none() {
            ctx.invocation_context_override = Some(AgentInvocationContext {
                principal: ctx.principal.clone().unwrap_or_default(),
                workspace: ctx.workspace.clone().unwrap_or_default(),
                source_agent_id: None,
                target_agent_id: ctx.agent_id.clone().unwrap_or_default(),
                surface: InvocationSurface::Chat,
                feature_mode: FeatureMode::None,
                source_kind: InvocationSourceKind::Direct,
                chat_session_id: ctx.chat_session_id.clone(),
                chat_turn_id: None,
            });
        }
        // The chat mouth is the parent engine of every operation this
        // grant's calls reach: named on the context's routing overrides,
        // which the door reads for each `tools/call` and the grant's scoped
        // executors route through. The native mouth names none.
        ctx.llm_routing_overrides = Some(
            ctx.llm_routing_overrides
                .clone()
                .unwrap_or_default()
                .with_parent_engine(ctx.harness_engine.clone()),
        );
        Self {
            ctx,
            executors: None,
            session_id,
            cancellation_token: Some(cancel.child_token()),
            allowed_tools: Vec::new(),
            catalog_profile: PlaneCatalogProfile::SpawnedBare,
            tool_index: Arc::new(ToolIndex::from_entries(Vec::new())),
            constraints: AgentConstraints::default(),
            live_harness_turn: true,
            elicitation_enabled: false,
            turn_stop: Arc::new(StdMutex::new(None)),
            dispatch_lock: Arc::new(Mutex::new(())),
            revoked: Arc::new(AtomicBool::new(false)),
            turn_stop_signal: CancellationToken::new(),
            turn_tool_call_limit: 0,
            turn_tool_calls_spent: Arc::new(AtomicU32::new(0)),
            pending_approval: Arc::new(StdMutex::new(None)),
            pending_delegation: Arc::new(StdMutex::new(None)),
            run_authority: None,
            preloaded_deferred: false,
            turn_ledger: Some(Arc::new(StdMutex::new(PlaneTurnLedger::default()))),
            bridged_tools: BTreeMap::new(),
            mouth_bridge: None,
        }
    }

    /// Mint-ready grant for a Magician-owned run that thinks with a harness.
    pub fn for_run(
        mut ctx: AgenticContext,
        executors: Arc<ActionExecutors>,
        execution_id: String,
    ) -> Self {
        let session_id = format!("sess-{execution_id}");
        // The run's engine is the grant's parent engine — the one choice the
        // decide seam and the run's own scoped router already make.
        ctx.llm_routing_overrides = Some(
            ctx.llm_routing_overrides
                .clone()
                .unwrap_or_default()
                .with_parent_engine(super::turn_engine::run_parent_engine(&ctx)),
        );
        let tool_index = ctx
            .tool_index
            .clone()
            .unwrap_or_else(|| Arc::new(ToolIndex::from_entries(Vec::new())));
        let mut constraints = AgentConstraints::default();
        constraints.requires_approval = ctx.approval_rules.clone();
        let catalog_ctx =
            crate::magician_v2::execution::agentic::native_integration::build_catalog_context(&ctx);
        let loaded =
            crate::magician_v2::execution::agentic::native_integration::snapshot_loaded_tools(&ctx);
        let catalog = crate::magician_v2::execution::flat_loop::build_flat_loop_tools(
            &catalog_ctx,
            &tool_index,
            &loaded,
        );
        // A run inherits its owner's actual catalog. Static plane hot names
        // are not proof that the scoped runtime still has a tool/provider, and
        // the index is not proof either: it says what the scope indexes, while
        // the executors' effective registry — narrowed to the owner's tool
        // scope — says what this run can dispatch. A name that is indexed but
        // has no provider here would be advertised, called, and fail with
        // "No provider registered". Magician's own loop avoids that only by
        // preferring `files`; a switched engine follows the catalog as written.
        // An empty registry is unknown dispatchability, not a denial: a run
        // whose scope has not been applied would otherwise lose its whole
        // catalog, which is a worse failure than advertising a tool that might
        // not dispatch.
        let dispatchable = executors
            .effective_capability_registry_snapshot()
            .filter(|registry| !registry.tool_names().is_empty());
        // A skill pack registers one provider under its pack name; its
        // actions are leaf tools (`browser__open`). The registry answers for
        // the pack, so a leaf is dispatchable when its pack is — checking the
        // leaf name alone dropped every action of every multi-action pack
        // from a switched engine's catalog.
        let mut allowed_tools: Vec<String> = catalog
            .hot
            .into_iter()
            .map(|tool| tool.name)
            .chain(catalog.deferred.into_iter().map(|tool| tool.name))
            .filter(|name| tool_index.get(name).is_some())
            .filter(|name| {
                dispatchable.as_ref().is_none_or(|registry| {
                    registry.has(name)
                        || tool_index.get(name).is_some_and(|entry| {
                            entry.pack_name != *name && registry.has(&entry.pack_name)
                        })
                })
            })
            .collect();
        // Keep an explicit non-empty ceiling even when the owner has no
        // runnable tools: empty allowed_tools means unrestricted on the plane.
        if !allowed_tools.iter().any(|name| name == "tool_search") {
            allowed_tools.push("tool_search".into());
        }
        // Delegation is the loop's, not a registered tool, so no index entry
        // ever admits it. A run whose executors can dispatch a delegation
        // advertises the verb; the loop's own validation then judges each
        // target exactly as it does for magician.
        if executors.delegation_dispatcher.is_some()
            && !allowed_tools.iter().any(|name| name == "delegate_to_agent")
        {
            allowed_tools.push("delegate_to_agent".into());
        }
        let mut grant = Self {
            ctx,
            executors: Some(executors),
            session_id,
            cancellation_token: Some(CancellationToken::new()),
            allowed_tools,
            catalog_profile: PlaneCatalogProfile::SpawnedBare,
            tool_index,
            constraints,
            live_harness_turn: true,
            elicitation_enabled: false,
            turn_stop: Arc::new(StdMutex::new(None)),
            dispatch_lock: Arc::new(Mutex::new(())),
            revoked: Arc::new(AtomicBool::new(false)),
            turn_stop_signal: CancellationToken::new(),
            turn_tool_call_limit: 0,
            turn_tool_calls_spent: Arc::new(AtomicU32::new(0)),
            pending_approval: Arc::new(StdMutex::new(None)),
            pending_delegation: Arc::new(StdMutex::new(None)),
            run_authority: None,
            preloaded_deferred: false,
            turn_ledger: None,
            bridged_tools: BTreeMap::new(),
            mouth_bridge: None,
        };
        grant.stamp_plane_surface();
        grant
    }

    pub fn replace_loaded_tools(&self, names: HashSet<String>) {
        *self
            .ctx
            .scratch
            .loaded_tools
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = names;
    }

    fn stamp_plane_surface(&mut self) {
        match self.ctx.invocation_context_override.as_mut() {
            Some(invocation) => invocation.surface = InvocationSurface::Plane,
            None => {
                self.ctx.invocation_context_override = Some(AgentInvocationContext {
                    principal: self.ctx.principal.clone().unwrap_or_default(),
                    workspace: self.ctx.workspace.clone().unwrap_or_default(),
                    source_agent_id: None,
                    target_agent_id: self.ctx.agent_id.clone().unwrap_or_default(),
                    surface: InvocationSurface::Plane,
                    feature_mode: FeatureMode::None,
                    source_kind: InvocationSourceKind::Direct,
                    chat_session_id: self.ctx.chat_session_id.clone(),
                    chat_turn_id: None,
                });
            },
        }
    }

    fn same_durable_authority(&self, other: &Self) -> bool {
        self.ctx.principal == other.ctx.principal
            && self.ctx.workspace == other.ctx.workspace
            && self.ctx.agent_id == other.ctx.agent_id
            && self.ctx.execution_id == other.ctx.execution_id
            && self.session_id == other.session_id
            && self.allowed_tools == other.allowed_tools
            && self.catalog_profile == other.catalog_profile
            && self.surface() == other.surface()
            && self.live_harness_turn == other.live_harness_turn
            && self.elicitation_enabled == other.elicitation_enabled
    }
}

#[derive(Default)]
pub struct PlaneGrantRegistry {
    grants: Mutex<HashMap<String, RegisteredPlaneGrant>>,
}

#[derive(Clone)]
struct RegisteredPlaneGrant {
    grant: PlaneGrant,
    origin: PlaneGrantOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaneGrantOrigin {
    RunScoped,
    DurableTerminal,
    ChatScoped,
}

impl PlaneGrantOrigin {
    /// Live map entries this process minted and can trust without
    /// revalidating a durable store row.
    fn is_process_local(self) -> bool {
        matches!(self, Self::RunScoped | Self::ChatScoped)
    }
}

impl PlaneGrantRegistry {
    /// Mint a fresh opaque `plt_` token for `grant` and register it.
    ///
    /// The grant is stamped onto [`InvocationSurface::Plane`] here, not by the
    /// caller: a mint that inherited Chat (or anything else) would make every
    /// default-surface agent reachable from a terminal.
    ///
    /// Returns the token to inject into the harness. Revoke with
    /// [`Self::revoke`] when the run settles.
    pub async fn mint(&self, mut grant: PlaneGrant) -> String {
        grant.stamp_plane_surface();
        if grant.cancellation_token.is_none() {
            grant.cancellation_token = Some(CancellationToken::new());
        }
        let token = mint_token_value(GRANT_TOKEN_PREFIX);
        self.grants.lock().await.insert(
            token.clone(),
            RegisteredPlaneGrant {
                grant,
                origin: PlaneGrantOrigin::RunScoped,
            },
        );
        token
    }

    /// Mint a conversation-scoped grant. Does **not** stamp Plane: the
    /// caller already set the chat/lane surface on the grant.
    pub async fn mint_chat(&self, mut grant: PlaneGrant) -> String {
        if grant.cancellation_token.is_none() {
            grant.cancellation_token = Some(CancellationToken::new());
        }
        let token = mint_token_value(GRANT_TOKEN_PREFIX);
        self.grants.lock().await.insert(
            token.clone(),
            RegisteredPlaneGrant {
                grant,
                origin: PlaneGrantOrigin::ChatScoped,
            },
        );
        token
    }

    /// Resolve a Bearer token to its grant (`None` = unknown / already
    /// revoked).
    ///
    /// Clones the grant out and drops the map guard before returning, so a
    /// later `await` on dispatch cannot hold the mutex — the same reason
    /// `citizen` clones out. Holding the guard across `execute_action` would
    /// make the future non-`Send` and deadlock a second concurrent call from
    /// the same run.
    pub async fn resolve(&self, token: &str) -> Option<PlaneGrant> {
        self.grants
            .lock()
            .await
            .get(token.trim())
            .map(|registered| registered.grant.clone())
    }

    /// Resolve a process-local grant minted by this process (a Magician-owned
    /// run, or a chat-turn conversation grant). Durable terminal credentials
    /// must be revalidated against `AuthStore` on every request; otherwise
    /// deleting or expiring their stored row would leave a cached live grant
    /// acting indefinitely.
    pub async fn resolve_run_scoped(&self, token: &str) -> Option<PlaneGrant> {
        self.grants
            .lock()
            .await
            .get(token.trim())
            .filter(|registered| registered.origin.is_process_local())
            .map(|registered| registered.grant.clone())
    }

    /// Cache the process-local projection of a terminal grant after the API
    /// has revalidated its durable authority. Reusing the projection preserves
    /// grant-local `tool_search` state across MCP requests.
    pub async fn cache_durable(&self, token: &str, grant: PlaneGrant) -> PlaneGrant {
        let mut grants = self.grants.lock().await;
        if let Some(existing) = grants
            .get_mut(token.trim())
            .filter(|registered| registered.origin == PlaneGrantOrigin::DurableTerminal)
        {
            if existing.grant.same_durable_authority(&grant) {
                // Runtime metadata can become available or refresh without
                // changing bearer authority. Keep the shared dispatch/approval
                // and loaded-tool state while refreshing the catalog handles.
                existing.grant.tool_index = grant.tool_index;
                existing.grant.ctx.tool_index = grant.ctx.tool_index;
                existing.grant.executors = grant.executors;
                return existing.grant.clone();
            }
            // The durable row was just revalidated, so its new identity,
            // workspace, tool authority, and execution projection replace the
            // old grant. Only catalog selection is safe/cacheable across requests.
            grant.replace_loaded_tools(existing.grant.loaded_tool_names());
            existing.grant.mark_revoked();
        }
        grants.insert(
            token.trim().to_string(),
            RegisteredPlaneGrant {
                grant: grant.clone(),
                origin: PlaneGrantOrigin::DurableTerminal,
            },
        );
        grant
    }

    /// Remove a cached terminal projection after durable validation fails.
    /// Run-scoped grants use the same token prefix and must not be removed by
    /// this cleanup path.
    pub async fn revoke_cached_durable(&self, token: &str) {
        let mut grants = self.grants.lock().await;
        let durable = grants
            .get(token.trim())
            .is_some_and(|registered| registered.origin == PlaneGrantOrigin::DurableTerminal);
        if !durable {
            return;
        }
        if let Some(registered) = grants.remove(token.trim()) {
            registered.grant.mark_revoked();
        }
        drop(grants);
        forget_runtime(token);
    }

    /// Drop a token the moment its run settles, so a stale token can never act.
    pub async fn revoke(&self, token: &str) {
        if let Some(registered) = self.grants.lock().await.remove(token.trim()) {
            registered.grant.mark_revoked();
        }
        forget_runtime(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn aborted_turn_releases_its_process_local_grant() {
        let execution_id = format!("aborted-turn-{}", Uuid::new_v4().simple());
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test(&execution_id))
            .await;
        let held = token.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let turn = tokio::spawn(async move {
            let _lease = RevokeGrantOnDrop::new(held);
            let _ = entered_tx.send(());
            std::future::pending::<()>().await;
        });
        entered_rx.await.expect("turn entered");
        turn.abort();
        let _ = turn.await;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while plane_grant_registry()
                .resolve_run_scoped(&token)
                .await
                .is_some()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborted turn revoked grant");
    }

    fn test_grant(execution_id: &str) -> PlaneGrant {
        PlaneGrant::for_test(execution_id)
    }

    /// A delegation the harness asked for is captured on the grant and handed
    /// to the loop exactly once, the way a gated action's approval capture is.
    #[test]
    fn a_pending_delegation_is_handed_to_the_loop_once() {
        let grant = test_grant("exec-1");
        grant.set_pending_delegation(vec![
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
        ]);
        grant.set_turn_stop(PlaneTurnStopReason::Delegate);
        let taken = grant
            .take_pending_delegation()
            .expect("the captured targets come back to the loop");
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].target_agent_id, "researcher");
        assert!(grant.take_pending_delegation().is_none());
        assert_eq!(
            grant.turn_stop_reason(),
            Some(PlaneTurnStopReason::Delegate)
        );
    }

    #[test]
    fn execution_grants_preserve_the_run_catalog_and_approval_rules() {
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use crate::magician_v2::test_utils::ConfigurableMockLlm;
        let executors = Arc::new(ActionExecutors::new(
            Arc::new(ConfigurableMockLlm::with_response("{}")),
            Arc::new(PromptManager::new(Arc::new(
                JsonPromptStorage::with_default_config().unwrap(),
            ))),
        ));
        let index = Arc::new(super::super::catalog::test_index_with(&["read_file"]));
        let mut ctx = AgenticContext::default();
        ctx.tool_index = Some(index.clone());
        ctx.approval_rules = vec![ApprovalRule {
            tool: "read_file".into(),
            action: ActionPattern::Single("*".into()),
            when: None,
            ttl_secs: None,
        }];
        let expected_rules = serde_json::to_value(&ctx.approval_rules).unwrap();
        let grant = PlaneGrant::for_run(ctx, executors, "exec-catalog".into());
        assert!(Arc::ptr_eq(&grant.tool_index, &index));
        assert_eq!(
            serde_json::to_value(&grant.constraints.requires_approval).unwrap(),
            expected_rules
        );
        assert!(
            !grant.permits("write_file"),
            "a static hot name absent from the run index is not runnable"
        );
        assert!(
            !grant.allowed_tools.is_empty(),
            "an empty execution catalog cannot widen into an open plane grant"
        );
    }

    /// A run driven by a switched engine can delegate only if its grant says
    /// so: the verb has no index entry, so a run that can dispatch a
    /// delegation must admit it explicitly, and one that cannot must not.
    #[test]
    fn a_run_that_can_dispatch_a_delegation_advertises_the_verb() {
        use crate::magician_v2::execution::agentic::delegation_dispatch::{
            DelegationDispatcher, DelegationSpawnResult, DelegationTarget, DispatchError,
        };
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use crate::magician_v2::test_utils::ConfigurableMockLlm;
        #[derive(Debug)]
        struct InertDispatcher;
        #[async_trait::async_trait]
        impl DelegationDispatcher for InertDispatcher {
            async fn available_targets(
                &self,
                _source_agent_id: &str,
                _principal: Option<&str>,
                _workspace: Option<&str>,
            ) -> Vec<DelegationTarget> {
                Vec::new()
            }
            async fn spawn_children(
                &self,
                _source_agent_id: &str,
                _source_execution_id: &str,
                _source_chain_id: Option<&str>,
                _targets: Vec<crate::magician_v2::execution::actions::DelegationTargetRequest>,
                _cancel: Option<tokio_util::sync::CancellationToken>,
            ) -> Result<DelegationSpawnResult, DispatchError> {
                Err(DispatchError::ServiceUnavailable)
            }
        }
        let build = |dispatcher: Option<Arc<dyn DelegationDispatcher>>| {
            let mut executors = ActionExecutors::new(
                Arc::new(ConfigurableMockLlm::with_response("{}")),
                Arc::new(PromptManager::new(Arc::new(
                    JsonPromptStorage::with_default_config().unwrap(),
                ))),
            );
            executors.delegation_dispatcher = dispatcher;
            let mut ctx = AgenticContext::default();
            ctx.tool_index = Some(Arc::new(super::super::catalog::test_index_with(&["files"])));
            // Two independent gates: the dispatcher decides whether the run may
            // delegate at all (`permits`), the targets decide whether the verb
            // is worth advertising (`catalog::builtin_hot_names_for_grant`).
            // This run has a target either way, so the dispatcher is the only
            // variable the assertions below move.
            ctx.delegation_targets = vec![
                crate::magician_v2::execution::agentic::delegation_dispatch::DelegationTarget {
                    agent_id: "researcher".to_string(),
                    name: "Scout".to_string(),
                    aliases: Vec::new(),
                    description: "reads fixtures".to_string(),
                    tools: Vec::new(),
                    allowed_invocation_surfaces: Vec::new(),
                },
            ];
            PlaneGrant::for_run(ctx, Arc::new(executors), "exec-delegation".into())
        };
        let can = build(Some(Arc::new(InertDispatcher)));
        assert!(can.permits("delegate_to_agent"), "{:?}", can.allowed_tools);
        assert!(
            super::super::catalog::advertised_names(&can)
                .contains(&"delegate_to_agent".to_string()),
            "{:?}",
            super::super::catalog::advertised_names(&can)
        );
        let cannot = build(None);
        assert!(
            !cannot.permits("delegate_to_agent"),
            "{:?}",
            cannot.allowed_tools
        );
    }

    #[test]
    fn an_empty_run_registry_does_not_strip_the_advertised_catalog() {
        use crate::magician_v2::execution::capability::CapabilityRegistry;
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use crate::magician_v2::test_utils::ConfigurableMockLlm;
        let mut executors = ActionExecutors::new(
            Arc::new(ConfigurableMockLlm::with_response("{}")),
            Arc::new(PromptManager::new(Arc::new(
                JsonPromptStorage::with_default_config().unwrap(),
            ))),
        );
        executors.capability_registry = Some(Arc::new(CapabilityRegistry::new()));
        let index = Arc::new(super::super::catalog::test_index_with(&[
            "read_file",
            "files",
        ]));
        let mut ctx = AgenticContext::default();
        ctx.tool_index = Some(index);
        let grant = PlaneGrant::for_run(ctx, Arc::new(executors), "exec-empty-registry".into());
        assert!(
            grant.permits("files") && grant.permits("read_file"),
            "an empty registry is unknown dispatchability, not a denial: {:?}",
            grant.allowed_tools
        );
    }

    /// The catalog a run grant advertises must be dispatchable by the run's
    /// own executors. The index says what exists in the scope; the executors'
    /// effective registry — narrowed to the owner's tool scope — says what
    /// this run can actually call. A name in the index but absent from that
    /// registry is a tool the engine would call and fail with "No provider
    /// registered", which Magician's own loop avoids only by preferring
    /// `files`.
    #[test]
    fn execution_grants_advertise_only_what_their_executors_can_dispatch() {
        use crate::magician_v2::execution::capability::CapabilityRegistry;
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use crate::magician_v2::test_utils::ConfigurableMockLlm;
        let mut executors = ActionExecutors::new(
            Arc::new(ConfigurableMockLlm::with_response("{}")),
            Arc::new(PromptManager::new(Arc::new(
                JsonPromptStorage::with_default_config().unwrap(),
            ))),
        );
        #[derive(Debug)]
        struct FilesProbe;

        #[async_trait::async_trait]
        impl crate::magician_v2::execution::capability::CapabilityProvider for FilesProbe {
            fn tool_name(&self) -> &str {
                "files"
            }
            fn lower(
                &self,
                _step: &crate::magician_v2::strategy::plan::PlanStep,
            ) -> Result<
                crate::magician_v2::resource_authority::gated_action::MaybeGatedAction,
                crate::magician_v2::execution::error::ExecutionError,
            > {
                unimplemented!()
            }
            async fn execute(
                &self,
                _action: &crate::magician_v2::execution::actions::ExecutableAction,
                _session_id: Option<String>,
                _timeout_secs: u64,
            ) -> Result<
                crate::magician_v2::execution::actions::ActionResult,
                crate::magician_v2::execution::error::ExecutionError,
            > {
                unimplemented!()
            }
        }

        // The scope indexes both leaves; this run's registry serves only `files`.
        let registry = CapabilityRegistry::new();
        registry.register(Arc::new(FilesProbe));
        executors.capability_registry = Some(Arc::new(registry));
        let index = Arc::new(super::super::catalog::test_index_with(&[
            "read_file",
            "files",
        ]));
        let mut ctx = AgenticContext::default();
        ctx.tool_index = Some(index);
        let grant = PlaneGrant::for_run(ctx, Arc::new(executors), "exec-dispatchable".into());
        assert!(
            grant.permits("files"),
            "a dispatchable indexed tool is advertised"
        );
        assert!(
            !grant.permits("read_file"),
            "an indexed tool with no provider in the run's registry is not advertised"
        );
        assert!(
            grant.allowed_tools.iter().any(|name| name == "tool_search"),
            "discovery stays available"
        );
    }

    /// A skill pack registers ONE provider under its pack name (`browser`);
    /// its actions are leaf tools (`browser__open`, `browser__snapshot`). The
    /// registry's `has` is a key lookup, so filtering the catalog's leaf
    /// names through it dropped every leaf of every multi-action pack: on a
    /// switched engine `tool_search` could not load `browser__*`, and Codex
    /// reported "the governed tool catalog does not expose the browser tool"
    /// on 2026-09-20 while Magician's own loop, which does not pass through
    /// this grant, had the same tools. A leaf is dispatchable when its pack
    /// is.
    #[test]
    fn execution_grants_advertise_the_leaves_of_a_dispatchable_pack() {
        use crate::magician_v2::execution::capability::CapabilityRegistry;
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use crate::magician_v2::test_utils::ConfigurableMockLlm;
        let mut executors = ActionExecutors::new(
            Arc::new(ConfigurableMockLlm::with_response("{}")),
            Arc::new(PromptManager::new(Arc::new(
                JsonPromptStorage::with_default_config().unwrap(),
            ))),
        );
        #[derive(Debug)]
        struct BrowserPackProbe;

        #[async_trait::async_trait]
        impl crate::magician_v2::execution::capability::CapabilityProvider for BrowserPackProbe {
            fn tool_name(&self) -> &str {
                "browser"
            }
            fn lower(
                &self,
                _step: &crate::magician_v2::strategy::plan::PlanStep,
            ) -> Result<
                crate::magician_v2::resource_authority::gated_action::MaybeGatedAction,
                crate::magician_v2::execution::error::ExecutionError,
            > {
                unimplemented!()
            }
            async fn execute(
                &self,
                _action: &crate::magician_v2::execution::actions::ExecutableAction,
                _session_id: Option<String>,
                _timeout_secs: u64,
            ) -> Result<
                crate::magician_v2::execution::actions::ActionResult,
                crate::magician_v2::execution::error::ExecutionError,
            > {
                unimplemented!()
            }
        }

        let registry = CapabilityRegistry::new();
        registry.register(Arc::new(BrowserPackProbe));
        executors.capability_registry = Some(Arc::new(registry));
        let index = Arc::new(super::super::catalog::test_index_with(&[
            "browser__open",
            "browser__snapshot",
            "gmail__send",
        ]));
        let mut ctx = AgenticContext::default();
        ctx.tool_index = Some(index);
        // The catalog's deferred tier is the agent's granted packs expanded to
        // leaves; grant both packs so only the registry decides.
        for pack in ["browser", "gmail"] {
            ctx.merged_agent_tools.push(runtime_core::ToolInfo {
                name: pack.to_string(),
                description: format!("{pack} pack"),
                category: "pack".to_string(),
                categories: vec!["pack".to_string()],
                parameters: vec![],
                enhanced_description: None,
                keywords: vec![],
                use_cases: vec![],
                composition_category: None,
                providing_agent_id: None,
            });
        }
        let grant = PlaneGrant::for_run(ctx, Arc::new(executors), "exec-pack-leaves".into());
        assert!(
            grant.permits("browser__open") && grant.permits("browser__snapshot"),
            "the leaves of a pack the run's registry serves are advertised: {:?}",
            grant.allowed_tools
        );
        assert!(
            !grant.permits("gmail__send"),
            "a leaf whose pack has no provider in this run's registry is not"
        );
    }

    #[test]
    fn execution_attenuation_and_exact_tool_denials_survive_the_plane_hop() {
        let mut grant = test_grant("exec-ceiling");
        grant.ctx.plane_allowed_capability_names =
            Some(vec!["read_file".into(), "browser__click".into()]);
        assert!(grant.permits("read_file"));
        assert!(!grant.permits("write_file"));
        grant
            .ctx
            .plane_denied_capability_names
            .push("browser__click".into());
        assert!(!grant.permits("browser__click"));
        grant.ctx.denied_capability_names.push("read_file".into());
        assert!(!grant.permits("read_file"));
        grant.ctx.plane_allowed_capability_names = Some(Vec::new());
        assert!(!grant.permits("tool_search"));
    }

    #[tokio::test]
    async fn revoke_cancels_in_flight_dispatch() {
        let registry = PlaneGrantRegistry::default();
        let token = registry.mint(test_grant("exec-cancel")).await;
        let grant = registry.resolve(&token).await.expect("resolves");
        let cancel = grant.cancellation_token.clone().expect("dispatch token");
        assert!(!cancel.is_cancelled());
        registry.revoke(&token).await;
        assert!(grant.is_revoked());
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn chat_turn_process_local_resolve_includes_chat_scoped() {
        let registry = PlaneGrantRegistry::default();
        let chat_token = registry
            .mint_chat(PlaneGrant::for_conversation(
                AgenticContext::default(),
                "sess-chat".to_string(),
                CancellationToken::new(),
            ))
            .await;
        assert!(
            registry.resolve_run_scoped(&chat_token).await.is_some(),
            "a ChatScoped grant must resolve at the process-local door"
        );

        let run_token = registry.mint(test_grant("exec-local")).await;
        assert!(registry.resolve_run_scoped(&run_token).await.is_some());

        registry
            .cache_durable("plt_durable_chat_turn", test_grant("term"))
            .await;
        assert!(
            registry
                .resolve_run_scoped("plt_durable_chat_turn")
                .await
                .is_none(),
            "durable terminal grants must still revalidate against the store"
        );
        registry.revoke(&chat_token).await;
        registry.revoke(&run_token).await;
        registry
            .revoke_cached_durable("plt_durable_chat_turn")
            .await;
    }

    #[tokio::test]
    async fn chat_turn_revoke_does_not_cancel_the_parent_token() {
        let parent = CancellationToken::new();
        let grant = PlaneGrant::for_conversation(
            AgenticContext::default(),
            "sess-cancel".to_string(),
            parent.clone(),
        );
        let child = grant.cancellation_token.clone().expect("child");
        assert!(!parent.is_cancelled());
        assert!(!child.is_cancelled());

        let registry = PlaneGrantRegistry::default();
        let token = registry.mint_chat(grant).await;
        registry.revoke(&token).await;
        assert!(child.is_cancelled(), "revoke must trip the grant child");
        assert!(
            !parent.is_cancelled(),
            "revoke must not cancel the chat turn"
        );
    }

    #[test]
    fn chat_turn_grant_child_cancels_when_the_session_cancels() {
        let parent = CancellationToken::new();
        let grant = PlaneGrant::for_conversation(
            AgenticContext::default(),
            "sess-parent".to_string(),
            parent.clone(),
        );
        let child = grant.cancellation_token.clone().expect("child");
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn chat_turn_for_conversation_without_override_still_keeps_chat_surface() {
        let grant = PlaneGrant::for_conversation(
            AgenticContext::default(),
            "sess-bare".to_string(),
            CancellationToken::new(),
        );
        assert_eq!(grant.surface(), InvocationSurface::Chat);
        assert_eq!(grant.pause_disposition(), PlanePauseDisposition::Refuse);
    }

    /// A conversation grant names the chat mouth as the parent of the
    /// operations its calls reach; the native mouth names none; endpoints
    /// the context already carried survive beside it.
    #[test]
    fn a_conversation_grant_carries_the_chat_mouth_as_parent() {
        use crate::magician_v2::query_analysis::operation_llm_router::{
            OperationRoutingEndpoint, OperationRoutingOverrides,
        };

        let parent_of = |grant: &PlaneGrant| {
            grant
                .ctx
                .llm_routing_overrides
                .as_ref()
                .and_then(|overrides| overrides.parent_engine.clone())
        };

        let mut ctx = AgenticContext::default();
        ctx.harness_engine = Some("codex".to_string());
        let grant =
            PlaneGrant::for_conversation(ctx, "sess-parent".to_string(), CancellationToken::new());
        assert_eq!(parent_of(&grant).as_deref(), Some("codex"));

        let mut ctx = AgenticContext::default();
        ctx.harness_engine = Some("magician".to_string());
        let native =
            PlaneGrant::for_conversation(ctx, "sess-native".to_string(), CancellationToken::new());
        assert_eq!(parent_of(&native), None, "the native mouth is no parent");

        let mut ctx = AgenticContext::default();
        ctx.harness_engine = Some("grok".to_string());
        ctx.llm_routing_overrides = Some(OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::for_profile("eval-luna"),
            ..Default::default()
        });
        let routed =
            PlaneGrant::for_conversation(ctx, "sess-routed".to_string(), CancellationToken::new());
        let overrides = routed
            .ctx
            .llm_routing_overrides
            .as_ref()
            .expect("the carried overrides survive");
        assert_eq!(overrides.parent_engine.as_deref(), Some("grok"));
        assert_eq!(
            overrides
                .planning
                .as_ref()
                .and_then(|endpoint| endpoint.profile_name()),
            Some("eval-luna")
        );
    }

    #[tokio::test]
    async fn a_grant_resolves_to_its_run_and_dies_with_it() {
        let registry = PlaneGrantRegistry::default();
        let token = registry.mint(test_grant("exec-1")).await;
        assert!(token.starts_with("plt_"));
        assert_eq!(
            registry
                .resolve(&token)
                .await
                .expect("resolves")
                .ctx
                .execution_id
                .as_deref(),
            Some("exec-1")
        );
        let resolved_before_revoke = registry.resolve(&token).await.expect("resolves");
        registry.revoke(&token).await;
        assert!(
            registry.resolve(&token).await.is_none(),
            "a revoked grant must not act"
        );
        assert!(
            resolved_before_revoke.is_revoked(),
            "a pre-resolved clone must share registry revocation"
        );
    }

    #[tokio::test]
    async fn an_unknown_token_resolves_to_nothing() {
        assert!(PlaneGrantRegistry::default()
            .resolve("plt_nope")
            .await
            .is_none());
    }

    /// Empty must NOT mean "the whole catalog". `citizen.rs` uses empty as a
    /// sentinel for "all", but its universe is ~3 extension tools; the plane's
    /// universe is the deferred catalog reachable through `tool_search`.
    #[test]
    fn an_unscoped_grant_still_denies_the_dangerous_families() {
        let grant = test_grant("exec-1");
        assert!(
            grant.allowed_tools.is_empty(),
            "this test is about the empty case"
        );
        for denied in NEVER_ON_THE_PLANE {
            assert!(!grant.permits(denied), "unscoped grant permitted {denied}");
        }
        assert!(grant.permits("read_file"));
        assert!(grant.permits("search_memory"));
    }

    /// The surface is the authority model. A grant that mints on a default
    /// surface would make every existing agent reachable from a terminal.
    #[tokio::test]
    async fn a_grant_mints_on_the_plane_surface_and_that_surface_is_not_a_default() {
        let registry = PlaneGrantRegistry::default();
        let grant = registry
            .resolve(&registry.mint(test_grant("exec-1")).await)
            .await
            .expect("resolves");
        assert_eq!(grant.surface(), InvocationSurface::Plane);
        assert_eq!(
            grant
                .ctx
                .invocation_context_override
                .as_ref()
                .map(|invocation| invocation.surface),
            Some(InvocationSurface::Plane)
        );
        assert!(
            !InvocationSurface::Plane.is_default_direct_surface(),
            "Plane must be opt-in, or every agent is reachable from a terminal"
        );
    }

    #[tokio::test]
    async fn mint_overwrites_a_caller_supplied_surface() {
        let mut grant = test_grant("exec-1");
        grant.ctx.invocation_context_override = Some(AgentInvocationContext {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            source_agent_id: None,
            target_agent_id: "personal-assistant".to_string(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::Direct,
            chat_session_id: None,
            chat_turn_id: None,
        });
        let registry = PlaneGrantRegistry::default();
        let minted = registry
            .resolve(&registry.mint(grant).await)
            .await
            .expect("resolves");
        assert_eq!(minted.surface(), InvocationSurface::Plane);
    }

    fn bridged_spec(name: &str) -> (String, LLMToolSpec) {
        (
            name.to_string(),
            LLMToolSpec {
                name: name.to_string(),
                description: format!("{name} on the native mouth"),
                parameters: serde_json::json!({"type": "object"}),
            },
        )
    }

    fn noop_bridge() -> ChatMouthBridge {
        Arc::new(
            |_call_id: String,
             _name: String,
             _arguments: Value,
             _cancel: CancellationToken|
             -> futures_util::future::BoxFuture<'static, Value> {
                Box::pin(async { serde_json::json!({"status": "ok"}) })
            },
        )
    }

    /// An attenuated allowlist admits the bridged names (once each, so a
    /// name already allowed is not repeated), or `permits` and the catalog
    /// would disagree about them; an empty allowlist already admits every
    /// name outside the denied families and stays empty. The bridge rides
    /// every clone of the grant.
    #[test]
    fn with_bridged_tools_admits_the_names_on_an_attenuated_allowlist() {
        let specs: BTreeMap<String, LLMToolSpec> = [
            bridged_spec("create_chat_thread"),
            bridged_spec("read_file"),
        ]
        .into_iter()
        .collect();

        let mut attenuated = test_grant("exec-bridged");
        attenuated.allowed_tools = vec!["read_file".to_string()];
        let attenuated = attenuated.with_bridged_tools(specs.clone(), noop_bridge());
        assert_eq!(
            attenuated.allowed_tools,
            vec!["read_file".to_string(), "create_chat_thread".to_string()]
        );
        assert!(attenuated.permits("create_chat_thread"));
        assert_eq!(attenuated.bridged_tools.len(), 2);
        assert!(attenuated.mouth_bridge.is_some());
        assert!(
            attenuated.clone().mouth_bridge.is_some(),
            "a resolved clone must carry the bridge"
        );

        let open = test_grant("exec-bridged-open").with_bridged_tools(specs, noop_bridge());
        assert!(
            open.allowed_tools.is_empty(),
            "an empty allowlist stays open"
        );
        assert!(open.permits("create_chat_thread"));

        let bare = test_grant("exec-bare");
        assert!(bare.bridged_tools.is_empty());
        assert!(bare.mouth_bridge.is_none());
    }

    #[test]
    fn mint_invocation_ref_is_unique_per_call() {
        let a = mint_invocation_ref();
        let b = mint_invocation_ref();
        assert!(a.starts_with("pltinv_"));
        assert_ne!(a, b, "two calls must not share an invocation ref");
    }

    #[test]
    fn a_turn_tool_budget_spent_refuses_further_calls() {
        let grant = test_grant("exec-budget").with_turn_tool_budget(2, 0);
        assert!(!grant.turn_tool_call_budget_exhausted());
        assert_eq!(grant.count_turn_tool_call(), 1);
        assert!(!grant.turn_tool_call_budget_exhausted());
        assert_eq!(grant.count_turn_tool_call(), 2);
        assert!(grant.turn_tool_call_budget_exhausted());
    }

    /// The bound is per logical turn and survives the approval pause: a
    /// resumed turn inherits only the remainder, not a fresh budget.
    #[test]
    fn a_resumed_turn_inherits_only_the_remainder_of_its_bound() {
        let grant = test_grant("exec-budget-resume").with_turn_tool_budget(5, 3);
        assert_eq!(grant.turn_tool_calls_spent(), 3);
        assert!(!grant.turn_tool_call_budget_exhausted());
        assert_eq!(grant.count_turn_tool_call(), 4);
        assert!(!grant.turn_tool_call_budget_exhausted());
        assert_eq!(grant.count_turn_tool_call(), 5);
        assert!(grant.turn_tool_call_budget_exhausted());
    }

    #[test]
    fn zero_means_no_tool_call_ceiling() {
        let grant = test_grant("exec-budget-none");
        assert_eq!(grant.turn_tool_call_limit(), 0);
        for _ in 0..10 {
            grant.count_turn_tool_call();
        }
        assert!(!grant.turn_tool_call_budget_exhausted());
    }

    /// `resolve` clones the grant; a call counted through one clone must be
    /// visible through the other or the bound is decoration.
    #[test]
    fn budget_state_is_shared_across_grant_clones() {
        let grant = test_grant("exec-budget-clone").with_turn_tool_budget(1, 0);
        let clone = grant.clone();
        assert_eq!(grant.count_turn_tool_call(), 1);
        assert!(
            clone.turn_tool_call_budget_exhausted(),
            "a resolved clone must observe the same spend"
        );
    }

    /// The ledger's byte budget latches shut on the first call that does
    /// not fit: that call and every later one keep their id and name, so
    /// the call count stays honest, while their payload is the omission
    /// record; the calls before it are untouched, and a drain leaves a
    /// fresh ledger.
    #[test]
    fn ledger_budget_spent_records_omissions() {
        let record = |index: usize| PlaneTurnToolCall {
            call_id: format!("pltinv_{index}"),
            tool_name: "list_tasks".to_string(),
            arguments: serde_json::json!({"status": "open"}),
            content: "x".repeat(PLANE_TURN_RESULT_MAX_BYTES),
            is_error: true,
        };
        let fits = PLANE_TURN_LEDGER_MAX_BYTES / record(0).payload_bytes();
        let total = fits + 3;
        let mut ledger = PlaneTurnLedger::default();
        for index in 0..total {
            ledger.record(record(index));
        }
        let calls = ledger.drain();
        assert_eq!(calls.len(), total, "every call is counted");
        for (index, call) in calls.iter().enumerate() {
            assert_eq!(
                call.call_id,
                format!("pltinv_{index}"),
                "order and ids are kept"
            );
            assert_eq!(call.tool_name, "list_tasks");
        }
        let (kept, omitted) = calls.split_at(fits);
        assert!(!kept.is_empty());
        assert!(kept.iter().all(|call| {
            call.arguments == serde_json::json!({"status": "open"})
                && call.content.len() == PLANE_TURN_RESULT_MAX_BYTES
                && call.is_error
        }));
        assert_eq!(omitted.len(), 3);
        assert!(omitted.iter().all(|call| {
            call.arguments == Value::Null
                && call.content == PLANE_TURN_LEDGER_BUDGET_SPENT
                && !call.is_error
        }));
        let spent: usize = kept.iter().map(PlaneTurnToolCall::payload_bytes).sum();
        assert!(spent <= PLANE_TURN_LEDGER_MAX_BYTES);
        assert!(
            spent + record(0).payload_bytes() > PLANE_TURN_LEDGER_MAX_BYTES,
            "the first omitted call is the first that did not fit"
        );

        // A small call after the latch is omitted too: the budget does not
        // reopen for whatever still fits, so the record stays monotone.
        let mut ledger = PlaneTurnLedger::default();
        for index in 0..=fits {
            ledger.record(record(index));
        }
        ledger.record(PlaneTurnToolCall {
            content: String::new(),
            ..record(fits + 1)
        });
        let calls = ledger.drain();
        assert_eq!(
            calls.last().map(|call| call.content.as_str()),
            Some(PLANE_TURN_LEDGER_BUDGET_SPENT)
        );
        assert!(ledger.drain().is_empty(), "the drain leaves a fresh ledger");
        ledger.record(record(0));
        assert_eq!(ledger.drain().len(), 1, "a drained ledger records again");
    }

    #[tokio::test]
    async fn durable_revalidation_replaces_authority_and_only_carries_loaded_tools() {
        let registry = PlaneGrantRegistry::default();
        let mut original = test_grant("durable-old");
        original.allowed_tools = vec!["old_tool".to_string()];
        original.replace_loaded_tools(HashSet::from(["pack__loaded".to_string()]));
        let old_clone = registry.cache_durable("plt_durable", original).await;
        old_clone.set_turn_stop(PlaneTurnStopReason::NeedsApproval);

        let mut refreshed = test_grant("durable-new");
        refreshed.allowed_tools = vec!["new_tool".to_string()];
        let current = registry.cache_durable("plt_durable", refreshed).await;

        assert!(old_clone.is_revoked());
        assert_eq!(
            current.ctx.execution_id.as_deref(),
            Some("durable-new"),
            "the cached projection must not retain old authority"
        );
        assert_eq!(current.allowed_tools, vec!["new_tool".to_string()]);
        assert_eq!(
            current.turn_stop_reason(),
            None,
            "only loaded-tool cache state may cross authority replacement"
        );
        assert_eq!(
            current.loaded_tool_names(),
            HashSet::from(["pack__loaded".to_string()])
        );
    }

    #[tokio::test]
    async fn unchanged_revalidated_durable_authority_keeps_one_live_projection() {
        let registry = PlaneGrantRegistry::default();
        let first = registry
            .cache_durable("plt_stable", test_grant("durable-stable"))
            .await;
        let second = registry
            .cache_durable("plt_stable", test_grant("durable-stable"))
            .await;
        assert!(!first.is_revoked());
        assert!(
            Arc::ptr_eq(&first.dispatch_lock, &second.dispatch_lock),
            "identical revalidation must not revoke an in-flight request"
        );
    }

    #[tokio::test]
    async fn durable_revalidation_refreshes_catalog_without_resetting_session_state() {
        let registry = PlaneGrantRegistry::default();
        let first = registry
            .cache_durable("plt_catalog", test_grant("catalog"))
            .await;
        first.replace_loaded_tools(HashSet::from(["read_file".into()]));
        let index = Arc::new(super::super::catalog::test_index_with(&["read_file"]));
        let refreshed = test_grant("catalog").with_tool_index(index.clone());
        let second = registry.cache_durable("plt_catalog", refreshed).await;
        assert!(Arc::ptr_eq(&second.tool_index, &index));
        assert!(Arc::ptr_eq(second.ctx.tool_index.as_ref().unwrap(), &index));
        assert!(Arc::ptr_eq(&first.dispatch_lock, &second.dispatch_lock));
        assert!(!first.is_revoked());
        assert_eq!(second.loaded_tool_names(), first.loaded_tool_names());
    }
}
