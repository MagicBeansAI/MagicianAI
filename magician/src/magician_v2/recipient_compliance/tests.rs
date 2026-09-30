//! §9B's four below-the-agent recipient checks, as behaviour.
//!
//! Each test names the failure it pins. Several of them assert **both**
//! directions in one test on purpose: a gate whose passing half can be deleted
//! without a test going red is a gate that becomes a hundred-percent refusal,
//! and a gate whose refusing half can be deleted is a guard-shaped hole.

use chrono::TimeZone;
use std::sync::Arc;

use crate::magician_v2::evidence::outward_assertions::PrepareOutwardAct;
use crate::magician_v2::suppression::{SuppressionEvidence, SuppressionReason};
use magician_media::scheduling::{
    absorb_recipient_compliance_reply_for_test, close_recipient_compliance_negotiation_for_test,
    open_recipient_compliance_negotiation_for_test, read_recipient_compliance_negotiations_json,
    ReplyKind, Slot,
};

use super::*;

struct TestSchedulingReader;

impl ComplianceSchedulingReader for TestSchedulingReader {
    fn negotiations(
        &self,
        workspace_layout: &ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<Vec<ComplianceNegotiation>> {
        let encoded = read_recipient_compliance_negotiations_json(
            workspace_layout.base_root(),
            principal,
            workspace,
        )?;
        Ok(serde_json::from_slice(&encoded)?)
    }
}

fn install_recipient_compliance_scheduling_reader() {
    let _ = install_compliance_scheduling_reader(Arc::new(TestSchedulingReader));
}

fn read_recipient_compliance_negotiations(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> anyhow::Result<Vec<ComplianceNegotiation>> {
    let encoded =
        read_recipient_compliance_negotiations_json(layout.base_root(), principal, workspace)?;
    Ok(serde_json::from_slice(&encoded)?)
}

// ── Fixtures ────────────────────────────────────────────────────────────────

fn layout() -> (tempfile::TempDir, ArtifactV2Workspace) {
    install_recipient_compliance_scheduling_reader();
    let tmp = tempfile::tempdir().expect("temp dir");
    let layout = ArtifactV2Workspace::new(tmp.path());
    (tmp, layout)
}

fn scope() -> ComplianceScope {
    ComplianceScope::new("alpha", "prod")
}

/// `day` days after a fixed origin.
///
/// Days from an origin rather than a day-of-month: several tests need a point
/// beyond the end of the month, and `with_ymd_and_hms(2026, 8, 40, ..)` is not
/// a date — it returns `None` and the unwrap panics inside the test that is
/// supposed to be checking the window.
fn at(day: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 1, 9, 0, 0).unwrap() + Duration::days(day)
}

fn outward(layout: &ArtifactV2Workspace) -> OutwardAssertionStore {
    OutwardAssertionStore::new(layout.clone())
}

/// Record an act that actually told somebody something: prepared, then moved to
/// `dispatching`, which is the first status
/// `OutwardActStatus::is_active_disclosure` accepts.
fn dispatched_act(
    store: &OutwardAssertionStore,
    key: &str,
    program: Option<&str>,
    engagement: Option<&str>,
    audience: &[&str],
    when: DateTime<Utc>,
) -> String {
    let act = prepared_act(store, key, program, engagement, audience, when);
    store
        .mark_dispatching(&scope().outward(), &act, &when.to_rfc3339())
        .expect("the act moves to dispatching");
    act
}

/// The same, left at `prepared` — a record that has told nobody anything yet.
fn prepared_act(
    store: &OutwardAssertionStore,
    key: &str,
    program: Option<&str>,
    engagement: Option<&str>,
    audience: &[&str],
    when: DateTime<Utc>,
) -> String {
    store
        .prepare(
            &scope().outward(),
            &PrepareOutwardAct {
                idempotency_key: key.to_string(),
                program_id: program.map(str::to_string),
                engagement_id: engagement.map(str::to_string),
                exact_payload_artifact_ref: format!("payload-{key}"),
                effective_sender: "owner@example.test".to_string(),
                intended_audience: audience.iter().map(|value| value.to_string()).collect(),
                channel: OutwardChannel::Email,
                consequence_class: "outward".to_string(),
            },
            &when.to_rfc3339(),
        )
        .expect("the act prepares")
        .outward_act_ref
}

fn slot(when: DateTime<Utc>) -> Slot {
    Slot {
        start: when + Duration::days(1),
        end: when + Duration::days(1) + Duration::hours(1),
    }
}

/// Open an ask and absorb one reply, so the negotiation stands in `kind`'s state.
fn negotiation_in(
    layout: &ArtifactV2Workspace,
    audience: &AudienceRef,
    counterparty: &str,
    kind: ReplyKind,
    when: DateTime<Utc>,
) -> String {
    let audience_json = serde_json::to_vec(audience).expect("serialize audience");
    let negotiation_id = open_recipient_compliance_negotiation_for_test(
        layout.base_root(),
        "alpha",
        "prod",
        &audience_json,
        counterparty,
        "intro call",
        &[slot(when)],
        when,
    )
    .expect("the ask opens");
    absorb_recipient_compliance_reply_for_test(
        layout.base_root(),
        "alpha",
        "prod",
        &audience_json,
        &negotiation_id,
        format!("msg-{negotiation_id}"),
        when + Duration::hours(2),
        kind,
    )
    .expect("the reply absorbs");
    negotiation_id
}

/// Open an ask and absorb nothing — nobody has replied.
fn negotiation_awaiting(
    layout: &ArtifactV2Workspace,
    audience: &AudienceRef,
    counterparty: &str,
    when: DateTime<Utc>,
) -> String {
    open_recipient_compliance_negotiation_for_test(
        layout.base_root(),
        "alpha",
        "prod",
        &serde_json::to_vec(audience).expect("serialize audience"),
        counterparty,
        "intro call",
        &[slot(when)],
        when,
    )
    .expect("the ask opens")
}

fn decide(
    layout: &ArtifactV2Workspace,
    recipients: &[&str],
    this_work: Option<&WorkContextKind>,
    this_act_ref: Option<&str>,
    now: DateTime<Utc>,
) -> RecipientComplianceDecision {
    decide_under(
        layout,
        recipients,
        this_work,
        this_act_ref,
        &NoJurisdictionRule,
        now,
    )
}

fn decide_under(
    layout: &ArtifactV2Workspace,
    recipients: &[&str],
    this_work: Option<&WorkContextKind>,
    this_act_ref: Option<&str>,
    jurisdiction: &dyn JurisdictionRule,
    now: DateTime<Utc>,
) -> RecipientComplianceDecision {
    let recipients: Vec<String> = recipients.iter().map(|value| value.to_string()).collect();
    screen(
        layout,
        &scope(),
        &recipients,
        Some(OutwardChannel::Email),
        this_work,
        this_act_ref,
        &CompliancePolicy::standard(),
        jurisdiction,
        now,
    )
    .expect("the screen runs")
}

fn verdict_of(decision: &RecipientComplianceDecision, rule: ComplianceRule) -> &RuleVerdict {
    &decision
        .finding(rule)
        .unwrap_or_else(|| panic!("every decision must carry a finding for {}", rule.as_str()))
        .verdict
}

struct RefusingJurisdiction;

impl JurisdictionRule for RefusingJurisdiction {
    fn name(&self) -> &str {
        "test_supplied_rule"
    }

    fn ruling(&self, _query: &JurisdictionQuery<'_>) -> Result<JurisdictionRuling> {
        Ok(JurisdictionRuling::Refused {
            detail: "the supplied rule refuses this contact".to_string(),
        })
    }
}

struct BrokenJurisdiction;

impl JurisdictionRule for BrokenJurisdiction {
    fn name(&self) -> &str {
        "test_broken_rule"
    }

    fn ruling(&self, _query: &JurisdictionQuery<'_>) -> Result<JurisdictionRuling> {
        anyhow::bail!("the rule's own source could not be reached")
    }
}

struct PermittingJurisdiction;

impl JurisdictionRule for PermittingJurisdiction {
    fn name(&self) -> &str {
        "test_permissive_rule"
    }

