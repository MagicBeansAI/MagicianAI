//! Name- and wording-based secret heuristics.
//!
//! These are **compatibility detection only**: a conservative signal that can
//! raise a value to sensitive when nothing typed says otherwise. Nothing keys
//! its custody, redaction, or vault decision on these alone — the stored
//! sensitivity spec on the request does that. Kept in one place so the
//! agentic executor, the request classifier, and the API agree.

use crate::magician_v2::user_requests::SensitiveKind;

/// Whether a parameter/field name reads as a secret.
///
/// Identifiers, not sentences, so most rules are substrings: `api_key`,
/// `apiKey`, `x-api-key` and `API_KEY` all name a key. `key` alone is not a
/// rule — `sort_key`, `primary_key` and `keyword` are ordinary — so a key is
/// recognised by what kind of key the name says it is.
pub fn is_secret_param_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    // Separators collapse so `api_key`, `api-key`, `api.key`, `apiKey` and
    // `api key` are one identifier.
    let flat: String = lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    lower.contains("password")
        || lower.contains("passwd")
        || lower.contains("passphrase")
        || lower.contains("secret")
        || lower.contains("token")
        || lower.contains("credential")
        || lower.contains("cvv")
        || lower.contains("otp")
        || is_word_match(&lower, "pin")
        || [
            "apikey",
            "accesskey",
            "privatekey",
            "authkey",
            "licensekey",
            "licencekey",
            "signingkey",
            "encryptionkey",
            "masterkey",
            "sessionkey",
            "clientkey",
            "sharedkey",
        ]
        .iter()
        .any(|needle| flat.contains(needle))
}

/// Whether a prompt, placeholder, or hint reads as asking for a secret.
///
/// Prose is matched by whole words: "Which tokens should the summary keep?"
/// and "mind the carbon footprint" are ordinary sentences, while "paste the
/// API token" and "your PIN" are not. (`is_secret_param_name` keeps its
/// substring rules — it classifies identifiers, not sentences.)
pub fn looks_like_secret_prompt_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "password",
        "passcode",
        "passphrase",
        "secret",
        "token",
        "otp",
        "totp",
        "pin",
        "cvv",
        "credential",
        "credentials",
    ]
    .iter()
    .any(|word| is_word_match(&lower, word))
        || lower.contains("api key")
        || lower.contains("access key")
        || lower.contains("private key")
        || lower.contains("license key")
        || lower.contains("licence key")
        || lower.contains("one-time password")
        || lower.contains("verification code")
        || lower.contains("security code")
        || lower.contains("auth code")
        || lower.contains("passcode")
        || lower.contains("mfa code")
        || lower.contains("2fa code")
        || lower.contains("two-factor")
        || lower.contains("two factor")
}

/// Whether a prompt or field name reads as the identifier half of a login.
pub fn looks_like_login_identifier(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("username")
        || lower.contains("user name")
        || lower.contains("user id")
        || lower.contains("email")
        || lower.contains("e-mail")
        || lower.contains("login")
        || lower.contains("account")
}

/// Check if `word` appears as a standalone word in `text` (bounded by
/// non-alphanumeric chars or string edges).
fn is_word_match(text: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = text[start..].find(word) {
        let abs_pos = start + pos;
        let before_ok = abs_pos == 0 || !text.as_bytes()[abs_pos - 1].is_ascii_alphanumeric();
        let end_pos = abs_pos + word.len();
        let after_ok = end_pos >= text.len() || !text.as_bytes()[end_pos].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        start = abs_pos + 1;
    }
    false
}

/// Whether a prompt reads as asking for a one-time code specifically.
pub fn looks_like_otp_prompt_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("one-time password")
        || lower.contains("verification code")
        || lower.contains("security code")
        || lower.contains("auth code")
        || lower.contains("passcode")
        || lower.contains("mfa code")
        || lower.contains("2fa code")
        || lower.contains("two-factor")
        || lower.contains("two factor")
        || is_word_match(&lower, "otp")
        || is_word_match(&lower, "totp")
        || code_with_a_one_time_cue(&lower)
}

