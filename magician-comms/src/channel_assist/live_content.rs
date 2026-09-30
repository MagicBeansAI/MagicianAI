//! Scoped, exact-message resolution and explicit live-content fetches shared
//! by Follow-ups and resurfacing.
//!
//! Metadata resolution never fetches a body. [`ChannelEvidenceResolver::fetch_original`]
//! is the only body-bearing path in this module; its return values are bounded,
//! request-local, never logged here, and never written back to either store.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::CapabilityWorkspaceManager;
use magician::magician_v2::attention_funnel::AttentionScope;

use super::adapter_registry;
use super::assist::content::{distill_chunk_chars, distill_max_chunks};
use super::assist::distill::{default_content_fetchers, ContentFetcher, DistillContext};
use super::channel_observe;
use super::ingest::IngestContext;
use super::store::MailAssistStore;
use super::types::{ChannelLane, MailMessageMeta, MailThreadAnnotation, MailThreadRecord};

pub const ORIGINAL_BODY_MAX_CHARS: usize = 20_000;
pub const ORIGINAL_RESPONSE_MAX_CHARS: usize = 40_000;
pub const ORIGINAL_EVIDENCE_MAX_MESSAGES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMessageKey {
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    pub message_id: String,
    pub internal_date: i64,
}

#[derive(Debug, Clone)]
pub struct ResolvedChannelEvidence {
    pub primary: MailMessageMeta,
    pub messages: Vec<MailMessageMeta>,
    pub thread: Option<MailThreadRecord>,
    pub has_newer: bool,
}

impl ResolvedChannelEvidence {
    pub fn sensitive_suppressed(&self) -> bool {
        self.primary.sensitive_suppressed
            || self
                .thread
                .as_ref()
                .is_some_and(|thread| thread.sensitive_suppressed)
            || self
                .messages
                .iter()
                .any(|message| message.sensitive_suppressed)
    }

    pub fn account_email(&self) -> Option<&str> {
        self.thread
            .as_ref()
            .and_then(|thread| thread.account_email.as_deref())
            .or(self.primary.account_email.as_deref())
    }

    pub fn lane(&self) -> ChannelLane {
        self.thread
            .as_ref()
            .map(|thread| thread.lane)
            .unwrap_or_else(|| {
                channel_observe::account_lane(&self.primary.provider, &self.primary.account_alias)
            })
    }
}

/// One request-local live source body. It is serializable only so an explicit
/// owner-facing HTTP response can return it; callers must not persist or log it.
#[derive(Clone, Serialize)]
pub struct LiveEvidenceMessage {
    pub message_id: String,
    pub subject: Option<String>,
    pub summary: Option<String>,
    pub received_at: i64,
    pub body: Option<String>,
    pub truncated: bool,
    pub attachment_count: usize,
}

#[derive(Clone, Serialize)]
pub struct LiveChannelEvidence {
    pub messages: Vec<LiveEvidenceMessage>,
    pub fetcher_available: bool,
    pub fetched_any: bool,
    pub fetch_failed: bool,
}

impl LiveChannelEvidence {
    pub fn primary(&self, message_id: &str) -> Option<&LiveEvidenceMessage> {
        self.messages
            .iter()
            .find(|message| message.message_id == message_id)
    }
}

#[derive(Clone)]
pub struct ChannelEvidenceResolver {
    workspace_layout: ArtifactV2Workspace,
    store: MailAssistStore,
    fetchers: Arc<Vec<Arc<dyn ContentFetcher>>>,
}

