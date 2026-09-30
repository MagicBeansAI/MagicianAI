//! Resolving an act against its envelopes at dispatch — plan phase 3.
//!
//! The resolver ([`super::resolver`]) is pure and knows nothing about storage.
//! This assembles what it needs, applies the mode, and records the debit. It is
//! the only place the three concerns meet, which is what keeps the decision
//! function testable without a store and the store useful without a dispatch.
//!
//! # Loose coupling is the point
//!
//! Nothing here is specific to outward acts, to OPC, or to the agentic executor.
//! A caller supplies a [`DispatchContext`] built from whatever it knows and gets
//! a [`GateOutcome`] back. The engagement identity list, the value and the
//! attachment count are **inputs** rather than lookups precisely so this does
//! not acquire a dependency on the engagement store, the billing model or the
//! asset ledger — none of which exist yet, and two of which may never be shaped
//! the way a lookup here would assume.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::magician_v2::agents::ConsequenceClass;
use crate::magician_v2::execution::EffectiveAction;

use super::resolver::resolve_any;
use super::store::{ApprovalEnvelopeStore, EnvelopeStoreScope};
use super::types::{
    ActFacts, ConsumptionEntry, EnvelopeDecision, EnvelopeMode, EnvelopeScope, NotCoveredReason,
};

/// Everything a caller knows about one act.
///
/// Every field the boundary predicates read is supplied rather than fetched.
/// See the module note: that is deliberate decoupling, not laziness.
#[derive(Debug, Clone)]
pub struct DispatchContext<'a> {
    /// Stable per-act key. The debit is idempotent on it, so a retry of one act
    /// cannot consume an envelope twice.
    pub act_ref: &'a str,
    pub effective: &'a EffectiveAction,
    pub consequence_class: ConsequenceClass,
    /// Which envelope scope this act belongs to. `None` means the caller could
    /// not place the act under a goal, program or engagement — so no envelope
    /// can apply and the answer is "ask".
    pub envelope_scope: Option<EnvelopeScope>,
    pub engagement_id: Option<&'a str>,
    pub engagement_identities: &'a [String],
    /// `None` is **unknown, not zero** — see [`ActFacts`], which fails the
    /// attachment predicate closed rather than letting a caller with no ledger
    /// to consult satisfy it by having nothing to report.
    pub attachments_outside_ledger: Option<usize>,
    pub value_micros: Option<u64>,
    pub now: DateTime<Utc>,
}

/// The field separator every derived id in this crate joins its parts with.
///
/// Declared per-module, like every other store here, so no module can be
/// silently re-separated by an edit to another.
const FIELD_SEP: char = '\u{1f}';

/// Turn an id a caller already holds into an envelope scope — or refuse to.
///
/// This is what [`DispatchContext::envelope_scope`] is meant to be built from.
/// Generic over the kind on purpose: dispatch sites differ in which id they hold
/// (a goal in the agentic executor, a program or an engagement elsewhere), and
/// every one of them needs the same two refusals, so the refusals live once
/// here rather than once per caller.
///
/// # Both refusals fail closed
///
/// `None` — absent id, or one that is blank or whitespace — yields no scope, and
/// an act with no scope resolves `no_envelope` and is asked about. That is the
/// correct answer for work no envelope was ever granted against, and it is the
/// answer a caller must accept rather than reaching for a placeholder: a
/// fabricated scope resolves against somebody else's consent, which is worse
/// than no consent at all.
///
/// An id containing [`FIELD_SEP`] (U+001F) is refused for the same reason
/// `run_state` refuses it. Scope keys are joined with that separator when
/// envelope ids are derived, so an id carrying one can move the boundary
/// between fields and make two different scopes derive one id.
///
/// The accepted id is carried through **verbatim**, not trimmed. Whitespace is
/// only inspected to decide whether the id is blank, because the grant path
/// does not canonicalise either — normalising on one side and not the other is
/// how an act stops matching the envelope granted for its own scope.
pub fn envelope_scope_from_id<F>(id: Option<&str>, kind: F) -> Option<EnvelopeScope>
where
    F: FnOnce(String) -> EnvelopeScope,
{
    let id = id?;
    if id.trim().is_empty() || id.contains(FIELD_SEP) {
        return None;
    }
    Some(kind(id.to_string()))
}

/// What the gate decided, and what it did about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// The mode is [`EnvelopeMode::Off`]. Nothing was resolved and nothing was
    /// written — behaviour is byte-for-byte what it was before envelopes
    /// existed, which is the plan's first acceptance criterion.
    NotEvaluated,
    /// Resolved for observation only. **The caller must still ask**, whatever
    /// the decision says.
    Shadow { decision: EnvelopeDecision },
    /// Covered, and the debit is recorded. The caller may proceed.
    Authorised {
        envelope_id: String,
        outcome: String,
        matched_predicates: Vec<String>,
    },
    /// Not covered. Ask, exactly as before envelopes existed.
    Refused { reason: NotCoveredReason },
}

