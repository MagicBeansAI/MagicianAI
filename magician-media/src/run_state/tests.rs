//! §5's contract, as behaviour.

use chrono::{DateTime, Duration, TimeZone, Utc};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::AudienceRef;

use super::store::{RunScope, RunStateStore};
use super::types::{Answer, Run, RunState};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
}

fn later(minutes: i64) -> DateTime<Utc> {
    now() + Duration::minutes(minutes)
}

fn fixture() -> (tempfile::TempDir, RunStateStore, RunScope) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let runs = RunStateStore::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, runs, RunScope::new("anonymous", "default"))
}

fn grounded(text: &str, refs: &[&str]) -> Answer {
    Answer {
        text: text.to_string(),
        evidence_refs: refs.iter().map(|held| held.to_string()).collect(),
    }
}

fn form_fields() -> Vec<(String, bool)> {
    vec![
        ("legal_name".to_string(), true),
        ("annual_revenue".to_string(), true),
        ("press_kit".to_string(), false),
    ]
}

/// The run's log file on disk — found, not derived: file names are a hash of
/// the run id, so the tests locate the one log in the scope rather than
/// reimplementing the hash.
fn run_log_path(tmp: &tempfile::TempDir, scope: &RunScope) -> std::path::PathBuf {
    let dir = ArtifactV2Workspace::new(tmp.path())
        .scope_root(&scope.principal, &scope.workspace)
        .join("run_state");
    let mut logs: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("run_state dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert_eq!(logs.len(), 1, "expected exactly one run log in the scope");
    logs.remove(0)
}

fn open_run(runs: &RunStateStore, scope: &RunScope) -> Run {
    runs.open(
        scope,
        "sbir phase one application",
        "portal:grants/form-7",
        Some(AudienceRef::account("grantor")),
        &form_fields(),
        "company-assistant",
        now(),
    )
    .expect("open")
}

fn ready_run(runs: &RunStateStore, scope: &RunScope) -> Run {
    let run = open_run(runs, scope);
    runs.record_answer(
        scope,
        &run.run_id,
        "legal_name",
        &grounded("Acme Ltd", &["doc:certificate-of-incorporation"]),
        later(1),
    )
    .expect("legal_name");
    runs.record_answer(
        scope,
        &run.run_id,
        "annual_revenue",
        &grounded("2.4M", &["doc:fy25-accounts"]),
        later(2),
    )
    .expect("annual_revenue")
}

/// §5: "A form abandoned halfway is worse than one never started." A crashed
/// session that opens again must land on THE run — same id, answers intact —
/// not on a duplicate beside it.
#[test]
fn a_crashed_session_resumes_the_run_it_opened() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    let answer = grounded("Acme Ltd", &["doc:certificate-of-incorporation"]);
    runs.record_answer(&scope, &run.run_id, "legal_name", &answer, later(5))
        .expect("answer");

    // The next session re-states the purpose loosely and knows nothing else —
    // no audience, no field roster.
    let resumed = runs
        .open(
            &scope,
            "  SBIR   Phase One   APPLICATION ",
            "portal:grants/form-7",
            None,
            &[],
            "another-session",
            later(60 * 24),
        )
        .expect("resume");

    assert_eq!(resumed.run_id, run.run_id);
    assert_eq!(
        resumed.opened_at,
        now(),
        "resuming must not move when the run began"
    );
    assert_eq!(resumed.opened_by, "company-assistant");
    assert_eq!(resumed.audience, Some(AudienceRef::account("grantor")));
    assert_eq!(
        resumed.fields.len(),
        3,
        "the roster comes from the first opening"
    );
    assert_eq!(
        resumed.field("legal_name").expect("field").answer,
        Some(answer)
    );
}

/// §5: "every emitted answer cites retrievable evidence — never answer from
/// nothing." An answer with no evidence refs is refused, never stored.
#[test]
fn an_answer_citing_no_evidence_is_refused() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);

    let empty = Answer {
        text: "Acme Ltd".to_string(),
        evidence_refs: Vec::new(),
    };
    assert!(runs
        .record_answer(&scope, &run.run_id, "legal_name", &empty, later(1))
        .is_err());

    // A blank ref cites nothing just as surely as no ref does.
    let blank = Answer {
        text: "Acme Ltd".to_string(),
        evidence_refs: vec!["   ".to_string()],
    };
    assert!(runs
        .record_answer(&scope, &run.run_id, "legal_name", &blank, later(2))
        .is_err());

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    let field = held.field("legal_name").expect("field");
    assert_eq!(field.answer, None, "the refusal must leave nothing behind");
    assert_eq!(field.revision, 0);
}

/// Fail closed: a name the run never declared is refused rather than adopted —
/// a typo must not create a phantom field no review ever sees.
#[test]
fn an_undeclared_field_is_refused_everywhere() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);

    assert!(runs
        .record_answer(
            &scope,
            &run.run_id,
            "legal_nmae",
            &grounded("Acme Ltd", &["doc:certificate-of-incorporation"]),
            later(1),
        )
        .is_err());
    assert!(runs
        .raise_gap(
            &scope,
            &run.run_id,
            "legal_nmae",
            "what is the registered name?",
            later(2)
        )
        .is_err());
    assert_eq!(
        runs.load(&scope, &run.run_id)
            .expect("load")
            .expect("run")
            .fields
            .len(),
        3
    );
}

/// §5: "a question the evidence store cannot ground becomes an owner-facing
/// gap, never synthesised text. The owner's reply is new evidence." End to
/// end: raise, resolve by a named person citing the reply, and the field
/// carries the answer — one truth — leaving the run ready.
#[test]
fn the_gap_path_grounds_a_question_end_to_end() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    runs.record_answer(
        &scope,
        &run.run_id,
        "legal_name",
        &grounded("Acme Ltd", &["doc:certificate-of-incorporation"]),
        later(1),
    )
    .expect("legal_name");

    let raised = runs
        .raise_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "What was revenue for the last financial year?",
            later(2),
        )
        .expect("raise");
    assert_eq!(
        raised.state(),
        RunState::Drafting,
        "an open gap on a required field is not ready"
    );

    let reply = grounded("2.4M", &["message:owner-reply-14"]);
    let resolved = runs
        .resolve_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "What was revenue for the last financial year?",
            &reply,
            "owner",
            later(3),
        )
        .expect("resolve");

    let gap = &resolved.gaps[0];
    let resolution = gap.resolution.as_ref().expect("resolution");
    assert_eq!(resolution.resolved_by, "owner");
    assert_eq!(resolution.at, later(3));
    assert_eq!(resolution.answer, reply);
    assert_eq!(
        resolved.field("annual_revenue").expect("field").answer,
        Some(reply.clone()),
        "the resolution writes the field too — one truth"
    );
    assert_eq!(resolved.state(), RunState::ReadyForReview);
}

