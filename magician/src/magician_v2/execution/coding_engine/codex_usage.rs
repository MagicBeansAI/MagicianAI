//! Usage carried by Codex app-server's thread/tokenUsage/updated protocol.
use crate::magician_v2::execution::coding_engine::CodingUsage;
use serde_json::Value;

pub(crate) fn parse_usage(value: &Value) -> Option<CodingUsage> {
    let count = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key).and_then(Value::as_u64))
    };
    let input = count(&["input", "inputTokens", "input_tokens"])?;
    let output = count(&["output", "outputTokens", "output_tokens"])?;
    Some(CodingUsage {
        input,
        output,
        cache_read: count(&["cacheRead", "cachedInputTokens", "cache_read"]).unwrap_or(0),
        cache_write: count(&["cacheWrite", "cacheWriteInputTokens", "cache_write"]).unwrap_or(0),
        total_tokens: count(&["totalTokens", "total_tokens"])
            .unwrap_or_else(|| input.saturating_add(output)),
        cost_total: value
            .get("cost")
            .and_then(|cost| {
                cost.as_f64()
                    .or_else(|| cost.get("total").and_then(Value::as_f64))
            })
            .or_else(|| value.get("costUsd").and_then(Value::as_f64)),
    })
}

fn difference(total: &CodingUsage, before: &CodingUsage) -> CodingUsage {
    CodingUsage {
        input: total.input.saturating_sub(before.input),
        output: total.output.saturating_sub(before.output),
        cache_read: total.cache_read.saturating_sub(before.cache_read),
        cache_write: total.cache_write.saturating_sub(before.cache_write),
        total_tokens: total.total_tokens.saturating_sub(before.total_tokens),
        cost_total: None,
    }
}

#[derive(Default)]
pub(crate) struct CodexUsageMeter {
    before: Option<CodingUsage>,
}

impl CodexUsageMeter {
    pub(crate) fn update(&mut self, params: &Value) -> Option<CodingUsage> {
        let Some(envelope) = params.get("tokenUsage") else {
            return parse_usage(params);
        };
        let total = parse_usage(envelope.get("total")?)?;
        let last = parse_usage(envelope.get("last")?)?;
        // `last` is the latest model request; `total` spans the native
        // conversation. Establish the pre-turn total once, then accumulate
        // all requests without recharging earlier turns on warm resume.
        let before = self.before.get_or_insert_with(|| difference(&total, &last));
        Some(difference(&total, before))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update(total: (u64, u64), last: (u64, u64)) -> Value {
        serde_json::json!({"threadId":"thread", "turnId":"turn", "tokenUsage":{
            "total":{"inputTokens":total.0,"outputTokens":total.1,"cachedInputTokens":0,"reasoningOutputTokens":1},
            "last":{"inputTokens":last.0,"outputTokens":last.1,"cachedInputTokens":0,"reasoningOutputTokens":1}
        }})
    }
    #[test]
    fn native_usage_counts_every_request_once_and_excludes_previous_turns() {
        let mut meter = CodexUsageMeter::default();
        let first = meter.update(&update((110, 12), (10, 2))).unwrap();
        assert_eq!((first.input, first.output), (10, 2));
        let second = meter.update(&update((140, 15), (30, 3))).unwrap();
        assert_eq!((second.input, second.output), (40, 5));
        assert_eq!(meter.update(&update((140, 15), (30, 3))).unwrap(), second);
    }
    #[test]
    fn unrelated_and_partial_objects_are_not_zero_usage() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"inputTokens":10}),
            serde_json::json!({"inputTokens":-1,"outputTokens":2}),
        ] {
            assert!(parse_usage(&value).is_none());
        }
        let usage =
            parse_usage(&serde_json::json!({"input":10,"output":2,"cachedInputTokens":8})).unwrap();
        assert_eq!((usage.input, usage.output, usage.cache_read), (10, 2, 8));
    }
}