    fn ruling(&self, _query: &JurisdictionQuery<'_>) -> Result<JurisdictionRuling> {
        Ok(JurisdictionRuling::Permitted)
    }
}

// ── Rule 1: duplicate recipient across work ─────────────────────────────────

/// The act being screened must never collide with its own record.
///
/// `record_outward_disclosure` writes the disclosure BEFORE every gate runs, so
/// by the time this rule looks, the act it is screening is already filed under
/// the recipient axis. Both halves are asserted in one test on purpose: without
/// the `this_act_ref` skip the very same fixture refuses, so deleting the guard
/// turns this test red instead of silently gating every outward send in the
/// runtime.
#[test]
fn the_act_being_screened_is_excluded_from_its_own_duplicate_scan() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    let act = dispatched_act(
        &store,
        "self-1",
        None,
        Some("acme"),
        &["alice@example.test"],
        at(0),
    );
    // The work the gate is told about deliberately differs from the work the
    // disclosure names. The runtime derives both from one binding so they agree
    // today — this pins that the guard survives them ever diverging, which is
    // the only shape in which the self-collision becomes a refusal rather than
    // a miscount.
    let mine = WorkContextKind::Engagement("beta".to_string());

    let with_guard = duplicate_contacts(
        &store,
        &scope().outward(),
        &["alice@example.test".to_string()],
        Some(act.as_str()),
        Some(&mine),
        Duration::days(DUPLICATE_CONTACT_WINDOW_DAYS),
        at(1),
    )
    .expect("the scan runs");
    assert_eq!(
        with_guard.own_act, 1,
        "the act under screen must be counted as its own record, not as somebody else's contact"
    );
    assert!(
        with_guard.collisions.is_empty(),
        "an act must never be a duplicate contact of itself: every outward send would refuse"
    );

    let without_guard = duplicate_contacts(
        &store,
        &scope().outward(),
        &["alice@example.test".to_string()],
        None,
        Some(&mine),
        Duration::days(DUPLICATE_CONTACT_WINDOW_DAYS),
        at(1),
    )
    .expect("the scan runs");
    assert_eq!(
        without_guard.collisions.len(),
        1,
        "without the exclusion the act collides with its own record — this is what the guard \
         prevents, and this half of the test is what stops the guard being removed"
    );
}

/// Two different works contacting one person inside the window is the collision
/// this rule exists to surface, and the refusal must name BOTH works — an owner
/// resolving it has to know who else is in the conversation.
#[test]
fn two_works_contacting_one_person_refuse_and_the_refusal_names_them() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    dispatched_act(
        &store,
        "prog-1",
        Some("outreach-q3"),
        None,
        &["alice@example.test"],
        at(0),
    );
    let mine = WorkContextKind::Engagement("acme".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&mine), None, at(1));

    assert!(
        !decision.is_clear(),
        "a second work contacting a person another work already reached must not be cleared"
    );
    match verdict_of(&decision, ComplianceRule::DuplicateRecipient) {
        RuleVerdict::Refused(refusal) => {
            let detail = refusal.detail();
            assert!(
                detail.contains("program:outreach-q3"),
                "the refusal must name the work that already contacted them, or the owner \
                 cannot go and look at it; got: {detail}"
            );
        },
        other => panic!(
            "expected a duplicate-recipient refusal, got {}",
            other.as_str()
        ),
    }
}

/// One work contacting the same person twice is a conversation, not a
/// collision. Refusing it would make every follow-up inside a live engagement
/// impossible, which is the failure `outward_gate` already names for a gate
/// that refuses one hundred percent of a flow.
#[test]
fn the_same_work_contacting_twice_does_not_refuse() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    dispatched_act(
        &store,
        "acme-1",
        None,
        Some("acme"),
        &["alice@example.test"],
        at(0),
    );
    let mine = WorkContextKind::Engagement("acme".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&mine), None, at(1));

    assert_eq!(
        decision.duplicate_scan.same_work, 1,
        "the earlier act belongs to the same work and must be counted as such"
    );
    assert!(
        decision.is_clear(),
        "a work following up inside its own relationship must not be refused"
    );
}

/// An act older than the window does not refuse — but it is still COUNTED. A
/// scan that dropped it would report `entries_seen` it cannot account for, and
/// the reconciliation is the only thing that catches a bucket going missing.
#[test]
fn an_act_outside_the_window_does_not_refuse_but_is_counted() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    dispatched_act(
        &store,
        "old-1",
        Some("outreach-q1"),
        None,
        &["alice@example.test"],
        at(0),
    );
    let mine = WorkContextKind::Engagement("acme".to_string());
    let decision = decide(
        &layout,
        &["alice@example.test"],
        Some(&mine),
        None,
        at(DUPLICATE_CONTACT_WINDOW_DAYS + 5),
    );

    assert_eq!(
        decision.duplicate_scan.outside_window, 1,
        "an act beyond the window must be counted under its own outcome, never dropped"
    );
    assert!(
        decision.is_clear(),
        "a contact from months ago is not a live collision"
    );
}

/// Every id the recipient axis offered ends under exactly one outcome.
///
/// The fixture deliberately holds one of each awkward shape at once: the act
/// under screen, an assertion-use id (the axis is mixed), an act whose
/// timestamp will not parse, an act naming no work, an act outside the window
/// and a real collision. A silently dropped entry is a real contact turning
/// into a clean pass, and only this equality catches it.
#[test]
fn the_duplicate_scan_accounts_for_every_entry_it_saw() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    let outward_scope = scope().outward();

    let mine_act = dispatched_act(
        &store,
        "mine",
        None,
        Some("beta"),
        &["alice@example.test"],
        at(0),
    );
    let collision = dispatched_act(
        &store,
        "collide",
        Some("outreach-q3"),
        None,
        &["alice@example.test"],
        at(0),
    );
    dispatched_act(
        &store,
        "unattributed",
        None,
        None,
        &["alice@example.test"],
        at(0),
    );
    dispatched_act(
        &store,
        "stale",
        Some("outreach-q1"),
        None,
        &["alice@example.test"],
        at(-60),
    );
    prepared_act(
        &store,
        "never-left",
        Some("outreach-q4"),
        None,
        &["alice@example.test"],
        at(0),
    );
    // An assertion-use id filed under the SAME recipient value. The axis holds
    // both kinds by design, so this is the normal case, not a fault.
    store
        .record_assertion_use(
            &outward_scope,
            &collision,
            "claim-1",
            "alice@example.test",
            &[],
            &[],
            &at(0).to_rfc3339(),
        )
        .expect("the assertion use records");
    // An act whose head carries an unparseable `prepared_at`. Written through
    // the store so the index entry exists exactly as a real one would.
    let torn = prepared_act(
        &store,
        "bad-clock",
        Some("outreach-q2"),
        None,
        &["alice@example.test"],
        at(0),
    );
    rewrite_prepared_at(&layout, &torn, "not-a-timestamp");

    let mine = WorkContextKind::Engagement("beta".to_string());
    let scan = duplicate_contacts(
        &store,
        &outward_scope,
        &["alice@example.test".to_string()],
        Some(mine_act.as_str()),
        Some(&mine),
        Duration::days(DUPLICATE_CONTACT_WINDOW_DAYS),
        at(1),
    )
    .expect("the scan runs");

    assert_eq!(
        scan.accounted_for(),
        scan.entries_seen,
        "every entry the recipient axis offered must end under exactly one outcome; a gap is an \
         entry this report cannot say what became of: {scan:?}"
    );
    assert_eq!(scan.own_act, 1, "the act under screen");
    assert_eq!(
        scan.not_an_act, 1,
        "the assertion-use id resolves to no act and must be counted as such, never as a contact"
    );
    assert_eq!(scan.unreadable_timestamp, 1, "the act with the bad clock");
    assert_eq!(scan.unattributed, 1, "the act naming no work");
    assert_eq!(scan.outside_window, 1, "the act from sixty days ago");
    assert_eq!(
        scan.inactive_disclosure, 1,
        "an act still merely prepared told nobody anything"
    );
    assert_eq!(scan.collisions.len(), 1, "the one real collision");
}

/// Rewrite an act's head so its `prepared_at` will not parse, leaving the
/// record otherwise valid — the shape a clock skew or a hand edit produces.
fn rewrite_prepared_at(layout: &ArtifactV2Workspace, act_ref: &str, prepared_at: &str) {
    let path = layout
        .scope_root("alpha", "prod")
        .join("outward_assertions")
        .join("acts")
        .join(format!("{act_ref}.jsonl"));
    let raw = layout
        .read_to_string_path_sync(&path)
        .expect("the act log reads");
    let mut lines = raw.lines();
    let mut value: serde_json::Value =
        serde_json::from_str(lines.next().expect("a head line")).expect("the head parses");
    value["prepared_at"] = serde_json::Value::String(prepared_at.to_string());
    let mut rewritten = serde_json::to_string(&value).expect("the head re-serialises");
    rewritten.push('\n');
    // The transitions after the head are kept. Dropping them would rewind the
    // act to `prepared`, so a fixture that meant to corrupt one field would
    // quietly have changed the act's status too — and the status is what
    // decides whether the act could have been a collision at all.
    for line in lines {
        rewritten.push_str(line);
        rewritten.push('\n');
    }
    layout
        .write_path_sync(&path, rewritten.as_bytes())
        .expect("the act log rewrites");
}

