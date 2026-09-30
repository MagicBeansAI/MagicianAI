//! Phase 1 + 3: launch policy, ACP allowlist, and isolation attestation
//! for local `grok agent stdio`.
//!
//! Frozen here:
//!
//! - Client identity is Magician. Never impersonate the Grok TUI.
//! - No ACP `fs` or `terminal` client capabilities. Grok uses its own tools
//!   inside the shadow workspace.
//! - Magician never sends `x.ai/*` methods. Unknown agent→client requests
//!   fail closed.
//! - Launch pins `--no-leader` so a shared leader process cannot widen the
//!   sandbox, and `--always-approve` so Grok does not elicit Magician as a
//!   permission host.
//! - `GROK_HOME` is not relocated. Auth may reuse `~/.grok`; Magician does
//!   not invent a fake home.
//! - Phase 3 attests initialize + session/new + a bounded `session/update`
//!   drain. Ready requires positive evidence: `mcpServers` present and
//!   empty, and a tool list with no web search / MCP-style names. Missing
//!   lists stay unattested (`unqualified`). Present dirty lists are
//!   `incompatible`.
//!
//! Plan: `docs/plans/2026-08-22-vibedev-grok-build-cli-support-plan.md`.

use serde::Deserialize;
use serde_json::Value;

use super::factory::GrokTurnMode;

pub const GROK_ACP_QUALIFIED_VERSION: &str = "1.0.5";
pub const GROK_ACP_MINIMUM_VERSION: &str = "1.0.5";
pub const GROK_ACP_PROTOCOL_VERSION: u32 = 1;
pub const GROK_CLIENT_NAME: &str = "magician";
pub const GROK_CLIENT_TITLE: &str = "Magician";
pub const GROK_ACP_PROTOCOL_FIXTURE: &str = include_str!("fixtures/grok_acp_protocol_1.0.5.json");

/// Stable ACP methods this adapter may send. Same-engine follow-up uses
/// `session/load`; a missing resume id uses `session/new`.
pub const GROK_ACP_STABLE_METHODS: &[&str] = &[
    "initialize",
    "session/new",
    "session/prompt",
    "session/cancel",
    "session/load",
];

/// Anything under this prefix is SpaceXAI-specific and must not be sent.
pub const GROK_ACP_PROHIBITED_PREFIX: &str = "x.ai/";

#[derive(Debug, Deserialize)]
pub struct ProtocolInventory {
    pub grok_cli_version: String,
    pub protocol_version: u32,
    pub client_methods: Vec<String>,
    pub prohibited_prefixes: Vec<String>,
    pub client_capabilities: Value,
    pub server_notifications: Vec<String>,
    pub session_update_kinds: Vec<String>,
}

pub fn protocol_inventory() -> Result<ProtocolInventory, String> {
    serde_json::from_str(GROK_ACP_PROTOCOL_FIXTURE)
        .map_err(|err| format!("grok protocol fixture: {err}"))
}

pub fn magician_grok_client_info() -> Value {
    serde_json::json!({
        "name": GROK_CLIENT_NAME,
        "title": GROK_CLIENT_TITLE,
        "version": env!("CARGO_PKG_VERSION"),
    })
}

pub fn initialize_params() -> Value {
    serde_json::json!({
        "protocolVersion": GROK_ACP_PROTOCOL_VERSION,
        "clientInfo": magician_grok_client_info(),
        "clientCapabilities": {},
    })
}

/// `session/new` for a VibeDev shadow or a disposable qualify cwd.
/// Magician always sends an empty MCP list; it cannot disable `~/.grok`
/// MCP via config overlay, so Phase 3 attests the result instead.
/// Missing advertised lists stay unattested, never Ready.
pub fn session_new_params(cwd: impl AsRef<str>) -> Value {
    serde_json::json!({
        "cwd": cwd.as_ref(),
        "mcpServers": [],
        "_meta": { "yoloMode": true },
    })
}

pub fn session_cancel_params(session_id: &str) -> Value {
    serde_json::json!({ "sessionId": session_id })
}

