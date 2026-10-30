//! Whose credentials the harness CLIs use (`execution.harness_auth`).
//!
//! Claude Code, Codex, Grok, Antigravity and Pi each keep their own sign-in
//! (a subscription or an account the owner logged into). A provider API key
//! in a CLI's environment usually wins over that sign-in and bills the API
//! instead, so Magician passes its keys only when this setting says so:
//!
//! - `login_first` (default): the CLI's own sign-in. When that sign-in is
//!   missing, expired, rejected or out of credits/quota the turn fails with
//!   the CLI's own reason and the owner is asked once ([`fallback`]) whether
//!   to use Magician's provider key: once, always for this CLI, or no. Never
//!   an automatic fallback: API charges need the owner's approval.
//! - `login_only`: the CLI's own sign-in, never Magician's keys. A CLI with no
//!   sign-in fails and raises an attention.
//! - `magician`: always Magician's provider key (the owner chose it).
//!
//! `engines` overrides the mode for one CLI. The older
//! `coding.claude.use_api_key` / `coding.agy.use_api_key` flags act as a
//! `magician` override for their engine when no override is set.
//!
//! Sign-in is a state, not a flag ([`SignInState`]): a dangling, empty or
//! unreadable login file is signed out; a login whose every credential has
//! expired with no refresh token, a CLI that recorded a permanent refresh
//! failure (Grok's `auth.permanent_failure.set`), or a turn the CLI refused
//! for authentication since the login file last changed is expired.
//!
//! The same answer decides background operations: an operation that would
//! follow a harness-driven run onto its CLI (`op-harness-<engine>`) runs on
//! Magician's own route for that operation when the CLI would not be on its
//! own sign-in, because the background CLI path never carries keys.

pub mod failure;
pub mod fallback;

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use crate::config::{HarnessAuthMode, HarnessAuthSettings, MagicianCodingSettings};

/// The CLIs this setting covers, by roster name.
pub const ENGINES: &[&str] = &["claude_code", "codex", "grok", "agy", "pi"];

fn settings_cell() -> &'static RwLock<HarnessAuthSettings> {
    static CELL: OnceLock<RwLock<HarnessAuthSettings>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(HarnessAuthSettings::default()))
}

/// Publish `execution.harness_auth`, folding the legacy `use_api_key` flags in
/// as overrides. Called at boot, on config reload and by the settings
/// endpoint.
pub fn configure(settings: &HarnessAuthSettings, coding: &MagicianCodingSettings) {
    *settings_cell()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = effective_settings(settings, coding);
}

/// `settings` with the legacy `use_api_key` flags folded in as overrides.
pub fn effective_settings(
    settings: &HarnessAuthSettings,
    coding: &MagicianCodingSettings,
) -> HarnessAuthSettings {
    let mut effective = settings.clone();
    for (engine, legacy) in [
        ("claude_code", coding.claude.use_api_key),
        ("agy", coding.agy.use_api_key),
    ] {
        if legacy {
            effective
                .engines
                .entry(engine.to_string())
                .or_insert(HarnessAuthMode::Magician);
        }
    }
    effective
}

/// The published settings (legacy flags folded in), or this thread's test
/// override.
pub fn settings() -> HarnessAuthSettings {
    if let Some(settings) = test_override::settings() {
        return settings;
    }
    settings_cell()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The roster name this setting files a launch under.
pub fn canonical_engine(engine: &str) -> &str {
    match engine {
        "codex_app_server" => "codex",
        "grok_acp" => "grok",
        "agy_cli" => "agy",
        "claude" => "claude_code",
        other => other,
    }
}

/// The mode that applies to `engine`.
pub fn mode_for(engine: &str) -> HarnessAuthMode {
    mode_in(&settings(), engine)
}

/// The mode `settings` gives `engine`.
pub fn mode_in(settings: &HarnessAuthSettings, engine: &str) -> HarnessAuthMode {
    settings
        .engines
        .get(canonical_engine(engine))
        .copied()
        .unwrap_or(settings.mode)
}

/// Where a CLI's own sign-in stands on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInState {
    SignedIn,
    /// A login exists but cannot be used without the owner signing in again.
    Expired,
    SignedOut,
}

