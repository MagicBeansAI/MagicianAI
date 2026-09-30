//! Codex and Grok installation discovery and readiness snapshots.
//!
//! Request paths read [`current_codex_readiness`] or
//! [`observe_grok_readiness`] (filesystem + cached overlays).
//! They never start a probe. Stage 2a resolves an operator path, PATH, and
//! reviewed install locations. Stage 2b may overlay a cached Codex
//! qualification receipt for the same identity. Grok version overlay comes
//! from the background tick that runs `grok --version`, never from HTTP.
//! Ready Grok also requires `~/.grok/auth.json` or a non-empty filtered
//! `XAI_API_KEY`, then a cached ACP isolation receipt (MCP / hooks /
//! plugins / web-search). HTTP never ACP-initializes.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, RwLock},
    time::Instant,
};

use serde::Serialize;
use serde_json::{json, Value};

use crate::config::{
    CodingProfileInfo, MagicianAgySettings, MagicianClaudeSettings, MagicianCodexSettings,
    MagicianGrokSettings,
};

pub const CODEX_DEFAULT_PROFILE_ID: &str = "codex-default";
pub const CODEX_CLIENT_NAME: &str = "magician";
pub const CODEX_CLIENT_TITLE: &str = "Magician";
pub const GROK_DEFAULT_PROFILE_ID: &str = "grok-default";
pub const CLAUDE_DEFAULT_PROFILE_ID: &str = "claude-default";
pub const AGY_DEFAULT_PROFILE_ID: &str = "agy-default";

const REFRESH_COALESCE: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexReadiness {
    Disabled,
    Missing,
    Checking,
    AmbiguousInstallation,
    Unqualified,
    Incompatible,
    AuthRequired,
    Ready,
}

impl CodexReadiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Missing => "missing",
            Self::Checking => "checking",
            Self::AmbiguousInstallation => "ambiguous_installation",
            Self::Unqualified => "unqualified",
            Self::Incompatible => "incompatible",
            Self::AuthRequired => "auth_required",
            Self::Ready => "ready",
        }
    }
}

/// Public snapshot. No paths, home, account, or environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodexReadinessSnapshot {
    pub readiness: CodexReadiness,
    pub reason: String,
    pub revision: u64,
    pub selectable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip)]
    identity: String,
}

impl Default for CodexReadinessSnapshot {
    fn default() -> Self {
        Self {
            readiness: CodexReadiness::Disabled,
            reason: "Codex app-server is disabled".to_string(),
            revision: 0,
            selectable: false,
            version: None,
            identity: "disabled".to_string(),
        }
    }
}

impl CodexReadinessSnapshot {
    pub fn public_json(&self) -> Value {
        let mut body = json!({
            "readiness": self.readiness,
            "reason": self.reason,
            "revision": self.revision,
            "selectable": self.selectable,
        });
        if let Some(version) = self.version.as_ref() {
            body["version"] = json!(version);
        }
        body
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn unqualified_for_test(identity: &str) -> Self {
        Self {
            readiness: CodexReadiness::Unqualified,
            reason: "Codex is installed but not yet qualified".to_string(),
            revision: 1,
            selectable: false,
            version: None,
            identity: identity.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CodexSearchPaths {
    pub path: Option<OsString>,
    pub reviewed: Vec<PathBuf>,
}

impl CodexSearchPaths {
    pub fn production() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            reviewed: default_reviewed_codex_locations(),
        }
    }
}

pub fn default_reviewed_codex_locations() -> Vec<PathBuf> {
    let mut locations = vec![
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ];
    if let Some(home) = dirs::home_dir() {
        locations.push(home.join(".local/bin/codex"));
        locations.push(home.join(".cargo/bin/codex"));
    }
    locations
}

pub fn magician_codex_client_info() -> Value {
    json!({
        "name": CODEX_CLIENT_NAME,
        "title": CODEX_CLIENT_TITLE,
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// Named-profile activation: only a Ready install is selectable. Disabled,
/// missing, checking, unqualified, incompatible, and auth-required stay hidden.
pub fn codex_is_selectable(readiness: CodexReadiness) -> bool {
    matches!(readiness, CodexReadiness::Ready)
}

pub fn observe_codex_readiness(
    config: &MagicianCodexSettings,
    search: &CodexSearchPaths,
) -> CodexReadinessSnapshot {
    let current = current_codex_readiness();
    let mut snapshot = resolve_codex_readiness(config, search);
    if current.identity() != snapshot.identity() {
        super::qualification::invalidate_receipt(current.identity());
    }
    if let Some(receipt) = super::qualification::cached_receipt(snapshot.identity()) {
        snapshot = super::qualification::overlay_receipt(snapshot, &receipt, now_ms());
    }
    if snapshot.identity == current.identity
        && snapshot.readiness == current.readiness
        && snapshot.version == current.version
    {
        return current;
    }
    snapshot.revision = current.revision.saturating_add(1);
    publish_if_newer(snapshot)
}

pub fn refresh_codex_readiness(
    config: &MagicianCodexSettings,
    search: &CodexSearchPaths,
) -> CodexReadinessSnapshot {
    let _guard = refresh_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(last) = last_refresh_at() {
        if last.elapsed() < REFRESH_COALESCE {
            return current_codex_readiness();
        }
    }
    let snapshot = observe_codex_readiness(config, search);
    record_refresh();
    snapshot
}

pub fn current_codex_readiness() -> CodexReadinessSnapshot {
    snapshot_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub fn project_coding_profiles(
    pi_rows: &[CodingProfileInfo],
    snapshot: &CodexReadinessSnapshot,
    grok: &GrokReadinessSnapshot,
    claude: &ClaudeReadinessSnapshot,
    agy: &AgyReadinessSnapshot,
) -> Vec<Value> {
    let mut rows: Vec<Value> = pi_rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "label": row.label,
                "llm_profile": row.llm_profile,
                "provider": row.provider,
                "model": row.model,
                "supports_user_image_inputs": row.supports_user_image_inputs,
                "is_default": row.is_default,
                "description": row.description,
                "engine": "pi",
                "selectable": true,
                "readiness": "ready",
            })
        })
        .collect();
    rows.push(codex_default_row(snapshot));
    rows.push(grok_default_row(grok));
    rows.push(claude_default_row(claude));
    rows.push(agy_default_row(agy));
    rows
}

fn codex_default_row(snapshot: &CodexReadinessSnapshot) -> Value {
    let mut row = json!({
        "id": CODEX_DEFAULT_PROFILE_ID,
        "label": "Codex",
        "engine": "codex_app_server",
        "selectable": snapshot.selectable,
        "readiness": snapshot.readiness,
        "reason": snapshot.reason,
        "is_default": false,
        "supports_user_image_inputs": false,
    });
    if let Some(version) = snapshot.version.as_ref() {
        row["version"] = json!(version);
    }
    row
}

fn resolve_codex_readiness(
    config: &MagicianCodexSettings,
    search: &CodexSearchPaths,
) -> CodexReadinessSnapshot {
    if !config.enabled {
        return CodexReadinessSnapshot {
            identity: "disabled".to_string(),
            ..CodexReadinessSnapshot::default()
        };
    }
    match resolve_codex_binary(config, search) {
        CodexResolution::Missing { reason } => snapshot(CodexReadiness::Missing, reason, "missing"),
        CodexResolution::Ambiguous { count } => snapshot(
            CodexReadiness::AmbiguousInstallation,
            format!(
                "multiple compatible Codex installations were found ({count}); set \
                 coding.codex.binary"
            ),
            format!("ambiguous:{count}"),
        ),
        CodexResolution::Found { identity, .. } => snapshot(
            CodexReadiness::Unqualified,
            "Codex is installed but not yet qualified".to_string(),
            identity,
        ),
    }
}

fn snapshot(
    readiness: CodexReadiness,
    reason: String,
    identity: impl Into<String>,
) -> CodexReadinessSnapshot {
    CodexReadinessSnapshot {
        readiness,
        reason,
        revision: 0,
        selectable: codex_is_selectable(readiness),
        version: None,
        identity: identity.into(),
    }
}

enum CodexResolution {
    Missing { reason: String },
    Ambiguous { count: usize },
    Found { identity: String, path: PathBuf },
}

/// Selected executable for the background qualify worker. Never returned on
/// the public snapshot or profiles API.
pub fn resolved_codex_executable(
    config: &MagicianCodexSettings,
    search: &CodexSearchPaths,
) -> Option<PathBuf> {
    match resolve_codex_binary(config, search) {
        CodexResolution::Found { path, .. } => Some(path),
        _ => None,
    }
}

fn resolve_codex_binary(
    config: &MagicianCodexSettings,
    search: &CodexSearchPaths,
) -> CodexResolution {
    if let Some(configured) = config
        .binary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let expanded = expand_home(configured);
        return match canonicalize_file(&expanded) {
            Some(canonical) => CodexResolution::Found {
                identity: identity_for(&canonical),
                path: canonical,
            },
            None => CodexResolution::Missing {
                reason: "configured Codex binary was not found".to_string(),
            },
        };
    }

    let mut found = BTreeSet::new();
    if let Some(path) = search.path.as_ref() {
        for dir in std::env::split_paths(path) {
            if let Some(canonical) = canonicalize_file(&dir.join("codex")) {
                found.insert(canonical);
            }
        }
    }
    for location in &search.reviewed {
        if let Some(canonical) = canonicalize_file(location) {
            found.insert(canonical);
        }
    }

    match found.len() {
        0 => CodexResolution::Missing {
            reason: "Codex is not installed".to_string(),
        },
        1 => {
            let path = found.into_iter().next().expect("one");
            CodexResolution::Found {
                identity: identity_for(&path),
                path,
            }
        },
        count => CodexResolution::Ambiguous { count },
    }
}

fn canonicalize_file(path: &Path) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    canonical.is_file().then_some(canonical)
}

pub fn identity_for(path: &Path) -> String {
    blake3::hash(path.display().to_string().as_bytes())
        .to_hex()
        .to_string()
}

/// Agy identity is the canonical path plus mtime and length. An in-place
/// binary replace must not inherit a Ready isolation receipt. This is a
/// filesystem stat only; it never spawns `agy --version`.
pub fn agy_identity_for(path: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.display().to_string().as_bytes());
    if let Ok(meta) = std::fs::metadata(path) {
        hasher.update(&meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified() {
            if let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH) {
                hasher.update(&elapsed.as_secs().to_le_bytes());
                hasher.update(&elapsed.subsec_nanos().to_le_bytes());
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn snapshot_slot() -> &'static RwLock<CodexReadinessSnapshot> {
    static SLOT: OnceLock<RwLock<CodexReadinessSnapshot>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(CodexReadinessSnapshot::default()))
}

fn refresh_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn last_refresh() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn last_refresh_at() -> Option<Instant> {
    *last_refresh()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn record_refresh() {
    *last_refresh()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
}

fn publish_if_newer(next: CodexReadinessSnapshot) -> CodexReadinessSnapshot {
    let mut slot = snapshot_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let kept = select_newer_snapshot(&slot, next);
    *slot = kept.clone();
    kept
}

fn select_newer_snapshot(
    current: &CodexReadinessSnapshot,
    next: CodexReadinessSnapshot,
) -> CodexReadinessSnapshot {
    if next.revision <= current.revision {
        current.clone()
    } else {
        next
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokReadiness {
    Disabled,
    Missing,
    Checking,
    AmbiguousInstallation,
    Unqualified,
    Incompatible,
    AuthRequired,
    Ready,
}

impl GrokReadiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Missing => "missing",
            Self::Checking => "checking",
            Self::AmbiguousInstallation => "ambiguous_installation",
            Self::Unqualified => "unqualified",
            Self::Incompatible => "incompatible",
            Self::AuthRequired => "auth_required",
            Self::Ready => "ready",
        }
    }
}

/// Operator-facing snapshot reasons. No paths, homes, filenames, or email.
pub const GROK_REASON_DISABLED: &str = "Grok Build CLI is disabled";
pub const GROK_REASON_MISSING: &str = "Grok is not installed";
pub const GROK_REASON_MISSING_CONFIGURED: &str = "configured Grok binary was not found";
pub const GROK_REASON_CHECKING: &str = "checking Grok Build CLI readiness";
pub const GROK_REASON_AMBIGUOUS: &str =
    "multiple compatible Grok installations were found; set coding.grok.binary";
pub const GROK_REASON_UNQUALIFIED_VERSION: &str =
    "Grok is installed but the CLI version is not yet known";
pub const GROK_REASON_UNQUALIFIED_ISOLATION: &str =
    "Grok is installed but isolation is not yet attested";
pub const GROK_REASON_UNQUALIFIED_UNATTESTED_LISTS: &str =
    "ACP initialize/session did not advertise MCP and tool lists; isolation is unattested";
pub const GROK_REASON_INCOMPATIBLE: &str = "Grok Build CLI is incompatible with Magician VibeDev";
pub const GROK_REASON_AUTH: &str = "sign in required";
pub const GROK_REASON_READY: &str = "Grok Build CLI is ready";

/// Default public reason for a readiness state. Specific overlays may refine
/// the copy (still without paths or email).
pub fn grok_reason_for(readiness: GrokReadiness) -> &'static str {
    match readiness {
        GrokReadiness::Disabled => GROK_REASON_DISABLED,
        GrokReadiness::Missing => GROK_REASON_MISSING,
        GrokReadiness::Checking => GROK_REASON_CHECKING,
        GrokReadiness::AmbiguousInstallation => GROK_REASON_AMBIGUOUS,
        GrokReadiness::Unqualified => GROK_REASON_UNQUALIFIED_ISOLATION,
        GrokReadiness::Incompatible => GROK_REASON_INCOMPATIBLE,
        GrokReadiness::AuthRequired => GROK_REASON_AUTH,
        GrokReadiness::Ready => GROK_REASON_READY,
    }
}

/// Public snapshot. No paths, home, account, or environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrokReadinessSnapshot {
    pub readiness: GrokReadiness,
    pub reason: String,
    pub revision: u64,
    pub selectable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip)]
    identity: String,
}