/// A bare "code" is a one-time code when the sentence says, BESIDE THE WORD,
/// where it came from or how long it is — "the 6-digit code from your
/// authenticator app", "the code we texted you", "enter the code sent to your
/// email". Wording is the only protection an untyped `text` ask has, and these
/// are the shapes real prompts use.
///
/// "Beside" is the whole rule. A cue anywhere in the sentence made "Which
/// discount code was in the email you forwarded?" a one-time code — which
/// clamps the ask's window to 180 s, makes it critical enough to fan out to
/// every channel, and hands the model a reference it can only spend in a
/// credential sink. "Area code", "zip code" and "which code path" must stay
/// ordinary even in a sentence that happens to mention email.
fn code_with_a_one_time_cue(lower: &str) -> bool {
    const CUES: [&str; 9] = [
        "digit",
        "authenticator",
        "authentication app",
        "we sent",
        "sent to",
        "texted",
        "sms",
        "sign in",
        "log in",
    ];
    const NEAR: usize = 40;
    let mut start = 0;
    while let Some(found) = lower[start..].find("code") {
        let at = start + found;
        start = at + 4;
        let before_ok = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
        let after = at + 4;
        // "code" or "codes", not "codec" or "encoded".
        let word_end = if lower[after..].starts_with('s') {
            after + 1
        } else {
            after
        };
        let after_ok =
            word_end >= lower.len() || !lower.as_bytes()[word_end].is_ascii_alphanumeric();
        if !before_ok || !after_ok {
            continue;
        }
        let window_start = at.saturating_sub(NEAR);
        let window_end = (word_end + NEAR).min(lower.len());
        let window =
            &lower[floor_char_boundary(lower, window_start)..ceil_char_boundary(lower, window_end)];
        if CUES.iter().any(|cue| window.contains(cue)) {
            return true;
        }
    }
    false
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Kind a single collected value has when its prompt wording is the only
/// signal: a one-time code where the wording says so, otherwise `other`.
pub fn kind_from_prompt_wording(text: &str) -> Option<SensitiveKind> {
    if looks_like_otp_prompt_text(text) {
        Some(SensitiveKind::Otp)
    } else if looks_like_secret_prompt_text(text) {
        Some(SensitiveKind::Other)
    } else {
        None
    }
}

/// One form question as the classifier sees it, independent of whether it
/// came from a JSON schema or a typed `FormQuestion`.
pub struct FormFieldSpec<'a> {
    pub id: &'a str,
    pub prompt: &'a str,
    pub input_type: &'a str,
    /// An explicit `sensitive: true` on the question.
    pub flagged: bool,
    /// The question offers options. A question the person ANSWERS BY CHOOSING
    /// collects no free-text value, so its wording must not make it sensitive —
    /// "Which password manager?" is a decision, and masking it would leave the
    /// operator unable to answer at all. A typed `password`/`otp` or an explicit
    /// flag still counts: those are the schema speaking, not the prose.
    pub has_options: bool,
}

