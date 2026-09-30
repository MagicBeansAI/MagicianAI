use std::{collections::BTreeMap, sync::Arc};

use anyhow::{anyhow, bail, Context, Result};

use super::{
    traits::{ContentReader, DiscoveryAdapter},
    types::{
        ContentDocument, ContentSourceDescriptor, DiscoveryPage, DiscoveryRequest, ReadRequest,
        RemoteDataPolicy,
    },
};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ContentPolicyDenied {
    message: String,
}

impl ContentPolicyDenied {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Default)]
pub struct ContentSourceRegistry {
    discovery: BTreeMap<String, Arc<dyn DiscoveryAdapter>>,
    readers: BTreeMap<String, Arc<dyn ContentReader>>,
}

impl ContentSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_discovery(&mut self, adapter: Arc<dyn DiscoveryAdapter>) -> Result<()> {
        let descriptor = adapter.descriptor();
        descriptor.validate()?;
        if !descriptor.capabilities.discovery {
            bail!(
                "adapter `{}` does not advertise discovery capability",
                descriptor.adapter_id
            );
        }
        let id = descriptor.adapter_id.clone();
        self.ensure_unique_action(&descriptor.retrieval.action_id)?;
        if self.discovery.contains_key(&id) {
            bail!("duplicate discovery adapter `{id}`");
        }
        self.discovery.insert(id, adapter);
        Ok(())
    }

    pub fn register_reader(&mut self, reader: Arc<dyn ContentReader>) -> Result<()> {
        let descriptor = reader.descriptor();
        descriptor.validate()?;
        if !descriptor.capabilities.full_content {
            bail!(
                "adapter `{}` does not advertise full-content capability",
                descriptor.adapter_id
            );
        }
        let id = descriptor.adapter_id.clone();
        self.ensure_unique_action(&descriptor.retrieval.action_id)?;
        if self.readers.contains_key(&id) {
            bail!("duplicate content reader `{id}`");
        }
        self.readers.insert(id, reader);
        Ok(())
    }

    pub fn discovery_descriptors(&self) -> Vec<ContentSourceDescriptor> {
        self.discovery
            .values()
            .map(|adapter| adapter.descriptor().clone())
            .collect()
    }

    pub fn discovery_descriptor(&self, adapter_id: &str) -> Option<ContentSourceDescriptor> {
        self.discovery
            .get(adapter_id)
            .map(|adapter| adapter.descriptor().clone())
    }

    pub fn reader_descriptors(&self) -> Vec<ContentSourceDescriptor> {
        self.readers
            .values()
            .map(|reader| reader.descriptor().clone())
            .collect()
    }

    pub fn reader_descriptor(&self, reader_id: &str) -> Option<ContentSourceDescriptor> {
        self.readers
            .get(reader_id)
            .map(|reader| reader.descriptor().clone())
    }

    pub async fn discover(
        &self,
        adapter_id: &str,
        request: &DiscoveryRequest,
    ) -> Result<DiscoveryPage> {
        request.validate()?;
        let adapter = self
            .discovery
            .get(adapter_id)
            .ok_or_else(|| anyhow!("unknown discovery adapter `{adapter_id}`"))?;
        let descriptor = adapter.descriptor();
        if request.cursor.is_some() && !descriptor.capabilities.cursor {
            bail!("discovery adapter `{adapter_id}` does not support cursors");
        }
        if !request.validators.is_empty() && !descriptor.capabilities.conditional_fetch {
            bail!("discovery adapter `{adapter_id}` does not support conditional validators");
        }
        if descriptor.capabilities.sends_user_intent
            && request.remote_query_policy == RemoteDataPolicy::Deny
        {
            return Err(ContentPolicyDenied::new(format!(
                "discovery adapter `{adapter_id}` requires permission to send a query remotely"
            ))
            .into());
        }

        let page = adapter
            .discover(request)
            .await
            .with_context(|| format!("running discovery adapter `{adapter_id}`"))?;
        if page.next_cursor.is_some() && !descriptor.capabilities.cursor {
            bail!("discovery adapter `{adapter_id}` returned an unsupported cursor");
        }
        if !page.validators.is_empty() && !descriptor.capabilities.conditional_fetch {
            bail!("discovery adapter `{adapter_id}` returned unsupported validators");
        }
        if page.validators.iter().any(|(target, validator)| {
            !request.targets.iter().any(|candidate| candidate == target)
                || validator.validate().is_err()
        }) {
            bail!("discovery adapter `{adapter_id}` returned invalid or foreign validators");
        }
        if page.items.len() > request.limit {
            bail!(
                "discovery adapter `{adapter_id}` returned {} items above limit {}",
                page.items.len(),
                request.limit
            );
        }
        for item in &page.items {
            item.validate()?;
            if item.identity.adapter_id != adapter_id {
                bail!(
                    "discovery adapter `{adapter_id}` returned foreign identity `{}`",
                    item.identity.adapter_id
                );
            }
        }
        match (&page.cost, descriptor.capabilities.metered) {
            (Some(cost), true) => cost.validate()?,
            (None, true) => {
                bail!("metered discovery adapter `{adapter_id}` omitted its cost")
            },
            (Some(_), false) => {
                bail!("unmetered discovery adapter `{adapter_id}` returned an unexpected cost")
            },
            (None, false) => {},
        }
        Ok(page)
    }

    pub async fn read(&self, reader_id: &str, request: &ReadRequest) -> Result<ContentDocument> {
        request.validate()?;
        let reader = self
            .readers
            .get(reader_id)
            .ok_or_else(|| anyhow!("unknown content reader `{reader_id}`"))?;
        let descriptor = reader.descriptor();
        if descriptor.capabilities.execution == super::types::AdapterExecution::RemoteEndpoint
            && request.remote_content_policy == RemoteDataPolicy::Deny
        {
            return Err(ContentPolicyDenied::new(format!(
                "content reader `{reader_id}` requires remote-content permission"
            ))
            .into());
        }
        let document = reader
            .read(request)
            .await
            .with_context(|| format!("running content reader `{reader_id}`"))?;
        document.validate()?;
        if document.identity != request.candidate.identity {
            bail!("content reader `{reader_id}` returned a foreign source identity");
        }
        if privacy_level(document.privacy) < privacy_level(request.candidate.privacy) {
            bail!("content reader `{reader_id}` downgraded candidate privacy");
        }
        Ok(document)
    }

    fn ensure_unique_action(&self, action_id: &str) -> Result<()> {
        if self
            .discovery
            .values()
            .any(|adapter| adapter.descriptor().retrieval.action_id == action_id)
            || self
                .readers
                .values()
                .any(|reader| reader.descriptor().retrieval.action_id == action_id)
        {
            bail!("duplicate retrieval action `{action_id}`");
        }
        Ok(())
    }
}

