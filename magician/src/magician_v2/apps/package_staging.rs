//! Descriptor-pinned, content-addressed app-package staging.
//!
//! Source trees are admitted iteratively through retained Unix directory
//! descriptors. Package members are written into a private sibling staging
//! directory, flushed, and promoted with a create-only atomic rename. A final
//! digest directory is therefore either absent or complete; exact replay
//! verifies the immutable bytes rather than trusting its name.

#[cfg(unix)]
mod read_cache;
#[cfg(unix)]
pub use read_cache::AppPackageReadStats;

#[cfg(unix)]
use std::os::unix::{
    ffi::OsStringExt,
    fs::{MetadataExt, OpenOptionsExt},
    io::{AsRawFd, FromRawFd, RawFd},
};
#[cfg(unix)]
use std::{
    ffi::{CStr, CString, OsString},
    fs::{self, File},
    io::{Read, Write},
    path::{Component, PathBuf},
};
use std::{path::Path, sync::Arc};

#[cfg(unix)]
use chrono::Duration as ChronoDuration;
use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;
use tokio::sync::Semaphore;
#[cfg(unix)]
use uuid::Uuid;

#[cfg(unix)]
use super::manifest::{AppBundleStagingAdmission, AppPackageLimits};
use super::{
    authority::{AppAuthorityError, AuthenticatedAppScope},
    manifest::{AppManifestDistribution, AppManifestError, AppPackageCandidate},
    models::AppDigest,
    records::AppScope,
    system_boot_admission::SystemSeedProvenance,
};
use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace, json_traversal::canonical_json_bytes,
};

const DEFAULT_BLOCKING_OPERATIONS: usize = 2;
const MAX_DIRECTORY_DEPTH: usize = 64;
const MAX_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_PACKAGE_INDEX_BYTES: usize = 1_048_576;
const MAX_RECOVERY_CANDIDATES: usize = 64;
const MAX_RECOVERY_SCAN_ENTRIES: usize = 100_000;
const STALE_STAGING_SECONDS: i64 = 60 * 60;
const PACKAGE_INDEX_SCHEMA: &str = "magician.app-package-index.v1";
const APP_SCOPE_BINDING_SCHEMA: &str = "magician.app-scope-binding.v1";
const APP_SCOPE_BINDING_FILE: &str = "scope-binding.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppPackageStageOutcome {
    Created,
    AlreadyPresent,
}

/// A package admitted from one descriptor-pinned snapshot and durably staged.
///
/// This value has no `Deserialize` implementation and exposes no path as
/// authority. Consumers carry the verified candidate and its digest key into
/// the conformance/registry boundary.
#[derive(Debug)]
pub struct StagedAppPackage {
    candidate: AppPackageCandidate,
    storage_digest: AppDigest,
    outcome: AppPackageStageOutcome,
}

impl StagedAppPackage {
    pub fn candidate(&self) -> &AppPackageCandidate {
        &self.candidate
    }

    pub fn storage_digest(&self) -> &AppDigest {
        &self.storage_digest
    }

    pub fn outcome(&self) -> AppPackageStageOutcome {
        self.outcome
    }

    pub fn into_candidate(self) -> AppPackageCandidate {
        self.candidate
    }
}

#[derive(Debug, Error)]
pub enum AppPackageStagingError {
    #[error(transparent)]
    Authentication(#[from] AppAuthorityError),
    #[error(transparent)]
    Manifest(#[from] AppManifestError),
    #[error("app package staging workers are shutting down")]
    Overloaded,
    #[error("app package staging worker terminated: {0}")]
    WorkerTerminated(String),
    #[error("app package staging is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("unsafe app package source: {0}")]
    UnsafeSource(&'static str),
    #[error("unsafe app package destination: {0}")]
    UnsafeDestination(&'static str),
    #[error("app package source changed during admission")]
    SourceChanged,
    #[error("app package destination authority changed during staging")]
    DestinationChanged,
    #[error("content-addressed app package bytes conflict with their digest key")]
    ContentConflict,
    #[error("app storage directory belongs to another authenticated scope")]
    ScopeCollision,
    #[error("app package publication completed but directory durability is unknown")]
    CommitStateUnknown,
    #[error("app package recovery exceeds its bounded candidate limit")]
    RecoveryLimitExceeded,
    #[error("failed to encode app package index: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("failed to access app package filesystem: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct AppPackageStager {
    workspace: ArtifactV2Workspace,
    blocking_slots: Arc<Semaphore>,
    #[cfg(unix)]
    read_cache: Arc<read_cache::VerifiedPackageReadCache>,
}

impl std::fmt::Debug for AppPackageStager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppPackageStager")
            .field("workspace_root", &self.workspace.base_root())
            .field(
                "available_blocking_slots",
                &self.blocking_slots.available_permits(),
            )
            .finish_non_exhaustive()
    }
}

impl AppPackageStager {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self::with_blocking_capacity(workspace, DEFAULT_BLOCKING_OPERATIONS)
            .expect("the default app-package staging capacity is positive")
    }

    pub fn workspace(&self) -> &ArtifactV2Workspace {
        &self.workspace
    }

    pub fn with_blocking_capacity(
        workspace: ArtifactV2Workspace,
        blocking_capacity: usize,
    ) -> Result<Self, AppPackageStagingError> {
        if blocking_capacity == 0 {
            return Err(AppPackageStagingError::UnsafeDestination(
                "blocking capacity must be greater than zero",
            ));
        }
        Ok(Self {
            workspace,
            blocking_slots: Arc::new(Semaphore::new(blocking_capacity)),
            #[cfg(unix)]
            read_cache: Arc::new(read_cache::VerifiedPackageReadCache::default()),
        })
    }

    /// Admit and publish one package without scanning any other scope or final
    /// package directory.
    pub async fn stage_from_directory(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        source_root: impl AsRef<Path>,
        now: DateTime<Utc>,
    ) -> Result<StagedAppPackage, AppPackageStagingError> {
        authenticated_scope.ensure_live_at(&now)?;
        let permit = Arc::clone(&self.blocking_slots)
            .acquire_owned()
            .await
            .map_err(|_| AppPackageStagingError::Overloaded)?;
        let workspace = self.workspace.clone();
        let scope = authenticated_scope.scope().clone();
        let source_root = source_root.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            stage_from_directory_blocking(&workspace, &scope, &source_root)
        })
        .await
        .map_err(|error| AppPackageStagingError::WorkerTerminated(error.to_string()))?
    }

    /// Publish an already admitted in-memory candidate. The archive route uses
    /// this only after bounded ZIP admission has rebuilt the same
    /// non-deserializable [`AppPackageCandidate`] used by directory staging.
    pub async fn stage_candidate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        candidate: AppPackageCandidate,
        now: DateTime<Utc>,
    ) -> Result<StagedAppPackage, AppPackageStagingError> {
        authenticated_scope.ensure_live_at(&now)?;
        validate_ordinary_staging_distribution(candidate.manifest().manifest().app.distribution)?;
        let permit = Arc::clone(&self.blocking_slots)
            .acquire_owned()
            .await
            .map_err(|_| AppPackageStagingError::Overloaded)?;
        let workspace = self.workspace.clone();
        let scope = authenticated_scope.scope().clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            publish_candidate_blocking(&workspace, &scope, candidate)
        })
        .await
        .map_err(|error| AppPackageStagingError::WorkerTerminated(error.to_string()))?
    }

    /// Publish a candidate whose bytes were proven to come from the
    /// deployment's read-only seed root.
    ///
    /// This is the only staging path that does not refuse
    /// `distribution: system`, and reaching it requires a
    /// [`SystemSeedProvenance`] — a witness whose fields are private to
    /// `system_boot_admission`, so no other module can construct one. The
    /// witness is re-checked against these exact bytes here rather than
    /// trusted on arrival: a proof minted for one package must not admit
    /// another.
    pub(crate) async fn stage_trusted_system_candidate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        candidate: AppPackageCandidate,
        provenance: &SystemSeedProvenance,
        now: DateTime<Utc>,
    ) -> Result<StagedAppPackage, AppPackageStagingError> {
        authenticated_scope.ensure_live_at(&now)?;
        if provenance.seed_digest() != candidate.bundle_digest() {
            return Err(AppPackageStagingError::UnsafeSource(
                "seed provenance does not match the candidate bytes",
            ));
        }
        // The inverse of the ordinary gate. An installable package has no
        // business on the trusted path either: silently granting it seed
        // provenance would be the same mistake in the other direction.
        if candidate.manifest().manifest().app.distribution != AppManifestDistribution::System {
            return Err(AppPackageStagingError::UnsafeSource(
                "trusted seed staging admits only system-distribution packages",
            ));
        }
        let permit = Arc::clone(&self.blocking_slots)
            .acquire_owned()
            .await
            .map_err(|_| AppPackageStagingError::Overloaded)?;
        let workspace = self.workspace.clone();
        let scope = authenticated_scope.scope().clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            publish_admitted_candidate_blocking(&workspace, &scope, candidate)
        })
        .await
        .map_err(|error| AppPackageStagingError::WorkerTerminated(error.to_string()))?
    }

    /// Reopen one exact scoped immutable package. Unchanged descriptor-pinned
    /// metadata and index bytes permit reuse of admitted in-memory bytes;
    /// changed files require full read/admission. Grants are never cached here.
    pub async fn load_staged_package(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        storage_digest: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<StagedAppPackage, AppPackageStagingError> {
        authenticated_scope.ensure_live_at(&now)?;
        let scope = authenticated_scope.scope().clone();
        #[cfg(unix)]
        let cache = Arc::clone(&self.read_cache);
        #[cfg(unix)]
        let read_guard = cache.gate(&scope, &storage_digest).lock_owned().await;
        let permit = Arc::clone(&self.blocking_slots)
            .acquire_owned()
            .await
            .map_err(|_| AppPackageStagingError::Overloaded)?;
        let workspace = self.workspace.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            #[cfg(unix)]
            {
                let _read_guard = read_guard;
                load_staged_package_blocking(&workspace, &scope, &storage_digest, &cache)
            }
            #[cfg(not(unix))]
            load_staged_package_blocking(&workspace, &scope, &storage_digest)
        })
        .await
        .map_err(|error| AppPackageStagingError::WorkerTerminated(error.to_string()))?
    }

    #[cfg(unix)]
    pub fn read_stats(&self) -> AppPackageReadStats {
        self.read_cache.stats()
    }

    /// Lazily remove old private staging directories for exactly one
    /// authenticated scope. It is never called at startup and never scans
    /// another scope.
    pub async fn recover_stale_staging(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<usize, AppPackageStagingError> {
        #[cfg(not(unix))]
        {
            let _ = (self, authenticated_scope, now);
            return Err(AppPackageStagingError::UnsupportedPlatform);
        }
        #[cfg(unix)]
        {
            authenticated_scope.ensure_live_at(&now)?;
            let permit = Arc::clone(&self.blocking_slots)
                .acquire_owned()
                .await
                .map_err(|_| AppPackageStagingError::Overloaded)?;
            let workspace = self.workspace.clone();
            let scope = authenticated_scope.scope().clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                recover_stale_staging_blocking(&workspace, &scope, now)
            })
            .await
            .map_err(|error| AppPackageStagingError::WorkerTerminated(error.to_string()))?
        }
    }
}

