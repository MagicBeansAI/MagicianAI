//! Errors of the decision plane.

use std::time::Duration;

use crate::primitives::{OptionId, QuestionId};

#[derive(Debug, Clone, thiserror::Error)]
pub enum DecisionError {
    /// Local scheduling pressure, never a provider outage or a billed attempt.
    #[error("decision model dispatch queue is full")]
    DispatchFull,

    #[error("decision model dispatch wait timed out")]
    DispatchTimeout,

    #[error("decision transport error: {0}")]
    Transport(String),

    #[error("decision request timed out")]
    Timeout,

    #[error("decision model rate limited (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },

    #[error("decision model returned status {status}: {body}")]
    ProviderStatus { status: u16, body: String },

    #[error("decision model response invalid: {0}")]
    InvalidResponse(String),

    /// The declared contract of the plane: an answer outside the supplied
    /// options is an error, never a coerce. Composition may degrade; it
    /// must not silently relabel.
    #[error("question '{question}' returned undeclared option '{option}'")]
    UnknownOption {
        question: QuestionId,
        option: OptionId,
    },

    #[error("decision pack not found: {0}")]
    PackMissing(String),

    #[error("decision operation unbound: {0}")]
    OperationUnbound(String),

    #[error("api key missing for decision model: {0}")]
    ApiKeyMissing(String),

    /// Every model on the operation's route was skipped (misfit, cooling
    /// down) or failed. Treated like unbound: the caller keeps its
    /// incumbent path.
    #[error("no decision model could take '{operation}': {skipped:?}")]
    NoFittingModel {
        operation: String,
        skipped: Vec<String>,
    },
}

impl DecisionError {
    /// Content-free provider health classification. Normal model-fit and
    /// confidence escalation must not be presented as an outage.
    pub fn health_reason(&self) -> Option<&'static str> {
        match self {
            Self::ApiKeyMissing(_) => Some("structured_provider_authentication"),
            Self::RateLimited { .. } => Some("structured_provider_rate_limit"),
            Self::Timeout => Some("structured_provider_timeout"),
            Self::Transport(_) => Some("structured_provider_unavailable"),
            Self::ProviderStatus { status, body } => {
                let body = body.to_ascii_lowercase();
                if *status == 402
                    || [
                        "insufficient_quota",
                        "insufficient credits",
                        "credit balance",
                        "billing",
                    ]
                    .iter()
                    .any(|s| body.contains(s))
                {
                    Some("structured_provider_credit")
                } else if matches!(status, 401 | 403) {
                    Some("structured_provider_authentication")
                } else if *status == 429 {
                    Some("structured_provider_rate_limit")
                } else if *status >= 500 {
                    Some("structured_provider_unavailable")
                } else {
                    None
                }
            },
            _ => None,
        }
    }
}
