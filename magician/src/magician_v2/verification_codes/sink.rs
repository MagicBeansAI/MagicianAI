//! The one door a retrieved code goes through: the ask's own answer path.
//!
//! A `user_request` (chat asks included) is answered with
//! `UserRequestService::respond_scoped` — first response wins, the value
//! enters service custody at accept exactly as a typed answer does. The
//! agentic sources are answered through the API's resume path, which the API
//! crate installs here once (`MagicianV2Api` is per worker; any worker's
//! instance serves).
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use zeroize::Zeroizing;

use super::sources::{AnswerOutcome, AnswerTarget, ChallengeAnswerSink};
use crate::magician_v2::user_requests::{ScopedResponseResult, UserRequestService, UserResponse};

/// The channel a retrieved answer is recorded under (kept on the record by
/// the request service, unlike a relay's channel).
pub const RESOLVER_CHANNEL: &str =
    crate::magician_v2::user_requests::VERIFICATION_CODE_RESOLVER_CHANNEL;

static AGENTIC_SINK: OnceLock<Arc<dyn ChallengeAnswerSink>> = OnceLock::new();

/// The API crate installs the sink that answers agentic pauses.
pub fn install_agentic_sink(sink: Arc<dyn ChallengeAnswerSink>) -> bool {
    AGENTIC_SINK.set(sink).is_ok()
}

pub fn is_agentic_source(source: &str) -> bool {
    matches!(
        source,
        "agentic" | "primitive" | "inner_loop" | "escalation"
    )
}

pub struct RuntimeAnswerSink {
    user_requests: Arc<UserRequestService>,
}

impl RuntimeAnswerSink {
    pub fn new(user_requests: Arc<UserRequestService>) -> Self {
        Self { user_requests }
    }
}

#[async_trait]
impl ChallengeAnswerSink for RuntimeAnswerSink {
    async fn answer(&self, target: &AnswerTarget, code: Zeroizing<String>) -> AnswerOutcome {
        if is_agentic_source(&target.source) {
            return match AGENTIC_SINK.get() {
                Some(sink) => sink.answer(target, code).await,
                None => AnswerOutcome::Refused(
                    "no answer path for an agentic pause is installed".to_string(),
                ),
            };
        }
        let response = UserResponse {
            request_id: target.correlation_id.clone(),
            decision: "provide_input".to_string(),
            input: Some(code.to_string()),
            channel: RESOLVER_CHANNEL.to_string(),
            sensitive: Vec::new(),
        };
        match self
            .user_requests
            .respond_scoped(response, Some(&target.principal), Some(&target.workspace))
            .await
        {
            ScopedResponseResult::Accepted => AnswerOutcome::Accepted,
            ScopedResponseResult::AlreadyResolved => AnswerOutcome::AlreadyResolved,
            ScopedResponseResult::ScopeMismatch => {
                AnswerOutcome::Refused("the request belongs to another scope".to_string())
            },
            ScopedResponseResult::PersistenceUnavailable => AnswerOutcome::Refused(
                "the request service cannot record an answer right now".to_string(),
            ),
        }
    }
}
