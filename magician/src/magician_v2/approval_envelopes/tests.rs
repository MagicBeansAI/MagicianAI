//! §10's acceptance criteria, as behaviour.
//!
//! Each test names the criterion it pins. They are written against the resolver
//! and store rather than against a mock, because the properties that matter here
//! — commitment is never covered, a spent cap asks, a batch is closed — are
//! exactly the ones a mock would be written to satisfy.

use chrono::{Duration, TimeZone, Utc};

use crate::magician_v2::agents::ConsequenceClass;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::execution::EffectiveAction;

use super::resolver::{resolve, resolve_any};
use super::store::{ApprovalEnvelopeStore, EnvelopeStoreScope, GrantEnvelope};
use super::types::{
    ActFacts, BatchInstance, BoundaryPredicate, ConsumptionEntry, EnvelopeDecision, EnvelopeKind,
    EnvelopeLimits, EnvelopeMode, EnvelopeScope, EnvelopeState, NotCoveredReason,
};
use super::{envelope_mode, install_envelope_mode};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap()
}

fn bindable(capability: &str, recipients: &[&str]) -> EffectiveAction {
    EffectiveAction {
        capability: capability.to_string(),
        action: "send".to_string(),
        recipients: recipients.iter().map(|r| (*r).to_string()).collect(),
        escape_hatches: Vec::new(),
        unmodelled_tokens: Vec::new(),
    }
}

fn facts<'a>(
    act_ref: &'a str,
    effective: &'a EffectiveAction,
    class: ConsequenceClass,
    identities: &'a [String],
) -> ActFacts<'a> {
    ActFacts {
        act_ref,
        effective,
        consequence_class: class,
        engagement_id: Some("eng-1"),
        engagement_identities: identities,
        attachments_outside_ledger: Some(0),
        value_micros: None,
        now: now(),
    }
}

fn standing(covers: Vec<ConsequenceClass>, limits: EnvelopeLimits) -> EnvelopeState {
    EnvelopeState {
        envelope: super::types::ApprovalEnvelope {
            envelope_id: "env-test".to_string(),
            scope: EnvelopeScope::Engagement("eng-1".to_string()),
            outcome: "approach and correspond with accelerators".to_string(),
            kind: EnvelopeKind::Standing,
            covers,
            limits,
            boundary: Vec::new(),
            granted_by: "owner".to_string(),
            granted_at: now() - Duration::hours(1),
            revoked_at: None,
        },
        consumed: Vec::new(),
    }
}

/// Limits carrying an expiry. A standing envelope without one is refused at the
/// door, so every store-granted standing envelope in these tests needs it.
fn expiring() -> EnvelopeLimits {
    EnvelopeLimits {
        expires_at: Some(now() + Duration::days(7)),
        ..EnvelopeLimits::default()
    }
}

fn store() -> (ApprovalEnvelopeStore, EnvelopeStoreScope, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = ArtifactV2Workspace::new(dir.path().to_path_buf());
    (
        ApprovalEnvelopeStore::new(workspace),
        EnvelopeStoreScope::new("anonymous", "default"),
        dir,
    )
}

// ── §10: with no envelopes, behaviour is byte-for-byte today's ──────────────

/// The default posture. An unconfigured process must authorise nothing, or
/// every entry point nobody remembered to wire becomes a silent approver.
///
/// Reads `EnvelopeMode::default()` rather than `envelope_mode()`: the global is
/// a `OnceLock`, and a test that asserts on it would pass or fail depending on
/// whether the install test happened to run first in the same process. nextest
/// gives each test its own process and would have hidden that; `cargo test`
/// would not.
#[test]
fn the_default_mode_authorises_nothing() {
    assert_eq!(EnvelopeMode::default(), EnvelopeMode::Off);
    assert!(!EnvelopeMode::Off.may_authorise());
    assert!(
        !EnvelopeMode::Shadow.may_authorise(),
        "shadow logs what WOULD have been covered and still asks — a shadow that \
         authorised would not be a shadow"
    );
    assert!(EnvelopeMode::Enforcing.may_authorise());
}

/// The first caller wins, so a later one cannot quietly promote shadow to
/// enforcing.
///
/// Deliberately does not assert the FIRST install succeeded: this is the only
/// test that installs, but asserting it would make the test depend on being the
/// first to run in its process. What matters is the property — after any
/// install, promotion is refused.
#[test]
fn the_mode_cannot_be_promoted_after_it_is_installed() {
    let _ = install_envelope_mode(EnvelopeMode::Shadow);

    assert!(
        !install_envelope_mode(EnvelopeMode::Enforcing),
        "a second install must be refused"
    );
    assert_ne!(
        envelope_mode(),
        EnvelopeMode::Enforcing,
        "a refused install must not take effect anyway"
    );
}

/// No candidate envelope means ask — §6's inversion of Resource Authority's
/// fail-open posture, and the property everything else rests on.
#[test]
fn no_envelope_means_ask() {
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let identities = vec!["a@example.com".to_string()];
    let decision = resolve_any(
        &[],
        &facts(
            "act-1",
            &effective,
            ConsequenceClass::BoundedCommunication,
            &identities,
        ),
    );
    assert_eq!(
        decision,
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::NoEnvelope
        }
    );
}

// ── §10: no envelope of any kind covers a commitment ────────────────────────

/// The rule with no exception anywhere in the plan. Asserted against BOTH kinds
/// and against a grant that explicitly tried to list it.
#[test]
fn commitment_is_never_covered_by_any_kind_of_envelope() {
    let effective = bindable("zepto-mcp", &["merchant"]);
    let identities = vec!["merchant".to_string()];

    for kind in [
        EnvelopeKind::Standing,
        EnvelopeKind::ReviewedBatch {
            instances: vec![BatchInstance {
                recipient: "merchant".to_string(),
                content_ref: None,
            }],
        },
    ] {
        let mut state = standing(
            vec![ConsequenceClass::CommitmentOrTransaction],
            EnvelopeLimits::default(),
        );
        state.envelope.kind = kind.clone();

        let decision = resolve(
            &state,
            &facts(
                "act-1",
                &effective,
                ConsequenceClass::CommitmentOrTransaction,
                &identities,
            ),
        );
        assert_eq!(
            decision,
            EnvelopeDecision::NotCovered {
                reason: NotCoveredReason::ClassNotCoverable {
                    class: ConsequenceClass::CommitmentOrTransaction,
                    kind: kind.label().to_string(),
                }
            },
            "a {} envelope must never carry a commitment, even when granted one",
            kind.label()
        );
    }
}

/// And the store refuses to mint one in the first place, so an impossible
/// envelope cannot sit there looking authoritative.
#[test]
fn the_store_refuses_to_grant_what_can_never_be_covered() {
    let (store, scope, _dir) = store();

    let err = store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "buy things".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::CommitmentOrTransaction],
                limits: EnvelopeLimits::default(),
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect_err("granting a commitment envelope must fail");
    assert!(err.to_string().contains("may never cover"));

    // A standing envelope over disclosure is equally impossible.
    let err = store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "share the data room".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::ConfidentialDisclosure],
                limits: EnvelopeLimits::default(),
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect_err("a standing envelope over disclosure must fail");
    assert!(err.to_string().contains("may never cover"));
}

