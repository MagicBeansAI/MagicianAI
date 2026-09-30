//! Phase C0/C1: frozen Claude Code headless NDJSON launch policy.
//!
//! Frozen here:
//!
//! - Headless uses `--print`/`-p` AND `--output-format stream-json` AND
//!   `--verbose` (stream-json fails without `--verbose`).
//! - The stream is NDJSON, not JSON-RPC.
//! - Magician never impersonates the Claude TUI. Never `--bare` (that
//!   forces API-key billing and disables OAuth/keychain).
//! - Child env is rebuilt from `PATH`, `HOME`, `USER`, `TMPDIR`, `LANG`,
//!   `TERM`. API keys are copied only when `coding.claude.use_api_key` is
//!   true. `MAGICIAN_*` is dropped.
//! - Native continuation is Claude `session_id` (UUID) on
//!   `CodingContinuationRef.native_session_id`. Magician's
//!   `result.session_id` is `claude-{uuid}`.
//!
//! Plan (archived): `docs/archive/plans/2026-08-23-vibedev-claude-code-and-agy-coding-engines-plan.md`.

use serde::Deserialize;
use serde_json::Value;

use super::factory::ClaudeTurnMode;

pub const CLAUDE_MINIMUM_VERSION: &str = "2.1.229";
pub const CLAUDE_PROTOCOL_FIXTURE: &str =
    include_str!("fixtures/claude_code_protocol_2.1.229.json");

pub const CLAUDE_DISALLOWED_TOOLS: &str = "WebSearch,WebFetch,Task,CronCreate,CronDelete,CronList,PushNotification,RemoteTrigger,Monitor,SendMessage,Workflow,ToolSearch";

const CLAUDE_CHILD_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME", "USER", "TMPDIR", "LANG", "TERM"];

const CLAUDE_API_KEY_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
];

#[derive(Debug, Deserialize)]
pub struct ProtocolInventory {
    pub claude_cli_version: String,
    pub transport: String,
    pub output_format: String,
    pub requires_verbose: bool,
    pub never_bare: bool,
    pub resume_flag: String,
    pub session_id_field: String,
    pub event_types: Vec<String>,
    pub launch_argv_build: Vec<String>,
    pub launch_argv_discuss_permission_mode: String,
    pub disallowed_tools: Vec<String>,
}

pub fn protocol_inventory() -> Result<ProtocolInventory, String> {
    serde_json::from_str(CLAUDE_PROTOCOL_FIXTURE)
        .map_err(|err| format!("claude protocol fixture: {err}"))
}

/// Arguments after the Claude executable. Discovery chooses the binary path.
/// Prompt is a single `-p` argv. Never `sh -c`. Never `--bare`.
pub fn launch_args(
    mode: ClaudeTurnMode,
    prompt: &str,
    resume_session_id: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--permission-mode".to_string(),
        mode.permission_mode().to_string(),
    ];
    if mode.includes_skip_permissions() {
        args.push("--dangerously-skip-permissions".to_string());
    }
    args.extend([
        "--strict-mcp-config".to_string(),
        "--setting-sources".to_string(),
        String::new(),
        "--disable-slash-commands".to_string(),
        "--no-chrome".to_string(),
        "--safe-mode".to_string(),
        "--disallowedTools".to_string(),
        CLAUDE_DISALLOWED_TOOLS.to_string(),
    ]);
    if let Some(id) = resume_session_id.map(str::trim).filter(|id| !id.is_empty()) {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }
    args
}

/// Isolation probe argv. No prompt token: the probe writes one stdin
/// JSONL user event, drains `system/init`, then kills the process group.
/// `--no-session-persistence` so the probe is not a VibeDev continuation.
/// Never `--bare` (that forces API-key billing).
pub fn qualify_launch_args() -> Vec<String> {
    vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--permission-mode".to_string(),
        ClaudeTurnMode::Build.permission_mode().to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--strict-mcp-config".to_string(),
        "--setting-sources".to_string(),
        String::new(),
        "--disable-slash-commands".to_string(),
        "--no-chrome".to_string(),
        "--safe-mode".to_string(),
        "--disallowedTools".to_string(),
        CLAUDE_DISALLOWED_TOOLS.to_string(),
        "--no-session-persistence".to_string(),
    ]
}

/// One stdin JSONL user event. Enough to make Claude emit `system/init`.
/// The worker kills after init so this must not become a billed turn
/// that Magician waits on.
pub fn qualify_user_line() -> Vec<u8> {
    let mut line =
        br#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"."}]}}"#
            .to_vec();
    line.push(b'\n');
    line
}

