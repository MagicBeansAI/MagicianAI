use std::{collections::BTreeSet, process::Command};

#[test]
fn live_approve_does_not_load_local_config_and_is_one_redacted_failure() {
    let temporary = tempfile::tempdir().expect("temporary test directory");
    let secret_marker = "token-secret-must-not-escape";
    let missing_config = temporary.path().join(format!("{secret_marker}.yaml"));

    let output = Command::new(env!("CARGO_BIN_EXE_magician"))
        .args([
            "--config",
            missing_config
                .to_str()
                .expect("temporary path must be valid UTF-8"),
            "app",
            "--json",
            "approve",
            "install_fixture",
            "--review-digest",
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--api-base",
            "http://127.0.0.1:1",
        ])
        .env("RUST_LOG", "trace")
        .output()
        .expect("run magician App JSON command");

    assert_eq!(output.status.code(), Some(1));
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout must be UTF-8 JSON");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "stdout must contain exactly one JSON line");
    assert!(stdout.ends_with('\n'));
    assert!(!stdout.contains(secret_marker));
    assert!(!stdout.contains(&missing_config.display().to_string()));
    assert!(!stdout.contains('\u{1b}'));

    let envelope: serde_json::Value =
        serde_json::from_str(lines[0]).expect("stdout line must be one JSON value");
    let object = envelope
        .as_object()
        .expect("failure envelope must be an object");
    assert_eq!(
        object.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "schema_version",
            "cli_protocol_version",
            "command",
            "ok",
            "error",
        ]),
    );
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["cli_protocol_version"], "1.0.0");
    assert_eq!(envelope["command"], "approve");
    assert_eq!(envelope["ok"], false);
    let error = envelope["error"]
        .as_object()
        .expect("failure error must be an object");
    assert_eq!(
        error.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["code", "message"]),
    );
    assert_eq!(
        error.get("code").and_then(serde_json::Value::as_str),
        Some("app_authoring_command_failed")
    );
    assert_eq!(
        error.get("message").and_then(serde_json::Value::as_str),
        Some("The app authoring command failed without granting runtime authority.")
    );
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not initialize or interleave tracing"
    );
}
