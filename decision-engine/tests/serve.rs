//! The unified engine over a real Unix socket, driven by the host client.
use decision_engine::Engine;
use decision_engine_contract::client::{ClientError, EngineClient};
use decision_engine_contract::wire::{DecideRequest, DecideStatus, Locality, CONTRACT_VERSION};
use magician_decision::adapters::MemoryDecisionModel;
use magician_decision::config::DecisionConfig;
use magician_decision::primitives::OptionId;
use magician_decision::request::{Answer, DecisionState};
use magician_decision::{DecisionRuntimeBuilder, PackStore};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn engine() -> Arc<Engine> {
    let model = MemoryDecisionModel::new("memory", "scripted")
        .with_answer(
            "next_action",
            Answer::Choice {
                choice: OptionId::new("read"),
                probabilities: BTreeMap::from([(OptionId::new("read"), 1.0)]),
                confidence: 1.0,
            },
        )
        .with_answer("evidence_sufficient", Answer::Noul { noul: 0.99 })
        .with_answer("action_applicable", Answer::Noul { noul: 0.99 });
    let pack = PackStore::new(None)
        .load("tool_action_judge", "1.0.0")
        .unwrap();
    let cloud = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind("tool_action_judge", pack, Arc::new(model))
            .build(),
    );
    let config: DecisionConfig = serde_yaml::from_str(
        "enabled: true\noperations:\n  tool_action_judge:\n    model: scripted\n    pack: tool_action_judge\n    sees_body: true\n    gate: {enabled: true}\n"
    ).unwrap();
    Arc::new(Engine::with_runtimes(config, None, Some(cloud)))
}
async fn serving() -> (EngineClient, PathBuf) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let socket = PathBuf::from(format!(
        "/tmp/de-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let path = socket.clone();
    tokio::spawn(async move {
        let _ = decision_engine::serve(engine(), &path).await;
    });
    let client = EngineClient::new(&socket, Duration::from_secs(5));
    for _ in 0..100 {
        if client.health().await.is_ok() {
            return (client, socket);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("engine did not start");
}
fn request() -> DecideRequest {
    DecideRequest {
        batch: Default::default(),
        contract_version: CONTRACT_VERSION,
        operation: "tool_action_judge".into(),
        state: DecisionState::from_text("Read the selected record."),
        choice_candidates: BTreeMap::from([(
            "next_action".into(),
            vec![("read".into(), "Read the record".into())],
        )]),
        locality: Locality::Cloud,
    }
}
#[actix_web::test]
async fn health_and_operations_report_only_the_shared_route() {
    let (client, socket) = serving().await;
    assert_eq!(client.health().await.unwrap().status, "ok");
    let ops = client.operations().await.unwrap();
    assert_eq!(ops.action_contract_version, Some(CONTRACT_VERSION));
    assert_eq!(ops.operations.len(), 1);
    let op = &ops.operations[0];
    assert_eq!(op.name, "tool_action_judge");
    assert!(op.gate);
    assert_eq!(op.route_cloud, vec!["scripted"]);
    assert!(op.route_local.is_empty());
    let _ = std::fs::remove_file(socket);
}
#[actix_web::test]
async fn retired_surface_endpoint_has_no_handler() {
    let app = actix_web::test::init_service(
        actix_web::App::new()
            .app_data(actix_web::web::Data::from(engine()))
            .configure(decision_engine::routes),
    )
    .await;
    let req = actix_web::test::TestRequest::post()
        .uri("/v1/step")
        .set_json(serde_json::json!({}))
        .to_request();
    assert_eq!(
        actix_web::test::call_service(&app, req).await.status(),
        actix_web::http::StatusCode::NOT_FOUND
    );
}
#[actix_web::test]
async fn typed_decisions_preserve_locality_thresholds_and_contract_checks() {
    let (client, socket) = serving().await;
    let req = request();
    let response = client.decide(&req).await.unwrap();
    assert_eq!(response.status, DecideStatus::Answered);
    assert_eq!(response.thresholds, Some(BTreeMap::new()));
    for changed in [
        DecideRequest {
            operation: "missing".into(),
            ..req.clone()
        },
        DecideRequest {
            locality: Locality::Local,
            ..req.clone()
        },
        DecideRequest {
            contract_version: CONTRACT_VERSION - 1,
            ..req
        },
    ] {
        assert_eq!(
            client.decide(&changed).await.unwrap().status,
            DecideStatus::Unbound
        );
    }
    let _ = std::fs::remove_file(socket);
}
#[tokio::test]
async fn no_engine_is_a_transport_error() {
    let client = EngineClient::new(
        "/nonexistent/decision-engine.sock",
        Duration::from_millis(200),
    );
    assert!(matches!(
        client.health().await,
        Err(ClientError::Connect(_))
    ));
    let reply = client.decide_or_unbound(&request()).await;
    assert_eq!(reply.status, DecideStatus::Unbound);
    assert!(reply.error.unwrap().contains("unreachable"));
}

#[test]
fn settings_load_from_the_engines_own_file_only() {
    let dir = std::env::temp_dir().join(format!("de-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let own = dir.join("decision-engine.yaml");
    std::fs::write(&own, "enabled: true\ntiers:\n  small: [laya]\n").expect("write");
    assert_eq!(
        decision_engine::load_config(&own).expect("own").tiers.small,
        vec!["laya"]
    );
    // A host config's `decision:` block is not the engine's settings: the
    // host keeps none of them, so a file shaped that way is refused rather
    // than half-read.
    let host = dir.join("magician-config.yaml");
    std::fs::write(
        &host,
        "decision:\n  enabled: true\n  tiers:\n    large: [jev]\n",
    )
    .expect("write");
    let error = decision_engine::load_config(&host).expect_err("host shape refused");
    assert!(error.contains("decision"), "{error}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn model_folders_and_onnx_runtime_resolve_from_settings() {
    use std::path::Path;
    let root = Path::new("/rt");
    let home = Path::new("/home/me");
    let parse = |yaml: &str| -> magician_decision::config::DecisionConfig {
        serde_yaml::from_str(yaml).expect("settings")
    };
    let dir = |config: &magician_decision::config::DecisionConfig, name: &str| {
        config.models[name].model_dir.clone().unwrap()
    };

    // Defaults: folders under <root>/models/decision, runtime under <root>/lib.
    let mut config = parse(
        "models:\n  a: {adapter: laya-onnx, model: m, model_dir: laya-multilingual}\n  b: {adapter: kev-mlx, model: m, model_dir: /abs/kev}\n",
    );
    decision_engine::resolve_model_dirs(&mut config, root, Some(home));
    assert_eq!(dir(&config, "a"), "/rt/models/decision/laya-multilingual");
    assert_eq!(
        dir(&config, "b"),
        "/abs/kev",
        "an absolute model_dir is kept"
    );
    let library = decision_engine::onnxruntime_library(&config, root, Some(home));
    assert!(
        library
            .display()
            .to_string()
            .starts_with("/rt/lib/onnxruntime/libonnxruntime."),
        "{library:?}"
    );

    // An alternate models folder (home-relative) and runtime file.
    let mut config = parse(
        "models_dir: ~/ssd/decision-models\nonnxruntime_path: /opt/ort/lib/libonnxruntime.1.30.0.dylib\nmodels:\n  a: {adapter: laya-onnx, model: m, model_dir: laya-multilingual}\n",
    );
    decision_engine::resolve_model_dirs(&mut config, root, Some(home));
    assert_eq!(
        dir(&config, "a"),
        "/home/me/ssd/decision-models/laya-multilingual"
    );
    assert_eq!(
        decision_engine::onnxruntime_library(&config, root, Some(home)),
        Path::new("/opt/ort/lib/libonnxruntime.1.30.0.dylib")
    );

    // A models folder relative to the runtime root.
    let mut config = parse(
        "models_dir: shared/models\nmodels:\n  a: {adapter: kev-onnx, model: m, model_dir: kev-0.8b}\n",
    );
    decision_engine::resolve_model_dirs(&mut config, root, Some(home));
    assert_eq!(dir(&config, "a"), "/rt/shared/models/kev-0.8b");
}
