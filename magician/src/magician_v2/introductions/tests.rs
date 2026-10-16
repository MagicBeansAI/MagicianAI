//! §3.4 as behaviour: an introduction is a trust root AND a debt.

use chrono::TimeZone;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceKind;
use crate::magician_v2::counterparties::{
    AddIdentity, CreateCounterparty, IdentityKind, MintSource,
};

use super::*;

fn store() -> (tempfile::TempDir, CounterpartyStore, CounterpartyScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = CounterpartyStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, CounterpartyScope::new("alpha", "prod"))
}

/// `day` days after a fixed origin.
///
/// Days from an origin rather than a day-of-month: the second test needs a
/// point 35 days in, and `with_ymd_and_hms(2026, 8, 35, ..)` is not a date — it
/// returns `None` and the `unwrap` panics inside the test that is supposed to
/// be checking the window.
fn at(day: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 1, 9, 0, 0).unwrap() + Duration::days(day)
}

/// Record an organisation with one address, optionally an introduction.
fn org(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    name: &str,
    address: &str,
    introducer: Option<&str>,
    when: DateTime<Utc>,
) -> String {
    let record = store
        .record_counterparty(
            scope,
            &CreateCounterparty {
                display_name: name.to_string(),
                domain: None,
                stage: None,
                created_by: "owner".to_string(),
            },
            when,
        )
        .expect("organisation");
    let source = match introducer {
        Some(_) => MintSource::Introduced,
        None => MintSource::OwnerStated,
    };
    store
        .add_identity(
            scope,
            &AddIdentity {
                counterparty_id: record.counterparty_id.clone(),
                kind: IdentityKind::Email,
                value: address.to_string(),
                source,
                evidence_ref: format!("note-{name}"),
                recorded_by: "owner".to_string(),
                introduced_by: introducer.map(str::to_string),
            },
            when,
        )
        .expect("identity");
    record.counterparty_id
}

/// The graph groups by introducer, and an organisation nobody vouched for is
/// simply absent — not an empty group.
#[test]
fn the_referral_graph_names_who_vouched_for_whom() {
    let (_tmp, store, scope) = store();
    org(
        &store,
        &scope,
        "Acme",
        "cto@acme.test",
        Some("sarah@example.test"),
        at(1),
    );
    org(
        &store,
        &scope,
        "Globex",
        "vp@globex.test",
        Some("sarah@example.test"),
        at(2),
    );
    org(
        &store,
        &scope,
        "Initech",
        "hi@initech.test",
        Some("dave@example.test"),
        at(3),
    );
    org(&store, &scope, "Cold Inc", "sales@cold.test", None, at(4));

    let graph = referral_graph(&store, &scope).expect("graph");
    assert_eq!(
        graph.len(),
        3,
        "the cold organisation is not an introduction"
    );
    assert_eq!(graph.by_introducer.len(), 2);

    let sarah = graph
        .by_introducer
        .get("sarah@example.test")
        .expect("sarah introduced two");
    assert_eq!(
        sarah
            .iter()
            .map(|i| i.display_name.as_str())
            .collect::<Vec<_>>(),
        vec!["Acme", "Globex"],
        "ordered by when the introduction landed"
    );
    assert!(
        !graph.by_introducer.contains_key(""),
        "an organisation nobody vouched for must not create a blank introducer"
    );
}

