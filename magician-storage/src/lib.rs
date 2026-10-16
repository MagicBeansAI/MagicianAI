//! Neutral Magician storage contracts.
//!
//! This crate owns identifiers, errors, profile parsing/validation, capability
//! traits, health types, conformance factories, and catalog readiness /
//! migration phase types. It must not depend on `magician`, AWS, PostgreSQL,
//! or domain product models. The coordinator that drives those phases lives
//! in `magician-storage-migration` and is not used by default startup.
//!
//! Domain repository traits stay with their owner crates.

pub mod conformance;
pub mod dataset;
pub mod error;
pub mod fs;
pub mod gate3;
pub mod gc;
pub mod health;
pub mod identifiers;
pub mod index;
pub mod lease;
pub mod migration;
pub mod object;
pub mod profile;
pub mod readiness;
pub mod repository;
pub mod runtime;
pub mod scope_lease;
pub mod scratch;
pub mod secret;

pub use conformance::{
    AdapterScenarioFactory, RepositoryScenarioFactory, ScenarioFactory, DATASET_STORE_CASES,
    LEASE_STORE_CASES, OBJECT_STORE_CASES, REPOSITORY_SCENARIO_CASES, SCRATCH_STORE_CASES,
};
pub use dataset::{
    DatasetId, DatasetManifest, DatasetPartReceipt, DatasetPartRef, DatasetStore,
    ManifestCommitReceipt, PartitionId, StageDatasetPart,
};
pub use error::{CommitLikelihood, RetryClass, StorageError};
pub use fs::LocalStorage;
pub use gate3::{
    accepted_budgets, assert_budget, assert_latency_budget, latency_budgets_enforced,
    percentile_ms, BudgetLine, BudgetVerdict, Gate3Budgets, ENFORCE_LATENCY_ENV, GATE3_ACCEPTED_AT,
    GATE3_CLOSED, GATE3_OWNER, ONLINE_MIGRATION_REQUIRED,
};
pub use gc::{GcPolicy, GcReport, ObjectReference, DEFAULT_TOMBSTONE_RETAIN};
pub use health::{HealthStatus, StorageHealth, StorageHealthRegistry};
pub use identifiers::{
    ContentDigest, DigestAlgorithm, LogicalObjectId, PrincipalId, PublicationId, ScopeId,
    StorageKey, StorageNamespace, StorageScope, WorkspaceId,
};
pub use index::{
    IndexId, IndexMutationBatch, IndexPage, IndexQuery, IndexStore, IndexWatermark,
    RebuildIndexRequest, RebuildReceipt,
};
pub use lease::{LeaseObservation, LeaseResource, LeaseStore, LeaseToken, OwnerId};
pub use migration::{
    legal_transition, MigrationCounts, MigrationDigests, MigrationPhase, SourceWatermark,
    StorageCatalogId, StorageMigrationRecord, MIGRATION_PHASES,
};
pub use object::{
    bytes_body, ByteRange, DeleteCondition, DeleteReceipt, ObjectBodyStream, ObjectMetadata,
    ObjectRead, ObjectStore, ObjectVersion, PutCondition, PutObjectReceipt, PutObjectRequest,
    StoragePrefix,
};
pub use profile::{
    resolve, BootstrapSource, ProfileError, ProfileKind, ResolveOptions, ResolvedStorageProfile,
    StorageProfileDocument, BOOTSTRAP_ENV, CURRENT_PROFILE_SCHEMA,
};
pub use readiness::{
    can_advance_readiness, validate_readiness_evidence, AuthorityState, EvidenceKind, EvidenceLink,
    LegacySourceState, ReadinessState, READINESS_STATES,
};
pub use repository::{IdempotencyKey, Revision, SchemaStoreId};
pub use runtime::{ScopeLeaseHealth, StorageOperatorHealth, StorageRuntime, LOCAL_ADAPTER_DIR};
pub use scope_lease::{lease_resource_for_scope, ScopeLeaseManager};
pub use scratch::{
    MaterializationPurpose, MaterializedFile, ObjectRef, ScratchCapacity, ScratchLease,
    ScratchRequest, ScratchStore,
};
pub use secret::{SecretMetadata, SecretPurpose, SecretRef, SecretStore, SecretValue};
