//! Voice request persistence reuses ChatStore's typed metadata owner, lifecycle
//! fence and durable commit. The coordinator is scoped to a listener workspace,
//! so results from different visible sessions still share one output lease.

use super::*;
use crate::magician_v2::chat::models::{
    ChatMessageContextOrigin, ChatMessageDirection, TranscriptBlock,
};
use crate::magician_v2::chat::voice_requests::{
    validate_voice_key, InternalVoiceSession, VoiceDeliveryStatus, VoiceWorkStatus,
};

fn coordinator_id(principal: &str, workspace: &str) -> String {
    let digest = Sha256::digest(format!("{}:{principal}{workspace}", principal.len()));
    format!("voice-coordinator-{digest:x}")
}

fn state_from_session(session: &ChatSession) -> Result<&VoiceCoordinatorState> {
    match session.internal_voice.as_ref() {
        Some(InternalVoiceSession::Coordinator { state }) => Ok(state),
        _ => Err(anyhow!("invalid_voice_coordinator")),
    }
}

fn concurrent_task_title(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title: String = normalized.chars().take(56).collect();
    if normalized.chars().count() > 56 {
        if let Some(boundary) = title.rfind(' ').filter(|&boundary| boundary > 10) {
            title.truncate(boundary);
        }
        title.push('…');
    }
    title
}

fn concurrent_session_title(sequence: u64, text: &str) -> String {
    format!("#con{sequence}-{}", concurrent_task_title(text))
}

impl FileChatStore {
    /// Commit a result copy and its receipt under the same coordinator fence.
    /// A newer task result cannot be overwritten or marked projected by an old
    /// reconciler. Replaying the bus event still uses append's ordinary dedupe.
    pub(super) async fn upsert_voice_projection_inner(
        &self,
        parent: &ChatSession,
        request: &VoiceRequest,
        mut message: ChatMessage,
    ) -> Result<bool> {
        let id = coordinator_id(&parent.principal, &parent.workspace);
        let _coordinator_guard = self.lock_session(&id).await?;
        let mut coordinator = self.load_document(&id).await?;
        let mut state = state_from_session(&coordinator.session)?.clone();
        let Some(current) = state.requests.iter().find(|r| r.id == request.id) else {
            return Ok(false);
        };
        if current.parent_session_id != parent.id
            || current.result_message_id != request.result_message_id
            || current.speech_text != request.speech_text
            || current.title != request.title
        {
            return Ok(false);
        }
        let origin = message.context_origin.as_ref().ok_or_else(|| anyhow!("voice_projection_origin_missing"))?;
        if message.id != format!("{}-result", current.id)
            || message.session_id != parent.id
            || origin.session_id != current.branch_session_id
            || origin.request_id != current.id
            || origin.message_id != current.result_message_id
        {
            return Err(anyhow!("voice_projection_origin_mismatch"));
        }
        let _parent_guard = self.lock_session(&parent.id).await?;
        let mut doc = self.load_document(&parent.id).await?;
        if doc.session.status != ChatSessionStatus::Active {
            return Ok(false);
        }
        self.ensure_segmented_document_for_write(&mut doc).await?;
        let mut found = false;
        let mut changed = false;
        for (_, path) in self.list_message_segments(&doc).await? {
            let mut messages = self.load_message_segment(&path).await?;
            if let Some(existing) = messages.iter_mut().find(|m| m.id == message.id) {
                // Keep its original place in the conversation when the answer
                // gains a final summary or a better canonical source link.
                message.created_at = existing.created_at;
                changed = serde_json::to_value(&*existing)? != serde_json::to_value(&message)?;
                if changed {
                    *existing = message.clone();
                    self.save_message_segment(&path, &messages).await?;
                }
                found = true;
                break;
            }
        }
        if !found {
            self.append_segmented_message(&doc, message).await?;
            changed = true;
        }
        if changed {
            doc.session.updated_at = Utc::now().timestamp_millis();
            doc.messages.clear();
            self.save_document(&doc).await?;
            self.index.update_timestamp(&parent.principal, &parent.workspace, &parent.ui_thread_id, &parent.id, doc.session.updated_at);
        }
        state.apply(VoiceMutation::Projected { request_id: request.id.clone() }, Utc::now().timestamp_millis())?;
        coordinator.session.internal_voice = Some(InternalVoiceSession::Coordinator { state });
        self.save_document(&coordinator).await?;
        Ok(changed)
    }

    /// Admission bypasses the ordinary first-message title generator. Take the
    /// parent lock before checking so a concurrent rename is never overwritten.
    async fn name_voice_parent_if_untitled(&self, session_id: &str, title: &str) -> Result<()> {
        let _guard = self.lock_session(session_id).await?;
        let mut doc = self.load_document(session_id).await?;
        if doc.session.title.as_deref().is_some_and(|title| !title.trim().is_empty()) {
            return Ok(());
        }
        self.ensure_segmented_document_for_write(&mut doc).await?;
        doc.session.title = Some(title.to_owned());
        doc.session.updated_at = Utc::now().timestamp_millis();
        self.save_document(&doc).await?;
        self.index.update_title(
            &doc.session.principal,
            &doc.session.workspace,
            &doc.session.ui_thread_id,
            session_id,
            doc.session.title.clone(),
            doc.session.updated_at,
        );
        Ok(())
    }

