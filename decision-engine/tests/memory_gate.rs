//! The local-model memory gate through the engine: on an 8 GB machine a
//! tier of [laya, jev] binds Jev only, and the in-process model is never
//! loaded (its directory does not exist). Its own process: the reading is
//! read once, from `DECISION_HOST_MEMORY_GB`.

use decision_engine::Engine;

#[test]
fn an_8_gb_machine_routes_past_local_models() {
    std::env::set_var("DECISION_HOST_MEMORY_GB", "8");
    std::env::set_var("TYPESAFE_API_KEY", "test-key");
    let config = serde_yaml::from_str(
        "enabled: true
models:
  laya:
    adapter: laya-onnx
    model: laya-multilingual-int8
    model_dir: /nonexistent/laya
  kev-small:
    adapter: kev-mlx
    model: kev-0.8b-mlx
    model_dir: /nonexistent/kev
    min_memory_gb: 4
  jev:
    adapter: typesafe
    model: jev-1.13.0
tiers:
  small: [laya, jev]
  large: [kev-small, jev]
operations:
  secondary_action_judge:
    tier: small
    pack: tool_action_judge
    pack_version: '1.0.0'
    thresholds_by_model: {jev: {}}
  tool_action_judge:
    tier: large
    pack: tool_action_judge
    pack_version: '1.0.0'
    thresholds_by_model: {jev: {}}
",
    )
    .expect("settings parse");
    let engine = Engine::from_config(config);
    let routes = engine.operations().operations;
    let secondary = routes
        .iter()
        .find(|o| o.name == "secondary_action_judge")
        .unwrap();
    assert_eq!(
        secondary.route_cloud,
        vec!["jev"],
        "laya needs 16 GB and this machine has 8"
    );
    // kev-small asks for 4 GB, so the gate passes it; binding then fails on
    // the missing directory (or, without the mlx feature, on the build),
    // and the route still falls through to Jev.
    let primary = routes
        .iter()
        .find(|o| o.name == "tool_action_judge")
        .unwrap();
    assert_eq!(primary.route_cloud, vec!["jev"]);
}
