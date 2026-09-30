//! What the provider did with an act after we dispatched it.
//!
//! # Why this exists
//!
//! [`evidence::outward_assertions`] records that something was dispatched and,
//! when the adapter cannot say what happened next, parks the act at
//! `dispatch_unknown` — *"neither evidence of a send nor of a non-send"*. Every
//! live outward send still lands there, because the executor's adapter cannot
//! say what happened next; what has changed is that it no longer has to **stay**
//! there forever. This module is the machinery for the missing half — a ledger
//! a receipt can be recorded into and a state a caller can query — and
//! [`intake`] is the door somebody walks a receipt through to move it.
//!
//! The blindness that remains is narrower and worth stating exactly: no
//! provider wired into this deployment emits delivery events, so nothing pushes
//! a receipt through that door unprompted. An act is reconciled when an owner,
//! an operator or a local bridge records what a provider reported.
//!
//! It is therefore the named gate for turning capture mode off. Capture proves
//! an act was *composed* correctly; it proves nothing about whether a provider
//! accepted it, whether it arrived, or whether the recipient reported us. Going
//! live without reconciliation means going live blind, and the blindness is
//! invisible: an unacknowledged send looks exactly like a successful one.
//!
//! # Generic
//!
//! A provider, a message id, an identity, an observed state, a clock, and a
//! payload ref for audit. Nothing here knows what was sent or why. Email,
//! WhatsApp, SMS and push can all reconcile through this one ledger.
//!
//! [`DeliveryLedger::reconcile`] now has exactly one production caller —
//! [`intake::ReceiptIntake::admit`], reached from
//! `POST /api/magician/v2/delivery/receipts` — and that is the whole of the
//! producer side. **No provider in this deployment pushes delivery events**, so
//! the door is currently walked by an owner or a local bridge rather than by a
//! webhook; see [`intake`] for exactly what is and is not wired.
//!
//! The **read** half is wired: `delivery_hygiene::silence` calls
//! [`unreconciled`](DeliveryLedger::unreconciled) from the delivery-hygiene
//! worker's tick and from `GET /api/magician/v2/delivery/unacknowledged`, so
//! the acts parked at `dispatch_unknown` are counted and named rather than
//! merely sitting there. What is still missing is the producer, which is what
//! those counts report. The act ref is **opaque**, so this module reconciles
//! *against* an outward act without owning one, exactly as the obligations
//! register takes its audiences from whoever owns the relationship. Callers
//! *consume; never own*: a second copy of "did it arrive" is a copy that will
//! drift.
//!
//! # The order, and why it is a total one
//!
//! Provider webhooks arrive **out of order**, are redelivered, and are
//! occasionally contradictory. A fold that applied "last line wins with guards"
//! would let arrival order decide the truth: `delivered` then `complained` and
//! `complained` then `delivered` would settle differently, which is a bug that
//! reproduces only under load.
//!
//! So the states are ranked by [`DeliveryState::severity`], the fold is the
//! maximum ([`dominant`]), and a receipt is admitted only when it *strictly
//! exceeds* what is already known ([`may_follow`]). Maximum is commutative and
//! associative, so the ledger lands on one answer no matter what order the
//! provider chose. Every edge — admitted and refused — is pinned in `tests.rs`.
//!
//! # Terminal states never resurrect
//!
//! The ranking is what enforces it. A `delivered` arriving after a `complained`
//! does not un-complain the act, and a `delivered` arriving after a hard bounce
//! does not resurrect a dead address into the sendable pool. Both are recorded
//! for audit — nothing is ever dropped — and neither moves the state.
//!
//! # `dispatch_unknown` is explicitly not success
//!
//! [`state_of`](DeliveryLedger::state_of) returns a [`DeliveryKnowledge`], not
//! an `Option<DeliveryState>`, so the absence of a receipt has to be handled
//! rather than pattern-matched away with a `_ =>`. Unknown reads as
//! `dispatch_unknown` and [`DeliveryKnowledge::reached`] is `false` for it. The
//! whole module exists because somebody could otherwise read "no bad news" as
//! good news.
//!
//! # Fail closed
//!
//! Reads go through [`crate::magician_v2::jsonl`]: only a genuinely absent log
//! reads as an empty ledger, and every other fault propagates. An unreadable
//! ledger answering "nothing bounced" is worse than no ledger, because it
//! manufactures confidence for exactly the send that should have been held.
//!
//! # Counts, never rates
//!
//! [`DeliveryTally`] reports whole numbers and carries how many acts were
//! behind them. There is deliberately no `bounce_rate()`: a rate hides its
//! denominator, and "2% bounces" over four acts and over forty thousand are
//! different facts that must never render alike.
//!
//! [`evidence::outward_assertions`]: crate::magician_v2::evidence::outward_assertions

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// The door a receipt comes through: validate, attribute, then hand it to
/// [`DeliveryLedger::reconcile`] untouched.
pub mod intake;

#[cfg(test)]
mod tests;

/// Field separator for derived ids. A unit separator cannot appear in a provider
/// id, an address or an act ref — every caller string that feeds a derivation is
/// refused if it holds one — so `(a, b)` and `(ab, "")` cannot collide into one
/// receipt.
const FIELD_SEP: char = '\u{1f}';

