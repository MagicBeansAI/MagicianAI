//! Making a send accountable: bind the disclosure to the provider message the
//! tool's own result named — OPC tier 4.
//!
//! # Where this runs, and why it is here rather than at the send
//!
//! The outward gate in [`super::executor`] writes the disclosure and marks the
//! act `dispatching` then `dispatch_unknown` **before** anything leaves, and it
//! is right to: an act recorded after the fact is a record that can be lost by
//! the crash it was meant to survive. But that is also the last moment the gate
//! sees, so the result — the only place a provider ever names the message it
//! created — was discarded.
//!
//! [`settle_outward_dispatch`] is the other end. It is called from
//! `execute_action`, which is the ONE funnel every action's result passes
//! through: `execute_action_inner` returns from a dozen dispatch routes, and a
//! settle wired into some of them would silently miss the rest.
//!
//! # The pre-write is kept, deliberately
//!
//! The act is already `dispatch_unknown` when this runs. That is the fail-closed
//! resting state: if the process dies between the send and its result, the
//! record says *"we do not know"*, which is exactly true. This only ever moves
//! an act FORWARD, and only on an id it actually read.
//!
//! # What it is not allowed to do
//!
//! - It never invents a receipt. No id means the act stays where it is, with a
//!   reason naming which capability and where the id was looked for.
//!   Unreconcilable is a failure to KNOW, not a failure to send.
//! - It never touches a CAPTURED act. A capture leaves the record at
//!   `prepared`, and the store refuses a binding there — a rehearsal must not
//!   acquire a real message id.
//! - It never turns a recording fault into an executor error. The send already
//!   happened; replacing the model's result with an opaque failure would lose
//!   the send AND the reason. A write fault is logged loudly and the act stays
//!   `dispatch_unknown`, which is the honest reading of a binding nobody could
//!   write.
//! - It never writes a delivery-ledger observation. A send response says the
//!   provider took the message, not that anybody received it, and an `accepted`
//!   observation for every send would empty
//!   [`crate::magician_v2::delivery::DeliveryLedger::unreconciled`] — the sweep
//!   whose entire value is naming sends no provider event has come back about.

use tracing::{info, warn};

use super::run_loop::effects::CommittedActRef;
use super::types::AgenticContext;
use super::ActionExecutors;
use crate::magician_v2::agents::outward_actions;
use crate::magician_v2::agents::outward_receipts::{self, ReceiptGap};
use crate::magician_v2::execution::actions::{ActionResult, ExecutableAction};

/// What one settle did. Returned for tests and logs; the executor ignores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// Not an outward dispatch, or nothing this runtime can settle against.
    NotApplicable,
    /// A disclosure exists but nothing left, or it has already settled — a
    /// captured act at `prepared`, an act at `failed` (refused before dispatch,
    /// or settled there afterwards by a receipt sweep), a delivered, corrected
    /// or retracted one. None of these may acquire a provider message.
    NotAwaitingReceipt,
    /// The act is now bound to a provider message.
    Bound {
        provider: String,
        provider_message_id: String,
    },
    /// No binding was made, and the reason was written onto the act.
    ///
    /// Almost always because the result named no usable id. It also covers a
    /// binding the store refused — a second, different id claimed for an act
    /// already bound — which is an error rather than a silent overwrite.
    StillUnknown { reason: String },
}

/// Bind a completed outward dispatch to the provider message it became.
///
/// **The named post-dispatch entry point.** Called once per action from
/// `execute_action`; a no-op for everything that is not a live outward send.
///
/// Deliberately infallible from the caller's side: every failure mode ends in a
/// log line and a [`Settlement`], because the alternative — propagating — would
/// replace a completed send's result with an error and lose both.
pub fn settle_outward_dispatch(
    action: &ExecutableAction,
    executors: &ActionExecutors,
    ctx: &AgenticContext,
    result: &ActionResult,
    // Which attempt is settling. The act was already identified by what it
    // discloses; this names the history line the binding is appended as, so an
    // act that was dispatched, lost its response, and was reconciled later can
    // say which attempt did each of those.
    effect_id: Option<&str>,
) -> Settlement {
    // Cheapest first, and pure: no dispatch that cannot reach anybody may cost
    // a filesystem read here. This is the same classifier the gate ran, asked
    // through the same public function, so the two cannot disagree about which
    // dispatches are outward.
    let ExecutableAction::Pack {
        capability_name,
        resolved_params,
        ..
    } = action
    else {
        return Settlement::NotApplicable;
    };
    let action_token = outward_actions::dispatch_action_token(capability_name, resolved_params);
    if outward_actions::outward_dispatch_class(capability_name, &action_token, resolved_params)
        .is_none()
    {
        return Settlement::NotApplicable;
    }

    let (Some(principal), Some(workspace), Some(artifact_service)) = (
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
        executors.artifact_v2_service.as_ref(),
    ) else {
        // No scoped store, so the gate wrote no disclosure either and there is
        // nothing to bind to.
        //
        // Not a silent pass, and the claim is worth citing rather than
        // asserting, because a reader who checks only `record_outward_disclosure`
        // — which returns `Ok(None)` here and says so — concludes the opposite.
        // The refusal is downstream of that, in `execute_action_inner`'s outward
        // branch, and there are two of them:
        //
        // - `outward_gate::contact_refusal` refuses outright on
        //   `(workspace_layout, principal, workspace)` not all being present:
        //   *"This execution carries no scoped store, so the owner's suppression
        //   register could not be consulted at all."* That covers every class
        //   whose `Addressing` is `RecipientRequired` (mail, message) and every
        //   act that names recipients at all.
        // - The dispatch-record block that follows destructures
        //   `(outward_act_ref, principal, workspace, artifact_service)` and
        //   returns `refuse_outward("NOT SENT — this outward action was
        //   refused…")` when any is absent. `outward_act_ref` is `None` for
        //   exactly this state, so this one covers the remainder —
        //   `MayReachNobody` classes with an empty recipient list, which is the
        //   only shape that survives the suppression screen unscoped.
        //
        // So nothing leaves in this state, which is also why
        // `reconcile_ref_for_dispatch` answers `Unscoped` rather than
        // `Undeterminable` for it: there is no live send to mis-file.
        return Settlement::NotApplicable;
    };

    let store = crate::magician_v2::evidence::OutwardAssertionStore::new(
        artifact_service.workspace().clone(),
    )
    .with_effect_id(effect_id.map(str::to_string));
    let scope = crate::magician_v2::evidence::OutwardScope::new(principal, workspace);

    // The same bytes the gate hashed into the disclosure's payload ref, so the
    // ref derives to the same act. The derivation itself lives in the store,
    // once, for exactly this reason.
    let payload = match serde_json::to_vec(resolved_params) {
        Ok(payload) => payload,
        Err(error) => {
            warn!(
                capability = %capability_name,
                action = %action_token,
                error = %error,
                "[OUTWARD-RECEIPT] the dispatch payload could not be re-serialised, so the \
                 disclosure this send belongs to cannot be named; it stays unreconcilable"
            );
            return Settlement::NotApplicable;
        },
    };
    let act_ref = match store.dispatch_act_ref(&scope, capability_name, &action_token, &payload) {
        Ok(act_ref) => act_ref,
        Err(error) => {
            warn!(
                capability = %capability_name,
                action = %action_token,
                error = %error,
                "[OUTWARD-RECEIPT] the act ref for this dispatch could not be derived; the send \
                 stays unreconcilable"
            );
            return Settlement::NotApplicable;
        },
    };

    bind_result_to_disclosure(
        &store,
        &scope,
        &act_ref,
        capability_name,
        &action_token,
        result.as_text(),
        &chrono::Utc::now().to_rfc3339(),
    )
}

