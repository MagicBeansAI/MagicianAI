//! §6A's two rules, as behaviour.

use chrono::{Duration, TimeZone, Utc};
use tokio_util::sync::CancellationToken;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceRef;
use crate::magician_v2::evidence::store_cursor::fold_was_cancelled;

use super::{
    CommitmentDecisionDisposition, CommitmentDecisionReceiptRequest, CommitmentDecisionVerb,
    CommitmentDirection, CommitmentRecord, CommitmentScope, CommitmentStatus, Commitments,
    RecordCommitment,
};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn store() -> (tempfile::TempDir, Commitments, CommitmentScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = Commitments::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, CommitmentScope::new("anonymous", "default"))
}

fn eng() -> AudienceRef {
    AudienceRef::engagement("eng-1")
}

fn terms(source: &str, direction: CommitmentDirection, what: &str) -> RecordCommitment {
    RecordCommitment {
        audience: AudienceRef::engagement("eng-1"),
        source_ref: source.to_string(),
        direction,
        terms: what.to_string(),
        stated_at: now(),
    }
}

/// **Rule 1.** Recording that they offered something is not agreeing to it, and
/// the agent may not restate an unconfirmed commitment to anyone.
#[test]
fn nothing_may_be_restated_outward_until_it_is_confirmed() {
    let (_tmp, store, scope) = store();

    for direction in [
        CommitmentDirection::OfferedToUs,
        CommitmentDirection::StatedByUs,
    ] {
        let recorded = store
            .record(&scope, &terms("msg-1", direction, "20% discount"), now())
            .expect("record");
        assert_eq!(
            recorded.status,
            CommitmentStatus::Unconfirmed,
            "a machine may only ever record unconfirmed"
        );
        assert!(
            !recorded.may_be_restated_outward(),
            "restating a term the owner never agreed to IS negotiating, which is the \
             exclusion §6A preserves"
        );
    }

    assert!(store
        .restatable(&scope, &eng())
        .expect("restatable")
        .is_empty());
}

/// **Rule 2.** Anything the agent believes *we* committed to needs owner
/// confirmation before any other module treats it as true — and finding those is
/// the query the feature exists for.
#[test]
fn what_we_appear_to_have_promised_is_findable_before_they_quote_it_back() {
    let (_tmp, store, scope) = store();
    store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::OfferedToUs, "they offer 20%"),
            now(),
        )
        .expect("theirs");
    let ours = store
        .record(
            &scope,
            &terms(
                "msg-2",
                CommitmentDirection::StatedByUs,
                "we deliver by March",
            ),
            now(),
        )
        .expect("ours");

    let owed_a_check = store.unconfirmed_from_us(&scope, &eng()).expect("query");
    assert_eq!(owed_a_check.len(), 1, "only what WE appear to have said");
    assert_eq!(owed_a_check[0].commitment_id, ours.commitment_id);
    assert!(ours.needs_owner_check());

    // Once the owner checks it, it stops appearing.
    store
        .confirm(&scope, &eng(), &ours.commitment_id, "owner", now())
        .expect("confirm");
    assert!(store
        .unconfirmed_from_us(&scope, &eng())
        .expect("query")
        .is_empty());
}

/// A confirmation with no name is not a confirmation — an unnamed one is how an
/// automated caller would grant itself the single control this register has.
#[test]
fn a_confirmation_must_name_who_confirmed_it() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::StatedByUs, "March"),
            now(),
        )
        .expect("record");

    assert!(store
        .confirm(&scope, &eng(), &recorded.commitment_id, "   ", now())
        .is_err());
    assert!(store
        .confirm(&scope, &eng(), &recorded.commitment_id, "owner", now())
        .is_ok());
}

/// There is no way for a caller to hand in a confirmed row: `RecordCommitment`
/// has no status field, so confirmation cannot be self-granted at the door.
#[test]
fn a_caller_cannot_record_something_already_confirmed() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::StatedByUs, "March"),
            now(),
        )
        .expect("record");
    assert_eq!(recorded.status, CommitmentStatus::Unconfirmed);
    assert!(recorded.confirmed_by.is_none());

    let confirmed = store
        .confirm(
            &scope,
            &eng(),
            &recorded.commitment_id,
            "owner",
            now() + Duration::hours(1),
        )
        .expect("confirm");
    assert_eq!(confirmed.confirmed_by.as_deref(), Some("owner"));
    assert_eq!(confirmed.confirmed_at, Some(now() + Duration::hours(1)));
    assert!(confirmed.may_be_restated_outward());
    assert_eq!(
        store.restatable(&scope, &eng()).expect("restatable").len(),
        1
    );
}

