//! Envoy's controlled outbound replies share the Claims Review register.
//! Preparing a candidate never says it was sent. Only the authenticated channel
//! bot may acknowledge provider acceptance; a crash leaves a visible unknown.
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::models::{ChatMessage, ChatMessageContent, ChatMessageDirection, ChatSession};
use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    evidence::outward_assertions::{OutwardActStatus, OutwardChannel, PrepareOutwardAct},
    evidence::{OutwardAssertionStore, OutwardScope, TranscriptIngestion},
    execution::file_edit::transaction::acquire_record_decision_lock,
};

fn key(session: &str, message: &str) -> String {
    format!(
        "envoy:{}",
        blake3::hash(format!("{session}\u{1f}{message}").as_bytes()).to_hex()
    )
}

/// Only channels whose adapters use the receipt protocol participate. A Gmail
/// address is a reply-message target, not a resolved email recipient.
pub fn channel_for_session(session: &ChatSession) -> Option<OutwardChannel> {
    match session.origin_channel.channel_type.as_str() {
        "kapso" | "whatsapp" => Some(OutwardChannel::WhatsApp),
        "agentmail" | "email" | "gmail" => Some(OutwardChannel::Email),
        "telegram" | "telegram-self" => Some(OutwardChannel::Room),
        _ => None,
    }
}

