//! Error classification: map `LLMError` variants to retry decisions.

use crate::error::LLMError;

use super::job::ErrorClass;

/// Classify an error for retry purposes.
pub fn classify(err: &LLMError) -> ErrorClass {
    let err = err.root_cause();
    match err {
        LLMError::Routed { .. } => unreachable!("root_cause removes routed wrappers"),
        LLMError::Transport(msg) => {
            // Coarse heuristic on transport messages — providers often wrap
            // timeouts/network errors in this variant.
            let lower = msg.to_ascii_lowercase();
            if lower.contains("timeout") || lower.contains("timed out") {
                ErrorClass::Timeout
            } else {
                ErrorClass::Network
            }
        },
        LLMError::Timeout => ErrorClass::Timeout,
        LLMError::RateLimited { .. } => ErrorClass::RateLimit,
        LLMError::WorkerWatchdog { .. } => ErrorClass::Timeout,
        LLMError::Provider { message, .. } => classify_provider_message(message),
        LLMError::ProviderStatus { status, .. } => match status {
            429 => ErrorClass::RateLimit,
            500..=599 => ErrorClass::Server5xx,
            _ => ErrorClass::Provider4xx,
        },
        LLMError::Cancelled { .. } => ErrorClass::Cancelled,
        LLMError::ProviderUnavailable => ErrorClass::Server5xx,
        LLMError::AllRetriesExhausted { .. } => ErrorClass::Unknown,
        LLMError::Context(_) => ErrorClass::Provider4xx,
        LLMError::Validation(_) => ErrorClass::Provider4xx,
        LLMError::UnsupportedCapability(_) => ErrorClass::Provider4xx,
        LLMError::Configuration(_) => ErrorClass::Provider4xx,
        LLMError::Serialization(_) => ErrorClass::ParseError,
        LLMError::DeadlineExceeded => ErrorClass::Cancelled,
        LLMError::QueueFull { .. }
        | LLMError::QueueBytesFull { .. }
        | LLMError::RequestTooLarge { .. } => ErrorClass::Unknown,
        LLMError::Other(_) => ErrorClass::Unknown,
    }
}

fn classify_provider_message(message: &str) -> ErrorClass {
    let lower = message.to_ascii_lowercase();
    if lower.contains("429") || lower.contains("rate limit") || lower.contains("too many requests")
    {
        return ErrorClass::RateLimit;
    }
    if lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("504")
        || lower.contains("internal server error")
        || lower.contains("bad gateway")
        || lower.contains("service unavailable")
        || lower.contains("gateway timeout")
    {
        return ErrorClass::Server5xx;
    }
    // Provider safety refusals are semantically distinct from an ordinary
    // caller 4xx even when the HTTP status is 400/403. Classify the more
    // specific contract first so Phase 2 reliability rows do not blame the
    // request schema for a policy decision.
    if lower.contains("content policy")
        || lower.contains("safety")
        || lower.contains("refusal")
        || lower.contains("blocked")
    {
        return ErrorClass::ContentPolicy;
    }
    if lower.contains("400")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("422")
        || lower.contains("invalid api key")
        || lower.contains("unauthor")
        || lower.contains("forbidden")
    {
        return ErrorClass::Provider4xx;
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return ErrorClass::Timeout;
    }
    if lower.contains("parse") || lower.contains("malformed") {
        return ErrorClass::ParseError;
    }
    ErrorClass::Unknown
}
