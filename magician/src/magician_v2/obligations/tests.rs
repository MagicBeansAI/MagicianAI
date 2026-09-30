//! §6's contract, as behaviour.

use chrono::{Duration, TimeZone, Utc};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceRef;

use super::store::{obligation_id_for, ObligationScope, ObligationStore};
use super::types::{ObligationDirection, ObligationState, RecordObligation, Settlement};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn store() -> (tempfile::TempDir, ObligationStore, ObligationScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = ObligationStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, ObligationScope::new("anonymous", "default"))
}

fn eng() -> AudienceRef {
    AudienceRef::engagement("eng-1")
}

fn promise(
    what: &str,
    due: chrono::DateTime<Utc>,
    direction: ObligationDirection,
) -> RecordObligation {
    RecordObligation {
        audience: eng(),
        program_id: Some("prog-1".to_string()),
        what: what.to_string(),
        due_at: due,
        direction,
        created_by: "company-assistant".to_string(),
        source_act_ref: Some("act-1".to_string()),
    }
}

/// The plan's example: *"send me your metrics by Friday"*. Nothing enforced
/// Friday before this.
#[test]
fn an_obligation_lapses_on_its_own_clock() {
    let (_tmp, store, scope) = store();
    let friday = now() + Duration::days(3);
    let recorded = store
        .record(
            &scope,
            &promise("metrics", friday, ObligationDirection::OwedByUs),
            now(),
        )
        .expect("record");

    assert_eq!(recorded.state(now()), ObligationState::Open);
    assert_eq!(
        recorded.state(friday + Duration::hours(1)),
        ObligationState::Lapsed,
        "nothing has to run for this to be late"
    );
    // Lapsing is inclusive of the deadline itself.
    assert_eq!(recorded.state(friday), ObligationState::Lapsed);
    assert_eq!(
        recorded.overdue_by(friday + Duration::hours(2)),
        Some(Duration::hours(2))
    );
    assert!(recorded.overdue_by(now()).is_none());
}

/// A lapse means two different things, and one list that read alike for both
/// would train the owner to skim it.
#[test]
fn lapses_are_separable_by_direction() {
    let (_tmp, store, scope) = store();
    let past = now() - Duration::days(1);
    store
        .record(
            &scope,
            &promise("our deck", past, ObligationDirection::OwedByUs),
            now() - Duration::days(5),
        )
        .expect("ours");
    store
        .record(
            &scope,
            &promise("their term sheet", past, ObligationDirection::OwedToUs),
            now() - Duration::days(5),
        )
        .expect("theirs");

    let all = store.lapsed(&scope, &eng(), None, now()).expect("all");
    assert_eq!(all.len(), 2);

    let ours = store
        .lapsed(&scope, &eng(), Some(ObligationDirection::OwedByUs), now())
        .expect("ours");
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].what, "our deck");
    assert!(ours[0].direction.lapse_is_ours());

    let theirs = store
        .lapsed(&scope, &eng(), Some(ObligationDirection::OwedToUs), now())
        .expect("theirs");
    assert_eq!(theirs.len(), 1);
    assert!(!theirs[0].direction.lapse_is_ours());
}

/// The same promise noticed twice — a transcript reprocessed, a poller
/// re-reading a thread — is one obligation. Duplicates are what make people
/// ignore a to-do list.
#[test]
fn the_same_promise_noticed_twice_is_one_obligation() {
    let (_tmp, store, scope) = store();
    let friday = now() + Duration::days(3);

    let first = store
        .record(
            &scope,
            &promise("send metrics", friday, ObligationDirection::OwedByUs),
            now(),
        )
        .expect("first");
    // Re-extracted from a transcript: different spacing and case, same promise.
    let again = store
        .record(
            &scope,
            &promise("Send   Metrics", friday, ObligationDirection::OwedByUs),
            now() + Duration::hours(2),
        )
        .expect("re-extracted");

    assert_eq!(first.obligation_id, again.obligation_id);
    assert_eq!(
        again.created_at, first.created_at,
        "the replay must not move when the promise was made"
    );
    assert_eq!(store.for_audience(&scope, &eng()).expect("read").len(), 1);
}

/// A different deadline or a different direction is a different promise.
#[test]
fn deadline_and_direction_are_part_of_the_promise() {
    let (_tmp, store, scope) = store();
    let friday = now() + Duration::days(3);

    store
        .record(
            &scope,
            &promise("metrics", friday, ObligationDirection::OwedByUs),
            now(),
        )
        .expect("a");
    store
        .record(
            &scope,
            &promise(
                "metrics",
                friday + Duration::days(7),
                ObligationDirection::OwedByUs,
            ),
            now(),
        )
        .expect("b");
    store
        .record(
            &scope,
            &promise("metrics", friday, ObligationDirection::OwedToUs),
            now(),
        )
        .expect("c");

    assert_eq!(
        store.for_audience(&scope, &eng()).expect("read").len(),
        3,
        "re-promising for a later date is a new promise, not the old one moved"
    );
}

