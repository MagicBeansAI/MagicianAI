//! The seams a source and the answer sink implement (plan §6.2).
//!
//! A source is authorised by the owner for this purpose; the registry
//! re-reads that authority on every call so revocation is seen before the
//! next fetch and before the answer. A watch fetches bounded, value-free
//! evidence for one challenge. The sink is the one door through which a code
//! becomes an answer — the same first-response-wins boundary a person uses.
use async_trait::async_trait;
use zeroize::Zeroizing;

use super::matching::{ChallengeContext, MessageEvidence, SourceKind};

/// One source the owner enabled and permitted for verification codes, as
/// the registry names it. `account` is the alias, mailbox or device id the
/// watch needs; `label` is what status shows (already masked).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthorizedSource {
    pub kind: SourceKind,
    pub account: String,
    pub label: String,
}

#[async_trait]
pub trait SourceRegistry: Send + Sync {
    /// Every source of the scope that is enabled, carries the
    /// verification-code purpose and is reachable now. Read fresh each time.
    async fn authorized_sources(&self, principal: &str, workspace: &str) -> Vec<AuthorizedSource>;
}

/// What a source that decides on its own (the Android companion) says
/// about the challenge, beyond the messages it hands over.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SourceSignal {
    /// Nothing beyond the messages.
    #[default]
    None,
    /// The source answered the challenge itself over its own authority:
    /// nothing to extract here, the ask's resolution arrives as
    /// `hitl.resolved`.
    AnsweredBySource,
    /// The source learned the ask is already resolved.
    AlreadyResolved,
    /// The source saw evidence it could not separate: the person decides.
    Ambiguous(String),
    /// The source cannot serve this challenge (no permission on the device,
    /// an answer the runtime refused, an older companion): a value-free
    /// sentence for status.
    Unavailable(String),
}

/// One poll's yield.
#[derive(Debug, Default)]
pub struct WatchPoll {
    pub messages: Vec<MessageEvidence>,
    /// The source will yield nothing more for this challenge (a device that
    /// waited the whole window, a source that answers once).
    pub exhausted: bool,
    pub signal: SourceSignal,
}

impl WatchPoll {
    pub fn messages(messages: Vec<MessageEvidence>) -> Self {
        Self {
            messages,
            exhausted: false,
            signal: SourceSignal::None,
        }
    }
}

#[async_trait]
pub trait SourceWatch: Send + Sync {
    fn kind(&self) -> SourceKind;
    /// Fetch at most `limit` messages received at or after `since_ms` that
    /// may belong to the challenge. Bounded by the implementation; an error
    /// is a value-free sentence.
    async fn poll(
        &self,
        source: &AuthorizedSource,
        challenge: &ChallengeContext,
        since_ms: i64,
        limit: usize,
    ) -> Result<WatchPoll, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerOutcome {
    /// The code became the ask's answer (first response).
    Accepted,
    /// Someone — the person, another source — answered first.
    AlreadyResolved,
    /// The ask could not take the answer (scope, persistence, unknown id).
    Refused(String),
}

/// The request as the lifecycle named it, for the sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerTarget {
    pub principal: String,
    pub workspace: String,
    pub correlation_id: String,
    /// `user_request`, `agentic`, …
    pub source: String,
    pub execution_id: Option<String>,
}

#[async_trait]
pub trait ChallengeAnswerSink: Send + Sync {
    async fn answer(&self, target: &AnswerTarget, code: Zeroizing<String>) -> AnswerOutcome;
}
