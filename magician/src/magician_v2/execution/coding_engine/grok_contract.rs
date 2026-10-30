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
//! - `GROK_HOME` and `HOME` are Magician's own ([`magician_grok_home`],
//!   2026-10-14): a link to the owner's `auth.json` and only their model and
//!   sign-in tables, so the owner's plugins, MCP servers and hooks cannot
//!   make a session dirty. A restricted turn refuses to launch without it.
//!   Super god mode keeps the owner's home.
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
    /// The session advertised `search_tool` / `use_tool`, Grok's MCP gateway.
    /// It reaches MCP servers from `~/.grok`, installed plugins and xAI
    /// account connectors that Magician's empty `mcpServers` does not cover,
    /// and Grok has no flag to withhold it. Owner decision 2026-10-09: not a
    /// block — Ready, with what it reaches disclosed so the owner chooses.
    pub mcp_gateway: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokIsolationError {
    McpServersAdvertised,
    /// `search_tool` / `use_tool`: Grok's MCP gateway meta-tools. Grok
    /// advertises them in every 1.0.41 session and has no per-session flag to
    /// withhold them; they reach MCP servers from `~/.grok` config, installed
    /// plugins and xAI-managed connectors that Magician's empty `mcpServers`
    /// does not cover. Refused unless the session's own record shows the
    /// gateway contained (`gateway_contained`).
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
    attest_grok_isolation_roots([initialize, session], None, false)
}

/// `gateway_contained`: the probe session's own record
/// ([`grok_session_record_contained`]) shows no MCP server configured and no
/// hook run, so the `search_tool` / `use_tool` gateway has nothing outside
/// Magician to reach. Running in Magician's own Grok home is not enough on
/// its own: probes there still loaded the owner's `context7` server and a
/// Claude plugin hook through `$HOME`. Grok advertises the gateway in every
/// session since 1.0.41, so without that record it fails closed.
pub fn attest_grok_isolation_roots<'a, I>(
    roots: I,
    canary_mcp_name: Option<&str>,
    gateway_contained: bool,
) -> GrokIsolationVerdict
where
    I: IntoIterator<Item = &'a Value>,
{
    let roots: Vec<&Value> = roots.into_iter().collect();
    let mut mcp_empty = false;
    let mut tools_present = false;
    let mut tools = Vec::new();
    let mut mcp_gateway = false;

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
        if collection_advertised(root, HOOK_KEYS) || hook_execution_ran(root) {
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
                if !gateway_contained {
                    return GrokIsolationVerdict::Incompatible(
                        GrokIsolationError::McpGatewayAdvertised,
                    );
                }
                mcp_gateway = true;
                continue;
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
        digest: grok_attestation_digest(&tools, mcp_gateway),
        mcp_gateway,
    })
}

fn grok_attestation_digest(tools: &[String], mcp_gateway: bool) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", GROK_ATTESTATION_DOMAIN);
    absorb(&mut hasher, "mcp_count", "0");
    absorb(
        &mut hasher,
        "mcp_gateway",
        if mcp_gateway { "1" } else { "0" },
    );
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

