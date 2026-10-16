//! Sessions, API tokens, and terminal grants — the expiring-credential
//! half of the store.
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md` §2.4.
//! One subsystem, three mint paths (identity doc §7): a login session, an
//! API token, and a terminal grant (plane plan Task 10, landed here so the
//! plane adopts the store instead of re-persisting). All are random
//! opaque values shown once, stored **hashed**, revoked by deleting their
//! row. The token *value* is never persisted.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::credentials::AuthMethod;

/// Bearer prefixes and what each engraves (workspace design §7C).
/// Classification must check the **longer** prefix first: `mag_pat_…`
/// starts with `mag_`.
pub const SESSION_TOKEN_PREFIX: &str = "mag_";
pub const API_TOKEN_PREFIX: &str = "mag_pat_";
pub const GRANT_TOKEN_PREFIX: &str = "plt_";

/// Which store a bearer value resolves against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Session,
    ApiToken,
    /// A `mag_bot_` bot-daemon token — resolves against the in-memory
    /// [`super::bot_tokens::BotTokenRegistry`], never against `tokens.json`.
    Bot,
    /// A `plt_` terminal grant — resolves against the grant rows of the
    /// same `tokens.json` via `AuthStore::resolve_grant`.
    Grant,
    Unknown,
}

/// Longest-prefix-first — see the const docs above.
pub fn classify_token(value: &str) -> TokenKind {
    let value = value.trim();
    if value.starts_with(API_TOKEN_PREFIX) {
        TokenKind::ApiToken
    // Before the session arm: `mag_bot_` starts with `mag_`, so testing the
    // session prefix first would classify every bot token as a session.
    } else if value.starts_with(super::bot_tokens::BOT_TOKEN_PREFIX) {
        TokenKind::Bot
    } else if value.starts_with(SESSION_TOKEN_PREFIX) {
        TokenKind::Session
    } else if value.starts_with(GRANT_TOKEN_PREFIX) {
        TokenKind::Grant
    } else {
        TokenKind::Unknown
    }
}

/// Mint an opaque bearer value: `prefix` + 43 chars of URL-safe base64 over
/// 32 bytes from the OS RNG. 256 bits of entropy — guessing is not a threat
/// model that matters.
pub fn mint_token_value(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// How tokens are stored: SHA-256 hex of the full value including prefix.
/// A stolen `sessions.json` does not contain usable credentials.
pub fn hash_token(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// A login session. Persisted (desktop installs stay logged in across
/// restarts); the token value is stored only as `token_hash`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub token_hash: String,
    pub identity: String,
    /// Scope engraved when the bearer is minted. Older rows predate scoped
    /// bearers and migrate to the historical `default` workspace on read.
    #[serde(default = "default_token_workspace")]
    pub workspace: String,
    pub method: AuthMethod,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A `mag_pat_` API token minted from the settings surface. No expiry by
/// default; revocation is deletion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiToken {
    pub id: Uuid,
    pub token_hash: String,
    pub identity: String,
    /// Scope engraved when the bearer is minted. A PAT is never a
    /// per-request workspace selector.
    #[serde(default = "default_token_workspace")]
    pub workspace: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<DateTime<Utc>>,
}

/// Tools no terminal grant may ever reach, whatever its allowlist says
/// (plane plan Task 2's floor, carried into the durable store by Task 10).
/// These families spawn harnesses carrying their own credentials — an
/// ungoverned harness inside a governed one. Mint **filters** these names
/// out of `allowed_tools` (reported, not silently dropped — see
/// [`floored_tools`]) and [`TerminalGrant::permits`] re-checks: the floor
/// is not negotiable by minting.
pub const NEVER_ON_THE_PLANE: &[&str] = &[
    // Arbitrary command execution can invoke every harness named below (or a
    // renamed copy), so filtering only their direct tool names is not a floor.
    // Plane consumers retain governed file/HTTP leaves, but never raw shell.
    "shell",
    // Coding-hot names that spawn Pi, Codex, or Grok with their own
    // citizen tokens.
    "run_coding_task",
    "apply_code_proposal",
    "run_project_checks",
    // CLI-delegate packs: a revival must not silently nest an ungoverned
    // CLI (with its native tools) inside a governed one.
    "claude",
    "codex",
    "agy",
    "opencode",
    "list_proposals",
    "interactive_process",
];

/// Grant lifetime bounds in hours: durable, but bounded — one hour to a
/// hard 90-day ceiling. There are no perpetual grants.
pub const GRANT_TTL_MIN_HOURS: u64 = 1;
pub const GRANT_TTL_MAX_HOURS: u64 = 2160;

/// A `plt_` terminal grant — the third mint path (plane plan Task 10).
/// Like every credential here: an opaque value shown once, stored hashed,
/// revoked by deleting its row — and, unlike API tokens, always expiring.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TerminalGrant {
    pub id: Uuid,
    /// SHA-256 hex of the full `plt_…` value — never the value.
    pub token_hash: String,
    /// The minting session's identity name — never client-chosen.
    pub identity: String,
    /// ENGRAVED at mint; validated against the ownership registry at mint
    /// and re-validated on every resolve. Not a per-request choice.
    pub workspace: String,
    /// Shown in the minting surface, e.g. "laptop terminal".
    pub label: String,
    /// The agent identity the grant acts as.
    pub agent_identity: String,
    /// `"claude_code" | "magician" | …` — who thinks when this grant calls
    /// tools. Client-supplied and REQUIRED: pinning `"magician"` is a
    /// deliberate anti-pattern pin, never a silent fallback.
    pub harness_engine: String,
    /// Empty = unscoped-but-floored (see [`TerminalGrant::permits`]) —
    /// never "all of the dangerous catalog".
    pub allowed_tools: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// `created_at + ttl_hours` (1..=2160) — required; grants are expiring
    /// credentials.
    pub expires_at: DateTime<Utc>,
    /// Grant ceilings for runs this grant starts (plane Task 6b/10).
    /// `None` leaves the run's ordinary limits in force.
    #[serde(default)]
    pub max_usd: Option<f64>,
    #[serde(default)]
    pub max_wall_clock_secs: Option<u64>,
    /// `None` means the door-side default concurrency ceiling applies.
    #[serde(default)]
    pub max_concurrent_runs: Option<usize>,
}

impl TerminalGrant {
    /// The dispatch-time check (the plane lowers a `tools/call` only when
    /// this returns `true`):
    ///
    /// * `allowed_tools` non-empty → the tool must be listed **and** not
    ///   floored;
    /// * `allowed_tools` empty → everything **except** the floor. Empty is
    ///   "the catalog minus the denied families", never "everything".
    ///
    /// The floor re-check is belt and braces: mint already filtered these
    /// names, but no hand-edited or future-minted record widens past it
    /// through this method.
    pub fn permits(&self, tool: &str) -> bool {
        if NEVER_ON_THE_PLANE.contains(&tool) {
            return false;
        }
        self.allowed_tools.is_empty() || self.allowed_tools.iter().any(|allowed| allowed == tool)
    }
}

/// Names the floor removes from a requested allowlist. Mint reports these
/// in the response (`floor_filtered_tools`) so the minting surface can show
/// what was dropped — the drop itself is never negotiable.
pub fn floored_tools(requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|tool| NEVER_ON_THE_PLANE.contains(&tool.as_str()))
        .cloned()
        .collect()
}

/// Mint input for [`TerminalGrant`] — everything the caller chooses; the
/// store derives `id`, `token_hash`, `identity`, `created_at`, and
/// `expires_at` from `ttl_hours`.
#[derive(Debug, Clone)]
pub struct MintGrantSpec {
    pub label: String,
    pub workspace: String,
    pub agent_identity: String,
    pub harness_engine: String,
    pub allowed_tools: Vec<String>,
    pub ttl_hours: u64,
    /// Grant ceilings for runs this grant starts; `None` leaves the run's
    /// ordinary limits in force.
    pub max_usd: Option<f64>,
    pub max_wall_clock_secs: Option<u64>,
    pub max_concurrent_runs: Option<usize>,
}

/// `sessions.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionsFile {
    pub schema_version: u32,
    #[serde(default)]
    pub sessions: Vec<Session>,
}

