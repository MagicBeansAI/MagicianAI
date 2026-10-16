//! Phase 3 SkipCondition evaluator. Returns:
//! - `Some(true)`: condition matched, step should be SKIPPED.
//! - `Some(false)`: condition did not match, step should execute.
//! - `None`: the source step's response is unavailable. Caller decides
//!   how to handle (current convention: treat as "do not skip" — failing
//!   open beats failing closed for the operator's intent).

use crate::magician_v2::api_mining::workflow::{SkipCondition, SkipOperator};
use crate::magician_v2::api_mining::workflow_replay::jsonpath;
use serde_json::Value;
use std::collections::HashMap;

pub fn evaluate_skip(
    cond: &SkipCondition,
    prior_responses: &HashMap<String, Value>,
) -> Option<bool> {
    let source_body = prior_responses.get(&cond.source_step)?;
    let extracted = jsonpath::extract_jsonpath(source_body, &cond.source_path);

    Some(match cond.operator {
        SkipOperator::Equals => match (&extracted, &cond.value) {
            (Some(s), Value::String(v)) => s == v,
            (Some(s), Value::Bool(v)) => s == &v.to_string(),
            (Some(s), Value::Number(v)) => s == &v.to_string(),
            (None, Value::Null) => true,
            _ => false,
        },
        SkipOperator::NotEquals => match (&extracted, &cond.value) {
            (Some(s), Value::String(v)) => s != v,
            (Some(s), Value::Bool(v)) => s != &v.to_string(),
            (Some(s), Value::Number(v)) => s != &v.to_string(),
            (None, Value::Null) => false,
            _ => true,
        },
        SkipOperator::IsEmpty => extracted.is_none() || extracted.as_deref() == Some(""),
        SkipOperator::IsNotEmpty => extracted.is_some() && extracted.as_deref() != Some(""),
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::workflow::{SkipCondition, SkipOperator};
    use serde_json::json;

    fn prior_with(step: &str, body: serde_json::Value) -> HashMap<String, serde_json::Value> {
        let mut map = HashMap::new();
        map.insert(step.to_string(), body);
        map
    }

    #[test]
    fn equals_matches_string() {
        let cond = SkipCondition {
            source_step: "step_0".to_string(),
            source_path: "$.status".to_string(),
            operator: SkipOperator::Equals,
            value: serde_json::Value::String("ok".to_string()),
        };
        let prior = prior_with("step_0", json!({"status":"ok"}));
        assert!(evaluate_skip(&cond, &prior).unwrap());
    }

    #[test]
    fn not_equals_passes_when_different() {
        let cond = SkipCondition {
            source_step: "step_0".to_string(),
            source_path: "$.status".to_string(),
            operator: SkipOperator::NotEquals,
            value: serde_json::Value::String("ok".to_string()),
        };
        let prior = prior_with("step_0", json!({"status":"error"}));
        assert!(evaluate_skip(&cond, &prior).unwrap());
    }

    #[test]
    fn is_empty_matches_missing_field() {
        let cond = SkipCondition {
            source_step: "step_0".to_string(),
            source_path: "$.requires_2fa".to_string(),
            operator: SkipOperator::IsEmpty,
            value: serde_json::Value::Null,
        };
        let prior = prior_with("step_0", json!({"other":"x"}));
        assert!(evaluate_skip(&cond, &prior).unwrap());
    }

    #[test]
    fn is_not_empty_matches_present_field() {
        let cond = SkipCondition {
            source_step: "step_0".to_string(),
            source_path: "$.token".to_string(),
            operator: SkipOperator::IsNotEmpty,
            value: serde_json::Value::Null,
        };
        let prior = prior_with("step_0", json!({"token":"sk_abc"}));
        assert!(evaluate_skip(&cond, &prior).unwrap());
    }

    #[test]
    fn missing_source_step_returns_none() {
        let cond = SkipCondition {
            source_step: "step_never_seen".to_string(),
            source_path: "$.x".to_string(),
            operator: SkipOperator::Equals,
            value: serde_json::Value::String("ok".to_string()),
        };
        let prior = HashMap::new();
        assert!(evaluate_skip(&cond, &prior).is_none());
    }

    #[test]
    fn equals_with_bool_value_matches() {
        let cond = SkipCondition {
            source_step: "step_0".to_string(),
            source_path: "$.requires_2fa".to_string(),
            operator: SkipOperator::Equals,
            value: serde_json::Value::Bool(false),
        };
        let prior = prior_with("step_0", json!({"requires_2fa": false}));
        assert!(evaluate_skip(&cond, &prior).unwrap());
    }
}
