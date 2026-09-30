//! Explicit composer queue admission and atomic queue-to-parallel handoff.
use super::*;
use crate::magician_v2::chat::voice_requests::{VoiceAdmission, VoiceRequest};

impl ChatService {
    pub async fn enqueue_composer_message(
        self: &Arc<Self>,
        message: QueuedMessage,
        credential: Option<Arc<crate::magician_v2::apps::boundary::AppOwnerExecutionCredential>>,
    ) -> Result<QueuedMessageReceipt> {
        let session_id = message.session_id.clone();
        let guard = self.lock_session_admission(&session_id).await;
        let mut queue = self.pending_messages.entry(session_id.clone()).or_default();
        // Explicit UI admission never silently evicts an earlier user message.
        if queue.len() >= MAX_QUEUED_PER_SESSION { return Err(anyhow!("queue_capacity_reached")); }
        let receipt = QueuedMessageReceipt {
            id: message.id.clone(), position: queue.len() + 1, dropped_oldest: None,
            reason: Some(super::super::models::QueueReason::InFlightTurn),
        };
        if let Some(credential) = credential {
            self.pending_queue_credentials.insert(message.id.clone(), credential);
        }
        queue.push_back(message);
        drop(queue);
        drop(guard);
        // Covers the race where the previous turn finished before this POST.
        self.spawn_pending_queue_drain(session_id);
        Ok(receipt)
    }

    pub async fn run_queued_message_in_parallel(
        self: &Arc<Self>, session_id: &str, message_id: &str,
        credential: Option<Arc<crate::magician_v2::apps::boundary::AppOwnerExecutionCredential>>,
    ) -> Result<VoiceRequest> {
        let _guard = self.lock_session_admission(session_id).await;
        let message = match self.list_queued_messages(session_id).into_iter().find(|message| message.id == message_id) {
            Some(message) => message,
            None => {
                let parent = self.chat_store.get_session(session_id).await?.ok_or_else(|| anyhow!("queue_session_not_found"))?;
                if let Some(request) = self.chat_store.voice_state(&parent.principal, &parent.workspace).await?.requests.into_iter()
                    .find(|request| request.parent_session_id == session_id && request.submission_id == format!("queue-{message_id}")) {
                    return Ok(request);
                }
                return Err(anyhow!("queue_message_already_started_or_removed"));
            },
        };
        if !message.attachment_ids.is_empty() || message.mode == ChatMessageMode::Plan {
            return Err(anyhow!("invalid_parallel_message_requires_plain_text"));
        }
        let text = message.text.clone().filter(|text| !text.trim().is_empty())
            .ok_or_else(|| anyhow!("invalid_parallel_message_requires_text"))?;
        let request = self.submit_concurrent_voice_request(session_id, VoiceAdmission {
            submission_id: format!("queue-{}", message.id),
            text, context_session_id: Some(session_id.to_string()),
            profile: message.profile_override, mode: message.mode,
            coding_choice: crate::magician_v2::vibedev::run_service::vibe_coding_choice_from_queue(message.coding_choice),
            source_surface: "web".to_string(), presence_session_id: message.presence_session_id,
            executor_id: String::new(),
        }, credential).await?;
        // Admission failure leaves the exact queued entry in place. The drain
        // holds this same fence before popping, so success cannot dispatch twice.
        self.delete_queued_message(session_id, message_id);
        Ok(request)
    }

    pub async fn stop_and_send_queued_message(self: &Arc<Self>, session_id: &str, message_id: &str) -> Result<()> {
        let guard = self.lock_session_admission(session_id).await;
        {
            let mut queue = self.pending_messages.get_mut(session_id)
                .ok_or_else(|| anyhow!("queue_message_already_started_or_removed"))?;
            let index = queue.iter().position(|message| message.id == message_id)
                .ok_or_else(|| anyhow!("queue_message_already_started_or_removed"))?;
            let message = queue.remove(index).expect("position was checked");
            queue.push_front(message);
        }
        self.cancel_chat_run(session_id).await?;
        self.release_tailed_task(session_id).await;
        drop(guard);
        self.spawn_pending_queue_drain(session_id.to_string());
        Ok(())
    }

    pub async fn remove_queued_message_admitted(&self, session_id: &str, message_id: &str) -> bool {
        let _guard = self.lock_session_admission(session_id).await;
        self.delete_queued_message(session_id, message_id)
    }

    pub async fn clear_queued_messages_admitted(&self, session_id: &str) -> usize {
        let _guard = self.lock_session_admission(session_id).await;
        self.clear_queued_messages(session_id)
    }
}