// ── §10 / §4A: every decision is made on the canonical effective action ─────

/// The §4A property, and the reason envelopes are not decoration today: an act
/// that accepts an escape hatch is never covered, however permissive the
/// envelope. Checked before class and limits, so no configuration reaches a
/// `Covered` verdict for one.
#[test]
fn an_act_with_an_escape_hatch_is_never_covered() {
    let mut effective = bindable("agentmail-send", &["a@example.com"]);
    effective.escape_hatches = vec!["extra_args".to_string()];
    let identities = vec!["a@example.com".to_string()];

    let state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    let decision = resolve(
        &state,
        &facts(
            "act-1",
            &effective,
            ConsequenceClass::BoundedCommunication,
            &identities,
        ),
    );

    assert_eq!(
        decision,
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::ActionNotBindable {
                escape_hatches: vec!["extra_args".to_string()]
            }
        },
        "authorising this would authorise a description of the act, not the act"
    );
}

// ── §10: a reviewed batch covers only its enumerated instances ──────────────

/// A batch is exhausted by its own list. A target the owner never reviewed is
/// never covered — which is the only reason a batch may carry disclosure and
/// submission at all.
#[test]
fn a_reviewed_batch_covers_only_what_the_owner_saw() {
    let identities = vec![
        "seen@example.com".to_string(),
        "new@example.com".to_string(),
    ];
    let mut state = standing(
        vec![ConsequenceClass::SubmissionOrPublication],
        EnvelopeLimits::default(),
    );
    state.envelope.kind = EnvelopeKind::ReviewedBatch {
        instances: vec![BatchInstance {
            recipient: "seen@example.com".to_string(),
            content_ref: None,
        }],
    };

    let reviewed = bindable("submit", &["seen@example.com"]);
    assert!(
        resolve(
            &state,
            &facts(
                "act-1",
                &reviewed,
                ConsequenceClass::SubmissionOrPublication,
                &identities
            )
        )
        .is_covered(),
        "the instance the owner reviewed is covered"
    );

    let unreviewed = bindable("submit", &["new@example.com"]);
    assert_eq!(
        resolve(
            &state,
            &facts(
                "act-2",
                &unreviewed,
                ConsequenceClass::SubmissionOrPublication,
                &identities
            )
        ),
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::NotInReviewedBatch {
                recipient: "new@example.com".to_string()
            }
        },
        "nothing can be added to a batch after the owner reviewed it"
    );
}

/// One unreviewed recipient poisons the whole act. A send to a reviewed target
/// AND an unreviewed one is not partly covered.
#[test]
fn a_batch_act_is_refused_if_any_recipient_is_unreviewed() {
    let identities = vec![
        "seen@example.com".to_string(),
        "new@example.com".to_string(),
    ];
    let mut state = standing(
        vec![ConsequenceClass::SubmissionOrPublication],
        EnvelopeLimits::default(),
    );
    state.envelope.kind = EnvelopeKind::ReviewedBatch {
        instances: vec![BatchInstance {
            recipient: "seen@example.com".to_string(),
            content_ref: None,
        }],
    };

    let mixed = bindable("submit", &["seen@example.com", "new@example.com"]);
    assert!(!resolve(
        &state,
        &facts(
            "act-1",
            &mixed,
            ConsequenceClass::SubmissionOrPublication,
            &identities
        )
    )
    .is_covered());
}

// ── §10: predicates evaluate from data the agent did not author ─────────────

/// The boundary holds against the engagement's identity list, which comes from
/// the engagement store. A recipient the agent added is simply not on it.
#[test]
fn a_recipient_outside_the_engagement_is_not_covered() {
    let identities = vec!["known@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    state.envelope.boundary = vec![BoundaryPredicate::RecipientInEngagement {
        engagement_id: "eng-1".to_string(),
    }];

    let known = bindable("agentmail-send", &["known@example.com"]);
    assert!(resolve(
        &state,
        &facts(
            "act-1",
            &known,
            ConsequenceClass::BoundedCommunication,
            &identities
        )
    )
    .is_covered());

    let smuggled = bindable(
        "agentmail-send",
        &["known@example.com", "extra@example.com"],
    );
    let decision = resolve(
        &state,
        &facts(
            "act-2",
            &smuggled,
            ConsequenceClass::BoundedCommunication,
            &identities,
        ),
    );
    assert!(
        !decision.is_covered(),
        "one unknown recipient refuses the act"
    );
}

/// An envelope bound to one engagement is not satisfied by an act on another,
/// even when that act's recipients happen to appear in the named engagement.
#[test]
fn an_envelope_bound_to_one_engagement_does_not_cover_another() {
    let identities = vec!["known@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    state.envelope.boundary = vec![BoundaryPredicate::RecipientInEngagement {
        engagement_id: "eng-1".to_string(),
    }];

    let effective = bindable("agentmail-send", &["known@example.com"]);
    let mut elsewhere = facts(
        "act-1",
        &effective,
        ConsequenceClass::BoundedCommunication,
        &identities,
    );
    elsewhere.engagement_id = Some("eng-2");

    assert!(!resolve(&state, &elsewhere).is_covered());
}

/// An act naming nobody must not satisfy a boundary built entirely from who it
/// reaches. Vacuous truth is how an unaddressed act slips through.
#[test]
fn an_act_with_no_recipients_does_not_vacuously_satisfy_the_boundary() {
    let identities = vec!["known@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    state.envelope.boundary = vec![BoundaryPredicate::RecipientInEngagement {
        engagement_id: "eng-1".to_string(),
    }];

    let unaddressed = bindable("agentmail-send", &[]);
    assert!(!resolve(
        &state,
        &facts(
            "act-1",
            &unaddressed,
            ConsequenceClass::BoundedCommunication,
            &identities
        )
    )
    .is_covered());
}

/// A capability outside the granted set is refused even when everything else
/// about the act is fine.
#[test]
fn a_capability_outside_the_granted_set_is_refused() {
    let identities = vec!["a@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    state.envelope.boundary = vec![BoundaryPredicate::CapabilityInSet {
        capabilities: vec!["agentmail-send".to_string()],
    }];

    let other = bindable("kapso-whatsapp-send", &["a@example.com"]);
    let decision = resolve(
        &state,
        &facts(
            "act-1",
            &other,
            ConsequenceClass::BoundedCommunication,
            &identities,
        ),
    );
    assert!(matches!(
        decision,
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::PredicateFailed { .. }
        }
    ));
}

// ── §10: expired, exhausted or revoked asks, and never silently proceeds ────

