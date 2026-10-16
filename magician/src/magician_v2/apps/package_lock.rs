//! Immutable dependency resolution for admitted app-package candidates.
//!
//! The lock accepts only a fully validated [`AppPackageCandidate`]. Registry
//! inputs enter only as non-deserializable evidence minted by the trusted
//! registry adapter from an immutable revision and its exact bytes. Vendored
//! identity and version are read from the `SKILL.md` already covered by the
//! package bundle digest; callers cannot assert either value independently.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    interactive::{
        reviewed_interactive_action_class, AppInteractiveCapabilityRequest,
        AppInteractiveEffectClass, AppInteractiveLockedActionContract, AppInteractiveOwnerKind,
        AppReviewedInteractiveCapabilityGrant,
    },
    manifest::{
        normalized_collision_key, AppBundlePath, AppContributionEvidenceClass, AppDependencyKind,
        AppManifestContributionPort, AppManifestRunner, AppPackageCandidate, AppPackageLimits,
    },
    models::{AppContractLimits, AppDigest, AppName, AppReference, AppRevision},
    primitive_catalog::{
        AppPrimitiveActionDescriptor, AppPrimitiveDescriptor, AppPrimitiveDispatchStatus,
        AppPrimitiveEffect, AppPrimitiveExecutionClass,
    },
    records::{AppContributionDestinationBinding, AppReviewedContributionPortGrant},
};
use crate::magician_v2::json_traversal::{
    json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded,
};

pub const APP_PACKAGE_LOCK_VERSION: u8 = 5;
const LEGACY_APP_PACKAGE_LOCK_VERSION: u8 = 1;
const DESCRIPTOR_APP_PACKAGE_LOCK_VERSION: u8 = 2;
const PRE_IMPLEMENTATION_RECIPE_LOCK_VERSION: u8 = 3;
const CONTRIBUTION_APP_PACKAGE_LOCK_VERSION: u8 = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLockedRecipeBinding {
    workflow_id: AppName,
    runner: AppManifestRunner,
    member_path: AppBundlePath,
    member_content_digest: AppDigest,
    compiled_bundle_digest: AppDigest,
    recipe_ref: AppReference,
    topology_revision: AppRevision,
    topology_digest: AppDigest,
    input_schema_ref: AppReference,
    output_schema_ref: AppReference,
    schema_refs: BTreeSet<AppReference>,
    supported_node_set: BTreeSet<AppName>,
    supported_node_set_digest: AppDigest,
    compiled_plan_digest: AppDigest,
    #[serde(default = "missing_recipe_implementation_digest")]
    implementation_digest: AppDigest,
    binding_digest: AppDigest,
}

/// Immutable package-side identity for one declared workflow contribution
/// port. The complete closed declaration and exact result contract are bound
/// before owner review; a friendly workflow/port name alone is never enough.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLockedContributionPortBinding {
    schema: String,
    workflow_id: AppName,
    port_id: AppName,
    declaration: AppManifestContributionPort,
    destination_binding: AppContributionDestinationBinding,
    workflow_declaration_digest: AppDigest,
    workflow_result_digest: AppDigest,
    action_declaration_digests: BTreeMap<AppName, AppDigest>,
    binding_digest: AppDigest,
}

impl AppLockedContributionPortBinding {
    pub fn workflow_id(&self) -> &AppName {
        &self.workflow_id
    }

    pub fn port_id(&self) -> &AppName {
        &self.port_id
    }

    pub fn declaration(&self) -> &AppManifestContributionPort {
        &self.declaration
    }

    pub fn workflow_result_digest(&self) -> &AppDigest {
        &self.workflow_result_digest
    }

    pub fn destination_binding(&self) -> AppContributionDestinationBinding {
        self.destination_binding
    }

    pub fn workflow_declaration_digest(&self) -> &AppDigest {
        &self.workflow_declaration_digest
    }

    pub fn action_declaration_digest(&self, action_id: &AppName) -> Option<&AppDigest> {
        self.action_declaration_digests.get(action_id)
    }

    pub fn action_declaration_digests(&self) -> &BTreeMap<AppName, AppDigest> {
        &self.action_declaration_digests
    }

    pub fn binding_digest(&self) -> &AppDigest {
        &self.binding_digest
    }

    fn validate(&self) -> Result<(), AppPackageLockError> {
        if self.schema != "magician.app-locked-contribution-port.v1"
            || self.declaration.purposes.is_empty()
            || self.declaration.audiences.is_empty()
            || self.declaration.evidence_classes.is_empty()
            || self.declaration.maximum_retention_seconds == 0
            || self.destination_binding.destination() != self.declaration.destination
            || self.declaration.audiences.len() != 1
            || self.declaration.audiences[0].as_str()
                != self.destination_binding.required_audience()
            || self.declaration.evidence_classes.as_slice()
                != [AppContributionEvidenceClass::Hypothesis]
            || self.action_declaration_digests.is_empty()
        {
            return Err(AppPackageLockError::InvalidContributionPort(
                "a locked contribution port has incomplete identity or ceilings".to_owned(),
            ));
        }
        self.declaration
            .frequency
            .validate()
            .map_err(|error| AppPackageLockError::InvalidContributionPort(error.to_string()))?;
        AppReviewedContributionPortGrant {
            schema: "magician.app-reviewed-contribution-port-grant.v1".to_owned(),
            workflow_id: self.workflow_id.clone(),
            port_id: self.port_id.clone(),
            locked_port_digest: self.binding_digest.clone(),
            source: self.declaration.source.clone(),
            destination: self.declaration.destination,
            destination_binding: self.destination_binding,
            purposes: self.declaration.purposes.clone(),
            audiences: self.declaration.audiences.clone(),
            evidence_classes: self.declaration.evidence_classes.clone(),
            frequency: self.declaration.frequency,
            maximum_retention_seconds: self.declaration.maximum_retention_seconds,
            grant_digest: AppDigest::blake3(b"pending-locked-port-validation"),
        }
        .seal()
        .map_err(|error| AppPackageLockError::InvalidContributionPort(error.to_string()))?;
        if locked_contribution_port_digest(self)? != self.binding_digest {
            return Err(AppPackageLockError::InvalidContributionPort(
                "locked contribution-port digest mismatch".to_owned(),
            ));
        }
        Ok(())
    }
}

fn missing_recipe_implementation_digest() -> AppDigest {
    AppDigest::blake3(b"missing-app-recipe-runtime-implementation")
}

impl AppLockedRecipeBinding {
    pub fn workflow_id(&self) -> &AppName {
        &self.workflow_id
    }

    pub fn runner(&self) -> AppManifestRunner {
        self.runner
    }

    pub fn member_path(&self) -> &AppBundlePath {
        &self.member_path
    }

    pub fn member_content_digest(&self) -> &AppDigest {
        &self.member_content_digest
    }

    pub fn compiled_bundle_digest(&self) -> &AppDigest {
        &self.compiled_bundle_digest
    }

    pub fn recipe_ref(&self) -> &AppReference {
        &self.recipe_ref
    }

    pub fn topology_revision(&self) -> AppRevision {
        self.topology_revision
    }

    pub fn topology_digest(&self) -> &AppDigest {
        &self.topology_digest
    }

    pub fn input_schema_ref(&self) -> &AppReference {
        &self.input_schema_ref
    }

    pub fn output_schema_ref(&self) -> &AppReference {
        &self.output_schema_ref
    }

    pub fn schema_refs(&self) -> &BTreeSet<AppReference> {
        &self.schema_refs
    }

    pub fn supported_node_set(&self) -> &BTreeSet<AppName> {
        &self.supported_node_set
    }

    pub fn compiled_plan_digest(&self) -> &AppDigest {
        &self.compiled_plan_digest
    }

    pub fn implementation_digest(&self) -> &AppDigest {
        &self.implementation_digest
    }

    pub fn binding_digest(&self) -> &AppDigest {
        &self.binding_digest
    }

