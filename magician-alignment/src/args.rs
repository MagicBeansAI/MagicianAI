//! The canonical form of an act's arguments: the evidence a judge reads and
//! the input to an act's binding. Keys sort, long strings are cut with a
//! marker, and host-injected `__*` parameters are left out.

use std::collections::HashMap;

use serde_json::Value;

/// Longest string kept in `normalized_args`. Arguments are evidence for the
/// judge, not a payload to replay; a long body is cut with a marker.
pub const MAX_ARG_STRING_CHARS: usize = 2_000;

/// Arguments in canonical key order with long strings cut. Host-injected
/// `__*` parameters are runtime identity, not part of the act, and are left
/// out.
pub fn normalized_args(params: &HashMap<String, Value>) -> Value {
    let mut sorted: Vec<(&String, &Value)> = params
        .iter()
        .filter(|(key, _)| !key.starts_with("__"))
        .collect();
    sorted.sort_by(|left, right| left.0.cmp(right.0));
    Value::Object(
        sorted
            .into_iter()
            .map(|(key, value)| (key.clone(), bounded_value(value)))
            .collect(),
    )
}

fn bounded_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > MAX_ARG_STRING_CHARS => {
            let mut cut: String = text.chars().take(MAX_ARG_STRING_CHARS).collect();
            cut.push_str("…[truncated]");
            Value::String(cut)
        },
        Value::Array(items) => Value::Array(items.iter().map(bounded_value).collect()),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), bounded_value(&map[key])))
                    .collect(),
            )
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalized_args_are_canonical_bounded_and_drop_host_identity() {
        let params = HashMap::from([
            ("b".to_string(), json!({"z": 1, "a": 2})),
            ("a".to_string(), json!("x".repeat(MAX_ARG_STRING_CHARS + 5))),
            ("__execution_id".to_string(), json!("exec-1")),
        ]);
        let normalized = normalized_args(&params);
        let keys: Vec<&String> = normalized.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["a", "b"]);
        assert!(normalized["a"].as_str().unwrap().ends_with("…[truncated]"));
    }
}