/// ACP `session/load` for a same-engine resume. Magician still sends an
/// empty MCP list; Phase 3 attestation of a live turn is unchanged.
pub fn session_load_params(session_id: &str, cwd: impl AsRef<str>) -> Value {
    serde_json::json!({
        "sessionId": session_id,
        "cwd": cwd.as_ref(),
        "mcpServers": [],
        "_meta": { "yoloMode": true },
    })
}

pub fn grok_session_id_from(result: &Value) -> Option<String> {
    result
        .get("sessionId")
        .or_else(|| result.get("session_id"))
        .or_else(|| result.pointer("/session/id"))
        .or_else(|| result.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Arguments after the Grok executable. Discovery chooses the binary path.
/// This never sets `GROK_HOME` and never passes `--leader`.
/// Grok 1.0.40 moved `--sandbox`, `--disable-web-search` and
/// `--no-subagents` to top-level `grok` options: after `agent` they are
/// rejected (`unexpected argument '--sandbox'`), the child exits, and every
/// probe and turn failed as bare I/O. It also removed `--no-auto-update`
/// outright; the CLI only prints an update notice. Only `--no-leader`,
/// `--always-approve` and `stdio` remain `agent` options.
pub fn agent_stdio_args(mode: GrokTurnMode) -> Vec<String> {
    vec![
        "--sandbox".to_string(),
        mode.sandbox_flag().to_string(),
        "--disable-web-search".to_string(),
        "--no-subagents".to_string(),
        "agent".to_string(),
        "--no-leader".to_string(),
        "--always-approve".to_string(),
        "stdio".to_string(),
    ]
}

pub fn may_send_method(method: &str) -> bool {
    !is_prohibited_method(method) && GROK_ACP_STABLE_METHODS.contains(&method)
}

pub fn is_prohibited_method(method: &str) -> bool {
    method.starts_with(GROK_ACP_PROHIBITED_PREFIX)
}

pub fn grok_version_meets_minimum(version: &str) -> bool {
    match (
        semver::Version::parse(version),
        semver::Version::parse(GROK_ACP_MINIMUM_VERSION),
    ) {
        (Ok(got), Ok(min)) => got >= min,
        _ => false,
    }
}

/// First `x.y.z` token in `grok --version` output.
pub fn parse_grok_cli_version(output: &str) -> Option<String> {
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

const GROK_ATTESTATION_DOMAIN: &str = "magician.coding_engine.grok_attestation.v1";

const MCP_SERVER_KEYS: &[&str] = &["mcpServers", "mcp_servers", "mcpServer", "mcp_server"];
const HOOK_KEYS: &[&str] = &["hooks", "hook"];
const PLUGIN_KEYS: &[&str] = &["plugins", "plugin", "installedPlugins", "installed_plugins"];
const TOOL_LIST_KEYS: &[&str] = &[
    "tools",
    "availableTools",
    "available_tools",
    "availableCommands",
    "available_commands",
    "advertisedTools",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokAttestation {
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokIsolationError {
    McpServersAdvertised,
    /// `search_tool` / `use_tool`: Grok's MCP gateway meta-tools. Grok
    /// advertises them in every 1.0.41 session and has no per-session flag to
    /// withhold them; they reach MCP servers from `~/.grok` config, installed
    /// plugins and xAI-managed connectors that Magician's empty `mcpServers`
    /// does not cover.
    McpGatewayAdvertised,
    WebSearchAdvertised,
    HooksAdvertised,
    PluginsAdvertised,
}

/// Isolation is fail-closed. Missing MCP/tool lists are not Ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokIsolationVerdict {
    Isolated(GrokAttestation),
    Unattested,
    Incompatible(GrokIsolationError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectionEvidence {
    Absent,
    Empty,
    Nonempty,
}

impl std::fmt::Display for GrokIsolationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::McpServersAdvertised => {
                write!(f, "ACP session advertised MCP servers")
            },
            Self::McpGatewayAdvertised => write!(
                f,
                "ACP session advertised the MCP gateway tools search_tool / use_tool"
            ),
            Self::WebSearchAdvertised => write!(
                f,
                "ACP session advertised web search after --disable-web-search"
            ),
            Self::HooksAdvertised => write!(f, "ACP session advertised hooks"),
            Self::PluginsAdvertised => write!(f, "ACP session advertised plugins"),
        }
    }
}

impl std::error::Error for GrokIsolationError {}

/// Fail closed unless initialize, session/new, or drained `session/update`
/// positively evidence empty MCP and a clean tool list. Missing lists are
/// unattested, not isolated. Present dirty lists are incompatible.
/// `mcpCapabilities` on the agent is not a loaded server list.
pub fn attest_grok_isolation(initialize: &Value, session: &Value) -> GrokIsolationVerdict {
    attest_grok_isolation_roots([initialize, session], None)
}

pub fn attest_grok_isolation_roots<'a, I>(
    roots: I,
    canary_mcp_name: Option<&str>,
) -> GrokIsolationVerdict
where
    I: IntoIterator<Item = &'a Value>,
{
    let roots: Vec<&Value> = roots.into_iter().collect();
    let mut mcp_empty = false;
    let mut tools_present = false;
    let mut tools = Vec::new();

    for root in &roots {
        match collection_evidence(root, MCP_SERVER_KEYS) {
            CollectionEvidence::Nonempty => {
                return GrokIsolationVerdict::Incompatible(
                    GrokIsolationError::McpServersAdvertised,
                );
            },
            CollectionEvidence::Empty => mcp_empty = true,
            CollectionEvidence::Absent => {},
        }
        if collection_advertised(root, HOOK_KEYS) {
            return GrokIsolationVerdict::Incompatible(GrokIsolationError::HooksAdvertised);
        }
        if collection_advertised(root, PLUGIN_KEYS) {
            return GrokIsolationVerdict::Incompatible(GrokIsolationError::PluginsAdvertised);
        }
        if let Some(canary) = canary_mcp_name {
            if canary_appears(root, canary) {
                return GrokIsolationVerdict::Incompatible(
                    GrokIsolationError::McpServersAdvertised,
                );
            }
        }
        if tool_list_present(root) {
            tools_present = true;
            tools.extend(advertised_tools(root));
        }
        for name in advertised_tools(root)
            .into_iter()
            .chain(tool_call_tool_names(root))
        {
            if is_mcp_gateway_tool(&name) {
                return GrokIsolationVerdict::Incompatible(
                    GrokIsolationError::McpGatewayAdvertised,
                );
            }
            if is_web_search_tool(&name) {
                return GrokIsolationVerdict::Incompatible(GrokIsolationError::WebSearchAdvertised);
            }
            if is_mcp_style_tool(&name) {
                return GrokIsolationVerdict::Incompatible(
                    GrokIsolationError::McpServersAdvertised,
                );
            }
        }
    }

    if !mcp_empty || !tools_present {
        return GrokIsolationVerdict::Unattested;
    }

    tools.sort();
    tools.dedup();
    GrokIsolationVerdict::Isolated(GrokAttestation {
        digest: grok_attestation_digest(&tools),
    })
}

fn grok_attestation_digest(tools: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", GROK_ATTESTATION_DOMAIN);
    absorb(&mut hasher, "mcp_count", "0");
    absorb(&mut hasher, "web_search", "disabled");
    absorb(&mut hasher, "tool_count", &tools.len().to_string());
    for tool in tools {
        absorb(&mut hasher, "tool", tool);
    }
    hasher.finalize().to_hex().to_string()
}

fn absorb(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn collection_advertised(value: &Value, keys: &[&str]) -> bool {
    collection_evidence(value, keys) == CollectionEvidence::Nonempty
}

fn collection_evidence(value: &Value, keys: &[&str]) -> CollectionEvidence {
    let mut empty = false;
    let mut nonempty = false;
    visit_objects(value, &mut |key, child| {
        if keys.iter().any(|wanted| *wanted == key) {
            if is_empty_collection(child) {
                empty = true;
            } else if collection_nonempty(child) {
                nonempty = true;
            }
        }
    });
    if nonempty {
        CollectionEvidence::Nonempty
    } else if empty {
        CollectionEvidence::Empty
    } else {
        CollectionEvidence::Absent
    }
}

fn is_empty_collection(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().all(Value::is_null),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn tool_list_present(value: &Value) -> bool {
    let mut found = false;
    visit_objects(value, &mut |key, child| {
        if TOOL_LIST_KEYS.iter().any(|wanted| *wanted == key)
            && (child.is_array() || child.is_object())
        {
            found = true;
        }
    });
    found
}

fn advertised_tools(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    visit_objects(value, &mut |key, child| {
        if TOOL_LIST_KEYS.iter().any(|wanted| *wanted == key) {
            push_names(child, &mut names);
        }
    });
    names
}

fn tool_call_tool_names(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    collect_tool_call_names(value, &mut names);
    names
}

fn collect_tool_call_names(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let kind = map
                .get("sessionUpdate")
                .or_else(|| map.get("session_update"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if kind == "tool_call" || kind == "tool_call_update" {
                push_names(value, names);
            }
            for child in map.values() {
                collect_tool_call_names(child, names);
            }
        },
        Value::Array(items) => {
            for item in items {
                collect_tool_call_names(item, names);
            }
        },
        _ => {},
    }
}

fn advertised_mcp_names(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    visit_objects(value, &mut |key, child| {
        if MCP_SERVER_KEYS.iter().any(|wanted| *wanted == key) {
            push_mcp_names(child, &mut names);
        }
    });
    names
}

fn push_mcp_names(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                push_mcp_names(item, names);
            }
        },
        Value::Object(map) => {
            let mut had_identity = false;
            for key in ["name", "id", "title"] {
                if let Some(name) = map.get(key).and_then(Value::as_str) {
                    if !name.is_empty() {
                        names.push(name.to_string());
                        had_identity = true;
                    }
                }
            }
            if !had_identity {
                for (id, spec) in map {
                    if !id.is_empty()
                        && spec.get("enabled").and_then(Value::as_bool).unwrap_or(true)
                    {
                        names.push(id.clone());
                    }
                    push_mcp_names(spec, names);
                }
            }
        },
        Value::String(name) if !name.is_empty() => names.push(name.clone()),
        _ => {},
    }
}

fn canary_appears(value: &Value, canary: &str) -> bool {
    advertised_mcp_names(value)
        .into_iter()
        .chain(advertised_tools(value))
        .chain(tool_call_tool_names(value))
        .any(|name| name == canary || name.contains(canary))
}

fn visit_objects(value: &Value, visit: &mut impl FnMut(&str, &Value)) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                visit(key, child);
                visit_objects(child, visit);
            }
        },
        Value::Array(items) => {
            for item in items {
                visit_objects(item, visit);
            }
        },
        _ => {},
    }
}

