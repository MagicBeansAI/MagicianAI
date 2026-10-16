//! Shared reviewed-candidate publication for SDK, CLI and VibeDev producers.
//!
//! Package transfer and staging deliberately create no installation authority.
//! This service is the next, separately authenticated boundary: it reruns the
//! provider-free conformance suite, resolves an exact local dependency lock and
//! publishes only an inert `ready_for_review` installation. It has no approval
//! or enablement API. VibeDev and external SDK producers therefore converge on
//! one candidate lifecycle instead of acquiring producer-specific shortcuts.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

use super::{
    authoring::verify_provider_free_candidate,
    authoring_catalog::{resolve_authoring_primitive_catalog, AuthoringDiscoveryRoots},
    authority::{AppAuthorityError, AuthenticatedAppScope},
    lifecycle::{AppInstallationLifecycle, AppInstallationStatus, AppLifecycleAttemptState},
    manifest::{AppDependencyKind, AppManifestDistribution, AppPackageCandidate, AppPackageLimits},
    models::{AppContractError, AppDigest, AppInstallationId, AppReference, AppRevision},
    package_lock::{
        lock_app_package_dependencies, AppLockedDependencySource, AppPackageLock,
        AppPackageLockError, AppPortablePackageLockClaim, AppVerifiedRegistryDependency,
    },
    package_staging::{
        admit_package_directory, AppPackageStageOutcome, AppPackageStager, AppPackageStagingError,
        StagedAppPackage,
    },
    package_transfer::AdmittedAppPackageArchive,
    portability::{
        AppPackageArchiveManifest, AppPortablePackageMember, APP_PORTABLE_ARCHIVE_VERSION,
    },
    records::{
        AppCompatibilityRequirement, AppInstallation, AppLifecycleAttempt, AppLifecycleAttemptKind,
        AppPackageRevision, AppPackageSourceKind,
    },
    registry::{
        canonical_package_revision_ref, canonical_package_revision_ref_from_identity,
        AppReadyForReviewPublication, AppRegistryError, AppRegistryPublicationOutcome,
        AppRegistryService, AppReviewableRevisionPublication, AppReviewableRevisionSourceFence,
    },
    schema_compiler::canonical_entity_schema_digest,
    system_boot_admission::{SystemSeedProvenance, TrustedSystemPackageAdmission},
    tool_catalog::{
        complete_declared_tool_evidence, AppReviewedToolCatalog, AppToolCatalogError,
        AppToolCatalogResolutionMode,
    },
};
use crate::magician_v2::{
    apps::manifest::canonical_view_schema_digest,
    artifact_v2::workspace::ArtifactV2Workspace,
    execution::{
        coding_engine::resolve_coding_repo_binding,
        file_edit::transaction::TransactionScope,
        verification::{
            attestation::{AttemptOutcome, VerificationAttestation},
            gate::{GateStatus, VerificationGate},
            ids::GateId,
            snapshot::{EntryKind, SourceSnapshot},
            store::VerificationStore,
        },
    },
};

const BUILTIN_CONTRACT_REVISION: u64 = 1;
const BUILTIN_CONTRACT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../docs/contracts/app-platform/components/v1/contract.json"
));