impl SignInState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SignedIn => "signed_in",
            Self::Expired => "expired",
            Self::SignedOut => "signed_out",
        }
    }
}

/// A CLI's sign-in: the state, the CLI's own words when it said why (bounded,
/// redacted), and when its login file last changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignIn {
    pub state: SignInState,
    pub detail: Option<String>,
    pub modified_ms: Option<i64>,
}

impl SignIn {
    fn new(state: SignInState) -> Self {
        Self {
            state,
            detail: None,
            modified_ms: None,
        }
    }
}

/// Whether `engine`'s CLI has its own usable sign-in on this machine.
pub fn signed_in(engine: &str) -> bool {
    sign_in(engine).state == SignInState::SignedIn
}

/// `engine`'s sign-in on this machine: its login files, the CLI's own record
/// of a permanent refresh failure, and the last authentication failure a
/// turn on it reported. Never prints or returns a credential.
pub fn sign_in(engine: &str) -> SignIn {
    let engine = canonical_engine(engine);
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut found = sign_in_at(&home, |key| std::env::var(key).ok(), engine);
    if engine == "grok" {
        // Magician's own Grok home logs the launches Magician makes.
        let magician_home =
            crate::magician_v2::execution::coding_engine::grok_contract::magician_grok_home_dir();
        found = apply_cli_failure(found, grok_permanent_failure(&magician_home));
    }
    failure::apply_recorded(engine, found)
}

/// [`signed_in`] against a given home directory and environment (files only).
pub fn signed_in_at(home: &Path, env: impl Fn(&str) -> Option<String>, engine: &str) -> bool {
    sign_in_at(home, env, engine).state == SignInState::SignedIn
}