/// Superseding and withdrawing take a term out of force, and neither may then be
/// restated.
#[test]
fn a_superseded_or_withdrawn_term_is_no_longer_in_force() {
    let (_tmp, store, scope) = store();
    let first = store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::OfferedToUs, "20%"),
            now(),
        )
        .expect("first");
    let second = store
        .record(
            &scope,
            &terms("msg-2", CommitmentDirection::OfferedToUs, "25%"),
            now(),
        )
        .expect("second");

    let superseded = store
        .supersede(
            &scope,
            &eng(),
            &first.commitment_id,
            &second.commitment_id,
            now(),
        )
        .expect("supersede");
    assert_eq!(superseded.status, CommitmentStatus::Superseded);
    assert_eq!(
        superseded.superseded_by.as_deref(),
        Some(second.commitment_id.as_str())
    );
    assert!(!superseded.status.is_live());
    assert!(!superseded.may_be_restated_outward());

    let withdrawn = store
        .withdraw(&scope, &eng(), &second.commitment_id, now())
        .expect("withdraw");
    assert_eq!(withdrawn.status, CommitmentStatus::Withdrawn);
    assert!(!withdrawn.status.is_live());
}

/// A term that is no longer in force cannot be confirmed back into life.
#[test]
fn a_withdrawn_term_cannot_be_confirmed() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::OfferedToUs, "20%"),
            now(),
        )
        .expect("record");
    store
        .withdraw(&scope, &eng(), &recorded.commitment_id, now())
        .expect("withdraw");

    assert!(
        store
            .confirm(&scope, &eng(), &recorded.commitment_id, "owner", now())
            .is_err(),
        "confirming a withdrawn term would resurrect something that was taken back"
    );
}

/// A commitment cannot supersede itself — that would take a live term out of
/// force with nothing replacing it.
#[test]
fn a_commitment_cannot_supersede_itself() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::OfferedToUs, "20%"),
            now(),
        )
        .expect("record");
    assert!(store
        .supersede(
            &scope,
            &eng(),
            &recorded.commitment_id,
            &recorded.commitment_id,
            now()
        )
        .is_err());
}

/// The same words in two different messages are two commitments: which message
/// it came from is what the owner reads before confirming.
#[test]
fn the_same_words_from_two_sources_are_two_commitments() {
    let (_tmp, store, scope) = store();
    store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::OfferedToUs, "20% discount"),
            now(),
        )
        .expect("a");
    store
        .record(
            &scope,
            &terms("msg-2", CommitmentDirection::OfferedToUs, "20% discount"),
            now(),
        )
        .expect("b");
    assert_eq!(store.for_audience(&scope, &eng()).expect("read").len(), 2);

    // But re-extracting the SAME statement is one, whatever the spacing.
    store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::OfferedToUs, "20%   Discount"),
            now(),
        )
        .expect("re-extract");
    assert_eq!(store.for_audience(&scope, &eng()).expect("read").len(), 2);
}

/// The owner confirms by reading the words, not the summary — so a commitment
/// must name its source, its engagement, and say something.
#[test]
fn a_commitment_must_be_checkable() {
    let (_tmp, store, scope) = store();
    for broken in [
        RecordCommitment {
            audience: AudienceRef::engagement("  "),
            ..terms("msg-1", CommitmentDirection::OfferedToUs, "x")
        },
        RecordCommitment {
            source_ref: "  ".to_string(),
            ..terms("msg-1", CommitmentDirection::OfferedToUs, "x")
        },
        RecordCommitment {
            terms: "  ".to_string(),
            ..terms("msg-1", CommitmentDirection::OfferedToUs, "x")
        },
    ] {
        assert!(store.record(&scope, &broken, now()).is_err());
    }
}

