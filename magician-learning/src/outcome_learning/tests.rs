//! The guardrails of §2 and §5, as behaviour.

use chrono::{Duration, TimeZone, Utc};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::store::{supersedes_silence, OutcomeScope, OutcomeStore};
use super::types::{Confounder, DeliveryState, OutcomeLabel, RecordOutcome};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap()
}

fn store() -> (tempfile::TempDir, OutcomeStore, OutcomeScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = OutcomeStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, OutcomeScope::new("anonymous", "default"))
}

fn outcome(act: &str, label: OutcomeLabel) -> RecordOutcome {
    RecordOutcome {
        engagement_id: Some("eng-1".to_string()),
        program_id: Some("prog-1".to_string()),
        act_ref: act.to_string(),
        variant_ref: "pitch-a".to_string(),
        variant_version: "v1".to_string(),
        label,
        delivery_state: DeliveryState::Delivered,
        matured_at: None,
        confounders: vec![Confounder::new("introduction", "warm")],
    }
}

/// §2's central guardrail. Before its window closes, silence is
/// indistinguishable from "not yet" — recording it early fills the sample with
/// counterparties who simply had not replied by Tuesday.
#[test]
fn silence_is_refused_until_its_window_closes() {
    let (_tmp, store, scope) = store();

    let missing_window = outcome("act-1", OutcomeLabel::Silent);
    let error = store
        .record(&scope, &missing_window, now())
        .expect_err("silence with no maturity time");
    assert!(error.to_string().contains("not yet"));

    let mut too_early = outcome("act-1", OutcomeLabel::Silent);
    too_early.matured_at = Some(now() + Duration::days(7));
    let error = store
        .record(&scope, &too_early, now())
        .expect_err("silence before maturity");
    assert!(error.to_string().contains("does not mature"));

    // Once the window closes it records.
    let mut matured = outcome("act-1", OutcomeLabel::Silent);
    matured.matured_at = Some(now() - Duration::hours(1));
    let recorded = store
        .record(&scope, &matured, now())
        .expect("mature silence");
    assert_eq!(recorded.label, OutcomeLabel::Silent);
}

/// An event is true the moment it happens, so only silence waits.
#[test]
fn an_event_label_needs_no_maturity() {
    let (_tmp, store, scope) = store();
    for label in [
        OutcomeLabel::Replied,
        OutcomeLabel::Accepted,
        OutcomeLabel::Rejected,
        OutcomeLabel::Opened,
        OutcomeLabel::Progressed,
    ] {
        assert!(!label.requires_maturity(), "{label:?} is an event");
        store
            .record(&scope, &outcome("act-1", label), now())
            .unwrap_or_else(|error| panic!("{label:?} should record: {error}"));
    }
    assert!(OutcomeLabel::Silent.requires_maturity());
}

/// §5: *"record which proposal version was active for every action"*. Without
/// it there is nothing to compare against, so the loop can only agree with
/// itself.
#[test]
fn an_outcome_with_no_cohort_key_is_refused() {
    let (_tmp, store, scope) = store();
    let mut no_version = outcome("act-1", OutcomeLabel::Replied);
    no_version.variant_version = "  ".to_string();

    let error = store
        .record(&scope, &no_version, now())
        .expect_err("no cohort key");
    assert!(error
        .to_string()
        .contains("confirm what is already believed"));
}

/// A count that grows on retry is the quietest way to make a small sample look
/// significant. A poller running twice, or a replayed webhook, must not inflate
/// it.
#[test]
fn re_observing_the_same_outcome_does_not_inflate_the_sample() {
    let (_tmp, store, scope) = store();
    let first = store
        .record(&scope, &outcome("act-1", OutcomeLabel::Replied), now())
        .expect("first");
    let again = store
        .record(
            &scope,
            &outcome("act-1", OutcomeLabel::Replied),
            now() + Duration::hours(3),
        )
        .expect("replay");

    assert_eq!(first.observation_id, again.observation_id);
    assert_eq!(
        again.observed_at, first.observed_at,
        "a replay must not rewrite when it was observed"
    );
    assert_eq!(
        store
            .observations_for_act(&scope, "act-1")
            .expect("read")
            .len(),
        1
    );
}

