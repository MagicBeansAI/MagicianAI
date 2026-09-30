//! Phase A0/A1: frozen Antigravity (`agy`) headless NDJSON launch policy.
//!
//! Frozen against installed `agy` 1.1.19:
//!
//! - Print mode: `-p` / `--print` + `--output-format stream-json`.
//! - Piped (non-TTY) stdout emits NDJSON. stdin is `/dev/null` for `-p`.
//! - Events are `{ "event": "init"|"step_update"|"result", ... }`.
//! - Native id is `conversation_id`. Resume is `--conversation <id>`.
//!   Never `--continue`/`-c` (that is "most recent", not Magician's id).
//! - Build: `--dangerously-skip-permissions` → init `permission_mode:
//!   always-proceed`. Discuss: `--mode plan` → `request-review`.
//! - `--sandbox` and `--disable-slash-commands` on every Magician launch.
//! - Qualify is `--input-format stream-json` with no user message: init
//!   arrives without a model turn. Kill after init.
//! - Usage is token-shaped; there is no USD. Cost is unknown.
//!
//! Plan (archived): `docs/archive/plans/2026-08-23-vibedev-claude-code-and-agy-coding-engines-plan.md`.

use serde::Deserialize;
use serde_json::Value;

use super::factory::AgyTurnMode;

pub const AGY_MINIMUM_VERSION: &str = "1.1.19";
pub const AGY_PROTOCOL_FIXTURE: &str = include_str!("fixtures/agy_cli_protocol_1.1.19.json");
/// Where the Agy CLI keeps its Antigravity OAuth token, newest first. 1.2.x
/// writes `jetski-standalone-oauth-token` (the only name in the 1.2.9 binary)
/// and never the 1.1.19 file, so checking only the old path reported a
/// signed-in 1.2.x host as "sign in required" and hid Agy from VibeDev.
pub const AGY_OAUTH_TOKEN_RELATIVE_PATHS: &[&str] = &[
    ".gemini/jetski-standalone-oauth-token",
    ".gemini/antigravity-cli/antigravity-oauth-token",
];

const AGY_CHILD_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "TMPDIR",
    "LANG",
    "TERM",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "VERTEX_LOCATION",
    "CLOUD_ML_PROJECT",
];

const AGY_FORBIDDEN_RUNTIME_TOOLS: &[&str] = &[
    "search_web",
    "read_url_content",
    "call_mcp_tool",
    "invoke_subagent",
    "define_subagent",
    "manage_subagents",
    "browser_subagent",
    "generate_image",
    "send_message",
];

#[derive(Debug, Deserialize)]
pub struct ProtocolInventory {
    pub agy_cli_version: String,
    pub transport: String,
    pub output_format: String,
    pub resume_flag: String,
    pub never_continue_flag: bool,
    pub session_id_field: String,
    pub event_types: Vec<String>,
}

pub fn protocol_inventory() -> Result<ProtocolInventory, String> {
    serde_json::from_str(AGY_PROTOCOL_FIXTURE).map_err(|err| format!("agy protocol fixture: {err}"))
}

pub fn launch_args(
    mode: AgyTurnMode,
    prompt: &str,
    resume_session_id: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--sandbox".to_string(),
        "--disable-slash-commands".to_string(),
    ];
    match mode {
        AgyTurnMode::Build => {
            args.push("--dangerously-skip-permissions".to_string());
        },
        AgyTurnMode::Discuss => {
            args.push("--mode".to_string());
            args.push("plan".to_string());
        },
    }
    if let Some(id) = resume_session_id.map(str::trim).filter(|id| !id.is_empty()) {
        args.push("--conversation".to_string());
        args.push(id.to_string());
    }
    args
}

/// Isolation probe: no `-p`, no user JSONL. Init is the first event.
pub fn qualify_launch_args() -> Vec<String> {
    vec![
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--sandbox".to_string(),
        "--disable-slash-commands".to_string(),
    ]
}

pub fn agy_child_env_allowlist() -> &'static [&'static str] {
    AGY_CHILD_ENV_ALLOWLIST
}