    /// Check the immutable evidence without interpreting it as runnable code.
    /// Accepted cleanup may outlive the runtime that originally admitted it.
    fn validate_identity(&self) -> Result<(), AppPackageLockError> {
        let supported_digest = AppDigest::blake3(
            &serde_json::to_vec(&self.supported_node_set)
                .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?,
        );
        if self.runner != AppManifestRunner::Recipe
            || self.schema_refs.is_empty()
            || !self.schema_refs.contains(&self.input_schema_ref)
            || !self.schema_refs.contains(&self.output_schema_ref)
            || self.supported_node_set.is_empty()
            || self.supported_node_set_digest != supported_digest
            || self.binding_digest != locked_recipe_binding_digest(self)?
        {
            return Err(AppPackageLockError::InvalidRecipe(
                "locked recipe binding is corrupt".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), AppPackageLockError> {
        self.validate_identity()?;
        if !super::recipe_lowering::app_recipe_runner_ready() {
            return Err(AppPackageLockError::InvalidRecipe(
                "recipe runner is conditional and not admitted".to_owned(),
            ));
        }
        let supported = super::recipe_lowering::supported_recipe_node_set()
            .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
        let mut original_v1 = supported.clone();
        original_v1
            .retain(|name| !matches!(name.as_str(), "contextual_round" | "store_transaction"));
        let mut round_v1 = original_v1.clone();
        round_v1.insert(
            AppName::parse("contextual_round")
                .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?,
        );
        let mut store_v1 = original_v1.clone();
        store_v1.insert(
            AppName::parse("store_transaction")
                .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?,
        );
        if (self.supported_node_set != supported
            && self.supported_node_set != original_v1
            && self.supported_node_set != round_v1
            && self.supported_node_set != store_v1)
            || self.implementation_digest
                != super::recipe_lowering::app_recipe_runtime_implementation_digest()
                    .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?
        {
            return Err(AppPackageLockError::InvalidRecipe(
                "locked recipe requires its reviewed runtime implementation".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Content-addressed action identity retained beside one locked capability.
///
/// This is evidence, not authority. Dispatch must compare it with a freshly
/// resolved descriptor before a physical-owner permit can be minted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLockedPrimitiveActionBinding {
    action_ref: AppReference,
    name: String,
    action_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_schema_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_schema_digest: Option<AppDigest>,
    /// Exact effect classes declared by the reviewed descriptor. Empty is
    /// accepted only while reading a pre-V5 lock and can never produce an
    /// interactive capability binding.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    effects: BTreeSet<AppPrimitiveEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    implementation_plan_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    physical_artifact_revision_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    physical_artifact_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport_result_byte_ceiling: Option<u64>,
    dispatchable: bool,
}

impl AppLockedPrimitiveActionBinding {
    pub fn action_ref(&self) -> &AppReference {
        &self.action_ref
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn action_digest(&self) -> &AppDigest {
        &self.action_digest
    }

    pub fn input_schema_digest(&self) -> Option<&AppDigest> {
        self.input_schema_digest.as_ref()
    }

    pub fn result_schema_digest(&self) -> Option<&AppDigest> {
        self.result_schema_digest.as_ref()
    }

    pub fn effects(&self) -> &BTreeSet<AppPrimitiveEffect> {
        &self.effects
    }

    pub fn implementation_plan_digest(&self) -> Option<&AppDigest> {
        self.implementation_plan_digest.as_ref()
    }

    pub fn physical_artifact_revision_ref(&self) -> Option<&AppReference> {
        self.physical_artifact_revision_ref.as_ref()
    }

    pub fn physical_artifact_digest(&self) -> Option<&AppDigest> {
        self.physical_artifact_digest.as_ref()
    }

    pub fn transport_result_byte_ceiling(&self) -> Option<u64> {
        self.transport_result_byte_ceiling
    }

    pub fn dispatchable(&self) -> bool {
        self.dispatchable
    }
}

/// Exact descriptor and selected action set admitted into a package lock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLockedPrimitiveBinding {
    primitive_ref: AppReference,
    descriptor_digest: AppDigest,
    source_content_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interactive_owner: Option<AppInteractiveOwnerKind>,
    actions: Vec<AppLockedPrimitiveActionBinding>,
    binding_digest: AppDigest,
}

impl AppLockedPrimitiveBinding {
    pub fn from_descriptor(
        descriptor: &AppPrimitiveDescriptor,
    ) -> Result<Self, AppPackageLockError> {
        Self::from_descriptor_with_action_selectors(descriptor, &[])
    }

    pub fn from_descriptor_with_action_selectors(
        descriptor: &AppPrimitiveDescriptor,
        selectors: &[String],
    ) -> Result<Self, AppPackageLockError> {
        let selected = selectors
            .iter()
            .map(|selector| selector.trim())
            .collect::<BTreeSet<_>>();
        if selected.len() != selectors.len() || selected.iter().any(|selector| selector.is_empty())
        {
            return Err(AppPackageLockError::InvalidManifest(
                "primitive action selectors must be non-empty and unique".to_owned(),
            ));
        }
        let actions = descriptor
            .actions()
            .iter()
            .filter(|action| {
                selected.is_empty()
                    || selected.iter().any(|selector| {
                        super::app_tool_bind::normalize_app_action_name(selector)
                            == super::app_tool_bind::normalize_app_action_name(action.name())
                            || *selector == action.identity().as_str()
                    })
            })
            .map(locked_action_binding)
            .collect::<Vec<_>>();
        if actions.is_empty() || (!selected.is_empty() && actions.len() != selected.len()) {
            // Name the primitive and both sides of the mismatch. This rejection
            // reaches an owner as "could not be approved from the current
            // package evidence", so without the primitive ref, the selectors
            // and what the descriptor actually offers, neither the owner nor an
            // operator can tell which dependency drifted — or whether a
            // selector matched nothing or matched more than one action.
            return Err(AppPackageLockError::InvalidManifest(format!(
                "primitive action selector does not name an exact descriptor action for {}: {} \
                 selector(s) [{}] matched {} descriptor action(s); descriptor offers [{}]",
                descriptor.identity().as_str(),
                selected.len(),
                selected.iter().copied().collect::<Vec<_>>().join(", "),
                actions.len(),
                descriptor
                    .actions()
                    .iter()
                    .map(|action| action.name())
                    .collect::<Vec<_>>()
                    .join(", "),
            )));
        }
        let interactive_owner = match descriptor.execution_class() {
            AppPrimitiveExecutionClass::BrowserOwner => Some(AppInteractiveOwnerKind::Browser),
            AppPrimitiveExecutionClass::MacosHostOwner => Some(AppInteractiveOwnerKind::Macos),
            AppPrimitiveExecutionClass::AndroidDeviceOwner => {
                Some(AppInteractiveOwnerKind::Android)
            },
            _ => None,
        };
        let binding_digest = primitive_binding_digest(
            descriptor.identity(),
            descriptor.descriptor_digest(),
            descriptor.source().content_digest(),
            interactive_owner,
            &actions,
        )?;
        Ok(Self {
            primitive_ref: descriptor.identity().clone(),
            descriptor_digest: descriptor.descriptor_digest().clone(),
            source_content_digest: descriptor.source().content_digest().clone(),
            interactive_owner,
            actions,
            binding_digest,
        })
    }

    pub fn primitive_ref(&self) -> &AppReference {
        &self.primitive_ref
    }

    pub fn descriptor_digest(&self) -> &AppDigest {
        &self.descriptor_digest
    }

    pub fn source_content_digest(&self) -> &AppDigest {
        &self.source_content_digest
    }

    pub fn interactive_owner(&self) -> Option<AppInteractiveOwnerKind> {
        self.interactive_owner
    }

    pub fn actions(&self) -> &[AppLockedPrimitiveActionBinding] {
        &self.actions
    }

    pub fn binding_digest(&self) -> &AppDigest {
        &self.binding_digest
    }

    pub fn action_named(&self, name: &str) -> Option<&AppLockedPrimitiveActionBinding> {
        let normalized = super::app_tool_bind::normalize_app_action_name(name)?;
        self.actions.iter().find(|action| {
            super::app_tool_bind::normalize_app_action_name(action.name()).as_ref()
                == Some(&normalized)
        })
    }

    pub fn select_actions(&self, selectors: &[String]) -> Result<Self, AppPackageLockError> {
        if selectors.is_empty() {
            return Ok(self.clone());
        }
        let selected = selectors
            .iter()
            .map(|selector| selector.trim())
            .collect::<BTreeSet<_>>();
        if selected.len() != selectors.len() || selected.iter().any(|selector| selector.is_empty())
        {
            return Err(AppPackageLockError::InvalidManifest(
                "primitive action selectors must be non-empty and unique".to_owned(),
            ));
        }
        let actions = self
            .actions
            .iter()
            .filter(|action| {
                selected.iter().any(|selector| {
                    super::app_tool_bind::normalize_app_action_name(selector)
                        == super::app_tool_bind::normalize_app_action_name(action.name())
                        || *selector == action.action_ref().as_str()
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if actions.is_empty() || actions.len() != selected.len() {
            // Same reasoning as the descriptor-side check above: this is the
            // re-restriction of an already-locked binding, so it fires when a
            // locked action set and the manifest's selectors have drifted apart.
            return Err(AppPackageLockError::InvalidManifest(format!(
                "primitive action selector does not name an exact descriptor action for {}: {} \
                 selector(s) [{}] matched {} locked action(s); lock holds [{}]",
                self.primitive_ref.as_str(),
                selected.len(),
                selected.iter().copied().collect::<Vec<_>>().join(", "),
                actions.len(),
                self.actions
                    .iter()
                    .map(|action| action.name())
                    .collect::<Vec<_>>()
                    .join(", "),
            )));
        }
        let binding_digest = primitive_binding_digest(
            &self.primitive_ref,
            &self.descriptor_digest,
            &self.source_content_digest,
            self.interactive_owner,
            &actions,
        )?;
        Ok(Self {
            primitive_ref: self.primitive_ref.clone(),
            descriptor_digest: self.descriptor_digest.clone(),
            source_content_digest: self.source_content_digest.clone(),
            interactive_owner: self.interactive_owner,
            actions,
            binding_digest,
        })
    }

    pub(crate) fn with_physical_artifacts(
        mut self,
        artifacts: &BTreeMap<String, (AppReference, AppDigest)>,
    ) -> Result<Self, AppPackageLockError> {
        if artifacts
            .keys()
            .any(|name| !self.actions.iter().any(|action| action.name() == name))
        {
            return Err(AppPackageLockError::InvalidManifest(
                "physical artifact evidence names an unselected primitive action".to_owned(),
            ));
        }
        for action in &mut self.actions {
            if let Some((revision_ref, digest)) = artifacts.get(action.name()) {
                if action
                    .physical_artifact_revision_ref
                    .as_ref()
                    .is_some_and(|current| current != revision_ref)
                    || action
                        .physical_artifact_digest
                        .as_ref()
                        .is_some_and(|current| current != digest)
                {
                    return Err(AppPackageLockError::InvalidManifest(
                        "physical artifact evidence conflicts with the reviewed descriptor"
                            .to_owned(),
                    ));
                }
                action.physical_artifact_revision_ref = Some(revision_ref.clone());
                action.physical_artifact_digest = Some(digest.clone());
            }
        }
        self.binding_digest = primitive_binding_digest(
            &self.primitive_ref,
            &self.descriptor_digest,
            &self.source_content_digest,
            self.interactive_owner,
            &self.actions,
        )?;
        self.validate()?;
        Ok(self)
    }

    pub fn matches_descriptor(
        &self,
        descriptor: &AppPrimitiveDescriptor,
    ) -> Result<bool, AppPackageLockError> {
        let selectors = self
            .actions
            .iter()
            .map(|action| action.action_ref().to_string())
            .collect::<Vec<_>>();
        // The selectors above are the *locked* action identities, so a live
        // descriptor that no longer carries them cannot resolve any of them.
        // That is exactly "does not match" — the question this function exists
        // to answer — and not an invalid manifest. Propagating the resolver's
        // `InvalidManifest` instead blamed the package author for a
        // platform-side descriptor change and short-circuited
        // `authorize_locked_primitive` before it could raise its own
        // `PrimitiveBindingMismatch`. Every other error is still a real fault
        // and still propagates.
        let current = match Self::from_descriptor_with_action_selectors(descriptor, &selectors) {
            Ok(current) => current,
            Err(AppPackageLockError::InvalidManifest(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut reviewed_source_projection = self.clone();
        for action in &mut reviewed_source_projection.actions {
            action.physical_artifact_revision_ref = None;
            action.physical_artifact_digest = None;
        }
        reviewed_source_projection.binding_digest = primitive_binding_digest(
            &reviewed_source_projection.primitive_ref,
            &reviewed_source_projection.descriptor_digest,
            &reviewed_source_projection.source_content_digest,
            reviewed_source_projection.interactive_owner,
            &reviewed_source_projection.actions,
        )?;
        Ok(reviewed_source_projection == current)
    }

    pub(crate) fn matches_source_binding(
        &self,
        expected: &Self,
    ) -> Result<bool, AppPackageLockError> {
        let mut projection = self.clone();
        for action in &mut projection.actions {
            action.physical_artifact_revision_ref = None;
            action.physical_artifact_digest = None;
        }
        projection.binding_digest = primitive_binding_digest(
            &projection.primitive_ref,
            &projection.descriptor_digest,
            &projection.source_content_digest,
            projection.interactive_owner,
            &projection.actions,
        )?;
        Ok(&projection == expected)
    }

    fn validate(&self) -> Result<(), AppPackageLockError> {
        if self.actions.is_empty() {
            return Err(AppPackageLockError::InvalidPersistedLock(
                "primitive binding must retain at least one reviewed action".to_owned(),
            ));
        }
        let mut names = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for action in &self.actions {
            if action.name.is_empty()
                || !names.insert(normalized_collision_key(&action.name))
                || !identities.insert(action.action_ref.clone())
                || action.physical_artifact_revision_ref.is_some()
                    != action.physical_artifact_digest.is_some()
                || (action.dispatchable
                    && action
                        .transport_result_byte_ceiling
                        .is_none_or(|ceiling| ceiling == 0))
            {
                return Err(AppPackageLockError::InvalidPersistedLock(
                    "primitive actions are empty, duplicated or ambiguous".to_owned(),
                ));
            }
        }
        let expected = primitive_binding_digest(
            &self.primitive_ref,
            &self.descriptor_digest,
            &self.source_content_digest,
            self.interactive_owner,
            &self.actions,
        )?;
        if expected != self.binding_digest {
            return Err(AppPackageLockError::InvalidPersistedLock(
                "primitive binding digest does not match its descriptor/action bytes".to_owned(),
            ));
        }
        Ok(())
    }
}

const APP_LOCKED_INTERACTIVE_BINDING_V1: &str = "magician.app-locked-interactive-capability.v1";

/// Exact package-side identity for one interactive dependency request.
///
/// This remains inert review evidence. Live authority still requires a
/// current owner-reviewed grant plus fresh installation, pairing/profile and
/// implementation revalidation at the physical disclosure boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLockedInteractiveCapabilityBinding {
    schema: String,
    dependency_ref: AppReference,
    owner: AppInteractiveOwnerKind,
    request: AppInteractiveCapabilityRequest,
    request_digest: AppDigest,
    target_selector_digest: AppDigest,
    owner_profile_request_digest: AppDigest,
    primitive_binding_digest: AppDigest,
    binding_digest: AppDigest,
}

impl AppLockedInteractiveCapabilityBinding {
    fn seal(
        dependency_ref: AppReference,
        owner: AppInteractiveOwnerKind,
        request: AppInteractiveCapabilityRequest,
        primitive: &AppLockedPrimitiveBinding,
    ) -> Result<Self, AppPackageLockError> {
        request
            .validate_for_admission()
            .map_err(|error| AppPackageLockError::InvalidInteractiveBinding(error.to_string()))?;
        let selected_action_class = primitive
            .actions()
            .first()
            .and_then(|action| reviewed_interactive_action_class(owner, action.name()));
        if request.owner != owner
            || primitive.actions().len() != 1
            || selected_action_class.is_none()
            || request.action_classes
                != BTreeSet::from([selected_action_class.expect("checked above")])
            || !primitive.actions()[0].dispatchable()
            || primitive.actions()[0].input_schema_digest().is_none()
            || primitive.actions()[0].result_schema_digest().is_none()
            || primitive.actions()[0]
                .implementation_plan_digest()
                .is_none()
            || primitive.actions()[0].effects().is_empty()
            || primitive.actions()[0]
                .effects()
                .contains(&AppPrimitiveEffect::Undeclared)
            || primitive.actions()[0].transport_result_byte_ceiling()
                != Some(request.resources.max_output_bytes())
        {
            return Err(AppPackageLockError::InvalidInteractiveBinding(
                "interactive request does not exactly match the selected physical-owner action"
                    .to_owned(),
            ));
        }
        let request_digest = request
            .request_digest()
            .map_err(|error| AppPackageLockError::InvalidInteractiveBinding(error.to_string()))?;
        let target_selector_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value((&request.allowed_origins, &request.target_selectors))
                .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?,
        )
        .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
        let owner_profile_request_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value((request.owner, request.target_profile_class))
                .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?,
        )
        .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
        let mut value = Self {
            schema: APP_LOCKED_INTERACTIVE_BINDING_V1.to_owned(),
            dependency_ref,
            owner,
            request,
            request_digest,
            target_selector_digest,
            owner_profile_request_digest,
            primitive_binding_digest: primitive.binding_digest().clone(),
            binding_digest: AppDigest::blake3(b"pending-locked-interactive-binding"),
        };
        value.binding_digest = locked_interactive_binding_digest(&value)?;
        Ok(value)
    }

    fn validate(
        &self,
        dependency_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
    ) -> Result<(), AppPackageLockError> {
        let reproduced = Self::seal(
            dependency_ref.clone(),
            self.owner,
            self.request.clone(),
            primitive,
        )?;
        if self.schema != APP_LOCKED_INTERACTIVE_BINDING_V1
            || self.dependency_ref != *dependency_ref
            || &self.primitive_binding_digest != primitive.binding_digest()
            || reproduced != *self
        {
            return Err(AppPackageLockError::InvalidInteractiveBinding(
                "persisted interactive binding is corrupt or substituted".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn dependency_ref(&self) -> &AppReference {
        &self.dependency_ref
    }

    pub fn owner(&self) -> AppInteractiveOwnerKind {
        self.owner
    }

    pub fn request(&self) -> &AppInteractiveCapabilityRequest {
        &self.request
    }

    pub fn request_digest(&self) -> &AppDigest {
        &self.request_digest
    }

    pub fn target_selector_digest(&self) -> &AppDigest {
        &self.target_selector_digest
    }

    pub fn owner_profile_request_digest(&self) -> &AppDigest {
        &self.owner_profile_request_digest
    }

    pub fn primitive_binding_digest(&self) -> &AppDigest {
        &self.primitive_binding_digest
    }

    pub fn binding_digest(&self) -> &AppDigest {
        &self.binding_digest
    }

    pub fn review_grant(
        &self,
        primitive: &AppLockedPrimitiveBinding,
        granted: AppInteractiveCapabilityRequest,
    ) -> Result<AppReviewedInteractiveCapabilityGrant, AppPackageLockError> {
        self.validate(&self.dependency_ref, primitive)?;
        let action = &primitive.actions()[0];
        let class =
            reviewed_interactive_action_class(self.owner, action.name()).ok_or_else(|| {
                AppPackageLockError::InvalidInteractiveBinding(
                    "interactive action is not implemented by its physical owner".to_owned(),
                )
            })?;
        let effects = action
            .effects()
            .iter()
            .map(|effect| match effect {
                AppPrimitiveEffect::Pure => Ok(AppInteractiveEffectClass::Pure),
                AppPrimitiveEffect::ClockRead => Ok(AppInteractiveEffectClass::ClockRead),
                AppPrimitiveEffect::NetworkRead => Ok(AppInteractiveEffectClass::NetworkRead),
                AppPrimitiveEffect::WorkspaceRead => Ok(AppInteractiveEffectClass::WorkspaceRead),
                AppPrimitiveEffect::WorkspaceWrite => Ok(AppInteractiveEffectClass::WorkspaceWrite),
                AppPrimitiveEffect::StructuredDataRead => {
                    Ok(AppInteractiveEffectClass::StructuredDataRead)
                },
                AppPrimitiveEffect::HostRead => Ok(AppInteractiveEffectClass::HostRead),
                AppPrimitiveEffect::ExternalMutation => {
                    Ok(AppInteractiveEffectClass::ExternalMutation)
                },
                AppPrimitiveEffect::DeviceInteraction => {
                    Ok(AppInteractiveEffectClass::DeviceInteraction)
                },
                AppPrimitiveEffect::Undeclared => {
                    Err(AppPackageLockError::InvalidInteractiveBinding(
                        "interactive action has an undeclared effect".to_owned(),
                    ))
                },
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        AppReviewedInteractiveCapabilityGrant::from_owner_review(
            self.dependency_ref.clone(),
            self.binding_digest.clone(),
            primitive.binding_digest().clone(),
            self.request_digest.clone(),
            self.request.clone(),
            granted,
            AppInteractiveLockedActionContract {
                action_ref: action.action_ref().clone(),
                class,
                action_digest: action.action_digest().clone(),
                input_schema_digest: action.input_schema_digest().cloned().ok_or_else(|| {
                    AppPackageLockError::InvalidInteractiveBinding(
                        "interactive action has no input schema digest".to_owned(),
                    )
                })?,
                result_schema_digest: action.result_schema_digest().cloned().ok_or_else(|| {
                    AppPackageLockError::InvalidInteractiveBinding(
                        "interactive action has no result schema digest".to_owned(),
                    )
                })?,
                effects,
                implementation_plan_digest: action
                    .implementation_plan_digest()
                    .cloned()
                    .ok_or_else(|| {
                        AppPackageLockError::InvalidInteractiveBinding(
                            "interactive action has no implementation plan digest".to_owned(),
                        )
                    })?,
                result_byte_ceiling: action.transport_result_byte_ceiling().ok_or_else(|| {
                    AppPackageLockError::InvalidInteractiveBinding(
                        "interactive action has no result ceiling".to_owned(),
                    )
                })?,
            },
        )
        .map_err(|error| AppPackageLockError::InvalidInteractiveBinding(error.to_string()))
    }
}

fn locked_interactive_binding_digest(
    binding: &AppLockedInteractiveCapabilityBinding,
) -> Result<AppDigest, AppPackageLockError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        schema: &'a str,
        dependency_ref: &'a AppReference,
        owner: AppInteractiveOwnerKind,
        request_digest: &'a AppDigest,
        target_selector_digest: &'a AppDigest,
        owner_profile_request_digest: &'a AppDigest,
        primitive_binding_digest: &'a AppDigest,
    }
    AppDigest::blake3_canonical_json(
        &serde_json::to_value(Identity {
            schema: &binding.schema,
            dependency_ref: &binding.dependency_ref,
            owner: binding.owner,
            request_digest: &binding.request_digest,
            target_selector_digest: &binding.target_selector_digest,
            owner_profile_request_digest: &binding.owner_profile_request_digest,
            primitive_binding_digest: &binding.primitive_binding_digest,
        })
        .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?,
    )
    .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))
}

fn locked_action_binding(action: &AppPrimitiveActionDescriptor) -> AppLockedPrimitiveActionBinding {
    AppLockedPrimitiveActionBinding {
        action_ref: action.identity().clone(),
        name: action.name().to_owned(),
        action_digest: action.action_digest().clone(),
        input_schema_digest: action.input_schema().digest().cloned(),
        result_schema_digest: action.result_schema().digest().cloned(),
        effects: action.effects().clone(),
        implementation_plan_digest: action.implementation_plan_digest().cloned(),
        physical_artifact_revision_ref: action.physical_artifact_revision_ref().cloned(),
        physical_artifact_digest: action.physical_artifact_digest().cloned(),
        transport_result_byte_ceiling: action.transport_result_byte_ceiling(),
        dispatchable: action.dispatch().status() == AppPrimitiveDispatchStatus::Ready,
    }
}

fn primitive_binding_digest(
    primitive_ref: &AppReference,
    descriptor_digest: &AppDigest,
    source_content_digest: &AppDigest,
    interactive_owner: Option<AppInteractiveOwnerKind>,
    actions: &[AppLockedPrimitiveActionBinding],
) -> Result<AppDigest, AppPackageLockError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        schema: &'static str,
        primitive_ref: &'a AppReference,
        descriptor_digest: &'a AppDigest,
        source_content_digest: &'a AppDigest,
        #[serde(skip_serializing_if = "Option::is_none")]
        interactive_owner: Option<AppInteractiveOwnerKind>,
        actions: &'a [AppLockedPrimitiveActionBinding],
    }
    let bytes = serde_json::to_vec(&Identity {
        schema: "magician.app-locked-primitive.v1",
        primitive_ref,
        descriptor_digest,
        source_content_digest,
        interactive_owner,
        actions,
    })
    .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
    Ok(AppDigest::blake3(&bytes))
}

/// Server-minted proof for bytes resolved through an immutable registry
/// revision. It is intentionally not deserializable and its fields are private:
/// transport claims cannot manufacture trusted dependency evidence.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppVerifiedRegistryDependency {
    kind: AppDependencyKind,
    dependency_ref: AppReference,
    semantic_version: String,
    immutable_revision_ref: AppReference,
    revision: AppRevision,
    content_digest: AppDigest,
    #[serde(skip_serializing_if = "Option::is_none")]
    primitive_binding: Option<AppLockedPrimitiveBinding>,
}

impl AppVerifiedRegistryDependency {
    /// Bind an immutable resolver result to the exact bytes it returned. Only
    /// the trusted registry adapter should call this constructor.
    pub fn from_trusted_registry_bytes(
        kind: AppDependencyKind,
        dependency_ref: AppReference,
        semantic_version: String,
        immutable_revision_ref: AppReference,
        revision: AppRevision,
        content_bytes: &[u8],
    ) -> Result<Self, AppPackageLockError> {
        let max_version_bytes = AppPackageLimits::default().max_string_bytes();
        if semantic_version.is_empty() || semantic_version.len() > max_version_bytes {
            return Err(AppPackageLockError::InvalidResolvedVersion {
                dependency_ref: dependency_ref.to_string(),
                reason: format!("version is empty or exceeds the {max_version_bytes} byte ceiling"),
            });
        }
        let semantic_version = semver::Version::parse(&semantic_version)
            .map_err(|error| AppPackageLockError::InvalidResolvedVersion {
                dependency_ref: dependency_ref.to_string(),
                reason: error.to_string(),
            })?
            .to_string();
        Ok(Self {
            kind,
            dependency_ref,
            semantic_version,
            immutable_revision_ref,
            revision,
            content_digest: AppDigest::blake3(content_bytes),
            primitive_binding: None,
        })
    }

    /// Bind registry bytes to the exact immutable primitive descriptor and its
    /// complete action set. Descriptor source bytes must be the same bytes the
    /// registry revision locks; a caller cannot pair a descriptor with another
    /// implementation merely because the friendly names agree.
    #[allow(clippy::too_many_arguments)]
    pub fn from_trusted_primitive_bytes(
        kind: AppDependencyKind,
        dependency_ref: AppReference,
        semantic_version: String,
        immutable_revision_ref: AppReference,
        revision: AppRevision,
        content_bytes: &[u8],
        descriptor: &AppPrimitiveDescriptor,
    ) -> Result<Self, AppPackageLockError> {
        if kind != AppDependencyKind::Capability
            || descriptor.source().content_digest() != &AppDigest::blake3(content_bytes)
        {
            return Err(AppPackageLockError::PrimitiveBindingMismatch(
                dependency_ref.to_string(),
            ));
        }
        let mut evidence = Self::from_trusted_registry_bytes(
            kind,
            dependency_ref,
            semantic_version,
            immutable_revision_ref,
            revision,
            content_bytes,
        )?;
        evidence.primitive_binding = Some(AppLockedPrimitiveBinding::from_descriptor(descriptor)?);
        Ok(evidence)
    }

    /// Same trusted constructor for catalog projections that already retained
    /// the validated binding independently of the descriptor DTO.
    #[allow(clippy::too_many_arguments)]
    pub fn from_trusted_primitive_binding_bytes(
        kind: AppDependencyKind,
        dependency_ref: AppReference,
        semantic_version: String,
        immutable_revision_ref: AppReference,
        revision: AppRevision,
        content_bytes: &[u8],
        primitive_binding: AppLockedPrimitiveBinding,
    ) -> Result<Self, AppPackageLockError> {
        primitive_binding.validate()?;
        if kind != AppDependencyKind::Capability
            || primitive_binding.source_content_digest() != &AppDigest::blake3(content_bytes)
        {
            return Err(AppPackageLockError::PrimitiveBindingMismatch(
                dependency_ref.to_string(),
            ));
        }
        let mut evidence = Self::from_trusted_registry_bytes(
            kind,
            dependency_ref,
            semantic_version,
            immutable_revision_ref,
            revision,
            content_bytes,
        )?;
        evidence.primitive_binding = Some(primitive_binding);
        Ok(evidence)
    }

    pub fn select_primitive_actions(
        mut self,
        selectors: &[String],
    ) -> Result<Self, AppPackageLockError> {
        if selectors.is_empty() {
            return Ok(self);
        }
        let binding = self.primitive_binding.take().ok_or_else(|| {
            AppPackageLockError::InvalidManifest(
                "action selectors require immutable primitive binding evidence".to_owned(),
            )
        })?;
        self.primitive_binding = Some(binding.select_actions(selectors)?);
        Ok(self)
    }

    pub fn dependency_ref(&self) -> &AppReference {
        &self.dependency_ref
    }

    pub fn semantic_version(&self) -> &str {
        &self.semantic_version
    }

    pub fn immutable_revision_ref(&self) -> &AppReference {
        &self.immutable_revision_ref
    }

    pub fn revision(&self) -> AppRevision {
        self.revision
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn primitive_binding(&self) -> Option<&AppLockedPrimitiveBinding> {
        self.primitive_binding.as_ref()
    }

    /// Build non-authoritative dependency evidence for the local authoring
    /// lock. The CLI reads the exact fixture bytes named by the developer and
    /// records their immutable identity, but the server still resolves and
    /// verifies every dependency independently before publication. This value
    /// therefore proves reproducibility only; it never grants authority.
    pub fn from_authoring_fixture_bytes(
        kind: AppDependencyKind,
        dependency_ref: AppReference,
        semantic_version: String,
        immutable_revision_ref: AppReference,
        revision: AppRevision,
        content_bytes: &[u8],
    ) -> Result<Self, AppPackageLockError> {
        Self::from_trusted_registry_bytes(
            kind,
            dependency_ref,
            semantic_version,
            immutable_revision_ref,
            revision,
            content_bytes,
        )
    }
}

/// Server-validated source identity retained in a dependency lock entry.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppLockedDependencySource {
    RegistryRevision {
        immutable_revision_ref: AppReference,
        revision: AppRevision,
    },
    VendoredBundleMember {
        path: AppBundlePath,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLockedDependency {
    kind: AppDependencyKind,
    dependency_ref: AppReference,
    semantic_version: String,
    content_digest: AppDigest,
    source: AppLockedDependencySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primitive_binding: Option<AppLockedPrimitiveBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interactive_binding: Option<AppLockedInteractiveCapabilityBinding>,
}

impl AppLockedDependency {
    pub fn kind(&self) -> AppDependencyKind {
        self.kind
    }

    pub fn dependency_ref(&self) -> &AppReference {
        &self.dependency_ref
    }

    pub fn semantic_version(&self) -> &str {
        &self.semantic_version
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn source(&self) -> &AppLockedDependencySource {
        &self.source
    }

    pub fn primitive_binding(&self) -> Option<&AppLockedPrimitiveBinding> {
        self.primitive_binding.as_ref()
    }

    pub fn interactive_binding(&self) -> Option<&AppLockedInteractiveCapabilityBinding> {
        self.interactive_binding.as_ref()
    }
}

/// Complete immutable dependency identity for one exact app bundle.
///
/// Private fields prevent callers from constructing a trusted lock directly.
/// This type is serialization-only; portable archives deserialize into the
/// distinct [`AppPortablePackageLockClaim`] transport type so request bytes
/// cannot accidentally acquire runtime authority.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPackageLock {
    lock_version: u8,
    manifest_digest: AppDigest,
    bundle_digest: AppDigest,
    dependencies: Vec<AppLockedDependency>,
    recipe_bindings: Vec<AppLockedRecipeBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    contribution_bindings: Vec<AppLockedContributionPortBinding>,
    lock_digest: AppDigest,
}

/// Strict but explicitly untrusted package-lock claim carried by a portable
/// archive. Candidate publication must independently reload registry revision
/// bytes and reproduce this digest before producing an authoritative lock.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct AppPortablePackageLockClaim(AppPackageLock);

impl<'de> Deserialize<'de> for AppPortablePackageLockClaim {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let persisted = PersistedAppPackageLock::deserialize(deserializer)?;
        validate_persisted_package_lock(persisted)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl AppPortablePackageLockClaim {
    pub fn from_trusted(lock: &AppPackageLock) -> Self {
        Self(lock.clone())
    }

    pub fn claimed_lock(&self) -> &AppPackageLock {
        &self.0
    }
}

/// Exact dependency authorization consumed by the governed skill runtime.
///
/// The fence is serialization-only, binds the invocation to the complete app
/// package lock and cannot be minted from a mutable skill name.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLockedSkillExecutionFence {
    package_lock_digest: AppDigest,
    dependency_ref: AppReference,
    semantic_version: String,
    content_digest: AppDigest,
    source: AppLockedDependencySource,
}

impl AppLockedSkillExecutionFence {
    pub fn package_lock_digest(&self) -> &AppDigest {
        &self.package_lock_digest
    }

    pub fn dependency_ref(&self) -> &AppReference {
        &self.dependency_ref
    }

    pub fn semantic_version(&self) -> &str {
        &self.semantic_version
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn source(&self) -> &AppLockedDependencySource {
        &self.source
    }
}

impl AppPackageLock {
    pub fn lock_version(&self) -> u8 {
        self.lock_version
    }

    pub fn manifest_digest(&self) -> &AppDigest {
        &self.manifest_digest
    }

    pub fn bundle_digest(&self) -> &AppDigest {
        &self.bundle_digest
    }

    pub fn dependencies(&self) -> &[AppLockedDependency] {
        &self.dependencies
    }

    pub fn recipe_bindings(&self) -> &[AppLockedRecipeBinding] {
        &self.recipe_bindings
    }

    pub fn contribution_bindings(&self) -> &[AppLockedContributionPortBinding] {
        &self.contribution_bindings
    }

    pub fn contribution_port(
        &self,
        workflow_id: &AppName,
        port_id: &AppName,
    ) -> Option<&AppLockedContributionPortBinding> {
        self.contribution_bindings
            .iter()
            .find(|binding| binding.workflow_id() == workflow_id && binding.port_id() == port_id)
    }

    pub fn recipe_for_workflow(&self, workflow_id: &AppName) -> Option<&AppLockedRecipeBinding> {
        self.recipe_bindings
            .iter()
            .find(|binding| binding.workflow_id() == workflow_id)
    }

    pub fn lock_digest(&self) -> &AppDigest {
        &self.lock_digest
    }

    pub fn capability(&self, dependency_ref: &AppReference) -> Option<&AppLockedDependency> {
        self.dependencies.iter().find(|dependency| {
            dependency.kind == AppDependencyKind::Capability
                && dependency.dependency_ref == *dependency_ref
        })
    }

    pub fn interactive_capability(
        &self,
        dependency_ref: &AppReference,
    ) -> Option<&AppLockedInteractiveCapabilityBinding> {
        self.capability(dependency_ref)
            .and_then(AppLockedDependency::interactive_binding)
    }

    pub fn capability_for_primitive(
        &self,
        primitive_ref: &AppReference,
    ) -> Option<&AppLockedDependency> {
        self.dependencies.iter().find(|dependency| {
            dependency.kind == AppDependencyKind::Capability
                && dependency
                    .primitive_binding()
                    .is_some_and(|binding| binding.primitive_ref() == primitive_ref)
        })
    }
}

/// Compare a locked capability with the freshly resolved descriptor used by
/// the physical dispatch owner. Legacy/name-only locks return an explicit,
/// recoverable error; they never acquire a descriptor binding by inference.
pub fn authorize_locked_primitive<'a>(
    lock: &'a AppPackageLock,
    dependency_ref: &AppReference,
    descriptor: &AppPrimitiveDescriptor,
) -> Result<&'a AppLockedPrimitiveBinding, AppPackageLockError> {
    let dependency = lock
        .capability(dependency_ref)
        .ok_or_else(|| AppPackageLockError::LockedPrimitiveMissing(dependency_ref.to_string()))?;
    let binding = dependency.primitive_binding().ok_or_else(|| {
        AppPackageLockError::LockedPrimitiveBindingUnavailable(dependency_ref.to_string())
    })?;
    if dependency.content_digest() != binding.source_content_digest()
        || !binding.matches_descriptor(descriptor)?
    {
        return Err(AppPackageLockError::PrimitiveBindingMismatch(
            dependency_ref.to_string(),
        ));
    }
    Ok(binding)
}

/// Revalidate one durable reviewed interactive grant against the exact current
/// package lock and freshly resolved descriptor. Call this at launch, after
/// recovery/wait, and immediately before any physical-owner disclosure.
pub fn authorize_reviewed_interactive_capability<'a>(
    lock: &'a AppPackageLock,
    grant: &AppReviewedInteractiveCapabilityGrant,
    descriptor: &AppPrimitiveDescriptor,
) -> Result<&'a AppLockedPrimitiveBinding, AppPackageLockError> {
    grant
        .validate()
        .map_err(|error| AppPackageLockError::InvalidInteractiveBinding(error.to_string()))?;
    let primitive = authorize_locked_primitive(lock, grant.dependency_ref(), descriptor)?;
    let locked = lock
        .interactive_capability(grant.dependency_ref())
        .ok_or_else(|| {
            AppPackageLockError::InvalidInteractiveBinding(format!(
                "capability `{}` has no reviewed interactive request binding",
                grant.dependency_ref()
            ))
        })?;
    if locked.binding_digest() != grant.locked_binding_digest()
        || locked.primitive_binding_digest() != grant.primitive_binding_digest()
        || locked.request_digest() != grant.requested_request_digest()
    {
        return Err(AppPackageLockError::InvalidInteractiveBinding(
            "interactive lock/request identity changed after owner review".to_owned(),
        ));
    }
    let reproduced = locked.review_grant(primitive, grant.granted().clone())?;
    if &reproduced != grant {
        return Err(AppPackageLockError::InvalidInteractiveBinding(
            "interactive action/schema/effect/implementation/result contract changed".to_owned(),
        ));
    }
    Ok(primitive)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAppPackageLock {
    lock_version: u8,
    manifest_digest: AppDigest,
    bundle_digest: AppDigest,
    dependencies: Vec<PersistedAppLockedDependency>,
    #[serde(default)]
    recipe_bindings: Vec<AppLockedRecipeBinding>,
    #[serde(default)]
    contribution_bindings: Vec<AppLockedContributionPortBinding>,
    lock_digest: AppDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAppLockedDependency {
    kind: AppDependencyKind,
    dependency_ref: AppReference,
    semantic_version: String,
    content_digest: AppDigest,
    source: PersistedAppLockedDependencySource,
    #[serde(default)]
    primitive_binding: Option<AppLockedPrimitiveBinding>,
    #[serde(default)]
    interactive_binding: Option<AppLockedInteractiveCapabilityBinding>,
}

#[derive(Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
enum PersistedAppLockedDependencySource {
    RegistryRevision {
        immutable_revision_ref: AppReference,
        revision: AppRevision,
    },
    VendoredBundleMember {
        path: AppBundlePath,
    },
}

/// Decode only bytes read from the registry-owned dependency-lock column.
/// Portable archives use the same validator through a distinct claim type.
pub fn decode_persisted_package_lock(bytes: &[u8]) -> Result<AppPackageLock, AppPackageLockError> {
    validate_persisted_package_lock(decode_persisted_lock_claim(bytes)?)
}

/// Validate registry-owned historical bytes for settlement/closure only. This
/// returns no usable lock, recipe binding, or dispatch authority. The caller
/// must independently verify the accepted resource tree and package row.
pub(super) fn validate_accepted_cleanup_package_lock(
    bytes: &[u8],
    expected_lock_digest: &AppDigest,
    expected_bundle_digest: &AppDigest,
) -> Result<(), AppPackageLockError> {
    let lock = validate_persisted_package_lock_for(
        decode_persisted_lock_claim(bytes)?,
        RecipeLockValidation::AcceptedCleanupIdentity,
    )?;
    if lock.lock_digest() != expected_lock_digest || lock.bundle_digest() != expected_bundle_digest
    {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "historical lock does not match the accepted package identity".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum RecipeLockValidation {
    CurrentExecution,
    AcceptedCleanupIdentity,
}

fn decode_persisted_lock_claim(
    bytes: &[u8],
) -> Result<PersistedAppPackageLock, AppPackageLockError> {
    let contract_limits = AppContractLimits::default();
    if bytes.len() > contract_limits.max_document_bytes()
        || !json_bytes_nesting_is_bounded(bytes, contract_limits.max_json_depth())
        || !json_bytes_nodes_are_bounded(bytes, contract_limits.max_json_nodes())
    {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "encoded lock exceeds its byte, depth or node ceiling".to_owned(),
        ));
    }
    serde_json::from_slice(bytes)
        .map_err(|error| AppPackageLockError::InvalidPersistedLock(error.to_string()))
}

fn validate_persisted_package_lock(
    persisted: PersistedAppPackageLock,
) -> Result<AppPackageLock, AppPackageLockError> {
    validate_persisted_package_lock_for(persisted, RecipeLockValidation::CurrentExecution)
}

fn validate_persisted_package_lock_for(
    persisted: PersistedAppPackageLock,
    recipe_validation: RecipeLockValidation,
) -> Result<AppPackageLock, AppPackageLockError> {
    if !matches!(
        persisted.lock_version,
        LEGACY_APP_PACKAGE_LOCK_VERSION
            | DESCRIPTOR_APP_PACKAGE_LOCK_VERSION
            | PRE_IMPLEMENTATION_RECIPE_LOCK_VERSION
            | CONTRIBUTION_APP_PACKAGE_LOCK_VERSION
            | APP_PACKAGE_LOCK_VERSION
    ) {
        return Err(AppPackageLockError::InvalidPersistedLock(format!(
            "unsupported lock version {}",
            persisted.lock_version
        )));
    }
    let package_limits = AppPackageLimits::default();
    if persisted.dependencies.len() > package_limits.max_dependencies() {
        return Err(AppPackageLockError::ResolutionLimit {
            limit: package_limits.max_dependencies(),
        });
    }

    let mut dependencies = Vec::with_capacity(persisted.dependencies.len());
    for dependency in persisted.dependencies {
        if dependency.semantic_version.is_empty()
            || dependency.semantic_version.len() > package_limits.max_string_bytes()
        {
            return Err(AppPackageLockError::InvalidPersistedLock(format!(
                "dependency `{}` has an empty or oversized semantic version",
                dependency.dependency_ref
            )));
        }
        semver::Version::parse(&dependency.semantic_version).map_err(|error| {
            AppPackageLockError::InvalidResolvedVersion {
                dependency_ref: dependency.dependency_ref.to_string(),
                reason: error.to_string(),
            }
        })?;
        let source = match dependency.source {
            PersistedAppLockedDependencySource::RegistryRevision {
                immutable_revision_ref,
                revision,
            } => AppLockedDependencySource::RegistryRevision {
                immutable_revision_ref,
                revision,
            },
            PersistedAppLockedDependencySource::VendoredBundleMember { path } => {
                if dependency.kind != AppDependencyKind::ProcedureSkill {
                    return Err(AppPackageLockError::VendoringNotAllowed(
                        dependency.dependency_ref.to_string(),
                    ));
                }
                let Some(skill_name) = dependency.dependency_ref.as_str().strip_prefix("skill:")
                else {
                    return Err(AppPackageLockError::InvalidPersistedLock(format!(
                        "vendored dependency `{}` is not a skill reference",
                        dependency.dependency_ref
                    )));
                };
                if path.as_str() != format!("vendor/skills/{skill_name}/SKILL.md") {
                    return Err(AppPackageLockError::VendoredPathIdentityMismatch {
                        path: path.to_string(),
                        declared_name: skill_name.to_owned(),
                    });
                }
                AppLockedDependencySource::VendoredBundleMember { path }
            },
        };
        if dependency.kind != AppDependencyKind::Capability
            && dependency.primitive_binding.is_some()
        {
            return Err(AppPackageLockError::InvalidPersistedLock(
                "only capability dependencies may carry primitive bindings".to_owned(),
            ));
        }
        if persisted.lock_version == LEGACY_APP_PACKAGE_LOCK_VERSION
            && dependency.primitive_binding.is_some()
        {
            return Err(AppPackageLockError::InvalidPersistedLock(
                "legacy package locks cannot carry primitive bindings".to_owned(),
            ));
        }
        if let Some(binding) = dependency.primitive_binding.as_ref() {
            binding.validate()?;
            if binding.source_content_digest() != &dependency.content_digest {
                return Err(AppPackageLockError::InvalidPersistedLock(
                    "primitive source digest does not match the locked capability bytes".to_owned(),
                ));
            }
        }
        if dependency.interactive_binding.is_some()
            && (persisted.lock_version != APP_PACKAGE_LOCK_VERSION
                || dependency.kind != AppDependencyKind::Capability)
        {
            return Err(AppPackageLockError::InvalidPersistedLock(
                "only V5 capability locks may carry interactive request bindings".to_owned(),
            ));
        }
        if let Some(interactive) = dependency.interactive_binding.as_ref() {
            let primitive = dependency.primitive_binding.as_ref().ok_or_else(|| {
                AppPackageLockError::InvalidInteractiveBinding(
                    "interactive binding is missing its exact primitive binding".to_owned(),
                )
            })?;
            interactive.validate(&dependency.dependency_ref, primitive)?;
        }
        dependencies.push(AppLockedDependency {
            kind: dependency.kind,
            dependency_ref: dependency.dependency_ref,
            semantic_version: dependency.semantic_version,
            content_digest: dependency.content_digest,
            source,
            primitive_binding: dependency.primitive_binding,
            interactive_binding: dependency.interactive_binding,
        });
    }
    let keys = dependencies
        .iter()
        .map(|dependency| dependency_key(dependency.kind, &dependency.dependency_ref))
        .collect::<Vec<_>>();
    if keys.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "dependencies are duplicated or not in canonical order".to_owned(),
        ));
    }
    if !matches!(
        persisted.lock_version,
        CONTRIBUTION_APP_PACKAGE_LOCK_VERSION | APP_PACKAGE_LOCK_VERSION
    ) && !persisted.recipe_bindings.is_empty()
    {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "legacy package locks cannot carry recipe bindings".to_owned(),
        ));
    }
    for binding in &persisted.recipe_bindings {
        match recipe_validation {
            RecipeLockValidation::CurrentExecution => binding.validate()?,
            RecipeLockValidation::AcceptedCleanupIdentity => binding.validate_identity()?,
        }
    }
    if persisted
        .recipe_bindings
        .windows(2)
        .any(|pair| pair[0].workflow_id >= pair[1].workflow_id)
    {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "recipe bindings are duplicated or not in canonical order".to_owned(),
        ));
    }
    if !matches!(
        persisted.lock_version,
        CONTRIBUTION_APP_PACKAGE_LOCK_VERSION | APP_PACKAGE_LOCK_VERSION
    ) && !persisted.contribution_bindings.is_empty()
    {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "legacy package locks cannot carry contribution-port bindings".to_owned(),
        ));
    }
    if persisted.contribution_bindings.len() > AppContractLimits::default().max_collection_items() {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "contribution-port bindings exceed the collection ceiling".to_owned(),
        ));
    }
    for binding in &persisted.contribution_bindings {
        binding.validate()?;
    }
    if persisted.contribution_bindings.windows(2).any(|pair| {
        (&pair[0].workflow_id, &pair[0].port_id) >= (&pair[1].workflow_id, &pair[1].port_id)
    }) {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "contribution-port bindings are duplicated or not in canonical order".to_owned(),
        ));
    }
    let expected_digest = package_lock_digest(
        persisted.lock_version,
        &persisted.manifest_digest,
        &persisted.bundle_digest,
        &dependencies,
        &persisted.recipe_bindings,
        &persisted.contribution_bindings,
    )?;
    if persisted.lock_digest != expected_digest {
        return Err(AppPackageLockError::InvalidPersistedLock(
            "lock digest does not match the persisted dependency bytes".to_owned(),
        ));
    }
    Ok(AppPackageLock {
        lock_version: persisted.lock_version,
        manifest_digest: persisted.manifest_digest,
        bundle_digest: persisted.bundle_digest,
        dependencies,
        recipe_bindings: persisted.recipe_bindings,
        contribution_bindings: persisted.contribution_bindings,
        lock_digest: persisted.lock_digest,
    })
}

