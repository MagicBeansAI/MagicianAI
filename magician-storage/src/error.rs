//! Actionable storage errors. Diagnostics never carry secrets or payload.

use std::fmt;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    Never,
    SameIdempotencyKey,
    NewAttempt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitLikelihood {
    No,
    Maybe,
    Yes,
}

#[derive(Debug, Clone, Error)]
pub enum StorageError {
    #[error("not found")]
    NotFound,
    #[error("conflict")]
    Conflict {
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("lease lost")]
    LeaseLost { resource: String, generation: u64 },
    #[error("integrity mismatch")]
    Integrity { expected: String, actual: String },
    #[error("corrupt store")]
    Corrupt { detail: String },
    #[error("unavailable")]
    Unavailable { retry_after: Option<Duration> },
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    #[error("permission denied")]
    PermissionDenied,
    #[error("capacity exceeded")]
    CapacityExceeded,
    #[error("invalid key")]
    InvalidKey { detail: String },
    #[error("unsupported capability")]
    UnsupportedCapability,
    #[error("timeout")]
    Timeout,
    #[error("backend error")]
    Backend { safe_detail: String },
}

impl StorageError {
    pub fn invalid_key(detail: impl Into<String>) -> Self {
        Self::InvalidKey {
            detail: detail.into(),
        }
    }

    pub fn backend(safe_detail: impl Into<String>) -> Self {
        Self::Backend {
            safe_detail: safe_detail.into(),
        }
    }

    pub fn retry_class(&self) -> RetryClass {
        match self {
            Self::Unavailable { .. }
            | Self::RateLimited { .. }
            | Self::Timeout
            | Self::Backend { .. } => RetryClass::SameIdempotencyKey,
            _ => RetryClass::Never,
        }
    }

    pub fn retry_safe(&self) -> bool {
        !matches!(self.retry_class(), RetryClass::Never)
    }

    pub fn requires_idempotency_key(&self) -> bool {
        matches!(self.retry_class(), RetryClass::SameIdempotencyKey)
    }

    pub fn may_have_committed(&self) -> CommitLikelihood {
        match self {
            Self::Unavailable { .. } | Self::Timeout | Self::Backend { .. } => {
                CommitLikelihood::Maybe
            },
            Self::Conflict { .. } => CommitLikelihood::Yes,
            _ => CommitLikelihood::No,
        }
    }

    pub fn fail_closed(&self) -> bool {
        !matches!(
            self,
            Self::UnsupportedCapability | Self::NotFound | Self::RateLimited { .. }
        )
    }

    pub fn safe_diagnostic(&self) -> String {
        match self {
            Self::NotFound => "not_found".into(),
            Self::Conflict { expected, actual } => format!(
                "conflict expected={} actual={}",
                expected.as_deref().unwrap_or("-"),
                actual.as_deref().unwrap_or("-")
            ),
            Self::LeaseLost {
                resource,
                generation,
            } => format!("lease_lost resource={resource} generation={generation}"),
            Self::Integrity { .. } => "integrity_mismatch".into(),
            Self::Corrupt { detail } => format!("corrupt:{detail}"),
            Self::Unavailable { retry_after } => format!("unavailable retry_after={retry_after:?}"),
            Self::RateLimited { retry_after } => {
                format!("rate_limited retry_after={retry_after:?}")
            },
            Self::PermissionDenied => "permission_denied".into(),
            Self::CapacityExceeded => "capacity_exceeded".into(),
            Self::InvalidKey { detail } => format!("invalid_key:{detail}"),
            Self::UnsupportedCapability => "unsupported_capability".into(),
            Self::Timeout => "timeout".into(),
            Self::Backend { safe_detail } => format!("backend:{safe_detail}"),
        }
    }
}

impl fmt::Display for RetryClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Never => f.write_str("never"),
            Self::SameIdempotencyKey => f.write_str("same_idempotency_key"),
            Self::NewAttempt => f.write_str("new_attempt"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_retryable_with_same_idempotency_key() {
        let err = StorageError::Timeout;
        assert!(err.retry_safe());
        assert!(err.requires_idempotency_key());
        assert_eq!(err.may_have_committed(), CommitLikelihood::Maybe);
        assert!(err.fail_closed());
    }

    #[test]
    fn invalid_key_is_not_retryable() {
        let err = StorageError::invalid_key("slash");
        assert!(!err.retry_safe());
        assert_eq!(err.retry_class(), RetryClass::Never);
        assert_eq!(err.may_have_committed(), CommitLikelihood::No);
    }

    #[test]
    fn diagnostics_do_not_echo_payload_or_dsn() {
        let err = StorageError::Backend {
            safe_detail: "s3_timeout".into(),
        };
        let text = err.safe_diagnostic();
        assert!(!text.contains("AKIA"));
        assert!(!text.contains("postgres://"));
        assert!(text.contains("s3_timeout"));
    }
}
