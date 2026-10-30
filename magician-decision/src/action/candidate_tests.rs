use super::candidates::{prepare_candidate_set, prepare_candidates};
use decision_engine_contract::action::*;
use decision_engine_contract::wire::CONTRACT_VERSION;
use serde_json::{json, Value};

fn request(schema: Value, evidence: Value) -> ActionRequest {
    ActionRequest {
        contract_version: CONTRACT_VERSION,
        snapshot: "candidate-regression".into(),
        locality: Default::default(),
        context: ActionContext {
            goal: "Continue the requested work".into(),
            evidence: vec![ActionEvidence {
                id: "result-1".into(),
                value: evidence,
                call: None,
                succeeded: Some(true),
            }],
            ..Default::default()
        },
        tools: vec![ActionTool {
            name: "generic_tool".into(),
            description: String::new(),
            parameters: schema,
        }],
        phase: ActionPhase::Select,
        plan: ActionPlan::default(),
        consecutive_gated_steps: 0,
    }
}

fn pack_schema(yaml: &str) -> Value {
    let pack: Value = serde_yaml::from_str(yaml).unwrap();
    let properties = pack["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_owned(),
                p.get("schema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type":p["param_type"]})),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let required: Vec<_> = pack["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["required"] == true)
        .map(|p| p["name"].clone())
        .collect();
    let mut schema = json!({"type":"object", "properties":properties, "required":required});
    if let Some(constraints) = pack.get("parameter_constraints") {
        schema["allOf"] = json!([constraints]);
    }
    schema
}

#[test]
fn observed_file_path_alias_is_a_complete_call() {
    let schema = pack_schema(include_str!(
        "../../../magician/src/magician_v2/execution/embedded_pack_defs/read_file.yaml"
    ));
    let req = request(
        schema,
        json!({"result":{"data":{"path":"/work/report.txt"}}}),
    );
    let calls = prepare_candidates(&req);
    assert!(calls
        .iter()
        .any(|c| c.call.arguments == json!({"file_path":"/work/report.txt"})));
}

#[test]
fn required_and_optional_arguments_keep_their_record_pairs() {
    let schema = json!({"type":"object","properties":{"key":{"type":"string"},"region":{"type":"string"}},"required":["key","region"]});
    let req = request(
        schema,
        json!({"records":[{"key":"a","region":"north"},{"key":"b","region":"south"}]}),
    );
    let calls = prepare_candidates(&req);
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|c| c.call.arguments == json!({"key":"a","region":"north"})
            || c.call.arguments == json!({"key":"b","region":"south"})));
    let mut optional = req.clone();
    optional.tools[0].parameters["required"] = json!(["key"]);
    let enriched = prepare_candidates(&optional);
    assert!(calls
        .iter()
        .all(|call| enriched.iter().any(|candidate| candidate.call == call.call)));
    assert!(enriched
        .iter()
        .any(|c| c.call.arguments == json!({"key":"a"})));
}

#[test]
fn missing_sibling_values_cannot_complete_a_call() {
    let req = request(
        json!({"type":"object","properties":{"key":{"type":"string"},"region":{"type":"string"}},"required":["key","region"]}),
        json!({"records":[{"key":"a"},{"region":"north"}]}),
    );
    assert!(prepare_candidates(&req).is_empty());
}

#[test]
fn wrapper_fields_are_available_without_crossing_siblings() {
    let req = request(
        json!({"type":"object","properties":{"server":{"type":"string"},"tool_name":{"type":"string","x-magician-grounding-aliases":["name"]}},"required":["server","tool_name"]}),
        json!({"server":"srv","tools":[{"name":"inspect"},{"name":"lookup"}]}),
    );
    let calls = prepare_candidates(&req);
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.call.arguments["server"] == "srv"));
}

#[test]
fn optional_only_content_contracts_need_grounded_inputs() {
    for yaml in [
        include_str!(
            "../../../magician/src/magician_v2/execution/embedded_pack_defs/content_search.yaml"
        ),
        include_str!(
            "../../../magician/src/magician_v2/execution/embedded_pack_defs/content_read.yaml"
        ),
        include_str!("../../../magician/src/magician_v2/execution/embedded_pack_defs/shell.yaml"),
    ] {
        let schema = pack_schema(yaml);
        assert!(prepare_candidates(&request(schema.clone(), json!({"status":"ready"}))).is_empty());
        let check = jsonschema::options().build(&schema).unwrap();
        assert!(!check.is_valid(&json!({})));
    }
    let search = pack_schema(include_str!(
        "../../../magician/src/magician_v2/execution/embedded_pack_defs/content_search.yaml"
    ));
    let calls = prepare_candidates(&request(
        search,
        json!({"query":"bounded search","limit":5}),
    ));
    assert!(calls
        .iter()
        .any(|c| c.call.arguments == json!({"query":"bounded search","limit":5})));
    assert!(calls
        .iter()
        .any(|c| c.call.arguments == json!({"query":"bounded search"})));
}

