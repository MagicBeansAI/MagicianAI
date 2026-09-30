//! Phase 8 deferred computed-capability catalog kernel.
//!
//! An enabled installation may expose only the exact capability revisions
//! named by its package lock and permitted by the current grant. Discovery
//! becomes empty on disable, quarantine, revoke, retain or purge. An
//! invented tool name cannot enter the catalog or bypass the grant. Dispatch
//! through the existing app-workflow USR pack path must present the exact
//! revision fence. A scope overlay projects those exact revisions into the
//! existing deferred/scoped USR catalog and disappears on hide. This module
//! does not add a second executor.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex},
};

use serde::Serialize;
use thiserror::Error;
use tool_runtime_core::manifest_parser::parse_skill_runtime_package;

use super::{
    lifecycle::AppInstallationStatus,
    manifest::AppDependencyKind,
    models::{AppDigest, AppInstallationId, AppReference, AppRevision},
    package_lock::{AppLockedDependencySource, AppPackageLock},
};

/// One hundred enabled installations may contribute computed capabilities.
/// Extra eligible installations fail closed instead of expanding the catalog.
pub const MAX_COMPUTED_CAPABILITY_INSTALLATIONS: usize = 100;
/// One hundred distinct USR tool names may appear in a scope overlay.
pub const MAX_COMPUTED_CAPABILITY_OVERLAY_ENTRIES: usize = 100;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppComputedCapabilityVisibility {
    Eligible,
    Hidden,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCataloguedCapability {
    pub dependency_ref: AppReference,
    pub semantic_version: String,
    pub content_digest: AppDigest,
    pub immutable_revision_ref: AppReference,
    pub revision: AppRevision,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppComputedCapabilityCatalog {
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub lock_digest: AppDigest,
    pub grant_revision: AppRevision,
    pub visibility: AppComputedCapabilityVisibility,
    pub capabilities: Vec<AppCataloguedCapability>,
    pub catalog_digest: AppDigest,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppComputedCapabilityCatalogError {
    #[error("computed-capability catalog names a stale package, lock or grant")]
    StaleRevision,
    #[error("computed-capability catalog is not eligible for this installation")]
    InstallationNotEligible,
    #[error("computed capability is not in the exact package lock")]
    InventedCapability,
    #[error("computed capability is locked but not granted")]
    CapabilityNotGranted,
    #[error("computed capability cannot be vendored into the catalog")]
    UnsupportedSource,
    #[error("computed-capability overlay exceeds the 100-app catalog bound")]
    CatalogBoundExceeded,
}

pub struct AppComputedCapabilityAdmission<'a> {
    pub installation_id: AppInstallationId,
    pub installation_status: AppInstallationStatus,
    pub grant_revoked: bool,
    pub package_revision_ref: &'a AppReference,
    pub live_package_revision_ref: &'a AppReference,
    pub lock_digest: &'a AppDigest,
    pub live_lock_digest: &'a AppDigest,
    pub grant_revision: AppRevision,
    pub live_grant_revision: AppRevision,
    pub permitted_tools: &'a BTreeSet<AppReference>,
}

pub fn compile_computed_capability_catalog(
    lock: &AppPackageLock,
    admission: AppComputedCapabilityAdmission<'_>,
) -> Result<AppComputedCapabilityCatalog, AppComputedCapabilityCatalogError> {
    if admission.package_revision_ref != admission.live_package_revision_ref
        || admission.lock_digest != admission.live_lock_digest
        || admission.grant_revision != admission.live_grant_revision
        || lock.lock_digest() != admission.live_lock_digest
    {
        return Err(AppComputedCapabilityCatalogError::StaleRevision);
    }

    let visibility = catalog_visibility(admission.installation_status, admission.grant_revoked);
    let capabilities = if visibility == AppComputedCapabilityVisibility::Eligible {
        locked_granted_capabilities(lock, admission.permitted_tools)?
    } else {
        Vec::new()
    };
    let catalog_digest = digest_catalog(
        &admission.installation_id,
        admission.live_package_revision_ref,
        admission.live_lock_digest,
        admission.live_grant_revision,
        visibility,
        &capabilities,
    );
    Ok(AppComputedCapabilityCatalog {
        installation_id: admission.installation_id,
        package_revision_ref: admission.live_package_revision_ref.clone(),
        lock_digest: admission.live_lock_digest.clone(),
        grant_revision: admission.live_grant_revision,
        visibility,
        capabilities,
        catalog_digest,
    })
}

/// Exact USR-facing dispatch ticket. It is serialization-only and cannot be
/// minted from a tool name or a transport claim.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppComputedCapabilityDispatchFence {
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    lock_digest: AppDigest,
    grant_revision: AppRevision,
    catalog_digest: AppDigest,
    capability: AppCataloguedCapability,
}

impl AppComputedCapabilityDispatchFence {
    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn package_revision_ref(&self) -> &AppReference {
        &self.package_revision_ref
    }

    pub fn lock_digest(&self) -> &AppDigest {
        &self.lock_digest
    }

    pub fn grant_revision(&self) -> AppRevision {
        self.grant_revision
    }

    pub fn catalog_digest(&self) -> &AppDigest {
        &self.catalog_digest
    }

    pub fn capability(&self) -> &AppCataloguedCapability {
        &self.capability
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppComputedCapabilityDocument {
    pub dependency_ref: AppReference,
    pub description: String,
    pub skill_document: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppComputedCapabilityOverlayEntry {
    pub tool_name: String,
    pub dependency_ref: AppReference,
    pub installation_id: AppInstallationId,
    pub semantic_version: String,
    pub content_digest: AppDigest,
    pub revision: AppRevision,
    pub description: String,
    pub skill_document: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppComputedCapabilityScopeOverlay {
    pub revision: String,
    pub entries: Vec<AppComputedCapabilityOverlayEntry>,
    pub overflow_omitted: bool,
}

impl AppComputedCapabilityScopeOverlay {
    pub fn empty() -> Self {
        Self {
            revision: AppDigest::blake3(b"empty-computed-capability-overlay")
                .as_str()
                .to_owned(),
            entries: Vec::new(),
            overflow_omitted: false,
        }
    }

    pub fn tool_names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.tool_name.as_str())
    }

    pub fn contains_tool(&self, tool_name: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.tool_name == tool_name)
    }
}

#[derive(Debug, Default)]
pub struct AppComputedCapabilityOverlayCache {
    views: Mutex<HashMap<(String, String), Arc<AppComputedCapabilityScopeOverlay>>>,
}

impl AppComputedCapabilityOverlayCache {
    pub fn get(&self, principal: &str, workspace: &str) -> Arc<AppComputedCapabilityScopeOverlay> {
        self.views
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(principal.to_owned(), workspace.to_owned()))
            .cloned()
            .unwrap_or_else(|| Arc::new(AppComputedCapabilityScopeOverlay::empty()))
    }

    pub fn replace(
        &self,
        principal: &str,
        workspace: &str,
        overlay: AppComputedCapabilityScopeOverlay,
    ) -> bool {
        let overlay = Arc::new(overlay);
        let mut views = self.views.lock().unwrap_or_else(|error| error.into_inner());
        let key = (principal.to_owned(), workspace.to_owned());
        let changed = views
            .get(&key)
            .map(|current| current.revision != overlay.revision)
            .unwrap_or(true);
        views.insert(key, overlay);
        changed
    }

    pub fn evict(&self, principal: &str, workspace: &str) {
        self.views
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(principal.to_owned(), workspace.to_owned()));
    }
}