fn collection_nonempty(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(|item| !item.is_null()),
        Value::Object(map) => map.iter().any(|(id, spec)| {
            !id.is_empty() && spec.get("enabled").and_then(Value::as_bool).unwrap_or(true)
        }),
        Value::String(text) => !text.trim().is_empty(),
        Value::Bool(true) => true,
        _ => false,
    }
}

fn push_names(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                push_names(item, names);
            }
        },
        Value::Object(map) => {
            for key in ["name", "id", "title", "command", "tool", "toolName"] {
                if let Some(name) = map.get(key).and_then(Value::as_str) {
                    if !name.is_empty() {
                        names.push(name.to_string());
                    }
                }
            }
        },
        Value::String(name) if !name.is_empty() => names.push(name.clone()),
        _ => {},
    }
}

fn normalize_tool_name(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace(' ', "_")
}

/// Grok's MCP gateway: `search_tool` finds tools across enabled MCP servers,
/// `use_tool` calls them (Grok user guide, "MCP servers"). `search_tool` used
/// to be listed here as web search, which mislabelled the block; Grok's real
/// web tools (`web_search`, `web_fetch`) are gone under --disable-web-search.
fn is_mcp_gateway_tool(name: &str) -> bool {
    matches!(
        normalize_tool_name(name).as_str(),
        "search_tool" | "searchtool" | "use_tool" | "usetool"
    )
}