/// Every degradation path. All three ask; none proceeds.
#[test]
fn expiry_exhaustion_and_revocation_all_ask() {
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let identities = vec!["a@example.com".to_string()];
    let act = facts(
        "act-new",
        &effective,
        ConsequenceClass::BoundedCommunication,
        &identities,
    );

    let mut expired = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            expires_at: Some(now() - Duration::minutes(1)),
            ..EnvelopeLimits::default()
        },
    );
    assert_eq!(
        resolve(&expired, &act),
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::Expired
        }
    );

    // Expiry is inclusive: an envelope expiring exactly now is spent.
    expired.envelope.limits.expires_at = Some(now());
    assert!(!resolve(&expired, &act).is_covered());

    let mut revoked = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    revoked.envelope.revoked_at = Some(now() - Duration::minutes(1));
    assert_eq!(
        resolve(&revoked, &act),
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::Revoked
        }
    );

    let mut exhausted = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            max_acts: Some(1),
            ..EnvelopeLimits::default()
        },
    );
    exhausted.consumed = vec![ConsumptionEntry {
        act_ref: "act-earlier".to_string(),
        at: now() - Duration::minutes(5),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    }];
    assert_eq!(
        resolve(&exhausted, &act),
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::LimitReached {
                limit: "max_acts".to_string()
            }
        }
    );
}

/// A cap is a ceiling on the total, not on the history: an envelope for two acts
/// authorises the second and refuses the third.
#[test]
fn a_cap_authorises_exactly_as_many_acts_as_it_names() {
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let identities = vec!["a@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            max_acts: Some(2),
            ..EnvelopeLimits::default()
        },
    );

    state.consumed = vec![ConsumptionEntry {
        act_ref: "act-1".to_string(),
        at: now(),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    }];
    assert!(resolve(
        &state,
        &facts(
            "act-2",
            &effective,
            ConsequenceClass::BoundedCommunication,
            &identities
        )
    )
    .is_covered());

    state.consumed.push(ConsumptionEntry {
        act_ref: "act-2".to_string(),
        at: now(),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    });
    assert!(!resolve(
        &state,
        &facts(
            "act-3",
            &effective,
            ConsequenceClass::BoundedCommunication,
            &identities
        )
    )
    .is_covered());
}

/// A retry of an act already debited must not be refused for a cap its own
/// earlier attempt filled — otherwise an idempotent resend of the last act under
/// an envelope would ask.
#[test]
fn a_retry_of_a_debited_act_is_not_refused_by_its_own_debit() {
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let identities = vec!["a@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            max_acts: Some(1),
            per_recipient_cap: Some(1),
            ..EnvelopeLimits::default()
        },
    );
    state.consumed = vec![ConsumptionEntry {
        act_ref: "act-1".to_string(),
        at: now(),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    }];

    assert!(
        resolve(
            &state,
            &facts(
                "act-1",
                &effective,
                ConsequenceClass::BoundedCommunication,
                &identities
            )
        )
        .is_covered(),
        "the same act replaying is the same act"
    );
    assert!(
        !resolve(
            &state,
            &facts(
                "act-2",
                &effective,
                ConsequenceClass::BoundedCommunication,
                &identities
            )
        )
        .is_covered(),
        "a DIFFERENT act still hits the cap"
    );
}

/// The per-recipient cap counts per recipient, not per act.
#[test]
fn the_per_recipient_cap_counts_per_recipient() {
    let identities = vec!["a@example.com".to_string(), "b@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            per_recipient_cap: Some(1),
            ..EnvelopeLimits::default()
        },
    );
    state.consumed = vec![ConsumptionEntry {
        act_ref: "act-1".to_string(),
        at: now(),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    }];

    let repeat = bindable("agentmail-send", &["a@example.com"]);
    assert!(!resolve(
        &state,
        &facts(
            "act-2",
            &repeat,
            ConsequenceClass::BoundedCommunication,
            &identities
        )
    )
    .is_covered());

    let fresh = bindable("agentmail-send", &["b@example.com"]);
    assert!(resolve(
        &state,
        &facts(
            "act-3",
            &fresh,
            ConsequenceClass::BoundedCommunication,
            &identities
        )
    )
    .is_covered());
}

// ── §7: every act under an envelope records which predicate matched ─────────

/// The audit property. A covered verdict names the envelope and every predicate
/// that matched, so "why did it send that" has an answer without reconstruction.
#[test]
fn a_covered_act_names_the_envelope_and_the_matching_predicates() {
    let identities = vec!["a@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    state.envelope.boundary = vec![
        BoundaryPredicate::RecipientInEngagement {
            engagement_id: "eng-1".to_string(),
        },
        BoundaryPredicate::CapabilityInSet {
            capabilities: vec!["agentmail-send".to_string()],
        },
        BoundaryPredicate::NoAttachmentOutsideLedger,
    ];

    let effective = bindable("agentmail-send", &["a@example.com"]);
    let decision = resolve(
        &state,
        &facts(
            "act-1",
            &effective,
            ConsequenceClass::BoundedCommunication,
            &identities,
        ),
    );

    match decision {
        EnvelopeDecision::Covered {
            envelope_id,
            matched_predicates,
            ..
        } => {
            assert_eq!(envelope_id, "env-test");
            assert_eq!(
                matched_predicates,
                vec![
                    "recipient_in_engagement".to_string(),
                    "capability_in_set".to_string(),
                    "no_attachment_outside_ledger".to_string(),
                ]
            );
        },
        other => panic!("expected coverage, got {other:?}"),
    }
}

/// When nothing covers, the reason reported is the most specific one seen. An
/// owner told "no envelope" when theirs is merely exhausted goes looking in the
/// wrong place.
#[test]
fn the_reported_refusal_is_the_most_specific_one() {
    let identities = vec!["a@example.com".to_string()];
    let effective = bindable("agentmail-send", &["a@example.com"]);

    // One envelope does not grant the class at all; another grants it but is
    // spent. The second is the useful answer.
    let ungranted = standing(
        vec![ConsequenceClass::SubmissionOrPublication],
        EnvelopeLimits::default(),
    );
    let mut spent = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            max_acts: Some(1),
            ..EnvelopeLimits::default()
        },
    );
    spent.consumed = vec![ConsumptionEntry {
        act_ref: "act-0".to_string(),
        at: now(),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    }];

    let decision = resolve_any(
        &[ungranted, spent],
        &facts(
            "act-1",
            &effective,
            ConsequenceClass::BoundedCommunication,
            &identities,
        ),
    );
    assert_eq!(
        decision,
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::LimitReached {
                limit: "max_acts".to_string()
            }
        }
    );
}

/// An unbindable act is a fact about the ACT, so it must outrank every
/// envelope-specific reason. Reporting `Expired` here would send the owner off
/// to renew an envelope that still would not cover it.
#[test]
fn an_unbindable_act_reports_itself_not_the_envelopes_state() {
    let identities = vec!["a@example.com".to_string()];
    let mut effective = bindable("agentmail-send", &["a@example.com"]);
    effective.escape_hatches = vec!["extra_args".to_string()];

    let expired = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            expires_at: Some(now() - Duration::minutes(1)),
            ..EnvelopeLimits::default()
        },
    );
    let mut spent = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            max_acts: Some(1),
            ..EnvelopeLimits::default()
        },
    );
    spent.consumed = vec![ConsumptionEntry {
        act_ref: "act-0".to_string(),
        at: now(),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    }];

    assert_eq!(
        resolve_any(
            &[expired, spent],
            &facts(
                "act-1",
                &effective,
                ConsequenceClass::BoundedCommunication,
                &identities
            )
        ),
        EnvelopeDecision::NotCovered {
            reason: NotCoveredReason::ActionNotBindable {
                escape_hatches: vec!["extra_args".to_string()]
            }
        },
        "no envelope fix changes an unbindable act, so that is what to report"
    );
}

