//! The joins, as behaviour: real records in, decisions and reports out.
//!
//! Every test below SEEDS a store and asserts the seeded thing is readable
//! before asserting anything about how it is folded, withheld or refused. A
//! test over an empty store would pass against the exact bug this module
//! exists to fix — four conversions that were correct and had no caller, so
//! every fold ran over an empty collection and every assertion about the
//! result was vacuously true.

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::data_room::access_store::{AccessScope, AccessStore};
use crate::data_room::{
    AccessEvent, DataRoomScope, DataRoomStore, DocumentVisibility, GrantDisclosure, OpenDataRoom,
    UserAgentClass,
};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{Audience, AudienceRef};
use magician::magician_v2::evidence::outward_assertions::{
    OutwardAssertionStore, OutwardChannel, OutwardScope, PrepareOutwardAct,
};
use magician::magician_v2::learning::{
    LearningCandidateFilters, LearningCandidateState, LearningCandidateType, LearningScope,
    LearningStore,
};
use magician::magician_v2::share_links::{IssueShareLink, ShareLinkScope, ShareLinkStore};

use super::super::feeders::{EvidenceFloor, LearningTarget};
use super::super::proposal::NotProposable;
use super::super::retirement::RetirementPolicy;
use super::super::store::{OutcomeScope, OutcomeStore};
use super::super::types::{DeliveryState, OutcomeLabel, RecordOutcome};
use super::worker::{finalize_tick, OutcomeProposalConfig, OutcomeProposalHealthSnapshot};
use super::{
    candidate_id_for, claim_health, comparisons_in_scope, market_read, sweep_scope,
    CohortComparison, ComparisonWithheld, MaturitySweepStanding,
};

const PRINCIPAL: &str = "anonymous";
const WORKSPACE: &str = "default";
const VARIANT: &str = "outreach-opener";
const DECK: &str = "artifact://deck@3";
const FINANCIALS: &str = "artifact://financials@1";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap()
}

fn workspace() -> (tempfile::TempDir, ArtifactV2Workspace) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let layout = ArtifactV2Workspace::new(tmp.path());
    (tmp, layout)
}

fn outcome_scope() -> OutcomeScope {
    OutcomeScope::new(PRINCIPAL, WORKSPACE)
}

fn learning_scope() -> LearningScope {
    LearningScope::new(PRINCIPAL, WORKSPACE)
}

fn target() -> LearningTarget {
    LearningTarget::new(LearningCandidateType::WorkflowTemplate).in_scope(PRINCIPAL, WORKSPACE)
}

/// A sweep that is running and covers this tenant.
fn swept() -> MaturitySweepStanding {
    MaturitySweepStanding::new(vec![(PRINCIPAL.to_string(), WORKSPACE.to_string())], true)
}

/// One recorded outcome. `Silent` matures at the instant it is recorded, which
/// the store accepts and nothing earlier would.
fn record_outcome(
    store: &OutcomeStore,
    act_ref: &str,
    version: &str,
    label: OutcomeLabel,
    engagement: &str,
    at: DateTime<Utc>,
) {
    store
        .record(
            &outcome_scope(),
            &RecordOutcome {
                engagement_id: Some(engagement.to_string()),
                program_id: None,
                act_ref: act_ref.to_string(),
                variant_ref: VARIANT.to_string(),
                variant_version: version.to_string(),
                label,
                delivery_state: DeliveryState::Delivered,
                matured_at: label.requires_maturity().then_some(at),
                confounders: Vec::new(),
            },
            at,
        )
        .expect("the outcome records");
}

/// A cohort of `engaged` replies and `silent` matured silences, each from its
/// own counterparty.
fn seed_cohort(
    store: &OutcomeStore,
    version: &str,
    engaged: usize,
    silent: usize,
    at: DateTime<Utc>,
) {
    for index in 0..engaged {
        record_outcome(
            store,
            &format!("act-{version}-e{index}"),
            version,
            OutcomeLabel::Replied,
            &format!("cp-{version}-{index}"),
            at,
        );
    }
    for index in 0..silent {
        record_outcome(
            store,
            &format!("act-{version}-s{index}"),
            version,
            OutcomeLabel::Silent,
            &format!("cp-{version}-{}", engaged + index),
            at,
        );
    }
}

// ── 1. Cohort comparisons → the learning substrate ──────────────────────────