/// The producer is trusted context, not a request-body discriminator. External
/// callers can publish SDK bytes, but cannot claim VibeDev verification.
#[derive(Debug)]
enum AppCandidateProducer {
    ExternalSdk,
    VerifiedVibeDev {
        verification_attestation_ref: AppReference,
    },
    /// The deployment's own read-only seed root. This is the only producer
    /// that may publish `distribution: system`, and it is unreachable without
    /// a `SystemSeedProvenance` witness minted by `system_boot_admission`.
    DeploymentSystemSeed,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppCandidatePublicationOutcome {
    Created,
    AlreadyPresent,
    /// The scope already installed this package at a different revision, so the
    /// shipped revision was NOT admitted as a second installation. The existing
    /// installation is named in the receipt and keeps serving; moving it to the
    /// shipped revision is an owner-driven update.
    UpdateAvailable,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCandidatePublicationReceipt {
    pub state: AppInstallationStatus,
    pub stage_outcome: &'static str,
    pub publication_outcome: AppCandidatePublicationOutcome,
    pub package_revision_ref: AppReference,
    pub attempt_id: AppReference,
    pub installation_id: AppInstallationId,
    pub package_content_digest: AppDigest,
    pub dependency_lock_digest: AppDigest,
    pub local_publisher_identity: AppReference,
    pub source_publisher_identity: AppReference,
    pub activation_authority_granted: bool,
}

#[derive(Debug, Error)]
pub enum AppCandidatePublicationError {
    #[error(transparent)]
    Authentication(#[from] AppAuthorityError),
    #[error(transparent)]
    Staging(#[from] AppPackageStagingError),
    #[error(transparent)]
    DependencyLock(#[from] AppPackageLockError),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error("app candidate conformance failed: {0}")]
    Conformance(String),
    #[error("app candidate identity is invalid: {0}")]
    Identity(String),
    #[error("app candidate conformance worker terminated: {0}")]
    WorkerTerminated(String),
    #[error("app candidate publication store is internally inconsistent")]
    PartialPublication,
    #[error("verified VibeDev candidate handoff failed: {0}")]
    Verification(String),
    #[error(transparent)]
    ToolCatalog(#[from] AppToolCatalogError),
    #[error("embedded app contract artifact is invalid or stale: {0}")]
    EmbeddedContract(String),
}

#[derive(Clone)]
pub struct AppCandidatePublicationService {
    workspace: ArtifactV2Workspace,
    registry: AppRegistryService,
    stager: AppPackageStager,
    publication_locks: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
    tool_catalog: AppReviewedToolCatalog,
}

impl AppCandidatePublicationService {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        let registry = AppRegistryService::new(workspace.clone());
        Self::from_parts(
            workspace.clone(),
            registry,
            AppPackageStager::new(workspace),
        )
    }

    pub fn from_parts(
        workspace: ArtifactV2Workspace,
        registry: AppRegistryService,
        stager: AppPackageStager,
    ) -> Self {
        Self {
            workspace,
            registry,
            stager,
            publication_locks: Arc::new(Mutex::new(HashMap::new())),
            tool_catalog: AppReviewedToolCatalog::resolver_snapshot(),
        }
    }

    pub fn with_tool_catalog(mut self, tool_catalog: AppReviewedToolCatalog) -> Self {
        self.tool_catalog = tool_catalog;
        self
    }

    /// Stage, conform and publish one package as an inert review candidate.
    /// Exact replay returns the same identities. No branch creates a grant,
    /// approval, schedule, workflow dispatch or enabled installation.
    pub async fn publish_archive_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        admitted: AdmittedAppPackageArchive,
        now: DateTime<Utc>,
    ) -> Result<AppCandidatePublicationReceipt, AppCandidatePublicationError> {
        authenticated.ensure_live_at(&now)?;
        let (portable, candidate) = admitted.into_parts();
        self.publish_candidate(
            authenticated,
            portable,
            candidate,
            AppCandidateProducer::ExternalSdk,
            None,
            None,
            None,
            now,
        )
        .await
    }

    /// Publish a conformance-complete package against one exact existing
    /// installation. The installation must already be parked by the lifecycle
    /// owner for `Update`, or retained and inert for `Reinstall`.
    pub async fn publish_archive_revision_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        admitted: AdmittedAppPackageArchive,
        installation_id: AppInstallationId,
        kind: AppLifecycleAttemptKind,
        now: DateTime<Utc>,
    ) -> Result<AppCandidatePublicationReceipt, AppCandidatePublicationError> {
        if kind == AppLifecycleAttemptKind::InitialInstall {
            return Err(AppCandidatePublicationError::Identity(
                "an existing-installation publication must be update or reinstall".to_owned(),
            ));
        }
        authenticated.ensure_live_at(&now)?;
        let source_fence = self
            .registry
            .reviewable_revision_source_fence(authenticated, &installation_id, kind, now)
            .await?;
        let (portable, candidate) = admitted.into_parts();
        self.publish_candidate(
            authenticated,
            portable,
            candidate,
            AppCandidateProducer::ExternalSdk,
            Some(source_fence),
            None,
            None,
            now,
        )
        .await
    }

    /// Convert one exact, green VibeDev source snapshot into the same inert
    /// review candidate used by SDK uploads. This is a server-owned seam, not
    /// an HTTP producer flag: a request body cannot mint the trusted producer
    /// discriminator or supply its own attestation reference.
    pub async fn publish_verified_vibedev_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        gate_id: GateId,
        root_task_id: String,
        root_execution_id: String,
        package_relative_path: PathBuf,
        now: DateTime<Utc>,
    ) -> Result<AppCandidatePublicationReceipt, AppCandidatePublicationError> {
        authenticated.ensure_live_at(&now)?;
        let workspace = self.workspace.clone();
        let transaction_scope = TransactionScope {
            principal: authenticated.scope().principal.as_str().to_owned(),
            workspace: authenticated.scope().workspace.as_str().to_owned(),
        };
        let publisher_identity = authenticated.actor_ref().clone();
        let tool_catalog = self.tool_catalog.clone();
        let verified = tokio::task::spawn_blocking(move || {
            let tool_catalog = scoped_reviewed_tool_catalog(
                &workspace,
                &transaction_scope.principal,
                &transaction_scope.workspace,
                &tool_catalog,
            )?;
            admit_verified_vibedev_candidate(
                &workspace,
                transaction_scope,
                &gate_id,
                &root_task_id,
                &root_execution_id,
                &package_relative_path,
                publisher_identity,
                &tool_catalog,
            )
        })
        .await
        .map_err(|error| AppCandidatePublicationError::WorkerTerminated(error.to_string()))??;
        authenticated.ensure_live_at(&now)?;
        self.publish_candidate(
            authenticated,
            verified.portable,
            verified.candidate,
            AppCandidateProducer::VerifiedVibeDev {
                verification_attestation_ref: verified.verification_attestation_ref,
            },
            None,
            None,
            None,
            now,
        )
        .await
    }

    /// Publish one system package out of the deployment's read-only seed root.
    ///
    /// This is the owner named by the two refusals that read *"system
    /// distribution packages require host-controlled digest-pinned boot
    /// admission"*. It consumes the admission by value, so one resolved
    /// package cannot be published twice from the same proof.
    ///
    /// Like every other producer it publishes an **inert `ready_for_review`
    /// installation** and nothing more. Seed provenance buys the right to
    /// claim system class; it does not buy approval, enablement, or grants.
    /// That separation is the reason a compromised seed is still not a
    /// running app.
    pub async fn publish_trusted_system_package(
        &self,
        authenticated: &AuthenticatedAppScope,
        admission: TrustedSystemPackageAdmission,
        now: DateTime<Utc>,
    ) -> Result<AppCandidatePublicationReceipt, AppCandidatePublicationError> {
        authenticated.ensure_live_at(&now)?;
        let tool_catalog = tokio::task::spawn_blocking(embedded_reviewed_tool_catalog)
            .await
            .map_err(|error| AppCandidatePublicationError::WorkerTerminated(error.to_string()))??;
        self.publish_trusted_system_package_with_catalog(
            authenticated,
            admission,
            tool_catalog,
            now,
        )
        .await
    }

    /// Test-fixture seam for reproducing a package revision published by an
    /// older binary whose immutable primitive catalog differed from the
    /// current one. Production callers always use the embedded catalog above.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn publish_trusted_system_package_with_catalog_for_test(
        &self,
        authenticated: &AuthenticatedAppScope,
        admission: TrustedSystemPackageAdmission,
        tool_catalog: AppReviewedToolCatalog,
        now: DateTime<Utc>,
    ) -> Result<AppCandidatePublicationReceipt, AppCandidatePublicationError> {
        self.publish_trusted_system_package_with_catalog(
            authenticated,
            admission,
            tool_catalog,
            now,
        )
        .await
    }

    /// The installation this scope already holds for the candidate's package,
    /// including a revision adopted by an in-place update.
    ///
    /// `None` means publication should proceed normally: either the scope has
    /// never installed this package, or only holds an initial review candidate.
    ///
    /// A `ready_for_review` row is deliberately NOT treated as installed. It is
    /// an un-approved attempt serving nothing, so replacing it costs an owner
    /// no running app — and skipping on it would pin the scope to an outdated
    /// candidate it never accepted.
    async fn installed_revision_for_package(
        &self,
        authenticated: &AuthenticatedAppScope,
        portable: &AppPackageArchiveManifest,
        now: DateTime<Utc>,
    ) -> Result<Option<AppInstallation>, AppCandidatePublicationError> {
        let installations = self
            .registry
            .installations_bounded(authenticated, 256, now)
            .await?;
        // Two passes on purpose. A scope can hold BOTH a row at this exact
        // revision and an installed row at another one, and the answers differ:
        // the first means "let the replay path own it", the second means "do not
        // mint a duplicate". A single pass would return whichever row the id
        // ordering happened to reach first, making the decision depend on a
        // hash. The replay answer wins whenever both are present.
        if let Some(installation) = installations.iter().find(|installation| {
            installation.package_revision_ref == portable.package_revision_ref
                && matches!(
                    installation.lifecycle.status,
                    AppInstallationStatus::Enabled
                        | AppInstallationStatus::Disabled
                        | AppInstallationStatus::UpdatePending
                )
        }) {
            return Ok(Some(installation.clone()));
        }
        if installations
            .iter()
            .any(|installation| installation.package_revision_ref == portable.package_revision_ref)
        {
            return Ok(None);
        }
        for installation in installations {
            if !matches!(
                installation.lifecycle.status,
                AppInstallationStatus::Enabled
                    | AppInstallationStatus::Disabled
                    | AppInstallationStatus::UpdatePending
            ) {
                continue;
            }
            let Some(revision) = self
                .registry
                .package_revision(authenticated, &installation.package_revision_ref, now)
                .await?
            else {
                // An installation whose revision record is gone cannot prove
                // which package it belongs to. Skip it rather than assume it is
                // unrelated, which would mint the duplicate this exists to stop.
                continue;
            };
            if revision.package_id == portable.package_id {
                return Ok(Some(installation));
            }
        }
        Ok(None)
    }

    async fn publish_trusted_system_package_with_catalog(
        &self,
        authenticated: &AuthenticatedAppScope,
        admission: TrustedSystemPackageAdmission,
        tool_catalog: AppReviewedToolCatalog,
        now: DateTime<Utc>,
    ) -> Result<AppCandidatePublicationReceipt, AppCandidatePublicationError> {
        authenticated.ensure_live_at(&now)?;
        let (_package_dir, candidate, provenance) = admission.into_parts();
        let publisher_identity = authenticated.actor_ref().clone();

        // A seed package has no portable archive, exactly like a VibeDev
        // publication; both build the same local manifest from admitted bytes.
        // Its dependency evidence comes from the binary's embedded catalog
        // alone, never from the scope's live skills, agent templates or
        // artifact reviews: a host-owned package declares only embedded
        // primitives and the builtin contract, so its lock is a function of
        // the binary and the seed bytes, identical in every scope and on every
        // boot of the same build. Resolving it against the live scope made the
        // lock move with the scope, and with one revision per package version
        // that refused every system package at boot.
        let locking_catalog = tool_catalog.clone();
        let (candidate, fresh_lock) = tokio::task::spawn_blocking(move || {
            let dependency_lock = local_dependency_lock(&candidate, Vec::new(), &locking_catalog)?;
            Ok::<_, AppCandidatePublicationError>((candidate, dependency_lock))
        })
        .await
        .map_err(|error| AppCandidatePublicationError::WorkerTerminated(error.to_string()))??;

        authenticated.ensure_live_at(&now)?;
        let dependency_lock = self
            .reconcile_trusted_seed_lock(authenticated, &candidate, fresh_lock, now)
            .await?;
        let (portable, candidate) = tokio::task::spawn_blocking(move || {
            let portable =
                build_local_archive_manifest(&candidate, &dependency_lock, publisher_identity)?;
            Ok::<_, AppCandidatePublicationError>((portable, candidate))
        })
        .await
        .map_err(|error| AppCandidatePublicationError::WorkerTerminated(error.to_string()))??;

        authenticated.ensure_live_at(&now)?;
        // A scope keeps at most ONE installation per system package.
        //
        // `installation_id` is derived from `package_revision_ref`, so changed
        // seed bytes mint a different id and every publication looked like a
        // first-time install. The scope then accumulated one installation per
        // revision: the previously approved one kept serving as `enabled` while
        // the new one sat in `ready_for_review` forever, which is why a package
        // could appear simultaneously under both "Installed" and "Needs
        // attention" and why five packages had ten installations between them.
        //
        // Admitting the revision is not the same as installing it. When the
        // scope already holds this package at another revision, publish nothing
        // and leave that installation exactly as it is. Moving it to the
        // shipped revision means parking it (`BeginUpdate`), which takes the app
        // out of service until an owner reviews — a deploy must not do that to a
        // running app unasked.
        if let Some(existing) = self
            .installed_revision_for_package(authenticated, &portable, now)
            .await?
        {
            // An update retains the original installation ID. Replaying it as
            // an initial candidate would look for the new revision-derived ID
            // and falsely report a partially published store.
            if existing.package_revision_ref == portable.package_revision_ref
                && matches!(
                    existing.lifecycle.status,
                    AppInstallationStatus::Enabled | AppInstallationStatus::Disabled
                )
            {
                let attempt = self
                    .registry
                    .committed_attempt_for_installation(
                        authenticated,
                        &existing.installation_id,
                        now,
                    )
                    .await?
                    .ok_or(AppCandidatePublicationError::PartialPublication)?;
                if attempt.candidate_package_revision_ref != portable.package_revision_ref {
                    return Err(AppCandidatePublicationError::PartialPublication);
                }
                return Ok(AppCandidatePublicationReceipt {
                    state: existing.lifecycle.status,
                    stage_outcome: "already_present",
                    publication_outcome: AppCandidatePublicationOutcome::AlreadyPresent,
                    package_revision_ref: portable.package_revision_ref.clone(),
                    attempt_id: attempt.attempt_id,
                    installation_id: existing.installation_id,
                    package_content_digest: portable.package_content_digest.clone(),
                    dependency_lock_digest: portable.dependency_lock_digest.clone(),
                    local_publisher_identity: authenticated.actor_ref().clone(),
                    source_publisher_identity: portable.publisher_identity.clone(),
                    activation_authority_granted: false,
                });
            }
            // BeginUpdate can outlive the upload/publication that was meant to
            // follow it. Complete that already requested update with the trusted
            // seed, using the ordinary generation-fenced revision publisher.
            // Enabled/disabled installs are never parked by boot, and an
            // existing review candidate is never replaced behind the owner.
            if existing.lifecycle.status == AppInstallationStatus::UpdatePending
                && self
                    .registry
                    .ready_for_review_attempt_for_installation(
                        authenticated,
                        &existing.installation_id,
                        now,
                    )
                    .await?
                    .is_none()
            {
                let source_fence = self
                    .registry
                    .reviewable_revision_source_fence(
                        authenticated,
                        &existing.installation_id,
                        AppLifecycleAttemptKind::Update,
                        now,
                    )
                    .await?;
                return self
                    .publish_candidate(
                        authenticated,
                        portable,
                        candidate,
                        AppCandidateProducer::DeploymentSystemSeed,
                        Some(source_fence),
                        Some(&provenance),
                        Some(tool_catalog),
                        now,
                    )
                    .await;
            }
            return Ok(AppCandidatePublicationReceipt {
                state: existing.lifecycle.status,
                stage_outcome: "update_available",
                publication_outcome: AppCandidatePublicationOutcome::UpdateAvailable,
                package_revision_ref: portable.package_revision_ref.clone(),
                attempt_id: derived_identity(
                    "attempt:app-candidate",
                    &portable.package_revision_ref,
                )?,
                installation_id: existing.installation_id.clone(),
                package_content_digest: portable.package_content_digest.clone(),
                dependency_lock_digest: portable.dependency_lock_digest.clone(),
                local_publisher_identity: authenticated.actor_ref().clone(),
                source_publisher_identity: portable.publisher_identity.clone(),
                activation_authority_granted: false,
            });
        }
        self.publish_candidate(
            authenticated,
            portable,
            candidate,
            AppCandidateProducer::DeploymentSystemSeed,
            None,
            Some(&provenance),
            Some(tool_catalog),
            now,
        )
        .await
    }

    /// A seed package is re-published on every boot from the same admitted
    /// bytes, and the registry keeps exactly one revision per package version.
    /// A version seals both those bytes and their dependency lock. Refuse lock
    /// drift at this boundary with the actionable identity instead of passing
    /// the published lock to `prepare_candidate`, where the current catalog
    /// can only reject it as generically non-reproducible. A deliberate re-lock
    /// must ship as a version bump. Changed bytes under the same version are
    /// likewise refused with both digests.
    async fn reconcile_trusted_seed_lock(
        &self,
        authenticated: &AuthenticatedAppScope,
        candidate: &AppPackageCandidate,
        fresh_lock: AppPackageLock,
        now: DateTime<Utc>,
    ) -> Result<AppPackageLock, AppCandidatePublicationError> {
        let package = candidate.manifest().manifest();
        let package_id = AppReference::parse(format!("app:{}", package.name))?;
        let Some(published) = self
            .registry
            .package_revision_for_version(authenticated, &package_id, &package.version, now)
            .await?
        else {
            return Ok(fresh_lock);
        };
        if published.content_digest != *candidate.bundle_digest() {
            return Err(AppCandidatePublicationError::Identity(format!(
                "system seed {} {} changed bytes without a version bump (published {}, admitted {}); bump the package version",
                package_id.as_str(),
                package.version,
                published.content_digest.as_str(),
                candidate.bundle_digest().as_str()
            )));
        }
        if published.dependency_lock_digest == *fresh_lock.lock_digest() {
            return Ok(fresh_lock);
        }
        Err(AppCandidatePublicationError::Identity(format!(
            "system seed {} {} resolved a different dependency lock without a version bump (published {}, admitted {}); bump the package version to re-lock",
            package_id.as_str(),
            package.version,
            published.dependency_lock_digest.as_str(),
            fresh_lock.lock_digest().as_str()
        )))
    }

    async fn publish_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        portable: AppPackageArchiveManifest,
        candidate: AppPackageCandidate,
        producer: AppCandidateProducer,
        source_fence: Option<AppReviewableRevisionSourceFence>,
        seed_provenance: Option<&SystemSeedProvenance>,
        dependency_catalog: Option<AppReviewedToolCatalog>,
        now: DateTime<Utc>,
    ) -> Result<AppCandidatePublicationReceipt, AppCandidatePublicationError> {
        authenticated.ensure_live_at(&now)?;
        // Producer is trusted context. The manifest class is only a claim:
        // uploads and VibeDev output must never mint system status or be
        // silently downgraded to installable. `DeploymentSystemSeed` is the
        // one producer that carries real provenance, and its own inverse gate
        // lives in `stage_trusted_system_candidate` — it admits system class
        // and nothing else.
        // Seed provenance is the discriminator, not the producer label: the
        // witness cannot be constructed outside `system_boot_admission`, while
        // an enum variant could be named by any future caller in this module.
        if seed_provenance.is_none() {
            validate_untrusted_candidate_distribution(
                candidate.manifest().manifest().app.distribution,
            )?;
        }
        let registry_evidence = self
            .trusted_registry_dependencies(
                authenticated,
                &candidate,
                portable.dependency_lock.claimed_lock(),
                dependency_catalog,
                now,
            )
            .await?;
        authenticated.ensure_live_at(&now)?;
        let source_publisher_identity = portable.publisher_identity.clone();
        let local_publisher_identity = authenticated.actor_ref().clone();
        let staged = match seed_provenance {
            Some(provenance) => {
                self.stager
                    .stage_trusted_system_candidate(authenticated, candidate, provenance, now)
                    .await?
            },
            None => {
                self.stager
                    .stage_candidate(authenticated, candidate, now)
                    .await?
            },
        };
        let stage_outcome = stage_outcome_label(staged.outcome());
        let prepared = tokio::task::spawn_blocking(move || {
            prepare_candidate(
                portable,
                staged,
                producer,
                local_publisher_identity,
                registry_evidence,
            )
        })
        .await
        .map_err(|error| AppCandidatePublicationError::WorkerTerminated(error.to_string()))??;

        let lock_identity = source_fence
            .as_ref()
            .map(|fence| &fence.fence_digest)
            .unwrap_or(&prepared.package_revision.content_digest);
        let lock = self.publication_lock(
            authenticated.scope_binding_ref(),
            &AppReference::parse(format!(
                "candidate-lock:{}",
                lock_identity.as_str().trim_start_matches("blake3:")
            ))?,
        );
        let _guard = lock.lock().await;
        if let Some(source_fence) = source_fence {
            return self
                .publish_prepared_revision_candidate(
                    authenticated,
                    prepared,
                    source_fence,
                    source_publisher_identity,
                    stage_outcome,
                    now,
                )
                .await;
        }
        if let Some(receipt) = self
            .existing_publication(authenticated, &prepared, now)
            .await?
        {
            return Ok(AppCandidatePublicationReceipt {
                state: receipt.state,
                stage_outcome,
                publication_outcome: AppCandidatePublicationOutcome::AlreadyPresent,
                package_revision_ref: receipt.package_revision_ref,
                attempt_id: receipt.attempt_id,
                installation_id: receipt.installation_id,
                package_content_digest: prepared.package_revision.content_digest,
                dependency_lock_digest: prepared.package_revision.dependency_lock_digest,
                local_publisher_identity: prepared.package_revision.publisher_identity,
                source_publisher_identity,
                activation_authority_granted: false,
            });
        }

        authenticated.ensure_live_at(&now)?;
        let publication = prepared.into_publication(authenticated, now)?;
        let package_content_digest = publication.package_revision().content_digest.clone();
        let dependency_lock_digest = publication
            .package_revision()
            .dependency_lock_digest
            .clone();
        let local_publisher_identity = publication.package_revision().publisher_identity.clone();
        let receipt = self
            .registry
            .publish_ready_for_review(authenticated, publication, now)
            .await?;
        Ok(AppCandidatePublicationReceipt {
            state: AppInstallationStatus::ReadyForReview,
            stage_outcome,
            publication_outcome: match receipt.outcome {
                AppRegistryPublicationOutcome::Created => AppCandidatePublicationOutcome::Created,
                AppRegistryPublicationOutcome::AlreadyPresent => {
                    AppCandidatePublicationOutcome::AlreadyPresent
                },
            },
            package_revision_ref: receipt.package_revision_ref,
            attempt_id: receipt.attempt_id,
            installation_id: receipt.installation_id,
            package_content_digest,
            dependency_lock_digest,
            local_publisher_identity,
            source_publisher_identity,
            activation_authority_granted: false,
        })
    }