/// An unattributed resolution is synthesised text wearing an owner's
/// authority; a nameless resolver is refused and the gap stays open.
#[test]
fn an_unnamed_gap_resolution_is_refused() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    runs.raise_gap(
        &scope,
        &run.run_id,
        "annual_revenue",
        "What was last year's revenue?",
        later(1),
    )
    .expect("raise");

    let reply = grounded("2.4M", &["message:owner-reply-14"]);
    assert!(runs
        .resolve_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "What was last year's revenue?",
            &reply,
            "   ",
            later(2),
        )
        .is_err());

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert!(
        held.gaps[0].is_open(),
        "a refused resolution must leave the gap open"
    );
    assert_eq!(held.field("annual_revenue").expect("field").answer, None);
}

/// "The owner's reply is new evidence" — so even a resolution must clear the
/// grounding bar. A reply with no citable ref is refused.
#[test]
fn a_gap_cannot_be_resolved_with_ungrounded_text() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    runs.raise_gap(
        &scope,
        &run.run_id,
        "annual_revenue",
        "What was last year's revenue?",
        later(1),
    )
    .expect("raise");

    let synthesised = Answer {
        text: "2.4M".to_string(),
        evidence_refs: Vec::new(),
    };
    assert!(runs
        .resolve_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "What was last year's revenue?",
            &synthesised,
            "owner",
            later(2),
        )
        .is_err());

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert!(held.gaps[0].is_open());
    assert_eq!(held.field("annual_revenue").expect("field").answer, None);
}

/// A reply must land on the question that raised it, and the owner's first
/// reply is the record — a later one must not silently replace it.
#[test]
fn resolving_an_unknown_or_settled_gap_is_refused() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);

    // Never raised.
    assert!(runs
        .resolve_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "was there revenue at all?",
            &grounded("2.4M", &["message:owner-reply-1"]),
            "owner",
            later(1),
        )
        .is_err());

    runs.raise_gap(
        &scope,
        &run.run_id,
        "annual_revenue",
        "What was last year's revenue?",
        later(2),
    )
    .expect("raise");
    runs.resolve_gap(
        &scope,
        &run.run_id,
        "annual_revenue",
        "What was last year's revenue?",
        &grounded("2.4M", &["message:owner-reply-1"]),
        "owner",
        later(3),
    )
    .expect("resolve");

    // The gap's identity survives spacing and case, so this is the SAME gap —
    // and it is already settled.
    assert!(runs
        .resolve_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "what was LAST year's revenue?",
            &grounded("2.6M", &["message:owner-reply-2"]),
            "someone-else",
            later(4),
        )
        .is_err());

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    let resolution = held.gaps[0].resolution.as_ref().expect("resolution");
    assert_eq!(resolution.resolved_by, "owner");
    assert_eq!(resolution.answer.text, "2.4M");
}

/// §5: "the agent must read its own inbox mid-flow, extract the code, and
/// continue" — the reading is the caller's; here the wait holds the run in
/// awaiting_external until the extracted event is fed back in.
#[test]
fn an_open_expectation_holds_the_run_awaiting_external() {
    let (_tmp, runs, scope) = fixture();
    let run = ready_run(&runs, &scope);
    assert_eq!(run.state(), RunState::ReadyForReview);

    let wait = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "verification code for the portal login",
            "inbox:company-assistant",
            later(3),
        )
        .expect("raise");
    assert!(wait.is_open());
    assert_eq!(
        runs.load(&scope, &run.run_id)
            .expect("load")
            .expect("run")
            .state(),
        RunState::AwaitingExternal
    );

    let fulfilled = runs
        .fulfill_expectation(
            &scope,
            &run.run_id,
            &wait.expectation_id,
            "message:portal-code-arrival",
            later(9),
        )
        .expect("fulfill");
    assert_eq!(
        fulfilled.state(),
        RunState::ReadyForReview,
        "the run leaves the wait the moment the event lands"
    );
    let fulfilment = fulfilled.expectations[0]
        .fulfilled
        .as_ref()
        .expect("fulfilment");
    assert_eq!(fulfilment.event_ref, "message:portal-code-arrival");
    assert_eq!(fulfilment.at, later(9));
}

/// Ids are derived, so the same wait re-raised by a resumed session is the
/// same expectation — not a second one holding the run hostage forever.
#[test]
fn the_same_wait_raised_twice_is_one_expectation() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);

    let first = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "verification code for the portal login",
            "inbox:company-assistant",
            later(1),
        )
        .expect("first");
    let again = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "  Verification   CODE for the portal login ",
            "inbox:company-assistant",
            later(2),
        )
        .expect("again");

    assert_eq!(again.expectation_id, first.expectation_id);
    assert_eq!(
        again.raised_at, first.raised_at,
        "the replay must not move when the wait began"
    );
    assert_eq!(
        runs.load(&scope, &run.run_id)
            .expect("load")
            .expect("run")
            .expectations
            .len(),
        1
    );

    // The same words awaited on a different channel are a different wait.
    let elsewhere = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "verification code for the portal login",
            "phone:sms",
            later(3),
        )
        .expect("elsewhere");
    assert_ne!(elsewhere.expectation_id, first.expectation_id);
}

/// Feeding an event to a wait this run never raised is how a stray email
/// resumes the wrong run; an unknown id is refused, and so is a fulfilment
/// that cites no event.
#[test]
fn an_event_for_an_unknown_wait_is_refused() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    assert!(runs
        .fulfill_expectation(
            &scope,
            &run.run_id,
            "exp-unknown",
            "message:stray",
            later(1)
        )
        .is_err());

    let wait = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "verification code for the portal login",
            "inbox:company-assistant",
            later(2),
        )
        .expect("raise");
    assert!(runs
        .fulfill_expectation(&scope, &run.run_id, &wait.expectation_id, "   ", later(3))
        .is_err());
    assert!(runs
        .load(&scope, &run.run_id)
        .expect("load")
        .expect("run")
        .expectations[0]
        .is_open());
}

/// Which event satisfied a wait is a fact; a second event must not rewrite it.
#[test]
fn a_second_event_cannot_rewrite_a_fulfilled_wait() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    let wait = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "verification code for the portal login",
            "inbox:company-assistant",
            later(1),
        )
        .expect("raise");
    runs.fulfill_expectation(
        &scope,
        &run.run_id,
        &wait.expectation_id,
        "message:code-1",
        later(2),
    )
    .expect("first");

    assert!(runs
        .fulfill_expectation(
            &scope,
            &run.run_id,
            &wait.expectation_id,
            "message:code-2",
            later(3)
        )
        .is_err());

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    let fulfilment = held.expectations[0].fulfilled.as_ref().expect("fulfilment");
    assert_eq!(fulfilment.event_ref, "message:code-1");
    assert_eq!(fulfilment.at, later(2));
}