/// A cohort comparison reaches the learning substrate as a candidate an owner
/// can decide on.
///
/// Pins the failure the whole module is named after: `candidates_to_learning`
/// had no caller outside its own file, so every comparison this loop could have
/// made was a fold over an empty slice and no owner has ever been offered one.
/// The two cohorts are asserted to EXIST first — without that, every assertion
/// below would pass against a store holding nothing.
#[test]
fn a_cohort_comparison_becomes_a_candidate_an_owner_can_decide_on() {
    let (_tmp, layout) = workspace();
    let outcomes = OutcomeStore::new(layout.clone());
    seed_cohort(&outcomes, "v1", 1, 4, now() - Duration::days(20));
    seed_cohort(&outcomes, "v2", 4, 1, now() - Duration::days(10));

    // The evidence really exists. Without this the sweep assertions would pass
    // over two empty cohorts.
    assert_eq!(
        outcomes
            .cohort(&outcome_scope(), VARIANT, "v1")
            .expect("read v1")
            .len(),
        5
    );
    assert_eq!(
        outcomes
            .cohort(&outcome_scope(), VARIANT, "v2")
            .expect("read v2")
            .len(),
        5
    );

    let sweep = sweep_scope(
        &layout,
        &outcome_scope(),
        EvidenceFloor::default(),
        &target(),
        &swept(),
        now(),
    )
    .expect("the sweep runs");

    assert_eq!(sweep.considered, 1);
    assert_eq!(sweep.proposed.len(), 1);
    assert_eq!(sweep.withheld, Vec::new());

    let expected_id = candidate_id_for(
        &outcome_scope(),
        &CohortComparison {
            variant_ref: VARIANT.to_string(),
            baseline_version: "v1".to_string(),
            candidate_version: "v2".to_string(),
        },
    );
    assert_eq!(sweep.proposed, vec![expected_id.clone()]);

    let candidate = LearningStore::new(layout.clone())
        .read_candidate(&learning_scope(), &expected_id)
        .expect("the candidate is readable");
    assert_eq!(
        candidate.title,
        "outreach-opener: version `v1` compared with `v2`"
    );
    assert_eq!(candidate.summary, "engaged 1 of 5, then 4 of 5");
    assert_eq!(candidate.proposed_target.as_deref(), Some(VARIANT));
    assert_eq!(candidate.state, LearningCandidateState::Proposed);
    assert!(candidate.review_required);
    // Never a rate. A cohort of five turned into `0.8` reads as precision that
    // is not there.
    assert_eq!(candidate.confidence, None);
    // Every observation behind both cohorts — §9's "every proposal carries
    // sample size, confounders and evidence refs".
    assert_eq!(candidate.evidence_refs.len(), 10);
    assert!(candidate
        .evidence_refs
        .iter()
        .all(|reference| reference.kind == "outcome_observation"));
}

/// A comparison the evidence cannot support is withheld **with its counts**,
/// and nothing is filed.
///
/// Pins the difference between the two answers an owner can act on: *"we have
/// no evidence"* and *"we have three observations and the floor is five"* are
/// not the same sentence, and only one of them says what to do next.
#[test]
fn a_comparison_below_the_floor_is_withheld_carrying_its_sample_sizes() {
    let (_tmp, layout) = workspace();
    let outcomes = OutcomeStore::new(layout.clone());
    seed_cohort(&outcomes, "v1", 1, 2, now() - Duration::days(20));
    seed_cohort(&outcomes, "v2", 2, 1, now() - Duration::days(10));

    assert_eq!(
        outcomes
            .cohort(&outcome_scope(), VARIANT, "v1")
            .expect("read v1")
            .len(),
        3,
        "the seeded cohort must be readable, or the refusal below proves nothing"
    );

    let sweep = sweep_scope(
        &layout,
        &outcome_scope(),
        EvidenceFloor::default(),
        &target(),
        &swept(),
        now(),
    )
    .expect("the sweep runs");

    assert_eq!(sweep.proposed, Vec::<String>::new());
    assert_eq!(sweep.withheld.len(), 1);
    assert_eq!(sweep.withheld[0].variant_ref, VARIANT);
    assert_eq!(
        sweep.withheld[0].reason,
        ComparisonWithheld::NotProposable(NotProposable::SampleTooSmall {
            baseline: 3,
            candidate: 3,
            needed: 5,
        })
    );

    assert_eq!(
        LearningStore::new(layout)
            .list_candidates(&learning_scope(), LearningCandidateFilters::default())
            .expect("read the substrate")
            .len(),
        0,
        "nothing may be filed from a sample that cannot support it"
    );
}