fn privacy_level(privacy: super::types::ContentPrivacy) -> u8 {
    match privacy {
        super::types::ContentPrivacy::Public => 0,
        super::types::ContentPrivacy::Private => 1,
        super::types::ContentPrivacy::Restricted => 2,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;

    use super::*;
    use crate::magician_v2::content_sources::{
        AdapterAuth, AdapterExecution, ContentCandidate, ContentInvocationSource, ContentPrivacy,
        ContentProvenance, ContentSourceCapabilities, ContentSourceClass, FreshnessPolicy,
        ReadDepth, ReadSelectionEvidence, ReadSelectionReason, RetrievalActionMetadata,
        RetrievalAuthority, RetrievalOutputKind, RetrievalRung, SourceIdentity,
        CONTENT_SOURCE_SCHEMA_VERSION,
    };

    struct FakeDiscovery {
        descriptor: ContentSourceDescriptor,
        identity_adapter: &'static str,
        next_cursor: Option<&'static str>,
        cost: Option<super::super::AdapterCost>,
    }

    struct FakeReader {
        descriptor: ContentSourceDescriptor,
        calls: AtomicUsize,
        privacy_override: Option<ContentPrivacy>,
    }

    #[async_trait]
    impl ContentReader for FakeReader {
        fn descriptor(&self) -> &ContentSourceDescriptor {
            &self.descriptor
        }

        async fn read(&self, request: &ReadRequest) -> Result<ContentDocument> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ContentDocument {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: request.candidate.identity.clone(),
                title: request.candidate.title.clone(),
                text: "A sufficiently useful full document".into(),
                canonical_url: request.candidate.canonical_url.clone(),
                media_type: Some("text/plain".into()),
                fetched_at_ms: 1,
                privacy: self.privacy_override.unwrap_or(request.candidate.privacy),
                content_hash: blake3::hash(b"A sufficiently useful full document")
                    .to_hex()
                    .to_string(),
                provenance: request.candidate.provenance.clone(),
                metadata: BTreeMap::new(),
            })
        }
    }

    #[async_trait]
    impl DiscoveryAdapter for FakeDiscovery {
        fn descriptor(&self) -> &ContentSourceDescriptor {
            &self.descriptor
        }

        async fn discover(&self, _request: &DiscoveryRequest) -> Result<DiscoveryPage> {
            Ok(DiscoveryPage {
                items: vec![ContentCandidate {
                    schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                    identity: SourceIdentity::new(self.identity_adapter, "one")?,
                    title: "One".into(),
                    cheap_text: "A useful result".into(),
                    canonical_url: Some("https://example.com/one".into()),
                    published_at_ms: None,
                    observed_at_ms: 1,
                    privacy: ContentPrivacy::Public,
                    content_hash: None,
                    provenance: ContentProvenance {
                        source_label: "Fake".into(),
                        source_url: None,
                        retrieved_by: self.descriptor.adapter_id.clone(),
                    },
                    metadata: BTreeMap::new(),
                }],
                next_cursor: self.next_cursor.map(str::to_string),
                validators: BTreeMap::new(),
                cost: self.cost.clone(),
                transport: Default::default(),
            })
        }
    }

    fn descriptor(id: &str, sends_user_intent: bool) -> ContentSourceDescriptor {
        ContentSourceDescriptor {
            adapter_id: id.into(),
            display_name: id.into(),
            class: ContentSourceClass::WebSearch,
            capabilities: ContentSourceCapabilities {
                discovery: true,
                full_content: false,
                cursor: false,
                conditional_fetch: false,
                execution: AdapterExecution::RemoteEndpoint,
                auth: AdapterAuth::Required,
                sends_user_intent,
                metered: false,
            },
            retrieval: RetrievalActionMetadata::discovery(
                format!("{id}.discover"),
                RetrievalRung::PublicSearch,
                RetrievalAuthority::AuthenticatedRead,
                true,
            ),
        }
    }

    fn request(policy: RemoteDataPolicy) -> DiscoveryRequest {
        DiscoveryRequest {
            principal: "p".into(),
            workspace: "w".into(),
            intent: Some("private acquisition target".into()),
            query: Some("query".into()),
            targets: Vec::new(),
            cursor: None,
            validators: BTreeMap::new(),
            limit: 10,
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: policy,
            invocation_source: super::super::ContentInvocationSource::UserFeed,
            options: BTreeMap::new(),
        }
    }

    fn read_request(selection: Option<ReadSelectionEvidence>) -> ReadRequest {
        ReadRequest {
            principal: "p".into(),
            workspace: "w".into(),
            candidate: ContentCandidate {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: SourceIdentity::new("search", "one").unwrap(),
                title: "One".into(),
                cheap_text: "A useful result".into(),
                canonical_url: Some("https://example.com/one".into()),
                published_at_ms: None,
                observed_at_ms: 1,
                privacy: ContentPrivacy::Public,
                content_hash: None,
                provenance: ContentProvenance {
                    source_label: "Fake".into(),
                    source_url: Some("https://example.com/one".into()),
                    retrieved_by: "search".into(),
                },
                metadata: BTreeMap::new(),
            },
            depth: ReadDepth::FullText,
            freshness: FreshnessPolicy::CachedOk,
            remote_content_policy: RemoteDataPolicy::Deny,
            invocation_source: ContentInvocationSource::UserFeed,
            selection,
            authority_grant_id: None,
        }
    }

    #[tokio::test]
    async fn remote_query_requires_explicit_policy() {
        let mut registry = ContentSourceRegistry::new();
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor("search", true),
                identity_adapter: "search",
                next_cursor: None,
                cost: None,
            }))
            .unwrap();

        assert!(registry
            .discover("search", &request(RemoteDataPolicy::Deny))
            .await
            .is_err());
        assert_eq!(
            registry
                .discover("search", &request(RemoteDataPolicy::Allow))
                .await
                .unwrap()
                .items
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn registry_rejects_foreign_adapter_identity() {
        let mut registry = ContentSourceRegistry::new();
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor("search", false),
                identity_adapter: "other",
                next_cursor: None,
                cost: None,
            }))
            .unwrap();
        assert!(registry
            .discover("search", &request(RemoteDataPolicy::Deny))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn registry_rejects_cursor_for_adapter_without_cursor_capability() {
        let mut registry = ContentSourceRegistry::new();
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor("search", false),
                identity_adapter: "search",
                next_cursor: None,
                cost: None,
            }))
            .unwrap();
        let mut request = request(RemoteDataPolicy::Deny);
        request.cursor = Some("opaque-cursor".to_string());
        assert!(registry.discover("search", &request).await.is_err());
    }

    #[tokio::test]
    async fn registry_rejects_returned_cursor_without_cursor_capability() {
        let mut registry = ContentSourceRegistry::new();
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor("search", false),
                identity_adapter: "search",
                next_cursor: Some("unexpected-cursor"),
                cost: None,
            }))
            .unwrap();
        assert!(registry
            .discover("search", &request(RemoteDataPolicy::Deny))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn registry_enforces_metered_cost_contract() {
        let mut registry = ContentSourceRegistry::new();
        let mut metered = descriptor("search", false);
        metered.capabilities.metered = true;
        registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: metered,
                identity_adapter: "search",
                next_cursor: None,
                cost: None,
            }))
            .unwrap();

        assert!(registry
            .discover("search", &request(RemoteDataPolicy::Deny))
            .await
            .unwrap_err()
            .to_string()
            .contains("omitted its cost"));
    }

    #[tokio::test]
    async fn registry_validates_cost_and_rejects_cost_from_unmetered_adapter() {
        let mut metered_registry = ContentSourceRegistry::new();
        let mut metered = descriptor("metered", false);
        metered.capabilities.metered = true;
        metered_registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: metered,
                identity_adapter: "metered",
                next_cursor: None,
                cost: Some(super::super::AdapterCost {
                    commodity: " ".into(),
                    amount_microunits: 1,
                }),
            }))
            .unwrap();
        assert!(metered_registry
            .discover("metered", &request(RemoteDataPolicy::Deny))
            .await
            .is_err());

        let mut unmetered_registry = ContentSourceRegistry::new();
        unmetered_registry
            .register_discovery(Arc::new(FakeDiscovery {
                descriptor: descriptor("free", false),
                identity_adapter: "free",
                next_cursor: None,
                cost: Some(super::super::AdapterCost {
                    commodity: "credits".into(),
                    amount_microunits: 1,
                }),
            }))
            .unwrap();
        assert!(unmetered_registry
            .discover("free", &request(RemoteDataPolicy::Deny))
            .await
            .unwrap_err()
            .to_string()
            .contains("unexpected cost"));
    }

    #[test]
    fn registry_rejects_duplicate_adapter_ids() {
        let mut registry = ContentSourceRegistry::new();
        let first = Arc::new(FakeDiscovery {
            descriptor: descriptor("search", false),
            identity_adapter: "search",
            next_cursor: None,
            cost: None,
        });
        let second = Arc::new(FakeDiscovery {
            descriptor: descriptor("search", false),
            identity_adapter: "search",
            next_cursor: None,
            cost: None,
        });
        registry.register_discovery(first).unwrap();
        assert!(registry.register_discovery(second).is_err());
    }

    #[tokio::test]
    async fn registry_rejects_unselected_feed_read_before_invoking_reader() {
        let reader = Arc::new(FakeReader {
            descriptor: ContentSourceDescriptor {
                adapter_id: "static-http".into(),
                display_name: "Static".into(),
                class: ContentSourceClass::WebPage,
                capabilities: ContentSourceCapabilities {
                    discovery: false,
                    full_content: true,
                    cursor: false,
                    conditional_fetch: true,
                    execution: AdapterExecution::LocalProcess,
                    auth: AdapterAuth::None,
                    sends_user_intent: false,
                    metered: false,
                },
                retrieval: RetrievalActionMetadata::reader(
                    "static-http.read",
                    RetrievalRung::PublicStatic,
                    RetrievalAuthority::PublicRemoteRead,
                    vec![RetrievalOutputKind::Gist, RetrievalOutputKind::FullText],
                ),
            },
            calls: AtomicUsize::new(0),
            privacy_override: None,
        });
        let mut registry = ContentSourceRegistry::new();
        registry.register_reader(reader.clone()).unwrap();

        assert!(registry
            .read("static-http", &read_request(None))
            .await
            .is_err());
        assert_eq!(reader.calls.load(Ordering::SeqCst), 0);

        let selection = ReadSelectionEvidence {
            selected_at_ms: 1,
            reason: ReadSelectionReason::FeedMatch,
            relevance_score: Some(0.9),
        };
        assert!(registry
            .read("static-http", &read_request(Some(selection)))
            .await
            .is_ok());
        assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn registry_rejects_reader_privacy_downgrades() {
        let reader = Arc::new(FakeReader {
            descriptor: ContentSourceDescriptor {
                adapter_id: "static-http".into(),
                display_name: "Static".into(),
                class: ContentSourceClass::WebPage,
                capabilities: ContentSourceCapabilities {
                    discovery: false,
                    full_content: true,
                    cursor: false,
                    conditional_fetch: true,
                    execution: AdapterExecution::LocalProcess,
                    auth: AdapterAuth::None,
                    sends_user_intent: false,
                    metered: false,
                },
                retrieval: RetrievalActionMetadata::reader(
                    "static-http.read",
                    RetrievalRung::PublicStatic,
                    RetrievalAuthority::PublicRemoteRead,
                    vec![RetrievalOutputKind::Gist, RetrievalOutputKind::FullText],
                ),
            },
            calls: AtomicUsize::new(0),
            privacy_override: Some(ContentPrivacy::Public),
        });
        let mut registry = ContentSourceRegistry::new();
        registry.register_reader(reader).unwrap();
        let mut request = read_request(Some(ReadSelectionEvidence {
            selected_at_ms: 1,
            reason: ReadSelectionReason::FeedMatch,
            relevance_score: Some(0.9),
        }));
        request.candidate.privacy = ContentPrivacy::Restricted;

        assert!(registry
            .read("static-http", &request)
            .await
            .unwrap_err()
            .to_string()
            .contains("downgraded candidate privacy"));
    }
}
