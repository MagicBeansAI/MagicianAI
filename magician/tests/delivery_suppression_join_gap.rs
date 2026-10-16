//! The delivery → suppression ingestion loop, and why it does not exist.
//!
//! `docs/components/magician/suppression.md` and `.../delivery.md` both described
//! this loop in the present tense — *"`ingest(signals)` consumes the
//! `(identity, reason)` pairs `magician_v2::delivery` exposes"*. No code path in
//! the tree joins them, and the two ends do not fit: what delivery returns
//! cannot construct what suppression accepts.
//!
//! This file is the pin for that. It lives in `tests/` deliberately: the two
//! modules do not import each other by design, so the seam has no home inside
//! either of them, and building it inside one would be the coupling the design
//! refuses.

use chrono::{DateTime, Duration, TimeZone, Utc};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::delivery::{
    DeliveryLedger, DeliveryReceipt, DeliveryScope, DeliveryState, SuppressionCause,
};
use magician::magician_v2::suppression::{
    SuppressionEvidence, SuppressionReason, SuppressionRegister, SuppressionScope,
    SuppressionSignal,
};

const PRINCIPAL: &str = "anonymous";
const WORKSPACE: &str = "default";
const DEAD: &str = "dead@example.com";

fn at(hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, hour, 0, 0)
        .single()
        .expect("a real instant")
}

/// A ledger holding one hard bounce for `DEAD`, recorded at 12:00.
fn ledger_with_a_hard_bounce() -> (tempfile::TempDir, DeliveryLedger, DeliveryScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let ledger = DeliveryLedger::new(ArtifactV2Workspace::new(tmp.path()));
    let scope = DeliveryScope::new(PRINCIPAL, WORKSPACE);
    ledger
        .reconcile(
            &scope,
            "act-1",
            &DeliveryReceipt {
                provider: "agentmail".to_string(),
                provider_message_id: "pm-1".to_string(),
                identity: DEAD.to_string(),
                state: DeliveryState::Bounced { hard: true },
                observed_at: at(9),
                payload_ref: "payload://pm-1/bounced".to_string(),
            },
            at(12),
        )
        .expect("a hard bounce reconciles");
    (tmp, ledger, scope)
}

/// Pins: `suppression_signals` hands back an identity and a cause and NOTHING
/// ELSE — no evidence ref, no timestamp, no receipt id.
///
/// The exact failure: a doc (and therefore a reader) believing the ingestion
/// loop exists, when the value delivery produces is missing three of the four
/// fields `SuppressionRegister::ingest` requires. The `SignalRow` behind this
/// answer carries `receipt_id`, `act_ref`, `observed_at` and `recorded_at`; the
/// fold discards all four, so the caller has nothing an auditor could follow.
#[test]
fn a_delivery_signal_is_an_identity_and_a_cause_with_no_evidence_attached() {
    let (_tmp, ledger, scope) = ledger_with_a_hard_bounce();

    let signals = ledger
        .suppression_signals(&scope, at(12) - Duration::days(1))
        .expect("the signal log reads");

    assert_eq!(
        signals,
        vec![(DEAD.to_string(), SuppressionCause::HardBounce)],
        "delivery exposes exactly one pair for this bounce, and the pair is the whole payload: \
         anything an evidence ref could be built from was dropped by the fold"
    );
}

/// Pins: the register reports the evidence-free signal as unreadable.
///
/// A caller writing the loop today has `(identity, cause)` and nothing else, so
/// the `SuppressionEvidence` it must supply has to be invented. The register
/// reports that signal as unreadable and writes nothing for it; the point is
/// that the "already wired" ingestion loop cannot establish a suppression.
#[test]
fn ingest_reports_the_evidence_free_signal_a_delivery_pair_can_build() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let register = SuppressionRegister::global(ArtifactV2Workspace::new(tmp.path()));
    let scope = SuppressionScope::new(PRINCIPAL, WORKSPACE);

    let ingested = register
        .ingest(
            &scope,
            &[SuppressionSignal {
                identity: DEAD.to_string(),
                reason: SuppressionReason::HardBounce,
                // Everything a delivery pair can supply. Both evidence strings
                // are blank because the pair carries neither.
                evidence: SuppressionEvidence::new(at(12), "", ""),
            }],
            at(12),
        )
        .expect("an unreadable signal is accounted for without wedging the sweep");
    assert!(ingested.recorded.is_empty());
    assert_eq!(ingested.newly_recorded, 0);
    assert_eq!(ingested.unreadable, vec![DEAD.to_string()]);

    // And nothing was written for the unreadable signal.
    assert_eq!(
        register
            .is_suppressed(&scope, DEAD, at(13))
            .expect("an absent register reads as nobody suppressed, not as an error"),
        None,
        "the unreadable signal must have written nothing"
    );
}

