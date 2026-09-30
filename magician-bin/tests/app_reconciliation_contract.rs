//! Exercise the production reconciliation planner without a model or host I/O.
use std::collections::BTreeMap;

use chrono::Utc;
use magician::magician_v2::apps::models::{
    AppFieldPath, AppMutationOperation, AppName, AppRecordId, AppRecordProjection, AppRevision,
};
use magician::magician_v2::apps::reconciliation::{
    plan_reconciliation, plan_reconciliation_with_sources, AppReconciliation,
};
use serde_json::{json, Value};

#[path = "../../magician/src/magician_v2/apps/model_output_schema.rs"]
mod model_output_schema;

#[path = "../../magician/src/magician_v2/execution/agentic/app_invocation.rs"]
mod app_invocation;

#[path = "../../magician/src/magician_v2/apps/effect_deadline.rs"]
mod effect_deadline;

#[path = "../../scripts/registry_admission_checks/src/lib.rs"]
mod registry_admission_contract;

#[test]
fn effect_admission_retains_the_resource_deadline_through_preflight_and_recovery() {
    use chrono::{DateTime, Duration};
    let root = DateTime::parse_from_rfc3339("2026-09-09T09:03:50.338296Z")
        .unwrap()
        .with_timezone(&Utc);
    // Reproduce the live failure: reservation at 100522ms, dispatch at
    // 109543ms, final I/O edge at 109939ms. A five-second side clock fails.
    let reserved = root + Duration::milliseconds(100522);
    let final_edge = root + Duration::milliseconds(109939);
    let deadline = effect_deadline::absolute_deadline(root, 130522).unwrap();
    assert!(final_edge > reserved + Duration::seconds(5));
    assert!(final_edge < deadline);
    assert_eq!((deadline - final_edge).num_milliseconds(), 20583);
    // Restart reconstructs from the durable origin, never a fresh `now`.
    let recovered_root = DateTime::parse_from_rfc3339(&root.to_rfc3339())
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        effect_deadline::absolute_deadline(recovered_root, 130522),
        Some(deadline)
    );
    assert_eq!(effect_deadline::absolute_deadline(root, u64::MAX), None);
    assert_eq!(
        effect_deadline::absolute_deadline(DateTime::<Utc>::MAX_UTC, 1),
        None
    );
}

#[test]
fn only_one_proven_pre_io_expiry_can_pass_the_identical_failure_retry_guard() {
    use magician::magician_v2::execution::agentic::types::{
        ActionOutcomeCategory, ActionResultRecord,
    };
    let proven = ActionResultRecord::new(
        false,
        None,
        None,
        1,
        Some(ActionOutcomeCategory::AdmissionExpiredBeforeIo),
    );
    let ordinary = ActionResultRecord::new(
        false,
        None,
        Some("app effect admission expired; durable settlement proves no I/O occurred".into()),
        1,
        Some(ActionOutcomeCategory::Failed),
    );
    assert!(ActionResultRecord::permits_admission_retry(&[&proven]));
    assert!(
        !ActionResultRecord::permits_admission_retry(&[&ordinary]),
        "copied error prose is not typed owner evidence"
    );
    assert!(!ActionResultRecord::permits_admission_retry(&[
        &proven, &proven
    ]));
    assert!(!ActionResultRecord::permits_admission_retry(&[
        &ordinary, &proven
    ]));
    let recovered: ActionResultRecord =
        serde_json::from_value(serde_json::to_value(&proven).unwrap()).unwrap();
    assert!(ActionResultRecord::permits_admission_retry(&[&recovered]));
    assert!(!ActionResultRecord::permits_admission_retry(&[
        &proven, &recovered
    ]));
}

#[test]
fn app_batch_calls_have_distinct_reservations_and_recovery_keeps_the_same_identity() {
    use magician::magician_v2::apps::models::AppDigest;
    let session = "agentic-session-exec_ambient";
    let roster = app_invocation::for_effect(session, "llm_1:tool:roster");
    let query = app_invocation::for_effect(session, "llm_1:tool:query");
    let second_query = app_invocation::for_effect(session, "llm_1:tool:query_2");
    let reservation =
        |invocation: &str| AppDigest::blake3(format!("reservation:{invocation}").as_bytes());
    assert_ne!(reservation(&roster), reservation(&query));
    assert_ne!(reservation(&query), reservation(&second_query));
    assert_eq!(
        query,
        app_invocation::for_effect(session, "llm_1:tool:query")
    );
    assert_ne!(
        query,
        app_invocation::for_effect("agentic-session-exec_other", "llm_1:tool:query")
    );
    assert_ne!(
        query,
        app_invocation::for_effect(session, "llm_2:tool:query")
    );
    // Length framing keeps session/effect separators unambiguous.
    assert_ne!(
        app_invocation::for_effect("a:b", "c"),
        app_invocation::for_effect("a", "b:c")
    );
}

#[test]
fn model_transport_preserves_optional_fields_and_validates_the_reviewed_contract() {
    use magician::magician_v2::apps::manifest::AppManifestInputSchema;
    let reviewed: AppManifestInputSchema = serde_json::from_value(json!({
        "type":"object", "fields":{
            "body":{"type":"markdown","required":true},
            "target_post_id":{"type":"text","nullable":true},
            "optional_note":{"type":"text"}
        }
    }))
    .unwrap();
    let schema = reviewed.to_primitive_json_schema();
    let before = schema.clone();
    let transport = model_output_schema::transport_schema(&schema).unwrap();
    assert_eq!(
        transport["required"],
        json!(["body", "optional_note", "target_post_id"])
    );
    assert_eq!(transport["additionalProperties"], false);
    assert_eq!(
        transport["properties"]["target_post_id"]["type"],
        json!(["string", "null"])
    );
    let mut output = json!({"body":"A thought", "target_post_id":null,"optional_note":null});
    model_output_schema::decode_transport(&schema, &mut output).unwrap();
    assert_eq!(output, json!({"body":"A thought","target_post_id":null}));
    reviewed.validate_value(&output).unwrap();
    output["body"] = Value::Null;
    model_output_schema::decode_transport(&schema, &mut output).unwrap();
    assert!(
        reviewed.validate_value(&output).is_err(),
        "required null must not be repaired"
    );
    output["body"] = json!("Valid");
    output["unknown"] = json!(true);
    model_output_schema::decode_transport(&schema, &mut output).unwrap();
    assert!(
        reviewed.validate_value(&output).is_err(),
        "unknown fields must not be stripped"
    );
    assert_eq!(
        schema, before,
        "transport must not mutate reviewed schema identity"
    );
}

