//! Slice 0b: launch policy, `config/read` attestation, and the 0.147.0 protocol
//! inventory for a local `codex app-server`.
//!
//! Decisions frozen here:
//!
//! - Reuse the user's `CODEX_HOME` so ChatGPT/OpenAI auth is not copied and no
//!   alternate Codex home is created.
//! - Narrow execution-bearing config with process-local `--disable` / `-c`
//!   overlays, then attest the effective `config/read` result. Overlay is not
//!   trusted on its own: a required MCP server or a default-on feature that
//!   survives the overlay keeps the profile unavailable.
//! - The coding profile never starts a Code Mode host. `features.code_mode_host`
//!   is stable and default-on in 0.147.0; the coding profile always passes
//!   `--disable code_mode_host` and never `--code-mode-host`. These are the
//!   coding profile's statements: the plane profile keeps the code-mode pair
//!   on, because it is current Codex's only path to MCP tools, and narrows
//!   its sandbox overlay to read-only instead (see [`CodexLaunchProfile`]).
//!
//! The generated 3.6 MiB schema bundle is not checked in. The compact
//! inventory under `fixtures/` is the durable evidence.
//!
//! Archived plan: `docs/archive/plans/2026-08-08-vibedev-codex-app-server-support-plan.md`.

use serde::Deserialize;
use serde_json::Value;

pub const CODEX_APP_SERVER_QUALIFIED_VERSION: &str = "0.147.0";
pub const CODEX_APP_SERVER_MINIMUM_VERSION: &str = "0.147.0";
/// Evidence-backed denylist. Empty until a shipped 0.147+ build is proven bad.
pub const CODEX_APP_SERVER_KNOWN_BAD_VERSIONS: &[&str] = &[];
pub const CODEX_APP_SERVER_LISTEN: &str = "stdio://";
pub const CODEX_APP_SERVER_PROTOCOL_FIXTURE: &str =
    include_str!("fixtures/codex_app_server_protocol_0.147.0.json");

/// V1 never opts into `initialize.params.capabilities.experimentalApi`.
pub const CODEX_APP_SERVER_EXPERIMENTAL_API_ENABLED: bool = false;

/// Parent `thread/archive` / `thread/delete` success does not settle Magician-
/// owned forks. Codex may cascade; Magician still reconciles each exact id.
pub const THREAD_DESCENDANT_SETTLEMENT_IMPLIED: bool = false;

/// Stable methods this adapter may send, plus the `initialized` notification
/// it must emit after `initialize`.
pub const CODEX_APP_SERVER_STABLE_METHODS: &[&str] = &[
    "initialize",
    "initialized",
    "account/read",
    "model/list",
    "config/read",
    "thread/start",
    "thread/resume",
    "thread/read",
    "thread/fork",
    "thread/archive",
    "thread/unarchive",
    "thread/delete",
    "turn/start",
    "turn/steer",
    "turn/interrupt",
];

/// Methods that must never be sent. Several run outside the Codex sandbox
/// (`thread/shellCommand`) or mutate host config/history.
pub const CODEX_APP_SERVER_PROHIBITED_METHODS: &[&str] = &[
    "thread/shellCommand",
    "thread/list",
    "thread/inject_items",
    "command/exec",
    "command/exec/write",
    "command/exec/resize",
    "command/exec/terminate",
    "process/spawn",
    "process/writeStdin",
    "process/resizePty",
    "process/kill",
    "config/value/write",
    "config/batchWrite",
    "config/mcpServer/reload",
    "marketplace/add",
    "marketplace/remove",
    "marketplace/upgrade",
    "plugin/install",
    "plugin/uninstall",
    "skills/config/write",
    "skills/extraRoots/set",
    "experimentalFeature/enablement/set",
    "externalAgentConfig/import",
    "fs/readFile",
    "fs/writeFile",
    "fs/createDirectory",
    "fs/remove",
    "fs/copy",
    "review/start",
    "mcpServer/tool/call",
    "mcpServer/oauth/login",
];

/// Experimental or unstable surfaces that stay off in V1 even if a newer
/// Codex build exposes them.
pub const CODEX_APP_SERVER_EXPERIMENTAL_OFF_IN_V1: &[&str] = &[
    "thread/turns/list",
    "thread/items/list",
    "thread/backgroundTerminals/clean",
    "thread/backgroundTerminals/list",
    "thread/backgroundTerminals/terminate",
    "process/spawn",
    "environment/info",
    "collaborationMode/list",
];

