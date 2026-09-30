//! The register's contract, as behaviour.
//!
//! Every test below names the exact failure it pins, because each one is a way
//! this module could hand one organisation's authority to another and then look
//! like it was working.

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceKind;

use crate::magician_v2::counterparty_store::{
    counterparty_id_for, identity_id_for, CounterpartyScope, CounterpartyStore,
};
use crate::magician_v2::counterparty_types::{
    AddIdentity, CreateCounterparty, IdentityKind, MergeDecision, MintSource, Promotion, Stage,
    TrustedSignal, Verification,
};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn store() -> (tempfile::TempDir, CounterpartyStore, CounterpartyScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = CounterpartyStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, store, CounterpartyScope::new("anonymous", "default"))
}

fn create(display_name: &str) -> CreateCounterparty {
    CreateCounterparty {
        display_name: display_name.to_string(),
        domain: None,
        stage: None,
        created_by: "owner".to_string(),
    }
}

fn address(counterparty_id: &str, kind: IdentityKind, value: &str) -> AddIdentity {
    AddIdentity {
        counterparty_id: counterparty_id.to_string(),
        kind,
        value: value.to_string(),
        source: MintSource::OwnerStated,
        evidence_ref: "msg-1".to_string(),
        recorded_by: "company-assistant".to_string(),
        introduced_by: None,
    }
}

/// A promotion whose signal a server would stand behind: an authenticated
/// request on a channel whose transport establishes the sender.
fn owner_promotion(decided_by: &str, evidence_ref: &str) -> Promotion {
    Promotion {
        decided_by: decided_by.to_string(),
        evidence_ref: evidence_ref.to_string(),
        signal: TrustedSignal::new("web", evidence_ref).authenticated(true),
    }
}

fn merge_decision() -> MergeDecision {
    MergeDecision {
        decided_by: "owner".to_string(),
        evidence_ref: "companies-house-filing".to_string(),
    }
}

// ── Resolution exactness ────────────────────────────────────────────────────

/// **A near miss must resolve to nobody.** Resolution is exact match on the
/// normalised value; if a one-character difference, a `+tag`, a stripped dot or
/// a `www.` prefix resolved to a real organisation, a stranger's address would
/// inherit that organisation's context and the caller could not tell, because a
/// wrong answer here is shaped exactly like a right one.
#[test]
fn a_near_miss_address_resolves_to_nobody() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("record");
    store
        .add_identity(
            &scope,
            &address(&acme.counterparty_id, IdentityKind::Email, "Ops@Acme.com"),
            now(),
        )
        .expect("add");

    // The folds that ARE performed: case, across the whole address.
    for spelling in ["ops@acme.com", "OPS@ACME.COM", "  Ops@Acme.Com  "] {
        assert_eq!(
            store
                .resolve(&scope, IdentityKind::Email, spelling)
                .expect("resolve")
                .map(|found| found.counterparty_id),
            Some(acme.counterparty_id.clone()),
            "`{spelling}` is the same mailbox"
        );
    }

    // Every fold that is NOT performed. Each of these is a different mailbox.
    for near_miss in [
        "ops@acme.co",
        "ops@acrne.com",
        "ops@www.acme.com",
        "ops+billing@acme.com",
        "o.ps@acme.com",
        "opss@acme.com",
        "ops@acme.com.co",
    ] {
        assert_eq!(
            store
                .resolve(&scope, IdentityKind::Email, near_miss)
                .expect("resolve"),
            None,
            "`{near_miss}` is not `ops@acme.com` and must resolve to nobody"
        );
    }

    // And the kind is part of the identity: a domain that spells the same as
    // nothing on file is still nobody.
    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Domain, "acme.com")
            .expect("resolve"),
        None,
        "an email at acme.com does not make the DOMAIN acme.com a resolvable identity"
    );
}

/// **A bare national number is not its international form.** No country code is
/// ever inferred, so `4155550123` and `+14155550123` are two identities.
/// Guessing `+1` because the owner happens to be in North America would route a
/// stranger's number straight into a real counterparty's thread.
#[test]
fn a_bare_national_number_never_becomes_an_international_one() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("record");
    store
        .add_identity(
            &scope,
            &address(
                &acme.counterparty_id,
                IdentityKind::Phone,
                "+1 (415) 555-0123",
            ),
            now(),
        )
        .expect("add");

    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Phone, "+14155550123")
            .expect("resolve")
            .map(|found| found.counterparty_id),
        Some(acme.counterparty_id.clone()),
        "formatting is not a distinction"
    );
    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Phone, "4155550123")
            .expect("resolve"),
        None,
        "the national form is a different number until somebody records it as one"
    );
}

/// **The same name on two platforms is two organisations.** An unqualified
/// handle is not an address, and resolving one platform's `@acme` to another's
/// is the authority hand-off this module exists to prevent.
#[test]
fn one_handle_on_two_platforms_stays_two_identities() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    let other = store
        .record_counterparty(&scope, &create("Acme Holdings SA"), now())
        .expect("other");
    store
        .add_identity(
            &scope,
            &address(&acme.counterparty_id, IdentityKind::Handle, "x:@Acme"),
            now(),
        )
        .expect("add");

    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Handle, "X:acme")
            .expect("resolve")
            .map(|found| found.counterparty_id),
        Some(acme.counterparty_id.clone())
    );
    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Handle, "linkedin:acme")
            .expect("resolve"),
        None,
        "a different platform is a different organisation"
    );

    // And the other platform's handle may be filed under a genuinely different
    // organisation without colliding.
    let filed = store
        .add_identity(
            &scope,
            &address(
                &other.counterparty_id,
                IdentityKind::Handle,
                "linkedin:acme",
            ),
            now(),
        )
        .expect("second platform");
    assert_eq!(filed.counterparty_id, other.counterparty_id);
    assert_eq!(filed.normalised, "linkedin:acme");
}