#[test]
fn model_transport_handles_nested_records_arrays_and_tagged_unions() {
    let record = json!({"type":"object","properties":{"note":{"type":"string"}},"required":[],"additionalProperties":false});
    let schema = json!({"type":"object","properties":{
        "rows":{"type":"array","items":record},
        "choice":{"oneOf":[
            {"type":"object","properties":{"kind":{"const":"a"},"value":record},"required":["kind","value"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"b"},"value":{"type":"integer"}},"required":["kind","value"],"additionalProperties":false}
        ]}
    },"required":["rows","choice"],"additionalProperties":false});
    let transport = model_output_schema::transport_schema(&schema).unwrap();
    assert!(transport["properties"]["choice"].get("oneOf").is_none());
    assert_eq!(
        transport["properties"]["choice"]["anyOf"][0]["properties"]["kind"],
        json!({"type":"string","enum":["a"]})
    );
    assert_eq!(
        transport["properties"]["rows"]["items"]["required"],
        json!(["note"])
    );
    let mut output =
        json!({"rows":[{"note":null},{"note":"keep"}],"choice":{"kind":"a","value":{"note":null}}});
    model_output_schema::decode_transport(&schema, &mut output).unwrap();
    assert_eq!(
        output,
        json!({"rows":[{},{"note":"keep"}],"choice":{"kind":"a","value":{}}})
    );
    output["choice"]["kind"] = json!("unknown");
    assert!(model_output_schema::decode_transport(&schema, &mut output).is_err());
}

#[test]
fn existing_record_processing_transition_is_explicit_scoped_and_preserves_other_policy() {
    use magician::magician_v2::apps::{
        models::{AppDigest, AppModelProcessing, AppReference},
        records::AppDataHandlingPolicy,
    };
    use magician_apps::apps::migration::{
        apply_migration_to_handling_policy, apply_migration_to_payload, compile_migration_plan,
        AppMigrationOperation,
    };
    let entity = AppName::parse("turn_cursor").unwrap();
    let operation = AppMigrationOperation::EnableRemoteProcessingForExistingRecords {
        entity: entity.clone(),
    };
    let schema = AppDigest::blake3(b"unchanged schema");
    let plan = compile_migration_plan(
        AppReference::parse("plan:owner-processing-transition").unwrap(),
        schema.clone(),
        schema.clone(),
        vec![operation.clone()],
    )
    .unwrap();
    assert!(plan.enables_remote_processing());
    assert!(!plan.record_processing_backup_required(0));
    assert!(plan.record_processing_backup_required(1));
    let before: AppDataHandlingPolicy = serde_json::from_value(json!({
        "classification_floor":"sensitive", "model_processing":"local_only",
        "personal_agent_access":"denied", "memory_promotion":"denied",
        "external_egress":"denied", "approved_destinations":[]
    }))
    .unwrap();
    let mut expected = before.clone();
    expected.model_processing = AppModelProcessing::RemoteAllowed;
    assert_eq!(
        apply_migration_to_handling_policy(&plan, &entity, &before),
        expected
    );
    assert_eq!(
        apply_migration_to_handling_policy(&plan, &AppName::parse("post").unwrap(), &before),
        before
    );
    assert_eq!(
        apply_migration_to_handling_policy(&plan, &entity, &expected),
        expected
    );
    let mut prohibited = before.clone();
    prohibited.model_processing = AppModelProcessing::None;
    assert_eq!(
        apply_migration_to_handling_policy(&plan, &entity, &prohibited),
        prohibited
    );
    let payload = json!({"cursor_key":"ambient_turn", "next_member_index":4});
    assert_eq!(
        apply_migration_to_payload(&plan, &entity, &payload).unwrap(),
        payload
    );
    assert!(compile_migration_plan(
        AppReference::parse("plan:duplicate-processing-transition").unwrap(),
        schema.clone(),
        schema,
        vec![operation.clone(), operation],
    )
    .is_err());
}