/// A delivery correction must land. An act recorded `delivered` that a provider
/// later reports as bounced would otherwise stay `delivered` for ever and be
/// counted as a counterparty ignoring us when nothing arrived.
#[test]
fn a_later_delivery_correction_supersedes_without_inflating_the_sample() {
    let (_tmp, store, scope) = store();

    let first = store
        .record(
            &scope,
            &outcome("act-1", OutcomeLabel::Silent).matured(now()),
            now(),
        )
        .expect("first");
    assert_eq!(first.delivery_state, DeliveryState::Delivered);
    assert!(first.is_usable_evidence(now()));

    // The provider reports the truth later.
    let mut corrected = outcome("act-1", OutcomeLabel::Silent).matured(now());
    corrected.delivery_state = DeliveryState::Bounced;
    let updated = store
        .record(&scope, &corrected, now() + Duration::days(1))
        .expect("correction");

    assert_eq!(updated.observation_id, first.observation_id, "same outcome");
    assert_eq!(updated.delivery_state, DeliveryState::Bounced);
    assert_eq!(
        updated.observed_at, first.observed_at,
        "a correction arriving later must not make the outcome look observed later"
    );

    let observations = store.observations_for_act(&scope, "act-1").expect("read");
    assert_eq!(observations.len(), 1, "corrected, not duplicated");
    assert_eq!(observations[0].delivery_state, DeliveryState::Bounced);

    // And the correction reaches the cohort read, exactly once.
    let cohort = store.cohort(&scope, "pitch-a", "v1").expect("cohort");
    assert_eq!(cohort.len(), 1, "the correction added no cohort pointer");
    assert_eq!(cohort[0].delivery_state, DeliveryState::Bounced);
    assert!(
        store
            .usable_cohort(&scope, "pitch-a", "v1", now())
            .expect("usable")
            .is_empty(),
        "once known to have bounced it is no longer evidence about anyone"
    );
}

/// One act legitimately produces several outcomes over time — accepted, then
/// delivered, then replied. Those are different observations, not corrections
/// of each other, so the label is part of the key.
#[test]
fn one_act_may_carry_several_distinct_outcomes() {
    let (_tmp, store, scope) = store();
    for label in [
        OutcomeLabel::Opened,
        OutcomeLabel::Replied,
        OutcomeLabel::Progressed,
    ] {
        store
            .record(&scope, &outcome("act-1", label), now())
            .expect("record");
    }
    assert_eq!(
        store
            .observations_for_act(&scope, "act-1")
            .expect("read")
            .len(),
        3
    );
}

/// The read the whole plan is built around: the same variant across versions.
/// Accepted proposals stay IN the sample — the before/after comparison is the
/// evidence, and excluding post-change data would throw away exactly what shows
/// whether the change helped.
#[test]
fn cohorts_separate_versions_of_the_same_variant() {
    let (_tmp, store, scope) = store();

    for act in ["act-1", "act-2"] {
        store
            .record(&scope, &outcome(act, OutcomeLabel::Replied), now())
            .expect("v1");
    }
    let mut v2 = outcome("act-3", OutcomeLabel::Replied);
    v2.variant_version = "v2".to_string();
    store.record(&scope, &v2, now()).expect("v2");

    assert_eq!(
        store.cohort(&scope, "pitch-a", "v1").expect("v1").len(),
        2,
        "the pre-change cohort"
    );
    assert_eq!(
        store.cohort(&scope, "pitch-a", "v2").expect("v2").len(),
        1,
        "the post-change cohort is kept, not discarded"
    );
    assert!(store
        .cohort(&scope, "pitch-b", "v1")
        .expect("other variant")
        .is_empty());
}

/// The cohort read is the plan's central query, so it must stay correct when one
/// act carries several observations — the shape that made the naive version
/// re-parse the same file once per row.
#[test]
fn a_cohort_reads_every_observation_including_several_from_one_act() {
    let (_tmp, store, scope) = store();
    for label in [
        OutcomeLabel::Opened,
        OutcomeLabel::Replied,
        OutcomeLabel::Progressed,
    ] {
        store
            .record(&scope, &outcome("act-1", label), now())
            .expect("record");
    }
    store
        .record(&scope, &outcome("act-2", OutcomeLabel::Replied), now())
        .expect("record");

    let cohort = store.cohort(&scope, "pitch-a", "v1").expect("cohort");
    assert_eq!(cohort.len(), 4, "three from one act plus one from another");

    // Index order is grant order, which is what an owner reading a cohort
    // expects.
    let labels: Vec<OutcomeLabel> = cohort.iter().map(|row| row.label).collect();
    assert_eq!(
        labels,
        vec![
            OutcomeLabel::Opened,
            OutcomeLabel::Replied,
            OutcomeLabel::Progressed,
            OutcomeLabel::Replied,
        ]
    );
}

/// A bounce that reads as `silent` would count as a counterparty ignoring us
/// when nothing arrived — the single most misleading confusion available here.
#[test]
fn an_act_that_never_arrived_is_not_evidence_about_a_counterparty() {
    let (_tmp, store, scope) = store();

    for (act, delivery) in [
        ("act-bounced", DeliveryState::Bounced),
        ("act-unknown", DeliveryState::Unknown),
        ("act-complained", DeliveryState::Complained),
    ] {
        let mut silent = outcome(act, OutcomeLabel::Silent);
        silent.delivery_state = delivery;
        silent.matured_at = Some(now() - Duration::hours(1));
        store.record(&scope, &silent, now()).expect("record");
    }
    let mut delivered = outcome("act-delivered", OutcomeLabel::Silent);
    delivered.delivery_state = DeliveryState::Delivered;
    delivered.matured_at = Some(now() - Duration::hours(1));
    store.record(&scope, &delivered, now()).expect("record");

    let all = store.cohort(&scope, "pitch-a", "v1").expect("cohort");
    assert_eq!(
        all.len(),
        4,
        "everything is recorded — the owner sees it all"
    );

    let usable = store
        .usable_cohort(&scope, "pitch-a", "v1", now())
        .expect("usable");
    assert_eq!(
        usable.len(),
        1,
        "only the one that actually reached a person is evidence about them"
    );
    assert_eq!(usable[0].act_ref, "act-delivered");
}