/// What the durable outward record says about a dispatch the loop lost track of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectReconciliation {
    /// A provider took it. The effect exists, whether or not this attempt is the
    /// one that produced it.
    Fired {
        /// Whether a transition on the act names THIS attempt. The act is keyed
        /// by what it discloses, so a byte-identical send from another attempt
        /// resolves to the same record — in which case the effect is real and
        /// this attempt is not its author. For deciding whether to re-send, only
        /// the first fact matters; for explaining what happened, both do.
        by_this_attempt: bool,
        status: crate::magician_v2::evidence::OutwardActStatus,
    },
    /// Positive evidence that nothing left: the act was refused, or never moved
    /// past preparation.
    DidNotFire {
        status: crate::magician_v2::evidence::OutwardActStatus,
    },
    /// No answer. Distinct from `DidNotFire`, and the distinction is the whole
    /// point — an act still at `dispatching`, or one that was never recorded,
    /// is evidence of neither a send nor a non-send.
    StillUnknown { reason: String },
}

/// Ask the outward record whether a dispatch whose outcome the loop lost
/// actually fired.
///
/// §3 of the turn-boundary contract says re-entry is *"idempotent up to the last
/// committed boundary"* — work done by a holder that never reported is discarded,
/// not replayed. `effect_id` made that enforceable; this is what reads it back.
///
/// **No new store.** The contract's own non-goal is "no new effect ledger", and
/// none is needed: the outward act already records what happened, and since the
/// effect identity work its transitions name the attempt that drove them. This
/// reads the record that exists rather than adding one that would have to be
/// kept in step with it.
///
/// Deliberately not called before every dispatch. The question only arises for a
/// dispatch that ran a transport and did not report success — rare, and the only
/// case where `Unknown` is the honest answer. Asking on the happy path would put
/// a filesystem read in front of every tool call to answer a question nobody had.
pub fn reconcile_outward_effect(
    action: &ExecutableAction,
    executors: &ActionExecutors,
    ctx: &AgenticContext,
    effect_id: Option<&str>,
) -> EffectReconciliation {
    // The in-process caller still derives, and may: the same bytes the gate
    // hashed are still in hand here, which is the one place that is true.
    // [`reconcile_committed_effect`] is the boundary-crossing counterpart.
    //
    // Both halves resolve the scope from this same `ctx`, so the scope check
    // inside `reconcile_committed_effect` cannot fail on this path — which is
    // the point. The check is for a pickup whose context is NOT the one that
    // derived the ref, and there is no such thing in process.
    match reconcile_ref_for_dispatch(action, executors, ctx) {
        DispatchReconcileRef::Ref(act_ref) => {
            reconcile_committed_effect(&act_ref, executors, ctx, effect_id)
        },
        // Not outward is not "nothing left" — it is "there was never a record to
        // ask". `StillUnknown` is the only honest answer, and it is what this
        // function has always returned here.
        DispatchReconcileRef::NotOutward { reason }
        | DispatchReconcileRef::Unscoped { reason }
        | DispatchReconcileRef::Undeterminable { reason } => {
            EffectReconciliation::StillUnknown { reason }
        },
    }
}

/// Ask the outward record about a dispatch, by the act ref the `Gate` committed.
///
/// **The entry point a stateless worker uses.** It needs no
/// [`ExecutableAction`] and no resolved params — everything it asks by is the
/// ref `PendingEffect::reconcile_ref` already carries, which is exactly what a
/// worker picking an execution up in `Apply` has.
///
/// [`reconcile_outward_effect`] re-derives that ref from
/// `serde_json::to_vec(resolved_params)`. That is sound only where the **same
/// map instance** the gate hashed is still in hand, which is narrower than it
/// sounds: `resolved_params` is a `HashMap` under the default `RandomState`, and
/// two maps with identical contents carry different hash keys and therefore
/// serialise their entries in different orders. A re-derived ref names a record
/// nobody wrote, and — see [`reconciliation_from_act`] — an absent record at a
/// ref is read as positive evidence that nothing left. Committing the ref at
/// `Gate` time and loading by it here removes the re-derivation, and with it
/// that failure.
///
/// A caller holding `None` must **not** call this with a substitute. `None`
/// means *no outward record names this dispatch* — either it was never outward,
/// or the execution had no scoped store and the outward gate refused the send on
/// the same three missing values. Neither is an answer about a send that left;
/// see the field's own documentation.
///
/// # The committed ref carries its scope, and a disagreement REFUSES
///
/// An act ref folds `principal` and `workspace` into its digest and is then
/// addressed under that scope's root. Until [`CommittedActRef`] existed, the
/// root asked here was rebuilt from `ctx` at pickup: a worker resuming under a
/// different principal or workspace looked in a directory the ref could never
/// name, read nothing, and got [`EffectReconciliation::DidNotFire`] — the same
/// false licence to re-send, arriving through the scope rather than through the
/// ref, and undetectable, because *"this scope has no such act"* and *"this is
/// the wrong scope"* are the same absence.
///
/// The ref now carries the scope it was derived under, and this asks whether
/// that is the scope the pickup is running in. **On disagreement it refuses**
/// with [`EffectReconciliation::StillUnknown`] rather than reading the record
/// under either scope. The alternative — read under the *committed* scope,
/// since that is where the record demonstrably is — was considered and
/// rejected, on three grounds:
///
/// 1. **Refusing is the only answer that cannot be wrong.** `StillUnknown` never
///    licenses a re-send; it surfaces to a person. Reading under the committed
///    scope produces a verdict — `Fired` or `DidNotFire` — from a scope this
///    process is not running under, and a `DidNotFire` produced that way is
///    exactly the sentence that sends a second live message. This is the path
///    where a wrong guess costs a real email, so it guesses at nothing.
/// 2. **It would make a durable row a cross-scope read primitive.** The scope
///    arrives off the wire on a `PendingEffect` loaded from the loop-state
///    store. Every other read and write this execution performs is scoped by
///    `ctx`; letting one wire field redirect a read — and, through
///    [`act_history_names_attempt`], a history read — into another principal's
///    `outward_assertions` tree dissolves the isolation the scope root *is*.
/// 3. **The information is worth more spent on detection than on repair.** The
///    whole gain from committing the scope is that the two absences are finally
///    distinguishable. Reading under the committed scope spends that on
///    papering the mismatch over. An execution's scope is fixed at start, so a
///    disagreement here cannot legitimately arise; a condition that cannot
///    legitimately arise should be loud.
///
/// The cost is real and bounded: a scope legitimately renamed mid-flight turns
/// a reconcilable effect into a question for a person instead of an automatic
/// answer. That is a visible cost paid once, against a silent one paid in sent
/// messages.
pub fn reconcile_committed_effect(
    reconcile_ref: &CommittedActRef,
    executors: &ActionExecutors,
    ctx: &AgenticContext,
    effect_id: Option<&str>,
) -> EffectReconciliation {
    let (store, scope) = match scoped_store(executors, ctx) {
        Ok(pair) => pair,
        Err(reason) => return EffectReconciliation::StillUnknown { reason },
    };
    reconciliation_from_committed(&store, &scope, reconcile_ref, effect_id)
}

/// The whole of [`reconcile_committed_effect`] except obtaining the store.
///
/// Split out for the reason [`reconciliation_from_act`] is split from its two
/// entry points, and for one more: the refusal above is the only thing in this
/// module that stands between a wrong-scope pickup and a re-sent message, and
/// an `ActionExecutors` carrying a live `ArtifactV2Service` is not something a
/// unit test can assemble. Everything that DECIDES is here, where a test with a
/// real store and two scopes can call it; the entry point above is two lines
/// that fetch and delegate.
///
/// The `let … else` is the refusal, not a convenience: [`CommittedActRef`]
/// yields no addressable string except through
/// [`CommittedActRef::act_ref_in_scope`], so there is no way to spell the read
/// this rejects. Replacing it with a read under the *committed* scope — the
/// alternative the entry point's documentation spends forty lines rejecting —
/// changes the answer this function gives for a wrong-scope pickup from
/// `StillUnknown` to a verdict, which is what
/// `a_pickup_in_the_wrong_scope_is_refused_rather_than_answered` fails on.
pub(crate) fn reconciliation_from_committed(
    store: &crate::magician_v2::evidence::OutwardAssertionStore,
    scope: &crate::magician_v2::evidence::OutwardScope,
    reconcile_ref: &CommittedActRef,
    effect_id: Option<&str>,
) -> EffectReconciliation {
    let Some(act_ref) = reconcile_ref.act_ref_in_scope(&scope.principal, &scope.workspace) else {
        return EffectReconciliation::StillUnknown {
            reason: scope_mismatch_reason(reconcile_ref, scope),
        };
    };
    reconciliation_from_act(store, scope, act_ref, effect_id)
}

