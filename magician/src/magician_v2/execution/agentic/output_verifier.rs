//! Schema-validated output verification (reliability pattern #9).
//!
//! Builds on the existing
//! [`crate::magician_v2::agents::types::ArtifactDeclaration::schema`]
//! field — agent / pack authors can declare a JSON-Schema-shaped
//! contract for each artifact the agent is expected to produce, and
//! the runtime validates `goal_reached` artifacts against it BEFORE
//! treating the call as success.
//!
//! ## Why a hand-rolled subset and not a full JSON Schema engine?
//!
//! Magician's existing dependency tree intentionally avoids
//! `jsonschema` (which pulls in ~10 transitives). The practical 80% of
//! agent output validation needs are:
//!
//! - "is this an object?" / "is this an array?"
//! - "does it have these required keys?"
//! - "are these specific string fields long enough / short enough?"
//! - "do these string fields match this regex?"
//! - "do these enum fields use one of the allowed values?"
//! - "does this array have at least N / at most N items?"
//!
//! This module supports exactly that subset, anchored on the
//! standard JSON Schema vocabulary so authors recognise the shape
//! and a future swap to a full engine is a drop-in. Unsupported
//! features (`oneOf`/`anyOf`/`$ref`/numeric `minimum`/etc.) are
//! ignored — not silently passed, but not enforced either. Authors
//! get a warning at first-use; the runtime continues.
//!
//! ## Output shape
//!
//! Validation returns a list of [`VerificationFailure`] entries with:
//!
//! - `path` — JSON Pointer to the offending location (e.g.,
//!   `/answers/q5/value`)
//! - `reason` — short, machine-parseable cause (`"missing_required"`,
//!   `"type_mismatch"`, `"pattern_mismatch"`, …)
//! - `detail` — human-readable explanation suitable for inclusion in
//!   the `goal_reached` rejection message.
//!
//! Callers compose these into a structured rejection that the LLM
//! sees — "schema validation failed at /answers/q5/value: missing
//! required field" — instead of a generic "evidence not sufficient."
//! The model then has a concrete next-action: produce q5.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One offending point detected during schema validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationFailure {
    /// JSON Pointer (RFC 6901) to the offending value within the
    /// artifact being verified. Empty string when the failure is at
    /// the root.
    pub path: String,
    /// Short reason token. Stable across releases so dashboards /
    /// eval pipelines can group failures by class.
    pub reason: &'static str,
    /// Human-readable explanation including the expectation and what
    /// was actually present. Designed for direct inclusion in
    /// `goal_reached` rejection text.
    pub detail: String,
}

impl VerificationFailure {
    fn at(path: &str, reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            path: path.to_string(),
            reason,
            detail: detail.into(),
        }
    }
}