/// Silence recorded but not yet matured is not usable evidence either, even
/// though it is on the record.
#[test]
fn unmatured_silence_is_recorded_but_not_yet_evidence() {
    let (_tmp, store, scope) = store();
    let mut silent = outcome("act-1", OutcomeLabel::Silent);
    silent.matured_at = Some(now() - Duration::hours(1));
    let recorded = store.record(&scope, &silent, now()).expect("record");

    assert!(recorded.is_usable_evidence(now()));
    assert!(
        !recorded.is_usable_evidence(now() - Duration::days(1)),
        "asked about a moment before it matured, it is not yet evidence"
    );
}

/// Confounders are what keep the loop honest: the strongest effect in a small
/// sample is usually warm-versus-cold, not the copy.
#[test]
fn confounders_survive_the_round_trip() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(&scope, &outcome("act-1", OutcomeLabel::Replied), now())
        .expect("record");
    assert_eq!(
        recorded.confounders,
        vec![Confounder::new("introduction", "warm")]
    );

    let read_back = store
        .observations_for_act(&scope, "act-1")
        .expect("read")
        .pop()
        .expect("one observation");
    assert_eq!(read_back.confounders, recorded.confounders);
}

/// A later reply does not rewrite an earlier silence. Both are true of their own
/// windows, and an outcome that can be edited afterwards is not evidence.
#[test]
fn a_reply_after_silence_coexists_with_it() {
    let (_tmp, store, scope) = store();
    let mut silent = outcome("act-1", OutcomeLabel::Silent);
    silent.matured_at = Some(now() - Duration::days(7));
    store.record(&scope, &silent, now()).expect("silence");
    store
        .record(
            &scope,
            &outcome("act-1", OutcomeLabel::Replied),
            now() + Duration::days(1),
        )
        .expect("later reply");

    let observations = store.observations_for_act(&scope, "act-1").expect("read");
    assert_eq!(observations.len(), 2, "nothing was rewritten");
    assert!(supersedes_silence(OutcomeLabel::Replied));
    assert!(!supersedes_silence(OutcomeLabel::Rejected));
}

/// An act nobody named cannot be linked back to what was said, so it is refused.
#[test]
fn an_outcome_with_no_act_is_refused() {
    let (_tmp, store, scope) = store();
    let mut orphan = outcome("", OutcomeLabel::Replied);
    orphan.act_ref = "   ".to_string();
    assert!(store.record(&scope, &orphan, now()).is_err());
}

/// Same trap, same reason: `as_str()` and serde's `rename_all` are both live —
/// one in logs and comparisons, one in the stored observation — and a renamed
/// variant would silently split them.
#[test]
fn label_and_delivery_spellings_never_diverge() {
    for label in [
        OutcomeLabel::Replied,
        OutcomeLabel::Silent,
        OutcomeLabel::Accepted,
        OutcomeLabel::Rejected,
        OutcomeLabel::Opened,
        OutcomeLabel::Progressed,
    ] {
        assert_eq!(
            serde_json::to_string(&label).expect("serialise"),
            format!("\"{}\"", label.as_str()),
            "{label:?} serialises differently from how it is logged"
        );
    }
    for delivery in [
        DeliveryState::Accepted,
        DeliveryState::Delivered,
        DeliveryState::Bounced,
        DeliveryState::Complained,
        DeliveryState::Unknown,
    ] {
        assert_eq!(
            serde_json::to_string(&delivery).expect("serialise"),
            format!("\"{}\"", delivery.as_str()),
            "{delivery:?} serialises differently from how it is logged"
        );
    }
}

/// Scopes do not leak into each other.
#[test]
fn one_scopes_outcomes_are_not_anothers() {
    let (_tmp, store, scope) = store();
    store
        .record(&scope, &outcome("act-1", OutcomeLabel::Replied), now())
        .expect("record");

    let other = OutcomeScope::new("someone-else", "default");
    assert!(store
        .observations_for_act(&other, "act-1")
        .expect("read")
        .is_empty());
    assert!(store
        .cohort(&other, "pitch-a", "v1")
        .expect("read")
        .is_empty());
}

// ── Maturity (phase 2) ──────────────────────────────────────────────────────

use super::maturity::{
    confounder_kind, mature_silences, AwaitingOutcome, MaturityPolicy, NotMatured,
};