/// Features that default on in 0.147.0 and would widen a VibeDev execution.
/// Missing after overlay is treated as still on.
pub const CODEX_FEATURES_MUST_BE_OFF: &[&str] = &[
    "code_mode_host",
    "code_mode",
    "apps",
    "hooks",
    "plugins",
    "remote_plugin",
    "plugin_sharing",
    "multi_agent",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "image_generation",
    "tool_suggest",
    "skill_search",
    "skill_mcp_dependency_install",
    "recommended_plugins",
    "enable_mcp_apps",
    "memories",
];

/// The code-mode pair inside [`CODEX_FEATURES_MUST_BE_OFF`]. The coding
/// contract turns it off with the rest; the plane harness keeps it on because
/// current Codex reaches MCP tools only through its code-mode host.
pub const CODEX_CODE_MODE_FEATURES: &[&str] = &["code_mode_host", "code_mode"];

/// Which feature posture a spawned `codex app-server` launches with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodexLaunchProfile {
    /// The coding contract: every default-on feature off, no code-mode host.
    #[default]
    Coding,
    /// The plane harness: the coding contract minus the code-mode pair, which
    /// is the only path to MCP tools in current Codex. The read-only sandbox
    /// and the shell/unified_exec disables still hold the posture; the plane
    /// door is the approval authority for every governed call.
    PlaneHarness,
}

/// The features a launch profile turns off, in contract order.
pub fn features_off_for(profile: CodexLaunchProfile) -> impl Iterator<Item = &'static str> {
    CODEX_FEATURES_MUST_BE_OFF
        .iter()
        .copied()
        .filter(move |feature| {
            profile == CodexLaunchProfile::Coding || !CODEX_CODE_MODE_FEATURES.contains(feature)
        })
}

/// The coding profile's process-local `-c` overlays, in contract order. The
/// plane profile rides the same list with one substitution: its sandbox
/// overlay is the read-only mode its isolated `config.toml` already pins, so
/// argv can never widen what the config narrowed (see
/// [`config_overlays_for`]).
const CONFIG_OVERLAYS: &[(&str, &str)] = &[
    ("approval_policy", "never"),
    ("sandbox_mode", "workspace-write"),
    ("web_search", "disabled"),
    ("agents.enabled", "false"),
    ("analytics.enabled", "false"),
    ("allow_login_shell", "false"),
];

const SANDBOX_MODE_OVERLAY_KEY: &str = "sandbox_mode";
const PLANE_HARNESS_SANDBOX_MODE: &str = "read-only";