    async fn publish_prepared_revision_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        prepared: PreparedCandidate,
        source_fence: AppReviewableRevisionSourceFence,
        source_publisher_identity: AppReference,
        stage_outcome: &'static str,
        now: DateTime<Utc>,
    ) -> Result<AppCandidatePublicationReceipt, AppCandidatePublicationError> {
        let attempt_id = revision_attempt_id(&prepared.package_revision_ref, &source_fence)?;
        if let Some(existing) = self
            .registry
            .lifecycle_attempt(authenticated, &attempt_id, now)
            .await?
        {
            let source_diff_ref = source_fence.permission_migration_diff_ref()?;
            if existing.kind != source_fence.attempt_kind
                || existing.installation_id.as_ref() != Some(&source_fence.installation_id)
                || existing.source_installation_generation
                    != Some(source_fence.source_installation_generation)
                || existing.candidate_package_revision_ref != prepared.package_revision_ref
                || existing.state != AppLifecycleAttemptState::ReadyForReview
                || existing.permission_migration_diff_ref.as_ref() != Some(&source_diff_ref)
            {
                return Err(AppCandidatePublicationError::Identity(
                    "an existing update/reinstall candidate identity has different source bytes"
                        .to_owned(),
                ));
            }
            return Ok(AppCandidatePublicationReceipt {
                state: match source_fence.attempt_kind {
                    AppLifecycleAttemptKind::Update => AppInstallationStatus::UpdatePending,
                    AppLifecycleAttemptKind::Reinstall => {
                        AppInstallationStatus::UninstalledRetained
                    },
                    AppLifecycleAttemptKind::InitialInstall => unreachable!(),
                },
                stage_outcome,
                publication_outcome: AppCandidatePublicationOutcome::AlreadyPresent,
                package_revision_ref: prepared.package_revision_ref,
                attempt_id,
                installation_id: source_fence.installation_id,
                package_content_digest: prepared.package_revision.content_digest,
                dependency_lock_digest: prepared.package_revision.dependency_lock_digest,
                local_publisher_identity: prepared.package_revision.publisher_identity,
                source_publisher_identity,
                activation_authority_granted: false,
            });
        }
        let state = match source_fence.attempt_kind {
            AppLifecycleAttemptKind::Update => AppInstallationStatus::UpdatePending,
            AppLifecycleAttemptKind::Reinstall => AppInstallationStatus::UninstalledRetained,
            AppLifecycleAttemptKind::InitialInstall => unreachable!(),
        };
        let package_content_digest = prepared.package_revision.content_digest.clone();
        let dependency_lock_digest = prepared.package_revision.dependency_lock_digest.clone();
        let local_publisher_identity = prepared.package_revision.publisher_identity.clone();
        let installation_id = source_fence.installation_id.clone();
        let package_created_at = match self
            .registry
            .package_revision(authenticated, &prepared.package_revision_ref, now)
            .await?
        {
            Some(existing) => {
                let mut expected = prepared.package_revision.clone();
                expected.created_at = existing.created_at;
                if expected != existing {
                    return Err(AppCandidatePublicationError::Identity(
                        "an existing package revision differs from the reviewed update candidate"
                            .to_owned(),
                    ));
                }
                existing.created_at
            },
            None => now,
        };
        let publication = prepared.into_revision_publication(
            source_fence,
            attempt_id,
            package_created_at,
            now,
        )?;
        authenticated.ensure_live_at(&now)?;
        let receipt = self
            .registry
            .publish_reviewable_revision(authenticated, publication, now)
            .await?;
        Ok(AppCandidatePublicationReceipt {
            state,
            stage_outcome,
            publication_outcome: match receipt.outcome {
                AppRegistryPublicationOutcome::Created => AppCandidatePublicationOutcome::Created,
                AppRegistryPublicationOutcome::AlreadyPresent => {
                    AppCandidatePublicationOutcome::AlreadyPresent
                },
            },
            package_revision_ref: receipt.package_revision_ref,
            attempt_id: receipt.attempt_id,
            installation_id,
            package_content_digest,
            dependency_lock_digest,
            local_publisher_identity,
            source_publisher_identity,
            activation_authority_granted: false,
        })
    }

    async fn trusted_registry_dependencies(
        &self,
        authenticated: &AuthenticatedAppScope,
        candidate: &AppPackageCandidate,
        portable_lock: &AppPackageLock,
        dependency_catalog: Option<AppReviewedToolCatalog>,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppVerifiedRegistryDependency>, AppCandidatePublicationError> {
        let procedure_refs = portable_lock
            .dependencies()
            .iter()
            .filter(|dependency| {
                dependency.kind() == AppDependencyKind::ProcedureSkill
                    && matches!(
                        dependency.source(),
                        AppLockedDependencySource::RegistryRevision { .. }
                    )
            })
            .map(|dependency| dependency.dependency_ref().clone())
            .collect::<Vec<_>>();
        let revisions = self
            .registry
            .locked_procedure_revisions(authenticated, portable_lock, &procedure_refs, now)
            .await?;
        let mut evidence = revisions
            .iter()
            .map(|revision| revision.dependency_evidence())
            .collect::<Vec<_>>();
        for dependency in portable_lock.dependencies() {
            if dependency.kind() != AppDependencyKind::Capability {
                continue;
            }
            let AppLockedDependencySource::RegistryRevision {
                immutable_revision_ref,
                ..
            } = dependency.source()
            else {
                continue;
            };
            // A scoped resolver source is reproduced from the same immutable
            // primitive snapshot below, not fetched through the standalone
            // registry. The final dependency-lock digest comparison still
            // requires the exact source ref, semantic version and bytes.
            if immutable_revision_ref
                .as_str()
                .starts_with("primitive-source:skill:")
            {
                continue;
            }
            // Same reasoning for an embedded compiled pack. `snapshot_compiled_pack`
            // mints `compiled-revision:<name>-<digest>` over the pack YAML that is
            // compiled INTO this binary, and `complete_declared_tool_evidence`
            // below re-derives exactly that evidence from those same bytes. There
            // is no registry record to find, and demanding one refused every
            // package depending on a host binder -- which is every system package
            // the platform ships. Reproducing from the binary is strictly stronger
            // evidence than a scoped registry row, and the final lock-digest
            // comparison is unchanged.
            if immutable_revision_ref
                .as_str()
                .starts_with("compiled-revision:")
            {
                continue;
            }
            let (receipt, bytes) = self
                .registry
                .resolve_standalone_skill_document(authenticated, immutable_revision_ref, now)
                .await?;
            super::skill_dependencies::AppStandaloneCapabilityCandidate::admit_untrusted(&bytes)
                .map_err(|error| AppCandidatePublicationError::Identity(error.to_string()))?;
            evidence.push(AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Capability,
                receipt.dependency_ref,
                receipt.semantic_version,
                receipt.immutable_revision_ref,
                receipt.revision,
                &bytes,
            )?);
        }
        let tool_catalog = if let Some(tool_catalog) = dependency_catalog {
            tool_catalog
        } else if self.tool_catalog.mode()
            == AppToolCatalogResolutionMode::ExternalImmutableEvidenceOnly
        {
            self.tool_catalog.clone()
        } else {
            let workspace = self.workspace.clone();
            let principal = authenticated.scope().principal.as_str().to_owned();
            let scope_workspace = authenticated.scope().workspace.as_str().to_owned();
            let configured = self.tool_catalog.clone();
            tokio::task::spawn_blocking(move || {
                scoped_reviewed_tool_catalog(&workspace, &principal, &scope_workspace, &configured)
            })
            .await
            .map_err(|error| AppCandidatePublicationError::WorkerTerminated(error.to_string()))??
        };
        Ok(complete_declared_tool_evidence(
            candidate,
            evidence,
            &tool_catalog,
        )?)
    }

    fn publication_lock(
        &self,
        scope_binding: &super::models::AppScopeBindingRef,
        package_revision_ref: &AppReference,
    ) -> Arc<AsyncMutex<()>> {
        let key = format!(
            "{}\0{}",
            scope_binding.as_str(),
            package_revision_ref.as_str()
        );
        let mut locks = self
            .publication_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        locks.retain(|_, weak| weak.strong_count() > 0);
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }

    async fn existing_publication(
        &self,
        authenticated: &AuthenticatedAppScope,
        prepared: &PreparedCandidate,
        now: DateTime<Utc>,
    ) -> Result<Option<ExistingPublication>, AppCandidatePublicationError> {
        let package = self
            .registry
            .package_revision(authenticated, &prepared.package_revision_ref, now)
            .await?;
        let attempt = self
            .registry
            .lifecycle_attempt(authenticated, &prepared.attempt_id, now)
            .await?;
        let installation = self
            .registry
            .installation(authenticated, &prepared.installation_id, now)
            .await?;
        match (package, attempt, installation) {
            (None, None, None) => Ok(None),
            (Some(package), Some(attempt), Some(installation)) => {
                if !prepared.matches_existing(authenticated, &package, &attempt, &installation) {
                    return Err(AppCandidatePublicationError::Identity(
                        "an existing candidate identity has different conformance bytes".to_owned(),
                    ));
                }
                Ok(Some(ExistingPublication {
                    package_revision_ref: prepared.package_revision_ref.clone(),
                    attempt_id: prepared.attempt_id.clone(),
                    installation_id: prepared.installation_id.clone(),
                    state: installation.lifecycle.status,
                }))
            },
            // Whole-installation purge deliberately deletes lifecycle
            // attempts but retains the immutable package revision and a
            // purged installation tombstone. A bundled seed is replayed on
            // every boot, so recognize only that exact terminal shape rather
            // than reporting a corrupt partial publication or resurrecting
            // an installation the owner explicitly purged.
            (Some(package), None, Some(installation))
                if prepared.matches_existing_bundled_purge(
                    authenticated,
                    &package,
                    &installation,
                ) =>
            {
                Ok(Some(ExistingPublication {
                    package_revision_ref: prepared.package_revision_ref.clone(),
                    attempt_id: prepared.attempt_id.clone(),
                    installation_id: prepared.installation_id.clone(),
                    state: AppInstallationStatus::Purged,
                }))
            },
            _ => Err(AppCandidatePublicationError::PartialPublication),
        }
    }
}

