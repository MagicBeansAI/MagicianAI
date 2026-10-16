//! Exercise the shipped recipe against realistic projected store records.
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::contextual_round_declaration::{AppRoundProgramDeclaration, AppRoundSemanticOutcome};
use crate::contextual_round_program::AppRoundValueEnvironment;

fn program() -> AppRoundProgramDeclaration {
    let bundle: Value = serde_json::from_str(include_str!(
        "../../../magician_data_v3/system/town_square/app/recipes/take-ambient-turn.json"
    ))
    .unwrap();
    let program: AppRoundProgramDeclaration =
        serde_json::from_value(bundle["recipe"]["nodes"]["root"]["node"]["program"].clone())
            .unwrap();
    program.validate().unwrap();
    assert_eq!(bundle["recipe"]["nodes"]["root"]["resources"]["max_parallelism"], json!(program.limits.max_concurrent));
    program
}

#[test]
fn conversation_mode_refreshes_between_speakers_and_rejects_parallel_snapshots() {
    use crate::contextual_round_declaration::AppRoundContextMode;
    let mut program = program();
    assert_eq!(program.context_mode, AppRoundContextMode::Progressive);
    assert_eq!(program.limits.max_concurrent, 1);
    assert!(program.limits.max_participants > 1);
    let progressive_slots = program.retained_query_slots(32, 8).unwrap();
    program.limits.max_concurrent = 4;
    assert!(program.validate().is_err());
    program.context_mode = AppRoundContextMode::Snapshot;
    program.validate().unwrap();
    let shared = program
        .queries
        .iter()
        .filter(|q| !q.per_participant)
        .count();
    let refresh = program
        .queries
        .iter()
        .filter(|q| q.per_participant || q.refresh_before_dispatch)
        .count();
    assert_eq!(progressive_slots, 32 * refresh + shared * 2 + 8);
    // Old declarations preserve their exact canonical representation.
    let old = serde_json::to_value(&program).unwrap();
    assert!(old.get("context_mode").is_none());
    let restored: AppRoundProgramDeclaration = serde_json::from_value(old).unwrap();
    assert_eq!(restored.context_mode, AppRoundContextMode::Snapshot);
}

#[test]
fn round_retention_reserves_every_agents_context_and_final_reads() {
    let program = program();
    let source_rows = 32;
    let slots = program.retained_query_slots(source_rows, 8).unwrap();
    let shared = program
        .queries
        .iter()
        .filter(|query| !query.per_participant)
        .count();
    let individual = program.queries.len() - shared;
    // A full round must retain preparation and commit context for every agent,
    // then still have room for the shared final reads and source pages.
    assert!(slots >= source_rows * individual + shared * 2 + 8);
    assert!(slots > 24);
    assert!(program.retained_query_slots(usize::MAX, 8).is_err());
}

fn row(id: &str, fields: Value) -> Value {
    json!({"record_id":id,"record_revision":7,"fields":fields})
}

fn context() -> Value {
    json!({
        "policy":[row("singleton", json!({"autonomy_state":"on","cooldown_seconds":1800,"max_post_chars":300,"max_autonomous_replies":3}))],
        "cursor":[row("singleton", json!({"last_member_id":null,"turns_taken":40}))],
        "member":[row("a", json!({"member_id":"a","enrolled":true,"opted_out":false}))],
        "members":[row("a",json!({"member_id":"a","display_name":"Alice"})),row("b",json!({"member_id":"b","display_name":"Bob"})),row("c",json!({"member_id":"c","display_name":"Carol"}))],
        "feed":[row("prior",json!({"post_id":"prior","author_id":"b","body":"A real earlier question"}))],
        "own_recent":[],
        "mentions":[
            row("info",json!({"delivery_kind":"reply_notification","post_id":"unrelated"})),
            row("addressed",json!({"delivery_kind":"explicit_mention","post_id":"prior"})),
            row("unanswered",json!({"delivery_kind":"explicit_mention","post_id":"different"}))
        ],
        "mood":[row("mood-a",json!({"valence":-0.5,"energy":0.2,"baseline_valence":0.0,"baseline_energy":0.5}))],
        "round":{"next_cursor":"b","summary":{"selected":2}}
    })
}