pub fn claude_child_env_allowlist() -> &'static [&'static str] {
    CLAUDE_CHILD_ENV_ALLOWLIST
}

pub fn claude_api_key_env_names() -> &'static [&'static str] {
    CLAUDE_API_KEY_ENV
}

pub fn is_claude_api_key_env(key: &str) -> bool {
    CLAUDE_API_KEY_ENV.iter().any(|name| *name == key)
}

pub fn claude_session_id_from(value: &Value) -> Option<String> {
    value
        .get("session_id")
        .or_else(|| value.get("sessionId"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

const CLAUDE_FORBIDDEN_INIT_TOOLS: &[&str] = &[
    "WebSearch",
    "WebFetch",
    "Task",
    "CronCreate",
    "CronDelete",
    "CronList",
    "PushNotification",
    "RemoteTrigger",
    "Monitor",
    "SendMessage",
    "Workflow",
    "ToolSearch",
];

/// Plugins the Claude CLI bundles and always reports, even under
/// `--setting-sources ""` and `--safe-mode` (2.1.281: `agents-md`,
/// `telemetry`). They add no tools — the tool list is checked separately — so
/// they do not break isolation. Anything else in `plugins`, including a
/// builtin not reviewed here, still fails closed.
const CLAUDE_REVIEWED_BUILTIN_PLUGINS: &[&str] = &["agents-md", "telemetry"];

fn is_reviewed_builtin_plugin(plugin: &Value) -> bool {
    let Some(name) = plugin.get("name").and_then(Value::as_str) else {
        return false;
    };
    CLAUDE_REVIEWED_BUILTIN_PLUGINS.contains(&name)
        && plugin.get("path").and_then(Value::as_str) == Some("builtin")
        && plugin
            .get("source")
            .and_then(Value::as_str)
            .is_some_and(|source| source == format!("{name}@builtin"))
}

/// Fail-closed isolation on a `system/init` event. Missing MCP/tool lists
/// are unattested, not isolated. Present dirty lists abort the turn.
pub fn claude_init_isolation_leak(init: &Value) -> Option<&'static str> {
    match init.get("mcp_servers").and_then(Value::as_array) {
        None => return Some("mcp_servers_unattested"),
        Some(servers) if !servers.is_empty() => return Some("mcp_servers"),
        Some(_) => {},
    }
    if let Some(plugins) = init.get("plugins").and_then(Value::as_array) {
        if !plugins.iter().all(is_reviewed_builtin_plugin) {
            return Some("plugins");
        }
    }
    let Some(tools) = init.get("tools").and_then(Value::as_array) else {
        return Some("tools_unattested");
    };
    for tool in tools {
        let name = tool
            .as_str()
            .or_else(|| tool.get("name").and_then(Value::as_str))
            .unwrap_or("");
        if CLAUDE_FORBIDDEN_INIT_TOOLS.iter().any(|item| *item == name)
            || name.contains("WebSearch")
            || name.contains("__")
            || name.starts_with("mcp__")
        {
            return Some("forbidden_tool");
        }
    }
    None
}

/// When Magician is using Max/OAuth, init must not report an API key source.
pub fn claude_init_api_key_source_leak(init: &Value, use_api_key: bool) -> Option<&'static str> {
    if use_api_key {
        return None;
    }
    match init.get("apiKeySource").and_then(Value::as_str) {
        None | Some("none") | Some("oauth") | Some("claude.ai") => None,
        Some(_) => Some("api_key_source"),
    }
}

pub fn claude_result_is_success(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("result")
        && value.get("subtype").and_then(Value::as_str) == Some("success")
        && value.get("is_error").and_then(Value::as_bool) != Some(true)
}

pub fn claude_result_is_terminal(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("result")
}

pub fn claude_version_meets_minimum(version: &str) -> bool {
    match (
        semver::Version::parse(version),
        semver::Version::parse(CLAUDE_MINIMUM_VERSION),
    ) {
        (Ok(got), Ok(min)) => got >= min,
        _ => false,
    }
}

/// First `x.y.z` token in `claude --version` output.
pub fn parse_claude_cli_version(output: &str) -> Option<String> {
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
    fn launch_args_build_has_bypass_and_skip_permissions() {
        let build = launch_args(ClaudeTurnMode::Build, "hello", None);
        assert_eq!(build[0], "-p");
        assert_eq!(build[1], "hello");
        assert!(has_pair(&build, "--output-format", "stream-json"));
        assert!(build.iter().any(|arg| arg == "--verbose"));
        assert!(has_pair(&build, "--permission-mode", "bypassPermissions"));
        assert!(build
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(build.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(has_pair(&build, "--setting-sources", ""));
        assert!(has_pair(
            &build,
            "--disallowedTools",
            CLAUDE_DISALLOWED_TOOLS
        ));
        assert!(CLAUDE_DISALLOWED_TOOLS.contains("WebSearch"));
        assert!(!build.iter().any(|arg| arg == "--bare"));
        assert!(!build.iter().any(|arg| arg == "--no-session-persistence"));
        assert!(!build.iter().any(|arg| matches!(
            arg.as_str(),
            "sh" | "bash" | "zsh" | "/bin/sh" | "/bin/bash" | "/bin/zsh"
        )));
    }

    #[test]
    fn launch_args_discuss_uses_plan_and_omits_skip_permissions() {
        let discuss = launch_args(ClaudeTurnMode::Discuss, "plan this", None);
        assert!(has_pair(&discuss, "--permission-mode", "plan"));
        assert!(!discuss
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(discuss.iter().any(|arg| arg == "--verbose"));
        assert!(has_pair(&discuss, "--output-format", "stream-json"));
        assert!(discuss.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(has_pair(
            &discuss,
            "--disallowedTools",
            CLAUDE_DISALLOWED_TOOLS
        ));
        assert!(!discuss.iter().any(|arg| arg == "--bare"));
    }

    #[test]
    fn qualify_launch_args_has_stream_json_input_and_never_bare() {
        let args = qualify_launch_args();
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "--output-format");
        assert!(has_pair(&args, "--input-format", "stream-json"));
        assert!(has_pair(&args, "--output-format", "stream-json"));
        assert!(args.iter().any(|arg| arg == "--verbose"));
        assert!(args.iter().any(|arg| arg == "--no-session-persistence"));
        assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(has_pair(&args, "--setting-sources", ""));
        assert!(has_pair(
            &args,
            "--disallowedTools",
            CLAUDE_DISALLOWED_TOOLS
        ));
        assert!(args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(!args.iter().any(|arg| arg == "--bare"));
        assert!(!args.iter().any(|arg| matches!(
            arg.as_str(),
            "sh" | "bash" | "zsh" | "/bin/sh" | "/bin/bash" | "/bin/zsh"
        )));
        assert_eq!(qualify_user_line().last().copied(), Some(b'\n'));
    }

    #[test]
    fn launch_args_resume_appends_uuid() {
        let args = launch_args(
            ClaudeTurnMode::Build,
            "continue",
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        );
        assert!(has_pair(
            &args,
            "--resume",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        ));
        assert!(!args.iter().any(|arg| arg == "--no-session-persistence"));
    }

    #[test]
    fn protocol_fixture_covers_the_frozen_argv() {
        let inventory = protocol_inventory().expect("fixture");
        assert_eq!(inventory.claude_cli_version, CLAUDE_MINIMUM_VERSION);
        assert_eq!(inventory.transport, "ndjson");
        assert_eq!(inventory.output_format, "stream-json");
        assert!(inventory.requires_verbose);
        assert!(inventory.never_bare);
        assert_eq!(inventory.resume_flag, "--resume");
        assert_eq!(inventory.session_id_field, "session_id");
        assert_eq!(inventory.launch_argv_discuss_permission_mode, "plan");
        assert!(inventory
            .disallowed_tools
            .iter()
            .any(|tool| tool == "WebSearch"));
        assert!(inventory
            .launch_argv_build
            .iter()
            .any(|arg| arg == "--verbose"));
        assert!(inventory
            .launch_argv_build
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(!inventory
            .launch_argv_build
            .iter()
            .any(|arg| arg == "--bare"));
    }

    #[test]
    fn version_floor_accepts_2_1_229_and_newer() {
        assert_eq!(parse_claude_cli_version("2.1.229"), Some("2.1.229".into()));
        assert_eq!(
            parse_claude_cli_version("claude 2.1.229 (built)"),
            Some("2.1.229".into())
        );
        assert!(claude_version_meets_minimum("2.1.229"));
        assert!(claude_version_meets_minimum("2.2.0"));
        assert!(!claude_version_meets_minimum("2.1.228"));
        assert!(!claude_version_meets_minimum("not-a-version"));
    }

    #[test]
    fn result_success_requires_subtype_and_not_error() {
        assert!(claude_result_is_success(&serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false
        })));
        assert!(!claude_result_is_success(&serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": false
        })));
        assert!(!claude_result_is_success(&serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": true
        })));
    }
}