/// A Grok `hook_execution` session update (`_x.ai/session/update`) that ran
/// at least one hook: a plugin or config hook executed in this session, so
/// it was not isolated whatever the advertised lists say.
pub fn hook_execution_ran(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            let kind = map
                .get("sessionUpdate")
                .or_else(|| map.get("session_update"))
                .and_then(Value::as_str);
            if kind == Some("hook_execution") && map.get("runs").is_some_and(collection_nonempty) {
                return true;
            }
            map.values().any(hook_execution_ran)
        },
        Value::Array(items) => items.iter().any(hook_execution_ran),
        _ => false,
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
    fn the_mcp_gateway_is_recorded_for_disclosure_not_refused() {
        // Grok 1.0.41 advertises both in every session. They are its MCP
        // gateway, not web search: once contained, isolated and flagged for
        // the picker.
        for tool in ["search_tool", "use_tool"] {
            let initialize = serde_json::json!({ "protocolVersion": 1 });
            let session = serde_json::json!({
                "sessionId": "s1",
                "mcpServers": [],
                "tools": ["read_file", tool],
            });
            match attest_grok_isolation_roots([&initialize, &session], None, true) {
                GrokIsolationVerdict::Isolated(attestation) => {
                    assert!(attestation.mcp_gateway, "{tool}")
                },
                other => panic!("expected isolated, got {other:?}"),
            }
        }
        let clean = isolated(
            serde_json::json!({ "protocolVersion": 1 }),
            serde_json::json!({ "sessionId": "s1", "mcpServers": [], "tools": ["read_file"] }),
        );
        assert!(!clean.mcp_gateway);
        // Loaded servers are still refused, gateway or not.
        let err = incompatible(
            serde_json::json!({ "protocolVersion": 1 }),
            serde_json::json!({
                "sessionId": "s1",
                "mcpServers": [{ "name": "github" }],
                "tools": ["read_file", "use_tool"],
            }),
        );
        assert_eq!(err, GrokIsolationError::McpServersAdvertised);
    }

    #[test]
    fn the_gateway_passes_only_in_magicians_own_grok_home() {
        let initialize = serde_json::json!({ "protocolVersion": 1 });
        let session = serde_json::json!({
            "sessionId": "s1",
            "mcpServers": [],
            "tools": ["read_file", "search_tool", "use_tool"],
        });
        assert!(matches!(
            attest_grok_isolation_roots([&initialize, &session], None, true),
            GrokIsolationVerdict::Isolated(_)
        ));
        assert_eq!(
            attest_grok_isolation_roots([&initialize, &session], None, false),
            GrokIsolationVerdict::Incompatible(GrokIsolationError::McpGatewayAdvertised)
        );
        // An advertised MCP server fails even in Magician's home.
        let dirty = serde_json::json!({
            "sessionId": "s1",
            "mcpServers": [{ "name": "context7" }],
            "tools": ["read_file", "search_tool"],
        });
        assert_eq!(
            attest_grok_isolation_roots([&initialize, &dirty], None, true),
            GrokIsolationVerdict::Incompatible(GrokIsolationError::McpServersAdvertised)
        );
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
    fn a_hook_execution_update_fails_isolation_as_hooks() {
        // Recorded from a probe session (Grok 1.0.52): a Claude plugin's
        // SessionStart hook ran although nothing advertised `hooks`.
        let frame: Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"_x.ai/session/update","params":{"sessionId":"01a1392f-81e5-7e10-bfcd-d63aa2bdf1d3","update":{"sessionUpdate":"hook_execution","event_name":"session_start","runs":[{"name":"plugin/codex/hooks:session_start[0].hooks[0]","status":{"status":"success","elapsed_ms":47}}]},"_meta":{"eventId":"01a1392f-81e5-7e10-bfcd-d63aa2bdf1d3-2","agentTimestampMs":1791960777329}}}"#,
        )
        .unwrap();
        assert!(hook_execution_ran(&frame));
        let session = serde_json::json!({
            "sessionId": "s1",
            "mcpServers": [],
            "tools": ["read_file", "bash"],
        });
        assert_eq!(
            attest_grok_isolation_roots(
                [
                    &serde_json::json!({ "protocolVersion": 1 }),
                    &session,
                    &frame
                ],
                None,
                true
            ),
            GrokIsolationVerdict::Incompatible(GrokIsolationError::HooksAdvertised)
        );
        // An update that ran no hook is not one.
        let idle = serde_json::json!({
            "params": { "update": { "sessionUpdate": "hook_execution", "runs": [] } }
        });
        assert!(!hook_execution_ran(&idle));
        assert!(matches!(
            attest_grok_isolation_roots(
                [
                    &serde_json::json!({ "protocolVersion": 1 }),
                    &session,
                    &idle
                ],
                None,
                false
            ),
            GrokIsolationVerdict::Isolated(_)
        ));
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
        let verdict = attest_grok_isolation_roots([&initialize, &session], Some(canary), false);
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
            false,
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

/// Magician's own Grok home (`GROK_HOME`), used for every restricted
/// Magician Grok session and qualification probe. The owner's Grok setup —
/// enabled plugins, marketplace installs, MCP servers, hooks — never reaches
/// it, so a plugin the owner turns on for their own Grok cannot make
/// Magician's Grok Incompatible (2026-10-14: `chrome-devtools` brought an MCP
/// server into every session). It holds a link to the owner's `auth.json`
/// (one sign-in, token refreshes shared) and a `config.toml` with only the
/// owner's model and sign-in tables ([`grok_home_config`]). Measured on Grok
/// 1.0.52: `GROK_HOME` moves plugins and MCP servers, and sign-in works
/// through the link. `HOME` must point here too ([`grok_contained_env`]):
/// Grok still reads Claude plugins and MCP servers under `$HOME`.
///
/// `None` when it cannot be prepared. A restricted turn then refuses to
/// launch, and the probe cannot attest a contained gateway (fail closed).
pub fn magician_grok_home() -> Option<std::path::PathBuf> {
    let owner = owner_grok_home()?;
    let home = magician_grok_home_dir();
    match prepare_grok_home(&home, &owner) {
        Ok(()) => Some(home),
        Err(error) => {
            tracing::warn!(%error, home = %home.display(), "[GROK] could not prepare Magician's Grok home");
            None
        },
    }
}

/// Where Magician's Grok home lives. [`magician_grok_home`] prepares it; the
/// harness box makes only its runtime state writable.
pub fn magician_grok_home_dir() -> std::path::PathBuf {
    crate::magician_v2::process_storage::runtime_root()
        .join("coding_engine")
        .join("grok_home")
}