/// The `-c` overlays a launch profile puts on argv, in contract order. The
/// coding profile is [`CONFIG_OVERLAYS`] exactly; the plane profile differs
/// only in the sandbox overlay, which it narrows to read-only.
pub fn config_overlays_for(
    profile: CodexLaunchProfile,
) -> impl Iterator<Item = (&'static str, &'static str)> {
    CONFIG_OVERLAYS.iter().map(move |(key, value)| {
        if profile == CodexLaunchProfile::PlaneHarness && *key == SANDBOX_MODE_OVERLAY_KEY {
            (*key, PLANE_HARNESS_SANDBOX_MODE)
        } else {
            (*key, *value)
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexCodeModePolicy {
    /// Do not start a local host and do not connect to a remote one.
    Disabled,
}

/// The coding profile's code-mode policy. The plane profile is the one
/// exception: it keeps the code-mode pair on because that host is current
/// Codex's only path to MCP tools, so a plane spawn runs model-authored code
/// under Codex's own confinement (see [`CodexLaunchProfile::PlaneHarness`]).
pub const CODE_MODE_POLICY: CodexCodeModePolicy = CodexCodeModePolicy::Disabled;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexServerRequestClass {
    /// ChatGPT token refresh. The only server request V1 may handle.
    AuthRefresh,
    /// Any approval, elicitation, or dynamic tool call. Fail closed.
    Consequential,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexAttestationError {
    MissingApprovalPolicy,
    ApprovalPolicyNotNever { got: String },
    MissingSandboxMode,
    DangerSandbox,
    WebSearchNotDisabled { got: String },
    FeatureStillOn { feature: String },
    McpServerEnabled { id: String },
    McpServerRequired { id: String },
    AgentsEnabled,
}

impl std::fmt::Display for CodexAttestationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingApprovalPolicy => write!(f, "effective config is missing approval_policy"),
            Self::ApprovalPolicyNotNever { got } => {
                write!(f, "approval_policy must be never, got {got}")
            },
            Self::MissingSandboxMode => write!(f, "effective config is missing sandbox_mode"),
            Self::DangerSandbox => write!(f, "sandbox_mode danger-full-access is not allowed"),
            Self::WebSearchNotDisabled { got } => {
                write!(f, "web_search must be disabled, got {got}")
            },
            Self::FeatureStillOn { feature } => {
                write!(f, "feature `{feature}` is still on after overlay")
            },
            Self::McpServerEnabled { id } => {
                write!(f, "MCP server `{id}` is still enabled")
            },
            Self::McpServerRequired { id } => {
                write!(f, "MCP server `{id}` is marked required")
            },
            Self::AgentsEnabled => write!(f, "agents.enabled is still true"),
        }
    }
}

impl std::error::Error for CodexAttestationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAttestation {
    pub digest: String,
    pub approval_policy: String,
    pub sandbox_mode: String,
    pub web_search: String,
    pub disabled_features: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProtocolInventory {
    pub codex_cli_version: String,
    pub experimental_api: bool,
    pub client_methods: Vec<String>,
    pub client_notifications: Vec<String>,
    pub server_notifications: Vec<String>,
    pub server_requests: Vec<String>,
}

pub fn protocol_inventory() -> Result<ProtocolInventory, String> {
    serde_json::from_str(CODEX_APP_SERVER_PROTOCOL_FIXTURE)
        .map_err(|err| format!("codex protocol fixture: {err}"))
}

/// Arguments after the Codex executable. The binary path is chosen by
/// discovery; this never sets `CODEX_HOME` or `--code-mode-host`.
/// Process-local `-c` overlays that turn off MCP servers still enabled in the
/// user's `CODEX_HOME` config after the first `config/read`.
pub fn mcp_disable_overlays(config: &Value) -> Vec<String> {
    let Some(servers) = config.get("mcp_servers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut extra = Vec::new();
    for (id, spec) in servers {
        let enabled = spec.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        if !enabled {
            continue;
        }
        if !mcp_server_id_is_overlay_safe(id) {
            continue;
        }
        extra.push("-c".to_string());
        extra.push(format!("mcp_servers.{id}.enabled=false"));
    }
    extra
}

fn mcp_server_id_is_overlay_safe(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub fn app_server_args() -> Vec<String> {
    app_server_args_for(CodexLaunchProfile::Coding)
}

/// Launch arguments for one profile. The coding profile is
/// [`app_server_args`] exactly; the plane profile differs only in the
/// code-mode pair it leaves on and the sandbox overlay it narrows to
/// read-only.
pub fn app_server_args_for(profile: CodexLaunchProfile) -> Vec<String> {
    let mut args = vec![
        "app-server".to_string(),
        "--listen".to_string(),
        CODEX_APP_SERVER_LISTEN.to_string(),
    ];
    let mut features: Vec<&str> = features_off_for(profile).collect();
    features.sort_unstable();
    features.dedup();
    for feature in features {
        args.push("--disable".to_string());
        args.push(feature.to_string());
    }
    for (key, value) in config_overlays_for(profile) {
        args.push("-c".to_string());
        args.push(format!("{key}={value}"));
    }
    args
}

pub fn launch_disables_local_code_mode_host() -> bool {
    let args = app_server_args();
    args.windows(2)
        .any(|pair| pair[0] == "--disable" && pair[1] == "code_mode_host")
        && !args.iter().any(|arg| arg == "--code-mode-host")
}

pub fn classify_server_request(method: &str) -> CodexServerRequestClass {
    match method {
        "account/chatgptAuthTokens/refresh" => CodexServerRequestClass::AuthRefresh,
        "applyPatchApproval"
        | "execCommandApproval"
        | "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval"
        | "item/tool/call"
        | "item/tool/requestUserInput"
        | "mcpServer/elicitation/request"
        | "attestation/generate" => CodexServerRequestClass::Consequential,
        _ => CodexServerRequestClass::Unknown,
    }
}

pub fn may_send_method(method: &str) -> bool {
    CODEX_APP_SERVER_STABLE_METHODS.contains(&method)
        && !CODEX_APP_SERVER_PROHIBITED_METHODS.contains(&method)
}

/// Compare `config/read.result.config` against the execution-bearing allowlist.
/// The raw config is not retained; only the digest and typed failures leave.
pub fn attest_effective_config(config: &Value) -> Result<CodexAttestation, CodexAttestationError> {
    let approval = string_at(config, &["approval_policy"])
        .ok_or(CodexAttestationError::MissingApprovalPolicy)?;
    if approval != "never" {
        return Err(CodexAttestationError::ApprovalPolicyNotNever { got: approval });
    }
    let sandbox =
        string_at(config, &["sandbox_mode"]).ok_or(CodexAttestationError::MissingSandboxMode)?;
    if sandbox == "danger-full-access" {
        return Err(CodexAttestationError::DangerSandbox);
    }
    let web_search = string_at(config, &["web_search"]).unwrap_or_else(|| "cached".to_string());
    if web_search != "disabled" {
        return Err(CodexAttestationError::WebSearchNotDisabled { got: web_search });
    }
    if bool_at(config, &["agents", "enabled"]) == Some(true) {
        return Err(CodexAttestationError::AgentsEnabled);
    }

    let mut disabled_features = Vec::new();
    for feature in CODEX_FEATURES_MUST_BE_OFF {
        if feature_on(config, feature) {
            return Err(CodexAttestationError::FeatureStillOn {
                feature: (*feature).to_string(),
            });
        }
        disabled_features.push((*feature).to_string());
    }

    if let Some(servers) = config.get("mcp_servers").and_then(Value::as_object) {
        for (id, spec) in servers {
            let enabled = spec.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            let required = spec
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if enabled {
                return Err(CodexAttestationError::McpServerEnabled { id: id.clone() });
            }
            if required {
                return Err(CodexAttestationError::McpServerRequired { id: id.clone() });
            }
        }
    }

    Ok(CodexAttestation {
        digest: attestation_digest(config, &approval, &sandbox, &web_search, &disabled_features),
        approval_policy: approval,
        sandbox_mode: sandbox,
        web_search,
        disabled_features,
    })
}

fn attestation_digest(
    config: &Value,
    approval: &str,
    sandbox: &str,
    web_search: &str,
    disabled_features: &[String],
) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(
        &mut hasher,
        "domain",
        "magician.coding_engine.codex_attestation.v1",
    );
    absorb(&mut hasher, "approval_policy", approval);
    absorb(&mut hasher, "sandbox_mode", sandbox);
    absorb(&mut hasher, "web_search", web_search);
    absorb(
        &mut hasher,
        "feature_count",
        &disabled_features.len().to_string(),
    );
    for feature in disabled_features {
        absorb(&mut hasher, "feature_off", feature);
    }
    if let Some(servers) = config.get("mcp_servers").and_then(Value::as_object) {
        let mut ids: Vec<_> = servers.keys().cloned().collect();
        ids.sort();
        absorb(&mut hasher, "mcp_count", &ids.len().to_string());
        for id in ids {
            absorb(&mut hasher, "mcp_disabled", &id);
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn absorb(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn lookup<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    lookup(value, path)?.as_str().map(str::to_string)
}

fn bool_at(value: &Value, path: &[&str]) -> Option<bool> {
    match lookup(value, path)? {
        Value::Bool(flag) => Some(*flag),
        Value::Object(map) => map.get("enabled").and_then(Value::as_bool),
        _ => None,
    }
}

fn feature_on(config: &Value, feature: &str) -> bool {
    match bool_at(config, &["features", feature]) {
        Some(flag) => flag,
        None => default_on(feature),
    }
}

fn default_on(feature: &str) -> bool {
    !matches!(
        feature,
        "code_mode" | "memories" | "recommended_plugins" | "enable_mcp_apps"
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;

    fn narrowed_config() -> Value {
        let mut features = serde_json::Map::new();
        for feature in CODEX_FEATURES_MUST_BE_OFF {
            features.insert((*feature).to_string(), Value::Bool(false));
        }
        json!({
            "approval_policy": "never",
            "sandbox_mode": "workspace-write",
            "web_search": "disabled",
            "agents": { "enabled": false },
            "features": features,
            "mcp_servers": {
                "notes": { "enabled": false, "required": false }
            }
        })
    }

    fn has_pair(args: &[String], left: &str, right: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == left && pair[1] == right)
    }

    #[test]
    fn launch_args_reuse_stdio_and_disable_code_mode_host() {
        let args = app_server_args();
        assert_eq!(args[0], "app-server");
        assert!(has_pair(&args, "--listen", "stdio://"));
        assert!(has_pair(&args, "--disable", "code_mode_host"));
        assert!(!args.iter().any(|arg| arg == "--code-mode-host"));
        assert!(!args.iter().any(|arg| arg.contains("CODEX_HOME")));
        assert_eq!(CODE_MODE_POLICY, CodexCodeModePolicy::Disabled);
        assert!(launch_disables_local_code_mode_host());
    }

    #[test]
    fn plane_harness_launch_args_leave_only_the_code_mode_pair_on() {
        let coding = app_server_args();
        let plane = app_server_args_for(CodexLaunchProfile::PlaneHarness);
        for feature in CODEX_CODE_MODE_FEATURES {
            assert!(has_pair(&coding, "--disable", feature), "{feature}");
            assert!(!has_pair(&plane, "--disable", feature), "{feature}");
        }
        for feature in CODEX_FEATURES_MUST_BE_OFF {
            if CODEX_CODE_MODE_FEATURES.contains(feature) {
                continue;
            }
            assert!(has_pair(&plane, "--disable", feature), "{feature}");
        }
        // Every overlay rides both argvs, except the sandbox overlay: the
        // plane profile narrows it to read-only (matching its isolated
        // config) while the coding profile keeps workspace-write.
        for (key, value) in CONFIG_OVERLAYS {
            if *key == SANDBOX_MODE_OVERLAY_KEY {
                continue;
            }
            assert!(has_pair(&plane, "-c", &format!("{key}={value}")), "{key}");
            assert!(has_pair(&coding, "-c", &format!("{key}={value}")), "{key}");
        }
        assert!(has_pair(&plane, "-c", "sandbox_mode=read-only"));
        assert!(!has_pair(&plane, "-c", "sandbox_mode=workspace-write"));
        assert!(has_pair(&coding, "-c", "sandbox_mode=workspace-write"));
        assert!(!has_pair(&coding, "-c", "sandbox_mode=read-only"));
        assert_eq!(
            plane
                .iter()
                .filter(|arg| arg.starts_with("sandbox_mode="))
                .count(),
            1,
            "the plane argv carries exactly one sandbox overlay"
        );
        assert!(!plane.iter().any(|arg| arg == "--code-mode-host"));
        let plane_off: Vec<&str> = features_off_for(CodexLaunchProfile::PlaneHarness).collect();
        let coding_off: Vec<&str> = features_off_for(CodexLaunchProfile::Coding).collect();
        assert_eq!(coding_off, CODEX_FEATURES_MUST_BE_OFF.to_vec());
        assert_eq!(
            plane_off.len() + CODEX_CODE_MODE_FEATURES.len(),
            coding_off.len()
        );
        assert!(plane_off.iter().all(|feature| coding_off.contains(feature)));
    }

    #[test]
    fn launch_args_pin_never_approval_and_disabled_web_search() {
        let args = app_server_args();
        assert!(has_pair(&args, "-c", "approval_policy=never"));
        assert!(has_pair(&args, "-c", "web_search=disabled"));
        assert!(has_pair(&args, "-c", "sandbox_mode=workspace-write"));
    }

    #[test]
    fn mcp_overlays_disable_only_enabled_servers() {
        let config = json!({
            "mcp_servers": {
                "spokenly": { "enabled": false },
                "node_repl": { "enabled": true },
                "openaiDeveloperDocs": { "enabled": true }
            }
        });
        let overlays = mcp_disable_overlays(&config);
        assert!(has_pair(
            &overlays,
            "-c",
            "mcp_servers.node_repl.enabled=false"
        ));
        assert!(has_pair(
            &overlays,
            "-c",
            "mcp_servers.openaiDeveloperDocs.enabled=false"
        ));
        assert!(!overlays.iter().any(|arg| arg.contains("spokenly")));
    }

    #[test]
    fn attestation_accepts_a_narrowed_config_and_is_stable() {
        let first = attest_effective_config(&narrowed_config()).expect("ok");
        let second = attest_effective_config(&narrowed_config()).expect("ok");
        assert_eq!(first.digest, second.digest);
        assert_eq!(first.approval_policy, "never");
        assert!(first.disabled_features.contains(&"code_mode_host".into()));
    }

    #[test]
    fn attestation_rejects_default_on_code_mode_host() {
        let mut config = narrowed_config();
        config["features"]["code_mode_host"] = json!(true);
        let err = attest_effective_config(&config).expect_err("host on");
        assert!(matches!(
            err,
            CodexAttestationError::FeatureStillOn { feature } if feature == "code_mode_host"
        ));
    }

    #[test]
    fn attestation_treats_missing_code_mode_host_as_still_on() {
        let mut config = narrowed_config();
        config["features"]
            .as_object_mut()
            .expect("features")
            .remove("code_mode_host");
        let err = attest_effective_config(&config).expect_err("missing host");
        assert!(matches!(
            err,
            CodexAttestationError::FeatureStillOn { feature } if feature == "code_mode_host"
        ));
    }

    #[test]
    fn attestation_rejects_enabled_or_required_mcp_and_danger_sandbox() {
        let mut enabled = narrowed_config();
        enabled["mcp_servers"]["notes"]["enabled"] = json!(true);
        assert!(matches!(
            attest_effective_config(&enabled),
            Err(CodexAttestationError::McpServerEnabled { .. })
        ));

        let mut required = narrowed_config();
        required["mcp_servers"]["notes"]["required"] = json!(true);
        assert!(matches!(
            attest_effective_config(&required),
            Err(CodexAttestationError::McpServerRequired { .. })
        ));

        let mut danger = narrowed_config();
        danger["sandbox_mode"] = json!("danger-full-access");
        assert!(matches!(
            attest_effective_config(&danger),
            Err(CodexAttestationError::DangerSandbox)
        ));

        let mut search = narrowed_config();
        search["web_search"] = json!("live");
        assert!(matches!(
            attest_effective_config(&search),
            Err(CodexAttestationError::WebSearchNotDisabled { .. })
        ));
    }

    #[test]
    fn only_chatgpt_token_refresh_is_a_handleable_server_request() {
        assert_eq!(
            classify_server_request("account/chatgptAuthTokens/refresh"),
            CodexServerRequestClass::AuthRefresh
        );
        for method in [
            "applyPatchApproval",
            "item/permissions/requestApproval",
            "mcpServer/elicitation/request",
            "item/tool/call",
            "attestation/generate",
        ] {
            assert_eq!(
                classify_server_request(method),
                CodexServerRequestClass::Consequential,
                "{method}"
            );
        }
        assert_eq!(
            classify_server_request("thread/shellCommand"),
            CodexServerRequestClass::Unknown
        );
    }

    #[test]
    fn protocol_fixture_covers_the_allowlist_and_the_dangerous_methods() {
        let inventory = protocol_inventory().expect("fixture");
        assert_eq!(
            inventory.codex_cli_version,
            CODEX_APP_SERVER_QUALIFIED_VERSION
        );
        assert!(!inventory.experimental_api);
        assert!(inventory
            .client_notifications
            .iter()
            .any(|m| m == "initialized"));

        let client: BTreeSet<_> = inventory
            .client_methods
            .iter()
            .map(String::as_str)
            .collect();
        for method in CODEX_APP_SERVER_STABLE_METHODS {
            if *method == "initialized" {
                continue;
            }
            assert!(
                client.contains(method),
                "{method} is stable for V1 but missing from the 0.147.0 client inventory"
            );
        }
        for method in [
            "thread/shellCommand",
            "command/exec",
            "config/value/write",
            "fs/writeFile",
            "plugin/install",
        ] {
            assert!(
                client.contains(method),
                "{method} must stay in the prohibited set because 0.147.0 still exposes it"
            );
            assert!(!may_send_method(method));
        }
        assert!(!client.contains("process/spawn"));
        assert!(!client.contains("thread/turns/list"));
        assert!(!THREAD_DESCENDANT_SETTLEMENT_IMPLIED);
        assert!(!CODEX_APP_SERVER_EXPERIMENTAL_API_ENABLED);
        for method in CODEX_APP_SERVER_EXPERIMENTAL_OFF_IN_V1 {
            assert!(!may_send_method(method), "{method}");
        }
        let requests: BTreeSet<_> = inventory
            .server_requests
            .iter()
            .map(String::as_str)
            .collect();
        assert!(requests.contains("account/chatgptAuthTokens/refresh"));
        assert!(requests.contains("item/permissions/requestApproval"));
    }

    #[test]
    fn unknown_notifications_are_counted_not_sent() {
        let inventory = protocol_inventory().expect("fixture");
        assert!(inventory
            .server_notifications
            .iter()
            .any(|m| m == "turn/completed"));
        assert!(inventory
            .server_notifications
            .iter()
            .any(|m| m == "item/agentMessage/delta"));
        assert!(!may_send_method("turn/completed"));
    }
}
