use std::time::Duration;
use thiserror::Error;

use crate::capability::LLMProviderKind;
use crate::chunking::ContextError;

/// Result alias used across the crate.
pub type LLMResult<T> = Result<T, LLMError>;

/// Common error surface for providers, router, and dispatch integrations.
#[derive(Debug, Error)]
pub enum LLMError {
    /// Error from a concrete route after profile defaults/fallback selection.
    /// The wrapped source retains retry classification while the route fields
    /// let observability attribute failures to the provider/model that
    /// actually ran.
    #[error("{source}")]
    Routed {
        profile: String,
        provider: LLMProviderKind,
        model: String,
        #[source]
        source: Box<LLMError>,
    },
    /// Typed physical/logical context budget failure.
    #[error(transparent)]
    Context(#[from] ContextError),
    /// Configuration issues (missing keys, invalid models, etc).
    #[error("configuration error: {0}")]
    Configuration(String),
    /// Provider returned an error.
    #[error("provider `{provider}` error: {message}")]
    Provider {
        /// Provider identifier.
        provider: String,
        /// Detailed message from the provider.
        message: String,
    },
    /// Provider answered with a non-2xx HTTP status. Unlike [`Self::Provider`],
    /// the status code survives as a typed field so callers can preserve
    /// HTTP-semantic behavior (429/5xx retriable, 4xx terminal) instead of
    /// re-deriving it from a string. Required by the embedding seam, where
    /// vector-index batch retry classification keys off the status.
    #[error("provider `{provider}` returned HTTP {status}: {body}")]
    ProviderStatus {
        /// Provider identifier.
        provider: String,
        /// HTTP status code returned by the provider.
        status: u16,
        /// Response body (diagnostic; redacted before leaving disclosure-bound
        /// requests).
        body: String,
    },
    /// Requested capability is not supported by the target model/provider.
    #[error("unsupported capability: {0}")]
    UnsupportedCapability(String),
    /// Input failed validation before hitting the provider.
    #[error("validation error: {0}")]
    Validation(String),
    /// Transport/network failure while calling the provider.
    #[error("transport error: {0}")]
    Transport(String),
    /// Timeout waiting for the provider response.
    #[error("request timed out while waiting for provider response")]
    Timeout,
    /// Provider returned 429 Too Many Requests.
    #[error("rate limited by provider; retry after {retry_after:?}")]
    RateLimited {
        /// Suggested retry delay from Retry-After header, when present.
        retry_after: Option<Duration>,
    },
    /// Serialization/deserialization failure while building payloads.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    /// Dispatch-queue: the owning task / chat session / explicit cancel signal
    /// fired before this call completed. Callers should treat as operational,
    /// not as an error worth escalating.
    #[error("call cancelled: {reason}")]
    Cancelled {
        /// Human-readable reason (e.g. "task_cancelled", "process_restart").
        reason: String,
    },
    /// Dispatch-queue: retry budget exhausted (default 6 attempts).
    #[error("all retries exhausted after {attempts} attempts: {last_error}")]
    AllRetriesExhausted {
        /// Total attempts made across all cycles.
        attempts: u32,
        /// Final error message from the last attempt.
        last_error: String,
    },
    /// Dispatch-queue: provider circuit breaker is open; new jobs fast-fail.
    #[error("provider unavailable (circuit breaker open)")]
    ProviderUnavailable,
    /// Dispatch-queue: worker watchdog fired (call ran longer than expected).
    #[error("worker watchdog: call exceeded {factor}x profile timeout")]
    WorkerWatchdog {
        /// Multiplier applied to the profile timeout that triggered the watchdog.
        factor: f64,
    },
    /// Dispatch-queue: caller-supplied submission deadline elapsed before
    /// pickup, OR submit was called after the deadline.
    #[error("submission deadline exceeded")]
    DeadlineExceeded,
    /// Dispatch-queue: bounded lane is at capacity; caller should back off.
    #[error("queue lane `{priority}` full ({depth}/{capacity})")]
    QueueFull {
        /// Priority lane that was full.
        priority: &'static str,
        /// Current depth of that lane.
        depth: usize,
        /// Configured capacity of that lane.
        capacity: usize,
    },
    /// Dispatch request exceeds the per-job retained-memory ceiling.
    #[error("request retained bytes {bytes} exceed the admitted {capacity_bytes}-byte ceiling")]
    RequestTooLarge { bytes: u64, capacity_bytes: u64 },
    /// Priority/global retained-byte budget is exhausted even though an entry
    /// slot may still be available.
    #[error("queue lane `{priority}` retained-byte budget full ({queued_bytes}/{capacity_bytes})")]
    QueueBytesFull {
        priority: &'static str,
        queued_bytes: u64,
        capacity_bytes: u64,
    },
    /// Unexpected failure.
    #[error("unexpected error: {0}")]
    Other(String),
}

impl Clone for LLMError {
    fn clone(&self) -> Self {
        match self {
            Self::Routed {
                profile,
                provider,
                model,
                source,
            } => Self::Routed {
                profile: profile.clone(),
                provider: provider.clone(),
                model: model.clone(),
                source: Box::new((**source).clone()),
            },
            Self::Context(error) => Self::Context(error.clone()),
            Self::Configuration(s) => Self::Configuration(s.clone()),
            Self::Provider { provider, message } => Self::Provider {
                provider: provider.clone(),
                message: message.clone(),
            },
            Self::ProviderStatus {
                provider,
                status,
                body,
            } => Self::ProviderStatus {
                provider: provider.clone(),
                status: *status,
                body: body.clone(),
            },
            Self::UnsupportedCapability(s) => Self::UnsupportedCapability(s.clone()),
            Self::Validation(s) => Self::Validation(s.clone()),
            Self::Transport(s) => Self::Transport(s.clone()),
            Self::Timeout => Self::Timeout,
            Self::RateLimited { retry_after } => Self::RateLimited {
                retry_after: *retry_after,
            },
            // `serde_json::Error` is not Clone. Rebuild an equivalent typed
            // invalid-data error instead of degrading this to `Other`, which
            // would change retry/analytics classification to `unknown` for
            // idempotent subscribers and other cloned terminal results.
            Self::Serialization(err) => Self::Serialization(serde_json::Error::io(
                std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()),
            )),
            Self::Cancelled { reason } => Self::Cancelled {
                reason: reason.clone(),
            },
            Self::AllRetriesExhausted {
                attempts,
                last_error,
            } => Self::AllRetriesExhausted {
                attempts: *attempts,
                last_error: last_error.clone(),
            },
            Self::ProviderUnavailable => Self::ProviderUnavailable,
            Self::WorkerWatchdog { factor } => Self::WorkerWatchdog { factor: *factor },
            Self::DeadlineExceeded => Self::DeadlineExceeded,
            Self::QueueFull {
                priority,
                depth,
                capacity,
            } => Self::QueueFull {
                priority: *priority,
                depth: *depth,
                capacity: *capacity,
            },
            Self::RequestTooLarge {
                bytes,
                capacity_bytes,
            } => Self::RequestTooLarge {
                bytes: *bytes,
                capacity_bytes: *capacity_bytes,
            },
            Self::QueueBytesFull {
                priority,
                queued_bytes,
                capacity_bytes,
            } => Self::QueueBytesFull {
                priority,
                queued_bytes: *queued_bytes,
                capacity_bytes: *capacity_bytes,
            },
            Self::Other(s) => Self::Other(s.clone()),
        }
    }
}