pub fn prepare_reply(
    layout: &ArtifactV2Workspace,
    session: &ChatSession,
    message: &ChatMessage,
    envoy_id: &str,
) -> Result<()> {
    if session.agent_id != envoy_id
        || message.direction != ChatMessageDirection::Assistant
        || message.session_id != session.id
    {
        return Ok(());
    }
    let Some(channel) = channel_for_session(session) else {
        return Ok(());
    };
    let Some(address) = session
        .origin_channel
        .address
        .as_deref()
        .filter(|a| !a.trim().is_empty())
    else {
        return Ok(());
    };
    // Source-surface text is caller supplied. The persisted session binding,
    // rather than that text, decides whether this is an Envoy reply.
    let ChatMessageContent::Text { text, .. } = &message.content else {
        return Ok(());
    };
    // The receipt-aware SDK sends canonical content for tracked replies. A
    // presentation sidecar is never a second source of outbound words.
    if text.trim().is_empty() {
        return Ok(());
    }
    let scope = OutwardScope::new(&session.principal, &session.workspace);
    let store = OutwardAssertionStore::new(layout.clone());
    let payload = store.store_payload(&scope, text.as_bytes())?;
    let intended_audience = vec![format!("{}:{address}", session.origin_channel.channel_type)];
    let act = store.prepare(
        &scope,
        &PrepareOutwardAct {
            idempotency_key: key(&session.id, &message.id),
            program_id: None,
            engagement_id: None,
            exact_payload_artifact_ref: payload.clone(),
            effective_sender: envoy_id.to_owned(),
            intended_audience: intended_audience.clone(),
            channel: channel.clone(),
            consequence_class: "bounded_communication".into(),
        },
        &Utc::now().to_rfc3339(),
    )?;
    anyhow::ensure!(
        act.exact_payload_artifact_ref == payload
            && act.effective_sender == envoy_id
            && act.intended_audience == intended_audience
            && act.channel == channel
            && !act.observed,
        "Envoy message or channel binding changed after preparation"
    );
    TranscriptIngestion::new(layout.clone()).queue_controlled_reply(
        &scope,
        &act.outward_act_ref,
        &format!("envoy:{}", session.id),
        &message.id,
        text,
        "envoy-reply-capture",
        chrono::DateTime::from_timestamp_millis(message.created_at)
            .context("invalid reply timestamp")?,
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPhase {
    Begin,
    ProviderAccepted,
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct DeliveryGrant {
    pub tracked: bool,
    pub send: bool,
    pub status: String,
}

/// Caller assertions are compared with the host's prepared payload and target.
/// They never select the authoritative recipient, sender or scope.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeliveryBinding {
    pub attempt_id: String,
    pub channel_type: String,
    pub channel_address: String,
    pub payload_sha256: String,
}

pub fn has_prepared_reply(
    layout: &ArtifactV2Workspace,
    session: &ChatSession,
    message_id: &str,
) -> Result<bool> {
    let scope = OutwardScope::new(&session.principal, &session.workspace);
    let act_ref = crate::magician_v2::evidence::outward_assertions::derive_act_ref(
        &scope,
        &key(&session.id, message_id),
    );
    Ok(OutwardAssertionStore::new(layout.clone())
        .load_act(&scope, &act_ref)?
        .is_some())
}

pub fn report_delivery(
    layout: &ArtifactV2Workspace,
    session: &ChatSession,
    message_id: &str,
    phase: DeliveryPhase,
    binding: &DeliveryBinding,
) -> Result<DeliveryGrant> {
    uuid::Uuid::parse_str(&binding.attempt_id).context("invalid delivery attempt id")?;
    let scope = OutwardScope::new(&session.principal, &session.workspace);
    let root = layout
        .scope_root(&scope.principal, &scope.workspace)
        .join("envoy_delivery_locks");
    let lock_key = key(&session.id, message_id).replace(':', "-");
    let _guard = acquire_record_decision_lock(&root, &lock_key, "Envoy delivery")?;
    let store = OutwardAssertionStore::new(layout.clone());
    let act_ref = crate::magician_v2::evidence::outward_assertions::derive_act_ref(
        &scope,
        &key(&session.id, message_id),
    );
    let act = store
        .load_act(&scope, &act_ref)?
        .context("Envoy reply was not prepared; refusing untracked delivery")?;
    let payload = store
        .load_payload(&scope, &act.exact_payload_artifact_ref)?
        .context("prepared Envoy payload is missing")?;
    anyhow::ensure!(
        !act.observed
            && act.effective_sender == session.agent_id
            && Some(act.channel) == channel_for_session(session)
            && binding.channel_type == session.origin_channel.channel_type
            && Some(binding.channel_address.as_str()) == session.origin_channel.address.as_deref()
            && act.intended_audience
                == vec![format!(
                    "{}:{}",
                    binding.channel_type, binding.channel_address
                )]
            && binding.payload_sha256 == format!("{:x}", Sha256::digest(payload.as_bytes())),
        "Envoy delivery differs from the prepared text or channel binding"
    );
    anyhow::ensure!(
        TranscriptIngestion::new(layout.clone())
            .controlled_reply_claim(
                &scope,
                &act_ref,
                &format!("envoy:{}", session.id),
                message_id
            )?
            .is_some(),
        "Envoy claim preparation is incomplete; refusing delivery"
    );

    // Store the attempt before granting dispatch. A lost Begin response can
    // resume ONLY the same in-process attempt; another bot/restart gets no send
    // grant. Once a send is uncertain or accepted, even that attempt cannot send
    // again. The SDK never reuses this grant after entering its adapter.
    let attempt_path = root.join(format!("{lock_key}.json"));
    let held = crate::magician_v2::jsonl::read_log_if_present(layout, &attempt_path)?
        .map(|raw| serde_json::from_str::<DeliveryBinding>(&raw))
        .transpose()?;
    let owns_attempt = held.as_ref() == Some(binding);
    let now = Utc::now().to_rfc3339();
    let send = match phase {
        DeliveryPhase::Begin if act.status == OutwardActStatus::Prepared => {
            layout.write_atomic_path_sync(&attempt_path, &serde_json::to_vec(binding)?)?;
            store.mark_dispatching(&scope, &act_ref, &now)?;
            true
        },
        DeliveryPhase::Begin => owns_attempt && act.status == OutwardActStatus::Dispatching,
        DeliveryPhase::ProviderAccepted | DeliveryPhase::Unknown => {
            anyhow::ensure!(owns_attempt, "delivery receipt does not own this attempt");
            match phase {
                DeliveryPhase::ProviderAccepted
                    if matches!(
                        act.status,
                        OutwardActStatus::Dispatching | OutwardActStatus::DispatchUnknown
                    ) =>
                {
                    store.record_provider_receipt(
                        &scope,
                        &act_ref,
                        &format!(
                            "channel-adapter:{}:{message_id}",
                            session.origin_channel.channel_type
                        ),
                        &now,
                    )?;
                },
                DeliveryPhase::Unknown if act.status == OutwardActStatus::Dispatching => {
                    store.mark_dispatch_unknown(
                        &scope,
                        &act_ref,
                        "channel send did not return an acceptance receipt",
                        &now,
                    )?;
                },
                DeliveryPhase::ProviderAccepted if act.status == OutwardActStatus::Prepared => {
                    anyhow::bail!("delivery must begin before acceptance")
                },
                _ => {},
            }
            false
        },
    };
    let status = store
        .load_act(&scope, &act_ref)?
        .context("Envoy act disappeared")?
        .status
        .as_str()
        .to_owned();
    Ok(DeliveryGrant {
        tracked: true,
        send,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::{
        chat::models::{ChatChannel, ChatSessionStatus},
        evidence::OwnerDecision,
        history::HistoryLane,
    };

    fn fixture() -> (
        tempfile::TempDir,
        ArtifactV2Workspace,
        ChatSession,
        ChatMessage,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let session = ChatSession {
            internal_voice: None,
            id: "session-1".into(),
            principal: "alice".into(),
            workspace: "work".into(),
            agent_id: "envoy".into(),
            ui_thread_id: "ext:telegram:42".into(),
            title: None,
            origin_channel: ChatChannel::new("telegram", "42"),
            status: ChatSessionStatus::Active,
            history_lane: HistoryLane::Personal,
            is_default_session: false,
            created_at: 1000,
            updated_at: 1000,
        };
        let message = ChatMessage::new(
            "message-1",
            "session-1",
            ChatMessageDirection::Assistant,
            ChatMessageContent::Text {
                text: "Our office opens at nine.".into(),
                plan_reply: None,
            },
            1000,
        );
        (tmp, layout, session, message)
    }

    fn binding() -> DeliveryBinding {
        DeliveryBinding {
            attempt_id: "00000000-0000-4000-8000-000000000001".into(),
            channel_type: "telegram".into(),
            channel_address: "42".into(),
            payload_sha256: format!("{:x}", Sha256::digest(b"Our office opens at nine.")),
        }
    }

    #[test]
    fn envoy_claims_require_a_receipt_and_a_separate_human_decision() {
        let (_tmp, layout, session, message) = fixture();
        prepare_reply(&layout, &session, &message, "envoy").unwrap();
        prepare_reply(&layout, &session, &message, "envoy").unwrap();
        let scope = OutwardScope::new("alice", "work");
        let ingestion = TranscriptIngestion::new(layout.clone());
        let claims = ingestion.pending_claims(&scope).unwrap();
        assert_eq!(claims.len(), 1);
        let claim = &claims[0];
        assert_eq!(claim.audience, vec!["telegram:42"]);
        assert!(claim.assertion_use_ids.is_empty());
        let owner = OwnerDecision::by("Alice");
        assert!(ingestion
            .confirm_claim_at_revision(&scope, &claim.claim_id, 1, "decision-1", &owner, Utc::now())
            .is_err());
        assert!(report_delivery(
            &layout,
            &session,
            &message.id,
            DeliveryPhase::ProviderAccepted,
            &binding()
        )
        .is_err());
        assert!(
            report_delivery(
                &layout,
                &session,
                &message.id,
                DeliveryPhase::Begin,
                &binding()
            )
            .unwrap()
            .send
        );
        assert!(
            report_delivery(
                &layout,
                &session,
                &message.id,
                DeliveryPhase::Begin,
                &binding()
            )
            .unwrap()
            .send
        );
        report_delivery(
            &layout,
            &session,
            &message.id,
            DeliveryPhase::Unknown,
            &binding(),
        )
        .unwrap();
        assert!(ingestion
            .confirm_claim(&scope, &claim.claim_id, &owner, Utc::now())
            .is_err());
        let receipt = report_delivery(
            &layout,
            &session,
            &message.id,
            DeliveryPhase::ProviderAccepted,
            &binding(),
        )
        .unwrap();
        assert_eq!(receipt.status, "provider_accepted");
        assert!(
            !report_delivery(
                &layout,
                &session,
                &message.id,
                DeliveryPhase::Begin,
                &binding()
            )
            .unwrap()
            .send
        );
        assert!(ingestion
            .confirm_claim(
                &scope,
                &claim.claim_id,
                &OwnerDecision::by("envoy-reply-capture"),
                Utc::now()
            )
            .is_err());
        let outcome = ingestion
            .confirm_claim_at_revision(&scope, &claim.claim_id, 1, "decision-1", &owner, Utc::now())
            .unwrap();
        assert!(outcome.claim.is_on_the_record());
        assert_eq!(outcome.claim.assertion_use_ids.len(), 1);
        assert!(ingestion
            .confirm_claim_at_revision(&scope, &claim.claim_id, 1, "decision-1", &owner, Utc::now())
            .is_ok());
    }

    #[test]
    fn envoy_claims_exclude_guest_words_owner_chat_and_internal_surfaces() {
        for variant in ["user", "owner", "web", "other-session"] {
            let (_tmp, layout, mut session, mut message) = fixture();
            match variant {
                "user" => message.direction = ChatMessageDirection::User,
                "owner" => session.agent_id = "personal-assistant".into(),
                "web" => session.origin_channel = ChatChannel::web(),
                _ => message.session_id = "other-session".into(),
            }
            prepare_reply(&layout, &session, &message, "envoy").unwrap();
            assert!(TranscriptIngestion::new(layout)
                .claims(&OutwardScope::new("alice", "work"))
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn envoy_claims_refuse_changed_words_and_cross_scope_receipts() {
        let (_tmp, layout, mut session, mut message) = fixture();
        prepare_reply(&layout, &session, &message, "envoy").unwrap();
        message.presentation = None;
        message.content = ChatMessageContent::Text {
            text: "Changed words".into(),
            plan_reply: None,
        };
        assert!(prepare_reply(&layout, &session, &message, "envoy").is_err());
        let original_address = session.origin_channel.address.clone();
        session.origin_channel.address = Some("different-recipient".into());
        assert!(prepare_reply(&layout, &session, &fixture().3, "envoy").is_err());
        assert!(report_delivery(
            &layout,
            &session,
            &message.id,
            DeliveryPhase::Begin,
            &binding()
        )
        .is_err());
        session.origin_channel.address = original_address;
        session.workspace = "elsewhere".into();
        assert!(report_delivery(
            &layout,
            &session,
            &message.id,
            DeliveryPhase::Begin,
            &binding()
        )
        .is_err());
    }

    #[test]
    fn envoy_claims_resume_only_the_same_unsent_attempt_after_a_lost_response() {
        let (tmp, layout, session, message) = fixture();
        prepare_reply(&layout, &session, &message, "envoy").unwrap();
        let first = binding();
        let mut second = binding();
        second.attempt_id = uuid::Uuid::new_v4().to_string();
        assert!(
            report_delivery(&layout, &session, &message.id, DeliveryPhase::Begin, &first)
                .unwrap()
                .send
        );
        let reopened = ArtifactV2Workspace::new(tmp.path());
        assert!(
            report_delivery(
                &reopened,
                &session,
                &message.id,
                DeliveryPhase::Begin,
                &first
            )
            .unwrap()
            .send
        );
        assert!(
            !report_delivery(
                &reopened,
                &session,
                &message.id,
                DeliveryPhase::Begin,
                &second
            )
            .unwrap()
            .send
        );
        assert!(report_delivery(
            &reopened,
            &session,
            &message.id,
            DeliveryPhase::ProviderAccepted,
            &second
        )
        .is_err());
        report_delivery(
            &reopened,
            &session,
            &message.id,
            DeliveryPhase::Unknown,
            &first,
        )
        .unwrap();
        assert!(
            !report_delivery(
                &reopened,
                &session,
                &message.id,
                DeliveryPhase::Begin,
                &first
            )
            .unwrap()
            .send
        );
        report_delivery(
            &reopened,
            &session,
            &message.id,
            DeliveryPhase::ProviderAccepted,
            &first,
        )
        .unwrap();
        assert_eq!(
            report_delivery(
                &reopened,
                &session,
                &message.id,
                DeliveryPhase::ProviderAccepted,
                &first
            )
            .unwrap()
            .status,
            "provider_accepted"
        );
    }

    #[test]
    fn envoy_claims_bind_dispatch_to_exact_adapter_text_and_recipient() {
        let (_tmp, layout, session, message) = fixture();
        prepare_reply(&layout, &session, &message, "envoy").unwrap();
        for field in ["text", "recipient", "channel", "attempt"] {
            let mut wrong = binding();
            match field {
                "text" => {
                    wrong.payload_sha256 = format!("{:x}", Sha256::digest(b"Different words"))
                },
                "recipient" => wrong.channel_address = "43".into(),
                "channel" => wrong.channel_type = "telegram-self".into(),
                _ => wrong.attempt_id = "not-an-attempt".into(),
            }
            assert!(
                report_delivery(&layout, &session, &message.id, DeliveryPhase::Begin, &wrong)
                    .is_err(),
                "{field}"
            );
        }
        assert!(
            report_delivery(
                &layout,
                &session,
                &message.id,
                DeliveryPhase::Begin,
                &binding()
            )
            .unwrap()
            .send
        );
    }

    #[test]
    fn envoy_claims_never_dispatch_a_partially_prepared_reply() {
        let (_tmp, layout, session, mut message) = fixture();
        // Invalid capture time fails after the act is prepared, before its claim
        // is queued. A delivery retry must not mistake that act for completion.
        message.created_at = i64::MAX;
        assert!(prepare_reply(&layout, &session, &message, "envoy").is_err());
        assert!(has_prepared_reply(&layout, &session, &message.id).unwrap());
        assert!(report_delivery(
            &layout,
            &session,
            &message.id,
            DeliveryPhase::Begin,
            &binding()
        )
        .is_err());
        message.created_at = 1000;
        prepare_reply(&layout, &session, &message, "envoy").unwrap();
        assert!(
            report_delivery(
                &layout,
                &session,
                &message.id,
                DeliveryPhase::Begin,
                &binding()
            )
            .unwrap()
            .send
        );
    }
}