/// Resolve every declared dependency exactly once and produce an immutable
/// lock. A failure returns no partially usable lock value.
pub fn lock_app_package_dependencies(
    package: &AppPackageCandidate,
    registry_evidence: Vec<AppVerifiedRegistryDependency>,
    limits: &AppPackageLimits,
) -> Result<AppPackageLock, AppPackageLockError> {
    let requirements = package
        .manifest()
        .manifest()
        .dependency_requirements(limits)
        .map_err(|error| AppPackageLockError::InvalidManifest(error.to_string()))?;
    if registry_evidence.len() > limits.max_dependencies() {
        return Err(AppPackageLockError::ResolutionLimit {
            limit: limits.max_dependencies(),
        });
    }

    let mut registry_by_key = BTreeMap::new();
    for claim in registry_evidence {
        let key = dependency_key(claim.kind, &claim.dependency_ref);
        if registry_by_key.insert(key, claim).is_some() {
            return Err(AppPackageLockError::DuplicateResolution);
        }
    }

    let mut dependencies = Vec::with_capacity(requirements.len());
    for requirement in requirements {
        if let Some(path) = requirement.vendored_path {
            if requirement.kind != AppDependencyKind::ProcedureSkill {
                return Err(AppPackageLockError::VendoringNotAllowed(
                    requirement.dependency_ref.to_string(),
                ));
            }
            let registry_key = dependency_key(requirement.kind, &requirement.dependency_ref);
            if registry_by_key.contains_key(&registry_key) {
                return Err(AppPackageLockError::ConflictingResolution(
                    requirement.dependency_ref.to_string(),
                ));
            }
            let vendored_identity = read_vendored_skill_identity(package, &path)?;
            let declared_name = AppName::parse(vendored_identity.name).map_err(|error| {
                AppPackageLockError::InvalidVendoredManifest {
                    path: path.to_string(),
                    reason: error.to_string(),
                }
            })?;
            let declared_ref =
                AppReference::parse(format!("skill:{declared_name}")).map_err(|error| {
                    AppPackageLockError::InvalidVendoredManifest {
                        path: path.to_string(),
                        reason: error.to_string(),
                    }
                })?;
            let expected_path = format!("vendor/skills/{declared_name}/SKILL.md");
            if path.as_str() != expected_path {
                return Err(AppPackageLockError::VendoredPathIdentityMismatch {
                    path: path.to_string(),
                    declared_name: declared_name.to_string(),
                });
            }
            if normalized_collision_key(declared_ref.as_str())
                != normalized_collision_key(requirement.dependency_ref.as_str())
            {
                return Err(AppPackageLockError::VendoredIdentityMismatch {
                    expected: requirement.dependency_ref.to_string(),
                    declared: declared_ref.to_string(),
                });
            }
            let version = validate_version_match(
                &requirement.dependency_ref,
                &requirement.version_requirement,
                &vendored_identity.version,
                limits,
            )?;
            package
                .member(&path)
                .ok_or_else(|| AppPackageLockError::MissingVendoredMember(path.to_string()))?;
            let content_digest = vendored_dependency_digest(package, &path)?;
            dependencies.push(AppLockedDependency {
                kind: requirement.kind,
                dependency_ref: requirement.dependency_ref,
                semantic_version: version,
                content_digest,
                source: AppLockedDependencySource::VendoredBundleMember { path },
                primitive_binding: None,
                interactive_binding: None,
            });
        } else {
            let key = dependency_key(requirement.kind, &requirement.dependency_ref);
            let claim = registry_by_key.remove(&key).ok_or_else(|| {
                AppPackageLockError::MissingResolution(requirement.dependency_ref.to_string())
            })?;
            let version = validate_version_match(
                &requirement.dependency_ref,
                &requirement.version_requirement,
                &claim.semantic_version,
                limits,
            )?;
            dependencies.push(AppLockedDependency {
                kind: requirement.kind,
                dependency_ref: requirement.dependency_ref,
                semantic_version: version,
                content_digest: claim.content_digest,
                source: AppLockedDependencySource::RegistryRevision {
                    immutable_revision_ref: claim.immutable_revision_ref,
                    revision: claim.revision,
                },
                primitive_binding: claim.primitive_binding,
                interactive_binding: None,
            });
        }
    }

    if let Some(claim) = registry_by_key.into_values().next() {
        return Err(AppPackageLockError::UndeclaredResolution(
            claim.dependency_ref.to_string(),
        ));
    }
    attach_interactive_request_bindings(package, &mut dependencies)?;
    dependencies.sort_by(|left, right| {
        dependency_key(left.kind, &left.dependency_ref)
            .cmp(&dependency_key(right.kind, &right.dependency_ref))
    });
    let unique = dependencies
        .iter()
        .map(|dependency| dependency_key(dependency.kind, &dependency.dependency_ref))
        .collect::<BTreeSet<_>>();
    if unique.len() != dependencies.len() {
        return Err(AppPackageLockError::DuplicateResolution);
    }

    let recipe_bindings = compile_locked_recipe_bindings(package, &dependencies)?;
    let contribution_bindings = compile_locked_contribution_bindings(package)?;

    let manifest_digest = package.manifest().manifest_digest().clone();
    let bundle_digest = package.bundle_digest().clone();
    let lock_digest = package_lock_digest(
        APP_PACKAGE_LOCK_VERSION,
        &manifest_digest,
        &bundle_digest,
        &dependencies,
        &recipe_bindings,
        &contribution_bindings,
    )?;
    Ok(AppPackageLock {
        lock_version: APP_PACKAGE_LOCK_VERSION,
        manifest_digest,
        bundle_digest,
        dependencies,
        recipe_bindings,
        contribution_bindings,
        lock_digest,
    })
}