/// The recipient index is keyed on the UNNORMALISED spelling the disclosure
/// carried, so the rule probes every spelling it can derive — and reports how
/// many, so a decision never claims more coverage than it had.
#[test]
fn both_spellings_of_one_address_are_probed_and_the_collision_is_found() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    dispatched_act(
        &store,
        "lower",
        Some("outreach-q3"),
        None,
        &["alice@example.test"],
        at(0),
    );
    dispatched_act(
        &store,
        "mixed",
        Some("outreach-q4"),
        None,
        &["Alice@Example.test"],
        at(0),
    );

    let mine = WorkContextKind::Engagement("acme".to_string());
    let decision = decide(&layout, &["Alice@Example.test"], Some(&mine), None, at(1));

    assert_eq!(
        decision.duplicate_scan.spellings_probed, 2,
        "the supplied spelling and its normalised form are two different index files, and both \
         must be probed"
    );
    assert_eq!(
        decision.duplicate_scan.collisions.len(),
        2,
        "both works contacted the same person; a decision that found one of them would clear a \
         real collision"
    );
}

/// **A known limit, pinned rather than assumed away.** A third spelling nobody
/// passed in is invisible to this rule, because `index_act` hashes the raw
/// `intended_audience` string. The durable fix is normalising before
/// `append_index` plus a re-index pass, and it belongs in
/// `evidence::outward_assertions`, not here. If somebody fixes it there, this
/// test goes red and should be deleted — which is the point of writing it.
#[test]
fn a_spelling_that_was_never_supplied_is_missed_by_the_duplicate_rule() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    dispatched_act(
        &store,
        "third-spelling",
        Some("outreach-q3"),
        None,
        &["ALICE@EXAMPLE.TEST"],
        at(0),
    );

    let mine = WorkContextKind::Engagement("acme".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&mine), None, at(1));

    assert!(
        decision.duplicate_scan.collisions.is_empty(),
        "documenting the limit: the index is keyed on the raw spelling, so a third one is not \
         reachable from the two this call could derive"
    );
    assert_eq!(
        decision.duplicate_scan.spellings_probed, 1,
        "the supplied spelling already IS the normalised form here, so there is exactly one \
         index file to probe and the report must say so rather than implying two"
    );
}

/// An act that names no work cannot name a COLLIDING work, so the rule abstains
/// rather than refusing — and the evidence it declined to decide on is still
/// carried, so an owner reading the decision sees the prior contacts.
///
/// §9B's open decision 0 — that a programme's acts reach dispatch unbound — has
/// since been closed: `work_binding_for_dispatch` binds `Program` as well as
/// `Engagement`. What still arrives with no work named is an execution carrying
/// no work authority at all, which `work_kind_of` maps to `None`. Refusing here
/// would refuse every ordinary second contact made by one.
#[test]
fn an_act_naming_no_work_abstains_from_the_duplicate_rule_rather_than_refusing() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    dispatched_act(
        &store,
        "prog-1",
        Some("outreach-q3"),
        None,
        &["alice@example.test"],
        at(0),
    );
    let decision = decide(&layout, &["alice@example.test"], None, None, at(1));

    match verdict_of(&decision, ComplianceRule::DuplicateRecipient) {
        RuleVerdict::NotAssessed { why } => assert_eq!(
            *why, "act_names_no_work",
            "the reason must say WHICH question could not be answered"
        ),
        other => panic!(
            "an unattributed act must neither refuse nor clear the duplicate rule, got {}",
            other.as_str()
        ),
    }
    assert_eq!(
        decision.duplicate_scan.collisions.len(),
        1,
        "the prior contact must still be reported, or `not assessed` is indistinguishable from \
         `nothing found`"
    );
}

/// An earlier contact by another work that this rule could not place in time
/// must not read as a clean pass.
///
/// An act whose `prepared_at` will not parse is neither inside the
/// duplicate-contact window nor outside it, and `Clear` asserts the second.
/// Counting it in `unreadable_timestamp` and then clearing the send leaves the
/// count as the only survivor of the fact — and nothing on the dispatch path
/// reads counts, so the send goes out on a register this rule could not read.
///
/// The second half pins the scoping: an act that told nobody anything cannot be
/// a collision whatever its clock says, so it must NOT poison the rule.
/// Refusing on it would block a send over a record that could never have
/// blocked it.
#[test]
fn a_prior_contact_by_another_work_whose_clock_will_not_parse_refuses() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    let torn = dispatched_act(
        &store,
        "bad-clock",
        Some("outreach-q3"),
        None,
        &["alice@example.test"],
        at(0),
    );
    rewrite_prepared_at(&layout, &torn, "not-a-timestamp");

    let mine = WorkContextKind::Engagement("acme".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&mine), None, at(1));

    match verdict_of(&decision, ComplianceRule::DuplicateRecipient) {
        RuleVerdict::Unreadable { cause } => assert!(
            cause.contains("prepared_at"),
            "the cause must name what could not be read, or an owner has nothing to go and \
             repair; got: {cause}"
        ),
        other => panic!(
            "a prior contact by another work that this rule could not place in time must not \
             clear the send, got {}",
            other.as_str()
        ),
    }
    assert_eq!(
        decision.duplicate_scan.unreadable_timestamp, 1,
        "the unplaceable act must still be counted, or the outcomes stop summing to the entries \
         the index offered"
    );
    assert_eq!(
        decision.duplicate_scan.unplaceable_collisions, 1,
        "an ACTIVE disclosure by another work is the one shape of unreadable clock that may \
         refuse, and the verdict rests on this count"
    );
    assert!(
        decision.duplicate_scan.collisions.is_empty(),
        "an act that could not be placed in the window is not a collision this rule FOUND, and \
         reporting it as one would put a time on it that nobody can read"
    );
    assert!(
        !decision.is_clear(),
        "`we could not check` is never permission"
    );

    // The other half: the same corruption on an act that told nobody anything
    // must leave the rule able to answer.
    let (_tmp2, layout2) = self::layout();
    let store2 = outward(&layout2);
    let never_left = prepared_act(
        &store2,
        "bad-clock-never-left",
        Some("outreach-q3"),
        None,
        &["alice@example.test"],
        at(0),
    );
    rewrite_prepared_at(&layout2, &never_left, "not-a-timestamp");
    let quiet = decide(&layout2, &["alice@example.test"], Some(&mine), None, at(1));

    assert_eq!(
        quiet.duplicate_scan.unreadable_timestamp, 1,
        "it is still counted — it was still an entry the index offered"
    );
    assert_eq!(
        quiet.duplicate_scan.unplaceable_collisions, 0,
        "an act still merely prepared told nobody anything, so its clock could not have changed \
         this rule's answer"
    );
    assert!(
        matches!(
            verdict_of(&quiet, ComplianceRule::DuplicateRecipient),
            RuleVerdict::Clear
        ),
        "a gate that refuses over a record that could never have refused it is a gate somebody \
         routes around"
    );
}

// ── Rule 2: reply before follow-up ──────────────────────────────────────────

/// A counterparty whose counter is waiting on us must not be followed up by
/// ANOTHER work — and must still be reachable by the work that owns the
/// relationship, because that send IS our answer.
///
/// Both directions in one test on purpose. Deleting the passing half deadlocks
/// every scheduling conversation; deleting the refusing half removes the rule.
#[test]
fn a_countered_ask_refuses_another_work_and_never_the_work_that_owns_it() {
    let (_tmp, layout) = layout();
    let audience = AudienceRef::engagement("acme");
    negotiation_in(
        &layout,
        &audience,
        "alice@example.test",
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );

    let other = WorkContextKind::Engagement("beta".to_string());
    let refused = decide(&layout, &["alice@example.test"], Some(&other), None, at(1));
    match verdict_of(&refused, ComplianceRule::ReplyPending) {
        RuleVerdict::Refused(refusal) => {
            let detail = refusal.detail();
            assert!(
                detail.contains("countered"),
                "the refusal must say what they said, not merely that they said something; \
                 got: {detail}"
            );
        },
        other => panic!(
            "a different work following up on somebody waiting on us must be refused, got {}",
            other.as_str()
        ),
    }

    let owner = WorkContextKind::Engagement("acme".to_string());
    let allowed = decide(&layout, &["alice@example.test"], Some(&owner), None, at(1));
    assert!(
        allowed.is_clear(),
        "the work that owns the relationship must still be able to ANSWER; refusing it would \
         mean the conversation could never move"
    );
    assert_eq!(
        allowed.reply_scan.awaiting_our_move, 1,
        "the ask is still counted as awaiting our move even where it does not refuse"
    );
}