/// *"We did it"* and *"we no longer have to"* are different facts. A register
/// that conflated them would report a hit rate that was partly wishful.
#[test]
fn met_and_released_are_distinct_settlements() {
    let (_tmp, store, scope) = store();
    let due = now() + Duration::days(1);
    let met = store
        .record(
            &scope,
            &promise("deck", due, ObligationDirection::OwedByUs),
            now(),
        )
        .expect("a");
    let released = store
        .record(
            &scope,
            &promise("intro", due, ObligationDirection::OwedToUs),
            now(),
        )
        .expect("b");

    let met = store
        .settle(
            &scope,
            &eng(),
            &met.obligation_id,
            Settlement::Met { note: None },
            now(),
        )
        .expect("met");
    assert_eq!(met.state(now()), ObligationState::Met);
    assert!(!met.state(now()).is_outstanding());

    let released = store
        .settle(
            &scope,
            &eng(),
            &released.obligation_id,
            Settlement::Released {
                reason: "they withdrew the ask".to_string(),
            },
            now(),
        )
        .expect("released");
    assert_eq!(released.state(now()), ObligationState::Released);
    assert_ne!(met.state(now()), released.state(now()));
}

/// A settled obligation never lapses, however long ago its deadline was.
#[test]
fn a_settled_obligation_never_lapses() {
    let (_tmp, store, scope) = store();
    let due = now() + Duration::days(1);
    let recorded = store
        .record(
            &scope,
            &promise("deck", due, ObligationDirection::OwedByUs),
            now(),
        )
        .expect("record");
    let settled = store
        .settle(
            &scope,
            &eng(),
            &recorded.obligation_id,
            Settlement::Met { note: None },
            now(),
        )
        .expect("settle");

    assert_eq!(
        settled.state(due + Duration::days(30)),
        ObligationState::Met
    );
    assert!(settled.overdue_by(due + Duration::days(30)).is_none());
    assert!(store
        .lapsed(&scope, &eng(), None, due + Duration::days(30))
        .expect("lapsed")
        .is_empty());
}

/// The first settlement is the settlement. Letting a second call move the date
/// would make the register unusable as evidence of what happened when.
#[test]
fn settling_twice_does_not_move_the_date() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(
            &scope,
            &promise(
                "deck",
                now() + Duration::days(1),
                ObligationDirection::OwedByUs,
            ),
            now(),
        )
        .expect("record");

    store
        .settle(
            &scope,
            &eng(),
            &recorded.obligation_id,
            Settlement::Met { note: None },
            now(),
        )
        .expect("first");
    let again = store
        .settle(
            &scope,
            &eng(),
            &recorded.obligation_id,
            Settlement::Released {
                reason: "changed my mind".to_string(),
            },
            now() + Duration::days(2),
        )
        .expect("second");

    assert_eq!(again.settled_at, Some(now()));
    assert_eq!(
        again.state(now()),
        ObligationState::Met,
        "the first settlement stands"
    );
}

/// The cycle asks "what is closest to being late", so outstanding is ordered by
/// deadline — which puts lapsed items first for free.
#[test]
fn outstanding_is_ordered_by_deadline_with_lapsed_first() {
    let (_tmp, store, scope) = store();
    store
        .record(
            &scope,
            &promise(
                "later",
                now() + Duration::days(10),
                ObligationDirection::OwedByUs,
            ),
            now(),
        )
        .expect("a");
    store
        .record(
            &scope,
            &promise(
                "soon",
                now() + Duration::days(1),
                ObligationDirection::OwedByUs,
            ),
            now(),
        )
        .expect("b");
    store
        .record(
            &scope,
            &promise(
                "overdue",
                now() - Duration::days(2),
                ObligationDirection::OwedByUs,
            ),
            now() - Duration::days(5),
        )
        .expect("c");

    let order: Vec<String> = store
        .outstanding(&scope, &eng(), now())
        .expect("outstanding")
        .into_iter()
        .map(|obligation| obligation.what)
        .collect();
    assert_eq!(order, vec!["overdue", "soon", "later"]);
}