fn awaiting(act: &str, acted_at: chrono::DateTime<Utc>) -> AwaitingOutcome {
    AwaitingOutcome {
        act_ref: act.to_string(),
        variant_ref: "pitch-a".to_string(),
        variant_version: "v1".to_string(),
        engagement_id: Some("eng-1".to_string()),
        program_id: Some("prog-1".to_string()),
        acted_at,
        delivery_state: DeliveryState::Delivered,
        confounders: vec![Confounder::new(confounder_kind::INTRODUCTION, "cold")],
    }
}

impl RecordOutcome {
    /// Test helper: give a silence a closed window.
    fn matured(mut self, at: chrono::DateTime<Utc>) -> Self {
        self.matured_at = Some(at - Duration::hours(1));
        self
    }
}

fn policy() -> MaturityPolicy {
    MaturityPolicy::new(Duration::days(14)).expect("policy")
}

/// A window of zero would mature silence the instant an act is sent, recording
/// a decision nobody had the chance to make. It cannot be reached by config.
#[test]
fn a_maturity_window_must_be_positive() {
    assert!(MaturityPolicy::new(Duration::zero()).is_err());
    assert!(MaturityPolicy::new(Duration::seconds(-1)).is_err());
    assert!(policy()
        .with_variant_window("pitch-b", Duration::zero())
        .is_err());
}

/// The window is a per-domain judgement: an accelerator that has not replied in
/// two weeks has decided; a support ticket in two hours has not.
#[test]
fn a_variant_may_have_its_own_window() {
    let policy = policy()
        .with_variant_window("support-reply", Duration::hours(2))
        .expect("variant window");

    assert_eq!(policy.window_for("pitch-a"), Duration::days(14));
    assert_eq!(policy.window_for("support-reply"), Duration::hours(2));
    assert_eq!(
        policy.matures_at("support-reply", now()),
        now() + Duration::hours(2)
    );
}

/// The sweep is what fills the sample. Without it the data contains only the
/// counterparties who replied — the most flattering possible dataset and the
/// least useful.
#[test]
fn the_sweep_records_silence_once_the_window_closes() {
    let (_tmp, store, scope) = store();
    let sent = now() - Duration::days(20);

    let report = mature_silences(&store, &scope, &[awaiting("act-1", sent)], &policy(), now())
        .expect("sweep");
    assert_eq!(report.matured, vec!["act-1".to_string()]);

    let observations = store.observations_for_act(&scope, "act-1").expect("read");
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].label, OutcomeLabel::Silent);
    assert_eq!(
        observations[0].matured_at,
        Some(sent + Duration::days(14)),
        "silence matured when its window closed, not when the sweep happened to run"
    );
    assert_eq!(
        observations[0].confounders.len(),
        1,
        "confounders carry through"
    );
}

/// An act still inside its window is left alone, and the caller is told when to
/// come back rather than seeing an undifferentiated skip.
#[test]
fn an_act_inside_its_window_is_left_alone() {
    let (_tmp, store, scope) = store();
    let sent = now() - Duration::days(3);

    let report = mature_silences(&store, &scope, &[awaiting("act-1", sent)], &policy(), now())
        .expect("sweep");
    assert!(report.matured.is_empty());
    assert_eq!(
        report.skipped,
        vec![(
            "act-1".to_string(),
            NotMatured::StillOpen {
                matures_at: sent + Duration::days(14)
            }
        )]
    );
    assert!(store
        .observations_for_act(&scope, "act-1")
        .expect("read")
        .is_empty());
}

/// The check that matters: an act the counterparty ANSWERED must never mature
/// into silence. The sweep reads the store rather than trusting the caller's
/// list to be current — a stale list would record that somebody ignored us when
/// they did not.
#[test]
fn an_answered_act_never_matures_into_silence() {
    let (_tmp, store, scope) = store();
    let sent = now() - Duration::days(20);
    store
        .record(&scope, &outcome("act-1", OutcomeLabel::Replied), now())
        .expect("they replied");

    let report = mature_silences(&store, &scope, &[awaiting("act-1", sent)], &policy(), now())
        .expect("sweep");
    assert!(report.matured.is_empty());
    assert_eq!(
        report.skipped,
        vec![(
            "act-1".to_string(),
            NotMatured::AlreadyAnswered {
                label: OutcomeLabel::Replied
            }
        )]
    );

    let observations = store.observations_for_act(&scope, "act-1").expect("read");
    assert_eq!(observations.len(), 1, "no silence was invented");
    assert_eq!(observations[0].label, OutcomeLabel::Replied);
}