pub fn discovery_tool_name(dependency_ref: &AppReference) -> Option<&str> {
    dependency_ref.as_str().strip_prefix("capability:")
}

pub fn locked_capability_is_usr_executable(skill_document: &[u8]) -> bool {
    let Ok(source) = std::str::from_utf8(skill_document) else {
        return false;
    };
    matches!(parse_skill_runtime_package(source), Ok(Some(_)))
}

pub fn compile_scope_computed_capability_overlay(
    catalogs: &[(
        AppComputedCapabilityCatalog,
        Vec<AppComputedCapabilityDocument>,
    )],
) -> AppComputedCapabilityScopeOverlay {
    let mut eligible = catalogs
        .iter()
        .filter(|(catalog, _)| catalog.visibility == AppComputedCapabilityVisibility::Eligible)
        .collect::<Vec<_>>();
    eligible.sort_by(|(left, _), (right, _)| {
        left.installation_id
            .as_str()
            .cmp(right.installation_id.as_str())
    });
    let overflow_installations = eligible.len() > MAX_COMPUTED_CAPABILITY_INSTALLATIONS;
    if overflow_installations {
        eligible.truncate(MAX_COMPUTED_CAPABILITY_INSTALLATIONS);
    }
    let mut claimed: BTreeMap<String, Option<AppComputedCapabilityOverlayEntry>> = BTreeMap::new();
    for (catalog, documents) in eligible {
        for capability in &catalog.capabilities {
            let Some(tool_name) = discovery_tool_name(&capability.dependency_ref) else {
                continue;
            };
            if claimed.contains_key(tool_name) {
                claimed.insert(tool_name.to_owned(), None);
                continue;
            }
            let document = documents
                .iter()
                .find(|document| document.dependency_ref == capability.dependency_ref);
            let Some(document) = document else {
                continue;
            };
            if AppDigest::blake3(&document.skill_document) != capability.content_digest
                || !locked_capability_is_usr_executable(&document.skill_document)
            {
                continue;
            }
            claimed.insert(
                tool_name.to_owned(),
                Some(AppComputedCapabilityOverlayEntry {
                    tool_name: tool_name.to_owned(),
                    dependency_ref: capability.dependency_ref.clone(),
                    installation_id: catalog.installation_id.clone(),
                    semantic_version: capability.semantic_version.clone(),
                    content_digest: capability.content_digest.clone(),
                    revision: capability.revision,
                    description: document.description.clone(),
                    skill_document: document.skill_document.clone(),
                }),
            );
        }
    }
    let mut entries = claimed.into_values().flatten().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.tool_name.cmp(&right.tool_name));
    let overflow_entries = entries.len() > MAX_COMPUTED_CAPABILITY_OVERLAY_ENTRIES;
    if overflow_entries {
        entries.truncate(MAX_COMPUTED_CAPABILITY_OVERLAY_ENTRIES);
    }
    let overflow_omitted = overflow_installations || overflow_entries;
    let mut material = String::from("computed-capability-overlay");
    if overflow_omitted {
        material.push_str("|overflow");
    }
    for entry in &entries {
        material.push('|');
        material.push_str(entry.installation_id.as_str());
        material.push('@');
        material.push_str(entry.dependency_ref.as_str());
        material.push('#');
        material.push_str(entry.content_digest.as_str());
    }
    AppComputedCapabilityScopeOverlay {
        revision: AppDigest::blake3(material.as_bytes()).as_str().to_owned(),
        entries,
        overflow_omitted,
    }
}