fn validate_untrusted_candidate_distribution(
    distribution: AppManifestDistribution,
) -> Result<(), AppCandidatePublicationError> {
    if distribution == AppManifestDistribution::System {
        return Err(AppCandidatePublicationError::Conformance(
            "system-distribution packages require host-controlled digest-pinned boot admission"
                .to_owned(),
        ));
    }
    Ok(())
}

/// The catalog a trusted seed locks against: the embedded compiled packs and
/// the default agent, resolved from zero discovery roots. No scope skill, agent
/// template copy or artifact review can enter it, so a system package that
/// declares one fails to publish loudly instead of locking mutable bytes.
fn embedded_reviewed_tool_catalog() -> Result<AppReviewedToolCatalog, AppToolCatalogError> {
    let roots = AuthoringDiscoveryRoots::from_explicit(
        std::iter::empty::<std::path::PathBuf>(),
        std::iter::empty::<std::path::PathBuf>(),
    );
    let snapshot = resolve_authoring_primitive_catalog(&roots);
    AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
}

fn scoped_reviewed_tool_catalog(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope_workspace: &str,
    configured: &AppReviewedToolCatalog,
) -> Result<AppReviewedToolCatalog, AppToolCatalogError> {
    if configured.mode() == AppToolCatalogResolutionMode::ExternalImmutableEvidenceOnly {
        return Ok(configured.clone());
    }
    let roots = AuthoringDiscoveryRoots::for_workspace_scope(workspace, principal, scope_workspace);
    let snapshot = resolve_authoring_primitive_catalog(&roots);
    match super::os_jail::AppOsJailArtifactStore::open_or_create(
        &workspace.apps_root(principal, scope_workspace),
    ) {
        Ok(artifact_store) => configured
            .resolved_with_snapshot_and_artifact_store(&snapshot, Arc::new(artifact_store)),
        // Store unavailability disables only OS-jail artifact evidence. The
        // package may still publish with inert ToolSkill bindings and fully
        // reviewed compiled primitives; installation review explains the
        // missing physical evidence rather than broadening authority.
        Err(_) => AppReviewedToolCatalog::from_primitive_snapshot(&snapshot),
    }
}

struct VerifiedVibeDevCandidate {
    portable: AppPackageArchiveManifest,
    candidate: AppPackageCandidate,
    verification_attestation_ref: AppReference,
}

#[allow(clippy::too_many_arguments)]
fn admit_verified_vibedev_candidate(
    workspace: &ArtifactV2Workspace,
    scope: TransactionScope,
    gate_id: &GateId,
    root_task_id: &str,
    root_execution_id: &str,
    package_relative_path: &Path,
    publisher_identity: AppReference,
    tool_catalog: &AppReviewedToolCatalog,
) -> Result<VerifiedVibeDevCandidate, AppCandidatePublicationError> {
    let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
    let store = VerificationStore::new(scope_root.clone(), scope.clone());
    let gate = store
        .load_gate(gate_id)
        .map_err(|error| AppCandidatePublicationError::Verification(error.to_string()))?;
    let attestation_id = gate.active_attestation_ref.as_ref().ok_or_else(|| {
        AppCandidatePublicationError::Verification(
            "verified gate has no active attestation reference".to_owned(),
        )
    })?;
    let attestation = store
        .load_attestation(attestation_id)
        .map_err(|error| AppCandidatePublicationError::Verification(error.to_string()))?;
    validate_verified_vibedev_evidence(
        &scope,
        &gate,
        &attestation,
        root_task_id,
        root_execution_id,
    )?;

    let binding = resolve_coding_repo_binding(&scope_root, Some(&gate.project_binding))
        .map_err(AppCandidatePublicationError::Verification)?;
    let verified_snapshot = SourceSnapshot::capture(&binding.real_path, &[])
        .map_err(|error| AppCandidatePublicationError::Verification(error.to_string()))?;
    if verified_snapshot.digest() != attestation.key.snapshot_digest {
        return Err(AppCandidatePublicationError::Verification(
            "the VibeDev repository no longer matches the accepted green snapshot".to_owned(),
        ));
    }

    let normalized_package_path = normalize_package_relative_path(package_relative_path)?;
    let package_root = binding.real_path.join(&normalized_package_path);
    let canonical_package_root = std::fs::canonicalize(&package_root).map_err(|error| {
        AppCandidatePublicationError::Verification(format!(
            "app package directory `{}` is unavailable: {error}",
            package_root.display()
        ))
    })?;
    if !canonical_package_root.starts_with(&binding.real_path) || !canonical_package_root.is_dir() {
        return Err(AppCandidatePublicationError::Verification(
            "app package directory must remain inside the verified repository".to_owned(),
        ));
    }
    let candidate = admit_package_directory(&canonical_package_root)?;
    ensure_candidate_matches_snapshot(&candidate, &verified_snapshot, &normalized_package_path)?;

    // The member-by-member comparison above proves which bytes were admitted.
    // Recapturing also refuses publication when the verified source tree was
    // left changed while this bounded handoff ran.
    let after = SourceSnapshot::capture(&binding.real_path, &[])
        .map_err(|error| AppCandidatePublicationError::Verification(error.to_string()))?;
    if after.digest() != attestation.key.snapshot_digest {
        return Err(AppCandidatePublicationError::Verification(
            "the VibeDev repository changed during candidate admission".to_owned(),
        ));
    }

    let dependency_lock = local_dependency_lock(&candidate, Vec::new(), tool_catalog)?;
    let portable = build_local_archive_manifest(&candidate, &dependency_lock, publisher_identity)?;
    let verification_attestation_ref = AppReference::parse(format!(
        "attestation:vibedev:{}",
        attestation.attestation_id.as_str()
    ))?;
    Ok(VerifiedVibeDevCandidate {
        portable,
        candidate,
        verification_attestation_ref,
    })
}

pub fn validate_verified_vibedev_evidence(
    expected_scope: &TransactionScope,
    gate: &VerificationGate,
    attestation: &VerificationAttestation,
    root_task_id: &str,
    root_execution_id: &str,
) -> Result<(), AppCandidatePublicationError> {
    if gate.status != GateStatus::Verified
        || &gate.scope != expected_scope
        || gate.root_task_id != root_task_id
        || gate.root_execution_id != root_execution_id
        || gate.active_attestation_ref.as_ref() != Some(&attestation.attestation_id)
        || attestation.key.scope != *expected_scope
        || attestation.key.project_binding != gate.project_binding
        || attestation.key.candidate_ref != gate.current_candidate.candidate_ref
    {
        return Err(AppCandidatePublicationError::Verification(
            "gate, scope, root execution, candidate, project and attestation identity must match \
             exactly"
                .to_owned(),
        ));
    }
    let accepted = attestation.accepted_result.as_ref().ok_or_else(|| {
        AppCandidatePublicationError::Verification(
            "the active verification attestation is not sealed".to_owned(),
        )
    })?;
    let attempt = attestation.attempt(&accepted.attempt_id).ok_or_else(|| {
        AppCandidatePublicationError::Verification(
            "the accepted verification attempt is missing".to_owned(),
        )
    })?;
    if accepted.outcome != AttemptOutcome::Green
        || attempt.outcome != AttemptOutcome::Green
        || accepted.generation != gate.generation
        || attempt.generation != accepted.generation
        || attempt.settled_at.is_none()
    {
        return Err(AppCandidatePublicationError::Verification(
            "only the exact settled green attempt at the gate's current generation may publish"
                .to_owned(),
        ));
    }
    Ok(())
}

pub fn normalize_package_relative_path(
    path: &Path,
) -> Result<PathBuf, AppCandidatePublicationError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {},
            Component::Normal(segment) if segment.to_str().is_some() => normalized.push(segment),
            _ => {
                return Err(AppCandidatePublicationError::Verification(
                    "app package path must be a UTF-8 relative path without parent traversal"
                        .to_owned(),
                ));
            },
        }
    }
    Ok(normalized)
}

fn ensure_candidate_matches_snapshot(
    candidate: &AppPackageCandidate,
    snapshot: &SourceSnapshot,
    package_relative_path: &Path,
) -> Result<(), AppCandidatePublicationError> {
    let prefix = package_relative_path
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => segment.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    let members = candidate
        .members()
        .iter()
        .map(|member| (member.path().as_str(), member))
        .collect::<HashMap<_, _>>();
    let mut expected_paths = HashSet::new();
    for (snapshot_path, entry) in &snapshot.entries {
        let Some(member_path) = snapshot_path.strip_prefix(&prefix) else {
            continue;
        };
        if member_path.is_empty() {
            continue;
        }
        expected_paths.insert(member_path);
        let member = members.get(member_path).ok_or_else(|| {
            AppCandidatePublicationError::Verification(format!(
                "verified package member `{member_path}` was not admitted"
            ))
        })?;
        let expected_digest = member
            .content_digest()
            .as_str()
            .strip_prefix("blake3:")
            .unwrap_or(member.content_digest().as_str());
        if entry.kind != EntryKind::File
            || entry.digest != expected_digest
            || entry.size != u64::try_from(member.bytes().len()).unwrap_or(u64::MAX)
        {
            return Err(AppCandidatePublicationError::Verification(format!(
                "admitted package member `{member_path}` differs from the verified snapshot"
            )));
        }
    }
    if expected_paths.len() != members.len()
        || members.keys().any(|path| !expected_paths.contains(*path))
    {
        return Err(AppCandidatePublicationError::Verification(
            "the admitted package member set differs from the verified snapshot".to_owned(),
        ));
    }
    Ok(())
}

fn build_local_archive_manifest(
    candidate: &AppPackageCandidate,
    dependency_lock: &AppPackageLock,
    publisher_identity: AppReference,
) -> Result<AppPackageArchiveManifest, AppCandidatePublicationError> {
    let package = candidate.manifest().manifest();
    let package_id = AppReference::parse(format!("app:{}", package.name))?;
    let package_revision_ref = canonical_package_revision_ref_from_identity(
        &package_id,
        &package.version,
        candidate.bundle_digest(),
        dependency_lock.lock_digest(),
    )?;
    let members = candidate
        .members()
        .iter()
        .map(|member| AppPortablePackageMember {
            path: member.path().clone(),
            content_digest: member.content_digest().clone(),
            byte_len: u64::try_from(member.bytes().len()).unwrap_or(u64::MAX),
        })
        .collect();
    let mut portable = AppPackageArchiveManifest {
        archive_version: APP_PORTABLE_ARCHIVE_VERSION,
        package_revision_ref,
        package_id,
        publisher_identity,
        semantic_version: package.version.clone(),
        package_content_digest: candidate.bundle_digest().clone(),
        manifest_digest: candidate.manifest().manifest_digest().clone(),
        dependency_lock: AppPortablePackageLockClaim::from_trusted(dependency_lock),
        dependency_lock_digest: dependency_lock.lock_digest().clone(),
        members,
        advisory_verification_evidence: Vec::new(),
        logical_payload_digest: AppDigest::blake3(b"pending"),
    };
    portable.logical_payload_digest = portable
        .recompute_digest()
        .map_err(|error| AppCandidatePublicationError::Identity(error.to_string()))?;
    Ok(portable)
}

struct ExistingPublication {
    package_revision_ref: AppReference,
    attempt_id: AppReference,
    installation_id: AppInstallationId,
    state: AppInstallationStatus,
}

struct PreparedCandidate {
    staged: StagedAppPackage,
    dependency_lock: AppPackageLock,
    package_revision_ref: AppReference,
    package_revision: AppPackageRevision,
    attempt_id: AppReference,
    installation_id: AppInstallationId,
    conformance_attestation_ref: AppReference,
    permission_diff_ref: AppReference,
}