impl GateOutcome {
    /// Whether the act may proceed **without** asking.
    ///
    /// False for everything except [`Self::Authorised`] — including `Shadow`,
    /// whose whole purpose is to observe while still asking, and `NotEvaluated`,
    /// which means no opinion was formed.
    pub fn authorises(&self) -> bool {
        matches!(self, Self::Authorised { .. })
    }

    /// The decision reached, for logging. `NotEvaluated` has none.
    pub fn decision(&self) -> Option<&EnvelopeDecision> {
        match self {
            Self::Shadow { decision } => Some(decision),
            _ => None,
        }
    }
}

/// Resolves acts against the envelopes stored for a scope.
#[derive(Debug, Clone)]
pub struct EnvelopeGate {
    store: ApprovalEnvelopeStore,
    scope: EnvelopeStoreScope,
}

impl EnvelopeGate {
    pub fn new(store: ApprovalEnvelopeStore, scope: EnvelopeStoreScope) -> Self {
        Self { store, scope }
    }

    /// Evaluate one act.
    ///
    /// # Why the debit happens before the act, not after
    ///
    /// A crash between debiting and acting costs one act of headroom. A crash
    /// between acting and debiting costs the cap entirely — the act happened and
    /// nothing recorded it, so the next resolution sees unused headroom and the
    /// envelope authorises more than it was granted for. Debiting first fails in
    /// the direction of asking, which is the whole posture.
    ///
    /// The debit is idempotent on `act_ref`, so the retry after such a crash
    /// resumes rather than double-counting.
    ///
    /// # Shadow never debits
    ///
    /// An act that was still asked about did not consume an envelope. Debiting
    /// in shadow would spend caps on acts the owner approved by hand, so the
    /// ledger would describe a history that never happened — and phase 3 exists
    /// precisely to compare the shadow against reality.
    pub fn evaluate(&self, mode: EnvelopeMode, ctx: &DispatchContext<'_>) -> Result<GateOutcome> {
        if matches!(mode, EnvelopeMode::Off) {
            return Ok(GateOutcome::NotEvaluated);
        }

        // Checked before the store is touched: an act with no stable id cannot
        // be debited idempotently, so every such act would share one debit slot
        // and an envelope for ten would authorise an unbounded number. The
        // resolver refuses it too; this avoids the read as well.
        if ctx.act_ref.trim().is_empty() {
            return Ok(self.decide(mode, NotCoveredReason::UnidentifiedAct));
        }

        let Some(envelope_scope) = ctx.envelope_scope.as_ref() else {
            return Ok(self.without_envelope(mode));
        };

        let states = self.store.load_for_scope(&self.scope, envelope_scope)?;
        if states.is_empty() {
            return Ok(self.without_envelope(mode));
        }

        let facts = ActFacts {
            act_ref: ctx.act_ref,
            effective: ctx.effective,
            consequence_class: ctx.consequence_class,
            engagement_id: ctx.engagement_id,
            engagement_identities: ctx.engagement_identities,
            attachments_outside_ledger: ctx.attachments_outside_ledger,
            value_micros: ctx.value_micros,
            now: ctx.now,
        };
        let decision = resolve_any(&states, &facts);

        if !mode.may_authorise() {
            return Ok(GateOutcome::Shadow { decision });
        }

        match decision {
            EnvelopeDecision::Covered {
                envelope_id,
                outcome,
                matched_predicates,
            } => {
                // A debit refused because the envelope was consumed between the
                // resolve and the write degrades to asking rather than failing
                // the dispatch. The act was covered a moment ago and is not now;
                // that is exactly the situation "ask" exists for, and returning
                // an error would turn a race into an outage.
                match self.store.record_consumption(
                    &self.scope,
                    &envelope_id,
                    &ConsumptionEntry {
                        act_ref: ctx.act_ref.to_string(),
                        at: ctx.now,
                        recipients: ctx.effective.recipients.clone(),
                        matched_predicates: matched_predicates.clone(),
                        value_micros: ctx.value_micros,
                    },
                ) {
                    Ok(_) => Ok(GateOutcome::Authorised {
                        envelope_id,
                        outcome,
                        matched_predicates,
                    }),
                    Err(_) => Ok(GateOutcome::Refused {
                        reason: NotCoveredReason::LimitReached {
                            limit: "max_acts".to_string(),
                        },
                    }),
                }
            },
            EnvelopeDecision::NotCovered { reason } => Ok(GateOutcome::Refused { reason }),
        }
    }

    /// No scope, or no envelope granted against it.
    ///
    /// Shadow still reports the decision, because "there was nothing to apply"
    /// is exactly what phase 3 needs to learn: an outward act nobody scoped is
    /// an act no envelope could ever have covered.
    fn without_envelope(&self, mode: EnvelopeMode) -> GateOutcome {
        self.decide(mode, NotCoveredReason::NoEnvelope)
    }

    /// Render one refusal into the shape the current mode calls for: enforcing
    /// refuses, shadow reports and still asks.
    fn decide(&self, mode: EnvelopeMode, reason: NotCoveredReason) -> GateOutcome {
        if mode.may_authorise() {
            GateOutcome::Refused { reason }
        } else {
            GateOutcome::Shadow {
                decision: EnvelopeDecision::NotCovered { reason },
            }
        }
    }
}