fn attach_interactive_request_bindings(
    package: &AppPackageCandidate,
    dependencies: &mut [AppLockedDependency],
) -> Result<(), AppPackageLockError> {
    let declared_tools = package
        .manifest()
        .manifest()
        .declared_tools()
        .map_err(|error| AppPackageLockError::InvalidManifest(error.to_string()))?;
    for dependency in dependencies
        .iter_mut()
        .filter(|dependency| dependency.kind == AppDependencyKind::Capability)
    {
        let name = dependency
            .dependency_ref
            .as_str()
            .strip_prefix("capability:")
            .ok_or_else(|| {
                AppPackageLockError::InvalidInteractiveBinding(
                    "capability dependency has an invalid reference".to_owned(),
                )
            })?;
        let declaration = declared_tools
            .iter()
            .find(|tool| {
                normalized_collision_key(tool.name.as_str()) == normalized_collision_key(name)
            })
            .ok_or_else(|| {
                AppPackageLockError::InvalidManifest(format!(
                    "locked capability `{}` has no exact manifest declaration",
                    dependency.dependency_ref
                ))
            })?;
        let Some(primitive) = dependency.primitive_binding.as_ref() else {
            if declaration.interactive.is_some() {
                return Err(AppPackageLockError::InvalidInteractiveBinding(format!(
                    "interactive request for `{}` has no immutable primitive binding",
                    dependency.dependency_ref
                )));
            }
            continue;
        };
        let Some(owner) = primitive.interactive_owner() else {
            if declaration.interactive.is_some() {
                return Err(AppPackageLockError::InvalidInteractiveBinding(format!(
                    "non-interactive dependency `{}` carries an interactive request",
                    dependency.dependency_ref
                )));
            }
            continue;
        };
        let exact_selected_leaf = declaration.actions.len() == 1
            && primitive.actions().len() == 1
            && (super::app_tool_bind::normalize_app_action_name(&declaration.actions[0])
                == super::app_tool_bind::normalize_app_action_name(primitive.actions()[0].name())
                || declaration.actions[0].trim() == primitive.actions()[0].action_ref().as_str());
        let legacy_snapshot = exact_selected_leaf && primitive.actions()[0].name() == "snapshot";
        let request = match &declaration.interactive {
            Some(request) => {
                if !exact_selected_leaf {
                    return Err(AppPackageLockError::InvalidInteractiveBinding(format!(
                        "interactive dependency `{}` must select one exact physical-owner action",
                        dependency.dependency_ref
                    )));
                }
                request.clone()
            },
            None if legacy_snapshot => {
                let result_ceiling = primitive.actions()[0]
                    .transport_result_byte_ceiling()
                    .ok_or_else(|| {
                        AppPackageLockError::InvalidInteractiveBinding(
                            "legacy snapshot action has no exact result ceiling".to_owned(),
                        )
                    })?;
                AppInteractiveCapabilityRequest::legacy_observe(owner, result_ceiling).map_err(
                    |error| AppPackageLockError::InvalidInteractiveBinding(error.to_string()),
                )?
            },
            None => {
                // Readable but inert legacy breadth. Installation review must
                // require republish/re-review; never infer all descriptor actions.
                continue;
            },
        };
        dependency.interactive_binding = Some(AppLockedInteractiveCapabilityBinding::seal(
            dependency.dependency_ref.clone(),
            owner,
            request,
            primitive,
        )?);
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn portable_test_lock(manifest_digest: AppDigest, bundle_digest: AppDigest) -> AppPackageLock {
    let dependencies = Vec::new();
    let recipe_bindings = Vec::new();
    let contribution_bindings = Vec::new();
    let lock_digest = package_lock_digest(
        APP_PACKAGE_LOCK_VERSION,
        &manifest_digest,
        &bundle_digest,
        &dependencies,
        &recipe_bindings,
        &contribution_bindings,
    )
    .expect("test package-lock identity is valid");
    AppPackageLock {
        lock_version: APP_PACKAGE_LOCK_VERSION,
        manifest_digest,
        bundle_digest,
        dependencies,
        recipe_bindings,
        contribution_bindings,
        lock_digest,
    }
}

fn package_lock_digest(
    lock_version: u8,
    manifest_digest: &AppDigest,
    bundle_digest: &AppDigest,
    dependencies: &[AppLockedDependency],
    recipe_bindings: &[AppLockedRecipeBinding],
    contribution_bindings: &[AppLockedContributionPortBinding],
) -> Result<AppDigest, AppPackageLockError> {
    if matches!(
        lock_version,
        LEGACY_APP_PACKAGE_LOCK_VERSION | DESCRIPTOR_APP_PACKAGE_LOCK_VERSION
    ) {
        #[derive(Serialize)]
        struct LegacyLockIdentity<'a> {
            lock_version: u8,
            manifest_digest: &'a AppDigest,
            bundle_digest: &'a AppDigest,
            dependencies: &'a [AppLockedDependency],
        }
        let bytes = serde_json::to_vec(&LegacyLockIdentity {
            lock_version,
            manifest_digest,
            bundle_digest,
            dependencies,
        })
        .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
        return Ok(AppDigest::blake3(&bytes));
    }
    #[derive(Serialize)]
    struct RecipeLockIdentity<'a> {
        lock_version: u8,
        manifest_digest: &'a AppDigest,
        bundle_digest: &'a AppDigest,
        dependencies: &'a [AppLockedDependency],
        recipe_bindings: &'a [AppLockedRecipeBinding],
    }
    let identity_bytes = if contribution_bindings.is_empty() {
        // Preserve the exact V4 identity of packages that do not opt into the
        // additive contribution-port feature.
        serde_json::to_vec(&RecipeLockIdentity {
            lock_version,
            manifest_digest,
            bundle_digest,
            dependencies,
            recipe_bindings,
        })
    } else {
        #[derive(Serialize)]
        struct ContributionLockIdentity<'a> {
            lock_version: u8,
            manifest_digest: &'a AppDigest,
            bundle_digest: &'a AppDigest,
            dependencies: &'a [AppLockedDependency],
            recipe_bindings: &'a [AppLockedRecipeBinding],
            contribution_bindings: &'a [AppLockedContributionPortBinding],
        }
        serde_json::to_vec(&ContributionLockIdentity {
            lock_version,
            manifest_digest,
            bundle_digest,
            dependencies,
            recipe_bindings,
            contribution_bindings,
        })
    }
    .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
    Ok(AppDigest::blake3(&identity_bytes))
}