/// The scoping bridge: an act serving a work context derives its envelope scope
/// from it, so "the work this belongs to" has one definition rather than two.
/// This is the gap the shadow log reported as its own finding.
#[test]
fn a_work_context_scopes_the_envelopes_its_acts_resolve_against() {
    use crate::magician_v2::work_context::WorkContextKind;

    assert_eq!(
        EnvelopeScope::from(&WorkContextKind::Program("fundraising".to_string())),
        EnvelopeScope::Program("fundraising".to_string())
    );
    assert_eq!(
        EnvelopeScope::from(&WorkContextKind::Engagement("eng-1".to_string())),
        EnvelopeScope::Engagement("eng-1".to_string())
    );
    // And the derived scope keys identically to a directly-built one, so both
    // find the same stored envelopes.
    assert_eq!(
        EnvelopeScope::from(&WorkContextKind::Engagement("eng-1".to_string())).as_key(),
        EnvelopeScope::Engagement("eng-1".to_string()).as_key()
    );
}

// ── The dispatch gate (phase 3) ─────────────────────────────────────────────

use super::gate::{reason_label, shadow_log_line, DispatchContext, EnvelopeGate, GateOutcome};

fn gate() -> (
    EnvelopeGate,
    ApprovalEnvelopeStore,
    EnvelopeStoreScope,
    tempfile::TempDir,
) {
    let (store, scope, dir) = store();
    (
        EnvelopeGate::new(store.clone(), scope.clone()),
        store,
        scope,
        dir,
    )
}

fn granted(store: &ApprovalEnvelopeStore, scope: &EnvelopeStoreScope, max_acts: u32) -> String {
    store
        .grant(
            scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "correspond".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::BoundedCommunication],
                limits: EnvelopeLimits {
                    max_acts: Some(max_acts),
                    ..expiring()
                },
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect("grant")
        .envelope_id
}

fn ctx<'a>(
    act_ref: &'a str,
    effective: &'a EffectiveAction,
    ids: &'a [String],
) -> DispatchContext<'a> {
    DispatchContext {
        act_ref,
        effective,
        consequence_class: ConsequenceClass::BoundedCommunication,
        envelope_scope: Some(EnvelopeScope::Engagement("eng-1".to_string())),
        engagement_id: Some("eng-1"),
        engagement_identities: ids,
        attachments_outside_ledger: Some(0),
        value_micros: None,
        now: now(),
    }
}

/// Mode `Off` resolves nothing and writes nothing — the plan's first acceptance
/// criterion, asserted by the ledger being untouched rather than by the return
/// value alone.
#[test]
fn mode_off_evaluates_nothing_and_writes_nothing() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 5);
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    let outcome = gate
        .evaluate(EnvelopeMode::Off, &ctx("act-1", &effective, &ids))
        .expect("evaluate");
    assert_eq!(outcome, GateOutcome::NotEvaluated);
    assert!(!outcome.authorises());

    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 0, "Off must not debit");
}

/// Shadow resolves, reports, and **never debits**. An act that was still asked
/// about did not consume an envelope; debiting here would spend caps on acts the
/// owner approved by hand, and phase 3 exists to compare the two.
#[test]
fn shadow_reports_a_decision_and_never_debits() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 5);
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    let outcome = gate
        .evaluate(EnvelopeMode::Shadow, &ctx("act-1", &effective, &ids))
        .expect("evaluate");

    match &outcome {
        GateOutcome::Shadow { decision } => assert!(
            decision.is_covered(),
            "this act is inside the envelope, which is what shadow should report"
        ),
        other => panic!("expected a shadow decision, got {other:?}"),
    }
    assert!(
        !outcome.authorises(),
        "shadow observes; it must never let an act through"
    );

    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 0, "shadow must not debit");
}

/// Enforcing debits, and the debit is idempotent, so a retry of one act cannot
/// consume an envelope twice.
#[test]
fn enforcing_authorises_and_debits_once_per_act() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 5);
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    let outcome = gate
        .evaluate(EnvelopeMode::Enforcing, &ctx("act-1", &effective, &ids))
        .expect("evaluate");
    assert!(outcome.authorises());

    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 1);

    // The same act again — a retry, not a second send.
    let repeat = gate
        .evaluate(EnvelopeMode::Enforcing, &ctx("act-1", &effective, &ids))
        .expect("evaluate");
    assert!(repeat.authorises());
    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 1, "one act, one debit");
}

/// An act nobody could place under a scope is an act no envelope could cover.
/// Enforcing refuses it; shadow still reports, because "nothing to apply" is
/// exactly what phase 3 needs to learn.
#[test]
fn an_unscoped_act_is_refused_but_still_reported_in_shadow() {
    let (gate, store, scope, _dir) = gate();
    let _ = granted(&store, &scope, 5);
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    let mut unscoped = ctx("act-1", &effective, &ids);
    unscoped.envelope_scope = None;

    assert_eq!(
        gate.evaluate(EnvelopeMode::Enforcing, &unscoped)
            .expect("evaluate"),
        GateOutcome::Refused {
            reason: NotCoveredReason::NoEnvelope
        }
    );
    assert!(matches!(
        gate.evaluate(EnvelopeMode::Shadow, &unscoped)
            .expect("evaluate"),
        GateOutcome::Shadow { .. }
    ));
}

/// Enforcing refuses an act outside the envelope, and the refusal does not
/// debit — a refused act consumed nothing.
#[test]
fn a_refused_act_does_not_consume_the_envelope() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 5);
    // Unbindable: the act carries an escape hatch.
    let mut effective = bindable("agentmail-send", &["a@example.com"]);
    effective.escape_hatches = vec!["extra_args".to_string()];
    let ids = vec!["a@example.com".to_string()];

    let outcome = gate
        .evaluate(EnvelopeMode::Enforcing, &ctx("act-1", &effective, &ids))
        .expect("evaluate");
    assert!(matches!(
        outcome,
        GateOutcome::Refused {
            reason: NotCoveredReason::ActionNotBindable { .. }
        }
    ));

    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 0);
}

/// The cap is honoured through the gate, not merely by the resolver in
/// isolation: the debits the gate wrote are what later resolutions read.
#[test]
fn the_gate_exhausts_an_envelope_after_its_last_act() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 2);
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    for act in ["act-1", "act-2"] {
        assert!(gate
            .evaluate(EnvelopeMode::Enforcing, &ctx(act, &effective, &ids))
            .expect("evaluate")
            .authorises());
    }

    let third = gate
        .evaluate(EnvelopeMode::Enforcing, &ctx("act-3", &effective, &ids))
        .expect("evaluate");
    assert_eq!(
        third,
        GateOutcome::Refused {
            reason: NotCoveredReason::LimitReached {
                limit: "max_acts".to_string()
            }
        },
        "exhaustion degrades to asking"
    );

    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 2, "the refused act debited nothing");
}