/// **One address belongs to one organisation.** Two rows for one address would
/// make resolution ambiguous, and an ambiguous resolve is one counterparty's
/// authority landing on another. The second binding is refused, and the first
/// still resolves.
#[test]
fn an_address_cannot_be_filed_under_two_organisations() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    let rival = store
        .record_counterparty(&scope, &create("Rival Inc"), now())
        .expect("rival");
    store
        .add_identity(
            &scope,
            &address(&acme.counterparty_id, IdentityKind::Email, "ops@acme.com"),
            now(),
        )
        .expect("first");

    let error = store
        .add_identity(
            &scope,
            &address(&rival.counterparty_id, IdentityKind::Email, "ops@acme.com"),
            now(),
        )
        .expect_err("second binding");
    assert!(
        error.to_string().contains("authority"),
        "the refusal must say why: {error}"
    );
    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Email, "ops@acme.com")
            .expect("resolve")
            .map(|found| found.counterparty_id),
        Some(acme.counterparty_id),
        "the first binding survives the refused second"
    );
    let rival_book: Vec<String> = store
        .identities_for(&scope, &rival.counterparty_id)
        .expect("rival")
        .into_iter()
        .map(|identity| identity.normalised)
        .collect();
    assert_eq!(
        rival_book,
        Vec::<String>::new(),
        "the refused binding left nothing behind on the rival"
    );
}

// ── Promotion ───────────────────────────────────────────────────────────────

/// **A guess may not be promoted on the strength of the guess.**
/// Inferred-and-unverified is the default state of everything research
/// produces. If the research note could serve as its own proof, or if the
/// researcher could approve its own output, an automatic path would grant
/// itself the only control this module has — and one counterparty's authority
/// would reach another with nobody deciding.
#[test]
fn an_inferred_identity_needs_an_owner_decision_that_the_inference_did_not_author() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    let guessed = store
        .add_identity(
            &scope,
            &AddIdentity {
                source: MintSource::ResearchInferred,
                evidence_ref: "research-note-7".to_string(),
                recorded_by: "identity-research".to_string(),
                ..address(&acme.counterparty_id, IdentityKind::Email, "ceo@acme.com")
            },
            now(),
        )
        .expect("record the guess");
    assert_eq!(guessed.is_inferred(), true);
    assert_eq!(guessed.verification, Verification::Unverified);

    // 1. The research note cannot be its own proof.
    let self_proof = store
        .promote_identity(
            &scope,
            &guessed.identity_id,
            &owner_promotion("owner", "research-note-7"),
            now(),
        )
        .expect_err("self-proof");
    assert!(
        self_proof.to_string().contains("its own proof"),
        "{self_proof}"
    );

    // 2. The researcher cannot approve its own output.
    let self_approval = store
        .promote_identity(
            &scope,
            &guessed.identity_id,
            &owner_promotion("identity-research", "delivery-receipt-9"),
            now(),
        )
        .expect_err("self-approval");
    assert!(
        self_approval.to_string().contains("approve it"),
        "{self_approval}"
    );

    // Neither refusal moved anything.
    assert_eq!(
        store
            .load_identity(&scope, &guessed.identity_id)
            .expect("load")
            .expect("present")
            .verification,
        Verification::Unverified
    );

    // 3. A named owner, evidence the inference did not author, on a trusted
    //    channel — this is what a promotion costs.
    let promoted = store
        .promote_identity(
            &scope,
            &guessed.identity_id,
            &owner_promotion("owner", "delivery-receipt-9"),
            now() + Duration::hours(1),
        )
        .expect("owner decision");
    assert_eq!(
        promoted.verification,
        Verification::Verified {
            channel: "web".to_string(),
            evidence_ref: "delivery-receipt-9".to_string(),
            decided_by: "owner".to_string(),
            at: now() + Duration::hours(1),
        }
    );
    // Promotion does not rewrite where the address came from.
    assert_eq!(promoted.minted_by.source, MintSource::ResearchInferred);
    assert_eq!(promoted.minted_by.evidence_ref, "research-note-7");
}

/// **Verification fails closed on the channel.** The signal is decided by
/// `channel_is_verified`, not by this module and not by the caller: an
/// unclassified or unauthenticating transport is not proof, and a claim the
/// caller supplies about itself may de-escalate but may never raise.
#[test]
fn a_channel_nobody_can_vouch_for_never_verifies_an_address() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    let identity = store
        .add_identity(
            &scope,
            &address(&acme.counterparty_id, IdentityKind::Email, "ops@acme.com"),
            now(),
        )
        .expect("add");

    // SMTP does not authenticate `From:`.
    let by_email = Promotion {
        decided_by: "owner".to_string(),
        evidence_ref: "reply-1".to_string(),
        signal: TrustedSignal::new("email", "reply-1").authenticated(true),
    };
    // A channel nobody has classified.
    let by_unknown = Promotion {
        decided_by: "owner".to_string(),
        evidence_ref: "reply-1".to_string(),
        signal: TrustedSignal::new("carrier-pigeon", "reply-1").authenticated(true),
    };
    // The caller says "trust me". It never raises.
    let by_claim = Promotion {
        decided_by: "owner".to_string(),
        evidence_ref: "reply-1".to_string(),
        signal: TrustedSignal::new("web", "reply-1")
            .authenticated(false)
            .with_caller_claim(Some(true)),
    };
    // A trusted transport with nothing to point at.
    let by_nothing = Promotion {
        decided_by: "owner".to_string(),
        evidence_ref: "reply-1".to_string(),
        signal: TrustedSignal::new("web", "   ").authenticated(true),
    };

    for refused in [&by_email, &by_unknown, &by_claim, &by_nothing] {
        let error = store
            .promote_identity(&scope, &identity.identity_id, refused, now())
            .expect_err("untrusted signal");
        assert!(
            error.to_string().contains("not one a server stands behind"),
            "{error}"
        );
    }
    assert_eq!(
        store
            .load_identity(&scope, &identity.identity_id)
            .expect("load")
            .expect("present")
            .verification,
        Verification::Unverified
    );
    assert_eq!(
        store
            .resolve_verified(&scope, IdentityKind::Email, "ops@acme.com")
            .expect("resolve"),
        None,
        "an unproved address is a stranger to anything that grants"
    );
}

// ── Observation ─────────────────────────────────────────────────────────────

/// **An observation is not a promotion.** Seeing an address again is the same
/// unauthenticated claim repeated, so `observe` moves `last_seen` and touches
/// nothing else. A store that let volume ripen into trust would verify whoever
/// was noisiest.
#[test]
fn observing_an_address_advances_last_seen_and_nothing_else() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    let minted = store
        .add_identity(
            &scope,
            &AddIdentity {
                source: MintSource::ObservedOnInbound,
                evidence_ref: "message-1".to_string(),
                ..address(&acme.counterparty_id, IdentityKind::Email, "ops@acme.com")
            },
            now(),
        )
        .expect("add");

    let later = now() + Duration::days(4);
    let observed = store
        .observe(&scope, &minted.identity_id, later)
        .expect("observe");

    assert_eq!(observed.last_seen, later, "we heard from them on the 24th");
    assert_eq!(
        observed.first_seen,
        now(),
        "when we FIRST met them is set once and never moves"
    );
    assert_eq!(
        observed.verification,
        Verification::Unverified,
        "a hundred sightings are a hundred repetitions of one unproved claim"
    );
    assert_eq!(
        observed.minted_by, minted.minted_by,
        "provenance is untouched"
    );
    assert_eq!(
        store
            .resolve_verified(&scope, IdentityKind::Email, "ops@acme.com")
            .expect("resolve"),
        None,
        "being seen is not being proved"
    );
}

