//! Snapshot existing typed skills into app-lock evidence.
//!
//! The author names a real tool. The engine looks it up in a reviewed
//! catalog, checks app-eligibility, and writes `capability:{name}` evidence.
//! Authors do not publish a second wrapper document.

use std::{collections::BTreeMap, fmt, path::PathBuf, sync::Arc};

use thiserror::Error;

use super::{
    manifest::{AppDependencyKind, AppPackageCandidate},
    models::{AppDigest, AppReference, AppRevision},
    os_jail::{AppOsJailArtifactStore, AppOsJailError, AppOsJailPhysicalOwnerAdapter},
    package_lock::{AppLockedPrimitiveBinding, AppPackageLockError, AppVerifiedRegistryDependency},
    primitive_catalog::{
        AppPrimitiveCatalogSnapshot, AppPrimitiveContainment, AppPrimitiveDispatchStatus,
        AppPrimitiveEligibilityStatus, AppPrimitiveExecutionClass, AppPrimitiveInvocationMode,
        AppPrimitiveKind, AppPrimitiveSchemaState, AppPrimitiveSnapshotBinding,
        AppPrimitiveSourceKind,
    },
    tool_eligibility::{
        assess_app_tool_eligibility, assess_compiled_pack_eligibility, AppToolAdmissionSource,
        AppToolEligibilityError,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppReviewedToolEntry {
    pub skill_document: Vec<u8>,
    pub immutable_revision_ref: AppReference,
    pub revision: AppRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppToolCatalogResolutionMode {
    ResolverSnapshot,
    ExternalImmutableEvidenceOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppReviewedToolCatalog {
    mode: AppToolCatalogResolutionMode,
    entries: BTreeMap<String, AppReviewedToolEntry>,
    agent_entries: BTreeMap<String, AppReviewedAgentToolEntry>,
    primitive_bindings: BTreeMap<String, AppLockedPrimitiveBinding>,
    artifact_reviews: BTreeMap<String, AppPhysicalArtifactReviewContext>,
    resolver_binding: Option<AppPrimitiveSnapshotBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppReviewedAgentToolEntry {
    agent_id: String,
    definition: Vec<u8>,
    semantic_version: String,
    immutable_revision_ref: AppReference,
    revision: AppRevision,
}

#[derive(Clone, PartialEq, Eq)]
struct AppPhysicalArtifactReviewContext {
    source_directory: PathBuf,
    store: Arc<AppOsJailArtifactStore>,
}

impl fmt::Debug for AppPhysicalArtifactReviewContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppPhysicalArtifactReviewContext")
            .field("configured", &true)
            .finish()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppToolCatalogError {
    #[error(transparent)]
    Eligibility(#[from] AppToolEligibilityError),
    #[error("reviewed catalog tool `{name}` does not match declared name `{declared}`")]
    NameMismatch { name: String, declared: String },
    #[error("compiled pack `{0}` is not in the embedded catalog")]
    MissingCompiledPack(String),
    #[error("tool `{0}` requires explicit external immutable resolution evidence")]
    ExternalEvidenceRequired(String),
    #[error("tool `{0}` is absent from the immutable resolver snapshot")]
    MissingResolverEntry(String),
    #[error("primitive resolver snapshot is unavailable")]
    ResolverUnavailable,
    #[error("primitive `{0}` has no retained immutable lock source")]
    MissingResolverSource(String),
    #[error("duplicate registry evidence for normalized tool `{0}`")]
    DuplicateRegistryEvidence(String),
    #[error("private physical artifact review failed for tool `{0}`")]
    PhysicalArtifactReview(String),
    #[error(transparent)]
    Lock(#[from] AppPackageLockError),
}

impl AppReviewedToolCatalog {
    pub fn resolver_snapshot() -> Self {
        Self {
            mode: AppToolCatalogResolutionMode::ResolverSnapshot,
            entries: BTreeMap::new(),
            agent_entries: BTreeMap::new(),
            primitive_bindings: BTreeMap::new(),
            artifact_reviews: BTreeMap::new(),
            resolver_binding: None,
        }
    }

    /// Explicit mode for portable/CLI paths whose exact registry evidence is
    /// supplied independently. Missing normal skills are errors; embedded
    /// platform packs still resolve from their process-owned immutable bytes.
    pub fn external_immutable_evidence_only() -> Self {
        Self {
            mode: AppToolCatalogResolutionMode::ExternalImmutableEvidenceOnly,
            entries: BTreeMap::new(),
            agent_entries: BTreeMap::new(),
            primitive_bindings: BTreeMap::new(),
            artifact_reviews: BTreeMap::new(),
            resolver_binding: None,
        }
    }

    /// Project reviewed skill evidence from the same immutable primitive
    /// snapshot used by authoring. Unambiguous, app-exposed, lockable tool and
    /// interactive descriptors enter the friendly-name map; private collisions
    /// remain available only by their qualified descriptor identities.
    /// Interactive presence here creates immutable lock evidence only. The
    /// installation owner separately admits the exact sealed Browser/macOS
    /// snapshot profiles and keeps every generic interactive descriptor inert.
    pub fn from_primitive_snapshot(
        snapshot: &AppPrimitiveCatalogSnapshot,
    ) -> Result<Self, AppToolCatalogError> {
        Self::from_primitive_snapshot_with_optional_artifact_store(snapshot, None)
    }

    pub(crate) fn from_primitive_snapshot_with_artifact_store(
        snapshot: &AppPrimitiveCatalogSnapshot,
        artifact_store: Arc<AppOsJailArtifactStore>,
    ) -> Result<Self, AppToolCatalogError> {
        Self::from_primitive_snapshot_with_optional_artifact_store(snapshot, Some(artifact_store))
    }

    fn from_primitive_snapshot_with_optional_artifact_store(
        snapshot: &AppPrimitiveCatalogSnapshot,
        artifact_store: Option<Arc<AppOsJailArtifactStore>>,
    ) -> Result<Self, AppToolCatalogError> {
        if !snapshot.complete() {
            return Err(AppToolCatalogError::ResolverUnavailable);
        }
        let mut catalog = Self {
            mode: AppToolCatalogResolutionMode::ResolverSnapshot,
            entries: BTreeMap::new(),
            agent_entries: BTreeMap::new(),
            primitive_bindings: BTreeMap::new(),
            artifact_reviews: BTreeMap::new(),
            resolver_binding: Some(snapshot.binding()),
        };
        for descriptor in snapshot.descriptors() {
            if !matches!(
                descriptor.kind(),
                AppPrimitiveKind::CompiledTool
                    | AppPrimitiveKind::ToolSkill
                    | AppPrimitiveKind::Agent
                    | AppPrimitiveKind::Interactive
            ) || !descriptor.exposure().apps()
                || descriptor.eligibility().status() != AppPrimitiveEligibilityStatus::Lockable
                || (descriptor.kind() == AppPrimitiveKind::Agent
                    && !is_sealed_agent_tool_descriptor(descriptor))
                || (normalized_tool_name(descriptor.name()) == "browser"
                    && !is_sealed_browser_snapshot_descriptor(descriptor))
            {
                continue;
            }
            // Discovery can include reviewed physical owners whose private
            // source was not supplied to this packaging invocation. Keep
            // those descriptors out of this resolver instead of failing an
            // unrelated package or retaining a binding with no immutable
            // reconstruction evidence.
            if descriptor.kind() != AppPrimitiveKind::CompiledTool
                && snapshot.source_material(descriptor.identity()).is_none()
            {
                continue;
            }
            let binding = AppLockedPrimitiveBinding::from_descriptor(descriptor)?;
            let artifact_review = if descriptor.kind() == AppPrimitiveKind::ToolSkill {
                artifact_store.as_ref().and_then(|store| {
                    snapshot
                        .source_directory(descriptor.identity())
                        .map(|source_directory| AppPhysicalArtifactReviewContext {
                            source_directory: source_directory.to_path_buf(),
                            store: Arc::clone(store),
                        })
                })
            } else {
                None
            };
            let identity_key = normalized_tool_name(descriptor.identity().as_str());
            catalog
                .primitive_bindings
                .insert(identity_key.clone(), binding.clone());
            if let Some(context) = artifact_review.as_ref() {
                catalog
                    .artifact_reviews
                    .insert(identity_key, context.clone());
            }
            if snapshot.alias_available(descriptor) {
                let alias_key = normalized_tool_name(descriptor.name());
                catalog
                    .primitive_bindings
                    .insert(alias_key.clone(), binding.clone());
                if let Some(context) = artifact_review {
                    catalog.artifact_reviews.insert(alias_key, context);
                }
            }
            if descriptor.kind() == AppPrimitiveKind::CompiledTool {
                continue;
            }
            let source = snapshot
                .source_material(descriptor.identity())
                .ok_or_else(|| {
                    AppToolCatalogError::MissingResolverSource(
                        descriptor.identity().as_str().to_owned(),
                    )
                })?;
            if descriptor.kind() == AppPrimitiveKind::Agent {
                let entry = AppReviewedAgentToolEntry {
                    agent_id: descriptor.name().to_owned(),
                    definition: source.to_vec(),
                    semantic_version: canonical_agent_semantic_version(
                        descriptor.semantic_version(),
                    )?,
                    immutable_revision_ref: descriptor.source().reference().clone(),
                    revision: AppRevision::new(1).map_err(|error| {
                        AppToolCatalogError::Lock(AppPackageLockError::InvalidManifest(
                            error.to_string(),
                        ))
                    })?,
                };
                catalog.insert_agent(descriptor.identity().as_str(), entry.clone());
                if snapshot.alias_available(descriptor) {
                    catalog.insert_agent(descriptor.name(), entry);
                }
                continue;
            }
            let entry = AppReviewedToolEntry {
                skill_document: source.to_vec(),
                immutable_revision_ref: descriptor.source().reference().clone(),
                revision: AppRevision::new(1).map_err(|error| {
                    AppToolCatalogError::Lock(AppPackageLockError::InvalidManifest(
                        error.to_string(),
                    ))
                })?,
            };
            catalog.insert(descriptor.identity().as_str(), entry.clone());
            if snapshot.alias_available(descriptor) {
                catalog.insert(descriptor.name(), entry.clone());
            }
            if descriptor.kind() == AppPrimitiveKind::Interactive {
                for action in binding.actions() {
                    let leaf_alias =
                        normalized_tool_name(&format!("{}__{}", descriptor.name(), action.name()));
                    if snapshot.resolve(&leaf_alias).is_ok()
                        || catalog.primitive_bindings.contains_key(&leaf_alias)
                        || catalog.entries.contains_key(&leaf_alias)
                    {
                        continue;
                    }
                    let selected = binding.select_actions(&[action.name().to_owned()])?;
                    catalog
                        .primitive_bindings
                        .insert(leaf_alias.clone(), selected);
                    catalog.insert(leaf_alias, entry.clone());
                }
            }
        }
        Ok(catalog)
    }

    /// Resolve a production catalog against a scoped snapshot. A catalog
    /// already carrying a resolver binding is itself an exact immutable
    /// projection and is not overlaid with mutable name-map entries.
    pub fn resolved_with_snapshot(
        &self,
        snapshot: &AppPrimitiveCatalogSnapshot,
    ) -> Result<Self, AppToolCatalogError> {
        if self.mode == AppToolCatalogResolutionMode::ExternalImmutableEvidenceOnly {
            return Ok(self.clone());
        }
        let snapshot_binding = snapshot.binding();
        if self.resolver_binding.as_ref() == Some(&snapshot_binding) {
            return Ok(self.clone());
        }
        // Never reuse immutable evidence projected for another owner or an
        // older snapshot. Rebuild from the supplied exact snapshot instead of
        // overlaying or trusting stale entries.
        Self::from_primitive_snapshot(snapshot)
    }

    pub(crate) fn resolved_with_snapshot_and_artifact_store(
        &self,
        snapshot: &AppPrimitiveCatalogSnapshot,
        artifact_store: Arc<AppOsJailArtifactStore>,
    ) -> Result<Self, AppToolCatalogError> {
        if self.mode == AppToolCatalogResolutionMode::ExternalImmutableEvidenceOnly {
            return Ok(self.clone());
        }
        Self::from_primitive_snapshot_with_artifact_store(snapshot, artifact_store)
    }

    pub fn mode(&self) -> AppToolCatalogResolutionMode {
        self.mode
    }

    pub fn resolver_binding(&self) -> Option<&AppPrimitiveSnapshotBinding> {
        self.resolver_binding.as_ref()
    }

    pub fn insert(&mut self, name: impl Into<String>, entry: AppReviewedToolEntry) {
        self.entries
            .insert(normalized_tool_name(&name.into()), entry);
    }

    fn insert_agent(&mut self, name: impl Into<String>, entry: AppReviewedAgentToolEntry) {
        self.agent_entries
            .insert(normalized_tool_name(&name.into()), entry);
    }

    pub fn get(&self, name: &str) -> Option<&AppReviewedToolEntry> {
        self.entries.get(&normalized_tool_name(name))
    }

    pub fn primitive_binding(&self, selector: &str) -> Option<&AppLockedPrimitiveBinding> {
        self.primitive_bindings.get(&normalized_tool_name(selector))
    }

    /// Snapshot every catalog-present declared tool. Missing names are left
    /// for registry evidence to fill; the later lock still fails closed.
    pub fn snapshot_declared_tools(
        &self,
        candidate: &AppPackageCandidate,
    ) -> Result<Vec<AppVerifiedRegistryDependency>, AppToolCatalogError> {
        let mut evidence = Vec::new();
        for tool in candidate.manifest().manifest().declared_tools()? {
            let selector = tool
                .primitive_ref
                .as_ref()
                .map(AppReference::as_str)
                .unwrap_or_else(|| tool.name.as_str());
            if let Some(snapshot) =
                self.snapshot_selected_tool(tool.name.as_str(), selector, &tool.actions)?
            {
                evidence.push(snapshot);
            }
        }
        Ok(evidence)
    }

    fn snapshot_selected_tool(
        &self,
        declared_name: &str,
        selector: &str,
        action_selectors: &[String],
    ) -> Result<Option<AppVerifiedRegistryDependency>, AppToolCatalogError> {
        let selected_binding = self
            .primitive_binding(selector)
            .map(|binding| binding.select_actions(action_selectors))
            .transpose()?;
        if let Some(agent) = self.agent_entries.get(&normalized_tool_name(selector)) {
            let binding = selected_binding.as_ref().ok_or_else(|| {
                AppToolCatalogError::MissingResolverEntry(declared_name.to_owned())
            })?;
            require_explicit_sealed_leaf_selector(
                declared_name,
                binding,
                action_selectors,
                "agent_as_tool",
            )?;
            return snapshot_reviewed_agent_tool(declared_name, agent, binding).map(Some);
        }
        let Some(entry) = self.get(selector) else {
            return Ok(None);
        };
        let binding = selected_binding;
        if binding
            .as_ref()
            .is_some_and(|binding| binding.interactive_owner().is_some())
        {
            if let Some(binding) = binding.as_ref() {
                let selected = binding
                    .actions()
                    .first()
                    .map(|action| action.name())
                    .ok_or_else(|| {
                        AppToolCatalogError::Lock(AppPackageLockError::InvalidManifest(
                            "sealed browser dependency has no exact action".to_owned(),
                        ))
                    })?;
                require_explicit_sealed_leaf_selector(
                    declared_name,
                    binding,
                    action_selectors,
                    selected,
                )?;
            }
        }
        let binding = match (
            binding,
            self.artifact_reviews.get(&normalized_tool_name(selector)),
        ) {
            (Some(binding), Some(context)) => Some(review_selected_physical_artifacts(
                declared_name,
                entry,
                binding,
                context,
            )?),
            (binding, _) => binding,
        };
        snapshot_reviewed_tool_with_binding(declared_name, entry, binding.as_ref(), &[]).map(Some)
    }
}

fn is_sealed_agent_tool_descriptor(
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> bool {
    if descriptor.kind() != AppPrimitiveKind::Agent
        || descriptor.source().kind() != AppPrimitiveSourceKind::ScopedAgent
        || descriptor.execution_class() != AppPrimitiveExecutionClass::AgentTask
        || descriptor.containment() != AppPrimitiveContainment::TaskOwner
        || descriptor.dispatch().status() != AppPrimitiveDispatchStatus::Ready
        || descriptor.allowed_modes()
            != &std::collections::BTreeSet::from([AppPrimitiveInvocationMode::AgentAsTool])
        || descriptor.actions().len() != 1
    {
        return false;
    }
    let action = &descriptor.actions()[0];
    action.name() == "agent_as_tool"
        && action.dispatch().status() == AppPrimitiveDispatchStatus::Ready
        && action.input_schema().state() == AppPrimitiveSchemaState::Inline
        && action.result_schema().state() == AppPrimitiveSchemaState::Inline
        && action.implementation_plan_digest().is_some()
        && action.physical_artifact_revision_ref().is_none()
        && action.physical_artifact_digest().is_none()
        && action
            .transport_result_byte_ceiling()
            .is_some_and(|ceiling| ceiling > 0 && ceiling <= 256 * 1024)
}

fn is_sealed_browser_snapshot_descriptor(
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
) -> bool {
    if descriptor.kind() != AppPrimitiveKind::Interactive
        || descriptor.name() != "browser"
        || descriptor.source().kind() != AppPrimitiveSourceKind::ScopedSkill
        || descriptor.execution_class() != AppPrimitiveExecutionClass::BrowserOwner
        || descriptor.containment() != AppPrimitiveContainment::BrowserSession
        || descriptor.dispatch().status() != AppPrimitiveDispatchStatus::Ready
        || descriptor.allowed_modes()
            != &std::collections::BTreeSet::from([
                AppPrimitiveInvocationMode::CallTool,
                AppPrimitiveInvocationMode::InteractiveAction,
            ])
        || descriptor.actions().len() != 4
    {
        return false;
    }
    let expected = std::collections::BTreeSet::from(["snapshot", "navigate", "scroll", "click"]);
    descriptor
        .actions()
        .iter()
        .map(|action| action.name())
        .collect::<std::collections::BTreeSet<_>>()
        == expected
        && descriptor.actions().iter().all(|action| {
            let input = super::browser_capability::browser_action_input_schema(action.name())
                .and_then(|schema| AppDigest::blake3_canonical_json(&schema).ok());
            let result = AppDigest::blake3_canonical_json(&if action.name() == "snapshot" {
                super::browser_capability::browser_observe_result_schema()
            } else {
                super::browser_capability::browser_action_result_schema()
            })
            .ok();
            let ceiling = if action.name() == "snapshot" {
                super::browser_capability::APP_BROWSER_OBSERVE_RESULT_CEILING
            } else {
                super::browser_capability::APP_BROWSER_ACTION_RESULT_CEILING
            };
            action.dispatch().status() == AppPrimitiveDispatchStatus::Ready
                && action.input_schema().digest() == input.as_ref()
                && action.result_schema().digest() == result.as_ref()
                && action.implementation_plan_digest()
                    == Some(&super::browser_capability::app_browser_runtime_implementation_digest())
                && action.physical_artifact_revision_ref().is_none()
                && action.physical_artifact_digest().is_none()
                && action.transport_result_byte_ceiling() == Some(ceiling)
        })
}

fn canonical_agent_semantic_version(version: Option<&str>) -> Result<String, AppToolCatalogError> {
    let raw = version.ok_or_else(|| {
        AppToolCatalogError::Lock(AppPackageLockError::InvalidManifest(
            "sealed agent tool has no semantic version".to_owned(),
        ))
    })?;
    let canonical = if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{raw}.0.0")
    } else {
        raw.to_owned()
    };
    semver::Version::parse(&canonical).map_err(|error| {
        AppToolCatalogError::Lock(AppPackageLockError::InvalidManifest(format!(
            "sealed agent tool has invalid semantic version: {error}"
        )))
    })?;
    Ok(canonical)
}

fn require_explicit_sealed_leaf_selector(
    declared_name: &str,
    selected_binding: &AppLockedPrimitiveBinding,
    selectors: &[String],
    expected_action: &str,
) -> Result<(), AppToolCatalogError> {
    if selectors.len() != 1
        || selected_binding.actions().len() != 1
        || selected_binding.actions()[0].name() != expected_action
    {
        return Err(AppToolCatalogError::Lock(
            AppPackageLockError::InvalidManifest(format!(
                "tool `{declared_name}` must explicitly select only `{expected_action}`"
            )),
        ));
    }
    Ok(())
}

fn snapshot_reviewed_agent_tool(
    declared_name: &str,
    entry: &AppReviewedAgentToolEntry,
    binding: &AppLockedPrimitiveBinding,
) -> Result<AppVerifiedRegistryDependency, AppToolCatalogError> {
    if normalized_tool_name(&entry.agent_id) != normalized_tool_name(declared_name)
        || binding.actions().len() != 1
        || binding.actions()[0].name() != "agent_as_tool"
        || binding.source_content_digest() != &AppDigest::blake3(&entry.definition)
    {
        return Err(AppToolCatalogError::NameMismatch {
            name: entry.agent_id.clone(),
            declared: declared_name.to_owned(),
        });
    }
    AppVerifiedRegistryDependency::from_trusted_primitive_binding_bytes(
        AppDependencyKind::Capability,
        AppReference::parse(format!("capability:{declared_name}"))?,
        entry.semantic_version.clone(),
        entry.immutable_revision_ref.clone(),
        entry.revision,
        &entry.definition,
        binding.clone(),
    )
    .map_err(AppToolCatalogError::from)
}

fn review_selected_physical_artifacts(
    declared_name: &str,
    entry: &AppReviewedToolEntry,
    binding: AppLockedPrimitiveBinding,
    context: &AppPhysicalArtifactReviewContext,
) -> Result<AppLockedPrimitiveBinding, AppToolCatalogError> {
    let adapter = match AppOsJailPhysicalOwnerAdapter::from_reviewed_source(&entry.skill_document) {
        Ok(adapter) => adapter,
        // CLI skills outside the first jail profile stay publishable without
        // physical evidence (non-dispatchable). MCP skills are not OS-jail
        // children; they keep this empty-artifact binding and dispatch through
        // the governed-MCP owner. Invalid retained bytes are a catalog
        // integrity failure, not an unsupported-profile fallback.
        Err(AppOsJailError::UnsupportedRuntime | AppOsJailError::AmbientAuthorityRequested) => {
            return Ok(binding);
        },
        Err(_) => {
            return Err(AppToolCatalogError::PhysicalArtifactReview(
                declared_name.to_owned(),
            ));
        },
    };
    adapter
        .validate_locked_actions(&binding)
        .map_err(|_| AppToolCatalogError::PhysicalArtifactReview(declared_name.to_owned()))?;
    let reviewed = adapter
        .review_private_artifacts(&binding, &context.source_directory, &context.store)
        .map_err(|_| AppToolCatalogError::PhysicalArtifactReview(declared_name.to_owned()))?
        .into_iter()
        .map(|(name, identity)| {
            (
                name,
                (identity.revision_ref().clone(), identity.digest().clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    binding
        .with_physical_artifacts(&reviewed)
        .map_err(AppToolCatalogError::from)
}

/// Produce trusted lock evidence from one reviewed catalog skill.
pub fn snapshot_reviewed_tool(
    declared_name: &str,
    entry: &AppReviewedToolEntry,
) -> Result<AppVerifiedRegistryDependency, AppToolCatalogError> {
    snapshot_reviewed_tool_with_binding(declared_name, entry, None, &[])
}

fn snapshot_reviewed_tool_with_binding(
    declared_name: &str,
    entry: &AppReviewedToolEntry,
    binding: Option<&AppLockedPrimitiveBinding>,
    action_selectors: &[String],
) -> Result<AppVerifiedRegistryDependency, AppToolCatalogError> {
    let eligible = assess_app_tool_eligibility(
        &entry.skill_document,
        AppToolAdmissionSource::ReviewedCatalog,
    )?;
    let exact_name =
        normalized_tool_name(eligible.name.as_str()) == normalized_tool_name(declared_name);
    let exact_interactive_leaf_alias = binding.is_some_and(|binding| {
        binding.interactive_owner().is_some()
            && binding.actions().len() == 1
            && normalized_tool_name(declared_name)
                == normalized_tool_name(&format!(
                    "{}__{}",
                    eligible.name,
                    binding.actions()[0].name()
                ))
    });
    if !exact_name && !exact_interactive_leaf_alias {
        return Err(AppToolCatalogError::NameMismatch {
            name: eligible.name.to_string(),
            declared: declared_name.to_owned(),
        });
    }
    let dependency_ref = AppReference::parse(format!("capability:{declared_name}"))?;
    match binding {
        Some(binding) => {
            let descriptor = binding.select_actions(action_selectors)?;
            AppVerifiedRegistryDependency::from_trusted_primitive_binding_bytes(
                AppDependencyKind::Capability,
                dependency_ref,
                eligible.semantic_version,
                entry.immutable_revision_ref.clone(),
                entry.revision,
                &entry.skill_document,
                descriptor,
            )
        },
        None => AppVerifiedRegistryDependency::from_trusted_registry_bytes(
            AppDependencyKind::Capability,
            dependency_ref,
            eligible.semantic_version,
            entry.immutable_revision_ref.clone(),
            entry.revision,
            &entry.skill_document,
        ),
    }
    .map_err(AppToolCatalogError::from)
}

/// Merge registry-minted evidence with catalog snapshots for any declared
/// tool the registry did not already resolve.
pub fn complete_declared_tool_evidence(
    candidate: &AppPackageCandidate,
    registry_evidence: Vec<AppVerifiedRegistryDependency>,
    catalog: &AppReviewedToolCatalog,
) -> Result<Vec<AppVerifiedRegistryDependency>, AppToolCatalogError> {
    let mut passthrough = Vec::new();
    let mut by_name = BTreeMap::new();
    for claim in registry_evidence {
        if let Some(name) = tool_name_from_dependency_ref(claim.dependency_ref().as_str()) {
            let key = normalized_tool_name(name);
            if by_name.insert(key.clone(), claim).is_some() {
                return Err(AppToolCatalogError::DuplicateRegistryEvidence(key));
            }
        } else {
            passthrough.push(claim);
        }
    }
    for tool in candidate.manifest().manifest().declared_tools()? {
        let key = normalized_tool_name(tool.name.as_str());
        let selector = tool
            .primitive_ref
            .as_ref()
            .map(AppReference::as_str)
            .unwrap_or_else(|| tool.name.as_str());
        let selected_source_binding = catalog
            .primitive_binding(selector)
            .map(|binding| binding.select_actions(&tool.actions))
            .transpose()?;
        let preserve_current_catalog_claim = by_name.get(&key).is_some_and(|claim| {
            claim
                .primitive_binding()
                .zip(selected_source_binding.as_ref())
                .is_some_and(|(claim, selected)| {
                    claim.matches_source_binding(selected).unwrap_or(false)
                        && (!catalog
                            .artifact_reviews
                            .contains_key(&normalized_tool_name(selector))
                            || claim.actions().iter().all(|action| {
                                action.physical_artifact_revision_ref().is_some()
                                    && action.physical_artifact_digest().is_some()
                            }))
                })
        });
        if tool.primitive_ref.is_none()
            && by_name.contains_key(&key)
            && catalog.primitive_binding(tool.name.as_str()).is_none()
        {
            if !tool.actions.is_empty() {
                let claim = by_name.remove(&key).ok_or_else(|| {
                    AppToolCatalogError::MissingResolverEntry(tool.name.to_string())
                })?;
                // A claim carried over from a lock written before primitive
                // bindings existed has no per-action evidence to narrow: action
                // grants live *inside* the binding, so a binding-less claim
                // holds no action authority that a selector could over-grant.
                // Selecting on it is a genuine no-op, and refusing instead
                // makes the previous binary's own immutable lock unpublishable
                // — which is exactly the transition a version bump has to be
                // able to cross. The next admission re-locks it with today's
                // bindings, and `select_primitive_actions` stays strict for
                // every claim that does carry evidence.
                let claim = if claim.primitive_binding().is_some() {
                    claim.select_primitive_actions(&tool.actions)?
                } else {
                    claim
                };
                by_name.insert(key, claim);
            }
            continue;
        }
        if !preserve_current_catalog_claim
            && (tool.primitive_ref.is_some()
                || catalog.primitive_binding(tool.name.as_str()).is_some())
        {
            // Exact selection and current scoped-catalog evidence replace a
            // same-named registry claim; mutable name evidence cannot retain a
            // weaker binding when an exact descriptor is available.
            by_name.remove(&key);
        }
        // Embedded platform names are reserved. A same-named private catalog
        // skill remains catalogued by its qualified descriptor identity, but
        // cannot replace the established platform dependency selected by the
        // manifest's friendly name.
        if tool.primitive_ref.is_none()
            && crate::magician_v2::execution::embedded_compiled_pack_yaml(tool.name.as_str())
                .is_some()
        {
            by_name.insert(
                key,
                snapshot_compiled_pack_with_binding(
                    tool.name.as_str(),
                    catalog.primitive_binding(tool.name.as_str()),
                    &tool.actions,
                )?,
            );
            continue;
        }
        if preserve_current_catalog_claim {
            continue;
        }
        if let Some(snapshot) =
            catalog.snapshot_selected_tool(tool.name.as_str(), selector, &tool.actions)?
        {
            by_name.insert(key, snapshot);
            continue;
        }
        return Err(match catalog.mode() {
            AppToolCatalogResolutionMode::ExternalImmutableEvidenceOnly => {
                AppToolCatalogError::ExternalEvidenceRequired(tool.name.to_string())
            },
            AppToolCatalogResolutionMode::ResolverSnapshot => {
                AppToolCatalogError::MissingResolverEntry(tool.name.to_string())
            },
        });
    }
    passthrough.extend(by_name.into_values());
    Ok(passthrough)
}

/// Snapshot one embedded compiled pack into internal `capability:{name}`
/// lock evidence. The pinned bytes are the pack YAML, not a wrapper skill.
pub fn snapshot_compiled_pack(
    declared_name: &str,
) -> Result<AppVerifiedRegistryDependency, AppToolCatalogError> {
    snapshot_compiled_pack_with_binding(declared_name, None, &[])
}

fn snapshot_compiled_pack_with_binding(
    declared_name: &str,
    binding: Option<&AppLockedPrimitiveBinding>,
    action_selectors: &[String],
) -> Result<AppVerifiedRegistryDependency, AppToolCatalogError> {
    let yaml = crate::magician_v2::execution::embedded_compiled_pack_yaml(declared_name)
        .ok_or_else(|| AppToolCatalogError::MissingCompiledPack(declared_name.to_owned()))?;
    let eligible = assess_compiled_pack_eligibility(yaml.as_bytes())?;
    if normalized_tool_name(eligible.name.as_str()) != normalized_tool_name(declared_name) {
        return Err(AppToolCatalogError::NameMismatch {
            name: eligible.name.to_string(),
            declared: declared_name.to_owned(),
        });
    }
    let dependency_ref = AppReference::parse(format!("capability:{}", eligible.name))?;
    let digest_suffix = eligible
        .content_digest
        .as_str()
        .trim_start_matches("blake3:");
    let short_digest = digest_suffix.get(..12).unwrap_or(digest_suffix);
    let immutable_revision_ref = AppReference::parse(format!(
        "compiled-revision:{}-{short_digest}",
        eligible.name
    ))?;
    let revision = AppRevision::new(1).map_err(|error| {
        AppToolCatalogError::Lock(AppPackageLockError::InvalidManifest(error.to_string()))
    })?;
    match binding {
        Some(binding) => AppVerifiedRegistryDependency::from_trusted_primitive_binding_bytes(
            AppDependencyKind::Capability,
            dependency_ref,
            eligible.semantic_version,
            immutable_revision_ref,
            revision,
            yaml.as_bytes(),
            binding.select_actions(action_selectors)?,
        ),
        None => AppVerifiedRegistryDependency::from_trusted_registry_bytes(
            AppDependencyKind::Capability,
            dependency_ref,
            eligible.semantic_version,
            immutable_revision_ref,
            revision,
            yaml.as_bytes(),
        ),
    }
    .map_err(AppToolCatalogError::from)
}

pub fn tool_name_from_dependency_ref(dependency_ref: &str) -> Option<&str> {
    dependency_ref.strip_prefix("capability:")
}

pub fn canonical_app_tool_ref(raw: &str) -> Result<AppReference, super::models::AppContractError> {
    let trimmed = raw.trim();
    if let Some(name) = trimmed.strip_prefix("capability:") {
        AppReference::parse(format!("capability:{name}"))
    } else if trimmed.contains(':') {
        AppReference::parse(trimmed)
    } else {
        AppReference::parse(format!("capability:{trimmed}"))
    }
}

fn normalized_tool_name(name: &str) -> String {
    super::manifest::normalized_collision_key(name)
}

impl From<super::models::AppContractError> for AppToolCatalogError {
    fn from(error: super::models::AppContractError) -> Self {
        Self::Lock(AppPackageLockError::InvalidManifest(error.to_string()))
    }
}

impl From<super::manifest::AppManifestError> for AppToolCatalogError {
    fn from(error: super::manifest::AppManifestError) -> Self {
        Self::Lock(AppPackageLockError::InvalidManifest(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::apps::{
        manifest::{build_app_package_candidate, tests::valid_bundle, AppPackageLimits},
        primitive_catalog::AppPrimitiveCatalogBuilder,
        tool_eligibility::typed_app_tool_document,
    };

    fn catalog_entry(name: &str) -> AppReviewedToolEntry {
        AppReviewedToolEntry {
            skill_document: typed_app_tool_document(
                name,
                "1.4.3",
                "    expose:\n      apps: true\n",
            ),
            immutable_revision_ref: AppReference::parse("catalog-revision:content-search")
                .expect("revision ref"),
            revision: AppRevision::new(1).expect("revision"),
        }
    }

    fn package_declaring_tool(name: &str) -> AppPackageCandidate {
        let mut bundle = valid_bundle();
        let manifest = bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .expect("manifest member");
        manifest.bytes = String::from_utf8(manifest.bytes.clone())
            .expect("UTF-8 manifest")
            .replace("content_search", name)
            .into_bytes();
        build_app_package_candidate(bundle, &AppPackageLimits::default()).expect("valid package")
    }

    fn package_declaring_exact_tool(
        name: &str,
        primitive_ref: &AppReference,
    ) -> AppPackageCandidate {
        let mut bundle = valid_bundle();
        let manifest = bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .expect("manifest member");
        manifest.bytes = String::from_utf8(manifest.bytes.clone())
            .expect("UTF-8 manifest")
            .replace("content_search", name)
            .replace(
                &format!(
                    "    capabilities:\n      - capability: {name}\n        \
                         version_requirement: \"^1\""
                ),
                &format!(
                    "    tools:\n      - name: {name}\n        primitive_ref: \
                         {primitive_ref}\n        version_requirement: \"^1\""
                ),
            )
            .into_bytes();
        build_app_package_candidate(bundle, &AppPackageLimits::default()).expect("valid package")
    }

    fn package_declaring_tool_action(name: &str, action: &str) -> AppPackageCandidate {
        let mut bundle = valid_bundle();
        let manifest = bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .expect("manifest member");
        manifest.bytes = String::from_utf8(manifest.bytes.clone())
            .expect("UTF-8 manifest")
            .replace("content_search", name)
            .replace(
                &format!(
                    "    capabilities:\n      - capability: {name}\n        version_requirement: \
                     \"^1\""
                ),
                &format!(
                    "    tools:\n      - name: {name}\n        actions: [{action}]\n        \
                     version_requirement: \"^1\""
                ),
            )
            .into_bytes();
        build_app_package_candidate(bundle, &AppPackageLimits::default()).expect("valid package")
    }

    fn callable_agent_source(agent_id: &str) -> String {
        format!(
            r#"agent_id: {agent_id}
version: 1
name: Reviewed child
description: One exact callable child.
persona: Return only the reviewed typed result.
tools: [content_read]
app_tool:
  input:
    type: object
    fields:
      request:
        type: text
        required: true
  result:
    type: object
    fields:
      answer:
        type: markdown
        required: true
  max_input_bytes: 16384
  max_result_bytes: 32768
"#
        )
    }

    #[test]
    fn catalog_snapshot_produces_internal_capability_identity() {
        let evidence = snapshot_reviewed_tool("content_search", &catalog_entry("content_search"))
            .expect("eligible catalog tool");
        assert_eq!(
            evidence.dependency_ref().as_str(),
            "capability:content_search"
        );
        assert_eq!(evidence.semantic_version(), "1.4.3");
    }

    #[test]
    fn catalog_snapshot_refuses_a_tool_without_expose_apps() {
        let mut entry = catalog_entry("content_search");
        entry.skill_document = typed_app_tool_document(
            "content_search",
            "1.4.3",
            "    expose:\n      apps: false\n",
        );
        let error = snapshot_reviewed_tool("content_search", &entry)
            .expect_err("explicit expose.apps: false is a deny");
        assert!(error.to_string().contains("expose.apps"));
    }

    #[test]
    fn catalog_snapshot_refuses_a_name_mismatch() {
        let error = snapshot_reviewed_tool("other-search", &catalog_entry("content_search"))
            .expect_err("name must match");
        assert!(matches!(error, AppToolCatalogError::NameMismatch { .. }));
    }

    #[test]
    fn complete_evidence_fills_only_missing_declared_tools() {
        let package = package_declaring_tool("next_step");
        let already = AppVerifiedRegistryDependency::from_trusted_registry_bytes(
            AppDependencyKind::Capability,
            AppReference::parse("capability:next_step").expect("ref"),
            "1.4.3".to_owned(),
            AppReference::parse("capability-revision:99").expect("rev"),
            AppRevision::new(99).expect("revision"),
            b"already-published",
        )
        .expect("registry evidence");
        let mut catalog = AppReviewedToolCatalog::resolver_snapshot();
        catalog.insert("next_step", catalog_entry("next_step"));
        let completed = complete_declared_tool_evidence(&package, vec![already.clone()], &catalog)
            .expect("merge");
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0].immutable_revision_ref().as_str(),
            "capability-revision:99"
        );

        let from_catalog =
            complete_declared_tool_evidence(&package, Vec::new(), &catalog).expect("catalog fill");
        assert_eq!(from_catalog.len(), 1);
        assert_eq!(
            from_catalog[0].immutable_revision_ref().as_str(),
            "catalog-revision:content-search"
        );
    }

    #[test]
    fn duplicate_normalized_registry_evidence_fails_closed_before_merge() {
        let package = package_declaring_tool("next_step");
        let evidence = AppVerifiedRegistryDependency::from_trusted_registry_bytes(
            AppDependencyKind::Capability,
            AppReference::parse("capability:next_step").expect("ref"),
            "1.4.3".to_owned(),
            AppReference::parse("capability-revision:99").expect("revision ref"),
            AppRevision::new(99).expect("revision"),
            b"already-published",
        )
        .expect("registry evidence");
        let differently_cased_evidence =
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Capability,
                AppReference::parse("capability:NEXT_STEP").expect("case-variant ref"),
                "1.4.3".to_owned(),
                AppReference::parse("capability-revision:100").expect("case-variant revision ref"),
                AppRevision::new(100).expect("case-variant revision"),
                b"case-variant-published",
            )
            .expect("case-variant registry evidence");
        let error = complete_declared_tool_evidence(
            &package,
            vec![evidence, differently_cased_evidence],
            &AppReviewedToolCatalog::resolver_snapshot(),
        )
        .expect_err("duplicate registry evidence must not be last-wins");
        assert_eq!(
            error,
            AppToolCatalogError::DuplicateRegistryEvidence("next_step".to_owned())
        );
    }

    #[test]
    fn primitive_snapshot_projects_exact_reviewed_skill_evidence() {
        let source =
            typed_app_tool_document("next_step", "1.4.3", "    expose:\n      apps: true\n");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(&source);
        let snapshot = builder.finish();
        let catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete primitive snapshot");
        assert_eq!(
            catalog
                .resolver_binding()
                .expect("resolver binding")
                .snapshot_digest(),
            snapshot.snapshot_digest()
        );
        let completed = complete_declared_tool_evidence(
            &package_declaring_tool("next_step"),
            Vec::new(),
            &catalog,
        )
        .expect("resolver evidence");
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0].content_digest(),
            &crate::magician_v2::apps::models::AppDigest::blake3(&source)
        );
        assert!(completed[0]
            .immutable_revision_ref()
            .as_str()
            .starts_with("primitive-source:skill:"));
    }

    #[test]
    fn interactive_snapshot_retains_exact_lock_evidence_without_generic_activation() {
        let source = String::from_utf8(typed_app_tool_document("browser", "1.0.0", ""))
            .expect("typed fixture is UTF-8")
            .replace(
                "---\nReturn one ranked next step.",
                "    runtime_catalog:\n      categories: [browser]\n      composition_category: \
                 web_operations\n---\nReturn one ranked next step.",
            )
            .into_bytes();
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(&source);
        let snapshot = builder.finish();
        let catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete interactive primitive snapshot");
        let binding = catalog
            .primitive_binding("browser")
            .expect("interactive lock evidence");
        assert_eq!(binding.actions().len(), 4);
        assert_eq!(
            binding
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            ["snapshot", "navigate", "scroll", "click"],
        );
        let error = complete_declared_tool_evidence(
            &package_declaring_tool("browser"),
            Vec::new(),
            &catalog,
        )
        .expect_err("browser dependency must name the sealed snapshot leaf");
        assert!(error
            .to_string()
            .contains("explicitly select only `snapshot`"));
        let completed = complete_declared_tool_evidence(
            &package_declaring_tool_action("browser", "snapshot"),
            Vec::new(),
            &catalog,
        )
        .expect("explicit interactive evidence locks");
        assert_eq!(completed.len(), 1);
        let selected = binding
            .select_actions(&["snapshot".to_owned()])
            .expect("exact Browser leaf selection");
        assert_eq!(completed[0].primitive_binding(), Some(&selected));
    }

    #[test]
    fn ordinary_skill_cannot_substitute_for_the_reserved_browser_owner() {
        let source = typed_app_tool_document("browser", "1.0.0", "");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(&source);
        let snapshot = builder.finish();
        let catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("ordinary source is a complete inert catalog input");
        assert!(catalog.primitive_binding("browser").is_none());
        let error = complete_declared_tool_evidence(
            &package_declaring_tool_action("browser", "snapshot"),
            Vec::new(),
            &catalog,
        )
        .expect_err("ordinary same-named skill cannot become Browser Observe");
        assert!(matches!(
            error,
            AppToolCatalogError::MissingResolverEntry(name) if name == "browser"
        ));
    }

    #[test]
    fn sealed_agent_definition_locks_only_with_explicit_agent_as_tool_action() {
        let source = callable_agent_source("reviewed-child");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        assert!(builder.add_agent_definition(source.as_bytes(), false));
        let snapshot = builder.finish();
        let catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete agent primitive snapshot");
        let binding = catalog
            .primitive_binding("reviewed-child")
            .expect("sealed agent binding");
        assert_eq!(binding.actions().len(), 1);
        assert_eq!(binding.actions()[0].name(), "agent_as_tool");

        let error = complete_declared_tool_evidence(
            &package_declaring_tool("reviewed-child"),
            Vec::new(),
            &catalog,
        )
        .expect_err("generic Agent selection stays closed");
        assert!(error
            .to_string()
            .contains("explicitly select only `agent_as_tool`"));

        let completed = complete_declared_tool_evidence(
            &package_declaring_tool_action("reviewed-child", "agent_as_tool"),
            Vec::new(),
            &catalog,
        )
        .expect("sealed agent leaf locks");
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].semantic_version(), "1.0.0");
        assert_eq!(completed[0].primitive_binding(), Some(binding));
        assert_eq!(
            completed[0].content_digest(),
            &AppDigest::blake3(source.as_bytes())
        );
    }

    #[test]
    fn exact_primitive_selector_can_lock_a_private_reserved_name() {
        let source =
            typed_app_tool_document("content_read", "1.4.3", "    expose:\n      apps: true\n");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_tool_skill(&source);
        let snapshot = builder.finish();
        let identity = snapshot.descriptors()[0].identity().clone();
        let catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete primitive snapshot");
        let completed = complete_declared_tool_evidence(
            &package_declaring_exact_tool("content_read", &identity),
            Vec::new(),
            &catalog,
        )
        .expect("exact private evidence");
        assert_eq!(
            completed[0].content_digest(),
            &crate::magician_v2::apps::models::AppDigest::blake3(&source)
        );
    }

    #[test]
    fn bare_tool_names_canonicalize_to_internal_capability_refs() {
        assert_eq!(
            canonical_app_tool_ref("content_search")
                .expect("bare name")
                .as_str(),
            "capability:content_search"
        );
        assert_eq!(
            canonical_app_tool_ref("capability:content_search")
                .expect("prefixed")
                .as_str(),
            "capability:content_search"
        );
        assert_eq!(
            canonical_app_tool_ref("video:render")
                .expect("other prefix stays")
                .as_str(),
            "video:render"
        );
    }

    #[test]
    fn compiled_content_read_snapshots_as_internal_capability() {
        let evidence = snapshot_compiled_pack("content_read").expect("compiled snapshot");
        assert_eq!(
            evidence.dependency_ref().as_str(),
            "capability:content_read"
        );
        assert!(evidence
            .immutable_revision_ref()
            .as_str()
            .starts_with("compiled-revision:content_read-"));
    }

    #[test]
    fn compiled_host_primitive_snapshots_as_internal_capability() {
        let evidence = snapshot_compiled_pack("files").expect("host primitive is lockable");
        assert_eq!(evidence.dependency_ref().as_str(), "capability:files");
    }

    #[test]
    fn external_evidence_mode_still_snapshots_reserved_embedded_tool() {
        let package = build_app_package_candidate(valid_bundle(), &AppPackageLimits::default())
            .expect("valid package");
        let filled = complete_declared_tool_evidence(
            &package,
            Vec::new(),
            &AppReviewedToolCatalog::external_immutable_evidence_only(),
        )
        .expect("compiled fill");
        assert_eq!(filled.len(), 1);
        assert_eq!(
            filled[0].dependency_ref().as_str(),
            "capability:content_search"
        );
        assert!(filled[0]
            .immutable_revision_ref()
            .as_str()
            .starts_with("compiled-revision:content_search-"));
    }

    #[test]
    fn external_evidence_mode_refuses_a_missing_normal_skill_explicitly() {
        let package = package_declaring_tool("next_step");
        let error = complete_declared_tool_evidence(
            &package,
            Vec::new(),
            &AppReviewedToolCatalog::external_immutable_evidence_only(),
        )
        .expect_err("normal skill needs exact external evidence");
        assert!(matches!(
            error,
            AppToolCatalogError::ExternalEvidenceRequired(name) if name == "next_step"
        ));
    }
}