/// The generalisation, as behaviour: terms get said in every kind of
/// relationship, and the register serves all of them — with the KIND part of the
/// identity, so one company as a live deal and as a standing client keep
/// separate registers.
#[test]
fn the_register_serves_any_kind_of_relationship() {
    let (_tmp, store, scope) = store();

    for audience in [
        AudienceRef::engagement("acme"),
        AudienceRef::account("acme"),
        AudienceRef::panel("audit-2026"),
        AudienceRef::person("candidate-7"),
    ] {
        let recorded = store
            .record(
                &scope,
                &RecordCommitment {
                    audience: audience.clone(),
                    ..terms("msg-1", CommitmentDirection::OfferedToUs, "start in March")
                },
                now(),
            )
            .expect("record");
        assert_eq!(recorded.audience, audience);
        assert_eq!(
            store.for_audience(&scope, &audience).expect("read").len(),
            1,
            "{} keeps its own register",
            audience.as_key()
        );
    }

    // Same id, different kind — genuinely separate.
    assert_ne!(
        store
            .for_audience(&scope, &AudienceRef::engagement("acme"))
            .expect("read")[0]
            .commitment_id,
        store
            .for_audience(&scope, &AudienceRef::account("acme"))
            .expect("read")[0]
            .commitment_id
    );
}

/// Audiences and scopes do not leak.
#[test]
fn registers_are_per_audience_and_per_scope() {
    let (_tmp, store, scope) = store();
    store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::OfferedToUs, "20%"),
            now(),
        )
        .expect("record");
    assert!(store
        .for_audience(&scope, &AudienceRef::engagement("eng-2"))
        .expect("other")
        .is_empty());
    assert!(store
        .for_audience(&CommitmentScope::new("someone-else", "default"), &eng())
        .expect("other scope")
        .is_empty());
}

/// The app/API path is revision-bound and returns the authoritative receipt
/// from the same log record. Retrying the exact decision changes neither the
/// destination head nor the original receipt timestamp.
#[test]
fn revision_bound_confirmation_is_receipted_and_exactly_idempotent() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(
            &scope,
            &terms("msg-cas", CommitmentDirection::StatedByUs, "ship Friday"),
            now(),
        )
        .expect("record");

    let first = store
        .confirm_at_revision(
            &scope,
            &eng(),
            &recorded.commitment_id,
            1,
            "decision-confirm-cas",
            "owner@example.com",
            now() + Duration::minutes(1),
        )
        .expect("confirm");
    assert_eq!(first.commitment.revision, 2);
    assert_eq!(first.commitment.status, CommitmentStatus::Confirmed);
    assert_eq!(first.receipt.verb, CommitmentDecisionVerb::Confirm);
    assert_eq!(
        first.receipt.disposition,
        CommitmentDecisionDisposition::Applied
    );
    assert_eq!(first.receipt.expected_revision, 1);
    assert_eq!(first.receipt.resulting_revision, 2);

    let replay = store
        .confirm_at_revision(
            &scope,
            &eng(),
            &recorded.commitment_id,
            1,
            "decision-confirm-cas",
            "owner@example.com",
            now() + Duration::days(1),
        )
        .expect("exact replay");
    assert_eq!(replay.commitment, first.commitment);
    assert_eq!(replay.receipt.receipt_id, first.receipt.receipt_id);
    assert_eq!(replay.receipt.recorded_at, first.receipt.recorded_at);
    assert_eq!(
        replay.receipt.disposition,
        CommitmentDecisionDisposition::AlreadyApplied
    );
}

/// An idempotency key binds the complete request. It must not turn changed
/// actor/revision/term input into a success response for the original action.
#[test]
fn commitment_decision_replay_refuses_substituted_input_and_stale_heads() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(
            &scope,
            &terms("msg-bind", CommitmentDirection::StatedByUs, "ship Friday"),
            now(),
        )
        .expect("record");
    store
        .confirm_at_revision(
            &scope,
            &eng(),
            &recorded.commitment_id,
            1,
            "decision-bind",
            "owner@example.com",
            now(),
        )
        .expect("first");

    let substituted = store
        .confirm_at_revision(
            &scope,
            &eng(),
            &recorded.commitment_id,
            1,
            "decision-bind",
            "somebody-else@example.com",
            now(),
        )
        .expect_err("same decision id with a different actor");
    assert!(substituted.to_string().contains("substituted"));

    let stale = store
        .confirm_at_revision(
            &scope,
            &eng(),
            &recorded.commitment_id,
            1,
            "decision-stale",
            "owner@example.com",
            now(),
        )
        .expect_err("different decision against the old head");
    assert!(stale.to_string().contains("stale commitment revision"));
}