#[test]
fn alternative_shapes_support_shell_status_without_inventing_commands() {
    let schema = pack_schema(include_str!(
        "../../../magician/src/magician_v2/execution/embedded_pack_defs/shell.yaml"
    ));
    let req = request(schema.clone(), json!({"job_id":"job-123"}));
    let calls = prepare_candidates(&req);
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|c| c.call.arguments["job_id"] == "job-123"
            && c.call.arguments.get("command").is_none()));
    let calls = prepare_candidates(&request(schema, json!({"command":"printf test"})));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call.arguments, json!({"command":"printf test"}));
}

#[test]
fn fresh_typed_values_only_and_no_optional_enum_or_boolean_invention() {
    let mut req = request(
        json!({"type":"object","properties":{"query":{"type":"string"},"mode":{"enum":["safe","other"]},"force":{"type":"boolean"}},"required":["query"]}),
        json!({"query":"current","force":"not a boolean"}),
    );
    assert_eq!(
        prepare_candidates(&req)[0].call.arguments,
        json!({"query":"current"})
    );
    req.context.evidence.push(ActionEvidence {
        id: "new".into(),
        value: json!({"other":"data"}),
        call: None,
        succeeded: Some(true),
    });
    assert!(prepare_candidates(&req).is_empty());
    req.context.evidence.last_mut().unwrap().value = json!({"query":"failed"});
    req.context.evidence.last_mut().unwrap().succeeded = Some(false);
    assert!(prepare_candidates(&req).is_empty());
}

#[test]
fn candidate_cap_is_fair_across_tools_and_empty_tools_do_not_starve_grounded_ones() {
    let mut req = request(
        json!({"type":"object","properties":{"key":{"type":"integer"}},"required":["key"]}),
        json!({"rows":[{"key":1},{"key":2},{"key":3}]}),
    );
    let template = req.tools[0].clone();
    req.tools = (0..20)
        .map(|i| ActionTool {
            name: format!("tool_{i}"),
            ..template.clone()
        })
        .collect();
    let calls = prepare_candidates(&req);
    assert_eq!(calls.len(), 60);
    assert_eq!(
        calls
            .iter()
            .take(20)
            .map(|c| c.call.tool.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        20
    );
    req.tools = (0..80)
        .map(|i| ActionTool {
            name: format!("empty_{i}"),
            description: String::new(),
            parameters: json!({"type":"object"}),
        })
        .collect();
    req.tools.push(template);
    let result = prepare_candidate_set(&req);
    assert_eq!(result.candidates.len(), 64);
    assert!(result.truncated);
    assert_eq!(result.candidates[0].call.tool, "generic_tool");
    assert_eq!(
        result
            .candidates
            .iter()
            .filter(|c| c.call.tool == "generic_tool")
            .count(),
        3
    );
    assert_eq!(result.candidates, prepare_candidates(&req));
}

#[test]
fn vector_contracts_reject_empty_items_and_mixed_scalar_inputs() {
    let schema = pack_schema(include_str!(
        "../../../magician/src/magician_v2/execution/embedded_pack_defs/content_read.yaml"
    ));
    let check = jsonschema::options().build(&schema).unwrap();
    for args in [
        json!({"requests":[{}]}),
        json!({"candidate":{}}),
        json!({"url":"https://example.test","requests":[{"url":"https://example.test"}]}),
    ] {
        assert!(!check.is_valid(&args), "{args}");
    }
    assert!(check.is_valid(&json!({"requests":[{"url":"https://example.test"}]})));
    assert!(check.is_valid(&json!({"url":"https://example.test"})));
}

#[test]
fn local_schema_references_keep_whole_call_validation_authoritative() {
    let schema = json!({"type":"object","$defs":{"id":{"type":"integer","minimum":1}},
        "properties":{"key":{"$ref":"#/$defs/id"}},"required":["key"]});
    assert_eq!(
        prepare_candidates(&request(schema.clone(), json!({"key":7}))).len(),
        1
    );
    assert!(prepare_candidates(&request(schema, json!({"key":-1}))).is_empty());
    let external = json!({"type":"object","properties":{"key":{"$ref":"https://example.test/schema"}},"required":["key"]});
    assert!(prepare_candidates(&request(external, json!({"key":7}))).is_empty());
}

#[test]
fn schema_valid_default_calls_remain_available_with_optional_filters() {
    let schema = json!({"type":"object","properties":{"profile":{"type":"string"},
        "decision_metadata":{"type":"object"}}});
    let calls = prepare_candidates(&request(schema, json!({"status":"ready"})));
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call.arguments, json!({}));
}
