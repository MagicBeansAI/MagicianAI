use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::types::{
    ContentCandidate, ContentPrivacy, ContentProvenance, SourceIdentity,
    CONTENT_SOURCE_SCHEMA_VERSION,
};
use crate::magician_v2::channel_types::{DistillState, MailMessageMeta};

/// Project a locally distilled channel message into the neutral selection
/// contract used by user-defined feeds. Suppressed or undistilled messages do
/// not project. Raw channel content is never read or copied here.
pub fn candidate_from_channel_message(message: &MailMessageMeta) -> Option<ContentCandidate> {
    if message.sensitive_suppressed || message.distill_state != DistillState::Done {
        return None;
    }

    let summary = message
        .distill_brief
        .as_ref()
        .map(|brief| brief.summary.trim())
        .filter(|summary| !summary.is_empty())
        .or_else(|| {
            message
                .summary
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })?;

    let title = message
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            message
                .from_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("Channel message")
        .to_string();

    let adapter_id = format!("channel:{}", message.provider.trim().to_ascii_lowercase());
    let mut metadata = BTreeMap::<String, Value>::new();
    metadata.insert("provider".into(), json!(message.provider));
    metadata.insert("account_alias".into(), json!(message.account_alias));
    metadata.insert("thread_id".into(), json!(message.thread_id));
    if let Some(intent) = message.intent.as_deref() {
        metadata.insert("intent".into(), json!(intent));
    }
    if let Some(sender) = message.from_name.as_deref() {
        metadata.insert("sender_name".into(), json!(sender));
    }
    if let Some(address) = message.from_address.as_deref() {
        metadata.insert("sender_address".into(), json!(address));
    }
    if let Some(brief) = message.distill_brief.as_ref() {
        metadata.insert("information_type".into(), json!(brief.information_type));
        metadata.insert("key_facts".into(), json!(brief.key_facts));
        metadata.insert("changes".into(), json!(brief.changes));
        metadata.insert("temporal_facts".into(), json!(brief.temporal_facts));
        if let Some(action) = brief.stated_action.as_deref() {
            metadata.insert("stated_action".into(), json!(action));
        }
    }

    let hash_input = format!(
        "{}\n{}\n{}\n{}",
        message.provider,
        message.message_id,
        message.distill_revision.unwrap_or_default(),
        summary
    );

    let source_item_id = format!("{}/{}", message.account_alias, message.message_id);
    metadata.insert("source_message_id".into(), json!(message.message_id));

    let candidate = ContentCandidate {
        schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
        identity: SourceIdentity::new(adapter_id.clone(), source_item_id).ok()?,
        title,
        cheap_text: summary.to_string(),
        canonical_url: None,
        published_at_ms: Some(message.internal_date),
        observed_at_ms: message.observed_at,
        // Owned communications remain private even when the sensitivity
        // detector did not suppress them.
        privacy: ContentPrivacy::Private,
        content_hash: Some(blake3::hash(hash_input.as_bytes()).to_hex().to_string()),
        provenance: ContentProvenance {
            source_label: message.provider.clone(),
            source_url: None,
            retrieved_by: adapter_id,
        },
        metadata,
    };
    candidate.validate().ok()?;
    Some(candidate)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::channel_types::{
        ChannelDetailStatus, ChannelInformationBrief, ChannelInformationType, DistillState,
        MailRecordOrigin, MessageDirection,
    };
    const MAIL_ASSIST_SCHEMA_VERSION: u32 = 9;

    fn message(suppressed: bool) -> MailMessageMeta {
        MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "whatsapp".into(),
            account_alias: "personal".into(),
            account_email: None,
            thread_id: "thread-1".into(),
            message_id: "message-1".into(),
            provider_cursor: None,
            label_ids: Vec::new(),
            subject: Some("Project update".into()),
            from_name: Some("Asha".into()),
            from_address: None,
            to_domains: Vec::new(),
            cc_domains: Vec::new(),
            internal_date: 1_700_000_000_000,
            observed_at: 1_700_000_000_100,
            direction: Some(MessageDirection::Inbound),
            summary: Some("Compatibility summary".into()),
            intent: Some("fyi".into()),
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: Some(ChannelInformationBrief {
                schema_version: 2,
                information_type: ChannelInformationType::ChangeNotice,
                summary: "The launch moved to Friday.".into(),
                key_facts: vec!["Launch is Friday".into()],
                changes: Vec::new(),
                temporal_facts: Vec::new(),
                stated_action: None,
                detail_status: ChannelDetailStatus::Complete,
                missing_details: Vec::new(),
            }),
            distill_contract_version: Some(2),
            distilled_at: Some(1_700_000_000_200),
            distill_revision: Some(7),
            distill_state: DistillState::Done,
            distill_attempts: 0,
            sensitive_suppressed: suppressed,
            origin: MailRecordOrigin::MetadataSync,
        }
    }

    #[test]
    fn every_distilled_channel_uses_the_provider_neutral_projection() {
        let candidate = candidate_from_channel_message(&message(false)).unwrap();
        assert_eq!(candidate.identity.adapter_id, "channel:whatsapp");
        assert_eq!(candidate.identity.item_id, "personal/message-1");
        assert_eq!(candidate.cheap_text, "The launch moved to Friday.");
        assert_eq!(candidate.privacy, ContentPrivacy::Private);
        assert_eq!(candidate.metadata["thread_id"], json!("thread-1"));
        assert!(candidate.validate().is_ok());
    }

    #[test]
    fn suppressed_channel_messages_never_enter_feed_selection() {
        assert!(candidate_from_channel_message(&message(true)).is_none());
    }

    #[test]
    fn stale_summary_never_projects_from_non_done_distill_states() {
        for state in [
            DistillState::Pending,
            DistillState::Failed,
            DistillState::Skipped,
            DistillState::Suppressed,
        ] {
            let mut row = message(false);
            row.distill_state = state;
            assert!(
                candidate_from_channel_message(&row).is_none(),
                "projected stale summary for {state:?}"
            );
        }
    }
}