/// **`last_seen` only moves forward, and an unknown address is an error.** A
/// backfilled thread arriving out of order must not rewrite when we last heard
/// from an organisation — that is what a silence sweep reads — and observing an
/// address we never recorded must not mint one, because an address whose
/// provenance is "something mentioned it" is a rumour nobody can decide about.
#[test]
fn an_observation_never_runs_the_clock_backwards_or_mints_an_address() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    let minted = store
        .add_identity(
            &scope,
            &address(&acme.counterparty_id, IdentityKind::Email, "ops@acme.com"),
            now(),
        )
        .expect("add");
    store
        .observe(&scope, &minted.identity_id, now() + Duration::days(4))
        .expect("forward");

    let backfilled = store
        .observe(&scope, &minted.identity_id, now() + Duration::days(1))
        .expect("out of order");
    assert_eq!(
        backfilled.last_seen,
        now() + Duration::days(4),
        "an older sighting does not un-hear the newer one"
    );

    let error = store
        .observe(&scope, "idy-never-recorded", now())
        .expect_err("unknown address");
    assert!(
        error.to_string().contains("does not mint an address"),
        "{error}"
    );
    assert_eq!(
        store
            .load_identity(&scope, "idy-never-recorded")
            .expect("load"),
        None,
        "the refused observation minted nothing"
    );
}

// ── Merging ─────────────────────────────────────────────────────────────────

/// **The fold follows the merge edge.** A merge is an append, not a rewrite:
/// addresses keep saying which name they were filed under, while every read
/// answers with the organisation that survived. If reads did not follow the
/// edge, half a company's addresses would become invisible to the other half —
/// and the reply that arrives on the old one would be answered as a stranger's.
#[test]
fn a_merge_is_an_edge_the_fold_follows() {
    let (_tmp, store, scope) = store();
    let old = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("old");
    let new = store
        .record_counterparty(&scope, &create("Acme GmbH"), now() + Duration::minutes(1))
        .expect("new");
    store
        .add_identity(
            &scope,
            &address(&old.counterparty_id, IdentityKind::Email, "ops@acme.com"),
            now(),
        )
        .expect("old address");
    store
        .add_identity(
            &scope,
            &address(&new.counterparty_id, IdentityKind::Email, "ops@acme.de"),
            now(),
        )
        .expect("new address");

    let merged = store
        .merge(
            &scope,
            &old.counterparty_id,
            &new.counterparty_id,
            &merge_decision(),
            now() + Duration::days(1),
        )
        .expect("merge");
    assert_eq!(merged.merged_into, Some(new.counterparty_id.clone()));
    assert_eq!(merged.is_live(), false);

    // Resolution follows the edge, from either address.
    for value in ["ops@acme.com", "ops@acme.de"] {
        assert_eq!(
            store
                .resolve(&scope, IdentityKind::Email, value)
                .expect("resolve")
                .map(|found| found.counterparty_id),
            Some(new.counterparty_id.clone()),
            "`{value}` reaches the organisation that survived"
        );
    }

    // Both address books are now one, whichever id you ask with.
    let by_new: Vec<String> = store
        .identities_for(&scope, &new.counterparty_id)
        .expect("by new")
        .into_iter()
        .map(|identity| identity.normalised)
        .collect();
    let by_old: Vec<String> = store
        .identities_for(&scope, &old.counterparty_id)
        .expect("by old")
        .into_iter()
        .map(|identity| identity.normalised)
        .collect();
    assert_eq!(
        by_new,
        vec!["ops@acme.com".to_string(), "ops@acme.de".to_string()]
    );
    assert_eq!(by_old, by_new, "the old name is a route to the same book");

    // The identity still records the name it was filed under: an append-only
    // log keeps "we did not know these were one company" as a fact.
    let filed_under = store
        .resolve(&scope, IdentityKind::Email, "ops@acme.com")
        .expect("resolve")
        .expect("found");
    assert_eq!(filed_under.counterparty_id, new.counterparty_id);
    assert_eq!(
        store
            .load_identity(
                &scope,
                &identity_id_for(&scope, IdentityKind::Email, "ops@acme.com").expect("id")
            )
            .expect("load")
            .expect("present")
            .counterparty_id,
        old.counterparty_id
    );

    let summary = store
        .summary(&scope, &old.counterparty_id)
        .expect("summary");
    assert_eq!(summary.counterparty_id, new.counterparty_id);
    assert_eq!(summary.display_name, "Acme GmbH");
    assert_eq!(summary.identity_count, 2);
    assert_eq!(summary.verified_identity_count, 0);
    assert_eq!(summary.merged_in_count, 1);

    let live: Vec<String> = store
        .list(&scope)
        .expect("list")
        .into_iter()
        .map(|held| held.display_name)
        .collect();
    assert_eq!(
        live,
        vec!["Acme GmbH".to_string()],
        "a merged name is a name, not a second organisation in the book"
    );
}