/// Ready needs every required field answered; one unanswered required field
/// holds the run in drafting.
#[test]
fn ready_needs_every_required_field_answered() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);

    let half = runs
        .record_answer(
            &scope,
            &run.run_id,
            "legal_name",
            &grounded("Acme Ltd", &["doc:certificate-of-incorporation"]),
            later(1),
        )
        .expect("legal_name");
    assert_eq!(half.state(), RunState::Drafting);

    let full = runs
        .record_answer(
            &scope,
            &run.run_id,
            "annual_revenue",
            &grounded("2.4M", &["doc:fy25-accounts"]),
            later(2),
        )
        .expect("annual_revenue");
    assert_eq!(full.state(), RunState::ReadyForReview);
}

/// Ready needs no open gap on a required field — even one whose field already
/// holds an answer, because an open question means the answer is in doubt.
#[test]
fn ready_needs_no_open_gap_on_a_required_field() {
    let (_tmp, runs, scope) = fixture();
    let run = ready_run(&runs, &scope);

    let doubted = runs
        .raise_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "Does that figure include the grant?",
            later(3),
        )
        .expect("raise");
    assert_eq!(doubted.state(), RunState::Drafting);

    let resolved = runs
        .resolve_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "Does that figure include the grant?",
            &grounded("2.4M excluding the grant", &["message:owner-reply-2"]),
            "owner",
            later(4),
        )
        .expect("resolve");
    assert_eq!(resolved.state(), RunState::ReadyForReview);
    assert_eq!(
        resolved
            .field("annual_revenue")
            .expect("field")
            .answer
            .as_ref()
            .expect("answer")
            .text,
        "2.4M excluding the grant"
    );
}

/// An optional field left pending does not block ready, and neither does an
/// open gap on one: the gate protects required content, while the pending
/// field and its open question stay visible for the review to weigh.
#[test]
fn an_unanswered_optional_field_does_not_block_ready() {
    let (_tmp, runs, scope) = fixture();
    let run = ready_run(&runs, &scope);
    assert_eq!(run.field("press_kit").expect("field").answer, None);
    assert_eq!(run.state(), RunState::ReadyForReview);

    let held = runs
        .raise_gap(
            &scope,
            &run.run_id,
            "press_kit",
            "Is there a current press kit?",
            later(3),
        )
        .expect("raise");
    assert_eq!(held.state(), RunState::ReadyForReview);
    assert!(
        held.gaps[0].is_open(),
        "the question stays visible even though it does not gate"
    );
}

/// Vacuous truth is a bug class: "every required field answered" must not hold
/// over a form with no fields at all, or an empty run would sail to the gate.
#[test]
fn a_run_with_no_fields_is_never_ready() {
    let (_tmp, runs, scope) = fixture();
    let run = runs
        .open(
            &scope,
            "empty shell",
            "portal:grants/form-9",
            None,
            &[],
            "company-assistant",
            now(),
        )
        .expect("open");
    assert_eq!(run.state(), RunState::Drafting);
    assert!(runs
        .record_submission(
            &scope,
            &run.run_id,
            "owner",
            "act:reviewed-batch-1",
            later(1)
        )
        .is_err());

    let declared = runs
        .declare_field(&scope, &run.run_id, "legal_name", true, later(2))
        .expect("declare");
    assert_eq!(declared.state(), RunState::Drafting);

    let answered = runs
        .record_answer(
            &scope,
            &run.run_id,
            "legal_name",
            &grounded("Acme Ltd", &["doc:certificate-of-incorporation"]),
            later(3),
        )
        .expect("answer");
    assert_eq!(answered.state(), RunState::ReadyForReview);
}

/// Forms reveal conditional sections; a field declared mid-run joins the
/// roster pending, and re-declaring it is a no-op — the first declaration,
/// including its shape, wins.
#[test]
fn a_conditional_section_declares_its_field_mid_run() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);

    let held = runs
        .declare_field(&scope, &run.run_id, "export_licence", true, later(1))
        .expect("declare");
    let field = held.field("export_licence").expect("field");
    assert_eq!(field.answer, None);
    assert_eq!(field.revision, 0);
    assert!(field.required);
    assert_eq!(held.fields.len(), 4);

    let again = runs
        .declare_field(&scope, &run.run_id, "export_licence", false, later(2))
        .expect("redeclare");
    assert_eq!(again.fields.len(), 4);
    assert!(
        again.field("export_licence").expect("field").required,
        "the first declaration's shape wins"
    );
}

/// §5: "The agent drafts the whole thing; a person presses send." Submission
/// takes readiness, a named person and the reviewed act's ref — the module has
/// no path to submitted without all three.
#[test]
fn submission_needs_ready_a_named_person_and_a_reviewed_act() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    // Not ready: required fields are unanswered.
    assert!(runs
        .record_submission(
            &scope,
            &run.run_id,
            "owner",
            "act:reviewed-batch-1",
            later(1)
        )
        .is_err());

    let run = ready_run(&runs, &scope);
    assert!(
        runs.record_submission(&scope, &run.run_id, "  ", "act:reviewed-batch-1", later(3))
            .is_err(),
        "an unnamed submission is how an agent would fire the gate itself"
    );
    assert!(
        runs.record_submission(&scope, &run.run_id, "owner", "", later(4))
            .is_err(),
        "a send with no reviewed act behind it is a standing envelope"
    );

    let sent = runs
        .record_submission(
            &scope,
            &run.run_id,
            "owner",
            "act:reviewed-batch-1",
            later(5),
        )
        .expect("submit");
    let submission = sent.submission.as_ref().expect("submission");
    assert_eq!(submission.by, "owner");
    assert_eq!(submission.act_ref, "act:reviewed-batch-1");
    assert_eq!(submission.at, later(5));
    assert_eq!(sent.state(), RunState::Submitted);
}

/// The first submission stands; a second send on a submitted form is the
/// exact event the gate exists to prevent.
#[test]
fn the_first_submission_stands() {
    let (_tmp, runs, scope) = fixture();
    let run = ready_run(&runs, &scope);
    runs.record_submission(
        &scope,
        &run.run_id,
        "owner",
        "act:reviewed-batch-1",
        later(3),
    )
    .expect("first");

    assert!(runs
        .record_submission(
            &scope,
            &run.run_id,
            "someone-else",
            "act:reviewed-batch-2",
            later(9)
        )
        .is_err());

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    let submission = held.submission.as_ref().expect("submission");
    assert_eq!(submission.by, "owner");
    assert_eq!(submission.act_ref, "act:reviewed-batch-1");
    assert_eq!(submission.at, later(3));
}