impl Default for GrokReadinessSnapshot {
    fn default() -> Self {
        Self {
            readiness: GrokReadiness::Disabled,
            reason: GROK_REASON_DISABLED.to_string(),
            revision: 0,
            selectable: false,
            version: None,
            identity: "disabled".to_string(),
        }
    }
}

impl GrokReadinessSnapshot {
    pub fn public_json(&self) -> Value {
        let mut body = json!({
            "readiness": self.readiness,
            "reason": self.reason,
            "revision": self.revision,
            "selectable": self.selectable,
        });
        if let Some(version) = self.version.as_ref() {
            body["version"] = json!(version);
        }
        body
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn unqualified_for_test(identity: &str) -> Self {
        Self {
            readiness: GrokReadiness::Unqualified,
            reason: GROK_REASON_UNQUALIFIED_ISOLATION.to_string(),
            revision: 1,
            selectable: false,
            version: Some("1.0.5".to_string()),
            identity: identity.to_string(),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn ready_for_test(identity: &str) -> Self {
        Self {
            readiness: GrokReadiness::Ready,
            reason: GROK_REASON_READY.to_string(),
            revision: 1,
            selectable: true,
            version: Some("1.0.5".to_string()),
            identity: identity.to_string(),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn auth_required_for_test(identity: &str) -> Self {
        Self {
            readiness: GrokReadiness::AuthRequired,
            reason: GROK_REASON_AUTH.to_string(),
            revision: 1,
            selectable: false,
            version: Some("1.0.5".to_string()),
            identity: identity.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GrokSearchPaths {
    pub path: Option<OsString>,
    pub reviewed: Vec<PathBuf>,
    /// Home used to locate `.grok/auth.json`. Tests inject a closed home.
    pub home: Option<PathBuf>,
    /// Env used for `XAI_API_KEY` after the Grok child filter. `None` means
    /// process env. Tests inject a closed map so the host key cannot leak.
    pub env: Option<Vec<(String, String)>>,
}

impl GrokSearchPaths {
    pub fn production() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            reviewed: default_reviewed_grok_locations(),
            home: dirs::home_dir(),
            env: None,
        }
    }
}

pub fn default_reviewed_grok_locations() -> Vec<PathBuf> {
    let mut locations = vec![
        PathBuf::from("/opt/homebrew/bin/grok"),
        PathBuf::from("/usr/local/bin/grok"),
    ];
    if let Some(home) = dirs::home_dir() {
        locations.push(home.join(".grok/bin/grok"));
        locations.push(home.join(".local/bin/grok"));
    }
    locations
}

pub fn grok_is_selectable(readiness: GrokReadiness) -> bool {
    matches!(readiness, GrokReadiness::Ready)
}

pub fn observe_grok_readiness(
    config: &MagicianGrokSettings,
    search: &GrokSearchPaths,
) -> GrokReadinessSnapshot {
    let current = current_grok_readiness();
    let mut snapshot = resolve_grok_readiness(config, search);
    if current.identity() != snapshot.identity() {
        invalidate_grok_overlays(current.identity());
    }
    if is_grok_found(&snapshot) {
        snapshot = overlay_cached_grok_version(snapshot, search);
        snapshot = overlay_cached_grok_attestation(snapshot);
    }
    if snapshot.identity == current.identity
        && snapshot.readiness == current.readiness
        && snapshot.version == current.version
    {
        return current;
    }
    snapshot.revision = current.revision.saturating_add(1);
    publish_grok_if_newer(snapshot)
}

/// Filesystem + cached version/auth overlay. Coalesced 2s. Never runs
/// `grok --version`; the background tick performs that probe.
pub fn refresh_grok_readiness(
    config: &MagicianGrokSettings,
    search: &GrokSearchPaths,
) -> GrokReadinessSnapshot {
    let _guard = grok_refresh_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(last) = grok_last_refresh_at() {
        if last.elapsed() < REFRESH_COALESCE {
            return current_grok_readiness();
        }
    }
    let snapshot = observe_grok_readiness(config, search);
    record_grok_refresh();
    snapshot
}

/// Background-only: spawn `grok --version` (5s) and fill the cache. HTTP
/// never calls this.
pub(crate) fn probe_grok_version_in_background(
    config: &MagicianGrokSettings,
    search: &GrokSearchPaths,
) {
    let snapshot = resolve_grok_readiness(config, search);
    if is_grok_found(&snapshot) {
        let _ = overlay_or_probe_grok_version(snapshot, config, search);
    }
}

pub fn current_grok_readiness() -> GrokReadinessSnapshot {
    grok_snapshot_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub fn resolved_grok_executable(
    config: &MagicianGrokSettings,
    search: &GrokSearchPaths,
) -> Option<PathBuf> {
    match resolve_grok_binary(config, search) {
        GrokResolution::Found { path, .. } => Some(path),
        _ => None,
    }
}

fn grok_default_row(snapshot: &GrokReadinessSnapshot) -> Value {
    let mut row = json!({
        "id": GROK_DEFAULT_PROFILE_ID,
        "label": "Grok",
        "engine": "grok_acp",
        "selectable": snapshot.selectable,
        "readiness": snapshot.readiness,
        "reason": snapshot.reason,
        "is_default": false,
        "supports_user_image_inputs": false,
    });
    if let Some(version) = snapshot.version.as_ref() {
        row["version"] = json!(version);
    }
    row
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeReadiness {
    Disabled,
    Missing,
    Checking,
    AmbiguousInstallation,
    Unqualified,
    Incompatible,
    AuthRequired,
    Ready,
}

impl ClaudeReadiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Missing => "missing",
            Self::Checking => "checking",
            Self::AmbiguousInstallation => "ambiguous_installation",
            Self::Unqualified => "unqualified",
            Self::Incompatible => "incompatible",
            Self::AuthRequired => "auth_required",
            Self::Ready => "ready",
        }
    }
}

pub const CLAUDE_REASON_DISABLED: &str = "Claude Code is disabled";
pub const CLAUDE_REASON_MISSING: &str = "Claude is not installed";
pub const CLAUDE_REASON_MISSING_CONFIGURED: &str = "configured Claude binary was not found";
pub const CLAUDE_REASON_AMBIGUOUS: &str =
    "multiple compatible Claude installations were found; set coding.claude.binary";
pub const CLAUDE_REASON_UNQUALIFIED_VERSION: &str =
    "Claude is installed but the CLI version is not yet known";
pub const CLAUDE_REASON_UNQUALIFIED_ISOLATION: &str =
    "Claude is installed but isolation is not yet attested";
pub const CLAUDE_REASON_UNQUALIFIED_UNATTESTED_LISTS: &str =
    "Claude init did not advertise MCP and tool lists; isolation is unattested";
pub const CLAUDE_REASON_READY: &str = "Claude Code is ready";

pub fn claude_reason_for(readiness: ClaudeReadiness) -> &'static str {
    match readiness {
        ClaudeReadiness::Disabled => CLAUDE_REASON_DISABLED,
        ClaudeReadiness::Missing => CLAUDE_REASON_MISSING,
        ClaudeReadiness::Checking => "checking Claude Code readiness",
        ClaudeReadiness::AmbiguousInstallation => CLAUDE_REASON_AMBIGUOUS,
        ClaudeReadiness::Unqualified => CLAUDE_REASON_UNQUALIFIED_ISOLATION,
        ClaudeReadiness::Incompatible => "Claude Code is incompatible with Magician VibeDev",
        ClaudeReadiness::AuthRequired => "sign in required",
        ClaudeReadiness::Ready => CLAUDE_REASON_READY,
    }
}

/// Public snapshot. No paths, home, account, or environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaudeReadinessSnapshot {
    pub readiness: ClaudeReadiness,
    pub reason: String,
    pub revision: u64,
    pub selectable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip)]
    identity: String,
}

impl Default for ClaudeReadinessSnapshot {
    fn default() -> Self {
        Self {
            readiness: ClaudeReadiness::Disabled,
            reason: CLAUDE_REASON_DISABLED.to_string(),
            revision: 0,
            selectable: false,
            version: None,
            identity: "disabled".to_string(),
        }
    }
}

impl ClaudeReadinessSnapshot {
    pub fn public_json(&self) -> Value {
        let mut body = json!({
            "readiness": self.readiness,
            "reason": self.reason,
            "revision": self.revision,
            "selectable": self.selectable,
        });
        if let Some(version) = self.version.as_ref() {
            body["version"] = json!(version);
        }
        body
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn ready_for_test(identity: &str) -> Self {
        Self {
            readiness: ClaudeReadiness::Ready,
            reason: CLAUDE_REASON_READY.to_string(),
            revision: 1,
            selectable: true,
            version: Some("2.1.229".to_string()),
            identity: identity.to_string(),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn unqualified_for_test(identity: &str) -> Self {
        Self {
            readiness: ClaudeReadiness::Unqualified,
            reason: CLAUDE_REASON_UNQUALIFIED_ISOLATION.to_string(),
            revision: 1,
            selectable: false,
            version: Some("2.1.229".to_string()),
            identity: identity.to_string(),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn auth_required_for_test(identity: &str) -> Self {
        Self {
            readiness: ClaudeReadiness::AuthRequired,
            reason: claude_reason_for(ClaudeReadiness::AuthRequired).to_string(),
            revision: 1,
            selectable: false,
            version: Some("2.1.229".to_string()),
            identity: identity.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClaudeSearchPaths {
    pub path: Option<OsString>,
    pub reviewed: Vec<PathBuf>,
    /// Home used to locate `~/.claude.json` OAuth. Tests inject a closed home.
    pub home: Option<PathBuf>,
    /// Env used only when `coding.claude.use_api_key` is true. `None` means
    /// process env. Tests inject a closed map so Magician's API key cannot
    /// impersonate a subscription.
    pub env: Option<Vec<(String, String)>>,
}

impl ClaudeSearchPaths {
    pub fn production() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            reviewed: default_reviewed_claude_locations(),
            home: dirs::home_dir(),
            env: None,
        }
    }
}

pub fn default_reviewed_claude_locations() -> Vec<PathBuf> {
    let mut locations = vec![
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ];
    if let Some(home) = dirs::home_dir() {
        locations.push(home.join(".local/bin/claude"));
    }
    locations
}

pub fn claude_is_selectable(readiness: ClaudeReadiness) -> bool {
    matches!(readiness, ClaudeReadiness::Ready)
}

pub fn observe_claude_readiness(
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) -> ClaudeReadinessSnapshot {
    let current = current_claude_readiness();
    let mut snapshot = resolve_claude_readiness(config, search);
    if current.identity() != snapshot.identity() {
        invalidate_claude_overlays(current.identity());
    }
    if is_claude_found(&snapshot) {
        snapshot = overlay_cached_claude_version(snapshot, search, config);
        snapshot = overlay_cached_claude_attestation(snapshot);
    }
    if snapshot.identity == current.identity
        && snapshot.readiness == current.readiness
        && snapshot.version == current.version
    {
        return current;
    }
    snapshot.revision = current.revision.saturating_add(1);
    publish_claude_if_newer(snapshot)
}

pub fn current_claude_readiness() -> ClaudeReadinessSnapshot {
    claude_snapshot_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub fn resolved_claude_executable(
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) -> Option<PathBuf> {
    match resolve_claude_binary(config, search) {
        ClaudeResolution::Found { path, .. } => Some(path),
        _ => None,
    }
}

fn claude_default_row(snapshot: &ClaudeReadinessSnapshot) -> Value {
    let mut row = json!({
        "id": CLAUDE_DEFAULT_PROFILE_ID,
        "label": "Claude",
        "engine": "claude_code",
        "selectable": snapshot.selectable,
        "readiness": snapshot.readiness,
        "reason": snapshot.reason,
        "is_default": false,
        "supports_user_image_inputs": false,
    });
    if let Some(version) = snapshot.version.as_ref() {
        row["version"] = json!(version);
    }
    row
}

fn resolve_claude_readiness(
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) -> ClaudeReadinessSnapshot {
    if !config.enabled {
        return ClaudeReadinessSnapshot {
            identity: "disabled".to_string(),
            ..ClaudeReadinessSnapshot::default()
        };
    }
    match resolve_claude_binary(config, search) {
        ClaudeResolution::Missing { reason } => {
            claude_snapshot(ClaudeReadiness::Missing, reason, "missing")
        },
        ClaudeResolution::Ambiguous { count } => claude_snapshot(
            ClaudeReadiness::AmbiguousInstallation,
            format!(
                "multiple compatible Claude installations were found ({count}); set \
                 coding.claude.binary"
            ),
            format!("ambiguous:{count}"),
        ),
        ClaudeResolution::Found { identity, .. } => claude_snapshot(
            ClaudeReadiness::Unqualified,
            CLAUDE_REASON_UNQUALIFIED_VERSION.to_string(),
            identity,
        ),
    }
}

fn is_claude_found(snapshot: &ClaudeReadinessSnapshot) -> bool {
    !matches!(
        snapshot.readiness,
        ClaudeReadiness::Disabled
            | ClaudeReadiness::Missing
            | ClaudeReadiness::AmbiguousInstallation
    )
}

/// Subscription-first: Magician's `ANTHROPIC_API_KEY` does not count unless
/// `coding.claude.use_api_key` is true. Public reason never names files or keys.
fn overlay_claude_auth(
    mut snapshot: ClaudeReadinessSnapshot,
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) -> ClaudeReadinessSnapshot {
    if snapshot.readiness != ClaudeReadiness::Ready {
        return snapshot;
    }
    if claude_has_auth(config, search) {
        return snapshot;
    }
    snapshot.readiness = ClaudeReadiness::AuthRequired;
    snapshot.selectable = false;
    snapshot.reason = claude_reason_for(ClaudeReadiness::AuthRequired).to_string();
    snapshot
}

fn claude_has_auth(config: &MagicianClaudeSettings, search: &ClaudeSearchPaths) -> bool {
    if config.use_api_key {
        return claude_api_key_present(search);
    }
    claude_oauth_present(search)
}

fn claude_oauth_present(search: &ClaudeSearchPaths) -> bool {
    let Some(home) = search.home.clone().or_else(dirs::home_dir) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(home.join(".claude.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    value
        .get("oauthAccount")
        .and_then(Value::as_object)
        .is_some_and(|account| !account.is_empty())
}

fn claude_api_key_present(search: &ClaudeSearchPaths) -> bool {
    let inherited: Vec<(String, String)> = match search.env.as_ref() {
        Some(env) => env.clone(),
        None => std::env::vars().collect(),
    };
    inherited.iter().any(|(key, value)| {
        super::claude_contract::is_claude_api_key_env(key) && !value.trim().is_empty()
    })
}

/// Filesystem + cached version/auth/isolation overlay. Coalesced 2s.
/// Never runs `claude --version`; the background tick performs that probe.
pub fn refresh_claude_readiness(
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) -> ClaudeReadinessSnapshot {
    let _guard = claude_refresh_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(last) = claude_last_refresh_at() {
        if last.elapsed() < REFRESH_COALESCE {
            return current_claude_readiness();
        }
    }
    let snapshot = observe_claude_readiness(config, search);
    record_claude_refresh();
    snapshot
}

/// Background-only: spawn `claude --version` (5s) and fill the cache. HTTP
/// never calls this.
pub(crate) fn probe_claude_version_in_background(
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) {
    let snapshot = resolve_claude_readiness(config, search);
    if is_claude_found(&snapshot) {
        let _ = overlay_or_probe_claude_version(snapshot, config, search);
    }
}

fn claude_snapshot(
    readiness: ClaudeReadiness,
    reason: String,
    identity: impl Into<String>,
) -> ClaudeReadinessSnapshot {
    ClaudeReadinessSnapshot {
        readiness,
        reason,
        revision: 0,
        selectable: claude_is_selectable(readiness),
        version: None,
        identity: identity.into(),
    }
}

enum ClaudeResolution {
    Missing { reason: String },
    Ambiguous { count: usize },
    Found { identity: String, path: PathBuf },
}

fn resolve_claude_binary(
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) -> ClaudeResolution {
    if let Some(configured) = config
        .binary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let expanded = expand_home(configured);
        return match canonicalize_file(&expanded) {
            Some(canonical) => ClaudeResolution::Found {
                identity: identity_for(&canonical),
                path: canonical,
            },
            None => ClaudeResolution::Missing {
                reason: CLAUDE_REASON_MISSING_CONFIGURED.to_string(),
            },
        };
    }

    let mut found = BTreeSet::new();
    if let Some(path) = search.path.as_ref() {
        for dir in std::env::split_paths(path) {
            if let Some(canonical) = canonicalize_file(&dir.join("claude")) {
                found.insert(canonical);
            }
        }
    }
    for location in &search.reviewed {
        if let Some(canonical) = canonicalize_file(location) {
            found.insert(canonical);
        }
    }

    match found.len() {
        0 => ClaudeResolution::Missing {
            reason: CLAUDE_REASON_MISSING.to_string(),
        },
        1 => {
            let path = found.into_iter().next().expect("one");
            ClaudeResolution::Found {
                identity: identity_for(&path),
                path,
            }
        },
        count => ClaudeResolution::Ambiguous { count },
    }
}

fn claude_snapshot_slot() -> &'static RwLock<ClaudeReadinessSnapshot> {
    static SLOT: OnceLock<RwLock<ClaudeReadinessSnapshot>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(ClaudeReadinessSnapshot::default()))
}

fn publish_claude_if_newer(next: ClaudeReadinessSnapshot) -> ClaudeReadinessSnapshot {
    let mut slot = claude_snapshot_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if next.revision <= slot.revision {
        return slot.clone();
    }
    *slot = next.clone();
    next
}

#[derive(Debug, Clone)]
enum CachedClaudeVersion {
    Parsed(String),
    Unparseable,
    TimedOut { at: Instant },
}

const CLAUDE_VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn overlay_cached_claude_version(
    snapshot: ClaudeReadinessSnapshot,
    search: &ClaudeSearchPaths,
    config: &MagicianClaudeSettings,
) -> ClaudeReadinessSnapshot {
    let identity = snapshot.identity.clone();
    let cached = claude_version_cache().get(&identity).cloned();
    let versioned = match cached {
        Some(CachedClaudeVersion::Parsed(version)) => {
            apply_claude_version(snapshot, Some(&version))
        },
        Some(CachedClaudeVersion::Unparseable) => apply_claude_version(snapshot, None),
        Some(CachedClaudeVersion::TimedOut { .. }) | None => snapshot,
    };
    overlay_claude_auth(versioned, config, search)
}

fn overlay_or_probe_claude_version(
    snapshot: ClaudeReadinessSnapshot,
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) -> ClaudeReadinessSnapshot {
    let identity = snapshot.identity.clone();
    let versioned = overlay_or_probe_claude_version_only(snapshot, config, search, &identity);
    overlay_claude_auth(versioned, config, search)
}

fn overlay_or_probe_claude_version_only(
    snapshot: ClaudeReadinessSnapshot,
    config: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
    identity: &str,
) -> ClaudeReadinessSnapshot {
    if let Some(cached) = claude_version_cache().get(identity).cloned() {
        match cached {
            CachedClaudeVersion::Parsed(version) => {
                return apply_claude_version(snapshot, Some(&version));
            },
            CachedClaudeVersion::Unparseable => return apply_claude_version(snapshot, None),
            CachedClaudeVersion::TimedOut { at } if at.elapsed() < REFRESH_COALESCE => {
                return snapshot;
            },
            CachedClaudeVersion::TimedOut { .. } => {},
        }
    }
    let Some(path) = resolved_claude_executable(config, search) else {
        return snapshot;
    };
    match probe_claude_cli_version(&path) {
        Some(raw) => {
            if let Some(version) = super::claude_contract::parse_claude_cli_version(&raw) {
                claude_version_cache().insert(
                    identity.to_string(),
                    CachedClaudeVersion::Parsed(version.clone()),
                );
                apply_claude_version(snapshot, Some(&version))
            } else {
                claude_version_cache()
                    .insert(identity.to_string(), CachedClaudeVersion::Unparseable);
                apply_claude_version(snapshot, None)
            }
        },
        None => {
            claude_version_cache().insert(
                identity.to_string(),
                CachedClaudeVersion::TimedOut { at: Instant::now() },
            );
            snapshot
        },
    }
}

fn overlay_cached_claude_attestation(snapshot: ClaudeReadinessSnapshot) -> ClaudeReadinessSnapshot {
    match snapshot.readiness {
        ClaudeReadiness::Ready | ClaudeReadiness::Unqualified => {},
        _ => return snapshot,
    }
    let version_ok = snapshot
        .version
        .as_deref()
        .is_some_and(super::claude_contract::claude_version_meets_minimum);
    if !version_ok && snapshot.readiness != ClaudeReadiness::Ready {
        return snapshot;
    }
    if let Some(receipt) = super::claude_qualification::cached_claude_receipt(snapshot.identity()) {
        return super::claude_qualification::overlay_claude_receipt(snapshot, &receipt, now_ms());
    }
    super::claude_qualification::demote_unattested_ready(snapshot)
}

fn apply_claude_version(
    mut snapshot: ClaudeReadinessSnapshot,
    version: Option<&str>,
) -> ClaudeReadinessSnapshot {
    match version {
        Some(version) if super::claude_contract::claude_version_meets_minimum(version) => {
            snapshot.readiness = ClaudeReadiness::Ready;
            snapshot.selectable = claude_is_selectable(snapshot.readiness);
            snapshot.version = Some(version.to_string());
            snapshot.reason = CLAUDE_REASON_READY.to_string();
        },
        Some(version) => {
            snapshot.readiness = ClaudeReadiness::Incompatible;
            snapshot.selectable = false;
            snapshot.version = Some(version.to_string());
            snapshot.reason = format!(
                "Claude CLI {version} is below the {} minimum",
                super::claude_contract::CLAUDE_MINIMUM_VERSION
            );
        },
        None => {
            snapshot.readiness = ClaudeReadiness::Incompatible;
            snapshot.selectable = false;
            snapshot.reason = "Claude CLI version was unparseable".to_string();
        },
    }
    snapshot
}

fn invalidate_claude_overlays(identity: &str) {
    claude_version_cache().remove(identity);
    super::claude_qualification::invalidate_claude_receipt(identity);
}

fn probe_claude_cli_version(path: &Path) -> Option<String> {
    let inherited: Vec<(String, String)> = std::env::vars().collect();
    let filtered = super::claude::filter_claude_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
        false,
    );
    let mut command = std::process::Command::new(path);
    command
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env_clear();
    for (key, value) in &filtered {
        command.env(key, value);
    }
    let mut child = command.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < CLAUDE_VERSION_PROBE_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            },
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            },
        }
    }
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut pipe, &mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
    }
    let text = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    Some(text)
}

fn claude_refresh_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn claude_last_refresh() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

fn claude_last_refresh_at() -> Option<Instant> {
    *claude_last_refresh()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn record_claude_refresh() {
    *claude_last_refresh()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
}

fn claude_version_cache() -> std::sync::MutexGuard<'static, HashMap<String, CachedClaudeVersion>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedClaudeVersion>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgyReadiness {
    Disabled,
    Missing,
    Checking,
    AmbiguousInstallation,
    Unqualified,
    Incompatible,
    AuthRequired,
    Ready,
}

impl AgyReadiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Missing => "missing",
            Self::Checking => "checking",
            Self::AmbiguousInstallation => "ambiguous_installation",
            Self::Unqualified => "unqualified",
            Self::Incompatible => "incompatible",
            Self::AuthRequired => "auth_required",
            Self::Ready => "ready",
        }
    }
}

