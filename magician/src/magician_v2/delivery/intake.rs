//! The door a receipt comes through, and the record of who brought it.
//!
//! # Why this exists
//!
//! [`DeliveryLedger::reconcile`] is the entire reconciliation contract — the
//! severity order, the terminal states, the one-message-one-identity rule — and
//! until this module it had **no production caller anywhere in the workspace**,
//! only test fixtures. Meanwhile every live outward send is recorded as a
//! disclosure and immediately parked at `dispatch_unknown` carrying the literal
//! reason *"no adapter receipt support: this send cannot be reconciled"*. The
//! ledger was complete. What was missing was the door.
//!
//! # An intake, deliberately not an integration
//!
//! **No provider wired into this deployment emits a delivery event stream
//! today.** The AgentMail bot receives `message.received` and logs every other
//! event type as *ignored*; the Kapso bot verifies provider HMACs for inbound
//! message webhooks but exposes no bounce, complaint, or delivery-status
//! surface; and no skill in `skillshub` exposes one either. Writing an
//! AgentMail-shaped ingest would be writing against a
//! stream that does not exist, and it would have to be written again for the
//! second rail.
//!
//! So this is the other move: **one normalised door**. A provider, a provider
//! message id, an identity, an observed state, an instant and a payload ref.
//! Kapso, an SMTP adapter, a push service and a human operator reading a bounce
//! out of a support ticket all land through the same call, and the moment a
//! real event stream exists it plugs in here rather than requiring this work
//! again.
//!
//! # Every invariant is the ledger's, unchanged
//!
//! Nothing here re-implements the order. [`ReceiptIntake::admit`] validates,
//! records the attempt, and hands the receipt to
//! [`DeliveryLedger::reconcile`] untouched — so terminal states still never
//! resurrect, a second identity under one provider message id is still refused,
//! an identical replay still resumes and a changed payload under one id is
//! still an error. A door that softened one of those would be a way around the
//! ledger rather than a way into it.
//!
//! # The act must have left
//!
//! A receipt names an act. [`ReceiptIntake::admit`] refuses one whose act this
//! scope never recorded as dispatched, and that refusal is the load-bearing
//! one: `Complained` and a hard bounce both carry a
//! [`SuppressionCause`](super::SuppressionCause), which the delivery-hygiene
//! sweep turns into a register entry that only an explicit owner act with
//! evidence can ever lift. Without the check, one call naming an arbitrary
//! string as an act ref could suppress any address forever. With it, a receipt
//! can only ever speak about something this runtime actually sent.
//!
//! The candidate list is **supplied, never discovered**, exactly as
//! [`DeliveryLedger::unreconciled`] takes it. The list of what was dispatched
//! belongs to whoever dispatched it, and reading it here would tie this module
//! to one outward store and make a second rail unreconcilable.
//!
//! # The attempt log, and why it is written first
//!
//! The ledger records *what a provider said*. It has no field for *who pushed
//! it through the door*, and it should not grow one: a receipt is a receipt
//! whoever carried it. But an owner recording `complained` by hand and a
//! provider webhook reporting `complained` are the same row in the ledger and
//! very different facts in a review, so the attribution is kept beside it, in
//! its own append-only log, under the same `delivery/` root.
//!
//! It is written **before** the reconcile, and that ordering is deliberate:
//!
//! - A crash between the two leaves an attempt with no observation, which is
//!   inert and visible. The reverse loses the attribution for a receipt that
//!   *was* recorded — and an unattributable suppression is the one nobody can
//!   audit afterwards.
//! - A **refused** reconcile still leaves its attempt row. That is the same
//!   choice the ledger makes when it writes observations the order declined:
//!   the attempt somebody made and the store rejected is exactly the row a
//!   security review needs, and dropping it would make the door's failures
//!   invisible.
//!
//! The log is an attempt log, never an outcome log. The outcome lives in the
//! ledger, in one place, because a second copy of "what happened to this send"
//! is a copy that will drift.
//!
//! # No coupling upwards
//!
//! [`IntakeAttribution::authentication`] is a plain string. This module does not
//! import the HTTP boundary's identity types and has no opinion about how a
//! caller was proved — that decision belongs to the surface, and a generic
//! ledger that imported an authentication enum could never serve a rail that
//! authenticated differently.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    normalise_identity, stable_id, validate_scope, validated_field, DeliveryLedger,
    DeliveryReceipt, DeliveryScope, DeliveryState, DispatchedAct, ReconcileOutcome, FIELD_SEP,
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// Who put a receipt through the door.
///
/// Not a trust level and not a permission. Both sources reach the same
/// [`DeliveryLedger::reconcile`] under the same rules; this only records which
/// route the fact travelled, so a review can tell a provider's own report from
/// a person's transcription of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptSource {
    /// A provider, adapter or bridge speaking for the provider's own event.
    /// `payload_ref` points at the body as it arrived.
    Provider,
    /// A person recording what a provider reported out of band — a bounce read
    /// out of a support ticket, a complaint relayed by a postmaster, a status
    /// a console showed and no webhook ever carried. `payload_ref` points at
    /// wherever that report lives.
    Operator,
}

