//! Module B's provenance contract, as behaviour.

use chrono::{Duration, TimeZone, Utc};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::{CarryingRevision, ClaimBinding, ClaimManifestScope, ClaimManifestStore};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn fixture() -> (tempfile::TempDir, ClaimManifestStore, ClaimManifestScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = ClaimManifestStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, ClaimManifestScope::new("anonymous", "default"))
}

fn binding(claim: &str, evidence: &[&str]) -> ClaimBinding {
    ClaimBinding {
        claim_ref: claim.to_string(),
        evidence_refs: evidence.iter().map(|held| held.to_string()).collect(),
    }
}

/// Module B: *"produce a persuasive artifact from a claim set, with
/// provenance"*. The manifest is the provenance: a bind reads back exactly
/// what was built, canonicalised — claims sorted by ref, evidence deduplicated
/// and sorted — so the same set always reads identically.
#[test]
fn bind_reads_back_exactly_what_was_built() {
    let (_tmp, store, scope) = fixture();
    let bound = store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![
                binding("claim-b", &["ev-2", "ev-1", "ev-2"]),
                binding("claim-a", &["ev-3"]),
            ],
            "company-assistant",
            now(),
        )
        .expect("bind");

    let expected_claims = vec![
        ClaimBinding {
            claim_ref: "claim-a".to_string(),
            evidence_refs: vec!["ev-3".to_string()],
        },
        ClaimBinding {
            claim_ref: "claim-b".to_string(),
            evidence_refs: vec!["ev-1".to_string(), "ev-2".to_string()],
        },
    ];
    assert_eq!(bound.artifact_ref, "deck-1");
    assert_eq!(bound.revision_ref, "rev-1");
    assert_eq!(bound.claims, expected_claims);
    assert_eq!(bound.bound_by, "company-assistant");
    assert_eq!(bound.bound_at, now());
    assert!(bound.manifest_id.starts_with("cmf-"));

    assert_eq!(
        store.manifest_for(&scope, "deck-1", "rev-1").expect("read"),
        Some(bound)
    );
}

/// Rebinding the same `(artifact, revision)` with identical claims —
/// order-insensitive — is an idempotent resume: derived ids make retries
/// resume instead of duplicate.
#[test]
fn rebinding_the_same_claims_in_any_order_resumes() {
    let (_tmp, store, scope) = fixture();
    let first = store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![
                binding("claim-b", &["ev-2", "ev-1"]),
                binding("claim-a", &["ev-3"]),
            ],
            "presto",
            now(),
        )
        .expect("first");

    // A retry hands the same set back reordered, with evidence reordered and
    // one ref repeated.
    let again = store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![
                binding("claim-a", &["ev-3", "ev-3"]),
                binding("claim-b", &["ev-1", "ev-2"]),
            ],
            "presto",
            now() + Duration::hours(2),
        )
        .expect("retry");

    assert_eq!(
        again, first,
        "the retry resumes the record it already wrote"
    );
    assert_eq!(
        again.bound_at,
        now(),
        "the retry must not move when the revision was bound"
    );
    assert_eq!(
        store
            .manifests_for_artifact(&scope, "deck-1")
            .expect("read"),
        vec![first]
    );
}

/// *"Revisions exist precisely so content cannot change under a reference —
/// the fix is a new revision."*
#[test]
fn a_revisions_claim_set_cannot_change_under_its_reference() {
    let (_tmp, store, scope) = fixture();
    let original = store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("bind");

    // A different claim set on the same reference: refused.
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"]), binding("claim-b", &["ev-2"])],
            "presto",
            now(),
        )
        .is_err());
    // The same claim on different evidence is a different claim set too.
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-9"])],
            "presto",
            now()
        )
        .is_err());

    // The refusals wrote nothing: the record still says exactly what rev-1
    // built, and the refused claim never entered the correction index.
    assert_eq!(
        store
            .manifests_for_artifact(&scope, "deck-1")
            .expect("read"),
        vec![original.clone()]
    );
    assert_eq!(
        store
            .revisions_carrying(&scope, "claim-b")
            .expect("carrying"),
        vec![]
    );

    // The sanctioned fix is a new revision.
    let fixed = store
        .bind(
            &scope,
            "deck-1",
            "rev-2",
            vec![binding("claim-a", &["ev-9"])],
            "presto",
            now(),
        )
        .expect("new revision");
    assert_ne!(fixed.manifest_id, original.manifest_id);
}

/// *"A claim in an artifact that cannot cite evidence is an invented figure
/// with a slide layout"* — a binding with no evidence refs is refused, and the
/// refusal writes nothing.
#[test]
fn a_claim_that_cites_no_evidence_is_refused() {
    let (_tmp, store, scope) = fixture();
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &[])],
            "presto",
            now()
        )
        .is_err());

    assert!(store
        .manifests_for_artifact(&scope, "deck-1")
        .expect("read")
        .is_empty());
    assert_eq!(
        store
            .revisions_carrying(&scope, "claim-a")
            .expect("carrying"),
        vec![]
    );
}