fn compile_locked_recipe_bindings(
    package: &AppPackageCandidate,
    dependencies: &[AppLockedDependency],
) -> Result<Vec<AppLockedRecipeBinding>, AppPackageLockError> {
    let limits = AppContractLimits::default();
    let mut bindings = Vec::new();
    for (workflow_id, workflow) in &package.manifest().manifest().app.workflows {
        if workflow.runner != AppManifestRunner::Recipe {
            continue;
        }
        if !super::recipe_lowering::app_recipe_runner_ready() {
            return Err(AppPackageLockError::InvalidRecipe(
                "recipe runner is conditional and not admitted".to_owned(),
            ));
        }
        let member_path = workflow.recipe.as_ref().ok_or_else(|| {
            AppPackageLockError::InvalidRecipe(format!(
                "recipe workflow `{workflow_id}` has no immutable member"
            ))
        })?;
        let member = package.member(member_path).ok_or_else(|| {
            AppPackageLockError::InvalidRecipe(format!("recipe member `{member_path}` is absent"))
        })?;
        if member.bytes().len() > limits.max_document_bytes()
            || !json_bytes_nesting_is_bounded(member.bytes(), limits.max_json_depth())
            || !json_bytes_nodes_are_bounded(member.bytes(), limits.max_json_nodes())
        {
            return Err(AppPackageLockError::InvalidRecipe(format!(
                "recipe member `{member_path}` exceeds JSON admission bounds"
            )));
        }
        let source: super::recipe_ir::AppRecipeBundleSource =
            serde_json::from_slice(member.bytes()).map_err(|error| {
                AppPackageLockError::InvalidRecipe(format!(
                    "recipe member `{member_path}` is invalid: {error}"
                ))
            })?;
        let compiled = super::recipe_ir::compile_recipe_bundle(source)
            .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
        let root = &compiled.recipe().source().nodes[&compiled.recipe().source().root];
        let manifest = package.manifest().manifest();
        let mut bound_behaviors = 0usize;
        for (action_id, operations, steps) in manifest
            .app
            .behaviors
            .iter()
            .map(|item| (&item.action, &item.operations, &item.steps))
            .chain(
                manifest
                    .app
                    .event_behaviors
                    .iter()
                    .map(|item| (&item.action, &item.operations, &item.steps)),
            )
        {
            if !manifest
                .app
                .actions
                .get(action_id)
                .is_some_and(|action| &action.workflow == workflow_id)
            {
                continue;
            }
            bound_behaviors += 1;
            let matches = match &root.node {
                super::recipe_ir::AppRecipeNodeKind::ContextualRound { program, .. } => {
                    steps.len() == 1
                        && steps[0].id.as_str() == program.semantic_step
                        && steps[0].when.is_none()
                        && operations.as_slice() == [steps[0].operation.clone()]
                },
                _ => operations.is_empty() && steps.is_empty(),
            };
            if !matches {
                return Err(AppPackageLockError::InvalidRecipe(
                    "native recipe semantic steps do not match its reviewed behavior".to_owned(),
                ));
            }
        }
        match &root.node {
            super::recipe_ir::AppRecipeNodeKind::ContextualRound {
                source, program, ..
            } => {
                if bound_behaviors == 0 {
                    return Err(AppPackageLockError::InvalidRecipe(
                        "contextual round requires a reviewed behavior semantic step".to_owned(),
                    ));
                }
                let targets = program
                    .mutation_entities()
                    .into_iter()
                    .map(AppName::parse)
                    .collect::<Result<BTreeSet<_>, _>>()
                    .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
                if workflow.uses.iter().cloned().collect::<BTreeSet<_>>()
                    != BTreeSet::from([source.tool.clone()])
                    || workflow.may_mutate.iter().cloned().collect::<BTreeSet<_>>() != targets
                {
                    return Err(AppPackageLockError::InvalidRecipe(
                        "contextual round must bind exact source and mutation targets".to_owned(),
                    ));
                }
                let plan = super::app_tool_bind::plan_app_tool_call(
                    source.tool.as_str(),
                    Some(source.action.as_str()),
                    super::app_tool_bind::AppToolContainProfile::InProcessCompiled,
                );
                if plan.io_kind != super::app_tool_bind::AppToolIoKind::BoundHostRead
                    || !super::app_tool_bind::app_effect_owner_supported(&plan)
                {
                    return Err(AppPackageLockError::InvalidRecipe(
                        "round source is not an approved host read".to_owned(),
                    ));
                }
                let tool_ref = AppReference::parse(format!("capability:{}", source.tool))
                    .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
                let primitive = dependencies
                    .iter()
                    .find(|dependency| dependency.dependency_ref() == &tool_ref)
                    .and_then(AppLockedDependency::primitive_binding)
                    .ok_or_else(|| {
                        AppPackageLockError::InvalidRecipe(
                            "round source has no locked primitive".to_owned(),
                        )
                    })?;
                let action = primitive
                    .action_named(source.action.as_str())
                    .ok_or_else(|| {
                        AppPackageLockError::InvalidRecipe(
                            "round source action is not locked".to_owned(),
                        )
                    })?;
                if &source.primitive_ref != primitive.primitive_ref()
                    || &source.action_ref != action.action_ref()
                {
                    return Err(AppPackageLockError::InvalidRecipe(
                        "round source differs from locked authority".to_owned(),
                    ));
                }
            },
            super::recipe_ir::AppRecipeNodeKind::StoreTransaction { sources, program } => {
                let targets = program
                    .mutation_entities()
                    .into_iter()
                    .map(AppName::parse)
                    .collect::<Result<BTreeSet<_>, _>>()
                    .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
                if workflow.uses.iter().cloned().collect::<BTreeSet<_>>()
                    != sources.values().map(|source| source.tool.clone()).collect()
                    || workflow.may_mutate.iter().cloned().collect::<BTreeSet<_>>() != targets
                {
                    return Err(AppPackageLockError::InvalidRecipe(
                        "store transaction must bind exact source and mutation targets".to_owned(),
                    ));
                }
                for source in sources.values() {
                    let plan = super::app_tool_bind::plan_app_tool_call(
                        source.tool.as_str(),
                        Some(source.action.as_str()),
                        super::app_tool_bind::AppToolContainProfile::InProcessCompiled,
                    );
                    if plan.io_kind != super::app_tool_bind::AppToolIoKind::BoundHostRead
                        || !super::app_tool_bind::app_effect_owner_supported(&plan)
                    {
                        return Err(AppPackageLockError::InvalidRecipe(
                            "transaction source is not an approved host read".to_owned(),
                        ));
                    }
                    let tool_ref = AppReference::parse(format!("capability:{}", source.tool))
                        .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
                    let primitive = dependencies
                        .iter()
                        .find(|dependency| dependency.dependency_ref() == &tool_ref)
                        .and_then(AppLockedDependency::primitive_binding)
                        .ok_or_else(|| {
                            AppPackageLockError::InvalidRecipe(
                                "transaction source has no locked primitive".to_owned(),
                            )
                        })?;
                    let action =
                        primitive
                            .action_named(source.action.as_str())
                            .ok_or_else(|| {
                                AppPackageLockError::InvalidRecipe(
                                    "transaction source action is not locked".to_owned(),
                                )
                            })?;
                    if &source.primitive_ref != primitive.primitive_ref()
                        || &source.action_ref != action.action_ref()
                    {
                        return Err(AppPackageLockError::InvalidRecipe(
                            "transaction source differs from locked authority".to_owned(),
                        ));
                    }
                }
            },
            super::recipe_ir::AppRecipeNodeKind::Reconcile { declaration } => {
                let tools: BTreeSet<_> = workflow.uses.iter().cloned().collect();
                let targets: BTreeSet<_> = workflow.may_mutate.iter().cloned().collect();
                let source_tools: BTreeSet<_> = std::iter::once(declaration.tool.clone())
                    .chain(
                        declaration
                            .sources
                            .values()
                            .map(|source| source.tool.clone()),
                    )
                    .collect();
                if tools != source_tools || targets != declaration.entities() {
                    return Err(AppPackageLockError::InvalidRecipe(
                        "reconciliation must bind exact approved host reads and mutation targets"
                            .to_owned(),
                    ));
                }
                let reads = std::iter::once((
                    &declaration.tool,
                    &declaration.action,
                    root.authority.primitive_ref.as_ref(),
                    root.authority.action_ref.as_ref(),
                ))
                .chain(declaration.sources.values().map(|source| {
                    (
                        &source.tool,
                        &source.action,
                        Some(&source.primitive_ref),
                        Some(&source.action_ref),
                    )
                }));
                for (tool, action_name, primitive_ref, action_ref) in reads {
                    let plan = super::app_tool_bind::plan_app_tool_call(
                        tool.as_str(),
                        Some(action_name.as_str()),
                        super::app_tool_bind::AppToolContainProfile::InProcessCompiled,
                    );
                    if plan.io_kind != super::app_tool_bind::AppToolIoKind::BoundHostRead
                        || !super::app_tool_bind::app_effect_owner_supported(&plan)
                    {
                        return Err(AppPackageLockError::InvalidRecipe(
                            "reconciliation source is not an approved host read".to_owned(),
                        ));
                    }
                    let tool_ref = AppReference::parse(format!("capability:{tool}"))
                        .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
                    let primitive = dependencies
                        .iter()
                        .find(|dependency| dependency.dependency_ref() == &tool_ref)
                        .and_then(AppLockedDependency::primitive_binding)
                        .ok_or_else(|| {
                            AppPackageLockError::InvalidRecipe(
                                "reconciliation source has no locked primitive binding".to_owned(),
                            )
                        })?;
                    let action = primitive.action_named(action_name.as_str())
                    .ok_or_else(|| AppPackageLockError::InvalidRecipe(
                        "reconciliation source action is not selected in the dependency lock".to_owned(),
                    ))?;
                    if primitive_ref != Some(primitive.primitive_ref())
                        || action_ref != Some(action.action_ref())
                    {
                        return Err(AppPackageLockError::InvalidRecipe(
                        "reconciliation source authority differs from the locked primitive/action; refresh its reviewed catalog references".to_owned(),
                    ));
                    }
                }
            },
            _ if !workflow.uses.is_empty() || !workflow.may_mutate.is_empty() => {
                return Err(AppPackageLockError::InvalidRecipe(
                    "this recipe has no owner for declared tool/mutation authority".to_owned(),
                ));
            },
            _ => {},
        }
        // Action refs such as `<workflow>.input` and `<workflow>.result` are
        // manifest-facing aliases. Recipe bundle refs are deliberately
        // content-addressed (`workflow-schema:<digest>`), so comparing the two
        // namespaces would reject every valid compiled recipe. Bind the
        // manifest input contract to the recipe boundary by canonical schema
        // identity instead. Recipe output remains bundle-owned in V1 because
        // recipe workflows cannot declare a manifest output schema.
        let manifest_input_schema = workflow
            .input
            .compiled_value_schema()
            .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
        let recipe_input_schema = compiled
            .schema(&compiled.recipe().source().input_schema_ref)
            .ok_or_else(|| {
                AppPackageLockError::InvalidRecipe(format!(
                    "recipe workflow `{workflow_id}` has no compiled input boundary schema"
                ))
            })?;
        if manifest_input_schema.content_digest() != recipe_input_schema.content_digest() {
            return Err(AppPackageLockError::InvalidRecipe(format!(
                "recipe workflow `{workflow_id}` input schema does not match its manifest contract"
            )));
        }
        let schema_refs = compiled
            .schemas()
            .map(|schema| schema.schema_ref().clone())
            .collect::<BTreeSet<_>>();
        let schema_catalog = compiled.schemas().collect::<Vec<_>>();
        let compiled_plan_digest =
            super::recipe_lowering::compile_recipe_plan_digest(compiled.recipe(), &schema_catalog)
                .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
        let supported_node_set =
            super::recipe_lowering::recipe_contract_node_set(compiled.recipe())
                .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?;
        let supported_node_set_digest = AppDigest::blake3(
            &serde_json::to_vec(&supported_node_set)
                .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?,
        );
        let mut binding = AppLockedRecipeBinding {
            workflow_id: workflow_id.clone(),
            runner: workflow.runner,
            member_path: member_path.clone(),
            member_content_digest: member.content_digest().clone(),
            compiled_bundle_digest: compiled.bundle_digest().clone(),
            recipe_ref: compiled.recipe().recipe_ref().clone(),
            topology_revision: compiled.recipe().source().evolution.topology_revision,
            topology_digest: compiled.recipe().topology_digest().clone(),
            input_schema_ref: compiled.recipe().source().input_schema_ref.clone(),
            output_schema_ref: compiled.recipe().source().output.schema_ref.clone(),
            schema_refs,
            supported_node_set,
            supported_node_set_digest,
            compiled_plan_digest,
            implementation_digest:
                super::recipe_lowering::app_recipe_runtime_implementation_digest()
                    .map_err(|error| AppPackageLockError::InvalidRecipe(error.to_string()))?,
            binding_digest: AppDigest::blake3(b"pending-locked-recipe-binding"),
        };
        binding.binding_digest = locked_recipe_binding_digest(&binding)?;
        binding.validate()?;
        bindings.push(binding);
    }
    bindings.sort_by(|left, right| left.workflow_id.cmp(&right.workflow_id));
    Ok(bindings)
}

