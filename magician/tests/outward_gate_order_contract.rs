//! Where the outward gate's refusals sit inside `execute_action_inner`.
//!
//! These are source-text contracts rather than behavioural tests because the
//! thing being pinned is *placement*: `execute_action_inner` is a forty-thousand
//! line async function whose refusal predicates are unit-tested elsewhere
//! (`agents::outward_gate`, `execution::restricted_action`). What no unit test
//! can see is which side of the capture/live branch each call landed on, and
//! that is precisely what two component docs asserted wrongly and in the UNSAFE
//! direction — see the doc comments on each test.
//!
//! Read from disk rather than `include_str!` so the ~1.5 MB executor is not
//! baked into the test binary.

use std::fs;

const EXECUTOR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/magician_v2/execution/agentic/executor.rs"
);

/// The body of `execute_action_inner`, from its signature to end of file.
///
/// Everything asserted below is unique in the whole file (each marker occurs
/// exactly once), so a slice from the signature onward cannot pick up a
/// same-named call in a different function.
fn executor_from_the_dispatch_fn() -> String {
    let source = fs::read_to_string(EXECUTOR).expect("the executor source must be readable");
    let start = source.find("async fn execute_action_inner(").expect(
        "`execute_action_inner` must exist: it is the one function every action passes through",
    );
    source[start..].to_string()
}

/// Position of a marker that must occur exactly once, or a panic naming it.
fn only_at(haystack: &str, marker: &str) -> usize {
    let count = haystack.matches(marker).count();
    assert_eq!(
        count, 1,
        "`{marker}` must appear exactly once after `execute_action_inner`; found {count}. \
         If the call moved or was duplicated, this contract needs rewriting rather than relaxing."
    );
    haystack.find(marker).expect("counted one occurrence")
}

/// Pins: the restricted-action refusal is NOT confined to the live branch.
///
/// `docs/components/magician/restricted-actions.md` guaranteed enforcement was
/// *"in the **live** branch of the outward gate only"* and that *"under capture —
/// today's default — behaviour is byte-for-byte unchanged."* Both were false and
/// false in the unsafe direction: a reviewer reading them would have concluded
/// that a passthrough send is still rehearsed under the default posture, when it
/// is refused.
///
/// The failure this pins is a future edit that moves the refusal below the
/// capture branch — restoring the doc's old claim silently, and with it the
/// rehearsal of acts that can never be authorised.
#[test]
fn the_restriction_refusal_runs_before_the_capture_branch_not_inside_the_live_one() {
    let body = executor_from_the_dispatch_fn();

    let restriction = only_at(
        &body,
        "outward_bound_dispatch(action, ctx, &capability, &action_token)",
    );
    let capture = only_at(
        &body,
        "matches!(disposition, OutwardDisposition::Capture(_))",
    );

    assert!(
        restriction < capture,
        "the restriction refusal is at byte {restriction} and the capture branch at {capture}: \
         a refusal placed after the capture test only fires on live sends, which is the \
         behaviour `restricted-actions.md` used to claim and no longer does"
    );
}

/// Pins: the work-context ceiling is consulted at dispatch, as gate 0.
///
/// `docs/components/magician/work-context.md` listed *"Wiring into dispatch.
/// Nothing consults this at tool-resolution time"* under **Not built here**, so
/// a reviewer would have concluded the whole module was inert. It is gate 0:
/// `execute_action_inner` calls `outward_gate::work_context_refusal`, which calls
/// `work_context::resolve`.
///
/// The order asserted is the documented one — work context, then restriction,
/// then suppression, then capture — and each pair is asserted on its own so a
/// failure names which link broke. The work-context call must come first
/// because it turns on nothing about the act's arguments.
#[test]
fn the_work_context_ceiling_is_gate_zero_ahead_of_restriction_suppression_and_capture() {
    let body = executor_from_the_dispatch_fn();

    let work_context = only_at(&body, "outward_gate::work_context_refusal(");
    let restriction = only_at(
        &body,
        "outward_bound_dispatch(action, ctx, &capability, &action_token)",
    );
    let suppression = only_at(&body, "outward_gate::contact_refusal(");
    let compliance = only_at(&body, "recipient_compliance::compliance_refusal(");
    let capture = only_at(
        &body,
        "matches!(disposition, OutwardDisposition::Capture(_))",
    );

    assert!(
        work_context < restriction,
        "work context ({work_context}) must precede the restriction refusal ({restriction})"
    );
    assert!(
        restriction < suppression,
        "the restriction refusal ({restriction}) must precede the suppression screen \
         ({suppression}): a decision taken on an act whose arguments can still widen is a \
         decision about a description of the act"
    );
    assert!(
        suppression < compliance,
        "the suppression screen ({suppression}) must precede recipient compliance \
         ({compliance}): suppression is the absolute bar — a person who opted out is not \
         contactable by any work — while compliance answers the narrower question of whether \
         THIS send is a duplicate, a follow-up or held. Asking the narrow question first would \
         let a suppressed recipient be refused for the wrong reason, and the refusal an \
         operator reads would name the wrong remedy"
    );
    assert!(
        compliance < capture,
        "recipient compliance ({compliance}) must precede the capture branch ({capture}): \
         capturing a message to somebody who has asked to be forgotten rehearses a forbidden \
         act, exactly as it does for somebody suppressed. A gate that only runs on the live \
         side is a gate capture mode can be used to walk around"
    );
}

/// Pins: the suppression screen reads the resolved recipient list, not `to`.
///
/// `docs/components/magician/jsonl-durability.md` said the suppression register
/// was *"not wired into dispatch yet"*. It is the dispatch gate — and the value
/// it screens is the one that matters: `effective.recipients`, the same list the
/// disclosure record and the envelope shadow use. Screening the typed `to` field
/// would clear an act that reaches five people after seeing one, and the four
/// unseen are exactly the ones a passthrough added.
#[test]
fn the_suppression_screen_reads_the_effective_recipients_not_the_typed_field() {
    let body = executor_from_the_dispatch_fn();

    let screened = only_at(&body, "let screened_recipients = effective");
    let call = only_at(&body, "outward_gate::contact_refusal(");
    assert!(
        screened < call,
        "the screened list ({screened}) must be built from `effective` before the screen ({call})"
    );
    assert!(
        body.contains(".map(|effective| effective.recipients.clone())"),
        "the screened list must come from `effective.recipients`; if it is ever taken from a \
         typed `to` parameter instead, the screen sees one address for an act that reaches five"
    );
}