/// After submission the log is evidence of what went out; every mutation —
/// including resolving a gap the run was submitted with — refuses, and the
/// run reads back exactly as it was sent. The gap rides on the OPTIONAL
/// field, because that is the one kind of open question a submittable run
/// can still carry.
#[test]
fn after_submission_the_record_is_frozen() {
    let (_tmp, runs, scope) = fixture();
    let run = ready_run(&runs, &scope);
    // An open gap on the optional field does not block the gate, so the run
    // goes out with its question still open — and that open question is part
    // of the frozen record.
    runs.raise_gap(
        &scope,
        &run.run_id,
        "press_kit",
        "Is there a current press kit?",
        later(2),
    )
    .expect("raise before submission");
    runs.record_submission(
        &scope,
        &run.run_id,
        "owner",
        "act:reviewed-batch-1",
        later(3),
    )
    .expect("submit");

    assert!(runs
        .record_answer(
            &scope,
            &run.run_id,
            "legal_name",
            &grounded("Acme Limited", &["doc:companies-register"]),
            later(4),
        )
        .is_err());
    assert!(runs
        .raise_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "second thoughts?",
            later(5)
        )
        .is_err());
    assert!(runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "late verification",
            "inbox:company-assistant",
            later(6)
        )
        .is_err());
    assert!(runs
        .declare_field(&scope, &run.run_id, "afterthought", false, later(7))
        .is_err());
    assert!(runs
        .fulfill_expectation(
            &scope,
            &run.run_id,
            "exp-unknown",
            "message:stray",
            later(8)
        )
        .is_err());
    assert!(
        runs.resolve_gap(
            &scope,
            &run.run_id,
            "press_kit",
            "Is there a current press kit?",
            &grounded("Yes, refreshed last month", &["message:owner-reply-31"]),
            "owner",
            later(9),
        )
        .is_err(),
        "resolving a submitted run's gap would edit the evidence of what went out"
    );

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert_eq!(
        held.field("legal_name")
            .expect("field")
            .answer
            .as_ref()
            .expect("answer")
            .text,
        "Acme Ltd"
    );
    assert_eq!(held.fields.len(), 3);
    assert_eq!(
        held.gaps.len(),
        1,
        "the question the run went out with is part of the record"
    );
    assert!(
        held.gaps[0].is_open(),
        "the refused resolution must leave the gap exactly as sent"
    );
    assert_eq!(held.field("press_kit").expect("field").answer, None);
    assert!(held.expectations.is_empty());
}

/// §5's "what changed since": strictly above the watermark, in change order —
/// and a never-filled field never appears, because pending is not a change.
#[test]
fn changed_since_reports_exactly_what_moved_past_the_watermark() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    runs.record_answer(
        &scope,
        &run.run_id,
        "legal_name",
        &grounded("Acme Ltd", &["doc:certificate-of-incorporation"]),
        later(1),
    )
    .expect("legal_name");
    runs.record_answer(
        &scope,
        &run.run_id,
        "annual_revenue",
        &grounded("2.4M", &["doc:fy25-accounts"]),
        later(2),
    )
    .expect("annual_revenue");

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    let names = |watermark: u64| {
        held.changed_since(watermark)
            .iter()
            .map(|field| field.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(0), vec!["legal_name", "annual_revenue"]);
    assert_eq!(names(1), vec!["annual_revenue"]);
    assert_eq!(names(2), Vec::<String>::new());

    assert_eq!(held.field("legal_name").expect("field").revision, 1);
    assert_eq!(held.field("annual_revenue").expect("field").revision, 2);
    assert_eq!(held.field("press_kit").expect("field").revision, 0);
    assert_eq!(held.revision_high_water(), 2);
}

/// Revisions are monotonic per field, and overwriting an answer before
/// submission is allowed — the append-only log keeps every revision as
/// history.
#[test]
fn revisions_are_monotonic_per_field_and_overwrites_are_allowed() {
    let (_tmp, runs, scope) = fixture();
    let run = ready_run(&runs, &scope); // legal_name at revision 1, annual_revenue at 2

    let redone = runs
        .record_answer(
            &scope,
            &run.run_id,
            "legal_name",
            &grounded("Acme Limited", &["doc:companies-register"]),
            later(3),
        )
        .expect("overwrite while drafting");
    let field = redone.field("legal_name").expect("field");
    assert_eq!(
        field.revision, 3,
        "the field's own sequence strictly increases"
    );
    assert_eq!(field.answer.as_ref().expect("answer").text, "Acme Limited");

    // Overwriting continues while the run awaits an external event.
    runs.raise_expectation(
        &scope,
        &run.run_id,
        "verification code for the portal login",
        "inbox:company-assistant",
        later(4),
    )
    .expect("raise");
    let held = runs
        .record_answer(
            &scope,
            &run.run_id,
            "annual_revenue",
            &grounded("2.5M", &["doc:fy25-accounts-revised"]),
            later(5),
        )
        .expect("overwrite while awaiting");
    assert_eq!(held.state(), RunState::AwaitingExternal);
    assert_eq!(held.field("annual_revenue").expect("field").revision, 4);
}

/// Runs do not leak: identity carries the scope, so the same work by two
/// principals never shares a half-filled form — and a different purpose
/// against the same portal is a different run.
#[test]
fn runs_do_not_leak_across_scopes_or_purposes() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    runs.record_answer(
        &scope,
        &run.run_id,
        "legal_name",
        &grounded("Acme Ltd", &["doc:certificate-of-incorporation"]),
        later(1),
    )
    .expect("answer");

    let other_scope = RunScope::new("someone-else", "default");
    let other = runs
        .open(
            &other_scope,
            "sbir phase one application",
            "portal:grants/form-7",
            None,
            &form_fields(),
            "company-assistant",
            now(),
        )
        .expect("other scope");
    assert_ne!(other.run_id, run.run_id);
    assert_eq!(other.field("legal_name").expect("field").answer, None);
    assert!(
        runs.load(&other_scope, &run.run_id)
            .expect("load")
            .is_none(),
        "one scope cannot read another's run"
    );

    let other_purpose = runs
        .open(
            &scope,
            "sttr phase one application",
            "portal:grants/form-7",
            None,
            &form_fields(),
            "company-assistant",
            now(),
        )
        .expect("other purpose");
    assert_ne!(
        other_purpose.run_id, run.run_id,
        "a different undertaking against the same portal is a different run"
    );
}

