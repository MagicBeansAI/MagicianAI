//! Inbound verification-code retrieval (secure HITL plan §6.2, P6).
//!
//! A verification challenge is a pending secure ask whose published spec is
//! `otp`. For such a challenge, and only while it is open, the resolver
//! watches the sources the owner permitted for this purpose, extracts a code
//! deterministically from a message whose trusted facts match the challenge,
//! and answers the ask through the same first-response-wins boundary a person
//! uses — so the code enters custody the way a typed answer does and the model
//! sees only status. Raw message content never leaves the watcher.
//!
//! - [`extract`] — deterministic, bounded code extraction.
//! - [`matching`] — the challenge context, the message evidence, the verdict.
//! - [`sources`] — the traits a source and the answer sink implement.
//! - [`resolver`] — the challenge registry, watchers, bounds, status.
use std::sync::{Arc, OnceLock};

pub mod android;
pub mod extract;
pub mod matching;
pub mod registry;
pub mod resolver;
pub mod sink;
pub mod sources;

pub use android::AndroidVerificationWatch;
pub use extract::{extract_code, ExpectedFormat, Extraction};
pub use matching::{
    registrable_domain, ChallengeContext, Decision, MessageEvidence, MessageIdentity, SourceKind,
    Verdict,
};
pub use registry::RuntimeSourceRegistry;
pub use resolver::{
    challenge_from_schema, ChallengeStatus, Clock, RetrievalStatus, StatusTransport, SystemClock,
    VerificationCodeResolver,
};
pub use sink::{install_agentic_sink, is_agentic_source, RuntimeAnswerSink, RESOLVER_CHANNEL};
pub use sources::{
    AnswerOutcome, AnswerTarget, AuthorizedSource, ChallengeAnswerSink, SourceRegistry,
    SourceSignal, SourceWatch, WatchPoll,
};

static GLOBAL: OnceLock<Arc<VerificationCodeResolver>> = OnceLock::new();

/// Publish the process's resolver so the API layer can install the agentic
/// answer sink and read status.
pub fn install_global(resolver: Arc<VerificationCodeResolver>) {
    let _ = GLOBAL.set(resolver);
}

pub fn global() -> Option<Arc<VerificationCodeResolver>> {
    GLOBAL.get().cloned()
}