/// Chat and personal-agent discovery may list overlay names. Dispatch through
/// those surfaces must still present an overlay entry. Invented names fail
/// closed. This is the membership half of the same fence workflows apply
/// through [`authorize_computed_capability_dispatch`].
pub fn authorize_discovered_computed_capability<'a>(
    overlay: &'a AppComputedCapabilityScopeOverlay,
    tool_name: &str,
) -> Result<&'a AppComputedCapabilityOverlayEntry, AppComputedCapabilityCatalogError> {
    overlay
        .entries
        .iter()
        .find(|entry| entry.tool_name == tool_name)
        .ok_or(AppComputedCapabilityCatalogError::InventedCapability)
}

/// Usage may reorder names already in the overlay. It cannot add an invented
/// name, restore an omitted overflow name, or drop a locked granted tool.
pub fn rank_computed_capability_names(
    overlay: &AppComputedCapabilityScopeOverlay,
    scores: &BTreeMap<String, u64>,
) -> Vec<String> {
    let mut names = overlay
        .entries
        .iter()
        .map(|entry| entry.tool_name.clone())
        .collect::<Vec<_>>();
    names.sort_by(|left, right| {
        scores
            .get(right)
            .copied()
            .unwrap_or(0)
            .cmp(&scores.get(left).copied().unwrap_or(0))
            .then_with(|| left.cmp(right))
    });
    names
}

