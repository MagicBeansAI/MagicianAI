//! Tiny JSONPath subset for Phase 3 data-flow extraction.
//!
//! Supports two prefixes:
//! - `$.field[.subfield...]` — direct field lookup (chain `.` separators),
//!   including numeric array indexes such as `$.hits[0].objectID`
//! - `$..field` — recursive descent, first match wins
//!
//! Returns `Option<String>` — `None` means missing/unsupported/null. Numbers
//! and booleans stringify; null is treated as missing because workflow
//! parameters are stringly-typed on the wire and a null wouldn't be a useful
//! data-flow value.
//!
//! Anything outside these two forms (wildcards, filters, slices) is not
//! supported in v1. Phase 4 can swap in a full JSONPath crate when the agents
//! start emitting richer paths.

use serde_json::Value;

pub fn extract_jsonpath(value: &Value, path: &str) -> Option<String> {
    extract_jsonpath_value(value, path).and_then(value_to_string)
}

/// Resolve the supported JSONPath subset while preserving the response
/// scalar's JSON type. Data-flow callers use [`extract_jsonpath`] because HTTP
/// request parameters are strings; answer projection uses this typed form so
/// numbers and booleans do not regress into quoted strings.
pub fn extract_jsonpath_value<'value>(value: &'value Value, path: &str) -> Option<&'value Value> {
    if let Some(rest) = path.strip_prefix("$..") {
        find_recursive(value, rest)
    } else if let Some(rest) = path.strip_prefix("$.") {
        find_direct(value, rest)
    } else if let Some(rest) = path.strip_prefix('$').filter(|rest| rest.starts_with('[')) {
        find_direct(value, rest)
    } else {
        tracing::debug!("workflow_replay::jsonpath: unsupported path {path}");
        None
    }
}

/// Validate the exact JSONPath subset understood by [`extract_jsonpath`]
/// without needing a representative response value.
pub fn is_supported_jsonpath(path: &str) -> bool {
    if let Some(target_key) = path.strip_prefix("$..") {
        return !target_key.is_empty() && !target_key.contains('.');
    }
    let rest = if let Some(rest) = path.strip_prefix("$.") {
        rest
    } else if let Some(rest) = path.strip_prefix('$').filter(|rest| rest.starts_with('[')) {
        rest
    } else {
        return false;
    };
    !rest.is_empty() && rest.split('.').all(direct_segment_is_supported)
}

fn direct_segment_is_supported(segment: &str) -> bool {
    if segment.is_empty() || segment.contains('*') || segment.contains('?') || segment.contains(':')
    {
        return false;
    }
    let Some(first_bracket) = segment.find('[') else {
        return true;
    };
    let mut rest = &segment[first_bracket..];
    while !rest.is_empty() {
        if !rest.starts_with('[') {
            return false;
        }
        let Some(close) = rest.find(']') else {
            return false;
        };
        let index_text = &rest[1..close];
        if index_text.is_empty()
            || !index_text
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            return false;
        }
        rest = &rest[close + 1..];
    }
    true
}

fn find_direct<'v>(value: &'v Value, dotted_path: &str) -> Option<&'v Value> {
    let mut current = value;
    for segment in dotted_path.split('.') {
        current = find_direct_segment(current, segment)?;
    }
    Some(current)
}

fn find_direct_segment<'v>(mut current: &'v Value, segment: &str) -> Option<&'v Value> {
    if segment.is_empty() || segment.contains('*') || segment.contains('?') || segment.contains(':')
    {
        return None;
    }

    let Some(first_bracket) = segment.find('[') else {
        return current.get(segment);
    };

    let field = &segment[..first_bracket];
    if !field.is_empty() {
        current = current.get(field)?;
    }

    let mut rest = &segment[first_bracket..];
    while !rest.is_empty() {
        if !rest.starts_with('[') {
            return None;
        }
        let close = rest.find(']')?;
        let index_text = &rest[1..close];
        if index_text.is_empty() || !index_text.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        let index = index_text.parse::<usize>().ok()?;
        current = current.as_array()?.get(index)?;
        rest = &rest[(close + 1)..];
    }

    Some(current)
}