/// Scope for a ledger call: whose ledger this is.
///
/// Two tenants never share one, and no derivation here can collide across them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryScope {
    pub principal: String,
    pub workspace: String,
}

impl DeliveryScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// What a provider observed about one message to one identity.
///
/// Deliberately **not** `Ord`/`PartialOrd`. Deriving them would order the
/// variants by declaration, which is not the order this module runs on;
/// [`DeliveryState::severity`] is, and a second silent ordering would be used by
/// accident within a week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// The provider took the message. **This is not delivery** — it is the
    /// weakest thing a provider can say, and treating it as success is the
    /// mistake the three-way split in `outward_assertions` already refused once.
    Accepted,
    /// It reached the recipient.
    Delivered,
    /// It came back. `hard` separates a dead address from a transient refusal,
    /// and only the hard one is a suppression signal.
    Bounced { hard: bool },
    /// They reported us. A consent decision expressed through a third party.
    Complained,
    /// The provider could not send it at all — a transport or acceptance
    /// failure, not a statement about the recipient's mailbox.
    Failed,
}

impl DeliveryState {
    /// Every state, weakest first. The order here **is** the severity order, and
    /// the exhaustive edge test walks it.
    pub const ALL: [DeliveryState; 6] = [
        DeliveryState::Accepted,
        DeliveryState::Bounced { hard: false },
        DeliveryState::Failed,
        DeliveryState::Delivered,
        DeliveryState::Bounced { hard: true },
        DeliveryState::Complained,
    ];

    /// The token used in ids, logs and comparisons. A soft and a hard bounce are
    /// different tokens because they are different facts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Delivered => "delivered",
            Self::Bounced { hard: false } => "soft_bounce",
            Self::Bounced { hard: true } => "hard_bounce",
            Self::Complained => "complained",
            Self::Failed => "failed",
        }
    }

    /// Where this observation sits in the order. Higher wins, always.
    ///
    /// The three ranks worth defending:
    ///
    /// - **`Delivered` outranks `Failed`.** The question this ledger answers is
    ///   *"did our words reach them"*, and the fail-closed answer to that is
    ///   "assume they did". A provider that reports both a transport failure and
    ///   a delivery told us the message landed; refusing the delivery would let
    ///   the disclosure register believe nobody saw what somebody did see, and
    ///   correction propagation would skip them.
    /// - **A hard bounce outranks `Delivered`.** A late hard bounce — a
    ///   forwarding chain dying downstream — is a dead address, and the safe
    ///   reading of "delivered, then hard bounced" is *stop sending here*.
    ///   Letting the delivery win would return a dead address to the pool.
    /// - **`Complained` outranks everything.** It is the strongest consent
    ///   signal a provider can hand us, and nothing may ever silence it.
    pub fn severity(self) -> u8 {
        match self {
            Self::Accepted => 0,
            Self::Bounced { hard: false } => 1,
            Self::Failed => 2,
            Self::Delivered => 3,
            Self::Bounced { hard: true } => 4,
            Self::Complained => 5,
        }
    }

    /// Whether this observation proves the message reached the recipient.
    ///
    /// A complaint counts: somebody who reports a message had to receive it
    /// first. A bounce, a failure and a bare acceptance do not.
    pub fn is_arrival(self) -> bool {
        matches!(self, Self::Delivered | Self::Complained)
    }

    /// Whether the act is settled: no *weaker* observation can ever move it
    /// again.
    ///
    /// Terminal does not mean nothing may follow. A complaint may still land on
    /// a delivered act, and a hard bounce may still land after a delivery,
    /// because losing a stronger observation is exactly the failure this module
    /// exists to prevent. What terminal forbids is going *backwards*.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Delivered | Self::Failed | Self::Complained | Self::Bounced { hard: true }
        )
    }

    /// The suppression cause this observation carries, if any.
    ///
    /// A **soft** bounce carries none. A mailbox that was full on Tuesday is not
    /// somebody we may never contact again, and suppressing on transient
    /// failures is how a sender quietly deletes its own audience.
    pub fn suppression_cause(self) -> Option<SuppressionCause> {
        match self {
            Self::Bounced { hard: true } => Some(SuppressionCause::HardBounce),
            Self::Complained => Some(SuppressionCause::Complaint),
            _ => None,
        }
    }
}

/// Whether `next` may be applied on top of `current`.
///
/// The order, in one line: a receipt is admitted only when it **strictly
/// exceeds** what is already known. Everything else — equal, or behind — is
/// refused and recorded rather than applied.
///
/// Refusing equal states is deliberate. An identical state for the same provider
/// message is a replay and is handled as one; an identical state reached by a
/// *different* provider message on the same act is a second recipient's story,
/// folded on its own identity rather than layered onto somebody else's.
pub fn may_follow(current: DeliveryState, next: DeliveryState) -> bool {
    next.severity() > current.severity()
}

/// The stronger of two observations — the fold's operator.
///
/// Maximum, so the fold is commutative and associative and the ledger cannot
/// depend on the order a provider chose to deliver its webhooks in.
pub fn dominant(left: DeliveryState, right: DeliveryState) -> DeliveryState {
    if right.severity() > left.severity() {
        right
    } else {
        left
    }
}

