//! A harness turn that failed for its CLI's sign-in, credits, quota, rate
//! limit or reachability, in the CLI's own words.
//!
//! [`HarnessFailure::classify`] reads what the CLI printed (its final error
//! line, stderr tail, or the launch error), keeps the one line that says why
//! — bounded and secrets-redacted — and pairs it with a remedy (`grok login`,
//! top up credits, wait for the limit). The chat reply, the run's failure
//! reason, the Attention notice and the key-fallback question all carry it,
//! so nobody sees a bare "the harness turn failed".
//!
//! The last authentication failure per CLI is remembered in memory so the
//! settings status can say "Session expired" for a login file that still
//! exists but the CLI's server rejected; a successful turn or a newer login
//! file clears it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::magician_v2::realtime_events::ServiceFailure;

use super::{canonical_engine, SignIn, SignInState};

/// The most of a CLI's reason a reply or notice carries.
pub const MAX_REASON_CHARS: usize = 300;

/// A failed harness turn, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessFailure {
    /// Roster name (`claude_code`, `codex`, `grok`, `agy`, `pi`).
    pub engine: String,
    pub class: ServiceFailure,
    /// The CLI's own reason: one line, bounded, secrets redacted.
    pub reason: String,
    /// What the owner can do about it.
    pub remedy: String,
}

/// The CLI's display name.
pub fn engine_label(engine: &str) -> &'static str {
    match canonical_engine(engine) {
        "claude_code" => "Claude Code",
        "codex" => "Codex",
        "grok" => "Grok",
        "agy" => "Antigravity",
        "pi" => "Pi",
        _ => "The harness",
    }
}

/// Whose API Magician's key for `engine` bills.
pub fn provider_label(engine: &str) -> &'static str {
    match canonical_engine(engine) {
        "claude_code" => "Anthropic",
        "codex" => "OpenAI",
        "grok" => "xAI",
        "agy" => "Google",
        _ => "provider",
    }
}

/// How the owner signs `engine`'s CLI in again.
pub fn sign_in_remedy(engine: &str) -> &'static str {
    match canonical_engine(engine) {
        "claude_code" => "Run `claude` and sign in with `/login`.",
        "codex" => "Run `codex login`.",
        "grok" => "Run `grok login`.",
        "agy" => "Open Antigravity (`agy`) and sign in again.",
        "pi" => "Run `pi` and sign in with `/login`, or pick a Magician profile for Pi.",
        _ => "Sign in to the CLI again.",
    }
}

fn remedy(engine: &str, class: ServiceFailure, on_magician_key: bool) -> String {
    let label = engine_label(engine);
    let provider = provider_label(engine);
    match class {
        ServiceFailure::Authentication if on_magician_key => format!(
            "Check Magician's {provider} API key in the runtime environment (Settings → Skills → Environment)."
        ),
        ServiceFailure::Authentication => sign_in_remedy(engine).to_string(),
        ServiceFailure::Credit if on_magician_key => {
            format!("Top up the {provider} account behind Magician's key, or check its billing.")
        },
        ServiceFailure::Credit => format!(
            "Top up credits, or check the plan and billing of the account {label} is signed in to."
        ),
        ServiceFailure::RateLimit => {
            "Wait for the limit to reset, or switch this chat or run to another engine.".to_string()
        },
        ServiceFailure::Unavailable => {
            "Check the network connection and the provider's status, then retry.".to_string()
        },
    }
}

impl HarnessFailure {
    /// Classify what a failed turn printed (`texts`: its reply or final error
    /// line, the CLI's diagnostics, the launch error). `None` when nothing in
    /// it is an auth, credit, quota, rate-limit or reachability failure.
    pub fn classify(engine: &str, texts: &[&str]) -> Option<Self> {
        let joined = texts
            .iter()
            .map(|text| text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let class = ServiceFailure::from_error(&joined)?;
        let engine = canonical_engine(engine).to_string();
        let reason = reason_line(&joined, class).unwrap_or_else(|| match class {
            ServiceFailure::Authentication => {
                format!("{} could not authenticate.", engine_label(&engine))
            },
            ServiceFailure::Credit => {
                "The account is out of credits or over its billing limit.".into()
            },
            ServiceFailure::RateLimit => "The account hit a rate or usage limit.".into(),
            ServiceFailure::Unavailable => "The provider could not be reached.".into(),
        });
        let on_key = super::uses_magician_credentials(&engine);
        Some(Self {
            remedy: remedy(&engine, class, on_key),
            engine,
            class,
            reason,
        })
    }

    /// The user-visible failure: who, the CLI's reason, and the remedy.
    pub fn user_message(&self) -> String {
        let mut reason = self.reason.trim().to_string();
        if !reason.ends_with(['.', '!', '?', '…']) {
            reason.push('.');
        }
        format!(
            "{} could not finish this turn: {reason} {}",
            engine_label(&self.engine),
            self.remedy
        )
    }

    /// `text` (what the turn left) led by the user-visible failure, unless it
    /// already is only the CLI's reason.
    pub fn annotate(&self, text: &str) -> String {
        let text = text.trim();
        let message = self.user_message();
        if text.is_empty() || self.reason.contains(text) || text.contains(&message) {
            return message;
        }
        let kept: String = text.chars().take(2_000).collect();
        format!("{message}\n\n{kept}")
    }

    /// Whether this failure is about the account the CLI runs on — where
    /// Magician's own key could stand in, after the owner approves.
    pub fn is_account_failure(&self) -> bool {
        matches!(
            self.class,
            ServiceFailure::Authentication | ServiceFailure::Credit | ServiceFailure::RateLimit
        )
    }
}

/// The line of `text` that says why, preferring the last one of `class` (a
/// CLI prints its refusal last). A JSON line is read for its message field.
fn reason_line(text: &str, class: ServiceFailure) -> Option<String> {
    let candidates: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| json_message(line).unwrap_or_else(|| line.to_string()))
        .collect();
    let of_class = candidates
        .iter()
        .rev()
        .find(|line| ServiceFailure::from_error(line) == Some(class));
    let any = || {
        candidates
            .iter()
            .rev()
            .find(|line| ServiceFailure::from_error(line).is_some())
    };
    of_class
        .or_else(any)
        .map(|line| bounded_reason(without_error_label(line)))
        .filter(|line| !line.is_empty())
}