/// **A merge cycle is refused, in both the obvious and the long form.** A cycle
/// has no head: a fold that walked one would either never terminate or stop on
/// whichever record it happened to reach, which means an organisation's mail
/// would route somewhere nobody chose.
#[test]
fn a_merge_that_would_close_a_cycle_is_refused() {
    let (_tmp, store, scope) = store();
    let a = store
        .record_counterparty(&scope, &create("Alpha"), now())
        .expect("a");
    let b = store
        .record_counterparty(&scope, &create("Beta"), now())
        .expect("b");
    let c = store
        .record_counterparty(&scope, &create("Gamma"), now())
        .expect("c");

    let self_merge = store
        .merge(
            &scope,
            &a.counterparty_id,
            &a.counterparty_id,
            &merge_decision(),
            now(),
        )
        .expect_err("self merge");
    assert!(self_merge.to_string().contains("cycle"), "{self_merge}");

    store
        .merge(
            &scope,
            &a.counterparty_id,
            &b.counterparty_id,
            &merge_decision(),
            now(),
        )
        .expect("a into b");
    store
        .merge(
            &scope,
            &b.counterparty_id,
            &c.counterparty_id,
            &merge_decision(),
            now(),
        )
        .expect("b into c");

    // The two-hop slip, and the three-hop one.
    let two_hop = store
        .merge(
            &scope,
            &c.counterparty_id,
            &b.counterparty_id,
            &merge_decision(),
            now(),
        )
        .expect_err("c into b");
    assert!(two_hop.to_string().contains("cycle"), "{two_hop}");
    let three_hop = store
        .merge(
            &scope,
            &c.counterparty_id,
            &a.counterparty_id,
            &merge_decision(),
            now(),
        )
        .expect_err("c into a");
    assert!(three_hop.to_string().contains("cycle"), "{three_hop}");

    // Nothing moved, and the chain still has exactly one head.
    assert_eq!(
        store
            .load(&scope, &c.counterparty_id)
            .expect("load")
            .expect("present")
            .merged_into,
        None
    );
    assert_eq!(
        store
            .summary(&scope, &a.counterparty_id)
            .expect("summary")
            .display_name,
        "Gamma"
    );
}

/// **Merged is terminal: it never merges again and never resurrects.** A second
/// edge would move an organisation's entire address book with nobody deciding
/// it should move, and re-recording the dead name would hand a caller an id it
/// would then file addresses under.
#[test]
fn a_merged_record_never_merges_again_or_comes_back() {
    let (_tmp, store, scope) = store();
    let a = store
        .record_counterparty(&scope, &create("Alpha"), now())
        .expect("a");
    let b = store
        .record_counterparty(&scope, &create("Beta"), now())
        .expect("b");
    let c = store
        .record_counterparty(&scope, &create("Gamma"), now())
        .expect("c");
    store
        .merge(
            &scope,
            &a.counterparty_id,
            &b.counterparty_id,
            &merge_decision(),
            now(),
        )
        .expect("a into b");

    // The identical merge resumes.
    let replay = store
        .merge(
            &scope,
            &a.counterparty_id,
            &b.counterparty_id,
            &merge_decision(),
            now() + Duration::days(1),
        )
        .expect("replay");
    assert_eq!(replay.merged_into, Some(b.counterparty_id.clone()));

    let second_edge = store
        .merge(
            &scope,
            &a.counterparty_id,
            &c.counterparty_id,
            &merge_decision(),
            now(),
        )
        .expect_err("second edge");
    assert!(
        second_edge.to_string().contains("never merges again"),
        "{second_edge}"
    );

    let resurrect = store
        .record_counterparty(&scope, &create("Alpha"), now())
        .expect_err("resurrection");
    assert!(
        resurrect.to_string().contains("never resurrects"),
        "{resurrect}"
    );

    let refile = store
        .add_identity(
            &scope,
            &address(&a.counterparty_id, IdentityKind::Email, "ops@alpha.com"),
            now(),
        )
        .expect_err("filing under a dead name");
    assert!(refile.to_string().contains("merged into"), "{refile}");

    assert_eq!(
        store
            .load(&scope, &a.counterparty_id)
            .expect("load")
            .expect("present")
            .merged_into,
        Some(b.counterparty_id),
        "the first edge is the edge"
    );
}

/// **A malformed merge chain must terminate as an error, not loop.** The store
/// refuses to create a cycle, but the register is folded from a log a second
/// process may be appending to and an older binary may have written. A reader
/// that walked a cycle would hang; one that stopped on an arbitrary record
/// would route an organisation's mail to whichever row the walk ended on.
#[test]
fn a_cycle_already_in_the_log_is_refused_rather_than_walked_forever() {
    use std::io::Write;

    let (_tmp, store, scope) = store();
    let a = store
        .record_counterparty(&scope, &create("Alpha"), now())
        .expect("a");
    let b = store
        .record_counterparty(&scope, &create("Beta"), now())
        .expect("b");
    store
        .add_identity(
            &scope,
            &address(&a.counterparty_id, IdentityKind::Email, "ops@alpha.com"),
            now(),
        )
        .expect("address");

    // Two edges no sequence of store calls could have produced.
    let path = store.register_path(&scope);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open register");
    for (from, into) in [
        (&a.counterparty_id, &b.counterparty_id),
        (&b.counterparty_id, &a.counterparty_id),
    ] {
        writeln!(
            file,
            "{{\"record\":\"merged\",\"from_counterparty_id\":\"{from}\",\
             \"into_counterparty_id\":\"{into}\",\"decided_by\":\"an-older-binary\",\
             \"evidence_ref\":\"none\",\"at\":\"2026-08-20T12:00:00Z\"}}"
        )
        .expect("write edge");
    }
    drop(file);

    let error = store
        .resolve(&scope, IdentityKind::Email, "ops@alpha.com")
        .expect_err("cycle in the log");
    assert!(error.to_string().contains("cycle"), "{error}");
    let summarised = store
        .summary(&scope, &a.counterparty_id)
        .expect_err("cycle in the log");
    assert!(summarised.to_string().contains("cycle"), "{summarised}");
    let listed = store
        .identities_for(&scope, &b.counterparty_id)
        .expect_err("cycle in the log");
    assert!(listed.to_string().contains("cycle"), "{listed}");
}

// ── Replay ──────────────────────────────────────────────────────────────────

