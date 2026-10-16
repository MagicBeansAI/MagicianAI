//! Journey and adaptation lanes, provider-free (plan §13.5; P8.3, P8.4).
//!
//! Each journey starts from the owner's own words entering memory through the
//! real chat save path (`merge_user_preference_with_event`, with and without
//! the host-scoped owner turn), and ends at what admission lets happen: the
//! real inventory, the real strict parse and host validator, the admission
//! ticket, the freshness fence, the backstop and the content-free log. Only
//! the model is scripted, and the scripts are deliberately adversarial where
//! that matters: the judge asks to hold on inferred evidence, and the host
//! must refuse to let it.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

use super::admission::{alignment_log_path, verify_fence, FenceVerdict};
use super::backstop::{
    backstop_with, note_loop_admission, BackstopEnvironment, BackstopScope, BackstopVerdict,
};
use super::collect::{action_binding, CheckRequest};
use super::contract::{
    AlignmentDecision, AlignmentInput, Assessment, Continuation, Intervention, OwnerSpan,
    ResolutionChoice, ALIGNMENT_DECISION_SCHEMA_VERSION,
};
use super::intention::AssertionStatus;
use super::judge::ScriptedJudge;
use super::owner_turn::{with_owner_turn, OwnerTurn};
use super::receipts::{receipt_token, token_purpose, AlignmentReceiptPurpose};
use super::service::{check_action, AlignmentAdmission, AlignmentEnvironment};
use crate::config::PreferenceAlignmentConfig;
use crate::magician_v2::agents::AgentMemoryResolver;
use crate::magician_v2::chat::service::merge_user_preference_with_event;
use crate::magician_v2::execution::actions::ExecutableAction;
use crate::magician_v2::execution::capability::ImplementationType;

const PRINCIPAL: &str = "journey-owner";
const WORKSPACE: &str = "default";
const STATEMENT: &str = "Check with me before ordering meat for me.";

/// A judge that wants to hold on anything it is shown, and counts its calls.
/// The host decides what the hold may become.
struct HoldOnEverything {
    calls: AtomicUsize,
}

impl HoldOnEverything {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ScriptedJudge for HoldOnEverything {
    fn respond(&self, input: &AlignmentInput) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let decision = AlignmentDecision {
            schema_version: ALIGNMENT_DECISION_SCHEMA_VERSION.to_string(),
            assessment: Assessment::Conflict,
            intervention: Intervention::Confirm,
            continuation: Continuation::Wait,
            supporting_intentions: input.intentions.iter().map(|i| i.id.clone()).collect(),
            holding_intentions: input.intentions.iter().map(|i| i.id.clone()).collect(),
            cited_owner_spans: Vec::new(),
            explanation: "This order includes meat, and you asked to be checked first."
                .to_string(),
            unresolved_facts: Vec::new(),
            resolution_choices: vec![ResolutionChoice::AllowOnce, ResolutionChoice::Cancel],
            remember_offer: None,
        };
        serde_json::to_string(&decision).map_err(|error| error.to_string())
    }
}

/// A judge that hears the owner cancel in the current turn.
struct OwnerCancels;

impl ScriptedJudge for OwnerCancels {
    fn respond(&self, input: &AlignmentInput) -> Result<String, String> {
        let decision = AlignmentDecision {
            schema_version: ALIGNMENT_DECISION_SCHEMA_VERSION.to_string(),
            assessment: Assessment::Conflict,
            intervention: Intervention::None,
            continuation: Continuation::Cancel,
            supporting_intentions: Vec::new(),
            holding_intentions: Vec::new(),
            cited_owner_spans: input
                .request
                .owner_spans
                .iter()
                .map(|span| span.id.clone())
                .collect(),
            explanation: "You said to call it off.".to_string(),
            unresolved_facts: Vec::new(),
            resolution_choices: Vec::new(),
            remember_offer: None,
        };
        serde_json::to_string(&decision).map_err(|error| error.to_string())
    }
}

fn config(mode: &str) -> PreferenceAlignmentConfig {
    PreferenceAlignmentConfig {
        mode: mode.to_string(),
        ..PreferenceAlignmentConfig::default()
    }
}

/// A food order: a known commitment (`swiggy-mcp` `place_food_order`).
fn order(item: &str) -> ExecutableAction {
    ExecutableAction::Pack {
        capability_name: "swiggy-mcp".to_string(),
        implementation: ImplementationType::Compiled {
            provider_name: "swiggy-mcp".to_string(),
        },
        resolved_params: std::collections::HashMap::from([
            ("action".to_string(), json!("place_food_order")),
            ("items".to_string(), json!([item])),
        ]),
    }
}