impl ReceiptSource {
    /// Every source this build knows, so a surface can name them without a
    /// hand-written list that goes stale.
    pub const ALL: [ReceiptSource; 2] = [ReceiptSource::Provider, ReceiptSource::Operator];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Operator => "operator",
        }
    }

    /// The source a caller named, or `None`.
    ///
    /// `None` is *"that is not a source"*, never a default. Guessing `provider`
    /// for an unreadable label would file a person's hand-typed complaint as a
    /// provider's own report, which is the one distinction this field exists to
    /// keep.
    pub fn parse(label: &str) -> Option<Self> {
        let wanted = label.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|source| source.as_str() == wanted.as_str())
    }
}

/// Who brought a receipt through the door, as the boundary proved them.
///
/// `actor` and `authentication` are the surface's to establish and this
/// module's to record. Neither is ever read off a request body — a receipt that
/// names its own author is a receipt that names anybody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeAttribution {
    pub source: ReceiptSource,
    /// The principal the outer boundary proved. Feeds the attempt id, so it is
    /// refused if it carries U+001F.
    pub actor: String,
    /// How that principal was proved, as the surface names it. A free string on
    /// purpose: see the module note on coupling.
    pub authentication: String,
}

/// One receipt as it was presented, and by whom.
///
/// Deliberately holds no disposition. What the ledger did with the receipt is
/// the ledger's to say, and a second copy here would be a copy that drifts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptAttempt {
    pub attempt_id: String,
    pub act_ref: String,
    pub provider: String,
    pub provider_message_id: String,
    pub identity: String,
    pub state: DeliveryState,
    /// When the **provider** observed it, as presented.
    pub observed_at: DateTime<Utc>,
    pub payload_ref: String,
    pub source: ReceiptSource,
    pub actor: String,
    pub authentication: String,
    /// When the receipt came through **this** door. Kept beside `observed_at`
    /// because a provider — or a person — can present a backdated event, and an
    /// audit that filtered on the presented clock would miss it.
    pub presented_at: DateTime<Utc>,
}

/// What one admitted receipt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedReceipt {
    pub attempt: ReceiptAttempt,
    /// The ledger's own answer, untouched.
    pub outcome: ReconcileOutcome,
}

/// The intake: validate, attribute, reconcile.
#[derive(Debug, Clone)]
pub struct ReceiptIntake {
    ledger: DeliveryLedger,
    workspace_layout: ArtifactV2Workspace,
}

