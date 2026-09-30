use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use tokio::sync::{broadcast, RwLock};
use tracing::warn;

use magician::magician_v2::{
    execution_panel::{ExecutionPanelShellEntry, ExecutionPanelShellLine},
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

const MAX_EXECUTIONS: usize = 64;
const MAX_STEPS_PER_EXECUTION: usize = 24;
const MAX_LINES_PER_STEP: usize = 200;

#[derive(Debug, Clone, Default)]
pub struct ExecutionPanelRuntimeStore {
    inner: Arc<RwLock<RuntimeState>>,
}

#[derive(Debug, Default)]
struct RuntimeState {
    execution_order: VecDeque<String>,
    shell_by_execution: HashMap<String, Vec<ExecutionPanelShellEntry>>,
}

impl ExecutionPanelRuntimeStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(self, event_broadcaster: Arc<RuntimeTransportBroadcaster>) {
        let mut event_rx = event_broadcaster.subscribe();
        tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(RuntimeTransportEvent::ShellOutputChunk {
                        execution_id,
                        step_id,
                        step_index,
                        command,
                        stream,
                        data,
                        is_final,
                        exit_code,
                        timestamp,
                        ..
                    }) => {
                        self.apply_shell_chunk(
                            &execution_id,
                            &step_id,
                            step_index as u32,
                            &command,
                            &stream,
                            &data,
                            is_final,
                            exit_code,
                            timestamp,
                        )
                        .await;
                    },
                    Ok(_) => {},
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(
                            skipped,
                            "execution panel runtime store lagged on realtime stream"
                        );
                    },
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    pub async fn shell_entries_for_execution(
        &self,
        execution_id: &str,
    ) -> Vec<ExecutionPanelShellEntry> {
        let state = self.inner.read().await;
        state
            .shell_by_execution
            .get(execution_id)
            .cloned()
            .unwrap_or_default()
    }

    async fn apply_shell_chunk(
        &self,
        execution_id: &str,
        step_id: &str,
        step_index: u32,
        command: &str,
        stream: &str,
        data: &str,
        is_final: bool,
        exit_code: Option<i32>,
        timestamp: i64,
    ) {
        let mut state = self.inner.write().await;
        touch_execution(&mut state.execution_order, execution_id);
        evict_old_executions(&mut state);

        let entries = state
            .shell_by_execution
            .entry(execution_id.to_string())
            .or_default();

        let position = entries.iter().position(|entry| entry.step_id == step_id);
        let entry = if let Some(position) = position {
            &mut entries[position]
        } else {
            if entries.len() >= MAX_STEPS_PER_EXECUTION {
                entries.remove(0);
            }
            entries.push(ExecutionPanelShellEntry {
                step_id: step_id.to_string(),
                step_index,
                command: command.to_string(),
                lines: Vec::new(),
                exit_code: None,
                is_complete: false,
                started_at: timestamp,
                execution_id: execution_id.to_string(),
            });
            entries.last_mut().expect("entry was just pushed")
        };

        if !command.trim().is_empty() {
            entry.command = command.to_string();
        }
        entry.exit_code = if is_final { exit_code } else { entry.exit_code };
        entry.is_complete = entry.is_complete || is_final;
        entry
            .lines
            .extend(parse_shell_lines(data, stream, timestamp));
        if entry.lines.len() > MAX_LINES_PER_STEP {
            let keep_from = entry.lines.len() - MAX_LINES_PER_STEP;
            entry.lines.drain(0..keep_from);
        }

        entries.sort_by(|left, right| right.started_at.cmp(&left.started_at));
    }
}

fn touch_execution(order: &mut VecDeque<String>, execution_id: &str) {
    if let Some(index) = order.iter().position(|value| value == execution_id) {
        order.remove(index);
    }
    order.push_back(execution_id.to_string());
}

fn evict_old_executions(state: &mut RuntimeState) {
    while state.execution_order.len() > MAX_EXECUTIONS {
        if let Some(oldest) = state.execution_order.pop_front() {
            state.shell_by_execution.remove(&oldest);
        }
    }
}

fn parse_shell_lines(data: &str, stream: &str, timestamp: i64) -> Vec<ExecutionPanelShellLine> {
    if data.is_empty() {
        return Vec::new();
    }

    let stream = match stream {
        "stderr" => "stderr",
        _ => "stdout",
    };

    let raw_lines = data.split('\n').collect::<Vec<_>>();
    let mut lines = raw_lines
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            if value.is_empty() && index == raw_lines.len() - 1 {
                return None;
            }
            Some(ExecutionPanelShellLine {
                text: (*value).to_string(),
                stream: stream.to_string(),
                timestamp,
            })
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {
        lines.push(ExecutionPanelShellLine {
            text: String::new(),
            stream: stream.to_string(),
            timestamp,
        });
    }

    lines
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shell_entries_append_lines_and_keep_latest_command() {
        let store = ExecutionPanelRuntimeStore::new();
        store
            .apply_shell_chunk(
                "exec-1",
                "step-1",
                0,
                "echo hi",
                "stdout",
                "hello\nworld\n",
                false,
                None,
                10,
            )
            .await;
        store
            .apply_shell_chunk(
                "exec-1",
                "step-1",
                0,
                "",
                "stderr",
                "oops",
                true,
                Some(1),
                11,
            )
            .await;

        let entries = store.shell_entries_for_execution("exec-1").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "echo hi");
        assert_eq!(entries[0].lines.len(), 3);
        assert_eq!(entries[0].lines[2].stream, "stderr");
        assert_eq!(entries[0].exit_code, Some(1));
        assert!(entries[0].is_complete);
    }
}
