//! Typed authentication challenges (secure HITL plan §5.1, P4 Task 4.1).
//!
//! A challenge is raised from a *bound observation* — the response the
//! request's own destination gave, the typed result a governed program's
//! declared prompt produced — never from arbitrary text. It names what is
//! asked for, where the answer will go, and a fresh id. The next secure ask
//! the run raises carries the id and destination in its spec, the material
//! the answer becomes is registered bound to them, and a delivery that names
//! another destination or challenge is refused by the store.
use crate::magician_v2::user_requests::SensitiveKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// One observed authentication challenge. Value-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticationChallenge {
    /// What the destination asked for.
    pub kind: SensitiveKind,
    /// Where the answer will be delivered, in the vocabulary the delivering
    /// adapter uses for its claim: the origin (`scheme://host[:port]`) for
    /// HTTP and the browser, the program path for a CLI.
    pub destination: String,
    /// Fresh per observation: a new challenge invalidates material bound to
    /// an older one.
    pub challenge_id: String,
    /// What was observed, for the audit line and the model — never a value.
    pub evidence: String,
}

impl AuthenticationChallenge {
    fn new(kind: SensitiveKind, destination: String, evidence: String) -> Self {
        let challenge_id = format!(
            "{}:{destination}:{}",
            kind_name(kind),
            uuid::Uuid::new_v4().simple()
        );
        Self {
            kind,
            destination,
            challenge_id,
            evidence,
        }
    }

    /// A challenge from an HTTP response: `401` with a `WWW-Authenticate`
    /// header, from the host the request was sent to. A bare `401` or body
    /// text is not evidence. `Basic` asks for a password; any other scheme
    /// (`Bearer`, `Digest`, a vendor scheme) is `other` material — a token or
    /// key the user holds — which the treasurer path or a secure ask supplies.
    pub fn from_http_response(
        url: &str,
        status: u16,
        headers: &HashMap<String, String>,
    ) -> Option<Self> {
        if status != 401 {
            return None;
        }
        let www_authenticate = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("www-authenticate"))
            .map(|(_, value)| value.trim())
            .filter(|value| !value.is_empty())?;
        let origin = crate::magician_v2::execution::native_executors::http_origin(url)?;
        let scheme = www_authenticate
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches(',')
            .to_ascii_lowercase();
        let kind = if scheme == "basic" {
            SensitiveKind::Password
        } else {
            SensitiveKind::Other
        };
        Some(Self::new(
            kind,
            origin,
            format!("HTTP 401 with WWW-Authenticate {scheme}"),
        ))
    }

    /// A challenge a governed program's declared login prompt produced (the
    /// CLI lane's `authentication_required` result). Fed by the governed
    /// runtime's own record of the prompt it matched
    /// (`LifecycleAuthenticationChallenge`), never by JSON a pack printed:
    /// text a tool returned is not an observation.
    pub fn from_tool_result_json(value: &Value) -> Option<Self> {
        if value.get("status").and_then(Value::as_str) != Some("authentication_required") {
            return None;
        }
        let kind = match value.get("kind").and_then(Value::as_str)? {
            "password" => SensitiveKind::Password,
            "otp" => SensitiveKind::Otp,
            "username" => SensitiveKind::LoginIdentifier,
            _ => return None,
        };
        let program = value.get("program").and_then(Value::as_str)?.to_string();
        let prompt = value
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Some(Self::new(
            kind,
            program.clone(),
            format!("`{program}` prompted `{prompt}`"),
        ))
    }
}

pub fn kind_name(kind: SensitiveKind) -> &'static str {
    match kind {
        SensitiveKind::LoginIdentifier => "login_identifier",
        SensitiveKind::Password => "password",
        SensitiveKind::Otp => "otp",
        SensitiveKind::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_401_with_a_challenge_header_from_the_host_is_a_challenge() {
        let headers = HashMap::from([(
            "WWW-Authenticate".to_string(),
            "Basic realm=\"ops\"".to_string(),
        )]);
        let challenge = AuthenticationChallenge::from_http_response(
            "https://api.example.test/v1",
            401,
            &headers,
        )
        .expect("basic challenge");
        assert_eq!(challenge.kind, SensitiveKind::Password);
        assert_eq!(challenge.destination, "https://api.example.test");
        assert!(challenge
            .challenge_id
            .starts_with("password:https://api.example.test:"));
        // A bearer scheme asks for a token, not a password.
        let bearer = HashMap::from([(
            "www-authenticate".to_string(),
            "Bearer realm=\"x\", error=\"invalid_token\"".to_string(),
        )]);
        assert_eq!(
            AuthenticationChallenge::from_http_response(
                "https://api.example.test/v1",
                401,
                &bearer
            )
            .unwrap()
            .kind,
            SensitiveKind::Other
        );
        // A bare 401, a 403, or a challenge on a 200 is not evidence.
        assert!(AuthenticationChallenge::from_http_response(
            "https://api.example.test/v1",
            401,
            &HashMap::new()
        )
        .is_none());
        assert!(AuthenticationChallenge::from_http_response(
            "https://api.example.test/v1",
            403,
            &headers
        )
        .is_none());
        assert!(AuthenticationChallenge::from_http_response(
            "https://api.example.test/v1",
            200,
            &headers
        )
        .is_none());
        // Two observations are two challenges.
        let again = AuthenticationChallenge::from_http_response(
            "https://api.example.test/v1",
            401,
            &headers,
        )
        .unwrap();
        assert_ne!(again.challenge_id, challenge.challenge_id);
    }

    #[test]
    fn a_governed_programs_typed_result_is_a_challenge() {
        let value = serde_json::json!({
            "status": "authentication_required",
            "kind": "otp",
            "program": "/usr/bin/vendor-cli",
            "prompt": "Code:",
        });
        let challenge = AuthenticationChallenge::from_tool_result_json(&value).unwrap();
        assert_eq!(challenge.kind, SensitiveKind::Otp);
        assert_eq!(challenge.destination, "/usr/bin/vendor-cli");
        assert!(challenge.evidence.contains("Code:"));
        assert!(AuthenticationChallenge::from_tool_result_json(
            &serde_json::json!({"status":"ok"})
        )
        .is_none());
        assert!(AuthenticationChallenge::from_tool_result_json(
            &serde_json::json!({"status":"authentication_required","kind":"operator","program":"x"})
        )
        .is_none());
    }
}
