//! Read-only source interaction adapters for resurfacing cards.
//!
//! This boundary is intentionally separate from [`magician::magician_v2::attention::resurfacing::sources::ResurfacingSource`]:
//! corpus adapters ingest safe summaries, while interaction adapters resolve a
//! selected candidate back to its current scoped source and, only on the
//! explicit original endpoint, request bounded live content.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use crate::channel_assist::adapter_registry::{
    capabilities_for as channel_capabilities_for, thread_url_for, ChannelThreadRef,
};
use crate::channel_assist::channel::ChannelAssistStore;
use crate::channel_assist::live_content::{
    ChannelEvidenceResolver, ChannelMessageKey, LiveEvidenceMessage, ResolvedChannelEvidence,
    ORIGINAL_BODY_MAX_CHARS, ORIGINAL_RESPONSE_MAX_CHARS,
};
use magician::magician_v2::agents::AgentMemoryResolver;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef, V3ReadApi};
use magician::magician_v2::attention_funnel::AttentionScope;
use magician::magician_v2::content_sources::canonicalize_http_url;
use magician::magician_v2::notes::NotesSettingsStore;
use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

use magician::magician_v2::attention::resurfacing::interaction::{
    ResurfacingActionKind, ResurfacingRecommendation,
};
use magician::magician_v2::attention::resurfacing::source_refs::parse_comm_source_ref;
use magician::magician_v2::attention::resurfacing::types::{Candidate, CandidateState, SourceKind};