/// The shadow line has to carry enough to compare against reality without
/// re-running anything.
#[test]
fn the_shadow_line_names_the_envelope_or_the_cause() {
    let covered = EnvelopeDecision::Covered {
        envelope_id: "env-1".to_string(),
        outcome: "correspond".to_string(),
        matched_predicates: vec!["capability_in_set".to_string()],
    };
    let line = shadow_log_line(&covered);
    assert!(line.contains("would_cover"));
    assert!(line.contains("env-1"));
    assert!(line.contains("capability_in_set"));

    let refused = EnvelopeDecision::NotCovered {
        reason: NotCoveredReason::LimitReached {
            limit: "max_acts".to_string(),
        },
    };
    assert_eq!(
        shadow_log_line(&refused),
        "would_ask reason=limit_reached[max_acts]"
    );

    // Every reason must produce a countable label rather than an empty string,
    // or a shadow log cannot be aggregated by cause.
    for reason in [
        NotCoveredReason::NoEnvelope,
        NotCoveredReason::NotEnforcing,
        NotCoveredReason::Revoked,
        NotCoveredReason::Expired,
        NotCoveredReason::ActionNotBindable {
            escape_hatches: vec!["extra_args".to_string()],
        },
        NotCoveredReason::ClassNotGranted {
            class: ConsequenceClass::BoundedCommunication,
        },
        NotCoveredReason::ClassNotCoverable {
            class: ConsequenceClass::CommitmentOrTransaction,
            kind: "standing".to_string(),
        },
        NotCoveredReason::PredicateFailed {
            predicate: "capability_in_set".to_string(),
            detail: String::new(),
        },
        NotCoveredReason::NotInReviewedBatch {
            recipient: "a@example.com".to_string(),
        },
        NotCoveredReason::UnidentifiedAct,
    ] {
        assert!(!reason_label(&reason).is_empty(), "{reason:?} has no label");
    }
}

/// An act with no stable id must never be authorised.
///
/// The ledger is idempotent **on `act_ref`**, so a blank one makes every
/// unidentified act share a single debit slot: the first is recorded, every
/// later one is treated as a replay of it, and an envelope for two acts
/// authorises an unbounded number. Refused before the store is even read.
#[test]
fn an_act_with_no_stable_id_is_never_authorised() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 2);
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    for blank in ["", "   "] {
        assert_eq!(
            gate.evaluate(EnvelopeMode::Enforcing, &ctx(blank, &effective, &ids))
                .expect("evaluate"),
            GateOutcome::Refused {
                reason: NotCoveredReason::UnidentifiedAct
            },
            "an act we cannot name cannot be debited idempotently"
        );
    }

    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 0);
}

/// Unknown attachment provenance is not "none outside the ledger".
///
/// A caller with no asset ledger to consult would otherwise satisfy the
/// predicate by having nothing to report — the same vacuous-truth shape as an
/// act naming no recipients passing a predicate about who it reaches.
#[test]
fn unknown_attachment_provenance_fails_the_predicate_closed() {
    let identities = vec!["a@example.com".to_string()];
    let mut state = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    state.envelope.boundary = vec![BoundaryPredicate::NoAttachmentOutsideLedger];
    let effective = bindable("agentmail-send", &["a@example.com"]);

    let mut known = facts(
        "act-1",
        &effective,
        ConsequenceClass::BoundedCommunication,
        &identities,
    );
    known.attachments_outside_ledger = Some(0);
    assert!(
        resolve(&state, &known).is_covered(),
        "nothing outside: covered"
    );

    let mut unknown = known.clone();
    unknown.attachments_outside_ledger = None;
    assert!(
        !resolve(&state, &unknown).is_covered(),
        "unknown must not read as none"
    );

    let mut some_outside = known.clone();
    some_outside.attachments_outside_ledger = Some(2);
    assert!(!resolve(&state, &some_outside).is_covered());
}

// ── The owner surface (phase 4) ─────────────────────────────────────────────

use super::owner_view::{EnvelopeStanding, Headroom, OwnerView};

/// The four questions §4 and §7 say an owner must be able to answer, in one
/// call: what did I authorise, how much is left, who was reached, and on what
/// grounds.
#[test]
fn the_owner_can_see_what_was_done_under_an_envelope() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 3);
    let view = OwnerView::new(store.clone(), scope.clone());
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    gate.evaluate(EnvelopeMode::Enforcing, &ctx("act-1", &effective, &ids))
        .expect("evaluate");

    let detail = view
        .detail(&id, now())
        .expect("detail")
        .expect("the envelope exists");

    assert_eq!(detail.summary.outcome, "correspond");
    assert_eq!(detail.summary.standing, EnvelopeStanding::Active);
    assert_eq!(detail.summary.acts.used, 1);
    assert_eq!(detail.summary.acts.remaining(), Some(2));
    assert_eq!(detail.summary.recipients_reached, vec!["a@example.com"]);

    assert_eq!(detail.consumed.len(), 1);
    assert_eq!(detail.consumed[0].act_ref, "act-1");
}

/// An unset cap is "no limit", never `0`. An owner shown "0 remaining" for an
/// uncapped envelope would revoke something that was working correctly.
#[test]
fn an_unset_cap_reads_as_no_limit_not_as_zero() {
    let uncapped = Headroom {
        used: 7,
        limit: None,
    };
    assert_eq!(uncapped.remaining(), None);
    assert!(!uncapped.is_spent());

    let spent = Headroom {
        used: 3,
        limit: Some(3),
    };
    assert_eq!(spent.remaining(), Some(0));
    assert!(spent.is_spent());

    // Over-consumption must not underflow into a huge number.
    let over = Headroom {
        used: 5,
        limit: Some(3),
    };
    assert_eq!(over.remaining(), Some(0));
}

/// Standing is derived, not stored, so it cannot disagree with what the resolver
/// would decide. Each state is reported by the reason that will not change by
/// waiting: revocation is a decision, expiry a fact about the clock, exhaustion
/// a fact about use.
#[test]
fn standing_is_derived_and_reports_the_reason_that_will_not_change() {
    let mut active = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            max_acts: Some(2),
            expires_at: Some(now() + Duration::days(1)),
            ..EnvelopeLimits::default()
        },
    );
    assert_eq!(
        super::owner_view::summarise(&active, now()).standing,
        EnvelopeStanding::Active
    );

    // Spent.
    active.consumed = vec![
        ConsumptionEntry {
            act_ref: "a".to_string(),
            at: now(),
            recipients: vec!["x".to_string()],
            matched_predicates: Vec::new(),
            value_micros: None,
        },
        ConsumptionEntry {
            act_ref: "b".to_string(),
            at: now(),
            recipients: vec!["y".to_string()],
            matched_predicates: Vec::new(),
            value_micros: None,
        },
    ];
    assert_eq!(
        super::owner_view::summarise(&active, now()).standing,
        EnvelopeStanding::Exhausted
    );

    // Expired outranks exhausted; revoked outranks both.
    let expired_at = now() + Duration::days(2);
    assert_eq!(
        super::owner_view::summarise(&active, expired_at).standing,
        EnvelopeStanding::Expired
    );
    active.envelope.revoked_at = Some(now());
    assert_eq!(
        super::owner_view::summarise(&active, expired_at).standing,
        EnvelopeStanding::Revoked
    );

    assert!(EnvelopeStanding::Active.authorises_anything());
    for dead in [
        EnvelopeStanding::Revoked,
        EnvelopeStanding::Expired,
        EnvelopeStanding::Exhausted,
    ] {
        assert!(
            !dead.authorises_anything(),
            "{dead:?} must not read as live"
        );
    }
}