fn request(text: &str) -> CheckRequest {
    CheckRequest {
        principal: PRINCIPAL.to_string(),
        workspace: WORKSPACE.to_string(),
        execution_id: Some("journey-exec".to_string()),
        request_text: text.to_string(),
        request_revision: 1,
        owner_spans: vec![OwnerSpan {
            id: "s1".to_string(),
            text: text.to_string(),
        }],
        pending_gates: Vec::new(),
    }
}

/// Save a preference through the chat save path. `owner_said` scopes the
/// owner's own turn around the call, as the chat dispatcher does on the
/// owner's direct surface.
async fn save(resolver: &AgentMemoryResolver, key: &str, value: Value, owner_said: bool) {
    let turn = owner_said
        .then(|| OwnerTurn::new("turn-1", "I'm vegetarian now. Check with me before ordering meat for me."))
        .flatten();
    let result = with_owner_turn(
        turn,
        merge_user_preference_with_event(
            resolver,
            PRINCIPAL,
            WORKSPACE,
            "preferences",
            key,
            value,
            None,
        ),
    )
    .await;
    assert_eq!(result["status"], "ok", "{result}");
}

fn environment<'a>(
    config: &'a PreferenceAlignmentConfig,
    resolver: &'a AgentMemoryResolver,
    judge: &'a dyn ScriptedJudge,
) -> AlignmentEnvironment<'a> {
    AlignmentEnvironment {
        config,
        router: None,
        scripted: Some(judge),
        resolver,
        taste_loader: None,
        taste_capture: None,
    }
}

fn is_checked(action: &ExecutableAction) -> bool {
    super::classify::classify_action(action).is_checked()
}

/// Adaptation lane: what the owner says in chat governs the next act.
#[tokio::test]
async fn an_explicit_preference_saved_in_chat_holds_the_next_act() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "meat_check", json!(STATEMENT), true).await;

    let action = order("chicken biryani");
    assert!(is_checked(&action), "a food order is an outward commitment");
    let judge = HoldOnEverything::new();
    let enforce = config("enforce");
    let check = check_action(
        &environment(&enforce, &resolver, &judge),
        &request("Order chicken biryani for dinner."),
        &action,
        &[],
    )
    .await;
    let AlignmentAdmission::Hold { .. } = check.admission else {
        panic!("an explicit check-first instruction must hold: {:?}", check.admission);
    };
    assert!(check.ticket(&request("x")).is_none(), "a held act has no ticket");

    // The owner's answer is bound to this exact act, under a purpose no other
    // gate's approval can satisfy.
    let canonical =
        crate::magician_v2::execution::agentic::stable_confirmation_action_json(&action).unwrap();
    let receipt = receipt_token(AlignmentReceiptPurpose::Exception, &canonical).unwrap();
    assert_eq!(
        token_purpose(&receipt),
        Some(AlignmentReceiptPurpose::Exception)
    );
    assert_ne!(receipt, canonical);
}

/// Adaptation lane: an inferred preference is used, never over-enforced.
#[tokio::test]
async fn an_inferred_preference_only_nudges_even_when_the_judge_asks_to_hold() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    // The same words, but no owner turn proves the owner said them.
    save(&resolver, "meat_check", json!(STATEMENT), false).await;

    let judge = HoldOnEverything::new();
    let enforce = config("enforce");
    let check = check_action(
        &environment(&enforce, &resolver, &judge),
        &request("Order chicken biryani for dinner."),
        &order("chicken biryani"),
        &[],
    )
    .await;
    assert_eq!(judge.calls(), 1);
    match &check.admission {
        AlignmentAdmission::ProceedWithNudge { outcome } => {
            let super::judge::JudgeOutcome::Decided { decision, .. } = outcome else {
                panic!("decided");
            };
            assert!(decision.downgrade.is_some(), "the host capped the judge's hold");
        },
        other => panic!("inferred evidence must never hold: {other:?}"),
    }
    assert!(check.ticket(&request("x")).is_some());
}

/// Supersession: a cleared preference stops governing.
#[tokio::test]
async fn a_cleared_preference_no_longer_governs() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "meat_check", json!(STATEMENT), true).await;
    save(&resolver, "meat_check", json!(""), true).await;

    let judge = HoldOnEverything::new();
    let enforce = config("enforce");
    let check = check_action(
        &environment(&enforce, &resolver, &judge),
        &request("Order chicken biryani for dinner."),
        &order("chicken biryani"),
        &[],
    )
    .await;
    assert_eq!(judge.calls(), 0, "nothing left to compare: no model call");
    assert_eq!(check.admission, AlignmentAdmission::Proceed);
}

/// The owner's current turn can cancel the work.
#[tokio::test]
async fn the_owner_cancelling_in_the_current_turn_cancels_the_act() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "meat_check", json!(STATEMENT), true).await;
    let enforce = config("enforce");
    let check = check_action(
        &environment(&enforce, &resolver, &OwnerCancels),
        &request("Actually, forget the order."),
        &order("chicken biryani"),
        &[],
    )
    .await;
    assert!(matches!(check.admission, AlignmentAdmission::Cancel { .. }));
}