const SAFE_SOURCE_TEXT_MAX_CHARS: usize = 20_000;
pub const RESURFACING_DEEP_SUMMARY_OPERATION: &str = "resurfacing_deep_summary";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResurfacingSourceStatus {
    Available,
    NewerAvailable,
    Stale,
    Offline,
    Deleted,
    Suppressed,
    Unsupported,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResurfacingSideEffect {
    None,
    CreatesTask,
    CreatesReminder,
    CreatesShareDraft,
    CreatesMemoryCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResurfacingActionCapability {
    pub kind: ResurfacingActionKind,
    pub label: &'static str,
    pub requires_input: bool,
    pub side_effect: ResurfacingSideEffect,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResurfacingSourceMetadata {
    Comm {
        provider: String,
        account_alias: String,
        account_email: Option<String>,
        thread_id: String,
        message_id: String,
        received_at: i64,
        evidence_message_ids: Vec<String>,
    },
    Task {
        task_id: String,
        status: String,
        outcome: Option<String>,
        updated_at: String,
    },
    Memory {
        tier: String,
        key: String,
        updated_at: Option<String>,
    },
    Web {
        url: String,
    },
    Note {
        provider: String,
        path: String,
    },
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResurfacingOriginalContent {
    Comm {
        message_id: String,
        subject: Option<String>,
        summary: Option<String>,
        received_at: i64,
        body: Option<String>,
        evidence_messages: Vec<LiveEvidenceMessage>,
    },
    Task {
        task_id: String,
        title: String,
        status: String,
        outcome: Option<String>,
    },
    Memory {
        tier: String,
        key: String,
        summary: String,
        updated_at: Option<String>,
    },
    Web {
        url: String,
        title: String,
        summary: String,
    },
    Note {
        provider: String,
        path: String,
        markdown: String,
    },
}

#[derive(Clone, Serialize)]
pub struct ResolvedResurfacingDetail {
    pub status: ResurfacingSourceStatus,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub source_revision: Option<String>,
    pub source_updated: bool,
    pub has_newer: bool,
    pub source_route: Option<String>,
    pub open_url: Option<String>,
    pub source: Option<ResurfacingSourceMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<ResurfacingOriginalContent>,
    #[serde(default)]
    pub actions: Vec<ResurfacingActionCapability>,
}

impl ResolvedResurfacingDetail {
    fn unavailable(status: ResurfacingSourceStatus) -> Self {
        Self {
            status,
            title: None,
            summary: None,
            source_revision: None,
            source_updated: false,
            has_newer: false,
            source_route: None,
            open_url: None,
            source: None,
            original: None,
            actions: Vec::new(),
        }
    }
}

#[async_trait]
pub trait ResurfacingInteractionAdapter: Send + Sync {
    fn source_kind(&self) -> SourceKind;

    async fn resolve_detail(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail>;

    async fn resolve_original(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        self.resolve_detail(scope, candidate).await
    }

    fn capabilities(
        &self,
        candidate: &Candidate,
        detail: Option<&ResolvedResurfacingDetail>,
    ) -> Vec<ResurfacingActionCapability>;
}

#[derive(Clone)]
pub struct ResurfacingInteractionRegistry {
    adapters: Arc<Vec<Arc<dyn ResurfacingInteractionAdapter>>>,
    operation_router: Option<Arc<OperationLlmRouter>>,
    rich_briefs_enabled: bool,
    source_details_enabled: bool,
    contextual_actions_enabled: bool,
    recommendations_enabled: bool,
    recommendation_min_confidence: f32,
}

impl ResurfacingInteractionRegistry {
    pub fn new(
        workspace_layout: ArtifactV2Workspace,
        channel_store: ChannelAssistStore,
        artifact_service: Arc<ArtifactV2Service>,
        memory_resolver: AgentMemoryResolver,
        operation_router: Option<Arc<OperationLlmRouter>>,
        rich_briefs_enabled: bool,
        source_details_enabled: bool,
        contextual_actions_enabled: bool,
        recommendations_enabled: bool,
        recommendation_min_confidence: f32,
    ) -> Self {
        let notes_store = NotesSettingsStore::with_workspace_layout(workspace_layout.clone());
        let evidence = ChannelEvidenceResolver::new(workspace_layout, channel_store);
        Self {
            adapters: Arc::new(vec![
                Arc::new(CommInteractionAdapter::new(evidence)),
                Arc::new(TaskInteractionAdapter::new(artifact_service)),
                Arc::new(MemoryInteractionAdapter::new(memory_resolver)),
                Arc::new(NoteInteractionAdapter::new(notes_store)),
                Arc::new(WebInteractionAdapter),
            ]),
            operation_router,
            rich_briefs_enabled,
            source_details_enabled,
            contextual_actions_enabled,
            recommendations_enabled,
            recommendation_min_confidence: recommendation_min_confidence.clamp(0.0, 1.0),
        }
    }

    pub fn from_adapters(adapters: Vec<Arc<dyn ResurfacingInteractionAdapter>>) -> Self {
        Self {
            adapters: Arc::new(adapters),
            operation_router: None,
            rich_briefs_enabled: true,
            source_details_enabled: true,
            contextual_actions_enabled: true,
            recommendations_enabled: true,
            recommendation_min_confidence: 0.65,
        }
    }

    fn adapter(&self, source_kind: SourceKind) -> Option<&dyn ResurfacingInteractionAdapter> {
        self.adapters
            .iter()
            .find(|adapter| adapter.source_kind() == source_kind)
            .map(Arc::as_ref)
    }

    pub async fn resolve_detail(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        if !self.source_details_enabled {
            let mut detail =
                ResolvedResurfacingDetail::unavailable(ResurfacingSourceStatus::Unavailable);
            detail.actions = self.capabilities(candidate, Some(&detail));
            return Ok(detail);
        }
        let Some(adapter) = self.adapter(candidate.source_kind) else {
            let mut detail =
                ResolvedResurfacingDetail::unavailable(ResurfacingSourceStatus::Unsupported);
            detail.actions = self.capabilities(candidate, Some(&detail));
            return Ok(detail);
        };
        let mut detail = adapter.resolve_detail(scope, candidate).await?;
        detail.actions = self.capabilities(candidate, Some(&detail));
        Ok(detail)
    }

    pub async fn resolve_original(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        if !self.source_details_enabled {
            let mut detail =
                ResolvedResurfacingDetail::unavailable(ResurfacingSourceStatus::Unavailable);
            detail.actions = self.capabilities(candidate, Some(&detail));
            return Ok(detail);
        }
        let Some(adapter) = self.adapter(candidate.source_kind) else {
            let mut detail =
                ResolvedResurfacingDetail::unavailable(ResurfacingSourceStatus::Unsupported);
            detail.actions = self.capabilities(candidate, Some(&detail));
            return Ok(detail);
        };
        let mut detail = adapter.resolve_original(scope, candidate).await?;
        detail.actions = self.capabilities(candidate, Some(&detail));
        Ok(detail)
    }

    pub fn capabilities(
        &self,
        candidate: &Candidate,
        detail: Option<&ResolvedResurfacingDetail>,
    ) -> Vec<ResurfacingActionCapability> {
        let capabilities = self
            .adapter(candidate.source_kind)
            .map(|adapter| adapter.capabilities(candidate, detail))
            .unwrap_or_else(|| static_capabilities(candidate, detail));
        let mut capabilities = apply_runtime_capability_policy(
            candidate,
            capabilities,
            self.operation_router.as_deref(),
            self.contextual_actions_enabled,
        );
        if !self.source_details_enabled {
            capabilities.retain(|action| action.kind == ResurfacingActionKind::ViewDetails);
        }
        capabilities
    }

    pub fn rich_briefs_enabled(&self) -> bool {
        self.rich_briefs_enabled
    }

    pub fn recommendation_is_visible(
        &self,
        candidate: &Candidate,
        capabilities: &[ResurfacingActionCapability],
        recommendation: &ResurfacingRecommendation,
    ) -> bool {
        self.recommendations_enabled
            && candidate.state == CandidateState::Surfaced
            && recommendation.confidence.is_finite()
            && recommendation.confidence >= self.recommendation_min_confidence
            && recommendation.content_revision == candidate.content_revision
            && capabilities
                .iter()
                .any(|capability| capability.kind == recommendation.kind)
    }

    pub fn recommendations_enabled(&self) -> bool {
        self.recommendations_enabled
    }
}

/// Metadata-only capabilities used in the curator prompt. This applies the
/// same rollout/local-provider policy as the live registry without resolving a
/// source or fetching original content.
pub fn metadata_capabilities(
    candidate: &Candidate,
    router: Option<&OperationLlmRouter>,
    contextual_actions_enabled: bool,
) -> Vec<ResurfacingActionCapability> {
    apply_runtime_capability_policy(
        candidate,
        static_capabilities(candidate, None),
        router,
        contextual_actions_enabled,
    )
}

fn apply_runtime_capability_policy(
    candidate: &Candidate,
    mut capabilities: Vec<ResurfacingActionCapability>,
    router: Option<&OperationLlmRouter>,
    contextual_actions_enabled: bool,
) -> Vec<ResurfacingActionCapability> {
    if !contextual_actions_enabled {
        capabilities.retain(|action| {
            matches!(
                action.kind,
                ResurfacingActionKind::ViewDetails
                    | ResurfacingActionKind::OpenSource
                    | ResurfacingActionKind::ShowOriginal
            )
        });
        return capabilities;
    }
    if candidate.source_kind == SourceKind::Comm
        && capabilities
            .iter()
            .any(|action| action.kind == ResurfacingActionKind::ShowOriginal)
        && deep_summary_local_available(router)
    {
        capabilities.push(capability(
            ResurfacingActionKind::SummarizeDeeper,
            "Summarize deeper",
            false,
            ResurfacingSideEffect::None,
        ));
    }
    capabilities
}

pub fn deep_summary_local_available(router: Option<&OperationLlmRouter>) -> bool {
    // Shared policy-aware core: available when explicitly bound to a profile
    // the current privacy.processing.mode permits (local ⇒ Ollama arm,
    // cloud ⇒ when_cloud arm).
    magician::magician_v2::llm_dispatch_seam::resolve_local_provider_for_operation(
        router,
        RESURFACING_DEEP_SUMMARY_OPERATION,
    )
    .is_ok()
}

/// Capability-only fallback used by list handlers in isolated tests where the
/// runtime registry is intentionally not mounted. It performs no source read.
pub fn static_capabilities(
    candidate: &Candidate,
    detail: Option<&ResolvedResurfacingDetail>,
) -> Vec<ResurfacingActionCapability> {
    match candidate.source_kind {
        SourceKind::Comm => comm_capabilities(candidate, detail),
        SourceKind::Task if candidate.source_ref.trim().is_empty() => vec![view_details()],
        SourceKind::Task => task_capabilities(detail),
        SourceKind::Memory if parse_memory_source_ref(&candidate.source_ref).is_none() => {
            vec![view_details()]
        },
        SourceKind::Memory => memory_capabilities(detail),
        SourceKind::Note => note_capabilities(detail),
        SourceKind::Web => web_capabilities(candidate, detail),
        SourceKind::Episode | SourceKind::Calendar => vec![view_details()],
    }
}

/// Fail-closed list fallback when the runtime interaction registry is absent.
/// It may expose source reads, but never advertises a contextual side effect
/// whose executor/rollout state cannot be verified.
pub fn static_read_capabilities(
    candidate: &Candidate,
    detail: Option<&ResolvedResurfacingDetail>,
) -> Vec<ResurfacingActionCapability> {
    static_capabilities(candidate, detail)
        .into_iter()
        .filter(|action| {
            matches!(
                action.kind,
                ResurfacingActionKind::ViewDetails
                    | ResurfacingActionKind::OpenSource
                    | ResurfacingActionKind::ShowOriginal
            )
        })
        .collect()
}

#[derive(Clone)]
pub struct CommInteractionAdapter {
    evidence: ChannelEvidenceResolver,
}

impl CommInteractionAdapter {
    pub fn new(evidence: ChannelEvidenceResolver) -> Self {
        Self { evidence }
    }

    async fn resolve_metadata(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<(ResolvedResurfacingDetail, Option<ResolvedChannelEvidence>)> {
        let Some(parsed) = parse_comm_source_ref(&candidate.source_ref) else {
            return Ok((
                ResolvedResurfacingDetail::unavailable(ResurfacingSourceStatus::Unavailable),
                None,
            ));
        };
        let key = ChannelMessageKey {
            provider: parsed.provider.clone(),
            account_alias: parsed.account_alias.clone(),
            thread_id: parsed.thread_id.clone(),
            message_id: parsed.message_id.clone(),
            internal_date: parsed.internal_date,
        };
        let Some(evidence) = self.evidence.resolve_exact(scope, &key).await? else {
            return Ok((
                ResolvedResurfacingDetail::unavailable(ResurfacingSourceStatus::Deleted),
                None,
            ));
        };
        if evidence.sensitive_suppressed() {
            return Ok((
                ResolvedResurfacingDetail::unavailable(ResurfacingSourceStatus::Suppressed),
                Some(evidence),
            ));
        }

        let connected = self.evidence.connection_status(scope, &evidence).await;
        let source_revision = evidence
            .primary
            .distill_revision
            .map(|value| value.to_string());
        let source_updated = revision_changed(
            candidate.content_revision.as_deref(),
            source_revision.as_deref(),
        );
        let status = if connected == Some(false) {
            ResurfacingSourceStatus::Offline
        } else if source_updated {
            ResurfacingSourceStatus::Stale
        } else if evidence.has_newer {
            ResurfacingSourceStatus::NewerAvailable
        } else {
            ResurfacingSourceStatus::Available
        };
        let open_url = thread_url_for(ChannelThreadRef {
            provider: &parsed.provider,
            account_alias: &parsed.account_alias,
            account_email: evidence.account_email(),
            thread_id: &parsed.thread_id,
        });
        let title = current_comm_title(&evidence).or_else(|| Some(candidate.title.clone()));
        let summary = evidence
            .primary
            .summary
            .clone()
            .or_else(|| Some(candidate.content_digest.clone()));
        let source = Some(ResurfacingSourceMetadata::Comm {
            provider: parsed.provider,
            account_alias: parsed.account_alias,
            account_email: evidence.account_email().map(str::to_string),
            thread_id: parsed.thread_id,
            message_id: parsed.message_id,
            received_at: evidence.primary.internal_date,
            evidence_message_ids: evidence
                .messages
                .iter()
                .map(|message| message.message_id.clone())
                .collect(),
        });
        Ok((
            ResolvedResurfacingDetail {
                status,
                title,
                summary,
                source_revision,
                source_updated,
                has_newer: evidence.has_newer,
                source_route: open_url.clone(),
                open_url,
                source,
                original: None,
                actions: Vec::new(),
            },
            Some(evidence),
        ))
    }
}

#[async_trait]
impl ResurfacingInteractionAdapter for CommInteractionAdapter {
    fn source_kind(&self) -> SourceKind {
        SourceKind::Comm
    }

    async fn resolve_detail(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        self.resolve_metadata(scope, candidate)
            .await
            .map(|(detail, _)| detail)
    }

    async fn resolve_original(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        let (mut detail, evidence) = self.resolve_metadata(scope, candidate).await?;
        let Some(evidence) = evidence else {
            return Ok(detail);
        };
        if matches!(
            detail.status,
            ResurfacingSourceStatus::Suppressed
                | ResurfacingSourceStatus::Deleted
                | ResurfacingSourceStatus::Offline
                | ResurfacingSourceStatus::Unavailable
        ) {
            return Ok(detail);
        }
        if !channel_capabilities_for(&evidence.primary.provider).content_fetch {
            detail.status = ResurfacingSourceStatus::Unsupported;
            return Ok(detail);
        }
        let original = self
            .evidence
            .fetch_original(
                scope,
                &evidence,
                ORIGINAL_BODY_MAX_CHARS,
                ORIGINAL_RESPONSE_MAX_CHARS,
            )
            .await;
        if !original.fetcher_available {
            detail.status = ResurfacingSourceStatus::Unsupported;
            return Ok(detail);
        }
        if original.fetch_failed && !original.fetched_any {
            detail.status = ResurfacingSourceStatus::Unavailable;
            return Ok(detail);
        }
        let primary = original.primary(&evidence.primary.message_id).cloned();
        detail.original = Some(ResurfacingOriginalContent::Comm {
            message_id: evidence.primary.message_id.clone(),
            subject: evidence.primary.subject.clone(),
            summary: evidence.primary.summary.clone(),
            received_at: evidence.primary.internal_date,
            body: primary.and_then(|message| message.body),
            evidence_messages: original.messages,
        });
        Ok(detail)
    }

    fn capabilities(
        &self,
        candidate: &Candidate,
        detail: Option<&ResolvedResurfacingDetail>,
    ) -> Vec<ResurfacingActionCapability> {
        comm_capabilities(candidate, detail)
    }
}

#[derive(Clone)]
pub struct TaskInteractionAdapter {
    service: Arc<ArtifactV2Service>,
}

impl TaskInteractionAdapter {
    pub fn new(service: Arc<ArtifactV2Service>) -> Self {
        Self { service }
    }

    async fn resolve(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
        include_original: bool,
    ) -> Result<ResolvedResurfacingDetail> {
        let task_scope = ScopeRef::system_internal_unauthenticated(
            &scope.principal.to_string(),
            &scope.workspace.to_string(),
        );
        let task = self
            .service
            .list_tasks(&task_scope)
            .await?
            .into_iter()
            .find(|task| task.id == candidate.source_ref);
        let Some(task) = task else {
            return Ok(ResolvedResurfacingDetail::unavailable(
                ResurfacingSourceStatus::Deleted,
            ));
        };
        let outcome = task
            .completion_summary
            .clone()
            .or(task.completion_outcome.clone());
        let current_digest = outcome.clone().unwrap_or_default();
        let source_updated = task.title.trim() != candidate.title.trim()
            || current_digest.trim() != candidate.content_digest.trim();
        let route = task_route(&task.id);
        Ok(ResolvedResurfacingDetail {
            status: if source_updated {
                ResurfacingSourceStatus::Stale
            } else {
                ResurfacingSourceStatus::Available
            },
            title: Some(task.title.clone()),
            summary: outcome.clone(),
            source_revision: Some(task.updated_at.clone()),
            source_updated,
            has_newer: false,
            source_route: Some(route.clone()),
            open_url: None,
            source: Some(ResurfacingSourceMetadata::Task {
                task_id: task.id.clone(),
                status: task.status.clone(),
                outcome: outcome.clone(),
                updated_at: task.updated_at.clone(),
            }),
            original: include_original.then(|| ResurfacingOriginalContent::Task {
                task_id: task.id,
                title: task.title,
                status: task.status,
                outcome,
            }),
            actions: Vec::new(),
        })
    }
}

#[async_trait]
impl ResurfacingInteractionAdapter for TaskInteractionAdapter {
    fn source_kind(&self) -> SourceKind {
        SourceKind::Task
    }

    async fn resolve_detail(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        self.resolve(scope, candidate, false).await
    }

    async fn resolve_original(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        self.resolve(scope, candidate, true).await
    }

    fn capabilities(
        &self,
        _candidate: &Candidate,
        detail: Option<&ResolvedResurfacingDetail>,
    ) -> Vec<ResurfacingActionCapability> {
        task_capabilities(detail)
    }
}

#[derive(Debug, Clone)]
pub struct MemoryInteractionAdapter {
    resolver: AgentMemoryResolver,
}

impl MemoryInteractionAdapter {
    pub fn new(resolver: AgentMemoryResolver) -> Self {
        Self { resolver }
    }

    async fn resolve(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
        include_original: bool,
    ) -> Result<ResolvedResurfacingDetail> {
        let Some((tier, key)) = parse_memory_source_ref(&candidate.source_ref) else {
            return Ok(ResolvedResurfacingDetail::unavailable(
                ResurfacingSourceStatus::Unavailable,
            ));
        };
        let service = self
            .resolver
            .resolve_for_scope(&scope.principal, &scope.workspace)?;
        let knowledge = service.load_user_knowledge().await?;
        let entry = find_memory_entry(&knowledge, &tier, &key);
        let Some(entry) = entry else {
            return Ok(ResolvedResurfacingDetail::unavailable(
                ResurfacingSourceStatus::Deleted,
            ));
        };
        if magician_vector_index::memory_temperature::candidate_metadata_has_superseded_lifecycle(
            entry,
        ) || magician::magician_v2::agents::memory_lifecycle::state(entry) == "unresolved"
        {
            return Ok(ResolvedResurfacingDetail::unavailable(
                ResurfacingSourceStatus::Stale,
            ));
        }
        let summary = bounded_source_text(memory_entry_text(entry));
        let updated_at = entry
            .get("updated_at")
            .and_then(Value::as_str)
            .map(str::to_string);
        let source_updated = summary.trim() != candidate.content_digest.trim();
        let route = memory_route(
            &tier,
            entry.get("key").and_then(Value::as_str).unwrap_or(&key),
        );
        Ok(ResolvedResurfacingDetail {
            status: if source_updated {
                ResurfacingSourceStatus::Stale
            } else {
                ResurfacingSourceStatus::Available
            },
            title: Some(candidate.title.clone()),
            summary: Some(summary.clone()),
            source_revision: updated_at.clone(),
            source_updated,
            has_newer: false,
            source_route: Some(route),
            open_url: None,
            source: Some(ResurfacingSourceMetadata::Memory {
                tier: tier.clone(),
                key: key.clone(),
                updated_at: updated_at.clone(),
            }),
            original: include_original.then(|| ResurfacingOriginalContent::Memory {
                tier,
                key,
                summary,
                updated_at,
            }),
            actions: Vec::new(),
        })
    }
}

#[async_trait]
impl ResurfacingInteractionAdapter for MemoryInteractionAdapter {
    fn source_kind(&self) -> SourceKind {
        SourceKind::Memory
    }

    async fn resolve_detail(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        self.resolve(scope, candidate, false).await
    }

    async fn resolve_original(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        self.resolve(scope, candidate, true).await
    }

    fn capabilities(
        &self,
        _candidate: &Candidate,
        detail: Option<&ResolvedResurfacingDetail>,
    ) -> Vec<ResurfacingActionCapability> {
        memory_capabilities(detail)
    }
}

#[derive(Debug, Clone)]
pub struct NoteInteractionAdapter {
    store: NotesSettingsStore,
}

impl NoteInteractionAdapter {
    pub fn new(store: NotesSettingsStore) -> Self {
        Self { store }
    }

    async fn resolve(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
        include_original: bool,
    ) -> Result<ResolvedResurfacingDetail> {
        let Some(note) = self
            .store
            .read_observation_note(&scope.principal, &scope.workspace, &candidate.source_ref)
            .await?
        else {
            return Ok(ResolvedResurfacingDetail::unavailable(
                ResurfacingSourceStatus::Deleted,
            ));
        };
        let source_updated =
            candidate.content_revision.as_deref() != Some(note.content_hash.as_str());
        let status = if source_updated {
            ResurfacingSourceStatus::NewerAvailable
        } else {
            ResurfacingSourceStatus::Available
        };
        let summary = bounded_source_text(note.markdown.clone());
        Ok(ResolvedResurfacingDetail {
            status,
            title: Some(note.title.clone()),
            summary: Some(summary),
            source_revision: Some(note.content_hash.clone()),
            source_updated,
            has_newer: source_updated,
            source_route: None,
            open_url: note.open_url.clone(),
            source: Some(ResurfacingSourceMetadata::Note {
                provider: note.provider.clone(),
                path: note.relative_path.clone(),
            }),
            original: include_original.then(|| ResurfacingOriginalContent::Note {
                provider: note.provider,
                path: note.relative_path,
                markdown: bounded_source_text(note.markdown),
            }),
            actions: Vec::new(),
        })
    }
}

#[async_trait]
impl ResurfacingInteractionAdapter for NoteInteractionAdapter {
    fn source_kind(&self) -> SourceKind {
        SourceKind::Note
    }

    async fn resolve_detail(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        self.resolve(scope, candidate, false).await
    }

    async fn resolve_original(
        &self,
        scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        self.resolve(scope, candidate, true).await
    }

    fn capabilities(
        &self,
        _candidate: &Candidate,
        detail: Option<&ResolvedResurfacingDetail>,
    ) -> Vec<ResurfacingActionCapability> {
        note_capabilities(detail)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WebInteractionAdapter;

impl WebInteractionAdapter {
    fn resolve(&self, candidate: &Candidate, include_original: bool) -> ResolvedResurfacingDetail {
        let Ok(url) = canonicalize_http_url(&candidate.source_ref) else {
            return ResolvedResurfacingDetail::unavailable(ResurfacingSourceStatus::Unavailable);
        };
        let title = candidate.title.clone();
        let summary = bounded_source_text(candidate.content_digest.clone());
        ResolvedResurfacingDetail {
            status: ResurfacingSourceStatus::Available,
            title: Some(title.clone()),
            summary: Some(summary.clone()),
            source_revision: candidate.content_revision.clone(),
            source_updated: false,
            has_newer: false,
            source_route: None,
            open_url: Some(url.clone()),
            source: Some(ResurfacingSourceMetadata::Web { url: url.clone() }),
            original: include_original.then(|| ResurfacingOriginalContent::Web {
                url,
                title,
                summary,
            }),
            actions: Vec::new(),
        }
    }
}

#[async_trait]
impl ResurfacingInteractionAdapter for WebInteractionAdapter {
    fn source_kind(&self) -> SourceKind {
        SourceKind::Web
    }

    async fn resolve_detail(
        &self,
        _scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        Ok(self.resolve(candidate, false))
    }

    async fn resolve_original(
        &self,
        _scope: &AttentionScope,
        candidate: &Candidate,
    ) -> Result<ResolvedResurfacingDetail> {
        Ok(self.resolve(candidate, true))
    }

    fn capabilities(
        &self,
        candidate: &Candidate,
        detail: Option<&ResolvedResurfacingDetail>,
    ) -> Vec<ResurfacingActionCapability> {
        web_capabilities(candidate, detail)
    }
}

fn comm_capabilities(
    candidate: &Candidate,
    detail: Option<&ResolvedResurfacingDetail>,
) -> Vec<ResurfacingActionCapability> {
    if detail.is_some_and(|detail| detail.status == ResurfacingSourceStatus::Suppressed) {
        return Vec::new();
    }
    let mut actions = vec![view_details()];
    let Some(parsed) = parse_comm_source_ref(&candidate.source_ref) else {
        return actions;
    };
    if detail.is_some_and(|detail| {
        matches!(
            detail.status,
            ResurfacingSourceStatus::Unavailable | ResurfacingSourceStatus::Unsupported
        )
    }) {
        return actions;
    }
    let provider_capabilities = channel_capabilities_for(&parsed.provider);
    let source_unavailable = detail.is_some_and(|detail| {
        matches!(
            detail.status,
            ResurfacingSourceStatus::Deleted
                | ResurfacingSourceStatus::Unavailable
                | ResurfacingSourceStatus::Unsupported
        )
    });
    if !source_unavailable
        && provider_capabilities.deep_link
        && detail.map_or(true, |detail| detail.open_url.is_some())
    {
        actions.push(capability(
            ResurfacingActionKind::OpenSource,
            "Open source",
            false,
            ResurfacingSideEffect::None,
        ));
    }
    if !source_unavailable
        && !detail.is_some_and(|detail| detail.status == ResurfacingSourceStatus::Offline)
        && provider_capabilities.content_fetch
    {
        actions.push(capability(
            ResurfacingActionKind::ShowOriginal,
            "Original",
            false,
            ResurfacingSideEffect::None,
        ));
    }
    actions.extend([
        ask_presto(),
        create_task(),
        create_reminder(),
        capability(
            ResurfacingActionKind::Share,
            "Share",
            true,
            ResurfacingSideEffect::CreatesShareDraft,
        ),
        capability(
            ResurfacingActionKind::SaveToMemory,
            "Save to memory",
            true,
            ResurfacingSideEffect::CreatesMemoryCandidate,
        ),
    ]);
    actions
}

fn task_capabilities(
    detail: Option<&ResolvedResurfacingDetail>,
) -> Vec<ResurfacingActionCapability> {
    let mut actions = vec![view_details()];
    if detail.is_some_and(|detail| {
        matches!(
            detail.status,
            ResurfacingSourceStatus::Unavailable | ResurfacingSourceStatus::Unsupported
        )
    }) {
        return actions;
    }
    if !detail.is_some_and(|detail| detail.status == ResurfacingSourceStatus::Deleted) {
        actions.push(capability(
            ResurfacingActionKind::OpenSource,
            "Open task",
            false,
            ResurfacingSideEffect::None,
        ));
    }
    actions.extend([ask_presto(), create_task(), create_reminder()]);
    actions
}

fn memory_capabilities(
    detail: Option<&ResolvedResurfacingDetail>,
) -> Vec<ResurfacingActionCapability> {
    let mut actions = vec![view_details()];
    if detail.is_some_and(|detail| {
        matches!(
            detail.status,
            ResurfacingSourceStatus::Unavailable | ResurfacingSourceStatus::Unsupported
        )
    }) {
        return actions;
    }
    if !detail.is_some_and(|detail| detail.status == ResurfacingSourceStatus::Deleted) {
        actions.push(capability(
            ResurfacingActionKind::OpenSource,
            "Open memory",
            false,
            ResurfacingSideEffect::None,
        ));
    }
    actions.extend([ask_presto(), create_task(), create_reminder()]);
    actions
}

fn web_capabilities(
    candidate: &Candidate,
    detail: Option<&ResolvedResurfacingDetail>,
) -> Vec<ResurfacingActionCapability> {
    let mut actions = vec![view_details()];
    let unavailable = detail.is_some_and(|detail| {
        matches!(
            detail.status,
            ResurfacingSourceStatus::Deleted
                | ResurfacingSourceStatus::Unavailable
                | ResurfacingSourceStatus::Unsupported
        )
    });
    if !unavailable && canonicalize_http_url(&candidate.source_ref).is_ok() {
        actions.push(capability(
            ResurfacingActionKind::OpenSource,
            "Open source",
            false,
            ResurfacingSideEffect::None,
        ));
    }
    if !unavailable {
        actions.extend([
            ask_presto(),
            create_task(),
            create_reminder(),
            capability(
                ResurfacingActionKind::Share,
                "Share",
                true,
                ResurfacingSideEffect::CreatesShareDraft,
            ),
            capability(
                ResurfacingActionKind::SaveToMemory,
                "Save to memory",
                true,
                ResurfacingSideEffect::CreatesMemoryCandidate,
            ),
        ]);
    }
    actions
}

fn note_capabilities(
    detail: Option<&ResolvedResurfacingDetail>,
) -> Vec<ResurfacingActionCapability> {
    let mut actions = vec![view_details()];
    let unavailable = detail.is_some_and(|detail| {
        matches!(
            detail.status,
            ResurfacingSourceStatus::Deleted
                | ResurfacingSourceStatus::Unavailable
                | ResurfacingSourceStatus::Unsupported
        )
    });
    if !unavailable && detail.and_then(|detail| detail.open_url.as_ref()).is_some() {
        actions.push(capability(
            ResurfacingActionKind::OpenSource,
            "Open note",
            false,
            ResurfacingSideEffect::None,
        ));
    }
    if !unavailable {
        actions.extend([ask_presto(), create_task(), create_reminder()]);
    }
    actions
}

fn view_details() -> ResurfacingActionCapability {
    capability(
        ResurfacingActionKind::ViewDetails,
        "Details",
        false,
        ResurfacingSideEffect::None,
    )
}

fn ask_presto() -> ResurfacingActionCapability {
    capability(
        ResurfacingActionKind::AskPresto,
        "Ask Presto",
        false,
        ResurfacingSideEffect::None,
    )
}

fn create_task() -> ResurfacingActionCapability {
    capability(
        ResurfacingActionKind::CreateTask,
        "Create task",
        true,
        ResurfacingSideEffect::CreatesTask,
    )
}

fn create_reminder() -> ResurfacingActionCapability {
    capability(
        ResurfacingActionKind::CreateReminder,
        "Create reminder",
        true,
        ResurfacingSideEffect::CreatesReminder,
    )
}

fn capability(
    kind: ResurfacingActionKind,
    label: &'static str,
    requires_input: bool,
    side_effect: ResurfacingSideEffect,
) -> ResurfacingActionCapability {
    ResurfacingActionCapability {
        kind,
        label,
        requires_input,
        side_effect,
    }
}

fn current_comm_title(evidence: &ResolvedChannelEvidence) -> Option<String> {
    for value in [
        evidence.primary.subject.as_deref(),
        evidence.primary.from_name.as_deref(),
        evidence.primary.from_address.as_deref(),
    ] {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            return Some(value.to_string());
        }
    }
    None
}

fn revision_changed(candidate: Option<&str>, source: Option<&str>) -> bool {
    match (candidate, source) {
        (None, None) => false,
        (Some(candidate), Some(source)) => candidate != source,
        _ => true,
    }
}

fn task_route(task_id: &str) -> String {
    format!(
        "/tasks?filter=all&selected={}",
        urlencoding::encode(task_id)
    )
}

fn memory_route(tier: &str, key: &str) -> String {
    format!(
        "/memory?tier={}&entry={}",
        urlencoding::encode(tier),
        urlencoding::encode(key)
    )
}

fn parse_memory_source_ref(source_ref: &str) -> Option<(String, String)> {
    let (tier, key) = source_ref.split_once('#')?;
    let tier = tier.trim();
    let key = key.trim();
    if tier.is_empty() || key.is_empty() {
        return None;
    }
    Some((tier.to_string(), key.to_string()))
}

fn find_memory_entry<'a>(knowledge: &'a Value, tier: &str, key: &str) -> Option<&'a Value> {
    let entries = knowledge.get(tier)?.as_array()?;
    if let Some(id) = key.strip_prefix("record:") {
        return entries
            .iter()
            .find(|entry| entry.get("memory_record_id").and_then(Value::as_str) == Some(id));
    }
    // Legacy links name a human key. Prefer its current revision; an old row
    // appearing first in retained history must not hide the current one.
    let mut current=entries.iter().filter(|entry|memory_entry_key(entry).as_deref()==Some(key)
        && !magician_vector_index::memory_temperature::candidate_metadata_has_superseded_lifecycle(entry));
    if let Some(entry) = current.next() {
        return if current.next().is_none() {
            Some(entry)
        } else {
            None
        };
    }
    entries
        .iter()
        .find(|entry| memory_entry_key(entry).as_deref() == Some(key))
        .or_else(|| {
            key.parse::<usize>().ok().and_then(|index| {
                entries
                    .get(index)
                    .filter(|entry| memory_entry_key(entry).is_none())
            })
        })
}

fn memory_entry_key(entry: &Value) -> Option<String> {
    ["key", "source_id", "name", "id"]
        .into_iter()
        .find_map(|field| {
            entry
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn memory_entry_text(entry: &Value) -> String {
    match entry.get("value") {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => entry.to_string(),
    }
}

fn bounded_source_text(value: String) -> String {
    value.chars().take(SAFE_SOURCE_TEXT_MAX_CHARS).collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::channel_assist::resurfacing::source_refs::comm_source_ref_for_message;
    use crate::channel_assist::types::{
        DistillState, MailMessageMeta, MailRecordOrigin, MessageDirection,
        MAIL_ASSIST_SCHEMA_VERSION,
    };
    use magician::magician_v2::artifact_v2::models::{TaskLifecycle, TaskOutputMode, TaskSyncMode};
    use magician::magician_v2::artifact_v2::service::CreateTaskInput;
    use magician::magician_v2::attention::resurfacing::interaction::ResurfacingRecommendationSource;
    use magician::magician_v2::attention::resurfacing::types::{
        candidate_id, CandidateState, SalienceSignals,
    };
    use magician::magician_v2::notes::WriteNoteMarkdownRequest;
    use magician::magician_v2::test_support::build_test_artifact_v2_service;

    fn candidate(kind: SourceKind, source_ref: &str, title: &str, digest: &str) -> Candidate {
        Candidate {
            candidate_id: candidate_id(kind, source_ref),
            source_kind: kind,
            source_ref: source_ref.to_string(),
            title: title.to_string(),
            content_digest: digest.to_string(),
            content_details: None,
            content_revision: None,
            semantic_features: None,
            salience_score: 0.5,
            signals: SalienceSignals::default(),
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Surfaced,
            first_seen_at: 1,
            last_scored_at: 1,
            last_surfaced_at: Some(1),
            cooldown_until: 0,
            surface_count: 1,
            dismiss_count: 0,
        }
    }

    #[test]
    fn disabled_contextual_rollout_keeps_only_read_capabilities() {
        let mut registry = ResurfacingInteractionRegistry::from_adapters(Vec::new());
        registry.contextual_actions_enabled = false;
        let candidate = candidate(
            SourceKind::Comm,
            "gmail|business|thread-1|message-1|123",
            "Policy",
            "Summary",
        );
        let capabilities = registry.capabilities(&candidate, None);
        assert!(capabilities.iter().all(|action| matches!(
            action.kind,
            ResurfacingActionKind::ViewDetails
                | ResurfacingActionKind::OpenSource
                | ResurfacingActionKind::ShowOriginal
        )));
        assert!(!capabilities
            .iter()
            .any(|action| action.kind == ResurfacingActionKind::CreateTask));
    }

    #[test]
    fn recommendation_visibility_requires_revision_confidence_and_capability() {
        let registry = ResurfacingInteractionRegistry::from_adapters(Vec::new());
        let mut candidate = candidate(SourceKind::Memory, "user.knowledge#trip", "Trip", "Summary");
        candidate.content_revision = Some("2".to_string());
        let capabilities = registry.capabilities(&candidate, None);
        let mut recommendation = ResurfacingRecommendation {
            kind: ResurfacingActionKind::ViewDetails,
            label: "Review details".to_string(),
            rationale: "Useful context".to_string(),
            confidence: 0.8,
            content_revision: Some("2".to_string()),
            source: ResurfacingRecommendationSource::Deterministic,
        };
        assert!(registry.recommendation_is_visible(&candidate, &capabilities, &recommendation));

        recommendation.content_revision = Some("1".to_string());
        assert!(!registry.recommendation_is_visible(&candidate, &capabilities, &recommendation));
        recommendation.content_revision = Some("2".to_string());
        recommendation.confidence = 0.2;
        assert!(!registry.recommendation_is_visible(&candidate, &capabilities, &recommendation));
        recommendation.confidence = 0.8;
        recommendation.kind = ResurfacingActionKind::SaveToMemory;
        assert!(!registry.recommendation_is_visible(&candidate, &capabilities, &recommendation));
        recommendation.kind = ResurfacingActionKind::ViewDetails;
        candidate.state = CandidateState::Acted;
        assert!(!registry.recommendation_is_visible(&candidate, &capabilities, &recommendation));
    }

    fn create_task_input(principal: &str, workspace: &str) -> CreateTaskInput {
        CreateTaskInput {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            title: "Finished report".to_string(),
            description: "Finished report".to_string(),
            agent_id: "personal-assistant".to_string(),
            goal_id: None,
            ui_thread_id: "general".to_string(),
            priority: None,
            due_date: None,
            tags: Vec::new(),
            created_by: "user".to_string(),
            depends_on: Vec::new(),
            approved: true,
            schedule: None,
            output_mode: TaskOutputMode::Accumulate,
            chat_session_id: None,
            lifecycle: TaskLifecycle::default(),
            sync_mode: TaskSyncMode::default(),
        }
    }

    fn comm_message(suppressed: bool) -> MailMessageMeta {
        MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            account_email: Some("owner@example.com".to_string()),
            thread_id: "thread-1".to_string(),
            message_id: "message-1".to_string(),
            provider_cursor: None,
            label_ids: Vec::new(),
            subject: Some("Policy details".to_string()),
            from_name: Some("Provider".to_string()),
            from_address: Some("provider@example.com".to_string()),
            to_domains: Vec::new(),
            cc_domains: Vec::new(),
            internal_date: 123,
            observed_at: 123,
            direction: Some(MessageDirection::Inbound),
            summary: Some("The limit changed to 10".to_string()),
            intent: Some("fyi".to_string()),
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: if suppressed {
                DistillState::Suppressed
            } else {
                DistillState::Done
            },
            distill_attempts: 0,
            sensitive_suppressed: suppressed,
            origin: MailRecordOrigin::MetadataSync,
        }
    }

    #[tokio::test]
    async fn task_adapter_resolves_only_the_scoped_task_and_canonical_route() {
        let tmp = tempdir().unwrap();
        let service = build_test_artifact_v2_service(tmp.path());
        let created = service
            .create_task(create_task_input("p", "w"))
            .await
            .unwrap();
        let task_id = created.manifest.task_id;
        let adapter = TaskInteractionAdapter::new(service);
        let current = candidate(SourceKind::Task, &task_id, "Finished report", "");
        let detail = adapter
            .resolve_detail(
                &AttentionScope {
                    principal: "p".to_string(),
                    workspace: "w".to_string(),
                },
                &current,
            )
            .await
            .unwrap();
        assert_eq!(detail.status, ResurfacingSourceStatus::Available);
        let expected_route = task_route(&task_id);
        assert_eq!(
            detail.source_route.as_deref(),
            Some(expected_route.as_str())
        );

        let other_scope = adapter
            .resolve_detail(
                &AttentionScope {
                    principal: "other".to_string(),
                    workspace: "w".to_string(),
                },
                &current,
            )
            .await
            .unwrap();
        assert_eq!(other_scope.status, ResurfacingSourceStatus::Deleted);
    }

    #[tokio::test]
    async fn memory_connections_activity_rejects_superseded_memory_with_unchanged_text() {
        for (field, value) in [
            ("memory_lifecycle", "superseded"),
            ("lifecycle", "replaced"),
            ("status", "SUPERSEDED"),
        ] {
            let tmp = tempdir().unwrap();
            let resolver = AgentMemoryResolver::new(tmp.path());
            let service = resolver.resolve_for_scope("p", "w").unwrap();
            let mut knowledge = serde_json::json!({"user.knowledge":[{"key":"lpg","value":"Booking limit is now 10","updated_at":"2026-07-12T00:00:00Z"}]});
            service.persist_user_knowledge(&knowledge).await.unwrap();
            let adapter = MemoryInteractionAdapter::new(resolver);
            let scope = AttentionScope {
                principal: "p".into(),
                workspace: "w".into(),
            };
            let current = candidate(
                SourceKind::Memory,
                "user.knowledge#lpg",
                "user.knowledge: lpg",
                "Booking limit is now 10",
            );
            assert_eq!(
                adapter
                    .resolve_detail(&scope, &current)
                    .await
                    .unwrap()
                    .status,
                ResurfacingSourceStatus::Available
            );
            knowledge["user.knowledge"][0][field] = serde_json::json!(value);
            service.persist_user_knowledge(&knowledge).await.unwrap();
            let detail = adapter.resolve_detail(&scope, &current).await.unwrap();
            assert_eq!(detail.status, ResurfacingSourceStatus::Stale);
            assert!(detail.summary.is_none());
            assert!(adapter
                .resolve_original(&scope, &current)
                .await
                .unwrap()
                .original
                .is_none());
        }
    }

    #[tokio::test]
    async fn memory_lifecycle_activity_resolves_revision_identity_and_legacy_current_key() {
        let temp = tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(temp.path());
        resolver.resolve_for_scope("p","w").unwrap().persist_user_knowledge(&serde_json::json!({"preferences":[
            {"key":"city","value":"Delhi","memory_record_id":"old","memory_lifecycle":"superseded"},
            {"key":"city","value":"Mumbai","memory_record_id":"current","memory_lifecycle":"active"}
        ]})).await.unwrap();
        let adapter = MemoryInteractionAdapter::new(resolver);
        let scope = AttentionScope {
            principal: "p".into(),
            workspace: "w".into(),
        };
        for source in ["preferences#city", "preferences#record:current"] {
            let detail = adapter
                .resolve_detail(
                    &scope,
                    &candidate(SourceKind::Memory, source, "City", "Mumbai"),
                )
                .await
                .unwrap();
            assert_eq!(detail.status, ResurfacingSourceStatus::Available);
        }
        let old = adapter
            .resolve_detail(
                &scope,
                &candidate(
                    SourceKind::Memory,
                    "preferences#record:old",
                    "City",
                    "Delhi",
                ),
            )
            .await
            .unwrap();
        assert_eq!(old.status, ResurfacingSourceStatus::Stale);
    }

    #[tokio::test]
    async fn memory_adapter_resolves_current_entry_without_save_to_memory_capability() {
        let tmp = tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        resolver
            .resolve_for_scope("p", "w")
            .unwrap()
            .save_user_knowledge(&serde_json::json!({
                "user.knowledge": [{
                    "key": "lpg",
                    "value": "Booking limit is now 10",
                    "updated_at": "2026-07-12T00:00:00Z"
                }]
            }))
            .await
            .unwrap();
        let adapter = MemoryInteractionAdapter::new(resolver);
        let current = candidate(
            SourceKind::Memory,
            "user.knowledge#lpg",
            "user.knowledge: lpg",
            "Booking limit is now 10",
        );
        let detail = adapter
            .resolve_original(
                &AttentionScope {
                    principal: "p".to_string(),
                    workspace: "w".to_string(),
                },
                &current,
            )
            .await
            .unwrap();
        assert_eq!(detail.status, ResurfacingSourceStatus::Available);
        assert!(matches!(
            detail.original,
            Some(ResurfacingOriginalContent::Memory { .. })
        ));
        assert!(!adapter
            .capabilities(&current, Some(&detail))
            .iter()
            .any(|capability| capability.kind == ResurfacingActionKind::SaveToMemory));
    }

    #[tokio::test]
    async fn comm_adapter_reports_offline_account_and_server_computed_gmail_link() {
        let tmp = tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let store = ChannelAssistStore::open(tmp.path()).unwrap();
        let message = comm_message(false);
        store
            .append_messages("p", "w", vec![message.clone()])
            .await
            .unwrap();
        let source_ref = comm_source_ref_for_message(&message);
        let mut current = candidate(
            SourceKind::Comm,
            &source_ref,
            "Policy details",
            "The limit changed to 10",
        );
        current.content_revision = None;
        let adapter = CommInteractionAdapter::new(ChannelEvidenceResolver::new(layout, store));
        let detail = adapter
            .resolve_detail(
                &AttentionScope {
                    principal: "p".to_string(),
                    workspace: "w".to_string(),
                },
                &current,
            )
            .await
            .unwrap();
        assert_eq!(detail.status, ResurfacingSourceStatus::Offline);
        assert_eq!(
            detail.open_url.as_deref(),
            Some("https://mail.google.com/mail/?authuser=owner%40example.com#all/thread-1")
        );
    }

    #[tokio::test]
    async fn comm_adapter_fails_closed_when_source_becomes_suppressed() {
        let tmp = tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(tmp.path());
        let store = ChannelAssistStore::open(tmp.path()).unwrap();
        let message = comm_message(true);
        store
            .append_messages("p", "w", vec![message.clone()])
            .await
            .unwrap();
        let current = candidate(
            SourceKind::Comm,
            &comm_source_ref_for_message(&message),
            "Policy details",
            "The limit changed to 10",
        );
        let adapter = CommInteractionAdapter::new(ChannelEvidenceResolver::new(layout, store));
        let detail = adapter
            .resolve_detail(
                &AttentionScope {
                    principal: "p".to_string(),
                    workspace: "w".to_string(),
                },
                &current,
            )
            .await
            .unwrap();
        assert_eq!(detail.status, ResurfacingSourceStatus::Suppressed);
        assert!(detail.source.is_none());
        assert!(adapter.capabilities(&current, Some(&detail)).is_empty());
    }

    #[test]
    fn communication_capabilities_follow_provider_registry() {
        let gmail = candidate(
            SourceKind::Comm,
            "gmail/business/thread/message@123",
            "Mail",
            "Summary",
        );
        let telegram = candidate(
            SourceKind::Comm,
            "telegram/presto/thread/message@123",
            "Chat",
            "Summary",
        );
        let gmail_actions = static_capabilities(&gmail, None);
        assert!(gmail_actions
            .iter()
            .any(|capability| capability.kind == ResurfacingActionKind::OpenSource));
        assert!(gmail_actions
            .iter()
            .any(|capability| capability.kind == ResurfacingActionKind::ShowOriginal));
        let telegram_actions = static_capabilities(&telegram, None);
        assert!(!telegram_actions
            .iter()
            .any(|capability| capability.kind == ResurfacingActionKind::OpenSource));
        assert!(telegram_actions
            .iter()
            .any(|capability| capability.kind == ResurfacingActionKind::ShowOriginal));
    }

    #[tokio::test]
    async fn note_adapter_resolves_scoped_live_content_open_url_and_newer_revision() {
        let tmp = tempdir().unwrap();
        let store = NotesSettingsStore::new(tmp.path());
        let write = |markdown: &str| WriteNoteMarkdownRequest {
            provider: Some("local_markdown".into()),
            target_path: "Inbox/live.md".into(),
            markdown: markdown.into(),
        };
        store
            .write_note_markdown("p", "w", write("# Live note\n\nVersion one"))
            .await
            .unwrap();
        let note = store
            .read_observation_note("p", "w", "notes:local_markdown:Inbox/live.md")
            .await
            .unwrap()
            .unwrap();
        let mut current = candidate(
            SourceKind::Note,
            &note.source_ref,
            &note.title,
            "Version one",
        );
        current.content_revision = Some(note.content_hash);
        let adapter = NoteInteractionAdapter::new(store.clone());
        let detail = adapter
            .resolve_original(
                &AttentionScope {
                    principal: "p".into(),
                    workspace: "w".into(),
                },
                &current,
            )
            .await
            .unwrap();
        assert_eq!(detail.status, ResurfacingSourceStatus::Available);
        assert!(detail
            .open_url
            .as_deref()
            .is_some_and(|url| url.starts_with("file://")));
        assert!(matches!(
            detail.original,
            Some(ResurfacingOriginalContent::Note { .. })
        ));
        assert!(adapter
            .capabilities(&current, Some(&detail))
            .iter()
            .any(|capability| capability.kind == ResurfacingActionKind::OpenSource));

        store
            .write_note_markdown("p", "w", write("# Live note\n\nVersion two"))
            .await
            .unwrap();
        let newer = adapter
            .resolve_detail(
                &AttentionScope {
                    principal: "p".into(),
                    workspace: "w".into(),
                },
                &current,
            )
            .await
            .unwrap();
        assert_eq!(newer.status, ResurfacingSourceStatus::NewerAvailable);
        assert!(newer.has_newer);
    }

    #[tokio::test]
    async fn web_adapter_preserves_canonical_open_url_and_never_fetches_original() {
        let current = candidate(
            SourceKind::Web,
            "https://example.com/products/useful",
            "Useful launch",
            "A concise source summary",
        );
        let adapter = WebInteractionAdapter;
        let detail = adapter
            .resolve_detail(
                &AttentionScope {
                    principal: "p".to_string(),
                    workspace: "w".to_string(),
                },
                &current,
            )
            .await
            .unwrap();

        assert_eq!(detail.status, ResurfacingSourceStatus::Available);
        assert_eq!(
            detail.open_url.as_deref(),
            Some("https://example.com/products/useful")
        );
        let actions = adapter.capabilities(&current, Some(&detail));
        assert!(actions
            .iter()
            .any(|action| action.kind == ResurfacingActionKind::OpenSource));
        assert!(!actions
            .iter()
            .any(|action| action.kind == ResurfacingActionKind::ShowOriginal));
    }
}