pub fn authorize_computed_capability_dispatch(
    lock: &AppPackageLock,
    admission: AppComputedCapabilityAdmission<'_>,
    requested: &AppReference,
) -> Result<AppComputedCapabilityDispatchFence, AppComputedCapabilityCatalogError> {
    let permitted_tools = admission.permitted_tools;
    let catalog = compile_computed_capability_catalog(lock, admission)?;
    let capability = admit_computed_capability(&catalog, requested, lock, permitted_tools)?.clone();
    Ok(AppComputedCapabilityDispatchFence {
        installation_id: catalog.installation_id,
        package_revision_ref: catalog.package_revision_ref,
        lock_digest: catalog.lock_digest,
        grant_revision: catalog.grant_revision,
        catalog_digest: catalog.catalog_digest,
        capability,
    })
}

pub fn admit_computed_capability<'a>(
    catalog: &'a AppComputedCapabilityCatalog,
    requested: &AppReference,
    lock: &AppPackageLock,
    permitted_tools: &BTreeSet<AppReference>,
) -> Result<&'a AppCataloguedCapability, AppComputedCapabilityCatalogError> {
    if catalog.visibility != AppComputedCapabilityVisibility::Eligible {
        return Err(AppComputedCapabilityCatalogError::InstallationNotEligible);
    }
    if !lock.dependencies().iter().any(|dependency| {
        dependency.kind() == AppDependencyKind::Capability
            && dependency.dependency_ref() == requested
    }) {
        return Err(AppComputedCapabilityCatalogError::InventedCapability);
    }
    if !permitted_tools.contains(requested) {
        return Err(AppComputedCapabilityCatalogError::CapabilityNotGranted);
    }
    catalog
        .capabilities
        .iter()
        .find(|capability| &capability.dependency_ref == requested)
        .ok_or(AppComputedCapabilityCatalogError::CapabilityNotGranted)
}

fn catalog_visibility(
    status: AppInstallationStatus,
    grant_revoked: bool,
) -> AppComputedCapabilityVisibility {
    if grant_revoked || status != AppInstallationStatus::Enabled {
        AppComputedCapabilityVisibility::Hidden
    } else {
        AppComputedCapabilityVisibility::Eligible
    }
}

fn locked_granted_capabilities(
    lock: &AppPackageLock,
    permitted_tools: &BTreeSet<AppReference>,
) -> Result<Vec<AppCataloguedCapability>, AppComputedCapabilityCatalogError> {
    let mut capabilities = Vec::new();
    for dependency in lock.dependencies() {
        if dependency.kind() != AppDependencyKind::Capability {
            continue;
        }
        if !permitted_tools.contains(dependency.dependency_ref()) {
            continue;
        }
        let AppLockedDependencySource::RegistryRevision {
            immutable_revision_ref,
            revision,
        } = dependency.source()
        else {
            return Err(AppComputedCapabilityCatalogError::UnsupportedSource);
        };
        capabilities.push(AppCataloguedCapability {
            dependency_ref: dependency.dependency_ref().clone(),
            semantic_version: dependency.semantic_version().to_owned(),
            content_digest: dependency.content_digest().clone(),
            immutable_revision_ref: immutable_revision_ref.clone(),
            revision: *revision,
        });
    }
    capabilities.sort_by(|left, right| {
        left.dependency_ref
            .as_str()
            .cmp(right.dependency_ref.as_str())
    });
    Ok(capabilities)
}