/// A second pass files nothing, and a decision already made does not move when
/// the configuration around it changes.
///
/// Pins the bug this subsystem has already had once, in `mature_silences`:
/// deriving the answer before consulting what was recorded meant a settled fact
/// reported differently the moment somebody widened a window. Here the second
/// and third passes must both answer `AlreadyProposed` — the third under a
/// floor ten times stricter, which would have refused the comparison outright
/// had it been re-derived.
#[test]
fn a_second_pass_files_nothing_and_a_stricter_floor_cannot_move_the_decision() {
    let (_tmp, layout) = workspace();
    let outcomes = OutcomeStore::new(layout.clone());
    seed_cohort(&outcomes, "v1", 1, 4, now() - Duration::days(20));
    seed_cohort(&outcomes, "v2", 4, 1, now() - Duration::days(10));

    let first = sweep_scope(
        &layout,
        &outcome_scope(),
        EvidenceFloor::default(),
        &target(),
        &swept(),
        now(),
    )
    .expect("first pass");
    assert_eq!(first.proposed.len(), 1);
    let candidate_id = first.proposed[0].clone();

    let second = sweep_scope(
        &layout,
        &outcome_scope(),
        EvidenceFloor::default(),
        &target(),
        &swept(),
        now(),
    )
    .expect("second pass");
    assert_eq!(second.proposed, Vec::<String>::new());
    assert_eq!(
        second.withheld[0].reason,
        ComparisonWithheld::AlreadyProposed {
            candidate_id: candidate_id.clone(),
        }
    );

    // A floor of fifty would refuse this comparison as `SampleTooSmall` if the
    // decision were re-derived. It must not be.
    let third = sweep_scope(
        &layout,
        &outcome_scope(),
        EvidenceFloor::new(50, 20).expect("a stricter floor"),
        &target(),
        &swept(),
        now(),
    )
    .expect("third pass");
    assert_eq!(
        third.withheld[0].reason,
        ComparisonWithheld::AlreadyProposed { candidate_id },
        "a decision already recorded must not change because a floor did"
    );

    assert_eq!(
        LearningStore::new(layout)
            .list_candidates(&learning_scope(), LearningCandidateFilters::default())
            .expect("read the substrate")
            .len(),
        1,
        "three passes over one comparison are one candidate"
    );
}

/// With no maturity sweep behind it, a comparison is withheld carrying the
/// sample sizes it would have used.
///
/// Pins the survivorship bias the maturity phase exists to prevent, in the
/// direction that arrives by omission: a store that only ever receives replies
/// is indistinguishable from a world where everybody answers, so a cohort read
/// out of one is not evidence about a variant. The counts still travel, because
/// *"five observations and no silence sweep"* is what tells an owner which
/// switch to turn on.
#[test]
fn without_a_maturity_sweep_a_comparison_is_withheld_with_its_sample_sizes() {
    let (_tmp, layout) = workspace();
    let outcomes = OutcomeStore::new(layout.clone());
    seed_cohort(&outcomes, "v1", 1, 4, now() - Duration::days(20));
    seed_cohort(&outcomes, "v2", 4, 1, now() - Duration::days(10));

    let sweep = sweep_scope(
        &layout,
        &outcome_scope(),
        EvidenceFloor::default(),
        &target(),
        &MaturitySweepStanding::never_swept(),
        now(),
    )
    .expect("the sweep runs");

    assert_eq!(sweep.proposed, Vec::<String>::new());
    assert_eq!(
        sweep.withheld[0].reason,
        ComparisonWithheld::SilenceNeverSwept {
            baseline_usable: 5,
            candidate_usable: 5,
        }
    );
    assert_eq!(
        LearningStore::new(layout)
            .list_candidates(&learning_scope(), LearningCandidateFilters::default())
            .expect("read the substrate")
            .len(),
        0
    );
}

/// A sweep configured over a roster that does not name this tenant covers
/// nothing here.
///
/// Pins the membership test that must not pass vacuously: an empty roster with
/// a completed tick is not coverage of everybody, and a covered roster with no
/// completed tick is not coverage at all.
#[test]
fn coverage_needs_both_the_roster_and_a_completed_tick() {
    let covered =
        MaturitySweepStanding::new(vec![(PRINCIPAL.to_string(), WORKSPACE.to_string())], true);
    assert!(covered.covers(PRINCIPAL, WORKSPACE));
    assert!(!covered.covers(PRINCIPAL, "second"));

    let never_ticked =
        MaturitySweepStanding::new(vec![(PRINCIPAL.to_string(), WORKSPACE.to_string())], false);
    assert!(!never_ticked.covers(PRINCIPAL, WORKSPACE));

    let empty_roster = MaturitySweepStanding::new(Vec::new(), true);
    assert!(!empty_roster.covers(PRINCIPAL, WORKSPACE));
    assert!(!MaturitySweepStanding::never_swept().covers(PRINCIPAL, WORKSPACE));
}