/// The owner's own Grok home: `GROK_HOME` as Magician was started, else
/// `~/.grok`.
pub fn owner_grok_home() -> Option<std::path::PathBuf> {
    std::env::var_os("GROK_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|home| home.join(".grok")))
}

/// Subdirectories of Magician's Grok home a boxed Grok may write: its
/// sessions and logs. Never the home root, `config.toml`, hooks or plugins.
/// Writable subdirectories of Magician's Grok home: its sessions and logs,
/// and the per-user caches tools find under `$HOME` (contained launches move
/// `HOME` here), so pip, Go and the like still cache. Grok loads no config or
/// capability from any of them.
pub const GROK_HOME_WRITABLE_DIRS: &[&str] = &["sessions", "logs", ".cache", "Library/Caches"];

/// Runtime state and cache files in the root of Magician's Grok home a boxed
/// Grok may write (with suffixed siblings: temp files, SQLite `-wal`/`-shm`,
/// locks). None of them is config Grok loads capabilities from.
const GROK_HOME_STATE_FILES: &[&str] = &[
    ".metadata_version",
    "agent_id",
    "models_cache.json",
    "settings_cache.json",
    "worktrees.db",
    "active_sessions.json",
    "active_sessions.lock",
    "campaigns_state.json",
    "slash-mru.json",
    "tip_cursor.json",
    "managed_config.lock",
    ".config-init.lock",
    "auth.json.lock",
];

/// Entries of Magician's Grok home that make a session run code or load
/// capabilities: hooks, plugins, managed and required config, trust and
/// sandbox overrides. [`prepare_grok_home`] empties each before every launch,
/// so nothing a session wrote there reaches the next one or the probe.
const GROK_HOME_RESET_DIRS: &[&str] = &["hooks", "installed-plugins", "plugins"];
const GROK_HOME_RESET_FILES: &[&str] = &[
    "hooks-paths",
    "managed_config.toml",
    "requirements.toml",
    "sandbox.toml",
    "trusted_folders.toml",
];
/// Claude, Cursor and Codex sources Grok imports from `$HOME`, which is this
/// home for a contained launch: removed before every launch.
const GROK_HOME_REMOVED: &[&str] = &[".claude", ".claude.json", ".cursor", ".codex"];

/// SBPL regex for [`GROK_HOME_STATE_FILES`] under `home` (already resolved).
pub fn grok_home_writable_regex(home: &std::path::Path) -> String {
    let names: Vec<String> = GROK_HOME_STATE_FILES
        .iter()
        .map(|name| sbpl_regex_escape(name))
        .collect();
    format!(
        // SBPL's regex rejects a `-` inside a bracket anywhere but last
        // ("unterminated bracket expression"), and a rejected profile means
        // Grok cannot start at all (2026-10-14). `-` lives only in the
        // separator bracket; `.tmp-1` and `-wal` still match segment by segment.
        "^{}/({})([.-][A-Za-z0-9_]+)*$",
        sbpl_regex_escape(&home.to_string_lossy()),
        names.join("|")
    )
}

/// SBPL regex for the owner's `auth.json` and its lock under `owner`
/// (already resolved): token refreshes through Magician's link.
pub fn owner_auth_writable_regex(owner: &std::path::Path) -> String {
    format!(
        "^{}/auth\\.json(\\.lock)?$",
        sbpl_regex_escape(&owner.to_string_lossy())
    )
}

/// Escape the POSIX ERE metacharacters only: SBPL's regex does not promise
/// to accept escapes of ordinary characters such as `-`.
fn sbpl_regex_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if "\\.^$|?*+()[]{}".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Env for a contained Grok launch (VibeDev turn and qualification probe):
/// `GROK_HOME` and `HOME` both Magician's Grok home, so the Claude, Cursor
/// and Codex sources Grok imports (`~/.claude.json`, `~/.claude/plugins`, …)
/// resolve to nothing, and the compat switches off, as the plane's Grok
/// engine sets them. The owner's toolchains and git config stay reachable by
/// explicit variable, so a turn's `cargo`, `npm` or `git` still works.
pub fn grok_contained_env(home: &std::path::Path) -> Vec<(String, String)> {
    grok_contained_env_for(home, dirs::home_dir().as_deref())
}

fn grok_contained_env_for(
    home: &std::path::Path,
    owner_home: Option<&std::path::Path>,
) -> Vec<(String, String)> {
    let home = home.display().to_string();
    let mut env = vec![
        ("GROK_HOME".to_string(), home.clone()),
        ("HOME".to_string(), home),
        ("GROK_CLAUDE_MCPS_ENABLED".to_string(), "false".to_string()),
        ("GROK_CURSOR_MCPS_ENABLED".to_string(), "false".to_string()),
        ("GROK_CLAUDE_HOOKS_ENABLED".to_string(), "false".to_string()),
        ("GROK_CURSOR_HOOKS_ENABLED".to_string(), "false".to_string()),
    ];
    if let Some(owner_home) = owner_home {
        for (key, relative) in [
            ("CARGO_HOME", ".cargo"),
            ("RUSTUP_HOME", ".rustup"),
            ("GIT_CONFIG_GLOBAL", ".gitconfig"),
            ("npm_config_cache", ".npm"),
        ] {
            let path = owner_home.join(relative);
            if path.exists() {
                env.push((key.to_string(), path.display().to_string()));
            }
        }
    }
    env
}

/// The record Grok keeps of `session_id` in `grok_home`
/// (`sessions/<cwd>/<id>/`): its `events.jsonl` and `updates.jsonl`. `None`
/// when there is no such session or no `events.jsonl`.
/// Grok's own report, on the ACP stream, of the MCP servers this session
/// loaded: at least one `_x.ai/mcp/servers_updated` notification, and every
/// one of them lists no server. The probe session is cancelled before Grok
/// writes its `events.jsonl` (measured 2026-10-14: the file is empty), so
/// this is the evidence the qualification probe actually has.
pub fn grok_updates_show_no_mcp_servers(updates: &[Value]) -> bool {
    let reports: Vec<&Value> = updates
        .iter()
        .filter(|frame| {
            frame.get("method").and_then(Value::as_str) == Some("_x.ai/mcp/servers_updated")
        })
        .collect();
    !reports.is_empty()
        && reports.iter().all(|frame| {
            frame
                .get("params")
                .and_then(|params| params.get("mcpServers"))
                .and_then(Value::as_array)
                .is_some_and(|servers| servers.is_empty())
        })
}

pub fn grok_session_record(
    grok_home: &std::path::Path,
    session_id: &str,
) -> Option<(String, String)> {
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }
    for entry in std::fs::read_dir(grok_home.join("sessions"))
        .ok()?
        .flatten()
    {
        let dir = entry.path().join(session_id);
        if dir.is_dir() {
            let events = std::fs::read_to_string(dir.join("events.jsonl")).ok()?;
            let updates = std::fs::read_to_string(dir.join("updates.jsonl")).unwrap_or_default();
            return Some((events, updates));
        }
    }
    None
}