impl LLMError {
    /// Remove provider-controlled/free-form diagnostic text before an error
    /// leaves a disclosure-bound request. The typed retry and routing shape is
    /// preserved, but response bodies, echoed prompts and transport details
    /// cannot enter logs, task state or caller-visible diagnostics.
    pub(crate) fn redact_disclosure_details(self) -> Self {
        match self {
            Self::Routed {
                profile,
                provider,
                model,
                source,
            } => Self::Routed {
                profile,
                provider,
                model,
                source: Box::new(source.redact_disclosure_details()),
            },
            Self::Configuration(_) => {
                Self::Configuration("guarded request configuration failed".to_owned())
            },
            Self::Provider { provider, .. } => Self::Provider {
                provider,
                message: "guarded provider request failed".to_owned(),
            },
            Self::ProviderStatus {
                provider, status, ..
            } => Self::ProviderStatus {
                provider,
                status,
                body: "guarded provider status body withheld".to_owned(),
            },
            Self::UnsupportedCapability(_) => {
                Self::UnsupportedCapability("guarded request capability is unavailable".to_owned())
            },
            Self::Validation(_) => Self::Validation("guarded request validation failed".to_owned()),
            Self::Transport(_) => Self::Transport("guarded provider transport failed".to_owned()),
            Self::Serialization(_) => {
                Self::Serialization(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "guarded request serialization or response decoding failed",
                )))
            },
            Self::Cancelled { .. } => Self::Cancelled {
                reason: "guarded request cancelled".to_owned(),
            },
            Self::AllRetriesExhausted { attempts, .. } => Self::AllRetriesExhausted {
                attempts,
                last_error: "guarded provider attempts failed".to_owned(),
            },
            Self::Other(_) => Self::Other("guarded provider request failed".to_owned()),
            safe @ (Self::Context(_)
            | Self::Timeout
            | Self::RateLimited { .. }
            | Self::ProviderUnavailable
            | Self::WorkerWatchdog { .. }
            | Self::DeadlineExceeded
            | Self::QueueFull { .. }
            | Self::RequestTooLarge { .. }
            | Self::QueueBytesFull { .. }) => safe,
        }
    }

    /// Attach the last concrete route without obscuring an identity already
    /// supplied by a deeper router.
    pub fn with_route(
        self,
        profile: impl Into<String>,
        provider: LLMProviderKind,
        model: impl Into<String>,
    ) -> Self {
        if matches!(&self, Self::Routed { .. }) {
            return self;
        }
        Self::Routed {
            profile: profile.into(),
            provider,
            model: model.into(),
            source: Box::new(self),
        }
    }

    /// Exact route attached by the transport router, when provider execution
    /// began before the error surfaced.
    pub fn effective_route(&self) -> Option<(&str, &LLMProviderKind, &str)> {
        match self {
            Self::Routed {
                profile,
                provider,
                model,
                ..
            } => Some((profile, provider, model)),
            _ => None,
        }
    }

    /// Underlying provider/transport error used by retry classifiers.
    pub fn root_cause(&self) -> &Self {
        match self {
            Self::Routed { source, .. } => source.root_cause(),
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloning_serialization_error_preserves_its_typed_class() {
        let source = serde_json::from_str::<serde_json::Value>("{")
            .expect_err("fixture must be invalid JSON");
        let error = LLMError::Serialization(source);
        let cloned = error.clone();

        assert!(matches!(cloned, LLMError::Serialization(_)));
        assert_eq!(
            crate::dispatch::classifier::classify(&cloned),
            crate::dispatch::ErrorClass::ParseError
        );
    }

    #[test]
    fn disclosure_redaction_preserves_route_and_retry_class_without_free_form_details() {
        let error = LLMError::Provider {
            provider: "openai".to_owned(),
            message: "upstream echoed protected-app-input".to_owned(),
        }
        .with_route("app-local", LLMProviderKind::OpenAI, "local-model");

        let redacted = error.redact_disclosure_details();
        assert_eq!(
            redacted.effective_route(),
            Some(("app-local", &LLMProviderKind::OpenAI, "local-model"))
        );
        assert!(matches!(redacted.root_cause(), LLMError::Provider { .. }));
        let diagnostic = redacted.to_string();
        assert!(!diagnostic.contains("protected-app-input"));
        assert!(diagnostic.contains("guarded provider request failed"));
    }
}
