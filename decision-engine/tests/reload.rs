//! Settings changes apply to a running engine: new policy and routes swap
//! in, and settings that do not parse leave the current ones serving.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decision_engine::Engine;
use decision_engine_contract::wire::OperationPolicy;

/// A loopback System One model: binds a body-seeing op in both locality
/// modes without a key or network at bind time.
fn settings(shadow: bool, gate: bool, secondary: bool) -> String {
    let mut yaml = format!(
        "enabled: true\nmodels:\n  laya:\n    adapter: systemone\n    model: laya-typed-decisions\n    endpoint: http://127.0.0.1:9/v1/systemone\noperations:\n  tool_action_judge:\n    model: laya\n    pack: tool_action_judge\n    pack_version: '1.0.0'\n    sees_body: true\n    shadow:\n      enabled: {shadow}\n    gate:\n      enabled: {gate}\n"
    );
    if secondary {
        yaml.push_str("  secondary_action_judge:\n    model: laya\n    pack: tool_action_judge\n    pack_version: '1.0.0'\n    sees_body: true\n");
    }
    yaml
}

fn policies(engine: &Engine) -> Vec<(String, bool, bool, Vec<String>)> {
    engine
        .operations()
        .operations
        .into_iter()
        .map(
            |OperationPolicy {
                 name,
                 shadow,
                 gate,
                 route_cloud,
                 ..
             }| (name, shadow, gate, route_cloud),
        )
        .collect()
}

fn parse(yaml: &str) -> magician_decision::config::DecisionConfig {
    serde_yaml::from_str(yaml).expect("parses")
}

#[test]
fn a_reload_swaps_in_the_new_policy_and_routes() {
    let engine = Engine::from_config(parse(&settings(true, false, false)));
    assert_eq!(
        policies(&engine),
        vec![("tool_action_judge".into(), true, false, vec!["laya".into()])]
    );
    engine.reload(parse(&settings(false, true, true)));
    assert_eq!(
        policies(&engine),
        vec![
            (
                "secondary_action_judge".into(),
                false,
                false,
                vec!["laya".into()]
            ),
            ("tool_action_judge".into(), false, true, vec!["laya".into()]),
        ]
    );
    engine.reload(parse("enabled: false\n"));
    assert!(policies(&engine).is_empty());
}

fn wait_for(engine: &Engine, want: impl Fn(&[(String, bool, bool, Vec<String>)]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !want(&policies(engine)) {
        assert!(
            Instant::now() < deadline,
            "settings change not applied: {:?}",
            policies(engine)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_settings_file_is_watched() {
    let dir = std::env::temp_dir().join(format!("decision-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("decision-engine.yaml");
    let first = settings(true, false, false);
    std::fs::write(&path, &first).unwrap();
    let engine = Arc::new(Engine::from_config(parse(&first)));
    decision_engine::watch_settings(
        Arc::clone(&engine),
        path.clone(),
        Some(first),
        Duration::from_millis(20),
        |_| {},
    )
    .unwrap();

    std::fs::write(&path, settings(true, true, false)).unwrap();
    wait_for(&engine, |p| p.len() == 1 && p[0].2);

    // A broken edit keeps the current settings; fixing it applies.
    std::fs::write(&path, "enabled: true\nbogus: 1\n").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        policies(&engine),
        vec![("tool_action_judge".into(), true, true, vec!["laya".into()])]
    );
    std::fs::write(&path, settings(false, false, true)).unwrap();
    wait_for(&engine, |p| p.len() == 2);
    let _ = std::fs::remove_dir_all(&dir);
}
