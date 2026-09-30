//! Provider-neutral content acquisition.
//!
//! This module is the shared boundary between user-authored content lenses and
//! the systems that can discover or read content. It does not materialize feed
//! items, schedule monitors, or run LLMs. Product consumers own those choices.

mod api_replay_reader;
mod browser;
mod cache;
mod capability_adapter;
mod capability_reader;
mod comms;
mod duckduckgo;
mod observable;
mod observation_runtime;
mod public_http;
pub(crate) use public_http::{
    resolve_public_browser_url, validate_public_http_url, validate_public_socket_addresses,
};
mod registry;
pub mod retrieval;
mod rss;
mod runtime;
mod service;
mod traits;
mod types;

pub use capability_adapter::{
    load_capability_discovery_manifest, CapabilityAdapterManifest, CapabilityBindingManifest,
    CapabilityCostEncoding, CapabilityCostManifest, CapabilityDiscoveryManifest,
    CapabilityInputManifest, CapabilityOptionManifest, CapabilityOptionType,
    CapabilityOutputManifest, CapabilityOutputMode, MappedItemManifest,
    CAPABILITY_DISCOVERY_EXTENSION, CAPABILITY_DISCOVERY_MANIFEST_SCHEMA_VERSION,
};
pub use capability_reader::{
    evaluate_extraction_quality, load_capability_reader_manifest,
    load_optional_capability_reader_manifest, CapabilityReaderAdapterManifest,
    CapabilityReaderBindingManifest, CapabilityReaderInputManifest, CapabilityReaderManifest,
    CapabilityReaderOutputManifest, CapabilityReaderOutputMode, ExtractionQuality,
    CAPABILITY_READER_EXTENSION, CAPABILITY_READER_MANIFEST_SCHEMA_VERSION,
};
pub use comms::candidate_from_channel_message;
pub use observable::{
    discover_observable_sources, load_observable_source_manifest, project_observable_source_offers,
    ObservableActionBinding, ObservableCatalog, ObservableCatalogIssue,
    ObservableCatalogIssueClass, ObservableSourceDefinition, ObservableSourceManifest,
    ObservableSourceOffer, ObservableSourcePolicy, ObservableSourceReadiness,
    ObservableSourceSettings, ObservableUnavailableReason, ObservationCadence,
    ObservationEscalation, ObservationProfile, ObservationProfileAcquisition,
    ObservationProfileLimits, ObservationSchedule, ObservationSurface, OBSERVE_SOURCE_EXTENSION,
    OBSERVE_SOURCE_SCHEMA_VERSION,
};
pub use observation_runtime::{
    CustomRssSubscription, ObservableSourceMetricsSnapshot, ObservableSourceOfferPage,
    ObservableSourceRuntime, ObservationObservabilityTotals, ObservationRunOutcome,
    ObservationRunRecord, ObservationRunRecordPage, ObservationRunStatus, ObservationRunTrigger,
    ObservationSourceObservabilityPage, ObservationSourceObservabilitySummary,
    ObservationSubscription, ObservationSubscriptionPage, ObservationSubscriptionState,
    ObservationSubscriptionView, PutObservationSubscription, SemanticIntentScorer,
    SubscriptionMutationError,
};
pub use registry::ContentSourceRegistry;
pub use retrieval::{
    BrowserRetrievalSettings, ContentAcquisitionSettings, DiscoveryEvidenceGoal, EvidenceGoal,
    ProgressiveRetrievalRollout, ProgressiveRetrievalSettings, ReadEvidenceGoal,
    RetrievalAttemptReceipt, RetrievalAuthorityGrant, RetrievalClassification, RetrievalHandoff,
    RetrievalHandoffKind, RetrievalLadderController, RetrievalNeed, RetrievalQualityFinding,
    RetrievalResult, RetrievalRungConfig, RetrievalRungMode, RetrievalSelectionReceipt,
    RetrievalStatus, RetrievalTarget, RetrievedCandidate, WorkingSetActivationDecision,
    WorkingSetActivationProbe, WorkingSetActivationSettings, WorkingSetCaptureSettings,
    RETRIEVAL_NEED_SCHEMA_VERSION,
};
pub use rss::RssDiscoveryAdapter;
pub use runtime::{ContentAcquisitionResolver, ContentServiceCacheStatus};
pub use service::{
    ContentAcquisitionCatalog, ContentAcquisitionMetricsSnapshot, ContentAcquisitionService,
    ContentAdapterKind, ContentOperationMetricsSnapshot, ContentRegistrationErrorClass,
    ContentRegistrationIssue,
};
pub use traits::{ContentReader, DiscoveryAdapter};
pub use types::{
    canonicalize_http_url, AdapterAuth, AdapterCost, AdapterExecution, ContentCandidate,
    ContentDocument, ContentInvocationSource, ContentPrivacy, ContentProvenance,
    ContentSourceCapabilities, ContentSourceClass, ContentSourceDescriptor, DiscoveryPage,
    DiscoveryRequest, DiscoveryTransportStats, DiscoveryValidator, FreshnessPolicy, ReadDepth,
    ReadRequest, ReadSelectionEvidence, ReadSelectionReason, RemoteDataPolicy,
    RetrievalActionMetadata, RetrievalAuthority, RetrievalOperation, RetrievalOutputKind,
    RetrievalRung, SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION, MAX_ADAPTER_ID_CHARS,
    MAX_CANDIDATE_CHEAP_TEXT_CHARS, MAX_CANDIDATE_TITLE_CHARS, MAX_CONTENT_HASH_CHARS,
    MAX_CONTENT_URL_CHARS, MAX_COST_COMMODITY_CHARS, MAX_DISCOVERY_ITEMS, MAX_DISCOVERY_TARGETS,
    MAX_DISPLAY_NAME_CHARS, MAX_DOCUMENT_TEXT_CHARS, MAX_METADATA_BYTES,
    MAX_PROVENANCE_LABEL_CHARS, MAX_PROVENANCE_RETRIEVER_CHARS, MAX_SOURCE_ITEM_ID_CHARS,
};

/// Registry containing deterministic, generally available acquisition
/// adapters. Metered search providers are registered only after their runtime
/// configuration has been resolved, so absence cannot silently select a
/// different paid provider.
pub fn shipped_content_source_registry() -> anyhow::Result<ContentSourceRegistry> {
    let mut registry = ContentSourceRegistry::new();
    registry.register_discovery(std::sync::Arc::new(RssDiscoveryAdapter::new()?))?;
    registry.register_discovery(std::sync::Arc::new(
        duckduckgo::DuckDuckGoDiscoveryAdapter::new(),
    ))?;
    Ok(registry)
}
