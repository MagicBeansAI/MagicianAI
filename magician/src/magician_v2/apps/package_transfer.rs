//! Bounded physical codec for package-only app transfer.
//!
//! The logical portability contract remains owned by [`super::portability`].
//! This module only carries that manifest and the exact immutable bundle bytes
//! in a deterministic ZIP. It has no fields for scope, installation, grants,
//! credentials, schedules, memory or app data.

use std::{
    collections::HashSet,
    io::{Cursor, Read, Write},
};

use thiserror::Error;

use super::{
    manifest::{
        AppBundleStagingAdmission, AppManifestError, AppPackageCandidate, AppPackageLimits,
    },
    models::{decode_app_contract, AppContractError, AppContractLimits, ValidateAppContract},
    portability::AppPackageArchiveManifest,
};
use crate::magician_v2::json_traversal::canonical_json_bytes;

pub const APP_PACKAGE_ARCHIVE_MEDIA_TYPE: &str = "application/vnd.app-platform.package+zip";
pub const APP_PACKAGE_ARCHIVE_MAX_BYTES: usize = 72 * 1_024 * 1_024;
const PACKAGE_MANIFEST_ENTRY: &str = "app-package.json";
const BUNDLE_ENTRY_PREFIX: &str = "bundle/";
const MAX_PACKAGE_MANIFEST_BYTES: usize = 1_048_576;

#[derive(Debug, Error)]
pub enum AppPackageTransferError {
    #[error("package archive exceeds its fixed byte limit")]
    ArchiveTooLarge,
    #[error("package archive contains too many entries")]
    TooManyEntries,
    #[error("package archive is missing its package manifest")]
    MissingManifest,
    #[error("package archive contains a duplicate entry `{0}`")]
    DuplicateEntry(String),
    #[error("package archive contains an unsupported entry `{0}`")]
    UnsupportedEntry(String),
    #[error("package archive member metadata is unsafe for `{0}`")]
    UnsafeMember(String),
    #[error("package archive member set does not match its logical manifest")]
    MemberSetMismatch,
    #[error("package archive digest does not match its admitted bundle")]
    DigestMismatch,
    #[error("package archive manifest is invalid: {0}")]
    InvalidContract(#[from] AppContractError),
    #[error("package archive bundle is invalid: {0}")]
    InvalidBundle(#[from] AppManifestError),
    #[error("package archive ZIP is invalid: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("package archive I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("package archive encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
}

/// An imported package rebuilt through the hostile-input bundle admission
/// boundary. Neither member bytes nor logical metadata are deserialized into a
/// registry publication or authority-bearing value.
#[derive(Debug)]
pub struct AdmittedAppPackageArchive {
    package: AppPackageArchiveManifest,
    candidate: AppPackageCandidate,
}

impl AdmittedAppPackageArchive {
    pub fn package(&self) -> &AppPackageArchiveManifest {
        &self.package
    }

    pub fn into_parts(self) -> (AppPackageArchiveManifest, AppPackageCandidate) {
        (self.package, self.candidate)
    }
}

pub fn encode_package_archive(
    package: &AppPackageArchiveManifest,
    candidate: &AppPackageCandidate,
) -> Result<Vec<u8>, AppPackageTransferError> {
    validate_package_candidate_binding(package, candidate)?;
    let manifest_value = serde_json::to_value(package)?;
    let manifest_bytes = canonical_json_bytes(&manifest_value)?;
    if manifest_bytes.len() > MAX_PACKAGE_MANIFEST_BYTES {
        return Err(AppPackageTransferError::ArchiveTooLarge);
    }

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o600);
        writer.start_file(PACKAGE_MANIFEST_ENTRY, options.clone())?;
        writer.write_all(&manifest_bytes)?;
        for member in candidate.members() {
            writer.start_file(
                format!("{BUNDLE_ENTRY_PREFIX}{}", member.path().as_str()),
                options.clone(),
            )?;
            writer.write_all(member.bytes())?;
        }
        writer.finish()?;
    }
    let bytes = cursor.into_inner();
    if bytes.len() > APP_PACKAGE_ARCHIVE_MAX_BYTES {
        return Err(AppPackageTransferError::ArchiveTooLarge);
    }
    Ok(bytes)
}