/// Why a committed ref and the pickup's scope cannot be reconciled.
///
/// Split out for the reason [`reconciliation_from_act`] is split from its two
/// entry points: the sentence an operator reads is worth exercising, and doing
/// it here needs no executor and no context. It is only the *message* — the
/// refusal itself is [`CommittedActRef::act_ref_in_scope`] answering `None`,
/// which is not skippable, because there is no other way to obtain the string
/// that addresses the record.
fn scope_mismatch_reason(
    reconcile_ref: &CommittedActRef,
    scope: &crate::magician_v2::evidence::OutwardScope,
) -> String {
    format!(
        "this act ref was derived under principal {:?} workspace {:?} and the pickup is running \
         under principal {:?} workspace {:?}; the scope is folded into the digest AND is the \
         directory the record lives in, so nothing read here could be an answer about this send",
        reconcile_ref.principal(),
        reconcile_ref.workspace(),
        scope.principal,
        scope.workspace,
    )
}

/// What the `Gate` could learn about which outward record a dispatch belongs to.
///
/// Four answers rather than an `Option`, because three of them are `None`-shaped
/// and they license different things. Collapsing them at the point the ref is
/// *computed* would put the very confusion `PendingEffect::reconcile_ref`'s
/// documentation warns readers about into the writer instead, where no reader
/// could see it.
///
/// # Why [`Self::Unscoped`] is not [`Self::Undeterminable`]
///
/// Both mean *outward, and no ref*. They are separated because only one of them
/// describes a dispatch that can actually send, and a refusal that cannot tell
/// them apart is the shape this branch has already shipped twice: a guard at the
/// chokepoint every caller passes through, in order to constrain one reader's
/// interpretation.
///
/// `Unscoped` is reached when this execution has no principal, no workspace or
/// no artifact service. Those are the same three values the outward gate itself
/// destructures, and it **refuses the send** when any is missing — see
/// [`settle_outward_dispatch`]'s no-scoped-store arm for the two refusal points
/// and the exact messages. So nothing leaves, and there is no live send for a
/// committed `None` to mis-file. Refusing here instead would kill the Apply
/// phase of every run without a scope — the resident driver's default — where
/// today the model receives a legible *"NOT SENT"* result and can adapt.
///
/// `Undeterminable` is reached only **after** the store was obtained, so the
/// scope is present, the outward gate will not refuse on that ground, and the
/// send proceeds. A `None` committed for one of those is the dangerous one, and
/// it is the one the `Gate` refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchReconcileRef {
    /// The dispatch cannot reach anybody — not a pack call, or a pack call the
    /// outward classifier does not class as outward.
    NotOutward { reason: String },
    /// The act ref to commit, bound to the scope it was derived under.
    Ref(CommittedActRef),
    /// Outward, and this execution has no scoped store — so the outward gate
    /// refuses the send on the same three missing values and nothing leaves.
    /// Committed as `None`; see the type's own documentation.
    Unscoped { reason: String },
    /// It **is** outward, a scoped store IS present, and the ref still could not
    /// be named. The send would proceed, so committing `None` here would file a
    /// live send as a dispatch that reaches nobody: the `Gate` refuses the
    /// dispatch instead.
    Undeterminable { reason: String },
}

impl DispatchReconcileRef {
    /// The value to commit on `PendingEffect::reconcile_ref`, or the reason the
    /// `Gate` must refuse the dispatch instead.
    ///
    /// **The only collapse of these four answers into that field's two
    /// states.** Four answers are worth nothing if the collapse is written out
    /// at each commit site, because the wrong one — `Undeterminable => None` —
    /// is the shorter `match` arm and reads as tidying. Here `Err` is reachable
    /// from [`Self::Undeterminable`] and from nowhere else, which is the
    /// property the `Gate` acts on.
    pub fn committed(self) -> Result<Option<CommittedActRef>, String> {
        match self {
            DispatchReconcileRef::NotOutward { .. } => Ok(None),
            DispatchReconcileRef::Ref(act_ref) => Ok(Some(act_ref)),
            DispatchReconcileRef::Unscoped { .. } => Ok(None),
            DispatchReconcileRef::Undeterminable { reason } => Err(reason),
        }
    }
}

/// The scoped outward store this execution reads and writes through.
fn scoped_store(
    executors: &ActionExecutors,
    ctx: &AgenticContext,
) -> Result<
    (
        crate::magician_v2::evidence::OutwardAssertionStore,
        crate::magician_v2::evidence::OutwardScope,
    ),
    String,
> {
    let (Some(principal), Some(workspace), Some(artifact_service)) = (
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
        executors.artifact_v2_service.as_ref(),
    ) else {
        return Err("no scoped store to read".to_string());
    };
    Ok((
        crate::magician_v2::evidence::OutwardAssertionStore::new(
            artifact_service.workspace().clone(),
        ),
        crate::magician_v2::evidence::OutwardScope::new(principal, workspace),
    ))
}

/// The act ref a dispatch will reconcile against, derived and **not written**.
///
/// Called at `Gate` time, while the resolved params are in hand — the same
/// place, and from the same values, as the outward classifier's verdict — so
/// `PendingEffect::reconcile_ref` carries a committed ref instead of leaving a
/// resuming worker to re-derive one.
///
/// The classifier is asked through the same public function the gate and the
/// settle use, so the three cannot disagree about which dispatches are outward.
/// Its pure half runs first: a dispatch that reaches nobody never touches a
/// store.
pub fn reconcile_ref_for_dispatch(
    action: &ExecutableAction,
    executors: &ActionExecutors,
    ctx: &AgenticContext,
) -> DispatchReconcileRef {
    let (capability_name, action_token, resolved_params) =
        match outward_dispatch_coordinates(action) {
            Ok(coordinates) => coordinates,
            Err(not_outward) => return not_outward,
        };
    // Not `NotOutward` from here down. Everything below already knows this
    // dispatch can reach somebody, so every remaining answer is a failure to
    // NAME its record rather than a statement that there is nothing to name.
    // Which of the two `None`-shaped answers it gets turns on whether the send
    // can still happen: see `DispatchReconcileRef`.
    //
    // The order matters for the opposite reason too: an execution with no scoped
    // store still dispatches ordinary local work, and asking for the store first
    // would make every one of those `Undeterminable` and refusable.
    //
    // `Unscoped`, not `Undeterminable`: the three values `scoped_store` wants are
    // the three the outward gate itself destructures before a send, and it
    // refuses the send when any is missing. Nothing leaves in this state, so
    // there is no live send for a committed `None` to mis-file — and refusing
    // here would end the Apply phase of every scopeless run, which is the
    // resident driver's default. See `DispatchReconcileRef`.
    let (store, scope) = match scoped_store(executors, ctx) {
        Ok(pair) => pair,
        Err(reason) => return DispatchReconcileRef::Unscoped { reason },
    };
    // The same bytes the gate hashes into the disclosure's payload ref, so this
    // derives the same act.
    let Ok(payload) = serde_json::to_vec(resolved_params) else {
        return DispatchReconcileRef::Undeterminable {
            reason: "payload could not be re-serialised".to_string(),
        };
    };
    let act_ref = match store.dispatch_act_ref(&scope, capability_name, &action_token, &payload) {
        Ok(act_ref) => act_ref,
        Err(error) => {
            return DispatchReconcileRef::Undeterminable {
                reason: format!("act ref could not be derived: {error}"),
            }
        },
    };
    // The scope is bound to the ref here, at the one moment both are certainly
    // the same one: `dispatch_act_ref` folded exactly these two strings into the
    // digest a line ago. Anywhere later is a re-statement, and a re-statement of
    // which scope a ref belongs to is the failure this pairing exists to close.
    //
    // A refusal here is `Undeterminable`, never `NotOutward`: everything from
    // the classifier down already knows this dispatch can reach somebody, and
    // committing `None` for one of those files a live send as a dispatch that
    // reaches nobody.
    match CommittedActRef::new(act_ref, scope.principal.as_str(), scope.workspace.as_str()) {
        Ok(committed) => DispatchReconcileRef::Ref(committed),
        Err(error) => DispatchReconcileRef::Undeterminable {
            reason: format!("the derived act ref is not one this runtime can commit: {error}"),
        },
    }
}