/// What we promised and delivered is the record a relationship is built on.
/// Hiding it would leave the register showing only failures.
#[test]
fn the_register_keeps_what_was_delivered() {
    let (_tmp, store, scope) = store();
    let done = store
        .record(
            &scope,
            &promise(
                "deck",
                now() + Duration::days(1),
                ObligationDirection::OwedByUs,
            ),
            now(),
        )
        .expect("record");
    store
        .settle(
            &scope,
            &eng(),
            &done.obligation_id,
            Settlement::Met {
                note: Some("sent".to_string()),
            },
            now(),
        )
        .expect("settle");

    assert_eq!(store.for_audience(&scope, &eng()).expect("read").len(), 1);
    assert!(store
        .outstanding(&scope, &eng(), now())
        .expect("outstanding")
        .is_empty());
}

/// §6: an obligation *"surfaces on the owning agent's cycle"* — and a cycle asks
/// about the whole book, not one relationship at a time. The book mixes kinds.
#[test]
fn lapses_surface_across_relationships_in_one_ordered_list() {
    let (_tmp, store, scope) = store();
    let long_ago = now() - Duration::days(9);

    // A real book mixes kinds: a live deal, a standing client, a cohort.
    for (audience, what, days_overdue) in [
        (AudienceRef::engagement("eng-1"), "our deck", 2),
        (AudienceRef::account("acme"), "their term sheet", 5),
        (AudienceRef::program("q3-intake"), "our metrics", 1),
    ] {
        let mut request = promise(
            what,
            now() - Duration::days(days_overdue),
            if what.starts_with("our") {
                ObligationDirection::OwedByUs
            } else {
                ObligationDirection::OwedToUs
            },
        );
        request.audience = audience;
        store.record(&scope, &request, long_ago).expect("record");
    }

    let book = vec![
        AudienceRef::engagement("eng-1"),
        AudienceRef::account("acme"),
        AudienceRef::program("q3-intake"),
        // A stale list may name a relationship with no register at all.
        AudienceRef::engagement("eng-gone"),
        // ...or repeat one.
        AudienceRef::engagement("eng-1"),
    ];

    let all = store
        .lapsed_across(&scope, &book, None, now())
        .expect("across");
    let order: Vec<&str> = all.iter().map(|o| o.what.as_str()).collect();
    assert_eq!(
        order,
        vec!["their term sheet", "our deck", "our metrics"],
        "one list, soonest-due first, across a book that mixes relationship kinds"
    );

    // A repeated engagement must not double an item.
    assert_eq!(all.len(), 3);

    // And the split still holds across engagements.
    let ours = store
        .lapsed_across(&scope, &book, Some(ObligationDirection::OwedByUs), now())
        .expect("ours");
    assert_eq!(ours.len(), 2);
    assert!(ours.iter().all(|o| o.direction.lapse_is_ours()));
}

/// An obligation the owner cannot read is one they cannot act on.
#[test]
fn an_obligation_must_name_its_relationship_and_say_what_is_owed() {
    let (_tmp, store, scope) = store();
    let mut orphan = promise("x", now(), ObligationDirection::OwedByUs);
    orphan.audience = AudienceRef::engagement("  ");
    assert!(store.record(&scope, &orphan, now()).is_err());

    let blank = promise("   ", now(), ObligationDirection::OwedByUs);
    assert!(store.record(&scope, &blank, now()).is_err());
}

/// The generalisation, as behaviour: promises are made in every kind of
/// relationship, and the kind is part of the register's identity — one company
/// as a live deal and as a standing client keep separate books.
#[test]
fn the_register_serves_any_kind_of_relationship() {
    let (_tmp, store, scope) = store();

    for audience in [
        AudienceRef::engagement("acme"),
        AudienceRef::account("acme"),
        AudienceRef::program("q3-intake"),
        AudienceRef::panel("audit-2026"),
        AudienceRef::person("candidate-7"),
    ] {
        let mut request = promise(
            "send metrics",
            now() + Duration::days(3),
            ObligationDirection::OwedByUs,
        );
        request.audience = audience.clone();
        let recorded = store.record(&scope, &request, now()).expect("record");
        assert_eq!(recorded.audience, audience);
        assert_eq!(
            store.for_audience(&scope, &audience).expect("read").len(),
            1,
            "{} keeps its own register",
            audience.as_key()
        );
    }

    // Same id, different KIND — genuinely separate books, not one merged
    // register. This is the specific way widening the binding could have gone
    // wrong.
    assert_ne!(
        store
            .for_audience(&scope, &AudienceRef::engagement("acme"))
            .expect("read")[0]
            .obligation_id,
        store
            .for_audience(&scope, &AudienceRef::account("acme"))
            .expect("read")[0]
            .obligation_id
    );
}