/// A reviewed batch has no act cap, so without this it would read `Active`
/// forever — telling the owner a batch is live when nothing remains that it
/// could authorise.
#[test]
fn a_batch_is_exhausted_once_every_instance_has_been_reached() {
    let mut batch = standing(
        vec![ConsequenceClass::SubmissionOrPublication],
        EnvelopeLimits::default(),
    );
    batch.envelope.kind = EnvelopeKind::ReviewedBatch {
        instances: vec![
            BatchInstance {
                recipient: "a@example.com".to_string(),
                content_ref: None,
            },
            BatchInstance {
                recipient: "b@example.com".to_string(),
                content_ref: None,
            },
        ],
    };

    assert_eq!(
        super::owner_view::summarise(&batch, now()).standing,
        EnvelopeStanding::Active
    );

    batch.consumed = vec![ConsumptionEntry {
        act_ref: "act-1".to_string(),
        at: now(),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    }];
    assert_eq!(
        super::owner_view::summarise(&batch, now()).standing,
        EnvelopeStanding::Active,
        "one of two reached is not exhausted"
    );

    batch.consumed.push(ConsumptionEntry {
        act_ref: "act-2".to_string(),
        at: now(),
        recipients: vec!["b@example.com".to_string()],
        matched_predicates: Vec::new(),
        value_micros: None,
    });
    assert_eq!(
        super::owner_view::summarise(&batch, now()).standing,
        EnvelopeStanding::Exhausted
    );
}

/// Revoked and expired envelopes stay listed. An owner asking "what did this
/// thing do" after revoking it is the main reason to look, so filtering them
/// would remove the answer exactly when it is wanted.
#[test]
fn revoking_keeps_the_envelope_visible_with_its_history() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 3);
    let view = OwnerView::new(store.clone(), scope.clone());
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    gate.evaluate(EnvelopeMode::Enforcing, &ctx("act-1", &effective, &ids))
        .expect("evaluate");

    let revoked = view.revoke(&id, "owner", now()).expect("revoke");
    assert_eq!(revoked.standing, EnvelopeStanding::Revoked);
    assert_eq!(revoked.acts.used, 1, "history survives revocation");

    // Idempotent — a double-click must not produce an error to interpret.
    let again = view.revoke(&id, "owner", now()).expect("revoke twice");
    assert_eq!(again.standing, EnvelopeStanding::Revoked);

    let listed = view
        .list(&EnvelopeScope::Engagement("eng-1".to_string()), now())
        .expect("list");
    assert_eq!(listed.len(), 1, "a revoked envelope stays visible");
    assert_eq!(listed[0].envelope_id, id);

    // And it authorises nothing from here on.
    assert!(!gate
        .evaluate(EnvelopeMode::Enforcing, &ctx("act-2", &effective, &ids))
        .expect("evaluate")
        .authorises());
}

/// Listing is newest-grant-first and stable, so a surface does not reorder
/// between reads.
#[test]
fn envelopes_list_newest_first_and_deterministically() {
    let (_gate, store, scope, _dir) = gate();
    let request = |outcome: &str| GrantEnvelope {
        scope: EnvelopeScope::Engagement("eng-1".to_string()),
        outcome: outcome.to_string(),
        kind: EnvelopeKind::Standing,
        covers: vec![ConsequenceClass::BoundedCommunication],
        limits: expiring(),
        boundary: Vec::new(),
        granted_by: "owner".to_string(),
    };
    store
        .grant(&scope, &request("older"), now())
        .expect("older");
    store
        .grant(&scope, &request("newer"), now() + Duration::hours(1))
        .expect("newer");

    let view = OwnerView::new(store.clone(), scope.clone());
    let listed = view
        .list(&EnvelopeScope::Engagement("eng-1".to_string()), now())
        .expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].outcome, "newer");
    assert_eq!(listed[1].outcome, "older");
}

/// An envelope that does not exist is `None`, not an error: asking about an id
/// that was never granted is a normal thing for a surface to do.
#[test]
fn an_unknown_envelope_is_absent_rather_than_an_error() {
    let (_gate, store, scope, _dir) = gate();
    let view = OwnerView::new(store, scope);
    assert!(view.detail("env-nope", now()).expect("detail").is_none());
}

/// Granting through the owner surface returns the same shape a listing does,
/// and defers every rule to the store — a second opinion here could disagree,
/// and which one applied would depend on the route a caller took.
#[test]
fn the_owner_can_grant_and_gets_back_the_same_shape_as_a_listing() {
    let (_gate, store, scope, _dir) = gate();
    let view = OwnerView::new(store.clone(), scope.clone());

    let summary = view
        .grant(
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "approach accelerators".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::BoundedCommunication],
                limits: EnvelopeLimits {
                    max_acts: Some(4),
                    ..expiring()
                },
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect("grant");

    assert_eq!(summary.standing, EnvelopeStanding::Active);
    assert_eq!(summary.acts.remaining(), Some(4));
    assert_eq!(summary.outcome, "approach accelerators");

    let listed = view
        .list(&EnvelopeScope::Engagement("eng-1".to_string()), now())
        .expect("list");
    assert_eq!(listed, vec![summary], "grant and list must agree exactly");

    // The store's rules still apply through this door.
    let refused = view.grant(
        &GrantEnvelope {
            scope: EnvelopeScope::Engagement("eng-1".to_string()),
            outcome: "buy things".to_string(),
            kind: EnvelopeKind::Standing,
            covers: vec![ConsequenceClass::CommitmentOrTransaction],
            limits: expiring(),
            boundary: Vec::new(),
            granted_by: "owner".to_string(),
        },
        now(),
    );
    assert!(
        refused.is_err(),
        "a commitment envelope must be refused whichever door it arrives through"
    );
}

// ── Generalisation past outward work (phase 5) ──────────────────────────────

use super::waiver::{resolve_approval_waiver, ApprovalContext, ApprovalWaiver};

fn approval_ctx<'a>(
    act_ref: &'a str,
    effective: &'a EffectiveAction,
    ids: &'a [String],
) -> ApprovalContext<'a> {
    ApprovalContext {
        act_ref,
        effective,
        envelope_scope: Some(EnvelopeScope::Engagement("eng-1".to_string())),
        engagement_id: Some("eng-1"),
        engagement_identities: ids,
        attachments_outside_ledger: Some(0),
        value_micros: None,
        now: now(),
    }
}

/// The generalisation working: an ordinary approval prompt for bounded
/// communication is waived by a standing envelope, and names it.
#[test]
fn a_covered_approval_is_waived_and_names_its_envelope() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 3);
    let effective = bindable("gmail", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    let waiver = resolve_approval_waiver(
        &gate,
        EnvelopeMode::Enforcing,
        "gmail",
        "send",
        approval_ctx("act-1", &effective, &ids),
    )
    .expect("waiver");

    match &waiver {
        ApprovalWaiver::Waived { envelope_id, .. } => assert_eq!(envelope_id, &id),
        other => panic!("expected a waiver, got {other:?}"),
    }
    assert!(waiver.is_waived());
    assert!(waiver.audit_line().contains("waived by"));

    // §7: a waived prompt is as auditable as an asked one — the debit is there.
    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 1);
}