/// The vacuous-truth guard: a list of blank refs must not satisfy "cites
/// evidence" the way an empty list is refused for.
#[test]
fn blank_evidence_is_not_evidence() {
    let (_tmp, store, scope) = fixture();
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["   "])],
            "presto",
            now()
        )
        .is_err());
    assert!(store
        .manifests_for_artifact(&scope, "deck-1")
        .expect("read")
        .is_empty());
}

/// *"A manifest that binds nothing is decoration"* — refuse the empty claim
/// set.
#[test]
fn a_manifest_binding_no_claims_is_refused() {
    let (_tmp, store, scope) = fixture();
    assert!(store
        .bind(&scope, "deck-1", "rev-1", vec![], "presto", now())
        .is_err());
    assert!(store
        .manifests_for_artifact(&scope, "deck-1")
        .expect("read")
        .is_empty());
}

/// Binding one claim twice in a manifest would make "what evidence backs this
/// claim here" ambiguous, exactly when a correction is being propagated.
#[test]
fn one_claim_bound_twice_in_a_manifest_is_refused() {
    let (_tmp, store, scope) = fixture();
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"]), binding("claim-a", &["ev-2"])],
            "presto",
            now(),
        )
        .is_err());
    assert!(store
        .manifests_for_artifact(&scope, "deck-1")
        .expect("read")
        .is_empty());
}

/// Provenance that cannot answer who vouched for these claims is not
/// provenance.
#[test]
fn an_unnamed_binder_is_refused() {
    let (_tmp, store, scope) = fixture();
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "   ",
            now()
        )
        .is_err());
    assert!(store
        .manifests_for_artifact(&scope, "deck-1")
        .expect("read")
        .is_empty());
}

/// A blank artifact or revision ref is provenance nothing can ever look up.
#[test]
fn a_manifest_must_name_its_artifact_and_revision() {
    let (_tmp, store, scope) = fixture();
    assert!(store
        .bind(
            &scope,
            "  ",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now()
        )
        .is_err());
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "  ",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now()
        )
        .is_err());
}

/// The outward-assertions motivation names *"the decks that show it, or the
/// drafts still carrying it"* — and drafts are exactly what the sent-record
/// cannot find. When a claim is corrected, this query is the list of built
/// revisions to fix, found through the reverse index across every artifact.
#[test]
fn the_correction_query_finds_every_carrying_revision_across_artifacts() {
    let (_tmp, store, scope) = fixture();
    store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-arr", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("deck-1 rev-1");
    store
        .bind(
            &scope,
            "deck-1",
            "rev-2",
            vec![
                binding("claim-arr", &["ev-1", "ev-2"]),
                binding("claim-team", &["ev-9"]),
            ],
            "presto",
            now() + Duration::hours(1),
        )
        .expect("deck-1 rev-2");
    store
        .bind(
            &scope,
            "memo-1",
            "rev-1",
            vec![binding("claim-arr", &["ev-3"])],
            "presto",
            now(),
        )
        .expect("memo-1 rev-1");
    // An artifact that never carried the corrected claim.
    store
        .bind(
            &scope,
            "deck-2",
            "rev-1",
            vec![binding("claim-team", &["ev-9"])],
            "presto",
            now(),
        )
        .expect("deck-2 rev-1");

    assert_eq!(
        store
            .revisions_carrying(&scope, "claim-arr")
            .expect("carrying"),
        vec![
            CarryingRevision {
                artifact_ref: "deck-1".to_string(),
                revision_ref: "rev-1".to_string(),
                bound_at: now(),
                evidence_refs: vec!["ev-1".to_string()],
            },
            CarryingRevision {
                artifact_ref: "deck-1".to_string(),
                revision_ref: "rev-2".to_string(),
                bound_at: now() + Duration::hours(1),
                evidence_refs: vec!["ev-1".to_string(), "ev-2".to_string()],
            },
            CarryingRevision {
                artifact_ref: "memo-1".to_string(),
                revision_ref: "rev-1".to_string(),
                bound_at: now(),
                evidence_refs: vec!["ev-3".to_string()],
            },
        ]
    );

    assert_eq!(
        store
            .revisions_carrying(&scope, "claim-team")
            .expect("carrying"),
        vec![
            CarryingRevision {
                artifact_ref: "deck-1".to_string(),
                revision_ref: "rev-2".to_string(),
                bound_at: now() + Duration::hours(1),
                evidence_refs: vec!["ev-9".to_string()],
            },
            CarryingRevision {
                artifact_ref: "deck-2".to_string(),
                revision_ref: "rev-1".to_string(),
                bound_at: now(),
                evidence_refs: vec!["ev-9".to_string()],
            },
        ]
    );
}