#[test]
fn shipped_recipes_compile_with_exact_schema_and_source_call_ceilings() {
    use magician::magician_v2::apps::recipe_ir::{compile_recipe_bundle, AppRecipeBundleSource};
    for bytes in [
        include_str!("../../magician_data_v3/system/town_square/app/recipes/sync-roster.json"),
        include_str!(
            "../../magician_data_v3/system/town_square/app/recipes/take-ambient-turn.json"
        ),
        include_str!("../../magician_data_v3/system/learning/app/recipes/sync-queue.json"),
        include_str!("../../magician_data_v3/system/thinking_map/app/recipes/sync-maps.json"),
        include_str!("../../magician_data_v3/system/learning/app/recipes/approve-candidate.json"),
        include_str!("../../magician_data_v3/system/learning/app/recipes/reject-candidate.json"),
        include_str!("../../magician_data_v3/system/learning/app/recipes/snooze-candidate.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/join.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/listen.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/pause.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/resume.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/stop.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/read-transcript.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/search-meetings.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/sync-sessions.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/sync-takeaways.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/sync-threads.json"),
        include_str!("../../magician_data_v3/system/meetings/app/recipes/sync-upcoming.json"),
        include_str!("../../magician_data_v3/system/claims_review/app/recipes/stage-ingest.json"),
        include_str!("../../magician_data_v3/system/claims_review/app/recipes/sync-claims.json"),
        include_str!("../../magician_data_v3/system/claims_review/app/recipes/sync-context.json"),
        include_str!("../../magician_data_v3/system/claims_review/app/recipes/confirm-claim.json"),
        include_str!(
            "../../magician_data_v3/system/claims_review/app/recipes/confirm-commitment.json"
        ),
        include_str!(
            "../../magician_data_v3/system/claims_review/app/recipes/record-commitment.json"
        ),
        include_str!("../../magician_data_v3/system/claims_review/app/recipes/reject-claim.json"),
        include_str!("../../magician_data_v3/system/town_square/app/recipes/set-policy.json"),
        include_str!("../../magician_data_v3/system/town_square/app/recipes/publish-post.json"),
        include_str!("../../magician_data_v3/system/town_square/app/recipes/create-group.json"),
        include_str!("../../magician_data_v3/system/town_square/app/recipes/react-to-post.json"),
        include_str!("../../magician_data_v3/system/town_square/app/recipes/sync-feed.json"),
    ] {
        let source: AppRecipeBundleSource = serde_json::from_str(bytes).unwrap();
        compile_recipe_bundle(source).expect("shipped schema and recipe compile");
    }
    let mut maps: Value = serde_json::from_str(include_str!(
        "../../magician_data_v3/system/thinking_map/app/recipes/sync-maps.json"
    ))
    .unwrap();
    maps["recipe"]["nodes"]["root"]["resources"]["max_tool_calls"] = json!(1);
    assert!(compile_recipe_bundle(serde_json::from_value(maps.clone()).unwrap()).is_err());
    maps["recipe"]["nodes"]["root"]["resources"]["max_tool_calls"] = json!(2);
    maps["recipe"]["nodes"]["root"]["node"]["declaration"]["sources"]["snapshot"]["action_ref"] =
        json!("action:guessed");
    assert!(compile_recipe_bundle(serde_json::from_value(maps).unwrap()).is_err());
}

