//! Locate response bodies that carry reported answer values.

use super::values::{normalize_value, ReportedValues};
use crate::magician_v2::api_mining::recipe::Extractor;
use crate::magician_v2::api_mining::types::NetworkTraceEvent;
use std::collections::HashSet;
use std::sync::OnceLock;

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_NODES: usize = 65_536;
const LONG_VALUE_CONTAINS_LEN: usize = 12;
const MAX_PLAIN_TEXT_BODY_BYTES: usize = 4 * 1024;
const PLAIN_TEXT_BODY_PATTERN: &str = r"(?s)\A\s*(.{1,4096}?)\s*\z";

#[derive(Debug, Clone, PartialEq)]
pub struct AnswerHit {
    pub reported_index: usize,
    pub trace_index: usize,
    pub field: Option<String>,
    pub value: String,
    pub extractor: Extractor,
}

/// Task words that name a JSON object key carried by some captured 2xx
/// response. These are the fields the task asked to have reported.
/// The clause that states the ask. A task's title names what is wanted, and
/// the description adds the steps to get there — including credentials
/// ("log in with username eval"). Reading field names out of the whole
/// description lets an instruction word that happens to be a response key
/// (`username`, `password`, `id`) pose as the asked field and refuse a recipe
/// that answers correctly, so only the title and an explicit `report …`
/// clause count as the ask.
fn ask_text(task_title: &str, task_text: &str) -> String {
    const ASK_MARKERS: &[&str] = &[
        "report the ",
        "report its ",
        "report ",
        "what is ",
        "tell me ",
    ];
    let lowered = task_text.to_ascii_lowercase();
    let mut ask = task_title.to_owned();
    for marker in ASK_MARKERS {
        let mut from = 0;
        while let Some(offset) = lowered[from..].find(marker) {
            let start = from + offset + marker.len();
            from = start;
            let end = task_text[start..]
                .find(['.', ',', ';', '\n'])
                .map(|index| start + index)
                .unwrap_or(task_text.len());
            ask.push(' ');
            ask.push_str(&task_text[start..end]);
        }
    }
    ask
}

pub fn asked_response_keys(
    traces: &[NetworkTraceEvent],
    task_title: &str,
    task_text: &str,
) -> Vec<String> {
    let words = super::values::task_field_words(&ask_text(task_title, task_text));
    if words.is_empty() {
        return Vec::new();
    }
    let mut keys = std::collections::HashSet::new();
    for trace in traces {
        let Some(body) = trace.response_body.as_deref() else {
            continue;
        };
        if body.is_empty() || body.len() > MAX_BODY_BYTES || !(200..300).contains(&trace.status) {
            continue;
        }
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
            collect_scalar_keys(&json, 0, &mut keys);
        }
    }
    words
        .into_iter()
        .filter(|word| keys.contains(word))
        .collect()
}

/// True when a response carries `value` under one of the keys the task named.
///
/// The asked-field guard exists to catch an agent that answered a different
/// question — reporting the title when the task asked for the author — because
/// a recipe built from that replays wrong with full confidence. On its own it
/// cannot tell that apart from an agent that answered correctly and merely
/// called the field something else, and a synonym is by far the commoner case:
/// the same task, site and code refused to compile because one run wrote
/// "score" where the task said "points". The two are distinguishable by the
/// value rather than the label — a synonym reports exactly what the asked key
/// holds, a wrong answer does not.
pub fn asked_key_carries_value(
    traces: &[NetworkTraceEvent],
    asked: &[String],
    value: &str,
) -> bool {
    if asked.is_empty() || value.is_empty() {
        return false;
    }
    let wanted = normalize_value(value);
    if wanted.is_empty() {
        return false;
    }
    traces.iter().any(|trace| {
        let Some(body) = trace.response_body.as_deref() else {
            return false;
        };
        if body.is_empty() || body.len() > MAX_BODY_BYTES || !(200..300).contains(&trace.status) {
            return false;
        }
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .is_some_and(|json| scalar_under_key_equals(&json, asked, &wanted, 0))
    })
}

fn scalar_under_key_equals(
    value: &serde_json::Value,
    asked: &[String],
    wanted: &str,
    depth: usize,
) -> bool {
    if depth > MAX_JSON_DEPTH {
        return false;
    }
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, child)| {
            let carried = asked.contains(&key.to_ascii_lowercase())
                && match child {
                    serde_json::Value::String(text) => normalize_value(text) == wanted,
                    serde_json::Value::Number(number) => {
                        normalize_value(&number.to_string()) == wanted
                    },
                    _ => false,
                };
            carried || scalar_under_key_equals(child, asked, wanted, depth + 1)
        }),
        serde_json::Value::Array(items) => items
            .iter()
            .take(MAX_JSON_NODES)
            .any(|item| scalar_under_key_equals(item, asked, wanted, depth + 1)),
        _ => false,
    }
}

