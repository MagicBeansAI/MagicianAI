//! `claim → load → one phase → commit → release`, so an execution survives the
//! death of the process running it.
//!
//! # The cycle, and why it is one phase and not a loop over phases
//!
//! The resident driver in `executor.rs` sequences the six phases by falling
//! through code: its continuation *is* the Rust stack of a tokio task, which is
//! precisely why a restart loses every in-flight execution. This driver holds no
//! continuation. One invocation does exactly:
//!
//! 1. **claim** — take the lease, or find somebody else holds it.
//! 2. **load** — the committed state and the revision to commit against.
//! 3. **guard** — every check the design puts at *phase entry* rather than in a
//!    spawned timer or a resident closure. See the table below.
//! 4. **run ONE phase** — through [`WorkerHost::run_phase`].
//! 5. **append + commit** — one journal append, one compare-and-swap.
//! 6. **project** — hand the events that commit made authoritative to the host's
//!    transports, then record how far ([`project_outbox`]). After the commit,
//!    never before it: a projection bounded by anything other than the committed
//!    watermark emits an attempt a stale commit is about to discard. It does
//!    nothing at all on a host that declares no transports, which is the default.
//! 7. **release**.
//!
//! Running two phases per claim would look like an optimisation and would delete
//! the property the whole refactor buys: a kill between them would discard both,
//! and the second one's effects would be re-run from a commit that never
//! recorded the first. The commit *between* phases is the product.
//!
//! # Why a host trait rather than calling the phases directly
//!
//! `run_loop::phases::{prepare,observe,decide,resolve,apply,epilogue}::run` take
//! 6, 7, 8, 9, 14 and 17 explicit bindings — `&mut AgenticContext`,
//! `&mut ExecutionHistory`, `&mut LoopProtectiveState`, `TrustDispatchGuard`,
//! and so on. [`super::state::LoopState`] is deliberately not that state: its own
//! documentation says so, and `phases/mod.rs` says why — the twenty-eight loop
//! locals have not moved yet, so *"a phase cannot take one `&LoopState`, because
//! the state it needs does not exist as one value to take"*.
//!
//! So this driver owns the **durable** half of the cycle and delegates the phase
//! body across [`WorkerHost`]. That is not a workaround for the extraction being
//! unfinished; it is where the seam has to be even after it finishes, because a
//! worker must be able to run a phase without knowing which phase's parameter
//! list it is. What this file must not do is pretend the live state is already a
//! value — a driver written against a `LoopState` it cannot actually hand to a
//! phase would compile and be untestable against the real loop.
//!
//! # Which failure-mode rows this implements
//!
//! From *Error handling* in `docs/archive/plans/2026-08-25-stateless-loop-design.md`:
//!
//! | Row | Where |
//! |---|---|
//! | Lease expires mid-phase → work discarded | the commit is the only durable write, and it is a CAS |
//! | Worker dies mid-`Decide` | Provider call may be billed twice; no fake adoption marker |
//! | Wall-clock deadline | [`advance_under_lease`], at phase entry, against the stored `deadline_at_ms` |
//! | Stale worker commits | `LoopStateStore::commit`'s CAS, surfaced as [`Refusal::StaleCommit`] |
//! | Effect result missing → `EffectIndeterminate` | [`resolve_effects`] |
//! | Phase panics repeatedly → quarantine | [`LoopState::phase_attempts`], checked at entry |
//! | Journal/snapshot divergence | [`verify_journal`], bounded by the committed watermark |
//! | (not in the table) A run past a non-resumable terminal | [`verify_journal`]'s terminal, refused as [`Advanced::RunAlreadyEnded`] |
//! | Terminal reached mid-iteration → skip `Epilogue` | [`next_cursor`] |
//! | Store unavailable → fail closed | every `StoreError` returns before a phase runs |
//! | Corrupt journal record → quarantine, retain bytes | [`verify_journal`]; this driver never deletes |
//! | Owner transition | [`apply_journaled_owner_transition`] |
//! | Parent parked with no live child | [`check_park`] |
//!
//! One row not shown in that table is required to make the first one complete:
//! accepted events are journaled rather than emitted inline. [`project_outbox`]
//! is active on the stateless production host; it runs only after the phase
//! commit makes the records authoritative. An event the producer cannot admit
//! to the outbox, plus every event on the no-drain in-process rollback arm,
//! retains inline delivery instead. A stale commit therefore cannot leak an
//! accepted event from an orphaned attempt onto a transport.
//!
//! **How many sites journal nothing at all is `phases::outbox`'s to state, not
//! this file's.** That number has been re-counted on every sweep and has gone
//! UP each time without any code regressing — it read nine here while the
//! census said ten and then twenty-one — so this line names the module and
//! not the figure. See that module's *WHAT IS NOT JOURNALLED*.
//!
//! Two rows are implemented narrower than the design's prose, and both are
//! stated at their implementation rather than left to be discovered:
//! [`next_cursor`] on what "`Terminal`/`Pause`/`Park`" actually name, and
//! [`apply_journaled_owner_transition`] on *when* a transition is applied.
//!
//! # The intent commit, and where it lives
//!
//! **This driver performs it.** The design asks for *"commit at every
//! non-`Advance` outcome, plus the intent commit inside `Execute` before its
//! effect fires"*, and the second half was unreachable until `phases::apply`
//! split: the phase gated, dispatched and settled inside one function, so there
//! was no moment at which a driver could act between the gate and the fire.
//!
//! There is now. `Apply` is [`WorkerHost::gate_apply`] →
//! [`record_batch_intents`] → [`WorkerHost::dispatch_apply`] →
//! [`record_batch_outcomes`], and the two middle steps are this file's because
//! they are the two that need a store — which `phases::apply` deliberately
//! cannot reach. The ordering is therefore enforced by *who holds the store*
//! rather than by a rule a phase has to remember, and
//! `effects::ApplyIntents` is the token that makes a dispatch un-runnable
//! without one of them having happened. A host that has not split its `Apply`
//! answers [`WorkerHost::splits_apply_gate_from_dispatch`] with `false` and is
//! routed back through [`WorkerHost::run_phase`] unchanged.
//!
//! The other half of the discipline — deciding whether anything fires *twice* —
//! is unchanged in intent and changed in what it reads. [`resolve_effects`]
//! resolves every effect this run has no answer for against the ledger and the
//! outward record before `Apply` re-runs, and refuses to advance rather than let
//! a re-run fire blind.
//!
//! **And since 2026-08-29 that resolution is acted on rather than only
//! computed.** The entry list goes onto [`PhaseEntry::effects`], which is what
//! the gate is *told*, and — as `effects::EffectPlan` — toward
//! [`WorkerHost::dispatch_apply`]. On recovery the driver refreshes that plan
//! after intent re-recording, because the write can re-arm `NotDispatched`, fill
//! a legacy reconcile ref, or observe a terminal row; the refreshed plan is the
//! one dispatch is bound by and validates against the gated batch before its
//! first fire. A re-entered `Apply` therefore skips the members the ledger has
//! an answer for and fires only what is genuinely owed. Before it, the host
//! refused any entry carrying resolved effects, because handing the phase a
//! batch with no plan would have dispatched every member a second time.
//!
//! # `state.pending` is the complete-set marker; the ledger is the member record
//!
//! This section used to describe [`LoopState::pending`] as *the* durable trace
//! of a batch, and it was materially wrong about which failures it covers.
//! `pending` was once written only by [`commit_failed_attempt`], from
//! [`PhaseFailure::pending`], so a worker death left it empty. The split driver
//! now also commits the complete gated batch after all member intents and before
//! dispatch. That one CAS is the activation marker for sequential
//! `prepared_only` row writes: a failed prefix remains inert. The marker stays
//! intact during a cold Apply → Resolve rewind and the recovery Resolve → Apply
//! boundary; otherwise the rows it activated — including rows already settled
//! inside the batch — become indistinguishable from an abandoned prefix.
//!
//! [`resolve_effects`] reads both. The batch supplies the atomic complete set and
//! covers unsplit hosts; active/legacy ledger rows cover a worker that dies
//! after the batch commit. Neither source may be dropped.
//!
//! [`PhaseReport::pending`] remains the *success*-path field and remains `None`
//! from `Apply`: a successful dispatch half has settled everything it fired, so
//! there is nothing in flight to report. [`commit_boundary`] still refuses a
//! report that tries to publish a batch at a cursor about to move away from
//! [`Phase::Apply`] rather than letting the next claim find it orphaned.

use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use async_trait::async_trait;
use chrono::Utc;

/// Re-exported rather than declared, so `driver_worker::EffectAction` — the path
/// every doc and every caller already uses — keeps resolving after the type
/// moved to `effects`. See that declaration for why it moved.
pub use super::effects::EffectAction;
use super::effects::{
    ApplyIntents, CommittedActRef, EffectDisposition, EffectId, EffectLedger, EffectLedgerEntry,
    EffectOutcome, EffectPlan, PendingBatch, PendingEffect, PlannedEffect, ReconciledEffect,
};
use super::journal::{
    apply_owner_transition, EmitRefused, EventKey, JournalAppend, JournalBody, ProjectedEventSink,
    ProjectorCursor, RecordedBoundary, RecordedEventRouting, RecordedStep, TerminalKind,
};
#[cfg(test)]
use super::journal::{replay, Journal};
use super::outcome::{BoundaryOutcome, Phase, PhaseStep};
use super::phases::apply::GatedApply;
use super::phases::outbox;
use super::state::{
    CommitPoint, IterationCheckpoint, LoopCursor, LoopState, LoopStateRefusal, RunIdentity,
    WaitReason, WorkerId,
};
use super::store::{ExecutionKey, Lease, LoopStateStore, Revision, StoreError};
use crate::magician_v2::execution::agentic::types::{AgenticContext, AgenticOutcome};
use crate::magician_v2::RuntimeTransportEvent;

// ============================================================================
// Configuration
// ============================================================================

/// What a worker needs to know that is not in the execution's own state.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// How long a claim is good for.
    ///
    /// It bounds a missing heartbeat, not a whole phase or run. While a phase is
    /// pending the driver renews at one third of this duration, so a slow
    /// provider remains live without requiring one lease long enough for the
    /// whole call. A worker that stops heartbeating becomes claimable after this
    /// interval.
    pub lease_ttl: Duration,

    /// Consecutive failures of one phase before the execution is quarantined.
    ///
    /// Counted in [`LoopState::phase_attempts`], which is committed, so the
    /// count survives the worker that incurred it. See
    /// [`Quarantine::PhaseAttemptsExhausted`] for what it can and cannot see.
    pub max_phase_attempts: u32,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            lease_ttl: Duration::from_secs(300),
            max_phase_attempts: 3,
        }
    }
}

// ============================================================================
// The seam
// ============================================================================

/// Everything a worker needs from the live runtime in order to run one phase.
///
/// # Every method here is a thing a `LoopState` cannot answer
///
/// That is the admission bar, and it is worth stating because the temptation is
/// to widen this into "the loop". The context is here because
/// [`LoopState::for_commit`] needs one and because the work-budget segment lives
/// on it; the ceiling lookup is here because a journaled owner transition
/// deliberately does **not** carry the incoming ceiling (see
/// [`apply_owner_transition`]); the two effect methods are here because both
/// read scope-rooted stores the state has no handle on.
///
/// # [`Self::reconcile_by_ref`] takes a ref, not a [`PendingEffect`]
///
/// Deliberately, and it is the only compiler-checked statement of Decision 1
/// this file can make. `PendingEffect::reconcile_ref` is
/// `Option<CommittedActRef>`, and `None` there means *this dispatch is not
/// outward* — it must never be read as *nothing left*. A method taking the whole
/// `PendingEffect` would let an implementation reach for the `None` case and
/// answer something about a send it has no record of. Taking the ref itself
/// makes the absent case unrepresentable at the call, so the driver has to
/// decide what `None` means before it can ask — and it decides
/// [`ReconciledEffect::SurfaceToUser`], in [`resolve_effects`].
///
/// The parameter is a [`CommittedActRef`] and **not** a `&str`, which is the
/// second half of the same argument. An act ref addresses
/// `outward_assertions/<principal>/<workspace>/acts/`, so a bare string is half
/// a coordinate: an implementation handed one has no choice but to rebuild the
/// root from whatever scope it happens to be resuming under, read a directory
/// the ref could never name, find nothing, and report `DidNotFire` — *positive
/// evidence that nothing was sent*, which licences a re-send of a live message.
/// `CommittedActRef` carries the scope that derived it and hands out the
/// addressable string only to a caller that can name that scope
/// ([`CommittedActRef::act_ref_in_scope`]), so this signature is what makes the
/// wrong-scope read unspellable at the one seam a stateless worker reconciles
/// through. Narrowing it back to `&str` would restore exactly that spelling.
/// What the durable coding invocation ledger says about a reattach reference.
///
/// `Settled` is deliberately distinct from `Live`: a completed invocation may
/// have been persisted before the loop recorded its effect outcome. Treating
/// both as a session handle redispatches an already-completed coding turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReattachState {
    Live { native_session_id: String },
    Settled,
    Absent,
}

/// An explicit operator answer for an effect the durable evidence could not
/// settle. Neither variant is inferred by the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectResolution {
    AdoptSucceeded,
    AuthorizeRefire,
}

/// Result of the durable operator-input fence immediately before a run-ending
/// boundary is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSteerAdmission {
    Admitted,
    ContinueToNextDecide,
}

/// Why a run-ending boundary is closing operator-steer admission.
///
/// An ordinary phase result is speculative: input accepted after its Decide
/// claim must keep the run alive for another Decide. A durable runtime closure
/// (an absolute deadline, an exhausted resource ceiling, or a lost runtime
/// scope) cannot do that without re-entering the same terminal guard, so it
/// atomically supersedes the current control generation instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSteerPolicy {
    SpeculativePhase,
    RuntimeClosure,
}

#[async_trait]
pub trait WorkerHost: Send {
    /// The live context this worker is advancing.
    fn context(&self) -> &AgenticContext;

    /// The same context, for the two budget fields the driver owns on it.
    ///
    /// Named for what the driver does with it rather than left as a general
    /// escape hatch: [`advance_under_lease`] seeds `work_budget_consumed_ms`
    /// from the loaded state and opens `work_budget_segment_started_at` at phase
    /// entry, and nothing else here writes through it.
    fn context_mut(&mut self) -> &mut AgenticContext;

    /// Whether this run's ephemeral secret scope currently holds a user-typed
    /// secret — Decision 4's input to [`LoopState::for_commit`].
    fn holds_user_typed_ephemeral_secret(&self) -> bool;

    /// The browser-transport ceiling the named agent declares.
    ///
    /// Read from the agent definition at the moment of the transition, because
    /// the journal record deliberately does not carry it: a ceiling on the
    /// record would be a second source of truth for an agent's declaration and
    /// the two would drift the first time a definition was edited between the
    /// transition and a replay.
    fn browser_ceiling_for(&self, agent_id: &str) -> Vec<String>;

    /// Adopt the loaded run's identity before the phase runs.
    ///
    /// Called at **every** phase entry, not only after a transition, because a
    /// worker that did not run the previous phase has no cached anything to
    /// compare against — "did the owner change" is a question only the host can
    /// answer, and asking it unconditionally is what makes the answer reliable.
    /// Implementations must invalidate cached prompt context, tool scope and
    /// policy fingerprint when the identity differs from the one they last ran
    /// under. A host that cannot perform that complete adoption must return an
    /// error; logging and continuing under its ambient owner would execute the
    /// committed run with a different authority than the journal names.
    fn adopt_owner(&mut self, identity: &RunIdentity) -> Result<(), String>;

    /// Complete identity and authority actually in force after a phase ran.
    ///
    /// The default preserves the committed value for hosts whose identity is
    /// immutable. Hosts that can refresh policy or hand over during a phase
    /// must override this with one atomic snapshot: a same-owner refresh can
    /// change an authority ceiling without producing an owner-transition row.
    fn identity_after_phase(&self, committed: &RunIdentity) -> RunIdentity {
        committed.clone()
    }

    /// Re-derive the dispatch an effect id names, from the assistant turn it was
    /// minted from.
    ///
    /// `None` means it could not be re-derived, which is **not** a licence to
    /// fire: the driver routes that to [`Advanced::EffectIndeterminate`].
    fn rederive_dispatch(&self, effect_id: &EffectId) -> Option<RederivedDispatch>;

    /// Ask the outward record what happened, by the act ref the batch committed.
    ///
    /// The implementation is `outward_settle::reconcile_committed_effect`, whose
    /// own documentation calls it *"the entry point a stateless worker uses"*.
    /// It derives nothing, and it refuses rather than reading when the ref's
    /// committed scope is not the scope the pickup is running in.
    fn reconcile_by_ref(
        &self,
        reconcile_ref: &CommittedActRef,
        effect_id: &EffectId,
    ) -> ReconciledEffect;

    /// The durable state of a reattachable effect's named coding invocation.
    ///
    /// The second half of the reattach rule. [`PendingEffect::reattach_ref`]
    /// names the **invocation** — a coding invocation id, durable before the job
    /// starts — and this turns it into the `native_session_id` a resume actually
    /// speaks to, which is durable only once the job has run.
    ///
    /// # Why the identity is a parameter and not read off the host
    ///
    /// [`resolve_effects`] runs **before** [`Self::adopt_owner`], deliberately:
    /// an unresolved effect blocks the phase, so it is asked before the phase is
    /// entered. A host reading its own ambient identity here would therefore be
    /// reading whatever run it last ran, which for a worker picking up somebody
    /// else's execution is the wrong scope and the wrong ledger. Passing the
    /// loaded [`LoopState::identity`] makes the coordinate the run's, in the same
    /// spirit as [`CommittedActRef`] carrying the scope it was derived under.
    ///
    /// # `Absent` is *no invocation was recorded*, and it must never be widened
    ///
    /// No ledger, no entry under that id, or an entry whose session is still
    /// absent because the worker died before the engine named one. All three are
    /// indeterminate, and the driver holds the run. An implementation that
    /// answered *the latest* invocation's session instead would resume a real
    /// session belonging to a different dispatch — silently, and only when a run
    /// fired more than one coding job.
    ///
    /// The third case is narrower than it was: since 2026-08-29 the coding
    /// adapters report their session mid-turn, so the absence covers the engine
    /// handshake rather than the whole turn. ~~The worker died before the job
    /// reported.~~ Widening the answer is still forbidden for the same reason.
    fn reattach_state(&self, identity: &RunIdentity, reattach_ref: &str) -> ReattachState;

    /// A durable user/operator resolution supplied by the execution surface.
    /// The default is no answer, which keeps the effect indeterminate.
    fn effect_resolution(&self, _effect_id: &EffectId) -> Option<EffectResolution> {
        None
    }

    /// Conclude a run whose committed wall-clock deadline passed before phase
    /// entry.
    ///
    /// This is a host operation because the canonical timeout reducer owns the
    /// live environment/history snapshot and completion projection. The driver
    /// still owns publication: it commits the returned run-ending report under
    /// the current lease, so a cold expired segment cannot remain nonterminal
    /// and be offered on every later scan.
    async fn conclude_deadline<'a>(
        &mut self,
        entry: PhaseEntry<'a>,
        deadline_at_ms: i64,
    ) -> PhaseReport;

    /// Return a terminal report for a non-deadline runtime boundary that must
    /// be committed before this phase may run.
    ///
    /// Production uses this as the one phase-entry durable-control poll, for
    /// already-exhausted work/token/cost ceilings, and for a runtime scope that
    /// disappeared between phase claims. Keeping the poll inside the claimed
    /// driver prevents the outer executor from returning an outcome while the
    /// durable cursor remains runnable. The default keeps test and library hosts
    /// free of production lifecycle policy.
    async fn conclude_before_phase<'a>(
        &mut self,
        _entry: PhaseEntry<'a>,
    ) -> Result<Option<PhaseReport>, PhaseFailure> {
        Ok(None)
    }

    /// Capture the iteration-entry scalars that Epilogue must survive a process
    /// boundary with. The driver publishes this only with a successful Prepare
    /// boundary and binds it to that boundary's iteration.
    fn iteration_checkpoint(&self, iteration: usize) -> IterationCheckpoint;

    /// Capture the bounded continuation a cold holder must restore before it
    /// may reclaim this exact segment. Implementations must include the latest
    /// history/protective/context state; a freshly composed host is not a
    /// substitute for the state the prior phase committed.
    fn continuation_checkpoint(
        &self,
        iteration: usize,
    ) -> Result<super::state::IterationContinuationCheckpoint, String> {
        Err(format!(
            "worker host did not provide the durable continuation for iteration {iteration}"
        ))
    }

    /// Retire a steer receipt after the fenced boundary that consumed it is
    /// authoritative. Ordinarily the following phase performs this operation;
    /// a phase that ends the run has no follower, so [`commit_boundary`] calls
    /// this hook immediately after its successful commit.
    async fn acknowledge_operator_steers(
        &mut self,
        _receipt: &super::steer_inbox::SteerConsumeReceipt,
    ) -> Result<(), String> {
        Err("worker host cannot acknowledge a committed operator steer receipt".to_owned())
    }

    /// Prepare the bounded, integrity-sealed Artifact/runtime receipt for an
    /// exact terminal journal seq. Production task-backed hosts must refuse if
    /// they cannot produce it; detached/test hosts may return `None` because no
    /// cross-layer lifecycle exists for them.
    async fn prepare_terminal_settlement_receipt(
        &mut self,
        _key: &ExecutionKey,
        _identity: &RunIdentity,
        _segment_binding: Option<&super::state::LoopSegmentBinding>,
        _terminal: TerminalKind,
        _terminal_seq: u64,
        _outcome: &AgenticOutcome,
        _source_revision: Revision,
        _lease: &Lease,
    ) -> Result<Option<super::state::TerminalSettlementReceipt>, String> {
        Ok(None)
    }

    /// Acquire any cross-process lifecycle exclusion required before a
    /// continuation-bearing terminal is admitted. The returned owned guard is
    /// kept by the driver across operator admission, pause staging, and the
    /// fenced LoopState CAS, then dropped on every return path.
    async fn acquire_terminal_settlement_exclusion(
        &mut self,
        _identity: &RunIdentity,
        _outcome: &AgenticOutcome,
    ) -> Result<Option<Box<dyn Send>>, String> {
        Ok(None)
    }

    /// Revoke an exact pre-CAS pause generation after this worker proves its
    /// receipt did not become authoritative. Implementations must exact-match
    /// revision/body authority and leave a peer replacement untouched.
    async fn abort_terminal_settlement_receipt(
        &mut self,
        _receipt: &super::state::TerminalSettlementReceipt,
    ) -> Result<(), String> {
        Err("worker host cannot revoke an uncommitted terminal receipt".to_owned())
    }

    /// Fence durable operator admission before publishing a run-ending loop
    /// boundary. A production stateless host checks the current runtime control
    /// generation and writes its terminal tombstone under the inbox's same
    /// cross-process mutation lock. If input arrived after this phase's Decide
    /// claim, the driver commits a continuing boundary instead so that input is
    /// consumed by another Decide.
    async fn admit_terminal_operator_steers(
        &mut self,
        _phase: Phase,
        _iteration: usize,
        _receipt: Option<&super::steer_inbox::SteerConsumeReceipt>,
        _policy: TerminalSteerPolicy,
    ) -> Result<TerminalSteerAdmission, String> {
        Ok(TerminalSteerAdmission::Admitted)
    }

    /// Reopen a durable terminal tombstone when a retried phase successfully
    /// produces a nonterminal result. This is a no-op for hosts without a
    /// durable operator inbox.
    async fn reopen_operator_steers_after_nonterminal(
        &mut self,
        _phase: Phase,
        _iteration: usize,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Run exactly one phase and report what it decided.
    ///
    /// The host performs the phase's own control flow — it does not re-perform
    /// the boundary. `Err` is a phase failure and increments
    /// [`LoopState::phase_attempts`]; it carries a [`PhaseFailure`] rather than
    /// a bare error so a phase that died mid-dispatch can still say what it had
    /// committed to. See that type for why the failure path is the only one that
    /// may publish a batch.
    ///
    /// A host that answers [`Self::splits_apply_gate_from_dispatch`] with `true`
    /// is never asked to run [`Phase::Apply`] through here.
    async fn run_phase<'a>(&mut self, entry: PhaseEntry<'a>) -> Result<PhaseReport, PhaseFailure>;

    /// Whether this host can gate `Apply` without dispatching it.
    ///
    /// **`false` by default, and the default is the honest answer rather than a
    /// convenience** — the same rule [`Self::emits_projected_events`] states, for
    /// the same reason. A host that runs `Apply` in one call has no moment
    /// between the gate and the fire, so a driver that asked it to commit intents
    /// there would be committing them *after* the effects had left. Answering
    /// `false` routes `Apply` back through [`Self::run_phase`], which is exactly
    /// the pre-seam behaviour: the batch is still published on the failure path,
    /// and the per-effect ledger is still not written.
    ///
    /// # A host answering `true` owes BOTH halves
    ///
    /// [`Self::gate_apply`] and [`Self::dispatch_apply`] each default to a
    /// refusal, so a host that flips this and implements one of them fails every
    /// `Apply` — loudly, at the first dispatch of the run, rather than quietly
    /// firing with nothing written down. That is deliberate, and it is the same
    /// choice the two projection rails make: a half-implemented pair must be a
    /// failed phase attempt, never a silent degradation.
    fn splits_apply_gate_from_dispatch(&self) -> bool {
        false
    }

    /// Whether this host still holds the Resolve output for the current Apply.
    /// A cold host answers false and the driver uses the durable pre-Resolve
    /// capsule to journal a rewind before any effect is considered.
    fn has_live_apply_carry(&self) -> bool {
        false
    }

    /// Whether this host still holds Observe + Decide output for the current
    /// Resolve. A cold host with no durable checkpoint must rewind to Observe;
    /// it may not fail forward or reconstruct a browser decision from stale DOM.
    fn has_live_resolve_carry(&self) -> bool {
        false
    }

    /// Whether this host still holds the fresh observation needed by Decide.
    /// A new host answers false and the driver journals Decide back to Observe
    /// instead of deciding from values that died with the prior holder.
    fn has_live_decide_carry(&self) -> bool {
        false
    }

    /// Run `Apply` up to its gate and stop. **Nothing may fire.**
    ///
    /// The first half of the seam. `phases::apply::gate` decides everything —
    /// candidate selection, the trust boundary, the approval and confirmation
    /// gates — and admits the whole in-turn batch without dispatching any of it.
    /// The driver then has the one moment the effect ledger needs: every effect
    /// this turn will make is named, and none of them has happened.
    ///
    /// A host implementing this owes the same bookkeeping its [`Self::run_phase`]
    /// does for every other phase. The outbox drain and the owner-transition
    /// record belong to whichever half produces a [`PhaseReport`], and a
    /// [`GatedPhase::Gated`] answer produces none — so both halves have to carry
    /// them, and neither may carry them twice.
    ///
    /// `Err` carries whatever the gate admitted before it refused, on
    /// [`PhaseFailure::pending`]. Nothing fired, but the next attempt must not be
    /// told that the run committed to nothing.
    async fn gate_apply<'a>(&mut self, _entry: PhaseEntry<'a>) -> Result<GatedPhase, PhaseFailure> {
        Err(PhaseFailure {
            error: anyhow!(
                "this host declared that it splits Apply's gate from its dispatch and did not \
                 implement the gate"
            ),
            pending: None,
            records: Vec::new(),
        })
    }

    /// Fire the batch [`Self::gate_apply`] admitted.
    ///
    /// The second half. `intents` is the receipt the driver minted by writing
    /// this batch's intents to the store — or by declaring that it has no store
    /// — and it is why a host cannot reach `phases::apply::dispatch` on its own.
    ///
    /// `plan` is the other half of what only a driver can supply: what
    /// [`resolve_effects`] established, per member, before the gate ran. A host
    /// forwards it; it cannot build one, and it must not filter it. Without it
    /// a re-entered `Apply` would dispatch the whole batch a second time — which
    /// is what this build did until 2026-08-29, and why the host refused the
    /// entry outright rather than running it.
    ///
    /// `settled` is an out-parameter rather than part of the return value, and
    /// the reason is the failure path: a dispatch that errored halfway through a
    /// batch has settled some of its members, and those outcomes are exactly the
    /// ones a resuming worker needs. An `Err` return cannot carry them.
    async fn dispatch_apply(
        &mut self,
        _gated: Box<GatedApply>,
        _intents: ApplyIntents,
        _plan: EffectPlan,
        _deadline_at_ms: Option<i64>,
        _settled: &mut Vec<(EffectId, EffectOutcome)>,
    ) -> Result<PhaseReport, PhaseFailure> {
        Err(PhaseFailure {
            error: anyhow!(
                "this host declared that it splits Apply's gate from its dispatch and did not \
                 implement the dispatch"
            ),
            pending: None,
            records: Vec::new(),
        })
    }

    /// Whether this host can put a journalled event back on a transport.
    ///
    /// **`false` by default, and the default is the honest answer rather than a
    /// convenience.** A host that has no broadcaster and no canonical sink cannot
    /// emit, and [`project_outbox`] responds to `false` by not projecting at
    /// all — no journal read, no mark moved, no record marked emitted by nobody.
    /// The events stay in the log for a process that can deliver them.
    ///
    /// The alternative default — pretending a sink-less host takes every event —
    /// is the failure mode this whole ordering is built to avoid: the mark would
    /// advance past records nothing emitted, and once the mark is durable nothing
    /// ever re-offers them.
    ///
    /// A host answering `true` owes an override of
    /// [`Self::emit_projected_event_with_key`]; see that method for what happens if it
    /// does not.
    ///
    /// The stateless production host answers `true`: accepted outbox records are
    /// projector-only and it implements both routed and named delivery. The
    /// in-process rollback arm never builds this host and emits inline because
    /// it has no durable journal drain. Other hosts keep the `false` default
    /// until they provide equivalent sinks; opting in without them would stall
    /// the projector on its first undeliverable record.
    fn emits_projected_events(&self) -> bool {
        false
    }

    /// Put one projected event on this host's transports.
    ///
    /// Called only from the sink [`project_outbox`] builds, and only for a
    /// record that [`outbox::rejoin`] turned back into the exact
    /// [`RuntimeTransportEvent`] a phase originally produced. There is no second
    /// event vocabulary: `JournalBody::Event { event_type, payload }` is a
    /// byte-exact split of this type's own `#[serde(tag, content)]` encoding.
    ///
    /// # `Err` means "not now", with one deliberate exception
    ///
    /// [`super::journal::EmitRefused`]'s docs are the rule and they are strict:
    /// a refusal stops the projection walk with the mark **below** the refused
    /// record and the retry is unbounded, so refusing something a later attempt
    /// cannot accept buries every event above it for the life of the run.
    /// Refuse a transport that is down or a channel that is full; take and drop
    /// anything this build cannot map.
    ///
    /// The exception is this default. A host that answered `true` above and did
    /// not override this is a programming error no later attempt clears, and it
    /// gets the head-of-line block on purpose: the outbox stalls at the first
    /// event, loudly, with every record still in the journal. Answering `Ok(())`
    /// instead would mark each of them emitted by nobody, which loses them
    /// permanently and silently — and silence is what makes a half-implemented
    /// pair dangerous rather than merely wrong.
    ///
    /// # `routing` IS THE DECISION, AND A HOST THAT IGNORES IT DOWNGRADES SILENTLY
    ///
    /// Added 2026-08-28 as the second half of `phases::outbox`'s condition 2.
    /// Before it, [`HostEventSink`] was on [`ProjectedEventSink::emit_routed`]'s
    /// discarding default and this method took the event alone — so
    /// [`super::journal::RecordedEventRouting`] was written into every record
    /// and then dropped one call before the only party that could act on it.
    ///
    /// What a host must do with it, and why re-deriving is not the same thing:
    ///
    /// - [`RecordedEventRouting::CanonicalRuntimeFact`] — the producer decided
    ///   this event was a canonical runtime fact of the run. The carried
    ///   [`super::journal::RecordedCanonicalScope`] holds **four** fields; the
    ///   fifth, `execution_id`, must come from the RUN and **not** from
    ///   [`super::journal::EventKey::execution_id`] or from the journal's key,
    ///   both of which carry a loop-state address. See that type's *THE FILE KEY
    ///   IS NOT THE EXECUTION ID*.
    /// - [`RecordedEventRouting::TransportOnly`] — live surfaces only.
    /// - [`RecordedEventRouting::Unrecorded`] — the record does not say. A host
    ///   may fall back to whatever live rule it has, and must say out loud that
    ///   it did; it may not read this as a synonym for `TransportOnly`.
    ///
    /// **Re-deriving instead of reading is the failure this parameter exists to
    /// end, even for a host that holds the producing executors.**
    /// `ActionExecutors::emit_event` decides from `canonical_event_scope`, and
    /// [`project_outbox`] walks the whole journal from seq one — so after a
    /// crash-restart under the same key it replays a DEAD process's records
    /// through a live process's executors. If that process's scope is absent,
    /// `emit_event`'s `unwrap_or(false)` turns a recorded runtime fact into a
    /// transport-only send with nothing logged. That is the silent downgrade the
    /// record was made durable to prevent.
    fn emit_projected_event(
        &mut self,
        _event: RuntimeTransportEvent,
        _routing: &RecordedEventRouting,
    ) -> Result<(), EmitRefused> {
        Err(EmitRefused {
            reason: "this host declared it emits projected events and did not implement the emit"
                .to_string(),
        })
    }

    /// Put one projected event on this host's transports with its durable
    /// source identity intact.
    ///
    /// The default delegates to the historical key-less seam so existing test
    /// and external hosts remain source-compatible. Production hosts that
    /// project canonical facts must override this method: [`EventKey`] is the
    /// only stable identity the journal projector owns across source replay.
    fn emit_projected_event_with_key(
        &mut self,
        _key: &EventKey,
        event: RuntimeTransportEvent,
        routing: &RecordedEventRouting,
    ) -> Result<(), EmitRefused> {
        self.emit_projected_event(event, routing)
    }

    /// Await durability receipts accumulated by the preceding synchronous
    /// projection walk.
    ///
    /// The default is a no-op for compatibility with transport-only and test
    /// hosts. A host that admits canonical events asynchronously must override
    /// it and include all accepted canonical appends (and their post-persistence
    /// observers) before returning success. [`project_outbox`] never saves its
    /// cursor after an error, preserving source and transport at-least-once
    /// replay.
    async fn await_projected_event_durability(&mut self) -> Result<(), EmitRefused> {
        Ok(())
    }

    /// Put one projected **named** event on this host's transports.
    ///
    /// The five arguments are `RuntimeTransportBroadcaster::emit_named`'s five
    /// arguments, carried by [`super::journal::JournalBody::NamedEvent`]. A host
    /// implementing this should call that method with them and nothing else: the
    /// record exists precisely because the rail's delivery — the `timestamp_ms`
    /// stamp and the chat fan-out — happens *inside* `emit_named`, so a host
    /// that rebuilt the envelope and reached for `emit_agent_transport_event`
    /// would deliver one envelope where the producer delivered `1 + n`. See that
    /// variant's docs.
    ///
    /// # The same default, for the same reason, and it is deliberate that it is
    /// the same
    ///
    /// [`Self::emit_projected_event`]'s doc carries the argument in full and it
    /// applies here unchanged: a host that answered `emits_projected_events()`
    /// with `true` and did not override this is a programming error no later
    /// attempt clears, so it gets the head-of-line block on purpose — the outbox
    /// stalls at the first named event, loudly, with every record still in the
    /// journal. `Ok(())` would mark each of them emitted by nobody.
    ///
    /// A host that overrides one of these two and not the other is the case
    /// worth naming, because it compiles: it delivers one rail and stalls the
    /// projection at the first record of the other, and every record above that
    /// one waits behind it. Overriding [`Self::emits_projected_events`] is the
    /// commitment to overriding **both**.
    fn emit_projected_named_event(
        &mut self,
        _name: &str,
        _agent_id: &str,
        _principal: Option<&str>,
        _workspace: Option<&str>,
        _payload: serde_json::Value,
    ) -> Result<(), EmitRefused> {
        Err(EmitRefused {
            reason: "this host declared it emits projected events and did not implement the named \
                     emit"
                .to_string(),
        })
    }
}

/// What [`WorkerHost::gate_apply`] answered.
///
/// The driver-side spelling of `phases::apply::ApplyGate`, and it is a second
/// type rather than the same one because the two speak different vocabularies:
/// the phase answers in [`PhaseStep`], and a host has already turned that into
/// the [`PhaseReport`] this driver commits.
pub enum GatedPhase {
    /// `Apply` decided without gating anything — seven of the eight decision
    /// arms, and every `Execute` path that ends before its first dispatch. There
    /// is nothing to commit an intent for and nothing left to fire.
    Settled(PhaseReport),
    /// A batch is gated and **nothing has fired**. The driver owes it an intent
    /// commit, and then owes it a [`WorkerHost::dispatch_apply`].
    ///
    /// Dropping this value without dispatching is not a leak of anything live —
    /// no effect has happened — but it does abandon a turn mid-phase, so the
    /// only place it happens is a store failure between the two halves, which
    /// returns [`Refusal::Store`] and leaves the cursor where it was.
    Gated(Box<GatedApply>),
}

/// A dispatch re-derived from the conversation, for the re-fire gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RederivedDispatch {
    pub tool: String,
    pub arguments_fingerprint: String,
}

/// What the driver hands a phase at entry.
#[derive(Debug)]
pub struct PhaseEntry<'a> {
    pub phase: Phase,
    /// The committed cursor's iteration. Present separately because the phase
    /// stamps it onto everything it emits and reading it off `state.cursor`
    /// twice is how the two drift.
    pub iteration: usize,
    pub state: &'a LoopState,
    /// Present only at [`Phase::Apply`] with a committed batch: what the driver
    /// has established about each member, before the phase touches any of them.
    ///
    /// Empty is not "nothing in flight" — it is "no committed batch". A member
    /// the driver could not resolve never reaches here, because the whole
    /// boundary refuses first.
    pub effects: Vec<ResolvedEffect>,
}

/// What a phase produced, in the vocabulary the durable layer already speaks.
///
/// No `Debug`: [`PhaseStep`] deliberately derives nothing, because its `Return`
/// arm boxes an `AgenticOutcome` whose layout is the reason the box is there.
pub struct PhaseReport {
    /// What the phase decided. The driver turns this into the phase-completion
    /// record and the next cursor; the host never writes either.
    pub step: PhaseStep<()>,

    /// Everything else the phase wants journaled — events, owner transitions.
    ///
    /// **Not** the phase-completion record: the driver appends that itself from
    /// [`Self::step`], so a report cannot describe one thing and record another.
    /// The driver re-stamps `iteration`, `phase` and `ordinal` on these, so a
    /// host cannot collide with the completion record's address — which means
    /// the host must emit them in a deterministic order, since that order is
    /// what the projector dedupes on after a re-run.
    pub records: Vec<JournalAppend>,

    /// The batch this phase left **in flight**, or `None` when nothing is.
    /// This normally replaces the loaded state, so `None` is how a batch the
    /// phase has now settled gets cleared — which is what every successful exit
    /// from [`Phase::Apply`] reports. The one exception is recovery Resolve:
    /// its loaded batch is the complete-set activation marker, not work Resolve
    /// produced, and [`commit_boundary`] retains it across Resolve → Apply.
    ///
    /// This is **not** where a dispatch that died mid-fire is recorded. That is
    /// [`PhaseFailure::pending`], and the difference is the cursor: this report
    /// is committed together with the cursor the boundary moves *to*, and only
    /// [`Phase::Apply`] consults a batch. [`commit_boundary`] refuses a `Some`
    /// here whose next cursor is not `Apply`, rather than committing a batch the
    /// following claim would have to quarantine.
    pub pending: Option<PendingBatch>,

    /// Pre-Resolve recovery capsule. This replaces the committed value just
    /// like `pending`: Decide publishes it only when every input is durable and
    /// authority-bound, a continuing Resolve retains it, and Apply clears it
    /// only after the phase has settled. `None` at Resolve is deliberately not
    /// cold-recoverable; it keeps token/ephemeral-backed live carry on its host.
    pub resolve_checkpoint: Option<super::state::ResolveCheckpoint>,

    /// Operator-steer batch consumed by this successful Decide boundary. It is
    /// committed atomically with the cursor; a following phase acknowledges it
    /// before work and reports `None` to clear the receipt.
    pub steer_consume_receipt: Option<super::steer_inbox::SteerConsumeReceipt>,

    /// How the terminal fence treats input that was accepted after this phase's
    /// Decide claim. Nonterminal reports leave this at the speculative default;
    /// it is read only when [`Self::step`] ends the run.
    pub terminal_steer_policy: TerminalSteerPolicy,

    /// Why the run is parked, if it parked.
    pub wait: Option<WaitReason>,
}

impl PhaseReport {
    /// The ordinary report: the phase produced its output and nothing else.
    pub fn continued() -> Self {
        Self {
            step: PhaseStep::Continue(()),
            records: Vec::new(),
            pending: None,
            resolve_checkpoint: None,
            steer_consume_receipt: None,
            terminal_steer_policy: TerminalSteerPolicy::SpeculativePhase,
            wait: None,
        }
    }

    /// The phase ended the iteration.
    pub fn exits(boundary: super::outcome::BoundaryOutcome) -> Self {
        Self {
            step: PhaseStep::Exit(boundary),
            ..Self::continued()
        }
    }

    /// The phase ended the run.
    pub fn ends_run(outcome: AgenticOutcome) -> Self {
        Self {
            step: PhaseStep::Return(Box::new(outcome)),
            ..Self::continued()
        }
    }

    /// The runtime itself is closing, so there can be no later Decide that
    /// consumes input accepted in the current control generation.
    pub fn ends_run_for_runtime_closure(outcome: AgenticOutcome) -> Self {
        Self {
            terminal_steer_policy: TerminalSteerPolicy::RuntimeClosure,
            ..Self::ends_run(outcome)
        }
    }
}

/// A phase that failed, and whatever it had already committed to when it did.
///
/// # Why the error carries a batch at all
///
/// A phase that errors *after* a dispatch has been gated — a transport that
/// panicked its way to an `anyhow`, a settle that could not write — has left a
/// live effect behind. An error type that carried only the message would drop
/// the record of what that attempt had committed to, and the next worker would
/// re-run [`Phase::Apply`] with an empty `pending`.
///
/// So the failure path is the *only* path on which a `PendingBatch` reaches
/// [`LoopState::pending`] under this driver, and it is the only placement that
/// would be correct: [`commit_failed_attempt`] leaves [`LoopState::cursor`]
/// where it was, so a batch committed here sits at `Apply`. A batch published on
/// the success path would land at the cursor the boundary *moves to* —
/// `Epilogue` — and [`Quarantine::OrphanedBatch`] would hold the run on the next
/// claim. On the success path there is nothing in flight to report anyway:
/// `phases::apply::dispatch` returns only after every member it fired has
/// settled, and `pending` means *in flight*.
///
/// # THIS IS NOT THE FIRE-BLIND CASE, and it never was
///
/// **Corrected 2026-08-29.** This block used to open *"this is the fire-blind
/// case, and it is the one the effect ledger exists for"*, and that sentence
/// checked the wrong population. Reaching here requires a phase to **return
/// `Err`** — an orderly failure, with a worker still alive to report it. The
/// case the ledger exists for is the worker that *does not* return: a panic, a
/// killed process, an expired lease. None of those runs
/// [`commit_failed_attempt`], so none of them writes a batch, and for the whole
/// life of that sentence the ledger's headline case left `pending: None` behind.
///
/// What actually covers it is the intent commit — [`record_batch_intents`],
/// written before the fire rather than after the failure — and
/// [`resolve_effects`] reading `EffectLedger::unsettled` rather than this field.
/// This value is a useful record of what one orderly failure had committed to.
/// It is not the mechanism, and describing it as one told a reader the problem
/// was already solved.
///
/// `None` means *this phase has nothing to say about a batch*, never *clear the
/// one that is there* — [`commit_failed_attempt`] leaves the committed batch
/// untouched for a `None`, because a phase that failed before it gated anything
/// has not learned that an earlier attempt's batch is finished.
pub struct PhaseFailure {
    /// What the phase reported going wrong. Its chain does not survive the
    /// commit — [`commit_failed_attempt`] keeps `to_string()` — so anything a
    /// reader needs must be in the message.
    pub error: anyhow::Error,
    /// What the phase had committed to when it failed.
    pub pending: Option<PendingBatch>,
    /// Events and owner transitions produced by durable effects before the
    /// failure. They are committed without a phase-completion record, so the
    /// cursor stays put while the projector can still publish the successful
    /// prefix. A retry addresses equivalent records with the same ordinals and
    /// the projector deduplicates them.
    pub records: Vec<JournalAppend>,
}

/// So every phase arm in a host can keep using `?` on ordinary `anyhow` errors.
///
/// The conversion answers `pending: None` and no records, which is the correct
/// default for a failure with no explicitly captured durable prefix. A host
/// that wants either published has to build the failure by hand, which is the
/// point: reporting committed work is a decision, and a decision that happens
/// by `?` is not one.
impl From<anyhow::Error> for PhaseFailure {
    fn from(error: anyhow::Error) -> Self {
        Self {
            error,
            pending: None,
            records: Vec::new(),
        }
    }
}

/// What the driver established about one committed effect before `Apply` re-ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEffect {
    pub effect_id: EffectId,
    pub action: EffectAction,
}

impl ResolvedEffect {
    /// The same fact, in the shape a phase reads it in.
    ///
    /// One conversion rather than a second struct literal at the call site: the
    /// gate and the dispatch have to be told the same thing about the same
    /// members, and two constructions of "the plan" is how they come to differ.
    fn planned(&self) -> PlannedEffect {
        PlannedEffect {
            effect_id: self.effect_id.clone(),
            action: self.action.clone(),
        }
    }
}

// ============================================================================
// What one invocation answers
// ============================================================================

/// The result of one `claim → load → one phase → commit → release` cycle.
///
/// Every variant is an outcome an operator or a scheduler acts on, which is why
/// there is no `Result` wrapper: a store that cannot answer is not an error the
/// caller may retry blindly, it is a decision not to advance, and it has to read
/// as one.
#[derive(Debug)]
pub enum Advanced {
    /// One phase ran and its boundary committed.
    Committed {
        revision: Revision,
        cursor: LoopCursor,
        step: RecordedStep,
    },
    /// No phase ran. A cold Apply cursor was journaled back to Resolve so the
    /// versioned checkpoint can reconstruct the iteration on the next claim.
    RecoveryRewound {
        revision: Revision,
        cursor: LoopCursor,
    },
    /// One phase ran and ended the run. The epilogue did not run.
    RunEnded {
        revision: Revision,
        cursor: LoopCursor,
        outcome: Box<AgenticOutcome>,
        terminal: TerminalKind,
    },
    /// A park was satisfied and cleared. No phase ran; the next claim runs one.
    LeftPark {
        revision: Revision,
        resolutions: usize,
    },
    /// Nothing is committed under this key.
    NothingToAdvance,
    /// Another worker holds the lease.
    LeaseHeld { by: WorkerId, until_ms: i64 },
    /// The run is pinned to a worker that is not this one — Decision 4.
    NotClaimable {
        pinned_to: WorkerId,
        pinned_until_ms: i64,
    },
    /// Parked, with no resolution outstanding.
    Parked { wait: WaitReason },
    /// A committed retry delay has not elapsed. The worker requeues rather than
    /// sleeping: sleeping holds a worker for the whole delay, which is the
    /// resident-task cost this design exists to remove.
    NotYetRunnable { runnable_at_ms: i64 },
    /// The run's journal already records a terminal it cannot come back from.
    ///
    /// A **resumable** terminal — a pause, a confirmation, a park — is not this:
    /// those end the invocation and leave the execution alive for something
    /// outside to answer, and are picked up by re-running the phase that ended.
    /// A `Failed`, a `Success`, an exhausted budget is over. Advancing one would
    /// append past its own terminal record, and `journal::replay_each` refuses
    /// that on the next read — so the run would be advanced once and then become
    /// permanently unreadable.
    RunAlreadyEnded { terminal: TerminalKind },
    /// A receipt-backed ending is committed and the host-free lifecycle still
    /// owns runtime/Artifact settlement and (for HITL) pause publication. The
    /// source segment must not rerun while that receipt remains on this exact
    /// revision, even when the terminal kind is resumable.
    TerminalSettlementPending {
        terminal: TerminalKind,
        revision: Revision,
    },
    /// The cursor is past the run's iteration ceiling.
    IterationCeiling {
        iteration: usize,
        max_iterations: usize,
    },
    /// An effect in a committed batch has no result and no licence to fire.
    /// Never a silent re-fire.
    EffectIndeterminate { effect_id: EffectId, reason: String },
    /// A re-derived dispatch did not reproduce what the run committed. The
    /// execution fails rather than firing something it cannot vouch for.
    ExecutionFailed { reason: String },
    /// The phase returned an error. The attempt is counted and committed.
    PhaseFailed {
        phase: Phase,
        attempts: u32,
        detail: String,
    },
    /// The execution is held for an operator. Nothing is deleted.
    Quarantined(Quarantine),
    /// The boundary refused. Nothing was committed.
    Refused(Refusal),
}

/// Why an execution is held rather than advanced.
///
/// **Nothing here deletes anything.** Retention for operator recovery is by not
/// writing: the driver refuses before it appends, so the bytes an operator needs
/// are exactly the bytes that are already there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quarantine {
    /// The committed state could not be read back.
    UnreadableState { detail: String },
    /// A journal record is corrupt, or the log's numbering is broken.
    CorruptJournal { detail: String },
    /// The committed cursor is not the cursor its own journal replays to.
    ///
    /// The snapshot is a cache and the journal is the record, so a disagreement
    /// is not repairable by preferring one — it means a writer produced a pair
    /// that no correct driver produces.
    JournalDiverged {
        committed: LoopCursor,
        replayed: LoopCursor,
        watermark: u64,
    },
    /// One phase failed [`WorkerConfig::max_phase_attempts`] times in a row.
    ///
    /// Counts failures a worker **survived to record**. A hard process kill
    /// mid-phase is not counted here and does not need to be: it leaves the
    /// lease to expire, which is a different backstop with a different owner.
    PhaseAttemptsExhausted { phase: Phase, attempts: u32 },
    /// A batch sits at — or is being published to — a cursor that never consults
    /// one.
    ///
    /// Only [`Phase::Apply`] reads [`LoopState::pending`], so a batch committed
    /// anywhere else would be dropped by the next boundary — silently, and with
    /// every member's fate unresolved. That is the exact shape of the failure
    /// the effect ledger exists to prevent, so it is held rather than tidied
    /// away.
    ///
    /// Raised from two places, and `phase` means the same thing in both: the
    /// cursor that would have to read the batch and does not.
    /// [`resolve_effects`] raises it for a batch already on disk;
    /// [`commit_boundary`] raises it for a [`PhaseReport::pending`] whose next
    /// cursor is not `Apply`, before anything is appended.
    OrphanedBatch { phase: Phase, effects: usize },
}

/// Why a boundary refused to publish.
#[derive(Debug)]
pub enum Refusal {
    /// The store could not answer. Fail closed: refuse to advance rather than
    /// run unrecorded work.
    Store {
        during: &'static str,
        error: StoreError,
    },
    /// [`LoopState::for_commit`] refused the projection.
    ///
    /// Three causes, and the third is the one that names a bug in a driver
    /// rather than in a run:
    ///
    /// - the `Portable ⇒ cdp-only` placement invariant,
    /// - a pin to a worker that does not hold the run's secrets,
    /// - `WorkBudgetWentBackwards` — a commit publishing *less* budget than the
    ///   loaded state already carries. Active time does not un-elapse, so that
    ///   is the signature of a driver that did not seed `ctx` from the state it
    ///   loaded. Both `project` call sites here seed, which is why it does not
    ///   fire on this driver — and which is a property a refactor must keep.
    Projection(LoopStateRefusal),
    /// Somebody else committed in between. This worker's phase is discarded;
    /// its journal records are orphans the next attempt sweeps.
    StaleCommit { expected: Revision, found: Revision },
    /// A park on children with no live child — a hang with nothing to wake it.
    ParkWithNoLiveChild,
    /// A phase claimed a boundary exit from the epilogue, which is where exits
    /// land rather than a place they come from.
    ExitFromTheEpilogue,
}

// ============================================================================
// The cycle
// ============================================================================

/// Advance one execution by exactly one phase.
///
/// The entry point the strangler flag calls when
/// `MAGICIAN_EXECUTION_DRIVER=stateless`. It returns rather than looping, so the
/// caller decides whether to take another claim, requeue, or move on — which is
/// what makes a worker short-lived rather than resident.
pub async fn advance_once(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    worker: &WorkerId,
    config: &WorkerConfig,
) -> Advanced {
    let mut lease = match store.claim(key, worker, config.lease_ttl).await {
        Ok(lease) => lease,
        Err(StoreError::LeaseHeld { by, until_ms }) => return Advanced::LeaseHeld { by, until_ms },
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "claim",
                error,
            })
        },
    };

    let advanced = advance_under_lease(store, host, key, worker, config, &mut lease).await;

    // A release that fails leaves the lease to expire, which is the same
    // position a dead worker leaves it in and is already handled. It is logged
    // rather than folded into the outcome because it changes nothing the caller
    // would do differently.
    if let Err(error) = store.release(lease).await {
        tracing::warn!(
            execution = %key,
            worker = %worker,
            error = %error,
            "[LOOP_WORKER] releasing the lease failed; it will expire instead"
        );
    }
    advanced
}

/// The cycle, minus the lease bracket.
///
/// Split out so every early return releases: a guard that refused without
/// releasing would hold the execution for a whole TTL for a condition another
/// worker would hit just as fast.
async fn await_with_lease_heartbeat<T, F>(
    store: &dyn LoopStateStore,
    lease: &mut Lease,
    ttl: Duration,
    future: F,
) -> Result<T, StoreError>
where
    F: Future<Output = T>,
{
    let period = (ttl / 3).max(Duration::from_millis(100));
    let first = tokio::time::Instant::now() + period;
    let mut heartbeat = tokio::time::interval_at(first, period);
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Ok(output),
            _ = heartbeat.tick() => {
                *lease = store.renew(lease, ttl).await?;
            }
        }
    }
}

async fn advance_under_lease(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    worker: &WorkerId,
    config: &WorkerConfig,
    lease: &mut Lease,
) -> Advanced {
    // Phase timing (`[LOOP-TIMING]`): one line per advanced phase with the
    // load / pre-phase / phase / commit split, so a slow iteration can be
    // attributed to the loop's own durable work rather than guessed at.
    // The first live measurement (2026-09-20, run 10) put ~3.7 s of
    // machinery around a 0.3 s browser click; this is what names it.
    let timing_started = Instant::now();
    // ── LOAD ────────────────────────────────────────────────────────────────
    let committed = match store.load(key).await {
        Ok(Some(committed)) => committed,
        Ok(None) => return Advanced::NothingToAdvance,
        Err(StoreError::Corrupt { detail, .. }) => {
            return Advanced::Quarantined(Quarantine::UnreadableState { detail })
        },
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "load",
                error,
            })
        },
    };
    let mut revision = committed.revision;
    let mut state = committed.state;

    // One clock for the whole cycle, read once.
    //
    // Every decision below that involves time — whether this worker may claim
    // the run, whether its deadline has passed, whether its retry delay has
    // elapsed — is taken against this single value. Reading the clock per check
    // would let a claim decision and a deadline decision disagree about what
    // time it is, which is a disagreement no log would ever show.
    let now_ms = Utc::now().timestamp_millis();

    // ── PLACEMENT ───────────────────────────────────────────────────────────
    // `claim` does not consult the placement — it cannot, because the placement
    // lives in the state the claim is taken in order to read. So the check is
    // here, after the claim and before any work, and the lease is given straight
    // back. Decision 4: a run holding a user-typed ephemeral secret is `Pinned`,
    // and the pin means the secrets are on one machine.
    //
    // `Placement::Pinned` carries an expiry, because a pin to a worker that never
    // returns is a run nothing ever sweeps — `list_runnable` filters it out
    // forever and `deadline_at_ms` cannot rescue it, since that deadline is
    // checked at a phase entry no worker reaches. Past the expiry the run is
    // claimable again, so `claimable_by` takes the clock: an expired pin cannot
    // be honoured by a caller who simply forgot to check for expiry.
    //
    // The clock passed is `now_ms` — the same one the deadline and retry checks
    // below use. Reading a second one here would let this worker decide it may
    // claim the run at one instant and decide the run is past its deadline at
    // another, and nothing downstream would ever show the disagreement.
    //
    // The `..` in the pattern is deliberate for the same reason: this code cares
    // that the run is pinned and to whom, not about the variant's exact field
    // list.
    if !state.claimable_by(worker, now_ms) {
        if let super::state::Placement::Pinned {
            worker: owner,
            pinned_until_ms,
        } = &state.placement
        {
            return Advanced::NotClaimable {
                pinned_to: owner.clone(),
                pinned_until_ms: *pinned_until_ms,
            };
        }
        // Unreachable: `claimable_by` is true for every `Portable` state. Kept
        // as a refusal rather than an `unreachable!` because the cost of being
        // wrong is a panic in a worker holding a lease.
        return Advanced::NotClaimable {
            pinned_to: worker.clone(),
            pinned_until_ms: now_ms,
        };
    }

    // ── JOURNAL ─────────────────────────────────────────────────────────────
    // Before any work runs, not after: "refuse to advance rather than run
    // unrecorded work" is only true if the check that the log is writable
    // happens first. A corrupt journal found at append time has already paid for
    // a `Decide`.
    let recorded_terminal = match verify_journal(store, key, &state).await {
        Ok(terminal) => terminal,
        Err(quarantine) => return quarantine,
    };

    // A run that ended for good is not advanced again. Not a nicety: the next
    // append would land after its own terminal record, and `replay_each` refuses
    // a record after a non-resumable terminal — so the run would advance once
    // and then be permanently unloadable, with the failure surfacing at a read
    // nowhere near the claim that caused it.
    //
    // Legacy resumable terminals fall through deliberately. Receipt-backed
    // pauses are instead held for the settlement lifecycle below: their exact
    // continuation authority must be projected before any phase can run again.
    if let Some(terminal) = recorded_terminal {
        if let Some(receipt) = state.terminal_settlement_receipt.as_ref() {
            if receipt.descriptor.terminal_kind != terminal.settlement_label()
                || receipt.descriptor.terminal_seq != state.journal_seq
                || receipt.descriptor.exact_segment_id != key.execution_id()
            {
                return Advanced::ExecutionFailed {
                    reason: format!(
                        "committed terminal receipt does not bind replayed {} at seq {}",
                        terminal.settlement_label(),
                        state.journal_seq
                    ),
                };
            }
            // Receipt-backed resumable endings are held exactly like final
            // endings. Their staged continuation and cross-layer projection
            // belong to the terminal lifecycle; rerunning the ending phase on
            // restart could duplicate effects before the user ever responds.
            return Advanced::TerminalSettlementPending { terminal, revision };
        }
        if !terminal.is_resumable() {
            // Ending the run does not retire its durable outbox debt. A crash,
            // transient journal/cursor read failure, or sink refusal after the
            // terminal commit leaves the mark below the ending batch. The
            // dedicated terminal-outbox lifecycle re-offers that exact debt; a
            // host that encounters the terminal directly may also advance it
            // here without appending past the ending.
            if let Err(reason) = host.adopt_owner(&state.identity) {
                return Advanced::ExecutionFailed {
                    reason: format!(
                        "committed owner could not be adopted for terminal outbox recovery: \
                         {reason}"
                    ),
                };
            }
            if state.terminal_settlement_receipt.is_none() {
                project_outbox(store, host, key, lease, state.journal_seq).await;
            }
            return Advanced::RunAlreadyEnded { terminal };
        }
    }

    // ── PARK ────────────────────────────────────────────────────────────────
    if let Some(wait) = state.wait.clone() {
        return leave_park(
            store, host, key, worker, lease, &mut state, revision, wait, now_ms,
        )
        .await;
    }

    // ── DEADLINE ────────────────────────────────────────────────────────────
    // At phase entry, against the stored value. The watchdog this replaces was a
    // `tokio::spawn` racing a finished-signal, which died with its worker and so
    // would silently stop enforcing under a stateless driver.
    if state.deadline_passed(now_ms) {
        let deadline_at_ms = state.deadline_at_ms.unwrap_or(now_ms);
        // A timeout is not evidence about an outward effect. Resolve the
        // committed batch before terminal publication so an indeterminate send
        // still reaches its operator decision instead of being buried by the
        // deadline. A safe-to-refire member is deliberately not dispatched:
        // once the ceiling passed, retirement wins over starting new effects.
        if let Err(advanced) = resolve_effects(store, host, key, &state, lease, now_ms).await {
            return advanced;
        }
        host.context_mut().work_budget_consumed_ms = state.work_budget_consumed_ms;
        host.context_mut().work_budget_segment_started_at = None;
        if let Err(reason) = host.adopt_owner(&state.identity) {
            return Advanced::ExecutionFailed {
                reason: format!(
                    "committed owner could not be adopted for deadline settlement: {reason}"
                ),
            };
        }
        let phase = state.cursor.phase;
        let iteration = state.cursor.iteration;
        let mut report = match await_with_lease_heartbeat(
            store,
            lease,
            config.lease_ttl,
            host.conclude_deadline(
                PhaseEntry {
                    phase,
                    iteration,
                    state: &state,
                    effects: Vec::new(),
                },
                deadline_at_ms,
            ),
        )
        .await
        {
            Ok(report) => report,
            Err(error) => {
                return Advanced::Refused(Refusal::Store {
                    during: "lease_heartbeat_during_deadline_settlement",
                    error,
                })
            },
        };
        if !matches!(&report.step, PhaseStep::Return(_)) {
            return Advanced::ExecutionFailed {
                reason: "the deadline reducer returned a nonterminal report for an expired run"
                    .to_owned(),
            };
        }
        match (
            state.steer_consume_receipt.as_ref(),
            report.steer_consume_receipt.as_ref(),
        ) {
            (Some(committed), Some(reported)) if committed != reported => {
                return Advanced::ExecutionFailed {
                    reason: "the deadline reducer replaced the committed operator-steer receipt \
                             before its terminal boundary acknowledged it"
                        .to_owned(),
                }
            },
            (Some(committed), None) => {
                // A deadline wins before ordinary phase entry, so there is no
                // following phase to acknowledge the preceding Decide receipt.
                // Carry it through this terminal commit: terminal admission
                // sees the exact consumed batch, and `commit_boundary`
                // acknowledges it only after the terminal is durable.
                report.steer_consume_receipt = Some(committed.clone());
            },
            (Some(_), Some(_)) | (None, Some(_)) | (None, None) => {},
        }
        // A deadline is a durable runtime closure, not a speculative phase
        // result. Close the steer epoch before publishing the terminal just as
        // the ordinary phase path does, but supersede rather than asking for a
        // Decide that can never run: the next claim would hit this same expired
        // deadline first and livelock forever.
        report.terminal_steer_policy = TerminalSteerPolicy::RuntimeClosure;
        let _terminal_settlement_exclusion = match &report.step {
            PhaseStep::Return(outcome) => match await_with_lease_heartbeat(
                store,
                lease,
                config.lease_ttl,
                host.acquire_terminal_settlement_exclusion(&state.identity, outcome.as_ref()),
            )
            .await
            {
                Ok(Ok(guard)) => guard,
                Ok(Err(error)) => {
                    return Advanced::ExecutionFailed {
                        reason: format!(
                            "durable deadline terminal lifecycle exclusion failed: {error}"
                        ),
                    }
                },
                Err(error) => {
                    return Advanced::Refused(Refusal::Store {
                        during: "lease_heartbeat_during_deadline_terminal_lifecycle_exclusion",
                        error,
                    })
                },
            },
            _ => None,
        };
        let admission = match await_with_lease_heartbeat(
            store,
            lease,
            config.lease_ttl,
            host.admit_terminal_operator_steers(
                phase,
                iteration,
                report.steer_consume_receipt.as_ref(),
                report.terminal_steer_policy,
            ),
        )
        .await
        {
            Ok(Ok(admission)) => admission,
            Ok(Err(error)) => {
                return Advanced::ExecutionFailed {
                    reason: format!("durable operator-steer deadline admission failed: {error}"),
                }
            },
            Err(error) => {
                return Advanced::Refused(Refusal::Store {
                    during: "lease_heartbeat_during_deadline_steer_admission",
                    error,
                })
            },
        };
        if admission != TerminalSteerAdmission::Admitted {
            return Advanced::ExecutionFailed {
                reason: "the durable deadline fence attempted to continue to a Decide after the \
                         absolute deadline had already passed"
                    .to_owned(),
            };
        }
        return commit_boundary(
            store,
            host,
            key,
            worker,
            config,
            state,
            revision,
            lease,
            phase,
            iteration,
            report,
            Utc::now().timestamp_millis(),
        )
        .await;
    }

    // ── RETRY TIME ──────────────────────────────────────────────────────────
    if state.runnable_at_ms > now_ms {
        return Advanced::NotYetRunnable {
            runnable_at_ms: state.runnable_at_ms,
        };
    }

    // ── ATTEMPTS ────────────────────────────────────────────────────────────
    if state.phase_attempts >= config.max_phase_attempts {
        return Advanced::Quarantined(Quarantine::PhaseAttemptsExhausted {
            phase: state.cursor.phase,
            attempts: state.phase_attempts,
        });
    }

    // ── ITERATION CEILING ───────────────────────────────────────────────────
    let max_iterations = host.context().max_iterations;
    if state.cursor.iteration > max_iterations {
        return Advanced::IterationCeiling {
            iteration: state.cursor.iteration,
            max_iterations,
        };
    }

    // A live host already owns Resolve's output and may enter Apply directly.
    // A cold holder may not deserialize that output after Resolve has mutated
    // history and authority state; it restores the pre-Resolve capsule instead.
    // Journal the backwards transition first so the snapshot never disagrees
    // with replay, then let the next claim enter Resolve normally.
    if state.cursor.phase == Phase::Decide && !host.has_live_decide_carry() {
        return rewind_to_observe(store, host, key, worker, lease, state, revision, now_ms).await;
    }

    if state.cursor.phase == Phase::Resolve
        && state.resolve_checkpoint.is_none()
        && !host.has_live_resolve_carry()
    {
        return rewind_to_observe(store, host, key, worker, lease, state, revision, now_ms).await;
    }

    if state.cursor.phase == Phase::Apply && !host.has_live_apply_carry() {
        if let Some(checkpoint) = state.resolve_checkpoint.as_ref() {
            if let Err(reason) = checkpoint.require_current() {
                return Advanced::ExecutionFailed { reason };
            }
            return rewind_apply_to_resolve(
                store, host, key, worker, lease, state, revision, now_ms,
            )
            .await;
        }
        return rewind_to_observe(store, host, key, worker, lease, state, revision, now_ms).await;
    }

    if state.cursor.phase == Phase::Epilogue
        && !state
            .iteration_checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.is_for(state.cursor))
    {
        return Advanced::ExecutionFailed {
            reason: format!(
                "execution_driver_stateless: Epilogue for iteration {} has no matching durable \
                 iteration checkpoint; refusing to substitute this worker's current history/time \
                 baselines",
                state.cursor.iteration
            ),
        };
    }

    // ── COMMITTED EFFECTS ───────────────────────────────────────────────────
    let recovering_committed_batch = state.pending.is_some();
    let (effects, operator_resolutions) =
        match resolve_effects(store, host, key, &state, lease, now_ms).await {
            Ok(resolved) => resolved,
            Err(advanced) => return advanced,
        };
    // THE ENTRY PLAN, taken before `effects` moves onto the `PhaseEntry`.
    //
    // Built here rather than inside the `Apply` arm below so the two consumers
    // are provably the same list: the gate is TOLD what was resolved through
    // `PhaseEntry::effects`, and the dispatch is BOUND by it through this. A
    // On recovery this entry answer is intentionally refreshed after intent
    // re-recording, because that write can re-arm `NotDispatched` or observe a
    // terminal row. The dispatch receives that post-intent answer; see the seam.
    //
    // Cheap for the case that dominates: `resolve_effects` returns an empty
    // vector at every cursor that is not `Apply`, and on the first entry into an
    // `Apply` too.
    let mut plan = EffectPlan::resolved(effects.iter().map(ResolvedEffect::planned).collect());

    // A host without the split gate has no later pre-dispatch intent seam. If
    // operator evidence licensed a re-fire from a committed batch, re-arm its
    // `NotDispatched` resolution now, before the opaque phase can send. The
    // committed batch remains the activation marker if this worker dies.
    if state.cursor.phase == Phase::Apply
        && !host.splits_apply_gate_from_dispatch()
        && !operator_resolutions.is_empty()
    {
        if let Err(error) =
            record_batch_intents(store, key, lease, state.pending.as_ref(), now_ms).await
        {
            return Advanced::Refused(Refusal::Store {
                during: "rearm_operator_authorized_effect",
                error,
            });
        }
    }

    // ── BUDGET: seed, then open ─────────────────────────────────────────────
    // `LoopState::for_commit` *replaces* `work_budget_consumed_ms` from the
    // context rather than adding to it, so a worker that did not seed publishes
    // a total starting at its own zero — silently, because a budget that comes
    // out low does not error, it just lets the run overrun. The segment is left
    // closed on load and reopened here, at phase entry, because parked and
    // queued time is free.
    host.context_mut().work_budget_consumed_ms = state.work_budget_consumed_ms;
    host.context_mut().work_budget_segment_started_at = None;
    if let Err(reason) = host.adopt_owner(&state.identity) {
        return Advanced::ExecutionFailed {
            reason: format!("committed owner could not be adopted: {reason}"),
        };
    }
    host.context_mut().work_budget_segment_started_at = Some(Instant::now());

    // ── RUN ONE PHASE ───────────────────────────────────────────────────────
    let phase = state.cursor.phase;
    let iteration = state.cursor.iteration;
    let timing_loaded_ms = timing_started.elapsed().as_millis() as u64;
    let pre_phase_report = match await_with_lease_heartbeat(
        store,
        lease,
        config.lease_ttl,
        host.conclude_before_phase(PhaseEntry {
            phase,
            iteration,
            state: &state,
            effects: Vec::new(),
        }),
    )
    .await
    {
        Ok(Ok(report)) => report,
        Ok(Err(failure)) => {
            return commit_failed_attempt(
                store,
                host,
                key,
                worker,
                state,
                revision,
                lease,
                phase,
                failure,
                Utc::now().timestamp_millis(),
            )
            .await;
        },
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "lease_heartbeat_during_pre_phase_conclusion",
                error,
            })
        },
    };
    // What the dispatch half settled, if this claim ran one. Declared out here
    // because it has to survive the `Err` path: a dispatch that died halfway
    // through a batch settled some of its members, and those answers are worth
    // more than the error is.
    let mut settled: Vec<(EffectId, EffectOutcome)> = Vec::new();
    let timing_pre_ms = timing_started.elapsed().as_millis() as u64;
    let report = if let Some(report) = pre_phase_report {
        Ok(report)
    } else if phase == Phase::Apply && host.splits_apply_gate_from_dispatch() {
        // ── THE SEAM ────────────────────────────────────────────────────────
        //
        // `Apply` is gate → **intents** → dispatch → **outcomes**, and the two
        // bold steps are this driver's because they are the two that need a
        // store. `phases::apply` deliberately cannot reach one: its parameter
        // list is the claim about what a phase may touch, so the ordering is
        // enforced by who is holding the store rather than by a rule a phase
        // has to remember.
        let gated = match await_with_lease_heartbeat(
            store,
            lease,
            config.lease_ttl,
            host.gate_apply(PhaseEntry {
                phase,
                iteration,
                state: &state,
                effects,
            }),
        )
        .await
        {
            Ok(gated) => gated,
            Err(error) => {
                return Advanced::Refused(Refusal::Store {
                    during: "lease_heartbeat_during_apply_gate",
                    error,
                })
            },
        };
        match gated {
            Err(failure) => Err(failure),
            // Nothing was gated, so there is nothing to commit and nothing to
            // fire. This is most of a first `Apply`. It is not a valid answer
            // while recovering an activated batch: clearing that marker would
            // turn its prepared rows into an abandoned prefix without the
            // dispatch ever consuming the recovery plan.
            Ok(GatedPhase::Settled(_report)) if recovering_committed_batch => {
                return Advanced::ExecutionFailed {
                    reason: "recovery Apply settled without re-gating the complete pending \
                             batch; its activation marker is retained and nothing dispatches"
                        .to_string(),
                }
            },
            Ok(GatedPhase::Settled(report)) => Ok(report),
            Ok(GatedPhase::Gated(gated)) => {
                let prepared_batch = match (state.pending.as_ref(), gated.batch()) {
                    (Some(committed), Some(prepared)) => {
                        match merge_re_gated_batch(committed, prepared) {
                            Ok(merged) => Some(merged),
                            Err(reason) => return Advanced::ExecutionFailed { reason },
                        }
                    },
                    (Some(_), None) => {
                        return Advanced::ExecutionFailed {
                            reason: "recovery Apply re-gated no keyed batch while a complete \
                                     pending batch is already committed at this cursor"
                                .to_string(),
                        }
                    },
                    (None, prepared) => prepared.cloned(),
                };
                // Validate recovery's re-derived batch before mutating even
                // one member row. A mismatch detected after intent writes would
                // leave a partial set of old active rows re-armed even though
                // dispatch never entered. Fresh batches have no committed
                // marker yet and retain the prepared-prefix protocol below.
                // THE INTENT COMMIT. Before the fire, which is the whole
                // contract, and reachable at last because the gate returned.
                let intents =
                    match record_batch_intents(store, key, lease, prepared_batch.as_ref(), now_ms)
                        .await
                    {
                        Ok(intents) => intents,
                        Err(error) => {
                            // Fail CLOSED. Nothing has fired yet, so refusing here
                            // costs one abandoned turn at an unmoved cursor and
                            // loses nothing; dispatching anyway would put effects on
                            // a transport that no resume could ever ask about, which
                            // is the exact condition the ledger exists to remove.
                            return Advanced::Refused(Refusal::Store {
                                during: "record_effect_intent",
                                error,
                            });
                        },
                    };
                // Publish the COMPLETE prepared set with one fenced CAS before
                // any member fires. Individual intent rows are necessarily
                // written one at a time; without this marker a failure on row N
                // leaves rows 0..N looking like effects that may have fired and
                // wedges the run in reconciliation even though dispatch was
                // never entered. New rows carry `prepared_only=true`, and
                // `resolve_effects` ignores them unless this state names them.
                if let Some(batch) = prepared_batch {
                    state.pending = Some(batch);
                    if let Err(refusal) =
                        project(&mut state, host, worker, Utc::now().timestamp_millis())
                    {
                        return Advanced::Refused(Refusal::Projection(refusal));
                    }
                    revision = match store.commit_fenced(key, &state, revision, lease).await {
                        Ok(revision) => revision,
                        Err(StoreError::Conflict { expected, found }) => {
                            return Advanced::Refused(Refusal::StaleCommit { expected, found })
                        },
                        Err(error) => {
                            return Advanced::Refused(Refusal::Store {
                                during: "commit_effect_batch_prepared",
                                error,
                            })
                        },
                    };
                }
                // Re-recording intents can legitimately change recovery's
                // answer: `NotDispatched` is re-armed to unknown, a legacy row
                // may gain its committed reconcile ref, and a concurrently
                // settled row must become Adopt. The pre-gate plan is therefore
                // only an entry snapshot. On a recovery claim, read and resolve
                // the activated complete batch again after every intent is
                // durable, then hand THAT plan to the dispatch. `plan_the_batch`
                // validates its effect-id correspondence before any member
                // fires. A first dispatch deliberately keeps the empty entry
                // plan; treating its newly prepared intents as recovery would
                // make local non-retry-safe work indeterminate before its first
                // attempt.
                if recovering_committed_batch {
                    let (refreshed, operator_resolutions) =
                        match resolve_effects(store, host, key, &state, lease, now_ms).await {
                            Ok(refreshed) => refreshed,
                            Err(advanced) => return advanced,
                        };
                    plan = EffectPlan::resolved(
                        refreshed.iter().map(ResolvedEffect::planned).collect(),
                    );
                    // `AuthorizeRefire` is represented durably as
                    // `NotDispatched` so disposition can recompute to Refire.
                    // That answer must NOT remain behind once dispatch is
                    // entered: it would be false positive evidence that a
                    // crash-mid-dispatch sent nothing. Re-arm every batch row
                    // that the refreshed resolution pass may have touched,
                    // after taking the plan but before handing control to the
                    // transport. Terminal adopted outcomes survive re-arming;
                    // `NotDispatched` returns to an unknown intent.
                    if !operator_resolutions.is_empty() {
                        if let Err(error) =
                            record_batch_intents(store, key, lease, state.pending.as_ref(), now_ms)
                                .await
                        {
                            return Advanced::Refused(Refusal::Store {
                                during: "rearm_operator_authorized_effect",
                                error,
                            });
                        }
                    }
                }
                // The plan travels with the receipt, and both are the driver's.
                // On a first attempt it is the same entry snapshot the gate was
                // told. On recovery it is the post-intent revalidation above,
                // because a plan taken before re-arming rows is stale by
                // construction.
                let dispatched = match await_with_lease_heartbeat(
                    store,
                    lease,
                    config.lease_ttl,
                    host.dispatch_apply(gated, intents, plan, state.deadline_at_ms, &mut settled),
                )
                .await
                {
                    Ok(dispatched) => dispatched,
                    Err(error) => {
                        return Advanced::Refused(Refusal::Store {
                            during: "lease_heartbeat_during_apply_dispatch",
                            error,
                        })
                    },
                };
                // THE OUTCOME COMMIT, on both paths. Ordered after the dispatch
                // and before any boundary, so a worker that dies between them
                // leaves rows an `unsettled` read will find rather than rows it
                // will not.
                if let Err(error) = record_batch_outcomes(store, key, lease, &settled).await {
                    return Advanced::Refused(Refusal::Store {
                        during: "record_effect_outcome",
                        error,
                    });
                }
                dispatched
            },
        }
    } else {
        match await_with_lease_heartbeat(
            store,
            lease,
            config.lease_ttl,
            host.run_phase(PhaseEntry {
                phase,
                iteration,
                state: &state,
                effects,
            }),
        )
        .await
        {
            Ok(report) => report,
            Err(error) => {
                return Advanced::Refused(Refusal::Store {
                    during: "lease_heartbeat_during_phase",
                    error,
                })
            },
        }
    };

    let mut report = match report {
        Ok(report) => report,
        Err(failure) => {
            // The batch travels with the error, and this is the only path that
            // publishes one — see [`PhaseFailure`]. The cursor does not move
            // here, so a batch committed on this path sits at the cursor
            // `resolve_effects` reads.
            return commit_failed_attempt(
                store,
                host,
                key,
                worker,
                state,
                revision,
                lease,
                phase,
                failure,
                Utc::now().timestamp_millis(),
            )
            .await;
        },
    };

    let _terminal_settlement_exclusion = if let PhaseStep::Return(outcome) = &report.step {
        match await_with_lease_heartbeat(
            store,
            lease,
            config.lease_ttl,
            host.acquire_terminal_settlement_exclusion(&state.identity, outcome.as_ref()),
        )
        .await
        {
            Ok(Ok(guard)) => guard,
            Ok(Err(error)) => {
                return commit_failed_attempt(
                    store,
                    host,
                    key,
                    worker,
                    state,
                    revision,
                    lease,
                    phase,
                    PhaseFailure::from(anyhow!("terminal lifecycle exclusion failed: {error}")),
                    Utc::now().timestamp_millis(),
                )
                .await;
            },
            Err(error) => {
                return Advanced::Refused(Refusal::Store {
                    during: "lease_heartbeat_during_terminal_lifecycle_exclusion",
                    error,
                })
            },
        }
    } else {
        None
    };

    if matches!(&report.step, PhaseStep::Return(_)) {
        let admission = match await_with_lease_heartbeat(
            store,
            lease,
            config.lease_ttl,
            host.admit_terminal_operator_steers(
                phase,
                iteration,
                report.steer_consume_receipt.as_ref(),
                report.terminal_steer_policy,
            ),
        )
        .await
        {
            Ok(Ok(admission)) => admission,
            Ok(Err(error)) => {
                return commit_failed_attempt(
                    store,
                    host,
                    key,
                    worker,
                    state,
                    revision,
                    lease,
                    phase,
                    PhaseFailure::from(anyhow!(
                        "durable operator-steer terminal admission failed: {error}"
                    )),
                    Utc::now().timestamp_millis(),
                )
                .await;
            },
            Err(error) => {
                return Advanced::Refused(Refusal::Store {
                    during: "lease_heartbeat_during_terminal_steer_admission",
                    error,
                })
            },
        };
        if admission == TerminalSteerAdmission::ContinueToNextDecide {
            // Preserve the phase's already-durable effect/event prefix, but do
            // not publish its terminal marker. Prepare/Observe can proceed
            // directly toward Decide; a terminal decision made at or after
            // Decide closes the iteration through Epilogue first. Either route
            // reaches a fresh Decide that claim-consumes the late batch.
            report.step = if matches!(phase, Phase::Prepare | Phase::Observe | Phase::Epilogue) {
                PhaseStep::Continue(())
            } else {
                PhaseStep::Exit(BoundaryOutcome::NextIteration)
            };
            report.wait = None;
        }
    }
    if !matches!(&report.step, PhaseStep::Return(_)) {
        match await_with_lease_heartbeat(
            store,
            lease,
            config.lease_ttl,
            host.reopen_operator_steers_after_nonterminal(phase, iteration),
        )
        .await
        {
            Ok(Ok(())) => {},
            Ok(Err(error)) => {
                return commit_failed_attempt(
                    store,
                    host,
                    key,
                    worker,
                    state,
                    revision,
                    lease,
                    phase,
                    PhaseFailure::from(anyhow!(
                        "durable operator-steer terminal fence reopen failed: {error}"
                    )),
                    Utc::now().timestamp_millis(),
                )
                .await;
            },
            Err(error) => {
                return Advanced::Refused(Refusal::Store {
                    during: "lease_heartbeat_during_terminal_steer_reopen",
                    error,
                })
            },
        }
    }
    // ── BOUNDARY ────────────────────────────────────────────────────────────
    let timing_phase_ms = timing_started.elapsed().as_millis() as u64;
    let advanced = commit_boundary(
        store,
        host,
        key,
        worker,
        config,
        state,
        revision,
        lease,
        phase,
        iteration,
        report,
        Utc::now().timestamp_millis(),
    )
    .await;
    let total_ms = timing_started.elapsed().as_millis() as u64;
    tracing::info!(
        iteration,
        phase = ?phase,
        load_ms = timing_loaded_ms,
        pre_ms = timing_pre_ms.saturating_sub(timing_loaded_ms),
        phase_ms = timing_phase_ms.saturating_sub(timing_pre_ms),
        commit_ms = total_ms.saturating_sub(timing_phase_ms),
        total_ms,
        "[LOOP-TIMING] phase advanced"
    );
    advanced
}

// ============================================================================
// Guards
// ============================================================================

/// Refuse to advance on a log this driver cannot vouch for.
///
/// Two distinct failures, and they are not degrees of the same thing:
///
/// - The log does not read, or its numbering is broken. Corruption; quarantine.
/// - The log reads, and the cursor it replays to is not the cursor the state
///   claims. Divergence; also quarantine, because the snapshot is a *cache* of
///   the journal and a cache that disagrees with its source is not repairable by
///   preferring either one.
///
/// The portable store contract reads the whole log. Filesystem production may
/// answer from its persistent integrity-bound tail index when the index checksum,
/// exact file identity/change stamp and committed watermark all match; any miss
/// falls back to the same bounded whole-log parse. This keeps the ordinary phase
/// entry proportional to its new boundary without turning a process-local cache
/// into journal authority.
///
/// **Replay is bounded by the committed watermark.** Records beyond it are an
/// orphaned attempt by a worker that appended and then failed to commit; they
/// are swept by the next append, and a driver that replayed them would resume
/// from a phase nothing committed. That is why this reads
/// [`Journal::authoritative`] and not `all_records` — and why the orphan test
/// for this function is the one that would catch reaching for the wrong one.
async fn verify_journal(
    store: &dyn LoopStateStore,
    key: &ExecutionKey,
    state: &LoopState,
) -> Result<Option<TerminalKind>, Advanced> {
    // One store verification, which remains whole-log for portable stores and
    // is index-backed only where the durable store can prove the exact prefix.
    //
    // This used to be `read_journal(key, 0)` followed by
    // `Journal::from_records(records)`. On `store::fs::FsLoopStateStore` — the
    // store the runtime configures — that pair deep-copied every record out of
    // the journal `Journal::parse` had *just* built and then re-walked the copy
    // applying the checks `parse` had already applied. This path runs at every
    // phase entry, so that was a full copy of the log six times an iteration.
    // `read_journal_verified` is the same contract with the copy removed where
    // the store can prove it is redundant; the trait's default still runs
    // `from_records`, so a store whose own read checks nothing — which is every
    // in-memory one — is checked exactly as it was before.
    //
    // The refusal split is unchanged: a journal error or a corrupt read is this
    // run's own bytes and quarantines it; anything else is the substrate and
    // refuses the claim. `during` still names `read_journal` because that is the
    // operation an operator is looking for, and it is what the default calls.
    let replayed = match store.replay_committed_journal(key, state.journal_seq).await {
        Ok(replayed) => replayed,
        Err(StoreError::Journal(error)) => {
            return Err(Advanced::Quarantined(Quarantine::CorruptJournal {
                detail: error.to_string(),
            }))
        },
        Err(StoreError::Corrupt { detail, .. }) => {
            return Err(Advanced::Quarantined(Quarantine::CorruptJournal { detail }))
        },
        Err(error) => {
            return Err(Advanced::Refused(Refusal::Store {
                during: "read_journal",
                error,
            }))
        },
    };
    let replayed_cursor = LoopCursor {
        iteration: replayed.iteration,
        phase: replayed.phase,
    };
    if replayed_cursor != state.cursor {
        return Err(Advanced::Quarantined(Quarantine::JournalDiverged {
            committed: state.cursor,
            replayed: replayed_cursor,
            watermark: state.journal_seq,
        }));
    }
    Ok(replayed.terminal)
}

/// Establish what may be done about every effect this run has no answer for,
/// before `Apply` re-runs.
///
/// The failure-mode row is *"Effect result missing → `EffectIndeterminate`;
/// never a silent re-fire"*, and the shape of this function is that sentence: an
/// effect that cannot be resolved aborts the whole boundary, so the phase never
/// gets the chance to fire blind. Resolving them one at a time and letting the
/// phase decide per effect would put the decision in the place that has the
/// least information about it.
///
/// # THE LEDGER RECORDS MEMBERS. `state.pending` ACTIVATES THE COMPLETE SET
///
/// This used to open by reading [`LoopState::pending`] and returning early when
/// it was `None`, and that early return was the bug. The split driver now
/// publishes the batch before dispatch, but legacy/unsplit paths and older
/// records still require the union below.
///
/// The ledger has no such gap, because the intent is written before the fire
/// rather than after the failure. [`EffectLedger::unsettled`] is now the
/// question, and it is asked of the ledger rather than of a batch:
///
/// - **an intent with no outcome** is an effect that may have fired;
/// - **a recorded `Indeterminate`** is the ledger saying it does not know, which
///   is the same position;
/// - everything else has an answer and is not asked about.
///
/// `state.pending` is read, validated, refused when orphaned, and resolved
/// member by member. For new rows it additionally proves the whole prepared set
/// landed; a `prepared_only` ledger row absent from it is ignored.
///
/// The two sources are a **union**, and dropping either loses a real case. The
/// batch is the only cover for a host that has not split its `Apply` and so
/// writes no intents; the ledger is the only cover for a worker that died rather
/// than returning `Err`. See the comment at the loop.
///
/// # Why this is not simply more expensive
///
/// It is one extra `load_effects` on a first claim whose cursor is `Apply` and
/// whose ledger is empty. A recovery claim reads once more after intent
/// re-recording so dispatch cannot act on a disposition that the write just
/// invalidated. In exchange, the boundary stops depending on a phase having had
/// the courtesy to fail rather than to die, and recovery never fires from a
/// stale pre-intent plan.
/// Take the host by `&mut`, not `&`, and not because anything is mutated.
///
/// This is `async` and holds the host across `load_effects(..).await`. A shared
/// `&dyn WorkerHost` is `Send` only if `dyn WorkerHost` is **`Sync`**, so a
/// shared reference here propagated a `Sync` bound all the way out to the
/// spawned core future in `executor.rs` and failed it — a host is one
/// execution's live borrows and has no business being `Sync`. `&mut T` is `Send`
/// whenever `T` is, which is the bound the trait already declares.
///
/// So: an exclusive borrow is the cheap fix, and requiring `Sync` would have
/// been the expensive one — it would force interior mutability or a lock into
/// every implementation to satisfy a thread-sharing property nothing wants.
async fn resolve_effects(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    state: &LoopState,
    lease: &mut Lease,
    now_ms: i64,
) -> Result<(Vec<ResolvedEffect>, Vec<EffectId>), Advanced> {
    // The batch is still checked wherever one is present. It is no longer what
    // decides whether there is anything to resolve — see this function's docs.
    if let Some(batch) = state.pending.as_ref() {
        // The sole non-Apply placement is the marker carried through the
        // journalled cold-recovery rewind. It names an Apply batch for this same
        // iteration and remains paired with the checkpoint Resolve is restoring.
        // Any other placement is still orphaned and fails closed.
        let recovery_rewind_marker = ((state.cold_reobserve
            && matches!(
                state.cursor.phase,
                Phase::Observe | Phase::Decide | Phase::Resolve
            ))
            || (state.cursor.phase == Phase::Resolve && state.resolve_checkpoint.is_some()))
            && batch.iteration == state.cursor.iteration
            && batch.phase == Phase::Apply;
        if state.cursor.phase != Phase::Apply && !recovery_rewind_marker {
            // Checked rather than ignored. Skipping straight past a batch at a
            // cursor that does not read one lets the next boundary overwrite
            // `pending` with the phase's own answer, which drops a live dispatch
            // with no record that anything was dropped.
            return Err(Advanced::Quarantined(Quarantine::OrphanedBatch {
                phase: state.cursor.phase,
                effects: batch.effects.len(),
            }));
        }
        // Checked on load, not only on construction: the value that hurts is the
        // one that arrived from disk.
        if let Err(error) = batch.validate() {
            return Err(Advanced::Quarantined(Quarantine::UnreadableState {
                detail: error.to_string(),
            }));
        }
    }
    // `Apply` is the only phase that dispatches, so it is the only cursor at
    // which an unresolved effect blocks anything. Asking anywhere else would
    // hold a run at `Observe` over a row `Apply` is about to settle.
    if state.cursor.phase != Phase::Apply {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut ledger = match store.load_effects(key).await {
        Ok(ledger) => ledger,
        Err(error) => {
            return Err(Advanced::Refused(Refusal::Store {
                during: "load_effects",
                error,
            }))
        },
    };

    // THE UNION OF BOTH SOURCES, and it has to be a union rather than a
    // replacement. Each covers a failure the other cannot see:
    //
    // - **The batch** covers a host that does not split its `Apply`
    //   (`WorkerHost::splits_apply_gate_from_dispatch` answering `false`, which
    //   is the default). Such a host writes no intents at all, so the ledger has
    //   nothing to say — and `commit_failed_attempt` still publishes what the
    //   phase had gated. Resolving only the ledger would silently drop that
    //   protection for every host that has not adopted the seam.
    // - **The ledger** covers the worker that DIED. `commit_failed_attempt` is
    //   reached only when a phase returns `Err`; a panic, a kill or an expired
    //   lease commits nothing, so `state.pending` is `None` for exactly the case
    //   the ledger exists for.
    //
    // Batch members first, so the order a phase gated in is the order a driver
    // reports — then the ledger rows the batch did not already name.
    let mut resolved = Vec::new();
    let batch_effects: Vec<PendingEffect> = state
        .pending
        .as_ref()
        .map(|batch| batch.effects.clone())
        .unwrap_or_default();
    let named_by_batch: std::collections::BTreeSet<String> = batch_effects
        .iter()
        .map(|effect| effect.effect_id.to_string())
        .collect();
    // The dispatch as the gate stated it, rebuilt from the row for anything the
    // batch does not carry. Total, because the row holds every field
    // `PendingEffect` has — including `reconcile_ref`, without which the
    // `Reconcile` arm below could never ask the outward record and every
    // non-retry-safe resume would surface to a user instead.
    let ledger_only: Vec<PendingEffect> = ledger
        .entries()
        .filter(|entry| entry.iteration == state.cursor.iteration)
        .filter(|entry| !named_by_batch.contains(entry.effect_id.as_str()))
        // New writers prepare member rows first and publish the complete batch
        // with one state CAS. A prepared row absent from that committed set is
        // a prefix of an aborted intent write, not evidence that dispatch may
        // have happened. Legacy rows lack the marker and stay conservative.
        .filter(|entry| !entry.prepared_only)
        .map(|entry| entry.pending())
        .collect();
    let mut operator_resolutions = Vec::new();
    for pending in batch_effects.iter().chain(ledger_only.iter()) {
        // A response is authoritative only while the ledger still has no
        // answer. A stale UI retry after an outcome landed is ignored, never
        // allowed to overwrite success/failure with an operator guess.
        if matches!(
            ledger.disposition(pending),
            EffectDisposition::Reconcile | EffectDisposition::Reattach { .. }
        ) {
            if let Some(resolution) = host.effect_resolution(&pending.effect_id) {
                if resolution == EffectResolution::AuthorizeRefire
                    && !host.splits_apply_gate_from_dispatch()
                    && state.pending.is_none()
                {
                    return Err(Advanced::EffectIndeterminate {
                        effect_id: pending.effect_id.clone(),
                        reason: "this legacy host has no committed batch or split pre-dispatch \
                                 seam through which an authorized retry can be re-armed safely"
                            .to_string(),
                    });
                }
                // A legacy/unsplit failed phase can publish a committed batch
                // without ever having written per-member intents. Resolution
                // must still be durable, so materialise that row under the
                // same lease before recording its answer.
                if ledger.get(&pending.effect_id).is_none() {
                    let entry = EffectLedgerEntry::intent(
                        pending,
                        state.cursor.iteration,
                        Phase::Apply,
                        now_ms,
                    );
                    if let Err(error) = store.record_effect_intent_fenced(key, &entry, lease).await
                    {
                        return Err(Advanced::Refused(Refusal::Store {
                            during: "record_effect_resolution_intent",
                            error,
                        }));
                    }
                    if let Err(error) = ledger.record_intent(entry) {
                        return Err(Advanced::ExecutionFailed {
                            reason: format!(
                                "the effect resolution intent was durable but could not be \
                                 applied to the loaded ledger: {error}"
                            ),
                        });
                    }
                }
                let outcome = match resolution {
                    EffectResolution::AdoptSucceeded => EffectOutcome::Succeeded { at_ms: now_ms },
                    EffectResolution::AuthorizeRefire => EffectOutcome::NotDispatched {
                        at_ms: now_ms,
                        reason: "an operator explicitly authorized retry after indeterminate \
                                 effect reconciliation"
                            .to_string(),
                    },
                };
                if let Err(error) = store
                    .record_effect_outcome_fenced(key, &pending.effect_id, outcome.clone(), lease)
                    .await
                {
                    return Err(Advanced::Refused(Refusal::Store {
                        during: "record_effect_resolution",
                        error,
                    }));
                }
                if let Err(error) = ledger.record_outcome(&pending.effect_id, outcome) {
                    return Err(Advanced::ExecutionFailed {
                        reason: format!(
                            "the effect resolution was durable but could not be applied to the \
                             loaded ledger: {error}"
                        ),
                    });
                }
                operator_resolutions.push(pending.effect_id.clone());
            }
        }
        let action = match ledger.disposition(pending) {
            EffectDisposition::Adopt => {
                let outcome = ledger
                    .get(&pending.effect_id)
                    .and_then(|entry| entry.outcome.as_ref());
                let result = outcome.and_then(|outcome| match outcome {
                    EffectOutcome::SucceededWithResult { result, .. }
                    | EffectOutcome::FailedWithResult { result, .. } => Some(result.clone()),
                    EffectOutcome::Succeeded { .. }
                    | EffectOutcome::Failed { .. }
                    | EffectOutcome::Indeterminate { .. }
                    | EffectOutcome::NotDispatched { .. } => None,
                });
                let succeeded = matches!(
                    outcome,
                    Some(
                        EffectOutcome::Succeeded { .. } | EffectOutcome::SucceededWithResult { .. }
                    )
                );
                EffectAction::Adopt { result, succeeded }
            },
            EffectDisposition::Refire => {
                authorize_refire(host, &ledger, pending)?;
                EffectAction::Refire
            },
            // THE REATTACH RULE, both halves. The row names the invocation; the
            // host turns that into the session. Neither step guesses: a row with
            // no invocation and an invocation with no session are different
            // failures with the same verdict, and both say which they are.
            EffectDisposition::Reattach {
                reattach_ref: Some(reattach_ref),
            } => match host.reattach_state(&state.identity, &reattach_ref) {
                ReattachState::Live { native_session_id } => {
                    EffectAction::Reattach { native_session_id }
                },
                ReattachState::Settled => {
                    return Err(Advanced::EffectIndeterminate {
                        effect_id: pending.effect_id.clone(),
                        reason: format!(
                            "the coding invocation this effect names ({reattach_ref}) is \
                                 already settled, but its effect outcome was not committed; the \
                                 settled result must be adopted or reconciled and must never be \
                                 dispatched again"
                        ),
                    });
                },
                ReattachState::Absent => {
                    return Err(Advanced::EffectIndeterminate {
                        effect_id: pending.effect_id.clone(),
                        reason: format!(
                            "the coding invocation this effect reattaches through ({reattach_ref}) \
                             reported no session, so there is nothing to resume and re-firing \
                             would run the job a second time"
                        ),
                    });
                },
            },
            EffectDisposition::Reattach { reattach_ref: None } => {
                return Err(Advanced::EffectIndeterminate {
                    effect_id: pending.effect_id.clone(),
                    reason: "a reattachable effect names no job to resume; its row was written \
                             before the gate minted one"
                        .to_string(),
                })
            },
            EffectDisposition::Reconcile => {
                // Decision 1, and the trap it names. `reconcile_ref: None` means
                // *this dispatch is not outward*. It is NOT "nothing left" — an
                // effect that reached `Reconcile` with no ref has no record to
                // ask, and no record to ask is `SurfaceToUser`. Reading the two
                // as the same thing re-sends live messages.
                // Recorded first, offered batch second. `record_intent` makes
                // the row monotonic specifically so an older re-gated batch
                // cannot erase the act identity recovery asks through.
                let reconcile_ref = ledger
                    .get(&pending.effect_id)
                    .and_then(|entry| entry.reconcile_ref.as_ref())
                    .or(pending.reconcile_ref.as_ref());
                let Some(reconcile_ref) = reconcile_ref else {
                    return Err(Advanced::EffectIndeterminate {
                        effect_id: pending.effect_id.clone(),
                        reason: "no outward record was committed for this dispatch, so nothing \
                                 can say whether it left"
                            .to_string(),
                    });
                };
                match host.reconcile_by_ref(reconcile_ref, &pending.effect_id) {
                    ReconciledEffect::AlreadyFired { by_this_attempt } => {
                        EffectAction::AlreadyFired { by_this_attempt }
                    },
                    ReconciledEffect::SafeToRefire => {
                        authorize_refire(host, &ledger, pending)?;
                        EffectAction::Refire
                    },
                    ReconciledEffect::SurfaceToUser { reason } => {
                        return Err(Advanced::EffectIndeterminate {
                            effect_id: pending.effect_id.clone(),
                            reason,
                        })
                    },
                }
            },
        };
        resolved.push(ResolvedEffect {
            effect_id: pending.effect_id.clone(),
            action,
        });
    }
    Ok((resolved, operator_resolutions))
}

/// Licence a re-fire, or refuse to advance.
///
/// Decision 1's second half. The fingerprint licenses a **re-fire**; the act ref
/// licenses a **reconciliation**; neither substitutes for the other, which is
/// why this runs on the `SafeToRefire` path too rather than only on the
/// retry-safe one. A mismatch fails the execution — the worker does not have the
/// dispatch it thinks it has, and firing anyway would send something the run
/// never committed to.
fn authorize_refire(
    host: &dyn WorkerHost,
    ledger: &EffectLedger,
    pending: &PendingEffect,
) -> Result<(), Advanced> {
    let Some(rederived) = host.rederive_dispatch(&pending.effect_id) else {
        return Err(Advanced::EffectIndeterminate {
            effect_id: pending.effect_id.clone(),
            reason: "the dispatch could not be re-derived from the turn its effect id names"
                .to_string(),
        });
    };
    ledger
        .authorize_refire(pending, &rederived.tool, &rederived.arguments_fingerprint)
        .map_err(|error| Advanced::ExecutionFailed {
            reason: error.to_string(),
        })
}

/// Refuse a park nothing can wake.
///
/// An idempotent retry can park a parent with no live child left to wake it — a
/// permanent hang — and under a stateless driver retries are routine, so the
/// hazard gets worse rather than better. [`WaitReason::children`] is the
/// constructor that enforces this, but a `WaitReason` can also be built by naming
/// the variant, so the boundary re-checks what it is about to publish.
fn check_park(wait: Option<&WaitReason>) -> Result<(), Refusal> {
    match wait {
        // `..` and not a binding: this refusal is about the live-child list being
        // empty, and the resume address has no bearing on it. Naming the field
        // here would suggest it participates.
        Some(WaitReason::Children {
            child_execution_ids,
            ..
        }) if child_execution_ids.is_empty() => Err(Refusal::ParkWithNoLiveChild),
        _ => Ok(()),
    }
}

// ============================================================================
// The boundary
// ============================================================================

/// The cursor a step moves to.
///
/// # This is where "`Epilogue` is skipped on a terminal" is implemented
///
/// The design's row says *"`Epilogue` runs on `NextIteration`/`Retry` outcomes
/// only, never on `Terminal`/`Pause`/`Park`"*. Those last three are not
/// [`super::outcome::BoundaryOutcome`] variants and never were — `outcome.rs`
/// documents at length that *a `break` never ends a run*, and that terminating,
/// pausing and parking are `return Ok(..)`. So the row's distinction is
/// [`PhaseStep::Return`] against [`PhaseStep::Exit`], and it lands here:
///
/// - `Exited` moves to [`Phase::Epilogue`] — the epilogue runs next claim. That
///   covers `Advance` and `PopFrame` as well as the two the row names, which is
///   what the resident driver does today: all four `break 'iteration_body` and
///   the epilogue runs after every one of them.
/// - `RunEnded` stays where it is. The epilogue never runs, and a resumable
///   terminal is picked up by re-running that same phase.
///
/// Taking the row literally — epilogue on `NextIteration`/`Retry` only — would
/// have skipped the stuck detector and `AgenticIterationCompleted` for every
/// rejection-driven `Advance`, which is a behaviour change dressed as
/// compliance.
///
/// This mirrors [`super::journal::replay_each`] exactly, and
/// `the_committed_cursor_is_the_cursor_the_journal_replays_to` asserts the two
/// agree phase by phase. Two implementations of one rule is a drift risk; a test
/// that walks both is what makes it a checked duplicate instead.
fn next_cursor(current: LoopCursor, step: &RecordedStep) -> Result<LoopCursor, Refusal> {
    match step {
        RecordedStep::Continued => Ok(match current.phase.next_in_iteration() {
            Some(next) => LoopCursor {
                iteration: current.iteration,
                phase: next,
            },
            // The one place the iteration counter steps. Folding this into a
            // plain `next()` would let a run walk off the end of its ceiling one
            // phase at a time.
            None => LoopCursor {
                iteration: current.iteration.saturating_add(1),
                phase: Phase::first(),
            },
        }),
        RecordedStep::Exited { .. } => {
            if current.phase == Phase::Epilogue {
                return Err(Refusal::ExitFromTheEpilogue);
            }
            Ok(LoopCursor {
                iteration: current.iteration,
                phase: Phase::Epilogue,
            })
        },
        RecordedStep::RunEnded { .. } => Ok(current),
    }
}

/// Apply an owner transition the phase journaled.
///
/// # Applied at the boundary, which under this driver *is* phase entry
///
/// The design says *"journaled as its own record and applied at phase entry,
/// never mid-phase"*. The second half is the load-bearing one and holds exactly:
/// the phase that decides a handover runs to completion under the old owner,
/// because this runs after [`WorkerHost::run_phase`] has returned.
///
/// The first half is implemented at the commit rather than at the *next* claim's
/// entry, and the reason is that the alternative cannot be made correct. The
/// record does not carry the incoming ceiling — deliberately, so the journal is
/// not a second source of truth for an agent's declaration — so applying it at a
/// later entry means committing a state whose `agent_id` disagrees with its own
/// log and re-scanning the log at every entry to find out. That divergence is
/// exactly what [`verify_journal`] exists to refuse, and `replay` cannot repair
/// it because replay does not touch identity. Applying here makes the record and
/// the state one atomic compare-and-swap.
///
/// Nothing observes the difference: one phase runs per claim, so there is no code
/// between "this commit" and "the next phase's entry". The entry-side obligation
/// the row also carries — invalidating cached prompt context, tool scope and
/// policy fingerprint, or refusing when the host cannot rebuild them — is
/// [`WorkerHost::adopt_owner`], which the driver calls at every entry.
///
/// The last transition in the batch wins, and a batch will not normally hold two.
fn apply_journaled_owner_transition(
    state: &mut LoopState,
    host: &dyn WorkerHost,
    records: &[JournalAppend],
) -> Result<(), LoopStateRefusal> {
    let Some(to_agent_id) = records.iter().rev().find_map(|append| match &append.body {
        JournalBody::OwnerTransition { to_agent_id, .. } => Some(to_agent_id.clone()),
        _ => None,
    }) else {
        return Ok(());
    };
    let incoming = host.browser_ceiling_for(&to_agent_id);
    apply_owner_transition(state, &to_agent_id, &incoming)
}

/// Publish a backwards cursor transition to a fresh Observe.
///
/// No phase and no effect runs here. Any checkpoint and the complete pending
/// batch are retained. The ledger records members one at a time; only the
/// pending batch is the atomic activation marker that distinguishes all of
/// those prepared rows from an abandoned prefix, including members whose
/// outcomes already settled before the crash.
async fn rewind_to_observe(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    worker: &WorkerId,
    lease: &Lease,
    mut state: LoopState,
    revision: Revision,
    now_ms: i64,
) -> Advanced {
    let iteration = state.cursor.iteration;

    host.context_mut().work_budget_consumed_ms = state.work_budget_consumed_ms;
    host.context_mut().work_budget_segment_started_at = None;
    if let Err(reason) = host.adopt_owner(&state.identity) {
        return Advanced::ExecutionFailed {
            reason: format!(
                "committed owner could not be adopted before recovery re-observation: {reason}"
            ),
        };
    }
    if let Err(refusal) = project(&mut state, host, worker, now_ms) {
        return Advanced::Refused(Refusal::Projection(refusal));
    }

    let append = match state.cursor.phase {
        Phase::Apply => JournalAppend::recovery_reobserve_from_apply(iteration),
        Phase::Decide => JournalAppend::recovery_reobserve_from_decide(iteration),
        Phase::Resolve => JournalAppend::recovery_reobserve(iteration),
        phase => {
            return Advanced::ExecutionFailed {
                reason: format!(
                    "cold re-observation cannot be represented from the {phase:?} cursor"
                ),
            }
        },
    };
    let seq = match store.append_journal_fenced(key, &[append], lease).await {
        Ok(seq) => seq,
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "append_recovery_reobserve",
                error,
            })
        },
    };
    let cursor = LoopCursor {
        iteration,
        phase: Phase::Observe,
    };
    state.journal_seq = seq;
    state.cursor = cursor;
    state.cold_reobserve = true;
    state.phase_attempts = 0;

    let revision = match store.commit_fenced(key, &state, revision, lease).await {
        Ok(revision) => revision,
        Err(StoreError::Conflict { expected, found }) => {
            return Advanced::Refused(Refusal::StaleCommit { expected, found })
        },
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "commit_recovery_reobserve",
                error,
            })
        },
    };
    project_outbox(store, host, key, lease, seq).await;
    Advanced::RecoveryRewound { revision, cursor }
}

async fn rewind_apply_to_resolve(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    worker: &WorkerId,
    lease: &Lease,
    mut state: LoopState,
    revision: Revision,
    now_ms: i64,
) -> Advanced {
    let iteration = state.cursor.iteration;

    host.context_mut().work_budget_consumed_ms = state.work_budget_consumed_ms;
    host.context_mut().work_budget_segment_started_at = None;
    if let Err(reason) = host.adopt_owner(&state.identity) {
        return Advanced::ExecutionFailed {
            reason: format!(
                "committed owner could not be adopted before recovery rewind: {reason}"
            ),
        };
    }
    if let Err(refusal) = project(&mut state, host, worker, now_ms) {
        return Advanced::Refused(Refusal::Projection(refusal));
    }

    let append = JournalAppend::recovery_rewind(iteration);
    let seq = match store.append_journal_fenced(key, &[append], lease).await {
        Ok(seq) => seq,
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "append_recovery_rewind",
                error,
            })
        },
    };
    let cursor = LoopCursor {
        iteration,
        phase: Phase::Resolve,
    };
    state.journal_seq = seq;
    state.cursor = cursor;
    state.cold_reobserve = true;
    state.phase_attempts = 0;

    let revision = match store.commit_fenced(key, &state, revision, lease).await {
        Ok(revision) => revision,
        Err(StoreError::Conflict { expected, found }) => {
            return Advanced::Refused(Refusal::StaleCommit { expected, found })
        },
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "commit_recovery_rewind",
                error,
            })
        },
    };
    project_outbox(store, host, key, lease, seq).await;
    Advanced::RecoveryRewound { revision, cursor }
}

/// Append, project the state, commit — and then emit what the commit made
/// authoritative.
///
/// Two different things are called *project* in this file and they are not
/// related: [`project`] is [`LoopState::for_commit`], the state projection every
/// commit site here runs, and [`project_outbox`] is the event outbox's read
/// side. The second one runs last, after the commit, because it may only walk
/// records at or below the watermark that commit published.
#[allow(clippy::too_many_arguments)]
async fn commit_boundary(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    worker: &WorkerId,
    config: &WorkerConfig,
    mut state: LoopState,
    revision: Revision,
    lease: &mut Lease,
    phase: Phase,
    iteration: usize,
    mut report: PhaseReport,
    // Publication time, read after the phase completes. Admission, deadline,
    // and retry checks intentionally share the earlier entry clock; pin renewal
    // must instead prove the holder is alive NOW. Reusing the entry clock after
    // a long phase would commit a pin that was already expired even though the
    // lease heartbeat kept the phase fenced throughout.
    now_ms: i64,
) -> Advanced {
    let sleeping_wake_at_ms = match &report.step {
        PhaseStep::Return(outcome) => match outcome.as_ref() {
            AgenticOutcome::Sleeping { wake_at, .. } => Some(wake_at.timestamp_millis()),
            _ => None,
        },
        _ => None,
    };
    let step = match &report.step {
        PhaseStep::Continue(()) => RecordedStep::Continued,
        PhaseStep::Exit(boundary) => RecordedStep::Exited {
            boundary: RecordedBoundary::from(*boundary),
        },
        PhaseStep::Return(outcome) => RecordedStep::RunEnded {
            terminal: TerminalKind::from(outcome.as_ref()),
        },
    };

    let cursor = match next_cursor(state.cursor, &step) {
        Ok(cursor) => cursor,
        Err(refusal) => return Advanced::Refused(refusal),
    };
    // Cold recovery carries the complete-set marker through every phase used to
    // reconstruct Apply's input. Those phases report no new batch of their own,
    // so an ordinary `None` must not erase the marker and turn prepared members
    // into an abandoned prefix. The Resolve/checkpoint arm retains compatibility
    // with recovery records written before `cold_reobserve` existed.
    let carries_recovery_batch = state.pending.is_some()
        && ((state.cold_reobserve
            && matches!(
                state.cursor.phase,
                Phase::Observe | Phase::Decide | Phase::Resolve
            ))
            || (state.cursor.phase == Phase::Resolve && state.resolve_checkpoint.is_some()));
    if carries_recovery_batch {
        let committed = state.pending.as_ref().expect("checked above");
        let expected = state.cursor.phase.next_in_iteration();
        if !matches!(step, RecordedStep::Continued) || expected != Some(cursor.phase) {
            return Advanced::Quarantined(Quarantine::OrphanedBatch {
                phase: cursor.phase,
                effects: committed.effects.len(),
            });
        }
        if report
            .pending
            .as_ref()
            .is_some_and(|reported| reported != committed)
        {
            return Advanced::ExecutionFailed {
                reason: "a cold reconstruction phase reported a pending batch different from \
                         the complete Apply batch marker it was carrying"
                    .to_string(),
            };
        }
        report.pending = state.pending.clone();
    }
    // Refused at PUBLICATION, not one claim later. `state.pending` is consumed
    // by exactly one cursor — Apply; cold reconstruction may carry it through
    // Observe, Decide and Resolve only while the durable marker is set. A report
    // publishing a batch anywhere else has produced a row nothing consumes.
    if let Some(batch) = report.pending.as_ref() {
        let recovery_destination = state.cold_reobserve
            && matches!(
                cursor.phase,
                Phase::Observe | Phase::Decide | Phase::Resolve
            );
        if cursor.phase != Phase::Apply && !recovery_destination {
            return Advanced::Quarantined(Quarantine::OrphanedBatch {
                phase: cursor.phase,
                effects: batch.effects.len(),
            });
        }
    }
    let terminal_park_has_no_live_child = matches!(
        &report.step,
        PhaseStep::Return(outcome)
            if matches!(
                outcome.as_ref(),
                AgenticOutcome::WaitingForChildren { child_execution_ids, .. }
                    if child_execution_ids.is_empty()
            )
    );
    if terminal_park_has_no_live_child {
        return Advanced::Refused(Refusal::ParkWithNoLiveChild);
    }
    if let Err(refusal) = check_park(report.wait.as_ref()) {
        return Advanced::Refused(refusal);
    }

    // The driver stamps the address on every record — iteration, phase and a
    // unique ordinal — so a host cannot collide with the completion record and
    // cannot mis-address an event as belonging to another phase.
    //
    // # A run-ending completion record goes LAST, and that is load-bearing
    //
    // For a phase that continues, the completion record comes first and the
    // host's records follow it. For a phase that ENDS THE RUN the order is
    // reversed, because `replay_each` clears a resumable terminal the moment any
    // record follows it (`journal.rs`, the `is_resumable` branch) — and it is
    // right to: a pause ends the invocation, something outside answers it, and
    // the run appends again. Replay cannot tell "the next record in this same
    // batch" from "the record that resumed this run", and it should not have to.
    //
    // Appending `[RunEnded{WaitingForConfirmation}, OwnerTransition]` in one
    // batch therefore made a **paused run read as live**: replay cleared the
    // terminal, `verify_journal` answered `Ok(None)`, and the next claim
    // re-entered the phase that had just paused — with nothing refused and
    // nothing logged. That is not hypothetical; an ordinary owner *collapse*
    // during `Resolve` produces exactly this batch.
    //
    // Putting the terminal last also matches what happened. Everything the phase
    // produced — its events, its owner transition — happened DURING the phase,
    // before it ended the run. The completion record is the last thing that was
    // true, so it is the last thing written.
    let ends_run = matches!(step, RecordedStep::RunEnded { .. });
    let mut appends = Vec::with_capacity(report.records.len() + 1);
    if !ends_run {
        appends.push(JournalAppend::phase_completed(iteration, phase, step));
    }
    for (index, append) in report.records.into_iter().enumerate() {
        appends.push(JournalAppend {
            iteration,
            phase,
            // The host's own position, and NOTHING else. An event's address is
            // the projector's dedupe key, so it must not shift because the phase
            // happened to end the run on this attempt — see
            // `journal::PHASE_COMPLETION_ORDINAL` for the silent-drop that
            // caused. The completion record takes a reserved ordinal at the top
            // of the range and never competes for these.
            ordinal: index as u32,
            body: append.body,
        });
    }
    if ends_run {
        appends.push(JournalAppend::phase_completed(iteration, phase, step));
    }

    // Applied before the projection, so `for_commit`'s `Portable ⇒ cdp-only`
    // check runs against the ceiling the handover leaves behind rather than the
    // one it replaced.
    if let Err(refusal) = apply_journaled_owner_transition(&mut state, host, &appends) {
        return Advanced::Refused(Refusal::Projection(refusal));
    }

    // The phase may have refreshed authority without changing owners. Persist
    // the same complete atomic identity that the next phase will validate.
    state.identity = host.identity_after_phase(&state.identity);

    // This is the logical iteration continuation, not a transcript snapshot for
    // every one of the six phase commits. The initial seed carries the pristine
    // boundary. Apply is the first phase that can add durable effects/history;
    // Epilogue closes that turn and supplies the next iteration's boundary.
    // ResolveCheckpoint remains the phase-local reconstruction source in
    // between. Refreshing here after every phase would repeatedly serialize the
    // same bounded history and turn a long run's checkpoint I/O quadratic.
    if !ends_run && matches!(phase, Phase::Apply | Phase::Epilogue) {
        state.continuation_checkpoint = match host.continuation_checkpoint(iteration) {
            Ok(checkpoint) => Some(checkpoint),
            Err(reason) => return Advanced::ExecutionFailed { reason },
        };
    }

    // Prepare is the only phase entered with fresh in-memory iteration
    // baselines. Publish them in the SAME fenced boundary that makes Observe
    // reachable, so every later cursor — especially a cold Epilogue — either
    // sees the exact iteration-bound values or refuses. A run-ending Prepare
    // never reaches Epilogue and therefore does not manufacture a checkpoint.
    if phase == Phase::Prepare && !ends_run {
        let checkpoint = host.iteration_checkpoint(iteration);
        if checkpoint.iteration != iteration {
            return Advanced::ExecutionFailed {
                reason: format!(
                    "the host supplied an iteration checkpoint for {} while committing Prepare \
                     for {iteration}",
                    checkpoint.iteration
                ),
            };
        }
        state.iteration_checkpoint = Some(checkpoint);
    }

    // Projected BEFORE the append, so a refused boundary writes nothing at all
    // rather than leaving an orphan record for the next attempt to sweep. The
    // projection reads only the placement, the identity's ceiling and the
    // context's budget, and none of the fields set below touch any of those — so
    // moving it earlier changes what a refusal costs and nothing else.
    if let Err(refusal) = project(&mut state, host, worker, now_ms) {
        return Advanced::Refused(Refusal::Projection(refusal));
    }

    // One append per commit boundary. A second append issued before the commit
    // that covers the first would find it sitting above the watermark, be unable
    // to tell it from a dead worker's attempt, and sweep it.
    let seq = match store.append_journal_fenced(key, &appends, lease).await {
        Ok(seq) => seq,
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "append_journal",
                error,
            })
        },
    };

    let terminal_settlement_receipt = match (&report.step, step) {
        (PhaseStep::Return(outcome), RecordedStep::RunEnded { terminal })
            if terminal != TerminalKind::HandedOff
                && (!terminal.is_resumable()
                    || matches!(
                        terminal,
                        TerminalKind::WaitingForUser
                            | TerminalKind::WaitingForConfirmation
                            | TerminalKind::PausedByUser
                    )) =>
        {
            let renewed = match store.renew(lease, config.lease_ttl).await {
                Ok(renewed) => renewed,
                Err(error) => {
                    return Advanced::Refused(Refusal::Store {
                        during: "renew_before_terminal_receipt",
                        error,
                    })
                },
            };
            *lease = renewed;
            let prepared = match host
                .prepare_terminal_settlement_receipt(
                    key,
                    &state.identity,
                    state.segment_binding.as_ref(),
                    terminal,
                    seq,
                    outcome.as_ref(),
                    revision,
                    lease,
                )
                .await
            {
                Ok(receipt) => receipt,
                Err(reason) => {
                    return Advanced::ExecutionFailed {
                        reason: format!(
                            "terminal outcome receipt could not be prepared before publishing \
                             RunEnded at seq {seq}: {reason}"
                        ),
                    }
                },
            };
            match store.renew(lease, config.lease_ttl).await {
                Ok(renewed) => {
                    *lease = renewed;
                    prepared
                },
                Err(error) => {
                    if let Some(receipt) = prepared.as_ref() {
                        if let Err(revoke_error) =
                            host.abort_terminal_settlement_receipt(receipt).await
                        {
                            tracing::error!(
                                execution = %key,
                                %revoke_error,
                                "[LOOP_WORKER] lost its lease after terminal pause preparation and could not exact-revoke the hidden generation"
                            );
                        }
                    }
                    return Advanced::Refused(Refusal::Store {
                        during: "renew_after_terminal_receipt",
                        error,
                    });
                },
            }
        },
        _ => None,
    };

    state.journal_seq = seq;
    // Every successful boundary replaces this pointer. A receipt can never
    // leak from an older ending into a later resumed prefix.
    state.terminal_settlement_receipt = terminal_settlement_receipt;
    state.cursor = cursor;
    state.pending = report.pending;
    state.resolve_checkpoint = report.resolve_checkpoint;
    state.steer_consume_receipt = report.steer_consume_receipt;
    if phase == Phase::Epilogue {
        state.iteration_checkpoint = None;
    }
    if phase == Phase::Apply {
        state.cold_reobserve = false;
    }
    state.wait = report.wait;
    // Consecutive, not lifetime: a phase that completed clears the count.
    state.phase_attempts = 0;
    if let Some(wake_at_ms) = sleeping_wake_at_ms {
        // A sleep deadline belongs to the same fenced state transaction as
        // its RunEnded record. The outer runtime queue is published only after
        // this call returns, so persisting it here is the boot-repair authority
        // for a process death in that gap.
        state.runnable_at_ms = wake_at_ms;
    }
    if let RecordedStep::Exited {
        boundary: RecordedBoundary::Retry { after_ms },
    } = step
    {
        // The resident driver sleeps here. A worker must not: sleeping holds a
        // worker for the whole delay, which is the resident-task cost this
        // design removes. The wait becomes a value the scheduler honours.
        //
        // Taken from the *recorded* delay rather than the `Duration`, so the
        // schedule and the log cannot disagree — `RecordedBoundary::from`
        // clamps, and reading the unclamped value here would schedule a wake the
        // journal does not describe.
        state.runnable_at_ms = Utc::now()
            .timestamp_millis()
            .saturating_add(i64::try_from(after_ms).unwrap_or(i64::MAX));
    }

    let revision = match store.commit_fenced(key, &state, revision, lease).await {
        Ok(revision) => revision,
        Err(StoreError::Conflict { expected, found }) => {
            if let Ok(Some(committed)) = store.load(key).await {
                if committed.state.journal_seq == seq
                    && committed.state.terminal_settlement_receipt
                        == state.terminal_settlement_receipt
                    && state.terminal_settlement_receipt.is_some()
                {
                    // A cross-process CAS can report a conflict after another
                    // observer adopted this exact state. The authoritative
                    // receipt proves the pause generation must be retained.
                    committed.revision
                } else {
                    if let Some(receipt) = state.terminal_settlement_receipt.as_ref() {
                        if let Err(revoke_error) =
                            host.abort_terminal_settlement_receipt(receipt).await
                        {
                            tracing::error!(
                                execution = %key,
                                %revoke_error,
                                "[LOOP_WORKER] stale terminal CAS could not exact-revoke its uncommitted pause generation"
                            );
                        }
                    }
                    tracing::warn!(
                        execution = %key,
                        worker = %worker,
                        fence = lease.fence,
                        "[LOOP_WORKER] a stale commit was refused; this attempt is discarded"
                    );
                    return Advanced::Refused(Refusal::StaleCommit { expected, found });
                }
            } else {
                // An unreadable authoritative state is not proof that the CAS
                // failed before publication. Leave the exact hidden generation
                // for bounded startup reconciliation rather than risk deleting
                // a receipt that actually landed.
                tracing::warn!(
                    execution = %key,
                    worker = %worker,
                    fence = lease.fence,
                    "[LOOP_WORKER] a stale commit was refused and authoritative receipt state could not be proven"
                );
                return Advanced::Refused(Refusal::StaleCommit { expected, found });
            }
        },
        Err(error) => {
            if let Ok(Some(committed)) = store.load(key).await {
                if committed.state.journal_seq == seq
                    && committed.state.terminal_settlement_receipt
                        == state.terminal_settlement_receipt
                    && state.terminal_settlement_receipt.is_some()
                {
                    committed.revision
                } else {
                    if let Some(receipt) = state.terminal_settlement_receipt.as_ref() {
                        if let Err(revoke_error) =
                            host.abort_terminal_settlement_receipt(receipt).await
                        {
                            tracing::error!(
                                execution = %key,
                                %revoke_error,
                                "[LOOP_WORKER] failed terminal CAS could not exact-revoke its uncommitted pause generation"
                            );
                        }
                    }
                    return Advanced::Refused(Refusal::Store {
                        during: "commit",
                        error,
                    });
                }
            } else {
                return Advanced::Refused(Refusal::Store {
                    during: "commit",
                    error,
                });
            }
        },
    };

    // Decide normally advances to Resolve, whose entry ACKs this receipt before
    // any work. Keep the terminal case honest too: a RunEnded boundary has no
    // following phase, so retire its committed batch here, and only here after
    // the fenced state write succeeded. A crash during this call leaves the
    // receipt in LoopState while the outer runtime is still Executing; startup
    // recovery can repeat the idempotent acknowledgement. Even if the outer
    // runtime settles before that recovery, the host-free terminal debt scan
    // includes `steer_consume_receipt`, ACKs it, and fenced-clears the pointer.
    if ends_run {
        if let Some(receipt) = state.steer_consume_receipt.as_ref() {
            if let Err(error) = host.acknowledge_operator_steers(receipt).await {
                tracing::error!(
                    execution = %key,
                    %error,
                    "[LOOP_WORKER] terminal boundary committed but its operator steer receipt could not be retired"
                );
            }
        }
    }

    // The outbox's read side, and it runs HERE — after the commit and before the
    // outcome is handed back — for two reasons that are not the same reason.
    //
    // After the commit, because the projection is bounded by the watermark that
    // commit published: everything above it is an attempt a `StaleCommit` may
    // still discard, and emitting it would put an orphan's events on a transport
    // no retraction ever follows.
    //
    // Receipt-backed endings are different: their request/event batch cannot
    // become visible until the host-free terminal lifecycle has converged
    // runtime + Artifact and installed its staged-admission handshake. The
    // durable terminal scan owns that whole committed batch. Projecting it here
    // would let a fast HITL response race an unavailable pause and would bypass
    // the cross-layer receipt even though the LoopState commit succeeded.
    let terminal_projection_owned_by_lifecycle =
        ends_run && state.terminal_settlement_receipt.is_some();
    if !terminal_projection_owned_by_lifecycle {
        project_outbox(store, host, key, lease, seq).await;
    }

    match report.step {
        PhaseStep::Return(outcome) => Advanced::RunEnded {
            revision,
            cursor,
            terminal: match step {
                RecordedStep::RunEnded { terminal } => terminal,
                // Not reachable: `step` is derived from `report.step` above.
                _ => TerminalKind::from(outcome.as_ref()),
            },
            outcome,
        },
        _ => Advanced::Committed {
            revision,
            cursor,
            step,
        },
    }
}

// ============================================================================
// The outbox's read side
// ============================================================================

/// A [`ProjectedEventSink`] that hands each record back to the host it came from.
///
/// # Where the vocabulary is rejoined, and why it is here
///
/// `JournalBody::Event { event_type, payload }` is a byte-exact split of
/// [`RuntimeTransportEvent`]'s own `#[serde(tag = "event_type", content =
/// "data")]` encoding, so [`outbox::rejoin`] is the whole conversion — there is
/// no mirror enum and nothing to keep in step. Doing it here rather than in the
/// host is deliberate: the take-and-drop policy below is the one decision that
/// can silently lose an event, and it belongs in the driver that owns the mark
/// rather than in each host that owns a transport.
///
/// # What this sink carries and what it refuses to decide
///
/// `ActionExecutors::emit_event` decides per event whether the event is a
/// **canonical runtime fact** or transport-only, from `canonical_event_scope`,
/// `canonical_event_sink` and `event_broadcaster`. A sink that reached for a
/// broadcaster directly would take every event down the transport-only branch
/// and quietly stop the persisted runtime-fact stream. `phases::outbox`'s module
/// docs carry the full argument.
///
/// So this sink decides nothing and delivers nothing. It rejoins, and hands the
/// record to the host **together with the producer's own decision** — since
/// 2026-08-28, when [`Self::emit_routed`] stopped being the trait's discarding
/// default. Before that the sentence here read *"the host still holds the
/// executors and still makes it"*, and it was the wrong reassurance twice over:
/// the routing was already recorded and was being thrown away, and a host
/// re-deriving from live executors is not the producing process on a
/// crash-restart replay. See [`Self::emit_routed`] and
/// [`WorkerHost::emit_projected_event_with_key`].
struct HostEventSink<'h> {
    host: &'h mut dyn WorkerHost,
    /// Records this build could not turn back into an event, and therefore
    /// **dropped**.
    ///
    /// Counted rather than refused. [`super::journal::EmitRefused`]'s docs give
    /// the reason and it is not a preference: a record whose payload this build
    /// cannot parse is refused identically on every retry, `ProjectorCursor`
    /// has no bound on those retries and no way to step over the record, and the
    /// whole tail of the run's outbox would be buried behind it. Losing one
    /// event is the smaller failure than losing every event after it.
    ///
    /// It is a real loss, so it is reported at the call site alongside
    /// [`Projection::emitted`](super::journal::Projection::emitted) — that count
    /// includes these, and a log line carrying it alone would overstate what
    /// reached a transport.
    unmappable: usize,
}

impl<'h> HostEventSink<'h> {
    fn new(host: &'h mut dyn WorkerHost) -> Self {
        Self {
            host,
            unmappable: 0,
        }
    }

    /// Rejoin one record and hand it to the host under the routing the caller
    /// has.
    ///
    /// **One body for both entry points, and that is what keeps the drop policy
    /// singular.** [`ProjectedEventSink::emit`] and
    /// [`ProjectedEventSink::emit_routed`] differ in exactly one thing — whether
    /// the caller knows what the producer decided — and every other line of this
    /// (the rejoin, the `unmappable` count, the take-and-drop) is the decision
    /// that can silently lose an event. Two copies of it is two chances to make
    /// one of them refuse instead, which buries every record above it for the
    /// life of the run.
    fn deliver(
        &mut self,
        key: &EventKey,
        event_type: &str,
        payload: &serde_json::Value,
        routing: &RecordedEventRouting,
    ) -> Result<(), EmitRefused> {
        let event = match outbox::rejoin(event_type, payload.clone()) {
            Ok(event) => event,
            Err(error) => {
                self.unmappable = self.unmappable.saturating_add(1);
                tracing::warn!(
                    address = %key,
                    event_type = %event_type,
                    %error,
                    "[LOOP_OUTBOX] a journalled event does not rejoin into an event this build \
                     knows; it is dropped rather than refused, because refusing it would bury \
                     every event above it for the life of the run"
                );
                // `Ok`, and the mark moves past it. See the field docs.
                return Ok(());
            },
        };
        self.host.emit_projected_event_with_key(key, event, routing)
    }
}

impl ProjectedEventSink for HostEventSink<'_> {
    /// The routing-less entry point, which the projection walk does not use.
    ///
    /// [`super::journal::ProjectorCursor::project`] calls [`Self::emit_routed`]
    /// for every `JournalBody::Event` it walks, so nothing in this driver
    /// reaches here. It is the trait's required method and it stays honest for
    /// whoever does call it: a caller with no routing to give describes exactly
    /// [`RecordedEventRouting::Unrecorded`] — *the record does not say* — and
    /// that is passed on as itself rather than guessed into `TransportOnly`,
    /// which would claim a decision nobody made.
    fn emit(
        &mut self,
        key: &EventKey,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Result<(), EmitRefused> {
        self.deliver(key, event_type, payload, &RecordedEventRouting::Unrecorded)
    }

    /// Hand the host the event **and** what its producer decided about it.
    ///
    /// # This override is half of `phases::outbox`'s condition 2
    ///
    /// Until 2026-08-28 this sink was on the trait's default, which forwards to
    /// [`Self::emit`] and drops `routing` on the floor — so
    /// [`RecordedEventRouting`] was computed by `phases::outbox::routing_for`,
    /// written into every `JournalBody::Event`, carried through the store, read
    /// back by the walk, and then discarded one call before the only party that
    /// can act on it. That is the shape the type's own docs call the worst
    /// available failure: every live surface keeps working and the persisted
    /// runtime-fact stream stops, with nothing looking wrong.
    ///
    /// **The obligation moves rather than ending here.** This sink does not
    /// deliver; it hands over. What discharges the other half is a host whose
    /// [`WorkerHost::emit_projected_event_with_key`] reads the value — see that method's
    /// *`routing` IS THE DECISION*. A host that takes the argument and ignores it
    /// is back to the same silent downgrade, and nothing in this file can tell.
    fn emit_routed(
        &mut self,
        key: &EventKey,
        event_type: &str,
        payload: &serde_json::Value,
        routing: &RecordedEventRouting,
    ) -> Result<(), EmitRefused> {
        self.deliver(key, event_type, payload, routing)
    }

    /// Hand the named rail straight to the host, with nothing rejoined.
    ///
    /// There is no `rejoin` step here and therefore no `unmappable` case: the
    /// record carries `emit_named`'s five arguments as themselves, so there is
    /// no encoding to parse and no build-specific vocabulary to fail on. What
    /// [`outbox::rejoin`] can refuse for a [`RuntimeTransportEvent`] — a variant
    /// this build does not know — has no analogue for a `String` name and a
    /// `Value` payload, which the broadcaster takes untyped in the first place.
    ///
    /// So this sink can lose a named event in exactly one way, and it is the
    /// host's refusal, which stops the walk with the mark below the record and
    /// is retried on the next boundary.
    fn emit_named(
        &mut self,
        _key: &EventKey,
        name: &str,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<(), EmitRefused> {
        // Cloned because the host takes the payload by value — it is going onto
        // a transport that owns it — while the walk hands out a borrow of the
        // record it is still holding. One clone per projected named event, on a
        // path that runs only for a host that declares transports; the same
        // clone `Self::emit` already pays for `outbox::rejoin`.
        //
        // A plain recursive `clone`, the same one `Self::emit` uses, rather
        // than `clone_json_iteratively`. Two bounds keep that safe and neither
        // is this line's: through `store::fs` a payload has been past
        // `serde_json::from_str`, whose default recursion limit is 128, so
        // anything deeper is a `CorruptRecord` at parse; through
        // `store::memory` it is the value `phases::outbox::build_named` cloned
        // out of a call-site `json!`, which is flat. Neither is an invariant
        // this file owns, which is why they are written down rather than
        // relied on silently — a producer that ever journalled deep untrusted
        // JSON makes this the wrong clone.
        self.host
            .emit_projected_named_event(name, agent_id, principal, workspace, payload.clone())
    }
}

/// Emit everything the commit that just landed made authoritative.
///
/// # Emit, THEN mark — and the order is the whole point
///
/// Marking first loses an event outright: a crash between the mark and the emit
/// leaves a record recorded as delivered that nothing ever delivered, and once
/// the mark is durable nothing re-offers it. Emitting first can deliver an event
/// twice — a crash after the emit and before the save re-emits that boundary on
/// the next pass — and **duplicate-but-marked is exactly what the dedupe window
/// absorbs**: the address is deterministic, so the second delivery is recognised
/// and dropped for as long as the original is inside
/// [`ProjectorCursor::DEFAULT_WINDOW`]. So this is at-least-once with a bounded
/// duplicate window, which is what an outbox buys, and it must never be
/// described as exactly-once.
///
/// # Bounded by the committed watermark, which is why it runs AFTER the commit
///
/// `watermark` is the `journal_seq` the commit published. Records beyond it are
/// an orphaned attempt by a worker that appended and then failed to commit —
/// never replayed and never projected. Projecting before the commit would emit
/// the events of an attempt a `Refusal::StaleCommit` was about to discard, which
/// is the one failure this ordering rule exists to prevent.
///
/// # Nothing here may fail the boundary
///
/// Every failure path warns and returns. The commit has already landed and the
/// phase has already advanced; turning an unreadable journal or an unwritable
/// mark into a refused boundary would hold a run hostage to its outbox. The cost
/// of each failure is stated where it is taken.
///
/// # What it costs
///
/// Per boundary, on a host that declares transports — the pass is skipped
/// entirely on one that does not, so none of this is paid by a host without transports:
///
/// - **One verified journal window**, on top of [`verify_journal`]. The portable
///   contract returns the full committed prefix. Filesystem production uses its
///   checksum-protected tail index to seek to the append-batch start containing
///   the projector mark and verifies the suffix hash chain; a legacy positional
///   dedupe key or any index miss falls back to the full bounded parse.
/// - **One bounded read of the mark**, [`LoopStateStore::load_projector_cursor`].
/// - **One durable write of the mark, on essentially every boundary.** Stated
///   because an earlier version of this section listed the journal read alone
///   and the guard below was commented as though the write were rare. It is not:
///   [`commit_boundary`] appends at least a `phase_completed` record every time,
///   the walk advances the mark past it, and so the mark moves on any boundary
///   this pass reaches at all. On `store::fs::FsLoopStateStore` that
///   write is `ensure_key_record`'s bounded read of `key.json`, then a temp file
///   created, written, **fsynced** and renamed, then a second fsync of the parent
///   directory. Six times an iteration.
///
/// So an operator sizing the flag flip should budget two bounded metadata/index
/// reads, a bounded tail read and one two-fsync cursor write per ordinary
/// boundary, with a full-journal fallback for legacy or invalid derived state.
///
/// # It runs under the lease, and that is what makes the save safe
///
/// [`advance_once`] releases only after [`advance_under_lease`] returns, so this
/// pass and its save happen while this worker still holds the claim. That is why
/// [`LoopStateStore::save_projector_cursor`] can be last-writer-wins instead of a
/// compare-and-swap: there is one writer at a time by construction, the same
/// property [`LoopStateStore::append_journal`] already relies on.
///
/// # WHAT THIS DOES NOT COVER
///
/// It is boundary-driven, not a background pump, and three gaps follow from that
/// rather than from anything being unfinished:
///
/// - **A refused emit waits for the next boundary.** A transport that is down
///   when a run parks leaves that boundary's events unemitted until something
///   wakes the run and commits again. [`leave_park`] and
///   [`commit_failed_attempt`] both commit and neither projects — deliberately,
///   since neither moves `journal_seq`, so there is nothing new to emit — but it
///   means a park is also a pause on the retry.
/// - **One commit outside this file moves the watermark without projecting.**
///   `reconciler::LoopReconciler::retire_under_lease` appends only a non-
///   emittable `RunEnded` completion. Before doing so it now reads the durable
///   projector cursor and refuses retirement if any accepted event is still
///   owed. That guard is part of this contract: adding an emittable record to
///   the retirement batch would require giving that path a real transport host.
/// - **A terminal refusal is retried by the host-free terminal lifecycle.** The
///   ending marker carries the last authoritative event seq; the dedicated
///   bounded debt scan keeps it visible until the projector cursor reaches it.
///   A direct `RunAlreadyEnded` encounter remains a safe opportunistic drain and
///   never appends past the terminal.
/// - **Only what a phase journalled.** Sites deliberately outside the outbox
///   still emit inline and have no replay protection. That does not require an
///   inline copy for accepted records: `phases::outbox` applies the delivery
///   decision per event.
///
///   **This line does not own those numbers and must not be read as a second
///   source for them.** It has been the stale copy once already: it said "nine
///   of the thirty-odd" for as long as the executor conversion had been in the
///   tree, telling a reader the outbox held a ninth of the timeline when it
///   held most of it. `phases::outbox`'s *WHAT IS NOT JOURNALLED* is
///   authoritative, carries the greps that reproduce both figures, and — the
///   part a number cannot convey — explains why the unjournalled figure is a
///   FLOOR that has risen on every re-sweep without any code regressing. Go
///   there before believing this bullet.
///
///   The authoritative census and the reason for every excluded rail live in
///   `phases::outbox`; this projection pass is already enabled on the stateless
///   production host.
async fn project_outbox(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    lease: &Lease,
    watermark: u64,
) {
    if !host.emits_projected_events() {
        // Not a log line. A host with no transports is the ordinary case for
        // every driver test and for the in-process arm until its host declares
        // one, and a warning per boundary for an expected configuration is how a
        // log stops being read.
        return;
    }

    let mut cursor = match store.load_projector_cursor(key).await {
        Ok(Some(cursor)) => cursor,
        // Nothing has projected this run yet. A fresh cursor is at seq zero with
        // an empty window, which is correct for a run that has emitted nothing.
        Ok(None) => ProjectorCursor::new(),
        Err(error) => {
            // Fail closed, and the direction matters. Substituting a fresh cursor
            // here would re-emit **every** authoritative event of the run on this
            // boundary and on every later one, because the mark that would have
            // stopped it is the thing that could not be read.
            tracing::warn!(
                execution = %key,
                %error,
                "[LOOP_OUTBOX] the projector mark could not be read; this run's events stay in its \
                 journal rather than being re-emitted from the beginning"
            );
            return;
        },
    };
    let mark_before = cursor.emitted_through_seq();
    let require_complete_history = cursor.requires_complete_history(key.execution_id());
    let records = match store
        .read_journal_projection(
            key,
            mark_before.saturating_add(1),
            watermark,
            require_complete_history,
        )
        .await
    {
        Ok(records) => records,
        Err(error) => {
            if matches!(error, StoreError::Journal(_) | StoreError::Corrupt { .. }) {
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] the journal does not parse after a commit; nothing is emitted \
                     and the mark is not moved"
                );
            } else {
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] the journal could not be read after a commit; this \
                     boundary's events are not emitted and the mark is not moved, so the next \
                     pass retries them"
                );
            }
            return;
        },
    };

    // Scoped so the sink's borrow of `host` ends before the store call below.
    // The projection is synchronous by construction — `ProjectedEventSink` is
    // deliberately not async — so nothing here is held across an await.
    let (projection, unmappable) = {
        let mut sink = HostEventSink::new(host);
        let projection = cursor.project_records(&records, key.execution_id(), watermark, &mut sink);
        (projection, sink.unmappable)
    };

    if projection.mark_beyond_watermark {
        // Not reachable from this driver: the mark only ever advances to a record
        // this store returned, under the watermark this store committed. It means
        // a mark restored against a journal that no longer reaches it — a hand
        // edit, a restored backup, or a mark saved under another execution's key.
        tracing::warn!(
            execution = %key,
            mark = mark_before,
            watermark = watermark,
            "[LOOP_OUTBOX] the projector mark is past the committed watermark, so this run's mark \
             does not belong to this run's journal; nothing is emitted until an operator \
             reconciles them"
        );
        return;
    }

    // Canonical sinks admit synchronously but persist asynchronously. Saving
    // the source cursor before those receipts resolve would turn a process loss
    // into a permanently missing canonical fact. Observer failure is part of
    // the same receipt: return with the cursor below the offered source event so
    // its stable EventKey-derived reference is replayed safely.
    if let Err(error) = host.await_projected_event_durability().await {
        tracing::warn!(
            execution = %key,
            mark = cursor.emitted_through_seq(),
            %error,
            "[LOOP_OUTBOX] canonical projection durability was not acknowledged; the source cursor is not saved and accepted transport deliveries may repeat"
        );
        return;
    }

    if let Some((seq, refused)) = &projection.stopped_at {
        // The mark is BELOW this record, so the next pass retries it. Unbounded,
        // by design — see `EmitRefused`. A sink that refuses the same record
        // forever buries everything above it, which is why the refusal names the
        // record rather than only the run.
        tracing::warn!(
            execution = %key,
            stopped_at_seq = *seq,
            reason = %refused,
            emitted = projection.emitted,
            "[LOOP_OUTBOX] a projection stopped at a refused emit; the mark is below that record \
             and every event above it waits behind it"
        );
    }

    if projection.emitted > 0 || projection.deduped > 0 || unmappable > 0 {
        tracing::debug!(
            execution = %key,
            emitted = projection.emitted,
            // Both reported, because `emitted` COUNTS the dropped ones: the sink
            // takes a record it cannot rejoin and answers `Ok`. A line carrying
            // `emitted` alone would say more events reached a transport than did.
            dropped_unmappable = unmappable,
            deduped = projection.deduped,
            watermark = watermark,
            "[LOOP_OUTBOX] projected"
        );
    }

    if cursor.emitted_through_seq() == mark_before {
        // Nothing moved, so there is nothing to record.
        //
        // This is RARE, and an earlier comment here claimed the opposite — that
        // it spared "a durable write on every boundary of a run that emits
        // nothing". It does not: `commit_boundary` appends at least a
        // `phase_completed` record on every boundary, and the walk advances the
        // mark past a non-event record, so the mark moves on any boundary this
        // pass reaches at all. A run that journals no events still writes the
        // mark six times an iteration, which is why that cost is listed in *What
        // it costs* above rather than described as avoided here.
        //
        // What actually reaches this line is a walk that moved nothing: the sink
        // refused the very FIRST pending record. Skipping the save then is still
        // correct — re-writing the mark it was loaded with buys nothing — but it
        // is a nicety on a failure path, not the guard the old comment described.
        return;
    }

    if let Err(error) = store
        .save_projector_cursor_fenced(key, &cursor, lease)
        .await
    {
        // Logged, never returned. The events are already on the transports; the
        // only thing lost is the record that they were, and the cost of that is
        // one boundary re-emitted after a restart — which the dedupe window
        // absorbs if the restart is soon and does not if it is not.
        tracing::warn!(
            execution = %key,
            mark = cursor.emitted_through_seq(),
            %error,
            "[LOOP_OUTBOX] events were emitted and the mark could not be saved; they may \
             be emitted again after a restart"
        );
    }
}

/// Validate a recovery re-gate and merge its one monotonic field.
///
/// Everything except `reconcile_ref` must reproduce the committed batch
/// exactly before any intent row is touched. A ref may fill an older absence;
/// an older gate may omit a ref the committed batch already has, in which case
/// the committed value is retained. Two present unequal refs are different
/// outward acts and fail closed. This mirrors `EffectLedger::record_intent`, but
/// runs over the whole batch first so a mismatch cannot leave a mutated prefix.
fn merge_re_gated_batch(
    committed: &PendingBatch,
    prepared: &PendingBatch,
) -> Result<PendingBatch, String> {
    if committed.iteration != prepared.iteration
        || committed.phase != prepared.phase
        || committed.mode != prepared.mode
        || committed.effects.len() != prepared.effects.len()
    {
        return Err(
            "Apply re-gated a batch with different iteration, phase, mode, or member count from \
             the complete batch already committed at this cursor"
                .to_string(),
        );
    }

    let mut merged = prepared.clone();
    for ((recorded, offered), out) in committed
        .effects
        .iter()
        .zip(prepared.effects.iter())
        .zip(merged.effects.iter_mut())
    {
        match (&recorded.reconcile_ref, &offered.reconcile_ref) {
            (Some(recorded_ref), Some(offered_ref)) if recorded_ref != offered_ref => {
                return Err(format!(
                    "effect {} was re-gated against outward act {} after the committed batch \
                     named {}; one effect id cannot reconcile through two acts",
                    offered.effect_id, offered_ref, recorded_ref
                ));
            },
            (Some(recorded_ref), None) => {
                out.reconcile_ref = Some(recorded_ref.clone());
            },
            (Some(_), Some(_)) | (None, Some(_)) | (None, None) => {},
        }

        let mut comparable_recorded = recorded.clone();
        comparable_recorded.reconcile_ref = out.reconcile_ref.clone();
        if comparable_recorded != *out {
            return Err(format!(
                "effect {} was re-gated with a dispatch identity other than the one in the \
                 committed complete batch",
                offered.effect_id
            ));
        }
    }
    merged
        .validate()
        .map_err(|error| format!("the merged recovery batch is invalid: {error}"))?;
    Ok(merged)
}

/// Write an intent for every member of a gated batch, before any of it fires.
///
/// **The one minter of a `Committed` [`ApplyIntents`], and the only place in the
/// tree that calls [`LoopStateStore::record_effect_intent`] in production.**
/// Both facts are the same fact: the receipt exists so that "a batch was
/// dispatched" and "its intents are durable" cannot come apart, and a second
/// minter would be a second definition of what the receipt claims.
///
/// # Why the whole batch, and how a partial write is made inert
///
/// A partially written batch used to wedge the run: the prefix rows looked like
/// effects that might have fired even though dispatch was never entered. New
/// rows are therefore `prepared_only`; after this function returns, the caller
/// publishes the complete [`PendingBatch`] with one fenced state CAS. Recovery
/// ignores a prepared row absent from that committed set. A single store failure
/// still aborts the turn with nothing fired, but its prefix is now inert rather
/// than a false in-flight effect.
///
/// # `None` is not a failure
///
/// A gate that admitted only dispatches it could not key publishes no batch.
/// There is nothing to write and the dispatch still runs — the effects are real,
/// they simply have no loop-side identity — so this answers a `Committed`
/// receipt for zero effects rather than refusing. The warning belongs at the
/// gate, which already issues it at the site that minted the unkeyable row.
async fn record_batch_intents(
    store: &dyn LoopStateStore,
    key: &ExecutionKey,
    lease: &Lease,
    batch: Option<&PendingBatch>,
    now_ms: i64,
) -> Result<ApplyIntents, StoreError> {
    let Some(batch) = batch else {
        return Ok(ApplyIntents::committed(0));
    };
    for effect in &batch.effects {
        let entry = EffectLedgerEntry::intent(effect, batch.iteration, batch.phase, now_ms);
        store
            .record_effect_intent_fenced(key, &entry, lease)
            .await?;
    }
    tracing::debug!(
        execution = %key,
        iteration = batch.iteration,
        effects = batch.effects.len(),
        "[LOOP_WORKER] committed this batch's intents; none of it has fired yet"
    );
    Ok(ApplyIntents::committed(batch.effects.len()))
}

/// Record what the dispatch learned about each member it settled.
///
/// Runs on the dispatch's success path *and* its failure path, which is the
/// point of `settled` being an out-parameter: a batch that died halfway has
/// answers for its earlier members, and those answers are the difference between
/// a resume that adopts them and one that reconciles them all over again.
///
/// # A conflict is a refusal, not a warning
///
/// [`EffectLedger::record_outcome`] refuses to overwrite one answer with a
/// different one, and this propagates that refusal rather than logging past it.
/// Two holders settling one effect differently is a real defect, and the version
/// that wins by arriving second is not more likely to be right — so the run is
/// held at an unmoved cursor where a human can look at it.
async fn record_batch_outcomes(
    store: &dyn LoopStateStore,
    key: &ExecutionKey,
    lease: &Lease,
    settled: &[(EffectId, EffectOutcome)],
) -> Result<(), StoreError> {
    for (effect_id, outcome) in settled {
        store
            .record_effect_outcome_fenced(key, effect_id, outcome.clone(), lease)
            .await?;
    }
    if !settled.is_empty() {
        tracing::debug!(
            execution = %key,
            settled = settled.len(),
            "[LOOP_WORKER] recorded this batch's outcomes"
        );
    }
    Ok(())
}

/// Count a failed attempt and publish it, so the next worker inherits it.
///
/// The count has to be durable or it is not a count: a phase that fails on
/// worker A and then on worker B has failed twice, and a counter that lived in
/// either process would read one. Committing costs one write on a path that has
/// already failed, which is the cheapest place in the cycle to pay for it.
#[allow(clippy::too_many_arguments)]
async fn commit_failed_attempt(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    worker: &WorkerId,
    mut state: LoopState,
    revision: Revision,
    lease: &Lease,
    phase: Phase,
    failure: PhaseFailure,
    now_ms: i64,
) -> Advanced {
    let PhaseFailure {
        error,
        pending,
        records,
    } = failure;
    let detail = error.to_string();
    state.phase_attempts = state.phase_attempts.saturating_add(1);
    let attempts = state.phase_attempts;
    // The one thing that DOES move, and only when the phase said something. The
    // record of what an orderly failure had committed to is worth keeping, and
    // it is worthless unless it is durable before the next worker re-enters the
    // same cursor. `None` is *this phase has nothing to say*, never *clear it*:
    // a phase that failed before it gated anything has learned nothing about an
    // earlier attempt's batch. Nor may a failure's partial re-gate replace an
    // existing complete activation marker; only the pre-dispatch fenced CAS can
    // publish a new complete set.
    //
    // NOT the mechanism that covers a dispatch nobody survived to report — see
    // `PhaseFailure`'s own *THIS IS NOT THE FIRE-BLIND CASE*. Reaching this
    // function at all means a phase returned `Err`, and a worker that died
    // returns nothing. The intent commit is what covers that, and it happens
    // before the fire rather than after the failure.
    //
    // Published here rather than at the boundary because the cursor does not
    // move on this path. `Apply` is the only cursor `resolve_effects` consults;
    // a batch published by `commit_boundary` would land at the cursor that
    // boundary moves TO, and be held as `Quarantine::OrphanedBatch`.
    if let Some(pending) = pending {
        if state
            .pending
            .as_ref()
            .is_some_and(|committed| committed != &pending)
        {
            return Advanced::ExecutionFailed {
                reason: "a failed recovery gate reported a partial or different pending batch; \
                         the already-committed complete activation marker is retained"
                    .to_string(),
            };
        }
        tracing::warn!(
            execution = %key,
            phase = ?phase,
            effects = pending.effects.len(),
            "[LOOP_WORKER] a phase failed after committing to a dispatch batch; the batch is \
             recorded at this cursor so the next attempt reconciles rather than re-fires"
        );
        if state.pending.is_none() {
            state.pending = Some(pending);
        }
    }
    // The cursor and decision capsule stay exactly where a fresh worker needs
    // them. Records from a successful prefix are authoritative even though the
    // whole phase did not complete: effect outcomes were fenced into the ledger
    // before reaching this function, so discarding their corresponding events
    // would make the durable state and user-visible history disagree.
    let appends = records
        .into_iter()
        .enumerate()
        .map(|(index, append)| JournalAppend {
            iteration: state.cursor.iteration,
            phase,
            ordinal: index as u32,
            body: append.body,
        })
        .collect::<Vec<_>>();
    if let Err(refusal) = apply_journaled_owner_transition(&mut state, host, &appends) {
        return Advanced::Refused(Refusal::Projection(refusal));
    }
    // A same-owner refresh has no transition record but can still change
    // authority. Commit the complete host snapshot, never a hand-picked subset.
    state.identity = host.identity_after_phase(&state.identity);
    if let Err(refusal) = project(&mut state, host, worker, now_ms) {
        return Advanced::Refused(Refusal::Projection(refusal));
    }
    let projected_through = if appends.is_empty() {
        None
    } else {
        match store.append_journal_fenced(key, &appends, lease).await {
            Ok(seq) => {
                state.journal_seq = seq;
                Some(seq)
            },
            Err(error) => {
                return Advanced::Refused(Refusal::Store {
                    during: "append_failed_attempt_journal",
                    error,
                })
            },
        }
    };
    match store.commit_fenced(key, &state, revision, lease).await {
        Ok(_) => {
            if let Some(seq) = projected_through {
                project_outbox(store, host, key, lease, seq).await;
            }
            Advanced::PhaseFailed {
                phase,
                attempts,
                detail,
            }
        },
        Err(StoreError::Conflict { expected, found }) => {
            Advanced::Refused(Refusal::StaleCommit { expected, found })
        },
        Err(error) => Advanced::Refused(Refusal::Store {
            during: "commit",
            error,
        }),
    }
}

/// The one projection, called at every commit site in this file.
///
/// A thin wrapper so the three sites cannot each assemble a
/// [`CommitPoint`] differently, and so the "only writer of
/// `work_budget_consumed_ms`" claim has one call to check per commit.
fn project(
    state: &mut LoopState,
    host: &dyn WorkerHost,
    worker: &WorkerId,
    now_ms: i64,
) -> Result<(), LoopStateRefusal> {
    let holds_user_typed_ephemeral_secret = host.holds_user_typed_ephemeral_secret();
    state.for_commit(CommitPoint {
        ctx: host.context(),
        worker,
        now_ms,
        holds_user_typed_ephemeral_secret,
    })
}

/// Clear a park whose wake has been resolved.
///
/// Three steps, in the order [`super::store::LoopStateStore::resolve_wake`]
/// fixes: read the resolutions, commit the state with the wait cleared, then
/// consume exactly those ids. Committing before consuming is what makes a crash
/// in the middle cost an extra round rather than a lost wake — a spin is bounded
/// and observable, a hang is neither.
///
/// No phase runs on this claim. Leaving the park is a durable transition of its
/// own, and folding it into the same invocation as a phase would make the
/// consume step's ordering depend on whether the phase succeeded.
// Eight parameters against `clippy.toml`'s `too-many-arguments-threshold = 7`.
// Pre-existing — this signature is unchanged by the stateless-loop work — and
// allowed rather than left to fail `cargo clippy -- -D warnings`. The three
// store-facing values (`key`, `state`, `revision`) travel together because a
// commit needs all three and the caller holds the lease that binds them; folding
// them into a struct here would name a shape only this function has.
#[allow(clippy::too_many_arguments)]
async fn leave_park(
    store: &dyn LoopStateStore,
    host: &mut dyn WorkerHost,
    key: &ExecutionKey,
    worker: &WorkerId,
    lease: &Lease,
    state: &mut LoopState,
    revision: Revision,
    wait: WaitReason,
    now_ms: i64,
) -> Advanced {
    let token = wait.wake_token();
    let resolutions = match store.wake_resolutions(key, &token).await {
        Ok(resolutions) => resolutions,
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "wake_resolutions",
                error,
            })
        },
    };
    if resolutions.is_empty() {
        return Advanced::Parked { wait };
    }

    state.wait = None;
    // Seeded here too: this commit publishes `work_budget_consumed_ms` like any
    // other, and a park that published this worker's zero would refund the whole
    // run's budget for free.
    host.context_mut().work_budget_consumed_ms = state.work_budget_consumed_ms;
    host.context_mut().work_budget_segment_started_at = None;
    if let Err(refusal) = project(state, host, worker, now_ms) {
        return Advanced::Refused(Refusal::Projection(refusal));
    }
    let revision = match store.commit_fenced(key, state, revision, lease).await {
        Ok(revision) => revision,
        Err(StoreError::Conflict { expected, found }) => {
            return Advanced::Refused(Refusal::StaleCommit { expected, found })
        },
        Err(error) => {
            return Advanced::Refused(Refusal::Store {
                during: "commit",
                error,
            })
        },
    };
    match store
        .consume_wake_fenced(key, &token, &resolutions, lease)
        .await
    {
        Ok(dropped) => Advanced::LeftPark {
            revision,
            resolutions: dropped,
        },
        Err(error) => {
            // The commit stands. An unconsumed resolution satisfies the *next*
            // park on this token without a fresh completion — one extra round,
            // which is the direction this ordering deliberately fails in.
            tracing::warn!(
                execution = %key,
                token = %token,
                error = %error,
                "[LOOP_WORKER] a wake was not consumed after its commit; the next park on this \
                 token will be satisfied once without a fresh completion"
            );
            Advanced::LeftPark {
                revision,
                resolutions: 0,
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::magician_v2::execution::agentic::run_loop::effects::{
        BatchMode, EffectId, EffectLedgerEntry, RetrySafety,
    };
    use crate::magician_v2::execution::agentic::run_loop::outcome::BoundaryOutcome;
    use crate::magician_v2::execution::agentic::run_loop::state::{Placement, ResolveCheckpoint};
    use crate::magician_v2::execution::agentic::run_loop::store::contract::{fresh_state, key};
    use crate::magician_v2::execution::agentic::run_loop::store::fs::FsLoopStateStore;
    use crate::magician_v2::execution::agentic::run_loop::store::memory::MemoryLoopStateStore;
    use crate::magician_v2::execution::agentic::run_loop::store::CommittedLoopState;
    use crate::magician_v2::execution::agentic::types::EnvironmentState;

    // ====================================================================
    // Fakes
    // ====================================================================

    /// What one phase call is told to do, scripted per invocation.
    enum Script {
        Continue,
        Exit(BoundaryOutcome),
        EndRun,
        Fail,
        /// Fail AFTER the gate admitted a dispatch, carrying the batch out on
        /// the error. The only shape that publishes a `PendingBatch`.
        FailMidDispatch,
        /// Continue, and journal an owner transition to this agent.
        Handover(String),
        /// Pause the run AND journal an owner transition, in one batch.
        ///
        /// The shape that had no script variant, which is why the defect it
        /// carries went unnoticed: `Handover` journals a transition but
        /// continues, and `EndRun` ends without one. Only both together produce
        /// the batch that made a paused run read as live.
        PauseWithHandover(String),
        /// Pause the run, journal these events AND an owner transition, in ONE
        /// batch — the events first, the transition last.
        ///
        /// The shape this fake could not say, and the gap is exactly the one
        /// `PauseWithHandover` left open a second time: `JournalEvents` journals
        /// events and hands over to nobody, `Handover` and `PauseWithHandover`
        /// hand over and journal no events. So no case in this file had ever put
        /// a record whose presence VARIES between attempts in the same batch as
        /// records whose addresses are counted from the start of it — which is
        /// the only shape in which `commit_boundary`'s `ordinal: index` can be
        /// wrong.
        ///
        /// # The order here mirrors production and does not stand in for it
        ///
        /// `executor.rs::InProcessWorkerHost::append_in_address_order` is what
        /// decides this order in the real host, and
        /// `executor.rs::tests::a_handover_never_shifts_an_event_ordinal` is
        /// what holds it. This arm reproduces the batch that function produces
        /// so the driver can be tested against it; a fixture cannot test the
        /// function that built it, and nothing below claims to.
        ///
        /// `WaitingForChildren` for the reason `PauseWithHandover` gives: it is
        /// a RESUMABLE terminal, so the cursor does not move and the next claim
        /// re-runs this same phase — which is what makes a SECOND attempt at the
        /// same address reachable at all.
        PauseWithHandoverAndEvents {
            to: String,
            events: Vec<(String, serde_json::Value)>,
        },
        /// Continue, and park on these children.
        Park(Vec<String>),
        /// Continue, and journal these events the way `phases::outbox` does.
        ///
        /// Raw `(event_type, payload)` pairs rather than
        /// [`RuntimeTransportEvent`]s, because one case has to journal a record
        /// this build **cannot** rejoin — which is not a value that type can
        /// hold. [`journals`] builds the ordinary case from a real event, so no
        /// test hand-writes a tag.
        JournalEvents(Vec<(String, serde_json::Value)>),
        /// Continue, and journal these events **with the routing the producer
        /// decided**, rather than with the placeholder [`JournalEvents`] uses.
        ///
        /// # It exists because the placeholder cannot express the defect
        ///
        /// Every other event-journalling arm above writes
        /// [`RecordedEventRouting::Unrecorded`], which is the field's `Default`
        /// **and** its `#[serde(default)]`. A case built on those arms therefore
        /// cannot tell a sink that hands the host the producer's decision from
        /// one on `emit_routed`'s discarding default: both put `Unrecorded` in
        /// front of the host, one because the record said so and one because the
        /// value was thrown away and re-defaulted. The assertion would be green
        /// either way, which is the shape of fake this tree has shipped three
        /// times.
        ///
        /// So this arm takes the routing per record and the case below asserts
        /// on values that are **not** the default.
        JournalRoutedEvents(Vec<(String, serde_json::Value, RecordedEventRouting)>),
        /// Continue, and journal these NAMED events the way
        /// `phases::outbox::journal_and_emit_named` does.
        ///
        /// `(name, payload)` pairs; the agent id and the scope are filled in by
        /// the fixture with values that are **not** the serde defaults — see
        /// [`FIXTURE_NAMED_AGENT`] — so a record that lost the scope on the way
        /// through the store fails rather than round-tripping `None` into
        /// `None`.
        JournalNamedEvents(Vec<(String, serde_json::Value)>),
        /// Continue, and journal BOTH rails in ONE batch — the transport events
        /// first, the named calls after, at consecutive ordinals.
        ///
        /// Its own variant rather than two scripted boundaries, because two
        /// boundaries are two batches and the property here is about one.
        /// `emit_step_events_if_signaled` is the production shape: it produces
        /// `AgenticStepStarted` and `plan.step.started` from a single phase, and
        /// they arrive in `PhaseReport::records` adjacent. A driver that routed
        /// every record down whichever rail it happened to implement passes a
        /// single-rail batch and fails this one.
        JournalBothRails {
            events: Vec<(String, serde_json::Value)>,
            named: Vec<(String, serde_json::Value)>,
        },
    }

    /// The agent id every scripted named record carries.
    ///
    /// A distinct string rather than `"__system__"`, which is what production
    /// substitutes when a context has no agent: a fixture using the fallback
    /// would pass a walk that dropped the field and re-derived the fallback.
    const FIXTURE_NAMED_AGENT: &str = "agent-named-rail";
    /// The scope every scripted named record carries.
    ///
    /// `Some` on both halves, and neither is the field's serde default. The
    /// record omits a `None` from the wire entirely
    /// (`skip_serializing_if = "Option::is_none"`), so a fixture built on `None`
    /// would round-trip through a store that dropped the fields and prove
    /// nothing about persistence.
    const FIXTURE_NAMED_PRINCIPAL: &str = "owner";
    const FIXTURE_NAMED_WORKSPACE: &str = "default";

    /// Split a real transport event the way `phases::outbox::split` does.
    ///
    /// Three lines rather than a call, because `split` is private to that
    /// module — and it has to stay the exact inverse of `outbox::rejoin`, which
    /// is what the projector's sink uses. The cases below compare the event a
    /// transport RECEIVES against the event this was built from, so a drift in
    /// either direction shows up as an inequality rather than as a passing test
    /// over a fixture of its own making.
    fn split_event(event: &RuntimeTransportEvent) -> (String, serde_json::Value) {
        let mut encoded = serde_json::to_value(event).expect("a transport event encodes");
        let object = encoded
            .as_object_mut()
            .expect("`#[serde(tag, content)]` encodes to an object");
        let event_type = match object.remove("event_type") {
            Some(serde_json::Value::String(tag)) => tag,
            other => panic!("the tag serde wrote is not a string: {other:?}"),
        };
        let payload = object.remove("data").unwrap_or(serde_json::Value::Null);
        (event_type, payload)
    }

    fn journals(event: &RuntimeTransportEvent) -> Script {
        Script::JournalEvents(vec![split_event(event)])
    }

    /// The records a phase's report would carry for these events.
    ///
    /// Built through [`JournalAppend::event`], the production constructor, so a
    /// fixture cannot journal a record the store would refuse.
    fn event_records(
        iteration: usize,
        phase: Phase,
        events: Vec<(String, serde_json::Value)>,
    ) -> Vec<JournalAppend> {
        events
            .into_iter()
            .map(|(event_type, payload)| {
                // Not about routing: this fixture exercises addresses and the
                // record-size ceiling. `Unrecorded` is the field's own answer
                // for that case, and it is a required parameter rather than a
                // default so the question is answered here rather than hidden.
                JournalAppend::event(
                    iteration,
                    phase,
                    event_type,
                    payload,
                    RecordedEventRouting::Unrecorded,
                )
                .expect("a fixture event fits a record")
            })
            .collect()
    }

    /// The event `phases::prepare` produces, for the run under test.
    ///
    /// A real variant of the real enum. A stand-in type would test nothing here:
    /// every property below is about what `RuntimeTransportEvent`'s own serde
    /// attributes do on the way into a record and back out of one.
    fn iteration_started(execution_id: &str) -> RuntimeTransportEvent {
        RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: execution_id.to_string(),
            principal: Some("owner".to_string()),
            workspace: Some("default".to_string()),
            plan_id: "plan-1".to_string(),
            step_id: "step-1".to_string(),
            iteration: 1,
            environment_type: "shell".to_string(),
            timestamp: 1_700_000_000_000,
        }
    }

    /// The ordinal of every EVENT-bodied record in a log, in seq order.
    ///
    /// Filtered by body rather than by position: the phase-completion record
    /// sits at `PHASE_COMPLETION_ORDINAL` and moves within the batch depending
    /// on whether the phase ended the run, and the owner transition is the
    /// record whose presence this is all about. Counting positions would make
    /// the assertion depend on both.
    fn event_ordinals(records: &[super::super::journal::JournalRecord]) -> Vec<u32> {
        records
            .iter()
            .filter(|record| matches!(record.body, JournalBody::Event { .. }))
            .map(|record| record.ordinal)
            .collect()
    }

    fn as_json(event: &RuntimeTransportEvent) -> serde_json::Value {
        serde_json::to_value(event).expect("a transport event encodes")
    }

    struct FakeHost {
        ctx: AgenticContext,
        script: Mutex<Vec<Script>>,
        /// One entry per phase actually run: which phase, under which owner, and
        /// whether the work-budget segment was open when it ran.
        ran: Mutex<Vec<(Phase, Option<String>, bool)>>,
        entries: Mutex<Vec<Vec<ResolvedEffect>>>,
        holds_secret: bool,
        /// When present, models a host whose live authority cannot be rebuilt to
        /// match the identity committed by the run.
        owner_adoption_error: Option<String>,
        ceilings: std::collections::HashMap<String, Vec<String>>,
        rederived: Option<RederivedDispatch>,
        reconciled: Mutex<Vec<String>>,
        reconcile_answer: ReconcileAnswer,
        /// Put a batch on the SUCCESS report — the placement the boundary
        /// refuses. Off by default because a correct host never does it.
        publish_batch_on_success: bool,
        /// What this host's transports received, or `None` when it has none.
        ///
        /// `None` is the default and it is the shape of every host in this file
        /// that is not testing the outbox — including, today, the real
        /// `InProcessWorkerHost`. `emits_projected_events` reads exactly this, so
        /// a case that forgets to give a host a transport gets *no projection*
        /// rather than a projection into a hole.
        transport: Option<Vec<RuntimeTransportEvent>>,
        /// What this host's transports received on the **named** rail, with all
        /// five arguments.
        ///
        /// A second vector rather than a re-encoding into `transport`, and the
        /// difference is not tidiness. `emit_named` really does end at
        /// `RuntimeTransportEvent::AgentEvent`, so pushing a rebuilt one into
        /// `transport` would look faithful — and it would make every case that
        /// asserts on `transport` unable to tell a named delivery from a
        /// transport-event delivery, which is the one distinction this whole
        /// variant exists for. Kept apart, a host that sent a named record down
        /// the wrong rail shows up as a missing entry here rather than as an
        /// extra one there.
        ///
        /// Not an `Option`: `emits_projected_events` reads `transport` and only
        /// `transport`, so a second nullable flag would let a host declare
        /// transports and still have nowhere for half its records to land.
        #[allow(clippy::type_complexity)]
        named_transport: Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            serde_json::Value,
        )>,
        /// Refuse this many emits before accepting any.
        ///
        /// Models a transport that is down and comes back — the only thing
        /// `EmitRefused` is allowed to mean. It is a COUNT and not a predicate on
        /// the event, because a refusal that is a function of the record is a
        /// permanent head-of-line block and no fixture should make one look
        /// routine.
        ///
        /// Shared by both rails, deliberately: a projection walk stops at the
        /// FIRST refusal whatever rail it came from, so a fixture with a
        /// per-rail counter could express a walk that carried on past one — a
        /// state the walk cannot reach.
        refusals_remaining: usize,
        /// The routing this host was handed for each accepted transport event,
        /// in delivery order.
        ///
        /// Recorded SEPARATELY from `transport` rather than folded into it, and
        /// that separation is the property under test. A fake that stored only
        /// the event could not tell a host that was handed the producer's
        /// decision from one that was handed the discarding default — which is
        /// exactly the failure `HostEventSink::emit_routed` exists to end, and
        /// exactly the shape of fake whose script vocabulary cannot express the
        /// defect.
        ///
        /// A refused emit appends nothing, for the same reason the refusal
        /// check runs before the push: a record the host did not take is not a
        /// record the host was told about.
        routings: Vec<RecordedEventRouting>,
        /// What each coding invocation id resolves to, as
        /// `reattach_state` would read it off a coding ledger.
        ///
        /// A MAP and not a single answer, because the defect worth catching is a
        /// ref that resolves to the wrong entry — and a fake that answered one
        /// session for every ref could not tell that apart from a correct
        /// lookup. An id with no key here is an invocation that reported no
        /// session, which is the honest indeterminate.
        reattach_sessions: std::collections::HashMap<String, String>,
        /// Invocation ids whose coding ledger entry is already settled.
        settled_reattach_refs: std::collections::HashSet<String>,
        /// Explicit operator answers, keyed by the effect id they resolve.
        effect_resolutions: std::collections::HashMap<String, EffectResolution>,
        /// Every `(execution_id, reattach_ref)` this host was asked about.
        ///
        /// The execution id is recorded because the identity is a PARAMETER
        /// rather than something the host reads off itself, and a host handed
        /// the wrong run's identity would read the wrong ledger — a failure that
        /// looks identical to "no session recorded" from the answer alone.
        reattach_asked: Mutex<Vec<(Option<String>, String)>>,
        live_apply_carry: bool,
        live_resolve_carry: bool,
        live_decide_carry: bool,
        /// Optional production-style resource/scope closure returned before
        /// phase work. Keeping this separate from `script` proves the driver
        /// consults the claimed pre-phase seam rather than merely exercising a
        /// phase that happens to return terminal.
        pre_phase_terminal_reason: Option<String>,
        terminal_admission_policies: Mutex<Vec<TerminalSteerPolicy>>,
        terminal_admission_answer: TerminalSteerAdmission,
    }

    #[derive(Clone, Copy)]
    enum ReconcileAnswer {
        Fired,
        Safe,
        Surface,
    }

    impl FakeHost {
        fn new(script: Vec<Script>) -> Self {
            let mut ctx = AgenticContext::new("goal", "criteria");
            ctx.max_iterations = 10;
            Self {
                ctx,
                script: Mutex::new(script),
                ran: Mutex::new(Vec::new()),
                entries: Mutex::new(Vec::new()),
                holds_secret: false,
                owner_adoption_error: None,
                ceilings: std::collections::HashMap::new(),
                rederived: Some(RederivedDispatch {
                    tool: "gmail__send".to_string(),
                    arguments_fingerprint: "fp-1".to_string(),
                }),
                reconciled: Mutex::new(Vec::new()),
                reconcile_answer: ReconcileAnswer::Surface,
                publish_batch_on_success: false,
                transport: None,
                named_transport: Vec::new(),
                refusals_remaining: 0,
                routings: Vec::new(),
                reattach_sessions: std::collections::HashMap::new(),
                settled_reattach_refs: std::collections::HashSet::new(),
                effect_resolutions: std::collections::HashMap::new(),
                reattach_asked: Mutex::new(Vec::new()),
                live_apply_carry: true,
                live_resolve_carry: true,
                live_decide_carry: true,
                pre_phase_terminal_reason: None,
                terminal_admission_policies: Mutex::new(Vec::new()),
                terminal_admission_answer: TerminalSteerAdmission::Admitted,
            }
        }

        /// The same host, with somewhere for a projected event to go.
        fn with_transport(script: Vec<Script>) -> Self {
            Self {
                transport: Some(Vec::new()),
                ..Self::new(script)
            }
        }

        fn phases_run(&self) -> Vec<Phase> {
            self.ran
                .lock()
                .unwrap()
                .iter()
                .map(|(p, _, _)| *p)
                .collect()
        }

        /// What reached this host's transports, as JSON.
        ///
        /// JSON because `RuntimeTransportEvent` derives no `PartialEq`, and
        /// comparing the encodings is the stronger check anyway: it is the
        /// encoding that travels through the journal.
        fn delivered(&self) -> Vec<serde_json::Value> {
            self.transport
                .as_ref()
                .expect("this host was built with a transport")
                .iter()
                .map(as_json)
                .collect()
        }

        /// What reached this host's transports on the NAMED rail, whole.
        ///
        /// All five arguments and not the name alone: the name is the field a
        /// projector is least likely to lose, and the scope pair is the one it
        /// is most likely to normalise — a `None` where the producer sent
        /// `Some` changes which envelope constructor `emit_named` reaches for.
        #[allow(clippy::type_complexity)]
        fn named_delivered(
            &self,
        ) -> Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            serde_json::Value,
        )> {
            self.named_transport.clone()
        }

        /// What the sink told this host about each event it accepted.
        fn routings_seen(&self) -> Vec<RecordedEventRouting> {
            self.routings.clone()
        }
    }

    fn fixture_continuation(
        iteration: usize,
    ) -> Result<super::super::state::IterationContinuationCheckpoint, String> {
        let pause = crate::magician_v2::execution::agentic::types::AgenticPauseState::new(
            iteration,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "test continuation",
            10,
            iteration.saturating_sub(1),
        )
        .with_scope("owner", "default");
        super::super::state::IterationContinuationCheckpoint::try_new(&pause)
    }

    #[async_trait]
    impl WorkerHost for FakeHost {
        fn context(&self) -> &AgenticContext {
            &self.ctx
        }

        fn context_mut(&mut self) -> &mut AgenticContext {
            &mut self.ctx
        }

        fn holds_user_typed_ephemeral_secret(&self) -> bool {
            self.holds_secret
        }

        fn browser_ceiling_for(&self, agent_id: &str) -> Vec<String> {
            self.ceilings
                .get(agent_id)
                .cloned()
                .unwrap_or_else(|| vec!["cdp".to_string()])
        }

        fn adopt_owner(&mut self, identity: &RunIdentity) -> Result<(), String> {
            if let Some(reason) = self.owner_adoption_error.as_ref() {
                return Err(reason.clone());
            }
            self.ctx.agent_id = identity.agent_id.clone();
            Ok(())
        }

        fn rederive_dispatch(&self, _effect_id: &EffectId) -> Option<RederivedDispatch> {
            self.rederived.clone()
        }

        fn reconcile_by_ref(
            &self,
            reconcile_ref: &CommittedActRef,
            _effect_id: &EffectId,
        ) -> ReconciledEffect {
            // `Display`, which renders `act-…@principal/workspace`. Recorded in
            // full rather than as a bare ref so a fixture that lost the scope
            // half would show up here rather than compare equal.
            self.reconciled
                .lock()
                .unwrap()
                .push(reconcile_ref.to_string());
            match self.reconcile_answer {
                ReconcileAnswer::Fired => ReconciledEffect::AlreadyFired {
                    by_this_attempt: false,
                },
                ReconcileAnswer::Safe => ReconciledEffect::SafeToRefire,
                ReconcileAnswer::Surface => ReconciledEffect::SurfaceToUser {
                    reason: "the outward record could not answer".to_string(),
                },
            }
        }

        fn reattach_state(&self, identity: &RunIdentity, reattach_ref: &str) -> ReattachState {
            self.reattach_asked
                .lock()
                .unwrap()
                .push((identity.execution_id.clone(), reattach_ref.to_string()));
            if self.settled_reattach_refs.contains(reattach_ref) {
                return ReattachState::Settled;
            }
            self.reattach_sessions
                .get(reattach_ref)
                .cloned()
                .map(|native_session_id| ReattachState::Live { native_session_id })
                .unwrap_or(ReattachState::Absent)
        }

        fn effect_resolution(&self, effect_id: &EffectId) -> Option<EffectResolution> {
            self.effect_resolutions.get(effect_id.as_str()).copied()
        }

        fn has_live_apply_carry(&self) -> bool {
            self.live_apply_carry
        }

        fn has_live_resolve_carry(&self) -> bool {
            self.live_resolve_carry
        }

        fn has_live_decide_carry(&self) -> bool {
            self.live_decide_carry
        }

        async fn conclude_deadline<'a>(
            &mut self,
            entry: PhaseEntry<'a>,
            deadline_at_ms: i64,
        ) -> PhaseReport {
            PhaseReport::ends_run(AgenticOutcome::Failed {
                reason: format!("execution deadline passed at {deadline_at_ms}"),
                last_state: EnvironmentState::Uninitialized,
                iterations_used: entry.iteration.saturating_sub(1),
            })
        }

        async fn conclude_before_phase<'a>(
            &mut self,
            entry: PhaseEntry<'a>,
        ) -> Result<Option<PhaseReport>, PhaseFailure> {
            Ok(self.pre_phase_terminal_reason.clone().map(|reason| {
                PhaseReport::ends_run_for_runtime_closure(AgenticOutcome::Failed {
                    reason,
                    last_state: EnvironmentState::Uninitialized,
                    iterations_used: entry.iteration.saturating_sub(1),
                })
            }))
        }

        async fn admit_terminal_operator_steers(
            &mut self,
            _phase: Phase,
            _iteration: usize,
            _receipt: Option<&super::super::steer_inbox::SteerConsumeReceipt>,
            policy: TerminalSteerPolicy,
        ) -> Result<TerminalSteerAdmission, String> {
            self.terminal_admission_policies
                .lock()
                .unwrap()
                .push(policy);
            Ok(self.terminal_admission_answer)
        }

        fn iteration_checkpoint(&self, iteration: usize) -> IterationCheckpoint {
            IterationCheckpoint {
                iteration,
                history_iterations_len_at_start: 0,
                started_at_ms: 1_000,
            }
        }

        fn continuation_checkpoint(
            &self,
            iteration: usize,
        ) -> Result<super::super::state::IterationContinuationCheckpoint, String> {
            fixture_continuation(iteration)
        }

        async fn run_phase<'a>(
            &mut self,
            entry: PhaseEntry<'a>,
        ) -> Result<PhaseReport, PhaseFailure> {
            self.ran.lock().unwrap().push((
                entry.phase,
                self.ctx.agent_id.clone(),
                self.ctx.work_budget_segment_started_at.is_some(),
            ));
            self.entries.lock().unwrap().push(entry.effects.clone());
            let script = {
                let mut script = self.script.lock().unwrap();
                if script.is_empty() {
                    Script::Continue
                } else {
                    script.remove(0)
                }
            };
            let publish_batch_on_success = self.publish_batch_on_success;
            let with_batch = |mut report: PhaseReport| {
                if publish_batch_on_success {
                    report.pending = Some(batch(vec![outward_effect(
                        "llm-1:tool:send-1",
                        Some(act_ref()),
                    )]));
                }
                report
            };
            Ok(match script {
                Script::Continue => with_batch(PhaseReport::continued()),
                Script::Exit(boundary) => with_batch(PhaseReport::exits(boundary)),
                Script::EndRun => PhaseReport::ends_run(AgenticOutcome::Success {
                    completion: crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                    open: Vec::new(),
                    final_state: EnvironmentState::Uninitialized,
                    iterations_used: entry.iteration,
                    artifacts: Vec::new(),
                }),
                Script::Fail => return Err(anyhow::anyhow!("the phase blew up").into()),
                // A dispatch that died AFTER the gate admitted it. The batch
                // travels with the error, which is the only path that publishes
                // one — see `PhaseFailure`.
                Script::FailMidDispatch => {
                    return Err(PhaseFailure {
                        error: anyhow::anyhow!("the transport died mid-dispatch"),
                        pending: Some(batch(vec![outward_effect(
                            "llm-1:tool:send-1",
                            Some(act_ref()),
                        )])),
                        records: event_records(
                            entry.iteration,
                            entry.phase,
                            vec![(
                                "successful_prefix".to_string(),
                                serde_json::json!({"member": 0}),
                            )],
                        ),
                    })
                },
                Script::Handover(to) => {
                    let mut report = PhaseReport::continued();
                    report.records.push(JournalAppend {
                        iteration: entry.iteration,
                        phase: entry.phase,
                        ordinal: 0,
                        body: JournalBody::OwnerTransition {
                            from_agent_id: None,
                            to_agent_id: to,
                            transition_authorization: Some("authorized".to_string()),
                        },
                    });
                    report
                },
                Script::PauseWithHandover(to) => {
                    // `WaitingForChildren` for the same reason `Script::Park`
                    // gives: it is a RESUMABLE terminal, which is the whole point
                    // here. A non-resumable one would make `replay_each` refuse
                    // the following record outright rather than clear the
                    // terminal, so the defect this pins could not arise.
                    let mut report = PhaseReport::continued();
                    report.step = PhaseStep::Return(Box::new(AgenticOutcome::WaitingForChildren {
                        child_execution_ids: vec!["child-1".to_string()],
                        last_state: EnvironmentState::Uninitialized,
                        iterations_used: entry.iteration,
                        pause_state: None,
                    }));
                    report.records.push(JournalAppend {
                        iteration: entry.iteration,
                        phase: entry.phase,
                        ordinal: 0,
                        body: JournalBody::OwnerTransition {
                            from_agent_id: None,
                            to_agent_id: to,
                            transition_authorization: None,
                        },
                    });
                    report
                },
                Script::PauseWithHandoverAndEvents { to, events } => {
                    let mut report = PhaseReport::continued();
                    report.step = PhaseStep::Return(Box::new(AgenticOutcome::WaitingForChildren {
                        child_execution_ids: vec!["child-1".to_string()],
                        last_state: EnvironmentState::Uninitialized,
                        iterations_used: entry.iteration,
                        pause_state: None,
                    }));
                    // THE EVENTS FIRST. Their index is their journal address and
                    // it must depend on nothing but their own order — see this
                    // variant's docs for whose job that actually is.
                    for (event_type, payload) in events {
                        report.records.push(
                            JournalAppend::event(
                                entry.iteration,
                                entry.phase,
                                event_type,
                                payload,
                                RecordedEventRouting::Unrecorded,
                            )
                            .expect("a fixture event fits a record"),
                        );
                    }
                    // THE TRANSITION LAST, because it is the record that is
                    // present on only some attempts.
                    report.records.push(JournalAppend {
                        iteration: entry.iteration,
                        phase: entry.phase,
                        ordinal: 0,
                        body: JournalBody::OwnerTransition {
                            from_agent_id: None,
                            to_agent_id: to,
                            transition_authorization: None,
                        },
                    });
                    report
                },
                Script::Park(children) => {
                    // `WaitingForChildren`, not `Success`: a park is a RESUMABLE
                    // terminal, and the distinction is load-bearing rather than
                    // cosmetic. `journal::replay_each` refuses any record after a
                    // non-resumable terminal, so a fixture that parked with
                    // `Success` would leave a journal that could never be
                    // appended to again — and the test would still pass, because
                    // it stops one claim short of finding out.
                    let mut report = PhaseReport::continued();
                    report.step = PhaseStep::Return(Box::new(AgenticOutcome::WaitingForChildren {
                        child_execution_ids: children.clone(),
                        last_state: EnvironmentState::Uninitialized,
                        iterations_used: entry.iteration,
                        pause_state: None,
                    }));
                    report.wait = WaitReason::children(children);
                    report
                },
                Script::JournalEvents(events) => {
                    let mut report = PhaseReport::continued();
                    for (event_type, payload) in events {
                        report.records.push(
                            // The production constructor, so a fixture cannot
                            // journal a record the store would refuse and cannot
                            // drift from the ceiling the store applies.
                            JournalAppend::event(
                                entry.iteration,
                                entry.phase,
                                event_type,
                                payload,
                                // Same reasoning: this path measures a record
                                // against the store's ceiling, not its routing.
                                RecordedEventRouting::Unrecorded,
                            )
                            .expect("a fixture event fits a record"),
                        );
                    }
                    report
                },
                Script::JournalRoutedEvents(events) => {
                    let mut report = PhaseReport::continued();
                    for (event_type, payload, routing) in events {
                        report.records.push(
                            // The production constructor, as every other arm
                            // uses — the routing is the only thing that differs.
                            JournalAppend::event(
                                entry.iteration,
                                entry.phase,
                                event_type,
                                payload,
                                routing,
                            )
                            .expect("a fixture event fits a record"),
                        );
                    }
                    report
                },
                Script::JournalNamedEvents(named) => {
                    let mut report = PhaseReport::continued();
                    for (name, payload) in named {
                        report.records.push(
                            // The production constructor, for the same reason
                            // the arm above uses it: a fixture must not be able
                            // to journal a record the store would refuse, and it
                            // must measure against the same ceiling.
                            JournalAppend::named_event(
                                entry.iteration,
                                entry.phase,
                                name,
                                FIXTURE_NAMED_AGENT,
                                Some(FIXTURE_NAMED_PRINCIPAL.to_string()),
                                Some(FIXTURE_NAMED_WORKSPACE.to_string()),
                                payload,
                            )
                            .expect("a fixture named event fits a record"),
                        );
                    }
                    report
                },
                Script::JournalBothRails { events, named } => {
                    // Built by pushing onto ONE `report.records`, which is what
                    // `commit_boundary` then stamps `ordinal: index` over — so
                    // the two rails get consecutive ordinals out of one counter,
                    // exactly as `emit_step_events_if_signaled` produces them.
                    // Two reports concatenated would restart the ordinal and
                    // would not be this shape.
                    let mut report = PhaseReport::continued();
                    for (event_type, payload) in events {
                        report.records.push(
                            JournalAppend::event(
                                entry.iteration,
                                entry.phase,
                                event_type,
                                payload,
                                RecordedEventRouting::Unrecorded,
                            )
                            .expect("a fixture event fits a record"),
                        );
                    }
                    for (name, payload) in named {
                        report.records.push(
                            JournalAppend::named_event(
                                entry.iteration,
                                entry.phase,
                                name,
                                FIXTURE_NAMED_AGENT,
                                Some(FIXTURE_NAMED_PRINCIPAL.to_string()),
                                Some(FIXTURE_NAMED_WORKSPACE.to_string()),
                                payload,
                            )
                            .expect("a fixture named event fits a record"),
                        );
                    }
                    report
                },
            })
        }

        fn emits_projected_events(&self) -> bool {
            self.transport.is_some()
        }

        fn emit_projected_event(
            &mut self,
            event: RuntimeTransportEvent,
            routing: &RecordedEventRouting,
        ) -> Result<(), EmitRefused> {
            if self.refusals_remaining > 0 {
                self.refusals_remaining -= 1;
                return Err(EmitRefused {
                    reason: "the transport is down".to_string(),
                });
            }
            // Unreachable through `project_outbox`, which asks
            // `emits_projected_events` first. Kept as a refusal rather than a
            // panic: a fixture that reached it would otherwise fail inside a
            // sink with no indication of which boundary called it.
            let Some(delivered) = self.transport.as_mut() else {
                return Err(EmitRefused {
                    reason: "this host has no transport".to_string(),
                });
            };
            delivered.push(event);
            // On the ACCEPTED path only, and after the delivery. A fake that
            // dropped this argument would record the same thing for a sink on
            // the discarding `emit_routed` default as for one that honours the
            // routing — see the field docs.
            self.routings.push(routing.clone());
            Ok(())
        }

        fn emit_projected_named_event(
            &mut self,
            name: &str,
            agent_id: &str,
            principal: Option<&str>,
            workspace: Option<&str>,
            payload: serde_json::Value,
        ) -> Result<(), EmitRefused> {
            // The SAME counter and the same order as `emit_projected_event`:
            // decrement, refuse, and only then deliver. A rail that checked the
            // counter after delivering would report a refused record as
            // delivered, and the two rails must be indistinguishable to a walk
            // that stops at the first refusal.
            if self.refusals_remaining > 0 {
                self.refusals_remaining -= 1;
                return Err(EmitRefused {
                    reason: "the transport is down".to_string(),
                });
            }
            // Gated on `transport`, not on `named_transport`, because
            // `emits_projected_events` reads `transport` — so a host with no
            // transport refuses on both rails identically, and a fixture that
            // reached this without one gets the same named refusal rather than a
            // silent accept into a vector nobody looks at.
            if self.transport.is_none() {
                return Err(EmitRefused {
                    reason: "this host has no transport".to_string(),
                });
            }
            self.named_transport.push((
                name.to_string(),
                agent_id.to_string(),
                principal.map(str::to_string),
                workspace.map(str::to_string),
                payload,
            ));
            Ok(())
        }
    }

    /// A store that records which methods were called, and can be made to fail.
    struct SpyStore {
        inner: MemoryLoopStateStore,
        calls: Mutex<Vec<&'static str>>,
        appends: AtomicUsize,
        journal_is_corrupt: bool,
        load_is_unavailable: bool,
        /// Take the emit and refuse to record that it happened.
        ///
        /// The one failure the outbox's ordering is chosen against: the events
        /// are already on a transport and the mark is not durable. It must not
        /// fail the boundary, and it must cost exactly one boundary's
        /// re-emission — which is what the case named for it asserts.
        projector_save_fails: bool,
    }

    impl SpyStore {
        fn new() -> Self {
            Self {
                inner: MemoryLoopStateStore::new(),
                calls: Mutex::new(Vec::new()),
                appends: AtomicUsize::new(0),
                journal_is_corrupt: false,
                load_is_unavailable: false,
                projector_save_fails: false,
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn note(&self, what: &'static str) {
            self.calls.lock().unwrap().push(what);
        }
    }

    #[async_trait]
    impl LoopStateStore for SpyStore {
        async fn load(
            &self,
            key: &ExecutionKey,
        ) -> super::super::store::StoreResult<Option<CommittedLoopState>> {
            self.note("load");
            if self.load_is_unavailable {
                return Err(StoreError::Unavailable {
                    detail: "the substrate is down".to_string(),
                });
            }
            self.inner.load(key).await
        }

        async fn commit(
            &self,
            key: &ExecutionKey,
            state: &LoopState,
            expected: Revision,
        ) -> super::super::store::StoreResult<Revision> {
            self.note("commit");
            self.inner.commit(key, state, expected).await
        }

        async fn append_journal(
            &self,
            key: &ExecutionKey,
            appends: &[JournalAppend],
        ) -> super::super::store::StoreResult<u64> {
            self.note("append_journal");
            self.appends.fetch_add(1, Ordering::SeqCst);
            self.inner.append_journal(key, appends).await
        }

        async fn read_journal(
            &self,
            key: &ExecutionKey,
            from_seq: u64,
        ) -> super::super::store::StoreResult<Vec<super::super::journal::JournalRecord>> {
            self.note("read_journal");
            if self.journal_is_corrupt {
                return Err(StoreError::Journal(
                    super::super::journal::JournalError::CorruptRecord {
                        line: 3,
                        reason: "a line that is not the last one failed to parse".to_string(),
                    },
                ));
            }
            self.inner.read_journal(key, from_seq).await
        }

        async fn record_effect_intent(
            &self,
            key: &ExecutionKey,
            entry: &EffectLedgerEntry,
        ) -> super::super::store::StoreResult<()> {
            self.note("record_effect_intent");
            self.inner.record_effect_intent(key, entry).await
        }

        async fn record_effect_outcome(
            &self,
            key: &ExecutionKey,
            effect_id: &EffectId,
            outcome: EffectOutcome,
        ) -> super::super::store::StoreResult<()> {
            self.note("record_effect_outcome");
            self.inner
                .record_effect_outcome(key, effect_id, outcome)
                .await
        }

        async fn load_effects(
            &self,
            key: &ExecutionKey,
        ) -> super::super::store::StoreResult<EffectLedger> {
            self.note("load_effects");
            self.inner.load_effects(key).await
        }

        async fn claim(
            &self,
            key: &ExecutionKey,
            worker: &WorkerId,
            ttl: Duration,
        ) -> super::super::store::StoreResult<Lease> {
            self.note("claim");
            self.inner.claim(key, worker, ttl).await
        }

        async fn renew(
            &self,
            lease: &Lease,
            ttl: Duration,
        ) -> super::super::store::StoreResult<Lease> {
            self.note("renew");
            self.inner.renew(lease, ttl).await
        }

        async fn release(&self, lease: Lease) -> super::super::store::StoreResult<()> {
            self.note("release");
            self.inner.release(lease).await
        }

        async fn resolve_wake(
            &self,
            key: &ExecutionKey,
            wake_token: &str,
            resolution_id: &str,
        ) -> super::super::store::StoreResult<()> {
            self.note("resolve_wake");
            self.inner
                .resolve_wake(key, wake_token, resolution_id)
                .await
        }

        async fn wake_resolutions(
            &self,
            key: &ExecutionKey,
            wake_token: &str,
        ) -> super::super::store::StoreResult<Vec<String>> {
            self.note("wake_resolutions");
            self.inner.wake_resolutions(key, wake_token).await
        }

        async fn consume_wake(
            &self,
            key: &ExecutionKey,
            wake_token: &str,
            resolution_ids: &[String],
        ) -> super::super::store::StoreResult<usize> {
            self.note("consume_wake");
            self.inner
                .consume_wake(key, wake_token, resolution_ids)
                .await
        }

        async fn list_runnable(
            &self,
            worker: &WorkerId,
            limit: usize,
        ) -> super::super::store::StoreResult<Vec<ExecutionKey>> {
            self.note("list_runnable");
            self.inner.list_runnable(worker, limit).await
        }

        // Delegated rather than left to the trait defaults. The defaults REFUSE,
        // and a spy that inherited them would silently stop projecting — so every
        // outbox case driven through this store would pass by doing nothing.
        async fn load_projector_cursor(
            &self,
            key: &ExecutionKey,
        ) -> super::super::store::StoreResult<Option<ProjectorCursor>> {
            self.note("load_projector_cursor");
            self.inner.load_projector_cursor(key).await
        }

        async fn save_projector_cursor(
            &self,
            key: &ExecutionKey,
            cursor: &ProjectorCursor,
        ) -> super::super::store::StoreResult<()> {
            self.note("save_projector_cursor");
            if self.projector_save_fails {
                return Err(StoreError::Unavailable {
                    detail: "the mark could not be written".to_string(),
                });
            }
            self.inner.save_projector_cursor(key, cursor).await
        }
    }

    // ====================================================================
    // Helpers
    // ====================================================================

    fn worker() -> WorkerId {
        WorkerId::new("worker-a")
    }

    fn config() -> WorkerConfig {
        WorkerConfig {
            lease_ttl: Duration::from_secs(30),
            max_phase_attempts: 3,
        }
    }

    async fn seed(store: &dyn LoopStateStore, key: &ExecutionKey, state: &LoopState) -> Revision {
        store
            .commit(key, state, Revision::INITIAL)
            .await
            .expect("the fixture's first commit")
    }

    async fn loaded(store: &dyn LoopStateStore, key: &ExecutionKey) -> LoopState {
        store
            .load(key)
            .await
            .expect("load")
            .expect("the fixture committed")
            .state
    }

    fn outward_effect(id: &str, reconcile_ref: Option<CommittedActRef>) -> PendingEffect {
        PendingEffect {
            effect_id: EffectId::parse(id).expect("a fixture id is well-formed"),
            tool: "gmail__send".to_string(),
            arguments_fingerprint: "fp-1".to_string(),
            retry_safety: RetrySafety::NotRetrySafe,
            reconcile_ref,
            reattach_ref: None,
        }
    }

    /// A coding dispatch as the gate would commit it: reattachable, not
    /// outward, and naming the invocation a resume goes through.
    ///
    /// The ref is a parameter so a case can express the row that names nothing —
    /// a snapshot written before the gate minted one — which is a different
    /// failure from an invocation that reported no session, and the two must not
    /// be spelled the same way.
    fn coding_effect(id: &str, reattach_ref: Option<&str>) -> PendingEffect {
        PendingEffect {
            effect_id: EffectId::parse(id).expect("a fixture id is well-formed"),
            tool: "run_coding_task".to_string(),
            arguments_fingerprint: "fp-1".to_string(),
            retry_safety: RetrySafety::Reattachable,
            reconcile_ref: None,
            reattach_ref: reattach_ref.map(str::to_string),
        }
    }

    /// The act ref an outward fixture carries, bound to the scope that derived
    /// it — because a ref without one addresses nothing, and the constructor is
    /// the only way to build the pair.
    fn act_ref() -> CommittedActRef {
        CommittedActRef::new(
            format!("act-{}", "0123456789abcdef".repeat(2)),
            "anonymous",
            "default",
        )
        .expect("the fixture ref must be the shape derive_act_ref mints")
    }

    fn batch(effects: Vec<PendingEffect>) -> PendingBatch {
        PendingBatch {
            iteration: 1,
            phase: Phase::Apply,
            mode: BatchMode::Sequential,
            effects,
        }
    }

    // ====================================================================
    // The cycle
    // ====================================================================

    #[tokio::test]
    async fn one_claim_runs_exactly_one_phase() {
        // The property the whole refactor buys. A driver that looped phases in
        // memory would pass every other test in this file and fail this one,
        // because the thing it deletes — the durable commit BETWEEN phases — is
        // invisible from the outcome of an iteration.
        let store = MemoryLoopStateStore::new();
        let key = key("one-phase");
        let state = fresh_state(&key);
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue, Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "one phase must commit, got {advanced:?}"
        );
        assert_eq!(host.phases_run(), vec![Phase::Prepare]);
        let committed = loaded(&store, &key).await;
        assert_eq!(committed.cursor.phase, Phase::Observe);
        assert_eq!(
            committed.iteration_checkpoint,
            Some(IterationCheckpoint {
                iteration: 1,
                history_iterations_len_at_start: 0,
                started_at_ms: 1_000,
            }),
            "Prepare must publish Epilogue's baselines in the same commit that exposes Observe"
        );
    }

    #[tokio::test]
    async fn the_committed_cursor_is_the_cursor_the_journal_replays_to() {
        // `next_cursor` and `journal::replay_each` are two implementations of one
        // rule. This walks a whole iteration through six separate claims and
        // asserts they agree at every one — which is the divergence check
        // `verify_journal` performs in production, run here against a driver
        // rather than against a fixture that was written to satisfy it.
        let store = MemoryLoopStateStore::new();
        let key = key("cursor-parity");
        seed(&store, &key, &fresh_state(&key)).await;
        // Apply exits; every other phase continues.
        let mut host = FakeHost::new(vec![
            Script::Continue,
            Script::Continue,
            Script::Continue,
            Script::Continue,
            Script::Exit(BoundaryOutcome::NextIteration),
            Script::Continue,
        ]);

        for _ in 0..6 {
            let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
            assert!(
                matches!(advanced, Advanced::Committed { .. }),
                "every phase of a clean iteration commits, got {advanced:?}"
            );
            let state = loaded(&store, &key).await;
            let records = store.read_journal(&key, 0).await.expect("read");
            let journal = Journal::from_records(records).expect("well-formed");
            let replayed =
                replay(journal.authoritative(state.journal_seq)).expect("the committed prefix");
            assert_eq!(
                (replayed.iteration, replayed.phase),
                (state.cursor.iteration, state.cursor.phase),
                "the committed cursor must be the one its own journal replays to"
            );
        }

        assert_eq!(
            host.phases_run(),
            vec![
                Phase::Prepare,
                Phase::Observe,
                Phase::Decide,
                Phase::Resolve,
                Phase::Apply,
                Phase::Epilogue,
            ]
        );
        let state = loaded(&store, &key).await;
        assert_eq!(
            (state.cursor.iteration, state.cursor.phase),
            (2, Phase::Prepare),
            "the epilogue is the one place the iteration counter steps"
        );
        assert!(
            state.iteration_checkpoint.is_none(),
            "the consumed iteration checkpoint must not leak into iteration two"
        );
    }

    /// The design's replay-determinism row, at the grain it is written in:
    /// *"replaying a journal reproduces the recorded `LoopState` at every seq"*.
    ///
    /// # What this asks that the test above does not
    ///
    /// `the_committed_cursor_is_the_cursor_the_journal_replays_to` replays a
    /// **growing prefix** after each claim and compares it against the state as
    /// it stands at that moment. So it can only ever check the newest seq, and
    /// only against a log that has not moved since. A resume never has that: it
    /// reads a log that has since grown past the seq it is resuming from, and
    /// asks what the state was *back there*.
    ///
    /// So this one records every commit as it happens, then reads the finished
    /// log ONCE and settles all of them against a single replay of it. A fold
    /// that was right at the end of every log and wrong in the middle passes the
    /// other test and fails this one.
    ///
    /// # And it asks about more of the state than the cursor
    ///
    /// `LoopState::identity.agent_id` is journal-derived too — the boundary
    /// writes it from an `OwnerTransition` record the phase produced — so the
    /// same log must reproduce it. A driver that applied a transition it did not
    /// journal, or journaled one it did not apply, disagrees here and nowhere
    /// else in this file.
    ///
    /// # Why the walk is five iterations and not one
    ///
    /// Every shape a boundary can take appears once: all four `BoundaryOutcome`
    /// variants, an exit from three different phases, the one report that
    /// carries a **second** record — so the watermark lands on a record that
    /// moves no cursor — and a run that ends without an epilogue. A
    /// single-shape walk would agree with a replay that mishandled the other
    /// three, and nothing would say so.
    #[tokio::test]
    async fn every_committed_state_is_reproduced_by_replaying_its_own_journal_to_that_seq() {
        // What the driver committed at one claim, kept so a single replay at
        // the end can be asked what it says about that claim's seq.
        #[derive(Debug)]
        struct CommittedObservation {
            claim: usize,
            journal_seq: u64,
            cursor: LoopCursor,
            agent_id: Option<String>,
        }

        let store = MemoryLoopStateStore::new();
        let key = key("replay-determinism");
        seed(&store, &key, &fresh_state(&key)).await;

        let mut host = FakeHost::new(vec![
            // Iteration 1 — the clean shape: `Apply` exits, the epilogue runs.
            Script::Continue,
            Script::Continue,
            Script::Continue,
            Script::Continue,
            Script::Exit(BoundaryOutcome::NextIteration),
            Script::Continue,
            // Iteration 2 — `Decide` exits, so `Resolve` and `Apply` never run.
            Script::Continue,
            Script::Continue,
            Script::Exit(BoundaryOutcome::Advance),
            Script::Continue,
            // Iteration 3 — a handover at `Prepare` (two records in one batch),
            // then a retry out of `Resolve`. The retry asks for no wait, so the
            // committed `runnable_at_ms` does not hold the next claim off.
            Script::Handover("agent-b".to_string()),
            Script::Continue,
            Script::Continue,
            Script::Exit(BoundaryOutcome::Retry(Duration::ZERO)),
            Script::Continue,
            // Iteration 4 — `Observe` pops the frame.
            Script::Continue,
            Script::Exit(BoundaryOutcome::PopFrame),
            Script::Continue,
            // Iteration 5 — the run ends at `Decide`. No epilogue.
            Script::Continue,
            Script::Continue,
            Script::EndRun,
        ]);

        let claims = 21;
        let mut observed: Vec<CommittedObservation> = Vec::with_capacity(claims);
        for claim in 1..=claims {
            let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
            let is_last = claim == claims;
            match (&advanced, is_last) {
                (Advanced::Committed { .. }, false) | (Advanced::RunEnded { .. }, true) => {},
                _ => panic!("claim {claim} produced {advanced:?}"),
            }
            let state = loaded(&store, &key).await;
            observed.push(CommittedObservation {
                claim,
                journal_seq: state.journal_seq,
                cursor: state.cursor,
                agent_id: state.identity.agent_id.clone(),
            });
        }

        // Spelled out rather than counted. The walk's whole value is which
        // phases it visited and which it skipped, and a length check would be
        // satisfied by a run that took the same number of the wrong ones.
        assert_eq!(
            host.phases_run(),
            vec![
                Phase::Prepare,
                Phase::Observe,
                Phase::Decide,
                Phase::Resolve,
                Phase::Apply,
                Phase::Epilogue,
                Phase::Prepare,
                Phase::Observe,
                Phase::Decide,
                Phase::Epilogue,
                Phase::Prepare,
                Phase::Observe,
                Phase::Decide,
                Phase::Resolve,
                Phase::Epilogue,
                Phase::Prepare,
                Phase::Observe,
                Phase::Epilogue,
                Phase::Prepare,
                Phase::Observe,
                Phase::Decide,
            ],
            "an exit lands on the epilogue; a run ending skips it"
        );

        // ONE read, ONE replay, at the end.
        let final_state = loaded(&store, &key).await;
        let records = store.read_journal(&key, 0).await.expect("read");
        let journal = Journal::from_records(records).expect("the driver wrote a well-formed log");
        let authoritative = journal.authoritative(final_state.journal_seq).to_vec();
        let cursors = super::super::journal::replay_each(&authoritative)
            .expect("the committed prefix must replay");
        assert_eq!(
            cursors.len(),
            authoritative.len(),
            "replay must answer for every record it was given"
        );
        assert!(
            authoritative.len() > claims,
            "the walk must journal at least one batch of more than one record, or the \
             watermark never lands on a record that moves no cursor and half of what this \
             test is named for is untested"
        );

        for observation in &observed {
            // Seqs are one-based and gapless, so the watermark indexes the
            // replay directly. Checked rather than assumed: an off-by-one would
            // compare every commit against its neighbour's cursor, and in a walk
            // where consecutive cursors differ that reads as a driver bug rather
            // than as a bug in this test.
            let index = usize::try_from(observation.journal_seq)
                .expect("a fixture watermark fits a usize")
                .checked_sub(1)
                .expect("every commit in this walk appended at least one record");
            let replayed = cursors[index];
            assert_eq!(
                replayed.seq, observation.journal_seq,
                "claim {} indexed the wrong record",
                observation.claim
            );
            assert_eq!(
                (replayed.iteration, replayed.phase),
                (observation.cursor.iteration, observation.cursor.phase),
                "claim {}: the state committed at seq {} is not the state its own journal \
                 replays to at that seq",
                observation.claim,
                observation.journal_seq
            );
        }

        let ended = observed.last().expect("the walk took at least one claim");
        let ended_index = usize::try_from(ended.journal_seq).expect("fits") - 1;
        assert_eq!(
            cursors[ended_index].terminal,
            Some(TerminalKind::Success),
            "how a run ended is part of what its journal replays to, not only where it stopped"
        );

        // The identity, folded from the same records a reader would fold.
        let mut owner: Option<String> = None;
        let mut owner_at_seq: Vec<Option<String>> = Vec::with_capacity(authoritative.len());
        for record in &authoritative {
            if let JournalBody::OwnerTransition { to_agent_id, .. } = &record.body {
                owner = Some(to_agent_id.clone());
            }
            owner_at_seq.push(owner.clone());
        }
        assert!(
            owner_at_seq.iter().any(|at_seq| at_seq.is_some()),
            "the walk must actually journal a handover, or the owner comparison below is \
             vacuously true for every seq"
        );
        for observation in &observed {
            let index = usize::try_from(observation.journal_seq).expect("fits") - 1;
            assert_eq!(
                owner_at_seq[index], observation.agent_id,
                "claim {}: the committed owner is not the owner this journal names at seq {}",
                observation.claim, observation.journal_seq
            );
        }

        // And the answer at a seq does not change as the log grows past it —
        // which is what entitles every comparison above to read a replay of the
        // FINISHED log rather than of the log as it stood at that claim.
        for length in 1..=authoritative.len() {
            let prefix = super::super::journal::replay_each(&authoritative[..length])
                .expect("a prefix must replay");
            assert_eq!(
                prefix.as_slice(),
                &cursors[..length],
                "replaying {length} of {} records disagreed about the first {length}",
                authoritative.len()
            );
        }
    }

    #[tokio::test]
    async fn a_run_that_ends_never_reaches_the_epilogue() {
        // The failure-mode row, at the distinction that actually exists: a
        // boundary exit lands ON the epilogue, a run ending skips it. A driver
        // that treated `Return` like `Exit` would run the stuck detector and emit
        // `IterationCompleted` for a finished run.
        let store = MemoryLoopStateStore::new();
        let key = key("terminal-skips-epilogue");
        let mut state = fresh_state(&key);
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Apply,
        };
        // The journal has to reach the cursor, or `verify_journal` quarantines
        // before anything runs — which would make this test pass for the wrong
        // reason.
        for phase in [
            Phase::Prepare,
            Phase::Observe,
            Phase::Decide,
            Phase::Resolve,
        ] {
            let seq = store
                .append_journal(
                    &key,
                    &[JournalAppend::phase_completed(
                        1,
                        phase,
                        RecordedStep::Continued,
                    )],
                )
                .await
                .expect("append");
            state.journal_seq = seq;
        }
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::EndRun]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        let Advanced::RunEnded { cursor, .. } = &advanced else {
            panic!("the run must end, got {advanced:?}");
        };
        assert_eq!(
            cursor.phase,
            Phase::Apply,
            "a run that ended stays at the phase that ended it"
        );
        assert_eq!(host.phases_run(), vec![Phase::Apply]);

        // And the run is not advanced a second time. A driver that re-ran the
        // phase here would append after its own terminal record, which
        // `replay_each` refuses on the next read — the run would advance once
        // and then be unloadable forever.
        let again = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(
                again,
                Advanced::RunAlreadyEnded {
                    terminal: TerminalKind::Success
                }
            ),
            "got {again:?}"
        );
        assert!(
            !host.phases_run().contains(&Phase::Epilogue),
            "the epilogue must never run after a terminal"
        );
    }

    #[tokio::test]
    async fn a_retry_is_committed_as_a_wake_time_rather_than_slept_on() {
        // The resident driver sleeps for the backoff. A worker that did the same
        // would hold a worker for the whole delay, which is the resident-task
        // cost this design removes. The assertion on elapsed time is what
        // separates the two: a sleeping driver takes the full ten seconds.
        let store = MemoryLoopStateStore::new();
        let key = key("retry-is-a-wake-time");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Exit(BoundaryOutcome::Retry(
            Duration::from_secs(10),
        ))]);

        let before = Instant::now();
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        let elapsed = before.elapsed();

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "the driver must not sleep the backoff; it took {elapsed:?}"
        );
        let state = loaded(&store, &key).await;
        let now_ms = Utc::now().timestamp_millis();
        assert!(
            state.runnable_at_ms > now_ms + 8_000,
            "the backoff must be committed as a wake time, got {} against now {now_ms}",
            state.runnable_at_ms
        );
        assert!(!state.is_runnable_at(now_ms));

        let again = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(again, Advanced::NotYetRunnable { .. }),
            "the wake time must hold the next claim off, got {again:?}"
        );
        assert_eq!(
            host.phases_run(),
            vec![Phase::Prepare],
            "no second phase may run before the backoff elapses"
        );
    }

    // ====================================================================
    // Crash points
    // ====================================================================

    #[tokio::test]
    async fn a_stale_worker_commits_nothing_and_leaves_only_orphans() {
        // "Lease expires mid-phase → work is discarded" and "stale worker
        // commits → rejected by CAS on Revision" are the same event seen from
        // two sides. Another worker commits while this one is mid-phase; this
        // one's commit must be refused and its journal records must sit above
        // the watermark, where the next append sweeps them.
        let store = MemoryLoopStateStore::new();
        let key = key("stale-commit");
        let state = fresh_state(&key);
        let revision = seed(&store, &key, &state).await;

        // The other worker advances while ours is mid-phase.
        let mut theirs = state.clone();
        theirs.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Observe,
        };
        let seq = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("their append");
        theirs.journal_seq = seq;
        let theirs_revision = store
            .commit(&key, &theirs, revision)
            .await
            .expect("their commit");

        // Ours loaded the older revision. Reproduce that by driving from it.
        let mut host = FakeHost::new(vec![Script::Continue]);
        let mut lease = store
            .claim(&key, &worker(), Duration::from_secs(5))
            .await
            .expect("claim");
        let advanced = commit_boundary(
            &store,
            &mut host,
            &key,
            &worker(),
            &config(),
            state,
            revision,
            &mut lease,
            Phase::Prepare,
            1,
            PhaseReport::continued(),
            0,
        )
        .await;

        assert!(
            matches!(advanced, Advanced::Refused(Refusal::StaleCommit { .. })),
            "a stale commit must be refused, got {advanced:?}"
        );
        let current = loaded(&store, &key).await;
        assert_eq!(
            store.load(&key).await.unwrap().unwrap().revision,
            theirs_revision,
            "the store still holds the other worker's commit"
        );
        let records = store.read_journal(&key, 0).await.expect("read");
        let journal = Journal::from_records(records).expect("well-formed");
        assert_eq!(
            journal.authoritative(current.journal_seq).len(),
            1,
            "only the other worker's record is authoritative"
        );
        assert_eq!(
            journal.orphaned(current.journal_seq).len(),
            1,
            "ours is an orphan the next append sweeps"
        );
    }

    #[tokio::test]
    async fn an_orphaned_attempt_is_never_replayed() {
        // A dead worker left a record above the watermark. The next worker must
        // load and run normally. A driver that replayed `all_records` instead of
        // `authoritative` would see the orphan, compute a cursor one phase ahead
        // of the commit, and quarantine a perfectly healthy run.
        let store = MemoryLoopStateStore::new();
        let key = key("orphans-are-not-history");
        seed(&store, &key, &fresh_state(&key)).await;
        store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("the attempt that never commits");

        let mut host = FakeHost::new(vec![Script::Continue]);
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "an orphan must not stop the next worker, got {advanced:?}"
        );
        assert_eq!(host.phases_run(), vec![Phase::Prepare]);
    }

    /// The design's crash-injection row, at the half nothing else asserted:
    /// *"kill at each commit boundary; **assert resume produces the same
    /// terminal outcome**"*.
    ///
    /// # What was already covered, and what was not
    ///
    /// `store::contract::a_crash_at_every_commit_boundary_resumes_at_the_same_cursor`
    /// kills at all six boundaries and asserts the resumed **cursor**. The
    /// effect half — that nothing non-retry-safe fires twice — is the ledger
    /// tests further down this file. Neither says the resumed run reaches the
    /// same END, and a driver that resumed at a correct cursor and then took a
    /// different path through the remaining phases satisfies both of them.
    ///
    /// That is the property the whole design rests on: the point of committing
    /// between phases is that a run outlives the process, and "outlives" means
    /// it finishes the same way, not merely that it restarts in the right place.
    ///
    /// # What "kill" means here
    ///
    /// What it means in production. The worker committed boundary *k*, appended
    /// the record for phase *k+1*, and died before committing it. The store
    /// keeps the orphan, the watermark does not cover it, and the next attempt
    /// sweeps it. A fresh host then picks the run up — rebuilt rather than
    /// reused, because a resumed run is a new process. [`FakeHost`]'s script is
    /// per-phase-call and deterministic, so the replacement is handed exactly
    /// the calls the dead one had not made.
    ///
    /// # What this does NOT cover, because a fake host cannot
    ///
    /// The real `InProcessWorkerHost` in `executor.rs` carries an
    /// `IterationCarry` — `Resolve`'s output is handed to `Apply` in memory and
    /// no `LoopState` holds it — so a real process resuming mid-iteration
    /// answers `carry_missing` rather than reaching this terminal. That is a
    /// stated limitation of the host, documented at `carry_missing` itself, and
    /// this test does not paper over it: what it pins is the **driver's** half,
    /// which is the half a different host would depend on. Read the two
    /// together before concluding that a crashed production run resumes.
    ///
    /// # Why the same worker id on both sides
    ///
    /// A dead worker's lease is a separate property with its own tests
    /// (`a_stale_worker_commits_nothing_and_leaves_only_orphans`, and the lease
    /// cases in the store contract). Reusing the id keeps this test about where
    /// the run ends rather than about a TTL.
    /// REGRESSION GUARD, at the DRIVER rather than at replay.
    ///
    /// `journal::tests::a_terminal_is_not_cleared_by_a_record_from_its_own_batch`
    /// pins what `replay_each` does with each ordering, but it builds its own
    /// records — so it stays green whatever the driver writes. This one drives
    /// the real `commit_boundary` and reads back the journal it actually wrote.
    ///
    /// The defect: a phase that pauses AND journals an owner transition used to
    /// emit `[RunEnded{WaitingForConfirmation}, OwnerTransition]`, because the
    /// completion record was pushed first. `replay_each` clears a resumable
    /// terminal as soon as any record follows it — correct for a genuine
    /// resumption, wrong for a sibling in the same batch — so the pause
    /// evaporated, `verify_journal` answered `Ok(None)`, and the next claim
    /// re-entered the phase that had just paused. Nothing refused, nothing
    /// logged.
    ///
    /// **What production change leaves this green?** Only one that keeps a
    /// run-ending completion record last in its batch. Restoring the
    /// completion-first order turns the replayed terminal to `None` and fails
    /// the second assertion.
    #[tokio::test]
    async fn a_pause_that_also_hands_over_is_still_a_pause_when_replayed() {
        let store = MemoryLoopStateStore::new();
        let key = key("pause-with-handover");
        let state = fresh_state(&key);
        seed(&store, &key, &state).await;

        let mut host = FakeHost::new(vec![Script::PauseWithHandover("agent-b".to_string())]);
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::RunEnded { .. }),
            "the scripted phase pauses, so the driver must report the run ended: {advanced:?}"
        );

        let records = store
            .read_journal(&key, 0)
            .await
            .expect("journal reads back");
        assert_eq!(
            records.len(),
            2,
            "one completion record and one owner transition: {records:?}"
        );

        // The order the driver chose, asserted directly — this is the fix.
        assert!(
            matches!(records[0].body, JournalBody::OwnerTransition { .. }),
            "what happened DURING the phase is written before the ending it reached: {records:?}"
        );

        let cursor = super::super::journal::replay(&records).expect("the batch replays");
        assert_eq!(
            cursor.terminal,
            Some(super::super::journal::TerminalKind::WaitingForChildren),
            "the run paused; a replay that loses the terminal reads it as live and the next \
             claim re-enters the phase that paused"
        );
    }

    #[tokio::test]
    async fn a_crash_at_any_commit_boundary_still_reaches_the_same_terminal() {
        // Two turns, because one cannot reach the transition that matters. The
        // first continues — six phases, its exit landing on the epilogue — and
        // the second ends the run at `Apply`. Eleven phase calls, so ten
        // interior boundaries to be killed at, one of which is the boundary
        // where the iteration counter steps.
        fn script() -> Vec<Script> {
            vec![
                Script::Continue,                             // 1 Prepare
                Script::Continue,                             // 1 Observe
                Script::Continue,                             // 1 Decide
                Script::Continue,                             // 1 Resolve
                Script::Exit(BoundaryOutcome::NextIteration), // 1 Apply
                Script::Continue,                             // 1 Epilogue
                Script::Continue,                             // 2 Prepare
                Script::Continue,                             // 2 Observe
                Script::Continue,                             // 2 Decide
                Script::Continue,                             // 2 Resolve
                Script::EndRun,                               // 2 Apply
            ]
        }

        /// Claim until the run ends, or until `stop_after` boundaries have
        /// committed. `None` means it was stopped rather than ended.
        ///
        /// Anything other than a commit or a terminal panics rather than
        /// returning: a scripted run has no lease contention, no retry delay and
        /// no park, so every other `Advanced` variant is a driver answering a
        /// question this fixture did not ask — and silently treating one as "not
        /// ended" would turn a real regression into a passing loop.
        async fn drive(
            store: &dyn LoopStateStore,
            host: &mut FakeHost,
            key: &ExecutionKey,
            stop_after: Option<usize>,
        ) -> Option<(TerminalKind, usize, LoopCursor)> {
            let mut commits = 0usize;
            loop {
                if Some(commits) == stop_after {
                    return None;
                }
                match advance_once(store, host, key, &worker(), &config()).await {
                    Advanced::Committed { .. } => commits += 1,
                    Advanced::RunEnded {
                        cursor,
                        outcome,
                        terminal,
                        ..
                    } => return Some((terminal, outcome.iterations_used(), cursor)),
                    other => panic!(
                        "a scripted run must only commit or end; got {other:?} after {commits} \
                         commits"
                    ),
                }
            }
        }

        // ── The baseline: the same script, nothing killed ───────────────────
        let store = MemoryLoopStateStore::new();
        let baseline_key = key("resume-equivalence-baseline");
        seed(&store, &baseline_key, &fresh_state(&baseline_key)).await;
        let mut baseline_host = FakeHost::new(script());
        let baseline = drive(&store, &mut baseline_host, &baseline_key, None)
            .await
            .expect("the uninterrupted run must reach a terminal");
        let baseline_phases = baseline_host.phases_run();
        assert_eq!(
            baseline_phases.len(),
            script().len(),
            "the baseline must run each scripted phase exactly once, or the comparisons below \
             are against the wrong run"
        );

        // ── And once more per interior boundary, killed at that one ─────────
        for kill_after in 1..script().len() {
            let store = MemoryLoopStateStore::new();
            let key = key(&format!("resume-equivalence-{kill_after}"));
            seed(&store, &key, &fresh_state(&key)).await;

            let mut dying = FakeHost::new(script());
            assert!(
                drive(&store, &mut dying, &key, Some(kill_after))
                    .await
                    .is_none(),
                "the run ended before boundary {kill_after}, so nothing was killed there"
            );

            // ...and here the worker dies, having appended the next phase's
            // record and never committed it.
            let resumed_at = loaded(&store, &key).await.cursor;
            store
                .append_journal(
                    &key,
                    &[JournalAppend::phase_completed(
                        resumed_at.iteration,
                        resumed_at.phase,
                        RecordedStep::Continued,
                    )],
                )
                .await
                .expect("the attempt that never commits");

            let mut resumed =
                FakeHost::new(script().into_iter().skip(kill_after).collect::<Vec<_>>());
            let after_crash = drive(&store, &mut resumed, &key, None)
                .await
                .expect("the resumed run must reach a terminal");

            assert_eq!(
                after_crash, baseline,
                "killing the worker after boundary {kill_after} — resuming at {resumed_at:?} — \
                 changed the terminal, the iteration count, or where the run ended"
            );

            let mut phases = dying.phases_run();
            phases.extend(resumed.phases_run());
            assert_eq!(
                phases, baseline_phases,
                "killing the worker after boundary {kill_after} changed which phases ran, or how \
                 many times each did. A phase that appears twice here is one whose commit did \
                 not survive its own crash"
            );
        }
    }

    #[tokio::test]
    async fn a_corrupt_journal_quarantines_before_any_work_runs() {
        let store = SpyStore {
            journal_is_corrupt: true,
            ..SpyStore::new()
        };
        let key = key("corrupt-journal");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(
                advanced,
                Advanced::Quarantined(Quarantine::CorruptJournal { .. })
            ),
            "got {advanced:?}"
        );
        assert!(
            host.phases_run().is_empty(),
            "quarantine must precede the work, not follow it"
        );
        assert_eq!(
            store.appends.load(Ordering::SeqCst),
            0,
            "a quarantined execution's bytes are retained by not writing to them"
        );
        assert!(
            store.calls().contains(&"release"),
            "the lease must be given back"
        );
    }

    #[tokio::test]
    async fn a_store_that_cannot_answer_refuses_to_advance() {
        // Fail closed: refuse to advance rather than run unrecorded work.
        let store = SpyStore {
            load_is_unavailable: true,
            ..SpyStore::new()
        };
        let key = key("store-down");
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(
                advanced,
                Advanced::Refused(Refusal::Store { during: "load", .. })
            ),
            "got {advanced:?}"
        );
        assert!(host.phases_run().is_empty());
    }

    #[tokio::test]
    async fn exactly_one_journal_append_happens_per_boundary() {
        // The store's own contract: a second append issued before the commit
        // that covers the first cannot be told from a dead worker's attempt, and
        // is swept.
        let store = SpyStore::new();
        let key = key("one-append");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let _ = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert_eq!(store.appends.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_cold_resolve_without_a_checkpoint_rewinds_to_observe() {
        let store = MemoryLoopStateStore::new();
        let key = key("cold-resolve-reobserve");
        let mut state = fresh_state(&key);
        let mut seq = 0;
        for phase in [Phase::Prepare, Phase::Observe, Phase::Decide] {
            seq = store
                .append_journal(
                    &key,
                    &[JournalAppend::phase_completed(
                        1,
                        phase,
                        RecordedStep::Continued,
                    )],
                )
                .await
                .expect("append prefix");
        }
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Resolve,
        };
        state.journal_seq = seq;
        state.resolve_checkpoint = None;
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.live_resolve_carry = false;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(
                advanced,
                Advanced::RecoveryRewound {
                    cursor: LoopCursor {
                        phase: Phase::Observe,
                        ..
                    },
                    ..
                }
            ),
            "a cold browser/credential/Primitive decision must be re-observed, got {advanced:?}"
        );
        assert!(
            host.phases_run().is_empty(),
            "Resolve must not run from missing carry"
        );
        let committed = loaded(&store, &key).await;
        assert_eq!(committed.cursor.phase, Phase::Observe);
        let journal = store.read_journal(&key, 0).await.expect("journal");
        assert!(matches!(
            journal.last().map(|record| &record.body),
            Some(JournalBody::RecoveryRewind { to: Phase::Observe })
        ));
    }

    #[tokio::test]
    async fn a_decide_cursor_without_live_carry_always_rewinds_to_observe() {
        // A crash immediately after an ordinary Observe commit predates
        // `cold_reobserve`: the committed cursor is Decide, but a reconstructed
        // host has no observation. Restricting this rewind to an already-cold
        // state enters Decide and charges `carry_missing` instead of recovering.
        let store = MemoryLoopStateStore::new();
        let key = key("cold-decide-reobserve");
        let mut state = fresh_state(&key);
        let mut seq = 0;
        for phase in [Phase::Prepare, Phase::Observe] {
            seq = store
                .append_journal(
                    &key,
                    &[JournalAppend::phase_completed(
                        1,
                        phase,
                        RecordedStep::Continued,
                    )],
                )
                .await
                .expect("append prefix");
        }
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Decide,
        };
        state.journal_seq = seq;
        state.cold_reobserve = false;
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.live_decide_carry = false;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(matches!(
            advanced,
            Advanced::RecoveryRewound {
                cursor: LoopCursor {
                    phase: Phase::Observe,
                    ..
                },
                ..
            }
        ));
        assert!(host.phases_run().is_empty(), "Decide must not run cold");
        let committed = loaded(&store, &key).await;
        assert!(committed.cold_reobserve);
        assert_eq!(committed.cursor.phase, Phase::Observe);
    }

    #[tokio::test]
    async fn a_cold_epilogue_uses_the_iteration_bound_checkpoint_once() {
        let store = MemoryLoopStateStore::new();
        let key = key("cold-epilogue-checkpoint");
        let mut state = fresh_state(&key);
        let mut seq = 0;
        for (phase, step) in [
            (Phase::Prepare, RecordedStep::Continued),
            (Phase::Observe, RecordedStep::Continued),
            (Phase::Decide, RecordedStep::Continued),
            (Phase::Resolve, RecordedStep::Continued),
            (
                Phase::Apply,
                RecordedStep::Exited {
                    boundary: RecordedBoundary::NextIteration,
                },
            ),
        ] {
            seq = store
                .append_journal(&key, &[JournalAppend::phase_completed(1, phase, step)])
                .await
                .expect("append prefix");
        }
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Epilogue,
        };
        state.journal_seq = seq;
        state.iteration_checkpoint = Some(IterationCheckpoint {
            iteration: 1,
            history_iterations_len_at_start: 4,
            started_at_ms: 1_000,
        });
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(matches!(
            advanced,
            Advanced::Committed {
                cursor: LoopCursor {
                    iteration: 2,
                    phase: Phase::Prepare,
                },
                ..
            }
        ));
        assert_eq!(host.phases_run(), vec![Phase::Epilogue]);
        assert!(loaded(&store, &key).await.iteration_checkpoint.is_none());
    }

    #[tokio::test]
    async fn an_epilogue_without_its_checkpoint_fails_before_phase_entry() {
        let store = MemoryLoopStateStore::new();
        let key = key("cold-epilogue-missing-checkpoint");
        let mut state = fresh_state(&key);
        let mut seq = 0;
        for (phase, step) in [
            (Phase::Prepare, RecordedStep::Continued),
            (Phase::Observe, RecordedStep::Continued),
            (Phase::Decide, RecordedStep::Continued),
            (Phase::Resolve, RecordedStep::Continued),
            (
                Phase::Apply,
                RecordedStep::Exited {
                    boundary: RecordedBoundary::NextIteration,
                },
            ),
        ] {
            seq = store
                .append_journal(&key, &[JournalAppend::phase_completed(1, phase, step)])
                .await
                .expect("append prefix");
        }
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Epilogue,
        };
        state.journal_seq = seq;
        state.iteration_checkpoint = None;
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(matches!(advanced, Advanced::ExecutionFailed { reason }
            if reason.contains("no matching durable iteration checkpoint")));
        assert!(host.phases_run().is_empty());
    }

    // ====================================================================
    // Committed effects
    // ====================================================================

    async fn apply_state(store: &dyn LoopStateStore, key: &ExecutionKey) -> LoopState {
        let mut state = fresh_state(key);
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Apply,
        };
        let mut seq = 0;
        for phase in [
            Phase::Prepare,
            Phase::Observe,
            Phase::Decide,
            Phase::Resolve,
        ] {
            seq = store
                .append_journal(
                    key,
                    &[JournalAppend::phase_completed(
                        1,
                        phase,
                        RecordedStep::Continued,
                    )],
                )
                .await
                .expect("append");
        }
        state.journal_seq = seq;
        state
    }

    #[tokio::test]
    async fn cold_apply_without_a_checkpoint_preserves_its_batch_until_reconstructed_apply() {
        let store = MemoryLoopStateStore::new();
        let key = key("cold-apply-reobserve-batch");
        let mut state = apply_state(&store, &key).await;
        let committed_batch = batch(vec![
            outward_effect("llm-1:tool:send-1", Some(act_ref())),
            outward_effect("llm-1:tool:send-2", Some(act_ref())),
        ]);
        state.pending = Some(committed_batch.clone());
        state.resolve_checkpoint = None;
        let revision = seed(&store, &key, &state).await;
        let mut lease = store
            .claim(&key, &worker(), Duration::from_secs(30))
            .await
            .expect("claim");
        let mut host = FakeHost::new(Vec::new());
        host.live_apply_carry = false;

        let advanced = rewind_to_observe(
            &store,
            &mut host,
            &key,
            &worker(),
            &mut lease,
            state,
            revision,
            30,
        )
        .await;
        let Advanced::RecoveryRewound { mut revision, .. } = advanced else {
            panic!("cold Apply must durably re-observe, got {advanced:?}");
        };
        let rewound = loaded(&store, &key).await;
        assert_eq!(rewound.cursor.phase, Phase::Observe);
        assert!(rewound.cold_reobserve);
        assert_eq!(rewound.pending.as_ref(), Some(&committed_batch));

        for phase in [Phase::Observe, Phase::Decide, Phase::Resolve] {
            let current = loaded(&store, &key).await;
            let advanced = commit_boundary(
                &store,
                &mut host,
                &key,
                &worker(),
                &config(),
                current,
                revision,
                &mut lease,
                phase,
                1,
                PhaseReport::continued(),
                40,
            )
            .await;
            let Advanced::Committed {
                revision: committed_revision,
                ..
            } = advanced
            else {
                panic!("{phase:?} must carry the recovery batch, got {advanced:?}");
            };
            revision = committed_revision;
            assert_eq!(
                loaded(&store, &key).await.pending.as_ref(),
                Some(&committed_batch),
                "{phase:?} must preserve the exact complete-set activation marker"
            );
        }

        let recovered_apply = loaded(&store, &key).await;
        assert_eq!(recovered_apply.cursor.phase, Phase::Apply);
        assert!(recovered_apply.cold_reobserve);
        assert_eq!(recovered_apply.pending.as_ref(), Some(&committed_batch));
    }

    #[test]
    fn recovery_batch_merge_is_monotonic_only_for_the_committed_act_ref() {
        let recorded_ref = act_ref();
        let mut recorded = batch(vec![outward_effect(
            "llm-1:tool:send-1",
            Some(recorded_ref.clone()),
        )]);
        let offered_without_ref = batch(vec![outward_effect("llm-1:tool:send-1", None)]);
        let merged = merge_re_gated_batch(&recorded, &offered_without_ref)
            .expect("an older omission retains the committed ref");
        assert_eq!(
            merged.effects[0].reconcile_ref.as_ref(),
            Some(&recorded_ref)
        );

        recorded.effects[0].reconcile_ref = None;
        let offered_with_ref = batch(vec![outward_effect(
            "llm-1:tool:send-1",
            Some(recorded_ref.clone()),
        )]);
        let merged = merge_re_gated_batch(&recorded, &offered_with_ref)
            .expect("a newer gate may fill a legacy absence");
        assert_eq!(
            merged.effects[0].reconcile_ref.as_ref(),
            Some(&recorded_ref)
        );

        recorded.effects[0].reconcile_ref = Some(recorded_ref);
        let different_ref = CommittedActRef::new(
            format!("act-{}", "fedcba9876543210".repeat(2)),
            "anonymous",
            "default",
        )
        .expect("fixture act ref");
        let conflicting = batch(vec![outward_effect(
            "llm-1:tool:send-1",
            Some(different_ref),
        )]);
        assert!(merge_re_gated_batch(&recorded, &conflicting).is_err());

        let different_effect = batch(vec![outward_effect(
            "llm-1:tool:send-2",
            recorded.effects[0].reconcile_ref.clone(),
        )]);
        assert!(merge_re_gated_batch(&recorded, &different_effect).is_err());
    }

    #[tokio::test]
    async fn cold_recovery_retains_the_complete_batch_marker_through_resolve() {
        let store = MemoryLoopStateStore::new();
        let key = key("cold-recovery-batch-marker");
        let mut state = apply_state(&store, &key).await;
        let committed_batch = batch(vec![
            outward_effect("llm-1:tool:send-1", Some(act_ref())),
            outward_effect("llm-1:tool:send-2", Some(act_ref())),
        ]);
        state.pending = Some(committed_batch.clone());
        state.resolve_checkpoint = Some(
            ResolveCheckpoint::try_new(serde_json::json!({"fixture": "pre-resolve"}))
                .expect("bounded current checkpoint"),
        );
        let revision = seed(&store, &key, &state).await;
        for effect in &committed_batch.effects {
            store
                .record_effect_intent(
                    &key,
                    &EffectLedgerEntry::intent(effect, 1, Phase::Apply, 10),
                )
                .await
                .expect("prepared member row");
        }
        store
            .record_effect_outcome(
                &key,
                &committed_batch.effects[0].effect_id,
                EffectOutcome::Succeeded { at_ms: 20 },
            )
            .await
            .expect("one member settled before the crash");

        let mut lease = store
            .claim(&key, &worker(), Duration::from_secs(30))
            .await
            .expect("claim");
        let mut host = FakeHost::new(Vec::new());
        let advanced = rewind_apply_to_resolve(
            &store,
            &mut host,
            &key,
            &worker(),
            &mut lease,
            state,
            revision,
            30,
        )
        .await;
        let Advanced::RecoveryRewound { revision, .. } = advanced else {
            panic!("cold Apply must rewind durably, got {advanced:?}");
        };
        let rewound = loaded(&store, &key).await;
        assert_eq!(rewound.cursor.phase, Phase::Resolve);
        assert_eq!(
            rewound.pending.as_ref(),
            Some(&committed_batch),
            "the marker, not the unsettled subset, activates the prepared rows"
        );

        let mut report = PhaseReport::continued();
        report.resolve_checkpoint = rewound.resolve_checkpoint.clone();
        let advanced = commit_boundary(
            &store,
            &mut host,
            &key,
            &worker(),
            &config(),
            rewound,
            revision,
            &mut lease,
            Phase::Resolve,
            1,
            report,
            40,
        )
        .await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "recovery Resolve must return to Apply, got {advanced:?}"
        );
        let recovered_apply = loaded(&store, &key).await;
        assert_eq!(recovered_apply.cursor.phase, Phase::Apply);
        assert_eq!(recovered_apply.pending.as_ref(), Some(&committed_batch));

        host.reconcile_answer = ReconcileAnswer::Fired;
        let (resolved, _) =
            resolve_effects(&store, &mut host, &key, &recovered_apply, &mut lease, 50)
                .await
                .expect("the complete activated batch resolves member by member");
        assert_eq!(resolved.len(), 2);
        assert!(matches!(&resolved[0].action, EffectAction::Adopt { .. }));
        assert!(matches!(
            &resolved[1].action,
            EffectAction::AlreadyFired { .. }
        ));
    }

    #[tokio::test]
    async fn a_failed_recovery_gate_cannot_replace_the_complete_marker_with_a_prefix() {
        let store = MemoryLoopStateStore::new();
        let key = key("recovery-marker-prefix");
        let mut state = apply_state(&store, &key).await;
        let complete = batch(vec![
            outward_effect("llm-1:tool:send-1", Some(act_ref())),
            outward_effect("llm-1:tool:send-2", Some(act_ref())),
        ]);
        state.pending = Some(complete.clone());
        let revision = seed(&store, &key, &state).await;
        let mut lease = store
            .claim(&key, &worker(), Duration::from_secs(30))
            .await
            .expect("claim");
        let mut host = FakeHost::new(Vec::new());
        let prefix = batch(vec![complete.effects[0].clone()]);

        let advanced = commit_failed_attempt(
            &store,
            &mut host,
            &key,
            &worker(),
            state,
            revision,
            &mut lease,
            Phase::Apply,
            PhaseFailure {
                error: anyhow!("the re-gate failed after its first member"),
                pending: Some(prefix),
                records: Vec::new(),
            },
            50,
        )
        .await;
        assert!(matches!(advanced, Advanced::ExecutionFailed { .. }));
        assert_eq!(
            loaded(&store, &key).await.pending.as_ref(),
            Some(&complete),
            "a failure report is not an activation CAS and cannot shrink the complete set"
        );
    }

    #[tokio::test]
    async fn a_committed_send_with_no_result_is_indeterminate_not_re_fired() {
        let store = MemoryLoopStateStore::new();
        let key = key("send-indeterminate");
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![outward_effect(
            "llm-1:tool:send-1",
            Some(act_ref()),
        )]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.reconcile_answer = ReconcileAnswer::Surface;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::EffectIndeterminate { .. }),
            "got {advanced:?}"
        );
        assert!(
            host.phases_run().is_empty(),
            "Apply must not re-run while a send's fate is unknown"
        );
        assert_eq!(
            host.reconciled.lock().unwrap().len(),
            1,
            "the committed act ref is what was asked, and it was asked once"
        );
    }

    #[tokio::test]
    async fn an_operator_can_durably_adopt_an_indeterminate_effect_as_succeeded() {
        let store = MemoryLoopStateStore::new();
        let key = key("effect-resolution-adopt");
        let pending = outward_effect("llm-1:tool:send-1", Some(act_ref()));
        let effect_id = pending.effect_id.clone();
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![pending]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.effect_resolutions
            .insert(effect_id.to_string(), EffectResolution::AdoptSucceeded);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "an adopted effect lets Apply advance, got {advanced:?}"
        );
        assert_eq!(
            host.entries.lock().unwrap()[0],
            vec![ResolvedEffect {
                effect_id: effect_id.clone(),
                action: EffectAction::Adopt {
                    result: None,
                    succeeded: true,
                },
            }]
        );
        let ledger = store.load_effects(&key).await.expect("load effects");
        assert!(matches!(
            ledger
                .get(&effect_id)
                .and_then(|entry| entry.outcome.as_ref()),
            Some(EffectOutcome::Succeeded { .. })
        ));
    }

    #[tokio::test]
    async fn an_authorized_refire_is_rearmed_before_an_unsplit_host_can_dispatch() {
        let store = MemoryLoopStateStore::new();
        let key = key("effect-resolution-refire");
        let pending = outward_effect("llm-1:tool:send-1", Some(act_ref()));
        let effect_id = pending.effect_id.clone();
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![pending]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.effect_resolutions
            .insert(effect_id.to_string(), EffectResolution::AuthorizeRefire);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert!(matches!(
            host.entries.lock().unwrap()[0][0].action,
            EffectAction::Refire
        ));
        let ledger = store.load_effects(&key).await.expect("load effects");
        assert!(
            ledger
                .get(&effect_id)
                .expect("resolution materialises an intent row")
                .outcome
                .is_none(),
            "NotDispatched licenses the plan, then must be re-armed before the opaque phase can \
             fire; leaving it behind would make a crash look like positive no-fire evidence"
        );
    }

    #[tokio::test]
    async fn a_dispatch_that_was_never_outward_is_surfaced_without_asking_the_record() {
        // Decision 1's trap. `reconcile_ref: None` means "not outward"; reading
        // it as "nothing left" answers `SafeToRefire` for every effect that
        // merely had no ref to carry, which re-sends live messages. The
        // assertion that no reconciliation was attempted is what distinguishes
        // this driver from one that substituted a re-derived ref.
        let store = MemoryLoopStateStore::new();
        let key = key("not-outward");
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![outward_effect("llm-1:tool:send-1", None)]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        // Rigged so that ASKING would license a re-fire. If the driver asked,
        // this test would see a committed phase instead of an indeterminate.
        host.reconcile_answer = ReconcileAnswer::Safe;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::EffectIndeterminate { .. }),
            "absent is not evidence, got {advanced:?}"
        );
        assert!(
            host.reconciled.lock().unwrap().is_empty(),
            "there is no ref to ask by, and no substitute may be derived"
        );
        assert!(host.phases_run().is_empty());
    }

    #[tokio::test]
    async fn a_re_fire_whose_arguments_drifted_fails_the_execution() {
        let store = MemoryLoopStateStore::new();
        let key = key("fingerprint-drift");
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![outward_effect(
            "llm-1:tool:send-1",
            Some(act_ref()),
        )]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.reconcile_answer = ReconcileAnswer::Safe;
        host.rederived = Some(RederivedDispatch {
            tool: "gmail__send".to_string(),
            arguments_fingerprint: "fp-DIFFERENT".to_string(),
        });

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::ExecutionFailed { .. }),
            "a drifted re-derivation must fail the execution rather than fire, got {advanced:?}"
        );
        assert!(host.phases_run().is_empty());
    }

    /// The reattach rule, end to end: a row that names an invocation, an
    /// invocation that reported a session, and a phase told to resume THAT
    /// session.
    ///
    /// The assertion is on the resolved session id and not on "did not go
    /// indeterminate". A negative assertion would stay green against a resolver
    /// that answered the newest invocation in the ledger — which is a resume
    /// that speaks to a real session belonging to a different dispatch, and is
    /// the failure mode the invocation-id ref exists to close.
    #[tokio::test]
    async fn a_reattachable_effect_resumes_the_session_its_own_invocation_reported() {
        let store = MemoryLoopStateStore::new();
        let key = key("reattach-resolves");
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![coding_effect(
            "llm-1:tool:code-1",
            Some("cinv-this-effects-own-job"),
        )]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        // Two invocations on the ledger, so an answer that ignored the ref has
        // somewhere wrong to land.
        host.reattach_sessions.insert(
            "cinv-this-effects-own-job".to_string(),
            "sess-mine".to_string(),
        );
        host.reattach_sessions.insert(
            "cinv-some-other-job".to_string(),
            "sess-someone-elses".to_string(),
        );

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        let entries = host.entries.lock().unwrap();
        assert_eq!(
            entries[0],
            vec![ResolvedEffect {
                effect_id: EffectId::parse("llm-1:tool:code-1").unwrap(),
                action: EffectAction::Reattach {
                    native_session_id: "sess-mine".to_string()
                },
            }],
            "the phase must resume the session this effect's own invocation reported"
        );
        drop(entries);
        assert_eq!(
            host.reattach_asked.lock().unwrap().as_slice(),
            [(
                Some(key.execution_id().to_string()),
                "cinv-this-effects-own-job".to_string()
            )],
            "the lookup is scoped by the LOADED run's identity, not by whatever run the host last \
             ran — `resolve_effects` runs before `adopt_owner`"
        );
    }

    /// An invocation that never reported a session stays indeterminate.
    ///
    /// The run is held rather than re-fired, and that is the point: re-firing a
    /// coding job runs it a second time against a real repository. The ledger
    /// row is well-formed and names a real job — the missing half is the
    /// session, which the worker died before writing.
    #[tokio::test]
    async fn a_reattachable_effect_whose_job_reported_no_session_is_held() {
        let store = MemoryLoopStateStore::new();
        let key = key("reattach-no-session");
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![coding_effect(
            "llm-1:tool:code-1",
            Some("cinv-prepared-but-silent"),
        )]));
        seed(&store, &key, &state).await;
        // A session for a DIFFERENT invocation, so a resolver that reached for
        // "the one we have" would find one.
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.reattach_sessions.insert(
            "cinv-a-different-job".to_string(),
            "sess-not-ours".to_string(),
        );

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        let Advanced::EffectIndeterminate { effect_id, reason } = advanced else {
            panic!("a job that reported no session must hold the run, got {advanced:?}");
        };
        assert_eq!(effect_id, EffectId::parse("llm-1:tool:code-1").unwrap());
        assert!(
            reason.contains("cinv-prepared-but-silent"),
            "the reason must name the invocation an operator has to go and look at, got {reason}"
        );
        assert!(
            host.phases_run().is_empty(),
            "nothing may run on a held run"
        );
    }

    #[tokio::test]
    async fn an_already_settled_coding_invocation_is_never_reattached() {
        let store = MemoryLoopStateStore::new();
        let key = key("reattach-settled");
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![coding_effect(
            "llm-1:tool:code-1",
            Some("cinv-already-settled"),
        )]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.settled_reattach_refs
            .insert("cinv-already-settled".to_string());

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        let Advanced::EffectIndeterminate { reason, .. } = advanced else {
            panic!("a settled invocation must stop for adoption, got {advanced:?}");
        };
        assert!(reason.contains("already settled"), "got {reason}");
        assert!(
            host.phases_run().is_empty(),
            "settled work must never redispatch"
        );
    }

    /// A row from before the gate minted refs names no job, and is also held —
    /// with a different reason.
    ///
    /// Two failures, two messages. Folding them into one would leave an operator
    /// unable to tell *nobody wrote down which job this was* from *the job never
    /// reported*, and only the first of those is a code-age problem that
    /// disappears on the next turn.
    #[tokio::test]
    async fn a_reattachable_row_that_names_no_job_is_held_and_says_so() {
        let store = MemoryLoopStateStore::new();
        let key = key("reattach-no-ref");
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![coding_effect("llm-1:tool:code-1", None)]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        let Advanced::EffectIndeterminate { reason, .. } = advanced else {
            panic!("got {advanced:?}");
        };
        assert!(reason.contains("names no job"), "got {reason}");
        assert!(
            host.reattach_asked.lock().unwrap().is_empty(),
            "there is nothing to look up, so nothing may be looked up"
        );
    }

    #[tokio::test]
    async fn the_outward_record_saying_it_already_fired_licenses_no_send() {
        let store = MemoryLoopStateStore::new();
        let key = key("already-fired");
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(batch(vec![outward_effect(
            "llm-1:tool:send-1",
            Some(act_ref()),
        )]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.reconcile_answer = ReconcileAnswer::Fired;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        let entries = host.entries.lock().unwrap();
        assert_eq!(
            entries[0],
            vec![ResolvedEffect {
                effect_id: EffectId::parse("llm-1:tool:send-1").unwrap(),
                action: EffectAction::AlreadyFired {
                    by_this_attempt: false
                },
            }],
            "the phase is told to settle against the record, never to send"
        );
    }

    #[tokio::test]
    async fn a_dispatch_that_died_mid_fire_leaves_its_batch_at_the_apply_cursor() {
        // The fire-blind case, and the ONLY path that publishes a batch. A
        // phase that errored after gating a send has left a live effect with no
        // record of it; if the batch is dropped here, the next worker re-enters
        // `Apply` with an empty `pending`, `resolve_effects` answers
        // `Ok(Vec::new())`, and the send is repeated with nothing consulted.
        //
        // What would leave this green if it were weaker: an assertion only that
        // `PhaseFailed` came back. That is why the load-back is the assertion —
        // `Advanced::PhaseFailed` is produced whether or not the batch was
        // recorded, and the cursor check is what says the row landed somewhere
        // that will actually be read.
        let store = MemoryLoopStateStore::new();
        let key = key("batch-survives-a-failed-dispatch");
        let state = apply_state(&store, &key).await;
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::FailMidDispatch]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(
                advanced,
                Advanced::PhaseFailed {
                    phase: Phase::Apply,
                    ..
                }
            ),
            "got {advanced:?}"
        );
        let committed = loaded(&store, &key).await;
        assert_eq!(
            committed.cursor.phase,
            Phase::Apply,
            "a failed attempt must not move the cursor, or the batch it just recorded is orphaned"
        );
        // Not named `batch`: that is the fixture constructor, and shadowing it
        // here would make the next assertion added to this test fail to compile
        // for a reason that has nothing to do with the property.
        let recorded = committed
            .pending
            .as_ref()
            .expect("the batch the phase died holding must be on disk");
        assert_eq!(
            recorded.effects.len(),
            1,
            "every member the gate admitted is recorded, not just the last"
        );
        assert!(
            store
                .read_journal(&key, 0)
                .await
                .expect("read")
                .iter()
                .any(|record| matches!(
                    &record.body,
                    JournalBody::Event { event_type, .. } if event_type == "successful_prefix"
                )),
            "events produced by the successful prefix must survive the failed attempt"
        );
        assert!(
            recorded.effects[0].reconcile_ref.is_some(),
            "an outward member keeps the act ref it will reconcile against; without it the next \
             claim has nothing to ask and must surface to a person"
        );
    }

    #[tokio::test]
    async fn a_batch_published_at_a_cursor_about_to_move_is_refused_before_it_is_written() {
        // `state.pending` is read by exactly one cursor. A phase that reported a
        // batch on its SUCCESS path would have it committed together with the
        // cursor the boundary moves to — `Epilogue` — where nothing consults it
        // and the next boundary overwrites it. Refusing at publication keeps the
        // bytes an operator needs off the disk-write path entirely.
        //
        // A weaker version of this test would script an ordinary phase and
        // assert nothing; it passes for a driver with no check at all. The
        // fixture has to actually publish a batch, which is what
        // `publish_batch_on_success` makes the fake do.
        let store = MemoryLoopStateStore::new();
        let key = key("published-batch-at-a-moving-cursor");
        let state = apply_state(&store, &key).await;
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Exit(BoundaryOutcome::NextIteration)]);
        host.publish_batch_on_success = true;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(
                advanced,
                Advanced::Quarantined(Quarantine::OrphanedBatch {
                    phase: Phase::Epilogue,
                    effects: 1
                })
            ),
            "got {advanced:?}"
        );
        let committed = loaded(&store, &key).await;
        assert!(
            committed.pending.is_none(),
            "the refusal happens before the append, so nothing about the run moved"
        );
        assert_eq!(
            committed.cursor.phase,
            Phase::Apply,
            "and the cursor stayed where it was"
        );
    }

    #[tokio::test]
    async fn a_committed_batch_at_a_cursor_that_never_reads_one_is_held() {
        // Only `Apply` consults `pending`. A batch anywhere else would be
        // overwritten by the next boundary's report, dropping a live dispatch
        // with nothing recording that it was dropped.
        let store = MemoryLoopStateStore::new();
        let key = key("orphaned-batch");
        let mut state = fresh_state(&key);
        state.pending = Some(batch(vec![outward_effect(
            "llm-1:tool:send-1",
            Some(act_ref()),
        )]));
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(
                advanced,
                Advanced::Quarantined(Quarantine::OrphanedBatch {
                    phase: Phase::Prepare,
                    effects: 1
                })
            ),
            "got {advanced:?}"
        );
        assert!(host.phases_run().is_empty());
    }

    // ====================================================================
    // Deadline, attempts, ceiling
    // ====================================================================

    #[tokio::test]
    async fn the_deadline_commits_the_canonical_terminal_without_running_a_phase() {
        // The watchdog this replaces was a spawned timer that died with its
        // worker. A stored deadline survives the handoff, and is checked by
        // whoever picks the run up.
        let store = MemoryLoopStateStore::new();
        let key = key("deadline");
        let mut state = fresh_state(&key);
        state.deadline_at_ms = Some(Utc::now().timestamp_millis() - 1_000);
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(matches!(
            advanced,
            Advanced::RunEnded {
                terminal: TerminalKind::Failed,
                outcome,
                ..
            } if matches!(outcome.as_ref(), AgenticOutcome::Failed { reason, .. }
                if reason.contains("execution deadline passed"))
        ));
        assert!(host.phases_run().is_empty());
        assert_eq!(
            *host.terminal_admission_policies.lock().unwrap(),
            vec![TerminalSteerPolicy::RuntimeClosure],
            "the deadline shortcut must close the durable steer epoch before publishing terminal"
        );
        assert!(
            store
                .list_runnable(&worker(), 10)
                .await
                .expect("scan")
                .is_empty(),
            "the committed non-resumable terminal must not be reoffered"
        );
    }

    #[tokio::test]
    async fn an_expired_run_never_livelocks_by_continuing_to_an_unreachable_decide() {
        let store = MemoryLoopStateStore::new();
        let key = key("deadline-cannot-continue");
        let mut state = fresh_state(&key);
        state.deadline_at_ms = Some(Utc::now().timestamp_millis() - 1_000);
        seed(&store, &key, &state).await;
        let mut host = FakeHost {
            terminal_admission_answer: TerminalSteerAdmission::ContinueToNextDecide,
            ..FakeHost::new(vec![Script::Continue])
        };

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(matches!(
            advanced,
            Advanced::ExecutionFailed { reason }
                if reason.contains("absolute deadline had already passed")
        ));
        assert!(host.phases_run().is_empty());
        assert_eq!(loaded(&store, &key).await.cursor, state.cursor);
    }

    #[tokio::test]
    async fn a_claimed_pre_phase_runtime_closure_commits_without_running_the_phase() {
        let store = MemoryLoopStateStore::new();
        let key = key("pre-phase-runtime-closure");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost {
            pre_phase_terminal_reason: Some("execution scope disappeared".to_owned()),
            ..FakeHost::new(vec![Script::Continue])
        };

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(matches!(
            advanced,
            Advanced::RunEnded {
                terminal: TerminalKind::Failed,
                outcome,
                ..
            } if matches!(outcome.as_ref(), AgenticOutcome::Failed { reason, .. }
                if reason == "execution scope disappeared")
        ));
        assert!(
            host.phases_run().is_empty(),
            "the resource/scope conclusion must win before phase work"
        );
        assert_eq!(
            *host.terminal_admission_policies.lock().unwrap(),
            vec![TerminalSteerPolicy::RuntimeClosure],
            "a runtime closure cannot leave accepted steer rows for an unreachable Decide"
        );
        assert!(
            store
                .list_runnable(&worker(), 10)
                .await
                .expect("scan")
                .is_empty(),
            "the claimed closure must leave a durable non-resumable terminal"
        );
    }

    #[tokio::test]
    async fn a_phase_that_keeps_failing_is_quarantined_rather_than_spun_on() {
        // The count has to survive the worker that incurred it, or a phase that
        // failed once on each of three workers reads as one failure forever.
        // Each round here reloads from the store, which is what a fresh worker
        // does.
        let store = MemoryLoopStateStore::new();
        let key = key("attempts");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Fail, Script::Fail, Script::Fail, Script::Fail]);

        for expected in 1..=3u32 {
            let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
            let Advanced::PhaseFailed { attempts, .. } = &advanced else {
                panic!("attempt {expected} must be counted, got {advanced:?}");
            };
            assert_eq!(*attempts, expected);
            assert_eq!(loaded(&store, &key).await.phase_attempts, expected);
        }

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(
                advanced,
                Advanced::Quarantined(Quarantine::PhaseAttemptsExhausted { .. })
            ),
            "got {advanced:?}"
        );
        assert_eq!(
            host.phases_run().len(),
            3,
            "a quarantined execution must stop calling the phase, not keep spinning"
        );
    }

    #[tokio::test]
    async fn a_completed_phase_clears_the_attempt_count() {
        // Consecutive, not lifetime. Without this a long run accumulates three
        // unrelated failures over hours and quarantines itself.
        let store = MemoryLoopStateStore::new();
        let key = key("attempts-reset");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Fail, Script::Continue]);

        let _ = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert_eq!(loaded(&store, &key).await.phase_attempts, 1);
        let _ = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert_eq!(loaded(&store, &key).await.phase_attempts, 0);
    }

    #[tokio::test]
    async fn a_cursor_past_the_iteration_ceiling_runs_nothing() {
        let store = MemoryLoopStateStore::new();
        let key = key("ceiling");
        let mut state = fresh_state(&key);
        state.cursor = LoopCursor {
            iteration: 99,
            phase: Phase::Prepare,
        };
        // The journal must reach the cursor or the divergence guard fires first.
        let mut seq = 0;
        for iteration in 1..99usize {
            for phase in Phase::ORDER {
                seq = store
                    .append_journal(
                        &key,
                        &[JournalAppend::phase_completed(
                            iteration,
                            phase,
                            RecordedStep::Continued,
                        )],
                    )
                    .await
                    .expect("append");
            }
        }
        state.journal_seq = seq;
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.ctx.max_iterations = 10;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::IterationCeiling { .. }),
            "got {advanced:?}"
        );
        assert!(host.phases_run().is_empty());
    }

    // ====================================================================
    // Placement, secrets, budget
    // ====================================================================

    #[tokio::test]
    async fn a_run_holding_a_user_typed_secret_is_pinned_before_it_is_published() {
        // Decision 4. Ephemeral secrets live in one process's memory, so a run
        // that took one must not be offered to another worker. The pin lands in
        // the same commit that publishes the state, so there is no window where
        // the run is both holding a secret and portable.
        let store = MemoryLoopStateStore::new();
        let key = key("pinned-by-secret");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.holds_secret = true;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        let placement = loaded(&store, &key).await.placement;
        // Matched rather than compared against a literal, so an expiry added to
        // `Pinned` does not turn this into a test of the struct's field list.
        let Placement::Pinned { worker: owner, .. } = &placement else {
            panic!("a run holding a user-typed secret must be pinned, got {placement:?}");
        };
        assert_eq!(owner, &worker());

        let mut other_host = FakeHost::new(vec![Script::Continue]);
        let other = WorkerId::new("worker-b");
        let advanced = advance_once(&store, &mut other_host, &key, &other, &config()).await;
        assert!(
            matches!(advanced, Advanced::NotClaimable { .. }),
            "another worker cannot take a run whose secrets it does not hold, got {advanced:?}"
        );
        assert!(other_host.phases_run().is_empty());
    }

    #[tokio::test]
    async fn a_portable_run_that_could_reach_a_local_browser_is_refused_at_commit() {
        // Decision 2. `BrowserTransportCeiling::parse(&[])` permits everything,
        // so a portable run with a headed ceiling is a run any worker may claim
        // and that may launch a Chrome the claimer does not have.
        let store = MemoryLoopStateStore::new();
        let key = key("portable-headed");
        let mut state = fresh_state(&key);
        state.identity.browser_transports = vec!["cdp".to_string(), "headed".to_string()];
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Refused(Refusal::Projection(_))),
            "got {advanced:?}"
        );
        assert_eq!(
            loaded(&store, &key).await.cursor.phase,
            Phase::Prepare,
            "a refused projection publishes nothing"
        );
        assert!(
            store.read_journal(&key, 0).await.expect("read").is_empty(),
            "a refusal writes nothing at all, not even an orphan"
        );
    }

    #[tokio::test]
    async fn the_budget_is_seeded_from_the_loaded_state_and_the_segment_reopens_at_entry() {
        // Decision 3's obligation on the driver. `for_commit` REPLACES
        // `work_budget_consumed_ms` from the context, so a driver that did not
        // seed publishes a total starting at this worker's zero — and a budget
        // that comes out low does not error, it just lets the run overrun.
        let store = MemoryLoopStateStore::new();
        let key = key("budget-seed");
        let mut state = fresh_state(&key);
        state.work_budget_consumed_ms = 45_000;
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        assert_eq!(
            host.ctx.work_budget_consumed_ms, 0,
            "a fresh worker starts at zero; that is the value that would be published"
        );

        let _ = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            loaded(&store, &key).await.work_budget_consumed_ms >= 45_000,
            "the previous workers' time must survive the handoff"
        );
        assert!(
            host.ran.lock().unwrap()[0].2,
            "the work-budget segment must be open while the phase runs, or the phase's own \
             time is free"
        );
    }

    // ====================================================================
    // Owner transitions
    // ====================================================================

    #[tokio::test]
    async fn a_host_that_cannot_adopt_the_committed_owner_fails_before_the_phase() {
        let store = MemoryLoopStateStore::new();
        let key = key("owner-adoption-refused");
        let mut state = fresh_state(&key);
        state.identity.agent_id = Some("committed-owner".to_string());
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);
        host.ctx.agent_id = Some("ambient-owner".to_string());
        host.owner_adoption_error =
            Some("live host is scoped to ambient-owner, not committed-owner".to_string());

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::ExecutionFailed { .. }),
            "authority mismatch must fail closed, got {advanced:?}"
        );
        assert!(
            host.phases_run().is_empty(),
            "no phase may run under the ambient owner's authority"
        );
        assert_eq!(
            loaded(&store, &key).await,
            state,
            "the refusal commits nothing"
        );
    }

    #[tokio::test]
    async fn a_handover_narrows_a_portable_run_and_takes_effect_only_after_the_phase() {
        let store = MemoryLoopStateStore::new();
        let key = key("handover");
        let mut state = fresh_state(&key);
        state.identity.agent_id = Some("agent-a".to_string());
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Handover("agent-b".to_string())]);
        host.ceilings.insert(
            "agent-b".to_string(),
            vec!["cdp".to_string(), "headed".to_string()],
        );

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert_eq!(
            host.ran.lock().unwrap()[0].1,
            Some("agent-a".to_string()),
            "the phase that decides a handover runs under the OLD owner"
        );
        let committed = loaded(&store, &key).await;
        assert_eq!(committed.identity.agent_id, Some("agent-b".to_string()));
        assert_eq!(
            committed.identity.browser_transports,
            vec!["cdp".to_string()],
            "a portable run's ceiling is intersected with cdp, never replaced by a wider one"
        );
    }

    #[tokio::test]
    async fn a_handover_to_an_owner_with_no_cdp_refuses_rather_than_widening() {
        // The intersection would be empty, and an empty ceiling parses as
        // UNRESTRICTED. Writing it literally turns the narrowest outcome into
        // the widest.
        let store = MemoryLoopStateStore::new();
        let key = key("handover-no-cdp");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Handover("agent-b".to_string())]);
        host.ceilings
            .insert("agent-b".to_string(), vec!["headed".to_string()]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Refused(Refusal::Projection(_))),
            "got {advanced:?}"
        );
        assert_eq!(
            loaded(&store, &key).await.identity.browser_transports,
            vec!["cdp".to_string()],
            "nothing was published"
        );
    }

    // ====================================================================
    // Parking
    // ====================================================================

    #[tokio::test]
    async fn parking_on_no_live_child_is_refused_rather_than_hung() {
        let store = MemoryLoopStateStore::new();
        let key = key("empty-park");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Park(Vec::new())]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Refused(Refusal::ParkWithNoLiveChild)),
            "got {advanced:?}"
        );
        assert!(
            loaded(&store, &key).await.wait.is_none(),
            "a park nothing can wake must never be published"
        );
    }

    #[tokio::test]
    async fn a_parked_run_waits_and_then_commits_before_it_consumes_its_wake() {
        // The store fixes this order and the reason is asymmetric: a crash
        // between the commit and the consume costs one extra round; consuming
        // first costs a lost wake and a run parked forever.
        let store = SpyStore::new();
        let key = key("park-and-wake");
        seed(&store, &key, &fresh_state(&key)).await;
        let mut host = FakeHost::new(vec![Script::Park(vec!["child-1".to_string()])]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::RunEnded { .. }),
            "got {advanced:?}"
        );
        let parked = loaded(&store, &key).await;
        let token = parked.wait.as_ref().expect("parked").wake_token();

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Parked { .. }),
            "an unresolved park runs nothing, got {advanced:?}"
        );

        store
            .resolve_wake(&key, &token, "child-1-completion")
            .await
            .expect("the child reports");
        store.calls.lock().unwrap().clear();

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::LeftPark { resolutions: 1, .. }),
            "got {advanced:?}"
        );
        assert!(loaded(&store, &key).await.wait.is_none());

        let calls = store.calls();
        let commit = calls
            .iter()
            .position(|c| *c == "commit")
            .expect("committed");
        let consume = calls
            .iter()
            .position(|c| *c == "consume_wake")
            .expect("consumed");
        assert!(
            commit < consume,
            "the commit that leaves the park must precede the consume: {calls:?}"
        );

        // And the woken run advances. This is the claim that would fail on a
        // journal whose terminal was recorded as non-resumable: the append after
        // it is refused, and a resumed park would be unable to write anything
        // ever again.
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "a woken run must be able to append after its own park record, got {advanced:?}"
        );
    }

    // ====================================================================
    // Divergence
    // ====================================================================

    #[tokio::test]
    async fn a_state_whose_cursor_its_journal_does_not_reach_is_quarantined() {
        // The snapshot is a cache of the journal. A cache that disagrees with
        // its source is not repairable by preferring either one.
        let store = MemoryLoopStateStore::new();
        let key = key("divergent");
        let mut state = fresh_state(&key);
        state.cursor = LoopCursor {
            iteration: 4,
            phase: Phase::Apply,
        };
        seed(&store, &key, &state).await;
        let mut host = FakeHost::new(vec![Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        assert!(
            matches!(
                advanced,
                Advanced::Quarantined(Quarantine::JournalDiverged { .. })
            ),
            "got {advanced:?}"
        );
        assert!(host.phases_run().is_empty());
    }

    // ====================================================================
    // Structure
    // ====================================================================

    #[test]
    fn an_exit_from_the_epilogue_is_refused_rather_than_recorded() {
        // `journal::replay_each` refuses this on read; refusing it on write is
        // what keeps a journal from being written that no reader can take back.
        let refusal = next_cursor(
            LoopCursor {
                iteration: 1,
                phase: Phase::Epilogue,
            },
            &RecordedStep::Exited {
                boundary: RecordedBoundary::NextIteration,
            },
        );
        assert!(matches!(refusal, Err(Refusal::ExitFromTheEpilogue)));
    }

    #[test]
    fn the_driver_advances_the_cursor_exactly_as_replay_does() {
        // Two implementations of one rule, checked against each other over every
        // phase and every step. A divergence here is a run that quarantines
        // itself the moment `verify_journal` reads what the driver wrote.
        for phase in Phase::ORDER {
            for step in [
                RecordedStep::Continued,
                RecordedStep::Exited {
                    boundary: RecordedBoundary::NextIteration,
                },
                RecordedStep::RunEnded {
                    terminal: TerminalKind::Success,
                },
            ] {
                let current = LoopCursor {
                    iteration: 7,
                    phase,
                };
                let mine = next_cursor(current, &step);
                let record = super::super::journal::JournalRecord {
                    seq: 1,
                    iteration: current.iteration,
                    phase,
                    ordinal: 0,
                    at_ms: 0,
                    body: JournalBody::PhaseCompleted { step },
                };
                let theirs = replay(&[record]);
                match (mine, theirs) {
                    (Ok(mine), Ok(theirs)) => assert_eq!(
                        (mine.iteration, mine.phase),
                        (theirs.iteration, theirs.phase),
                        "driver and replay disagree at {phase} on {step:?}"
                    ),
                    (Err(_), Err(_)) => {},
                    (mine, theirs) => panic!(
                        "driver and replay disagree about whether {phase} may take {step:?}: \
                         {mine:?} vs {theirs:?}"
                    ),
                }
            }
        }
    }

    // ====================================================================
    // The event outbox
    // ====================================================================

    #[tokio::test]
    async fn the_producers_routing_reaches_the_host_rather_than_being_dropped_by_the_sink() {
        // `phases::outbox`'s condition 2, as the one thing a case can watch.
        //
        // `routing_for` computes what `ActionExecutors::emit_event` decided,
        // `JournalBody::Event` carries it, the store persists it and
        // `ProjectorCursor::project` reads it back — and until 2026-08-28
        // `HostEventSink` was on `emit_routed`'s discarding default, so the
        // value reached the last call before the host and was thrown away.
        // Every one of those upstream steps had a test; the drop had none,
        // because no fixture asserted on what the HOST was told.
        //
        // # What production change would leave this green
        //
        // Deleting the `emit_routed` override alone would not: the default
        // forwards to `emit`, which passes `Unrecorded`, and both assertions
        // below are on values that are not `Unrecorded`. Deleting the
        // `routings` field's push would not either — the vector would be empty
        // against a two-element expectation.
        //
        // The one shape that would is a fixture built on `Script::JournalEvents`,
        // whose records carry `Unrecorded` — which is the field's `Default` AND
        // its `#[serde(default)]`, so a discarded routing and a recorded one are
        // the same value. That is why `JournalRoutedEvents` exists and why both
        // expectations below are non-default variants.
        let store = MemoryLoopStateStore::new();
        let key = key("outbox-routing");
        seed(&store, &key, &fresh_state(&key)).await;

        let fact = iteration_started("outbox-routing");
        let live_only = iteration_started("outbox-routing-live");
        let (fact_type, fact_payload) = split_event(&fact);
        let (live_type, live_payload) = split_event(&live_only);
        // Four fields and not five. `RecordedCanonicalScope` deliberately omits
        // `execution_id`, because a reader must take it from the RUN and not
        // from the journal's key — see that type's *THE FILE KEY IS NOT THE
        // EXECUTION ID*. A fixture that carried a fifth would be asserting on a
        // shape the format cannot hold.
        let recorded_scope = super::super::journal::RecordedCanonicalScope {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            task_id: "task-7".to_string(),
            ui_thread_id: "thread-7".to_string(),
        };
        let mut host = FakeHost::with_transport(vec![Script::JournalRoutedEvents(vec![
            (
                fact_type,
                fact_payload,
                RecordedEventRouting::CanonicalRuntimeFact {
                    scope: recorded_scope.clone(),
                },
            ),
            (live_type, live_payload, RecordedEventRouting::TransportOnly),
        ])]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );

        assert_eq!(
            host.delivered().len(),
            2,
            "both records must reach the host; a routing assertion over an empty transport would \
             pass for a walk that emitted nothing"
        );
        assert_eq!(
            host.routings_seen(),
            vec![
                RecordedEventRouting::CanonicalRuntimeFact {
                    scope: recorded_scope
                },
                RecordedEventRouting::TransportOnly,
            ],
            "the host must be handed what the producer decided, per record and in delivery order. \
             A sink on the discarding default hands it `Unrecorded` for both — every live surface \
             still fed, and the persisted runtime-fact stream silently off"
        );
    }

    #[tokio::test]
    async fn a_journalled_event_reaches_a_transport_once_and_a_second_worker_does_not_repeat_it() {
        // The whole point of the outbox, in one case: the event a phase
        // journalled is emitted, it is emitted ONCE, and the record of that
        // survives the process. The second half is what a process-local mark
        // would fail — so the later boundaries run on a FRESH host with an empty
        // transport, which is what a restart or a second worker looks like.
        let store = MemoryLoopStateStore::new();
        let key = key("outbox-once");
        seed(&store, &key, &fresh_state(&key)).await;
        let event = iteration_started("outbox-once");
        let mut first = FakeHost::with_transport(vec![journals(&event)]);

        let advanced = advance_once(&store, &mut first, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert_eq!(
            first.delivered(),
            vec![as_json(&event)],
            "the event a transport receives must be the event the phase produced, byte for byte \
             — this is what a rejoin that lost the payload or renamed the tag would fail"
        );

        // Durable, not process-local. Against the event's OWN seq rather than
        // against a constant: a mark that merely moved would satisfy `> 0` while
        // sitting below the record it was supposed to pass.
        let event_seq = store
            .read_journal(&key, 0)
            .await
            .expect("read")
            .into_iter()
            .find(|record| matches!(record.body, JournalBody::Event { .. }))
            .expect("the phase journalled one")
            .seq;
        let mark = store
            .load_projector_cursor(&key)
            .await
            .expect("the mark reads back")
            .expect("a projection that emitted must have saved a mark");
        assert!(
            mark.emitted_through_seq() >= event_seq,
            "the mark must have moved past the emitted record at seq {event_seq}, got {}",
            mark.emitted_through_seq()
        );

        let mut second = FakeHost::with_transport(vec![Script::Continue, Script::Continue]);
        for _ in 0..2 {
            let advanced = advance_once(&store, &mut second, &key, &worker(), &config()).await;
            assert!(
                matches!(advanced, Advanced::Committed { .. }),
                "got {advanced:?}"
            );
        }
        assert!(
            second.delivered().is_empty(),
            "a worker that did not emit the event must not emit it again, got {:?}",
            second.delivered()
        );
    }

    #[tokio::test]
    async fn an_events_address_does_not_move_because_the_attempt_also_handed_over() {
        // TWO ATTEMPTS AT ONE CURSOR, WITH DIFFERENT BATCH COMPOSITIONS. That
        // pairing is the whole case, and until this test nothing in this file
        // produced it.
        //
        // `commit_boundary` stamps `ordinal: index` over `PhaseReport::records`,
        // so a record's position in that list IS its journal address, and
        // `ProjectorCursor::project` dedupes by address — `EventKey` is
        // `(execution_id, iteration, phase, ordinal)` and carries no seq, which
        // is what lets a re-run's duplicate be recognised at all.
        //
        // The owner-transition record is present on only SOME attempts. So the
        // batch this run's first claim publishes is four records and the batch
        // its second claim publishes at the same cursor is three, and the
        // question this asserts is whether the three events kept their
        // addresses across that difference.
        //
        // # Why the second attempt happens at all
        //
        // The first pauses on `WaitingForChildren`, a RESUMABLE terminal:
        // `advance_once` falls through for those rather than answering
        // `RunAlreadyEnded`, and a run-ending step leaves the cursor where it
        // was. So the next claim re-enters the same phase of the same iteration
        // — the ordinary shape of a run that pauses for a confirmation while
        // collapsing its owner and then continues after the answer.
        //
        // # What would break this, and each one has shipped here before
        //
        // - **A completion record that competes for a low ordinal.** If the
        //   `!ends_run` batch started the host's records at index 1 to make room
        //   for the completion, the paused attempt's events would sit at 0..2 and
        //   the continuing attempt's at 1..3 — the same events at six addresses.
        //   `journal::PHASE_COMPLETION_ORDINAL` exists because that is what the
        //   driver used to do.
        // - **A projector that deduped on seq.** The second attempt's records
        //   are appended at higher seqs by construction, so a seq-keyed window
        //   re-emits every one of them.
        // - **A stamper that counted only some bodies.** Anything that made an
        //   event's index a function of what ELSE was in the batch.
        //
        // # What it does NOT hold
        //
        // The order inside the first batch is the fixture's, so this says
        // nothing about `executor.rs::InProcessWorkerHost::run_phase` choosing
        // it. That function has its own test — `a_handover_never_shifts_an_event_ordinal`
        // — over `append_in_address_order`, which is the single statement it
        // assembles the list with. The two together are the property; neither
        // alone is.
        let store = MemoryLoopStateStore::new();
        let key = key("handover-and-events");
        seed(&store, &key, &fresh_state(&key)).await;

        // THREE, and distinguishable. With one event a build that shifted every
        // address by one would put that event where no other attempt put
        // anything, and the shift would be a re-emit that looks like a first
        // emit; with identical events a shift would be invisible in
        // `delivered()`.
        let produced: Vec<RuntimeTransportEvent> = (0..3)
            .map(|n| RuntimeTransportEvent::AgenticIterationStarted {
                execution_id: "handover-and-events".to_string(),
                principal: Some("owner".to_string()),
                workspace: Some("default".to_string()),
                plan_id: "plan-1".to_string(),
                step_id: format!("step-{n}"),
                iteration: 1,
                environment_type: "shell".to_string(),
                timestamp: 1_700_000_000_000,
            })
            .collect();
        let split: Vec<(String, serde_json::Value)> = produced.iter().map(split_event).collect();
        let expected: Vec<serde_json::Value> = produced.iter().map(as_json).collect();

        let mut host = FakeHost::with_transport(vec![
            Script::PauseWithHandoverAndEvents {
                to: "agent-b".to_string(),
                events: split.clone(),
            },
            Script::JournalEvents(split.clone()),
        ]);

        // ── The attempt that handed over ────────────────────────────────────
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::RunEnded { .. }),
            "the scripted phase pauses, so this claim must report the run ended: {advanced:?}"
        );
        assert_eq!(
            host.delivered(),
            expected,
            "the paused boundary's events must reach the transport before its outcome is handed \
             back"
        );

        let after_handover = store.read_journal(&key, 0).await.expect("read");
        assert_eq!(
            event_ordinals(&after_handover),
            vec![0, 1, 2],
            "the three events this phase emitted must take the first three addresses; the \
             transition is the record that may or may not be here and nothing may be addressed \
             behind it, got {after_handover:?}"
        );
        assert_eq!(
            after_handover
                .iter()
                .find(|record| matches!(record.body, JournalBody::OwnerTransition { .. }))
                .map(|record| record.ordinal),
            Some(3),
            "and the handover must still be IN the batch — without it `LoopState`'s identity \
             never tracks the owner and the next phase either runs stale authority or is \
             fail-closed by `adopt_owner`"
        );

        // ── The re-attempt at the same cursor, with no handover ─────────────
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "a resumable terminal is re-entered rather than refused: {advanced:?}"
        );

        let after_rerun = store.read_journal(&key, 0).await.expect("read");
        assert_eq!(
            event_ordinals(&after_rerun),
            vec![0, 1, 2, 0, 1, 2],
            "the re-attempt must re-derive the SAME three addresses. Anything else and the \
             dedupe window is looking for addresses that are not there, got {after_rerun:?}"
        );

        // The consequence, on the same host so the two attempts' deliveries
        // accumulate. This is the assertion that fails loudly on any of the
        // three regressions named above: six entries here is every event emitted
        // twice.
        assert_eq!(
            host.delivered(),
            expected,
            "the re-attempt journalled the same three events at the same three addresses, so \
             the projector must recognise every one of them as a duplicate and emit nothing"
        );
    }

    #[tokio::test]
    async fn a_stale_commit_emits_nothing_because_its_records_are_never_authoritative() {
        // The watermark rule, at the driver rather than at the cursor. Another
        // worker commits while this one is mid-phase, so this one's append sits
        // ABOVE the watermark and its commit is refused. A projection placed
        // before the commit — or bounded by `last_seq` rather than by the
        // committed `journal_seq` — would have put a discarded attempt's event on
        // a transport, with nothing that ever retracts it.
        let store = MemoryLoopStateStore::new();
        let key = key("stale-emits-nothing");
        let state = fresh_state(&key);
        let revision = seed(&store, &key, &state).await;

        let mut theirs = state.clone();
        theirs.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Observe,
        };
        let seq = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("their append");
        theirs.journal_seq = seq;
        store
            .commit(&key, &theirs, revision)
            .await
            .expect("their commit");

        let event = iteration_started("stale-emits-nothing");
        let mut host = FakeHost::with_transport(vec![]);
        let mut lease = store
            .claim(&key, &worker(), Duration::from_secs(5))
            .await
            .expect("claim");
        // The report a phase would have produced, journalling an event, committed
        // against the revision this worker loaded — which the store has moved
        // past.
        let mut report = PhaseReport::continued();
        report.records = event_records(1, Phase::Prepare, vec![split_event(&event)]);
        let advanced = commit_boundary(
            &store,
            &mut host,
            &key,
            &worker(),
            &config(),
            state,
            revision,
            &mut lease,
            Phase::Prepare,
            1,
            report,
            0,
        )
        .await;

        assert!(
            matches!(advanced, Advanced::Refused(Refusal::StaleCommit { .. })),
            "got {advanced:?}"
        );
        assert!(
            host.delivered().is_empty(),
            "a discarded attempt's events must never reach a transport, got {:?}",
            host.delivered()
        );
        assert!(
            store
                .load_projector_cursor(&key)
                .await
                .expect("the mark reads back")
                .is_none(),
            "and nothing may be marked emitted"
        );
    }

    #[tokio::test]
    async fn a_named_record_reaches_the_host_through_its_own_rail() {
        // End to end for the second rail: a phase journals a named call, the
        // boundary appends and commits it, the projection pass reads it back and
        // the host receives all five arguments — through
        // `emit_projected_named_event` and not through `emit_projected_event`.
        //
        // THE TWO RAILS IN ONE BATCH, and the case is built that way rather than
        // described that way. An earlier version of this comment claimed the
        // batch property while the body ran two hosts over two executions with
        // one body kind each; the property survived by a different mechanism
        // (each host was asserted both ways) and the interleaved batch was
        // covered only at unit level in `journal.rs`. `Script::JournalBothRails`
        // puts one `AgenticIterationStarted` and one `plan.step.started` into
        // ONE `PhaseReport::records`, which is the shape
        // `emit_step_events_if_signaled` produces.
        //
        // What that buys over two single-rail batches: `commit_boundary` stamps
        // `ordinal: index` over one list, so the two rails take ordinals 0 and 1
        // out of one counter and `Journal::check_batch_addresses` sees them in
        // one call; and the projector walks them in seq order out of one window.
        // A driver that delivered every record down whichever rail it happened to
        // implement, or a walk that dispatched per batch rather than per record,
        // passes a single-rail batch and fails this.
        let store = MemoryLoopStateStore::new();
        // BOTH keys before the shadowing `let key`, because `key` is the
        // imported fixture function and a `let key = key(..)` shadows it in the
        // value namespace for the rest of the body — a second `key("..")` below
        // it is `E0618: expected function, found ExecutionKey`, which is exactly
        // what the version of this case that ran two hosts hit.
        let key_named_only = key("named-rail-only");
        let key = key("named-rail");
        seed(&store, &key, &fresh_state(&key)).await;
        let event = iteration_started("named-rail");

        let mut host = FakeHost::with_transport(vec![Script::JournalBothRails {
            events: vec![split_event(&event)],
            named: vec![(
                "plan.step.started".to_string(),
                serde_json::json!({ "step_id": "s-1", "task_id": "task-t" }),
            )],
        }]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );

        assert_eq!(
            host.named_delivered(),
            vec![(
                "plan.step.started".to_string(),
                FIXTURE_NAMED_AGENT.to_string(),
                Some(FIXTURE_NAMED_PRINCIPAL.to_string()),
                Some(FIXTURE_NAMED_WORKSPACE.to_string()),
                serde_json::json!({ "step_id": "s-1", "task_id": "task-t" }),
            )],
            "the named record must reach the host with every argument the producer supplied; \
             the scope halves are `Some` here precisely because they are omitted from the wire \
             when they are `None`, so a store or a walk that dropped them would otherwise \
             round-trip `None` into `None`"
        );
        assert_eq!(
            host.delivered(),
            vec![as_json(&event)],
            "and the transport record in the SAME batch must reach the transport rail — \
             exactly one entry on each side is what says the walk routed by body and not by \
             habit"
        );

        // Addresses, because the shared `0..n` is what the projector's dedupe
        // window keys on and a per-rail counter would restart it.
        //
        // `records` also holds the phase-completion record this boundary wrote,
        // at `PHASE_COMPLETION_ORDINAL` (`u32::MAX`) rather than in the host's
        // range — so the two emittable records are filtered out by body rather
        // than by counting, which is also what keeps this assertion from
        // breaking if the completion record ever moves within the batch.
        let records = store.read_journal(&key, 0).await.expect("read");
        let emittable: Vec<(u32, &JournalBody)> = records
            .iter()
            .filter(|record| {
                !matches!(
                    record.body,
                    JournalBody::PhaseCompleted { .. }
                        | JournalBody::RecoveryRewind { .. }
                        | JournalBody::OwnerTransition { .. }
                )
            })
            .map(|record| (record.ordinal, &record.body))
            .collect();
        assert_eq!(emittable.len(), 2, "one batch, both rails, one sequence");
        assert_eq!(
            emittable
                .iter()
                .map(|(ordinal, _)| *ordinal)
                .collect::<Vec<_>>(),
            vec![0, 1],
            "the boundary stamps ordinals over one list whatever bodies are in it; a counter \
             per rail would give both records ordinal 0 and the projector would dedupe the \
             second against the first"
        );
        assert!(
            matches!(emittable[0].1, JournalBody::Event { .. })
                && matches!(emittable[1].1, JournalBody::NamedEvent { .. }),
            "and the store returns them in the order the phase produced them, got {:?}",
            emittable
        );

        // The mark passed both, so a second pass does not deliver either again.
        let mark = store
            .load_projector_cursor(&key)
            .await
            .expect("the mark reads back")
            .expect("something was emitted, so a mark was saved");
        let last_seq = records
            .last()
            .map(|record| record.seq)
            .expect("the boundary appended");
        assert_eq!(
            mark.emitted_through_seq(),
            last_seq,
            "the mark must sit past every record this boundary made authoritative, or the next \
             boundary re-delivers this step lifecycle"
        );

        // A named-only batch, so the case above is not the only thing saying a
        // host with no transport-rail record still gets its named one. The
        // mixed batch could pass a walk that only ever emitted the FIRST record
        // of a batch down each rail; this one could not.
        let mut named_only = FakeHost::with_transport(vec![Script::JournalNamedEvents(vec![(
            "plan.step.finished".to_string(),
            serde_json::json!({ "step_id": "s-1", "status": "completed" }),
        )])]);
        seed(&store, &key_named_only, &fresh_state(&key_named_only)).await;
        let advanced = advance_once(
            &store,
            &mut named_only,
            &key_named_only,
            &worker(),
            &config(),
        )
        .await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert_eq!(named_only.named_delivered().len(), 1);
        assert!(
            named_only.delivered().is_empty(),
            "a named record must NOT be delivered as a transport event: that rail replays \
             through `emit_transport_only`, which performs none of the chat fan-out this \
             record exists to preserve. Got {:?}",
            named_only.delivered()
        );
    }

    #[tokio::test]
    async fn an_event_this_build_cannot_rejoin_is_dropped_rather_than_burying_the_one_behind_it() {
        // `EmitRefused`'s rule, exercised where it is decided. A record whose
        // `event_type` this build has no variant for is refused by `rejoin` on
        // every retry, forever — so the sink must TAKE it and drop it. A sink
        // that returned `Err` instead would stop the walk with the mark below it
        // and bury every event above it for the life of the run, which is what
        // the second assertion here would catch.
        let store = MemoryLoopStateStore::new();
        let key = key("unmappable-first");
        seed(&store, &key, &fresh_state(&key)).await;
        let good = iteration_started("unmappable-first");
        let pairs = vec![
            (
                "AnEventTypeThisBuildHasNoVariantFor".to_string(),
                serde_json::json!({ "whatever": true }),
            ),
            split_event(&good),
        ];
        let mut host = FakeHost::with_transport(vec![Script::JournalEvents(pairs)]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert_eq!(
            host.delivered(),
            vec![as_json(&good)],
            "the event behind the unmappable one must still be delivered"
        );

        // And the mark passed BOTH, so the unmappable record is never re-offered
        // and never stalls a later pass.
        let mark = store
            .load_projector_cursor(&key)
            .await
            .expect("the mark reads back")
            .expect("something was emitted, so a mark was saved");
        let last_seq = store
            .read_journal(&key, 0)
            .await
            .expect("read")
            .last()
            .map(|record| record.seq)
            .expect("the boundary appended");
        assert_eq!(
            mark.emitted_through_seq(),
            last_seq,
            "the mark must sit past every record this boundary made authoritative"
        );
    }

    #[tokio::test]
    async fn a_refused_emit_leaves_the_mark_below_the_record_and_the_next_pass_retries_it() {
        // The one thing `EmitRefused` is allowed to mean: not now. The transport
        // is down for exactly one attempt. The mark must stop BELOW the refused
        // record — a walk that skipped ahead would pass a record nothing emitted,
        // and once the mark is durable nothing comes back for it.
        let store = MemoryLoopStateStore::new();
        let key = key("transport-down");
        seed(&store, &key, &fresh_state(&key)).await;
        let event = iteration_started("transport-down");
        let mut host = FakeHost::with_transport(vec![journals(&event), Script::Continue]);
        host.refusals_remaining = 1;

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "a transport that is down must not fail the boundary, got {advanced:?}"
        );
        assert!(
            host.delivered().is_empty(),
            "nothing was delivered on the refused pass, got {:?}",
            host.delivered()
        );
        let event_seq = store
            .read_journal(&key, 0)
            .await
            .expect("read")
            .into_iter()
            .find(|record| matches!(record.body, JournalBody::Event { .. }))
            .expect("the phase journalled one")
            .seq;
        let mark = store
            .load_projector_cursor(&key)
            .await
            .expect("the mark reads back")
            .expect("the walk still passed the completion record");
        assert!(
            mark.emitted_through_seq() < event_seq,
            "the mark must sit below the refused record at seq {event_seq}, got {}",
            mark.emitted_through_seq()
        );

        // The transport comes back and the next boundary picks the same record up.
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert_eq!(
            host.delivered(),
            vec![as_json(&event)],
            "the record the refusal held must be retried, not skipped"
        );
    }

    #[tokio::test]
    async fn a_host_with_no_transports_marks_nothing_emitted() {
        // The safe trait default: `emits_projected_events` is `false` until a
        // host overrides it. Nothing may be marked emitted by a process that cannot
        // emit — a projector that ran anyway would move the mark past every event
        // of every run, and once that mark is durable those events are gone.
        let store = MemoryLoopStateStore::new();
        let key = key("no-transport");
        seed(&store, &key, &fresh_state(&key)).await;
        let event = iteration_started("no-transport");
        let mut host = FakeHost::new(vec![journals(&event), Script::Continue]);

        for _ in 0..2 {
            let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
            assert!(
                matches!(advanced, Advanced::Committed { .. }),
                "got {advanced:?}"
            );
        }

        assert!(
            store
                .load_projector_cursor(&key)
                .await
                .expect("the mark reads back")
                .is_none(),
            "a host with no transports must leave no mark at all"
        );
        // And the record is still there for a build that can deliver it.
        assert!(
            store
                .read_journal(&key, 0)
                .await
                .expect("read")
                .iter()
                .any(|record| matches!(record.body, JournalBody::Event { .. })),
            "the event stays in the journal"
        );
    }

    #[tokio::test]
    async fn a_mark_that_cannot_be_saved_costs_one_boundary_of_duplicates_and_not_the_run() {
        // Emit, THEN mark — and this is the bill for that order, paid in the
        // open. The events are already on the transport when the save fails, so
        // the boundary must not be refused; the cost is that the next pass emits
        // them again, which is what the dedupe window exists to absorb.
        //
        // The other order — mark, then emit — would have no duplicate here and
        // would instead lose the event outright on a crash between the two, with
        // nothing that ever re-offers it. That is why this asserts a duplicate
        // rather than treating one as a bug.
        let store = SpyStore {
            projector_save_fails: true,
            ..SpyStore::new()
        };
        let key = key("mark-unwritable");
        seed(&store, &key, &fresh_state(&key)).await;
        let event = iteration_started("mark-unwritable");
        let mut host = FakeHost::with_transport(vec![journals(&event), Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "an unwritable mark must not hold the run up, got {advanced:?}"
        );
        assert_eq!(host.delivered().len(), 1, "the event was emitted");

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert_eq!(
            host.delivered(),
            vec![as_json(&event), as_json(&event)],
            "with no durable mark the same event is emitted again — at-least-once, which is what \
             an outbox buys and is not to be described as exactly-once"
        );
    }

    #[tokio::test]
    async fn a_run_ending_boundarys_events_are_emitted_before_the_outcome_is_handed_back() {
        // `RunEnded` returns out of `commit_boundary`, and nothing downstream
        // comes back to project — so a projection placed after the `match` on
        // `report.step`, or only on the `Committed` arm, would silently drop the
        // last boundary's events. Those are the ones a reader most wants.
        let store = MemoryLoopStateStore::new();
        let key = key("outbox-at-the-end");
        let event = iteration_started("outbox-at-the-end");
        // No script: this case calls `commit_boundary` directly, because the
        // report it needs — a run-ending step CARRYING an event record — is the
        // batch whose append order `commit_boundary` reverses, and `advance_once`
        // would need a phase that produced both.
        let mut host = FakeHost::with_transport(vec![]);
        let mut report = PhaseReport::ends_run(AgenticOutcome::Success {
            completion: crate::magician_v2::execution::agentic::types::CompletionKind::Full,
            open: Vec::new(),
            final_state: EnvironmentState::Uninitialized,
            iterations_used: 1,
            artifacts: Vec::new(),
        });
        report.records = event_records(1, Phase::Epilogue, vec![split_event(&event)]);
        let mut state = fresh_state(&key);
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Epilogue,
        };
        let revision = seed(&store, &key, &state).await;
        let mut lease = store
            .claim(&key, &worker(), Duration::from_secs(5))
            .await
            .expect("claim");

        let advanced = commit_boundary(
            &store,
            &mut host,
            &key,
            &worker(),
            &config(),
            state,
            revision,
            &mut lease,
            Phase::Epilogue,
            1,
            report,
            0,
        )
        .await;

        assert!(
            matches!(advanced, Advanced::RunEnded { .. }),
            "got {advanced:?}"
        );
        assert_eq!(
            host.delivered(),
            vec![as_json(&event)],
            "the last boundary's events must be emitted before the run's outcome is returned"
        );
    }

    #[tokio::test]
    async fn the_mark_this_driver_writes_reads_back_off_a_real_filesystem_store() {
        // Every other outbox case above drives `MemoryLoopStateStore`, which holds
        // a `ProjectorCursor` **by value** — so it cannot fail either of the two
        // things the durable store does to a mark: round-trip it through
        // `ProjectorCursorWire`, and measure it against `MAX_PROJECTOR_BYTES` and
        // `MAX_PROJECTOR_JSON_NODES`. A mark this driver saved and could not read
        // back would look green in all of them and lose a boundary's worth of
        // events the first time it ran in production.
        //
        // So this runs the same two boundaries against `FsLoopStateStore`, the
        // store the runtime configures, over real `projector.json` bytes.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = FsLoopStateStore::new(dir.path());
        let key = key("fs-projector");
        seed(&store, &key, &fresh_state(&key)).await;
        let event = iteration_started("fs-projector");
        let mut host = FakeHost::with_transport(vec![journals(&event), Script::Continue]);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert_eq!(
            host.delivered(),
            vec![as_json(&event)],
            "the journalled event reaches the transport on the boundary that committed it"
        );

        let event_seq = store
            .read_journal(&key, 0)
            .await
            .expect("read")
            .into_iter()
            .find(|record| matches!(record.body, JournalBody::Event { .. }))
            .expect("the phase journalled one")
            .seq;

        // A second worker over the same root, holding nothing the first one
        // learned — the shape a restart arrives in, and the only one that reads
        // the mark back off disk.
        let reopened = FsLoopStateStore::new(dir.path());
        let mark = reopened
            .load_projector_cursor(&key)
            .await
            .expect("a mark this store wrote must be one it can read back")
            .expect("a boundary that emitted must have left one");
        assert!(
            mark.emitted_through_seq() >= event_seq,
            "the durable mark must cover the event it emitted at seq {event_seq}, got {}",
            mark.emitted_through_seq()
        );

        // Asserted as well as the mark, because an UNREADABLE mark also produces
        // an unchanged delivery list — `project_outbox` fails closed and emits
        // nothing. The two assertions together are what separate "the mark
        // survived" from "the projector gave up".
        let advanced = advance_once(&reopened, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "got {advanced:?}"
        );
        assert_eq!(
            host.delivered(),
            vec![as_json(&event)],
            "a mark that survived the filesystem must not let the same address emit twice"
        );
    }

    // ====================================================================
    // The seam: gate → intents → dispatch → outcomes
    // ====================================================================

    /// A host that really does split `Apply`, and that **looks at the store from
    /// inside its dispatch**.
    ///
    /// That last part is the whole reason this is a separate fake rather than a
    /// flag on [`FakeHost`]. Asserting the ledger after `advance_once` returns
    /// proves only that the driver wrote intents *at some point in the phase* —
    /// a driver that dispatched first and recorded everything afterwards passes
    /// that test, and that driver is precisely the bug the seam exists to make
    /// impossible. Reading the ledger from within `dispatch_apply` is the only
    /// vantage point from which *before the fire* is a checkable claim.
    struct SeamHost {
        ctx: AgenticContext,
        /// The same store `advance_once` is driving — a second **handle**, not
        /// a second store. `MemoryLoopStateStore` is an `Arc<Mutex<..>>` inside
        /// and says so: cloning it shares the map. A genuinely separate store
        /// would answer about an empty ledger and this fake's whole assertion
        /// would pass for the wrong reason.
        store: MemoryLoopStateStore,
        key: ExecutionKey,
        /// The batch the gate hands back.
        batch: PendingBatch,
        /// Whether the dispatch half dies after settling its first member.
        fail_mid_dispatch: bool,
        /// Whether this fake's outward record answers *it already left*.
        ///
        /// The default answer is `SurfaceToUser`, which holds the run at
        /// `resolve_effects` and so never reaches a plan. A fixture that wants
        /// to observe what the DISPATCH is handed has to get past that gate, and
        /// `AlreadyFired` is the cheapest honest way: it is a real answer the
        /// outward record gives, and it produces an `EffectAction` the phase must
        /// not dispatch.
        reconciles_as_fired: bool,
        /// What the ledger held **at the moment the dispatch was entered**:
        /// `(effect_id, an_outcome_is_recorded)` per row, sorted.
        seen_at_dispatch: Arc<Mutex<Vec<(String, bool)>>>,
        /// How many members the driver's plan named when the dispatch was
        /// entered. `None` would mean the driver declared it resolves nothing,
        /// which this driver never does.
        plan_at_dispatch: Arc<Mutex<Option<usize>>>,
        /// The first member's exact post-intent verdict at dispatch entry.
        plan_action_at_dispatch: Arc<Mutex<Option<EffectAction>>>,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl SeamHost {
        fn new(store: MemoryLoopStateStore, key: ExecutionKey, batch: PendingBatch) -> Self {
            let mut ctx = AgenticContext::new("goal", "criteria");
            ctx.max_iterations = 10;
            Self {
                ctx,
                store,
                key,
                batch,
                fail_mid_dispatch: false,
                reconciles_as_fired: false,
                seen_at_dispatch: Arc::new(Mutex::new(Vec::new())),
                plan_at_dispatch: Arc::new(Mutex::new(None)),
                plan_action_at_dispatch: Arc::new(Mutex::new(None)),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn failing_mid_dispatch(self) -> Self {
            Self {
                fail_mid_dispatch: true,
                ..self
            }
        }

        fn reconciling_as_already_fired(self) -> Self {
            Self {
                reconciles_as_fired: true,
                ..self
            }
        }
    }

    #[async_trait]
    impl WorkerHost for SeamHost {
        fn context(&self) -> &AgenticContext {
            &self.ctx
        }

        fn context_mut(&mut self) -> &mut AgenticContext {
            &mut self.ctx
        }

        fn holds_user_typed_ephemeral_secret(&self) -> bool {
            false
        }

        fn browser_ceiling_for(&self, _agent_id: &str) -> Vec<String> {
            vec!["cdp".to_string()]
        }

        fn adopt_owner(&mut self, identity: &RunIdentity) -> Result<(), String> {
            self.ctx.agent_id = identity.agent_id.clone();
            Ok(())
        }

        fn rederive_dispatch(&self, _effect_id: &EffectId) -> Option<RederivedDispatch> {
            Some(RederivedDispatch {
                tool: "gmail__send".to_string(),
                arguments_fingerprint: "fp-1".to_string(),
            })
        }

        fn reconcile_by_ref(
            &self,
            _reconcile_ref: &CommittedActRef,
            _effect_id: &EffectId,
        ) -> ReconciledEffect {
            if self.reconciles_as_fired {
                return ReconciledEffect::AlreadyFired {
                    by_this_attempt: true,
                };
            }
            ReconciledEffect::SurfaceToUser {
                reason: "this fake does not read an outward record".to_string(),
            }
        }

        fn reattach_state(&self, _identity: &RunIdentity, _reattach_ref: &str) -> ReattachState {
            ReattachState::Absent
        }

        async fn conclude_deadline<'a>(
            &mut self,
            entry: PhaseEntry<'a>,
            deadline_at_ms: i64,
        ) -> PhaseReport {
            PhaseReport::ends_run(AgenticOutcome::Failed {
                reason: format!("execution deadline passed at {deadline_at_ms}"),
                last_state: EnvironmentState::Uninitialized,
                iterations_used: entry.iteration.saturating_sub(1),
            })
        }

        fn iteration_checkpoint(&self, iteration: usize) -> IterationCheckpoint {
            IterationCheckpoint {
                iteration,
                history_iterations_len_at_start: 0,
                started_at_ms: 1_000,
            }
        }

        fn continuation_checkpoint(
            &self,
            iteration: usize,
        ) -> Result<super::super::state::IterationContinuationCheckpoint, String> {
            fixture_continuation(iteration)
        }

        async fn run_phase<'a>(
            &mut self,
            entry: PhaseEntry<'a>,
        ) -> Result<PhaseReport, PhaseFailure> {
            assert_ne!(
                entry.phase,
                Phase::Apply,
                "a host that splits its Apply must never be asked to run it whole"
            );
            self.calls.lock().unwrap().push("run_phase");
            Ok(PhaseReport::continued())
        }

        fn splits_apply_gate_from_dispatch(&self) -> bool {
            true
        }

        fn has_live_apply_carry(&self) -> bool {
            true
        }

        async fn gate_apply<'a>(
            &mut self,
            _entry: PhaseEntry<'a>,
        ) -> Result<GatedPhase, PhaseFailure> {
            self.calls.lock().unwrap().push("gate_apply");
            Ok(GatedPhase::Gated(Box::new(
                GatedApply::from_batch_for_test(self.batch.clone()),
            )))
        }

        async fn dispatch_apply(
            &mut self,
            _gated: Box<GatedApply>,
            intents: ApplyIntents,
            plan: EffectPlan,
            _deadline_at_ms: Option<i64>,
            settled: &mut Vec<(EffectId, EffectOutcome)>,
        ) -> Result<PhaseReport, PhaseFailure> {
            self.calls.lock().unwrap().push("dispatch_apply");
            // Recorded, not asserted, so the two tests that use this fake can
            // each say what they expect. A fixture that asserted here would make
            // every future test of the seam agree with the first one.
            *self.plan_at_dispatch.lock().unwrap() = plan.planned_effects();
            *self.plan_action_at_dispatch.lock().unwrap() = self
                .batch
                .effects
                .first()
                .and_then(|effect| plan.action_for(&effect.effect_id))
                .cloned();

            // THE OBSERVATION THIS FAKE EXISTS FOR. The durable ledger, read
            // from inside the fire rather than after it.
            let ledger = self
                .store
                .load_effects(&self.key)
                .await
                .expect("the store must answer inside the dispatch");
            let committed = self
                .store
                .load(&self.key)
                .await
                .expect("the store must answer inside the dispatch")
                .expect("the run exists");
            assert_eq!(
                committed.state.pending.as_ref(),
                Some(&self.batch),
                "the complete batch marker must be fenced into state before dispatch"
            );
            let mut seen: Vec<(String, bool)> = ledger
                .entries()
                .map(|entry| (entry.effect_id.to_string(), entry.outcome.is_some()))
                .collect();
            seen.sort();
            *self.seen_at_dispatch.lock().unwrap() = seen;

            assert_eq!(
                intents.recorded_intents(),
                Some(self.batch.effects.len()),
                "the receipt must claim exactly this batch's members"
            );

            // Settle the first member, then either finish or die mid-batch.
            if let Some(first) = self.batch.effects.first() {
                settled.push((
                    first.effect_id.clone(),
                    EffectOutcome::Succeeded { at_ms: 99 },
                ));
            }
            if self.fail_mid_dispatch {
                return Err(PhaseFailure {
                    error: anyhow!("the transport died mid-batch"),
                    pending: Some(self.batch.clone()),
                    records: Vec::new(),
                });
            }
            for effect in self.batch.effects.iter().skip(1) {
                settled.push((
                    effect.effect_id.clone(),
                    EffectOutcome::NotDispatched {
                        at_ms: 99,
                        reason: "the batch bailed".to_string(),
                    },
                ));
            }
            Ok(PhaseReport::exits(BoundaryOutcome::NextIteration))
        }
    }

    fn seam_batch() -> PendingBatch {
        batch(vec![
            outward_effect("llm-1:tool:send-1", Some(act_ref())),
            outward_effect("llm-1:tool:send-2", Some(act_ref())),
        ])
    }

    #[tokio::test]
    async fn every_intent_is_on_disk_before_the_dispatch_runs() {
        // THE SEAM'S ONE CLAIM. Not "the intents end up recorded" — that is also
        // true of a driver that writes them after firing, which is the failure
        // this whole split exists to prevent. What is asserted is what the
        // ledger held at the moment `dispatch_apply` was entered.
        let store = MemoryLoopStateStore::new();
        let key = key("seam-order");
        // `apply_state` and not a hand-placed `cursor.phase`, and the difference
        // is not cosmetic: `verify_journal` runs at every phase entry and
        // refuses a committed cursor its own journal does not replay to. A state
        // moved to `Apply` without the four phase-completion records is
        // `Quarantine::JournalDiverged` before the phase ever runs, which fails
        // this test for a reason that has nothing to do with the seam.
        let state = apply_state(&store, &key).await;
        seed(&store, &key, &state).await;
        let mut host = SeamHost::new(store.clone(), key.clone(), seam_batch());
        let seen = Arc::clone(&host.seen_at_dispatch);
        let calls = Arc::clone(&host.calls);

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "the phase exits its iteration, got {advanced:?}"
        );

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &["gate_apply", "dispatch_apply"],
            "the driver must gate and then dispatch, and must not run Apply whole"
        );
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[
                ("llm-1:tool:send-1".to_string(), false),
                ("llm-1:tool:send-2".to_string(), false),
            ],
            "at the moment the dispatch was entered, EVERY member had a durable \
             intent and NONE had an outcome. A row missing here is an effect \
             about to fire with nothing on disk saying it was attempted"
        );
    }

    #[tokio::test]
    async fn recovery_recomputes_the_plan_after_intent_rearming_before_dispatch() {
        let store = MemoryLoopStateStore::new();
        let key = key("seam-post-intent-plan");
        let committed_batch = batch(vec![outward_effect("llm-1:tool:send-1", Some(act_ref()))]);
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(committed_batch.clone());
        seed(&store, &key, &state).await;
        store
            .record_effect_intent(
                &key,
                &EffectLedgerEntry::intent(&committed_batch.effects[0], 1, Phase::Apply, 10),
            )
            .await
            .expect("intent");
        store
            .record_effect_outcome(
                &key,
                &committed_batch.effects[0].effect_id,
                EffectOutcome::NotDispatched {
                    at_ms: 20,
                    reason: "the earlier batch stopped short".to_string(),
                },
            )
            .await
            .expect("positive evidence licensed the initial recovery plan");

        // Before intent re-recording, NotDispatched plans Refire. Recording the
        // new intent re-arms that evidence to unknown; the outward record now
        // says the act exists, so the only safe post-intent plan is
        // AlreadyFired. Dispatch must receive the second answer, not the stale
        // first one.
        let mut host = SeamHost::new(store.clone(), key.clone(), committed_batch)
            .reconciling_as_already_fired();
        let action = Arc::clone(&host.plan_action_at_dispatch);
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "the refreshed plan settles and advances, got {advanced:?}"
        );
        assert!(matches!(
            action.lock().unwrap().as_ref(),
            Some(EffectAction::AlreadyFired { .. })
        ));
    }

    #[tokio::test]
    async fn a_re_gated_batch_mismatch_is_refused_before_any_intent_row_changes() {
        let store = MemoryLoopStateStore::new();
        let key = key("seam-re-gate-mismatch");
        let committed_batch = batch(vec![outward_effect("llm-1:tool:send-1", Some(act_ref()))]);
        let mut state = apply_state(&store, &key).await;
        state.pending = Some(committed_batch.clone());
        seed(&store, &key, &state).await;
        store
            .record_effect_intent(
                &key,
                &EffectLedgerEntry::intent(&committed_batch.effects[0], 1, Phase::Apply, 10),
            )
            .await
            .expect("intent");
        let not_dispatched = EffectOutcome::NotDispatched {
            at_ms: 20,
            reason: "the earlier batch stopped short".to_string(),
        };
        store
            .record_effect_outcome(
                &key,
                &committed_batch.effects[0].effect_id,
                not_dispatched.clone(),
            )
            .await
            .expect("positive no-fire evidence");

        let different = batch(vec![outward_effect("llm-1:tool:send-2", Some(act_ref()))]);
        let mut host = SeamHost::new(store.clone(), key.clone(), different);
        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(matches!(advanced, Advanced::ExecutionFailed { .. }));

        let ledger = store.load_effects(&key).await.expect("load");
        assert_eq!(
            ledger
                .get(&committed_batch.effects[0].effect_id)
                .and_then(|entry| entry.outcome.as_ref()),
            Some(&not_dispatched),
            "comparison must precede intent re-arming, or a batch that never dispatches loses \
             its positive no-fire evidence"
        );
        assert!(
            ledger
                .get(&EffectId::parse("llm-1:tool:send-2").expect("fixture id"))
                .is_none(),
            "the mismatched batch must not leave even a prepared prefix"
        );
    }

    #[tokio::test]
    async fn a_prepared_prefix_without_the_complete_batch_marker_is_inert() {
        let store = MemoryLoopStateStore::new();
        let key = key("seam-partial-intents");
        let state = apply_state(&store, &key).await;
        seed(&store, &key, &state).await;
        let mut lease = store
            .claim(&key, &worker(), Duration::from_secs(30))
            .await
            .expect("claim");
        let batch = seam_batch();
        store
            .record_effect_intent(
                &key,
                &EffectLedgerEntry::intent(&batch.effects[0], 1, Phase::Apply, 10),
            )
            .await
            .expect("the prefix row lands");

        let mut host = SeamHost::new(store.clone(), key.clone(), batch);
        let (resolved, _) = resolve_effects(&store, &mut host, &key, &state, &mut lease, 20)
            .await
            .expect("an abandoned prepared prefix is not an in-flight effect");
        assert!(resolved.is_empty());
    }

    #[tokio::test]
    async fn what_the_driver_resolved_reaches_the_dispatch_that_must_honour_it() {
        // The seam's third claim, and the one that was missing until 2026-08-29.
        // `resolve_effects` produced a verdict per member from the day it was
        // written; nothing consumed it. `PhaseEntry::effects` told the GATE, and
        // the gate does not fire — so a re-entered `Apply` either refused
        // outright (what the host did) or dispatched the whole batch again.
        //
        // What is asserted is that the dispatch is HANDED the same list, at the
        // moment it could still act on it. Asserting the ledger afterwards would
        // not distinguish this from a driver that resolved and then dropped it.
        let store = MemoryLoopStateStore::new();
        let key = key("seam-plan");
        let state = apply_state(&store, &key).await;
        seed(&store, &key, &state).await;

        // First claim: dies after settling member one. Member one now has an
        // outcome; member two has an intent and nothing else.
        let mut host =
            SeamHost::new(store.clone(), key.clone(), seam_batch()).failing_mid_dispatch();
        let _ = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        // Second claim, by a worker whose outward record answers. Without that
        // answer the run is held at `resolve_effects` and no plan is ever built
        // — which is what `the_unsettled_member_is_what_holds_the_next_claim`
        // measures, and is why that test needs a fake that refuses.
        let mut resumed =
            SeamHost::new(store.clone(), key.clone(), seam_batch()).reconciling_as_already_fired();
        let planned = Arc::clone(&resumed.plan_at_dispatch);
        let calls = Arc::clone(&resumed.calls);
        let advanced = advance_once(&store, &mut resumed, &key, &worker(), &config()).await;

        assert!(
            matches!(advanced, Advanced::Committed { .. }),
            "with both members resolved the claim advances rather than holding, got {advanced:?}"
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &["gate_apply", "dispatch_apply"],
            "the re-entered Apply gates and dispatches; refusing the entry is what \
             this change replaced"
        );
        assert_eq!(
            *planned.lock().unwrap(),
            Some(2),
            "the dispatch must be handed a verdict for EVERY member the driver \
             resolved — the adopted one and the already-fired one. A plan short \
             by one member is a member the phase would fire a second time"
        );
    }

    #[tokio::test]
    async fn a_dispatch_that_dies_mid_batch_leaves_an_intent_for_every_member() {
        // The plan's own acceptance test, and the case the ledger was built for.
        // The members the dispatch never reached have no outcome, and it is the
        // INTENT that says they may have fired. Before the seam there was no
        // intent to find.
        let store = MemoryLoopStateStore::new();
        let key = key("seam-mid-dispatch");
        let state = apply_state(&store, &key).await;
        seed(&store, &key, &state).await;
        let mut host =
            SeamHost::new(store.clone(), key.clone(), seam_batch()).failing_mid_dispatch();

        let advanced = advance_once(&store, &mut host, &key, &worker(), &config()).await;
        assert!(
            matches!(advanced, Advanced::PhaseFailed { .. }),
            "a dispatch that died is a failed attempt, got {advanced:?}"
        );

        let ledger = store.load_effects(&key).await.expect("load");
        let mut rows: Vec<(String, bool)> = ledger
            .entries()
            .map(|entry| (entry.effect_id.to_string(), entry.outcome.is_some()))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("llm-1:tool:send-1".to_string(), true),
                ("llm-1:tool:send-2".to_string(), false),
            ],
            "every member keeps its intent; the one the dispatch settled has an \
             outcome and the one it never reached does not"
        );

        // And the outcome the dying dispatch DID produce survived the error.
        // `settled` is an out-parameter for exactly this: an `Err` cannot carry
        // the members that finished before it.
        let settled_id = EffectId::parse("llm-1:tool:send-1").expect("well-formed");
        assert!(
            matches!(
                ledger.get(&settled_id).and_then(|e| e.outcome.clone()),
                Some(EffectOutcome::Succeeded { .. })
            ),
            "the member that settled before the failure must keep its outcome"
        );
    }

    #[tokio::test]
    async fn the_unsettled_member_is_what_holds_the_next_claim() {
        // The other half of the seam, end to end: the row written before the
        // fire is the row a LATER claim reads. `resolve_effects` no longer gates
        // on `state.pending`, so this is what actually stops a re-run — and the
        // member that has an outcome is not asked about at all.
        let store = MemoryLoopStateStore::new();
        let key = key("seam-resume");
        let state = apply_state(&store, &key).await;
        seed(&store, &key, &state).await;
        let mut host =
            SeamHost::new(store.clone(), key.clone(), seam_batch()).failing_mid_dispatch();
        let _ = advance_once(&store, &mut host, &key, &worker(), &config()).await;

        // A fresh host, as a resuming worker would be. It never gets to gate:
        // the boundary refuses first.
        let mut resumed = SeamHost::new(store.clone(), key.clone(), seam_batch());
        let calls = Arc::clone(&resumed.calls);
        let advanced = advance_once(&store, &mut resumed, &key, &worker(), &config()).await;

        match advanced {
            Advanced::EffectIndeterminate { effect_id, reason } => {
                assert_eq!(
                    effect_id.as_str(),
                    "llm-1:tool:send-2",
                    "the member with no outcome is the one that holds the run; the \
                     settled one is adopted and never asked about"
                );
                // WHICH indeterminate this is matters, and this is the assertion
                // that tells the two apart. Reaching the host at all means the
                // row carried its `reconcile_ref` from the gate, through the
                // store, and back out of `EffectLedgerEntry::pending`. Drop that
                // field on the way to disk and `disposition` still answers
                // `Reconcile`, `resolve_effects` still refuses, and the effect id
                // is still this one — the run would just be held for *nothing
                // can say whether it left* instead of for what the outward record
                // said, and an assertion on the id alone would not notice.
                assert_eq!(
                    reason, "this fake does not read an outward record",
                    "the refusal must come from the outward record the ROW named; \
                     a row that lost its `reconcile_ref` in the store refuses with \
                     a different reason and never reaches the host"
                );
            },
            other => panic!("an unsettled non-retry-safe effect must hold the run, got {other:?}"),
        }
        assert!(
            calls.lock().unwrap().is_empty(),
            "the boundary refuses BEFORE the phase runs; a resume that reached \
             the gate would re-fire the batch"
        );
    }

    #[test]
    fn terminal_receipts_cover_cross_layer_hitl_before_loopstate_publication() {
        let source = include_str!("driver_worker.rs");
        let boundary = source
            .split("let terminal_settlement_receipt =")
            .nth(1)
            .and_then(|tail| tail.split("state.journal_seq = seq").next())
            .expect("terminal receipt boundary");
        assert!(boundary.contains("TerminalKind::WaitingForUser"));
        assert!(boundary.contains("TerminalKind::WaitingForConfirmation"));
        assert!(boundary.contains("TerminalKind::PausedByUser"));
        assert!(boundary.contains("prepare_terminal_settlement_receipt"));
        assert!(boundary.contains(".await"));
    }

    #[test]
    fn receipt_backed_terminal_batch_is_only_projected_by_settlement_aware_lifecycle() {
        let source = include_str!("driver_worker.rs");
        let boundary = source
            .split("let terminal_projection_owned_by_lifecycle =")
            .nth(1)
            .and_then(|tail| tail.split("match report.step").next())
            .expect("receipt-backed terminal projection fence");
        assert!(boundary.contains("state.terminal_settlement_receipt.is_some()"));
        assert!(boundary.contains("if !terminal_projection_owned_by_lifecycle"));
        assert!(boundary.contains("project_outbox(store, host, key, lease, seq).await"));
    }

    #[test]
    fn terminal_pause_exclusion_spans_admission_and_fenced_boundary_commit() {
        let source = include_str!("driver_worker.rs");
        let advance = source
            .split("async fn advance_once")
            .nth(1)
            .and_then(|tail| tail.split("// ============================================================================\n// Guards").next())
            .expect("one-phase driver source");
        let acquire = advance
            .rfind("host.acquire_terminal_settlement_exclusion")
            .expect("terminal lifecycle exclusion");
        let admission = advance[acquire..]
            .find("host.admit_terminal_operator_steers")
            .map(|offset| acquire + offset)
            .expect("terminal steer admission");
        let boundary = advance[admission..]
            .find("commit_boundary(")
            .map(|offset| admission + offset)
            .expect("fenced terminal boundary");
        assert!(acquire < admission && admission < boundary);
        assert!(advance[..acquire].contains("let _terminal_settlement_exclusion"));
    }
}