/// **An identical replay resumes; a changed payload is an error, not a silent
/// no-op.** Every write here sits behind something retryable — a proxy, a
/// poller, a caller that never saw our response. A retry must not duplicate an
/// organisation or an address; but a retry that quietly kept the FIRST version
/// of a changed payload would leave the caller believing the register holds what
/// it just sent while the register holds something else.
#[test]
fn an_identical_replay_resumes_and_a_changed_one_is_refused() {
    let (_tmp, store, scope) = store();
    let first = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("first");
    let replay = store
        .record_counterparty(&scope, &create("  acme   ltd  "), now() + Duration::days(2))
        .expect("replay under another spelling");
    assert_eq!(replay.counterparty_id, first.counterparty_id);
    assert_eq!(
        replay.created_at,
        now(),
        "the replay did not restart the clock"
    );
    assert_eq!(
        replay.display_name, "Acme Ltd",
        "the first spelling is the one shown"
    );

    let changed_domain = store
        .record_counterparty(
            &scope,
            &CreateCounterparty {
                domain: Some("acme.com".to_string()),
                ..create("Acme Ltd")
            },
            now(),
        )
        .expect_err("changed domain");
    assert!(
        changed_domain.to_string().contains("set_domain"),
        "{changed_domain}"
    );

    let changed_stage = store
        .record_counterparty(
            &scope,
            &CreateCounterparty {
                stage: Some(Stage::new("active").expect("stage")),
                ..create("Acme Ltd")
            },
            now(),
        )
        .expect_err("changed stage");
    assert!(
        changed_stage.to_string().contains("set_stage"),
        "{changed_stage}"
    );

    let minted = store
        .add_identity(
            &scope,
            &address(&first.counterparty_id, IdentityKind::Email, "ops@acme.com"),
            now(),
        )
        .expect("first address");
    let address_replay = store
        .add_identity(
            &scope,
            &address(&first.counterparty_id, IdentityKind::Email, "OPS@acme.com"),
            now() + Duration::days(2),
        )
        .expect("address replay");
    assert_eq!(address_replay.identity_id, minted.identity_id);
    assert_eq!(
        address_replay.first_seen,
        now(),
        "a retry is the same sighting, not a new one"
    );

    let changed_provenance = store
        .add_identity(
            &scope,
            &AddIdentity {
                evidence_ref: "a-different-thread".to_string(),
                ..address(&first.counterparty_id, IdentityKind::Email, "ops@acme.com")
            },
            now(),
        )
        .expect_err("changed provenance");
    assert!(
        changed_provenance
            .to_string()
            .contains("different provenance"),
        "{changed_provenance}"
    );

    store
        .promote_identity(
            &scope,
            &minted.identity_id,
            &owner_promotion("owner", "receipt-1"),
            now(),
        )
        .expect("promote");
    let promotion_replay = store
        .promote_identity(
            &scope,
            &minted.identity_id,
            &owner_promotion("owner", "receipt-1"),
            now() + Duration::days(3),
        )
        .expect("promotion replay");
    assert_eq!(
        promotion_replay.verification,
        Verification::Verified {
            channel: "web".to_string(),
            evidence_ref: "receipt-1".to_string(),
            decided_by: "owner".to_string(),
            at: now(),
        },
        "the first promotion is the promotion, and its timestamp does not drift"
    );
    let changed_promotion = store
        .promote_identity(
            &scope,
            &minted.identity_id,
            &owner_promotion("someone-else", "receipt-2"),
            now(),
        )
        .expect_err("changed promotion");
    assert!(
        changed_promotion.to_string().contains("already verified"),
        "{changed_promotion}"
    );
}

/// **Derived ids, so a lost response is not a second row.** A caller that
/// recorded something and never saw our answer must be able to compute the same
/// handle again — otherwise every dropped connection leaves a duplicate
/// organisation, and half a company's addresses go to the copy.
#[test]
fn ids_are_derived_from_the_thing_not_assigned_to_it() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("record");
    assert_eq!(
        counterparty_id_for(&scope, "  ACME   Ltd ").expect("derive"),
        acme.counterparty_id,
        "the derivation and the write cannot drift"
    );

    let identity = store
        .add_identity(
            &scope,
            &address(&acme.counterparty_id, IdentityKind::Email, "Ops@Acme.com"),
            now(),
        )
        .expect("add");
    assert_eq!(
        identity_id_for(&scope, IdentityKind::Email, "ops@ACME.com").expect("derive"),
        identity.identity_id
    );
    // The kind is part of the id, so a handle and a domain that spell alike are
    // still two addresses.
    assert_ne!(
        identity_id_for(&scope, IdentityKind::Domain, "acme.com").expect("derive"),
        identity_id_for(&scope, IdentityKind::Handle, "x:acme.com").expect("derive")
    );
    // And so is the scope: one owner's book is not another's.
    assert_ne!(
        counterparty_id_for(
            &CounterpartyScope::new("someone-else", "default"),
            "Acme Ltd"
        )
        .expect("derive"),
        acme.counterparty_id
    );
}

/// **U+001F is refused wherever a caller string feeds a derived id.** The
/// separator is what keeps a derivation's components from bleeding into each
/// other; a component carrying it can fuse two components into one, and here
/// "two components" means two organisations sharing an id.
#[test]
fn the_separator_is_refused_in_every_string_that_derives_an_id() {
    let (_tmp, store, scope) = store();

    let name = store
        .record_counterparty(&scope, &create("Acme\u{1f}Ltd"), now())
        .expect_err("separator in a display name");
    assert!(name.to_string().contains("U+001F"), "{name}");

    let derived = counterparty_id_for(&scope, "Acme\u{1f}Ltd").expect_err("separator");
    assert!(derived.to_string().contains("U+001F"), "{derived}");

    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    let value = store
        .add_identity(
            &scope,
            &address(
                &acme.counterparty_id,
                IdentityKind::Email,
                "ops\u{1f}@acme.com",
            ),
            now(),
        )
        .expect_err("separator in an address");
    assert!(value.to_string().contains("U+001F"), "{value}");

    let poisoned_scope = CounterpartyScope::new("anon\u{1f}ymous", "default");
    let scoped = store
        .record_counterparty(&poisoned_scope, &create("Acme Ltd"), now())
        .expect_err("separator in a scope");
    assert!(scoped.to_string().contains("U+001F"), "{scoped}");

    // Nothing was written by any of the refusals.
    let names: Vec<String> = store
        .list(&scope)
        .expect("list")
        .into_iter()
        .map(|held| held.display_name)
        .collect();
    assert_eq!(names, vec!["Acme Ltd".to_string()]);
}

// ── Reading an empty register ───────────────────────────────────────────────

/// **A fold over an empty log answers "nothing", and nothing is never
/// permission.** An absent register means no organisation has been recorded —
/// so every lookup is `None`, every listing is empty, and every question about a
/// specific organisation is an ERROR rather than an empty answer. "We have never
/// heard of them" and "we know them and hold nothing" must not collapse into one
/// reply, or an unknown organisation reads as a known one with nothing owed.
#[test]
fn an_empty_register_answers_nothing_and_grants_nothing() {
    let (_tmp, store, scope) = store();

    assert_eq!(store.list(&scope).expect("list"), Vec::new());
    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Email, "ops@acme.com")
            .expect("resolve"),
        None
    );
    assert_eq!(
        store
            .resolve_verified(&scope, IdentityKind::Email, "ops@acme.com")
            .expect("resolve"),
        None
    );
    assert_eq!(
        store
            .candidates_by_domain(&scope, "acme.com")
            .expect("candidates"),
        Vec::new()
    );
    assert_eq!(store.load(&scope, "cp-nobody").expect("load"), None);
    assert_eq!(
        store.load_identity(&scope, "idy-nobody").expect("load"),
        None
    );

    for error in [
        store
            .identities_for(&scope, "cp-nobody")
            .expect_err("identities"),
        store
            .verified_identities_for(&scope, "cp-nobody")
            .expect_err("verified"),
        store
            .audience_for(&scope, "cp-nobody", AudienceKind::Engagement)
            .expect_err("audience"),
        store.summary(&scope, "cp-nobody").expect_err("summary"),
    ] {
        assert!(
            error.to_string().contains("no counterparty `cp-nobody`"),
            "{error}"
        );
    }
}