#[test]
fn commitment_record_receipt_replays_but_changed_terms_do_not() {
    let (_tmp, store, scope) = store();
    let request = terms(
        "claim-act-1",
        CommitmentDirection::StatedByUs,
        "ship Friday",
    );
    let first = store
        .record_at_claim_revision(&scope, &request, 2, "decision-record", now())
        .expect("record");
    assert_eq!(first.commitment.status, CommitmentStatus::Unconfirmed);
    assert_eq!(first.receipt.verb, CommitmentDecisionVerb::Record);

    let replay = store
        .record_at_claim_revision_guarded(
            &scope,
            &request,
            2,
            "decision-record",
            now() + Duration::hours(1),
            || panic!("exact receipt replay must bypass write admission"),
        )
        .expect("replay");
    assert_eq!(replay.commitment, first.commitment);
    assert_eq!(
        replay.receipt.disposition,
        CommitmentDecisionDisposition::AlreadyApplied
    );

    let changed = terms(
        "claim-act-1",
        CommitmentDirection::StatedByUs,
        "ship Monday",
    );
    let error = store
        .record_at_claim_revision(&scope, &changed, 2, "decision-record", now())
        .expect_err("changed request under one decision id");
    assert!(error.to_string().contains("substituted"));
}

#[test]
fn guarded_destination_denial_precedes_commitment_writes() {
    let (_tmp, store, scope) = store();
    let request = terms(
        "claim-act-guarded",
        CommitmentDirection::StatedByUs,
        "ship Friday",
    );
    let denied = store
        .record_at_claim_revision_guarded(
            &scope,
            &request,
            2,
            "decision-record-guarded",
            now(),
            || anyhow::bail!("admission expired"),
        )
        .expect_err("a denied record cannot cross its first write boundary");
    assert!(denied.to_string().contains("admission expired"));
    assert!(store
        .for_audience(&scope, &request.audience)
        .expect("audience remains readable")
        .is_empty());
    assert!(store
        .recover_decision_receipt(
            &scope,
            "decision-record-guarded",
            CommitmentDecisionReceiptRequest::Record {
                request: &request,
                expected_claim_revision: 2,
            },
            now(),
        )
        .expect("denied decision remains readable")
        .is_none());

    let recorded = store
        .record_at_claim_revision(&scope, &request, 2, "decision-record-guarded", now())
        .expect("denial left no decision preparation behind");
    let denied = store
        .confirm_at_revision_guarded(
            &scope,
            &request.audience,
            &recorded.commitment.commitment_id,
            recorded.commitment.revision,
            "decision-confirm-guarded",
            "owner@example.com",
            now(),
            || anyhow::bail!("scope revoked"),
        )
        .expect_err("a denied confirmation cannot cross its first write boundary");
    assert!(denied.to_string().contains("scope revoked"));
    let held = store
        .load(
            &scope,
            &request.audience,
            &recorded.commitment.commitment_id,
        )
        .expect("load")
        .expect("commitment");
    assert_eq!(held.status, CommitmentStatus::Unconfirmed);
    assert!(store
        .recover_decision_receipt(
            &scope,
            "decision-confirm-guarded",
            CommitmentDecisionReceiptRequest::Confirm {
                audience: &request.audience,
                commitment_id: &recorded.commitment.commitment_id,
                expected_revision: recorded.commitment.revision,
                by: "owner@example.com",
            },
            now(),
        )
        .expect("denied confirmation remains readable")
        .is_none());
}

#[test]
fn decision_id_is_scope_wide_across_audience_shards() {
    let (_tmp, store, scope) = store();
    let first = terms(
        "claim-act-a",
        CommitmentDirection::StatedByUs,
        "ship Friday",
    );
    store
        .record_at_claim_revision(&scope, &first, 2, "decision-scope-wide", now())
        .expect("first audience");

    let mut substituted = terms(
        "claim-act-b",
        CommitmentDirection::StatedByUs,
        "ship Monday",
    );
    substituted.audience = AudienceRef::engagement("eng-2");
    let error = store
        .record_at_claim_revision(&scope, &substituted, 2, "decision-scope-wide", now())
        .expect_err("one decision id cannot move to another audience shard");
    assert!(error.to_string().contains("substituted"));
    assert!(store
        .for_audience(&scope, &substituted.audience)
        .expect("second audience")
        .is_empty());
}