/// A commitment is never waived, by any envelope, in any mode — and this is the
/// layer where breaking that would actually remove a prompt.
#[test]
fn a_commitment_approval_is_never_waived() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 5);
    let effective = bindable("zepto-mcp", &["merchant"]);
    let ids = vec!["merchant".to_string()];

    for (capability, action) in [
        ("zepto-mcp", "create_order"),
        ("swiggy-mcp", "place_food_order"),
        ("swiggy-mcp", "book_table"),
    ] {
        let waiver = resolve_approval_waiver(
            &gate,
            EnvelopeMode::Enforcing,
            capability,
            action,
            approval_ctx("act-1", &effective, &ids),
        )
        .expect("waiver");
        assert!(
            !waiver.is_waived(),
            "{capability}/{action} moves money and must always ask"
        );
    }

    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 0, "a refused waiver consumes nothing");
}

/// An act nobody classified is treated as a commitment, so generalising cannot
/// quietly waive prompts for acts no one reasoned about. This is the property
/// that makes "make it generic" safe.
#[test]
fn an_unclassified_gated_act_is_never_waived() {
    let (gate, store, scope, _dir) = gate();
    let _ = granted(&store, &scope, 5);
    let effective = bindable("some-new-tool", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    let waiver = resolve_approval_waiver(
        &gate,
        EnvelopeMode::Enforcing,
        "some-new-tool",
        "do_something",
        approval_ctx("act-1", &effective, &ids),
    )
    .expect("waiver");
    assert!(
        !waiver.is_waived(),
        "an act behind approval that nobody classified must keep asking"
    );
}

/// Shadow never waives, whatever it would have decided — and the reason it
/// reports is that the mode is not enforcing, which is the truthful answer to
/// "why was I asked".
#[test]
fn shadow_never_waives_a_prompt() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 3);
    let effective = bindable("gmail", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    let waiver = resolve_approval_waiver(
        &gate,
        EnvelopeMode::Shadow,
        "gmail",
        "send",
        approval_ctx("act-1", &effective, &ids),
    )
    .expect("waiver");

    assert_eq!(
        waiver,
        ApprovalWaiver::Ask {
            reason: Some(NotCoveredReason::NotEnforcing)
        }
    );
    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(
        state.acts_used(),
        0,
        "shadow must not debit through this path either"
    );
}

/// Mode off asks with no reason — distinct from "an envelope declined", because
/// an owner surface needs to tell "envelopes are not on" from "yours did not
/// cover this".
#[test]
fn mode_off_asks_without_a_reason() {
    let (gate, store, scope, _dir) = gate();
    let _ = granted(&store, &scope, 3);
    let effective = bindable("gmail", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    let waiver = resolve_approval_waiver(
        &gate,
        EnvelopeMode::Off,
        "gmail",
        "send",
        approval_ctx("act-1", &effective, &ids),
    )
    .expect("waiver");
    assert_eq!(waiver, ApprovalWaiver::Ask { reason: None });
    assert_eq!(waiver.audit_line(), "asked (envelopes off)");
}

/// A preview must never consume. A surface calling the committing path to decide
/// what to render would spend an act per render — the quietest possible way to
/// exhaust an envelope without performing a single act.
#[test]
fn previewing_a_waiver_never_consumes_the_envelope() {
    let (gate, store, scope, _dir) = gate();
    let id = granted(&store, &scope, 2);
    let effective = bindable("gmail", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    for _ in 0..5 {
        let preview = super::waiver::preview_approval_waiver(
            &gate,
            EnvelopeMode::Enforcing,
            "gmail",
            "send",
            approval_ctx("act-1", &effective, &ids),
        )
        .expect("preview");
        assert!(
            !preview.is_waived(),
            "a preview reports, it does not authorise"
        );
    }

    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 0, "five previews spent nothing");

    // And the committing path still works afterwards, with full headroom.
    assert!(resolve_approval_waiver(
        &gate,
        EnvelopeMode::Enforcing,
        "gmail",
        "send",
        approval_ctx("act-1", &effective, &ids),
    )
    .expect("commit")
    .is_waived());
    let state = store.load(&scope, &id).expect("load").expect("envelope");
    assert_eq!(state.acts_used(), 1);
}

/// A preview when the feature is off must not resolve at all, or a surface would
/// show coverage that could never apply.
#[test]
fn a_preview_with_envelopes_off_reports_nothing() {
    let (gate, store, scope, _dir) = gate();
    let _ = granted(&store, &scope, 2);
    let effective = bindable("gmail", &["a@example.com"]);
    let ids = vec!["a@example.com".to_string()];

    assert_eq!(
        super::waiver::preview_approval_waiver(
            &gate,
            EnvelopeMode::Off,
            "gmail",
            "send",
            approval_ctx("act-1", &effective, &ids),
        )
        .expect("preview"),
        ApprovalWaiver::Ask { reason: None }
    );
}

/// Every outcome produces an audit line. A prompt that vanished without a record
/// is what would make envelopes untrustworthy.
#[test]
fn every_waiver_outcome_is_auditable() {
    for waiver in [
        ApprovalWaiver::Ask { reason: None },
        ApprovalWaiver::Ask {
            reason: Some(NotCoveredReason::Expired),
        },
        ApprovalWaiver::Waived {
            envelope_id: "env-1".to_string(),
            outcome: "correspond".to_string(),
            matched_predicates: vec!["capability_in_set".to_string()],
        },
    ] {
        assert!(
            !waiver.audit_line().is_empty(),
            "{waiver:?} has no audit line"
        );
    }
}

/// Which envelope pays for an act must not depend on the order they were
/// granted in — and spending the one about to lapse first keeps headroom from
/// being stranded in an envelope that then expires unused.
#[test]
fn the_soonest_expiring_covering_envelope_is_the_one_spent() {
    let identities = vec!["a@example.com".to_string()];
    let effective = bindable("agentmail-send", &["a@example.com"]);

    let mut soon = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            expires_at: Some(now() + Duration::hours(1)),
            ..EnvelopeLimits::default()
        },
    );
    soon.envelope.envelope_id = "env-soon".to_string();

    let mut later = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits {
            expires_at: Some(now() + Duration::days(30)),
            ..EnvelopeLimits::default()
        },
    );
    later.envelope.envelope_id = "env-later".to_string();

    let act = facts(
        "act-1",
        &effective,
        ConsequenceClass::BoundedCommunication,
        &identities,
    );

    // Both orderings must pick the same envelope.
    for states in [
        vec![soon.clone(), later.clone()],
        vec![later.clone(), soon.clone()],
    ] {
        assert_eq!(
            resolve_any(&states, &act).envelope_id(),
            Some("env-soon"),
            "the envelope about to lapse is spent first, whatever order it is stored in"
        );
    }

    // A never-expiring envelope is spent last: it can always be used later.
    let mut never = standing(
        vec![ConsequenceClass::BoundedCommunication],
        EnvelopeLimits::default(),
    );
    never.envelope.envelope_id = "env-never".to_string();
    assert_eq!(
        resolve_any(&[never.clone(), later.clone()], &act).envelope_id(),
        Some("env-later")
    );
}