/// Every engagement label counts as an answer, not just a reply.
#[test]
fn any_engagement_counts_as_an_answer() {
    // `store` the helper is shadowed by the local binding inside the loop, so
    // every fixture this test needs is created before the first binding.
    let engagement_fixtures: Vec<_> = (0..4).map(|_| store()).collect();
    let (_rej_tmp, rejection_store, rejection_scope) = store();

    for (index, label) in [
        OutcomeLabel::Replied,
        OutcomeLabel::Accepted,
        OutcomeLabel::Opened,
        OutcomeLabel::Progressed,
    ]
    .into_iter()
    .enumerate()
    {
        let (_tmp, store, scope) = &engagement_fixtures[index];
        store
            .record(scope, &outcome("act-1", label), now())
            .expect("record");
        let report = mature_silences(
            store,
            scope,
            &[awaiting("act-1", now() - Duration::days(20))],
            &policy(),
            now(),
        )
        .expect("sweep");
        assert!(report.matured.is_empty(), "{label:?} is an answer");
    }

    // A REJECTION is the case that makes this more than "engagement". It is not
    // engagement, but the counterparty answered — maturing it into silence would
    // record that they both replied and ignored us.
    let (store, scope) = (&rejection_store, &rejection_scope);
    store
        .record(scope, &outcome("act-1", OutcomeLabel::Rejected), now())
        .expect("record");
    let report = mature_silences(
        store,
        scope,
        &[awaiting("act-1", now() - Duration::days(20))],
        &policy(),
        now(),
    )
    .expect("sweep");

    assert!(report.matured.is_empty(), "a rejection is an answer");
    assert_eq!(
        report.skipped,
        vec![(
            "act-1".to_string(),
            NotMatured::AlreadyAnswered {
                label: OutcomeLabel::Rejected
            }
        )]
    );
    let labels: Vec<OutcomeLabel> = store
        .observations_for_act(scope, "act-1")
        .expect("read")
        .iter()
        .map(|row| row.label)
        .collect();
    assert_eq!(
        labels,
        vec![OutcomeLabel::Rejected],
        "no silence was invented alongside the rejection"
    );
}

/// A repeated sweep must not double-record or re-do work.
#[test]
fn a_repeated_sweep_is_idempotent() {
    let (_tmp, store, scope) = store();
    let acts = [awaiting("act-1", now() - Duration::days(20))];

    let first = mature_silences(&store, &scope, &acts, &policy(), now()).expect("first");
    assert_eq!(first.matured_count(), 1);

    let second = mature_silences(&store, &scope, &acts, &policy(), now()).expect("second");
    assert_eq!(second.matured_count(), 0);
    assert_eq!(
        second.skipped,
        vec![("act-1".to_string(), NotMatured::AlreadyRecorded)]
    );
    assert_eq!(
        store
            .observations_for_act(&scope, "act-1")
            .expect("read")
            .len(),
        1
    );
}

/// A bounce that then went quiet is still recorded — omitting it would make the
/// cohort count disagree with the number of acts performed — and it is
/// `usable_cohort` that keeps it out of evidence.
#[test]
fn a_bounced_act_matures_but_is_not_evidence() {
    let (_tmp, store, scope) = store();
    let mut bounced = awaiting("act-1", now() - Duration::days(20));
    bounced.delivery_state = DeliveryState::Bounced;

    mature_silences(&store, &scope, &[bounced], &policy(), now()).expect("sweep");

    assert_eq!(
        store.cohort(&scope, "pitch-a", "v1").expect("cohort").len(),
        1
    );
    assert!(store
        .usable_cohort(&scope, "pitch-a", "v1", now())
        .expect("usable")
        .is_empty());
}

// ── Proposals (phase 3) ─────────────────────────────────────────────────────

use super::proposal::{
    propose, summarise_cohort, CohortSummary, NotProposable, MINIMUM_COHORT, MINIMUM_COUNTERPARTIES,
};
use super::types::OutcomeObservation;

fn observation(
    id: &str,
    engagement: &str,
    label: OutcomeLabel,
    delivery: DeliveryState,
    confounders: Vec<Confounder>,
) -> OutcomeObservation {
    OutcomeObservation {
        observation_id: id.to_string(),
        engagement_id: Some(engagement.to_string()),
        program_id: None,
        act_ref: format!("act-{id}"),
        variant_ref: "pitch-a".to_string(),
        variant_version: "v1".to_string(),
        label,
        delivery_state: delivery,
        observed_at: now(),
        matured_at: if label.requires_maturity() {
            Some(now() - Duration::hours(1))
        } else {
            None
        },
        confounders,
    }
}

/// Builds a cohort of `n` observations, `engaged` of them engagements, each from
/// its own counterparty, all carrying `confounder`.
fn cohort(version: &str, n: usize, engaged: usize, confounder: (&str, &str)) -> CohortSummary {
    let rows: Vec<OutcomeObservation> = (0..n)
        .map(|i| {
            let mut row = observation(
                &format!("{version}-{i}"),
                &format!("eng-{version}-{i}"),
                if i < engaged {
                    OutcomeLabel::Replied
                } else {
                    OutcomeLabel::Silent
                },
                DeliveryState::Delivered,
                vec![Confounder::new(confounder.0, confounder.1)],
            );
            row.variant_version = version.to_string();
            row
        })
        .collect();
    summarise_cohort("pitch-a", version, &rows, now())
}