/// The crash this module exists to survive must not brick the log it wrote:
/// a session dying mid-append leaves a partial fragment as the run's last
/// line, and before the fold tolerated a torn tail, every later load —
/// including the open() a resuming session starts with — errored forever.
#[test]
fn a_torn_final_line_does_not_brick_the_run() {
    let (tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    let answer = grounded("Acme Ltd", &["doc:certificate-of-incorporation"]);
    runs.record_answer(&scope, &run.run_id, "legal_name", &answer, later(1))
        .expect("answer");

    // The crash: a partial record, no trailing newline, at the tail.
    let log = run_log_path(&tmp, &scope);
    let mut raw = std::fs::read(&log).expect("read log");
    raw.extend_from_slice(b"{\"record\":\"answer_rec");
    std::fs::write(&log, &raw).expect("tear the tail");

    let held = runs
        .load(&scope, &run.run_id)
        .expect("a torn tail must not brick the run")
        .expect("run");
    let field = held.field("legal_name").expect("field");
    assert_eq!(field.answer, Some(answer.clone()));
    assert_eq!(
        field.revision, 1,
        "the torn append never happened; nothing else moved"
    );

    // And the resuming session lands on THE run, answers intact — not on an
    // error, and not on a fabricated duplicate.
    let resumed = runs
        .open(
            &scope,
            "sbir phase one application",
            "portal:grants/form-7",
            None,
            &[],
            "resuming-session",
            later(60),
        )
        .expect("open must resume over a torn tail");
    assert_eq!(resumed.run_id, run.run_id);
    assert_eq!(resumed.opened_by, "company-assistant");
    assert_eq!(
        resumed.field("legal_name").expect("field").answer,
        Some(answer)
    );
}

/// An unreadable log must surface as an ERROR, never as "never opened":
/// before read_if_present distinguished NotFound from other failures, a
/// transient read fault made load() report no run — and open() would
/// fabricate a blank run over a form holding days of answers.
#[cfg(unix)]
#[test]
fn an_unreadable_log_is_an_error_not_an_absent_run() {
    use std::os::unix::fs::PermissionsExt;

    let (tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);
    let answer = grounded("Acme Ltd", &["doc:certificate-of-incorporation"]);
    runs.record_answer(&scope, &run.run_id, "legal_name", &answer, later(1))
        .expect("answer");

    let log = run_log_path(&tmp, &scope);
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).expect("deny reads");

    assert!(
        runs.load(&scope, &run.run_id).is_err(),
        "an unreadable log must never read as an absent run"
    );
    assert!(
        runs.open(
            &scope,
            "sbir phase one application",
            "portal:grants/form-7",
            None,
            &form_fields(),
            "another-session",
            later(60),
        )
        .is_err(),
        "open must not fabricate a blank run over a log it cannot read"
    );

    // The fault was transient: readable again, the run holds everything it
    // held — the refusals above wrote nothing over it.
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).expect("restore");
    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert_eq!(held.opened_at, now());
    assert_eq!(held.opened_by, "company-assistant");
    assert_eq!(
        held.field("legal_name").expect("field").answer,
        Some(answer)
    );
}

/// record_submission's already-submitted check is check-then-append with no
/// lock, so two racing sessions can BOTH pass it and BOTH append — the second
/// send silently authorised by the gate built to prevent it. The fold is the
/// arbiter that keeps the gate meaningful: the FIRST Submitted record wins,
/// and the run reads back with exactly one submission however the race
/// interleaved.
#[test]
fn racing_submissions_converge_on_the_first_record() {
    let (tmp, runs, scope) = fixture();
    let run = ready_run(&runs, &scope);
    runs.record_submission(
        &scope,
        &run.run_id,
        "owner",
        "act:reviewed-batch-1",
        later(3),
    )
    .expect("winner");

    // The losing racer read `submission: None` before the winner's append
    // landed, so its own append hits the log past the check — replayed here
    // as the bytes it would have written, since a single-threaded test cannot
    // race the check itself.
    let loser = "{\"record\":\"submitted\",\"by\":\"someone-else\",\
                 \"act_ref\":\"act:reviewed-batch-2\",\"at\":\"2026-08-20T12:09:00Z\"}\n";
    let log = run_log_path(&tmp, &scope);
    let mut raw = std::fs::read(&log).expect("read log");
    raw.extend_from_slice(loser.as_bytes());
    std::fs::write(&log, &raw).expect("loser's append");

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    let submission = held.submission.as_ref().expect("submission");
    assert_eq!(submission.by, "owner");
    assert_eq!(submission.act_ref, "act:reviewed-batch-1");
    assert_eq!(submission.at, later(3));
    assert_eq!(held.state(), RunState::Submitted);
}

/// The resume story invites replay, and every other write absorbs one; until
/// this was pinned, re-recording the identical answer appended a fresh record
/// and bumped the revision, so changed_since reported a change when nothing
/// changed. A byte-identical replay is a no-op; a different answer — even the
/// same text on different evidence — still bumps.
#[test]
fn replaying_the_identical_answer_changes_nothing() {
    let (_tmp, runs, scope) = fixture();
    let run = ready_run(&runs, &scope); // legal_name at revision 1, annual_revenue at 2
    let answer = grounded("Acme Ltd", &["doc:certificate-of-incorporation"]);

    let replayed = runs
        .record_answer(&scope, &run.run_id, "legal_name", &answer, later(30))
        .expect("replay");
    let field = replayed.field("legal_name").expect("field");
    assert_eq!(field.revision, 1, "a replay must not burn a revision");
    assert_eq!(field.updated_at, later(1), "a replay is not an update");
    assert_eq!(field.answer, Some(answer));
    assert_eq!(replayed.revision_high_water(), 2);
    assert!(
        replayed.changed_since(2).is_empty(),
        "a watermark held across the replay must report nothing"
    );

    // Same text, different evidence: that IS a change — the grounding moved.
    let regrounded = runs
        .record_answer(
            &scope,
            &run.run_id,
            "legal_name",
            &grounded("Acme Ltd", &["doc:companies-register"]),
            later(31),
        )
        .expect("different evidence");
    assert_eq!(regrounded.field("legal_name").expect("field").revision, 3);
    let changed: Vec<&str> = regrounded
        .changed_since(2)
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(changed, vec!["legal_name"]);
}

/// The id separator survives whitespace normalisation, so a crafted pair of
/// identity components could shift bytes across it and fuse two distinct
/// undertakings into one run id; open() refuses either component carrying it,
/// and a refused open leaves nothing on disk.
#[test]
fn an_identity_component_carrying_the_separator_is_refused() {
    let (tmp, runs, scope) = fixture();
    assert!(runs
        .open(
            &scope,
            "sbir\u{1f}phase one application",
            "portal:grants/form-7",
            None,
            &form_fields(),
            "company-assistant",
            now(),
        )
        .is_err());
    assert!(runs
        .open(
            &scope,
            "sbir phase one application",
            "portal:grants\u{1f}form-7",
            None,
            &form_fields(),
            "company-assistant",
            now(),
        )
        .is_err());

    let dir = ArtifactV2Workspace::new(tmp.path())
        .scope_root(&scope.principal, &scope.workspace)
        .join("run_state");
    assert!(!dir.exists(), "a refused open must write nothing");
}