/// The debt is measured from the MOST RECENT contact on any introduction, not
/// the oldest.
///
/// Somebody who introduced us in March and again last week does not need an
/// update about March, and a derivation reading the oldest would tell them so
/// every time they helped again — which is the exact behaviour that makes a
/// person stop introducing you.
#[test]
fn an_introducer_who_helped_again_is_not_owed_an_update_for_the_first_one() {
    let (_tmp, store, scope) = store();
    org(
        &store,
        &scope,
        "Old Co",
        "a@old.test",
        Some("sarah@example.test"),
        at(1),
    );
    org(
        &store,
        &scope,
        "New Co",
        "b@new.test",
        Some("sarah@example.test"),
        at(20),
    );

    let graph = referral_graph(&store, &scope).expect("graph");
    let policy = IntroducerPolicy::new(Duration::days(14)).expect("policy");

    // Ten days after the SECOND introduction: nineteen days after the first,
    // and nothing is owed.
    let owed = introducer_debts(&graph, policy, "cycle", at(30));
    assert!(owed.is_empty(), "the recent introduction is what counts");

    // Fifteen days after the second: now it is owed, and ONE row covers both.
    let owed = introducer_debts(&graph, policy, "cycle", at(35));
    assert_eq!(
        owed.len(),
        1,
        "one update per introducer, not per introduction"
    );
    assert_eq!(owed[0].covers.len(), 2);
    assert!(
        owed[0].obligation.what.contains("2 introductions"),
        "{}",
        owed[0].obligation.what
    );
    assert_eq!(owed[0].obligation.direction, ObligationDirection::OwedByUs);
    assert_eq!(
        owed[0].obligation.audience.kind,
        AudienceKind::Person,
        "an introducer is one named individual, not a bilateral engagement"
    );
    assert_eq!(
        owed[0].obligation.due_at,
        at(35),
        "the window already elapsed, so a future deadline would say the debt has not started"
    );
}

/// An introducer nobody has recorded is still owed what they are owed.
///
/// Whether we can REACH somebody is a different question from whether we owe
/// them an update, and the obligation register records what is owed rather than
/// how to deliver it. The audience is a PERSON keyed by the introducer, so
/// there is no relationship to resolve and none to guess at.
#[test]
fn an_introducer_the_register_has_never_heard_of_is_still_owed_an_update() {
    let (_tmp, store, scope) = store();
    org(
        &store,
        &scope,
        "Acme",
        "cto@acme.test",
        Some("stranger@example.test"),
        at(1),
    );

    let graph = referral_graph(&store, &scope).expect("graph");
    let policy = IntroducerPolicy::new(Duration::days(7)).expect("policy");
    let owed = introducer_debts(&graph, policy, "cycle", at(20));

    assert_eq!(owed.len(), 1);
    assert_eq!(owed[0].obligation.audience.kind, AudienceKind::Person);
    assert_eq!(
        owed[0].obligation.audience.id, "stranger@example.test",
        "keyed by the introducer, so two spellings share one log and nothing is guessed"
    );
}

/// A zero or negative window is refused rather than substituted.
///
/// It would owe an update the instant an introduction lands — a debt that was
/// never dischargeable, and a register that fills on its first pass.
#[test]
fn a_window_that_owes_immediately_is_refused() {
    assert!(IntroducerPolicy::new(Duration::zero()).is_err());
    assert!(IntroducerPolicy::new(Duration::seconds(-1)).is_err());
    assert!(IntroducerPolicy::new(Duration::seconds(1)).is_ok());
}

/// Two spellings of one introducer are one introducer, and one debt.
///
/// `add_identity` trims `introduced_by` and does not lowercase it, so the same
/// person recorded two ways arrives here as two. Owing them two updates for work
/// they did once is the behaviour this module exists to prevent, not produce.
#[test]
fn two_spellings_of_one_introducer_owe_one_update() {
    let (_tmp, store, scope) = store();
    org(
        &store,
        &scope,
        "Acme",
        "cto@acme.test",
        Some("Sarah@Example.test"),
        at(1),
    );
    org(
        &store,
        &scope,
        "Globex",
        "vp@globex.test",
        Some("  sarah@example.test "),
        at(2),
    );

    let graph = referral_graph(&store, &scope).expect("graph");
    assert_eq!(
        graph.by_introducer.len(),
        1,
        "one person, however they were spelled: {:?}",
        graph.by_introducer.keys().collect::<Vec<_>>()
    );
    assert_eq!(graph.len(), 2);

    let group = graph
        .by_introducer
        .get("sarah@example.test")
        .expect("keyed by the comparison form");
    assert!(
        group
            .iter()
            .all(|held| held.introducer == "Sarah@Example.test"),
        "the group shows the spelling its first introduction used"
    );

    let policy = IntroducerPolicy::new(Duration::days(7)).expect("policy");
    let owed = introducer_debts(&graph, policy, "cycle", at(20));
    assert_eq!(owed.len(), 1, "one update, not two");
    assert_eq!(owed[0].introducer, "Sarah@Example.test");
    assert_eq!(owed[0].covers.len(), 2);
}