/// The summary counts what is there and reports what is NOT usable separately,
/// so "we sent thirty" and "thirty are comparable" stay different numbers.
#[test]
fn a_summary_separates_what_is_usable_from_what_was_recorded() {
    let rows = vec![
        observation(
            "a",
            "eng-1",
            OutcomeLabel::Replied,
            DeliveryState::Delivered,
            vec![],
        ),
        observation(
            "b",
            "eng-2",
            OutcomeLabel::Silent,
            DeliveryState::Delivered,
            vec![],
        ),
        observation(
            "c",
            "eng-3",
            OutcomeLabel::Rejected,
            DeliveryState::Delivered,
            vec![],
        ),
        // Never arrived — recorded, but says nothing about a counterparty.
        observation(
            "d",
            "eng-4",
            OutcomeLabel::Silent,
            DeliveryState::Bounced,
            vec![],
        ),
    ];
    let summary = summarise_cohort("pitch-a", "v1", &rows, now());

    assert_eq!(summary.usable, 3);
    assert_eq!(
        summary.unusable, 1,
        "the bounce is recorded but not evidence"
    );
    assert_eq!(summary.engaged, 1);
    assert_eq!(summary.silent, 1);
    assert_eq!(summary.rejected, 1);
    assert_eq!(
        summary.counterparties, 3,
        "the bounced act's counterparty does not count either"
    );
}

/// An observation nobody can attribute to a counterparty cannot contribute to
/// diversity — otherwise a single counterparty plus four anonymous rows would
/// read as five.
#[test]
fn an_unattributable_observation_does_not_count_toward_diversity() {
    let mut anonymous = observation(
        "a",
        "eng-1",
        OutcomeLabel::Replied,
        DeliveryState::Delivered,
        vec![],
    );
    anonymous.engagement_id = None;
    let rows = vec![
        anonymous,
        observation(
            "b",
            "eng-1",
            OutcomeLabel::Replied,
            DeliveryState::Delivered,
            vec![],
        ),
    ];
    let summary = summarise_cohort("pitch-a", "v1", &rows, now());
    assert_eq!(summary.usable, 2);
    assert_eq!(summary.counterparties, 1, "one named counterparty, not two");
}

/// §8: *"a proposal that cannot state its N is not made."*
#[test]
fn a_sample_below_the_floor_produces_no_proposal() {
    let small = cohort("v1", MINIMUM_COHORT - 1, 1, ("introduction", "cold"));
    let big = cohort("v2", MINIMUM_COHORT + 3, 3, ("introduction", "cold"));

    assert_eq!(
        propose("pitch-a", &small, &big, vec![], now()),
        Err(NotProposable::SampleTooSmall {
            baseline: MINIMUM_COHORT - 1,
            candidate: MINIMUM_COHORT + 3,
            needed: MINIMUM_COHORT,
        })
    );
    assert!(!small.can_support_a_proposal());
    assert!(big.can_support_a_proposal());
}

/// §5: *"One rejection is a fact about that counterparty, not about the
/// pitch."* Ten observations from one counterparty is one data point.
#[test]
fn ten_observations_from_one_counterparty_are_not_a_pattern() {
    let rows: Vec<OutcomeObservation> = (0..10)
        .map(|i| {
            observation(
                &format!("x{i}"),
                "eng-only-one",
                OutcomeLabel::Replied,
                DeliveryState::Delivered,
                vec![],
            )
        })
        .collect();
    let lopsided = summarise_cohort("pitch-a", "v1", &rows, now());
    assert_eq!(lopsided.usable, 10);
    assert_eq!(lopsided.counterparties, 1);
    assert!(!lopsided.can_support_a_proposal());

    let healthy = cohort("v2", 6, 3, ("introduction", "cold"));
    assert_eq!(
        propose("pitch-a", &lopsided, &healthy, vec![], now()),
        Err(NotProposable::TooFewCounterparties {
            baseline: 1,
            candidate: 6,
            needed: MINIMUM_COUNTERPARTIES,
        })
    );
}

/// §3's central honesty problem: *"A loop that cannot see that will confidently
/// attribute an introducer's effect to a subject line."* Cohorts dominated by
/// different values of the same confounder cannot be compared on the copy.
#[test]
fn cohorts_dominated_by_different_confounders_are_refused() {
    let cold = cohort("v1", 6, 1, ("introduction", "cold"));
    let warm = cohort("v2", 6, 5, ("introduction", "warm"));

    assert_eq!(
        propose("pitch-a", &cold, &warm, vec!["obs-1".to_string()], now()),
        Err(NotProposable::ConfoundedBy {
            kind: "introduction".to_string(),
            baseline: "cold".to_string(),
            candidate: "warm".to_string(),
        }),
        "the warm intros explain this at least as well as the new copy does"
    );
}

/// With the confounder held constant, the same difference becomes proposable.
#[test]
fn holding_the_confounder_constant_makes_it_proposable() {
    let before = cohort("v1", 6, 1, ("introduction", "cold"));
    let after = cohort("v2", 6, 5, ("introduction", "cold"));

    let candidate = propose(
        "pitch-a",
        &before,
        &after,
        vec!["obs-1".to_string(), "obs-2".to_string()],
        now(),
    )
    .expect("same confounder on both sides");

    assert_eq!(candidate.engagement_counts(), ((1, 6), (5, 6)));
    assert_eq!(candidate.evidence_refs.len(), 2, "a reader can go and look");
    assert!(candidate.caveats.is_empty());
}