fn digest_catalog(
    installation_id: &AppInstallationId,
    package_revision_ref: &AppReference,
    lock_digest: &AppDigest,
    grant_revision: AppRevision,
    visibility: AppComputedCapabilityVisibility,
    capabilities: &[AppCataloguedCapability],
) -> AppDigest {
    let mut material = format!(
        "{}|{}|{}|{}|{}",
        installation_id.as_str(),
        package_revision_ref.as_str(),
        lock_digest.as_str(),
        grant_revision.get(),
        match visibility {
            AppComputedCapabilityVisibility::Eligible => "eligible",
            AppComputedCapabilityVisibility::Hidden => "hidden",
        }
    );
    for capability in capabilities {
        material.push('|');
        material.push_str(capability.dependency_ref.as_str());
        material.push('@');
        material.push_str(capability.semantic_version.as_str());
        material.push('#');
        material.push_str(capability.immutable_revision_ref.as_str());
    }
    AppDigest::blake3(material.as_bytes())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::apps::{
        manifest::{build_app_package_candidate, tests::valid_bundle, AppPackageLimits},
        package_lock::{lock_app_package_dependencies, AppVerifiedRegistryDependency},
    };

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn lock() -> AppPackageLock {
        lock_app_package_dependencies(
            &build_app_package_candidate(valid_bundle(), &AppPackageLimits::default()).unwrap(),
            vec![
                AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                    AppDependencyKind::Contract,
                    reference("contract:magician_contract"),
                    "1.2.0".to_owned(),
                    reference("contract-revision:42"),
                    AppRevision::new(42).unwrap(),
                    b"contract-v1.2.0",
                )
                .unwrap(),
                AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                    AppDependencyKind::Capability,
                    reference("capability:content_search"),
                    "1.4.3".to_owned(),
                    reference("capability-revision:99"),
                    AppRevision::new(99).unwrap(),
                    capability_skill_document(),
                )
                .unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .unwrap()
    }

    fn admission<'a>(
        package: &'a AppReference,
        lock_digest: &'a AppDigest,
        status: AppInstallationStatus,
        grant_revoked: bool,
        permitted: &'a BTreeSet<AppReference>,
    ) -> AppComputedCapabilityAdmission<'a> {
        AppComputedCapabilityAdmission {
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            installation_status: status,
            grant_revoked,
            package_revision_ref: package,
            live_package_revision_ref: package,
            lock_digest,
            live_lock_digest: lock_digest,
            grant_revision: AppRevision::new(1).unwrap(),
            live_grant_revision: AppRevision::new(1).unwrap(),
            permitted_tools: permitted,
        }
    }

    #[test]
    fn enabled_grant_exposes_only_locked_permitted_capabilities() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let catalog = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
        )
        .expect("catalog");
        assert_eq!(
            catalog.visibility,
            AppComputedCapabilityVisibility::Eligible
        );
        assert_eq!(catalog.capabilities.len(), 1);
        assert_eq!(
            catalog.capabilities[0].dependency_ref.as_str(),
            "capability:content_search"
        );
        assert_eq!(catalog.capabilities[0].revision.get(), 99);
        admit_computed_capability(
            &catalog,
            &reference("capability:content_search"),
            &package_lock,
            &permitted,
        )
        .expect("admitted");
    }

    #[test]
    fn disable_quarantine_revoke_and_purge_hide_the_catalog() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        for (status, revoked) in [
            (AppInstallationStatus::Disabled, false),
            (AppInstallationStatus::Quarantined, false),
            (AppInstallationStatus::UninstalledRetained, false),
            (AppInstallationStatus::Purged, false),
            (AppInstallationStatus::Enabled, true),
        ] {
            let catalog = compile_computed_capability_catalog(
                &package_lock,
                admission(&package, &digest, status, revoked, &permitted),
            )
            .expect("hidden");
            assert_eq!(catalog.visibility, AppComputedCapabilityVisibility::Hidden);
            assert!(catalog.capabilities.is_empty());
            assert_eq!(
                admit_computed_capability(
                    &catalog,
                    &reference("capability:content_search"),
                    &package_lock,
                    &permitted,
                ),
                Err(AppComputedCapabilityCatalogError::InstallationNotEligible)
            );
        }
    }

    #[test]
    fn invented_names_and_ungranted_locked_names_fail_closed() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let empty = BTreeSet::new();
        let catalog = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &empty,
            ),
        )
        .expect("empty grant");
        assert!(catalog.capabilities.is_empty());
        assert_eq!(
            admit_computed_capability(
                &catalog,
                &reference("capability:invented_tool"),
                &package_lock,
                &empty,
            ),
            Err(AppComputedCapabilityCatalogError::InventedCapability)
        );
        assert_eq!(
            admit_computed_capability(
                &catalog,
                &reference("capability:content_search"),
                &package_lock,
                &empty,
            ),
            Err(AppComputedCapabilityCatalogError::CapabilityNotGranted)
        );
    }

    #[test]
    fn stale_lock_or_package_fails_closed() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let other = AppDigest::blake3(b"other-lock");
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let error = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &other,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
        )
        .expect_err("stale");
        assert_eq!(error, AppComputedCapabilityCatalogError::StaleRevision);
        assert_ne!(other, digest);
    }

    #[test]
    fn dispatch_fence_binds_the_exact_locked_revision() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let fence = authorize_computed_capability_dispatch(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
            &reference("capability:content_search"),
        )
        .expect("fence");
        assert_eq!(
            fence.capability().immutable_revision_ref.as_str(),
            "capability-revision:99"
        );
        assert_eq!(fence.lock_digest(), package_lock.lock_digest());
        static_assertions::assert_not_impl_any!(
            AppComputedCapabilityDispatchFence: serde::de::DeserializeOwned
        );
    }

    #[test]
    fn dispatch_refuses_invented_and_hidden_names() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        assert_eq!(
            authorize_computed_capability_dispatch(
                &package_lock,
                admission(
                    &package,
                    &digest,
                    AppInstallationStatus::Enabled,
                    false,
                    &permitted,
                ),
                &reference("capability:invented_tool"),
            ),
            Err(AppComputedCapabilityCatalogError::InventedCapability)
        );
        assert_eq!(
            authorize_computed_capability_dispatch(
                &package_lock,
                admission(
                    &package,
                    &digest,
                    AppInstallationStatus::Disabled,
                    false,
                    &permitted,
                ),
                &reference("capability:content_search"),
            ),
            Err(AppComputedCapabilityCatalogError::InstallationNotEligible)
        );
    }

    fn capability_skill_document() -> &'static [u8] {
        br#"---