pub const AGY_REASON_DISABLED: &str = "Antigravity is disabled";
pub const AGY_REASON_MISSING: &str = "Antigravity is not installed";
pub const AGY_REASON_MISSING_CONFIGURED: &str = "configured Agy binary was not found";
pub const AGY_REASON_AMBIGUOUS: &str =
    "multiple compatible Agy installations were found; set coding.agy.binary";
pub const AGY_REASON_UNQUALIFIED_VERSION: &str =
    "Antigravity is installed but the CLI version is not yet known";
pub const AGY_REASON_UNQUALIFIED_ISOLATION: &str =
    "Antigravity is installed but isolation is not yet attested";
pub const AGY_REASON_UNQUALIFIED_UNATTESTED_LISTS: &str =
    "Agy init did not advertise tools and permission_mode; isolation is unattested";
pub const AGY_REASON_READY: &str = "Antigravity is ready";

pub fn agy_reason_for(readiness: AgyReadiness) -> &'static str {
    match readiness {
        AgyReadiness::Disabled => AGY_REASON_DISABLED,
        AgyReadiness::Missing => AGY_REASON_MISSING,
        AgyReadiness::Checking => "checking Antigravity readiness",
        AgyReadiness::AmbiguousInstallation => AGY_REASON_AMBIGUOUS,
        AgyReadiness::Unqualified => AGY_REASON_UNQUALIFIED_ISOLATION,
        AgyReadiness::Incompatible => "Antigravity is incompatible with Magician VibeDev",
        AgyReadiness::AuthRequired => "sign in required",
        AgyReadiness::Ready => AGY_REASON_READY,
    }
}

/// Public snapshot. No paths, home, account, or environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgyReadinessSnapshot {
    pub readiness: AgyReadiness,
    pub reason: String,
    pub revision: u64,
    pub selectable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip)]
    identity: String,
}

impl Default for AgyReadinessSnapshot {
    fn default() -> Self {
        Self {
            readiness: AgyReadiness::Disabled,
            reason: AGY_REASON_DISABLED.to_string(),
            revision: 0,
            selectable: false,
            version: None,
            identity: "disabled".to_string(),
        }
    }
}