/// derive_run_id joins principal, workspace, purpose and resource_ref with
/// U+001F, but open() guarded only purpose and resource_ref — so the scopes
/// ("anonymous\u{1f}default", "extra") and ("anonymous", "default\u{1f}extra")
/// fed derivation the identical byte string and fused two scopes' work into
/// ONE run id, the exact cross-principal merge scope isolation exists to
/// prevent.
#[test]
fn a_scope_component_carrying_the_separator_is_refused() {
    let (tmp, runs, scope) = fixture();

    let fused_left = RunScope::new("anonymous\u{1f}default", "extra");
    let error = runs
        .open(
            &fused_left,
            "sbir phase one application",
            "portal:grants/form-7",
            None,
            &form_fields(),
            "company-assistant",
            now(),
        )
        .expect_err("a principal carrying the separator must be refused");
    assert!(error.to_string().contains("U+001F"), "{error}");

    let fused_right = RunScope::new("anonymous", "default\u{1f}extra");
    let error = runs
        .open(
            &fused_right,
            "sbir phase one application",
            "portal:grants/form-7",
            None,
            &form_fields(),
            "company-assistant",
            now(),
        )
        .expect_err("a workspace carrying the separator must be refused");
    assert!(error.to_string().contains("U+001F"), "{error}");

    for refused in [&fused_left, &fused_right] {
        let dir = ArtifactV2Workspace::new(tmp.path())
            .scope_root(&refused.principal, &refused.workspace)
            .join("run_state");
        assert!(!dir.exists(), "a refused open must write nothing");
    }

    // A clean scope still opens — the guard does not block real work.
    let run = open_run(&runs, &scope);
    assert_eq!(run.fields.len(), 3);
    assert_eq!(run.opened_by, "company-assistant");
}

/// Field names feed gap_key, joined to the question with U+001F, but neither
/// open() nor declare_field() refused a name carrying the separator — so the
/// field "a\u{1f}b" with question "c" and the field "a" with question
/// "b\u{1f}c" produced the SAME gap key, fusing two different unknowns into
/// one gap.
#[test]
fn a_field_name_carrying_the_separator_is_refused() {
    let (_tmp, runs, scope) = fixture();

    let error = runs
        .open(
            &scope,
            "sbir phase one application",
            "portal:grants/form-7",
            None,
            &[("legal\u{1f}name".to_string(), true)],
            "company-assistant",
            now(),
        )
        .expect_err("an opening field name carrying the separator must be refused");
    assert!(error.to_string().contains("U+001F"), "{error}");

    let run = open_run(&runs, &scope);
    let error = runs
        .declare_field(&scope, &run.run_id, "export\u{1f}licence", true, later(1))
        .expect_err("a declared field name carrying the separator must be refused");
    assert!(error.to_string().contains("U+001F"), "{error}");

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert_eq!(
        held.fields.len(),
        3,
        "the refused declaration must add nothing"
    );

    // A clean declaration still lands.
    let declared = runs
        .declare_field(&scope, &run.run_id, "export_licence", true, later(2))
        .expect("clean declare");
    assert_eq!(declared.fields.len(), 4);
    assert!(declared.field("export_licence").expect("field").required);
}

/// gap_key joins field and question with U+001F, but raise_gap and
/// resolve_gap accepted a question carrying the separator — a crafted
/// question could shift bytes across the key and fuse two different unknowns
/// into one gap, one owner reply silently answering both.
#[test]
fn a_gap_question_carrying_the_separator_is_refused() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);

    let error = runs
        .raise_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "gross\u{1f}or net?",
            later(1),
        )
        .expect_err("a raised question carrying the separator must be refused");
    assert!(error.to_string().contains("U+001F"), "{error}");
    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert_eq!(held.gaps.len(), 0, "the refused gap must add nothing");

    runs.raise_gap(
        &scope,
        &run.run_id,
        "annual_revenue",
        "gross or net?",
        later(2),
    )
    .expect("clean raise");
    let error = runs
        .resolve_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "gross\u{1f}or net?",
            &grounded("net", &["message:owner-reply-9"]),
            "owner",
            later(3),
        )
        .expect_err("a resolving question carrying the separator must be refused");
    assert!(error.to_string().contains("U+001F"), "{error}");
    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert!(
        held.gaps[0].is_open(),
        "the refused resolution must leave the gap open"
    );

    // The clean question still resolves — the guard does not block real work.
    let resolved = runs
        .resolve_gap(
            &scope,
            &run.run_id,
            "annual_revenue",
            "gross or net?",
            &grounded("net", &["message:owner-reply-9"]),
            "owner",
            later(4),
        )
        .expect("clean resolve");
    assert_eq!(
        resolved.gaps[0]
            .resolution
            .as_ref()
            .expect("resolution")
            .answer
            .text,
        "net"
    );
    assert_eq!(
        resolved
            .field("annual_revenue")
            .expect("field")
            .answer
            .as_ref()
            .expect("answer")
            .text,
        "net"
    );
}

/// derive_expectation_id joins run_id, description and source_hint with
/// U+001F, but raise_expectation accepted components carrying it — so the
/// waits ("verification code", "inbox\u{1f}sms") and
/// ("verification code\u{1f}inbox", "sms") hashed to the SAME expectation id,
/// fusing two different waits into one so a single arriving event closed
/// both.
#[test]
fn an_expectation_component_carrying_the_separator_is_refused() {
    let (_tmp, runs, scope) = fixture();
    let run = open_run(&runs, &scope);

    let error = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "verification code",
            "inbox\u{1f}sms",
            later(1),
        )
        .expect_err("a source hint carrying the separator must be refused");
    assert!(error.to_string().contains("U+001F"), "{error}");
    let error = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "verification code\u{1f}inbox",
            "sms",
            later(2),
        )
        .expect_err("a description carrying the separator must be refused");
    assert!(error.to_string().contains("U+001F"), "{error}");

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert_eq!(
        held.expectations.len(),
        0,
        "a refused wait must leave nothing behind"
    );

    // A clean wait still lands.
    let wait = runs
        .raise_expectation(
            &scope,
            &run.run_id,
            "verification code",
            "inbox:company-assistant",
            later(3),
        )
        .expect("clean raise");
    assert!(wait.is_open());
    assert_eq!(wait.description, "verification code");
    assert_eq!(wait.source_hint, "inbox:company-assistant");
    assert_eq!(wait.raised_at, later(3));
}

// ── The submission IS an outward act ────────────────────────────────────────

use super::store::FormSubmissionAct;
use magician::magician_v2::evidence::{
    OutwardActStatus, OutwardAssertionStore, OutwardChannel, OutwardScope,
};

/// The outward-assertions register the run accounts to, rooted in the same
/// workspace as the run itself — which is where the store derives the outward
/// scope from, so a test reads back through the same path production does.
fn register(tmp: &tempfile::TempDir) -> OutwardAssertionStore {
    OutwardAssertionStore::new(ArtifactV2Workspace::new(tmp.path()))
}

/// The scope the store derives for the register, spelled out so reads in the
/// tests cannot silently diverge from the writes the store makes.
fn outward_scope(scope: &RunScope) -> OutwardScope {
    OutwardScope::new(scope.principal.clone(), scope.workspace.clone())
}

fn recipients() -> Vec<String> {
    vec!["grants@agency.test".to_string()]
}