/// Classify a form's fields. Typed `password`/`otp` questions and explicitly
/// flagged ones are sensitive by schema; a question whose prompt or id reads
/// as a secret is sensitive by wording. When a password field is present, a
/// sibling text/email question whose id or prompt reads as a login identifier
/// is the identifier half of one authentication bundle (plan §3.3). Returns
/// `(field id, kind)` in question order, then `typed` — whether any decision
/// came from the schema rather than wording.
pub fn classify_form_fields(
    questions: &[FormFieldSpec<'_>],
) -> (Vec<(String, SensitiveKind)>, bool) {
    let mut fields: Vec<(String, SensitiveKind)> = Vec::new();
    let mut typed = false;
    let mut has_password = false;
    for q in questions {
        if q.id.is_empty() {
            continue;
        }
        let names_a_password = |text: &str| is_word_match(&text.to_ascii_lowercase(), "password");
        let kind = match q.input_type {
            "password" => Some(SensitiveKind::Password),
            "otp" => Some(SensitiveKind::Otp),
            _ if q.flagged => Some(SensitiveKind::Other),
            // Everything below is WORDING, and a question with options is a
            // decision whose wording decides nothing.
            _ if q.has_options => None,
            // One-time wording wins over the word "password" ("one-time
            // password" is a code, never saved).
            _ if looks_like_otp_prompt_text(q.prompt) => Some(SensitiveKind::Otp),
            // The `need_user_input` form schema has no typed password field
            // yet, so a question that names a password is one: that is what
            // lets the login-identifier bundle rule fire for model forms.
            _ if names_a_password(q.prompt) || names_a_password(q.id) => {
                Some(SensitiveKind::Password)
            },
            _ if looks_like_secret_prompt_text(q.prompt) || is_secret_param_name(q.id) => {
                Some(SensitiveKind::Other)
            },
            _ => None,
        };
        let Some(kind) = kind else { continue };
        typed |= matches!(q.input_type, "password" | "otp") || q.flagged;
        has_password |= kind == SensitiveKind::Password;
        fields.push((q.id.to_string(), kind));
    }
    if has_password {
        for q in questions {
            if q.id.is_empty() || fields.iter().any(|(id, _)| id == q.id) {
                continue;
            }
            if matches!(q.input_type, "" | "text" | "email")
                && !q.has_options
                && (looks_like_login_identifier(q.id) || looks_like_login_identifier(q.prompt))
            {
                fields.push((q.id.to_string(), SensitiveKind::LoginIdentifier));
            }
        }
    }
    (fields, typed)
}

/// What a planning clarification answer becomes on the record (P3).
///
/// A clarification is durable plan text the planner reads back — and its
/// answer goes through an LLM interpreter on the way — so it can never carry
/// a secret. A free-text question whose wording asks for one (a password, a
/// verification code, an API key: the same whole-word rules every other
/// surface uses) records this marker instead of the answer, and the marker
/// says where the value will come from: a `need_user_input` at execution
/// time, which the run vaults. A question that offers options is a decision
/// and is never withheld; so is an empty answer.
pub const CLARIFICATION_SENSITIVE_ANSWER_WITHHELD: &str =
    "[sensitive answer withheld: collect it at execution time with need_user_input]";

pub fn clarification_answer_for_record(
    question_text: &str,
    has_options: bool,
    answer: String,
) -> String {
    if has_options || answer.trim().is_empty() {
        return answer;
    }
    if kind_from_prompt_wording(question_text).is_some()
        || looks_like_secret_prompt_text(question_text)
    {
        return CLARIFICATION_SENSITIVE_ANSWER_WITHHELD.to_string();
    }
    answer
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn a_key_is_a_secret_by_what_kind_of_key_its_name_says() {
        for name in [
            "api_key",
            "apiKey",
            "API_KEY",
            "x-api-key",
            "api.key",
            "access_key",
            "private_key",
            "auth_key",
            "license_key",
            "signing_key",
            "encryption_key",
            "master_key",
            "session_key",
            "shared_key",
            "client_secret",
            "passphrase",
            "passwd",
            "credentials",
            "bot_token",
            "cvv",
            "pin",
            "otp_code",
        ] {
            assert!(is_secret_param_name(name), "{name} names a secret");
        }
        for name in [
            "sort_key",
            "primary_key",
            "keyword",
            "keyboard",
            "monkey",
            "city",
            "email",
            "username",
            "pinned",
            "spin_count",
            "notes",
        ] {
            assert!(!is_secret_param_name(name), "{name} is ordinary");
        }
        assert!(looks_like_secret_prompt_text("Paste your API key"));
        assert!(looks_like_secret_prompt_text(
            "Enter the private key for the deploy host"
        ));
        assert!(looks_like_secret_prompt_text("Provide your credentials"));
        assert!(!looks_like_secret_prompt_text(
            "Which key metrics should the summary keep?"
        ));
        assert!(!looks_like_secret_prompt_text(
            "Turn the keyboard backlight off"
        ));
    }

    /// For an untyped `text` ask the wording is the ONLY protection, and the
    /// shapes real services use say "code" without ever saying "verification".
    #[test]
    fn a_bare_code_is_one_time_material_when_the_sentence_says_where_it_came_from() {
        for prompt in [
            "Enter the 6-digit code from your authenticator app",
            "What is the code we texted you?",
            "Type the code sent to your email",
            "Paste the code from the SMS to sign in",
        ] {
            assert_eq!(
                kind_from_prompt_wording(prompt),
                Some(SensitiveKind::Otp),
                "{prompt} asks for a one-time code"
            );
        }
        for prompt in [
            "Which code path should the fix take?",
            "What area code should the number use?",
            "Is the zip code required on the form?",
            "Refactor the code in this module",
            // A cue elsewhere in the sentence is not a cue beside the word: this
            // one would otherwise clamp its window to 180 s and fan out to every
            // channel as a critical request.
            "Which discount code should the winter promotion use for the subscribers we sent it to?",
            "Is the codec set correctly?",
        ] {
            assert_eq!(kind_from_prompt_wording(prompt), None, "{prompt} is ordinary");
        }
    }
}

#[cfg(test)]
mod clarification_tests {
    use super::*;

    #[test]
    fn a_secret_worded_clarification_records_a_marker_not_the_answer() {
        assert_eq!(
            clarification_answer_for_record(
                "What is your account password?",
                false,
                "p3-clar-canary-3".into()
            ),
            CLARIFICATION_SENSITIVE_ANSWER_WITHHELD
        );
        assert_eq!(
            clarification_answer_for_record(
                "Enter the verification code we sent",
                false,
                "007123".into()
            ),
            CLARIFICATION_SENSITIVE_ANSWER_WITHHELD
        );
        assert_eq!(
            clarification_answer_for_record(
                "Which city should the report cover?",
                false,
                "Lisbon".into()
            ),
            "Lisbon"
        );
        assert_eq!(
            clarification_answer_for_record(
                "Pin the token to the release or archive it?",
                true,
                "pin".into()
            ),
            "pin",
            "a decision with options is never withheld"
        );
        assert_eq!(
            clarification_answer_for_record("What is your password?", false, "  ".into()),
            "  "
        );
    }
}
