//! Goal-level churn: redirect the model instead of stopping it.
//!
//! The stuck guards in `decision` catch identical repeats and long read-only
//! streaks, and they conclude the run. They miss the commonest real failure: the
//! same goal attempted again and again with *different* arguments — new SQL,
//! another field path, a different tool on the same server — each attempt
//! failing or re-reading the same thing. Tool-reported errors that arrive
//! inside a successful transport call (an MCP result with `is_error` /
//! `remote_error`) are invisible to them too.
//!
//! This module reads the recent history and returns steer lines for the
//! current turn. It never ends the run; it tells the model what it is doing and
//! names the generic ways out (another tool of the same pack, running the saved
//! artifact through its own system, moving data by reference and finishing the
//! work locally).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::types::{ExecutionHistory, IterationRecord};
use crate::magician_v2::execution::actions::ExecutableAction;

const WINDOW: usize = 15;
const FAILURE_THRESHOLD: usize = 3;
const REREAD_THRESHOLD: usize = 3;
const ERROR_SNIPPET_CHARS: usize = 200;
const ERROR_SCAN_DEPTH: usize = 6;
/// Arguments that name *what* is read; two calls sharing one of these read
/// the same resource even when their other arguments differ.
const RESOURCE_KEYS: &[&str] = &[
    "result_ref",
    "uris",
    "uri",
    "url",
    "file_path",
    "path",
    "id",
    "question_id",
    "card_id",
    "table_id",
    "spreadsheet_id",
    "spreadsheetId",
    "document_id",
];

/// Steer lines for this turn; empty when nothing is churning.
pub(crate) fn goal_churn_steer_lines(history: &ExecutionHistory) -> Vec<String> {
    let start = history.iterations.len().saturating_sub(WINDOW);
    let recent = &history.iterations[start..];
    let Some(latest) = recent.last() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let Some(line) = failure_churn_line(recent, latest) {
        lines.push(line);
    }
    if let Some(line) = reread_line(recent, latest) {
        lines.push(line);
    }
    lines
}

fn failure_churn_line(recent: &[IterationRecord], latest: &IterationRecord) -> Option<String> {
    let latest_tool = tool_identity(&latest.action)?;
    let latest_error = record_failure(latest)?;
    let mut failures = 0usize;
    let mut argument_shapes = HashSet::new();
    for record in recent {
        if tool_identity(&record.action).as_deref() != Some(latest_tool.as_str()) {
            continue;
        }
        if record_failure(record).is_some() {
            failures += 1;
            argument_shapes.insert(arguments_fingerprint(&record.action));
        }
    }
    // Identical repeats are the existing guard's case; this one is about
    // varying the arguments and failing anyway.
    if failures < FAILURE_THRESHOLD || argument_shapes.len() < 2 {
        return None;
    }
    Some(format!(
        "[goal-churn] `{latest_tool}` has failed {failures} times in your last {} steps with \
         different arguments (latest error: {latest_error}). Varying the arguments again is \
         unlikely to work. Before the next call, list the approaches you have NOT tried — another \
         tool of the same pack or server (check its tool list or guide), running an existing saved \
         query/report/script through its own system, or exporting the data with export_result and \
         finishing the work locally with shell/duckdb — and switch to the most different one.",
        recent.len()
    ))
}

fn reread_line(recent: &[IterationRecord], latest: &IterationRecord) -> Option<String> {
    let latest_keys = resource_keys(&latest.action);
    if latest_keys.is_empty() {
        return None;
    }
    let all_keys: Vec<Vec<String>> = recent.iter().map(|r| resource_keys(&r.action)).collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for keys in &all_keys {
        for key in keys {
            *counts.entry(key.as_str()).or_default() += 1;
        }
    }
    let (key, count) = latest_keys
        .iter()
        .filter_map(|key| counts.get(key.as_str()).map(|count| (key, *count)))
        .max_by_key(|(_, count)| *count)?;
    if count < REREAD_THRESHOLD {
        return None;
    }
    Some(format!(
        "[goal-churn] You have now read {key} {count} times in your last {} steps; it will not \
         change between reads. If the value is too large to use from context (you need to copy it \
         into another call or process it), stop paging and re-typing it: call export_result to \
         save it to a file and work on the file with shell/duckdb, or pass \
         {{\"$ref\": \"<result_ref>#/json/pointer\"}} (or {{\"$file\": \"<path>\"}}) as the \
         argument of the tool that needs it.",
        recent.len()
    ))
}