fn collect_scalar_keys(
    value: &serde_json::Value,
    depth: usize,
    keys: &mut std::collections::HashSet<String>,
) {
    if depth > MAX_JSON_DEPTH {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if !matches!(
                    child,
                    serde_json::Value::Object(_) | serde_json::Value::Array(_)
                ) {
                    keys.insert(key.to_ascii_lowercase());
                }
                collect_scalar_keys(child, depth + 1, keys);
            }
        },
        serde_json::Value::Array(items) => {
            for item in items.iter().take(MAX_JSON_NODES) {
                collect_scalar_keys(item, depth + 1, keys);
            }
        },
        _ => {},
    }
}

pub fn locate_answer_traces(
    traces: &[NetworkTraceEvent],
    values: &ReportedValues,
) -> Vec<AnswerHit> {
    if values.is_empty() {
        return Vec::new();
    }
    let requires_fields: Vec<_> = values
        .values
        .iter()
        .enumerate()
        .map(|(index, reported)| {
            !super::values::is_useful(&reported.normalized)
                || equal_value_has_distinct_fields(values, index)
        })
        .collect();
    let mut per_trace = Vec::new();
    for (index, trace) in traces.iter().enumerate() {
        let Some(body) = trace.response_body.as_deref() else {
            continue;
        };
        if body.is_empty() || body.len() > MAX_BODY_BYTES || !(200..300).contains(&trace.status) {
            continue;
        }
        let mut hits = Vec::new();
        let is_json = match serde_json::from_str::<serde_json::Value>(body) {
            Ok(json) => {
                let mut visited = 0;
                walk_json(
                    &json,
                    "$",
                    None,
                    values,
                    &requires_fields,
                    index,
                    0,
                    &mut visited,
                    &mut hits,
                );
                true
            },
            Err(_) => {
                locate_text_hits(body, values, index, &mut hits);
                false
            },
        };
        if !hits.is_empty() {
            per_trace.push((hits, is_json));
        }
    }
    greedy_cover(per_trace)
}

fn walk_json(
    value: &serde_json::Value,
    path: &str,
    field: Option<&str>,
    values: &ReportedValues,
    requires_fields: &[bool],
    trace_index: usize,
    depth: usize,
    visited: &mut usize,
    output: &mut Vec<AnswerHit>,
) {
    if depth > MAX_JSON_DEPTH || *visited >= MAX_JSON_NODES || output.len() >= values.values.len() {
        return;
    }
    *visited += 1;
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                // Syntax validation alone is insufficient: a literal key
                // `customer.name` would alias the distinct nested name field.
                let Some(child_path) = super::resolve::supported_child_json_path(path, key) else {
                    continue;
                };
                walk_json(
                    child,
                    &child_path,
                    Some(key),
                    values,
                    requires_fields,
                    trace_index,
                    depth + 1,
                    visited,
                    output,
                );
                if *visited >= MAX_JSON_NODES || output.len() >= values.values.len() {
                    break;
                }
            }
        },
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let Some(child_path) = super::resolve::supported_index_json_path(path, index)
                else {
                    continue;
                };
                walk_json(
                    child,
                    &child_path,
                    field,
                    values,
                    requires_fields,
                    trace_index,
                    depth + 1,
                    visited,
                    output,
                );
                if *visited >= MAX_JSON_NODES || output.len() >= values.values.len() {
                    break;
                }
            }
        },
        serde_json::Value::String(value) => locate_scalar(
            &normalize_value(value),
            path,
            field,
            values,
            requires_fields,
            trace_index,
            output,
        ),
        serde_json::Value::Number(value) => locate_scalar(
            &normalize_value(&value.to_string()),
            path,
            field,
            values,
            requires_fields,
            trace_index,
            output,
        ),
        serde_json::Value::Bool(value) => locate_scalar(
            if *value { "true" } else { "false" },
            path,
            field,
            values,
            requires_fields,
            trace_index,
            output,
        ),
        _ => {},
    }
}

fn locate_scalar(
    normalized: &str,
    path: &str,
    field: Option<&str>,
    values: &ReportedValues,
    requires_fields: &[bool],
    trace_index: usize,
    output: &mut Vec<AnswerHit>,
) {
    for (reported_index, reported) in values.values.iter().enumerate() {
        if requires_fields[reported_index]
            && !reported
                .field
                .as_deref()
                .zip(field)
                .is_some_and(|(reported, actual)| reported.eq_ignore_ascii_case(actual))
        {
            continue;
        }
        let matches = reported.normalized == normalized
            || (reported.normalized.len() >= LONG_VALUE_CONTAINS_LEN
                && normalized.contains(&reported.normalized));
        if matches
            && !output
                .iter()
                .any(|hit| hit.reported_index == reported_index)
        {
            output.push(AnswerHit {
                reported_index,
                trace_index,
                field: reported.field.clone().or_else(|| field.map(str::to_owned)),
                value: reported.normalized.clone(),
                extractor: Extractor::JsonPath {
                    path: path.to_owned(),
                },
            });
        }
    }
}