impl ReceiptIntake {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            ledger: DeliveryLedger::new(workspace_layout.clone()),
            workspace_layout,
        }
    }

    /// The ledger this door writes into, for callers that also need to read it.
    ///
    /// Handed out rather than rebuilt, so a caller cannot end up reconciling
    /// into one workspace layout and reading from another.
    pub fn ledger(&self) -> &DeliveryLedger {
        &self.ledger
    }

    /// One attempt log per act, named by the **hash** of the act ref.
    ///
    /// The ref itself is never a path component: it arrives from outside, and a
    /// caller-supplied string used raw as a filename is a traversal waiting for
    /// its first hostile payload. The same reasoning, and the same `delivery/`
    /// root, as [`DeliveryLedger`]'s own logs — an attribution kept somewhere
    /// else is an attribution that drifts from the receipt it belongs to.
    fn attempt_path(&self, scope: &DeliveryScope, act_ref: &str) -> std::path::PathBuf {
        self.ledger
            .root(scope)
            .join("intake")
            .join(format!("{}.jsonl", stable_id(act_ref)))
    }

    /// Put one receipt through the door.
    ///
    /// `dispatched` is the list of acts this scope recorded as having left,
    /// supplied by whoever dispatched them. The receipt's act must be on it —
    /// see the module note on why that check is the one that matters.
    ///
    /// Everything the ledger refuses, this refuses, because the ledger is what
    /// decides. Everything the ledger admits is admitted here unchanged.
    ///
    /// # Ordering
    ///
    /// Attempt row, then reconcile. Both the crash window and the refused-attempt
    /// case are argued in the module note.
    pub fn admit(
        &self,
        scope: &DeliveryScope,
        dispatched: &[DispatchedAct],
        act_ref: &str,
        receipt: &DeliveryReceipt,
        attribution: &IntakeAttribution,
        now: DateTime<Utc>,
    ) -> Result<AdmittedReceipt> {
        validate_scope(scope)?;
        let act_ref = validated_field(act_ref, "an act ref")?;
        let provider = validated_field(&receipt.provider, "a provider name")?;
        let provider_message_id =
            validated_field(&receipt.provider_message_id, "a provider message id")?;
        let payload_ref = validated_field(&receipt.payload_ref, "a payload ref")?;
        let identity = normalise_identity(&receipt.identity)?;
        let actor = validated_field(&attribution.actor, "the recording actor")?;
        let authentication =
            validated_field(&attribution.authentication, "the authentication class")?;

        // The act must have left this scope. Argued at length in the module
        // note: without this, one call naming any string as an act ref could
        // record a complaint and suppress that address for good.
        if dispatched.is_empty() {
            anyhow::bail!(
                "this scope has no acts recorded as dispatched at all, so there is nothing a \
                 receipt could be about. A receipt arriving here means either the wrong scope or \
                 a dispatch log that was never written — and admitting it would let a complaint \
                 suppress an address this runtime never wrote to"
            );
        }
        if !dispatched
            .iter()
            .any(|candidate| candidate.act_ref == act_ref)
        {
            anyhow::bail!(
                "act `{act_ref}` is not among the {} acts this scope recorded as dispatched, so \
                 this runtime never sent it and no provider can have a receipt for it. A hard \
                 bounce or a complaint admitted here would suppress an identity on the strength \
                 of an act that does not exist, and nothing but an explicit owner act could lift \
                 it again",
                dispatched.len()
            );
        }

        let attempt = ReceiptAttempt {
            attempt_id: derive_attempt_id(
                scope,
                &act_ref,
                &provider,
                &provider_message_id,
                receipt.state,
                attribution.source,
                &actor,
            ),
            act_ref: act_ref.clone(),
            provider: provider.clone(),
            provider_message_id: provider_message_id.clone(),
            identity: identity.clone(),
            state: receipt.state,
            observed_at: receipt.observed_at,
            payload_ref: payload_ref.clone(),
            source: attribution.source,
            actor,
            authentication,
            presented_at: now,
        };

        // Before the reconcile, and before any refusal below it: an attempt
        // nobody can see is an attempt nobody can review.
        let line = serde_json::to_vec(&attempt)?;
        crate::magician_v2::jsonl::append_log_line(
            &self.workspace_layout,
            &self.attempt_path(scope, &act_ref),
            &line,
        )?;

        // The ledger decides. The receipt is rebuilt from the validated fields
        // rather than forwarded as presented, so the identity the attempt log
        // records and the identity the ledger folds on are the same string.
        let outcome = self.ledger.reconcile(
            scope,
            &act_ref,
            &DeliveryReceipt {
                provider,
                provider_message_id,
                identity,
                state: receipt.state,
                observed_at: receipt.observed_at,
                payload_ref,
            },
            now,
        )?;

        Ok(AdmittedReceipt { attempt, outcome })
    }

    /// Every receipt presented for one act, oldest first.
    ///
    /// Refused attempts are included: an attempt the ledger declined is exactly
    /// the row somebody will need when they ask who tried to mark an address
    /// complained. An absent log reads as no attempts; every other fault
    /// propagates, because an unreadable audit trail answering "nobody tried" is
    /// worse than no audit trail at all.
    pub fn attempts(&self, scope: &DeliveryScope, act_ref: &str) -> Result<Vec<ReceiptAttempt>> {
        validate_scope(scope)?;
        let act_ref = validated_field(act_ref, "an act ref")?;
        let path = self.attempt_path(scope, &act_ref);
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(Vec::new());
        };
        let rows = crate::magician_v2::jsonl::parse_log_lines::<ReceiptAttempt>(&raw, &path)?;
        // An identical replay through the same door is one attempt, not two —
        // the same arbitration the ledger applies to a doubled observation.
        let mut seen = std::collections::BTreeSet::new();
        Ok(rows
            .into_iter()
            .filter(|row| seen.insert(row.attempt_id.clone()))
            .collect())
    }
}