/// Why a delivery observation makes an identity unsafe to contact.
///
/// Plain data on purpose. The suppression register ingests these; this module
/// does not import it, so a consumer decides what a hard bounce is worth without
/// this module deciding for them — and neither side can drift into owning the
/// other's rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionCause {
    HardBounce,
    Complaint,
}

impl SuppressionCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HardBounce => "hard_bounce",
            Self::Complaint => "complaint",
        }
    }
}

/// What is known about an act's delivery.
///
/// A type rather than an `Option<DeliveryState>`, because the absence of a
/// receipt is a *state with a name* that callers must handle: the
/// `dispatch_unknown` an outward act is parked at until something reconciles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "knowledge", content = "state")]
pub enum DeliveryKnowledge {
    /// Nothing has been reconciled. Neither evidence of arrival nor of failure —
    /// a question held open, and **never** success.
    DispatchUnknown,
    Observed(DeliveryState),
}

impl DeliveryKnowledge {
    /// Whether the message is known to have reached the recipient.
    ///
    /// `false` for `DispatchUnknown`, which is the entire point of the type.
    pub fn reached(self) -> bool {
        matches!(self, Self::Observed(state) if state.is_arrival())
    }

    /// Whether anything at all has come back from the provider.
    pub fn is_reconciled(self) -> bool {
        matches!(self, Self::Observed(_))
    }

    pub fn state(self) -> Option<DeliveryState> {
        match self {
            Self::DispatchUnknown => None,
            Self::Observed(state) => Some(state),
        }
    }

    /// The token — `dispatch_unknown` for the unknown arm, so it reads the same
    /// in this ledger's output as it does on the outward act it reconciles.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DispatchUnknown => "dispatch_unknown",
            Self::Observed(state) => state.as_str(),
        }
    }
}

/// What a provider told us, as the caller received it.
///
/// `payload_ref` is required. A receipt nobody can go back and read is an
/// assertion rather than a record, and the first person to doubt a suppression
/// raised from it will remove it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReceipt {
    /// Which provider is speaking — `agentmail`, `kapso`, a push service.
    pub provider: String,
    /// The provider's own id for the message. The idempotency key.
    pub provider_message_id: String,
    /// Who the receipt is about. Required even for an acceptance: a signal
    /// without an identity is a signal the suppression register cannot use.
    pub identity: String,
    pub state: DeliveryState,
    /// When the **provider** observed it. Not our clock — see
    /// [`DeliveryLedger::suppression_signals`] for why the two are kept apart.
    pub observed_at: DateTime<Utc>,
    /// A ref to the exact webhook body, for audit.
    pub payload_ref: String,
}

/// One reconciled receipt, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryObservation {
    pub receipt_id: String,
    pub act_ref: String,
    pub provider: String,
    pub provider_message_id: String,
    pub identity: String,
    pub state: DeliveryState,
    pub observed_at: DateTime<Utc>,
    /// When **we** learned it. Kept beside `observed_at` because a provider can
    /// hand us a backdated event, and a sweep that filtered on the provider's
    /// clock would miss it forever.
    pub recorded_at: DateTime<Utc>,
    pub payload_ref: String,
}

/// What one call to [`DeliveryLedger::reconcile`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciliation {
    /// The first thing this provider has said about this recipient on this act.
    Opened,
    /// The identical receipt again. One record, not two.
    Replayed,
    /// A strictly stronger observation. The recipient's state moved.
    Advanced { from: DeliveryState },
    /// Recorded for audit, and refused by the order: it is behind — or
    /// contradicts — what is already known. The state did **not** move.
    Superseded { held: DeliveryState },
}

/// The outcome of reconciling one receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub observation: DeliveryObservation,
    pub disposition: Reconciliation,
    /// This recipient's state on this act, after the receipt. The partial order
    /// governs **one recipient's message**, so this is the number the order
    /// tests pin.
    pub identity_state: DeliveryState,
    /// The act's state across every recipient: the strongest observation on it.
    /// One recipient's complaint is the act's complaint, because an act that
    /// went wrong for anybody went wrong.
    pub act_state: DeliveryState,
}

/// An act the caller believes left the building, and when.
///
/// Supplied, never discovered. The list of what was dispatched belongs to
/// whoever dispatched it; reading it here would tie this module to one outward
/// store and make a second one unreconcilable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchedAct {
    pub act_ref: String,
    pub dispatched_at: DateTime<Utc>,
}

impl DispatchedAct {
    pub fn new(act_ref: impl Into<String>, dispatched_at: DateTime<Utc>) -> Self {
        Self {
            act_ref: act_ref.into(),
            dispatched_at,
        }
    }
}

/// An act dispatched and never acknowledged.
///
/// `silent_for` is **derived from the clock**, never stored, so nothing has to
/// have run for an act to be overdue. A ledger that depended on a sweep having
/// written a `stale` flag would report silence only where somebody remembered to
/// look, which is the shape of the bug this signal is meant to catch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreconciledAct {
    pub act_ref: String,
    pub dispatched_at: DateTime<Utc>,
    pub silent_for: Duration,
}

