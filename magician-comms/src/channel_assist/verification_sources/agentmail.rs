//! A service-owned AgentMail inbox as a verification-code source: the named
//! mailbox. A Presto inbox is not interchangeable with the owner's personal
//! mailbox.
//!
//! # This source cannot answer a challenge today, and says so
//!
//! AgentMail exposes no sender authentication. The original rule — "a message
//! here matches only a challenge that bound its destination, and the sender's
//! domain is the check" — rested on a `From` header that anyone who knows the
//! mailbox address can write: send mail claiming the destination's domain
//! inside the challenge window and its digits were used. `matching::judge` now
//! requires a provider verdict from any source that could have one, so every
//! message from here is refused rather than trusted, and this watch reports
//! `Unavailable` so the person is told to type the code instead of waiting on
//! a source that will never answer. Re-enabling it needs the provider's own
//! SPF/DKIM/DMARC result on the message, not a wider rule here.
use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::verification_codes::{
    AuthorizedSource, ChallengeContext, SourceKind, SourceSignal, SourceWatch, WatchPoll,
};

/// The fields stay so the composition root's construction is unchanged and
/// re-enabling the read (once messages carry a verdict) is a one-function
/// change rather than a re-wiring.
pub struct AgentMailVerificationWatch {
    #[allow(dead_code)]
    workspace_layout: Arc<ArtifactV2Workspace>,
    #[allow(dead_code)]
    repo_root: PathBuf,
}

impl AgentMailVerificationWatch {
    pub fn new(workspace_layout: Arc<ArtifactV2Workspace>, repo_root: PathBuf) -> Self {
        Self {
            workspace_layout,
            repo_root,
        }
    }
}

#[async_trait]
impl SourceWatch for AgentMailVerificationWatch {
    fn kind(&self) -> SourceKind {
        SourceKind::AgentMail
    }

    async fn poll(
        &self,
        _source: &AuthorizedSource,
        _challenge: &ChallengeContext,
        _since_ms: i64,
        _limit: usize,
    ) -> Result<WatchPoll, String> {
        // Nothing is read. A message this mailbox holds carries no
        // authentication verdict, so the matcher must refuse it (see the module
        // note); reading it anyway would put an unauthenticated body through
        // extraction for no possible outcome. `exhausted` ends the watch on the
        // first poll and the signal tells the person why, instead of leaving a
        // source that will never answer looking like one that might.
        Ok(WatchPoll {
            messages: Vec::new(),
            exhausted: true,
            signal: SourceSignal::Unavailable(
                "this mailbox carries no sender authentication, so a code in it cannot be trusted"
                    .to_string(),
            ),
        })
    }
}