/// Whether a Grok session's own record shows its MCP gateway contained:
/// MCP config resolved to no server (or MCP init completed with none), and
/// no hook ran. Fail closed: no such event, any server configured, starting
/// or connected, or any hook event is not contained.
pub fn grok_session_record_contained(events: &str, updates: &str) -> bool {
    let mut no_servers = false;
    for line in events.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if hook_execution_ran(&event) {
            return false;
        }
        match event.get("type").and_then(Value::as_str) {
            Some("mcp_config_resolved") => match event.get("servers") {
                Some(Value::Array(servers)) if servers.is_empty() => no_servers = true,
                _ => return false,
            },
            Some("mcp_init_completed") => {
                match event.get("total_servers").and_then(Value::as_u64) {
                    Some(0) => no_servers = true,
                    _ => return false,
                }
            },
            Some(kind) if kind.starts_with("mcp_server") || kind.starts_with("hook") => {
                return false;
            },
            _ => {},
        }
    }
    let hook_ran = updates
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .any(|update| hook_execution_ran(&update));
    no_servers && !hook_ran
}

fn prepare_grok_home(home: &std::path::Path, owner: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
    }
    let auth_target = owner.join("auth.json");
    let auth_link = home.join("auth.json");
    if std::fs::read_link(&auth_link).ok().as_deref() != Some(auth_target.as_path()) {
        let _ = std::fs::remove_file(&auth_link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&auth_target, &auth_link)?;
    }
    let owner_config = std::fs::read_to_string(owner.join("config.toml")).unwrap_or_default();
    let config = grok_home_config(&owner_config);
    let config_path = home.join("config.toml");
    if std::fs::read_to_string(&config_path).ok().as_deref() != Some(config.as_str()) {
        std::fs::write(&config_path, config)?;
    }
    for dir in GROK_HOME_RESET_DIRS {
        reset_grok_home_entry(&home.join(dir), true, true)?;
    }
    for file in GROK_HOME_RESET_FILES {
        reset_grok_home_entry(&home.join(file), false, true)?;
    }
    for entry in GROK_HOME_REMOVED {
        reset_grok_home_entry(&home.join(entry), false, false)?;
    }
    for dir in GROK_HOME_WRITABLE_DIRS {
        std::fs::create_dir_all(home.join(dir))?;
    }
    Ok(())
}

/// Leave `path` an empty directory (`dir`) or empty file, or absent when not
/// `recreate`. Never follows a symlink: a link is removed, not its target.
fn reset_grok_home_entry(path: &std::path::Path, dir: bool, recreate: bool) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let already_empty = if dir {
                meta.is_dir() && std::fs::read_dir(path)?.next().is_none()
            } else {
                meta.is_file() && meta.len() == 0
            };
            if already_empty && recreate {
                return Ok(());
            }
            if meta.is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error),
    }
    if !recreate {
        return Ok(());
    }
    if dir {
        std::fs::create_dir(path)
    } else {
        std::fs::write(path, b"")
    }
}

/// Owner `config.toml` tables Magician's Grok home keeps: the model choice
/// and how Grok reaches and signs in to it (custom models, enterprise
/// endpoints). Never `plugins`, `mcp_servers`, `hooks`, `compat` or
/// anything else.
const GROK_HOME_COPIED_TABLES: &[&str] = &[
    "models",
    "model",
    "endpoints",
    "auth",
    "grok_com_config",
    "auth_provider",
];