pub fn agy_conversation_id_from(value: &Value) -> Option<String> {
    value
        .get("conversation_id")
        .or_else(|| value.pointer("/init/conversation_id"))
        .or_else(|| value.pointer("/result/conversation_id"))
        .or_else(|| value.pointer("/step_update/conversation_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

pub fn agy_event_name(value: &Value) -> &str {
    value.get("event").and_then(Value::as_str).unwrap_or("")
}

/// Fail-closed on `init`. Agy does not emit an MCP list; tools +
/// `permission_mode` are the positive evidence. Default catalog names
/// like `search_web` are not Ready-incompatible (they are always
/// advertised). Runtime use is fail-closed in [`agy_step_isolation_leak`].
pub fn agy_init_isolation_leak(init: &Value) -> Option<&'static str> {
    let payload = init.get("init").unwrap_or(init);
    if payload
        .get("permission_mode")
        .and_then(Value::as_str)
        .is_none()
    {
        return Some("permission_mode_unattested");
    }
    match payload.get("tools").and_then(Value::as_array) {
        None => Some("tools_unattested"),
        Some(tools) if tools.is_empty() => Some("tools_unattested"),
        Some(_) => None,
    }
}

pub fn agy_step_isolation_leak(step: &Value) -> Option<&'static str> {
    let payload = step.get("step_update").unwrap_or(step);
    if payload.get("subagent_info").is_some() {
        return Some("subagent");
    }
    let name = payload
        .get("tool_name")
        .or_else(|| payload.pointer("/tool_info/name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if name.is_empty() {
        return None;
    }
    if AGY_FORBIDDEN_RUNTIME_TOOLS.iter().any(|item| *item == name)
        || name.starts_with("browser_")
        || name.contains("mcp")
    {
        return Some("forbidden_tool");
    }
    None
}

pub fn agy_result_is_success(value: &Value) -> bool {
    agy_event_name(value) == "result"
        && value.pointer("/result/status").and_then(Value::as_str) == Some("SUCCESS")
}

pub fn agy_result_is_terminal(value: &Value) -> bool {
    agy_event_name(value) == "result"
}

pub fn agy_result_status(value: &Value) -> Option<&str> {
    value.pointer("/result/status").and_then(Value::as_str)
}

pub fn agy_version_meets_minimum(version: &str) -> bool {
    match (
        semver::Version::parse(version),
        semver::Version::parse(AGY_MINIMUM_VERSION),
    ) {
        (Ok(got), Ok(min)) => got >= min,
        _ => false,
    }
}

pub fn parse_agy_cli_version(output: &str) -> Option<String> {
    for raw in output.split(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == '-')) {
        let token = raw.trim().trim_start_matches('v');
        if token.is_empty() {
            continue;
        }
        if semver::Version::parse(token).is_ok() {
            return Some(token.to_string());
        }
    }
    None
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn has_pair(args: &[String], left: &str, right: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == left && pair[1] == right)
    }

    #[test]
    fn launch_args_build_skips_permissions_and_sandboxes() {
        let build = launch_args(AgyTurnMode::Build, "hello", None);
        assert_eq!(build[0], "-p");
        assert_eq!(build[1], "hello");
        assert!(has_pair(&build, "--output-format", "stream-json"));
        assert!(build
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(build.iter().any(|arg| arg == "--sandbox"));
        assert!(build.iter().any(|arg| arg == "--disable-slash-commands"));
        assert!(!build.iter().any(|arg| arg == "--continue" || arg == "-c"));
        assert!(!build.iter().any(|arg| matches!(
            arg.as_str(),
            "sh" | "bash" | "zsh" | "/bin/sh" | "/bin/bash" | "/bin/zsh"
        )));
    }

    #[test]
    fn launch_args_discuss_uses_plan_without_skip_permissions() {
        let discuss = launch_args(AgyTurnMode::Discuss, "plan this", None);
        assert!(has_pair(&discuss, "--mode", "plan"));
        assert!(!discuss
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(discuss.iter().any(|arg| arg == "--sandbox"));
        assert!(has_pair(&discuss, "--output-format", "stream-json"));
    }

    #[test]
    fn launch_args_resume_uses_conversation_not_continue() {
        let args = launch_args(
            AgyTurnMode::Build,
            "continue",
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        );
        assert!(has_pair(
            &args,
            "--conversation",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        ));
        assert!(!args.iter().any(|arg| arg == "--continue" || arg == "-c"));
    }

    #[test]
    fn qualify_argv_is_stream_json_input_without_print_prompt() {
        let args = qualify_launch_args();
        assert_ne!(args[0], "-p");
        assert!(has_pair(&args, "--input-format", "stream-json"));
        assert!(has_pair(&args, "--output-format", "stream-json"));
        assert!(!args.iter().any(|arg| arg == "-p"));
    }

    #[test]
    fn protocol_fixture_covers_the_frozen_argv() {
        let inventory = protocol_inventory().expect("fixture");
        assert_eq!(inventory.agy_cli_version, AGY_MINIMUM_VERSION);
        assert_eq!(inventory.transport, "ndjson");
        assert_eq!(inventory.output_format, "stream-json");
        assert!(inventory.never_continue_flag);
        assert_eq!(inventory.resume_flag, "--conversation");
        assert_eq!(inventory.session_id_field, "conversation_id");
    }

    #[test]
    fn version_floor_accepts_1_1_19_and_newer() {
        assert_eq!(parse_agy_cli_version("1.1.19"), Some("1.1.19".into()));
        assert!(agy_version_meets_minimum("1.1.19"));
        assert!(agy_version_meets_minimum("1.2.0"));
        assert!(!agy_version_meets_minimum("1.1.18"));
    }

    #[test]
    fn result_success_requires_status() {
        assert!(agy_result_is_success(&serde_json::json!({
            "event": "result",
            "result": { "status": "SUCCESS" }
        })));
        assert!(!agy_result_is_success(&serde_json::json!({
            "event": "result",
            "result": { "status": "ERROR" }
        })));
    }

    #[test]
    fn init_missing_tools_is_unattested() {
        assert_eq!(
            agy_init_isolation_leak(&serde_json::json!({
                "event": "init",
                "init": { "permission_mode": "always-proceed" }
            })),
            Some("tools_unattested")
        );
    }

    #[test]
    fn step_search_web_is_forbidden() {
        assert_eq!(
            agy_step_isolation_leak(&serde_json::json!({
                "event": "step_update",
                "step_update": { "step_type": "tool", "tool_name": "search_web" }
            })),
            Some("forbidden_tool")
        );
    }
}