/// Recovery mints nothing: no register row, no second receipt, no revision
/// bump. The one write it may make is the idempotent completion-journal repair
/// pinned by the test below, which announces a decision the register already
/// accepted rather than applying one.
#[test]
fn exact_receipt_recovery_mints_nothing_and_binds_audience_and_actor() {
    let (_tmp, store, scope) = store();
    let request = terms(
        "claim-act-recover",
        CommitmentDirection::StatedByUs,
        "ship Friday",
    );
    let recorded = store
        .record_at_claim_revision(&scope, &request, 2, "decision-record-recover", now())
        .expect("record");
    assert_eq!(recorded.receipt.audience.as_ref(), Some(&request.audience));
    assert_eq!(recorded.receipt.by, None);

    let recovered_record = store
        .recover_decision_receipt(
            &scope,
            "decision-record-recover",
            CommitmentDecisionReceiptRequest::Record {
                request: &request,
                expected_claim_revision: 2,
            },
            now() + Duration::minutes(2),
        )
        .expect("recover record")
        .expect("record receipt");
    assert_eq!(
        recovered_record.disposition,
        CommitmentDecisionDisposition::AlreadyApplied
    );
    assert_eq!(recovered_record.receipt_id, recorded.receipt.receipt_id);

    let confirmed = store
        .confirm_at_revision(
            &scope,
            &request.audience,
            &recorded.commitment.commitment_id,
            1,
            "decision-confirm-recover",
            "owner@example.com",
            now() + Duration::minutes(1),
        )
        .expect("confirm");
    assert_eq!(confirmed.receipt.audience.as_ref(), Some(&request.audience));
    assert_eq!(confirmed.receipt.by.as_deref(), Some("owner@example.com"));

    let recovered_confirm = store
        .recover_decision_receipt(
            &scope,
            "decision-confirm-recover",
            CommitmentDecisionReceiptRequest::Confirm {
                audience: &request.audience,
                commitment_id: &recorded.commitment.commitment_id,
                expected_revision: 1,
                by: "owner@example.com",
            },
            now() + Duration::minutes(3),
        )
        .expect("recover confirmation")
        .expect("confirmation receipt");
    assert_eq!(recovered_confirm.receipt_id, confirmed.receipt.receipt_id);
    assert_eq!(
        recovered_confirm.disposition,
        CommitmentDecisionDisposition::AlreadyApplied
    );

    let absent = store
        .recover_decision_receipt(
            &scope,
            "decision-never-applied",
            CommitmentDecisionReceiptRequest::Confirm {
                audience: &request.audience,
                commitment_id: &recorded.commitment.commitment_id,
                expected_revision: 1,
                by: "owner@example.com",
            },
            now() + Duration::minutes(4),
        )
        .expect("missing receipt is known absence");
    assert!(absent.is_none());
}