#[test]
fn deployed_v1_packages_keep_exact_locks_and_current_physical_action_bindings() {
    use magician::magician_v2::apps::{
        authoring_catalog::{resolve_authoring_primitive_catalog, AuthoringDiscoveryRoots},
        package_lock::authorize_locked_primitive,
        package_transfer::admit_package_archive,
    };
    let catalog = resolve_authoring_primitive_catalog(&AuthoringDiscoveryRoots::default());
    for (name, bytes, expected) in [
        (
            "town_square",
            include_bytes!("fixtures/app-runtime-v1/town_square.magician-app").as_slice(),
            "blake3:b56071bc223b0d353e8b9ec4c094acddec04a5e40394670bb6a8a7979e4fa085",
        ),
        (
            "meetings",
            include_bytes!("fixtures/app-runtime-v1/meetings.magician-app").as_slice(),
            "blake3:6d2274648cd036e424641f6dd3a0332e397aaf509791f58ff33476b72b571820",
        ),
        (
            "thinking_map",
            include_bytes!("fixtures/app-runtime-v1/thinking_map.magician-app").as_slice(),
            "blake3:4e6930cee4600f1d71e0027be7296f0b1381bff186ac63ab0429cee860826d6b",
        ),
        (
            "claims_review",
            include_bytes!("fixtures/app-runtime-v1/claims_review.magician-app").as_slice(),
            "blake3:e48b117ccb5267a1be5ea3ce7392cc3239d8789dd28c5ca3b907225b290628a2",
        ),
        (
            "learning",
            include_bytes!("fixtures/app-runtime-v1/learning.magician-app").as_slice(),
            "blake3:1ee77c02933ae9f025bc3bb158792e7675a43d9c614107cb3e2228ed45b08964",
        ),
    ] {
        let archive =
            admit_package_archive(bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        let lock = archive.package().dependency_lock.claimed_lock();
        assert_eq!(
            lock.lock_digest().as_str(),
            expected,
            "{name}: immutable lock changed"
        );
        let mut physical_bindings = 0;
        for dependency in lock.dependencies() {
            let Some(binding) = dependency.primitive_binding() else {
                continue;
            };
            let descriptor = catalog
                .descriptors()
                .iter()
                .find(|descriptor| descriptor.identity() == binding.primitive_ref())
                .unwrap_or_else(|| {
                    panic!(
                        "{name}: current descriptor missing for {}",
                        dependency.dependency_ref()
                    )
                });
            authorize_locked_primitive(lock, dependency.dependency_ref(), descriptor)
                .unwrap_or_else(|error| {
                    panic!("{name}: current physical binding changed: {error}")
                });
            physical_bindings += 1;
        }
        assert!(
            physical_bindings > 0,
            "fixture must exercise a physical dependency"
        );
    }
}

#[test]
fn shipped_native_round_manifest_admits_one_exact_contextual_semantic_step() {
    use magician::magician_v2::apps::package_staging::admit_package_directory;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../magician_data_v3/system/town_square/app")
        .canonicalize()
        .expect("seed package directory");
    let candidate = admit_package_directory(&root).expect("native round manifest admission");
    let manifest = candidate.manifest().manifest();
    assert_eq!(manifest.version, "0.1.33");
    let behavior = &manifest.app.behaviors[0];
    assert_eq!(behavior.steps.len(), 1);
    assert!(behavior.steps[0].when.is_none());
    behavior.steps[0]
        .output_schema
        .validate_value(&json!({
            "outcome":"quiet", "reason":"nothing_to_add", "body":"",
            "post_type":"thought", "target_post_id":null, "mentioned_member_ids":[],
        }))
        .expect("reviewed explicit quiet shape");
    assert!(
        behavior.steps[0]
            .output_schema
            .validate_value(&json!({
                "outcome":"draft", "reason":"reply", "body":"A thought",
                "post_type":"thought", "target_post_id":null, "mentioned_member_ids":[42],
            }))
            .is_err(),
        "model cannot replace typed member identities"
    );
}

fn declaration() -> AppReconciliation {
    serde_json::from_value(json!({
        "tool":"internal_data", "action":"list_learning_candidates",
        "parameters":{"limit":25}, "input_parameters":{"limit":"limit","state":"state"},
        "rows_field":"candidates", "next_cursor_field":"next_cursor", "truncated_field":"scan_truncated",
        "max_source_rows":25, "max_existing_rows":100,
        "targets":[{
            "entity":"candidate", "key_field":"candidate_id", "source_key":"id",
            "fields":{
                "title":{"kind":"source","field":"title","fallback":null},
                "source_agent_id":{"kind":"source","field":"source_agent_id","fallback":null,"allow_null":true},
                "synced_at":{"kind":"timestamp"}
            },
            "update_fields":["title","source_agent_id","synced_at"], "seeds":[], "retirement":null
        }]
    })).unwrap()
}

#[test]
fn optional_snapshot_is_input_bound_lossless_and_atomic_with_the_page() {
    let mut raw = serde_json::to_value(declaration()).unwrap();
    raw["sources"] = json!({"detail":{
        "tool":"thinking_maps_data","action":"read_map",
        "primitive_ref":format!("primitive:blake3:{}", "a".repeat(64)),
        "action_ref":format!("primitive-action:blake3:{}", "b".repeat(64)),
        "parameters":{},"input_parameters":{"map_id":"map_id"},
        "when_input_present":"map_id","rows":{"kind":"document"}
    }});
    raw["targets"].as_array_mut().unwrap().push(json!({
        "entity":"snapshot","source":"detail","key_field":"map_id","source_key":"map_id",
        "fields":{"snapshot":{"kind":"source_document","max_bytes":16384}},
        "update_fields":["snapshot"],"seeds":[],"retirement":null
    }));
    let declaration: AppReconciliation = serde_json::from_value(raw.clone()).unwrap();
    let detail = &declaration.sources[&AppName::parse("detail").unwrap()];
    assert!(!detail.enabled(&json!({"limit":1})));
    assert!(!detail.enabled(&json!({"map_id":null})));
    let input = json!({"map_id":"map-one"});
    assert_eq!(
        serde_json::to_value(detail.bound_parameters(&input).unwrap()).unwrap(),
        input
    );
    // A present empty ID is never silently substituted or invented. The
    // actual host read's argument proof rejects it before I/O.
    assert_eq!(
        serde_json::to_value(detail.bound_parameters(&json!({"map_id":""})).unwrap()).unwrap(),
        json!({"map_id":""})
    );
    let source = json!({"candidates":[]});
    let existing = BTreeMap::from([
        (AppName::parse("candidate").unwrap(), vec![stored("one")]),
        (AppName::parse("snapshot").unwrap(), vec![]),
    ]);
    let skipped = plan_reconciliation_with_sources(
        &declaration,
        &json!({}),
        &source,
        &BTreeMap::new(),
        &existing,
        Utc::now(),
    )
    .unwrap();
    assert!(skipped.operations.is_empty());
    assert!(plan_reconciliation_with_sources(
        &declaration,
        &input,
        &source,
        &BTreeMap::new(),
        &existing,
        Utc::now()
    )
    .is_err());
    let document = json!({"map_id":"map-one","nodes":[{"body":"x".repeat(5000)}],"edges":[],"extra":{"preserve":true}});
    let sources = BTreeMap::from([(AppName::parse("detail").unwrap(), document.clone())]);
    let plan = plan_reconciliation_with_sources(
        &declaration,
        &input,
        &source,
        &sources,
        &existing,
        Utc::now(),
    )
    .unwrap();
    let AppMutationOperation::Create { payload, .. } = &plan.operations[0] else {
        panic!("snapshot create")
    };
    assert_eq!(
        serde_json::from_str::<Value>(payload["snapshot"].as_str().unwrap()).unwrap(),
        document
    );
    assert!(plan_reconciliation_with_sources(
        &declaration,
        &json!({}),
        &source,
        &sources,
        &existing,
        Utc::now()
    )
    .is_err());
    raw["targets"][1]["fields"]["snapshot"]["max_bytes"] = json!(1024);
    let bounded = serde_json::from_value(raw.clone()).unwrap();
    assert!(plan_reconciliation_with_sources(
        &bounded,
        &input,
        &source,
        &sources,
        &existing,
        Utc::now()
    )
    .is_err());
    raw["targets"][1]["retirement"] =
        json!({"discriminator":"map_id","value":"map-one","patch":{"snapshot":""}});
    assert!(serde_json::from_value::<AppReconciliation>(raw)
        .unwrap()
        .validate()
        .is_err());
}

fn stored(key: &str) -> AppRecordProjection {
    AppRecordProjection {
        entity: AppName::parse("candidate").unwrap(),
        record_id: AppRecordId::parse(format!("existing_{key}")).unwrap(),
        record_revision: AppRevision::new(7).unwrap(),
        fields: BTreeMap::from([
            (AppFieldPath::parse("candidate_id").unwrap(), json!(key)),
            (AppFieldPath::parse("title").unwrap(), json!("Old title")),
            (AppFieldPath::parse("source_agent_id").unwrap(), Value::Null),
            (
                AppFieldPath::parse("owner_note").unwrap(),
                json!("Preserve this"),
            ),
        ]),
    }
}

#[test]
fn only_declared_inputs_override_defaults_and_scope_is_never_forwarded() {
    let declaration = declaration();
    let bound = declaration.bound_parameters(&json!({
        "limit":1,"state":"proposed","principal":"other","workspace":"other","operation":"delete"
    })).unwrap();
    assert_eq!(
        serde_json::to_value(bound).unwrap(),
        json!({"limit":1,"state":"proposed"})
    );
    assert_eq!(
        serde_json::to_value(declaration.bound_parameters(&json!({})).unwrap()).unwrap(),
        json!({"limit":25})
    );
    assert!(declaration
        .bound_parameters(&json!({"limit":{"injected":1}}))
        .is_err());
    for name in [
        "principal",
        "workspace",
        "action",
        "operation",
        "method",
        "__principal",
    ] {
        let mut bad = declaration.clone();
        if let Ok(parameter) = AppName::parse(name) {
            bad.input_parameters
                .insert(parameter, AppName::parse("state").unwrap());
            assert!(bad.validate().is_err(), "unsafe parameter {name}");
        }
    }
}

#[test]
fn bounded_learning_page_preserves_nulls_stable_ids_and_unselected_records() {
    let declaration = declaration();
    let source = json!({"candidates":[
        {"id":"one","title":"Updated title","source_agent_id":null},
        {"id":"two","title":"New title","source_agent_id":"agent-a"}
    ]});
    let existing = BTreeMap::from([(
        AppName::parse("candidate").unwrap(),
        vec![stored("one"), stored("unselected")],
    )]);
    let now = Utc::now();
    let plan = plan_reconciliation(&declaration, &source, &existing, now).unwrap();
    assert_eq!(plan.operations.len(), 2);
    assert_eq!(plan.expected_record_revisions.len(), 1);
    assert_eq!(
        plan.expected_record_revisions[0].revision,
        AppRevision::new(7).unwrap()
    );
    match &plan.operations[0] {
        AppMutationOperation::Update {
            record_id, patch, ..
        } => {
            assert_eq!(record_id.as_str(), "existing_one");
            assert_eq!(patch["source_agent_id"], Value::Null);
            assert!(patch.get("owner_note").is_none());
        },
        _ => panic!("existing natural key must update with its revision"),
    }
    let again = plan_reconciliation(&declaration, &source, &existing, now).unwrap();
    assert_eq!(
        serde_json::to_value(&plan.operations).unwrap(),
        serde_json::to_value(&again.operations).unwrap()
    );
    let empty =
        plan_reconciliation(&declaration, &json!({"candidates":[]}), &existing, now).unwrap();
    assert!(empty.operations.is_empty());
    assert!(empty.expected_record_revisions.is_empty());
}

#[test]
fn retirement_still_requires_complete_source_evidence_and_rows_remain_bounded() {
    let declaration = declaration();
    let existing = BTreeMap::from([(AppName::parse("candidate").unwrap(), vec![stored("one")])]);
    let mut retiring = serde_json::to_value(&declaration).unwrap();
    retiring["targets"][0]["retirement"] =
        json!({"discriminator":"source_agent_id","value":null,"patch":{"title":"Retired"}});
    let retiring: AppReconciliation = serde_json::from_value(retiring).unwrap();
    assert!(
        plan_reconciliation(&retiring, &json!({"candidates":[]}), &existing, Utc::now()).is_err()
    );
    let partial = json!({"candidates":[],"next_cursor":"more","scan_truncated":false});
    assert!(
        plan_reconciliation(&retiring, &partial, &existing, Utc::now())
            .unwrap()
            .operations
            .is_empty()
    );
    let complete = json!({"candidates":[],"next_cursor":null,"scan_truncated":false});
    assert_eq!(
        plan_reconciliation(&retiring, &complete, &existing, Utc::now())
            .unwrap()
            .operations
            .len(),
        1
    );
    let oversized =
        json!({"candidates":vec![json!({"id":"x","title":"X","source_agent_id":null});26]});
    assert!(plan_reconciliation(&declaration, &oversized, &existing, Utc::now()).is_err());
    let missing = json!({"candidates":[{"id":"x","title":"X"}]});
    assert!(plan_reconciliation(&declaration, &missing, &existing, Utc::now()).is_err());
}

// Compile the same small production proof reducer without enabling the
// runtime's fixture-encryption feature or compiling its entire unit-test crate.
use magician::magician_v2::apps::{entity_store, manifest, models, records, workflows};
#[path = "../../magician/src/magician_v2/apps/scheduled_input.rs"]
mod scheduled_input;

#[path = "../../magician/src/magician_v2/apps/query_contract.rs"]
mod query_contract;

#[test]
fn terminal_tool_schema_exposes_mutation_discriminators_and_revision_wire_names() {
    let operations = query_contract::mutation_operations_schema(true);
    assert_eq!(operations["maxItems"], 1000);
    assert_eq!(
        query_contract::mutation_operations_schema(false)["maxItems"],
        0
    );
    let variants = operations["items"]["oneOf"].as_array().unwrap();
    let examples = [
        json!({"kind":"create","entity":"post","temporary_id":"draft","payload":{"body":"hello"}}),
        json!({"kind":"update","entity":"turn_cursor","record_id":"singleton","patch":{"turns_taken":1}}),
        json!({"kind":"delete","entity":"post","record_id":"post_a"}),
        json!({"kind":"restore","entity":"post","record_id":"post_a"}),
        json!({"kind":"create_relation","relation":"parent","from_record_id":"post_a","to_record_id":"post_b","expected_from_revision":1,"expected_to_revision":2}),
        json!({"kind":"delete_relation","relation":"parent","from_record_id":"post_a","to_record_id":"post_b","expected_from_revision":1,"expected_to_revision":2}),
    ];
    assert_eq!(variants.len(), examples.len());
    for example in examples {
        let decoded: AppMutationOperation = serde_json::from_value(example.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), example);
        let variant = variants
            .iter()
            .find(|v| v["properties"]["kind"]["const"] == example["kind"])
            .unwrap();
        assert_eq!(variant["additionalProperties"], false);
        for key in variant["required"].as_array().unwrap() {
            assert!(example.get(key.as_str().unwrap()).is_some());
        }
        for key in example.as_object().unwrap().keys() {
            assert!(variant["properties"].get(key).is_some());
        }
    }
    let revisions = query_contract::expected_record_revisions_schema();
    assert_eq!(revisions["items"]["additionalProperties"], false);
    assert!(revisions["items"]["properties"].get("revision").is_some());
    assert!(revisions["items"]["properties"]
        .get("record_revision")
        .is_none());
    assert!(!serde_json::to_string(&operations)
        .unwrap()
        .contains("\"$ref\""));
    assert!(!serde_json::to_string(&revisions)
        .unwrap()
        .contains("\"$ref\""));
    let good = json!({
        "operations":[{"kind":"update","entity":"turn_cursor","record_id":"singleton","patch":{"turns_taken":1}}],
        "expected_record_revisions":[{"entity":"turn_cursor","record_id":"singleton","revision":2}]
    });
    assert!(serde_json::from_value::<workflows::AppWorkflowTerminalRequest>(good.clone()).is_ok());
    let mut wrong_revision = good.clone();
    wrong_revision["expected_record_revisions"][0]
        .as_object_mut()
        .unwrap()
        .insert("record_revision".into(), json!(2));
    assert!(
        serde_json::from_value::<workflows::AppWorkflowTerminalRequest>(wrong_revision).is_err()
    );
    let mut wrong_kind = good;
    let operation = wrong_kind["operations"][0].as_object_mut().unwrap();
    operation.remove("kind");
    operation.insert("op".into(), json!("update"));
    assert!(serde_json::from_value::<workflows::AppWorkflowTerminalRequest>(wrong_kind).is_err());
}