fn locked_recipe_binding_digest(
    binding: &AppLockedRecipeBinding,
) -> Result<AppDigest, AppPackageLockError> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": "magician.app-locked-recipe-binding.v2",
        "workflow_id": &binding.workflow_id,
        "runner": binding.runner,
        "member_path": &binding.member_path,
        "member_content_digest": &binding.member_content_digest,
        "compiled_bundle_digest": &binding.compiled_bundle_digest,
        "recipe_ref": &binding.recipe_ref,
        "topology_revision": binding.topology_revision,
        "topology_digest": &binding.topology_digest,
        "input_schema_ref": &binding.input_schema_ref,
        "output_schema_ref": &binding.output_schema_ref,
        "schema_refs": &binding.schema_refs,
        "supported_node_set": &binding.supported_node_set,
        "supported_node_set_digest": &binding.supported_node_set_digest,
        "compiled_plan_digest": &binding.compiled_plan_digest,
        "implementation_digest": &binding.implementation_digest,
    }))
    .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
    Ok(AppDigest::blake3(&bytes))
}

fn compile_locked_contribution_bindings(
    package: &AppPackageCandidate,
) -> Result<Vec<AppLockedContributionPortBinding>, AppPackageLockError> {
    let mut bindings = Vec::new();
    for (workflow_id, workflow) in &package.manifest().manifest().app.workflows {
        let workflow_declaration_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value(workflow)
                .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?,
        )
        .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
        let workflow_result_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value(&workflow.result)
                .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?,
        )
        .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
        let action_declaration_digests =
            package
                .manifest()
                .manifest()
                .app
                .actions
                .iter()
                .filter(|(_, action)| action.workflow == *workflow_id)
                .map(|(action_id, action)| {
                    AppDigest::blake3_canonical_json(&serde_json::to_value(action).map_err(
                        |error| AppPackageLockError::CanonicalEncoding(error.to_string()),
                    )?)
                    .map(|digest| (action_id.clone(), digest))
                    .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?;
        if !workflow.contribution_ports.is_empty() && action_declaration_digests.is_empty() {
            return Err(AppPackageLockError::InvalidContributionPort(format!(
                "workflow `{workflow_id}` has contribution ports but no exact action declaration"
            )));
        }
        for (port_id, declaration) in &workflow.contribution_ports {
            let mut binding = AppLockedContributionPortBinding {
                schema: "magician.app-locked-contribution-port.v1".to_owned(),
                workflow_id: workflow_id.clone(),
                port_id: port_id.clone(),
                declaration: declaration.clone(),
                destination_binding: AppContributionDestinationBinding::for_destination(
                    declaration.destination,
                ),
                workflow_declaration_digest: workflow_declaration_digest.clone(),
                workflow_result_digest: workflow_result_digest.clone(),
                action_declaration_digests: action_declaration_digests.clone(),
                binding_digest: AppDigest::blake3(b"pending-contribution-port-binding"),
            };
            binding.binding_digest = locked_contribution_port_digest(&binding)?;
            binding.validate()?;
            bindings.push(binding);
        }
    }
    bindings.sort_by(|left, right| {
        (&left.workflow_id, &left.port_id).cmp(&(&right.workflow_id, &right.port_id))
    });
    if bindings.len() > AppContractLimits::default().max_collection_items() {
        return Err(AppPackageLockError::ResolutionLimit {
            limit: AppContractLimits::default().max_collection_items(),
        });
    }
    Ok(bindings)
}

fn locked_contribution_port_digest(
    binding: &AppLockedContributionPortBinding,
) -> Result<AppDigest, AppPackageLockError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "schema": binding.schema,
        "workflow_id": binding.workflow_id,
        "port_id": binding.port_id,
        "declaration": binding.declaration,
        "destination_binding": binding.destination_binding,
        "workflow_declaration_digest": binding.workflow_declaration_digest,
        "workflow_result_digest": binding.workflow_result_digest,
        "action_declaration_digests": binding.action_declaration_digests,
    }))
    .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))
}

/// Bind a registry-backed procedure invocation to the exact immutable
/// revision and bytes captured by the package lock.
pub fn authorize_locked_registry_skill(
    lock: &AppPackageLock,
    dependency_ref: &AppReference,
    current: &AppVerifiedRegistryDependency,
) -> Result<AppLockedSkillExecutionFence, AppPackageLockError> {
    let locked = locked_procedure_dependency(lock, dependency_ref)?;
    let AppLockedDependencySource::RegistryRevision {
        immutable_revision_ref,
        revision,
    } = locked.source()
    else {
        return Err(AppPackageLockError::LockedSkillSourceMismatch(
            dependency_ref.to_string(),
        ));
    };
    if current.kind != AppDependencyKind::ProcedureSkill
        || current.dependency_ref != *locked.dependency_ref()
        || current.semantic_version != locked.semantic_version()
        || current.content_digest != *locked.content_digest()
        || current.immutable_revision_ref != *immutable_revision_ref
        || current.revision != *revision
    {
        return Err(AppPackageLockError::LockedSkillIdentityMismatch(
            dependency_ref.to_string(),
        ));
    }
    Ok(skill_execution_fence(lock, locked))
}

/// Bind a vendored procedure invocation to the exact admitted package subtree.
/// A global skill with the same name is never consulted.
pub fn authorize_locked_vendored_skill(
    lock: &AppPackageLock,
    package: &AppPackageCandidate,
    dependency_ref: &AppReference,
) -> Result<AppLockedSkillExecutionFence, AppPackageLockError> {
    if lock.manifest_digest() != package.manifest().manifest_digest()
        || lock.bundle_digest() != package.bundle_digest()
    {
        return Err(AppPackageLockError::LockedSkillPackageMismatch);
    }
    let locked = locked_procedure_dependency(lock, dependency_ref)?;
    let AppLockedDependencySource::VendoredBundleMember { path } = locked.source() else {
        return Err(AppPackageLockError::LockedSkillSourceMismatch(
            dependency_ref.to_string(),
        ));
    };
    let current_digest = vendored_dependency_digest(package, path)?;
    if current_digest != *locked.content_digest() {
        return Err(AppPackageLockError::LockedSkillIdentityMismatch(
            dependency_ref.to_string(),
        ));
    }
    Ok(skill_execution_fence(lock, locked))
}

fn locked_procedure_dependency<'a>(
    lock: &'a AppPackageLock,
    dependency_ref: &AppReference,
) -> Result<&'a AppLockedDependency, AppPackageLockError> {
    let dependency = lock
        .dependencies()
        .iter()
        .find(|dependency| dependency.dependency_ref() == dependency_ref)
        .ok_or_else(|| AppPackageLockError::LockedSkillMissing(dependency_ref.to_string()))?;
    if dependency.kind() != AppDependencyKind::ProcedureSkill {
        return Err(AppPackageLockError::LockedDependencyIsNotSkill(
            dependency_ref.to_string(),
        ));
    }
    Ok(dependency)
}

fn skill_execution_fence(
    lock: &AppPackageLock,
    dependency: &AppLockedDependency,
) -> AppLockedSkillExecutionFence {
    AppLockedSkillExecutionFence {
        package_lock_digest: lock.lock_digest().clone(),
        dependency_ref: dependency.dependency_ref().clone(),
        semantic_version: dependency.semantic_version().to_owned(),
        content_digest: dependency.content_digest().clone(),
        source: dependency.source().clone(),
    }
}

fn dependency_key(kind: AppDependencyKind, dependency_ref: &AppReference) -> (u8, String) {
    let kind = match kind {
        AppDependencyKind::Contract => 0,
        AppDependencyKind::ProcedureSkill => 1,
        AppDependencyKind::Capability => 2,
    };
    (kind, normalized_collision_key(dependency_ref.as_str()))
}

#[derive(Deserialize)]
struct VendoredSkillIdentity {
    name: String,
    version: String,
}

fn read_vendored_skill_identity(
    package: &AppPackageCandidate,
    skill_manifest_path: &AppBundlePath,
) -> Result<VendoredSkillIdentity, AppPackageLockError> {
    let member = package.member(skill_manifest_path).ok_or_else(|| {
        AppPackageLockError::MissingVendoredMember(skill_manifest_path.to_string())
    })?;
    let source = std::str::from_utf8(member.bytes()).map_err(|error| {
        AppPackageLockError::InvalidVendoredManifest {
            path: skill_manifest_path.to_string(),
            reason: format!("SKILL.md is not UTF-8: {error}"),
        }
    })?;
    tool_runtime_core::manifest_parser::parse_skill_frontmatter(source).map_err(|error| {
        AppPackageLockError::InvalidVendoredManifest {
            path: skill_manifest_path.to_string(),
            reason: error.to_string(),
        }
    })
}

fn vendored_dependency_digest(
    package: &AppPackageCandidate,
    skill_manifest_path: &AppBundlePath,
) -> Result<AppDigest, AppPackageLockError> {
    let root = skill_manifest_path
        .as_str()
        .strip_suffix("SKILL.md")
        .ok_or_else(|| AppPackageLockError::InvalidVendoredPath(skill_manifest_path.to_string()))?;
    #[derive(Serialize)]
    struct VendoredIdentity<'a> {
        version: u8,
        root: &'a str,
        members: Vec<VendoredMemberIdentity<'a>>,
    }
    #[derive(Serialize)]
    struct VendoredMemberIdentity<'a> {
        path: &'a AppBundlePath,
        content_digest: &'a AppDigest,
    }
    let members = package
        .members()
        .iter()
        .filter(|member| member.path().as_str().starts_with(root))
        .map(|member| VendoredMemberIdentity {
            path: member.path(),
            content_digest: member.content_digest(),
        })
        .collect::<Vec<_>>();
    if members.is_empty() {
        return Err(AppPackageLockError::MissingVendoredMember(
            skill_manifest_path.to_string(),
        ));
    }
    let encoded = serde_json::to_vec(&VendoredIdentity {
        version: 1,
        root,
        members,
    })
    .map_err(|error| AppPackageLockError::CanonicalEncoding(error.to_string()))?;
    Ok(AppDigest::blake3(&encoded))
}