/// Who an act reached, and who it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachReport {
    pub act_ref: String,
    /// Distinct identities asked about. Carried so that a report over nobody
    /// cannot render as a report over everybody.
    pub checked: usize,
    /// Identities with an observation proving arrival.
    pub reached: Vec<String>,
    /// Identities whose strongest observation is not an arrival, and what it is.
    pub fell_short: Vec<(String, DeliveryState)>,
    /// Identities with no receipt at all — `dispatch_unknown`, which is neither
    /// a failure nor a success.
    pub unobserved: Vec<String>,
}

impl ReachReport {
    /// Whether the act reached **everybody** asked about.
    ///
    /// `checked > 0` is load-bearing, not defensive noise. A predicate over an
    /// empty collection is vacuously true, and "every one of the zero recipients
    /// we checked received it" is how a broken audience resolution reports a
    /// clean send. An empty report is `false`.
    pub fn everyone_reached(&self) -> bool {
        self.checked > 0 && self.fell_short.is_empty() && self.unobserved.is_empty()
    }
}

/// Whole numbers, and how many acts produced them.
///
/// No rates. See the module note.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryTally {
    /// Distinct acts folded into this tally. Without it the counts are
    /// unreadable: six bounces is a catastrophe over eight acts and a rounding
    /// error over eight thousand.
    pub acts_checked: usize,
    pub accepted: usize,
    pub soft_bounced: usize,
    pub failed: usize,
    pub delivered: usize,
    pub hard_bounced: usize,
    pub complained: usize,
    /// Acts with no receipt at all. A tally where this dominates is a provider
    /// integration that is not wired up, not a quiet week.
    pub dispatch_unknown: usize,
}

/// One line of the suppression-signal index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SignalRow {
    receipt_id: String,
    act_ref: String,
    identity: String,
    cause: SuppressionCause,
    observed_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
}

/// The append-only ledger of provider receipts.
#[derive(Debug, Clone)]
pub struct DeliveryLedger {
    workspace_layout: ArtifactV2Workspace,
}

/// One row of the provider-message binding index: which act a provider message
/// belongs to, and whose story it is.
///
/// The identity is carried alongside the act because the index is written
/// before the observation row, so it is the only thing that survives a crash
/// between the two.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundReceipt {
    act_ref: String,
    identity: String,
}