/// A decline IS an answer, so nobody is left waiting on us. Refusing here would
/// make it impossible to reply to somebody who said no.
#[test]
fn a_declined_ask_does_not_refuse() {
    let (_tmp, layout) = layout();
    negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        "alice@example.test",
        ReplyKind::Declined { reason: None },
        at(0),
    );
    let other = WorkContextKind::Engagement("beta".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&other), None, at(1));

    assert_eq!(
        decision.reply_scan.declined, 1,
        "a decline must be counted under its own outcome"
    );
    assert!(
        decision.is_clear(),
        "a decline leaves nobody waiting on us, so this rule has nothing to refuse"
    );
}

/// Nobody has replied, so nobody is waiting on us. Chasing silence is
/// `scheduling`'s Module D and is deliberately not a refusal here — a rule that
/// blocked the chase would block the only thing that ever un-sticks the ask.
#[test]
fn an_unanswered_ask_does_not_refuse() {
    let (_tmp, layout) = layout();
    negotiation_awaiting(
        &layout,
        &AudienceRef::engagement("acme"),
        "alice@example.test",
        at(0),
    );
    let other = WorkContextKind::Engagement("beta".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&other), None, at(1));

    assert_eq!(decision.reply_scan.awaiting_reply, 1);
    assert!(
        decision.is_clear(),
        "silence is not a reply, and this rule is only about replies we have not acted on"
    );
}

/// Every negotiation in the scope ends under exactly one outcome, so a report
/// that says it read N cannot quietly have decided about fewer.
#[test]
fn the_reply_scan_accounts_for_every_negotiation() {
    let (_tmp, layout) = layout();
    negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        "alice@example.test",
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    negotiation_in(
        &layout,
        &AudienceRef::engagement("beta"),
        "bob@example.test",
        ReplyKind::Declined { reason: None },
        at(0),
    );
    negotiation_awaiting(
        &layout,
        &AudienceRef::engagement("gamma"),
        "carol@example.test",
        at(0),
    );

    let negotiations = read_recipient_compliance_negotiations(&layout, "alpha", "prod")
        .expect("the scheduling projection reads");
    let index = reply_index(&negotiations).expect("the index builds");
    assert_eq!(index.scan.negotiations_seen, 3);
    assert_eq!(
        index.scan.accounted_for(),
        index.scan.negotiations_seen,
        "a negotiation that fell out of every bucket is a live ask this rule cannot say what it \
         did about: {:?}",
        index.scan
    );
}

/// An unattributed act abstains from rule 2 too, and for a sharper reason than
/// rule 1: refusing would refuse OUR OWN ANSWER to the counterparty, and with
/// no work named there is no ownership test that could tell that answer from a
/// follow-up. §9B's open decision 0 — that a programme's acts reach dispatch
/// unbound — has since been closed, so what arrives here with no work named is
/// an execution carrying no work authority at all, which `work_kind_of` maps to
/// `None`. The pending ask is still counted, so the abstention is visible
/// rather than silent.
#[test]
fn an_act_naming_no_work_abstains_from_the_reply_rule_rather_than_deadlocking() {
    let (_tmp, layout) = layout();
    negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        "alice@example.test",
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    let decision = decide(&layout, &["alice@example.test"], None, None, at(1));

    match verdict_of(&decision, ComplianceRule::ReplyPending) {
        RuleVerdict::NotAssessed { why } => assert_eq!(*why, "act_names_no_work"),
        other => panic!(
            "an unattributed act must not be refused its own answer, got {}",
            other.as_str()
        ),
    }
    assert_eq!(
        decision.reply_scan.awaiting_our_move, 1,
        "the ask waiting on us must still be counted, or the abstention hides a live obligation"
    );
}

// ── Rule 2: one unreadable row must not refuse the whole workspace ──────────

/// A counterparty holding U+001F, which `SchedulingStore::open` will happily
/// store because it checks only that the value is non-blank.
///
/// This is the exact shape that used to refuse every outward send in the whole
/// `(principal, workspace)`: one row, one audience's log, and no remedy the
/// append-only store offers for rewriting it.
const MALFORMED_COUNTERPARTY: &str = "alice@example.test\u{1f}x";

/// One malformed row must refuse only the recipients it could be about.
///
/// Both directions in one test on purpose. Deleting the narrowing puts the
/// workspace-wide refusal back; deleting the refusing half lets a live ask that
/// might be about the person in front of us read as "nobody is waiting on you",
/// which is the fail-open reading of an unreadable register.
#[test]
fn one_unreadable_ask_refuses_only_the_recipients_it_could_be_about() {
    let (_tmp, layout) = layout();
    negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        MALFORMED_COUNTERPARTY,
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    let other = WorkContextKind::Engagement("beta".to_string());

    // Somebody the row could be about: `alice@example.test` is one of the
    // readings of that counterparty, so the rule still cannot answer.
    let refused = decide(&layout, &["alice@example.test"], Some(&other), None, at(1));
    assert!(
        matches!(
            verdict_of(&refused, ComplianceRule::ReplyPending),
            RuleVerdict::Unreadable { .. }
        ),
        "a row that could be about this very recipient must refuse; `we could not check` is \
         never permission, got {}",
        verdict_of(&refused, ComplianceRule::ReplyPending).as_str()
    );

    // Somebody the row demonstrably is not about. Before the narrowing this
    // send was refused too, and nothing about it had any relationship to the
    // malformed negotiation.
    let unrelated = decide(&layout, &["bob@example.test"], Some(&other), None, at(1));
    assert!(
        unrelated.is_clear(),
        "one malformed row must not refuse a send to somebody it could not be about; the \
         refusal was: {:?}",
        unrelated.refusal_message()
    );
    assert_eq!(
        unrelated.reply_scan.unmatchable_counterparty, 1,
        "the unreadable row must still be COUNTED on a send it does not refuse, or the \
         narrowing hides the fault from the only person who can repair it"
    );
}

/// A row nothing can be recovered from refuses everybody, because it could be
/// anybody.
///
/// The counterpart to the test above, and the reason the narrowing is not
/// fail-open: `candidate_identities` being empty means *"we recovered nothing"*,
/// which must never collapse into *"this is about nobody"*.
#[test]
fn an_unreadable_ask_that_recovers_nothing_refuses_every_recipient() {
    let (_tmp, layout) = layout();
    negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        // Control characters only: no fragment and no stripped form normalises,
        // so nothing at all can be said about who this ask is with.
        "\u{1}\u{2}",
        // The slot the fixture offered — `absorb` refuses an acceptance of a
        // slot that was never on the table.
        ReplyKind::Accepted { slot: slot(at(0)) },
        at(0),
    );
    let other = WorkContextKind::Engagement("beta".to_string());
    let decision = decide(&layout, &["bob@example.test"], Some(&other), None, at(1));

    match verdict_of(&decision, ComplianceRule::ReplyPending) {
        RuleVerdict::Unreadable { cause } => assert!(
            cause.contains("could be about anyone"),
            "the refusal must say that nothing was recovered, so an owner knows this row \
             refuses every send rather than one; got: {cause}"
        ),
        other => panic!(
            "a row nothing could be recovered from might be about the recipient in front of us \
             and must refuse, got {}",
            other.as_str()
        ),
    }
}

/// The refusal must name the row, and must still report how many there were.
///
/// A count nobody can act on was the old failure; a narrowed refusal that
/// stopped reporting the count would be the opposite one. Both are pinned here,
/// together with the escaping — the fault being reported is that the recorded
/// value carries control characters, and a refusal travels into log lines and a
/// model's prompt.
#[test]
fn the_narrowed_refusal_names_the_row_and_still_reports_the_count() {
    let (_tmp, layout) = layout();
    let blocking = negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        MALFORMED_COUNTERPARTY,
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    // A second malformed row about somebody else entirely. It does not refuse
    // this send, and it must still be counted in the refusal this send gets.
    negotiation_in(
        &layout,
        &AudienceRef::engagement("gamma"),
        "carol@example.test\u{1f}x",
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );

    let other = WorkContextKind::Engagement("beta".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&other), None, at(1));

    let RuleVerdict::Unreadable { cause } = verdict_of(&decision, ComplianceRule::ReplyPending)
    else {
        panic!(
            "the row that could be about this recipient must refuse, got {}",
            verdict_of(&decision, ComplianceRule::ReplyPending).as_str()
        );
    };
    assert!(
        cause.contains(&blocking),
        "the refusal must name the negotiation, or the only way to find the offending row is to \
         read every negotiation in the workspace by hand; got: {cause}"
    );
    assert!(
        cause.contains("engagement:acme"),
        "the refusal must name the relationship the row lives in — it is the one field of an \
         unreadable row that IS readable; got: {cause}"
    );
    assert!(
        cause.contains("1 of 2"),
        "the refusal must report what it narrowed to AND how many unreadable rows the scope \
         holds, or a scope with forty bad rows reads like a scope with one; got: {cause}"
    );
    assert!(
        !cause.contains('\u{1f}'),
        "the refusal must not carry the control character it is reporting; got: {cause}"
    );
    assert!(
        cause.contains("\\u{001f}"),
        "the recorded value must still be recognisable to an owner searching their scheduling \
         log; got: {cause}"
    );

    assert_eq!(
        decision.reply_scan.unmatchable_counterparty, 2,
        "both malformed rows must be counted, whichever of them refuses this send"
    );
    assert_eq!(
        decision.reply_scan.unmatchable.len(),
        decision.reply_scan.unmatchable_counterparty,
        "the count and the named rows must never disagree, or a reader who checks one against \
         the other stops trusting every figure this gate reports"
    );
    assert_eq!(
        decision.reply_scan.accounted_for(),
        decision.reply_scan.negotiations_seen,
        "an unreadable row is still a negotiation that must land in exactly one bucket: {:?}",
        decision.reply_scan
    );
}