/// A one-line summary for the shadow log.
///
/// Phase 3's output is a comparison against reality, so the line has to carry
/// enough to make that comparison without re-running anything: what was decided,
/// under which envelope, and on what grounds.
pub fn shadow_log_line(decision: &EnvelopeDecision) -> String {
    match decision {
        EnvelopeDecision::Covered {
            envelope_id,
            outcome,
            matched_predicates,
        } => format!(
            "would_cover envelope={envelope_id} outcome={outcome:?} matched={}",
            matched_predicates.join("+")
        ),
        EnvelopeDecision::NotCovered { reason } => {
            format!("would_ask reason={}", reason_label(reason))
        },
    }
}

/// A stable token per refusal, so a shadow log can be counted by cause.
pub fn reason_label(reason: &NotCoveredReason) -> String {
    match reason {
        NotCoveredReason::NoEnvelope => "no_envelope".to_string(),
        NotCoveredReason::NotEnforcing => "not_enforcing".to_string(),
        NotCoveredReason::ActionNotBindable { escape_hatches } => {
            format!("action_not_bindable[{}]", escape_hatches.join("+"))
        },
        NotCoveredReason::ClassNotCoverable { class, kind } => {
            format!("class_not_coverable[{}/{kind}]", class.as_str())
        },
        NotCoveredReason::ClassNotGranted { class } => {
            format!("class_not_granted[{}]", class.as_str())
        },
        NotCoveredReason::Revoked => "revoked".to_string(),
        NotCoveredReason::Expired => "expired".to_string(),
        NotCoveredReason::LimitReached { limit } => format!("limit_reached[{limit}]"),
        NotCoveredReason::PredicateFailed { predicate, .. } => {
            format!("predicate_failed[{predicate}]")
        },
        NotCoveredReason::NotInReviewedBatch { .. } => "not_in_reviewed_batch".to_string(),
        NotCoveredReason::UnidentifiedAct => "unidentified_act".to_string(),
    }
}

#[cfg(test)]
mod dispatch_scope_tests {
    use super::*;

    use chrono::{Duration, TimeZone};