impl DeliveryLedger {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &DeliveryScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("delivery")
    }

    /// One log per act.
    ///
    /// The file name is the **hash** of the act ref, not the ref itself. Act refs
    /// arrive from outside this module, and a caller-supplied string used raw as
    /// a path component is a directory traversal waiting for its first hostile
    /// webhook.
    fn act_path(&self, scope: &DeliveryScope, act_ref: &str) -> PathBuf {
        self.root(scope)
            .join("acts")
            .join(format!("{}.jsonl", stable_id(act_ref)))
    }

    /// Which act a provider message is bound to. One provider message belongs to
    /// exactly one act, and this index is what makes that checkable.
    fn binding_path(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        provider_message_id: &str,
    ) -> PathBuf {
        self.root(scope)
            .join("index")
            .join("provider")
            .join(format!(
                "{}.jsonl",
                stable_id(&format!("{provider}{FIELD_SEP}{provider_message_id}"))
            ))
    }

    /// Every suppression-worthy observation, in one file.
    ///
    /// A single log rather than a scan across acts: the suppression register's
    /// sweep is the reverse lookup this module exists to serve, and a scan would
    /// be correct and would also be the thing nobody runs.
    fn signals_path(&self, scope: &DeliveryScope) -> PathBuf {
        self.root(scope).join("index").join("signals.jsonl")
    }

    /// Reconcile one provider receipt against an outward act.
    ///
    /// Idempotent by provider message id: the identical receipt twice is one
    /// record. A **different state** for the same provider message id is not a
    /// duplicate — it is the message's next observation, and it is admitted only
    /// if [`may_follow`] admits it. A refused observation is still written, so
    /// the log holds everything the provider ever said even where the state
    /// refused to move; nothing is dropped, ever.
    ///
    /// A receipt that repeats a provider message id **with a changed body** —
    /// a different identity, a different provider timestamp, a different payload
    /// ref — is an **error**, not a silent no-op. Two different stories under one
    /// id means somebody upstream is reusing ids or mis-parsing them, and
    /// quietly keeping the first would hide it until the two stories mattered.
    ///
    /// The **identity** half of that refusal is keyed on the provider message id
    /// alone, independently of the state the receipt claims. `derive_receipt_id`
    /// folds the state into the id, so a check keyed on the id would fire only
    /// while the state was unchanged, and a receipt that changed both its
    /// identity and its state would pass — recording a complaint against an
    /// address that never complained, which nothing downstream can undo.
    ///
    /// # Ordering
    ///
    /// Index before row, both of them. A crash between the two leaves a binding
    /// or a suppression signal pointing at a row that does not exist yet, which
    /// is inert — the reads deduplicate and load by id. The reverse order loses
    /// a suppression signal for a receipt that *was* recorded, and a lost
    /// complaint is a compliance incident rather than an inconsistency.
    pub fn reconcile(
        &self,
        scope: &DeliveryScope,
        act_ref: &str,
        receipt: &DeliveryReceipt,
        now: DateTime<Utc>,
    ) -> Result<ReconcileOutcome> {
        validate_scope(scope)?;
        let act_ref = validated_field(act_ref, "an act ref")?;
        let provider = validated_field(&receipt.provider, "a provider name")?;
        let provider_message_id =
            validated_field(&receipt.provider_message_id, "a provider message id")?;
        let payload_ref = validated_field(&receipt.payload_ref, "a payload ref")?;
        let identity = normalise_identity(&receipt.identity)?;

        let receipt_id = derive_receipt_id(
            scope,
            &act_ref,
            &provider,
            &provider_message_id,
            receipt.state,
        );

        let existing = self.observations(scope, &act_ref)?;
        let held_for_identity = fold_state(existing.iter().filter(|row| row.identity == identity));
        let held_for_act = fold_state(existing.iter());

        // One provider message is one identity's story, and this check is keyed
        // on the provider message id ALONE. `receipt_id` folds the observed
        // state in, so a check keyed on it fires only while the state is
        // unchanged: a second receipt reusing a provider id with a different
        // identity AND a different state derives a different id, misses it
        // entirely, and — where the new state carries a suppression cause —
        // writes an irreversible complaint or hard-bounce signal against an
        // address that never reported us. The same identity reporting a new
        // state is a transition, judged by `may_follow` below.
        if let Some(prior) = existing.iter().find(|row| {
            row.provider == provider
                && row.provider_message_id == provider_message_id
                && row.identity != identity
        }) {
            anyhow::bail!(
                "provider message `{provider}:{provider_message_id}` is already reconciled for \
                 `{}`, and this receipt says `{}` for `{}`: one provider message is one \
                 identity's story, so two different stories under one id are refused whatever \
                 state the second claims — admitting it would record a bounce or a complaint \
                 against an address that never earned one",
                prior.identity,
                receipt.state.as_str(),
                identity,
            );
        }

        if let Some(held) = existing.iter().find(|row| row.receipt_id == receipt_id) {
            if held.observed_at != receipt.observed_at || held.payload_ref != payload_ref {
                anyhow::bail!(
                    "provider message `{provider}:{provider_message_id}` was already reconciled \
                     as `{}` observed at {} from `{}`, and this receipt reports the same state \
                     observed at {} from `{}`: an identical replay resumes the record, but a \
                     changed one is two different stories under one id and keeping the first \
                     would hide whichever is wrong",
                    held.state.as_str(),
                    held.observed_at.to_rfc3339(),
                    held.payload_ref,
                    receipt.observed_at.to_rfc3339(),
                    payload_ref,
                );
            }
            return Ok(ReconcileOutcome {
                observation: held.clone(),
                disposition: Reconciliation::Replayed,
                identity_state: held_for_identity.unwrap_or(held.state),
                act_state: held_for_act.unwrap_or(held.state),
            });
        }

        for bound in self.binding(scope, &provider, &provider_message_id)? {
            if bound.act_ref != act_ref {
                let bound_act = &bound.act_ref;
                anyhow::bail!(
                    "provider message `{provider}:{provider_message_id}` is already reconciled \
                     against act `{bound_act}` and cannot also belong to `{act_ref}`: one provider \
                     message is one act, and a second claim on it would move a bounce or a \
                     complaint onto a disclosure that never carried it"
                );
            }
            // The identity half closes the crash window. The observation row
            // may be missing — the binding is written first — but the binding
            // itself remembers whose story this provider message was, so a
            // second receipt claiming a different address is refused even when
            // there is no row left to compare it against.
            if bound.identity != identity {
                let bound_identity = &bound.identity;
                anyhow::bail!(
                    "provider message `{provider}:{provider_message_id}` already belongs to \
                     `{bound_identity}` and cannot also belong to `{identity}`: one provider \
                     message is one identity's story, and a second claim on it would record a \
                     bounce or a complaint against an address that never sent one"
                );
            }
        }

        let disposition = match held_for_identity {
            None => Reconciliation::Opened,
            Some(held) if may_follow(held, receipt.state) => {
                Reconciliation::Advanced { from: held }
            },
            Some(held) => Reconciliation::Superseded { held },
        };

        let observation = DeliveryObservation {
            receipt_id,
            act_ref: act_ref.clone(),
            provider: provider.clone(),
            provider_message_id: provider_message_id.clone(),
            identity: identity.clone(),
            state: receipt.state,
            observed_at: receipt.observed_at,
            recorded_at: now,
            payload_ref,
        };

        // The binding carries the IDENTITY as well as the act, joined by the
        // house separator. Act alone was not enough: this index is written
        // BEFORE the observation row (deliberately — a pointer with no row is
        // recoverable, a row nothing points at is invisible), so a crash between
        // the two leaves a binding whose act matches and whose identity nothing
        // can check. A second receipt then found no prior row to compare
        // against, passed the act check, and admitted a different address —
        // which for a complaint means suppressing somebody who never complained.
        self.append_line(
            &self.binding_path(scope, &provider, &provider_message_id),
            format!("{act_ref}{FIELD_SEP}{identity}").as_bytes(),
        )?;
        // A suppression signal is emitted whatever the order decided. A hard
        // bounce behind a complaint still says the address is dead, and the
        // register would rather hear it twice than not at all.
        if let Some(cause) = receipt.state.suppression_cause() {
            let row = SignalRow {
                receipt_id: observation.receipt_id.clone(),
                act_ref: act_ref.clone(),
                identity: identity.clone(),
                cause,
                observed_at: receipt.observed_at,
                recorded_at: now,
            };
            self.append_json(&self.signals_path(scope), &row)?;
        }
        self.append_json(&self.act_path(scope, &act_ref), &observation)?;

        Ok(ReconcileOutcome {
            identity_state: held_for_identity
                .map_or(receipt.state, |held| dominant(held, receipt.state)),
            act_state: held_for_act.map_or(receipt.state, |held| dominant(held, receipt.state)),
            observation,
            disposition,
        })
    }

    /// Everything the provider ever said about one act, oldest first.
    ///
    /// Refused observations are included: the log is the audit trail, and an
    /// observation the order declined is exactly the one somebody will need to
    /// see when they ask why the state did not move.
    pub fn observations(
        &self,
        scope: &DeliveryScope,
        act_ref: &str,
    ) -> Result<Vec<DeliveryObservation>> {
        validate_scope(scope)?;
        let act_ref = validated_field(act_ref, "an act ref")?;
        let path = self.act_path(scope, &act_ref);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let rows = crate::magician_v2::jsonl::parse_log_lines::<DeliveryObservation>(&raw, &path)?;
        // Two concurrent reconciles of one receipt can both miss each other and
        // both append. The fold arbitrates: the first wins, and the duplicate is
        // the race losing honestly rather than a doubled bounce.
        let mut seen = BTreeSet::new();
        Ok(rows
            .into_iter()
            .filter(|row| seen.insert(row.receipt_id.clone()))
            .collect())
    }

    /// The act's state: the strongest observation across every recipient.
    ///
    /// An act nothing has reconciled is [`DeliveryKnowledge::DispatchUnknown`],
    /// which is **not** success — see the module note.
    pub fn state_of(&self, scope: &DeliveryScope, act_ref: &str) -> Result<DeliveryKnowledge> {
        Ok(knowledge(fold_state(
            self.observations(scope, act_ref)?.iter(),
        )))
    }

    /// One recipient's state on one act.
    ///
    /// The per-identity fold is the one the order governs. An act to nine
    /// recipients holds nine independent stories, and reading a tenth person's
    /// bounce as this person's is how a working address gets suppressed.
    pub fn state_for(
        &self,
        scope: &DeliveryScope,
        act_ref: &str,
        identity: &str,
    ) -> Result<DeliveryKnowledge> {
        let identity = normalise_identity(identity)?;
        Ok(knowledge(fold_state(
            self.observations(scope, act_ref)?
                .iter()
                .filter(|row| row.identity == identity),
        )))
    }

    /// Acts dispatched and never acknowledged — the signal that a provider
    /// integration is silently broken.
    ///
    /// Silence is measured against the clock and the caller's `dispatched_at`,
    /// so nothing has to have run for an act to show up here. `older_than` is
    /// **inclusive**: an act silent for exactly the grace period is overdue, like
    /// every other expiry in this codebase.
    ///
    /// Acknowledged means *anything came back*, including a bare
    /// [`DeliveryState::Accepted`]. An integration that only ever emits
    /// acceptances is the other shape of broken, and it shows up in
    /// [`tally`](Self::tally) as acceptances with no deliveries rather than here.
    ///
    /// # An empty candidate list is refused
    ///
    /// A sweep over nothing finds nothing, and an empty `Vec` returned from a
    /// health check reads as a clean bill of health. That is vacuous truth
    /// dressed as reassurance, so it is an error instead: a caller with no
    /// candidates should not be running the sweep.
    pub fn unreconciled(
        &self,
        scope: &DeliveryScope,
        dispatched: &[DispatchedAct],
        older_than: Duration,
        now: DateTime<Utc>,
    ) -> Result<Vec<UnreconciledAct>> {
        validate_scope(scope)?;
        if dispatched.is_empty() {
            anyhow::bail!(
                "an unreconciled sweep needs candidates: over an empty list it returns nothing \
                 and reads as a clean bill of health, which is the vacuous pass this check \
                 exists to prevent"
            );
        }
        if older_than < Duration::zero() {
            anyhow::bail!(
                "a negative grace period would mark acts overdue before they were dispatched"
            );
        }

        // A doubled candidate must not produce a doubled finding, and the
        // earliest dispatch wins so a repeat cannot shorten the recorded silence.
        let mut candidates: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        for candidate in dispatched {
            let act_ref = validated_field(&candidate.act_ref, "an act ref")?;
            candidates
                .entry(act_ref)
                .and_modify(|held| {
                    if candidate.dispatched_at < *held {
                        *held = candidate.dispatched_at;
                    }
                })
                .or_insert(candidate.dispatched_at);
        }

        let mut out = Vec::new();
        for (act_ref, dispatched_at) in candidates {
            if self.state_of(scope, &act_ref)?.is_reconciled() {
                continue;
            }
            let silent_for = now - dispatched_at;
            if silent_for >= older_than {
                out.push(UnreconciledAct {
                    act_ref,
                    dispatched_at,
                    silent_for,
                });
            }
        }
        out.sort_by(|left, right| {
            left.dispatched_at
                .cmp(&right.dispatched_at)
                .then_with(|| left.act_ref.cmp(&right.act_ref))
        });
        Ok(out)
    }

    /// Hard bounces and complaints, for the suppression register to ingest.
    ///
    /// Plain `(identity, cause)` pairs, distinct and sorted. This module does not
    /// import the register and does not decide what a bounce is worth; it reports
    /// what the provider said and lets the register apply its own rules — the
    /// same decoupling that lets the register serve mail, messaging and any rail
    /// that has not been built yet.
    ///
    /// # `recorded_since`, not `observed_since`
    ///
    /// The window is on **our** clock. Providers backdate: a webhook that arrives
    /// today carrying yesterday's timestamp would fall behind a sweep that had
    /// already passed yesterday, and the complaint inside it would be lost
    /// forever. Filtering on when we learned it means a late signal is caught by
    /// the next sweep, and the boundary is inclusive so a signal landing exactly
    /// on it is re-offered rather than dropped.
    pub fn suppression_signals(
        &self,
        scope: &DeliveryScope,
        recorded_since: DateTime<Utc>,
    ) -> Result<Vec<(String, SuppressionCause)>> {
        validate_scope(scope)?;
        let path = self.signals_path(scope);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let rows = crate::magician_v2::jsonl::parse_log_lines::<SignalRow>(&raw, &path)?;
        let mut seen_receipts = BTreeSet::new();
        let mut out = BTreeSet::new();
        for row in rows {
            if !seen_receipts.insert(row.receipt_id.clone()) {
                continue;
            }
            if row.recorded_at >= recorded_since {
                out.insert((row.identity, row.cause));
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Who an act reached, and who it did not.
    ///
    /// The audience is supplied by the caller from whatever resolved it for the
    /// send, never inferred from the receipts: receipts only exist for people the
    /// provider said something about, so inferring the audience from them would
    /// make everybody the provider stayed silent about disappear — and silence is
    /// the case this report exists to surface.
    ///
    /// An empty audience is refused. See
    /// [`unreconciled`](Self::unreconciled) for the same reasoning, and
    /// [`ReachReport::everyone_reached`] for the guard that holds even if a
    /// report is built by hand.
    pub fn reach(
        &self,
        scope: &DeliveryScope,
        act_ref: &str,
        identities: &[String],
    ) -> Result<ReachReport> {
        validate_scope(scope)?;
        let act_ref = validated_field(act_ref, "an act ref")?;
        if identities.is_empty() {
            anyhow::bail!(
                "a reach report needs an audience: over nobody every recipient trivially \
                 received it, and that vacuous pass is indistinguishable from a send that \
                 worked"
            );
        }

        let observations = self.observations(scope, &act_ref)?;
        let mut wanted = BTreeSet::new();
        for identity in identities {
            wanted.insert(normalise_identity(identity)?);
        }

        let mut reached = Vec::new();
        let mut fell_short = Vec::new();
        let mut unobserved = Vec::new();
        for identity in &wanted {
            let state = fold_state(observations.iter().filter(|row| &row.identity == identity));
            match state {
                None => unobserved.push(identity.clone()),
                Some(state) if state.is_arrival() => reached.push(identity.clone()),
                Some(state) => fell_short.push((identity.clone(), state)),
            }
        }
        Ok(ReachReport {
            act_ref,
            checked: wanted.len(),
            reached,
            fell_short,
            unobserved,
        })
    }

    /// Counts across a supplied set of acts. Never rates — see the module note.
    ///
    /// An empty act list is refused for the same reason a sweep over nothing is:
    /// a tally of all zeroes is not evidence that nothing went wrong.
    pub fn tally(&self, scope: &DeliveryScope, act_refs: &[String]) -> Result<DeliveryTally> {
        validate_scope(scope)?;
        if act_refs.is_empty() {
            anyhow::bail!(
                "a tally needs acts: all-zero counts over an empty list read exactly like a \
                 clean week"
            );
        }
        let mut distinct = BTreeSet::new();
        for act_ref in act_refs {
            distinct.insert(validated_field(act_ref, "an act ref")?);
        }

        let mut tally = DeliveryTally {
            acts_checked: distinct.len(),
            ..DeliveryTally::default()
        };
        for act_ref in &distinct {
            match self.state_of(scope, act_ref)? {
                DeliveryKnowledge::DispatchUnknown => tally.dispatch_unknown += 1,
                DeliveryKnowledge::Observed(DeliveryState::Accepted) => tally.accepted += 1,
                DeliveryKnowledge::Observed(DeliveryState::Bounced { hard: false }) => {
                    tally.soft_bounced += 1
                },
                DeliveryKnowledge::Observed(DeliveryState::Failed) => tally.failed += 1,
                DeliveryKnowledge::Observed(DeliveryState::Delivered) => tally.delivered += 1,
                DeliveryKnowledge::Observed(DeliveryState::Bounced { hard: true }) => {
                    tally.hard_bounced += 1
                },
                DeliveryKnowledge::Observed(DeliveryState::Complained) => tally.complained += 1,
            }
        }
        Ok(tally)
    }

    // ── Internals ───────────────────────────────────────────────────────────

    fn binding(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        provider_message_id: &str,
    ) -> Result<Vec<BoundReceipt>> {
        let path = self.binding_path(scope, provider, provider_message_id);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };
        let mut seen = BTreeSet::new();
        let mut bound = Vec::new();
        for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
            // Refused, not tolerated. `append_binding` is this file's only
            // writer and always joins with the separator, and neither half can
            // carry one — the act ref is refused it at record time and the
            // identity is normalised. So a line without it was not written by
            // this store, and reading it as an act with an unknown identity
            // would reinstate exactly the ambiguity this format removes.
            let Some((act_ref, identity)) = line.split_once(FIELD_SEP) else {
                anyhow::bail!(
                    "binding line in {} carries no {} separator, so it names an act without an \
                     identity: this store never writes that shape, and treating the identity as \
                     unknown is how a provider message ends up suppressing an address that never \
                     complained",
                    path.display(),
                    "U+001F"
                );
            };
            if seen.insert(line.to_string()) {
                bound.push(BoundReceipt {
                    act_ref: act_ref.to_string(),
                    identity: identity.to_string(),
                });
            }
        }
        Ok(bound)
    }

    fn append_json<T: Serialize>(&self, path: &PathBuf, value: &T) -> Result<()> {
        let line = serde_json::to_vec(value)?;
        self.append_line(path, &line)
    }

    fn append_line(&self, path: &PathBuf, line: &[u8]) -> Result<()> {
        // Torn-tail healing and the append protocol both live in
        // `magician_v2::jsonl`; nothing here writes to the workspace directly.
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, line)
            .with_context(|| format!("appending {}", path.display()))
    }

    fn read_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        // NotFound is the only error that reads as an empty ledger. An
        // unreadable log folded to "nothing bounced" would clear a send that
        // should have been held.
        crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