/// `capability` or, for a gateway call such as MCP `call_tool`, the inner tool.
fn tool_identity(action: &ExecutableAction) -> Option<String> {
    let ExecutableAction::Pack {
        capability_name,
        resolved_params,
        ..
    } = action
    else {
        return None;
    };
    let inner = resolved_params
        .get("tool_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty());
    Some(match inner {
        Some(inner) => format!("{capability_name} → {inner}"),
        None => capability_name.clone(),
    })
}

/// Arguments as one value, with a JSON-string argument (MCP `arguments_json`)
/// parsed so its contents count.
fn effective_arguments(action: &ExecutableAction) -> Option<serde_json::Map<String, Value>> {
    let ExecutableAction::Pack {
        resolved_params, ..
    } = action
    else {
        return None;
    };
    let mut merged = serde_json::Map::new();
    for (key, value) in resolved_params {
        if key.starts_with("__") {
            continue;
        }
        if let Some(Value::Object(inner)) = value
            .as_str()
            .filter(|text| text.trim_start().starts_with('{'))
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
        {
            merged.extend(inner);
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    Some(merged)
}

fn arguments_fingerprint(action: &ExecutableAction) -> String {
    effective_arguments(action)
        .map(|args| Value::Object(args).to_string())
        .unwrap_or_default()
}

fn resource_keys(action: &ExecutableAction) -> Vec<String> {
    let (Some(tool), Some(args)) = (tool_identity(action), effective_arguments(action)) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    for name in RESOURCE_KEYS {
        let Some(value) = args.get(*name) else {
            continue;
        };
        let rendered = match value {
            Value::String(text) if !text.trim().is_empty() => text.trim().to_string(),
            Value::Number(number) => number.to_string(),
            Value::Array(items) if !items.is_empty() => Value::Array(items.clone()).to_string(),
            _ => continue,
        };
        // A result_ref names data, whichever tool reads it.
        keys.push(if *name == "result_ref" {
            format!("`{rendered}`")
        } else {
            format!("`{name}={rendered}` via `{tool}`")
        });
    }
    keys
}

/// The failure message when the step failed — at the transport, or inside a
/// result that reports an error in its own payload.
fn record_failure(record: &IterationRecord) -> Option<String> {
    if !record.result.success || record.result.error.is_some() {
        return Some(snippet(
            record.result.error.as_deref().unwrap_or("the call failed"),
        ));
    }
    let output = record.result.output.as_deref()?;
    let value: Value = serde_json::from_str(output).ok()?;
    payload_error(&value, 0).map(|message| snippet(&message))
}

fn payload_error(value: &Value, depth: usize) -> Option<String> {
    if depth > ERROR_SCAN_DEPTH {
        return None;
    }
    match value {
        Value::Object(map) => {
            let flagged = map.get("is_error").and_then(Value::as_bool) == Some(true)
                || map.get("remote_error").and_then(Value::as_bool) == Some(true)
                || matches!(
                    map.get("status").and_then(Value::as_str),
                    Some("error" | "failed")
                );
            if flagged {
                return Some(error_message(map).unwrap_or_else(|| "tool reported an error".into()));
            }
            map.values()
                .find_map(|value| payload_error(value, depth + 1))
        },
        Value::Array(items) => items
            .iter()
            .take(8)
            .find_map(|value| payload_error(value, depth + 1)),
        _ => None,
    }
}

fn error_message(map: &serde_json::Map<String, Value>) -> Option<String> {
    for key in ["error", "reason", "message", "error_code"] {
        if let Some(text) = map.get(key).and_then(Value::as_str) {
            return Some(text.to_string());
        }
    }
    // MCP: {"content": [{"type": "text", "text": "…"}], "is_error": true}
    map.get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find_map(|item| item.get("text")?.as_str()))
        .map(str::to_string)
        .or_else(|| {
            map.values()
                .filter_map(Value::as_object)
                .find_map(error_message)
        })
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > ERROR_SNIPPET_CHARS {
        format!(
            "{}…",
            flat.chars().take(ERROR_SNIPPET_CHARS).collect::<String>()
        )
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as Map;

    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::execution::{
        agentic::types::{ActionResultRecord, EnvironmentState},
        capability::ImplementationType,
    };

    fn pack(capability: &str, params: Value) -> ExecutableAction {
        ExecutableAction::Pack {
            capability_name: capability.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: capability.to_string(),
            },
            resolved_params: params
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Map<_, _>>(),
        }
    }

    fn record(iteration: usize, action: ExecutableAction, output: Value) -> IterationRecord {
        IterationRecord {
            iteration,
            state_before: EnvironmentState::Uninitialized,
            action,
            result: ActionResultRecord {
                success: true,
                output: Some(output.to_string()),
                error: None,
                duration_ms: 1,
                outcome_category: None,
                api_replay_used: None,
                api_replay_time_ms: None,
                browser_fallback_reason: None,
                tool_result_projection: None,
            },
            state_after: EnvironmentState::Uninitialized,
            timestamp: Utc::now(),
            verification: None,
            llm_reasoning: None,
        }
    }

    fn history(records: Vec<IterationRecord>) -> ExecutionHistory {
        let mut history = ExecutionHistory::new();
        history.iterations = records;
        history
    }

    fn sql_call(iteration: usize, sql: &str) -> IterationRecord {
        record(
            iteration,
            pack(
                "metabase-mcp__call_tool",
                json!({
                    "tool_name": "execute_sql",
                    "arguments_json": json!({"database_id": 1, "sql": sql}).to_string()
                }),
            ),
            json!({"result": {"remote_error": true, "content": [{"type": "text", "text": "Syntax error at #"}]}}),
        )
    }

    #[test]
    fn varied_arguments_failing_inside_successful_transport_trigger_a_redirect() {
        let lines = goal_churn_steer_lines(&history(vec![
            sql_call(1, "select 1 #a"),
            sql_call(2, "select 1 #b"),
            sql_call(3, "select 1 #c"),
        ]));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("metabase-mcp__call_tool → execute_sql"));
        assert!(lines[0].contains("Syntax error at #"));
    }

    #[test]
    fn identical_repeats_are_left_to_the_existing_guard() {
        let lines = goal_churn_steer_lines(&history(vec![
            sql_call(1, "same"),
            sql_call(2, "same"),
            sql_call(3, "same"),
        ]));
        assert!(lines.iter().all(|line| !line.contains("has failed")));
    }

    #[test]
    fn rereading_one_result_with_different_pointers_suggests_by_reference_use() {
        let read = |iteration, path: &str| {
            record(
                iteration,
                pack(
                    "read_result",
                    json!({"result_ref": "result_ref_v1_abc", "field_paths": [path]}),
                ),
                json!({"page": {}}),
            )
        };
        let lines =
            goal_churn_steer_lines(&history(vec![read(1, "/a"), read(2, "/b"), read(3, "/c")]));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("export_result"));
        assert!(lines[0].contains("result_ref_v1_abc"));
    }

    #[test]
    fn a_healthy_run_gets_no_steer() {
        let ok = |iteration, id: u64| {
            record(
                iteration,
                pack(
                    "metabase-mcp__call_tool",
                    json!({"tool_name": "read_resource", "arguments_json": json!({"uris": [format!("q/{id}")]}).to_string()}),
                ),
                json!({"result": {"remote_error": false}}),
            )
        };
        assert!(goal_churn_steer_lines(&history(vec![ok(1, 1), ok(2, 2), ok(3, 3)])).is_empty());
    }
}