/// A judge that records the statements it was shown, and asks to hold.
struct RecordsStatements {
    seen: std::sync::Mutex<Vec<String>>,
}

impl ScriptedJudge for RecordsStatements {
    fn respond(&self, input: &AlignmentInput) -> Result<String, String> {
        self.seen
            .lock()
            .unwrap()
            .extend(input.intentions.iter().map(|i| i.statement.clone()));
        HoldOnEverything::new().respond(input)
    }
}

/// Save under `key` with the owner's own `words` scoped around the call.
async fn save_said(resolver: &AgentMemoryResolver, key: &str, value: &str, words: &str) -> Value {
    with_owner_turn(
        OwnerTurn::new("turn-2", words),
        merge_user_preference_with_event(
            resolver,
            PRINCIPAL,
            WORKSPACE,
            "preferences",
            key,
            json!(value),
            None,
        ),
    )
    .await
}

const CORRECTION: &str = "No need to check with me before ordering meat any more.";

/// Adaptation lane: the owner corrects a preference in chat and the model
/// saves the correction under the same key. The earlier claim is superseded,
/// so only the correction reaches the judge. Left live beside it, both
/// governed the next act and alignment held it (2026-10-12 journeys).
#[tokio::test]
async fn a_correction_saved_under_the_same_key_supersedes_the_earlier_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "meat_check", json!(STATEMENT), true).await;
    let saved = save_said(
        &resolver,
        "meat_check",
        CORRECTION,
        "Actually, update that: you don't need to check with me before ordering meat.",
    )
    .await;
    assert_eq!(saved["superseded"], 1, "{saved}");

    let judge = RecordsStatements {
        seen: Default::default(),
    };
    let enforce = config("enforce");
    check_action(
        &environment(&enforce, &resolver, &judge),
        &request("Order chicken biryani for dinner."),
        &order("chicken biryani"),
        &[],
    )
    .await;
    let seen = judge.seen.lock().unwrap().clone();
    assert!(!seen.iter().any(|s| s.contains(STATEMENT)), "{seen:?}");
    assert!(seen.iter().any(|s| s.contains(CORRECTION)), "{seen:?}");
}

/// Only the owner's own turn replaces the owner's instruction: a same-key
/// write with no owner turn, or one whose words do not name the rule, leaves
/// it in force.
#[tokio::test]
async fn a_same_key_write_without_the_owners_word_keeps_the_instruction() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "meat_check", json!(STATEMENT), true).await;

    let unscoped = with_owner_turn(
        None,
        merge_user_preference_with_event(
            &resolver,
            PRINCIPAL,
            WORKSPACE,
            "preferences",
            "meat_check",
            json!(CORRECTION),
            None,
        ),
    )
    .await;
    assert!(unscoped.get("superseded").is_none(), "{unscoped}");

    let unrelated = save_said(&resolver, "meat_check", CORRECTION, "What's the weather like?").await;
    assert!(unrelated.get("superseded").is_none(), "{unrelated}");

    let judge = RecordsStatements {
        seen: Default::default(),
    };
    let enforce = config("enforce");
    check_action(
        &environment(&enforce, &resolver, &judge),
        &request("Order chicken biryani for dinner."),
        &order("chicken biryani"),
        &[],
    )
    .await;
    let seen = judge.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|s| s.contains(STATEMENT)), "{seen:?}");
}

/// The fence: a preference saved while the check was running voids the
/// admission; an unchanged one keeps it.
#[tokio::test]
async fn a_preference_saved_after_the_check_stops_the_admitted_act() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "spice", json!("I like spicy food."), false).await;

    let judge = HoldOnEverything::new();
    let enforce = config("enforce");
    let request = request("Order paneer tikka.");
    let check = check_action(
        &environment(&enforce, &resolver, &judge),
        &request,
        &order("paneer tikka"),
        &[],
    )
    .await;
    let ticket = check.ticket(&request).expect("an admitted act has a ticket");
    assert!(ticket.fenced_generation().is_some());
    assert_eq!(
        verify_fence(&resolver, None, &ticket).await,
        FenceVerdict::Fresh
    );

    // The owner speaks while the act waits to dispatch.
    save(&resolver, "meat_check", json!(STATEMENT), true).await;
    assert!(matches!(
        verify_fence(&resolver, None, &ticket).await,
        FenceVerdict::Stale { .. }
    ));
}