/// The fold: the strongest observation, or `None` if there is none.
fn fold_state<'row>(
    rows: impl Iterator<Item = &'row DeliveryObservation>,
) -> Option<DeliveryState> {
    rows.map(|row| row.state).reduce(dominant)
}

fn knowledge(state: Option<DeliveryState>) -> DeliveryKnowledge {
    match state {
        None => DeliveryKnowledge::DispatchUnknown,
        Some(state) => DeliveryKnowledge::Observed(state),
    }
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The id for one observation.
///
/// Derived from `(owner, act, provider, provider message id, state)`. The
/// **state** is in the tuple, and that is the load-bearing choice: without it a
/// bounce arriving after an acceptance would resume the acceptance's row and the
/// transition would be swallowed as a duplicate. With it, the same receipt twice
/// is one record and a *different* state is a new record the order then judges.
///
/// The **identity is not in the tuple**, and because the state is, an id-keyed
/// check only fires while the state holds still. So the "one provider message is
/// one identity's story" guard in [`DeliveryLedger::reconcile`] is keyed on the
/// provider message id alone rather than on this id.
fn derive_receipt_id(
    scope: &DeliveryScope,
    act_ref: &str,
    provider: &str,
    provider_message_id: &str,
    state: DeliveryState,
) -> String {
    format!(
        "rcpt-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{act_ref}{FIELD_SEP}{provider}{FIELD_SEP}\
             {provider_message_id}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            state.as_str()
        ))
    )
}