fn environment<'a>(
    context: &'a Value,
    model: Option<&'a Value>,
    participant: &'a Value,
    author: &'a str,
) -> AppRoundValueEnvironment<'a> {
    AppRoundValueEnvironment {
        input: &Value::Null,
        participant,
        context,
        model,
        item: None,
        now: DateTime::parse_from_rfc3339("2026-09-09T16:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        run_id: "exec-round",
        participant_id: author,
    }
}

fn draft() -> Value {
    json!({"outcome":"draft","reason":"relevant_contribution","body":"Bob, Carol: this answers the earlier question.","post_type":"reply","target_post_id":"prior","mentioned_member_ids":["b","c","invented"]})
}

#[test]
fn shipped_round_excludes_policy_optouts_and_cooldowns_without_a_model() {
    let program = program();
    let mut participant =
        json!({"enabled":true,"declares_social_persona":true,"opted_out":false,"busy":false});
    let mut context = context();
    let excluded = |context: &Value, participant: &Value| {
        let values = program
            .values
            .evaluate(&environment(context, None, participant, "a"))
            .unwrap();
        program.exclusion(&values)
    };
    assert_eq!(excluded(&context, &participant), None);
    context["policy"] = json!([]);
    assert_eq!(
        excluded(&context, &participant).as_deref(),
        Some("autonomy_off")
    );
    context = self::context();
    participant["opted_out"] = json!(true);
    assert_eq!(
        excluded(&context, &participant).as_deref(),
        Some("agent_opted_out")
    );
    participant["opted_out"] = json!(false);
    // Idle agents only: a busy roster row is excluded, a row whose bit the
    // runtime could not determine is not — unknown is not busy.
    participant["busy"] = json!(true);
    assert_eq!(
        excluded(&context, &participant).as_deref(),
        Some("agent_busy")
    );
    participant["busy"] = Value::Null;
    assert_eq!(excluded(&context, &participant), None);
    participant["busy"] = json!(false);
    context["own_recent"] = json!([row("recent", json!({"created_at":"2026-09-09T15:59:00Z"}))]);
    assert_eq!(
        excluded(&context, &participant).as_deref(),
        Some("cooldown")
    );
    context["own_recent"] = json!([row("old", json!({"created_at":"2026-09-09T15:00:00Z"}))]);
    assert_eq!(excluded(&context, &participant), None);
}

#[test]
fn shipped_round_has_independent_replayable_author_ids_and_nonempty_posts() {
    let program = program();
    let context = context();
    let model = draft();
    let plan = |author| {
        program
            .plan_mutations(
                false,
                &environment(&context, Some(&model), &Value::Null, author),
            )
            .unwrap()
    };
    let a = plan("a");
    let b = plan("b");
    assert_ne!(a.semantic_record_ids, b.semantic_record_ids);
    assert_eq!(a.operations, plan("a").operations);
    assert_eq!(a.operations[0]["payload"]["author_id"], "a");
    assert_eq!(b.operations[0]["payload"]["author_id"], "b");
    assert!(!a.operations[0]["payload"]["body"]
        .as_str()
        .unwrap()
        .is_empty());
    assert_eq!(a.semantic_record_ids.len(), 1);
    let empty = json!({"outcome":"draft","body":" \n ","reason":"empty"});
    let values = program
        .values
        .evaluate(&environment(&context, Some(&empty), &Value::Null, "a"))
        .unwrap();
    assert!(program.semantic_outcome(&values).is_err());
}