/// The CLI's words without a leading `Error:` label: the user message already
/// says the turn failed.
fn without_error_label(line: &str) -> &str {
    ["error:", "error -", "fatal:"]
        .iter()
        .find_map(|label| {
            line.get(..label.len())
                .filter(|head| head.eq_ignore_ascii_case(label))
                .map(|_| line[label.len()..].trim_start())
        })
        .filter(|rest| !rest.is_empty())
        .unwrap_or(line)
}

fn json_message(line: &str) -> Option<String> {
    if !line.starts_with('{') {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    [
        "/error/message",
        "/message",
        "/error",
        "/result",
        "/ctx/message",
        "/msg",
    ]
    .iter()
    .find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

/// One line of a CLI's reason, safe to show: credentials redacted, terminal
/// escapes and control characters dropped, whitespace collapsed, bounded.
pub fn bounded_reason(text: &str) -> String {
    let redacted = crate::magician_v2::secrets::sanitize_text_for_provider(text);
    let mut clean = String::with_capacity(redacted.len().min(MAX_REASON_CHARS * 4));
    let mut chars = redacted.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI sequence: ESC [ ... final byte in @..~
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() {
            clean.push(' ');
        } else {
            clean.push(c);
        }
    }
    let collapsed = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_REASON_CHARS {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(MAX_REASON_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The last account failure a turn on a CLI reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedFailure {
    pub class: ServiceFailure,
    pub reason: String,
    pub remedy: String,
    pub at_ms: i64,
}

fn recorded() -> &'static Mutex<HashMap<String, RecordedFailure>> {
    static RECORDED: OnceLock<Mutex<HashMap<String, RecordedFailure>>> = OnceLock::new();
    RECORDED.get_or_init(Mutex::default)
}

/// Remember `failure` for the settings status (account failures only).
pub fn record(failure: &HarnessFailure) {
    if !failure.is_account_failure() {
        return;
    }
    recorded()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            failure.engine.clone(),
            RecordedFailure {
                class: failure.class,
                reason: failure.reason.clone(),
                remedy: failure.remedy.clone(),
                at_ms: chrono::Utc::now().timestamp_millis(),
            },
        );
}

/// A turn on `engine` succeeded: its last failure no longer stands.
pub fn record_success(engine: &str) {
    recorded()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(canonical_engine(engine));
}

/// Record a settled turn's outcome: its failure, or a success.
pub fn observe(
    engine: &str,
    health: Option<Result<(), ServiceFailure>>,
    failure: Option<&HarnessFailure>,
) {
    match (failure, health) {
        (Some(failure), _) => record(failure),
        (None, Some(Ok(()))) => record_success(engine),
        _ => {},
    }
}

pub fn last_failure(engine: &str) -> Option<RecordedFailure> {
    recorded()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(canonical_engine(engine))
        .cloned()
}

/// A login file that still exists but whose CLI was refused authentication
/// since it last changed is expired, in the CLI's words.
pub(super) fn apply_recorded(engine: &str, found: SignIn) -> SignIn {
    apply_failure(found, last_failure(engine))
}

fn apply_failure(found: SignIn, failure: Option<RecordedFailure>) -> SignIn {
    let Some(failure) = failure.filter(|f| f.class == ServiceFailure::Authentication) else {
        return found;
    };
    match found.state {
        SignInState::SignedIn
            if found
                .modified_ms
                .is_none_or(|modified| modified <= failure.at_ms) =>
        {
            SignIn {
                state: SignInState::Expired,
                detail: Some(failure.reason),
                modified_ms: found.modified_ms,
            }
        },
        SignInState::SignedIn => found,
        _ if found.detail.is_none() => SignIn {
            detail: Some(failure.reason),
            ..found
        },
        _ => found,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_cli_failure_class_carries_the_cli_reason_and_a_remedy() {
        let grok = HarnessFailure::classify(
            "grok",
            &[
                "",
                "auth: refreshing\nError: Your session has expired. Run `grok login` to sign in again.",
            ],
        )
        .expect("expired session");
        assert_eq!(grok.class, ServiceFailure::Authentication);
        assert!(grok.reason.contains("session has expired"), "{grok:?}");
        assert!(grok.remedy.contains("grok login"));
        assert!(grok
            .user_message()
            .starts_with("Grok could not finish this turn: Your session"));

        let codex = HarnessFailure::classify(
            "codex_app_server",
            &[r#"{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: token expired"}}"#],
        )
        .unwrap();
        assert_eq!(codex.engine, "codex");
        assert_eq!(codex.class, ServiceFailure::Authentication);
        assert!(
            codex.reason.starts_with("unexpected status 401"),
            "{codex:?}"
        );
        assert!(codex.remedy.contains("codex login"));

        let claude = HarnessFailure::classify("claude", &["Credit balance is too low"]).unwrap();
        assert_eq!(claude.class, ServiceFailure::Credit);
        assert!(claude.remedy.contains("Top up credits"));

        let agy = HarnessFailure::classify(
            "agy",
            &["RESOURCE_EXHAUSTED (code 429): Individual quota reached."],
        )
        .unwrap();
        assert_eq!(agy.class, ServiceFailure::RateLimit);
        assert!(agy.is_account_failure());

        let pi = HarnessFailure::classify("pi", &["Pi turn failed: 402 Payment Required"]).unwrap();
        assert_eq!(pi.class, ServiceFailure::Credit);

        let offline = HarnessFailure::classify(
            "codex",
            &["stream error: failed to lookup address information: nodename nor servname provided"],
        )
        .unwrap();
        assert_eq!(offline.class, ServiceFailure::Unavailable);
        assert!(!offline.is_account_failure());

        assert!(HarnessFailure::classify("grok", &["Action denied by task policy"]).is_none());
    }

    #[test]
    fn reasons_are_bounded_redacted_and_single_line() {
        let long = format!(
            "\u{1b}[31m401 Unauthorized\u{1b}[0m\nfor key sk-ant-api03-{}\t{}",
            "A".repeat(40),
            "x".repeat(1_000)
        );
        let reason = bounded_reason(&long);
        assert!(reason.chars().count() <= MAX_REASON_CHARS);
        assert!(!reason.contains('\u{1b}'));
        assert!(!reason.contains('\n'));
        assert!(!reason.contains(&"A".repeat(40)), "{reason}");
        assert!(reason.starts_with("401 Unauthorized"));
    }

    #[test]
    fn annotate_leads_with_the_failure_and_keeps_other_text() {
        let failure =
            HarnessFailure::classify("grok", &["Your session has expired. Run `grok login`"])
                .unwrap();
        assert_eq!(failure.annotate(""), failure.user_message());
        assert_eq!(
            failure.annotate("Your session has expired. Run `grok login`"),
            failure.user_message()
        );
        let annotated = failure.annotate("partial answer");
        assert!(annotated.starts_with(&failure.user_message()));
        assert!(annotated.ends_with("partial answer"));
    }

    #[test]
    fn a_recorded_auth_failure_expires_an_older_login_until_a_newer_one() {
        let failure = RecordedFailure {
            class: ServiceFailure::Authentication,
            reason: "Your session has expired.".into(),
            remedy: "Run `grok login`.".into(),
            at_ms: 1_000,
        };
        let signed_in = |modified| SignIn {
            state: SignInState::SignedIn,
            detail: None,
            modified_ms: modified,
        };
        let expired = apply_failure(signed_in(Some(900)), Some(failure.clone()));
        assert_eq!(expired.state, SignInState::Expired);
        assert_eq!(expired.detail.as_deref(), Some("Your session has expired."));
        assert_eq!(
            apply_failure(signed_in(Some(1_100)), Some(failure.clone())).state,
            SignInState::SignedIn
        );
        let credit = RecordedFailure {
            class: ServiceFailure::Credit,
            ..failure
        };
        assert_eq!(
            apply_failure(signed_in(Some(900)), Some(credit)).state,
            SignInState::SignedIn
        );
    }

    #[test]
    fn a_success_clears_the_recorded_failure() {
        let engine = "test-engine-failure-record";
        let failure = HarnessFailure {
            engine: engine.into(),
            class: ServiceFailure::Authentication,
            reason: "expired".into(),
            remedy: "sign in".into(),
        };
        observe(
            engine,
            Some(Err(ServiceFailure::Authentication)),
            Some(&failure),
        );
        assert_eq!(last_failure(engine).unwrap().reason, "expired");
        observe(engine, Some(Ok(())), None);
        assert!(last_failure(engine).is_none());
    }
}