fn is_web_search_tool(name: &str) -> bool {
    let normalized = normalize_tool_name(name);
    normalized == "web_search"
        || normalized == "websearch"
        || normalized.ends_with("_web_search")
        || normalized.starts_with("web_search_")
}

fn is_mcp_style_tool(name: &str) -> bool {
    let normalized = normalize_tool_name(name);
    normalized.contains("__")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::Value;

    fn has_pair(args: &[String], left: &str, right: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == left && pair[1] == right)
    }

    #[test]
    fn launch_args_pin_stdio_and_never_enable_leader() {
        let build = agent_stdio_args(GrokTurnMode::Build);
        // Isolation flags are top-level `grok` options (1.0.40): they must
        // precede the `agent` subcommand, which rejects them.
        let agent = build.iter().position(|arg| arg == "agent").expect("agent");
        let position = |flag: &str| build.iter().position(|arg| arg == flag);
        for flag in ["--sandbox", "--disable-web-search", "--no-subagents"] {
            assert!(
                position(flag).is_some_and(|at| at < agent),
                "{flag} must precede agent: {build:?}"
            );
        }
        for flag in ["--no-leader", "--always-approve"] {
            assert!(
                position(flag).is_some_and(|at| at > agent),
                "{flag} is an agent option: {build:?}"
            );
        }
        assert_eq!(build.last().map(String::as_str), Some("stdio"));
        assert!(
            !build.iter().any(|arg| arg == "--no-auto-update"),
            "removed in 1.0.40"
        );
        assert!(!build.iter().any(|arg| arg == "--leader"));
        assert!(has_pair(&build, "--sandbox", "workspace"));
        assert!(!build.iter().any(|arg| arg.contains("GROK_HOME")));

        let discuss = agent_stdio_args(GrokTurnMode::Discuss);
        assert!(has_pair(&discuss, "--sandbox", "read-only"));
        assert!(!discuss.iter().any(|arg| arg == "--leader"));
    }

    #[test]
    fn allowlist_excludes_xai_extensions() {
        for method in GROK_ACP_STABLE_METHODS {
            assert!(may_send_method(method), "{method}");
        }
        assert!(!may_send_method("x.ai/fs/read_file"));
        assert!(!may_send_method("x.ai/session/fork"));
        assert!(is_prohibited_method("x.ai/auth/get_url"));
        assert!(!may_send_method("session/set_mode"));
    }

    #[test]
    fn initialize_advertises_no_fs_or_terminal() {
        let params = initialize_params();
        assert_eq!(params["protocolVersion"], GROK_ACP_PROTOCOL_VERSION);
        assert_eq!(params["clientCapabilities"], serde_json::json!({}));
        let encoded = params.to_string();
        assert!(!encoded.contains("\"fs\""), "{encoded}");
        assert!(!encoded.contains("terminal"), "{encoded}");
        let info = magician_grok_client_info();
        assert_eq!(info["name"], GROK_CLIENT_NAME);
        assert_eq!(info["title"], GROK_CLIENT_TITLE);
        let text = info.to_string();
        assert!(!text.to_ascii_lowercase().contains("zed"), "{text}");
        assert!(!text.contains("grok-tui"), "{text}");
    }

    #[test]
    fn session_new_params_send_empty_mcp_servers() {
        let params = session_new_params("/tmp/shadow");
        assert_eq!(params["cwd"], "/tmp/shadow");
        assert_eq!(params["mcpServers"], serde_json::json!([]));
        assert_eq!(params["_meta"]["yoloMode"], true);
    }

    #[test]
    fn session_load_params_keep_empty_mcp_servers() {
        let params = session_load_params("sess-secret", "/tmp/shadow");
        assert_eq!(params["sessionId"], "sess-secret");
        assert_eq!(params["cwd"], "/tmp/shadow");
        assert_eq!(params["mcpServers"], serde_json::json!([]));
        assert_eq!(params["_meta"]["yoloMode"], true);
    }

    #[test]
    fn no_leader_is_required_when_sandbox_is_not_off() {
        for mode in [GrokTurnMode::Build, GrokTurnMode::Discuss] {
            assert_ne!(mode.sandbox_flag(), "off");
            let args = agent_stdio_args(mode);
            assert!(
                args.iter().any(|arg| arg == "--no-leader"),
                "{mode:?} argv missing --no-leader: {args:?}"
            );
            assert!(!args.iter().any(|arg| arg == "--leader"));
        }
    }

    fn incompatible(init: Value, session: Value) -> GrokIsolationError {
        match attest_grok_isolation(&init, &session) {
            GrokIsolationVerdict::Incompatible(error) => error,
            other => panic!("expected incompatible, got {other:?}"),
        }
    }

    fn isolated(init: Value, session: Value) -> GrokAttestation {
        match attest_grok_isolation(&init, &session) {
            GrokIsolationVerdict::Isolated(attestation) => attestation,
            other => panic!("expected isolated, got {other:?}"),
        }
    }

    #[test]
    fn fake_initialize_with_mcp_servers_fails_isolation() {
        let err = incompatible(
            serde_json::json!({
                "protocolVersion": 1,
                "mcpServers": [{ "name": "github" }]
            }),
            serde_json::json!({ "sessionId": "s1", "mcpServers": [] }),
        );
        assert_eq!(err, GrokIsolationError::McpServersAdvertised);
        assert!(!err.to_string().contains("github"));
    }

    #[test]
    fn empty_mcp_and_expected_tools_attest() {
        let attestation = isolated(
            serde_json::json!({ "protocolVersion": 1 }),
            serde_json::json!({
                "sessionId": "s1",
                "mcpServers": [],
                "tools": ["read_file", "bash", "grep_search", "list_dir", "search_replace"],
            }),
        );
        assert!(!attestation.digest.is_empty());
        assert!(!attestation.digest.contains("s1"));
    }

    #[test]
    fn missing_mcp_servers_is_unattested() {
        let verdict = attest_grok_isolation(
            &serde_json::json!({ "protocolVersion": 1 }),
            &serde_json::json!({
                "sessionId": "s1",
                "tools": ["read_file", "bash"],
            }),
        );
        assert_eq!(verdict, GrokIsolationVerdict::Unattested);
    }

    #[test]
    fn missing_tool_list_is_unattested() {
        let verdict = attest_grok_isolation(
            &serde_json::json!({ "protocolVersion": 1 }),
            &serde_json::json!({ "sessionId": "s1", "mcpServers": [] }),
        );
        assert_eq!(verdict, GrokIsolationVerdict::Unattested);
    }

    #[test]
    fn advertised_web_search_tool_fails_isolation() {
        let err = incompatible(
            serde_json::json!({ "protocolVersion": 1 }),
            serde_json::json!({
                "sessionId": "s1",
                "mcpServers": [],
                "availableTools": ["read_file", "web_search"],
            }),
        );
        assert_eq!(err, GrokIsolationError::WebSearchAdvertised);
    }

    #[test]
    fn advertised_mcp_gateway_tools_fail_isolation_as_the_gateway() {
        // Grok 1.0.41 advertises both in every session; they are its MCP
        // gateway, not web search, and the reason must say so.
        for tool in ["search_tool", "use_tool"] {
            let err = incompatible(
                serde_json::json!({ "protocolVersion": 1 }),
                serde_json::json!({
                    "sessionId": "s1",
                    "mcpServers": [],
                    "tools": ["read_file", tool],
                }),
            );
            assert_eq!(err, GrokIsolationError::McpGatewayAdvertised, "{tool}");
        }
        assert!(GrokIsolationError::McpGatewayAdvertised
            .to_string()
            .contains("MCP gateway"));
    }

    #[test]
    fn mcp_style_or_use_tool_fails_isolation() {
        let dotted = incompatible(
            serde_json::json!({ "protocolVersion": 1 }),
            serde_json::json!({
                "sessionId": "s1",
                "mcpServers": [],
                "tools": ["read_file", "github__list_issues"],
            }),
        );
        assert_eq!(dotted, GrokIsolationError::McpServersAdvertised);
    }

    #[test]
    fn advertised_hooks_or_plugins_fail_isolation() {
        let hooks = incompatible(
            serde_json::json!({ "hooks": [{ "event": "PreToolUse" }] }),
            serde_json::json!({ "sessionId": "s1" }),
        );
        assert_eq!(hooks, GrokIsolationError::HooksAdvertised);
        let plugins = incompatible(
            serde_json::json!({ "protocolVersion": 1 }),
            serde_json::json!({ "plugins": { "market": { "enabled": true } } }),
        );
        assert_eq!(plugins, GrokIsolationError::PluginsAdvertised);
    }

    #[test]
    fn agent_mcp_capabilities_are_not_loaded_servers() {
        isolated(
            serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": { "mcpCapabilities": { "http": true, "sse": true } }
            }),
            serde_json::json!({
                "sessionId": "s1",
                "mcpServers": [],
                "tools": ["read_file", "bash"],
            }),
        );
    }

    #[test]
    fn canary_server_name_is_incompatible() {
        let canary = "magician-grok-qualify-canary-test";
        let initialize = serde_json::json!({ "protocolVersion": 1 });
        let session = serde_json::json!({
            "sessionId": "s1",
            "mcpServers": [{ "name": canary }],
            "tools": ["read_file"],
        });
        let verdict = attest_grok_isolation_roots([&initialize, &session], Some(canary));
        assert_eq!(
            verdict,
            GrokIsolationVerdict::Incompatible(GrokIsolationError::McpServersAdvertised)
        );
        assert!(!format!("{verdict:?}").contains("github"));
    }

    #[test]
    fn session_update_lists_can_attest() {
        let update = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": ["read_file", "bash", "grep"],
                    "mcpServers": []
                }
            }
        });
        match attest_grok_isolation_roots(
            [
                &serde_json::json!({ "protocolVersion": 1 }),
                &serde_json::json!({ "sessionId": "s1" }),
                &update,
            ],
            None,
        ) {
            GrokIsolationVerdict::Isolated(attestation) => {
                assert!(!attestation.digest.is_empty());
            },
            other => panic!("expected isolated from updates, got {other:?}"),
        }
    }

    #[test]
    fn protocol_fixture_covers_the_allowlist() {
        let inventory = protocol_inventory().expect("fixture");
        assert_eq!(inventory.grok_cli_version, GROK_ACP_QUALIFIED_VERSION);
        assert_eq!(inventory.protocol_version, GROK_ACP_PROTOCOL_VERSION);
        assert_eq!(inventory.client_capabilities, serde_json::json!({}));
        for method in GROK_ACP_STABLE_METHODS {
            assert!(
                inventory.client_methods.iter().any(|item| item == method),
                "{method} missing from the 1.0.5 inventory"
            );
        }
        assert!(inventory
            .prohibited_prefixes
            .iter()
            .any(|prefix| prefix == GROK_ACP_PROHIBITED_PREFIX));
        assert!(inventory
            .server_notifications
            .iter()
            .any(|item| item == "session/update"));
        for kind in [
            "agent_message_chunk",
            "agent_thought_chunk",
            "tool_call",
            "tool_call_update",
            "plan",
        ] {
            assert!(
                inventory
                    .session_update_kinds
                    .iter()
                    .any(|item| item == kind),
                "{kind}"
            );
        }
    }

    #[test]
    fn version_floor_accepts_1_0_5_and_newer() {
        assert_eq!(parse_grok_cli_version("1.0.5"), Some("1.0.5".into()));
        assert_eq!(
            parse_grok_cli_version("grok 1.0.5 (build)"),
            Some("1.0.5".into())
        );
        assert!(grok_version_meets_minimum("1.0.5"));
        assert!(grok_version_meets_minimum("1.1.0"));
        assert!(!grok_version_meets_minimum("1.0.4"));
        assert!(!grok_version_meets_minimum("not-a-version"));
        assert_eq!(parse_grok_cli_version("no version here"), None);
    }
}