// ── The store ───────────────────────────────────────────────────────────────

/// Grant, debit, fold. The ledger is the log and the state is its fold.
#[test]
fn consumption_is_appended_and_folds_back() {
    let (store, scope, _dir) = store();
    let envelope = store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "correspond".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::BoundedCommunication],
                limits: EnvelopeLimits {
                    max_acts: Some(5),
                    ..expiring()
                },
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect("grant");

    let entry = ConsumptionEntry {
        act_ref: "act-1".to_string(),
        at: now(),
        recipients: vec!["a@example.com".to_string()],
        matched_predicates: vec!["capability_in_set".to_string()],
        value_micros: None,
    };
    let state = store
        .record_consumption(&scope, &envelope.envelope_id, &entry)
        .expect("debit");
    assert_eq!(state.acts_used(), 1);
    assert_eq!(state.acts_for_recipient("a@example.com"), 1);

    // Idempotent: the same act debits once.
    let state = store
        .record_consumption(&scope, &envelope.envelope_id, &entry)
        .expect("replay");
    assert_eq!(
        state.acts_used(),
        1,
        "a retry of one act must not consume an envelope twice"
    );
}

/// Revocation is a successor, not a rewrite: the consumption that happened
/// under the envelope is still there afterwards, which is exactly what an owner
/// needs to see.
#[test]
fn revoking_preserves_what_was_done_under_the_envelope() {
    let (store, scope, _dir) = store();
    let envelope = store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "correspond".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::BoundedCommunication],
                limits: expiring(),
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect("grant");

    store
        .record_consumption(
            &scope,
            &envelope.envelope_id,
            &ConsumptionEntry {
                act_ref: "act-1".to_string(),
                at: now(),
                recipients: vec!["a@example.com".to_string()],
                matched_predicates: Vec::new(),
                value_micros: None,
            },
        )
        .expect("debit");

    let revoked = store
        .revoke(&scope, &envelope.envelope_id, "owner", now())
        .expect("revoke");
    assert!(revoked.envelope.is_revoked());
    assert_eq!(
        revoked.acts_used(),
        1,
        "what was done under the envelope survives revoking it"
    );

    // And it takes effect on the next act, with nothing to clean up.
    let effective = bindable("agentmail-send", &["a@example.com"]);
    let identities = vec!["a@example.com".to_string()];
    assert!(!resolve(
        &revoked,
        &facts(
            "act-2",
            &effective,
            ConsequenceClass::BoundedCommunication,
            &identities
        )
    )
    .is_covered());
}

/// Re-granting the same outcome after expiry produces a NEW envelope rather
/// than resuming the spent one. An expiry that could be undone by re-issuing the
/// same words would not be an expiry.
#[test]
fn re_granting_the_same_outcome_later_is_a_different_envelope() {
    let (store, scope, _dir) = store();
    let request = GrantEnvelope {
        scope: EnvelopeScope::Engagement("eng-1".to_string()),
        outcome: "correspond".to_string(),
        kind: EnvelopeKind::Standing,
        covers: vec![ConsequenceClass::BoundedCommunication],
        limits: expiring(),
        boundary: Vec::new(),
        granted_by: "owner".to_string(),
    };

    let first = store.grant(&scope, &request, now()).expect("first");
    let same = store.grant(&scope, &request, now()).expect("same instant");
    assert_eq!(
        first.envelope_id, same.envelope_id,
        "one grant at one instant is one envelope"
    );

    let later = store
        .grant(&scope, &request, now() + Duration::days(1))
        .expect("later");
    assert_ne!(first.envelope_id, later.envelope_id);

    let found = store
        .load_for_scope(&scope, &EnvelopeScope::Engagement("eng-1".to_string()))
        .expect("scope lookup");
    assert_eq!(found.len(), 2);
}

/// Scopes do not leak into each other.
#[test]
fn one_scopes_envelopes_are_not_anothers() {
    let (store, scope, _dir) = store();
    store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "correspond".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::BoundedCommunication],
                limits: expiring(),
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect("grant");

    assert!(store
        .load_for_scope(&scope, &EnvelopeScope::Engagement("eng-2".to_string()))
        .expect("other engagement")
        .is_empty());
    assert!(store
        .load_for_scope(
            &EnvelopeStoreScope::new("someone-else", "default"),
            &EnvelopeScope::Engagement("eng-1".to_string())
        )
        .expect("other principal")
        .is_empty());
}

/// An empty batch covers nothing, and must not read as covering everything.
#[test]
fn an_empty_reviewed_batch_is_refused_at_the_door() {
    let (store, scope, _dir) = store();
    let err = store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "submit to nobody".to_string(),
                kind: EnvelopeKind::ReviewedBatch {
                    instances: Vec::new(),
                },
                covers: vec![ConsequenceClass::SubmissionOrPublication],
                limits: EnvelopeLimits::default(),
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect_err("an empty batch must not be grantable");
    assert!(err.to_string().contains("exhausted by its own list"));
}

/// §9's "envelope sprawl" and "blanket yes" controls, as behaviour. A standing
/// envelope is consent to acts the owner has not seen; one that never lapses is
/// exactly the blanket yes the risk table exists to prevent.
///
/// A reviewed batch is exempt because it is self-limiting — exhausted by its own
/// list, with nothing addable to it.
#[test]
fn a_standing_envelope_must_expire_but_a_reviewed_batch_need_not() {
    let (store, scope, _dir) = store();

    let err = store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "correspond forever".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::BoundedCommunication],
                limits: EnvelopeLimits::default(),
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect_err("a standing envelope with no expiry must be refused");
    assert!(err.to_string().contains("must expire"));

    store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Engagement("eng-1".to_string()),
                outcome: "submit to these three".to_string(),
                kind: EnvelopeKind::ReviewedBatch {
                    instances: vec![BatchInstance {
                        recipient: "a@example.com".to_string(),
                        content_ref: None,
                    }],
                },
                covers: vec![ConsequenceClass::SubmissionOrPublication],
                limits: EnvelopeLimits::default(),
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect("a batch is bounded by its own list, so it need not expire");
}

/// An envelope over ungated work authorises nothing, so granting one is a
/// mistake worth catching rather than storing.
#[test]
fn an_envelope_over_ungated_work_is_refused() {
    let (store, scope, _dir) = store();
    let err = store
        .grant(
            &scope,
            &GrantEnvelope {
                scope: EnvelopeScope::Goal("goal-1".to_string()),
                outcome: "do local work".to_string(),
                kind: EnvelopeKind::Standing,
                covers: vec![ConsequenceClass::PrivateLocal],
                limits: EnvelopeLimits::default(),
                boundary: Vec::new(),
                granted_by: "owner".to_string(),
            },
            now(),
        )
        .expect_err("private/local needs no gate");
    assert!(err.to_string().contains("needs no gate"));
}
