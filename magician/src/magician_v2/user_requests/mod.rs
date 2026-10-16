//! UserRequestService — central request/response hub for asking users questions.
//!
//! Any subsystem (executor, scheduler, etc.) can call
//! `UserRequestService::ask()` to block until the user responds or a timeout
//! fires, or `UserRequestService::submit_nonblocking()` to return after durable
//! acceptance. The first response wins; all other channels are dismissed via
//! the canonical HITL lifecycle.
//!
//! This module now backs executor-managed escalation paths that need
//! channel-agnostic human decisions without routing through `agentic-resume`.

pub mod service;

pub(crate) use service::ONE_TIME_COLLECTION_MAX_SECS;
pub use service::{
    RequestOption, ScopedResponseResult, SensitiveAnswer, SensitiveAnswerStatus, SensitiveField,
    SensitiveInputSpec, SensitiveKind, SensitiveProvenance, UserRequest, UserRequestRecord,
    UserRequestService, UserRequestStatus, UserRequestSubmission, UserRequestSubmissionError,
    UserResponse, ANDROID_NOTIFICATION_CHANNEL, SECURE_ANSWER_CHANNEL,
    VERIFICATION_CODE_RESOLVER_CHANNEL,
};
