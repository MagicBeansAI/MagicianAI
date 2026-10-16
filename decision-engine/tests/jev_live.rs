//! Live Jev through the shared action HTTP endpoint. Fixtures select calls;
//! they do not dispatch work tools. `make bench-decision-jev` opts in.
#[path = "fixtures/action.rs"]
mod fixture;
use decision_engine::Engine;
use decision_engine_contract::{action::ActionVerdict, client::EngineClient, wire::Locality};
use serde_json::json;
use std::{sync::Arc, time::Duration};

#[actix_web::test]
#[ignore = "live: requires TYPESAFE_EVAL_KEY and spends real Jev quota"]
async fn jev_evaluates_generic_tools_and_refuses_stale_evidence_over_the_shared_endpoint() {
    assert!(
        std::env::var("TYPESAFE_EVAL_KEY").is_ok(),
        "set TYPESAFE_EVAL_KEY to run this explicit live test"
    );
    let mut settings = fixture::settings(
        json!({"adapter":"typesafe","model":"jev-1.13.0","api_key_env":"TYPESAFE_EVAL_KEY"}),
    );
    // Live evaluations must use the configured policy, not the synthetic
    // fixture's fixed thresholds. An override can select the runtime config.
    let config_path = std::env::var_os("DECISION_JEV_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decision-engine.yaml")
        });
    let configured = decision_engine::parse_config(
        &std::fs::read_to_string(&config_path).expect("read Jev evaluation config"),
        &config_path,
    )
    .expect("parse Jev evaluation config");
    let thresholds = configured
        .operations
        .get("tool_action_judge")
        .expect("configured shared action operation")
        .thresholds
        .clone();
    println!("[JEV-SHARED] configured thresholds: {thresholds:?}");
    settings
        .operations
        .get_mut("tool_action_judge")
        .unwrap()
        .thresholds = thresholds;
    let engine = Arc::new(Engine::from_config(settings));
    let socket = std::path::PathBuf::from(format!("/tmp/jev-rail-{}.sock", std::process::id()));
    let path = socket.clone();
    let service = tokio::spawn(async move { decision_engine::serve(engine, &path).await });
    let client = EngineClient::new(&socket, Duration::from_secs(20));
    for _ in 0..100 {
        if client.health().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut selected = 0;
    for tool in [
        "browser_fixture__click",
        "android_fixture__tap",
        "cua_fixture__click",
        "previously_unknown_tool",
    ] {
        let req = fixture::request(tool, Locality::Cloud);
        let reply = client.action(&req).await.expect("shared endpoint");
        assert_eq!(
            reply.model.as_ref().expect("real model answered").adapter,
            "typesafe",
            "{reply:?}"
        );
        assert!(reply.usage.as_ref().unwrap().input_tokens > 0);
        if let ActionVerdict::Execute { call, .. } = &reply.verdict {
            selected += 1;
            assert_eq!(call.tool, tool);
            assert_eq!(call.arguments, json!({"target":"@e7"}));
        }
        println!(
            "[JEV-SHARED] tool={tool} reason={} model={:?} review={:?} latency_ms={} usage={:?}",
            reply.reason, reply.model, reply.review_model, reply.latency_ms, reply.usage
        );
    }
    // Gate coverage is a live-model metric, not a deterministic protocol
    // invariant. Report escalation against the configured thresholds.
    println!("[JEV-SHARED] structured selections: {selected}/4");
    let mut stale = fixture::request("browser_fixture__click", Locality::Cloud);
    stale.context.goal = "Click Continue on the current page after navigation.".into();
    stale.context.observation = json!({"page":"A different page after navigation","note":"No fresh snapshot exists. @e7 belonged to the previous page."});
    stale
        .context
        .evidence
        .push(decision_engine_contract::action::ActionEvidence {
            id: "navigation".into(),
            value: json!({"navigated":true,"old_targets_invalid":true}),
            call: None,
            succeeded: Some(true),
        });
    // Keep the obsolete target as a planned candidate so this tests Jev's
    // freshness judgment, rather than stopping at no_grounded_candidates.
    stale
        .plan
        .steps
        .push(decision_engine_contract::action::ActionCandidate {
            id: "old-target".into(),
            call: decision_engine_contract::action::ToolCall {
                tool: "browser_fixture__click".into(),
                arguments: json!({"target":"@e7"}),
            },
            bindings: Vec::new(),
            reason: "A plan from before navigation".into(),
        });
    let reply = client.action(&stale).await.unwrap();
    assert_eq!(
        reply
            .model
            .as_ref()
            .expect("Jev judged the stale candidate")
            .adapter,
        "typesafe"
    );
    assert!(
        matches!(reply.verdict, ActionVerdict::NeedPlanner { .. }),
        "{reply:?}"
    );
    println!(
        "[JEV-SHARED] stale: {} ({} ms)",
        reply.reason, reply.latency_ms
    );
    service.abort();
    let _ = std::fs::remove_file(socket);
}
