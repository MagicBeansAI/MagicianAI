//! Phase 3B as behaviour: research writes a guess down, and a guess grants
//! nothing.

use chrono::TimeZone;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::counterparties::{
    resolve_inbound, ChannelAddress, IdentityKind, InboundVerification,
};

// `AddIdentity`, `CreateCounterparty` and `MintSource` come through `super::*`.

use super::*;

fn fixture() -> (tempfile::TempDir, CounterpartyStore, CounterpartyScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = CounterpartyStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, CounterpartyScope::new("anonymous", "default"))
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 21, 9, 0, 0).unwrap()
}

fn sender(channel: &str, address: &str) -> PublicContactSenderKey {
    PublicContactSenderKey {
        workspace: "default".to_string(),
        channel_type: channel.to_string(),
        channel_address: address.to_string(),
    }
}

fn research(
    org: Option<&str>,
    confidence: PublicContactProfileConfidence,
) -> PublicContactResearchResult {
    PublicContactResearchResult {
        task_id: "task-7".to_string(),
        execution_id: None,
        primary_user_output_id: None,
        summary: None,
        excerpt: None,
        confidence,
        possible_identity: None,
        org: org.map(str::to_string),
        role: None,
        sources: Vec::new(),
        warnings: Vec::new(),
        suggested_memory_fields: Vec::new(),
        stored_at: 0,
    }
}

/// Research that concluded something writes an organisation and an address —
/// and the address confers **nothing**.
///
/// That last part is what makes writing a guess down safe. An unverified
/// identity answers `None` to `authority()`, so inbound routing still treats
/// the sender as a guest and no engagement lane opens on the strength of a
/// pattern somebody's research matched.
#[test]
fn a_confident_result_is_minted_unverified_and_grants_nothing() {
    let (_tmp, store, scope) = fixture();
    let recorded = record_research(
        &store,
        &scope,
        &sender("email", "Ada@Example.TEST"),
        &research(
            Some("Example Ltd"),
            PublicContactProfileConfidence::ResearchedHigh,
        ),
        now(),
    )
    .expect("record");

    let Recorded::Minted {
        counterparty,
        identity,
    } = recorded
    else {
        panic!("expected a mint, got {recorded:?}");
    };
    assert_eq!(
        identity, "ada@example.test",
        "stored in its comparison form"
    );

    // On file, and carrying no authority.
    let resolved = store
        .resolve(&scope, IdentityKind::Email, "ada@example.test")
        .expect("resolve")
        .expect("on file");
    assert_eq!(resolved, counterparty);
    assert!(
        store
            .resolve_verified(&scope, IdentityKind::Email, "ada@example.test")
            .expect("resolve")
            .is_none(),
        "a research guess is never verified, so nothing may be granted on it"
    );

    // And the inbound path agrees: identified, but not authoritative.
    let identified = resolve_inbound(
        &store,
        &scope,
        "email",
        &ChannelAddress::new(IdentityKind::Email, "ada@example.test"),
        InboundVerification::from_boundary(true),
        now(),
    )
    .expect("identify");
    assert!(
        identified.authority().is_none(),
        "a proved channel plus an unproved address is still not authority"
    );
}

/// Re-running research over the same sender resumes, and does not error.
///
/// `evidence_ref` carries the research task id, so a second run arrives with
/// DIFFERENT provenance — and the register refuses a changed payload rather
/// than silently keeping the first, because provenance is what an owner reads
/// before trusting an address. That refusal is right and general. What would be
/// wrong is letting the ordinary case of research running twice surface as an
/// error, so the address is resolved before anything is written.
#[test]
fn re_running_research_resumes_rather_than_colliding_on_provenance() {
    let (_tmp, store, scope) = fixture();
    let first = record_research(
        &store,
        &scope,
        &sender("email", "ada@example.test"),
        &research(
            Some("Example Ltd"),
            PublicContactProfileConfidence::ResearchedMedium,
        ),
        now(),
    )
    .expect("first");
    // A DIFFERENT task id, which is what a real second run carries.
    let mut rerun = research(
        Some("Example Ltd"),
        PublicContactProfileConfidence::ResearchedMedium,
    );
    rerun.task_id = "task-9".to_string();
    let second = record_research(
        &store,
        &scope,
        &sender("email", "ada@example.test"),
        &rerun,
        now(),
    )
    .expect("a second run must not be an error");

    let (
        Recorded::Minted {
            counterparty: first_cp,
            identity: first_id,
        },
        Recorded::AlreadyOnFile {
            counterparty: second_cp,
            identity: second_id,
        },
    ) = (first, second)
    else {
        panic!("first run mints, second finds it already on file");
    };
    assert_eq!(first_cp, second_cp);
    assert_eq!(first_id, second_id);
    assert_eq!(store.list(&scope).expect("list").len(), 1);
    assert_eq!(
        store
            .identities_for(&scope, first_cp.as_str())
            .expect("identities")
            .len(),
        1,
        "and the address was not written twice"
    );
}