/// The crash window that only the recovery route can close.
///
/// The register row is authoritative and lands before its journal entry, so a
/// process that dies in between leaves a completion no cursor would surface.
/// The mutation path heals that on its replay branch — but a signed caller
/// whose admission lifetime elapsed may not re-enter it, and this route is what
/// it has left. If recovery journalled nothing the hole would be permanent, and
/// seq numbers stay dense either way (they are assigned at append), so nothing
/// downstream would ever detect it.
#[test]
fn recovery_repairs_a_journal_entry_the_mutation_path_can_no_longer_reach() {
    use crate::magician_v2::evidence::completion_journal::{
        EvidenceCompletionCursor, EvidenceCompletionJournal, EvidenceDecisionScope,
        EvidenceDecisionTarget,
    };

    let (_tmp, store, scope) = store();
    let request = terms(
        "claim-act-journal-recover",
        CommitmentDirection::StatedByUs,
        "ship Friday",
    );
    let recorded = store
        .record_at_claim_revision(&scope, &request, 1, "decision-journal-recover", now())
        .expect("record");

    let journal_root = store
        .workspace_layout
        .scope_root(&scope.principal, &scope.workspace)
        .join("decision_journal");
    std::fs::remove_dir_all(&journal_root)
        .expect("simulate a crash between the register row and its journal append");

    let journal = EvidenceCompletionJournal::new(store.workspace_layout.clone());
    let journal_scope = EvidenceDecisionScope::new("anonymous", "default");
    assert_eq!(journal.head(&journal_scope).expect("head").seq(), 0);

    let recovered = store
        .recover_decision_receipt(
            &scope,
            "decision-journal-recover",
            CommitmentDecisionReceiptRequest::Record {
                request: &request,
                expected_claim_revision: 1,
            },
            now() + Duration::days(5),
        )
        .expect("recover")
        .expect("the decision was applied");
    assert_eq!(
        recovered.disposition,
        CommitmentDecisionDisposition::AlreadyApplied
    );

    let page = journal
        .page_after(&journal_scope, EvidenceCompletionCursor::START, 10)
        .expect("page");
    assert_eq!(
        page.entries.len(),
        1,
        "the recovery route restored the completion the mutation path never journalled"
    );
    assert_eq!(page.entries[0].decision_id, "decision-journal-recover");
    assert_eq!(page.entries[0].receipt_id, recorded.receipt.receipt_id);
    assert_eq!(
        page.entries[0].target,
        EvidenceDecisionTarget::Commitment {
            audience: request.audience.clone(),
            commitment_id: recorded.commitment.commitment_id.clone(),
        },
        "the repaired entry carries the shard, or recovering its receipt means folding \
         every shard in the scope"
    );
    // The register's acceptance time, not the recovery's — the gap between the
    // two is the honest record that this entry was healed late.
    assert_eq!(page.entries[0].completed_at, recorded.receipt.recorded_at);
    assert_eq!(page.entries[0].journaled_at, now() + Duration::days(5));

    // Recovery is repeatable by construction, so it must not append twice.
    store
        .recover_decision_receipt(
            &scope,
            "decision-journal-recover",
            CommitmentDecisionReceiptRequest::Record {
                request: &request,
                expected_claim_revision: 1,
            },
            now() + Duration::days(6),
        )
        .expect("recover again")
        .expect("the decision was applied");
    assert_eq!(journal.head(&journal_scope).expect("head").seq(), 1);
}

#[test]
fn prepared_index_retry_finishes_first_write_and_scope_page_is_bounded() {
    let (_tmp, store, scope) = store();
    let request = terms(
        "claim-act-prepared",
        CommitmentDirection::StatedByUs,
        "ship Friday",
    );
    let recorded = store.record(&scope, &request, now()).expect("record");
    let decision_id = "decision-prepared-confirm";
    let fingerprint = super::confirm_request_fingerprint(
        &request.audience,
        &recorded.commitment_id,
        1,
        "owner@example.com",
    );
    // `bind_decision` is gone; a prepared intent is now an index built from the
    // same request the recovery below asks about. Going through the production
    // builder rather than hand-writing the struct is what makes this a test of
    // the prepared-intent path instead of a test of a literal.
    let prepared = store
        .decision_index_for_request(
            &scope,
            decision_id,
            CommitmentDecisionReceiptRequest::Confirm {
                audience: &request.audience,
                commitment_id: &recorded.commitment_id,
                expected_revision: 1,
                by: "owner@example.com",
            },
        )
        .expect("build prepared intent");
    assert_eq!(
        prepared.request_fingerprint, fingerprint,
        "the fingerprint helper and the index builder must agree"
    );
    store
        .write_decision_index(&scope, &prepared)
        .expect("persist prepared intent");
    assert!(store
        .recover_decision_receipt(
            &scope,
            decision_id,
            CommitmentDecisionReceiptRequest::Confirm {
                audience: &request.audience,
                commitment_id: &recorded.commitment_id,
                expected_revision: 1,
                by: "owner@example.com",
            },
            now(),
        )
        .expect("prepared recovery")
        .is_none());

    let applied = store
        .confirm_at_revision(
            &scope,
            &request.audience,
            &recorded.commitment_id,
            1,
            decision_id,
            "owner@example.com",
            now(),
        )
        .expect("same request resumes prepared first write");
    assert_eq!(applied.receipt.audience.as_ref(), Some(&request.audience));
    assert_eq!(applied.receipt.by.as_deref(), Some("owner@example.com"));
}