fn find_recursive<'v>(value: &'v Value, target_key: &str) -> Option<&'v Value> {
    if target_key.is_empty() || target_key.contains('.') {
        return None;
    }
    match value {
        Value::Object(map) => {
            if let Some(v) = map.get(target_key) {
                return Some(v);
            }
            for child in map.values() {
                if let Some(hit) = find_recursive(child, target_key) {
                    return Some(hit);
                }
            }
            None
        },
        Value::Array(items) => {
            for child in items {
                if let Some(hit) = find_recursive(child, target_key) {
                    return Some(hit);
                }
            }
            None
        },
        _ => None,
    }
}

fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        Value::Array(_) | Value::Object(_) => Some(v.to_string()),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn direct_field_extraction() {
        let value = json!({"session_token": "sk_abc", "user_id": 42});
        let out = extract_jsonpath(&value, "$.session_token").unwrap();
        assert_eq!(out, "sk_abc");
    }

    #[test]
    fn recursive_descent_first_match() {
        let value = json!({"data": {"nested": {"access_token": "tk_xyz"}}});
        let out = extract_jsonpath(&value, "$..access_token").unwrap();
        assert_eq!(out, "tk_xyz");
    }

    #[test]
    fn missing_field_returns_none() {
        let value = json!({"a": 1});
        assert!(extract_jsonpath(&value, "$.missing").is_none());
        assert!(extract_jsonpath(&value, "$..missing").is_none());
    }

    #[test]
    fn unsupported_path_returns_none() {
        let value = json!({"a": 1});
        assert!(extract_jsonpath(&value, "weird/path").is_none());
        assert!(extract_jsonpath(&value, "$.items[*]").is_none());
        assert!(extract_jsonpath(&value, "$.items[0:1]").is_none());
        assert!(!is_supported_jsonpath("weird/path"));
        assert!(!is_supported_jsonpath("$.items[*]"));
        assert!(!is_supported_jsonpath("$.items[0:1]"));
        assert!(is_supported_jsonpath("$.items[0].id"));
        assert!(is_supported_jsonpath("$..id"));
    }

    #[test]
    fn direct_path_supports_numeric_array_index() {
        let value = json!({
            "hits": [
                {"objectID": "22238335"},
                {"objectID": "40172033"}
            ]
        });
        let out = extract_jsonpath(&value, "$.hits[0].objectID").unwrap();
        assert_eq!(out, "22238335");
    }

    #[test]
    fn direct_path_supports_terminal_array_index() {
        let value = json!({"items": ["first", "second"]});
        let out = extract_jsonpath(&value, "$.items[1]").unwrap();
        assert_eq!(out, "second");
    }

    #[test]
    fn direct_path_supports_root_array_index() {
        let value = json!([{"id": "first"}, {"id": "second"}]);
        assert_eq!(
            extract_jsonpath(&value, "$[1].id").as_deref(),
            Some("second")
        );
        assert!(is_supported_jsonpath("$[1].id"));
    }

    #[test]
    fn typed_extraction_preserves_numbers_and_booleans() {
        let value = json!({"points": 1582, "active": true});
        assert_eq!(
            extract_jsonpath_value(&value, "$.points"),
            value.get("points")
        );
        assert_eq!(
            extract_jsonpath_value(&value, "$.active"),
            value.get("active")
        );
    }

    #[test]
    fn nested_direct_path() {
        let value = json!({"a": {"b": {"c": "deep"}}});
        let out = extract_jsonpath(&value, "$.a.b.c").unwrap();
        assert_eq!(out, "deep");
    }

    #[test]
    fn extracts_numeric_as_string() {
        let value = json!({"id": 42});
        let out = extract_jsonpath(&value, "$.id").unwrap();
        assert_eq!(out, "42");
    }

    #[test]
    fn extracts_booleans_as_strings_null_as_missing() {
        assert_eq!(
            extract_jsonpath(&json!({"f": true}), "$.f").unwrap(),
            "true"
        );
        assert_eq!(
            extract_jsonpath(&json!({"f": false}), "$.f").unwrap(),
            "false"
        );
        assert!(extract_jsonpath(&json!({"f": null}), "$.f").is_none());
    }
}