/// A confounder that differs without dominating is a CAVEAT, not a block.
/// Filtering it away would hand the owner a cleaner story than the data
/// supports.
#[test]
fn a_minor_confounder_difference_is_surfaced_rather_than_hidden() {
    let mut before = cohort("v1", 6, 1, ("introduction", "cold"));
    let mut after = cohort("v2", 6, 5, ("introduction", "cold"));
    // Neither side has a majority for `stage`, so it explains nothing on its
    // own — but it is not the same on both sides.
    before.confounders.insert(
        "stage".to_string(),
        [("seed".to_string(), 2), ("series_a".to_string(), 2)].into(),
    );
    after.confounders.insert(
        "stage".to_string(),
        [("seed".to_string(), 1), ("series_a".to_string(), 3)].into(),
    );

    let candidate = propose("pitch-a", &before, &after, vec!["obs-1".to_string()], now())
        .expect("not blocking");
    assert_eq!(candidate.caveats.len(), 1);
    assert!(candidate.caveats[0].contains("stage"));
}

/// Two cohorts that behaved identically are not a finding.
#[test]
fn identical_cohorts_produce_no_proposal() {
    let before = cohort("v1", 6, 3, ("introduction", "cold"));
    let after = cohort("v2", 6, 3, ("introduction", "cold"));
    assert_eq!(
        propose("pitch-a", &before, &after, vec!["obs-1".to_string()], now()),
        Err(NotProposable::NoDifference)
    );
}

/// §9: *"Every proposal carries sample size, confounders and evidence refs."*
/// A candidate whose claims cannot be checked looks like a finding and cannot be
/// audited into one — worse than no candidate.
#[test]
fn a_candidate_citing_no_evidence_is_refused() {
    let before = cohort("v1", 6, 1, ("introduction", "cold"));
    let after = cohort("v2", 6, 5, ("introduction", "cold"));

    assert_eq!(
        propose("pitch-a", &before, &after, vec![], now()),
        Err(NotProposable::NoEvidenceCited)
    );
    assert!(propose("pitch-a", &before, &after, vec!["obs-1".to_string()], now()).is_ok());
}

/// The worse confounder case: dominant on one side, **unrecorded** on the other.
/// That is no information, not no difference — and treating an absent confounder
/// as absent-in-fact is how a cohort that simply was not measured passes as
/// comparable.
#[test]
fn a_confounder_recorded_on_only_one_side_blocks_the_comparison() {
    let mut measured = cohort("v1", 6, 1, ("introduction", "cold"));
    let mut unmeasured = cohort("v2", 6, 5, ("introduction", "cold"));
    // Only the candidate cohort recorded whether an introducer was involved,
    // and it is dominated by "yes".
    unmeasured
        .confounders
        .insert("introducer".to_string(), [("yes".to_string(), 5)].into());

    assert_eq!(
        propose(
            "pitch-a",
            &measured,
            &unmeasured,
            vec!["obs-1".to_string()],
            now()
        ),
        Err(NotProposable::ConfoundedBy {
            kind: "introducer".to_string(),
            baseline: "<unrecorded>".to_string(),
            candidate: "yes".to_string(),
        })
    );

    // Symmetric: dominant on the baseline, unrecorded on the candidate.
    measured
        .confounders
        .insert("introducer".to_string(), [("yes".to_string(), 5)].into());
    unmeasured.confounders.remove("introducer");
    assert_eq!(
        propose(
            "pitch-a",
            &measured,
            &unmeasured,
            vec!["obs-1".to_string()],
            now()
        ),
        Err(NotProposable::ConfoundedBy {
            kind: "introducer".to_string(),
            baseline: "yes".to_string(),
            candidate: "<unrecorded>".to_string(),
        })
    );
}

/// Counts, never a rate. On a sample of six, `0.33` versus `0.50` is two replies
/// versus three — and the decimal reads as precision that is not there.
#[test]
fn a_candidate_reports_counts_not_rates() {
    let before = cohort("v1", 6, 2, ("introduction", "cold"));
    let after = cohort("v2", 6, 3, ("introduction", "cold"));
    let candidate =
        propose("pitch-a", &before, &after, vec!["obs-1".to_string()], now()).expect("proposable");

    let ((be, bn), (ce, cn)) = candidate.engagement_counts();
    assert_eq!((be, bn, ce, cn), (2, 6, 3, 6));

    // The type carries no rate, score or ranking at all — that absence is the
    // control, not an omission.
    let json = serde_json::to_string(&candidate).expect("serialise");
    for forbidden in ["\"rate\"", "\"score\"", "\"rank\"", "\"confidence\""] {
        assert!(
            !json.contains(forbidden),
            "a candidate must not carry {forbidden}: it would be optimising a metric nobody chose"
        );
    }
}