pub fn admit_package_archive(
    bytes: &[u8],
) -> Result<AdmittedAppPackageArchive, AppPackageTransferError> {
    if bytes.len() > APP_PACKAGE_ARCHIVE_MAX_BYTES {
        return Err(AppPackageTransferError::ArchiveTooLarge);
    }
    let package_limits = AppPackageLimits::default();
    let maximum_entries = package_limits.max_bundle_files().saturating_add(1);
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    if archive.len() == 0 || archive.len() > maximum_entries {
        return Err(AppPackageTransferError::TooManyEntries);
    }

    let mut names = HashSet::with_capacity(archive.len());
    let mut manifest_bytes = None;
    let mut declared_bundle_bytes = 0usize;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if std::str::from_utf8(file.name_raw()).is_err()
            || file.name().len()
                > BUNDLE_ENTRY_PREFIX
                    .len()
                    .saturating_add(package_limits.max_bundle_path_bytes())
                    .max(PACKAGE_MANIFEST_ENTRY.len())
        {
            return Err(AppPackageTransferError::UnsafeMember(
                "invalid or overlong ZIP member name".to_owned(),
            ));
        }
        let name = file.name().to_owned();
        if !names.insert(name.clone()) {
            return Err(AppPackageTransferError::DuplicateEntry(name));
        }
        validate_zip_regular_file(&file, &name)?;
        if name == PACKAGE_MANIFEST_ENTRY {
            if file.size() > u64::try_from(MAX_PACKAGE_MANIFEST_BYTES).unwrap_or(u64::MAX) {
                return Err(AppPackageTransferError::ArchiveTooLarge);
            }
            let mut admitted = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
            file.by_ref()
                .take(
                    u64::try_from(MAX_PACKAGE_MANIFEST_BYTES.saturating_add(1)).unwrap_or(u64::MAX),
                )
                .read_to_end(&mut admitted)?;
            if admitted.len() > MAX_PACKAGE_MANIFEST_BYTES {
                return Err(AppPackageTransferError::ArchiveTooLarge);
            }
            manifest_bytes = Some(admitted);
        } else if let Some(path) = name.strip_prefix(BUNDLE_ENTRY_PREFIX) {
            if path.is_empty()
                || file.size()
                    > u64::try_from(package_limits.max_bundle_file_bytes()).unwrap_or(u64::MAX)
            {
                return Err(AppPackageTransferError::UnsafeMember(name));
            }
            declared_bundle_bytes = declared_bundle_bytes
                .checked_add(
                    usize::try_from(file.size())
                        .map_err(|_| AppPackageTransferError::UnsafeMember(name.clone()))?,
                )
                .ok_or(AppPackageTransferError::ArchiveTooLarge)?;
            if declared_bundle_bytes > package_limits.max_bundle_bytes() {
                return Err(AppPackageTransferError::ArchiveTooLarge);
            }
        } else {
            return Err(AppPackageTransferError::UnsupportedEntry(name));
        }
    }

    let manifest_bytes = manifest_bytes.ok_or(AppPackageTransferError::MissingManifest)?;
    let package: AppPackageArchiveManifest =
        decode_app_contract(&manifest_bytes, &AppContractLimits::default())?;

    // Reopen from immutable request bytes so ZIP directory metadata inspected
    // above cannot be confused with the streamed member read below.
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let mut admission = AppBundleStagingAdmission::default();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let name = file.name().to_owned();
        if name == PACKAGE_MANIFEST_ENTRY {
            continue;
        }
        let path = name
            .strip_prefix(BUNDLE_ENTRY_PREFIX)
            .ok_or_else(|| AppPackageTransferError::UnsupportedEntry(name.clone()))?;
        admission.push_regular_reader(path, Some(file.size()), &mut file)?;
    }
    let candidate = admission.finish()?;
    validate_package_candidate_binding(&package, &candidate)?;
    Ok(AdmittedAppPackageArchive { package, candidate })
}

fn validate_zip_regular_file<R: Read>(
    file: &zip::read::ZipFile<'_, R>,
    name: &str,
) -> Result<(), AppPackageTransferError> {
    if file.is_dir() || name.ends_with('/') || name.contains('\\') || name.as_bytes().contains(&0) {
        return Err(AppPackageTransferError::UnsafeMember(name.to_owned()));
    }
    if let Some(mode) = file.unix_mode() {
        let file_type = mode & 0o170_000;
        if file_type != 0 && file_type != 0o100_000 {
            return Err(AppPackageTransferError::UnsafeMember(name.to_owned()));
        }
    }
    Ok(())
}