/// Admit one local authoring directory through the exact descriptor-pinned,
/// bounded source reader used by package staging, without materializing any
/// runtime scope or package store. The authoring CLI consumes this seam so a
/// successful local check cannot disagree with server-side byte admission.
pub fn admit_package_directory(
    source_root: impl AsRef<Path>,
) -> Result<AppPackageCandidate, AppPackageStagingError> {
    #[cfg(unix)]
    {
        let source = open_existing_directory_path(source_root.as_ref(), PathUse::Source)?;
        return admit_descriptor_tree(source, PathUse::Source);
    }
    #[cfg(not(unix))]
    {
        let _ = source_root;
        Err(AppPackageStagingError::UnsupportedPlatform)
    }
}

fn stage_from_directory_blocking(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    source_root: &Path,
) -> Result<StagedAppPackage, AppPackageStagingError> {
    #[cfg(unix)]
    {
        let source = open_existing_directory_path(source_root, PathUse::Source)?;
        let candidate = admit_descriptor_tree(source, PathUse::Source)?;
        return publish_candidate_blocking(workspace, scope, candidate);
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, scope, source_root);
        Err(AppPackageStagingError::UnsupportedPlatform)
    }
}

fn publish_candidate_blocking(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    candidate: AppPackageCandidate,
) -> Result<StagedAppPackage, AppPackageStagingError> {
    validate_ordinary_staging_distribution(candidate.manifest().manifest().app.distribution)?;
    publish_admitted_candidate_blocking(workspace, scope, candidate)
}

/// Write one already distribution-admitted candidate into the scope's package
/// store. Callers own the distribution decision; this function does not repeat
/// it, which is why it is private to this module.
fn publish_admitted_candidate_blocking(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    candidate: AppPackageCandidate,
) -> Result<StagedAppPackage, AppPackageStagingError> {
    #[cfg(unix)]
    {
        let storage_digest = candidate.bundle_digest().clone();
        let index = encode_package_index(&candidate)?;
        let packages = open_packages_root(workspace, scope)?;
        let final_name = digest_directory_name(&storage_digest)?;
        let staging_name = format!(
            ".staging-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        );
        create_directory_at(&packages, &staging_name, 0o700)?;
        let staging = open_directory_at(&packages, &staging_name)?;
        let packages_identity = FileSnapshot::from_metadata(&packages.metadata()?);
        validate_directory_metadata(
            &staging.metadata()?,
            Some((packages_identity.device, packages_identity.owner)),
            PathUse::Destination,
        )?;

        let publish = (|| {
            write_candidate(&staging, &candidate, &index)?;
            match rename_entry_noclobber(&packages, &staging_name, &final_name) {
                Ok(()) => {
                    packages
                        .sync_all()
                        .map_err(|_| AppPackageStagingError::CommitStateUnknown)?;
                    Ok(AppPackageStageOutcome::Created)
                },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    verify_existing_package(&packages, &final_name, &candidate, &index)?;
                    remove_tree_at(&packages, &staging_name)?;
                    packages.sync_all()?;
                    Ok(AppPackageStageOutcome::AlreadyPresent)
                },
                Err(error) => Err(error.into()),
            }
        })();

        drop(staging);
        if publish.is_err() {
            let _ = remove_tree_at(&packages, &staging_name);
        }
        return publish.map(|outcome| StagedAppPackage {
            candidate,
            storage_digest,
            outcome,
        });
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, scope, candidate);
        Err(AppPackageStagingError::UnsupportedPlatform)
    }
}

fn validate_ordinary_staging_distribution(
    distribution: AppManifestDistribution,
) -> Result<(), AppPackageStagingError> {
    if distribution == AppManifestDistribution::System {
        return Err(AppPackageStagingError::UnsafeSource(
            "system-distribution packages require host-controlled digest-pinned boot admission",
        ));
    }
    Ok(())
}

fn load_staged_package_blocking(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    storage_digest: &AppDigest,
    #[cfg(unix)] cache: &read_cache::VerifiedPackageReadCache,
) -> Result<StagedAppPackage, AppPackageStagingError> {
    #[cfg(unix)]
    {
        // Export/read paths must not materialize an absent app store. A
        // missing package tree is a failed lookup, not permission to create
        // the scope binding or package directory as a side effect of GET.
        let packages = open_existing_packages_root(workspace, scope)?;
        let final_name = digest_directory_name(storage_digest)?;
        let final_directory = open_directory_at(&packages, &final_name)
            .map_err(|_| AppPackageStagingError::ContentConflict)?;
        let packages_identity = FileSnapshot::from_metadata(&packages.metadata()?);
        validate_directory_metadata(
            &final_directory.metadata()?,
            Some((packages_identity.device, packages_identity.owner)),
            PathUse::Destination,
        )
        .map_err(|_| AppPackageStagingError::ContentConflict)?;
        let names = list_directory_names(&final_directory, PathUse::Destination)?;
        if names != ["bundle".to_owned(), "package-index.json".to_owned()] {
            return Err(AppPackageStagingError::ContentConflict);
        }
        let bundle = open_directory_at(&final_directory, "bundle")?;
        validate_directory_metadata(
            &bundle.metadata()?,
            Some((packages_identity.device, packages_identity.owner)),
            PathUse::Destination,
        )
        .map_err(|_| AppPackageStagingError::ContentConflict)?;
        let observed_index = read_bounded_file_at(
            &final_directory,
            "package-index.json",
            MAX_PACKAGE_INDEX_BYTES,
        )?;
        let (_, fingerprint) =
            scan_descriptor_tree(bundle.try_clone()?, PathUse::Destination, false)?;
        if let Some(candidate) = cache.get(scope, storage_digest, &fingerprint, &observed_index) {
            return Ok(StagedAppPackage {
                candidate,
                storage_digest: storage_digest.clone(),
                outcome: AppPackageStageOutcome::AlreadyPresent,
            });
        }
        let (candidate, admitted_fingerprint) =
            scan_descriptor_tree(bundle, PathUse::Destination, true)?;
        let candidate = candidate.expect("full descriptor admission requested");
        if fingerprint != admitted_fingerprint {
            return Err(AppPackageStagingError::DestinationChanged);
        }
        if candidate.bundle_digest() != storage_digest {
            return Err(AppPackageStagingError::ContentConflict);
        }
        let expected_index = encode_package_index(&candidate)?;
        if observed_index != expected_index {
            return Err(AppPackageStagingError::ContentConflict);
        }
        cache.insert(
            scope,
            storage_digest,
            admitted_fingerprint,
            observed_index,
            &candidate,
        );
        return Ok(StagedAppPackage {
            candidate,
            storage_digest: storage_digest.clone(),
            outcome: AppPackageStageOutcome::AlreadyPresent,
        });
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, scope, storage_digest);
        Err(AppPackageStagingError::UnsupportedPlatform)
    }
}