/// Shadow: the full check runs and is recorded, content-free; the act always
/// proceeds.
#[tokio::test]
async fn shadow_records_a_content_free_line_and_never_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "meat_check", json!(STATEMENT), true).await;

    let judge = HoldOnEverything::new();
    let shadow = config("shadow");
    let check = check_action(
        &environment(&shadow, &resolver, &judge),
        &request("Order chicken biryani for dinner."),
        &order("chicken biryani"),
        &[],
    )
    .await;
    assert_eq!(check.admission, AlignmentAdmission::Proceed);
    let ticket = check.ticket(&request("x")).expect("shadow admits");
    assert!(ticket.fenced_generation().is_none(), "shadow is never fenced");

    let memory = resolver.resolve_for_scope(PRINCIPAL, WORKSPACE).unwrap();
    let log = std::fs::read_to_string(alignment_log_path(memory.storage())).unwrap();
    let line: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(line["kind"], "check");
    assert_eq!(line["disposition"], "hold");
    assert_eq!(line["admission"], "proceed");
    let judged = line["judged"].as_array().expect("the judged intentions");
    assert_eq!(judged.len(), 1);
    assert!(judged[0]["id"].is_string() && judged[0]["revision"].is_string());
    assert!(line["policy_version"].is_string());
    assert!(!log.contains("meat"), "the log carries no owner content");
    assert!(!log.contains("biryani"), "the log carries no arguments");
}

/// The backstop: an act the loop admitted is consumed once without a second
/// model call; an act nobody admitted is judged and, held, refused.
#[tokio::test]
async fn the_backstop_consumes_loop_admissions_and_judges_everything_else() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "meat_check", json!(STATEMENT), true).await;

    let judge = HoldOnEverything::new();
    let enforce = config("enforce");
    let environment = BackstopEnvironment {
        router: None,
        scripted: Some(&judge),
        resolver: &resolver,
        taste_loader: None,
    };
    let scope = BackstopScope {
        principal: Some(PRINCIPAL),
        workspace: Some(WORKSPACE),
        execution_id: Some("journey-backstop"),
        request_text: "Order chicken biryani for dinner.",
        owner_turn: None,
    };
    let action = order("chicken biryani");

    // A plane call nobody admitted: judged and held, with the typed seed a
    // pausing caller turns into the owner's review.
    let held = backstop_with(&enforce, &environment, &scope, &action).await;
    let BackstopVerdict::Hold { seed, message } = held else {
        panic!("the act is held for the owner: {held:?}");
    };
    assert!(message.starts_with("NOT executed"));
    assert_eq!(seed.binding_id, action_binding(&action).unwrap());
    assert!(!seed.intention_ids.is_empty());
    assert_eq!(judge.calls(), 1);

    // The loop admitted this exact act: consumed, no second judge call.
    note_loop_admission(
        PRINCIPAL,
        WORKSPACE,
        Some("journey-backstop"),
        &action_binding(&action).unwrap(),
    );
    assert_eq!(
        backstop_with(&enforce, &environment, &scope, &action).await,
        BackstopVerdict::Continue
    );
    assert_eq!(judge.calls(), 1);

    // Consumed once: the same act again is judged again.
    assert_ne!(
        backstop_with(&enforce, &environment, &scope, &action).await,
        BackstopVerdict::Continue
    );
    assert_eq!(judge.calls(), 2);

    // An argument widened after admission is a different act.
    note_loop_admission(
        PRINCIPAL,
        WORKSPACE,
        Some("journey-backstop"),
        &action_binding(&action).unwrap(),
    );
    let widened = order("two chicken biryanis");
    assert_ne!(
        backstop_with(&enforce, &environment, &scope, &widened).await,
        BackstopVerdict::Continue
    );
    assert_eq!(judge.calls(), 3);
}

/// Read-only and private acts are steered, never checked: no judge call, no
/// interruption.
#[tokio::test]
async fn private_acts_are_never_judged() {
    let tmp = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(tmp.path());
    save(&resolver, "meat_check", json!(STATEMENT), true).await;
    let judge = HoldOnEverything::new();
    let enforce = config("enforce");
    let read = ExecutableAction::Http(crate::magician_v2::execution::actions::HttpAction::get(
        "https://example.invalid/menu",
    ));
    let check = check_action(
        &environment(&enforce, &resolver, &judge),
        &request("What's on the menu?"),
        &read,
        &[],
    )
    .await;
    assert_eq!(check.admission, AlignmentAdmission::NotChecked);
    assert_eq!(judge.calls(), 0);
}

#[test]
fn the_saved_record_reads_as_the_owners_own_words() {
    // Guard for the journeys above: the owner-turn save is what makes the
    // record explicit. Without it these journeys would test nothing.
    let item = json!({
        "source_type": "explicit_user_statement",
        "value": "check with me before ordering",
        "memory_evidence": [OwnerTurn::new("t", "check with me before ordering").unwrap().evidence()],
    });
    assert_eq!(
        super::sources::native::assertion_status(&item),
        AssertionStatus::DirectOwnerStatement
    );
}