/// Comparisons pair consecutive versions in the order the store first saw
/// outcomes for each.
///
/// Pins that a variant with three versions produces two comparisons and not
/// three: pairing every version against every other would offer an owner the
/// same evidence twice under a different heading, and pairing only the first
/// and last would hide the change that happened in between.
#[test]
fn comparisons_pair_consecutive_versions_oldest_first() {
    let (_tmp, layout) = workspace();
    let outcomes = OutcomeStore::new(layout.clone());
    seed_cohort(&outcomes, "v3", 1, 0, now() - Duration::days(5));
    seed_cohort(&outcomes, "v1", 1, 0, now() - Duration::days(30));
    seed_cohort(&outcomes, "v2", 1, 0, now() - Duration::days(20));

    let recorded = outcomes
        .recorded_cohorts(&outcome_scope())
        .expect("the cohorts are readable");
    assert_eq!(recorded.len(), 3, "three cohorts must be on disk");
    assert_eq!(
        recorded
            .iter()
            .map(|cohort| cohort.variant_version.as_str())
            .collect::<Vec<_>>(),
        vec!["v1", "v2", "v3"],
        "ordered by first observation, not by the order they were written"
    );

    assert_eq!(
        comparisons_in_scope(&outcomes, &outcome_scope()).expect("comparisons"),
        vec![
            CohortComparison {
                variant_ref: VARIANT.to_string(),
                baseline_version: "v1".to_string(),
                candidate_version: "v2".to_string(),
            },
            CohortComparison {
                variant_ref: VARIANT.to_string(),
                baseline_version: "v2".to_string(),
                candidate_version: "v3".to_string(),
            },
        ]
    );
}

/// One version alone yields no comparison.
///
/// Pins the self-confirmation refusal: a cohort has nothing to be compared
/// against until a second version exists, and comparing it with itself would
/// let the loop agree with whatever it already does.
#[test]
fn a_single_version_produces_no_comparison() {
    let (_tmp, layout) = workspace();
    let outcomes = OutcomeStore::new(layout.clone());
    seed_cohort(&outcomes, "v1", 3, 2, now() - Duration::days(20));

    assert_eq!(
        outcomes
            .recorded_cohorts(&outcome_scope())
            .expect("cohorts")
            .len(),
        1
    );
    assert_eq!(
        comparisons_in_scope(&outcomes, &outcome_scope()).expect("comparisons"),
        Vec::new()
    );
}

// ── 2. Rooms and their access lane → the market read ────────────────────────

fn seed_room(
    layout: &ArtifactV2Workspace,
    audience: &AudienceRef,
    holders: &[&str],
    documents: &[&str],
    at: DateTime<Utc>,
) -> String {
    let rooms = DataRoomStore::new(layout.clone());
    let scope = DataRoomScope::new(PRINCIPAL, WORKSPACE);
    let room = rooms
        .open(
            &scope,
            &OpenDataRoom {
                audience: audience.clone(),
                opened_by: "owner@ours.test".to_string(),
                closes_at: None,
            },
            at,
        )
        .expect("open the room");

    let links = ShareLinkStore::new(layout.clone());
    for holder in holders {
        links
            .issue(
                &ShareLinkScope::new(PRINCIPAL, WORKSPACE),
                &IssueShareLink {
                    resource_ref: room.room_id.clone(),
                    audience: audience.clone(),
                    issued_to: (*holder).to_string(),
                    secret: format!("secret-for-{holder}"),
                    expires_at: at + Duration::days(60),
                },
                at,
            )
            .expect("issue the grant");
    }

    let assertions = OutwardAssertionStore::new(layout.clone());
    let roster = Audience::new(
        audience.clone(),
        holders.iter().map(|held| (*held).to_string()).collect(),
    );
    let holder_names: Vec<String> = holders.iter().map(|held| (*held).to_string()).collect();
    for document in documents {
        rooms
            .add_document(
                &scope,
                &room.room_id,
                document,
                DocumentVisibility::Everyone,
                "owner@ours.test",
                &GrantDisclosure {
                    assertions: &assertions,
                    audience: &roster,
                    holders: &holder_names,
                    disclosed_by: "owner@ours.test",
                },
                at,
            )
            .expect("add the document");
    }
    room.room_id
}

fn record_access(
    layout: &ArtifactV2Workspace,
    room_id: &str,
    audience: &AudienceRef,
    token: &str,
    document: Option<&str>,
    sequence: u32,
    at: DateTime<Utc>,
) {
    AccessStore::new(layout.clone())
        .record_access(
            &AccessScope::new(PRINCIPAL, WORKSPACE),
            room_id,
            &AccessEvent {
                room_id: room_id.to_string(),
                audience: audience.clone(),
                token_issued_to: token.to_string(),
                document_ref: document.map(str::to_string),
                occurred_at: at,
                dwell_ms: None,
                sequence,
                user_agent_class: UserAgentClass::Unknown,
            },
        )
        .expect("the presentation records");
}