/// Whether a dispatch can reach anybody, and under what coordinates.
///
/// The **pure** half of naming an act: no store, no filesystem, no clone of a
/// workspace handle. Split out so the `Gate` answers "is this outward" on the
/// cheap path for the overwhelming majority of dispatches that are not — and so
/// that answer can be exercised without an executor, the same split
/// [`bind_result_to_disclosure`] already makes for the write half.
#[allow(clippy::type_complexity)]
fn outward_dispatch_coordinates(
    action: &ExecutableAction,
) -> Result<
    (
        &str,
        String,
        &std::collections::HashMap<String, serde_json::Value>,
    ),
    DispatchReconcileRef,
> {
    let ExecutableAction::Pack {
        capability_name,
        resolved_params,
        ..
    } = action
    else {
        return Err(DispatchReconcileRef::NotOutward {
            reason: "not a pack dispatch".to_string(),
        });
    };
    let action_token = outward_actions::dispatch_action_token(capability_name, resolved_params);
    if outward_actions::outward_dispatch_class(capability_name, &action_token, resolved_params)
        .is_none()
    {
        return Err(DispatchReconcileRef::NotOutward {
            reason: "not an outward dispatch".to_string(),
        });
    }
    Ok((capability_name, action_token, resolved_params))
}

/// What an already-identified act says about the dispatch that named it.
///
/// The decision half, split from the two entry points above for the reason
/// [`bind_result_to_disclosure`] is split from [`settle_outward_dispatch`]:
/// everything before this is finding out WHICH act a dispatch belongs to, and
/// everything here is what the act's answer means. Splitting it also lets the
/// answer be exercised against a real store without an executor.
///
/// # An absent record answers, and that is why the ref must be the committed one
///
/// `Ok(None)` is [`EffectReconciliation::DidNotFire`] — the dispatch never
/// reached the gate's write point, so nothing left. That is sound for the ref
/// the gate actually wrote and false for any other: ask by a ref nobody derived
/// and a real, provider-accepted send answers "nothing left", which is a licence
/// to re-send it.
///
/// The shape check below closes only the *malformed* half of that. It cannot
/// close the half that matters — a ref re-derived from re-serialised params is a
/// perfectly well-formed digest that names a record nobody wrote, so it clears
/// every check here and still answers `DidNotFire`. Nothing but **not
/// re-deriving** closes that one, which is what `PendingEffect::reconcile_ref`
/// is for.
///
/// # `failed` is not one answer
///
/// Two different writers put an act at [`OutwardActStatus::Failed`] and they mean
/// opposite things. The gate's refusal path writes it *before* `mark_dispatching`
/// — nothing left. The delivery-hygiene receipt sweep writes it when every
/// recipient's provider receipt is a hard bounce, which is an act a provider
/// **took** and then rejected. Reading the second as the first hands back
/// [`EffectReconciliation::DidNotFire`] for a message that has already been sent,
/// and a bounce is among the likeliest things to be true of an act by the time a
/// resumed worker asks about it. `dispatched_at` separates them, at no extra
/// read: `load_act`'s fold sets it from the `dispatching` transition, which is
/// written after every refusal path and immediately before the transport runs.
pub fn reconciliation_from_act(
    store: &crate::magician_v2::evidence::OutwardAssertionStore,
    scope: &crate::magician_v2::evidence::OutwardScope,
    act_ref: &str,
    effect_id: Option<&str>,
) -> EffectReconciliation {
    use crate::magician_v2::evidence::OutwardActStatus;

    // An underived ref cannot answer this question, and the honest answer is
    // that we do not know.
    //
    // This is the ONE caller that reads "no record" as positive evidence that
    // nothing was sent. For every other reader of `load_act`, absence is a fine
    // answer — `recipient_compliance`'s duplicate scan walks an index of mixed
    // id kinds and uses the `None` as its discriminator. So the check lives
    // here, at the reader that turns absence into a licence, rather than in the
    // store, where a first cut put it and broke four unrelated callers.
    //
    // `StillUnknown`, not an `Err`: a caller holding a ref it cannot explain
    // must be stopped from re-sending, not stopped from running. The distinction
    // matters because `DidNotFire` is what licenses the re-fire, and the whole
    // point of committing `reconcile_ref` at Gate time is that this branch is
    // unreachable in a correct run — a worker never re-derives the ref, so a
    // malformed one here means something upstream is wrong and guessing is the
    // worst available response.
    if !crate::magician_v2::evidence::outward_assertions::is_derived_act_ref(act_ref) {
        return EffectReconciliation::StillUnknown {
            reason: format!(
                "the act ref {:?} is not the shape derive_act_ref produces, so whether this \
                 effect left cannot be established from it",
                act_ref.chars().take(64).collect::<String>()
            ),
        };
    }

    let act = match store.load_act(scope, act_ref) {
        Ok(Some(act)) => act,
        Ok(None) => {
            return EffectReconciliation::DidNotFire {
                status: OutwardActStatus::Prepared,
            }
        },
        Err(error) => {
            return EffectReconciliation::StillUnknown {
                reason: format!("the disclosure could not be read: {error}"),
            }
        },
    };

    // Whether a transport ever ran for this act. Everything below that reads a
    // resting state as *nothing left* rests on this being false.
    let dispatched = act.dispatched_at.is_some();
    let by_this_attempt = || {
        effect_id
            .map(|id| act_history_names_attempt(store, scope, act_ref, id))
            .unwrap_or(false)
    };

    match act.status {
        // A provider took it. Later states are reached only through acceptance,
        // so they answer the same question the same way.
        OutwardActStatus::ProviderAccepted
        | OutwardActStatus::Delivered
        | OutwardActStatus::Corrected
        | OutwardActStatus::Retracted => EffectReconciliation::Fired {
            by_this_attempt: by_this_attempt(),
            status: act.status,
        },
        // Refused before anything left, or never moved past preparation. The
        // `dispatched_at` guard is what makes that sentence true rather than
        // merely intended — see this function's own documentation on the two
        // writers of `failed`.
        OutwardActStatus::Failed | OutwardActStatus::Prepared if !dispatched => {
            EffectReconciliation::DidNotFire { status: act.status }
        },
        // Dispatched, and settled as failed afterwards. A provider named the
        // message, so the effect exists whatever became of it since: a hard
        // bounce is a send that was delivered to a provider and rejected, never
        // a send that did not happen.
        OutwardActStatus::Failed | OutwardActStatus::Prepared
            if act.provider_message_id.is_some() =>
        {
            EffectReconciliation::Fired {
                by_this_attempt: by_this_attempt(),
                status: act.status,
            }
        },
        // Dispatched, settled as failed, and no provider ever named a message.
        // The transport ran; what it did is not recorded here, and guessing
        // either way is the one thing this function may not do.
        OutwardActStatus::Failed | OutwardActStatus::Prepared => {
            EffectReconciliation::StillUnknown {
                reason: format!(
                    "the act rests at {:?} after it was dispatched, and no provider ever named \
                     the message it became",
                    act.status
                ),
            }
        },
        // Held open on purpose. `dispatch_unknown` is the store's way of saying
        // it does not know either, and `dispatching` means the answer has not
        // arrived yet — neither may be read as a non-send.
        OutwardActStatus::Dispatching | OutwardActStatus::DispatchUnknown => {
            EffectReconciliation::StillUnknown {
                reason: format!("the act rests at {:?}", act.status),
            }
        },
    }
}

