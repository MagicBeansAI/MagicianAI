//! Durable work and speech are separate state machines. This reducer is shared
//! by dictation and realtime; neither microphone nor provider lifetime owns work.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const MAX_VOICE_OUTSTANDING: usize = 32;
pub const MAX_VOICE_RECORDS: usize = 96;
pub const VOICE_OUTPUT_LEASE_MS: i64 = 30_000;
pub const VOICE_ATTEMPT_LEASE_MS: i64 = 30_000;

/// Trusted ingress supplies surface and executor identity. The caller can
/// address a prior context; it cannot supply another scope or agent identity.
#[derive(Debug, Clone)]
pub struct VoiceAdmission {
    pub submission_id: String,
    pub text: String,
    pub context_session_id: Option<String>,
    pub profile: Option<String>,
    pub mode: super::models::ChatMessageMode,
    pub coding_choice: Option<crate::magician_v2::vibedev::dispatch_intent::VibeDevCodingChoice>,
    pub source_surface: String,
    pub presence_session_id: Option<String>,
    pub executor_id: String,
}

/// Stored inside the governed chat metadata owner. Execution branches appear
/// in Automated history; the coordinator is addressable internally and hidden.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InternalVoiceSession {
    Branch { parent_session_id: String },
    Coordinator { state: VoiceCoordinatorState },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceCoordinatorState {
    pub revision: u64,
    /// Durable workspace-wide name allocation; never reset by receipt pruning.
    #[serde(default)]
    pub session_sequence: u64,
    #[serde(default)]
    pub requests: Vec<VoiceRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<VoiceOutputLease>,
    #[serde(default)]
    pub output_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceOutputLease {
    pub device_id: String,
    pub interaction_id: String,
    pub epoch: u64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceWorkStatus {
    Accepted,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl VoiceWorkStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Accepted | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceDeliveryStatus {
    Waiting,
    Pending,
    Claimed,
    Playing,
    Played,
    Deferred,
    Uncertain,
    Dismissed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoicePlaybackAttempt {
    pub id: String,
    pub device_id: String,
    pub interaction_id: String,
    pub output_epoch: u64,
    pub focus_epoch: u64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceRequest {
    pub id: String,
    pub submission_id: String,
    pub fingerprint: String,
    pub parent_session_id: String,
    pub branch_session_id: String,
    /// Resolved on the server from the addressed session, never caller identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_thread_id: Option<String>,
    /// The exact conversation whose committed context seeded this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_session_id: Option<String>,
    pub chat_turn_id: String,
    pub title: String,
    pub source_surface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_session_id: Option<String>,
    pub executor_id: String,
    pub work_status: VoiceWorkStatus,
    pub delivery_status: VoiceDeliveryStatus,
    /// Acknowledges a visibly opened result independently of audio playback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_result_created_at: Option<i64>,
    #[serde(default)]
    pub result_projected: bool,
    #[serde(default)]
    pub pending_tasks: Vec<String>,
    #[serde(default)]
    pub notified_task_results: Vec<String>,
    #[serde(default)]
    pub task_notification: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<VoicePlaybackAttempt>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub enum VoiceMutation {
    Start {
        request_id: String,
        executor_id: String,
    },
    Finish {
        request_id: String,
        executor_id: String,
        status: VoiceWorkStatus,
        result_message_id: Option<String>,
        speech_text: Option<String>,
        error: Option<String>,
    },
    TaskUpdate {
        branch_session_id: String,
        task_key: String,
        notification_id: String,
        message_id: String,
        message_created_at: i64,
        title: String,
        terminal_status: Option<VoiceWorkStatus>,
        speech_text: String,
    },
    Projected {
        request_id: String,
    },
    Cancel {
        request_id: String,
    },
    Recover {
        executor_id: String,
    },
    AcquireOutput {
        device_id: String,
        interaction_id: String,
    },
    ReleaseOutput {
        device_id: String,
        interaction_id: String,
        epoch: u64,
    },
    Claim {
        request_id: String,
        device_id: String,
        interaction_id: String,
        epoch: u64,
        focus_epoch: u64,
        attempt_id: String,
        replay: bool,
    },
    Playback {
        request_id: String,
        device_id: String,
        interaction_id: String,
        epoch: u64,
        attempt_id: String,
        event: VoicePlaybackEvent,
    },
    Read {
        request_id: String,
    },
    Dismiss {
        request_id: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoicePlaybackEvent {
    Started,
    Progress,
    Completed,
    Interrupted,
    Rejected,
}

impl VoiceCoordinatorState {
    pub fn request(&self, id: &str) -> Result<&VoiceRequest> {
        self.requests
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("voice_request_not_found"))
    }

    fn request_mut(&mut self, id: &str) -> Result<&mut VoiceRequest> {
        self.requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("voice_request_not_found"))
    }

    pub fn check_submission(
        &self,
        submission_id: &str,
        fingerprint: &str,
    ) -> Result<Option<VoiceRequest>> {
        if let Some(existing) = self
            .requests
            .iter()
            .find(|r| r.submission_id == submission_id)
        {
            if existing.fingerprint != fingerprint {
                bail!("voice_submission_conflict");
            }
            return Ok(Some(existing.clone()));
        }
        Ok(None)
    }

    pub fn check_capacity(&self) -> Result<()> {
        if self
            .requests
            .iter()
            .filter(|r| r.work_status.is_active() || !r.pending_tasks.is_empty())
            .count()
            >= MAX_VOICE_OUTSTANDING
        {
            bail!("voice_capacity_exceeded");
        }
        if self.requests.len() >= MAX_VOICE_RECORDS {
            bail!("voice_delivery_backlog_full");
        }
        Ok(())
    }

    /// Retain accepted work and undelivered answers. Only old terminal records
    /// whose presentation has been consumed can leave this bounded index; their
    /// canonical branch conversations remain stored.
    pub fn prune_consumed(&mut self, now: i64) {
        self.requests.retain(|r| {
            r.work_status.is_active()
                || !r.pending_tasks.is_empty()
                || (r.read_at.is_none()
                    && !matches!(
                        r.delivery_status,
                        VoiceDeliveryStatus::Played | VoiceDeliveryStatus::Dismissed
                    ))
                || now.saturating_sub(r.updated_at) < 60 * 60 * 1000
        });
    }

    pub fn insert(&mut self, request: VoiceRequest) -> Result<()> {
        self.check_capacity()?;
        if self
            .requests
            .iter()
            .any(|r| r.id == request.id || r.submission_id == request.submission_id)
        {
            bail!("voice_submission_conflict");
        }
        if self
            .requests
            .iter()
            .any(|r| r.branch_session_id == request.branch_session_id && r.work_status.is_active())
        {
            bail!("voice_branch_busy");
        }
        self.requests.push(request);
        self.revision += 1;
        Ok(())
    }

    fn require_output(&self, device: &str, interaction: &str, epoch: u64, now: i64) -> Result<()> {
        if !self.output.as_ref().is_some_and(|o| {
            o.device_id == device
                && o.interaction_id == interaction
                && o.epoch == epoch
                && o.expires_at > now
        }) {
            bail!("voice_output_lease_stale");
        }
        Ok(())
    }

    fn expire_attempts(&mut self, now: i64) {
        for r in &mut self.requests {
            if r.attempt.as_ref().is_some_and(|a| a.expires_at <= now) {
                match r.delivery_status {
                    VoiceDeliveryStatus::Claimed => {
                        r.delivery_status = VoiceDeliveryStatus::Pending
                    },
                    VoiceDeliveryStatus::Playing => {
                        r.delivery_status = VoiceDeliveryStatus::Uncertain
                    },
                    _ => continue,
                }
                r.updated_at = now;
            }
        }
    }

    /// Apply to a clone before committing. A rejected transition has no durable
    /// effects, including lease expiration discovered while validating it.
    pub fn apply(&mut self, mutation: VoiceMutation, now: i64) -> Result<()> {
        let mut next = self.clone();
        next.apply_inner(mutation, now)?;
        next.revision = self.revision.saturating_add(1);
        *self = next;
        Ok(())
    }

    fn apply_inner(&mut self, mutation: VoiceMutation, now: i64) -> Result<()> {
        self.expire_attempts(now);
        match mutation {
            VoiceMutation::Start {
                request_id,
                executor_id,
            } => {
                let r = self.request_mut(&request_id)?;
                if r.executor_id != executor_id || r.work_status != VoiceWorkStatus::Accepted {
                    bail!("voice_execution_stale");
                }
                r.work_status = VoiceWorkStatus::Running;
                r.updated_at = now;
            },
            VoiceMutation::Finish {
                request_id,
                executor_id,
                status,
                result_message_id,
                speech_text,
                error,
            } => {
                if !matches!(
                    status,
                    VoiceWorkStatus::Completed
                        | VoiceWorkStatus::Failed
                        | VoiceWorkStatus::Cancelled
                        | VoiceWorkStatus::Interrupted
                ) {
                    bail!("invalid_voice_terminal_status");
                }
                let r = self.request_mut(&request_id)?;
                if r.executor_id != executor_id {
                    bail!("voice_execution_stale");
                }
                // Explicit cancellation wins over a late model/tool callback.
                if !r.work_status.is_active() {
                    return Ok(());
                }
                r.work_status = status;
                r.result_message_id = result_message_id;
                r.speech_text = speech_text.map(|s| s.chars().take(1600).collect());
                r.error = error.map(|s| s.chars().take(400).collect());
                r.delivery_status = if r.speech_text.is_some() {
                    VoiceDeliveryStatus::Pending
                } else {
                    VoiceDeliveryStatus::Dismissed
                };
                r.updated_at = now;
            },
            VoiceMutation::TaskUpdate {
                branch_session_id,
                task_key,
                notification_id,
                message_id,
                message_created_at,
                title,
                terminal_status,
                speech_text,
            } => {
                let Some(index) = self
                    .requests
                    .iter()
                    .position(|r| r.branch_session_id == branch_session_id && !r.task_notification)
                else {
                    return Ok(());
                };
                if self.requests[index]
                    .notified_task_results
                    .contains(&task_key)
                {
                    // Task completion and its synthesized answer can arrive in
                    // separate messages. Refresh the same receipt, preserving
                    // any in-flight or consumed playback attempt.
                    if let Some(status) = terminal_status {
                        if let Some(notice) =
                            self.requests.iter_mut().find(|r| r.id == notification_id)
                        {
                            let title: String = title.chars().take(120).collect();
                            let speech_text: String = speech_text.chars().take(1600).collect();
                            if notice
                                .task_result_created_at
                                .is_none_or(|at| message_created_at > at
                                    || (message_created_at == at && notice.result_message_id.as_deref() == Some(&message_id)))
                                && (notice.result_message_id.as_deref() != Some(&message_id)
                                    || notice.title != title
                                    || notice.speech_text.as_deref() != Some(&speech_text)
                                    || notice.work_status != status)
                            {
                                notice.result_message_id = Some(message_id);
                                notice.task_result_created_at = Some(message_created_at);
                                notice.title = title;
                                notice.work_status = status;
                                notice.speech_text = Some(speech_text);
                                notice.result_projected = false;
                                notice.updated_at = now;
                            }
                        }
                    }
                    return Ok(());
                }
                let task_id = task_key.split(':').next().unwrap_or(&task_key).to_string();
                if !self.requests[index].pending_tasks.contains(&task_id) {
                    self.requests[index].pending_tasks.push(task_id.clone());
                }
                let Some(status) = terminal_status else {
                    return Ok(());
                };
                if self.requests[index].work_status == VoiceWorkStatus::Cancelled {
                    self.requests[index]
                        .pending_tasks
                        .retain(|key| key != &task_id);
                    self.requests[index].notified_task_results.push(task_key);
                    return Ok(());
                }
                // Keep pending on capacity pressure. Read-time reconciliation
                // retries the canonical task result after space is available.
                if self.requests.len() >= MAX_VOICE_RECORDS {
                    return Ok(());
                }
                let mut notice = self.requests[index].clone();
                self.requests[index]
                    .pending_tasks
                    .retain(|key| key != &task_id);
                self.requests[index].notified_task_results.push(task_key);
                notice.id = notification_id.clone();
                notice.submission_id = notification_id;
                notice.task_notification = true;
                notice.pending_tasks.clear();
                notice.notified_task_results.clear();
                notice.title = title.chars().take(120).collect();
                notice.work_status = status;
                notice.delivery_status = VoiceDeliveryStatus::Pending;
                notice.read_at = None;
                notice.result_message_id = Some(message_id);
                notice.task_result_created_at = Some(message_created_at);
                notice.result_projected = false;
                notice.speech_text = Some(speech_text.chars().take(1600).collect());
                notice.error = None;
                notice.attempt = None;
                notice.created_at = now;
                notice.updated_at = now;
                self.requests.push(notice);
            },
            VoiceMutation::Projected { request_id } => {
                self.request_mut(&request_id)?.result_projected = true;
            },
            VoiceMutation::Cancel { request_id } => {
                let r = self.request_mut(&request_id)?;
                if r.work_status.is_active() || !r.pending_tasks.is_empty() {
                    r.work_status = VoiceWorkStatus::Cancelled;
                    r.delivery_status = VoiceDeliveryStatus::Dismissed;
                    r.updated_at = now;
                }
            },
            VoiceMutation::Recover { executor_id } => {
                for r in &mut self.requests {
                    if r.executor_id != executor_id && r.work_status.is_active() {
                        r.work_status = VoiceWorkStatus::Interrupted;
                        r.error = Some("The service restarted before this request finished. Review its context before retrying.".into());
                        r.delivery_status = VoiceDeliveryStatus::Deferred;
                        r.updated_at = now;
                    }
                }
            },
            VoiceMutation::AcquireOutput {
                device_id,
                interaction_id,
            } => {
                validate_voice_key(&device_id)?;
                validate_voice_key(&interaction_id)?;
                if let Some(output) = self.output.as_mut().filter(|o| o.expires_at > now) {
                    if output.device_id != device_id || output.interaction_id != interaction_id {
                        bail!("voice_output_owned_elsewhere");
                    }
                    output.expires_at = now + VOICE_OUTPUT_LEASE_MS;
                } else {
                    self.output_epoch = self.output_epoch.saturating_add(1);
                    self.output = Some(VoiceOutputLease {
                        device_id,
                        interaction_id,
                        epoch: self.output_epoch,
                        expires_at: now + VOICE_OUTPUT_LEASE_MS,
                    });
                }
            },
            VoiceMutation::ReleaseOutput {
                device_id,
                interaction_id,
                epoch,
            } => {
                self.require_output(&device_id, &interaction_id, epoch, now)?;
                self.output = None;
                for r in &mut self.requests {
                    if r.attempt.as_ref().is_some_and(|a| a.output_epoch == epoch) {
                        r.delivery_status = match r.delivery_status {
                            VoiceDeliveryStatus::Claimed => VoiceDeliveryStatus::Pending,
                            VoiceDeliveryStatus::Playing => VoiceDeliveryStatus::Uncertain,
                            other => other,
                        };
                    }
                }
            },
            VoiceMutation::Claim {
                request_id,
                device_id,
                interaction_id,
                epoch,
                focus_epoch,
                attempt_id,
                replay,
            } => {
                self.require_output(&device_id, &interaction_id, epoch, now)?;
                validate_voice_key(&attempt_id)?;
                if self.requests.iter().any(|r| {
                    matches!(
                        r.delivery_status,
                        VoiceDeliveryStatus::Claimed | VoiceDeliveryStatus::Playing
                    ) && !(r.id == request_id
                        && r.attempt.as_ref().is_some_and(|a| a.id == attempt_id))
                }) {
                    bail!("voice_output_busy");
                }
                let r = self.request_mut(&request_id)?;
                if r.attempt
                    .as_ref()
                    .is_some_and(|a| a.id == attempt_id && a.output_epoch == epoch)
                    && matches!(
                        r.delivery_status,
                        VoiceDeliveryStatus::Claimed | VoiceDeliveryStatus::Playing
                    )
                {
                    return Ok(());
                }
                if r.work_status.is_active() || r.speech_text.is_none() {
                    bail!("voice_result_not_ready");
                }
                if (r.delivery_status != VoiceDeliveryStatus::Pending || r.read_at.is_some())
                    && !replay
                {
                    bail!("voice_delivery_requires_explicit_replay");
                }
                r.attempt = Some(VoicePlaybackAttempt {
                    id: attempt_id,
                    device_id,
                    interaction_id,
                    output_epoch: epoch,
                    focus_epoch,
                    expires_at: now + VOICE_ATTEMPT_LEASE_MS,
                });
                r.delivery_status = VoiceDeliveryStatus::Claimed;
                r.updated_at = now;
            },
            VoiceMutation::Playback {
                request_id,
                device_id,
                interaction_id,
                epoch,
                attempt_id,
                event,
            } => {
                self.require_output(&device_id, &interaction_id, epoch, now)?;
                let r = self.request_mut(&request_id)?;
                let a = r
                    .attempt
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("voice_playback_attempt_missing"))?;
                if a.id != attempt_id
                    || a.device_id != device_id
                    || a.interaction_id != interaction_id
                    || a.output_epoch != epoch
                {
                    bail!("voice_playback_attempt_stale");
                }
                // Repeated terminal receipts are harmless. A stale Started must
                // never put completed playback back on the floor.
                if matches!(
                    r.delivery_status,
                    VoiceDeliveryStatus::Played | VoiceDeliveryStatus::Deferred
                ) {
                    return Ok(());
                }
                if !matches!(
                    r.delivery_status,
                    VoiceDeliveryStatus::Claimed | VoiceDeliveryStatus::Playing
                ) {
                    bail!("voice_playback_attempt_expired");
                }
                r.delivery_status = match event {
                    VoicePlaybackEvent::Started => VoiceDeliveryStatus::Playing,
                    VoicePlaybackEvent::Progress => r.delivery_status,
                    VoicePlaybackEvent::Completed
                        if r.delivery_status == VoiceDeliveryStatus::Playing =>
                    {
                        VoiceDeliveryStatus::Played
                    },
                    VoicePlaybackEvent::Completed => bail!("voice_playback_not_started"),
                    VoicePlaybackEvent::Interrupted => VoiceDeliveryStatus::Deferred,
                    VoicePlaybackEvent::Rejected
                        if r.delivery_status == VoiceDeliveryStatus::Claimed =>
                    {
                        VoiceDeliveryStatus::Pending
                    },
                    VoicePlaybackEvent::Rejected => VoiceDeliveryStatus::Uncertain,
                };
                a.expires_at = now + VOICE_ATTEMPT_LEASE_MS;
                r.updated_at = now;
            },
            VoiceMutation::Read { request_id } => {
                let r = self.request_mut(&request_id)?;
                if r.result_message_id.is_none() {
                    bail!("voice_result_not_ready");
                }
                // Do not forge audio completion or disturb an in-flight receipt.
                r.read_at.get_or_insert(now);
                r.updated_at = now;
            },
            VoiceMutation::Dismiss { request_id } => {
                let r = self.request_mut(&request_id)?;
                if r.work_status.is_active() {
                    bail!("voice_work_still_active");
                }
                if matches!(
                    r.delivery_status,
                    VoiceDeliveryStatus::Claimed | VoiceDeliveryStatus::Playing
                ) {
                    bail!("voice_output_busy");
                }
                r.delivery_status = VoiceDeliveryStatus::Dismissed;
                r.updated_at = now;
            },
        }
        Ok(())
    }
}

pub fn validate_voice_key(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    {
        bail!("invalid_voice_identifier");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str) -> VoiceRequest {
        VoiceRequest {
            id: id.into(),
            submission_id: id.into(),
            fingerprint: id.into(),
            parent_session_id: "parent".into(),
            branch_session_id: format!("branch-{id}"),
            ui_thread_id: Some("general".into()),
            context_session_id: Some("parent".into()),
            chat_turn_id: id.into(),
            title: id.into(),
            source_surface: "web".into(),
            presence_session_id: None,
            executor_id: "process-a".into(),
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
            created_at: 1,
            updated_at: 1,
        }
    }
    #[test]
    fn voice_requests_read_receipt_preserves_work_and_requires_explicit_replay() {
        let mut state = VoiceCoordinatorState::default();
        state.insert(request("a")).unwrap();
        assert!(state
            .apply(
                VoiceMutation::Read {
                    request_id: "a".into()
                },
                2
            )
            .is_err());
        state.apply(finish("a"), 3).unwrap();
        state
            .apply(
                VoiceMutation::Read {
                    request_id: "a".into(),
                },
                4,
            )
            .unwrap();
        assert_eq!(state.request("a").unwrap().read_at, Some(4));
        assert_eq!(
            state.request("a").unwrap().work_status,
            VoiceWorkStatus::Completed
        );
        assert_eq!(
            state.request("a").unwrap().delivery_status,
            VoiceDeliveryStatus::Pending
        );
        state.apply(acquire(), 5).unwrap();
        assert!(state.apply(claim("a"), 6).is_err());
        let mut replay = claim("a");
        if let VoiceMutation::Claim { replay, .. } = &mut replay {
            *replay = true;
        }
        state.apply(replay, 7).unwrap();
        state
            .apply(
                VoiceMutation::Read {
                    request_id: "a".into(),
                },
                8,
            )
            .unwrap();
        assert_eq!(
            state.request("a").unwrap().delivery_status,
            VoiceDeliveryStatus::Claimed
        );
        assert_eq!(state.request("a").unwrap().read_at, Some(4));
    }

    fn finish(id: &str) -> VoiceMutation {
        VoiceMutation::Finish {
            request_id: id.into(),
            executor_id: "process-a".into(),
            status: VoiceWorkStatus::Completed,
            result_message_id: Some(format!("reply-{id}")),
            speech_text: Some(format!("Answer {id}")),
            error: None,
        }
    }
    fn acquire() -> VoiceMutation {
        VoiceMutation::AcquireOutput {
            device_id: "device".into(),
            interaction_id: "call".into(),
        }
    }
    fn claim(id: &str) -> VoiceMutation {
        VoiceMutation::Claim {
            request_id: id.into(),
            device_id: "device".into(),
            interaction_id: "call".into(),
            epoch: 1,
            focus_epoch: 7,
            attempt_id: format!("attempt-{id}"),
            replay: false,
        }
    }
    fn playback(id: &str, event: VoicePlaybackEvent) -> VoiceMutation {
        VoiceMutation::Playback {
            request_id: id.into(),
            device_id: "device".into(),
            interaction_id: "call".into(),
            epoch: 1,
            attempt_id: format!("attempt-{id}"),
            event,
        }
    }

    #[test]
    fn concurrent_results_have_one_speaker_and_independent_cancellation() {
        let mut s = VoiceCoordinatorState::default();
        for id in ["a", "b", "c"] {
            s.insert(request(id)).unwrap();
        }
        s.apply(
            VoiceMutation::Cancel {
                request_id: "b".into(),
            },
            2,
        )
        .unwrap();
        s.apply(finish("c"), 3).unwrap();
        s.apply(finish("a"), 4).unwrap();
        s.apply(finish("b"), 5).unwrap();
        assert_eq!(
            s.request("b").unwrap().work_status,
            VoiceWorkStatus::Cancelled
        );
        s.apply(acquire(), 6).unwrap();
        s.apply(claim("c"), 7).unwrap();
        assert!(s.apply(claim("a"), 8).is_err());
        assert!(s
            .apply(playback("c", VoicePlaybackEvent::Completed), 9)
            .is_err());
        s.apply(playback("c", VoicePlaybackEvent::Started), 10)
            .unwrap();
        s.apply(playback("c", VoicePlaybackEvent::Completed), 11)
            .unwrap();
        s.apply(playback("c", VoicePlaybackEvent::Started), 12)
            .unwrap();
        assert_eq!(
            s.request("c").unwrap().delivery_status,
            VoiceDeliveryStatus::Played
        );
        s.apply(claim("a"), 13).unwrap();
    }

    #[test]
    fn expired_playback_is_uncertain_and_cannot_automatically_repeat() {
        let mut s = VoiceCoordinatorState::default();
        s.insert(request("a")).unwrap();
        s.apply(finish("a"), 2).unwrap();
        s.apply(acquire(), 3).unwrap();
        s.apply(claim("a"), 4).unwrap();
        s.apply(playback("a", VoicePlaybackEvent::Started), 5)
            .unwrap();
        s.apply(acquire(), 40_000).unwrap();
        assert_eq!(
            s.request("a").unwrap().delivery_status,
            VoiceDeliveryStatus::Uncertain
        );
        assert!(s
            .apply(playback("a", VoicePlaybackEvent::Completed), 40_001)
            .is_err());
        let mut next_claim = claim("a");
        if let VoiceMutation::Claim { ref mut epoch, .. } = next_claim {
            *epoch = 2;
        }
        assert!(s.apply(next_claim, 40_002).is_err());
    }

    #[test]
    fn barge_in_defers_presentation_without_cancelling_work() {
        let mut s = VoiceCoordinatorState::default();
        s.insert(request("a")).unwrap();
        s.insert(request("b")).unwrap();
        s.apply(finish("a"), 2).unwrap();
        s.apply(acquire(), 3).unwrap();
        s.apply(claim("a"), 4).unwrap();
        s.apply(playback("a", VoicePlaybackEvent::Started), 5)
            .unwrap();
        s.apply(playback("a", VoicePlaybackEvent::Interrupted), 6)
            .unwrap();
        assert_eq!(
            s.request("a").unwrap().work_status,
            VoiceWorkStatus::Completed
        );
        assert_eq!(
            s.request("a").unwrap().delivery_status,
            VoiceDeliveryStatus::Deferred
        );
        assert!(s.request("b").unwrap().work_status.is_active());
    }

    #[test]
    fn capacity_never_drops_accepted_work_and_submission_keys_bind_payloads() {
        let mut s = VoiceCoordinatorState::default();
        for n in 0..MAX_VOICE_OUTSTANDING {
            s.insert(request(&n.to_string())).unwrap();
        }
        assert!(s.insert(request("overflow")).is_err());
        assert_eq!(s.requests.len(), MAX_VOICE_OUTSTANDING);
        assert!(s.check_submission("0", "0").unwrap().is_some());
        assert!(s.check_submission("0", "different").is_err());
        assert!(s
            .apply(
                VoiceMutation::AcquireOutput {
                    device_id: "phone".into(),
                    interaction_id: "call".into()
                },
                2
            )
            .is_ok());
        assert!(s.apply(acquire(), 3).is_err());
    }

    #[test]
    fn recovery_reports_interruption_without_replaying_effects() {
        let mut s = VoiceCoordinatorState::default();
        s.insert(request("a")).unwrap();
        s.apply(
            VoiceMutation::Recover {
                executor_id: "process-b".into(),
            },
            10,
        )
        .unwrap();
        assert_eq!(
            s.request("a").unwrap().work_status,
            VoiceWorkStatus::Interrupted
        );
        assert_eq!(
            s.request("a").unwrap().delivery_status,
            VoiceDeliveryStatus::Deferred
        );
    }
    #[test]
    fn voice_requests_task_results_are_durable_deduplicated_and_do_not_steal_an_attempt() {
        let mut state = VoiceCoordinatorState::default();
        state.insert(request("a")).unwrap();
        let update = |status| VoiceMutation::TaskUpdate {
            branch_session_id: "branch-a".into(),
            task_key: "task:execution".into(),
            notification_id: "task-result".into(),
            message_id: "task-message".into(),
            message_created_at: 7,
            title: "Research".into(),
            terminal_status: status,
            speech_text: "Research finished".into(),
        };
        state.apply(update(None), 2).unwrap();
        assert_eq!(state.request("a").unwrap().pending_tasks, vec!["task"]);
        state.apply(finish("a"), 3).unwrap();
        state.apply(acquire(), 4).unwrap();
        state.apply(claim("a"), 5).unwrap();
        state
            .apply(playback("a", VoicePlaybackEvent::Started), 6)
            .unwrap();
        state
            .apply(update(Some(VoiceWorkStatus::Completed)), 7)
            .unwrap();
        state
            .apply(update(Some(VoiceWorkStatus::Completed)), 8)
            .unwrap();
        assert_eq!(state.requests.len(), 2);
        assert!(state.request("a").unwrap().pending_tasks.is_empty());
        assert_eq!(
            state.request("a").unwrap().delivery_status,
            VoiceDeliveryStatus::Playing
        );
        assert_eq!(
            state.request("task-result").unwrap().delivery_status,
            VoiceDeliveryStatus::Pending
        );
        assert_eq!(
            state.request("task-result").unwrap().branch_session_id,
            "branch-a"
        );
        let richer = |at, text: &str| VoiceMutation::TaskUpdate {
            branch_session_id: "branch-a".into(),
            task_key: "task:execution".into(),
            notification_id: "task-result".into(),
            message_id: format!("task-message-{at}"),
            message_created_at: at,
            title: "Research".into(),
            terminal_status: Some(VoiceWorkStatus::Completed),
            speech_text: text.into(),
        };
        state.apply(richer(9, "The actual answer"), 9).unwrap();
        assert_eq!(
            state
                .request("task-result")
                .unwrap()
                .result_message_id
                .as_deref(),
            Some("task-message-9")
        );
        state
            .apply(playback("a", VoicePlaybackEvent::Completed), 10)
            .unwrap();
        state.apply(claim("task-result"), 11).unwrap();
        state
            .apply(playback("task-result", VoicePlaybackEvent::Started), 12)
            .unwrap();
        state.apply(richer(13, "Final synthesis"), 13).unwrap();
        assert_eq!(
            state.request("task-result").unwrap().delivery_status,
            VoiceDeliveryStatus::Playing
        );
        assert_eq!(
            state
                .request("task-result")
                .unwrap()
                .attempt
                .as_ref()
                .unwrap()
                .id,
            "attempt-task-result"
        );
        state
            .apply(playback("task-result", VoicePlaybackEvent::Completed), 14)
            .unwrap();
        state.apply(richer(15, "Final artifact"), 15).unwrap();
        state.apply(richer(9, "Stale answer"), 16).unwrap();
        assert_eq!(state.requests.len(), 2);
        assert_eq!(
            state.request("task-result").unwrap().delivery_status,
            VoiceDeliveryStatus::Played
        );
        assert_eq!(
            state.request("task-result").unwrap().speech_text.as_deref(),
            Some("Final artifact")
        );
        assert_eq!(
            state
                .request("task-result")
                .unwrap()
                .result_message_id
                .as_deref(),
            Some("task-message-15")
        );
    }
}
