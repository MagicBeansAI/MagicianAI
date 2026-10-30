//! Owner-approved fallback to Magician's provider key (`login_first`).
//!
//! When a CLI's own sign-in is missing, expired, rejected, or out of
//! credits/quota, Magician never swaps its API key in on its own: API charges
//! need the owner. The turn fails with the CLI's reason and one owner decision
//! is raised on the shared user-request prompts (source `harness_auth`,
//! `request_type` [`REQUEST_TYPE`], answered through the canonical
//! `/hitl/{id}/respond` handle like every user request):
//!
//! - `once`: Magician's key for this CLI until the retried turn ends (bounded
//!   by [`ONCE_TTL_MS`]); the failed turn is retried on it.
//! - `always`: writes `execution.harness_auth.engines.<cli>: magician`
//!   through the comment-preserving config writer and applies it at once.
//! - `no` (also the timeout): the turn stays failed; the remedy says what to
//!   do (`grok login`, top up credits).
//!
//! One pending decision per scope, CLI and failure class: concurrent failed
//! turns wait on the same question instead of raising another. `magician`
//! needs no approval (the owner chose it); `login_only` never offers the key;
//! Pi's fallback is a Magician profile, not a key, so it is never offered.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::config::{HarnessAuthMode, HarnessAuthSettings, MagicianCodingSettings};
use crate::magician_v2::realtime_events::ServiceFailure;
use crate::magician_v2::user_requests::service::{RequestOption, UserRequest};

use super::failure::{engine_label, provider_label, sign_in_remedy};
use super::{canonical_engine, mode_for, SignInState, ENGINES};

/// `UserRequest.request_type` of the key-fallback decision.
pub const REQUEST_TYPE: &str = "harness_key_fallback";
/// `UserRequest.source` of the key-fallback decision.
pub const SOURCE: &str = "harness_auth";
/// How long an unanswered decision waits before it counts as `no`.
pub const ASK_TIMEOUT_SECS: u64 = 300;
/// How long a "once" approval stands if the retried turn never ends it.
pub const ONCE_TTL_MS: i64 = 15 * 60 * 1000;

/// The environment variables Magician's key for `engine` rides in.
pub fn key_env(engine: &str) -> &'static [&'static str] {
    match canonical_engine(engine) {
        "claude_code" => &["ANTHROPIC_API_KEY"],
        "codex" => &["OPENAI_API_KEY"],
        "grok" => &["XAI_API_KEY"],
        "agy" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        _ => &[],
    }
}