impl AgyReadinessSnapshot {
    pub fn public_json(&self) -> Value {
        let mut body = json!({
            "readiness": self.readiness,
            "reason": self.reason,
            "revision": self.revision,
            "selectable": self.selectable,
        });
        if let Some(version) = self.version.as_ref() {
            body["version"] = json!(version);
        }
        body
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn ready_for_test(identity: &str) -> Self {
        Self {
            readiness: AgyReadiness::Ready,
            reason: AGY_REASON_READY.to_string(),
            revision: 1,
            selectable: true,
            version: Some("1.1.19".to_string()),
            identity: identity.to_string(),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn unqualified_for_test(identity: &str) -> Self {
        Self {
            readiness: AgyReadiness::Unqualified,
            reason: AGY_REASON_UNQUALIFIED_ISOLATION.to_string(),
            revision: 1,
            selectable: false,
            version: Some("1.1.19".to_string()),
            identity: identity.to_string(),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn auth_required_for_test(identity: &str) -> Self {
        Self {
            readiness: AgyReadiness::AuthRequired,
            reason: agy_reason_for(AgyReadiness::AuthRequired).to_string(),
            revision: 1,
            selectable: false,
            version: Some("1.1.19".to_string()),
            identity: identity.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgySearchPaths {
    pub path: Option<OsString>,
    pub reviewed: Vec<PathBuf>,
    /// Home used to locate the Antigravity OAuth token. Tests inject a closed home.
    pub home: Option<PathBuf>,
    /// Env used only to detect already-present Google/Gemini keys. `None`
    /// means process env. Tests inject a closed map.
    pub env: Option<Vec<(String, String)>>,
}

impl AgySearchPaths {
    pub fn production() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            reviewed: default_reviewed_agy_locations(),
            home: dirs::home_dir(),
            env: None,
        }
    }
}

pub fn default_reviewed_agy_locations() -> Vec<PathBuf> {
    let mut locations = vec![
        PathBuf::from("/opt/homebrew/bin/agy"),
        PathBuf::from("/usr/local/bin/agy"),
    ];
    if let Some(home) = dirs::home_dir() {
        locations.push(home.join(".local/bin/agy"));
    }
    locations
}

pub fn agy_is_selectable(readiness: AgyReadiness) -> bool {
    matches!(readiness, AgyReadiness::Ready)
}

pub fn observe_agy_readiness(
    config: &MagicianAgySettings,
    search: &AgySearchPaths,
) -> AgyReadinessSnapshot {
    let current = current_agy_readiness();
    let mut snapshot = resolve_agy_readiness(config, search);
    if current.identity() != snapshot.identity() {
        invalidate_agy_overlays(current.identity());
    }
    if is_agy_found(&snapshot) {
        snapshot = overlay_cached_agy_version(snapshot, search, config);
        snapshot = overlay_cached_agy_attestation(snapshot);
    }
    if snapshot.identity == current.identity
        && snapshot.readiness == current.readiness
        && snapshot.version == current.version
    {
        return current;
    }
    snapshot.revision = current.revision.saturating_add(1);
    publish_agy_if_newer(snapshot)
}

pub fn current_agy_readiness() -> AgyReadinessSnapshot {
    agy_snapshot_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub fn resolved_agy_executable(
    config: &MagicianAgySettings,
    search: &AgySearchPaths,
) -> Option<PathBuf> {
    match resolve_agy_binary(config, search) {
        AgyResolution::Found { path, .. } => Some(path),
        _ => None,
    }
}

fn agy_default_row(snapshot: &AgyReadinessSnapshot) -> Value {
    let mut row = json!({
        "id": AGY_DEFAULT_PROFILE_ID,
        "label": "Antigravity",
        "engine": "agy_cli",
        "selectable": snapshot.selectable,
        "readiness": snapshot.readiness,
        "reason": snapshot.reason,
        "is_default": false,
        "supports_user_image_inputs": false,
    });
    if let Some(version) = snapshot.version.as_ref() {
        row["version"] = json!(version);
    }
    row
}

fn resolve_agy_readiness(
    config: &MagicianAgySettings,
    search: &AgySearchPaths,
) -> AgyReadinessSnapshot {
    if !config.enabled {
        return AgyReadinessSnapshot {
            identity: "disabled".to_string(),
            ..AgyReadinessSnapshot::default()
        };
    }
    match resolve_agy_binary(config, search) {
        AgyResolution::Missing { reason } => agy_snapshot(AgyReadiness::Missing, reason, "missing"),
        AgyResolution::Ambiguous { count } => agy_snapshot(
            AgyReadiness::AmbiguousInstallation,
            format!(
                "multiple compatible Agy installations were found ({count}); set \
                 coding.agy.binary"
            ),
            format!("ambiguous:{count}"),
        ),
        AgyResolution::Found { identity, .. } => agy_snapshot(
            AgyReadiness::Unqualified,
            AGY_REASON_UNQUALIFIED_VERSION.to_string(),
            identity,
        ),
    }
}

fn is_agy_found(snapshot: &AgyReadinessSnapshot) -> bool {
    !matches!(
        snapshot.readiness,
        AgyReadiness::Disabled | AgyReadiness::Missing | AgyReadiness::AmbiguousInstallation
    )
}

fn overlay_agy_auth(
    mut snapshot: AgyReadinessSnapshot,
    config: &MagicianAgySettings,
    search: &AgySearchPaths,
) -> AgyReadinessSnapshot {
    if snapshot.readiness != AgyReadiness::Ready {
        return snapshot;
    }
    if agy_has_auth(config, search) {
        return snapshot;
    }
    snapshot.readiness = AgyReadiness::AuthRequired;
    snapshot.selectable = false;
    snapshot.reason = agy_reason_for(AgyReadiness::AuthRequired).to_string();
    snapshot
}

fn agy_has_auth(config: &MagicianAgySettings, search: &AgySearchPaths) -> bool {
    if agy_oauth_present(search) {
        return true;
    }
    config.use_api_key && agy_api_key_present(search)
}

fn agy_oauth_present(search: &AgySearchPaths) -> bool {
    let Some(home) = search.home.clone().or_else(dirs::home_dir) else {
        return false;
    };
    super::agy_contract::AGY_OAUTH_TOKEN_RELATIVE_PATHS
        .iter()
        .any(|relative| {
            std::fs::read_to_string(home.join(relative)).is_ok_and(|text| !text.trim().is_empty())
        })
}

fn agy_api_key_present(search: &AgySearchPaths) -> bool {
    let inherited: Vec<(String, String)> = match search.env.as_ref() {
        Some(env) => env.clone(),
        None => std::env::vars().collect(),
    };
    inherited.iter().any(|(key, value)| {
        matches!(key.as_str(), "GEMINI_API_KEY" | "GOOGLE_API_KEY") && !value.trim().is_empty()
    })
}

/// Filesystem + cached version/auth/isolation overlay. Coalesced 2s.
/// Never runs `agy --version`; the background tick performs that probe.
pub fn refresh_agy_readiness(
    config: &MagicianAgySettings,
    search: &AgySearchPaths,
) -> AgyReadinessSnapshot {
    let _guard = agy_refresh_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(last) = agy_last_refresh_at() {
        if last.elapsed() < REFRESH_COALESCE {
            return current_agy_readiness();
        }
    }
    let snapshot = observe_agy_readiness(config, search);
    record_agy_refresh();
    snapshot
}

/// Background-only: spawn `agy --version` (5s) and fill the cache. HTTP
/// never calls this.
pub(crate) fn probe_agy_version_in_background(
    config: &MagicianAgySettings,
    search: &AgySearchPaths,
) {
    let snapshot = resolve_agy_readiness(config, search);
    if is_agy_found(&snapshot) {
        let _ = overlay_or_probe_agy_version(snapshot, config, search);
    }
}

fn agy_snapshot(
    readiness: AgyReadiness,
    reason: String,
    identity: impl Into<String>,
) -> AgyReadinessSnapshot {
    AgyReadinessSnapshot {
        readiness,
        reason,
        revision: 0,
        selectable: agy_is_selectable(readiness),
        version: None,
        identity: identity.into(),
    }
}

enum AgyResolution {
    Missing { reason: String },
    Ambiguous { count: usize },
    Found { identity: String, path: PathBuf },
}

fn resolve_agy_binary(config: &MagicianAgySettings, search: &AgySearchPaths) -> AgyResolution {
    if let Some(configured) = config
        .binary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let expanded = expand_home(configured);
        return match canonicalize_file(&expanded) {
            Some(canonical) => AgyResolution::Found {
                identity: agy_identity_for(&canonical),
                path: canonical,
            },
            None => AgyResolution::Missing {
                reason: AGY_REASON_MISSING_CONFIGURED.to_string(),
            },
        };
    }

    let mut found = BTreeSet::new();
    if let Some(path) = search.path.as_ref() {
        for dir in std::env::split_paths(path) {
            if let Some(canonical) = canonicalize_file(&dir.join("agy")) {
                found.insert(canonical);
            }
        }
    }
    for location in &search.reviewed {
        if let Some(canonical) = canonicalize_file(location) {
            found.insert(canonical);
        }
    }

    match found.len() {
        0 => AgyResolution::Missing {
            reason: AGY_REASON_MISSING.to_string(),
        },
        1 => {
            let path = found.into_iter().next().expect("one");
            AgyResolution::Found {
                identity: agy_identity_for(&path),
                path,
            }
        },
        count => AgyResolution::Ambiguous { count },
    }
}

fn agy_snapshot_slot() -> &'static RwLock<AgyReadinessSnapshot> {
    static SLOT: OnceLock<RwLock<AgyReadinessSnapshot>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(AgyReadinessSnapshot::default()))
}

fn publish_agy_if_newer(next: AgyReadinessSnapshot) -> AgyReadinessSnapshot {
    let mut slot = agy_snapshot_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if next.revision <= slot.revision {
        return slot.clone();
    }
    *slot = next.clone();
    next
}

#[derive(Debug, Clone)]
enum CachedAgyVersion {
    Parsed(String),
    Unparseable,
    TimedOut { at: Instant },
}

const AGY_VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn overlay_cached_agy_version(
    snapshot: AgyReadinessSnapshot,
    search: &AgySearchPaths,
    config: &MagicianAgySettings,
) -> AgyReadinessSnapshot {
    let identity = snapshot.identity.clone();
    let cached = agy_version_cache().get(&identity).cloned();
    let versioned = match cached {
        Some(CachedAgyVersion::Parsed(version)) => apply_agy_version(snapshot, Some(&version)),
        Some(CachedAgyVersion::Unparseable) => apply_agy_version(snapshot, None),
        Some(CachedAgyVersion::TimedOut { .. }) | None => snapshot,
    };
    overlay_agy_auth(versioned, config, search)
}

fn overlay_or_probe_agy_version(
    snapshot: AgyReadinessSnapshot,
    config: &MagicianAgySettings,
    search: &AgySearchPaths,
) -> AgyReadinessSnapshot {
    let identity = snapshot.identity.clone();
    let versioned = overlay_or_probe_agy_version_only(snapshot, config, search, &identity);
    overlay_agy_auth(versioned, config, search)
}

fn overlay_or_probe_agy_version_only(
    snapshot: AgyReadinessSnapshot,
    config: &MagicianAgySettings,
    search: &AgySearchPaths,
    identity: &str,
) -> AgyReadinessSnapshot {
    if let Some(cached) = agy_version_cache().get(identity).cloned() {
        match cached {
            CachedAgyVersion::TimedOut { at } if at.elapsed() < REFRESH_COALESCE => {
                return snapshot;
            },
            CachedAgyVersion::Parsed(_)
            | CachedAgyVersion::Unparseable
            | CachedAgyVersion::TimedOut { .. } => {},
        }
    }
    let Some(path) = resolved_agy_executable(config, search) else {
        return snapshot;
    };
    match probe_agy_cli_version(&path) {
        Some(raw) => {
            if let Some(version) = super::agy_contract::parse_agy_cli_version(&raw) {
                if let Some(CachedAgyVersion::Parsed(previous)) =
                    agy_version_cache().get(identity).cloned()
                {
                    if previous != version {
                        super::agy_qualification::invalidate_agy_receipt(identity);
                    }
                }
                agy_version_cache().insert(
                    identity.to_string(),
                    CachedAgyVersion::Parsed(version.clone()),
                );
                apply_agy_version(snapshot, Some(&version))
            } else {
                agy_version_cache().insert(identity.to_string(), CachedAgyVersion::Unparseable);
                apply_agy_version(snapshot, None)
            }
        },
        None => {
            agy_version_cache().insert(
                identity.to_string(),
                CachedAgyVersion::TimedOut { at: Instant::now() },
            );
            snapshot
        },
    }
}

fn overlay_cached_agy_attestation(snapshot: AgyReadinessSnapshot) -> AgyReadinessSnapshot {
    match snapshot.readiness {
        AgyReadiness::Ready | AgyReadiness::Unqualified => {},
        _ => return snapshot,
    }
    let version_ok = snapshot
        .version
        .as_deref()
        .is_some_and(super::agy_contract::agy_version_meets_minimum);
    if !version_ok && snapshot.readiness != AgyReadiness::Ready {
        return snapshot;
    }
    if let Some(receipt) = super::agy_qualification::cached_agy_receipt(snapshot.identity()) {
        return super::agy_qualification::overlay_agy_receipt(snapshot, &receipt, now_ms());
    }
    super::agy_qualification::demote_unattested_ready(snapshot)
}

fn apply_agy_version(
    mut snapshot: AgyReadinessSnapshot,
    version: Option<&str>,
) -> AgyReadinessSnapshot {
    match version {
        Some(version) if super::agy_contract::agy_version_meets_minimum(version) => {
            snapshot.readiness = AgyReadiness::Ready;
            snapshot.selectable = agy_is_selectable(snapshot.readiness);
            snapshot.version = Some(version.to_string());
            snapshot.reason = AGY_REASON_READY.to_string();
        },
        Some(version) => {
            snapshot.readiness = AgyReadiness::Incompatible;
            snapshot.selectable = false;
            snapshot.version = Some(version.to_string());
            snapshot.reason = format!(
                "Agy CLI {version} is below the {} minimum",
                super::agy_contract::AGY_MINIMUM_VERSION
            );
        },
        None => {
            snapshot.readiness = AgyReadiness::Incompatible;
            snapshot.selectable = false;
            snapshot.reason = "Agy CLI version was unparseable".to_string();
        },
    }
    snapshot
}

fn invalidate_agy_overlays(identity: &str) {
    agy_version_cache().remove(identity);
    super::agy_qualification::invalidate_agy_receipt(identity);
}

fn probe_agy_cli_version(path: &Path) -> Option<String> {
    let inherited: Vec<(String, String)> = std::env::vars().collect();
    let filtered = super::agy::filter_agy_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
        false,
    );
    let mut command = std::process::Command::new(path);
    command
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env_clear();
    for (key, value) in &filtered {
        command.env(key, value);
    }
    let mut child = command.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < AGY_VERSION_PROBE_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            },
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            },
        }
    }
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut pipe, &mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
    }
    let text = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    Some(text)
}

fn agy_refresh_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn agy_last_refresh() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

fn agy_last_refresh_at() -> Option<Instant> {
    *agy_last_refresh()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn record_agy_refresh() {
    *agy_last_refresh()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
}

fn agy_version_cache() -> std::sync::MutexGuard<'static, HashMap<String, CachedAgyVersion>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedAgyVersion>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn resolve_grok_readiness(
    config: &MagicianGrokSettings,
    search: &GrokSearchPaths,
) -> GrokReadinessSnapshot {
    if !config.enabled {
        return GrokReadinessSnapshot {
            identity: "disabled".to_string(),
            ..GrokReadinessSnapshot::default()
        };
    }
    match resolve_grok_binary(config, search) {
        GrokResolution::Missing { reason } => {
            grok_snapshot(GrokReadiness::Missing, reason, "missing")
        },
        GrokResolution::Ambiguous { count } => grok_snapshot(
            GrokReadiness::AmbiguousInstallation,
            format!(
                "multiple compatible Grok installations were found ({count}); set \
                 coding.grok.binary"
            ),
            format!("ambiguous:{count}"),
        ),
        GrokResolution::Found { identity, .. } => grok_snapshot(
            GrokReadiness::Unqualified,
            GROK_REASON_UNQUALIFIED_VERSION.to_string(),
            identity,
        ),
    }
}

fn grok_snapshot(
    readiness: GrokReadiness,
    reason: String,
    identity: impl Into<String>,
) -> GrokReadinessSnapshot {
    GrokReadinessSnapshot {
        readiness,
        reason,
        revision: 0,
        selectable: grok_is_selectable(readiness),
        version: None,
        identity: identity.into(),
    }
}