/// Pins: the two vocabularies are not one, so the join needs a decision.
///
/// `delivery::SuppressionCause` has two variants; `suppression::SuppressionReason`
/// has five. A mapping exists in neither module — `SuppressionCause` is named
/// nowhere outside `delivery` — and neither module may import the other, which
/// is what makes "who owns the mapping" a design decision rather than a missing
/// function. The two do agree on their wire strings today, and that agreement is
/// a coincidence nothing enforces: this test is what would fail if one side
/// renamed a variant and left the other believing the loop still lined up.
#[test]
fn the_cause_and_reason_vocabularies_agree_only_by_string_and_only_on_two_of_five() {
    assert_eq!(SuppressionCause::HardBounce.as_str(), "hard_bounce");
    assert_eq!(SuppressionCause::Complaint.as_str(), "complaint");

    assert_eq!(SuppressionReason::HardBounce.as_str(), "hard_bounce");
    assert_eq!(SuppressionReason::Complaint.as_str(), "complaint");

    assert_eq!(
        SuppressionReason::ALL.len(),
        5,
        "the register carries five reasons; delivery can speak to two of them, so a mapping is \
         total in one direction only"
    );
    assert_eq!(
        SuppressionReason::ALL
            .iter()
            .filter(|reason| reason.requires_owner_act_to_lift())
            .map(|reason| reason.as_str())
            .collect::<Vec<_>>(),
        vec!["opt_out", "complaint"],
        "a complaint arriving from a provider is a consent decision no sweep may reverse, which \
         is why a mapping cannot be a mechanical string match"
    );
}

/// Pins: a fully-evidenced signal DOES record, so the gap is the evidence and
/// the mapping — not the register.
///
/// Without this, the two tests above would be consistent with `ingest` being
/// broken, and a reader could conclude the register is the thing that needs
/// fixing. It is not: given the four fields, it records and the screen then
/// blocks the address.
#[test]
fn the_same_signal_records_once_the_evidence_a_delivery_pair_lacks_is_supplied() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let register = SuppressionRegister::global(ArtifactV2Workspace::new(tmp.path()));
    let scope = SuppressionScope::new(PRINCIPAL, WORKSPACE);

    let recorded = register
        .ingest(
            &scope,
            &[SuppressionSignal {
                identity: DEAD.to_string(),
                reason: SuppressionReason::HardBounce,
                // The three facts a delivery pair would have to carry, and does
                // not: the receipt to follow, when the provider observed it, and
                // who filed it.
                evidence: SuppressionEvidence::new(at(9), "agentmail:pm-1", "delivery-sweep"),
            }],
            at(12),
        )
        .expect("a fully evidenced signal records")
        // `ingest` now answers with `Ingested`, which carries the same list as
        // `recorded` plus how many of them this call actually appended. The
        // assertions below are about the entries themselves, so take the list.
        .recorded;

    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].identity, DEAD);
    assert_eq!(recorded[0].reason, SuppressionReason::HardBounce);
    assert_eq!(recorded[0].evidence.evidence_ref, "agentmail:pm-1");
    assert_eq!(recorded[0].evidence.established_at, at(9));

    let in_force = register
        .is_suppressed(&scope, DEAD, at(13))
        .expect("the register reads")
        .expect("the address must be off limits after the bounce is ingested");
    assert_eq!(in_force.reason, SuppressionReason::HardBounce);
    assert_eq!(in_force.evidence.evidence_ref, "agentmail:pm-1");
}