fn validate_version_match(
    dependency_ref: &AppReference,
    requirement: &str,
    version: &str,
    limits: &AppPackageLimits,
) -> Result<String, AppPackageLockError> {
    if version.is_empty() || version.len() > limits.max_string_bytes() {
        return Err(AppPackageLockError::InvalidResolvedVersion {
            dependency_ref: dependency_ref.to_string(),
            reason: "version is empty or exceeds the string ceiling".to_owned(),
        });
    }
    let requirement = semver::VersionReq::parse(requirement)
        .map_err(|error| AppPackageLockError::InvalidManifest(error.to_string()))?;
    let version = semver::Version::parse(version).map_err(|error| {
        AppPackageLockError::InvalidResolvedVersion {
            dependency_ref: dependency_ref.to_string(),
            reason: error.to_string(),
        }
    })?;
    if !requirement.matches(&version) {
        return Err(AppPackageLockError::VersionMismatch {
            dependency_ref: dependency_ref.to_string(),
            requirement: requirement.to_string(),
            resolved: version.to_string(),
        });
    }
    Ok(version.to_string())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppPackageLockError {
    #[error("invalid app manifest dependency contract: {0}")]
    InvalidManifest(String),
    #[error("invalid immutable app recipe contract: {0}")]
    InvalidRecipe(String),
    #[error("invalid immutable app contribution-port contract: {0}")]
    InvalidContributionPort(String),
    #[error("invalid immutable interactive capability binding: {0}")]
    InvalidInteractiveBinding(String),
    #[error("dependency resolution set exceeds the {limit} item ceiling")]
    ResolutionLimit { limit: usize },
    #[error("dependency resolution contains a duplicate normalized identity")]
    DuplicateResolution,
    #[error("missing immutable resolution for dependency `{0}`")]
    MissingResolution(String),
    #[error("resolution was supplied for undeclared dependency `{0}`")]
    UndeclaredResolution(String),
    #[error("dependency `{0}` has both vendored and registry resolutions")]
    ConflictingResolution(String),
    #[error("dependency `{0}` cannot be vendored")]
    VendoringNotAllowed(String),
    #[error("vendored dependency member `{0}` is absent from the validated bundle")]
    MissingVendoredMember(String),
    #[error("vendored dependency path `{0}` does not identify a SKILL.md member")]
    InvalidVendoredPath(String),
    #[error("vendored dependency manifest `{path}` is invalid: {reason}")]
    InvalidVendoredManifest { path: String, reason: String },
    #[error("vendored dependency expected `{expected}` but its SKILL.md declares `{declared}`")]
    VendoredIdentityMismatch { expected: String, declared: String },
    #[error("vendored dependency path `{path}` does not match declared skill `{declared_name}`")]
    VendoredPathIdentityMismatch { path: String, declared_name: String },
    #[error("dependency `{dependency_ref}` has invalid resolved version: {reason}")]
    InvalidResolvedVersion {
        dependency_ref: String,
        reason: String,
    },
    #[error("dependency `{dependency_ref}` requires `{requirement}` but resolved `{resolved}`")]
    VersionMismatch {
        dependency_ref: String,
        requirement: String,
        resolved: String,
    },
    #[error("failed to encode canonical dependency lock: {0}")]
    CanonicalEncoding(String),
    #[error("persisted dependency lock is invalid: {0}")]
    InvalidPersistedLock(String),
    #[error("package lock has no procedure dependency `{0}`")]
    LockedSkillMissing(String),
    #[error("locked dependency `{0}` is not a procedure skill")]
    LockedDependencyIsNotSkill(String),
    #[error("locked procedure dependency `{0}` resolved through a different source class")]
    LockedSkillSourceMismatch(String),
    #[error("locked procedure dependency `{0}` no longer has the exact immutable identity")]
    LockedSkillIdentityMismatch(String),
    #[error("vendored procedure dependency was presented with a different admitted package")]
    LockedSkillPackageMismatch,
    #[error("package lock has no capability dependency `{0}`")]
    LockedPrimitiveMissing(String),
    #[error(
        "capability `{0}` was locked without descriptor/action evidence; republish the package"
    )]
    LockedPrimitiveBindingUnavailable(String),
    #[error("capability `{0}` no longer matches its locked primitive descriptor/action identity")]
    PrimitiveBindingMismatch(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) mod tests {
    use super::*;
    use crate::magician_v2::apps::manifest::{
        build_app_package_candidate,
        tests::{contribution_port_bundle, valid_bundle, valid_skill_document},
        AppBundleMember,
    };
    use crate::magician_v2::apps::{
        models::{AppDataClassification, AppModelProcessing},
        recipe_ir::{
            compile_workflow_value_schema, AppRecipeAuthorityContract, AppRecipeBundleSource,
            AppRecipeBundleVersion, AppRecipeCancellationContract, AppRecipeCancellationMode,
            AppRecipeEffectClass, AppRecipeEffectContract, AppRecipeEvolutionContract,
            AppRecipeGraphCeiling, AppRecipeIdempotencyContract, AppRecipeIrSource,
            AppRecipeMigrationPolicy, AppRecipeNode, AppRecipeNodeKind,
            AppRecipeNodeResourceCeiling, AppRecipeOutputAuthority, AppRecipeOutputContract,
            AppRecipeOutputKind, AppRecipeProvenanceJoin, AppRecipeRetryContract,
            AppRecipeUncertaintyContract, AppRecipeVersion, AppWorkflowHandlingFloor,
            AppWorkflowRecordField, AppWorkflowValueSchemaSource, AppWorkflowValueSchemaVersion,
            AppWorkflowValueTypeNode,
        },
    };

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).expect("valid reference")
    }

    fn evidence(
        kind: AppDependencyKind,
        dependency_ref: &str,
        semantic_version: &str,
        immutable_revision_ref: &str,
        revision: u64,
        bytes: &[u8],
    ) -> AppVerifiedRegistryDependency {
        AppVerifiedRegistryDependency::from_trusted_registry_bytes(
            kind,
            reference(dependency_ref),
            semantic_version.to_owned(),
            reference(immutable_revision_ref),
            AppRevision::new(revision).expect("positive revision"),
            bytes,
        )
        .expect("verified evidence")
    }

    fn registry_evidence() -> Vec<AppVerifiedRegistryDependency> {
        vec![
            evidence(
                AppDependencyKind::Contract,
                "contract:magician_contract",
                "1.2.0",
                "contract-revision:42",
                42,
                b"contract-v1.2.0",
            ),
            evidence(
                AppDependencyKind::Capability,
                "capability:content_search",
                "1.4.3",
                "capability-revision:99",
                99,
                b"content-search-v1.4.3",
            ),
        ]
    }

    fn recipe_input_schema(max_text_bytes: u32) -> AppWorkflowValueSchemaSource {
        AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 1,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Public,
                model_processing: AppModelProcessing::RemoteAllowed,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Text {
                    max_bytes: max_text_bytes,
                },
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([(
                        AppName::parse("topic").unwrap(),
                        AppWorkflowRecordField {
                            value_type: 0,
                            required: true,
                        },
                    )]),
                },
            ],
        }
    }

    fn validate_recipe_bundle(schema_source: AppWorkflowValueSchemaSource) -> Vec<u8> {
        let compiled_schema = compile_workflow_value_schema(schema_source.clone()).unwrap();
        let output = AppRecipeOutputContract {
            kind: AppRecipeOutputKind::TypedValue,
            schema_ref: compiled_schema.schema_ref().clone(),
            authority: AppRecipeOutputAuthority::Derived,
        };
        let root = AppName::parse("root").unwrap();
        serde_json::to_vec(&AppRecipeBundleSource {
            version: AppRecipeBundleVersion::V1,
            schemas: BTreeMap::from([(AppName::parse("input").unwrap(), schema_source)]),
            recipe: AppRecipeIrSource {
                version: AppRecipeVersion::V1,
                input_schema_ref: compiled_schema.schema_ref().clone(),
                output: output.clone(),
                root: root.clone(),
                nodes: BTreeMap::from([(
                    root,
                    AppRecipeNode {
                        input_schema_ref: compiled_schema.schema_ref().clone(),
                        output,
                        node: AppRecipeNodeKind::Validate,
                        effect: AppRecipeEffectContract {
                            class: AppRecipeEffectClass::None,
                            idempotency: AppRecipeIdempotencyContract::NotApplicable,
                            uncertainty: AppRecipeUncertaintyContract::Impossible,
                        },
                        authority: AppRecipeAuthorityContract {
                            primitive_ref: None,
                            action_ref: None,
                            target_app_ref: None,
                            required_grant_refs: BTreeSet::new(),
                            resource_scope_refs: BTreeSet::new(),
                        },
                        resources: AppRecipeNodeResourceCeiling {
                            max_active_millis: 1_000,
                            max_input_bytes: 4_096,
                            max_output_bytes: 4_096,
                            max_cost_microusd: 0,
                            max_tool_calls: 0,
                            max_parallelism: 1,
                        },
                        retry: AppRecipeRetryContract::None,
                        cancellation: AppRecipeCancellationContract {
                            mode: AppRecipeCancellationMode::Propagate,
                            acknowledgement_timeout_millis: 100,
                        },
                        provenance_join: AppRecipeProvenanceJoin::Preserve,
                    },
                )]),
                ceilings: AppRecipeGraphCeiling {
                    max_nodes: 1,
                    max_edges: 1,
                    max_depth: 1,
                    max_fan_out: 1,
                    max_parallelism: 1,
                    max_payload_bytes: 8_192,
                    max_active_millis: 1_000,
                    max_cost_microusd: 0,
                    max_tool_calls: 0,
                },
                evolution: AppRecipeEvolutionContract {
                    topology_revision: AppRevision::new(1).unwrap(),
                    migration: AppRecipeMigrationPolicy::RecompileRequired,
                    predecessor_recipe_ref: None,
                },
            },
        })
        .unwrap()
    }

    fn recipe_package(schema_source: AppWorkflowValueSchemaSource) -> AppPackageCandidate {
        let original = valid_skill_document();
        let manifest = original.replace(
            r#"    build:
      prompt: workflows/build.md
      runner: auto
      uses: [content_search]
      procedures: [skill:summarize]
      input:
        type: object
        fields:
          topic: { type: text, required: true }
      result:
        kind: entity_projection
        entities: [plan]
      may_mutate: [plan]
      trigger: user"#,
            r#"    build:
      prompt: workflows/build.md
      runner: recipe
      recipe: workflows/build.recipe.json
      input:
        type: object
        fields:
          topic: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
      trigger: user
    lookup:
      prompt: workflows/build.md
      runner: auto
      uses: [content_search]
      procedures: [skill:summarize]
      input:
        type: object
        fields:
          topic: { type: text, required: true }
      result:
        kind: entity_projection
        entities: [plan]
      may_mutate: [plan]
      trigger: user"#,
        );
        assert_ne!(
            manifest, original,
            "recipe manifest fixture rewrite drifted"
        );
        let mut bundle = valid_bundle();
        bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap()
            .bytes = manifest.into_bytes();
        bundle.push(
            AppBundleMember::regular_file(
                "workflows/build.recipe.json",
                validate_recipe_bundle(schema_source),
            )
            .unwrap(),
        );
        build_app_package_candidate(bundle, &AppPackageLimits::default()).unwrap()
    }

    fn package() -> AppPackageCandidate {
        build_app_package_candidate(valid_bundle(), &AppPackageLimits::default())
            .expect("valid package")
    }

    pub(crate) fn locked_native_recipe_fixture() -> (AppPackageCandidate, AppPackageLock) {
        let candidate = recipe_package(recipe_input_schema(262_144));
        let lock = lock_app_package_dependencies(
            &candidate,
            registry_evidence(),
            &AppPackageLimits::default(),
        )
        .unwrap();
        (candidate, lock)
    }

    #[test]
    fn recipe_lock_binds_action_aliases_by_canonical_input_schema_identity() {
        let limits = AppPackageLimits::default();
        let package = recipe_package(recipe_input_schema(262_144));
        let manifest = package.manifest().manifest();
        let action = &manifest.app.actions[&AppName::parse("create").unwrap()];
        let lock = lock_app_package_dependencies(&package, registry_evidence(), &limits)
            .expect("matching manifest and recipe schemas must lock");
        let binding = lock
            .recipe_for_workflow(&AppName::parse("build").unwrap())
            .expect("recipe binding");

        assert_eq!(action.input_from.as_str(), "build.input");
        assert_eq!(action.result_from.as_str(), "build.result");
        assert_ne!(&action.input_from, binding.input_schema_ref());
        assert_ne!(&action.result_from, binding.output_schema_ref());
        assert!(binding
            .input_schema_ref()
            .as_str()
            .starts_with("workflow-schema:blake3:"));

        let mismatched = recipe_package(recipe_input_schema(262_143));
        let error = lock_app_package_dependencies(&mismatched, registry_evidence(), &limits)
            .expect_err("schema substitution must fail package locking");
        assert!(matches!(
            error,
            AppPackageLockError::InvalidRecipe(message)
                if message.contains("input schema does not match its manifest contract")
        ));
    }

    fn locked_interactive_primitive(owner: AppInteractiveOwnerKind) -> AppLockedPrimitiveBinding {
        let action = AppLockedPrimitiveActionBinding {
            action_ref: reference("primitive-action:browser-snapshot"),
            name: "snapshot".to_owned(),
            action_digest: AppDigest::blake3(b"snapshot-action"),
            input_schema_digest: Some(AppDigest::blake3(b"snapshot-input")),
            result_schema_digest: Some(AppDigest::blake3(b"snapshot-result")),
            effects: BTreeSet::from([AppPrimitiveEffect::HostRead]),
            implementation_plan_digest: Some(AppDigest::blake3(b"snapshot-implementation")),
            physical_artifact_revision_ref: None,
            physical_artifact_digest: None,
            transport_result_byte_ceiling: Some(4096),
            dispatchable: true,
        };
        let primitive_ref = reference("primitive:tool-skill:browser:fixture");
        let descriptor_digest = AppDigest::blake3(b"descriptor");
        let source_content_digest = AppDigest::blake3(b"source");
        let actions = vec![action];
        let binding_digest = primitive_binding_digest(
            &primitive_ref,
            &descriptor_digest,
            &source_content_digest,
            Some(owner),
            &actions,
        )
        .unwrap();
        AppLockedPrimitiveBinding {
            primitive_ref,
            descriptor_digest,
            source_content_digest,
            interactive_owner: Some(owner),
            actions,
            binding_digest,
        }
    }

    #[test]
    fn interactive_lock_binds_request_effect_schema_implementation_and_result_ceiling() {
        let dependency_ref = reference("capability:browser");
        let primitive = locked_interactive_primitive(AppInteractiveOwnerKind::Browser);
        let request =
            AppInteractiveCapabilityRequest::legacy_observe(AppInteractiveOwnerKind::Browser, 4096)
                .unwrap();
        let binding = AppLockedInteractiveCapabilityBinding::seal(
            dependency_ref.clone(),
            AppInteractiveOwnerKind::Browser,
            request,
            &primitive,
        )
        .unwrap();
        binding.validate(&dependency_ref, &primitive).unwrap();

        let mut effect_substitution = primitive.clone();
        effect_substitution.actions[0].effects = BTreeSet::from([AppPrimitiveEffect::NetworkRead]);
        effect_substitution.binding_digest = primitive_binding_digest(
            &effect_substitution.primitive_ref,
            &effect_substitution.descriptor_digest,
            &effect_substitution.source_content_digest,
            effect_substitution.interactive_owner,
            &effect_substitution.actions,
        )
        .unwrap();
        assert!(binding
            .validate(&dependency_ref, &effect_substitution)
            .is_err());

        let mut implementation_substitution = primitive.clone();
        implementation_substitution.actions[0].implementation_plan_digest =
            Some(AppDigest::blake3(b"other-implementation"));
        implementation_substitution.binding_digest = primitive_binding_digest(
            &implementation_substitution.primitive_ref,
            &implementation_substitution.descriptor_digest,
            &implementation_substitution.source_content_digest,
            implementation_substitution.interactive_owner,
            &implementation_substitution.actions,
        )
        .unwrap();
        assert!(binding
            .validate(&dependency_ref, &implementation_substitution)
            .is_err());

        let mut result_substitution = primitive.clone();
        result_substitution.actions[0].transport_result_byte_ceiling = Some(2048);
        result_substitution.binding_digest = primitive_binding_digest(
            &result_substitution.primitive_ref,
            &result_substitution.descriptor_digest,
            &result_substitution.source_content_digest,
            result_substitution.interactive_owner,
            &result_substitution.actions,
        )
        .unwrap();
        assert!(binding
            .validate(&dependency_ref, &result_substitution)
            .is_err());
    }

    #[test]
    fn catalog_snapshot_locks_a_declared_tool_by_real_name() {
        use crate::magician_v2::apps::{
            tool_catalog::{snapshot_reviewed_tool, AppReviewedToolEntry},
            tool_eligibility::typed_app_tool_document,
        };

        let package = package();
        let entry = AppReviewedToolEntry {
            skill_document: typed_app_tool_document(
                "content_search",
                "1.4.3",
                "    expose:\n      apps: true\n",
            ),
            immutable_revision_ref: reference("catalog-revision:content-search"),
            revision: AppRevision::new(1).expect("revision"),
        };
        let tool = snapshot_reviewed_tool("content_search", &entry).expect("catalog snapshot");
        let lock = lock_app_package_dependencies(
            &package,
            vec![
                evidence(
                    AppDependencyKind::Contract,
                    "contract:magician_contract",
                    "1.2.0",
                    "contract-revision:42",
                    42,
                    b"contract-v1.2.0",
                ),
                tool,
            ],
            &AppPackageLimits::default(),
        )
        .expect("catalog-backed lock");
        let locked = lock
            .dependencies()
            .iter()
            .find(|dependency| dependency.dependency_ref().as_str() == "capability:content_search")
            .expect("internal capability identity");
        assert_eq!(locked.semantic_version(), "1.4.3");
        assert_eq!(
            locked.source(),
            &AppLockedDependencySource::RegistryRevision {
                immutable_revision_ref: reference("catalog-revision:content-search"),
                revision: AppRevision::new(1).expect("revision"),
            }
        );
    }

    #[test]
    fn complete_resolution_produces_a_stable_immutable_lock() {
        let limits = AppPackageLimits::default();
        let package = package();
        let first = lock_app_package_dependencies(&package, registry_evidence(), &limits)
            .expect("first lock");
        let mut reversed = registry_evidence();
        reversed.reverse();
        let second =
            lock_app_package_dependencies(&package, reversed, &limits).expect("second lock");

        assert_eq!(first, second);
        assert_eq!(first.lock_version(), APP_PACKAGE_LOCK_VERSION);
        assert_eq!(first.dependencies().len(), 3);
        assert_eq!(
            first.manifest_digest(),
            package.manifest().manifest_digest()
        );
        assert_eq!(first.bundle_digest(), package.bundle_digest());
        assert_eq!(
            first.manifest_digest().as_str(),
            "blake3:b27280ae62f316bdbcec4832383a8fe4693365a6170d1b7a1f52dc1d0cc1d6fd"
        );
        assert_eq!(
            first.bundle_digest().as_str(),
            "blake3:d2178ce276bd3b772579f3b63eb07948575203b21bf9c88d55091a81bf3675c8"
        );
        assert_eq!(
            first.lock_digest().as_str(),
            "blake3:ec028d1169306a5b07428b8658dd14653ff4cbc52deba0e5380ec9db17e7eb90"
        );
    }

    #[test]
    fn registry_owned_lock_decode_round_trips_and_rejects_tampering() {
        let lock = lock_app_package_dependencies(
            &package(),
            registry_evidence(),
            &AppPackageLimits::default(),
        )
        .expect("lock");
        let bytes = serde_json::to_vec(&lock).expect("encode lock");
        assert_eq!(decode_persisted_package_lock(&bytes).unwrap(), lock);

        let mut tampered: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        tampered["lock_digest"] =
            serde_json::to_value(AppDigest::blake3(b"different lock")).unwrap();
        assert!(matches!(
            decode_persisted_package_lock(&serde_json::to_vec(&tampered).unwrap()),
            Err(AppPackageLockError::InvalidPersistedLock(_))
        ));

        let mut reordered: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        reordered["dependencies"].as_array_mut().unwrap().reverse();
        assert!(matches!(
            decode_persisted_package_lock(&serde_json::to_vec(&reordered).unwrap()),
            Err(AppPackageLockError::InvalidPersistedLock(_))
        ));
    }

    #[test]
    fn accepted_cleanup_validates_historical_identity_without_authorizing_execution() {
        let mut lock = lock_app_package_dependencies(
            &recipe_package(recipe_input_schema(262_144)),
            registry_evidence(),
            &AppPackageLimits::default(),
        )
        .expect("recipe lock");
        let binding = &mut lock.recipe_bindings[0];
        binding.implementation_digest = AppDigest::blake3(b"previous reviewed implementation");
        binding.binding_digest = locked_recipe_binding_digest(binding).unwrap();
        lock.lock_digest = package_lock_digest(
            lock.lock_version,
            &lock.manifest_digest,
            &lock.bundle_digest,
            &lock.dependencies,
            &lock.recipe_bindings,
            &lock.contribution_bindings,
        )
        .unwrap();
        let bytes = serde_json::to_vec(&lock).unwrap();
        assert!(decode_persisted_package_lock(&bytes).is_err());
        assert!(serde_json::from_slice::<AppPortablePackageLockClaim>(&bytes).is_err());
        validate_accepted_cleanup_package_lock(&bytes, lock.lock_digest(), lock.bundle_digest())
            .expect("accepted cleanup survives an implementation upgrade");
        assert!(validate_accepted_cleanup_package_lock(
            &bytes,
            &AppDigest::blake3(b"another accepted package"),
            lock.bundle_digest(),
        )
        .is_err());

        // Even a recomputed outer digest cannot bless a corrupt inner binding.
        lock.recipe_bindings[0].implementation_digest = AppDigest::blake3(b"tampered binding");
        lock.lock_digest = package_lock_digest(
            lock.lock_version,
            &lock.manifest_digest,
            &lock.bundle_digest,
            &lock.dependencies,
            &lock.recipe_bindings,
            &lock.contribution_bindings,
        )
        .unwrap();
        assert!(validate_accepted_cleanup_package_lock(
            &serde_json::to_vec(&lock).unwrap(),
            lock.lock_digest(),
            lock.bundle_digest(),
        )
        .is_err());
    }

    #[test]
    fn contribution_ports_are_immutable_lock_members_and_fail_on_substitution() {
        let package =
            build_app_package_candidate(contribution_port_bundle(), &AppPackageLimits::default())
                .expect("package with contribution port");
        let lock = lock_app_package_dependencies(
            &package,
            registry_evidence(),
            &AppPackageLimits::default(),
        )
        .expect("locked contribution declaration");
        let binding = lock
            .contribution_port(
                &AppName::parse("build").unwrap(),
                &AppName::parse("summary_memory").unwrap(),
            )
            .expect("workflow-local contribution binding");
        assert_eq!(
            binding.declaration().destination,
            super::super::records::AppContributionDestination::Memory
        );

        let mut substituted = serde_json::to_value(&lock).unwrap();
        substituted["contribution_bindings"][0]["declaration"]["maximum_retention_seconds"] =
            serde_json::json!(1);
        assert!(matches!(
            decode_persisted_package_lock(&serde_json::to_vec(&substituted).unwrap()),
            Err(AppPackageLockError::InvalidContributionPort(_))
                | Err(AppPackageLockError::InvalidPersistedLock(_))
        ));
    }

    #[test]
    fn verified_registry_evidence_cannot_be_deserialized_from_transport() {
        static_assertions::assert_not_impl_any!(
            AppVerifiedRegistryDependency: serde::de::DeserializeOwned
        );
    }

    #[test]
    fn registry_evidence_binds_digest_to_bytes_and_rejects_bad_versions() {
        let limits = AppPackageLimits::default();
        let proof = evidence(
            AppDependencyKind::Capability,
            "capability:content_search",
            "1.4.3",
            "capability-revision:99",
            99,
            b"exact registry bytes",
        );
        assert_eq!(
            proof.content_digest,
            AppDigest::blake3(b"exact registry bytes")
        );
        assert!(AppRevision::new(0).is_err());
        assert!(matches!(
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Capability,
                reference("capability:content_search"),
                "not-semver".to_owned(),
                reference("capability-revision:99"),
                AppRevision::new(99).unwrap(),
                b"bytes",
            ),
            Err(AppPackageLockError::InvalidResolvedVersion { .. })
        ));

        let mut wrong_version = registry_evidence();
        wrong_version[1].semantic_version = "2.0.0".to_owned();
        assert!(matches!(
            lock_app_package_dependencies(&package(), wrong_version, &limits),
            Err(AppPackageLockError::VersionMismatch { .. })
        ));

        let oversized = "1".repeat(limits.max_string_bytes() + 1);
        assert!(matches!(
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Capability,
                reference("capability:content_search"),
                oversized,
                reference("capability-revision:99"),
                AppRevision::new(99).unwrap(),
                b"bytes",
            ),
            Err(AppPackageLockError::InvalidResolvedVersion { .. })
        ));
    }

    #[test]
    fn missing_extra_duplicate_and_conflicting_resolutions_are_rejected() {
        let limits = AppPackageLimits::default();
        let mut missing = registry_evidence();
        missing.pop();
        assert!(matches!(
            lock_app_package_dependencies(&package(), missing, &limits),
            Err(AppPackageLockError::MissingResolution(_))
        ));

        let mut extra = registry_evidence();
        extra.push(evidence(
            AppDependencyKind::Capability,
            "capability:undeclared",
            "1.0.0",
            "capability-revision:100",
            100,
            b"undeclared",
        ));
        assert!(matches!(
            lock_app_package_dependencies(&package(), extra, &limits),
            Err(AppPackageLockError::UndeclaredResolution(_))
        ));

        let mut duplicate = registry_evidence();
        let first = duplicate[0].clone();
        duplicate.push(first);
        assert!(matches!(
            lock_app_package_dependencies(&package(), duplicate, &limits),
            Err(AppPackageLockError::DuplicateResolution)
        ));

        let mut normalized_duplicate = registry_evidence();
        let mut alias = normalized_duplicate[1].clone();
        alias.dependency_ref = reference("capability:CONTENT_SEARCH");
        normalized_duplicate.push(alias);
        assert!(matches!(
            lock_app_package_dependencies(&package(), normalized_duplicate, &limits),
            Err(AppPackageLockError::DuplicateResolution)
        ));

        let mut conflicting = registry_evidence();
        conflicting.push(evidence(
            AppDependencyKind::ProcedureSkill,
            "skill:summarize",
            "2.1.0",
            "skill-revision:1",
            1,
            b"registry-skill",
        ));
        assert!(matches!(
            lock_app_package_dependencies(&package(), conflicting, &limits),
            Err(AppPackageLockError::ConflictingResolution(_))
        ));
    }

    #[test]
    fn vendored_dependency_digest_is_derived_from_bundle_bytes() {
        let limits = AppPackageLimits::default();
        let first = lock_app_package_dependencies(&package(), registry_evidence(), &limits)
            .expect("first lock");
        let mut bundle = valid_bundle();
        bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "vendor/skills/summarize/bin/summarize.py")
            .expect("vendored member")
            .bytes
            .push(b'!');
        let changed_package =
            build_app_package_candidate(bundle, &limits).expect("changed package");
        let second = lock_app_package_dependencies(&changed_package, registry_evidence(), &limits)
            .expect("second lock");

        let first_vendored = first
            .dependencies()
            .iter()
            .find(|dependency| dependency.kind() == AppDependencyKind::ProcedureSkill)
            .expect("first vendored");
        let second_vendored = second
            .dependencies()
            .iter()
            .find(|dependency| dependency.kind() == AppDependencyKind::ProcedureSkill)
            .expect("second vendored");
        assert_ne!(
            first_vendored.content_digest(),
            second_vendored.content_digest()
        );
        assert_ne!(first.lock_digest(), second.lock_digest());
    }

    #[test]
    fn every_registry_revision_and_digest_change_rotates_the_lock() {
        let limits = AppPackageLimits::default();
        let baseline = lock_app_package_dependencies(&package(), registry_evidence(), &limits)
            .expect("baseline");
        let mut changed = registry_evidence();
        changed[0] = evidence(
            AppDependencyKind::Contract,
            "contract:magician_contract",
            "1.2.0",
            "contract-revision:43",
            43,
            b"contract-v1.2.0-repacked",
        );
        let changed = lock_app_package_dependencies(&package(), changed, &limits).expect("changed");
        assert_ne!(baseline.lock_digest(), changed.lock_digest());
    }

    #[test]
    fn lock_carries_no_authority() {
        let lock = lock_app_package_dependencies(
            &package(),
            registry_evidence(),
            &AppPackageLimits::default(),
        )
        .expect("lock");
        let encoded = serde_json::to_string(&lock).expect("serialize lock");
        for forbidden in ["grant", "scope", "principal", "workspace", "authority"] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[test]
    fn hostile_resolution_cardinality_is_bounded_before_matching() {
        let limits = AppPackageLimits::default();
        let mut claims = registry_evidence();
        for index in 0..limits.max_dependencies() {
            claims.push(evidence(
                AppDependencyKind::Capability,
                &format!("capability:extra-{index}"),
                "1.0.0",
                &format!("capability-revision:{index}"),
                u64::try_from(index).unwrap_or_default().saturating_add(1),
                index.to_string().as_bytes(),
            ));
        }
        assert!(matches!(
            lock_app_package_dependencies(&package(), claims, &limits),
            Err(AppPackageLockError::ResolutionLimit { .. })
        ));
    }

    #[test]
    fn vendored_name_and_version_are_derived_from_the_bundled_skill_manifest() {
        let limits = AppPackageLimits::default();
        for replacement in [
            "---\nname: other-skill\nversion: 2.1.0\n---\n",
            "---\nname: summarize\nversion: 9.0.0\n---\n",
        ] {
            let mut bundle = valid_bundle();
            bundle
                .iter_mut()
                .find(|member| member.path.as_str() == "vendor/skills/summarize/SKILL.md")
                .expect("vendored manifest")
                .bytes = replacement.as_bytes().to_vec();
            let package = build_app_package_candidate(bundle, &limits).expect("package");
            assert!(matches!(
                lock_app_package_dependencies(&package, registry_evidence(), &limits),
                Err(AppPackageLockError::VendoredIdentityMismatch { .. }
                    | AppPackageLockError::VendoredPathIdentityMismatch { .. }
                    | AppPackageLockError::VersionMismatch { .. })
            ));
        }
    }

    #[test]
    fn referenced_vendored_member_cannot_be_substituted_outside_the_bundle() {
        let limits = AppPackageLimits::default();
        let bundle = valid_bundle()
            .into_iter()
            .filter(|member: &AppBundleMember| {
                member.path.as_str() != "vendor/skills/summarize/SKILL.md"
            })
            .collect();
        assert!(build_app_package_candidate(bundle, &limits).is_err());
    }

    #[test]
    fn governed_runtime_requires_exact_locked_registry_skill_revision_and_bytes() {
        let limits = AppPackageLimits::default();
        let mut bundle = valid_bundle();
        let skill_document = std::str::from_utf8(
            bundle
                .iter()
                .find(|member| member.path.as_str() == "SKILL.md")
                .unwrap()
                .bytes
                .as_slice(),
        )
        .unwrap()
        .replace(
            "        vendored_path: vendor/skills/summarize/SKILL.md\n",
            "",
        );
        bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap()
            .bytes = skill_document.into_bytes();
        let package = build_app_package_candidate(bundle, &limits).unwrap();
        let skill = evidence(
            AppDependencyKind::ProcedureSkill,
            "skill:summarize",
            "2.1.0",
            "skill-revision:7",
            7,
            b"immutable skill bytes",
        );
        let mut resolutions = registry_evidence();
        resolutions.push(skill.clone());
        let lock = lock_app_package_dependencies(&package, resolutions, &limits).unwrap();

        let fence =
            authorize_locked_registry_skill(&lock, &reference("skill:summarize"), &skill).unwrap();
        assert_eq!(fence.package_lock_digest(), lock.lock_digest());

        let mutable_name_new_revision = evidence(
            AppDependencyKind::ProcedureSkill,
            "skill:summarize",
            "2.1.0",
            "skill-revision:8",
            8,
            b"changed skill bytes",
        );
        assert!(matches!(
            authorize_locked_registry_skill(
                &lock,
                &reference("skill:summarize"),
                &mutable_name_new_revision,
            ),
            Err(AppPackageLockError::LockedSkillIdentityMismatch(_))
        ));
    }

    #[test]
    fn governed_runtime_uses_exact_vendored_subtree_not_a_global_name() {
        let limits = AppPackageLimits::default();
        let package = package();
        let lock = lock_app_package_dependencies(&package, registry_evidence(), &limits).unwrap();
        let fence = authorize_locked_vendored_skill(&lock, &package, &reference("skill:summarize"))
            .unwrap();
        assert!(matches!(
            fence.source(),
            AppLockedDependencySource::VendoredBundleMember { .. }
        ));
        static_assertions::assert_not_impl_any!(
            AppLockedSkillExecutionFence: serde::de::DeserializeOwned
        );
    }
}