    async fn ensure_voice_coordinator(&self, principal: &str, workspace: &str) -> Result<String> {
        let id = coordinator_id(principal, workspace);
        let lock = self.get_scope_lock(principal, workspace, "__voice_coordinator");
        let _guard = lock.lock().await;
        if self.get_session(&id).await?.is_some() {
            return Ok(id);
        }
        let mut session = self.create_session_inner(
            principal,
            workspace,
            "general",
            &ChatChannel::web(),
            crate::magician_v2::chat::DEFAULT_AGENT_ID,
            HistoryLane::Personal,
        );
        session.id = id.clone();
        session.internal_voice = Some(InternalVoiceSession::Coordinator {
            state: VoiceCoordinatorState::default(),
        });
        self.save_new_document_with_lifecycle(&ChatSessionDocument {
            format_version: CHAT_SESSION_DOCUMENT_FORMAT_VERSION,
            session,
            messages: Vec::new(),
            llm_history: Vec::new(),
        })
        .await?;
        Ok(id)
    }

    pub(super) async fn load_voice_state(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<VoiceCoordinatorState> {
        match self
            .get_session(&coordinator_id(principal, workspace))
            .await?
        {
            Some(session) => Ok(state_from_session(&session)?.clone()),
            None => Ok(VoiceCoordinatorState::default()),
        }
    }

    pub(super) async fn mutate_voice_state_inner(
        &self,
        principal: &str,
        workspace: &str,
        mutation: VoiceMutation,
    ) -> Result<VoiceCoordinatorState> {
        let id = self.ensure_voice_coordinator(principal, workspace).await?;
        let _guard = self.lock_session(&id).await?;
        let mut doc = self.load_document(&id).await?;
        let mut state = state_from_session(&doc.session)?.clone();
        state.apply(mutation, Utc::now().timestamp_millis())?;
        doc.session.internal_voice = Some(InternalVoiceSession::Coordinator {
            state: state.clone(),
        });
        self.save_document(&doc).await?;
        Ok(state)
    }

    /// Called after committing a canonical branch message and releasing its
    /// lock: admission takes coordinator then branch locks, so the opposite
    /// order here would deadlock. Task result identity survives retransmission.
    pub(super) async fn record_voice_task_update(
        &self,
        session: &ChatSession,
        message: &ChatMessage,
    ) -> Result<()> {
        if !matches!(
            session.internal_voice,
            Some(InternalVoiceSession::Branch { .. })
        ) {
            return Ok(());
        }
        let ChatMessageContent::TaskStatusUpdate {
            task_id,
            status,
            display_label,
            summary,
            execution_id,
            synthesis_pending,
            speech_tts,
            ..
        } = &message.content
        else {
            return Ok(());
        };
        let task_key = format!(
            "{}:{}",
            task_id,
            execution_id.as_deref().unwrap_or("default")
        );
        let terminal_status = if *synthesis_pending {
            None
        } else {
            match status.as_str() {
                "completed" => Some(VoiceWorkStatus::Completed),
                "failed" => Some(VoiceWorkStatus::Failed),
                "cancelled" => Some(VoiceWorkStatus::Cancelled),
                _ => None,
            }
        };
        let notification_id = format!(
            "voice-task-result-{:x}",
            Sha256::digest(format!("{}:{task_key}", session.id))
        );
        let title = display_label
            .clone()
            .filter(|title| !title.trim().is_empty())
            .or_else(|| session.title.clone())
            .unwrap_or_else(|| "Background task".into());
        let speech_text = format!(
            "Back to {title}. {}",
            speech_tts
                .as_deref()
                .or(summary.as_deref())
                .unwrap_or(status)
        );
        self.mutate_voice_state_inner(
            &session.principal,
            &session.workspace,
            VoiceMutation::TaskUpdate {
                branch_session_id: session.id.clone(),
                task_key,
                notification_id,
                message_id: message.id.clone(),
                message_created_at: message.created_at,
                title,
                terminal_status,
                speech_text,
            },
        )
        .await?;
        Ok(())
    }

    pub(super) async fn admit_voice_request_inner(
        &self,
        parent_session_id: &str,
        admission: VoiceAdmission,
    ) -> Result<(VoiceRequest, bool)> {
        validate_voice_key(&admission.submission_id)?;
        if admission.text.trim().is_empty() || admission.text.len() > 32_768 {
            return Err(anyhow!("invalid_voice_text"));
        }
        if admission.profile.as_ref().is_some_and(|p| p.len() > 4096) {
            return Err(anyhow!("invalid_voice_profile"));
        }
        let parent = self
            .get_session(parent_session_id)
            .await?
            .ok_or_else(|| anyhow!("voice_parent_not_found"))?;
        // A source link opens a real execution conversation. It can own a new
        // independent question just like a visible session: keep the viewed
        // branch as context and project the new answer back into it. Only the
        // internal coordinator is not a conversation.
        if parent.status != ChatSessionStatus::Active
            || matches!(parent.internal_voice, Some(InternalVoiceSession::Coordinator { .. }))
        {
            return Err(anyhow!("voice_parent_not_active"));
        }
        let coordinator = self
            .ensure_voice_coordinator(&parent.principal, &parent.workspace)
            .await?;
        let _guard = self.lock_session(&coordinator).await?;
        let mut doc = self.load_document(&coordinator).await?;
        let mut state = state_from_session(&doc.session)?.clone();
        // Length-framed JSON gives unambiguous field boundaries. Executor ID is
        // intentionally excluded so a retry after restart recovers its receipt.
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                parent_session_id,
                admission.text.trim(),
                &admission.context_session_id,
                &admission.profile,
                admission.mode,
                &admission.coding_choice,
                &admission.source_surface,
                &admission.presence_session_id,
            ))?)
        );
        if let Some(existing) = state.check_submission(&admission.submission_id, &fingerprint)? {
            return Ok((existing, false));
        }
        let now = Utc::now().timestamp_millis();
        state.prune_consumed(now);
        state.check_capacity()?;

        let context_id = admission
            .context_session_id
            .as_deref()
            .unwrap_or(parent_session_id);
        let context = self
            .get_session(context_id)
            .await?
            .ok_or_else(|| anyhow!("voice_context_not_found"))?;
        if context.principal != parent.principal
            || context.workspace != parent.workspace
            || context.agent_id != parent.agent_id
        {
            return Err(anyhow!("voice_context_scope_mismatch"));
        }
        if context_id != parent_session_id
            && !matches!(&context.internal_voice,
            Some(InternalVoiceSession::Branch { parent_session_id: owner }) if owner == parent_session_id)
        {
            return Err(anyhow!("voice_context_parent_mismatch"));
        }
        let key = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                &parent.principal,
                &parent.workspace,
                &admission.submission_id
            ))?)
        );
        let id = format!("voice-request-{key}");
        let branch_id = format!("voice-branch-{key}");
        // A retained execution session is also the permanent idempotency fence
        // after its consumed receipt ages out of the bounded coordinator index.
        // A partial admission never runs merely because a client retries it.
        if self.get_session(&branch_id).await?.is_some() {
            return Err(anyhow!("voice_submission_requires_recovery"));
        }
        let title: String = admission.text.trim().chars().take(120).collect();
        state.session_sequence = state.session_sequence.checked_add(1)
            .ok_or_else(|| anyhow!("voice_session_sequence_exhausted"))?;
        let session_title = concurrent_session_title(state.session_sequence, &admission.text);
        // Reserve before creating the branch: even an interrupted admission
        // cannot reuse a name already attached to a retained execution session.
        doc.session.internal_voice = Some(InternalVoiceSession::Coordinator { state: state.clone() });
        self.save_document(&doc).await?;
        let mut branch = self.create_session_inner(
            &parent.principal,
            &parent.workspace,
            &parent.ui_thread_id,
            &parent.origin_channel,
            &parent.agent_id,
            HistoryLane::Automated,
        );
        branch.id = branch_id.clone();
        branch.title = Some(session_title.clone());
        branch.internal_voice = Some(InternalVoiceSession::Branch {
            parent_session_id: parent.id.clone(),
        });
        self.save_new_document_with_lifecycle(&ChatSessionDocument {
            format_version: CHAT_SESSION_DOCUMENT_FORMAT_VERSION,
            session: branch.clone(),
            messages: Vec::new(),
            llm_history: Vec::new(),
        })
        .await?;

        self.index.insert(&branch.principal, &branch.workspace, &branch.ui_thread_id, SessionIndexEntry {
            session_id: branch.id.clone(), status: branch.status.clone(),
            created_at: branch.created_at, updated_at: branch.updated_at,
            title: branch.title.clone(), agent_id: branch.agent_id.clone(),
            history_lane: HistoryLane::Automated, is_default_session: false, is_concurrent: true,
        });

        // Freeze committed conversational text only. Never inherit unresolved
        // tool calls, provider continuation tokens or per-turn secret handles.
        let mut history = Vec::new();
        if context.internal_voice.is_some() {
            // Branch history contains its inherited snapshot too. Carry that
            // text forward, but never a dangling user/tool/provider exchange.
            let entries = self.get_llm_history_tail(context_id, 128).await?;
            let last = entries.iter().rposition(|entry| matches!(entry,
                ChatLlmTranscriptEntry::AssistantTurn { text: Some(_), tool_calls, .. } if tool_calls.is_empty()));
            if let Some(last) = last {
                for entry in &entries[..=last] {
                    match entry {
                        ChatLlmTranscriptEntry::UserText { text } => {
                            history.push(ChatLlmTranscriptEntry::UserText { text: text.clone() })
                        },
                        ChatLlmTranscriptEntry::UserTurn { content } => {
                            let text = content
                                .iter()
                                .filter_map(|block| match block {
                                    TranscriptBlock::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            if !text.is_empty() {
                                history.push(ChatLlmTranscriptEntry::UserText { text });
                            }
                        },
                        ChatLlmTranscriptEntry::AssistantTurn {
                            text: Some(text),
                            tool_calls,
                            ..
                        } if tool_calls.is_empty() => {
                            history.push(ChatLlmTranscriptEntry::AssistantTurn {
                                text: Some(text.clone()),
                                tool_calls: Vec::new(),
                                provider_state: None,
                            })
                        },
                        _ => {},
                    }
                }
            }
            // Async tasks finish after the chat acknowledgement. Their final
            // answer is a canonical task-status message, not necessarily an
            // LLM transcript entry. Carry each execution's latest finalized
            // result into the addressed follow-up, including its task identity
            // so tools can reopen the original outputs when needed.
            let mut seen = std::collections::HashSet::new();
            let mut task_results = Vec::new();
            for message in self.get_messages(context_id, 128).await?.into_iter().rev() {
                if let ChatMessageContent::TaskStatusUpdate {
                    task_id,
                    execution_id,
                    status,
                    display_label,
                    summary,
                    synthesis_pending,
                    speech_tts,
                    ..
                } = message.content
                {
                    if !seen.insert((task_id.clone(), execution_id))
                        || synthesis_pending
                        || !matches!(status.as_str(), "completed" | "failed" | "cancelled")
                    {
                        continue;
                    }
                    task_results.push(ChatLlmTranscriptEntry::AssistantTurn {
                        text: Some(format!(
                            "Task {} ({task_id}) {status}: {}",
                            display_label.as_deref().unwrap_or("Background task"),
                            summary
                                .as_deref()
                                .or(speech_tts.as_deref())
                                .unwrap_or(&status),
                        )),
                        tool_calls: Vec::new(),
                        provider_state: None,
                    });
                }
            }
            history.extend(task_results.into_iter().rev());
        } else {
            let messages = self.get_context_messages(context_id, 128).await?;
            let answered: std::collections::HashSet<_> = messages
                .iter()
                .filter(|m| {
                    m.direction == ChatMessageDirection::Assistant
                        && m.content.text_content().is_some()
                })
                .filter_map(|m| m.chat_turn_id.as_deref())
                .collect();
            let last = messages
                .iter()
                .rposition(|m| m.direction == ChatMessageDirection::Assistant);
            if let Some(last) = last {
                for message in &messages[..=last] {
                    if message.direction == ChatMessageDirection::User
                        && message
                            .chat_turn_id
                            .as_deref()
                            .is_some_and(|id| !answered.contains(id))
                    {
                        continue;
                    }
                    let Some(text) = message.content.text_content() else {
                        continue;
                    };
                    match message.direction {
                        ChatMessageDirection::User => {
                            history.push(ChatLlmTranscriptEntry::UserText {
                                text: text.to_owned(),
                            })
                        },
                        ChatMessageDirection::Assistant => {
                            history.push(ChatLlmTranscriptEntry::AssistantTurn {
                                text: Some(text.to_owned()),
                                tool_calls: Vec::new(),
                                provider_state: None,
                            })
                        },
                        _ => {},
                    }
                }
            }
        }
        let mut remaining = 24_000;
        let mut bounded = Vec::new();
        for mut entry in history.into_iter().rev() {
            let text = match &mut entry {
                ChatLlmTranscriptEntry::UserText { text } => text,
                ChatLlmTranscriptEntry::AssistantTurn {
                    text: Some(text), ..
                } => text,
                _ => continue,
            };
            if remaining == 0 {
                break;
            }
            *text = text.chars().take(remaining.min(6000)).collect();
            remaining -= text.chars().count();
            bounded.push(entry);
        }
        bounded.reverse();
        if !bounded.is_empty() {
            self.append_llm_history_entries(&branch_id, bounded).await?;
        }

        let request = VoiceRequest {
            id: id.clone(),
            submission_id: admission.submission_id,
            fingerprint,
            parent_session_id: parent.id.clone(),
            branch_session_id: branch_id.clone(),
            ui_thread_id: Some(parent.ui_thread_id.clone()),
            context_session_id: Some(context_id.to_owned()),
            chat_turn_id: id.clone(),
            title,
            source_surface: admission.source_surface.clone(),
            presence_session_id: admission.presence_session_id.clone(),
            executor_id: admission.executor_id,
            work_status: VoiceWorkStatus::Accepted,
            delivery_status: VoiceDeliveryStatus::Waiting,
            read_at: None,
            result_message_id: None,
            task_result_created_at: None,
            result_projected: false,
            pending_tasks: vec![],
            notified_task_results: vec![],
            task_notification: false,
            speech_text: None,
            error: None,
            attempt: None,
            created_at: now,
            updated_at: now,
        };
        self.append_message(
            parent_session_id,
            ChatMessage::new(
                format!("{id}-user"),
                parent_session_id,
                ChatMessageDirection::User,
                ChatMessageContent::Text {
                    text: admission.text.trim().to_string(),
                    plan_reply: None,
                },
                now,
            )
            .with_context_origin(ChatMessageContextOrigin {
                ui_thread_id: parent.ui_thread_id.clone(),
                session_id: branch_id,
                request_id: id.clone(),
                message_id: None,
                result_created_at: None,
            })
            .with_chat_turn_id(Some(id))
            .with_voice_origin(Some(true))
            .with_source_surface(Some(admission.source_surface))
            .with_presence_session_id(admission.presence_session_id),
        )
        .await?;
        self.name_voice_parent_if_untitled(parent_session_id, &concurrent_task_title(&admission.text)).await?;
        state.insert(request.clone())?;
        doc.session.internal_voice = Some(InternalVoiceSession::Coordinator { state });
        self.save_document(&doc).await?;
        Ok((request, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admission(id: &str) -> VoiceAdmission {
        VoiceAdmission {
            submission_id: id.into(),
            text: format!("Question {id}"),
            context_session_id: None,
            profile: None,
            mode: crate::magician_v2::chat::models::ChatMessageMode::Ask,
            coding_choice: None,
            source_surface: "web".into(),
            presence_session_id: None,
            executor_id: "process".into(),
        }
    }

    #[tokio::test]
    async fn concurrent_voice_final_result_refreshes_one_saved_projection_without_reannouncement() {
        let root = tempfile::tempdir().unwrap();
        let store = FileChatStore::with_index(root.path()).await.unwrap();
        let parent = store.new_session("alice", "default", "general", &ChatChannel::web(), "agent").await.unwrap();
        let (work, _) = store.admit_voice_request(&parent.id, admission("article")).await.unwrap();
        let branch = store.get_session(&work.branch_session_id).await.unwrap().unwrap();
        let task_result = |at, summary: &str| ChatMessage::new(
            format!("answer-{at}"), &branch.id, ChatMessageDirection::System,
            ChatMessageContent::TaskStatusUpdate {
                task_id: "task-article".into(), execution_id: Some("execution-one".into()),
                status: "completed".into(), display_label: None, summary: Some(summary.into()),
                synthesis_pending: false, ui_thread_id: Some("general".into()),
                output_files: vec![], speech_tts: None,
            }, at,
        );
        let projection = |request: &VoiceRequest, at| ChatMessage::new(
            format!("{}-result", request.id), &parent.id, ChatMessageDirection::Assistant,
            ChatMessageContent::Text { text: request.speech_text.clone().unwrap(), plan_reply: None }, at,
        ).with_chat_turn_id(Some(request.chat_turn_id.clone()))
            .with_context_origin(ChatMessageContextOrigin {
                ui_thread_id: "general".into(), session_id: branch.id.clone(),
                request_id: request.id.clone(), message_id: request.result_message_id.clone(),
                result_created_at: Some(at),
            });
        store.append_message(&branch.id, task_result(10, "Execution completed.")).await.unwrap();
        let first = store.voice_state("alice", "default").await.unwrap().requests.into_iter().find(|r| r.task_notification).unwrap();
        assert!(first.title.starts_with("#con1-Question article"), "missing labels use the actual topic");
        assert!(store.upsert_voice_result_projection(&parent, &first, projection(&first, 10)).await.unwrap());
        assert!(!store.upsert_voice_result_projection(&parent, &first, projection(&first, 10)).await.unwrap());
        store.mutate_voice_state("alice", "default", VoiceMutation::Read { request_id: first.id.clone() }).await.unwrap();
        let read = store.voice_state("alice", "default").await.unwrap().requests.into_iter().find(|r| r.id == first.id).unwrap();

        // Roll over the tail: the refresh must replace the original segment,
        // not append another copy or silently drop the update as a duplicate.
        for n in 0..=CHAT_MESSAGE_SEGMENT_SIZE {
            store.append_message(&parent.id, ChatMessage::new(format!("filler-{n}"), &parent.id,
                ChatMessageDirection::User, ChatMessageContent::Text { text: "Later message".into(), plan_reply: None }, 20 + n as i64)).await.unwrap();
        }
        store.append_message(&branch.id, task_result(30, "The full article is ready.")).await.unwrap();
        let final_result = store.voice_state("alice", "default").await.unwrap().requests.into_iter().find(|r| r.id == first.id).unwrap();
        assert!(!final_result.result_projected);
        assert_eq!(final_result.read_at, read.read_at);
        assert_eq!(final_result.delivery_status, read.delivery_status);
        assert!(store.upsert_voice_result_projection(&parent, &final_result, projection(&final_result, 30)).await.unwrap());
        assert!(!store.upsert_voice_result_projection(&parent, &first, projection(&first, 10)).await.unwrap(), "stale worker cannot regress the final answer");
        // A replay from the asynchronous event sink also cannot duplicate it.
        store.append_message(&parent.id, projection(&first, 10)).await.unwrap();
        let restored = FileChatStore::with_index(root.path()).await.unwrap();
        let saved = restored.get_messages(&parent.id, CHAT_MESSAGE_SEGMENT_SIZE + 10).await.unwrap();
        let copies: Vec<_> = saved.iter().filter(|m| m.id == format!("{}-result", first.id)).collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].created_at, 10, "keep its position in the conversation");
        assert!(copies[0].content.text_content().unwrap().contains("full article"));
        assert_eq!(copies[0].context_origin.as_ref().unwrap().message_id.as_deref(), Some("answer-30"));
        let state = restored.voice_state("alice", "default").await.unwrap();
        let notice = state.requests.iter().find(|r| r.id == first.id).unwrap();
        assert!(notice.result_projected);
        assert_eq!(notice.read_at, read.read_at);
        assert_eq!(state.requests.iter().filter(|r| r.task_notification).count(), 1);
    }

    #[tokio::test]
    async fn concurrent_voice_from_linked_branch_keeps_viewed_context_and_retry_identity() {
        let root = tempfile::tempdir().unwrap();
        let store = FileChatStore::with_index(root.path()).await.unwrap();
        let parent = store.new_session("alice", "default", "general", &ChatChannel::web(), "agent").await.unwrap();
        let (first, _) = store.admit_voice_request(&parent.id, admission("first")).await.unwrap();
        let (followup, fresh) = store.admit_voice_request(&first.branch_session_id, admission("followup")).await.unwrap();
        assert!(fresh);
        assert_eq!(followup.parent_session_id, first.branch_session_id);
        assert_eq!(followup.context_session_id.as_deref(), Some(first.branch_session_id.as_str()));
        assert_ne!(followup.branch_session_id, first.branch_session_id);
        // Admission writes the projected follow-up here; the first canonical
        // user turn is appended later by execution, which this storage test
        // deliberately does not start.
        let displayed = store.get_messages(&first.branch_session_id, 10).await.unwrap();
        assert_eq!(displayed.len(), 1);
        assert_eq!(displayed[0].context_origin.as_ref().unwrap().request_id, followup.id);
        assert_eq!(store.get_messages(&parent.id, 10).await.unwrap().len(), 1);

        drop(store);
        let store = FileChatStore::with_index(root.path()).await.unwrap();
        let (again, fresh) = store.admit_voice_request(&first.branch_session_id, admission("followup")).await.unwrap();
        assert!(!fresh);
        assert_eq!(again.id, followup.id);
        let unrelated = store.new_session("alice", "default", "other", &ChatChannel::web(), "agent").await.unwrap();
        let mut invalid = admission("invalid");
        invalid.context_session_id = Some(unrelated.id);
        assert!(store.admit_voice_request(&first.branch_session_id, invalid).await.unwrap_err().to_string().contains("parent_mismatch"));
        let coordinator = coordinator_id("alice", "default");
        assert!(store.admit_voice_request(&coordinator, admission("coordinator")).await.unwrap_err().to_string().contains("parent_not_active"));
    }

    #[tokio::test]
    async fn concurrent_voice_admission_persists_deduplicates_and_hides_internal_sessions() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(FileChatStore::with_index(root.path()).await.unwrap());
        let parent = store
            .new_session("alice", "default", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();
        let (a, b) = tokio::join!(
            store.admit_voice_request(&parent.id, admission("a")),
            store.admit_voice_request(&parent.id, admission("b"))
        );
        let (a, fresh) = a.unwrap();
        let (b, _) = b.unwrap();
        assert!(fresh);
        assert_ne!(a.branch_session_id, b.branch_session_id);
        let mut branch_titles = Vec::new();
        for request in [&a, &b] {
            let branch = store.get_session(&request.branch_session_id).await.unwrap().unwrap();
            let title = branch.title.unwrap();
            assert!(title.ends_with(&request.title));
            branch_titles.push(title);
        }
        branch_titles.sort();
        assert!(branch_titles[0].starts_with("#con1-"));
        assert!(branch_titles[1].starts_with("#con2-"));
        assert_eq!(
            store.get_session(&parent.id).await.unwrap().unwrap().title.as_ref(),
            Some(&branch_titles[0].split_once('-').unwrap().1.to_string())
        );
        let (again, fresh) = store
            .admit_voice_request(&parent.id, admission("a"))
            .await
            .unwrap();
        assert!(!fresh);
        assert_eq!(again.id, a.id);
        assert_eq!(store.get_messages(&parent.id, 10).await.unwrap().len(), 2);
        assert_eq!(
            store.list_sessions("alice", "default").await.unwrap().len(),
            3
        );
        // Old branches inherited Personal. Provenance reclassifies them on
        // reload without relying on a title prefix or touching their parent.
        let mut old = store.load_document(&a.branch_session_id).await.unwrap();
        old.session.history_lane = HistoryLane::Personal;
        store.save_document(&old).await.unwrap();
        let restored = FileChatStore::with_index(root.path()).await.unwrap();
        assert_eq!(
            restored
                .voice_state("alice", "default")
                .await
                .unwrap()
                .requests
                .len(),
            2
        );
        assert!(restored
            .voice_state("bob", "default")
            .await
            .unwrap()
            .requests
            .is_empty());
        assert_eq!(
            restored
                .list_sessions("alice", "default")
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(restored
            .get_session(&a.branch_session_id)
            .await
            .unwrap()
            .is_some());
        let listed = restored.list_sessions("alice", "default").await.unwrap();
        assert_eq!(listed.iter().filter(|s| s.effective_history_lane() == HistoryLane::Automated).count(), 2);
        assert_eq!(restored.get_session(&parent.id).await.unwrap().unwrap().effective_history_lane(), HistoryLane::Personal);
        assert_eq!(restored.get_or_create_active_session("alice", "default", "general", &ChatChannel::web(), "agent").await.unwrap().id, parent.id);
        // Retries don't allocate another number; reopening the store preserves
        // the allocator, and a user rename wins over subsequent voice requests.
        restored.update_session_title(&parent.id, "My planning").await.unwrap();
        let mut next = admission("after-restart");
        next.text = format!("  Plan\n\t{}  ", "旅行".repeat(40));
        let (next, _) = restored.admit_voice_request(&parent.id, next).await.unwrap();
        let named = restored.get_session(&next.branch_session_id).await.unwrap().unwrap();
        let title = named.title.unwrap();
        assert!(title.starts_with("#con3-Plan "));
        assert!(!title.contains('\n'));
        assert!(title.ends_with('…'));
        assert!(title.len() <= 256);
        assert_eq!(restored.get_session(&parent.id).await.unwrap().unwrap().title.as_deref(), Some("My planning"));
        let mut state = restored.voice_state("alice", "default").await.unwrap();
        for request in &mut state.requests {
            request.work_status = VoiceWorkStatus::Completed;
            request.read_at = Some(1);
            request.updated_at = 1;
        }
        state.prune_consumed(3_600_002);
        assert!(state.requests.is_empty());
        assert_eq!(state.session_sequence, 3);
    }

    #[tokio::test]
    async fn voice_requests_projection_history_is_display_only_even_after_reload() {
        let root = tempfile::tempdir().unwrap();
        let store = FileChatStore::with_index(root.path()).await.unwrap();
        let parent = store
            .new_session("alice", "default", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();
        for (id, direction, text) in [
            (
                "main-user",
                ChatMessageDirection::User,
                "Main thread marker COBALT",
            ),
            (
                "main-answer",
                ChatMessageDirection::Assistant,
                "Main answer COBALT",
            ),
        ] {
            store
                .append_message(
                    &parent.id,
                    ChatMessage::new(
                        id,
                        &parent.id,
                        direction,
                        ChatMessageContent::Text {
                            text: text.into(),
                            plan_reply: None,
                        },
                        1,
                    )
                    .with_chat_turn_id(Some("main-turn".into())),
                )
                .await
                .unwrap();
        }
        // More than a page of old-style projected turns must neither enter
        // context nor crowd out real messages when the context limit is small.
        for index in 0..70 {
            let turn = format!("voice-request-sibling-{index}");
            for (suffix, direction) in [
                ("user", ChatMessageDirection::User),
                ("result", ChatMessageDirection::Assistant),
            ] {
                let mut message = ChatMessage::new(
                    format!("{turn}-{suffix}"),
                    &parent.id,
                    direction,
                    ChatMessageContent::Text {
                        text: "Unrelated sibling ORCHID".into(),
                        plan_reply: None,
                    },
                    2,
                )
                .with_chat_turn_id(Some(turn.clone()));
                if index % 2 == 0 {
                    message.context_origin =
                        Some(crate::magician_v2::chat::models::ChatMessageContextOrigin {
                            ui_thread_id: "general".into(),
                            session_id: format!("branch-{index}"),
                            request_id: turn.clone(),
                            message_id: None,
                            result_created_at: None,
                        });
                }
                store.append_message(&parent.id, message).await.unwrap();
            }
        }
        store
            .append_message(
                &parent.id,
                ChatMessage::new(
                    "voice-task-result-legacy-result",
                    &parent.id,
                    ChatMessageDirection::Assistant,
                    ChatMessageContent::Text {
                        text: "Unrelated task ORCHID".into(),
                        plan_reply: None,
                    },
                    2,
                )
                .with_chat_turn_id(Some("voice-request-sibling-0".into())),
            )
            .await
            .unwrap();
        let context = store.get_context_messages(&parent.id, 2).await.unwrap();
        assert_eq!(
            context.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["main-user", "main-answer"]
        );
        for history in [
            store.get_llm_history(&parent.id).await.unwrap(),
            store.get_llm_history_tail(&parent.id, 2).await.unwrap(),
        ] {
            let text = serde_json::to_string(&history).unwrap();
            assert!(text.contains("COBALT"));
            assert!(!text.contains("ORCHID"));
        }
        let (request, _) = store
            .admit_voice_request(&parent.id, admission("isolated"))
            .await
            .unwrap();
        assert_eq!(request.ui_thread_id.as_deref(), Some("general"));
        assert_eq!(
            request.context_session_id.as_deref(),
            Some(parent.id.as_str())
        );
        let text = serde_json::to_string(
            &store
                .get_llm_history(&request.branch_session_id)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(text.contains("COBALT"));
        assert!(!text.contains("ORCHID"));
        let projected = store
            .get_messages(&parent.id, 1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert!(projected.is_context_projection());
        let origin = projected.context_origin.unwrap();
        assert_eq!(origin.session_id, request.branch_session_id);
        assert_eq!(origin.request_id, request.id);
        assert_eq!(origin.ui_thread_id, "general");
        // Real branch messages keep the same turn correlation without being projections.
        let canonical = ChatMessage::new(
            "canonical-answer",
            &request.branch_session_id,
            ChatMessageDirection::Assistant,
            ChatMessageContent::Text {
                text: "Canonical TOPAZ".into(),
                plan_reply: None,
            },
            3,
        )
        .with_chat_turn_id(Some(request.chat_turn_id.clone()));
        assert!(!canonical.is_context_projection());
        drop(store);
        let restored = FileChatStore::with_index(root.path()).await.unwrap();
        let restored_request = restored
            .voice_state("alice", "default")
            .await
            .unwrap()
            .requests
            .remove(0);
        assert_eq!(
            restored_request.context_session_id,
            request.context_session_id
        );
        let text =
            serde_json::to_string(&restored.get_llm_history(&parent.id).await.unwrap()).unwrap();
        assert!(!text.contains("ORCHID"));
    }

    #[tokio::test]
    async fn voice_context_cannot_cross_parent_or_identity() {
        let root = tempfile::tempdir().unwrap();
        let store = FileChatStore::with_index(root.path()).await.unwrap();
        let a = store
            .new_session("alice", "default", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();
        let b = store
            .new_session("bob", "default", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();
        let mut input = admission("a");
        input.context_session_id = Some(b.id);
        assert!(store.admit_voice_request(&a.id, input).await.is_err());
        assert!(store
            .voice_state("alice", "default")
            .await
            .unwrap()
            .requests
            .is_empty());
    }
    #[tokio::test]
    async fn voice_snapshot_excludes_unanswered_siblings_and_keeps_inherited_followup_context() {
        let root = tempfile::tempdir().unwrap();
        let store = FileChatStore::with_index(root.path()).await.unwrap();
        let parent = store
            .new_session("alice", "default", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();
        for (id, direction, text, turn) in [
            (
                "u1",
                ChatMessageDirection::User,
                "Remember the code cobalt",
                "one",
            ),
            (
                "u2",
                ChatMessageDirection::User,
                "Pending sibling secret",
                "two",
            ),
            (
                "a1",
                ChatMessageDirection::Assistant,
                "The code is cobalt",
                "one",
            ),
        ] {
            store
                .append_message(
                    &parent.id,
                    ChatMessage::new(
                        id,
                        &parent.id,
                        direction,
                        ChatMessageContent::Text {
                            text: text.into(),
                            plan_reply: None,
                        },
                        1,
                    )
                    .with_chat_turn_id(Some(turn.into())),
                )
                .await
                .unwrap();
        }
        let (first, _) = store
            .admit_voice_request(&parent.id, admission("first"))
            .await
            .unwrap();
        let history = store
            .get_llm_history(&first.branch_session_id)
            .await
            .unwrap();
        let encoded = serde_json::to_string(&history).unwrap();
        assert!(encoded.contains("cobalt"));
        assert!(!encoded.contains("Pending sibling"));
        store
            .append_llm_history_entries(
                &first.branch_session_id,
                vec![
                    ChatLlmTranscriptEntry::UserTurn {
                        content: vec![TranscriptBlock::Text {
                            text: "What colour is that? Context code ORCHID77.".into(),
                        }],
                    },
                    ChatLlmTranscriptEntry::AssistantTurn {
                        text: Some("Blue".into()),
                        tool_calls: vec![],
                        provider_state: None,
                    },
                ],
            )
            .await
            .unwrap();
        let mut next = admission("next");
        next.context_session_id = Some(first.branch_session_id.clone());
        let (next, _) = store.admit_voice_request(&parent.id, next).await.unwrap();
        let encoded = serde_json::to_string(
            &store
                .get_llm_history(&next.branch_session_id)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(encoded.contains("cobalt") && encoded.contains("Blue"));
        assert!(encoded.contains("ORCHID77"));
        assert!(store
            .update_session_status(&first.branch_session_id, "archived")
            .await
            .is_err());
        assert!(store
            .delete_session(&first.branch_session_id)
            .await
            .is_err());
    }
    #[tokio::test]
    async fn voice_requests_task_receipt_survives_restart_and_waits_for_synthesis() {
        let root = tempfile::tempdir().unwrap();
        let store = FileChatStore::with_index(root.path()).await.unwrap();
        let parent = store
            .new_session("alice", "default", "general", &ChatChannel::web(), "agent")
            .await
            .unwrap();
        let (request, _) = store
            .admit_voice_request(&parent.id, admission("task-origin"))
            .await
            .unwrap();
        let card = |id: &str, pending| {
            ChatMessage::new(
                id,
                &request.branch_session_id,
                ChatMessageDirection::System,
                ChatMessageContent::TaskStatusUpdate {
                    task_id: "task-one".into(),
                    status: "completed".into(),
                    display_label: Some("Research".into()),
                    summary: Some("Actual task result".into()),
                    execution_id: Some("execution-one".into()),
                    ui_thread_id: None,
                    output_files: vec![],
                    synthesis_pending: pending,
                    speech_tts: None,
                },
                3,
            )
            .with_chat_turn_id(Some(request.chat_turn_id.clone()))
        };
        store
            .append_message(&request.branch_session_id, card("pending", true))
            .await
            .unwrap();
        let state = store.voice_state("alice", "default").await.unwrap();
        assert_eq!(state.requests.len(), 1);
        assert_eq!(state.requests[0].pending_tasks, vec!["task-one"]);
        let mut pending_followup = admission("pending-task-followup");
        pending_followup.context_session_id = Some(request.branch_session_id.clone());
        let (pending_followup, _) = store
            .admit_voice_request(&parent.id, pending_followup)
            .await
            .unwrap();
        assert!(store
            .get_llm_history_tail(&pending_followup.branch_session_id, 128)
            .await
            .unwrap()
            .is_empty());
        store
            .append_message(&request.branch_session_id, card("finished", false))
            .await
            .unwrap();
        store
            .append_message(&request.branch_session_id, card("finished-replayed", false))
            .await
            .unwrap();
        let restarted = FileChatStore::with_index(root.path()).await.unwrap();
        let state = restarted.voice_state("alice", "default").await.unwrap();
        assert_eq!(state.requests.len(), 3);
        assert!(state.requests[0].pending_tasks.is_empty());
        let notice = state.requests.iter().find(|r| r.task_notification).unwrap();
        assert_eq!(notice.delivery_status, VoiceDeliveryStatus::Pending);
        assert_eq!(notice.result_message_id.as_deref(), Some("finished"));
        let mut followup = admission("finished-task-followup");
        followup.context_session_id = Some(request.branch_session_id.clone());
        let (followup, _) = restarted
            .admit_voice_request(&parent.id, followup)
            .await
            .unwrap();
        let history = restarted
            .get_llm_history_tail(&followup.branch_session_id, 128)
            .await
            .unwrap();
        let results: Vec<_> = history
            .iter()
            .filter_map(|entry| match entry {
                ChatLlmTranscriptEntry::AssistantTurn {
                    text: Some(text), ..
                } if text.contains("Actual task result") => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 1);
        assert!(results[0].contains("task-one"));
    }
}