#[test]
fn store_query_schema_exposes_canonical_wire_types_without_dangling_references() {
    fn no_refs(value: &Value) {
        match value {
            Value::Object(fields) => {
                assert!(
                    !fields.contains_key("$ref"),
                    "embedded parameter has a root-relative reference"
                );
                fields.values().for_each(no_refs);
            },
            Value::Array(values) => values.iter().for_each(no_refs),
            _ => {},
        }
    }
    let order = query_contract::order_schema();
    no_refs(&order);
    assert_eq!(order["items"]["additionalProperties"], false);
    let directions = order["items"]["properties"]["direction"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(directions.len(), 2);
    for direction in directions {
        let value = json!({"field":"synced_at", "direction":direction});
        let decoded: models::AppQueryOrder = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
    assert!(serde_json::from_value::<models::AppQueryOrder>(
        json!({"field":"synced_at","direction":"desc"})
    )
    .is_err());
    let predicate = query_contract::predicate_schema();
    no_refs(&predicate);
    assert!(predicate["properties"].get("root").is_some());
    assert!(predicate["properties"].get("nodes").is_some());
    let value = json!({"root":0,"nodes":[{"kind":"compare","field":"status","operator":"equal","value":"active"}]});
    let decoded: models::AppPredicate = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    let expansions = query_contract::relation_expansions_schema();
    no_refs(&expansions);
    for field in ["relation", "select", "max_depth", "max_rows"] {
        assert!(expansions["items"]["properties"].get(field).is_some());
    }
}

#[test]
fn scheduled_model_projection_requires_exact_live_selector_revision_content_and_policy() {
    use models::{AppDataEnvelope, AppDataSource, AppDigest, AppReference, AppScopeBindingRef};
    let selector: manifest::AppManifestBehaviorInputSelector = serde_json::from_value(json!({
        "entity":"turn_cursor", "record_id":"singleton", "fields":["turns_taken"]
    }))
    .unwrap();
    let policy: records::AppDataHandlingPolicy = serde_json::from_value(json!({
        "classification_floor":"sensitive", "model_processing":"remote_allowed",
        "personal_agent_access":"approved_projection", "memory_promotion":"denied",
        "external_egress":"denied", "approved_destinations":[]
    }))
    .unwrap();
    let selector_digest = AppDigest::blake3_canonical_json(&json!({
        "entity":"turn_cursor", "record_id":"singleton"
    }))
    .unwrap();
    let value = json!({"turns_taken":0});
    let input: AppDataEnvelope<Value> = serde_json::from_value(json!({
        "protocol_version":"1", "source":"app_store", "scope_binding_ref":"scope_test_default",
        "installation_id":"install_test", "package_revision_ref":"package:test", "schema_revision":1,
        "grant_revision":1, "value_schema_ref":"schema:test", "value":value,
        "source_refs":[{"kind":"entity_field", "reference":format!("record:{selector_digest}"),
            "revision":2, "fields":["turns_taken"]}],
        "handling_labels":{"classification":"sensitive", "model_processing":"remote_allowed",
            "policy_digest":AppDigest::blake3_canonical_json(&serde_json::to_value(&policy).unwrap()).unwrap(),
            "provenance_digest":AppDigest::blake3(b"source")},
        "content_digest":AppDigest::blake3_canonical_json(&value).unwrap(),
        "produced_at":Utc::now(), "expires_at":null
    })).unwrap();
    let approve = |current: &AppDataEnvelope<Value>,
                   current_policy: &records::AppDataHandlingPolicy| {
        scheduled_input::approve_current_scheduled_projection(
            &selector,
            &input,
            &policy,
            current,
            current_policy,
        )
    };
    let proof = approve(&input, &policy).unwrap();
    assert_eq!(proof.entity.as_str(), "turn_cursor");
    assert_eq!(
        proof.fields,
        vec![AppFieldPath::parse("turns_taken").unwrap()]
    );
    assert_eq!(
        proof.record_revisions,
        vec![AppReference::parse(format!("record:{selector_digest}@2")).unwrap()]
    );
    let mut changed = input.clone();
    changed.source_refs[0].revision = Some(AppRevision::new(3).unwrap());
    assert!(approve(&changed, &policy).is_err());
    changed = input.clone();
    changed.source_refs[0]
        .fields
        .push(AppFieldPath::parse("private_text").unwrap());
    assert!(approve(&changed, &policy).is_err());
    changed = input.clone();
    changed.value["turns_taken"] = json!(1);
    assert!(approve(&changed, &policy).is_err());
    changed = input.clone();
    changed.scope_binding_ref = AppScopeBindingRef::parse("scope_other").unwrap();
    assert!(approve(&changed, &policy).is_err());
    changed = input.clone();
    changed.source = AppDataSource::UserInput;
    assert!(approve(&changed, &policy).is_err());
    let mut tightened = policy.clone();
    tightened.model_processing = models::AppModelProcessing::LocalOnly;
    assert!(approve(&input, &tightened).is_err());
    let mut wrong_selector = selector.clone();
    wrong_selector.record_id = models::AppRecordId::parse("another").unwrap();
    assert!(scheduled_input::approve_current_scheduled_projection(
        &wrong_selector,
        &input,
        &policy,
        &input,
        &policy
    )
    .is_err());
}

#[path = "../../magician/src/magician_v2/execution/agentic/app_tool_feedback.rs"]
mod app_tool_feedback;

#[test]
fn governed_failure_feedback_discards_private_details_and_cannot_bypass_result_checkpoints() {
    let mut failure = json!({"success":false, "error":"private-error-sentinel",
        "result":{"private":"private-result-sentinel"}, "output":"private-output-sentinel"});
    app_tool_feedback::normalize_unlabeled_feedback(&mut failure);
    assert!(app_tool_feedback::is_content_free_feedback(&failure));
    assert!(!failure.to_string().contains("sentinel"));
    assert_eq!(failure["success"], false);
    assert!(failure["guidance"]
        .as_str()
        .unwrap()
        .contains("record_revision"));
    let mut forged = failure.clone();
    forged["result"] = json!("private-result-sentinel");
    assert!(!app_tool_feedback::is_content_free_feedback(&forged));
    let mut checkpointed =
        json!({"success":false,"app_result_checkpoint":{"forged":true},"result":"data"});
    let before = checkpointed.clone();
    app_tool_feedback::normalize_unlabeled_feedback(&mut checkpointed);
    assert_eq!(checkpointed, before);
    assert!(!app_tool_feedback::is_content_free_feedback(&checkpointed));
    let mut success = json!({"success":true,"result":"unlabeled"});
    app_tool_feedback::normalize_unlabeled_feedback(&mut success);
    assert!(!app_tool_feedback::is_content_free_feedback(&success));
    assert!(app_tool_feedback::is_content_free_feedback(
        &json!({"status":"deferred"})
    ));
    assert!(!app_tool_feedback::is_content_free_feedback(
        &json!({"status":"deferred","result":"private"})
    ));
}

#[test]
fn store_query_preflight_rejects_schema_mistakes_without_record_or_cursor_io() {
    use magician::magician_v2::apps::{
        entity_store::{ActiveAppEntitySchema, AppEntityStoreError},
        manifest::{parse_app_manifest_yaml, AppPackageLimits},
        models::{AppDigest, AppInstallationId, AppQueryRequest, AppReference},
        query_semantics::AppQuerySemanticsError,
        records::{AppDataHandlingPolicy, AppGrantRevision, AppSchemaCompatibility},
        schema_compiler::{canonical_entity_schema_digest, compile_app_schema},
    };
    // Compile the actual shipped entity names. No runtime, database, Keychain,
    // resource ledger or record/cursor store is constructed for this preflight.
    let source = include_str!("../../magician_data_v3/system/town_square/app/SKILL.md");
    let yaml = source.splitn(3, "---").nth(1).unwrap();
    let manifest = parse_app_manifest_yaml(yaml.as_bytes(), &AppPackageLimits::default()).unwrap();
    let policy: AppDataHandlingPolicy = serde_json::from_value(json!({
        "classification_floor":"personal", "model_processing":"remote_allowed",
        "personal_agent_access":"approved_projection", "memory_promotion":"denied",
        "external_egress":"denied", "approved_destinations":[]
    }))
    .unwrap();
    let resource = json!({"max_input_tokens":1000,"max_output_tokens":1000,
        "max_cost_microusd":100000,"max_paid_tool_invocations":0,
        "max_active_seconds":60,"max_lifetime_seconds":600,"max_browser_network_actions":0,
        "max_concurrent_foreground_runs":1,"max_concurrent_background_runs":0,
        "max_records":1000,"max_payload_bytes":100000,"max_attachment_bytes":0,
        "max_monthly_tokens":10000,"max_monthly_cost_microusd":1000000});
    let digest = AppDigest::blake3(b"preflight-test");
    let now = Utc::now();
    let grant: AppGrantRevision = serde_json::from_value(json!({
        "installation_id":"install_preflight", "revision":1,
        "package_revision_ref":"package-revision:preflight",
        "requested_tools":[],"granted_tools":[],"requested_context_reads":[],"granted_context_reads":[],
        "requested_personal_agent_data_access":[],"granted_personal_agent_data_access":[],
        "requested_data_handling_policy":policy,"granted_data_handling_policy":policy,
        "granted_data_handling_policy_digest":digest,
        "requested_background_execution":{"mode":"denied"},"granted_background_execution":{"mode":"denied"},
        "requested_network_policy":{"mode":"denied"},"granted_network_policy":{"mode":"denied"},
        "requested_resource_ceiling":resource,"granted_resource_ceiling":resource,
        "approved_by":"actor:test", "approved_at":now,"authority_digest":digest
    })).unwrap();
    let compiled = compile_app_schema(
        manifest.manifest(),
        AppInstallationId::parse("install_preflight").unwrap(),
        AppReference::parse("package-revision:preflight").unwrap(),
        AppRevision::new(1).unwrap(),
        policy,
        AppSchemaCompatibility::Initial,
        None,
        &canonical_entity_schema_digest(manifest.manifest()).unwrap(),
        now,
    )
    .unwrap();
    let active = ActiveAppEntitySchema::from_reviewed_update(
        1,
        AppRevision::new(1).unwrap(),
        grant,
        compiled.into_revision(),
    )
    .unwrap();
    let catalog = query_contract::entity_catalog(&active);
    assert!(catalog["post"].get("member_id").is_none());
    assert_eq!(catalog["post"]["author_id"]["type"], "text");
    assert_eq!(catalog["mention"]["mentioned_member_id"]["required"], true);
    assert_eq!(catalog["post"]["parent_id"]["nullable"], true);
    assert_eq!(
        catalog["post"]["surface"]["values"],
        json!(["feed", "group"])
    );
    assert!(catalog["post"].get("record_id").is_none());
    assert!(catalog["post"].get("record_revision").is_none());
    assert!(!catalog.to_string().contains("effective_policy"));
    let base = json!({"protocol_version":models::AppProtocolVersion::V1, "source_installation_id":"install_preflight",
        "entity":"post", "select":["author_id","body"], "predicate":null,"order":[],
        "cursor":null,"limit":100,"relation_expansions":[],"purpose":"app_workflow_store_query"});
    let good: AppQueryRequest = serde_json::from_value(base.clone()).unwrap();
    active.validate_query_contract(&good).unwrap();
    for patch in [
        json!({"select":["member_id"]}),
        json!({"order":[{"field":"member_id","direction":"ascending"}]}),
        json!({"predicate":{"root":0,"nodes":[{"kind":"compare","field":"member_id","operator":"equal","value":"agent-a"}]}}),
    ] {
        let mut invalid = base.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let invalid: AppQueryRequest = serde_json::from_value(invalid).unwrap();
        assert!(matches!(active.validate_query_contract(&invalid),
            Err(AppEntityStoreError::Query(AppQuerySemanticsError::UnknownField(field))) if field == "member_id"));
    }
    let mut wrong_installation = good.clone();
    wrong_installation.source_installation_id = AppInstallationId::parse("install_other").unwrap();
    assert!(matches!(
        active.validate_query_contract(&wrong_installation),
        Err(AppEntityStoreError::ScopeOrIdentityMismatch)
    ));
    let mut unknown_entity = good.clone();
    unknown_entity.entity = AppName::parse("unknown_entity").unwrap();
    assert!(matches!(
        active.validate_query_contract(&unknown_entity),
        Err(AppEntityStoreError::UnknownEntity(_))
    ));
    let mut expired_cursor = good;
    expired_cursor.cursor = Some(AppReference::parse("cursor:missing").unwrap());
    active.validate_query_contract(&expired_cursor).unwrap();
    // Preflight is deliberately not cursor or read authority. The real store
    // must still reject this absent cursor under its transaction and fence.
}

fn lock_current_package(
    candidate: &magician::magician_v2::apps::manifest::AppPackageCandidate,
) -> Result<magician::magician_v2::apps::package_lock::AppPackageLock, String> {
    use magician::magician_v2::apps::{
        authoring_catalog::{resolve_authoring_primitive_catalog, AuthoringDiscoveryRoots},
        manifest::{AppDependencyKind, AppPackageLimits},
        models::{AppReference, APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION},
        package_lock::{lock_app_package_dependencies, AppVerifiedRegistryDependency},
        tool_catalog::{complete_declared_tool_evidence, AppReviewedToolCatalog},
    };
    let snapshot = resolve_authoring_primitive_catalog(&AuthoringDiscoveryRoots::default());
    let catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
        .map_err(|error| error.to_string())?;
    let contract = AppVerifiedRegistryDependency::from_trusted_registry_bytes(
        AppDependencyKind::Contract,
        AppReference::parse("contract:magician_contract").unwrap(),
        APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION.to_owned(),
        AppReference::parse("contract-revision:magician-contract-v2").unwrap(),
        AppRevision::new(1).unwrap(),
        include_bytes!("../../docs/contracts/app-platform/components/v1/contract.json"),
    )
    .map_err(|error| error.to_string())?;
    let evidence = complete_declared_tool_evidence(candidate, vec![contract], &catalog)
        .map_err(|error| error.to_string())?;
    lock_app_package_dependencies(candidate, evidence, &AppPackageLimits::default())
        .map_err(|error| error.to_string())
}

#[test]
fn all_five_current_packages_lock_their_exact_native_workflows() {
    use magician::magician_v2::apps::package_staging::admit_package_directory;
    for name in [
        "town_square",
        "meetings",
        "learning",
        "thinking_map",
        "claims_review",
    ] {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../magician_data_v3/system/{name}/app"))
            .canonicalize()
            .unwrap();
        let candidate = admit_package_directory(&root).unwrap();
        lock_current_package(&candidate).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

#[test]
fn reviewed_round_model_steps_cannot_be_attached_to_a_mechanical_recipe() {
    use magician::magician_v2::apps::{
        manifest::{build_app_package_candidate, AppBundleMember, AppPackageLimits},
        package_staging::admit_package_directory,
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../magician_data_v3/system/town_square/app")
        .canonicalize()
        .unwrap();
    let candidate = admit_package_directory(&root).unwrap();
    // Keep the reviewed behavior and its operation; substitute a valid compiled
    // StoreTransaction at its immutable entry point. Manifest parsing alone
    // cannot decide this: dependency locking must reject the model binding.
    let members = candidate
        .members()
        .iter()
        .map(|member| {
            let bytes = if member.path().as_str() == "recipes/take-ambient-turn.json" {
                include_bytes!(
                    "../../magician_data_v3/system/town_square/app/recipes/sync-feed.json"
                )
                .to_vec()
            } else {
                member.bytes().to_vec()
            };
            AppBundleMember::regular_file(member.path().as_str(), bytes).unwrap()
        })
        .collect();
    let mechanical = build_app_package_candidate(members, &AppPackageLimits::default()).unwrap();
    let error = lock_current_package(&mechanical).unwrap_err();
    assert!(
        error.contains("native recipe semantic steps do not match its reviewed behavior"),
        "{error}"
    );
}