fn validate_package_candidate_binding(
    package: &AppPackageArchiveManifest,
    candidate: &AppPackageCandidate,
) -> Result<(), AppPackageTransferError> {
    package.validate_app_contract(&AppContractLimits::default())?;
    if package.package_content_digest != *candidate.bundle_digest()
        || package.manifest_digest != *candidate.manifest().manifest_digest()
    {
        return Err(AppPackageTransferError::DigestMismatch);
    }
    if package.members.len() != candidate.members().len()
        || package
            .members
            .iter()
            .zip(candidate.members())
            .any(|(portable, member)| {
                portable.path != *member.path()
                    || portable.content_digest != *member.content_digest()
                    || portable.byte_len != u64::try_from(member.bytes().len()).unwrap_or(u64::MAX)
            })
    {
        return Err(AppPackageTransferError::MemberSetMismatch);
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod tests {
    use super::*;
    use crate::magician_v2::apps::{
        manifest::{build_app_package_candidate, tests::valid_bundle},
        models::{AppDigest, AppReference},
        portability::{AppPortablePackageMember, APP_PORTABLE_ARCHIVE_VERSION},
    };

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn archive_fixture(candidate: &AppPackageCandidate) -> AppPackageArchiveManifest {
        let dependency_lock = super::super::package_lock::portable_test_lock(
            candidate.manifest().manifest_digest().clone(),
            candidate.bundle_digest().clone(),
        );
        let members = candidate
            .members()
            .iter()
            .map(|member| AppPortablePackageMember {
                path: member.path().clone(),
                content_digest: member.content_digest().clone(),
                byte_len: u64::try_from(member.bytes().len()).unwrap(),
            })
            .collect();
        let mut archive = AppPackageArchiveManifest {
            archive_version: APP_PORTABLE_ARCHIVE_VERSION,
            package_revision_ref: reference("package-revision:fixture"),
            package_id: reference("app:fixture"),
            publisher_identity: reference("publisher:fixture"),
            semantic_version: candidate.manifest().manifest().version.clone(),
            package_content_digest: candidate.bundle_digest().clone(),
            manifest_digest: candidate.manifest().manifest_digest().clone(),
            dependency_lock_digest: dependency_lock.lock_digest().clone(),
            dependency_lock: super::super::package_lock::AppPortablePackageLockClaim::from_trusted(
                &dependency_lock,
            ),
            members,
            advisory_verification_evidence: Vec::new(),
            logical_payload_digest: AppDigest::blake3(b"placeholder"),
        };
        archive.logical_payload_digest = archive.recompute_digest().unwrap();
        archive
    }

    pub fn archive_bytes_fixture() -> Vec<u8> {
        let candidate =
            build_app_package_candidate(valid_bundle(), &AppPackageLimits::default()).unwrap();
        let manifest = archive_fixture(&candidate);
        encode_package_archive(&manifest, &candidate).unwrap()
    }

    #[test]
    fn package_zip_round_trip_preserves_only_logical_manifest_and_bundle() {
        let candidate =
            build_app_package_candidate(valid_bundle(), &AppPackageLimits::default()).unwrap();
        let manifest = archive_fixture(&candidate);
        let bytes = archive_bytes_fixture();
        let admitted = admit_package_archive(&bytes).unwrap();
        assert_eq!(admitted.package(), &manifest);
        assert_eq!(
            admitted.candidate.bundle_digest(),
            candidate.bundle_digest()
        );
        assert_eq!(admitted.candidate.members(), candidate.members());

        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let names = (0..zip.len())
            .map(|index| zip.by_index(index).unwrap().name().to_owned())
            .collect::<Vec<_>>();
        assert!(names.iter().all(|name| {
            name == PACKAGE_MANIFEST_ENTRY || name.starts_with(BUNDLE_ENTRY_PREFIX)
        }));
    }

    #[test]
    fn unlisted_extra_file_is_rejected_before_staging() {
        let candidate =
            build_app_package_candidate(valid_bundle(), &AppPackageLimits::default()).unwrap();
        let manifest = archive_fixture(&candidate);
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            writer
                .start_file(PACKAGE_MANIFEST_ENTRY, options.clone())
                .unwrap();
            writer
                .write_all(&canonical_json_bytes(&serde_json::to_value(manifest).unwrap()).unwrap())
                .unwrap();
            writer.start_file("scope.json", options).unwrap();
            writer.write_all(br#"{"principal":"stolen"}"#).unwrap();
            writer.finish().unwrap();
        }
        assert!(matches!(
            admit_package_archive(cursor.get_ref()),
            Err(AppPackageTransferError::UnsupportedEntry(name)) if name == "scope.json"
        ));
    }
}