fn equal_value_has_distinct_fields(values: &ReportedValues, index: usize) -> bool {
    let reported = &values.values[index];
    values
        .values
        .iter()
        .enumerate()
        .any(|(other_index, other)| {
            other_index != index
                && other.normalized == reported.normalized
                && other.field != reported.field
        })
}

fn locate_text_hits(
    body: &str,
    values: &ReportedValues,
    trace_index: usize,
    output: &mut Vec<AnswerHit>,
) {
    let body = body.trim();
    if body.is_empty() || body.len() > MAX_PLAIN_TEXT_BODY_BYTES {
        return;
    }
    let normalized_body = normalize_value(body);
    for (reported_index, reported) in values.values.iter().enumerate() {
        if !super::values::is_useful(&reported.normalized)
            || equal_value_has_distinct_fields(values, reported_index)
        {
            continue;
        }
        if reported.normalized == normalized_body {
            output.push(AnswerHit {
                reported_index,
                trace_index,
                field: reported.field.clone(),
                value: reported.normalized.clone(),
                extractor: Extractor::Regex {
                    pattern: PLAIN_TEXT_BODY_PATTERN.into(),
                    group: 1,
                },
            });
            break;
        }
    }
    if output.is_empty() {
        locate_html_text_hits(body, values, trace_index, output);
    }
}

/// Locate a reported value inside a simple HTML text element without
/// persisting the value (or nearby private markup) in the extractor. The
/// durable regex is derived only from the tag name, and ambiguous repeated
/// tags are rejected so replay cannot silently extract a different element.
fn locate_html_text_hits(
    body: &str,
    values: &ReportedValues,
    trace_index: usize,
    output: &mut Vec<AnswerHit>,
) {
    static ELEMENT: OnceLock<regex::Regex> = OnceLock::new();
    let expression = ELEMENT.get_or_init(|| {
        regex::Regex::new(
            r"(?is)<([a-z][a-z0-9:-]*)\b[^>]*>\s*([^<]{1,4096}?)\s*</([a-z][a-z0-9:-]*)\s*>",
        )
        .expect("the bounded HTML element expression is valid")
    });
    for (reported_index, reported) in values.values.iter().enumerate() {
        if !super::values::is_useful(&reported.normalized)
            || equal_value_has_distinct_fields(values, reported_index)
        {
            continue;
        }
        let mut matching_tag: Option<&str> = None;
        let mut ambiguous = false;
        for captures in expression.captures_iter(body) {
            let Some(open_tag) = captures.get(1).map(|value| value.as_str()) else {
                continue;
            };
            let Some(text) = captures.get(2).map(|value| value.as_str()) else {
                continue;
            };
            let Some(close_tag) = captures.get(3).map(|value| value.as_str()) else {
                continue;
            };
            if !open_tag.eq_ignore_ascii_case(close_tag)
                || normalize_value(text) != reported.normalized
            {
                continue;
            }
            if matching_tag.is_some() {
                ambiguous = true;
                break;
            }
            matching_tag = Some(open_tag);
        }
        let Some(tag) = matching_tag.filter(|_| !ambiguous) else {
            continue;
        };
        let same_tag_elements = expression
            .captures_iter(body)
            .filter(|captures| {
                captures
                    .get(1)
                    .zip(captures.get(3))
                    .is_some_and(|(open, close)| {
                        open.as_str().eq_ignore_ascii_case(tag)
                            && close.as_str().eq_ignore_ascii_case(tag)
                    })
            })
            .take(2)
            .count();
        if same_tag_elements != 1 {
            continue;
        }
        let tag = regex::escape(tag);
        output.push(AnswerHit {
            reported_index,
            trace_index,
            field: reported.field.clone(),
            value: reported.normalized.clone(),
            extractor: Extractor::Regex {
                pattern: format!(r"(?is)<{tag}\b[^>]*>\s*([^<]{{1,4096}}?)\s*</{tag}\s*>"),
                group: 1,
            },
        });
    }
}