#[test]
fn confirmation_fold_is_single_transition_and_rejects_false_receipt_revisions() {
    let (_tmp, store, scope) = store();
    let request = terms(
        "claim-act-fold",
        CommitmentDirection::StatedByUs,
        "ship Friday",
    );
    let recorded = store.record(&scope, &request, now()).expect("record");
    store
        .confirm(
            &scope,
            &request.audience,
            &recorded.commitment_id,
            "first",
            now(),
        )
        .expect("confirm");
    store
        .append(
            &store.path(&scope, &request.audience),
            &CommitmentRecord::Confirmed {
                commitment_id: recorded.commitment_id.clone(),
                by: "second".to_string(),
                at: now() + Duration::minutes(1),
            },
        )
        .expect("append historical duplicate");
    let held = store
        .load(&scope, &request.audience, &recorded.commitment_id)
        .expect("load")
        .expect("commitment");
    assert_eq!(held.revision, 2);
    assert_eq!(held.confirmed_by.as_deref(), Some("first"));

    let other_request = terms(
        "claim-act-bad-receipt",
        CommitmentDirection::StatedByUs,
        "ship Monday",
    );
    let other = store
        .record_at_claim_revision(&scope, &other_request, 2, "decision-other-record", now())
        .expect("other record");
    let mut false_receipt = store
        .confirm_at_revision(
            &scope,
            &other_request.audience,
            &other.commitment.commitment_id,
            1,
            "decision-other-confirm",
            "owner",
            now(),
        )
        .expect("other confirm")
        .receipt;
    false_receipt.resulting_revision = 99;
    store
        .append(
            &store.path(&scope, &other_request.audience),
            &CommitmentRecord::ConfirmedWithReceipt {
                commitment_id: other.commitment.commitment_id.clone(),
                by: "owner".to_string(),
                at: now() + Duration::minutes(1),
                receipt: false_receipt,
            },
        )
        .expect("append corrupt receipt fixture");
    assert!(store
        .load(
            &scope,
            &other_request.audience,
            &other.commitment.commitment_id,
        )
        .expect_err("false resulting revision must fail closed")
        .to_string()
        .contains("receipt"));
}

/// E2's wiring on the commitment side. Every receipted transition lands in the
/// scope's completion journal, in completion order, exactly once.
///
/// The register's own decision index is named `blake3(decision_id)`, so walking
/// it yields hash order — an order a decision taken tomorrow can sort *behind*,
/// which is why the scope-wide receipt page built on it was lossy and removed.
/// The journal's order is assigned at append time instead.
#[test]
fn receipted_commitment_transitions_are_journalled_once_in_completion_order() {
    use crate::magician_v2::evidence::completion_journal::{
        EvidenceCompletionCursor, EvidenceCompletionJournal, EvidenceDecisionScope,
        EvidenceDecisionTarget,
    };

    let (_tmp, store, scope) = store();
    let request = terms(
        "claim-act-journal",
        CommitmentDirection::StatedByUs,
        "ship Friday",
    );
    let recorded = store
        .record_at_claim_revision(&scope, &request, 1, "decision-journal-record", now())
        .expect("record");
    let confirmed = store
        .confirm_at_revision(
            &scope,
            &request.audience,
            &recorded.commitment.commitment_id,
            1,
            "decision-journal-confirm",
            "owner@example.com",
            now() + Duration::minutes(1),
        )
        .expect("confirm");

    let journal = EvidenceCompletionJournal::new(store.workspace_layout.clone());
    let journal_scope = EvidenceDecisionScope::new("anonymous", "default");
    let page = journal
        .page_after(&journal_scope, EvidenceCompletionCursor::START, 10)
        .expect("page the journal");
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| (entry.seq, entry.decision_id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1, "decision-journal-record"),
            (2, "decision-journal-confirm"),
        ],
        "the cursor reads completion order, not identity order"
    );
    assert_eq!(page.entries[1].receipt_id, confirmed.receipt.receipt_id);
    assert_eq!(
        page.entries[0].target,
        EvidenceDecisionTarget::Commitment {
            audience: request.audience.clone(),
            commitment_id: recorded.commitment.commitment_id.clone(),
        },
        "the shard rides in the entry because recovering that receipt without it means \
         folding every shard in the scope"
    );
    assert!(!page.has_more);

    // An exact replay is `already_applied`. Journalling it again would hand the
    // projector the same completion twice.
    let replay = store
        .confirm_at_revision(
            &scope,
            &request.audience,
            &recorded.commitment.commitment_id,
            1,
            "decision-journal-confirm",
            "owner@example.com",
            now() + Duration::minutes(2),
        )
        .expect("exact replay");
    assert_eq!(
        replay.receipt.disposition,
        CommitmentDecisionDisposition::AlreadyApplied
    );
    assert_eq!(journal.head(&journal_scope).expect("head").seq(), 2);
}