/// Whether any transition on the act names this attempt.
fn act_history_names_attempt(
    store: &crate::magician_v2::evidence::OutwardAssertionStore,
    scope: &crate::magician_v2::evidence::OutwardScope,
    act_ref: &str,
    effect_id: &str,
) -> bool {
    store
        .act_attempts(scope, act_ref)
        .map(|attempts| attempts.iter().any(|recorded| recorded == effect_id))
        .unwrap_or(false)
}

/// Bind one already-identified disclosure to whatever its result named.
///
/// The decision half, split from [`settle_outward_dispatch`] so it can be
/// exercised against a real store without an executor: everything above this is
/// finding out WHICH disclosure a dispatch belongs to, and everything here is
/// what may be done to it.
#[allow(clippy::too_many_arguments)]
pub fn bind_result_to_disclosure(
    store: &crate::magician_v2::evidence::OutwardAssertionStore,
    scope: &crate::magician_v2::evidence::OutwardScope,
    act_ref: &str,
    capability: &str,
    action: &str,
    result_text: Option<&str>,
    now: &str,
) -> Settlement {
    use crate::magician_v2::evidence::OutwardActStatus;

    let act = match store.load_act(scope, act_ref) {
        Ok(Some(act)) => act,
        // No disclosure at this ref: the dispatch never reached the gate's
        // write point. Nothing to bind, and nothing to invent.
        Ok(None) => return Settlement::NotApplicable,
        Err(error) => {
            warn!(
                capability = %capability,
                outward_act_ref = %act_ref,
                error = %error,
                "[OUTWARD-RECEIPT] the disclosure for this send could not be read, so no \
                 provider message can be bound to it; it stays unreconcilable"
            );
            return Settlement::NotApplicable;
        },
    };

    // Only a send that LEFT can carry a receipt. A captured act rests at
    // `prepared` and a refused one at `failed`, and neither may acquire a
    // provider message: a rehearsal that became reconcilable would be evidence
    // of a send that never happened. A settled act — delivered, retracted,
    // corrected — does not reopen. The store refuses all of these as well; this
    // check is what keeps every captured dispatch from producing a loud error
    // line on the way there.
    //
    // `ProviderAccepted` IS admitted, and deliberately: an identical retry
    // resolves to the same act, and *"a replay resumes, a changed payload is an
    // error"* is the store's rule to apply, not one to pre-empt by refusing to
    // ask. Skipping here would make a replay silently different from a first
    // call, and a second id under one act would go unreported.
    if !matches!(
        act.status,
        OutwardActStatus::Dispatching
            | OutwardActStatus::DispatchUnknown
            | OutwardActStatus::ProviderAccepted
    ) {
        return Settlement::NotAwaitingReceipt;
    }

    match outward_receipts::provider_message_from_result(capability, action, result_text) {
        Ok(message) => {
            if let Err(error) = store.record_provider_message(
                scope,
                act_ref,
                &message.provider,
                &message.provider_message_id,
                now,
            ) {
                warn!(
                    capability = %capability,
                    outward_act_ref = %act_ref,
                    provider = %message.provider,
                    error = %error,
                    "[OUTWARD-RECEIPT] the provider message could not be bound to this \
                     disclosure; the send stays unreconcilable"
                );
                return Settlement::StillUnknown {
                    reason: error.to_string(),
                };
            }
            info!(
                capability = %capability,
                outward_act_ref = %act_ref,
                provider = %message.provider,
                provider_message_id = %message.provider_message_id,
                "[OUTWARD-RECEIPT] this send is now a named provider message and can be \
                 reconciled against"
            );
            Settlement::Bound {
                provider: message.provider,
                provider_message_id: message.provider_message_id,
            }
        },
        Err(gap) => record_gap(store, scope, act_ref, capability, gap, now),
    }
}