/// The work that owns the relationship is not refused by its own unreadable row.
///
/// The same exemption a readable pending ask gets, applied to the one field of
/// an unreadable row that is still readable. Without it, a malformed
/// counterparty would deadlock the very conversation it belongs to: our answer
/// to that counterparty is itself an outward act to them.
#[test]
fn the_work_that_owns_the_relationship_is_not_refused_by_its_own_unreadable_ask() {
    let (_tmp, layout) = layout();
    negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        MALFORMED_COUNTERPARTY,
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    let owner = WorkContextKind::Engagement("acme".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&owner), None, at(1));

    assert!(
        decision.is_clear(),
        "the work that owns the ask must still be able to answer it; the refusal was: {:?}",
        decision.refusal_message()
    );
    assert_eq!(
        decision.reply_scan.unmatchable_counterparty, 1,
        "the exemption must not hide the malformed row from the report"
    );
}

/// The counterparty `<>` — three characters — reaches *"could be about anyone"*.
///
/// `SchedulingStore::open` refuses a counterparty only when it is blank once
/// TRIMMED. `normalise_identity` also unwraps angle brackets, and `<>` unwraps
/// to nothing, so the row stores and can never be keyed — and neither reading
/// `recoverable_identities` takes recovers an address from it. That is the same
/// workspace-wide blast radius the candidate narrowing was built to remove,
/// reached through a different door, and there is no repair for it: the
/// scheduling log is append-only and the negotiation id is derived from the
/// counterparty.
///
/// Both directions, in one test on purpose. An act that NAMES A WORK is still
/// refused — a real counterparty really is waiting on us and nothing in reach
/// can say who, and "we could not check" is never permission. An act that names
/// none abstains, for exactly the reason it abstains over an ask it can fully
/// read: with no work there is no ownership test to run, so rule 2's own
/// question cannot be put to any row.
#[test]
fn a_counterparty_of_angle_brackets_alone_recovers_nothing_and_still_refuses_a_bound_act() {
    assert!(
        normalise_identity("<>").is_err(),
        "if this ever keyed, the row this test is about would not exist and the rest of it \
         would pass vacuously"
    );
    assert!(
        recoverable_identities("<>").is_empty(),
        "neither the separator reading nor the noise reading recovers an address, which is what \
         makes this row one that could be about anyone"
    );

    let (_tmp, layout) = layout();
    negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        "<>",
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );

    let other = WorkContextKind::Engagement("beta".to_string());
    let bound = decide(&layout, &["bob@example.test"], Some(&other), None, at(1));
    assert!(
        matches!(
            verdict_of(&bound, ComplianceRule::ReplyPending),
            RuleVerdict::Unreadable { .. }
        ),
        "an act that names a work must still be refused by a row that could be about anyone, or \
         an unreadable register reads as an empty one; got {}",
        verdict_of(&bound, ComplianceRule::ReplyPending).as_str()
    );

    let unbound = decide(&layout, &["bob@example.test"], None, None, at(1));
    match verdict_of(&unbound, ComplianceRule::ReplyPending) {
        RuleVerdict::NotAssessed { why } => assert_eq!(
            *why, "act_names_no_work_and_a_row_was_unreadable",
            "the abstention must still say a row could not be read, or it renders identically \
             to an abstention over evidence this rule understood"
        ),
        other => panic!(
            "one unrepairable row must not refuse every unattributed act in the workspace — \
             every execution that carries no work authority arrives here unbound, and \
             `work_kind_of` maps that to `None`; got {}",
            other.as_str()
        ),
    }
    assert_eq!(
        unbound.reply_scan.unmatchable_counterparty, 1,
        "the row must still be COUNTED on the act it does not refuse; a silently narrowed \
         refusal hides the fault from the only person who can close the ask"
    );
    assert_eq!(
        unbound.reply_scan.unmatchable.len(),
        unbound.reply_scan.unmatchable_counterparty,
        "the count and the named rows must never disagree, whichever verdict was reached"
    );
}

