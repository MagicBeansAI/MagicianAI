//! Gmail as a verification-code source: `messages list` with an `after:`
//! query anchored on the challenge window; for each message not yet read
//! for this challenge, the headers first (`format=metadata`: the
//! provider's receive time, `From`, `Subject`, `Authentication-Results`),
//! and the body only when the receive time is inside the window. A message
//! is remembered as read only once it was read; a burst of newer mail is
//! paged past, within a bound.
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

use magician::magician_v2::artifact_v2::{
    workspace::ArtifactV2Workspace, CapabilityScopePaths, CapabilityWorkspaceManager,
};
use magician::magician_v2::verification_codes::{
    AuthorizedSource, ChallengeContext, MessageEvidence, SourceKind, SourceWatch, WatchPoll,
};

use super::{bounded, MAX_BODY_BYTES};
use crate::channel_assist::assist::content::extract_text;
use crate::channel_assist::gws_client::{
    GmailMessageRefPage, GmailVerificationHeaders, GmailVerificationMessage, GwsGmailClient,
    GwsGmailError,
};

/// The three provider reads a poll makes, behind a seam so the paging and
/// remembering logic is tested without the CLI.
#[async_trait]
pub(crate) trait GmailReader: Send + Sync {
    async fn list_refs(
        &self,
        account: &str,
        query: &str,
        max_results: u32,
        page_token: Option<&str>,
    ) -> Result<GmailMessageRefPage, GwsGmailError>;
    async fn headers(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<GmailVerificationHeaders, GwsGmailError>;
    async fn full(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<GmailVerificationMessage, GwsGmailError>;
}

#[async_trait]
impl GmailReader for GwsGmailClient {
    async fn list_refs(
        &self,
        account: &str,
        query: &str,
        max_results: u32,
        page_token: Option<&str>,
    ) -> Result<GmailMessageRefPage, GwsGmailError> {
        self.list_message_refs(account, Some(query), max_results, page_token)
            .await
    }

    async fn headers(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<GmailVerificationHeaders, GwsGmailError> {
        self.get_message_headers_for_verification(account, message_id)
            .await
    }

    async fn full(
        &self,
        account: &str,
        message_id: &str,
    ) -> Result<GmailVerificationMessage, GwsGmailError> {
        self.get_message_for_verification(account, message_id).await
    }
}

/// Messages already read for a challenge, so a poll spends provider calls
/// on new mail only. Bounded per challenge and dropped with it.
const MAX_REMEMBERED: usize = 256;
/// Challenges remembered at once; a challenge outlives none of its watch.
const MAX_CHALLENGES: usize = 64;
/// List pages one poll may walk when every message on a page is newer
/// than the window's end of the last read.
const MAX_PAGES: usize = 3;

pub struct GmailVerificationWatch {
    workspace_layout: Arc<ArtifactV2Workspace>,
    repo_root: PathBuf,
    /// (scope, challenge) → message ids already read.
    read: Mutex<HashMap<String, HashSet<String>>>,
}

impl GmailVerificationWatch {
    pub fn new(workspace_layout: Arc<ArtifactV2Workspace>, repo_root: PathBuf) -> Self {
        Self {
            workspace_layout,
            repo_root,
            read: Mutex::new(HashMap::new()),
        }
    }

    fn client(&self, principal: &str, workspace: &str) -> GwsGmailClient {
        let auth_root = self
            .workspace_layout
            .capability_auth_root(principal, workspace);
        let scope_paths: CapabilityScopePaths = CapabilityWorkspaceManager::new(
            (*self.workspace_layout).clone(),
            self.repo_root.clone(),
        )
        .scope_paths(principal, workspace);
        GwsGmailClient::new(auth_root, Some(scope_paths))
    }

    fn already_read(&self, challenge_key: &str, message_id: &str) -> bool {
        let read = self
            .read
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        read.get(challenge_key)
            .is_some_and(|set| set.contains(message_id))
    }

    /// Remember a message once it was read (a failed read is retried on
    /// the next poll). At the bound, the oldest challenges' memory goes.
    fn mark_read(&self, challenge_key: &str, message_id: &str) -> bool {
        let mut read = self
            .read
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if read.len() >= MAX_CHALLENGES && !read.contains_key(challenge_key) {
            read.clear();
        }
        let set = read.entry(challenge_key.to_string()).or_default();
        if set.len() >= MAX_REMEMBERED {
            return false;
        }
        set.insert(message_id.to_string())
    }
}

#[async_trait]
impl SourceWatch for GmailVerificationWatch {
    fn kind(&self) -> SourceKind {
        SourceKind::Gmail
    }

    async fn poll(
        &self,
        source: &AuthorizedSource,
        challenge: &ChallengeContext,
        since_ms: i64,
        limit: usize,
    ) -> Result<WatchPoll, String> {
        let client = self.client(&challenge.principal, &challenge.workspace);
        self.poll_with(&client, source, challenge, since_ms, limit)
            .await
    }
}

impl GmailVerificationWatch {
    async fn poll_with(
        &self,
        client: &dyn GmailReader,
        source: &AuthorizedSource,
        challenge: &ChallengeContext,
        since_ms: i64,
        limit: usize,
    ) -> Result<WatchPoll, String> {
        // Gmail's `after:` takes epoch seconds and is inclusive of the day
        // granularity on some paths; the receive time is re-checked by the
        // matcher, this only keeps the list short.
        let query = format!("after:{}", (since_ms / 1000).saturating_sub(1).max(0));
        let challenge_key = format!(
            "{}|{}|{}",
            challenge.principal, challenge.workspace, challenge.correlation_id
        );
        let mut messages = Vec::new();
        let mut page_token: Option<String> = None;
        let mut reads = 0usize;
        for _ in 0..MAX_PAGES {
            let page = client
                .list_refs(
                    &source.account,
                    &query,
                    limit.clamp(1, 20) as u32,
                    page_token.as_deref(),
                )
                .await
                .map_err(|error| format!("gmail list failed: {}", error_kind(&error)))?;
            // Newest first: once a message older than the window shows up,
            // everything after it is older still.
            let mut reached_window_start = false;
            for reference in page.refs {
                if self.already_read(&challenge_key, &reference.message_id) {
                    continue;
                }
                if reads >= limit {
                    break;
                }
                reads += 1;
                let headers = match client.headers(&source.account, &reference.message_id).await {
                    Ok(headers) => headers,
                    Err(error) => {
                        tracing::warn!(kind = "gmail", error = %error_kind(&error), "[VERIFICATION-CODES] message headers read failed");
                        continue;
                    },
                };
                if headers.internal_date_ms < since_ms {
                    self.mark_read(&challenge_key, &reference.message_id);
                    reached_window_start = true;
                    break;
                }
                let full = match client.full(&source.account, &reference.message_id).await {
                    Ok(full) => full,
                    Err(error) => {
                        tracing::warn!(kind = "gmail", error = %error_kind(&error), "[VERIFICATION-CODES] message read failed");
                        continue;
                    },
                };
                self.mark_read(&challenge_key, &reference.message_id);
                let body = full
                    .payload
                    .as_ref()
                    .map(|payload| bounded(&extract_text(payload).text, MAX_BODY_BYTES))
                    .unwrap_or_default();
                messages.push(MessageEvidence {
                    source: SourceKind::Gmail,
                    account: source.account.clone(),
                    message_id: full.message_id.clone(),
                    received_at_ms: full.internal_date_ms,
                    sender_address: headers.from.as_deref().map(address_of),
                    authenticated: headers.sender_authenticated(),
                    subject: headers.subject.as_deref().map(|s| bounded(s, 512)),
                    body,
                });
            }
            // Another page while the window's start was not reached and this
            // poll may still read — a page full of mail read on an earlier
            // poll is walked past, never a wall the eleventh message hides
            // behind.
            page_token = page
                .next_page_token
                .filter(|_| !reached_window_start && reads < limit);
            if page_token.is_none() {
                break;
            }
        }
        Ok(WatchPoll::messages(messages))
    }
}

/// `Name <user@host>` → `user@host`; a bare address stays as it is.
pub fn address_of(from: &str) -> String {
    let trimmed = from.trim();
    match (trimmed.rfind('<'), trimmed.rfind('>')) {
        (Some(start), Some(end)) if end > start => {
            trimmed[start + 1..end].trim().to_ascii_lowercase()
        },
        _ => trimmed.to_ascii_lowercase(),
    }
}

/// A value-free description of a provider error for the log.
fn error_kind(error: &GwsGmailError) -> &'static str {
    if error.history_expired() {
        "history expired"
    } else if error.not_found() {
        "not found"
    } else {
        "provider error"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_assist::gws_client::GmailMessageRef;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mailbox of messages, newest first, all received at one time, ten
    /// to a page.
    struct FakeMailbox {
        ids: Vec<String>,
        received_at_ms: i64,
        lists: AtomicUsize,
        headers: AtomicUsize,
        fulls: AtomicUsize,
    }

    impl FakeMailbox {
        fn new(ids: Vec<String>, received_at_ms: i64) -> Self {
            Self {
                ids,
                received_at_ms,
                lists: AtomicUsize::new(0),
                headers: AtomicUsize::new(0),
                fulls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl GmailReader for FakeMailbox {
        async fn list_refs(
            &self,
            _: &str,
            _: &str,
            max: u32,
            page_token: Option<&str>,
        ) -> Result<GmailMessageRefPage, GwsGmailError> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            let start: usize = page_token.map(|t| t.parse().unwrap()).unwrap_or(0);
            let end = (start + max as usize).min(self.ids.len());
            Ok(GmailMessageRefPage {
                refs: self.ids[start..end]
                    .iter()
                    .map(|id| GmailMessageRef {
                        message_id: id.clone(),
                        thread_id: "t".into(),
                    })
                    .collect(),
                next_page_token: (end < self.ids.len()).then(|| end.to_string()),
                result_size_estimate: Some(self.ids.len() as u64),
            })
        }
        async fn headers(
            &self,
            _: &str,
            id: &str,
        ) -> Result<GmailVerificationHeaders, GwsGmailError> {
            self.headers.fetch_add(1, Ordering::SeqCst);
            Ok(GmailVerificationHeaders {
                message_id: id.into(),
                internal_date_ms: self.received_at_ms,
                from: Some("codes@example.test".into()),
                subject: Some("Your code".into()),
                authentication_results: vec![],
            })
        }
        async fn full(&self, _: &str, id: &str) -> Result<GmailVerificationMessage, GwsGmailError> {
            self.fulls.fetch_add(1, Ordering::SeqCst);
            Ok(GmailVerificationMessage {
                message_id: id.into(),
                thread_id: "t".into(),
                internal_date_ms: self.received_at_ms,
                from: Some("codes@example.test".into()),
                subject: Some("Your code".into()),
                authentication_results: vec![],
                payload: None,
            })
        }
    }

    fn make_watch() -> GmailVerificationWatch {
        GmailVerificationWatch::new(
            Arc::new(ArtifactV2Workspace::new(
                std::env::temp_dir().join("gmail-verification-watch-test"),
            )),
            PathBuf::from("."),
        )
    }

    fn challenge() -> ChallengeContext {
        ChallengeContext {
            principal: "owner".into(),
            workspace: "ws".into(),
            correlation_id: "req-1".into(),
            started_at_ms: 1_000_000,
            deadline_ms: Some(1_600_000),
            expected_host: None,
            expected: Default::default(),
            lookback_ms: 90_000,
            not_before_ms: None,
            lane: "agentic".to_string(),
        }
    }

    fn source() -> AuthorizedSource {
        AuthorizedSource {
            kind: SourceKind::Gmail,
            account: "personal".into(),
            label: "gmail".into(),
        }
    }

    /// A burst of newer mail: poll one reads the ten newest; poll two walks
    /// past them (already read) to the eleventh instead of listing the same
    /// ten forever. Bodies are fetched only for in-window mail, and a
    /// message is remembered only once read.
    #[tokio::test]
    async fn a_second_poll_pages_past_mail_already_read_to_the_eleventh_message() {
        let mailbox = FakeMailbox::new((1..=11).map(|n| format!("m{n}")).collect(), 1_010_000);
        let watch = make_watch();
        let first = watch
            .poll_with(&mailbox, &source(), &challenge(), 910_000, 10)
            .await
            .unwrap();
        assert_eq!(first.messages.len(), 10);
        assert_eq!(
            mailbox.lists.load(Ordering::SeqCst),
            1,
            "ten reads fill the poll; no second page yet"
        );
        let second = watch
            .poll_with(&mailbox, &source(), &challenge(), 910_000, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = second
            .messages
            .iter()
            .map(|m| m.message_id.as_str())
            .collect();
        assert_eq!(ids, vec!["m11"], "the page of read mail is walked past");
        assert_eq!(mailbox.lists.load(Ordering::SeqCst), 3);
        assert_eq!(
            mailbox.headers.load(Ordering::SeqCst),
            11,
            "every message's headers once"
        );
        assert_eq!(mailbox.fulls.load(Ordering::SeqCst), 11);
        let third = watch
            .poll_with(&mailbox, &source(), &challenge(), 910_000, 10)
            .await
            .unwrap();
        assert!(third.messages.is_empty());
        assert_eq!(
            mailbox.headers.load(Ordering::SeqCst),
            11,
            "nothing re-read"
        );
        // Older than the window: headers only, no body, and the walk stops there.
        let stale = FakeMailbox::new(vec!["old1".into(), "old2".into()], 800_000);
        let watch = make_watch();
        let poll = watch
            .poll_with(&stale, &source(), &challenge(), 910_000, 10)
            .await
            .unwrap();
        assert!(poll.messages.is_empty());
        assert_eq!(
            stale.headers.load(Ordering::SeqCst),
            1,
            "the first old message ends the walk"
        );
        assert_eq!(
            stale.fulls.load(Ordering::SeqCst),
            0,
            "no body for mail outside the window"
        );
    }

    #[test]
    fn the_sender_address_is_taken_from_the_angle_brackets() {
        assert_eq!(
            address_of("Example Security <no-reply@Accounts.Example.test>"),
            "no-reply@accounts.example.test"
        );
        assert_eq!(address_of("codes@example.test"), "codes@example.test");
        assert_eq!(
            address_of("\"Spoof <x@y>\" <real@example.test>"),
            "real@example.test"
        );
    }

    #[test]
    fn a_message_is_remembered_only_once_read_and_the_memory_is_bounded() {
        let watch = GmailVerificationWatch::new(
            Arc::new(ArtifactV2Workspace::new(
                std::env::temp_dir().join("gmail-verification-watch-test"),
            )),
            PathBuf::from("."),
        );
        assert!(!watch.already_read("scope|c1", "m1"));
        assert!(watch.mark_read("scope|c1", "m1"));
        assert!(watch.already_read("scope|c1", "m1"));
        assert!(
            !watch.already_read("scope|c2", "m1"),
            "memory is per challenge"
        );
        for n in 0..MAX_REMEMBERED {
            watch.mark_read("scope|c3", &format!("m{n}"));
        }
        assert!(
            !watch.mark_read("scope|c3", "one-more"),
            "a challenge remembers a bounded number of messages"
        );
        for n in 0..MAX_CHALLENGES {
            watch.mark_read(&format!("scope|c-{n}"), "m");
        }
        assert!(
            !watch.already_read("scope|c1", "m1"),
            "the memory of old challenges goes at the bound"
        );
    }
}