name: content_search
version: 1.4.3
description: Search
metadata:
  magician:
    skill_type: tool
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires: {bins: [content-search]}
      runtime:
        protocol: cli
        command_prefix: []
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        search:
          description: Search.
          fixed_args: [search]
---
# Body
"#
    }

    fn documents() -> Vec<AppComputedCapabilityDocument> {
        vec![AppComputedCapabilityDocument {
            dependency_ref: reference("capability:content_search"),
            description: "Search".to_owned(),
            skill_document: capability_skill_document().to_vec(),
        }]
    }

    #[test]
    fn scope_overlay_lists_only_eligible_locked_capabilities() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let enabled = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
        )
        .unwrap();
        let hidden = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Disabled,
                false,
                &permitted,
            ),
        )
        .unwrap();
        let visible = compile_scope_computed_capability_overlay(&[(enabled, documents())]);
        assert!(visible.contains_tool("content_search"));
        assert!(!visible.contains_tool("invented_tool"));
        let gone = compile_scope_computed_capability_overlay(&[(hidden, documents())]);
        assert!(!gone.contains_tool("content_search"));
        assert_ne!(visible.revision, gone.revision);
    }

    #[test]
    fn colliding_capability_names_are_omitted() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let first = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
        )
        .unwrap();
        let mut second = first.clone();
        second.installation_id = AppInstallationId::parse("install_2").unwrap();
        let overlay = compile_scope_computed_capability_overlay(&[
            (first, documents()),
            (second, documents()),
        ]);
        assert!(!overlay.contains_tool("content_search"));
    }

    #[test]
    fn non_executable_locked_documents_never_enter_discovery() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let catalog = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
        )
        .unwrap();
        let overlay = compile_scope_computed_capability_overlay(&[(
            catalog,
            vec![AppComputedCapabilityDocument {
                dependency_ref: reference("capability:content_search"),
                description: "Search".to_owned(),
                skill_document: capability_skill_document().to_vec(),
            }],
        )]);
        assert!(overlay.contains_tool("content_search"));
        assert!(locked_capability_is_usr_executable(
            capability_skill_document()
        ));
        assert!(!locked_capability_is_usr_executable(b"not a runtime skill"));
    }

    #[test]
    fn discovered_dispatch_uses_overlay_membership_as_the_fence() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let catalog = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
        )
        .unwrap();
        let overlay = compile_scope_computed_capability_overlay(&[(catalog, documents())]);
        assert_eq!(
            authorize_discovered_computed_capability(&overlay, "content_search")
                .expect("admitted")
                .dependency_ref
                .as_str(),
            "capability:content_search"
        );
        assert_eq!(
            authorize_discovered_computed_capability(&overlay, "invented_tool"),
            Err(AppComputedCapabilityCatalogError::InventedCapability)
        );
        let hidden = compile_scope_computed_capability_overlay(&[]);
        assert_eq!(
            authorize_discovered_computed_capability(&hidden, "content_search"),
            Err(AppComputedCapabilityCatalogError::InventedCapability)
        );
    }

    #[test]
    fn ranking_cannot_add_drop_or_restore_authority() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let catalog = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
        )
        .unwrap();
        let overlay = compile_scope_computed_capability_overlay(&[(catalog, documents())]);
        let mut scores = BTreeMap::new();
        scores.insert("invented_tool".to_owned(), 10_000);
        scores.insert("content_search".to_owned(), 0);
        let ranked = rank_computed_capability_names(&overlay, &scores);
        assert_eq!(ranked, vec!["content_search".to_owned()]);
        assert!(!ranked.contains(&"invented_tool".to_owned()));
        let authority = overlay
            .entries
            .iter()
            .map(|entry| entry.tool_name.as_str())
            .collect::<BTreeSet<_>>();
        let ranked_set = ranked.iter().map(String::as_str).collect::<BTreeSet<_>>();
        assert_eq!(authority, ranked_set);
    }

    #[test]
    fn more_than_one_hundred_installations_cannot_expand_the_overlay() {
        let package_lock = lock();
        let package = reference("package-revision:reading-list");
        let digest = package_lock.lock_digest().clone();
        let permitted = BTreeSet::from([reference("capability:content_search")]);
        let catalog = compile_computed_capability_catalog(
            &package_lock,
            admission(
                &package,
                &digest,
                AppInstallationStatus::Enabled,
                false,
                &permitted,
            ),
        )
        .unwrap();
        let mut catalogs = Vec::new();
        for index in 0..=MAX_COMPUTED_CAPABILITY_INSTALLATIONS {
            let mut next = catalog.clone();
            next.installation_id =
                AppInstallationId::parse(&format!("install_{index:03}")).unwrap();
            let tool = reference(&format!("capability:tool_{index:03}"));
            next.capabilities[0].dependency_ref = tool.clone();
            let mut docs = documents();
            docs[0].dependency_ref = tool;
            catalogs.push((next, docs));
        }
        let overlay = compile_scope_computed_capability_overlay(&catalogs);
        assert!(overlay.overflow_omitted);
        assert!(overlay.entries.len() <= MAX_COMPUTED_CAPABILITY_OVERLAY_ENTRIES);
        assert!(!overlay.contains_tool("invented_tool"));
        let ranked = rank_computed_capability_names(&overlay, &BTreeMap::new());
        assert_eq!(ranked.len(), overlay.entries.len());
    }
}