/// Rule 2 must not answer WEAKER evidence more harshly than stronger evidence.
///
/// Both halves are one and the same unattributed act. A pending ask this rule
/// can FULLY READ — it knows the person, it knows they are waiting on us —
/// abstains, because with no work named there is no telling a follow-up from
/// our own answer. A row it cannot read at all is strictly less informative,
/// and it used to REFUSE: the rule was more permissive about the case it
/// understood than about the case it did not, which is not fail-closed, it is
/// incoherent. Deleting either half of this test puts that asymmetry back.
#[test]
fn an_unattributed_act_is_answered_no_more_harshly_over_a_row_that_could_not_be_read() {
    let (_readable_tmp, readable) = layout();
    negotiation_in(
        &readable,
        &AudienceRef::engagement("acme"),
        "alice@example.test",
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    let readable_decision = decide(&readable, &["alice@example.test"], None, None, at(1));

    let (_unreadable_tmp, unreadable) = layout();
    negotiation_in(
        &unreadable,
        &AudienceRef::engagement("acme"),
        MALFORMED_COUNTERPARTY,
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    let unreadable_decision = decide(&unreadable, &["alice@example.test"], None, None, at(1));

    assert!(
        matches!(
            verdict_of(&readable_decision, ComplianceRule::ReplyPending),
            RuleVerdict::NotAssessed { .. }
        ),
        "the stronger evidence must abstain, or an unattributed act can never answer the \
         counterparty waiting on it; got {}",
        verdict_of(&readable_decision, ComplianceRule::ReplyPending).as_str()
    );
    match verdict_of(&unreadable_decision, ComplianceRule::ReplyPending) {
        RuleVerdict::NotAssessed { why } => assert_eq!(
            *why, "act_names_no_work_and_a_row_was_unreadable",
            "the two abstentions must stay apart: `we read it and could not decide` and `we \
             could not read it` are different facts, and only one of them names a row to close"
        ),
        other => panic!(
            "a row this rule could not read AT ALL must not be answered more harshly than one \
             it could read in full, for the identical act; got {}",
            other.as_str()
        ),
    }
    assert_eq!(
        unreadable_decision.reply_scan.unmatchable_counterparty, 1,
        "abstaining must not hide the unreadable row: the scan is the only thing that can point \
         an owner at the ask to close"
    );
}

/// The refusal names the remedy that exists, and stops naming one that does not.
///
/// It used to advise repairing the counterparty first. The scheduling log is
/// append-only and offers no rewrite, and `derive_negotiation_id` folds the
/// counterparty into the id — so re-opening under a corrected spelling files a
/// SECOND ask and leaves the offending row exactly as live. An owner who
/// followed that advice would watch the same refusal come back after the "fix".
/// `SchedulingStore::close` is the one remedy, and the last assertions run it.
#[test]
fn the_unreadable_refusal_names_the_only_remedy_the_store_actually_offers() {
    let (_tmp, layout) = layout();
    let blocking = negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        MALFORMED_COUNTERPARTY,
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    let other = WorkContextKind::Engagement("beta".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&other), None, at(1));
    let RuleVerdict::Unreadable { cause } = verdict_of(&decision, ComplianceRule::ReplyPending)
    else {
        panic!(
            "the row that could be about this recipient must refuse, got {}",
            verdict_of(&decision, ComplianceRule::ReplyPending).as_str()
        );
    };

    assert!(
        cause.contains("CLOSE the named ask"),
        "the refusal must point at the remedy that exists; got: {cause}"
    );
    assert!(
        cause.contains("cannot be repaired in place"),
        "it must also say the obvious fix is not available, or the owner repairs the spelling, \
         files a second ask and meets this same refusal again; got: {cause}"
    );
    // `close` is called with the audience and the negotiation id, and both are
    // already in this sentence — so the remedy is reachable from what the owner
    // was handed, without reading every negotiation in the workspace by hand.
    assert!(
        cause.contains(&blocking) && cause.contains("engagement:acme"),
        "the remedy must be actionable from this sentence alone; got: {cause}"
    );

    // A named remedy nobody ran is a promise. Run it.
    close_recipient_compliance_negotiation_for_test(
        layout.base_root(),
        "alpha",
        "prod",
        &serde_json::to_vec(&AudienceRef::engagement("acme")).expect("serialize audience"),
        &blocking,
        "counterparty unreadable; refiled under the corrected spelling",
        at(2),
    )
    .expect("the ask closes");
    let after = decide(&layout, &["alice@example.test"], Some(&other), None, at(3));
    assert!(
        after.is_clear(),
        "a closed ask is settled and must stop poisoning this rule, or the remedy the refusal \
         names does not work; the refusal was: {:?}",
        after.refusal_message()
    );
    assert_eq!(
        after.reply_scan.settled, 1,
        "the closed ask must land in the settled bucket rather than vanishing from the scan"
    );
    assert_eq!(
        after.reply_scan.unmatchable_counterparty, 0,
        "a settled row is no longer a live ask this rule could not key, and must not keep being \
         reported as one"
    );
}

/// An unreadable row and a fully-read pending ask in the SAME call: the send
/// still refuses, and the refusal does not claim an ignorance the gate does not
/// have.
///
/// One rule emits one verdict, and `Unreadable` is the arm `screen` reaches
/// whenever any row blocks — so the `AwaitingOurReply` evidence beside it never
/// reaches the sentence. That is survivable only while the sentence stops short
/// of speaking for the recipients: it used to say the rule "cannot say whether
/// any of them has already answered and is waiting on us", which is flatly
/// false here, because the very same call read `bob@example.test`'s counter in
/// full. A refusal an owner can disprove from the decision beside it is a
/// refusal they stop believing.
#[test]
fn a_refusal_over_an_unreadable_row_does_not_deny_a_pending_ask_it_did_read() {
    let (_tmp, layout) = layout();
    negotiation_in(
        &layout,
        &AudienceRef::engagement("acme"),
        MALFORMED_COUNTERPARTY,
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );
    // Fully read, in a relationship this act's work does not own, and about a
    // recipient the malformed row could NOT be about — so the two pieces of
    // evidence are genuinely independent.
    negotiation_in(
        &layout,
        &AudienceRef::engagement("delta"),
        "bob@example.test",
        ReplyKind::Countered {
            slots: vec![slot(at(3))],
        },
        at(0),
    );

    let other = WorkContextKind::Engagement("beta".to_string());
    let decision = decide(
        &layout,
        &["alice@example.test", "bob@example.test"],
        Some(&other),
        None,
        at(1),
    );

    let RuleVerdict::Unreadable { cause } = verdict_of(&decision, ComplianceRule::ReplyPending)
    else {
        panic!(
            "an unreadable row that could be about a recipient must still stop the send, got {}",
            verdict_of(&decision, ComplianceRule::ReplyPending).as_str()
        );
    };
    assert!(
        !cause.contains("cannot say whether"),
        "the refusal must not deny knowing whether a recipient is waiting on us while the same \
         call has read exactly that; got: {cause}"
    );
    assert!(
        !decision.is_clear(),
        "both kinds of evidence refuse, so the send must not pass whichever verdict won"
    );
    assert_eq!(
        decision.reply_scan.awaiting_our_move, 1,
        "the ask the rule DID read must still be counted, or the verdict that outranked it \
         erases it from the decision as well as from the sentence"
    );
    assert_eq!(
        decision.reply_scan.unmatchable_counterparty, 1,
        "the unreadable row must be counted beside it, not instead of it"
    );
    assert_eq!(
        decision.reply_scan.accounted_for(),
        decision.reply_scan.negotiations_seen,
        "each negotiation must land in exactly one bucket: {:?}",
        decision.reply_scan
    );
}

/// Both readings of a control character are recovered, and no third is claimed.
///
/// Pins what `could_be_about` is allowed to narrow away. If this ever recovered
/// only one reading, a row whose corruption took the other shape would be
/// silently narrowed away from a recipient it really was about.
#[test]
fn an_unreadable_counterparty_is_recovered_as_separator_and_as_noise() {
    let recovered = recoverable_identities(MALFORMED_COUNTERPARTY);
    assert!(
        recovered.contains(&"alice@example.test".to_string()),
        "the control character read as a SEPARATOR must yield the address beside it; got \
         {recovered:?}"
    );
    assert!(
        recovered.contains(&"alice@example.testx".to_string()),
        "the control character read as NOISE inside one address must yield the joined form; got \
         {recovered:?}"
    );
    assert!(
        recoverable_identities("\u{1}\u{2}").is_empty(),
        "a value with nothing recoverable must recover nothing — an empty candidate list is \
         what makes a row refuse everybody"
    );
}

/// A recorded value repeated in a refusal is bounded, and says when it was cut.
///
/// The value is caller-supplied and arrives here precisely because it is
/// malformed. An unbounded one would put whatever it holds into a log line and
/// a model's prompt; an unmarked truncation would have an owner searching their
/// scheduling log for a prefix believing it was the whole value.
#[test]
fn a_recorded_counterparty_repeated_in_a_refusal_is_bounded_and_marked() {
    let long = "a".repeat(MAX_RENDERED_COUNTERPARTY + 50);
    let rendered = escaped_for_display(&long);
    assert!(
        rendered.contains("truncated"),
        "a cut value must say it was cut; got: {rendered}"
    );
    assert!(
        rendered.chars().count() < long.chars().count(),
        "the bound must actually bind"
    );

    let short = escaped_for_display("alice@example.test");
    assert_eq!(
        short, "alice@example.test",
        "a value inside the bound must survive verbatim, or an owner cannot search for it"
    );
}

/// The bound must hold for the whole sentence, not for one field of it.
///
/// `candidate_identities` is recovered FROM the malformed counterparty, so a
/// refusal that bounded `counterparty_as_recorded` and then printed the
/// candidates whole would re-emit, in the same sentence, the megabyte the bound
/// exists to keep out of a log line and a model's prompt. `purpose` and the
/// audience id are caller-supplied on the same row — `SchedulingStore::open`
/// checks only that they are non-blank — so they are bounded here too.
#[test]
fn a_refusal_bounds_every_caller_supplied_part_not_only_the_recorded_value() {
    let long = "a".repeat(MAX_RENDERED_COUNTERPARTY + 500);
    let raw = format!("{long}\u{1f}{long}");
    let recovered = recoverable_identities(&raw);
    assert!(
        recovered
            .iter()
            .any(|candidate| candidate.chars().count() > MAX_RENDERED_COUNTERPARTY),
        "the fixture must recover a candidate longer than the bound, or this test proves \
         nothing; got {:?}",
        recovered
            .iter()
            .map(|candidate| candidate.chars().count())
            .collect::<Vec<_>>()
    );

    let ask = UnmatchableAsk {
        negotiation_id: "neg-1".to_string(),
        audience: AudienceRef::engagement(long.clone()),
        counterparty_as_recorded: escaped_for_display(&raw),
        purpose: long.clone(),
        candidate_identities: recovered,
        why: "it holds a control character".to_string(),
    };
    let rendered = unmatchable_reply_cause(&[&ask], 1);

    assert!(
        !rendered.contains(&"a".repeat(MAX_RENDERED_COUNTERPARTY + 1)),
        "no caller-supplied field may exceed the bound: a run longer than \
         MAX_RENDERED_COUNTERPARTY means one of them was printed whole. Rendered {} chars",
        rendered.chars().count()
    );
    assert!(
        rendered.contains("truncated"),
        "a cut value must still say it was cut, or an owner searches their scheduling log for a \
         prefix believing it is the whole value; got: {rendered}"
    );
    assert!(
        rendered.contains("neg-1"),
        "bounding the fields must not cost the row its name — naming the row is the whole point \
         of this sentence; got: {rendered}"
    );
}

// ── Enablement ──────────────────────────────────────────────────────────────

/// The shipped default is `off`, and an unreadable posture is not `blocking`.
///
/// Both halves in one test. This gate REFUSES sends, so a posture that switched
/// itself on — or that guessed `blocking` from a misspelling — would take every
/// outward send in a fleet down on the strength of a typo.
#[test]
fn the_default_recipient_compliance_posture_is_off_and_a_typo_never_blocks() {
    assert_eq!(
        RecipientComplianceMode::default(),
        RecipientComplianceMode::Off
    );
    assert!(!RecipientComplianceMode::default().blocks());

    assert_eq!(
        parse_recipient_compliance_mode("blocking"),
        Some(RecipientComplianceMode::Blocking),
        "the one posture that turns the gate on must be readable, or the key is unusable"
    );
    assert!(RecipientComplianceMode::Blocking.blocks());

    for written in ["block", "on", "true", "yes", "", "enforcing", "shadow"] {
        assert_eq!(
            parse_recipient_compliance_mode(written),
            None,
            "`{written}` names no posture here; every caller must read it as `off` rather than \
             start refusing sends on a misspelling"
        );
    }
}

// ── Rule 3: the jurisdiction port ───────────────────────────────────────────

/// With no rule supplied, the verdict must be `NotAssessed` — never `Clear`.
///
/// Asserted on the ARM, not on `is_clear()`, so a change that folded the two
/// arms together fails here. A pass reported as `Clear` would say a
/// jurisdiction cleared this send when nothing looked at it.
#[test]
fn no_jurisdiction_rule_yields_not_assessed_never_clear() {
    let (_tmp, layout) = layout();
    let decision = decide(&layout, &["alice@example.test"], None, None, at(1));

    match verdict_of(&decision, ComplianceRule::Jurisdiction) {
        RuleVerdict::NotAssessed { why } => assert_eq!(
            *why, "no_rule_applied_to_every_recipient",
            "the default port must say that nothing ruled, not that something permitted"
        ),
        other => panic!(
            "the default jurisdiction port must not report a verdict it has no basis for, got {}",
            other.as_str()
        ),
    }
    assert!(
        decision.is_clear(),
        "not-assessed does not refuse: a default that refused would refuse every send in the \
         codebase"
    );
}

/// A rule that ruled on every recipient and permitted them all is the only
/// thing that earns `Clear`.
#[test]
fn a_rule_that_permits_every_recipient_clears_the_jurisdiction_check() {
    let (_tmp, layout) = layout();
    let decision = decide_under(
        &layout,
        &["alice@example.test"],
        None,
        None,
        &PermittingJurisdiction,
        at(1),
    );
    assert_eq!(
        verdict_of(&decision, ComplianceRule::Jurisdiction),
        &RuleVerdict::Clear,
        "a supplied rule that looked and said yes is the one case where `clear` is true"
    );
}

/// A supplied rule that refuses refuses the send, and the refusal names the
/// rule so an owner knows what to go and read.
#[test]
fn a_jurisdiction_rule_that_refuses_stops_the_send() {
    let (_tmp, layout) = layout();
    let decision = decide_under(
        &layout,
        &["alice@example.test"],
        None,
        None,
        &RefusingJurisdiction,
        at(1),
    );

    assert!(!decision.is_clear());
    match verdict_of(&decision, ComplianceRule::Jurisdiction) {
        RuleVerdict::Refused(refusal) => assert!(
            refusal.detail().contains("test_supplied_rule"),
            "the refusal must name the rule; `a jurisdiction said no` with no name is not \
             actionable"
        ),
        other => panic!("expected a jurisdiction refusal, got {}", other.as_str()),
    }
}

/// A rule that could not answer has not answered yes.
#[test]
fn a_jurisdiction_rule_that_errors_refuses_rather_than_passing() {
    let (_tmp, layout) = layout();
    let decision = decide_under(
        &layout,
        &["alice@example.test"],
        None,
        None,
        &BrokenJurisdiction,
        at(1),
    );

    match verdict_of(&decision, ComplianceRule::Jurisdiction) {
        RuleVerdict::Unreadable { cause } => assert!(
            cause.contains("could not be reached"),
            "the cause must survive to the caller; `we could not check` with no reason is not \
             actionable"
        ),
        other => panic!(
            "a jurisdiction rule that failed must refuse, not clear, got {}",
            other.as_str()
        ),
    }
    assert!(
        !decision.is_clear(),
        "an unanswerable rule is never permission"
    );
}

// ── Rule 4: the erasure request ─────────────────────────────────────────────

/// A recorded erasure request refuses contact, and keeps refusing it. There is
/// no fulfilment path that lifts it, because the request row IS the evidence
/// the refusal rests on — deleting it would resurrect the contact.
#[test]
fn a_recorded_erasure_request_refuses_and_a_second_record_does_not_lift_it() {
    let (_tmp, layout) = layout();
    let log = ErasureRequestLog::new(layout.clone());
    log.record(
        &scope(),
        "Alice@Example.test",
        at(0),
        "erasure-ticket-1",
        "owner",
    )
    .expect("the request records");

    let decision = decide(&layout, &["alice@example.test"], None, None, at(1));
    assert!(!decision.is_clear());
    assert!(matches!(
        verdict_of(&decision, ComplianceRule::ErasureRequested),
        RuleVerdict::Refused(_)
    ));

    // A second, later request — the shape a "we have now handled it" record
    // would take. It must not read as a lift.
    log.record(
        &scope(),
        "alice@example.test",
        at(2),
        "erasure-ticket-2-fulfilment",
        "owner",
    )
    .expect("the second request records");
    let after = decide(&layout, &["alice@example.test"], None, None, at(3));
    assert!(
        !after.is_clear(),
        "nothing appended to this log may un-refuse a contact; the row is the evidence the \
         refusal rests on"
    );
}

/// The request is normalised on the way in, so a request filed under one
/// spelling refuses every spelling of the same address. Anything less and a
/// person who asked to be forgotten is contacted through a capital letter.
#[test]
fn an_erasure_request_refuses_every_spelling_of_the_same_address() {
    let (_tmp, layout) = layout();
    ErasureRequestLog::new(layout.clone())
        .record(
            &scope(),
            "  <Alice@Example.test> ",
            at(0),
            "ticket",
            "owner",
        )
        .expect("the request records");

    let decision = decide(&layout, &["ALICE@EXAMPLE.TEST"], None, None, at(1));
    assert!(
        !decision.is_clear(),
        "the erasure register is normalised, so a differently-spelled address is the same person"
    );
}

/// Blank evidence and a blank recorder are refused. A request nobody can check
/// is an assertion, and this one refuses contact permanently.
#[test]
fn an_unauditable_erasure_request_is_refused() {
    let (_tmp, layout) = layout();
    let log = ErasureRequestLog::new(layout.clone());
    assert!(
        log.record(&scope(), "alice@example.test", at(0), "  ", "owner")
            .is_err(),
        "a request with no evidence ref cannot be audited and must not be recordable"
    );
    assert!(
        log.record(&scope(), "alice@example.test", at(0), "ticket", " ")
            .is_err(),
        "a register that cannot answer who acted cannot be reviewed"
    );
}

/// Replaying one request resumes the row already written; a DIFFERENT payload
/// under the same evidence is an error, because two accounts of one act have to
/// be reconciled by a person.
#[test]
fn replaying_an_erasure_request_resumes_and_a_changed_payload_refuses() {
    let (_tmp, layout) = layout();
    let log = ErasureRequestLog::new(layout.clone());
    let first = log
        .record(&scope(), "alice@example.test", at(0), "ticket", "owner")
        .expect("the request records");
    let replay = log
        .record(&scope(), "alice@example.test", at(0), "ticket", "owner")
        .expect("the replay resumes");
    assert_eq!(
        first, replay,
        "a replay must resume, never stack a second row"
    );
    assert_eq!(
        log.requests_for(&scope(), "alice@example.test")
            .expect("the log reads")
            .len(),
        1,
        "a replayed request must not double-count the person who asked"
    );

    assert!(
        log.record(&scope(), "alice@example.test", at(5), "ticket", "owner")
            .is_err(),
        "two different accounts of one act must be reconciled by a person, not swallowed"
    );
}

/// A deletion proposal RETAINS every suppression row the register holds, and
/// never lists one as reachable. Deleting the row that records why somebody
/// must not be contacted is what resurrects the suppressed contact.
#[test]
fn the_erasure_proposal_retains_every_suppression_row_and_reaches_none_of_them() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    let act = dispatched_act(
        &store,
        "reachable-1",
        None,
        Some("acme"),
        &["alice@example.test"],
        at(0),
    );
    let register = SuppressionRegister::global(layout.clone());
    register
        .suppress(
            &scope().suppression(),
            "alice@example.test",
            SuppressionReason::OptOut,
            SuppressionEvidence::new(at(0), "unsub-1", "owner"),
            at(0),
        )
        .expect("the suppression records");
    let log = ErasureRequestLog::new(layout.clone());
    log.record(&scope(), "alice@example.test", at(1), "ticket", "owner")
        .expect("the request records");

    let proposal = erasure_proposal(&log, &register, &store, &scope(), "alice@example.test")
        .expect("the proposal builds");

    assert_eq!(
        proposal.accounted_for(),
        proposal.index_entries_seen,
        "every id the recipient axis offered must end under exactly one outcome: {proposal:?}"
    );
    assert!(
        proposal
            .reachable_acts
            .iter()
            .any(|record| record.reference == act),
        "the disclosure itself is what a deletion would reach, and the proposal has to say so"
    );
    assert!(
        proposal
            .retained
            .iter()
            .any(|record| record.because == RetainedBecause::SuppressionEvidence),
        "the suppression row must be RETAINED; deleting it would resurrect the suppressed contact"
    );
    assert!(
        proposal
            .retained
            .iter()
            .any(|record| record.because == RetainedBecause::TheErasureRequestItself),
        "the request itself must be retained, or honouring it would un-refuse the contact"
    );
    assert!(
        proposal
            .reachable_acts
            .iter()
            .chain(proposal.reachable_payloads.iter())
            .all(|record| record.kind != "suppression"),
        "no suppression row may ever appear as reachable"
    );
    assert!(
        !proposal.out_of_reach.is_empty(),
        "a proposal that listed only what it CAN delete reads as a complete erasure"
    );
}