/// Write WHY this send cannot be reconciled onto the act, and leave it unknown.
///
/// The act is already `dispatch_unknown` from the pre-dispatch write, whose
/// reason can only say that nothing has acknowledged it yet. This appends the
/// specific one — which capability, and where the id was looked for — because a
/// permanent unknown nobody can act on is the state this tier exists to end.
/// The status does not move: appending the same state with a better reason is a
/// note in the history, not a transition.
///
/// Every gap is recorded, including *"this capability has no described
/// send-response shape"*. That reason reads like noise until you notice where
/// it can occur: this function is reached only for a dispatch the gate already
/// classified as outward, whose disclosure exists and is awaiting a receipt. So
/// it does not mean *"an ordinary tool call"* — it means **this channel can
/// never be reconciled until its skill changes**, which is the single most
/// useful thing an owner can be told about a rail.
fn record_gap(
    store: &crate::magician_v2::evidence::OutwardAssertionStore,
    scope: &crate::magician_v2::evidence::OutwardScope,
    act_ref: &str,
    capability: &str,
    gap: ReceiptGap,
    now: &str,
) -> Settlement {
    let reason = gap.reason();
    if let Err(error) = store.mark_dispatch_unknown(scope, act_ref, &reason, now) {
        warn!(
            capability = %capability,
            outward_act_ref = %act_ref,
            error = %error,
            "[OUTWARD-RECEIPT] the reason this send cannot be reconciled could not be recorded"
        );
    } else {
        warn!(
            capability = %capability,
            outward_act_ref = %act_ref,
            reason = %reason,
            "[OUTWARD-RECEIPT] this send left and cannot be reconciled: unreconcilable is a \
             failure to KNOW, never evidence that nothing was sent"
        );
    }
    Settlement::StillUnknown { reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::evidence::{
        OutwardActStatus, OutwardAssertionStore, OutwardChannel, OutwardScope, PrepareOutwardAct,
    };

    const AGENTMAIL_RESULT: &str = r#"{"message_id":"msg_01H8","thread_id":"thr_77"}"#;

    fn store() -> (tempfile::TempDir, OutwardAssertionStore, OutwardScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let store = OutwardAssertionStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, store, OutwardScope::new("anonymous", "default"))
    }

    fn request(key: &str) -> PrepareOutwardAct {
        PrepareOutwardAct {
            idempotency_key: key.to_string(),
            program_id: None,
            engagement_id: None,
            exact_payload_artifact_ref: "payload://blake3:deadbeef".to_string(),
            effective_sender: "presto".to_string(),
            intended_audience: vec!["investor@example.com".to_string()],
            channel: OutwardChannel::Email,
            consequence_class: "bounded_communication".to_string(),
        }
    }

    /// The state a LIVE send reaches on the dispatch path: recorded, in flight,
    /// and parked at `dispatch_unknown` before anything came back.
    fn live_send(store: &OutwardAssertionStore, scope: &OutwardScope, key: &str) -> String {
        let act = store.prepare(scope, &request(key), "t0").expect("prepare");
        let act_ref = act.outward_act_ref;
        store.mark_dispatching(scope, &act_ref, "t1").expect("t1");
        store
            .mark_dispatch_unknown(scope, &act_ref, "dispatched; no provider message yet", "t2")
            .expect("t2");
        act_ref
    }

    // ────────────────────────────────────────────────────────────────────
    // Reading the attempt key back — turn-boundary contract §3
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn an_acts_history_names_the_attempts_that_drove_it() {
        // The reader for a field that was written and never read. Without it the
        // attempt id on a transition answers nothing, and §3's "work done by a
        // holder that never reported is discarded, not replayed" stays a rule
        // with no way to evaluate it.
        let (_tmp, base, scope) = store();
        let act = base
            .prepare(&scope, &request("attempts"), "t0")
            .expect("prepare");
        let act_ref = act.outward_act_ref;

        // The disclosure line names no attempt: the act is keyed by what it
        // discloses, so its creation has no author.
        assert!(base
            .act_attempts(&scope, &act_ref)
            .expect("read")
            .is_empty());

        let first = base
            .clone()
            .with_effect_id(Some("llm_a:tool:t1".to_string()));
        first.mark_dispatching(&scope, &act_ref, "t1").expect("t1");
        let second = base
            .clone()
            .with_effect_id(Some("llm_b:tool:t2".to_string()));
        second
            .mark_dispatch_unknown(&scope, &act_ref, "no provider message yet", "t2")
            .expect("t2");
        // A transition from a handle that names no attempt is skipped, not
        // represented: "some attempt we cannot name" answers nothing.
        base.mark_dispatch_unknown(&scope, &act_ref, "still nothing", "t3")
            .expect("t3");

        assert_eq!(
            base.act_attempts(&scope, &act_ref).expect("read"),
            vec!["llm_a:tool:t1".to_string(), "llm_b:tool:t2".to_string()],
            "every attempt that drove the act, in order, and only those it can name"
        );
    }

    #[test]
    fn an_unknown_act_is_not_evidence_that_nothing_was_sent() {
        // The distinction the whole reconciliation rests on. An act parked at
        // `dispatch_unknown` is the store saying it does not know either — read
        // as "did not fire", a live send would be re-issued.
        let (_tmp, base, scope) = store();
        let act_ref = live_send(&base, &scope, "held-open");
        let act = base.load_act(&scope, &act_ref).expect("load").expect("act");
        assert_eq!(act.status, OutwardActStatus::DispatchUnknown);
    }

    #[test]
    fn a_provider_accepted_act_says_the_effect_exists() {
        // The answer that matters: something left, so a retry would duplicate
        // it. Whether THIS attempt is the one that sent it is a second question —
        // the act is content-keyed, so a byte-identical send from another attempt
        // resolves to the same record.
        let (_tmp, base, scope) = store();
        let act_ref = live_send(&base, &scope, "accepted");
        let sender = base
            .clone()
            .with_effect_id(Some("llm_a:tool:t1".to_string()));
        sender
            .record_provider_message(&scope, &act_ref, "agentmail", "msg_01H8", "t3")
            .expect("bind");

        let act = base.load_act(&scope, &act_ref).expect("load").expect("act");
        assert_eq!(act.status, OutwardActStatus::ProviderAccepted);
        assert!(
            base.act_attempts(&scope, &act_ref)
                .expect("read")
                .iter()
                .any(|a| a == "llm_a:tool:t1"),
            "the attempt that bound the provider message must be findable"
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Reconciling by a COMMITTED ref — stateless-driver decision 1
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn a_ref_one_character_off_reads_a_real_send_as_evidence_that_nothing_left() {
        // The whole argument for committing `PendingEffect::reconcile_ref`, as
        // one pair of assertions. An absent record at a ref is read as positive
        // evidence that nothing left — sound for the ref the gate wrote, and a
        // silent licence to re-send a live message for any other.
        //
        // What the shape guard does and does not buy is worth stating exactly,
        // because the two are easy to run together. It refuses a MALFORMED ref,
        // which is a ref no derivation produces. It does not refuse a
        // re-derived one: a ref re-derived from re-serialised params is a
        // well-formed digest, clears every check here, and answers `DidNotFire`
        // — which the third assertion below pins, deliberately. Only not
        // re-deriving closes that, and that is the field, not this guard.
        let (_tmp, base, scope) = store();
        let act_ref = live_send(&base, &scope, "committed-ref");
        let sender = base
            .clone()
            .with_effect_id(Some("llm_a:tool:t1".to_string()));
        sender
            .record_provider_message(&scope, &act_ref, "agentmail", "msg_01H8", "t3")
            .expect("bind");

        assert_eq!(
            reconciliation_from_act(&base, &scope, &act_ref, Some("llm_a:tool:t1")),
            EffectReconciliation::Fired {
                by_this_attempt: true,
                status: OutwardActStatus::ProviderAccepted,
            },
            "asked by the ref the gate wrote, the record says the message left"
        );

        // A ref one character off is not the derived shape, so it is refused
        // before it can be read. This assertion used to expect `DidNotFire`,
        // demonstrating the hazard; the guard in `reconciliation_from_act` now
        // closes it, and the test asserts the closure instead of the wound.
        //
        // `StillUnknown` is the whole point: it is the one answer that neither
        // re-sends nor claims delivery.
        let one_off = format!("{act_ref}0");
        assert_ne!(one_off, act_ref);
        assert!(
            matches!(
                reconciliation_from_act(&base, &scope, &one_off, Some("llm_a:tool:t1")),
                EffectReconciliation::StillUnknown { .. }
            ),
            "a ref nobody derived must not answer that nothing left; that answer is a \
             licence to re-send a live message"
        );

        // The guard must not be satisfiable by shape alone: a ref that IS the
        // derived shape and names nothing still answers `DidNotFire`, which is
        // the behaviour the re-fire path depends on. Without this, deleting the
        // `load_act` call entirely and always returning `StillUnknown` would
        // pass the assertion above.
        let derived_but_absent = format!("act-{}", "0123456789abcdef".repeat(2));
        assert_eq!(
            reconciliation_from_act(&base, &scope, &derived_but_absent, Some("llm_a:tool:t1")),
            EffectReconciliation::DidNotFire {
                status: OutwardActStatus::Prepared,
            },
            "a well-formed ref naming no record is still positive evidence nothing left"
        );
    }

    #[test]
    fn a_bounced_send_is_not_evidence_that_nothing_left() {
        // The worst answer this module can give, and `failed` was giving it.
        //
        // Two writers put an act at `failed`. The gate's refusal path writes it
        // BEFORE `mark_dispatching`, and that one does mean nothing left. The
        // delivery-hygiene receipt sweep writes it when every recipient's
        // provider receipt is a hard bounce — `receipts::may_write` admits
        // `provider_accepted -> failed` explicitly — and that act was TAKEN by a
        // provider and then rejected. Read as the first, it hands back
        // `DidNotFire`, which `ReconciledEffect::SafeToRefire` turns into a
        // re-send of a message that has already gone out.
        let (_tmp, base, scope) = store();
        let sender = base
            .clone()
            .with_effect_id(Some("llm_a:tool:t1".to_string()));

        // Sent, accepted by the provider, then hard-bounced.
        let bounced = live_send(&base, &scope, "bounced");
        sender
            .record_provider_message(&scope, &bounced, "agentmail", "msg_01H8", "t3")
            .expect("bind");
        base.mark_failed(&scope, &bounced, "every recipient hard bounced", "t4")
            .expect("the receipt sweep settles it");
        assert_eq!(
            base.load_act(&scope, &bounced)
                .expect("load")
                .expect("act")
                .status,
            OutwardActStatus::Failed,
            "the fixture must actually reach `failed`, or it tests nothing"
        );
        assert_eq!(
            reconciliation_from_act(&base, &scope, &bounced, Some("llm_a:tool:t1")),
            EffectReconciliation::Fired {
                by_this_attempt: true,
                status: OutwardActStatus::Failed,
            },
            "a provider named the message, so the effect exists whatever became of it since"
        );

        // Refused before anything left: the other writer, and the reading the
        // arm was originally written for. This must still be `DidNotFire`, or
        // the fix has simply moved the failure to the other side.
        let refused = base
            .prepare(&scope, &request("refused"), "t0")
            .expect("prepare")
            .outward_act_ref;
        base.mark_failed(&scope, &refused, "refused: recipient suppressed", "t1")
            .expect("the gate refuses it");
        assert_eq!(
            reconciliation_from_act(&base, &scope, &refused, Some("llm_a:tool:t1")),
            EffectReconciliation::DidNotFire {
                status: OutwardActStatus::Failed,
            },
            "an act refused before `mark_dispatching` never left, and re-firing it is the point"
        );

        // Dispatched, settled as failed, and no provider ever named a message.
        // Neither answer is available, and `StillUnknown` is the only one that
        // does not invent evidence.
        let opaque = live_send(&base, &scope, "opaque");
        base.mark_failed(&scope, &opaque, "the channel reported a failure", "t3")
            .expect("settle");
        assert!(
            matches!(
                reconciliation_from_act(&base, &scope, &opaque, Some("llm_a:tool:t1")),
                EffectReconciliation::StillUnknown { .. }
            ),
            "a transport that ran and left no provider id answers neither question"
        );
    }

    #[test]
    fn a_pickup_in_the_wrong_scope_cannot_address_the_record_at_all() {
        // The failure this pairing exists to delete, demonstrated at the store.
        //
        // A real send is recorded under `anonymous/default` and left at
        // `dispatch_unknown` — the fail-closed resting state of a send that
        // left and whose response was lost. A worker resuming this execution
        // under a DIFFERENT principal used to rebuild the store root from its
        // own context, join the very same ref under it, read an empty
        // directory, and get `DidNotFire { Prepared }`: positive evidence that
        // nothing was sent, which is a licence to send it again.
        //
        // The two halves below are the proof, and they are separate claims. The
        // first is that the wrong scope genuinely reads as "nothing here" — so
        // the hazard is real and not an argument about a case that cannot
        // happen. The second is that the committed ref will not hand out the
        // string needed to spell that read.
        let (_tmp, base, scope) = store();
        let act_ref = live_send(&base, &scope, "wrong-scope");
        let committed = CommittedActRef::new(
            act_ref.as_str(),
            scope.principal.as_str(),
            scope.workspace.as_str(),
        )
        .expect("the store mints the derived shape");

        let elsewhere = OutwardScope::new("someone-else", scope.workspace.as_str());
        assert_eq!(
            reconciliation_from_act(&base, &elsewhere, &act_ref, Some("llm_a:tool:t1")),
            EffectReconciliation::DidNotFire {
                status: OutwardActStatus::Prepared,
            },
            "the wrong scope reads as an absent record, which is read as evidence nothing left — \
             this is the hazard, and it is why the ref may not be addressable without its scope"
        );

        assert_eq!(
            committed.act_ref_in_scope(&elsewhere.principal, &elsewhere.workspace),
            None,
            "a committed ref must not yield an addressable string to a scope that did not \
             derive it"
        );
        assert_eq!(
            committed.act_ref_in_scope(&scope.principal, &scope.workspace),
            Some(act_ref.as_str()),
            "and it must still yield one to the scope that did, or reconciliation is dead"
        );

        // The sentence an operator gets, and the two scopes named in it. A
        // refusal nobody can act on is a hang with a log line.
        let reason = scope_mismatch_reason(&committed, &elsewhere);
        assert!(reason.contains("someone-else"), "{reason}");
        assert!(reason.contains(&scope.principal), "{reason}");
    }

    #[test]
    fn only_a_dispatch_that_could_still_send_refuses_instead_of_committing_none() {
        // Four answers are worth nothing if each commit site writes the collapse
        // itself, because the dangerous arm — `Undeterminable => None` — is the
        // shorter one and reads as tidying. `committed` is the single collapse,
        // and `Err` is reachable from `Undeterminable` alone.
        assert_eq!(
            DispatchReconcileRef::NotOutward {
                reason: "not a pack dispatch".to_string()
            }
            .committed(),
            Ok(None)
        );
        let committed_ref = CommittedActRef::new(
            format!("act-{}", "0123456789abcdef".repeat(2)),
            "anonymous",
            "default",
        )
        .expect("a derived-shape ref");
        assert_eq!(
            DispatchReconcileRef::Ref(committed_ref.clone()).committed(),
            Ok(Some(committed_ref))
        );
        assert_eq!(
            DispatchReconcileRef::Undeterminable {
                reason: "act ref could not be derived: boom".to_string()
            }
            .committed(),
            Err("act ref could not be derived: boom".to_string()),
            "an outward dispatch that WILL send and whose record cannot be named must refuse, \
             never commit `None`"
        );
        // And the arm that must NOT refuse, which is the whole reason there are
        // four answers rather than three. An execution with no scoped store
        // cannot send at all — `outward_gate::contact_refusal` and the
        // dispatch-record block both refuse on the same three missing values —
        // so there is no live send for this `None` to mis-file. Folding it back
        // into `Undeterminable` ends the Apply phase of every scopeless run,
        // which is the resident driver's default, and this assertion is what
        // fails when somebody does.
        assert_eq!(
            DispatchReconcileRef::Unscoped {
                reason: "no scoped store to read".to_string()
            }
            .committed(),
            Ok(None),
            "a dispatch the outward gate will refuse anyway must not also fail the phase"
        );
    }

    #[test]
    fn a_pickup_in_the_wrong_scope_is_refused_rather_than_answered() {
        // The production entry point's decision, against a real store.
        //
        // `a_pickup_in_the_wrong_scope_cannot_address_the_record_at_all` pins the
        // ACCESSOR — that `CommittedActRef` withholds the string. This pins what
        // the reconciliation path does with that refusal, which is the half that
        // touches a live send. Two named regressions fail here and pass there:
        //
        // - Reaching the bare ref another way (`Display` renders
        //   `act-…@principal/workspace`, and splitting on `@` yields a string
        //   `is_derived_act_ref` accepts) and dropping the `let … else`. That
        //   restores `Fired` for a scope this process is not running under.
        // - Reading under the COMMITTED scope instead of refusing — the
        //   alternative `reconcile_committed_effect`'s documentation rejects at
        //   length. That also answers `Fired`.
        //
        // Both are verdicts. Only `StillUnknown` is neither a re-send nor a
        // claim of delivery, and only `StillUnknown` passes.
        let (_tmp, base, scope) = store();
        let act_ref = live_send(&base, &scope, "wrong-scope-entry");
        let sender = base
            .clone()
            .with_effect_id(Some("llm_a:tool:t1".to_string()));
        sender
            .record_provider_message(&scope, &act_ref, "agentmail", "msg_01H8", "t3")
            .expect("bind");
        let committed = CommittedActRef::new(
            act_ref.as_str(),
            scope.principal.as_str(),
            scope.workspace.as_str(),
        )
        .expect("the store mints the derived shape");

        let elsewhere = OutwardScope::new("someone-else", scope.workspace.as_str());
        let refused =
            reconciliation_from_committed(&base, &elsewhere, &committed, Some("llm_a:tool:t1"));
        match &refused {
            EffectReconciliation::StillUnknown { reason } => {
                assert!(reason.contains("someone-else"), "{reason}");
                assert!(reason.contains(scope.principal.as_str()), "{reason}");
            },
            other => {
                panic!("a pickup under the wrong scope must be refused, not answered: {other:?}")
            },
        }

        // And the right scope still gets its answer, or the refusal has eaten
        // reconciliation entirely — a guard that refuses everything passes the
        // assertion above while protecting nothing.
        assert_eq!(
            reconciliation_from_committed(&base, &scope, &committed, Some("llm_a:tool:t1")),
            EffectReconciliation::Fired {
                by_this_attempt: true,
                status: OutwardActStatus::ProviderAccepted,
            },
            "the scope that derived the ref must still be able to read the record"
        );
    }

    #[test]
    fn the_gate_computes_a_ref_only_for_a_dispatch_that_can_reach_somebody() {
        // `None` on the committed row means "not outward", so the writer has to
        // be as careful about that word as the reader is. A capability that can
        // send gets a ref; one that cannot gets `NotOutward` — and neither is
        // allowed to arrive as the other, which is why this returns three
        // answers rather than an `Option`.
        let (_tmp, store, scope) = store();
        let sending_argv: std::collections::HashMap<String, serde_json::Value> = [(
            "args".to_string(),
            serde_json::json!(["+send", "--to", "x@example.com"]),
        )]
        .into_iter()
        .collect();
        let pack = |capability: &str| ExecutableAction::Pack {
            capability_name: capability.to_string(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: "test".to_string(),
                },
            resolved_params: sending_argv.clone(),
        };

        let sending = pack("gmail");
        let (capability, action_token, params) = outward_dispatch_coordinates(&sending)
            .expect("an argv passthrough on a mail capability can send");
        let act_ref = store
            .dispatch_act_ref(
                &scope,
                capability,
                &action_token,
                &serde_json::to_vec(params).expect("encode"),
            )
            .expect("derive");

        // The shape the loop-side check refuses anything but. The committed ref
        // addresses a file, so `PendingEffect`'s deserializer refuses one that is
        // not the derived form; if `derive_act_ref` ever changes shape that has
        // to fail HERE, loudly, rather than by quietly making every committed
        // ref in the system unloadable.
        crate::magician_v2::execution::agentic::run_loop::effects::validate_reconcile_ref(&act_ref)
            .expect("a derived act ref must satisfy the shape the committed field is checked for");

        // The same parameters on a capability that reaches nobody.
        assert!(
            matches!(
                outward_dispatch_coordinates(&pack("websearch")),
                Err(DispatchReconcileRef::NotOutward { .. })
            ),
            "a passthrough on local work is not an outward act"
        );
        assert!(matches!(
            outward_dispatch_coordinates(&ExecutableAction::SpawnSubGoal {
                goal: "anything".to_string(),
                budget: 1,
            }),
            Err(DispatchReconcileRef::NotOutward { .. })
        ));
    }

    /// The whole claim of this tier, end to end.
    ///
    /// Before it, an AgentMail send named its message id in the result and the
    /// runtime threw it away, so a bounce carrying that id had no route back to
    /// the disclosure. This pins that the id survives the send and that the
    /// disclosure is reachable FROM the id — the only direction a provider
    /// event can travel.
    #[test]
    fn a_live_send_becomes_findable_from_the_provider_message_it_named() {
        let (_tmp, store, scope) = store();
        let act_ref = live_send(&store, &scope, "send-1");

        let settlement = bind_result_to_disclosure(
            &store,
            &scope,
            &act_ref,
            "agentmail-send",
            "send",
            Some(AGENTMAIL_RESULT),
            "t3",
        );

        assert_eq!(
            settlement,
            Settlement::Bound {
                provider: "agentmail".to_string(),
                provider_message_id: "msg_01H8".to_string(),
            }
        );
        let act = store
            .load_act(&scope, &act_ref)
            .expect("load")
            .expect("present");
        assert_eq!(act.provider_message_id.as_deref(), Some("msg_01H8"));
        assert_eq!(
            act.status,
            OutwardActStatus::ProviderAccepted,
            "the provider took it; acceptance is not delivery and the state must not claim it"
        );
        assert_eq!(
            store
                .act_for_provider_message(&scope, "agentmail", "msg_01H8")
                .expect("lookup"),
            Some(act_ref)
        );
    }

    /// Capture mode must never manufacture a receipt.
    ///
    /// A captured act rests at `prepared` — composed, nothing sent. If a
    /// rehearsal could acquire a real provider message id, a send that never
    /// happened would be reconcilable evidence that it did, and capture mode
    /// would stop being a rehearsal.
    #[test]
    fn a_captured_act_is_never_bound_even_when_a_result_names_a_message() {
        let (_tmp, store, scope) = store();
        let act = store
            .prepare(&scope, &request("captured"), "t0")
            .expect("prepare");

        let settlement = bind_result_to_disclosure(
            &store,
            &scope,
            &act.outward_act_ref,
            "agentmail-send",
            "send",
            Some(AGENTMAIL_RESULT),
            "t1",
        );

        assert_eq!(settlement, Settlement::NotAwaitingReceipt);
        let reloaded = store
            .load_act(&scope, &act.outward_act_ref)
            .expect("load")
            .expect("present");
        assert_eq!(reloaded.status, OutwardActStatus::Prepared);
        assert_eq!(reloaded.provider_message_id, None);
        assert_eq!(
            store
                .act_for_provider_message(&scope, "agentmail", "msg_01H8")
                .expect("lookup"),
            None,
            "nothing may be filed against a rehearsal"
        );
    }

    /// A send that cannot be reconciled stays UNKNOWN, and the record says why.
    ///
    /// The distinction this tier exists to keep: unreconcilable is a failure to
    /// KNOW, not a failure to send. The act must not move to `failed`, and it
    /// must not move forward either.
    #[test]
    fn a_send_with_no_recoverable_id_stays_unknown_and_names_the_reason() {
        let (_tmp, store, scope) = store();
        let act_ref = live_send(&store, &scope, "send-1");

        // `imessage_send` returns AppleScript's exit status and nothing that
        // identifies the message, so this channel genuinely cannot be
        // reconciled until the handler changes.
        let settlement = bind_result_to_disclosure(
            &store,
            &scope,
            &act_ref,
            "imessage_send",
            "execute",
            Some(r#"{"status":"ok","to":"+15551234567","exit_code":0}"#),
            "t3",
        );

        let Settlement::StillUnknown { reason } = settlement else {
            panic!("an act with no recoverable id must stay unknown, got {settlement:?}");
        };
        assert!(
            reason.contains("imessage_send"),
            "the reason must name the channel that cannot be reconciled: {reason}"
        );

        let act = store
            .load_act(&scope, &act_ref)
            .expect("load")
            .expect("present");
        assert_eq!(
            act.status,
            OutwardActStatus::DispatchUnknown,
            "a send nobody can identify is unknown, never failed and never accepted"
        );
        assert_eq!(act.provider_message_id, None);
        assert!(
            act.status.is_active_disclosure(),
            "it may have arrived, so a correction obligation must still be able to attach"
        );
        assert_eq!(
            store
                .load_act_history(&scope, &act_ref)
                .expect("history")
                .len(),
            4,
            "the specific reason is appended as a note, not swallowed: prepared, dispatching, \
             dispatch_unknown, dispatch_unknown"
        );
    }

    /// A send whose own result went missing is still a send.
    #[test]
    fn an_unreadable_result_leaves_the_act_unknown_rather_than_failed() {
        let (_tmp, store, scope) = store();
        let act_ref = live_send(&store, &scope, "send-1");

        let settlement = bind_result_to_disclosure(
            &store,
            &scope,
            &act_ref,
            "agentmail-send",
            "send",
            None,
            "t3",
        );

        let Settlement::StillUnknown { reason } = settlement else {
            panic!("no text is no id, got {settlement:?}");
        };
        assert!(reason.contains("no readable text"), "{reason}");
        assert_eq!(
            store
                .load_act(&scope, &act_ref)
                .expect("load")
                .expect("present")
                .status,
            OutwardActStatus::DispatchUnknown
        );
    }

    /// A dispatch whose disclosure was never written binds nothing, and
    /// certainly does not create one.
    #[test]
    fn a_dispatch_with_no_disclosure_binds_nothing() {
        let (_tmp, store, scope) = store();

        let settlement = bind_result_to_disclosure(
            &store,
            &scope,
            "act-never-prepared",
            "agentmail-send",
            "send",
            Some(AGENTMAIL_RESULT),
            "t3",
        );

        assert_eq!(settlement, Settlement::NotApplicable);
        assert!(store
            .load_act(&scope, "act-never-prepared")
            .expect("load")
            .is_none());
        assert_eq!(
            store
                .act_for_provider_message(&scope, "agentmail", "msg_01H8")
                .expect("lookup"),
            None
        );
    }

    /// Replaying the same result is one binding, not two.
    #[test]
    fn replaying_the_same_result_resumes_the_same_binding() {
        let (_tmp, store, scope) = store();
        let act_ref = live_send(&store, &scope, "send-1");

        let first = bind_result_to_disclosure(
            &store,
            &scope,
            &act_ref,
            "agentmail-send",
            "send",
            Some(AGENTMAIL_RESULT),
            "t3",
        );
        let second = bind_result_to_disclosure(
            &store,
            &scope,
            &act_ref,
            "agentmail-send",
            "send",
            Some(AGENTMAIL_RESULT),
            "t4",
        );
        assert_eq!(first, second);
        assert_eq!(
            store
                .load_act_history(&scope, &act_ref)
                .expect("history")
                .len(),
            4,
            "prepared, dispatching, dispatch_unknown, provider_accepted — the replay appended \
             nothing"
        );
    }
}