/// A share that never appeared stays in the market read's denominator, and is
/// reported as a delivery question.
///
/// Pins the inflation this feeder exists to prevent: dropping the people who
/// never came would report a room half of whose shares went silent as fully
/// read, and *"opened by one of two"* would become *"opened by one of one"* —
/// the one number a positioning read must not be able to flatter itself with.
#[test]
fn a_share_that_never_appeared_stays_in_the_denominator() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::engagement("acme");
    let room_id = seed_room(
        &layout,
        &audience,
        &["reader-a", "reader-b"],
        &[DECK],
        now() - Duration::days(3),
    );
    record_access(
        &layout,
        &room_id,
        &audience,
        "reader-a",
        Some(DECK),
        1,
        now() - Duration::days(2),
    );

    // The visit really happened, and both grants really exist.
    assert_eq!(
        AccessStore::new(layout.clone())
            .events_for(&AccessScope::new(PRINCIPAL, WORKSPACE), &room_id)
            .expect("events")
            .len(),
        1
    );
    assert_eq!(
        ShareLinkStore::new(layout.clone())
            .for_resource(&ShareLinkScope::new(PRINCIPAL, WORKSPACE), &room_id)
            .expect("grants")
            .len(),
        2
    );

    let read = market_read(&layout, &outcome_scope()).expect("the market read");
    assert_eq!(read.rooms_seen, 1);
    assert_eq!(read.shares_seen, 2);
    assert_eq!(read.documents.len(), 1);
    assert_eq!(read.documents[0].read.document, DECK);
    assert_eq!(read.documents[0].read.opened_by, 1);
    assert_eq!(
        read.documents[0].read.shared_with, 2,
        "the share that never appeared is the denominator's whole point"
    );
    assert_eq!(read.documents[0].read.audiences, 1);
    // One audience is a fact about that audience, not about the market.
    assert!(!read.documents[0].market_backed);
    assert_eq!(
        read.never_opened,
        vec![(audience, "reader-b".to_string())],
        "a share that never appeared is a delivery question, not a nudge"
    );
    assert_eq!(read.unattributed, Vec::new());
}

/// One visit that clicks twice is one visit; two visits are two.
///
/// Pins visits against clicks. The distinction lives in the event `sequence`
/// and nothing here re-derives it — counting events instead would report
/// *"came back to it"* about somebody who came once and opened two things,
/// which is the difference between interest and a single read.
#[test]
fn a_visit_that_opens_two_things_is_still_one_visit() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::engagement("acme");
    let room_id = seed_room(
        &layout,
        &audience,
        &["reader-a"],
        &[DECK, FINANCIALS],
        now() - Duration::days(3),
    );
    // One sitting: the index, then two documents. Three events, one sequence.
    record_access(
        &layout,
        &room_id,
        &audience,
        "reader-a",
        None,
        1,
        now() - Duration::days(2),
    );
    record_access(
        &layout,
        &room_id,
        &audience,
        "reader-a",
        Some(DECK),
        1,
        now() - Duration::days(2) + Duration::minutes(1),
    );
    record_access(
        &layout,
        &room_id,
        &audience,
        "reader-a",
        Some(FINANCIALS),
        1,
        now() - Duration::days(2) + Duration::minutes(2),
    );

    assert_eq!(
        AccessStore::new(layout.clone())
            .events_for(&AccessScope::new(PRINCIPAL, WORKSPACE), &room_id)
            .expect("events")
            .len(),
        3,
        "three events must be on disk, or the visit assertion proves nothing"
    );

    let read = market_read(&layout, &outcome_scope()).expect("the market read");
    assert_eq!(
        read.returned,
        Vec::new(),
        "three clicks in one sitting is one visit, and one visit is not a return"
    );
    assert_eq!(read.never_opened, Vec::new());

    // A second sitting IS a return.
    record_access(
        &layout,
        &room_id,
        &audience,
        "reader-a",
        Some(DECK),
        2,
        now() - Duration::days(1),
    );
    let read = market_read(&layout, &outcome_scope()).expect("the market read");
    assert_eq!(read.returned, vec![(audience, "reader-a".to_string())]);
}