// ── The separator, and the index that is joined with it ─────────────────────

/// Pins: every caller string that feeds the observation id, the cohort key or
/// the cohort index line refuses U+001F.
///
/// This store was the only one in the set that never did. U+001F is what joins
/// an id's components, so a value hiding one moves the boundary between two of
/// them: variant `pitch<U+001F>a` at version `v1` and variant `pitch` at version
/// `a<U+001F>v1` derived the SAME `obs-…` and named the SAME cohort file. Two
/// different cohorts folded into one sample, and the second outcome resumed the
/// first instead of being recorded — a sample that stops growing without saying
/// so.
#[test]
fn a_unit_separator_in_any_caller_string_is_refused() {
    let (_tmp, store, scope) = store();
    let sep = '\u{1f}';

    let mut act = outcome("act-1", OutcomeLabel::Replied);
    act.act_ref = format!("act{sep}1");
    let mut variant = outcome("act-1", OutcomeLabel::Replied);
    variant.variant_ref = format!("pitch{sep}a");
    let mut version = outcome("act-1", OutcomeLabel::Replied);
    version.variant_version = format!("v{sep}1");

    for (what, request) in [
        ("act ref", act),
        ("variant ref", variant),
        ("variant version", version),
    ] {
        let error = store
            .record(&scope, &request, now())
            .expect_err("a crafted component must not derive an id");
        assert!(error.to_string().contains("U+001F"), "{what}: {error}");
    }

    let crafted_scope = OutcomeScope::new(format!("anon{sep}ymous"), "default");
    let error = store
        .record(
            &crafted_scope,
            &outcome("act-1", OutcomeLabel::Replied),
            now(),
        )
        .expect_err("a crafted scope must not fold into another owner's sample");
    assert!(error.to_string().contains("U+001F"), "{error}");

    // The reads refuse it too: a crafted cohort key must not name another
    // cohort's file, and a crafted act ref must not name another act's log.
    let cohort = store
        .cohort(&scope, &format!("pitch{sep}a"), "")
        .expect_err("cohort key");
    assert!(cohort.to_string().contains("U+001F"), "{cohort}");
    let acts = store
        .observations_for_act(&scope, &format!("act{sep}1"))
        .expect_err("act ref");
    assert!(acts.to_string().contains("U+001F"), "{acts}");

    // Every refusal wrote nothing, and the honest outcome still records.
    store
        .record(&scope, &outcome("act-1", OutcomeLabel::Replied), now())
        .expect("an uncrafted outcome records");
    let recorded: Vec<String> = store
        .cohort(&scope, "pitch-a", "v1")
        .expect("cohort")
        .into_iter()
        .map(|observation| observation.act_ref)
        .collect();
    assert_eq!(recorded, vec!["act-1".to_string()]);
}

/// Pins: a cohort index line that will not split is refused, not skipped.
///
/// The reader used to `continue` past it, so one damaged line dropped a
/// recorded observation out of the cohort in silence: the comparison then ran
/// on a sample smaller than the one the owner was shown, and the smaller a
/// sample is the more decisive a difference in it looks. The fold refuses
/// rather than reading past corruption, exactly as `parse_log_lines` does.
#[test]
fn a_cohort_index_line_that_will_not_split_is_refused_not_skipped() {
    let (tmp, store, scope) = store();
    store
        .record(&scope, &outcome("act-1", OutcomeLabel::Replied), now())
        .expect("act-1");
    store
        .record(&scope, &outcome("act-2", OutcomeLabel::Replied), now())
        .expect("act-2");

    let before: Vec<String> = store
        .cohort(&scope, "pitch-a", "v1")
        .expect("cohort")
        .into_iter()
        .map(|observation| observation.act_ref)
        .collect();
    assert_eq!(before, vec!["act-1".to_string(), "act-2".to_string()]);

    let cohorts = ArtifactV2Workspace::new(tmp.path())
        .scope_root("anonymous", "default")
        .join("outcome_learning")
        .join("cohorts");
    let indexes: Vec<std::path::PathBuf> = std::fs::read_dir(&cohorts)
        .expect("cohorts dir")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(indexes.len(), 1, "one cohort key, one index: {indexes:?}");
    let mut damaged = std::fs::read_to_string(&indexes[0]).expect("index");
    damaged.push_str("act-3-carries-no-separator\n");
    std::fs::write(&indexes[0], damaged).expect("damage the index");

    let error = store
        .cohort(&scope, "pitch-a", "v1")
        .expect_err("a line that will not split is corruption");
    assert!(error.to_string().contains("no U+001F separator"), "{error}");

    // The filtered read refuses for the same reason rather than answering with
    // the two rows it could still resolve.
    let usable = store
        .usable_cohort(&scope, "pitch-a", "v1", now())
        .expect_err("the same refusal");
    assert!(
        usable.to_string().contains("no U+001F separator"),
        "{usable}"
    );
}