// ── The audience bridge ─────────────────────────────────────────────────────

/// **Only proved addresses cross into an audience, and an empty audience admits
/// nobody.** An audience is what admits somebody; putting an unproved address in
/// one is the authority hand-off this module exists to prevent. The second half
/// is the one that has bitten before: a counterparty with nothing verified
/// yields an audience with no members, and a membership check against an empty
/// set must FAIL, never pass vacuously.
#[test]
fn an_audience_carries_only_addresses_somebody_proved() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    let unproved = store
        .record_counterparty(&scope, &create("Nobody Proved Anything Ltd"), now())
        .expect("unproved");

    let proved = store
        .add_identity(
            &scope,
            &address(&acme.counterparty_id, IdentityKind::Email, "ceo@acme.com"),
            now(),
        )
        .expect("proved");
    store
        .add_identity(
            &scope,
            &address(&acme.counterparty_id, IdentityKind::Email, "ops@acme.com"),
            now(),
        )
        .expect("unproved address");
    store
        .add_identity(
            &scope,
            &address(
                &unproved.counterparty_id,
                IdentityKind::Email,
                "hi@nobody.example",
            ),
            now(),
        )
        .expect("their only address");
    store
        .promote_identity(
            &scope,
            &proved.identity_id,
            &owner_promotion("owner", "receipt-1"),
            now(),
        )
        .expect("promote");

    let audience = store
        .audience_for(&scope, &acme.counterparty_id, AudienceKind::Engagement)
        .expect("audience");
    assert_eq!(audience.identities, vec!["ceo@acme.com".to_string()]);
    assert_eq!(
        audience.reference.as_key(),
        format!("engagement:{}", acme.counterparty_id)
    );
    assert_eq!(audience.admits("ceo@acme.com", now()), true);
    assert_eq!(
        audience.admits("ops@acme.com", now()),
        false,
        "on file is not proved, and an audience admits only the proved"
    );

    // The vacuous-truth case: nothing verified, so nobody is admitted — not
    // even the address the register holds.
    let empty = store
        .audience_for(&scope, &unproved.counterparty_id, AudienceKind::Account)
        .expect("empty audience");
    assert_eq!(empty.identities, Vec::<String>::new());
    assert_eq!(empty.size(), 0);
    assert_eq!(
        empty.is_current(now()),
        true,
        "the relationship is live; emptiness is the only thing refusing admission"
    );
    assert_eq!(empty.admits("hi@nobody.example", now()), false);
    assert_eq!(empty.admits("anyone@anywhere.example", now()), false);

    let verified: Vec<String> = store
        .verified_identities_for(&scope, &acme.counterparty_id)
        .expect("verified")
        .into_iter()
        .map(|identity| identity.normalised)
        .collect();
    assert_eq!(verified, vec!["ceo@acme.com".to_string()]);
}

// ── Candidates ──────────────────────────────────────────────────────────────

/// **A domain match is a review list, never a resolution.** Holding a mailbox at
/// a large company proves somebody works there and nothing more. If a domain
/// candidate resolved automatically, one convincing lookalike registration would
/// walk into an existing counterparty's context. Candidates make the owner's
/// decision cheap; they do not make it.
#[test]
fn a_domain_yields_candidates_for_review_and_never_a_resolution() {
    let (_tmp, store, scope) = store();
    let by_hint = store
        .record_counterparty(
            &scope,
            &CreateCounterparty {
                domain: Some("Acme.com.".to_string()),
                ..create("Acme Ltd")
            },
            now(),
        )
        .expect("hint");
    let by_address = store
        .record_counterparty(&scope, &create("Acme Services BV"), now())
        .expect("address");
    let unrelated = store
        .record_counterparty(&scope, &create("Rival Inc"), now())
        .expect("rival");

    store
        .add_identity(
            &scope,
            &address(
                &by_address.counterparty_id,
                IdentityKind::Email,
                "support@acme.com",
            ),
            now(),
        )
        .expect("support");
    store
        .add_identity(
            &scope,
            &AddIdentity {
                source: MintSource::ResearchInferred,
                evidence_ref: "research-note-3".to_string(),
                recorded_by: "identity-research".to_string(),
                ..address(
                    &by_address.counterparty_id,
                    IdentityKind::Email,
                    "ceo@acme.com",
                )
            },
            now(),
        )
        .expect("guessed");
    store
        .add_identity(
            &scope,
            &address(
                &unrelated.counterparty_id,
                IdentityKind::Email,
                "ops@rival.com",
            ),
            now(),
        )
        .expect("rival address");

    let candidates = store
        .candidates_by_domain(&scope, "acme.com")
        .expect("candidates");
    let named: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.display_name.clone())
        .collect();
    assert_eq!(
        named,
        vec!["Acme Ltd".to_string(), "Acme Services BV".to_string()],
        "both the domain hint and the addresses in that domain are leads"
    );
    assert_eq!(candidates[0].counterparty_id, by_hint.counterparty_id);
    assert_eq!(candidates[0].identity_count, 0);
    assert_eq!(candidates[1].counterparty_id, by_address.counterparty_id);
    assert_eq!(candidates[1].identity_count, 2);
    assert_eq!(candidates[1].verified_identity_count, 0);
    assert_eq!(
        candidates[1].inferred_identity_count, 1,
        "the reviewer sees how much of this record is a guess"
    );

    // Two candidates, and still no resolution: an address nobody recorded is
    // nobody's, however familiar the domain looks.
    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Email, "someone-new@acme.com")
            .expect("resolve"),
        None
    );
    assert_eq!(
        store
            .resolve_verified(&scope, IdentityKind::Email, "support@acme.com")
            .expect("resolve"),
        None,
        "on the domain, on file, and still not proved"
    );

    // And an empty candidate list is "nothing to review", not "cleared".
    assert_eq!(
        store
            .candidates_by_domain(&scope, "lookalike-acme.com")
            .expect("candidates"),
        Vec::new()
    );
    assert_eq!(
        store
            .resolve(&scope, IdentityKind::Email, "ops@lookalike-acme.com")
            .expect("resolve"),
        None
    );
}