impl PreparedCandidate {
    fn into_publication(
        mut self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<AppReadyForReviewPublication, AppCandidatePublicationError> {
        self.package_revision.created_at = now;
        let attempt = AppLifecycleAttempt {
            attempt_id: self.attempt_id,
            kind: AppLifecycleAttemptKind::InitialInstall,
            installation_id: None,
            source_installation_generation: None,
            candidate_package_revision_ref: self.package_revision_ref.clone(),
            state: AppLifecycleAttemptState::ReadyForReview,
            conformance_attestation_ref: Some(self.conformance_attestation_ref),
            permission_migration_diff_ref: Some(self.permission_diff_ref),
            approval_ref: None,
            failure_code: None,
            created_at: now,
            updated_at: now,
        };
        let installation = AppInstallation {
            scope: authenticated.scope().clone(),
            installation_id: self.installation_id,
            package_revision_ref: self.package_revision_ref,
            lifecycle: AppInstallationLifecycle::ready_for_review(),
            grant_revision: None,
            active_schema_revision: None,
            active_surface_revision: None,
            created_at: now,
            updated_at: now,
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: None,
            purged_at: None,
        };
        AppReadyForReviewPublication::from_verified_conformance(
            self.package_revision,
            self.staged,
            &self.dependency_lock,
            attempt,
            installation,
        )
        .map_err(AppCandidatePublicationError::from)
    }

    fn into_revision_publication(
        mut self,
        source_fence: AppReviewableRevisionSourceFence,
        attempt_id: AppReference,
        package_created_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<AppReviewableRevisionPublication, AppCandidatePublicationError> {
        self.package_revision.created_at = package_created_at;
        let source_diff_ref = source_fence.permission_migration_diff_ref()?;
        let attempt = AppLifecycleAttempt {
            attempt_id,
            kind: source_fence.attempt_kind,
            installation_id: Some(source_fence.installation_id.clone()),
            source_installation_generation: Some(source_fence.source_installation_generation),
            candidate_package_revision_ref: self.package_revision_ref,
            state: AppLifecycleAttemptState::ReadyForReview,
            conformance_attestation_ref: Some(self.conformance_attestation_ref),
            permission_migration_diff_ref: Some(source_diff_ref),
            approval_ref: None,
            failure_code: None,
            created_at: now,
            updated_at: now,
        };
        AppReviewableRevisionPublication::from_verified_conformance(
            self.package_revision,
            self.staged,
            &self.dependency_lock,
            attempt,
            source_fence,
        )
        .map_err(AppCandidatePublicationError::from)
    }

    fn matches_existing(
        &self,
        authenticated: &AuthenticatedAppScope,
        package: &AppPackageRevision,
        attempt: &AppLifecycleAttempt,
        installation: &AppInstallation,
    ) -> bool {
        let mut expected_package = self.package_revision.clone();
        expected_package.created_at = package.created_at;
        // Publication owns the immutable attempt identity, not its later
        // reviewed lifecycle progress. Exact replay remains valid after an
        // owner or trusted-system host commits the attempt; requiring the
        // original ReadyForReview state makes every enabled seed fail on the
        // next boot even though none of its candidate bytes changed.
        let attempt_progress_matches = match attempt.state {
            AppLifecycleAttemptState::ReadyForReview => {
                attempt.approval_ref.is_none()
                    && attempt.failure_code.is_none()
                    && installation.lifecycle.status == AppInstallationStatus::ReadyForReview
            },
            AppLifecycleAttemptState::Committed => {
                attempt.approval_ref.is_some()
                    && attempt.failure_code.is_none()
                    && installation.lifecycle.status != AppInstallationStatus::ReadyForReview
            },
            AppLifecycleAttemptState::Staged
            | AppLifecycleAttemptState::Conforming
            | AppLifecycleAttemptState::Failed => false,
        };
        expected_package == *package
            && attempt.attempt_id == self.attempt_id
            && attempt.kind == AppLifecycleAttemptKind::InitialInstall
            && attempt.installation_id.is_none()
            && attempt.source_installation_generation.is_none()
            && attempt.candidate_package_revision_ref == self.package_revision_ref
            && attempt.conformance_attestation_ref.as_ref()
                == Some(&self.conformance_attestation_ref)
            && attempt.permission_migration_diff_ref.as_ref() == Some(&self.permission_diff_ref)
            && attempt_progress_matches
            && installation.scope == *authenticated.scope()
            && installation.installation_id == self.installation_id
            && installation.package_revision_ref == self.package_revision_ref
    }

    fn matches_existing_bundled_purge(
        &self,
        authenticated: &AuthenticatedAppScope,
        package: &AppPackageRevision,
        installation: &AppInstallation,
    ) -> bool {
        let mut expected_package = self.package_revision.clone();
        expected_package.created_at = package.created_at;
        expected_package.source_kind == AppPackageSourceKind::Bundled
            && expected_package == *package
            && installation.scope == *authenticated.scope()
            && installation.installation_id == self.installation_id
            && installation.package_revision_ref == self.package_revision_ref
            && installation.lifecycle.status == AppInstallationStatus::Purged
            && installation.purged_at.is_some()
    }
}

fn revision_attempt_id(
    destination_package_revision_ref: &AppReference,
    source_fence: &AppReviewableRevisionSourceFence,
) -> Result<AppReference, AppCandidatePublicationError> {
    let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-update-candidate.v1",
        "destination_package_revision_ref": destination_package_revision_ref,
        "source_fence_digest": source_fence.fence_digest,
        "attempt_kind": source_fence.attempt_kind,
        "installation_id": source_fence.installation_id,
    }))
    .map_err(|error| AppCandidatePublicationError::Identity(error.to_string()))?;
    AppReference::parse(format!(
        "attempt:app-{}:{}",
        match source_fence.attempt_kind {
            AppLifecycleAttemptKind::Update => "update",
            AppLifecycleAttemptKind::Reinstall => "reinstall",
            AppLifecycleAttemptKind::InitialInstall => "invalid",
        },
        digest.as_str().trim_start_matches("blake3:")
    ))
    .map_err(AppCandidatePublicationError::from)
}

fn prepare_candidate(
    portable: AppPackageArchiveManifest,
    staged: StagedAppPackage,
    producer: AppCandidateProducer,
    local_publisher_identity: AppReference,
    registry_evidence: Vec<AppVerifiedRegistryDependency>,
) -> Result<PreparedCandidate, AppCandidatePublicationError> {
    verify_provider_free_candidate(staged.candidate())
        .map_err(AppCandidatePublicationError::Conformance)?;
    let manifest = staged.candidate().manifest().manifest();
    let expected_package_id = AppReference::parse(format!("app:{}", manifest.name))?;
    if portable.package_id != expected_package_id
        || portable.semantic_version != manifest.version
        || portable.manifest_digest != *staged.candidate().manifest().manifest_digest()
        || portable.package_content_digest != *staged.storage_digest()
    {
        return Err(AppCandidatePublicationError::Identity(
            "portable identity does not match the locally admitted manifest".to_owned(),
        ));
    }

    let dependency_lock = local_dependency_lock(
        staged.candidate(),
        registry_evidence,
        &AppReviewedToolCatalog::external_immutable_evidence_only(),
    )?;
    if portable.dependency_lock_digest != *dependency_lock.lock_digest() {
        return Err(AppCandidatePublicationError::Identity(
            "portable dependency lock is not reproducible from trusted local dependencies"
                .to_owned(),
        ));
    }

    let mut requested_authority = serde_json::json!({
        "distribution": manifest.app.distribution,
        "widgets": manifest.app.widgets,
        "indicators": manifest.app.indicators,
        "navigation": manifest.app.navigation,
        "resources": manifest.app.resources,
        "dependencies": manifest.app.dependencies,
        "workflows": manifest.app.workflows,
        "actions": manifest.app.actions,
    });
    if !manifest.app.behaviors.is_empty() {
        requested_authority["behaviors"] = serde_json::to_value(&manifest.app.behaviors)
            .map_err(|error| AppCandidatePublicationError::Conformance(error.to_string()))?;
    }
    if !manifest.app.event_behaviors.is_empty() {
        requested_authority["event_behaviors"] =
            serde_json::to_value(&manifest.app.event_behaviors)
                .map_err(|error| AppCandidatePublicationError::Conformance(error.to_string()))?;
    }
    let requested_authority_digest = AppDigest::blake3_canonical_json(&requested_authority)
        .map_err(|error| AppCandidatePublicationError::Conformance(error.to_string()))?;
    let requested_data_policy_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "data_policy": manifest.app.data_policy,
        "entities": manifest.app.entities,
        "workflow_inputs": manifest.app.workflows,
    }))
    .map_err(|error| AppCandidatePublicationError::Conformance(error.to_string()))?;
    let entity_schema_digest = canonical_entity_schema_digest(manifest)
        .map_err(|error| AppCandidatePublicationError::Conformance(error.to_string()))?;
    let view_schema_digest = canonical_view_schema_digest(manifest)
        .map_err(|error| AppCandidatePublicationError::Conformance(error.to_string()))?;
    let workflow_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "workflows": manifest.app.workflows,
        "actions": manifest.app.actions,
    }))
    .map_err(|error| AppCandidatePublicationError::Conformance(error.to_string()))?;
    let compatibility = manifest
        .app
        .compatibility
        .iter()
        .map(|(contract, requirement)| AppCompatibilityRequirement {
            contract: contract.clone(),
            requirement: requirement.clone(),
        })
        .collect();
    let (source_kind, verification_attestation_ref) = match producer {
        AppCandidateProducer::ExternalSdk => (AppPackageSourceKind::LocalAuthoring, None),
        AppCandidateProducer::VerifiedVibeDev {
            verification_attestation_ref,
        } => (
            AppPackageSourceKind::LocalVibedev,
            Some(verification_attestation_ref),
        ),
        // `Bundled` predates this producer and describes exactly it: a package
        // that shipped with the deployment rather than arriving through
        // authoring or a marketplace.
        AppCandidateProducer::DeploymentSystemSeed => (AppPackageSourceKind::Bundled, None),
    };
    let conformance_attestation_ref = derived_reference(
        "attestation:app-conformance",
        &[
            staged.storage_digest(),
            dependency_lock.lock_digest(),
            &requested_authority_digest,
            &requested_data_policy_digest,
        ],
    )?;
    let package_revision = AppPackageRevision {
        package_id: expected_package_id,
        semantic_version: manifest.version.clone(),
        content_digest: staged.storage_digest().clone(),
        manifest_schema_version: manifest.metadata.magician.app_manifest_version.clone(),
        authoring_sdk_version: manifest.metadata.magician.app_sdk_version.clone(),
        // Foreign archive publisher identity is advisory. The authenticated
        // local actor owns the review candidate that is actually published.
        publisher_identity: local_publisher_identity,
        source_kind,
        compatibility,
        requested_authority_digest,
        requested_data_policy_digest,
        dependency_lock_digest: dependency_lock.lock_digest().clone(),
        entity_schema_digest,
        view_schema_digest,
        workflow_digest,
        verification_attestation_ref,
        conformance_attestation_ref: conformance_attestation_ref.clone(),
        created_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let package_revision_ref = canonical_package_revision_ref(&package_revision)?;
    if portable.package_revision_ref != package_revision_ref {
        return Err(AppCandidatePublicationError::Identity(
            "portable package revision does not match local conformance identity".to_owned(),
        ));
    }
    let attempt_id = derived_identity("attempt:app-candidate", &package_revision_ref)?;
    let installation_id = AppInstallationId::parse(
        derived_identity("install", &package_revision_ref)?
            .as_str()
            .replace(':', "_"),
    )?;
    let permission_diff_ref = derived_identity("diff:app-permissions", &package_revision_ref)?;
    Ok(PreparedCandidate {
        staged,
        dependency_lock,
        package_revision_ref,
        package_revision,
        attempt_id,
        installation_id,
        conformance_attestation_ref,
        permission_diff_ref,
    })
}

fn local_dependency_lock(
    candidate: &super::manifest::AppPackageCandidate,
    registry_evidence: Vec<AppVerifiedRegistryDependency>,
    tool_catalog: &AppReviewedToolCatalog,
) -> Result<AppPackageLock, AppCandidatePublicationError> {
    let mut registry_evidence =
        complete_declared_tool_evidence(candidate, registry_evidence, tool_catalog)?;
    let contract = AppVerifiedRegistryDependency::from_trusted_registry_bytes(
        AppDependencyKind::Contract,
        AppReference::parse("contract:magician_contract")?,
        super::models::APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION.to_owned(),
        AppReference::parse("contract-revision:magician-contract-v2")?,
        AppRevision::new(BUILTIN_CONTRACT_REVISION)?,
        current_builtin_contract_bytes()?,
    )?;
    registry_evidence.push(contract);
    lock_app_package_dependencies(candidate, registry_evidence, &AppPackageLimits::default())
        .map_err(AppCandidatePublicationError::from)
}

