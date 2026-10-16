use std::collections::HashMap;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::types::LearningSkillInvocationFailureClass;

const MAX_OBJECT_FIELDS: usize = 64;
const MAX_ARRAY_ITEMS: usize = 8;
const MAX_DEPTH: usize = 6;

pub fn redacted_input_shape(parameters: &HashMap<String, Value>) -> Value {
    let mut object = Map::new();
    let mut keys: Vec<&String> = parameters.keys().collect();
    keys.sort();
    for key in keys.into_iter().take(MAX_OBJECT_FIELDS) {
        if is_sensitive_key(key) {
            object.insert(key.clone(), json!({"kind": "redacted"}));
        } else if let Some(value) = parameters.get(key) {
            object.insert(key.clone(), shape_for_value(Some(key), value, 0));
        }
    }
    if parameters.len() > MAX_OBJECT_FIELDS {
        object.insert(
            "__truncated_fields".to_string(),
            json!(parameters.len() - MAX_OBJECT_FIELDS),
        );
    }
    Value::Object(object)
}

pub fn fingerprint_input_shape(shape: &Value) -> String {
    let bytes = serde_json::to_vec(shape).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(b"magician-learning-skill-invocation-input-v1\0");
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

pub fn classify_skill_invocation_failure(message: &str) -> LearningSkillInvocationFailureClass {
    let lower = message.to_ascii_lowercase();
    if lower.contains("cancelled") || lower.contains("canceled") {
        return LearningSkillInvocationFailureClass::Cancelled;
    }
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("deadline")
        || lower.contains("elapsed")
    {
        return LearningSkillInvocationFailureClass::Timeout;
    }
    if lower.contains("no budget configured")
        || lower.contains("spend tracking")
        || lower.contains("resource authority")
        || lower.contains("spend token")
        || lower.contains("ceiling")
    {
        return LearningSkillInvocationFailureClass::ResourceAuthorityDenied;
    }
    if lower.contains("hitl")
        || lower.contains("user denied")
        || lower.contains("user rejected")
        || lower.contains("owner denied")
        || lower.contains("owner rejected")
        || lower.contains("confirmation denied")
    {
        return LearningSkillInvocationFailureClass::UserDeniedOrHitlBlocked;
    }
    if lower.contains("unauthorized")
        || lower.contains("unauthorised")
        || lower.contains("forbidden")
        || lower.contains("oauth")
        || lower.contains("login")
        || lower.contains("authentication")
        || lower.contains("authorization")
        || lower.contains("session expired")
        || lower.contains("invalid token")
    {
        return LearningSkillInvocationFailureClass::AuthFailure;
    }
    if lower.contains("connection refused")
        || lower.contains("dns")
        || lower.contains("network")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
    {
        return LearningSkillInvocationFailureClass::NetworkServiceFailure;
    }
    if lower.contains("environment variable")
        || lower.contains("env var")
        || lower.contains("api key")
        || lower.contains("missing key")
        || lower.contains("not configured")
        || lower.contains("configuration")
    {
        return LearningSkillInvocationFailureClass::MissingEnvConfig;
    }
    if lower.contains("schema")
        || lower.contains("invalid arguments")
        || lower.contains("invalid argument")
        || lower.contains("missing field")
        || lower.contains("unknown field")
        || lower.contains("deserialize")
        || lower.contains("deserializ")
    {
        return LearningSkillInvocationFailureClass::BadSchema;
    }
    if lower.contains("parse")
        || lower.contains("invalid json")
        || lower.contains("expected value")
        || lower.contains("yaml")
    {
        return LearningSkillInvocationFailureClass::ParseFailure;
    }
    if lower.contains("capability not found")
        || lower.contains("provider not found")
        || lower.contains("not a compiled pack")
        || lower.contains("unknown tool")
        || lower.contains("no such tool")
    {
        return LearningSkillInvocationFailureClass::CapabilityUnavailable;
    }
    if lower.contains("tool misuse")
        || lower.contains("wrong tool")
        || lower.contains("unsupported action")
        || lower.contains("unsupported command")
    {
        return LearningSkillInvocationFailureClass::ToolMisuse;
    }
    if lower.contains("panic")
        || lower.contains("crash")
        || lower.contains("subprocess")
        || lower.contains("exit code")
        || lower.contains("stderr")
    {
        return LearningSkillInvocationFailureClass::WrapperCrash;
    }
    LearningSkillInvocationFailureClass::Unknown
}

fn shape_for_value(key: Option<&str>, value: &Value, depth: usize) -> Value {
    if key.is_some_and(is_sensitive_key) {
        return json!({"kind": "redacted"});
    }
    if depth >= MAX_DEPTH {
        return json!({"kind": "truncated_depth"});
    }
    match value {
        Value::Null => json!({"kind": "null"}),
        Value::Bool(_) => json!({"kind": "bool"}),
        Value::Number(number) => {
            let number_kind = if number.is_i64() {
                "integer"
            } else if number.is_u64() {
                "unsigned"
            } else {
                "float"
            };
            json!({"kind": "number", "number_kind": number_kind})
        },
        Value::String(value) => json!({"kind": "string", "len": value.chars().count()}),
        Value::Array(items) => {
            let shapes = items
                .iter()
                .take(MAX_ARRAY_ITEMS)
                .map(|item| shape_for_value(None, item, depth + 1))
                .collect::<Vec<_>>();
            json!({
                "kind": "array",
                "len": items.len(),
                "items": shapes,
                "truncated": items.len().saturating_sub(MAX_ARRAY_ITEMS),
            })
        },
        Value::Object(fields) => {
            let mut shaped = Map::new();
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort();
            for key in keys.into_iter().take(MAX_OBJECT_FIELDS) {
                if let Some(value) = fields.get(key) {
                    shaped.insert(key.clone(), shape_for_value(Some(key), value, depth + 1));
                }
            }
            json!({
                "kind": "object",
                "fields": shaped,
                "truncated": fields.len().saturating_sub(MAX_OBJECT_FIELDS),
            })
        },
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    [
        "authorization",
        "password",
        "passwd",
        "secret",
        "token",
        "cookie",
        "credential",
        "api_key",
        "apikey",
        "access_key",
        "private_key",
        "refresh",
        "oauth",
        "bearer",
        "cvv",
        "card",
        "pan",
        "expiry",
        "otp",
        "pin",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn redacted_shape_sorts_and_removes_sensitive_values() {
        let params = HashMap::from([
            ("query".to_string(), json!("coke zero")),
            ("access_token".to_string(), json!("secret-token")),
            ("nested".to_string(), json!({"password": "pw", "count": 3})),
        ]);

        let shape = redacted_input_shape(&params);
        assert_eq!(
            shape
                .get("access_token")
                .and_then(|v| v.get("kind"))
                .and_then(Value::as_str),
            Some("redacted")
        );
        assert_eq!(
            shape
                .pointer("/nested/fields/password/kind")
                .and_then(Value::as_str),
            Some("redacted")
        );
        assert_eq!(
            shape
                .pointer("/nested/fields/count/kind")
                .and_then(Value::as_str),
            Some("number")
        );
        assert_eq!(
            shape
                .get("query")
                .and_then(|v| v.get("kind"))
                .and_then(Value::as_str),
            Some("string")
        );
    }

    #[test]
    fn fingerprint_is_stable_for_same_shape() {
        let params = HashMap::from([
            ("b".to_string(), json!(true)),
            ("a".to_string(), json!("value")),
        ]);
        let first = fingerprint_input_shape(&redacted_input_shape(&params));
        let second = fingerprint_input_shape(&redacted_input_shape(&params));
        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
    }

    #[test]
    fn classifier_groups_common_failures() {
        assert_eq!(
            classify_skill_invocation_failure("No budget configured authorizing shell"),
            LearningSkillInvocationFailureClass::ResourceAuthorityDenied
        );
        assert_eq!(
            classify_skill_invocation_failure("request timed out after 30s"),
            LearningSkillInvocationFailureClass::Timeout
        );
        assert_eq!(
            classify_skill_invocation_failure("OAuth session expired"),
            LearningSkillInvocationFailureClass::AuthFailure
        );
    }
}
