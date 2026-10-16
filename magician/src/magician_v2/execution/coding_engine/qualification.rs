//! Stage 2b: qualify a discovered Codex binary from injected evidence.
//!
//! Request handlers never call this. They only read a cached receipt that
//! matches the current filesystem identity. Live stdio probes wait for a
//! background worker; this module is the fail-closed classifier those
//! probes (and tests) feed.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use semver::Version;
use serde_json::Value;

use super::{
    codex_contract::{
        attest_effective_config, CODEX_APP_SERVER_KNOWN_BAD_VERSIONS,
        CODEX_APP_SERVER_MINIMUM_VERSION,
    },
    discovery::{codex_is_selectable, CodexReadiness, CodexReadinessSnapshot},
};

/// Names a child `codex app-server` may inherit. Values are never logged.
pub const CODEX_CHILD_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "TEMP",
    "TMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "CODEX_HOME",
    "OPENAI_API_KEY",
    // The plane harness engine's grant channel: its isolated config.toml
    // names this variable via `bearer_token_env_var`, so the child MUST
    // receive it or every plane tool call authenticates nothing. Scoped to
    // the run's `plt_` grant; vibedev launches simply never set it.
    "MAGICIAN_PLANE_GRANT",
];

const RECEIPT_TTL_MS: i64 = 6 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuthClass {
    SignedIn,
    ApiKey,
    RequiredMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexVersionClass {
    Compatible { version: String },
    NewerCompatible { version: String },
    BelowMinimum { version: String },
    KnownBad { version: String },
    Unparseable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceProbe {
    pub thread_id: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CodexQualifyEvidence {
    pub identity: String,
    pub cli_version: Option<String>,
    pub config: Option<Value>,
    pub account: Option<Value>,
    pub instruction_sources: Vec<Value>,
    pub project_root: Option<PathBuf>,
    pub persistence_probe: Option<PersistenceProbe>,
    /// Why the qualification `thread/start` was refused, when it was.
    ///
    /// A refusal used to be swallowed, leaving `persistence_probe` empty and
    /// the readiness checks with nothing to look at — so an install that
    /// rejects the parameters every run sends still published as Ready. This
    /// carries the refusal so [`qualify_from_evidence`] can fail closed.
    pub thread_start_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexQualificationReceipt {
    pub identity: String,
    pub version: Option<String>,
    pub readiness: CodexReadiness,
    pub reason: String,
    pub attestation_digest: Option<String>,
    pub auth_class: Option<CodexAuthClass>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub qualified_at_ms: i64,
    pub expires_at_ms: i64,
}

impl CodexQualificationReceipt {
    pub fn expired_at(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionSourceError {
    Denied { class: String },
    OutsideProject,
    MissingPath,
}

/// Classify version, attestation, auth, instruction sources, and probe cleanup.
/// The raw config/account JSON is not stored on the receipt.
pub fn qualify_from_evidence(evidence: CodexQualifyEvidence) -> CodexQualificationReceipt {
    let now = now_ms();
    let mut receipt = CodexQualificationReceipt {
        identity: evidence.identity.clone(),
        version: None,
        readiness: CodexReadiness::Unqualified,
        reason: "Codex is installed but not yet qualified".to_string(),
        attestation_digest: None,
        auth_class: None,
        model: None,
        reasoning_effort: None,
        qualified_at_ms: now,
        expires_at_ms: now.saturating_add(RECEIPT_TTL_MS),
    };

    if evidence.cli_version.is_none() && evidence.config.is_none() {
        receipt.readiness = CodexReadiness::Unqualified;
        receipt.reason = "qualification session did not complete".to_string();
        return receipt;
    }

    match classify_codex_version(evidence.cli_version.as_deref().unwrap_or("")) {
        CodexVersionClass::Unparseable => {
            receipt.readiness = CodexReadiness::Incompatible;
            receipt.reason = "Codex version could not be parsed".to_string();
            return receipt;
        },
        CodexVersionClass::BelowMinimum { version } => {
            receipt.version = Some(version.clone());
            receipt.readiness = CodexReadiness::Incompatible;
            receipt.reason =
                format!("Codex {version} is below the minimum {CODEX_APP_SERVER_MINIMUM_VERSION}");
            return receipt;
        },
        CodexVersionClass::KnownBad { version } => {
            receipt.version = Some(version.clone());
            receipt.readiness = CodexReadiness::Incompatible;
            receipt.reason = format!("Codex {version} is on the known-bad list");
            return receipt;
        },
        CodexVersionClass::Compatible { version }
        | CodexVersionClass::NewerCompatible { version } => {
            receipt.version = Some(version);
        },
    }

    let Some(config) = evidence.config.as_ref() else {
        receipt.readiness = CodexReadiness::Incompatible;
        receipt.reason =
            "qualification did not receive an effective config/read result".to_string();
        return receipt;
    };
    match attest_effective_config(config) {
        Ok(attestation) => receipt.attestation_digest = Some(attestation.digest),
        Err(error) => {
            receipt.readiness = CodexReadiness::Incompatible;
            receipt.reason = format!("effective config failed attestation: {error}");
            return receipt;
        },
    }

    match classify_account(evidence.account.as_ref()) {
        CodexAuthClass::RequiredMissing => {
            receipt.auth_class = Some(CodexAuthClass::RequiredMissing);
            receipt.readiness = CodexReadiness::AuthRequired;
            receipt.reason =
                "Codex is signed out and its provider requires authentication".to_string();
            return receipt;
        },
        class => receipt.auth_class = Some(class),
    }

    for source in &evidence.instruction_sources {
        if let Err(error) = classify_instruction_source(source, evidence.project_root.as_deref()) {
            receipt.readiness = CodexReadiness::Incompatible;
            receipt.reason = match error {
                InstructionSourceError::Denied { class } => {
                    format!("instruction source `{class}` is not allowed")
                },
                InstructionSourceError::OutsideProject => {
                    "instruction source is outside the project root".to_string()
                },
                InstructionSourceError::MissingPath => {
                    "instruction source is missing a path".to_string()
                },
            };
            return receipt;
        }
    }

    // A refused `thread/start` means this install rejects the parameters every
    // run sends, so it is not usable however healthy the rest of the handshake
    // looked. Checked before the cleanup probe because a refusal leaves no
    // probe to inspect.
    if let Some(error) = evidence.thread_start_error.as_deref() {
        receipt.readiness = CodexReadiness::Incompatible;
        receipt.reason = format!("qualification thread/start was refused ({error})");
        return receipt;
    }

    if let Some(probe) = evidence.persistence_probe.as_ref() {
        if !probe.deleted {
            receipt.readiness = CodexReadiness::Unqualified;
            receipt.reason = "qualification thread was created but not deleted; readiness stays \
                              unpublished"
                .to_string();
            return receipt;
        }
    }

    let model = config_string(config, &["model"]);
    let reasoning_effort = config_string(config, &["model_reasoning_effort"])
        .or_else(|| config_string(config, &["reasoning_effort"]));
    match (model, reasoning_effort) {
        (Some(model), Some(effort)) if !model.is_empty() && !effort.is_empty() => {
            receipt.model = Some(model);
            receipt.reasoning_effort = Some(effort);
        },
        _ => {
            receipt.readiness = CodexReadiness::Incompatible;
            receipt.reason =
                "qualified config is missing a concrete model and reasoning effort".to_string();
            return receipt;
        },
    }

    receipt.readiness = CodexReadiness::Ready;
    receipt.reason = "Codex is qualified and waiting for adapter activation".to_string();
    receipt
}

fn config_string(config: &Value, path: &[&str]) -> Option<String> {
    let mut current = config;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(str::to_string)
}

pub fn classify_codex_version(raw: &str) -> CodexVersionClass {
    let Some(version) = parse_codex_version(raw) else {
        return CodexVersionClass::Unparseable;
    };
    let rendered = version.to_string();
    if is_known_bad(&version) {
        return CodexVersionClass::KnownBad { version: rendered };
    }
    let Some(minimum) = Version::parse(CODEX_APP_SERVER_MINIMUM_VERSION).ok() else {
        return CodexVersionClass::Unparseable;
    };
    if version < minimum {
        return CodexVersionClass::BelowMinimum { version: rendered };
    }
    if rendered == CODEX_APP_SERVER_MINIMUM_VERSION {
        CodexVersionClass::Compatible { version: rendered }
    } else {
        CodexVersionClass::NewerCompatible { version: rendered }
    }
}

pub fn classify_account(account: Option<&Value>) -> CodexAuthClass {
    let Some(account) = account else {
        return CodexAuthClass::RequiredMissing;
    };
    let inner = account.get("account").unwrap_or(account);
    let signed_in = flag(account, &["chatgpt", "signedIn"])
        || flag(account, &["chatgpt", "signed_in"])
        || flag(account, &["signedIn"])
        || flag(account, &["authenticated"])
        || flag(inner, &["chatgpt", "signedIn"])
        || flag(inner, &["signedIn"])
        || flag(inner, &["authenticated"])
        || chatgpt_profile_present(inner);
    let api_key = flag(account, &["hasApiKey"])
        || flag(account, &["apiKeyPresent"])
        || flag(account, &["api_key_present"])
        || flag(inner, &["hasApiKey"])
        || flag(inner, &["apiKeyPresent"])
        || flag(inner, &["api_key_present"]);
    if signed_in {
        CodexAuthClass::SignedIn
    } else if api_key {
        CodexAuthClass::ApiKey
    } else {
        CodexAuthClass::RequiredMissing
    }
}

fn chatgpt_profile_present(account: &Value) -> bool {
    if account.get("type").and_then(Value::as_str) != Some("chatgpt") {
        return false;
    }
    nonempty_str(account, "email") || nonempty_str(account, "planType")
}

fn nonempty_str(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

pub fn classify_instruction_source(
    source: &Value,
    project_root: Option<&Path>,
) -> Result<String, InstructionSourceError> {
    let class = source
        .get("type")
        .or_else(|| source.get("origin"))
        .or_else(|| source.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("path");
    let lowered = class.to_ascii_lowercase();
    if matches!(
        lowered.as_str(),
        "user" | "global" | "plugin" | "skill" | "home" | "codex_home"
    ) {
        return Err(InstructionSourceError::Denied { class: lowered });
    }
    let raw_path = source
        .as_str()
        .map(PathBuf::from)
        .or_else(|| {
            source
                .get("path")
                .or_else(|| source.get("uri"))
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .ok_or(InstructionSourceError::MissingPath)?;
    if raw_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(InstructionSourceError::OutsideProject);
    }
    if is_reused_codex_home_agents(&raw_path) {
        return Ok("codex_home_agents".to_string());
    }
    let Some(project_root) = project_root else {
        if raw_path.is_absolute() {
            return Err(InstructionSourceError::OutsideProject);
        }
        return match raw_path.file_name().and_then(|name| name.to_str()) {
            Some("AGENTS.md") => Ok("project_agents".to_string()),
            _ => Err(InstructionSourceError::Denied {
                class: "unknown".to_string(),
            }),
        };
    };
    let project = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let candidate = if raw_path.is_absolute() {
        raw_path
    } else {
        project.join(raw_path)
    };
    let canonical = candidate.canonicalize().unwrap_or(candidate);
    if !canonical.starts_with(&project) {
        return Err(InstructionSourceError::OutsideProject);
    }
    match canonical.file_name().and_then(|name| name.to_str()) {
        Some("AGENTS.md") => Ok("project_agents".to_string()),
        _ => Err(InstructionSourceError::Denied {
            class: "unknown".to_string(),
        }),
    }
}

pub fn filter_codex_child_env<'a, I>(entries: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let allowed: BTreeMap<&str, ()> = CODEX_CHILD_ENV_ALLOWLIST
        .iter()
        .map(|key| (*key, ()))
        .collect();
    let mut filtered = BTreeMap::new();
    for (key, value) in entries {
        if allowed.contains_key(key) {
            filtered.insert(key.to_string(), value.to_string());
        }
    }
    filtered
}

pub fn cache_receipt(receipt: CodexQualificationReceipt) {
    receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(receipt.identity.clone(), receipt);
}

pub fn cached_receipt(identity: &str) -> Option<CodexQualificationReceipt> {
    receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(identity)
        .cloned()
}

pub fn invalidate_receipt(identity: &str) {
    receipt_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(identity);
}

pub fn overlay_receipt(
    mut snapshot: CodexReadinessSnapshot,
    receipt: &CodexQualificationReceipt,
    now_ms: i64,
) -> CodexReadinessSnapshot {
    if receipt.identity != snapshot.identity() || receipt.expired_at(now_ms) {
        return snapshot;
    }
    snapshot.readiness = receipt.readiness;
    snapshot.reason = receipt.reason.clone();
    snapshot.version = receipt.version.clone();
    snapshot.selectable = codex_is_selectable(receipt.readiness);
    snapshot
}

fn is_reused_codex_home_agents(path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) != Some("AGENTS.md") {
        return false;
    }
    if let Ok(home) = std::env::var("CODEX_HOME") {
        if !home.trim().is_empty() && path.starts_with(PathBuf::from(home)) {
            return true;
        }
    }
    dirs::home_dir().is_some_and(|home| path.starts_with(home.join(".codex")))
}

fn parse_codex_version(raw: &str) -> Option<Version> {
    let trimmed = raw.trim();
    let rest = trimmed
        .strip_prefix("codex-cli ")
        .or_else(|| trimmed.strip_prefix("codex "))
        .or_else(|| trimmed.strip_prefix('v'))
        .unwrap_or(trimmed)
        .trim();
    let token = rest.split_whitespace().next().unwrap_or(rest);
    Version::parse(token).ok()
}

fn is_known_bad(version: &Version) -> bool {
    CODEX_APP_SERVER_KNOWN_BAD_VERSIONS.iter().any(|listed| {
        Version::parse(listed)
            .ok()
            .is_some_and(|bad| bad == *version)
    })
}

fn flag(value: &Value, path: &[&str]) -> bool {
    let mut current = value;
    for key in path {
        current = match current.get(*key) {
            Some(next) => next,
            None => return false,
        };
    }
    current.as_bool().unwrap_or(false)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn receipt_slot() -> &'static Mutex<BTreeMap<String, CodexQualificationReceipt>> {
    static SLOT: OnceLock<Mutex<BTreeMap<String, CodexQualificationReceipt>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    fn narrowed_config() -> Value {
        let mut features = serde_json::Map::new();
        for feature in super::super::codex_contract::CODEX_FEATURES_MUST_BE_OFF {
            features.insert((*feature).to_string(), Value::Bool(false));
        }
        json!({
            "approval_policy": "never",
            "sandbox_mode": "workspace-write",
            "web_search": "disabled",
            "model": "gpt-5.6-terra",
            "model_reasoning_effort": "xhigh",
            "agents": { "enabled": false },
            "features": features
        })
    }

    fn signed_in_account() -> Value {
        json!({ "chatgpt": { "signedIn": true }, "email": "secret@example.com" })
    }

    fn ready_evidence() -> CodexQualifyEvidence {
        CodexQualifyEvidence {
            identity: "bin-1".to_string(),
            cli_version: Some("codex-cli 0.147.0".to_string()),
            config: Some(narrowed_config()),
            account: Some(signed_in_account()),
            instruction_sources: Vec::new(),
            project_root: None,
            persistence_probe: Some(PersistenceProbe {
                thread_id: "thread-1".to_string(),
                deleted: true,
            }),
            thread_start_error: None,
        }
    }

    #[test]
    fn version_policy_accepts_minimum_and_newer_and_rejects_old_or_garbage() {
        assert!(matches!(
            classify_codex_version("0.147.0"),
            CodexVersionClass::Compatible { .. }
        ));
        assert!(matches!(
            classify_codex_version("codex-cli 0.148.1"),
            CodexVersionClass::NewerCompatible { version } if version == "0.148.1"
        ));
        assert!(matches!(
            classify_codex_version("0.146.0"),
            CodexVersionClass::BelowMinimum { .. }
        ));
        assert!(matches!(
            classify_codex_version("not-a-version"),
            CodexVersionClass::Unparseable
        ));
    }

    #[test]
    fn incomplete_session_evidence_stays_unqualified() {
        let receipt = qualify_from_evidence(CodexQualifyEvidence {
            identity: "bin-1".to_string(),
            ..CodexQualifyEvidence::default()
        });
        assert_eq!(receipt.readiness, CodexReadiness::Unqualified);
        assert!(
            receipt.reason.contains("did not complete"),
            "{}",
            receipt.reason
        );
        assert!(!receipt.reason.contains("could not be parsed"));
    }

    #[test]
    fn a_refused_thread_start_is_incompatible_not_ready() {
        // The regression this guards: the refusal used to be dropped on the
        // floor, so an install that rejects the arguments every run sends still
        // published as Ready and failed at dispatch instead.
        let mut evidence = ready_evidence();
        evidence.persistence_probe = None;
        evidence.thread_start_error = Some("workspace-write: unknown variant".to_string());
        let receipt = qualify_from_evidence(evidence);
        assert_eq!(receipt.readiness, CodexReadiness::Incompatible);
        assert!(!codex_is_selectable(receipt.readiness));
        assert!(
            receipt.reason.contains("thread/start"),
            "{}",
            receipt.reason
        );
    }

    #[test]
    fn ready_evidence_is_selectable_and_drops_account_secrets() {
        let receipt = qualify_from_evidence(ready_evidence());
        assert_eq!(receipt.readiness, CodexReadiness::Ready);
        assert!(codex_is_selectable(receipt.readiness));
        assert_eq!(receipt.auth_class, Some(CodexAuthClass::SignedIn));
        assert_eq!(receipt.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(receipt.reasoning_effort.as_deref(), Some("xhigh"));
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("secret@example.com"), "{rendered}");
        assert!(receipt.attestation_digest.is_some());
    }

    #[test]
    fn signed_out_openai_account_is_auth_required() {
        let mut evidence = ready_evidence();
        evidence.account = Some(json!({ "chatgpt": { "signedIn": false } }));
        let receipt = qualify_from_evidence(evidence);
        assert_eq!(receipt.readiness, CodexReadiness::AuthRequired);
    }

    #[test]
    fn live_chatgpt_account_read_shape_is_signed_in() {
        let mut evidence = ready_evidence();
        evidence.account = Some(json!({
            "account": { "type": "chatgpt", "email": "dev@example.com", "planType": "pro" },
            "requiresOpenaiAuth": true
        }));
        let receipt = qualify_from_evidence(evidence);
        assert_eq!(receipt.readiness, CodexReadiness::Ready);
        assert_eq!(receipt.auth_class, Some(CodexAuthClass::SignedIn));
    }

    #[test]
    fn reused_codex_home_agents_md_does_not_block_ready() {
        let home = dirs::home_dir().expect("home");
        let source = home.join(".codex").join("AGENTS.md");
        let class = classify_instruction_source(&json!(source.to_string_lossy()), None)
            .expect("codex home agents");
        assert_eq!(class, "codex_home_agents");
    }

    #[test]
    fn attestation_failure_is_incompatible() {
        let mut evidence = ready_evidence();
        evidence.config = Some(json!({ "approval_policy": "on-request" }));
        let receipt = qualify_from_evidence(evidence);
        assert_eq!(receipt.readiness, CodexReadiness::Incompatible);
        assert!(receipt.reason.contains("attestation"), "{}", receipt.reason);
    }

    #[test]
    fn undeleted_probe_thread_blocks_ready() {
        let mut evidence = ready_evidence();
        evidence.persistence_probe = Some(PersistenceProbe {
            thread_id: "thread-1".to_string(),
            deleted: false,
        });
        let receipt = qualify_from_evidence(evidence);
        assert_eq!(receipt.readiness, CodexReadiness::Unqualified);
        assert!(receipt.reason.contains("not deleted"), "{}", receipt.reason);
    }

    #[test]
    fn home_instruction_source_is_rejected() {
        let project = tempfile::tempdir().expect("project");
        let err = classify_instruction_source(
            &json!({ "type": "home", "path": "AGENTS.md" }),
            Some(project.path()),
        )
        .expect_err("home");
        assert!(matches!(err, InstructionSourceError::Denied { .. }));
    }

    #[test]
    fn project_agents_md_is_allowed() {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(project.path().join("AGENTS.md"), "# agents\n").expect("write");
        let class =
            classify_instruction_source(&json!({ "path": "AGENTS.md" }), Some(project.path()))
                .expect("project");
        assert_eq!(class, "project_agents");
        let relative = classify_instruction_source(&json!({ "path": "AGENTS.md" }), None)
            .expect("relative agents");
        assert_eq!(relative, "project_agents");
        let unexpected =
            classify_instruction_source(&json!({ "path": "notes.md" }), None).expect_err("unknown");
        assert!(matches!(unexpected, InstructionSourceError::Denied { .. }));
    }

    #[test]
    fn unexpected_instruction_sources_fail_closed_without_a_project() {
        let mut evidence = ready_evidence();
        evidence.instruction_sources = vec![json!({ "type": "plugin", "path": "AGENTS.md" })];
        let receipt = qualify_from_evidence(evidence);
        assert_eq!(receipt.readiness, CodexReadiness::Incompatible);
        assert!(receipt.reason.contains("plugin"), "{}", receipt.reason);
    }

    #[test]
    fn child_env_allowlist_drops_magician_secrets() {
        let filtered = filter_codex_child_env([
            ("PATH", "/usr/bin"),
            ("CODEX_HOME", "/secret/codex"),
            ("MAGICIAN_ADMIN_TOKEN", "nope"),
            ("OPENAI_API_KEY", "sk-test"),
        ]);
        assert!(filtered.contains_key("PATH"));
        assert!(filtered.contains_key("CODEX_HOME"));
        assert!(filtered.contains_key("OPENAI_API_KEY"));
        assert!(!filtered.contains_key("MAGICIAN_ADMIN_TOKEN"));
    }

    #[test]
    fn expired_or_mismatched_receipt_does_not_overlay() {
        let snapshot = CodexReadinessSnapshot::unqualified_for_test("bin-1");
        let mut receipt = qualify_from_evidence(ready_evidence());
        receipt.identity = "bin-1".to_string();
        receipt.expires_at_ms = 1;
        let kept = overlay_receipt(snapshot.clone(), &receipt, 2);
        assert_eq!(kept.readiness, CodexReadiness::Unqualified);
        receipt.expires_at_ms = 10;
        receipt.identity = "other".to_string();
        let kept = overlay_receipt(snapshot, &receipt, 2);
        assert_eq!(kept.readiness, CodexReadiness::Unqualified);
    }

    #[test]
    fn matching_fresh_receipt_overlays_ready_and_selectable() {
        let snapshot = CodexReadinessSnapshot::unqualified_for_test("bin-1");
        let mut receipt = qualify_from_evidence(ready_evidence());
        receipt.identity = "bin-1".to_string();
        let overlaid = overlay_receipt(snapshot, &receipt, receipt.qualified_at_ms);
        assert_eq!(overlaid.readiness, CodexReadiness::Ready);
        assert!(overlaid.selectable);
        assert_eq!(overlaid.version.as_deref(), Some("0.147.0"));
    }

    #[test]
    fn request_handlers_do_not_start_qualification() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        assert!(
            !web.contains("qualify_from_evidence("),
            "GET/POST coding handlers must not start a qualification probe"
        );
        let discovery = include_str!("discovery.rs");
        assert!(
            !discovery.contains("qualify_from_evidence("),
            "filesystem observe/refresh must not start a qualification probe"
        );
        let web_again = include_str!("../../../../../magician-api/src/web_api.rs");
        assert!(
            !web_again.contains("qualify_over_stdio(")
                && !web_again.contains("qualify_child_stdio(")
                && !web_again.contains("grok_qualify_over_stdio(")
                && !web_again.contains("grok_qualify_child_stdio("),
            "HTTP handlers must not open a qualify stdio session"
        );
    }
}