fn greedy_cover(mut candidates: Vec<(Vec<AnswerHit>, bool)>) -> Vec<AnswerHit> {
    let mut covered = HashSet::new();
    let mut chosen = Vec::new();
    loop {
        candidates.sort_by(|left, right| {
            let left_uncovered = left
                .0
                .iter()
                .filter(|hit| !covered.contains(&hit.reported_index))
                .count();
            let right_uncovered = right
                .0
                .iter()
                .filter(|hit| !covered.contains(&hit.reported_index))
                .count();
            right_uncovered
                .cmp(&left_uncovered)
                .then_with(|| right.1.cmp(&left.1))
        });
        let Some((hits, _)) = candidates.first() else {
            break;
        };
        let fresh: Vec<_> = hits
            .iter()
            .filter(|hit| !covered.contains(&hit.reported_index))
            .cloned()
            .collect();
        if fresh.is_empty() {
            break;
        }
        covered.extend(fresh.iter().map(|hit| hit.reported_index));
        chosen.extend(fresh);
        candidates.remove(0);
    }
    chosen
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::values::ReportedValue;
    use super::*;

    #[test]
    fn literal_json_keys_cannot_alias_nested_fields_or_array_indexes() {
        let values = super::super::values::collect_reported_values(
            "",
            &[serde_json::json!({"answer": "wanted result"})],
            &[],
        );
        for json in [
            serde_json::json!({"customer.name":"wanted result", "customer":{"name":"wrong result"}}),
            serde_json::json!({"items[0]":"wanted result", "items":["wrong result"]}),
            serde_json::json!({".name":"wanted result", "nested":{"name":"wrong result"}}),
            serde_json::json!({"outer.inner":{"answer":"wanted result"}, "outer":{"inner":{"answer":"wrong result"}}}),
        ] {
            let mut hits = Vec::new();
            let mut visited = 0;
            walk_json(
                &json,
                "$",
                None,
                &values,
                &[false],
                0,
                0,
                &mut visited,
                &mut hits,
            );
            assert!(
                hits.is_empty(),
                "literal keys must not learn a different value's path: {json}"
            );
        }

        // Supported nested fields and genuine indexes retain typed extraction.
        let json = serde_json::json!({"items":[{"answer":"wanted result"}]});
        let mut hits = Vec::new();
        let mut visited = 0;
        walk_json(
            &json,
            "$",
            None,
            &values,
            &[false],
            0,
            0,
            &mut visited,
            &mut hits,
        );
        assert_eq!(hits.len(), 1);
        let Extractor::JsonPath { path } = &hits[0].extractor else {
            panic!("JSON answer must have a JSONPath extractor");
        };
        assert_eq!(path, "$.items[0].answer");
        assert_eq!(
            crate::magician_v2::api_mining::workflow_replay::jsonpath::extract_jsonpath(
                &json, path
            )
            .as_deref(),
            Some("wanted result")
        );
    }

    #[test]
    fn small_named_scalars_require_the_response_field_not_an_incidental_equal_value() {
        let values = super::super::values::collect_reported_values(
            "",
            &[serde_json::json!({"count": 0, "enabled": false})],
            &[],
        );
        let mut hits = Vec::new();
        let mut visited = 0;
        walk_json(
            &serde_json::json!({"unrelated": 0, "telemetry": false}),
            "$",
            None,
            &values,
            &[true, true],
            0,
            0,
            &mut visited,
            &mut hits,
        );
        assert!(hits.is_empty());
        visited = 0;
        walk_json(
            &serde_json::json!({"count": 0, "enabled": false}),
            "$",
            None,
            &values,
            &[true, true],
            0,
            0,
            &mut visited,
            &mut hits,
        );
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn plain_text_extractor_contains_no_captured_response_literal() {
        let values = ReportedValues {
            values: vec![ReportedValue {
                field: Some("answer".into()),
                raw: "Private Result".into(),
                normalized: "private result".into(),
            }],
        };
        let mut hits = Vec::new();

        locate_text_hits("  Private Result\n", &values, 3, &mut hits);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].trace_index, 3);
        let Extractor::Regex { pattern, group } = &hits[0].extractor else {
            panic!("plain text must use the fixed regex extractor");
        };
        assert_eq!(pattern, PLAIN_TEXT_BODY_PATTERN);
        assert_eq!(*group, 1);
        assert!(!pattern.to_ascii_lowercase().contains("private result"));
    }

    #[test]
    fn embedded_html_answer_uses_a_value_free_tag_extractor() {
        let values = ReportedValues {
            values: vec![ReportedValue {
                field: Some("answer".into()),
                raw: "Private Result".into(),
                normalized: "private result".into(),
            }],
        };
        let mut hits = Vec::new();

        locate_text_hits(
            r#"<div data-token="secret-prefix">Private Result</div>"#,
            &values,
            0,
            &mut hits,
        );

        assert_eq!(hits.len(), 1);
        let Extractor::Regex { pattern, group } = &hits[0].extractor else {
            panic!("HTML text must use a bounded generic tag extractor");
        };
        assert_eq!(*group, 1);
        assert!(!pattern.contains("Private Result"));
        assert!(!pattern.contains("secret-prefix"));
    }
}