#[test]
fn shipped_reply_delivers_once_and_leaves_unanswered_mentions_pending() {
    let program = program();
    let context = context();
    let model = draft();
    let plan = program
        .plan_mutations(
            false,
            &environment(&context, Some(&model), &Value::Null, "a"),
        )
        .unwrap();
    let deliveries: Vec<_> = plan
        .operations
        .iter()
        .filter(|op| op["entity"] == "mention" && op["kind"] == "create")
        .collect();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0]["payload"]["mentioned_member_id"], "b");
    assert_eq!(
        deliveries[0]["payload"]["delivery_kind"],
        "reply_notification"
    );
    assert_eq!(deliveries[1]["payload"]["mentioned_member_id"], "c");
    assert_eq!(
        deliveries[1]["payload"]["delivery_kind"],
        "explicit_mention"
    );
    assert!(plan
        .operations
        .iter()
        .any(|op| op["record_id"] == "info" && op["patch"]["status"] == "dropped"));
    assert!(plan
        .operations
        .iter()
        .any(|op| op["record_id"] == "addressed" && op["patch"]["status"] == "handled"));
    assert!(!plan
        .operations
        .iter()
        .any(|op| op["record_id"] == "unanswered"));
}

#[test]
fn shipped_quiet_keeps_explicit_mentions_and_mood_baselines_without_post_evidence() {
    let program = program();
    let context = context();
    let model = json!({"outcome":"quiet","reason":"nothing_to_add","body":"","post_type":"thought","target_post_id":null,"mentioned_member_ids":[]});
    let env = environment(&context, Some(&model), &Value::Null, "a");
    let values = program.values.evaluate(&env).unwrap();
    assert_eq!(
        program.semantic_outcome(&values).unwrap(),
        AppRoundSemanticOutcome::Quiet("nothing_to_add".into())
    );
    let plan = program.plan_mutations(false, &env).unwrap();
    assert_eq!(plan.operations.len(), 2);
    assert!(plan.semantic_record_ids.is_empty());
    assert_eq!(plan.operations[0]["record_id"], "info");
    let mood = &plan.operations[1]["patch"];
    assert!((mood["valence"].as_f64().unwrap() + 0.45).abs() < 1e-10);
    assert!((mood["energy"].as_f64().unwrap() - 0.23).abs() < 1e-10);
    assert!(mood.get("baseline_valence").is_none());
    assert!(mood.get("baseline_energy").is_none());
    assert!(!plan
        .operations
        .iter()
        .any(|op| op["entity"] == "turn_cursor"));
}

#[test]
fn shipped_cursor_advances_once_after_all_participants_and_keeps_revision_fences() {
    let program = program();
    let context = context();
    let plan = program
        .plan_mutations(true, &environment(&context, None, &Value::Null, ""))
        .unwrap();
    assert_eq!(plan.operations.len(), 1);
    assert_eq!(plan.operations[0]["patch"]["last_member_id"], "b");
    assert_eq!(plan.operations[0]["patch"]["turns_taken"], 42);
    assert_eq!(plan.expected_record_revisions[0]["revision"], 7);
    assert!(plan.semantic_record_ids.is_empty());
}

#[test]
fn shipped_draft_cannot_reply_to_a_fabricated_parent_or_escape_unicode_body_bounds() {
    let program = program();
    let mut context = context();
    context["policy"][0]["fields"]["max_post_chars"] = json!(4);
    let mut model = draft();
    model["target_post_id"] = json!("invented");
    model["body"] = json!("  一二三四五  ");
    let plan = program
        .plan_mutations(
            false,
            &environment(&context, Some(&model), &Value::Null, "a"),
        )
        .unwrap();
    assert_eq!(plan.operations[0]["payload"]["post_type"], "thought");
    assert_eq!(plan.operations[0]["payload"]["parent_id"], Value::Null);
    assert_eq!(plan.operations[0]["payload"]["body"], "一二三四");
    assert!(!plan
        .operations
        .iter()
        .any(|op| op["entity"] == "mention" && op["kind"] == "create"));
}