/// Validate `value` against `schema`. Returns `Ok(())` when the
/// artifact satisfies every declared constraint, or `Err(failures)`
/// listing every detected mismatch. The validator accumulates failures
/// rather than short-circuiting so the LLM sees all problems in one
/// rejection message — fewer retry cycles than fix-one-at-a-time.
pub fn verify_value_against_schema(
    value: &Value,
    schema: &Value,
) -> Result<(), Vec<VerificationFailure>> {
    let mut failures = Vec::new();
    validate(&mut failures, "", value, schema);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

/// Render a failure list as a markdown-friendly bullet block suitable
/// for prepending to a `goal_reached` rejection message.
pub fn format_failures(failures: &[VerificationFailure]) -> String {
    let mut out = String::with_capacity(failures.len() * 64);
    for failure in failures {
        if failure.path.is_empty() {
            out.push_str(&format!("- [{}] {}\n", failure.reason, failure.detail));
        } else {
            out.push_str(&format!(
                "- [{}] at `{}`: {}\n",
                failure.reason, failure.path, failure.detail
            ));
        }
    }
    out
}

fn validate(failures: &mut Vec<VerificationFailure>, path: &str, value: &Value, schema: &Value) {
    let Some(schema_obj) = schema.as_object() else {
        // Schema isn't an object — author error. Record a single
        // failure rather than silently passing or panicking.
        failures.push(VerificationFailure::at(
            path,
            "schema_malformed",
            format!(
                "schema entry is not a JSON object (got `{}`)",
                value_kind(schema)
            ),
        ));
        return;
    };

    if let Some(declared_type) = schema_obj.get("type").and_then(Value::as_str) {
        if !value_matches_type(value, declared_type) {
            failures.push(VerificationFailure::at(
                path,
                "type_mismatch",
                format!(
                    "expected JSON type `{}`, got `{}`",
                    declared_type,
                    value_kind(value)
                ),
            ));
            // Skip further constraints — they reference fields of the
            // wrong shape and would produce confusing cascades.
            return;
        }
    }

    if let Some(enum_values) = schema_obj.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|allowed| allowed == value) {
            let allowed_summary = summarise_enum(enum_values);
            failures.push(VerificationFailure::at(
                path,
                "enum_mismatch",
                format!(
                    "value `{}` is not in the allowed set: {}",
                    short_value_excerpt(value),
                    allowed_summary
                ),
            ));
        }
    }

    match value {
        Value::Object(obj) => {
            if let Some(required) = schema_obj.get("required").and_then(Value::as_array) {
                let present_keys: HashSet<&str> = obj.keys().map(String::as_str).collect();
                for required_key in required.iter().filter_map(Value::as_str) {
                    if !present_keys.contains(required_key) {
                        let child_path = join_pointer(path, required_key);
                        failures.push(VerificationFailure::at(
                            &child_path,
                            "missing_required",
                            format!("required key `{}` is missing", required_key),
                        ));
                    }
                }
            }

            if let Some(properties) = schema_obj.get("properties").and_then(Value::as_object) {
                for (key, child_schema) in properties {
                    if let Some(child_value) = obj.get(key) {
                        let child_path = join_pointer(path, key);
                        validate(failures, &child_path, child_value, child_schema);
                    }
                }
            }

            if let Some(min) = schema_obj.get("minProperties").and_then(Value::as_u64) {
                if (obj.len() as u64) < min {
                    failures.push(VerificationFailure::at(
                        path,
                        "min_properties",
                        format!(
                            "object has {} properties, schema requires at least {}",
                            obj.len(),
                            min
                        ),
                    ));
                }
            }
            if let Some(max) = schema_obj.get("maxProperties").and_then(Value::as_u64) {
                if (obj.len() as u64) > max {
                    failures.push(VerificationFailure::at(
                        path,
                        "max_properties",
                        format!(
                            "object has {} properties, schema allows at most {}",
                            obj.len(),
                            max
                        ),
                    ));
                }
            }
        },
        Value::Array(items) => {
            if let Some(min) = schema_obj.get("minItems").and_then(Value::as_u64) {
                if (items.len() as u64) < min {
                    failures.push(VerificationFailure::at(
                        path,
                        "min_items",
                        format!(
                            "array has {} items, schema requires at least {}",
                            items.len(),
                            min
                        ),
                    ));
                }
            }
            if let Some(max) = schema_obj.get("maxItems").and_then(Value::as_u64) {
                if (items.len() as u64) > max {
                    failures.push(VerificationFailure::at(
                        path,
                        "max_items",
                        format!(
                            "array has {} items, schema allows at most {}",
                            items.len(),
                            max
                        ),
                    ));
                }
            }
            if let Some(item_schema) = schema_obj.get("items") {
                for (idx, item) in items.iter().enumerate() {
                    let child_path = format!("{path}/{idx}");
                    validate(failures, &child_path, item, item_schema);
                }
            }
        },
        Value::String(s) => {
            if let Some(min) = schema_obj.get("minLength").and_then(Value::as_u64) {
                if (s.chars().count() as u64) < min {
                    failures.push(VerificationFailure::at(
                        path,
                        "min_length",
                        format!(
                            "string has {} chars, schema requires at least {}",
                            s.chars().count(),
                            min
                        ),
                    ));
                }
            }
            if let Some(max) = schema_obj.get("maxLength").and_then(Value::as_u64) {
                if (s.chars().count() as u64) > max {
                    failures.push(VerificationFailure::at(
                        path,
                        "max_length",
                        format!(
                            "string has {} chars, schema allows at most {}",
                            s.chars().count(),
                            max
                        ),
                    ));
                }
            }
            if let Some(pattern) = schema_obj.get("pattern").and_then(Value::as_str) {
                match regex::Regex::new(pattern) {
                    Ok(re) if !re.is_match(s) => {
                        failures.push(VerificationFailure::at(
                            path,
                            "pattern_mismatch",
                            format!(
                                "string `{}` does not match required pattern `{}`",
                                short_value_excerpt(value),
                                pattern
                            ),
                        ));
                    },
                    Ok(_) => {},
                    Err(err) => {
                        failures.push(VerificationFailure::at(
                            path,
                            "schema_malformed",
                            format!("invalid regex in `pattern`: {err}"),
                        ));
                    },
                }
            }
            if let Some(forbidden) = schema_obj
                .get("forbiddenSubstrings")
                .and_then(Value::as_array)
            {
                for needle in forbidden.iter().filter_map(Value::as_str) {
                    if s.contains(needle) {
                        failures.push(VerificationFailure::at(
                            path,
                            "forbidden_substring",
                            format!(
                                "string contains forbidden substring `{}` (often a hallucinated refusal — produce a real answer)",
                                needle
                            ),
                        ));
                    }
                }
            }
            if let Some(required_subs) = schema_obj
                .get("requiredSubstrings")
                .and_then(Value::as_array)
            {
                for needle in required_subs.iter().filter_map(Value::as_str) {
                    if !s.contains(needle) {
                        failures.push(VerificationFailure::at(
                            path,
                            "required_substring",
                            format!("string is missing required substring `{}`", needle),
                        ));
                    }
                }
            }
        },
        Value::Number(_) => {
            if let Some(min) = schema_obj.get("minimum").and_then(Value::as_f64) {
                if let Some(actual) = value.as_f64() {
                    if actual < min {
                        failures.push(VerificationFailure::at(
                            path,
                            "minimum",
                            format!("number {} is below minimum {}", actual, min),
                        ));
                    }
                }
            }
            if let Some(max) = schema_obj.get("maximum").and_then(Value::as_f64) {
                if let Some(actual) = value.as_f64() {
                    if actual > max {
                        failures.push(VerificationFailure::at(
                            path,
                            "maximum",
                            format!("number {} exceeds maximum {}", actual, max),
                        ));
                    }
                }
            }
        },
        _ => {},
    }
}