/// Four refusals, each named, and none of them an error.
///
/// A research run that concluded nothing is the ordinary case, and folding
/// these into one "no" would leave whoever is wondering why the register is not
/// filling unable to tell an adapter problem from a research problem.
#[test]
fn what_is_refused_is_refused_by_name() {
    let (_tmp, store, scope) = fixture();
    let cases: &[(
        &str,
        &str,
        Option<&str>,
        PublicContactProfileConfidence,
        &str,
    )] = &[
        // Research named no organisation — the one thing the register keys on.
        (
            "email",
            "ada@example.test",
            None,
            PublicContactProfileConfidence::ResearchedHigh,
            "no_organisation",
        ),
        // Research that did not convince itself does not get to fill an owner's
        // review queue.
        (
            "email",
            "ada@example.test",
            Some("Example Ltd"),
            PublicContactProfileConfidence::ResearchedLow,
            "below_confidence",
        ),
        (
            "email",
            "ada@example.test",
            Some("Example Ltd"),
            PublicContactProfileConfidence::Unknown,
            "below_confidence",
        ),
        // A channel whose transport fixes no address kind. A fact about the
        // adapter, not about the sender.
        (
            "slack",
            "U123",
            Some("Example Ltd"),
            PublicContactProfileConfidence::ResearchedHigh,
            "address_kind_not_fixed",
        ),
        // Nothing to file.
        (
            "email",
            "   ",
            Some("Example Ltd"),
            PublicContactProfileConfidence::ResearchedHigh,
            "no_address",
        ),
    ];

    for (channel, address, org, confidence, expected) in cases {
        let recorded = record_research(
            &store,
            &scope,
            &sender(channel, address),
            &research(*org, confidence.clone()),
            now(),
        )
        .expect("record");
        let Recorded::Skipped(why) = recorded else {
            panic!("`{expected}` should have been skipped");
        };
        assert_eq!(why.as_str(), *expected);
    }
    assert!(
        store.list(&scope).expect("list").is_empty(),
        "nothing refused wrote a row"
    );
}

/// An address already belonging to somebody else is reported, never forced.
///
/// Research says one organisation and the register says another. That is a real
/// disagreement an owner should look at — not a crash, and not a silent
/// overwrite. The register would refuse the write itself; catching it first
/// turns it into a named outcome and leaves no organisation row behind that
/// nothing points at.
#[test]
fn an_address_owned_by_another_organisation_is_reported_not_forced() {
    let (_tmp, store, scope) = fixture();
    // The owner already recorded this address under Globex.
    let globex = store
        .record_counterparty(
            &scope,
            &CreateCounterparty {
                display_name: "Globex".to_string(),
                domain: None,
                stage: None,
                created_by: "owner".to_string(),
            },
            now(),
        )
        .expect("org");
    store
        .add_identity(
            &scope,
            &AddIdentity {
                counterparty_id: globex.counterparty_id.clone(),
                kind: IdentityKind::Email,
                value: "ada@example.test".to_string(),
                source: MintSource::OwnerStated,
                evidence_ref: "the owner said so".to_string(),
                recorded_by: "owner".to_string(),
                introduced_by: None,
            },
            now(),
        )
        .expect("address");

    // Research concludes it is Example Ltd.
    let recorded = record_research(
        &store,
        &scope,
        &sender("email", "ada@example.test"),
        &research(
            Some("Example Ltd"),
            PublicContactProfileConfidence::ResearchedHigh,
        ),
        now(),
    )
    .expect("a disagreement is not an error");

    let Recorded::Skipped(NotRecorded::AddressBelongsToAnother { counterparty_id }) = recorded
    else {
        panic!("expected the disagreement to be reported, got {recorded:?}");
    };
    assert_eq!(counterparty_id, globex.counterparty_id);
    assert_eq!(
        store.list(&scope).expect("list").len(),
        1,
        "and no organisation row was left behind for the name research guessed"
    );
}

/// An owner's own statement is refused — and not for being weak.
///
/// An owner-reviewed identity is stronger than anything research produces. It is
/// refused because minting it as `ResearchInferred` would file an owner's
/// statement under research provenance, and provenance is what an owner reads
/// before trusting an address.
#[test]
fn an_owner_statement_is_refused_as_not_research_rather_than_as_weak() {
    let (_tmp, store, scope) = fixture();
    for confidence in [
        PublicContactProfileConfidence::OwnerReviewed,
        PublicContactProfileConfidence::UserClaimed,
    ] {
        let recorded = record_research(
            &store,
            &scope,
            &sender("email", "ada@example.test"),
            &research(Some("Example Ltd"), confidence),
            now(),
        )
        .expect("record");
        let Recorded::Skipped(why) = recorded else {
            panic!("an owner statement must not be minted as research");
        };
        assert_eq!(
            why.as_str(),
            "not_research",
            "not `below_confidence` — the reason matters to whoever reads it"
        );
    }
}