/// The sign-in the login files under `home` (and `env`'s CLI homes) show.
pub fn sign_in_at(home: &Path, env: impl Fn(&str) -> Option<String>, engine: &str) -> SignIn {
    let env_dir = |key: &str| {
        env(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    match canonical_engine(engine) {
        "claude_code" => {
            let account = read_login(&home.join(".claude.json")).and_then(|(value, modified)| {
                value
                    .get("oauthAccount")
                    .and_then(|v| v.as_object())
                    .filter(|account| !account.is_empty())
                    .map(|_| modified)
            });
            match account {
                None => SignIn::new(SignInState::SignedOut),
                Some(modified) => {
                    // The OAuth tokens live in the keychain on macOS and in
                    // `~/.claude/.credentials.json` elsewhere.
                    let credentials = read_login(&home.join(".claude").join(".credentials.json"));
                    let expired = credentials
                        .as_ref()
                        .is_some_and(|(value, _)| login_expired(value, now_ms));
                    file_state(
                        expired,
                        credentials.map(|(_, at)| at).unwrap_or(modified),
                        "claude_code",
                    )
                },
            }
        },
        "codex" => {
            let path = env_dir("CODEX_HOME")
                .unwrap_or_else(|| home.join(".codex"))
                .join("auth.json");
            match read_login(&path) {
                Some((value, modified))
                    if value
                        .get("OPENAI_API_KEY")
                        .and_then(|v| v.as_str())
                        .is_some_and(|key| !key.trim().is_empty())
                        || value
                            .get("tokens")
                            .and_then(|v| v.as_object())
                            .is_some_and(|tokens| !tokens.is_empty()) =>
                {
                    file_state(login_expired(&value, now_ms), modified, "codex")
                },
                _ => SignIn::new(SignInState::SignedOut),
            }
        },
        "grok" => {
            let grok_home = env_dir("GROK_HOME").unwrap_or_else(|| home.join(".grok"));
            let found = match read_login(&grok_home.join("auth.json")) {
                Some((value, modified)) if value.as_object().is_some_and(|o| !o.is_empty()) => {
                    file_state(login_expired(&value, now_ms), modified, "grok")
                },
                _ => SignIn::new(SignInState::SignedOut),
            };
            apply_cli_failure(found, grok_permanent_failure(&grok_home))
        },
        "agy" => {
            let mut found = SignIn::new(SignInState::SignedOut);
            for relative in
                crate::magician_v2::execution::coding_engine::agy_contract::AGY_OAUTH_TOKEN_RELATIVE_PATHS
            {
                let path = home.join(relative);
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                let modified = modified_ms(&path);
                // An opaque token is a sign-in; a JSON one can say it expired.
                let expired = serde_json::from_str::<serde_json::Value>(&text)
                    .is_ok_and(|value| login_expired(&value, now_ms));
                let state = file_state(expired, modified, "agy");
                if state.state == SignInState::SignedIn {
                    return state;
                }
                found = state;
            }
            found
        },
        "pi" => {
            let path = env_dir("PI_CODING_AGENT_DIR")
                .unwrap_or_else(|| home.join(".pi").join("agent"))
                .join("auth.json");
            match read_login(&path) {
                Some((value, modified)) if value.as_object().is_some_and(|o| !o.is_empty()) => {
                    file_state(login_expired(&value, now_ms), modified, "pi")
                },
                _ => SignIn::new(SignInState::SignedOut),
            }
        },
        _ => SignIn::new(SignInState::SignedOut),
    }
}

fn file_state(expired: bool, modified: Option<i64>, engine: &str) -> SignIn {
    SignIn {
        state: if expired {
            SignInState::Expired
        } else {
            SignInState::SignedIn
        },
        detail: expired.then(|| {
            format!(
                "The saved sign-in has expired and cannot be refreshed. {}",
                failure::sign_in_remedy(engine)
            )
        }),
        modified_ms: modified,
    }
}

/// A login file's JSON and modification time. A dangling link, an empty,
/// unreadable or non-JSON file is no login.
fn read_login(path: &Path) -> Option<(serde_json::Value, Option<i64>)> {
    let text = std::fs::read_to_string(path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    if value.is_null() {
        return None;
    }
    Some((value, modified_ms(path)))
}

fn modified_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|age| age.as_millis() as i64)
}

const EXPIRY_KEYS: &[&str] = &[
    "expires_at",
    "expiresAt",
    "expires",
    "expiry",
    "expiry_date",
    "expiryDate",
    "expires_on",
    "expiresOn",
];
const REFRESH_KEYS: &[&str] = &["refresh_token", "refreshToken", "refresh"];
const STANDING_KEY_KEYS: &[&str] = &["key", "api_key", "apiKey", "OPENAI_API_KEY"];

/// Whether every credential in a login document that carries an expiry has
/// passed it with no refresh token beside it, and nothing in it is a
/// standing key. A refreshable login is left to its CLI to refresh.
pub fn login_expired(value: &serde_json::Value, now_ms: i64) -> bool {
    #[derive(Default)]
    struct Seen {
        expiring: usize,
        dead: usize,
        standing: bool,
    }
    fn non_empty(value: Option<&serde_json::Value>) -> bool {
        value
            .and_then(|v| v.as_str())
            .is_some_and(|text| !text.trim().is_empty())
    }
    fn walk(value: &serde_json::Value, now_ms: i64, depth: usize, seen: &mut Seen) {
        let Some(object) = value.as_object() else {
            return;
        };
        if depth > 5 {
            return;
        }
        let expiry = EXPIRY_KEYS
            .iter()
            .find_map(|key| object.get(*key))
            .and_then(expiry_ms);
        let refreshable = REFRESH_KEYS.iter().any(|key| non_empty(object.get(*key)));
        match expiry {
            Some(at) => {
                seen.expiring += 1;
                if at <= now_ms && !refreshable {
                    seen.dead += 1;
                }
            },
            None => {
                if refreshable
                    || STANDING_KEY_KEYS
                        .iter()
                        .any(|key| non_empty(object.get(*key)))
                {
                    seen.standing = true;
                }
            },
        }
        for child in object.values() {
            walk(child, now_ms, depth + 1, seen);
        }
    }
    let mut seen = Seen::default();
    walk(value, now_ms, 0, &mut seen);
    seen.expiring > 0 && seen.dead == seen.expiring && !seen.standing
}

/// An expiry as Unix milliseconds: epoch milliseconds or seconds, or an
/// RFC 3339 time.
fn expiry_ms(value: &serde_json::Value) -> Option<i64> {
    let number = match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => {
            if let Ok(at) = chrono::DateTime::parse_from_rfc3339(text.trim()) {
                return Some(at.timestamp_millis());
            }
            text.trim().parse::<f64>().ok()
        },
        _ => None,
    }?;
    if !number.is_finite() || number <= 0.0 {
        return None;
    }
    Some(if number > 1e12 {
        number as i64
    } else {
        (number * 1000.0) as i64
    })
}