fn value_matches_type(value: &Value, declared: &str) -> bool {
    matches!(
        (declared, value),
        ("object", Value::Object(_))
            | ("array", Value::Array(_))
            | ("string", Value::String(_))
            | ("number", Value::Number(_))
            | ("integer", Value::Number(_))
            | ("boolean", Value::Bool(_))
            | ("null", Value::Null)
    ) && (declared != "integer" || value.is_i64() || value.is_u64())
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn short_value_excerpt(value: &Value) -> String {
    let raw = serde_json::to_string(value).unwrap_or_default();
    if raw.chars().count() > 80 {
        let head: String = raw.chars().take(77).collect();
        format!("{head}…")
    } else {
        raw
    }
}

fn summarise_enum(values: &[Value]) -> String {
    if values.len() <= 6 {
        values
            .iter()
            .map(short_value_excerpt)
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        let head: Vec<String> = values.iter().take(5).map(short_value_excerpt).collect();
        format!("{}, … ({} more)", head.join(", "), values.len() - 5)
    }
}

fn join_pointer(parent: &str, key: &str) -> String {
    let escaped = key.replace('~', "~0").replace('/', "~1");
    format!("{parent}/{escaped}")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn type_mismatch_at_root_records_one_failure() {
        let schema = json!({"type": "object"});
        let value = json!("not an object");
        let err = verify_value_against_schema(&value, &schema).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].reason, "type_mismatch");
    }

    #[test]
    fn missing_required_field_path_is_pointer_to_missing_key() {
        let schema = json!({
            "type": "object",
            "required": ["q1", "q2", "q3", "q4", "q5"]
        });
        let value = json!({ "q1": "a", "q2": "b", "q3": "c", "q4": "d" });
        let err = verify_value_against_schema(&value, &schema).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].reason, "missing_required");
        assert_eq!(err[0].path, "/q5");
        assert!(err[0].detail.contains("q5"));
    }

    #[test]
    fn accumulates_multiple_failures_in_one_pass() {
        let schema = json!({
            "type": "object",
            "required": ["q1", "q2"],
            "properties": {
                "q1": {"type": "string", "minLength": 5},
                "q2": {"type": "number"}
            }
        });
        let value = json!({ "q1": "hi", "q2": "not a number" });
        let err = verify_value_against_schema(&value, &schema).unwrap_err();
        assert_eq!(err.len(), 2);
        let reasons: Vec<&str> = err.iter().map(|f| f.reason).collect();
        assert!(reasons.contains(&"min_length"), "got {reasons:?}");
        assert!(reasons.contains(&"type_mismatch"), "got {reasons:?}");
    }

    #[test]
    fn pattern_mismatch_surfaces_in_failures() {
        let schema = json!({
            "type": "string",
            "pattern": "^answer:\\s+"
        });
        let value = json!("Sorry I cannot help with that");
        let err = verify_value_against_schema(&value, &schema).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].reason, "pattern_mismatch");
    }

    #[test]
    fn forbidden_substrings_catch_hedged_refusals() {
        // Catches the "as an AI I cannot" hallucination flavor where
        // the agent claims goal_reached with a string that's really
        // a refusal.
        let schema = json!({
            "type": "string",
            "forbiddenSubstrings": ["I cannot", "as an AI", "unable to provide"]
        });
        let value = json!("As an AI, I cannot answer that question.");
        let err = verify_value_against_schema(&value, &schema).unwrap_err();
        let reasons: Vec<&str> = err.iter().map(|f| f.reason).collect();
        assert!(
            reasons
                .iter()
                .filter(|r| **r == "forbidden_substring")
                .count()
                >= 1,
            "expected at least one forbidden_substring failure; got {reasons:?}"
        );
    }

    #[test]
    fn required_substrings_demand_real_content() {
        let schema = json!({
            "type": "string",
            "requiredSubstrings": ["Q1:", "Q2:", "Q3:"]
        });
        let value = json!("Q1: 12 Q3: 4");
        let err = verify_value_against_schema(&value, &schema).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].reason, "required_substring");
        assert!(err[0].detail.contains("Q2:"));
    }

    #[test]
    fn array_min_max_items_enforced() {
        let schema = json!({
            "type": "array",
            "minItems": 3,
            "maxItems": 5
        });
        let too_few = verify_value_against_schema(&json!([1, 2]), &schema).unwrap_err();
        assert_eq!(too_few[0].reason, "min_items");
        let too_many =
            verify_value_against_schema(&json!([1, 2, 3, 4, 5, 6]), &schema).unwrap_err();
        assert_eq!(too_many[0].reason, "max_items");
        let just_right = verify_value_against_schema(&json!([1, 2, 3, 4]), &schema);
        assert!(just_right.is_ok());
    }

    #[test]
    fn nested_properties_validated_recursively() {
        let schema = json!({
            "type": "object",
            "properties": {
                "answers": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["question", "answer"]
                    }
                }
            }
        });
        let value = json!({
            "answers": [
                { "question": "Q1", "answer": "A1" },
                { "question": "Q2" }
            ]
        });
        let err = verify_value_against_schema(&value, &schema).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].reason, "missing_required");
        assert_eq!(err[0].path, "/answers/1/answer");
    }

    #[test]
    fn enum_mismatch_lists_allowed_values() {
        let schema = json!({
            "type": "string",
            "enum": ["small", "medium", "large"]
        });
        let err = verify_value_against_schema(&json!("xl"), &schema).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].reason, "enum_mismatch");
        assert!(err[0].detail.contains("small"));
    }

    #[test]
    fn passes_when_value_matches_schema_completely() {
        let schema = json!({
            "type": "object",
            "required": ["q1", "q2", "q3", "q4", "q5"],
            "properties": {
                "q1": {"type": "string", "minLength": 1},
                "q2": {"type": "string", "minLength": 1},
                "q3": {"type": "string", "minLength": 1},
                "q4": {"type": "string", "minLength": 1},
                "q5": {"type": "string", "minLength": 1}
            }
        });
        let value = json!({
            "q1": "a", "q2": "b", "q3": "c", "q4": "d", "q5": "e"
        });
        assert!(verify_value_against_schema(&value, &schema).is_ok());
    }

    #[test]
    fn json_pointer_escapes_slashes_and_tildes_in_keys() {
        let schema = json!({
            "type": "object",
            "required": ["a/b"]
        });
        let value = json!({});
        let err = verify_value_against_schema(&value, &schema).unwrap_err();
        // `/` in key → `~1`, per RFC 6901.
        assert_eq!(err[0].path, "/a~1b");
    }

    #[test]
    fn format_failures_renders_bullets() {
        let failures = vec![
            VerificationFailure::at("/q5", "missing_required", "required key `q5` is missing"),
            VerificationFailure::at(
                "/q1",
                "min_length",
                "string has 2 chars, schema requires at least 5",
            ),
        ];
        let rendered = format_failures(&failures);
        assert!(rendered.contains("[missing_required] at `/q5`"));
        assert!(rendered.contains("[min_length] at `/q1`"));
    }
}