    use crate::magician_v2::approval_envelopes::store::GrantEnvelope;
    use crate::magician_v2::approval_envelopes::types::{EnvelopeKind, EnvelopeLimits};
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    const GOAL: &str = "goal-42";
    const OUTCOME: &str = "correspond with the people this goal is about";

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap()
    }

    fn sending(recipients: &[&str]) -> EffectiveAction {
        EffectiveAction {
            capability: "agentmail-send".to_string(),
            action: "send".to_string(),
            recipients: recipients.iter().map(|to| (*to).to_string()).collect(),
            escape_hatches: Vec::new(),
            unmodelled_tokens: Vec::new(),
        }
    }

    /// A gate over a store holding one standing envelope granted for `GOAL`.
    fn gate_for_goal() -> (
        EnvelopeGate,
        ApprovalEnvelopeStore,
        EnvelopeStoreScope,
        String,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ApprovalEnvelopeStore::new(ArtifactV2Workspace::new(dir.path().to_path_buf()));
        let scope = EnvelopeStoreScope::new("anonymous", "default");
        let envelope_id = store
            .grant(
                &scope,
                &GrantEnvelope {
                    scope: EnvelopeScope::Goal(GOAL.to_string()),
                    outcome: OUTCOME.to_string(),
                    kind: EnvelopeKind::Standing,
                    covers: vec![ConsequenceClass::BoundedCommunication],
                    limits: EnvelopeLimits {
                        max_acts: Some(5),
                        expires_at: Some(at() + Duration::days(7)),
                        ..EnvelopeLimits::default()
                    },
                    boundary: Vec::new(),
                    granted_by: "owner".to_string(),
                },
                at(),
            )
            .expect("grant")
            .envelope_id;
        (
            EnvelopeGate::new(store.clone(), scope.clone()),
            store,
            scope,
            envelope_id,
            dir,
        )
    }

    fn act<'a>(
        act_ref: &'a str,
        effective: &'a EffectiveAction,
        identities: &'a [String],
        envelope_scope: Option<EnvelopeScope>,
    ) -> DispatchContext<'a> {
        DispatchContext {
            act_ref,
            effective,
            consequence_class: ConsequenceClass::BoundedCommunication,
            envelope_scope,
            engagement_id: None,
            engagement_identities: identities,
            attachments_outside_ledger: Some(0),
            value_micros: None,
            now: at(),
        }
    }

    /// An act that names the work it belongs to resolves against the envelope
    /// granted for exactly that work.
    ///
    /// This is the property the hardcoded `envelope_scope: None` at dispatch
    /// destroyed: with no scope on the act, no envelope could ever be selected,
    /// every resolution answered `no_envelope`, and a granted envelope had
    /// nothing it could ever enable.
    #[test]
    fn an_act_scoped_to_its_goal_resolves_against_that_goals_envelope() {
        let (gate, store, store_scope, envelope_id, _dir) = gate_for_goal();
        let effective = sending(&["a@example.com"]);
        let identities = vec!["a@example.com".to_string()];
        let scope = envelope_scope_from_id(Some(GOAL), EnvelopeScope::Goal);
        assert_eq!(scope, Some(EnvelopeScope::Goal(GOAL.to_string())));

        let outcome = gate
            .evaluate(
                EnvelopeMode::Enforcing,
                &act("act-1", &effective, &identities, scope),
            )
            .expect("evaluate");

        match &outcome {
            GateOutcome::Authorised {
                envelope_id: named,
                outcome: consented_to,
                ..
            } => {
                assert_eq!(
                    named, &envelope_id,
                    "the act must name the envelope it used"
                );
                assert_eq!(consented_to, OUTCOME);
            },
            other => panic!("expected the goal's envelope to cover this act, got {other:?}"),
        }
        assert!(outcome.authorises());

        let state = store
            .load(&store_scope, &envelope_id)
            .expect("load")
            .expect("envelope");
        assert_eq!(
            state.acts_used(),
            1,
            "an authorised act debits exactly once"
        );
    }

    /// An act that names no work resolves `no_envelope` and is asked about, and
    /// the envelope it did not reach is left untouched.
    ///
    /// Pins the fail-closed half of the same wiring: threading a scope must not
    /// become "thread something", because an act placed under the wrong goal
    /// spends consent its owner gave for different work.
    #[test]
    fn an_act_that_names_no_goal_resolves_no_envelope() {
        let (gate, store, store_scope, envelope_id, _dir) = gate_for_goal();
        let effective = sending(&["a@example.com"]);
        let identities = vec!["a@example.com".to_string()];
        let unscoped = envelope_scope_from_id(None, EnvelopeScope::Goal);
        assert_eq!(unscoped, None);

        assert_eq!(
            gate.evaluate(
                EnvelopeMode::Enforcing,
                &act("act-1", &effective, &identities, unscoped),
            )
            .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::NoEnvelope
            }
        );

        let state = store
            .load(&store_scope, &envelope_id)
            .expect("load")
            .expect("envelope");
        assert_eq!(
            state.acts_used(),
            0,
            "an act that reached no envelope consumed none"
        );
    }

    /// One goal's envelope is not another's. The scope key carries the kind and
    /// the id, so consent granted for one piece of work cannot be spent by an
    /// act belonging to a different one.
    #[test]
    fn another_goals_envelope_does_not_cover_this_act() {
        let (gate, _store, _store_scope, _envelope_id, _dir) = gate_for_goal();
        let effective = sending(&["a@example.com"]);
        let identities = vec!["a@example.com".to_string()];
        let elsewhere = envelope_scope_from_id(Some("goal-99"), EnvelopeScope::Goal);
        assert_eq!(elsewhere, Some(EnvelopeScope::Goal("goal-99".to_string())));

        assert_eq!(
            gate.evaluate(
                EnvelopeMode::Enforcing,
                &act("act-1", &effective, &identities, elsewhere),
            )
            .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::NoEnvelope
            }
        );
    }

    /// Shadow resolves the scope, reports that the act WOULD have been covered,
    /// authorises nothing and debits nothing.
    ///
    /// Both halves matter. A shadow that authorised would be enforcing under
    /// another name; a shadow that debited would spend caps on acts the owner
    /// went on to approve by hand, so the ledger would describe a history that
    /// never happened — and comparing the shadow against reality is the entire
    /// point of running it.
    #[test]
    fn shadow_resolves_the_scope_without_debiting_or_authorising() {
        let (gate, store, store_scope, envelope_id, _dir) = gate_for_goal();
        let effective = sending(&["a@example.com"]);
        let identities = vec!["a@example.com".to_string()];
        let scope = envelope_scope_from_id(Some(GOAL), EnvelopeScope::Goal);

        let outcome = gate
            .evaluate(
                EnvelopeMode::Shadow,
                &act("act-1", &effective, &identities, scope),
            )
            .expect("evaluate");

        match &outcome {
            GateOutcome::Shadow { decision } => {
                assert_eq!(
                    decision.envelope_id(),
                    Some(envelope_id.as_str()),
                    "shadow must report WHICH envelope would have covered the act"
                );
            },
            other => panic!("expected a shadow decision, got {other:?}"),
        }
        assert!(
            !outcome.authorises(),
            "shadow observes; it must never let an act through"
        );
        assert_eq!(
            shadow_log_line(outcome.decision().expect("a shadow decision")),
            format!("would_cover envelope={envelope_id} outcome=\"{OUTCOME}\" matched=")
        );

        let state = store
            .load(&store_scope, &envelope_id)
            .expect("load")
            .expect("envelope");
        assert_eq!(state.acts_used(), 0, "shadow must not debit");
    }

    /// An absent or blank id yields no scope rather than a placeholder one.
    ///
    /// The temptation at a dispatch site with nothing to hand is to invent
    /// something — an empty string, "unknown", the agent's own id. Every one of
    /// those is a scope an envelope could be granted against by accident, so the
    /// refusal has to live here rather than in each caller's judgement.
    #[test]
    fn an_absent_or_blank_id_yields_no_scope_rather_than_a_placeholder() {
        assert_eq!(envelope_scope_from_id(None, EnvelopeScope::Goal), None);
        assert_eq!(envelope_scope_from_id(Some(""), EnvelopeScope::Goal), None);
        assert_eq!(
            envelope_scope_from_id(Some("   \t\n"), EnvelopeScope::Goal),
            None
        );

        // A real id is carried through verbatim, whitespace included: the grant
        // path does not canonicalise either, and normalising on one side only
        // is how an act stops matching its own envelope.
        assert_eq!(
            envelope_scope_from_id(Some(" goal-42 "), EnvelopeScope::Program),
            Some(EnvelopeScope::Program(" goal-42 ".to_string()))
        );
    }

    /// An id carrying U+001F is refused, because that byte is the separator
    /// envelope ids are derived over.
    ///
    /// Without this, `goal-1\u{1f}x` and a differently-split pair of fields can
    /// derive the same id, so one scope's grant becomes reachable from another's
    /// key — the same separator-injection hole `run_state::open` refuses at its
    /// own door.
    #[test]
    fn an_id_carrying_the_field_separator_is_refused() {
        for injected in [
            "goal-1\u{1f}goal-2",
            "\u{1f}",
            "\u{1f}goal-1",
            "goal-1\u{1f}",
        ] {
            assert_eq!(
                envelope_scope_from_id(Some(injected), EnvelopeScope::Goal),
                None,
                "an id containing the field separator must never become a scope"
            );
            assert_eq!(
                envelope_scope_from_id(Some(injected), EnvelopeScope::Engagement),
                None,
                "the refusal is the id's, not one kind's"
            );
        }
    }
}