/// Magician's Grok `config.toml`: the owner's model and sign-in tables as
/// written ([`GROK_HOME_COPIED_TABLES`]), then Magician's own tables: no
/// self-update, the official marketplace marked as already offered (so
/// nothing installs itself), and no imports from other tools' setups (Grok
/// reads MCP servers, hooks and skills from `~/.claude.json`, Cursor and
/// Codex by default — measured 2026-10-14: the owner's Claude Code
/// `context7` and `sequential-thinking` servers appeared in every Grok
/// session even in a clean `GROK_HOME`). Instruction files (`CLAUDE.md`,
/// rules) still load: they are project context, not capabilities.
pub fn grok_home_config(owner_config: &str) -> String {
    let mut config = String::from(
        "# Written by Magician: its own Grok home, without the owner's plugins,\n\
         # marketplace installs, MCP servers or hooks.\n",
    );
    let copied = owner_config_tables(owner_config);
    if !copied.is_empty() {
        config.push('\n');
        config.push_str(&copied);
    }
    config.push_str(
        "\n[cli]\nauto_update = false\n\n\
         [marketplace]\ndefault_skills_installs_purged = true\nofficial_marketplace_auto_installed = true\n\n\
         [compat.claude]\nmcps = false\nhooks = false\nskills = false\n\n\
         [compat.cursor]\nmcps = false\nhooks = false\nskills = false\n\n\
         [compat.codex]\nhooks = false\nskills = false\n\n\
         [managed_mcps]\ngateway_tools_enabled = false\n",
    );
    config
}

/// The owner's [`GROK_HOME_COPIED_TABLES`], line for line. A table starts at
/// a `[name…]` or `[[name…]]` header outside any multi-line string or array;
/// root keys and every other table are dropped. An owner config that does
/// not close its last string or array copies nothing.
fn owner_config_tables(owner_config: &str) -> String {
    let mut copied = String::new();
    let mut copying = false;
    let mut scan = TomlLineScan::default();
    for line in owner_config.lines() {
        if scan.at_top_level() {
            if let Some(name) = toml_table_name(line) {
                copying = GROK_HOME_COPIED_TABLES.contains(&name.as_str());
            }
        }
        scan.scan(line);
        if copying {
            copied.push_str(line);
            copied.push('\n');
        }
    }
    if scan.at_top_level() {
        copied
    } else {
        String::new()
    }
}

/// The first key of a table header line (`[ models ]`, `[model.custom]`,
/// `[[hooks.SessionStart]]`, `["auth"] # comment`).
fn toml_table_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let inner = match trimmed.strip_prefix("[[") {
        Some(rest) => rest.split_once("]]")?.0,
        None => trimmed.strip_prefix('[')?.split_once(']')?.0,
    };
    let first = inner
        .split('.')
        .next()?
        .trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'');
    (!first.is_empty()).then(|| first.to_string())
}

/// Enough TOML lexing to know whether a line starts at the top level: open
/// arrays and inline tables, and multi-line strings.
#[derive(Default)]
struct TomlLineScan {
    depth: i64,
    multiline: Option<&'static [u8]>,
}

impl TomlLineScan {
    fn at_top_level(&self) -> bool {
        self.depth == 0 && self.multiline.is_none()
    }

    fn scan(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            if let Some(close) = self.multiline {
                match bytes[at..]
                    .windows(close.len())
                    .position(|window| window == close)
                {
                    Some(found) => {
                        at += found + close.len();
                        self.multiline = None;
                        continue;
                    },
                    None => return,
                }
            }
            match bytes[at] {
                b'#' => return,
                quote @ (b'"' | b'\'') => {
                    let triple: &'static [u8] = if quote == b'"' { b"\"\"\"" } else { b"'''" };
                    if bytes[at..].starts_with(triple) {
                        self.multiline = Some(triple);
                        at += triple.len();
                        continue;
                    }
                    at += 1;
                    while at < bytes.len() && bytes[at] != quote {
                        if quote == b'"' && bytes[at] == b'\\' {
                            at += 1;
                        }
                        at += 1;
                    }
                },
                b'[' | b'{' => self.depth += 1,
                b']' | b'}' => self.depth -= 1,
                _ => {},
            }
            at += 1;
        }
    }
}

#[cfg(test)]
mod grok_home_tests {
    use super::*;

    #[test]
    fn magicians_grok_home_keeps_only_the_owner_models() {
        let owner = "[cli]\nauto_update = true\n\n[plugins]\nenabled = [\n    \"chrome-devtools\",\n]\n\n[models]\ndefault = \"grok-4.6\"\ndefault_reasoning_effort = \"xhigh\"\n\n[mcp_servers.x]\ncommand = \"npx\"\n";
        let config = grok_home_config(owner);
        assert!(config.contains("auto_update = false"));
        assert!(config.contains("official_marketplace_auto_installed = true"));
        for vendor in ["claude", "cursor"] {
            assert!(
                config.contains(&format!(
                    "[compat.{vendor}]\nmcps = false\nhooks = false\nskills = false\n"
                )),
                "{config}"
            );
        }
        assert!(
            config.contains("[compat.codex]\nhooks = false\nskills = false\n"),
            "{config}"
        );
        assert!(
            config.contains("[managed_mcps]\ngateway_tools_enabled = false\n"),
            "{config}"
        );
        assert!(config
            .contains("[models]\ndefault = \"grok-4.6\"\ndefault_reasoning_effort = \"xhigh\"\n"));
        assert!(
            !config.contains("chrome-devtools")
                && !config.contains("mcp_servers")
                && !config.contains("npx"),
            "{config}"
        );
        assert!(!grok_home_config("").contains("[models]"));
    }