fn current_builtin_contract_bytes() -> Result<&'static [u8], AppCandidatePublicationError> {
    super::component_contract::validate_app_data_plane_component_contract(BUILTIN_CONTRACT_BYTES)
        .map_err(|error| {
        AppCandidatePublicationError::EmbeddedContract(format!(
            "embedded app data-plane component contract is invalid: {error}"
        ))
    })?;
    Ok(BUILTIN_CONTRACT_BYTES)
}

fn derived_reference(
    prefix: &str,
    digests: &[&AppDigest],
) -> Result<AppReference, AppCandidatePublicationError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(prefix.as_bytes());
    for digest in digests {
        hasher.update(&(digest.as_str().len() as u64).to_le_bytes());
        hasher.update(digest.as_str().as_bytes());
    }
    AppReference::parse(format!("{prefix}:{}", hasher.finalize().to_hex()))
        .map_err(AppCandidatePublicationError::from)
}

fn derived_identity(
    prefix: &str,
    package_revision_ref: &AppReference,
) -> Result<AppReference, AppCandidatePublicationError> {
    let digest = AppDigest::blake3(
        format!("magician/app-candidate/v1\0{prefix}\0{package_revision_ref}").as_bytes(),
    );
    AppReference::parse(format!(
        "{prefix}:{}",
        digest
            .as_str()
            .strip_prefix("blake3:")
            .expect("blake3 app digest has its canonical prefix")
    ))
    .map_err(AppCandidatePublicationError::from)
}

