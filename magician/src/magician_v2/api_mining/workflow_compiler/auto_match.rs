//! Pass 1 of workflow compilation: deterministic cross-step string matching.
//!
//! For every pair `(step_i, step_j)` where `j > i`, search step_j's request
//! params for strings that appear in step_i's response body. When found,
//! emit a `DataFlow` with `InferenceMethod::AutoMatch` and confidence 0.9.
//!
//! Handles ~60% of cases: session tokens, entity ids, pagination cursors,
//! CSRF tokens. Pass 2 (LLM compilation) picks up the rest.

use crate::magician_v2::api_mining::sequence::CapabilitySequence;
use crate::magician_v2::api_mining::workflow::{DataFlow, InferenceMethod};
use ulid::Ulid;

/// Minimum length for a substring to count as a meaningful match. Shorter
/// values produce too many false positives (every "1" in a response body
/// becomes a "data flow").
const MIN_MATCH_LEN: usize = 4;

/// Values that should never count as a data flow even when long enough.
/// Booleans, nulls, and trivial sentinels are noise.
fn is_trivial_value(value: &str) -> bool {
    matches!(
        value,
        "true" | "false" | "null" | "TRUE" | "FALSE" | "NULL" | "None"
    )
}

pub fn infer_auto_data_flows(sequences: &[CapabilitySequence]) -> Vec<DataFlow> {
    let mut flows = Vec::new();
    for sequence in sequences {
        flows.extend(infer_flows_within_sequence(sequence));
    }
    dedupe_by_signature(flows)
}

fn infer_flows_within_sequence(sequence: &CapabilitySequence) -> Vec<DataFlow> {
    let mut out = Vec::new();
    for (i, source) in sequence.steps.iter().enumerate() {
        let source_body = match source.response_body.as_deref() {
            Some(body) if !body.is_empty() => body,
            _ => continue,
        };
        for target in sequence.steps.iter().skip(i + 1) {
            for (param_name, param_value) in &target.request_params {
                if param_value.len() < MIN_MATCH_LEN {
                    continue;
                }
                if is_trivial_value(param_value) {
                    continue;
                }
                if source_body.contains(param_value.as_str()) {
                    let source_path =
                        infer_source_path_for_value(source_body, param_name, param_value);
                    out.push(DataFlow {
                        id: format!("df_{}", Ulid::new()),
                        source_step: format!("step_{}", source.step_index),
                        source_path,
                        target_step: format!("step_{}", target.step_index),
                        target_param: param_name.clone(),
                        inference_method: InferenceMethod::AutoMatch,
                        confidence: 0.9,
                    });
                }
            }
        }
    }
    out
}

fn infer_source_path_for_value(
    source_body: &str,
    target_param: &str,
    target_value: &str,
) -> String {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(source_body) {
        if let Some(key) = find_json_key_for_scalar_value(&parsed, target_value) {
            return format!("$..{key}");
        }
    }
    if let Some(key) = find_nearby_json_key_for_value(source_body, target_value) {
        return format!("$..{key}");
    }
    format!("$..{target_param}")
}

fn find_json_key_for_scalar_value(value: &serde_json::Value, target: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if json_scalar_equals_string(child, target) {
                    return Some(key.clone());
                }
                if let Some(found) = find_json_key_for_scalar_value(child, target) {
                    return Some(found);
                }
            }
            None
        },
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|child| find_json_key_for_scalar_value(child, target)),
        _ => None,
    }
}

fn json_scalar_equals_string(value: &serde_json::Value, target: &str) -> bool {
    match value {
        serde_json::Value::String(text) => text == target,
        serde_json::Value::Number(number) => number.to_string() == target,
        serde_json::Value::Bool(boolean) => boolean.to_string() == target,
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            false
        },
    }
}

fn find_nearby_json_key_for_value(source_body: &str, target_value: &str) -> Option<String> {
    if target_value.is_empty() {
        return None;
    }
    let mut search_start = 0usize;
    while let Some(relative_pos) = source_body[search_start..].find(target_value) {
        let value_pos = search_start + relative_pos;
        if !value_has_json_boundary(source_body, value_pos, target_value.len()) {
            search_start = value_pos + target_value.len();
            continue;
        }
        let prefix = &source_body[..value_pos];
        let Some(colon_pos) = prefix.rfind(':') else {
            search_start = value_pos + target_value.len();
            continue;
        };
        if !source_body[colon_pos + 1..value_pos]
            .chars()
            .all(|ch| ch.is_whitespace() || ch == '"')
        {
            search_start = value_pos + target_value.len();
            continue;
        }
        let before_colon = &source_body[..colon_pos];
        let Some(key_end) = before_colon.rfind('"') else {
            search_start = value_pos + target_value.len();
            continue;
        };
        let Some(key_start) = before_colon[..key_end].rfind('"') else {
            search_start = value_pos + target_value.len();
            continue;
        };
        let key = &before_colon[key_start + 1..key_end];
        if is_reasonable_json_key(key) {
            return Some(key.to_string());
        }
        search_start = value_pos + target_value.len();
    }
    None
}

fn value_has_json_boundary(source_body: &str, value_pos: usize, value_len: usize) -> bool {
    let before = source_body[..value_pos]
        .chars()
        .rev()
        .find(|ch| !ch.is_whitespace());
    let after = source_body[value_pos + value_len..]
        .chars()
        .find(|ch| !ch.is_whitespace());
    let before_ok = matches!(before, Some(':') | Some('"'));
    let after_ok = matches!(after, Some(',') | Some('}') | Some(']') | Some('"'));
    before_ok && after_ok
}