    #[test]
    fn magicians_grok_home_keeps_the_owner_model_and_sign_in_tables() {
        let owner = "model = \"root-key-dropped\"\n\n\
            [ models ]\ndefault = \"custom-1\"\nlist = [\n  [\"a\", \"b\"],\n  [\"c\"],\n]\n\n\
            [model.custom-1]\nbase_url = \"https://grok.example.corp/v1\"\n\n\
            [endpoints] # enterprise\napi = \"https://grok.example.corp\"\n\n\
            [auth]\nmethod = \"sso\"\n\n\
            [auth_provider.okta]\nissuer = \"https://example.okta.com\"\n\n\
            [grok_com_config]\nregion = \"eu\"\n\n\
            [plugins]\nenabled = [\"chrome-devtools\"]\n\n\
            [[hooks.SessionStart]]\ncommand = \"curl evil\"\n\n\
            [mcp_servers.x]\ncommand = \"npx\"\n\n\
            [compat.claude]\nmcps = true\n";
        let config = grok_home_config(owner);
        for kept in [
            "[ models ]\ndefault = \"custom-1\"\nlist = [\n  [\"a\", \"b\"],\n  [\"c\"],\n]\n",
            "[model.custom-1]\nbase_url = \"https://grok.example.corp/v1\"\n",
            "[endpoints] # enterprise\napi = \"https://grok.example.corp\"\n",
            "[auth]\nmethod = \"sso\"\n",
            "[auth_provider.okta]\nissuer = \"https://example.okta.com\"\n",
            "[grok_com_config]\nregion = \"eu\"\n",
        ] {
            assert!(config.contains(kept), "missing {kept:?} in {config}");
        }
        for dropped in [
            "root-key-dropped",
            "chrome-devtools",
            "hooks.SessionStart",
            "curl evil",
            "mcp_servers",
            "npx",
            "mcps = true",
        ] {
            assert!(
                !config.contains(dropped),
                "{dropped:?} leaked into {config}"
            );
        }
        // Magician's own tables come last, once.
        assert_eq!(config.matches("[compat.claude]").count(), 1, "{config}");
        assert!(
            config.find("[cli]").unwrap() > config.find("[grok_com_config]").unwrap(),
            "{config}"
        );
    }

    #[test]
    fn a_table_header_inside_a_multiline_value_does_not_switch_tables() {
        let owner = "[models]\nnote = \"\"\"\n[plugins]\n\"\"\"\ndefault = \"grok-4.6\"\n\n[plugins]\nenabled = [\"x\"]\n";
        let config = grok_home_config(owner);
        assert!(config.contains("default = \"grok-4.6\""), "{config}");
        assert!(!config.contains("enabled = [\"x\"]"), "{config}");
        // `[models] # x` is the models table too.
        assert!(grok_home_config("[models] # x\ndefault = \"m\"\n").contains("default = \"m\""));
        // An owner config that never closes its array copies nothing.
        let broken = grok_home_config("[models]\nlist = [\n  \"a\",\n");
        assert!(!broken.contains("list = ["), "{broken}");
        assert!(broken.contains("[cli]\nauto_update = false"), "{broken}");
    }