/// A proposal for somebody who never asked is a deletion plan nobody requested.
#[test]
fn a_proposal_without_a_recorded_request_is_refused() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    let register = SuppressionRegister::global(layout.clone());
    let log = ErasureRequestLog::new(layout.clone());
    assert!(
        erasure_proposal(&log, &register, &store, &scope(), "alice@example.test").is_err(),
        "a deletion proposal must never come into existence for somebody who never asked"
    );
}

// ── The gate as a whole ─────────────────────────────────────────────────────

/// The module's own API must not be usable vacuously: `no recipient is refused`
/// over zero recipients is true and reads exactly like a clean bill of health.
#[test]
fn screening_an_empty_recipient_list_is_an_error() {
    let (_tmp, layout) = layout();
    assert!(
        screen(
            &layout,
            &scope(),
            &[],
            Some(OutwardChannel::Email),
            None,
            None,
            &CompliancePolicy::standard(),
            &NoJurisdictionRule,
            at(1),
        )
        .is_err(),
        "a screen over nobody must not be mistakable for a screen that cleared everybody"
    );
}

/// An unreadable outward-assertion store must refuse, never render as "nothing
/// found". Absent and unreadable are opposite facts and only one is actionable.
#[test]
fn an_unreadable_outward_store_refuses_rather_than_reading_as_empty() {
    let (_tmp, layout) = layout();
    let store = outward(&layout);
    let act = dispatched_act(
        &store,
        "corrupt-me",
        Some("outreach-q3"),
        None,
        &["alice@example.test"],
        at(0),
    );
    // A terminated, unparseable interior line: corruption, not a torn tail.
    let path = layout
        .scope_root("alpha", "prod")
        .join("outward_assertions")
        .join("acts")
        .join(format!("{act}.jsonl"));
    layout
        .append_path_sync(&path, b"this line is not a transition\n")
        .expect("the corruption writes");

    let mine = WorkContextKind::Engagement("acme".to_string());
    let decision = decide(&layout, &["alice@example.test"], Some(&mine), None, at(1));

    assert!(matches!(
        verdict_of(&decision, ComplianceRule::DuplicateRecipient),
        RuleVerdict::Unreadable { .. }
    ));
    assert!(
        !decision.is_clear(),
        "a store we could not read is never a store with nothing in it"
    );
    assert!(
        decision.refusal_message().is_some(),
        "the caller must be handed a refusal, or the send goes out on an unread register"
    );
}