/// An access event whose token holds no grant is reported, never counted.
///
/// Pins the rule that the grant ledger is the authority on who was shared:
/// counting a forwarded link's presentation as a reader would inflate
/// `opened_by` past the number of people we ever shared with, and the report
/// would read as engagement we never measured.
#[test]
fn an_event_from_a_token_nobody_was_granted_is_reported_not_counted() {
    let (_tmp, layout) = workspace();
    let audience = AudienceRef::engagement("acme");
    let room_id = seed_room(
        &layout,
        &audience,
        &["reader-a"],
        &[DECK],
        now() - Duration::days(3),
    );
    record_access(
        &layout,
        &room_id,
        &audience,
        "stranger",
        Some(DECK),
        1,
        now() - Duration::days(2),
    );

    assert_eq!(
        AccessStore::new(layout.clone())
            .events_for(&AccessScope::new(PRINCIPAL, WORKSPACE), &room_id)
            .expect("events")
            .len(),
        1,
        "the stranger's presentation must be on disk"
    );

    let read = market_read(&layout, &outcome_scope()).expect("the market read");
    assert_eq!(read.shares_seen, 1);
    assert_eq!(read.documents[0].read.opened_by, 0);
    assert_eq!(read.documents[0].read.shared_with, 1);
    assert_eq!(read.unattributed.len(), 1);
    assert_eq!(read.unattributed[0].token_issued_to, "stranger");
    assert_eq!(
        read.never_opened,
        vec![(audience, "reader-a".to_string())],
        "the granted reader still never came, whatever the stranger did"
    );
}

/// A scope with no rooms reads as nothing, and nothing passes the market floor.
///
/// Pins the vacuous case: an empty read must not be mistaken for a market that
/// found everything unremarkable.
#[test]
fn an_empty_scope_has_no_market_read_and_no_market_backing() {
    let (_tmp, layout) = workspace();
    let read = market_read(&layout, &outcome_scope()).expect("the market read");
    assert_eq!(read.rooms_seen, 0);
    assert_eq!(read.shares_seen, 0);
    assert_eq!(read.documents.len(), 0);
    assert_eq!(read.never_opened, Vec::new());
    assert_eq!(read.returned, Vec::new());
    assert_eq!(read.unattributed, Vec::new());
}

// ── 3. The assertion reverse index → stale and contradicted claims ──────────

fn outward_scope() -> OutwardScope {
    OutwardScope::new(PRINCIPAL, WORKSPACE)
}

/// One recorded act that asserted one claim to one audience. Returns the
/// assertion use id, which is what a supersession names.
fn seed_assertion(
    store: &OutwardAssertionStore,
    key: &str,
    claim_ref: &str,
    audience: &str,
    supersedes: &[String],
    at: DateTime<Utc>,
) -> String {
    let stamp = at.to_rfc3339();
    let act = store
        .prepare(
            &outward_scope(),
            &PrepareOutwardAct {
                idempotency_key: key.to_string(),
                program_id: None,
                engagement_id: Some("acme".to_string()),
                exact_payload_artifact_ref: format!("artifact://{key}@1"),
                effective_sender: "owner@ours.test".to_string(),
                intended_audience: vec![audience.to_string()],
                channel: OutwardChannel::Email,
                consequence_class: "outward_message".to_string(),
            },
            &stamp,
        )
        .expect("the act records");
    store
        .record_assertion_use(
            &outward_scope(),
            &act.outward_act_ref,
            claim_ref,
            audience,
            &[],
            supersedes,
            &stamp,
        )
        .expect("the assertion use records")
        .assertion_use_id
}

/// Asking about a correction finds every act that carried the figure it
/// replaced, after it was replaced.
///
/// Pins the translation `claim_uses_from_index` documents and nothing called:
/// an index row's `supersedes` names assertion **uses** and retirement compares
/// **claims**, so passing the ids through unchanged compiles and matches
/// nothing — no supersession is ever recognised and no contradiction is ever
/// raised. It also pins the closure: the superseded use lives under a different
/// claim's axis, so a caller reading one claim's index alone would leave the
/// pointer unresolved and answer that everything is fine.
#[test]
fn a_correction_finds_every_audience_still_told_the_old_figure() {
    let (_tmp, layout) = workspace();
    let store = OutwardAssertionStore::new(layout.clone());

    let old_use = seed_assertion(
        &store,
        "act-old",
        "claim-revenue-v1",
        "investor-a",
        &[],
        now() - Duration::days(10),
    );
    seed_assertion(
        &store,
        "act-correction",
        "claim-revenue-v2",
        "investor-b",
        std::slice::from_ref(&old_use),
        now() - Duration::days(5),
    );
    let stale_again = seed_assertion(
        &store,
        "act-relapse",
        "claim-revenue-v1",
        "investor-c",
        &[],
        now() - Duration::days(2),
    );

    // The record really holds all three, and the correction really points at
    // the use it replaces.
    assert_eq!(
        store
            .index_entries(
                &outward_scope(),
                magician::magician_v2::evidence::outward_assertions::CLAIM_AXIS,
                "claim-revenue-v1"
            )
            .expect("index")
            .len(),
        2
    );

    let health = claim_health(
        &store,
        &outward_scope(),
        &["claim-revenue-v2".to_string()],
        &RetirementPolicy::new(Duration::days(180)).expect("policy"),
        now(),
    )
    .expect("the report builds");

    assert_eq!(health.claims_asked, vec!["claim-revenue-v2".to_string()]);
    assert_eq!(
        health.claims_reached,
        vec![
            "claim-revenue-v1".to_string(),
            "claim-revenue-v2".to_string()
        ],
        "the closure must pull in the superseded claim's WHOLE history"
    );
    assert_eq!(health.uses_seen, 3);
    assert!(health.complete);
    assert_eq!(health.unresolved, Vec::new());

    assert_eq!(health.contradictions.len(), 1);
    let contradiction = &health.contradictions[0];
    assert_eq!(contradiction.stale_claim, "claim-revenue-v1");
    assert_eq!(contradiction.superseding_claim, "claim-revenue-v2");
    assert_eq!(contradiction.superseded_at, now() - Duration::days(5));
    assert_eq!(contradiction.audiences_told_stale, 1);
    assert_eq!(contradiction.offending_uses.len(), 1);
    assert_eq!(contradiction.offending_uses[0].use_ref, stale_again);
    assert_eq!(contradiction.offending_uses[0].audience, "investor-c");
}