fn stage_outcome_label(outcome: AppPackageStageOutcome) -> &'static str {
    match outcome {
        AppPackageStageOutcome::Created => "created",
        AppPackageStageOutcome::AlreadyPresent => "already_present",
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::fs;

    use super::*;
    use crate::magician_v2::{
        apps::{
            authoring::{
                run_app_authoring_command, AppAuthoringCommand, AppCatalogDiscoverArgs,
                AppCheckArgs, AppInitArgs, AppPackArgs,
            },
            models::{AppContractLimits, ValidateAppContract},
            package_transfer::{admit_package_archive, tests::archive_bytes_fixture},
            procedure_publication::AppProcedurePublicationService,
            registry::tests::{authenticated_scope, canonical_tempdir, time},
            vibedev_artifact_handoff::{
                vibedev_artifact_handoff_path, VibeDevArtifactHandoffService,
                VibeDevArtifactPublicationReceipt,
            },
        },
        execution::verification::{
            attestation::{AcceptedResult, AttestationKey, VerificationAttempt},
            gate::{GateBudgets, GateOrigin},
            ids::{AttemptId, CandidateRevision, Generation},
        },
    };

    fn seed_admission_scope(now: DateTime<Utc>) -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_system_worker(
            super::super::records::AppScope {
                principal: AppReference::parse("principal-a").expect("principal"),
                workspace: AppReference::parse("workspace-a").expect("workspace"),
            },
            super::super::models::AppScopeBindingRef::parse("scope-binding-1").expect("binding"),
            AppReference::parse("worker:seed-test").expect("worker"),
            AppReference::parse("run:seed-test").expect("run"),
            now,
            now + chrono::Duration::minutes(5),
        )
        .expect("system worker scope")
    }

    fn first_seed_admission(
        workspace: &ArtifactV2Workspace,
    ) -> super::super::system_boot_admission::TrustedSystemPackageAdmission {
        super::super::system_boot_admission::resolve_system_package_inventory(workspace)
            .expect("the repo seed root resolves")
            .into_iter()
            .next()
            .expect("the repo seed root ships at least one system package")
    }

    /// Boot re-publishes every seed from the same bytes. When the embedded tool
    /// catalog resolves differently than it did on the boot that first
    /// published the package, the version no longer identifies one immutable
    /// revision. Refuse at reconciliation with an actionable version-bump
    /// error rather than carrying a stale lock into the reproduction gate.
    #[tokio::test]
    async fn trusted_seed_lock_drift_is_refused_at_the_reconciliation_boundary() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let service = AppCandidatePublicationService::new(workspace.clone());
        let now = Utc::now();
        let authenticated = seed_admission_scope(now);

        let first = service
            .publish_trusted_system_package(&authenticated, first_seed_admission(&workspace), now)
            .await
            .expect("first boot admits the seed");

        // Keep the native recipe's required primitive bindings and change one
        // immutable dependency revision, as a previous binary could resolve it.
        let (_dir, candidate, _provenance) = first_seed_admission(&workspace).into_parts();
        let catalog = embedded_reviewed_tool_catalog().unwrap();
        let mut evidence =
            complete_declared_tool_evidence(&candidate, Vec::new(), &catalog).unwrap();
        evidence.push(
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Contract,
                AppReference::parse("contract:magician_contract").unwrap(),
                super::super::models::APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION.to_owned(),
                AppReference::parse("contract-revision:previous-fixture").unwrap(),
                AppRevision::new(1).unwrap(),
                current_builtin_contract_bytes().unwrap(),
            )
            .unwrap(),
        );
        let drifted =
            lock_app_package_dependencies(&candidate, evidence, &AppPackageLimits::default())
                .expect("a previous contract revision remains a structurally valid lock");
        assert_ne!(
            drifted.lock_digest().as_str(),
            first.dependency_lock_digest.as_str(),
            "the test needs a lock that actually drifted"
        );

        let error = service
            .reconcile_trusted_seed_lock(&authenticated, &candidate, drifted, now)
            .await
            .expect_err("one immutable version cannot carry two dependency locks");
        let message = error.to_string();
        assert!(
            message.contains("different dependency lock without a version bump"),
            "unexpected refusal: {message}"
        );
        assert!(message.contains(first.dependency_lock_digest.as_str()));

        // The unchanged embedded catalog remains exactly idempotent.
        let again = service
            .publish_trusted_system_package(&authenticated, first_seed_admission(&workspace), now)
            .await
            .expect("second boot re-admits the seed");
        assert_eq!(again.package_revision_ref, first.package_revision_ref);
        assert!(matches!(
            again.publication_outcome,
            AppCandidatePublicationOutcome::AlreadyPresent
        ));
    }

    #[tokio::test]
    async fn trusted_seed_repairs_a_parked_update_without_minting_another_installation() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let service = AppCandidatePublicationService::new(workspace.clone());
        let now = Utc::now();
        let authenticated = seed_admission_scope(now);
        let first = service
            .publish_trusted_system_package(&authenticated, first_seed_admission(&workspace), now)
            .await
            .unwrap();
        let installation_id = first.installation_id.clone();
        let attempt_id = first.attempt_id.clone();
        // Reproduce the retained metadata after an approved installation was
        // parked, but its update candidate was never published. No live store
        // is opened: these are synthetic records in the temporary registry.
        service.registry.execute_scoped_test_write(&authenticated, &now, move |connection, _| {
            let bytes: Vec<u8> = connection.query_row(
                "SELECT record_json FROM app_installations WHERE installation_id = ?1",
                rusqlite::params![installation_id.as_str()], |row| row.get(0),
            )?;
            let mut installation: AppInstallation = serde_json::from_slice(&bytes)?;
            installation.lifecycle.status = AppInstallationStatus::UpdatePending;
            installation.lifecycle.generation = 3;
            installation.lifecycle.update_return_status = Some(super::super::lifecycle::AppStableOperationalStatus::Enabled);
            installation.grant_revision = Some(AppRevision::new(1)?);
            installation.active_schema_revision = Some(AppRevision::new(1)?);
            installation.active_surface_revision = Some(AppRevision::new(1)?);
            installation.validate_app_contract(&AppContractLimits::default())?;
            connection.execute(
                "UPDATE app_installations SET lifecycle_status = 'update_pending', lifecycle_generation = 3, record_json = ?1 WHERE installation_id = ?2",
                rusqlite::params![serde_json::to_vec(&installation)?, installation_id.as_str()],
            )?;
            let bytes: Vec<u8> = connection.query_row(
                "SELECT record_json FROM app_lifecycle_attempts WHERE attempt_id = ?1",
                rusqlite::params![attempt_id.as_str()], |row| row.get(0),
            )?;
            let mut attempt: super::super::records::AppLifecycleAttempt = serde_json::from_slice(&bytes)?;
            attempt.state = AppLifecycleAttemptState::Committed;
            connection.execute(
                "UPDATE app_lifecycle_attempts SET state = 'committed', record_json = ?1 WHERE attempt_id = ?2",
                rusqlite::params![serde_json::to_vec(&attempt)?, attempt_id.as_str()],
            )?;
            Ok(())
        }).await.unwrap();

        // The new attempt is later than publication of its immutable package.
        // Only the attempt timestamp advances; package replay keeps its bytes.
        let now = now + chrono::Duration::seconds(1);
        let repaired = service
            .publish_trusted_system_package(&authenticated, first_seed_admission(&workspace), now)
            .await
            .expect("boot fills the missing review candidate");
        assert_eq!(repaired.installation_id, first.installation_id);
        assert_eq!(repaired.state, AppInstallationStatus::UpdatePending);
        assert!(!repaired.activation_authority_granted);
        let attempt = service
            .registry
            .ready_for_review_attempt_for_installation(&authenticated, &first.installation_id, now)
            .await
            .unwrap()
            .expect("review can find the update");
        assert_eq!(attempt.kind, AppLifecycleAttemptKind::Update);
        assert_eq!(attempt.source_installation_generation, Some(2));
        assert_eq!(attempt.attempt_id, repaired.attempt_id);
        assert_eq!(
            service
                .registry
                .installations_bounded(&authenticated, 10, now)
                .await
                .unwrap()
                .len(),
            1
        );

        service
            .publish_trusted_system_package(&authenticated, first_seed_admission(&workspace), now)
            .await
            .expect("repeated boot preserves the owner's review candidate");
        let retained = service
            .registry
            .ready_for_review_attempt_for_installation(&authenticated, &first.installation_id, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.attempt_id, attempt.attempt_id);
    }

    /// Whole-installation purge retains the immutable bundled package and its
    /// installation tombstone while deleting the linked lifecycle attempt.
    /// The next boot must recognize that exact terminal shape without
    /// resurrecting the package or reporting registry corruption.
    #[tokio::test]
    async fn trusted_seed_replay_respects_a_purged_installation_tombstone() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let service = AppCandidatePublicationService::new(workspace.clone());
        let now = Utc::now();
        let authenticated = seed_admission_scope(now);
        let first = service
            .publish_trusted_system_package(&authenticated, first_seed_admission(&workspace), now)
            .await
            .expect("first boot admits the seed");

        let installation_id = first.installation_id.clone();
        let attempt_id = first.attempt_id.clone();
        let purged_at = now + chrono::Duration::seconds(1);
        service
            .registry
            .execute_scoped_test_write(&authenticated, &purged_at, move |connection, _scope| {
                let bytes: Vec<u8> = connection.query_row(
                    "SELECT record_json FROM app_installations WHERE installation_id = ?1",
                    rusqlite::params![installation_id.as_str()],
                    |row| row.get(0),
                )?;
                let mut installation: AppInstallation = serde_json::from_slice(&bytes)?;
                installation.lifecycle.status = AppInstallationStatus::Purged;
                installation.lifecycle.generation = installation
                    .lifecycle
                    .generation
                    .checked_add(1)
                    .expect("test generation does not overflow");
                installation.lifecycle.update_return_status = None;
                installation.grant_revision = None;
                installation.active_schema_revision = None;
                installation.active_surface_revision = None;
                installation.purged_at = Some(purged_at);
                installation.updated_at = purged_at;
                installation.validate_app_contract(&AppContractLimits::default())?;
                connection.execute(
                    "DELETE FROM app_installation_approvals WHERE attempt_id = ?1",
                    rusqlite::params![attempt_id.as_str()],
                )?;
                connection.execute(
                    "DELETE FROM app_lifecycle_attempts WHERE attempt_id = ?1",
                    rusqlite::params![attempt_id.as_str()],
                )?;
                connection.execute(
                    "UPDATE app_installations
                            SET lifecycle_status = 'purged', lifecycle_generation = ?1,
                                record_json = ?2, updated_at = ?3
                          WHERE installation_id = ?4",
                    rusqlite::params![
                        i64::try_from(installation.lifecycle.generation)
                            .expect("test generation fits SQLite"),
                        serde_json::to_vec(&installation)?,
                        purged_at.to_rfc3339(),
                        installation.installation_id.as_str(),
                    ],
                )?;
                Ok(())
            })
            .await
            .expect("purged storage shape is installed");

        let replay = service
            .publish_trusted_system_package(
                &authenticated,
                first_seed_admission(&workspace),
                purged_at,
            )
            .await
            .expect("next boot recognizes the purge tombstone");
        assert_eq!(
            replay.publication_outcome,
            AppCandidatePublicationOutcome::AlreadyPresent
        );
        assert_eq!(replay.state, AppInstallationStatus::Purged);
        assert!(service
            .registry
            .lifecycle_attempt(&authenticated, &first.attempt_id, purged_at)
            .await
            .expect("attempt lookup succeeds")
            .is_none());
    }

    fn seed_admission_scope_for(principal: &str, now: DateTime<Utc>) -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_system_worker(
            super::super::records::AppScope {
                principal: AppReference::parse(principal).expect("principal"),
                workspace: AppReference::parse("workspace-a").expect("workspace"),
            },
            super::super::models::AppScopeBindingRef::parse("scope-binding-1").expect("binding"),
            AppReference::parse("worker:seed-test").expect("worker"),
            AppReference::parse("run:seed-test").expect("run"),
            now,
            now + chrono::Duration::minutes(5),
        )
        .expect("system worker scope")
    }

    /// A system package's lock is a function of the binary and the seed
    /// bytes: two scopes on the same build publish the same revision reference.
    #[tokio::test]
    async fn trusted_seed_lock_is_identical_across_scopes() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let service = AppCandidatePublicationService::new(workspace.clone());
        let now = Utc::now();

        let in_a = service
            .publish_trusted_system_package(
                &seed_admission_scope_for("principal-a", now),
                first_seed_admission(&workspace),
                now,
            )
            .await
            .expect("scope a admits the seed");
        let in_b = service
            .publish_trusted_system_package(
                &seed_admission_scope_for("principal-b", now),
                first_seed_admission(&workspace),
                now,
            )
            .await
            .expect("scope b admits the seed");

        assert_eq!(in_a.dependency_lock_digest, in_b.dependency_lock_digest);
        assert_eq!(in_a.package_revision_ref, in_b.package_revision_ref);
    }

    fn sdk_archive_bytes(root: &std::path::Path) -> Vec<u8> {
        let project = root.join("external-reading-list");
        run_app_authoring_command(&AppAuthoringCommand::Init(AppInitArgs {
            name: "external-reading-list".to_owned(),
            path: Some(project.clone()),
        }))
        .expect("external SDK scaffold succeeds");
        let archive = root.join("external-reading-list.app.zip");
        run_app_authoring_command(&AppAuthoringCommand::Pack(AppPackArgs {
            path: project,
            publisher: "publisher:external-sdk".to_owned(),
            resolutions: None,
            output: Some(archive.clone()),
            discover: AppCatalogDiscoverArgs::default(),
        }))
        .expect("external SDK package succeeds");
        fs::read(archive).expect("SDK archive is readable")
    }

    #[test]
    fn ordinary_candidate_publication_refuses_system_distribution_claims() {
        assert!(
            validate_untrusted_candidate_distribution(AppManifestDistribution::Installable).is_ok()
        );
        assert!(matches!(
            validate_untrusted_candidate_distribution(AppManifestDistribution::System),
            Err(AppCandidatePublicationError::Conformance(message))
                if message.contains("host-controlled digest-pinned boot admission")
        ));
    }

    fn write_green_vibedev_evidence(
        workspace: &ArtifactV2Workspace,
        scope: TransactionScope,
        project_binding: &str,
        snapshot_digest: String,
    ) -> (GateId, String, String) {
        let root_task_id = "task-vibedev-app".to_owned();
        let root_execution_id = "execution-vibedev-app".to_owned();
        let mut gate = VerificationGate::new(
            scope.clone(),
            project_binding,
            root_task_id.clone(),
            root_execution_id.clone(),
            CandidateRevision::new("candidate-vibedev-app", 1)
                .expect("candidate identity is valid"),
            GateOrigin {
                engineer_agent_id: "engineer".to_owned(),
                coding_profile: None,
                coding_engine: Some("codex_app_server".to_owned()),
                constraint_auto: false,
                coding_invocation_ref: Some("server-run".to_owned()),
                child_execution_id: Some("child-execution".to_owned()),
            },
            GateBudgets::default(),
        )
        .expect("gate is valid");
        let attempt_id = AttemptId::new();
        let generation = Generation(1);
        let attestation = VerificationAttestation {
            schema_version:
                crate::magician_v2::execution::verification::attestation::ATTESTATION_SCHEMA_VERSION,
            attestation_id: crate::magician_v2::execution::verification::ids::AttestationId::new(),
            key: AttestationKey {
                scope,
                project_binding: project_binding.to_owned(),
                candidate_ref: gate.current_candidate.candidate_ref.clone(),
                proposal_ref: None,
                snapshot_digest,
                policy_digest: "policy-vibedev-app".to_owned(),
                runner_env_digest: "runner-vibedev-app".to_owned(),
            },
            created_at: time(0),
            attempts: vec![VerificationAttempt {
                attempt_id: attempt_id.clone(),
                generation,
                fenced_lease: "lease-vibedev-app".to_owned(),
                started_at: time(0),
                settled_at: Some(time(1)),
                command_results: Vec::new(),
                outcome: AttemptOutcome::Green,
            }],
            accepted_result: Some(AcceptedResult {
                attempt_id,
                outcome: AttemptOutcome::Green,
                accepted_at: time(1),
                generation,
            }),
        };
        gate.status = GateStatus::Verified;
        gate.generation = generation;
        gate.active_attestation_ref = Some(attestation.attestation_id.clone());
        gate.updated_at = time(1);

        let scope_root = workspace.scope_root(&gate.scope.principal, &gate.scope.workspace);
        let gates = scope_root.join("verification/gates");
        let attestations = scope_root.join("verification/attestations");
        fs::create_dir_all(&gates).expect("gate directory is created");
        fs::create_dir_all(&attestations).expect("attestation directory is created");
        fs::write(
            gates.join(format!("{}.json", gate.gate_id.as_str())),
            serde_json::to_vec_pretty(&gate).expect("gate encodes"),
        )
        .expect("gate is written");
        fs::write(
            attestations.join(format!("{}.json", attestation.attestation_id.as_str())),
            serde_json::to_vec_pretty(&attestation).expect("attestation encodes"),
        )
        .expect("attestation is written");
        (gate.gate_id, root_task_id, root_execution_id)
    }

    #[tokio::test]
    async fn external_sdk_candidate_converges_on_review_without_activation_authority() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let service = AppCandidatePublicationService::new(workspace);
        let authenticated = authenticated_scope("owner", "workspace");
        let archive = sdk_archive_bytes(temporary.path());

        let created = service
            .publish_archive_candidate(
                &authenticated,
                admit_package_archive(&archive).expect("archive admission succeeds"),
                time(1),
            )
            .await
            .expect("candidate publication succeeds");
        assert_eq!(created.state, AppInstallationStatus::ReadyForReview);
        assert_eq!(
            created.publication_outcome,
            AppCandidatePublicationOutcome::Created
        );
        assert!(!created.activation_authority_granted);
        assert_eq!(
            created.source_publisher_identity.as_str(),
            "publisher:external-sdk"
        );
        assert_eq!(created.local_publisher_identity.as_str(), "actor:owner");

        let package = service
            .registry
            .package_revision(&authenticated, &created.package_revision_ref, time(2))
            .await
            .expect("package lookup succeeds")
            .expect("package was published");
        assert_eq!(package.source_kind, AppPackageSourceKind::LocalAuthoring);
        assert_eq!(package.publisher_identity.as_str(), "actor:owner");
        let installation = service
            .registry
            .installation(&authenticated, &created.installation_id, time(2))
            .await
            .expect("installation lookup succeeds")
            .expect("review installation was published");
        assert_eq!(
            installation.lifecycle.status,
            AppInstallationStatus::ReadyForReview
        );
        assert!(installation.grant_revision.is_none());
        assert!(installation.active_schema_revision.is_none());
        assert!(installation.active_surface_revision.is_none());

        let replay = service
            .publish_archive_candidate(
                &authenticated,
                admit_package_archive(&archive).expect("replay archive admission succeeds"),
                time(3),
            )
            .await
            .expect("exact publication replay succeeds");
        assert_eq!(
            replay.publication_outcome,
            AppCandidatePublicationOutcome::AlreadyPresent
        );
        assert_eq!(replay.stage_outcome, "already_present");
        assert_eq!(replay.package_revision_ref, created.package_revision_ref);
        assert_eq!(replay.attempt_id, created.attempt_id);
        assert_eq!(replay.installation_id, created.installation_id);
        assert!(!replay.activation_authority_granted);
    }

    #[tokio::test]
    async fn external_candidate_rebuilds_registry_procedure_lock_from_exact_published_bytes() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let registry = AppRegistryService::new(workspace.clone());
        let service = AppCandidatePublicationService::from_parts(
            workspace.clone(),
            registry.clone(),
            AppPackageStager::new(workspace),
        );
        let authenticated = authenticated_scope("owner", "workspace");
        let procedure_bytes = b"---\nname: external-summary\nversion: 1.0.0\ndescription: Summarize reviewed input.\nmetadata:\n  magician:\n    skill_type: procedure\n---\nProduce one concise summary.\n";
        let procedure = AppProcedurePublicationService::new(registry)
            .publish(&authenticated, procedure_bytes, time(1))
            .await
            .expect("procedure revision publishes");

        let project = temporary.path().join("external-procedure-app");
        run_app_authoring_command(&AppAuthoringCommand::Init(AppInitArgs {
            name: "external-procedure-app".to_owned(),
            path: Some(project.clone()),
        }))
        .unwrap();
        let manifest_path = project.join("SKILL.md");
        let manifest = fs::read_to_string(&manifest_path).unwrap().replace(
            "    procedure_skills: []",
            "    procedure_skills:\n      - skill: skill:external-summary\n        \
             version_requirement: \"^1\"",
        );
        fs::write(&manifest_path, manifest).unwrap();
        run_app_authoring_command(&AppAuthoringCommand::Check(AppCheckArgs {
            path: project.clone(),
            write_generated: true,
        }))
        .unwrap();

        let resolution_root = temporary.path().join("procedure-resolution");
        fs::create_dir_all(resolution_root.join("external-summary")).unwrap();
        fs::write(
            resolution_root.join("external-summary/SKILL.md"),
            procedure_bytes,
        )
        .unwrap();
        let resolution_path = resolution_root.join("resolutions.json");
        fs::write(
            &resolution_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1,
                "dependencies": [{
                    "kind": "procedure_skill",
                    "dependency_ref": procedure.dependency_ref,
                    "semantic_version": procedure.semantic_version,
                    "immutable_revision_ref": procedure.immutable_revision_ref,
                    "revision": procedure.revision,
                    "content_path": "external-summary/SKILL.md"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let archive_path = temporary.path().join("external-procedure-app.app.zip");
        run_app_authoring_command(&AppAuthoringCommand::Pack(AppPackArgs {
            path: project,
            publisher: "publisher:external-sdk".to_owned(),
            resolutions: Some(resolution_path),
            output: Some(archive_path.clone()),
            discover: AppCatalogDiscoverArgs::default(),
        }))
        .unwrap();
        let archive = fs::read(archive_path).unwrap();

        let published = service
            .publish_archive_candidate(
                &authenticated,
                admit_package_archive(&archive).expect("portable lock is admitted"),
                time(2),
            )
            .await
            .expect("trusted registry bytes reproduce the authoring lock");
        assert_eq!(published.state, AppInstallationStatus::ReadyForReview);
        assert!(!published.activation_authority_granted);
    }

    #[tokio::test]
    async fn verified_vibedev_snapshot_converges_on_the_same_inert_review_boundary() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let authenticated = authenticated_scope("owner", "workspace");
        let scope = TransactionScope {
            principal: "owner".to_owned(),
            workspace: "workspace".to_owned(),
        };
        let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
        let project = scope_root.join("vibedev-project");
        let package = project.join("app");
        fs::create_dir_all(&project).expect("VibeDev project directory is created");
        run_app_authoring_command(&AppAuthoringCommand::Init(AppInitArgs {
            name: "vibedev-reading-list".to_owned(),
            path: Some(package.clone()),
        }))
        .expect("VibeDev app fixture is created");
        let snapshot = SourceSnapshot::capture(&project, &[]).expect("source snapshot is captured");
        let (gate_id, task_id, execution_id) =
            write_green_vibedev_evidence(&workspace, scope, "vibedev-project", snapshot.digest());
        let service = AppCandidatePublicationService::new(workspace);

        let receipt = service
            .publish_verified_vibedev_candidate(
                &authenticated,
                gate_id,
                task_id,
                execution_id,
                PathBuf::from("app"),
                time(2),
            )
            .await
            .expect("green VibeDev candidate is published for review");
        assert_eq!(receipt.state, AppInstallationStatus::ReadyForReview);
        assert!(!receipt.activation_authority_granted);
        let revision = service
            .registry
            .package_revision(&authenticated, &receipt.package_revision_ref, time(3))
            .await
            .expect("package lookup succeeds")
            .expect("package revision exists");
        assert_eq!(revision.source_kind, AppPackageSourceKind::LocalVibedev);
        assert!(revision.verification_attestation_ref.is_some());
    }

    #[tokio::test]
    async fn verified_vibedev_handoff_runs_server_selector_and_publishes_app_review() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let authenticated = authenticated_scope("owner", "workspace");
        let scope = TransactionScope {
            principal: "owner".to_owned(),
            workspace: "workspace".to_owned(),
        };
        let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
        let project = scope_root.join("vibedev-project");
        let package = project.join("app");
        fs::create_dir_all(project.join(".magician/app-artifact-handoffs"))
            .expect("handoff directory is created");
        run_app_authoring_command(&AppAuthoringCommand::Init(AppInitArgs {
            name: "vibedev-selected-app".to_owned(),
            path: Some(package),
        }))
        .expect("VibeDev app fixture is created");
        fs::write(
            project.join(
                vibedev_artifact_handoff_path("task-vibedev-app").expect("valid handoff path"),
            ),
            br#"{"schema_version":1,"requirements":["durable_typed_records"],"artifact_path":"app"}"#,
        )
        .expect("handoff is written");
        let snapshot = SourceSnapshot::capture(&project, &[]).expect("source snapshot is captured");
        let (gate_id, _task_id, _execution_id) =
            write_green_vibedev_evidence(&workspace, scope, "vibedev-project", snapshot.digest());
        let registry = AppRegistryService::new(workspace.clone());
        let candidates = AppCandidatePublicationService::from_parts(
            workspace.clone(),
            registry.clone(),
            AppPackageStager::new(workspace.clone()),
        );
        let handoff = VibeDevArtifactHandoffService::new(
            workspace,
            candidates,
            AppProcedurePublicationService::new(registry.clone()),
            crate::magician_v2::apps::capability_publication::AppCapabilityPublicationService::new(
                registry,
            ),
        );

        let receipt = handoff
            .publish_if_present(&authenticated, gate_id, time(2))
            .await
            .expect("verified handoff publishes")
            .expect("handoff is present");
        let VibeDevArtifactPublicationReceipt::App { publication, .. } = receipt else {
            panic!("durable records must select an app");
        };
        assert_eq!(publication.state, AppInstallationStatus::ReadyForReview);
        assert!(!publication.activation_authority_granted);
    }

    #[tokio::test]
    async fn verified_vibedev_handoff_selects_standalone_procedure_without_global_activation() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let authenticated = authenticated_scope("owner", "workspace");
        let scope = TransactionScope {
            principal: "owner".to_owned(),
            workspace: "workspace".to_owned(),
        };
        let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
        let project = scope_root.join("vibedev-project");
        fs::create_dir_all(project.join(".magician/app-artifact-handoffs"))
            .expect("handoff directory is created");
        fs::write(
            project.join("SKILL.md"),
            b"---\nname: selected-summary\nversion: 1.0.0\ndescription: Summarize reviewed input.\nmetadata:\n  magician:\n    skill_type: procedure\n---\nProduce one concise summary.\n",
        )
        .expect("procedure is written");
        fs::write(
            project.join(
                vibedev_artifact_handoff_path("task-vibedev-app").expect("valid handoff path"),
            ),
            br#"{"schema_version":1,"requirements":["reusable_instructions"],"artifact_path":"SKILL.md"}"#,
        )
        .expect("handoff is written");
        let snapshot = SourceSnapshot::capture(&project, &[]).expect("source snapshot is captured");
        let (gate_id, _task_id, _execution_id) =
            write_green_vibedev_evidence(&workspace, scope, "vibedev-project", snapshot.digest());
        let registry = AppRegistryService::new(workspace.clone());
        let candidates = AppCandidatePublicationService::from_parts(
            workspace.clone(),
            registry.clone(),
            AppPackageStager::new(workspace.clone()),
        );
        let handoff = VibeDevArtifactHandoffService::new(
            workspace,
            candidates,
            AppProcedurePublicationService::new(registry.clone()),
            crate::magician_v2::apps::capability_publication::AppCapabilityPublicationService::new(
                registry,
            ),
        );

        let receipt = handoff
            .publish_if_present(&authenticated, gate_id, time(2))
            .await
            .expect("verified procedure handoff publishes")
            .expect("handoff is present");
        let VibeDevArtifactPublicationReceipt::ProcedureSkill { publication, .. } = receipt else {
            panic!("reusable instructions must select a procedure");
        };
        assert_eq!(
            publication.dependency_ref.as_str(),
            "skill:selected-summary"
        );
        assert!(!publication.global_skill_catalog_published);
        assert!(!publication.activation_authority_granted);
    }

    #[tokio::test]
    async fn verified_vibedev_handoff_publishes_an_inert_executable_capability() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let authenticated = authenticated_scope("owner", "workspace");
        let scope = TransactionScope {
            principal: "owner".to_owned(),
            workspace: "workspace".to_owned(),
        };
        let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
        let project = scope_root.join("vibedev-project");
        fs::create_dir_all(project.join(".magician/app-artifact-handoffs"))
            .expect("handoff directory is created");
        fs::write(
            project.join("SKILL.md"),
            b"---\nname: next-step\nversion: 1.0.0\ndescription: Rank the next learning step.\nmetadata:\n  magician:\n    skill_type: tool\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {bins: [next-step]}\n      runtime:\n        protocol: cli\n        command_prefix: []\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        rank:\n          description: Rank the next step.\n          fixed_args: [rank]\n---\nReturn one ranked next step.\n",
        )
        .expect("capability is written");
        fs::write(
            project.join(
                vibedev_artifact_handoff_path("task-vibedev-app").expect("valid handoff path"),
            ),
            br#"{"schema_version":1,"requirements":["new_executable_integration"],"artifact_path":"SKILL.md"}"#,
        )
        .expect("handoff is written");
        let snapshot = SourceSnapshot::capture(&project, &[]).expect("source snapshot is captured");
        let (gate_id, _task_id, _execution_id) =
            write_green_vibedev_evidence(&workspace, scope, "vibedev-project", snapshot.digest());
        let registry = AppRegistryService::new(workspace.clone());
        let candidates = AppCandidatePublicationService::from_parts(
            workspace.clone(),
            registry.clone(),
            AppPackageStager::new(workspace.clone()),
        );
        let capabilities =
            crate::magician_v2::apps::capability_publication::AppCapabilityPublicationService::new(
                registry.clone(),
            );
        let handoff = VibeDevArtifactHandoffService::new(
            workspace,
            candidates,
            AppProcedurePublicationService::new(registry),
            capabilities,
        );

        let receipt = handoff
            .publish_if_present(&authenticated, gate_id, time(2))
            .await
            .expect("verified capability handoff publishes")
            .expect("handoff is present");
        let VibeDevArtifactPublicationReceipt::ExecutableCapability { publication, .. } = receipt
        else {
            panic!("new executable integration must select a capability");
        };
        assert_eq!(publication.dependency_ref.as_str(), "capability:next-step");
        assert!(!publication.global_skill_catalog_published);
        assert!(!publication.activation_authority_granted);
    }

    #[tokio::test]
    async fn verified_vibedev_handoff_rejects_source_drift_before_staging() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let authenticated = authenticated_scope("owner", "workspace");
        let scope = TransactionScope {
            principal: "owner".to_owned(),
            workspace: "workspace".to_owned(),
        };
        let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
        let project = scope_root.join("vibedev-project");
        let package = project.join("app");
        fs::create_dir_all(&project).expect("VibeDev project directory is created");
        run_app_authoring_command(&AppAuthoringCommand::Init(AppInitArgs {
            name: "vibedev-drift".to_owned(),
            path: Some(package.clone()),
        }))
        .expect("VibeDev app fixture is created");
        let snapshot = SourceSnapshot::capture(&project, &[]).expect("source snapshot is captured");
        let (gate_id, task_id, execution_id) =
            write_green_vibedev_evidence(&workspace, scope, "vibedev-project", snapshot.digest());
        fs::write(package.join("SKILL.md"), b"changed after verification")
            .expect("source is changed");
        let service = AppCandidatePublicationService::new(workspace);

        let error = service
            .publish_verified_vibedev_candidate(
                &authenticated,
                gate_id,
                task_id,
                execution_id,
                PathBuf::from("app"),
                time(2),
            )
            .await
            .expect_err("changed source cannot reuse green verification");
        assert!(matches!(
            error,
            AppCandidatePublicationError::Verification(_)
        ));
    }

    #[test]
    fn verified_vibedev_evidence_rejects_root_or_generation_rebinding() {
        let scope = TransactionScope {
            principal: "owner".to_owned(),
            workspace: "workspace".to_owned(),
        };
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
        let project = scope_root.join("project");
        fs::create_dir_all(&project).expect("project exists");
        fs::write(project.join("file"), b"verified").expect("source exists");
        let snapshot = SourceSnapshot::capture(&project, &[]).expect("snapshot exists");
        let (gate_id, task_id, execution_id) =
            write_green_vibedev_evidence(&workspace, scope.clone(), "project", snapshot.digest());
        let store = VerificationStore::new(scope_root, scope.clone());
        let gate = store.load_gate(&gate_id).expect("gate loads");
        let attestation = store
            .load_attestation(
                gate.active_attestation_ref
                    .as_ref()
                    .expect("active attestation exists"),
            )
            .expect("attestation loads");
        assert!(validate_verified_vibedev_evidence(
            &scope,
            &gate,
            &attestation,
            &task_id,
            &execution_id,
        )
        .is_ok());
        assert!(validate_verified_vibedev_evidence(
            &scope,
            &gate,
            &attestation,
            "other-task",
            &execution_id,
        )
        .is_err());
        let mut stale = attestation;
        stale
            .accepted_result
            .as_mut()
            .expect("accepted result exists")
            .generation = Generation(2);
        assert!(
            validate_verified_vibedev_evidence(&scope, &gate, &stale, &task_id, &execution_id,)
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_server_conformance_artifacts_never_create_review_authority() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let service = AppCandidatePublicationService::new(workspace);
        let authenticated = authenticated_scope("owner", "workspace");
        let admitted = admit_package_archive(&archive_bytes_fixture())
            .expect("structurally valid transfer fixture is admitted");
        let package_revision_ref = admitted.package().package_revision_ref.clone();

        let error = service
            .publish_archive_candidate(&authenticated, admitted, time(1))
            .await
            .expect_err("missing authoring fixtures/generated output fail conformance");
        assert!(matches!(
            error,
            AppCandidatePublicationError::Conformance(_)
        ));
        assert!(service
            .registry
            .package_revision(&authenticated, &package_revision_ref, time(2))
            .await
            .expect("registry lookup succeeds")
            .is_none());
        let attempt_id = derived_identity("attempt:app-candidate", &package_revision_ref)
            .expect("derived attempt identity is valid");
        assert!(service
            .registry
            .lifecycle_attempt(&authenticated, &attempt_id, time(2))
            .await
            .expect("attempt lookup succeeds")
            .is_none());
    }
}
