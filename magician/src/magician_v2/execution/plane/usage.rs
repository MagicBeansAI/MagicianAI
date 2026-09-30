//! Complete usage snapshots from the installed harness CLI protocols.
use crate::magician_v2::execution::plane::engine::HarnessUsage;
use serde_json::Value;

/// Terminal CLI usage is a snapshot, not a text delta. Ignore per-message
/// updates so an early input-only notification cannot look like complete
/// metering, and replace repeated terminal snapshots rather than double charge.
pub(crate) fn usage_from_event(value: &Value) -> Option<HarnessUsage> {
    if !matches!(
        value
            .get("type")
            .or_else(|| value.get("event"))
            .and_then(Value::as_str),
        Some("result" | "turn.completed")
    ) {
        return None;
    }
    let usage = value
        .get("usage")
        .or_else(|| value.get("token_count"))
        .or_else(|| value.pointer("/result/usage"))?;
    let mut parsed = usage_snapshot(usage)?;
    parsed.cost_usd = [
        value.get("total_cost_usd"),
        value.get("cost_usd"),
        value.pointer("/result/total_cost_usd"),
        value.pointer("/result/cost_usd"),
        usage.get("total_cost_usd"),
        usage.get("cost_usd"),
        usage.pointer("/cost/total"),
    ]
    .into_iter()
    .flatten()
    .find_map(valid_cost);
    parsed.model = value
        .get("model")
        .or_else(|| value.pointer("/result/model"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("modelUsage")
                .and_then(Value::as_object)
                .filter(|models| models.len() == 1)
                .and_then(|models| models.keys().next().cloned())
        });
    Some(parsed)
}

/// Agy reports complete per-response usage at DONE before it continues its
/// native loop. Only the proposal adapter stops at that boundary.
pub(crate) fn usage_from_agy_completed_response(value: &Value) -> Option<HarnessUsage> {
    let step = value.get("step_update")?;
    if value["event"] != "step_update"
        || step["step_type"] != "agent_response"
        || step["state"] != "DONE"
    {
        return None;
    }
    usage_snapshot(step.get("usage")?)
}

fn valid_cost(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
}

fn usage_snapshot(usage: &Value) -> Option<HarnessUsage> {
    let input = usage.get("input_tokens")?.as_u64()?;
    let output = usage.get("output_tokens")?.as_u64()?;
    // Anthropic-shaped CLIs (Claude Code) report cache reads beside
    // `input_tokens`; Codex exec reports `cached_input_tokens` inside it.
    let cached = usage
        .get("cache_read_input_tokens")
        .or_else(|| usage.get("cached_input_tokens"))
        .or_else(|| usage.get("cache_read_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(HarnessUsage {
        cached_input_tokens: cached,
        cache_read_reported: [
            "cache_read_input_tokens",
            "cached_input_tokens",
            "cache_read_tokens",
        ]
        .iter()
        .any(|key| usage.get(key).and_then(Value::as_u64).is_some()),
        cache_creation_tokens: usage
            .get("cache_creation_input_tokens")
            .or_else(|| usage.get("cache_creation_tokens"))
            .or_else(|| usage.get("cache_write_input_tokens"))
            .and_then(Value::as_u64),
        cost_usd: usage.get("cost_usd").and_then(valid_cost),
        model: None,
        input_tokens: input
            .saturating_add(
                usage
                    .get("cache_read_input_tokens")
                    .or_else(|| usage.get("cache_read_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
            .saturating_add(
                usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ),
        // Codex reasoning and agy thinking counts are subsets of output.
        output_tokens: output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_usage_preserves_reported_cost_and_unknown_cache_buckets() {
        let claude = usage_from_event(&serde_json::json!({
            "type":"result", "total_cost_usd":0.125,
            "modelUsage":{"claude-fixture":{}},
            "usage":{"input_tokens":20,"cache_read_input_tokens":70,"cache_creation_input_tokens":10,"output_tokens":4}
        })).unwrap();
        assert_eq!(claude.input_tokens, 100);
        assert_eq!(claude.cache_creation_tokens, Some(10));
        assert_eq!(claude.cost_usd, Some(0.125));
        assert_eq!(claude.model.as_deref(), Some("claude-fixture"));
        assert!(claude.availability().cache_read);
        let unknown = usage_from_event(&serde_json::json!({
            "type":"turn.completed","usage":{"input_tokens":100,"output_tokens":4}
        }))
        .unwrap();
        assert!(!unknown.availability().cache_read);
        assert_eq!(unknown.cost_usd, None);
        let codex = usage_from_event(&serde_json::json!({
            "type":"turn.completed", "usage":{"input_tokens":100,"cached_input_tokens":80,"cache_write_input_tokens":5,"output_tokens":4}
        })).unwrap();
        assert_eq!(codex.input_tokens, 100);
        assert_eq!(codex.cache_creation_tokens, Some(5));
        assert!(codex.availability().cache_write);
        let mut total = claude;
        total.accumulate(unknown);
        assert_eq!(total.input_tokens, 200);
        assert_eq!(total.cached_input_tokens, 70);
        assert!(!total.availability().cache_read);
        assert_eq!(total.cache_creation_tokens, None);
        assert_eq!(total.cost_usd, None);
        for (cost, expected) in [
            (serde_json::json!(0), Some(0.0)),
            (serde_json::json!(-1), None),
            (serde_json::json!("NaN"), None),
        ] {
            let u = usage_from_event(&serde_json::json!({"type":"result","total_cost_usd":cost,"usage":{"input_tokens":1,"output_tokens":1}})).unwrap();
            assert_eq!(u.cost_usd, expected);
        }
    }

    #[test]
    fn terminal_usage_is_complete_and_cache_tokens_are_not_lost_or_double_counted() {
        for value in [
            serde_json::json!({"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":4}}),
            serde_json::json!({"type":"result","usage":{"input_tokens":20,"cache_read_input_tokens":70,"cache_creation_input_tokens":10,"output_tokens":4}}),
            serde_json::json!({"event":"result","result":{"usage":{"input_tokens":100,"output_tokens":4,"thinking_tokens":3,"total_tokens":104}}}),
            serde_json::json!({"event":"result","result":{"usage":{"input_tokens":20,"cache_read_tokens":80,"output_tokens":4,"thinking_tokens":3}}}),
        ] {
            let usage = usage_from_event(&value).unwrap();
            assert_eq!((usage.input_tokens, usage.output_tokens), (100, 4));
            assert!(usage.cached_input_tokens <= usage.input_tokens);
        }
        for value in [
            serde_json::json!({"type":"message_start","usage":{"input_tokens":100,"output_tokens":0}}),
            serde_json::json!({"type":"result","usage":{"input_tokens":100}}),
            serde_json::json!({"type":"result","usage":{"input_tokens":-1,"output_tokens":4}}),
        ] {
            assert!(usage_from_event(&value).is_none());
        }
    }
}