/// Every decision carries one finding per rule, asserted by iterating
/// `ComplianceRule::ALL` rather than by a length check — so a rule added to the
/// enum and forgotten in `screen` fails here.
#[test]
fn every_decision_answers_all_four_questions() {
    let (_tmp, layout) = layout();
    let decision = decide(&layout, &["alice@example.test"], None, None, at(1));
    for rule in ComplianceRule::ALL {
        assert!(
            decision.finding(rule).is_some(),
            "a decision that claims to have asked four questions must carry an answer for {}",
            rule.as_str()
        );
    }
    assert_eq!(
        decision.findings.len(),
        ComplianceRule::ALL.len(),
        "no rule may be answered twice"
    );
}

/// One address supplied twice is one decision, and the decision says which
/// identities it actually covered — a count alone cannot be checked against
/// what the dispatcher is about to send.
#[test]
fn a_repeated_recipient_yields_one_screened_identity() {
    let (_tmp, layout) = layout();
    let decision = decide(
        &layout,
        &["alice@example.test", "Alice@Example.test"],
        None,
        None,
        at(1),
    );
    assert_eq!(
        decision.identities,
        vec!["alice@example.test".to_string()],
        "two spellings of one address are one person and must yield one decision"
    );
}

/// A malformed recipient fails the whole screen rather than being skipped:
/// dropping it from the CHECK while the dispatcher still holds it in the SEND
/// is fail-open wearing a different hat.
#[test]
fn a_malformed_recipient_fails_the_whole_screen() {
    let (_tmp, layout) = layout();
    assert!(
        screen(
            &layout,
            &scope(),
            &["  ".to_string()],
            Some(OutwardChannel::Email),
            None,
            None,
            &CompliancePolicy::standard(),
            &NoJurisdictionRule,
            at(1),
        )
        .is_err(),
        "`we could not parse the recipient` is not permission to contact them"
    );
}

/// A non-positive duplicate window makes rule 1 vacuous — every prior contact
/// falls outside it — so the policy refuses to be built with one.
#[test]
fn a_non_positive_duplicate_window_is_refused() {
    assert!(CompliancePolicy::new(Duration::zero()).is_err());
    assert!(CompliancePolicy::new(Duration::days(-1)).is_err());
    assert!(CompliancePolicy::new(Duration::days(1)).is_ok());
}

// ── The wrapper the dispatch path calls ─────────────────────────────────────

/// An execution with no scoped store refuses. An unconsultable register is not
/// an empty one.
#[test]
fn the_wrapper_refuses_when_there_is_no_store_to_consult() {
    let refusal = compliance_refusal(
        None,
        None,
        None,
        &["alice@example.test".to_string()],
        Some(OutwardChannel::Email),
        None,
        None,
        &CompliancePolicy::standard(),
        &NoJurisdictionRule,
        at(1),
    );
    assert!(
        refusal.is_some_and(|message| message.starts_with("NOT SENT")),
        "an unchecked recipient is never a permitted one, and the model must read the same \
         opening sentence whichever gate refused"
    );
}

/// An EMPTY recipient list passes through untouched, because
/// `outward_gate::contact_refusal` owns that decision and has already made it.
/// Re-deciding it here would need this generic coordinator to learn what an
/// `OutwardClass` is.
#[test]
fn the_wrapper_leaves_the_empty_recipient_decision_to_the_gate_that_owns_it() {
    let (_tmp, layout) = layout();
    assert!(
        compliance_refusal(
            Some(&layout),
            Some("alpha"),
            Some("prod"),
            &[],
            Some(OutwardChannel::Email),
            None,
            None,
            &CompliancePolicy::standard(),
            &NoJurisdictionRule,
            at(1),
        )
        .is_none(),
        "all four rules are per-identity and have nothing to say about nobody"
    );
}

/// The wrapper's refusal carries the rule that refused, so the model and the
/// owner both learn WHICH question came back no.
#[test]
fn the_wrapper_names_the_rule_that_refused() {
    let (_tmp, layout) = layout();
    ErasureRequestLog::new(layout.clone())
        .record(&scope(), "alice@example.test", at(0), "ticket", "owner")
        .expect("the request records");

    let refusal = compliance_refusal(
        Some(&layout),
        Some("alpha"),
        Some("prod"),
        &["alice@example.test".to_string()],
        Some(OutwardChannel::Email),
        None,
        None,
        &CompliancePolicy::standard(),
        &NoJurisdictionRule,
        at(1),
    )
    .expect("a recorded erasure request refuses");

    assert!(
        refusal.contains("erasure_requested"),
        "a refusal that does not say which rule refused leaves the caller guessing: {refusal}"
    );
    assert!(
        refusal.contains("not be applied"),
        "the refusal must also report the rules that could not be applied, or a decision with \
         three unassessed rules reads like a fully checked one: {refusal}"
    );
}