/// A CLI's own record that its login is gone for good, newer than the
/// login file, makes the sign-in expired with the CLI's words.
fn apply_cli_failure(found: SignIn, failure: Option<(i64, String)>) -> SignIn {
    let Some((at_ms, message)) = failure else {
        return found;
    };
    if found.state == SignInState::SignedIn
        && found.modified_ms.is_some_and(|modified| modified > at_ms)
    {
        // Signed in again since.
        return found;
    }
    if found.state == SignInState::Expired && found.detail.is_some() {
        return found;
    }
    SignIn {
        state: SignInState::Expired,
        detail: Some(message),
        modified_ms: found.modified_ms,
    }
}

/// How much of a Grok log's tail is read for its last permanent failure.
const GROK_LOG_TAIL_BYTES: u64 = 256 * 1024;

/// Grok's own record of a permanent refresh failure
/// (`auth.permanent_failure.set`) in `grok_home/logs/unified.jsonl`, newest
/// last: `(when, its message)`. A login written after it supersedes it
/// ([`apply_cli_failure`] compares the login file's time).
fn grok_permanent_failure(grok_home: &Path) -> Option<(i64, String)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(grok_home.join("logs").join("unified.jsonl")).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(GROK_LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(GROK_LOG_TAIL_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    grok_permanent_failure_in(&String::from_utf8_lossy(&bytes))
}

fn grok_permanent_failure_in(log: &str) -> Option<(i64, String)> {
    let mut last = None;
    for line in log.lines() {
        // Cheap filter before parsing: only auth state changes matter.
        if !line.contains("auth") {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let msg = event
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let at = event
            .get("ts")
            .and_then(|v| v.as_str())
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .map(|ts| ts.timestamp_millis());
        match msg {
            "auth.permanent_failure.set" => {
                let message = event
                    .pointer("/ctx/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.pointer("/ctx/reason").and_then(|v| v.as_str()))
                    .unwrap_or("Grok's saved sign-in was rejected.");
                if let Some(at) = at {
                    last = Some((at, failure::bounded_reason(message)));
                }
            },
            _ => {},
        }
    }
    last
}

/// Whether a launch of `engine` gets Magician's provider key: always under
/// `magician`, never under `login_only`, and under `login_first` only while
/// the owner's approval for this CLI stands ([`fallback::approve_once`];
/// "always" writes a `magician` override instead).
pub fn uses_magician_credentials(engine: &str) -> bool {
    let engine = canonical_engine(engine);
    decide(mode_for(engine), || fallback::once_active(engine))
}

/// The sign-in a launch decision sees: this thread's test override, else
/// the machine. Test builds never read the developer's own CLI logins: with
/// no override every CLI counts as signed in (hermetic); a test that needs
/// another answer installs one with [`test_override::with_sign_in`].
fn sign_in_for_launch(engine: &str) -> SignInState {
    if let Some(answer) = test_override::signed_in(engine) {
        return if answer {
            SignInState::SignedIn
        } else {
            SignInState::SignedOut
        };
    }
    if cfg!(any(test, feature = "test-fixtures")) {
        return SignInState::SignedIn;
    }
    sign_in(engine).state
}

fn signed_in_for_launch(engine: &str) -> bool {
    sign_in_for_launch(engine) == SignInState::SignedIn
}

fn decide(mode: HarnessAuthMode, owner_approved: impl FnOnce() -> bool) -> bool {
    match mode {
        HarnessAuthMode::Magician => true,
        HarnessAuthMode::LoginOnly => false,
        // Never automatic: the owner approves API charges first.
        HarnessAuthMode::LoginFirst => owner_approved(),
    }
}

/// Whether a background operation may follow a run onto `engine`'s CLI. The
/// background CLI path carries no keys, so it needs the CLI on its own
/// sign-in; otherwise the operation stays on Magician's route.
pub fn background_may_use_cli(engine: &str) -> bool {
    !uses_magician_credentials(engine) && signed_in_for_launch(engine)
}

/// What a launch of `engine` runs on, for engine pickers: `login` (its own
/// sign-in), `magician_key`, `expired` (its sign-in expired; Magician's key
/// only after the owner approves) or `none` (no sign-in). `None` for engines
/// this setting does not cover (`magician`).
pub fn credential_label(engine: &str) -> Option<&'static str> {
    let engine = canonical_engine(engine);
    if !ENGINES.contains(&engine) {
        return None;
    }
    if uses_magician_credentials(engine) {
        return Some("magician_key");
    }
    match sign_in_for_launch(engine) {
        SignInState::SignedIn => Some("login"),
        // Pi turns carry the selected Magician profile's key even without a
        // Pi login; only its background operations need one.
        _ if engine == "pi" => Some("magician_key"),
        SignInState::Expired => Some("expired"),
        SignInState::SignedOut => Some("none"),
    }
}

/// How Magician's key can come in for `engine` when its sign-in fails:
/// `always` (the `magician` mode), `ask` (`login_first`: only after the owner
/// approves), `never` (`login_only`, and Pi, whose route is its profile).
pub fn key_fallback_policy(engine: &str) -> &'static str {
    let engine = canonical_engine(engine);
    match mode_for(engine) {
        HarnessAuthMode::Magician => "always",
        HarnessAuthMode::LoginOnly => "never",
        HarnessAuthMode::LoginFirst if engine == "pi" => "never",
        HarnessAuthMode::LoginFirst => "ask",
    }
}

/// `{engine: {mode, signed_in, sign_in, sign_in_detail, remedy, last_failure,
/// uses_magician_credentials, key_fallback, magician_key_available}}` for the
/// settings surface. `signed_in` stays a boolean for older clients; `sign_in`
/// is `signed_in`, `expired` or `signed_out`.
pub fn status() -> serde_json::Value {
    let rows: serde_json::Map<String, serde_json::Value> = ENGINES
        .iter()
        .map(|engine| {
            let found = sign_in(engine);
            let recorded = failure::last_failure(engine);
            let remedy = match found.state {
                SignInState::SignedIn => recorded.as_ref().map(|failure| failure.remedy.clone()),
                _ => Some(failure::sign_in_remedy(engine).to_string()),
            };
            (
                (*engine).to_string(),
                serde_json::json!({
                    "mode": mode_for(engine),
                    "signed_in": found.state == SignInState::SignedIn,
                    "sign_in": found.state.as_str(),
                    "sign_in_detail": found.detail,
                    "remedy": remedy,
                    "last_failure": recorded.map(|failure| serde_json::json!({
                        "class": failure.class.code(),
                        "reason": failure.reason,
                        "remedy": failure.remedy,
                        "at_ms": failure.at_ms,
                    })),
                    "uses_magician_credentials": uses_magician_credentials(engine),
                    "key_fallback": key_fallback_policy(engine),
                    "magician_key_available": fallback::magician_key_available(engine),
                }),
            )
        })
        .collect();
    serde_json::Value::Object(rows)
}

/// Per-thread overrides for tests: settings and sign-in answers that do not
/// leak into tests running beside them (the published settings are global).
pub mod test_override {
    use std::cell::RefCell;
    use std::sync::Arc;

    use crate::config::HarnessAuthSettings;

    type SignIn = Arc<dyn Fn(&str) -> bool>;

    thread_local! {
        static SETTINGS: RefCell<Option<HarnessAuthSettings>> = const { RefCell::new(None) };
        static SIGN_IN: RefCell<Option<SignIn>> = const { RefCell::new(None) };
    }

    pub(super) fn settings() -> Option<HarnessAuthSettings> {
        SETTINGS.with(|cell| cell.borrow().clone())
    }

    pub(super) fn signed_in(engine: &str) -> Option<bool> {
        SIGN_IN.with(|cell| cell.borrow().as_ref().map(|answer| answer(engine)))
    }

    /// Run `body` on this thread with `settings` published and `signed_in`
    /// answering for every CLI; both are restored afterwards.
    pub fn with_sign_in<R>(
        settings: HarnessAuthSettings,
        signed_in: impl Fn(&str) -> bool + 'static,
        body: impl FnOnce() -> R,
    ) -> R {
        let previous_settings = SETTINGS.with(|cell| cell.replace(Some(settings)));
        let previous_sign_in = SIGN_IN.with(|cell| cell.replace(Some(Arc::new(signed_in))));
        struct Restore(Option<HarnessAuthSettings>, Option<SignIn>);
        impl Drop for Restore {
            fn drop(&mut self) {
                SETTINGS.with(|cell| *cell.borrow_mut() = self.0.take());
                SIGN_IN.with(|cell| *cell.borrow_mut() = self.1.take());
            }
        }
        let _restore = Restore(previous_settings, previous_sign_in);
        body()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_first_never_falls_back_to_magician_keys_without_the_owner() {
        // Signed in or not: only the owner's approval brings the key in.
        assert!(!decide(HarnessAuthMode::LoginFirst, || false));
        assert!(decide(HarnessAuthMode::LoginFirst, || true));
        assert!(!decide(HarnessAuthMode::LoginOnly, || true));
        assert!(decide(HarnessAuthMode::Magician, || false));
    }

    #[test]
    fn legacy_use_api_key_is_an_override_unless_one_is_set() {
        let mut coding = MagicianCodingSettings::default();
        coding.claude.use_api_key = true;
        coding.agy.use_api_key = true;
        let mut settings = HarnessAuthSettings::default();
        settings
            .engines
            .insert("agy".to_string(), HarnessAuthMode::LoginOnly);
        let effective = effective_settings(&settings, &coding);
        assert_eq!(
            mode_in(&effective, "claude_code"),
            HarnessAuthMode::Magician
        );
        assert_eq!(mode_in(&effective, "claude"), HarnessAuthMode::Magician);
        assert_eq!(mode_in(&effective, "agy"), HarnessAuthMode::LoginOnly);
        assert_eq!(
            mode_in(&effective, "codex_app_server"),
            HarnessAuthMode::LoginFirst
        );
    }

    #[test]
    fn each_cli_sign_in_is_read_from_its_own_login_file() {
        let home = tempfile::tempdir().expect("tempdir");
        let no_env = |_: &str| None;
        for engine in ENGINES {
            assert!(!signed_in_at(home.path(), no_env, engine), "{engine}");
        }
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"oauthAccount":{"id":"x"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(
            home.path().join(".codex/auth.json"),
            r#"{"OPENAI_API_KEY":null,"tokens":{"access_token":"a","refresh_token":"r"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".grok")).unwrap();
        std::fs::write(
            home.path().join(".grok/auth.json"),
            r#"{"scope":{"key":"k"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".pi/agent")).unwrap();
        std::fs::write(home.path().join(".pi/agent/auth.json"), r#"{"openai":{}}"#).unwrap();
        for engine in ["claude_code", "codex", "grok", "pi"] {
            assert!(signed_in_at(home.path(), no_env, engine), "{engine}");
        }
        // An empty Pi login, a Codex file with no tokens and an env-pointed
        // Codex home are honoured.
        std::fs::write(home.path().join(".pi/agent/auth.json"), "{}").unwrap();
        assert!(!signed_in_at(home.path(), no_env, "pi"));
        std::fs::write(home.path().join(".codex/auth.json"), "{}").unwrap();
        assert!(!signed_in_at(home.path(), no_env, "codex"));
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let codex_env =
            |key: &str| (key == "CODEX_HOME").then(|| elsewhere.path().display().to_string());
        assert!(!signed_in_at(home.path(), codex_env, "codex"));
    }

    /// Grok deletes its login after a rejected refresh, leaving Magician's
    /// home link dangling: that is signed out, never signed in.
    #[cfg(unix)]
    #[test]
    fn a_dangling_or_empty_login_file_is_not_a_sign_in() {
        let home = tempfile::tempdir().expect("tempdir");
        let grok = home.path().join(".grok");
        std::fs::create_dir_all(&grok).unwrap();
        std::os::unix::fs::symlink(home.path().join("gone.json"), grok.join("auth.json")).unwrap();
        let found = sign_in_at(home.path(), |_| None, "grok");
        assert_eq!(found.state, SignInState::SignedOut);
        std::fs::remove_file(grok.join("auth.json")).unwrap();
        std::fs::write(grok.join("auth.json"), "  \n").unwrap();
        assert_eq!(
            sign_in_at(home.path(), |_| None, "grok").state,
            SignInState::SignedOut
        );
    }

    #[test]
    fn an_unrefreshable_past_expiry_is_expired_and_a_refreshable_one_is_not() {
        let now = 1_800_000_000_000;
        let past_s = serde_json::json!(1_700_000_000);
        // Every credential expired, none refreshable.
        assert!(login_expired(
            &serde_json::json!({"anthropic": {"type": "oauth", "access": "a", "expires": now - 1}}),
            now
        ));
        assert!(login_expired(
            &serde_json::json!({"scope": {"access_token": "a", "expires_at": "2026-10-15T16:34:02.286432+00:00"}}),
            now
        ));
        assert!(login_expired(
            &serde_json::json!({"token": {"expiry_date": past_s}}),
            now
        ));
        // A refresh token, a standing key, a future expiry, or no expiry at all.
        assert!(!login_expired(
            &serde_json::json!({"token": {"access_token": "a", "refresh_token": "r", "expiry": "2020-01-01T00:00:00Z"}}),
            now
        ));
        assert!(!login_expired(
            &serde_json::json!({"a": {"expires": now - 1}, "b": {"type": "api_key", "key": "k"}}),
            now
        ));
        assert!(!login_expired(
            &serde_json::json!({"a": {"expires": now + 60_000}}),
            now
        ));
        assert!(!login_expired(
            &serde_json::json!({"tokens": {"access_token": "a"}}),
            now
        ));
    }

    #[test]
    fn pi_with_only_expired_unrefreshable_logins_reports_expired_with_a_remedy() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(home.path().join(".pi/agent")).unwrap();
        std::fs::write(
            home.path().join(".pi/agent/auth.json"),
            r#"{"anthropic":{"type":"oauth","access":"a","expires":1000}}"#,
        )
        .unwrap();
        let found = sign_in_at(home.path(), |_| None, "pi");
        assert_eq!(found.state, SignInState::Expired);
        assert!(found.detail.unwrap().contains("/login"));
    }

    #[test]
    fn grok_permanent_refresh_failure_reads_as_expired_until_a_newer_login() {
        let log = concat!(
            r#"{"ts":"2026-10-20T04:28:51.372Z","lvl":"info","msg":"auth init token state","ctx":{"is_expired":"True"}}"#,
            "\n",
            r#"{"ts":"2026-10-20T04:28:52.147Z","lvl":"warn","msg":"auth.permanent_failure.set","ctx":{"reason":"RefreshTokenRejected","message":"Your session has expired. Run `grok login` to sign in again.","ttl_seconds":"None"}}"#,
            "\n",
        );
        let (at, message) = grok_permanent_failure_in(log).expect("failure");
        assert!(message.contains("grok login"), "{message}");
        // The login file is gone: expired, in Grok's words.
        let gone = apply_cli_failure(
            SignIn::new(SignInState::SignedOut),
            Some((at, message.clone())),
        );
        assert_eq!(gone.state, SignInState::Expired);
        assert_eq!(gone.detail.as_deref(), Some(message.as_str()));
        // An older login file: still expired.
        let stale = SignIn {
            state: SignInState::SignedIn,
            detail: None,
            modified_ms: Some(at - 1),
        };
        assert_eq!(
            apply_cli_failure(stale, Some((at, message.clone()))).state,
            SignInState::Expired
        );
        // Signed in again afterwards.
        let fresh = SignIn {
            state: SignInState::SignedIn,
            detail: None,
            modified_ms: Some(at + 1),
        };
        assert_eq!(
            apply_cli_failure(fresh, Some((at, message))).state,
            SignInState::SignedIn
        );
        assert!(grok_permanent_failure_in("not json\n{}\n").is_none());
    }

    /// The whole launch decision, signed in and not, per mode.
    #[test]
    fn launches_carry_magician_keys_only_when_the_mode_or_the_owner_says_so() {
        let login_first = HarnessAuthSettings::default();
        test_override::with_sign_in(
            login_first.clone(),
            |_| false,
            || {
                // No silent fallback: a missing login gets no key.
                assert!(!uses_magician_credentials("codex"));
                assert!(!background_may_use_cli("codex"));
                assert_eq!(credential_label("codex"), Some("none"));
                assert_eq!(key_fallback_policy("codex"), "ask");
                assert_eq!(key_fallback_policy("pi"), "never");
            },
        );
        test_override::with_sign_in(
            login_first,
            |_| true,
            || {
                assert!(!uses_magician_credentials("codex"));
                assert!(background_may_use_cli("codex"));
                assert_eq!(credential_label("codex"), Some("login"));
            },
        );
        let mut login_only = HarnessAuthSettings::default();
        login_only.mode = HarnessAuthMode::LoginOnly;
        test_override::with_sign_in(
            login_only,
            |_| false,
            || {
                assert!(!uses_magician_credentials("grok"));
                assert_eq!(credential_label("grok"), Some("none"));
                assert_eq!(key_fallback_policy("grok"), "never");
                // Pi turns still run on the selected Magician profile.
                assert_eq!(credential_label("pi"), Some("magician_key"));
            },
        );
        let mut magician = HarnessAuthSettings::default();
        magician.mode = HarnessAuthMode::Magician;
        test_override::with_sign_in(
            magician,
            |_| true,
            || {
                assert!(uses_magician_credentials("claude_code"));
                assert!(!background_may_use_cli("claude_code"));
                assert_eq!(key_fallback_policy("claude_code"), "always");
            },
        );
        // Overrides are gone afterwards.
        assert!(test_override::settings().is_none());
        assert_eq!(test_override::signed_in("codex"), None);
        assert_eq!(credential_label("magician"), None);
    }
}