/// Establish or verify the exact scope owner for a normalized `apps/` path.
/// The registry calls this before creating SQLite so package bytes and rows
/// share one collision boundary regardless of which Phase-1 writer runs first.
pub fn ensure_app_scope_binding(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
) -> Result<(), AppPackageStagingError> {
    #[cfg(unix)]
    {
        let apps = open_apps_root(workspace, scope)?;
        ensure_scope_binding_at(&apps, scope)
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, scope);
        Err(AppPackageStagingError::UnsupportedPlatform)
    }
}

/// Revalidate the exact immutable package bytes immediately before the
/// registry publishes a reviewable revision. This is a sibling-module seam,
/// not a path-based authority exposed to routes or app code.
pub fn verify_staged_package_snapshot(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    candidate: &AppPackageCandidate,
) -> Result<(), AppPackageStagingError> {
    #[cfg(unix)]
    {
        let packages = open_packages_root(workspace, scope)?;
        let final_name = digest_directory_name(candidate.bundle_digest())?;
        let index = encode_package_index(candidate)?;
        verify_existing_package(&packages, &final_name, candidate, &index)
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, scope, candidate);
        Err(AppPackageStagingError::UnsupportedPlatform)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn materialize_test_staged_package(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    candidate: AppPackageCandidate,
) -> StagedAppPackage {
    #[cfg(unix)]
    {
        let packages = open_packages_root(workspace, scope).unwrap();
        let final_name = digest_directory_name(candidate.bundle_digest()).unwrap();
        let index = encode_package_index(&candidate).unwrap();
        let outcome = if metadata_at(
            packages.as_raw_fd(),
            &c_string_component(&final_name, PathUse::Destination).unwrap(),
        )
        .is_ok()
        {
            verify_existing_package(&packages, &final_name, &candidate, &index).unwrap();
            AppPackageStageOutcome::AlreadyPresent
        } else {
            let staging_name = format!(".staging-test-{}", Uuid::new_v4().simple());
            create_directory_at(&packages, &staging_name, 0o700).unwrap();
            let staging = open_directory_at(&packages, &staging_name).unwrap();
            write_candidate(&staging, &candidate, &index).unwrap();
            drop(staging);
            rename_entry_noclobber(&packages, &staging_name, &final_name).unwrap();
            packages.sync_all().unwrap();
            AppPackageStageOutcome::Created
        };
        return StagedAppPackage {
            storage_digest: candidate.bundle_digest().clone(),
            candidate,
            outcome,
        };
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, scope);
        StagedAppPackage {
            storage_digest: candidate.bundle_digest().clone(),
            candidate,
            outcome: AppPackageStageOutcome::Created,
        }
    }
}

#[derive(Debug, Serialize)]
struct PackageIndex<'a> {
    schema_version: &'static str,
    bundle_digest: &'a AppDigest,
    manifest_digest: &'a AppDigest,
    members: Vec<PackageIndexMember<'a>>,
}

#[derive(Debug, Serialize)]
struct PackageIndexMember<'a> {
    path: &'a str,
    content_digest: &'a AppDigest,
    byte_len: u64,
}

#[derive(Debug, Serialize)]
struct AppScopeBinding<'a> {
    schema_version: &'static str,
    principal: &'a str,
    workspace: &'a str,
}

fn encode_package_index(
    candidate: &AppPackageCandidate,
) -> Result<Vec<u8>, AppPackageStagingError> {
    let members = candidate
        .members()
        .iter()
        .map(|member| PackageIndexMember {
            path: member.path().as_str(),
            content_digest: member.content_digest(),
            byte_len: u64::try_from(member.bytes().len()).unwrap_or(u64::MAX),
        })
        .collect();
    let value = serde_json::to_value(PackageIndex {
        schema_version: PACKAGE_INDEX_SCHEMA,
        bundle_digest: candidate.bundle_digest(),
        manifest_digest: candidate.manifest().manifest_digest(),
        members,
    })?;
    let bytes = canonical_json_bytes(&value)?;
    if bytes.len() > MAX_PACKAGE_INDEX_BYTES {
        return Err(AppPackageStagingError::UnsafeDestination(
            "package index exceeds its fixed byte limit",
        ));
    }
    Ok(bytes)
}