/// Audiences and scopes do not leak into each other.
#[test]
fn registers_are_per_audience_and_per_scope() {
    let (_tmp, store, scope) = store();
    store
        .record(
            &scope,
            &promise("deck", now(), ObligationDirection::OwedByUs),
            now(),
        )
        .expect("record");

    assert!(store
        .for_audience(&scope, &AudienceRef::engagement("eng-2"))
        .expect("other engagement")
        .is_empty());
    assert!(store
        .for_audience(&ObligationScope::new("someone-else", "default"), &eng())
        .expect("other scope")
        .is_empty());
}

/// Pins the sealed-handle failure: the register's ids are derived internally
/// and the derivation was private, so an emitter that recorded a derived
/// obligation had NO handle to settle it later — when its basis changed (the
/// counterparty replied, a fresh visit superseded the old silence) the stale
/// row stayed open forever. `obligation_id_for` rebuilds the id from the same
/// request tuple, so the emitter settles a row it never kept.
#[test]
fn an_emitter_settles_via_the_rebuilt_id_without_keeping_the_row() {
    let (_tmp, store, scope) = store();
    let request = promise(
        "follow up with bob",
        now() - Duration::days(1),
        ObligationDirection::OwedByUs,
    );
    let recorded = store
        .record(&scope, &request, now() - Duration::days(2))
        .expect("record");

    // The emitter kept nothing. The handle is rebuilt from the tuple alone,
    // and it IS the register's id.
    let handle = obligation_id_for(&scope, &request);
    assert_eq!(handle, recorded.obligation_id);

    let settled = store
        .settle(
            &scope,
            &eng(),
            &handle,
            Settlement::Released {
                reason: "superseded: the basis advanced to a later visit".to_string(),
            },
            now(),
        )
        .expect("settle via the rebuilt handle");
    assert_eq!(settled.obligation_id, recorded.obligation_id);
    assert_eq!(settled.state(now()), ObligationState::Released);
    assert_eq!(settled.settled_at, Some(now()));
    assert_eq!(
        settled.settlement,
        Some(Settlement::Released {
            reason: "superseded: the basis advanced to a later visit".to_string(),
        })
    );
    assert!(
        store
            .lapsed(&scope, &eng(), None, now())
            .expect("lapsed")
            .is_empty(),
        "the settled row no longer surfaces as lapsed"
    );
}

/// The whole register comes back without a roster, and an unreadable one is
/// not "nothing is owed".
///
/// Pins the invisibility `lapsed_across` leaves behind: it takes the audiences
/// as a parameter, so a promise filed against a relationship missing from the
/// supplied list is invisible — and the promise nobody remembered is the one
/// this register exists to catch.
#[test]
fn the_whole_register_reads_without_a_roster() {
    let (tmp, store, scope) = store();
    let ours = store
        .record(
            &scope,
            &promise(
                "the metrics",
                now() - Duration::days(1),
                ObligationDirection::OwedByUs,
            ),
            now() - Duration::days(3),
        )
        .expect("record ours");
    let theirs = store
        .record(
            &scope,
            &RecordObligation {
                audience: AudienceRef::panel("panel-9"),
                program_id: None,
                what: "the signed letter".to_string(),
                due_at: now() + Duration::days(2),
                direction: ObligationDirection::OwedToUs,
                created_by: "company-assistant".to_string(),
                source_act_ref: None,
            },
            now() - Duration::days(3),
        )
        .expect("record theirs");

    let all = store.all_obligations(&scope).expect("read the register");
    assert_eq!(all.len(), 2, "both relationships are in one register read");
    // Soonest deadline first — the order the register itself surfaces.
    assert_eq!(all[0].obligation_id, ours.obligation_id);
    assert_eq!(all[1].obligation_id, theirs.obligation_id);
    assert_eq!(all[0].state(now()), ObligationState::Lapsed);
    assert_eq!(all[1].state(now()), ObligationState::Open);

    assert!(
        store
            .all_obligations(&ObligationScope::new("someone-else", "default"))
            .expect("read an untouched scope")
            .is_empty(),
        "a scope that has never recorded a promise owes nothing"
    );

    let dir = ArtifactV2Workspace::new(tmp.path())
        .scope_root(&scope.principal, &scope.workspace)
        .join("obligations");
    let log = std::fs::read_dir(&dir)
        .expect("obligations dir")
        .map(|entry| entry.expect("dir entry").path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("one register");
    let mut raw = std::fs::read_to_string(&log).expect("read");
    raw.push_str("{not json}\n");
    std::fs::write(&log, raw).expect("write");
    let error = store
        .all_obligations(&scope)
        .expect_err("an unreadable register must refuse, never answer `nothing is owed`");
    assert!(
        format!("{error:#}").contains("unparseable record"),
        "{error:#}"
    );
}
