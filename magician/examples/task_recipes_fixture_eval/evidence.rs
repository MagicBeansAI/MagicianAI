//! Evidence readers: the per-execution `events.jsonl` on disk and a window of
//! the running Magician's log. Both feed a failing gate's diagnostics.

use std::{
    fs,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::Value;

const MAX_LOG_LINES: usize = 400;
const MAX_LOG_LINE_BYTES: usize = 2048;

pub fn execution_events_path(
    runtime_root: &Path,
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
) -> PathBuf {
    runtime_root
        .join("scopes")
        .join(principal)
        .join(workspace)
        .join("tasks")
        .join(task_id)
        .join("executions")
        .join(execution_id)
        .join("events.jsonl")
}

/// Every event row for the execution; a missing file is an empty timeline.
pub fn execution_events(
    runtime_root: &Path,
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
) -> Result<Vec<Value>> {
    let path = execution_events_path(runtime_root, principal, workspace, task_id, execution_id);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect())
}

pub fn event_type(event: &Value) -> &str {
    event
        .get("event_type")
        .or_else(|| event.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Recipe events carry the family in `event_type` (`recipe.replay`) and the
/// specific lifecycle kind in `payload.kind` (`recipe.replay.started`).
pub fn event_kind(event: &Value) -> &str {
    event
        .get("payload")
        .and_then(|payload| payload.get("kind"))
        .or_else(|| event.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| event_type(event))
}

fn matches_event(event: &Value, wanted: &str) -> bool {
    event_type(event) == wanted || event_kind(event) == wanted
}

/// Events from the recipe rail and the mining pipeline.
pub fn recipe_events(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| {
            let kind = event_type(event);
            kind.starts_with("recipe.") || kind.starts_with("api_mining.")
        })
        .cloned()
        .collect()
}

/// The canonical outcome: the last `execution.outcome_observed` row whose
/// `execution_status` is terminal. The v3 task API exposes no outcome type or
/// summary; this event is where they live.
pub fn terminal_outcome(events: &[Value]) -> Option<(String, String, String)> {
    events
        .iter()
        .rev()
        .filter(|event| event_type(event) == "execution.outcome_observed")
        .map(|event| event.get("payload").unwrap_or(event))
        .find(|payload| {
            matches!(
                payload.get("execution_status").and_then(Value::as_str),
                Some("completed" | "failed" | "cancelled" | "canceled")
            )
        })
        .map(|payload| {
            (
                payload
                    .get("execution_status")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                payload
                    .get("outcome_type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                payload
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
}

pub fn has_event(events: &[Value], wanted: &str) -> bool {
    events.iter().any(|event| matches_event(event, wanted))
}

pub fn count_event(events: &[Value], wanted: &str) -> usize {
    events
        .iter()
        .filter(|event| matches_event(event, wanted))
        .count()
}

/// Lines appended to a log file after `open`.
pub struct LogWindow {
    path: PathBuf,
    start_offset: u64,
}

impl LogWindow {
    pub fn open(path: &Path) -> Self {
        let start_offset = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        Self {
            path: path.to_path_buf(),
            start_offset,
        }
    }

    pub fn lines_mentioning(&self, needles: &[&str]) -> Result<Vec<String>> {
        let Ok(file) = fs::File::open(&self.path) else {
            return Ok(Vec::new());
        };
        let mut reader = BufReader::new(file);
        // A rotated/truncated log restarts the window from the top.
        let len = reader
            .get_ref()
            .metadata()
            .map(|meta| meta.len())
            .unwrap_or(0);
        if len >= self.start_offset {
            reader.seek(SeekFrom::Start(self.start_offset))?;
        }
        let mut out = Vec::new();
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if needles
                .iter()
                .any(|needle| !needle.is_empty() && line.contains(needle))
            {
                let mut kept = line;
                if kept.len() > MAX_LOG_LINE_BYTES {
                    let mut cut = MAX_LOG_LINE_BYTES;
                    while !kept.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    kept.truncate(cut);
                    kept.push('…');
                }
                out.push(kept);
                if out.len() >= MAX_LOG_LINES {
                    break;
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn events_reader_filters_recipe_family() {
        let temp = tempfile::tempdir().unwrap();
        let path = execution_events_path(temp.path(), "p", "w", "task_1", "exec_1");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            concat!(
                r#"{"event_type":"execution.started"}"#,
                "\n",
                r#"{"event_type":"recipe.replay.started","execution_id":"exec_1"}"#,
                "\n",
                "not json\n",
                r#"{"type":"api_mining.compile.completed"}"#,
                "\n",
                r#"{"event_type":"recipe.replay.completed"}"#,
                "\n",
            ),
        )
        .unwrap();
        let events = execution_events(temp.path(), "p", "w", "task_1", "exec_1").unwrap();
        assert_eq!(events.len(), 4);
        let recipe = recipe_events(&events);
        assert_eq!(recipe.len(), 3);
        assert!(has_event(&events, "recipe.replay.completed"));
        assert!(!has_event(&events, "recipe.replay.auth.healed"));
        assert_eq!(count_event(&events, "recipe.replay.started"), 1);
        assert!(execution_events(temp.path(), "p", "w", "task_2", "exec_9")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn lifecycle_kinds_live_in_the_payload() {
        let events = vec![
            serde_json::json!({"event_type":"recipe.replay","payload":{"kind":"recipe.replay.started","recipe_id":"r1"}}),
            serde_json::json!({"event_type":"recipe.replay","payload":{"kind":"recipe.replay.step.failed","class":"auth"}}),
        ];
        assert!(has_event(&events, "recipe.replay.started"));
        assert!(has_event(&events, "recipe.replay"));
        assert!(!has_event(&events, "recipe.replay.completed"));
        assert_eq!(count_event(&events, "recipe.replay.step.failed"), 1);
    }

    #[test]
    fn terminal_outcome_is_the_last_terminal_observation() {
        let events = vec![
            serde_json::json!({"event_type":"execution.outcome_observed","payload":{"execution_status":"running","outcome_type":"running","summary":"Execution is running."}}),
            serde_json::json!({"event_type":"execution.outcome_observed","payload":{"execution_status":"completed","outcome_type":"goal_achieved_partial","summary":"3119"}}),
            serde_json::json!({"event_type":"execution.finalizer.completed","payload":{}}),
        ];
        assert_eq!(
            terminal_outcome(&events),
            Some((
                "completed".to_owned(),
                "goal_achieved_partial".to_owned(),
                "3119".to_owned()
            ))
        );
        assert_eq!(terminal_outcome(&events[..1]), None);
    }

    #[test]
    fn log_window_only_returns_lines_after_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("magician.log");
        fs::write(&path, "before task_1\n").unwrap();
        let window = LogWindow::open(&path);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "after task_1 [API_MINING] compile").unwrap();
        writeln!(file, "unrelated line").unwrap();
        let lines = window
            .lines_mentioning(&["task_1", "[API_MINING]"])
            .unwrap();
        assert_eq!(lines, vec!["after task_1 [API_MINING] compile".to_owned()]);
        assert!(LogWindow::open(temp.path().join("missing.log").as_path())
            .lines_mentioning(&["x"])
            .unwrap()
            .is_empty());
    }
}
