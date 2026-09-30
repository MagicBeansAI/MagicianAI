//! A pending delegation is an asynchronous receipt, not permission to relaunch
//! the same worker because its result is not available on the next model step.
use super::*;

fn current_turn_delegations(history: &[ChatLlmTranscriptEntry], args: &Value) -> Vec<Value> {
    let targets: HashSet<&str> = args
        .get("delegation_targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|target| target.get("target_agent_id").and_then(Value::as_str))
        .map(str::trim)
        .collect();
    let start = history
        .iter()
        .rposition(|entry| {
            matches!(
                entry,
                ChatLlmTranscriptEntry::UserText { .. } | ChatLlmTranscriptEntry::UserTurn { .. }
            )
        })
        .map_or(0, |index| index + 1);
    let mut calls = HashSet::new();
    let mut seen = HashSet::new();
    let mut receipts = Vec::new();
    for entry in &history[start..] {
        let value = match entry {
            ChatLlmTranscriptEntry::AssistantTurn { tool_calls, .. } => {
                calls.extend(
                    tool_calls
                        .iter()
                        .filter(|call| call.name == "delegate_to_agent")
                        .map(|call| call.id.as_str()),
                );
                continue;
            },
            ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id,
                projection,
                ..
            } if calls.contains(tool_call_id.as_str())
                && projection.identity.tool_call_id == *tool_call_id
                && projection.validate_schema_version().is_ok() =>
            {
                projection
                    .model
                    .value
                    .get("data")
                    .cloned()
                    .unwrap_or(Value::Null)
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id,
                content,
                ..
            } if calls.contains(tool_call_id.as_str()) => {
                serde_json::from_str(content).unwrap_or(Value::Null)
            },
            _ => continue,
        };
        for receipt in value
            .get("delegations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let target = receipt
                .get("target_agent_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let task_id = receipt
                .get("task_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if targets.contains(target) && !task_id.is_empty() && seen.insert(task_id.to_owned()) {
                receipts.push(receipt.clone());
            }
        }
    }
    receipts
}

impl ChatService {
    pub(super) async fn pending_delegation_receipt(
        &self,
        session: &ChatSession,
        args: &Value,
    ) -> Result<Option<Value>> {
        let Some(service) = self.artifact_v2_service.as_ref() else {
            return Ok(None);
        };
        let history = self
            .chat_store
            .get_llm_history_tail(&session.id, 128)
            .await?;
        let scope =
            ScopeRef::system_internal_unauthenticated(&session.principal, &session.workspace);
        let mut pending = Vec::new();
        for receipt in current_turn_delegations(&history, args) {
            let task_id = receipt["task_id"].as_str().unwrap_or_default();
            let task = match service.get_task(&scope, task_id).await {
                Ok(task) => task,
                Err(ArtifactV2Error::TaskNotFound(_)) => continue,
                Err(error) => return Err(error.into()),
            };
            if task.manifest.chat_session_id.as_deref() != Some(&session.id)
                || Some(task.manifest.agent_id.as_str()) != receipt["target_agent_id"].as_str()
                || !matches!(
                    task.manifest.created_by.as_str(),
                    "chat_delegate" | "chat_delegate_tracked"
                )
            {
                continue;
            }
            if !matches!(
                task.state.status.as_str(),
                "completed" | "failed" | "cancelled"
            ) || !task.state.synthesis_pending_executions.is_empty()
            {
                pending.push(receipt);
            }
        }
        Ok((!pending.is_empty()).then(|| json!({
            "status": "enqueued",
            "delegations": pending,
            "reused_existing": true,
            "note": "No new work was started. These agents already have accepted work from this user turn and their results arrive asynchronously. A running task with no output is not stalled. Acknowledge that work is underway and let its existing task card deliver completion; do not poll rapidly or redelegate it. Independent parallel assignments belong together in the initial delegation_targets batch. Other agents and later user turns remain independent.",
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::chat::models::StoredToolCall;

    #[test]
    fn concurrent_voice_rephrased_delegation_reuses_current_turn_but_not_another_question() {
        let receipt = json!({"status":"enqueued", "delegations":[
            {"target_agent_id":"writer","task_id":"task-one","execution_id":"exec-one"},
            {"target_agent_id":"writer","task_id":"task-two","execution_id":"exec-two"}
        ]});
        let mut history = vec![
            ChatLlmTranscriptEntry::UserText {
                text: "Explain eclipses".into(),
            },
            ChatLlmTranscriptEntry::AssistantTurn {
                text: None,
                provider_state: None,
                tool_calls: vec![StoredToolCall {
                    id: "delegate".into(),
                    name: "delegate_to_agent".into(),
                    arguments: json!({}),
                }],
            },
            ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: "delegate".into(),
                tool_name: Some("delegate_to_agent".into()),
                content: receipt.to_string(),
            },
        ];
        let retry = json!({"delegation_targets":[{"target_agent_id":"writer","context":"The earlier work seems stalled; write it again now"}]});
        assert_eq!(
            current_turn_delegations(&history, &retry).len(),
            2,
            "retain all independently admitted children in the original batch"
        );
        assert!(current_turn_delegations(
            &history,
            &json!({"delegation_targets":[{"target_agent_id":"researcher"}]})
        )
        .is_empty());
        history.push(ChatLlmTranscriptEntry::UserText {
            text: "Now write about volcanoes".into(),
        });
        assert!(
            current_turn_delegations(&history, &retry).is_empty(),
            "new questions may use the same agent concurrently"
        );
    }
}