/// Asking only about the stale claim finds nothing, and the report says what
/// it looked at.
///
/// Pins a limitation rather than a behaviour, deliberately: a supersession is
/// declared on the newer claim's row and the index has no reverse pointer, so
/// walking forward from a stale claim reaches nothing. The report is truthful
/// about the claims it reached, which is what stops the empty answer being read
/// as a clean bill of health.
#[test]
fn asking_only_about_the_stale_claim_reaches_only_the_stale_claim() {
    let (_tmp, layout) = workspace();
    let store = OutwardAssertionStore::new(layout.clone());
    let old_use = seed_assertion(
        &store,
        "act-old",
        "claim-revenue-v1",
        "investor-a",
        &[],
        now() - Duration::days(10),
    );
    seed_assertion(
        &store,
        "act-correction",
        "claim-revenue-v2",
        "investor-b",
        std::slice::from_ref(&old_use),
        now() - Duration::days(5),
    );
    seed_assertion(
        &store,
        "act-relapse",
        "claim-revenue-v1",
        "investor-c",
        &[],
        now() - Duration::days(2),
    );

    let health = claim_health(
        &store,
        &outward_scope(),
        &["claim-revenue-v1".to_string()],
        &RetirementPolicy::new(Duration::days(180)).expect("policy"),
        now(),
    )
    .expect("the report builds");

    assert_eq!(health.uses_seen, 2, "only the stale claim's own two uses");
    assert_eq!(
        health.claims_reached,
        vec!["claim-revenue-v1".to_string()],
        "the correction is not reachable from the claim it corrects"
    );
    assert_eq!(health.contradictions, Vec::new());
}

/// A claim nobody has asserted inside the window is nominated, with the uses it
/// was counted from.
///
/// Pins §8's structural control: a candidate that cannot cite its uses is not
/// made, so the receipts travel with the count.
#[test]
fn a_claim_unused_past_the_window_is_nominated_with_its_receipts() {
    let (_tmp, layout) = workspace();
    let store = OutwardAssertionStore::new(layout.clone());
    let use_ref = seed_assertion(
        &store,
        "act-pricing",
        "claim-pricing-v1",
        "investor-a",
        &[],
        now() - Duration::days(200),
    );

    assert_eq!(
        store
            .load_assertion_use(&outward_scope(), &use_ref)
            .expect("read")
            .expect("the use exists")
            .approved_claim_ref,
        "claim-pricing-v1"
    );

    let health = claim_health(
        &store,
        &outward_scope(),
        &["claim-pricing-v1".to_string()],
        &RetirementPolicy::new(Duration::days(180)).expect("policy"),
        now(),
    )
    .expect("the report builds");

    assert_eq!(health.retirement.len(), 1);
    assert_eq!(health.retirement[0].claim_ref, "claim-pricing-v1");
    assert_eq!(health.retirement[0].total_uses, 1);
    assert_eq!(health.retirement[0].evidence_refs, vec![use_ref]);
    assert_eq!(health.retirement[0].idle_for.num_days(), 200);
    assert!(health.complete);

    // The same claim is NOT nominated under a window it has not outlived.
    let fresh = claim_health(
        &store,
        &outward_scope(),
        &["claim-pricing-v1".to_string()],
        &RetirementPolicy::new(Duration::days(365)).expect("policy"),
        now(),
    )
    .expect("the report builds");
    assert_eq!(fresh.retirement.len(), 0);
}