// ── Provenance shape ────────────────────────────────────────────────────────

/// **An introduction with no introducer is not an introduction.** The whole
/// value of the provenance is the person who vouched; recording the label
/// without them would leave a stronger-than-domain signal pointing at nobody.
/// The converse matters too: an introducer named on a source that was not an
/// introduction records a vouch nobody made.
#[test]
fn an_introduction_must_name_who_vouched_and_nothing_else_may() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");

    let unvouched = store
        .add_identity(
            &scope,
            &AddIdentity {
                source: MintSource::Introduced,
                ..address(&acme.counterparty_id, IdentityKind::Email, "ceo@acme.com")
            },
            now(),
        )
        .expect_err("introduction with no introducer");
    assert!(
        unvouched.to_string().contains("no introducer"),
        "{unvouched}"
    );

    let decorated = store
        .add_identity(
            &scope,
            &AddIdentity {
                introduced_by: Some("idy-a-friend".to_string()),
                ..address(&acme.counterparty_id, IdentityKind::Email, "ceo@acme.com")
            },
            now(),
        )
        .expect_err("introducer on a non-introduction");
    assert!(
        decorated.to_string().contains("only meaningful"),
        "{decorated}"
    );

    let introduced = store
        .add_identity(
            &scope,
            &AddIdentity {
                source: MintSource::Introduced,
                introduced_by: Some("idy-a-friend".to_string()),
                ..address(&acme.counterparty_id, IdentityKind::Email, "ceo@acme.com")
            },
            now(),
        )
        .expect("a real introduction");
    assert_eq!(introduced.introduced_by, Some("idy-a-friend".to_string()));
    assert_eq!(
        introduced.verification,
        Verification::Unverified,
        "a vouch is evidence, not proof"
    );
    assert_eq!(introduced.minted_by.source.implies_verified(), false);
}

/// **A stage is the label the owner last set, not a state machine.** Two
/// spellings of one label are one stage, so a store that treated case as a
/// distinction would show an owner two stages they never meant to have.
#[test]
fn a_stage_is_the_label_the_owner_last_set() {
    let (_tmp, store, scope) = store();
    let acme = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("acme");
    assert_eq!(acme.stage, None);

    let active = store
        .set_stage(
            &scope,
            &acme.counterparty_id,
            &Stage::new("In Diligence").expect("stage"),
            "owner",
            now(),
        )
        .expect("set");
    assert_eq!(
        active
            .stage
            .as_ref()
            .map(|stage| stage.as_str().to_string()),
        Some("In Diligence".to_string())
    );

    let restated = store
        .set_stage(
            &scope,
            &acme.counterparty_id,
            &Stage::new("in diligence").expect("stage"),
            "owner",
            now() + Duration::days(1),
        )
        .expect("restate");
    assert_eq!(
        restated
            .stage
            .as_ref()
            .map(|stage| stage.as_str().to_string()),
        Some("In Diligence".to_string()),
        "case is not a distinction an owner meant to draw"
    );

    let moved = store
        .set_stage(
            &scope,
            &acme.counterparty_id,
            &Stage::new("closed").expect("stage"),
            "owner",
            now() + Duration::days(2),
        )
        .expect("move");
    assert_eq!(
        moved.stage.as_ref().map(|stage| stage.as_str().to_string()),
        Some("closed".to_string())
    );
    assert_eq!(
        store
            .summary(&scope, &acme.counterparty_id)
            .expect("summary")
            .stage,
        Some(Stage::new("closed").expect("stage"))
    );
}