fn digest_directory_name(digest: &AppDigest) -> Result<String, AppPackageStagingError> {
    digest
        .as_str()
        .strip_prefix("blake3:")
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        .map(str::to_owned)
        .ok_or(AppPackageStagingError::UnsafeDestination(
            "bundle digest is not canonical",
        ))
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum PathUse {
    Source,
    Destination,
}

#[cfg(unix)]
impl PathUse {
    fn unsafe_error(self, message: &'static str) -> AppPackageStagingError {
        match self {
            Self::Source => AppPackageStagingError::UnsafeSource(message),
            Self::Destination => AppPackageStagingError::UnsafeDestination(message),
        }
    }

    fn changed_error(self) -> AppPackageStagingError {
        match self {
            Self::Source => AppPackageStagingError::SourceChanged,
            Self::Destination => AppPackageStagingError::DestinationChanged,
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(unix)]
impl FileSnapshot {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[cfg(unix)]
struct SourceFrame {
    directory: File,
    relative_components: Vec<String>,
    names: Vec<String>,
    next_name: usize,
    before: FileSnapshot,
}

#[cfg(unix)]
fn admit_descriptor_tree(
    root: File,
    path_use: PathUse,
) -> Result<AppPackageCandidate, AppPackageStagingError> {
    Ok(scan_descriptor_tree(root, path_use, true)?
        .0
        .expect("full descriptor admission requested"))
}

#[cfg(unix)]
fn scan_descriptor_tree(
    root: File,
    path_use: PathUse,
    include_bytes: bool,
) -> Result<(Option<AppPackageCandidate>, read_cache::Fingerprint), AppPackageStagingError> {
    let root_metadata = root.metadata()?;
    let root_snapshot = validate_directory_metadata(&root_metadata, None, path_use)?;
    let root_device = root_snapshot.device;
    let root_owner = root_snapshot.owner;
    let mut fingerprint = vec![(String::new(), root_snapshot)];
    let root_names = list_directory_names(&root, path_use)?;
    let mut stack = vec![SourceFrame {
        directory: root,
        relative_components: Vec::new(),
        names: root_names,
        next_name: 0,
        before: root_snapshot,
    }];
    let mut entry_count = 0usize;
    let mut file_count = 0usize;
    let limits = AppPackageLimits::default();
    let mut admission = include_bytes.then(AppBundleStagingAdmission::default);
    let mut total_bytes = 0_u64;

    while !stack.is_empty() {
        let frame_complete = stack
            .last()
            .is_some_and(|frame| frame.next_name >= frame.names.len());
        if frame_complete {
            let frame = stack.pop().expect("the stack was just observed non-empty");
            let after = FileSnapshot::from_metadata(&frame.directory.metadata()?);
            if after != frame.before
                || list_directory_names(&frame.directory, path_use)? != frame.names
            {
                return Err(path_use.changed_error());
            }
            continue;
        }

        let (parent_fd, name, relative_components) = {
            let frame = stack.last_mut().expect("the stack is non-empty");
            let name = frame.names[frame.next_name].clone();
            frame.next_name = frame.next_name.saturating_add(1);
            let mut relative_components = frame.relative_components.clone();
            relative_components.push(name.clone());
            (frame.directory.as_raw_fd(), name, relative_components)
        };
        entry_count = entry_count.saturating_add(1);
        if entry_count > MAX_DIRECTORY_ENTRIES {
            return Err(path_use.unsafe_error("directory entry limit exceeded"));
        }

        let name_c = c_string_component(&name, path_use)?;
        let named = metadata_at(parent_fd, &name_c)?;
        match file_type(named.st_mode) {
            libc::S_IFDIR => {
                if relative_components.len() > MAX_DIRECTORY_DEPTH {
                    return Err(path_use.unsafe_error("directory depth limit exceeded"));
                }
                validate_directory_stat(&named, root_device, root_owner, path_use)?;
                let child = open_directory_at_raw(parent_fd, &name_c)?;
                let opened = child.metadata()?;
                if !stat_matches_metadata(&named, &opened) {
                    return Err(path_use.changed_error());
                }
                let snapshot = validate_directory_metadata(
                    &opened,
                    Some((root_device, root_owner)),
                    path_use,
                )?;
                let names = list_directory_names(&child, path_use)?;
                fingerprint.push((relative_components.join("/"), snapshot));
                stack.push(SourceFrame {
                    directory: child,
                    relative_components,
                    names,
                    next_name: 0,
                    before: snapshot,
                });
            },
            libc::S_IFREG => {
                file_count = file_count.saturating_add(1);
                if file_count > limits.max_bundle_files() {
                    return Err(path_use.unsafe_error("regular-file limit exceeded"));
                }
                validate_regular_stat(&named, root_device, root_owner, path_use)?;
                let mut file = open_file_at_raw(
                    parent_fd,
                    &name_c,
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )?;
                let opened = file.metadata()?;
                if !stat_matches_metadata(&named, &opened) {
                    return Err(path_use.changed_error());
                }
                let before = validate_regular_metadata(&opened, root_device, root_owner, path_use)?;
                let relative_path = relative_components.join("/");
                if relative_path.len() > limits.max_bundle_path_bytes() {
                    return Err(path_use.unsafe_error("bundle path byte limit exceeded"));
                }
                total_bytes = total_bytes.saturating_add(before.length);
                if before.length > limits.max_bundle_file_bytes() as u64
                    || total_bytes > limits.max_bundle_bytes() as u64
                {
                    return Err(path_use.unsafe_error("package byte limit exceeded"));
                }
                fingerprint.push((relative_path.clone(), before));
                if let Some(admission) = &mut admission {
                    admission.push_regular_reader(relative_path, Some(before.length), &mut file)?;
                }
                let after = FileSnapshot::from_metadata(&file.metadata()?);
                let renamed = metadata_at(parent_fd, &name_c)?;
                if before != after || !stat_matches_metadata(&renamed, &opened) {
                    return Err(path_use.changed_error());
                }
            },
            libc::S_IFLNK => return Err(path_use.unsafe_error("symbolic links are forbidden")),
            _ => {
                return Err(path_use.unsafe_error("only regular files and directories are allowed"))
            },
        }
    }
    Ok((
        admission
            .map(AppBundleStagingAdmission::finish)
            .transpose()?,
        fingerprint,
    ))
}

#[cfg(unix)]
fn open_existing_directory_path(
    path: &Path,
    path_use: PathUse,
) -> Result<File, AppPackageStagingError> {
    let absolute = absolute_normal_path(path, path_use)?;
    let mut current = open_root_directory()?;
    for component in absolute.components() {
        match component {
            Component::RootDir => {},
            Component::Normal(segment) => {
                let segment = segment
                    .to_str()
                    .ok_or_else(|| path_use.unsafe_error("path is not valid UTF-8"))?;
                current = open_directory_at(&current, segment)?;
            },
            _ => return Err(path_use.unsafe_error("path contains a non-normal component")),
        }
    }
    let metadata = current.metadata()?;
    validate_directory_metadata(&metadata, None, path_use)?;
    Ok(current)
}

#[cfg(unix)]
fn open_packages_root(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
) -> Result<File, AppPackageStagingError> {
    let apps = open_apps_root(workspace, scope)?;
    ensure_scope_binding_at(&apps, scope)?;
    let packages = match open_directory_at(&apps, "packages") {
        Ok(directory) => directory,
        Err(AppPackageStagingError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            create_directory_at(&apps, "packages", 0o700)?;
            apps.sync_all()?;
            open_directory_at(&apps, "packages")?
        },
        Err(error) => return Err(error),
    };
    let apps_identity = FileSnapshot::from_metadata(&apps.metadata()?);
    validate_directory_metadata(
        &packages.metadata()?,
        Some((apps_identity.device, apps_identity.owner)),
        PathUse::Destination,
    )?;
    Ok(packages)
}

#[cfg(unix)]
fn open_existing_packages_root(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
) -> Result<File, AppPackageStagingError> {
    let apps = open_existing_apps_root(workspace, scope)?;
    verify_scope_binding_at(&apps, scope)?;
    let packages = open_directory_at(&apps, "packages")?;
    let apps_identity = FileSnapshot::from_metadata(&apps.metadata()?);
    validate_directory_metadata(
        &packages.metadata()?,
        Some((apps_identity.device, apps_identity.owner)),
        PathUse::Destination,
    )?;
    Ok(packages)
}

#[cfg(unix)]
fn open_apps_root(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
) -> Result<File, AppPackageStagingError> {
    let base_path = absolute_normal_path(workspace.base_root(), PathUse::Destination)?;
    let apps_path = absolute_normal_path(
        &workspace.apps_root(scope.principal.as_str(), scope.workspace.as_str()),
        PathUse::Destination,
    )?;
    let relative = apps_path
        .strip_prefix(&base_path)
        .map_err(|_| AppPackageStagingError::UnsafeDestination("apps path escapes workspace"))?;
    let mut current = open_existing_directory_path(&base_path, PathUse::Destination)?;
    let root = FileSnapshot::from_metadata(&current.metadata()?);
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(AppPackageStagingError::UnsafeDestination(
                "packages path contains a non-normal component",
            ));
        };
        let segment = segment
            .to_str()
            .ok_or(AppPackageStagingError::UnsafeDestination(
                "packages path is not valid UTF-8",
            ))?;
        match open_directory_at(&current, segment) {
            Ok(directory) => current = directory,
            Err(AppPackageStagingError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                create_directory_at(&current, segment, 0o700)?;
                current.sync_all()?;
                current = open_directory_at(&current, segment)?;
            },
            Err(error) => return Err(error),
        }
        validate_directory_metadata(
            &current.metadata()?,
            Some((root.device, root.owner)),
            PathUse::Destination,
        )?;
    }
    Ok(current)
}

#[cfg(unix)]
fn open_existing_apps_root(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
) -> Result<File, AppPackageStagingError> {
    let base_path = absolute_normal_path(workspace.base_root(), PathUse::Destination)?;
    let apps_path = absolute_normal_path(
        &workspace.apps_root(scope.principal.as_str(), scope.workspace.as_str()),
        PathUse::Destination,
    )?;
    let relative = apps_path
        .strip_prefix(&base_path)
        .map_err(|_| AppPackageStagingError::UnsafeDestination("apps path escapes workspace"))?;
    let mut current = open_existing_directory_path(&base_path, PathUse::Destination)?;
    let root = FileSnapshot::from_metadata(&current.metadata()?);
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(AppPackageStagingError::UnsafeDestination(
                "apps path contains a non-normal component",
            ));
        };
        let segment = segment
            .to_str()
            .ok_or(AppPackageStagingError::UnsafeDestination(
                "apps path is not valid UTF-8",
            ))?;
        current = open_directory_at(&current, segment)?;
        validate_directory_metadata(
            &current.metadata()?,
            Some((root.device, root.owner)),
            PathUse::Destination,
        )?;
    }
    Ok(current)
}

#[cfg(unix)]
fn ensure_scope_binding_at(apps: &File, scope: &AppScope) -> Result<(), AppPackageStagingError> {
    let value = serde_json::to_value(AppScopeBinding {
        schema_version: APP_SCOPE_BINDING_SCHEMA,
        principal: scope.principal.as_str(),
        workspace: scope.workspace.as_str(),
    })?;
    let expected = canonical_json_bytes(&value)?;
    if expected.len() > 1_024 {
        return Err(AppPackageStagingError::UnsafeDestination(
            "scope binding exceeds its fixed byte limit",
        ));
    }

    match read_bounded_file_at(apps, APP_SCOPE_BINDING_FILE, 1_024) {
        Ok(existing) => {
            return if existing == expected {
                Ok(())
            } else {
                Err(AppPackageStagingError::ScopeCollision)
            };
        },
        Err(AppPackageStagingError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
        },
        Err(error) => return Err(error),
    }

    let temporary_name = format!(".scope-binding-{}.tmp", Uuid::new_v4().simple());
    write_new_file(apps, &temporary_name, &expected)?;
    match rename_entry_noclobber(apps, &temporary_name, APP_SCOPE_BINDING_FILE) {
        Ok(()) => apps
            .sync_all()
            .map_err(|_| AppPackageStagingError::CommitStateUnknown),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = unlink_at(apps, &temporary_name, false);
            apps.sync_all()?;
            let existing = read_bounded_file_at(apps, APP_SCOPE_BINDING_FILE, 1_024)?;
            if existing == expected {
                Ok(())
            } else {
                Err(AppPackageStagingError::ScopeCollision)
            }
        },
        Err(error) => {
            let _ = unlink_at(apps, &temporary_name, false);
            let _ = apps.sync_all();
            Err(error.into())
        },
    }
}

