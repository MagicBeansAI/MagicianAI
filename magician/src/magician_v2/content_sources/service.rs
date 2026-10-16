use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{bail, Result};
use serde::Serialize;

use super::{
    capability_reader::CONTENT_CACHE_OUTCOME_METADATA_KEY, registry::ContentPolicyDenied,
    retrieval::RetrievalRuntimeState, types::validate_content_scope_component, ContentDocument,
    ContentSourceDescriptor, ContentSourceRegistry, DiscoveryPage, DiscoveryRequest,
    ProgressiveRetrievalSettings, ReadRequest, RetrievalLadderController,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentAdapterKind {
    Discovery,
    Reader,
    /// Scope-installed skill metadata is broken before any optional content
    /// adapter can be classified as discovery or reader.
    SkillInstallation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRegistrationErrorClass {
    ManifestScanFailed,
    ManifestInvalid,
    CapabilityUnavailable,
    AdapterInitializationFailed,
    DuplicateAdapter,
    BrokenSkillInstallation,
}

/// Credential-free registration failure suitable for readiness endpoints.
/// Raw manifest contents, environment values, and request data are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentRegistrationIssue {
    pub kind: ContentAdapterKind,
    pub source_id: String,
    pub error_class: ContentRegistrationErrorClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentAcquisitionCatalog {
    pub principal: String,
    pub workspace: String,
    pub capability_revision: String,
    pub discovery: Vec<ContentSourceDescriptor>,
    pub readers: Vec<ContentSourceDescriptor>,
    pub unavailable: Vec<ContentRegistrationIssue>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContentOperationMetricsSnapshot {
    pub calls: u64,
    pub successes: u64,
    pub failures: u64,
    pub policy_denials: u64,
    pub total_latency_ms: u64,
    pub returned_items: u64,
    pub cost_microunits: BTreeMap<String, u64>,
    pub cache_outcomes: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContentAcquisitionMetricsSnapshot {
    pub discovery: BTreeMap<String, ContentOperationMetricsSnapshot>,
    pub reads: BTreeMap<String, ContentOperationMetricsSnapshot>,
}

#[derive(Default)]
struct ContentAcquisitionMetrics {
    discovery: BTreeMap<String, ContentOperationMetricsSnapshot>,
    reads: BTreeMap<String, ContentOperationMetricsSnapshot>,
}

/// Provider-neutral acquisition facade bound to exactly one runtime scope.
/// Product consumers own ranking, persistence, scheduling, and presentation.
pub struct ContentAcquisitionService {
    principal: String,
    workspace: String,
    capability_revision: String,
    registry: Arc<ContentSourceRegistry>,
    registration_issues: Arc<Vec<ContentRegistrationIssue>>,
    metrics: Arc<Mutex<ContentAcquisitionMetrics>>,
    retrieval_state: Arc<RetrievalRuntimeState>,
}

impl std::fmt::Debug for ContentAcquisitionService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ContentAcquisitionService")
            .field("principal", &self.principal)
            .field("workspace", &self.workspace)
            .field("capability_revision", &self.capability_revision)
            .field(
                "discovery_adapter_count",
                &self.registry.discovery_descriptors().len(),
            )
            .field("reader_count", &self.registry.reader_descriptors().len())
            .field("registration_issue_count", &self.registration_issues.len())
            .finish()
    }
}

impl ContentAcquisitionService {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn new(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        capability_revision: impl Into<String>,
        registry: ContentSourceRegistry,
        registration_issues: Vec<ContentRegistrationIssue>,
    ) -> Result<Self> {
        Self::new_with_retrieval_state(
            principal,
            workspace,
            capability_revision,
            registry,
            registration_issues,
            Arc::new(RetrievalRuntimeState::default()),
        )
    }

    pub fn new_with_retrieval_state(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        capability_revision: impl Into<String>,
        registry: ContentSourceRegistry,
        registration_issues: Vec<ContentRegistrationIssue>,
        retrieval_state: Arc<RetrievalRuntimeState>,
    ) -> Result<Self> {
        let principal = principal.into();
        let workspace = workspace.into();
        validate_content_scope_component(&principal, "content acquisition principal")?;
        validate_content_scope_component(&workspace, "content acquisition workspace")?;
        Ok(Self {
            principal,
            workspace,
            capability_revision: capability_revision.into(),
            registry: Arc::new(registry),
            registration_issues: Arc::new(registration_issues),
            metrics: Arc::new(Mutex::new(ContentAcquisitionMetrics::default())),
            retrieval_state,
        })
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn capability_revision(&self) -> &str {
        &self.capability_revision
    }

    pub fn catalog(&self) -> ContentAcquisitionCatalog {
        ContentAcquisitionCatalog {
            principal: self.principal.clone(),
            workspace: self.workspace.clone(),
            capability_revision: self.capability_revision.clone(),
            discovery: self.registry.discovery_descriptors(),
            readers: self.registry.reader_descriptors(),
            unavailable: self.registration_issues.as_ref().clone(),
        }
    }

    pub fn metrics(&self) -> ContentAcquisitionMetricsSnapshot {
        let metrics = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ContentAcquisitionMetricsSnapshot {
            discovery: metrics.discovery.clone(),
            reads: metrics.reads.clone(),
        }
    }

    pub fn retrieval_controller(
        self: &Arc<Self>,
        settings: ProgressiveRetrievalSettings,
    ) -> RetrievalLadderController {
        RetrievalLadderController::new(
            Arc::clone(self),
            settings,
            Arc::clone(&self.retrieval_state),
        )
    }

    pub async fn discover(
        &self,
        adapter_id: &str,
        request: &DiscoveryRequest,
    ) -> Result<DiscoveryPage> {
        self.require_scope(&request.principal, &request.workspace)?;
        let started = Instant::now();
        // Heap-owned: this metrics wrapper should not carry the adapter's state
        // machine in its own frame.
        let result = Box::pin(self.registry.discover(adapter_id, request)).await;
        let elapsed_ms = elapsed_ms(started);
        let policy_denied = is_policy_denial(&result);
        let mut metrics = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = metrics.discovery.entry(adapter_id.to_string()).or_default();
        observe_call(entry, elapsed_ms, result.is_ok(), policy_denied);
        if let Ok(page) = result.as_ref() {
            entry.returned_items = entry.returned_items.saturating_add(page.items.len() as u64);
            if let Some(cost) = page.cost.as_ref() {
                let total = entry
                    .cost_microunits
                    .entry(cost.commodity.clone())
                    .or_default();
                *total = total.saturating_add(cost.amount_microunits);
            }
        }
        drop(metrics);
        result
    }

    pub async fn read(&self, reader_id: &str, request: &ReadRequest) -> Result<ContentDocument> {
        self.require_scope(&request.principal, &request.workspace)?;
        let started = Instant::now();
        let result = self.registry.read(reader_id, request).await;
        let elapsed_ms = elapsed_ms(started);
        let policy_denied = is_policy_denial(&result);
        let mut metrics = self
            .metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = metrics.reads.entry(reader_id.to_string()).or_default();
        observe_call(entry, elapsed_ms, result.is_ok(), policy_denied);
        if let Some(outcome) = result.as_ref().ok().and_then(|document| {
            document
                .metadata
                .get(CONTENT_CACHE_OUTCOME_METADATA_KEY)
                .and_then(serde_json::Value::as_str)
        }) {
            let count = entry.cache_outcomes.entry(outcome.to_string()).or_default();
            *count = count.saturating_add(1);
        }
        drop(metrics);
        result
    }

    fn require_scope(&self, principal: &str, workspace: &str) -> Result<()> {
        if principal != self.principal || workspace != self.workspace {
            bail!(
                "content acquisition request scope {principal}/{workspace} does not match bound \
                 scope {}/{}",
                self.principal,
                self.workspace
            );
        }
        Ok(())
    }
}

fn is_policy_denial<T>(result: &Result<T>) -> bool {
    result.as_ref().err().is_some_and(|error| {
        error
            .chain()
            .any(|cause| cause.downcast_ref::<ContentPolicyDenied>().is_some())
    })
}

fn observe_call(
    metrics: &mut ContentOperationMetricsSnapshot,
    elapsed_ms: u64,
    success: bool,
    policy_denied: bool,
) {
    metrics.calls = metrics.calls.saturating_add(1);
    metrics.total_latency_ms = metrics.total_latency_ms.saturating_add(elapsed_ms);
    if success {
        metrics.successes = metrics.successes.saturating_add(1);
    } else {
        metrics.failures = metrics.failures.saturating_add(1);
    }
    if policy_denied {
        metrics.policy_denials = metrics.policy_denials.saturating_add(1);
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::content_sources::{
        AdapterAuth, AdapterCost, AdapterExecution, ContentCandidate, ContentInvocationSource,
        ContentPrivacy, ContentProvenance, ContentSourceCapabilities, ContentSourceClass,
        DiscoveryAdapter, FreshnessPolicy, ReadDepth, ReadSelectionEvidence, ReadSelectionReason,
        RemoteDataPolicy, RetrievalActionMetadata, RetrievalAuthority, RetrievalOutputKind,
        RetrievalRung, SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION,
    };

    struct FakeDiscovery {
        descriptor: ContentSourceDescriptor,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl DiscoveryAdapter for FakeDiscovery {
        fn descriptor(&self) -> &ContentSourceDescriptor {
            &self.descriptor
        }

        async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryPage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(DiscoveryPage {
                items: vec![candidate(&self.descriptor.adapter_id)],
                next_cursor: None,
                validators: BTreeMap::new(),
                cost: self.descriptor.capabilities.metered.then_some(AdapterCost {
                    commodity: "credits".to_string(),
                    amount_microunits: request.limit as u64,
                }),
                transport: Default::default(),
            })
        }
    }

    struct FakeReader {
        descriptor: ContentSourceDescriptor,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl super::super::ContentReader for FakeReader {
        fn descriptor(&self) -> &ContentSourceDescriptor {
            &self.descriptor
        }

        async fn read(&self, request: &ReadRequest) -> Result<ContentDocument> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ContentDocument {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: request.candidate.identity.clone(),
                title: request.candidate.title.clone(),
                text: "A complete provider-free document used by the acquisition handoff test."
                    .to_string(),
                canonical_url: request.candidate.canonical_url.clone(),
                media_type: Some("text/plain".to_string()),
                fetched_at_ms: 2,
                privacy: ContentPrivacy::Public,
                content_hash: blake3::hash(b"handoff-document").to_hex().to_string(),
                provenance: request.candidate.provenance.clone(),
                metadata: BTreeMap::from([(
                    CONTENT_CACHE_OUTCOME_METADATA_KEY.to_string(),
                    json!("fresh_hit"),
                )]),
            })
        }
    }

    fn descriptor(
        id: &str,
        discovery: bool,
        full_content: bool,
        execution: AdapterExecution,
        metered: bool,
    ) -> ContentSourceDescriptor {
        ContentSourceDescriptor {
            adapter_id: id.to_string(),
            display_name: id.to_string(),
            class: if full_content {
                ContentSourceClass::WebPage
            } else {
                ContentSourceClass::WebSearch
            },
            capabilities: ContentSourceCapabilities {
                discovery,
                full_content,
                cursor: false,
                conditional_fetch: full_content,
                execution,
                auth: AdapterAuth::None,
                sends_user_intent: execution == AdapterExecution::RemoteEndpoint,
                metered,
            },
            retrieval: if discovery {
                RetrievalActionMetadata::discovery(
                    format!("{id}.discover"),
                    RetrievalRung::PublicSearch,
                    if execution == AdapterExecution::LocalProcess {
                        RetrievalAuthority::LocalOnly
                    } else {
                        RetrievalAuthority::PublicRemoteRead
                    },
                    true,
                )
            } else {
                RetrievalActionMetadata::reader(
                    format!("{id}.read"),
                    RetrievalRung::PublicStatic,
                    RetrievalAuthority::PublicRemoteRead,
                    vec![RetrievalOutputKind::Gist, RetrievalOutputKind::FullText],
                )
            },
        }
    }

    fn candidate(adapter_id: &str) -> ContentCandidate {
        ContentCandidate {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new(adapter_id, "item-1").unwrap(),
            title: "Selected result".to_string(),
            cheap_text: "Enough bounded text for a consumer to select the result.".to_string(),
            canonical_url: Some("https://example.com/item-1".to_string()),
            published_at_ms: Some(1),
            observed_at_ms: 1,
            privacy: ContentPrivacy::Public,
            content_hash: None,
            provenance: ContentProvenance {
                source_label: "example.com".to_string(),
                source_url: Some("https://example.com/item-1".to_string()),
                retrieved_by: adapter_id.to_string(),
            },
            metadata: BTreeMap::new(),
        }
    }

    fn discovery_request(principal: &str, workspace: &str) -> DiscoveryRequest {
        DiscoveryRequest {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            intent: Some("track selected engineering updates".to_string()),
            query: Some("engineering updates".to_string()),
            targets: Vec::new(),
            cursor: None,
            validators: BTreeMap::new(),
            limit: 3,
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::UserFeed,
            options: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn foreign_scope_is_rejected_before_adapter_execution() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ContentSourceRegistry::new();
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor(
                    "search",
                    true,
                    false,
                    AdapterExecution::LocalProcess,
                    false,
                ),
                calls: Arc::clone(&calls),
            }))
            .unwrap();
        let service =
            ContentAcquisitionService::new("owner", "default", "revision-1", registry, Vec::new())
                .unwrap();

        let error = service
            .discover("search", &discovery_request("guest", "default"))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("does not match bound scope"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(service.metrics().discovery.is_empty());
    }

    #[tokio::test]
    async fn catalog_discover_selection_read_handoff_preserves_scope_and_telemetry() {
        let discovery_calls = Arc::new(AtomicUsize::new(0));
        let reader_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ContentSourceRegistry::new();
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor(
                    "search",
                    true,
                    false,
                    AdapterExecution::RemoteEndpoint,
                    true,
                ),
                calls: Arc::clone(&discovery_calls),
            }))
            .unwrap();
        registry
            .register_reader(Arc::new(FakeReader {
                descriptor: descriptor(
                    "static-http",
                    false,
                    true,
                    AdapterExecution::LocalProcess,
                    false,
                ),
                calls: Arc::clone(&reader_calls),
            }))
            .unwrap();
        let service =
            ContentAcquisitionService::new("owner", "default", "revision-1", registry, Vec::new())
                .unwrap();
        assert_eq!(service.catalog().discovery.len(), 1);
        assert_eq!(service.catalog().readers.len(), 1);

        let page = service
            .discover("search", &discovery_request("owner", "default"))
            .await
            .unwrap();
        let read = ReadRequest {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            candidate: page.items[0].clone(),
            depth: ReadDepth::FullText,
            freshness: FreshnessPolicy::CachedOk,
            remote_content_policy: RemoteDataPolicy::Deny,
            invocation_source: ContentInvocationSource::UserFeed,
            selection: Some(ReadSelectionEvidence {
                selected_at_ms: 2,
                reason: ReadSelectionReason::FeedMatch,
                relevance_score: Some(0.91),
            }),
            authority_grant_id: None,
        };
        let document = service.read("static-http", &read).await.unwrap();

        assert_eq!(document.identity, page.items[0].identity);
        assert_eq!(discovery_calls.load(Ordering::SeqCst), 1);
        assert_eq!(reader_calls.load(Ordering::SeqCst), 1);
        let metrics = service.metrics();
        assert_eq!(metrics.discovery["search"].returned_items, 1);
        assert_eq!(metrics.discovery["search"].cost_microunits["credits"], 3);
        assert_eq!(metrics.reads["static-http"].cache_outcomes["fresh_hit"], 1);
    }

    #[tokio::test]
    async fn remote_policy_denial_is_counted_without_calling_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ContentSourceRegistry::new();
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor(
                    "remote",
                    true,
                    false,
                    AdapterExecution::RemoteEndpoint,
                    true,
                ),
                calls: Arc::clone(&calls),
            }))
            .unwrap();
        let service =
            ContentAcquisitionService::new("owner", "default", "revision-1", registry, Vec::new())
                .unwrap();
        let mut request = discovery_request("owner", "default");
        request.remote_query_policy = RemoteDataPolicy::Deny;

        assert!(service.discover("remote", &request).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let metrics = service.metrics();
        assert_eq!(metrics.discovery["remote"].calls, 1);
        assert_eq!(metrics.discovery["remote"].failures, 1);
        assert_eq!(metrics.discovery["remote"].policy_denials, 1);
    }

    #[tokio::test]
    async fn malformed_remote_request_is_not_misclassified_as_policy_denial() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ContentSourceRegistry::new();
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor(
                    "remote",
                    true,
                    false,
                    AdapterExecution::RemoteEndpoint,
                    true,
                ),
                calls: Arc::clone(&calls),
            }))
            .unwrap();
        let service =
            ContentAcquisitionService::new("owner", "default", "revision-1", registry, Vec::new())
                .unwrap();
        let mut request = discovery_request("owner", "default");
        request.limit = 0;
        request.remote_query_policy = RemoteDataPolicy::Deny;

        assert!(service.discover("remote", &request).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let metrics = service.metrics();
        assert_eq!(metrics.discovery["remote"].failures, 1);
        assert_eq!(metrics.discovery["remote"].policy_denials, 0);
    }
}