fn is_grok_found(snapshot: &GrokReadinessSnapshot) -> bool {
    !matches!(
        snapshot.readiness,
        GrokReadiness::Disabled | GrokReadiness::Missing | GrokReadiness::AmbiguousInstallation
    )
}

enum GrokResolution {
    Missing { reason: String },
    Ambiguous { count: usize },
    Found { identity: String, path: PathBuf },
}

fn resolve_grok_binary(config: &MagicianGrokSettings, search: &GrokSearchPaths) -> GrokResolution {
    if let Some(configured) = config
        .binary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let expanded = expand_home(configured);
        return match canonicalize_file(&expanded) {
            Some(canonical) => GrokResolution::Found {
                identity: identity_for(&canonical),
                path: canonical,
            },
            None => GrokResolution::Missing {
                reason: GROK_REASON_MISSING_CONFIGURED.to_string(),
            },
        };
    }

    let mut found = BTreeSet::new();
    if let Some(path) = search.path.as_ref() {
        for dir in std::env::split_paths(path) {
            if let Some(canonical) = canonicalize_file(&dir.join("grok")) {
                found.insert(canonical);
            }
        }
    }
    for location in &search.reviewed {
        if let Some(canonical) = canonicalize_file(location) {
            found.insert(canonical);
        }
    }

    match found.len() {
        0 => GrokResolution::Missing {
            reason: GROK_REASON_MISSING.to_string(),
        },
        1 => {
            let path = found.into_iter().next().expect("one");
            GrokResolution::Found {
                identity: identity_for(&path),
                path,
            }
        },
        count => GrokResolution::Ambiguous { count },
    }
}

#[derive(Debug, Clone)]
enum CachedGrokVersion {
    Parsed(String),
    Unparseable,
    TimedOut { at: Instant },
}

const GROK_VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn overlay_cached_grok_version(
    snapshot: GrokReadinessSnapshot,
    search: &GrokSearchPaths,
) -> GrokReadinessSnapshot {
    let identity = snapshot.identity.clone();
    let cached = grok_version_cache().get(&identity).cloned();
    let versioned = match cached {
        Some(CachedGrokVersion::Parsed(version)) => apply_grok_version(snapshot, Some(&version)),
        Some(CachedGrokVersion::Unparseable) => apply_grok_version(snapshot, None),
        Some(CachedGrokVersion::TimedOut { .. }) | None => snapshot,
    };
    overlay_grok_auth(versioned, search)
}

fn overlay_or_probe_grok_version(
    snapshot: GrokReadinessSnapshot,
    config: &MagicianGrokSettings,
    search: &GrokSearchPaths,
) -> GrokReadinessSnapshot {
    let identity = snapshot.identity.clone();
    let versioned = overlay_or_probe_grok_version_only(snapshot, config, search, &identity);
    overlay_grok_auth(versioned, search)
}

fn overlay_or_probe_grok_version_only(
    snapshot: GrokReadinessSnapshot,
    config: &MagicianGrokSettings,
    search: &GrokSearchPaths,
    identity: &str,
) -> GrokReadinessSnapshot {
    if let Some(cached) = grok_version_cache().get(identity).cloned() {
        match cached {
            CachedGrokVersion::Parsed(version) => {
                return apply_grok_version(snapshot, Some(&version));
            },
            CachedGrokVersion::Unparseable => return apply_grok_version(snapshot, None),
            CachedGrokVersion::TimedOut { at } if at.elapsed() < REFRESH_COALESCE => {
                return snapshot;
            },
            CachedGrokVersion::TimedOut { .. } => {},
        }
    }
    let Some(path) = resolved_grok_executable(config, search) else {
        return snapshot;
    };
    match probe_grok_cli_version(&path) {
        Some(raw) => {
            if let Some(version) = super::grok_contract::parse_grok_cli_version(&raw) {
                grok_version_cache().insert(
                    identity.to_string(),
                    CachedGrokVersion::Parsed(version.clone()),
                );
                apply_grok_version(snapshot, Some(&version))
            } else {
                grok_version_cache().insert(identity.to_string(), CachedGrokVersion::Unparseable);
                apply_grok_version(snapshot, None)
            }
        },
        None => {
            grok_version_cache().insert(
                identity.to_string(),
                CachedGrokVersion::TimedOut { at: Instant::now() },
            );
            snapshot
        },
    }
}

fn overlay_grok_auth(
    mut snapshot: GrokReadinessSnapshot,
    search: &GrokSearchPaths,
) -> GrokReadinessSnapshot {
    if snapshot.readiness != GrokReadiness::Ready {
        return snapshot;
    }
    let has_auth = grok_has_auth(search);
    grok_auth_cache().insert(snapshot.identity.clone(), has_auth);
    if has_auth {
        return snapshot;
    }
    snapshot.readiness = GrokReadiness::AuthRequired;
    snapshot.selectable = grok_is_selectable(snapshot.readiness);
    snapshot.reason = GROK_REASON_AUTH.to_string();
    snapshot
}

fn grok_has_auth(search: &GrokSearchPaths) -> bool {
    grok_auth_file_present(search) || grok_api_key_present(search)
}

fn grok_auth_file_present(search: &GrokSearchPaths) -> bool {
    grok_home_dir(search)
        .map(|home| home.join(".grok").join("auth.json").is_file())
        .unwrap_or(false)
}

fn grok_home_dir(search: &GrokSearchPaths) -> Option<PathBuf> {
    search.home.clone().or_else(dirs::home_dir)
}

fn grok_api_key_present(search: &GrokSearchPaths) -> bool {
    let inherited: Vec<(String, String)> = match search.env.as_ref() {
        Some(env) => env.clone(),
        None => std::env::vars().collect(),
    };
    let filtered = grok_version_probe_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    filtered
        .get("XAI_API_KEY")
        .is_some_and(|value| !value.trim().is_empty())
}

fn invalidate_grok_overlays(identity: &str) {
    grok_version_cache().remove(identity);
    grok_auth_cache().remove(identity);
    super::grok_qualification::invalidate_grok_receipt(identity);
}

fn overlay_cached_grok_attestation(snapshot: GrokReadinessSnapshot) -> GrokReadinessSnapshot {
    match snapshot.readiness {
        GrokReadiness::Ready | GrokReadiness::Unqualified => {},
        _ => return snapshot,
    }
    let version_ok = snapshot
        .version
        .as_deref()
        .is_some_and(super::grok_contract::grok_version_meets_minimum);
    if !version_ok && snapshot.readiness != GrokReadiness::Ready {
        return snapshot;
    }
    if let Some(receipt) = super::grok_qualification::cached_grok_receipt(snapshot.identity()) {
        return super::grok_qualification::overlay_grok_receipt(snapshot, &receipt, now_ms());
    }
    super::grok_qualification::demote_unattested_ready(snapshot)
}

fn apply_grok_version(
    mut snapshot: GrokReadinessSnapshot,
    version: Option<&str>,
) -> GrokReadinessSnapshot {
    match version {
        Some(version) if super::grok_contract::grok_version_meets_minimum(version) => {
            snapshot.readiness = GrokReadiness::Ready;
            snapshot.selectable = grok_is_selectable(snapshot.readiness);
            snapshot.version = Some(version.to_string());
            snapshot.reason = GROK_REASON_READY.to_string();
        },
        Some(version) => {
            snapshot.readiness = GrokReadiness::Incompatible;
            snapshot.selectable = false;
            snapshot.version = Some(version.to_string());
            snapshot.reason = format!(
                "Grok CLI {version} is below the {} minimum",
                super::grok_contract::GROK_ACP_MINIMUM_VERSION
            );
        },
        None => {
            snapshot.readiness = GrokReadiness::Incompatible;
            snapshot.selectable = false;
            snapshot.reason = "Grok CLI version was unparseable".to_string();
        },
    }
    snapshot
}

fn grok_version_probe_env<'a, I>(entries: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    super::grok::filter_grok_child_env(entries)
}

fn probe_grok_cli_version(path: &Path) -> Option<String> {
    let inherited: Vec<(String, String)> = std::env::vars().collect();
    let filtered = grok_version_probe_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    let mut command = std::process::Command::new(path);
    command
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env_clear();
    for (key, value) in &filtered {
        command.env(key, value);
    }
    let mut child = command.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < GROK_VERSION_PROBE_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            },
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            },
        }
    }
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut pipe, &mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
    }
    let text = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    Some(text)
}

fn grok_snapshot_slot() -> &'static RwLock<GrokReadinessSnapshot> {
    static SLOT: OnceLock<RwLock<GrokReadinessSnapshot>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(GrokReadinessSnapshot::default()))
}

fn grok_refresh_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn grok_last_refresh() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