impl ChannelEvidenceResolver {
    pub fn new(workspace_layout: ArtifactV2Workspace, store: MailAssistStore) -> Self {
        let fetchers = default_content_fetchers()
            .into_iter()
            .map(Arc::<dyn ContentFetcher>::from)
            .collect();
        Self {
            workspace_layout,
            store,
            fetchers: Arc::new(fetchers),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_fetchers(
        workspace_layout: ArtifactV2Workspace,
        store: MailAssistStore,
        fetchers: Vec<Arc<dyn ContentFetcher>>,
    ) -> Self {
        Self {
            workspace_layout,
            store,
            fetchers: Arc::new(fetchers),
        }
    }

    /// Resolve one exact scoped message plus the coalesced evidence ids that
    /// fed its current distillation. A message-id collision in another thread
    /// or a changed provider timestamp is rejected rather than substituted.
    pub async fn resolve_exact(
        &self,
        scope: &AttentionScope,
        key: &ChannelMessageKey,
    ) -> Result<Option<ResolvedChannelEvidence>> {
        let Some(primary) = self
            .store
            .get_message(
                &scope.principal,
                &scope.workspace,
                &key.provider,
                &key.account_alias,
                &key.message_id,
            )
            .await?
        else {
            return Ok(None);
        };
        if primary.thread_id != key.thread_id || primary.internal_date != key.internal_date {
            return Ok(None);
        }
        self.resolve_from_primary(scope, primary, &[])
            .await
            .map(Some)
    }

    /// Resolve the exact message batch that a Follow-up annotation used. Old
    /// annotations without an exact evidence id retain the existing fallback
    /// to the newest distilled message in that thread.
    pub async fn resolve_annotation(
        &self,
        scope: &AttentionScope,
        annotation: &MailThreadAnnotation,
    ) -> Result<Option<ResolvedChannelEvidence>> {
        let primary = if let Some(message_id) = annotation.evidence_message_id.as_deref() {
            self.store
                .get_message(
                    &scope.principal,
                    &scope.workspace,
                    &annotation.provider,
                    &annotation.account_alias,
                    message_id,
                )
                .await?
        } else {
            self.store
                .distilled_message_for_thread(
                    &scope.principal,
                    &scope.workspace,
                    &annotation.provider,
                    &annotation.account_alias,
                    &annotation.thread_id,
                )
                .await?
        };
        let Some(primary) = primary else {
            return Ok(None);
        };
        if primary.provider != annotation.provider
            || primary.account_alias != annotation.account_alias
            || primary.thread_id != annotation.thread_id
        {
            return Ok(None);
        }
        let ids = annotation_evidence_message_ids(annotation);
        self.resolve_from_primary(scope, primary, &ids)
            .await
            .map(Some)
    }

    async fn resolve_from_primary(
        &self,
        scope: &AttentionScope,
        primary: MailMessageMeta,
        preferred_ids: &[String],
    ) -> Result<ResolvedChannelEvidence> {
        let stored_ids = self
            .store
            .get_distill_evidence_message_ids(
                &scope.principal,
                &scope.workspace,
                &primary.provider,
                &primary.account_alias,
                &primary.message_id,
            )
            .await?
            .unwrap_or_default();
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for id in std::iter::once(&primary.message_id)
            .chain(preferred_ids.iter())
            .chain(stored_ids.iter())
        {
            let id = id.trim();
            if !id.is_empty() && seen.insert(id.to_string()) {
                ids.push(id.to_string());
            }
            if ids.len() >= ORIGINAL_EVIDENCE_MAX_MESSAGES {
                break;
            }
        }

        let mut messages = Vec::with_capacity(ids.len());
        for id in ids {
            let message = if id == primary.message_id {
                Some(primary.clone())
            } else {
                self.store
                    .get_message(
                        &scope.principal,
                        &scope.workspace,
                        &primary.provider,
                        &primary.account_alias,
                        &id,
                    )
                    .await?
            };
            if let Some(message) = message.filter(|message| {
                message.provider == primary.provider
                    && message.account_alias == primary.account_alias
                    && message.thread_id == primary.thread_id
            }) {
                messages.push(message);
            }
        }
        if messages.is_empty() {
            messages.push(primary.clone());
        }

        let thread = self
            .store
            .get_threads_by_ids(
                &scope.principal,
                &scope.workspace,
                &primary.provider,
                &primary.account_alias,
                std::slice::from_ref(&primary.thread_id),
            )
            .await?
            .into_iter()
            .next();
        let has_newer = self
            .store
            .has_message_after(
                &scope.principal,
                &scope.workspace,
                &primary.provider,
                &primary.account_alias,
                &primary.thread_id,
                primary.internal_date,
            )
            .await?;
        Ok(ResolvedChannelEvidence {
            primary,
            messages,
            thread,
            has_newer,
        })
    }

    /// `Some(false)` means the scoped account is explicitly disabled or a
    /// provider with a connection-status adapter is not ready. `None` means
    /// that provider has no reliable readiness probe; it is not treated as
    /// offline merely because the capability is absent.
    pub async fn connection_status(
        &self,
        scope: &AttentionScope,
        evidence: &ResolvedChannelEvidence,
    ) -> Option<bool> {
        let config = channel_observe::read_channel_observe(
            &self.workspace_layout,
            &scope.principal,
            &scope.workspace,
        )
        .await;
        if config.as_ref().is_some_and(|config| {
            config.channels.iter().any(|entry| {
                channel_observe::channel_to_provider(&entry.channel)
                    == Some(evidence.primary.provider.as_str())
                    && entry.account == evidence.primary.account_alias
                    && !entry.enabled
            })
        }) {
            return Some(false);
        }

        let capabilities = adapter_registry::capabilities_for(&evidence.primary.provider);
        if !capabilities.connection_status {
            return None;
        }
        let config = config.unwrap_or_default();
        let ctx = ingest_context(
            &self.workspace_layout,
            scope,
            config.suppress_sensitive,
            config.history_lookback_days,
        );
        Some(
            adapter_registry::connection_status_for(
                &ctx,
                &evidence.primary.provider,
                &evidence.primary.account_alias,
                evidence.lane(),
            )
            .unwrap_or(false),
        )
    }

    /// Fetch the already-resolved evidence batch live. This performs no store
    /// write and returns no provider error text, body text, or credentials to
    /// logs/telemetry. Per-message and aggregate character limits are both
    /// enforced after provider preparation.
    pub async fn fetch_original(
        &self,
        scope: &AttentionScope,
        evidence: &ResolvedChannelEvidence,
        per_message_max_chars: usize,
        response_max_chars: usize,
    ) -> LiveChannelEvidence {
        let fetcher = self
            .fetchers
            .iter()
            .find(|fetcher| fetcher.provider() == evidence.primary.provider);
        let Some(fetcher) = fetcher else {
            return LiveChannelEvidence {
                messages: metadata_only_messages(&evidence.messages),
                fetcher_available: false,
                fetched_any: false,
                fetch_failed: false,
            };
        };
        if evidence.sensitive_suppressed() {
            return LiveChannelEvidence {
                messages: metadata_only_messages(&evidence.messages),
                fetcher_available: true,
                fetched_any: false,
                fetch_failed: false,
            };
        }

        let config = channel_observe::read_channel_observe(
            &self.workspace_layout,
            &scope.principal,
            &scope.workspace,
        )
        .await
        .unwrap_or_default();
        let ctx = distill_context(&self.workspace_layout, scope);
        let mut remaining = response_max_chars;
        let mut fetched_any = false;
        let mut fetch_failed = false;
        let mut messages = Vec::with_capacity(evidence.messages.len());
        for message in &evidence.messages {
            let allowed = per_message_max_chars.min(remaining);
            if allowed == 0 {
                messages.push(LiveEvidenceMessage {
                    message_id: message.message_id.clone(),
                    subject: message.subject.clone(),
                    summary: message.summary.clone(),
                    received_at: message.internal_date,
                    body: None,
                    truncated: true,
                    attachment_count: 0,
                });
                continue;
            }
            if config.suppress_sensitive && message.sensitive_suppressed {
                messages.push(metadata_only_message(message));
                continue;
            }
            match fetcher.fetch(&ctx, message).await {
                Ok(content) => {
                    fetched_any = true;
                    let raw = content.chunks.join("\n\n").trim().to_string();
                    let raw_chars = raw.chars().count();
                    let body = if raw.is_empty() {
                        None
                    } else {
                        Some(raw.chars().take(allowed).collect::<String>())
                    };
                    let body_chars = body
                        .as_deref()
                        .map(|body| body.chars().count())
                        .unwrap_or(0);
                    remaining = remaining.saturating_sub(body_chars);
                    messages.push(LiveEvidenceMessage {
                        message_id: message.message_id.clone(),
                        subject: message.subject.clone(),
                        summary: message.summary.clone(),
                        received_at: message.internal_date,
                        body,
                        truncated: content.truncated || raw_chars > allowed,
                        attachment_count: content.attachment_count,
                    });
                },
                Err(_) => {
                    fetch_failed = true;
                    messages.push(metadata_only_message(message));
                },
            }
        }
        LiveChannelEvidence {
            messages,
            fetcher_available: true,
            fetched_any,
            fetch_failed,
        }
    }
}

pub fn annotation_evidence_message_ids(annotation: &MailThreadAnnotation) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(primary) = annotation.evidence_message_id.as_deref() {
        ids.push(primary.to_string());
    }
    for evidence_ref in &annotation.evidence_refs {
        let Some(message_id) = evidence_ref.strip_prefix("message:") else {
            continue;
        };
        let message_id = message_id.trim();
        if !message_id.is_empty() && !ids.iter().any(|seen| seen == message_id) {
            ids.push(message_id.to_string());
        }
    }
    ids
}

fn metadata_only_messages(messages: &[MailMessageMeta]) -> Vec<LiveEvidenceMessage> {
    messages.iter().map(metadata_only_message).collect()
}

fn metadata_only_message(message: &MailMessageMeta) -> LiveEvidenceMessage {
    LiveEvidenceMessage {
        message_id: message.message_id.clone(),
        subject: message.subject.clone(),
        summary: message.summary.clone(),
        received_at: message.internal_date,
        body: None,
        truncated: false,
        attachment_count: 0,
    }
}

fn distill_context(
    workspace_layout: &ArtifactV2Workspace,
    scope: &AttentionScope,
) -> DistillContext {
    let auth_root = workspace_layout.capability_auth_root(&scope.principal, &scope.workspace);
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let scope_paths = CapabilityWorkspaceManager::new(workspace_layout.clone(), repo_root)
        .scope_paths(&scope.principal, &scope.workspace);
    DistillContext {
        principal: scope.principal.to_string(),
        workspace: scope.workspace.to_string(),
        workspace_layout: workspace_layout.clone(),
        auth_root,
        scope_paths,
        chunk_chars: distill_chunk_chars(),
        max_chunks: distill_max_chunks(),
    }
}

fn ingest_context(
    workspace_layout: &ArtifactV2Workspace,
    scope: &AttentionScope,
    suppress_sensitive: bool,
    history_lookback_days: u32,
) -> IngestContext {
    let ctx = distill_context(workspace_layout, scope);
    IngestContext {
        principal: ctx.principal,
        workspace: ctx.workspace,
        workspace_layout: ctx.workspace_layout,
        auth_root: ctx.auth_root,
        scope_paths: ctx.scope_paths,
        suppress_sensitive,
        backfill_days: channel_observe::normalize_history_lookback_days(history_lookback_days),
        min_internal_date: 0,
        max_threads: 0,
        ignore_provider_cursor: false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::*;
    use crate::channel_assist::assist::content::DistillContent;
    use crate::channel_assist::types::{
        DistillState, MailRecordOrigin, MessageDirection, MAIL_ASSIST_SCHEMA_VERSION,
    };

    struct TestFetcher;

    #[async_trait]
    impl ContentFetcher for TestFetcher {
        fn provider(&self) -> &'static str {
            "test_provider"
        }

        async fn fetch(
            &self,
            _ctx: &DistillContext,
            message: &MailMessageMeta,
        ) -> Result<DistillContent> {
            Ok(DistillContent {
                chunks: vec![format!("{}:{}", message.message_id, "x".repeat(50))],
                attachment_count: 2,
                ..DistillContent::default()
            })
        }
    }

    fn message(id: &str, at: i64) -> MailMessageMeta {
        MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "test_provider".to_string(),
            account_alias: "account".to_string(),
            account_email: None,
            thread_id: "thread".to_string(),
            message_id: id.to_string(),
            provider_cursor: None,
            label_ids: Vec::new(),
            subject: Some(format!("Subject {id}")),
            from_name: None,
            from_address: None,
            to_domains: Vec::new(),
            cc_domains: Vec::new(),
            internal_date: at,
            observed_at: at,
            direction: Some(MessageDirection::Inbound),
            summary: None,
            intent: None,
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: DistillState::Pending,
            distill_attempts: 0,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
        }
    }