/// `tokens.json` — API tokens and terminal grants; the plane's Task 10
/// grants live in this same file per workspace design §8 note 1 (one
/// subsystem, three mint paths). `grants` is `#[serde(default)]` so
/// pre-grant files open unchanged, and unknown fields stay tolerated on
/// read like every auth document.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokensFile {
    pub schema_version: u32,
    #[serde(default)]
    pub tokens: Vec<ApiToken>,
    #[serde(default)]
    pub grants: Vec<TerminalGrant>,
}

/// What a resolved bearer proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BearerKind {
    Session(AuthMethod),
    ApiToken,
    /// A `plt_` terminal grant. The workspace travels on the kind because
    /// it was **engraved at mint** — the middleware refuses any selector
    /// that differs from it instead of honoring a per-request choice.
    Grant {
        workspace: String,
    },
    /// A `mag_bot_` token the runtime minted for a bot daemon it spawned.
    /// Its scope was engraved at spawn from the bot's own config directory,
    /// and no login identity stands behind it — the runtime is the minter.
    /// See `bot_tokens` for the lifetime rules.
    Bot {
        /// Bot name as keyed in `bot_configs.yaml` (`telegram`, `kapso`, …).
        bot_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerIdentity {
    /// The identity name the bearer resolves to — the only principal any
    /// downstream scope may be built from.
    pub identity: String,
    /// Workspace proved by the bearer record, never copied from the request.
    pub workspace: String,
    pub kind: BearerKind,
}

fn default_token_workspace() -> String {
    "default".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_checks_the_longer_prefix_first() {
        assert_eq!(classify_token("mag_pat_abcd"), TokenKind::ApiToken);
        assert_eq!(classify_token("mag_abcd"), TokenKind::Session);
        assert_eq!(classify_token("plt_abcd"), TokenKind::Grant);
        assert_eq!(classify_token("czt_abcd"), TokenKind::Unknown);
        assert_eq!(classify_token(""), TokenKind::Unknown);
        assert_eq!(classify_token(" mag_pat_x "), TokenKind::ApiToken);
    }

    #[test]
    fn minted_tokens_carry_their_prefix_and_full_entropy() {
        let token = mint_token_value(SESSION_TOKEN_PREFIX);
        assert!(token.starts_with("mag_"));
        assert_eq!(token.len(), "mag_".len() + 43);
        let other = mint_token_value(SESSION_TOKEN_PREFIX);
        assert_ne!(token, other);
    }

    #[test]
    fn token_hash_is_deterministic_and_prefix_aware() {
        assert_eq!(hash_token("mag_x"), hash_token("mag_x"));
        assert_ne!(hash_token("mag_x"), hash_token("mag_y"));
        assert_eq!(hash_token("mag_x").len(), 64);
    }

    fn grant_with(allowed_tools: &[&str]) -> TerminalGrant {
        TerminalGrant {
            id: Uuid::new_v4(),
            token_hash: hash_token("plt_x"),
            identity: "owner".to_string(),
            workspace: "default".to_string(),
            label: "laptop terminal".to_string(),
            agent_identity: "plane-agent".to_string(),
            harness_engine: "claude_code".to_string(),
            allowed_tools: allowed_tools.iter().map(|tool| tool.to_string()).collect(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            max_usd: None,
            max_wall_clock_secs: None,
            max_concurrent_runs: None,
        }
    }

    #[test]
    fn permits_enforces_the_floor_on_both_allowlist_shapes() {
        // Empty allowlist: unscoped-but-floored — read tools pass, the
        // floor never does.
        let unscoped = grant_with(&[]);
        assert!(unscoped.permits("read_file"));
        assert!(unscoped.permits("magician_send_email"));
        for tool in NEVER_ON_THE_PLANE {
            assert!(!unscoped.permits(tool), "floored: {tool}");
        }
        // Non-empty: listed tools pass, unlisted do not, and a floored name
        // listed anyway still refuses — mint filters, permits() re-checks.
        let scoped = grant_with(&["read_file", "run_coding_task"]);
        assert!(scoped.permits("read_file"));
        assert!(!scoped.permits("write_file"));
        assert!(!scoped.permits("run_coding_task"));
        assert!(!scoped.permits("claude"));
    }

    #[test]
    fn floored_tools_reports_what_mint_would_drop() {
        let requested = vec![
            "read_file".to_string(),
            "claude".to_string(),
            "opencode".to_string(),
        ];
        assert_eq!(
            floored_tools(&requested),
            vec!["claude".to_string(), "opencode".to_string()]
        );
        assert!(floored_tools(&["read_file".to_string()]).is_empty());
    }
}