/// Whether Magician's runtime holds a key `engine` could use. Reads only
/// whether the variable is set, never its value.
pub fn magician_key_available(engine: &str) -> bool {
    key_env(engine).iter().any(|key| {
        std::env::var(key)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

/// Whether a failure of `class` on `engine` may be offered Magician's key:
/// `login_first`, not Pi, an account failure, not already on the key, and a
/// key to offer.
pub fn offers_key(engine: &str, class: ServiceFailure) -> bool {
    let engine = canonical_engine(engine);
    ENGINES.contains(&engine)
        && engine != "pi"
        && mode_for(engine) == HarnessAuthMode::LoginFirst
        && matches!(
            class,
            ServiceFailure::Authentication | ServiceFailure::Credit | ServiceFailure::RateLimit
        )
        && !super::uses_magician_credentials(engine)
        && magician_key_available(engine)
}

/// The owner's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDecision {
    Once,
    Always,
    No,
}

impl KeyDecision {
    pub fn approved(self) -> bool {
        !matches!(self, Self::No)
    }

    fn from_choice(choice: &str) -> Self {
        match choice {
            "once" => Self::Once,
            "always" => Self::Always,
            _ => Self::No,
        }
    }
}

/// Where the decision is asked: the owner's scope and the run or task the
/// failed turn belongs to.
#[derive(Debug, Clone, Default)]
pub struct AskScope {
    pub principal: String,
    pub workspace: String,
    pub execution_id: Option<String>,
    pub task_id: Option<String>,
}

#[derive(Default)]
struct OnceGrants {
    until_ms: HashMap<String, i64>,
}

impl OnceGrants {
    fn approve(&mut self, engine: &str, now_ms: i64) {
        self.until_ms
            .insert(canonical_engine(engine).to_string(), now_ms + ONCE_TTL_MS);
    }

    fn active(&mut self, engine: &str, now_ms: i64) -> bool {
        let engine = canonical_engine(engine);
        match self.until_ms.get(engine) {
            Some(until) if *until > now_ms => true,
            Some(_) => {
                self.until_ms.remove(engine);
                false
            },
            None => false,
        }
    }

    fn finish(&mut self, engine: &str) -> bool {
        self.until_ms.remove(canonical_engine(engine)).is_some()
    }
}

fn once_grants() -> &'static Mutex<OnceGrants> {
    static GRANTS: OnceLock<Mutex<OnceGrants>> = OnceLock::new();
    GRANTS.get_or_init(Mutex::default)
}

/// The owner approved Magician's key for `engine`'s next turn.
pub fn approve_once(engine: &str) {
    once_grants()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .approve(engine, chrono::Utc::now().timestamp_millis());
}

/// Whether a "once" approval stands for `engine`.
pub fn once_active(engine: &str) -> bool {
    once_grants()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .active(engine, chrono::Utc::now().timestamp_millis())
}

/// The retried turn ended: a "once" approval is spent. Returns whether one
/// was standing.
pub fn finish_once(engine: &str) -> bool {
    once_grants()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .finish(engine)
}

type PendingKey = (String, String, String, &'static str);

fn pending(
) -> &'static Mutex<HashMap<PendingKey, tokio::sync::watch::Receiver<Option<KeyDecision>>>> {
    static PENDING: OnceLock<
        Mutex<HashMap<PendingKey, tokio::sync::watch::Receiver<Option<KeyDecision>>>>,
    > = OnceLock::new();
    PENDING.get_or_init(Mutex::default)
}

/// Removes the leader's pending entry however its ask ends (answered,
/// cancelled with the turn, or panicked).
struct PendingEntry(PendingKey);

impl Drop for PendingEntry {
    fn drop(&mut self) {
        pending()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

/// Before a launch: a CLI that is not signed in (or whose session expired)
/// under `login_first` asks the owner first, so an approved launch runs on
/// the key instead of failing. `None` when nothing needed asking.
pub async fn preflight(scope: &AskScope, engine: &str) -> Option<KeyDecision> {
    let engine = canonical_engine(engine);
    if !offers_key(engine, ServiceFailure::Authentication) {
        return None;
    }
    let state = super::sign_in_for_launch(engine);
    if state == SignInState::SignedIn {
        return None;
    }
    let detail = if cfg!(any(test, feature = "test-fixtures")) {
        None
    } else {
        super::sign_in(engine).detail
    };
    let reason = detail.unwrap_or_else(|| match state {
        SignInState::Expired => format!("{}'s session has expired.", engine_label(engine)),
        _ => format!("{} is not signed in.", engine_label(engine)),
    });
    Some(ask_key_fallback(scope, engine, ServiceFailure::Authentication, &reason).await)
}

/// Ask the owner whether `engine` may use Magician's key after a failure of
/// `class` (`reason`: the CLI's own words, bounded and redacted). Applies the
/// answer before returning it. Concurrent asks for the same scope, CLI and
/// class share one question.
pub async fn ask_key_fallback(
    scope: &AskScope,
    engine: &str,
    class: ServiceFailure,
    reason: &str,
) -> KeyDecision {
    let engine = canonical_engine(engine).to_string();
    if !offers_key(&engine, class) || scope.principal.is_empty() || scope.workspace.is_empty() {
        return KeyDecision::No;
    }
    let Some(service) = crate::magician_v2::god_mode::approval_service() else {
        tracing::warn!(
            engine = %engine,
            "[HARNESS-AUTH] no approval channel; Magician's key stays off"
        );
        return KeyDecision::No;
    };
    let key: PendingKey = (
        scope.principal.clone(),
        scope.workspace.clone(),
        engine.clone(),
        class.code(),
    );
    let leader = {
        let mut map = pending()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match map.get(&key) {
            Some(waiting) => Err(waiting.clone()),
            None => {
                let (tx, rx) = tokio::sync::watch::channel(None);
                map.insert(key.clone(), rx);
                Ok(tx)
            },
        }
    };
    let tx = match leader {
        Ok(tx) => tx,
        Err(mut waiting) => {
            // Another failed turn already asked: wait for its answer.
            loop {
                if let Some(decision) = *waiting.borrow() {
                    return decision;
                }
                if waiting.changed().await.is_err() {
                    return waiting.borrow().unwrap_or(KeyDecision::No);
                }
            }
        },
    };
    let _entry = PendingEntry(key);
    let (question, options, context) = question(&engine, class, reason);
    let response = service
        .ask(UserRequest {
            id: String::new(),
            request_type: REQUEST_TYPE.to_string(),
            question,
            options,
            principal: scope.principal.clone(),
            workspace: scope.workspace.clone(),
            context,
            source: SOURCE.to_string(),
            execution_id: scope.execution_id.clone(),
            task_id: scope.task_id.clone(),
            timeout_secs: ASK_TIMEOUT_SECS,
            default_on_timeout: "no".to_string(),
            created_at: 0,
            sensitive: None,
        })
        .await;
    let mut decision = KeyDecision::from_choice(&response.decision);
    tracing::warn!(
        engine = %engine,
        failure = class.code(),
        decision = ?decision,
        channel = %response.channel,
        "[HARNESS-AUTH] owner answered the key-fallback decision"
    );
    match decision {
        KeyDecision::Once => approve_once(&engine),
        KeyDecision::Always => {
            let persisted = {
                let engine = engine.clone();
                tokio::task::spawn_blocking(move || {
                    persist_engine_mode(&engine, HarnessAuthMode::Magician)
                })
                .await
            };
            match persisted {
                Ok(Ok(())) => {},
                failed => {
                    // The owner approved at least this turn.
                    tracing::error!(engine = %engine, ?failed, "[HARNESS-AUTH] could not save the per-CLI override; using the key once");
                    approve_once(&engine);
                    decision = KeyDecision::Once;
                },
            }
        },
        KeyDecision::No => {},
    }
    let _ = tx.send(Some(decision));
    decision
}

fn question(
    engine: &str,
    class: ServiceFailure,
    reason: &str,
) -> (String, Vec<RequestOption>, serde_json::Value) {
    let label = engine_label(engine);
    let provider = provider_label(engine);
    let mut said = reason.trim().to_string();
    if !said.ends_with(['.', '!', '?', '…']) {
        said.push('.');
    }
    let question =
        format!("{label}: {said} Use Magician's {provider} API key instead? API charges apply.");
    let remedy = match class {
        ServiceFailure::Authentication => sign_in_remedy(engine).to_string(),
        ServiceFailure::Credit => "Top up the account's credits.".to_string(),
        _ => "Wait for the limit to reset.".to_string(),
    };
    let option = |id: &str, label: String| RequestOption {
        id: id.to_string(),
        label,
        requires_input: false,
    };
    let options = vec![
        option("once", "Use the key for this turn".to_string()),
        option("always", format!("Always use the key for {label}")),
        option("no", format!("No — {}", remedy.trim_end_matches('.'))),
    ];
    let context = serde_json::json!({
        "input_type": "choice",
        "hint": "No API key is used until you approve. \"Always\" sets this CLI to Magician's keys in Settings → Harness credentials.",
        "engine": engine,
        "health_issue": class.code(),
        "reason": reason,
        "remedy": remedy,
        "provider": provider,
        "env": key_env(engine),
    });
    (question, options, context)
}

/// Write `execution.harness_auth.engines.<engine>: <mode>` to the live
/// config (comment-preserving, under the shared config-file lock) and
/// publish it.
pub fn persist_engine_mode(engine: &str, mode: HarnessAuthMode) -> std::io::Result<()> {
    use crate::magician_v2::runtime_settings::{
        config_file_lock, runtime_settings_paths, write_text_file_atomic,
    };
    let engine = canonical_engine(engine);
    if !ENGINES.contains(&engine) {
        return Err(std::io::Error::other(format!("unknown engine `{engine}`")));
    }
    let path = runtime_settings_paths().config_path;
    let lock = config_file_lock(&path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(&path)?;
    let (next, after, coding) = with_engine_mode(&original, engine, mode)?;
    if next != original {
        write_text_file_atomic(&path, &next, None)?;
    }
    super::configure(&after, &coding);
    Ok(())
}

/// The config text with `engine`'s override set, the stored settings after
/// it, and the `coding` block (for the legacy flags).
fn with_engine_mode(
    contents: &str,
    engine: &str,
    mode: HarnessAuthMode,
) -> std::io::Result<(String, HarnessAuthSettings, MagicianCodingSettings)> {
    use crate::magician_v2::runtime_settings::apply_yaml_section;
    let document: serde_yaml::Value =
        serde_yaml::from_str(contents).map_err(std::io::Error::other)?;
    let before: HarnessAuthSettings = match document
        .get("execution")
        .and_then(|execution| execution.get("harness_auth"))
    {
        Some(value) => serde_yaml::from_value(value.clone()).map_err(std::io::Error::other)?,
        None => HarnessAuthSettings::default(),
    };
    let mut after = before.clone();
    after.engines.insert(engine.to_string(), mode);
    let next = apply_yaml_section(contents, &["execution", "harness_auth"], &before, &after)?;
    let coding = document
        .get("coding")
        .cloned()
        .and_then(|coding| serde_yaml::from_value(coding).ok())
        .unwrap_or_default();
    Ok((next, after, coding))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_once_approval_lasts_until_the_turn_ends_or_it_times_out() {
        let mut grants = OnceGrants::default();
        assert!(!grants.active("grok", 0));
        grants.approve("grok_acp", 1_000);
        assert!(grants.active("grok", 1_000 + ONCE_TTL_MS - 1));
        assert!(!grants.active("codex", 1_000));
        assert!(grants.finish("grok"));
        assert!(!grants.active("grok", 1_001));
        grants.approve("codex", 0);
        assert!(!grants.active("codex", ONCE_TTL_MS), "expired");
        assert!(!grants.finish("codex"), "an expired grant is already gone");
    }

    #[test]
    fn the_question_names_the_cli_its_reason_the_key_and_three_choices() {
        let (question, options, context) = question(
            "grok",
            ServiceFailure::Authentication,
            "Your session has expired. Run `grok login` to sign in again.",
        );
        assert_eq!(
            question,
            "Grok: Your session has expired. Run `grok login` to sign in again. Use Magician's xAI API key instead? API charges apply."
        );
        let ids: Vec<&str> = options.iter().map(|option| option.id.as_str()).collect();
        assert_eq!(ids, ["once", "always", "no"]);
        assert!(options[2].label.contains("grok login"));
        assert_eq!(context["health_issue"], "authentication");
        assert_eq!(context["env"][0], "XAI_API_KEY");
        assert_eq!(KeyDecision::from_choice("once"), KeyDecision::Once);
        assert_eq!(KeyDecision::from_choice("always"), KeyDecision::Always);
        assert_eq!(KeyDecision::from_choice("deny"), KeyDecision::No);
        assert!(!KeyDecision::No.approved());
    }

    #[test]
    fn only_login_first_account_failures_with_a_key_are_offered() {
        let mut login_only = HarnessAuthSettings::default();
        login_only.mode = HarnessAuthMode::LoginOnly;
        super::super::test_override::with_sign_in(
            login_only,
            |_| false,
            || {
                assert!(!offers_key("grok", ServiceFailure::Authentication));
            },
        );
        let mut magician = HarnessAuthSettings::default();
        magician.mode = HarnessAuthMode::Magician;
        super::super::test_override::with_sign_in(
            magician,
            |_| false,
            || {
                assert!(
                    !offers_key("grok", ServiceFailure::Authentication),
                    "already on the key"
                );
            },
        );
        super::super::test_override::with_sign_in(
            HarnessAuthSettings::default(),
            |_| false,
            || {
                assert!(!offers_key("pi", ServiceFailure::Authentication));
                assert!(!offers_key("grok", ServiceFailure::Unavailable));
                assert!(!offers_key("magician", ServiceFailure::Authentication));
            },
        );
    }

    #[test]
    fn always_writes_one_override_and_keeps_the_owners_comments() {
        let config = "execution:\n  # owner note\n  harness_auth:\n    mode: login_first\ncoding:\n  claude:\n    use_api_key: false\n";
        let (next, after, _) = with_engine_mode(config, "grok", HarnessAuthMode::Magician).unwrap();
        assert!(next.contains("  # owner note\n"), "{next}");
        assert_eq!(after.engines.get("grok"), Some(&HarnessAuthMode::Magician));
        assert_eq!(after.mode, HarnessAuthMode::LoginFirst);
        let reread: serde_yaml::Value = serde_yaml::from_str(&next).unwrap();
        assert_eq!(
            reread["execution"]["harness_auth"]["engines"]["grok"],
            serde_yaml::Value::String("magician".into())
        );
    }
}