const PAYLOAD: &str = "artifact://sbir-application-r3";

fn submission_act<'a>(
    assertions: &'a OutwardAssertionStore,
    recipients: &'a [String],
) -> FormSubmissionAct<'a> {
    FormSubmissionAct {
        assertions,
        exact_payload_artifact_ref: PAYLOAD,
        recipients,
    }
}

/// A ready run bound to a named relationship of any kind — the same form,
/// opened for a different counterparty so its id does not collide.
fn ready_run_bound_to(
    runs: &RunStateStore,
    scope: &RunScope,
    purpose: &str,
    audience: AudienceRef,
) -> Run {
    let run = runs
        .open(
            scope,
            purpose,
            "portal:grants/form-7",
            Some(audience),
            &form_fields(),
            "company-assistant",
            now(),
        )
        .expect("open");
    runs.record_answer(
        scope,
        &run.run_id,
        "legal_name",
        &grounded("Acme Ltd", &["doc:certificate-of-incorporation"]),
        later(1),
    )
    .expect("legal_name");
    runs.record_answer(
        scope,
        &run.run_id,
        "annual_revenue",
        &grounded("2.4M", &["doc:fy25-accounts"]),
        later(2),
    )
    .expect("annual_revenue")
}

/// The failure this pins: `record_submission` demanded the covering act's ref
/// and nothing in the codebase recorded a `Form`-channel act, so the only way
/// to pass the submit gate was to hand it a ref pointing at nothing. `submit`
/// records the act itself, and the ref the run cites now resolves to a real
/// disclosure — one that says WHAT went out, to WHOM, and that it is in
/// flight rather than merely drafted.
#[test]
fn a_submission_records_a_form_act_whose_ref_the_run_then_cites() {
    let (_tmp, runs, scope) = fixture();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    let run = ready_run(&runs, &scope);
    let recipients = recipients();

    let sent = runs
        .submit(
            &scope,
            &run.run_id,
            "owner",
            &submission_act(&assertions, &recipients),
            later(5),
        )
        .expect("submit");

    assert_eq!(sent.state(), RunState::Submitted);
    let submission = sent.submission.as_ref().expect("submission");
    assert_eq!(submission.by, "owner");
    assert_eq!(submission.at, later(5));

    let act = assertions
        .load_act(&outward, &submission.act_ref)
        .expect("load")
        .expect("the ref the run cites must resolve to a real act");
    assert_eq!(act.channel, OutwardChannel::Form);
    assert_eq!(act.consequence_class, "submission_or_publication");
    assert_eq!(
        act.effective_sender, "owner",
        "the person who pressed send is who asserted it"
    );
    assert_eq!(act.intended_audience, recipients);
    assert_eq!(act.exact_payload_artifact_ref, PAYLOAD);
    assert_eq!(act.prepared_at, later(5).to_rfc3339());
    assert_eq!(
        act.status,
        OutwardActStatus::Dispatching,
        "an act resting at Prepared reads as `never left`, which is_active_disclosure \
         excludes — a corrected figure would never find the body that received the form"
    );
    assert_eq!(
        act.dispatched_at.as_deref(),
        Some(later(5).to_rfc3339().as_str())
    );
    assert_eq!(
        act.effect_receipt_ref, None,
        "there is no provider receipt for a form, and inventing one would be fabricated \
         evidence of acceptance"
    );
    assert_eq!(act.settled_at, None);
    assert!(
        !act.observed,
        "a form is a controlled channel: prepared before the act, never observed after"
    );
    // Neither work field, for ANY audience kind — not because this fixture's
    // run happens to be bound to an account. An audience id and a work id are
    // different id spaces, so writing either one here fabricates a linkage the
    // reverse indexes then serve as fact. The relationship is recorded on
    // `audience` instead.
    assert_eq!(act.engagement_id, None);
    assert_eq!(act.program_id, None);
    assert_eq!(
        assertions
            .load_act_history(&outward, &submission.act_ref)
            .expect("history"),
        vec![OutwardActStatus::Prepared, OutwardActStatus::Dispatching],
    );
    assert_eq!(
        assertions
            .index_entries(&outward, "recipient", "grants@agency.test")
            .expect("recipient index"),
        vec![submission.act_ref.clone()],
        "the reverse lookup that makes correction propagation possible"
    );
}

/// Record-before-act, as behaviour: a submission whose act cannot be prepared
/// does not submit. Each refusal is read back off the run — the failure being
/// pinned is a sealed run whose disclosure was never recorded, which an error
/// return alone would not catch — and off the register, which must hold no
/// half-prepared act either.
#[test]
fn a_submission_without_a_preparable_act_does_not_submit() {
    let (_tmp, runs, scope) = fixture();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    let run = ready_run(&runs, &scope);
    let recipients = recipients();

    // Nothing to say WHAT went out.
    assert!(runs
        .submit(
            &scope,
            &run.run_id,
            "owner",
            &FormSubmissionAct {
                assertions: &assertions,
                exact_payload_artifact_ref: "   ",
                recipients: &recipients,
            },
            later(5),
        )
        .is_err());

    // Nobody to say it went TO — the vacuous case: an act with no recipient
    // satisfies every recipient lookup.
    assert!(runs
        .submit(
            &scope,
            &run.run_id,
            "owner",
            &FormSubmissionAct {
                assertions: &assertions,
                exact_payload_artifact_ref: PAYLOAD,
                recipients: &[],
            },
            later(6),
        )
        .is_err());

    // A blank recipient is an unaddressed disclosure wearing a real one's
    // shape, so a non-empty list is not enough.
    assert!(runs
        .submit(
            &scope,
            &run.run_id,
            "owner",
            &FormSubmissionAct {
                assertions: &assertions,
                exact_payload_artifact_ref: PAYLOAD,
                recipients: &["   ".to_string()],
            },
            later(7),
        )
        .is_err());

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    assert_eq!(held.submission, None, "the run did not submit");
    assert_eq!(held.state(), RunState::ReadyForReview);
    assert_eq!(
        assertions
            .index_entries(&outward, "artifact", PAYLOAD)
            .expect("artifact index"),
        Vec::<String>::new(),
        "and no act was left behind claiming a submission that never happened"
    );
}

