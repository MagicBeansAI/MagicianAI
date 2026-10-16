//! The local macOS Messages store as a verification-code source: a bounded,
//! read-only query of inbound messages received inside the challenge window.
//! Availability depends on what this Mac's Messages holds (forwarded SMS
//! included); nothing here reaches into any phone.
use std::sync::Arc;

use async_trait::async_trait;

use magician::magician_v2::verification_codes::{
    AuthorizedSource, ChallengeContext, MessageEvidence, SourceKind, SourceWatch, WatchPoll,
};

use super::{bounded, MAX_BODY_BYTES};
use crate::channel_assist::ingest_imessage::recent_inbound_for_verification;

#[derive(Default)]
pub struct MessagesVerificationWatch;

impl MessagesVerificationWatch {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl SourceWatch for MessagesVerificationWatch {
    fn kind(&self) -> SourceKind {
        SourceKind::Messages
    }

    async fn poll(
        &self,
        source: &AuthorizedSource,
        _challenge: &ChallengeContext,
        since_ms: i64,
        limit: usize,
    ) -> Result<WatchPoll, String> {
        let rows = recent_inbound_for_verification(since_ms, limit.clamp(1, 20))
            .await
            .map_err(|_| "messages store read failed".to_string())?;
        let messages = rows
            .into_iter()
            .map(|row| MessageEvidence {
                source: SourceKind::Messages,
                account: source.account.clone(),
                message_id: row.rowid.to_string(),
                received_at_ms: row.received_at_ms,
                // A phone number or short code labels the sender; it is not
                // an authenticated identity and the matcher treats it as a hint.
                sender_address: row.sender,
                authenticated: None,
                subject: None,
                body: bounded(&row.text, MAX_BODY_BYTES),
            })
            .collect();
        Ok(WatchPoll::messages(messages))
    }
}