/// *"'Still carrying it now' vs 'carried it once' are different questions; a
/// corrected deck whose new revision dropped the claim must NOT be flagged —
/// that is the fixed case."*
#[test]
fn latest_revision_separates_fixed_from_still_carrying() {
    let (_tmp, store, scope) = fixture();
    // deck-1 carried the claim, then a corrected revision dropped it: fixed.
    store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-arr", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("deck-1 rev-1");
    store
        .bind(
            &scope,
            "deck-1",
            "rev-2",
            vec![binding("claim-team", &["ev-2"])],
            "presto",
            now() + Duration::hours(1),
        )
        .expect("deck-1 rev-2");
    // memo-1's newest (only) revision still carries it.
    store
        .bind(
            &scope,
            "memo-1",
            "rev-1",
            vec![binding("claim-arr", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("memo-1 rev-1");
    // deck-2's newest revision GAINED the claim: flagged, however its history
    // started.
    store
        .bind(
            &scope,
            "deck-2",
            "rev-1",
            vec![binding("claim-team", &["ev-2"])],
            "presto",
            now(),
        )
        .expect("deck-2 rev-1");
    store
        .bind(
            &scope,
            "deck-2",
            "rev-2",
            vec![binding("claim-arr", &["ev-3"])],
            "presto",
            now() + Duration::hours(1),
        )
        .expect("deck-2 rev-2");

    assert_eq!(
        store
            .latest_revision_carrying(&scope, "claim-arr")
            .expect("latest"),
        vec!["deck-2".to_string(), "memo-1".to_string()],
        "the corrected deck is the fixed case and must not be flagged"
    );
    assert_eq!(
        store
            .latest_revision_carrying(&scope, "claim-team")
            .expect("latest"),
        vec!["deck-1".to_string()],
        "dropping one claim for another flips which lists an artifact is on"
    );

    // "Carried it once" is still on record: the correction query finds the old
    // revision even though the artifact has moved on.
    let once = store
        .revisions_carrying(&scope, "claim-arr")
        .expect("carrying");
    assert_eq!(once.len(), 3);
    assert!(once
        .iter()
        .any(|held| held.artifact_ref == "deck-1" && held.revision_ref == "rev-1"));
}

/// `manifests_for_artifact` is *"the revision history of what it claimed"* —
/// oldest first.
#[test]
fn an_artifacts_manifest_history_reads_oldest_first() {
    let (_tmp, store, scope) = fixture();
    store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("rev-1");
    store
        .bind(
            &scope,
            "deck-1",
            "rev-2",
            vec![binding("claim-a", &["ev-1"]), binding("claim-b", &["ev-2"])],
            "presto",
            now() + Duration::hours(1),
        )
        .expect("rev-2");
    store
        .bind(
            &scope,
            "deck-1",
            "rev-3",
            vec![binding("claim-b", &["ev-2"])],
            "presto",
            now() + Duration::hours(2),
        )
        .expect("rev-3");

    let history = store
        .manifests_for_artifact(&scope, "deck-1")
        .expect("history");
    assert_eq!(
        history
            .iter()
            .map(|held| held.revision_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["rev-1", "rev-2", "rev-3"]
    );
    assert_eq!(
        history.iter().map(|held| held.bound_at).collect::<Vec<_>>(),
        vec![
            now(),
            now() + Duration::hours(1),
            now() + Duration::hours(2)
        ]
    );
}

/// One tenant's provenance must never answer another tenant's question —
/// through the direct reads or the correction index.
#[test]
fn scopes_do_not_leak() {
    let (_tmp, store, scope) = fixture();
    store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("bind");

    let other_principal = ClaimManifestScope::new("someone-else", "default");
    assert_eq!(
        store
            .manifest_for(&other_principal, "deck-1", "rev-1")
            .expect("read"),
        None
    );
    assert!(store
        .manifests_for_artifact(&other_principal, "deck-1")
        .expect("read")
        .is_empty());
    assert_eq!(
        store
            .revisions_carrying(&other_principal, "claim-a")
            .expect("carrying"),
        vec![]
    );
    assert_eq!(
        store
            .latest_revision_carrying(&other_principal, "claim-a")
            .expect("latest"),
        Vec::<String>::new()
    );

    let other_workspace = ClaimManifestScope::new("anonymous", "second");
    assert!(store
        .manifests_for_artifact(&other_workspace, "deck-1")
        .expect("read")
        .is_empty());
}

/// A crash mid-append leaves a partial line, and only at the tail — this is
/// how the tests manufacture one.
fn append_raw(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open log for raw append");
    file.write_all(bytes).expect("raw append");
}

/// Pins the fail-open read: the first cut mapped EVERY read error — not just
/// NotFound — to "empty store", so an unreadable artifact log let a
/// content-DIFFERENT rebind of the same revision slip past the immutability
/// refusal and append a quiet sibling manifest. An unreadable log must be an
/// error, never an absence.
#[test]
fn an_unreadable_log_is_an_error_not_an_empty_store() {
    let (_tmp, store, scope) = fixture();
    store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("bind");

    // Make the artifact log unreadable: invalid UTF-8, as a torn disk write
    // would leave it.
    let path = store.manifest_path(&scope, "deck-1");
    std::fs::write(&path, b"\xFF\xFEnot utf-8").expect("corrupt log");

    let error = store
        .manifests_for_artifact(&scope, "deck-1")
        .expect_err("an unreadable log must not read as an empty one");
    assert!(
        format!("{error:#}").contains("an unreadable log must never be treated as an empty one"),
        "the error must say why: {error:#}"
    );

    // The immutability refusal must hold while the log is unreadable: a
    // content-different rebind of the same revision errors instead of
    // appending a second manifest under the same id.
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-b", &["ev-9"])],
            "presto",
            now()
        )
        .is_err());
}

/// The index half of the same fail-open: with the per-claim index unreadable,
/// the first cut returned an empty answer — "no decks carry this corrected
/// claim" as a confident wrong answer, the exact silent miss the index exists
/// to prevent. The read must fail instead.
#[test]
fn an_unreadable_index_fails_the_correction_query() {
    let (_tmp, store, scope) = fixture();
    store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("bind");

    let index_path = store.claim_index_path(&scope, "claim-a");
    std::fs::write(&index_path, b"\xFF\xFEnot utf-8").expect("corrupt index");

    let error = store
        .revisions_carrying(&scope, "claim-a")
        .expect_err("an unreadable index must not read as an empty one");
    assert!(
        format!("{error:#}").contains("an unreadable log must never be treated as an empty one"),
        "the error must say why: {error:#}"
    );
    assert!(store.latest_revision_carrying(&scope, "claim-a").is_err());
}

/// Pins the index tail-tolerance split: an unparseable FINAL index line is a
/// torn append — index-before-row means the manifest it would have named was
/// never written, so the query reads past it — while an unparseable INTERIOR
/// line cannot be produced by the write protocol and fails the read.
#[test]
fn a_torn_final_index_line_is_skipped_but_interior_corruption_fails() {
    let (_tmp, store, scope) = fixture();
    store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("bind");

    let index_path = store.claim_index_path(&scope, "claim-a");
    append_raw(&index_path, b"\"memo-");

    assert_eq!(
        store
            .revisions_carrying(&scope, "claim-a")
            .expect("tolerant tail"),
        vec![CarryingRevision {
            artifact_ref: "deck-1".to_string(),
            revision_ref: "rev-1".to_string(),
            bound_at: now(),
            evidence_refs: vec!["ev-1".to_string()],
        }]
    );
    assert_eq!(
        store
            .latest_revision_carrying(&scope, "claim-a")
            .expect("tolerant tail"),
        vec!["deck-1".to_string()]
    );

    // Complete the file past the tear: the garbage is now interior, which the
    // write protocol cannot produce — corruption, and the read refuses.
    append_raw(&index_path, b"\n\"memo-1\"\n");
    let error = store
        .revisions_carrying(&scope, "claim-a")
        .expect_err("interior corruption must fail the read");
    assert!(
        format!("{error:#}").contains("interior"),
        "the error must name it: {error:#}"
    );
}

/// Pins the manifest-log tail-tolerance: a partial final line is a bind that
/// never completed, so the history reads exactly what WAS bound — the retried
/// identical bind still resumes the original record, and a content-different
/// rebind is still refused, neither tripping over the tear.
#[test]
fn a_torn_final_manifest_line_reads_as_a_bind_that_never_happened() {
    let (_tmp, store, scope) = fixture();
    let original = store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now(),
        )
        .expect("bind");

    append_raw(
        &store.manifest_path(&scope, "deck-1"),
        b"{\"manifest_id\":\"cmf-torn",
    );

    assert_eq!(
        store
            .manifests_for_artifact(&scope, "deck-1")
            .expect("tolerant tail"),
        vec![original.clone()]
    );
    let again = store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-a", &["ev-1"])],
            "presto",
            now() + Duration::hours(1),
        )
        .expect("resume past the tear");
    assert_eq!(again, original);
    assert!(store
        .bind(
            &scope,
            "deck-1",
            "rev-1",
            vec![binding("claim-b", &["ev-2"])],
            "presto",
            now()
        )
        .is_err());
}