/// The whole gate runs before the register is touched. A run that is not ready
/// must leave NO prepared act: an act claiming a submission that the gate then
/// refused is as much a lie as a submission with no record, and `unreconciled`
/// would surface it forever as a send nobody can account for.
#[test]
fn a_run_that_is_not_ready_prepares_no_act() {
    let (_tmp, runs, scope) = fixture();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    // Opened, nothing answered.
    let run = open_run(&runs, &scope);
    let recipients = recipients();

    assert!(runs
        .submit(
            &scope,
            &run.run_id,
            "owner",
            &submission_act(&assertions, &recipients),
            later(5),
        )
        .is_err());
    // And an unnamed person cannot fire the gate into the register either.
    assert!(runs
        .submit(
            &scope,
            &run.run_id,
            "   ",
            &submission_act(&assertions, &recipients),
            later(6),
        )
        .is_err());

    assert_eq!(
        runs.load(&scope, &run.run_id)
            .expect("load")
            .expect("run")
            .submission,
        None
    );
    assert_eq!(
        assertions
            .index_entries(&outward, "artifact", PAYLOAD)
            .expect("artifact index"),
        Vec::<String>::new()
    );
    assert_eq!(
        assertions
            .index_entries(&outward, "recipient", "grants@agency.test")
            .expect("recipient index"),
        Vec::<String>::new()
    );
}

/// A submitted run is terminal, and a second `submit` must neither seal it
/// again nor stack a second ladder onto its act. The failure pinned is a
/// re-run appending a second `Dispatching`, which would read as the form
/// having gone out twice.
#[test]
fn a_second_submit_neither_re_seals_the_run_nor_re_dispatches_its_act() {
    let (_tmp, runs, scope) = fixture();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    let run = ready_run(&runs, &scope);
    let recipients = recipients();

    let sent = runs
        .submit(
            &scope,
            &run.run_id,
            "owner",
            &submission_act(&assertions, &recipients),
            later(5),
        )
        .expect("submit");
    let act_ref = sent
        .submission
        .as_ref()
        .expect("submission")
        .act_ref
        .clone();

    assert!(runs
        .submit(
            &scope,
            &run.run_id,
            "someone-else",
            &submission_act(&assertions, &recipients),
            later(9),
        )
        .is_err());

    let held = runs.load(&scope, &run.run_id).expect("load").expect("run");
    let submission = held.submission.as_ref().expect("submission");
    assert_eq!(submission.by, "owner", "the first submission stands");
    assert_eq!(submission.act_ref, act_ref);
    assert_eq!(submission.at, later(5));
    assert_eq!(
        assertions
            .load_act_history(&outward, &act_ref)
            .expect("history"),
        vec![OutwardActStatus::Prepared, OutwardActStatus::Dispatching],
        "one ladder, never two"
    );
}

/// A submission names the AUDIENCE it served, and neither work field.
///
/// This test used to assert the opposite — that an engagement-kind audience
/// filled `engagement_id` and a program-kind one filled `program_id`, on the
/// reasoning that an id rides on the field matching its kind. The kinds match;
/// the ID SPACES do not. An `AudienceRef` carries no contract about what its id
/// names, and the one thing that resolves an engagement-kind audience to
/// members reads it with `counterparty_store.load` — so it is a counterparty
/// id, while `engagement_id` is what every reverse lookup under
/// `WorkContextKind::ENGAGEMENT_TOKEN` asks with. The old behaviour filed every
/// submission under a linkage that does not exist, and `reindex_work_axes`
/// counted those acts as correctly attributed.
#[test]
fn a_submission_names_its_audience_and_fabricates_no_work_linkage() {
    let (_tmp, runs, scope) = fixture();
    let assertions = register(&_tmp);
    let outward = outward_scope(&scope);
    let recipients = recipients();

    for (purpose, audience) in [
        (
            "engagement bound application",
            AudienceRef::engagement("eng-7"),
        ),
        (
            "program bound application",
            AudienceRef::program("q3-intake"),
        ),
        ("panel bound application", AudienceRef::panel("audit-2026")),
    ] {
        // Cloned: `ready_run_bound_to` takes the ref by value, and the
        // assertion below compares against it afterwards.
        let run = ready_run_bound_to(&runs, &scope, purpose, audience.clone());
        let sent = runs
            .submit(
                &scope,
                &run.run_id,
                "owner",
                &submission_act(&assertions, &recipients),
                later(5),
            )
            .expect("submit");
        let act = assertions
            .load_act(
                &outward,
                &sent.submission.as_ref().expect("submission").act_ref,
            )
            .expect("load")
            .expect("act");
        assert_eq!(
            act.audience.as_ref(),
            Some(&audience),
            "{purpose}: the act must say which relationship it served"
        );
        assert_eq!(
            (act.engagement_id.as_deref(), act.program_id.as_deref()),
            (None, None),
            "{purpose}: an audience id is not a work id, and asserting one here \
             fabricates a linkage the reverse indexes then serve as fact"
        );
    }
}

/// Every run in the scope comes back, and an unreadable log is not "no runs".
///
/// Pins the reachability failure: run ids are DERIVED from
/// `(scope, purpose, resource_ref)` and nothing indexes them, so before this
/// listing an owner who could not restate that tuple character-for-character
/// could not reach a form holding days of their answers. And a listing that
/// failed open would tell them the form does not exist, after which `open`
/// would fabricate a blank one beside it.
#[test]
fn every_run_in_the_scope_is_listed_and_an_unreadable_log_is_not_empty() {
    let (tmp, runs, scope) = fixture();
    let first = open_run(&runs, &scope);
    let second = runs
        .open(
            &scope,
            "a second undertaking",
            "portal:grants/form-9",
            None,
            &form_fields(),
            "company-assistant",
            later(5),
        )
        .expect("open a second run");
    assert_ne!(first.run_id, second.run_id, "two undertakings, two runs");

    let listed = runs.all_runs(&scope).expect("list");
    assert_eq!(listed.len(), 2);
    // Opened order, so the listing is comparable across calls.
    assert_eq!(listed[0].run_id, first.run_id);
    assert_eq!(listed[1].run_id, second.run_id);
    assert_eq!(listed[1].purpose, "a second undertaking");

    // Another scope's listing is empty — the scope root is the isolation.
    let other = RunScope::new("someone-else", "default");
    assert!(
        runs.all_runs(&other)
            .expect("list an untouched scope")
            .is_empty(),
        "a scope that has never opened a run has no runs"
    );

    // A TERMINATED unparseable line is corruption, not a torn tail.
    let log = run_log_path_for(&tmp, &scope, &first.run_id);
    let mut raw = std::fs::read_to_string(&log).expect("read");
    raw.push_str("{not json}\n");
    std::fs::write(&log, raw).expect("write");
    let error = runs
        .all_runs(&scope)
        .expect_err("an unreadable log must refuse, never fold to an empty listing");
    assert!(
        format!("{error:#}").contains("unparseable record"),
        "{error:#}"
    );
}

/// One run's log, located by folding rather than by rebuilding the hash.
fn run_log_path_for(tmp: &tempfile::TempDir, scope: &RunScope, run_id: &str) -> std::path::PathBuf {
    let dir = ArtifactV2Workspace::new(tmp.path())
        .scope_root(&scope.principal, &scope.workspace)
        .join("run_state");
    std::fs::read_dir(&dir)
        .expect("run_state dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .find(|path| {
            std::fs::read_to_string(path)
                .expect("read")
                .contains(run_id)
        })
        .expect("the run's own log")
}