fn grok_last_refresh_at() -> Option<Instant> {
    *grok_last_refresh()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn record_grok_refresh() {
    *grok_last_refresh()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
}

fn grok_version_cache() -> std::sync::MutexGuard<'static, HashMap<String, CachedGrokVersion>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedGrokVersion>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn grok_auth_cache() -> std::sync::MutexGuard<'static, HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn publish_grok_if_newer(next: GrokReadinessSnapshot) -> GrokReadinessSnapshot {
    let mut slot = grok_snapshot_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let kept = if next.revision <= slot.revision {
        slot.clone()
    } else {
        next
    };
    *slot = kept.clone();
    kept
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use super::*;

    fn settings(enabled: bool, binary: Option<&str>) -> MagicianCodexSettings {
        MagicianCodexSettings {
            enabled,
            binary: binary.map(str::to_string),
        }
    }

    fn isolated_search() -> CodexSearchPaths {
        CodexSearchPaths {
            path: Some(OsString::from("/no-such-codex-path")),
            reviewed: Vec::new(),
        }
    }

    #[test]
    fn kill_switch_disables_discovery() {
        let snapshot =
            resolve_codex_readiness(&settings(false, Some("/tmp/codex")), &isolated_search());
        assert_eq!(snapshot.readiness, CodexReadiness::Disabled);
        assert!(!snapshot.selectable);
        let public = serde_json::to_string(&snapshot).expect("json");
        assert!(!public.contains("/tmp"), "{public}");
    }

    #[test]
    fn missing_operator_binary_is_missing() {
        let snapshot = resolve_codex_readiness(
            &settings(true, Some("/no/such/codex-binary")),
            &isolated_search(),
        );
        assert_eq!(snapshot.readiness, CodexReadiness::Missing);
        assert!(
            snapshot.reason.contains("configured"),
            "{}",
            snapshot.reason
        );
        assert!(!snapshot.selectable);
    }

    #[test]
    fn one_reviewed_binary_is_unqualified_and_not_selectable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("codex");
        fs::write(&binary, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let search = CodexSearchPaths {
            path: Some(OsString::from("/no-such-codex-path")),
            reviewed: vec![binary.clone()],
        };
        let snapshot = resolve_codex_readiness(&settings(true, None), &search);
        assert_eq!(snapshot.readiness, CodexReadiness::Unqualified);
        assert!(!snapshot.selectable);
        assert!(codex_is_selectable(CodexReadiness::Ready));
        assert!(!codex_is_selectable(CodexReadiness::Unqualified));
        assert!(!codex_is_selectable(CodexReadiness::Disabled));
        let public = serde_json::to_string(&snapshot).expect("json");
        assert!(
            !public.contains(binary.to_string_lossy().as_ref()),
            "public snapshot must not leak the binary path: {public}"
        );
    }

    #[test]
    fn two_distinct_binaries_are_ambiguous_without_an_operator_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("a");
        let second = dir.path().join("b");
        fs::create_dir_all(&first).expect("a");
        fs::create_dir_all(&second).expect("b");
        let left = first.join("codex");
        let right = second.join("codex");
        fs::write(&left, b"left").expect("left");
        fs::write(&right, b"right").expect("right");
        let search = CodexSearchPaths {
            path: Some(OsString::from("/no-such-codex-path")),
            reviewed: vec![left, right],
        };
        let snapshot = resolve_codex_readiness(&settings(true, None), &search);
        assert_eq!(snapshot.readiness, CodexReadiness::AmbiguousInstallation);
        assert!(
            snapshot.reason.contains("coding.codex.binary"),
            "{}",
            snapshot.reason
        );
        assert!(!snapshot.selectable);
    }

    #[test]
    fn operator_path_wins_over_ambiguity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("a/codex");
        let second = dir.path().join("b/codex");
        fs::create_dir_all(first.parent().unwrap()).expect("a");
        fs::create_dir_all(second.parent().unwrap()).expect("b");
        fs::write(&first, b"left").expect("left");
        fs::write(&second, b"right").expect("right");
        let search = CodexSearchPaths {
            path: Some(OsString::from("/no-such-codex-path")),
            reviewed: vec![first.clone(), second],
        };
        let snapshot = resolve_codex_readiness(
            &settings(true, Some(first.to_str().expect("utf8"))),
            &search,
        );
        assert_eq!(snapshot.readiness, CodexReadiness::Unqualified);
    }

    #[test]
    fn projected_rows_keep_pi_shape_and_omit_codex_llm_profile() {
        let pi = CodingProfileInfo {
            id: "coding-balanced".to_string(),
            label: "Balanced".to_string(),
            llm_profile: "cheap".to_string(),
            provider: "openai".to_string(),
            model: "gpt".to_string(),
            supports_user_image_inputs: true,
            is_default: true,
            description: None,
        };
        let rows = project_coding_profiles(
            &[pi],
            &CodexReadinessSnapshot::default(),
            &GrokReadinessSnapshot::default(),
            &ClaudeReadinessSnapshot::default(),
            &AgyReadinessSnapshot::default(),
        );
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["engine"], "pi");
        assert_eq!(rows[0]["selectable"], true);
        assert_eq!(rows[0]["llm_profile"], "cheap");
        assert_eq!(rows[1]["id"], CODEX_DEFAULT_PROFILE_ID);
        assert_eq!(rows[1]["engine"], "codex_app_server");
        assert_eq!(rows[1]["selectable"], false);
        assert!(rows[1].get("llm_profile").is_none(), "{}", rows[1]);
        let encoded = rows[1].to_string();
        assert!(!encoded.contains("CODEX_HOME"), "{encoded}");
        assert_eq!(rows[2]["id"], GROK_DEFAULT_PROFILE_ID);
        assert_eq!(rows[2]["engine"], "grok_acp");
        assert_eq!(rows[2]["selectable"], false);
        assert_eq!(rows[2]["label"], "Grok");
        assert!(rows[2].get("llm_profile").is_none(), "{}", rows[2]);
        assert_eq!(rows[3]["id"], CLAUDE_DEFAULT_PROFILE_ID);
        assert_eq!(rows[3]["engine"], "claude_code");
        assert_eq!(rows[3]["selectable"], false);
        assert_eq!(rows[3]["label"], "Claude");
        assert!(rows[3].get("llm_profile").is_none(), "{}", rows[3]);
        assert_eq!(rows[4]["id"], AGY_DEFAULT_PROFILE_ID);
        assert_eq!(rows[4]["engine"], "agy_cli");
        assert_eq!(rows[4]["selectable"], false);
        assert_eq!(rows[4]["label"], "Antigravity");
        assert!(rows[4].get("llm_profile").is_none(), "{}", rows[4]);
        let claude_public =
            serde_json::to_string(&ClaudeReadinessSnapshot::default()).expect("json");
        assert!(!claude_public.contains("/opt"), "{claude_public}");
        assert!(!claude_public.contains("ANTHROPIC"), "{claude_public}");
        let agy_public = serde_json::to_string(&AgyReadinessSnapshot::default()).expect("json");
        assert!(!agy_public.contains("/opt"), "{agy_public}");
        assert!(!agy_public.contains("antigravity-oauth"), "{agy_public}");
    }

    #[test]
    fn stale_snapshot_cannot_overwrite_a_newer_revision() {
        let current = CodexReadinessSnapshot {
            revision: 4,
            identity: "new".to_string(),
            ..CodexReadinessSnapshot::default()
        };
        let stale = CodexReadinessSnapshot {
            revision: 3,
            identity: "old".to_string(),
            readiness: CodexReadiness::Ready,
            ..CodexReadinessSnapshot::default()
        };
        let kept = select_newer_snapshot(&current, stale);
        assert_eq!(kept.revision, 4);
        assert_eq!(kept.readiness, CodexReadiness::Disabled);
        assert_eq!(kept.identity, "new");
    }

    #[test]
    fn client_info_is_magician_not_a_first_party_codex_client() {
        let info = magician_codex_client_info();
        assert_eq!(info["name"], CODEX_CLIENT_NAME);
        assert_eq!(info["title"], CODEX_CLIENT_TITLE);
        let text = info.to_string();
        assert!(!text.to_ascii_lowercase().contains("vscode"), "{text}");
        assert!(!text.contains("codex-cli"), "{text}");
    }

    fn grok_settings(enabled: bool, binary: Option<&str>) -> MagicianGrokSettings {
        MagicianGrokSettings {
            enabled,
            binary: binary.map(str::to_string),
        }
    }

    fn isolated_grok_search() -> GrokSearchPaths {
        GrokSearchPaths {
            path: Some(OsString::from("/no-such-grok-path")),
            reviewed: Vec::new(),
            home: Some(PathBuf::from("/no-such-grok-home")),
            env: Some(Vec::new()),
        }
    }

    fn grok_search_reviewed(reviewed: Vec<PathBuf>) -> GrokSearchPaths {
        GrokSearchPaths {
            reviewed,
            ..isolated_grok_search()
        }
    }

    #[cfg(unix)]
    fn grok_home_with_auth(root: &std::path::Path) -> PathBuf {
        let home = root.join("home");
        fs::create_dir_all(home.join(".grok")).expect("grok dir");
        fs::write(home.join(".grok").join("auth.json"), b"{}\n").expect("auth");
        home
    }

    #[cfg(unix)]
    fn assert_public_omits_secrets(snapshot: &GrokReadinessSnapshot, leaks: &[&str]) {
        let public = serde_json::to_string(&snapshot.public_json()).expect("json");
        for leak in leaks {
            assert!(
                !public.contains(leak),
                "public snapshot must not leak {leak}: {public}"
            );
        }
        assert!(!public.contains("auth.json"), "{public}");
        assert!(!public.contains("XAI_API_KEY"), "{public}");
        assert!(!public.contains(".grok"), "{public}");
    }

    #[test]
    fn grok_kill_switch_disables_discovery() {
        let snapshot = resolve_grok_readiness(
            &grok_settings(false, Some("/tmp/grok")),
            &isolated_grok_search(),
        );
        assert_eq!(snapshot.readiness, GrokReadiness::Disabled);
        assert!(!snapshot.selectable);
        let public = serde_json::to_string(&snapshot).expect("json");
        assert!(!public.contains("/tmp"), "{public}");
    }

    #[test]
    fn grok_missing_operator_binary_is_missing() {
        let snapshot = resolve_grok_readiness(
            &grok_settings(true, Some("/no/such/grok-binary")),
            &isolated_grok_search(),
        );
        assert_eq!(snapshot.readiness, GrokReadiness::Missing);
        assert!(
            snapshot.reason.contains("configured"),
            "{}",
            snapshot.reason
        );
        assert!(!snapshot.selectable);
    }

    #[test]
    fn one_reviewed_grok_binary_is_unqualified_until_versioned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        fs::write(&binary, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let search = grok_search_reviewed(vec![binary.clone()]);
        let snapshot = resolve_grok_readiness(&grok_settings(true, None), &search);
        assert_eq!(snapshot.readiness, GrokReadiness::Unqualified);
        assert!(!snapshot.selectable);
        assert!(grok_is_selectable(GrokReadiness::Ready));
        assert!(!grok_is_selectable(GrokReadiness::Unqualified));
        assert!(!grok_is_selectable(GrokReadiness::AuthRequired));
        let public = serde_json::to_string(&snapshot).expect("json");
        assert!(
            !public.contains(binary.to_string_lossy().as_ref()),
            "public snapshot must not leak the binary path: {public}"
        );
    }

    #[test]
    fn two_distinct_grok_binaries_are_ambiguous_without_an_operator_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("a");
        let second = dir.path().join("b");
        fs::create_dir_all(&first).expect("a");
        fs::create_dir_all(&second).expect("b");
        let left = first.join("grok");
        let right = second.join("grok");
        fs::write(&left, b"left").expect("left");
        fs::write(&right, b"right").expect("right");
        let search = grok_search_reviewed(vec![left, right]);
        let snapshot = resolve_grok_readiness(&grok_settings(true, None), &search);
        assert_eq!(snapshot.readiness, GrokReadiness::AmbiguousInstallation);
        assert!(
            snapshot.reason.contains("coding.grok.binary"),
            "{}",
            snapshot.reason
        );
        assert!(!snapshot.selectable);
    }

    #[test]
    fn grok_operator_path_wins_over_ambiguity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("a/grok");
        let second = dir.path().join("b/grok");
        fs::create_dir_all(first.parent().unwrap()).expect("a");
        fs::create_dir_all(second.parent().unwrap()).expect("b");
        fs::write(&first, b"left").expect("left");
        fs::write(&second, b"right").expect("right");
        let search = grok_search_reviewed(vec![first.clone(), second]);
        let snapshot = resolve_grok_readiness(
            &grok_settings(true, Some(first.to_str().expect("utf8"))),
            &search,
        );
        assert_eq!(snapshot.readiness, GrokReadiness::Unqualified);
        assert!(!snapshot.selectable);
    }

    #[cfg(unix)]
    #[test]
    fn observe_marks_a_parseable_minimum_version_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        fs::write(&binary, b"#!/bin/sh\necho 1.0.5\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        let search = GrokSearchPaths {
            reviewed: vec![binary],
            home: Some(grok_home_with_auth(dir.path())),
            ..isolated_grok_search()
        };
        let snapshot = overlay_or_probe_grok_version(
            resolve_grok_readiness(&grok_settings(true, None), &search),
            &grok_settings(true, None),
            &search,
        );
        assert_eq!(snapshot.readiness, GrokReadiness::Ready);
        assert!(snapshot.selectable);
        assert_eq!(snapshot.version.as_deref(), Some("1.0.5"));
        let dir_s = dir.path().to_string_lossy().into_owned();
        let home_s = search
            .home
            .as_ref()
            .expect("home")
            .to_string_lossy()
            .into_owned();
        assert_public_omits_secrets(&snapshot, &[&dir_s, &home_s]);
    }

    #[cfg(unix)]
    #[test]
    fn observe_marks_an_old_grok_cli_incompatible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        fs::write(&binary, b"#!/bin/sh\necho 1.0.0\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        let search = GrokSearchPaths {
            reviewed: vec![binary],
            home: Some(grok_home_with_auth(dir.path())),
            ..isolated_grok_search()
        };
        let snapshot = overlay_or_probe_grok_version(
            resolve_grok_readiness(&grok_settings(true, None), &search),
            &grok_settings(true, None),
            &search,
        );
        assert_eq!(snapshot.readiness, GrokReadiness::Incompatible);
        assert!(!snapshot.selectable);
        let dir_s = dir.path().to_string_lossy().into_owned();
        assert_public_omits_secrets(&snapshot, &[&dir_s]);
    }

    #[cfg(unix)]
    #[test]
    fn version_ok_without_auth_is_auth_required() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        fs::write(&binary, b"#!/bin/sh\necho 1.0.5\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        let home = dir.path().join("empty-home");
        fs::create_dir_all(&home).expect("home");
        let search = GrokSearchPaths {
            reviewed: vec![binary.clone()],
            home: Some(home.clone()),
            env: Some(Vec::new()),
            ..isolated_grok_search()
        };
        let snapshot = overlay_or_probe_grok_version(
            resolve_grok_readiness(&grok_settings(true, None), &search),
            &grok_settings(true, None),
            &search,
        );
        assert_eq!(snapshot.readiness, GrokReadiness::AuthRequired);
        assert!(!snapshot.selectable);
        assert_eq!(snapshot.reason, GROK_REASON_AUTH);
        assert_eq!(snapshot.version.as_deref(), Some("1.0.5"));
        let binary_s = binary.to_string_lossy().into_owned();
        let home_s = home.to_string_lossy().into_owned();
        assert_public_omits_secrets(&snapshot, &[&binary_s, &home_s]);
    }

    #[cfg(unix)]
    #[test]
    fn version_ok_with_filtered_api_key_is_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        fs::write(&binary, b"#!/bin/sh\necho 1.0.5\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        let key = "xai-test-key-must-not-leak";
        let search = GrokSearchPaths {
            reviewed: vec![binary],
            home: Some(dir.path().join("empty-home")),
            env: Some(vec![("XAI_API_KEY".to_string(), key.to_string())]),
            ..isolated_grok_search()
        };
        let snapshot = overlay_or_probe_grok_version(
            resolve_grok_readiness(&grok_settings(true, None), &search),
            &grok_settings(true, None),
            &search,
        );
        assert_eq!(snapshot.readiness, GrokReadiness::Ready);
        assert!(snapshot.selectable);
        let dir_s = dir.path().to_string_lossy().into_owned();
        assert_public_omits_secrets(&snapshot, &[key, &dir_s]);
    }

    #[test]
    fn grok_identity_change_invalidates_version_and_auth_overlays() {
        let identity = "old-grok-identity";
        grok_version_cache().insert(
            identity.to_string(),
            CachedGrokVersion::Parsed("1.0.5".to_string()),
        );
        grok_auth_cache().insert(identity.to_string(), true);
        super::super::grok_qualification::cache_grok_receipt(
            super::super::grok_qualification::GrokQualificationReceipt {
                identity: identity.to_string(),
                readiness: GrokReadiness::Ready,
                reason: GROK_REASON_READY.to_string(),
                attestation_digest: Some("digest".to_string()),
                qualified_at_ms: 1,
                expires_at_ms: i64::MAX,
            },
        );
        invalidate_grok_overlays(identity);
        assert!(grok_version_cache().get(identity).is_none());
        assert!(grok_auth_cache().get(identity).is_none());
        assert!(super::super::grok_qualification::cached_grok_receipt(identity).is_none());
        let source = include_str!("discovery.rs");
        let start = source
            .find("pub fn observe_grok_readiness")
            .expect("observe_grok_readiness");
        let body = &source[start..];
        let end = body
            .find("pub fn refresh_grok_readiness")
            .expect("refresh follows observe");
        assert!(
            body[..end].contains("invalidate_grok_overlays"),
            "identity change must drop version and auth overlays"
        );
    }

    #[test]
    fn version_ok_without_attestation_receipt_is_not_selectable() {
        let snapshot = overlay_cached_grok_attestation(GrokReadinessSnapshot::ready_for_test(
            "grok-attest-missing",
        ));
        assert_eq!(snapshot.readiness, GrokReadiness::Unqualified);
        assert!(!snapshot.selectable);
        assert!(snapshot.reason.contains("isolation"), "{}", snapshot.reason);
        assert_eq!(snapshot.version.as_deref(), Some("1.0.5"));
    }

    #[test]
    fn matching_attestation_receipt_overlays_ready() {
        let identity = "grok-attest-ready";
        super::super::grok_qualification::invalidate_grok_receipt(identity);
        let receipt = super::super::grok_qualification::qualify_from_grok_evidence(
            super::super::grok_qualification::GrokQualifyEvidence {
                identity: identity.to_string(),
                initialize: json!({ "protocolVersion": 1 }),
                session: json!({
                    "sessionId": "sess-probe",
                    "mcpServers": [],
                    "tools": ["read_file", "bash"],
                }),
                session_id: Some("sess-probe".to_string()),
                cancelled: true,
                ..Default::default()
            },
        );
        super::super::grok_qualification::cache_grok_receipt(receipt);
        let snapshot =
            overlay_cached_grok_attestation(GrokReadinessSnapshot::ready_for_test(identity));
        assert_eq!(snapshot.readiness, GrokReadiness::Ready);
        assert!(snapshot.selectable);
        super::super::grok_qualification::invalidate_grok_receipt(identity);
    }

    #[test]
    fn mcp_attestation_receipt_overlays_incompatible() {
        let identity = "grok-attest-mcp";
        super::super::grok_qualification::invalidate_grok_receipt(identity);
        let receipt = super::super::grok_qualification::qualify_from_grok_evidence(
            super::super::grok_qualification::GrokQualifyEvidence {
                identity: identity.to_string(),
                initialize: json!({
                    "protocolVersion": 1,
                    "mcpServers": [{ "name": "github" }]
                }),
                session: json!({ "sessionId": "sess-probe", "mcpServers": [] }),
                session_id: Some("sess-probe".to_string()),
                cancelled: true,
                ..Default::default()
            },
        );
        super::super::grok_qualification::cache_grok_receipt(receipt);
        let snapshot =
            overlay_cached_grok_attestation(GrokReadinessSnapshot::ready_for_test(identity));
        assert_eq!(snapshot.readiness, GrokReadiness::Incompatible);
        assert!(!snapshot.selectable);
        assert!(snapshot.reason.contains("MCP"), "{}", snapshot.reason);
        super::super::grok_qualification::invalidate_grok_receipt(identity);
    }

    #[test]
    fn observe_grok_readiness_does_not_spawn_the_cli() {
        let source = include_str!("discovery.rs");
        let start = source
            .find("pub fn observe_grok_readiness")
            .expect("observe_grok_readiness");
        let after = &source[start..];
        let end = after
            .find("pub fn refresh_grok_readiness")
            .expect("refresh follows observe");
        let body = &after[..end];
        assert!(
            !body.contains("probe_grok_cli_version"),
            "filesystem observe must not spawn grok --version"
        );
        assert!(
            !body.contains("overlay_or_probe_grok_version"),
            "filesystem observe must not probe; overlay cache only"
        );
        assert!(body.contains("overlay_cached_grok_version"));
        assert!(body.contains("overlay_cached_grok_attestation"));
        assert!(body.contains("resolve_grok_readiness"));
        assert!(body.contains("invalidate_grok_overlays"));
        assert!(
            !body.contains("grok_qualify_over_stdio"),
            "filesystem observe must not ACP-initialize"
        );
        assert!(!body.contains("grok_qualify_child_stdio"));
        assert!(!body.contains("attest_grok_isolation"));
    }

    #[test]
    fn refresh_grok_readiness_does_not_spawn_the_cli() {
        let source = include_str!("discovery.rs");
        let start = source
            .find("pub fn refresh_grok_readiness")
            .expect("refresh_grok_readiness");
        let after = &source[start..];
        let end = after
            .find("pub(crate) fn probe_grok_version_in_background")
            .expect("probe helper follows refresh");
        let body = &after[..end];
        assert!(body.contains("observe_grok_readiness"));
        assert!(body.contains("REFRESH_COALESCE"));
        assert!(
            !body.contains("probe_grok_cli_version"),
            "refresh must not spawn grok --version"
        );
        assert!(!body.contains("overlay_or_probe_grok_version"));
        assert!(
            !body.contains("grok_qualify_over_stdio"),
            "refresh must not ACP-initialize"
        );
        assert!(!body.contains("attest_grok_isolation"));
    }

    #[test]
    fn grok_version_probe_env_drops_magician_secrets() {
        let filtered = grok_version_probe_env([
            ("PATH", "/usr/bin"),
            ("HOME", "/tmp"),
            ("MAGICIAN_ADMIN_TOKEN", "nope"),
            ("MAGICIAN_FOO", "secret"),
            ("CODEX_HOME", "/secret"),
        ]);
        assert_eq!(filtered.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert!(filtered.contains_key("HOME"));
        assert!(
            !filtered.contains_key("MAGICIAN_ADMIN_TOKEN"),
            "version probe must not inherit MAGICIAN_ADMIN_TOKEN: {filtered:?}"
        );
        assert!(!filtered.contains_key("MAGICIAN_FOO"));
        assert!(!filtered.contains_key("CODEX_HOME"));
    }

    #[test]
    fn probe_grok_cli_version_clears_then_rebuilds_env() {
        let source = include_str!("discovery.rs");
        let start = source
            .find("fn probe_grok_cli_version")
            .expect("probe_grok_cli_version");
        let body = &source[start..];
        let end = body.find("\nfn grok_snapshot_slot").unwrap_or(body.len());
        let probe = &body[..end];
        assert!(
            probe.contains("env_clear"),
            "grok --version must env_clear before rebuild"
        );
        assert!(
            probe.contains("grok_version_probe_env"),
            "grok --version must rebuild from the Grok child allowlist"
        );
    }

    #[test]
    fn list_coding_profiles_reads_the_grok_snapshot() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        let start = web
            .find("pub async fn list_coding_profiles")
            .expect("list_coding_profiles");
        let rest = &web[start..];
        let end = rest.find("\n    pub async fn ").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("observe_grok_readiness"),
            "GET /coding/profiles must observe filesystem + cached Grok overlays"
        );
        assert!(
            body.contains("observe_claude_readiness"),
            "GET /coding/profiles must observe filesystem Claude readiness"
        );
        assert!(
            body.contains("observe_agy_readiness"),
            "GET /coding/profiles must observe filesystem Agy readiness"
        );
        assert!(body.contains("project_coding_profiles"));
        assert!(
            !body.contains("probe_grok_cli_version"),
            "GET /coding/profiles must not spawn grok --version"
        );
        assert!(
            !body.contains("overlay_or_probe"),
            "GET /coding/profiles must not probe Grok version"
        );
        assert!(
            !body.contains("current_grok_readiness"),
            "GET /coding/profiles must observe, not only read the last published snapshot"
        );
        assert!(
            !body.contains("refresh_grok_readiness"),
            "GET /coding/profiles must not refresh/probe Grok on the Actix worker"
        );
        assert!(
            !body.contains("probe_grok_cli_version"),
            "GET /coding/profiles must not mention probe_grok_cli_version"
        );
        assert!(
            !body.contains("grok_qualify_over_stdio"),
            "GET /coding/profiles must not ACP-initialize"
        );
        assert!(!body.contains("attest_grok_isolation"));
    }

    #[test]
    fn refresh_grok_acp_handler_does_not_probe_cli_version() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        let start = web
            .find("pub async fn refresh_grok_acp")
            .expect("refresh_grok_acp");
        let rest = &web[start..];
        let end = rest.find("\n    pub async fn ").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("refresh_grok_readiness"),
            "POST /coding/engines/grok_acp/refresh must observe filesystem via refresh_grok_readiness"
        );
        assert!(body.contains("Accepted"));
        assert!(body.contains("checking"));
        assert!(
            !body.contains("probe_grok_cli_version"),
            "refresh handler must not spawn grok --version"
        );
        assert!(!body.contains("overlay_or_probe"));
        assert!(!body.contains("tick_grok_version_worker"));
        assert!(!body.contains("grok_qualify_over_stdio"));
        assert!(!body.contains("grok_qualify_child_stdio"));
        assert!(!body.contains("attest_grok_isolation"));
        let handler_start = web
            .find("pub async fn refresh_grok_acp_handler")
            .expect("refresh_grok_acp_handler");
        let handler = &web[handler_start..];
        let handler_end = handler.find("\npub async fn ").unwrap_or(handler.len());
        assert!(!handler[..handler_end].contains("probe_grok_cli_version"));
        assert!(!handler[..handler_end].contains("grok_qualify_over_stdio"));
        assert!(!handler[..handler_end].contains("attest_grok_isolation"));
        let bin = include_str!("../../../../../magician-bin/src/main.rs");
        assert!(
            bin.contains("/coding/engines/grok_acp/refresh"),
            "magician-bin must mount the Grok refresh route"
        );
        assert!(bin.contains("refresh_grok_acp_handler"));
    }

    #[test]
    fn grok_reason_copy_covers_every_state_without_secrets() {
        let states = [
            GrokReadiness::Disabled,
            GrokReadiness::Missing,
            GrokReadiness::Checking,
            GrokReadiness::AmbiguousInstallation,
            GrokReadiness::Unqualified,
            GrokReadiness::Incompatible,
            GrokReadiness::AuthRequired,
            GrokReadiness::Ready,
        ];
        for readiness in states {
            let reason = grok_reason_for(readiness);
            assert!(!reason.is_empty(), "{readiness:?}");
            assert!(!reason.contains('@'), "{reason}");
            assert!(!reason.contains("auth.json"), "{reason}");
            assert!(!reason.contains("XAI_API_KEY"), "{reason}");
            assert!(!reason.contains("/Users"), "{reason}");
            assert!(!reason.contains("/.grok"), "{reason}");
            assert!(!reason.contains("gmail"), "{reason}");
            assert!(!reason.contains("http"), "{reason}");
        }
        assert_eq!(
            grok_reason_for(GrokReadiness::AuthRequired),
            GROK_REASON_AUTH
        );
        assert_eq!(
            grok_reason_for(GrokReadiness::Checking),
            GROK_REASON_CHECKING
        );
    }

    fn claude_settings(enabled: bool, binary: Option<&str>) -> MagicianClaudeSettings {
        MagicianClaudeSettings {
            enabled,
            binary: binary.map(str::to_string),
            use_api_key: false,
        }
    }

    fn isolated_claude_search() -> ClaudeSearchPaths {
        ClaudeSearchPaths {
            path: Some(OsString::from("/no-such-claude-path")),
            reviewed: Vec::new(),
            home: None,
            env: Some(Vec::new()),
        }
    }

    fn claude_search_with_binary(binary: PathBuf, home: Option<PathBuf>) -> ClaudeSearchPaths {
        ClaudeSearchPaths {
            path: Some(OsString::from("/no-such-claude-path")),
            reviewed: vec![binary],
            home,
            env: Some(Vec::new()),
        }
    }

    #[test]
    fn claude_kill_switch_disables_discovery() {
        let snapshot = resolve_claude_readiness(
            &claude_settings(false, Some("/tmp/claude")),
            &isolated_claude_search(),
        );
        assert_eq!(snapshot.readiness, ClaudeReadiness::Disabled);
        assert!(!snapshot.selectable);
        let public = serde_json::to_string(&snapshot.public_json()).expect("json");
        assert!(!public.contains("/tmp"), "{public}");
        assert!(!public.contains("ANTHROPIC"), "{public}");
    }

    fn write_executable_claude(dir: &Path) -> PathBuf {
        let binary = dir.join("claude");
        fs::write(&binary, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        binary
    }

    fn write_versioned_claude(dir: &Path, version: &str) -> PathBuf {
        let binary = dir.join("claude");
        fs::write(&binary, format!("#!/bin/sh\necho {version}\n")).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        binary
    }

    #[test]
    fn a_found_claude_binary_is_unqualified_until_version_and_isolation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = write_executable_claude(dir.path());
        let home = dir.path().join("home");
        fs::create_dir_all(&home).expect("home");
        fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"accountUuid":"not-a-secret-id"}}"#,
        )
        .expect("oauth");
        let snapshot = resolve_claude_readiness(
            &claude_settings(true, None),
            &claude_search_with_binary(binary, Some(home)),
        );
        assert_eq!(snapshot.readiness, ClaudeReadiness::Unqualified);
        assert!(!snapshot.selectable);
        assert_eq!(snapshot.reason, CLAUDE_REASON_UNQUALIFIED_VERSION);
    }

    #[cfg(unix)]
    #[test]
    fn a_claude_binary_without_oauth_is_auth_required_even_if_anthropic_key_is_in_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = write_versioned_claude(dir.path(), "2.1.229");
        let closed_home = dir.path().join("no-home");
        fs::create_dir_all(&closed_home).expect("home");
        let mut search = claude_search_with_binary(binary.clone(), Some(closed_home));
        search.env = Some(vec![(
            "ANTHROPIC_API_KEY".to_string(),
            "sk-secret-must-not-count".to_string(),
        )]);
        let snapshot = overlay_or_probe_claude_version(
            resolve_claude_readiness(&claude_settings(true, None), &search),
            &claude_settings(true, None),
            &search,
        );
        assert_eq!(snapshot.readiness, ClaudeReadiness::AuthRequired);
        assert!(!snapshot.selectable);
        let public = serde_json::to_string(&snapshot.public_json()).expect("json");
        assert!(!public.contains("sk-secret"), "{public}");
        assert!(!public.contains("ANTHROPIC"), "{public}");
        assert!(
            !public.contains(binary.to_string_lossy().as_ref()),
            "{public}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn claude_oauth_plus_version_without_isolation_receipt_is_not_selectable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = write_versioned_claude(dir.path(), "2.1.229");
        let home = dir.path().join("home");
        fs::create_dir_all(&home).expect("home");
        fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"accountUuid":"not-a-secret-id"}}"#,
        )
        .expect("oauth");
        let search = claude_search_with_binary(binary, Some(home));
        let versioned = overlay_or_probe_claude_version(
            resolve_claude_readiness(&claude_settings(true, None), &search),
            &claude_settings(true, None),
            &search,
        );
        assert_eq!(versioned.readiness, ClaudeReadiness::Ready);
        let snapshot = overlay_cached_claude_attestation(versioned);
        assert_eq!(snapshot.readiness, ClaudeReadiness::Unqualified);
        assert!(!snapshot.selectable);
        assert_eq!(snapshot.reason, CLAUDE_REASON_UNQUALIFIED_ISOLATION);
    }

    #[cfg(unix)]
    #[test]
    fn claude_oauth_version_and_isolation_receipt_are_ready_without_an_api_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = write_versioned_claude(dir.path(), "2.1.229");
        let home = dir.path().join("home");
        fs::create_dir_all(&home).expect("home");
        fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"accountUuid":"not-a-secret-id"}}"#,
        )
        .expect("oauth");
        let search = claude_search_with_binary(binary, Some(home));
        let settings = claude_settings(true, None);
        let versioned = overlay_or_probe_claude_version(
            resolve_claude_readiness(&settings, &search),
            &settings,
            &search,
        );
        super::super::claude_qualification::cache_claude_receipt(
            super::super::claude_qualification::ClaudeQualificationReceipt {
                identity: versioned.identity().to_string(),
                version: versioned.version.clone(),
                readiness: ClaudeReadiness::Ready,
                reason: CLAUDE_REASON_READY.to_string(),
                attestation_digest: Some("digest".to_string()),
                qualified_at_ms: 1,
                expires_at_ms: i64::MAX,
            },
        );
        let snapshot = overlay_cached_claude_attestation(versioned);
        assert_eq!(snapshot.readiness, ClaudeReadiness::Ready);
        assert!(snapshot.selectable);
        let public = serde_json::to_string(&snapshot.public_json()).expect("json");
        assert!(!public.contains("ANTHROPIC"), "{public}");
        assert!(!public.contains("/Users"), "{public}");
    }

    #[cfg(unix)]
    #[test]
    fn claude_use_api_key_accepts_parent_key_and_still_omits_it_from_public_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = write_versioned_claude(dir.path(), "2.1.229");
        let closed_home = dir.path().join("no-home");
        fs::create_dir_all(&closed_home).expect("home");
        let mut search = claude_search_with_binary(binary, Some(closed_home));
        search.env = Some(vec![(
            "ANTHROPIC_API_KEY".to_string(),
            "sk-secret-key".to_string(),
        )]);
        let mut settings = claude_settings(true, None);
        settings.use_api_key = true;
        let snapshot = overlay_or_probe_claude_version(
            resolve_claude_readiness(&settings, &search),
            &settings,
            &search,
        );
        assert_eq!(snapshot.readiness, ClaudeReadiness::Ready);
        let public = serde_json::to_string(&snapshot.public_json()).expect("json");
        assert!(!public.contains("sk-secret"), "{public}");
        assert!(!public.contains("ANTHROPIC"), "{public}");
    }

    #[cfg(unix)]
    #[test]
    fn old_claude_cli_is_incompatible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = write_versioned_claude(dir.path(), "2.1.228");
        let search = claude_search_with_binary(binary, None);
        let snapshot = overlay_or_probe_claude_version(
            resolve_claude_readiness(&claude_settings(true, None), &search),
            &claude_settings(true, None),
            &search,
        );
        assert_eq!(snapshot.readiness, ClaudeReadiness::Incompatible);
        assert!(!snapshot.selectable);
        assert!(snapshot.reason.contains("2.1.228"), "{}", snapshot.reason);
    }

    #[test]
    fn observe_claude_readiness_does_not_spawn_the_cli() {
        let source = include_str!("discovery.rs");
        let start = source
            .find("pub fn observe_claude_readiness")
            .expect("observe_claude_readiness");
        let body = &source[start..];
        let end = body
            .find("pub fn current_claude_readiness")
            .expect("current follows observe");
        assert!(body[..end].contains("overlay_cached_claude_version"));
        assert!(body[..end].contains("overlay_cached_claude_attestation"));
        assert!(!body[..end].contains("probe_claude_cli_version"));
        assert!(!body[..end].contains("overlay_or_probe_claude_version("));
    }

    #[test]
    fn refresh_claude_readiness_does_not_spawn_the_cli() {
        let source = include_str!("discovery.rs");
        let start = source
            .find("pub fn refresh_claude_readiness")
            .expect("refresh_claude_readiness");
        let body = &source[start..];
        let end = body
            .find("pub(crate) fn probe_claude_version_in_background")
            .expect("probe follows refresh");
        assert!(body[..end].contains("observe_claude_readiness"));
        assert!(!body[..end].contains("probe_claude_cli_version"));
        assert!(!body[..end].contains("overlay_or_probe_claude_version("));
    }

    #[test]
    fn claude_identity_change_invalidates_version_and_isolation_overlays() {
        let identity = "old-claude-identity";
        claude_version_cache().insert(
            identity.to_string(),
            CachedClaudeVersion::Parsed("2.1.229".to_string()),
        );
        super::super::claude_qualification::cache_claude_receipt(
            super::super::claude_qualification::ClaudeQualificationReceipt {
                identity: identity.to_string(),
                version: Some("2.1.229".to_string()),
                readiness: ClaudeReadiness::Ready,
                reason: CLAUDE_REASON_READY.to_string(),
                attestation_digest: Some("digest".to_string()),
                qualified_at_ms: 1,
                expires_at_ms: i64::MAX,
            },
        );
        invalidate_claude_overlays(identity);
        assert!(claude_version_cache().get(identity).is_none());
        assert!(super::super::claude_qualification::cached_claude_receipt(identity).is_none());
        let source = include_str!("discovery.rs");
        let start = source
            .find("pub fn observe_claude_readiness")
            .expect("observe_claude_readiness");
        let body = &source[start..];
        let end = body
            .find("pub fn current_claude_readiness")
            .expect("current follows observe");
        assert!(
            body[..end].contains("invalidate_claude_overlays"),
            "identity change must drop version and isolation overlays"
        );
    }

    #[test]
    fn refresh_claude_code_handler_does_not_probe_cli_version() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        let start = web
            .find("pub async fn refresh_claude_code")
            .expect("refresh_claude_code");
        let rest = &web[start..];
        let end = rest.find("\n    pub async fn ").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("refresh_claude_readiness"),
            "POST /coding/engines/claude_code/refresh must observe filesystem via refresh_claude_readiness"
        );
        assert!(body.contains("Accepted"));
        assert!(body.contains("checking"));
        assert!(
            !body.contains("probe_claude_cli_version"),
            "refresh handler must not spawn claude --version"
        );
        assert!(!body.contains("overlay_or_probe"));
        assert!(!body.contains("tick_claude_version_worker"));
        assert!(!body.contains("claude_qualify_over_stdio"));
        assert!(!body.contains("claude_qualify_child_stdio"));
        let handler_start = web
            .find("pub async fn refresh_claude_code_handler")
            .expect("refresh_claude_code_handler");
        let handler = &web[handler_start..];
        let handler_end = handler.find("\npub async fn ").unwrap_or(handler.len());
        assert!(!handler[..handler_end].contains("probe_claude_cli_version"));
        assert!(!handler[..handler_end].contains("claude_qualify_over_stdio"));
        let bin = include_str!("../../../../../magician-bin/src/main.rs");
        assert!(
            bin.contains("/coding/engines/claude_code/refresh"),
            "magician-bin must mount the Claude refresh route"
        );
        assert!(bin.contains("spawn_claude_version_worker()"));
    }

    #[test]
    fn two_distinct_claude_binaries_are_ambiguous_without_an_operator_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("a");
        let second = dir.path().join("b");
        fs::create_dir_all(&first).expect("a");
        fs::create_dir_all(&second).expect("b");
        let left = first.join("claude");
        let right = second.join("claude");
        fs::write(&left, b"left").expect("left");
        fs::write(&right, b"right").expect("right");
        let search = ClaudeSearchPaths {
            path: Some(OsString::from("/no-such-claude-path")),
            reviewed: vec![left, right],
            home: None,
            env: Some(Vec::new()),
        };
        let snapshot = resolve_claude_readiness(&claude_settings(true, None), &search);
        assert_eq!(snapshot.readiness, ClaudeReadiness::AmbiguousInstallation);
        assert!(
            snapshot.reason.contains("coding.claude.binary"),
            "{}",
            snapshot.reason
        );
        assert!(!snapshot.selectable);
    }

    #[test]
    fn claude_public_json_omits_paths() {
        let snapshot = ClaudeReadinessSnapshot::ready_for_test("/Users/me/.local/bin/claude");
        let public = serde_json::to_string(&snapshot.public_json()).expect("json");
        assert!(!public.contains("/Users"), "{public}");
        assert!(!public.contains("/opt"), "{public}");
        assert!(!public.contains("ANTHROPIC"), "{public}");
        assert!(!public.contains(".local/bin"), "{public}");
        assert_eq!(snapshot.public_json()["selectable"], true);
        assert_eq!(snapshot.public_json()["readiness"], "ready");
    }

    fn agy_settings(enabled: bool, binary: Option<&str>) -> MagicianAgySettings {
        MagicianAgySettings {
            enabled,
            binary: binary.map(str::to_string),
            use_api_key: false,
        }
    }

    #[test]
    fn two_distinct_agy_binaries_are_ambiguous_without_an_operator_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("a");
        let second = dir.path().join("b");
        fs::create_dir_all(&first).expect("a");
        fs::create_dir_all(&second).expect("b");
        let left = first.join("agy");
        let right = second.join("agy");
        fs::write(&left, b"left").expect("left");
        fs::write(&right, b"right").expect("right");
        let search = AgySearchPaths {
            path: Some(OsString::from("/no-such-agy-path")),
            reviewed: vec![left, right],
            home: None,
            env: Some(Vec::new()),
        };
        let snapshot = resolve_agy_readiness(&agy_settings(true, None), &search);
        assert_eq!(snapshot.readiness, AgyReadiness::AmbiguousInstallation);
        assert!(
            snapshot.reason.contains("coding.agy.binary"),
            "{}",
            snapshot.reason
        );
        assert!(!snapshot.selectable);
    }

    #[test]
    fn agy_public_json_omits_paths() {
        let snapshot = AgyReadinessSnapshot::ready_for_test("/Users/me/.local/bin/agy");
        let public = serde_json::to_string(&snapshot.public_json()).expect("json");
        assert!(!public.contains("/Users"), "{public}");
        assert!(!public.contains("/opt"), "{public}");
        assert!(!public.contains("antigravity-oauth"), "{public}");
        assert!(!public.contains(".gemini"), "{public}");
        assert!(!public.contains(".local/bin"), "{public}");
        assert_eq!(snapshot.public_json()["selectable"], true);
        assert_eq!(snapshot.public_json()["readiness"], "ready");
        assert_eq!(
            agy_reason_for(AgyReadiness::AuthRequired),
            "sign in required"
        );
    }

    #[test]
    fn agy_identity_change_invalidates_version_and_isolation_overlays() {
        let identity = "old-agy-identity";
        agy_version_cache().insert(
            identity.to_string(),
            CachedAgyVersion::Parsed("1.1.19".to_string()),
        );
        super::super::agy_qualification::cache_agy_receipt(
            super::super::agy_qualification::AgyQualificationReceipt {
                identity: identity.to_string(),
                version: Some("1.1.19".to_string()),
                readiness: AgyReadiness::Ready,
                reason: AGY_REASON_READY.to_string(),
                attestation_digest: Some("digest".to_string()),
                qualified_at_ms: 1,
                expires_at_ms: i64::MAX,
            },
        );
        invalidate_agy_overlays(identity);
        assert!(agy_version_cache().get(identity).is_none());
        assert!(super::super::agy_qualification::cached_agy_receipt(identity).is_none());
        let source = include_str!("discovery.rs");
        let start = source
            .find("pub fn observe_agy_readiness")
            .expect("observe_agy_readiness");
        let body = &source[start..];
        let end = body
            .find("pub fn current_agy_readiness")
            .expect("current follows observe");
        assert!(
            body[..end].contains("invalidate_agy_overlays"),
            "identity change must drop version and isolation overlays"
        );
    }

    #[test]
    fn agy_identity_changes_when_the_binary_is_replaced_in_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("agy");
        fs::write(&binary, b"agy-v1").expect("write");
        let canonical = binary.canonicalize().expect("canonical binary");
        let first = agy_identity_for(&canonical);
        let again = agy_identity_for(&canonical);
        assert_eq!(first, again, "stable identity for an unchanged file");
        fs::write(&binary, b"agy-v2-replaced-in-place").expect("replace");
        let second = agy_identity_for(&canonical);
        assert_ne!(
            first, second,
            "in-place replace must not reuse the Ready identity"
        );
        assert_ne!(first, identity_for(&binary));
        let search = AgySearchPaths {
            path: Some(OsString::from("/no-such-agy-path")),
            reviewed: vec![binary],
            home: None,
            env: Some(Vec::new()),
        };
        let snapshot = resolve_agy_readiness(&agy_settings(true, None), &search);
        assert_eq!(snapshot.identity(), second.as_str());
        let source = include_str!("discovery.rs");
        let start = source
            .find("fn resolve_agy_binary")
            .expect("resolve_agy_binary");
        let body = &source[start..];
        let end = body
            .find("fn agy_snapshot_slot")
            .expect("slot follows resolve");
        assert!(
            body[..end].contains("agy_identity_for"),
            "Agy resolution must use path+mtime+len, not path-only identity_for"
        );
        assert!(!body[..end].contains("identity: identity_for("));
    }

    #[test]
    fn refresh_agy_cli_handler_does_not_probe_cli_version() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        let start = web
            .find("pub async fn refresh_agy_cli")
            .expect("refresh_agy_cli");
        let rest = &web[start..];
        let end = rest.find("\n    pub async fn ").unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("refresh_agy_readiness"),
            "POST /coding/engines/agy_cli/refresh must observe filesystem via refresh_agy_readiness"
        );
        assert!(body.contains("Accepted"));
        assert!(body.contains("checking"));
        assert!(
            !body.contains("probe_agy_cli_version"),
            "refresh handler must not spawn agy --version"
        );
        assert!(!body.contains("overlay_or_probe"));
        assert!(!body.contains("tick_agy_version_worker"));
        assert!(!body.contains("agy_qualify_over_stdio"));
        assert!(!body.contains("agy_qualify_child_stdio"));
        let handler_start = web
            .find("pub async fn refresh_agy_cli_handler")
            .expect("refresh_agy_cli_handler");
        let handler = &web[handler_start..];
        let handler_end = handler.find("\npub async fn ").unwrap_or(handler.len());
        assert!(!handler[..handler_end].contains("probe_agy_cli_version"));
        assert!(!handler[..handler_end].contains("agy_qualify_over_stdio"));
        let bin = include_str!("../../../../../magician-bin/src/main.rs");
        assert!(
            bin.contains("/coding/engines/agy_cli/refresh"),
            "magician-bin must mount the Agy refresh route"
        );
        assert!(bin.contains("spawn_agy_version_worker()"));
    }

    #[test]
    fn refresh_agy_readiness_does_not_spawn_version() {
        let source = include_str!("discovery.rs");
        let start = source
            .find("pub fn refresh_agy_readiness")
            .expect("refresh_agy_readiness");
        let body = &source[start..];
        let end = body
            .find("pub(crate) fn probe_agy_version_in_background")
            .expect("probe follows refresh");
        assert!(body[..end].contains("observe_agy_readiness"));
        assert!(!body[..end].contains("probe_agy_cli_version"));
        assert!(!body[..end].contains("overlay_or_probe_agy_version("));
    }
}