fn validate_scope(scope: &DeliveryScope) -> Result<()> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator \
             that keeps a receipt id's components apart, and a crafted scope could otherwise \
             resume another owner's record"
        );
    }
    Ok(())
}

/// A caller string that feeds an id derivation, checked and trimmed.
///
/// Blank is refused because an unidentifiable receipt cannot be reconciled
/// against anything, and U+001F is refused because it is the separator: a
/// crafted provider id could otherwise shift a component boundary and resume —
/// or shadow — another act's record. Other control characters are refused too:
/// one arriving here means the value was mis-parsed upstream, and a newline in
/// particular would tear the append-only log into two lines.
fn validated_field(value: &str, what: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{what} is required: a receipt that names nothing reconciles nothing");
    }
    if trimmed.contains(FIELD_SEP) {
        anyhow::bail!(
            "{what} must not contain U+001F: it is the separator that keeps a receipt id's \
             components from bleeding into each other"
        );
    }
    if trimmed.chars().any(char::is_control) {
        anyhow::bail!(
            "{what} must not contain control characters: one arriving here means the value was \
             mis-parsed upstream, and a newline would tear the log line in two"
        );
    }
    Ok(trimmed.to_string())
}

/// The canonical form of an identity, and the only form this ledger stores.
///
/// Trim, unwrap one surrounding pair of angle brackets, trim again, lowercase.
/// The same over-matching rule the suppression register applies, kept local so
/// this module takes no dependency on it: providers hand back their own
/// capitalisation and their own angle-wrapping of the same mailbox, and a ledger
/// that matched on the string rather than the person would report a bounce for
/// `A@B` and a silence for `a@b`.
pub fn normalise_identity(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let unwrapped = trimmed
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(trimmed)
        .trim();
    Ok(validated_field(unwrapped, "an identity")?.to_lowercase())
}