    #[test]
    fn every_prepare_empties_hooks_plugins_and_managed_config() {
        let owner = tempfile::tempdir().unwrap();
        std::fs::write(owner.path().join("auth.json"), "{}").unwrap();
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("grok_home");
        prepare_grok_home(&home, owner.path()).unwrap();
        // What a session could plant between launches.
        std::fs::write(home.join("hooks").join("evil.json"), "{}").unwrap();
        std::fs::create_dir_all(home.join("installed-plugins").join("evil")).unwrap();
        std::fs::create_dir_all(home.join("plugins")).unwrap();
        std::fs::write(home.join("plugins").join("evil.toml"), "x").unwrap();
        std::fs::write(home.join("hooks-paths"), "/tmp/evil").unwrap();
        for file in [
            "managed_config.toml",
            "requirements.toml",
            "sandbox.toml",
            "trusted_folders.toml",
        ] {
            std::fs::write(
                home.join(file),
                "[[hooks.SessionStart]]\ncommand = \"evil\"\n",
            )
            .unwrap();
        }
        std::fs::create_dir_all(home.join(".claude").join("plugins")).unwrap();
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        std::fs::write(home.join("sessions").join("kept.txt"), "x").unwrap();
        prepare_grok_home(&home, owner.path()).unwrap();
        for dir in ["hooks", "installed-plugins", "plugins"] {
            assert_eq!(
                std::fs::read_dir(home.join(dir)).unwrap().count(),
                0,
                "{dir}"
            );
        }
        for file in [
            "hooks-paths",
            "managed_config.toml",
            "requirements.toml",
            "sandbox.toml",
            "trusted_folders.toml",
        ] {
            assert_eq!(std::fs::read(home.join(file)).unwrap(), b"", "{file}");
        }
        assert!(!home.join(".claude").exists() && !home.join(".claude.json").exists());
        assert!(
            home.join("sessions").join("kept.txt").is_file(),
            "sessions are runtime state"
        );
        assert!(home.join("logs").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_link_is_removed_not_followed() {
        let owner = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("grok_home");
        prepare_grok_home(&home, owner.path()).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "x").unwrap();
        std::fs::remove_dir(home.join("hooks")).unwrap();
        std::os::unix::fs::symlink(&outside, home.join("hooks")).unwrap();
        prepare_grok_home(&home, owner.path()).unwrap();
        assert!(outside.join("keep.txt").is_file());
        assert!(std::fs::symlink_metadata(home.join("hooks"))
            .unwrap()
            .is_dir());
    }

    #[test]
    fn the_box_may_write_grok_state_files_but_not_config_hooks_or_plugins() {
        let pattern = grok_home_writable_regex(std::path::Path::new("/r/grok-home"));
        let matcher = regex::Regex::new(&pattern).unwrap();
        for allowed in [
            "/r/grok-home/models_cache.json",
            "/r/grok-home/models_cache.json.tmp1",
            "/r/grok-home/worktrees.db-wal",
            "/r/grok-home/agent_id",
            "/r/grok-home/auth.json.lock",
        ] {
            assert!(matcher.is_match(allowed), "{allowed} vs {pattern}");
        }
        for denied in [
            "/r/grok-home/config.toml",
            "/r/grok-home/managed_config.toml",
            "/r/grok-home/requirements.toml",
            "/r/grok-home/hooks-paths",
            "/r/grok-home/hooks/x.json",
            "/r/grok-home/installed-plugins/x",
            "/r/grok-home/auth.json",
            "/r/grok-home/models_cache.json/../config.toml",
        ] {
            assert!(!matcher.is_match(denied), "{denied} vs {pattern}");
        }
        assert!(!GROK_HOME_WRITABLE_DIRS.iter().any(|dir| [
            "hooks",
            "plugins",
            "installed-plugins",
            ""
        ]
        .contains(dir)));
        let owner = regex::Regex::new(&owner_auth_writable_regex(std::path::Path::new(
            "/Users/o/.grok",
        )))
        .unwrap();
        assert!(
            owner.is_match("/Users/o/.grok/auth.json")
                && owner.is_match("/Users/o/.grok/auth.json.lock")
        );
        assert!(
            !owner.is_match("/Users/o/.grok/config.toml")
                && !owner.is_match("/Users/o/.grok/hooks/x.json")
        );
    }

    #[test]
    fn a_contained_launch_moves_home_and_turns_compat_imports_off() {
        let owner_home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(owner_home.path().join(".rustup")).unwrap();
        let env = grok_contained_env_for(
            std::path::Path::new("/r/grok_home"),
            Some(owner_home.path()),
        );
        let get = |key: &str| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(get("GROK_HOME").as_deref(), Some("/r/grok_home"));
        assert_eq!(get("HOME").as_deref(), Some("/r/grok_home"));
        for flag in [
            "GROK_CLAUDE_MCPS_ENABLED",
            "GROK_CURSOR_MCPS_ENABLED",
            "GROK_CLAUDE_HOOKS_ENABLED",
            "GROK_CURSOR_HOOKS_ENABLED",
        ] {
            assert_eq!(get(flag).as_deref(), Some("false"), "{flag}");
        }
        assert_eq!(
            get("RUSTUP_HOME"),
            Some(owner_home.path().join(".rustup").display().to_string())
        );
        assert_eq!(get("CARGO_HOME"), None, "only toolchain dirs that exist");
    }

    #[test]
    fn only_a_session_record_with_no_servers_and_no_hooks_is_contained() {
        // Recorded (Grok 1.0.52, Magician's home before HOME moved): the
        // owner's context7 server and a Claude plugin hook.
        let dirty_events = r#"{"ts":"2026-10-14T06:52:57.271Z","type":"mcp_config_resolved","servers":[{"name":"context7","transport":"http","source":"local"}],"disabled":[]}
{"ts":"2026-10-14T06:52:57.286Z","type":"mcp_server_starting","server_name":"context7","transport":"http","target":"https://mcp.context7.com/mcp","timeout_sec":30}
{"ts":"2026-10-14T06:52:58.296Z","type":"mcp_init_completed","total_servers":1,"succeeded":1,"failed":0,"auth_required":0,"total_tools":2,"duration_ms":1010,"is_reinit":false}
"#;
        let hook_updates = r#"{"timestamp":1791960777,"method":"_x.ai/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"hook_execution","event_name":"session_start","runs":[{"name":"plugin/codex/hooks:session_start[0].hooks[0]","status":{"status":"success","elapsed_ms":47}}]}}}
"#;
        let clean_events = r#"{"ts":"t","type":"mcp_config_resolved","servers":[],"disabled":[]}
{"ts":"t","type":"mcp_init_completed","total_servers":0,"succeeded":0,"failed":0}
"#;
        assert!(!grok_session_record_contained(dirty_events, ""));
        assert!(grok_session_record_contained(clean_events, ""));
        assert!(!grok_session_record_contained(clean_events, hook_updates));
        assert!(
            !grok_session_record_contained("", ""),
            "no record is not contained"
        );
        assert!(
            !grok_session_record_contained(r#"{"type":"phase_changed"}"#, ""),
            "no MCP evidence"
        );
        assert!(!grok_session_record_contained(
            r#"{"type":"mcp_init_completed","total_servers":0}
{"type":"mcp_server_connected","server_name":"x"}"#,
            ""
        ));
    }

    #[test]
    fn a_session_record_is_found_by_id_under_any_cwd() {
        let root = tempfile::tempdir().unwrap();
        let session = root
            .path()
            .join("sessions")
            .join("%2Ftmp%2Fq")
            .join("01abc-def");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("events.jsonl"), "{\"type\":\"x\"}\n").unwrap();
        let (events, updates) = grok_session_record(root.path(), "01abc-def").unwrap();
        assert!(events.contains("\"x\""));
        assert!(updates.is_empty());
        assert!(grok_session_record(root.path(), "missing").is_none());
        assert!(grok_session_record(root.path(), "../01abc-def").is_none());
        assert!(grok_session_record(root.path(), "").is_none());
    }

    #[test]
    fn magicians_grok_home_links_the_owner_sign_in() {
        let owner = tempfile::tempdir().unwrap();
        std::fs::write(owner.path().join("auth.json"), "{}").unwrap();
        std::fs::write(
            owner.path().join("config.toml"),
            "[plugins]\nenabled = [\"x\"]\n",
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("grok_home");
        prepare_grok_home(&home, owner.path()).unwrap();
        prepare_grok_home(&home, owner.path()).unwrap();
        assert_eq!(
            std::fs::read_link(home.join("auth.json")).unwrap(),
            owner.path().join("auth.json")
        );
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(
            !config.contains("[plugins]") && !config.contains("[\"x\"]"),
            "{config}"
        );
    }

    #[test]
    fn grok_reports_no_mcp_servers_only_with_an_empty_servers_update() {
        let empty = serde_json::json!({"jsonrpc":"2.0","method":"_x.ai/mcp/servers_updated","params":{"mcpServers":[]}});
        let loaded = serde_json::json!({"jsonrpc":"2.0","method":"_x.ai/mcp/servers_updated","params":{"mcpServers":[{"name":"context7","source":"local"}]}});
        let other = serde_json::json!({"jsonrpc":"2.0","method":"_x.ai/mcp_initialized","params":{"mcpToolCount":2}});
        assert!(grok_updates_show_no_mcp_servers(&[
            empty.clone(),
            other.clone()
        ]));
        assert!(
            !grok_updates_show_no_mcp_servers(&[other.clone()]),
            "no report is not evidence"
        );
        assert!(
            !grok_updates_show_no_mcp_servers(&[empty, loaded]),
            "any loaded server fails"
        );
    }

    /// The real launcher must accept every writable regex a harness box
    /// carries: a profile `sandbox-exec` rejects means the engine never
    /// starts ("unterminated bracket expression", 2026-10-14).
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_exec_accepts_the_grok_box_profile() {
        if !crate::magician_v2::execution::shell_box::available() {
            return;
        }
        // Under the home directory: temp itself is writable inside the box.
        let user_home = dirs::home_dir().unwrap();
        let home = tempfile::tempdir_in(&user_home).unwrap();
        let owner = tempfile::tempdir_in(&user_home).unwrap();
        let resolved = |path: &std::path::Path| path.canonicalize().unwrap();
        let harness = crate::magician_v2::execution::shell_box::HarnessBox {
            engine_binary: std::path::PathBuf::from("/usr/bin/true"),
            writable_regex: vec![
                grok_home_writable_regex(&resolved(home.path())),
                owner_auth_writable_regex(&resolved(owner.path())),
            ],
            ..Default::default()
        };
        let state = resolved(home.path()).join("agent_id.tmp-1");
        let config = resolved(home.path()).join("config.toml");
        let boxed = crate::magician_v2::execution::shell_box::wrap_harness(
            std::ffi::OsStr::new("/bin/sh"),
            &[
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(format!(
                    "touch '{}' && ! touch '{}' 2>/dev/null",
                    state.display(),
                    config.display()
                )),
            ],
            &harness,
        )
        .expect("boxed");
        let status = std::process::Command::new(&boxed.program)
            .args(&boxed.args)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "the profile compiles, state files are writable, config is not"
        );
        assert!(state.exists() && !config.exists());
    }
}