#[cfg(unix)]
fn verify_scope_binding_at(apps: &File, scope: &AppScope) -> Result<(), AppPackageStagingError> {
    let value = serde_json::to_value(AppScopeBinding {
        schema_version: APP_SCOPE_BINDING_SCHEMA,
        principal: scope.principal.as_str(),
        workspace: scope.workspace.as_str(),
    })?;
    let expected = canonical_json_bytes(&value)?;
    if expected.len() > 1_024 {
        return Err(AppPackageStagingError::UnsafeDestination(
            "scope binding exceeds its fixed byte limit",
        ));
    }
    let existing = read_bounded_file_at(apps, APP_SCOPE_BINDING_FILE, 1_024)?;
    if existing == expected {
        Ok(())
    } else {
        Err(AppPackageStagingError::ScopeCollision)
    }
}

#[cfg(unix)]
fn write_candidate(
    staging: &File,
    candidate: &AppPackageCandidate,
    index: &[u8],
) -> Result<(), AppPackageStagingError> {
    create_directory_at(staging, "bundle", 0o700)?;
    staging.sync_all()?;
    let bundle = open_directory_at(staging, "bundle")?;
    for member in candidate.members() {
        write_member(&bundle, member.path().as_str(), member.bytes())?;
    }
    bundle.sync_all()?;
    write_new_file(staging, "package-index.json", index)?;
    staging.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn write_member(root: &File, path: &str, bytes: &[u8]) -> Result<(), AppPackageStagingError> {
    let components = path.split('/').collect::<Vec<_>>();
    let (file_name, parents) =
        components
            .split_last()
            .ok_or(AppPackageStagingError::UnsafeDestination(
                "empty bundle path",
            ))?;
    let mut current = root.try_clone()?;
    for parent in parents {
        match open_directory_at(&current, parent) {
            Ok(directory) => current = directory,
            Err(AppPackageStagingError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                create_directory_at(&current, parent, 0o700)?;
                current.sync_all()?;
                current = open_directory_at(&current, parent)?;
            },
            Err(error) => return Err(error),
        }
    }
    write_new_file(&current, file_name, bytes)?;
    current.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn write_new_file(
    directory: &File,
    name: &str,
    bytes: &[u8],
) -> Result<(), AppPackageStagingError> {
    let name = c_string_component(name, PathUse::Destination)?;
    let mut file = open_file_at_raw(
        directory.as_raw_fd(),
        &name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o600,
    )?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn verify_existing_package(
    packages: &File,
    final_name: &str,
    expected: &AppPackageCandidate,
    expected_index: &[u8],
) -> Result<(), AppPackageStagingError> {
    let final_directory = open_directory_at(packages, final_name)
        .map_err(|_| AppPackageStagingError::ContentConflict)?;
    let packages_identity = FileSnapshot::from_metadata(&packages.metadata()?);
    validate_directory_metadata(
        &final_directory.metadata()?,
        Some((packages_identity.device, packages_identity.owner)),
        PathUse::Destination,
    )
    .map_err(|_| AppPackageStagingError::ContentConflict)?;
    let names = list_directory_names(&final_directory, PathUse::Destination)?;
    if names != ["bundle".to_owned(), "package-index.json".to_owned()] {
        return Err(AppPackageStagingError::ContentConflict);
    }
    let index = read_bounded_file_at(
        &final_directory,
        "package-index.json",
        MAX_PACKAGE_INDEX_BYTES,
    )?;
    if index != expected_index {
        return Err(AppPackageStagingError::ContentConflict);
    }
    let bundle = open_directory_at(&final_directory, "bundle")?;
    validate_directory_metadata(
        &bundle.metadata()?,
        Some((packages_identity.device, packages_identity.owner)),
        PathUse::Destination,
    )
    .map_err(|_| AppPackageStagingError::ContentConflict)?;
    let observed = admit_descriptor_tree(bundle, PathUse::Destination)?;
    if observed.bundle_digest() != expected.bundle_digest()
        || observed.manifest().manifest_digest() != expected.manifest().manifest_digest()
        || observed.members().len() != expected.members().len()
        || observed
            .members()
            .iter()
            .zip(expected.members())
            .any(|(left, right)| {
                left.path() != right.path()
                    || left.content_digest() != right.content_digest()
                    || left.bytes() != right.bytes()
            })
    {
        return Err(AppPackageStagingError::ContentConflict);
    }
    Ok(())
}

#[cfg(unix)]
fn read_bounded_file_at(
    directory: &File,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, AppPackageStagingError> {
    let name = c_string_component(name, PathUse::Destination)?;
    let mut file = open_file_at_raw(
        directory.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )?;
    let before = FileSnapshot::from_metadata(&file.metadata()?);
    if before.mode & libc::S_IFMT as u32 != libc::S_IFREG as u32
        || before.links != 1
        || before.owner != effective_user_id()
        || before.mode & 0o022 != 0
        || before.length > u64::try_from(limit).unwrap_or(u64::MAX)
    {
        return Err(AppPackageStagingError::ContentConflict);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(before.length).unwrap_or(0));
    std::io::Read::by_ref(&mut file)
        .take(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    let after_metadata = file.metadata()?;
    let after = FileSnapshot::from_metadata(&after_metadata);
    let named = metadata_at(directory.as_raw_fd(), &name)?;
    if bytes.len() > limit || after != before || !stat_matches_metadata(&named, &after_metadata) {
        return Err(AppPackageStagingError::ContentConflict);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn recover_stale_staging_blocking(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    now: DateTime<Utc>,
) -> Result<usize, AppPackageStagingError> {
    let packages = open_packages_root(workspace, scope)?;
    let candidates = list_staging_candidate_names(&packages)?;
    if candidates.len() > MAX_RECOVERY_CANDIDATES {
        return Err(AppPackageStagingError::RecoveryLimitExceeded);
    }
    let cutoff = now - ChronoDuration::seconds(STALE_STAGING_SECONDS);
    let packages_identity = FileSnapshot::from_metadata(&packages.metadata()?);
    let mut removed = 0usize;
    for name in candidates {
        let name_c = c_string_component(&name, PathUse::Destination)?;
        let metadata = metadata_at(packages.as_raw_fd(), &name_c)?;
        if file_type(metadata.st_mode) != libc::S_IFDIR {
            continue;
        }
        validate_directory_stat(
            &metadata,
            packages_identity.device,
            packages_identity.owner,
            PathUse::Destination,
        )?;
        let modified =
            DateTime::<Utc>::from_timestamp(metadata.st_mtime, stat_mtime_nanoseconds(&metadata))
                .ok_or(AppPackageStagingError::UnsafeDestination(
                "staging timestamp is invalid",
            ))?;
        if modified < cutoff {
            remove_tree_at(&packages, &name)?;
            removed = removed.saturating_add(1);
        }
    }
    if removed != 0 {
        packages.sync_all()?;
    }
    Ok(removed)
}

#[cfg(unix)]
fn absolute_normal_path(path: &Path, path_use: PathUse) -> Result<PathBuf, AppPackageStagingError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute
        .components()
        .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(path_use.unsafe_error("path contains dot or parent traversal"));
    }
    Ok(absolute)
}

#[cfg(unix)]
fn open_root_directory() -> Result<File, AppPackageStagingError> {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY);
    Ok(options.open("/")?)
}

#[cfg(unix)]
fn open_directory_at(directory: &File, name: &str) -> Result<File, AppPackageStagingError> {
    let name = c_string_component(name, PathUse::Destination)?;
    open_directory_at_raw(directory.as_raw_fd(), &name)
}

#[cfg(unix)]
fn open_directory_at_raw(
    directory_fd: RawFd,
    name: &CString,
) -> Result<File, AppPackageStagingError> {
    open_file_at_raw(
        directory_fd,
        name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        0,
    )
}

#[cfg(unix)]
fn open_file_at_raw(
    directory_fd: RawFd,
    name: &CString,
    flags: i32,
    mode: u32,
) -> Result<File, AppPackageStagingError> {
    // SAFETY: `directory_fd` is retained by the caller and `name` is one
    // NUL-free component. `openat` returns a fresh owned descriptor on success.
    let descriptor = unsafe { libc::openat(directory_fd, name.as_ptr(), flags, mode) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: the descriptor above is freshly owned.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn create_directory_at(
    directory: &File,
    name: &str,
    mode: libc::mode_t,
) -> Result<(), AppPackageStagingError> {
    let name = c_string_component(name, PathUse::Destination)?;
    // SAFETY: the directory descriptor and component remain live for mkdirat.
    let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), mode) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(unix)]
fn metadata_at(directory_fd: RawFd, name: &CString) -> Result<libc::stat, AppPackageStagingError> {
    // SAFETY: zero is a valid initial stat value and fstatat initializes it on
    // success. The descriptor and component are retained by the caller.
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::fstatat(
            directory_fd,
            name.as_ptr(),
            &mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(metadata)
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(unix)]
fn validate_directory_metadata(
    metadata: &fs::Metadata,
    root: Option<(u64, u32)>,
    path_use: PathUse,
) -> Result<FileSnapshot, AppPackageStagingError> {
    let snapshot = FileSnapshot::from_metadata(metadata);
    if !metadata.is_dir() || snapshot.mode & 0o022 != 0 {
        return Err(path_use.unsafe_error("directory is not private and real"));
    }
    if snapshot.owner != effective_user_id() {
        return Err(path_use.unsafe_error("directory owner is not the current user"));
    }
    if root.is_some_and(|(device, owner)| snapshot.device != device || snapshot.owner != owner) {
        return Err(path_use.unsafe_error("directory crosses its admitted authority root"));
    }
    Ok(snapshot)
}

#[cfg(unix)]
fn validate_directory_stat(
    metadata: &libc::stat,
    root_device: u64,
    root_owner: u32,
    path_use: PathUse,
) -> Result<(), AppPackageStagingError> {
    if file_type(metadata.st_mode) != libc::S_IFDIR
        || metadata.st_dev as u64 != root_device
        || metadata.st_uid != root_owner
        || metadata.st_mode & 0o022 != 0
    {
        return Err(path_use.unsafe_error("directory escaped the admitted source root"));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_regular_stat(
    metadata: &libc::stat,
    root_device: u64,
    root_owner: u32,
    path_use: PathUse,
) -> Result<(), AppPackageStagingError> {
    if file_type(metadata.st_mode) != libc::S_IFREG
        || metadata.st_dev as u64 != root_device
        || metadata.st_uid != root_owner
        || metadata.st_mode & 0o022 != 0
        || metadata.st_nlink != 1
    {
        return Err(path_use.unsafe_error("regular file has unsafe ownership or links"));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_regular_metadata(
    metadata: &fs::Metadata,
    root_device: u64,
    root_owner: u32,
    path_use: PathUse,
) -> Result<FileSnapshot, AppPackageStagingError> {
    let snapshot = FileSnapshot::from_metadata(metadata);
    if !metadata.is_file()
        || snapshot.device != root_device
        || snapshot.owner != root_owner
        || snapshot.mode & 0o022 != 0
        || snapshot.links != 1
    {
        return Err(path_use.unsafe_error("regular file has unsafe ownership or links"));
    }
    Ok(snapshot)
}

#[cfg(unix)]
fn stat_matches_metadata(stat: &libc::stat, metadata: &fs::Metadata) -> bool {
    stat.st_dev as u64 == metadata.dev()
        && stat.st_ino as u64 == metadata.ino()
        && stat.st_uid == metadata.uid()
        && stat.st_mode as u32 == metadata.mode()
        && stat.st_size >= 0
        && stat.st_size as u64 == metadata.len()
}

#[cfg(unix)]
fn list_directory_names(
    directory: &File,
    path_use: PathUse,
) -> Result<Vec<String>, AppPackageStagingError> {
    collect_directory_names(
        directory,
        path_use,
        MAX_DIRECTORY_ENTRIES,
        MAX_DIRECTORY_ENTRIES,
        |_| true,
    )
}

#[cfg(unix)]
fn list_staging_candidate_names(directory: &File) -> Result<Vec<String>, AppPackageStagingError> {
    collect_directory_names(
        directory,
        PathUse::Destination,
        MAX_RECOVERY_SCAN_ENTRIES,
        MAX_RECOVERY_CANDIDATES.saturating_add(1),
        |name| name.starts_with(".staging-"),
    )
}

#[cfg(unix)]
fn collect_directory_names(
    directory: &File,
    path_use: PathUse,
    max_scanned: usize,
    max_retained: usize,
    mut retain: impl FnMut(&str) -> bool,
) -> Result<Vec<String>, AppPackageStagingError> {
    // Open `.` relative to the retained authority so each scan owns an
    // independent directory offset. `dup` is not sufficient here: duplicated
    // directory descriptors share one open-file description, so the first
    // `readdir` would advance the retained descriptor to EOF and make the
    // mandatory revalidation scan falsely report a changed directory.
    let current = c".";
    // SAFETY: the retained descriptor names a live directory and `.` cannot
    // escape it. The returned descriptor is freshly owned on success.
    let scan_descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            current.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
            0,
        )
    };
    if scan_descriptor < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: the fresh scan descriptor names an open directory.
    let stream = unsafe { libc::fdopendir(scan_descriptor) };
    if stream.is_null() {
        // SAFETY: fdopendir did not take ownership on failure.
        unsafe { libc::close(scan_descriptor) };
        return Err(std::io::Error::last_os_error().into());
    }
    let result = (|| {
        let mut names = Vec::new();
        let mut scanned = 0usize;
        loop {
            clear_errno();
            // SAFETY: `stream` is valid until closed below; readdir's pointer
            // remains valid until the next call and is copied immediately.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                if current_errno() == 0 {
                    break;
                }
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: POSIX dirent names are NUL-terminated.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            scanned = scanned.saturating_add(1);
            if scanned > max_scanned {
                return Err(path_use.unsafe_error("directory scan limit exceeded"));
            }
            let name = OsString::from_vec(bytes.to_vec())
                .into_string()
                .map_err(|_| path_use.unsafe_error("non-UTF-8 entry name"))?;
            if retain(&name) {
                names.push(name);
                if names.len() > max_retained {
                    return Err(path_use.unsafe_error("matching entry limit exceeded"));
                }
            }
        }
        names.sort();
        Ok(names)
    })();
    // SAFETY: fdopendir owns the duplicate; closedir releases it exactly once.
    let close_result = unsafe { libc::closedir(stream) };
    if result.is_ok() && close_result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    result
}

#[cfg(unix)]
fn c_string_component(name: &str, path_use: PathUse) -> Result<CString, AppPackageStagingError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(path_use.unsafe_error("path component is not a single safe segment"));
    }
    CString::new(name).map_err(|_| path_use.unsafe_error("path component contains NUL"))
}

#[cfg(unix)]
fn file_type(mode: libc::mode_t) -> libc::mode_t {
    mode & libc::S_IFMT
}

#[cfg(unix)]
fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(target_os = "macos")]
fn clear_errno() {
    // SAFETY: __error returns the calling thread's errno location.
    unsafe { *libc::__error() = 0 };
}

#[cfg(target_os = "macos")]
fn current_errno() -> i32 {
    // SAFETY: __error returns the calling thread's errno location.
    unsafe { *libc::__error() }
}

#[cfg(target_os = "linux")]
fn clear_errno() {
    // SAFETY: __errno_location returns the calling thread's errno location.
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(target_os = "linux")]
fn current_errno() -> i32 {
    // SAFETY: __errno_location returns the calling thread's errno location.
    unsafe { *libc::__errno_location() }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clear_errno() {}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn current_errno() -> i32 {
    0
}

#[cfg(target_os = "macos")]
fn stat_mtime_nanoseconds(metadata: &libc::stat) -> u32 {
    u32::try_from(metadata.st_mtime_nsec).unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn stat_mtime_nanoseconds(metadata: &libc::stat) -> u32 {
    u32::try_from(metadata.st_mtime_nsec).unwrap_or(0)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn stat_mtime_nanoseconds(_metadata: &libc::stat) -> u32 {
    0
}

#[cfg(target_os = "macos")]
fn rename_entry_noclobber(
    directory: &File,
    source: &str,
    destination: &str,
) -> std::io::Result<()> {
    const RENAME_EXCL: u32 = 0x0000_0004;
    unsafe extern "C" {
        fn renameatx_np(
            from_fd: libc::c_int,
            from: *const libc::c_char,
            to_fd: libc::c_int,
            to: *const libc::c_char,
            flags: libc::c_uint,
        ) -> libc::c_int;
    }
    let source =
        CString::new(source).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let destination = CString::new(destination)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: both names and the pinned directory remain live. RENAME_EXCL
    // provides create-only atomic publication.
    let result = unsafe {
        renameatx_np(
            directory.as_raw_fd(),
            source.as_ptr(),
            directory.as_raw_fd(),
            destination.as_ptr(),
            RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rename_entry_noclobber(
    directory: &File,
    source: &str,
    destination: &str,
) -> std::io::Result<()> {
    const RENAME_NOREPLACE: libc::c_uint = 1;
    let source =
        CString::new(source).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let destination = CString::new(destination)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: renameat2 receives two live single-component names and a pinned
    // directory descriptor; RENAME_NOREPLACE provides create-only publication.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            directory.as_raw_fd(),
            source.as_ptr(),
            directory.as_raw_fd(),
            destination.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn rename_entry_noclobber(
    _directory: &File,
    _source: &str,
    _destination: &str,
) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

#[cfg(unix)]
fn remove_tree_at(parent: &File, name: &str) -> Result<(), AppPackageStagingError> {
    struct RemoveFrame {
        directory: File,
        name_in_parent: String,
        names: Vec<String>,
        next_name: usize,
    }

    let parent_identity = FileSnapshot::from_metadata(&parent.metadata()?);
    let name_c = c_string_component(name, PathUse::Destination)?;
    let root_named = metadata_at(parent.as_raw_fd(), &name_c)?;
    validate_directory_stat(
        &root_named,
        parent_identity.device,
        parent_identity.owner,
        PathUse::Destination,
    )?;
    let root = open_directory_at(parent, name)?;
    if !stat_matches_metadata(&root_named, &root.metadata()?) {
        return Err(AppPackageStagingError::DestinationChanged);
    }
    let root_names = list_directory_names(&root, PathUse::Destination)?;
    let mut stack = vec![RemoveFrame {
        directory: root,
        name_in_parent: name.to_owned(),
        names: root_names,
        next_name: 0,
    }];
    let mut visited = 0usize;
    while !stack.is_empty() {
        let complete = stack
            .last()
            .is_some_and(|frame| frame.next_name >= frame.names.len());
        if complete {
            let frame = stack.pop().expect("remove stack is non-empty");
            frame.directory.sync_all()?;
            drop(frame.directory);
            let parent_directory = stack.last().map(|frame| &frame.directory).unwrap_or(parent);
            unlink_at(parent_directory, &frame.name_in_parent, true)?;
            parent_directory.sync_all()?;
            continue;
        }
        let (directory_fd, child_name) = {
            let frame = stack.last_mut().expect("remove stack is non-empty");
            let child_name = frame.names[frame.next_name].clone();
            frame.next_name = frame.next_name.saturating_add(1);
            (frame.directory.as_raw_fd(), child_name)
        };
        visited = visited.saturating_add(1);
        if visited > MAX_DIRECTORY_ENTRIES {
            return Err(AppPackageStagingError::RecoveryLimitExceeded);
        }
        let child_c = c_string_component(&child_name, PathUse::Destination)?;
        let metadata = metadata_at(directory_fd, &child_c)?;
        if file_type(metadata.st_mode) == libc::S_IFDIR {
            if stack.len() >= MAX_DIRECTORY_DEPTH {
                return Err(AppPackageStagingError::RecoveryLimitExceeded);
            }
            validate_directory_stat(
                &metadata,
                parent_identity.device,
                parent_identity.owner,
                PathUse::Destination,
            )?;
            let parent_directory = stack
                .last()
                .expect("remove stack is non-empty")
                .directory
                .try_clone()?;
            let child = open_directory_at(&parent_directory, &child_name)?;
            let names = list_directory_names(&child, PathUse::Destination)?;
            stack.push(RemoveFrame {
                directory: child,
                name_in_parent: child_name,
                names,
                next_name: 0,
            });
        } else {
            let directory = &stack.last().expect("remove stack is non-empty").directory;
            unlink_at(directory, &child_name, false)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn unlink_at(
    directory: &File,
    name: &str,
    directory_entry: bool,
) -> Result<(), AppPackageStagingError> {
    let name = c_string_component(name, PathUse::Destination)?;
    let flags = if directory_entry {
        libc::AT_REMOVEDIR
    } else {
        0
    };
    // SAFETY: the pinned directory and component remain live for unlinkat.
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::apps::{
        authority::AuthenticatedAppScope,
        manifest::tests::valid_skill_document,
        models::{AppReference, AppRevision, AppScopeBindingRef},
        records::AppScope,
    };

    fn canonical_tempdir() -> TempDir {
        let root = fs::canonicalize(std::env::temp_dir()).expect("canonical temporary root");
        tempfile::tempdir_in(root).expect("temporary directory")
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 15, 12, 0, 0).unwrap()
    }

    fn authenticated_scope_for(
        principal: &str,
        workspace: &str,
        now: DateTime<Utc>,
    ) -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: AppReference::parse(principal).unwrap(),
                workspace: AppReference::parse(workspace).unwrap(),
            },
            AppScopeBindingRef::parse("scope_binding_test").unwrap(),
            AppReference::parse("actor:test").unwrap(),
            AppReference::parse("session:test").unwrap(),
            AppRevision::new(1).unwrap(),
            now,
            now + ChronoDuration::minutes(30),
        )
        .unwrap()
    }

    fn authenticated_scope(now: DateTime<Utc>) -> AuthenticatedAppScope {
        authenticated_scope_for("anonymous", "default", now)
    }

    fn source_package() -> TempDir {
        let source = canonical_tempdir();
        fs::write(source.path().join("SKILL.md"), valid_skill_document()).unwrap();
        fs::create_dir(source.path().join("workflows")).unwrap();
        fs::write(source.path().join("workflows/build.md"), b"Build a plan.").unwrap();
        fs::create_dir(source.path().join("assets")).unwrap();
        fs::write(source.path().join("assets/icon.svg"), b"<svg/>").unwrap();
        fs::create_dir_all(source.path().join("vendor/skills/summarize/bin")).unwrap();
        fs::write(
            source.path().join("vendor/skills/summarize/SKILL.md"),
            b"---\nname: summarize\nversion: 2.1.0\n---\n",
        )
        .unwrap();
        fs::write(
            source
                .path()
                .join("vendor/skills/summarize/bin/summarize.py"),
            b"print('summary')\n",
        )
        .unwrap();
        source
    }

    #[test]
    fn ordinary_stager_refuses_system_distribution_before_writing() {
        assert!(
            validate_ordinary_staging_distribution(AppManifestDistribution::Installable).is_ok()
        );
        assert!(matches!(
            validate_ordinary_staging_distribution(AppManifestDistribution::System),
            Err(AppPackageStagingError::UnsafeSource(_))
        ));
    }

    #[test]
    fn repeated_descriptor_directory_scans_use_independent_offsets() {
        let source = source_package();
        let directory =
            open_existing_directory_path(source.path(), PathUse::Source).expect("open source");

        let first = list_directory_names(&directory, PathUse::Source).expect("first scan");
        let second = list_directory_names(&directory, PathUse::Source).expect("second scan");

        assert!(!first.is_empty());
        assert_eq!(second, first);
    }

    #[tokio::test]
    async fn stages_complete_package_and_exact_replay_is_idempotent() {
        let storage = canonical_tempdir();
        let source = source_package();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let scope = authenticated_scope(now);

        let first = service
            .stage_from_directory(&scope, source.path(), now)
            .await
            .unwrap();
        assert_eq!(first.outcome(), AppPackageStageOutcome::Created);
        let second = service
            .stage_from_directory(&scope, source.path(), now)
            .await
            .unwrap();
        assert_eq!(second.outcome(), AppPackageStageOutcome::AlreadyPresent);
        assert_eq!(first.storage_digest(), second.storage_digest());
        let loaded = service
            .load_staged_package(&scope, first.storage_digest().clone(), now)
            .await
            .unwrap();
        assert_eq!(loaded.storage_digest(), first.storage_digest());
        assert_eq!(loaded.candidate(), first.candidate());

        let final_name = digest_directory_name(first.storage_digest()).unwrap();
        let final_root = storage
            .path()
            .join("scopes/anonymous/default/apps/packages")
            .join(final_name);
        assert_eq!(
            fs::read(final_root.join("bundle/assets/icon.svg")).unwrap(),
            b"<svg/>"
        );
        assert!(final_root.join("package-index.json").is_file());
    }

    #[tokio::test]
    async fn missing_package_read_does_not_materialize_app_storage() {
        let storage = canonical_tempdir();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let scope = authenticated_scope(now);

        assert!(service
            .load_staged_package(&scope, AppDigest::blake3(b"missing-package"), now)
            .await
            .is_err());
        assert!(!storage
            .path()
            .join("scopes/anonymous/default/apps")
            .exists());
    }

    #[tokio::test]
    async fn rejects_symlink_and_hardlink_members() {
        let storage = canonical_tempdir();
        let source = source_package();
        symlink("assets/icon.svg", source.path().join("linked.txt")).unwrap();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let scope = authenticated_scope(now);
        assert!(matches!(
            service
                .stage_from_directory(&scope, source.path(), now)
                .await,
            Err(AppPackageStagingError::UnsafeSource(_))
        ));

        fs::remove_file(source.path().join("linked.txt")).unwrap();
        fs::hard_link(
            source.path().join("assets/icon.svg"),
            source.path().join("hardlink.txt"),
        )
        .unwrap();
        assert!(matches!(
            service
                .stage_from_directory(&scope, source.path(), now)
                .await,
            Err(AppPackageStagingError::UnsafeSource(_))
        ));
    }

    #[tokio::test]
    async fn rejects_group_writable_source_tree() {
        let storage = canonical_tempdir();
        let source = source_package();
        fs::set_permissions(source.path(), fs::Permissions::from_mode(0o770)).unwrap();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let scope = authenticated_scope(now);
        assert!(matches!(
            service
                .stage_from_directory(&scope, source.path(), now)
                .await,
            Err(AppPackageStagingError::UnsafeSource(_))
        ));
    }

    #[tokio::test]
    async fn conflicting_existing_digest_directory_fails_closed() {
        let storage = canonical_tempdir();
        let source = source_package();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let scope = authenticated_scope(now);
        let first = service
            .stage_from_directory(&scope, source.path(), now)
            .await
            .unwrap();
        let final_name = digest_directory_name(first.storage_digest()).unwrap();
        let index = storage
            .path()
            .join("scopes/anonymous/default/apps/packages")
            .join(final_name)
            .join("package-index.json");
        fs::write(index, b"corrupt").unwrap();

        assert!(matches!(
            service
                .load_staged_package(&scope, first.storage_digest().clone(), now)
                .await,
            Err(AppPackageStagingError::ContentConflict)
        ));
        assert!(matches!(
            service
                .stage_from_directory(&scope, source.path(), now)
                .await,
            Err(AppPackageStagingError::ContentConflict)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn expired_authentication_and_cancelled_capacity_wait_touch_no_destination() {
        let storage = canonical_tempdir();
        let source = source_package();
        let issued = now();
        let scope = authenticated_scope(issued);
        let service =
            AppPackageStager::with_blocking_capacity(ArtifactV2Workspace::new(storage.path()), 1)
                .unwrap();

        assert!(matches!(
            service
                .stage_from_directory(&scope, source.path(), issued + ChronoDuration::minutes(31),)
                .await,
            Err(AppPackageStagingError::Authentication(_))
        ));
        assert!(!storage.path().join("scopes").exists());

        let permit = Arc::clone(&service.blocking_slots)
            .try_acquire_owned()
            .unwrap();
        let mut waiting = Box::pin(service.stage_from_directory(&scope, source.path(), issued));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(60), &mut waiting)
                .await
                .is_err()
        );
        assert!(!storage.path().join("scopes").exists());
        drop(waiting);
        drop(permit);
        service
            .stage_from_directory(&scope, source.path(), issued)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn warm_package_reads_share_admitted_bytes_and_detect_changed_content_and_index() {
        let storage = canonical_tempdir();
        let source = source_package();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let scope = authenticated_scope(now);
        let staged = service
            .stage_from_directory(&scope, source.path(), now)
            .await
            .unwrap();
        let digest = staged.storage_digest().clone();
        let first = service
            .load_staged_package(&scope, digest.clone(), now)
            .await
            .unwrap();
        let second = service
            .load_staged_package(&scope, digest.clone(), now)
            .await
            .unwrap();
        assert_eq!(service.read_stats().admissions, 1);
        assert_eq!(service.read_stats().hits, 1);
        assert_eq!(first.candidate(), second.candidate());
        assert!(
            std::ptr::eq(
                first.candidate().members()[0].bytes().as_ptr(),
                second.candidate().members()[0].bytes().as_ptr()
            ),
            "verified immutable member bytes should be shared, not reallocated per load"
        );
        let root = storage
            .path()
            .join("scopes/anonymous/default/apps/packages")
            .join(digest_directory_name(&digest).unwrap());
        let workflow = root.join("bundle/workflows/build.md");
        let before = fs::metadata(&workflow).unwrap();
        // Same-length content and a restored mtime must still invalidate via
        // ctime; changed bytes under the old digest cannot use cached admission.
        fs::write(&workflow, b"Break a plan.").unwrap();
        File::options()
            .write(true)
            .open(&workflow)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        assert!(service
            .load_staged_package(&scope, digest.clone(), now)
            .await
            .is_err());
        fs::write(&workflow, b"Build a plan.").unwrap();
        service
            .load_staged_package(&scope, digest.clone(), now)
            .await
            .unwrap();
        fs::write(root.join("package-index.json"), b"corrupt").unwrap();
        assert!(matches!(
            service.load_staged_package(&scope, digest, now).await,
            Err(AppPackageStagingError::ContentConflict)
        ));
    }

    #[tokio::test]
    async fn concurrent_cold_package_reads_admit_once_without_worker_rejection() {
        let storage = canonical_tempdir();
        let source = source_package();
        let service =
            AppPackageStager::with_blocking_capacity(ArtifactV2Workspace::new(storage.path()), 1)
                .unwrap();
        let now = now();
        let scope = authenticated_scope(now);
        let staged = service
            .stage_from_directory(&scope, source.path(), now)
            .await
            .unwrap();
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let (service, scope, digest) = (
                service.clone(),
                scope.clone(),
                staged.storage_digest().clone(),
            );
            tasks.push(tokio::spawn(async move {
                service.load_staged_package(&scope, digest, now).await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        assert_eq!(service.read_stats().admissions, 1);
        assert_eq!(service.read_stats().hits, 11);
    }

    #[tokio::test]
    async fn destination_symlink_is_rejected_without_writing_through_it() {
        let storage = canonical_tempdir();
        let escaped = canonical_tempdir();
        let source = source_package();
        fs::create_dir(storage.path().join("scopes")).unwrap();
        symlink(escaped.path(), storage.path().join("scopes/anonymous")).unwrap();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let scope = authenticated_scope(now);

        assert!(service
            .stage_from_directory(&scope, source.path(), now)
            .await
            .is_err());
        assert_eq!(fs::read_dir(escaped.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn normalized_scope_path_collision_is_rejected_before_package_write() {
        let storage = canonical_tempdir();
        let source = source_package();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let first_scope = authenticated_scope_for("owner:a", "default", now);
        let colliding_scope = authenticated_scope_for("owner_a", "default", now);
        service
            .stage_from_directory(&first_scope, source.path(), now)
            .await
            .unwrap();

        assert!(matches!(
            service
                .stage_from_directory(&colliding_scope, source.path(), now)
                .await,
            Err(AppPackageStagingError::ScopeCollision)
        ));
        let binding = fs::read(
            storage
                .path()
                .join("scopes/owner_a/default/apps/scope-binding.json"),
        )
        .unwrap();
        assert!(String::from_utf8(binding).unwrap().contains("owner:a"));
    }

    #[tokio::test]
    async fn recovery_removes_only_stale_private_staging_directories() {
        let storage = canonical_tempdir();
        let source = source_package();
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let issued = now();
        let scope = authenticated_scope(issued);
        service
            .stage_from_directory(&scope, source.path(), issued)
            .await
            .unwrap();
        let packages = storage
            .path()
            .join("scopes/anonymous/default/apps/packages");
        let abandoned = packages.join(".staging-abandoned");
        fs::create_dir(&abandoned).unwrap();
        fs::write(abandoned.join("partial"), b"partial").unwrap();
        let ordinary = packages.join("ordinary-directory");
        fs::create_dir(&ordinary).unwrap();

        let recovery_time = Utc::now() + ChronoDuration::hours(2);
        let recovery_scope = authenticated_scope(recovery_time);
        assert_eq!(
            service
                .recover_stale_staging(&recovery_scope, recovery_time)
                .await
                .unwrap(),
            1
        );
        assert!(!abandoned.exists());
        assert!(ordinary.is_dir());
    }

    #[tokio::test]
    async fn depth_limit_rejects_iteratively_without_stack_growth() {
        let storage = canonical_tempdir();
        let source = canonical_tempdir();
        fs::write(source.path().join("SKILL.md"), valid_skill_document()).unwrap();
        let mut current = source.path().to_path_buf();
        for _ in 0..=MAX_DIRECTORY_DEPTH {
            current.push("d");
            fs::create_dir(&current).unwrap();
        }
        let service = AppPackageStager::new(ArtifactV2Workspace::new(storage.path()));
        let now = now();
        let scope = authenticated_scope(now);
        assert!(matches!(
            service
                .stage_from_directory(&scope, source.path(), now)
                .await,
            Err(AppPackageStagingError::UnsafeSource(_))
        ));
    }
}