/// Which flows read this register, and which questions they ask.
///
/// # What this used to pin, and why it changed
///
/// It pinned a header sentence that had drifted the other way: the header named
/// *"scheduling, outbound envelopes, inbound routing, obligations, audiences"*
/// as flows that "each consume the same three answers", and none of the five
/// read this module at all. The one consumer was the data-room reader surface,
/// asking `audience_for` and nothing else.
///
/// That is no longer true, and the change is deliberate. Inbound identification
/// (`chat::inbound_sender`), the outbound minting leg
/// (`counterparties::outbound`) and the dispatch path (`executor`) now build a
/// store, so *"whose address is this"* and *"how do we reach them"* have live
/// callers. The list below is the current answer; a change to it is a change to
/// which flows can reach a counterparty, and should be read as one.
///
/// **The scan over the six modules stays as it was.** Those are the modules that
/// must NOT import this one — the inverted-coupling rule — and that half of the
/// property is unchanged by anything above.
#[test]
fn the_register_has_one_consumer_and_it_asks_only_about_verification() {
    use crate::magician_v2::doc_wiring_scan::{scan, scan_workspace, scan_workspace_subtree};

    // None of the six flows the old header named touches this module. The
    // scheduling owner now lives in magician-media, while the other five
    // remain in the core crate.
    let scheduling =
        scan_workspace_subtree("magician-media/src/scheduling", "counterparties::", &[]);
    assert!(scheduling.files_searched > 0);
    assert_eq!(scheduling.hits, Vec::<String>::new());

    for module in [
        "magician_v2/approval_envelopes",
        "magician_v2/obligations",
        "magician_v2/audience",
        "magician_v2/delivery",
        "magician_v2/suppression",
    ] {
        let found = scan(module, "counterparties::", &[]);
        assert!(
            found.files_searched > 0,
            "{module} produced no files to search, so this proves nothing"
        );
        assert_eq!(
            found.hits,
            Vec::<String>::new(),
            "{module} reads the counterparty register now; the header says no flow does"
        );
    }

    // Who builds a store, across the whole workspace.
    //
    // `magician-bin` builds the process-wide one and INSTALLS it — which is the
    // load-bearing half, and the half that was missing until 2026-08-21:
    // `install_global_counterparty_store` had zero callers anywhere, so every
    // consumer below reached `global_counterparty_store()`, got `None`, and
    // answered "this organisation has no addresses" for a register nobody had
    // opened. Each of the others is a real consumer seam.
    let builders = scan_workspace(
        "CounterpartyStore::new",
        &[
            "magician/src/magician_v2/counterparty_store_tests.rs",
            "magician/src/magician_v2/counterparty_consumers.rs",
        ],
    );
    assert!(
        builders.files_searched > 100,
        "only {} files were read, so this proves nothing",
        builders.files_searched
    );
    assert_eq!(
        builders.hits,
        vec![
            // The data room's reader surface, in its own `#[cfg(test)]`
            // fixture: the audience port's engagement-expiry tests drive a
            // REAL register rather than a mock roster, because a mock
            // implements `AudienceSource` itself and so never exercises
            // `CounterpartyAudiences` — the only implementation production
            // runs. The scan is textual and cannot tell a fixture from a flow,
            // so it is listed rather than skipped: skipping the file would
            // also hide a production build appearing there later.
            "magician-api/src/data_room_reader_api.rs".to_string(),
            "magician-bin/src/main.rs".to_string(),
            "magician/src/magician_v2/chat/inbound_authority.rs".to_string(),
            "magician/src/magician_v2/chat/inbound_sender.rs".to_string(),
            // Focused fixtures exercise the real register rather than a mock;
            // the textual scanner intentionally keeps those constructors
            // visible so a production use cannot hide in either module.
            "magician/src/magician_v2/contact_research/tests.rs".to_string(),
            "magician/src/magician_v2/execution/agentic/executor.rs".to_string(),
            "magician/src/magician_v2/introductions/tests.rs".to_string(),
            "magician/src/magician_v2/outbound.rs".to_string(),
        ],
        "the set of places building a `CounterpartyStore` changed — which is a change to \
         which flows can reach a counterparty, not a bookkeeping detail"
    );

    // And the process-wide handle is INSTALLED. A store that is built and never
    // installed is the exact defect above: every consumer resolves `None` and
    // reports an empty register as an empty organisation.
    let installers = scan_workspace(
        "install_global_counterparty_store",
        &[
            "magician/src/magician_v2/counterparty_store_tests.rs",
            "magician/src/magician_v2/counterparty_store.rs",
        ],
    );
    assert_eq!(
        installers.hits,
        vec!["magician-bin/src/main.rs".to_string()],
        "nothing installs the process-wide counterparty register, so every consumer of \
         `global_counterparty_store()` answers `None` — which reads as `this organisation has \
         no addresses` and is a broken process describing a healthy one"
    );

    // And the only question anybody asks it is the third one. `consumers.rs`
    // is skipped for the same reason the builder scan skips it: it is this
    // module's own re-shaping of `audience_for` (`audience_for_label`), not a
    // second flow reading the register.
    let askers = scan_workspace(
        ".audience_for(",
        &[
            "magician/src/magician_v2/counterparty_store_tests.rs",
            "magician/src/magician_v2/counterparty_consumers.rs",
        ],
    );
    assert_eq!(
        askers.hits,
        vec![
            // The owner's own view of a counterparty's reachable addresses.
            "magician-api/src/counterparties_api.rs".to_string(),
            // The one that decides who may READ a document, which is why this
            // list is worth pinning at all: a new entry here is a new thing
            // deciding who counts as inside a relationship.
            "magician-api/src/data_room_reader_api.rs".to_string(),
        ],
        "the set of flows resolving an audience changed"
    );

    // The label seam has exactly one caller outside this module, and it is the
    // engagement audience route. The header names it; a second one appearing
    // here means the sentence is out of date.
    let by_label = scan_workspace(
        "audience_for_label(",
        &[
            // This file, because the needle above is itself a non-comment line.
            "magician/src/magician_v2/counterparty_store_tests.rs",
            "magician/src/magician_v2/counterparty_consumers.rs",
            "magician/src/magician_v2/counterparty_store.rs",
        ],
    );
    assert!(
        by_label.files_searched > 100,
        "only {} files were read, so this proves nothing",
        by_label.files_searched
    );
    assert_eq!(
        by_label.hits,
        vec![
            "magician-api/src/engagements_api.rs".to_string(),
            // The dispatch boundary. It resolves the work's audience so the
            // envelope predicates have a real identity list to check a
            // recipient against — the empty `Vec` that used to sit there
            // refused every recipient of every act, which read exactly like a
            // gate working.
            "magician/src/magician_v2/execution/agentic/executor.rs".to_string(),
        ],
        "the set of flows resolving a work label to an audience changed"
    );
}

/// The batch resolver answers exactly what `resolve_label` answers, in one read.
///
/// It exists because the single form costs TWO register reads per label and its
/// natural caller is a loop — the inbound engagement lane resolves the label of
/// every live engagement to find which one a proved sender belongs to. That was
/// `2N` full reads and parses of the register on a chat request. Same answers,
/// one read; this pins "same answers" so the speed-up cannot quietly change
/// which engagement an inbound message reaches.
#[test]
fn resolving_labels_in_a_batch_agrees_with_resolving_them_one_at_a_time() {
    use crate::magician_v2::counterparties::resolve_label;

    let (_tmp, store, scope) = store();
    let old = store
        .record_counterparty(&scope, &create("Acme Ltd"), now())
        .expect("old");
    let new = store
        .record_counterparty(&scope, &create("Acme GmbH"), now() + Duration::minutes(1))
        .expect("new");
    store
        .merge(
            &scope,
            &old.counterparty_id,
            &new.counterparty_id,
            &merge_decision(),
            now() + Duration::days(1),
        )
        .expect("merge");

    let labels = vec![
        // The surviving name.
        "Acme GmbH".to_string(),
        // A name merged away: must answer with the record that SURVIVED, or an
        // engagement labelled with the old name stops reaching the counterparty
        // whose addresses it holds.
        "Acme Ltd".to_string(),
        // Case and spacing are not distinctions an owner meant to draw — the
        // comparison key is derived, so this is the same organisation.
        "  acme  gmbh ".to_string(),
        // Nobody has recorded this. `None`, not a near miss.
        "Globex".to_string(),
        // Malformed. `None` rather than an error: one engagement carrying a
        // blank label must not take inbound routing down for every other
        // engagement in the scope.
        "   ".to_string(),
    ];
    let batch = store.resolve_labels(&scope, &labels).expect("batch");

    for label in &labels {
        let single = resolve_label(&store, &scope, label)
            .ok()
            .and_then(|standing| standing.registered().cloned());
        assert_eq!(
            batch.get(label).cloned().flatten(),
            single,
            "`{label}` resolved differently in a batch than on its own"
        );
    }

    assert_eq!(
        batch
            .get("Acme Ltd")
            .cloned()
            .flatten()
            .map(|held| held.as_str().to_string()),
        Some(new.counterparty_id.clone()),
        "a merged-away name answers with the organisation that survived"
    );
    assert!(batch.get("Globex").cloned().flatten().is_none());
    assert!(batch.get("   ").cloned().flatten().is_none());
}
