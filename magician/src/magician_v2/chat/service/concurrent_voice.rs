//! Accepted voice work belongs to the execution runtime, independently of its
//! HTTP request, capture turn, provider socket and playback attempt.

use super::*;
use crate::magician_v2::chat::voice_requests::{
    VoiceAdmission, VoiceCoordinatorState, VoiceMutation, VoiceRequest, VoiceWorkStatus,
};
use crate::magician_v2::execution::runtime_boundary::spawn_execution_job;

static VOICE_EXECUTOR_ID: OnceLock<String> = OnceLock::new();
static VOICE_WORKERS: OnceLock<DashMap<String, CancellationToken>> = OnceLock::new();
static VOICE_RECONCILE_AT: OnceLock<DashMap<(String, String), i64>> = OnceLock::new();
static VOICE_CAPACITY: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn executor_id() -> &'static str {
    VOICE_EXECUTOR_ID.get_or_init(|| Uuid::new_v4().to_string())
}

fn workers() -> &'static DashMap<String, CancellationToken> {
    VOICE_WORKERS.get_or_init(DashMap::new)
}

struct WorkerGuard(String);
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        workers().remove(&self.0);
    }
}

impl ChatService {
    /// Persist admission before returning. The job is spawned exactly once for
    /// a newly committed receipt; retrying transports never launch it again.
    pub async fn submit_concurrent_voice_request(
        self: &Arc<Self>,
        parent_session_id: &str,
        mut admission: VoiceAdmission,
        credential: Option<Arc<crate::magician_v2::apps::boundary::AppOwnerExecutionCredential>>,
    ) -> Result<VoiceRequest> {
        let parent = self
            .chat_store
            .get_session(parent_session_id)
            .await?
            .ok_or_else(|| anyhow!("voice_parent_not_found"))?;
        // This seam is for authenticated personal listening surfaces. A room
        // or public contact must not acquire background owner execution by
        // opting into a different transport contract.
        if !matches!(
            admission.source_surface.as_str(),
            "web" | "mobile" | "authenticated_realtime_voice" | "dictation" | "global_voice_note"
        ) {
            return Err(anyhow!("concurrent_voice_surface_not_supported"));
        }
        admission.executor_id = executor_id().to_string();
        let (request, fresh) = self
            .chat_store
            .admit_voice_request(parent_session_id, admission.clone())
            .await?;
        if !fresh {
            return Ok(request);
        }
        let cancel = CancellationToken::new();
        workers().insert(request.id.clone(), cancel.clone());
        let worker_guard = WorkerGuard(request.id.clone());
        let service = Arc::clone(self);
        let work = request.clone();
        spawn_execution_job(move || async move {
            let _guard = worker_guard;
            let capacity = VOICE_CAPACITY.get_or_init(|| Arc::new(Semaphore::new(4)));
            let _permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                permit = capacity.acquire() => match permit { Ok(p) => p, Err(_) => return },
            };
            if !service
                .chat_store
                .get_session(&parent.id)
                .await
                .ok()
                .flatten()
                .is_some_and(|p| p.status == ChatSessionStatus::Active)
            {
                let _ = service
                    .cancel_concurrent_voice_request(&parent.principal, &parent.workspace, &work.id)
                    .await;
                return;
            }
            if let Err(error) = service
                .chat_store
                .mutate_voice_state(
                    &parent.principal,
                    &parent.workspace,
                    VoiceMutation::Start {
                        request_id: work.id.clone(),
                        executor_id: executor_id().to_string(),
                    },
                )
                .await
            {
                warn!(request_id = %work.id, %error, "voice request could not start");
                return;
            }
            let response = CHAT_APP_OWNER_EXECUTION_CREDENTIAL
                .scope(credential, async {
                    // Construct and poll the entire future here, not in an Actix
                    // actor. Cancellation cannot race a separately spawned child
                    // that has not yet registered its active-run token.
                    let turn = Box::pin(service.process_message_with_mode(
                        &work.branch_session_id,
                        Some(&admission.text),
                        &[],
                        admission.profile.as_deref(),
                        admission.mode,
                        None,
                        None,
                        Some(&work.chat_turn_id),
                        true,
                        Some(&admission.source_surface),
                        admission.presence_session_id.as_deref(),
                        None,
                        admission.coding_choice,
                    ));
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            let _ = service.cancel_chat_run(&work.branch_session_id).await;
                            Ok(service.build_cancelled_response(&work.branch_session_id))
                        },
                        result = turn => result,
                    }
                })
                .await;
            if let Err(error) = service
                .finish_concurrent_voice_request(&parent, &work, response)
                .await
            {
                warn!(request_id = %work.id, %error, "voice result awaits reconciliation");
            }
        });
        Ok(request)
    }

    async fn finish_concurrent_voice_request(
        &self,
        parent: &ChatSession,
        request: &VoiceRequest,
        response: Result<ChatResponse>,
    ) -> Result<()> {
        let (status, message, error) = match response {
            Ok(response) if response.cancelled => (VoiceWorkStatus::Cancelled, None, None),
            Ok(response) if response.queued.is_some() => (
                VoiceWorkStatus::Failed,
                None,
                Some(
                    "The execution context was unexpectedly busy. Please retry this request."
                        .to_string(),
                ),
            ),
            Ok(response) => (VoiceWorkStatus::Completed, response.assistant_message, None),
            Err(error) => {
                warn!(request_id = %request.id, %error, "concurrent voice execution failed");
                (
                    VoiceWorkStatus::Failed,
                    None,
                    Some(
                        "This request could not finish. Open its context for details or retry."
                            .to_string(),
                    ),
                )
            },
        };
        // The model's live chat event can precede its asynchronous store sink.
        // Persist the canonical result now; voice-turn appends dedupe replay.
        let mut speech = None;
        let mut result_message_id = None;
        if let Some(message) = message.as_ref() {
            self.chat_store
                .append_message(&request.branch_session_id, message.clone())
                .await?;
            result_message_id = Some(message.id.clone());
            speech = Some(voice_result_speech(request, &message));
        } else if let Some(error) = error.as_ref() {
            speech = Some(format!("About {}: {error}", request.title));
        }
        let state = self
            .chat_store
            .mutate_voice_state(
                &parent.principal,
                &parent.workspace,
                VoiceMutation::Finish {
                    request_id: request.id.clone(),
                    executor_id: request.executor_id.clone(),
                    status,
                    result_message_id,
                    speech_text: speech,
                    error,
                },
            )
            .await?;
        if let Some(request) = state
            .requests
            .iter()
            .find(|r| r.id == request.id && r.work_status == VoiceWorkStatus::Completed)
        {
            if let Some(message) = message.as_ref() {
                self.project_voice_result(parent, request, message).await?;
            }
        }
        Ok(())
    }

    async fn project_voice_result(
        &self,
        parent: &ChatSession,
        request: &VoiceRequest,
        result: &ChatMessage,
    ) -> Result<()> {
        if parent.status != ChatSessionStatus::Active {
            return Ok(());
        }
        // Keep artifact/file references in their canonical execution context.
        // The parent gets a concise text projection; View result loads the
        // original message with the branch's path resolver.
        let mut projected = ChatMessage::new(
            format!("{}-result", request.id),
            &parent.id,
            ChatMessageDirection::Assistant,
            ChatMessageContent::Text {
                text: voice_result_speech(request, result),
                plan_reply: None,
            },
            result.created_at,
        );
        projected.chat_turn_id = Some(request.chat_turn_id.clone());
        projected.context_origin = Some(voice_result_origin(parent, request, result));
        projected.voice_origin = Some(false);
        if !self.chat_store
            .upsert_voice_result_projection(parent, request, projected.clone())
            .await? {
            return Ok(());
        }
        self.event_broadcaster
            .emit_transport_only(RuntimeTransportEvent::ChatMessageReceived {
                session_id: parent.id.clone(),
                message: projected,
                principal: Some(parent.principal.clone()),
                workspace: Some(parent.workspace.clone()),
                origin_channel: Some(parent.origin_channel.clone()),
                timestamp: Utc::now().timestamp_millis(),
            });
        Ok(())
    }

    /// Restart recovery reconciles already committed results before reporting
    /// interrupted work. It never replays a tool action or an opaque LLM future.
    pub async fn concurrent_voice_state(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<VoiceCoordinatorState> {
        let mut snapshot = self.chat_store.voice_state(principal, workspace).await?;
        let now = Utc::now().timestamp_millis();
        let reconcile = {
            let mut last = VOICE_RECONCILE_AT
                .get_or_init(DashMap::new)
                .entry((principal.to_owned(), workspace.to_owned()))
                .or_insert(0);
            if now - *last >= 10_000 {
                *last = now;
                true
            } else {
                false
            }
        };
        if reconcile {
            for request in snapshot.requests.iter().filter(|r| {
                !r.task_notification
                    && (!r.pending_tasks.is_empty() || now - r.created_at < 3_600_000)
            }) {
                if let Some(branch) = self
                    .chat_store
                    .get_session(&request.branch_session_id)
                    .await?
                {
                    let messages = self.chat_store.get_messages(&branch.id, 256).await?;
                    let saved_ids: std::collections::HashSet<_> =
                        messages.iter().map(|m| m.id.clone()).collect();
                    let messages = self
                        .reconcile_terminal_task_status_messages(&branch, messages)
                        .await;
                    // Only the latest row for each task owns its current state.
                    let mut seen = std::collections::HashSet::new();
                    for message in messages.into_iter().rev() {
                        if let ChatMessageContent::TaskStatusUpdate { task_id, .. } =
                            &message.content
                        {
                            if seen.insert(task_id.clone()) {
                                if !saved_ids.contains(&message.id) {
                                    self.chat_store
                                        .append_message(&branch.id, message.clone())
                                        .await?;
                                }
                                self.chat_store
                                    .record_concurrent_voice_task(&branch, &message)
                                    .await?;
                            }
                        }
                    }
                }
                if request.work_status == VoiceWorkStatus::Cancelled {
                    self.cancel_concurrent_voice_tasks(principal, workspace, request)
                        .await?;
                }
            }
            snapshot = self.chat_store.voice_state(principal, workspace).await?;
        }
        for request in &snapshot.requests {
            if !self
                .chat_store
                .get_session(&request.parent_session_id)
                .await?
                .is_some_and(|p| p.status == ChatSessionStatus::Active)
            {
                if request.work_status.is_active() || !request.pending_tasks.is_empty() {
                    self.cancel_concurrent_voice_request(principal, workspace, &request.id)
                        .await?;
                }
                if request.delivery_status
                    != crate::magician_v2::chat::voice_requests::VoiceDeliveryStatus::Dismissed
                {
                    self.chat_store
                        .mutate_voice_state(
                            principal,
                            workspace,
                            VoiceMutation::Dismiss {
                                request_id: request.id.clone(),
                            },
                        )
                        .await?;
                }
                continue;
            }
            let orphan = request.work_status.is_active()
                && (request.executor_id != executor_id()
                    || (!workers().contains_key(&request.id)
                        && Utc::now().timestamp_millis() - request.created_at > 30_000));
            // Also repair legacy receipts whose final result was refreshed
            // without invalidating the old parent projection.
            let unprojected = request.result_message_id.is_some()
                && (!request.result_projected || (reconcile && request.task_notification));
            if !orphan && !unprojected {
                continue;
            }
            let result = self
                .chat_store
                .get_messages(&request.branch_session_id, 256)
                .await
                .ok()
                .and_then(|messages| {
                    messages.into_iter().rev().find(|m| {
                        request.result_message_id.as_deref().map_or_else(
                            || {
                                m.direction == ChatMessageDirection::Assistant
                                    && m.chat_turn_id.as_deref() == Some(&request.chat_turn_id)
                                    && m.content.text_content().is_some()
                            },
                            |id| m.id == id,
                        )
                    })
                });
            if let Some(result) = result {
                if request.task_notification && reconcile {
                    if let Some(branch) = self.chat_store.get_session(&request.branch_session_id).await? {
                        self.chat_store.record_concurrent_voice_task(&branch, &result).await?;
                    }
                }
                let state = self
                    .chat_store
                    .mutate_voice_state(
                        principal,
                        workspace,
                        VoiceMutation::Finish {
                            request_id: request.id.clone(),
                            executor_id: request.executor_id.clone(),
                            status: VoiceWorkStatus::Completed,
                            result_message_id: Some(result.id.clone()),
                            speech_text: Some(voice_result_speech(request, &result)),
                            error: None,
                        },
                    )
                    .await?;
                if let Some(request) = state
                    .requests
                    .iter()
                    .find(|r| r.id == request.id && r.result_message_id.is_some())
                {
                    if let Some(parent) = self
                        .chat_store
                        .get_session(&request.parent_session_id)
                        .await?
                    {
                        self.project_voice_result(&parent, request, &result).await?;
                    }
                }
            } else if orphan {
                self.chat_store.mutate_voice_state(principal, workspace, VoiceMutation::Finish {
                    request_id: request.id.clone(), executor_id: request.executor_id.clone(), status: VoiceWorkStatus::Interrupted,
                    result_message_id: None, speech_text: None,
                    error: Some("Execution was interrupted. Review the saved context before retrying; actions are not automatically repeated.".into()),
                }).await?;
            }
        }
        if snapshot
            .requests
            .iter()
            .any(|r| r.work_status.is_active() && r.executor_id != executor_id())
        {
            return self
                .chat_store
                .mutate_voice_state(
                    principal,
                    workspace,
                    VoiceMutation::Recover {
                        executor_id: executor_id().to_string(),
                    },
                )
                .await;
        }
        self.chat_store.voice_state(principal, workspace).await
    }

    pub async fn concurrent_voice_result(
        &self,
        principal: &str,
        workspace: &str,
        request_id: &str,
    ) -> Result<ChatMessage> {
        let state = self.chat_store.voice_state(principal, workspace).await?;
        let request = state
            .requests
            .iter()
            .find(|r| r.id == request_id)
            .ok_or_else(|| anyhow!("voice_request_not_found"))?;
        let result_id = request
            .result_message_id
            .as_deref()
            .ok_or_else(|| anyhow!("voice_result_not_found"))?;
        self.chat_store
            .get_messages(&request.branch_session_id, 256)
            .await?
            .into_iter()
            .find(|message| message.id == result_id)
            .ok_or_else(|| anyhow!("voice_result_not_found"))
    }

    pub async fn mutate_concurrent_voice_delivery(
        &self,
        principal: &str,
        workspace: &str,
        mutation: VoiceMutation,
    ) -> Result<VoiceCoordinatorState> {
        self.chat_store
            .mutate_voice_state(principal, workspace, mutation)
            .await
    }

    async fn cancel_concurrent_voice_tasks(
        &self,
        principal: &str,
        workspace: &str,
        request: &VoiceRequest,
    ) -> Result<()> {
        let Some(v3) = self.artifact_v2_service.as_ref() else {
            return Ok(());
        };
        let scope = ScopeRef::system_internal_unauthenticated(principal, workspace);
        for task_id in &request.pending_tasks {
            let task = v3.get_task(&scope, task_id).await?;
            if task.manifest.chat_session_id.as_deref() != Some(&request.branch_session_id) {
                continue;
            }
            if let Some(execution_id) = task.state.active_root_execution_id.as_deref() {
                v3.cancel_execution_by_id(&scope, execution_id).await?;
            }
        }
        Ok(())
    }

    pub async fn cancel_concurrent_voice_request(
        &self,
        principal: &str,
        workspace: &str,
        request_id: &str,
    ) -> Result<VoiceCoordinatorState> {
        let prior = self.chat_store.voice_state(principal, workspace).await?;
        let request = prior
            .requests
            .iter()
            .find(|r| r.id == request_id)
            .ok_or_else(|| anyhow!("voice_request_not_found"))?;
        self.cancel_concurrent_voice_tasks(principal, workspace, request)
            .await?;
        let state = self
            .chat_store
            .mutate_voice_state(
                principal,
                workspace,
                VoiceMutation::Cancel {
                    request_id: request_id.to_string(),
                },
            )
            .await?;
        if let Some(cancel) = workers().get(request_id) {
            cancel.cancel();
        }
        Ok(state)
    }
}