    #[tokio::test]
    async fn exact_resolution_keeps_coalesced_evidence_and_never_substitutes_latest() {
        let tmp = tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let scope = AttentionScope {
            principal: "p".to_string(),
            workspace: "w".to_string(),
        };
        let mut first = message("m1", 100);
        let second = message("m2", 200);
        let latest = message("m3", 300);
        first.summary = Some("coalesced safe summary".to_string());
        store
            .append_messages(
                &scope.principal,
                &scope.workspace,
                vec![first, second, latest],
            )
            .await
            .unwrap();
        store
            .set_distill_result_with_evidence_ids(
                &scope.principal,
                &scope.workspace,
                "test_provider",
                "account",
                "m1",
                "coalesced safe summary",
                "fyi",
                false,
                None,
                &["m1".to_string(), "m2".to_string()],
            )
            .await
            .unwrap();
        let resolver =
            ChannelEvidenceResolver::with_fetchers(layout, store, vec![Arc::new(TestFetcher)]);
        let resolved = resolver
            .resolve_exact(
                &scope,
                &ChannelMessageKey {
                    provider: "test_provider".to_string(),
                    account_alias: "account".to_string(),
                    thread_id: "thread".to_string(),
                    message_id: "m1".to_string(),
                    internal_date: 100,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.primary.message_id, "m1");
        assert_eq!(
            resolved
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["m1", "m2"]
        );
        assert!(resolved.has_newer);
    }

    #[tokio::test]
    async fn original_fetch_enforces_per_message_and_aggregate_bounds() {
        let tmp = tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let resolver =
            ChannelEvidenceResolver::with_fetchers(layout, store, vec![Arc::new(TestFetcher)]);
        let scope = AttentionScope {
            principal: "p".to_string(),
            workspace: "w".to_string(),
        };
        let primary = message("m1", 100);
        let evidence = ResolvedChannelEvidence {
            primary: primary.clone(),
            messages: vec![primary, message("m2", 200)],
            thread: None,
            has_newer: false,
        };
        let original = resolver.fetch_original(&scope, &evidence, 20, 30).await;
        let lengths = original
            .messages
            .iter()
            .map(|message| {
                message
                    .body
                    .as_deref()
                    .map(|body| body.chars().count())
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();
        assert_eq!(lengths, vec![20, 10]);
        assert_eq!(lengths.iter().sum::<usize>(), 30);
        assert!(original.messages.iter().all(|message| message.truncated));
        assert!(original.fetched_any);
    }

    #[tokio::test]
    async fn suppressed_evidence_never_calls_live_fetch() {
        let tmp = tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let resolver =
            ChannelEvidenceResolver::with_fetchers(layout, store, vec![Arc::new(TestFetcher)]);
        let scope = AttentionScope {
            principal: "p".to_string(),
            workspace: "w".to_string(),
        };
        let mut primary = message("m1", 100);
        primary.sensitive_suppressed = true;
        let evidence = ResolvedChannelEvidence {
            primary: primary.clone(),
            messages: vec![primary],
            thread: None,
            has_newer: false,
        };
        let original = resolver.fetch_original(&scope, &evidence, 20, 30).await;
        assert!(!original.fetched_any);
        assert!(original.messages[0].body.is_none());
    }
}