// ── E4: the fold is indexed, bounded and abandonable ────────────────────────

/// A caller that has stopped waiting gets a refusal, not a short register.
///
/// The reopen this closes: the provider isolates this fold on a blocking
/// worker, but dropping the join handle when the outer timeout fires does not
/// stop the thread. Cancellation is the only thing that does — and it must
/// refuse rather than return what it had, because a commitment log is a log of
/// transitions: stopping before a `Withdrawn` record reports a dead term as
/// live, which is exactly the fail-open the register exists to prevent.
#[test]
fn a_cancelled_commitment_fold_refuses_rather_than_returning_a_partial_register() {
    let (_tmp, store, scope) = store();
    let recorded = store
        .record(
            &scope,
            &terms(
                "msg-1",
                CommitmentDirection::StatedByUs,
                "we deliver by March",
            ),
            now(),
        )
        .expect("record");
    store
        .withdraw(&scope, &eng(), &recorded.commitment_id, now())
        .expect("withdraw");

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = store
        .for_audience_until_cancelled(&scope, &eng(), &cancellation)
        .expect_err("a cancelled fold refuses");
    assert!(
        fold_was_cancelled(&error),
        "a cancelled read is the caller's own doing, not a register fault: {error}"
    );
}

/// Cancellation is the only thing the cancellable entry point changes. An
/// uncancelled fold answers exactly what the ordinary read answers, transitions
/// and order included — the indexed fold is a cost fix, not a new answer.
#[test]
fn an_uncancelled_fold_answers_exactly_what_the_ordinary_read_answers() {
    let (_tmp, store, scope) = store();
    let first = store
        .record(
            &scope,
            &terms("msg-1", CommitmentDirection::OfferedToUs, "they offer 20%"),
            now(),
        )
        .expect("theirs");
    let second = store
        .record(
            &scope,
            &terms(
                "msg-2",
                CommitmentDirection::StatedByUs,
                "we deliver by March",
            ),
            now(),
        )
        .expect("ours");
    store
        .confirm(&scope, &eng(), &second.commitment_id, "owner", now())
        .expect("confirm");
    store
        .withdraw(&scope, &eng(), &first.commitment_id, now())
        .expect("withdraw");

    let ordinary = store.for_audience(&scope, &eng()).expect("ordinary read");
    let cancellable = store
        .for_audience_until_cancelled(&scope, &eng(), &CancellationToken::new())
        .expect("a token nobody cancels");
    assert_eq!(ordinary, cancellable);

    // And the transitions the index has to carry actually landed, in append
    // order — the property a hash-keyed lookup would be free to lose.
    assert_eq!(
        ordinary
            .iter()
            .map(|held| (held.commitment_id.as_str(), held.status))
            .collect::<Vec<_>>(),
        vec![
            (first.commitment_id.as_str(), CommitmentStatus::Withdrawn),
            (second.commitment_id.as_str(), CommitmentStatus::Confirmed),
        ]
    );
}

/// A register the fold never reaches is still cancelled: the checkpoint before
/// the read means an abandoned fold does no I/O at all, empty shard or not.
#[test]
fn a_fold_cancelled_before_it_starts_refuses_on_an_empty_shard() {
    let (_tmp, store, scope) = store();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = store
        .for_audience_until_cancelled(&scope, &eng(), &cancellation)
        .expect_err("cancelled before the read");
    assert!(fold_was_cancelled(&error), "{error}");
}