fn voice_result_speech(request: &VoiceRequest, message: &ChatMessage) -> String {
    let body = message
        .speech_segments
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|segments| {
            segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .or_else(|| message.content.text_content().map(str::to_owned))
        .or_else(|| match &message.content {
            ChatMessageContent::TaskStatusUpdate {
                speech_tts,
                summary,
                status,
                ..
            } => Some(
                speech_tts
                    .clone()
                    .or_else(|| summary.clone())
                    .unwrap_or_else(|| status.clone()),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let mut summary: String = body.chars().take(1100).collect();
    if body.chars().count() > 1100 {
        if let Some(end) = summary.rfind(['.', '!', '?']).filter(|end| *end > 200) {
            summary.truncate(end + 1);
        }
        summary.push_str(" The full answer is saved in this conversation.");
    }
    format!("Back to {}. {}", request.title, summary)
}

fn voice_result_origin(
    parent: &ChatSession,
    request: &VoiceRequest,
    result: &ChatMessage,
) -> super::super::models::ChatMessageContextOrigin {
    super::super::models::ChatMessageContextOrigin {
        ui_thread_id: parent.ui_thread_id.clone(),
        session_id: request.branch_session_id.clone(),
        request_id: request.id.clone(),
        message_id: Some(result.id.clone()),
        result_created_at: Some(result.created_at),
    }
}

#[cfg(test)]
mod concurrent_voice_link_tests {
    use super::super::super::models::ChatMessageContextOrigin;

    #[test]
    fn concurrent_voice_source_link_survives_serialization_and_old_records() {
        let old = serde_json::json!({
            "ui_thread_id": "research", "session_id": "voice-branch-one",
            "request_id": "voice-request-one"
        });
        let mut origin: ChatMessageContextOrigin = serde_json::from_value(old).unwrap();
        assert!(origin.message_id.is_none());
        origin.message_id = Some("canonical-answer".into());
        let saved = serde_json::to_value(&origin).unwrap();
        let restored: ChatMessageContextOrigin = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.message_id.as_deref(), Some("canonical-answer"));
        assert_eq!(restored.session_id, "voice-branch-one");
        assert_eq!(restored.ui_thread_id, "research");
    }
}