/// The carrier the agentic dispatch actually derives its scope from.
///
/// `log_envelope_shadow` in the agentic executor builds its scope from the
/// §4.2c [`EngagementAuthorityRef`] the execution carries, and from nothing
/// else. These tests run that exact derivation — the same expression, against
/// the same store — so the properties the dispatch depends on are pinned here
/// rather than only inside a 39k-line file that cannot be unit-tested cheaply.
///
/// The reason the engagement ref is the carrier and `goal_id` is not: the ref is
/// inherited verbatim from the parent's durable `ExecutionRun` and is never
/// sourced from model-supplied targets, whereas a delegated execution's
/// `goal_id` is a truncated free-text label the delegating agent wrote. An
/// agent that could name its own scope could name a scope the owner granted an
/// envelope for and spend consent given for entirely different work.
#[cfg(test)]
mod engagement_carrier_tests {
    use super::*;

    use chrono::{Duration, TimeZone};

    use crate::magician_v2::approval_envelopes::store::GrantEnvelope;
    use crate::magician_v2::approval_envelopes::types::{
        BoundaryPredicate, EnvelopeKind, EnvelopeLimits,
    };
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::engagements::EngagementAuthorityRef;

    const ENGAGEMENT: &str = "eng-7";
    const OUTCOME: &str = "correspond with the people this engagement is with";
    const COUNTERPARTY: &str = "a@example.com";

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap()
    }

    fn carrying(engagement_id: &str, authority_revision: u64) -> EngagementAuthorityRef {
        EngagementAuthorityRef {
            engagement_id: engagement_id.to_string(),
            authority_revision,
        }
    }

    fn sending(recipients: &[&str]) -> EffectiveAction {
        EffectiveAction {
            capability: "agentmail-send".to_string(),
            action: "send".to_string(),
            recipients: recipients.iter().map(|to| (*to).to_string()).collect(),
            escape_hatches: Vec::new(),
            unmodelled_tokens: Vec::new(),
        }
    }

    struct Fixture {
        gate: EnvelopeGate,
        store: ApprovalEnvelopeStore,
        store_scope: EnvelopeStoreScope,
        envelope_id: String,
        _dir: tempfile::TempDir,
    }

    impl Fixture {
        /// How many acts have actually been debited against the envelope.
        fn acts_used(&self) -> u32 {
            self.store
                .load(&self.store_scope, &self.envelope_id)
                .expect("load")
                .expect("envelope")
                .acts_used()
        }
    }

    /// A gate over a store holding one standing envelope granted for
    /// `ENGAGEMENT`, carrying whatever boundary the test needs.
    fn gate_for_engagement(boundary: Vec<BoundaryPredicate>) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ApprovalEnvelopeStore::new(ArtifactV2Workspace::new(dir.path().to_path_buf()));
        let store_scope = EnvelopeStoreScope::new("anonymous", "default");
        let envelope_id = store
            .grant(
                &store_scope,
                &GrantEnvelope {
                    scope: EnvelopeScope::Engagement(ENGAGEMENT.to_string()),
                    outcome: OUTCOME.to_string(),
                    kind: EnvelopeKind::Standing,
                    covers: vec![ConsequenceClass::BoundedCommunication],
                    limits: EnvelopeLimits {
                        max_acts: Some(5),
                        expires_at: Some(at() + Duration::days(7)),
                        ..EnvelopeLimits::default()
                    },
                    boundary,
                    granted_by: "owner".to_string(),
                },
                at(),
            )
            .expect("grant")
            .envelope_id;
        Fixture {
            gate: EnvelopeGate::new(store.clone(), store_scope.clone()),
            store,
            store_scope,
            envelope_id,
            _dir: dir,
        }
    }

    /// The bounded envelope the dispatch is realistically resolved against: it
    /// may only reach identities already on the engagement.
    fn bounded() -> Fixture {
        gate_for_engagement(vec![BoundaryPredicate::RecipientInEngagement {
            engagement_id: ENGAGEMENT.to_string(),
        }])
    }

    /// **The derivation under test.** Every field here is what
    /// `log_envelope_shadow` passes, including `attachments_outside_ledger:
    /// None` — the dispatch has no asset ledger to consult, and this must stay
    /// faithful or the tests would prove a wiring nobody runs.
    fn act_from_execution<'a>(
        act_ref: &'a str,
        effective: &'a EffectiveAction,
        carried: Option<&'a EngagementAuthorityRef>,
        identities: &'a [String],
    ) -> DispatchContext<'a> {
        let engagement_id = carried.map(|carried| carried.engagement_id.as_str());
        DispatchContext {
            act_ref,
            effective,
            consequence_class: ConsequenceClass::BoundedCommunication,
            envelope_scope: envelope_scope_from_id(engagement_id, EnvelopeScope::Engagement),
            engagement_id,
            engagement_identities: identities,
            attachments_outside_ledger: None,
            value_micros: None,
            now: at(),
        }
    }

    /// The keystone: an act dispatched by an execution that carries an
    /// engagement authority resolves against the envelope granted for exactly
    /// that engagement, names it, and debits it once.
    ///
    /// This is the property the hardcoded `envelope_scope: None` at dispatch
    /// destroyed. With no scope on the act, no envelope could ever be selected,
    /// every resolution answered `no_envelope`, and a granted envelope had
    /// nothing it could ever enable — so enablement had nothing to enable.
    #[test]
    fn an_execution_carrying_an_engagement_resolves_against_that_engagements_envelope() {
        let fixture = bounded();
        let effective = sending(&[COUNTERPARTY]);
        let identities = vec![COUNTERPARTY.to_string()];
        let carried = carrying(ENGAGEMENT, 3);

        let dispatch = act_from_execution("act-1", &effective, Some(&carried), &identities);
        assert_eq!(
            dispatch.envelope_scope,
            Some(EnvelopeScope::Engagement(ENGAGEMENT.to_string())),
            "the carried authority is what names the scope"
        );

        match fixture
            .gate
            .evaluate(EnvelopeMode::Enforcing, &dispatch)
            .expect("evaluate")
        {
            GateOutcome::Authorised {
                envelope_id,
                outcome,
                matched_predicates,
            } => {
                assert_eq!(envelope_id, fixture.envelope_id);
                assert_eq!(outcome, OUTCOME);
                assert_eq!(matched_predicates, vec!["recipient_in_engagement"]);
            },
            other => panic!("expected the engagement's envelope to cover this act, got {other:?}"),
        }
        assert_eq!(fixture.acts_used(), 1);
    }

    /// The scope is the engagement id alone. A revision bump — every mutation
    /// bumps it — must not orphan the envelope granted for that engagement.
    ///
    /// Had the revision entered the scope key, the first narrowing of an
    /// engagement would have silently made every envelope granted against it
    /// unreachable: fail-closed in direction, but a consent the owner gave and
    /// can still see would stop applying with nothing anywhere saying why.
    #[test]
    fn a_bumped_authority_revision_still_reaches_the_same_envelope() {
        let fixture = bounded();
        let effective = sending(&[COUNTERPARTY]);
        let identities = vec![COUNTERPARTY.to_string()];
        let before = carrying(ENGAGEMENT, 3);
        let after = carrying(ENGAGEMENT, 9);

        assert_eq!(
            act_from_execution("act-1", &effective, Some(&before), &identities).envelope_scope,
            act_from_execution("act-2", &effective, Some(&after), &identities).envelope_scope,
            "the revision pins the authority, not the scope"
        );

        for (act_ref, carried) in [("act-1", &before), ("act-2", &after)] {
            match fixture
                .gate
                .evaluate(
                    EnvelopeMode::Enforcing,
                    &act_from_execution(act_ref, &effective, Some(carried), &identities),
                )
                .expect("evaluate")
            {
                GateOutcome::Authorised { envelope_id, .. } => {
                    assert_eq!(envelope_id, fixture.envelope_id)
                },
                other => panic!("{act_ref} should have been covered, got {other:?}"),
            }
        }
        assert_eq!(
            fixture.acts_used(),
            2,
            "two distinct acts debit twice; neither is a replay of the other"
        );
    }

    /// An execution carrying no engagement stays unscoped, is asked about, and
    /// leaves the envelope it never reached untouched.
    ///
    /// This is the half that must survive the wiring. Threading a scope must
    /// never become "thread something": an act placed under a scope it does not
    /// belong to spends consent the owner gave for different work, which is the
    /// exact failure envelopes exist to prevent.
    #[test]
    fn an_execution_carrying_no_engagement_resolves_no_envelope() {
        let fixture = bounded();
        let effective = sending(&[COUNTERPARTY]);
        let identities = vec![COUNTERPARTY.to_string()];

        let dispatch = act_from_execution("act-1", &effective, None, &identities);
        assert_eq!(dispatch.envelope_scope, None);
        assert_eq!(dispatch.engagement_id, None);

        assert_eq!(
            fixture
                .gate
                .evaluate(EnvelopeMode::Enforcing, &dispatch)
                .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::NoEnvelope
            }
        );
        assert_eq!(fixture.acts_used(), 0);
    }

    /// One engagement's envelope is not another's.
    #[test]
    fn another_engagements_authority_does_not_reach_this_envelope() {
        let fixture = bounded();
        let effective = sending(&[COUNTERPARTY]);
        let identities = vec![COUNTERPARTY.to_string()];
        let elsewhere = carrying("eng-8", 3);

        assert_eq!(
            fixture
                .gate
                .evaluate(
                    EnvelopeMode::Enforcing,
                    &act_from_execution("act-1", &effective, Some(&elsewhere), &identities),
                )
                .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::NoEnvelope
            }
        );
        assert_eq!(fixture.acts_used(), 0);
    }

    /// The scope key carries the KIND, so an id that happens to name a goal as
    /// well reaches nothing granted for the engagement of the same name.
    ///
    /// Ids are opaque strings from different namespaces; without the kind in
    /// the key, a goal called `eng-7` would spend the engagement's consent.
    #[test]
    fn the_same_id_under_a_different_kind_reaches_nothing() {
        let fixture = bounded();
        let effective = sending(&[COUNTERPARTY]);
        let identities = vec![COUNTERPARTY.to_string()];

        assert_ne!(
            EnvelopeScope::Goal(ENGAGEMENT.to_string()).as_key(),
            EnvelopeScope::Engagement(ENGAGEMENT.to_string()).as_key()
        );

        let mut as_a_goal = act_from_execution("act-1", &effective, None, &identities);
        as_a_goal.envelope_scope = envelope_scope_from_id(Some(ENGAGEMENT), EnvelopeScope::Goal);

        assert_eq!(
            fixture
                .gate
                .evaluate(EnvelopeMode::Enforcing, &as_a_goal)
                .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::NoEnvelope
            }
        );
        assert_eq!(fixture.acts_used(), 0);
    }

    /// Shadow resolves the carried scope, reports which envelope WOULD have
    /// covered the act, authorises nothing and debits nothing.
    ///
    /// Both halves matter. A shadow that authorised would be enforcing under
    /// another name; a shadow that debited would spend caps on acts the owner
    /// then approved by hand, so the ledger would describe a history that never
    /// happened — and comparing the shadow against reality is the entire reason
    /// to run it before enforcing anything.
    #[test]
    fn shadow_resolves_the_engagement_scope_without_debiting_or_authorising() {
        let fixture = bounded();
        let effective = sending(&[COUNTERPARTY]);
        let identities = vec![COUNTERPARTY.to_string()];
        let carried = carrying(ENGAGEMENT, 3);

        let outcome = fixture
            .gate
            .evaluate(
                EnvelopeMode::Shadow,
                &act_from_execution("act-1", &effective, Some(&carried), &identities),
            )
            .expect("evaluate");

        match &outcome {
            GateOutcome::Shadow { decision } => assert_eq!(
                decision.envelope_id(),
                Some(fixture.envelope_id.as_str()),
                "shadow must report WHICH envelope would have covered the act"
            ),
            other => panic!("expected a shadow decision, got {other:?}"),
        }
        assert!(
            !outcome.authorises(),
            "shadow observes; it must never let an act through"
        );
        assert_eq!(
            shadow_log_line(outcome.decision().expect("a shadow decision")),
            format!(
                "would_cover envelope={} outcome=\"{OUTCOME}\" matched=recipient_in_engagement",
                fixture.envelope_id
            )
        );
        assert_eq!(fixture.acts_used(), 0, "shadow must not debit");
    }

    /// An empty identity list refuses every recipient rather than passing
    /// vacuously — which is the state the dispatch is in today, because the
    /// engagement store does not yet supply identities there.
    ///
    /// This is why threading the engagement id was safe to do before the
    /// identities exist: a predicate over an empty collection that answered
    /// "none of them are outside" would turn a boundary built entirely from
    /// recipients into a rubber stamp, and the refusal must name the recipient
    /// so an owner can see which one was not on the engagement.
    #[test]
    fn an_empty_identity_list_refuses_every_recipient_rather_than_passing_vacuously() {
        let fixture = bounded();
        let effective = sending(&[COUNTERPARTY]);
        let nobody: Vec<String> = Vec::new();
        let carried = carrying(ENGAGEMENT, 3);

        assert_eq!(
            fixture
                .gate
                .evaluate(
                    EnvelopeMode::Enforcing,
                    &act_from_execution("act-1", &effective, Some(&carried), &nobody),
                )
                .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::PredicateFailed {
                    predicate: "recipient_in_engagement".to_string(),
                    detail: format!("{COUNTERPARTY} is not an identity on {ENGAGEMENT}"),
                }
            }
        );
        assert_eq!(fixture.acts_used(), 0);
    }

    /// An act naming NO recipients is refused too. "Nobody" is not "everybody
    /// on the engagement": an unaddressed act would otherwise satisfy a
    /// boundary made entirely of recipients by having none to check.
    #[test]
    fn an_act_naming_no_recipients_cannot_satisfy_a_recipient_boundary() {
        let fixture = bounded();
        let effective = sending(&[]);
        let identities = vec![COUNTERPARTY.to_string()];
        let carried = carrying(ENGAGEMENT, 3);

        assert_eq!(
            fixture
                .gate
                .evaluate(
                    EnvelopeMode::Enforcing,
                    &act_from_execution("act-1", &effective, Some(&carried), &identities),
                )
                .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::PredicateFailed {
                    predicate: "recipient_in_engagement".to_string(),
                    detail: "act names no recipients".to_string(),
                }
            }
        );
        assert_eq!(fixture.acts_used(), 0);
    }

    /// The dispatch cannot see an asset ledger, so an envelope bounded to one
    /// is refused there rather than satisfied by having nothing to report.
    ///
    /// Unknown is not zero. This pins the dispatch's `attachments_outside_ledger:
    /// None` as a refusal, so a caller with no ledger to consult can never
    /// clear a boundary that exists to keep unlisted attachments out.
    #[test]
    fn an_envelope_bounded_to_the_asset_ledger_is_refused_where_provenance_is_unknown() {
        let fixture = gate_for_engagement(vec![BoundaryPredicate::NoAttachmentOutsideLedger]);
        let effective = sending(&[COUNTERPARTY]);
        let identities = vec![COUNTERPARTY.to_string()];
        let carried = carrying(ENGAGEMENT, 3);

        assert_eq!(
            fixture
                .gate
                .evaluate(
                    EnvelopeMode::Enforcing,
                    &act_from_execution("act-1", &effective, Some(&carried), &identities),
                )
                .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::PredicateFailed {
                    predicate: "no_attachment_outside_ledger".to_string(),
                    detail: "attachment provenance is unknown to the caller".to_string(),
                }
            }
        );
        assert_eq!(fixture.acts_used(), 0);
    }

    /// An act whose disclosure record is missing carries no key, and is refused
    /// as `unidentified_act` rather than borrowing a placeholder one.
    #[test]
    fn an_act_with_no_disclosure_record_is_refused_as_unidentified() {
        let fixture = bounded();
        let effective = sending(&[COUNTERPARTY]);
        let identities = vec![COUNTERPARTY.to_string()];
        let carried = carrying(ENGAGEMENT, 3);

        // Exactly what the dispatch passes when `outward_act_ref` is `None`.
        for act_ref in ["", "   "] {
            assert_eq!(
                fixture
                    .gate
                    .evaluate(
                        EnvelopeMode::Enforcing,
                        &act_from_execution(act_ref, &effective, Some(&carried), &identities),
                    )
                    .expect("evaluate"),
                GateOutcome::Refused {
                    reason: NotCoveredReason::UnidentifiedAct
                }
            );
        }
        assert_eq!(fixture.acts_used(), 0);
    }

    /// Why the refusal above is the only safe answer: the ledger is idempotent
    /// ON the key, so two DIFFERENT acts sharing one key debit exactly once.
    ///
    /// A dispatch that handed unrecorded acts a shared placeholder would
    /// therefore put all of them in a single debit slot — the first recorded,
    /// every later one read as a replay of it — and an envelope for five acts
    /// would authorise an unbounded number of sends to unbounded recipients.
    /// Here two acts reaching two different people cost exactly one slot.
    #[test]
    fn two_acts_sharing_one_key_debit_once_which_is_why_an_unrecorded_act_gets_none() {
        let fixture = bounded();
        let identities = vec![COUNTERPARTY.to_string(), "b@example.com".to_string()];
        let carried = carrying(ENGAGEMENT, 3);
        let first = sending(&[COUNTERPARTY]);
        let second = sending(&["b@example.com"]);

        for effective in [&first, &second] {
            assert!(
                fixture
                    .gate
                    .evaluate(
                        EnvelopeMode::Enforcing,
                        &act_from_execution("shared", effective, Some(&carried), &identities),
                    )
                    .expect("evaluate")
                    .authorises(),
                "both acts pass, because the second reads as a replay of the first"
            );
        }
        assert_eq!(
            fixture.acts_used(),
            1,
            "two acts to two different people cost ONE act of a five-act envelope"
        );
    }
}