fn is_reasonable_json_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
}

/// Deduplicate flows with identical (source_step, target_step, target_param) —
/// the same pair across multiple sequences shouldn't multiply.
fn dedupe_by_signature(flows: Vec<DataFlow>) -> Vec<DataFlow> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for flow in flows {
        let sig = (
            flow.source_step.clone(),
            flow.target_step.clone(),
            flow.target_param.clone(),
        );
        if seen.insert(sig) {
            out.push(flow);
        }
    }
    out
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::sequence::{
        CapabilitySequence, ExecutionPath, SequenceStep,
    };

    fn step(idx: usize, params: &[(&str, &str)], body: Option<&str>) -> SequenceStep {
        SequenceStep {
            step_index: idx,
            capability_id: Some(format!("cap_{idx}")),
            origin: "https://example.com".to_string(),
            concrete_url: format!("https://example.com/api/{idx}"),
            method: "GET".to_string(),
            request_params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            request_body: None,
            response_status: Some(200),
            response_body: body.map(str::to_string),
            action_binding_id: None,
            browser_action_desc: None,
            browser_action: None,
            browser_arguments: None,
            executed_via: ExecutionPath::ApiReplay,
            timestamp_ms: 1_780_000_000_000 + (idx as i64) * 1000,
            duration_ms: 50,
        }
    }

    fn sequence(steps: Vec<SequenceStep>) -> CapabilitySequence {
        CapabilitySequence {
            id: "seq_test".to_string(),
            task_id: "task_x".to_string(),
            execution_id: "exec_x".to_string(),
            origin_key: "example.com".to_string(),
            steps,
            captured_at_ms: 1_780_000_000_000,
            finalized: true,
        }
    }

    #[test]
    fn auto_match_detects_session_token_flow() {
        let seq = sequence(vec![
            step(
                0,
                &[("user", "alice")],
                Some(r#"{"session_token":"sk_abcd1234efgh"}"#),
            ),
            step(1, &[("token", "sk_abcd1234efgh")], Some(r#"{"ok":true}"#)),
        ]);
        let flows = infer_auto_data_flows(&[seq]);
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].source_step, "step_0");
        assert_eq!(flows[0].target_step, "step_1");
        assert_eq!(flows[0].target_param, "token");
        assert_eq!(flows[0].inference_method, InferenceMethod::AutoMatch);
        assert!(flows[0].confidence > 0.85);
    }

    #[test]
    fn auto_match_uses_source_key_when_target_param_name_differs() {
        let seq = sequence(vec![
            step(0, &[], Some(r#"{"hits":[{"story_id":22238335}]}"#)),
            step(1, &[("item_id", "22238335")], Some(r#"{"ok":true}"#)),
        ]);
        let flows = infer_auto_data_flows(&[seq]);
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].source_path, "$..story_id");
        assert_eq!(flows[0].target_param, "item_id");
    }

    #[test]
    fn auto_match_uses_nearby_key_for_truncated_json() {
        let seq = sequence(vec![
            step(
                0,
                &[],
                Some(r#"{"hits":[{"story_id":22238335,"title":"x"}]}…"#),
            ),
            step(1, &[("item_id", "22238335")], Some(r#"{"ok":true}"#)),
        ]);
        let flows = infer_auto_data_flows(&[seq]);
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].source_path, "$..story_id");
    }

    #[test]
    fn auto_match_skips_short_values() {
        // value "42" is 2 chars — below MIN_MATCH_LEN.
        let seq = sequence(vec![
            step(0, &[], Some(r#"{"id":42}"#)),
            step(1, &[("id", "42")], None),
        ]);
        let flows = infer_auto_data_flows(&[seq]);
        assert!(flows.is_empty());
    }

    #[test]
    fn auto_match_skips_trivial_values() {
        let seq = sequence(vec![
            step(0, &[], Some(r#"{"ok":true}"#)),
            step(1, &[("active", "true")], None),
        ]);
        let flows = infer_auto_data_flows(&[seq]);
        assert!(flows.is_empty());
    }

    #[test]
    fn auto_match_dedupes_across_sequences() {
        let seq_a = sequence(vec![
            step(0, &[], Some(r#"{"token":"sk_abcd1234efgh"}"#)),
            step(1, &[("auth", "sk_abcd1234efgh")], None),
        ]);
        let seq_b = sequence(vec![
            step(0, &[], Some(r#"{"token":"sk_zzzz9999wwww"}"#)),
            step(1, &[("auth", "sk_zzzz9999wwww")], None),
        ]);
        let flows = infer_auto_data_flows(&[seq_a, seq_b]);
        // Both sequences contribute the SAME (source_step, target_step,
        // target_param) signature; should dedupe to one flow.
        assert_eq!(flows.len(), 1);
    }

    #[test]
    fn auto_match_does_not_match_backwards() {
        // step_j cannot use a value from step_k where k > j.
        let seq = sequence(vec![
            step(0, &[("token", "sk_abcd1234efgh")], None),
            step(1, &[], Some(r#"{"token":"sk_abcd1234efgh"}"#)),
        ]);
        let flows = infer_auto_data_flows(&[seq]);
        assert!(flows.is_empty());
    }
}