/// A report over no claims is refused rather than answered clean.
///
/// Pins the vacuous pass: `retirement_candidates` over an empty slice returns
/// an empty list, which reads exactly like *"nothing is stale"*. An empty
/// question must not produce a reassuring answer.
#[test]
fn a_report_over_no_claims_is_refused() {
    let (_tmp, layout) = workspace();
    let store = OutwardAssertionStore::new(layout);
    let error = claim_health(
        &store,
        &outward_scope(),
        &[],
        &RetirementPolicy::new(Duration::days(180)).expect("policy"),
        now(),
    )
    .expect_err("an empty claim list is refused");
    assert!(
        format!("{error}").contains("would answer `nothing is stale"),
        "the refusal must say why an empty pass is not a clean one: {error}"
    );
}

/// A claim ref carrying the field separator is refused before it reaches an
/// index path.
///
/// Pins the rule this codebase applies at every id derivation: U+001F is what
/// joins a derived id's components, so a value carrying it could address a
/// record it does not name.
#[test]
fn a_claim_ref_carrying_the_field_separator_is_refused() {
    let (_tmp, layout) = workspace();
    let store = OutwardAssertionStore::new(layout);
    let error = claim_health(
        &store,
        &outward_scope(),
        &["claim\u{1f}forged".to_string()],
        &RetirementPolicy::new(Duration::days(180)).expect("policy"),
        now(),
    )
    .expect_err("a crafted claim ref is refused");
    assert!(
        format!("{error}").contains("U+001F"),
        "the refusal must name the character: {error}"
    );
}

// ── The tick's three zeros ──────────────────────────────────────────────────

/// A pass that saw no scope is degraded, not idle.
///
/// Pins the vacuous green: an empty roster and a roster nobody could read
/// produce the same count, and reporting that as a quiet day is how a loop runs
/// for a year having done nothing while every dashboard reads healthy.
#[test]
fn a_pass_over_no_scopes_is_degraded() {
    let mut snapshot = OutcomeProposalHealthSnapshot::configured(&OutcomeProposalConfig::default());
    finalize_tick(&mut snapshot, "2026-08-21T12:00:00Z");
    assert_eq!(snapshot.state, "degraded");
    assert!(snapshot
        .last_error
        .as_deref()
        .expect("a reason")
        .contains("no scope was named"));
    assert_eq!(snapshot.last_success_at, None);
}

/// A pass with scopes but no maturity sweep behind any of them is degraded.
///
/// Pins the structurally-mute tick: the loop is running, every comparison is
/// withheld, and nothing about the counts alone would say so.
#[test]
fn a_pass_with_no_silence_recorded_anywhere_is_degraded() {
    let mut snapshot = OutcomeProposalHealthSnapshot::configured(&OutcomeProposalConfig::default());
    snapshot.scopes_seen = 2;
    snapshot.scopes_with_silence_swept = 0;
    finalize_tick(&mut snapshot, "2026-08-21T12:00:00Z");
    assert_eq!(snapshot.state, "degraded");
    assert!(snapshot
        .last_error
        .as_deref()
        .expect("a reason")
        .contains("structurally cannot say anything"));
}

/// A pass that swept real scopes and proposed nothing is idle.
///
/// Pins the ordinary case against the two above: cohorts that are not big
/// enough yet, or comparisons already decided, are a quiet day and not a fault.
#[test]
fn a_pass_that_proposed_nothing_over_swept_scopes_is_idle() {
    let mut snapshot = OutcomeProposalHealthSnapshot::configured(&OutcomeProposalConfig::default());
    snapshot.scopes_seen = 1;
    snapshot.scopes_with_silence_swept = 1;
    finalize_tick(&mut snapshot, "2026-08-21T12:00:00Z");
    assert_eq!(snapshot.state, "idle");
    assert_eq!(
        snapshot.last_success_at.as_deref(),
        Some("2026-08-21T12:00:00Z")
    );
    assert_eq!(snapshot.last_error, None);
}

/// A floor of zero cannot be configured.
///
/// Pins the predicate that would pass over an empty collection: `0 >= 0` is
/// satisfied by a cohort with nothing in it, so a comparison of nothing against
/// nothing would arrive as a finding.
#[test]
fn a_configured_floor_of_zero_refuses_to_start() {
    let config = OutcomeProposalConfig {
        minimum_usable: 0,
        minimum_counterparties: 0,
        ..OutcomeProposalConfig::default()
    };
    let error = config.floor().expect_err("a floor of zero is refused");
    assert!(
        format!("{error}").contains("vacuously"),
        "the refusal must name the vacuous pass: {error}"
    );
}