/// The id for one attempt.
///
/// Derived from `(owner, act, provider, provider message id, state, source,
/// actor)`. The **actor** and the **source** are in the tuple on purpose: the
/// same receipt presented by two different people is two facts a review has to
/// be able to see, and folding them onto one id would hide the second person
/// entirely. Every component is refused if it carries the separator.
fn derive_attempt_id(
    scope: &DeliveryScope,
    act_ref: &str,
    provider: &str,
    provider_message_id: &str,
    state: DeliveryState,
    source: ReceiptSource,
    actor: &str,
) -> String {
    format!(
        "attempt-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{act_ref}{FIELD_SEP}{provider}{FIELD_SEP}\
             {provider_message_id}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{actor}",
            scope.principal,
            scope.workspace,
            state.as_str(),
            source.as_str()
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const HARD: DeliveryState = DeliveryState::Bounced { hard: true };

    fn t(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 21, hour, 0, 0).unwrap()
    }

    fn intake() -> (tempfile::TempDir, ReceiptIntake, DeliveryScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let intake = ReceiptIntake::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, intake, DeliveryScope::new("alpha", "prod"))
    }

    fn sent() -> Vec<DispatchedAct> {
        vec![DispatchedAct::new("act-1", t(9))]
    }

    fn receipt(state: DeliveryState, message_id: &str, identity: &str) -> DeliveryReceipt {
        DeliveryReceipt {
            provider: "agentmail".to_string(),
            provider_message_id: message_id.to_string(),
            identity: identity.to_string(),
            state,
            observed_at: t(10),
            payload_ref: format!("payload://{message_id}"),
        }
    }

    fn by(source: ReceiptSource, actor: &str) -> IntakeAttribution {
        IntakeAttribution {
            source,
            actor: actor.to_string(),
            authentication: "cloudflare_access".to_string(),
        }
    }

    /// The door reaches the ledger, and the ledger's state moves.
    ///
    /// Pins the failure this module was written for: `DeliveryLedger::reconcile`
    /// had no production caller, so every live send stayed at `dispatch_unknown`
    /// forever and an unacknowledged send was indistinguishable from a
    /// successful one. The ledger is asserted to hold the exact state and the
    /// exact suppression signal afterwards, so nothing here can pass against a
    /// door that validated its input and wrote nothing.
    #[test]
    fn an_admitted_receipt_moves_the_ledger_and_raises_its_suppression_signal() {
        let (_tmp, intake, scope) = intake();

        // Before: the act is parked at `dispatch_unknown`, which is not success.
        let before = intake.ledger().state_of(&scope, "act-1").expect("a read");
        assert_eq!(before.as_str(), "dispatch_unknown");
        assert!(!before.is_reconciled());
        assert!(!before.reached());

        let admitted = intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &receipt(HARD, "pm-1", "Dead@Example.Test"),
                &by(ReceiptSource::Provider, "owner"),
                t(11),
            )
            .expect("the receipt is admitted");

        assert_eq!(
            admitted.outcome.disposition,
            super::super::Reconciliation::Opened
        );
        assert_eq!(admitted.outcome.identity_state, HARD);
        assert_eq!(admitted.outcome.act_state, HARD);
        assert_eq!(admitted.outcome.observation.identity, "dead@example.test");
        assert_eq!(admitted.attempt.source, ReceiptSource::Provider);
        assert_eq!(admitted.attempt.actor, "owner");

        let after = intake.ledger().state_of(&scope, "act-1").expect("a read");
        assert_eq!(after.as_str(), "hard_bounce");
        assert!(after.is_reconciled());
        assert!(!after.reached(), "a hard bounce is not an arrival");

        // The suppression signal the delivery-hygiene sweep reads is really on
        // disk, with the normalised identity and the exact cause.
        let signals = intake
            .ledger()
            .suppression_signals(&scope, t(0))
            .expect("signals read");
        assert_eq!(
            signals,
            vec![(
                "dead@example.test".to_string(),
                super::super::SuppressionCause::HardBounce
            )]
        );
    }

    /// A receipt for an act this scope never dispatched is refused, and nothing
    /// is written to the ledger.
    ///
    /// **The remote suppression primitive, pinned.** `Complained` carries a
    /// suppression cause that only an explicit owner act with evidence can
    /// lift, so a door that admitted a receipt naming an arbitrary act ref
    /// would let one call silence any address permanently. The assertion that
    /// matters is the second one: the ledger is still `dispatch_unknown` and
    /// the signal list is still empty afterwards.
    #[test]
    fn a_receipt_for_an_act_that_never_left_is_refused_and_suppresses_nobody() {
        let (_tmp, intake, scope) = intake();

        let refused = intake
            .admit(
                &scope,
                &sent(),
                "act-nobody-sent",
                &receipt(DeliveryState::Complained, "pm-9", "victim@example.test"),
                &by(ReceiptSource::Provider, "owner"),
                t(11),
            )
            .expect_err("an undispatched act is refused");
        let sentence = format!("{refused:#}");
        assert!(sentence.contains("not among the 1 acts"), "{sentence}");

        assert_eq!(
            intake
                .ledger()
                .state_of(&scope, "act-nobody-sent")
                .expect("a read")
                .as_str(),
            "dispatch_unknown"
        );
        assert_eq!(
            intake
                .ledger()
                .suppression_signals(&scope, t(0))
                .expect("signals read"),
            Vec::new(),
            "a refused receipt must never raise a suppression signal"
        );
    }

    /// A scope that has dispatched nothing refuses every receipt, and says why.
    ///
    /// Separate from the case above because the two are different faults: an
    /// unknown act in a scope that sends is a bad ref, and an unknown act in a
    /// scope that has never sent is the wrong scope or a dispatch log that was
    /// never written. Folding them into one message would send an operator
    /// looking in the wrong place.
    #[test]
    fn a_scope_that_dispatched_nothing_refuses_every_receipt() {
        let (_tmp, intake, scope) = intake();

        let refused = intake
            .admit(
                &scope,
                &[],
                "act-1",
                &receipt(DeliveryState::Delivered, "pm-1", "them@example.test"),
                &by(ReceiptSource::Provider, "owner"),
                t(11),
            )
            .expect_err("an empty candidate list is refused");
        assert!(
            format!("{refused:#}").contains("no acts recorded as dispatched at all"),
            "{refused:#}"
        );
    }

    /// The ledger's order is not softened by the door.
    ///
    /// A `delivered` arriving after a hard bounce is recorded for audit and does
    /// **not** move the state — a resurrected dead address is a live send to a
    /// mailbox that already bounced. The disposition says `Superseded` and the
    /// state after is still the hard bounce.
    #[test]
    fn a_weaker_receipt_after_a_terminal_one_is_recorded_and_refused_by_the_order() {
        let (_tmp, intake, scope) = intake();
        intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &receipt(HARD, "pm-1", "dead@example.test"),
                &by(ReceiptSource::Provider, "owner"),
                t(11),
            )
            .expect("the hard bounce lands");

        let later = intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &receipt(DeliveryState::Delivered, "pm-2", "dead@example.test"),
                &by(ReceiptSource::Provider, "owner"),
                t(12),
            )
            .expect("the later receipt is recorded");

        assert_eq!(
            later.outcome.disposition,
            super::super::Reconciliation::Superseded { held: HARD }
        );
        assert_eq!(later.outcome.identity_state, HARD);
        assert_eq!(
            intake
                .ledger()
                .state_of(&scope, "act-1")
                .expect("a read")
                .as_str(),
            "hard_bounce",
            "a terminal state must never resurrect through this door"
        );
        // Nothing was dropped: both observations are on the act's log.
        assert_eq!(
            intake
                .ledger()
                .observations(&scope, "act-1")
                .expect("a read")
                .len(),
            2
        );
    }

    /// Two people presenting the same receipt are two attempts, one observation.
    ///
    /// The attempt id folds the actor in, so an operator re-recording what a
    /// provider already reported is visible as a second person's act; the ledger
    /// still holds exactly one observation, because an identical replay resumes.
    #[test]
    fn the_same_receipt_from_two_actors_is_two_attempts_and_one_observation() {
        let (_tmp, intake, scope) = intake();
        let presented = receipt(DeliveryState::Complained, "pm-7", "cross@example.test");

        let first = intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &presented,
                &by(ReceiptSource::Provider, "bridge"),
                t(11),
            )
            .expect("the provider's report lands");
        let second = intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &presented,
                &by(ReceiptSource::Operator, "owner"),
                t(12),
            )
            .expect("the operator's transcription replays");

        assert_eq!(
            first.outcome.disposition,
            super::super::Reconciliation::Opened
        );
        assert_eq!(
            second.outcome.disposition,
            super::super::Reconciliation::Replayed
        );
        assert_ne!(first.attempt.attempt_id, second.attempt.attempt_id);

        let attempts = intake.attempts(&scope, "act-1").expect("the attempt log");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].actor, "bridge");
        assert_eq!(attempts[0].source, ReceiptSource::Provider);
        assert_eq!(attempts[1].actor, "owner");
        assert_eq!(attempts[1].source, ReceiptSource::Operator);
        assert_eq!(attempts[1].authentication, "cloudflare_access");

        assert_eq!(
            intake
                .ledger()
                .observations(&scope, "act-1")
                .expect("a read")
                .len(),
            1,
            "an identical replay is one record, whoever carried it"
        );
    }

    /// A refused receipt still leaves its attempt row.
    ///
    /// The door's failures have to be reviewable. A second identity presented
    /// under a provider message id already bound to somebody else is the exact
    /// shape of an attempt to suppress the wrong person, and it is refused —
    /// but the attempt is on the log, naming who made it.
    #[test]
    fn a_refused_attempt_is_still_recorded_against_the_actor_who_made_it() {
        let (_tmp, intake, scope) = intake();
        intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &receipt(DeliveryState::Delivered, "pm-1", "real@example.test"),
                &by(ReceiptSource::Provider, "bridge"),
                t(11),
            )
            .expect("the first receipt lands");

        let refused = intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &receipt(DeliveryState::Complained, "pm-1", "victim@example.test"),
                &by(ReceiptSource::Operator, "someone-else"),
                t(12),
            )
            .expect_err("one provider message is one identity's story");
        assert!(
            format!("{refused:#}").contains("one identity's story"),
            "{refused:#}"
        );

        let attempts = intake.attempts(&scope, "act-1").expect("the attempt log");
        assert_eq!(attempts.len(), 2, "the refused attempt is on the log");
        assert_eq!(attempts[1].actor, "someone-else");
        assert_eq!(attempts[1].identity, "victim@example.test");

        // And the victim was never suppressed.
        assert_eq!(
            intake
                .ledger()
                .suppression_signals(&scope, t(0))
                .expect("signals read"),
            Vec::new()
        );
    }

    /// A caller string carrying the id separator is refused before anything is
    /// written.
    ///
    /// U+001F is what keeps a derived id's components apart. A crafted actor or
    /// provider id could otherwise shift a boundary and resume another attempt's
    /// record, so every one of them is checked at this door.
    #[test]
    fn a_component_carrying_the_unit_separator_is_refused_everywhere() {
        let (_tmp, intake, scope) = intake();

        let mut hostile_provider = receipt(HARD, "pm-1", "dead@example.test");
        hostile_provider.provider = "agent\u{1f}mail".to_string();
        let refused = intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &hostile_provider,
                &by(ReceiptSource::Provider, "owner"),
                t(11),
            )
            .expect_err("a provider name holding the separator is refused");
        assert!(format!("{refused:#}").contains("U+001F"), "{refused:#}");

        let refused = intake
            .admit(
                &scope,
                &sent(),
                "act-1",
                &receipt(HARD, "pm-1", "dead@example.test"),
                &by(ReceiptSource::Provider, "own\u{1f}er"),
                t(11),
            )
            .expect_err("an actor holding the separator is refused");
        assert!(format!("{refused:#}").contains("U+001F"), "{refused:#}");

        // Nothing was written by either attempt.
        assert_eq!(intake.attempts(&scope, "act-1").expect("a read").len(), 0);
        assert_eq!(
            intake
                .ledger()
                .state_of(&scope, "act-1")
                .expect("a read")
                .as_str(),
            "dispatch_unknown"
        );
    }

    /// An unnamed source is refused rather than defaulted.
    ///
    /// `provider` is the convenient default and the wrong one: it would file a
    /// person's hand-typed complaint as a provider's own report, and a review
    /// asking "did a provider really say this" would get the wrong answer.
    #[test]
    fn an_unknown_source_label_is_refused_never_defaulted() {
        assert_eq!(
            ReceiptSource::parse("provider"),
            Some(ReceiptSource::Provider)
        );
        assert_eq!(
            ReceiptSource::parse(" Operator "),
            Some(ReceiptSource::Operator)
        );
        assert_eq!(ReceiptSource::parse("webhook"), None);
        assert_eq!(ReceiptSource::parse(""), None);
    }
}
