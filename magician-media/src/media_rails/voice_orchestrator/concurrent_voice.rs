//! Opt-in asynchronous work for authenticated personal voice clients.
use super::*;
use magician::magician_v2::chat::voice_requests::{VoiceAdmission, VoiceRequest};

impl VoiceOrchestrator {
    pub fn concurrent_voice_instructions() -> &'static str {
        "Concurrent requests are enabled. You are the live conversational interface. For every substantive question, research, explanation, coding request or work requiring tools, call delegate_to_chat once with the user's exact request and relevant context. The background Magician agent has the full tool catalog and can produce written answers; do not look for a specialist or refuse because this voice session has no other tools. Each acceptance is independent and survives later questions and the call ending. Give only a short acceptance acknowledgement. The application speaks completed results when the listener is free; never invent, poll for, or repeat a delegated result. You may answer brief small talk and simple conversational questions directly. New speech interrupts audio, never cancels accepted work."
    }

    pub(super) fn apply_concurrent_voice_context(context: &mut VoiceSessionContext) {
        // Keep execution behind durable admission. Advertising the old direct
        // task/tool surface lets the provider bypass independent request
        // ownership and the result-delivery coordinator.
        context
            .tools
            .retain(|tool| tool.name == VOICE_DELEGATE_TO_CHAT_TOOL);
        for tool in &mut context.tools {
            tool.description = "Start an independent background request with Magician's full chat tools and reasoning. Returns an acceptance immediately, not the completed answer. Use once for each substantive question or task; the application queues and speaks its result. Include the user's exact words and relevant context in intent.".into();
        }
        context.instructions.push('\n');
        context
            .instructions
            .push_str(Self::concurrent_voice_instructions());
    }

    pub fn concurrent_voice_cancel_command(text: &str) -> Option<&'static str> {
        let normalized = text
            .trim()
            .trim_end_matches(['.', '!', '?'])
            .trim()
            .to_ascii_lowercase();
        match normalized.strip_prefix("please ").unwrap_or(&normalized) {
            "cancel that request" | "cancel this request" | "cancel the current request" => {
                Some("current")
            },
            "cancel the previous request" => Some("previous"),
            "cancel all background requests" => Some("all"),
            _ => None,
        }
    }

    pub async fn cancel_concurrent_voice_command(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<usize, OrchestratorError> {
        let command = Self::concurrent_voice_cancel_command(text)
            .ok_or_else(|| OrchestratorError::Io("invalid_voice_command".into()))?;
        let parent = self.chat_session_id_for_call().await?;
        let (principal, workspace) = {
            let guard = self.state.lock().await;
            let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
            if !state.concurrent_requests {
                return Err(OrchestratorError::NotConfigured(
                    "concurrent voice was not negotiated".into(),
                ));
            }
            (state.principal.clone(), state.workspace.clone())
        };
        let state = self
            .chat_service
            .concurrent_voice_state(&principal, &workspace)
            .await
            .map_err(|e| OrchestratorError::Io(e.to_string()))?;
        let mut candidates: Vec<_> = state
            .requests
            .iter()
            .filter(|r| {
                !r.task_notification && (r.work_status.is_active() || !r.pending_tasks.is_empty())
            })
            .filter(|r| {
                command == "all"
                    || if command == "current" && context.is_some() {
                        Some(r.branch_session_id.as_str()) == context
                    } else {
                        r.parent_session_id == parent
                    }
            })
            .collect();
        candidates.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        if command != "all" {
            candidates.truncate(1);
        }
        for request in &candidates {
            self.chat_service
                .cancel_concurrent_voice_request(&principal, &workspace, &request.id)
                .await
                .map_err(|e| OrchestratorError::Io(e.to_string()))?;
        }
        Ok(candidates.len())
    }

    pub(super) fn allows_concurrent_voice(source_surface: &str, requested: bool) -> bool {
        // A caller capability cannot promote a room/public surface to owner.
        requested && matches!(source_surface, "authenticated_realtime_voice" | "realtime_voice")
    }

    pub async fn concurrent_voice_enabled(&self) -> bool {
        self.state
            .lock()
            .await
            .as_ref()
            .is_some_and(|s| s.concurrent_requests)
    }

    pub async fn set_concurrent_voice_context(&self, context: Option<String>) {
        if let Some(state) = self.state.lock().await.as_mut() {
            state.concurrent_context_session_id = context;
        }
    }

    pub async fn submit_concurrent_voice_turn(
        &self,
        text: &str,
        submission_id: &str,
        context: Option<String>,
    ) -> Result<VoiceRequest, OrchestratorError> {
        let mut parent = self.chat_session_id_for_call().await?;
        let admission = {
            let guard = self.state.lock().await;
            let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
            if !state.concurrent_requests {
                return Err(OrchestratorError::NotConfigured(
                    "concurrent voice was not negotiated".into(),
                ));
            }
            VoiceAdmission {
                submission_id: submission_id.to_string(),
                text: text.to_string(),
                context_session_id: context,
                profile: state
                    .chat_choice
                    .as_ref()
                    .map(magician::magician_v2::execution::plane::encode_chat_harness_choice),
                mode: ChatMessageMode::Ask,
                coding_choice: state.coding_choice.clone(),
                source_surface: "authenticated_realtime_voice".into(),
                presence_session_id: Some(state.voice_session_id.clone()),
                executor_id: String::new(),
            }
        };
        if let Some(context_id) = admission.context_session_id.as_deref() {
            let context = self
                .chat_store
                .get_session(context_id)
                .await
                .map_err(|e| OrchestratorError::Io(e.to_string()))?
                .ok_or_else(|| OrchestratorError::Io("voice_context_not_found".into()))?;
            let guard = self.state.lock().await;
            let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
            if context.principal != state.principal
                || context.workspace != state.workspace
                || context.agent_id != state.chat_agent_id
            {
                return Err(OrchestratorError::Io("voice_context_scope_mismatch".into()));
            }
            if let Some(
                magician::magician_v2::chat::voice_requests::InternalVoiceSession::Branch {
                    parent_session_id,
                },
            ) = context.internal_voice
            {
                parent = parent_session_id;
            }
        }
        // Never extend a provider response's revocable app-owner grant to an
        // unrelated background job. The ordinary engine applies its own policy.
        self.chat_service
            .submit_concurrent_voice_request(&parent, admission, None)
            .await
            .map_err(|error| OrchestratorError::Io(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_voice_catalog_routes_work_through_durable_admission() {
        let mut context = VoiceSessionContext {
            instructions: "Existing persona".into(),
            tools: vec![
                "create_task",
                VOICE_DELEGATE_TO_CHAT_TOOL,
                "delegate_to_agent",
            ]
            .into_iter()
            .map(|name| LLMToolSpec {
                name: name.into(),
                description: "Old synchronous instruction".into(),
                parameters: json!({"type":"object"}),
            })
            .collect(),
            policy_snapshot_id: Some("scope-policy".into()),
            per_turn_context_configured: false,
            per_turn_context_enabled: false,
            per_turn_context_budget_ms: 100,
        };
        VoiceOrchestrator::apply_concurrent_voice_context(&mut context);
        assert_eq!(context.tools.len(), 1);
        assert_eq!(context.tools[0].name, VOICE_DELEGATE_TO_CHAT_TOOL);
        assert!(context.tools[0]
            .description
            .contains("acceptance immediately"));
        assert_eq!(context.policy_snapshot_id.as_deref(), Some("scope-policy"));
        assert!(context.instructions.starts_with("Existing persona"));
        assert!(context.instructions.contains("full tool catalog"));
    }
    #[test]
    fn concurrent_voice_cancellation_requires_an_explicit_request_command() {
        assert_eq!(
            VoiceOrchestrator::concurrent_voice_cancel_command(
                "Please cancel the previous request."
            ),
            Some("previous")
        );
        assert_eq!(
            VoiceOrchestrator::concurrent_voice_cancel_command("cancel all background requests"),
            Some("all")
        );
        assert_eq!(
            VoiceOrchestrator::concurrent_voice_cancel_command("cancel my flight"),
            None
        );
        assert_eq!(
            VoiceOrchestrator::concurrent_voice_cancel_command("do not cancel this request"),
            None
        );
    }
}
